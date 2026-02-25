//! One-call compression and decompression wrappers.
//!
//! This module provides high-level convenience functions that perform complete
//! compression or decompression in a single call, without requiring the caller
//! to manage streaming state explicitly. These are the Rust equivalents of the
//! C functions `compress()`, `compress2()`, `compressBound()`, `uncompress()`,
//! and `uncompress2()` from `compress.c` and `uncompr.c`.
//!
//! Internally, each function creates a temporary [`ZStream`], initializes the
//! appropriate compression or decompression engine, processes all data, and
//! cleans up. This mirrors the C implementation's approach of wrapping the
//! streaming `deflate`/`inflate` API in one-shot convenience functions.
//!
//! # Examples
//!
//! ```no_run
//! use zlib_rs::compress::{compress, uncompress, compress_bound};
//! use zlib_rs::error::ReturnCode;
//!
//! let source = b"Hello, world! Hello, world! Hello, world!";
//! let mut compressed = Vec::new();
//! compress(&mut compressed, source).unwrap();
//!
//! let mut decompressed = vec![0u8; source.len()];
//! uncompress(&mut decompressed, &compressed).unwrap();
//! assert_eq!(&decompressed, source);
//! ```
//!
//! # Safety
//!
//! This module contains **zero** `unsafe` blocks. All buffer management uses
//! safe Rust slice operations and owned collections.

use crate::constants::{Z_DEFAULT_COMPRESSION, Z_FINISH, Z_NO_FLUSH};
use crate::deflate;
use crate::error::ReturnCode;
use crate::inflate;
use crate::stream::ZStream;

// ─── compress_bound ──────────────────────────────────────────────────────────

/// Returns an upper bound on the compressed size for a given source length.
///
/// The returned value is the maximum number of bytes that the compressed
/// output can occupy when compressing `source_len` bytes at the default
/// compression settings (`windowBits = 15`, `memLevel = 8`). Callers should
/// allocate at least this many bytes for the destination buffer when calling
/// [`compress`] or [`compress2`].
///
/// The bound is approximately 0.1% larger than `source_len` plus 12 bytes
/// of header/trailer overhead, matching the formula used by the C
/// `compressBound()` function.
///
/// If the computed bound overflows `usize` (which can happen for extremely
/// large inputs on 32-bit platforms), [`usize::MAX`] is returned.
///
/// # Parameters
///
/// * `source_len` — The length in bytes of the uncompressed source data.
///
/// # Returns
///
/// The maximum compressed output size in bytes.
///
/// # Examples
///
/// ```
/// use zlib_rs::compress::compress_bound;
///
/// let bound = compress_bound(1000);
/// assert!(bound > 1000); // always larger due to header overhead
/// assert!(bound < 1100); // but not by much (~0.1% + 13)
/// ```
///
/// # C Equivalent
///
/// ```c
/// z_size_t compressBound_z(z_size_t sourceLen);
/// ```
///
/// Ported from `compress.c` lines 91–95.
#[must_use]
pub fn compress_bound(source_len: usize) -> usize {
    // The formula matches C's compressBound_z exactly:
    //   sourceLen + (sourceLen >> 12) + (sourceLen >> 14)
    //            + (sourceLen >> 25) + 13
    //
    // This accounts for:
    //   - zlib header (2 bytes) and Adler-32 trailer (4 bytes) = 6 bytes
    //   - Deflate block headers and stored-block worst case
    //   - ~0.1% expansion for incompressible data
    //   - Rounding up to ensure the bound is always sufficient
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);

    // If the addition wrapped around (bound < source_len), return usize::MAX.
    // This mirrors the C code: `return bound < sourceLen ? (z_size_t)-1 : bound;`
    if bound < source_len {
        usize::MAX
    } else {
        bound
    }
}

// ─── compress ────────────────────────────────────────────────────────────────

/// Compresses a byte slice into a destination vector using the default
/// compression level.
///
/// This is the simplest compression interface. It uses
/// [`Z_DEFAULT_COMPRESSION`] (level 6) and handles all buffer sizing
/// internally. On success, `dest` is filled with the compressed data
/// (any previous contents are replaced).
///
/// # Parameters
///
/// * `dest` — Destination vector that will receive the compressed output.
///   Previous contents are discarded and replaced with the compressed data.
/// * `source` — The uncompressed input data to compress.
///
/// # Returns
///
/// `Ok(())` on success, or `Err(ReturnCode)` on failure.
///
/// # Errors
///
/// * [`ReturnCode::MemError`] — Insufficient memory to allocate internal
///   compression state.
/// * [`ReturnCode::BufError`] — Internal output buffer was insufficient
///   (should not occur with correct `compress_bound` sizing).
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::compress::compress;
///
/// let source = b"repetitive repetitive repetitive data";
/// let mut compressed = Vec::new();
/// compress(&mut compressed, source).unwrap();
/// assert!(compressed.len() < source.len());
/// ```
///
/// # C Equivalent
///
/// ```c
/// int compress(Bytef *dest, uLongf *destLen,
///              const Bytef *source, uLong sourceLen);
/// ```
///
/// Ported from `compress.c` lines 77–84.
pub fn compress(dest: &mut Vec<u8>, source: &[u8]) -> Result<(), ReturnCode> {
    compress2(dest, source, Z_DEFAULT_COMPRESSION)
}

// ─── compress2 ───────────────────────────────────────────────────────────────

/// Compresses a byte slice into a destination vector with a specified
/// compression level.
///
/// This function provides full control over the compression level while
/// handling all buffer management internally. The compression level
/// controls the trade-off between compression speed and compression ratio.
///
/// Internally, this function:
/// 1. Creates a temporary [`ZStream`].
/// 2. Initializes the deflate engine with the specified level.
/// 3. Sets the input to the entire source slice.
/// 4. Allocates an output buffer sized by [`compress_bound`].
/// 5. Compresses all data in a single `Z_FINISH` pass.
/// 6. Cleans up the deflate state.
///
/// On success, `dest` is filled with the compressed data (any previous
/// contents are replaced).
///
/// # Parameters
///
/// * `dest` — Destination vector that will receive the compressed output.
///   Previous contents are discarded and replaced with the compressed data.
/// * `source` — The uncompressed input data to compress.
/// * `level` — Compression level:
///   - `0` ([`Z_NO_COMPRESSION`](crate::constants::Z_NO_COMPRESSION)) — no
///     compression (stored)
///   - `1` ([`Z_BEST_SPEED`](crate::constants::Z_BEST_SPEED)) — fastest
///   - `6` — default balance of speed and size
///   - `9` ([`Z_BEST_COMPRESSION`](crate::constants::Z_BEST_COMPRESSION)) —
///     smallest output
///   - `-1` ([`Z_DEFAULT_COMPRESSION`]) — library default (currently 6)
///
/// # Returns
///
/// `Ok(())` on success, or `Err(ReturnCode)` on failure.
///
/// # Errors
///
/// * [`ReturnCode::StreamError`] — Invalid compression level.
/// * [`ReturnCode::MemError`] — Insufficient memory for internal state.
/// * [`ReturnCode::BufError`] — Internal output buffer was insufficient.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::compress::compress2;
/// use zlib_rs::constants::Z_BEST_COMPRESSION;
///
/// let source = b"compress this data at maximum level";
/// let mut compressed = Vec::new();
/// compress2(&mut compressed, source, Z_BEST_COMPRESSION).unwrap();
/// ```
///
/// # C Equivalent
///
/// ```c
/// int compress2(Bytef *dest, uLongf *destLen,
///               const Bytef *source, uLong sourceLen, int level);
/// ```
///
/// Ported from `compress.c` lines 24–66.
pub fn compress2(dest: &mut Vec<u8>, source: &[u8], level: i32) -> Result<(), ReturnCode> {
    let mut stream = ZStream::new();

    // Initialize the deflate engine with the requested compression level.
    let ret = deflate::deflate_init(&mut stream, level);
    if ret != ReturnCode::Ok {
        return Err(ret);
    }

    // Set up the input buffer — the entire source slice.
    stream.set_input(source);

    // Allocate the output buffer to the upper-bound compressed size.
    // This guarantees that a single deflate(Z_FINISH) call will have enough
    // room to produce the complete compressed output.
    let bound = compress_bound(source.len());
    stream.set_output_buffer(bound);

    // Compress all input data in a single pass with Z_FINISH.
    //
    // The C implementation uses a chunking loop because avail_in/avail_out
    // are limited to uInt (32-bit) while source/dest lengths can be
    // z_size_t (64-bit). In Rust, ZStream uses usize-based buffer management,
    // so the entire input and output fit without chunking.
    //
    // Since all input is provided at once via set_input() and all output
    // space is allocated via set_output_buffer(), we use Z_FINISH from the
    // first call — mirroring the C behavior when sourceLen == 0 (all fed).
    //
    // We loop in case deflate returns Z_OK (progress made but not yet
    // complete), though with compress_bound sizing it should complete in
    // one call returning Z_STREAM_END.
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }

    // Clean up deflate state. We capture the end result but prioritize the
    // compression result for error reporting.
    let _end_ret = deflate::deflate_end(&mut stream);

    // Interpret the result.
    // Z_STREAM_END means compression completed successfully — map to Ok.
    // Z_OK at this point means the output buffer was too small (shouldn't
    // happen with compress_bound sizing, but handle gracefully).
    // Any other code is an error.
    if ret == ReturnCode::StreamEnd {
        *dest = stream.take_output();
        Ok(())
    } else if ret == ReturnCode::Ok {
        // Output buffer insufficient — should not happen with compress_bound.
        Err(ReturnCode::BufError)
    } else {
        Err(ret)
    }
}

// ─── uncompress ──────────────────────────────────────────────────────────────

/// Decompresses a byte slice into a destination vector.
///
/// This is the simplest decompression interface. The caller must
/// pre-allocate `dest` to be large enough to hold the entire decompressed
/// output. The required size must have been saved previously by the
/// compressor and transmitted to the decompressor by some mechanism
/// outside the scope of this compression library.
///
/// On success, `dest` is truncated to the actual decompressed size.
///
/// # Parameters
///
/// * `dest` — Destination vector pre-allocated to the expected decompressed
///   size. On success, truncated to the actual output size.
/// * `source` — The compressed input data.
///
/// # Returns
///
/// `Ok(())` on success, or `Err(ReturnCode)` on failure.
///
/// # Errors
///
/// * [`ReturnCode::MemError`] — Insufficient memory for internal state.
/// * [`ReturnCode::BufError`] — `dest` is too small for the decompressed data.
/// * [`ReturnCode::DataError`] — The compressed data is corrupted or
///   incomplete.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::compress::{compress, uncompress};
///
/// let source = b"Hello, world!";
/// let mut compressed = Vec::new();
/// compress(&mut compressed, source).unwrap();
///
/// let mut decompressed = vec![0u8; source.len()];
/// uncompress(&mut decompressed, &compressed).unwrap();
/// assert_eq!(&decompressed, source);
/// ```
///
/// # C Equivalent
///
/// ```c
/// int uncompress(Bytef *dest, uLongf *destLen,
///                const Bytef *source, uLong sourceLen);
/// ```
///
/// Ported from `uncompr.c` lines 92–101.
pub fn uncompress(dest: &mut Vec<u8>, source: &[u8]) -> Result<(), ReturnCode> {
    let (_dest_used, _source_consumed) = uncompress2(dest, source)?;
    Ok(())
}

// ─── uncompress2 ─────────────────────────────────────────────────────────────

/// Decompresses a byte slice into a destination vector, returning the
/// actual sizes consumed and produced.
///
/// This is the detailed decompression interface. The caller must
/// pre-allocate `dest` to be large enough to hold the entire decompressed
/// output (its current `len()` is used as the maximum output capacity).
///
/// On success, `dest` is truncated to the actual decompressed size. The
/// return value provides both the number of output bytes produced and the
/// number of input bytes consumed, which is useful when the source buffer
/// contains additional data after the compressed stream.
///
/// # Parameters
///
/// * `dest` — Destination vector pre-allocated to the expected decompressed
///   size. On success, truncated to the actual output size.
/// * `source` — The compressed input data.
///
/// # Returns
///
/// `Ok((dest_used, source_consumed))` on success, where:
/// - `dest_used` — Number of decompressed bytes written to `dest`.
/// - `source_consumed` — Number of compressed bytes consumed from `source`.
///
/// # Errors
///
/// * [`ReturnCode::MemError`] — Insufficient memory for internal state.
/// * [`ReturnCode::BufError`] — `dest` is too small for the decompressed
///   data, and not all input was consumed yet.
/// * [`ReturnCode::DataError`] — The compressed data is corrupted,
///   incomplete, or requires a preset dictionary.
///
/// # Error Mapping
///
/// This function maps certain inflate return codes to match the C behavior:
///
/// | Inflate result | Condition | Returned error |
/// |----------------|-----------|----------------|
/// | `StreamEnd` | — | `Ok(...)` |
/// | `NeedDict` | — | `DataError` |
/// | `BufError` | all input consumed | `DataError` |
/// | Other | — | passthrough |
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::compress::{compress, uncompress2};
///
/// let source = b"Hello, world!";
/// let mut compressed = Vec::new();
/// compress(&mut compressed, source).unwrap();
///
/// let mut decompressed = vec![0u8; source.len()];
/// let (dest_used, src_consumed) = uncompress2(&mut decompressed, &compressed).unwrap();
/// assert_eq!(dest_used, source.len());
/// assert_eq!(src_consumed, compressed.len());
/// ```
///
/// # C Equivalent
///
/// ```c
/// int uncompress2(Bytef *dest, uLongf *destLen,
///                 const Bytef *source, uLong *sourceLen);
/// ```
///
/// Ported from `uncompr.c` lines 29–82.
#[allow(clippy::cast_possible_truncation)]
pub fn uncompress2(dest: &mut Vec<u8>, source: &[u8]) -> Result<(usize, usize), ReturnCode> {
    let mut stream = ZStream::new();

    // Initialize the inflate engine.
    let ret = inflate::inflate_init(&mut stream);
    if ret != ReturnCode::Ok {
        return Err(ret);
    }

    let dest_capacity = dest.len();

    // Set up input — the entire compressed source.
    stream.set_input(source);

    // Set up output — use the caller's pre-allocated dest size as capacity.
    // If dest_capacity is 0, allocate a minimal buffer so the stream's
    // output pointer is valid (mirrors C's `dest = &stream.reserved` trick).
    if dest_capacity == 0 {
        stream.set_output_buffer(1);
    } else {
        stream.set_output_buffer(dest_capacity);
    }

    // Extract the inflate state from the stream so we can call inflate()
    // which takes separate state and stream parameters.
    let Some(mut state_box) = stream.state.take() else {
        return Err(ReturnCode::StreamError);
    };

    let Some(state) = state_box.downcast_mut::<inflate::InflateState>() else {
        // Put state back before returning error.
        stream.state = Some(state_box);
        return Err(ReturnCode::StreamError);
    };

    // Decompress in a loop.
    //
    // The C implementation loops because avail_in/avail_out may be smaller
    // than the total lengths (uInt vs z_size_t). In Rust with usize-based
    // buffers, a single inflate call typically processes everything. We still
    // loop for correctness: inflate returns Z_OK when progress was made but
    // the stream is not yet complete.
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(state, &mut stream, Z_NO_FLUSH);
    }

    // Track remaining unconsumed input for the BufError→DataError mapping.
    // This must be captured before inflate_end clears state.
    let remaining_input = stream.avail_in();

    // Calculate source consumed = total source provided minus remaining input.
    let source_consumed = source.len() - remaining_input;

    // Clean up inflate state.
    let _end_ret = inflate::inflate_end(state, &mut stream);

    // Restore state box to stream (will be dropped when stream goes out of scope).
    stream.state = Some(state_box);

    // Take the decompressed output from the stream.
    // The output vector length equals the number of bytes actually written,
    // which is the most reliable measure of decompressed output size.
    let output = stream.take_output();
    let dest_used = output.len();

    // Copy the decompressed output into dest.
    dest.clear();
    dest.extend_from_slice(&output);

    // Map error codes per C uncompr.c behavior (lines 78–81):
    //
    //   return err == Z_STREAM_END ? Z_OK :
    //          err == Z_NEED_DICT ? Z_DATA_ERROR :
    //          err == Z_BUF_ERROR && len == 0 ? Z_DATA_ERROR :
    //          err;
    //
    // Where `len == 0` means all source input was consumed (no remaining
    // unconsumed bytes). This detects truncated/incomplete streams.
    match ret {
        ReturnCode::StreamEnd => Ok((dest_used, source_consumed)),
        ReturnCode::NeedDict => Err(ReturnCode::DataError),
        ReturnCode::BufError if remaining_input == 0 => Err(ReturnCode::DataError),
        other => Err(other),
    }
}
