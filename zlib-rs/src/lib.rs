//! A pure safe Rust implementation of the zlib compression library.
//!
//! This crate provides a complete implementation of the DEFLATE compression
//! algorithm ([RFC 1951]), the zlib format ([RFC 1950]), and the gzip format
//! ([RFC 1952]). It is a faithful port of the C zlib library version
//! 1.3.2.1-motley.
//!
//! # Overview
//!
//! The library offers three levels of API:
//!
//! - **One-call functions** — [`compress`], [`compress2`], [`uncompress`], and
//!   [`uncompress2`] handle complete compression or decompression in a single
//!   call.
//! - **Streaming API** — [`ZStream`] paired with the [`deflate`] and [`inflate`]
//!   modules provides incremental compression and decompression for large or
//!   unbounded data.
//! - **Gzip file I/O** — The [`gz`] module (enabled by the `gz-io` feature)
//!   provides buffered readers and writers for gzip-format files.
//!
//! Two checksum algorithms are provided for data integrity:
//!
//! - [`adler32`] — Adler-32 checksum used by the zlib format.
//! - [`crc32`] — CRC-32 checksum used by the gzip format.
//!
//! # Safety
//!
//! This crate contains **zero** `unsafe` blocks. All memory management uses
//! Rust's ownership model via owned `Vec<u8>` buffers, `Box<T>` allocations,
//! and `Drop` trait implementations. The unsafe FFI boundary layer is in the
//! separate `libz-rs-sys` crate.
//!
//! # Features
//!
//! - `gz-io` (default) — Enables gzip file I/O operations ([`gz`] module),
//!   including [`GzReader`] and [`GzWriter`] types.
//! - `gzip` (default) — Enables gzip header encoding/decoding in the
//!   streaming compression and decompression engines.
//!
//! # Quick Start
//!
//! ## One-call compression and decompression
//!
//! ```no_run
//! use zlib_rs::{compress, uncompress, compress_bound};
//!
//! let source = b"Hello, zlib-rs! Hello, zlib-rs!";
//! let bound = compress_bound(source.len());
//! let mut compressed = Vec::new();
//! compress(&mut compressed, source).unwrap();
//!
//! let mut decompressed = vec![0u8; source.len()];
//! uncompress(&mut decompressed, &compressed).unwrap();
//! assert_eq!(&decompressed, source);
//! ```
//!
//! ## Version information
//!
//! ```
//! use zlib_rs::{ZLIB_VERSION, ZLIB_VERNUM};
//!
//! assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
//! assert_eq!(ZLIB_VERNUM, 0x1321);
//! ```
//!
//! ## Checksum computation
//!
//! ```
//! use zlib_rs::{adler32, crc32};
//!
//! let data = b"Hello, world!";
//! let adler = adler32(1, data);
//! let crc = crc32(0, data);
//! assert_ne!(adler, 0);
//! assert_ne!(crc, 0);
//! ```
//!
//! # Module Organization
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`stream`] | Rust-native `ZStream` and `GzHeader` types |
//! | [`error`] | `ReturnCode` enum and `ZlibError` type |
//! | [`constants`] | Version info, flush modes, strategies, limits |
//! | [`deflate`] | DEFLATE compression engine |
//! | [`inflate`] | DEFLATE decompression engine |
//! | [`compress`](self::compress) | One-call compress/uncompress wrappers |
//! | [`checksum`] | Adler-32 and CRC-32 checksum engines |
//! | [`gz`] | Gzip file I/O (requires `gz-io` feature) |
//!
//! [RFC 1950]: https://datatracker.ietf.org/doc/html/rfc1950
//! [RFC 1951]: https://datatracker.ietf.org/doc/html/rfc1951
//! [RFC 1952]: https://datatracker.ietf.org/doc/html/rfc1952

// ─── Crate-Level Lint Attributes ─────────────────────────────────────────────
//
// These attributes enforce the coding standards mandated by the AAP:
// - `forbid(unsafe_code)`: Compile-time enforcement of zero unsafe blocks in
//   the core crate (AAP Section 0.7.1).
// - `deny(missing_docs)`: All public items must be documented (AAP Section 0.7.2).
// - `deny(clippy::all, clippy::pedantic)`: Strict lint enforcement (AAP Section 0.7.2).

#![deny(missing_docs)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

// ─── Module Declarations ─────────────────────────────────────────────────────
//
// Nine modules composing the complete zlib library, organized by functional area.
// The module hierarchy mirrors the six-layer architecture from the original C
// library: Public API, Deflate Engine, Inflate Engine, Checksum Engines,
// Gzip File I/O, and Internal Utilities.

/// Streaming I/O types: [`ZStream`] and [`GzHeader`].
///
/// This module replaces the C `z_stream_s` and `gz_header_s` structures from
/// `zlib.h` with safe Rust equivalents that use owned buffers instead of raw
/// pointers.
pub mod stream;

/// Return codes and error types for zlib operations.
///
/// Provides the [`ReturnCode`] enum (mapping all 9 C zlib return codes with
/// exact `i32` discriminants) and [`ZlibError`] (a rich error wrapper
/// implementing `std::error::Error`).
pub mod error;

/// Public constants: version info, flush modes, strategies, compression levels,
/// and internal limits.
///
/// All values are exact ports of their C `#define` equivalents from `zlib.h`,
/// `zutil.h`, `zconf.h`, and `deflate.h`.
pub mod constants;

/// DEFLATE compression engine: streaming compression, dictionary support, and
/// parameter tuning.
///
/// Ports `deflate.c` (2185 lines) and its supporting modules (`trees.c`,
/// `deflate.h`) into safe Rust. Provides functions such as [`deflate::deflate()`],
/// [`deflate::deflate_init2()`], and [`deflate::deflate_end()`].
pub mod deflate;

/// DEFLATE decompression engine: streaming decompression, sync recovery, and
/// format auto-detection.
///
/// Ports `inflate.c` (1413 lines) and its supporting modules (`inffast.c`,
/// `inftrees.c`, `infback.c`) into safe Rust. Provides functions such as
/// [`inflate::inflate()`], [`inflate::inflate_init2()`], and
/// [`inflate::inflate_end()`].
pub mod inflate;

/// One-call compression and decompression wrappers.
///
/// Ports `compress.c` (99 lines) and `uncompr.c` (101 lines) into safe Rust.
/// Provides [`compress()`], [`compress2()`], [`compress_bound()`],
/// [`uncompress()`], and [`uncompress2()`] for single-call operations without
/// manual stream management.
pub mod compress;

/// Gzip file I/O: buffered reading and writing of gzip-format files.
///
/// This module is conditionally compiled when the `gz-io` Cargo feature is
/// enabled, corresponding to the C `Z_SOLO` exclusion pattern where gzip
/// file I/O is disabled for minimal or embedded builds.
///
/// Ports `gzlib.c` (609 lines), `gzread.c` (668 lines), `gzwrite.c`
/// (700 lines), and `gzclose.c` (23 lines) into safe Rust.
#[cfg(feature = "gz-io")]
pub mod gz;

/// Checksum computation engines: Adler-32 and CRC-32.
///
/// Ports `adler32.c` (164 lines) and `crc32.c` (983 lines) into safe Rust
/// with compile-time `const` lookup tables, eliminating the C
/// `DYNAMIC_CRC_TABLE` runtime initialization pattern.
pub mod checksum;

/// Internal utility functions, error messages, and helper constants.
///
/// This module has `pub(crate)` visibility — it is not part of the public API.
/// It corresponds to the C `zutil.h`/`zutil.c`, which the C library
/// documentation describes as "part of the implementation … Applications should
/// only use zlib.h."
///
/// Selected functions (`zlib_version`, `zlib_compile_flags`, `err_msg`) are
/// re-exported at the crate root for use by the FFI crate.
pub(crate) mod util;

// ─── Primary Type Re-exports ─────────────────────────────────────────────────
//
// These re-exports provide ergonomic top-level access to the most commonly used
// types, so that users can write `use zlib_rs::ZStream;` instead of
// `use zlib_rs::stream::ZStream;`.

/// Re-export of [`stream::ZStream`] — the main compression/decompression stream.
pub use stream::ZStream;

/// Re-export of [`stream::GzHeader`] — gzip header metadata.
pub use stream::GzHeader;

/// Re-export of [`error::ReturnCode`] — zlib return code enum.
///
/// Re-export of [`error::ZlibError`] — rich error type for zlib operations.
pub use error::{ReturnCode, ZlibError};

// ─── Compression/Decompression State Re-exports ─────────────────────────────
//
// State types for the streaming compression and decompression engines.

/// Re-export of [`deflate::DeflateState`] — DEFLATE compression engine state.
pub use deflate::DeflateState;

/// Re-export of [`inflate::InflateState`] — DEFLATE decompression engine state.
pub use inflate::InflateState;

// ─── Gzip Types (conditional on `gz-io` feature) ────────────────────────────
//
// When the `gz-io` feature is enabled, the `GzReader` and `GzWriter` types are
// re-exported at the crate root for convenient access to gzip file I/O.

/// Re-export of [`gz::read::GzReader`] — buffered gzip file reader with
/// auto-detection of gzip vs transparent data.
#[cfg(feature = "gz-io")]
pub use gz::read::GzReader;

/// Re-export of [`gz::write::GzWriter`] — buffered gzip file writer with
/// compression.
#[cfg(feature = "gz-io")]
pub use gz::write::GzWriter;

// ─── One-call Compression/Decompression Function Re-exports ─────────────────
//
// Convenience functions for single-call compression and decompression,
// re-exported from the `compress` module.

pub use self::compress::{compress, compress2, compress_bound, uncompress, uncompress2};

// ─── Checksum Function Re-exports ───────────────────────────────────────────
//
// Primary checksum functions re-exported from their respective submodules for
// ergonomic top-level access.

/// Re-export of [`checksum::adler32::adler32`] — compute/update Adler-32.
pub use checksum::adler32::{adler32, adler32_combine};

/// Re-export of [`checksum::crc32::crc32`] — compute/update CRC-32.
pub use checksum::crc32::{crc32, crc32_combine};

// ─── Version Constant Re-exports ────────────────────────────────────────────
//
// Library version information re-exported from the `constants` module.

/// Re-export of [`constants::ZLIB_VERSION`] — the library version string.
///
/// Re-export of [`constants::ZLIB_VERNUM`] — the library version number.
pub use constants::{ZLIB_VERNUM, ZLIB_VERSION};

// ─── Utility Function Re-exports ────────────────────────────────────────────
//
// Selected internal utility functions are re-exported at the crate root so
// that the FFI crate (`libz-rs-sys`) can wrap them as extern "C" functions.
//
// - `zlib_version` → wraps C `zlibVersion()`
// - `zlib_compile_flags` → wraps C `zlibCompileFlags()`
// - `err_msg` → wraps C `zError()`

/// Re-export of [`util::zlib_version`] — returns the library version string.
pub use util::zlib_version;

/// Re-export of [`util::zlib_compile_flags`] — returns compile-time
/// configuration flags.
pub use util::zlib_compile_flags;

/// Returns the error message string for the given integer error code.
///
/// Maps the integer error code (range `[-6, 2]`) to its corresponding
/// human-readable message from the internal error message table. Out-of-range
/// values return an empty string.
///
/// This function is the Rust equivalent of the C `zError()` function from
/// `zutil.c`, and is intended for use by the FFI crate (`libz-rs-sys`) to
/// implement the `zError` export.
///
/// # Arguments
///
/// * `err` — An integer error code matching the `ReturnCode` enum discriminants.
///   Valid range: `Z_VERSION_ERROR` (-6) through `Z_NEED_DICT` (2).
///
/// # Returns
///
/// A static string with the human-readable error message, or an empty string
/// for out-of-range codes.
///
/// # Examples
///
/// ```
/// use zlib_rs::err_msg;
///
/// assert_eq!(err_msg(0), "");           // Z_OK
/// assert_eq!(err_msg(-3), "data error"); // Z_DATA_ERROR
/// assert_eq!(err_msg(99), "");           // out-of-range → empty
/// ```
#[must_use]
pub fn err_msg(err: i32) -> &'static str {
    util::err_msg(err)
}
