//! DEFLATE compression engine.
//!
//! This module implements the complete DEFLATE compression algorithm as
//! specified in RFC 1951, with zlib (RFC 1950) and gzip (RFC 1952) framing
//! support.

pub mod state;
pub mod trees;
pub mod rle;
pub mod huff;
pub mod stored;
pub mod slow;
pub mod fast;
