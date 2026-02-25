//! Checksum computation engines for data integrity verification.
//!
//! This module provides two checksum algorithms used throughout the zlib
//! compression library:
//!
//! - **CRC-32** — The cyclic redundancy check used in the gzip format (RFC 1952)
//!   and for combining checksums of concatenated data.
//!
//! Both implementations are pure safe Rust with zero `unsafe` blocks. Lookup
//! tables are compile-time `const` arrays — no runtime initialization is needed
//! (replacing the C `DYNAMIC_CRC_TABLE` pattern).

pub mod crc32;
