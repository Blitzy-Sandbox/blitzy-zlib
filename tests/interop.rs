//! Interoperability oracle: `zlib_rs` vs `flate2` (canonical C zlib).
//!
//! This integration-test suite is the project's **byte-for-byte compatibility
//! oracle** (AAP §0.4.1 "flate2 (C zlib) byte-identical oracle"; §0.6.7
//! bit-exact parity; §0.7.2 user constraint *"Output must be binary-compatible
//! with zlib-produced streams"*). It validates the rewrite against the real
//! reference implementation along three axes:
//!
//! 1. **Byte-identity** — for the same input, level, window size, memory level,
//!    and the default strategy, `zlib_rs` must emit the *exact same bytes* as C
//!    zlib. The [`flate2`] dev-dependency is built with its `zlib` feature, so
//!    it links **canonical C zlib** (`libz-sys`); whatever bytes it produces are
//!    the ground truth. The C backend initializes deflate with `memLevel = 8`,
//!    `Z_DEFAULT_STRATEGY`, and `windowBits = ±15`, which is exactly what
//!    `zlib_rs`'s [`compress2`]/[`Deflate`] use, so byte-identity is the
//!    expected (not merely hoped-for) result.
//! 2. **Decode of C zlib output** — `zlib_rs` must inflate any zlib, raw, or
//!    gzip stream that C zlib produces.
//! 3. **Decode by C zlib** — C zlib must inflate the zlib and raw streams that
//!    `zlib_rs` produces, including streams built with non-default strategies
//!    and with a mid-stream `SyncFlush`.
//!
//! The corpora and round-trip shape are conceptually derived from
//! `test/example.c` (notably its `"hello, hello!"` string, whose repeated token
//! "stresses the compression code better").
//!
//! Run with `cargo test --test interop`.
//!
//! # Symbol-collision note
//!
//! `zlib_rs`'s `#[no_mangle] extern "C"` exports live behind the non-default
//! `capi` feature, so during `cargo test` the C ABI symbols (`compress`,
//! `crc32`, …) are *not* exported by `zlib_rs`. That is what lets this test
//! link the C zlib that `flate2` pulls in without a duplicate-symbol clash. We
//! therefore never enable the `capi` feature here.

// The compatibility core is pure safe Rust, and this oracle exercises only safe
// APIs of both crates — enforce that no `unsafe` sneaks in (AAP §0.7.2
// "Zero unsafe blocks in core compression logic").
#![forbid(unsafe_code)]

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use zlib_rs::util::{compress2, uncompress};
use zlib_rs::{
    DEF_MEM_LEVEL, Deflate, FlushMode, Inflate, MAX_WBITS, ReturnCode, Strategy,
    Z_DEFAULT_COMPRESSION, Z_NO_FLUSH, Z_OK, Z_STREAM_END, adler32, crc32,
};

// ===========================================================================
// Test corpora
// ===========================================================================

/// A natural-language prose block (the classic "lorem ipsum"), representative of
/// English text: moderately compressible with a mix of repeated and unique
/// tokens.
const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis \
nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute \
irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla \
pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia \
deserunt mollit anim id est laborum.";

/// Build a structured, JSON-like blob: highly regular framing (keys, brackets,
/// separators) interleaved with varying values — the shape that exercises the
/// dynamic-Huffman path well.
fn json_like_blob() -> Vec<u8> {
    let mut text = String::from("[\n");
    for id in 0..256 {
        text.push_str(&format!(
            "  {{\"id\": {id}, \"name\": \"item-{id}\", \"active\": {}, \"tags\": [\"a\", \"b\", \"c\"]}},\n",
            id % 2 == 0
        ));
    }
    text.push_str("]\n");
    text.into_bytes()
}

/// Build a deterministic, low-redundancy byte stream via a simple linear
/// congruential generator. This is the least compressible corpus and pushes
/// `zlib_rs` toward the stored/dynamic block-type boundary, exercising the
/// block-selection logic that must match C zlib bit-for-bit.
fn mixed_binary_blob() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(3000);
    let mut state: u32 = 0x1234_5678;
    for _ in 0..3000 {
        // glibc's LCG constants; the high bits are the most "random".
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        bytes.push((state >> 16) as u8);
    }
    bytes
}

/// The shared corpus set used by every byte-identity and round-trip test. Each
/// element stresses a different part of the encoder:
///
/// * `"hello, hello!"` — the canonical `test/example.c` string (short, repeated
///   token);
/// * [`LOREM`] — natural-language prose;
/// * 5000 repeated `b'a'` — maximal redundancy (run-length friendly);
/// * [`json_like_blob`] — structured/regular text;
/// * [`mixed_binary_blob`] — near-incompressible bytes.
fn corpora() -> Vec<Vec<u8>> {
    vec![
        b"hello, hello!".to_vec(),
        LOREM.as_bytes().to_vec(),
        vec![b'a'; 5000],
        json_like_blob(),
        mixed_binary_blob(),
    ]
}

// ===========================================================================
// flate2 (canonical C zlib) helpers
// ===========================================================================

/// Drive a `flate2` [`Compress`] to completion over `data`, returning the full
/// compressed stream.
///
/// `compress_vec` writes into the spare capacity of the output `Vec` and tracks
/// consumption via the compressor's own `total_in`, so we reserve room, advance
/// the input window by `total_in`, and loop until [`Status::StreamEnd`]. A
/// progress assertion guards against an accidental infinite loop.
fn drive_flate2_compress(mut compressor: Compress, data: &[u8]) -> Vec<u8> {
    // Generous initial capacity so the common case finishes in one call; the
    // loop grows it should any corpus ever need more room (e.g. level-0 stored).
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 64);
    loop {
        if out.len() == out.capacity() {
            out.reserve(data.len() / 2 + 64);
        }
        let already_consumed = compressor.total_in() as usize;
        let progress_before = (compressor.total_in(), compressor.total_out());
        let status = compressor
            .compress_vec(&data[already_consumed..], &mut out, FlushCompress::Finish)
            .expect("flate2 Compress::compress_vec returned an error");
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                assert_ne!(
                    progress_before,
                    (compressor.total_in(), compressor.total_out()),
                    "flate2 compression stalled before reaching the end of the stream"
                );
            }
        }
    }
    out
}

/// Drive a `flate2` [`Decompress`] to completion over `compressed`, returning
/// the recovered plaintext. `hint_len` is the expected output size, used only to
/// size the working buffer; the loop grows it if needed.
fn drive_flate2_decompress(
    mut decompressor: Decompress,
    compressed: &[u8],
    hint_len: usize,
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(hint_len + 64);
    loop {
        if out.len() == out.capacity() {
            out.reserve(hint_len + 64);
        }
        let already_consumed = decompressor.total_in() as usize;
        let progress_before = (decompressor.total_in(), decompressor.total_out());
        let status = decompressor
            .decompress_vec(
                &compressed[already_consumed..],
                &mut out,
                FlushDecompress::Finish,
            )
            .expect("flate2 Decompress::decompress_vec returned an error");
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                assert_ne!(
                    progress_before,
                    (decompressor.total_in(), decompressor.total_out()),
                    "flate2 decompression stalled before reaching the end of the stream"
                );
            }
        }
    }
    out
}

/// Compress `data` to an RFC 1950 zlib stream using canonical C zlib at `level`
/// (default strategy, `memLevel = 8`, `windowBits = 15`).
fn flate2_zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
    drive_flate2_compress(Compress::new(Compression::new(level), true), data)
}

/// Compress `data` to a raw RFC 1951 DEFLATE stream (no wrapper) using canonical
/// C zlib at `level` (`windowBits = -15`).
fn flate2_raw_compress(data: &[u8], level: u32) -> Vec<u8> {
    drive_flate2_compress(Compress::new(Compression::new(level), false), data)
}

/// Compress `data` to an RFC 1952 gzip stream using canonical C zlib at `level`
/// (`windowBits = 15 + 16`). Gated on `gzip` because the helper is only used by
/// the gzip-framed interop test.
#[cfg(feature = "gzip")]
fn flate2_gzip_compress(data: &[u8], level: u32) -> Vec<u8> {
    drive_flate2_compress(Compress::new_gzip(Compression::new(level), 15), data)
}

/// Decompress an RFC 1950 zlib stream with canonical C zlib.
fn flate2_zlib_decompress(compressed: &[u8], hint_len: usize) -> Vec<u8> {
    drive_flate2_decompress(Decompress::new(true), compressed, hint_len)
}

/// Decompress a raw RFC 1951 DEFLATE stream with canonical C zlib.
fn flate2_raw_decompress(compressed: &[u8], hint_len: usize) -> Vec<u8> {
    drive_flate2_decompress(Decompress::new(false), compressed, hint_len)
}

// ===========================================================================
// zlib_rs helpers
// ===========================================================================

/// Compress `data` with `zlib_rs` using full control over framing/strategy.
///
/// The output buffer is sized via [`Deflate::bound`] (the exact upper bound for
/// the configured stream), so a single [`FlushMode::Finish`] call completes the
/// stream; both the completion and full input consumption are asserted.
fn zlibrs_deflate(data: &[u8], level: i32, window_bits: i32, strategy: Strategy) -> Vec<u8> {
    let mut compressor = Deflate::with_options(level, window_bits, DEF_MEM_LEVEL, strategy)
        .expect("zlib_rs Deflate::with_options rejected valid parameters");
    let bound = compressor.bound(data.len() as u64) as usize;
    let mut out = vec![0u8; bound.max(64)];
    let outcome = compressor
        .compress(data, &mut out, FlushMode::Finish)
        .expect("zlib_rs Deflate::compress returned an error");
    assert_eq!(
        outcome.code,
        ReturnCode::StreamEnd,
        "zlib_rs deflate did not finish in a single call (deflate_bound undersized?)"
    );
    assert_eq!(
        outcome.consumed,
        data.len(),
        "zlib_rs deflate left input unconsumed"
    );
    out.truncate(outcome.produced);
    out
}

/// Decompress `compressed` with `zlib_rs` for the given `window_bits` framing,
/// returning the recovered plaintext. `hint_len` sizes the working buffer; the
/// loop keeps feeding until [`Z_STREAM_END`].
fn zlibrs_inflate(compressed: &[u8], window_bits: i32, hint_len: usize) -> Vec<u8> {
    let mut decompressor = Inflate::with_window_bits(window_bits)
        .expect("zlib_rs Inflate::with_window_bits rejected valid window_bits");
    let mut out: Vec<u8> = Vec::new();
    let mut scratch = vec![0u8; hint_len.max(64) + 64];
    let mut in_pos = 0usize;
    loop {
        let (ret, consumed, produced) =
            decompressor.inflate(&compressed[in_pos..], &mut scratch, Z_NO_FLUSH);
        in_pos += consumed;
        out.extend_from_slice(&scratch[..produced]);
        // Explicit `==` comparisons (rather than bare-const match patterns) make
        // the intent unambiguous: `Z_STREAM_END`/`Z_OK` are plain `i32`
        // constants, and matching them as patterns would silently bind a fresh
        // variable if one were ever not in scope.
        if ret == Z_STREAM_END {
            break;
        }
        if ret == Z_OK {
            assert!(
                consumed != 0 || produced != 0,
                "zlib_rs inflate made no progress (ret = Z_OK)"
            );
        } else {
            panic!("zlib_rs inflate failed with return code {ret}");
        }
    }
    out
}

// ===========================================================================
// Group A — byte-identical compressed output: zlib_rs vs C zlib
//
// At the default strategy, canonical zlib output is fully deterministic for a
// given (level, windowBits, memLevel). `zlib_rs` MUST reproduce it byte-for-byte
// (AAP §0.6.7 / §0.7.2). Each test sweeps the whole corpus set at one level.
// ===========================================================================

/// Assert `zlib_rs` zlib-wrapper output equals C zlib output, byte-for-byte,
/// across the whole corpus set at `level`, and that the shared bytes round-trip.
fn assert_zlib_byte_identical(level: i32) {
    for data in corpora() {
        let produced_rs = compress2(&data, level).expect("zlib_rs compress2 failed");
        let produced_c = flate2_zlib_compress(&data, level as u32);
        assert_eq!(
            produced_rs,
            produced_c,
            "zlib-wrapper output differs from C zlib at level {level} for a {}-byte corpus",
            data.len()
        );
        // The identical bytes must of course decode back to the original.
        let restored = zlibrs_inflate(&produced_rs, MAX_WBITS, data.len());
        assert_eq!(
            restored, data,
            "zlib_rs failed to round-trip its own level-{level} stream"
        );
    }
}

#[test]
fn zlib_wrapper_byte_identical_level_0() {
    assert_zlib_byte_identical(0);
}

#[test]
fn zlib_wrapper_byte_identical_level_1() {
    assert_zlib_byte_identical(1);
}

#[test]
fn zlib_wrapper_byte_identical_level_2() {
    assert_zlib_byte_identical(2);
}

#[test]
fn zlib_wrapper_byte_identical_level_6() {
    assert_zlib_byte_identical(6);
}

#[test]
fn zlib_wrapper_byte_identical_level_9() {
    assert_zlib_byte_identical(9);
}

/// Assert `zlib_rs` raw-DEFLATE output equals C zlib output, byte-for-byte,
/// across the whole corpus set at `level`, and that the shared bytes round-trip.
fn assert_raw_byte_identical(level: i32) {
    for data in corpora() {
        let produced_rs = zlibrs_deflate(&data, level, -MAX_WBITS, Strategy::Default);
        let produced_c = flate2_raw_compress(&data, level as u32);
        assert_eq!(
            produced_rs,
            produced_c,
            "raw DEFLATE output differs from C zlib at level {level} for a {}-byte corpus",
            data.len()
        );
        let restored = zlibrs_inflate(&produced_rs, -MAX_WBITS, data.len());
        assert_eq!(
            restored, data,
            "zlib_rs failed to round-trip its own raw level-{level} stream"
        );
    }
}

#[test]
fn raw_deflate_byte_identical_level_1() {
    assert_raw_byte_identical(1);
}

#[test]
fn raw_deflate_byte_identical_level_6() {
    assert_raw_byte_identical(6);
}

#[test]
fn raw_deflate_byte_identical_level_9() {
    assert_raw_byte_identical(9);
}

// ===========================================================================
// Group B — zlib_rs decodes C zlib output (we read what C zlib writes)
// ===========================================================================

/// C zlib compresses (zlib wrapper) across several levels; `zlib_rs` must
/// inflate the result back to the original. Decode is checked via both the
/// streaming [`Inflate`] front end and the one-shot [`uncompress`] helper.
#[test]
fn inflate_flate2_zlib_stream() {
    for level in [0u32, 1, 6, 9] {
        for data in corpora() {
            let compressed = flate2_zlib_compress(&data, level);

            // Streaming decode.
            let restored = zlibrs_inflate(&compressed, MAX_WBITS, data.len());
            assert_eq!(
                restored, data,
                "zlib_rs Inflate failed on a C zlib stream (level {level})"
            );

            // One-shot decode through the `uncompress` convenience helper.
            let mut one_shot = vec![0u8; data.len()];
            let produced =
                uncompress(&mut one_shot, &compressed).expect("zlib_rs uncompress failed");
            assert_eq!(produced, data.len(), "uncompress reported the wrong length");
            assert_eq!(
                &one_shot[..produced],
                &data[..],
                "zlib_rs uncompress failed on a C zlib stream (level {level})"
            );
        }
    }
}

/// C zlib compresses (raw DEFLATE) across several levels; `zlib_rs` must inflate
/// the result with `windowBits = -15`.
#[test]
fn inflate_flate2_raw_stream() {
    for level in [1u32, 6, 9] {
        for data in corpora() {
            let compressed = flate2_raw_compress(&data, level);
            let restored = zlibrs_inflate(&compressed, -MAX_WBITS, data.len());
            assert_eq!(
                restored, data,
                "zlib_rs Inflate failed on a C zlib raw stream (level {level})"
            );
        }
    }
}

/// C zlib compresses (gzip wrapper); `zlib_rs` must inflate the result via both
/// explicit gzip (`windowBits = 31`) and zlib/gzip auto-detection
/// (`windowBits = 47`). Gated on `gzip`.
#[cfg(feature = "gzip")]
#[test]
fn inflate_flate2_gzip_stream() {
    for level in [1u32, 6, 9] {
        for data in corpora() {
            let compressed = flate2_gzip_compress(&data, level);

            let via_auto = zlibrs_inflate(&compressed, 47, data.len());
            assert_eq!(
                via_auto, data,
                "zlib_rs failed to inflate a C zlib gzip stream via auto-detect (level {level})"
            );

            let via_gzip = zlibrs_inflate(&compressed, 31, data.len());
            assert_eq!(
                via_gzip, data,
                "zlib_rs failed to inflate a C zlib gzip stream via windowBits=31 (level {level})"
            );
        }
    }
}

// ===========================================================================
// Group C — C zlib decodes zlib_rs output (C zlib reads what we write)
// ===========================================================================

/// `zlib_rs` compresses (zlib wrapper) across several levels; canonical C zlib
/// must inflate the result back to the original.
#[test]
fn flate2_inflates_zlibrs_zlib() {
    for level in [0i32, 1, 6, 9] {
        for data in corpora() {
            let compressed = compress2(&data, level).expect("zlib_rs compress2 failed");
            let restored = flate2_zlib_decompress(&compressed, data.len());
            assert_eq!(
                restored, data,
                "C zlib failed to inflate a zlib_rs zlib stream (level {level})"
            );
        }
    }
}

/// `zlib_rs` compresses (raw DEFLATE) across several levels; canonical C zlib
/// must inflate the result back to the original.
#[test]
fn flate2_inflates_zlibrs_raw() {
    for level in [1i32, 6, 9] {
        for data in corpora() {
            let compressed = zlibrs_deflate(&data, level, -MAX_WBITS, Strategy::Default);
            let restored = flate2_raw_decompress(&compressed, data.len());
            assert_eq!(
                restored, data,
                "C zlib failed to inflate a zlib_rs raw stream (level {level})"
            );
        }
    }
}

// ===========================================================================
// Group D — strategy variations (cross-decode)
//
// Non-default strategies change the LZ77/Huffman decisions but the emitted
// stream must remain valid DEFLATE. The authoritative check is that canonical C
// zlib can decode whatever a given strategy produces; we also confirm zlib_rs
// round-trips its own strategy-varied output.
// ===========================================================================

/// For `strategy`, sweep a few levels and the whole corpus set: `zlib_rs`
/// deflates (zlib wrapper) with that strategy, then (1) C zlib must inflate the
/// result and (2) `zlib_rs` must inflate it too.
fn assert_strategy_crossdecode(strategy: Strategy) {
    for level in [1i32, 6, 9] {
        for data in corpora() {
            let compressed = zlibrs_deflate(&data, level, MAX_WBITS, strategy);

            let by_c = flate2_zlib_decompress(&compressed, data.len());
            assert_eq!(
                by_c, data,
                "C zlib failed to decode a zlib_rs {strategy:?} stream (level {level})"
            );

            let by_rs = zlibrs_inflate(&compressed, MAX_WBITS, data.len());
            assert_eq!(
                by_rs, data,
                "zlib_rs failed to round-trip its own {strategy:?} stream (level {level})"
            );
        }
    }
}

#[test]
fn strategy_filtered_crossdecode() {
    assert_strategy_crossdecode(Strategy::Filtered);
}

#[test]
fn strategy_huffman_only_crossdecode() {
    assert_strategy_crossdecode(Strategy::HuffmanOnly);
}

#[test]
fn strategy_rle_crossdecode() {
    assert_strategy_crossdecode(Strategy::Rle);
}

#[test]
fn strategy_fixed_crossdecode() {
    assert_strategy_crossdecode(Strategy::Fixed);
}

// ===========================================================================
// Group E — checksum cross-validation
// ===========================================================================

/// `zlib_rs::crc32` must agree with `flate2::Crc` (also IEEE CRC-32) for a range
/// of inputs, and must reproduce the canonical `"123456789"` known-answer value
/// `0xCBF4_3926`.
#[test]
fn crc32_matches_flate2() {
    let zeros = [0u8; 1024];
    let inputs: [&[u8]; 5] = [
        b"123456789",
        b"hello, hello!",
        b"",
        LOREM.as_bytes(),
        &zeros,
    ];
    for data in inputs {
        let from_rs = crc32(0, data);
        let mut reference = flate2::Crc::new();
        reference.update(data);
        assert_eq!(
            from_rs,
            reference.sum(),
            "crc32 disagreed with flate2 for a {}-byte input",
            data.len()
        );
    }

    // The standard CRC-32 check value for the ASCII string "123456789".
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);

    // Streaming a checksum in two chunks must equal the one-shot computation.
    let one_shot = crc32(0, b"hello, hello!");
    let chunked = crc32(crc32(0, b"hello, "), b"hello!");
    assert_eq!(one_shot, chunked, "crc32 chunking changed the result");
}

/// `zlib_rs::adler32` must reproduce the canonical `"123456789"` known-answer
/// value `0x091E_01DE`. `flate2` does not expose an Adler-32 accessor, so the
/// published reference value is the oracle here; we additionally verify the
/// seed and chunk-associativity invariants.
#[test]
fn adler32_matches_reference() {
    // Canonical known-answer value (seed 1).
    assert_eq!(adler32(1, b"123456789"), 0x091E_01DE);

    // Adler-32 of the empty input is just the seed.
    assert_eq!(adler32(1, b""), 1);

    // Streaming a checksum in two chunks must equal the one-shot computation.
    let one_shot = adler32(1, b"hello, hello!");
    let chunked = adler32(adler32(1, b"hello, "), b"hello!");
    assert_eq!(one_shot, chunked, "adler32 chunking changed the result");
}

// ===========================================================================
// Group F — flush-mode interop
// ===========================================================================

/// A `SyncFlush` in the middle of a stream must produce a byte boundary that C
/// zlib can decode. We deflate the first half with [`FlushMode::SyncFlush`],
/// then the remainder with [`FlushMode::Finish`], and confirm both C zlib and
/// `zlib_rs` recover the original.
#[test]
fn sync_flush_interop() {
    for data in corpora() {
        // Need at least a couple of bytes on each side of the flush point.
        if data.len() < 4 {
            continue;
        }
        let split = data.len() / 2;

        let mut compressor = Deflate::with_options(6, MAX_WBITS, DEF_MEM_LEVEL, Strategy::Default)
            .expect("zlib_rs Deflate::with_options rejected valid parameters");
        let mut scratch = vec![0u8; data.len() + 128];
        let mut stream: Vec<u8> = Vec::new();

        // First half, flushed to a sync boundary.
        let first = compressor
            .compress(&data[..split], &mut scratch, FlushMode::SyncFlush)
            .expect("zlib_rs deflate (SyncFlush) returned an error");
        assert_eq!(
            first.consumed, split,
            "SyncFlush did not consume the first half in one call"
        );
        stream.extend_from_slice(&scratch[..first.produced]);

        // Remaining half, finishing the stream (drain pending output via Finish).
        let mut in_pos = split;
        loop {
            let outcome = compressor
                .compress(&data[in_pos..], &mut scratch, FlushMode::Finish)
                .expect("zlib_rs deflate (Finish) returned an error");
            in_pos += outcome.consumed;
            stream.extend_from_slice(&scratch[..outcome.produced]);
            if outcome.code == ReturnCode::StreamEnd {
                break;
            }
            assert!(
                outcome.consumed != 0 || outcome.produced != 0,
                "zlib_rs deflate (Finish) stalled after a SyncFlush"
            );
        }

        let by_c = flate2_zlib_decompress(&stream, data.len());
        assert_eq!(
            by_c, data,
            "C zlib failed to decode a zlib_rs sync-flushed stream"
        );

        let by_rs = zlibrs_inflate(&stream, MAX_WBITS, data.len());
        assert_eq!(
            by_rs, data,
            "zlib_rs failed to round-trip its own sync-flushed stream"
        );
    }
}

// ===========================================================================
// Extra fixed-corpus interop variants
// ===========================================================================

/// `Z_DEFAULT_COMPRESSION` (-1) must resolve to level 6, so its output must be
/// byte-identical to both an explicit level-6 `zlib_rs` stream and C zlib's
/// level-6 output.
#[test]
fn default_compression_resolves_to_level_6() {
    for data in corpora() {
        let default_level =
            compress2(&data, Z_DEFAULT_COMPRESSION).expect("zlib_rs compress2 (default) failed");
        let explicit_level_6 = compress2(&data, 6).expect("zlib_rs compress2 (level 6) failed");
        let c_level_6 = flate2_zlib_compress(&data, 6);

        assert_eq!(
            default_level, explicit_level_6,
            "Z_DEFAULT_COMPRESSION must produce the same bytes as explicit level 6"
        );
        assert_eq!(
            default_level, c_level_6,
            "zlib_rs default-level output must be byte-identical to C zlib level 6"
        );
    }
}

/// A larger, mildly repetitive corpus that spans multiple DEFLATE blocks:
/// verify byte-identity at the default level and decode in both directions.
#[test]
fn large_repetitive_corpus_interops_both_directions() {
    const SIZE: usize = 64 * 1024;
    let mut data = Vec::with_capacity(SIZE);
    for i in 0..SIZE {
        // A periodic base pattern perturbed slowly so the stream is neither
        // trivially run-length nor random — good multi-block coverage.
        let base = b"abcdefgh"[i % 8];
        data.push(base ^ ((i / 997) as u8));
    }

    let level = 6i32;
    let produced_rs = compress2(&data, level).expect("zlib_rs compress2 failed");
    let produced_c = flate2_zlib_compress(&data, level as u32);
    assert_eq!(
        produced_rs, produced_c,
        "byte-identity must hold for the large corpus at level 6"
    );

    // C zlib decodes zlib_rs output…
    let by_c = flate2_zlib_decompress(&produced_rs, data.len());
    assert_eq!(
        by_c, data,
        "C zlib failed to decode the large zlib_rs stream"
    );

    // …and zlib_rs decodes C zlib output.
    let by_rs = zlibrs_inflate(&produced_c, MAX_WBITS, data.len());
    assert_eq!(
        by_rs, data,
        "zlib_rs failed to decode the large C zlib stream"
    );
}
