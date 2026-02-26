//! Checksum computation engines for data integrity verification.
//!
//! This module provides two checksum algorithms used throughout the zlib
//! compression library:
//!
//! - **Adler-32** — A fast checksum algorithm used in the zlib format (RFC 1950)
//!   for integrity verification of uncompressed data.
//! - **CRC-32** — The cyclic redundancy check used in the gzip format (RFC 1952)
//!   and for combining checksums of concatenated data.
//!
//! Both implementations are pure safe Rust with zero `unsafe` blocks. Lookup
//! tables are compile-time `const` arrays — no runtime initialization is needed
//! (replacing the C `DYNAMIC_CRC_TABLE` pattern).
//!
//! # Module Organization
//!
//! The checksum functionality is split into two submodules, each ported from the
//! corresponding C source file:
//!
//! - `adler32` — Ported from `adler32.c`. Provides `adler32`,
//!   `adler32_z`, and `adler32_combine`.
//! - `crc32` — Ported from `crc32.c` and `crc32.h`. Provides `crc32`,
//!   `crc32_z`, `crc32_combine`, `crc32_combine_op`,
//!   `crc32_combine_gen`, and `get_crc_table`.
//!
//! Primary functions are re-exported at this module level for convenience, so
//! callers may use either `checksum::adler32::adler32(...)` or the fully
//! qualified submodule path.

/// Adler-32 checksum engine.
///
/// Provides the Adler-32 checksum algorithm as specified in RFC 1950 for
/// data integrity verification in the zlib compressed data format. Ported
/// from `adler32.c` (zlib 1.3.2.1-motley).
///
/// # Key Functions
///
/// - `adler32` — Compute/update an Adler-32 checksum (convenience alias).
/// - `adler32_z` — Compute/update an Adler-32 checksum (size-typed).
/// - `adler32_combine` — Combine two Adler-32 checksums algebraically.
pub mod adler32;

/// CRC-32 checksum engine.
///
/// Provides the CRC-32 algorithm using the polynomial `0xEDB88320` (reflected)
/// for data integrity verification in the gzip file format (RFC 1952) and
/// other contexts. Ported from `crc32.c` and `crc32.h` (zlib 1.3.2.1-motley).
///
/// # Key Functions
///
/// - `crc32` — Compute/update a CRC-32 checksum (convenience alias).
/// - `crc32_z` — Compute/update a CRC-32 checksum (size-typed).
/// - `crc32_combine` — Combine two CRC-32 checksums for concatenated data.
/// - `crc32_combine_op` — Combine CRC-32 values using a pre-computed operator.
/// - `crc32_combine_gen` — Pre-compute an operator for CRC-32 combination.
/// - `get_crc_table` — Obtain a reference to the CRC-32 lookup table.
pub mod crc32;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------
// These re-exports allow callers to access the primary checksum functions
// directly from the `checksum` module without fully qualifying through the
// submodule name:
//
//   use zlib_rs::checksum::{adler32_z, crc32_z};
//
// The submodule paths remain available for callers who prefer explicit
// namespacing:
//
//   use zlib_rs::checksum::adler32::adler32_combine;
//   use zlib_rs::checksum::crc32::crc32_combine_gen;

// -- Adler-32 re-exports --

/// Re-export of `adler32::adler32` for convenient access.
pub use adler32::adler32;

/// Re-export of `adler32::adler32_z` for convenient access.
pub use adler32::adler32_z;

/// Re-export of `adler32::adler32_combine` for convenient access.
pub use adler32::adler32_combine;

// -- CRC-32 re-exports --

/// Re-export of `crc32::crc32` for convenient access.
pub use crc32::crc32;

/// Re-export of `crc32::crc32_z` for convenient access.
pub use crc32::crc32_z;

/// Re-export of `crc32::crc32_combine` for convenient access.
pub use crc32::crc32_combine;

/// Re-export of `crc32::crc32_combine_op` for convenient access.
pub use crc32::crc32_combine_op;

/// Re-export of `crc32::crc32_combine_gen` for convenient access.
pub use crc32::crc32_combine_gen;

/// Re-export of `crc32::get_crc_table` for convenient access.
pub use crc32::get_crc_table;
