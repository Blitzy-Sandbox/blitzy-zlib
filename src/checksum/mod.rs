//! Checksum computation for zlib and gzip formats.
//!
//! This module provides two checksum algorithms used by the zlib family of
//! compressed data formats:
//!
//! - **[Adler-32](`adler32`)** — A fast checksum used in the zlib compressed
//!   data format (RFC 1950) for header and trailer integrity verification.
//!   Adler-32 is almost as reliable as CRC-32 but can be computed much faster.
//!
//! - **[CRC-32](`crc32`)** — A stronger checksum based on the IEEE 802.3
//!   polynomial, used in the gzip file format (RFC 1952) for data integrity
//!   verification.  When the `simd` feature is enabled, CRC-32 computation is
//!   hardware-accelerated via the [`crc32fast`](https://crates.io/crates/crc32fast)
//!   crate.
//!
//! Both checksum families support **combine operations** that allow the
//! checksum of the concatenation of two byte sequences to be computed from
//! the individual checksums and the length of the second sequence, without
//! re-processing any data.  This is useful for parallel or incremental
//! computation of checksums over large datasets.
//!
//! # `no_std` Compatibility
//!
//! This module is fully `no_std` compatible when the `std` feature is
//! disabled.  No heap allocation or file I/O is performed by the checksum
//! functions.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::checksum::{adler32, crc32, Adler32, Crc32};
//!
//! // One-shot Adler-32
//! let a = adler32(1, b"Hello, World!");
//! assert_ne!(a, 0);
//!
//! // One-shot CRC-32
//! let c = crc32(0, b"Hello, World!");
//! assert_ne!(c, 0);
//!
//! // Streaming Adler-32
//! let mut adler = Adler32::new();
//! adler.update(b"Hello, ");
//! adler.update(b"World!");
//! assert_eq!(adler.finalize(), a);
//!
//! // Streaming CRC-32
//! let mut hasher = Crc32::new();
//! hasher.update(b"Hello, ");
//! hasher.update(b"World!");
//! assert_eq!(hasher.finalize(), c);
//! ```

// ---------------------------------------------------------------------------
// Submodule declarations
// ---------------------------------------------------------------------------

/// Adler-32 checksum computation submodule.
///
/// Provides the core [`adler32_z`](adler32::adler32_z) function and its
/// convenience wrappers, combine operations, and the [`Adler32`](adler32::Adler32)
/// streaming struct.
pub mod adler32;

/// CRC-32 checksum computation submodule.
///
/// Provides the core [`crc32_z`](crc32::crc32_z) function and its convenience
/// wrappers, combine and pre-computed operator operations, table access, and
/// the [`Crc32`](crc32::Crc32) streaming struct.
pub mod crc32;

// ---------------------------------------------------------------------------
// Re-export Adler-32 public API
// ---------------------------------------------------------------------------

// Function re-exports matching the C zlib public API (zlib.h lines 1809-1837).
// In Rust, modules and functions occupy different namespaces (type vs value),
// so `pub mod adler32` (type namespace) and `pub use self::adler32::adler32`
// (value namespace) coexist without conflict.

/// Re-export of [`adler32::adler32`] — C-compatible Adler-32 computation.
pub use self::adler32::adler32;

/// Re-export of [`adler32::adler32_z`] — Adler-32 computation with `usize` length.
pub use self::adler32::adler32_z;

/// Re-export of [`adler32::adler32_combine`] — combine two Adler-32 checksums.
pub use self::adler32::adler32_combine;

/// Re-export of [`adler32::adler32_combine64`] — combine two Adler-32 checksums (64-bit length).
pub use self::adler32::adler32_combine64;

/// Re-export of [`adler32::Adler32`] — streaming Adler-32 computer.
pub use self::adler32::Adler32;

// ---------------------------------------------------------------------------
// Re-export CRC-32 public API
// ---------------------------------------------------------------------------

// Function re-exports matching the C zlib public API (zlib.h lines 1848-1894, 2035).
// Same namespace separation applies: `pub mod crc32` (type) and
// `pub use self::crc32::crc32` (value) coexist without conflict.

/// Re-export of [`crc32::crc32`] — C-compatible CRC-32 computation.
pub use self::crc32::crc32;

/// Re-export of [`crc32::crc32_z`] — CRC-32 computation with `usize` length.
pub use self::crc32::crc32_z;

/// Re-export of [`crc32::crc32_combine`] — combine two CRC-32 checksums.
pub use self::crc32::crc32_combine;

/// Re-export of [`crc32::crc32_combine64`] — combine two CRC-32 checksums (64-bit length).
pub use self::crc32::crc32_combine64;

/// Re-export of [`crc32::crc32_combine_gen`] — pre-compute CRC-32 combine operator.
pub use self::crc32::crc32_combine_gen;

/// Re-export of [`crc32::crc32_combine_gen64`] — pre-compute CRC-32 combine operator (64-bit length).
pub use self::crc32::crc32_combine_gen64;

/// Re-export of [`crc32::crc32_combine_op`] — combine CRC-32s with pre-computed operator.
pub use self::crc32::crc32_combine_op;

/// Re-export of [`crc32::get_crc_table`] — access the CRC-32 lookup table.
pub use self::crc32::get_crc_table;

/// Re-export of [`crc32::Crc32`] — streaming CRC-32 computer.
pub use self::crc32::Crc32;
