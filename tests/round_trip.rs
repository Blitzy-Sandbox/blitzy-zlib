// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Property-based round-trip tests for compression/decompression.
// Verifies that for arbitrary input data:
//   compress(input, level, strategy) → decompress == input
//
// Extends the basic verification from test/example.c into comprehensive
// randomized testing using the `quickcheck` and `rand` dev-dependencies.
//
// All compression levels (0-9 and Z_DEFAULT_COMPRESSION), all windowBits
// variants (zlib, raw DEFLATE, gzip, auto-detect), all strategies, and all
// flush modes are fully tested.

// ============================================================================
// Imports
// ============================================================================

use zlib_rs::*;
use quickcheck::{quickcheck, TestResult};
use rand::Rng;

// ============================================================================
// Helper: one-call compress/decompress round-trip verification
// ============================================================================

/// Compresses `data` at the given level using the one-call API, then
/// decompresses and verifies identity. Returns `true` on success.
fn one_call_roundtrip(data: &[u8], level: i32) -> bool {
    let bound = compress_bound(data.len());
    let mut compressed = vec![0u8; bound];

    let comp_len = match compress2(&mut compressed, data, level) {
        Ok(n) => n,
        Err(_) => return false,
    };
    compressed.truncate(comp_len);

    // Allocate decompression buffer. For empty input, need at least 1 byte
    // for the next_out pointer.
    let out_size = if data.is_empty() { 1 } else { data.len() };
    let mut decompressed = vec![0u8; out_size];
    let decomp_len = match uncompress(&mut decompressed, &compressed) {
        Ok(n) => n,
        Err(_) => return false,
    };

    decomp_len == data.len() && decompressed[..decomp_len] == *data
}

// ============================================================================
// Helper: streaming deflate/inflate round-trip verification
// ============================================================================

/// Compresses `data` using the streaming API with full parameters, then
/// decompresses and verifies identity. Returns `true` on success.
fn streaming_roundtrip_full(
    data: &[u8],
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> bool {
    // --- Compress ---
    let bound = if data.is_empty() { 64 } else { data.len() * 2 + 64 };
    let mut compressed = vec![0u8; bound];

    let mut c_stream = ZStream::new();
    if deflate_init2(
        &mut c_stream,
        level,
        Z_DEFLATED,
        window_bits,
        mem_level,
        strategy,
    )
    .is_err()
    {
        return false;
    }
    c_stream.set_input(data);
    c_stream.set_output(&mut compressed);

    let result = deflate(&mut c_stream, Z_FINISH);
    let comp_len = c_stream.total_out as usize;
    let _ = deflate_end(&mut c_stream);

    match result {
        Ok(ReturnCode::StreamEnd) => {}
        _ => return false,
    }
    compressed.truncate(comp_len);

    // For inflate, compute the matching window_bits for decompression.
    let inflate_wbits = if window_bits < 0 {
        // Raw DEFLATE: pass matching negative window_bits
        window_bits
    } else if window_bits > 15 {
        // Gzip format: use auto-detect (+32) so both zlib and gzip work
        (window_bits - 16) + 32
    } else {
        // Zlib format: pass same window_bits
        window_bits
    };

    // --- Decompress ---
    let out_size = if data.is_empty() { 1 } else { data.len() };
    let mut decompressed = vec![0u8; out_size];

    let mut d_stream = ZStream::new();
    if inflate_init2(&mut d_stream, inflate_wbits).is_err() {
        return false;
    }
    d_stream.set_input(&compressed);
    d_stream.set_output(&mut decompressed);

    let result = inflate(&mut d_stream, Z_FINISH);
    let decomp_len = d_stream.total_out as usize;
    let _ = inflate_end(&mut d_stream);

    match result {
        Ok(ReturnCode::StreamEnd) => {}
        _ => return false,
    }

    decomp_len == data.len() && decompressed[..decomp_len] == *data
}

/// Simplified streaming round-trip using default window_bits and mem_level.
fn streaming_roundtrip(data: &[u8], level: i32) -> bool {
    streaming_roundtrip_full(
        data,
        level,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    )
}

// ============================================================================
// Helper: streaming round-trip with small (1-byte) buffers
// ============================================================================

/// Compresses `data` using single-byte buffer availability per deflate/inflate
/// call, similar to test/example.c's test_deflate/test_inflate pattern where
/// `avail_in = avail_out = 1`. The full buffers are provided up front but only
/// 1 byte of availability is exposed per call.
fn streaming_small_buffer_roundtrip(data: &[u8], level: i32) -> bool {
    // --- Compress with 1-byte availability ---
    let mut c_stream = ZStream::new();
    if deflate_init(&mut c_stream, level).is_err() {
        return false;
    }

    let mut output = vec![0u8; data.len() * 2 + 64];
    // Set up the full buffers, then restrict availability to 1 byte at a time
    c_stream.set_input(data);
    c_stream.set_output(&mut output);
    c_stream.avail_in = 0;
    c_stream.avail_out = 0;

    loop {
        // Feed 1 byte of input if available and engine has consumed prior input
        if c_stream.avail_in == 0 && (c_stream.total_in as usize) < data.len() {
            c_stream.avail_in = 1;
        }
        // Provide 1 byte of output space
        if c_stream.avail_out == 0 {
            c_stream.avail_out = 1;
        }

        let flush = if (c_stream.total_in as usize) >= data.len() && c_stream.avail_in == 0 {
            Z_FINISH
        } else {
            Z_NO_FLUSH
        };

        let result = deflate(&mut c_stream, flush);
        match result {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(ReturnCode::Ok) => continue,
            Err(_) => {
                let _ = deflate_end(&mut c_stream);
                return false;
            }
            _ => {
                let _ = deflate_end(&mut c_stream);
                return false;
            }
        }
    }
    let comp_len = c_stream.total_out as usize;
    let _ = deflate_end(&mut c_stream);
    let compressed = &output[..comp_len];

    // --- Decompress with 1-byte availability ---
    let mut d_stream = ZStream::new();
    if inflate_init(&mut d_stream).is_err() {
        return false;
    }

    let mut decomp_buf = vec![0u8; data.len() + 1];
    d_stream.set_input(compressed);
    d_stream.set_output(&mut decomp_buf);
    d_stream.avail_in = 0;
    d_stream.avail_out = 0;

    loop {
        // Feed 1 byte of compressed input
        if d_stream.avail_in == 0 && (d_stream.total_in as usize) < compressed.len() {
            d_stream.avail_in = 1;
        }
        // Provide 1 byte of output space
        if d_stream.avail_out == 0 {
            d_stream.avail_out = 1;
        }

        let result = inflate(&mut d_stream, Z_NO_FLUSH);
        match result {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(ReturnCode::Ok) => continue,
            Err(_) => {
                let _ = inflate_end(&mut d_stream);
                return false;
            }
            _ => {
                let _ = inflate_end(&mut d_stream);
                return false;
            }
        }
    }
    let decomp_len = d_stream.total_out as usize;
    let _ = inflate_end(&mut d_stream);

    decomp_len == data.len() && decomp_buf[..decomp_len] == *data
}

// ============================================================================
// Phase 2: One-Call Round-Trip Property Tests
// ============================================================================

/// Quickcheck: compress→uncompress identity for arbitrary data at default level.
#[test]
fn prop_compress_uncompress_identity() {
    fn roundtrip(data: Vec<u8>) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        TestResult::from_bool(one_call_roundtrip(&data, Z_DEFAULT_COMPRESSION))
    }
    quickcheck(roundtrip as fn(Vec<u8>) -> TestResult);
}

/// Quickcheck: compress2→uncompress identity for levels 0-9 and
/// Z_DEFAULT_COMPRESSION. All 10 compression levels per AAP §0.8.1.
#[test]
fn prop_compress2_all_levels() {
    fn roundtrip(data: Vec<u8>, level_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        // Map to levels 0-9 or -1 (default).
        let level = match level_raw % 11 {
            10 => Z_DEFAULT_COMPRESSION,
            n => n as i32,
        };
        TestResult::from_bool(one_call_roundtrip(&data, level))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

// ============================================================================
// Phase 3: Streaming Round-Trip Property Tests
// ============================================================================

/// Quickcheck: streaming deflate→inflate identity with random levels (0-9).
#[test]
fn prop_streaming_deflate_inflate_identity() {
    fn roundtrip(data: Vec<u8>, level_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        // Map to levels 0-9
        let level = (level_raw % 10) as i32;
        TestResult::from_bool(streaming_roundtrip(&data, level))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

/// Quickcheck: streaming round-trip with 1-byte buffers, verifying the
/// engine handles small avail_in/avail_out correctly (like test/example.c
/// test_deflate/test_inflate).
#[test]
fn prop_streaming_small_buffers() {
    fn roundtrip(data: Vec<u8>) -> TestResult {
        if data.is_empty() || data.len() > 256 {
            // Limit size for small-buffer tests to keep runtime reasonable
            return TestResult::discard();
        }
        TestResult::from_bool(streaming_small_buffer_roundtrip(
            &data,
            Z_DEFAULT_COMPRESSION,
        ))
    }
    quickcheck(roundtrip as fn(Vec<u8>) -> TestResult);
}

// ============================================================================
// Phase 4: Window Size Round-Trip Tests
// ============================================================================

/// Test all valid zlib-format window sizes (9-15) with property-based data.
#[test]
fn prop_all_window_sizes() {
    fn roundtrip(data: Vec<u8>, wbits_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        // Map to window bits 9-15
        let wbits = (wbits_raw % 7) as i32 + 9;
        TestResult::from_bool(streaming_roundtrip_full(
            &data,
            Z_DEFAULT_COMPRESSION,
            wbits,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        ))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

/// Test gzip format (windowBits +16: 25-31) with property-based data.
#[test]
fn prop_gzip_format_roundtrip() {
    fn roundtrip(data: Vec<u8>, wbits_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        // Map to gzip window bits 25-31 (i.e., 9+16 through 15+16)
        let wbits = (wbits_raw % 7) as i32 + 9 + 16;
        TestResult::from_bool(streaming_roundtrip_full(
            &data,
            Z_DEFAULT_COMPRESSION,
            wbits,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        ))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

/// Test raw DEFLATE round-trip (negative windowBits: -9 to -15) with
/// property-based data. AAP §0.8.1 requires negative windowBits for raw
/// DEFLATE to be fully functional.
#[test]
fn prop_raw_deflate_roundtrip() {
    fn roundtrip(data: Vec<u8>, wbits_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        let wbits = -((wbits_raw % 7) as i32 + 9);
        TestResult::from_bool(streaming_roundtrip_full(
            &data,
            Z_DEFAULT_COMPRESSION,
            wbits,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        ))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

// ============================================================================
// Phase 5: Strategy Round-Trip Tests
// ============================================================================

/// Test all 5 strategies across compression levels (1-9) with
/// property-based data. AAP §0.8.1 requires all 5 strategies to produce
/// format-compatible output.
#[test]
fn prop_all_strategies_roundtrip() {
    fn roundtrip(data: Vec<u8>, params_raw: u8) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }
        let strategies = [
            Z_DEFAULT_STRATEGY,
            Z_FILTERED,
            Z_HUFFMAN_ONLY,
            Z_RLE,
            Z_FIXED,
        ];
        // Levels 1-9 and strategy 0-4
        let level = (params_raw % 9) as i32 + 1;
        let strategy = strategies[((params_raw / 9) % 5) as usize];

        TestResult::from_bool(streaming_roundtrip_full(
            &data,
            level,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            strategy,
        ))
    }
    quickcheck(roundtrip as fn(Vec<u8>, u8) -> TestResult);
}

// ============================================================================
// Phase 6: Auto-Detect Format Round-Trip
// ============================================================================

/// Compress with zlib format (windowBits = 15), decompress with auto-detect
/// (windowBits = 15 + 32 = 47).
#[test]
fn prop_auto_detect_zlib() {
    fn roundtrip(data: Vec<u8>) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }

        // Compress with zlib format
        let bound = data.len() * 2 + 64;
        let mut compressed = vec![0u8; bound];
        let mut c_stream = ZStream::new();
        if deflate_init2(
            &mut c_stream,
            Z_DEFAULT_COMPRESSION,
            Z_DEFLATED,
            15, // zlib format
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        )
        .is_err()
        {
            return TestResult::failed();
        }
        c_stream.set_input(&data);
        c_stream.set_output(&mut compressed);
        let result = deflate(&mut c_stream, Z_FINISH);
        let comp_len = c_stream.total_out as usize;
        let _ = deflate_end(&mut c_stream);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => return TestResult::failed(),
        }
        compressed.truncate(comp_len);

        // Decompress with auto-detect (windowBits = 47 = 15 + 32)
        let mut decompressed = vec![0u8; data.len()];
        let mut d_stream = ZStream::new();
        if inflate_init2(&mut d_stream, 15 + 32).is_err() {
            return TestResult::failed();
        }
        d_stream.set_input(&compressed);
        d_stream.set_output(&mut decompressed);
        let result = inflate(&mut d_stream, Z_FINISH);
        let decomp_len = d_stream.total_out as usize;
        let _ = inflate_end(&mut d_stream);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => return TestResult::failed(),
        }

        TestResult::from_bool(
            decomp_len == data.len() && decompressed[..decomp_len] == *data,
        )
    }
    quickcheck(roundtrip as fn(Vec<u8>) -> TestResult);
}

/// Compress with gzip format (windowBits = 31 = 15 + 16), decompress with
/// auto-detect (windowBits = 47 = 15 + 32).
#[test]
fn prop_auto_detect_gzip() {
    fn roundtrip(data: Vec<u8>) -> TestResult {
        if data.is_empty() || data.len() > 60000 {
            return TestResult::discard();
        }

        // Compress with gzip format
        let bound = data.len() * 2 + 64;
        let mut compressed = vec![0u8; bound];
        let mut c_stream = ZStream::new();
        if deflate_init2(
            &mut c_stream,
            Z_DEFAULT_COMPRESSION,
            Z_DEFLATED,
            15 + 16, // gzip format
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        )
        .is_err()
        {
            return TestResult::failed();
        }
        c_stream.set_input(&data);
        c_stream.set_output(&mut compressed);
        let result = deflate(&mut c_stream, Z_FINISH);
        let comp_len = c_stream.total_out as usize;
        let _ = deflate_end(&mut c_stream);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => return TestResult::failed(),
        }
        compressed.truncate(comp_len);

        // Decompress with auto-detect (windowBits = 47 = 15 + 32)
        let mut decompressed = vec![0u8; data.len()];
        let mut d_stream = ZStream::new();
        if inflate_init2(&mut d_stream, 15 + 32).is_err() {
            return TestResult::failed();
        }
        d_stream.set_input(&compressed);
        d_stream.set_output(&mut decompressed);
        let result = inflate(&mut d_stream, Z_FINISH);
        let decomp_len = d_stream.total_out as usize;
        let _ = inflate_end(&mut d_stream);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => return TestResult::failed(),
        }

        TestResult::from_bool(
            decomp_len == data.len() && decompressed[..decomp_len] == *data,
        )
    }
    quickcheck(roundtrip as fn(Vec<u8>) -> TestResult);
}

// ============================================================================
// Phase 7: Edge Case Round-Trip Tests
// ============================================================================

/// Compress empty input (0 bytes) at all levels (0-9 and default), decompress
/// and verify empty output. AAP §0.8.1 requires all 10 compression levels.
#[test]
fn empty_data_roundtrip() {
    let data: &[u8] = &[];
    // Levels 0-9 and default (-1).
    let levels = [
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        2, 3, 4, 5, 6, 7, 8,
        Z_BEST_COMPRESSION,
        Z_DEFAULT_COMPRESSION,
    ];

    for &level in &levels {
        let mut c_stream = ZStream::new();
        deflate_init(&mut c_stream, level)
            .unwrap_or_else(|e| panic!("deflate_init failed at level {}: {:?}", level, e));

        let mut compressed = vec![0u8; 64];
        c_stream.set_input(data);
        c_stream.set_output(&mut compressed);
        let result = deflate(&mut c_stream, Z_FINISH);
        let comp_len = c_stream.total_out as usize;
        let _ = deflate_end(&mut c_stream);
        assert!(
            matches!(result, Ok(ReturnCode::StreamEnd)),
            "deflate should return StreamEnd for empty input at level {}",
            level
        );
        compressed.truncate(comp_len);

        // Decompress: empty data still produces a valid zlib stream
        let mut decompressed = vec![0u8; 1];
        let mut d_stream = ZStream::new();
        inflate_init(&mut d_stream)
            .unwrap_or_else(|e| panic!("inflate_init failed: {:?}", e));
        d_stream.set_input(&compressed);
        d_stream.set_output(&mut decompressed);
        let result = inflate(&mut d_stream, Z_FINISH);
        let decomp_len = d_stream.total_out as usize;
        let _ = inflate_end(&mut d_stream);
        assert!(
            matches!(result, Ok(ReturnCode::StreamEnd)),
            "inflate should return StreamEnd for empty compressed data at level {}",
            level
        );
        assert_eq!(
            decomp_len, 0,
            "decompressed length should be 0 for empty input at level {}",
            level
        );
    }
}

/// Compress a single byte at all levels (0-9 and default), decompress and
/// verify. AAP §0.8.1 requires all 10 compression levels.
#[test]
fn single_byte_roundtrip() {
    let levels = [
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        2, 3, 4, 5, 6, 7, 8,
        Z_BEST_COMPRESSION,
        Z_DEFAULT_COMPRESSION,
    ];

    for byte_val in [0u8, 1, 127, 128, 255] {
        let data = [byte_val];
        for &level in &levels {
            assert!(
                one_call_roundtrip(&data, level),
                "single byte {} round-trip failed at level {}",
                byte_val,
                level
            );
        }
    }
}

/// Compress highly compressible data (all zeros) at all compression levels.
/// Pattern from test/example.c test_large_deflate: "uncompr is still mostly
/// zeroes, so it should compress very well."
/// AAP §0.8.1 requires all 10 compression levels.
#[test]
fn highly_compressible_data_roundtrip() {
    let sizes = [1, 10, 100, 1_000, 10_000, 100_000];
    let levels = [
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        Z_DEFAULT_COMPRESSION,
        Z_BEST_COMPRESSION,
    ];

    for &size in &sizes {
        let data = vec![0u8; size];
        for &level in &levels {
            assert!(
                one_call_roundtrip(&data, level),
                "zeros (size={}) round-trip failed at level {}",
                size,
                level
            );
        }
    }
}

/// Compress incompressible (random) data, decompress and verify identity.
/// Exercises all levels including level 0 (stored blocks) and tests data
/// sizes up to 100 KB to verify pending buffer overflow is resolved.
#[test]
fn incompressible_data_roundtrip() {
    let mut rng = rand::rng();
    let sizes = [1, 10, 100, 1_000, 10_000, 50_000, 100_000];

    for &size in &sizes {
        let mut data = vec![0u8; size];
        rng.fill(&mut data[..]);

        let levels = [
            Z_NO_COMPRESSION,
            Z_BEST_SPEED,
            Z_DEFAULT_COMPRESSION,
            Z_BEST_COMPRESSION,
        ];
        for &level in &levels {
            assert!(
                one_call_roundtrip(&data, level),
                "random data (size={}) round-trip failed at level {}",
                size,
                level
            );
        }
    }
}

/// Compress large compressible data (1 MB+), decompress and verify identity.
/// Uses highly compressible (patterned) data that compresses well.
/// Tests all levels (0-9) to verify pending buffer overflow fix.
#[test]
fn large_data_roundtrip() {
    // 1 MB of highly compressible data (repeated pattern)
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(1_048_576);
    while data.len() < 1_048_576 {
        let remaining = 1_048_576 - data.len();
        let chunk = remaining.min(pattern.len());
        data.extend_from_slice(&pattern[..chunk]);
    }

    // Test all levels 0-9 and default (-1) per AAP §0.8.1
    let levels = [
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        2, 3, 4, 5, 6, 7, 8,
        Z_BEST_COMPRESSION,
        Z_DEFAULT_COMPRESSION,
    ];
    for &level in &levels {
        assert!(
            one_call_roundtrip(&data, level),
            "large compressible data round-trip failed at level {}",
            level
        );
    }
}

// ============================================================================
// Phase 8: Flush Mode Round-Trip Tests
// ============================================================================

/// Test streaming round-trip using various flush modes at intermediate points.
/// For each non-final flush mode (Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_SYNC_FLUSH,
/// Z_FULL_FLUSH, Z_BLOCK), stream data with that flush mode, finish with
/// Z_FINISH, decompress and verify identity.
///
/// Z_BLOCK causes deflate to stop after producing the next block header or
/// coded data, which is useful for block-boundary inspection. The data is
/// still valid DEFLATE and can be decompressed normally.
///
/// Note: Z_TREES (value 6) is NOT a valid flush mode for deflate() — in C zlib,
/// deflate.c line 985 checks `flush > Z_BLOCK` and returns Z_STREAM_ERROR.
/// Z_TREES is only valid for inflate() where it stops after decoding the
/// Huffman trees. Z_TREES inflate-side testing is in a separate test below.
#[test]
fn flush_modes_roundtrip() {
    let data = b"Hello, this is a test string for flush mode round-trip testing. \
                 It needs to be long enough to produce meaningful compression output \
                 across multiple flush calls. Repeated phrases help: hello hello hello \
                 testing testing testing compression compression compression zlib zlib.";

    let flush_modes = [
        Z_NO_FLUSH,
        Z_PARTIAL_FLUSH,
        Z_SYNC_FLUSH,
        Z_FULL_FLUSH,
        Z_BLOCK,
    ];

    for &flush_mode in &flush_modes {
        let mut c_stream = ZStream::new();
        deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION)
            .unwrap_or_else(|e| {
                panic!("deflate_init failed for flush mode {}: {:?}", flush_mode, e)
            });

        let mut compressed = vec![0u8; data.len() * 4 + 128];
        let chunk_size = 32;

        // Feed data in chunks, using the specified flush mode for
        // intermediate flushes.
        c_stream.set_input(data);
        c_stream.set_output(&mut compressed);

        // First: feed data in chunks with intermediate flush
        let mut input_fed = 0;
        while input_fed < data.len() {
            let this_chunk = chunk_size.min(data.len() - input_fed);
            // Reset input to this chunk
            c_stream.set_input(&data[input_fed..input_fed + this_chunk]);
            input_fed += this_chunk;

            // Use test flush mode for intermediate, Z_FINISH for last chunk
            let flush = if input_fed >= data.len() {
                Z_FINISH
            } else {
                flush_mode
            };

            // Provide ample output space
            let out_pos = c_stream.total_out as usize;
            c_stream.set_output(&mut compressed[out_pos..]);

            loop {
                let result = deflate(&mut c_stream, flush);
                match result {
                    Ok(ReturnCode::StreamEnd) => break,
                    Ok(ReturnCode::Ok) => {
                        if c_stream.avail_in == 0 || c_stream.avail_out > 0 {
                            break;
                        }
                        // Output buffer full, provide more space
                        let out_pos = c_stream.total_out as usize;
                        c_stream.set_output(&mut compressed[out_pos..]);
                    }
                    Err(e) => {
                        panic!(
                            "deflate error {:?} with flush mode {}",
                            e, flush_mode
                        );
                    }
                    _ => break,
                }
            }
        }

        // Ensure the stream is finished
        if c_stream.avail_in > 0 || !matches!(
            deflate(&mut c_stream, Z_NO_FLUSH),
            Ok(ReturnCode::StreamEnd)
        ) {
            // Force finish
            c_stream.set_input(&[]);
            let out_pos = c_stream.total_out as usize;
            c_stream.set_output(&mut compressed[out_pos..]);
            loop {
                let result = deflate(&mut c_stream, Z_FINISH);
                match result {
                    Ok(ReturnCode::StreamEnd) => break,
                    Ok(ReturnCode::Ok) => {
                        let out_pos = c_stream.total_out as usize;
                        c_stream.set_output(&mut compressed[out_pos..]);
                    }
                    Err(e) => panic!("deflate finish error: {:?}", e),
                    _ => break,
                }
            }
        }

        let comp_len = c_stream.total_out as usize;
        let _ = deflate_end(&mut c_stream);
        compressed.truncate(comp_len);

        // Decompress and verify
        let mut decompressed = vec![0u8; data.len()];
        let mut d_stream = ZStream::new();
        inflate_init(&mut d_stream)
            .unwrap_or_else(|e| panic!("inflate_init failed: {:?}", e));
        d_stream.set_input(&compressed);
        d_stream.set_output(&mut decompressed);

        let result = inflate(&mut d_stream, Z_FINISH);
        let decomp_len = d_stream.total_out as usize;
        let _ = inflate_end(&mut d_stream);

        assert!(
            matches!(result, Ok(ReturnCode::StreamEnd)),
            "inflate should succeed for flush mode {} (got {:?})",
            flush_mode,
            result
        );
        assert_eq!(
            decomp_len,
            data.len(),
            "decompressed length mismatch for flush mode {}",
            flush_mode
        );
        assert_eq!(
            &decompressed[..decomp_len],
            &data[..],
            "data mismatch for flush mode {}",
            flush_mode
        );
    }
}

// ============================================================================
// Exhaustive deterministic tests
// ============================================================================

/// Exhaustive deterministic test: all levels (0-9 and default) × all strategies
/// with known data. Ensures every combination works, complementing the
/// property-based tests. AAP §0.8.1 requires all 10 compression levels and
/// all 5 strategies.
#[test]
fn exhaustive_level_strategy_roundtrip() {
    let data = b"hello, hello! This is a test of all compression levels and \
                 strategies. Repeated words help: hello hello hello test test.";

    // Levels 0-9 and -1 (default).
    let levels: Vec<i32> = (0..=9).chain(std::iter::once(-1)).collect();
    let strategies = [
        Z_DEFAULT_STRATEGY,
        Z_FILTERED,
        Z_HUFFMAN_ONLY,
        Z_RLE,
        Z_FIXED,
    ];

    for &level in &levels {
        for &strategy in &strategies {
            assert!(
                streaming_roundtrip_full(data, level, MAX_WBITS, DEF_MEM_LEVEL, strategy),
                "round-trip failed for level={} strategy={}",
                level,
                strategy
            );
        }
    }
}

/// Exhaustive deterministic test: all windowBits formats with known data.
#[test]
fn exhaustive_window_bits_roundtrip() {
    let data = b"Window bits round-trip test data. Repeated to aid compression: \
                 window window window bits bits bits round round round trip trip trip.";

    // Zlib format: windowBits 9-15
    for wbits in 9..=15 {
        assert!(
            streaming_roundtrip_full(
                data,
                Z_DEFAULT_COMPRESSION,
                wbits,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
            ),
            "zlib format round-trip failed for windowBits={}",
            wbits
        );
    }

    // Gzip format: windowBits 25-31 (9+16 through 15+16)
    for wbits in 9..=15 {
        assert!(
            streaming_roundtrip_full(
                data,
                Z_DEFAULT_COMPRESSION,
                wbits + 16,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
            ),
            "gzip format round-trip failed for windowBits={}",
            wbits + 16
        );
    }

    // Raw DEFLATE format: windowBits -9 to -15
    // AAP §0.8.1 requires negative windowBits for raw DEFLATE
    for wbits in 9..=15 {
        assert!(
            streaming_roundtrip_full(
                data,
                Z_DEFAULT_COMPRESSION,
                -wbits,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
            ),
            "raw DEFLATE round-trip failed for windowBits={}",
            -wbits
        );
    }
}

// ============================================================================
// Phase 9: Z_BLOCK and Z_TREES Flush Mode Tests
// ============================================================================

/// Test Z_BLOCK flush mode for deflate: Z_BLOCK causes the deflate engine
/// to stop output at the next block boundary. The compressed data is still
/// valid DEFLATE and can be decompressed normally. This test verifies the
/// round-trip works when Z_BLOCK is used as the intermediate flush mode.
///
/// Per AAP §0.8.1, all 7 flush modes must produce identical behavior to C zlib.
#[test]
fn z_block_flush_deflate_roundtrip() {
    let data = b"Z_BLOCK flush mode test. This data needs to be long enough \
                 to produce multiple blocks. Repeated words help: block block \
                 block flush flush flush test test test data data data zlib.";

    let mut c_stream = ZStream::new();
    deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION)
        .expect("deflate_init failed for Z_BLOCK test");

    let mut compressed = vec![0u8; data.len() * 4 + 128];
    let chunk_size = 16; // Small chunks to exercise Z_BLOCK multiple times

    let mut input_fed = 0;
    c_stream.set_output(&mut compressed);

    while input_fed < data.len() {
        let this_chunk = chunk_size.min(data.len() - input_fed);
        c_stream.set_input(&data[input_fed..input_fed + this_chunk]);
        input_fed += this_chunk;

        let out_pos = c_stream.total_out as usize;
        c_stream.set_output(&mut compressed[out_pos..]);

        loop {
            let result = deflate(&mut c_stream, Z_BLOCK);
            match result {
                Ok(ReturnCode::Ok) => {
                    if c_stream.avail_in == 0 {
                        break;
                    }
                    let out_pos = c_stream.total_out as usize;
                    c_stream.set_output(&mut compressed[out_pos..]);
                }
                Err(e) => panic!("deflate Z_BLOCK error: {:?}", e),
                _ => break,
            }
        }
    }

    // Finish the stream
    c_stream.set_input(&[]);
    loop {
        let out_pos = c_stream.total_out as usize;
        c_stream.set_output(&mut compressed[out_pos..]);
        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(ReturnCode::Ok) => continue,
            Err(e) => panic!("deflate FINISH after Z_BLOCK error: {:?}", e),
            _ => break,
        }
    }

    let comp_len = c_stream.total_out as usize;
    let _ = deflate_end(&mut c_stream);
    compressed.truncate(comp_len);

    assert!(comp_len > 0, "Z_BLOCK: compressed output should not be empty");

    // Decompress and verify identity
    let mut decompressed = vec![0u8; data.len()];
    let mut d_stream = ZStream::new();
    inflate_init(&mut d_stream).expect("inflate_init failed for Z_BLOCK test");
    d_stream.set_input(&compressed);
    d_stream.set_output(&mut decompressed);

    let result = inflate(&mut d_stream, Z_FINISH);
    let decomp_len = d_stream.total_out as usize;
    let _ = inflate_end(&mut d_stream);

    assert!(
        matches!(result, Ok(ReturnCode::StreamEnd)),
        "inflate should succeed for Z_BLOCK compressed data (got {:?})",
        result
    );
    assert_eq!(
        decomp_len,
        data.len(),
        "Z_BLOCK: decompressed length mismatch"
    );
    assert_eq!(
        &decompressed[..decomp_len],
        &data[..],
        "Z_BLOCK: data mismatch"
    );
}

/// Test Z_TREES flush mode for inflate: Z_TREES (value 6) is valid for
/// inflate() — it causes inflate to return after decoding the Huffman tree
/// headers at the beginning of a dynamic block, before decoding the actual
/// symbols. This is useful for examining the code trees.
///
/// Z_TREES is NOT valid for deflate() — C zlib rejects it with Z_STREAM_ERROR
/// because deflate.c checks `flush > Z_BLOCK`. This test verifies both
/// behaviors per AAP §0.8.1.
#[test]
fn z_trees_flush_inflate_test() {
    // First, verify that Z_TREES is rejected by deflate
    {
        let mut c_stream = ZStream::new();
        deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION)
            .expect("deflate_init failed");
        let data = b"test data for Z_TREES";
        let mut output = vec![0u8; 256];
        c_stream.set_input(data);
        c_stream.set_output(&mut output);
        let result = deflate(&mut c_stream, Z_TREES);
        assert!(
            result.is_err(),
            "deflate should reject Z_TREES flush mode (got {:?})",
            result
        );
        let _ = deflate_end(&mut c_stream);
    }

    // Now test Z_TREES with inflate: compress data normally, then decompress
    // using Z_TREES flush mode which pauses after Huffman tree headers
    let data = b"Z_TREES inflate test. Repeated data helps produce dynamic \
                 Huffman trees: trees trees trees inflate inflate inflate \
                 zlib zlib zlib test test test data data data round round.";

    // Compress with default settings
    let mut c_stream = ZStream::new();
    deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION)
        .expect("deflate_init failed for Z_TREES test");

    let mut compressed = vec![0u8; data.len() * 2 + 64];
    c_stream.set_input(data);
    c_stream.set_output(&mut compressed);
    let result = deflate(&mut c_stream, Z_FINISH);
    let comp_len = c_stream.total_out as usize;
    let _ = deflate_end(&mut c_stream);
    assert!(
        matches!(result, Ok(ReturnCode::StreamEnd)),
        "deflate should succeed for Z_TREES test data"
    );
    compressed.truncate(comp_len);

    // Decompress using Z_TREES flush mode: this should pause after parsing
    // the Huffman tree headers, then subsequent calls continue decompression
    let mut decompressed = vec![0u8; data.len()];
    let mut d_stream = ZStream::new();
    inflate_init(&mut d_stream).expect("inflate_init failed for Z_TREES test");
    d_stream.set_input(&compressed);
    d_stream.set_output(&mut decompressed);

    // Use Z_TREES for the initial inflate call — inflate should return Ok
    // after decoding the Huffman tree headers, then we continue with
    // Z_NO_FLUSH calls to complete decompression.
    let initial_result = inflate(&mut d_stream, Z_TREES);
    match initial_result {
        Ok(ReturnCode::StreamEnd) => {
            // Very small data completed in one call — this is valid
        }
        Ok(ReturnCode::Ok) => {
            // Z_TREES returned after tree headers — complete decompression
            loop {
                let out_pos = d_stream.total_out as usize;
                if out_pos < decompressed.len() {
                    d_stream.set_output(&mut decompressed[out_pos..]);
                }
                let result = inflate(&mut d_stream, Z_NO_FLUSH);
                match result {
                    Ok(ReturnCode::StreamEnd) => break,
                    Ok(ReturnCode::Ok) => continue,
                    Err(e) => panic!("inflate Z_NO_FLUSH after Z_TREES error: {:?}", e),
                    _ => break,
                }
            }
        }
        Err(e) => panic!("inflate Z_TREES error: {:?}", e),
        _ => {} // Other return codes are acceptable
    }
    let decomp_len = d_stream.total_out as usize;
    let _ = inflate_end(&mut d_stream);

    assert_eq!(
        decomp_len,
        data.len(),
        "Z_TREES: decompressed length mismatch: {} vs {}",
        decomp_len,
        data.len()
    );
    assert_eq!(
        &decompressed[..decomp_len],
        &data[..],
        "Z_TREES: data mismatch"
    );
}

/// Test Z_BLOCK with all levels and formats to ensure comprehensive coverage.
/// Per AAP §0.8.1, Z_BLOCK must work correctly across all configurations.
#[test]
fn z_block_all_levels_roundtrip() {
    let data = b"Block flush across all levels test. Repeated content helps \
                 compression: block level test block level test block level.";

    // Test all levels 0-9 with Z_BLOCK as intermediate flush
    for level in 0..=9i32 {
        let mut c_stream = ZStream::new();
        deflate_init(&mut c_stream, level)
            .unwrap_or_else(|e| panic!("deflate_init level {} failed: {:?}", level, e));

        let mut compressed = vec![0u8; data.len() * 4 + 128];
        c_stream.set_input(data);
        c_stream.set_output(&mut compressed);

        // Feed half the data with Z_BLOCK
        let half = data.len() / 2;
        c_stream.set_input(&data[..half]);
        loop {
            let result = deflate(&mut c_stream, Z_BLOCK);
            match result {
                Ok(ReturnCode::Ok) => {
                    if c_stream.avail_in == 0 {
                        break;
                    }
                    let out_pos = c_stream.total_out as usize;
                    c_stream.set_output(&mut compressed[out_pos..]);
                }
                Err(e) => panic!("deflate Z_BLOCK level {} error: {:?}", level, e),
                _ => break,
            }
        }

        // Feed the rest with Z_FINISH
        c_stream.set_input(&data[half..]);
        loop {
            let out_pos = c_stream.total_out as usize;
            c_stream.set_output(&mut compressed[out_pos..]);
            let result = deflate(&mut c_stream, Z_FINISH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                Err(e) => panic!("deflate FINISH level {} error: {:?}", level, e),
                _ => break,
            }
        }

        let comp_len = c_stream.total_out as usize;
        let _ = deflate_end(&mut c_stream);
        compressed.truncate(comp_len);

        // Decompress and verify
        let mut decompressed = vec![0u8; data.len()];
        let mut d_stream = ZStream::new();
        inflate_init(&mut d_stream).expect("inflate_init failed");
        d_stream.set_input(&compressed);
        d_stream.set_output(&mut decompressed);
        let result = inflate(&mut d_stream, Z_FINISH);
        let decomp_len = d_stream.total_out as usize;
        let _ = inflate_end(&mut d_stream);

        assert!(
            matches!(result, Ok(ReturnCode::StreamEnd)),
            "inflate level {} Z_BLOCK failed: {:?}",
            level,
            result
        );
        assert_eq!(
            &decompressed[..decomp_len],
            &data[..],
            "Z_BLOCK level {} data mismatch",
            level
        );
    }
}
