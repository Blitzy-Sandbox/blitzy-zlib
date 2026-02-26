//! Cross-implementation compatibility tests.
//!
//! Validates that data compressed by `zlib-rs` can be decompressed by reference
//! C zlib (via flate2), and vice versa. This ensures binary stream compatibility
//! per RFC 1950 (zlib), RFC 1951 (DEFLATE), and RFC 1952 (gzip).
//!
//! # Test Directions
//!
//! - **Rust → flate2**: `zlib-rs` compress, `flate2` decompress
//! - **flate2 → Rust**: `flate2` compress, `zlib-rs` decompress
//!
//! # Formats Tested
//!
//! - zlib (RFC 1950) — header/trailer with Adler-32
//! - gzip (RFC 1952) — header/trailer with CRC-32
//! - raw DEFLATE (RFC 1951) — no header/trailer

// Test code may use wildcard imports for concise access to shared helpers.
#![allow(clippy::wildcard_imports)]
// Test helper functions always produce used values; #[must_use] is noise here.
#![allow(clippy::must_use_candidate)]
// Test helpers intentionally panic on failure; documenting every unwrap is noise.
#![allow(clippy::missing_panics_doc)]

use std::io::{Read, Write};

use flate2::read::{DeflateDecoder, DeflateEncoder, GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder as GzWriteEncoder, ZlibEncoder as ZlibWriteEncoder};
use flate2::Compression;

use zlib_rs::deflate;
use zlib_rs::inflate;
use zlib_rs_tests::*;

// =============================================================================
// Streaming Compression / Decompression Helpers (zlib-rs)
// =============================================================================

/// Compress `data` using the zlib-rs streaming deflate API.
///
/// `window_bits` selects the output format:
/// - `MAX_WBITS` (15): zlib (RFC 1950)
/// - `-MAX_WBITS` (−15): raw DEFLATE (RFC 1951)
/// - `MAX_WBITS + 16` (31): gzip (RFC 1952)
fn zlib_rs_streaming_compress(data: &[u8], window_bits: i32, level: i32) -> Vec<u8> {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        level,
        Z_DEFLATED,
        window_bits,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed: {ret:?}");

    stream.set_input(data);
    // compress_bound covers zlib overhead; add 64 for gzip header/trailer margin.
    let bound = compress_bound(data.len()).max(128) + 64;
    stream.set_output_buffer(bound);

    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "deflate did not complete: {ret:?}");

    let output = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);
    output
}

/// Decompress `compressed` using the zlib-rs streaming inflate API.
///
/// `window_bits` must match the format produced by the compressor.
/// `expected_size` is used to pre-allocate the output buffer.
///
/// The inflate API takes separate `&mut InflateState` and `&mut ZStream`
/// parameters.  We create the state externally with [`InflateState::new()`]
/// and configure it via [`inflate::inflate_reset2`], mirroring the pattern
/// used in the official infcover test suite.
fn zlib_rs_streaming_decompress(
    compressed: &[u8],
    window_bits: i32,
    expected_size: usize,
) -> Vec<u8> {
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, window_bits);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed: {ret:?}");

    stream.set_input(compressed);
    stream.set_output_buffer(expected_size + 1024);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "inflate did not complete: {ret:?}"
    );

    let _ = inflate::inflate_end(&mut state, &mut stream);
    stream.take_output()
}

// =============================================================================
// flate2 Compression / Decompression Helpers
// =============================================================================

/// Compress `data` with flate2 in zlib format at the given level.
fn flate2_zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = ZlibWriteEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(data).expect("flate2 zlib write failed");
    enc.finish().expect("flate2 zlib finish failed")
}

/// Decompress zlib-format bytes with flate2.
fn flate2_zlib_decompress(compressed: &[u8]) -> Vec<u8> {
    let mut dec = ZlibDecoder::new(compressed);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .expect("flate2 zlib decompress failed");
    out
}

/// Compress `data` with flate2 in gzip format at the given level.
fn flate2_gz_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = GzWriteEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(data).expect("flate2 gz write failed");
    enc.finish().expect("flate2 gz finish failed")
}

/// Decompress gzip-format bytes with flate2.
fn flate2_gz_decompress(compressed: &[u8]) -> Vec<u8> {
    let mut dec = GzDecoder::new(compressed);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .expect("flate2 gz decompress failed");
    out
}

/// Compress `data` with flate2 in raw DEFLATE format at the given level.
fn flate2_raw_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(data, Compression::new(level));
    let mut out = Vec::new();
    enc.read_to_end(&mut out)
        .expect("flate2 raw compress failed");
    out
}

/// Decompress raw DEFLATE bytes with flate2.
fn flate2_raw_decompress(compressed: &[u8]) -> Vec<u8> {
    let mut dec = DeflateDecoder::new(compressed);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .expect("flate2 raw decompress failed");
    out
}

// =============================================================================
// Test Data Generators
// =============================================================================

/// 10 KiB of mixed repeating English text.
fn generate_medium_data() -> Vec<u8> {
    let pat = b"The quick brown fox jumps over the lazy dog. ";
    let target = 10_240;
    let mut data = Vec::with_capacity(target);
    while data.len() < target {
        let take = (target - data.len()).min(pat.len());
        data.extend_from_slice(&pat[..take]);
    }
    data
}

/// 1 MiB of repeating alphanumeric pattern.
fn generate_large_data() -> Vec<u8> {
    let pat = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz\n";
    let target = 1_048_576;
    let mut data = Vec::with_capacity(target);
    while data.len() < target {
        let take = (target - data.len()).min(pat.len());
        data.extend_from_slice(&pat[..take]);
    }
    data
}

/// `len` bytes of pseudo-random data (deterministic xorshift PRNG).
#[allow(clippy::cast_possible_truncation)]
fn generate_random_data(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    let mut s: u64 = 0xDEAD_BEEF_CAFE_BABE;
    for byte in &mut data {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *byte = (s & 0xFF) as u8;
    }
    data
}

/// ~2 KiB of typical English prose.
fn generate_text_data() -> Vec<u8> {
    let text = b"The quick brown fox jumps over the lazy dog. \
                 Pack my box with five dozen liquor jugs. \
                 How vexingly quick daft zebras jump. \
                 Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
                 sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
    let target = 2048;
    let mut data = Vec::with_capacity(target);
    while data.len() < target {
        let take = (target - data.len()).min(text.len());
        data.extend_from_slice(&text[..take]);
    }
    data
}

/// ~4 KiB of mixed binary patterns: sequential, reversed, alternating, zeros/ones.
fn generate_binary_data() -> Vec<u8> {
    let mut data = Vec::with_capacity(4096);
    // Sequential 0x00..0xFF
    for i in 0..=255u16 {
        #[allow(clippy::cast_possible_truncation)]
        data.push(i as u8);
    }
    // Reversed 0xFF..0x00
    for i in (0..=255u16).rev() {
        #[allow(clippy::cast_possible_truncation)]
        data.push(i as u8);
    }
    // Alternating 0x55/0xAA
    for _ in 0..256 {
        data.push(0x55);
        data.push(0xAA);
    }
    // Fill remainder with a 0x00/0xFF mix
    while data.len() < 4096 {
        data.push(if data.len() % 3 == 0 { 0x00 } else { 0xFF });
    }
    data
}

// =============================================================================
// Phase 2: Rust Compress → flate2 Decompress (zlib format)
// =============================================================================

/// Small payload: "hello, hello!" compressed by zlib-rs, decompressed by flate2.
#[test]
fn test_rust_compress_flate2_decompress_zlib_small() {
    let source = &HELLO[..HELLO.len() - 1]; // strip NUL terminator
    let mut compressed = Vec::new();
    compress(&mut compressed, source).expect("zlib-rs compress failed");
    assert!(!compressed.is_empty(), "compressed output must not be empty");
    let decompressed = flate2_zlib_decompress(&compressed);
    assert_bytes_equal(&decompressed, source, "small zlib-rs→flate2");
}

/// Medium payload: 10 KiB compressed by zlib-rs, decompressed by flate2.
#[test]
fn test_rust_compress_flate2_decompress_zlib_medium() {
    let data = generate_medium_data();
    let mut compressed = Vec::new();
    compress2(&mut compressed, &data, Z_DEFAULT_COMPRESSION).expect("zlib-rs compress2 failed");
    let decompressed = flate2_zlib_decompress(&compressed);
    assert_bytes_equal(&decompressed, &data, "medium zlib-rs→flate2");
}

/// Large payload: 1 MiB compressed by zlib-rs, decompressed by flate2.
#[test]
fn test_rust_compress_flate2_decompress_zlib_large() {
    let data = generate_large_data();
    let mut compressed = Vec::new();
    compress(&mut compressed, &data).expect("zlib-rs compress failed");
    let decompressed = flate2_zlib_decompress(&compressed);
    assert_bytes_equal(&decompressed, &data, "large zlib-rs→flate2");
}

/// All levels (0–9): zlib-rs compress at each level, flate2 decompresses.
#[test]
fn test_rust_compress_flate2_decompress_all_levels() {
    let data = generate_medium_data();
    for level in 0..=9_i32 {
        let mut compressed = Vec::new();
        compress2(&mut compressed, &data, level)
            .unwrap_or_else(|e| panic!("zlib-rs compress2 level {level} failed: {e:?}"));
        let decompressed = flate2_zlib_decompress(&compressed);
        assert_bytes_equal(
            &decompressed,
            &data,
            &format!("all-levels zlib-rs→flate2 level={level}"),
        );
    }
}

// =============================================================================
// Phase 3: flate2 Compress → Rust Decompress (zlib format)
// =============================================================================

/// Small payload: flate2 compresses "hello, hello!", zlib-rs decompresses.
#[test]
fn test_flate2_compress_rust_decompress_zlib_small() {
    let source = &HELLO[..HELLO.len() - 1];
    let compressed = flate2_zlib_compress(source, 6);
    let mut decompressed = vec![0u8; source.len()];
    uncompress(&mut decompressed, &compressed).expect("zlib-rs uncompress failed");
    assert_bytes_equal(&decompressed, source, "small flate2→zlib-rs");
}

/// Medium payload: flate2 compresses 10 KiB, zlib-rs decompresses.
#[test]
fn test_flate2_compress_rust_decompress_zlib_medium() {
    let data = generate_medium_data();
    let compressed = flate2_zlib_compress(&data, 6);
    let mut decompressed = vec![0u8; data.len()];
    uncompress(&mut decompressed, &compressed).expect("zlib-rs uncompress failed");
    assert_bytes_equal(&decompressed, &data, "medium flate2→zlib-rs");
}

/// Large payload: flate2 compresses 1 MiB, zlib-rs decompresses.
#[test]
fn test_flate2_compress_rust_decompress_zlib_large() {
    let data = generate_large_data();
    let compressed = flate2_zlib_compress(&data, 6);
    let mut decompressed = vec![0u8; data.len()];
    uncompress(&mut decompressed, &compressed).expect("zlib-rs uncompress failed");
    assert_bytes_equal(&decompressed, &data, "large flate2→zlib-rs");
}

/// All levels (0–9): flate2 compresses at each level, zlib-rs decompresses.
#[test]
fn test_flate2_compress_rust_decompress_all_levels() {
    let data = generate_medium_data();
    for level in 0..=9_u32 {
        let compressed = flate2_zlib_compress(&data, level);
        let mut decompressed = vec![0u8; data.len()];
        uncompress(&mut decompressed, &compressed)
            .unwrap_or_else(|e| panic!("zlib-rs uncompress level {level} failed: {e:?}"));
        assert_bytes_equal(
            &decompressed,
            &data,
            &format!("all-levels flate2→zlib-rs level={level}"),
        );
    }
}

// =============================================================================
// Phase 4: Gzip Format Cross-Compatibility
// =============================================================================

/// zlib-rs compresses with gzip framing (windowBits = MAX_WBITS + 16 = 31),
/// flate2 decompresses with its `GzDecoder`.
#[test]
fn test_rust_compress_flate2_decompress_gzip() {
    let data = generate_medium_data();
    let gzip_window_bits = MAX_WBITS + 16; // 31 → gzip format
    let compressed =
        zlib_rs_streaming_compress(&data, gzip_window_bits, Z_DEFAULT_COMPRESSION);
    assert!(!compressed.is_empty(), "gzip compressed output must not be empty");

    let decompressed = flate2_gz_decompress(&compressed);
    assert_bytes_equal(&decompressed, &data, "gzip zlib-rs→flate2");
}

/// flate2 compresses with gzip framing, zlib-rs decompresses using
/// windowBits = MAX_WBITS + 32 = 47 (auto-detect zlib / gzip).
#[test]
fn test_flate2_compress_rust_decompress_gzip() {
    let data = generate_medium_data();
    let compressed = flate2_gz_compress(&data, 6);
    assert!(!compressed.is_empty(), "flate2 gzip output must not be empty");

    // auto-detect: windowBits = MAX_WBITS + 32 = 47 accepts either zlib or gzip.
    let auto_detect_window_bits = MAX_WBITS + 32; // 47
    let decompressed =
        zlib_rs_streaming_decompress(&compressed, auto_detect_window_bits, data.len());
    assert_bytes_equal(&decompressed, &data, "gzip flate2→zlib-rs");
}

// =============================================================================
// Phase 5: Raw DEFLATE Cross-Compatibility
// =============================================================================

/// zlib-rs compresses raw DEFLATE (negative windowBits), flate2 decompresses.
#[test]
fn test_rust_compress_flate2_decompress_raw() {
    let data = generate_medium_data();
    let raw_window_bits = -MAX_WBITS; // −15 → raw DEFLATE
    let compressed =
        zlib_rs_streaming_compress(&data, raw_window_bits, Z_DEFAULT_COMPRESSION);
    assert!(
        !compressed.is_empty(),
        "raw compressed output must not be empty"
    );

    let decompressed = flate2_raw_decompress(&compressed);
    assert_bytes_equal(&decompressed, &data, "raw zlib-rs→flate2");
}

/// flate2 compresses raw DEFLATE, zlib-rs decompresses with negative windowBits.
#[test]
fn test_flate2_compress_rust_decompress_raw() {
    let data = generate_medium_data();
    let compressed = flate2_raw_compress(&data, 6);
    assert!(!compressed.is_empty(), "flate2 raw output must not be empty");

    let raw_window_bits = -MAX_WBITS; // −15
    let decompressed =
        zlib_rs_streaming_decompress(&compressed, raw_window_bits, data.len());
    assert_bytes_equal(&decompressed, &data, "raw flate2→zlib-rs");
}

// =============================================================================
// Phase 6: Decompression Compatibility Verification
// =============================================================================
//
// NOTE: True bit-for-bit identity of *compressed* output requires both sides to
// use the exact same DEFLATE encoder.  `flate2` defaults to `miniz_oxide` which
// is a different implementation from `zlib-rs`.  Identical compressed bytes are
// therefore NOT expected.
//
// What IS guaranteed (and tested here) is *decompression* compatibility: data
// compressed by either implementation must be decompressible by the other and
// produce the same original bytes.

/// Verify that compressing the same input with both zlib-rs and flate2 produces
/// streams that are each decompressible by the other library, yielding identical
/// original data.
#[test]
fn test_compress_output_decompressible_by_both() {
    let data = generate_medium_data();

    // Compress with zlib-rs (default level)
    let mut zlib_rs_compressed = Vec::new();
    compress(&mut zlib_rs_compressed, &data).expect("zlib-rs compress failed");

    // Compress with flate2 (default level = 6)
    let flate2_compressed = flate2_zlib_compress(&data, 6);

    // Both compressed streams should be non-empty.
    assert!(!zlib_rs_compressed.is_empty());
    assert!(!flate2_compressed.is_empty());

    // flate2 can decompress the zlib-rs output.
    let from_zlib_rs = flate2_zlib_decompress(&zlib_rs_compressed);
    assert_bytes_equal(&from_zlib_rs, &data, "flate2 decompresses zlib-rs output");

    // zlib-rs can decompress the flate2 output.
    let mut from_flate2 = vec![0u8; data.len()];
    uncompress(&mut from_flate2, &flate2_compressed).expect("zlib-rs uncompress flate2 output");
    assert_bytes_equal(&from_flate2, &data, "zlib-rs decompresses flate2 output");
}

// =============================================================================
// Phase 7: Data Pattern Variety Tests
// =============================================================================
//
// Each test performs bidirectional round-trips:
//   1. zlib-rs compress → flate2 decompress
//   2. flate2 compress → zlib-rs decompress

/// Verify that both directions survive a round-trip for the given data.
fn assert_zlib_cross_compat(data: &[u8], context: &str) {
    // Direction 1: zlib-rs → flate2
    let mut zlib_rs_out = Vec::new();
    compress(&mut zlib_rs_out, data).expect("zlib-rs compress failed");
    let d1 = flate2_zlib_decompress(&zlib_rs_out);
    assert_bytes_equal(&d1, data, &format!("{context}: zlib-rs→flate2"));

    // Direction 2: flate2 → zlib-rs
    let flate2_out = flate2_zlib_compress(data, 6);
    // For uncompress, pre-allocate dest to original data length (or 1 for empty).
    let dest_len = if data.is_empty() { 1 } else { data.len() };
    let mut d2 = vec![0u8; dest_len];
    uncompress(&mut d2, &flate2_out).expect("zlib-rs uncompress failed");
    // For empty input, the decompressed output should also be empty.
    let expected = if data.is_empty() { &[][..] } else { data };
    assert_bytes_equal(&d2, expected, &format!("{context}: flate2→zlib-rs"));
}

/// Empty input: both directions must handle zero-length data.
#[test]
fn test_cross_compat_empty_data() {
    assert_zlib_cross_compat(&[], "empty");
}

/// Single byte: minimal non-trivial payload.
#[test]
fn test_cross_compat_single_byte() {
    assert_zlib_cross_compat(&[42u8], "single-byte");
}

/// Repeated data: one byte repeated 10 000 times (extremely compressible).
#[test]
fn test_cross_compat_repeated_data() {
    let data = vec![b'A'; 10_000];
    assert_zlib_cross_compat(&data, "repeated");
}

/// Pseudo-random data: 10 000 bytes (essentially incompressible).
#[test]
fn test_cross_compat_random_data() {
    let data = generate_random_data(10_000);
    assert_zlib_cross_compat(&data, "random");
}

/// English text: typical compression scenario (~2 KiB).
#[test]
fn test_cross_compat_text_data() {
    let data = generate_text_data();
    assert_zlib_cross_compat(&data, "text");
}

/// Binary data: mixed patterns (~4 KiB) — sequential, alternating, zeros/ones.
#[test]
fn test_cross_compat_binary_data() {
    let data = generate_binary_data();
    assert_zlib_cross_compat(&data, "binary");
}

// =============================================================================
// Phase 8: Streaming Cross-Compatibility
// =============================================================================

/// Streaming compression with `Z_SYNC_FLUSH` between chunks, decompressed by
/// flate2 as a single contiguous stream.
///
/// This validates that sync flush points emitted by zlib-rs produce a valid
/// stream that external decompressors can handle.
#[test]
fn test_cross_compat_streaming() {
    let data = generate_medium_data(); // 10 KiB
    let chunk_size = data.len() / 3;

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,    // zlib format
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed");

    // Pre-allocate a generous output buffer covering all chunks plus flush markers.
    let bound = compress_bound(data.len()) + 1024;
    stream.set_output_buffer(bound);

    // --- Chunk 1: compress with Z_SYNC_FLUSH ---
    stream.set_input(&data[..chunk_size]);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    assert_eq!(
        ret,
        ReturnCode::Ok,
        "sync flush chunk 1 failed: {ret:?}"
    );

    // --- Chunk 2: compress with Z_SYNC_FLUSH ---
    stream.set_input(&data[chunk_size..chunk_size * 2]);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    assert_eq!(
        ret,
        ReturnCode::Ok,
        "sync flush chunk 2 failed: {ret:?}"
    );

    // --- Chunk 3: finish the stream ---
    stream.set_input(&data[chunk_size * 2..]);
    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    // Loop until the compressor signals stream end.
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "final deflate did not complete: {ret:?}"
    );

    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);
    assert!(
        !compressed.is_empty(),
        "streaming compressed output must not be empty"
    );

    // Decompress the entire stream using flate2 in one shot.
    let decompressed = flate2_zlib_decompress(&compressed);
    assert_bytes_equal(&decompressed, &data, "streaming zlib-rs→flate2");
}
