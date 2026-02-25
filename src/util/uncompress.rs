// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// One-call decompression convenience wrappers.
// Port of C `uncompr.c` (101 lines).

//! One-call decompression utilities.
//!
//! This module provides convenience wrappers that decompress a complete source
//! buffer into a destination buffer in a single function call, abstracting
//! away the streaming `inflate_init` / `inflate` / `inflate_end` lifecycle.
//!
//! # Functions
//!
//! - [`uncompress`] — Decompress, returning the number of decompressed bytes.
//! - [`uncompress2`] — Decompress, returning both bytes written and bytes
//!   consumed.
//!
//! # C-to-Rust Mapping
//!
//! In C zlib, there are separate `_z` (accepting `z_size_t`) and non-`_z`
//! (accepting `uLong`) variants of each function. In Rust, `usize` is used
//! natively, so a single function replaces both variants:
//!
//! | C Functions                       | Rust Equivalent       |
//! |-----------------------------------|-----------------------|
//! | `uncompress`, `uncompress_z`      | [`uncompress`]        |
//! | `uncompress2`, `uncompress2_z`    | [`uncompress2`]       |
//!
//! # Examples
//!
//! ```no_run
//! use zlib_rs::util::uncompress::{uncompress, uncompress2};
//! use zlib_rs::util::compress::{compress, compress_bound};
//!
//! let source = b"Hello, zlib-rs!";
//! let bound = compress_bound(source.len());
//! let mut compressed = vec![0u8; bound];
//! let comp_len = compress(&mut compressed, source).unwrap();
//! compressed.truncate(comp_len);
//!
//! let mut decompressed = vec![0u8; source.len()];
//! let decomp_len = uncompress(&mut decompressed, &compressed).unwrap();
//! assert_eq!(&decompressed[..decomp_len], source);
//! ```

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

use crate::constants::Z_NO_FLUSH;
use crate::error::{ReturnCode, ZlibError, ZlibResult};
use crate::inflate;
use crate::stream::ZStream;

// ---------------------------------------------------------------------------
// uncompress — convenience wrapper returning only bytes written
// ---------------------------------------------------------------------------

/// Decompresses the source buffer into the destination buffer.
///
/// Returns the number of bytes written to `dest` on success. The destination
/// buffer must be large enough to hold the entire uncompressed data. (The
/// size of the uncompressed data must have been saved previously by the
/// compressor and transmitted to the decompressor by some mechanism outside
/// the scope of this library.)
///
/// This is a one-call convenience wrapper around the streaming inflate API.
/// It is equivalent to C zlib's `uncompress()` / `uncompress_z()`.
///
/// # Errors
///
/// - [`ZlibError::MemError`] — insufficient memory to allocate decompression
///   state.
/// - [`ZlibError::BufError`] — the destination buffer is too small to hold
///   the decompressed output.
/// - [`ZlibError::DataError`] — the input data is corrupted, represents an
///   incomplete zlib stream, or requires a preset dictionary.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::util::compress::{compress, compress_bound};
/// use zlib_rs::util::uncompress::uncompress;
///
/// let source = b"Hello, World!";
/// let bound = compress_bound(source.len());
/// let mut compressed = vec![0u8; bound];
/// let comp_len = compress(&mut compressed, source).unwrap();
/// compressed.truncate(comp_len);
///
/// let mut decompressed = vec![0u8; source.len()];
/// let decomp_len = uncompress(&mut decompressed, &compressed).unwrap();
/// assert_eq!(&decompressed[..decomp_len], source);
/// ```
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    let (bytes_written, _bytes_consumed) = uncompress2(dest, source)?;
    Ok(bytes_written)
}

// ---------------------------------------------------------------------------
// uncompress2 — full version returning (bytes_written, bytes_consumed)
// ---------------------------------------------------------------------------

/// Decompresses the source buffer into the destination buffer, returning both
/// the number of bytes written and the number of source bytes consumed.
///
/// Unlike [`uncompress`], this function also reports how many source bytes
/// were consumed, allowing the caller to detect trailing data after the
/// compressed stream. On success, `source[..bytes_consumed]` is the exact
/// compressed payload, and `source[bytes_consumed..]` contains any trailing
/// data.
///
/// This is a one-call convenience wrapper around the streaming inflate API.
/// It is equivalent to C zlib's `uncompress2()` / `uncompress2_z()`.
///
/// # Algorithm
///
/// Internally, `uncompress2` performs the following steps:
///
/// 1. Creates a fresh [`ZStream`] and initialises inflate state via
///    [`inflate_init`](`crate::inflate::inflate_init`).
/// 2. Feeds input and output in `u32`-sized chunks (matching C zlib's `uInt`
///    overflow handling for buffers exceeding 4 GB on 64-bit platforms).
/// 3. Calls [`inflate`](`crate::inflate::inflate`) in a loop with
///    [`Z_NO_FLUSH`] until the stream ends or an error occurs.
/// 4. Always calls [`inflate_end`](`crate::inflate::inflate_end`) to release
///    internal state, even on error.
///
/// # Return Value
///
/// On success: `Ok((decompressed_bytes, source_bytes_consumed))`
///
/// # Errors
///
/// - [`ZlibError::MemError`] — insufficient memory to allocate decompression
///   state.
/// - [`ZlibError::BufError`] — the destination buffer is too small to hold
///   the decompressed output.
/// - [`ZlibError::DataError`] — the input data is corrupted, represents an
///   incomplete zlib stream, or requires a preset dictionary (which is not
///   supported by the one-call API).
///
/// # Error Mapping (matches C `uncompress2_z` lines 78–81)
///
/// | Inflate Result     | Condition           | Returned Error       |
/// |--------------------|---------------------|----------------------|
/// | `StreamEnd`        | —                   | `Ok(…)`              |
/// | `NeedDict`         | —                   | `DataError`          |
/// | `BufError`         | all input consumed  | `DataError`          |
/// | `BufError`         | input remains       | `BufError`           |
/// | any other error    | —                   | passed through       |
///
/// The `NeedDict → DataError` mapping reflects that the one-call API does not
/// support preset dictionaries; a stream requiring one is treated as invalid
/// data. The `BufError` with all input consumed indicates an incomplete
/// compressed stream (all data was read but decompression never completed),
/// which is also reported as a data error.
///
/// # Examples
///
/// ```no_run
/// use zlib_rs::util::compress::{compress, compress_bound};
/// use zlib_rs::util::uncompress::uncompress2;
///
/// let source = b"Hello, World!";
/// let bound = compress_bound(source.len());
/// let mut compressed = vec![0u8; bound];
/// let comp_len = compress(&mut compressed, source).unwrap();
/// compressed.truncate(comp_len);
///
/// let mut decompressed = vec![0u8; source.len()];
/// let (written, consumed) = uncompress2(&mut decompressed, &compressed).unwrap();
/// assert_eq!(written, source.len());
/// assert_eq!(consumed, compressed.len());
/// assert_eq!(&decompressed[..written], source);
/// ```
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    // Track remaining data as usize (can exceed u32::MAX on 64-bit platforms).
    // These variables mirror C uncompress2_z's `len` and `left` respectively.
    let mut source_remaining: usize = source.len(); // C: len = *sourceLen
    let mut dest_remaining: usize = dest.len(); // C: left = *destLen

    // 1. Create a new stream.
    let mut stream = ZStream::new();

    // Set next_in pointer before inflate_init (matches C order where
    // stream.next_in is set before inflateInit, and avail_in = 0).
    // For buffers that fit in u32, use the safe set_input helper; for
    // larger buffers (extremely rare, >4 GB), set the pointer directly.
    if source.len() <= u32::MAX as usize {
        stream.set_input(source);
    } else {
        stream.next_in = source.as_ptr();
    }
    stream.avail_in = 0;

    // 2. Initialise the inflate state.
    //    If this fails (e.g. MemError), return immediately — no state was
    //    allocated, so inflate_end is not needed.
    inflate::inflate_init(&mut stream)?;

    // 3. Set next_out pointer AFTER inflate_init (matches C order where
    //    stream.next_out is set after successful inflateInit).
    if dest.len() <= u32::MAX as usize {
        stream.set_output(dest);
    } else {
        stream.next_out = dest.as_mut_ptr();
    }
    stream.avail_out = 0;

    // 4. Main decompression loop — port of C uncompress2_z's do-while loop.
    //
    // Each iteration:
    //   a) If avail_out is exhausted, feed the next chunk of dest capacity.
    //   b) If avail_in is exhausted, feed the next chunk of source data.
    //   c) Call inflate(&stream, Z_NO_FLUSH).
    //   d) Continue while inflate returns Ok (ReturnCode::Ok).
    //      Any other result (StreamEnd, NeedDict, or an error) breaks.
    let result: ZlibResult = loop {
        // (a) Feed output capacity in u32-sized chunks.
        // C: if (stream.avail_out == 0) {
        //        stream.avail_out = left > max ? max : (uInt)left;
        //        left -= stream.avail_out;
        //    }
        if stream.avail_out == 0 {
            let chunk = dest_remaining.min(u32::MAX as usize);
            stream.avail_out = chunk as u32;
            dest_remaining -= chunk;
        }

        // (b) Feed input data in u32-sized chunks.
        // C: if (stream.avail_in == 0) {
        //        stream.avail_in = len > max ? max : (uInt)len;
        //        len -= stream.avail_in;
        //    }
        if stream.avail_in == 0 {
            let chunk = source_remaining.min(u32::MAX as usize);
            stream.avail_in = chunk as u32;
            source_remaining -= chunk;
        }

        // (c) Perform decompression.
        // C: err = inflate(&stream, Z_NO_FLUSH);
        let r = inflate::inflate(&mut stream, Z_NO_FLUSH);
        match r {
            Ok(ReturnCode::Ok) => continue,
            other => break other,
        }
    };

    // 5. After the loop, add back unconsumed data to the remaining counts.
    //    C: len += stream.avail_in;
    //       left += stream.avail_out;
    source_remaining += stream.avail_in as usize;
    dest_remaining += stream.avail_out as usize;

    // 6. Compute bytes consumed and produced.
    //    C: *sourceLen -= len;  →  bytes consumed = initial - remaining
    //       *destLen -= left;   →  bytes written  = initial - remaining
    let bytes_consumed = source.len() - source_remaining;
    let bytes_written = dest.len() - dest_remaining;

    // 7. Always clean up the inflate state, even on error.
    //    The result of inflate_end is intentionally discarded — the primary
    //    error (if any) is from the decompression loop above, and the state
    //    cleanup is for resource management only (Rust's Drop would also
    //    handle this, but explicit cleanup matches C zlib's contract).
    let _ = inflate::inflate_end(&mut stream);

    // 8. Map the loop result to the caller's expected return type.
    //    C: return err == Z_STREAM_END ? Z_OK :
    //           err == Z_NEED_DICT ? Z_DATA_ERROR :
    //           err == Z_BUF_ERROR && len == 0 ? Z_DATA_ERROR :
    //           err;
    //
    //    Note: `source_remaining` here is the Rust equivalent of C's `len`
    //    after the post-loop adjustment. When `source_remaining == 0`, all
    //    input was consumed but we never got StreamEnd — the stream is
    //    incomplete, so we report DataError instead of BufError.
    match result {
        Ok(ReturnCode::StreamEnd) => Ok((bytes_written, bytes_consumed)),
        Ok(ReturnCode::NeedDict) => Err(ZlibError::DataError),
        Err(ZlibError::BufError) if source_remaining == 0 => Err(ZlibError::DataError),
        Err(e) => Err(e),
        Ok(ReturnCode::Ok) => {
            // This branch should be unreachable: the loop continues while
            // inflate returns Ok, so it can only break on a different result.
            // Treat as BufError to be safe.
            Err(ZlibError::BufError)
        }
    }
}
