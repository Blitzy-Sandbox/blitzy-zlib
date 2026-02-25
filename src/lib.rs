// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib

//! # zlib-rs — A Pure Rust Implementation of the zlib Compression Library
//!
//! `zlib-rs` is a complete, byte-level compatible Rust rewrite of the zlib
//! compression library (v1.3.2.1-motley, VERNUM 0x1321). It implements the
//! DEFLATE compression algorithm and its standard framing formats:
//!
//! - **RFC 1950** — zlib compressed data format
//! - **RFC 1951** — DEFLATE compressed data format
//! - **RFC 1952** — gzip file format
//!
//! The crate is a drop-in replacement for the C zlib library's functionality,
//! providing identical wire-format output for the same inputs, compression
//! levels, and strategies. It leverages Rust's ownership model and type system
//! to eliminate entire categories of memory-safety vulnerabilities (buffer
//! overflows, use-after-free, double-free, uninitialised memory access) while
//! preserving streaming semantics and API compatibility.
//!
//! # Quick Start — Compression
//!
//! ```no_run
//! use zlib_rs::{compress, compress_bound, uncompress};
//!
//! let data = b"Hello, zlib-rs! This is a compression test.";
//! let bound = compress_bound(data.len());
//! let mut compressed = vec![0u8; bound];
//!
//! let comp_len = compress(&mut compressed, data).unwrap();
//! compressed.truncate(comp_len);
//!
//! let mut decompressed = vec![0u8; data.len()];
//! let decomp_len = uncompress(&mut decompressed, &compressed).unwrap();
//! assert_eq!(&decompressed[..decomp_len], &data[..]);
//! ```
//!
//! # Quick Start — Streaming Deflate/Inflate
//!
//! ```no_run
//! use zlib_rs::stream::ZStream;
//! use zlib_rs::deflate::{deflate_init, deflate, deflate_end};
//! use zlib_rs::inflate::{inflate_init, inflate, inflate_end};
//! use zlib_rs::constants::{Z_FINISH, Z_DEFAULT_COMPRESSION};
//!
//! // Compress
//! let input = b"Hello, streaming zlib-rs!";
//! let mut output = vec![0u8; 256];
//! let mut strm = ZStream::new();
//!
//! deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).unwrap();
//! strm.set_input(input);
//! strm.set_output(&mut output);
//! deflate(&mut strm, Z_FINISH).unwrap();
//! let compressed_len = strm.total_out as usize;
//! deflate_end(&mut strm).unwrap();
//!
//! // Decompress
//! let mut result = vec![0u8; 256];
//! let mut strm2 = ZStream::new();
//! inflate_init(&mut strm2).unwrap();
//! strm2.set_input(&output[..compressed_len]);
//! strm2.set_output(&mut result);
//! inflate(&mut strm2, Z_FINISH).unwrap();
//! let decompressed_len = strm2.total_out as usize;
//! inflate_end(&mut strm2).unwrap();
//!
//! assert_eq!(&result[..decompressed_len], input);
//! ```
//!
//! # Quick Start — Checksums
//!
//! ```
//! use zlib_rs::checksum::{adler32, crc32};
//!
//! let data = b"Hello, checksums!";
//! let a = adler32(1, data);
//! let c = crc32(0, data);
//! assert_ne!(a, 0);
//! assert_ne!(c, 0);
//! ```
//!
//! # Feature Flags
//!
//! | Feature    | Default | Description |
//! |------------|---------|-------------|
//! | `std`      | yes     | Enable standard library support (required for gzip file I/O) |
//! | `gzip`     | yes     | Enable gzip (RFC 1952) format support in deflate/inflate |
//! | `gz-io`    | yes     | Enable gzip file I/O (`gz_open`, `gz_read`, `gz_write`, etc.); implies `std` + `gzip` |
//! | `no-std`   | no      | Bare-metal mode: no stdio, no file I/O — compression/decompression/checksums only |
//! | `simd`     | yes     | SIMD-accelerated CRC-32 via `crc32fast` |
//!
//! # Modules
//!
//! - [`error`] — Error types ([`ZlibError`], [`ReturnCode`]) and result alias ([`ZlibResult`])
//! - [`constants`] — Flush modes, compression levels, strategies, algorithm limits
//! - [`stream`] — [`ZStream`] central exchange structure for streaming operations
//! - [`gz_header`] — [`GzHeader`] for gzip metadata
//! - [`deflate`] — DEFLATE compression engine (14 public API functions)
//! - [`inflate`] — DEFLATE decompression engine (21+ public API functions)
//! - [`checksum`] — Adler-32 and CRC-32 computation with combine operations
//! - [`gz`] — Gzip file I/O (feature-gated behind `gz-io`)
//! - [`util`] — One-call compression/decompression wrappers and version utilities
//!
//! # Compatibility
//!
//! Compatible with C zlib v1.3.2.1-motley (VERNUM 0x1321). Data compressed by
//! the original C zlib decompresses identically through this Rust implementation,
//! and vice versa — full interoperability at the wire format level.

// ============================================================================
// Crate-level attributes
// ============================================================================

// Enable `no_std` mode when the `std` feature is not active (AAP §0.6.3, §0.8.2).
// Core compression, decompression, and checksum modules compile with no_std.
#![cfg_attr(not(feature = "std"), no_std)]

// Encourage documentation on all public items.
#![warn(missing_docs)]

// ============================================================================
// Feature conflict guard
// ============================================================================

// The `no-std` and `gz-io` features are contradictory: `gz-io` requires `std`
// for file I/O, which is incompatible with bare-metal `no-std` mode.
#[cfg(all(feature = "no-std", feature = "gz-io"))]
compile_error!(
    "Feature conflict: `no-std` and `gz-io` cannot be enabled simultaneously. \
     `gz-io` requires `std` for file I/O, which is incompatible with `no-std` mode."
);

// ============================================================================
// Conditional compilation for std / no_std
// ============================================================================

// Use cfg_if for ergonomic conditional compilation, replacing C preprocessor
// #if/#ifdef chains (AAP §0.6.1).
cfg_if::cfg_if! {
    if #[cfg(feature = "std")] {
        extern crate std;
    } else {
        // In no_std mode, pull in the alloc crate for Vec, Box, String
        // which are needed by the compression engine's internal buffers.
        extern crate alloc;
    }
}

// ============================================================================
// Module declarations (AAP §0.4.1)
// ============================================================================

/// Error types, result aliases, and diagnostic utilities for zlib error
/// handling.
///
/// Provides [`ZlibError`] (negative C return codes), [`ReturnCode`]
/// (non-negative success codes), the [`ZlibResult`] type alias, and
/// the [`error_message`] lookup function.
pub mod error;

/// Constants for flush modes, compression levels, strategies, data types,
/// DEFLATE algorithm parameters, and Huffman table size limits.
///
/// All constants are re-exported at the crate root via `pub use constants::*`.
pub mod constants;

/// Streaming compression/decompression exchange structure.
///
/// Provides [`ZStream`] — the central data structure through which callers
/// interact with the DEFLATE compression and decompression engines — and
/// [`StreamState`], the enum that owns the internal engine state.
pub mod stream;

/// Gzip header metadata types.
///
/// Provides [`GzHeader`] for reading and writing gzip header fields
/// (name, comment, extra data, modification time, OS identifier) as
/// defined in RFC 1952.
pub mod gz_header;

/// DEFLATE compression engine.
///
/// Implements the complete DEFLATE compression algorithm with all 5 strategy
/// functions (stored, fast, slow, huff, rle), 10 compression levels (0–9),
/// and zlib/gzip/raw framing support.
pub mod deflate;

/// DEFLATE decompression engine.
///
/// Implements the 30+ mode state machine for stream decompression with
/// format auto-detection (zlib/gzip/raw), sync recovery, and callback-based
/// decompression.
pub mod inflate;

/// Adler-32 and CRC-32 checksum computation.
///
/// Provides both one-shot and streaming checksum APIs with combine operations
/// for parallel/incremental computation.
pub mod checksum;

/// Gzip file I/O operations.
///
/// Provides a stdio-like interface for reading and writing `.gz` files.
/// Only available when the `gz-io` feature is enabled (default).
#[cfg(feature = "gz-io")]
pub mod gz;

/// One-call compression/decompression utilities and version information.
///
/// Provides convenience wrappers ([`compress`], [`uncompress`]) that perform
/// `init → process → end` in a single call, plus version and configuration
/// query functions.
pub mod util;

// ============================================================================
// Version constants (from zlib.h lines 44–49)
// ============================================================================

/// The zlib-rs version string, compatible with C zlib v1.3.2.1-motley.
///
/// This constant mirrors the `ZLIB_VERSION` macro defined in the original
/// `zlib.h` (line 44). Applications can compare this value against the
/// return value of [`zlib_version()`] to verify that header and library
/// versions match.
///
/// # Examples
///
/// ```
/// assert_eq!(zlib_rs::ZLIB_VERSION, "1.3.2.1-motley");
/// ```
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// The zlib-rs version number encoded as a hexadecimal integer.
///
/// Format: `0xMNRP` where M=major, N=minor, R=revision, P=subrevision.
/// Mirrors the `ZLIB_VERNUM` macro from `zlib.h` (line 45).
///
/// # Examples
///
/// ```
/// assert_eq!(zlib_rs::ZLIB_VERNUM, 0x1321);
/// ```
pub const ZLIB_VERNUM: u32 = 0x1321;

/// Major version number (1).
///
/// Mirrors `ZLIB_VER_MAJOR` from `zlib.h` (line 46).
pub const ZLIB_VER_MAJOR: u32 = 1;

/// Minor version number (3).
///
/// Mirrors `ZLIB_VER_MINOR` from `zlib.h` (line 47).
pub const ZLIB_VER_MINOR: u32 = 3;

/// Revision number (2).
///
/// Mirrors `ZLIB_VER_REVISION` from `zlib.h` (line 48).
pub const ZLIB_VER_REVISION: u32 = 2;

/// Sub-revision number (1).
///
/// Mirrors `ZLIB_VER_SUBREVISION` from `zlib.h` (line 49).
pub const ZLIB_VER_SUBREVISION: u32 = 1;

// ============================================================================
// zlib_version() function
// ============================================================================

/// Returns the zlib-rs library version string at runtime.
///
/// Equivalent to C zlib's `zlibVersion()`. Applications can compare the
/// return value against the compile-time [`ZLIB_VERSION`] constant to
/// verify version compatibility between headers and library.
///
/// # Examples
///
/// ```
/// assert_eq!(zlib_rs::zlib_version(), zlib_rs::ZLIB_VERSION);
/// assert_eq!(zlib_rs::zlib_version(), "1.3.2.1-motley");
/// ```
#[inline]
pub fn zlib_version() -> &'static str {
    ZLIB_VERSION
}

// ============================================================================
// Public API re-exports — Error types (from src/error.rs)
// ============================================================================

/// Re-export of [`error::ZlibError`] — error conditions for zlib operations.
pub use error::ZlibError;

/// Re-export of [`error::ReturnCode`] — success conditions for zlib operations.
pub use error::ReturnCode;

/// Re-export of [`error::ZlibResult`] — `Result<ReturnCode, ZlibError>` type alias.
pub use error::ZlibResult;

/// Re-export of [`error::error_message`] — C return code to human-readable message.
pub use error::error_message;

// ============================================================================
// Public API re-exports — Constants (from src/constants.rs)
// ============================================================================

// Re-export all public constants and enums from the constants module so that
// users can write `use zlib_rs::Z_NO_FLUSH` or `use zlib_rs::FlushMode`
// without navigating the module hierarchy.
pub use constants::*;

// ============================================================================
// Public API re-exports — Stream types (from src/stream.rs)
// ============================================================================

/// Re-export of [`stream::ZStream`] — the central streaming exchange structure.
pub use stream::ZStream;

/// Re-export of [`stream::StreamState`] — internal state discriminant
/// (None / Deflate / Inflate) for testing and advanced introspection.
pub use stream::StreamState;

// ============================================================================
// Public API re-exports — GzHeader (from src/gz_header.rs)
// ============================================================================

/// Re-export of [`gz_header::GzHeader`] — gzip header metadata.
pub use gz_header::GzHeader;

// ============================================================================
// Public API re-exports — Deflate API (from src/deflate/mod.rs)
// ============================================================================

pub use deflate::deflate_init;
pub use deflate::deflate_init2;
pub use deflate::deflate;
pub use deflate::deflate_end;
pub use deflate::deflate_reset;
pub use deflate::deflate_params;
pub use deflate::deflate_tune;
pub use deflate::deflate_bound;
pub use deflate::deflate_pending;
pub use deflate::deflate_prime;
pub use deflate::deflate_set_header;
pub use deflate::deflate_set_dictionary;
pub use deflate::deflate_get_dictionary;
pub use deflate::deflate_copy;

// ============================================================================
// Public API re-exports — Inflate API (from src/inflate/mod.rs)
// ============================================================================

pub use inflate::inflate_init;
pub use inflate::inflate_init2;
pub use inflate::inflate;
pub use inflate::inflate_end;
pub use inflate::inflate_reset;
pub use inflate::inflate_reset2;
pub use inflate::inflate_sync;
pub use inflate::inflate_copy;
pub use inflate::inflate_prime;
pub use inflate::inflate_mark;
pub use inflate::inflate_get_header;
pub use inflate::inflate_set_dictionary;
pub use inflate::inflate_get_dictionary;
pub use inflate::inflate_validate;
pub use inflate::inflate_sync_point;
pub use inflate::inflate_undermine;

// Callback-based raw DEFLATE decompression (inflateBack family)
pub use inflate::inflate_back_init;
pub use inflate::inflate_back;
pub use inflate::inflate_back_end;

/// Re-export of [`inflate::InflateBackInput`] — callback trait for inflate_back input.
pub use inflate::InflateBackInput;

/// Re-export of [`inflate::InflateBackOutput`] — callback trait for inflate_back output.
pub use inflate::InflateBackOutput;

/// Re-export of [`inflate::InflateState`] — inflate decompression state
/// (re-exported for coverage testing and advanced introspection).
pub use inflate::InflateState;

/// Re-export of [`inflate::InflateMode`] — inflate state machine mode enum
/// (re-exported for coverage testing and advanced introspection).
pub use inflate::InflateMode;

/// Re-export of [`inflate::inflate_table`] — Huffman table builder for inflate.
pub use inflate::inflate_table;

/// Re-export of [`inflate::Code`] — Huffman decode table entry.
pub use inflate::Code;

/// Re-export of [`inflate::CodeType`] — discriminant for Huffman table type.
pub use inflate::CodeType;

// ============================================================================
// Public API re-exports — Checksum API (from src/checksum/mod.rs)
// ============================================================================

pub use checksum::adler32;
pub use checksum::adler32_z;
pub use checksum::adler32_combine;
pub use checksum::crc32;
pub use checksum::crc32_z;
pub use checksum::crc32_combine;
pub use checksum::crc32_combine_gen;
pub use checksum::crc32_combine_op;
pub use checksum::get_crc_table;

// ============================================================================
// Public API re-exports — Gzip file I/O (feature-gated, from src/gz/mod.rs)
// ============================================================================

#[cfg(feature = "gz-io")]
pub use gz::GzState;

#[cfg(feature = "gz-io")]
pub use gz::GzMode;

#[cfg(feature = "gz-io")]
pub use gz::GzHow;

#[cfg(feature = "gz-io")]
pub use gz::gz_open;

#[cfg(feature = "gz-io")]
pub use gz::gz_dopen;

#[cfg(feature = "gz-io")]
pub use gz::gz_buffer;

#[cfg(feature = "gz-io")]
pub use gz::gz_read;

#[cfg(feature = "gz-io")]
pub use gz::gz_fread;

#[cfg(feature = "gz-io")]
pub use gz::gz_write;

#[cfg(feature = "gz-io")]
pub use gz::gz_fwrite;

#[cfg(feature = "gz-io")]
pub use gz::gz_close;

#[cfg(feature = "gz-io")]
pub use gz::gz_getc;

#[cfg(feature = "gz-io")]
pub use gz::gz_putc;

#[cfg(feature = "gz-io")]
pub use gz::gz_gets;

#[cfg(feature = "gz-io")]
pub use gz::gz_puts;

#[cfg(feature = "gz-io")]
pub use gz::gz_printf;

#[cfg(feature = "gz-io")]
pub use gz::gz_flush;

#[cfg(feature = "gz-io")]
pub use gz::gz_rewind;

#[cfg(feature = "gz-io")]
pub use gz::gz_seek;

#[cfg(feature = "gz-io")]
pub use gz::gz_tell;

#[cfg(feature = "gz-io")]
pub use gz::gz_offset;

#[cfg(feature = "gz-io")]
pub use gz::gz_eof;

#[cfg(feature = "gz-io")]
pub use gz::gz_direct;

#[cfg(feature = "gz-io")]
pub use gz::gz_error_msg;

#[cfg(feature = "gz-io")]
pub use gz::gz_clearerr;

#[cfg(feature = "gz-io")]
pub use gz::gz_setparams;

#[cfg(feature = "gz-io")]
pub use gz::gz_ungetc;

// ============================================================================
// Public API re-exports — Utility functions (from src/util/mod.rs)
// ============================================================================

pub use util::compress;
pub use util::compress2;
pub use util::compress_bound;
pub use util::uncompress;
pub use util::uncompress2;
