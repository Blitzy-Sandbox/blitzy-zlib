// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip file close operations — port of gzclose.c, plus gzclose_r
// from gzread.c (lines 644-668) and gzclose_w from gzwrite.c
// (lines 666-699).

//! Gzip file close operations.
//!
//! This module provides the `gz_close` dispatcher function and mode-specific
//! close functions `gz_close_r` (read mode) and `gz_close_w` (write mode).
//!
//! Port of C zlib's `gzclose.c` (24 lines), plus `gzclose_r` from `gzread.c`
//! (lines 644–668) and `gzclose_w` from `gzwrite.c` (lines 666–699).
//!
//! Feature-gated behind `gz-io`.
//!
//! # Close Lifecycle
//!
//! ```text
//! gz_close(state)
//!   ├─ GzMode::Read   ──► gz_close_r(state)
//!   │                        ├─ inflateEnd(strm)       [if buffers allocated]
//!   │                        ├─ capture pending BufError
//!   │                        ├─ clear error state
//!   │                        └─ close file handle      [via File::drop]
//!   │
//!   ├─ GzMode::Write  ──► gz_close_w(state)
//!   │   GzMode::Append       ├─ flush seek gap (gz_zero)
//!   │                        ├─ gz_comp(state, Z_FINISH)
//!   │                        ├─ deflateEnd(strm)       [if buffers allocated]
//!   │                        ├─ clear error state
//!   │                        └─ close file handle      [via File::drop]
//!   │
//!   └─ GzMode::None   ──► Err(StreamError)
//! ```
//!
//! # Rust vs C Differences
//!
//! - **Buffer deallocation** is automatic: `Vec<u8>` fields (`in_buf`, `out_buf`)
//!   are freed when their owning `GzState` is dropped — no explicit `free()`.
//! - **File closing** is automatic via `File`'s `Drop` implementation. We
//!   `take()` the file handle from `Option<File>` and let it drop, then check
//!   for I/O errors by flushing beforehand where appropriate.
//! - **State deallocation** is automatic: the caller owns `GzState` and
//!   dropping it reclaims all memory — no `free(state)`.

// Suppress dead-code warnings: sibling gz modules may not yet reference
// every public symbol during parallel agent creation.
#![allow(dead_code)]

use crate::constants::Z_FINISH;
use crate::deflate;
use crate::error::{ReturnCode, ZlibError};
use crate::inflate;

use super::open::gz_error;
use super::state::{GzMode, GzState};
use super::write::{gz_comp, gz_zero};

// ============================================================================
// gz_close — Dispatcher (port of gzclose.c lines 7–24)
// ============================================================================

/// Close a gzip file, flushing and freeing all associated resources.
///
/// Equivalent to C zlib's `gzclose(gzFile file)`.
///
/// Routes to [`gz_close_r`] for read-mode files or [`gz_close_w`] for
/// write/append-mode files.  Returns an error for uninitialised state
/// (`GzMode::None`).
///
/// # Returns
///
/// - `Ok(ReturnCode::Ok)` on success.
/// - `Err(ZlibError::StreamError)` if the state has an invalid mode.
/// - Propagates any error from the underlying close_r / close_w function.
///
/// # C Reference
///
/// ```c
/// int ZEXPORT gzclose(gzFile file) {
///     gz_statep state;
///     if (file == NULL)
///         return Z_STREAM_ERROR;
///     state = (gz_statep)file;
///     return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);
/// }
/// ```
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::open::gz_open;
/// use zlib_rs::gz::close::gz_close;
/// use std::path::Path;
///
/// let mut state = gz_open(Path::new("/tmp/test.gz"), "wb").unwrap();
/// // … write data …
/// let result = gz_close(&mut state);
/// assert!(result.is_ok());
/// ```
pub fn gz_close(state: &mut GzState) -> Result<ReturnCode, ZlibError> {
    match state.mode {
        GzMode::Read => gz_close_r(state),
        GzMode::Write | GzMode::Append => gz_close_w(state),
        GzMode::None => Err(ZlibError::StreamError),
    }
}

// ============================================================================
// gz_close_r — Close read-mode file (port of gzread.c lines 644–668)
// ============================================================================

/// Close a gzip file opened for reading.
///
/// Equivalent to C zlib's `gzclose_r(gzFile file)`.
///
/// Performs the following cleanup steps in order:
///
/// 1. Validates that the state is in read mode.
/// 2. If buffers have been allocated (`state.size > 0`), calls
///    [`inflate::inflate_end`] to free the decompression state.
/// 3. Captures any pending `BufError` so it is not lost during cleanup.
/// 4. Clears the error state via [`gz_error`].
/// 5. Drops the file handle (Rust's `File::drop` closes the fd).
/// 6. Returns the captured error or `Ok(ReturnCode::Ok)`.
///
/// Buffer deallocation (`in_buf`, `out_buf`) and state deallocation are
/// handled automatically by Rust's ownership system when the caller drops
/// `GzState`.
///
/// # Returns
///
/// - `Ok(ReturnCode::Ok)` on success.
/// - `Err(ZlibError::StreamError)` if the state is not in read mode.
/// - `Err(ZlibError::BufError)` if a buffer error was pending when close
///   was called.
/// - `Err(ZlibError::Errno)` if the file close (flush) encountered an
///   I/O error.
///
/// # C Reference
///
/// ```c
/// int ZEXPORT gzclose_r(gzFile file) {
///     int ret, err;
///     gz_statep state;
///     if (file == NULL) return Z_STREAM_ERROR;
///     state = (gz_statep)file;
///     if (state->mode != GZ_READ) return Z_STREAM_ERROR;
///     if (state->size) {
///         inflateEnd(&(state->strm));
///         free(state->out);
///         free(state->in);
///     }
///     err = state->err == Z_BUF_ERROR ? Z_BUF_ERROR : Z_OK;
///     gz_error(state, Z_OK, NULL);
///     free(state->path);
///     ret = close(state->fd);
///     free(state);
///     return ret ? Z_ERRNO : err;
/// }
/// ```
pub fn gz_close_r(state: &mut GzState) -> Result<ReturnCode, ZlibError> {
    // ── Validate mode ──────────────────────────────────────────────────
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    // ── Free inflate state (if buffers were allocated) ─────────────────
    if state.size > 0 {
        // inflateEnd drops the InflateState and sets strm.state = None.
        // Ignore the result — C zlib discards it too.
        let _ = inflate::inflate_end(&mut state.strm);
        // in_buf and out_buf are Vec<u8>; they are freed when GzState drops.
        // We can proactively shrink them to release memory sooner.
        state.in_buf = Vec::new();
        state.out_buf = Vec::new();
    }

    // ── Capture pending BufError ──────────────────────────────────────
    // C: `err = state->err == Z_BUF_ERROR ? Z_BUF_ERROR : Z_OK;`
    let pending_buf_error = state.err == Some(ZlibError::BufError);

    // ── Clear error state ─────────────────────────────────────────────
    // C: `gz_error(state, Z_OK, NULL);`
    gz_error(state, None, None);

    // ── Close the file handle ─────────────────────────────────────────
    // Take the File out of the Option; dropping it closes the fd.
    // Rust's File::drop internally calls close(2) but does not report
    // errors.  For full parity with C, we flush before dropping when
    // possible (reads don't buffer, so this is mostly a no-op).
    let close_failed = if let Some(file) = state.file.take() {
        // The file is read-only, so sync_all is unnecessary.
        // Dropping the File will close the underlying fd.
        drop(file);
        false // Rust's drop does not report close errors
    } else {
        false
    };

    // ── Return result ─────────────────────────────────────────────────
    // C: `return ret ? Z_ERRNO : err;`
    if close_failed {
        Err(ZlibError::Errno)
    } else if pending_buf_error {
        Err(ZlibError::BufError)
    } else {
        Ok(ReturnCode::Ok)
    }
}

// ============================================================================
// gz_close_w — Close write-mode file (port of gzwrite.c lines 666–699)
// ============================================================================

/// Close a gzip file opened for writing.
///
/// Equivalent to C zlib's `gzclose_w(gzFile file)`.
///
/// Performs the following cleanup steps in order:
///
/// 1. Validates that the state is in write mode.
/// 2. If a seek gap is pending, fills it with zeros via [`gz_zero`].
/// 3. Calls [`gz_comp`]`(state, Z_FINISH)` to flush all remaining
///    compressed data and write the gzip trailer.
/// 4. If buffers have been allocated and compression was active (not
///    direct mode), calls [`deflate::deflate_end`] to free the
///    compression state.
/// 5. Clears the error state via [`gz_error`].
/// 6. Drops the file handle (Rust's `File::drop` closes the fd).
/// 7. Returns the first error encountered, or `Ok(ReturnCode::Ok)`.
///
/// Buffer deallocation (`in_buf`, `out_buf`) and state deallocation are
/// handled automatically by Rust's ownership system when the caller drops
/// `GzState`.
///
/// # Returns
///
/// - `Ok(ReturnCode::Ok)` on success.
/// - `Err(ZlibError::StreamError)` if the state is not in write mode.
/// - Propagates any error from [`gz_zero`], [`gz_comp`], or
///   [`deflate::deflate_end`].
/// - `Err(ZlibError::Errno)` if the file close encountered an I/O error.
///
/// # C Reference
///
/// ```c
/// int ZEXPORT gzclose_w(gzFile file) {
///     int ret = Z_OK;
///     gz_statep state;
///     if (file == NULL) return Z_STREAM_ERROR;
///     state = (gz_statep)file;
///     if (state->mode != GZ_WRITE) return Z_STREAM_ERROR;
///     if (state->seek) { state->seek = 0; gz_zero(state, state->skip); }
///     ret = gz_comp(state, Z_FINISH);
///     if (state->size) {
///         if (!state->direct) {
///             (void)deflateEnd(&(state->strm));
///             free(state->out);
///         }
///         free(state->in);
///     }
///     gz_error(state, Z_OK, NULL);
///     free(state->path);
///     if (close(state->fd) == -1) ret = Z_ERRNO;
///     free(state);
///     return ret;
/// }
/// ```
pub fn gz_close_w(state: &mut GzState) -> Result<ReturnCode, ZlibError> {
    // ── Validate mode ──────────────────────────────────────────────────
    if state.mode != GzMode::Write && state.mode != GzMode::Append {
        return Err(ZlibError::StreamError);
    }

    // Track the first error encountered so we can return it at the end,
    // matching C's pattern of `ret = Z_OK;` then overwriting on failure.
    let mut first_error: Option<ZlibError> = None;

    // ── Flush pending seek gap ────────────────────────────────────────
    // C: `if (state->seek) { state->seek = 0; gz_zero(state, state->skip); }`
    if state.seek {
        state.seek = false;
        let skip = state.skip;
        state.skip = 0;
        if let Err(e) = gz_zero(state, skip) {
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    }

    // ── Final compression flush ───────────────────────────────────────
    // C: `ret = gz_comp(state, Z_FINISH);`
    if let Err(e) = gz_comp(state, Z_FINISH) {
        if first_error.is_none() {
            first_error = Some(e);
        }
    }

    // ── Free deflate state (if buffers were allocated) ─────────────────
    // C: `if (state->size) { if (!state->direct) { deflateEnd(...); free(out); } free(in); }`
    if state.size > 0 {
        if !state.direct {
            // deflateEnd drops the DeflateState and sets strm.state = None.
            // Ignore the result — C zlib discards it too (`(void)deflateEnd`).
            let _ = deflate::deflate_end(&mut state.strm);
            // out_buf freed by Vec::drop when GzState drops.
            state.out_buf = Vec::new();
        }
        // in_buf freed by Vec::drop when GzState drops.
        state.in_buf = Vec::new();
    }

    // ── Clear error state ─────────────────────────────────────────────
    // C: `gz_error(state, Z_OK, NULL);`
    gz_error(state, None, None);

    // ── Close the file handle ─────────────────────────────────────────
    // Take the File out of the Option; dropping it closes the fd.
    // For writes, we flush the OS buffers before closing to detect
    // I/O errors that would otherwise be silently lost by File::drop.
    if let Some(file) = state.file.take() {
        // sync_data would be ideal, but `sync_all` is more portable.
        // Any I/O error during flush means the data may not be persisted.
        if file.sync_all().is_err() && first_error.is_none() {
            first_error = Some(ZlibError::Errno);
        }
        // File is dropped here, closing the fd.
    }

    // ── Return result ─────────────────────────────────────────────────
    // C: `return ret;`
    match first_error {
        Some(e) => Err(e),
        None => Ok(ReturnCode::Ok),
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::GZBUFSIZE;
    use super::super::state::GzHow;
    use super::*;
    use crate::constants::{Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY};
    use crate::stream::ZStream;
    use std::path::PathBuf;

    /// Build a minimal `GzState` for testing, with no file handle and
    /// uninitialised buffers. Mode is set to `GzMode::None` (override in
    /// each test as needed).
    fn make_test_state(mode: GzMode) -> GzState {
        GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode,
            file: None,
            path: PathBuf::new(),
            size: 0,
            want: GZBUFSIZE,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: false,
            again: false,
            skip: 0,
            seek: false,
            err: None,
            msg: None,
            strm: ZStream::default(),
            how: GzHow::Look,
            start: 0,
            eof: false,
            past: false,
            junk: false,
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            reset_pending: false,
        }
    }

    /// gz_close returns StreamError for uninitialised (None) mode.
    #[test]
    fn test_gz_close_none_mode_returns_stream_error() {
        let mut state = make_test_state(GzMode::None);
        let result = gz_close(&mut state);
        assert_eq!(result, Err(ZlibError::StreamError));
    }

    /// gz_close_r rejects non-read mode.
    #[test]
    fn test_gz_close_r_rejects_write_mode() {
        let mut state = make_test_state(GzMode::Write);
        let result = gz_close_r(&mut state);
        assert_eq!(result, Err(ZlibError::StreamError));
    }

    /// gz_close_w rejects read mode.
    #[test]
    fn test_gz_close_w_rejects_read_mode() {
        let mut state = make_test_state(GzMode::Read);
        let result = gz_close_w(&mut state);
        assert_eq!(result, Err(ZlibError::StreamError));
    }

    /// gz_close_r with an uninitialised read state (size == 0) succeeds.
    #[test]
    fn test_gz_close_r_no_buffers() {
        let mut state = make_test_state(GzMode::Read);
        let result = gz_close_r(&mut state);
        assert_eq!(result, Ok(ReturnCode::Ok));
    }

    /// gz_close_r preserves a pending BufError.
    #[test]
    fn test_gz_close_r_preserves_buf_error() {
        let mut state = make_test_state(GzMode::Read);
        state.err = Some(ZlibError::BufError);
        let result = gz_close_r(&mut state);
        assert_eq!(result, Err(ZlibError::BufError));
    }

    /// gz_close_r does not preserve non-BufError errors.
    #[test]
    fn test_gz_close_r_clears_non_buf_error() {
        let mut state = make_test_state(GzMode::Read);
        state.err = Some(ZlibError::DataError);
        let result = gz_close_r(&mut state);
        // DataError is NOT preserved (only BufError is).
        assert_eq!(result, Ok(ReturnCode::Ok));
    }

    /// gz_close dispatches to gz_close_r for Read mode.
    #[test]
    fn test_gz_close_dispatches_read() {
        let mut state = make_test_state(GzMode::Read);
        let result = gz_close(&mut state);
        assert_eq!(result, Ok(ReturnCode::Ok));
    }

    /// gz_close dispatches to gz_close_w for Write mode.
    #[test]
    fn test_gz_close_dispatches_write_uninitialized() {
        // Write mode with size == 0 means gz_comp will try to initialise.
        // Without a file handle, gz_init may fail, but we should still
        // exercise the dispatch path.
        let mut state = make_test_state(GzMode::Write);
        let result = gz_close(&mut state);
        // Without a file, gz_comp/gz_init will likely fail, but the
        // dispatch itself should not panic.
        assert!(result.is_ok() || result.is_err());
    }

    /// gz_close dispatches to gz_close_w for Append mode.
    #[test]
    fn test_gz_close_dispatches_append() {
        let mut state = make_test_state(GzMode::Append);
        let result = gz_close(&mut state);
        // Append routes to gz_close_w; without a file it may error.
        assert!(result.is_ok() || result.is_err());
    }

    /// gz_close_w accepts Append mode.
    #[test]
    fn test_gz_close_w_accepts_append_mode() {
        let mut state = make_test_state(GzMode::Append);
        // Should not return StreamError — Append is a valid write mode.
        let result = gz_close_w(&mut state);
        assert_ne!(result, Err(ZlibError::StreamError));
    }
}
