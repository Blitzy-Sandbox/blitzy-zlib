//! Internal state types for the gzip file I/O layer.
//!
//! This module defines the core types that form the foundation of the gzip
//! file I/O subsystem. The central type is [`GzState`], a direct port of
//! the C `gz_state` structure from `gzguts.h` (lines 169–204), translated
//! to idiomatic Rust with owned resources and RAII-based cleanup.
//!
//! # Types
//!
//! - [`GzMode`] — Operating mode enum replacing C integer magic constants
//! - [`GzHow`] — Read processing mode enum for the auto-detect pipeline
//! - [`GzExposed`] — Exposed state for efficient single-byte reads
//! - [`GzState`] — Complete internal state for gzip file operations
//!
//! # RAII and Resource Management
//!
//! In the C implementation, `gzclose_r()` and `gzclose_w()` must manually
//! free five distinct resources: the inflate/deflate state, the input buffer,
//! the output buffer, the path string, and the file descriptor. In Rust, all
//! of these are owned fields on [`GzState`] and are cleaned up automatically
//! when the value is dropped.

use std::fmt;
use std::fs::File;
use std::io::{self, Seek, SeekFrom};

use crate::constants;
use crate::stream::ZStream;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Default I/O buffer size for gzip operations.
///
/// Double this for the output buffer when reading (for `gzungetc` space),
/// and double for the input buffer when writing (for `gzprintf` space).
/// This constant and twice this must fit in a `usize`. Defined as
/// `GZBUFSIZE` in `gzguts.h` line 156.
pub const GZBUFSIZE: usize = 8192;

// ─── GzMode ───────────────────────────────────────────────────────────────────

/// Operating mode of a gzip file handle.
///
/// Replaces the C integer constants `GZ_NONE` (0), `GZ_READ` (7247),
/// `GZ_WRITE` (31153), and `GZ_APPEND` (1) from `gzguts.h` lines 158–162.
/// In C, the specific integer values serve as integrity checks on the state
/// structure; in Rust, the enum itself provides compile-time type safety.
// Variants `None` and `Append` are retained for C API completeness; they are
// used during gz_open initialization before the mode is resolved to Read/Write.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GzMode {
    /// Not yet initialized.
    ///
    /// Equivalent to C `GZ_NONE` (0). The file has been allocated but no
    /// mode has been selected yet.
    None,

    /// Reading gzip data.
    ///
    /// Equivalent to C `GZ_READ` (7247). The file is open for reading and
    /// the auto-detection pipeline will determine if the input is gzip
    /// compressed or transparent.
    Read,

    /// Writing gzip data.
    ///
    /// Equivalent to C `GZ_WRITE` (31153). The file is open for writing
    /// with compression (or transparent copy if `direct` is set).
    Write,

    /// Appending to a gzip file.
    ///
    /// Equivalent to C `GZ_APPEND` (1). After opening, the file is seeked
    /// to the end and the mode is converted to [`Write`](GzMode::Write).
    Append,
}

// ─── GzHow ────────────────────────────────────────────────────────────────────

/// How data is being processed during reading.
///
/// Controls the auto-detection pipeline that determines whether to treat
/// input as gzip-compressed data or transparent (uncompressed) data.
/// Replaces the C constants `LOOK` (0), `COPY` (1), `GZIP` (2) from
/// `gzguts.h` lines 164–167.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GzHow {
    /// Look for a gzip header.
    ///
    /// Equivalent to C `LOOK` (0). The reader is examining input bytes
    /// to determine if they form a valid gzip header (`1f 8b 08`).
    Look,

    /// Copy input directly (transparent mode).
    ///
    /// Equivalent to C `COPY` (1). The input is not gzip-compressed;
    /// bytes are passed through to the output without decompression.
    Copy,

    /// Decompress a gzip stream.
    ///
    /// Equivalent to C `GZIP` (2). The input has been identified as
    /// gzip-compressed and is being decompressed via the inflate engine.
    Gzip,
}

// ─── GzExposed ────────────────────────────────────────────────────────────────

/// Exposed state for efficient single-byte reads.
///
/// Mirrors the C `gzFile_s` exposed struct used by the `gzgetc()` macro
/// (from `zlib.h` lines 1384–1387 and `gzguts.h` lines 171–175). This
/// struct provides fast access to buffered output data without going
/// through the full read pipeline.
///
/// # C-to-Rust Transformation
///
/// In C, `x.next` is a raw pointer into the output buffer that is
/// advanced as bytes are consumed. In Rust, `next` is an index offset
/// into the output `Vec<u8>`, providing bounds-safe access.
#[derive(Debug, Clone)]
pub struct GzExposed {
    /// Number of bytes available at the current read position.
    ///
    /// When nonzero, there are buffered bytes ready to deliver without
    /// additional I/O or decompression.
    pub have: usize,

    /// Index into the output buffer for the next data to deliver.
    ///
    /// In C this is a raw pointer (`unsigned char *`); here it is a
    /// safe index into the output `Vec<u8>`.
    pub next: usize,

    /// Current position in uncompressed data.
    ///
    /// Tracks the logical byte position as seen by the caller, accounting
    /// for all bytes read or written so far. Equivalent to C `z_off64_t`.
    pub pos: i64,
}

impl GzExposed {
    /// Creates a new [`GzExposed`] with all fields zeroed.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use zlib_rs::gz::state::GzExposed;
    ///
    /// let exposed = GzExposed::new();
    /// assert_eq!(exposed.have, 0);
    /// assert_eq!(exposed.next, 0);
    /// assert_eq!(exposed.pos, 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            have: 0,
            next: 0,
            pos: 0,
        }
    }

    /// Resets all fields to their initial (zeroed) state.
    ///
    /// Used during `gz_reset()` to clear buffered output state before
    /// rewinding or reinitializing the gzip stream.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.have = 0;
        self.next = 0;
        self.pos = 0;
    }
}

impl Default for GzExposed {
    fn default() -> Self {
        Self::new()
    }
}

// ─── GzState ──────────────────────────────────────────────────────────────────

/// Internal state for gzip file operations.
///
/// This is the Rust port of the C `gz_state` structure from `gzguts.h`
/// lines 169–204. It owns the file handle, input/output buffers, and an
/// embedded [`ZStream`] for compression/decompression. Resource cleanup
/// is automatic via Rust's ownership model — when a `GzState` is dropped,
/// all owned resources (file descriptor, heap buffers, stream state) are
/// released in the correct order.
///
/// # Field Groups
///
/// The fields are organized into the same groups as the C original:
///
/// - **Exposed contents** — Fast-path state for buffered reads
/// - **Both reading and writing** — File handle, buffers, mode
/// - **Just for reading** — Auto-detect pipeline state, EOF tracking
/// - **Just for writing** — Compression level/strategy, reset flag
/// - **Seek request** — Pending skip amount
/// - **Error information** — Error code and message
/// - **Stream** — Embedded inflate/deflate stream
///
/// # C-to-Rust Transformations
///
/// | C Field | Rust Field | C Type | Rust Type |
/// |---------|-----------|--------|-----------|
/// | `fd` | `file` | `int` | [`File`] |
/// | `path` | `path` | `char *` | [`String`] |
/// | `in` | `input` | `unsigned char *` | `Vec<u8>` |
/// | `out` | `output` | `unsigned char *` | `Vec<u8>` |
/// | `msg` | `msg` | `char *` | `Option<String>` |
/// | `mode` | `mode` | `int` (magic) | [`GzMode`] |
/// | `how` | `how` | `int` (0/1/2) | [`GzHow`] |
/// | `eof`/`past`/`again`/`reset` | same | `int` | [`bool`] |
/// | `strm` | `strm` | `z_stream` | [`ZStream`] |
// The boolean fields mirror the C `gz_state` struct's flag integers; collapsing
// them into a bitfield or enum would reduce fidelity to the original.
#[allow(clippy::struct_excessive_bools, clippy::module_name_repetitions)]
pub struct GzState {
    // ── Exposed contents for efficient reads ──

    /// Exposed state providing fast-path access to buffered output.
    ///
    /// Contains `have` (bytes available), `next` (buffer index), and
    /// `pos` (uncompressed position). Mirrors C `struct gzFile_s x`.
    pub x: GzExposed,

    // ── Used for both reading and writing ──

    /// Current operating mode (Read, Write, Append, or None).
    ///
    /// Replaces C `int mode` with the magic values 7247/31153/1/0.
    pub mode: GzMode,

    /// Underlying file handle, owned by this state.
    ///
    /// Replaces C `int fd`. Closed automatically when dropped.
    pub file: File,

    /// File path string for error messages.
    ///
    /// Replaces C `char *path` (heap-allocated). Freed on drop.
    pub path: String,

    /// Buffer size, zero if buffers have not been allocated yet.
    ///
    /// Once allocated, this equals the value of `want` at allocation time.
    /// A nonzero value indicates that `input` and `output` buffers are
    /// initialized and ready for use.
    pub size: usize,

    /// Requested buffer size, default is [`GZBUFSIZE`] (8192).
    ///
    /// Can be changed via `gzbuffer()` before any I/O operation. Once
    /// buffers are allocated (indicated by `size != 0`), this value
    /// cannot be changed.
    pub want: usize,

    /// Input buffer.
    ///
    /// Replaces C `unsigned char *in` (heap-allocated). When writing,
    /// this buffer is double-sized to support `gzprintf` formatting.
    /// When reading, it holds raw file data before decompression.
    pub input: Vec<u8>,

    /// Output buffer.
    ///
    /// Replaces C `unsigned char *out` (heap-allocated). When reading,
    /// this buffer is double-sized to support `gzungetc` push-back.
    /// When writing, it holds compressed data before flushing to disk.
    pub output: Vec<u8>,

    /// Direct (transparent) mode flag.
    ///
    /// For reading:
    /// - `1` = auto-detect mode (default) — look for gzip header
    /// - `-1` = gzip-only mode (set by 'G' flag)
    /// - `0` = gzip detected, processing gzip stream
    ///
    /// For writing:
    /// - `0` = gzip mode (compress data)
    /// - `1` = transparent mode (copy data without compression)
    pub direct: i32,

    // ── Just for reading ──

    /// Junk detection state for multi-member gzip streams.
    ///
    /// - `-1` = start (first member, haven't read anything yet)
    /// - `0` = inside a gzip stream
    /// - `1` = candidate for junk/trailing data between members
    pub junk: i32,

    /// How data is currently being processed during reading.
    ///
    /// Controls the auto-detection pipeline: [`Look`](GzHow::Look) to
    /// scan for a gzip header, [`Copy`](GzHow::Copy) for transparent
    /// pass-through, or [`Gzip`](GzHow::Gzip) for decompression.
    pub how: GzHow,

    /// `true` if `EAGAIN` or `EWOULDBLOCK` occurred on the last I/O.
    ///
    /// Used for non-blocking file descriptor support. When set, the
    /// next I/O attempt may succeed where the previous one stalled.
    pub again: bool,

    /// File offset where the gzip data started, used for rewinding.
    ///
    /// Recorded after opening so that `gzrewind()` can seek back to
    /// the beginning of the compressed data.
    pub start: i64,

    /// `true` if the end of the input file has been reached.
    ///
    /// Set when a `read()` call returns zero bytes. Once set, no more
    /// file I/O is attempted — only buffered data is returned.
    pub eof: bool,

    /// `true` if a read was requested past the end of the data.
    ///
    /// Set when the caller tries to read after all data (including
    /// buffered data) has been consumed. This is the flag returned
    /// by `gzeof()`.
    pub past: bool,

    // ── Just for writing ──

    /// Compression level for the deflate engine.
    ///
    /// Valid range: `-1` ([`Z_DEFAULT_COMPRESSION`](constants::Z_DEFAULT_COMPRESSION))
    /// through `9` (`Z_BEST_COMPRESSION`). Set during construction and
    /// changeable via `gzsetparams()`.
    pub level: i32,

    /// Compression strategy for the deflate engine.
    ///
    /// One of [`Z_DEFAULT_STRATEGY`](constants::Z_DEFAULT_STRATEGY),
    /// `Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE`, or `Z_FIXED`. Set during
    /// construction and changeable via `gzsetparams()`.
    pub strategy: i32,

    /// `true` if a deflate reset is pending after a `Z_FINISH` flush.
    ///
    /// After completing a gzip member with `Z_FINISH`, the deflate
    /// state must be reset before starting a new member. This flag
    /// defers the reset until the next write operation.
    pub reset: bool,

    // ── Seek request ──

    /// Amount of data to skip, in bytes.
    ///
    /// For forward seeks, this is the number of bytes to discard from
    /// the output. For backward seeks, the file is rewound first and
    /// then `skip` bytes are consumed. A value of zero means no skip
    /// is pending.
    pub skip: i64,

    // ── Error information ──

    /// Error code from the last operation.
    ///
    /// Holds one of the `Z_*` return codes (e.g., [`Z_OK`](constants::Z_OK),
    /// `Z_ERRNO`, `Z_STREAM_ERROR`). Reset to `Z_OK` by `gz_error()` or
    /// `gzclearerr()`.
    pub err: i32,

    /// Error message from the last operation, if any.
    ///
    /// When an error occurs, this holds a descriptive message formatted
    /// as `"{path}: {description}"`. Replaces C's heap-allocated
    /// `char *msg` with an owned `Option<String>`.
    pub msg: Option<String>,

    // ── zlib inflate or deflate stream ──

    /// Embedded compression/decompression stream.
    ///
    /// Owned in-place (not behind a pointer), mirroring the C pattern
    /// of `z_stream strm` embedded directly in `gz_state`. The stream
    /// provides `total_in`/`total_out` counters, `avail_in` tracking,
    /// and `adler`/`data_type` feedback used by the gz read/write
    /// pipelines.
    pub strm: ZStream,
}

impl GzState {
    /// Creates a new [`GzState`] configured for reading.
    ///
    /// Initializes the state with [`GzMode::Read`], default buffer size
    /// [`GZBUFSIZE`], auto-detect mode (`direct = 1`), and empty buffers.
    /// Buffers are allocated lazily on the first read (in `gz_look()`).
    ///
    /// This corresponds to the read-mode initialization path in C
    /// `gz_open()` (`gzlib.c` lines 86–285).
    ///
    /// # Arguments
    ///
    /// * `file` — Owned file handle for the gzip file to read.
    /// * `path` — File path string used in error messages.
    #[must_use]
    pub fn new_reader(file: File, path: String) -> Self {
        Self {
            x: GzExposed::new(),
            mode: GzMode::Read,
            file,
            path,
            size: 0,
            want: GZBUFSIZE,
            input: Vec::new(),
            output: Vec::new(),
            direct: 1,   // auto-detect for reading (gzlib.c line 189)
            junk: -1,    // mark first member (gzlib.c line 75)
            how: GzHow::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            level: constants::Z_DEFAULT_COMPRESSION,
            strategy: constants::Z_DEFAULT_STRATEGY,
            reset: false,
            skip: 0,
            err: constants::Z_OK,
            msg: None,
            strm: ZStream::new(),
        }
    }

    /// Creates a new [`GzState`] configured for writing.
    ///
    /// Initializes the state with [`GzMode::Write`], the specified
    /// compression level and strategy, and empty buffers. Buffers are
    /// allocated lazily on the first write (in `gz_init()`).
    ///
    /// This corresponds to the write-mode initialization path in C
    /// `gz_open()` (`gzlib.c` lines 86–285).
    ///
    /// # Arguments
    ///
    /// * `file` — Owned file handle for the gzip file to write.
    /// * `path` — File path string used in error messages.
    /// * `level` — Compression level (`-1` for default, `0`–`9` for explicit).
    /// * `strategy` — Compression strategy (`Z_DEFAULT_STRATEGY`, etc.).
    /// * `direct` — If `true`, write data transparently without compression.
    #[must_use]
    pub fn new_writer(
        file: File,
        path: String,
        level: i32,
        strategy: i32,
        direct: bool,
    ) -> Self {
        Self {
            x: GzExposed::new(),
            mode: GzMode::Write,
            file,
            path,
            size: 0,
            want: GZBUFSIZE,
            input: Vec::new(),
            output: Vec::new(),
            direct: i32::from(direct),
            junk: 0,
            how: GzHow::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            level,
            strategy,
            reset: false,
            skip: 0,
            err: constants::Z_OK,
            msg: None,
            strm: ZStream::new(),
        }
    }

    /// Creates a new [`GzState`] configured for appending.
    ///
    /// Seeks to the end of the file first (so that `gzoffset()` returns
    /// the correct value), then configures the state identically to
    /// [`new_writer`](GzState::new_writer) with [`GzMode::Write`].
    ///
    /// This corresponds to the append-mode path in C `gz_open()`
    /// (`gzlib.c` lines 269–272): the file is seeked to the end and
    /// the mode is immediately converted from `GZ_APPEND` to `GZ_WRITE`.
    ///
    /// # Arguments
    ///
    /// * `file` — Owned file handle for the gzip file to append to.
    /// * `path` — File path string used in error messages.
    /// * `level` — Compression level (`-1` for default, `0`–`9` for explicit).
    /// * `strategy` — Compression strategy (`Z_DEFAULT_STRATEGY`, etc.).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the seek-to-end operation fails.
    pub fn new_appender(
        mut file: File,
        path: String,
        level: i32,
        strategy: i32,
    ) -> io::Result<Self> {
        // Seek to end so gzoffset() is correct (gzlib.c line 270)
        file.seek(SeekFrom::End(0))?;

        // Convert to write mode immediately (gzlib.c line 271)
        Ok(Self {
            x: GzExposed::new(),
            mode: GzMode::Write,
            file,
            path,
            size: 0,
            want: GZBUFSIZE,
            input: Vec::new(),
            output: Vec::new(),
            direct: 0,
            junk: 0,
            how: GzHow::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            level,
            strategy,
            reset: false,
            skip: 0,
            err: constants::Z_OK,
            msg: None,
            strm: ZStream::new(),
        })
    }

    /// Returns `true` if buffers have been allocated.
    ///
    /// Buffers are allocated lazily on the first I/O operation. A return
    /// value of `false` means the state has been constructed but no data
    /// has been read or written yet.
    ///
    /// Equivalent to checking `state->size != 0` in C.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.size != 0
    }

    /// Records the current file position as the rewind target.
    ///
    /// Called after opening a file for reading to record where the gzip
    /// data starts. The `start` position is used by `gzrewind()` to
    /// seek back to the beginning of the compressed data.
    ///
    /// Corresponds to C `gzlib.c` lines 275–278:
    /// ```c
    /// state->start = LSEEK(state->fd, 0, SEEK_CUR);
    /// if (state->start == -1) state->start = 0;
    /// ```
    ///
    /// # Errors
    ///
    /// If the file position query fails, `start` is set to zero
    /// (matching the C fallback behavior) and `Ok(())` is returned.
    pub fn record_start_position(&mut self) {
        if let Ok(pos) = self.file.stream_position() {
            // File positions fit in i64 on all practical platforms.
            // Fall back to 0 for positions exceeding i64::MAX.
            self.start = i64::try_from(pos).unwrap_or(0);
        } else {
            // Fallback: treat unknown position as zero (gzlib.c line 277)
            self.start = 0;
        }
    }
}

// ─── Drop ─────────────────────────────────────────────────────────────────────

/// Custom [`Drop`] implementation for [`GzState`].
///
/// In C, `gzclose_r()` (`gzread.c` lines 645–668) and `gzclose_w()`
/// (`gzwrite.c` lines 667–700) must manually free five resources:
///
/// 1. `inflateEnd()` / `deflateEnd()` — stream cleanup
/// 2. `free(state->out)` — output buffer
/// 3. `free(state->in)` — input buffer
/// 4. `free(state->path)` — path string
/// 5. `close(state->fd)` — file descriptor
/// 6. `free(state)` — the state struct itself
///
/// In Rust, ALL of this is automatic through ownership:
///
/// - [`ZStream`] `Drop` handles inflate/deflate state cleanup
/// - `Vec<u8>` `Drop` frees input and output buffers
/// - [`String`] `Drop` frees the path string
/// - [`File`] `Drop` closes the file descriptor
/// - The struct itself is freed by the allocator
///
/// The explicit `Drop` implementation eagerly clears the error message
/// to ensure deterministic cleanup ordering.
impl Drop for GzState {
    fn drop(&mut self) {
        // Eagerly clear the error message. All other resources (File,
        // Vec<u8>, String, ZStream) are cleaned up automatically by
        // their own Drop implementations in field declaration order.
        self.msg = None;
    }
}

// ─── Debug ────────────────────────────────────────────────────────────────────

/// Custom [`Debug`] implementation for [`GzState`].
///
/// Avoids printing potentially large buffer contents (`input`, `output`),
/// instead showing their lengths. All other important fields are printed
/// to aid in diagnostics without overwhelming log output.
impl fmt::Debug for GzState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GzState")
            .field("mode", &self.mode)
            .field("path", &self.path)
            .field("size", &self.size)
            .field("want", &self.want)
            .field("input_len", &self.input.len())
            .field("output_len", &self.output.len())
            .field("direct", &self.direct)
            .field("how", &self.how)
            .field("junk", &self.junk)
            .field("again", &self.again)
            .field("start", &self.start)
            .field("eof", &self.eof)
            .field("past", &self.past)
            .field("level", &self.level)
            .field("strategy", &self.strategy)
            .field("reset", &self.reset)
            .field("skip", &self.skip)
            .field("err", &self.err)
            .field("msg", &self.msg)
            .field("x", &self.x)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_gz_mode_equality() {
        assert_eq!(GzMode::None, GzMode::None);
        assert_ne!(GzMode::Read, GzMode::Write);
        assert_ne!(GzMode::Write, GzMode::Append);
    }

    #[test]
    fn test_gz_how_equality() {
        assert_eq!(GzHow::Look, GzHow::Look);
        assert_ne!(GzHow::Copy, GzHow::Gzip);
    }

    #[test]
    fn test_gz_exposed_new() {
        let exposed = GzExposed::new();
        assert_eq!(exposed.have, 0);
        assert_eq!(exposed.next, 0);
        assert_eq!(exposed.pos, 0);
    }

    #[test]
    fn test_gz_exposed_default() {
        let exposed = GzExposed::default();
        assert_eq!(exposed.have, 0);
        assert_eq!(exposed.next, 0);
        assert_eq!(exposed.pos, 0);
    }

    #[test]
    fn test_gz_exposed_reset() {
        let mut exposed = GzExposed {
            have: 42,
            next: 10,
            pos: 1024,
        };
        exposed.reset();
        assert_eq!(exposed.have, 0);
        assert_eq!(exposed.next, 0);
        assert_eq!(exposed.pos, 0);
    }

    #[test]
    fn test_gzbufsize_value() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    #[test]
    fn test_new_reader_defaults() {
        // Create a temporary file for the reader
        let mut tmpfile = tempfile::tempfile().expect("create temp file");
        tmpfile.write_all(b"test data").expect("write temp");

        let state = GzState::new_reader(tmpfile, "/tmp/test.gz".to_string());
        assert_eq!(state.mode, GzMode::Read);
        assert_eq!(state.size, 0);
        assert_eq!(state.want, GZBUFSIZE);
        assert!(state.input.is_empty());
        assert!(state.output.is_empty());
        assert_eq!(state.direct, 1); // auto-detect
        assert_eq!(state.junk, -1);  // mark first member
        assert_eq!(state.how, GzHow::Look);
        assert!(!state.again);
        assert_eq!(state.start, 0);
        assert!(!state.eof);
        assert!(!state.past);
        assert_eq!(state.level, constants::Z_DEFAULT_COMPRESSION);
        assert_eq!(state.strategy, constants::Z_DEFAULT_STRATEGY);
        assert!(!state.reset);
        assert_eq!(state.skip, 0);
        assert_eq!(state.err, constants::Z_OK);
        assert!(state.msg.is_none());
        assert!(!state.is_initialized());
    }

    #[test]
    fn test_new_writer_defaults() {
        let tmpfile = tempfile::tempfile().expect("create temp file");
        let state = GzState::new_writer(
            tmpfile,
            "/tmp/test.gz".to_string(),
            constants::Z_DEFAULT_COMPRESSION,
            constants::Z_DEFAULT_STRATEGY,
            false,
        );
        assert_eq!(state.mode, GzMode::Write);
        assert_eq!(state.size, 0);
        assert_eq!(state.want, GZBUFSIZE);
        assert_eq!(state.direct, 0); // gzip mode
        assert_eq!(state.level, constants::Z_DEFAULT_COMPRESSION);
        assert_eq!(state.strategy, constants::Z_DEFAULT_STRATEGY);
        assert!(!state.is_initialized());
    }

    #[test]
    fn test_new_writer_direct_mode() {
        let tmpfile = tempfile::tempfile().expect("create temp file");
        let state = GzState::new_writer(
            tmpfile,
            "/tmp/test.gz".to_string(),
            0,
            0,
            true,
        );
        assert_eq!(state.direct, 1); // transparent mode
    }

    #[test]
    fn test_new_appender() {
        let mut tmpfile = tempfile::tempfile().expect("create temp file");
        tmpfile.write_all(b"existing data").expect("write");

        let state = GzState::new_appender(
            tmpfile,
            "/tmp/test.gz".to_string(),
            constants::Z_DEFAULT_COMPRESSION,
            constants::Z_DEFAULT_STRATEGY,
        );
        assert!(state.is_ok());
        let state = state.expect("appender creation");
        assert_eq!(state.mode, GzMode::Write);
        assert_eq!(state.direct, 0);
    }

    #[test]
    fn test_is_initialized() {
        let tmpfile = tempfile::tempfile().expect("create temp file");
        let mut state = GzState::new_reader(tmpfile, String::new());
        assert!(!state.is_initialized());
        state.size = 8192;
        assert!(state.is_initialized());
    }

    #[test]
    fn test_record_start_position() {
        let mut tmpfile = tempfile::tempfile().expect("create temp file");
        tmpfile.write_all(b"some header data").expect("write");

        let mut state = GzState::new_reader(tmpfile, String::new());
        state.record_start_position();
        // File position after writing 16 bytes then creating reader
        // should be 16 (writer left position at end)
        assert_eq!(state.start, 16);
    }

    #[test]
    fn test_debug_format() {
        let tmpfile = tempfile::tempfile().expect("create temp file");
        let state = GzState::new_reader(tmpfile, "/tmp/debug_test.gz".to_string());
        let debug_output = format!("{state:?}");
        // Should contain key fields but not raw buffer contents
        assert!(debug_output.contains("GzState"));
        assert!(debug_output.contains("Read"));
        assert!(debug_output.contains("/tmp/debug_test.gz"));
        assert!(debug_output.contains("input_len"));
        assert!(debug_output.contains("output_len"));
    }

    #[test]
    fn test_gz_mode_debug() {
        assert_eq!(format!("{:?}", GzMode::None), "None");
        assert_eq!(format!("{:?}", GzMode::Read), "Read");
        assert_eq!(format!("{:?}", GzMode::Write), "Write");
        assert_eq!(format!("{:?}", GzMode::Append), "Append");
    }

    #[test]
    fn test_gz_how_debug() {
        assert_eq!(format!("{:?}", GzHow::Look), "Look");
        assert_eq!(format!("{:?}", GzHow::Copy), "Copy");
        assert_eq!(format!("{:?}", GzHow::Gzip), "Gzip");
    }

    #[test]
    fn test_gz_mode_clone() {
        let mode = GzMode::Read;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_gz_how_clone() {
        let how = GzHow::Gzip;
        let cloned = how;
        assert_eq!(how, cloned);
    }

    #[test]
    fn test_gz_exposed_clone() {
        let exposed = GzExposed {
            have: 10,
            next: 5,
            pos: 100,
        };
        let cloned = exposed.clone();
        assert_eq!(cloned.have, 10);
        assert_eq!(cloned.next, 5);
        assert_eq!(cloned.pos, 100);
    }
}
