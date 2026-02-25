//! DEFLATE compression engine.
//!
//! This module provides the deflate (compression) implementation ported
//! from the C zlib library's `deflate.c`, `trees.c`, and related sources.

/// Compression configuration table mapping levels 0–9 to parameters.
pub mod params;

/// DeflateState struct and supporting types (status enum, CtData, newtypes).
pub mod state;

// Re-export primary public types from the params module.
pub use params::{CONFIGURATION_TABLE, CompressionConfig, CompressionFunc};

// Re-export primary public types from the state module.
pub use state::{CtData, DeflateState, DeflateStatus, IPos, Pos, TreeDescState};
