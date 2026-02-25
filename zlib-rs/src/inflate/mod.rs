//! DEFLATE decompression engine.
//!
//! This module provides the inflate (decompression) implementation ported
//! from the C zlib library's `inflate.c`, `inftrees.c`, `inffast.c`,
//! `infback.c`, and `inffixed.h`.

pub mod table;

// Re-export primary public types from the table module.
pub use table::{Code, CodeType, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS};
