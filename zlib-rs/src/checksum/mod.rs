//! Integrity checksums for the zlib/gzip wire formats.
//!
//! - [`adler32`]: Adler-32 running checksum (RFC 1950 / zlib container).
//! - [`crc32`]: CRC-32 (IEEE 802.3, reflected polynomial `0xedb88320`; RFC 1952 / gzip container).
//!
//! This is the foundational leaf module of the `zlib-rs` core: it depends on nothing
//! else in `src/`, is entirely safe (crate-level `#![forbid(unsafe_code)]`), and is
//! `no_std`-capable. All functions return the **raw `u32` checksum**; the on-the-wire
//! trailer endianness (gzip CRC-32 little-endian, zlib Adler-32 big-endian) is applied
//! by the callers (`deflate`/`inflate`/`gz`).

pub mod adler32;
pub mod crc32;

pub use adler32::{adler32, adler32_combine, adler32_z};
pub use crc32::{
    crc32, crc32_combine, crc32_combine_gen, crc32_combine_op, crc32_z, get_crc_table,
};
