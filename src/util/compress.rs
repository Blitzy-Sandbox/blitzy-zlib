// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// One-call compression convenience wrappers.
// Port of C `compress.c` (99 lines).

//! One-call compression utilities.
//!
//! This module provides convenience wrappers that compress a complete source
//! buffer into a destination buffer in a single function call, abstracting
//! away the streaming `deflate_init` / `deflate` / `deflate_end` lifecycle.
//!
//! # Functions
//!
//! - [`compress`] — Compress with the default compression level (-1 → level 6).
//! - [`compress2`] — Compress with an explicit compression level (0–9 or -1).
//! - [`compress_bound`] — Calculate an upper bound on the compressed output size.
//!
//! # C-to-Rust Mapping
//!
//! In C zlib, there are separate `_z` (accepting `z_size_t`) and non-`_z`
//! (accepting `uLong`) variants of each function. In Rust, `usize` is used
//! natively, so a single function replaces both variants:
//!
//! | C Functions                     | Rust Equivalent     |
//! |---------------------------------|---------------------|
//! | `compress`, `compress_z`        | [`compress`]        |
//! | `compress2`, `compress2_z`      | [`compress2`]       |
//! | `compressBound`, `compressBound_z` | [`compress_bound`] |
//!
//! # Examples
//!
//! ```no_run
//! use zlib_rs::util::compress::{compress, compress2, compress_bound};
//!
//! let source = b"Hello, zlib-rs!";
//! let bound = compress_bound(source.len());
//! let mut dest = vec![0u8; bound];
//!
//! // Compress with default level
//! let compressed_len = compress(&mut dest, source).unwrap();
//! dest.truncate(compressed_len);
//! ```

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

use crate::constants::{Z_DEFAULT_COMPRESSION, Z_FINISH, Z_NO_FLUSH};
use crate::deflate;
use crate::error::{ReturnCode, ZlibError, ZlibResult};
use crate::stream::ZStream;

// ---------------------------------------------------------------------------
// compress — default-level convenience wrapper
// ---------------------------------------------------------------------------

/// Compresses the source buffer into the destination buffer using the default
/// compression level.
///
/// Returns the number of bytes written to `dest` on success. The destination
/// buffer must be at least [`compress_bound`]`(source.len())` bytes long to
/// guarantee success.
///
/// This is a one-call convenience wrapper around the streaming deflate API.
/// It is equivalent to C zlib's `compress()` / `compress_z()`.
///
/// # Errors
///
/// - [`ZlibError::MemError`] — insufficient memory to allocate compression
///   state (~256 KB at default settings).
/// - [`ZlibError::BufError`] — the destination buffer is too small to hold
///   the compressed output.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::util::compress::{compress, compress_bound};
///
/// let source = b"Hello, World!";
/// let bound = compress_bound(source.len());
/// let mut dest = vec![0u8; bound];
/// let compressed_len = compress(&mut dest, source).unwrap();
/// dest.truncate(compressed_len);
/// assert!(compressed_len > 0);
/// ```
pub fn compress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    compress2(dest, source, Z_DEFAULT_COMPRESSION)
}

// ---------------------------------------------------------------------------
// compress2 — explicit-level compression
// ---------------------------------------------------------------------------

/// Compresses the source buffer into the destination buffer with an explicit
/// compression level.
///
/// Returns the number of bytes written to `dest` on success. The `level`
/// parameter accepts values from 0 (no compression) to 9 (best compression),
/// or [`Z_DEFAULT_COMPRESSION`](`crate::constants::Z_DEFAULT_COMPRESSION`)
/// (-1) for the default compromise (currently level 6).
///
/// This is a one-call convenience wrapper around the streaming deflate API.
/// It is equivalent to C zlib's `compress2()` / `compress2_z()`.
///
/// # Algorithm
///
/// Internally, `compress2` performs the following steps:
///
/// 1. Creates a fresh [`ZStream`] and initializes deflate state via
///    [`deflate_init`](`crate::deflate::deflate_init`).
/// 2. Feeds input and output in u32-sized chunks (matching C zlib's `uInt`
///    overflow handling for buffers exceeding 4 GB on 64-bit platforms).
/// 3. Calls [`deflate`](`crate::deflate::deflate`) in a loop with
///    [`Z_NO_FLUSH`] while source data remains, switching to [`Z_FINISH`]
///    for the final call.
/// 4. Always calls [`deflate_end`](`crate::deflate::deflate_end`) to release
///    internal state, even on error.
///
/// # Errors
///
/// - [`ZlibError::StreamError`] — the compression `level` is invalid.
/// - [`ZlibError::MemError`] — insufficient memory to allocate compression
///   state.
/// - [`ZlibError::BufError`] — the destination buffer is too small.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::util::compress::{compress2, compress_bound};
/// use zlib_rs::constants::Z_BEST_SPEED;
///
/// let source = b"Repetitive data: aaaaaaaaaa";
/// let bound = compress_bound(source.len());
/// let mut dest = vec![0u8; bound];
/// let compressed_len = compress2(&mut dest, source, Z_BEST_SPEED).unwrap();
/// dest.truncate(compressed_len);
/// assert!(compressed_len > 0);
/// ```
pub fn compress2(dest: &mut [u8], source: &[u8], level: i32) -> Result<usize, ZlibError> {
    // 1. Create a new stream and initialize deflate state.
    let mut stream = ZStream::new();
    deflate::deflate_init(&mut stream, level)?;

    // 2. Set up buffer pointers.
    //
    // The ZStream's avail_in/avail_out fields are u32, but the source/dest
    // slices may exceed u32::MAX on 64-bit platforms. We use set_input() and
    // set_output() for buffers that fit in u32, falling back to direct
    // pointer assignment for larger buffers. In both cases, we then reset
    // avail_in and avail_out to 0 and feed data in u32-sized chunks in the
    // loop below, exactly matching C zlib's compress2_z pattern.
    if source.len() <= u32::MAX as usize {
        stream.set_input(source);
    } else {
        // Direct pointer setup for >4 GB buffers (extremely rare).
        stream.next_in = source.as_ptr();
    }
    if dest.len() <= u32::MAX as usize {
        stream.set_output(dest);
    } else {
        // Direct pointer setup for >4 GB buffers (extremely rare).
        stream.next_out = dest.as_mut_ptr();
    }

    // Reset avail counts to 0; the loop below feeds u32-sized chunks.
    stream.avail_out = 0;
    stream.avail_in = 0;

    // Track remaining data as usize (can exceed u32::MAX).
    let mut left = dest.len();
    let mut source_remaining = source.len();

    // 3. Main compression loop — port of C compress2_z's do-while loop.
    //
    // Each iteration:
    //   a) If avail_out is exhausted, feed the next chunk of dest capacity.
    //   b) If avail_in is exhausted, feed the next chunk of source data.
    //   c) Select flush mode: Z_NO_FLUSH while source remains, Z_FINISH once
    //      all source data has been fed.
    //   d) Call deflate(). Continue while deflate returns Ok (ReturnCode::Ok).
    //      Any other result (StreamEnd, or an error) breaks the loop.
    let result: ZlibResult = loop {
        // (a) Feed output capacity in u32-sized chunks.
        if stream.avail_out == 0 {
            let chunk = left.min(u32::MAX as usize);
            stream.avail_out = chunk as u32;
            left -= chunk;
        }

        // (b) Feed input data in u32-sized chunks.
        if stream.avail_in == 0 {
            let chunk = source_remaining.min(u32::MAX as usize);
            stream.avail_in = chunk as u32;
            source_remaining -= chunk;
        }

        // (c) Select flush mode.
        let flush = if source_remaining > 0 {
            Z_NO_FLUSH
        } else {
            Z_FINISH
        };

        // (d) Perform compression.
        let r = deflate::deflate(&mut stream, flush);
        match r {
            Ok(ReturnCode::Ok) => continue,
            other => break other,
        }
    };

    // 4. Record total compressed bytes before cleanup.
    let total = stream.total_out as usize;

    // 5. Always clean up the deflate state, even on error.
    //    The result of deflate_end is intentionally discarded — the primary
    //    error (if any) is from the compression loop above, and the state
    //    cleanup is for resource management only (Rust's Drop would also
    //    handle this, but explicit cleanup matches C zlib's contract).
    let _ = deflate::deflate_end(&mut stream);

    // 6. Map the loop result to the caller's expected return type.
    //    C zlib: `return err == Z_STREAM_END ? Z_OK : err;`
    match result {
        Ok(ReturnCode::StreamEnd) => Ok(total),
        Ok(_) => {
            // Compression ended without StreamEnd — output buffer was too
            // small or an unexpected success code was returned.
            Err(ZlibError::BufError)
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// compress_bound — upper-bound calculation
// ---------------------------------------------------------------------------

/// Returns an upper bound on the compressed output size for a given
/// uncompressed source length.
///
/// The returned value is the minimum destination buffer size that guarantees
/// [`compress`] or [`compress2`] will succeed for any input of the given
/// length.
///
/// # Formula
///
/// The formula **must** match C zlib exactly (per AAP §0.8.1):
///
/// ```text
/// source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13
/// ```
///
/// The `+13` accounts for the zlib wrapper overhead: a 2-byte header, a
/// 4-byte Adler-32 trailer, plus conservative padding for stored block
/// headers when the data is incompressible.
///
/// # Overflow Handling
///
/// If the computed bound overflows `usize` (i.e., `bound < source_len` due
/// to wrapping arithmetic), [`usize::MAX`] is returned. This mirrors C
/// zlib's `compressBound_z` returning `(z_size_t)-1` on overflow.
///
/// # Note
///
/// This function assumes the default `deflateInit` parameters
/// (`windowBits=15`, `memLevel=8`). If you use `deflate_init2` with
/// different settings, use [`deflate_bound`](`crate::deflate::deflate_bound`)
/// instead for a tighter bound.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::compress::compress_bound;
///
/// assert_eq!(compress_bound(0), 13);
/// assert_eq!(compress_bound(1), 14);
/// assert_eq!(compress_bound(100), 113);
/// assert_eq!(compress_bound(usize::MAX), usize::MAX);
/// ```
pub fn compress_bound(source_len: usize) -> usize {
    // Exact C formula: sourceLen + (sourceLen >> 12) + (sourceLen >> 14) +
    //                  (sourceLen >> 25) + 13
    // Uses wrapping_add to detect overflow without panicking.
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);

    // Overflow guard: if the bound wrapped around to a value less than the
    // source length, the true bound exceeds usize::MAX.
    if bound < source_len {
        usize::MAX
    } else {
        bound
    }
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // compress_bound tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compress_bound_zero() {
        // Empty input needs only the zlib wrapper overhead.
        assert_eq!(compress_bound(0), 13);
    }

    #[test]
    fn test_compress_bound_one() {
        // 1 + 0 + 0 + 0 + 13 = 14
        assert_eq!(compress_bound(1), 14);
    }

    #[test]
    fn test_compress_bound_hundred() {
        // 100 + (100>>12=0) + (100>>14=0) + (100>>25=0) + 13 = 113
        assert_eq!(compress_bound(100), 113);
    }

    #[test]
    fn test_compress_bound_large() {
        // For 1 MiB = 1_048_576 = 2^20:
        // 1048576 + (1048576>>12=256) + (1048576>>14=64) + (1048576>>25=0) + 13
        // = 1048576 + 256 + 64 + 0 + 13 = 1048909
        assert_eq!(compress_bound(1_048_576), 1_048_909);
    }

    #[test]
    fn test_compress_bound_overflow() {
        // For usize::MAX, the formula overflows → returns usize::MAX.
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
    }

    #[test]
    fn test_compress_bound_near_overflow() {
        // For a very large value that should still not overflow on 64-bit:
        // usize::MAX - 1000 should still overflow due to the additions.
        let big = usize::MAX - 1000;
        assert_eq!(compress_bound(big), usize::MAX);
    }
}
