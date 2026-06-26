//! Buffered gzip **read** path — an idiomatic, `unsafe`-free Rust port of
//! `gzread.c` (`ZLIB_VERSION "1.3.2.1-motley"`).
//!
//! This module implements the decompress-on-read engine that backs the public
//! `gz*` reading API. It is the safe-core counterpart of the C translation
//! unit `gzread.c`: every function here mirrors a C function of the same name,
//! preserving its control flow and, crucially, its *byte-exact* behavior
//! (gzip-member auto-detection, concatenated-member handling, trailing-garbage
//! tolerance, and the `gzungetc` push-back buffer geometry).
//!
//! # Architectural mapping (C ⇒ safe Rust)
//!
//! The C engine drives an embedded `z_stream` through raw `next_in` /
//! `avail_in` / `next_out` / `avail_out` pointers that alias `state->in` and
//! `state->out`. The safe core cannot alias buffers with raw pointers, so the
//! translation is:
//!
//! * **Input window.** The embedded [`ZStream`](crate::ZStream) in this design does *not* store
//!   `next_in` / `avail_in`. Instead the unconsumed compressed bytes are tracked
//!   on [`GzState`] itself via the [`in_next`](GzState::in_next) offset and
//!   [`in_have`](GzState::in_have) length: the live window is
//!   `in_buf[in_next .. in_next + in_have]`. This pair *is* the C
//!   `strm->next_in` / `strm->avail_in`, and it is the backbone of
//!   [`gz_avail`], [`gz_look`], and [`gz_decomp`].
//! * **Output buffer.** `state.out_buf` is sized `2 * want` (double) so a
//!   transparent COPY of the single-sized input fits and so [`gzungetc`] always
//!   has room to slide. Available output lives at `out_buf[next .. next + have]`.
//! * **File I/O.** The C `int fd` and `read(2)` become [`std::fs::File`] and
//!   [`std::io::Read`]. A blocking `File` never reports `EAGAIN`, so the C
//!   non-blocking "again" path is structurally unreachable here;
//!   [`again`](GzState::again) is therefore always cleared and never set.
//! * **Decompression.** `inflateInit2(&strm, 15 + 16)` (gzip wrapper only —
//!   *not* `47`/auto-detect, because this layer performs its own 4-byte magic
//!   check) becomes [`InflateState::new`]`(31)`, installed into the embedded
//!   stream; `inflate()` becomes [`InflateState::inflate`]; `inflateReset`
//!   becomes [`InflateState::reset`]; and `inflateEnd` is handled by RAII when
//!   the stream (and its boxed state) drops.
//!
//! # Teardown contract for `close.rs`
//!
//! The C `gzclose_r` (defined in `gzread.c`) is intentionally **not** ported
//! here — the assigned-folder layout routes it to `close.rs`. The inflate
//! engine and the I/O buffers are released by [`Drop`] (the boxed
//! `InflateState` frees its own allocations, and `in_buf` / `out_buf` are owned
//! [`Vec`]s), so `close.rs` need only read [`GzState::err`] to compute its
//! return code — `Z_BUF_ERROR` maps to `Z_BUF_ERROR`, anything else to `Z_OK`
//! — and then drop the state. No explicit teardown hook is exported, which both
//! matches the RAII model and avoids dead code in standalone builds.
//!
//! # Safety
//!
//! The entire `zlib-rs` core is compiled with `#![forbid(unsafe_code)]`; this
//! module contains **zero** `unsafe`. All buffer manipulation is expressed as
//! slice/index arithmetic over `Vec<u8>` and all file reads go through
//! [`std::io::Read`].

use std::fs::File;
use std::io::Read;

use crate::constants::Flush;
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GzHow, GzMode, GzState};
use crate::inflate::InflateState;

// `Vec`, the `vec!` macro, and the `ToString` trait (for `to_string`) come from
// the standard prelude: this module compiles only under the `gz-io` feature,
// which always enables `std`, so the crate is a normal `std` crate here (the
// `#![no_std]` attribute in `lib.rs` is gated on `not(feature = "std")`). This
// mirrors the prelude-reliant style of the sibling `open.rs`.

// Local mirrors of the zlib integer status codes, derived from the canonical
// enums exactly as `state.rs` does so the gz error slot (an `i32`) stays in
// lock-step with the rest of the crate.
const Z_OK: i32 = ReturnCode::Ok.as_i32();
const Z_STREAM_END: i32 = ReturnCode::StreamEnd.as_i32();
const Z_ERRNO: i32 = ZlibError::ErrNo.as_i32();
const Z_STREAM_ERROR: i32 = ZlibError::StreamError.as_i32();
const Z_DATA_ERROR: i32 = ZlibError::DataError.as_i32();
const Z_MEM_ERROR: i32 = ZlibError::MemError.as_i32();
const Z_BUF_ERROR: i32 = ZlibError::BufError.as_i32();

/// The gzip magic-number gate used by [`gz_look`]: the first four bytes are
/// consistent with a gzip member when they are `1f 8b 08` followed by a flags
/// byte with no reserved bits set (`< 32`). Mirrors `gzread.c`'s
/// `next_in[0] == 31 && next_in[1] == 139 && next_in[2] == 8 && next_in[3] < 32`.
const GZIP_MAGIC_0: u8 = 31;
const GZIP_MAGIC_1: u8 = 139;
const GZIP_MAGIC_2: u8 = 8; // CM = 8 (deflate)
const GZIP_FLAG_MAX: u8 = 32; // FLG must have the reserved high bits clear

/// gzip wrapper window-bits for [`InflateState::new`]: `15` (max window) `+ 16`
/// selects the gzip-only header mode. The gz layer performs its **own** 4-byte
/// magic detection (see [`gz_look`]), so auto-detect (`47`) is deliberately not
/// used here.
const GUNZIP_WINDOW_BITS: i32 = 15 + 16;

/// Read from the file into `dst`, looping until `dst` is full or end-of-file is
/// reached — the inner read loop of the C `gz_load`.
///
/// Returns `(bytes_read, eof_hit, error)`:
/// * `bytes_read` — number of bytes actually placed into `dst` (`*have` in C);
/// * `eof_hit` — `true` if the file signaled end-of-file (a zero-length read),
///   mirroring the C rule that `state->eof` is set only when `read()` returns 0;
/// * `error` — the first non-recoverable [`std::io::Error`] encountered, if any.
///
/// `read(2)` may legally return fewer bytes than requested, so the loop
/// continues until `dst` is full; an [`ErrorKind::Interrupted`] is retried
/// (the Rust analogue of the C `EINTR` retry that `read()` performs
/// internally). A blocking [`File`] never returns `WouldBlock`, so the C
/// `EAGAIN` / `EWOULDBLOCK` "again" branch has no analogue here.
///
/// [`ErrorKind::Interrupted`]: std::io::ErrorKind::Interrupted
fn fill_from_file(file: &mut File, dst: &mut [u8]) -> (usize, bool, Option<std::io::Error>) {
    let len = dst.len();
    let mut have = 0usize;
    while have < len {
        match file.read(&mut dst[have..]) {
            // End-of-file: no more data will ever arrive. Mark EOF and stop.
            Ok(0) => return (have, true, None),
            Ok(n) => have += n,
            // Interrupted by a signal before any byte was transferred: retry,
            // exactly as a C `read()` loop would on `EINTR`.
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return (have, false, Some(e)),
        }
    }
    (have, false, None)
}

/// Load up to `len` bytes from the file into one of the state's own buffers,
/// starting at offset `off` — the C `gz_load` specialized to `state->in` /
/// `state->out`.
///
/// When `into_out` is `true` the destination is `out_buf` (the transparent-COPY
/// and direct-decompress-target case); otherwise it is `in_buf` (the
/// compressed-input case). Returns `Ok(bytes_read)` on success (which may be
/// fewer than `len`, or `0` at EOF) or `Err(())` after recording a `Z_ERRNO`
/// error in the state. As in C, [`again`](GzState::again) is cleared on entry.
fn gz_load_buf(state: &mut GzState, into_out: bool, off: usize, len: usize) -> Result<usize, ()> {
    // C `state->again = 0;` — clear the (here always-false) non-blocking flag.
    state.again = false;
    if len == 0 {
        return Ok(0);
    }

    // `file` and `in_buf` / `out_buf` are distinct fields, so these disjoint
    // borrows of `*state` are accepted by the borrow checker without `unsafe`.
    let (read, eof_hit, error) = if into_out {
        fill_from_file(&mut state.file, &mut state.out_buf[off..off + len])
    } else {
        fill_from_file(&mut state.file, &mut state.in_buf[off..off + len])
    };

    if let Some(err) = error {
        // C calls `gz_error(state, Z_ERRNO, zstrerror())`; the OS message is the
        // closest analogue of `strerror(errno)`.
        let msg = err.to_string();
        state.set_error(Z_ERRNO, Some(&msg));
        return Err(());
    }
    if eof_hit {
        state.eof = true;
    }
    Ok(read)
}

/// Load up to `len` bytes from the file directly into the caller-supplied
/// buffer at offset `off` — the C `gz_load(state, buf, n, &n)` used by the
/// large-read fast paths in [`gz_read`].
///
/// Behaves exactly like [`gz_load_buf`] but targets an external slice rather
/// than one of the state's buffers.
fn gz_load_user(state: &mut GzState, buf: &mut [u8], off: usize, len: usize) -> Result<usize, ()> {
    state.again = false;
    if len == 0 {
        return Ok(0);
    }

    // `buf` is an independent borrow, disjoint from every field of `*state`.
    let (read, eof_hit, error) = fill_from_file(&mut state.file, &mut buf[off..off + len]);

    if let Some(err) = error {
        let msg = err.to_string();
        state.set_error(Z_ERRNO, Some(&msg));
        return Err(());
    }
    if eof_hit {
        state.eof = true;
    }
    Ok(read)
}

/// Load the input buffer, setting the EOF flag if the last of the file is read
/// — a port of the C `gz_avail`.
///
/// If unconsumed input remains (`in_have > 0`) it is first moved to the start
/// of `in_buf` (C copies `next_in` back to `state->in`), then the remainder of
/// the buffer is filled from the file. On return the live input window is the
/// contiguous range `in_buf[0 .. in_have]` (i.e. `in_next == 0`), matching the
/// C post-condition `strm->next_in = state->in`.
///
/// Returns `0` on success and `-1` if a serious error is pending or a read
/// error occurs (recorded in the state).
fn gz_avail(state: &mut GzState) -> i32 {
    // Do not proceed if a previous serious error is latched (C checks
    // `state->err != Z_OK && state->err != Z_BUF_ERROR`).
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }

    if !state.eof {
        // Move any leftover bytes to the front so the freshly read data is
        // contiguous after them. C only copies when `next_in != state->in`,
        // i.e. when the window does not already start at offset 0.
        if state.in_have != 0 && state.in_next != 0 {
            state
                .in_buf
                .copy_within(state.in_next..state.in_next + state.in_have, 0);
        }

        // Fill `in_buf[in_have .. size]` from the file. `in_have <= size` is a
        // maintained invariant (the buffer holds at most `size` bytes), so the
        // length never underflows.
        let off = state.in_have;
        let len = state.size - state.in_have;
        match gz_load_buf(state, false, off, len) {
            Ok(got) => state.in_have += got,
            Err(()) => return -1,
        }

        // The window now starts at the beginning of `in_buf` (C: next_in = in).
        state.in_next = 0;
    }

    0
}

/// Look for a gzip header and set up for inflate or for a transparent copy — a
/// port of the C `gz_look`. Precondition: [`have`](GzState::have) is `0`.
///
/// On the first call it allocates the I/O buffers and the inflate engine.
/// Afterwards it either:
/// * leaves [`how`](GzState::how) as [`GzHow::Look`] when no input is yet
///   available (a transparent read of zero bytes — *not* an error);
/// * sets `how` to [`GzHow::Gzip`] and resets the engine when a gzip member is
///   detected (or forced); or
/// * sets `how` to [`GzHow::Copy`], copying any leftover input to the output
///   buffer, for a transparent (non-gzip) stream.
///
/// Returns `0` on success or `-1` on failure (out-of-memory or a read error).
///
/// The `junk` / `direct` tri-state transitions here are load-bearing for
/// bit-exact behavior: they implement gzip-only mode (`"G"`), auto-detection at
/// the start, and the search for a *subsequent* concatenated member after one
/// has completed.
fn gz_look(state: &mut GzState) -> i32 {
    // First time in: allocate the read buffers and the inflate engine.
    if state.size == 0 {
        // Input buffer is single-sized; the output buffer is double-sized so a
        // transparent COPY of a full input fits and gzungetc always has room.
        state.in_buf = vec![0u8; state.want];
        state.out_buf = vec![0u8; state.want << 1];
        state.size = state.want;

        // Initialize inflate for gunzip (gzip wrapper only). On failure, release
        // the buffers and report out of memory, restoring `size == 0` so a later
        // call can retry the allocation.
        match InflateState::new(GUNZIP_WINDOW_BITS) {
            Ok(engine) => state.strm.set_inflate_state(engine),
            Err(_) => {
                state.in_buf = Vec::new();
                state.out_buf = Vec::new();
                state.size = 0;
                state.set_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }
    }

    // If transparent reading is disabled — which can only happen at the start
    // in gzip-only ("G") mode (`direct == -1`) — or if we are looking for a
    // gzip member after the first one (`junk == 0`), proceed directly to
    // decompressing the next member without re-running magic detection.
    if state.direct == -1 || state.junk == 0 {
        if let Some(engine) = state.strm.inflate_state_mut() {
            engine.reset();
        }
        state.how = GzHow::Gzip;
        // C: `state->junk = state->junk != -1;`. A first member marked `-1`
        // becomes `0` (junk disabled for it); a completed-member marker `0`
        // becomes `1` (re-enable trailing-garbage tolerance for the new member).
        state.junk = i32::from(state.junk != -1);
        state.direct = 0;
        return 0;
    }

    // Otherwise we are at the start with auto-detect. Load any header bytes; an
    // empty input here is a transparent read of zero bytes, not an error.
    if gz_avail(state) == -1 {
        return -1;
    }
    if state.in_have == 0 || (state.again && state.in_have < 4) {
        // No data at all (EOF), or a non-blocking stall before four bytes
        // arrived (unreachable with a blocking `File`). Leave `how` as LOOK and
        // wait for a later call to accumulate enough input.
        return 0;
    }

    // See if this is (likely) gzip input. If the first four bytes are consistent
    // with a gzip header, go decompress the first member; otherwise copy the
    // input transparently.
    if state.in_have > 3
        && state.in_buf[state.in_next] == GZIP_MAGIC_0
        && state.in_buf[state.in_next + 1] == GZIP_MAGIC_1
        && state.in_buf[state.in_next + 2] == GZIP_MAGIC_2
        && state.in_buf[state.in_next + 3] < GZIP_FLAG_MAX
    {
        if let Some(engine) = state.strm.inflate_state_mut() {
            engine.reset();
        }
        state.how = GzHow::Gzip;
        state.junk = 1;
        state.direct = 0;
        return 0;
    }

    // Raw I/O: copy any leftover input to the output buffer and switch to
    // passthrough. This assumes the output buffer is larger than the input
    // buffer (guaranteed by the `want << 1` sizing), which also assures space
    // for gzungetc. `direct` is deliberately left unchanged — it is `1` for an
    // auto-detect read, so gzdirect correctly reports a transparent stream.
    state.next = 0;
    let n = state.in_have;
    state.out_buf[..n].copy_from_slice(&state.in_buf[state.in_next..state.in_next + n]);
    state.have = n;
    state.in_have = 0;
    state.how = GzHow::Copy;
    0
}

/// Decompress from the input window into `out`, filling it up to the end of the
/// current deflate stream — a port of the C `gz_decomp`.
///
/// Returns `(produced, status)` where `produced` is the number of decompressed
/// bytes written to `out` and `status` is `0` on success or `-1` on failure
/// (the failure detail is recorded in [`GzState::err`]). When the gzip member
/// completes, [`how`](GzState::how) is reset to [`GzHow::Look`] so the next call
/// looks for a following member or raw data.
///
/// The caller chooses the destination: [`gz_fetch`] passes a slice of
/// `out_buf`, while [`gz_read`]'s large-read fast path passes the caller's own
/// buffer. This function never touches [`have`](GzState::have) /
/// [`next`](GzState::next); the caller sets those from `produced` as
/// appropriate.
fn gz_decomp(state: &mut GzState, out: &mut [u8]) -> (usize, i32) {
    let cap = out.len();
    let mut produced = 0usize;
    let mut ret = Z_OK;

    // Fill the output buffer up to the end of the deflate stream (C do-while).
    loop {
        // Get more input for inflate() if the window is empty.
        if state.in_have == 0 {
            if gz_avail(state) == -1 {
                ret = state.err;
                break;
            }
            if state.in_have == 0 {
                // No more input. With a blocking `File`, `again` is always
                // false, so this is a genuine unexpected EOF inside a member.
                if !state.again {
                    state.set_error(Z_BUF_ERROR, Some("unexpected end of file"));
                }
                break;
            }
        }

        // Decompress one step. The engine (`state.strm`), the input window
        // (`state.in_buf`), and the output slice (`out`) are mutually disjoint,
        // so this needs no `unsafe`.
        let in_start = state.in_next;
        let in_end = in_start + state.in_have;
        let result = {
            let Some(engine) = state.strm.inflate_state_mut() else {
                // The engine is always installed once `how == GZIP`; treat its
                // absence as corrupt internal state rather than panicking.
                state.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: inflate stream corrupt"),
                );
                ret = Z_STREAM_ERROR;
                break;
            };
            engine.inflate(
                &state.in_buf[in_start..in_end],
                &mut out[produced..],
                Flush::NoFlush,
            )
        };

        // Account for what inflate consumed and produced.
        state.in_next += result.consumed;
        state.in_have -= result.consumed;
        produced += result.produced;
        if result.produced != 0 {
            // Any decompressed output proves this is a real gzip stream, so
            // trailing-garbage tolerance is disabled from here on. This is set
            // *before* the status is examined, exactly as in C, so that a data
            // error accompanying real output is treated as a true error.
            state.junk = 0;
        }
        let progressed = result.consumed != 0 || result.produced != 0;

        match result.status {
            Ok(ReturnCode::StreamEnd) => ret = Z_STREAM_END,
            Ok(ReturnCode::Ok) => ret = Z_OK,
            // inflate() never requests a dictionary for a gzip stream; treat it,
            // as the C code does, as internal corruption.
            Ok(ReturnCode::NeedDict) => {
                state.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: inflate stream corrupt"),
                );
                ret = Z_STREAM_ERROR;
                break;
            }
            Err(ZlibError::StreamError) => {
                state.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: inflate stream corrupt"),
                );
                ret = Z_STREAM_ERROR;
                break;
            }
            Err(ZlibError::MemError) => {
                state.set_error(Z_MEM_ERROR, Some("out of memory"));
                ret = Z_MEM_ERROR;
                break;
            }
            Err(ZlibError::DataError) => {
                ret = Z_DATA_ERROR;
                if state.junk == 1 {
                    // A data error while still tolerating junk (no output has
                    // ever been produced for this member) means trailing garbage
                    // after a complete member — a clean EOF, not a failure.
                    state.in_have = 0;
                    state.eof = true;
                    state.how = GzHow::Look;
                    ret = Z_OK;
                } else {
                    // A real deflate-stream error; surface the engine's message
                    // (a `'static` string, so it does not borrow the stream).
                    let msg = state
                        .strm
                        .inflate_state()
                        .and_then(|s| s.msg)
                        .unwrap_or("compressed data error");
                    state.set_error(Z_DATA_ERROR, Some(msg));
                }
                break;
            }
            // Z_BUF_ERROR (no forward progress) and any other residual error
            // (ErrNo / VersionError, which inflate does not emit here): record
            // the code and let the progress guard below stop the loop.
            Err(other) => ret = other.as_i32(),
        }

        // Continue while output capacity remains and the stream has not ended.
        // The `progressed` guard is defensive: inflate only reports no progress
        // (`Z_BUF_ERROR`) when it needs more input or output, and this loop
        // always supplies both for a valid stream, so a valid stream always
        // advances. Breaking on a (theoretical) no-progress step guarantees the
        // loop terminates regardless.
        if produced >= cap || ret == Z_STREAM_END || !progressed {
            break;
        }
    }

    // If the gzip stream completed successfully, look for another member next.
    if ret == Z_STREAM_END {
        state.junk = 0;
        state.how = GzHow::Look;
        return (produced, 0);
    }

    (produced, if ret != Z_OK { -1 } else { 0 })
}

/// Fetch data into the output buffer — a port of the C `gz_fetch`. Precondition:
/// [`have`](GzState::have) is `0`.
///
/// Depending on [`how`](GzState::how) the data is obtained by looking for a
/// header ([`GzHow::Look`]), copied directly from the file ([`GzHow::Copy`]), or
/// decompressed from the file ([`GzHow::Gzip`]). On return `how` is left as
/// `Copy` or `Gzip` unless the end of the input file has been reached and all
/// data has been processed. Returns `-1` on error, otherwise `0`.
///
/// The loop is what chains *concatenated* gzip members: after one member ends,
/// `gz_decomp` resets `how` to `Look`, and if more input remains the loop runs
/// `gz_look` again to start the following member — all within one `gz_fetch`.
fn gz_fetch(state: &mut GzState) -> i32 {
    loop {
        match state.how {
            GzHow::Look => {
                // -> LOOK, COPY (only if never GZIP), or GZIP.
                if gz_look(state) == -1 {
                    return -1;
                }
                if state.how == GzHow::Look {
                    // No data yet (waiting for input, or transparent zero-byte
                    // read at EOF).
                    return 0;
                }
                // `how` is now COPY or GZIP: fall through to the loop condition.
            }
            GzHow::Copy => {
                // -> COPY: read straight into the (double-sized) output buffer
                // and return immediately, exactly as C does.
                let len = state.size << 1;
                match gz_load_buf(state, true, 0, len) {
                    Ok(got) => {
                        state.have = got;
                        state.next = 0;
                        return 0;
                    }
                    Err(()) => return -1,
                }
            }
            GzHow::Gzip => {
                // -> GZIP or LOOK (if end of gzip stream): decompress into the
                // output buffer. `out_buf` is temporarily detached via
                // `mem::take` so it can be the output slice while `state` is
                // borrowed mutably by `gz_decomp`; it is restored immediately
                // after. `gz_decomp` never reads `out_buf`, so the swap is safe.
                let len = state.size << 1;
                let mut out = core::mem::take(&mut state.out_buf);
                let (produced, ret) = gz_decomp(state, &mut out[..len]);
                state.out_buf = out;
                if ret == -1 {
                    return -1;
                }
                // C sets `x.have = had - avail_out` (= produced) and
                // `x.next = next_out - x.have` (= start of out_buf).
                state.have = produced;
                state.next = 0;
                // Fall through to the loop condition.
            }
        }

        // C: `while (state->x.have == 0 && (!state->eof || strm->avail_in))`.
        // Stop as soon as we have output, or once the input is fully exhausted.
        if !(state.have == 0 && (!state.eof || state.in_have != 0)) {
            return 0;
        }
    }
}

/// Skip [`skip`](GzState::skip) (`> 0`) uncompressed output bytes for a deferred
/// seek — a port of the C `gz_skip`. Returns `-1` on error, `0` on success.
fn gz_skip(state: &mut GzState) -> i32 {
    // Skip over `skip` bytes or reach end-of-file, whichever comes first.
    while state.skip > 0 {
        if state.have != 0 {
            // Skip over whatever is already in the output buffer. `skip` is an
            // `i64`; clamp it to the available `have` for this step.
            let n = core::cmp::min(state.have as i64, state.skip) as usize;
            // `consume_output` advances next/have/pos together.
            state.consume_output(n);
            state.skip -= n as i64;
        } else if state.eof && state.in_have == 0 {
            // Output buffer empty and at the end of the input: stop.
            break;
        } else if gz_fetch(state) == -1 {
            // Need more data to skip -- load up the output buffer.
            return -1;
        }
    }
    0
}

/// Read up to `buf.len()` uncompressed bytes into `buf`, returning the number of
/// bytes actually produced — a port of the C `gz_read`.
///
/// If `0` is returned, either the end of the input was reached or an error
/// occurred; [`GzState::err`] must be consulted to distinguish them. If an
/// error occurs after some bytes were already produced, that partial count is
/// returned and the error is deferred to the next call (the C "deferred error"
/// contract), which the buffer-copy step detects via [`GzState::err`].
fn gz_read(state: &mut GzState, buf: &mut [u8]) -> usize {
    // If len is zero, avoid unnecessary operations.
    if buf.is_empty() {
        return 0;
    }

    // Process a deferred skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return 0;
    }

    let total = buf.len();
    let mut got = 0usize; // bytes delivered into `buf` (C `got`)
    let mut err = false; // sticky error flag (C `err`)

    // Get `total` bytes into `buf`, or fewer if we reach the end of the input.
    loop {
        let remaining = total - got; // C `len`

        if state.have != 0 {
            // First just try copying data from the output buffer.
            let n = core::cmp::min(state.have, remaining);
            buf[got..got + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.next += n;
            state.have -= n;
            if state.err != Z_OK {
                // Caught a deferred error from a previous gz_fetch().
                err = true;
            }
            got += n;
            state.pos += n as i64;
        } else if state.eof && state.in_have == 0 {
            // Output buffer empty -- stop, we are at the end of the input.
            break;
        } else if state.how == GzHow::Look || remaining < (state.size << 1) {
            // Small read or a new stream: load up our output buffer so that
            // gzgetc() can be fast. If gz_fetch fails and produced nothing, the
            // error is fatal here; otherwise it will be caught after the copy on
            // the next pass.
            if gz_fetch(state) == -1 && state.have == 0 {
                err = true;
            }
            // No progress yet -- loop back to the copy step above. That copy
            // assures we will leave with space in the output buffer, allowing at
            // least one gzungetc() to succeed.
        } else if state.how == GzHow::Copy {
            // Large read -- copy directly into the user buffer.
            match gz_load_user(state, buf, got, remaining) {
                Ok(n) => {
                    got += n;
                    state.pos += n as i64;
                }
                Err(()) => err = true,
            }
        } else {
            // Large read -- decompress directly into the user buffer (how ==
            // GZIP). `buf` is disjoint from `state`, so no `mem::take` is needed.
            let (n, ret) = gz_decomp(state, &mut buf[got..got + remaining]);
            // The just-decompressed bytes went to the user, not the output
            // buffer, so there is nothing buffered (C: `x.have = 0`).
            state.have = 0;
            if ret == -1 {
                err = true;
            }
            got += n;
            state.pos += n as i64;
        }

        // C: `while (len && !err)`.
        if got >= total || err {
            break;
        }
    }

    // Note a read that went past the end of the file.
    if got < total && state.eof {
        state.past = true;
    }

    got
}

// ===========================================================================
// Public gz read API
//
// These functions are the safe-core entry points; the `libz-rs-sys` FFI shim
// adapts the C `gzFile` / pointer / length calling convention onto them. Each
// is a port of the like-named function in `gzread.c` (or, for `gzeof`, the one
// routed here from `gzlib.c`). `gzclose_r` is intentionally absent — it is
// routed to `close.rs` (see the module-level teardown contract).
// ===========================================================================

/// Read up to `buf.len()` bytes into `buf` — a port of the C `gzread`.
///
/// Returns the number of bytes read (which may be fewer than requested at end
/// of file), or `-1` on error. The C `gzFile`/`voidp`/`unsigned` parameter
/// triple and the "len fits in an int" guard are handled by the FFI shim; this
/// core entry point takes a borrowed output slice whose length is always valid.
pub(crate) fn gzread(state: &mut GzState, buf: &mut [u8]) -> i32 {
    // Check that the stream is open for reading.
    if state.mode != GzMode::Read {
        return -1;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.set_error(Z_OK, None); // C: gz_error(state, Z_OK, NULL)

    // Read `buf.len()` or fewer bytes.
    let n = gz_read(state, buf);

    // Distinguish a genuine error from a clean EOF when nothing was produced.
    if n == 0 {
        if state.err != Z_OK && state.err != Z_BUF_ERROR {
            return -1;
        }
        if state.again {
            // Non-blocking input stalled after some compressed bytes were read
            // but no output was produced -- not EOF. Unreachable with a blocking
            // `File`, but preserved for behavioral fidelity.
            state.set_error(Z_ERRNO, Some("read stalled"));
            return -1;
        }
    }

    n as i32
}

/// Read `nitems` items of `size` bytes each into `buf` — a port of the C
/// `gzfread`. Returns the number of *full* items read.
///
/// Mirrors the `fread`-style interface: the total byte count is `size *
/// nitems`, guarded against `size_t` overflow exactly as in C. The argument
/// order matches the C function (`buf, size, nitems, file`).
pub(crate) fn gzfread(buf: &mut [u8], size: usize, nitems: usize, state: &mut GzState) -> usize {
    // Check that the stream is open for reading.
    if state.mode != GzMode::Read {
        return 0;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return 0;
    }
    state.set_error(Z_OK, None);

    // Compute the number of bytes to read, erroring on multiplication overflow
    // (C: `len = nitems * size; if (size && len / size != nitems) ...`).
    let len = nitems.wrapping_mul(size);
    if size != 0 && len / size != nitems {
        state.set_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
        return 0;
    }
    if len == 0 {
        return 0;
    }

    // Read `len` or fewer bytes and return the number of full items. `len != 0`
    // implies `size != 0`, so the division is well defined. The read length is
    // clamped to `buf` defensively (the FFI shim sizes `buf` as `nitems * size`).
    let want = core::cmp::min(len, buf.len());
    gz_read(state, &mut buf[..want]) / size
}

/// Read and return the next byte, or `-1` for end of file or error — a port of
/// the C `gzgetc`.
pub(crate) fn gzgetc(state: &mut GzState) -> i32 {
    // Check that the stream is open for reading.
    if state.mode != GzMode::Read {
        return -1;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.set_error(Z_OK, None);

    // Fast path: return a byte straight from the output buffer if one is
    // buffered (no need to check the skip request). `pop_byte` advances
    // next/have/pos exactly as the C `gzgetc` macro does.
    if let Some(byte) = state.pop_byte() {
        return i32::from(byte);
    }

    // Nothing there -- fall back to the full read path for a single byte.
    let mut one = [0u8; 1];
    if gz_read(state, &mut one) < 1 {
        -1
    } else {
        i32::from(one[0])
    }
}

/// Backward-compatible alias for [`gzgetc`] — a port of the C `gzgetc_` (the
/// out-of-line function that older binaries call instead of the inline macro).
pub(crate) fn gzgetc_(state: &mut GzState) -> i32 {
    gzgetc(state)
}

/// Push one byte back onto the stream so it is returned by the next read — a
/// port of the C `gzungetc`. Returns the pushed byte, or `-1` on error.
///
/// The pushed byte is stored in the (double-sized) output buffer: when the
/// buffer is empty it is placed at the very end so that subsequent pushes can
/// prepend in front of it; otherwise the buffered data is slid toward the high
/// end if necessary to make room. This geometry is byte-for-byte faithful to
/// the C implementation.
pub(crate) fn gzungetc(c: i32, state: &mut GzState) -> i32 {
    // Check that the stream is open for reading.
    if state.mode != GzMode::Read {
        return -1;
    }

    // In case the file was just opened, set up the input buffer (and detect the
    // header). The result is intentionally ignored, as in C.
    if state.how == GzHow::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.set_error(Z_OK, None);

    // Process a deferred skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return -1;
    }

    // Can't push EOF.
    if c < 0 {
        return -1;
    }

    // Capacity of the double-sized output buffer.
    let dsize = state.size << 1;

    // If the output buffer is empty, put the byte at the very end so more bytes
    // can be pushed in front of it later.
    if state.have == 0 {
        state.next = dsize - 1;
        state.out_buf[state.next] = c as u8;
        state.have = 1;
        state.pos -= 1;
        state.past = false;
        return c;
    }

    // If there is no room, give up (a gzungetc() must already have been done).
    if state.have == dsize {
        state.set_error(Z_DATA_ERROR, Some("out of room to push characters"));
        return -1;
    }

    // Slide the buffered data toward the high end if it starts at offset 0, so
    // there is room to prepend the pushed byte. C copies backward from the data
    // end to the buffer end; `copy_within` has `memmove` semantics and handles
    // the possible overlap correctly.
    if state.next == 0 {
        state.out_buf.copy_within(0..state.have, dsize - state.have);
        state.next = dsize - state.have;
    }

    // Insert the byte just before the existing data.
    state.have += 1;
    state.next -= 1;
    state.out_buf[state.next] = c as u8;
    state.pos -= 1;
    state.past = false;
    c
}

/// Read a line into `buf`, stopping at a newline, after `buf.len() - 1` bytes,
/// or at end of file — a port of the C `gzgets`.
///
/// On success the read bytes are written into `buf`, a terminating NUL is
/// appended after them, and the slice of read bytes (excluding the NUL) is
/// returned. Returns [`None`] on error or when nothing was read (the C `NULL`
/// return). The FFI shim adapts this to the C `char *gzgets(gzFile, char *,
/// int)` signature, returning the original buffer pointer on `Some`.
pub(crate) fn gzgets<'a>(state: &mut GzState, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    // Check parameters and that the stream is open for reading.
    if buf.is_empty() || state.mode != GzMode::Read {
        return None;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return None;
    }
    state.set_error(Z_OK, None);

    // Process a deferred skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return None;
    }

    // Copy output up to a newline, `buf.len() - 1` bytes, or end of file --
    // whichever comes first. One byte is reserved for the terminating NUL.
    let left = buf.len() - 1;
    let mut total = 0usize;
    if left > 0 {
        loop {
            // Assure that something is in the output buffer.
            if state.have == 0 && gz_fetch(state) == -1 {
                break; // error (deferred in state.err)
            }
            if state.have == 0 {
                state.past = true; // reached end of file
                break;
            }

            // Look for the end-of-line in the current output buffer.
            let n_avail = core::cmp::min(state.have, left - total);
            let (n, found_eol) = match state.out_buf[state.next..state.next + n_avail]
                .iter()
                .position(|&b| b == b'\n')
            {
                Some(idx) => (idx + 1, true),
                None => (n_avail, false),
            };

            // Copy through the end-of-line, or the remainder if not found.
            buf[total..total + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.have -= n;
            state.next += n;
            state.pos += n as i64;
            total += n;

            if total >= left || found_eol {
                break;
            }
        }
    }

    // Append a terminating zero to the string, or return None if nothing was
    // read (the user is responsible for embedded zeros in the content).
    if total == 0 {
        return None;
    }
    buf[total] = 0;
    Some(&buf[..total])
}

/// Return `1` if the stream is being read transparently (not a gzip stream),
/// `0` otherwise — a port of the C `gzdirect`.
///
/// If the stream's nature is not yet known but can be determined, this triggers
/// the header look-up first (mainly useful right after `gzopen`/`gzdopen`).
pub(crate) fn gzdirect(state: &mut GzState) -> i32 {
    // If the state is not known, but we can find out, then do so (this is mainly
    // for right after a gzopen() or gzdopen()).
    if state.mode == GzMode::Read && state.how == GzHow::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    // 1 if transparent, 0 if processing a gzip stream.
    i32::from(state.direct == 1)
}

/// Return non-zero if a read request went *past* the end of the file — a port
/// of the C `gzeof` (defined in `gzlib.c`, routed here by the folder layout).
///
/// Crucially this reflects [`past`](GzState::past) (a read that asked for more
/// than was available), **not** [`eof`](GzState::eof) (merely having seen the
/// input EOF). For a write stream, or any non-read/non-write mode, it returns
/// `0`.
pub(crate) fn gzeof(state: &GzState) -> i32 {
    if state.mode == GzMode::Read {
        i32::from(state.past)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Write` and the atomics are not in the prelude; `String`, `Vec`, the
    // `vec!` / `format!` macros, and `ToString` all come from the std prelude
    // (tests build under `std`), mirroring the parent module's import style.
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // --- Canonical gzip fixtures (generated with Python `gzip`, mtime = 0,
    //     compresslevel = 9) and their decompressed plaintext references. ---

    /// `gzip(b"hello, hello, hello, hello, world!\n")` — one complete member.
    const GZ_SINGLE: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0xc8, 0xc0, 0x46, 0x95, 0xe7, 0x17, 0xe5, 0xa4, 0x28, 0x72, 0x01, 0x00, 0x84,
        0x12, 0x08, 0x6f, 0x23, 0x00, 0x00, 0x00,
    ];
    const SINGLE_PLAIN: &[u8] = b"hello, hello, hello, hello, world!\n";

    /// `gzip(b"first member\n") ++ gzip(b"second member\n")` — concatenated.
    const GZ_CONCAT: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x4b, 0xcb, 0x2c, 0x2a, 0x2e,
        0x51, 0xc8, 0x4d, 0xcd, 0x4d, 0x4a, 0x2d, 0xe2, 0x02, 0x00, 0xa7, 0xf4, 0x85, 0x0a, 0x0d,
        0x00, 0x00, 0x00, 0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x2b, 0x4e,
        0x4d, 0xce, 0xcf, 0x4b, 0x51, 0xc8, 0x4d, 0xcd, 0x4d, 0x4a, 0x2d, 0xe2, 0x02, 0x00, 0x36,
        0x18, 0x4b, 0x0e, 0x0e, 0x00, 0x00, 0x00,
    ];
    const CONCAT_PLAIN: &[u8] = b"first member\nsecond member\n";

    /// `gzip(b"clean data line\n")` followed by non-gzip trailing bytes.
    const GZ_TRAILING_GARBAGE: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x4b, 0xce, 0x49, 0x4d, 0xcc,
        0x53, 0x48, 0x49, 0x2c, 0x49, 0x54, 0xc8, 0xc9, 0xcc, 0x4b, 0xe5, 0x02, 0x00, 0x8a, 0x12,
        0x84, 0x56, 0x10, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x74, 0x72, 0x61, 0x69, 0x6c,
        0x69, 0x6e, 0x67, 0x20, 0x6a, 0x75, 0x6e, 0x6b, 0x20, 0x74, 0x68, 0x61, 0x74, 0x20, 0x69,
        0x73, 0x20, 0x6e, 0x6f, 0x74, 0x20, 0x67, 0x7a, 0x69, 0x70,
    ];
    const TRAILING_PLAIN: &[u8] = b"clean data line\n";

    /// A plain (non-gzip) payload for transparent-read tests.
    const TRANSPARENT_PLAIN: &[u8] = b"this is not gzip, just raw text\n";

    /// Write `bytes` to a unique temp file and return a read state over it,
    /// mimicking a plain `gzopen(path, "r")`. The temp file is unlinked
    /// immediately after opening (unlink-after-open on Unix), so no litter
    /// remains even if a test panics before the state is dropped.
    fn open_read(bytes: &[u8]) -> GzState {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zlib_rs_gzread_test_{}_{}.bin",
            std::process::id(),
            id
        ));
        {
            let mut f = File::create(&path).expect("create temp file");
            f.write_all(bytes).expect("write temp file");
            f.flush().expect("flush temp file");
        }
        let file = File::open(&path).expect("open temp file for read");
        // Unlink now; the open handle keeps the data readable until it drops.
        let _ = std::fs::remove_file(&path);
        let mut state = GzState::new(file, path.to_string_lossy().to_string(), GzMode::Read);
        // gz_open's read-mode finalization (open.rs / gzlib.c): a plain "r" open
        // enables auto-detect transparency, which sets `direct = 1`.
        state.direct = 1;
        state
    }

    /// Build a non-read state for negative tests, over `/dev/null`.
    fn null_state(mode: GzMode) -> GzState {
        let file = File::open("/dev/null").expect("open /dev/null");
        GzState::new(file, String::from("null"), mode)
    }

    /// Read all output via repeated `gzread` (deliberately small chunks, to
    /// exercise the buffered path) until EOF, asserting no error occurs.
    fn read_all(state: &mut GzState) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 7];
        loop {
            let n = gzread(state, &mut chunk);
            assert!(n >= 0, "gzread returned an error: err = {}", state.err);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n as usize]);
        }
        out
    }

    #[test]
    fn gzip_stream_is_detected_and_not_direct() {
        let mut state = open_read(GZ_SINGLE);
        assert_eq!(
            gzdirect(&mut state),
            0,
            "gzip stream must report not-direct"
        );
        assert_eq!(state.how, GzHow::Gzip);
        assert_eq!(state.direct, 0);
    }

    #[test]
    fn transparent_stream_is_direct() {
        let mut state = open_read(TRANSPARENT_PLAIN);
        assert_eq!(
            gzdirect(&mut state),
            1,
            "non-gzip stream must report direct"
        );
        assert_eq!(state.how, GzHow::Copy);
        assert_eq!(state.direct, 1);
    }

    #[test]
    fn gzread_decompresses_single_member_in_small_chunks() {
        let mut state = open_read(GZ_SINGLE);
        assert_eq!(read_all(&mut state), SINGLE_PLAIN);
    }

    #[test]
    fn gzread_decompresses_single_member_in_one_call() {
        let mut state = open_read(GZ_SINGLE);
        let mut buf = [0u8; 256];
        let n = gzread(&mut state, &mut buf);
        assert_eq!(n as usize, SINGLE_PLAIN.len());
        assert_eq!(&buf[..n as usize], SINGLE_PLAIN);
    }

    #[test]
    fn gzgetc_reads_byte_by_byte_then_eof() {
        let mut state = open_read(GZ_SINGLE);
        let mut out = Vec::new();
        loop {
            let c = gzgetc(&mut state);
            if c < 0 {
                break;
            }
            out.push(c as u8);
        }
        assert_eq!(out, SINGLE_PLAIN);
        // `gzgetc_` is a backward-compatible alias and behaves identically.
        assert_eq!(gzgetc_(&mut state), -1);
    }

    #[test]
    fn gzgets_reads_whole_single_line_and_nul_terminates() {
        let mut state = open_read(GZ_SINGLE);
        let mut buf = [0u8; 64];
        let line = gzgets(&mut state, &mut buf).expect("a line").to_vec();
        assert_eq!(line, SINGLE_PLAIN);
        // A terminating NUL is written just past the returned data.
        assert_eq!(buf[SINGLE_PLAIN.len()], 0);
        // At end of file, `gzgets` returns None.
        let mut buf2 = [0u8; 64];
        assert!(gzgets(&mut state, &mut buf2).is_none());
    }

    #[test]
    fn gzgets_stops_at_each_newline() {
        let mut state = open_read(GZ_CONCAT); // "first member\nsecond member\n"
        let mut buf = [0u8; 64];
        let first = gzgets(&mut state, &mut buf).expect("first line").to_vec();
        assert_eq!(first, b"first member\n");
        let second = gzgets(&mut state, &mut buf).expect("second line").to_vec();
        assert_eq!(second, b"second member\n");
    }

    #[test]
    fn gzungetc_round_trips_a_read_byte() {
        let mut state = open_read(GZ_SINGLE);
        let first = gzgetc(&mut state);
        assert_eq!(first, i32::from(SINGLE_PLAIN[0])); // 'h'
        assert_eq!(gzungetc(first, &mut state), first);
        assert_eq!(gzgetc(&mut state), first); // same byte comes back
        let mut rest = Vec::new();
        loop {
            let c = gzgetc(&mut state);
            if c < 0 {
                break;
            }
            rest.push(c as u8);
        }
        assert_eq!(rest, &SINGLE_PLAIN[1..]);
    }

    #[test]
    fn gzungetc_before_any_read_is_returned_first() {
        let mut state = open_read(GZ_SINGLE);
        assert_eq!(gzungetc(i32::from(b'Z'), &mut state), i32::from(b'Z'));
        assert_eq!(gzgetc(&mut state), i32::from(b'Z'));
        // The real content follows the pushed byte.
        assert_eq!(read_all(&mut state), SINGLE_PLAIN);
    }

    #[test]
    fn gzungetc_slides_when_data_starts_at_buffer_front() {
        let mut state = open_read(GZ_SINGLE);
        // Drive a fetch directly so the buffer holds the member with next == 0
        // (the condition that forces the slide branch in gzungetc).
        assert_eq!(gz_fetch(&mut state), 0);
        assert!(state.have > 0);
        assert_eq!(state.next, 0);
        let pushed = i32::from(b'Q');
        assert_eq!(gzungetc(pushed, &mut state), pushed);
        assert_eq!(gzgetc(&mut state), pushed);
        assert_eq!(read_all(&mut state), SINGLE_PLAIN);
    }

    #[test]
    fn gzungetc_rejects_eof_push() {
        let mut state = open_read(GZ_SINGLE);
        assert_eq!(gzungetc(-1, &mut state), -1, "cannot push EOF");
    }

    #[test]
    fn transparent_passthrough_returns_raw_bytes() {
        let mut state = open_read(TRANSPARENT_PLAIN);
        assert_eq!(read_all(&mut state), TRANSPARENT_PLAIN);
        assert_eq!(gzdirect(&mut state), 1);
    }

    #[test]
    fn concatenated_members_are_joined() {
        let mut state = open_read(GZ_CONCAT);
        assert_eq!(read_all(&mut state), CONCAT_PLAIN);
    }

    #[test]
    fn trailing_garbage_after_member_is_tolerated() {
        let mut state = open_read(GZ_TRAILING_GARBAGE);
        // Only the clean member is returned; the trailing garbage is silently
        // dropped at a clean EOF — this exercises the `junk == 1` machinery.
        assert_eq!(read_all(&mut state), TRAILING_PLAIN);
        assert!(
            state.err == Z_OK || state.err == Z_BUF_ERROR,
            "trailing garbage must not raise a serious error (err = {})",
            state.err
        );
    }

    #[test]
    fn gzeof_reports_past_not_eof() {
        let mut state = open_read(GZ_SINGLE);
        // Read exactly the payload: the request is fully satisfied, so a read
        // past the end has not occurred even though the input EOF was observed.
        let mut exact = vec![0u8; SINGLE_PLAIN.len()];
        let n = gzread(&mut state, &mut exact);
        assert_eq!(n as usize, SINGLE_PLAIN.len());
        assert_eq!(exact, SINGLE_PLAIN);
        assert_eq!(gzeof(&state), 0, "an exact read must not set `past`");
        // A further read finds nothing and goes past the end.
        let mut more = [0u8; 8];
        assert_eq!(gzread(&mut state, &mut more), 0);
        assert_eq!(gzeof(&state), 1, "a read past the end must set `past`");
    }

    #[test]
    fn gzfread_returns_full_items_only() {
        let mut state = open_read(GZ_SINGLE); // 35-byte payload
        let mut buf = [0u8; 64];
        // 35 bytes / 5-byte items = 7 full items (the 0 leftover byte is kept).
        let items = gzfread(&mut buf, 5, 12, &mut state);
        assert_eq!(items, SINGLE_PLAIN.len() / 5);
        assert_eq!(&buf[..SINGLE_PLAIN.len()], SINGLE_PLAIN);
    }

    #[test]
    fn gzfread_detects_size_overflow() {
        let mut state = open_read(GZ_SINGLE);
        let mut buf = [0u8; 8];
        // size * nitems overflows usize -> error, zero items.
        let items = gzfread(&mut buf, usize::MAX, 2, &mut state);
        assert_eq!(items, 0);
        assert_eq!(state.err, Z_STREAM_ERROR);
    }

    #[test]
    fn read_api_rejects_non_read_mode() {
        let mut state = null_state(GzMode::Write);
        let mut buf = [0u8; 4];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(gzgetc(&mut state), -1);
        assert_eq!(gzgetc_(&mut state), -1);
        assert_eq!(gzungetc(i32::from(b'a'), &mut state), -1);
        assert_eq!(gzfread(&mut buf, 1, 4, &mut state), 0);
        assert!(gzgets(&mut state, &mut buf).is_none());
    }

    #[test]
    fn gzeof_is_zero_for_non_read_modes() {
        assert_eq!(gzeof(&null_state(GzMode::Write)), 0);
        assert_eq!(gzeof(&null_state(GzMode::None)), 0);
    }

    #[test]
    fn empty_output_buffer_reads_nothing() {
        let mut state = open_read(GZ_SINGLE);
        let mut empty: [u8; 0] = [];
        assert_eq!(gzread(&mut state, &mut empty), 0);
        assert!(gzgets(&mut state, &mut empty).is_none());
    }
}
