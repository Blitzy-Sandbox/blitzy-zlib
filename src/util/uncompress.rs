//! `uncompr.c` — one-shot in-memory decompression helpers.
//!
//! Safe-Rust port of zlib's `uncompr.c` (Copyright (C) 1995-2026 Jean-loup
//! Gailly, Mark Adler; for conditions of distribution and use, see the
//! copyright notice in `zlib.h`). This module provides the convenience
//! [`uncompress`] / [`uncompress2`] entry points that decompress a complete,
//! in-memory RFC 1950 (zlib-wrapped) stream in a single call — the mirror image
//! of the one-shot `compress` helpers.
//!
//! # Relationship to the C original
//!
//! C `uncompress2_z` drives a `z_stream` through `inflateInit` → a
//! `do { … } while (err == Z_OK)` loop → `inflateEnd`. That loop exists in C
//! only to refill the 32-bit `avail_in`/`avail_out` counters, in bounded
//! chunks, from the (potentially larger) `size_t` lengths. This port hands the
//! whole `source`/`dest` slices to [`Inflate::inflate`] at once, so the
//! chunking is unnecessary — yet the loop itself is preserved for a subtler
//! reason: it is what turns a *stalled* stream into the terminal `Z_BUF_ERROR`
//! that C's return-code mapping is written against. A single `inflate` call on
//! a truncated stream returns `Z_OK` (progress was made; the stream is merely
//! unfinished); only a follow-up call with no remaining input surfaces the
//! `Z_BUF_ERROR` that distinguishes "incomplete input" from "output buffer too
//! small". We therefore loop until `inflate` stops returning `Z_OK`, exactly as
//! C does. The loop is bounded — each iteration that continues makes strictly
//! positive progress in `source` and/or `dest` — so it cannot spin.
//!
//! # Error mapping (uncompr.c L78-81)
//!
//! The terminal code is mapped precisely as in the C source:
//!
//! * `Z_STREAM_END` → success;
//! * `Z_NEED_DICT` → [`ZlibError::DataError`] (a one-shot call has no way to
//!   supply the requested preset dictionary);
//! * `Z_BUF_ERROR` **with the entire input consumed** → [`ZlibError::DataError`]
//!   (the stream was incomplete / truncated). This is C's `len == 0` test,
//!   where `len` counts the *unused* input bytes — **not** the consumed count;
//! * any other `Z_BUF_ERROR` (input left over) → [`ZlibError::BufError`]
//!   (`dest` was too small to hold the full output);
//! * every other code is passed through unchanged.
//!
//! # FFI mapping
//!
//! All four C entry points — `uncompress`, `uncompress_z`, `uncompress2`, and
//! `uncompress2_z` — collapse onto the single [`uncompress2_to_buf`] core at
//! the `crate::ffi` boundary: the by-value vs. by-reference `sourceLen` and the
//! `uLong` vs. `z_size_t` width distinctions are C-pointer concerns the shim
//! resolves, while the safe core always reports both bytes produced and bytes
//! consumed.
//!
//! # Constraints
//!
//! * **100% safe Rust** — no `unsafe` (AAP §0.6.2).
//! * **`no_std`-clean** — the decompression path uses only [`core`] and the
//!   crate's own modules; teardown is handled by [`Inflate`]'s `Drop`
//!   (replacing `inflateEnd`), so there is nothing to free explicitly.

use crate::constants::{Z_BUF_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK, Z_STREAM_END};
use crate::error::ZlibError;
use crate::inflate::Inflate;

/// Decompress the complete zlib stream in `source` into `dest`, returning
/// `Ok((produced, consumed))`: the number of bytes written to `dest` and the
/// number of bytes read from `source`.
///
/// This is the C-semantics core shared by the idiomatic [`uncompress`] /
/// [`uncompress2`] helpers and the `crate::ffi` shims; it reproduces
/// `uncompress2_z` from `uncompr.c`.
///
/// `dest` must be large enough to hold the entire decompressed output. Its size
/// is presumed to have been recorded by the compressor and transmitted
/// out-of-band, exactly as the C contract requires.
///
/// # Errors
///
/// * [`ZlibError::DataError`] — the input is corrupt, an incomplete / truncated
///   zlib stream (empty input included), or requires a preset dictionary.
/// * [`ZlibError::BufError`] — `dest` was too small to hold the full output.
/// * Any other [`ZlibError`] propagated unchanged from the inflate engine
///   (e.g. [`ZlibError::MemError`]).
pub fn uncompress2_to_buf(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    // C `inflateInit(&stream)` — a zlib (RFC 1950) decompressor with the
    // default 32 KiB window (DEF_WBITS = 15). The handle's `Drop` is C
    // `inflateEnd`, so every return path below is leak-free with no explicit
    // teardown.
    let mut inf = Inflate::new()?;

    // C `do { … } while (err == Z_OK)`: feed the whole remaining slices each
    // iteration (no 32-bit chunking required) and accumulate the running totals.
    let mut consumed = 0usize;
    let mut produced = 0usize;
    let code = loop {
        let (ret, in_used, out_used) =
            inf.inflate(&source[consumed..], &mut dest[produced..], Z_NO_FLUSH);
        consumed += in_used;
        produced += out_used;

        if ret != Z_OK {
            break ret;
        }
        // A `Z_OK` that made no progress could not make progress on a further
        // call either, so break instead of spinning. The engine already folds a
        // no-progress call into `Z_BUF_ERROR`; this guard is defensive and also
        // bounds the loop by at most `source.len() + dest.len()` iterations.
        if in_used == 0 && out_used == 0 {
            break Z_BUF_ERROR;
        }
    };

    // uncompr.c L78-81, evaluated on the terminal `code`. `unused` is C's
    // post-loop `len` (the count of `source` bytes never consumed); `unused == 0`
    // means the whole input was consumed yet the stream did not end — i.e. it
    // was incomplete — which C reports as a data error, distinct from a `dest`
    // that simply ran out of room (input left over → `Z_BUF_ERROR` passthrough).
    let unused = source.len() - consumed;
    match code {
        Z_STREAM_END => Ok((produced, consumed)),
        Z_NEED_DICT => Err(ZlibError::DataError),
        Z_BUF_ERROR if unused == 0 => Err(ZlibError::DataError),
        other => Err(ZlibError::from_c_int(other).unwrap_or(ZlibError::DataError)),
    }
}

/// Decompress the zlib stream in `source` into `dest`, returning the number of
/// bytes written.
///
/// `dest` must be large enough to hold the entire decompressed output (its size
/// is conveyed out-of-band by the compressor — see [`uncompress2_to_buf`]).
/// This is the idiomatic counterpart of C `uncompress`; the crate re-exports it
/// as `zlib_rs::uncompress`.
///
/// # Errors
///
/// Returns [`ZlibError::DataError`] if `source` is corrupt or an incomplete
/// zlib stream, or [`ZlibError::BufError`] if `dest` is too small. See
/// [`uncompress2_to_buf`] for the full mapping.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::uncompress::uncompress;
///
/// // A zlib stream (RFC 1950) produced by canonical zlib for
/// // `b"hello, hello, hello, world!"`.
/// let packed: [u8; 23] = [
///     120, 156, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139, 114, 82, 20, 1,
///     133, 250, 9, 106,
/// ];
/// let mut out = [0u8; 27];
/// let n = uncompress(&mut out, &packed).expect("valid zlib stream");
/// assert_eq!(&out[..n], b"hello, hello, hello, world!");
/// ```
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    uncompress2_to_buf(dest, source).map(|(produced, _consumed)| produced)
}

/// Like [`uncompress`], but also reports how many input bytes were consumed,
/// returning `(produced, consumed)`.
///
/// This is useful when `source` may carry trailing data after the zlib stream:
/// `consumed` marks the first byte past the stream. It is the idiomatic
/// counterpart of C `uncompress2`; the crate re-exports it as
/// `zlib_rs::uncompress2`.
///
/// # Errors
///
/// Identical to [`uncompress`]: see [`uncompress2_to_buf`].
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    uncompress2_to_buf(dest, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical-zlib (RFC 1950, level 6) test vectors generated by the system
    // zlib oracle. `PACKED` decompresses to exactly `ORIGINAL`.
    const ORIGINAL: [u8; 177] = [
        84, 104, 101, 32, 113, 117, 105, 99, 107, 32, 98, 114, 111, 119, 110, 32, 102, 111, 120,
        32, 106, 117, 109, 112, 115, 32, 111, 118, 101, 114, 32, 116, 104, 101, 32, 108, 97, 122,
        121, 32, 100, 111, 103, 46, 32, 80, 97, 99, 107, 32, 109, 121, 32, 98, 111, 120, 32, 119,
        105, 116, 104, 32, 102, 105, 118, 101, 32, 100, 111, 122, 101, 110, 32, 108, 105, 113, 117,
        111, 114, 32, 106, 117, 103, 115, 46, 32, 72, 111, 119, 32, 114, 97, 122, 111, 114, 98, 97,
        99, 107, 45, 106, 117, 109, 112, 105, 110, 103, 32, 102, 114, 111, 103, 115, 32, 99, 97,
        110, 32, 108, 101, 118, 101, 108, 32, 115, 105, 120, 32, 112, 105, 113, 117, 101, 100, 32,
        103, 121, 109, 110, 97, 115, 116, 115, 33, 32, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 32,
        48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 32, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57,
    ];
    const PACKED: [u8; 138] = [
        120, 156, 109, 140, 75, 18, 130, 48, 16, 5, 175, 242, 60, 128, 148, 255, 207, 13, 92, 186,
        240, 2, 9, 76, 66, 20, 50, 144, 132, 0, 57, 189, 227, 222, 221, 171, 234, 238, 247, 106, 9,
        227, 228, 234, 15, 116, 224, 217, 195, 240, 130, 247, 212, 15, 17, 156, 41, 32, 9, 238, 84,
        89, 209, 176, 173, 240, 84, 226, 245, 43, 180, 72, 179, 75, 45, 140, 203, 36, 168, 144, 71,
        231, 198, 137, 131, 180, 54, 86, 120, 240, 140, 160, 10, 7, 45, 197, 246, 247, 231, 188,
        133, 9, 108, 35, 106, 37, 50, 101, 234, 16, 221, 130, 65, 50, 106, 96, 215, 222, 171, 152,
        226, 6, 187, 253, 225, 120, 58, 95, 174, 183, 251, 255, 249, 5, 4, 146, 59, 56,
    ];
    // zlib stream of empty input — decompresses to zero bytes.
    const EMPTY_PACKED: [u8; 8] = [120, 156, 3, 0, 0, 0, 0, 1];

    #[test]
    fn round_trip_into_exact_buffer() {
        let mut dest = [0u8; ORIGINAL.len()];
        let produced = uncompress(&mut dest, &PACKED).unwrap();
        assert_eq!(produced, ORIGINAL.len());
        assert_eq!(&dest[..produced], &ORIGINAL[..]);
    }

    #[test]
    fn round_trip_into_oversized_buffer() {
        let mut dest = [0u8; ORIGINAL.len() + 64];
        let produced = uncompress(&mut dest, &PACKED).unwrap();
        assert_eq!(produced, ORIGINAL.len());
        assert_eq!(&dest[..produced], &ORIGINAL[..]);
    }

    #[test]
    fn uncompress2_reports_produced_and_consumed() {
        let mut dest = [0u8; ORIGINAL.len()];
        let (produced, consumed) = uncompress2(&mut dest, &PACKED).unwrap();
        assert_eq!(produced, ORIGINAL.len());
        assert_eq!(consumed, PACKED.len());
        assert_eq!(&dest[..produced], &ORIGINAL[..]);
    }

    #[test]
    fn truncated_trailer_is_data_error() {
        // Drop the 4-byte Adler-32 trailer: the DEFLATE body is complete but the
        // checksum is missing, so the stream is incomplete (all input consumed,
        // `unused == 0`) → DataError.
        let truncated = &PACKED[..PACKED.len() - 4];
        let mut dest = [0u8; ORIGINAL.len() + 64];
        assert_eq!(uncompress(&mut dest, truncated), Err(ZlibError::DataError));
    }

    #[test]
    fn truncated_body_is_data_error() {
        // Drop 20 bytes — truncates mid-DEFLATE. Still fully consumed → DataError.
        let truncated = &PACKED[..PACKED.len() - 20];
        let mut dest = [0u8; ORIGINAL.len() + 64];
        assert_eq!(uncompress(&mut dest, truncated), Err(ZlibError::DataError));
    }

    #[test]
    fn output_too_small_is_buf_error() {
        // A 1-byte destination for 177 bytes of output: input is left unconsumed
        // (`unused > 0`) → BufError, not DataError.
        let mut dest = [0u8; 1];
        assert_eq!(uncompress(&mut dest, &PACKED), Err(ZlibError::BufError));
    }

    #[test]
    fn empty_input_is_data_error() {
        let mut dest = [0u8; 64];
        assert_eq!(uncompress(&mut dest, &[]), Err(ZlibError::DataError));
    }

    #[test]
    fn garbage_input_is_data_error() {
        let garbage = [0xFFu8; 16];
        let mut dest = [0u8; 64];
        assert_eq!(uncompress(&mut dest, &garbage), Err(ZlibError::DataError));
    }

    #[test]
    fn empty_document_round_trips() {
        // The zlib stream of empty input decompresses to zero bytes, even into a
        // zero-length destination.
        let mut dest = [0u8; 0];
        let (produced, consumed) = uncompress2(&mut dest, &EMPTY_PACKED).unwrap();
        assert_eq!(produced, 0);
        assert_eq!(consumed, EMPTY_PACKED.len());
    }

    #[test]
    fn consumed_marks_end_of_stream_with_trailing_data() {
        // Extra bytes after a complete stream must not be consumed: `consumed`
        // equals the exact stream length, leaving the trailer for the caller.
        let mut padded = [0u8; PACKED.len() + 5];
        padded[..PACKED.len()].copy_from_slice(&PACKED);
        let mut dest = [0u8; ORIGINAL.len()];
        let (produced, consumed) = uncompress2(&mut dest, &padded).unwrap();
        assert_eq!(produced, ORIGINAL.len());
        assert_eq!(consumed, PACKED.len());
        assert_eq!(&dest[..produced], &ORIGINAL[..]);
    }
}
