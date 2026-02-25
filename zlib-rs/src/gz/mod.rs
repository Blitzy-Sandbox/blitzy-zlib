//! Gzip file I/O module.
//!
//! Provides buffered reading and writing of gzip-compressed files,
//! with auto-detection of gzip vs transparent data on read.
//! This module is conditionally compiled when the `gz-io` feature is enabled.
//!
//! # Architecture
//!
//! - [`GzReader`] — Buffered gzip reader with auto-detect pipeline
//! - [`GzWriter`] — Buffered gzip writer with compression
//! - [`GzFile`] — Unified handle wrapping either a reader or writer
//! - [`GzState`](state::GzState) — Internal state shared between reader/writer
//!
//! # Ported From
//!
//! This module ports the shared functions from `gzlib.c` (609 lines) and
//! the close dispatch from `gzclose.c` (23 lines). The submodules port
//! `gzread.c`, `gzwrite.c`, and `gzguts.h` respectively.
//!
//! # Conditional Compilation
//!
//! The entire module is gated on `#[cfg(feature = "gz-io")]` at the crate
//! root (`lib.rs`), corresponding to the C `Z_SOLO` exclusion pattern.

// ─── Submodule Declarations ─────────────────────────────────────────────────

/// Gzip read pipeline with LOOK/COPY/GZIP auto-detection.
pub mod read;
/// Gzip write pipeline with buffered compression.
pub mod write;
/// Internal state types for the gzip file I/O layer.
pub(crate) mod state;

// ─── Re-exports ─────────────────────────────────────────────────────────────

pub use read::GzReader;
pub use write::GzWriter;

/// Default I/O buffer size for gzip operations (8 KiB).
///
/// Double this for the output buffer when reading (for `gzungetc` space),
/// and double for the input buffer when writing (for `gzprintf` space).
/// Matches the C `GZBUFSIZE` constant from `gzguts.h` line 156.
pub use state::GZBUFSIZE;

// ─── Imports ────────────────────────────────────────────────────────────────

use std::io::{self, Seek, SeekFrom};

use crate::constants::{Z_BUF_ERROR, Z_MEM_ERROR, Z_OK};
use crate::error::ReturnCode;

use state::{GzHow, GzMode, GzState};

// ─── Internal Constants ─────────────────────────────────────────────────────

/// Seek from beginning of file (POSIX `SEEK_SET` = 0).
const GZ_SEEK_SET: i32 = 0;

/// Seek from current position (POSIX `SEEK_CUR` = 1).
const GZ_SEEK_CUR: i32 = 1;

// ─── GzFile ─────────────────────────────────────────────────────────────────

/// A unified gzip file handle that wraps either a reader or writer.
///
/// Equivalent to the C `gzFile` opaque type, which dispatches
/// close operations based on the internal mode (`GZ_READ` or `GZ_WRITE`).
///
/// This enum provides a single entry point for opening gzip files in either
/// mode, with mode selection driven by the mode string argument to [`open`].
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::gz::GzFile;
///
/// // Open for reading
/// let file = GzFile::open("data.gz", "r").unwrap();
/// file.close().unwrap();
///
/// // Open for writing
/// let file = GzFile::open("output.gz", "w").unwrap();
/// file.close().unwrap();
/// ```
///
/// [`open`]: GzFile::open
pub enum GzFile {
    /// A gzip file opened for reading.
    Reader(GzReader),
    /// A gzip file opened for writing (or appending).
    Writer(GzWriter),
}

impl GzFile {
    /// Opens a gzip file by path with the given mode string.
    ///
    /// Parses the mode string to determine whether to open for reading
    /// or writing, then delegates to [`GzReader::open`] or
    /// [`GzWriter::open_with_mode`] accordingly.
    ///
    /// # Mode Characters
    ///
    /// | Character | Effect |
    /// |-----------|--------|
    /// | `'r'` | Open for reading (auto-detect gzip vs transparent) |
    /// | `'w'` | Open for writing (create/truncate) |
    /// | `'a'` | Open for appending (create, seek to end) |
    /// | `'0'`–`'9'` | Compression level (write mode only) |
    /// | `'f'`/`'h'`/`'R'`/`'F'` | Compression strategy (write mode only) |
    /// | `'T'` | Transparent (direct copy, no compression) |
    /// | `'b'` | Binary mode (ignored on most platforms) |
    /// | `'+'` | Error — simultaneous read/write not supported |
    ///
    /// This is the Rust equivalent of `gz_open()` from `gzlib.c` lines 86–285.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if:
    /// - The mode string contains `'+'` (read+write not supported)
    /// - The mode string contains both `'r'` and `'w'`/`'a'`
    /// - The mode string contains neither `'r'` nor `'w'`/`'a'`
    /// - The underlying file cannot be opened
    pub fn open(path: &str, mode: &str) -> io::Result<Self> {
        let mut is_read = false;
        let mut is_write = false;

        for ch in mode.chars() {
            match ch {
                'r' => is_read = true,
                'w' | 'a' => is_write = true,
                '+' => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot open gzip file for both reading and writing",
                    ));
                }
                _ => {} // Other mode chars handled by GzWriter::open_with_mode
            }
        }

        if is_read && is_write {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot open gzip file for both reading and writing",
            ));
        }

        if is_read {
            let reader = GzReader::open(path)?;
            Ok(GzFile::Reader(reader))
        } else if is_write {
            let writer = GzWriter::open_with_mode(path, mode)?;
            Ok(GzFile::Writer(writer))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mode must include 'r', 'w', or 'a'",
            ))
        }
    }

    /// Closes the gzip file and flushes any pending data.
    ///
    /// For writers, this finalizes the gzip stream (writing the trailer)
    /// and flushes all buffered data to disk. For readers, cleanup is
    /// handled automatically via the `Drop` implementation.
    ///
    /// This is the Rust equivalent of `gzclose()` from `gzclose.c`
    /// lines 11–23, which dispatches to `gzclose_r()` or `gzclose_w()`
    /// based on the internal mode.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if flushing the writer fails.
    pub fn close(self) -> io::Result<()> {
        match self {
            GzFile::Reader(_reader) => {
                // Drop handles cleanup for reader (inflateEnd, file close, etc.)
                // GzReader's Drop impl calls inflate_end and all owned resources
                // are freed automatically.
                Ok(())
            }
            GzFile::Writer(writer) => writer.close(),
        }
    }

    /// Sets the internal buffer size for I/O operations.
    ///
    /// Must be called before any read or write operation. Once buffers
    /// are allocated (after the first I/O operation), the size cannot
    /// be changed.
    ///
    /// This is the Rust equivalent of `gzbuffer()` from `gzlib.c`
    /// lines 322–343.
    ///
    /// # Arguments
    ///
    /// * `size` — Desired buffer size in bytes. Clamped to a minimum of 2.
    ///   Must not overflow when doubled (for internal double-buffering).
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::StreamError`] if buffers are already allocated
    /// or the size would overflow when doubled.
    pub fn set_buffer_size(&mut self, size: usize) -> Result<(), ReturnCode> {
        match self {
            GzFile::Reader(reader) => reader.set_buffer_size(size),
            GzFile::Writer(writer) => writer.set_buffer_size(size),
        }
    }
}

// ─── Shared Functions ───────────────────────────────────────────────────────
//
// The following functions are ported from gzlib.c and used by both the
// read and write pipelines. They operate on GzState directly rather than
// on GzReader/GzWriter, enabling the FFI layer to call them without
// knowing the specific reader/writer type.

/// Sets the error state on a gzip state structure.
///
/// Clears any previous error message (unless the current error is
/// [`Z_MEM_ERROR`], which uses a static message). For fatal errors
/// (not [`Z_OK`] and not [`Z_BUF_ERROR`], and no I/O stall pending),
/// clears the available output count to force `gzgetc` to fail.
///
/// If `msg` is provided and the error is not [`Z_MEM_ERROR`], constructs
/// a formatted error message as `"{path}: {msg}"` and stores it in
/// `state.msg`.
///
/// Port of `gz_error()` from `gzlib.c` lines 555–590.
///
/// # Arguments
///
/// * `state` — Mutable reference to the gzip state.
/// * `err` — Error code (e.g., [`Z_OK`], [`Z_MEM_ERROR`], [`Z_BUF_ERROR`]).
/// * `msg` — Optional error message string. `None` to clear the message.
pub(crate) fn gz_error(state: &mut GzState, err: i32, msg: Option<&str>) {
    // Free previously allocated message and clear.
    // In C, we must check for Z_MEM_ERROR before freeing because the message
    // might be a static literal. In Rust, Option<String> is always safely
    // droppable, so we unconditionally clear.
    state.msg = None;

    // If fatal error (not OK, not BUF_ERROR, and not stalled), set
    // state.x.have to 0 so the gzgetc macro fails.
    // gzlib.c lines 563–565
    if err != Z_OK && err != Z_BUF_ERROR && !state.again {
        state.x.have = 0;
    }

    // Set error code
    state.err = err;

    // If no message provided, we're done
    let Some(msg) = msg else {
        return;
    };

    // For out-of-memory errors, don't try to allocate a message string.
    // The caller should use gz_get_error() which returns a static
    // "out of memory" string for Z_MEM_ERROR.
    // gzlib.c lines 572–574
    if err == Z_MEM_ERROR {
        return;
    }

    // Construct error message with path prefix: "{path}: {msg}"
    // gzlib.c lines 576–589
    // Note: In Rust, format!() cannot fail with OOM in the way C malloc can.
    // On allocation failure, Rust will abort (global allocator behavior).
    // This matches the C behavior where gz_error sets Z_MEM_ERROR on
    // malloc failure.
    state.msg = Some(format!("{}: {}", state.path, msg));
}

/// Resets the gzip file state to its initial values.
///
/// Clears all buffered data, resets the position counter, and clears
/// the error state. For read mode, resets the auto-detect pipeline
/// to [`GzHow::Look`]. For write mode, clears the pending deflate
/// reset flag.
///
/// Port of `gz_reset()` from `gzlib.c` lines 69–84.
///
/// # Arguments
///
/// * `state` — Mutable reference to the gzip state to reset.
pub(crate) fn gz_reset(state: &mut GzState) {
    // No output data available
    state.x.have = 0;

    if state.mode == GzMode::Read {
        // Reset read-mode state
        state.eof = false;
        state.past = false;
        state.how = GzHow::Look;
        state.junk = -1; // mark first member
    } else {
        // Reset write-mode state
        state.reset = false;
    }

    // Common reset
    state.again = false;
    state.skip = 0;

    // Clear error
    gz_error(state, Z_OK, None);

    // Reset position
    state.x.pos = 0;

    // Clear any buffered input on the stream
    // C: state->strm.avail_in = 0;
    state.strm.set_input(&[]);
}

/// Seeks to a position in the uncompressed data stream.
///
/// Supports `SEEK_SET` (absolute) and `SEEK_CUR` (relative) positioning.
/// `SEEK_END` is not supported for gzip streams.
///
/// For read mode:
/// - Fast path when in COPY mode with a positive offset: seeks the
///   underlying file directly.
/// - Backward seeks rewind the file and skip forward to the target.
/// - Buffered output bytes are consumed before setting a pending skip.
///
/// For write mode:
/// - Only forward seeks are supported (backward would require
///   re-compressing from the start).
/// - The skip amount is recorded and zero-filled during subsequent writes.
///
/// Port of `gzseek64()` from `gzlib.c` lines 367–435.
///
/// # Arguments
///
/// * `state` — Mutable reference to the gzip state.
/// * `offset` — Byte offset relative to `whence`.
/// * `whence` — Seek origin: `0` for `SEEK_SET`, `1` for `SEEK_CUR`.
///
/// # Errors
///
/// Returns an I/O error if:
/// - The state mode is invalid
/// - There is a pending fatal error
/// - `whence` is not `SEEK_SET` or `SEEK_CUR`
/// - A backward seek fails (file seek error)
/// - The computed position would be negative
pub(crate) fn gz_seek(
    state: &mut GzState,
    offset: i64,
    whence: i32,
) -> io::Result<i64> {
    // Validate mode — must be in a valid read or write state
    if state.mode != GzMode::Read && state.mode != GzMode::Write {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid gzip state mode for seek",
        ));
    }

    // Check that there's no fatal error
    // gzlib.c lines 380–381
    if state.err != Z_OK && state.err != Z_BUF_ERROR {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "cannot seek: pending error on gzip state",
        ));
    }

    // Can only seek from start or relative to current position
    // gzlib.c lines 384–385
    if whence != GZ_SEEK_SET && whence != GZ_SEEK_CUR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only SEEK_SET and SEEK_CUR are supported for gzip seek",
        ));
    }

    // Normalize offset to a SEEK_CUR specification
    // gzlib.c lines 388–393
    let mut offset = if whence == GZ_SEEK_SET {
        offset - state.x.pos
    } else {
        let adj = if state.past { 0 } else { state.skip };
        state.skip = 0;
        offset + adj
    };

    // Fast path: if within raw area while reading (COPY mode), just
    // seek the underlying file directly.
    // gzlib.c lines 396–409
    if state.mode == GzMode::Read
        && state.how == GzHow::Copy
        && state.x.pos + offset >= 0
    {
        // Compute the seek delta: offset minus buffered-but-undelivered bytes
        #[allow(clippy::cast_possible_wrap)]
        let seek_delta = offset - state.x.have as i64;
        let result = state.file.seek(SeekFrom::Current(seek_delta));
        match result {
            Ok(_) => {
                state.x.have = 0;
                state.eof = false;
                state.past = false;
                state.skip = 0;
                gz_error(state, Z_OK, None);
                state.strm.set_input(&[]);
                state.x.pos += offset;
                return Ok(state.x.pos);
            }
            Err(e) => return Err(e),
        }
    }

    // Calculate skip amount, rewinding if needed for backward seek
    // gzlib.c lines 412–420
    if offset < 0 {
        if state.mode != GzMode::Read {
            // Writing — can't go backwards
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek backwards in gzip write mode",
            ));
        }
        offset += state.x.pos;
        if offset < 0 {
            // Before start of file
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position before start of file",
            ));
        }
        // Rewind: seek to start position and reset state
        // Equivalent to gzrewind(file) from gzlib.c lines 346–364
        #[allow(clippy::cast_sign_loss)]
        let start_pos = state.start as u64;
        state.file.seek(SeekFrom::Start(start_pos))?;
        gz_reset(state);
    }

    // If reading, skip what's in the output buffer (one less gzgetc check)
    // gzlib.c lines 422–430
    if state.mode == GzMode::Read {
        let n = min_of_have_and_offset(state.x.have, offset);
        state.x.have -= n;
        state.x.next += n;
        #[allow(clippy::cast_possible_wrap)]
        let n_i64 = n as i64;
        state.x.pos += n_i64;
        offset -= n_i64;
    }

    // Request skip (if not zero)
    // gzlib.c lines 432–434
    state.skip = offset;
    Ok(state.x.pos + offset)
}

/// Returns the current virtual position in the uncompressed data stream.
///
/// The position accounts for any pending skip amount that has not yet
/// been consumed. For read mode, skips due to seeks that haven't been
/// fulfilled yet are included. For state marked as past-EOF, the skip
/// is excluded.
///
/// Port of `gztell64()` from `gzlib.c` lines 446–458.
///
/// # Arguments
///
/// * `state` — Reference to the gzip state.
pub(crate) fn gz_tell(state: &GzState) -> i64 {
    state.x.pos + if state.past { 0 } else { state.skip }
}

/// Returns the current byte offset in the compressed file.
///
/// Queries the underlying file position and adjusts for any buffered
/// input data that has been read from the file but not yet consumed
/// by the decompression engine.
///
/// Port of `gzoffset64()` from `gzlib.c` lines 469–487.
///
/// # Arguments
///
/// * `state` — Mutable reference to the gzip state (required for file seek).
///
/// # Errors
///
/// Returns an I/O error if the file position cannot be queried.
pub(crate) fn gz_offset(state: &mut GzState) -> io::Result<i64> {
    // Get current file position
    let file_pos = state.file.stream_position()?;

    #[allow(clippy::cast_possible_wrap)]
    let mut offset = file_pos as i64;

    // If reading, subtract buffered input that hasn't been consumed yet
    // gzlib.c lines 484–485
    if state.mode == GzMode::Read {
        #[allow(clippy::cast_possible_wrap)]
        let avail = state.strm.avail_in() as i64;
        offset -= avail;
    }

    Ok(offset)
}

/// Returns whether the end of the uncompressed data has been reached.
///
/// For read mode, returns `true` if a read was attempted past the end
/// of all available data (including multi-member gzip streams). For
/// write mode, always returns `false`.
///
/// Port of `gzeof()` from `gzlib.c` lines 498–510.
///
/// # Arguments
///
/// * `state` — Reference to the gzip state.
pub(crate) fn gz_eof(state: &GzState) -> bool {
    if state.mode == GzMode::Read {
        state.past
    } else {
        false
    }
}

/// Returns the current error code and message for the gzip state.
///
/// Returns a tuple of `(error_code, message_string)`. For
/// [`Z_MEM_ERROR`], the message is the static string `"out of memory"`
/// regardless of `state.msg`. For all other errors, the message comes
/// from `state.msg`, defaulting to an empty string if not set.
///
/// Port of `gzerror()` from `gzlib.c` lines 513–528.
///
/// # Arguments
///
/// * `state` — Reference to the gzip state.
pub(crate) fn gz_get_error(state: &GzState) -> (i32, &str) {
    if state.err == Z_MEM_ERROR {
        (state.err, "out of memory")
    } else {
        let msg = match state.msg {
            Some(ref s) => s.as_str(),
            None => "",
        };
        (state.err, msg)
    }
}

/// Clears the error and end-of-file state.
///
/// For read mode, clears the EOF and past-EOF flags so that further
/// reads can be attempted (e.g., if the file has been extended).
/// For both modes, resets the error code to [`Z_OK`] and clears the
/// error message.
///
/// Port of `gzclearerr()` from `gzlib.c` lines 531–547.
///
/// # Arguments
///
/// * `state` — Mutable reference to the gzip state.
pub(crate) fn gz_clearerr(state: &mut GzState) {
    // Clear end-of-file state for read mode
    // gzlib.c lines 542–545
    if state.mode == GzMode::Read {
        state.eof = false;
        state.past = false;
    }

    // Clear error
    gz_error(state, Z_OK, None);
}

// ─── Internal Helpers ───────────────────────────────────────────────────────

/// Returns the minimum of a `usize` buffer count and a non-negative `i64` offset.
///
/// This is the Rust equivalent of the C pattern:
/// ```c
/// n = GT_OFF(state->x.have) || (z_off64_t)state->x.have > offset ?
///     (unsigned)offset : state->x.have;
/// ```
///
/// The `GT_OFF` macro in C checks if the unsigned value exceeds the maximum
/// `z_off64_t` (signed 64-bit). In Rust, we handle this via `i64::try_from`
/// to safely compare `usize` with `i64`.
///
/// # Arguments
///
/// * `have` — Number of bytes available in the buffer (`usize`).
/// * `offset` — Non-negative byte offset (`i64`, must be `>= 0`).
///
/// # Returns
///
/// The smaller of `have` and `offset`, as a `usize`.
fn min_of_have_and_offset(have: usize, offset: i64) -> usize {
    debug_assert!(offset >= 0, "offset must be non-negative");

    match i64::try_from(have) {
        Ok(have_i64) => {
            if have_i64 <= offset {
                // have fits in i64 and is <= offset: take all of have
                have
            } else {
                // have > offset: take offset amount
                // offset >= 0 and offset < have (which fits in usize),
                // so offset fits in usize
                usize::try_from(offset).unwrap_or(usize::MAX)
            }
        }
        Err(_) => {
            // have > i64::MAX, so have is definitely > offset
            // offset >= 0 and is i64, so it fits in usize on 64-bit platforms.
            // On 32-bit platforms where i64 can exceed usize, clamp.
            usize::try_from(offset).unwrap_or(usize::MAX)
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gzbufsize_value() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    #[test]
    fn test_min_of_have_and_offset_have_smaller() {
        assert_eq!(min_of_have_and_offset(10, 100), 10);
    }

    #[test]
    fn test_min_of_have_and_offset_offset_smaller() {
        assert_eq!(min_of_have_and_offset(100, 10), 10);
    }

    #[test]
    fn test_min_of_have_and_offset_equal() {
        assert_eq!(min_of_have_and_offset(50, 50), 50);
    }

    #[test]
    fn test_min_of_have_and_offset_zero_offset() {
        assert_eq!(min_of_have_and_offset(100, 0), 0);
    }

    #[test]
    fn test_min_of_have_and_offset_zero_have() {
        assert_eq!(min_of_have_and_offset(0, 100), 0);
    }

    #[test]
    fn test_gz_tell_no_skip() {
        let state = make_test_state(GzMode::Read, 0, 100, false);
        assert_eq!(gz_tell(&state), 100);
    }

    #[test]
    fn test_gz_tell_with_skip_not_past() {
        let state = make_test_state_with_skip(GzMode::Read, 100, false, 50);
        assert_eq!(gz_tell(&state), 150);
    }

    #[test]
    fn test_gz_tell_with_skip_past() {
        let state = make_test_state_with_skip(GzMode::Read, 100, true, 50);
        assert_eq!(gz_tell(&state), 100);
    }

    #[test]
    fn test_gz_eof_read_mode_not_past() {
        let state = make_test_state(GzMode::Read, 0, 0, false);
        assert!(!gz_eof(&state));
    }

    #[test]
    fn test_gz_eof_read_mode_past() {
        let state = make_test_state(GzMode::Read, 0, 0, true);
        assert!(gz_eof(&state));
    }

    #[test]
    fn test_gz_eof_write_mode() {
        let state = make_test_state(GzMode::Write, 0, 0, false);
        assert!(!gz_eof(&state));
    }

    #[test]
    fn test_gz_get_error_ok() {
        let state = make_test_state(GzMode::Read, 0, 0, false);
        let (code, msg) = gz_get_error(&state);
        assert_eq!(code, Z_OK);
        assert_eq!(msg, "");
    }

    #[test]
    fn test_gz_get_error_mem_error() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        state.err = Z_MEM_ERROR;
        state.msg = Some("ignored message".to_string());
        let (code, msg) = gz_get_error(&state);
        assert_eq!(code, Z_MEM_ERROR);
        assert_eq!(msg, "out of memory");
    }

    #[test]
    fn test_gz_get_error_with_message() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        state.err = Z_BUF_ERROR;
        state.msg = Some("test.gz: buffer full".to_string());
        let (code, msg) = gz_get_error(&state);
        assert_eq!(code, Z_BUF_ERROR);
        assert_eq!(msg, "test.gz: buffer full");
    }

    #[test]
    fn test_gz_error_sets_code_and_message() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        gz_error(&mut state, Z_BUF_ERROR, Some("stall detected"));
        assert_eq!(state.err, Z_BUF_ERROR);
        assert!(state.msg.is_some());
        let msg = state.msg.as_ref().map(String::as_str).unwrap_or("");
        assert!(msg.contains("stall detected"));
    }

    #[test]
    fn test_gz_error_clears_on_ok() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        state.err = Z_BUF_ERROR;
        state.msg = Some("old error".to_string());
        gz_error(&mut state, Z_OK, None);
        assert_eq!(state.err, Z_OK);
        assert!(state.msg.is_none());
    }

    #[test]
    fn test_gz_error_mem_error_no_allocate() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        gz_error(&mut state, Z_MEM_ERROR, Some("out of memory"));
        assert_eq!(state.err, Z_MEM_ERROR);
        // For Z_MEM_ERROR, msg should NOT be allocated (remains None)
        assert!(state.msg.is_none());
    }

    #[test]
    fn test_gz_error_fatal_clears_have() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        state.x.have = 100;
        state.again = false;
        // Z_STREAM_ERROR is fatal (not Z_OK, not Z_BUF_ERROR)
        gz_error(&mut state, crate::constants::Z_STREAM_ERROR, None);
        assert_eq!(state.x.have, 0);
    }

    #[test]
    fn test_gz_error_buf_error_preserves_have() {
        let mut state = make_test_state(GzMode::Read, 0, 0, false);
        state.x.have = 100;
        gz_error(&mut state, Z_BUF_ERROR, None);
        // Z_BUF_ERROR is non-fatal — have should be preserved
        assert_eq!(state.x.have, 100);
    }

    #[test]
    fn test_gz_reset_read_mode() {
        let mut state = make_test_state(GzMode::Read, 0, 500, true);
        state.x.have = 42;
        state.eof = true;
        state.how = GzHow::Gzip;
        state.skip = 100;
        state.again = true;
        state.err = Z_BUF_ERROR;

        gz_reset(&mut state);

        assert_eq!(state.x.have, 0);
        assert!(!state.eof);
        assert!(!state.past);
        assert_eq!(state.how, GzHow::Look);
        assert_eq!(state.junk, -1);
        assert!(!state.again);
        assert_eq!(state.skip, 0);
        assert_eq!(state.err, Z_OK);
        assert_eq!(state.x.pos, 0);
    }

    #[test]
    fn test_gz_reset_write_mode() {
        let mut state = make_test_state(GzMode::Write, 0, 500, false);
        state.x.have = 42;
        state.reset = true;
        state.skip = 100;
        state.again = true;

        gz_reset(&mut state);

        assert_eq!(state.x.have, 0);
        assert!(!state.reset);
        assert!(!state.again);
        assert_eq!(state.skip, 0);
        assert_eq!(state.x.pos, 0);
    }

    #[test]
    fn test_gz_clearerr_read_mode() {
        let mut state = make_test_state(GzMode::Read, 0, 0, true);
        state.err = Z_BUF_ERROR;
        state.eof = true;

        gz_clearerr(&mut state);

        assert!(!state.eof);
        assert!(!state.past);
        assert_eq!(state.err, Z_OK);
    }

    #[test]
    fn test_gz_clearerr_write_mode() {
        let mut state = make_test_state(GzMode::Write, 0, 0, false);
        state.err = Z_BUF_ERROR;

        gz_clearerr(&mut state);

        assert_eq!(state.err, Z_OK);
    }

    // ── Test Helpers ────────────────────────────────────────────────────────

    /// Creates a minimal `GzState` for testing shared functions.
    ///
    /// Uses a temporary file so the state has a valid file handle.
    fn make_test_state(mode: GzMode, err: i32, pos: i64, past: bool) -> GzState {
        use std::io::Write;
        let mut tmpfile = tempfile().unwrap_or_else(|_| {
            // Fallback: create a named temp file
            let path = std::env::temp_dir().join("gz_mod_test");
            std::fs::File::create(&path).expect("failed to create temp file")
        });
        let _ = tmpfile.write_all(b"test data");
        let _ = tmpfile.seek(SeekFrom::Start(0));

        let mut state = GzState::new_reader(tmpfile, "test.gz".to_string());
        state.mode = mode;
        state.err = err;
        state.x.pos = pos;
        state.past = past;
        state
    }

    /// Creates a `GzState` with specific skip and past values.
    fn make_test_state_with_skip(
        mode: GzMode,
        pos: i64,
        past: bool,
        skip: i64,
    ) -> GzState {
        let mut state = make_test_state(mode, Z_OK, pos, past);
        state.skip = skip;
        state
    }

    /// Creates an anonymous temporary file for testing.
    fn tempfile() -> io::Result<std::fs::File> {
        use std::fs;

        let dir = std::env::temp_dir();
        let path = dir.join(format!("gz_mod_test_{}", std::process::id()));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        // Attempt to remove the file name — the handle keeps the file alive.
        // If removal fails (e.g., on Windows), it's harmless.
        let _ = fs::remove_file(&path);
        Ok(file)
    }
}
