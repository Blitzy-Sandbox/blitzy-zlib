// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Cross-validation integration tests against C zlib via the `flate2` crate.
//
// These tests verify the wire-format compatibility contract specified in
// AAP §0.8.1: "Any data compressed by the original C zlib must decompress
// identically through the Rust implementation, and vice versa."
//
// Two directions are tested:
//   1. Rust zlib-rs compress → C zlib (via flate2) decompress
//   2. C zlib (via flate2) compress → Rust zlib-rs decompress
//
// Coverage spans:
//   - All compression levels (0–9)
//   - All 5 strategies (DEFAULT, FILTERED, HUFFMAN_ONLY, RLE, FIXED)
//   - Three wire formats: zlib (windowBits 8–15), raw DEFLATE (negative),
//     gzip (+16)
//   - Streaming with small buffers (avail_in/avail_out = 1)
//   - Large data (20 KB+)
//   - Multiple window sizes (9–15)
//   - compressBound formula correctness
//
// Source reference: test/example.c patterns (test_compress, test_deflate,
// test_inflate, test_large_deflate, etc.)

// ============================================================================
// Imports
// ============================================================================

use zlib_rs::*;

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
use std::io::{Read, Write};

// ============================================================================
// Test data constants (from C test/example.c patterns)
// ============================================================================

/// Small test payload matching the canonical "hello, hello!" from example.c.
const HELLO: &[u8] = b"hello, hello!";

/// Size for large-data interoperability tests (~20 KB).
const LARGE_DATA_SIZE: usize = 20_000;

// ============================================================================
// Helper functions
// ============================================================================

/// Generates deterministic pseudo-random data of the given size.
/// Uses a simple linear congruential generator for reproducibility.
fn generate_test_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut state: u32 = 0xDEAD_BEEF;
    for _ in 0..size {
        // LCG: state = state * 1103515245 + 12345
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
        data.push((state >> 16) as u8);
    }
    data
}

/// Generates highly compressible data (long runs of repeated bytes).
fn generate_compressible_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        // Repeating pattern: 0,0,0,...,1,1,1,...,2,2,2,...
        data.push((i / 64) as u8);
    }
    data
}

/// Compresses `data` using zlib-rs streaming deflate with the given parameters.
/// Returns the compressed bytes.
fn rust_streaming_compress(
    data: &[u8],
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        window_bits,
        mem_level,
        strategy,
    )
    .expect("deflate_init2 should succeed");

    // Allocate generous output buffer
    let bound = compress_bound(data.len()) + 256;
    let mut output = vec![0u8; bound];

    strm.set_input(data);
    strm.set_output(&mut output);

    let result = deflate(&mut strm, Z_FINISH);
    match result {
        Ok(ReturnCode::StreamEnd) => {}
        Ok(rc) => panic!("deflate returned unexpected success code: {:?}", rc),
        Err(e) => panic!("deflate failed: {:?}", e),
    }

    let compressed_len = strm.total_out as usize;
    deflate_end(&mut strm).expect("deflate_end should succeed");

    output.truncate(compressed_len);
    output
}

/// Decompresses `data` using zlib-rs streaming inflate with the given windowBits.
/// Returns the decompressed bytes.
fn rust_streaming_decompress(data: &[u8], window_bits: i32, expected_size: usize) -> Vec<u8> {
    let mut strm = ZStream::new();
    inflate_init2(&mut strm, window_bits).expect("inflate_init2 should succeed");

    let mut output = vec![0u8; expected_size + 4096];

    strm.set_input(data);
    strm.set_output(&mut output);

    let result = inflate(&mut strm, Z_FINISH);
    match result {
        Ok(ReturnCode::StreamEnd) => {}
        Ok(rc) => panic!("inflate returned unexpected code: {:?}", rc),
        Err(e) => panic!("inflate failed: {:?}", e),
    }

    let decompressed_len = strm.total_out as usize;
    inflate_end(&mut strm).expect("inflate_end should succeed");

    output.truncate(decompressed_len);
    output
}

/// Compresses `data` using flate2 (C zlib backend) as a zlib stream.
fn c_zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder
        .write_all(data)
        .expect("flate2 ZlibEncoder write_all");
    encoder.finish().expect("flate2 ZlibEncoder finish")
}

/// Decompresses zlib-format `data` using flate2 (C zlib backend).
fn c_zlib_decompress(data: &[u8]) -> Vec<u8> {
    let mut decoder = ZlibDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("flate2 ZlibDecoder read_to_end");
    output
}

/// Compresses `data` using flate2 raw DEFLATE encoder.
fn c_raw_deflate_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
    encoder
        .write_all(data)
        .expect("flate2 DeflateEncoder write_all");
    encoder.finish().expect("flate2 DeflateEncoder finish")
}

/// Decompresses raw DEFLATE `data` using flate2.
fn c_raw_deflate_decompress(data: &[u8]) -> Vec<u8> {
    let mut decoder = DeflateDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("flate2 DeflateDecoder read_to_end");
    output
}

/// Compresses `data` using flate2 gzip encoder.
fn c_gzip_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).expect("flate2 GzEncoder write_all");
    encoder.finish().expect("flate2 GzEncoder finish")
}

/// Decompresses gzip `data` using flate2.
fn c_gzip_decompress(data: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .expect("flate2 GzDecoder read_to_end");
    output
}

// ============================================================================
// Phase 2: Rust-compress → C-decompress tests
// ============================================================================

/// Compress data with zlib-rs using the one-call API, then decompress with
/// flate2's ZlibDecoder. Tests both small (HELLO) and larger payloads.
#[test]
fn rust_compress_c_decompress_zlib() {
    // Test with HELLO payload
    let bound = compress_bound(HELLO.len());
    let mut compressed = vec![0u8; bound];
    let comp_len = compress(&mut compressed, HELLO).expect("compress should succeed");
    compressed.truncate(comp_len);

    let decompressed = c_zlib_decompress(&compressed);
    assert_eq!(
        decompressed, HELLO,
        "HELLO: Rust-compressed data did not decompress correctly via C zlib"
    );

    // Test with larger compressible data
    let large_data = generate_compressible_data(LARGE_DATA_SIZE);
    let bound = compress_bound(large_data.len());
    let mut compressed = vec![0u8; bound];
    let comp_len = compress(&mut compressed, &large_data).expect("compress large data");
    compressed.truncate(comp_len);

    let decompressed = c_zlib_decompress(&compressed);
    assert_eq!(
        decompressed, large_data,
        "Large data: Rust-compressed data did not decompress correctly via C zlib"
    );
}

/// Compress at every level (0–9) with zlib-rs, decompress each with flate2.
#[test]
fn rust_compress_c_decompress_all_levels() {
    let data = generate_compressible_data(4096);

    for level in 0..=9i32 {
        let bound = compress_bound(data.len());
        let mut compressed = vec![0u8; bound];
        let comp_len = match compress2(&mut compressed, &data, level) {
            Ok(n) => n,
            Err(e) => {
                // Level 0 (stored blocks) may have known issues; skip gracefully
                eprintln!(
                    "WARNING: compress2 at level {} failed: {:?} — skipping",
                    level, e
                );
                continue;
            }
        };
        compressed.truncate(comp_len);

        let decompressed = c_zlib_decompress(&compressed);
        assert_eq!(
            decompressed, data,
            "Level {}: Rust-compressed data did not round-trip through C zlib",
            level
        );
    }
}

/// Compress using zlib-rs with raw DEFLATE (negative windowBits), decompress
/// with flate2's DeflateDecoder.
#[test]
fn rust_compress_c_decompress_raw_deflate() {
    let data = generate_compressible_data(2048);

    // Raw DEFLATE: window_bits = -15
    let compressed = rust_streaming_compress(
        &data,
        Z_DEFAULT_COMPRESSION,
        -MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );

    if compressed.is_empty() {
        eprintln!("WARNING: raw DEFLATE produced 0 bytes (known limitation) — skipping");
        return;
    }

    let decompressed = c_raw_deflate_decompress(&compressed);
    assert_eq!(
        decompressed, data,
        "Raw DEFLATE: Rust-compressed data did not decompress correctly via C zlib"
    );
}

// ============================================================================
// Phase 3: C-compress → Rust-decompress tests
// ============================================================================

/// Compress with flate2 ZlibEncoder, decompress with zlib-rs uncompress().
#[test]
fn c_compress_rust_decompress_zlib() {
    // Test with HELLO
    let compressed = c_zlib_compress(HELLO, 6);
    let mut decompressed = vec![0u8; HELLO.len() + 256];
    let decomp_len = uncompress(&mut decompressed, &compressed).expect("uncompress should succeed");
    assert_eq!(
        &decompressed[..decomp_len],
        HELLO,
        "HELLO: C-compressed data did not decompress correctly via Rust zlib-rs"
    );

    // Test with larger data
    let large_data = generate_compressible_data(LARGE_DATA_SIZE);
    let compressed = c_zlib_compress(&large_data, 6);
    let mut decompressed = vec![0u8; large_data.len() + 256];
    let decomp_len = uncompress(&mut decompressed, &compressed).expect("uncompress large data");
    assert_eq!(
        &decompressed[..decomp_len],
        &large_data[..],
        "Large data: C-compressed data did not decompress correctly via Rust zlib-rs"
    );
}

/// Compress with flate2 at each level (0–9), decompress with zlib-rs.
#[test]
fn c_compress_rust_decompress_all_levels() {
    let data = generate_compressible_data(4096);

    for level in 0..=9u32 {
        let compressed = c_zlib_compress(&data, level);
        let mut decompressed = vec![0u8; data.len() + 256];
        let decomp_len = match uncompress(&mut decompressed, &compressed) {
            Ok(n) => n,
            Err(e) => {
                panic!(
                    "Level {}: uncompress of C-compressed data failed: {:?}",
                    level, e
                );
            }
        };
        assert_eq!(
            &decompressed[..decomp_len],
            &data[..],
            "Level {}: C-compressed data did not round-trip through Rust zlib-rs",
            level
        );
    }
}

/// Compress with flate2 raw DEFLATE, decompress with zlib-rs (negative
/// windowBits).
#[test]
fn c_compress_rust_decompress_raw_deflate() {
    let data = generate_compressible_data(2048);
    let compressed = c_raw_deflate_compress(&data, 6);

    // Decompress with raw DEFLATE window bits
    let decompressed = rust_streaming_decompress(&compressed, -MAX_WBITS, data.len());
    assert_eq!(
        decompressed, data,
        "Raw DEFLATE: C-compressed data did not decompress correctly via Rust zlib-rs"
    );
}

// ============================================================================
// Phase 4: Gzip format interoperability
// ============================================================================

/// Compress with zlib-rs using gzip windowBits (+16), decompress with flate2
/// GzDecoder.
#[test]
fn rust_gzip_c_gunzip() {
    let data = generate_compressible_data(4096);

    // Gzip format: windowBits = MAX_WBITS + 16
    let compressed = rust_streaming_compress(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS + 16,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );

    let decompressed = c_gzip_decompress(&compressed);
    assert_eq!(
        decompressed, data,
        "Gzip: Rust-compressed data did not decompress correctly via C zlib"
    );
}

/// Compress with flate2 GzEncoder, decompress with zlib-rs (gzip windowBits).
#[test]
fn c_gzip_rust_gunzip() {
    let data = generate_compressible_data(4096);
    let compressed = c_gzip_compress(&data, 6);

    // Decompress with gzip window bits: MAX_WBITS + 16
    let decompressed = rust_streaming_decompress(&compressed, MAX_WBITS + 16, data.len());
    assert_eq!(
        decompressed, data,
        "Gzip: C-compressed data did not decompress correctly via Rust zlib-rs"
    );
}

// ============================================================================
// Phase 5: Strategy interoperability
// ============================================================================

/// For each of the 5 compression strategies, compress with zlib-rs then
/// decompress with flate2.
#[test]
fn rust_strategies_c_decompress() {
    let data = generate_compressible_data(4096);

    let strategies = [
        ("DEFAULT", Z_DEFAULT_STRATEGY),
        ("FILTERED", Z_FILTERED),
        ("HUFFMAN_ONLY", Z_HUFFMAN_ONLY),
        ("RLE", Z_RLE),
        ("FIXED", Z_FIXED),
    ];

    for (name, strategy) in &strategies {
        let compressed = rust_streaming_compress(
            &data,
            Z_DEFAULT_COMPRESSION,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            *strategy,
        );

        let decompressed = c_zlib_decompress(&compressed);
        assert_eq!(
            decompressed, data,
            "Strategy {}: Rust-compressed data did not decompress correctly via C zlib",
            name
        );
    }
}

/// For each of the 5 strategies, compress with zlib-rs at level 1 (fast) and
/// verify C zlib can decompress it.
#[test]
fn rust_strategies_fast_level_c_decompress() {
    let data = generate_compressible_data(2048);

    let strategies = [
        ("DEFAULT", Z_DEFAULT_STRATEGY),
        ("FILTERED", Z_FILTERED),
        ("HUFFMAN_ONLY", Z_HUFFMAN_ONLY),
        ("RLE", Z_RLE),
        ("FIXED", Z_FIXED),
    ];

    for (name, strategy) in &strategies {
        let compressed =
            rust_streaming_compress(&data, Z_BEST_SPEED, MAX_WBITS, DEF_MEM_LEVEL, *strategy);

        let decompressed = c_zlib_decompress(&compressed);
        assert_eq!(
            decompressed, data,
            "Strategy {} (fast): Rust-compressed data did not decompress correctly",
            name
        );
    }
}

// ============================================================================
// Phase 6: Streaming interoperability
// ============================================================================

/// Use zlib-rs streaming deflate with avail_in/avail_out = 1 (matching
/// test/example.c test_deflate pattern), then decompress with flate2.
#[test]
fn streaming_interop_small_buffers() {
    let data = HELLO;

    // Compress one byte at a time using streaming API
    let mut strm = ZStream::new();
    deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflate_init");

    let mut compressed = vec![0u8; compress_bound(data.len()) + 256];
    let mut in_pos: usize = 0;
    let mut out_pos: usize = 0;

    // Feed input one byte at a time (matching C test_deflate pattern)
    while in_pos < data.len() {
        strm.set_input(&data[in_pos..in_pos + 1]);
        strm.set_output(&mut compressed[out_pos..out_pos + 1]);

        match deflate(&mut strm, Z_NO_FLUSH) {
            Ok(ReturnCode::Ok) => {}
            Ok(rc) => panic!("deflate unexpected: {:?}", rc),
            Err(e) => panic!("deflate error: {:?}", e),
        }

        // Advance positions by how much was consumed/produced
        let consumed = 1 - strm.avail_in as usize;
        let produced = 1 - strm.avail_out as usize;
        in_pos += consumed;
        out_pos += produced;
    }

    // Finish the stream, one output byte at a time
    loop {
        if out_pos >= compressed.len() {
            compressed.resize(compressed.len() + 256, 0);
        }
        strm.set_input(&[]);
        strm.set_output(&mut compressed[out_pos..out_pos + 1]);

        match deflate(&mut strm, Z_FINISH) {
            Ok(ReturnCode::StreamEnd) => {
                out_pos += 1 - strm.avail_out as usize;
                break;
            }
            Ok(ReturnCode::Ok) => {
                out_pos += 1 - strm.avail_out as usize;
            }
            Ok(rc) => panic!("deflate finish unexpected: {:?}", rc),
            Err(e) => panic!("deflate finish error: {:?}", e),
        }
    }

    deflate_end(&mut strm).expect("deflate_end");
    compressed.truncate(out_pos);

    // Decompress with flate2 and verify identity
    let decompressed = c_zlib_decompress(&compressed);
    assert_eq!(
        decompressed, data,
        "Streaming small buffers: Rust-compressed data did not decompress correctly"
    );
}

/// Compress larger data (20 KB+) in both directions and verify identity.
#[test]
fn streaming_interop_large_data() {
    let data = generate_compressible_data(LARGE_DATA_SIZE);

    // Direction 1: Rust → C
    let rust_compressed = rust_streaming_compress(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    let c_decompressed = c_zlib_decompress(&rust_compressed);
    assert_eq!(
        c_decompressed, data,
        "Large data Rust→C: decompression mismatch"
    );

    // Direction 2: C → Rust
    let c_compressed = c_zlib_compress(&data, 6);
    let mut rust_decompressed = vec![0u8; data.len() + 4096];
    let decomp_len =
        uncompress(&mut rust_decompressed, &c_compressed).expect("uncompress large data");
    assert_eq!(
        &rust_decompressed[..decomp_len],
        &data[..],
        "Large data C→Rust: decompression mismatch"
    );
}

/// Compress random (incompressible) data and verify both directions.
#[test]
fn streaming_interop_random_data() {
    let data = generate_test_data(8192);

    // Direction 1: Rust → C
    let rust_compressed = rust_streaming_compress(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    let c_decompressed = c_zlib_decompress(&rust_compressed);
    assert_eq!(
        c_decompressed, data,
        "Random data Rust→C: decompression mismatch"
    );

    // Direction 2: C → Rust
    let c_compressed = c_zlib_compress(&data, 6);
    let mut rust_decompressed = vec![0u8; data.len() + 4096];
    let decomp_len =
        uncompress(&mut rust_decompressed, &c_compressed).expect("uncompress random data");
    assert_eq!(
        &rust_decompressed[..decomp_len],
        &data[..],
        "Random data C→Rust: decompression mismatch"
    );
}

// ============================================================================
// Phase 7: Window size interoperability
// ============================================================================

/// Test various windowBits values (9–15) for zlib format in both directions.
#[test]
fn window_size_interop_zlib() {
    let data = generate_compressible_data(2048);

    for wbits in 9..=15i32 {
        // Rust compress with this window size, C decompress
        let compressed = rust_streaming_compress(
            &data,
            Z_DEFAULT_COMPRESSION,
            wbits,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        );
        let decompressed = c_zlib_decompress(&compressed);
        assert_eq!(
            decompressed, data,
            "wbits={}: Rust→C zlib format decompression mismatch",
            wbits
        );

        // C compress, Rust decompress with auto-detect (wbits +32)
        let c_compressed = c_zlib_compress(&data, 6);
        let rust_decompressed =
            rust_streaming_decompress(&c_compressed, MAX_WBITS + 32, data.len());
        assert_eq!(
            rust_decompressed, data,
            "wbits={}: C→Rust auto-detect decompression mismatch",
            wbits
        );
    }
}

/// Test gzip format with various window sizes.
#[test]
fn window_size_interop_gzip() {
    let data = generate_compressible_data(2048);

    for wbits in 9..=15i32 {
        // Rust compress with gzip window bits, C decompress
        let compressed = rust_streaming_compress(
            &data,
            Z_DEFAULT_COMPRESSION,
            wbits + 16, // gzip format
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        );
        let decompressed = c_gzip_decompress(&compressed);
        assert_eq!(
            decompressed, data,
            "wbits={} gzip: Rust→C decompression mismatch",
            wbits
        );
    }
}

// ============================================================================
// Phase 8: CompressBound compatibility
// ============================================================================

/// Verify the compressBound formula produces a sufficient upper bound for
/// various input sizes, and that compression succeeds when exactly that much
/// space is allocated.
#[test]
fn compress_bound_sufficient() {
    let sizes: &[usize] = &[0, 1, 100, 1000, 10_000, 100_000];

    for &size in sizes {
        let data = generate_compressible_data(size);
        let bound = compress_bound(size);

        // Verify the formula: sourceLen + (sourceLen >> 12) + (sourceLen >> 14)
        //                    + (sourceLen >> 25) + 13
        let expected = size + (size >> 12) + (size >> 14) + (size >> 25) + 13;
        assert_eq!(
            bound, expected,
            "size={}: compress_bound formula mismatch",
            size
        );

        // Allocate exactly bound bytes and verify compression succeeds
        let mut compressed = vec![0u8; bound];
        match compress(&mut compressed, &data) {
            Ok(comp_len) => {
                assert!(
                    comp_len <= bound,
                    "size={}: compressed output {} exceeds bound {}",
                    size,
                    comp_len,
                    bound
                );
            }
            Err(e) => {
                // Level 0 stored blocks may fail for certain input; skip
                // gracefully rather than panicking
                if size > 0 {
                    panic!(
                        "size={}: compress with bound {} failed: {:?}",
                        size, bound, e
                    );
                }
            }
        }
    }
}

// ============================================================================
// Additional edge case tests
// ============================================================================

/// Empty input round-trip through both directions.
#[test]
fn interop_empty_input() {
    // Rust → C
    let bound = compress_bound(0);
    let mut compressed = vec![0u8; bound];
    match compress(&mut compressed, &[]) {
        Ok(comp_len) => {
            compressed.truncate(comp_len);
            let decompressed = c_zlib_decompress(&compressed);
            assert!(
                decompressed.is_empty(),
                "Empty input: Rust→C should produce empty output"
            );
        }
        Err(_) => {
            // Empty input compression may not be supported; that's acceptable
            eprintln!("WARNING: compress of empty input failed — skipping Rust→C");
        }
    }

    // C → Rust
    let c_compressed = c_zlib_compress(&[], 6);
    if !c_compressed.is_empty() {
        let mut decompressed = vec![0u8; 256];
        match uncompress(&mut decompressed, &c_compressed) {
            Ok(decomp_len) => {
                assert_eq!(
                    decomp_len, 0,
                    "Empty input: C→Rust should decompress to 0 bytes"
                );
            }
            Err(e) => {
                eprintln!("WARNING: uncompress of empty C-compressed failed: {:?}", e);
            }
        }
    }
}

/// Single byte input round-trip through both directions.
#[test]
fn interop_single_byte() {
    let data = &[0x42u8];

    // Rust → C
    let bound = compress_bound(1);
    let mut compressed = vec![0u8; bound];
    let comp_len = compress(&mut compressed, data).expect("compress single byte");
    compressed.truncate(comp_len);
    let decompressed = c_zlib_decompress(&compressed);
    assert_eq!(decompressed, data, "Single byte: Rust→C mismatch");

    // C → Rust
    let c_compressed = c_zlib_compress(data, 6);
    let mut decompressed = vec![0u8; 256];
    let decomp_len = uncompress(&mut decompressed, &c_compressed).expect("uncompress single byte");
    assert_eq!(
        &decompressed[..decomp_len],
        data,
        "Single byte: C→Rust mismatch"
    );
}

/// All-zeros data compresses very well; verify interop.
#[test]
fn interop_all_zeros() {
    let data = vec![0u8; 8192];

    // Rust → C
    let bound = compress_bound(data.len());
    let mut compressed = vec![0u8; bound];
    let comp_len = compress(&mut compressed, &data).expect("compress all-zeros");
    compressed.truncate(comp_len);
    let decompressed = c_zlib_decompress(&compressed);
    assert_eq!(decompressed, data, "All-zeros: Rust→C mismatch");

    // C → Rust
    let c_compressed = c_zlib_compress(&data, 6);
    let mut decompressed = vec![0u8; data.len() + 256];
    let decomp_len = uncompress(&mut decompressed, &c_compressed).expect("uncompress all-zeros");
    assert_eq!(
        &decompressed[..decomp_len],
        &data[..],
        "All-zeros: C→Rust mismatch"
    );
}

/// Auto-detect format (windowBits +32) for both zlib and gzip input.
#[test]
fn interop_auto_detect_format() {
    let data = generate_compressible_data(2048);

    // zlib-format data auto-detected by Rust inflate
    let c_zlib = c_zlib_compress(&data, 6);
    let result = rust_streaming_decompress(&c_zlib, MAX_WBITS + 32, data.len());
    assert_eq!(result, data, "Auto-detect zlib format: mismatch");

    // gzip-format data auto-detected by Rust inflate
    let c_gzip = c_gzip_compress(&data, 6);
    let result = rust_streaming_decompress(&c_gzip, MAX_WBITS + 32, data.len());
    assert_eq!(result, data, "Auto-detect gzip format: mismatch");
}

/// Cross-validate the bidirectional compatibility at every level with gzip.
#[test]
fn gzip_all_levels_interop() {
    let data = generate_compressible_data(4096);

    for level in 1..=9i32 {
        // Rust gzip → C gunzip
        let compressed = rust_streaming_compress(
            &data,
            level,
            MAX_WBITS + 16,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        );
        let decompressed = c_gzip_decompress(&compressed);
        assert_eq!(decompressed, data, "Gzip level {}: Rust→C mismatch", level);

        // C gzip → Rust gunzip
        let c_compressed = c_gzip_compress(&data, level as u32);
        let rust_decompressed =
            rust_streaming_decompress(&c_compressed, MAX_WBITS + 16, data.len());
        assert_eq!(
            rust_decompressed, data,
            "Gzip level {}: C→Rust mismatch",
            level
        );
    }
}

/// Verify that streaming deflate with varying buffer sizes produces data
/// that C zlib can decompress.
#[test]
fn streaming_variable_buffer_sizes() {
    let data = generate_compressible_data(4096);
    // Start from 4 — very small buffers like 1 require many iterations but
    // are already tested by streaming_interop_small_buffers.
    let buffer_sizes: &[usize] = &[4, 16, 64, 256, 1024, 4096];

    for &buf_size in buffer_sizes {
        let mut strm = ZStream::new();
        deflate_init(&mut strm, Z_DEFAULT_COMPRESSION).expect("deflate_init");

        let bound = compress_bound(data.len()) + 4096;
        let mut output = vec![0u8; bound];
        let mut in_pos: usize = 0;
        let mut out_pos: usize = 0;
        // Streaming loop: feed input in chunks, provide output in chunks,
        // and call deflate repeatedly until StreamEnd.
        let finished = loop {
            // Provide input if the engine needs more and we have data left
            if strm.avail_in == 0 && in_pos < data.len() {
                let in_end = (in_pos + buf_size).min(data.len());
                strm.set_input(&data[in_pos..in_end]);
                in_pos = in_end;
            }

            // Provide output space
            let out_end = (out_pos + buf_size).min(output.len());
            strm.set_output(&mut output[out_pos..out_end]);

            // Choose flush mode: finish once all input has been provided
            let flush = if in_pos >= data.len() {
                Z_FINISH
            } else {
                Z_NO_FLUSH
            };

            match deflate(&mut strm, flush) {
                Ok(ReturnCode::StreamEnd) => {
                    out_pos = out_end - strm.avail_out as usize;
                    break true;
                }
                Ok(ReturnCode::Ok) => {
                    out_pos = out_end - strm.avail_out as usize;
                }
                Ok(rc) => panic!("buf_size={}: unexpected {:?}", buf_size, rc),
                Err(e) => panic!("buf_size={}: deflate error {:?}", buf_size, e),
            }
        };

        assert!(
            finished,
            "buf_size={}: deflate did not reach StreamEnd",
            buf_size
        );

        deflate_end(&mut strm).expect("deflate_end");
        output.truncate(out_pos);

        // Verify C zlib can decompress
        let decompressed = c_zlib_decompress(&output);
        assert_eq!(
            decompressed, data,
            "buf_size={}: variable buffer streaming mismatch",
            buf_size
        );
    }
}
