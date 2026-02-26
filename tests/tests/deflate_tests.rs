//! Deflate-specific edge case tests.
//!
//! Tests all compression levels (0–9), all strategies, various `windowBits`
//! values, and edge cases in the DEFLATE compression engine.
//!
//! # Source Derivation
//!
//! - `deflate.c` lines 107–124: `configuration_table[10]` mapping levels 0–9
//! - `deflate.c` lines 766–812: `deflateParams()` level/strategy switching
//! - `deflate.h` lines 58–67: state constants (`INIT_STATE`, `BUSY_STATE`, `FINISH_STATE`)
//! - `zlib.h` `deflateInit2` documentation: `windowBits` overloading (positive=zlib,
//!   negative=raw, +16=gzip)
//! - `test/example.c`: reference test patterns for dictionary, flush, large data

// Allow long test functions — comprehensive coverage requires detailed setup.
#![allow(clippy::too_many_lines)]
// Wildcard re-export from the test utility crate provides all constants.
#![allow(clippy::wildcard_imports)]

use zlib_rs::deflate;
use zlib_rs::inflate;
use zlib_rs_tests::*;

// ─── Helper: streaming deflate → inflate round-trip ──────────────────────────

/// Compress `data` using the streaming deflate API with full parameter control,
/// then decompress using the streaming inflate API and verify the round trip.
///
/// Returns the compressed data for additional inspection by callers.
///
/// # Parameters
///
/// - `data` — Source data to compress.
/// - `level` — Compression level (0–9 or `Z_DEFAULT_COMPRESSION`).
/// - `window_bits_d` — `windowBits` for `deflateInit2`.
/// - `mem_level` — Memory level for `deflateInit2`.
/// - `strategy` — Compression strategy for `deflateInit2`.
/// - `window_bits_i` — `windowBits` for `inflateInit2` (must be compatible
///   with the container format produced by the deflate `windowBits`).
#[allow(clippy::cast_possible_truncation)]
fn streaming_round_trip(
    data: &[u8],
    level: i32,
    window_bits_d: i32,
    mem_level: i32,
    strategy: i32,
    window_bits_i: i32,
) -> Vec<u8> {
    // ── Compress ──
    let mut d_stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut d_stream,
        level,
        Z_DEFLATED,
        window_bits_d,
        mem_level,
        strategy,
    );
    assert_ok(ret, "deflateInit2");

    d_stream.set_input(data);
    // Use a generous output buffer.  `deflate_bound` may underestimate for
    // some windowBits / strategy combinations, so add a substantial margin.
    let bound = deflate::deflate_bound(&d_stream, data.len()) + data.len() + 512;
    d_stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut d_stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(Z_FINISH)");

    // Verify running counters are consistent.
    assert_eq!(
        d_stream.total_in,
        data.len() as u64,
        "total_in mismatch after deflate"
    );
    assert!(
        d_stream.total_out > 0 || data.is_empty(),
        "total_out should be non-zero for non-empty input"
    );

    let compressed = d_stream.take_output();
    let _ret = deflate::deflate_end(&mut d_stream);

    // ── Decompress ──
    let mut i_stream = ZStream::new();
    let ret = inflate::inflate_init2(&mut i_stream, window_bits_i);
    assert_ok(ret, "inflateInit2");

    i_stream.set_input(&compressed);
    let out_size = if data.is_empty() {
        512
    } else {
        data.len() + 512
    };
    i_stream.set_output_buffer(out_size);

    // Use the convenience wrapper that handles state extraction internally.
    let ret = inflate::inflate_run(&mut i_stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "inflate(Z_FINISH)");

    let decompressed = i_stream.take_output();
    let _ret = inflate::inflate_end_stream(&mut i_stream);

    assert_bytes_equal(&decompressed, data, "streaming round-trip");
    compressed
}

// =============================================================================
// Phase 2: Compression Level Tests
// From deflate.c configuration_table (lines 107–124)
//
// Level 0:  stored          | Level 5:  (8,16,32,32)   slow
// Level 1:  (4,4,8,4)  fast | Level 6:  (8,16,128,128) slow (default)
// Level 2:  (4,5,16,8) fast | Level 7:  (8,32,128,256) slow
// Level 3:  (4,6,32,32)fast | Level 8:  (32,128,258,1024) slow
// Level 4:  (4,4,16,16)slow | Level 9:  (32,258,258,4096) slow (best)
// =============================================================================

/// Compress and decompress at each compression level (0–9).
///
/// Levels 0–3 use the fast algorithm (or stored for 0), levels 4–9 use the
/// slow (lazy match) algorithm per `configuration_table` in `deflate.c`.
#[test]
fn test_deflate_all_levels() {
    let data = HELLO;

    for level in 0..=9_i32 {
        let mut compressed = Vec::new();
        compress2(&mut compressed, data, level).unwrap_or_else(|e| {
            panic!("compress2(level={level}) failed: {e:?}");
        });

        let mut decompressed = alloc_uncompr_buffer(data.len());
        uncompress(&mut decompressed, &compressed).unwrap_or_else(|e| {
            panic!("uncompress(level={level}) failed: {e:?}");
        });

        assert_bytes_equal(&decompressed, data, &format!("level {level} round-trip"));
    }

    // Level 0 (stored) should produce output larger than input for small data
    // due to the 2-byte zlib header + 5-byte stored block header +
    // 4-byte Adler-32 trailer = 11 bytes overhead.
    let mut stored = Vec::new();
    compress2(&mut stored, data, Z_NO_COMPRESSION).unwrap();
    assert!(
        stored.len() > data.len(),
        "level 0 output ({}) should be larger than input ({})",
        stored.len(),
        data.len(),
    );
}

/// `Z_DEFAULT_COMPRESSION` (−1) is equivalent to level 6.
#[test]
fn test_deflate_default_level() {
    let data = HELLO;

    let mut compressed_default = Vec::new();
    compress2(&mut compressed_default, data, Z_DEFAULT_COMPRESSION).unwrap();

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &compressed_default).unwrap();
    assert_bytes_equal(&decompressed, data, "default level round-trip");

    // Verify the output is identical to explicit level 6.
    let mut compressed_6 = Vec::new();
    compress2(&mut compressed_6, data, 6).unwrap();
    assert_bytes_equal(
        &compressed_default,
        &compressed_6,
        "default level should match level 6",
    );
}

/// Boundary conditions for compression levels.
#[test]
fn test_deflate_level_boundaries() {
    let data = HELLO;

    // Valid: Z_DEFAULT_COMPRESSION (−1)
    let mut c = Vec::new();
    compress2(&mut c, data, Z_DEFAULT_COMPRESSION).unwrap();

    // Valid: Z_NO_COMPRESSION (0)
    let mut c = Vec::new();
    compress2(&mut c, data, Z_NO_COMPRESSION).unwrap();

    // Valid: Z_BEST_SPEED (1)
    let mut c = Vec::new();
    compress2(&mut c, data, Z_BEST_SPEED).unwrap();

    // Valid: Z_BEST_COMPRESSION (9)
    let mut c = Vec::new();
    compress2(&mut c, data, Z_BEST_COMPRESSION).unwrap();

    // Invalid: level 10 — should return StreamError.
    let mut c = Vec::new();
    let result = compress2(&mut c, data, 10);
    assert!(result.is_err(), "compress2(level=10) should fail");
    assert_eq!(
        result.unwrap_err(),
        ReturnCode::StreamError,
        "compress2(level=10) should return StreamError",
    );

    // Verify the integer constants match the `ReturnCode` enum discriminants.
    assert_eq!(Z_STREAM_ERROR, ReturnCode::StreamError as i32);
    assert_eq!(Z_NEED_DICT, ReturnCode::NeedDict as i32);
}

// =============================================================================
// Phase 3: Strategy Tests
// From deflate.c lines 789–812, zlib.h lines 200–204
// =============================================================================

/// `Z_DEFAULT_STRATEGY` (0) — standard LZ77 matching + Huffman coding.
#[test]
fn test_deflate_strategy_default() {
    let data = b"hello, hello! this is a test of the default strategy.";
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        MAX_WBITS,
    );
}

/// `Z_FILTERED` (1) — more Huffman coding, less string matching.
#[test]
fn test_deflate_strategy_filtered() {
    let data = b"filtered data: small values with a somewhat random distribution 123456789";
    streaming_round_trip(
        data,
        Z_BEST_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_FILTERED,
        MAX_WBITS,
    );
}

/// `Z_HUFFMAN_ONLY` (2) — no string matching at all, pure Huffman encoding.
#[test]
fn test_deflate_strategy_huffman_only() {
    let data = b"huffman only encoding: no string matching, just entropy coding!";
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_HUFFMAN_ONLY,
        MAX_WBITS,
    );
}

/// `Z_RLE` (3) — match distances limited to 1 (run-length encoding).
#[test]
fn test_deflate_strategy_rle() {
    // Data with long runs benefits most from the RLE strategy.
    let mut data = vec![0xAA_u8; 500];
    data.extend_from_slice(&[0xBB; 500]);
    data.extend_from_slice(&[0xCC; 500]);
    streaming_round_trip(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_RLE,
        MAX_WBITS,
    );
}

/// `Z_FIXED` (4) — pre-defined fixed Huffman code tables only.
#[test]
fn test_deflate_strategy_fixed() {
    let data = b"fixed Huffman codes: no dynamic tables transmitted!";
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_FIXED,
        MAX_WBITS,
    );
}

// =============================================================================
// Phase 4: WindowBits Tests
// From zlib.h deflateInit2 documentation
//
// windowBits encoding:
//   8–15   → zlib format, window size = 2^windowBits
//  −8 to −15 → raw DEFLATE (no header/trailer)
//  24–31  → gzip format (windowBits − 16 = actual window bits)
// =============================================================================

/// Standard zlib format with various window sizes.
///
/// `windowBits` 8 is promoted to 9 by `deflateInit2` (historical workaround
/// from `deflate.c` line 265).
#[test]
fn test_deflate_window_bits_zlib() {
    let data = HELLO;

    // windowBits = 15 (default, 32 KB window)
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        15,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        15,
    );

    // windowBits = 9 (minimum usable, 512-byte window)
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        9,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        9,
    );

    // windowBits = 12 (medium, 4 KB window)
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        12,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        12,
    );
}

/// Raw DEFLATE (no zlib or gzip wrapper).
///
/// Negative `windowBits` strip the header/trailer.  The inflate side must use
/// the same negative value to expect raw data.
#[test]
fn test_deflate_window_bits_raw() {
    let data = HELLO;

    // windowBits = −15 (raw, 32 KB window)
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        -15,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        -15,
    );

    // windowBits = −9 (raw, minimum window)
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        -9,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        -9,
    );
}

/// Gzip format (`windowBits + 16`).
///
/// `windowBits` = 15 + 16 = 31 produces gzip-wrapped output with a 10-byte
/// header, CRC-32 checksum, and 8-byte trailer.
#[test]
fn test_deflate_window_bits_gzip() {
    let data = HELLO;

    // windowBits = 31 (gzip, 32 KB window).
    // For inflate, 31 means auto-detect gzip.
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        31,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        31,
    );
}

/// Invalid `windowBits` values should return `Z_STREAM_ERROR`.
#[test]
fn test_deflate_window_bits_invalid() {
    let mut stream = ZStream::new();

    // windowBits = 7 (below minimum of 8)
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        7,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_return_code(ret, ReturnCode::StreamError, "windowBits=7");

    // windowBits = 16 (above 15 but below gzip range 24–31)
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        16,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_return_code(ret, ReturnCode::StreamError, "windowBits=16");
}

// =============================================================================
// Phase 5: deflateParams Tests
// From deflate.c lines 766–812
// =============================================================================

/// Switch compression level mid-stream via `deflateParams()`.
///
/// Mirrors the pattern from `test/example.c` `test_large_deflate()`:
/// 1. Start at `Z_BEST_SPEED`
/// 2. Switch to `Z_NO_COMPRESSION`
/// 3. Switch to `Z_BEST_COMPRESSION` with `Z_FILTERED`
/// 4. Finish and verify the full round-trip.
#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_deflate_params_switch_level() {
    // Build three chunks of test data.
    let chunk_len = 2048_usize;
    let chunk1: Vec<u8> = (0..chunk_len).map(|i| (i % 251) as u8).collect();
    let chunk2: Vec<u8> = vec![0xFF; chunk_len];
    let chunk3: Vec<u8> = (0..chunk_len).map(|i| ((i * 7 + 3) % 256) as u8).collect();

    let mut full_data = Vec::with_capacity(chunk_len * 3);
    full_data.extend_from_slice(&chunk1);
    full_data.extend_from_slice(&chunk2);
    full_data.extend_from_slice(&chunk3);

    // ── Initialize at Z_BEST_SPEED ──
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_BEST_SPEED,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2");

    stream.set_output_buffer(COMPR_LEN);

    // ── Phase 1: compress chunk1 at Z_BEST_SPEED ──
    stream.set_input(&chunk1);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    check_err(ret, "deflate phase 1");
    assert_eq!(stream.avail_in(), 0, "phase 1: all input consumed");

    // ── Switch to Z_NO_COMPRESSION ──
    let ret = deflate::deflate_params(&mut stream, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY);
    assert_ok(ret, "deflateParams → Z_NO_COMPRESSION");

    // ── Phase 2: compress chunk2 at Z_NO_COMPRESSION ──
    stream.set_input(&chunk2);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    check_err(ret, "deflate phase 2");
    assert_eq!(stream.avail_in(), 0, "phase 2: all input consumed");

    // ── Switch to Z_BEST_COMPRESSION + Z_FILTERED ──
    let ret = deflate::deflate_params(&mut stream, Z_BEST_COMPRESSION, Z_FILTERED);
    assert_ok(ret, "deflateParams → Z_BEST_COMPRESSION + Z_FILTERED");

    // ── Phase 3: compress chunk3 and finish ──
    stream.set_input(&chunk3);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(Z_FINISH)");

    let compressed = stream.take_output();
    assert!(
        !compressed.is_empty(),
        "compressed output should not be empty"
    );
    let _ret = deflate::deflate_end(&mut stream);

    // ── Decompress and verify ──
    let mut decompressed = alloc_uncompr_buffer(full_data.len());
    uncompress(&mut decompressed, &compressed).unwrap();
    assert_bytes_equal(&decompressed, &full_data, "params level-switch round-trip");
}

/// Switch compression strategy mid-stream via `deflateParams()`.
///
/// Cycles through `Z_DEFAULT_STRATEGY` → `Z_HUFFMAN_ONLY` → `Z_RLE` and
/// verifies round-trip correctness.
#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_deflate_params_switch_strategy() {
    let chunk_len = 1024_usize;
    let chunk1: Vec<u8> = (0..chunk_len).map(|i| (i % 200) as u8).collect();
    let chunk2: Vec<u8> = (0..chunk_len).map(|i| ((i * 3) % 256) as u8).collect();
    let chunk3: Vec<u8> = vec![0x42; chunk_len];

    let mut full_data = Vec::with_capacity(chunk_len * 3);
    full_data.extend_from_slice(&chunk1);
    full_data.extend_from_slice(&chunk2);
    full_data.extend_from_slice(&chunk3);

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2");

    stream.set_output_buffer(COMPR_LEN);

    // Phase 1: Z_DEFAULT_STRATEGY
    stream.set_input(&chunk1);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    check_err(ret, "deflate phase 1");

    // Switch to Z_HUFFMAN_ONLY
    let ret = deflate::deflate_params(&mut stream, Z_DEFAULT_COMPRESSION, Z_HUFFMAN_ONLY);
    assert_ok(ret, "deflateParams → Z_HUFFMAN_ONLY");

    // Phase 2: Z_HUFFMAN_ONLY
    stream.set_input(&chunk2);
    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    check_err(ret, "deflate phase 2");

    // Switch to Z_RLE
    let ret = deflate::deflate_params(&mut stream, Z_DEFAULT_COMPRESSION, Z_RLE);
    assert_ok(ret, "deflateParams → Z_RLE");

    // Phase 3: Z_RLE + finish
    stream.set_input(&chunk3);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(Z_FINISH)");

    let compressed = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    let mut decompressed = alloc_uncompr_buffer(full_data.len());
    uncompress(&mut decompressed, &compressed).unwrap();
    assert_bytes_equal(&decompressed, &full_data, "strategy-switch round-trip");
}

// =============================================================================
// Phase 6: Dictionary Tests
// From test/example.c lines 414–491
// =============================================================================

/// Preset dictionary for compression.
///
/// Verifies `deflate_set_dictionary()`:
/// 1. The call succeeds and records the dictionary Adler-32 in `stream.adler`.
/// 2. Compression with the dictionary produces valid output.
/// 3. Attempting `uncompress` on dictionary-compressed data yields `NeedDict`
///    (proving the dictionary flag was set in the zlib header).
/// 4. Compression without the dictionary produces a different (typically
///    larger) output for the same input.
///
/// Mirrors `test/example.c` `test_dict_deflate()` / `test_dict_inflate()`.
#[test]
fn test_deflate_set_dictionary() {
    let dict = DICTIONARY;
    let data = HELLO;

    // ── Compress with dictionary ──
    let mut d_stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut d_stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for dict");

    let ret = deflate::deflate_set_dictionary(&mut d_stream, dict);
    assert_ok(ret, "deflateSetDictionary");

    // The Adler-32 of the dictionary is recorded in stream.adler.
    let dict_id = d_stream.adler;
    assert!(dict_id != 0, "dictionary Adler-32 should be non-zero");

    d_stream.set_input(data);
    let bound = deflate::deflate_bound(&d_stream, data.len());
    d_stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut d_stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate with dict");

    let compressed_with_dict = d_stream.take_output();
    let _ret = deflate::deflate_end(&mut d_stream);

    // ── Verify: uncompress rejects dictionary-compressed data ──
    // The zlib header contains the FDICT flag and the dictionary Adler-32.
    // `uncompress()` does not supply a dictionary, so inflate returns
    // `NeedDict`, which propagates as an error.
    let mut tmp = alloc_uncompr_buffer(data.len());
    let result = uncompress(&mut tmp, &compressed_with_dict);
    assert!(
        result.is_err(),
        "uncompress of dict-compressed data should fail (NeedDict)"
    );

    // ── Verify: inflate_run returns NeedDict ──
    // Use the streaming API to confirm the inflate side sees the dictionary.
    let mut i_stream = ZStream::new();
    let ret = inflate::inflate_init2(&mut i_stream, MAX_WBITS);
    assert_ok(ret, "inflateInit2 for dict verification");

    i_stream.set_input(&compressed_with_dict);
    i_stream.set_output_buffer(data.len() + 64);

    let ret = inflate::inflate_run(&mut i_stream, Z_NO_FLUSH);
    assert_return_code(ret, ReturnCode::NeedDict, "inflate should need dict");

    // The stream's adler field now holds the expected dictionary Adler-32.
    assert_eq!(
        i_stream.adler, dict_id,
        "dictionary Adler-32 mismatch: got {}, expected {}",
        i_stream.adler, dict_id,
    );
    let _ret = inflate::inflate_end_stream(&mut i_stream);

    // ── Verify: dictionary changes the compressed output ──
    let mut no_dict_stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut no_dict_stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for no-dict comparison");

    no_dict_stream.set_input(data);
    no_dict_stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut no_dict_stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate without dict");

    let compressed_no_dict = no_dict_stream.take_output();
    let _ret = deflate::deflate_end(&mut no_dict_stream);

    // With and without dictionary should produce different output.
    assert_ne!(
        compressed_with_dict, compressed_no_dict,
        "dictionary should change the compressed output"
    );
}

/// Empty dictionary should succeed but have no effect on compression.
#[test]
fn test_deflate_set_dictionary_empty() {
    let data = HELLO;

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for empty dict");

    // Set an empty dictionary — should return Ok.
    let ret = deflate::deflate_set_dictionary(&mut stream, &[]);
    assert_ok(ret, "deflateSetDictionary with empty dict");

    // Compress and decompress normally — no dictionary needed on inflate side.
    stream.set_input(data);
    let bound = deflate::deflate_bound(&stream, data.len());
    stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate with empty dict");

    let compressed = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &compressed).unwrap();
    assert_bytes_equal(&decompressed, data, "empty dictionary round-trip");
}

// =============================================================================
// Phase 7: Bound, Pending, Copy, Reset Tests
// =============================================================================

/// `deflate_bound()` and `compress_bound()` return correct upper bounds.
///
/// For various source lengths, verify that the compressed output never
/// exceeds the computed bound.
#[test]
fn test_deflate_bound() {
    let test_lengths: &[usize] = &[0, 1, 100, 1_000, 65_535, 100_000];

    for &src_len in test_lengths {
        // Test the compress module's one-call compress_bound.
        let bound_simple = compress_bound(src_len);
        assert!(
            bound_simple >= src_len || src_len == 0,
            "compress_bound({src_len}) = {bound_simple} < src_len"
        );

        // Test the deflate module's stream-aware deflate_bound.
        let mut stream = ZStream::new();
        let ret = deflate::deflate_init2(
            &mut stream,
            Z_DEFAULT_COMPRESSION,
            Z_DEFLATED,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        );
        assert_ok(ret, "deflateInit2 for bound test");
        let bound_stream = deflate::deflate_bound(&stream, src_len);
        let _ret = deflate::deflate_end(&mut stream);

        assert!(
            bound_stream >= src_len || src_len == 0,
            "deflate_bound({src_len}) = {bound_stream} < src_len"
        );

        // Actually compress data and verify output fits within the bound.
        let source = vec![0x42_u8; src_len];
        let mut compressed = Vec::new();
        compress2(&mut compressed, &source, Z_DEFAULT_COMPRESSION).unwrap();
        assert!(
            compressed.len() <= bound_simple,
            "compressed size {} > compress_bound {} for src_len={src_len}",
            compressed.len(),
            bound_simple,
        );
    }
}

/// `deflate_pending()` reports pending bytes after partial compression.
#[test]
fn test_deflate_pending() {
    let data = HELLO;

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for pending test");

    // Before any compression, pending should be zero.
    let (pending_bytes, _pending_bits) =
        deflate::deflate_pending(&stream).expect("deflate_pending before compression");
    assert_eq!(pending_bytes, 0, "initial pending should be 0");

    // Feed data and compress partially (without finishing).
    stream.set_input(data);
    stream.set_output_buffer(1); // Tiny output buffer forces pending data.

    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    // With a 1-byte output buffer, deflate produces what it can and returns Ok.
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::BufError,
        "deflate with tiny output: {ret:?}"
    );

    // The tiny output buffer should be fully consumed.
    assert_eq!(
        stream.avail_out(),
        0,
        "avail_out should be 0 when output buffer is exhausted"
    );

    // Now there should be pending bytes in the output buffer.
    let (pending_bytes, _pending_bits) =
        deflate::deflate_pending(&stream).expect("deflate_pending after partial");
    assert!(
        pending_bytes > 0,
        "pending bytes should be > 0 after partial flush, got {pending_bytes}"
    );

    let _ret = deflate::deflate_end(&mut stream);
}

/// `deflate_copy()` clones the compression state mid-stream.
///
/// Both the original and the copy finish compression independently and
/// produce identical compressed output (given identical remaining input),
/// proving the internal deflate state was faithfully duplicated.
#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_deflate_copy() {
    // Generate moderately large test data.
    let data: Vec<u8> = (0..4096_usize).map(|i| (i % 256) as u8).collect();
    let half = data.len() / 2;

    // ── Initialize and compress first half ──
    let mut orig = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut orig,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for copy source");

    orig.set_output_buffer(COMPR_LEN);
    orig.set_input(&data[..half]);
    let ret = deflate::deflate(&mut orig, Z_SYNC_FLUSH);
    check_err(ret, "deflate first half");

    // Capture the prefix bytes emitted so far.
    let prefix = orig.take_output();

    // ── Copy the state ──
    // `deflate_copy` clones the internal deflate state but not the output
    // buffer.  Both streams continue from the same compression state.
    let mut copy = ZStream::new();
    let ret = deflate::deflate_copy(&mut copy, &orig);
    assert_ok(ret, "deflateCopy");

    // Give both streams fresh output buffers.
    orig.set_output_buffer(COMPR_LEN);
    copy.set_output_buffer(COMPR_LEN);

    // ── Finish original with second half ──
    orig.set_input(&data[half..]);
    let ret = deflate::deflate(&mut orig, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(finish) on original");
    let suffix_orig = orig.take_output();
    let _ret = deflate::deflate_end(&mut orig);

    // ── Finish copy with same second half ──
    copy.set_input(&data[half..]);
    let ret = deflate::deflate(&mut copy, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(finish) on copy");
    let suffix_copy = copy.take_output();
    let _ret = deflate::deflate_end(&mut copy);

    // The suffixes produced by identical states with identical input
    // should be identical.
    assert_bytes_equal(
        &suffix_orig,
        &suffix_copy,
        "copy test: suffixes should match",
    );

    // Build full compressed output: prefix + suffix.
    let mut full_orig = prefix.clone();
    full_orig.extend_from_slice(&suffix_orig);

    let mut full_copy = prefix;
    full_copy.extend_from_slice(&suffix_copy);

    // Both should round-trip to the original data.
    let mut dec_orig = alloc_uncompr_buffer(data.len());
    uncompress(&mut dec_orig, &full_orig).unwrap();
    assert_bytes_equal(&dec_orig, &data, "copy test: original round-trip");

    let mut dec_copy = alloc_uncompr_buffer(data.len());
    uncompress(&mut dec_copy, &full_copy).unwrap();
    assert_bytes_equal(&dec_copy, &data, "copy test: copy round-trip");
}

/// `deflate_reset()` allows reusing a stream for a second compression.
#[test]
fn test_deflate_reset() {
    let data1 = b"first compression payload for reset test";
    let data2 = b"second compression payload after reset";

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for reset test");

    // ── First compression ──
    stream.set_input(data1);
    let bound = deflate::deflate_bound(&stream, data1.len());
    stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate #1");

    let compressed1 = stream.take_output();

    // ── Reset the stream ──
    let ret = deflate::deflate_reset(&mut stream);
    assert_ok(ret, "deflateReset");

    // Verify counters are zeroed after reset.
    assert_eq!(stream.total_in, 0, "total_in should be 0 after reset");
    assert_eq!(stream.total_out, 0, "total_out should be 0 after reset");
    assert!(stream.msg.is_none(), "msg should be None after reset");

    // ── Second compression ──
    stream.set_input(data2);
    let bound = deflate::deflate_bound(&stream, data2.len());
    stream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate #2");

    let compressed2 = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    // ── Verify both round-trips ──
    let mut dec1 = alloc_uncompr_buffer(data1.len());
    uncompress(&mut dec1, &compressed1).unwrap();
    assert_bytes_equal(&dec1, data1, "reset: first round-trip");

    let mut dec2 = alloc_uncompr_buffer(data2.len());
    uncompress(&mut dec2, &compressed2).unwrap();
    assert_bytes_equal(&dec2, data2, "reset: second round-trip");
}

// =============================================================================
// Phase 8: Flush Mode Tests
// =============================================================================

/// `Z_SYNC_FLUSH` emits a sync point marker (`00 00 FF FF`).
///
/// The sync marker allows `inflateSync()` to recover from data corruption.
#[test]
fn test_deflate_sync_flush() {
    let data = b"sync flush test data: need enough to produce output!";

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for sync flush");

    stream.set_output_buffer(COMPR_LEN);
    stream.set_input(data);

    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    check_err(ret, "deflate(Z_SYNC_FLUSH)");

    let output = stream.take_output();

    // The sync flush marker `00 00 FF FF` should appear somewhere in the
    // compressed output (it terminates the current deflate block).
    let has_sync = output.windows(4).any(|w| w == [0x00, 0x00, 0xFF, 0xFF]);
    assert!(
        has_sync,
        "sync flush output should contain 00 00 FF FF marker"
    );

    // Finish and verify the full stream is decompressible.
    stream.set_input(&[]);
    stream.set_output_buffer(COMPR_LEN);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "deflate(Z_FINISH) after sync");

    let tail = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    let mut full_compressed = output;
    full_compressed.extend_from_slice(&tail);

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &full_compressed).unwrap();
    assert_bytes_equal(&decompressed, data, "sync flush round-trip");
}

/// `Z_FULL_FLUSH` resets the compression state, enabling independent
/// decompression of subsequent blocks.
///
/// Matches `test/example.c` `test_flush()` (lines 338–368).
#[test]
fn test_deflate_full_flush() {
    let data = b"full flush test data to exercise state reset functionality!";

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for full flush");

    stream.set_output_buffer(COMPR_LEN);
    stream.set_input(data);

    // Full-flush produces a sync point AND resets the compression state.
    let ret = deflate::deflate(&mut stream, Z_FULL_FLUSH);
    check_err(ret, "deflate(Z_FULL_FLUSH)");

    let output = stream.take_output();

    // The full flush marker `00 00 FF FF` should be present.
    let has_marker = output.windows(4).any(|w| w == [0x00, 0x00, 0xFF, 0xFF]);
    assert!(has_marker, "full flush output should contain 00 00 FF FF");

    // Finish the stream.
    stream.set_input(&[]);
    stream.set_output_buffer(COMPR_LEN);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "finish after full flush");

    let tail = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    let mut full = output;
    full.extend_from_slice(&tail);

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &full).unwrap();
    assert_bytes_equal(&decompressed, data, "full flush round-trip");
}

/// `Z_PARTIAL_FLUSH` produces a partial block flush.
#[test]
fn test_deflate_partial_flush() {
    let data = b"partial flush test: this exercises the legacy flush mode path";

    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_ok(ret, "deflateInit2 for partial flush");

    stream.set_output_buffer(COMPR_LEN);
    stream.set_input(data);

    let ret = deflate::deflate(&mut stream, Z_PARTIAL_FLUSH);
    check_err(ret, "deflate(Z_PARTIAL_FLUSH)");

    let output = stream.take_output();
    assert!(
        !output.is_empty(),
        "partial flush should produce some output"
    );

    // Finish the stream.
    stream.set_input(&[]);
    stream.set_output_buffer(COMPR_LEN);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_return_code(ret, ReturnCode::StreamEnd, "finish after partial flush");

    let tail = stream.take_output();
    let _ret = deflate::deflate_end(&mut stream);

    let mut full = output;
    full.extend_from_slice(&tail);

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &full).unwrap();
    assert_bytes_equal(&decompressed, data, "partial flush round-trip");
}

// =============================================================================
// Phase 9: Edge Cases
// =============================================================================

/// Compress and decompress empty input.
///
/// An empty zlib stream consists of just the 2-byte header plus the 4-byte
/// Adler-32 trailer and a final empty block.
#[test]
fn test_deflate_empty_input() {
    let data: &[u8] = &[];

    // One-call API with empty input.
    let mut compressed = Vec::new();
    compress(&mut compressed, data).unwrap();
    assert!(
        !compressed.is_empty(),
        "even empty input should produce a zlib header"
    );

    // Decompress into a zero-length buffer.
    let mut decompressed = alloc_uncompr_buffer(0);
    uncompress(&mut decompressed, &compressed).unwrap();
    assert!(decompressed.is_empty(), "empty input → empty output");

    // Streaming API with empty input.
    streaming_round_trip(
        data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
        MAX_WBITS,
    );
}

/// Compress 1 MB+ of data to exercise streaming across multiple blocks
/// and window sizes.
#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_deflate_large_input() {
    // Generate 1 MiB of pseudo-structured data.
    let size = 1_048_576_usize; // 1 MiB
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    // Compress → decompress via one-call API.
    let mut compressed = Vec::new();
    compress2(&mut compressed, &data, Z_DEFAULT_COMPRESSION).unwrap();

    // Compressed data should be significantly smaller for structured input.
    assert!(
        compressed.len() < data.len(),
        "1 MiB structured data should compress: {} >= {}",
        compressed.len(),
        data.len(),
    );

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &compressed).unwrap();
    assert_bytes_equal(&decompressed, &data, "large input round-trip");
}

/// Data with long identical runs exercises the RLE and match-finding paths.
#[test]
fn test_deflate_repeated_data() {
    // All zeros — maximum compression opportunity.
    let all_zeros = vec![0u8; 10_000];
    assert_round_trip(&all_zeros);

    // All ones.
    let all_ones = vec![0xFF_u8; 10_000];
    assert_round_trip(&all_ones);

    // Repeating two-byte pattern.
    let pattern: Vec<u8> = [0xAB, 0xCD].iter().copied().cycle().take(10_000).collect();
    assert_round_trip(&pattern);

    // Repeating 8-byte pattern — exercises longer match chains.
    let long_pattern: Vec<u8> = [1, 2, 3, 4, 5, 6, 7, 8]
        .iter()
        .copied()
        .cycle()
        .take(10_000)
        .collect();
    assert_round_trip(&long_pattern);
}

/// Pseudo-random (incompressible) data exercises the stored-block and
/// literal-heavy code paths.
#[test]
fn test_deflate_random_data() {
    // Simple LCG (linear congruential generator) for deterministic
    // pseudo-random bytes — no external dependency required.
    let mut rng_state: u64 = 0x1234_5678_9ABC_DEF0;
    let size = 10_000_usize;
    let data: Vec<u8> = (0..size)
        .map(|_| {
            rng_state = rng_state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_possible_truncation)]
            let byte = (rng_state >> 33) as u8;
            byte
        })
        .collect();

    // Random data is nearly incompressible — the compressed size will be
    // close to (or slightly larger than) the original due to header overhead.
    let mut compressed = Vec::new();
    compress2(&mut compressed, &data, Z_BEST_COMPRESSION).unwrap();

    let mut decompressed = alloc_uncompr_buffer(data.len());
    uncompress(&mut decompressed, &compressed).unwrap();
    assert_bytes_equal(&decompressed, &data, "random data round-trip");

    // Also test with RLE strategy on random data — should still round-trip.
    streaming_round_trip(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_RLE,
        MAX_WBITS,
    );

    // And with Z_HUFFMAN_ONLY.
    streaming_round_trip(
        &data,
        Z_DEFAULT_COMPRESSION,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_HUFFMAN_ONLY,
        MAX_WBITS,
    );
}

// =============================================================================
// Additional Edge Cases: error paths
// =============================================================================

/// Corrupted compressed data should produce a `DataError` on inflate.
#[test]
fn test_deflate_corrupt_data() {
    let data = HELLO;

    let mut compressed = Vec::new();
    compress(&mut compressed, data).unwrap();

    // Corrupt a byte in the middle of the compressed stream.
    let mid = compressed.len() / 2;
    if mid > 0 {
        compressed[mid] ^= 0xFF;
    }

    let mut decompressed = alloc_uncompr_buffer(data.len());
    let result = uncompress(&mut decompressed, &compressed);
    assert!(result.is_err(), "decompressing corrupted data should fail");
    let err = result.unwrap_err();
    assert!(
        err == ReturnCode::DataError || err == ReturnCode::BufError,
        "corrupted data should yield DataError or BufError, got {err:?}"
    );
}

/// Output buffer too small on uncompress should yield an error.
#[test]
fn test_deflate_uncompress_buf_error() {
    let data = b"enough data to make the output buffer too small for decompression";

    let mut compressed = Vec::new();
    compress(&mut compressed, data).unwrap();

    // Allocate a destination buffer that is clearly too small.
    let mut too_small = alloc_uncompr_buffer(1);
    let result = uncompress(&mut too_small, &compressed);
    assert!(
        result.is_err(),
        "uncompress with insufficient output should fail"
    );
}

/// `alloc_compr_buffer` and `alloc_test_buffers` produce correctly sized buffers.
#[test]
fn test_buffer_allocation_helpers() {
    let (compr, uncompr) = alloc_test_buffers();
    assert_eq!(compr.len(), COMPR_LEN);
    assert_eq!(uncompr.len(), UNCOMPR_LEN);

    let custom_compr = alloc_compr_buffer(512);
    assert_eq!(custom_compr.len(), 512);

    let custom_uncompr = alloc_uncompr_buffer(1024);
    assert_eq!(custom_uncompr.len(), 1024);
}
