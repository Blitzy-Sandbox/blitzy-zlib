//! A pure safe Rust implementation of the zlib compression library.
#![forbid(unsafe_code)]

pub mod constants;
pub mod error;
pub mod checksum;
pub mod inflate;
