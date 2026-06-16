//! gzip **write** path, ported from zlib `gzwrite.c`.
//!
//! This module drives the [`crate::deflate`] engine configured for the gzip
//! wrapper (RFC 1952, `windowBits == 31`) and writes the produced bytes to the
//! owned [`std::fs::File`] held by the [`GzState`] handle. The gzip framing —
//! the 10-byte header and the CRC-32 + ISIZE trailer — is emitted entirely by
//! the deflate engine because of the `MAX_WBITS + 16` window selector; this file
//! therefore only shuttles bytes and manages the input/output working buffers
//! and the flush state machine (AAP §0.6.1, §0.7.1).
//!
//! # What is ported
//!
//! | C (`gzwrite.c`)        | Rust (this file)                                  |
//! |------------------------|---------------------------------------------------|
//! | `gz_init`              | [`gz_init`] — lazy buffer/engine allocation       |
//! | `gz_comp`              | [`gz_comp`] — the single output choke point       |
//! | `gz_zero`              | [`gz_zero`] — compress a run of zero bytes        |
//! | `gz_write`             | [`gz_write`] — buffer/compress a user slice       |
//! | `gzwrite`              | [`gzwrite`]                                        |
//! | `gzfwrite`             | [`gzfwrite`]                                       |
//! | `gzputc`               | [`gzputc`]                                         |
//! | `gzputs`               | [`gzputs`]                                         |
//! | `gzvprintf`/`gzprintf` | [`gzprintf`] (safe `fmt::Arguments` core)         |
//! | `gzflush`              | [`gzflush`]                                        |
//! | `gzsetparams` body     | [`set_params`] (shared with `open.rs`)            |
//! | `gzclose_w`            | [`gzclose_w`] / [`finish`]                         |
//!
//! # Shared helpers
//!
//! [`gz_comp`], [`gz_zero`], [`gz_init`], [`set_params`], [`finish`], and
//! [`gzclose_w`] are `pub(crate)` so the sibling `open.rs` (`gzsetparams`),
//! `close.rs` (`gzclose`), and the `state.rs` RAII path can reuse the
//! deflate-driving logic without duplicating it (AAP §0.3.2, §0.6.3). `gz_comp`
//! is the *single* point through which every output byte flows — normal writes,
//! explicit flushes, the final `Z_FINISH`, the seek-forward zero fill, and the
//! `gzsetparams` block flush all funnel through it.
//!
//! # Buffer sizing (frozen behavior, AAP §0.6.1)
//!
//! * The **input** buffer is sized `want << 1` (DOUBLE). The second half exists
//!   so [`gzprintf`] can format up to `want` bytes after any currently buffered
//!   input without overflowing — see [`gz_init`].
//! * The **output** buffer is sized `want` (allocated only when compressing,
//!   i.e. not a transparent `direct` write).
//! * `size` is set to `want` once the buffers exist; `size == 0` is the
//!   "not yet initialized" sentinel, exactly like the C `malloc`-on-first-use
//!   scheme.
//!
//! # deflate configuration (frozen behavior, AAP §0.6.1)
//!
//! The engine is initialized with
//! `deflateInit2(level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL, strategy)`,
//! i.e. `windowBits == 31`, which selects the gzip wrapper. The CRC-32 +
//! ISIZE trailer is produced by the engine on `Z_FINISH`; this file never
//! hand-rolls it.
//!
//! # Safety
//!
//! This file contains **no `unsafe`** (AAP §0.6.2). The variadic `gzprintf`
//! `va_list` marshalling and all C-string handling stay in `crate::ffi`; here
//! the public surface is safe Rust (`&str` / `&[u8]` / [`core::fmt::Arguments`]).

use crate::deflate::state::DeflateStream;
use crate::deflate::{
    DeflateState, deflate, deflate_end, deflate_init2, deflate_params, deflate_reset,
};
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::GzState;
// Glob-import the constant surface (all `Z_*` codes plus `FlushMode`,
// `MAX_WBITS`, `DEF_MEM_LEVEL`, `Z_DEFLATED`, ...). The `crate::error` glob is
// intentionally avoided because its `Result<T>` 1-arg alias would shadow the
// std 2-arg `Result` used in this file's helper signatures.
use crate::constants::*;
use std::fs::File;
use std::io::Write;

// ===========================================================================
// Internal: the deflate drain loop shared by every compressing code path
// ===========================================================================

/// Error returned by [`drain_deflate`], distinguishing an I/O failure on the
/// destination file from an internal deflate-engine inconsistency.
///
/// The caller maps each variant onto the canonical zlib error recorded by
/// [`GzState::gz_error`]: [`DrainErr::Io`] → [`Z_ERRNO`] (with the OS error
/// text), [`DrainErr::Deflate`] → [`Z_STREAM_ERROR`] (the C
/// `"internal error: deflate stream corrupt"`).
enum DrainErr {
    /// A `write_all` to the destination file failed; carries the OS error so the
    /// caller can format the `gz_error` message.
    Io(std::io::Error),
    /// `deflate()` reported [`ZlibError::StreamError`] (a corrupt engine state).
    Deflate,
}

/// Run the deflate engine over `input` with `flush`, draining **all** produced
/// output to `file`, and report how much input was consumed and whether the
/// stream reached its end — the safe-Rust core of C `gz_comp`'s
/// `do { ... } while (have)` loop (`gzwrite.c` L103-140).
///
/// `out_buf` is the `want`-sized staging buffer. Each iteration hands the engine
/// the full staging buffer as output space; whatever it produces is immediately
/// written to `file` with [`Write::write_all`] and the buffer is reused. Because
/// the deflate **stream** bytes are independent of how the output is chunked
/// toward the file, draining the whole staging buffer every iteration yields
/// file content byte-identical to C zlib while removing the need to persist an
/// output cursor across calls.
///
/// The loop continues while the engine completely fills the staging buffer
/// (more output may remain) and stops once a call leaves spare output room
/// (the engine ran out of work for this flush) or signals
/// [`ReturnCode::StreamEnd`].
///
/// # Returns
///
/// `Ok((consumed, stream_end))` where `consumed` is the number of input bytes
/// taken from `input` and `stream_end` is `true` if the engine returned
/// [`ReturnCode::StreamEnd`] (only possible under [`FlushMode::Finish`]).
///
/// # Errors
///
/// * [`DrainErr::Io`] — a `write_all` to `file` failed.
/// * [`DrainErr::Deflate`] — the engine returned [`ZlibError::StreamError`].
fn drain_deflate(
    dstate: &mut DeflateState,
    input: &[u8],
    out_buf: &mut [u8],
    file: &mut File,
    flush: FlushMode,
) -> Result<(usize, bool), DrainErr> {
    let buflen = out_buf.len();
    let mut consumed = 0usize;
    let mut stream_end = false;

    loop {
        // Compress one bufferful. The per-call `DeflateStream` borrows the
        // persistent engine state plus the remaining input window and the whole
        // staging buffer; its cursors report what happened on this call.
        let produced;
        let ret;
        {
            let mut ds = DeflateStream::new(&mut *dstate, &input[consumed..], &mut out_buf[..]);
            ret = deflate(&mut ds, flush);
            consumed += ds.in_next;
            produced = ds.out_next;
        }

        // Drain everything the engine just produced to the destination file.
        if produced > 0 {
            file.write_all(&out_buf[..produced]).map_err(DrainErr::Io)?;
        }

        match ret {
            // Stream fully finished (only under Z_FINISH): drain done, stop.
            Ok(ReturnCode::StreamEnd) => {
                stream_end = true;
                break;
            }
            // Progress made (or a benign Ok): keep going only if the buffer was
            // completely filled (the engine may still have more to emit).
            Ok(_) => {}
            // No progress is possible (e.g. a no-op flush): nothing more to do.
            Err(ZlibError::BufError) => break,
            // Corrupt engine state — surface to the caller as Z_STREAM_ERROR.
            Err(ZlibError::StreamError) => return Err(DrainErr::Deflate),
            // No other error is reachable from `deflate()`; treat defensively as
            // "stop draining" so the loop can never spin.
            Err(_) => break,
        }

        // The engine had spare output room, so it stopped for lack of work, not
        // lack of space: this flush is fully drained.
        if produced < buflen {
            break;
        }
    }

    Ok((consumed, stream_end))
}

// ===========================================================================
// gz_init — lazy buffer + engine allocation (gzwrite.c L11-57)
// ===========================================================================

/// Allocate the working buffers and initialize the deflate engine on the first
/// write — the safe-Rust port of C `gz_init` (`gzwrite.c` L11-57).
///
/// Initialization is marked by setting [`GzState::size`] to a non-zero value
/// (`want`), mirroring C. The input buffer is sized `want << 1` (DOUBLE) so
/// [`gzprintf`] can format into the second half; the output buffer is sized
/// `want` and the deflate engine is configured for the gzip wrapper
/// (`windowBits == MAX_WBITS + 16 == 31`) — both only when compressing
/// (a transparent `direct` write needs neither).
///
/// # Returns
///
/// `0` on success, or `-1` on an allocation / engine-init failure (with
/// [`Z_MEM_ERROR`] recorded via [`GzState::gz_error`]).
pub(crate) fn gz_init(state: &mut GzState) -> i32 {
    // Allocate the (doubled) input buffer. `Vec` cannot fail like `malloc`, so
    // probe capacity with `try_reserve_exact` first and map an allocation
    // failure to the same Z_MEM_ERROR the C code reports for `in == NULL`.
    let in_size = state.want << 1;
    let mut in_buf: Vec<u8> = Vec::new();
    if in_buf.try_reserve_exact(in_size).is_err() {
        state.gz_error(Z_MEM_ERROR, Some("out of memory"));
        return -1;
    }
    in_buf.resize(in_size, 0);
    state.in_buf = in_buf;

    // Only compressing streams need the output buffer and the deflate engine.
    if state.direct == 0 {
        let mut out_buf: Vec<u8> = Vec::new();
        if out_buf.try_reserve_exact(state.want).is_err() {
            // Match C cleanup: free the input buffer before reporting OOM.
            state.in_buf = Vec::new();
            state.gz_error(Z_MEM_ERROR, Some("out of memory"));
            return -1;
        }
        out_buf.resize(state.want, 0);
        state.out_buf = out_buf;

        // Configure the engine for gzip compression. `windowBits = MAX_WBITS +
        // 16 = 31` selects the RFC 1952 gzip wrapper; the engine then emits the
        // header and the CRC-32 + ISIZE trailer itself. The Rust allocator
        // stands in for the C `zalloc`/`zfree`/`opaque == Z_NULL` triple.
        match deflate_init2(
            state.level,
            Z_DEFLATED,
            MAX_WBITS + 16,
            DEF_MEM_LEVEL,
            state.strategy,
        ) {
            Ok(dstate) => state.strm.set_state(dstate),
            Err(_) => {
                // Match C cleanup: free both buffers before reporting OOM.
                state.out_buf = Vec::new();
                state.in_buf = Vec::new();
                state.gz_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }
    }

    // Mark the state initialized and reset the input cursor. The engine's output
    // cursor is established per-call in `drain_deflate`, so unlike C there is no
    // persistent `next_out`/`x.next` to seed here.
    state.size = state.want;
    state.in_next = 0;
    state.in_avail = 0;
    0
}

// ===========================================================================
// gz_comp — the single output choke point (gzwrite.c L59-148)
// ===========================================================================

/// Compress whatever input is currently buffered in [`GzState::in_buf`]
/// (`in_buf[in_next .. in_next + in_avail]`) with `flush`, writing all produced
/// output to the file — the safe-Rust port of C `gz_comp` (`gzwrite.c`
/// L59-148).
///
/// This is the workhorse reused by [`gz_zero`], [`gzflush`], [`set_params`],
/// [`gzclose_w`], and [`finish`]: **every** output byte the gzip write path ever
/// emits flows through here.
///
/// Behavior, mirroring C:
///
/// * Performs lazy initialization ([`gz_init`]) if the buffers do not exist yet.
/// * In transparent (`direct`) mode, writes the buffered input straight to the
///   file with no compression and ignores `flush`.
/// * Honors a pending `reset` (a `deflateReset` deferred after a prior
///   `Z_FINISH`): a fresh gzip member is started lazily, and not at all if there
///   is no data to write and the flush is [`Z_NO_FLUSH`].
/// * Runs the deflate drain loop ([`drain_deflate`]) over the buffered input.
/// * After a [`Z_FINISH`], sets [`GzState::reset`] so the next write begins a new
///   gzip member (multi-member parity).
///
/// # Returns
///
/// `0` on success, or `-1` on a write or engine error (recorded via
/// [`GzState::gz_error`]).
pub(crate) fn gz_comp(state: &mut GzState, flush: i32) -> i32 {
    // Allocate on first use; propagate an allocation failure.
    if state.size == 0 && gz_init(state) == -1 {
        return -1;
    }

    // --- transparent (direct) write: bypass deflate entirely ---------------
    if state.direct != 0 {
        if state.in_avail != 0 {
            let start = state.in_next;
            let end = start + state.in_avail;
            let io_result = match state.file.as_mut() {
                Some(file) => file.write_all(&state.in_buf[start..end]),
                None => Ok(()),
            };
            if let Err(err) = io_result {
                // C parity: the OS error text becomes the `gz_error` message,
                // exactly as C passes `zstrerror(errno)`. It is surfaced by the
                // safe-Rust `gzerror`; the FFI-visible `gzerror` returns only the
                // fixed `'static` text for the code, so no OS detail crosses the
                // C ABI. See the error-disclosure note on `GzState::gz_error`.
                let msg = err.to_string();
                state.gz_error(Z_ERRNO, Some(&msg));
                return -1;
            }
        }
        // All buffered input has been written; rewind the input cursor.
        state.in_next = 0;
        state.in_avail = 0;
        return 0;
    }

    // --- honor a pending reset after a previous Z_FINISH -------------------
    if state.reset {
        // Don't start a new gzip member unless there is data to write and we are
        // not merely flushing (matches C exactly).
        if state.in_avail == 0 && flush == Z_NO_FLUSH {
            return 0;
        }
        if let Some(dstate) = state.strm.state_as_mut::<DeflateState>() {
            let _ = deflate_reset(dstate);
        }
        state.reset = false;
    }

    // Translate the raw flush code into the engine's typed flush. All callers
    // pass validated values (Z_NO_FLUSH/Z_BLOCK/Z_FINISH/...); fall back to
    // NoFlush defensively for any unexpected value.
    let flush_mode = FlushMode::try_from(flush).unwrap_or(FlushMode::NoFlush);

    // Run the drain loop over the buffered input. The engine state, the input
    // window, the staging buffer, and the file are disjoint fields of `state`,
    // so they can be borrowed simultaneously.
    let drain_result = {
        let start = state.in_next;
        let end = start + state.in_avail;
        // Defensive, non-panicking access to the engine and file handle. Both
        // are invariants for a non-direct writing stream (gz_init attached
        // them), but a corrupted or partially-finalized `GzState` must yield a
        // zlib error code — never a panic — matching C's `gz_comp` returning -1
        // with the error recorded.
        let Some(dstate) = state.strm.state_as_mut::<DeflateState>() else {
            state.gz_error(
                Z_STREAM_ERROR,
                Some("internal error: deflate stream corrupt"),
            );
            return -1;
        };
        let input = &state.in_buf[start..end];
        let out = state.out_buf.as_mut_slice();
        let Some(file) = state.file.as_mut() else {
            state.gz_error(Z_STREAM_ERROR, Some("internal error: file handle missing"));
            return -1;
        };
        drain_deflate(dstate, input, out, file, flush_mode)
    };

    match drain_result {
        Ok((consumed, _stream_end)) => {
            state.in_next += consumed;
            state.in_avail -= consumed;
            // When the input buffer empties, rewind the cursor to its start so
            // the next accumulation begins at offset 0 (matches C resetting
            // `strm.next_in = state->in`).
            if state.in_avail == 0 {
                state.in_next = 0;
            }
        }
        Err(DrainErr::Io(err)) => {
            let msg = err.to_string();
            state.gz_error(Z_ERRNO, Some(&msg));
            return -1;
        }
        Err(DrainErr::Deflate) => {
            state.gz_error(
                Z_STREAM_ERROR,
                Some("internal error: deflate stream corrupt"),
            );
            return -1;
        }
    }

    // If that completed a deflate stream, allow another gzip member to start.
    if flush == Z_FINISH {
        state.reset = true;
    }
    0
}

// ===========================================================================
// gz_zero — compress a run of zero bytes (gzwrite.c L150-182)
// ===========================================================================

/// Compress `len` zero bytes to the output — the safe-Rust port of C `gz_zero`
/// (`gzwrite.c` L150-182), used to satisfy a forward `gzseek` on a write stream
/// by writing the skipped region as zeros.
///
/// Any buffered input is flushed first (with [`Z_NO_FLUSH`]) so the zeros are
/// appended in order, then the zeros are fed through [`gz_comp`] in `want`-sized
/// chunks. [`GzState::pos`] advances by the number of zeros written.
///
/// # Returns
///
/// `0` on success, or `-1` on a write / allocation error.
pub(crate) fn gz_zero(state: &mut GzState, len: i64) -> i32 {
    // Allocate on first use.
    if state.size == 0 && gz_init(state) == -1 {
        return -1;
    }

    // Consume whatever is already buffered before appending zeros.
    if state.in_avail != 0 && gz_comp(state, Z_NO_FLUSH) == -1 {
        return -1;
    }

    // Compress `len` zeros, `want` bytes at a time. The first chunk zero-fills
    // the input buffer; subsequent chunks reuse those zeros (gz_comp only reads
    // the input, never writes it), mirroring C's `first` optimization.
    let mut remaining = len;
    let mut first = true;
    while remaining > 0 {
        let n = core::cmp::min(remaining as usize, state.want);
        if first {
            for byte in &mut state.in_buf[..n] {
                *byte = 0;
            }
            first = false;
        }
        state.in_next = 0;
        state.in_avail = n;
        let ret = gz_comp(state, Z_NO_FLUSH);
        // gz_comp consumes the whole chunk under Z_NO_FLUSH with ample output.
        state.pos += n as i64;
        remaining -= n as i64;
        if ret == -1 {
            return -1;
        }
    }
    0
}

// ===========================================================================
// gz_write — buffer or directly compress a user slice (gzwrite.c L184-252)
// ===========================================================================

/// Write `buf` to the gzip stream, returning the number of bytes written — the
/// safe-Rust port of C `gz_write` (`gzwrite.c` L184-252).
///
/// Small writes (`buf.len() < want`) are copied into the input buffer and
/// compressed only once it fills, which lets many tiny writes coalesce into full
/// deflate blocks. Large writes (`buf.len() >= want`) first flush any buffered
/// input and then feed the user slice **directly** to the engine with no
/// intermediate copy (matching C pointing `strm.next_in` at the user buffer).
/// Either way the deflate **stream** bytes are identical to C zlib.
///
/// # Returns
///
/// The number of bytes accepted (always `buf.len()` on success), or `0` on
/// error (with [`GzState::gz_error`] set).
pub(crate) fn gz_write(state: &mut GzState, buf: &[u8]) -> usize {
    // Nothing to do for an empty write.
    if buf.is_empty() {
        return 0;
    }

    // Allocate on first use.
    if state.size == 0 && gz_init(state) == -1 {
        return 0;
    }

    // Satisfy a pending seek-forward by writing the skipped span as zeros.
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return 0;
        }
    }

    let len = buf.len();

    if len < state.want {
        // Small write: accumulate into the input buffer, compressing when full.
        let mut pos = 0usize;
        loop {
            if state.in_avail == 0 {
                state.in_next = 0;
            }
            // Bytes already occupied in the input buffer, measured from its
            // start (C `(next_in + avail_in) - in`).
            let have = state.in_next + state.in_avail;
            let mut copy = state.size - have;
            if copy > len - pos {
                copy = len - pos;
            }
            state.in_buf[have..have + copy].copy_from_slice(&buf[pos..pos + copy]);
            state.in_avail += copy;
            state.pos += copy as i64;
            pos += copy;
            if pos == len {
                break;
            }
            // Buffer full with more to write: compress and continue.
            if gz_comp(state, Z_NO_FLUSH) == -1 {
                return 0;
            }
        }
    } else {
        // Large write: flush any buffered input, then process the user slice
        // directly.
        if state.in_avail != 0 && gz_comp(state, Z_NO_FLUSH) == -1 {
            return 0;
        }

        if state.direct != 0 {
            // Transparent mode: write the user slice straight to the file.
            let io_result = match state.file.as_mut() {
                Some(file) => file.write_all(buf),
                None => Ok(()),
            };
            if let Err(err) = io_result {
                // C parity: the OS error text becomes the `gz_error` message,
                // exactly as C passes `zstrerror(errno)`. It is surfaced by the
                // safe-Rust `gzerror`; the FFI-visible `gzerror` returns only the
                // fixed `'static` text for the code, so no OS detail crosses the
                // C ABI. See the error-disclosure note on `GzState::gz_error`.
                let msg = err.to_string();
                state.gz_error(Z_ERRNO, Some(&msg));
                return 0;
            }
        } else {
            // Compress the user slice directly (no intermediate copy).
            // Defensive, non-panicking access: the engine and file handle are
            // invariants for a non-direct writing stream (gz_init attached
            // them), but a corrupted or partially-finalized `GzState` must yield
            // a zlib error (here a 0-byte write) — never a panic — matching C's
            // error-return contract.
            let drain_result = {
                let Some(dstate) = state.strm.state_as_mut::<DeflateState>() else {
                    state.gz_error(
                        Z_STREAM_ERROR,
                        Some("internal error: deflate stream corrupt"),
                    );
                    return 0;
                };
                let out = state.out_buf.as_mut_slice();
                let Some(file) = state.file.as_mut() else {
                    state.gz_error(Z_STREAM_ERROR, Some("internal error: file handle missing"));
                    return 0;
                };
                drain_deflate(dstate, buf, out, file, FlushMode::NoFlush)
            };
            match drain_result {
                Ok(_) => {}
                Err(DrainErr::Io(err)) => {
                    let msg = err.to_string();
                    state.gz_error(Z_ERRNO, Some(&msg));
                    return 0;
                }
                Err(DrainErr::Deflate) => {
                    state.gz_error(
                        Z_STREAM_ERROR,
                        Some("internal error: deflate stream corrupt"),
                    );
                    return 0;
                }
            }
        }
        state.pos += len as i64;
    }

    len
}

// ===========================================================================
// Public C-faithful write API (gzwrite.c)
// ===========================================================================

/// Write `buf` to the gzip file, returning the number of **uncompressed** bytes
/// written — the safe-Rust port of C `gzwrite` (`gzwrite.c` L254-277).
///
/// Returns `0` on error (with [`GzState::gz_error`] set) or if the handle is not
/// open for writing. Because the C return type is `int`, a length that does not
/// fit in a positive `int` is rejected with [`Z_DATA_ERROR`], matching C.
pub fn gzwrite(state: &mut GzState, buf: &[u8]) -> i32 {
    // Must be writing with no serious pending error.
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.clear_error();

    // Mirror C's "fits in an int" guard so the returned count is always valid.
    if buf.len() > i32::MAX as usize {
        state.gz_error(Z_DATA_ERROR, Some("requested length does not fit in int"));
        return 0;
    }

    gz_write(state, buf) as i32
}

/// Write `nitems` items of `item_size` bytes each from `buf`, returning the
/// number of **full items** written — the safe-Rust port of C `gzfwrite`
/// (`gzwrite.c` L279-304).
///
/// The byte count `item_size * nitems` is computed with a checked multiply; an
/// overflow is rejected with [`Z_STREAM_ERROR`] (matching C's
/// `len / size != nitems` guard) and returns `0`. A zero byte count returns `0`.
pub fn gzfwrite(state: &mut GzState, item_size: usize, nitems: usize, buf: &[u8]) -> usize {
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.clear_error();

    // Compute the total byte count, rejecting overflow exactly like C.
    let len = match item_size.checked_mul(nitems) {
        Some(len) => len,
        None => {
            state.gz_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
            return 0;
        }
    };
    if len == 0 {
        return 0;
    }

    // Write `len` bytes (bounded by the slice) and report full items written.
    // `item_size` is necessarily > 0 here (otherwise `len == 0` above).
    let data = if buf.len() >= len { &buf[..len] } else { buf };
    gz_write(state, data) / item_size
}

/// Write a single byte `c` (low 8 bits) to the gzip file, returning the byte
/// written (`c & 0xff`) or `-1` on error — the safe-Rust port of C `gzputc`
/// (`gzwrite.c` L306-347).
///
/// Reproduces C's fast path: when the input buffer is initialized and has room,
/// the byte is appended directly and [`GzState::pos`] is bumped without invoking
/// the full [`gz_write`] machinery.
pub fn gzputc(state: &mut GzState, c: i32) -> i32 {
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.clear_error();

    // Satisfy a pending seek-forward first.
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return -1;
        }
    }

    // Fast path: append directly into the input buffer if there is room.
    if state.size != 0 {
        if state.in_avail == 0 {
            state.in_next = 0;
        }
        let have = state.in_next + state.in_avail;
        if have < state.size {
            state.in_buf[have] = c as u8;
            state.in_avail += 1;
            state.pos += 1;
            return c & 0xff;
        }
    }

    // No room (or not initialized): fall back to the general write path.
    let one = [c as u8];
    if gz_write(state, &one) != 1 {
        return -1;
    }
    c & 0xff
}

/// Write the bytes of `s` to the gzip file, returning the number of bytes
/// written or `-1` on error — the safe-Rust port of C `gzputs`
/// (`gzwrite.c` L349-372).
///
/// The C function takes a NUL-terminated `char *`; this safe core takes a
/// [`str`] and writes its UTF-8 bytes. The C-string marshalling lives in
/// `crate::ffi`.
pub fn gzputs(state: &mut GzState, s: &str) -> i32 {
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.clear_error();

    let bytes = s.as_bytes();
    let len = bytes.len();
    // The C return type is `int`; reject a length that cannot be represented.
    if len > i32::MAX as usize {
        state.gz_error(Z_STREAM_ERROR, Some("string length does not fit in int"));
        return -1;
    }

    let put = gz_write(state, bytes);
    if len != 0 && put == 0 { -1 } else { put as i32 }
}

/// Format `args` and write the result to the gzip file, returning the number of
/// bytes written or a negative error code — the safe-Rust core of C `gzvprintf`
/// / `gzprintf` (`gzwrite.c` L403-495).
///
/// The C variadic `va_list` marshalling and the in-place `vsnprintf` into the
/// second half of the doubled input buffer are reproduced at the `crate::ffi`
/// boundary; this safe core accepts pre-built [`core::fmt::Arguments`] (e.g. from
/// the standard [`format_args!`] macro). The doubled input buffer
/// (`want << 1`, allocated by [`gz_init`]) exists precisely so a `want`-sized
/// formatted result always fits; to preserve C's contract this function refuses
/// (returns `0`) any result that would reach or exceed `want` bytes.
///
/// # Returns
///
/// The number of formatted bytes written, `0` if the formatted text is empty or
/// does not fit, or a negative [`Z_STREAM_ERROR`] / recorded error code on
/// failure.
pub fn gzprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // Ensure the buffers exist so `state.size` (== want) bounds the result.
    if state.size == 0 && gz_init(state) == -1 {
        return state.err;
    }

    // Satisfy a pending seek-forward first.
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // Format into an owned buffer (the safe analogue of C's vsnprintf into the
    // doubled input buffer's second half).
    let formatted = std::fmt::format(args);
    let bytes = formatted.as_bytes();

    // C refuses an empty result or one that does not fit in the `want`-sized
    // formatting region (`len == 0 || len >= size`).
    if bytes.is_empty() || bytes.len() >= state.size {
        return 0;
    }

    let put = gz_write(state, bytes);
    if put == 0 {
        // A write error occurred; surface the recorded code if any.
        return if state.err != Z_OK { state.err } else { 0 };
    }
    put as i32
}

/// Flush buffered data to the gzip file with the requested `flush` mode,
/// returning [`Z_OK`] or an error code — the safe-Rust port of C `gzflush`
/// (`gzwrite.c` L602-627).
///
/// `flush` must be in `0..=Z_FINISH`; an out-of-range value yields
/// [`Z_STREAM_ERROR`]. Note that `Z_FINISH` completes the current gzip member
/// (the next write starts a new one).
pub fn gzflush(state: &mut GzState, flush: i32) -> i32 {
    if !state.is_writing() || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // Validate the flush parameter exactly as C does (`0..=Z_FINISH`).
    if !(0..=Z_FINISH).contains(&flush) {
        return Z_STREAM_ERROR;
    }

    // Satisfy a pending seek-forward first.
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // Compress remaining buffered data with the requested flush.
    let _ = gz_comp(state, flush);
    state.err
}

// ===========================================================================
// set_params — the gzsetparams deflate driver (gzwrite.c L629-664)
// ===========================================================================

/// Change the compression `level` and `strategy` for subsequent input — the
/// safe-Rust port of C `gzsetparams`' body (`gzwrite.c` L629-664).
///
/// The AAP places the `gzsetparams` *entry point* in `open.rs`, but its logic
/// lives here (its source is `gzwrite.c`) so the deflate-driving code is not
/// duplicated; `open.rs::gzsetparams` is a thin wrapper over this function
/// (AAP §0.4.1 — `open.rs` lists `gzwrite.c` among its sources for exactly this
/// reason).
///
/// If a stream is already active, the currently buffered input is flushed with
/// [`Z_BLOCK`] (preserving the previous parameters for that data) before
/// `deflateParams` applies the new ones; any bytes `deflateParams` emits at the
/// block boundary are drained to the file here.
///
/// # Returns
///
/// [`Z_OK`] on success (including the no-change fast path), or an error code.
pub(crate) fn set_params(state: &mut GzState, level: i32, strategy: i32) -> i32 {
    // Must be compressing (not transparent) with no serious pending error.
    if !state.is_writing() || (state.err != Z_OK && !state.again) || state.direct != 0 {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // No change requested -> nothing to do.
    if level == state.level && strategy == state.strategy {
        return Z_OK;
    }

    // Satisfy a pending seek-forward first.
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // Apply the change to an already-running stream.
    if state.size != 0 {
        // Flush the previously buffered input under the *old* parameters.
        if state.in_avail != 0 && gz_comp(state, Z_BLOCK) == -1 {
            return state.err;
        }

        // Call deflateParams with empty input, capturing any boundary output.
        let produced = {
            let dstate = match state.strm.state_as_mut::<DeflateState>() {
                Some(dstate) => dstate,
                None => {
                    state.gz_error(
                        Z_STREAM_ERROR,
                        Some("internal error: deflate stream corrupt"),
                    );
                    return state.err;
                }
            };
            let empty: [u8; 0] = [];
            let out = state.out_buf.as_mut_slice();
            let mut ds = DeflateStream::new(dstate, &empty, out);
            // C ignores the deflateParams return value (it is `void`-cast).
            let _ = deflate_params(&mut ds, level, strategy);
            ds.out_next
        };

        // Drain any bytes produced by deflateParams to the file.
        if produced > 0 {
            let io_result = match state.file.as_mut() {
                Some(file) => file.write_all(&state.out_buf[..produced]),
                None => Ok(()),
            };
            if let Err(err) = io_result {
                let msg = err.to_string();
                state.gz_error(Z_ERRNO, Some(&msg));
                return state.err;
            }
        }
    }

    state.level = level;
    state.strategy = strategy;
    Z_OK
}

// ===========================================================================
// gzclose_w / finish — write-side close (gzwrite.c L666-700)
// ===========================================================================

/// Close a gzip file open for writing, returning [`Z_OK`] or the first error
/// encountered — the safe-Rust port of C `gzclose_w` (`gzwrite.c` L666-700).
///
/// Steps, mirroring C: satisfy any pending seek (writing zeros), finish the
/// stream with [`Z_FINISH`] (emitting the final deflate data and the gzip
/// CRC-32 + ISIZE trailer), run the engine's `deflateEnd` status check, then
/// release the engine state and buffers and close the file. The owned
/// [`std::fs::File`], the engine state, and the working buffers are freed by
/// ordinary ownership (the RAII replacement for the C `free`/`close` calls).
///
/// The handle is marked [`finalized`](GzState::finalize) so the subsequent
/// [`Drop`] becomes a no-op (no double-finalize).
///
/// This is exposed crate-wide so `close.rs::gzclose` can dispatch to it.
pub fn gzclose_w(state: &mut GzState) -> i32 {
    let mut ret = Z_OK;

    // Must be a write handle.
    if !state.is_writing() {
        return Z_STREAM_ERROR;
    }

    // Satisfy a pending seek-forward (record but do not abort on error).
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            ret = state.err;
        }
    }

    // Finish the stream: emit trailing data + the gzip trailer.
    if gz_comp(state, Z_FINISH) == -1 {
        ret = state.err;
    }

    // deflateEnd status check (the owned buffers are freed below by ownership).
    if state.size != 0 && state.direct == 0 {
        if let Some(dstate) = state.strm.state_as_mut::<DeflateState>() {
            let _ = deflate_end(dstate);
        }
    }

    // Release the engine state and working buffers (RAII free).
    state.strm.end();
    state.in_buf = Vec::new();
    state.out_buf = Vec::new();
    state.size = 0;
    state.clear_error();

    // Close the file by dropping it (RAII close).
    let _ = state.file.take();

    // Mark finalized so Drop does not run finalization again.
    let _ = state.finalize();

    ret
}

/// Best-effort, error-discarding finish used by the RAII / `Drop` path — emits
/// the final deflate data and the gzip trailer and runs the engine teardown,
/// ignoring any error (a destructor cannot return one).
///
/// Sibling modules (`close.rs`) and the [`GzWriter`] wrapper call this to ensure
/// a write stream is flushed and the gzip trailer written even when the caller
/// did not invoke an explicit close. After it runs, the handle is
/// [`finalized`](GzState::finalize) so a later [`Drop`] is a no-op.
pub(crate) fn finish(state: &mut GzState) -> i32 {
    // Only finish an initialized write stream that has not been finalized.
    if state.is_writing() && state.size != 0 {
        let _ = gz_comp(state, Z_FINISH);
        if state.direct == 0 {
            if let Some(dstate) = state.strm.state_as_mut::<DeflateState>() {
                let _ = deflate_end(dstate);
            }
        }
        state.strm.end();
        state.in_buf = Vec::new();
        state.out_buf = Vec::new();
        state.size = 0;
    }
    let _ = state.finalize();
    Z_OK
}

// ===========================================================================
// GzWriter — idiomatic `std::io::Write` wrapper (AAP §0.3.2)
// ===========================================================================

/// An idiomatic [`std::io::Write`] adapter over a gzip write [`GzState`]
/// (AAP §0.3.2 — trait abstractions for the gzip layer).
///
/// `GzWriter` owns the handle and maps [`Write::write`] onto [`gz_write`] and
/// [`Write::flush`] onto a [`Z_SYNC_FLUSH`] [`gz_comp`] followed by a flush of
/// the underlying file. On [`Drop`] it performs a best-effort [`finish`] so the
/// gzip trailer is always written even if the caller forgets to finish
/// explicitly (mirroring `flate2`'s `GzEncoder`).
///
/// # Construction
///
/// Obtain a write handle from the `gzopen`-family entry points and convert it
/// into a `GzWriter`, e.g.:
///
/// ```no_run
/// use std::io::Write;
/// use std::path::Path;
/// use zlib_rs::gz::{gzopen, GzWriter};
///
/// let handle = gzopen(Path::new("out.gz"), "wb").expect("open for writing");
/// let mut writer = GzWriter::from(handle); // or: handle.into()
/// writer.write_all(b"hello, gzip").unwrap();
/// writer.finish();                          // or just drop `writer`
/// ```
///
/// The handle must be open for writing (a `"w"`/`"a"` mode); a read handle is
/// accepted but inert (writes fail and the [`Drop`] finish is a no-op), matching
/// the defensive behavior of the C `gzwrite` family.
pub struct GzWriter {
    state: GzState,
}

impl GzWriter {
    /// Wrap an already-opened write-mode [`GzState`] in a [`GzWriter`].
    pub(crate) fn new(state: GzState) -> GzWriter {
        GzWriter { state }
    }

    /// Wrap a write-mode handle returned by the `gzopen`-family entry points
    /// (a `Box<`[`GzState`]`>`) in an idiomatic [`std::io::Write`] adapter
    /// (AAP §0.3.2 — "impl Read/Write/BufRead/Seek for the gzip layer").
    ///
    /// This is the public constructor for [`GzWriter`]: the C-style
    /// [`gzopen`](crate::gz::open::gzopen) returns the opaque
    /// `Box<`[`GzState`]`>` handle, and this converts it into the streaming
    /// `Write` wrapper so external code can use it in any `std::io` pipeline
    /// (`write!`, [`std::io::copy`], `BufWriter`, …). See also the
    /// [`From<Box<GzState>>`](#impl-From%3CBox%3CGzState%3E%3E-for-GzWriter)
    /// conversion (`handle.into()`).
    ///
    /// The handle should be open for writing; a read handle is accepted but
    /// inert (see the type-level docs).
    #[must_use]
    pub fn from_handle(handle: Box<GzState>) -> GzWriter {
        GzWriter { state: *handle }
    }

    /// Borrow the underlying [`GzState`].
    pub(crate) fn state_mut(&mut self) -> &mut GzState {
        &mut self.state
    }

    /// Finish the gzip stream explicitly, returning the underlying error code
    /// ([`Z_OK`] on success). Equivalent to [`gzclose_w`] but keeps the wrapper
    /// alive (a subsequent [`Drop`] is a no-op).
    pub fn finish(&mut self) -> i32 {
        finish(&mut self.state)
    }
}

/// Convert a `gzopen`-family write handle (`Box<`[`GzState`]`>`) into the
/// idiomatic [`std::io::Write`] adapter, so `handle.into()` works alongside the
/// explicit [`GzWriter::from_handle`] constructor (AAP §0.3.2). Equivalent to
/// `GzWriter::from_handle(handle)`.
impl From<Box<GzState>> for GzWriter {
    fn from(handle: Box<GzState>) -> GzWriter {
        GzWriter::from_handle(handle)
    }
}

impl Write for GzWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = gz_write(&mut self.state, buf);
        if written == 0 && !buf.is_empty() {
            return Err(std::io::Error::other("gzip write failed"));
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Z_SYNC_FLUSH forces all buffered input through the engine and emits a
        // sync marker, so the file holds everything written so far (the standard
        // semantics for a compressing `Write::flush`, as in flate2).
        if gz_comp(&mut self.state, Z_SYNC_FLUSH) == -1 {
            return Err(std::io::Error::other("gzip flush failed"));
        }
        if let Some(file) = self.state.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

impl Drop for GzWriter {
    fn drop(&mut self) {
        // Best-effort finish so the gzip trailer is always emitted.
        let _ = finish(&mut self.state);
    }
}

// ===========================================================================
// Tests
// ===========================================================================
//
// These exercise the full gzip write path end to end. Output is validated two
// ways: (1) the raw file bytes must begin with the RFC 1952 gzip magic
// (`0x1f 0x8b`) and the DEFLATE method byte (`0x08`), proving the engine used
// the `windowBits = MAX_WBITS + 16 = 31` gzip wrapper; and (2) the bytes are
// decoded with `flate2` (which links canonical C zlib via its `zlib` feature),
// serving as the byte-level interop oracle required by the AAP (§0.6.7). A
// successful `flate2` decode also proves the gzip CRC-32 + ISIZE trailer that
// the deflate engine appends on `Z_FINISH` is correct.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gz::state::{GzState, Mode};
    use std::fs::File;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Generate a unique temp path (no external `tempfile` crate needed).
    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!("zlibrs_gzwrite_test_{tag}_{pid}_{nanos}_{n}.gz"));
        dir
    }

    /// RAII temp-file holder: opens write handles over a unique path and removes
    /// the file on drop (even if a test assertion panics).
    struct TempGz {
        path: PathBuf,
    }

    impl TempGz {
        fn new(tag: &str) -> TempGz {
            TempGz {
                path: temp_path(tag),
            }
        }

        /// Create the backing file and a write-mode [`GzState`] over it.
        fn open_write(&self) -> GzState {
            let file = File::create(&self.path).expect("create temp file");
            GzState::new(file, self.path.to_string_lossy().into_owned(), Mode::Write)
        }

        /// Read the raw on-disk bytes (the produced gzip stream).
        fn read_raw(&self) -> Vec<u8> {
            std::fs::read(&self.path).expect("read temp file")
        }
    }

    impl Drop for TempGz {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Decompress a single-member gzip stream with the canonical C-zlib oracle.
    fn gunzip(data: &[u8]) -> Vec<u8> {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("flate2 (C zlib) failed to decode the produced gzip stream");
        out
    }

    /// Assert that `raw` is a gzip stream that decodes to exactly `expected`.
    fn assert_gzip_roundtrip(raw: &[u8], expected: &[u8]) {
        assert!(
            raw.len() >= 3,
            "gzip stream too short ({} bytes)",
            raw.len()
        );
        assert_eq!(
            &raw[..3],
            &[0x1f, 0x8b, 0x08],
            "missing gzip magic / DEFLATE method (windowBits 31 wrapper)"
        );
        assert_eq!(
            gunzip(raw),
            expected,
            "round-trip through C zlib did not reproduce the input"
        );
    }

    /// Deterministic pseudo-random byte vector for compressible-but-varied data.
    fn pseudo_random(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| {
                let i = i as u32;
                (i.wrapping_mul(2_654_435_761) >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn empty_stream_is_valid_gzip() {
        let g = TempGz::new("empty");
        let mut state = g.open_write();
        // Closing with no writes still emits a valid (empty-payload) member.
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), b"");
    }

    #[test]
    fn gzwrite_empty_buffer_is_noop() {
        let g = TempGz::new("noop");
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, b""), 0);
        assert_eq!(state.err, Z_OK, "empty write must not set an error");
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), b"");
    }

    #[test]
    fn small_write_round_trips() {
        let g = TempGz::new("small");
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, data), data.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn large_write_round_trips() {
        // Exceeds the default `want` (8192) to force the large-write direct-feed
        // path through the engine.
        let g = TempGz::new("large");
        let data = pseudo_random(50_000);
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, &data), data.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), &data);
    }

    #[test]
    fn many_small_writes_round_trip() {
        // Repeated small writes accumulate across buffer-fill boundaries.
        let g = TempGz::new("many");
        let mut expected = Vec::new();
        let mut state = g.open_write();
        for i in 0..2000 {
            let chunk = format!("line {i}: lorem ipsum dolor sit amet\n");
            expected.extend_from_slice(chunk.as_bytes());
            assert_eq!(gzwrite(&mut state, chunk.as_bytes()), chunk.len() as i32);
        }
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), &expected);
    }

    #[test]
    fn gzputc_round_trips() {
        let g = TempGz::new("putc");
        let data = b"ABCDEFG-0123456789";
        let mut state = g.open_write();
        for &b in data {
            assert_eq!(gzputc(&mut state, i32::from(b)), i32::from(b));
        }
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn gzputc_past_buffer_round_trips() {
        // Writing more than `want` bytes one at a time forces the fast-append
        // path to overflow into the general `gz_write` fallback.
        let g = TempGz::new("putc_big");
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let mut state = g.open_write();
        for &b in &data {
            assert_eq!(gzputc(&mut state, i32::from(b)), i32::from(b));
        }
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), &data);
    }

    #[test]
    fn gzputs_round_trips() {
        let g = TempGz::new("puts");
        let s = "hello, gzip world — ported from gzwrite.c";
        let mut state = g.open_write();
        assert_eq!(gzputs(&mut state, s), s.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), s.as_bytes());
    }

    #[test]
    fn gzprintf_formats_and_round_trips() {
        let g = TempGz::new("printf");
        let mut state = g.open_write();
        let expected = "x=42 y=-7 s=hi pct=3.50";
        let n = gzprintf(
            &mut state,
            format_args!("x={} y={} s={} pct={:.2}", 42, -7, "hi", 3.5_f64),
        );
        assert_eq!(n, expected.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), expected.as_bytes());
    }

    #[test]
    fn gzprintf_near_want_does_not_overflow() {
        // A result just under `want` (8192) must format safely into the doubled
        // input buffer and round-trip cleanly.
        let g = TempGz::new("printf_big");
        let body = "a".repeat(8000);
        let mut state = g.open_write();
        let n = gzprintf(&mut state, format_args!("{body}"));
        assert_eq!(n, 8000);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), body.as_bytes());
    }

    #[test]
    fn gzprintf_too_large_returns_zero() {
        // A result reaching/exceeding `want` is refused (returns 0) without UB,
        // matching C's `len >= size` guard.
        let g = TempGz::new("printf_huge");
        let huge = "b".repeat(9000);
        let mut state = g.open_write();
        assert_eq!(gzprintf(&mut state, format_args!("{huge}")), 0);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        // Nothing was written, so the stream decodes to empty.
        assert_gzip_roundtrip(&g.read_raw(), b"");
    }

    #[test]
    fn gzflush_rejects_out_of_range() {
        let g = TempGz::new("flush_bad");
        let mut state = g.open_write();
        assert_eq!(gzflush(&mut state, -1), Z_STREAM_ERROR);
        assert_eq!(gzflush(&mut state, Z_FINISH + 1), Z_STREAM_ERROR);
        assert_eq!(gzclose_w(&mut state), Z_OK);
    }

    #[test]
    fn gzflush_sync_then_more_round_trips() {
        // A sync flush mid-stream pushes buffered data to the file; subsequent
        // writes append, and the whole stream still decodes to the concatenation.
        let g = TempGz::new("flush_sync");
        let first = b"first part ";
        let second = b"second part";
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, first), first.len() as i32);
        assert_eq!(gzflush(&mut state, Z_SYNC_FLUSH), Z_OK);
        assert_eq!(gzwrite(&mut state, second), second.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_gzip_roundtrip(&g.read_raw(), &expected);
    }

    #[test]
    fn gzfwrite_writes_full_items() {
        let g = TempGz::new("fwrite");
        let data = b"0123456789ABCDEF"; // 16 bytes
        let mut state = g.open_write();
        // 4 items of 4 bytes each.
        assert_eq!(gzfwrite(&mut state, 4, 4, data), 4);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn gzfwrite_overflow_guard_returns_zero() {
        // `item_size * nitems` overflows usize -> guard returns 0 (no UB) and
        // records Z_STREAM_ERROR, matching C's `len / size != nitems` check.
        let g = TempGz::new("fwrite_ovf");
        let mut state = g.open_write();
        assert_eq!(gzfwrite(&mut state, usize::MAX, 2, b"x"), 0);
        assert_eq!(state.err, Z_STREAM_ERROR);
    }

    #[test]
    fn set_params_before_write_round_trips() {
        // Changing parameters before the stream is initialized just records them;
        // the subsequent write uses level 9.
        let g = TempGz::new("params_pre");
        let mut state = g.open_write();
        assert_eq!(set_params(&mut state, 9, Z_DEFAULT_STRATEGY), Z_OK);
        assert_eq!(state.level, 9);
        let data = b"compress me with level 9 data data data data data data";
        assert_eq!(gzwrite(&mut state, data), data.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn set_params_mid_stream_round_trips() {
        // Changing parameters on a running stream flushes buffered input with
        // Z_BLOCK and calls deflateParams; the full output must still decode.
        let g = TempGz::new("params_mid");
        let first = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, first), first.len() as i32);
        assert_eq!(set_params(&mut state, 1, Z_DEFAULT_STRATEGY), Z_OK);
        assert_eq!(state.level, 1);
        assert_eq!(gzwrite(&mut state, second), second.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_gzip_roundtrip(&g.read_raw(), &expected);
    }

    #[test]
    fn finish_then_write_makes_multi_member_stream() {
        // gzflush(Z_FINISH) arms the reset flag so the next write starts a new
        // gzip member; the concatenation is a valid multi-member stream.
        let g = TempGz::new("multi");
        let first = b"member one payload";
        let second = b"member two payload";
        let mut state = g.open_write();
        assert_eq!(gzwrite(&mut state, first), first.len() as i32);
        assert_eq!(gzflush(&mut state, Z_FINISH), Z_OK);
        assert!(state.reset, "Z_FINISH must arm the deflate-reset flag");
        assert_eq!(gzwrite(&mut state, second), second.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);

        let raw = g.read_raw();
        assert_eq!(&raw[..3], &[0x1f, 0x8b, 0x08], "first member gzip magic");

        // The full multi-member stream decodes to first ++ second.
        let mut all = Vec::new();
        flate2::read::MultiGzDecoder::new(&raw[..])
            .read_to_end(&mut all)
            .expect("flate2 multi-member decode");
        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_eq!(all, expected);
    }

    #[test]
    fn direct_transparent_small_write_is_raw() {
        // In transparent (direct) mode no gzip wrapper is applied: the file
        // contains the raw input bytes verbatim.
        let g = TempGz::new("direct_small");
        let data = b"raw uncompressed passthrough bytes";
        let mut state = g.open_write();
        state.direct = 1;
        assert_eq!(gzwrite(&mut state, data), data.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_eq!(g.read_raw(), data, "direct write must be byte-for-byte raw");
    }

    #[test]
    fn direct_transparent_large_write_is_raw() {
        let g = TempGz::new("direct_large");
        let data = pseudo_random(20_000);
        let mut state = g.open_write();
        state.direct = 1;
        assert_eq!(gzwrite(&mut state, &data), data.len() as i32);
        assert_eq!(gzclose_w(&mut state), Z_OK);
        assert_eq!(g.read_raw(), data);
    }

    #[test]
    fn gz_writer_impl_write_round_trips() {
        // The idiomatic std::io::Write wrapper: write/flush, and Drop emits the
        // trailer automatically.
        let g = TempGz::new("writer");
        let data = b"streamed through the std::io::Write wrapper";
        {
            let state = g.open_write();
            let mut writer = GzWriter::new(state);
            writer.write_all(data).expect("Write::write_all");
            writer.flush().expect("Write::flush");
            // `writer` drops here -> finish() writes the trailer; then the inner
            // GzState drops, closing the file.
        }
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn gz_writer_explicit_finish_then_drop_is_noop() {
        let g = TempGz::new("writer_finish");
        let data = b"explicit finish";
        let state = g.open_write();
        let mut writer = GzWriter::new(state);
        writer.write_all(data).expect("write_all");
        assert_eq!(writer.finish(), Z_OK);
        // Borrow the inner state to prove finalization stuck (Drop will no-op).
        assert!(writer.state_mut().finalized);
        drop(writer);
        assert_gzip_roundtrip(&g.read_raw(), data);
    }

    #[test]
    fn write_apis_reject_non_write_handle() {
        let path = temp_path("notwrite");
        let file = File::create(&path).expect("create temp file");
        let mut state = GzState::new(file, path.to_string_lossy().into_owned(), Mode::Read);
        assert_eq!(gzwrite(&mut state, b"x"), 0);
        assert_eq!(gzfwrite(&mut state, 1, 1, b"x"), 0);
        assert_eq!(gzputc(&mut state, i32::from(b'x')), -1);
        assert_eq!(gzputs(&mut state, "x"), -1);
        assert_eq!(gzprintf(&mut state, format_args!("x")), Z_STREAM_ERROR);
        assert_eq!(gzflush(&mut state, Z_FINISH), Z_STREAM_ERROR);
        assert_eq!(
            set_params(&mut state, 1, Z_DEFAULT_STRATEGY),
            Z_STREAM_ERROR
        );
        assert_eq!(gzclose_w(&mut state), Z_STREAM_ERROR);
        let _ = std::fs::remove_file(&path);
    }
}
