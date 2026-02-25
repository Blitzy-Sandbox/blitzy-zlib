// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate — regression test suite
// SPDX-License-Identifier: Zlib
//
// Port of C `test/example.c` (553 lines, 10 ordered regression test functions).
// Exercises virtually every zlib API group: compress/uncompress, gzip I/O,
// streaming deflate/inflate, flush modes, sync recovery, and dictionary support.
//
// Per AAP §0.5.1 — target: tests/regression.rs
// Per AAP §0.8.2 — testing parity with C test/example.c

// ============================================================================
// Imports
// ============================================================================

use zlib_rs::*;

// std::path is used indirectly via .as_path() and std::env::temp_dir().
// std::fs::remove_file is used for cleanup of gzip temp files.

// ============================================================================
// Shared test data — matching C example.c globals
// ============================================================================

/// Matching C: `static z_const char hello[] = "hello, hello!";`
/// Note: C strlen(hello)+1 = 14 (includes null terminator).
/// In Rust we use the raw bytes without a null terminator for most operations,
/// but match the C len of 14 where needed by appending a zero byte.
const HELLO: &[u8] = b"hello, hello!";

/// C-compatible length including null terminator: strlen("hello, hello!")+1 = 14
const HELLO_LEN: usize = HELLO.len() + 1; // 14

/// Matching C: `static const char dictionary[] = "hello";`
/// In C, sizeof(dictionary) = 6 (includes null terminator).
const DICTIONARY: &[u8] = b"hello\0";

/// Buffer sizes matching C main(): uncomprLen = 20000, comprLen = 3 * uncomprLen
const UNCOMPR_LEN: usize = 20_000;
const COMPR_LEN: usize = 3 * UNCOMPR_LEN;

// ============================================================================
// Helper — CHECK_ERR equivalent
// ============================================================================

/// Asserts that a `ZlibResult` is `Ok`. Panics with a descriptive message if
/// the result is an error, mirroring C's `CHECK_ERR(err, msg)` macro.
fn check_ok(result: ZlibResult, msg: &str) {
    match result {
        Ok(_) => {}
        Err(e) => panic!("{} error: {:?}", msg, e),
    }
}

/// Returns the hello data with a trailing null byte, matching C `strlen(hello)+1`.
fn hello_with_null() -> Vec<u8> {
    let mut v = HELLO.to_vec();
    v.push(0);
    v
}

// ============================================================================
// Test 0: Version check (C main() lines 497-513)
// ============================================================================

#[test]
fn test_version() {
    // C: if (zlibVersion()[0] != myVersion[0]) { ... }
    let version = zlib_version();
    assert!(
        !version.is_empty(),
        "zlib_version() returned empty string"
    );

    // Verify the version string matches the compile-time constant.
    assert_eq!(
        version, ZLIB_VERSION,
        "runtime zlib_version() '{}' does not match compile-time ZLIB_VERSION '{}'",
        version, ZLIB_VERSION
    );

    // Verify ZLIB_VERNUM as specified in AAP.
    assert_eq!(
        ZLIB_VERNUM, 0x1321,
        "ZLIB_VERNUM mismatch: expected 0x1321, got {:#06x}",
        ZLIB_VERNUM
    );
}

// ============================================================================
// Test 1: test_compress (C lines 66-85)
// ============================================================================

#[test]
fn test_compress() {
    let hello_data = hello_with_null();
    let mut compr = vec![0u8; COMPR_LEN];
    let mut uncompr = vec![0u8; UNCOMPR_LEN];

    // compress(compr, &comprLen, hello, len)
    let compr_used = compress(&mut compr, &hello_data)
        .expect("compress error");
    assert!(compr_used > 0, "compress produced zero bytes");

    // Garbage-fill uncompr before decompression — matches C: strcpy(uncompr, "garbage")
    uncompr[..7].copy_from_slice(b"garbage");

    // uncompress(uncompr, &uncomprLen, compr, comprLen)
    let uncompr_used = uncompress(&mut uncompr, &compr[..compr_used])
        .expect("uncompress error");

    // Verify decompressed data matches original.
    assert_eq!(
        &uncompr[..uncompr_used],
        &hello_data[..],
        "bad uncompress: data mismatch"
    );
}

// ============================================================================
// Test 2: test_gzio (C lines 90-164)
// ============================================================================

#[test]
fn test_gzio() {
    // Construct a temporary file path for the gzip test.
    let temp_dir = std::env::temp_dir();
    let gz_path = temp_dir.join("zlib_rs_regression_test.gz");

    // === Write phase (C lines 99-114) ===
    {
        let mut file = gz_open(gz_path.as_path(), "wb")
            .expect("gzopen error (write)");

        // gzputc(file, 'h')
        gz_putc(&mut file, b'h').expect("gzputc error");

        // gzputs(file, "ello") → verify returns 4
        let puts_result = gz_puts(&mut file, "ello").expect("gzputs error");
        assert_eq!(puts_result, 4, "gzputs err: expected 4, got {}", puts_result);

        // gzprintf(file, ", %s!", "hello") → verify returns 8
        let formatted = format!(", {}!", "hello");
        let printf_result = gz_printf(&mut file, &formatted).expect("gzprintf error");
        assert_eq!(printf_result, 8, "gzprintf err: expected 8, got {}", printf_result);

        // gzseek(file, 1L, SEEK_CUR) — add one zero byte
        // SEEK_CUR = 1
        gz_seek(&mut file, 1, 1).expect("gzseek error (write)");

        // gzclose
        gz_close(&mut file).expect("gzclose error (write)");
    }

    // === Read phase (C lines 116-163) ===
    {
        let mut file = gz_open(gz_path.as_path(), "rb")
            .expect("gzopen error (read)");

        let mut uncompr = vec![0u8; UNCOMPR_LEN];
        // Garbage-fill: strcpy(uncompr, "garbage")
        uncompr[..7].copy_from_slice(b"garbage");

        // gzread(file, uncompr, uncomprLen) → verify returns len (14)
        let read_len = gz_read(&mut file, &mut uncompr)
            .expect("gzread error");
        assert_eq!(
            read_len, HELLO_LEN,
            "gzread err: expected {}, got {}",
            HELLO_LEN, read_len
        );

        // Verify content matches hello (with null terminator).
        let hello_null = hello_with_null();
        assert_eq!(
            &uncompr[..hello_null.len()],
            &hello_null[..],
            "bad gzread: content mismatch"
        );

        // gzseek(file, -8L, SEEK_CUR) → pos must be 6
        let pos = gz_seek(&mut file, -8, 1)
            .expect("gzseek error (read)");
        assert_eq!(pos, 6, "gzseek error, pos={}", pos);

        // gztell must equal pos
        let tell_pos = gz_tell(&file);
        assert_eq!(tell_pos, pos, "gzseek error, gztell={}", tell_pos);

        // gzgetc → must return ' ' (space, byte 0x20)
        let ch = gz_getc(&mut file).expect("gzgetc error");
        assert_eq!(ch, Some(b' '), "gzgetc error: expected ' ', got {:?}", ch);

        // gzungetc(' ', file) → must return ' '
        let unch = gz_ungetc(&mut file, b' ').expect("gzungetc error");
        assert_eq!(unch, b' ', "gzungetc error: expected ' ', got {}", unch);

        // gzgets(file, uncompr, uncomprLen)
        let mut gets_buf = vec![0u8; UNCOMPR_LEN];
        let gets_len = gz_gets(&mut file, &mut gets_buf)
            .expect("gzgets error");
        // The Rust gz_gets returns the raw byte count (includes embedded null
        // from gz_seek), while C uses strlen which stops at null. The C test
        // checks strlen == 7 for " hello!"; the total bytes read by Rust
        // is 8 (including the trailing '\0' from gz_seek(1, SEEK_CUR)).
        // Verify content: first 7 bytes must be " hello!"
        assert!(
            gets_len >= 7,
            "gzgets err after gzseek: expected >= 7 bytes, got {}",
            gets_len
        );
        let expected_tail = b" hello!";
        assert_eq!(
            &gets_buf[..expected_tail.len()],
            &expected_tail[..],
            "bad gzgets after gzseek"
        );

        gz_close(&mut file).expect("gzclose error (read)");
    }

    // Clean up temp file.
    let _ = std::fs::remove_file(&gz_path);
}

// ============================================================================
// Test 3+4: test_deflate + test_inflate (C lines 172-241)
// These are combined because test_inflate needs test_deflate's output.
// ============================================================================

#[test]
fn test_deflate_inflate() {
    let hello_data = hello_with_null();
    let mut compr = vec![0u8; COMPR_LEN];
    let compr_len = COMPR_LEN;

    // === test_deflate (C lines 172-202): streaming with small buffers ===
    let compressed_len: usize;
    {
        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION),
            "deflateInit",
        );

        c_stream.set_input(&hello_data);
        c_stream.set_output(&mut compr);

        // Force small buffers: avail_in = avail_out = 1 per iteration.
        while (c_stream.total_in as usize) != hello_data.len()
            && (c_stream.total_out as usize) < compr_len
        {
            c_stream.avail_in = 1;
            c_stream.avail_out = 1;
            check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate");
        }

        // Finish the stream, still forcing small buffers.
        loop {
            c_stream.avail_out = 1;
            let result = deflate(&mut c_stream, Z_FINISH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "deflate finish"),
            }
        }

        compressed_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd");
    }

    // === test_inflate (C lines 207-241): streaming with small buffers ===
    {
        let mut uncompr = vec![0u8; UNCOMPR_LEN];
        // Garbage-fill.
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..compressed_len]);
        d_stream.avail_in = 0;
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit");

        while (d_stream.total_out as usize) < UNCOMPR_LEN
            && (d_stream.total_in as usize) < compressed_len
        {
            d_stream.avail_in = 1;
            d_stream.avail_out = 1;
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "inflate"),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd");

        // Verify decompressed data matches original.
        let decompressed_len = d_stream.total_out as usize;
        assert_eq!(
            &uncompr[..decompressed_len],
            &hello_data[..],
            "bad inflate: data mismatch"
        );
    }
}

// ============================================================================
// Test 5+6: test_large_deflate + test_large_inflate (C lines 246-333)
// Combined because test_large_inflate needs test_large_deflate's output.
// ============================================================================

#[test]
fn test_large_deflate_inflate() {
    let mut compr = vec![0u8; COMPR_LEN];
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    let _compr_len = COMPR_LEN;
    let uncompr_len = UNCOMPR_LEN;

    // === test_large_deflate (C lines 246-294) ===
    let compressed_len: usize;
    {
        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_BEST_SPEED),
            "deflateInit (large)",
        );

        c_stream.set_output(&mut compr);

        // First pass: Feed uncompr (mostly zeros, should compress well).
        c_stream.set_input(&uncompr);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 1)");
        assert_eq!(
            c_stream.avail_in, 0,
            "deflate not greedy"
        );

        // Switch to no compression and feed compressed data back.
        // C: deflateParams(&c_stream, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY)
        // NOTE: In C test/example.c, the return value of deflateParams is
        // intentionally NOT checked with CHECK_ERR. It may return Z_BUF_ERROR
        // which is a non-fatal condition (the params change still takes effect).
        let _ = deflate_params(&mut c_stream, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY);

        // Feed compr data (up to uncomprLen/2 bytes) — use a snapshot of compr.
        let pass2_data: Vec<u8> = compr[..uncompr_len / 2].to_vec();
        c_stream.set_input(&pass2_data);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 2)");

        // Switch back to best compression with filtered strategy.
        // NOTE: Return value intentionally not checked (matches C test/example.c).
        let _ = deflate_params(&mut c_stream, Z_BEST_COMPRESSION, Z_FILTERED);
        // Feed uncompr again.
        c_stream.set_input(&uncompr);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 3)");

        // Finish.
        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => panic!("deflate should report Z_STREAM_END"),
        }

        compressed_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd (large)");
    }

    // === test_large_inflate (C lines 299-333) ===
    {
        // Garbage-fill uncompr.
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..compressed_len]);

        check_ok(inflate_init(&mut d_stream), "inflateInit (large)");

        loop {
            d_stream.set_output(&mut uncompr); // discard the output each iteration
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "large inflate"),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (large)");

        // Critical check: total_out must equal 2*uncomprLen + uncomprLen/2.
        // This validates that the large deflate's multi-section output decompresses correctly.
        let expected_total = 2 * uncompr_len + uncompr_len / 2;
        assert_eq!(
            d_stream.total_out as usize, expected_total,
            "bad large inflate: total_out={}, expected={}",
            d_stream.total_out, expected_total
        );
    }
}

// ============================================================================
// Test 7+8: test_flush + test_sync (C lines 338-409)
// Combined because test_sync needs test_flush's corrupted compressed output.
// ============================================================================

#[test]
fn test_flush_sync() {
    let hello_data = hello_with_null();
    let mut compr = vec![0u8; COMPR_LEN];

    // === test_flush (C lines 338-368) ===
    let flush_compr_len: usize;
    {
        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION),
            "deflateInit (flush)",
        );

        c_stream.set_input(&hello_data[..3]); // First 3 bytes: "hel"
        c_stream.set_output(&mut compr);

        // Compress first 3 bytes with Z_FULL_FLUSH.
        check_ok(deflate(&mut c_stream, Z_FULL_FLUSH), "deflate (full flush)");

        // Corrupt data: compr[3] += 1 (force error in first compressed block).
        compr[3] = compr[3].wrapping_add(1);

        // Feed remaining bytes and finish.
        c_stream.set_input(&hello_data[3..]);
        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) | Ok(ReturnCode::Ok) | Ok(ReturnCode::NeedDict) => {}
            Err(e) => panic!("deflate (finish after flush) error: {:?}", e),
        }

        check_ok(deflate_end(&mut c_stream), "deflateEnd (flush)");
        flush_compr_len = c_stream.total_out as usize;
    }

    // === test_sync (C lines 373-409) ===
    {
        let mut uncompr = vec![0u8; UNCOMPR_LEN];
        // Garbage-fill.
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        // Feed only 2 bytes initially (just the zlib header).
        d_stream.set_input(&compr[..2]);
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit (sync)");

        // inflate to process the header.
        check_ok(inflate(&mut d_stream, Z_NO_FLUSH), "inflate (header)");

        // Now feed remaining corrupted data.
        d_stream.set_input(&compr[2..flush_compr_len]);

        // inflateSync — skip the damaged block.
        check_ok(inflate_sync(&mut d_stream), "inflateSync");

        // inflate(Z_FINISH) — should reach Z_STREAM_END.
        let result = inflate(&mut d_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            Ok(ReturnCode::Ok) => {
                panic!("inflate should report Z_STREAM_END");
            }
            Ok(ReturnCode::NeedDict) => {
                panic!("inflate unexpectedly returned NeedDict");
            }
            Err(e) => panic!("inflate (finish after sync) error: {:?}", e),
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (sync)");

        // The output after sync recovery should contain partial data.
        // C: printf("after inflateSync(): hel%s\n", (char *)uncompr);
        // The first 3 bytes before corruption were "hel", then sync recovery
        // picks up the rest. The exact content depends on recovery, but
        // total_out should be > 0 showing some data was recovered.
        assert!(
            d_stream.total_out > 0,
            "inflateSync recovery produced no output"
        );
    }
}

// ============================================================================
// Test 9+10: test_dict_deflate + test_dict_inflate (C lines 414-491)
// Combined because test_dict_inflate needs test_dict_deflate's output.
// ============================================================================

#[test]
fn test_dict_deflate_inflate() {
    let hello_data = hello_with_null();
    let mut compr = vec![0u8; COMPR_LEN];

    // === test_dict_deflate (C lines 414-443) ===
    let dict_compr_len: usize;
    let dict_id: u64;
    {
        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_BEST_COMPRESSION),
            "deflateInit (dict)",
        );

        // deflateSetDictionary — C passes sizeof(dictionary) which includes '\0'.
        check_ok(
            deflate_set_dictionary(&mut c_stream, DICTIONARY),
            "deflateSetDictionary",
        );

        // Save Adler-32 of the dictionary for later verification.
        dict_id = c_stream.adler;

        c_stream.set_output(&mut compr);
        c_stream.set_input(&hello_data);

        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => panic!("deflate should report Z_STREAM_END (dict)"),
        }

        dict_compr_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd (dict)");
    }

    // === test_dict_inflate (C lines 448-491) ===
    {
        let mut uncompr = vec![0u8; UNCOMPR_LEN];
        // Garbage-fill.
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..dict_compr_len]);
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit (dict)");

        loop {
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::NeedDict) => {
                    // Verify dictionary ID matches.
                    assert_eq!(
                        d_stream.adler, dict_id,
                        "unexpected dictionary: adler={}, expected={}",
                        d_stream.adler, dict_id
                    );
                    check_ok(
                        inflate_set_dictionary(&mut d_stream, DICTIONARY),
                        "inflateSetDictionary",
                    );
                }
                Ok(ReturnCode::Ok) => continue,
                Err(e) => panic!("inflate with dict error: {:?}", e),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (dict)");

        // Verify decompressed data matches original.
        let decompressed_len = d_stream.total_out as usize;
        assert_eq!(
            &uncompr[..decompressed_len],
            &hello_data[..],
            "bad inflate with dict: data mismatch"
        );
    }
}

// ============================================================================
// Full regression sequence — runs all 10 tests in order within a single
// function, sharing compr/uncompr buffers, matching the C main() flow exactly.
// (C example.c lines 529-546)
// ============================================================================

#[test]
#[allow(unused_assignments)]
fn full_regression_sequence() {
    let hello_data = hello_with_null();
    let mut compr = vec![0u8; COMPR_LEN];
    let mut uncompr = vec![0u8; UNCOMPR_LEN];
    let mut compr_len: usize = COMPR_LEN;
    let uncompr_len: usize = UNCOMPR_LEN;

    // -----------------------------------------------------------------------
    // Version check (C main() lines 497-513)
    // -----------------------------------------------------------------------
    {
        let version = zlib_version();
        assert!(!version.is_empty());
        assert_eq!(version, ZLIB_VERSION);
        assert_eq!(ZLIB_VERNUM, 0x1321);
    }

    // -----------------------------------------------------------------------
    // 1. test_compress (C lines 66-85)
    // -----------------------------------------------------------------------
    {
        let compr_used = compress(&mut compr, &hello_data)
            .expect("compress error");
        assert!(compr_used > 0);

        uncompr[..7].copy_from_slice(b"garbage");
        let uncompr_used = uncompress(&mut uncompr, &compr[..compr_used])
            .expect("uncompress error");
        assert_eq!(
            &uncompr[..uncompr_used],
            &hello_data[..],
            "bad uncompress"
        );
    }

    // -----------------------------------------------------------------------
    // 2. test_gzio (C lines 90-164)
    // -----------------------------------------------------------------------
    {
        let temp_dir = std::env::temp_dir();
        let gz_path = temp_dir.join("zlib_rs_full_regression.gz");

        // Write phase.
        {
            let mut file = gz_open(gz_path.as_path(), "wb")
                .expect("gzopen error (write)");

            gz_putc(&mut file, b'h').expect("gzputc error");
            let puts_result = gz_puts(&mut file, "ello").expect("gzputs error");
            assert_eq!(puts_result, 4, "gzputs err");

            let formatted = format!(", {}!", "hello");
            let printf_result = gz_printf(&mut file, &formatted).expect("gzprintf error");
            assert_eq!(printf_result, 8, "gzprintf err");

            gz_seek(&mut file, 1, 1).expect("gzseek error");
            gz_close(&mut file).expect("gzclose error (write)");
        }

        // Read phase.
        {
            let mut file = gz_open(gz_path.as_path(), "rb")
                .expect("gzopen error (read)");

            uncompr[..7].copy_from_slice(b"garbage");
            let read_len = gz_read(&mut file, &mut uncompr)
                .expect("gzread error");
            assert_eq!(read_len, HELLO_LEN, "gzread length mismatch");

            let hello_null = hello_with_null();
            assert_eq!(
                &uncompr[..hello_null.len()],
                &hello_null[..],
                "bad gzread"
            );

            let pos = gz_seek(&mut file, -8, 1)
                .expect("gzseek error (read)");
            assert_eq!(pos, 6, "gzseek error, pos={}", pos);
            assert_eq!(gz_tell(&file), pos, "gztell mismatch");

            let ch = gz_getc(&mut file).expect("gzgetc error");
            assert_eq!(ch, Some(b' '), "gzgetc error");

            let unch = gz_ungetc(&mut file, b' ').expect("gzungetc error");
            assert_eq!(unch, b' ', "gzungetc error");

            let mut gets_buf = vec![0u8; UNCOMPR_LEN];
            let gets_len = gz_gets(&mut file, &mut gets_buf)
                .expect("gzgets error");
            // Rust gz_gets returns raw byte count (>=7), C checks strlen==7.
            assert!(gets_len >= 7, "gzgets err after gzseek: len={}", gets_len);
            assert_eq!(
                &gets_buf[..7],
                b" hello!",
                "bad gzgets after gzseek"
            );

            gz_close(&mut file).expect("gzclose error (read)");
        }

        let _ = std::fs::remove_file(&gz_path);
    }

    // -----------------------------------------------------------------------
    // 3. test_deflate (C lines 172-202)
    // -----------------------------------------------------------------------
    {
        compr = vec![0u8; COMPR_LEN];
        compr_len = COMPR_LEN;

        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION),
            "deflateInit",
        );

        c_stream.set_input(&hello_data);
        c_stream.set_output(&mut compr);

        while (c_stream.total_in as usize) != hello_data.len()
            && (c_stream.total_out as usize) < compr_len
        {
            c_stream.avail_in = 1;
            c_stream.avail_out = 1;
            check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate");
        }

        loop {
            c_stream.avail_out = 1;
            let result = deflate(&mut c_stream, Z_FINISH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "deflate finish"),
            }
        }

        compr_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd");
    }

    // -----------------------------------------------------------------------
    // 4. test_inflate (C lines 207-241)
    // -----------------------------------------------------------------------
    {
        uncompr = vec![0u8; UNCOMPR_LEN];
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..compr_len]);
        d_stream.avail_in = 0;
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit");

        while (d_stream.total_out as usize) < uncompr_len
            && (d_stream.total_in as usize) < compr_len
        {
            d_stream.avail_in = 1;
            d_stream.avail_out = 1;
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "inflate"),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd");

        let decompressed_len = d_stream.total_out as usize;
        assert_eq!(
            &uncompr[..decompressed_len],
            &hello_data[..],
            "bad inflate"
        );
    }

    // -----------------------------------------------------------------------
    // 5. test_large_deflate (C lines 246-294)
    // -----------------------------------------------------------------------
    {
        compr = vec![0u8; COMPR_LEN];
        uncompr = vec![0u8; UNCOMPR_LEN];
        compr_len = COMPR_LEN;

        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_BEST_SPEED),
            "deflateInit (large)",
        );

        c_stream.set_output(&mut compr);

        // First pass: feed uncompr (mostly zeros).
        c_stream.set_input(&uncompr);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 1)");
        assert_eq!(c_stream.avail_in, 0, "deflate not greedy");

        // Switch to no compression.
        // NOTE: Return value intentionally not checked (matches C test).
        let _ = deflate_params(&mut c_stream, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY);
        let pass2_data: Vec<u8> = compr[..uncompr_len / 2].to_vec();
        c_stream.set_input(&pass2_data);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 2)");

        // Switch to best compression.
        // NOTE: Return value intentionally not checked (matches C test).
        let _ = deflate_params(&mut c_stream, Z_BEST_COMPRESSION, Z_FILTERED);
        c_stream.set_input(&uncompr);
        check_ok(deflate(&mut c_stream, Z_NO_FLUSH), "deflate (large, pass 3)");

        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => panic!("deflate should report Z_STREAM_END (large)"),
        }

        compr_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd (large)");
    }

    // -----------------------------------------------------------------------
    // 6. test_large_inflate (C lines 299-333)
    // -----------------------------------------------------------------------
    {
        uncompr = vec![0u8; UNCOMPR_LEN];
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..compr_len]);

        check_ok(inflate_init(&mut d_stream), "inflateInit (large)");

        loop {
            d_stream.set_output(&mut uncompr);
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::Ok) => continue,
                other => check_ok(other, "large inflate"),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (large)");

        let expected_total = 2 * uncompr_len + uncompr_len / 2;
        assert_eq!(
            d_stream.total_out as usize, expected_total,
            "bad large inflate: total_out={}, expected={}",
            d_stream.total_out, expected_total
        );
    }

    // -----------------------------------------------------------------------
    // 7. test_flush (C lines 338-368)
    // -----------------------------------------------------------------------
    {
        compr = vec![0u8; COMPR_LEN];
        compr_len = COMPR_LEN;

        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_DEFAULT_COMPRESSION),
            "deflateInit (flush)",
        );

        c_stream.set_input(&hello_data[..3]);
        c_stream.set_output(&mut compr);

        check_ok(deflate(&mut c_stream, Z_FULL_FLUSH), "deflate (full flush)");

        // Corrupt data to force error in first compressed block.
        compr[3] = compr[3].wrapping_add(1);

        c_stream.set_input(&hello_data[3..]);
        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) | Ok(ReturnCode::Ok) | Ok(ReturnCode::NeedDict) => {}
            Err(e) => panic!("deflate (finish after flush) error: {:?}", e),
        }

        check_ok(deflate_end(&mut c_stream), "deflateEnd (flush)");
        compr_len = c_stream.total_out as usize;
    }

    // -----------------------------------------------------------------------
    // 8. test_sync (C lines 373-409)
    // -----------------------------------------------------------------------
    {
        uncompr = vec![0u8; UNCOMPR_LEN];
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..2]);
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit (sync)");

        check_ok(inflate(&mut d_stream, Z_NO_FLUSH), "inflate (header)");

        d_stream.set_input(&compr[2..compr_len]);

        check_ok(inflate_sync(&mut d_stream), "inflateSync");

        let result = inflate(&mut d_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            Ok(ReturnCode::Ok) => {
                panic!("inflate should report Z_STREAM_END (sync)");
            }
            Ok(ReturnCode::NeedDict) => {
                panic!("inflate unexpectedly returned NeedDict (sync)");
            }
            Err(e) => panic!("inflate (finish after sync) error: {:?}", e),
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (sync)");

        assert!(
            d_stream.total_out > 0,
            "inflateSync recovery produced no output"
        );
    }

    // Reset comprLen for dictionary tests (C: comprLen = 3 * uncomprLen).
    compr_len = 3 * uncompr_len;

    // -----------------------------------------------------------------------
    // 9. test_dict_deflate (C lines 414-443)
    // -----------------------------------------------------------------------
    let dict_id: u64;
    {
        compr = vec![0u8; compr_len];

        let mut c_stream = ZStream::new();
        check_ok(
            deflate_init(&mut c_stream, Z_BEST_COMPRESSION),
            "deflateInit (dict)",
        );

        check_ok(
            deflate_set_dictionary(&mut c_stream, DICTIONARY),
            "deflateSetDictionary",
        );

        dict_id = c_stream.adler;

        c_stream.set_output(&mut compr);
        c_stream.set_input(&hello_data);

        let result = deflate(&mut c_stream, Z_FINISH);
        match result {
            Ok(ReturnCode::StreamEnd) => {}
            _ => panic!("deflate should report Z_STREAM_END (dict)"),
        }

        compr_len = c_stream.total_out as usize;
        check_ok(deflate_end(&mut c_stream), "deflateEnd (dict)");
    }

    // -----------------------------------------------------------------------
    // 10. test_dict_inflate (C lines 448-491)
    // -----------------------------------------------------------------------
    {
        uncompr = vec![0u8; UNCOMPR_LEN];
        uncompr[..7].copy_from_slice(b"garbage");

        let mut d_stream = ZStream::new();
        d_stream.set_input(&compr[..compr_len]);
        d_stream.set_output(&mut uncompr);

        check_ok(inflate_init(&mut d_stream), "inflateInit (dict)");

        loop {
            let result = inflate(&mut d_stream, Z_NO_FLUSH);
            match result {
                Ok(ReturnCode::StreamEnd) => break,
                Ok(ReturnCode::NeedDict) => {
                    assert_eq!(
                        d_stream.adler, dict_id,
                        "unexpected dictionary"
                    );
                    check_ok(
                        inflate_set_dictionary(&mut d_stream, DICTIONARY),
                        "inflateSetDictionary",
                    );
                }
                Ok(ReturnCode::Ok) => continue,
                Err(e) => panic!("inflate with dict error: {:?}", e),
            }
        }

        check_ok(inflate_end(&mut d_stream), "inflateEnd (dict)");

        let decompressed_len = d_stream.total_out as usize;
        assert_eq!(
            &uncompr[..decompressed_len],
            &hello_data[..],
            "bad inflate with dict"
        );
    }
}
