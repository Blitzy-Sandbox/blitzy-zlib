// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip file write pipeline — port of gzwrite.c (701 lines).

//! Gzip file write operations.
//!
//! This module provides write functionality for gzip files including
//! buffered writing, single-byte output, string output, formatted output,
//! flushing, and dynamic parameter changes.
//!
//! Port of C zlib's `gzwrite.c` (701 lines).
//!
//! Feature-gated behind `gz-io`.
//!
//! # Write Pipeline
//!
//! ```text
//! caller data ──► in_buf ──► deflate ──► out_buf ──► file
//!           (small writes)        (gz_comp)       (write_all)
//!
//! caller data ─────────────► deflate ──► out_buf ──► file
//!           (large writes)        (gz_comp)       (write_all)
//! ```
//!
//! Small writes (< buffer size) are accumulated in `in_buf` and compressed
//! when the buffer fills.  Large writes bypass `in_buf` and are fed
//! directly to the deflate engine.

// During parallel module creation, sibling modules may not yet reference
// every public symbol — suppress dead-code warnings.
#![allow(dead_code)]

use std::io::{self, Write};

use crate::constants::{
    DEF_MEM_LEVEL, MAX_WBITS, Z_BLOCK, Z_DEFLATED, Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH,
    Z_PARTIAL_FLUSH, Z_SYNC_FLUSH, Z_TREES,
};
use crate::deflate;
use crate::error::{ReturnCode, ZlibError};

use super::open::gz_error;
use super::state::{GzMode, GzState};

// ============================================================================
// gz_init — Initialize write buffers and deflate engine
// ============================================================================

/// Initialize gzip write state: allocate buffers and start the deflate
/// compression engine.
///
/// Called lazily on the first write operation.  Allocates the input buffer
/// (`in_buf`, size `want`) and, when not in direct/transparent mode, the
/// output buffer (`out_buf`, also size `want`) plus the deflate engine
/// configured for gzip framing (windowBits = `MAX_WBITS + 16` = 31).
///
/// After this call, `state.size` is set to `state.want`, signalling that
/// buffers have been allocated.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gz_init()` (lines 11–57).
///
/// # Errors
///
/// - [`ZlibError::MemError`] — buffer allocation or `deflateInit2` failed.
pub(crate) fn gz_init(state: &mut GzState) -> Result<(), ZlibError> {
    // Allocate input buffer.
    // C doubles this for gzprintf; in Rust, callers use format!() externally
    // so a single-size buffer suffices.
    state.in_buf = vec![0u8; state.want];

    // Only need output buffer and deflate state when compressing.
    if !state.direct {
        // Allocate output buffer.
        state.out_buf = vec![0u8; state.want];

        // Initialize deflate for gzip format (windowBits = MAX_WBITS + 16).
        let init_result = deflate::deflate_init2(
            &mut state.strm,
            state.level,
            Z_DEFLATED,
            MAX_WBITS + 16, // 31 → gzip wrapper
            DEF_MEM_LEVEL,
            state.strategy,
        );

        if let Err(e) = init_result {
            // Clean up on failure.
            state.out_buf = Vec::new();
            state.in_buf = Vec::new();
            gz_error(
                state,
                Some(ZlibError::MemError),
                Some("out of memory".into()),
            );
            return Err(e);
        }

        // Point strm output to our output buffer.
        state.strm.set_output(&mut state.out_buf[..state.want]);
        // Track write-to-file position within out_buf (reuse `next` field).
        state.next = 0;
    }

    // Mark state as initialized.
    state.size = state.want;
    Ok(())
}

// ============================================================================
// Internal helper: write a slice to the file inside GzState
// ============================================================================

/// Write `data` to the file handle inside `state`, returning an I/O
/// result.  This helper confines the borrow of `state.file` so that
/// `gz_error` can be called afterwards.
///
/// **NOTE:** For call sites that also borrow other fields of `state`
/// (e.g. `state.out_buf`), callers should borrow `state.file` directly
/// via `state.file.as_mut()` to satisfy the borrow checker instead of
/// calling this function.
#[inline]
fn file_write_all(state: &mut GzState, data: &[u8]) -> io::Result<()> {
    match state.file.as_mut() {
        Some(f) => f.write_all(data),
        None => Err(io::Error::new(io::ErrorKind::Other, "no file handle")),
    }
}

// ============================================================================
// gz_comp — Compress and write to file
// ============================================================================

/// Compress whatever input is pending in the stream and write the
/// resulting compressed bytes to the output file.
///
/// This is the core compression loop.  It repeatedly calls `deflate()`
/// and flushes the output buffer to the file via [`std::io::Write`].
///
/// When `flush` is [`Z_FINISH`], the deflate stream is marked for reset
/// so that the next write starts a new gzip member.
///
/// **Note:** This function only handles compressed (non-direct) mode.
/// Direct-mode writes are handled at the call site.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gz_comp()` (lines 65–148), compressed branch.
///
/// # Errors
///
/// - [`ZlibError::Errno`] — file write failed.
/// - [`ZlibError::StreamError`] — deflate returned an internal error.
pub(crate) fn gz_comp(state: &mut GzState, flush: i32) -> Result<(), ZlibError> {
    // In direct (transparent) mode there are no compression buffers and
    // no deflate engine — nothing to do.  This guard is necessary because
    // gz_close_w calls gz_comp(state, Z_FINISH) unconditionally (matching
    // the C code), but in direct mode the output buffer and deflate
    // stream are never initialised.  In C this "works" through undefined
    // behavior (NULL pointer comparisons); in Rust we handle it cleanly.
    if state.direct {
        return Ok(());
    }

    // Lazy allocation on first call.
    if state.size == 0 {
        gz_init(state)?;
    }

    // Check for a pending reset (after a previous Z_FINISH completed a
    // gzip member).  Don't actually reset unless there is data to write
    // or a non-trivial flush is requested.
    if state.reset_pending {
        if state.strm.avail_in == 0 && flush == Z_NO_FLUSH {
            return Ok(());
        }
        let reset_result = deflate::deflate_reset(&mut state.strm);
        if let Err(e) = reset_result {
            gz_error(
                state,
                Some(ZlibError::StreamError),
                Some("internal error: deflate stream corrupt".into()),
            );
            return Err(e);
        }
        state.reset_pending = false;
    }

    // Run deflate() on provided input until it produces no more output
    // or the stream is complete.
    //
    // C equivalent (gzwrite.c gz_comp):
    //
    //   ret = Z_OK;
    //   do {
    //       /* write output buffer when full or flushing */
    //       if (...) { write(...); }
    //       if (strm.avail_out == 0) { reset out_buf; }
    //       have = strm.avail_out;
    //       ret  = deflate(&strm, flush);
    //       have -= strm.avail_out;
    //   } while (have);
    //
    // In the Rust port the loop may also terminate when deflate signals
    // StreamEnd (Z_STREAM_END), because subsequent calls with unconsumed
    // input would hit the `status==Finish && avail_in!=0 → BufError`
    // check inside deflate.  Exiting on StreamEnd is semantically correct
    // — once the gzip member is complete, there is nothing left to do.
    let mut ret = ReturnCode::Ok;
    loop {
        // ── Flush output buffer to file when needed ────────────────────
        let out_pos = state.size - state.strm.avail_out as usize;

        // Decide whether to flush compressed output to the file:
        //  • output buffer is full (avail_out == 0), OR
        //  • we are flushing and the gzip stream is complete (StreamEnd).
        //  For Z_FINISH, wait until Z_STREAM_END so the complete trailer
        //  is written in one shot.
        let should_write = state.strm.avail_out == 0
            || (flush != Z_NO_FLUSH && (flush != Z_FINISH || ret == ReturnCode::StreamEnd));

        if should_write && out_pos > state.next {
            let write_result = {
                let data = &state.out_buf[state.next..out_pos];
                match state.file.as_mut() {
                    Some(f) => f.write_all(data),
                    None => Err(io::Error::new(io::ErrorKind::Other, "no file handle")),
                }
            };
            if let Err(e) = write_result {
                gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
                return Err(ZlibError::Errno);
            }
            state.next = out_pos;
        }

        // ── Exit on StreamEnd — the gzip member is complete ────────────
        if ret == ReturnCode::StreamEnd {
            break;
        }

        // ── Reset output buffer when completely full ───────────────────
        if state.strm.avail_out == 0 {
            let buf_size = state.size;
            state.strm.set_output(&mut state.out_buf[..buf_size]);
            state.next = 0;
        }

        // ── Compress ───────────────────────────────────────────────────
        let old_avail_out = state.strm.avail_out;
        let deflate_result = deflate::deflate(&mut state.strm, flush);
        match deflate_result {
            Ok(rc) => ret = rc,
            Err(ZlibError::BufError) => {
                // BufError means "no progress possible" — this is normal
                // during flush loops when all input has been consumed and
                // deflate cannot produce more output.  C zlib's gz_comp
                // only checks for Z_STREAM_ERROR; Z_BUF_ERROR naturally
                // terminates the loop via the `have == 0` exit below.
                // We break explicitly for clarity.
                break;
            }
            Err(e) => {
                gz_error(
                    state,
                    Some(ZlibError::StreamError),
                    Some("internal error: deflate stream corrupt".into()),
                );
                return Err(e);
            }
        }
        let have = old_avail_out - state.strm.avail_out;

        // deflate produced no output — nothing more to do.
        if have == 0 {
            break;
        }
    }

    // If a complete gzip member was finished, mark for reset so the
    // next write starts a new gzip member.
    if flush == Z_FINISH {
        state.reset_pending = true;
    }

    Ok(())
}

// ============================================================================
// gz_zero — Write zero bytes for seek gaps
// ============================================================================

/// Write `remaining` zero bytes to fill a seek gap.
///
/// Used when [`gz_seek`](super::open::gz_seek) moves past the current
/// position in write mode, requiring zeros to fill the gap between the
/// old and new positions.
///
/// In direct mode the zeros are written straight to the file; in
/// compressed mode they are fed through the deflate engine via
/// [`gz_comp`].
///
/// # C Reference
///
/// Port of `gzwrite.c` `gz_zero()` (lines 154–182).
///
/// # Errors
///
/// - [`ZlibError::Errno`] — file write failed (direct mode).
/// - [`ZlibError::StreamError`] / [`ZlibError::MemError`] — deflate error.
pub(crate) fn gz_zero(state: &mut GzState, mut remaining: u64) -> Result<(), ZlibError> {
    if remaining == 0 {
        return Ok(());
    }

    // Ensure buffers are allocated.
    if state.size == 0 {
        gz_init(state)?;
    }

    if state.direct {
        // Direct mode: write zeros straight to the file.
        while remaining > 0 {
            let n = core::cmp::min(remaining, state.size as u64) as usize;
            state.in_buf[..n].fill(0);
            // Use field-level borrowing to avoid aliasing &mut state +
            // &state.in_buf.
            let write_result = {
                let data = &state.in_buf[..n];
                match state.file.as_mut() {
                    Some(f) => f.write_all(data),
                    None => Err(io::Error::new(io::ErrorKind::Other, "no file handle")),
                }
            };
            if let Err(e) = write_result {
                gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
                return Err(ZlibError::Errno);
            }
            state.pos += n as u64;
            remaining -= n as u64;
        }
        return Ok(());
    }

    // Compressed mode: consume any pending input first.
    if state.strm.avail_in > 0 {
        gz_comp(state, Z_NO_FLUSH)?;
    }

    // Feed zero-filled chunks through deflate.
    let mut first = true;
    while remaining > 0 {
        let n = core::cmp::min(remaining, state.size as u64) as usize;
        if first {
            state.in_buf[..n].fill(0);
            first = false;
        }
        // Point strm input to the zero-filled region of in_buf.
        state.strm.set_input(&state.in_buf[..n]);
        gz_comp(state, Z_NO_FLUSH)?;
        // Track how much was consumed (should be all after gz_comp).
        let consumed = n - state.strm.avail_in as usize;
        state.pos += consumed as u64;
        remaining -= consumed as u64;
        if consumed == 0 {
            // Safety valve: avoid infinite loop if deflate stalls.
            break;
        }
    }
    Ok(())
}

// ============================================================================
// gz_write_internal — Internal buffered write
// ============================================================================

/// Compute how much of `in_buf` is currently occupied by pending data.
///
/// Uses pointer-to-integer arithmetic on `strm.next_in` vs the base
/// address of `in_buf` — this is safe because we never dereference the
/// raw pointer, only compare its numeric value.
#[inline]
fn in_buf_have(state: &GzState) -> usize {
    if state.strm.avail_in == 0 {
        return 0;
    }
    let base = state.in_buf.as_ptr() as usize;
    let next = state.strm.next_in as usize;
    // Guard against stale pointers that precede the buffer start.
    let offset = next.saturating_sub(base);
    offset + state.strm.avail_in as usize
}

/// Internal write function: buffers data and compresses when the buffer
/// is full.
///
/// Handles three paths:
///
/// 1. **Direct mode** — bytes go straight to the file (no compression).
/// 2. **Small writes** (< buffer size) — data is copied into `in_buf`;
///    when the buffer fills, [`gz_comp`] flushes it.
/// 3. **Large writes** (≥ buffer size) — any buffered data is flushed,
///    then the caller's buffer is fed directly to deflate.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gz_write()` (lines 188–252).
///
/// # Returns
///
/// Number of bytes written (equal to `buf.len()` on success).
fn gz_write_internal(state: &mut GzState, buf: &[u8]) -> Result<usize, ZlibError> {
    let len = buf.len();
    if len == 0 {
        return Ok(0);
    }

    // Lazy allocation on first write.
    if state.size == 0 {
        gz_init(state)?;
    }

    // Handle pending seek gap.
    if state.seek {
        state.seek = false;
        let skip = state.skip;
        state.skip = 0;
        gz_zero(state, skip)?;
    }

    // ── Direct (transparent) mode ──────────────────────────────────────
    if state.direct {
        let write_result = file_write_all(state, buf);
        if let Err(e) = write_result {
            gz_error(state, Some(ZlibError::Errno), Some(e.to_string()));
            return Err(ZlibError::Errno);
        }
        state.pos += len as u64;
        return Ok(len);
    }

    // ── Small write path: copy to in_buf, compress when full ───────────
    if len < state.size {
        let mut offset = 0usize;
        while offset < len {
            // When the buffer is empty, reset next_in to the start.
            if state.strm.avail_in == 0 {
                state.strm.next_in = state.in_buf.as_ptr();
            }

            // Compute occupied extent of in_buf.
            let have = in_buf_have(state);

            // Room left in the buffer.
            let space = state.size - have;
            let copy = core::cmp::min(space, len - offset);

            // Copy caller's data into in_buf.
            state.in_buf[have..have + copy].copy_from_slice(&buf[offset..offset + copy]);
            state.strm.avail_in += copy as u32;
            state.pos += copy as u64;
            offset += copy;

            if offset >= len {
                break; // All data buffered — done.
            }

            // Buffer is full — compress it.
            gz_comp(state, Z_NO_FLUSH)?;
        }
        return Ok(len);
    }

    // ── Large write path: compress directly from caller's buffer ───────
    // First flush any existing data in in_buf.
    if state.strm.avail_in > 0 {
        gz_comp(state, Z_NO_FLUSH)?;
    }

    // Feed the caller's buffer directly to deflate in u32-sized chunks.
    let mut offset = 0usize;
    while offset < len {
        let chunk = core::cmp::min(len - offset, u32::MAX as usize);
        state.strm.set_input(&buf[offset..offset + chunk]);
        gz_comp(state, Z_NO_FLUSH)?;
        let consumed = chunk - state.strm.avail_in as usize;
        state.pos += consumed as u64;
        offset += consumed;
        if consumed == 0 {
            // No progress — prevent infinite loop.
            break;
        }
    }

    Ok(len)
}

// ============================================================================
// Public API: gz_write
// ============================================================================

/// Write data to a gzip file.
///
/// Equivalent to C zlib's `gzwrite(gzFile file, voidpc buf, unsigned len)`.
/// Compresses the data and writes it to the underlying file.
///
/// # Arguments
///
/// * `state` — Gzip state opened in write mode.
/// * `buf` — Data to write.
///
/// # Returns
///
/// Number of uncompressed bytes written.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — file is not in write mode or has a
///   non-recoverable error.
/// - [`ZlibError::Errno`] — file write failed.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_write;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// let n = gz_write(&mut state, b"Hello, world!").unwrap();
/// assert_eq!(n, 13);
/// ```
pub fn gz_write(state: &mut GzState, buf: &[u8]) -> Result<usize, ZlibError> {
    // Check that we're writing and that there's no serious error.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }

    // Clear any previous error.
    gz_error(state, None, None);

    gz_write_internal(state, buf)
}

// ============================================================================
// Public API: gz_fwrite
// ============================================================================

/// Write data to a gzip file with `fwrite`-like semantics.
///
/// Equivalent to C zlib's
/// `gzfwrite(voidpc buf, z_size_t size, z_size_t nitems, gzFile file)`.
///
/// # Arguments
///
/// * `state` — Gzip state opened in write mode.
/// * `buf` — Data buffer containing `nitems` × `item_size` bytes.
/// * `item_size` — Size of each item in bytes.
/// * `nitems` — Number of items to write.
///
/// # Returns
///
/// Number of **complete items** written.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — overflow when computing total bytes,
///   or file not in write mode.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_fwrite;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// let data = b"AABBCCDD";
/// let items = gz_fwrite(&mut state, data, 2, 4).unwrap();
/// assert_eq!(items, 4);
/// ```
pub fn gz_fwrite(
    state: &mut GzState,
    buf: &[u8],
    item_size: usize,
    nitems: usize,
) -> Result<usize, ZlibError> {
    // Check mode and error state.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    // Compute total bytes, checking for overflow.
    let total = item_size.checked_mul(nitems).ok_or_else(|| {
        gz_error(
            state,
            Some(ZlibError::StreamError),
            Some("request does not fit in a size_t".into()),
        );
        ZlibError::StreamError
    })?;

    if total == 0 || item_size == 0 {
        return Ok(0);
    }

    // Ensure the buffer is large enough.
    let actual_len = core::cmp::min(total, buf.len());
    let written = gz_write_internal(state, &buf[..actual_len])?;

    // Return number of complete items.
    Ok(written / item_size)
}

// ============================================================================
// Public API: gz_putc
// ============================================================================

/// Write a single byte to a gzip file.
///
/// Equivalent to C zlib's `gzputc(gzFile file, int c)`.
///
/// # Optimisation
///
/// If the input buffer has room, the byte is written directly into
/// `in_buf` without invoking the full [`gz_write_internal`] pipeline,
/// avoiding function-call overhead for repeated single-byte writes.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gzputc()` (lines 307–347).
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_putc;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// let byte = gz_putc(&mut state, b'A').unwrap();
/// assert_eq!(byte, b'A');
/// ```
pub fn gz_putc(state: &mut GzState, c: u8) -> Result<u8, ZlibError> {
    // Check mode and error state.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    // Handle pending seek gap.
    if state.seek {
        state.seek = false;
        let skip = state.skip;
        state.skip = 0;
        gz_zero(state, skip)?;
    }

    // Fast path: if buffer is initialised and has room, write directly.
    if state.size > 0 && !state.direct {
        if state.strm.avail_in == 0 {
            state.strm.next_in = state.in_buf.as_ptr();
        }
        let have = in_buf_have(state);
        if have < state.size {
            state.in_buf[have] = c;
            state.strm.avail_in += 1;
            state.pos += 1;
            return Ok(c);
        }
    }

    // Slow path: use the full write pipeline.
    gz_write_internal(state, &[c])?;
    Ok(c)
}

// ============================================================================
// Public API: gz_puts
// ============================================================================

/// Write a string to a gzip file.
///
/// Equivalent to C zlib's `gzputs(gzFile file, const char *s)`.
///
/// # Returns
///
/// Number of bytes written (the byte length of the string).
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_puts;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// let n = gz_puts(&mut state, "Hello, world!").unwrap();
/// assert_eq!(n, 13);
/// ```
pub fn gz_puts(state: &mut GzState, s: &str) -> Result<usize, ZlibError> {
    // Check mode and error state.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    gz_write_internal(state, s.as_bytes())
}

// ============================================================================
// Public API: gz_printf
// ============================================================================

/// Write formatted data to a gzip file.
///
/// In Rust, this accepts a **pre-formatted** string rather than C-style
/// format arguments.  Callers should use the [`format!`] macro to prepare
/// the string before calling this function.
///
/// The C version (`gzvprintf` / `gzprintf`) uses `vsnprintf` with a
/// double-sized buffer.  In Rust, formatting is handled by `format!()` at
/// the call site, so no internal formatting buffer is needed.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gzprintf()` (lines 487–495).
///
/// # Returns
///
/// Number of bytes written.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_printf;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// let n = gz_printf(&mut state, &format!("value = {}", 42)).unwrap();
/// assert!(n > 0);
/// ```
pub fn gz_printf(state: &mut GzState, formatted: &str) -> Result<usize, ZlibError> {
    // Check mode and error state.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    gz_write_internal(state, formatted.as_bytes())
}

// ============================================================================
// Public API: gz_flush
// ============================================================================

/// Flush output to a gzip file.
///
/// Equivalent to C zlib's `gzflush(gzFile file, int flush)`.
///
/// Supported flush values:
/// - [`Z_NO_FLUSH`] — allow deflate to decide when to produce output.
/// - [`Z_PARTIAL_FLUSH`] — flush pending output (backward-compat).
/// - [`Z_SYNC_FLUSH`] — flush and emit a sync marker.
/// - [`Z_FULL_FLUSH`] — flush and reset compression state.
/// - [`Z_FINISH`] — finalize the gzip stream.
///
/// [`Z_BLOCK`] and [`Z_TREES`] are **not** supported and will return
/// [`ZlibError::StreamError`].
///
/// # C Reference
///
/// Port of `gzwrite.c` `gzflush()` (lines 603–627).
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::{gz_write, gz_flush};
/// use zlib_rs::constants::Z_SYNC_FLUSH;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// gz_write(&mut state, b"data").unwrap();
/// gz_flush(&mut state, Z_SYNC_FLUSH).unwrap();
/// ```
pub fn gz_flush(state: &mut GzState, flush: i32) -> Result<ReturnCode, ZlibError> {
    // Validate mode.
    if state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    // Validate flush mode — only subset of modes supported for gz files.
    match flush {
        Z_NO_FLUSH | Z_PARTIAL_FLUSH | Z_SYNC_FLUSH | Z_FULL_FLUSH | Z_FINISH => {
            // Supported — continue.
        }
        Z_BLOCK | Z_TREES => {
            gz_error(
                state,
                Some(ZlibError::StreamError),
                Some("flush mode not supported for gz files".into()),
            );
            return Err(ZlibError::StreamError);
        }
        _ => {
            return Err(ZlibError::StreamError);
        }
    }

    // Handle pending seek gap.
    if state.seek {
        state.seek = false;
        let skip = state.skip;
        state.skip = 0;
        gz_zero(state, skip)?;
    }

    // In direct mode there is nothing to flush (no compression buffers).
    if !state.direct {
        gz_comp(state, flush)?;
    }

    Ok(ReturnCode::Ok)
}

// ============================================================================
// Public API: gz_setparams
// ============================================================================

/// Dynamically change the compression level and strategy.
///
/// Equivalent to C zlib's `gzsetparams(gzFile file, int level, int strategy)`.
///
/// If the new parameters differ from the current ones, any buffered input
/// is first flushed via a [`Z_BLOCK`] flush, then
/// [`deflate_params`](crate::deflate::deflate_params) is called to apply
/// the change to the deflate engine.
///
/// # C Reference
///
/// Port of `gzwrite.c` `gzsetparams()` (lines 630–664).
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — file is not in (non-direct) write mode,
///   or `deflateParams` rejected the new parameters.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::write::gz_setparams;
/// use zlib_rs::constants::{Z_BEST_SPEED, Z_DEFAULT_STRATEGY};
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("output.gz"), "w").unwrap();
/// gz_setparams(&mut state, Z_BEST_SPEED, Z_DEFAULT_STRATEGY).unwrap();
/// ```
pub fn gz_setparams(
    state: &mut GzState,
    level: i32,
    strategy: i32,
) -> Result<ReturnCode, ZlibError> {
    // Must be writing and compressing (not direct mode).
    if state.mode != GzMode::Write || state.direct {
        return Err(ZlibError::StreamError);
    }
    if let Some(ref err) = state.err {
        if !state.again {
            return Err(*err);
        }
    }
    gz_error(state, None, None);

    // No change requested — return immediately.
    if level == state.level && strategy == state.strategy {
        return Ok(ReturnCode::Ok);
    }

    // Handle pending seek gap.
    if state.seek {
        state.seek = false;
        let skip = state.skip;
        state.skip = 0;
        gz_zero(state, skip)?;
    }

    // If buffers are initialized and there is pending input, flush first.
    // C: `if (strm->avail_in && gz_comp(state, Z_BLOCK) == -1) return ...;`
    if state.size > 0 && state.strm.avail_in > 0 {
        gz_comp(state, Z_BLOCK)?;
    }

    // Change deflate parameters.
    // C zlib ignores the return value of deflateParams() — it may
    // return Z_BUF_ERROR if there is pending data in the window, but
    // gzsetparams treats this as benign because subsequent gz_comp
    // calls will flush the remaining data at the new parameters.
    //
    // C: `deflateParams(strm, level, strategy);`  (return value ignored)
    if state.size > 0 {
        let _ = deflate::deflate_params(&mut state.strm, level, strategy);
    }

    // Record the new parameters.
    state.level = level;
    state.strategy = strategy;
    Ok(ReturnCode::Ok)
}
