//! gzip **write** path, ported from zlib `gzwrite.c`.
//!
//! This module drives the DEFLATE engine ([`crate::deflate`]) configured for the
//! **gzip wrapper** (`windowBits = MAX_WBITS + 16 = 31`, RFC 1952) and writes the
//! produced bytes to the owned [`std::fs::File`]. The gzip framing — the 10-byte
//! header *and* the 8-byte CRC-32 + ISIZE trailer — is emitted entirely by the
//! deflate engine because of that `+16` wrapper selection; this module never
//! hand-rolls any part of the gzip format. It only shuttles bytes, manages the
//! input/output buffers, and chooses the `deflate()` flush mode.
//!
//! # Public C-faithful API
//!
//! The functions named after the C entry points preserve the documented zlib
//! return conventions (`gzwrite`/`gzfwrite`/`gzputc`/`gzputs`/`gzprintf`/
//! `gzflush`/`gzclose_w`). The variadic `gzprintf` C-string / `va_list`
//! marshalling is *not* done here — it belongs at the `ffi.rs` boundary; the
//! safe core exposes a [`fmt::Arguments`]-based [`gzprintf`].
//!
//! # Shared `pub(crate)` helpers
//!
//! [`gz_init`], [`gz_comp`], [`gz_zero`], [`set_params`], [`finish`] and
//! [`gzclose_w`] are `pub(crate)` so that the sibling gz modules reuse the
//! deflate-driving logic without duplicating it:
//!
//! * `open.rs::gzsetparams` is a thin wrapper over [`set_params`] (its body
//!   comes from `gzwrite.c`'s `gzsetparams`);
//! * `close.rs::gzclose` dispatches the write side to [`gzclose_w`];
//! * `state.rs`'s [`Drop`](crate::gz::state::GzState) safety net invokes
//!   [`finish`] (installed as the [`FinalizeFn`](crate::gz::state::FinalizeFn)).
//!
//! # Memory model (AAP §0.6.3)
//!
//! All buffers are owned `Vec<u8>`; the deflate engine is an owned
//! `Box<DeflateState>` held in [`GzState::deflate`](crate::gz::state). There is
//! **no `unsafe`** anywhere in this file — raw-pointer cursor walking is replaced
//! by slice indexing and an explicit input cursor pair (`in_next`/`in_avail`).
//!
//! ## The doubled input buffer
//!
//! [`gz_init`] allocates the input buffer at **`want << 1`** (double the nominal
//! size). C `gzprintf` formats up to `want` bytes into the *second half* of this
//! buffer; the safe port formats into a temporary [`String`] instead, but the
//! doubled allocation is preserved verbatim for byte-for-byte parity with C and
//! so a future `ffi.rs` direct-format path can rely on it.

use std::io::Write;

use crate::constants::{
    DEF_MEM_LEVEL, FlushMode, MAX_WBITS, Z_BLOCK, Z_DATA_ERROR, Z_DEFLATED, Z_ERRNO, Z_FINISH,
    Z_MEM_ERROR, Z_NO_FLUSH, Z_OK, Z_STREAM_ERROR,
};
use crate::deflate::state::DeflateStream;
use crate::deflate::{deflate, deflate_end, deflate_init2, deflate_params, deflate_reset};
use crate::error::ZlibError;
use crate::gz::state::{GzState, Mode};

// ===========================================================================
// Private helpers
// ===========================================================================

/// Allocate a zero-filled `Vec<u8>` of `len` bytes, returning `None` on an
/// allocation failure instead of aborting.
///
/// This is the safe-Rust analogue of C `malloc` + `NULL` check (`gz_init`):
/// `Vec`'s infallible `vec![0; len]` would abort the process on OOM, so
/// [`Vec::try_reserve_exact`] is used to surface the failure as a recoverable
/// `Z_MEM_ERROR` exactly as the C port does.
fn alloc_zeroed(len: usize) -> Option<Vec<u8>> {
    let mut v: Vec<u8> = Vec::new();
    if v.try_reserve_exact(len).is_err() {
        return None;
    }
    v.resize(len, 0);
    Some(v)
}

/// Write `out_buf[..len]` to the output file, mapping an I/O error to
/// `gz_error(Z_ERRNO, …)`. Returns `0` on success, `-1` on error.
///
/// `state.out_buf` and `state.file` are disjoint fields, so the immutable borrow
/// of `out_buf` and the mutable borrow of `file` coexist; the `io::Result` is
/// captured before any whole-`state` borrow (the `gz_error` call).
fn write_out_buf(state: &mut GzState, len: usize) -> i32 {
    if len == 0 {
        return 0;
    }
    let res = match state.file.as_mut() {
        Some(f) => f.write_all(&state.out_buf[..len]),
        None => {
            state.gz_error(Z_STREAM_ERROR, Some("internal error: missing output file"));
            return -1;
        }
    };
    if let Err(e) = res {
        let msg = e.to_string();
        state.gz_error(Z_ERRNO, Some(&msg));
        return -1;
    }
    0
}

/// Run `deflate()` over `input`, draining **all** produced output to the file,
/// and report `(ret, consumed)` where `ret` is `0` on success / `-1` on error
/// and `consumed` is the number of input bytes the engine accepted.
///
/// `input` must **not** alias any field of `state` (it is either a buffer lifted
/// out of `state` via [`core::mem::take`] or the caller's external buffer). The
/// engine (`state.deflate`) and the scratch output buffer (`state.out_buf`) are
/// distinct fields and are borrowed disjointly while constructing the transient
/// [`DeflateStream`].
///
/// The loop mirrors C `gz_comp`'s `do { write produced } while (have);`: each
/// `deflate()` call is given the *full* fresh output buffer and every byte it
/// produces is written immediately. For a regular file this yields a byte stream
/// identical to C's accumulate-then-flush scheme — only the write chunking
/// differs, never the file contents. A `Z_BUF_ERROR` (no forward progress, e.g.
/// an empty `Z_NO_FLUSH`) is benign and ends the loop, matching C's silent
/// tolerance of `Z_BUF_ERROR`; a `Z_STREAM_ERROR` is fatal.
fn drive_deflate(state: &mut GzState, input: &[u8], flush: FlushMode) -> (i32, usize) {
    let want = state.want;
    let mut consumed_total = 0usize;
    loop {
        // --- one deflate() call over the not-yet-consumed tail of `input` ----
        // `state.deflate` and `state.out_buf` are disjoint fields; `input` is
        // external. All three borrows release when the block yields its tuple.
        let (result, consumed, produced) = {
            let engine = match state.deflate.as_deref_mut() {
                Some(d) => d,
                None => {
                    state.gz_error(
                        Z_STREAM_ERROR,
                        Some("internal error: deflate engine not initialized"),
                    );
                    return (-1, consumed_total);
                }
            };
            let mut stream = DeflateStream {
                state: engine,
                input: &input[consumed_total..],
                in_next: 0,
                output: &mut state.out_buf[..want],
                out_next: 0,
            };
            let r = deflate(&mut stream, flush);
            (r, stream.in_next, stream.out_next)
        };
        consumed_total += consumed;

        // --- drain whatever the engine produced -----------------------------
        if produced > 0 && write_out_buf(state, produced) == -1 {
            return (-1, consumed_total);
        }

        match result {
            Ok(_) => {
                // C: `while (have)` — keep going only while output was produced.
                if produced == 0 {
                    break;
                }
            }
            // No forward progress possible (e.g. empty Z_NO_FLUSH). C silently
            // tolerates Z_BUF_ERROR inside gz_comp; we simply stop.
            Err(ZlibError::BufError) => break,
            // Any genuine engine inconsistency is fatal (C: Z_STREAM_ERROR).
            Err(_) => {
                state.gz_error(
                    Z_STREAM_ERROR,
                    Some("internal error: deflate stream corrupt"),
                );
                return (-1, consumed_total);
            }
        }
    }
    (0, consumed_total)
}

// ===========================================================================
// Phase B — gz_init
// ===========================================================================

/// Initialize state for writing a gzip file (C `gz_init`, `gzwrite.c` L11-57).
///
/// Allocates the input buffer at `want << 1` (doubled for `gzprintf`) and, when
/// compressing (`direct == 0`), the `want`-sized output buffer plus a fresh
/// deflate engine configured for the gzip wrapper:
/// `deflateInit2(level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL, strategy)`.
/// Initialization is marked by setting `size` to the (non-zero) `want`.
///
/// Returns `0` on success, or `-1` after recording `Z_MEM_ERROR` on an
/// allocation / engine-init failure (buffers already taken are released).
pub(crate) fn gz_init(state: &mut GzState) -> i32 {
    // allocate input buffer (double size for gzprintf)
    let in_size = match state.want.checked_mul(2) {
        Some(n) => n,
        None => {
            state.gz_error(Z_MEM_ERROR, Some("out of memory"));
            return -1;
        }
    };
    state.in_buf = match alloc_zeroed(in_size) {
        Some(v) => v,
        None => {
            state.gz_error(Z_MEM_ERROR, Some("out of memory"));
            return -1;
        }
    };

    // only need output buffer and deflate state if compressing
    if state.direct == 0 {
        // allocate output buffer
        state.out_buf = match alloc_zeroed(state.want) {
            Some(v) => v,
            None => {
                state.in_buf = Vec::new();
                state.gz_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        };

        // allocate deflate memory, set up for gzip compression
        // (windowBits = MAX_WBITS + 16 = 31 selects the RFC 1952 gzip wrapper)
        match deflate_init2(
            state.level,
            Z_DEFLATED,
            MAX_WBITS + 16,
            DEF_MEM_LEVEL,
            state.strategy,
        ) {
            Ok(engine) => state.deflate = Some(engine),
            Err(_) => {
                state.out_buf = Vec::new();
                state.in_buf = Vec::new();
                state.gz_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }

        // Install the best-effort finalize hook so a dropped, un-closed write
        // stream still flushes + finishes (C has no analogue; this realises the
        // RAII Drop contract that replaces the manual `gzclose_w`).
        state.set_finalize(finish);
    }

    // mark state as initialized
    state.size = state.want;

    // reset the write-side input cursor (C: `strm->next_in = NULL`, avail_in 0).
    state.in_next = 0;
    state.in_avail = 0;
    0
}

// ===========================================================================
// Phase C — gz_comp
// ===========================================================================

/// Compress whatever is buffered at `in_buf[in_next .. in_next + in_avail]` with
/// the given `flush`, writing all produced output to the file (C `gz_comp`,
/// `gzwrite.c` L65-148).
///
/// This is the single choke point for **all** compressed output — ordinary
/// writes, explicit flushes, `Z_FINISH`, seek-fill zeros, and the param-change
/// flush all funnel through here. `flush` is assumed to be a valid `deflate()`
/// flush value (callers validate). Behavior:
///
/// * **Lazy init** — if `size == 0`, [`gz_init`] runs first (`-1` on failure).
/// * **Transparent (`direct`) path** — the buffered bytes are written straight
///   to the file with no compression and the cursor is reset.
/// * **Pending reset** — after a previous `Z_FINISH`, the next non-empty /
///   flushing call resets the engine so a fresh gzip member begins (multi-member
///   parity).
/// * **`Z_FINISH`** — sets [`reset`](crate::gz::state) so the *next* write starts
///   a new member.
///
/// Returns `0` on success, `-1` on a write or fatal deflate error (with
/// `gz_error` recorded).
pub(crate) fn gz_comp(state: &mut GzState, flush: i32) -> i32 {
    // allocate memory if this is the first time through
    if state.size == 0 && gz_init(state) == -1 {
        return -1;
    }

    // write directly if requested (transparent passthrough — no deflate)
    if state.direct != 0 {
        let len = state.in_avail;
        if len > 0 {
            let start = state.in_next;
            let res = match state.file.as_mut() {
                Some(f) => f.write_all(&state.in_buf[start..start + len]),
                None => {
                    state.gz_error(Z_STREAM_ERROR, Some("internal error: missing output file"));
                    return -1;
                }
            };
            if let Err(e) = res {
                let msg = e.to_string();
                state.gz_error(Z_ERRNO, Some(&msg));
                return -1;
            }
        }
        state.in_next = 0;
        state.in_avail = 0;
        return 0;
    }

    // check for a pending reset (start a new gzip member)
    if state.reset {
        // don't start a new member unless there is data to write and we're not
        // merely flushing nothing (C L97-98).
        if state.in_avail == 0 && flush == Z_NO_FLUSH {
            return 0;
        }
        if let Some(engine) = state.deflate.as_deref_mut() {
            let _ = deflate_reset(engine);
        }
        state.reset = false;
    }

    let fm = FlushMode::try_from(flush).unwrap_or(FlushMode::NoFlush);

    // Drive deflate over the buffered input. `in_buf` is lifted out of `state`
    // so the input slice does not alias the `&mut state` passed to
    // `drive_deflate` (which only touches `deflate`/`out_buf`/`file`). The take
    // is allocation-free (swaps in an empty `Vec`) and the buffer is restored
    // immediately afterwards.
    let in_start = state.in_next;
    let in_len = state.in_avail;
    let in_buf = core::mem::take(&mut state.in_buf);
    let (ret, consumed) = drive_deflate(state, &in_buf[in_start..in_start + in_len], fm);
    state.in_buf = in_buf;
    if ret == -1 {
        return -1;
    }

    // advance the buffered-input cursor by however much deflate consumed
    state.in_next += consumed;
    state.in_avail -= consumed;
    if state.in_avail == 0 {
        state.in_next = 0;
    }

    // if that completed a deflate stream, allow another to start
    if flush == Z_FINISH {
        state.reset = true;
    }
    0
}

// ===========================================================================
// Phase D — gz_zero
// ===========================================================================

/// Compress `len` zero bytes to the output (C `gz_zero`, `gzwrite.c` L154-182).
///
/// Used by a forward `gzseek` on a write stream to materialise the skipped
/// region as a run of zeros. Any input already buffered is flushed first, then
/// `len` zeros are fed through [`gz_comp`] in `want`-sized chunks, advancing
/// `pos`. Returns `0` on success, `-1` on a write / allocation failure.
///
/// Unlike C — which lets the first `gz_comp` perform the lazy `gz_init` because
/// it sizes the per-iteration chunk from `state->size` (`0` until init) — this
/// port sizes chunks from `want` (always non-zero), so it must guarantee the
/// input buffer exists *before* indexing it. It therefore initializes upfront.
pub(crate) fn gz_zero(state: &mut GzState, len: i64) -> i32 {
    // ensure the input buffer is allocated before we index it
    if state.size == 0 && gz_init(state) == -1 {
        return -1;
    }

    // consume whatever's left in the input buffer
    if state.in_avail != 0 && gz_comp(state, Z_NO_FLUSH) == -1 {
        return -1;
    }

    // compress `len` zeros
    let want = state.want;
    let mut remaining = len;
    let mut first = true;
    while remaining > 0 {
        let n = if (want as i64) > remaining {
            remaining as usize
        } else {
            want
        };
        if first {
            // zero only once: gz_comp reads (never mutates) the buffer, and each
            // subsequent chunk is no larger, so the leading `n` bytes stay zero.
            for b in state.in_buf[..n].iter_mut() {
                *b = 0;
            }
            first = false;
        }
        state.in_next = 0;
        state.in_avail = n;
        if gz_comp(state, Z_NO_FLUSH) == -1 {
            return -1;
        }
        // gz_comp(Z_NO_FLUSH) consumes the whole buffered chunk on a blocking
        // file, so `n` bytes were written.
        state.pos += n as i64;
        remaining -= n as i64;
    }
    0
}

// ===========================================================================
// Phase E — gz_write core
// ===========================================================================

/// Write `buf` to the gzip stream (C `gz_write`, `gzwrite.c` L188-252).
///
/// Returns the number of bytes accepted (`buf.len()` on success, `0` on error
/// with `gz_error` recorded). Mirrors C's two-way split:
///
/// * **Small** (`len < want`) — copied into the input buffer, compressing via
///   [`gz_comp`] whenever the buffer fills.
/// * **Large** (`len >= want`) — any buffered input is flushed first, then the
///   caller's buffer is fed **directly** to the engine with no intermediate
///   copy (the user slice cannot alias `state`, so the borrow is sound).
pub(crate) fn gz_write(state: &mut GzState, buf: &[u8]) -> usize {
    let len = buf.len();

    // if len is zero, avoid unnecessary operations
    if len == 0 {
        return 0;
    }

    // allocate memory if this is the first time through
    if state.size == 0 && gz_init(state) == -1 {
        return 0;
    }

    // check for seek request (write skipped region as zeros)
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return 0;
        }
    }

    if len < state.want {
        // copy to input buffer, compress when full
        let want = state.want;
        let mut buf_pos = 0usize;
        loop {
            if state.in_avail == 0 {
                state.in_next = 0;
            }
            let have = state.in_next + state.in_avail;
            let mut copy = want - have;
            if copy > len - buf_pos {
                copy = len - buf_pos;
            }
            state.in_buf[have..have + copy].copy_from_slice(&buf[buf_pos..buf_pos + copy]);
            state.in_avail += copy;
            state.pos += copy as i64;
            buf_pos += copy;
            if buf_pos == len {
                break;
            }
            if gz_comp(state, Z_NO_FLUSH) == -1 {
                return 0;
            }
        }
    } else {
        // consume whatever's left in the input buffer
        if state.in_avail != 0 && gz_comp(state, Z_NO_FLUSH) == -1 {
            return 0;
        }
        // directly compress the user buffer to the file (no copy)
        let (ret, _consumed) = drive_deflate(state, buf, FlushMode::NoFlush);
        if ret == -1 {
            return 0;
        }
        state.pos += len as i64;
    }

    // input was all buffered or compressed
    len
}

// ===========================================================================
// Phase F — public write API (C-faithful names)
// ===========================================================================

/// Write `buf` to a gzip file (C `gzwrite`, `gzwrite.c` L255-277).
///
/// Returns the number of **uncompressed** bytes written, or `0` on error.
/// Validates that the handle is an error-free write stream first.
pub fn gzwrite(state: &mut GzState, buf: &[u8]) -> i32 {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.clear_error();

    // since an int is returned, make sure len fits in one
    if buf.len() > i32::MAX as usize {
        state.gz_error(Z_DATA_ERROR, Some("requested length does not fit in int"));
        return 0;
    }

    gz_write(state, buf) as i32
}

/// Write `nitems` items of `item_size` bytes from `buf` (C `gzfwrite`,
/// `gzwrite.c` L280-304).
///
/// Returns the number of **full items** written. Guards `item_size * nitems`
/// against overflow (C checks `len / size != nitems`); on overflow records
/// `Z_STREAM_ERROR` and returns `0`.
pub fn gzfwrite(state: &mut GzState, item_size: usize, nitems: usize, buf: &[u8]) -> usize {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.clear_error();

    // compute bytes to write -- error on overflow
    let len = match nitems.checked_mul(item_size) {
        Some(l) => l,
        None => {
            state.gz_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
            return 0;
        }
    };
    if len == 0 {
        return 0;
    }

    // write `len` bytes; the caller (and the ffi.rs shim) guarantees `buf` holds
    // at least `len` bytes, but clamp defensively to avoid any panic.
    let to_write = len.min(buf.len());
    let written = gz_write(state, &buf[..to_write]);

    // number of full items written (item_size is non-zero here, since len != 0)
    written / item_size
}

/// Write a single byte `c` (low 8 bits) to a gzip file (C `gzputc`,
/// `gzwrite.c` L307-347).
///
/// Returns the byte written (`c & 0xff`) or `-1` on error. Uses C's fast path:
/// if the input buffer has room, the byte is appended directly without invoking
/// the full [`gz_write`] machinery.
pub fn gzputc(state: &mut GzState, c: i32) -> i32 {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.clear_error();

    // check for seek request
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return -1;
        }
    }

    // try writing to the input buffer for speed (size == 0 if uninitialized)
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

    // no room in buffer or not initialized, use gz_write()
    let one = [c as u8];
    if gz_write(state, &one) != 1 {
        return -1;
    }
    c & 0xff
}

/// Write the bytes of `s` to a gzip file (C `gzputs`, `gzwrite.c` L350-372).
///
/// The C entry point takes a NUL-terminated `char *`; the safe core takes the
/// raw bytes (`&[u8]`) — the NUL-termination handling lives in `ffi.rs`. Returns
/// the number of bytes written, or `-1` on error.
pub fn gzputs(state: &mut GzState, s: &[u8]) -> i32 {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.clear_error();

    let len = s.len();
    if len > i32::MAX as usize {
        state.gz_error(Z_STREAM_ERROR, Some("string length does not fit in int"));
        return -1;
    }

    let put = gz_write(state, s);
    if len != 0 && put == 0 { -1 } else { put as i32 }
}

/// Formatted write to a gzip file — safe core of C `gzvprintf` / `gzprintf`
/// (`gzwrite.c` L403-495).
///
/// The C variadic `gzprintf(file, format, …)` marshals its `va_list` and runs
/// `vsnprintf` into the *second half* of the doubled input buffer; that
/// `unsafe`, `va_list`-dependent shim belongs in `ffi.rs`. Here the caller
/// supplies the already-typed [`fmt::Arguments`] (e.g. via `format_args!`), which
/// are rendered into a temporary [`String`] and pushed through [`gz_write`].
///
/// Returns the number of bytes written. Per C, a render that is empty or would
/// not fit in `want` bytes (`len == 0 || len >= want`) yields `0`; any
/// `gz_write` error also surfaces as the recorded `state.err`.
pub fn gzprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // make sure we have some buffer space (and that `want`/`size` are known)
    if state.size == 0 && gz_init(state) == -1 {
        return state.err;
    }

    // check for seek request
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // render the arguments into a temporary buffer (the safe analogue of C
    // formatting into the doubled input buffer's second half)
    let mut rendered = String::new();
    if core::fmt::write(&mut rendered, args).is_err() {
        return 0;
    }
    let bytes = rendered.as_bytes();
    let len = bytes.len();

    // check that the formatted result fits (C: len == 0 || len >= size)
    if len == 0 || len >= state.size {
        return 0;
    }

    let put = gz_write(state, bytes);
    if state.err != Z_OK && !state.again {
        return state.err;
    }
    // `put` equals `len` on success (gz_write accepts the whole slice).
    put as i32
}

/// Flush pending compressed output with the requested deflate `flush` mode
/// (C `gzflush`, `gzwrite.c` L603-627).
///
/// Validates `flush` is in `0..=Z_FINISH` (rejecting `Z_BLOCK`/`Z_TREES`, exactly
/// like C), then drives [`gz_comp`]. Returns `Z_OK` on success or the recorded
/// error code.
pub fn gzflush(state: &mut GzState, flush: i32) -> i32 {
    // check that we're writing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // check flush parameter (C `flush < 0 || flush > Z_FINISH`)
    if !(0..=Z_FINISH).contains(&flush) {
        return Z_STREAM_ERROR;
    }

    // check for seek request
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // compress remaining data with the requested flush
    let _ = gz_comp(state, flush);
    state.err
}

// ===========================================================================
// Phase G — gzsetparams deflate driver (shared with open.rs)
// ===========================================================================

/// Change the compression `level` / `strategy` for subsequent input
/// (C `gzsetparams`, `gzwrite.c` L630-664).
///
/// The body lives here (not in `open.rs`) because it drives the deflate engine;
/// `open.rs::gzsetparams` is a thin wrapper that calls this. Rejects non-write /
/// transparent / errored handles; a no-op change returns `Z_OK`; otherwise any
/// pending input is flushed with `Z_BLOCK` (so the old parameters apply to it
/// before the switch), the engine's parameters are updated via
/// [`deflate_params`], and the new settings are stored.
///
/// Unlike C — whose persistent `next_out` keeps `deflateParams`' flush output in
/// the output buffer for a later `gz_comp` to drain — the slice API does not
/// persist the output cursor, so any bytes `deflate_params` emits are written to
/// the file immediately here.
pub(crate) fn set_params(state: &mut GzState, level: i32, strategy: i32) -> i32 {
    // check that we're compressing and that there's no (serious) error
    if state.mode != Mode::Write || (state.err != Z_OK && !state.again) || state.direct != 0 {
        return Z_STREAM_ERROR;
    }
    state.clear_error();

    // if no change is requested, then do nothing
    if level == state.level && strategy == state.strategy {
        return Z_OK;
    }

    // check for seek request
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            return state.err;
        }
    }

    // change compression parameters for subsequent input
    if state.size != 0 {
        // flush previous input with previous parameters before changing
        if state.in_avail != 0 && gz_comp(state, Z_BLOCK) == -1 {
            return state.err;
        }

        // apply the new parameters; deflate_params may emit a flushed block into
        // the output buffer, which we drain to the file right away.
        let want = state.want;
        let produced = {
            let engine = match state.deflate.as_deref_mut() {
                Some(d) => d,
                None => {
                    state.gz_error(
                        Z_STREAM_ERROR,
                        Some("internal error: deflate engine not initialized"),
                    );
                    return state.err;
                }
            };
            let empty: [u8; 0] = [];
            let mut stream = DeflateStream {
                state: engine,
                input: &empty,
                in_next: 0,
                output: &mut state.out_buf[..want],
                out_next: 0,
            };
            // C ignores deflateParams' return value; so do we (a Z_BUF_ERROR
            // simply means nothing needed flushing).
            let _ = deflate_params(&mut stream, level, strategy);
            stream.out_next
        };
        if produced > 0 && write_out_buf(state, produced) == -1 {
            return state.err;
        }
    }

    state.level = level;
    state.strategy = strategy;
    Z_OK
}

// ===========================================================================
// Phase F (cont.) — close / finalize
// ===========================================================================

/// Best-effort flush + finish used by the [`Drop`](crate::gz::state) safety net
/// (installed via [`set_finalize`](crate::gz::state) in [`gz_init`]).
///
/// Performs the same `seek-fill → gz_comp(Z_FINISH) → deflateEnd` sequence as
/// [`gzclose_w`] but does **not** touch the file handle or buffers (the owning
/// `GzState`'s own `Drop` frees those). The returned `Z_*` code lets the explicit
/// close path observe errors; `Drop` discards it.
pub(crate) fn finish(state: &mut GzState) -> i32 {
    let mut ret = Z_OK;

    // check for seek request
    if state.skip != 0 {
        let skip = state.skip;
        state.skip = 0;
        if gz_zero(state, skip) == -1 {
            ret = state.err;
        }
    }

    // flush remaining input and finish the gzip stream (writes header/trailer)
    if gz_comp(state, Z_FINISH) == -1 {
        ret = state.err;
    }

    // deflate teardown is a status check only; the owned engine frees on Drop.
    if state.size != 0 && state.direct == 0 {
        if let Some(engine) = state.deflate.as_deref() {
            let _ = deflate_end(engine);
        }
    }
    ret
}

/// Close a gzip file open for writing (C `gzclose_w`, `gzwrite.c` L667-700).
///
/// Flushes any pending seek as zeros, finishes the gzip stream
/// (`gz_comp(Z_FINISH)` — which emits the CRC-32 + ISIZE trailer), tears the
/// engine down, frees the buffers, and closes the file. Returns the first error
/// encountered, or `Z_OK`.
///
/// Exposed `pub(crate)` so `close.rs::gzclose` can dispatch the write side here.
/// It takes the installed [`FinalizeFn`](crate::gz::state) (so the eventual
/// `Drop` does not finalize a second time) and marks the handle closed.
pub fn gzclose_w(state: &mut GzState) -> i32 {
    // check that we're writing
    if state.mode != Mode::Write {
        return Z_STREAM_ERROR;
    }

    // take the finalize hook so the later Drop is a no-op (double-finalize guard)
    let _ = state.take_finalize();

    // flush + finish the gzip stream (seek-fill, Z_FINISH, deflateEnd)
    let mut ret = finish(state);

    // free engine + buffers (their Drop would also free them, but match C's
    // explicit teardown so the handle is inert before the file closes)
    state.deflate = None;
    state.in_buf = Vec::new();
    state.out_buf = Vec::new();
    state.clear_error();

    // close the file: flush the OS buffers, then drop the handle to close the fd
    if let Some(mut f) = state.file.take() {
        if f.flush().is_err() {
            ret = Z_ERRNO;
        }
        // `f` dropped here closes the underlying descriptor.
    }

    // mark closed so a subsequent Drop early-returns without re-finalizing
    state.mark_closed();
    ret
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{Z_FINISH, Z_OK, Z_STREAM_ERROR, Z_SYNC_FLUSH, Z_TREES};
    use std::fs::File;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A process-unique temp path so parallel test threads never collide.
    fn temp_path(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("blitzy_gzwrite_ut_{tag}_{pid}_{n}.gz"))
    }

    /// Construct a fresh write-mode `GzState` backed by a real file at `path`.
    fn write_state(path: &Path) -> GzState {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .expect("open temp file for writing");
        GzState::new(file, path.to_string_lossy().into_owned(), Mode::Write)
    }

    /// Decode a gzip file with the flate2 (C zlib) oracle — single member.
    fn decode_gzip(path: &Path) -> Vec<u8> {
        let data = std::fs::read(path).expect("read gz file");
        let mut dec = flate2::read::GzDecoder::new(&data[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).expect("flate2 decode");
        out
    }

    /// Decode all concatenated gzip members with flate2's multi-member decoder.
    fn decode_gzip_multi(path: &Path) -> Vec<u8> {
        let data = std::fs::read(path).expect("read gz file");
        let mut dec = flate2::read::MultiGzDecoder::new(&data[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).expect("flate2 multi decode");
        out
    }

    fn raw_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).expect("read raw file")
    }

    #[test]
    fn writes_gzip_magic_and_round_trips() {
        let p = temp_path("magic");
        let data: Vec<u8> = b"hello, gzip world! ".repeat(200);
        {
            let mut st = write_state(&p);
            let n = gzwrite(&mut st, &data);
            assert_eq!(n as usize, data.len(), "gzwrite should report full length");
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        // gzip magic + DEFLATE method byte (windowBits = 31 → gzip wrapper).
        let raw = raw_bytes(&p);
        assert_eq!(&raw[..3], &[0x1f, 0x8b, 0x08], "gzip magic + method");
        // byte-for-byte round trip through the C-zlib oracle.
        assert_eq!(decode_gzip(&p), data);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn small_then_large_write_in_one_stream() {
        let p = temp_path("smalllarge");
        // small (< want=8192) followed by large (>= want) to exercise both the
        // accumulate path and the direct-feed path within one gzip member.
        let small = b"small chunk".to_vec();
        let large: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let mut expected = small.clone();
        expected.extend_from_slice(&large);
        {
            let mut st = write_state(&p);
            assert_eq!(gzwrite(&mut st, &small) as usize, small.len());
            assert_eq!(gzwrite(&mut st, &large) as usize, large.len());
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), expected);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzputc_and_gzputs_round_trip() {
        let p = temp_path("putcputs");
        {
            let mut st = write_state(&p);
            assert_eq!(gzputc(&mut st, b'H' as i32), b'H' as i32);
            assert_eq!(gzputc(&mut st, b'i' as i32), b'i' as i32);
            assert_eq!(gzputs(&mut st, b" there"), 6);
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), b"Hi there");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzprintf_formats_near_want_without_overflow() {
        let p = temp_path("printf");
        // Render ~want-1 bytes: must succeed (len < size) and round-trip.
        let body = "a".repeat(8000);
        {
            let mut st = write_state(&p);
            let n = gzprintf(&mut st, format_args!("{body}"));
            assert_eq!(n, 8000, "gzprintf should report formatted length");
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), body.as_bytes());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzprintf_rejects_output_not_fitting_in_want() {
        let p = temp_path("printfbig");
        // Render >= want (8192) bytes: C returns 0 and writes nothing.
        let body = "b".repeat(9000);
        {
            let mut st = write_state(&p);
            let n = gzprintf(&mut st, format_args!("{body}"));
            assert_eq!(n, 0, "oversized gzprintf must return 0");
            // a subsequent in-range gzprintf still works
            assert_eq!(gzprintf(&mut st, format_args!("ok")), 2);
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), b"ok");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzflush_rejects_out_of_range_values() {
        let p = temp_path("flushrange");
        let mut st = write_state(&p);
        // Z_BLOCK (5) and Z_TREES (6) are > Z_FINISH (4): rejected.
        assert_eq!(gzflush(&mut st, Z_BLOCK), Z_STREAM_ERROR);
        assert_eq!(gzflush(&mut st, Z_TREES), Z_STREAM_ERROR);
        assert_eq!(gzflush(&mut st, -1), Z_STREAM_ERROR);
        // a valid flush succeeds
        assert_eq!(gzwrite(&mut st, b"data"), 4);
        assert_eq!(gzflush(&mut st, Z_SYNC_FLUSH), Z_OK);
        assert_eq!(gzclose_w(&mut st), Z_OK);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzflush_sync_then_more_data_round_trips() {
        let p = temp_path("syncflush");
        let part1 = b"first part before flush ".to_vec();
        let part2 = b"second part after flush".to_vec();
        let mut expected = part1.clone();
        expected.extend_from_slice(&part2);
        {
            let mut st = write_state(&p);
            assert_eq!(gzwrite(&mut st, &part1) as usize, part1.len());
            assert_eq!(gzflush(&mut st, Z_SYNC_FLUSH), Z_OK);
            assert_eq!(gzwrite(&mut st, &part2) as usize, part2.len());
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), expected);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn gzfwrite_items_and_overflow_guard() {
        let p = temp_path("fwrite");
        {
            let mut st = write_state(&p);
            // 4 items of 3 bytes = 12 bytes
            let buf = b"abcDEFghiJKL"; // 12 bytes
            let items = gzfwrite(&mut st, 3, 4, buf);
            assert_eq!(items, 4, "should report 4 full items");
            // overflow: item_size * nitems overflows usize → returns 0, no UB
            let overflow = gzfwrite(&mut st, usize::MAX, 2, buf);
            assert_eq!(overflow, 0, "overflowing item count must return 0");
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), b"abcDEFghiJKL");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_write_stream_closes_to_valid_gzip() {
        let p = temp_path("empty");
        {
            let mut st = write_state(&p);
            // no writes at all — close must still emit a valid (empty) member
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        let raw = raw_bytes(&p);
        assert_eq!(&raw[..3], &[0x1f, 0x8b, 0x08]);
        assert_eq!(decode_gzip(&p), b"");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn multi_member_via_finish_then_write() {
        let p = temp_path("multimember");
        let a = b"member one payload ".to_vec();
        let b = b"member two payload".to_vec();
        let mut expected = a.clone();
        expected.extend_from_slice(&b);
        {
            let mut st = write_state(&p);
            assert_eq!(gzwrite(&mut st, &a) as usize, a.len());
            // Z_FINISH ends the first member and arms the reset for the next write
            assert_eq!(gzflush(&mut st, Z_FINISH), Z_OK);
            assert_eq!(gzwrite(&mut st, &b) as usize, b.len());
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        // multi-member decode concatenates both members
        assert_eq!(decode_gzip_multi(&p), expected);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn direct_transparent_write_is_not_compressed() {
        let p = temp_path("direct");
        let data = b"raw passthrough, no gzip wrapper".to_vec();
        {
            let mut st = write_state(&p);
            st.direct = 1; // simulate a transparent (copy) stream
            assert_eq!(gzwrite(&mut st, &data) as usize, data.len());
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        // direct mode writes the bytes verbatim — no gzip header.
        assert_eq!(raw_bytes(&p), data);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_after_close_is_rejected() {
        let p = temp_path("afterclose");
        let mut st = write_state(&p);
        assert_eq!(gzwrite(&mut st, b"x"), 1);
        assert_eq!(gzclose_w(&mut st), Z_OK);
        // after close the mode is no longer Write → operations are no-ops/errors
        assert_eq!(gzwrite(&mut st, b"y"), 0);
        assert_eq!(gzputc(&mut st, b'z' as i32), -1);
        assert_eq!(gzflush(&mut st, Z_FINISH), Z_STREAM_ERROR);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dropped_writer_still_finalizes() {
        let p = temp_path("droponly");
        let data = b"finalized via Drop, not explicit close".to_vec();
        {
            let mut st = write_state(&p);
            assert_eq!(gzwrite(&mut st, &data) as usize, data.len());
            // intentionally no gzclose_w: Drop's finalize hook must flush+finish
        }
        assert_eq!(decode_gzip(&p), data);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn set_params_changes_level_and_round_trips() {
        let p = temp_path("setparams");
        let part1 = b"compressed at default level ".repeat(20);
        let part2 = b"compressed after switching to level 1 ".repeat(20);
        let mut expected = part1.clone();
        expected.extend_from_slice(&part2);
        {
            let mut st = write_state(&p);
            assert_eq!(gzwrite(&mut st, &part1) as usize, part1.len());
            // switch to (level 1, default strategy)
            assert_eq!(
                set_params(&mut st, 1, crate::constants::Z_DEFAULT_STRATEGY),
                Z_OK
            );
            assert_eq!(st.level, 1);
            assert_eq!(gzwrite(&mut st, &part2) as usize, part2.len());
            assert_eq!(gzclose_w(&mut st), Z_OK);
        }
        assert_eq!(decode_gzip(&p), expected);
        let _ = std::fs::remove_file(&p);
    }
}
