// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip file I/O state structure — port of gzguts.h gz_state.

//! Gzip file I/O state structure.
//!
//! This module defines the core [`GzState`] struct that holds all state for
//! a gzip file I/O session, along with the [`GzMode`] and [`GzHow`] enums
//! that replace C integer constants.
//!
//! Port of C zlib's `gzguts.h` (217 lines), specifically the `gz_state`
//! struct (~25 fields) and associated constants.
//!
//! Feature-gated behind `gz-io` (applied at the module declaration level
//! in `src/gz/mod.rs` and `src/lib.rs`).
//!
//! ## C-to-Rust Translation
//!
//! | C Field / Constant | Rust Equivalent | Notes |
//! |--------------------|-----------------|-------|
//! | `GZ_NONE = 0` | [`GzMode::None`] | Enum replaces magic int |
//! | `GZ_READ = 7247` | [`GzMode::Read`] | Enum replaces magic int |
//! | `GZ_WRITE = 31153` | [`GzMode::Write`] | Enum replaces magic int |
//! | `GZ_APPEND = 1` | [`GzMode::Append`] | Enum replaces magic int |
//! | `LOOK = 0` | [`GzHow::Look`] | Enum replaces magic int |
//! | `COPY = 1` | [`GzHow::Copy`] | Enum replaces magic int |
//! | `GZIP = 2` | [`GzHow::Gzip`] | Enum replaces magic int |
//! | `GZBUFSIZE 8192` | [`GZBUFSIZE`] | Same value |
//! | `int fd` | `Option<File>` | RAII file handle |
//! | `char *path` | `PathBuf` | Owned path buffer |
//! | `unsigned char *in` | `Vec<u8>` | Owned input buffer |
//! | `unsigned char *out` | `Vec<u8>` | Owned output buffer |
//! | `z_stream strm` | [`ZStream`] | Embedded (not pointer) |
//! | `int err` | `Option<ZlibError>` | Type-safe error |
//! | `char *msg` | `Option<String>` | Owned error message |
//! | `off64_t skip` | `u64` | 64-bit offset |
//! | `off64_t start` | `u64` | 64-bit offset |
//! | `off64_t pos` | `u64` | 64-bit position |
//! | `int (boolean)` | `bool` | Native Rust bool |
//!
//! ## Ownership Model
//!
//! ```text
//! GzState (owns everything)
//! ├── File handle (Option<File>, auto-close on drop)
//! ├── in_buf (Vec<u8>, auto-free on drop)
//! ├── out_buf (Vec<u8>, auto-free on drop)
//! ├── ZStream (embedded, owns internal state)
//! └── path (PathBuf, auto-free on drop)
//! ```
//!
//! When a `GzState` is dropped, Rust's ownership system automatically
//! closes the file handle, deallocates all buffers, and cleans up the
//! embedded `ZStream` — replacing the manual `free()` calls in C zlib's
//! `gzclose_r()` and `gzclose_w()`.

// Suppress dead-code warnings: fields and types are consumed by sibling gz
// modules (open.rs, read.rs, write.rs, close.rs) that are created in parallel.
#![allow(dead_code)]

use std::fs::File;
use std::path::PathBuf;

use crate::constants::{Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY};
use crate::error::ZlibError;
use crate::stream::ZStream;

// ===========================================================================
// Constants
// ===========================================================================

/// Default gzip I/O buffer size in bytes.
///
/// This is the initial buffer allocation for gzip file read/write operations.
/// The actual buffer may be resized via `gz_buffer()` before the first I/O
/// operation. When reading, the output buffer is double this size for
/// look-ahead capacity.
///
/// # C Reference
///
/// ```c
/// /* gzguts.h line 156 */
/// #define GZBUFSIZE 8192
/// ```
pub const GZBUFSIZE: usize = 8192;

// ===========================================================================
// GzMode — gzip file operation mode
// ===========================================================================

/// Gzip file operation mode.
///
/// Replaces the C integer constants used for corruption detection:
///
/// | C Constant | Value | Rust Variant | Notes |
/// |------------|-------|--------------|-------|
/// | `GZ_NONE` | 0 | [`GzMode::None`] | Not initialised |
/// | `GZ_READ` | 7247 | [`GzMode::Read`] | `'G' * 256 + 'Z' - 1` |
/// | `GZ_WRITE` | 31153 | [`GzMode::Write`] | `'z' * 256 + 'l' + 1` |
/// | `GZ_APPEND` | 1 | [`GzMode::Append`] | Write in append mode |
///
/// In C, the magic numbers were chosen to detect memory corruption of the
/// `gz_state` structure (accidental use of an invalid mode would be unlikely
/// to produce 7247 or 31153). In Rust, the enum type system provides this
/// guarantee at compile time.
///
/// # Examples
///
/// ```
/// use zlib_rs::gz::state::GzMode;
///
/// let mode = GzMode::Read;
/// assert_ne!(mode, GzMode::Write);
/// assert_eq!(GzMode::default(), GzMode::None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GzMode {
    /// Not yet initialised or invalid state.
    ///
    /// Corresponds to C `GZ_NONE = 0`. A `GzState` with this mode has not
    /// been opened for any operation and must not be used for reading or
    /// writing.
    None,

    /// File opened for reading (decompression).
    ///
    /// Corresponds to C `GZ_READ = 7247`. The read pipeline (LOOK → COPY
    /// or GZIP) is used to produce decompressed output.
    Read,

    /// File opened for writing (compression).
    ///
    /// Corresponds to C `GZ_WRITE = 31153`. Data is compressed via the
    /// deflate engine and written to the underlying file.
    Write,

    /// File opened for appending.
    ///
    /// Corresponds to C `GZ_APPEND = 1`. Similar to [`Write`](GzMode::Write)
    /// but the file is opened in append mode so new compressed data is
    /// appended after existing content.
    Append,
}

impl Default for GzMode {
    /// Returns [`GzMode::None`] — the default uninitialised state.
    #[inline]
    fn default() -> Self {
        GzMode::None
    }
}

impl core::fmt::Display for GzMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GzMode::None => f.write_str("none"),
            GzMode::Read => f.write_str("read"),
            GzMode::Write => f.write_str("write"),
            GzMode::Append => f.write_str("append"),
        }
    }
}

// ===========================================================================
// GzHow — read pipeline state
// ===========================================================================

/// Read pipeline state: how to produce output data.
///
/// This enum drives the gzip read state machine implemented in
/// `src/gz/read.rs`. When a file is first opened for reading, the pipeline
/// starts in [`Look`](GzHow::Look) mode to detect whether the input is
/// gzip-compressed or plain data.
///
/// Replaces the C integer constants:
///
/// | C Constant | Value | Rust Variant | Meaning |
/// |------------|-------|--------------|---------|
/// | `LOOK` | 0 | [`GzHow::Look`] | Examining first bytes |
/// | `COPY` | 1 | [`GzHow::Copy`] | Direct byte-for-byte copy |
/// | `GZIP` | 2 | [`GzHow::Gzip`] | Inflate decompression |
///
/// Used only in read mode to track the current processing strategy.
///
/// # Examples
///
/// ```
/// use zlib_rs::gz::state::GzHow;
///
/// let how = GzHow::default();
/// assert_eq!(how, GzHow::Look);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GzHow {
    /// Initial state: look at data to determine format.
    ///
    /// The read pipeline examines the first bytes of input to detect the
    /// gzip magic number (`0x1f 0x8b`). Based on the result, the state
    /// transitions to either [`Copy`](GzHow::Copy) or
    /// [`Gzip`](GzHow::Gzip).
    Look,

    /// Direct copy: data is not gzip-compressed.
    ///
    /// Input bytes are passed through to the output buffer without any
    /// decompression. This occurs when the input file does not start with
    /// a valid gzip header (transparent read mode).
    Copy,

    /// Gzip decompression: inflate compressed data.
    ///
    /// A valid gzip header was detected. The inflate engine decompresses
    /// input data from the file into the output buffer. At the end of a
    /// gzip member, the state returns to [`Look`](GzHow::Look) to handle
    /// concatenated gzip streams.
    Gzip,
}

impl Default for GzHow {
    /// Returns [`GzHow::Look`] — the initial format-detection state.
    #[inline]
    fn default() -> Self {
        GzHow::Look
    }
}

impl core::fmt::Display for GzHow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GzHow::Look => f.write_str("look"),
            GzHow::Copy => f.write_str("copy"),
            GzHow::Gzip => f.write_str("gzip"),
        }
    }
}

// ===========================================================================
// GzState — complete gzip file I/O session state
// ===========================================================================

/// Complete state for a gzip file I/O session.
///
/// This is the Rust equivalent of C zlib's `gz_state` struct defined in
/// `gzguts.h` (lines 170–203). It holds all internal buffers, the file
/// handle, compression/decompression parameters, error state, and the
/// embedded [`ZStream`] used to drive the inflate or deflate engines.
///
/// # Field Organisation
///
/// Fields are grouped into four categories matching the C structure layout:
///
/// 1. **Exposed fields** — The `gzFile_s` sub-struct equivalent (`have`,
///    `next`, `pos`) used by the fast-path `gzgetc` macro in C.
/// 2. **Shared fields** — Used by both read and write modes (`mode`, `file`,
///    `path`, `size`, `want`, `in_buf`, `out_buf`, `direct`, `again`,
///    `skip`, `seek`, `err`, `msg`, `strm`).
/// 3. **Read-only fields** — Used only in read mode (`how`, `start`, `eof`,
///    `past`, `junk`).
/// 4. **Write-only fields** — Used only in write mode (`level`, `strategy`,
///    `reset_pending`).
///
/// # C Structure Mapping
///
/// ```text
/// C gz_state                    Rust GzState
/// ─────────────────────         ─────────────────────
/// struct gzFile_s x             have: usize
///   x.have (unsigned)           next: usize
///   x.next (unsigned char*)     pos: u64
///   x.pos  (z_off64_t)
/// int mode                      mode: GzMode
/// int fd                        file: Option<File>
/// char *path                    path: PathBuf
/// unsigned size                 size: usize
/// unsigned want                 want: usize
/// unsigned char *in             in_buf: Vec<u8>
/// unsigned char *out            out_buf: Vec<u8>
/// int direct                    direct: bool
/// int again                     again: bool
/// z_off64_t skip                skip: u64
/// int seek                      seek: bool
/// int err                       err: Option<ZlibError>
/// char *msg                     msg: Option<String>
/// z_stream strm                 strm: ZStream
/// int how                       how: GzHow
/// z_off64_t start               start: u64
/// int eof                       eof: bool
/// int past                      past: bool
/// int junk                      junk: bool
/// int level                     level: i32
/// int strategy                  strategy: i32
/// int reset                     reset_pending: bool
/// ```
///
/// # Ownership
///
/// `GzState` owns all its resources. When dropped, Rust's ownership system
/// automatically:
/// - Closes the file handle (`Option<File>` drops)
/// - Deallocates input and output buffers (`Vec<u8>` drops)
/// - Cleans up the embedded `ZStream` (which drops its internal
///   `DeflateState` or `InflateState`)
/// - Frees the path string (`PathBuf` drops)
/// - Frees the error message (`Option<String>` drops)
///
/// This replaces the manual `free()` calls in C zlib's `gzclose_r()` and
/// `gzclose_w()` functions.
pub struct GzState {
    // =======================================================================
    // Exposed fields (gzFile_s equivalent)
    // =======================================================================

    /// Number of bytes available in the output buffer.
    ///
    /// In read mode, this is the count of decompressed bytes ready for the
    /// caller at position `out_buf[next..next+have]`. In write mode, this
    /// tracks buffered but not-yet-compressed input bytes.
    ///
    /// C equivalent: `state->x.have` (unsigned).
    pub(crate) have: usize,

    /// Current read position in the output buffer (index into `out_buf`).
    ///
    /// Points to the next byte to deliver to the caller. Advances as bytes
    /// are consumed via `gz_read` or `gz_getc`.
    ///
    /// C equivalent: `state->x.next` (unsigned char pointer, here an index).
    pub(crate) next: usize,

    /// Current position in the uncompressed data stream.
    ///
    /// Tracks the total number of uncompressed bytes delivered to (read mode)
    /// or received from (write mode) the caller. Used by `gz_tell()` and
    /// `gz_seek()`.
    ///
    /// C equivalent: `state->x.pos` (z_off64_t).
    pub(crate) pos: u64,

    // =======================================================================
    // Shared fields (used for both reading and writing)
    // =======================================================================

    /// File operation mode (Read, Write, Append, or None).
    ///
    /// Set during `gz_open()` / `gz_dopen()` based on the mode string.
    /// Determines which code path (read pipeline vs write pipeline) is used
    /// for all subsequent operations.
    ///
    /// C equivalent: `state->mode` (int: GZ_NONE/GZ_READ/GZ_WRITE/GZ_APPEND).
    pub(crate) mode: GzMode,

    /// Underlying file handle (owned).
    ///
    /// Wrapped in `Option` so it can be `take()`-en during close operations
    /// to transfer ownership. `None` before open or after close.
    ///
    /// C equivalent: `state->fd` (int file descriptor).
    pub(crate) file: Option<File>,

    /// File path for error messages (or `"<fd:N>"` for `gz_dopen`).
    ///
    /// Stored for diagnostic purposes — included in error messages generated
    /// by `gz_error()` to help callers identify which file caused a problem.
    ///
    /// C equivalent: `state->path` (char pointer, heap-allocated copy).
    pub(crate) path: PathBuf,

    /// Actual buffer size (0 if buffers not yet allocated; set on first I/O).
    ///
    /// Once buffers are allocated (in `gz_init` for write or `gz_look` for
    /// read), this is set to the value of `want`. A value of 0 indicates
    /// that lazy buffer allocation has not yet occurred.
    ///
    /// C equivalent: `state->size` (unsigned).
    pub(crate) size: usize,

    /// Requested/desired buffer size (default [`GZBUFSIZE`] = 8192).
    ///
    /// Can be changed via `gz_buffer()` before the first I/O operation
    /// (while `size == 0`). After first I/O, `want` is frozen.
    ///
    /// C equivalent: `state->want` (unsigned).
    pub(crate) want: usize,

    /// Input buffer: raw bytes read from file (for inflate or direct copy).
    ///
    /// Allocated lazily on first I/O operation. In read mode, holds raw
    /// (possibly compressed) bytes from the file. In write mode, holds
    /// uncompressed data from the caller before feeding to deflate.
    ///
    /// Starts as an empty `Vec` and is resized to `want` (or `want * 2`
    /// in some write paths) on first use.
    ///
    /// C equivalent: `state->in` (unsigned char pointer, malloc'd).
    pub(crate) in_buf: Vec<u8>,

    /// Output buffer: decompressed bytes (read) or compressed bytes (write).
    ///
    /// In read mode, holds decompressed data ready for the caller. Sized to
    /// `want * 2` for look-ahead capacity. In write mode, holds compressed
    /// output from deflate before writing to file. Sized to `want`.
    ///
    /// C equivalent: `state->out` (unsigned char pointer, malloc'd).
    pub(crate) out_buf: Vec<u8>,

    /// True if data is not gzip-compressed (transparent copy mode).
    ///
    /// In read mode: set when `gz_look()` determines the input file is not
    /// gzip-formatted, causing bytes to be copied through without
    /// decompression.
    ///
    /// In write mode: set when the `'T'` flag is specified in the mode
    /// string, causing raw bytes to be written without compression.
    ///
    /// C equivalent: `state->direct` (int, 0 or 1 after initialisation).
    pub(crate) direct: bool,

    /// True if the last I/O operation was interrupted (EAGAIN/EWOULDBLOCK).
    ///
    /// Set by `gz_load()` when a non-blocking file descriptor returns
    /// `WouldBlock`. Cleared on the next successful I/O operation.
    ///
    /// C equivalent: `state->again` (int boolean).
    pub(crate) again: bool,

    /// Number of bytes to skip for a pending seek in read mode.
    ///
    /// Set by `gz_seek()` when a forward seek is requested. The actual
    /// skipping (reading and discarding decompressed bytes) is deferred
    /// until the next read operation via `gz_skip()`.
    ///
    /// In write mode, this is the number of zero bytes to insert at the
    /// current position before the next write (filling the seek gap).
    ///
    /// C equivalent: `state->skip` (z_off64_t).
    pub(crate) skip: u64,

    /// True if a seek request is pending.
    ///
    /// Set by `gz_seek()` and cleared when the seek is executed (by
    /// `gz_skip()` in read mode or `gz_zero()` in write mode) at the
    /// start of the next read/write operation.
    ///
    /// C equivalent: `state->seek` (int boolean).
    pub(crate) seek: bool,

    /// Current error state (`None` = no error, i.e. `Z_OK`).
    ///
    /// Set by `gz_error()` when an operation fails. Checked by various
    /// pipeline functions to short-circuit when an error has already
    /// occurred.
    ///
    /// C equivalent: `state->err` (int: Z_OK, Z_ERRNO, Z_STREAM_ERROR,
    /// Z_DATA_ERROR, Z_MEM_ERROR, Z_BUF_ERROR).
    pub(crate) err: Option<ZlibError>,

    /// Error message string (`None` = no message).
    ///
    /// Set alongside `err` by `gz_error()` to provide a human-readable
    /// description of the error. For `Errno` errors, this is the OS error
    /// string; for other errors, it is a descriptive message from the
    /// library.
    ///
    /// C equivalent: `state->msg` (char pointer, heap-allocated or null).
    pub(crate) msg: Option<String>,

    /// Embedded z_stream for inflate (read) or deflate (write).
    ///
    /// The central exchange structure through which the inflate/deflate
    /// engines are driven. In read mode, `gz_decomp()` passes this to
    /// `inflate()`. In write mode, `gz_comp()` passes this to `deflate()`.
    ///
    /// Embedded in-place (not a pointer), matching C's `z_stream strm`
    /// field in `gz_state`. Owns its internal `DeflateState` or
    /// `InflateState` via the `StreamState` enum.
    ///
    /// C equivalent: `state->strm` (z_stream, in-place struct).
    pub(crate) strm: ZStream,

    // =======================================================================
    // Read-only fields (used only in read mode)
    // =======================================================================

    /// Read pipeline state: how to produce output (Look, Copy, Gzip).
    ///
    /// Drives the read state machine in `gz_fetch()`:
    /// - [`GzHow::Look`] — examine first bytes to detect gzip format
    /// - [`GzHow::Copy`] — pass through uncompressed data directly
    /// - [`GzHow::Gzip`] — decompress via the inflate engine
    ///
    /// Resets to `Look` at the end of each gzip member to handle
    /// concatenated gzip streams.
    ///
    /// C equivalent: `state->how` (int: LOOK=0, COPY=1, GZIP=2).
    pub(crate) how: GzHow,

    /// File offset where compressed data started (after the gzip header).
    ///
    /// Recorded after the gzip header is parsed in `gz_look()`. Used by
    /// `gz_rewind()` to seek back to the start of the data for re-reading.
    ///
    /// C equivalent: `state->start` (z_off64_t).
    pub(crate) start: u64,

    /// True if the end of the input file has been reached.
    ///
    /// Set by `gz_load()` when `read()` returns zero bytes. Once set,
    /// no more data will be read from the file.
    ///
    /// C equivalent: `state->eof` (int boolean).
    pub(crate) eof: bool,

    /// True if the caller has attempted to read past the end of data.
    ///
    /// Distinct from `eof`: `eof` means the file is exhausted, while
    /// `past` means the caller tried to read when there was nothing left.
    /// Used by `gz_eof()` to report the EOF condition.
    ///
    /// C equivalent: `state->past` (int boolean).
    pub(crate) past: bool,

    /// True if trailing junk was detected after a gzip stream end.
    ///
    /// In the C implementation this is a three-state `int` (-1 = first
    /// member, 0 = in gzip, 1 = junk detected). In the Rust translation,
    /// the "first member" state is handled implicitly during initialisation,
    /// and this field indicates whether non-gzip trailing data was found.
    ///
    /// C equivalent: `state->junk` (int: -1/0/1).
    pub(crate) junk: bool,

    // =======================================================================
    // Write-only fields (used only in write mode)
    // =======================================================================

    /// Compression level (0–9, or `Z_DEFAULT_COMPRESSION` = -1).
    ///
    /// Passed to `deflateInit2()` when the write pipeline is initialised
    /// in `gz_init()`. Can be changed mid-stream via `gz_setparams()`.
    ///
    /// - 0 = no compression (stored blocks)
    /// - 1 = best speed (greedy matching)
    /// - 9 = best compression (exhaustive lazy matching)
    /// - -1 = default (currently equivalent to level 6)
    ///
    /// C equivalent: `state->level` (int).
    pub(crate) level: i32,

    /// Compression strategy (`Z_DEFAULT_STRATEGY`, `Z_FILTERED`, etc.).
    ///
    /// Passed to `deflateInit2()` and can be changed mid-stream via
    /// `gz_setparams()`. Controls which compression function is selected
    /// (stored, fast, slow, huffman-only, RLE, fixed).
    ///
    /// C equivalent: `state->strategy` (int).
    pub(crate) strategy: i32,

    /// True if a `deflateReset` is pending after a completed stream.
    ///
    /// Set when `gz_comp()` completes a `Z_FINISH` flush, indicating that
    /// the deflate engine needs to be reset before the next write can
    /// proceed. The reset is performed at the start of the next write
    /// operation.
    ///
    /// C equivalent: `state->reset` (int boolean).
    pub(crate) reset_pending: bool,
}

// ===========================================================================
// GzState — construction
// ===========================================================================

impl GzState {
    /// Creates a new `GzState` with default values for the given mode.
    ///
    /// All buffers start empty and are allocated lazily on the first I/O
    /// operation (in `gz_init` for write mode or `gz_look` for read mode).
    /// The default requested buffer size is [`GZBUFSIZE`] (8192 bytes).
    ///
    /// # Arguments
    ///
    /// * `mode` — The operation mode (Read, Write, or Append).
    /// * `file` — The opened file handle (ownership transferred).
    /// * `path` — The file path for error diagnostics.
    ///
    /// # Defaults
    ///
    /// | Field | Default | C equivalent |
    /// |-------|---------|-------------|
    /// | `have` | 0 | `x.have = 0` |
    /// | `next` | 0 | `x.next = NULL` |
    /// | `pos` | 0 | `x.pos = 0` |
    /// | `size` | 0 | `state->size = 0` |
    /// | `want` | 8192 | `state->want = GZBUFSIZE` |
    /// | `level` | -1 | `Z_DEFAULT_COMPRESSION` |
    /// | `strategy` | 0 | `Z_DEFAULT_STRATEGY` |
    /// | `how` | Look | `LOOK = 0` |
    /// | `err` | None | `Z_OK = 0` |
    ///
    /// # C Reference
    ///
    /// Matches the initialisation pattern in `gzlib.c` `gz_open()` (lines
    /// 100–112), where the state is malloc'd and fields are set to zero or
    /// default values before `gz_reset()` is called.
    pub(crate) fn new(mode: GzMode, file: File, path: PathBuf) -> Self {
        GzState {
            // Exposed fields
            have: 0,
            next: 0,
            pos: 0,

            // Shared fields
            mode,
            file: Some(file),
            path,
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

            // Read-only fields
            how: GzHow::Look,
            start: 0,
            eof: false,
            past: false,
            junk: false,

            // Write-only fields
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            reset_pending: false,
        }
    }
}

// ===========================================================================
// GzState — trait implementations
// ===========================================================================

impl core::fmt::Debug for GzState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GzState")
            .field("have", &self.have)
            .field("next", &self.next)
            .field("pos", &self.pos)
            .field("mode", &self.mode)
            .field("file", &self.file.is_some())
            .field("path", &self.path)
            .field("size", &self.size)
            .field("want", &self.want)
            .field("in_buf.len", &self.in_buf.len())
            .field("out_buf.len", &self.out_buf.len())
            .field("direct", &self.direct)
            .field("again", &self.again)
            .field("skip", &self.skip)
            .field("seek", &self.seek)
            .field("err", &self.err)
            .field("msg", &self.msg)
            .field("strm", &"ZStream{..}")
            .field("how", &self.how)
            .field("start", &self.start)
            .field("eof", &self.eof)
            .field("past", &self.past)
            .field("junk", &self.junk)
            .field("level", &self.level)
            .field("strategy", &self.strategy)
            .field("reset_pending", &self.reset_pending)
            .finish()
    }
}

// Drop is automatically derived by the compiler for GzState. Each field's
// Drop implementation handles resource cleanup:
//
// - `file: Option<File>` — File::drop closes the file descriptor
// - `in_buf: Vec<u8>` — Vec::drop deallocates the heap buffer
// - `out_buf: Vec<u8>` — Vec::drop deallocates the heap buffer
// - `path: PathBuf` — PathBuf::drop frees the path string
// - `msg: Option<String>` — String::drop frees the message buffer
// - `strm: ZStream` — ZStream::drop frees internal compression state
//
// This replaces the manual free() calls in C zlib's gzclose_r (gzread.c
// lines 634-669) and gzclose_w (gzwrite.c lines 558-659).

// ===========================================================================
// Unit tests (in-crate, can access pub(crate) items)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonically-increasing counter for unique temp file names.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Helper: create a temporary file and return (File, PathBuf).
    /// Each call produces a unique file name to avoid races between
    /// tests running in parallel.
    fn temp_file() -> (File, PathBuf) {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "gz_state_test_{}_{id}",
            std::process::id()
        ));
        let file = File::create(&path).expect("create temp file");
        (file, path)
    }

    // ---------------------------------------------------------------
    // GZBUFSIZE
    // ---------------------------------------------------------------

    #[test]
    fn gzbufsize_matches_c_definition() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    // ---------------------------------------------------------------
    // GzMode
    // ---------------------------------------------------------------

    #[test]
    fn gz_mode_default_is_none() {
        assert_eq!(GzMode::default(), GzMode::None);
    }

    #[test]
    fn gz_mode_all_variants_distinct() {
        let modes = [GzMode::None, GzMode::Read, GzMode::Write, GzMode::Append];
        for (i, a) in modes.iter().enumerate() {
            for (j, b) in modes.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn gz_mode_display() {
        assert_eq!(format!("{}", GzMode::None), "none");
        assert_eq!(format!("{}", GzMode::Read), "read");
        assert_eq!(format!("{}", GzMode::Write), "write");
        assert_eq!(format!("{}", GzMode::Append), "append");
    }

    #[test]
    fn gz_mode_debug() {
        assert_eq!(format!("{:?}", GzMode::Read), "Read");
        assert_eq!(format!("{:?}", GzMode::Append), "Append");
    }

    #[test]
    fn gz_mode_copy() {
        let a = GzMode::Write;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn gz_mode_clone() {
        let a = GzMode::Read;
        #[allow(clippy::clone_on_copy)]
        let b = a.clone();
        assert_eq!(a, b);
    }

    // ---------------------------------------------------------------
    // GzHow
    // ---------------------------------------------------------------

    #[test]
    fn gz_how_default_is_look() {
        assert_eq!(GzHow::default(), GzHow::Look);
    }

    #[test]
    fn gz_how_all_variants_distinct() {
        let hows = [GzHow::Look, GzHow::Copy, GzHow::Gzip];
        for (i, a) in hows.iter().enumerate() {
            for (j, b) in hows.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn gz_how_display() {
        assert_eq!(format!("{}", GzHow::Look), "look");
        assert_eq!(format!("{}", GzHow::Copy), "copy");
        assert_eq!(format!("{}", GzHow::Gzip), "gzip");
    }

    #[test]
    fn gz_how_debug() {
        assert_eq!(format!("{:?}", GzHow::Gzip), "Gzip");
    }

    #[test]
    fn gz_how_copy_trait() {
        let a = GzHow::Gzip;
        let b = a;
        assert_eq!(a, b);
    }

    // ---------------------------------------------------------------
    // GzState::new
    // ---------------------------------------------------------------

    #[test]
    fn new_sets_mode() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.mode, GzMode::Read);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_stores_file() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Write, f, p.clone());
        assert!(s.file.is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_stores_path() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.path, p);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_exposed_fields_zeroed() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.have, 0);
        assert_eq!(s.next, 0);
        assert_eq!(s.pos, 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_buffers_empty() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert!(s.in_buf.is_empty());
        assert!(s.out_buf.is_empty());
        assert_eq!(s.size, 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_want_is_gzbufsize() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Write, f, p.clone());
        assert_eq!(s.want, GZBUFSIZE);
        assert_eq!(s.want, 8192);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_booleans_false() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert!(!s.direct);
        assert!(!s.again);
        assert!(!s.seek);
        assert!(!s.eof);
        assert!(!s.past);
        assert!(!s.junk);
        assert!(!s.reset_pending);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_offsets_zeroed() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.skip, 0);
        assert_eq!(s.start, 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_error_fields_none() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert!(s.err.is_none());
        assert!(s.msg.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_default_compression() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Write, f, p.clone());
        assert_eq!(s.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(s.level, -1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_default_strategy() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Write, f, p.clone());
        assert_eq!(s.strategy, Z_DEFAULT_STRATEGY);
        assert_eq!(s.strategy, 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_how_is_look() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.how, GzHow::Look);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_strm_is_default() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        assert_eq!(s.strm.avail_in, 0);
        assert_eq!(s.strm.avail_out, 0);
        assert_eq!(s.strm.total_in, 0);
        assert_eq!(s.strm.total_out, 0);
        assert!(s.strm.msg.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn new_with_all_modes() {
        for mode in [GzMode::Read, GzMode::Write, GzMode::Append, GzMode::None] {
            let (f, p) = temp_file();
            let s = GzState::new(mode, f, p.clone());
            assert_eq!(s.mode, mode);
            let _ = std::fs::remove_file(&p);
        }
    }

    // ---------------------------------------------------------------
    // Drop / RAII
    // ---------------------------------------------------------------

    #[test]
    fn drop_does_not_panic() {
        let (f, p) = temp_file();
        {
            let _s = GzState::new(GzMode::Write, f, p.clone());
        }
        // GzState dropped without panic — success
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn drop_closes_file() {
        let (f, p) = temp_file();
        // Write some data so we can verify the file was created
        let mut s = GzState::new(GzMode::Write, f, p.clone());
        if let Some(ref mut file) = s.file {
            let _ = file.write_all(b"test data");
        }
        drop(s);
        // File should still exist on disk (drop closes handle, doesn't delete)
        assert!(p.exists());
        let _ = std::fs::remove_file(&p);
    }

    // ---------------------------------------------------------------
    // Debug
    // ---------------------------------------------------------------

    #[test]
    fn debug_output_is_reasonable() {
        let (f, p) = temp_file();
        let s = GzState::new(GzMode::Read, f, p.clone());
        let dbg = format!("{s:?}");
        assert!(dbg.contains("GzState"));
        assert!(dbg.contains("mode: Read"));
        assert!(dbg.contains("how: Look"));
        assert!(dbg.contains("level: -1"));
        assert!(dbg.contains("strategy: 0"));
        let _ = std::fs::remove_file(&p);
    }
}
