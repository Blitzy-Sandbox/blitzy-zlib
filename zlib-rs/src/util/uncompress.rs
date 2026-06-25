//! One-shot in-memory decompression — the safe-Rust port of C `uncompr.c`.
//!
//! This module provides the two convenience wrappers that decompress a complete
//! compressed buffer held entirely in memory, the safe-Rust equivalents of the
//! C library's `uncompress()` / `uncompress2()` entry points (`uncompr.c`). They
//! are thin drivers layered over the streaming decompressor in
//! [`crate::inflate`]: a single [`InflateState`] is allocated, fed the whole
//! source buffer, and run with [`Flush::NoFlush`] until the stream ends or an
//! error occurs.
//!
//! # Relationship to the C source
//!
//! The C code (`uncompr.c::uncompress2_z`) creates a stack `z_stream`, calls
//! `inflateInit`, then loops:
//!
//! ```text
//! do {
//!     if (avail_out == 0) { avail_out = min(left, max); left -= avail_out; }
//!     if (avail_in  == 0) { avail_in  = min(len,  max); len  -= avail_in;  }
//!     err = inflate(&stream, Z_NO_FLUSH);
//! } while (err == Z_OK);
//! ```
//!
//! The `min(.., max)` chunking exists only because the C `z_stream` caps each
//! call's `avail_in`/`avail_out` at a `uInt`. Rust slices carry their own length
//! (a `usize`), so the port simply hands the *remaining* input/output slice to
//! each [`InflateState::inflate`] call and advances cursors by the reported
//! `consumed`/`produced` counts — no chunking is required.
//!
//! The C NULL-pointer / `destLen` / `sourceLen` validation block is subsumed by
//! Rust's slice model: a `&[u8]` / `&mut [u8]` is always a valid, non-null
//! region, and the C `left == 0 && dest == NULL` "dummy output pointer" special
//! case is handled naturally by an empty `&mut []` output slice. No NULL checks
//! are reproduced.
//!
//! `inflateEnd` is realized by the automatic `Drop` of the owned
//! `Box<InflateState>` when the helper returns; no explicit teardown call is
//! needed.
//!
//! # `no_std`
//!
//! This module names only `core`/`alloc` items (the heap-owned [`InflateState`]
//! is allocated inside [`crate::inflate`]), so it participates in the crate's
//! `--no-default-features` `no_std` build, mirroring the C library's `Z_SOLO`
//! configuration.

use crate::constants::Flush;
use crate::error::{ReturnCode, ZlibError};
use crate::inflate::InflateState;

/// One-shot decompress `source` into `dest`, reporting both byte counts.
///
/// Safe-Rust port of C `uncompress2()` / `uncompress2_z()` (`uncompr.c`
/// L29-L91), which report *both* the number of output bytes produced
/// (`*destLen`) and the number of input bytes consumed (`*sourceLen`).
///
/// On success returns `Ok((produced, consumed))` where:
/// * `produced` — number of bytes written to the front of `dest` (the size of
///   the decompressed data), and
/// * `consumed` — number of bytes read from the front of `source`; the unused
///   tail begins at `source[consumed..]` (mirroring the C contract that
///   `source + *sourceLen` points at the first unused input byte).
///
/// `dest` must be large enough to hold the entire decompressed output; the size
/// of the uncompressed data must have been recorded by the compressor and
/// conveyed out of band, exactly as documented for the C function.
///
/// # Errors
///
/// Reproduces the C return mapping (`uncompr.c` L78-L81) precisely:
///
/// * [`ZlibError::DataError`] — the input is corrupt, *or* it is an incomplete
///   / truncated zlib stream (the C `Z_BUF_ERROR && len == 0` case, i.e. inflate
///   ran out of input before reaching the end of the stream), *or* the stream
///   asked for a preset dictionary (`Z_NEED_DICT`), which a one-shot caller has
///   no way to supply.
/// * [`ZlibError::BufError`] — `dest` was not large enough to hold the
///   decompressed data (the output buffer filled while input still remained).
/// * [`ZlibError::MemError`] — the decompressor state could not be allocated.
/// * Any other [`ZlibError`] propagated unchanged from the engine.
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> Result<(usize, usize), ZlibError> {
    // C `uncompress2_z` zero-inits a `z_stream` and calls `inflateInit(&stream)`.
    // The idiomatic port allocates the owning `Box<InflateState>` via
    // `InflateState::new_default` (== C `inflateInit`: default `windowBits` ==
    // `DEF_WBITS` == 15, i.e. the zlib / RFC-1950 container). `?` propagates an
    // initialization failure (C `inflateInit` returning `!= Z_OK`).
    let mut state = InflateState::new_default()?;

    // Cursors into `source` / `dest`. They play the role of the C
    // `stream.next_in`/`next_out` advance: each iteration hands the *remaining*
    // slices to `inflate`, then advances by the reported counts.
    let mut in_pos: usize = 0;
    let mut out_pos: usize = 0;

    loop {
        // The Rust `inflate` takes input/output as borrowed slices and returns
        // an `InflateResult { consumed, produced, status }` struct (its shape /
        // field order intentionally differs from the deflate side). C drives the
        // whole decompression with `Z_NO_FLUSH` — preserved here as
        // [`Flush::NoFlush`] (unlike compression, which finishes with
        // `Z_FINISH`).
        let result = state.inflate(&source[in_pos..], &mut dest[out_pos..], Flush::NoFlush);
        in_pos += result.consumed;
        out_pos += result.produced;

        match result.status {
            // C: `err == Z_STREAM_END ? Z_OK`. The entire stream decoded and its
            // trailer (Adler-32 for the zlib container) validated. Report the
            // produced/consumed byte counts.
            Ok(ReturnCode::StreamEnd) => return Ok((out_pos, in_pos)),

            // C: `err == Z_NEED_DICT ? Z_DATA_ERROR`. A one-shot helper cannot
            // supply a preset dictionary, so a dictionary request is a hard data
            // error.
            Ok(ReturnCode::NeedDict) => return Err(ZlibError::DataError),

            // C: `while (err == Z_OK)` — keep iterating while inflate makes
            // forward progress. A `Z_OK` with neither input consumed nor output
            // produced means we are stuck; classify it like the `Z_BUF_ERROR`
            // arm below. (The engine already converts a no-progress `Z_OK` into
            // `Z_BUF_ERROR`, so this guard is a defensive equivalent that also
            // guarantees loop termination.)
            Ok(_) => {
                if result.consumed == 0 && result.produced == 0 {
                    return Err(buf_or_data_error(in_pos, source.len()));
                }
                // Forward progress was made: completing this `match` (the loop
                // body) re-enters the loop. No explicit `continue` is used, so
                // `clippy::needless_continue` stays satisfied.
            }

            // C: `err == Z_BUF_ERROR && len == 0 ? Z_DATA_ERROR : err`.
            // `inflate` returns `Z_BUF_ERROR` when no progress is possible. If
            // *all* input has been consumed (C `len == 0`, i.e.
            // `in_pos == source.len()` here) the stream is incomplete / truncated
            // — a data error; otherwise the caller's `dest` is simply full — a
            // genuine buffer error.
            Err(ZlibError::BufError) => return Err(buf_or_data_error(in_pos, source.len())),

            // C: the trailing `: err`. Any other failure (corrupt data,
            // out-of-memory, stream/parameter error, …) is returned verbatim.
            Err(e) => return Err(e),
        }
    }
}

/// Classify a `Z_BUF_ERROR` exactly as C `uncompress2_z` does (`uncompr.c`
/// L80): when *all* input has been consumed the stream is truncated / incomplete
/// and cannot finish, which becomes [`ZlibError::DataError`]; otherwise the
/// output buffer is merely full and the error stays [`ZlibError::BufError`].
#[inline]
fn buf_or_data_error(consumed: usize, source_len: usize) -> ZlibError {
    // C `len == 0` ⇔ "every source byte was handed to and consumed by inflate".
    if consumed == source_len {
        ZlibError::DataError
    } else {
        ZlibError::BufError
    }
}

/// One-shot decompress `source` into `dest`, returning the produced byte count.
///
/// Safe-Rust port of C `uncompress()` / `uncompress_z()` (`uncompr.c`
/// L92-L101), which take the source length *by value* and report only
/// `*destLen`. This wrapper delegates to [`uncompress2`] and discards the
/// consumed count.
///
/// On success returns `Ok(produced)` — the number of bytes written to the front
/// of `dest` (the size of the decompressed data). `dest` must be large enough to
/// hold the entire decompressed output.
///
/// # Errors
///
/// Identical to [`uncompress2`]: [`ZlibError::DataError`] for corrupt or
/// truncated input (and for an unsatisfiable `Z_NEED_DICT`),
/// [`ZlibError::BufError`] when `dest` is too small, [`ZlibError::MemError`] on
/// allocation failure, or any other engine error propagated unchanged.
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Result<usize, ZlibError> {
    uncompress2(dest, source).map(|(produced, _consumed)| produced)
}

// ===========================================================================
// Tests
// ===========================================================================
//
// The byte vectors below are *canonical* zlib streams produced by the reference
// C zlib library (generated with Python's `zlib` module, which links that same
// library). They therefore double as interop fixtures: the safe-Rust
// decompressor must accept any valid zlib stream byte-for-byte. The sibling
// `crate::util::compress` module is deliberately NOT relied upon here so this
// file can be authored and validated independently of it.
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Round-trip plaintext payload (deterministic ASCII, 64 bytes).
    const RT_PAYLOAD: &[u8] = &[
        0x7a, 0x6c, 0x69, 0x62, 0x2d, 0x72, 0x73, 0x20, 0x75, 0x6e, 0x63, 0x6f, 0x6d, 0x70, 0x72,
        0x65, 0x73, 0x73, 0x28, 0x29, 0x20, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x2d, 0x74, 0x72, 0x69,
        0x70, 0x3a, 0x20, 0x74, 0x68, 0x65, 0x20, 0x71, 0x75, 0x69, 0x63, 0x6b, 0x20, 0x62, 0x72,
        0x6f, 0x77, 0x6e, 0x20, 0x66, 0x6f, 0x78, 0x20, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x21,
    ];

    /// Canonical zlib stream of `RT_PAYLOAD` (`zlib.compress(_, 6)`): an RFC-1950
    /// container — 2-byte header, one final fixed-Huffman block, 4-byte Adler-32
    /// trailer (72 bytes total).
    const RT_STREAM: &[u8] = &[
        0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x56, 0x28, 0xcd, 0x4b, 0xce, 0xcf,
        0x2d, 0x28, 0x4a, 0x2d, 0x2e, 0xd6, 0xd0, 0x54, 0x28, 0xca, 0x2f, 0xcd, 0x4b, 0xd1, 0x2d,
        0x29, 0xca, 0x2c, 0xb0, 0x52, 0x28, 0xc9, 0x48, 0x55, 0x28, 0x2c, 0xcd, 0x4c, 0xce, 0x56,
        0x48, 0x2a, 0xca, 0x2f, 0xcf, 0x53, 0x48, 0xcb, 0xaf, 0x50, 0x30, 0x30, 0x34, 0x32, 0x36,
        0x31, 0x35, 0x33, 0xb7, 0xb0, 0x54, 0x04, 0x00, 0xf7, 0x26, 0x15, 0x93,
    ];

    /// A deliberately TRUNCATED copy of `RT_STREAM` (last 6 bytes removed, 66
    /// bytes): the DEFLATE data and the Adler-32 trailer are incomplete, so
    /// inflate exhausts all input before reaching stream-end — exercising the C
    /// `Z_BUF_ERROR && len == 0` → `Z_DATA_ERROR` path.
    const TRUNCATED_STREAM: &[u8] = &[
        0x78, 0x9c, 0xab, 0xca, 0xc9, 0x4c, 0xd2, 0x2d, 0x2a, 0x56, 0x28, 0xcd, 0x4b, 0xce, 0xcf,
        0x2d, 0x28, 0x4a, 0x2d, 0x2e, 0xd6, 0xd0, 0x54, 0x28, 0xca, 0x2f, 0xcd, 0x4b, 0xd1, 0x2d,
        0x29, 0xca, 0x2c, 0xb0, 0x52, 0x28, 0xc9, 0x48, 0x55, 0x28, 0x2c, 0xcd, 0x4c, 0xce, 0x56,
        0x48, 0x2a, 0xca, 0x2f, 0xcf, 0x53, 0x48, 0xcb, 0xaf, 0x50, 0x30, 0x30, 0x34, 0x32, 0x36,
        0x31, 0x35, 0x33, 0xb7, 0xb0, 0x54,
    ];

    /// Canonical zlib stream whose plaintext is [`INCOMPRESSIBLE_LEN`] bytes of
    /// incompressible (deterministic LCG) data; zlib stored it almost verbatim
    /// (one final *stored* block, 211 bytes total). A tiny output buffer fills
    /// while input still remains — exercising the C `Z_BUF_ERROR && len != 0` →
    /// `Z_BUF_ERROR` path.
    const INCOMPRESSIBLE_STREAM: &[u8] = &[
        0x78, 0x9c, 0x01, 0xc8, 0x00, 0x37, 0xff, 0x71, 0x47, 0x1d, 0x94, 0xec, 0x89, 0x93, 0xc7,
        0x44, 0xbc, 0xd8, 0xcf, 0xcb, 0x3c, 0xc5, 0xa6, 0x68, 0x19, 0xa8, 0xe6, 0xca, 0xa4, 0xe2,
        0x3b, 0x69, 0xbd, 0x41, 0x89, 0x41, 0xda, 0x1e, 0xdc, 0x4e, 0xd8, 0x36, 0x13, 0xc6, 0x82,
        0x49, 0x4c, 0x19, 0xe2, 0x74, 0x0e, 0xa9, 0x4a, 0x4f, 0x39, 0x49, 0x20, 0xc6, 0xae, 0x77,
        0x6d, 0xb8, 0xbe, 0x59, 0x25, 0x51, 0x54, 0x7a, 0x34, 0x28, 0xe1, 0x41, 0x4d, 0x18, 0x0a,
        0x35, 0x71, 0xde, 0x14, 0xf2, 0x41, 0x77, 0x0b, 0xea, 0x05, 0x37, 0xb5, 0xdd, 0x78, 0xa9,
        0x3a, 0x16, 0x59, 0x2a, 0x90, 0x6b, 0xb2, 0x44, 0xa8, 0xf2, 0xe6, 0xcb, 0x5a, 0x83, 0x7e,
        0xba, 0x11, 0xf2, 0xb0, 0xcb, 0x36, 0x09, 0xb3, 0xd8, 0x5e, 0x48, 0xc5, 0xf4, 0x32, 0x5c,
        0xf8, 0x48, 0x22, 0x5f, 0xc0, 0xaf, 0xc9, 0xd5, 0x3e, 0x11, 0x1e, 0x62, 0x4a, 0x80, 0x60,
        0x4d, 0x43, 0x14, 0xc0, 0xb5, 0x96, 0x87, 0xcb, 0x95, 0x0f, 0x8f, 0x9e, 0x79, 0xe2, 0xff,
        0xc8, 0xfd, 0x78, 0x9c, 0xfd, 0x0a, 0xfb, 0xc0, 0x80, 0xd0, 0xa0, 0xb1, 0x4e, 0x83, 0xb7,
        0xbe, 0x0b, 0xd5, 0x74, 0x1f, 0xae, 0x36, 0x7b, 0x8a, 0xeb, 0xce, 0x2d, 0x95, 0x63, 0x36,
        0xb5, 0xcf, 0x8e, 0xfa, 0xd1, 0x9c, 0x64, 0xcf, 0x60, 0xd4, 0xce, 0x95, 0xb0, 0x1b, 0xd0,
        0x0b, 0x85, 0xfe, 0x73, 0x54, 0xe9, 0xd2, 0x73, 0x2d, 0xb7, 0x4d, 0xad, 0xd8, 0x33, 0x66,
        0x99,
    ];

    /// Decompressed length of [`INCOMPRESSIBLE_STREAM`].
    const INCOMPRESSIBLE_LEN: usize = 200;

    /// A valid zlib stream decompresses back to the exact original payload, and
    /// [`uncompress`] reports the produced length. (Mirrors the C `example.c`
    /// `test_compress` round-trip assertion.)
    #[test]
    fn uncompress_round_trip_recovers_payload() {
        let mut dest: Vec<u8> = vec![0u8; RT_PAYLOAD.len()];
        let produced = uncompress(&mut dest, RT_STREAM).expect("a valid zlib stream must decode");
        assert_eq!(produced, RT_PAYLOAD.len());
        assert_eq!(&dest[..produced], RT_PAYLOAD);
    }

    /// [`uncompress2`] reports BOTH the produced byte count and the number of
    /// source bytes consumed; for a complete stream every byte (header + data +
    /// trailer) is consumed.
    #[test]
    fn uncompress2_reports_produced_and_consumed() {
        let mut dest: Vec<u8> = vec![0u8; RT_PAYLOAD.len()];
        let (produced, consumed) =
            uncompress2(&mut dest, RT_STREAM).expect("a valid zlib stream must decode");
        assert_eq!(produced, RT_PAYLOAD.len());
        assert_eq!(consumed, RT_STREAM.len());
        assert_eq!(&dest[..produced], RT_PAYLOAD);
    }

    /// A truncated (incomplete) stream is a data error, not a buffer error: with
    /// ample output space the only reason inflate can stop is input exhaustion,
    /// which the C code maps `Z_BUF_ERROR && len == 0` → `Z_DATA_ERROR`.
    #[test]
    fn uncompress_truncated_stream_is_data_error() {
        // Generously oversized so the output buffer never limits progress.
        let mut dest: Vec<u8> = vec![0u8; RT_PAYLOAD.len() * 4];
        let err = uncompress(&mut dest, TRUNCATED_STREAM).unwrap_err();
        assert_eq!(err, ZlibError::DataError);
    }

    /// An output buffer that is too small for the decompressed data yields a
    /// buffer error (input still remains when `dest` fills): the C
    /// `Z_BUF_ERROR && len != 0` → `Z_BUF_ERROR` path.
    #[test]
    fn uncompress_output_too_small_is_buf_error() {
        let mut dest: Vec<u8> = vec![0u8; 8];
        assert!(
            dest.len() < INCOMPRESSIBLE_LEN,
            "dest must be smaller than the output"
        );
        let err = uncompress(&mut dest, INCOMPRESSIBLE_STREAM).unwrap_err();
        assert_eq!(err, ZlibError::BufError);
    }

    /// `uncompress2` agrees with `uncompress` on the too-small-output case and
    /// reports a non-zero amount of partial output before failing.
    #[test]
    fn uncompress2_output_too_small_is_buf_error() {
        let mut dest: Vec<u8> = vec![0u8; 8];
        let err = uncompress2(&mut dest, INCOMPRESSIBLE_STREAM).unwrap_err();
        assert_eq!(err, ZlibError::BufError);
    }

    /// A corrupted zlib header (failing the `(CMF*256+FLG) % 31 == 0` check and
    /// the deflate compression-method check) is rejected as a data error before
    /// any output is produced.
    #[test]
    fn uncompress_corrupt_header_is_data_error() {
        let mut bad: Vec<u8> = RT_STREAM.to_vec();
        bad[0] ^= 0xFF;
        let mut dest: Vec<u8> = vec![0u8; RT_PAYLOAD.len()];
        let err = uncompress(&mut dest, &bad).unwrap_err();
        assert_eq!(err, ZlibError::DataError);
    }

    /// Decompressing a valid, non-empty stream into a zero-length output buffer
    /// is a buffer error (input remains), never a (truncated-stream) data error.
    #[test]
    fn uncompress_into_empty_output_is_buf_error() {
        let mut dest: [u8; 0] = [];
        let err = uncompress(&mut dest, RT_STREAM).unwrap_err();
        assert_eq!(err, ZlibError::BufError);
    }
}
