//! Gzip file I/O module.
//!
//! Provides buffered reading and writing of gzip-compressed files,
//! with auto-detection of gzip vs transparent data on read.
//! This module is conditionally compiled when the `gz-io` feature is enabled.

pub(crate) mod state;
