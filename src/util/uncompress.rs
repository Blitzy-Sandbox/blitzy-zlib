//! One-call, buffer-to-buffer decompression helpers — the safe-Rust port of C
//! `uncompr.c` (AAP §0.4.1, source `uncompr.c`).
//!
//! This module provides the two convenience wrappers that decompress an entire
//! zlib stream held in memory into a caller-supplied output buffer in a single
//! call, without the caller having to drive the streaming [`crate::inflate`]
//! engine directly:
//!
//! * [`uncompress2`] — the primary entry point. It reports how many source
//!   bytes were consumed through an out-parameter, mirroring C
//!   `uncompress2`/`uncompress2_z` (`uncompr.c` L29-L90).
//! * [`uncompress`](uncompress()) — the classic convenience form that ignores the
//!   consumed-length report, mirroring C `uncompress` (`uncompr.c` L92-L101).
//!
//! # Relationship to the C original
//!
//! C `uncompr.c` exposes four symbols: the size-generic `uncompress2_z`
//! (the real implementation), the `uLong`-typed `uncompress2`, and the
//! `sourceLen`-by-value shims `uncompress_z` / `uncompress`. In safe Rust a
//! slice already carries its own length, so the `size_t`-vs-`uLong` distinction
//! collapses: there is a single size-generic implementation ([`uncompress2`])
//! and a single convenience wrapper ([`uncompress`](uncompress())). The raw-pointer,
//! `z_size_t`/`uLong` FFI shims that reproduce the exact C ABI live in
//! `src/ffi/util.rs` and are **not** part of this module.
//!
//! # Safety and portability
//!
//! * **Zero `unsafe`.** Everything here operates over safe slices; the pointer
//!   validation and the `next_out`-cannot-be-`NULL` scratch trick from the C
//!   original (`uncompr.c` L42-L43) are FFI-boundary concerns and are
//!   unnecessary in the slice-based API — an empty `&mut []` output is already a
//!   valid, non-null buffer.
//! * **`no_std` friendly.** The module depends only on `core` (through the
//!   crate's shared types) and never references `std`.
//!
//! # Byte-exact parity
//!
//! Decompressed output is byte-identical to reference zlib for the same input
//! (AAP §0.6.4 / §0.7.1); the subtle return-code mapping of `uncompr.c`
//! (L78-L81) — folding `Z_NEED_DICT` and "all input consumed but stream not
//! finished" into `Z_DATA_ERROR` — is reproduced exactly in [`uncompress2`].

use crate::constants::Z_NO_FLUSH;
use crate::error::ReturnCode;
use crate::inflate::{inflate, inflate_end, inflate_init};
use crate::stream::ZStream;

/// Decompresses the whole zlib stream in `source` into `dest`, reporting the
/// number of source bytes consumed.
///
/// This is the idiomatic port of C `uncompress2` / `uncompress2_z`
/// (`uncompr.c` L29-L90): it initializes a fresh [`crate::inflate`] engine with
/// the default `windowBits` (15, i.e. C `inflateInit`), feeds the entire input
/// through the engine, and tears the engine down before returning.
///
/// # Parameters
///
/// * `dest` — the output buffer. It must be large enough to hold the *entire*
///   decompressed payload; its length is the available output capacity. (As
///   with C zlib, the uncompressed size must have been recorded by the
///   compressor and communicated out of band.)
/// * `source` — the compressed input.
/// * `source_len` — an **in/out** parameter:
///   * **On entry** it caps how many bytes of `source` may be read. The number
///     of input bytes actually made available to the engine is
///     `source.len().min(*source_len)`, so passing `source.len()` (or any larger
///     value) simply means "use the whole slice". This mirrors the C contract
///     where `*sourceLen` is the length of the `source` buffer.
///   * **On exit** it is overwritten with the number of source bytes actually
///     consumed (C `*sourceLen -= len`). After a successful call,
///     `source[*source_len..]` is the first unused input byte onward — exactly
///     C's "`source + *sourceLen` points to the first unused input byte".
/// * `dest_len` — an **out** parameter written with the number of output bytes
///   actually produced (C `*destLen -= left`, i.e. `total_out`). Reproducing C
///   `uncompress2_z`, this is written on **every** path that reaches the decode
///   loop — success **and** failure — so a caller observing `Z_BUF_ERROR` (or
///   any error) still learns how many bytes were written before the failure.
///
/// Both out-parameters are written on every path that reaches the decode loop
/// (success *and* failure); they are left untouched only if engine
/// initialization itself fails, matching C's early `return err;` after a failed
/// `inflateInit`.
///
/// # Returns
///
/// * `Ok(produced)` — the stream was fully decompressed; `produced` bytes were
///   written to the front of `dest`.
/// * `Err(code)` — decompression failed; `code` follows the exact `uncompr.c`
///   L78-L81 mapping (see *Errors*).
///
/// # Errors
///
/// Reproduces `uncompr.c` L78-L81 verbatim:
///
/// * [`ReturnCode::DataError`] — the input was corrupt, **or** it was an
///   incomplete / truncated zlib stream (all input was consumed yet the stream
///   never reached its end), **or** a preset dictionary was required
///   (`Z_NEED_DICT`, which the one-call path cannot supply and therefore treats
///   as a data error).
/// * [`ReturnCode::BufError`] — `dest` was too small to hold the decompressed
///   data while input still remained to be read.
/// * [`ReturnCode::MemError`] — the engine could not allocate its working state.
/// * Any other code surfaced unchanged by the underlying engine.
///
/// # Examples
///
/// ```ignore
/// // `zlib` is a valid zlib stream and `plain_len` its decompressed size.
/// let mut out = vec![0u8; plain_len];
/// let mut consumed = zlib.len();
/// let mut produced = out.len();
/// let n = uncompress2(&mut out, &zlib, &mut consumed, &mut produced)?;
/// assert_eq!(n, plain_len);
/// assert_eq!(produced, plain_len);   // output count reported on all paths
/// assert_eq!(consumed, zlib.len());  // whole stream read
/// ```
pub fn uncompress2(
    dest: &mut [u8],
    source: &[u8],
    source_len: &mut usize,
    dest_len: &mut usize,
) -> Result<usize, ReturnCode> {
    // Amount of input the engine is allowed to see. Honor the caller-declared
    // length (C `*sourceLen`) but never read past the end of the slice.
    let available = source.len().min(*source_len);

    // C `inflateInit(&stream)` with the default window size. `ZStream::new`
    // installs the default allocator; `inflate_init` defaults `windowBits` to
    // `DEF_WBITS` (15). On failure we return immediately WITHOUT touching
    // `*source_len`, matching C `uncompress2_z` which does `return err;` before
    // it computes the consumed/produced accounting.
    let mut strm = ZStream::new();
    if let Err(err) = inflate_init(&mut strm) {
        return Err(err.as_return_code());
    }

    // Drive the engine. Because [`ZStream`] carries no `next_in`/`next_out`
    // cursor fields and the driver consumes/produces whole slices per call, the
    // C `uInt`-sized chunking loop (`avail_in`/`avail_out` capped at `(uInt)-1`)
    // collapses to: hand the engine the *remaining* input and output, add the
    // reported progress to our running cursors, and repeat while the engine
    // keeps returning [`ReturnCode::Ok`] — exactly C's
    // `do { … } while (err == Z_OK);` (`uncompr.c` L57-L67).
    let mut in_pos: usize = 0; // total input consumed  (C: original sourceLen - len)
    let mut out_pos: usize = 0; // total output produced (C: original destLen  - left)
    let code = loop {
        let outcome = inflate(
            &mut strm,
            &source[in_pos..available],
            &mut dest[out_pos..],
            Z_NO_FLUSH,
        );
        in_pos += outcome.consumed;
        out_pos += outcome.produced;

        // Any non-`Ok` code terminates the loop (C's `while (err == Z_OK)`).
        if outcome.code != ReturnCode::Ok {
            break outcome.code;
        }

        // Defensive termination: a conforming engine never reports `Ok` while
        // making no progress — it returns `BufError` when it is stalled for
        // want of input or output. Guard against a hang regardless, folding a
        // stall into the same `BufError` C would observe on its next iteration
        // (an empty `avail_in`/`avail_out` refill yields `Z_BUF_ERROR`).
        if outcome.consumed == 0 && outcome.produced == 0 {
            break ReturnCode::BufError;
        }
    };

    // C `inflateEnd(&stream)`. Discarding the result is intentional: our return
    // value has already been decided, and dropping `strm` afterwards is safe
    // because the engine state has been released (RAII `Drop` then sees an
    // empty stream — no double free).
    let _ = inflate_end(&mut strm);

    // C accounting (`uncompr.c` L69-L74): after the loop, `len` is the number of
    // input bytes NOT consumed and `*sourceLen` becomes the number that WAS.
    // Both counts are published on every post-init path (success AND failure),
    // exactly as C `uncompress2_z` computes `*sourceLen -= len` / `*destLen -=
    // left` before `return err;`, so a caller that hit `Z_BUF_ERROR` still sees
    // how many output bytes were produced before the buffer ran out.
    let leftover_in = available - in_pos;
    *source_len = in_pos;
    *dest_len = out_pos;

    // Return mapping — reproduces `uncompr.c` L78-L81 exactly:
    //   Z_STREAM_END                              -> Ok(produced)
    //   Z_NEED_DICT                               -> Z_DATA_ERROR
    //   Z_BUF_ERROR with every input byte consumed-> Z_DATA_ERROR (truncated)
    //   otherwise                                 -> the engine's code unchanged
    // A `Z_BUF_ERROR` with input still pending (`leftover_in > 0`) means the
    // output buffer was too small, and falls through to `Err(BufError)`.
    match code {
        ReturnCode::StreamEnd => Ok(out_pos),
        ReturnCode::NeedDict => Err(ReturnCode::DataError),
        ReturnCode::BufError if leftover_in == 0 => Err(ReturnCode::DataError),
        other => Err(other),
    }
}

/// Decompresses the whole zlib stream in `source` into `dest`.
///
/// This is the classic convenience wrapper — the port of C `uncompress`
/// (`uncompr.c` L92-L101) — for callers that do not care how many input bytes
/// were consumed. It treats the *entire* `source` slice as the available input
/// and forwards to [`uncompress2`], discarding the consumed-length report.
///
/// # Returns
///
/// * `Ok(produced)` — `produced` bytes were written to the front of `dest`.
/// * `Err(code)` — see [`uncompress2`] for the exact error mapping.
///
/// # Errors
///
/// Identical to [`uncompress2`]: [`ReturnCode::DataError`] for corrupt or
/// incomplete input (or a needed preset dictionary), [`ReturnCode::BufError`]
/// when `dest` is too small, and [`ReturnCode::MemError`] on allocation failure.
///
/// # Examples
///
/// ```ignore
/// let mut out = vec![0u8; plain_len];
/// let n = uncompress(&mut out, &zlib)?;
/// assert_eq!(n, plain_len);
/// ```
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Result<usize, ReturnCode> {
    // C `uncompress` seeds a local `used = sourceLen` and calls `uncompress2`,
    // then throws the updated `used` away. The produced-count out-parameter is
    // likewise discarded here (the `Ok(produced)` return still carries it).
    let mut used = source.len();
    let mut produced = dest.len();
    uncompress2(dest, source, &mut used, &mut produced)
}

#[cfg(test)]
mod tests {
    use super::{uncompress, uncompress2};
    use crate::error::ReturnCode;

    // -----------------------------------------------------------------------
    // Deterministic zlib streams produced by reference zlib (level 6). Using
    // fixed vectors keeps the tests self-contained — they neither depend on the
    // sibling `compress` module nor on any external crate — while still
    // exercising the real `crate::inflate` engine end-to-end.
    // -----------------------------------------------------------------------

    /// `zlib.compress(b"hello, world")`.
    const HELLO_PLAIN: &[u8] = b"hello, world";
    const HELLO_ZLIB: [u8; 20] = [
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01,
        0x00, 0x1d, 0x54, 0x04, 0x89,
    ];

    /// `zlib.compress(b"A" * 300)` — highly compressible.
    const BIG_ZLIB: [u8; 13] = [
        0x78, 0x9c, 0x73, 0x74, 0x1c, 0x05, 0xc4, 0x02, 0x00, 0xcb, 0x9e, 0x4c, 0x2d,
    ];
    const BIG_PLAIN_LEN: usize = 300;

    /// `zlib.compress(b"")` — the empty payload (8-byte stream).
    const EMPTY_ZLIB: [u8; 8] = [0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// `zlib.compress(bytes(0..64))`.
    const MIXED_ZLIB: [u8; 72] = [
        0x78, 0x9c, 0x63, 0x60, 0x64, 0x62, 0x66, 0x61, 0x65, 0x63, 0xe7, 0xe0, 0xe4, 0xe2, 0xe6,
        0xe1, 0xe5, 0xe3, 0x17, 0x10, 0x14, 0x12, 0x16, 0x11, 0x15, 0x13, 0x97, 0x90, 0x94, 0x92,
        0x96, 0x91, 0x95, 0x93, 0x57, 0x50, 0x54, 0x52, 0x56, 0x51, 0x55, 0x53, 0xd7, 0xd0, 0xd4,
        0xd2, 0xd6, 0xd1, 0xd5, 0xd3, 0x37, 0x30, 0x34, 0x32, 0x36, 0x31, 0x35, 0x33, 0xb7, 0xb0,
        0xb4, 0xb2, 0xb6, 0xb1, 0xb5, 0xb3, 0x07, 0x00, 0xaa, 0xe0, 0x07, 0xe1,
    ];

    /// `zlib.compress(<256 pseudo-random bytes>)` — incompressible, so the
    /// compressed form is *larger* than a small output buffer and filling that
    /// buffer leaves input unconsumed.
    const INCOMP_ZLIB: [u8; 267] = [
        0x78, 0x9c, 0x01, 0x00, 0x01, 0xff, 0xfe, 0xe1, 0x3b, 0x03, 0x2e, 0x11, 0x2a, 0x32, 0xb5,
        0x79, 0x08, 0x0f, 0x08, 0xb1, 0xf7, 0xed, 0x4c, 0x2e, 0x5d, 0x3a, 0x07, 0xf9, 0x7f, 0x21,
        0xee, 0x23, 0x2d, 0x17, 0x8a, 0x20, 0x9a, 0xf6, 0xb5, 0x88, 0x7f, 0x66, 0xe8, 0x09, 0x24,
        0x02, 0xaa, 0x49, 0xf2, 0xc1, 0x55, 0x1b, 0x27, 0xfe, 0x53, 0x26, 0x6e, 0x49, 0x0d, 0xb1,
        0x38, 0x48, 0x9c, 0xe8, 0x14, 0xd5, 0x8d, 0x14, 0x5a, 0x8b, 0x4f, 0x99, 0x4f, 0xed, 0x15,
        0xc5, 0xb2, 0xfd, 0xae, 0xef, 0xf3, 0x17, 0xf1, 0x57, 0xe1, 0xe0, 0x97, 0x8c, 0x3f, 0x5f,
        0xd5, 0xdf, 0x3d, 0x34, 0xf8, 0xc0, 0x82, 0x62, 0xb0, 0x37, 0x50, 0x89, 0x4f, 0xa5, 0xe4,
        0x24, 0x28, 0xca, 0x6d, 0x18, 0x92, 0x13, 0x70, 0x2c, 0xa2, 0x9c, 0xeb, 0x21, 0x83, 0x25,
        0xda, 0x67, 0x33, 0xcb, 0x63, 0xeb, 0x78, 0xb8, 0x69, 0xd7, 0x59, 0x68, 0x9a, 0x1e, 0xb4,
        0x4e, 0xff, 0xf1, 0xaa, 0x47, 0x43, 0x18, 0x54, 0x4a, 0x23, 0xa6, 0x57, 0x00, 0x1f, 0x2c,
        0x4b, 0x6f, 0x14, 0xdd, 0xc8, 0xa6, 0x6a, 0xc3, 0x8f, 0x9b, 0xd8, 0xa3, 0x4d, 0x2f, 0x85,
        0x8e, 0xd2, 0xcc, 0x8d, 0x3a, 0xc0, 0x8c, 0x6d, 0x98, 0xcb, 0x1a, 0xb2, 0xe1, 0x77, 0xfb,
        0x54, 0xc2, 0x9d, 0x01, 0x25, 0xf5, 0xca, 0x98, 0xdb, 0xf5, 0x5f, 0xcd, 0xf4, 0x50, 0x90,
        0xbd, 0xb1, 0x69, 0x56, 0xea, 0xf2, 0x0e, 0xef, 0x35, 0x0d, 0xbb, 0xf3, 0x21, 0x47, 0xa9,
        0xb2, 0x94, 0x98, 0xa9, 0x96, 0x63, 0x8e, 0x25, 0x68, 0xad, 0xab, 0xa4, 0xea, 0x88, 0x2b,
        0x3d, 0x7d, 0x83, 0xbe, 0x46, 0x0e, 0xca, 0x13, 0x16, 0x6a, 0x4f, 0xa0, 0xb5, 0xde, 0x23,
        0x9c, 0x85, 0xf8, 0x70, 0xb2, 0x2a, 0x09, 0xa9, 0x75, 0x53, 0xf4, 0xff, 0x47, 0x22, 0x4a,
        0x7c, 0x54, 0xc9, 0xa7, 0x42, 0xe4, 0x14, 0xbe, 0xeb, 0xaa, 0x7e, 0x17,
    ];
    const INCOMP_PLAIN: [u8; 256] = [
        0xe1, 0x3b, 0x03, 0x2e, 0x11, 0x2a, 0x32, 0xb5, 0x79, 0x08, 0x0f, 0x08, 0xb1, 0xf7, 0xed,
        0x4c, 0x2e, 0x5d, 0x3a, 0x07, 0xf9, 0x7f, 0x21, 0xee, 0x23, 0x2d, 0x17, 0x8a, 0x20, 0x9a,
        0xf6, 0xb5, 0x88, 0x7f, 0x66, 0xe8, 0x09, 0x24, 0x02, 0xaa, 0x49, 0xf2, 0xc1, 0x55, 0x1b,
        0x27, 0xfe, 0x53, 0x26, 0x6e, 0x49, 0x0d, 0xb1, 0x38, 0x48, 0x9c, 0xe8, 0x14, 0xd5, 0x8d,
        0x14, 0x5a, 0x8b, 0x4f, 0x99, 0x4f, 0xed, 0x15, 0xc5, 0xb2, 0xfd, 0xae, 0xef, 0xf3, 0x17,
        0xf1, 0x57, 0xe1, 0xe0, 0x97, 0x8c, 0x3f, 0x5f, 0xd5, 0xdf, 0x3d, 0x34, 0xf8, 0xc0, 0x82,
        0x62, 0xb0, 0x37, 0x50, 0x89, 0x4f, 0xa5, 0xe4, 0x24, 0x28, 0xca, 0x6d, 0x18, 0x92, 0x13,
        0x70, 0x2c, 0xa2, 0x9c, 0xeb, 0x21, 0x83, 0x25, 0xda, 0x67, 0x33, 0xcb, 0x63, 0xeb, 0x78,
        0xb8, 0x69, 0xd7, 0x59, 0x68, 0x9a, 0x1e, 0xb4, 0x4e, 0xff, 0xf1, 0xaa, 0x47, 0x43, 0x18,
        0x54, 0x4a, 0x23, 0xa6, 0x57, 0x00, 0x1f, 0x2c, 0x4b, 0x6f, 0x14, 0xdd, 0xc8, 0xa6, 0x6a,
        0xc3, 0x8f, 0x9b, 0xd8, 0xa3, 0x4d, 0x2f, 0x85, 0x8e, 0xd2, 0xcc, 0x8d, 0x3a, 0xc0, 0x8c,
        0x6d, 0x98, 0xcb, 0x1a, 0xb2, 0xe1, 0x77, 0xfb, 0x54, 0xc2, 0x9d, 0x01, 0x25, 0xf5, 0xca,
        0x98, 0xdb, 0xf5, 0x5f, 0xcd, 0xf4, 0x50, 0x90, 0xbd, 0xb1, 0x69, 0x56, 0xea, 0xf2, 0x0e,
        0xef, 0x35, 0x0d, 0xbb, 0xf3, 0x21, 0x47, 0xa9, 0xb2, 0x94, 0x98, 0xa9, 0x96, 0x63, 0x8e,
        0x25, 0x68, 0xad, 0xab, 0xa4, 0xea, 0x88, 0x2b, 0x3d, 0x7d, 0x83, 0xbe, 0x46, 0x0e, 0xca,
        0x13, 0x16, 0x6a, 0x4f, 0xa0, 0xb5, 0xde, 0x23, 0x9c, 0x85, 0xf8, 0x70, 0xb2, 0x2a, 0x09,
        0xa9, 0x75, 0x53, 0xf4, 0xff, 0x47, 0x22, 0x4a, 0x7c, 0x54, 0xc9, 0xa7, 0x42, 0xe4, 0x14,
        0xbe,
    ];

    #[test]
    fn roundtrip_small() {
        let mut out = [0u8; 32];
        let n = uncompress(&mut out, &HELLO_ZLIB).expect("valid stream decompresses");
        assert_eq!(n, HELLO_PLAIN.len());
        assert_eq!(&out[..n], HELLO_PLAIN);
    }

    #[test]
    fn roundtrip_empty() {
        // An empty payload decodes to zero bytes.
        let mut out = [0u8; 8];
        let n = uncompress(&mut out, &EMPTY_ZLIB).expect("empty stream decompresses");
        assert_eq!(n, 0);
    }

    #[test]
    fn roundtrip_empty_into_zero_length_dest() {
        // The C `next_out == NULL` scratch trick is unnecessary in safe Rust:
        // an empty `&mut []` output is a valid, non-null buffer, and an empty
        // payload needs no output space at all.
        let mut out: [u8; 0] = [];
        let n = uncompress(&mut out, &EMPTY_ZLIB).expect("empty stream, empty dest");
        assert_eq!(n, 0);
    }

    #[test]
    fn roundtrip_highly_compressible() {
        let mut out = [0u8; BIG_PLAIN_LEN];
        let n = uncompress(&mut out, &BIG_ZLIB).expect("valid stream decompresses");
        assert_eq!(n, BIG_PLAIN_LEN);
        assert!(out.iter().all(|&b| b == b'A'), "all bytes are 'A'");
    }

    #[test]
    fn roundtrip_mixed() {
        let mut expected = [0u8; 64];
        for (i, b) in expected.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut out = [0u8; 64];
        let n = uncompress(&mut out, &MIXED_ZLIB).expect("valid stream decompresses");
        assert_eq!(n, expected.len());
        assert_eq!(&out[..n], &expected[..]);
    }

    #[test]
    fn roundtrip_incompressible() {
        let mut out = [0u8; 256];
        let n = uncompress(&mut out, &INCOMP_ZLIB).expect("valid stream decompresses");
        assert_eq!(n, INCOMP_PLAIN.len());
        assert_eq!(&out[..n], &INCOMP_PLAIN[..]);
    }

    #[test]
    fn uncompress2_reports_consumed_length() {
        let mut out = [0u8; 32];
        let mut consumed = HELLO_ZLIB.len();
        let mut produced = out.len();
        let n = uncompress2(&mut out, &HELLO_ZLIB, &mut consumed, &mut produced).expect("ok");
        assert_eq!(n, HELLO_PLAIN.len());
        assert_eq!(consumed, HELLO_ZLIB.len(), "the entire stream is consumed");
        assert_eq!(
            produced,
            HELLO_PLAIN.len(),
            "the produced-count out-parameter matches the return value"
        );
    }

    #[test]
    fn uncompress2_consumes_only_the_stream_with_trailing_bytes() {
        // A valid stream followed by trailing junk: only the stream bytes are
        // consumed, and `*source_len` reports exactly that count.
        let mut buf = [0u8; 40];
        buf[..HELLO_ZLIB.len()].copy_from_slice(&HELLO_ZLIB);
        let mut out = [0u8; 32];
        let mut consumed = buf.len();
        let mut produced = out.len();
        let n = uncompress2(&mut out, &buf, &mut consumed, &mut produced).expect("ok");
        assert_eq!(n, HELLO_PLAIN.len());
        assert_eq!(
            consumed,
            HELLO_ZLIB.len(),
            "trailing bytes are not consumed"
        );
        assert_eq!(produced, HELLO_PLAIN.len(), "produced count is reported");
        assert_eq!(&out[..n], HELLO_PLAIN);
    }

    #[test]
    fn truncated_input_is_data_error() {
        // Feed only a prefix that cuts into the deflate data. The engine
        // consumes all of it yet never reaches stream end (leftover_in == 0),
        // which `uncompr.c` L80-81 maps to Z_DATA_ERROR.
        let mut out = [0u8; BIG_PLAIN_LEN];
        let err = uncompress(&mut out, &BIG_ZLIB[..7]).unwrap_err();
        assert_eq!(err, ReturnCode::DataError);
    }

    #[test]
    fn dest_too_small_is_buf_error() {
        // Full, valid, incompressible input but a far-too-small output buffer:
        // output fills while input still remains (leftover_in > 0), which falls
        // through to Z_BUF_ERROR.
        let mut out = [0u8; 64];
        let err = uncompress(&mut out, &INCOMP_ZLIB).unwrap_err();
        assert_eq!(err, ReturnCode::BufError);
    }

    #[test]
    fn corrupt_input_is_data_error() {
        let mut corrupt = HELLO_ZLIB; // `[u8; 20]` is `Copy`.
        corrupt[8] ^= 0xff; // Damage a deflate-data byte.
        let mut out = [0u8; 32];
        let err = uncompress(&mut out, &corrupt).unwrap_err();
        assert_eq!(err, ReturnCode::DataError);
    }

    #[test]
    fn zero_declared_source_len_is_data_error() {
        // Declaring zero available input means nothing can be decoded; with all
        // (zero) input "consumed" this is the truncated-stream branch.
        let mut out = [0u8; 32];
        let mut consumed = 0usize;
        let mut produced = out.len();
        let err = uncompress2(&mut out, &HELLO_ZLIB, &mut consumed, &mut produced).unwrap_err();
        assert_eq!(err, ReturnCode::DataError);
        assert_eq!(consumed, 0);
        assert_eq!(produced, 0, "no output is produced on this error path");
    }

    #[test]
    fn source_len_cap_is_honored() {
        // Capping the declared input below the stream length prevents the
        // engine from ever finishing, and the cap is never exceeded.
        let mut out = [0u8; 32];
        let mut consumed = 6usize; // Fewer than the 20-byte stream.
        let mut produced = out.len();
        let result = uncompress2(&mut out, &HELLO_ZLIB, &mut consumed, &mut produced);
        assert!(result.is_err(), "a capped, incomplete stream cannot finish");
        assert!(consumed <= 6, "never read past the declared cap");
        assert!(
            produced <= out.len(),
            "produced count stays within the buffer"
        );
    }
}
