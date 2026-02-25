// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip file read operations — port of gzread.c (669 lines).

//! Gzip file read operations.
//!
//! This module provides read functionality for gzip files including the
//! decompression pipeline (LOOK / COPY / GZIP state machine), single-byte
//! reads, line reads, character pushback, and direct copy detection.
//!
//! Port of C zlib's `gzread.c` (669 lines).
//!
//! Feature-gated behind `gz-io`.
//!
//! ## Read Pipeline Architecture
//!
//! ```text
//! file → gz_load → in_buf → gz_look  (format detect)
//!                          → gz_decomp (inflate) → out_buf → gz_read → user
//!                          → direct copy          → out_buf → gz_read → user
//! ```
//!
//! ## Internal Function Call Graph
//!
//! ```text
//! gz_read / gz_fread / gz_getc / gz_gets
//!     └── gz_read_internal
//!             ├── gz_skip (deferred seek)
//!             └── gz_fetch
//!                     ├── gz_look  (LOOK → COPY or GZIP)
//!                     │       ├── gz_avail
//!                     │       │       └── gz_load
//!                     │       └── inflate_init2 / inflate_reset
//!                     ├── gz_load  (COPY mode — direct)
//!                     └── gz_decomp (GZIP mode)
//!                             ├── gz_avail
//!                             │       └── gz_load
//!                             └── inflate
//! ```

// During parallel module creation sibling modules may not yet reference
// every public symbol — suppress dead-code warnings.
#![allow(dead_code)]

use std::fs::File;
use std::io;

use crate::constants::{MAX_WBITS, Z_NO_FLUSH};
use crate::error::{ReturnCode, ZlibError, ZlibResult};
use crate::inflate;

use super::open::gz_error;
use super::state::{GzHow, GzMode, GzState};

// ============================================================================
// gz_load — Read from file into buffer (gzread.c lines 11–47)
// ============================================================================

/// Result of a low-level file read operation.
struct LoadResult {
    /// Number of bytes successfully read into the buffer.
    have: usize,
    /// `true` if the end of the underlying file was reached.
    eof: bool,
    /// `true` if a non-blocking descriptor returned `WouldBlock`.
    again: bool,
    /// I/O error, if one occurred.
    io_err: Option<io::Error>,
}

/// Low-level file read into `buf`.
///
/// This helper is separated from [`gz_load`] to avoid borrow-checker
/// conflicts when the caller needs to pass both `&mut GzState` (for error
/// recording) and a mutable slice that belongs to the same `GzState`.
///
/// Reads repeatedly until `buf` is filled or EOF / error is encountered,
/// matching the POSIX `read()` retry semantics of the original C code.
fn load_from_file(file: &mut File, buf: &mut [u8]) -> LoadResult {
    let mut result = LoadResult {
        have: 0,
        eof: false,
        again: false,
        io_err: None,
    };

    while result.have < buf.len() {
        match io::Read::read(file, &mut buf[result.have..]) {
            Ok(0) => {
                result.eof = true;
                break;
            }
            Ok(n) => {
                result.have += n;
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                continue; // POSIX EINTR — retry.
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                result.again = true;
                if result.have != 0 {
                    break; // Partial data is fine.
                }
                result.io_err = Some(io::Error::from(e.kind()));
                break;
            }
            Err(e) => {
                result.io_err = Some(e);
                break;
            }
        }
    }

    result
}

/// Load raw bytes from the underlying file into `buf`.
///
/// Reads repeatedly until `buf` is filled or EOF / error is encountered,
/// matching the POSIX `read()` retry semantics of the original C code.
///
/// On success returns `(bytes_loaded, eof_reached)`.
///
/// # Error Handling
///
/// - [`io::ErrorKind::Interrupted`] — retries automatically (POSIX EINTR).
/// - [`io::ErrorKind::WouldBlock`] — sets `state.again = true` and returns
///   the partial result (non-blocking I/O support).
/// - All other I/O errors — records via [`gz_error`] and returns
///   [`ZlibError::Errno`].
///
/// # C Reference
///
/// Port of `gzread.c` `gz_load()` (lines 11–47).
pub(crate) fn gz_load(state: &mut GzState, buf: &mut [u8]) -> Result<(usize, bool), ZlibError> {
    state.again = false;

    let file = match state.file.as_mut() {
        Some(f) => f,
        None => {
            gz_error(state, Some(ZlibError::Errno), Some("file not open".into()));
            return Err(ZlibError::Errno);
        }
    };

    let lr = load_from_file(file, buf);

    state.again = lr.again;
    if lr.eof {
        state.eof = true;
    }

    if let Some(e) = lr.io_err {
        gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
        return Err(ZlibError::Errno);
    }

    Ok((lr.have, state.eof))
}

// ============================================================================
// gz_avail — Ensure input buffer has data (gzread.c lines 55–82)
// ============================================================================

/// Ensure the input buffer has data ready for consumption.
///
/// If there are unconsumed bytes from a previous read they are moved to the
/// start of `in_buf`. Then any remaining capacity is filled from the file
/// via [`gz_load`].
///
/// # C Reference
///
/// Port of `gzread.c` `gz_avail()` (lines 55–82).
pub(crate) fn gz_avail(state: &mut GzState) -> Result<(), ZlibError> {
    // Short-circuit on fatal error (non-BufError).
    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError {
            return Err(*err);
        }
    }

    if !state.eof {
        let avail = state.strm.avail_in as usize;

        // Move any remaining input to the start of in_buf.
        if avail > 0 {
            let next_in_offset = buffer_offset_of_next_in(state);
            if next_in_offset != 0 {
                state
                    .in_buf
                    .copy_within(next_in_offset..next_in_offset + avail, 0);
            }
        }

        // Fill the rest of in_buf from the file.
        let size = state.size;
        let space = size - avail;
        if space > 0 {
            // Split the borrow: take the file handle out temporarily.
            let file_opt = state.file.take();
            let got = if let Some(mut file) = file_opt {
                let lr = load_from_file(&mut file, &mut state.in_buf[avail..avail + space]);
                state.file = Some(file); // put it back
                state.again = lr.again;
                if lr.eof {
                    state.eof = true;
                }
                if let Some(e) = lr.io_err {
                    gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
                    return Err(ZlibError::Errno);
                }
                lr.have
            } else {
                gz_error(state, Some(ZlibError::Errno), Some("file not open".into()));
                return Err(ZlibError::Errno);
            };
            state.strm.avail_in = (avail + got) as u32;
        }

        // Reset next_in pointer to start of in_buf.
        state.strm.next_in = state.in_buf.as_ptr();
    }

    Ok(())
}

/// Compute the byte offset of `strm.next_in` relative to `in_buf.as_ptr()`.
///
/// Returns 0 if next_in is null or points before in_buf (should not happen
/// in practice).
#[inline]
fn buffer_offset_of_next_in(state: &GzState) -> usize {
    if state.strm.next_in.is_null() || state.in_buf.is_empty() {
        return 0;
    }
    let base = state.in_buf.as_ptr() as usize;
    let current = state.strm.next_in as usize;
    current.saturating_sub(base)
}

// ============================================================================
// gz_look — Detect format and initialise (gzread.c lines 84–170)
// ============================================================================

/// Look at the first bytes of input to determine if the file contains gzip
/// data or uncompressed data.
///
/// On the very first call (when `state.size == 0`), this function:
/// 1. Allocates the input and output buffers.
/// 2. Initialises the inflate engine with `windowBits = 15 + 16`
///    (auto-detect gzip).
///
/// It then reads initial data and checks for the gzip magic number
/// (`0x1f 0x8b`) with method 8 and sensible flags (< 32).
///
/// # State Transitions
///
/// - **Gzip detected** → `state.how = GzHow::Gzip`, inflate is reset.
/// - **Not gzip** → `state.how = GzHow::Copy`, remaining input copied
///   to the output buffer for transparent pass-through.
/// - **No data yet** → `state.how` remains `GzHow::Look`.
///
/// # C Reference
///
/// Port of `gzread.c` `gz_look()` (lines 84–170).
pub(crate) fn gz_look(state: &mut GzState) -> Result<GzHow, ZlibError> {
    // ---- Lazy buffer allocation (first call) ----
    if state.size == 0 {
        // Allocate input buffer (state.want bytes).
        state.in_buf = vec![0u8; state.want];
        // Allocate output buffer (double-sized for look-ahead/ungetc room).
        state.out_buf = vec![0u8; state.want << 1];
        state.size = state.want;

        // Initialise inflate with auto-detect gzip (windowBits = 15 + 16).
        state.strm.avail_in = 0;
        state.strm.next_in = core::ptr::null();
        if let Err(_e) = inflate::inflate_init2(&mut state.strm, MAX_WBITS + 16) {
            // Roll back buffer allocation on inflate init failure.
            state.in_buf = Vec::new();
            state.out_buf = Vec::new();
            state.size = 0;
            gz_error(
                state,
                Some(ZlibError::MemError),
                Some("out of memory".into()),
            );
            return Err(ZlibError::MemError);
        }
    }

    // ---- If transparent reading is disabled (direct == false started,
    //      or looking for next gzip member), go straight to GZIP mode. ----
    // In C this checks `state->direct == -1 || state->junk == 0`.
    // We simplify: if we already know it's gzip (not the first call and
    // junk is false), reset inflate and go to GZIP mode.
    if state.how != GzHow::Look {
        // This shouldn't happen — gz_look is only called when how == Look.
        // But if re-entering for a concatenated member, reset inflate.
        inflate::inflate_reset(&mut state.strm).ok();
        state.how = GzHow::Gzip;
        state.direct = false;
        return Ok(GzHow::Gzip);
    }

    // ---- Load initial data ----
    gz_avail(state)?;
    let avail = state.strm.avail_in as usize;

    if avail == 0 || (state.again && avail < 4) {
        // No data yet or non-blocking stall before 4 bytes — stay in LOOK.
        return Ok(GzHow::Look);
    }

    // ---- Check for gzip magic header ----
    // Gzip header: magic[0]=0x1f, magic[1]=0x8b, method=8, flags<32.
    let inp = &state.in_buf[..avail.min(state.size)];
    if avail > 3 && inp[0] == 0x1f && inp[1] == 0x8b && inp[2] == 8 && inp[3] < 32 {
        // Looks like gzip — reset inflate and switch to GZIP mode.
        // The inflate engine (initialised with windowBits=15+16) will
        // parse the gzip header itself.
        inflate::inflate_reset(&mut state.strm).ok();
        state.how = GzHow::Gzip;
        state.direct = false;
        return Ok(GzHow::Gzip);
    }

    // ---- Not gzip: transparent copy mode ----
    // Copy leftover input to output buffer. The output buffer (size*2) is
    // guaranteed to be larger than the input buffer (size), leaving room
    // for gzungetc.
    let copy_len = avail.min(state.out_buf.len());
    state.out_buf[..copy_len].copy_from_slice(&state.in_buf[..copy_len]);
    state.have = copy_len;
    state.next = 0;
    state.strm.avail_in = 0;
    state.how = GzHow::Copy;
    state.direct = true;
    Ok(GzHow::Copy)
}

// ============================================================================
// gz_decomp — Decompress from input to output (gzread.c lines 172–240)
// ============================================================================

/// Decompress data from the input buffer into the output buffer using
/// the inflate engine.
///
/// The output position and count in `state` are set to point at the
/// freshly decompressed data. If the gzip stream completes
/// (`Z_STREAM_END`), `state.how` is reset to [`GzHow::Look`] to allow
/// detection of concatenated gzip members.
///
/// # C Reference
///
/// Port of `gzread.c` `gz_decomp()` (lines 172–240).
pub(crate) fn gz_decomp(state: &mut GzState) -> Result<(), ZlibError> {
    let had = state.strm.avail_out;
    let mut ret: ZlibResult = Ok(ReturnCode::Ok);

    loop {
        // Get more input for inflate() if needed.
        if state.strm.avail_in == 0 {
            if let Err(e) = gz_avail(state) {
                ret = Err(e);
                break;
            }
            if state.strm.avail_in == 0 {
                if !state.again {
                    gz_error(
                        state,
                        Some(ZlibError::BufError),
                        Some("unexpected end of file".into()),
                    );
                }
                break;
            }
        }

        // Decompress and handle errors.
        ret = inflate::inflate(&mut state.strm, Z_NO_FLUSH);
        match &ret {
            Err(ZlibError::StreamError) => {
                gz_error(
                    state,
                    Some(ZlibError::StreamError),
                    Some("internal error: inflate stream corrupt".into()),
                );
                break;
            }
            Err(ZlibError::MemError) => {
                gz_error(
                    state,
                    Some(ZlibError::MemError),
                    Some("out of memory".into()),
                );
                break;
            }
            Err(ZlibError::DataError) => {
                let msg = state
                    .strm
                    .msg
                    .clone()
                    .unwrap_or_else(|| "compressed data error".into());
                gz_error(state, Some(ZlibError::DataError), Some(msg));
                break;
            }
            Err(e) => {
                gz_error(state, Some(*e), None);
                break;
            }
            Ok(ReturnCode::NeedDict) => {
                gz_error(
                    state,
                    Some(ZlibError::StreamError),
                    Some("internal error: inflate stream corrupt".into()),
                );
                ret = Err(ZlibError::StreamError);
                break;
            }
            Ok(ReturnCode::StreamEnd) => {
                break;
            }
            Ok(ReturnCode::Ok) => {
                // Progress made — continue if more output space available.
            }
        }

        if state.strm.avail_out == 0 {
            break;
        }
    }

    // Update available output count and position.
    state.have = (had - state.strm.avail_out) as usize;
    state.next = 0;

    // If the gzip stream completed, look for another member.
    if ret == Ok(ReturnCode::StreamEnd) {
        state.how = GzHow::Look;
        return Ok(());
    }

    match ret {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

// ============================================================================
// gz_fetch — Fetch decompressed data (gzread.c lines 242–277)
// ============================================================================

/// Fetch data into the output buffer, driving the read state machine.
///
/// Depending on `state.how`:
/// - [`GzHow::Look`] — calls [`gz_look`] to detect format.
/// - [`GzHow::Copy`] — loads data directly from file into `out_buf`.
/// - [`GzHow::Gzip`] — sets up inflate output and calls [`gz_decomp`].
///
/// Loops until output data is available, EOF is reached, or an error occurs.
///
/// # C Reference
///
/// Port of `gzread.c` `gz_fetch()` (lines 242–277).
pub(crate) fn gz_fetch(state: &mut GzState) -> Result<(), ZlibError> {
    loop {
        match state.how {
            GzHow::Look => {
                let how = gz_look(state)?;
                if how == GzHow::Look {
                    return Ok(());
                }
            }
            GzHow::Copy => {
                // Direct copy: load raw bytes from file into out_buf.
                // Split borrow: temporarily take the file handle.
                let out_len = state.size << 1;
                let buf_len = state.out_buf.len().min(out_len);
                let file_opt = state.file.take();
                if let Some(mut file) = file_opt {
                    let lr = load_from_file(&mut file, &mut state.out_buf[..buf_len]);
                    state.file = Some(file);
                    state.again = lr.again;
                    if lr.eof {
                        state.eof = true;
                    }
                    if let Some(e) = lr.io_err {
                        gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
                        return Err(ZlibError::Errno);
                    }
                    state.have = lr.have;
                } else {
                    gz_error(state, Some(ZlibError::Errno), Some("file not open".into()));
                    return Err(ZlibError::Errno);
                }
                state.next = 0;
                return Ok(());
            }
            GzHow::Gzip => {
                let out_len = (state.size << 1) as u32;
                state
                    .strm
                    .set_output(&mut state.out_buf[..out_len as usize]);
                gz_decomp(state)?;
            }
        }

        if state.have > 0 {
            return Ok(());
        }

        if state.eof && state.strm.avail_in == 0 {
            return Ok(());
        }
    }
}

// ============================================================================
// gz_skip — Skip bytes for seek (gzread.c lines 279–309)
// ============================================================================

/// Skip decompressed bytes of output (for gzseek in read mode).
///
/// Reads the skip amount from `state.skip` and clears it as bytes are
/// consumed.
///
/// # C Reference
///
/// Port of `gzread.c` `gz_skip()` (lines 279–309).
pub(crate) fn gz_skip(state: &mut GzState) -> Result<(), ZlibError> {
    while state.skip > 0 {
        if state.have > 0 {
            let n = (state.have as u64).min(state.skip) as usize;
            state.have -= n;
            state.next += n;
            state.pos += n as u64;
            state.skip -= n as u64;
        } else if state.eof && state.strm.avail_in == 0 {
            break;
        } else {
            gz_fetch(state)?;
        }
    }
    Ok(())
}

// ============================================================================
// gz_read_internal — Core read (gzread.c lines 311–393)
// ============================================================================

/// Internal read function: copies decompressed data to a user buffer.
///
/// Handles pending seeks, then loops fetching and copying data until the
/// buffer is filled or EOF/error is reached.
///
/// Returns the number of bytes actually read.
///
/// # C Reference
///
/// Port of `gzread.c` `gz_read()` (lines 311–393).
pub(crate) fn gz_read_internal(state: &mut GzState, buf: &mut [u8]) -> Result<usize, ZlibError> {
    let len = buf.len();
    if len == 0 {
        return Ok(0);
    }

    // Process any deferred skip (from gzseek).
    if state.seek {
        state.seek = false;
        gz_skip(state)?;
    }

    let mut got: usize = 0;
    let mut remaining = len;
    let mut err_flag = false;

    while remaining > 0 && !err_flag {
        let n_max = remaining;

        if state.have > 0 {
            let n = state.have.min(n_max);
            buf[got..got + n].copy_from_slice(&state.out_buf[state.next..state.next + n]);
            state.next += n;
            state.have -= n;

            if state.err.is_some() && state.err != Some(ZlibError::BufError) {
                err_flag = true;
            }

            got += n;
            remaining -= n;
            state.pos += n as u64;
        } else if state.eof && state.strm.avail_in == 0 {
            break;
        } else if state.how == GzHow::Look || n_max < (state.size << 1) {
            match gz_fetch(state) {
                Ok(()) => {
                    if state.have == 0 {
                        break;
                    }
                }
                Err(e) => {
                    if state.have == 0 {
                        err_flag = true;
                        if state.err.is_none() {
                            state.err = Some(e);
                        }
                    }
                }
            }
            continue;
        } else if state.how == GzHow::Copy {
            let target = &mut buf[got..got + n_max];
            match gz_load(state, target) {
                Ok((n, _eof)) => {
                    got += n;
                    remaining -= n;
                    state.pos += n as u64;
                }
                Err(_) => {
                    err_flag = true;
                }
            }
        } else {
            state.strm.set_output(&mut buf[got..got + n_max]);
            match gz_decomp(state) {
                Ok(()) => {
                    let n = state.have;
                    state.have = 0;
                    got += n;
                    remaining -= n;
                    state.pos += n as u64;
                }
                Err(_) => {
                    let n = state.have;
                    state.have = 0;
                    got += n;
                    remaining -= n;
                    state.pos += n as u64;
                    err_flag = true;
                }
            }
        }
    }

    if remaining > 0 && state.eof {
        state.past = true;
    }

    Ok(got)
}

// ============================================================================
// Public API functions
// ============================================================================

/// Read decompressed data from a gzip file.
///
/// Equivalent to C zlib's `gzread(gzFile file, voidp buf, unsigned len)`.
/// Returns the number of bytes actually read, or 0 at EOF.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — file is not in read mode.
/// - Propagates errors from the read pipeline.
///
/// # C Reference
///
/// Port of `gzread.c` `gzread()` (lines 396–436).
pub fn gz_read(state: &mut GzState, buf: &mut [u8]) -> Result<usize, ZlibError> {
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError && !state.again {
            return Err(*err);
        }
    }

    gz_error(state, None, None);

    let got = gz_read_internal(state, buf)?;

    if got == 0 {
        if let Some(ref err) = state.err {
            if *err != ZlibError::BufError {
                return Err(*err);
            }
        }
    }

    Ok(got)
}

/// Read from a gzip file with fread-like semantics.
///
/// Reads `nitems` items of `item_size` bytes each. Returns the number of
/// **complete** items successfully read.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — not in read mode or overflow.
///
/// # C Reference
///
/// Port of `gzread.c` `gzfread()` (lines 438–465).
pub fn gz_fread(
    state: &mut GzState,
    buf: &mut [u8],
    item_size: usize,
    nitems: usize,
) -> Result<usize, ZlibError> {
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError && !state.again {
            return Ok(0);
        }
    }

    gz_error(state, None, None);

    if item_size == 0 || nitems == 0 {
        return Ok(0);
    }
    let total = item_size
        .checked_mul(nitems)
        .ok_or(ZlibError::StreamError)?;

    if total > buf.len() {
        return Err(ZlibError::StreamError);
    }

    let bytes_read = gz_read_internal(state, &mut buf[..total])?;
    Ok(bytes_read / item_size)
}

/// Read a single byte from a gzip file.
///
/// Optimised: reads directly from the output buffer when data is available.
///
/// Returns `Ok(Some(byte))` on success, `Ok(None)` at EOF.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — file is not in read mode.
///
/// # C Reference
///
/// Port of `gzread.c` `gzgetc()` (lines 473–498).
pub fn gz_getc(state: &mut GzState) -> Result<Option<u8>, ZlibError> {
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError && !state.again {
            return Err(*err);
        }
    }

    gz_error(state, None, None);

    // Fast path: return byte from output buffer.
    if state.have > 0 {
        state.have -= 1;
        state.pos += 1;
        let byte = state.out_buf[state.next];
        state.next += 1;
        return Ok(Some(byte));
    }

    // Slow path.
    let mut single = [0u8; 1];
    let got = gz_read_internal(state, &mut single)?;
    if got < 1 {
        Ok(None)
    } else {
        Ok(Some(single[0]))
    }
}

/// Push back a byte into the read stream.
///
/// Places `c` back so the next read returns it.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — not in read mode.
/// - [`ZlibError::DataError`] — output buffer is full.
///
/// # C Reference
///
/// Port of `gzread.c` `gzungetc()` (lines 505–563).
pub fn gz_ungetc(state: &mut GzState, c: u8) -> Result<u8, ZlibError> {
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    if state.how == GzHow::Look && state.have == 0 {
        let _ = gz_look(state);
    }

    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError && !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    if state.seek {
        state.seek = false;
        gz_skip(state)?;
    }

    // Empty buffer — place at end.
    if state.have == 0 {
        state.have = 1;
        let end_pos = (state.size << 1) - 1;
        state.next = end_pos;
        state.out_buf[end_pos] = c;
        state.pos = state.pos.wrapping_sub(1);
        state.past = false;
        return Ok(c);
    }

    // Buffer full — cannot push back.
    if state.have == (state.size << 1) {
        gz_error(
            state,
            Some(ZlibError::DataError),
            Some("out of room to push characters".into()),
        );
        return Err(ZlibError::DataError);
    }

    // Slide data if next is at position 0.
    if state.next == 0 {
        let have = state.have;
        let buf_size = state.size << 1;
        state.out_buf.copy_within(0..have, buf_size - have);
        state.next = buf_size - have;
    }

    state.have += 1;
    state.next -= 1;
    state.out_buf[state.next] = c;
    state.pos = state.pos.wrapping_sub(1);
    state.past = false;
    Ok(c)
}

/// Read a line from a gzip file, up to `buf.len() - 1` bytes.
///
/// Stops at newline or buffer limit. The newline, if found, is included.
/// Returns the number of bytes stored.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — not in read mode or empty buffer.
///
/// # C Reference
///
/// Port of `gzread.c` `gzgets()` (lines 565–624).
pub fn gz_gets(state: &mut GzState, buf: &mut [u8]) -> Result<usize, ZlibError> {
    if buf.is_empty() {
        return Err(ZlibError::StreamError);
    }

    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError && !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    if state.seek {
        state.seek = false;
        gz_skip(state)?;
    }

    let max_bytes = buf.len().saturating_sub(1);
    if max_bytes == 0 {
        return Ok(0);
    }

    let mut written: usize = 0;

    while written < max_bytes {
        if state.have == 0 {
            match gz_fetch(state) {
                Ok(()) => {}
                Err(_) => break,
            }
            if state.have == 0 {
                state.past = true;
                break;
            }
        }

        let scan_len = state.have.min(max_bytes - written);
        let eol_pos = state.out_buf[state.next..state.next + scan_len]
            .iter()
            .position(|&b| b == b'\n');
        let copy_len = if let Some(pos) = eol_pos {
            pos + 1
        } else {
            scan_len
        };

        buf[written..written + copy_len]
            .copy_from_slice(&state.out_buf[state.next..state.next + copy_len]);
        state.have -= copy_len;
        state.next += copy_len;
        state.pos += copy_len as u64;
        written += copy_len;

        if eol_pos.is_some() {
            break;
        }
    }

    Ok(written)
}

/// Check if data passes through without decompression.
///
/// Returns `true` if the file is not gzip-formatted and data is being
/// copied directly. May trigger format detection on first call.
///
/// # C Reference
///
/// Port of `gzread.c` `gzdirect()` (lines 627–642).
pub fn gz_direct(state: &mut GzState) -> bool {
    if state.mode == GzMode::Read && state.how == GzHow::Look && state.have == 0 {
        let _ = gz_look(state);
    }
    state.direct
}
