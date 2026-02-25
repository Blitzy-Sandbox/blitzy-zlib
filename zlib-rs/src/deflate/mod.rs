//! DEFLATE compression engine.
//!
//! This module provides the deflate (compression) implementation ported
//! from the C zlib library's `deflate.c`, `trees.c`, and related sources.

/// Compression configuration table mapping levels 0–9 to parameters.
pub mod params;

/// DeflateState struct and supporting types (status enum, CtData, newtypes).
pub mod state;

/// Hash chain management, window filling, and longest-match search.
pub mod hash;

/// Huffman tree construction, bit-level output, and block encoding.
pub mod trees;

/// Five compression strategy functions (stored, fast, slow, rle, huff).
pub mod algorithm;

// Re-export primary public types from the params module.
pub use params::{CONFIGURATION_TABLE, CompressionConfig, CompressionFunc};

// Re-export primary public types from the state module.
pub use state::{CtData, DeflateState, DeflateStatus, IPos, Pos, TreeDescState};
