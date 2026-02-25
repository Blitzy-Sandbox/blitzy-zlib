// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip file I/O module root — re-exports and std::io trait implementations.
// Port of C zlib's gzlib.c, gzread.c, gzwrite.c, gzclose.c, and gzguts.h.

//! Gzip file I/O operations.
//!
//! This module provides a stdio-like interface for reading and writing gzip
//! (`.gz`) files. It supports transparent reading of both gzip and non-gzip
//! files, buffered I/O, seeking within compressed streams, and dynamic
//! compression parameter changes.
//!
//! Port of C zlib's gzip file I/O layer:
//! - `gzlib.c` (609 lines) — open, seek, tell, error operations
//! - `gzread.c` (668 lines) — read pipeline with LOOK/COPY/GZIP state machine
//! - `gzwrite.c` (700 lines) — write pipeline with buffered compression
//! - `gzclose.c` (23 lines) — close dispatcher
//! - `gzguts.h` (216 lines) — internal state structure
//!
//! # Feature Gate
//!
//! This module is only available when the `gz-io` feature is enabled
//! (which is a default feature). The `gz-io` feature implies both `std`
//! and `gzip` features:
//!
//! ```toml
//! [dependencies]
//! zlib-rs = { version = "0.1", features = ["gz-io"] }
//! ```
//!
//! # Quick Start
//!
//! ## Writing compressed data
//!
//! ```rust,no_run
//! use zlib_rs::gz;
//!
//! // Open a file for writing with compression level 6
//! let mut f = gz::gz_open(std::path::Path::new("file.gz"), "wb6").unwrap();
//!
//! // Write data — it will be compressed automatically
//! gz::gz_write(&mut f, b"Hello, gzip world!").unwrap();
//!
//! // Close flushes remaining compressed data and writes the gzip trailer
//! gz::gz_close(&mut f).unwrap();
//! ```
//!
//! ## Reading compressed data
//!
//! ```rust,no_run
//! use zlib_rs::gz;
//!
//! // Open a gzip file for reading
//! let mut f = gz::gz_open(std::path::Path::new("file.gz"), "rb").unwrap();
//!
//! // Read decompressed data
//! let mut buf = [0u8; 256];
//! let n = gz::gz_read(&mut f, &mut buf).unwrap();
//! println!("Read {} decompressed bytes", n);
//!
//! // Close releases all resources
//! gz::gz_close(&mut f).unwrap();
//! ```
//!
//! ## Using `std::io` traits
//!
//! [`GzState`] implements [`std::io::Read`] and [`std::io::Write`], allowing
//! it to be used with any code that accepts generic I/O types:
//!
//! ```rust,no_run
//! use std::io::{Read, Write};
//! use zlib_rs::gz;
//!
//! // Write using std::io::Write
//! let mut f = gz::gz_open(std::path::Path::new("output.gz"), "wb").unwrap();
//! f.write_all(b"using std::io::Write!").unwrap();
//! f.flush().unwrap();
//! gz::gz_close(&mut f).unwrap();
//!
//! // Read using std::io::Read
//! let mut f = gz::gz_open(std::path::Path::new("output.gz"), "rb").unwrap();
//! let mut contents = Vec::new();
//! f.read_to_end(&mut contents).unwrap();
//! gz::gz_close(&mut f).unwrap();
//! ```
//!
//! # Architecture
//!
//! The module is organized into five submodules, each porting a specific C
//! source file:
//!
//! | Submodule | C Source | Responsibility |
//! |-----------|----------|----------------|
//! | [`state`] | `gzguts.h` | Core types: [`GzState`], [`GzMode`], [`GzHow`] |
//! | [`open`] | `gzlib.c` | File open, seek, tell, error management |
//! | [`read`] | `gzread.c` | Read pipeline with format auto-detection |
//! | [`write`] | `gzwrite.c` | Write pipeline with buffered compression |
//! | [`close`] | `gzclose.c` | Close dispatcher and cleanup |
//!
//! Internal helpers (e.g., `gz_load`, `gz_avail`, `gz_comp`, `gz_zero`) are
//! kept `pub(crate)` or private and are **not** re-exported from this module.
//! Users should access all public API functions through the `gz` namespace
//! (e.g., `zlib_rs::gz::gz_open`).

// ===========================================================================
// Submodule declarations
// ===========================================================================

/// Core gzip I/O types: [`GzState`], [`GzMode`], [`GzHow`], and the
/// [`GZBUFSIZE`](state::GZBUFSIZE) constant.
///
/// This module defines the central state structure and enums used by all
/// other gzip I/O submodules.
pub mod state;

/// File open, seek, tell, and error management operations.
///
/// Port of `gzlib.c` (609 lines). Provides [`gz_open`], [`gz_dopen`],
/// [`gz_buffer`], [`gz_rewind`], [`gz_seek`], [`gz_tell`], [`gz_offset`],
/// [`gz_eof`], [`gz_error_msg`], and [`gz_clearerr`].
pub mod open;

/// Read pipeline with LOOK/COPY/GZIP state machine.
///
/// Port of `gzread.c` (668 lines). Provides [`gz_read`], [`gz_fread`],
/// [`gz_getc`], [`gz_ungetc`], [`gz_gets`], and [`gz_direct`].
pub mod read;

/// Write pipeline with buffered compression.
///
/// Port of `gzwrite.c` (700 lines). Provides [`gz_write`], [`gz_fwrite`],
/// [`gz_putc`], [`gz_puts`], [`gz_printf`], [`gz_flush`], and
/// [`gz_setparams`].
pub mod write;

/// Close dispatcher routing to read-close or write-close.
///
/// Port of `gzclose.c` (23 lines) plus close helpers from `gzread.c` and
/// `gzwrite.c`. Provides [`gz_close`].
pub mod close;

// ===========================================================================
// Public re-exports: types from state.rs
// ===========================================================================

/// Re-export of [`state::GzState`] — the core gzip file I/O session state.
pub use state::GzState;

/// Re-export of [`state::GzMode`] — gzip file operation mode enum.
pub use state::GzMode;

/// Re-export of [`state::GzHow`] — read pipeline state enum.
pub use state::GzHow;

// ===========================================================================
// Public re-exports: open / lifecycle operations (from open.rs)
// ===========================================================================

pub use open::gz_buffer;
pub use open::gz_clearerr;
pub use open::gz_dopen;
pub use open::gz_eof;
pub use open::gz_error_msg;
pub use open::gz_offset;
pub use open::gz_open;
pub use open::gz_rewind;
pub use open::gz_seek;
pub use open::gz_tell;

// ===========================================================================
// Public re-exports: read operations (from read.rs)
// ===========================================================================

pub use read::gz_direct;
pub use read::gz_fread;
pub use read::gz_getc;
pub use read::gz_gets;
pub use read::gz_read;
pub use read::gz_ungetc;

// ===========================================================================
// Public re-exports: write operations (from write.rs)
// ===========================================================================

pub use write::gz_flush;
pub use write::gz_fwrite;
pub use write::gz_printf;
pub use write::gz_putc;
pub use write::gz_puts;
pub use write::gz_setparams;
pub use write::gz_write;

// ===========================================================================
// Public re-exports: close operations (from close.rs)
// ===========================================================================

pub use close::gz_close;

// ===========================================================================
// Imports for trait implementations
// ===========================================================================

use std::io;

use crate::constants::Z_SYNC_FLUSH;
use crate::error::ZlibError;

// ===========================================================================
// Helper: ZlibError → std::io::Error conversion
// ===========================================================================

/// Convert a [`ZlibError`] into a [`std::io::Error`] for use in the
/// [`std::io::Read`] and [`std::io::Write`] trait implementations on
/// [`GzState`].
///
/// All zlib error variants are mapped to [`std::io::ErrorKind::Other`] with
/// a [`Debug`]-formatted message string, providing diagnostic information
/// while maintaining compatibility with Rust's standard I/O error handling.
///
/// # Examples
///
/// ```rust
/// # use zlib_rs::error::ZlibError;
/// // This is an internal helper; shown here for documentation purposes.
/// let io_err = std::io::Error::new(
///     std::io::ErrorKind::Other,
///     format!("{:?}", ZlibError::DataError),
/// );
/// assert_eq!(io_err.kind(), std::io::ErrorKind::Other);
/// ```
#[inline]
fn zlib_to_io_error(e: ZlibError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{:?}", e))
}

// ===========================================================================
// std::io::Read implementation for GzState
// ===========================================================================

/// Bridges [`GzState`] with Rust's standard [`io::Read`] trait.
///
/// Delegates to [`gz_read`] for decompression, converting any
/// [`ZlibError`] into a [`std::io::Error`] via [`zlib_to_io_error`].
///
/// This allows `GzState` to be used anywhere an `impl Read` is accepted,
/// including:
/// - [`std::io::BufReader`] for buffered reading
/// - [`std::io::Read::read_to_end`] for consuming all data into a `Vec<u8>`
/// - [`std::io::Read::read_exact`] for reading an exact number of bytes
/// - [`std::io::copy`] for piping data to a writer
///
/// # Mode Requirement
///
/// The `GzState` must have been opened in read mode (mode string containing
/// `'r'`, e.g., `"rb"`). If the state is in write mode, each `read()` call
/// will return an error with [`std::io::ErrorKind::Other`].
///
/// # Examples
///
/// ```rust,no_run
/// use std::io::Read;
/// use zlib_rs::gz;
///
/// let mut f = gz::gz_open(std::path::Path::new("data.gz"), "rb").unwrap();
/// let mut buf = [0u8; 1024];
/// let n = f.read(&mut buf).unwrap();
/// println!("Read {} bytes", n);
/// gz::gz_close(&mut f).unwrap();
/// ```
impl io::Read for GzState {
    /// Read decompressed bytes from the gzip file into `buf`.
    ///
    /// Returns the number of bytes placed into `buf`, or 0 at EOF.
    /// Errors from the decompression pipeline are converted to
    /// [`std::io::Error`].
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read::gz_read(self, buf).map_err(zlib_to_io_error)
    }
}

// ===========================================================================
// std::io::Write implementation for GzState
// ===========================================================================

/// Bridges [`GzState`] with Rust's standard [`io::Write`] trait.
///
/// - [`write`](io::Write::write) delegates to [`gz_write`] for compression.
/// - [`flush`](io::Write::flush) delegates to [`gz_flush`] with
///   [`Z_SYNC_FLUSH`](crate::constants::Z_SYNC_FLUSH) to emit a sync
///   marker, matching the semantics expected by Rust I/O consumers.
///
/// Any [`ZlibError`] is converted into a [`std::io::Error`] via
/// [`zlib_to_io_error`].
///
/// This allows `GzState` to be used anywhere an `impl Write` is accepted,
/// including:
/// - [`std::io::BufWriter`] for buffered writing
/// - [`std::io::Write::write_all`] for writing all data
/// - [`std::io::copy`] for piping data from a reader
///
/// # Mode Requirement
///
/// The `GzState` must have been opened in write or append mode (mode string
/// containing `'w'` or `'a'`, e.g., `"wb"` or `"ab"`). If the state is in
/// read mode, each `write()` call will return an error.
///
/// # Flush Behaviour
///
/// The [`flush`](io::Write::flush) method uses [`Z_SYNC_FLUSH`] (value 2),
/// which emits all pending output and a sync marker (`00 00 FF FF`). This
/// ensures that downstream consumers can immediately read all data written
/// so far, at the cost of a small compression ratio penalty.
///
/// # Examples
///
/// ```rust,no_run
/// use std::io::Write;
/// use zlib_rs::gz;
///
/// let mut f = gz::gz_open(std::path::Path::new("out.gz"), "wb").unwrap();
/// f.write_all(b"compressed via std::io::Write").unwrap();
/// f.flush().unwrap();
/// gz::gz_close(&mut f).unwrap();
/// ```
impl io::Write for GzState {
    /// Compress and write `buf` to the gzip file.
    ///
    /// Returns the number of uncompressed bytes accepted (always equal to
    /// `buf.len()` on success). Errors from the compression pipeline are
    /// converted to [`std::io::Error`].
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write::gz_write(self, buf).map_err(zlib_to_io_error)
    }

    /// Flush all buffered compressed data to the underlying file.
    ///
    /// Delegates to [`gz_flush`] with [`Z_SYNC_FLUSH`] to emit a sync
    /// marker, ensuring all data written so far can be read by a
    /// decompressor.
    fn flush(&mut self) -> io::Result<()> {
        write::gz_flush(self, Z_SYNC_FLUSH)
            .map(|_| ())
            .map_err(zlib_to_io_error)
    }
}
