//! A pure safe Rust implementation of the zlib compression library.
#![forbid(unsafe_code)]

/// Checksum algorithms: Adler-32 and CRC-32.
pub mod checksum;
/// Public constants: flush modes, strategies, compression levels, and limits.
pub mod constants;
/// DEFLATE compression engine: streaming compression, dictionary support, and parameter tuning.
pub mod deflate;
/// Return codes and error types for zlib operations.
pub mod error;
/// DEFLATE decompression engine: streaming decompression, sync recovery, and format auto-detection.
pub mod inflate;
/// Streaming I/O types: `ZStream` and `GzHeader`.
pub mod stream;
/// Internal utility functions and constants.
pub mod util;

/// Gzip file I/O: buffered reading and writing of gzip-format files.
#[cfg(feature = "gz-io")]
pub mod gz;
