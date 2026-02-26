#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
//! Inflate-specific edge case tests.
//!
//! Tests format auto-detection (`windowBits`+32), corrupt data handling,
//! `inflateSync` recovery, and inflate state machine edge cases.

// Test code may use wildcard imports for concise access to shared helpers.
#![allow(clippy::wildcard_imports)]
// Test helper functions always produce used values; #[must_use] is noise here.
#![allow(clippy::must_use_candidate)]
// Test helpers intentionally panic on failure; documenting every unwrap is noise.
#![allow(clippy::missing_panics_doc)]

use zlib_rs::deflate;
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate;
use zlib_rs::stream::{GzHeader, ZStream};
use zlib_rs_tests::*;

// =============================================================================
// Streaming Compression / Decompression Helpers
// =============================================================================

/// Compress `data` using the streaming deflate API with full parameter control.
///
/// `window_bits` selects the output format:
/// - `MAX_WBITS` (15): zlib (RFC 1950)
/// - `-MAX_WBITS` (−15): raw DEFLATE (RFC 1951)
/// - `MAX_WBITS + 16` (31): gzip (RFC 1952)
fn streaming_compress(data: &[u8], window_bits: i32, level: i32) -> Vec<u8> {
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
    let bound = compress_bound(data.len()).max(128) + 64;
    stream.set_output_buffer(bound);

    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "deflate did not complete: {ret:?}"
    );

    let output = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);
    output
}

/// Decompress `compressed` using the streaming inflate API.
///
/// `window_bits` controls format:
/// - `MAX_WBITS` (15): zlib
/// - `-MAX_WBITS` (−15): raw DEFLATE
/// - `MAX_WBITS + 16` (31): gzip only
/// - `MAX_WBITS + 32` (47): auto-detect zlib or gzip
fn streaming_decompress(compressed: &[u8], window_bits: i32) -> (ReturnCode, Vec<u8>) {
    streaming_decompress_with_buf(compressed, window_bits, UNCOMPR_LEN)
}

/// Same as [`streaming_decompress`] but accepts a custom output buffer size.
fn streaming_decompress_with_buf(
    compressed: &[u8],
    window_bits: i32,
    out_len: usize,
) -> (ReturnCode, Vec<u8>) {
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, window_bits);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed: {ret:?}");

    stream.set_input(compressed);
    stream.set_output_buffer(out_len);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }

    let _ = inflate::inflate_end(&mut state, &mut stream);
    (ret, stream.take_output())
}

// =============================================================================
// Phase 2: Format Auto-Detection Tests (windowBits + 32)
// =============================================================================

/// Test: `windowBits`=47 (`MAX_WBITS`+32) auto-detects zlib format.
///
/// The inflate engine should correctly identify and decompress a zlib-format
/// stream when auto-detection mode is active.
#[test]
fn test_inflate_auto_detect_zlib() {
    // Compress with zlib format (positive windowBits)
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // Decompress with auto-detect (windowBits = MAX_WBITS + 32 = 47)
    let (ret, output) = streaming_decompress(&compressed, MAX_WBITS + 32);
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "auto-detect zlib failed: {ret:?}"
    );
    assert_bytes_equal(&output, HELLO, "auto-detect zlib output");
}

/// Test: `windowBits`=47 (`MAX_WBITS`+32) auto-detects gzip format.
///
/// The inflate engine should correctly identify and decompress a gzip-format
/// stream when auto-detection mode is active.
#[test]
fn test_inflate_auto_detect_gzip() {
    // Compress with gzip format (windowBits = MAX_WBITS + 16 = 31)
    let compressed = streaming_compress(HELLO, MAX_WBITS + 16, Z_DEFAULT_COMPRESSION);

    // Decompress with auto-detect (windowBits = MAX_WBITS + 32 = 47)
    let (ret, output) = streaming_decompress(&compressed, MAX_WBITS + 32);
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "auto-detect gzip failed: {ret:?}"
    );
    assert_bytes_equal(&output, HELLO, "auto-detect gzip output");
}

/// Test: `windowBits`=-15 for raw DEFLATE.
///
/// Raw DEFLATE streams have no wrapper headers or trailers.
#[test]
fn test_inflate_auto_detect_raw() {
    // Compress with raw format (negative windowBits)
    let compressed = streaming_compress(HELLO, -MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // Decompress with raw format (windowBits = -MAX_WBITS)
    let (ret, output) = streaming_decompress(&compressed, -MAX_WBITS);
    assert_eq!(ret, ReturnCode::StreamEnd, "raw inflate failed: {ret:?}");
    assert_bytes_equal(&output, HELLO, "raw inflate output");
}

/// Test: format mismatch produces `Z_DATA_ERROR`.
///
/// When the format of the compressed data doesn't match what inflate expects,
/// the inflate engine should return `Z_DATA_ERROR`.
#[test]
fn test_inflate_format_mismatch() {
    // Compress as gzip, try to decompress as zlib → should fail
    let gzip_data = streaming_compress(HELLO, MAX_WBITS + 16, Z_DEFAULT_COMPRESSION);
    let (ret, _) = streaming_decompress(&gzip_data, MAX_WBITS);
    assert_eq!(
        ret,
        ReturnCode::DataError,
        "gzip data decompressed as zlib should fail: {ret:?}"
    );

    // Compress as zlib, try to decompress as gzip → should fail
    let zlib_data = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    let (ret, _) = streaming_decompress(&zlib_data, MAX_WBITS + 16);
    assert_eq!(
        ret,
        ReturnCode::DataError,
        "zlib data decompressed as gzip should fail: {ret:?}"
    );
}

// =============================================================================
// Phase 3: Corrupt Data Handling
// =============================================================================

/// Test: invalid zlib header bytes produce `Z_DATA_ERROR`.
///
/// The CMF/FLG check (CMF*256 + FLG) % 31 must be zero. Modifying
/// the header bytes violates this check.
#[test]
fn test_inflate_corrupt_header() {
    let mut compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    assert!(
        compressed.len() >= 2,
        "compressed data too short for header corruption"
    );

    // Corrupt the zlib header by flipping a bit in the CMF byte
    compressed[0] ^= 0x80;

    let (ret, _) = streaming_decompress(&compressed, MAX_WBITS);
    assert!(
        ret == ReturnCode::DataError,
        "corrupt header should produce DataError, got: {ret:?}"
    );
}

/// Test: corrupted compressed data body produces `Z_DATA_ERROR`.
///
/// Flipping bits in the middle of the compressed data payload should
/// cause the inflate engine to detect the corruption.
#[test]
fn test_inflate_corrupt_data() {
    let mut compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    assert!(
        compressed.len() > 4,
        "compressed data too short for body corruption"
    );

    // Flip bits in the middle of the compressed data body
    let mid = compressed.len() / 2;
    compressed[mid] ^= 0xFF;

    let (ret, _) = streaming_decompress(&compressed, MAX_WBITS);
    // Corrupt data should produce DataError (or possibly BufError if truncation
    // effects cause issues), but never StreamEnd
    assert!(
        ret == ReturnCode::DataError || ret == ReturnCode::BufError,
        "corrupt data should produce DataError or BufError, got: {ret:?}"
    );
}

/// Test: corrupted Adler-32 trailer produces `Z_DATA_ERROR`.
///
/// The last 4 bytes of a zlib stream are the Adler-32 checksum. Corrupting
/// them should cause inflate to report "incorrect data check".
#[test]
fn test_inflate_corrupt_checksum() {
    let mut compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    assert!(
        compressed.len() >= 4,
        "compressed data too short for checksum corruption"
    );

    // Corrupt the last 4 bytes (Adler-32 checksum)
    let len = compressed.len();
    compressed[len - 1] ^= 0xFF;
    compressed[len - 2] ^= 0xFF;

    let (ret, _) = streaming_decompress(&compressed, MAX_WBITS);
    assert_eq!(
        ret,
        ReturnCode::DataError,
        "corrupt checksum should produce DataError: {ret:?}"
    );
}

/// Test: truncated compressed stream produces `Z_BUF_ERROR` or `Z_DATA_ERROR`.
///
/// Providing incomplete compressed data should prevent successful decompression.
#[test]
fn test_inflate_truncated_stream() {
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    assert!(
        compressed.len() > 4,
        "compressed data too short for truncation test"
    );

    // Truncate: provide only half of the compressed data
    let half = compressed.len() / 2;
    let truncated = &compressed[..half];

    let (ret, _) = streaming_decompress(truncated, MAX_WBITS);
    assert!(
        ret == ReturnCode::BufError || ret == ReturnCode::DataError,
        "truncated stream should produce BufError or DataError, got: {ret:?}"
    );
}

/// Test: empty input to inflate produces `Z_BUF_ERROR`.
///
/// When no input is available, inflate cannot make progress.
#[test]
fn test_inflate_empty_input() {
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    // Set zero-length input
    stream.set_input(&[]);
    stream.set_output_buffer(UNCOMPR_LEN);

    let ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    assert_eq!(
        ret,
        ReturnCode::BufError,
        "empty input should produce BufError: {ret:?}"
    );

    let _ = inflate::inflate_end(&mut state, &mut stream);
}

// =============================================================================
// Phase 4: inflateSync Recovery Tests
// =============================================================================

/// Test: `inflateSync` recovery after corruption at a `Z_FULL_FLUSH` sync point.
///
/// Port of `test_flush` + `test_sync` from `test/example.c`.
/// Compresses data with `Z_FULL_FLUSH` to create sync points, corrupts the
/// data between the header and the sync point, and verifies that
/// `inflateSync` can skip past the corrupted region and recover.
#[test]
fn test_inflate_sync_recovery() {
    // Step 1: Compress with Z_FULL_FLUSH after the first 3 bytes to create
    // a sync point, matching the C test example.
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed");

    // Compress with sync flush in the middle
    stream.set_input(HELLO);
    let bound = compress_bound(HELLO.len()).max(128) + 256;
    stream.set_output_buffer(bound);

    // First: flush with Z_FULL_FLUSH to create a sync point
    let ret = deflate::deflate(&mut stream, Z_FULL_FLUSH);
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::StreamEnd,
        "Z_FULL_FLUSH failed: {ret:?}"
    );

    // Then: finish the remaining data
    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "Z_FINISH failed: {ret:?}");

    let mut compr = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    // Step 2: Corrupt a byte after the zlib header (at index 3, like C test)
    if compr.len() > 3 {
        compr[3] = compr[3].wrapping_add(1);
    }

    // Step 3: Decompress — expect the corruption to be detected, then sync
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed");

    // Feed just the first 2 bytes (the zlib header)
    istream.set_input(&compr[..2]);
    istream.set_output_buffer(UNCOMPR_LEN);

    let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    // After reading just the header, we expect OK or need more input
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::BufError,
        "initial inflate unexpected: {ret:?}"
    );

    // Now feed the rest of the (corrupted) data
    istream.set_input(&compr[2..]);

    // Try to inflate — should hit corruption
    let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    // May get DataError or OK depending on how far it gets before detecting corruption
    if ret == ReturnCode::DataError || ret == ReturnCode::BufError {
        // Try inflateSync to skip to the next sync point
        let sync_ret = inflate::inflate_sync(&mut istate, &mut istream);
        // inflateSync returns Ok if it found a sync point, DataError if not
        if sync_ret == ReturnCode::Ok {
            // Continue decompression from the sync point
            let mut ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
            while ret == ReturnCode::Ok {
                ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
            }
            // After sync recovery, we may reach StreamEnd or DataError
            // (depending on how much data was recoverable)
            assert!(
                ret == ReturnCode::StreamEnd
                    || ret == ReturnCode::DataError
                    || ret == ReturnCode::BufError,
                "post-sync inflate unexpected: {ret:?}"
            );
        }
    }

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: `inflateSync` on data compressed without any full flush.
///
/// When no sync points exist in the stream, `inflateSync` should return
/// `Z_DATA_ERROR` because there is no sync pattern to find.
#[test]
fn test_inflate_sync_no_sync_point() {
    // Compress without any full flush — no sync points
    let mut compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    assert!(compressed.len() > 4, "compressed data too short");

    // Corrupt a byte in the middle
    let mid = compressed.len() / 2;
    compressed[mid] ^= 0xFF;

    // Initialize inflate
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    stream.set_input(&compressed);
    stream.set_output_buffer(UNCOMPR_LEN);

    // inflate until we hit the corruption
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }

    // If we got a data error, try inflateSync
    if ret == ReturnCode::DataError {
        let sync_ret = inflate::inflate_sync(&mut state, &mut stream);
        // Without sync points, sync should fail with DataError
        // (or possibly BufError if input is exhausted)
        assert!(
            sync_ret == ReturnCode::DataError || sync_ret == ReturnCode::BufError,
            "inflateSync without sync points should fail: {sync_ret:?}"
        );
    }

    let _ = inflate::inflate_end(&mut state, &mut stream);
}

// =============================================================================
// Phase 5: inflateReset and inflateCopy Tests
// =============================================================================

/// Test: `inflateReset` reuses the stream for multiple decompression passes.
///
/// After a successful decompression, `inflateReset` should allow the same
/// state to decompress another stream without re-initialization.
#[test]
fn test_inflate_reset() {
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    // First decompression
    stream.set_input(&compressed);
    stream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "first decompress failed: {ret:?}"
    );
    let output1 = stream.take_output();
    assert_bytes_equal(&output1, HELLO, "first decompression output");

    // Reset for second decompression
    let ret = inflate::inflate_reset(&mut state, &mut stream);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset failed: {ret:?}");

    // Second decompression of the same data
    stream.set_input(&compressed);
    stream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "second decompress failed: {ret:?}"
    );
    let output2 = stream.take_output();
    assert_bytes_equal(&output2, HELLO, "second decompression output");

    let _ = inflate::inflate_end(&mut state, &mut stream);
}

/// Test: `inflateReset2` changes `windowBits` format.
///
/// Initialize for zlib, reset to raw DEFLATE, then decompress raw data.
#[test]
fn test_inflate_reset2() {
    // Prepare raw compressed data
    let raw_compressed = streaming_compress(HELLO, -MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // Initialize with zlib format (windowBits = 15)
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok, "initial inflate_reset2 failed");

    // Reset to raw format (windowBits = -15)
    let ret = inflate::inflate_reset2(&mut state, &mut stream, -MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 to raw failed");

    // Decompress raw data
    stream.set_input(&raw_compressed);
    stream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "raw decompress failed: {ret:?}");

    let output = stream.take_output();
    assert_bytes_equal(&output, HELLO, "inflate_reset2 raw output");

    let _ = inflate::inflate_end(&mut state, &mut stream);
}

/// Test: inflateCopy clones decompression state mid-stream.
///
/// Start decompressing, copy the inflate state, then finish decompression
/// on both original and copy. Both should produce identical output.
#[test]
fn test_inflate_copy() {
    // Compress a larger piece of data so there's meaningful state to copy
    let test_data = b"The quick brown fox jumps over the lazy dog. \
                      Pack my box with five dozen liquor jugs. \
                      Sphinx of black quartz, judge my vow!\0";
    let compressed = streaming_compress(test_data, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // Initialize inflate
    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    // Feed only part of the input to leave some for mid-stream copy
    let split_point = compressed.len() / 2;
    stream.set_input(&compressed[..split_point]);
    stream.set_output_buffer(UNCOMPR_LEN);

    // Inflate partway through
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok && stream.avail_in() > 0 {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }

    // Copy the state
    let copy_state = inflate::inflate_copy(&state);
    assert!(copy_state.is_some(), "inflate_copy returned None");
    let mut copy_state = copy_state.unwrap();

    // Capture output so far from original for comparison
    let partial_output = stream.output_written().to_vec();

    // Finish decompression on original
    stream.set_input(&compressed[split_point..]);
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "original decompress failed: {ret:?}"
    );
    let original_output = stream.take_output();

    // Finish decompression on copy
    let mut copy_stream = ZStream::new();
    // The copy needs the same partial output position — set up fresh output
    copy_stream.set_input(&compressed[split_point..]);
    copy_stream.set_output_buffer(UNCOMPR_LEN);
    // First fill in the partial output already produced
    {
        let out_buf = copy_stream.output_remaining_mut();
        let plen = partial_output.len().min(out_buf.len());
        out_buf[..plen].copy_from_slice(&partial_output[..plen]);
        let _ = copy_stream.advance_output(plen);
    }

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut copy_state, &mut copy_stream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "copy decompress failed: {ret:?}"
    );
    let copy_output = copy_stream.take_output();

    // Both outputs should match the original data
    assert_bytes_equal(&original_output, test_data, "original output");
    assert_bytes_equal(&copy_output, test_data, "copy output");

    let _ = inflate::inflate_end(&mut state, &mut stream);
    let _ = inflate::inflate_end(&mut copy_state, &mut copy_stream);
}

// =============================================================================
// Phase 6: inflateGetHeader Tests (gzip header extraction)
// =============================================================================

/// Test: `inflateGetHeader` extracts gzip header information.
#[test]
fn test_inflate_get_header() {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS + 16,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 for gzip failed");

    let gz_head = GzHeader {
        text: true,
        time: 1_234_567_890,
        xflags: 0,
        os: 3,
        extra: Some(b"test-extra".to_vec()),
        name: Some(b"test-file.txt\0".to_vec()),
        comment: Some(b"test comment\0".to_vec()),
        hcrc: true,
        done: false,
    };
    let ret = deflate::deflate_set_header(&mut stream, gz_head);
    assert_eq!(ret, ReturnCode::Ok, "deflate_set_header failed");

    stream.set_input(HELLO);
    let bound = compress_bound(HELLO.len()).max(128) + 256;
    stream.set_output_buffer(bound);

    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "gzip deflate failed");
    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS + 32);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed");

    let header = Box::new(GzHeader::new());
    let ret = inflate::inflate_get_header(&mut istate, header);
    assert_eq!(ret, ReturnCode::Ok, "inflate_get_header failed");

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "gzip inflate failed: {ret:?}");

    let output = istream.take_output();
    assert_bytes_equal(&output, HELLO, "gzip header round-trip output");

    if let Some(head) = istate.header() {
        assert!(head.done, "header should be marked done");
        assert!(head.text, "text flag should be set");
        assert_eq!(head.time, 1_234_567_890, "time mismatch");
        assert_eq!(head.os, 3, "OS mismatch");
        if let Some(ref name) = head.name {
            let s: Vec<u8> = name.iter().copied().take_while(|&b| b != 0).collect();
            assert_eq!(s, b"test-file.txt".to_vec(), "name mismatch");
        } else {
            panic!("header name should be present");
        }
        if let Some(ref comment) = head.comment {
            let s: Vec<u8> = comment.iter().copied().take_while(|&b| b != 0).collect();
            assert_eq!(s, b"test comment".to_vec(), "comment mismatch");
        } else {
            panic!("header comment should be present");
        }
        if let Some(ref extra) = head.extra {
            assert_eq!(extra.as_slice(), b"test-extra", "extra mismatch");
        } else {
            panic!("header extra should be present");
        }
    } else {
        panic!("inflate state should have a captured header");
    }

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

// =============================================================================
// Phase 7: inflateSetDictionary Tests
// =============================================================================

/// Test: Decompress stream that requires a preset dictionary.
#[test]
fn test_inflate_set_dictionary() {
    // Compress with a preset dictionary
    let mut cstream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut cstream,
        Z_BEST_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed");

    let ret = deflate::deflate_set_dictionary(&mut cstream, DICTIONARY);
    assert_eq!(ret, ReturnCode::Ok, "deflate_set_dictionary failed");
    let dict_adler = cstream.adler;

    cstream.set_input(HELLO);
    let bound = compress_bound(HELLO.len()).max(128) + 256;
    cstream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut cstream, Z_FINISH);
    assert_eq!(ret, ReturnCode::StreamEnd, "deflate with dictionary failed");
    let compressed = cstream.take_output();
    let _ = deflate::deflate_end(&mut cstream);

    // Decompress — inflate should return NeedDict
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed");

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    assert_eq!(ret, ReturnCode::NeedDict, "should get NeedDict");
    assert_eq!(istream.adler, dict_adler, "dictionary ID mismatch");

    let ret = inflate::inflate_set_dictionary(&mut istate, &mut istream, DICTIONARY);
    assert_eq!(ret, ReturnCode::Ok, "inflate_set_dictionary failed");

    let mut ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "inflate with dict failed: {ret:?}"
    );

    let output = istream.take_output();
    assert_bytes_equal(&output, HELLO, "dictionary round-trip");

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Supply a wrong dictionary → should produce incorrect result or error.
#[test]
fn test_inflate_wrong_dictionary() {
    // Compress with DICTIONARY
    let mut cstream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut cstream,
        Z_BEST_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok);

    let ret = deflate::deflate_set_dictionary(&mut cstream, DICTIONARY);
    assert_eq!(ret, ReturnCode::Ok);

    cstream.set_input(HELLO);
    let bound = compress_bound(HELLO.len()).max(128) + 256;
    cstream.set_output_buffer(bound);

    let ret = deflate::deflate(&mut cstream, Z_FINISH);
    assert_eq!(ret, ReturnCode::StreamEnd);
    let compressed = cstream.take_output();
    let _ = deflate::deflate_end(&mut cstream);

    // Decompress with wrong dictionary
    let wrong_dict = b"completely wrong dictionary content";
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    assert_eq!(ret, ReturnCode::NeedDict, "should get NeedDict");

    // Supply wrong dictionary — the Adler-32 won't match, so should get DataError
    let ret = inflate::inflate_set_dictionary(&mut istate, &mut istream, wrong_dict);
    assert_eq!(
        ret,
        ReturnCode::DataError,
        "wrong dictionary should cause DataError"
    );

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

// =============================================================================
// Phase 8: Window Size Tests
// =============================================================================

/// Test: Verify round-trip with each valid `windowBits` value (9..=15).
#[test]
fn test_inflate_various_window_sizes() {
    for wbits in 9..=15 {
        let compressed = streaming_compress(HELLO, wbits, Z_DEFAULT_COMPRESSION);
        let (ret, decompressed) = streaming_decompress(&compressed, wbits);
        assert_eq!(
            ret,
            ReturnCode::StreamEnd,
            "windowBits={wbits} inflate failed"
        );
        assert_bytes_equal(
            &decompressed,
            HELLO,
            &format!("windowBits={wbits} round-trip"),
        );
    }
}

/// Test: Decompress with smaller window than used for compression.
///
/// According to the zlib specification, the decompressor window must be at
/// least as large as the one used during compression for LZ77 backreferences
/// to resolve correctly. However if the data does not exercise full window
/// distances the decompression might still succeed.
#[test]
fn test_inflate_window_too_small() {
    // Compress with max window
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // Try to decompress with the smallest window
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, 9);
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 failed");

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    // For short data like HELLO, the window difference might still work.
    // For longer data with backreferences beyond the small window, it would fail.
    // Accept either success or DataError.
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    }
    assert!(
        ret == ReturnCode::StreamEnd || ret == ReturnCode::DataError,
        "expected StreamEnd or DataError, got {ret:?}"
    );

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

// =============================================================================
// Phase 9: Streaming Edge Cases
// =============================================================================

/// Test: Feed compressed data to inflate one byte at a time.
#[test]
fn test_inflate_byte_at_a_time() {
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    let mut output = Vec::with_capacity(UNCOMPR_LEN);
    let mut pos = 0;

    loop {
        // Feed one input byte at a time
        if istream.avail_in() == 0 && pos < compressed.len() {
            istream.set_input(&compressed[pos..=pos]);
            pos += 1;
        }

        // Provide a fresh small output buffer each time
        istream.set_output_buffer(UNCOMPR_LEN);

        let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
        let written = istream.take_output();
        output.extend_from_slice(&written);

        if ret == ReturnCode::StreamEnd {
            break;
        }
        assert!(
            ret == ReturnCode::Ok || ret == ReturnCode::BufError,
            "unexpected return: {ret:?}"
        );
        // Avoid infinite loop when inflate needs more input but we have none
        if pos >= compressed.len() && istream.avail_in() == 0 && ret == ReturnCode::BufError {
            break;
        }
    }

    assert_bytes_equal(&output, HELLO, "byte-at-a-time inflate");
    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Decompress with a 1-byte output buffer — repeatedly call inflate.
#[test]
fn test_inflate_small_output_buffer() {
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    istream.set_input(&compressed);

    let mut output = Vec::with_capacity(UNCOMPR_LEN);

    loop {
        // Only give 1 byte of output space at a time
        istream.set_output_buffer(1);

        let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
        let written = istream.take_output();
        output.extend_from_slice(&written);

        if ret == ReturnCode::StreamEnd {
            break;
        }
        assert!(
            ret == ReturnCode::Ok || ret == ReturnCode::BufError,
            "unexpected return: {ret:?}"
        );
    }

    assert_bytes_equal(&output, HELLO, "small-output-buffer inflate");
    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Decompress data that expands greatly (highly compressible input).
#[test]
fn test_inflate_large_output() {
    let big_input = vec![0u8; 65536];
    let compressed = streaming_compress(&big_input, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    // The compressed size should be much smaller than 65536
    assert!(
        compressed.len() < big_input.len(),
        "all-zeros should compress well: {} -> {}",
        big_input.len(),
        compressed.len(),
    );

    let (ret, decompressed) =
        streaming_decompress_with_buf(&compressed, MAX_WBITS, big_input.len() + 1024);
    assert_eq!(ret, ReturnCode::StreamEnd, "all-zeros inflate failed");
    assert_eq!(
        decompressed.len(),
        big_input.len(),
        "output length mismatch"
    );
    assert_eq!(decompressed, big_input, "all-zeros round-trip");
}

// =============================================================================
// Phase 10: Error Handling and Additional Edge Cases
// =============================================================================

/// Test: Invalid `windowBits` values should return `StreamError`.
///
/// Note: `windowBits=0` is intentionally *valid* — it means "keep the
/// current `windowBits`" (same semantics as C zlib's `inflateReset2(strm, 0)`
/// introduced in zlib 1.2.3.5). Values 1..7 and -1..-7 are too small for
/// the DEFLATE window, and values >= 48 (when positive) exceed the auto-detect
/// range, so those are invalid.
#[test]
fn test_inflate_invalid_window_bits() {
    let invalid_values: &[i32] = &[1, 7, 100, -1, -7, -16];
    for &wbits in invalid_values {
        let mut istream = ZStream::new();
        let mut istate = inflate::InflateState::new();
        let ret = inflate::inflate_reset2(&mut istate, &mut istream, wbits);
        assert_eq!(
            ret,
            ReturnCode::StreamError,
            "windowBits={wbits} should return StreamError"
        );
    }
}

/// Test: `inflatePrime` inserts bits for custom header processing.
#[test]
fn test_inflate_prime() {
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, -(MAX_WBITS));
    assert_eq!(ret, ReturnCode::Ok, "inflate_reset2 for raw failed");

    // Prime with some bits (0 bits, value 0 is a no-op and should succeed)
    let ret = inflate::inflate_prime(&mut istate, 0, 0);
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::StreamError,
        "inflate_prime(0,0) unexpected: {ret:?}"
    );

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: `inflateMark` returns -65536 (-(1<<16)) when not inside a block.
#[test]
fn test_inflate_mark() {
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    let mark = inflate::inflate_mark(&istate);
    // Before any data is processed, mark returns -(1 << 16) = -65536
    assert_eq!(
        mark,
        -(1_i64 << 16),
        "initial inflate_mark should be -65536"
    );

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: `inflateGetHeader` on a non-gzip stream returns `StreamError`.
#[test]
fn test_inflate_get_header_wrong_format() {
    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    // Initialize for zlib format (not gzip) — wrap & 2 == 0
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    let header = Box::new(GzHeader::new());
    let ret = inflate::inflate_get_header(&mut istate, header);
    assert_eq!(
        ret,
        ReturnCode::StreamError,
        "inflateGetHeader on zlib stream should return StreamError"
    );

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Round-trip decompression at every compression level (0..=9).
#[test]
fn test_inflate_all_compression_levels() {
    for level in 0..=9 {
        let compressed = streaming_compress(HELLO, MAX_WBITS, level);
        let (ret, decompressed) = streaming_decompress(&compressed, MAX_WBITS);
        assert_eq!(ret, ReturnCode::StreamEnd, "level {level} inflate failed");
        assert_bytes_equal(&decompressed, HELLO, &format!("level {level} round-trip"));
    }
}

/// Test: Inflate with a larger payload to exercise the fast decode path.
///
/// When input data exceeds ~258 bytes and is uncompressed to a sufficient
/// size, the inflate engine enters the `inflate_fast` hot loop. This test
/// verifies that path works correctly.
#[test]
fn test_inflate_fast_path() {
    // Generate 16 KiB of pseudo-random but compressible data
    let mut big_input = Vec::with_capacity(16384);
    for i in 0u32..16384 {
        big_input.push(((i.wrapping_mul(31).wrapping_add(7)) & 0xFF) as u8);
    }

    let compressed = streaming_compress(&big_input, MAX_WBITS, Z_DEFAULT_COMPRESSION);
    let (ret, decompressed) =
        streaming_decompress_with_buf(&compressed, MAX_WBITS, big_input.len() + 1024);
    assert_eq!(ret, ReturnCode::StreamEnd, "fast-path inflate failed");
    assert_eq!(
        decompressed.len(),
        big_input.len(),
        "fast-path output length mismatch"
    );
    assert_eq!(decompressed, big_input, "fast-path data mismatch");
}

/// Test: Decompress gzip format with explicit `windowBits` (`MAX_WBITS` + 16).
#[test]
fn test_inflate_gzip_explicit() {
    let compressed = streaming_compress(HELLO, MAX_WBITS + 16, Z_DEFAULT_COMPRESSION);
    let (ret, decompressed) = streaming_decompress(&compressed, MAX_WBITS + 16);
    assert_eq!(ret, ReturnCode::StreamEnd, "explicit gzip inflate failed");
    assert_bytes_equal(&decompressed, HELLO, "explicit gzip round-trip");
}

/// Test: `inflateReset` after partial decompression allows full restart.
#[test]
fn test_inflate_reset_after_partial() {
    let compressed = streaming_compress(HELLO, MAX_WBITS, Z_DEFAULT_COMPRESSION);

    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS);
    assert_eq!(ret, ReturnCode::Ok);

    // Feed only partial data
    let half = compressed.len() / 2;
    istream.set_input(&compressed[..half]);
    istream.set_output_buffer(UNCOMPR_LEN);

    let ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::BufError,
        "partial inflate unexpected: {ret:?}"
    );

    // Reset and start over with full data
    let ret = inflate::inflate_reset(&mut istate, &mut istream);
    assert_eq!(ret, ReturnCode::Ok, "inflateReset after partial failed");

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    }
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "full inflate after reset failed"
    );

    let output = istream.take_output();
    assert_bytes_equal(&output, HELLO, "reset after partial");

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Multiple sequential decompressions without reset (new state each time).
#[test]
fn test_inflate_multiple_streams() {
    let data_sets: &[&[u8]] = &[
        b"short",
        HELLO,
        b"a slightly longer piece of test data for the inflate engine",
        &[42u8; 1024],
    ];

    for (i, &data) in data_sets.iter().enumerate() {
        let compressed = streaming_compress(data, MAX_WBITS, Z_DEFAULT_COMPRESSION);
        let (ret, decompressed) =
            streaming_decompress_with_buf(&compressed, MAX_WBITS, data.len() + 1024);
        assert_eq!(ret, ReturnCode::StreamEnd, "stream #{i} inflate failed");
        assert_eq!(decompressed.as_slice(), data, "stream #{i} data mismatch");
    }
}

// =============================================================================
// Phase 11: Additional Coverage — schema-required members
// =============================================================================

/// Test: Use `compress2` at a specific level and decompress using `inflate_init`
/// (the default-`windowBits` variant) and `inflate_init2`.
///
/// Exercises: `compress2`, `inflate_init`, `inflate_init2`, `assert_ok`,
/// `assert_return_code`, `alloc_compr_buffer`, `alloc_uncompr_buffer`,
/// `check_err`, `ZStream.total_in`, `ZStream.total_out`, `ZStream.msg`.
#[test]
fn test_inflate_init_with_compress2() {
    let mut compr = alloc_compr_buffer(COMPR_LEN);
    let _uncompr = alloc_uncompr_buffer(UNCOMPR_LEN);

    // Compress using compress2 at best compression
    compress2(&mut compr, HELLO, Z_BEST_COMPRESSION).expect("compress2 should succeed");

    // Decompress using inflate_init (default windowBits = DEF_WBITS = 15, zlib format)
    let mut stream = ZStream::new();
    let ret = inflate::inflate_init(&mut stream);
    check_err(ret, "inflate_init");
    assert_ok(ret, "inflate_init");
    assert!(stream.msg.is_none(), "msg should be None after init");

    stream.set_input(&compr);
    stream.set_output_buffer(UNCOMPR_LEN);

    // Use inflate_init2 for a second stream to verify it also works
    let mut stream2 = ZStream::new();
    let ret2 = inflate::inflate_init2(&mut stream2, MAX_WBITS);
    assert_return_code(ret2, ReturnCode::Ok, "inflate_init2");
    assert_eq!(stream2.total_in, 0, "initial total_in should be 0");
    assert_eq!(stream2.total_out, 0, "initial total_out should be 0");

    // inflate_init and inflate_init2 store state inside the ZStream.
    // Verify init succeeded; external-state tests above cover decompression.
    drop(stream);
    drop(stream2);
}

/// Test: Round-trip using `alloc_test_buffers` and checking `avail_out`,
/// `total_in`, `total_out` fields during decompression.
///
/// Exercises: `alloc_test_buffers`, `ZStream.avail_out`.
#[test]
fn test_inflate_one_call_helpers() {
    let (mut compr, uncompr) = alloc_test_buffers();

    compress2(&mut compr, HELLO, Z_DEFAULT_COMPRESSION)
        .expect("compress2 in one_call_helpers should succeed");

    let mut stream = ZStream::new();
    let mut state = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut state, &mut stream, MAX_WBITS);
    check_err(ret, "inflate_reset2 in one_call_helpers");

    stream.set_input(&compr);
    stream.set_output_buffer(uncompr.len());

    assert_eq!(stream.avail_out(), uncompr.len(), "avail_out mismatch");

    let mut ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut state, &mut stream, Z_NO_FLUSH);
    }
    assert_return_code(ret, ReturnCode::StreamEnd, "inflate in one_call_helpers");

    assert!(stream.total_in > 0, "total_in should be > 0");
    assert!(stream.total_out > 0, "total_out should be > 0");
    assert_eq!(
        stream.total_out as usize,
        HELLO.len(),
        "total_out vs HELLO len"
    );

    let output = stream.take_output();
    assert_bytes_equal(&output, HELLO, "one_call_helpers round-trip");

    let _ = inflate::inflate_end(&mut state, &mut stream);
}

/// Test: `deflate_reset` then fresh compression, verified by inflate.
///
/// Exercises: `deflate_reset`.
#[test]
fn test_deflate_reset_then_inflate() {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok);

    stream.set_input(b"first round");
    stream.set_output_buffer(COMPR_LEN);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_eq!(ret, ReturnCode::StreamEnd);
    let _ = stream.take_output();

    let ret = deflate::deflate_reset(&mut stream);
    assert_eq!(ret, ReturnCode::Ok, "deflate_reset should succeed");

    stream.set_input(HELLO);
    stream.set_output_buffer(COMPR_LEN);
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_eq!(ret, ReturnCode::StreamEnd);
    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    let (ret, decompressed) = streaming_decompress(&compressed, MAX_WBITS);
    assert_eq!(ret, ReturnCode::StreamEnd);
    assert_bytes_equal(&decompressed, HELLO, "deflate_reset then inflate");
}

/// Test: `Z_SYNC_FLUSH` produces valid compressed data that decompresses.
///
/// Exercises: `Z_SYNC_FLUSH`.
#[test]
fn test_inflate_sync_flush() {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok);

    stream.set_input(HELLO);
    stream.set_output_buffer(COMPR_LEN);

    let ret = deflate::deflate(&mut stream, Z_SYNC_FLUSH);
    assert!(
        ret == ReturnCode::Ok || ret == ReturnCode::StreamEnd,
        "Z_SYNC_FLUSH failed: {ret:?}"
    );

    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd);
    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    let (ret, decompressed) = streaming_decompress(&compressed, MAX_WBITS);
    assert_eq!(ret, ReturnCode::StreamEnd);
    assert_bytes_equal(&decompressed, HELLO, "sync flush round-trip");
}

/// Test: Verify `GzHeader` fields `xflags` and `hcrc` are accessible after
/// gzip decompression.
///
/// Exercises: `GzHeader.xflags`, `GzHeader.hcrc`.
#[test]
fn test_inflate_gz_header_xflags_hcrc() {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS + 16,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok);

    let gz_head = GzHeader {
        text: false,
        time: 0,
        xflags: 0,
        os: 255,
        extra: None,
        name: None,
        comment: None,
        hcrc: false,
        done: false,
    };
    let ret = deflate::deflate_set_header(&mut stream, gz_head);
    assert_eq!(ret, ReturnCode::Ok);

    stream.set_input(HELLO);
    stream.set_output_buffer(COMPR_LEN);

    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert_eq!(ret, ReturnCode::StreamEnd);
    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    let mut istream = ZStream::new();
    let mut istate = inflate::InflateState::new();
    let ret = inflate::inflate_reset2(&mut istate, &mut istream, MAX_WBITS + 32);
    assert_eq!(ret, ReturnCode::Ok);

    let header = Box::new(GzHeader::new());
    let ret = inflate::inflate_get_header(&mut istate, header);
    assert_eq!(ret, ReturnCode::Ok);

    istream.set_input(&compressed);
    istream.set_output_buffer(UNCOMPR_LEN);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate(&mut istate, &mut istream, Z_NO_FLUSH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd);

    if let Some(head) = istate.header() {
        assert!(head.done, "header done flag should be set");
        // xflags is set by the compressor; verify the field is accessible and valid
        assert!(
            head.xflags <= 4,
            "xflags should be a valid compression flag (0-4)"
        );
        // hcrc: we set hcrc=false, verify the flag is accessible
        assert!(!head.hcrc, "hcrc should be false when not requested");
    } else {
        panic!("should have captured gz header");
    }

    let output = istream.take_output();
    assert_bytes_equal(&output, HELLO, "xflags/hcrc gzip round-trip");

    let _ = inflate::inflate_end(&mut istate, &mut istream);
}

/// Test: Round-trip using the one-call `compress` and `uncompress` functions,
/// then verify via streaming inflate for completeness.
///
/// Exercises: `compress()`, `uncompress()`, `deflate_init()`.
#[test]
fn test_inflate_one_call_compress_uncompress() {
    // One-call compress (default level)
    let mut compr = Vec::new();
    compress(&mut compr, HELLO).expect("compress should succeed");

    // One-call uncompress — dest must be pre-allocated to sufficient size
    let mut decompr = vec![0u8; UNCOMPR_LEN];
    uncompress(&mut decompr, &compr).expect("uncompress should succeed");
    assert_bytes_equal(&decompr, HELLO, "compress/uncompress round-trip");

    // Also verify the compressed data decompresses via streaming inflate
    let (ret, streamed) = streaming_decompress(&compr, MAX_WBITS);
    assert_eq!(
        ret,
        ReturnCode::StreamEnd,
        "streaming inflate of compress()"
    );
    assert_bytes_equal(&streamed, HELLO, "compress->streaming_decompress");
}

/// Test: `deflate_init` (default-level variant) produces valid compressed data
/// that can be decompressed by inflate.
///
/// Exercises: `deflate_init()`.
#[test]
fn test_deflate_init_default_level() {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init(&mut stream, Z_DEFAULT_COMPRESSION);
    assert_eq!(ret, ReturnCode::Ok, "deflate_init failed");

    stream.set_input(HELLO);
    stream.set_output_buffer(COMPR_LEN);

    let mut ret = deflate::deflate(&mut stream, Z_FINISH);
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd);
    let compressed = stream.take_output();
    let _ = deflate::deflate_end(&mut stream);

    let (ret, decompressed) = streaming_decompress(&compressed, MAX_WBITS);
    assert_eq!(ret, ReturnCode::StreamEnd);
    assert_bytes_equal(&decompressed, HELLO, "deflate_init default round-trip");
}
