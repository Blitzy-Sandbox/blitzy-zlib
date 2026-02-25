//! DEFLATE decompression engine.
//!
//! This module provides the inflate (decompression) implementation ported
//! from the C zlib library's `inflate.c`, `inftrees.c`, `inffast.c`,
//! `infback.c`, and `inffixed.h`.

pub mod back;
pub mod fast;
pub mod fixed;
pub mod state;
pub mod table;

// Re-export primary public types from the table module.
pub use table::{Code, CodeType, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS};

// Re-export primary public types from the state module.
pub use state::{CodeTableRef, InflateMode, InflateState};
