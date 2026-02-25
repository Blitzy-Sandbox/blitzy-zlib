//! Gzip file open, seek, tell, and error management operations.
//!
//! This module provides the core gzip file lifecycle operations including
//! opening files (by path or file handle), setting buffer sizes, seeking
//! within compressed streams, position reporting, and error management.
//!
//! Port of C zlib's `gzlib.c` (610 lines).
//!
//! Feature-gated behind `gz-io`.

// During parallel module creation sibling modules (read, write, close) may
// not yet reference every public symbol — suppress dead-code warnings.
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::constants::{
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FILTERED, Z_FIXED, Z_HUFFMAN_ONLY, Z_RLE,
};
use crate::error::ZlibError;
use crate::inflate::inflate_reset;

use super::state::{GzHow, GzMode, GzState, GZBUFSIZE};

// ========================================================================
// Seek whence constants (matching POSIX SEEK_SET / SEEK_CUR / SEEK_END)
// ========================================================================

/// Seek from start of stream. Equivalent to POSIX `SEEK_SET`.
pub const SEEK_SET: i32 = 0;

/// Seek from current position. Equivalent to POSIX `SEEK_CUR`.
pub const SEEK_CUR: i32 = 1;

/// Seek from end of stream. Equivalent to POSIX `SEEK_END` (**not** supported
/// for gzip streams).
pub const SEEK_END: i32 = 2;

// ========================================================================
// Internal: mode string parser
// ========================================================================

/// Intermediate result produced by [`parse_mode_string`].
struct ParsedMode {
    mode: GzMode,
    level: i32,
    strategy: i32,
    /// 0 = default, 1 = 'T' (transparent/direct), -1 = 'G' (gzip-only)
    direct_flag: i32,
}

/// Parse a C-style gzip mode string into structured parameters.
///
/// Supports all mode characters recognised by C zlib's `gz_open`:
///
/// | Char | Meaning |
/// |------|---------|
/// | `'r'` | Read mode |
/// | `'w'` | Write mode (create/truncate) |
/// | `'a'` | Append mode (create/append) |
/// | `'0'`–`'9'` | Compression level |
/// | `'f'` | `Z_FILTERED` strategy |
/// | `'h'` | `Z_HUFFMAN_ONLY` strategy |
/// | `'R'` | `Z_RLE` strategy |
/// | `'F'` | `Z_FIXED` strategy |
/// | `'T'` | Transparent write (no compression) |
/// | `'G'` | Force gzip-only read |
/// | `'b'` | Binary (ignored) |
/// | `'+'` | Read+write – **not supported** (returns error) |
///
/// Unknown characters are silently ignored, matching C behaviour.
fn parse_mode_string(mode: &str) -> Result<ParsedMode, ZlibError> {
    let mut result = ParsedMode {
        mode: GzMode::None,
        level: Z_DEFAULT_COMPRESSION,
        strategy: Z_DEFAULT_STRATEGY,
        direct_flag: 0,
    };

    for ch in mode.chars() {
        match ch {
            'r' => result.mode = GzMode::Read,
            'w' => result.mode = GzMode::Write,
            'a' => result.mode = GzMode::Append,
            '0'..='9' => result.level = (ch as i32) - ('0' as i32),
            'f' => result.strategy = Z_FILTERED,
            'h' => result.strategy = Z_HUFFMAN_ONLY,
            'R' => result.strategy = Z_RLE,
            'F' => result.strategy = Z_FIXED,
            'T' => result.direct_flag = 1,
            'G' => result.direct_flag = -1,
            '+' => return Err(ZlibError::StreamError),
            // 'b' binary, 'e' O_CLOEXEC, 'x' O_EXCL, 'N' O_NONBLOCK – ignored
            'b' | 'e' | 'x' | 'N' => {}
            _ => {} // unknown chars silently ignored (C compat)
        }
    }

    // A mode (r/w/a) is mandatory.
    if result.mode == GzMode::None {
        return Err(ZlibError::StreamError);
    }

    // Validate direct_flag / mode combinations (gzlib.c lines 179-197).
    match result.mode {
        GzMode::Read if result.direct_flag == 1 => {
            // 'T' cannot force transparent read
            return Err(ZlibError::StreamError);
        }
        GzMode::Write | GzMode::Append if result.direct_flag == -1 => {
            // 'G' (gzip-only) has no meaning when writing
            return Err(ZlibError::StreamError);
        }
        _ => {}
    }

    Ok(result)
}

// ========================================================================
// gz_reset — Reset shared state (gzlib.c lines 69-84)
// ========================================================================

/// Reset gzip state for a new read/write session.
///
/// Resets all shared and mode-specific bookkeeping fields to their initial
/// values.  Called by [`gz_open`] after state construction, by
/// [`gz_rewind`] when seeking back to the file start, and internally when
/// re-initialising after error recovery.
///
/// # C Reference
///
/// Direct port of `gzlib.c` `gz_reset()` (lines 69-84):
///
/// ```c
/// local void gz_reset(gz_statep state) {
///     state->x.have = 0;
///     if (state->mode == GZ_READ) {
///         state->eof = 0;  state->past = 0;
///         state->how = LOOK;  state->junk = -1;
///     } else
///         state->reset = 0;
///     state->again = 0;
///     state->skip = 0;
///     gz_error(state, Z_OK, NULL);
///     state->x.pos = 0;
///     state->strm.avail_in = 0;
/// }
/// ```
pub(crate) fn gz_reset(state: &mut GzState) {
    // No output data available.
    state.have = 0;
    state.next = 0;

    if state.mode == GzMode::Read {
        // Read-specific resets.
        state.eof = false;
        state.past = false;
        state.how = GzHow::Look;
        state.junk = false; // C: junk = -1 (sentinel "first member")
    } else {
        // Write-specific resets.
        state.reset_pending = false;
    }

    // Common resets.
    state.again = false;
    state.seek = false;
    state.skip = 0;

    // Clear error (inline equivalent of gz_error(state, Z_OK, NULL)).
    state.err = None;
    state.msg = None;

    // Reset position tracking and input.
    state.pos = 0;
    state.strm.avail_in = 0;
}

// ========================================================================
// gz_open — Open gzip file by path (gzlib.c lines 87-295)
// ========================================================================

/// Open a gzip file for reading, writing, or appending.
///
/// Parses the mode string, opens the file with appropriate flags, creates
/// and initialises the [`GzState`], and returns it ready for I/O.
///
/// # Mode String
///
/// The mode string follows C zlib conventions:
///
/// | Character | Meaning |
/// |-----------|---------|
/// | `'r'` | Open for reading (decompression) |
/// | `'w'` | Open for writing (compression), truncating existing file |
/// | `'a'` | Open for appending (compression), preserving existing data |
/// | `'0'`–`'9'` | Compression level (0 = none, 9 = best) |
/// | `'f'` | Filtered compression strategy |
/// | `'h'` | Huffman-only compression strategy |
/// | `'R'` | RLE compression strategy |
/// | `'F'` | Fixed Huffman codes strategy |
/// | `'T'` | Transparent write (no compression, write-mode only) |
/// | `'b'` | Binary mode (ignored; always binary in Rust) |
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — invalid mode string.
/// - [`ZlibError::Errno`] — file could not be opened.
pub fn gz_open(path: &Path, mode: &str) -> Result<GzState, ZlibError> {
    let parsed = parse_mode_string(mode)?;

    // Open with flags matching the requested mode.
    let file = match parsed.mode {
        GzMode::Read => OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|_| ZlibError::Errno)?,
        GzMode::Write => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|_| ZlibError::Errno)?,
        GzMode::Append => OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| ZlibError::Errno)?,
        GzMode::None => return Err(ZlibError::StreamError),
    };

    gz_open_common(file, path.to_path_buf(), parsed)
}

// ========================================================================
// gz_dopen — Open from existing File handle (gzlib.c lines 297-312)
// ========================================================================

/// Open a gzip file from an existing [`File`] handle.
///
/// Takes ownership of the `File`. Equivalent to C zlib's
/// `gzdopen(int fd, const char *mode)`.
///
/// The file must already be opened with permissions matching the mode
/// string (e.g., read permission for `"r"`, write for `"w"`).
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — invalid mode string.
pub fn gz_dopen(file: File, mode: &str) -> Result<GzState, ZlibError> {
    let parsed = parse_mode_string(mode)?;
    gz_open_common(file, PathBuf::from("<fd>"), parsed)
}

/// Common initialisation logic shared by [`gz_open`] and [`gz_dopen`].
///
/// Creates the [`GzState`], applies compression parameters from the parsed
/// mode string, handles append-mode seek-to-end, records the start
/// position for read-mode rewind support, and calls [`gz_reset`] to
/// initialise remaining fields.
fn gz_open_common(
    mut file: File,
    path: PathBuf,
    parsed: ParsedMode,
) -> Result<GzState, ZlibError> {
    let mut effective_mode = parsed.mode;

    // For append mode: the file was opened with O_APPEND semantics.
    // Seek to end so that gz_offset() reports the correct compressed
    // position, then normalise mode to Write for simpler checks.
    if effective_mode == GzMode::Append {
        file.seek(SeekFrom::End(0)).map_err(|_| ZlibError::Errno)?;
        effective_mode = GzMode::Write;
    }

    // Create state with the effective mode.
    let mut state = GzState::new(effective_mode, file, path);

    // Apply parsed compression parameters.
    state.level = parsed.level;
    state.strategy = parsed.strategy;
    state.direct = parsed.direct_flag == 1; // 'T' for transparent write
    state.want = GZBUFSIZE;

    // For read mode: record the start position so gz_rewind can seek back.
    // If the position query fails (e.g., pipe / stdin), default to 0.
    if effective_mode == GzMode::Read {
        if let Some(ref mut f) = state.file {
            state.start = f.stream_position().unwrap_or(0);
        }
    }

    // Initialise all remaining fields.
    gz_reset(&mut state);

    Ok(state)
}

// ========================================================================
// gz_buffer — Set buffer size (gzlib.c lines 322-343)
// ========================================================================

/// Set the internal buffer size for gzip I/O.
///
/// Must be called after opening and **before** the first read or write
/// operation.  The specified size is used for the input buffer; the output
/// buffer may be double this size for read-mode look-ahead.
///
/// # Arguments
///
/// * `state` — Gzip state (must not have started I/O yet).
/// * `size` — Desired buffer size in bytes.  Clamped to a minimum of 8.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — mode is invalid, I/O has already
///   started (`state.size != 0`), or the size would overflow when doubled.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzbuffer()` (lines 322-343).
pub fn gz_buffer(state: &mut GzState, size: u32) -> Result<(), ZlibError> {
    // Validate mode.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }

    // Cannot change buffer size after I/O has started.
    if state.size != 0 {
        return Err(ZlibError::StreamError);
    }

    // The buffer must be doubleable (C: `if ((size << 1) < size) return -1`).
    if size.checked_mul(2).is_none() {
        return Err(ZlibError::StreamError);
    }

    // Minimum of 8 for well-behaved flushing (gzlib.c line 340).
    state.want = size.max(8) as usize;
    Ok(())
}

// ========================================================================
// gz_rewind — Rewind to start (gzlib.c lines 346-364)
// ========================================================================

/// Rewind a read-mode gzip file to the beginning.
///
/// Equivalent to `gz_seek(state, 0, SEEK_SET)` but also clears the error
/// and EOF state.  Only valid for files opened in read mode.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — file is not in read mode, or has a
///   fatal error (anything other than `BufError`).
/// - [`ZlibError::Errno`] — the underlying file seek failed.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzrewind()` (lines 346-364).
pub fn gz_rewind(state: &mut GzState) -> Result<(), ZlibError> {
    // Must be in read mode.
    if state.mode != GzMode::Read {
        return Err(ZlibError::StreamError);
    }

    // Allow rewind when no error, or when only BufError (non-fatal).
    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError {
            return Err(ZlibError::StreamError);
        }
    }

    // Proactively reset inflate state if it has been initialised,
    // rather than relying on lazy re-init via gz_look.
    if state.size != 0 && state.strm.state.is_inflate() {
        let _ = inflate_reset(&mut state.strm);
    }

    // Seek the underlying file back to the saved start position.
    let start = state.start;
    if let Some(ref mut file) = state.file {
        file.seek(SeekFrom::Start(start))
            .map_err(|_| ZlibError::Errno)?;
    } else {
        return Err(ZlibError::Errno);
    }

    // Re-initialise all state fields.
    gz_reset(state);

    Ok(())
}

// ========================================================================
// gz_seek — Seek within gzip stream (gzlib.c lines 367-443)
// ========================================================================

/// Seek within a gzip stream.
///
/// Supports seeking by absolute position (`SEEK_SET = 0`) or relative to
/// the current position (`SEEK_CUR = 1`).  Seeking from end
/// (`SEEK_END = 2`) is **not** supported because the uncompressed size
/// cannot be determined without decompressing the entire stream.
///
/// # Read Mode
///
/// - **Forward seek** — data is read and discarded to advance.  When the
///   stream is in raw copy mode ([`GzHow::Copy`]), the underlying file is
///   seeked directly.
/// - **Backward seek** — the file is rewound to the start, then data is
///   read and discarded up to the target position.
///
/// # Write Mode
///
/// - Only **forward** seeks are supported.  The gap is filled with zero
///   bytes on the next write operation.
///
/// # Arguments
///
/// * `state` — Gzip state.
/// * `offset` — Byte offset (interpretation depends on `whence`).
/// * `whence` — `0` (`SEEK_SET`) or `1` (`SEEK_CUR`).
///
/// # Returns
///
/// The new position in the uncompressed stream on success.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — invalid mode, unsupported `whence`
///   (`SEEK_END`), or backward seek in write mode.
/// - [`ZlibError::Errno`] — underlying file seek failed.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzseek64()` (lines 367-435) and `gzseek()`
/// (lines 438-443).
pub fn gz_seek(state: &mut GzState, offset: i64, whence: i32) -> Result<u64, ZlibError> {
    // Validate mode.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }

    // Fail on non-recoverable error.
    if let Some(ref err) = state.err {
        if *err != ZlibError::BufError {
            return Err(ZlibError::StreamError);
        }
    }

    // Only SEEK_SET and SEEK_CUR are supported.
    if whence != SEEK_SET && whence != SEEK_CUR {
        return Err(ZlibError::StreamError);
    }

    // ---- Normalise offset to a SEEK_CUR-relative value. ----
    let mut offset: i64 = if whence == SEEK_SET {
        // Convert absolute target to relative.
        offset - state.pos as i64
    } else {
        // SEEK_CUR: fold any pending skip into the new offset, then
        // clear the old skip — the end of this function sets a new one.
        let adj = if state.past { 0i64 } else { state.skip as i64 };
        state.skip = 0;
        state.seek = false;
        offset + adj
    };

    // ---- Optimisation: raw COPY area direct seek (gzlib.c lines 393-407). ----
    if state.mode == GzMode::Read
        && state.how == GzHow::Copy
        && (state.pos as i64 + offset) >= 0
    {
        let seek_delta = offset - state.have as i64;
        if let Some(ref mut file) = state.file {
            file.seek(SeekFrom::Current(seek_delta))
                .map_err(|_| ZlibError::Errno)?;
        } else {
            return Err(ZlibError::Errno);
        }
        state.have = 0;
        state.next = 0;
        state.eof = false;
        state.past = false;
        state.seek = false;
        state.skip = 0;
        state.err = None;
        state.msg = None;
        state.strm.avail_in = 0;
        state.pos = (state.pos as i64 + offset) as u64;
        return Ok(state.pos);
    }

    // ---- Backward seek: rewind then forward-skip (gzlib.c lines 409-416). ----
    if offset < 0 {
        if state.mode != GzMode::Read {
            return Err(ZlibError::StreamError);
        }
        // Convert relative-backward offset to absolute target.
        let abs_target = state.pos as i64 + offset;
        if abs_target < 0 {
            return Err(ZlibError::StreamError);
        }
        gz_rewind(state)?;
        // After rewind pos == 0; treat the absolute target as forward skip.
        offset = abs_target;
    }

    // ---- Skip past buffered output data (read mode only). ----
    if state.mode == GzMode::Read {
        let skip_from_buf = (state.have as u64).min(offset as u64) as usize;
        state.have -= skip_from_buf;
        state.next += skip_from_buf;
        state.pos += skip_from_buf as u64;
        offset -= skip_from_buf as i64;
    }

    // ---- Record deferred skip for next read/write operation. ----
    state.skip = offset as u64;
    state.seek = offset > 0;
    Ok(state.pos + offset as u64)
}

// ========================================================================
// gz_tell — Report uncompressed position (gzlib.c lines 446-466)
// ========================================================================

/// Return the current position in the uncompressed data stream.
///
/// The returned value includes any pending seek offset that has not yet
/// been materialised, matching the position reported immediately after a
/// successful [`gz_seek`] call.
///
/// # C Reference
///
/// Port of `gzlib.c` `gztell64()` (lines 446-458) and `gztell()`
/// (lines 461-466).
pub fn gz_tell(state: &GzState) -> u64 {
    state.pos + if state.past { 0 } else { state.skip }
}

// ========================================================================
// gz_offset — Report compressed file offset (gzlib.c lines 469-495)
// ========================================================================

/// Return the current byte offset in the compressed (on-disk) file.
///
/// For read mode the returned value subtracts any buffered input that has
/// been read from the file but not yet consumed by the inflate engine.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — mode is invalid.
/// - [`ZlibError::Errno`] — file position cannot be determined.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzoffset64()` (lines 469-487) and `gzoffset()`
/// (lines 490-495).
pub fn gz_offset(state: &mut GzState) -> Result<i64, ZlibError> {
    // Validate mode.
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return Err(ZlibError::StreamError);
    }

    let file_pos = if let Some(ref mut file) = state.file {
        file.stream_position().map_err(|_| ZlibError::Errno)? as i64
    } else {
        return Err(ZlibError::Errno);
    };

    // For reading: subtract buffered input not yet consumed by inflate.
    if state.mode == GzMode::Read {
        Ok(file_pos - state.strm.avail_in as i64)
    } else {
        Ok(file_pos)
    }
}

// ========================================================================
// gz_eof — Check for end of file (gzlib.c lines 498-510)
// ========================================================================

/// Check whether the end of file has been reached while reading.
///
/// Returns `true` if the caller has attempted to read past the end of the
/// uncompressed data.  Always returns `false` for write-mode files.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzeof()` (lines 498-510).
pub fn gz_eof(state: &GzState) -> bool {
    state.mode == GzMode::Read && state.past
}

// ========================================================================
// gz_error_msg — Get error information (gzlib.c lines 513-528)
// ========================================================================

/// Get the last error code and message for this gzip file.
///
/// Returns a tuple of optional references:
///
/// * The error variant, or `None` if no error.
/// * The error message, or `None` if no message is available.
///
/// For [`ZlibError::MemError`], the message is always `"out of memory"`
/// regardless of the stored value, matching C zlib behaviour.
///
/// # C Reference
///
/// Port of `gzlib.c` `gzerror()` (lines 513-528).
pub fn gz_error_msg(state: &GzState) -> (Option<&ZlibError>, Option<&str>) {
    let err_ref = state.err.as_ref();
    let msg_ref = if state.err == Some(ZlibError::MemError) {
        Some("out of memory")
    } else {
        state.msg.as_deref()
    };
    (err_ref, msg_ref)
}

// ========================================================================
// gz_clearerr — Clear error and EOF state (gzlib.c lines 531-547)
// ========================================================================

/// Clear the error and end-of-file state for this gzip file.
///
/// After calling this function [`gz_eof`] returns `false` and
/// [`gz_error_msg`] returns `(None, None)`, allowing the caller to retry
/// operations blocked by a transient error (e.g., [`ZlibError::BufError`]).
///
/// # C Reference
///
/// Port of `gzlib.c` `gzclearerr()` (lines 531-547).
pub fn gz_clearerr(state: &mut GzState) {
    // Clear EOF indicators (read mode only).
    if state.mode == GzMode::Read {
        state.eof = false;
        state.past = false;
    }

    // Clear error state.
    state.err = None;
    state.msg = None;
}

// ========================================================================
// gz_error — Internal error setter (gzlib.c lines 555-590)
// ========================================================================

/// Set the error state for a gzip file.
///
/// Internal helper used throughout the `gz` module to record errors on the
/// [`GzState`].  When `err` is `None` the error is cleared.  For
/// [`ZlibError::Errno`] errors without an explicit message the current OS
/// error string is captured automatically.
///
/// If a fatal error occurs (anything other than `None` or `BufError`) and
/// no stalled I/O is pending (`!state.again`), the output buffer is marked
/// empty so that subsequent read attempts fail immediately.
///
/// # Arguments
///
/// * `state` — Gzip state to update.
/// * `err` — Error to set, or `None` to clear.
/// * `msg` — Optional error message.  For `Errno` errors without a
///   message the OS error string is used.
///
/// # C Reference
///
/// Port of `gzlib.c` `gz_error()` (lines 555-590).
pub(crate) fn gz_error(state: &mut GzState, err: Option<ZlibError>, msg: Option<String>) {
    // Clear any previous message.
    state.msg = None;

    // If clearing the error (Z_OK equivalent), reset both fields.
    if err.is_none() {
        state.err = None;
        return;
    }

    let err_val = err.unwrap();

    // Fatal error (not BufError) with no stalled I/O → clear output buffer
    // so that gzgetc() and similar fast paths fail immediately.
    if err_val != ZlibError::BufError && !state.again {
        state.have = 0;
    }

    // Store the error code.
    state.err = Some(err_val);

    // For memory errors don't allocate a message (we might be OOM).
    if err_val == ZlibError::MemError {
        return;
    }

    // Build the error message.
    let message = if let Some(m) = msg {
        m
    } else if err_val == ZlibError::Errno {
        // Capture the OS error string for errno-type errors.
        io::Error::last_os_error().to_string()
    } else {
        return;
    };

    // Prefix message with path (C: "<path>: <msg>").
    let path_str = state.path.display().to_string();
    if path_str.is_empty() || path_str == "<fd>" {
        state.msg = Some(message);
    } else {
        state.msg = Some(format!("{}: {}", path_str, message));
    }
}

// ========================================================================
// Unit tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("zlib_rs_open_test");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    // ---- parse_mode_string ----

    #[test]
    fn mode_read() {
        let p = parse_mode_string("rb").unwrap();
        assert_eq!(p.mode, GzMode::Read);
        assert_eq!(p.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(p.strategy, Z_DEFAULT_STRATEGY);
    }

    #[test]
    fn mode_write_level9() {
        let p = parse_mode_string("wb9").unwrap();
        assert_eq!(p.mode, GzMode::Write);
        assert_eq!(p.level, 9);
    }

    #[test]
    fn mode_append() {
        let p = parse_mode_string("ab").unwrap();
        assert_eq!(p.mode, GzMode::Append);
    }

    #[test]
    fn mode_strategies() {
        assert_eq!(parse_mode_string("wf").unwrap().strategy, Z_FILTERED);
        assert_eq!(parse_mode_string("wh").unwrap().strategy, Z_HUFFMAN_ONLY);
        assert_eq!(parse_mode_string("wR").unwrap().strategy, Z_RLE);
        assert_eq!(parse_mode_string("wF").unwrap().strategy, Z_FIXED);
    }

    #[test]
    fn mode_plus_rejected() {
        assert!(parse_mode_string("r+").is_err());
    }

    #[test]
    fn mode_empty_rejected() {
        assert!(parse_mode_string("b").is_err());
    }

    #[test]
    fn mode_transparent_write() {
        let p = parse_mode_string("wT").unwrap();
        assert_eq!(p.direct_flag, 1);
    }

    #[test]
    fn mode_transparent_read_rejected() {
        assert!(parse_mode_string("rT").is_err());
    }

    #[test]
    fn mode_gzip_only_write_rejected() {
        assert!(parse_mode_string("wG").is_err());
    }

    // ---- gz_open ----

    #[test]
    fn open_write_creates_file() {
        let p = temp_path("open_write.gz");
        let s = gz_open(&p, "wb").unwrap();
        assert_eq!(s.mode, GzMode::Write);
        assert!(!s.direct);
        assert_eq!(s.want, GZBUFSIZE);
        assert_eq!(s.pos, 0);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn open_read_existing() {
        let p = temp_path("open_read.gz");
        File::create(&p).unwrap();
        let s = gz_open(&p, "rb").unwrap();
        assert_eq!(s.mode, GzMode::Read);
        assert!(!s.eof);
        assert!(!s.past);
        assert_eq!(s.how, GzHow::Look);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn open_append_normalises_to_write() {
        let p = temp_path("open_append.gz");
        let s = gz_open(&p, "ab").unwrap();
        assert_eq!(s.mode, GzMode::Write);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn open_nonexistent_read_errno() {
        let p = temp_path("no_such_file_999.gz");
        assert!(matches!(gz_open(&p, "rb"), Err(ZlibError::Errno)));
    }

    #[test]
    fn open_level_and_strategy() {
        let p = temp_path("open_level6f.gz");
        let s = gz_open(&p, "wb6f").unwrap();
        assert_eq!(s.level, 6);
        assert_eq!(s.strategy, Z_FILTERED);
        fs::remove_file(&p).ok();
    }

    // ---- gz_dopen ----

    #[test]
    fn dopen_write() {
        let p = temp_path("dopen_w.gz");
        let f = File::create(&p).unwrap();
        let s = gz_dopen(f, "wb").unwrap();
        assert_eq!(s.mode, GzMode::Write);
        assert!(s.path.to_str().unwrap().contains("<fd>"));
        fs::remove_file(&p).ok();
    }

    // ---- gz_buffer ----

    #[test]
    fn buffer_before_io() {
        let p = temp_path("buf.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert!(gz_buffer(&mut s, 16384).is_ok());
        assert_eq!(s.want, 16384);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn buffer_minimum_clamped() {
        let p = temp_path("buf_min.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert!(gz_buffer(&mut s, 1).is_ok());
        assert_eq!(s.want, 8);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn buffer_after_io_rejected() {
        let p = temp_path("buf_after.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.size = 1024;
        assert!(gz_buffer(&mut s, 16384).is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn buffer_overflow_rejected() {
        let p = temp_path("buf_ovf.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert!(gz_buffer(&mut s, u32::MAX).is_err());
        fs::remove_file(&p).ok();
    }

    // ---- gz_reset ----

    #[test]
    fn reset_read() {
        let p = temp_path("reset_r.gz");
        File::create(&p).unwrap();
        let mut s = gz_open(&p, "rb").unwrap();
        s.pos = 100;
        s.have = 50;
        s.eof = true;
        s.past = true;
        gz_reset(&mut s);
        assert_eq!(s.pos, 0);
        assert_eq!(s.have, 0);
        assert!(!s.eof);
        assert!(!s.past);
        assert_eq!(s.how, GzHow::Look);
        assert!(!s.seek);
        assert_eq!(s.skip, 0);
        assert!(s.err.is_none());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn reset_write() {
        let p = temp_path("reset_w.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.pos = 200;
        s.have = 30;
        s.reset_pending = true;
        gz_reset(&mut s);
        assert_eq!(s.pos, 0);
        assert_eq!(s.have, 0);
        assert!(!s.reset_pending);
        fs::remove_file(&p).ok();
    }

    // ---- gz_rewind ----

    #[test]
    fn rewind_read() {
        let p = temp_path("rewind.gz");
        {
            let mut f = File::create(&p).unwrap();
            f.write_all(&[0u8; 100]).unwrap();
        }
        let mut s = gz_open(&p, "rb").unwrap();
        s.pos = 50;
        assert!(gz_rewind(&mut s).is_ok());
        assert_eq!(s.pos, 0);
        assert!(!s.eof);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn rewind_write_rejected() {
        let p = temp_path("rewind_w.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert!(gz_rewind(&mut s).is_err());
        fs::remove_file(&p).ok();
    }

    // ---- gz_seek ----

    #[test]
    fn seek_set_write_forward() {
        let p = temp_path("seek_set.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        let r = gz_seek(&mut s, 100, SEEK_SET).unwrap();
        assert_eq!(r, 100);
        assert!(s.seek);
        assert_eq!(s.skip, 100);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn seek_cur_write_forward() {
        let p = temp_path("seek_cur.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.pos = 50;
        let r = gz_seek(&mut s, 30, SEEK_CUR).unwrap();
        assert_eq!(r, 80);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn seek_end_rejected() {
        let p = temp_path("seek_end.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert!(gz_seek(&mut s, 0, SEEK_END).is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn seek_backward_write_rejected() {
        let p = temp_path("seek_bw.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.pos = 100;
        assert!(gz_seek(&mut s, 50, SEEK_SET).is_err());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn seek_read_skips_buffer() {
        let p = temp_path("seek_buf.gz");
        {
            let mut f = File::create(&p).unwrap();
            f.write_all(&[0u8; 200]).unwrap();
        }
        let mut s = gz_open(&p, "rb").unwrap();
        s.pos = 10;
        s.have = 20;
        // SEEK_SET to 25 → relative offset = 15 → skip 15 from buffer
        let r = gz_seek(&mut s, 25, SEEK_SET).unwrap();
        assert_eq!(r, 25);
        assert_eq!(s.have, 5); // 20 - 15 = 5 remaining
        assert_eq!(s.pos, 25);
        assert!(!s.seek); // no deferred skip needed
        fs::remove_file(&p).ok();
    }

    // ---- gz_tell ----

    #[test]
    fn tell_basic() {
        let p = temp_path("tell.gz");
        let s = gz_open(&p, "wb").unwrap();
        assert_eq!(gz_tell(&s), 0);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn tell_with_skip() {
        let p = temp_path("tell_skip.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.pos = 50;
        s.skip = 30;
        assert_eq!(gz_tell(&s), 80);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn tell_past_eof_ignores_skip() {
        let p = temp_path("tell_past.gz");
        File::create(&p).unwrap();
        let mut s = gz_open(&p, "rb").unwrap();
        s.pos = 50;
        s.skip = 30;
        s.past = true;
        assert_eq!(gz_tell(&s), 50);
        fs::remove_file(&p).ok();
    }

    // ---- gz_offset ----

    #[test]
    fn offset_write_at_start() {
        let p = temp_path("off_w.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        assert_eq!(gz_offset(&mut s).unwrap(), 0);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn offset_invalid_mode() {
        let p = temp_path("off_none.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.mode = GzMode::None;
        assert!(gz_offset(&mut s).is_err());
        fs::remove_file(&p).ok();
    }

    // ---- gz_eof ----

    #[test]
    fn eof_read_not_past() {
        let p = temp_path("eof_r.gz");
        File::create(&p).unwrap();
        let s = gz_open(&p, "rb").unwrap();
        assert!(!gz_eof(&s));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn eof_read_past() {
        let p = temp_path("eof_past.gz");
        File::create(&p).unwrap();
        let mut s = gz_open(&p, "rb").unwrap();
        s.past = true;
        assert!(gz_eof(&s));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn eof_write_always_false() {
        let p = temp_path("eof_w.gz");
        let s = gz_open(&p, "wb").unwrap();
        assert!(!gz_eof(&s));
        fs::remove_file(&p).ok();
    }

    // ---- gz_error_msg ----

    #[test]
    fn error_msg_no_error() {
        let p = temp_path("em_none.gz");
        let s = gz_open(&p, "wb").unwrap();
        let (e, m) = gz_error_msg(&s);
        assert!(e.is_none());
        assert!(m.is_none());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn error_msg_mem_override() {
        let p = temp_path("em_mem.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        gz_error(&mut s, Some(ZlibError::MemError), None);
        let (e, m) = gz_error_msg(&s);
        assert_eq!(*e.unwrap(), ZlibError::MemError);
        assert_eq!(m.unwrap(), "out of memory");
        fs::remove_file(&p).ok();
    }

    // ---- gz_clearerr ----

    #[test]
    fn clearerr_clears_all() {
        let p = temp_path("ce.gz");
        File::create(&p).unwrap();
        let mut s = gz_open(&p, "rb").unwrap();
        s.eof = true;
        s.past = true;
        s.err = Some(ZlibError::BufError);
        s.msg = Some("x".into());
        gz_clearerr(&mut s);
        assert!(!s.eof);
        assert!(!s.past);
        assert!(s.err.is_none());
        assert!(s.msg.is_none());
        fs::remove_file(&p).ok();
    }

    // ---- gz_error internal ----

    #[test]
    fn error_clear() {
        let p = temp_path("ge_clr.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.err = Some(ZlibError::BufError);
        s.msg = Some("old".into());
        gz_error(&mut s, None, None);
        assert!(s.err.is_none());
        assert!(s.msg.is_none());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn error_fatal_clears_buffer() {
        let p = temp_path("ge_fatal.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.have = 100;
        s.again = false;
        gz_error(&mut s, Some(ZlibError::StreamError), Some("f".into()));
        assert_eq!(s.have, 0);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn error_buferror_preserves_buffer() {
        let p = temp_path("ge_buf.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        s.have = 100;
        gz_error(&mut s, Some(ZlibError::BufError), Some("b".into()));
        assert_eq!(s.have, 100);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn error_path_prefix() {
        let p = temp_path("ge_path.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        gz_error(&mut s, Some(ZlibError::StreamError), Some("bad".into()));
        let m = s.msg.as_ref().unwrap();
        assert!(m.contains("ge_path.gz"));
        assert!(m.contains("bad"));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn error_mem_skips_message() {
        let p = temp_path("ge_mem.gz");
        let mut s = gz_open(&p, "wb").unwrap();
        gz_error(&mut s, Some(ZlibError::MemError), Some("ignored".into()));
        assert!(s.msg.is_none());
        assert_eq!(s.err, Some(ZlibError::MemError));
        fs::remove_file(&p).ok();
    }
}
