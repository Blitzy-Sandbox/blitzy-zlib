//! Checksum computation for zlib and gzip formats.
//!
//! This module provides Adler-32 and CRC-32 checksum algorithms:
//!
//! - **Adler-32** is used in zlib format headers and trailers (RFC 1950).
//!   It is faster to compute than CRC-32 but provides weaker error detection.
//!
//! - **CRC-32** is used in gzip format headers and trailers (RFC 1952).
//!   It provides stronger error detection at a slightly higher computational cost.
//!
//! Both checksums support combine operations for parallel or incremental
//! computation of concatenated data streams.
//!
//! This module is `no_std` compatible when the `std` feature is disabled.

pub mod adler32;
pub mod crc32;

// Re-export Adler-32 public API for convenience access via `checksum::*`.
pub use self::adler32::Adler32;
pub use self::adler32::adler32 as adler32_func;
pub use self::adler32::adler32_combine;
pub use self::adler32::adler32_combine64;
pub use self::adler32::adler32_z;

// Re-export CRC-32 public API for convenience access via `checksum::*`.
pub use self::crc32::Crc32;
pub use self::crc32::crc32;
pub use self::crc32::crc32_combine;
pub use self::crc32::crc32_combine_gen;
pub use self::crc32::crc32_combine_gen64;
pub use self::crc32::crc32_combine_op;
pub use self::crc32::crc32_combine64;
pub use self::crc32::crc32_z;
pub use self::crc32::get_crc_table;
