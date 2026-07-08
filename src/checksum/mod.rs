//! Checksum layer: Adler-32 (RFC 1950 zlib trailer) and CRC-32/IEEE (RFC 1952 gzip trailer).
//!
//! Idiomatic, memory-safe Rust ports of the C `adler32.c` and `crc32.c`. All output is
//! bit-identical to reference zlib. This layer is foundational: the deflate, inflate, and
//! gz modules build on it, and it is surfaced at the FFI boundary by `src/ffi/util.rs`.

pub mod adler32;
pub mod crc32;

pub use adler32::{adler32, adler32_combine, adler32_z};
pub use crc32::{
    crc32, crc32_combine, crc32_combine_gen, crc32_combine_op, crc32_z, get_crc_table,
};
