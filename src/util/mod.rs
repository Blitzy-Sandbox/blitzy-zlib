// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib

//! One-call compression/decompression utilities and version information.
//!
//! This module provides convenience wrappers for single-buffer compression
//! and decompression operations, plus version and configuration query
//! functions.  It replaces the C source files `compress.c`, `uncompr.c`,
//! and the utility functions from `zutil.c` with idiomatic Rust equivalents.
//!
//! # Compression
//!
//! - [`compress`] — Compress data with the default compression level
//! - [`compress2`] — Compress data with an explicit compression level (0–9)
//! - [`compress_bound`] — Calculate an upper bound on the compressed output size
//!
//! # Decompression
//!
//! - [`uncompress`] — Decompress data in one call
//! - [`uncompress2`] — Decompress data, also returning consumed source bytes
//!
//! # Version and Metadata
//!
//! - [`ZLIB_VERSION`] — Compile-time version string constant
//! - [`zlib_version`] — Get the library version string at runtime
//! - [`compile_flags`] — Get a bitmask of compile-time configuration flags
//! - [`error_message`] — Convert a zlib return code to a human-readable message
//!
//! # Relationship to C zlib
//!
//! In C zlib every function listed above has a companion `_z` variant that
//! accepts `z_size_t` (a typedef for `size_t`) instead of `uLong`.  Because
//! Rust uses `usize` natively, a single function replaces both the `_z` and
//! non-`_z` variants:
//!
//! | C Functions                          | Rust Equivalent       |
//! |--------------------------------------|-----------------------|
//! | `compress` / `compress_z`            | [`compress`]          |
//! | `compress2` / `compress2_z`          | [`compress2`]         |
//! | `compressBound` / `compressBound_z`  | [`compress_bound`]    |
//! | `uncompress` / `uncompress_z`        | [`uncompress`]        |
//! | `uncompress2` / `uncompress2_z`      | [`uncompress2`]       |
//! | `zlibVersion`                        | [`zlib_version`]      |
//! | `zlibCompileFlags`                   | [`compile_flags`]     |
//! | `zError`                             | [`error_message`]     |
//!
//! # Streaming Alternative
//!
//! For finer control over buffer management, incremental processing, or
//! custom flush modes, use the streaming APIs in [`crate::deflate`] and
//! [`crate::inflate`] directly.  The functions in this module are thin
//! wrappers that perform `init → process → end` in a single call.
//!
//! # Quick Start
//!
//! ```no_run
//! use zlib_rs::util::{compress, compress_bound, uncompress};
//!
//! // Compress
//! let data = b"Hello, zlib-rs!";
//! let bound = compress_bound(data.len());
//! let mut compressed = vec![0u8; bound];
//! let comp_len = compress(&mut compressed, data).unwrap();
//! compressed.truncate(comp_len);
//!
//! // Decompress
//! let mut decompressed = vec![0u8; data.len()];
//! let decomp_len = uncompress(&mut decompressed, &compressed).unwrap();
//! assert_eq!(&decompressed[..decomp_len], &data[..]);
//! ```

// ---------------------------------------------------------------------------
// Submodule declarations
// ---------------------------------------------------------------------------

/// One-call compression wrappers (port of C `compress.c`).
pub mod compress;

/// One-call decompression wrappers (port of C `uncompr.c`).
pub mod uncompress;

/// Version, compile-time configuration, and error message utilities
/// (port of C `zutil.c` utility functions).
pub mod version;

// ---------------------------------------------------------------------------
// Public re-exports for flat access via `use zlib_rs::util::*`
// ---------------------------------------------------------------------------

// Compression utilities (from compress.c)
pub use self::compress::{compress, compress_bound, compress2};

// Decompression utilities (from uncompr.c)
pub use self::uncompress::{uncompress, uncompress2};

// Version and metadata (from zutil.c)
pub use self::version::{ZLIB_VERSION, compile_flags, error_message, zlib_version};
