//! Buffered gzip **write** path — an idiomatic, memory-safe port of the C
//! `gzwrite.c` translation unit.
//!
//! This module implements the engine that turns caller writes into a gzip
//! stream on disk, plus the public `gz*` write API:
//!
//! * Internal engine ([`GzState`] methods): [`GzState::gz_init`],
//!   [`GzState::gz_comp`], [`GzState::gz_zero`], [`GzState::gz_write`],
//!   [`GzState::gz_vacate`] and the finalize hook [`GzState::finish_write`].
//! * Public API (free functions taking `&mut GzState`): [`gzwrite`],
//!   [`gzfwrite`], [`gzputc`], [`gzputs`], [`gzprintf`] / [`gzvprintf`] and
//!   [`gzflush`].
//!
//! # Routing (per the gz-module split)
//!
//! * `gzsetparams` lives in `open.rs`; it *calls* [`GzState::gz_comp`] /
//!   [`GzState::gz_zero`], which is why those are `pub(crate)` methods here.
//! * `gzclose_w` lives in `close.rs`; it (and [`GzState`]'s `Drop`/`finalize`)
//!   call [`GzState::finish_write`], the `gz_comp(Z_FINISH)` finalize path.
//!
//! # Safety & mapping
//!
//! The whole crate is built under `#![forbid(unsafe_code)]`, so this file
//! contains **zero** `unsafe`. The C `int fd` + `write()` becomes
//! [`std::io::Write`] on `state.file: std::fs::File`; the `vsnprintf`-into-a-
//! double-buffer dance for `gzprintf` becomes [`core::fmt`] via a small bounded
//! [`core::fmt::Write`] adapter ([`BufWriter`]); and raw-pointer buffer
//! arithmetic becomes index/slice operations over `state.in_buf` /
//! `state.out_buf: Vec<u8>`.
//!
//! ## Buffer / cursor model
//!
//! The C code keeps live `strm->next_in`/`avail_in`/`next_out`/`avail_out`
//! cursors across calls. The crate's [`ZStream`](crate::stream::ZStream) stores
//! none of these (input and output are per-call borrowed slices), and
//! [`GzState`] has no dedicated write cursors, so this port:
//!
//! * tracks **pending write input** as a byte count in `state.have`, with the
//!   bytes left-packed at `in_buf[0..have]`. In write mode `have`/`next` are
//!   otherwise unused (the seek/tell helpers touch them only when *reading*),
//!   and `set_error` clears `have` only on a *fatal* error — which already
//!   tears the stream down — so the reuse is invisible to callers; and
//! * treats `out_buf` as **per-call scratch**: every [`GzState::gz_comp`] call
//!   drains all bytes it produces to the file before returning. The bytes
//!   written are exactly the DEFLATE engine's output regardless of *when* the
//!   buffer is drained, so the on-disk stream stays byte-identical to C zlib.
//!
//! Compiled only when the `gz-io` feature is enabled (which implies `std`).

use crate::constants::{DEF_MEM_LEVEL, Flush, MAX_WBITS, Strategy, Z_DEFLATED};
use crate::deflate;
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GzMode, GzState};
use std::io::Write;

// ---------------------------------------------------------------------------
// zlib status codes, as `i32`, mirroring the values `gzwrite.c` passes around.
// Derived from the strongly-typed enums so they cannot drift out of sync. Only
// the codes actually used by this module are defined (others would trip the
// crate's `-D warnings` dead-code gate).
// ---------------------------------------------------------------------------

/// `Z_OK` (0).
const Z_OK: i32 = ReturnCode::Ok.as_i32();
/// `Z_ERRNO` (-1) — reported for a failed file write.
const Z_ERRNO: i32 = ZlibError::ErrNo.as_i32();
/// `Z_STREAM_ERROR` (-2).
const Z_STREAM_ERROR: i32 = ZlibError::StreamError.as_i32();
/// `Z_MEM_ERROR` (-4).
const Z_MEM_ERROR: i32 = ZlibError::MemError.as_i32();
/// `Z_FINISH` (4) — upper bound of the `gzflush` flush argument.
const Z_FINISH: i32 = Flush::Finish.as_i32();

// ---------------------------------------------------------------------------
// `core::fmt` sink for `gzprintf`
// ---------------------------------------------------------------------------

/// A bounded [`core::fmt::Write`] sink that formats directly into a fixed byte
/// region of the gz input buffer, replacing the C `vsnprintf(next, size, ...)`.
///
/// Writing stops (and [`overflow`](Self::overflow) is set) the moment the
/// formatted output would exceed the region, mirroring `vsnprintf`'s truncation
/// behavior. [`pos`](Self::pos) is the number of bytes successfully written.
struct BufWriter<'a> {
    /// The scratch region to format into.
    buf: &'a mut [u8],
    /// Number of bytes written so far.
    pos: usize,
    /// Set once a write was refused because it would overrun `buf`.
    overflow: bool,
}

impl core::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.pos + bytes.len();
        if end > self.buf.len() {
            // Would overrun the scratch region: record truncation and abort the
            // format so `core::fmt::write` returns `Err`.
            self.overflow = true;
            return Err(core::fmt::Error);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }
}

// ===========================================================================
// Internal engine — `impl GzState`
// ===========================================================================

impl GzState {
    /// Allocate the write buffers and (when compressing) initialize the gzip
    /// DEFLATE engine. Port of C `gz_init` (`gzwrite.c` lines 11-57).
    ///
    /// The input buffer is **double-sized** (`want << 1`): the upper half is
    /// scratch space for [`gzvprintf`]. The output buffer is single-sized
    /// (`want`) and is allocated *only* when compressing (`direct == 0`); a
    /// transparent stream needs neither an output buffer nor a deflate engine.
    ///
    /// Returns `0` on success or `-1` after recording `Z_MEM_ERROR`.
    pub(crate) fn gz_init(&mut self) -> i32 {
        // Input buffer, double-sized for `gzprintf` (C: `malloc(want << 1)`).
        self.in_buf = vec![0u8; self.want << 1];

        if self.direct == 0 {
            // Compressing: output buffer + gzip-wrapped deflate engine.
            self.out_buf = vec![0u8; self.want];

            // `strategy` is stored as a raw C int; convert it to the strongly
            // typed enum here, exactly as documented on the field. An invalid
            // value is handled like any failed `deflateInit2` in C `gz_init`.
            let Some(strategy) = Strategy::try_from_i32(self.strategy) else {
                self.in_buf = Vec::new();
                self.out_buf = Vec::new();
                self.set_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            };

            // windowBits = MAX_WBITS + 16 selects the **gzip** wrapper;
            // method = Z_DEFLATED; memLevel = DEF_MEM_LEVEL (C lines 30-33).
            if deflate::deflate_init2(
                &mut self.strm,
                self.level,
                Z_DEFLATED,
                MAX_WBITS + 16,
                DEF_MEM_LEVEL,
                strategy,
            )
            .is_err()
            {
                self.in_buf = Vec::new();
                self.out_buf = Vec::new();
                self.set_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }

        // Mark the buffers as allocated. `size != 0` is the load-bearing
        // "initialized" sentinel checked throughout the write path.
        self.size = self.want;
        0
    }

    /// Slice-driven compression core: compress (or transparently pass through)
    /// `input`, draining all output to the file. Shared implementation behind
    /// [`gz_comp`](Self::gz_comp) and the large-write fast path of
    /// [`gz_write`](Self::gz_write). Port of the body of C `gz_comp`
    /// (`gzwrite.c` lines 65-148).
    ///
    /// The caller guarantees the buffers are already allocated (`size != 0`).
    /// `flush` is a valid deflate flush value. Returns `0` on success, `-1`
    /// after recording an error.
    fn gz_comp_input(&mut self, input: &[u8], flush: Flush) -> i32 {
        // -- transparent write: copy straight to the file (C lines 76-92). --
        if self.direct != 0 {
            if !input.is_empty() {
                if let Err(e) = self.file.write_all(input) {
                    let msg = e.to_string();
                    self.again = false; // blocking File: the EAGAIN retry is unreachable
                    self.set_error(Z_ERRNO, Some(&msg));
                    return -1;
                }
            }
            return 0;
        }

        // -- run deflate to consume a previous Z_FINISH (C lines 94-101). --
        if self.reset {
            if input.is_empty() && flush == Flush::NoFlush {
                // Don't start a new gzip member with no data and no flush.
                return 0;
            }
            // Begin a fresh gzip member. `deflate_reset` cannot fail for an
            // initialized stream; ignore its result as the C code does.
            let _ = deflate::deflate_reset(&mut self.strm);
            self.reset = false;
        }

        // -- compress loop (C lines 104-140). --
        //
        // Output cursors are local because `out_buf` is per-call scratch:
        //  * `produced` — bytes deflate has emitted into `out_buf`;
        //  * `written`  — bytes of `out_buf` already flushed to the file.
        let size = self.size;
        let mut consumed = 0usize;
        let mut produced = 0usize;
        let mut written = 0usize;
        let mut ret: Result<ReturnCode, ZlibError> = Ok(ReturnCode::Ok);

        loop {
            // Write out the buffer when it is full, or when flushing — but for a
            // Z_FINISH, hold off until the stream has actually ended
            // (C lines 106-122).
            let avail_out = size - produced;
            let flushing = flush != Flush::NoFlush
                && (flush != Flush::Finish || matches!(ret, Ok(ReturnCode::StreamEnd)));
            if avail_out == 0 || flushing {
                if written < produced {
                    if let Err(e) = self.file.write_all(&self.out_buf[written..produced]) {
                        let msg = e.to_string();
                        self.again = false;
                        self.set_error(Z_ERRNO, Some(&msg));
                        return -1;
                    }
                    written = produced;
                }
                if avail_out == 0 {
                    // Buffer fully drained: reuse it from the top.
                    produced = 0;
                    written = 0;
                }
            }

            // Compress into the free tail of the output buffer (C lines 124-133).
            // `strm`, `out_buf` and `input` are disjoint, so the borrows are
            // independent.
            let (r, c, p) = deflate::deflate(
                &mut self.strm,
                &input[consumed..],
                &mut self.out_buf[produced..size],
                flush,
            );
            if matches!(r, Err(ZlibError::StreamError)) {
                // The stream state is corrupt — should be impossible.
                self.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: deflate stream corrupt"),
                );
                return -1;
            }
            ret = r;
            consumed += c;
            produced += p;

            // Continue while deflate keeps producing output (C `while (have)`).
            if p == 0 {
                break;
            }
        }

        // `out_buf` is per-call scratch, so flush any remaining produced bytes
        // (the Z_NO_FLUSH leftover the C code would have carried to a later
        // call). The on-disk bytes are identical either way.
        if written < produced {
            if let Err(e) = self.file.write_all(&self.out_buf[written..produced]) {
                let msg = e.to_string();
                self.again = false;
                self.set_error(Z_ERRNO, Some(&msg));
                return -1;
            }
        }

        // After a finish, the next write must begin a fresh gzip member
        // (C lines 142-145).
        if flush == Flush::Finish {
            self.reset = true;
        }
        0
    }

    /// Compress the pending input buffered at `in_buf[0..have]` and write the
    /// result to the file. Buffered entry point used by the public write API,
    /// [`gz_zero`](Self::gz_zero), [`gz_vacate`](Self::gz_vacate),
    /// [`finish_write`](Self::finish_write) and `gzsetparams` (in `open.rs`).
    /// Port of C `gz_comp` (`gzwrite.c` lines 65-148).
    ///
    /// Returns `0` on success, `-1` after recording an error. Calling it with
    /// no pending input and `Flush::NoFlush` is a safe no-op.
    pub(crate) fn gz_comp(&mut self, flush: Flush) -> i32 {
        // Allocate on first use (C lines 70-73).
        if self.size == 0 && self.gz_init() == -1 {
            return -1;
        }

        // Lend the pending-input region to the slice-driven core. `mem::take`
        // moves `in_buf` out so it can be borrowed immutably while `&mut self`
        // is held for `strm`/`out_buf`/`file`; it is restored immediately after.
        let have = self.have;
        let in_buf = core::mem::take(&mut self.in_buf);
        let ret = self.gz_comp_input(&in_buf[..have], flush);
        self.in_buf = in_buf;
        if ret != -1 {
            // Blocking File never leaves a partial remainder: all pending input
            // has been consumed.
            self.have = 0;
        }
        ret
    }

    /// Compress `state.skip` zero bytes to satisfy a pending forward seek. Port
    /// of C `gz_zero` (`gzwrite.c` lines 154-182).
    ///
    /// Any genuine pending input is compressed first, then runs of zeros are
    /// fed through the engine in `size`-byte chunks. Returns `0`/`-1`.
    ///
    /// Exposed as `pub(crate)` because `gz/open.rs` (the home of `gzsetparams`)
    /// calls `state.gz_zero()` to drain a pending forward seek before changing
    /// compression parameters.
    pub(crate) fn gz_zero(&mut self) -> i32 {
        // Compress whatever is already buffered (C lines 160-161).
        if self.have > 0 && self.gz_comp(Flush::NoFlush) == -1 {
            return -1;
        }

        // Compress `skip` zero bytes in chunks of at most `size` (C lines 164-180).
        let mut first = true;
        while self.skip > 0 {
            // n = min(size, skip).
            let n = if (self.size as i64) < self.skip {
                self.size
            } else {
                self.skip as usize
            };
            if first {
                // Zero the region once; deflate never mutates its input, so the
                // zeros persist across the remaining chunks.
                self.in_buf[..n].fill(0);
                first = false;
            }
            self.have = n;
            let ret = self.gz_comp(Flush::NoFlush);
            // The whole chunk is consumed on a blocking File.
            self.pos += n as i64;
            self.skip -= n as i64;
            if ret == -1 {
                return -1;
            }
        }
        0
    }

    /// Write `buf` to the stream, compressing as needed. Port of C `gz_write`
    /// (`gzwrite.c` lines 188-252).
    ///
    /// Returns the number of bytes consumed: the full `buf.len()` on success,
    /// `0` on error (all input is buffered or compressed, so there is no partial
    /// result on a blocking File).
    pub(crate) fn gz_write(&mut self, buf: &[u8]) -> usize {
        let len = buf.len();
        // Nothing to do (C lines 193-194).
        if len == 0 {
            return 0;
        }

        // Allocate buffers on first use (C lines 197-198).
        if self.size == 0 && self.gz_init() == -1 {
            return 0;
        }

        // Honor a pending forward seek (C lines 201-202).
        if self.skip != 0 && self.gz_zero() == -1 {
            return 0;
        }

        if len < self.size {
            // -- small write: accumulate into the input buffer, compressing
            //    whenever it fills (C lines 205-228). Pending bytes are
            //    left-packed at `in_buf[0..have]`, so the next byte goes at
            //    index `have` and no cursor reset is required. --
            let mut off = 0usize;
            loop {
                let have = self.have;
                let copy = core::cmp::min(self.size - have, len - off);
                self.in_buf[have..have + copy].copy_from_slice(&buf[off..off + copy]);
                self.have += copy;
                self.pos += copy as i64;
                off += copy;
                if off == len {
                    break;
                }
                // Buffer full: compress and keep going.
                if self.gz_comp(Flush::NoFlush) == -1 {
                    return 0;
                }
            }
        } else {
            // -- large write: drain any pending input, then compress the caller
            //    buffer directly without copying through `in_buf`
            //    (C lines 230-247). Chunking is unnecessary because the
            //    slice-driven core drains output as it fills, so it accepts an
            //    input of any length. --
            if self.have > 0 && self.gz_comp(Flush::NoFlush) == -1 {
                return 0;
            }
            if self.gz_comp_input(buf, Flush::NoFlush) == -1 {
                return 0;
            }
            self.pos += len as i64;
        }

        // All of `buf` was buffered or compressed (C line 251).
        len
    }

    /// Compact the input buffer so [`gzvprintf`] has its full `size`-byte
    /// scratch region in the upper half available. Port of C `gz_vacate`
    /// (`gzwrite.c` lines 382-396).
    ///
    /// If the buffered input does not extend past the first half there is
    /// nothing to do; otherwise it is compressed out (fully drained on a
    /// blocking File). The `gz_comp` result is intentionally ignored — the
    /// caller inspects `state.err`. Returns `1` if the second half is *still*
    /// occupied afterward (unreachable here), else `0`.
    fn gz_vacate(&mut self) -> i32 {
        // Pending input confined to the first half: scratch already free.
        if self.have <= self.size {
            return 0;
        }
        // Compress to free the second half.
        let _ = self.gz_comp(Flush::NoFlush);
        if self.have == 0 {
            return 0;
        }
        // Unreachable with a blocking File (gz_comp drains fully). The data is
        // already left-packed at index 0, so there is nothing to move.
        (self.have > self.size) as i32
    }

    /// Finish the gzip stream: satisfy a pending seek, then run
    /// `gz_comp(Z_FINISH)` so the closing block, CRC-32 and ISIZE trailer are
    /// emitted and flushed. This is the compress-finish portion of C
    /// `gzclose_w` (`gzwrite.c` lines 667-700), factored out for `close.rs` and
    /// for [`GzState`]'s finalize/`Drop`.
    ///
    /// Idempotent: a second call after a completed finish (nothing newly
    /// buffered) is a no-op, so no duplicate gzip trailer is emitted. The owned
    /// [`ZStream`](crate::stream::ZStream) tears the deflate engine down on
    /// drop, so no explicit `deflateEnd` is required here. Returns the recorded
    /// error code (`Z_OK` on success).
    pub(crate) fn finish_write(&mut self) -> i32 {
        // Flush a pending forward seek first (C `gzclose_w`).
        if self.skip != 0 && self.gz_zero() == -1 {
            return self.err;
        }
        // If a finish has already completed and nothing new is buffered, do not
        // emit a second trailer.
        if self.reset && self.have == 0 {
            return self.err;
        }
        // Finish the deflate stream and flush it to the file.
        let _ = self.gz_comp(Flush::Finish);
        self.err
    }
}

// ===========================================================================
// Public gz write API (free functions over `&mut GzState`)
// ===========================================================================

/// Write `buf` to the gzip stream. Port of C `gzwrite` (`gzwrite.c` lines
/// 255-279).
///
/// Returns the number of bytes written, or `0` on error or for a stream not
/// open for writing. The C "length fits in an `int`" guard is a concern of the
/// FFI boundary and is intentionally not reproduced in the safe core.
pub(crate) fn gzwrite(state: &mut GzState, buf: &[u8]) -> i32 {
    // Writable stream with no serious error? (C lines 263-264.)
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.set_error(Z_OK, None);
    state.gz_write(buf) as i32
}

/// Write `nitems` items of `size` bytes each from `buf`. Port of C `gzfwrite`
/// (`gzwrite.c` lines 280-304).
///
/// Returns the number of full items written, or `0` on error. `buf` must
/// contain at least `size * nitems` bytes (the caller's contract, as in C);
/// the bound is enforced with [`slice::get`] so the safe API never panics.
pub(crate) fn gzfwrite(buf: &[u8], size: usize, nitems: usize, state: &mut GzState) -> usize {
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return 0;
    }
    state.set_error(Z_OK, None);

    // Total length, guarding against `size_t` overflow exactly as the C
    // `len / size != nitems` check does (C lines 297-301).
    let Some(len) = size.checked_mul(nitems) else {
        state.set_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
        return 0;
    };
    if len == 0 {
        return 0;
    }
    let Some(slice) = buf.get(..len) else {
        state.set_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
        return 0;
    };
    // `size != 0` here (len != 0), so the division is well defined (C line 304).
    state.gz_write(slice) / size
}

/// Write a single byte (the low 8 bits of `c`). Port of C `gzputc`
/// (`gzwrite.c` lines 307-348).
///
/// Returns the byte written (`c & 0xff`), or `-1` on error.
pub(crate) fn gzputc(state: &mut GzState, c: i32) -> i32 {
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.set_error(Z_OK, None);

    // Honor a pending forward seek (C lines 326-327).
    if state.skip != 0 && state.gz_zero() == -1 {
        return -1;
    }

    // Fast path: drop the byte straight into the input buffer if there is room
    // (C lines 330-344). Pending bytes are left-packed, so `have` is both the
    // current fill and the index of the next free slot.
    if state.size != 0 {
        let have = state.have;
        if have < state.size {
            state.in_buf[have] = c as u8;
            state.have += 1;
            state.pos += 1;
            return c & 0xff;
        }
    }

    // Otherwise fall back to the general write path (C lines 347).
    let byte = [c as u8];
    if state.gz_write(&byte) != 1 {
        return -1;
    }
    c & 0xff
}

/// Write the bytes of `s`. Port of C `gzputs` (`gzwrite.c` lines 350-372).
///
/// Takes a byte slice rather than a NUL-terminated C string: the FFI shim
/// supplies the string's bytes (without the NUL). Returns the number of bytes
/// written, or `-1` on error.
pub(crate) fn gzputs(state: &mut GzState, s: &[u8]) -> i32 {
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return -1;
    }
    state.set_error(Z_OK, None);

    let len = s.len();
    // (The C "length fits in an `int`" guard is an FFI-boundary concern.)
    let put = state.gz_write(s);
    if len != 0 && put == 0 { -1 } else { put as i32 }
}

/// Formatted write into the gzip stream — the [`core::fmt`] analogue of C
/// `gzvprintf` (`gzwrite.c` lines 403-498).
///
/// There is no `va_list` in Rust: callers build [`core::fmt::Arguments`] with
/// the standard [`format_args!`] macro. The arguments are formatted into the
/// upper (scratch) half of the double-sized input buffer — which is exactly why
/// [`GzState::gz_init`] allocates `want << 1` bytes — then compressed via the
/// [`gz_vacate`](GzState::gz_vacate) discipline.
///
/// Returns the number of bytes written, `0` if the formatted output did not fit
/// in `size` bytes, or a negative `Z_*` code on error.
pub(crate) fn gzvprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    // Writable stream with no serious error? (C lines 422-423.)
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.set_error(Z_OK, None);

    // Allocate on first use (C lines 427-428).
    if state.size == 0 && state.gz_init() == -1 {
        return state.err;
    }

    // Honor a pending forward seek (C lines 431-432).
    if state.skip != 0 && state.gz_zero() == -1 {
        return state.err;
    }

    // Free the second (scratch) half, compressing out the first half if needed
    // (C lines 438-448). The non-blocking "stalled write" Z_BUF_ERROR branch is
    // unreachable with a blocking File.
    state.gz_vacate();
    if state.err != Z_OK && !state.again {
        return state.err;
    }

    // Format directly into `in_buf[have .. have + size]`. After `gz_vacate`,
    // `have <= size`, so this scratch slice stays within the double-sized
    // buffer (C lines 450-453).
    let have = state.have;
    let size = state.size;
    let len = {
        let region = &mut state.in_buf[have..have + size];
        let mut writer = BufWriter {
            buf: region,
            pos: 0,
            overflow: false,
        };
        let _ = core::fmt::write(&mut writer, args);
        if writer.overflow {
            // Truncated: the formatted output did not fit (C `len >= size`).
            return 0;
        }
        writer.pos
    };

    // Check that the result fits (C lines 470-471): reject empty or
    // size-or-larger output, leaving at most `size - 1` usable bytes as C does.
    if len == 0 || len >= size {
        return 0;
    }

    // Account for the formatted bytes (C lines 474-475).
    state.have += len;
    state.pos += len as i64;

    // Write out the buffer if more than half is occupied (C lines 480-482).
    state.gz_vacate();
    if state.err != Z_OK && !state.again {
        return state.err;
    }
    len as i32
}

/// Formatted write into the gzip stream. Port of C `gzprintf` (`gzwrite.c`
/// lines 487-498); a thin wrapper over [`gzvprintf`]. Callers pass
/// [`core::fmt::Arguments`] built with [`format_args!`].
pub(crate) fn gzprintf(state: &mut GzState, args: core::fmt::Arguments<'_>) -> i32 {
    gzvprintf(state, args)
}

/// Flush pending data with the requested `flush` mode. Port of C `gzflush`
/// (`gzwrite.c` lines 603-627).
///
/// `flush` is a raw C flush code in `0..=Z_FINISH`; out-of-range values yield
/// `Z_STREAM_ERROR`. Returns the recorded error code.
pub(crate) fn gzflush(state: &mut GzState, flush: i32) -> i32 {
    if state.mode != GzMode::Write || (state.err != Z_OK && !state.again) {
        return Z_STREAM_ERROR;
    }
    state.set_error(Z_OK, None);

    // Validate the flush parameter (C lines 615-616).
    if !(0..=Z_FINISH).contains(&flush) {
        return Z_STREAM_ERROR;
    }

    // Honor a pending forward seek (C lines 619-620).
    if state.skip != 0 && state.gz_zero() == -1 {
        return state.err;
    }

    // Compress remaining data with the requested flush (C lines 623-624). The
    // range check above guarantees the conversion succeeds.
    let f = Flush::try_from_i32(flush).unwrap_or(Flush::NoFlush);
    let _ = state.gz_comp(f);
    state.err
}

// ===========================================================================
// Unit tests
// ===========================================================================
//
// These are intentionally lightweight and self-contained: they use only `std`
// and the crate itself (the crate declares no dev-dependencies). Written gzip
// streams are validated structurally — the gzip magic plus the CRC-32 / ISIZE
// trailer, which the DEFLATE engine computes over exactly the bytes it
// processed. That confirms no input was dropped or duplicated and that the
// framing is correct, without needing a decompressor here. Full byte-exact and
// inflate round-trip coverage lives in the crate-level `tests/` suites.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A temp-file path that deletes itself on drop, keeping the test tree
    /// clean even if a test panics.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            p.push(format!("zlib_rs_gz_write_test_{tag}_{pid}_{n}.gz"));
            // Start from a clean slate.
            let _ = std::fs::remove_file(&p);
            TempFile(p)
        }

        fn path(&self) -> &PathBuf {
            &self.0
        }

        fn open_rw(&self) -> File {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.0)
                .expect("open temp file")
        }

        fn path_string(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// `gz_init` allocates the double-sized input buffer and a single-sized
    /// output buffer when compressing, and marks the state initialized.
    #[test]
    fn gz_init_buffer_sizes_compressed() {
        let tf = TempFile::new("init_comp");
        let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
        state.want = 64;
        assert_eq!(state.gz_init(), 0);
        assert_eq!(state.in_buf.len(), 128, "input buffer must be double-sized");
        assert_eq!(
            state.out_buf.len(),
            64,
            "output buffer must be single-sized"
        );
        assert_eq!(state.size, 64, "size marks the buffers as initialized");
    }

    /// A transparent (`direct`) stream needs neither an output buffer nor a
    /// deflate engine.
    #[test]
    fn gz_init_buffer_sizes_direct() {
        let tf = TempFile::new("init_direct");
        let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
        state.want = 64;
        state.direct = 1;
        assert_eq!(state.gz_init(), 0);
        assert_eq!(
            state.in_buf.len(),
            128,
            "input buffer is always double-sized"
        );
        assert!(
            state.out_buf.is_empty(),
            "no output buffer when transparent"
        );
        assert_eq!(state.size, 64);
    }

    /// `gzputc` buffers bytes in place and masks to the low 8 bits.
    #[test]
    fn gzputc_fast_path_buffers() {
        let tf = TempFile::new("putc");
        let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
        state.want = 32;

        // First putc initializes the buffers via gz_write, then buffers 'A'.
        assert_eq!(gzputc(&mut state, b'A' as i32), b'A' as i32);
        assert_eq!(state.have, 1);
        assert_eq!(state.pos, 1);
        assert_eq!(state.in_buf[0], b'A');

        // Subsequent putc take the fast in-buffer path.
        assert_eq!(gzputc(&mut state, b'B' as i32), b'B' as i32);
        assert_eq!(state.have, 2);
        assert_eq!(state.in_buf[1], b'B');

        // The high bits are masked off (0x141 & 0xff == 0x41 == 'A').
        assert_eq!(gzputc(&mut state, 0x141), 0x41);
        assert_eq!(state.have, 3);
        assert_eq!(state.in_buf[2], 0x41);
    }

    /// A transparent write passes the bytes through verbatim — no gzip framing,
    /// no compression.
    #[test]
    fn direct_write_is_byte_identical() {
        let tf = TempFile::new("direct_rt");
        let data: &[u8] = b"transparent bytes, not compressed\x00\x01\x02\xff";
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            state.direct = 1;
            assert_eq!(gzwrite(&mut state, data) as usize, data.len());
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_eq!(bytes, data, "transparent write must be byte-identical");
    }

    /// Computes the expected gzip trailer (CRC-32 little-endian + ISIZE
    /// little-endian) for `data`.
    fn gzip_trailer(data: &[u8]) -> [u8; 8] {
        let crc = crate::checksum::crc32::crc32(0, data);
        let isize = (data.len() as u32).to_le_bytes();
        let crc = crc.to_le_bytes();
        [
            crc[0], crc[1], crc[2], crc[3], isize[0], isize[1], isize[2], isize[3],
        ]
    }

    /// Asserts that `bytes` is a gzip stream whose trailer matches `data`.
    fn assert_gzip_of(bytes: &[u8], data: &[u8]) {
        assert!(
            bytes.len() >= 18,
            "gzip stream too short: {} bytes",
            bytes.len()
        );
        assert_eq!(
            &bytes[0..3],
            &[0x1f, 0x8b, 0x08],
            "gzip magic + deflate method"
        );
        let tail = &bytes[bytes.len() - 8..];
        assert_eq!(
            tail,
            gzip_trailer(data),
            "CRC-32 / ISIZE trailer must match input"
        );
    }

    /// A compressed write produces a valid gzip stream whose trailer encodes
    /// the original data's CRC-32 and length.
    #[test]
    fn compressed_write_has_valid_gzip_framing() {
        let tf = TempFile::new("comp_frame");
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(50);
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            assert_eq!(gzwrite(&mut state, &data) as usize, data.len());
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, &data);
    }

    /// An empty compressed stream (opened then finished with nothing written)
    /// is still a valid, complete gzip member.
    #[test]
    fn empty_compressed_write_is_valid_gzip() {
        let tf = TempFile::new("comp_empty");
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, b"");
    }

    /// `finish_write` is idempotent: a second call writes no second trailer.
    #[test]
    fn finish_write_is_idempotent() {
        let tf = TempFile::new("finish_idem");
        let data = b"some payload to finish";
        let first_len;
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            assert_eq!(gzwrite(&mut state, data) as usize, data.len());
            assert_eq!(state.finish_write(), Z_OK);
            first_len = std::fs::metadata(tf.path()).unwrap().len();
            // Second finish must be a no-op (no duplicate gzip trailer).
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_eq!(
            bytes.len() as u64,
            first_len,
            "a redundant finish_write must not append a second trailer"
        );
        assert_gzip_of(&bytes, data);
    }

    /// `gzflush` mid-stream writes data to the file without ending the stream;
    /// the final stream still carries the full data's trailer.
    #[test]
    fn gzflush_midstream_then_finish() {
        const Z_SYNC_FLUSH: i32 = 2;
        let tf = TempFile::new("flush");
        let part1 = b"first chunk of data ".repeat(20);
        let part2 = b"second chunk ".repeat(5);
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            assert_eq!(gzwrite(&mut state, &part1) as usize, part1.len());

            // A sync flush forces the header + buffered data out to the file.
            assert_eq!(gzflush(&mut state, Z_SYNC_FLUSH), Z_OK);
            let mid = std::fs::metadata(tf.path()).unwrap().len();
            assert!(mid > 0, "sync flush should have written bytes to the file");

            assert_eq!(gzwrite(&mut state, &part2) as usize, part2.len());
            assert_eq!(state.finish_write(), Z_OK);
        }
        let mut all = part1.clone();
        all.extend_from_slice(&part2);
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, &all);
    }

    /// An out-of-range flush argument is rejected with `Z_STREAM_ERROR`.
    #[test]
    fn gzflush_rejects_bad_flush() {
        let tf = TempFile::new("flush_bad");
        let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
        assert_eq!(gzflush(&mut state, -1), Z_STREAM_ERROR);
        assert_eq!(gzflush(&mut state, Z_FINISH + 1), Z_STREAM_ERROR);
    }

    /// `gzprintf` formats via `core::fmt` into the input buffer and the
    /// resulting stream's trailer reflects exactly the formatted bytes.
    #[test]
    fn gzprintf_formats_into_buffer() {
        let tf = TempFile::new("printf");
        let expected = "value=42 hex=ff pad=  7";
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            let n = gzprintf(
                &mut state,
                format_args!("value={} hex={:x} pad={:3}", 42, 255, 7),
            );
            assert_eq!(n, expected.len() as i32);
            // The formatted bytes are buffered, not yet flushed.
            assert_eq!(state.have, expected.len());
            assert_eq!(&state.in_buf[..expected.len()], expected.as_bytes());
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, expected.as_bytes());
    }

    /// `gzputs` writes the bytes of a slice and returns the count.
    #[test]
    fn gzputs_writes_all_bytes() {
        let tf = TempFile::new("puts");
        let s = b"a line of text";
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            assert_eq!(gzputs(&mut state, s), s.len() as i32);
            assert_eq!(state.finish_write(), Z_OK);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, s);
    }

    /// `gzfwrite` writes whole items and reports the item count; an
    /// overflowing request is rejected.
    #[test]
    fn gzfwrite_items_and_overflow() {
        let tf = TempFile::new("fwrite");
        let data = b"0123456789ABCDEF".repeat(4); // 64 bytes
        {
            let mut state = GzState::new(tf.open_rw(), tf.path_string(), GzMode::Write);
            // 16 items of 4 bytes each = 64 bytes.
            assert_eq!(gzfwrite(&data, 4, 16, &mut state), 16);
            assert_eq!(state.finish_write(), Z_OK);

            // An overflowing (size * nitems) request is rejected.
            assert_eq!(gzfwrite(&data, usize::MAX, 2, &mut state), 0);
            assert_eq!(state.err, Z_STREAM_ERROR);
        }
        let bytes = std::fs::read(tf.path()).expect("read back");
        assert_gzip_of(&bytes, &data);
    }
}
