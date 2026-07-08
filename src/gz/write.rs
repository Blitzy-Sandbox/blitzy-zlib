//! The **write** side of the gzip file-I/O layer (`gz*` writing API) — a
//! faithful, memory-safe Rust port of C `gzwrite.c`.
//!
//! This module implements the stdio-like `gz*` writing entry points
//! (`gzwrite`, `gzfwrite`, `gzputc`, `gzputs`, `gzprintf`, `gzflush`) plus the
//! internal compress pipeline (`gz_init`, `gz_comp`, `gz_zero`, `gz_write`)
//! that drives compression through the crate's DEFLATE engine
//! ([`crate::deflate`]). It operates entirely on the shared
//! [`GzState`](crate::gz::state::GzState) defined by the gz-layer state module,
//! and produces output that is **byte-identical** to reference zlib for the
//! same input, level, and strategy, framed as a valid gzip member (RFC 1952).
//!
//! # Relationship to C `gzwrite.c`
//!
//! The port preserves the C structure function-for-function (C source line
//! ranges are cited in comments), but replaces the raw `z_stream` cursor
//! quadruple (`next_in`/`avail_in`/`next_out`/`avail_out`) and the manual
//! `malloc`/`free` bookkeeping with safe Rust:
//!
//! * The idiomatic [`crate::deflate::deflate`] takes the input and output as
//!   **slices** per call and returns a [`DeflateOutcome`](crate::deflate::DeflateOutcome)
//!   (`consumed`/`produced`/`code`) — there is no persistent `next_out` cursor
//!   inside [`ZStream`](crate::stream::ZStream). This module therefore owns the
//!   buffering: the compressed output is flushed to the file after each
//!   `deflate` call, which is byte-identical to the C behaviour of accumulating
//!   in `state->out` before writing (the DEFLATE byte stream is independent of
//!   how the output is chunked).
//! * The C write path never uses the `gzFile_s.have` window field, so this port
//!   repurposes [`GzState::have`](crate::gz::state::GzState) to hold the count of
//!   **buffered, not-yet-compressed input bytes** (the C `strm.avail_in`). The
//!   buffered input always begins at `in_buf[0]` (the C `next_in == in`
//!   invariant), so pending input is exactly `in_buf[0..have]`.
//! * `deflateEnd`/`free`/`close` are subsumed by RAII: dropping the
//!   [`GzState`](crate::gz::state::GzState) releases the buffers, the engine
//!   state, and the OS file handle.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`**. All buffer access is bounds-checked
//! slice indexing; the raw C-string / raw-pointer handling for the FFI `gz*`
//! entry points lives in `src/ffi/gz.rs`, never here. The public
//! [`std::io::Write`] implementation on [`GzState`](crate::gz::state::GzState)
//! offers an idiomatic Rust API alongside the C-compatible functions.

// Compile-enforce the folder unsafe-policy: this module must contain zero
// `unsafe` (all raw-pointer / C-string handling for the FFI `gz*` entry points
// lives in `src/ffi/gz.rs`). Mirrors the sibling deflate modules.
#![deny(unsafe_code)]

use std::io::{self, Write};

use crate::constants::{DEF_MEM_LEVEL, FlushMode, MAX_WBITS, Strategy, Z_DEFLATED, Z_FINISH};
use crate::deflate;
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GzMode, GzState};

/// Returns `true` if `state` is a live write stream with no *serious* pending
/// error, i.e. it is ready to accept a new write request.
///
/// Mirrors the C guard used at the top of every public write entry point:
/// `state->mode != GZ_WRITE || (state->err != Z_OK && !state->again)`
/// (`gzwrite.c` L263-L264). A [`ReturnCode::BufError`]-style soft error is not
/// possible on the write path, so the check reduces to "mode is
/// [`GzMode::Write`] and either there is no error or a non-blocking retry is
/// pending".
#[inline]
fn write_ready(state: &GzState) -> bool {
    state.mode == GzMode::Write && (state.err == ReturnCode::Ok || state.again)
}

/// Initializes state for writing a gzip file — port of C `gz_init`
/// (`gzwrite.c` L11-L57).
///
/// Allocates the input buffer at **double** the requested size (the second
/// half gives [`gzvprintf`] room to format before compressing), and — unless
/// the stream is in transparent/`direct` mode — allocates the output buffer and
/// initializes the DEFLATE engine for **gzip** framing (`windowBits =
/// MAX_WBITS + 16`, C L35). Initialization is marked complete by setting
/// [`GzState::size`](crate::gz::state::GzState) to the (non-zero) buffer size,
/// exactly as the C sentinel does.
///
/// # Errors
///
/// Returns [`ZlibError::MemError`] if the engine initialization fails (the C
/// code reports every `gz_init` failure as `Z_MEM_ERROR` / "out of memory",
/// C L38-L42); the state's error is set via
/// [`GzState::error`](crate::gz::state::GzState::error) as a side effect.
pub(crate) fn gz_init(state: &mut GzState) -> Result<(), ZlibError> {
    // Allocate the input buffer, double-sized for `gzprintf` (C L14-L19).
    state.in_buf = vec![0u8; state.want << 1];

    // Only need an output buffer and a deflate engine when compressing
    // (C L22-L44); a `direct` stream writes straight to the file.
    if state.direct == 0 {
        // Allocate the output buffer (C L23-L29).
        state.out_buf = vec![0u8; state.want];

        // Set up for gzip compression. `MAX_WBITS + 16` selects gzip
        // header/trailer framing inside the engine (C L34-L36). The gzip
        // CRC-32 and ISIZE trailer are produced internally by the engine, so
        // this layer never touches `crate::checksum` directly.
        let strategy = Strategy::from_c_int(state.strategy).unwrap_or(Strategy::Default);
        if deflate::deflate_init2(
            &mut state.strm,
            state.level,
            Z_DEFLATED,
            MAX_WBITS + 16,
            DEF_MEM_LEVEL,
            strategy,
        )
        .is_err()
        {
            // C L37-L43: any init failure is surfaced as an out-of-memory error.
            state.error(ReturnCode::MemError, Some("out of memory"));
            return Err(ZlibError::MemError);
        }
    }

    // Mark the state as initialized (C L46). The compressed-output window
    // (C L49-L55) is modelled implicitly: `out_buf[0..size]` is the scratch
    // area handed to `deflate` on each call, and it is fully drained to the
    // file within `gz_comp`, so no persistent output cursor is required.
    state.size = state.want;
    Ok(())
}

/// The maximum number of bytes written to the file in a single `write` call —
/// the C `max = ((unsigned)-1 >> 2) + 1` cap (`gzwrite.c` L67), preserved so a
/// pathologically large buffer cannot overflow the platform's write count.
const WRITE_MAX: usize = (u32::MAX >> 2) as usize + 1;

/// Writes `input` straight to the file with no compression — the transparent
/// (`direct`) branch of C `gz_comp` (`gzwrite.c` L74-L95).
///
/// Returns the number of bytes written. On a non-blocking stall
/// ([`io::ErrorKind::WouldBlock`]) it sets [`GzState::again`](crate::gz::state::GzState)
/// and reports [`ReturnCode::ErrNo`]; on any other write failure it likewise
/// reports [`ReturnCode::ErrNo`].
fn write_direct(state: &mut GzState, input: &[u8]) -> Result<usize, ZlibError> {
    let mut off = 0usize;
    while off < input.len() {
        state.again = false;
        let end = off + core::cmp::min(WRITE_MAX, input.len() - off);
        match state.file.write(&input[off..end]) {
            Ok(0) => break, // no progress possible on a real file with a non-empty slice
            Ok(written) => off += written,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                state.again = true;
                state.error(ReturnCode::ErrNo, Some("write error"));
                return Err(ZlibError::ErrNo);
            }
            Err(_) => {
                state.error(ReturnCode::ErrNo, Some("write error"));
                return Err(ZlibError::ErrNo);
            }
        }
    }
    Ok(off)
}

/// Runs `deflate` over `input`, writing every produced byte to the file, until
/// the engine has no more work for the requested `flush` — the compress loop of
/// C `gz_comp` (`gzwrite.c` L109-L142). Returns the number of input bytes
/// consumed.
///
/// `input` must **not** alias any field of `state` (it is either an external
/// caller slice or the `in_buf` moved out with [`core::mem::take`]), which is
/// what lets the loop borrow [`GzState::strm`](crate::gz::state::GzState),
/// [`GzState::out_buf`](crate::gz::state::GzState), and
/// [`GzState::file`](crate::gz::state::GzState) as disjoint fields.
///
/// Unlike C — which accumulates output in `state->out` and writes only when the
/// buffer fills or a flush occurs — this drains `out_buf` after every `deflate`
/// call. The DEFLATE byte stream is identical either way; only the number of
/// `write` syscalls differs.
///
/// # Errors
///
/// * [`ZlibError::StreamError`] if `deflate` reports a corrupt stream
///   (C L136-L140) — fatal.
/// * [`ZlibError::ErrNo`] on a file write error (C L119-L124), including a
///   non-blocking stall (which also sets [`GzState::again`](crate::gz::state::GzState)).
fn gz_deflate_loop(
    state: &mut GzState,
    input: &[u8],
    flush: FlushMode,
) -> Result<usize, ZlibError> {
    let size = state.size;
    let flush_i32 = flush.as_c_int();
    let mut consumed = 0usize;

    loop {
        // Compress the still-unconsumed tail of `input` into the full output
        // scratch buffer. `input` is external, and `strm`/`out_buf` are
        // distinct `state` fields, so these borrows are disjoint.
        let outcome = deflate::deflate(
            &mut state.strm,
            &input[consumed..],
            &mut state.out_buf[..size],
            flush_i32,
        );

        // A corrupt stream is fatal (C L136-L140).
        if outcome.code == ReturnCode::StreamError {
            state.error(
                ReturnCode::StreamError,
                Some("internal error: deflate stream corrupt"),
            );
            return Err(ZlibError::StreamError);
        }

        consumed += outcome.consumed;

        // Flush the freshly produced bytes to the file (C L114-L124). Drain with
        // an explicit, progress-preserving write loop — the same pattern as the
        // transparent [`write_direct`] path and the C inner
        // `while (strm->next_out > state->x.next)` loop (gzwrite.c L114-L124),
        // which advances the output cursor by each successful `write()`.
        //
        // `write_all` is deliberately avoided here: it hides how many bytes were
        // written before a later error, so a partial write followed by a fault
        // (or a non-blocking / short-writing descriptor) could lose progress and
        // corrupt the gzip output — or duplicate bytes on retry. Advancing `off`
        // by each successful count keeps the exact write progress, retries a
        // slice interrupted by a signal, and reports a non-blocking stall via
        // [`GzState::again`](crate::gz::state::GzState), matching zlib.
        if outcome.produced > 0 {
            let produced = outcome.produced;
            let mut off = 0usize;
            while off < produced {
                state.again = false;
                let end = off + core::cmp::min(WRITE_MAX, produced - off);
                match state.file.write(&state.out_buf[off..end]) {
                    // No progress possible on a real file with a non-empty slice.
                    Ok(0) => break,
                    Ok(written) => off += written,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        state.again = true;
                        state.error(ReturnCode::ErrNo, Some("write error"));
                        return Err(ZlibError::ErrNo);
                    }
                    Err(_) => {
                        state.error(ReturnCode::ErrNo, Some("write error"));
                        return Err(ZlibError::ErrNo);
                    }
                }
            }
        }

        let input_left = input.len() - consumed;
        let output_was_full = outcome.produced == size;
        // While finishing, keep going until the engine emits `Z_STREAM_END`
        // (the final block plus the gzip trailer have all been produced).
        let finishing = flush == FlushMode::Finish && outcome.code != ReturnCode::StreamEnd;

        // The engine is done for this flush once all input is consumed, the
        // output was not completely filled (so nothing is pending), and we are
        // not mid-finish. This mirrors the C `while (have)` guard, where
        // `have` is the bytes produced by the last `deflate`.
        if input_left == 0 && !output_was_full && !finishing {
            break;
        }

        // Safety valve: if a call made no progress at all and we are not
        // finishing, stop rather than spin forever.
        if outcome.consumed == 0 && outcome.produced == 0 && !finishing {
            break;
        }
    }

    Ok(consumed)
}

/// Compress-and-write step over an explicit `input` slice — the body of C
/// `gz_comp` (`gzwrite.c` L65-L148) minus the lazy buffer allocation (callers
/// guarantee the buffers are initialized). Returns the bytes consumed.
///
/// Handles the transparent (`direct`) branch, the pending-reset branch, the
/// compress loop, and the post-`Z_FINISH` reset arm. `input` must not alias any
/// `state` field (see [`gz_deflate_loop`]).
fn gz_comp_slice(state: &mut GzState, input: &[u8], flush: FlushMode) -> Result<usize, ZlibError> {
    // Write directly if requested (C L73-L96).
    if state.direct == 1 {
        return write_direct(state, input);
    }

    // Check for a pending reset (C L98-L108): after a `Z_FINISH` a new gzip
    // member must not be started until there is data to write and we are not
    // merely flushing.
    if state.reset {
        if input.is_empty() && flush == FlushMode::NoFlush {
            return Ok(0);
        }
        if deflate::deflate_reset(&mut state.strm).is_err() {
            state.error(
                ReturnCode::StreamError,
                Some("internal error: deflate stream corrupt"),
            );
            return Err(ZlibError::StreamError);
        }
        state.reset = false;
    }

    // Run deflate() until it produces no more output (C L110-L142).
    let consumed = gz_deflate_loop(state, input, flush)?;

    // If that completed a deflate stream, allow another to start (C L144-L146).
    if flush == FlushMode::Finish {
        state.reset = true;
    }

    Ok(consumed)
}

/// Compress whatever is buffered in `in_buf[0..have]` and write it to the file
/// — the public face of C `gz_comp` (`gzwrite.c` L65-L148).
///
/// This is the shared workhorse consumed by [`gzflush`] (here), `gzsetparams`
/// (in `open.rs`), and `gzclose_w` (in `close.rs`), so it is `pub(crate)`. It
/// lazily allocates the buffers on first use, then delegates to
/// [`gz_comp_slice`] over the buffered input. The buffered input is moved out
/// with [`core::mem::take`] so the slice does not alias `state`; on success all
/// of it is consumed, so [`GzState::have`](crate::gz::state::GzState) is reset
/// to `0`.
///
/// # Errors
///
/// Propagates the [`ZlibError`] from [`gz_init`] (memory) or [`gz_comp_slice`]
/// (a corrupt stream or a file write error); the state's error code is set as a
/// side effect.
pub(crate) fn gz_comp(state: &mut GzState, flush: FlushMode) -> Result<(), ZlibError> {
    // Allocate memory if this is the first time through (C L70-L71).
    if state.size == 0 {
        gz_init(state)?;
    }

    // Move the input buffer out so the borrow of its `[0..have]` slice does not
    // conflict with the `&mut state` handed to `gz_comp_slice`. `take` leaves a
    // cheap empty `Vec` behind, which is swapped back immediately afterwards.
    let buf = core::mem::take(&mut state.in_buf);
    let have = state.have;
    let result = gz_comp_slice(state, &buf[..have], flush);
    state.in_buf = buf;

    // On success every buffered byte was consumed by the engine.
    if result.is_ok() {
        state.have = 0;
    }

    result.map(|_| ())
}

/// Compress [`GzState::skip`](crate::gz::state::GzState) zero bytes to the
/// output — port of C `gz_zero` (`gzwrite.c` L154-L182).
///
/// Used to satisfy a forward seek on a write stream: the gap is realized by
/// compressing the requested number of zero bytes. Any buffered input is
/// flushed first, then zeros are fed in `size`-sized chunks.
///
/// # Errors
///
/// Propagates the [`ZlibError`] from [`gz_comp`].
pub(crate) fn gz_zero(state: &mut GzState) -> Result<(), ZlibError> {
    // Consume whatever is left in the input buffer (C L160-L162).
    if state.have != 0 {
        gz_comp(state, FlushMode::NoFlush)?;
    }

    // Compress `skip` zero bytes (C L164-L181).
    let mut first = true;
    while state.skip > 0 {
        // n = min(size, skip), guarded against the size not fitting in the
        // 64-bit offset (C `GT_OFF`); `size` is a small buffer size and `skip`
        // is positive here, so a plain minimum suffices.
        let n = core::cmp::min(state.size as u64, state.skip as u64) as usize;

        // Only need to zero the buffer once, then it is reused (C L167-L170).
        if first {
            for b in state.in_buf[..n].iter_mut() {
                *b = 0;
            }
            first = false;
        }

        // Present the `n` zero bytes as the buffered input and compress them.
        state.have = n;
        gz_comp(state, FlushMode::NoFlush)?;
        state.pos += n as i64;
        state.skip -= n as i64;
    }

    Ok(())
}

/// Write `buf` to the file, returning the number of bytes consumed — port of
/// C `gz_write` (`gzwrite.c` L188-L252).
///
/// Small writes are copied into the input buffer and compressed once it fills
/// (amortizing the per-call engine overhead); a write at least as large as the
/// buffer first drains any buffered input, then feeds the caller's slice
/// directly to the engine in `u32::MAX`-sized chunks. Both paths produce
/// identical DEFLATE output.
///
/// If the returned value is less than `buf.len()`, an error occurred. For a
/// non-blocking stall the count of bytes accepted so far is returned (the
/// caller may retry); for any other error `0` is returned.
pub(crate) fn gz_write(state: &mut GzState, buf: &[u8]) -> usize {
    // If len is zero, avoid unnecessary operations (C L193-L194).
    if buf.is_empty() {
        return 0;
    }

    // Allocate memory if this is the first time through (C L197-L198).
    if state.size == 0 && gz_init(state).is_err() {
        return 0;
    }

    // Check for a seek request (C L201-L202).
    if state.skip != 0 && gz_zero(state).is_err() {
        return 0;
    }

    let put = buf.len();

    if buf.len() < state.size {
        // For small len, copy to the input buffer, compressing when full
        // (C L205-L226).
        let mut off = 0usize;
        loop {
            // Free region of the input buffer. Buffered input occupies
            // `in_buf[0..have]` (C `next_in == in`), so the next free byte is at
            // `have`. `saturating_sub` guards the (post-`gzprintf`) case where
            // `have` momentarily exceeds `size`.
            let have = state.have;
            let copy = core::cmp::min(state.size.saturating_sub(have), buf.len() - off);
            state.in_buf[have..have + copy].copy_from_slice(&buf[off..off + copy]);
            state.have += copy;
            state.pos += copy as i64;
            off += copy;

            if off == buf.len() {
                break;
            }

            // The input buffer is full — compress it (C L224-L225).
            if gz_comp(state, FlushMode::NoFlush).is_err() {
                // Report partial progress only on a non-blocking stall.
                return if state.again { off } else { 0 };
            }
        }
    } else {
        // Consume whatever is left in the input buffer (C L229-L230).
        if state.have != 0 && gz_comp(state, FlushMode::NoFlush).is_err() {
            return 0;
        }

        // Directly compress the user buffer to the file (C L233-L247). The
        // caller's slice is external, so feeding it to the engine involves no
        // extra copy and no aliasing with `state`.
        let mut off = 0usize;
        while off < buf.len() {
            let n = core::cmp::min(u32::MAX as usize, buf.len() - off);
            match gz_comp_slice(state, &buf[off..off + n], FlushMode::NoFlush) {
                Ok(consumed) => {
                    state.pos += consumed as i64;
                    off += consumed;
                    if consumed < n {
                        // A short consume can only mean a non-blocking stall.
                        return if state.again { off } else { 0 };
                    }
                }
                Err(_) => {
                    return if state.again { off } else { 0 };
                }
            }
        }
    }

    // Input was all buffered or compressed (C L251).
    put
}

/// Write `buf` to the gzip file — port of C `gzwrite` (`gzwrite.c` L255-L277).
///
/// Returns the number of bytes written, or `0` on error or if the stream is not
/// writable. Because the count is returned as an [`i32`], a length that does not
/// fit in a positive [`i32`] is rejected with [`ReturnCode::DataError`], exactly
/// as the C interface guards against its `unsigned`→`int` return.
pub fn gzwrite(state: &mut GzState, buf: &[u8]) -> i32 {
    // Check that we're writing and that there's no serious error (C L263-L266).
    if !write_ready(state) {
        return 0;
    }
    state.clear_error();

    // Since an int is returned, make sure the length fits in one (C L270-L273).
    if buf.len() > i32::MAX as usize {
        state.error(
            ReturnCode::DataError,
            Some("requested length does not fit in int"),
        );
        return 0;
    }

    // Write the bytes (the return value now fits in an int, C L276).
    gz_write(state, buf) as i32
}

/// Write `nitems` items of `size` bytes each to the gzip file — port of C
/// `gzfwrite` (`gzwrite.c` L280-L304).
///
/// `buf` must contain at least `size * nitems` bytes. This precondition is
/// enforced: if `buf` is smaller than the requested `size * nitems`, the request
/// is rejected with [`ReturnCode::StreamError`] and `0` is returned — the write
/// is **not** silently truncated to `buf.len()`, so a caller-side sizing mistake
/// surfaces as an error instead of being hidden (and the C-compatible request
/// contract the FFI layer relies on is preserved). Returns the number of full
/// items written; `0` on error or overflow. The `size * nitems` multiplication
/// is overflow-checked, matching the C guard that the request fits in a
/// `size_t`.
pub fn gzfwrite(state: &mut GzState, buf: &[u8], size: usize, nitems: usize) -> usize {
    // Check that we're writing and that there's no serious error (C L289-L292).
    if !write_ready(state) {
        return 0;
    }
    state.clear_error();

    // Compute the number of bytes to write, erroring on overflow (C L295-L299).
    let len = match nitems.checked_mul(size) {
        Some(len) => len,
        None => {
            state.error(
                ReturnCode::StreamError,
                Some("request does not fit in a size_t"),
            );
            return 0;
        }
    };

    // Write `len` bytes, returning the number of full items written (C L303).
    if len == 0 {
        return 0;
    }

    // Enforce the documented `buf` >= `size * nitems` precondition. C uses a raw
    // pointer and trusts the caller to have provided a large enough buffer; the
    // safe-slice API instead surfaces an undersized buffer as a stream error and
    // writes nothing, rather than silently clamping the write to `buf.len()`
    // (which would hide the caller's mistake and let the returned item count
    // diverge from the C `gz_write(state, buf, len) / size` contract).
    if buf.len() < len {
        state.error(
            ReturnCode::StreamError,
            Some("input buffer smaller than requested size * nitems"),
        );
        return 0;
    }

    gz_write(state, &buf[..len]) / size
}

/// Write one byte `c` (its low 8 bits) to the gzip file — port of C `gzputc`
/// (`gzwrite.c` L307-L347).
///
/// Returns the byte written (`c & 0xff`) on success, or `-1` on error. The fast
/// path appends straight into the input buffer when there is room, avoiding a
/// call into `gz_write`.
pub fn gzputc(state: &mut GzState, c: i32) -> i32 {
    // Check that we're writing and that there's no serious error (C L318-L321).
    if !write_ready(state) {
        return -1;
    }
    state.clear_error();

    // Check for a seek request (C L324-L325).
    if state.skip != 0 && gz_zero(state).is_err() {
        return -1;
    }

    // Try writing to the input buffer for speed (C L329-L340). `size == 0` means
    // the buffers are not initialized yet, in which case fall through.
    if state.size != 0 {
        let have = state.have;
        if have < state.size {
            state.in_buf[have] = c as u8;
            state.have += 1;
            state.pos += 1;
            return c & 0xff;
        }
    }

    // No room in the buffer (or not initialized) — use gz_write (C L343-L346).
    let byte = [c as u8];
    if gz_write(state, &byte) != 1 {
        return -1;
    }
    c & 0xff
}

/// Write the string `s` (its bytes) to the gzip file — port of C `gzputs`
/// (`gzwrite.c` L350-L372).
///
/// Returns the number of bytes written, or `-1` on error. Mirrors the C guard
/// that the string length fits in an [`i32`].
pub fn gzputs(state: &mut GzState, s: &str) -> i32 {
    // Check that we're writing and that there's no serious error (C L358-L361).
    if !write_ready(state) {
        return -1;
    }
    state.clear_error();

    // Length, with the "fits in an int" guard (C L364-L368).
    let len = s.len();
    if len > i32::MAX as usize {
        state.error(
            ReturnCode::StreamError,
            Some("string length does not fit in int"),
        );
        return -1;
    }

    // Write the string; a non-empty string that wrote nothing is an error
    // (C L369-L371).
    let put = gz_write(state, s.as_bytes());
    if len != 0 && put == 0 { -1 } else { put as i32 }
}

/// If the second half of the input buffer is occupied, write out the contents;
/// if input remains after a non-blocking stall, keep it at the front of the
/// buffer — port of C `gz_vacate` (`gzwrite.c` L382-L396).
///
/// Returns `true` if this did **not** free up the second half of the buffer
/// (i.e. more than `size` bytes are still buffered). The caller must inspect
/// [`GzState::err`](crate::gz::state::GzState) afterwards to detect a
/// [`gz_comp`] error. Because this port maintains the `next_in == in` invariant
/// (buffered input always starts at `in_buf[0]`), the C `memmove` that slides
/// leftover input to the front is a no-op and is omitted.
fn gz_vacate(state: &mut GzState) -> bool {
    // If the current contents fit within the first `size` bytes, the second
    // half is already free (C L389-L390): buffered input is `in_buf[0..have]`.
    if state.have <= state.size {
        return false;
    }

    // Otherwise write out what we can (C L391); the error is left on `state`
    // for the caller to observe.
    let _ = gz_comp(state, FlushMode::NoFlush);

    // If everything drained, the buffer is empty and starts at the front
    // (C L392-L395).
    if state.have == 0 {
        return false;
    }

    // Leftover remains (only on a non-blocking stall); it already starts at
    // `in_buf[0]`, so report whether it still exceeds `size` (C L396).
    state.have > state.size
}

/// Formatted write to the gzip file — port of C `gzvprintf` (`gzwrite.c`
/// L420-L485, the `gz_vacate`-based variant).
///
/// Because Rust has no C variadics, the formatted text is supplied as
/// [`core::fmt::Arguments`] (produced by [`format_args!`]); the raw
/// `gzprintf(file, fmt, ...)` C entry point is reconstructed only in
/// `src/ffi/gz.rs`. The formatted bytes are staged in the free second half of
/// the double-sized input buffer and then compressed, exactly mirroring the C
/// `vsnprintf`-into-`next` sequence.
///
/// Returns the number of formatted bytes written, `0` if the result did not fit
/// in the buffer, or a negative [`ReturnCode`] code on error.
pub fn gzvprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    // Check that we're writing and that there's no serious error (C L436-L439).
    if !write_ready(state) {
        return ReturnCode::StreamError.as_c_int();
    }
    state.clear_error();

    // Make sure we have some buffer space (C L442-L443).
    if state.size == 0 && gz_init(state).is_err() {
        return state.err.as_c_int();
    }

    // Check for a seek request (C L446-L447).
    if state.skip != 0 && gz_zero(state).is_err() {
        return state.err.as_c_int();
    }

    // Ensure the second half of the input buffer is free to format into
    // (C L455). A stall that leaves the second half occupied is reported as a
    // retryable Z_BUF_ERROR (C L456-L468).
    let stalled = gz_vacate(state);
    if state.err != ReturnCode::Ok {
        if stalled && state.again {
            state.error(ReturnCode::BufError, Some("stalled write on gzprintf"));
        }
        if !state.again {
            return state.err.as_c_int();
        }
    }

    // Format into the free region. The buffered input occupies `in_buf[0..have]`
    // (`next_in == in`), so formatting begins at offset `have`; the buffer is
    // double-sized, so at least `size` bytes are available there (C L471-L473).
    let start = state.have;
    let mut formatted = String::new();
    let _ = core::fmt::write(&mut formatted, args);
    let bytes = formatted.as_bytes();
    let len = bytes.len();

    // Check that the result fits in the buffer (C L488-L489): C requires the
    // formatted length to be non-zero and strictly less than `size`.
    if len == 0 || len >= state.size {
        return 0;
    }

    // Copy the formatted bytes in and update the buffer and position
    // (C L492-L494).
    state.in_buf[start..start + len].copy_from_slice(bytes);
    state.have += len;
    state.pos += len as i64;

    // Write out the buffer if more than half is now occupied (C L497-L499).
    let _ = gz_vacate(state);
    if state.err != ReturnCode::Ok && !state.again {
        return state.err.as_c_int();
    }

    len as i32
}

/// Formatted write to the gzip file — the idiomatic wrapper over [`gzvprintf`]
/// standing in for C `gzprintf` (`gzwrite.c` L486-L494).
///
/// Rust callers pass the formatted text via [`format_args!`], e.g.
/// `gzprintf(state, format_args!("{}", value))`.
#[inline]
pub fn gzprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    gzvprintf(state, args)
}

/// Flush pending output to the gzip file with the requested `flush` mode —
/// port of C `gzflush` (`gzwrite.c` L540-L560).
///
/// Returns [`ReturnCode::Ok`] (`0`) on success or the appropriate negative code
/// on error. `flush` must be in `0..=Z_FINISH`.
pub fn gzflush(state: &mut GzState, flush: i32) -> i32 {
    // Check that we're writing and that there's no serious error (C L546-L549).
    if !write_ready(state) {
        return ReturnCode::StreamError.as_c_int();
    }
    state.clear_error();

    // Check the flush parameter (C L552-L553).
    if !(0..=Z_FINISH).contains(&flush) {
        return ReturnCode::StreamError.as_c_int();
    }
    let flush_mode = FlushMode::from_c_int(flush).unwrap_or(FlushMode::NoFlush);

    // Check for a seek request (C L556-L557).
    if state.skip != 0 && gz_zero(state).is_err() {
        return state.err.as_c_int();
    }

    // Compress the remaining data with the requested flush (C L559-L560); the
    // resulting error state is reported via `state.err`.
    let _ = gz_comp(state, flush_mode);
    state.err.as_c_int()
}

/// Finalize a write stream by flushing with [`FlushMode::Finish`], returning the
/// resulting [`ReturnCode`].
///
/// This keeps the write-specific finalization logic in this module: it produces
/// the final block and the gzip CRC-32 + ISIZE trailer (a `Z_FINISH`), after
/// which the buffers, engine, and file are released by
/// [`GzState`](crate::gz::state::GzState)'s `Drop` — the RAII replacement for
/// the C `deflateEnd`/`free`/`close` tail of `gzclose_w` (`gzwrite.c`
/// L667-L700). It is exercised by this module's unit tests and reserved as the
/// write-side finalizer.
///
/// `#[allow(dead_code)]`: `gzclose_w` (in `close.rs`) does **not** call this
/// helper — it inlines a C-exact finalize because this function returns
/// `state.err` on a *successful* flush, which would let a pre-existing non-fatal
/// error (e.g. `Z_ERRNO`) override `gzclose_w`'s result even when the `Z_FINISH`
/// itself succeeded, diverging from C's "only a failed finalize sets `ret`"
/// contract. The function is therefore unused by the library target (only by
/// tests) but is deliberately retained.
#[allow(dead_code)]
pub(crate) fn finish_write(state: &mut GzState) -> ReturnCode {
    match gz_comp(state, FlushMode::Finish) {
        Ok(()) => state.err,
        Err(e) => e.as_return_code(),
    }
}

/// An idiomatic [`std::io::Write`] view of a write-mode gzip file, so Rust
/// callers can use `write!`, `writeln!`, and [`Write::write_all`] alongside the
/// C-compatible `gz*` functions.
///
/// [`Write::write`] delegates to `gz_write`; [`Write::flush`] delegates to
/// `gz_comp` with [`FlushMode::SyncFlush`], which flushes all pending output
/// to the underlying file and aligns to a byte boundary **without** ending the
/// gzip member (finishing is reserved for `gzclose_w`/`finish_write`).
/// [`ZlibError`] is mapped to an [`io::Error`].
impl Write for GzState {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !write_ready(self) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip stream is not open for writing",
            ));
        }
        self.clear_error();

        let n = gz_write(self, buf);
        if n == 0 {
            // A non-empty write that consumed nothing is an error; surface the
            // recorded stream error (or a generic one if none was set).
            return Err(match ZlibError::from_return_code(self.err) {
                Some(e) => io::Error::other(e),
                None => io::Error::new(io::ErrorKind::WriteZero, "gzip write made no progress"),
            });
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        gz_comp(self, FlushMode::SyncFlush).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    //! Unit and round-trip tests for the gzip write side.
    //!
    //! The round-trip tests write data through the `gz*` API, finalize with
    //! [`finish_write`], then decompress the resulting file with the reference
    //! `flate2` decoder (its pure-Rust `miniz_oxide` backend), asserting the
    //! recovered bytes equal the original. This validates both that valid gzip
    //! (RFC 1952) framing is produced and that the output is decodable by an
    //! independent implementation. All buffer handling here is safe Rust —
    //! there is **zero `unsafe`** in this module.

    use super::*;

    use crate::gz::state::How;
    use crate::stream::ZStream;
    use std::fs::File;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use flate2::read::GzDecoder;

    /// `Z_SYNC_FLUSH` numeric flush code (mirrors `FlushMode::SyncFlush`).
    const Z_SYNC_FLUSH: i32 = 2;

    /// Generates a unique, process- and counter-tagged temporary file path.
    ///
    /// The `blitzy_adhoc_test_` prefix keeps these files out of any commit and
    /// makes them easy to clean up.
    fn temp_path(tag: &str) -> PathBuf {
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "blitzy_adhoc_test_gzwrite_{tag}_{}_{n}.gz",
            std::process::id()
        ));
        p
    }

    /// Builds a fresh write-mode [`GzState`] backed by a real, truncated temp
    /// file. `size` is left `0` so the first write lazily runs [`gz_init`],
    /// exactly as a real `gzopen` would.
    fn new_write_state(
        path: &Path,
        want: usize,
        level: i32,
        strategy: i32,
        direct: i32,
    ) -> GzState {
        let file = File::create(path).expect("create writable temp file");
        GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode: GzMode::Write,
            file,
            path: path.display().to_string(),
            size: 0,
            want,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct,
            how: How::Look,
            junk: 0,
            again: false,
            in_next: 0,
            in_avail: 0,
            start: 0,
            eof: false,
            past: false,
            level,
            strategy,
            reset: false,
            skip: 0,
            err: ReturnCode::Ok,
            msg: None,
            msg_c: None,
            strm: ZStream::new(),
        }
    }

    /// Decompresses a complete gzip member with the reference `flate2` decoder.
    fn gunzip(compressed: &[u8]) -> Vec<u8> {
        let mut decoder = GzDecoder::new(compressed);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("output is a valid gzip member");
        out
    }

    /// Reads the whole file at `path`, then removes it, returning the bytes.
    fn read_and_remove(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("read compressed output");
        let _ = std::fs::remove_file(path);
        bytes
    }

    #[test]
    fn write_ready_requires_write_mode_and_no_serious_error() {
        let path = temp_path("ready");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);
        assert!(write_ready(&state), "fresh write stream is ready");

        state.mode = GzMode::Read;
        assert!(!write_ready(&state), "a read stream is not ready to write");

        state.mode = GzMode::Write;
        state.err = ReturnCode::DataError;
        assert!(!write_ready(&state), "a serious error blocks writing");

        state.again = true;
        assert!(
            write_ready(&state),
            "a pending non-blocking retry is writable"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_init_allocates_buffers_and_engine_for_gzip() {
        let path = temp_path("init_gzip");
        let mut state = new_write_state(&path, 128, 6, 0, 0);
        gz_init(&mut state).expect("gz_init succeeds");

        // Input buffer is double-sized; output buffer is single-sized.
        assert_eq!(state.in_buf.len(), 256, "input buffer is want << 1");
        assert_eq!(state.out_buf.len(), 128, "output buffer is want");
        assert_eq!(state.size, 128, "size marks the buffers as initialized");
        assert!(state.strm.is_deflate(), "a deflate engine was initialized");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_init_direct_skips_output_buffer_and_engine() {
        let path = temp_path("init_direct");
        let mut state = new_write_state(&path, 128, 6, 0, 1);
        gz_init(&mut state).expect("gz_init succeeds");

        assert_eq!(
            state.in_buf.len(),
            256,
            "input buffer is still double-sized"
        );
        assert!(state.out_buf.is_empty(), "no output buffer in direct mode");
        assert_eq!(state.size, 128, "size still marks initialization");
        assert!(
            !state.strm.is_deflate(),
            "no deflate engine for a transparent stream"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gz_write_empty_is_noop() {
        let path = temp_path("empty");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);
        assert_eq!(gz_write(&mut state, &[]), 0, "empty write consumes nothing");
        assert_eq!(state.size, 0, "an empty write does not trigger gz_init");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn roundtrip_small_gzwrite() {
        let path = temp_path("small");
        let data = b"The quick brown fox jumps over the lazy dog.";
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            assert_eq!(gzwrite(&mut state, data), data.len() as i32);
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert!(
            compressed.starts_with(&[0x1f, 0x8b, 0x08]),
            "gzip magic + deflate method are present"
        );
        assert_eq!(gunzip(&compressed), data, "round-trips to the original");
    }

    #[test]
    fn roundtrip_crosses_small_buffer() {
        let path = temp_path("large");
        // ~9 KiB of semi-repetitive data, far larger than the tiny buffer, so
        // both the small-write-buffering and large-write-direct-feed paths run.
        let mut data = Vec::new();
        for i in 0..1024u32 {
            data.extend_from_slice(format!("line number {i:05}\n").as_bytes());
        }
        {
            let mut state = new_write_state(&path, 64, 9, 0, 0);
            assert_eq!(gzwrite(&mut state, &data), data.len() as i32);
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(gunzip(&compressed), data, "large input round-trips");
    }

    #[test]
    fn roundtrip_mixed_putc_puts_printf() {
        let path = temp_path("mixed");
        let mut expected = Vec::new();
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);

            assert_eq!(gzputc(&mut state, b'H' as i32), b'H' as i32);
            expected.push(b'H');

            assert_eq!(gzputs(&mut state, "ello, "), 6);
            expected.extend_from_slice(b"ello, ");

            let n = gzprintf(&mut state, format_args!("{}+{}={}", 2, 3, 5));
            assert_eq!(n, 5, "gzprintf reports the formatted length");
            expected.extend_from_slice(b"2+3=5");

            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(gunzip(&compressed), expected, "mixed API round-trips");
    }

    #[test]
    fn gzputc_uses_input_buffer_fast_path() {
        let path = temp_path("putc");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);

        // First putc lazily initializes (via gz_write) and buffers one byte.
        assert_eq!(gzputc(&mut state, 0x41), 0x41);
        assert_eq!(state.have, 1);
        assert_eq!(state.in_buf[0], 0x41);

        // Second putc takes the fast path (size != 0, room available).
        assert_eq!(gzputc(&mut state, 0x42), 0x42);
        assert_eq!(state.have, 2);
        assert_eq!(state.in_buf[1], 0x42);
        assert_eq!(state.pos, 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn direct_mode_writes_transparently() {
        let path = temp_path("direct");
        let data = b"raw passthrough, no gzip framing here";
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 1);
            assert_eq!(gzwrite(&mut state, data), data.len() as i32);
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let written = read_and_remove(&path);
        assert_eq!(written, data, "direct mode writes the bytes verbatim");
    }

    #[test]
    fn all_levels_and_strategies_roundtrip() {
        // Data with both structure and noise, exercising every strategy.
        let data: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        for level in [0, 1, 6, 9, -1] {
            for strategy in [0, 1, 2, 3, 4] {
                let path = temp_path("lvlstrat");
                {
                    let mut state = new_write_state(&path, 8192, level, strategy, 0);
                    assert_eq!(
                        gzwrite(&mut state, &data),
                        data.len() as i32,
                        "level {level} strategy {strategy} accepts all input"
                    );
                    assert_eq!(
                        finish_write(&mut state),
                        ReturnCode::Ok,
                        "level {level} strategy {strategy} finalizes cleanly"
                    );
                }
                let compressed = read_and_remove(&path);
                assert_eq!(
                    gunzip(&compressed),
                    data,
                    "level {level} strategy {strategy} round-trips"
                );
            }
        }
    }

    #[test]
    fn gzflush_sync_then_continue() {
        let path = temp_path("flush");
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            assert_eq!(gzwrite(&mut state, b"first-"), 6);
            assert_eq!(
                gzflush(&mut state, Z_SYNC_FLUSH),
                0,
                "sync flush returns Z_OK"
            );
            assert_eq!(gzwrite(&mut state, b"second"), 6);
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(
            gunzip(&compressed),
            b"first-second",
            "data spanning a sync flush round-trips"
        );
    }

    #[test]
    fn gzflush_rejects_out_of_range_flush() {
        let path = temp_path("flushbad");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);
        assert_eq!(gzflush(&mut state, 99), ReturnCode::StreamError.as_c_int());
        assert_eq!(gzflush(&mut state, -1), ReturnCode::StreamError.as_c_int());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzfwrite_returns_full_item_count() {
        let path = temp_path("fwrite");
        let buf = b"ABCDEFGHIJKL"; // 12 bytes = 3 items of 4
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            assert_eq!(gzfwrite(&mut state, buf, 4, 3), 3, "3 full items written");
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(gunzip(&compressed), buf, "gzfwrite payload round-trips");
    }

    #[test]
    fn gzfwrite_overflow_is_rejected() {
        let path = temp_path("fwrite_ovf");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);
        // size * nitems overflows usize -> rejected with a stream error.
        assert_eq!(gzfwrite(&mut state, b"x", usize::MAX, 2), 0);
        assert_eq!(state.err, ReturnCode::StreamError);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gzwrite_on_read_mode_returns_zero() {
        let path = temp_path("notwrite");
        let mut state = new_write_state(&path, 8192, 6, 0, 0);
        state.mode = GzMode::Read;
        assert_eq!(
            gzwrite(&mut state, b"data"),
            0,
            "a read stream rejects writes"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn io_write_trait_roundtrip() {
        let path = temp_path("iowrite");
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            state.write_all(b"hello ").expect("write_all succeeds");
            write!(state, "world {}", 42).expect("write! macro succeeds");
            state.flush().expect("io flush (sync) succeeds");
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(
            gunzip(&compressed),
            b"hello world 42",
            "the std::io::Write path round-trips"
        );
    }

    #[test]
    fn finish_on_empty_stream_is_valid_empty_gzip() {
        let path = temp_path("emptyfin");
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            // No data at all, then finalize.
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert!(
            compressed.starts_with(&[0x1f, 0x8b, 0x08]),
            "an empty member still has a gzip header"
        );
        assert_eq!(gunzip(&compressed), b"", "empty member decodes to nothing");
    }

    #[test]
    fn gz_zero_compresses_skip_zero_bytes() {
        let path = temp_path("zero");
        {
            let mut state = new_write_state(&path, 8192, 6, 0, 0);
            gz_init(&mut state).expect("init");
            state.skip = 100;
            gz_zero(&mut state).expect("gz_zero succeeds");
            assert_eq!(state.skip, 0, "the whole gap was consumed");
            assert_eq!(state.pos, 100, "position advanced by the gap size");
            assert_eq!(finish_write(&mut state), ReturnCode::Ok);
        }
        let compressed = read_and_remove(&path);
        assert_eq!(
            gunzip(&compressed),
            vec![0u8; 100],
            "a forward seek materializes as zero bytes"
        );
    }
}
