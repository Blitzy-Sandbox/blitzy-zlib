//! gzip read path ported from zlib `gzread.c`: the LOOK/COPY/GZIP substate
//! machine, gzip auto-detection, and the driving of the underlying inflate
//! engine (windowBits = 31, i.e. `MAX_WBITS + 16`). Files that are not gzip are
//! read transparently (verbatim passthrough).
//!
//! This module is the safe-Rust twin of zlib's `gzread.c`. It threads the owned
//! [`GzState`] handle (from [`crate::gz::state`]) through the read pipeline and
//! drives [`crate::inflate`] to decompress a gzip (or, when not gzip,
//! transparently copy) the bytes read from the owned [`std::fs::File`]. The
//! whole `gz` layer is gated by the `gz-io` Cargo feature (which implies `std` +
//! `gzip`), so this file may freely use `std`.
//!
//! # The LOOK / COPY / GZIP substate machine ([`How`])
//!
//! On the first read (and after every completed gzip member) the reader is in
//! [`How::Look`]. [`gz_look`] inspects the first bytes of input:
//!
//! * if they are a gzip header (`1f 8b 08` with a valid flags byte), it switches
//!   to [`How::Gzip`] and decompresses through inflate;
//! * otherwise the data is **not** gzip, so it switches to [`How::Copy`] and
//!   delivers the bytes verbatim (the already-buffered header bytes are copied
//!   into the output buffer, never re-read).
//!
//! On `Z_STREAM_END` the engine returns to [`How::Look`] so that **concatenated
//! gzip members** are decompressed end-to-end, and trailing garbage after a
//! complete member is tolerated via the `junk`-candidate flag (see
//! [`gz_decomp`]).
//!
//! # C pointers → safe Rust
//!
//! Following the [`GzState`] cursor model, the C raw pointers `strm.next_in` /
//! `x.next` become `usize` indices ([`in_next`](GzState::in_next) /
//! [`next`](GzState::next)) into the owned [`Vec<u8>`] buffers, paired with the
//! length counters [`in_avail`](GzState::in_avail) / [`have`](GzState::have).
//! The engine's `next_out`/`avail_out` are not stored on the stream; each
//! inflate call is handed a fresh `&mut [u8]` slice of the destination buffer.
//!
//! # Safety
//!
//! This file contains **no `unsafe`** (AAP §0.6.2 — `unsafe` is confined to
//! `crate::ffi` and `crate::inflate::fast`). C-string/`char *` handling (the
//! `gzgets` NUL-termination, the `gzread` `void *` surface) is deferred to
//! `crate::ffi`.

use std::io::{self, BufRead, Read, Seek, SeekFrom};

use crate::constants::*;
use crate::gz::state::{GzState, How, Mode};
use crate::inflate::{InflateState, inflate, inflate_init2, inflate_reset};

/// Destination for [`gz_load`] — the safe-Rust replacement for the C
/// `gz_load(state, buf, len, *have)` raw-pointer-plus-length argument.
///
/// The C function fills one of three different buffers (the input buffer, the
/// output buffer, or a caller-supplied buffer). Because the first two are owned
/// fields of [`GzState`], an enum selects the destination so [`gz_load`] can
/// resolve the slice internally (keeping the file handle and the destination
/// buffer as disjoint field borrows).
///
/// `pub(crate)` so it can appear in the signature of the `pub(crate)`
/// [`gz_load`]; the variants are never constructed outside this module.
pub(crate) enum LoadTarget<'a> {
    /// Fill `state.in_buf[start .. state.size]` from the file (the `gz_avail`
    /// top-up). `start` is where the preserved leftover input ends.
    InBuf { start: usize },
    /// Fill the whole output buffer `state.out_buf[..]` (the transparent
    /// [`How::Copy`] fetch).
    OutBuf,
    /// Fill the caller's buffer (the large transparent read straight into the
    /// user's slice, bypassing the output buffer).
    User(&'a mut [u8]),
}

/// Destination for [`gz_decomp`] — where inflate writes the decompressed bytes.
///
/// Mirrors the C convention where `gz_decomp` honors whatever `strm.next_out`
/// the caller set: the output buffer for the normal fetch path, or the caller's
/// own buffer for a large read (the decompress-directly-into-user-buffer
/// optimization).
///
/// `pub(crate)` so it can appear in the signature of the `pub(crate)`
/// [`gz_decomp`]; the variants are never constructed outside this module.
pub(crate) enum DecompTarget<'a> {
    /// Decompress into `state.out_buf` (the normal fetch path); afterwards
    /// [`have`](GzState::have)/[`next`](GzState::next) describe the produced
    /// window.
    OutBuf,
    /// Decompress straight into the caller's buffer (large-read optimization).
    User(&'a mut [u8]),
}

/// Resets the attached inflate engine to begin a fresh gzip member
/// (C `inflateReset`), returning `false` if no inflate state is attached.
///
/// The gz read path does not surface the running check value, so the
/// `adler` out-parameter of [`inflate_reset`] is discarded into a throwaway
/// local — the check is tracked internally by the [`InflateState`].
fn reset_inflate(state: &mut GzState) -> bool {
    match state.strm.state_as_mut::<InflateState>() {
        Some(infl) => {
            let mut adler = 0u32;
            inflate_reset(infl, &mut adler);
            true
        }
        None => false,
    }
}

/// Use the file to fill the buffer selected by `target` — the safe port of C
/// `gz_load` (`gzread.c` L18-47). Returns `(bytes_read, status)` where `status`
/// is `0` on success and `-1` on a recorded error.
///
/// The loop reads until the destination is full or the file signals EOF,
/// faithfully reproducing the C contract:
///
/// * a read of `0` bytes sets [`eof`](GzState::eof);
/// * a `WouldBlock` (the C `EAGAIN`/`EWOULDBLOCK`) sets
///   [`again`](GzState::again); if some bytes were already read it returns
///   success (`0`), otherwise it records a [`Z_ERRNO`] error and returns `-1`;
/// * any other I/O error records [`Z_ERRNO`] and returns `-1`.
///
/// To keep the safe core free of borrow conflicts, the [`File`](std::fs::File)
/// is moved out of the handle for the duration of the read (so the file and the
/// destination buffer — which may itself be a field of `state` — are never
/// borrowed from `state` simultaneously), then moved back. All observable
/// state mutations (`again`/`eof`/error) are applied after the read loop.
pub(crate) fn gz_load(state: &mut GzState, mut target: LoadTarget<'_>) -> (usize, i32) {
    // C: state->again = 0; errno = 0; *have = 0;
    state.again = false;

    // Move the file out so the destination buffer (possibly `state.in_buf` /
    // `state.out_buf`) can be borrowed without aliasing the file handle.
    let mut file = match state.file.take() {
        Some(f) => f,
        // No file attached (already closed): nothing to read, mirror EOF-less
        // success with zero bytes.
        None => return (0, 0),
    };

    // Upper bound for the input-buffer fill (C `state->size`). Captured by value
    // before borrowing any buffer.
    let size = state.size;

    let mut got: usize = 0;
    let mut hit_eof = false;
    let mut again = false;
    let mut err_msg: Option<String> = None;

    loop {
        // Resolve the destination slice for this iteration as a disjoint field
        // borrow; `got` advances the write cursor.
        let dst: &mut [u8] = match &mut target {
            LoadTarget::InBuf { start } => &mut state.in_buf[*start + got..size],
            LoadTarget::OutBuf => &mut state.out_buf[got..],
            LoadTarget::User(buf) => &mut buf[got..],
        };

        // Destination full -> done (C loop exits when *have == len).
        if dst.is_empty() {
            break;
        }

        // `File::read` already loops internally on short reads at the OS level,
        // but read() may still return fewer bytes than requested, so we loop.
        match file.read(dst) {
            Ok(0) => {
                // End of file.
                hit_eof = true;
                break;
            }
            Ok(n) => {
                got += n;
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    // Non-blocking stall (C EAGAIN/EWOULDBLOCK).
                    again = true;
                    if got != 0 {
                        // Some data was read: report success but flag `again`.
                        break;
                    }
                }
                // No progress on a stall, or a genuine I/O error: record it.
                err_msg = Some(e.to_string());
                break;
            }
        }
    }

    // Restore the file handle.
    state.file = Some(file);

    // Apply the deferred state mutations now that no buffer borrow is held.
    state.again = again;
    if let Some(msg) = err_msg {
        // C: gz_error(state, Z_ERRNO, zstrerror()); return -1;
        state.gz_error(Z_ERRNO, Some(&msg));
        return (got, -1);
    }
    if hit_eof {
        state.eof = true;
    }
    (got, 0)
}

/// Top up the input buffer for inflate — the safe port of C `gz_avail`
/// (`gzread.c` L49-82). Returns `0` on success, `-1` on a recorded error.
///
/// If the handle already carries a fatal error (anything but [`Z_OK`] or
/// [`Z_BUF_ERROR`]) it returns `-1` immediately. Otherwise, while not at EOF, it
/// slides any unconsumed input to the front of the buffer (the C
/// `memmove(state->in, strm->next_in, strm->avail_in)`) and reads more from the
/// file to refill the remainder, updating the input cursor/length.
pub(crate) fn gz_avail(state: &mut GzState) -> i32 {
    // C: if (state->err != Z_OK && state->err != Z_BUF_ERROR) return -1;
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return -1;
    }

    if !state.eof {
        // Move whatever is left to the start of the input buffer so the fresh
        // read appends contiguously (C memmove of `avail_in` bytes).
        if state.in_avail != 0 && state.in_next != 0 {
            let from = state.in_next;
            let len = state.in_avail;
            state.in_buf.copy_within(from..from + len, 0);
        }
        // The retained bytes now start at index 0.
        state.in_next = 0;

        // Fill the buffer from `in_avail` up to `size` (C `state->size -
        // strm->avail_in` bytes at `state->in + strm->avail_in`).
        let start = state.in_avail;
        let (got, status) = gz_load(state, LoadTarget::InBuf { start });
        if status == -1 {
            return -1;
        }
        state.in_avail += got;
        // C: strm->next_in = state->in;  (already 0).
        state.in_next = 0;
    }
    0
}

/// Look for a gzip header and set up for inflate or transparent copy — the safe
/// port of C `gz_look` (`gzread.c` L84-170). Returns `0` on success, `-1` on a
/// recorded error. Assumes [`have`](GzState::have) is `0`.
///
/// This is the heart of gzip auto-detection:
///
/// 1. **First call** (`state.size == 0`): allocate the input buffer (size
///    [`want`](GzState::want)) and the output buffer (size `want << 1`, double),
///    set [`size`](GzState::size), and initialize the inflate engine with
///    windowBits `MAX_WBITS + 16` (= 31, "gunzip"). On allocation/init failure
///    the buffers are freed and a [`Z_MEM_ERROR`] is recorded.
/// 2. **Gzip-only or next member** (`direct == -1`, set by the `'G'` open flag,
///    or `junk == 0`, set after a completed member): reset inflate and go
///    straight to [`How::Gzip`] without sniffing. `junk` becomes `1`
///    (junk-candidate) unless we were at the very start (`-1`), in which case it
///    becomes `0` so a first-member error is reported rather than tolerated.
/// 3. **Auto-detect**: load at least four header bytes; if they match the gzip
///    magic (`1f 8b 08`, flags `< 32`) switch to [`How::Gzip`]; otherwise the
///    data is not gzip — switch to [`How::Copy`], copy the already-buffered
///    bytes verbatim into the output buffer, and mark the read transparent.
pub(crate) fn gz_look(state: &mut GzState) -> i32 {
    // (1) First time in: allocate buffers and the inflate engine.
    if state.size == 0 {
        // Input buffer sized `want`; output buffer DOUBLE (`want << 1`) so a
        // transparent copy of a full input buffer always fits and there is room
        // for at least one gzungetc() push.
        state.in_buf = vec![0u8; state.want];
        state.out_buf = vec![0u8; state.want << 1];
        state.size = state.want;

        // Initialize inflate for gunzip (windowBits = MAX_WBITS + 16 = 31). The
        // gzip-vs-transparent decision is made by the magic sniff below, exactly
        // as the C source does (it does not use the `+32` auto-detect window).
        match inflate_init2(MAX_WBITS + 16) {
            Ok(boxed) => state.strm.set_state(boxed),
            Err(_) => {
                // Free the buffers and report out of memory (C frees in/out,
                // resets size, then gz_error(Z_MEM_ERROR)).
                state.in_buf = Vec::new();
                state.out_buf = Vec::new();
                state.size = 0;
                state.gz_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
        }
        // C: strm.avail_in = 0; strm.next_in = Z_NULL;
        state.in_avail = 0;
        state.in_next = 0;
    }

    // (2) Transparent reading disabled ('G' => direct == -1), or we are looking
    // for a gzip member after the first one (junk == 0): proceed directly to a
    // gzip member without sniffing.
    if state.direct == -1 || state.junk == 0 {
        if !reset_inflate(state) {
            state.gz_error(
                Z_STREAM_ERROR,
                Some("internal error: inflate stream corrupt"),
            );
            return -1;
        }
        state.how = How::Gzip;
        // C: state->junk = state->junk != -1;  (-1 -> 0 first member; 0 -> 1
        // junk candidate).
        state.junk = i32::from(state.junk != -1);
        state.direct = 0;
        return 0;
    }

    // (3) Otherwise we are at the start with auto-detect. Load header bytes; an
    // empty input is not an error (it is a transparent read of zero bytes).
    if gz_avail(state) == -1 {
        return -1;
    }
    if state.in_avail == 0 || (state.again && state.in_avail < 4) {
        // Non-blocking input stalled before four bytes -- wait for a later call.
        return 0;
    }

    // See if the first four bytes are consistent with a gzip header. The C
    // source uses the strict 4-byte sniff: magic `1f 8b`, method `08`
    // (deflate), and a flags byte with no reserved bits set (`< 32`).
    let n = state.in_next;
    if state.in_avail > 3
        && state.in_buf[n] == 31
        && state.in_buf[n + 1] == 139
        && state.in_buf[n + 2] == 8
        && state.in_buf[n + 3] < 32
    {
        if !reset_inflate(state) {
            state.gz_error(
                Z_STREAM_ERROR,
                Some("internal error: inflate stream corrupt"),
            );
            return -1;
        }
        state.how = How::Gzip;
        state.junk = 1;
        state.direct = 0;
        return 0;
    }

    // Doing raw I/O: copy any leftover input to the output buffer verbatim. This
    // relies on the output buffer being larger than the input buffer (it is —
    // `want << 1` vs `want`), which also assures space for gzungetc().
    let avail = state.in_avail;
    let src = state.in_next;
    state.out_buf[..avail].copy_from_slice(&state.in_buf[src..src + avail]);
    state.next = 0;
    state.have = avail;
    state.in_avail = 0;
    state.how = How::Copy;
    0
}

/// Decompress from the buffered input into `target` — the safe port of C
/// `gz_decomp` (`gzread.c` L172-235). Returns `0` on success, `-1` on a recorded
/// error.
///
/// On return, [`have`](GzState::have) is the number of decompressed bytes and
/// [`next`](GzState::next) is `0` (for the [`DecompTarget::OutBuf`] path, the
/// produced window is `out_buf[0 .. have]`). When a gzip member completes
/// (`Z_STREAM_END`), [`how`](GzState::how) is reset to [`How::Look`] so the next
/// read sniffs for a concatenated member. Trailing garbage after a complete
/// member (a `Z_DATA_ERROR` while `junk == 1`, i.e. before any byte of the
/// candidate member decoded) is tolerated as a clean end of file.
pub(crate) fn gz_decomp(state: &mut GzState, mut target: DecompTarget<'_>) -> i32 {
    // The inflate engine must be attached (gz_look initializes it before `how`
    // becomes GZIP). Guard once so the per-iteration downcast can be infallible.
    if state.strm.state_as::<InflateState>().is_none() {
        state.gz_error(
            Z_STREAM_ERROR,
            Some("internal error: inflate stream corrupt"),
        );
        state.have = 0;
        state.next = 0;
        return -1;
    }

    // C `had = strm->avail_out;` — the full capacity of the destination.
    let out_cap = match &target {
        DecompTarget::OutBuf => state.out_buf.len(),
        DecompTarget::User(buf) => buf.len(),
    };

    let mut produced: usize = 0; // C `had - strm->avail_out`
    let mut ret: i32 = Z_OK;
    let mut stream_end = false;

    loop {
        // Get more input for inflate() if the buffered input is exhausted.
        if state.in_avail == 0 {
            if gz_avail(state) == -1 {
                ret = state.err;
                break;
            }
            if state.in_avail == 0 {
                // No more input. Unless this was a non-blocking stall, the file
                // ended in the middle of a member -> unexpected end of file.
                if !state.again {
                    state.gz_error(Z_BUF_ERROR, Some("unexpected end of file"));
                }
                break;
            }
        }

        // Decompress. `infl` (state.strm), `input` (state.in_buf), and the
        // output slice (state.out_buf or the user buffer) are disjoint borrows.
        let outcome = {
            // Defensive, non-panicking access. The inflate engine is attached at
            // function entry for a decompressing stream, but a corrupted or
            // partially-finalized `GzState` must yield a zlib error code — never
            // a panic — matching C's `gz_decomp` recording the error and
            // returning (this mirrors the `Z_STREAM_ERROR` handling below).
            let Some(infl) = state.strm.state_as_mut::<InflateState>() else {
                state.gz_error(
                    Z_STREAM_ERROR,
                    Some("internal error: inflate stream corrupt"),
                );
                ret = Z_STREAM_ERROR;
                break;
            };
            let input = &state.in_buf[state.in_next..state.in_next + state.in_avail];
            match &mut target {
                DecompTarget::OutBuf => {
                    let output = &mut state.out_buf[produced..];
                    inflate(infl, input, output, Z_NO_FLUSH)
                }
                DecompTarget::User(buf) => {
                    let output = &mut buf[produced..];
                    inflate(infl, input, output, Z_NO_FLUSH)
                }
            }
        };

        // Advance the input cursor and the produced count from the outcome.
        state.in_next += outcome.in_consumed;
        state.in_avail -= outcome.in_consumed;
        produced += outcome.out_produced;
        ret = outcome.ret;

        // Any decompressed data marks this as a real gzip stream (so a later
        // data error is reported rather than treated as trailing garbage).
        if produced > 0 {
            state.junk = 0;
        }

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
                // Trailing garbage after a complete member is ok: end cleanly.
                state.in_avail = 0;
                state.eof = true;
                state.how = How::Look;
                ret = Z_OK;
                break;
            }
            // C parity: the inflate engine's own diagnostic (`strm->msg`, e.g.
            // "incorrect data check") is copied into the gz error exactly as C
            // `gz_decomp` does (`gzread.c`). It is surfaced verbatim by the
            // safe-Rust `gzerror`; the FFI-visible `gzerror` instead returns the
            // fixed `'static` text for the code, so no path/inflate detail
            // crosses the C ABI. See the error-disclosure note on
            // `GzState::gz_error`.
            let msg = outcome.msg.unwrap_or("compressed data error");
            state.gz_error(Z_DATA_ERROR, Some(msg));
            break;
        }
        if ret == Z_STREAM_END {
            stream_end = true;
        }

        // C `while (strm->avail_out && ret != Z_STREAM_END)`.
        if produced >= out_cap || stream_end {
            break;
        }
    }

    // Update the available output window.
    state.have = produced;
    state.next = 0;

    // If the gzip stream completed, look for another (concatenated) member.
    if ret == Z_STREAM_END {
        state.junk = 0;
        state.how = How::Look;
        return 0;
    }

    if ret != Z_OK { -1 } else { 0 }
}

/// Make some output available in the output buffer — the safe port of C
/// `gz_fetch` (`gzread.c` L237-274). Returns `0` on success, `-1` on a recorded
/// error. Assumes [`have`](GzState::have) is `0`.
///
/// Depending on [`how`](GzState::how) the data is obtained by looking for a
/// header ([`How::Look`]), copying transparently ([`How::Copy`]), or
/// decompressing ([`How::Gzip`]). The loop runs until output is produced or the
/// end of input is reached with no buffered input left.
pub(crate) fn gz_fetch(state: &mut GzState) -> i32 {
    loop {
        match state.how {
            How::Look => {
                // -> LOOK, COPY (only if never GZIP), or GZIP
                if gz_look(state) == -1 {
                    return -1;
                }
                if state.how == How::Look {
                    // Still looking (e.g. empty/stalled input): nothing to do.
                    return 0;
                }
            }
            How::Copy => {
                // -> COPY: load straight into the (whole) output buffer.
                let (got, status) = gz_load(state, LoadTarget::OutBuf);
                if status == -1 {
                    return -1;
                }
                state.have = got;
                state.next = 0;
                return 0;
            }
            How::Gzip => {
                // -> GZIP or LOOK (if end of gzip stream).
                if gz_decomp(state, DecompTarget::OutBuf) == -1 {
                    return -1;
                }
            }
        }

        // C `while (state->x.have == 0 && (!state->eof || strm->avail_in))`.
        if !(state.have == 0 && (!state.eof || state.in_avail != 0)) {
            return 0;
        }
    }
}

/// Skip [`skip`](GzState::skip) uncompressed bytes of output — the safe port of
/// C `gz_skip` (`gzread.c` L276-303). Returns `0` on success, `-1` on a recorded
/// error.
///
/// Bytes already in the output buffer are discarded first; when it empties more
/// output is fetched, until the skip amount is consumed or the end of input is
/// reached. The uncompressed position [`pos`](GzState::pos) advances by the
/// number of bytes skipped.
pub(crate) fn gz_skip(state: &mut GzState) -> i32 {
    loop {
        if state.have != 0 {
            // Skip over whatever is in the output buffer (min(skip, have)).
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
            // Output buffer empty and at end of input.
            break;
        } else {
            // Need more data to skip -- load up the output buffer.
            if gz_fetch(state) == -1 {
                return -1;
            }
        }

        // C `do { ... } while (state->skip)`.
        if state.skip == 0 {
            break;
        }
    }
    0
}

/// Read up to `buf.len()` bytes into `buf` — the safe port of the internal C
/// `gz_read` (`gzread.c` L305-389). Returns the number of bytes read; `0`
/// indicates either end of file or an error (consult [`err`](GzState::err) to
/// distinguish, exactly as C does). A recorded error after some bytes were read
/// is deferred to the next call.
///
/// Reproduces the C fast/slow paths precisely:
///
/// * available output is copied straight from the output buffer;
/// * for a small request (or while still in [`How::Look`]) the output buffer is
///   refilled via [`gz_fetch`] so that [`gzgetc`] stays fast;
/// * for a large transparent ([`How::Copy`]) request the file is read directly
///   into `buf`;
/// * for a large gzip ([`How::Gzip`]) request inflate decompresses directly into
///   `buf`, bypassing the intermediate output buffer.
pub(crate) fn gz_read(state: &mut GzState, buf: &mut [u8]) -> usize {
    let total = buf.len();
    // If len is zero, avoid unnecessary operations.
    if total == 0 {
        return 0;
    }

    // Process a skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return 0;
    }

    let mut got: usize = 0; // bytes delivered into `buf`
    let mut err = false;
    let mut remaining = total; // C `len`

    while remaining != 0 && !err {
        // Cap a single transfer at what fits in a C `unsigned` (matches the C
        // chunking and guards any 32-bit length assumption downstream).
        let mut n = remaining.min(u32::MAX as usize);

        if state.have != 0 {
            // First just try copying from the output buffer.
            if state.have < n {
                n = state.have;
            }
            buf[got..got + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.next += n;
            state.have -= n;
            if state.err != Z_OK {
                // Caught a deferred error from a previous gz_fetch().
                err = true;
            }
        } else if state.eof && state.in_avail == 0 {
            // Output buffer empty and at end of input.
            break;
        } else if state.how == How::Look || n < (state.size << 1) {
            // Small request or new stream: refill the output buffer so the next
            // gzgetc() can be fast. (If have stays 0, the error is fatal.)
            if gz_fetch(state) == -1 && state.have == 0 {
                err = true;
            }
            // No progress here -- go back to the copy branch above.
            continue;
        } else if state.how == How::Copy {
            // Large transparent request: read directly into the user buffer.
            let (got_n, status) = gz_load(state, LoadTarget::User(&mut buf[got..got + n]));
            n = got_n;
            if status == -1 {
                err = true;
            }
        } else {
            // Large gzip request: decompress directly into the user buffer.
            let status = gz_decomp(state, DecompTarget::User(&mut buf[got..got + n]));
            n = state.have;
            state.have = 0;
            if status == -1 {
                err = true;
            }
        }

        // Update progress.
        remaining -= n;
        got += n;
        state.pos += n as i64;
    }

    // Note a read past EOF.
    if remaining != 0 && state.eof {
        state.past = true;
    }

    got
}

// ===========================================================================
// Public read API — faithful ports of the C `gz*` entry points (`gzread.c`).
//
// These take the owned [`GzState`] by `&mut` (the safe-Rust replacement for the
// C `gzFile` opaque handle). The C `voidp`/`char *` surface and the
// NUL-terminated-string conventions live in `crate::ffi`; here the buffers are
// `&[u8]`/`&mut [u8]` slices and the returns are plain integers/counts.
// ===========================================================================

/// Read up to `buf.len()` bytes — the safe port of C `gzread` (`gzread.c`
/// L396-436). Returns the number of bytes read, or `-1` on error.
///
/// Mirrors the C validation sequence: the handle must be open for reading and
/// free of a serious prior error; because the C function returns an `int`, the
/// request length must fit in an `i32` (the C `(int)len < 0` guard). A return of
/// `0` means end of file *or* error; on a non-blocking stall that produced no
/// output ([`again`](GzState::again)) a [`Z_ERRNO`] error is recorded and `-1`
/// returned so the caller can tell a stall from a true EOF.
pub fn gzread(state: &mut GzState, buf: &mut [u8]) -> i32 {
    // Get internal structure and check that it's for reading.
    if state.mode != Mode::Read {
        return -1;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // Since an int is returned, make sure len fits in one, otherwise return with
    // an error (this avoids a flaw in the interface). C: `if ((int)len < 0)`.
    if buf.len() > i32::MAX as usize {
        state.gz_error(Z_STREAM_ERROR, Some("request does not fit in an int"));
        return -1;
    }

    // Read len or fewer bytes to buf.
    let len = gz_read(state, buf);

    // Check for an error.
    if len == 0 {
        if state.err != Z_OK && state.err != Z_BUF_ERROR {
            return -1;
        }
        if state.again {
            // Non-blocking input stalled after some input was read, but no
            // uncompressed bytes were produced -- let the application know this
            // isn't EOF (C records zstrerror(); EAGAIN's text is used here).
            state.gz_error(Z_ERRNO, Some("resource temporarily unavailable"));
            return -1;
        }
    }

    // Return the number of bytes read (guaranteed <= i32::MAX by the guard).
    len as i32
}

/// Read `nitems` items of `size` bytes each — the safe port of C `gzfread`
/// (`gzread.c` L439-465). Returns the number of **full items** read.
///
/// The byte count `size * nitems` is computed with an overflow guard (the C
/// `len / size != nitems` check, expressed here as a checked multiply, which
/// also yields `0` cleanly when `size == 0`). At most `buf.len()` bytes are
/// read, so the caller's slice bounds the transfer (the FFI layer sizes the
/// slice to exactly `size * nitems`).
pub fn gzfread(state: &mut GzState, size: usize, nitems: usize, buf: &mut [u8]) -> usize {
    // Get internal structure and check that it's for reading.
    if state.mode != Mode::Read {
        return 0;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return 0;
    }
    state.clear_error();

    // Compute bytes to read -- error on overflow. `checked_mul` returns `None`
    // exactly when the C `size && len / size != nitems` overflow test fires, and
    // returns `Some(0)` when `size == 0` (matching the C `len ? ... : 0` tail).
    let len = match size.checked_mul(nitems) {
        Some(len) => len,
        None => {
            state.gz_error(Z_STREAM_ERROR, Some("request does not fit in a size_t"));
            return 0;
        }
    };
    if len == 0 {
        return 0;
    }

    // Read len (or fewer) bytes; the slice bounds the transfer.
    let to_read = len.min(buf.len());
    let got = gz_read(state, &mut buf[..to_read]);

    // Return the number of full items read.
    got / size
}

/// Read one byte — the safe port of C `gzgetc` (`gzread.c` L473-498). Returns
/// the byte as `0..=255`, or `-1` at end of file or on error.
///
/// Reproduces the C fast path: if a decompressed byte is already buffered it is
/// returned directly (advancing the cursor); otherwise a one-byte [`gz_read`]
/// is performed.
pub fn gzgetc(state: &mut GzState) -> i32 {
    // Get internal structure and check that it's for reading.
    if state.mode != Mode::Read {
        return -1;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // Try output buffer (no need to check for skip request). C: `state->x.have--;
    // state->x.pos++; return *(state->x.next)++;`.
    if state.have != 0 {
        let byte = state.out_buf[state.next];
        state.next += 1;
        state.have -= 1;
        state.pos += 1;
        return i32::from(byte);
    }

    // Nothing there -- try gz_read().
    let mut one = [0u8; 1];
    if gz_read(state, &mut one) < 1 {
        -1
    } else {
        i32::from(one[0])
    }
}

/// The function form of [`gzgetc`] — the safe port of C `gzgetc_` (`gzread.c`
/// L500-502).
///
/// In C, `gzgetc` is also exposed as a macro with an inlined fast path; the
/// `gzgetc_` symbol is the guaranteed real function the macro falls back to.
/// Here both are ordinary functions, so this simply forwards to [`gzgetc`].
pub fn gzgetc_(state: &mut GzState) -> i32 {
    gzgetc(state)
}

/// Push one byte back into the input — the safe port of C `gzungetc`
/// (`gzread.c` L505-563). Returns `c` on success, or `-1` on error.
///
/// The byte is inserted at the front of the output buffer so the next read
/// returns it. The implementation faithfully reproduces the three C cases:
///
/// * **empty buffer** — place the byte at the very end of the (double-sized)
///   output buffer, leaving the maximum room for further pushes;
/// * **no room** — the buffer already holds `size << 1` bytes (a prior
///   `gzungetc` filled it), so record a [`Z_DATA_ERROR`] and fail;
/// * **room before the data** — if the data starts at index `0`, slide it to
///   the end first, then insert the byte just before it.
///
/// If the handle was only just opened, [`gz_look`] is invoked first to allocate
/// the output buffer (so there is somewhere to push into).
pub fn gzungetc(state: &mut GzState, c: i32) -> i32 {
    // Get internal structure and check that it's for reading.
    if state.mode != Mode::Read {
        return -1;
    }

    // In case this was just opened, set up the input buffer (and allocate the
    // output buffer this push needs). C ignores the return value.
    if state.how == How::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return -1;
    }
    state.clear_error();

    // Process a skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return -1;
    }

    // Can't push EOF.
    if c < 0 {
        return -1;
    }

    // Defensive: if `gz_look` could not allocate the output buffer there is
    // nowhere to push (provably unreachable once a buffer exists, but this keeps
    // the index arithmetic below from underflowing).
    if state.out_buf.is_empty() {
        return -1;
    }
    let byte = c as u8; // C: (unsigned char)c
    let end = state.size << 1; // == out_buf.len()

    // If output buffer empty, put byte at end (allows more pushing).
    if state.have == 0 {
        state.have = 1;
        state.next = end - 1;
        state.out_buf[state.next] = byte;
        state.pos -= 1;
        state.past = false;
        return c;
    }

    // If no room, give up (must have already done a gzungetc()).
    if state.have == end {
        state.gz_error(Z_DATA_ERROR, Some("out of room to push characters"));
        return -1;
    }

    // Slide output data if needed and insert byte before existing data. C copies
    // backward from `out + have` down to `out`, landing at `out + (size<<1)`;
    // `copy_within` performs the equivalent overlap-safe move.
    if state.next == 0 {
        let have = state.have;
        state.out_buf.copy_within(0..have, end - have);
        state.next = end - have;
    }
    state.have += 1;
    state.next -= 1;
    state.out_buf[state.next] = byte;
    state.pos -= 1;
    state.past = false;
    c
}

/// Read a line into `buf` — the safe port of C `gzgets` (`gzread.c` L566-624).
/// Returns the number of bytes written into `buf` (0 if nothing was read).
///
/// Bytes are copied up to and including the first newline, until `buf` is full,
/// or until end of file — whichever comes first. The C `char *` return and the
/// terminating NUL are added by `crate::ffi`; this safe core only fills the
/// provided slice (the FFI layer passes a slice one byte shorter than the user
/// buffer so it can append the NUL), faithfully reproducing the C line scan
/// (including the `memchr` for `'\n'`).
pub fn gzgets(state: &mut GzState, buf: &mut [u8]) -> usize {
    let total = buf.len();

    // Check parameters, get internal structure, and check that it's for reading.
    // C also rejects `len < 1`; an empty slice is the equivalent here.
    if total == 0 || state.mode != Mode::Read {
        return 0;
    }

    // Check that there was no (serious) error.
    if state.err != Z_OK && state.err != Z_BUF_ERROR && !state.again {
        return 0;
    }
    state.clear_error();

    // Process a skip request.
    if state.skip != 0 && gz_skip(state) == -1 {
        return 0;
    }

    // Copy output up to a new line, `buf.len()` bytes, or until there is no more
    // output, whichever comes first. (The C `left = len - 1` reservation for the
    // NUL is handled by the FFI caller sizing this slice.)
    let mut written = 0usize;
    loop {
        // Assure that something is in the output buffer.
        if state.have == 0 {
            if gz_fetch(state) == -1 {
                break; // error
            }
            if state.have == 0 {
                // End of file.
                state.past = true; // read past end
                break; // return what we have
            }
        }

        // Look for end-of-line in the current output buffer, within the
        // min(have, remaining) bytes we are allowed to copy this round.
        let left = total - written;
        let mut n = if state.have > left { left } else { state.have };
        let window = &state.out_buf[state.next..state.next + n];
        let eol = window.iter().position(|&b| b == b'\n');
        if let Some(pos) = eol {
            n = pos + 1;
        }

        // Copy through end-of-line, or the remainder if not found.
        buf[written..written + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
        state.have -= n;
        state.next += n;
        state.pos += n as i64;
        written += n;

        // C `while (left && eol == NULL)`: stop on a newline or a full buffer.
        if eol.is_some() || written == total {
            break;
        }
    }

    written
}

/// Report whether the stream is being read transparently — the safe port of C
/// `gzdirect` (`gzread.c` L627-642). Returns `1` if the file is *not* a gzip
/// stream (read verbatim), `0` if it is being decompressed.
///
/// If the stream type is not yet known (right after open: still in
/// [`How::Look`] with no buffered output) [`gz_look`] is run first so the answer
/// is definitive.
pub fn gzdirect(state: &mut GzState) -> i32 {
    // If the state is not known, but we can find out, then do so (this is mainly
    // for right after a gzopen()/gzdopen()).
    if state.mode == Mode::Read && state.how == How::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    // Return 1 if transparent, 0 if processing a gzip stream.
    i32::from(state.direct == 1)
}

/// Close a read handle — the safe port of C `gzclose_r` (`gzread.c` L645-668).
/// Returns [`Z_OK`] (or [`Z_BUF_ERROR`] if a premature EOF was pending), or
/// [`Z_STREAM_ERROR`] if the handle is not a read handle.
///
/// Tears down the read side: the inflate engine is released (via
/// [`ZStream::end`](crate::stream::ZStream::end), the [`Drop`]-based
/// `inflateEnd`), the working buffers are freed, and the file is closed. The
/// handle is marked [`finalized`](GzState::finalize) so the eventual `Drop` is a
/// no-op (no double teardown).
///
/// This is `pub(crate)` so the `gzclose` dispatcher in `crate::gz::close` routes
/// read handles here.
///
/// Note: Rust's [`std::fs::File`] does not surface a `close(2)` error when the
/// handle is dropped, so the C `ret ? Z_ERRNO : err` close-failure path collapses
/// to returning `err` (the common case).
pub(crate) fn gzclose_r(state: &mut GzState) -> i32 {
    // Get internal structure and check that it's for reading.
    if state.mode != Mode::Read {
        return Z_STREAM_ERROR;
    }

    // Free memory and close file. C: `inflateEnd(&strm); free(out); free(in);`.
    if state.size != 0 {
        // Dropping the boxed inflate state runs the inflateEnd-equivalent.
        state.strm.end();
        state.out_buf = Vec::new();
        state.in_buf = Vec::new();
        state.size = 0;
    }

    // Preserve a pending Z_BUF_ERROR (premature EOF) across the close.
    let err = if state.err == Z_BUF_ERROR {
        Z_BUF_ERROR
    } else {
        Z_OK
    };
    state.clear_error();

    // Close the file (C `close(fd)`); dropping the File closes it.
    let _ = state.file.take();

    // Mark finalized so the subsequent Drop does not repeat the teardown.
    state.finalize();

    err
}

// ===========================================================================
// Idiomatic streaming adapters (AAP §0.3.2 — "impl Read/Write/BufRead/Seek for
// the gzip layer").
//
// A [`GzState`] opened for reading *is* a byte source, so it implements
// [`Read`] (and [`BufRead`]) directly over the same machinery the C-style
// `gzread`/`gzgets` entry points use. This lets callers drop a gzip file into
// any `std::io` pipeline (`BufReader`, `read_to_end`, `lines`, `io::copy`, …)
// while the FFI surface keeps the C contract.
// ===========================================================================

impl Read for GzState {
    /// Reads decompressed (or, for a non-gzip file, verbatim) bytes into `buf`,
    /// delegating to the public [`gzread`] entry point so the exact zlib error
    /// semantics are preserved.
    ///
    /// Error mapping (driven by [`gzread`]'s `-1` return):
    ///
    /// * a non-blocking stall ([`again`](GzState::again)) surfaces as
    ///   [`io::ErrorKind::WouldBlock`];
    /// * a fatal stream error (e.g. [`Z_DATA_ERROR`]) surfaces as
    ///   [`io::ErrorKind::InvalidData`] carrying the recorded message;
    /// * a clean end of file — including the benign [`Z_BUF_ERROR`] raised by the
    ///   end-of-input probe for a concatenated member, which zlib treats as
    ///   non-fatal — returns `Ok(0)`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.mode != Mode::Read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip handle is not open for reading",
            ));
        }

        // `gzread` returns the byte count, or `-1` on a genuine error (its
        // contract already treats Z_BUF_ERROR as a non-fatal EOF and only
        // returns -1 for a serious error or a non-blocking stall).
        let n = gzread(self, buf);
        if n < 0 {
            if self.again {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let msg = self
                .msg
                .clone()
                .unwrap_or_else(|| "gzip read error".to_string());
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
        }
        Ok(n as usize)
    }
}

impl BufRead for GzState {
    /// Returns the buffered decompressed bytes, refilling via [`gz_fetch`] when
    /// the output buffer is empty. An empty slice signals end of file.
    ///
    /// Consistent with [`Read::read`](Self::read): a non-blocking stall surfaces
    /// as [`io::ErrorKind::WouldBlock`] and a fatal stream error as
    /// [`io::ErrorKind::InvalidData`], while the benign [`Z_BUF_ERROR`] from the
    /// concatenated-member EOF probe is reported as a clean end of file (an empty
    /// slice).
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.have == 0 {
            // Process a pending skip request, then top up the output buffer. A
            // failure is inspected below against the error code so a benign
            // Z_BUF_ERROR is not mistaken for a hard error.
            if self.skip != 0 {
                let _ = gz_skip(self);
            }
            if self.have == 0 {
                let _ = gz_fetch(self);
            }

            if self.have == 0 {
                if self.again {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                if self.err != Z_OK && self.err != Z_BUF_ERROR {
                    let msg = self
                        .msg
                        .clone()
                        .unwrap_or_else(|| "gzip read error".to_string());
                    return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                }
            }
        }
        Ok(self.out_slice())
    }

    /// Advances past `amt` bytes previously returned by [`fill_buf`](Self::fill_buf),
    /// updating the output cursor and the uncompressed position.
    fn consume(&mut self, amt: usize) {
        let n = amt.min(self.have);
        self.next += n;
        self.have -= n;
        self.pos += n as i64;
    }
}

/// Idiomatic [`std::io::Seek`] over the gzip layer (AAP §0.3.2 — "impl
/// Read/Write/BufRead/`Seek` for the gzip layer"), delegating to the C-faithful
/// [`gzseek`](crate::gz::open::gzseek) so the two surfaces share one
/// implementation and one set of semantics.
///
/// Supported origins, mirroring C `gzseek` (`gzlib.c`):
///
/// * [`SeekFrom::Start`] — absolute uncompressed offset (C `SEEK_SET`).
/// * [`SeekFrom::Current`] — relative to the current uncompressed position
///   (C `SEEK_CUR`); a **read** handle may seek backwards (rewind + skip
///   forward), a write handle only forwards.
///
/// [`SeekFrom::End`] is intentionally **unsupported**: a gzip stream's
/// uncompressed length is not known without decoding to the end, so C `gzseek`
/// rejects `SEEK_END` and `flate2` likewise omits end-relative seeking. It
/// returns [`io::ErrorKind::Unsupported`] rather than silently mis-seeking.
///
/// The returned value is the new uncompressed position from the start of the
/// stream, exactly as [`std::io::Seek::seek`] requires (and as C `gzseek`
/// returns). Like C, the seek is applied lazily — a forward skip is realized on
/// the next read/write — but the reported position already reflects it. An
/// invalid handle or a failed underlying file seek surfaces as an
/// [`io::Error`].
impl Seek for GzState {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => {
                // C `gzseek` takes a signed `z_off64_t`; reject an offset that
                // cannot be represented rather than wrapping it negative.
                let offset = i64::try_from(offset).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "seek offset exceeds the addressable gzip range",
                    )
                })?;
                crate::gz::open::gzseek(self, offset, SEEK_SET)
            }
            SeekFrom::Current(offset) => crate::gz::open::gzseek(self, offset, SEEK_CUR),
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "gzip streams do not support SeekFrom::End (the uncompressed \
                     length is unknown without decoding to the end); use \
                     SeekFrom::Start or SeekFrom::Current",
                ));
            }
        };

        if new_pos < 0 {
            // `gzseek` returns -1 for an invalid handle, a pending hard error,
            // an out-of-range backward seek, or an underlying file-seek failure.
            return Err(io::Error::other(
                "gzip seek failed (invalid handle, out-of-range, or I/O error)",
            ));
        }
        // `new_pos >= 0` here, so the cast is lossless.
        Ok(new_pos as u64)
    }
}

#[cfg(test)]
mod tests {
    //! Integration-style unit tests for the gzip read path.
    //!
    //! These exercise the full LOOK/COPY/GZIP machine end-to-end: gzip streams
    //! are produced with `flate2` (the dev-only oracle, which links canonical C
    //! zlib) so the tests validate that this port decodes real C-zlib output
    //! byte-for-byte, and that the transparent path passes non-gzip data through
    //! verbatim.
    //!
    //! A read handle is built directly via [`GzState::new`] (the `pub(crate)`
    //! constructor `open.rs`/`gzopen` uses). To match what `gzopen` does for a
    //! `"rb"` handle, [`direct`](GzState::direct) is set to `1` — the C
    //! "transparent assumption for an empty file" / auto-detect default
    //! (`gzlib.c` `gz_open`); the C `gz_look` COPY branch relies on this default
    //! and never sets `direct` itself.

    use super::*;
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temp path (process id + counter) so parallel test runs do not
    /// collide, without pulling in any extra crate.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "zlib_rs_gzread_test_{}_{}_{}.bin",
            tag,
            std::process::id(),
            n
        ));
        p
    }

    /// Writes `bytes` to a fresh temp file and opens it for reading, returning a
    /// read-mode [`GzState`] (with `direct == 1`, i.e. auto-detect, as `gzopen`
    /// would set it) plus the path so the caller can clean up.
    fn read_state_with(bytes: &[u8], tag: &str) -> (GzState, PathBuf) {
        let path = unique_temp_path(tag);
        {
            let mut f = File::create(&path).expect("create temp file");
            f.write_all(bytes).expect("write temp file");
            f.flush().expect("flush temp file");
        }
        let file = File::open(&path).expect("open temp file for reading");
        let mut state = GzState::new(file, path.to_string_lossy().into_owned(), Mode::Read);
        // gzopen sets direct = 1 for a read handle (auto-detect); GzState::new
        // leaves it at 0, so apply the open-time adjustment here.
        state.direct = 1;
        (state, path)
    }

    /// gzip-compresses `data` with `flate2` (canonical C zlib) at the default
    /// level, producing a standard RFC 1952 gzip member.
    fn gzip(data: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).expect("gz encode write");
        enc.finish().expect("gz encode finish")
    }

    fn cleanup(state: &mut GzState, path: &Path) {
        let _ = gzclose_r(state);
        let _ = std::fs::remove_file(path);
    }

    /// A gzip file is auto-detected and transparently decompressed; `gzdirect`
    /// reports `0` (a real gzip stream).
    #[test]
    fn auto_detect_gzip_is_decompressed() {
        let original = b"The quick brown fox jumps over the lazy dog.\n".repeat(40);
        let compressed = gzip(&original);
        // Sanity: the compressed form actually starts with the gzip magic.
        assert_eq!(&compressed[..3], &[0x1f, 0x8b, 0x08]);

        let (mut state, path) = read_state_with(&compressed, "autodetect");

        let mut out = vec![0u8; original.len() + 64];
        let n = gzread(&mut state, &mut out);
        assert!(n >= 0, "gzread reported an error: err={}", state.err);
        assert_eq!(n as usize, original.len(), "decompressed length mismatch");
        assert_eq!(
            &out[..n as usize],
            &original[..],
            "decompressed bytes differ"
        );

        // It was a gzip stream, so the read is NOT transparent.
        assert_eq!(gzdirect(&mut state), 0);

        cleanup(&mut state, &path);
    }

    /// A non-gzip file is passed through verbatim via the LOOK -> COPY path;
    /// `gzdirect` reports `1` (transparent), and the already-buffered sniff
    /// bytes are delivered (not dropped).
    #[test]
    fn transparent_non_gzip_is_passed_through() {
        let raw = b"plain, uncompressed text -- definitely not gzip\n".repeat(7);
        let (mut state, path) = read_state_with(&raw, "transparent");

        // gzdirect on a fresh handle forces gz_look; non-gzip => transparent.
        assert_eq!(gzdirect(&mut state), 1);

        let mut out = vec![0u8; raw.len() + 64];
        let n = gzread(&mut state, &mut out);
        assert_eq!(n as usize, raw.len(), "transparent length mismatch");
        assert_eq!(&out[..n as usize], &raw[..], "transparent bytes differ");

        cleanup(&mut state, &path);
    }

    /// A short non-gzip file (fewer than four bytes) still reads transparently —
    /// the magic sniff requires four bytes, so it falls through to COPY.
    #[test]
    fn transparent_short_file() {
        let raw = b"hi";
        let (mut state, path) = read_state_with(raw, "short");
        let mut out = [0u8; 16];
        let n = gzread(&mut state, &mut out);
        assert_eq!(n as usize, raw.len());
        assert_eq!(&out[..n as usize], raw);
        assert_eq!(gzdirect(&mut state), 1);
        cleanup(&mut state, &path);
    }

    /// Two concatenated gzip members decode to the concatenation of their
    /// payloads (validates the `Z_STREAM_END` -> LOOK transition).
    #[test]
    fn concatenated_members_decompress_to_concatenation() {
        let a = b"first gzip member payload\n".to_vec();
        let b = b"second gzip member payload, a bit longer\n".to_vec();
        let mut file_bytes = gzip(&a);
        file_bytes.extend_from_slice(&gzip(&b));

        let (mut state, path) = read_state_with(&file_bytes, "concat");

        let mut expected = a.clone();
        expected.extend_from_slice(&b);
        let mut out = vec![0u8; expected.len() + 64];
        let n = gzread(&mut state, &mut out);
        assert_eq!(n as usize, expected.len(), "concatenated length mismatch");
        assert_eq!(
            &out[..n as usize],
            &expected[..],
            "concatenated bytes differ"
        );

        cleanup(&mut state, &path);
    }

    /// `gzgetc` returns bytes one at a time; `gzungetc` pushes one back so the
    /// next `gzgetc` returns it again, and the reported position is consistent.
    #[test]
    fn gzgetc_then_gzungetc_round_trip() {
        let original = b"ABCDEF".to_vec();
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "getc");

        let c0 = gzgetc(&mut state);
        assert_eq!(c0, i32::from(b'A'));
        let pos_after_first = state.pos;
        assert_eq!(pos_after_first, 1);

        // Push 'A' back; position rewinds by one.
        assert_eq!(gzungetc(&mut state, c0), c0);
        assert_eq!(state.pos, 0);

        // Reading again yields 'A', then the stream continues with 'B'.
        assert_eq!(gzgetc(&mut state), i32::from(b'A'));
        assert_eq!(gzgetc(&mut state), i32::from(b'B'));
        assert_eq!(state.pos, 2);

        cleanup(&mut state, &path);
    }

    /// `gzungetc` on a freshly opened handle (output buffer empty) places the
    /// byte at the end of the buffer so it is returned before any stream data.
    #[test]
    fn gzungetc_on_fresh_handle() {
        let original = b"XYZ".to_vec();
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "unget_fresh");

        // Push a byte before reading anything: gz_look runs, buffer allocated,
        // byte parked at the end of the output buffer.
        assert_eq!(gzungetc(&mut state, i32::from(b'!')), i32::from(b'!'));
        assert_eq!(gzgetc(&mut state), i32::from(b'!'));
        assert_eq!(gzgetc(&mut state), i32::from(b'X'));

        cleanup(&mut state, &path);
    }

    /// `gzgets` reads up to and including each newline, then returns the final
    /// unterminated line, and finally `0` at end of file.
    #[test]
    fn gzgets_reads_lines() {
        let original = b"line one\nline two\nno newline at end".to_vec();
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "gets");

        let mut buf = vec![0u8; 64];

        let n1 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n1], b"line one\n");

        let n2 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n2], b"line two\n");

        let n3 = gzgets(&mut state, &mut buf);
        assert_eq!(&buf[..n3], b"no newline at end");

        // End of file: nothing more to read.
        let n4 = gzgets(&mut state, &mut buf);
        assert_eq!(n4, 0);

        cleanup(&mut state, &path);
    }

    /// `gzgets` honors a short buffer: it copies at most `buf.len()` bytes and
    /// stops without requiring a newline.
    #[test]
    fn gzgets_respects_buffer_length() {
        let original = b"a very long line with no early newline\n".to_vec();
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "gets_short");

        let mut buf = [0u8; 8];
        let n = gzgets(&mut state, &mut buf);
        assert_eq!(n, 8);
        assert_eq!(&buf[..n], &original[..8]);

        cleanup(&mut state, &path);
    }

    /// `gzfread` returns the number of whole items read.
    #[test]
    fn gzfread_returns_full_items() {
        let original = b"0123456789ABCDEF".to_vec(); // 16 bytes
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "fread");

        let mut buf = vec![0u8; 16];
        // 4 items of 4 bytes each.
        let items = gzfread(&mut state, 4, 4, &mut buf);
        assert_eq!(items, 4);
        assert_eq!(&buf[..16], &original[..]);

        cleanup(&mut state, &path);
    }

    /// The idiomatic [`Read`] impl decompresses an entire stream through
    /// `read_to_end`, exercising the repeated-`read` + clean-EOF path (including
    /// the benign Z_BUF_ERROR member probe being treated as EOF).
    #[test]
    fn read_trait_round_trips_via_read_to_end() {
        let original = b"Read trait integration payload line.\n".repeat(60);
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "readtrait");

        let mut out = Vec::new();
        let n = state
            .read_to_end(&mut out)
            .expect("read_to_end should succeed");
        assert_eq!(n, original.len());
        assert_eq!(out, original);

        cleanup(&mut state, &path);
    }

    /// The idiomatic [`BufRead`] impl streams the decompressed bytes through
    /// `fill_buf`/`consume`; here via the provided `lines()` adaptor.
    #[test]
    fn bufread_trait_yields_lines() {
        let original = b"alpha\nbeta\ngamma\n".to_vec();
        let compressed = gzip(&original);
        let (mut state, path) = read_state_with(&compressed, "bufread");

        let mut lines = Vec::new();
        loop {
            let buf = state.fill_buf().expect("fill_buf");
            if buf.is_empty() {
                break;
            }
            // Find a newline within the available window.
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8(buf[..pos].to_vec()).unwrap();
                lines.push(line);
                state.consume(pos + 1);
            } else {
                let n = buf.len();
                state.consume(n);
            }
        }
        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);

        cleanup(&mut state, &path);
    }

    /// Reading from a handle that is not open for reading fails cleanly.
    #[test]
    fn wrong_mode_is_rejected() {
        let (mut state, path) = read_state_with(b"data", "wrongmode");
        state.mode = Mode::Write;
        assert_eq!(gzread(&mut state, &mut [0u8; 4]), -1);
        assert_eq!(gzgetc(&mut state), -1);
        assert_eq!(gzungetc(&mut state, b'a' as i32), -1);
        // restore so cleanup's gzclose_r runs the read teardown
        state.mode = Mode::Read;
        cleanup(&mut state, &path);
    }
}
