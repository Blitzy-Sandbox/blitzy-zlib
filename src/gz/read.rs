//! gzip read path, ported from zlib `gzread.c` (Mark Adler).
//!
//! This module implements the **LOOK / COPY / GZIP substate machine** that
//! drives decompression of a gzip (or zlib / raw) stream read from the owned
//! [`std::fs::File`], together with the public read API (`gzread`, `gzgetc`,
//! `gzgets`, `gzungetc`, `gzdirect`, …). It auto-detects whether the file is
//! gzip-compressed — by sniffing the four-byte gzip magic header — and, if not,
//! reads the file **transparently** (the COPY path), delivering bytes verbatim.
//!
//! # Substate machine ([`How`])
//!
//! Each call that needs more output runs `gz_fetch`, which dispatches on the
//! read substate (C `state->how`):
//!
//! * [`How::Look`] — examine the next bytes (allocating buffers and the inflate
//!   engine on first entry). If they look like a gzip member, switch to
//!   [`How::Gzip`]; otherwise switch to [`How::Copy`] and deliver the
//!   already-buffered bytes verbatim.
//! * [`How::Copy`] — transparent passthrough: read straight from the file into
//!   the output buffer (or, for large requests, directly into the caller's
//!   buffer).
//! * [`How::Gzip`] — feed the inflate engine; on `Z_STREAM_END` return to
//!   [`How::Look`] so concatenated gzip members are decoded, and tolerate
//!   trailing garbage after a complete member (the `junk` flag).
//!
//! # Inflate configuration
//!
//! Decompression is performed by [`crate::inflate`] initialised with
//! `windowBits = MAX_WBITS + 16 = 31` — the value zlib's `gzread.c` passes to
//! `inflateInit2` (`15 + 16`) to select gzip decoding. The gzip CRC-32 and
//! ISIZE trailer are validated by the inflate engine itself; this module simply
//! propagates `Z_DATA_ERROR` (except in the trailing-garbage / junk case).
//!
//! # Engine bridge
//!
//! [`GzState`] owns its engine inside [`crate::stream::ZStream`] as a
//! `Box<dyn StreamState>`. The inflate engine, however, is driven through *free
//! functions* over the concrete [`InflateState`]. The recovery from the trait
//! object back to `&mut InflateState` goes through the additive
//! [`StreamState::as_any_mut`](crate::stream::StreamState::as_any_mut) seam plus
//! a checked [`Any::downcast_mut`](core::any::Any) — see `inflate_engine`. No
//! `unsafe` is involved anywhere in this module (AAP §0.6.2).
//!
//! # Cursors
//!
//! Following the [`GzState`] model, C's raw pointers are integer indices: input
//! lives in `in_buf[in_next .. in_next + in_avail]`, decompressed/copied output
//! in `out_buf[next .. next + have]`. The inflate engine receives `&[u8]` /
//! `&mut [u8]` slices carved from those buffers rather than stored pointers.

use std::fs::File;
use std::io::{self, BufRead, Read};

use crate::constants::{
    MAX_WBITS, Z_BUF_ERROR, Z_DATA_ERROR, Z_ERRNO, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK,
    Z_STREAM_END, Z_STREAM_ERROR,
};
use crate::gz::state::{GzState, How, alloc_zeroed};
use crate::inflate::{InflateState, inflate, inflate_init2, inflate_reset};
use crate::stream::ZStream;

// ===========================================================================
// Low-level file loading (port of `gz_load`, gzread.c L18-47).
// ===========================================================================

/// The terminal disposition of a [`gz_load_into`] read loop.
///
/// This separates the *pure I/O* (filling a byte buffer) from the *state
/// bookkeeping* (`eof` / `again` / error recording) so that the buffer being
/// filled — which may be a field of [`GzState`] (`in_buf` / `out_buf`) or an
/// external caller buffer — can be borrowed disjointly from the rest of the
/// state. The state mutation is applied afterwards by [`apply_load_outcome`].
enum LoadOutcome {
    /// The buffer was filled completely (`got == len`).
    Ok,
    /// End of file: a `read` returned `0` before the buffer was full. The
    /// caller sets [`GzState::eof`].
    Eof,
    /// A non-blocking descriptor stalled (`EAGAIN` / `EWOULDBLOCK`). The caller
    /// sets [`GzState::again`]; if no bytes were read it is also a hard error.
    Again,
    /// A genuine I/O error occurred (anything other than `EOF` / `WouldBlock` /
    /// retryable `Interrupted`). The caller records `Z_ERRNO`.
    Err(io::Error),
}

/// Fill `buf` from `file`, looping until full or a terminal condition — the
/// pure-I/O core of C `gz_load` (`gzread.c` L18-47).
///
/// `read()` is not guaranteed to return all requested bytes in one call (and
/// historically returned an `int`), so this loops, accumulating into `got`,
/// exactly like the C `do { … } while (*have < len)`. Returns the number of
/// bytes read together with the terminal [`LoadOutcome`]; it performs **no**
/// state mutation (see [`apply_load_outcome`]).
///
/// `Interrupted` (`EINTR`) is transparently retried — the idiomatic Rust I/O
/// convention — which is benign for the regular-file case that `gz*` targets.
fn gz_load_into(file: &mut File, buf: &mut [u8]) -> (usize, LoadOutcome) {
    let len = buf.len();
    let mut got = 0usize;
    loop {
        if got >= len {
            return (got, LoadOutcome::Ok);
        }
        match file.read(&mut buf[got..len]) {
            Ok(0) => return (got, LoadOutcome::Eof),
            Ok(n) => got += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                return (got, LoadOutcome::Again);
            }
            Err(e) => return (got, LoadOutcome::Err(e)),
        }
    }
}

/// Apply a [`gz_load_into`] outcome to the stream — the state-mutating tail of
/// C `gz_load` (`gzread.c` L35-46).
///
/// Returns `0` on success (the caller continues) or `-1` if a fatal error was
/// recorded (the caller propagates `-1`). The `again` flag is cleared first
/// (C sets `state->again = 0` at the top of `gz_load`) and re-raised only on a
/// genuine non-blocking stall. A non-blocking stall that nonetheless read some
/// bytes is *not* fatal (matches C `if (*have != 0) return 0;`).
fn apply_load_outcome(state: &mut GzState, got: usize, outcome: LoadOutcome) -> i32 {
    // C `gz_load`: `state->again = 0;` before the read loop.
    state.again = false;
    match outcome {
        LoadOutcome::Ok => 0,
        LoadOutcome::Eof => {
            // C: `if (ret == 0) state->eof = 1;`
            state.eof = true;
            0
        }
        LoadOutcome::Again => {
            // C: `errno == EAGAIN || EWOULDBLOCK` -> `state->again = 1;`
            state.again = true;
            if got != 0 {
                // Some data was read despite the stall -> not an error.
                0
            } else {
                state.gz_error(Z_ERRNO, Some("resource temporarily unavailable"));
                -1
            }
        }
        LoadOutcome::Err(e) => {
            // C: `gz_error(state, Z_ERRNO, zstrerror());`
            let msg = e.to_string();
            state.gz_error(Z_ERRNO, Some(&msg));
            -1
        }
    }
}

// ===========================================================================
// Engine bridge: recover `&mut InflateState` from the boxed `dyn StreamState`.
// ===========================================================================

/// Recover the concrete inflate engine stored inside `strm`.
///
/// [`GzState`] keeps the engine in [`ZStream`] as a `Box<dyn StreamState>`, but
/// the inflate free functions ([`inflate`], [`inflate_reset`]) need a
/// `&mut InflateState`. The recovery chains the [`ZStream::state_mut`] borrow
/// through the additive
/// [`StreamState::as_any_mut`](crate::stream::StreamState::as_any_mut) upcast
/// and a checked [`Any::downcast_mut`](core::any::Any). Returns `None` only if
/// no engine is installed or it is not an [`InflateState`] (an internal
/// invariant violation, never expected once [`How::Gzip`] is reached).
///
/// Taking `&mut ZStream` (rather than `&mut GzState`) keeps the borrow confined
/// to the `strm` field, so callers may simultaneously borrow the disjoint
/// `in_buf` / `out_buf` fields for the slice arguments to [`inflate`].
#[inline]
fn inflate_engine(strm: &mut ZStream) -> Option<&mut InflateState> {
    strm.state_mut()?
        .as_any_mut()?
        .downcast_mut::<InflateState>()
}

/// Reset the installed inflate engine in place — the equivalent of C
/// `inflateReset(strm)` as used by [`gz_look`].
///
/// The engine-internal reset is performed via [`inflate_reset`], which writes
/// the freshly-seeded public check value through its `&mut u32` parameter; a
/// local is used so the engine borrow (through `strm`) does not also need to
/// borrow `strm.adler`. The stream-level accounting (`total_in`/`total_out`/
/// `msg`) that C's `inflateReset` clears is reset here too, since those fields
/// live on [`ZStream`] rather than on [`InflateState`].
fn reset_inflate_engine(state: &mut GzState) {
    let mut adler = state.strm.adler;
    if let Some(engine) = inflate_engine(&mut state.strm) {
        inflate_reset(engine, &mut adler);
    }
    state.strm.adler = adler;
    // C `inflateResetKeep`: `strm->total_in = strm->total_out = 0; msg = NULL;`
    state.strm.total_in = 0;
    state.strm.total_out = 0;
    state.strm.msg = None;
}

// ===========================================================================
// gz_avail — ensure input is available for inflate (port of `gz_avail`,
// gzread.c L56-82).
// ===========================================================================

/// Load up the input buffer, setting [`GzState::eof`] when the file is
/// exhausted — port of C `gz_avail` (`gzread.c` L56-82).
///
/// Any unconsumed input is first moved to the front of `in_buf`
/// (`memmove` -> [`slice::copy_within`]); the remainder of the buffer is then
/// topped up from the file. Returns `0` on success or `-1` if a prior fatal
/// error is latched or the read fails.
fn gz_avail(state: &mut GzState) -> i32 {
    // C: a latched fatal error (anything but Z_OK / Z_BUF_ERROR) is sticky.
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }
    if !state.eof {
        // Move whatever is left to the start of the input buffer so the load
        // can append behind it (C `memmove(state->in, strm->next_in, avail)`).
        if state.in_avail != 0 && state.in_next != 0 {
            let from = state.in_next;
            let n = state.in_avail;
            state.in_buf.copy_within(from..from + n, 0);
        }
        state.in_next = 0;

        // Load the rest of the buffer (C `gz_load(state, in + avail, size -
        // avail, &got)`). `in_buf` is sized to `size` (== `want`) for reading.
        let start = state.in_avail;
        let size = state.size;
        let (got, outcome) = {
            let Some(file) = state.file.as_mut() else {
                state.gz_error(Z_ERRNO, Some("file is closed"));
                return -1;
            };
            gz_load_into(file, &mut state.in_buf[start..size])
        };
        // C `gz_avail` returns -1 (without updating avail_in) if gz_load failed.
        if apply_load_outcome(state, got, outcome) == -1 {
            return -1;
        }
        // C: `strm->avail_in += got; strm->next_in = state->in;`
        state.in_avail += got;
        state.in_next = 0;
    }
    0
}

// ===========================================================================
// gz_look — first-time setup + gzip auto-detection (port of `gz_look`,
// gzread.c L93-170). This is the heart of the transparent-read machine.
// ===========================================================================

/// Look for a gzip header, setting up for decompression or transparent copy —
/// port of C `gz_look` (`gzread.c` L93-170).
///
/// On first entry (`size == 0`) the input/output buffers and the inflate engine
/// are allocated: `in_buf` is sized `want`, `out_buf` is sized `want << 1`
/// (doubled — this guarantees room for the transparent-copy carry-over and for
/// [`gzungetc`]), and the engine is initialised with `windowBits = 31` (gzip).
///
/// Then, mirroring C exactly:
///
/// 1. If transparent reading is disabled (`direct == -1`) or we are looking for
///    a member *after* the first (`junk == 0`), go straight to [`How::Gzip`].
/// 2. Otherwise ensure at least four bytes are buffered and test the gzip magic
///    `1f 8b 08` with flags `< 32`. A match selects [`How::Gzip`].
/// 3. A non-match is a transparent read: the already-buffered bytes are copied
///    verbatim into `out_buf` and [`How::Copy`] is selected.
///
/// `state->x.have` must be `0` on entry. Returns `0` on success, `-1` on
/// allocation/engine-init or read failure.
fn gz_look(state: &mut GzState) -> i32 {
    // --- first-time allocation of buffers and the inflate engine ----------
    if state.size == 0 {
        // C: `state->in = malloc(want); state->out = malloc(want << 1);
        //     if (state->in == NULL || state->out == NULL) { free(state->out);
        //     free(state->in); gz_error(state, Z_MEM_ERROR, "out of memory");
        //     return -1; }`
        //
        // Use the fallible `alloc_zeroed` (try_reserve_exact) rather than the
        // infallible `vec![0u8; ...]`, which would abort the whole process on
        // OOM. On failure either allocation rolls both buffers back to empty,
        // resets `size = 0`, records `Z_MEM_ERROR`, and returns `-1` — exactly
        // the C checked-`malloc` contract (matching the write path's
        // `gz_init`). Both buffers are allocated up front and only committed to
        // `state` once both have succeeded, so a partial allocation never
        // leaves the handle in a half-initialised state.
        let (Some(in_buf), Some(out_buf)) =
            (alloc_zeroed(state.want), alloc_zeroed(state.want << 1))
        else {
            // Roll back any buffer that did allocate (C frees both), keep
            // `size == 0` so a later call retries, and surface out of memory.
            state.in_buf = Vec::new();
            state.out_buf = Vec::new();
            state.size = 0;
            state.gz_error(Z_MEM_ERROR, Some("out of memory"));
            return -1;
        };
        state.in_buf = in_buf;
        state.out_buf = out_buf;
        state.size = state.want;

        // C: `inflateInit2(&strm, 15 + 16)` -> gunzip (windowBits = 31).
        match inflate_init2(MAX_WBITS + 16) {
            Ok(engine) => state.strm.set_state(engine),
            Err(_) => {
                // Roll back the buffers and report out of memory (C frees
                // `out`/`in`, resets `size = 0`, then `gz_error(Z_MEM_ERROR)`).
                state.in_buf = Vec::new();
                state.out_buf = Vec::new();
                state.size = 0;
                state.gz_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }
        // C: `strm.avail_in = 0; strm.next_in = Z_NULL;`
        state.in_avail = 0;
        state.in_next = 0;
        // Seed the stream-level accounting for the fresh engine.
        state.strm.total_in = 0;
        state.strm.total_out = 0;
        state.strm.msg = None;
    }

    // --- (1) gzip-only, or scanning for a subsequent concatenated member ---
    // C: `if (state->direct == -1 || state->junk == 0)`.
    if state.direct == -1 || state.junk == 0 {
        reset_inflate_engine(state);
        state.how = How::Gzip;
        // C: `state->junk = state->junk != -1;`
        state.junk = i32::from(state.junk != -1);
        state.direct = 0;
        return 0;
    }

    // --- (2) auto-detect: make sure at least four header bytes are present -
    if gz_avail(state) == -1 {
        return -1;
    }
    // C: empty input is a transparent read of zero bytes; a non-blocking stall
    // before four bytes simply waits for a later call.
    if state.in_avail == 0 || (state.again && state.in_avail < 4) {
        return 0;
    }

    // --- gzip magic sniff (four bytes: 1f 8b 08, flags < 32) --------------
    // C: `next_in[0]==31 && [1]==139 && [2]==8 && [3]<32`.
    let p = state.in_next;
    if state.in_avail > 3
        && state.in_buf[p] == 31
        && state.in_buf[p + 1] == 139
        && state.in_buf[p + 2] == 8
        && state.in_buf[p + 3] < 32
    {
        reset_inflate_engine(state);
        state.how = How::Gzip;
        state.junk = 1;
        state.direct = 0;
        return 0;
    }

    // --- (3) transparent copy: deliver the buffered bytes verbatim --------
    // C: `memcpy(state->out, strm->next_in, avail); x.have = avail; avail = 0;`
    let n = state.in_avail;
    let src = state.in_next;
    // `out_buf` (>= 2*want) is guaranteed larger than `in_buf` (== want), so the
    // already-loaded input always fits, leaving room for a later `gzungetc`.
    state.out_buf[..n].copy_from_slice(&state.in_buf[src..src + n]);
    state.next = 0;
    state.have = n;
    state.in_avail = 0;
    state.how = How::Copy;
    0
}

// ===========================================================================
// gz_decomp — decompress into a target buffer (port of `gz_decomp`,
// gzread.c L180-240).
// ===========================================================================

/// Decompress as much as possible into `out`, updating [`GzState::have`] /
/// [`GzState::next`] — port of C `gz_decomp` (`gzread.c` L180-240).
///
/// `out` is the inflate destination (the internal `out_buf` for [`gz_fetch`],
/// or the caller's buffer for the large-read fast path). The inflate engine is
/// fed slices of `in_buf`, topping up via [`gz_avail`] when input runs out. On
/// `Z_STREAM_END` the substate is reset to [`How::Look`] so concatenated gzip
/// members are decoded; trailing garbage after a complete member (`junk == 1`)
/// is tolerated and treated as end-of-data. Returns `0` on success, `-1` on a
/// fatal decompression/IO error (with the error latched on the stream).
fn gz_decomp_into(state: &mut GzState, out: &mut [u8]) -> i32 {
    // C: `had = strm->avail_out;` — the room available at entry.
    let had = out.len();
    let mut put = 0usize; // bytes produced so far (output cursor within `out`)
    let mut ret = Z_OK;

    loop {
        // --- get more input for inflate() ---------------------------------
        // C: `if (avail_in == 0 && gz_avail() == -1) { ret = err; break; }`
        if state.in_avail == 0 {
            if gz_avail(state) == -1 {
                ret = state.err;
                break;
            }
            // C: `if (avail_in == 0) { if (!again) gz_error(Z_BUF_ERROR, ...); break; }`
            if state.in_avail == 0 {
                if !state.again {
                    state.gz_error(Z_BUF_ERROR, Some("unexpected end of file"));
                }
                break;
            }
        }

        // --- decompress (one inflate() call) ------------------------------
        let next = state.in_next;
        let avail = state.in_avail;
        let outcome = match inflate_engine(&mut state.strm) {
            Some(engine) => inflate(
                engine,
                &state.in_buf[next..next + avail],
                &mut out[put..],
                Z_NO_FLUSH,
            ),
            None => {
                // No inflate engine installed — an internal invariant breach.
                state.gz_error(
                    Z_STREAM_ERROR,
                    Some("internal error: inflate stream corrupt"),
                );
                ret = Z_STREAM_ERROR;
                break;
            }
        };

        // Advance the input cursors and mirror the engine accounting onto the
        // stream (as the idiomatic `Inflate` wrapper does).
        state.in_next += outcome.in_consumed;
        state.in_avail -= outcome.in_consumed;
        put += outcome.out_produced;
        state.strm.total_in += outcome.total_in_delta;
        state.strm.total_out += outcome.total_out_delta;
        state.strm.adler = outcome.adler;
        state.strm.data_type = outcome.data_type;
        state.strm.msg = outcome.msg;
        ret = outcome.ret;

        // C: `if (avail_out < had) state->junk = 0;` — any output produced
        // marks this as a real (non-garbage) gzip stream.
        if put > 0 {
            state.junk = 0;
        }

        // --- handle inflate() return codes exactly as C ------------------
        if ret == Z_STREAM_ERROR || ret == Z_NEED_DICT {
            state.gz_error(
                Z_STREAM_ERROR,
                Some("internal error: inflate stream corrupt"),
            );
            break;
        }
        if ret == Z_MEM_ERROR {
            state.gz_error(Z_MEM_ERROR, Some("out of memory"));
            break;
        }
        if ret == Z_DATA_ERROR {
            if state.junk == 1 {
                // Trailing garbage after a completed member is acceptable: stop
                // cleanly (C: `avail_in = 0; eof = 1; how = LOOK; ret = Z_OK;`).
                state.in_avail = 0;
                state.eof = true;
                state.how = How::Look;
                ret = Z_OK;
                break;
            }
            let msg = state.strm.msg.unwrap_or("compressed data error");
            state.gz_error(Z_DATA_ERROR, Some(msg));
            break;
        }

        // C `do { … } while (strm->avail_out && ret != Z_STREAM_END);`
        // avail_out == had - put, so continue while there is room and the
        // member has not completed.
        if put >= had || ret == Z_STREAM_END {
            break;
        }
    }

    // C: `state->x.have = had - avail_out;` (== put) and `x.next` points at the
    // start of the freshly produced data (index 0 of the target buffer).
    state.have = put;
    state.next = 0;

    // C: a completed gzip member -> look for the next one once `have` drains.
    if ret == Z_STREAM_END {
        state.junk = 0;
        state.how = How::Look;
        return 0;
    }

    // C: `return ret != Z_OK ? -1 : 0;`
    if ret != Z_OK { -1 } else { 0 }
}

// ===========================================================================
// gz_fetch — make some output available (port of `gz_fetch`, gzread.c L248-277).
// ===========================================================================

/// Make output available in `out_buf`, dispatching on the read substate — port
/// of C `gz_fetch` (`gzread.c` L248-277). Assumes [`GzState::have`] is `0`.
///
/// Loops until output is produced or the input is fully consumed at EOF.
/// Returns `0` on success, `-1` on error.
pub(crate) fn gz_fetch(state: &mut GzState) -> i32 {
    // C: `do { switch (how) { … } } while (x.have == 0 && (!eof || avail_in));`
    loop {
        match state.how {
            How::Look => {
                // -> LOOK (more input needed), COPY, or GZIP.
                if gz_look(state) == -1 {
                    return -1;
                }
                if state.how == How::Look {
                    return 0;
                }
            }
            How::Copy => {
                // Transparent read straight into the (doubled) output buffer.
                // C: `gz_load(state, state->out, size << 1, &x.have); x.next = out;`
                state.again = false;
                let cap = state.out_buf.len(); // == size << 1
                let (got, outcome) = {
                    let Some(file) = state.file.as_mut() else {
                        state.gz_error(Z_ERRNO, Some("file is closed"));
                        return -1;
                    };
                    gz_load_into(file, &mut state.out_buf[..cap])
                };
                // `x.have` is set to the read count even on a fatal error (C
                // `gz_load` writes `*have` before `gz_error` may zero it).
                state.have = got;
                state.next = 0;
                if apply_load_outcome(state, got, outcome) == -1 {
                    return -1;
                }
                return 0;
            }
            How::Gzip => {
                // Decompress into the full (doubled) output buffer. `out_buf` is
                // moved out so it can be borrowed disjointly from `state`.
                let mut out = core::mem::take(&mut state.out_buf);
                let rc = gz_decomp_into(state, &mut out);
                state.out_buf = out;
                if rc == -1 {
                    return -1;
                }
            }
        }
        // do-while tail.
        if !(state.have == 0 && (!state.eof || state.in_avail != 0)) {
            return 0;
        }
    }
}

// ===========================================================================
// gz_skip — discard uncompressed output bytes (port of `gz_skip`,
// gzread.c L281-309).
// ===========================================================================

/// Skip [`GzState::skip`] uncompressed bytes of output — port of C `gz_skip`
/// (`gzread.c` L281-309). Used by the forward-seek path. Returns `0` on
/// success, `-1` on error.
pub(crate) fn gz_skip(state: &mut GzState) -> i32 {
    loop {
        if state.have != 0 {
            // Discard whatever is already buffered, up to the remaining skip.
            let n = if (state.have as i64) > state.skip {
                state.skip as usize
            } else {
                state.have
            };
            state.have -= n;
            state.next += n;
            state.pos += n as i64;
            state.skip -= n as i64;
        } else if state.eof && state.in_avail == 0 {
            // Output empty and input exhausted -> nothing more to skip.
            break;
        } else {
            // Need more output to skip over: refill.
            if gz_fetch(state) == -1 {
                return -1;
            }
        }
        if state.skip == 0 {
            break;
        }
    }
    0
}

// ===========================================================================
// gz_read — internal read into a caller buffer (port of `gz_read`,
// gzread.c L317-393).
// ===========================================================================

/// Read up to `buf.len()` bytes into `buf`, returning the number actually read
/// — port of C `gz_read` (`gzread.c` L317-393).
///
/// A return of `0` means either end-of-file or an error; the caller inspects
/// [`GzState::err`] to distinguish. If some bytes were delivered before an
/// error, that partial count is returned and the error is deferred to the next
/// call (C's "deferred error" behaviour).
///
/// Three delivery strategies, exactly as C:
///
/// * copy from the buffered output (`have`) — the fast path that keeps
///   [`gzgetc`] cheap and always leaves room for a [`gzungetc`];
/// * for a *large* request whose size reaches `out_buf`'s capacity, read/inflate
///   **directly into the caller's buffer**, skipping the intermediate copy;
/// * otherwise refill the internal buffer via [`gz_fetch`].
///
/// Exposed `pub(crate)` so the seek helpers in `open.rs` can reuse it.
pub(crate) fn gz_read(state: &mut GzState, buf: &mut [u8]) -> usize {
    let len = buf.len();
    // C: `if (len == 0) return 0;`
    if len == 0 {
        return 0;
    }

    // C: `if (state->skip && gz_skip(state) == -1) return 0;`
    if state.skip != 0 && gz_skip(state) == -1 {
        return 0;
    }

    let mut got = 0usize; // total delivered
    let mut off = 0usize; // cursor within `buf` (C advances `buf`/decrements `len`)
    let mut err = false;

    loop {
        // C: `n = (unsigned)-1; if (n > len) n = len;` — here `len` is what is
        // still requested, i.e. `len - off`.
        let mut n = len - off;

        if state.have != 0 {
            // --- fast path: copy from the buffered output -----------------
            if state.have < n {
                n = state.have;
            }
            buf[off..off + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.next += n;
            state.have -= n;
            // C: a deferred error from a prior gz_fetch surfaces here.
            if state.err != Z_OK {
                err = true;
            }
        } else if state.eof && state.in_avail == 0 {
            // Output drained and input exhausted -> end of data.
            break;
        } else if state.how == How::Look || n < (state.size << 1) {
            // --- small request / new stream: refill the internal buffer ---
            if gz_fetch(state) == -1 && state.have == 0 {
                // If have != 0 the error will be caught after the copy above.
                err = true;
            }
            // C `continue;` -> re-evaluate the `while (len && !err)` tail.
            if err {
                break;
            }
            continue;
        } else if state.how == How::Copy {
            // --- large request, transparent: read straight into `buf` -----
            state.again = false;
            let load = state
                .file
                .as_mut()
                .map(|file| gz_load_into(file, &mut buf[off..off + n]));
            match load {
                Some((got2, outcome)) => {
                    n = got2;
                    if apply_load_outcome(state, got2, outcome) == -1 {
                        err = true;
                    }
                }
                None => {
                    state.gz_error(Z_ERRNO, Some("file is closed"));
                    n = 0;
                    err = true;
                }
            }
        } else {
            // --- large request, gzip: decompress straight into `buf` ------
            // C: `avail_out = n; next_out = buf; gz_decomp(); n = x.have; x.have = 0;`
            let rc = gz_decomp_into(state, &mut buf[off..off + n]);
            n = state.have;
            state.have = 0;
            if rc == -1 {
                err = true;
            }
        }

        // C: `len -= n; buf += n; got += n; state->x.pos += n;`
        off += n;
        got += n;
        state.pos += n as i64;

        // C `while (len && !err)` -> while there is more to deliver and no error.
        if off >= len || err {
            break;
        }
    }

    // C: `if (len && state->eof) state->past = 1;` — short read at EOF.
    if off < len && state.eof {
        state.past = true;
    }

    got
}

// ===========================================================================
// Public read API — faithful zlib `gz*` names (gzread.c L396-642).
// ===========================================================================

/// Read up to `buf.len()` bytes — port of C `gzread` (`gzread.c` L396-436).
///
/// Returns the number of uncompressed bytes read (`0` at EOF), or `-1` on
/// error. The C `gzFile` null check and the raw-pointer/length marshalling live
/// in `src/ffi.rs`; here `state` is a validated borrow.
pub fn gzread(state: &mut GzState, buf: &mut [u8]) -> i32 {
    // C: `if (state->mode != GZ_READ) return -1;`
    if !state.is_reading() {
        return -1;
    }
    // C: serious-error guard (a non-blocking `again` is not fatal).
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // C: `if ((int)len < 0)` — the count must fit in a C `int`.
    if buf.len() > i32::MAX as usize {
        state.gz_error(Z_STREAM_ERROR, Some("request does not fit in an int"));
        return -1;
    }

    let n = gz_read(state, buf);

    // C: distinguish EOF from a latched / non-blocking error on a zero return.
    if n == 0 {
        if state.err != Z_OK && state.err != Z_BUF_ERROR {
            return -1;
        }
        if state.again {
            state.gz_error(Z_ERRNO, Some("resource temporarily unavailable"));
            return -1;
        }
    }

    n as i32
}

/// `fread`-style read of `nitems` items of `size` bytes — port of C `gzfread`
/// (`gzread.c` L439-465). Returns the number of **full items** read.
///
/// The product `size * nitems` is range-checked (C errors with
/// `Z_STREAM_ERROR` on overflow). At most `buf.len()` bytes are read, so the
/// caller's slice bounds the request.
pub fn gzfread(state: &mut GzState, size: usize, nitems: usize, buf: &mut [u8]) -> usize {
    if !state.is_reading() {
        return 0;
    }
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return 0;
    }
    state.clear_error();

    // C: `len = nitems * size; if (size && len / size != nitems) error;`
    let len = match nitems.checked_mul(size) {
        Some(l) => l,
        None => {
            state.gz_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
            return 0;
        }
    };
    if len == 0 {
        return 0;
    }

    // The caller buffer bounds the actual read (idiomatic slice safety).
    let to_read = len.min(buf.len());
    let got = gz_read(state, &mut buf[..to_read]);

    // C: `return len ? gz_read(...) / size : 0;` — number of complete items.
    got.checked_div(size).unwrap_or(0)
}

/// Read one byte — port of C `gzgetc` (`gzread.c` L473-498).
///
/// Returns the byte (`0..=255`) or `-1` at EOF / on error. Takes the fast path
/// straight off the output buffer when possible, otherwise falls back to a
/// one-byte `gz_read`.
pub fn gzgetc(state: &mut GzState) -> i32 {
    if !state.is_reading() {
        return -1;
    }
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // C fast path: `if (x.have) { x.have--; x.pos++; return *x.next++; }`
    if state.have != 0 {
        let b = state.out_buf[state.next];
        state.next += 1;
        state.have -= 1;
        state.pos += 1;
        return i32::from(b);
    }

    // C slow path: `return gz_read(state, buf, 1) < 1 ? -1 : buf[0];`
    let mut one = [0u8; 1];
    if gz_read(state, &mut one) < 1 {
        -1
    } else {
        i32::from(one[0])
    }
}

/// Function form of [`gzgetc`] — port of C `gzgetc_` (`gzread.c` L500-502).
///
/// In C this is the non-macro entry point (the `gzgetc` macro's fast path lives
/// in the header / FFI layer); here it simply delegates.
#[inline]
pub fn gzgetc_(state: &mut GzState) -> i32 {
    gzgetc(state)
}

/// Push one byte back into the stream — port of C `gzungetc` (`gzread.c`
/// L505-563).
///
/// At most as many bytes as the output buffer holds may be pushed back. When
/// the buffer is empty the byte is placed at its very end (so several pushes can
/// follow); otherwise existing data is slid toward the end if needed and the
/// byte is inserted immediately before it. Returns the pushed byte, or `-1` if
/// `c < 0`, the stream is not readable, or there is no room.
pub fn gzungetc(c: i32, state: &mut GzState) -> i32 {
    if !state.is_reading() {
        return -1;
    }

    // C: if just opened, prime the input buffer so `out`/`size` are set up.
    if state.how == How::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // C: process a pending skip first.
    if state.skip != 0 && gz_skip(state) == -1 {
        return -1;
    }

    // C: `if (c < 0) return -1;` — can't push EOF.
    if c < 0 {
        return -1;
    }

    let cap = state.size << 1; // out_buf capacity

    // C: empty buffer -> place the byte at the very end.
    if state.have == 0 {
        state.have = 1;
        state.next = cap - 1;
        state.out_buf[state.next] = c as u8;
        state.pos -= 1;
        state.past = false;
        return c;
    }

    // C: `if (x.have == (size << 1))` -> no room left.
    if state.have == cap {
        state.gz_error(Z_DATA_ERROR, Some("out of room to push characters"));
        return -1;
    }

    // C: if data sits at the front, slide it to the end to make room.
    if state.next == 0 {
        let have = state.have;
        state.out_buf.copy_within(0..have, cap - have);
        state.next = cap - have;
    }

    // C: `x.have++; x.next--; x.next[0] = c; x.pos--; past = 0;`
    state.have += 1;
    state.next -= 1;
    state.out_buf[state.next] = c as u8;
    state.pos -= 1;
    state.past = false;
    c
}

/// Read a line into `buf` (up to and including a newline) — safe-core port of C
/// `gzgets` (`gzread.c` L566-624). Returns the number of bytes written.
///
/// Reading stops after a `\n`, when `buf` is full, or at EOF — whichever comes
/// first. Unlike C, this safe core does **not** NUL-terminate or return a
/// `char*`; the FFI shim reserves the final byte for the terminator and turns
/// the count into the C `char*` contract. A return of `0` means nothing was
/// read (EOF on entry).
pub fn gzgets(state: &mut GzState, buf: &mut [u8]) -> usize {
    if buf.is_empty() || !state.is_reading() {
        return 0;
    }
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return 0;
    }
    state.clear_error();

    if state.skip != 0 && gz_skip(state) == -1 {
        return 0;
    }

    let total = buf.len();
    let mut written = 0usize;
    let mut found_eol = false;

    // C `do { … } while (left && eol == NULL);`
    while written < total && !found_eol {
        // C: ensure something is in the output buffer.
        if state.have == 0 {
            if gz_fetch(state) == -1 {
                break; // error (deferred to a subsequent call)
            }
            if state.have == 0 {
                state.past = true; // read past end
                break;
            }
        }

        // C: `n = x.have > left ? left : x.have;` then search for '\n'.
        let left = total - written;
        let mut n = if state.have > left { left } else { state.have };
        let start = state.next;
        if let Some(pos) = state.out_buf[start..start + n]
            .iter()
            .position(|&b| b == b'\n')
        {
            n = pos + 1;
            found_eol = true;
        }

        // C: copy through the newline (or the remainder if none found).
        buf[written..written + n].copy_from_slice(&state.out_buf[start..start + n]);
        state.have -= n;
        state.next += n;
        state.pos += n as i64;
        written += n;
    }

    written
}

/// Report whether the stream is being read transparently — port of C
/// `gzdirect` (`gzread.c` L627-642).
///
/// Returns `1` for a transparent (non-gzip) read, `0` when decompressing. If
/// the substate is still [`How::Look`] (nothing read yet), a `gz_look` is
/// forced first so the answer is meaningful right after `gzopen`.
pub fn gzdirect(state: &mut GzState) -> i32 {
    // C: `if (mode == GZ_READ && how == LOOK && x.have == 0) gz_look(state);`
    if state.is_reading() && state.how == How::Look && state.have == 0 {
        let _ = gz_look(state);
    }
    i32::from(state.direct == 1)
}

/// Close the read side: tear down the inflate engine and release buffers — port
/// of C `gzclose_r` (`gzread.c` L645-668).
///
/// The engine `Box<InflateState>` is dropped via [`ZStream::end`] (the RAII
/// replacement for `inflateEnd`), the owned [`File`] is closed by dropping it,
/// and the handle is marked closed so a later [`Drop`] does not double-finalize.
/// Returns `Z_BUF_ERROR` if that was the latched state, else `Z_OK` (or
/// `Z_STREAM_ERROR` if the handle is not a read stream).
///
/// Exposed `pub(crate)` so the `close.rs` dispatcher can route to it.
pub(crate) fn gzclose_r(state: &mut GzState) -> i32 {
    if !state.is_reading() {
        return Z_STREAM_ERROR;
    }

    // C: `if (state->size) { inflateEnd(&strm); free(out); free(in); }`
    if state.size != 0 {
        state.strm.end(); // drops Box<InflateState> == inflateEnd (RAII)
        state.in_buf = Vec::new();
        state.out_buf = Vec::new();
        state.size = 0;
    }

    // C: `err = state->err == Z_BUF_ERROR ? Z_BUF_ERROR : Z_OK;`
    let err = if state.err == Z_BUF_ERROR {
        Z_BUF_ERROR
    } else {
        Z_OK
    };
    state.clear_error();

    // C: `ret = close(state->fd);` — dropping the File closes the descriptor.
    // std offers no close()-error code for a read handle, so a successful drop
    // maps to Z_OK; the latched `err` is returned otherwise.
    let _ = state.file.take();

    // Mark inert so the eventual Drop neither re-finalizes nor errors.
    state.mark_closed();

    err
}

// ===========================================================================
// GzReader — idiomatic `Read` / `BufRead` adapter (AAP §0.3.2).
// ===========================================================================

/// An idiomatic [`Read`] / [`BufRead`] adapter over a borrowed [`GzState`].
///
/// This realises the "`impl Read`/`BufRead` for the gzip layer" goal (AAP
/// §0.3.2): it lets a gzip stream be consumed with the full standard-library
/// reader ecosystem (`read_to_end`, `BufReader`-style line iteration, `io::copy`,
/// …) while delegating all decoding to the faithful `gz_*` port above.
///
/// It borrows the state mutably for its lifetime; construct one with
/// [`GzReader::new`] from a `&mut GzState` obtained from the `gzopen` path.
pub struct GzReader<'a> {
    state: &'a mut GzState,
}

impl<'a> GzReader<'a> {
    /// Wrap a mutable [`GzState`] as a [`Read`]/[`BufRead`] adapter.
    #[inline]
    #[must_use]
    pub fn new(state: &'a mut GzState) -> Self {
        GzReader { state }
    }

    /// Borrow the underlying [`GzState`].
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &GzState {
        self.state
    }

    /// Mutably borrow the underlying [`GzState`].
    #[inline]
    pub fn get_mut(&mut self) -> &mut GzState {
        self.state
    }
}

impl Read for GzReader<'_> {
    /// Decompress/copy up to `buf.len()` bytes into `buf`.
    ///
    /// Maps the `gz_read` count onto [`io::Result`]: a latched fatal error
    /// (anything but `Z_OK` / `Z_BUF_ERROR`) on a zero-byte read becomes an
    /// [`io::Error`]; otherwise a short or zero read is reported as `Ok` (EOF
    /// is `Ok(0)`), matching the [`Read`] contract.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = gz_read(self.state, buf);
        if n == 0 && self.state.err != Z_OK && self.state.err != Z_BUF_ERROR {
            return Err(io::Error::other(self.state.error_message().to_string()));
        }
        Ok(n)
    }
}

impl BufRead for GzReader<'_> {
    /// Return the buffered output, refilling it via `gz_fetch` when empty.
    ///
    /// The returned slice is exactly the available window
    /// `out_buf[next .. next + have]`; an empty slice signals EOF.
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.state.have == 0 && gz_fetch(self.state) == -1 {
            return Err(io::Error::other(self.state.error_message().to_string()));
        }
        Ok(self.state.out_slice())
    }

    /// Consume `amt` bytes previously returned by [`fill_buf`](Self::fill_buf).
    ///
    /// Advances the output cursor and the uncompressed position, clamped to the
    /// bytes actually available (per the [`BufRead`] contract `amt` must not
    /// exceed the last `fill_buf` length).
    fn consume(&mut self, amt: usize) {
        let n = amt.min(self.state.have);
        self.state.next += n;
        self.state.have -= n;
        self.state.pos += n as i64;
    }
}

// ===========================================================================
// Tests — exercise the LOOK/COPY/GZIP machine end-to-end.
//
// These build real gzip members with the `flate2` dev-dependency (its `zlib`
// backend links canonical C zlib, so this is a true cross-implementation
// oracle) and drive the read path over a temporary on-disk `std::fs::File`,
// validating transparent copy, gzip auto-detection, concatenated members, the
// gzgetc/gzungetc round-trip, gzgets line scanning, and large multi-fetch
// reads. They require the `gz-io` feature (which pulls in `std`).
// ===========================================================================
#[cfg(all(test, feature = "gz-io"))]
mod tests {
    use super::*;
    use crate::gz::state::{GzState, Mode};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic counter so concurrently-run tests never collide on a path.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A temporary file that deletes itself on drop.
    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        /// Create a uniquely-named temp file containing `data`.
        fn with_contents(tag: &str, data: &[u8]) -> TempFile {
            let mut path = std::env::temp_dir();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            path.push(format!(
                "zlibrs_gzread_{}_{}_{}.bin",
                tag,
                std::process::id(),
                seq
            ));
            {
                let mut f = File::create(&path).expect("create temp file");
                f.write_all(data).expect("write temp file");
                f.sync_all().expect("sync temp file");
            }
            TempFile { path }
        }

        /// Open the file read-only and wrap it in a fresh read-mode `GzState`.
        ///
        /// `GzState::new` leaves `direct == 0` (the post-allocation default);
        /// a real read-mode `gzopen` then sets `direct = 1` for auto-detect
        /// (`gzlib.c` L186-189). Since `open.rs` is a sibling agent's file, the
        /// test reproduces that one-line open-time initialisation here so the
        /// read path sees the same starting state it would in production.
        fn open_read(&self) -> GzState {
            let file = File::open(&self.path).expect("open temp file");
            let mut state = GzState::new(file, self.path.display().to_string(), Mode::Read);
            state.direct = 1;
            state
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Build a single gzip member from `data` using flate2 (canonical C zlib).
    fn gzip(data: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).expect("gz encode");
        e.finish().expect("gz finish")
    }

    /// Drain the whole stream through repeated `gzread` calls of `chunk` bytes.
    fn read_all(state: &mut GzState, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            let n = gzread(state, &mut buf);
            assert!(n >= 0, "gzread error: {}", state.error_message());
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    #[test]
    fn transparent_copy_reads_plain_file_verbatim() {
        // A non-gzip file must be read byte-for-byte (the LOOK -> COPY path).
        let payload = b"plain text, definitely not gzip-compressed data \x00\x01\x02";
        let tmp = TempFile::with_contents("plain", payload);
        let mut state = tmp.open_read();

        // gzdirect forces the initial look and must report transparent (1).
        assert_eq!(gzdirect(&mut state), 1, "non-gzip file must be transparent");

        let got = read_all(&mut state, 8);
        assert_eq!(got, payload);
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn gzip_autodetect_decompresses() {
        // A gzip file must be auto-detected (magic sniff) and decompressed.
        let payload: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 3) as u8).collect();
        let tmp = TempFile::with_contents("gzip", &gzip(&payload));
        let mut state = tmp.open_read();

        assert_eq!(gzdirect(&mut state), 0, "gzip file must not be transparent");

        let got = read_all(&mut state, 256);
        assert_eq!(got, payload);
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn concatenated_members_decode_to_concatenation() {
        // Two gzip members in one file must decode to the concatenation
        // (Z_STREAM_END -> How::Look -> next member).
        let a = b"first member payload ";
        let b = b"second member payload!";
        let mut file = gzip(a);
        file.extend_from_slice(&gzip(b));
        let tmp = TempFile::with_contents("concat", &file);
        let mut state = tmp.open_read();

        let got = read_all(&mut state, 7);
        let mut expected = a.to_vec();
        expected.extend_from_slice(b);
        assert_eq!(got, expected);
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn trailing_garbage_after_member_is_tolerated() {
        // A valid member followed by non-gzip junk: the junk is tolerated and
        // treated as end-of-data (the `junk == 1` branch in gz_decomp).
        let payload = b"the real content";
        let mut file = gzip(payload);
        file.extend_from_slice(b"\x00\x01 not a gzip member \xff\xfe");
        let tmp = TempFile::with_contents("junk", &file);
        let mut state = tmp.open_read();

        let got = read_all(&mut state, 4);
        assert_eq!(got, payload);
        // No fatal error must be latched (trailing garbage is OK).
        assert!(state.err == Z_OK || state.err == Z_BUF_ERROR);
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn gzgetc_then_gzungetc_round_trip() {
        let payload = b"ABCDEF";
        let tmp = TempFile::with_contents("getc", &gzip(payload));
        let mut state = tmp.open_read();

        let first = gzgetc(&mut state);
        assert_eq!(first, i32::from(b'A'));
        let pos_after_get = state.pos;

        // Push it back and read it again -> same byte, position restored.
        assert_eq!(gzungetc(first, &mut state), first);
        assert_eq!(state.pos, pos_after_get - 1);
        assert_eq!(gzgetc(&mut state), i32::from(b'A'));
        assert_eq!(state.pos, pos_after_get);

        // gzgetc_ is the function form and must behave identically.
        assert_eq!(gzgetc_(&mut state), i32::from(b'B'));
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn gzungetc_on_fresh_stream_then_read() {
        // Pushing before any read primes the buffer and prepends the byte.
        let payload = b"XYZ";
        let tmp = TempFile::with_contents("unget_fresh", &gzip(payload));
        let mut state = tmp.open_read();

        assert_eq!(gzungetc(i32::from(b'Q'), &mut state), i32::from(b'Q'));
        let got = read_all(&mut state, 2);
        assert_eq!(got, b"QXYZ");
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn gzgets_reads_line_then_eof() {
        let payload = b"line one\nline two\nno newline tail";
        let tmp = TempFile::with_contents("gets", &gzip(payload));
        let mut state = tmp.open_read();

        let mut buf = [0u8; 64];
        let n1 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n1], b"line one\n");

        let n2 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n2], b"line two\n");

        let n3 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n3], b"no newline tail");

        // At EOF gzgets returns 0.
        let n4 = gzgets(&mut state, &mut buf);
        assert_eq!(n4, 0);

        // Reading a single member to EOF via gzgets leaves the benign
        // Z_BUF_ERROR ("unexpected end of file"): after Z_STREAM_END the read
        // path re-looks for a concatenated member (how -> GZIP) and the final
        // gz_fetch meets a clean EOF. This is verified byte-for-byte against the
        // repository's own C zlib (1.3.2.1-motley); the older system 1.3.1
        // predates this `junk`-driven behaviour. gzclose_r surfaces that code.
        let close = gzclose_r(&mut state);
        assert!(
            close == Z_OK || close == Z_BUF_ERROR,
            "gzclose_r returned unexpected code {close}"
        );
    }

    #[test]
    fn large_payload_round_trips_across_buffer_boundaries() {
        // A payload several times the 16 KiB output buffer exercises multiple
        // gz_fetch refills and the large-request direct-decompress path.
        let payload: Vec<u8> = (0..70_000u32)
            .map(|i| ((i ^ (i >> 3)).wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let tmp = TempFile::with_contents("large", &gzip(&payload));

        // Small chunks (forces many fetches) and one giant chunk (direct path).
        for &chunk in &[1usize, 333, 16384, 65536, payload.len() + 10] {
            let mut state = tmp.open_read();
            let got = read_all(&mut state, chunk);
            assert_eq!(got, payload, "mismatch at chunk size {chunk}");
            assert_eq!(gzclose_r(&mut state), Z_OK);
        }
    }

    #[test]
    fn gzreader_impl_read_and_bufread() {
        // The idiomatic adapter must decode via std `Read` / `BufRead`.
        let payload = b"one\ntwo\nthree\n";
        let tmp = TempFile::with_contents("reader", &gzip(payload));

        // Read::read_to_end
        {
            let mut state = tmp.open_read();
            let mut sink = Vec::new();
            {
                let mut r = GzReader::new(&mut state);
                let total = r.read_to_end(&mut sink).expect("read_to_end");
                assert_eq!(total, payload.len());
            }
            assert_eq!(sink, payload);
            assert_eq!(gzclose_r(&mut state), Z_OK);
        }

        // BufRead::read_line
        {
            let mut state = tmp.open_read();
            {
                let mut r = GzReader::new(&mut state);
                let mut line = String::new();
                let n = r.read_line(&mut line).expect("read_line");
                assert_eq!(n, 4);
                assert_eq!(line, "one\n");
            }
            assert_eq!(gzclose_r(&mut state), Z_OK);
        }
    }

    #[test]
    fn empty_gzip_member_yields_no_data() {
        // A gzip member wrapping zero bytes must read back as empty.
        let tmp = TempFile::with_contents("empty", &gzip(b""));
        let mut state = tmp.open_read();
        let got = read_all(&mut state, 16);
        assert!(got.is_empty());
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }

    #[test]
    fn empty_plain_file_is_transparent_and_empty() {
        // An empty non-gzip file: transparent read of zero bytes.
        let tmp = TempFile::with_contents("empty_plain", b"");
        let mut state = tmp.open_read();
        let got = read_all(&mut state, 16);
        assert!(got.is_empty());
        // direct may resolve to 1 (transparent) once looked at.
        assert_eq!(gzdirect(&mut state), 1);
        assert_eq!(gzclose_r(&mut state), Z_OK);
    }
}
