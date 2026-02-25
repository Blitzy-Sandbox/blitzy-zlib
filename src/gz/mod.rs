//! Gzip file I/O operations.
//!
//! This module provides a stdio-like interface for reading and writing gzip
//! (.gz) files. It supports transparent reading of both gzip and non-gzip
//! files, buffered I/O, seeking within compressed streams, and dynamic
//! compression parameter changes.
//!
//! Port of C zlib's gzip file I/O layer:
//! - `gzlib.c` (609 lines) — open, seek, tell, error operations
//! - `gzread.c` (668 lines) — read pipeline with LOOK/COPY/GZIP state machine
//! - `gzwrite.c` (700 lines) — write pipeline with buffered compression
//! - `gzclose.c` (23 lines) — close dispatcher
//! - `gzguts.h` (216 lines) — internal state structure
//!
//! # Feature Gate
//!
//! This module is only available when the `gz-io` feature is enabled
//! (which is a default feature). The `gz-io` feature implies both `std`
//! and `gzip` features.

pub mod state;
pub mod open;
pub mod read;
pub mod write;
pub mod close;
