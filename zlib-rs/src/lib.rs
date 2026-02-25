//! A pure safe Rust implementation of the zlib compression library.
#![forbid(unsafe_code)]

pub mod checksum;
pub mod constants;
pub mod deflate;
pub mod error;
pub mod inflate;
pub mod stream;
pub mod util;
