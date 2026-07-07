//! The READ side of the gzip file-I/O layer (`gz*` reading API) of the
//! `zlib-rs` crate — a faithful, memory-safe Rust port of C `gzread.c`.
//!
//! This module implements the stdio-like `gz*` reading functions
//! ([`gzread`], [`gzfread`], [`gzgetc`], [`gzungetc`], [`gzgets`]) together with
//! the look-ahead / decompress pipeline that drives them: the `LOOK` → `COPY` /
//! `GZIP` mode transitions ([`How`]), gzip-header sniffing, transparent
//! pass-through of non-gzip input, and multi-member gzip decoding. Decompression
//! is performed by the crate's inflate engine ([`crate::inflate`]); the gzip
//! CRC-32 + `ISIZE` trailer is verified *inside* that engine, so this layer never
//! touches [`crate::checksum`] directly.
//!
//! The behaviour — buffering, transparent copy, EOF / `EAGAIN` (again) handling,
//! and trailing-garbage tolerance — matches reference zlib exactly.
//!
//! # Relationship to the C input model
//!
//! Reference zlib stores the *compressed-input* cursor on the embedded
//! `z_stream` (`strm.next_in` / `strm.avail_in`). This crate's idiomatic
//! [`ZStream`](crate::stream::ZStream) carries **no** such cursor fields — input
//! is handed to [`inflate`](crate::inflate::inflate) as a `&[u8]` slice on every
//! call, and the number of bytes consumed is reported back explicitly. The read
//! driver therefore keeps that bookkeeping itself, on the gz state: the
//! not-yet-decompressed compressed input is the slice
//! `in_buf[in_next .. in_next + in_avail]` (see
//! [`GzState::in_next`](crate::gz::state::GzState) /
//! [`GzState::in_avail`](crate::gz::state::GzState)). The decompressed output the
//! caller has not yet consumed is likewise the slice `out_buf[next .. next + have]`.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`**. The moving C pointers `strm.next_in`,
//! `strm.next_out`, and `state->x.next` become plain `usize` indices, so all
//! buffer access is bounds-checked slice indexing. Because the idiomatic
//! [`inflate`](crate::inflate::inflate) takes input/output as *separate* slice
//! arguments (never as fields of the state), the output buffer is temporarily
//! moved out of the state with [`std::mem::take`] while it is written, which lets
//! the input buffer, the stream, and the output buffer be borrowed
//! simultaneously without any raw pointers.

use crate::constants::Z_NO_FLUSH;
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::{GzMode, GzState, How};
use crate::inflate;
use std::io::{self, BufRead, Read};

// ===========================================================================
// Internal helpers.
// ===========================================================================

/// Allocates a zero-filled `Vec<u8>` of `len` bytes, returning [`None`] if the
/// allocation fails.
///
/// Reference zlib's `gz_look` treats a `malloc` returning `NULL` as a
/// recoverable [`ReturnCode::MemError`]. Rust's `vec![0; len]` aborts the process
/// on allocation failure, which would *not* reproduce that behaviour, so this
/// helper uses [`Vec::try_reserve_exact`] to fail gracefully and let the caller
/// record the error.
fn alloc_zeroed(len: usize) -> Option<Vec<u8>> {
    let mut v: Vec<u8> = Vec::new();
    v.try_reserve_exact(len).ok()?;
    v.resize(len, 0);
    Some(v)
}

/// Maps the error currently recorded on the state into a [`ZlibError`] for
/// internal [`Result`](core::result::Result) propagation.
///
/// This is only ever called on a path where [`GzState::err`](crate::gz::state::GzState)
/// is known to be a non-[`ReturnCode::Ok`] code, but it falls back to
/// [`ZlibError::StreamError`] defensively so the function is total.
fn current_error(state: &GzState) -> ZlibError {
    ZlibError::from_return_code(state.err).unwrap_or(ZlibError::StreamError)
}

/// Converts a zlib [`ReturnCode`] (plus an optional detail message) into an
/// [`io::Error`] for the idiomatic [`Read`] / [`BufRead`] implementations.
fn zlib_to_io(code: ReturnCode, msg: Option<&str>) -> io::Error {
    let kind = match code {
        ReturnCode::DataError => io::ErrorKind::InvalidData,
        ReturnCode::MemError => io::ErrorKind::OutOfMemory,
        ReturnCode::StreamError => io::ErrorKind::InvalidInput,
        ReturnCode::BufError => io::ErrorKind::UnexpectedEof,
        _ => io::ErrorKind::Other,
    };
    match msg {
        Some(m) => io::Error::new(kind, m.to_string()),
        None => io::Error::new(kind, code.message()),
    }
}

// ===========================================================================
// Phase 1 — low-level input: gz_load, gz_avail.
// ===========================================================================

/// Uses the file handle to load a buffer — the Rust port of C `gz_load`
/// (`gzread.c` L18-47).
///
/// Reads from [`GzState::file`](crate::gz::state::GzState) into `into`, looping
/// because a single `read` is not guaranteed to fill the slice. `*have` is reset
/// to `0` and then accumulates the number of bytes read.
///
/// * On end-of-file (`read` returns `Ok(0)`), [`GzState::eof`](crate::gz::state::GzState)
///   is set and the loop stops.
/// * On [`io::ErrorKind::WouldBlock`] (a non-blocking descriptor with no data),
///   [`GzState::again`](crate::gz::state::GzState) is set; if some bytes were
///   already read the call still succeeds, otherwise a [`ReturnCode::ErrNo`]
///   error is recorded.
/// * Any other error records [`ReturnCode::ErrNo`].
///
/// `into` must not alias a field of `state` (callers pass either a user buffer or
/// a buffer temporarily moved out of the state with [`std::mem::take`]), which is
/// what allows this function to borrow `state.file` while writing `into`.
pub(crate) fn gz_load(
    state: &mut GzState,
    into: &mut [u8],
    have: &mut usize,
) -> Result<(), ZlibError> {
    state.again = false;
    *have = 0;
    let len = into.len();
    while *have < len {
        match state.file.read(&mut into[*have..]) {
            Ok(0) => {
                // End of file (C L44-45).
                state.eof = true;
                break;
            }
            Ok(n) => {
                *have += n;
            }
            Err(e) => {
                // A non-blocking stall that still made progress is not an error
                // (C L36-40); everything else records Z_ERRNO (C L41-42).
                if e.kind() == io::ErrorKind::WouldBlock {
                    state.again = true;
                    if *have != 0 {
                        return Ok(());
                    }
                }
                let msg = e.to_string();
                state.error(ReturnCode::ErrNo, Some(&msg));
                return Err(ZlibError::ErrNo);
            }
        }
    }
    Ok(())
}

/// Loads the input buffer, setting the EOF flag when the last data is read — the
/// Rust port of C `gz_avail` (`gzread.c` L56-82).
///
/// If a serious error is already recorded the call fails immediately. Otherwise,
/// while the end of the file has not been reached, any unconsumed input is slid
/// to the front of [`GzState::in_buf`](crate::gz::state::GzState) and the
/// remainder of the buffer is filled from the file, updating
/// [`in_avail`](crate::gz::state::GzState) and resetting the input cursor
/// [`in_next`](crate::gz::state::GzState) to the start of the buffer.
pub(crate) fn gz_avail(state: &mut GzState) -> Result<(), ZlibError> {
    // A serious (non-recoverable) error is fatal here (C L60-61).
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError {
        return Err(current_error(state));
    }
    if !state.eof {
        // Copy any unconsumed input to the start of the buffer (C L63-74).
        if state.in_avail != 0 && state.in_next != 0 {
            let (start, end) = (state.in_next, state.in_next + state.in_avail);
            state.in_buf.copy_within(start..end, 0);
        }
        state.in_next = 0;

        // Fill the rest of the buffer from the file (C L75-79). The input buffer
        // is moved out so `gz_load` can borrow `state.file` while writing it.
        let start = state.in_avail;
        let size = state.size;
        let mut inbuf = core::mem::take(&mut state.in_buf);
        let mut got = 0usize;
        let res = gz_load(state, &mut inbuf[start..size], &mut got);
        state.in_buf = inbuf;
        res?;
        state.in_avail += got;
        // `next_in` has been reset to the buffer start (in_next == 0 above).
    }
    Ok(())
}

// ===========================================================================
// Phase 2 — header sniff: gz_look.
// ===========================================================================

/// Looks for a gzip header, sets up for either decompression or transparent
/// copying, and (on the first call) allocates the working buffers and inflate
/// state — the Rust port of C `gz_look` (`gzread.c` L93-170).
///
/// This is `pub(crate)` because `open.rs::gzdirect` calls it to resolve the
/// transparent-versus-gzip decision immediately after a file is opened.
///
/// The output buffer is allocated at **twice** the input buffer size. That extra
/// room is what guarantees space for the transparent-copy path (which relocates
/// the sniffed input into the output buffer) and for at least one
/// [`gzungetc`] push-back.
///
/// State on return:
/// * [`How::Gzip`] — a gzip stream was detected (or forced); decode with inflate.
/// * [`How::Copy`] — the input is not gzip; deliver it transparently.
/// * [`How::Look`] — not enough bytes are available yet to decide (a stall; a
///   transparent read of zero bytes is *not* an error).
pub(crate) fn gz_look(state: &mut GzState) -> Result<(), ZlibError> {
    // Allocate read buffers and inflate state the first time we get here
    // (C L96-122).
    if state.size == 0 {
        let want = state.want;
        // Input buffer, and an output buffer twice its size (C L98-105).
        let in_buf = match alloc_zeroed(want) {
            Some(v) => v,
            None => {
                state.error(ReturnCode::MemError, Some("out of memory"));
                return Err(ZlibError::MemError);
            }
        };
        let out_buf = match alloc_zeroed(want << 1) {
            Some(v) => v,
            None => {
                state.error(ReturnCode::MemError, Some("out of memory"));
                return Err(ZlibError::MemError);
            }
        };
        state.in_buf = in_buf;
        state.out_buf = out_buf;
        state.in_next = 0;
        state.in_avail = 0;

        // Set up for decompression: `15 + 16` requests the maximum window size
        // and gzip wrapper detection (gunzip) (C L108-121).
        if inflate::inflate_init2(&mut state.strm, 15 + 16).is_err() {
            // On failure release the buffers, reset `size`, and report OOM
            // (C L116-120).
            state.in_buf = Vec::new();
            state.out_buf = Vec::new();
            state.size = 0;
            state.error(ReturnCode::MemError, Some("out of memory"));
            return Err(ZlibError::MemError);
        }
        state.size = want;
    }

    // Get at least the magic-number bytes in the input buffer. If transparent
    // reads are disabled (`direct == -1`, forced gzip) or we are looking for a
    // subsequent member in an already-recognised gzip stream (`junk == 0`), go
    // straight to gzip decoding (C L124-133).
    if state.direct == -1 || state.junk == 0 {
        let _ = inflate::inflate_reset(&mut state.strm);
        state.how = How::Gzip;
        state.junk = i32::from(state.junk != -1);
        state.direct = 0;
        return Ok(());
    }

    // Ensure the header bytes are available (C L139-146).
    gz_avail(state)?;
    if state.in_avail == 0 || (state.again && state.in_avail < 4) {
        // Not enough bytes yet — leave `how == Look` and stall. Reading zero
        // bytes transparently is not an error.
        return Ok(());
    }

    // Sniff the gzip magic number: 0x1f 0x8b, method 8 (deflate), and a flags
    // byte with no reserved bits set (`< 32`) (C L148-159).
    if state.in_avail > 3
        && state.in_buf[state.in_next] == 31
        && state.in_buf[state.in_next + 1] == 139
        && state.in_buf[state.in_next + 2] == 8
        && state.in_buf[state.in_next + 3] < 32
    {
        let _ = inflate::inflate_reset(&mut state.strm);
        state.how = How::Gzip;
        state.junk = 1;
        state.direct = 0;
        return Ok(());
    }

    // No gzip header: copy the buffered input verbatim to the output buffer and
    // deliver it transparently. This relies on the output buffer being larger
    // than the input buffer, which the double-sizing above guarantees
    // (C L161-169).
    let avail = state.in_avail;
    let in_start = state.in_next;
    state.out_buf[..avail].copy_from_slice(&state.in_buf[in_start..in_start + avail]);
    state.next = 0;
    state.have = avail;
    state.in_avail = 0;
    state.how = How::Copy;
    Ok(())
}

// ===========================================================================
// Phase 3 — decompress: gz_decomp.
// ===========================================================================

/// Decompresses input into `output`, until the output buffer is full or the end
/// of the gzip stream is reached — the Rust port of C `gz_decomp`
/// (`gzread.c` L180-240).
///
/// On success returns the number of bytes produced (which is also stored in
/// [`GzState::have`](crate::gz::state::GzState)); those bytes occupy
/// `output[..have]`. When a complete gzip member is decoded the state's
/// [`how`](crate::gz::state::How) is reset to [`How::Look`] so the next call
/// looks for a subsequent member.
///
/// The C "return -1" paths become [`Err`]; the C "return 0" paths become
/// [`Ok`], with a deferred error (e.g. [`ReturnCode::BufError`] for an
/// unexpected end of file) left recorded on the state for the caller to observe.
///
/// `output` must not alias a field of `state`; callers pass either the user
/// buffer or the output buffer temporarily moved out with [`std::mem::take`].
pub(crate) fn gz_decomp(state: &mut GzState, output: &mut [u8]) -> Result<usize, ZlibError> {
    let had = output.len();
    let mut out_pos = 0usize;
    let mut ret = ReturnCode::Ok;

    // Fill the output buffer up to the end of the deflate stream (C L188-225).
    loop {
        // Get more input for inflate() (C L190-197).
        if state.in_avail == 0 {
            if gz_avail(state).is_err() {
                ret = state.err;
                break;
            }
            if state.in_avail == 0 {
                if !state.again {
                    state.error(ReturnCode::BufError, Some("unexpected end of file"));
                }
                break;
            }
        }

        // Decompress and handle the outcome (C L200-224). Input is the buffered
        // slice; output is the caller-owned slice from `out_pos` onward.
        let in_start = state.in_next;
        let in_end = state.in_next + state.in_avail;
        let outcome = inflate::inflate(
            &mut state.strm,
            &state.in_buf[in_start..in_end],
            &mut output[out_pos..],
            Z_NO_FLUSH,
        );
        state.in_next += outcome.consumed;
        state.in_avail -= outcome.consumed;
        out_pos += outcome.produced;
        ret = outcome.code;

        // Any decompressed output confirms this is a genuine gzip member, so a
        // later data error can no longer be tolerated as trailing junk
        // (C L201-203).
        if out_pos != 0 {
            state.junk = 0;
        }

        match ret {
            ReturnCode::StreamError | ReturnCode::NeedDict => {
                state.error(
                    ReturnCode::StreamError,
                    Some("internal error: inflate stream corrupt"),
                );
                break;
            }
            ReturnCode::MemError => {
                state.error(ReturnCode::MemError, Some("out of memory"));
                break;
            }
            ReturnCode::DataError => {
                // A data error on bytes that were only *candidate* gzip data
                // (`junk == 1`, i.e. nothing decompressed yet) is treated as
                // trailing garbage and accepted as a clean end (C L211-219).
                if state.junk == 1 {
                    state.in_avail = 0;
                    state.eof = true;
                    state.how = How::Look;
                    ret = ReturnCode::Ok;
                    break;
                }
                let msg = state.strm.msg.unwrap_or("compressed data error");
                state.error(ReturnCode::DataError, Some(msg));
                break;
            }
            _ => {}
        }

        // Continue while there is output space and the stream has not ended
        // (C L225).
        if out_pos >= had || ret == ReturnCode::StreamEnd {
            break;
        }
    }

    // Publish the produced output: bytes occupy `output[..out_pos]` (C L227-229).
    state.have = out_pos;

    // If the gzip stream completed, arrange to look for another member
    // (C L231-236).
    if ret == ReturnCode::StreamEnd {
        state.junk = 0;
        state.how = How::Look;
        return Ok(out_pos);
    }

    // Otherwise succeed only if inflate() returned Z_OK (C L238-239).
    if ret == ReturnCode::Ok {
        Ok(out_pos)
    } else {
        Err(current_error(state))
    }
}

// ===========================================================================
// Phase 4 — gz_fetch, gz_skip.
// ===========================================================================

/// Fetches data and puts it into the output buffer — the Rust port of C
/// `gz_fetch` (`gzread.c` L248-277).
///
/// Assumes the output buffer is empty (`state.have == 0`) on entry, and on a
/// successful non-stalled return leaves output ready to consume at
/// `out_buf[next .. next + have]`. Depending on the current
/// [`how`](crate::gz::state::How) mode it looks for a header
/// ([`gz_look`]), copies raw input ([`gz_load`]), or decompresses
/// ([`gz_decomp`]), repeating while nothing has been produced yet and input
/// remains.
pub(crate) fn gz_fetch(state: &mut GzState) -> Result<(), ZlibError> {
    loop {
        match state.how {
            // Look for/read a gzip header (C L255-259).
            How::Look => {
                gz_look(state)?;
                if state.how == How::Look {
                    // Still looking: not enough input to decide yet — stall.
                    return Ok(());
                }
            }
            // Straight copy of transparent input (C L260-263).
            How::Copy => {
                let cap = state.size << 1;
                let mut out = core::mem::take(&mut state.out_buf);
                let mut have = 0usize;
                let res = gz_load(state, &mut out[..cap], &mut have);
                state.out_buf = out;
                res?;
                state.have = have;
                state.next = 0;
                return Ok(());
            }
            // Decompress into the output buffer (C L264-268).
            How::Gzip => {
                let cap = state.size << 1;
                let mut out = core::mem::take(&mut state.out_buf);
                let result = gz_decomp(state, &mut out[..cap]);
                state.out_buf = out;
                state.next = 0;
                result?;
            }
        }

        // Repeat while no output has been produced and there is still input to
        // process (C L275).
        if !(state.have == 0 && (!state.eof || state.in_avail != 0)) {
            break;
        }
    }
    Ok(())
}

/// Skips [`GzState::skip`](crate::gz::state::GzState) uncompressed output bytes —
/// the Rust port of C `gz_skip` (`gzread.c` L281-309).
///
/// Bytes already in the output buffer are discarded first; otherwise more output
/// is fetched via [`gz_fetch`], stopping cleanly at end of input.
pub(crate) fn gz_skip(state: &mut GzState) -> Result<(), ZlibError> {
    while state.skip != 0 {
        if state.have != 0 {
            // Skip what is available in the output buffer. The `min` mirrors the
            // C `GT_OFF` overflow guard: `state.have` is a small buffer count, so
            // widening it to `i64` cannot overflow (C L287-295).
            let n = core::cmp::min(state.have as i64, state.skip) as usize;
            state.have -= n;
            state.next += n;
            state.pos += n as i64;
            state.skip -= n as i64;
        } else if state.eof && state.in_avail == 0 {
            // Nothing left to skip (C L297-298).
            break;
        } else {
            // Get more output to skip over (C L301-303).
            gz_fetch(state)?;
        }
    }
    Ok(())
}

// ===========================================================================
// Phase 5 — core read: gz_read.
// ===========================================================================

/// Reads as much as possible into `buf`, returning the number of bytes copied —
/// the Rust port of C `gz_read` (`gzread.c` L317-393).
///
/// This is the shared engine behind the public reading functions. It copies from
/// the output buffer when data is buffered, and otherwise chooses the most
/// efficient source: for a small request (or a not-yet-classified stream) it
/// fills the internal output buffer via [`gz_fetch`] (keeping [`gzgetc`] fast and
/// preserving room for one [`gzungetc`]); for a large request it reads
/// ([`gz_load`]) or decompresses ([`gz_decomp`]) **directly** into the caller's
/// buffer.
///
/// Errors are reported through [`GzState::err`](crate::gz::state::GzState); a
/// short read (fewer bytes than requested) at end of input sets
/// [`GzState::past`](crate::gz::state::GzState).
pub(crate) fn gz_read(state: &mut GzState, buf: &mut [u8]) -> usize {
    // Nothing to do if no output is wanted (C L322-323).
    if buf.is_empty() {
        return 0;
    }

    // Process a skip request if one is pending (C L326-328).
    if state.skip != 0 && gz_skip(state).is_err() {
        return 0;
    }

    let total = buf.len();
    let mut buf_pos = 0usize;
    let mut got = 0usize;
    let mut err = false;

    // Get `total` bytes, up to the end of the input or an error (C L332-386).
    loop {
        let remaining = total - buf_pos;
        if remaining == 0 {
            break;
        }
        // Set `n` to the maximum amount of `len` that fits in an unsigned int
        // (C L334-337).
        let mut n = remaining.min(u32::MAX as usize);
        // Whether this iteration advances the output cursors by `n`. The
        // "small request / new stream" branch fills buffers without delivering
        // bytes here, mirroring the C `continue` that skips the advance
        // (C L364).
        let mut advanced = true;

        if state.have != 0 {
            // Bytes are buffered: copy from the output buffer (C L340-349).
            if state.have < n {
                n = state.have;
            }
            buf[buf_pos..buf_pos + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.next += n;
            state.have -= n;
            // A deferred error recorded by a previous gz_fetch() surfaces once
            // the buffered bytes have been delivered (C L346-348).
            if state.err != ReturnCode::Ok {
                err = true;
            }
        } else if state.eof && state.in_avail == 0 {
            // Output buffer empty and at end of input: done (C L351-353).
            break;
        } else if state.how == How::Look || n < (state.size << 1) {
            // Get more output for small reads or for a stream we have not yet
            // classified. This fills the output buffer, keeping gzgetc() fast
            // and guaranteeing room for one gzungetc() (C L355-365).
            if gz_fetch(state).is_err() {
                err = true;
            }
            advanced = false;
        } else if state.how == How::Copy {
            // Large request, transparent input: read directly into the caller's
            // buffer (C L367-369).
            let mut have = 0usize;
            if gz_load(state, &mut buf[buf_pos..buf_pos + n], &mut have).is_err() {
                err = true;
            }
            n = have;
        } else {
            // Large request, gzip input: decompress directly into the caller's
            // buffer (C L371-378).
            let result = gz_decomp(state, &mut buf[buf_pos..buf_pos + n]);
            n = state.have;
            state.have = 0;
            if result.is_err() {
                err = true;
            }
        }

        // Update the progress counters (C L380-385).
        if advanced {
            buf_pos += n;
            got += n;
            state.pos += n as i64;
        }

        // C's `while (len && !err)` loop condition (C L386).
        if total - buf_pos == 0 || err {
            break;
        }
    }

    // Note a read that ran past the end of the file (C L388-389).
    if total - buf_pos != 0 && state.eof {
        state.past = true;
    }

    got
}

// ===========================================================================
// Phase 6 — public read API.
// ===========================================================================

/// Reads up to `buf.len()` uncompressed bytes — the Rust port of C `gzread`
/// (`gzread.c` L396-436).
///
/// Returns the number of bytes read (`0` at end of file), or `-1` on error. The
/// handle must be open for reading and free of a serious error. Because the C
/// API returns an `int`, a request larger than [`i32::MAX`] is itself an error.
pub(crate) fn gzread(state: &mut GzState, buf: &mut [u8]) -> i32 {
    // The handle must be open for reading (C L403-404).
    if state.mode != GzMode::Read {
        return -1;
    }
    // And must not be in a serious (non-recoverable, non-again) error state
    // (C L407-409).
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError && !state.again {
        return -1;
    }
    state.clear_error();

    // Since an int is returned, the request length must fit in one (C L411-416).
    if buf.len() > i32::MAX as usize {
        state.error(
            ReturnCode::StreamError,
            Some("request does not fit in an int"),
        );
        return -1;
    }

    // Read the bytes (C L418-419).
    let n = gz_read(state, buf);

    // Distinguish an end-of-file `0` from an error `0` (C L421-432).
    if n == 0 {
        if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError {
            return -1;
        }
        if state.again {
            // A non-blocking descriptor with no data available yet: report it as
            // an errno-class failure so the caller can retry (C L428-430).
            state.error(ReturnCode::ErrNo, Some("resource temporarily unavailable"));
            return -1;
        }
    }

    // `n` fits in an i32: `buf.len() <= i32::MAX` was checked above (C L435).
    n as i32
}

/// Reads `size * nitems` bytes and returns the number of complete items read —
/// the Rust port of C `gzfread` (`gzread.c` L439-465).
///
/// `buf` must be able to hold `size * nitems` bytes. A `size * nitems` product
/// that overflows [`usize`] is an error (returns `0`). If a partial item is read
/// at end of file, its bytes are still delivered into `buf` but are not counted
/// in the returned item total; the leftover can be recovered with [`gzgetc`].
pub(crate) fn gzfread(state: &mut GzState, buf: &mut [u8], size: usize, nitems: usize) -> usize {
    // The handle must be open for reading (C L446-447).
    if state.mode != GzMode::Read {
        return 0;
    }
    // And free of a serious error (C L450-452).
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError && !state.again {
        return 0;
    }
    state.clear_error();

    // Compute the number of bytes to read; a product overflow is an error
    // (C L455-461).
    let len = match size.checked_mul(nitems) {
        Some(l) => l,
        None => {
            state.error(
                ReturnCode::StreamError,
                Some("request does not fit in a size_t"),
            );
            return 0;
        }
    };

    // Read `len` bytes and return the number of full items (C L464). A zero
    // `len` (either operand zero) yields zero items with no read.
    if len == 0 {
        return 0;
    }
    let want = len.min(buf.len());
    gz_read(state, &mut buf[..want]) / size
}

/// Reads and returns a single byte (`0..=255`), or `-1` on end of file or error —
/// the Rust port of C `gzgetc` (`gzread.c` L473-498).
///
/// A byte already in the output buffer is returned directly on the fast path;
/// otherwise [`gz_read`] is used to obtain one.
pub(crate) fn gzgetc(state: &mut GzState) -> i32 {
    // The handle must be open for reading (C L480-481).
    if state.mode != GzMode::Read {
        return -1;
    }
    // And free of a serious error (C L484-486).
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError && !state.again {
        return -1;
    }
    state.clear_error();

    // Fast path: return a byte already in the output buffer (C L489-495).
    if state.have != 0 {
        state.have -= 1;
        state.pos += 1;
        let b = state.out_buf[state.next];
        state.next += 1;
        return i32::from(b);
    }

    // Slow path: read one byte via gz_read (C L497).
    let mut b = [0u8; 1];
    if gz_read(state, &mut b) < 1 {
        -1
    } else {
        i32::from(b[0])
    }
}

/// The non-macro supporting function for `gzgetc` — the Rust port of C `gzgetc_`
/// (`gzread.c` L500-502).
///
/// Reference zlib exposes `gzgetc` as a performance macro whose out-of-line
/// fallback is `gzgetc_`; both share the identical behaviour of [`gzgetc`].
pub(crate) fn gzgetc_(state: &mut GzState) -> i32 {
    gzgetc(state)
}

/// Pushes one byte back so the next read returns it — the Rust port of C
/// `gzungetc` (`gzread.c` L505-563).
///
/// Returns the byte pushed, or `-1` on error. At most one push is guaranteed
/// after a fresh open (the double-sized output buffer reserves room); further
/// pushes succeed only while buffer space remains. `c` must be a valid byte
/// (`>= 0`); pushing EOF is rejected.
pub(crate) fn gzungetc(c: i32, state: &mut GzState) -> i32 {
    // The handle must be open for reading (C L512-513).
    if state.mode != GzMode::Read {
        return -1;
    }

    // If the buffers have not been set up yet (nothing has been read), do a
    // gz_look() so the output buffer exists to push into (C L516-517).
    if state.how == How::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    // No pushing after a serious error (C L520-522). `gz_look` above may itself
    // have recorded one.
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError && !state.again {
        return -1;
    }
    state.clear_error();

    // Process a skip request if one is pending (C L525-526).
    if state.skip != 0 && gz_skip(state).is_err() {
        return -1;
    }

    // Can't push EOF (C L529-530).
    if c < 0 {
        return -1;
    }

    let cap = state.size << 1;

    // If the output buffer is empty, put the byte at the *end* so subsequent
    // pushes still have room in front of it (C L533-540).
    if state.have == 0 {
        state.have = 1;
        state.next = cap - 1;
        state.out_buf[state.next] = c as u8;
        state.pos -= 1;
        state.past = false;
        return c;
    }

    // If there is no room to push, give up (C L543-546).
    if state.have == cap {
        state.error(
            ReturnCode::DataError,
            Some("out of room to push characters"),
        );
        return -1;
    }

    // Slide the output data to the end of the buffer if it is at the front, so
    // there is room in front of it for the pushed byte (C L549-554).
    if state.next == 0 {
        let have = state.have;
        state.out_buf.copy_within(0..have, cap - have);
        state.next = cap - have;
    }

    // Insert the byte just before the existing data (C L556-561).
    state.have += 1;
    state.next -= 1;
    state.out_buf[state.next] = c as u8;
    state.pos -= 1;
    state.past = false;
    c
}

/// Reads a `'\n'`-terminated line into `buf` — the Rust port of C `gzgets`
/// (`gzread.c` L566-624).
///
/// Copies bytes until a newline is seen (which is included), `buf.len() - 1`
/// bytes have been copied, or end of file is reached — whichever comes first —
/// then NUL-terminates the result. Returns [`Some`] with the number of bytes
/// written (excluding the terminator), or [`None`] if `buf` is empty, the handle
/// is not readable, or nothing was read before end of file.
pub(crate) fn gzgets(state: &mut GzState, buf: &mut [u8]) -> Option<usize> {
    // Check parameters: the buffer must have room for at least the terminator
    // (C L574-575).
    if buf.is_empty() {
        return None;
    }
    // The handle must be open for reading (C L578-579).
    if state.mode != GzMode::Read {
        return None;
    }
    // And free of a serious error (C L582-584).
    if state.err != ReturnCode::Ok && state.err != ReturnCode::BufError && !state.again {
        return None;
    }
    state.clear_error();

    // Process a skip request if one is pending (C L587-588).
    if state.skip != 0 && gz_skip(state).is_err() {
        return None;
    }

    // Copy output bytes up to a newline, `len - 1` bytes, or EOF, whichever
    // comes first, refilling the output buffer as needed (C L591-615).
    let mut buf_pos = 0usize;
    let mut left = buf.len() - 1;
    if left != 0 {
        loop {
            // Assure that something is in the output buffer (C L595-600).
            if state.have == 0 && gz_fetch(state).is_err() {
                break;
            }
            if state.have == 0 {
                // End of file (C L599).
                state.past = true;
                break;
            }

            // Look for the newline in the currently available output, limited to
            // what will fit (C L602-606).
            let n_avail = if state.have > left { left } else { state.have };
            let window = &state.out_buf[state.next..state.next + n_avail];
            let (n, found_eol) = match window.iter().position(|&b| b == b'\n') {
                Some(pos) => (pos + 1, true),
                None => (n_avail, false),
            };

            // Copy through the end-of-line, or the whole window if none found
            // (C L609-614).
            buf[buf_pos..buf_pos + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.have -= n;
            state.next += n;
            state.pos += n as i64;
            left -= n;
            buf_pos += n;

            if left == 0 || found_eol {
                break;
            }
        }
    }

    // Return `None` if nothing was read (C L618-619).
    if buf_pos == 0 {
        return None;
    }

    // NUL-terminate the string and report the number of bytes written
    // (C L622-623).
    buf[buf_pos] = 0;
    Some(buf_pos)
}

// ===========================================================================
// Phase 7 — idiomatic std::io::Read / BufRead.
// ===========================================================================

/// Idiomatic Rust reading over a gzip file handle, decompressing transparently.
///
/// This is the first-class Rust API that sits alongside the C-compatible
/// [`gzread`] family: it lets a [`GzState`] opened for reading be used with the
/// standard [`Read`] combinators (`read_to_end`, `read_exact`, `io::copy`, …).
/// Internally it delegates to [`gz_read`] and translates a recorded
/// [`ReturnCode`] error into an [`io::Error`]. End of file is reported as
/// `Ok(0)`, matching the [`Read`] contract.
impl Read for GzState {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip file not open for reading",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        // Surface an already-pending serious error before reading.
        if self.err != ReturnCode::Ok && self.err != ReturnCode::BufError && !self.again {
            return Err(zlib_to_io(self.err, self.msg.as_deref()));
        }
        self.clear_error();

        let n = gz_read(self, buf);
        if n == 0 && self.err != ReturnCode::Ok && self.err != ReturnCode::BufError {
            // A genuine error (as opposed to a clean end of file).
            return Err(zlib_to_io(self.err, self.msg.as_deref()));
        }
        Ok(n)
    }
}

/// Buffered idiomatic reading over a gzip file handle.
///
/// Implementing [`BufRead`] exposes the internal decompressed output buffer
/// directly, so the standard `read_line` / `read_until` / `lines` combinators
/// become the idiomatic analog of [`gzgets`], and callers can peek at
/// decompressed data without an extra copy.
impl BufRead for GzState {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.mode != GzMode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip file not open for reading",
            ));
        }
        // Honour a pending skip request first.
        if self.skip != 0 && gz_skip(self).is_err() {
            return Err(zlib_to_io(self.err, self.msg.as_deref()));
        }
        // Make sure the output buffer holds something (unless at end of input).
        // `gz_fetch` mutates `self.have`, so the pre-fetch guard and the
        // post-fetch emptiness check read distinct values and must stay
        // separate (an error that still managed to buffer data is not fatal).
        if self.have == 0 {
            let fetched = gz_fetch(self);
            if fetched.is_err() && self.have == 0 {
                return Err(zlib_to_io(self.err, self.msg.as_deref()));
            }
        }
        Ok(&self.out_buf[self.next..self.next + self.have])
    }

    fn consume(&mut self, amt: usize) {
        let n = amt.min(self.have);
        self.next += n;
        self.have -= n;
        self.pos += n as i64;
    }
}

// ===========================================================================
// Phase 8 — read finaliser for close.rs.
// ===========================================================================

/// Computes the close status for the read side — the read-specific half of C
/// `gzclose_r` (`gzread.c` L645-668).
///
/// `close.rs::gzclose_r` calls this to capture the return code before the handle
/// is dropped: a pending [`ReturnCode::BufError`] (an unexpected end of file
/// during the final decode) is preserved and reported, otherwise the close is
/// [`ReturnCode::Ok`]. The actual resource release (`inflateEnd`, buffer frees,
/// and closing the file) is handled by [`Drop`] on [`GzState`] / its
/// [`ZStream`](crate::stream::ZStream), so this function only reports status.
pub(crate) fn finish_read(state: &GzState) -> ReturnCode {
    if state.err == ReturnCode::BufError {
        ReturnCode::BufError
    } else {
        ReturnCode::Ok
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::ZStream;
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A 32-byte gzip member decoding to `"hello, world"` (produced by reference
    /// zlib / `gzip`). Used to exercise the gzip decode path end to end.
    const HELLO_GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 203, 72, 205, 201, 201, 215, 81, 40, 207, 47, 202, 73,
        1, 0, 58, 114, 171, 255, 12, 0, 0, 0,
    ];

    /// Two concatenated gzip members decoding to `"ABC"` + `"DEF"` = `"ABCDEF"`.
    /// Used to verify multi-member decoding (`how` returns to `Look`).
    const CONCAT_GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 115, 116, 114, 6, 0, 72, 3, 131, 163, 3, 0, 0, 0, 31,
        139, 8, 0, 0, 0, 0, 0, 2, 255, 115, 113, 117, 3, 0, 235, 163, 99, 154, 3, 0, 0, 0,
    ];

    /// A gzip member decoding to `"0123456789"` repeated 20 times (200 bytes).
    /// Combined with a tiny `want`, it forces many input/output buffer refills.
    const MULTIFILL_GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 51, 48, 52, 50, 54, 49, 53, 51, 183, 176, 52, 24, 210,
        44, 0, 64, 163, 41, 65, 200, 0, 0, 0,
    ];

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Returns a unique temporary path (never committed; cleaned up per test).
    fn unique_temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        p.push(format!(
            "blitzy_adhoc_test_gzread_{tag}_{}_{n}",
            std::process::id()
        ));
        p
    }

    /// Writes `bytes` to a fresh temp file and returns its path.
    fn write_temp(tag: &str, bytes: &[u8]) -> PathBuf {
        let path = unique_temp_path(tag);
        let mut f = File::create(&path).expect("create temp file");
        f.write_all(bytes).expect("write temp file");
        f.flush().expect("flush temp file");
        path
    }

    /// Builds a `GzState` open for reading over `path`, with I/O buffer size
    /// `want`, in the fresh auto-detect state a real `gzopen` would produce
    /// (`direct = 1`, `junk = -1`, `how = Look`, `size = 0`).
    fn open_read(path: &Path, want: usize) -> GzState {
        let file = File::open(path).expect("open temp file for reading");
        GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode: GzMode::Read,
            file,
            path: path.to_string_lossy().into_owned(),
            size: 0,
            want,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 1,
            how: How::Look,
            junk: -1,
            again: false,
            in_next: 0,
            in_avail: 0,
            start: 0,
            eof: false,
            past: false,
            level: 0,
            strategy: 0,
            reset: false,
            skip: 0,
            err: ReturnCode::Ok,
            msg: None,
            strm: ZStream::new(),
        }
    }

    /// Reads the whole handle in small chunks (to exercise buffer refills).
    fn read_all(state: &mut GzState) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let n = gz_read(state, &mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    #[test]
    fn decompresses_a_gzip_member() {
        let path = write_temp("hello", HELLO_GZIP);
        let mut state = open_read(&path, 8192);

        let got = read_all(&mut state);
        assert_eq!(got, b"hello, world");
        // A clean end of stream leaves no serious error and `how` back at Look.
        assert_eq!(state.err, ReturnCode::Ok);
        assert_eq!(state.how, How::Look);
        // The gzip stream was recognised, not passed through transparently.
        assert_eq!(state.direct, 0);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reads_non_gzip_input_transparently() {
        let raw = b"This is plain text, definitely not a gzip stream.";
        let path = write_temp("plain", raw);
        let mut state = open_read(&path, 8192);

        let got = read_all(&mut state);
        assert_eq!(got, raw);
        // Transparent copy: `how` is Copy and `direct` stays 1 (transparent).
        assert_eq!(state.how, How::Copy);
        assert_eq!(state.direct, 1);
        assert_eq!(state.err, ReturnCode::Ok);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn short_non_gzip_input_is_transparent() {
        // Fewer than 4 bytes at EOF must be delivered transparently, not stalled.
        let path = write_temp("ab", b"AB");
        let mut state = open_read(&path, 8192);

        assert_eq!(read_all(&mut state), b"AB");
        assert_eq!(state.how, How::Copy);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn decodes_concatenated_gzip_members() {
        let path = write_temp("concat", CONCAT_GZIP);
        let mut state = open_read(&path, 8192);

        assert_eq!(read_all(&mut state), b"ABCDEF");
        // After decoding all members, the driver returns to looking for more.
        assert_eq!(state.how, How::Look);
        assert_eq!(state.err, ReturnCode::Ok);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn decodes_across_many_buffer_fills() {
        // A tiny `want` forces the input to be reloaded many times mid-stream.
        let path = write_temp("multifill", MULTIFILL_GZIP);
        let mut state = open_read(&path, 8);

        let expected: Vec<u8> = b"0123456789".iter().cycle().take(200).copied().collect();
        assert_eq!(read_all(&mut state), expected);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_file_reads_zero_and_marks_past() {
        let path = write_temp("empty", b"");
        let mut state = open_read(&path, 8192);

        let mut buf = [0u8; 8];
        assert_eq!(gz_read(&mut state, &mut buf), 0);
        assert!(state.past, "reading past EOF must set `past`");

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gzgetc_returns_bytes_then_eof() {
        let path = write_temp("getc", b"AB");
        let mut state = open_read(&path, 8192);

        assert_eq!(gzgetc(&mut state), i32::from(b'A'));
        assert_eq!(gzgetc(&mut state), i32::from(b'B'));
        assert_eq!(gzgetc(&mut state), -1);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gzungetc_after_read_pushes_byte_back() {
        let path = write_temp("unget_mid", b"XY");
        let mut state = open_read(&path, 8192);

        assert_eq!(gzgetc(&mut state), i32::from(b'X'));
        // Push the byte back and read it again.
        assert_eq!(gzungetc(i32::from(b'X'), &mut state), i32::from(b'X'));
        assert_eq!(gzgetc(&mut state), i32::from(b'X'));
        assert_eq!(gzgetc(&mut state), i32::from(b'Y'));
        assert_eq!(gzgetc(&mut state), -1);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gzungetc_before_any_read_prepends_byte() {
        // Ungetc on a fresh handle must set the buffers up (via gz_look) and
        // deliver the pushed byte ahead of the real (gzip-decoded) data.
        let path = write_temp("unget_fresh", HELLO_GZIP);
        let mut state = open_read(&path, 8192);

        assert_eq!(gzungetc(i32::from(b'Z'), &mut state), i32::from(b'Z'));
        assert_eq!(gzgetc(&mut state), i32::from(b'Z'));
        // The underlying gzip stream is still intact after the pushed byte.
        assert_eq!(read_all(&mut state), b"hello, world");

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gzgets_reads_lines_including_newline() {
        let path = write_temp("gets", b"line1\nline2\n");
        let mut state = open_read(&path, 8192);

        let mut buf = [0u8; 64];
        assert_eq!(gzgets(&mut state, &mut buf), Some(6));
        assert_eq!(&buf[..6], b"line1\n");
        assert_eq!(buf[6], 0, "gzgets must NUL-terminate");

        assert_eq!(gzgets(&mut state, &mut buf), Some(6));
        assert_eq!(&buf[..6], b"line2\n");

        // Nothing left: EOF returns None.
        assert_eq!(gzgets(&mut state, &mut buf), None);
        assert!(state.past);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gzgets_truncates_to_buffer_size() {
        let path = write_temp("gets_trunc", b"abcdef\n");
        let mut state = open_read(&path, 8192);

        // A 4-byte buffer holds 3 data bytes plus the terminator.
        let mut buf = [0u8; 4];
        assert_eq!(gzgets(&mut state, &mut buf), Some(3));
        assert_eq!(&buf[..3], b"abc");
        assert_eq!(buf[3], 0);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn io_read_trait_decompresses() {
        let path = write_temp("io_read", HELLO_GZIP);
        let mut state = open_read(&path, 8192);

        let mut got = Vec::new();
        let n = state.read_to_end(&mut got).expect("read_to_end");
        assert_eq!(n, got.len());
        assert_eq!(got, b"hello, world");

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn bufread_read_line_splits_transparent_input() {
        let path = write_temp("bufread", b"alpha\nbeta\n");
        let mut state = open_read(&path, 8192);

        let mut line = String::new();
        assert_eq!(state.read_line(&mut line).expect("read_line"), 6);
        assert_eq!(line, "alpha\n");

        line.clear();
        assert_eq!(state.read_line(&mut line).expect("read_line"), 5);
        assert_eq!(line, "beta\n");

        // End of input: read_line returns Ok(0).
        line.clear();
        assert_eq!(state.read_line(&mut line).expect("read_line"), 0);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn finish_read_reports_buf_error() {
        let path = write_temp("finish", b"");
        let mut state = open_read(&path, 8192);
        assert_eq!(finish_read(&state), ReturnCode::Ok);

        // Simulate a pending unexpected-EOF buffer error.
        state.err = ReturnCode::BufError;
        assert_eq!(finish_read(&state), ReturnCode::BufError);

        drop(state);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_operations_on_a_write_handle() {
        let path = write_temp("wrongmode", HELLO_GZIP);
        let mut state = open_read(&path, 8192);
        state.mode = GzMode::Write;

        let mut buf = [0u8; 8];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(gzgetc(&mut state), -1);
        assert_eq!(gzungetc(i32::from(b'x'), &mut state), -1);
        assert_eq!(gzgets(&mut state, &mut buf), None);

        drop(state);
        std::fs::remove_file(&path).ok();
    }
}
