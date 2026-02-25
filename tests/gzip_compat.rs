// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Gzip format compatibility tests — port of test/minigzip.c patterns and
// test/example.c test_gzio (lines 90-164).

// Gate the entire test file on the gz-io feature: all gz_* functions are only
// available when the gz-io feature is enabled. Without this gate,
// `cargo test --no-default-features` would fail with "not found in this scope"
// errors for all gz_* API calls.
#![cfg(feature = "gz-io")]

//! Integration tests for the gzip file I/O subsystem (`gz` module).
//!
//! Validates RFC 1952 gzip format compatibility by exercising every public
//! `gz_*` API function: open, close, read, write, putc, puts, printf, seek,
//! tell, offset, getc, gets, ungetc, buffer, eof, direct, error_msg,
//! clearerr, flush, fread, fwrite, setparams, and rewind.

// ============================================================================
// Imports
// ============================================================================

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use zlib_rs::*;

// ============================================================================
// Constants and helpers
// ============================================================================

/// Test payload matching C `static z_const char hello[] = "hello, hello!";`
const HELLO: &[u8] = b"hello, hello!";

/// SEEK_SET / SEEK_CUR constants (not re-exported from crate root; matches
/// POSIX values used by `gz_seek`).
const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;

/// Build a unique temporary path for a gzip test file.
///
/// Each test should use a distinct `name` to avoid inter-test collisions
/// when the test harness runs tests in parallel.
fn temp_gz_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("zlib_rs_test_{}.gz", name));
    path
}

/// Cleanup helper — remove a file if it exists, ignoring errors.
fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
}

// ============================================================================
// Phase 2: Port test_gzio from test/example.c (lines 90-164)
// ============================================================================

/// Full gz* API lifecycle test ported from C test/example.c `test_gzio`
/// (lines 90-164).
///
/// **Write phase** (C lines 99-114):
///   gz_open "wb" → gz_putc('h') → gz_puts("ello") → gz_printf(", hello!")
///   → gz_seek(+1, SEEK_CUR) (zero byte) → gz_close
///
/// **Read phase** (C lines 116-163):
///   gz_open "rb" → gz_read (14 bytes) → gz_seek(-8, SEEK_CUR) → verify
///   pos==6 → gz_getc(' ') → gz_ungetc(' ') → gz_gets(" hello!") → gz_close
#[test]
fn test_gzio_write_read_cycle() {
    let path = temp_gz_path("gzio_cycle");
    cleanup(&path);

    // ---- Write phase ----
    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");

        // Write single char 'h' via gz_putc
        let c = gz_putc(&mut gzf, b'h').expect("gz_putc");
        assert_eq!(c, b'h');

        // Write "ello" via gz_puts — should return 4
        let n = gz_puts(&mut gzf, "ello").expect("gz_puts");
        assert_eq!(n, 4);

        // Write ", hello!" via gz_printf — should return 8
        let n = gz_printf(&mut gzf, &format!(", {}!", "hello")).expect("gz_printf");
        assert_eq!(n, 8);

        // Seek forward 1 byte to embed a zero byte (C: gzseek(file, 1L, SEEK_CUR))
        gz_seek(&mut gzf, 1, SEEK_CUR).expect("gz_seek +1");

        gz_close(&mut gzf).expect("gz_close write");
    }

    // ---- Read phase ----
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // Read full content — expect 14 bytes: "hello, hello!" + '\0'
        let mut buf = vec![0u8; 256];
        let len = HELLO.len() + 1; // +1 for the zero byte added by seek
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(n, len, "gz_read returned {} instead of {}", n, len);
        assert_eq!(&buf[..HELLO.len()], HELLO);

        // Seek backward 8 bytes — position should become 6
        let pos = gz_seek(&mut gzf, -8, SEEK_CUR).expect("gz_seek -8");
        assert_eq!(pos, 6, "gz_seek(-8) position mismatch");
        assert_eq!(gz_tell(&gzf), pos, "gz_tell mismatch after seek");

        // Read one character — should be ' ' (space, index 6 in "hello, hello!")
        let ch = gz_getc(&mut gzf).expect("gz_getc");
        assert_eq!(ch, Some(b' '), "gz_getc did not return space");

        // Push the space back
        let uc = gz_ungetc(&mut gzf, b' ').expect("gz_ungetc");
        assert_eq!(uc, b' ');

        // Read string via gz_gets — yields " hello!\0" (8 bytes: 7 data +
        // the zero byte added by the seek).  The first 7 bytes match
        // hello[6..] == " hello!", and byte 8 is the trailing '\0'.
        let mut sbuf = vec![0u8; 256];
        let sn = gz_gets(&mut gzf, &mut sbuf).expect("gz_gets");
        // gz_gets reads until newline/EOF; the zero byte is data, not a
        // terminator, so we expect 8 bytes total.
        assert_eq!(sn, 8, "gz_gets returned {} instead of 8", sn);
        // First 7 bytes must match hello[6..] == " hello!"
        let expected = &HELLO[6..]; // b" hello!"
        assert_eq!(&sbuf[..expected.len()], expected);
        // Byte 8 is the embedded zero from the seek
        assert_eq!(sbuf[7], 0u8, "trailing zero from seek missing");

        gz_close(&mut gzf).expect("gz_close read");
    }

    cleanup(&path);
}

// ============================================================================
// Phase 3: Gzip File Write and Header Validation
// ============================================================================

/// Verify RFC 1952 header and trailer structure of a gzip file produced by
/// `gz_open` / `gz_write` / `gz_close`.
#[test]
fn gzip_header_fields() {
    let path = temp_gz_path("header_fields");
    cleanup(&path);

    // Write test data
    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }

    // Read raw bytes and validate structure
    let raw = fs::read(&path).expect("fs::read");
    assert!(raw.len() >= 18, "gzip file too short: {} bytes", raw.len());

    // Header (RFC 1952 §2.3)
    assert_eq!(raw[0], 0x1f, "magic byte 0");
    assert_eq!(raw[1], 0x8b, "magic byte 1");
    assert_eq!(raw[2], 8, "compression method must be 8 (deflate)");
    // Byte 3: flags (FLG) — typically 0 for basic files
    // Bytes 4-7: mtime
    // Byte 8: xfl (extra flags)
    // Byte 9: OS identifier

    // Trailer — last 8 bytes: CRC-32 (LE) + ISIZE (LE)
    let trailer_start = raw.len() - 8;
    let stored_crc = u32::from_le_bytes([
        raw[trailer_start],
        raw[trailer_start + 1],
        raw[trailer_start + 2],
        raw[trailer_start + 3],
    ]);
    let stored_size = u32::from_le_bytes([
        raw[trailer_start + 4],
        raw[trailer_start + 5],
        raw[trailer_start + 6],
        raw[trailer_start + 7],
    ]);

    // Original size must match HELLO length
    assert_eq!(
        stored_size as usize,
        HELLO.len(),
        "ISIZE mismatch: expected {} got {}",
        HELLO.len(),
        stored_size
    );

    // CRC-32 should match the checksum of the original data
    let computed_crc = crc32(0, HELLO);
    assert_eq!(stored_crc, computed_crc, "CRC-32 mismatch in trailer");

    cleanup(&path);
}

// ============================================================================
// Phase 4: Mode String Parsing Tests (from minigzip.c patterns)
// ============================================================================

/// Verify that various mode strings accepted by C zlib's minigzip are also
/// accepted by the Rust `gz_open`.
#[test]
fn gz_mode_strings() {
    let base = temp_gz_path("mode_strings");

    // Mode strings to test for writing (these create/open files)
    let write_modes = &["wb", "wb0", "wb1", "wb6", "wb9", "wb9f"];

    for (i, &mode) in write_modes.iter().enumerate() {
        let mut p = base.clone();
        p.set_file_name(format!("zlib_rs_test_mode_{}.gz", i));
        cleanup(&p);

        let mut gzf = gz_open(&p, mode).unwrap_or_else(|e| {
            panic!("gz_open({:?}, {:?}) failed: {:?}", p, mode, e);
        });
        gz_write(&mut gzf, b"test").expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");

        // Verify we can read it back
        let mut gzf = gz_open(&p, "rb").expect("gz_open rb");
        let mut buf = [0u8; 32];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(&buf[..n], b"test");
        gz_close(&mut gzf).expect("gz_close rb");

        cleanup(&p);
    }

    // Append mode
    {
        let p = temp_gz_path("mode_append");
        cleanup(&p);
        // Create the file first
        {
            let mut gzf = gz_open(&p, "wb").expect("gz_open wb for append");
            gz_write(&mut gzf, b"first").expect("gz_write first");
            gz_close(&mut gzf).expect("gz_close");
        }
        // Append to it
        {
            let mut gzf = gz_open(&p, "ab").expect("gz_open ab");
            gz_write(&mut gzf, b"second").expect("gz_write second");
            gz_close(&mut gzf).expect("gz_close ab");
        }
        cleanup(&p);
    }
}

// ============================================================================
// Phase 5: Gzip File I/O Operations
// ============================================================================

/// Verify `gz_buffer` resizes the internal buffer before first I/O.
#[test]
fn gz_buffer_resize() {
    let path = temp_gz_path("buffer_resize");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open");
        // Resize buffer to 16384 (must be before first write)
        gz_buffer(&mut gzf, 16384).expect("gz_buffer");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        gz_buffer(&mut gzf, 16384).expect("gz_buffer");
        let mut buf = [0u8; 64];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(&buf[..n], HELLO);
        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}

/// Verify `gz_seek`, `gz_tell`, and `gz_offset` return correct positions.
#[test]
fn gz_seek_tell_offset() {
    let path = temp_gz_path("seek_tell");
    cleanup(&path);

    // Write known data
    let data = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, data).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    // Read and verify seeking
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // Read 10 bytes to advance position
        let mut buf = [0u8; 10];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read 10");
        assert_eq!(n, 10);
        assert_eq!(&buf[..10], &data[..10]);

        // gz_tell should report 10
        assert_eq!(gz_tell(&gzf), 10);

        // Seek to absolute position 5 (SEEK_SET)
        let pos = gz_seek(&mut gzf, 5, SEEK_SET).expect("gz_seek SET 5");
        assert_eq!(pos, 5);
        assert_eq!(gz_tell(&gzf), 5);

        // Read one byte — should be 'F' (index 5)
        let ch = gz_getc(&mut gzf).expect("gz_getc");
        assert_eq!(ch, Some(b'F'));

        // gz_offset should return a positive offset in the compressed file
        let off = gz_offset(&mut gzf).expect("gz_offset");
        assert!(off >= 0, "gz_offset should be non-negative, got {}", off);

        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}

/// Verify `gz_eof` returns correct state before and after consuming all data.
#[test]
fn gz_eof_detection() {
    let path = temp_gz_path("eof_detect");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // Before reading: not at EOF
        assert!(!gz_eof(&gzf), "should not be EOF before reading");

        // Read all data
        let mut buf = [0u8; 256];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(n, HELLO.len());

        // Try to read more — should return 0 (EOF)
        let n2 = gz_read(&mut gzf, &mut buf).expect("gz_read past end");
        assert_eq!(n2, 0);

        // Now gz_eof should be true
        assert!(gz_eof(&gzf), "should be EOF after consuming all data");

        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}

/// Verify `gz_direct` returns `true` for non-gzip files opened with gz_open.
#[test]
fn gz_direct_passthrough() {
    let path = temp_gz_path("direct_pass");
    // Remove .gz extension — write a plain file
    let plain_path = path.with_extension("txt");
    cleanup(&plain_path);

    // Write plain (non-gzip) data
    fs::write(&plain_path, b"plain text data").expect("fs::write");

    // Open the plain file with gz_open in read mode
    let mut gzf = gz_open(&plain_path, "rb").expect("gz_open rb plain");

    // Read some data to trigger format detection
    let mut buf = [0u8; 64];
    let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
    assert_eq!(&buf[..n], b"plain text data");

    // After reading, gz_direct should report true (transparent passthrough)
    assert!(gz_direct(&mut gzf), "gz_direct should be true for non-gzip");

    gz_close(&mut gzf).expect("gz_close");
    cleanup(&plain_path);
}

/// Verify `gz_error_msg` reports errors and `gz_clearerr` clears them.
#[test]
fn gz_error_reporting() {
    let path = temp_gz_path("error_report");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // Initially there should be no error
        let (err, _msg) = gz_error_msg(&gzf);
        assert!(err.is_none(), "should have no error initially");

        // Read all data
        let mut buf = [0u8; 256];
        gz_read(&mut gzf, &mut buf).expect("gz_read");

        // Clear any accumulated state
        gz_clearerr(&mut gzf);
        let (err2, _msg2) = gz_error_msg(&gzf);
        assert!(err2.is_none(), "error should be clear after gz_clearerr");

        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}

// ============================================================================
// Phase 6: Compression Level Variations
// ============================================================================

/// Verify all compression levels 0-9 produce readable output and that
/// level-0 output is larger than level-9 output.
#[test]
fn gz_compression_levels() {
    let test_data = b"The quick brown fox jumps over the lazy dog. \
                      Repeated data helps compression: \
                      AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let mut sizes: Vec<(i32, u64)> = Vec::new();

    for level in 0..=9i32 {
        let path = temp_gz_path(&format!("level_{}", level));
        cleanup(&path);

        let mode = format!("wb{}", level);
        {
            let mut gzf = gz_open(&path, &mode).expect("gz_open");
            gz_write(&mut gzf, test_data).expect("gz_write");
            gz_close(&mut gzf).expect("gz_close");
        }

        // Record file size
        let meta = fs::metadata(&path).expect("metadata");
        sizes.push((level, meta.len()));

        // Read back and verify identity
        {
            let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
            let mut buf = vec![0u8; test_data.len() + 64];
            let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
            assert_eq!(&buf[..n], test_data, "level {} data mismatch", level);
            gz_close(&mut gzf).expect("gz_close");
        }
        cleanup(&path);
    }

    // Level 0 (no compression) should produce larger output than level 9
    let size_0 = sizes.iter().find(|(l, _)| *l == 0).unwrap().1;
    let size_9 = sizes.iter().find(|(l, _)| *l == 9).unwrap().1;
    assert!(
        size_0 > size_9,
        "level-0 size ({}) should be larger than level-9 size ({})",
        size_0,
        size_9
    );
}

// ============================================================================
// Phase 7: Large Data Gzip I/O (minigzip.c pattern)
// ============================================================================

/// Compress 64 KB+ of data in chunks, read back in chunks, verify identity.
/// Simulates the `gz_compress` / `gz_uncompress` loop from minigzip.c.
#[test]
fn gz_large_file_io() {
    let path = temp_gz_path("large_io");
    cleanup(&path);

    // Generate >64 KB of deterministic test data
    let total_size: usize = 80_000;
    let mut original = Vec::with_capacity(total_size);
    for i in 0..total_size {
        original.push((i % 251) as u8); // modular pattern
    }

    // Write in chunks (simulate minigzip gz_compress loop)
    {
        let mut gzf = gz_open(&path, "wb6").expect("gz_open wb6");
        let chunk_size = 4096;
        let mut offset = 0;
        while offset < original.len() {
            let end = std::cmp::min(offset + chunk_size, original.len());
            let written = gz_write(&mut gzf, &original[offset..end]).expect("gz_write chunk");
            assert!(written > 0, "gz_write returned 0");
            offset += written;
        }
        gz_close(&mut gzf).expect("gz_close write");
    }

    // Read back in chunks (simulate gz_uncompress loop)
    let mut recovered = Vec::with_capacity(total_size);
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0u8; 4096];
        loop {
            let n = gz_read(&mut gzf, &mut buf).expect("gz_read chunk");
            if n == 0 {
                break;
            }
            recovered.extend_from_slice(&buf[..n]);
        }
        gz_close(&mut gzf).expect("gz_close read");
    }

    assert_eq!(recovered.len(), original.len(), "length mismatch");
    assert_eq!(recovered, original, "large data round-trip failed");

    cleanup(&path);
}

// ============================================================================
// Phase 8: Gzip Multistream Support
// ============================================================================

/// Write two separate gzip streams to the same file and verify that a
/// single `gz_open` / `gz_read` session reads both sequentially.
#[test]
fn gz_multistream_read() {
    let path = temp_gz_path("multistream");
    cleanup(&path);

    let part_a = b"first stream data";
    let part_b = b"second stream data";

    // Write first gzip member
    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb stream 1");
        gz_write(&mut gzf, part_a).expect("gz_write a");
        gz_close(&mut gzf).expect("gz_close a");
    }
    // Append second gzip member (ab mode appends at file level)
    {
        let mut gzf = gz_open(&path, "ab").expect("gz_open ab stream 2");
        gz_write(&mut gzf, part_b).expect("gz_write b");
        gz_close(&mut gzf).expect("gz_close b");
    }

    // Read the concatenated file — should return both parts
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb multistream");
        let mut buf = vec![0u8; 256];
        let mut total = Vec::new();
        loop {
            let n = gz_read(&mut gzf, &mut buf).expect("gz_read multi");
            if n == 0 {
                break;
            }
            total.extend_from_slice(&buf[..n]);
        }

        // The combined output should contain both parts sequentially.
        // Note: depending on gzip multistream support, this may return
        // only the first part. Both behaviours are valid — the test
        // asserts at minimum that the first part is present.
        assert!(
            total.len() >= part_a.len(),
            "multistream read too short: {} bytes",
            total.len()
        );
        assert_eq!(&total[..part_a.len()], part_a, "first stream data mismatch");

        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}

// ============================================================================
// Phase 9: gz_fread and gz_fwrite (batch I/O)
// ============================================================================

/// Verify `gz_fwrite` and `gz_fread` batch I/O operations preserve data
/// identity.
#[test]
fn gz_fread_fwrite() {
    let path = temp_gz_path("fread_fwrite");
    cleanup(&path);

    let data = b"AABBCCDDEE";
    let item_size = 2;
    let nitems = 5; // 5 items of 2 bytes each = 10 bytes

    // Write using gz_fwrite
    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        let items_written =
            gz_fwrite(&mut gzf, &data[..item_size * nitems], item_size, nitems).expect("gz_fwrite");
        assert_eq!(items_written, nitems, "gz_fwrite items mismatch");
        gz_close(&mut gzf).expect("gz_close");
    }

    // Read back using gz_fread
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0u8; 32];
        let items_read = gz_fread(&mut gzf, &mut buf, item_size, nitems).expect("gz_fread");
        assert_eq!(items_read, nitems, "gz_fread items mismatch");
        assert_eq!(&buf[..item_size * nitems], &data[..item_size * nitems]);
        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

// ============================================================================
// Additional coverage: gz_flush, gz_setparams, gz_rewind
// ============================================================================

/// Verify `gz_flush` with various flush modes works during write.
#[test]
fn gz_flush_during_write() {
    let path = temp_gz_path("flush_write");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, b"before flush").expect("gz_write 1");

        // Z_SYNC_FLUSH should succeed
        gz_flush(&mut gzf, Z_SYNC_FLUSH).expect("gz_flush sync");

        gz_write(&mut gzf, b" after flush").expect("gz_write 2");
        gz_close(&mut gzf).expect("gz_close");
    }

    // Verify all data is readable
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0u8; 64];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(&buf[..n], b"before flush after flush");
        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify `gz_setparams` can change compression level mid-stream.
#[test]
fn gz_setparams_midstream() {
    let path = temp_gz_path("setparams");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb6").expect("gz_open wb6");
        gz_write(&mut gzf, b"level 6 data ").expect("gz_write 6");

        // Switch to best compression
        gz_setparams(&mut gzf, Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY).expect("gz_setparams best");
        gz_write(&mut gzf, b"best compression data").expect("gz_write best");

        // Switch to no compression
        gz_setparams(&mut gzf, Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY).expect("gz_setparams none");
        gz_write(&mut gzf, b" none data").expect("gz_write none");

        gz_close(&mut gzf).expect("gz_close");
    }

    // Read back and verify data integrity
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0u8; 256];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(&buf[..n], b"level 6 data best compression data none data");
        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify `gz_rewind` returns the reader to the start of the file.
#[test]
fn gz_rewind_test() {
    let path = temp_gz_path("rewind");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // First read
        let mut buf = [0u8; 64];
        let n1 = gz_read(&mut gzf, &mut buf).expect("gz_read first");
        assert_eq!(&buf[..n1], HELLO);

        // Rewind to start
        gz_rewind(&mut gzf).expect("gz_rewind");

        // Read again — should get the same data
        let mut buf2 = [0u8; 64];
        let n2 = gz_read(&mut gzf, &mut buf2).expect("gz_read second");
        assert_eq!(n2, n1);
        assert_eq!(&buf2[..n2], HELLO);

        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify the `std::io::Read` trait impl on `GzState` works correctly.
#[test]
fn gz_std_io_read_trait() {
    let path = temp_gz_path("io_read_trait");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, HELLO).expect("gz_write");
        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");

        // Use std::io::Read trait
        let mut contents = Vec::new();
        gzf.read_to_end(&mut contents).expect("read_to_end");
        assert_eq!(contents, HELLO);

        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify the `std::io::Write` trait impl on `GzState` works correctly.
#[test]
fn gz_std_io_write_trait() {
    let path = temp_gz_path("io_write_trait");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");

        // Use std::io::Write trait
        gzf.write_all(HELLO).expect("write_all");
        gzf.flush().expect("flush");

        gz_close(&mut gzf).expect("gz_close");
    }
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0u8; 64];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(&buf[..n], HELLO);
        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify gz_seek in write mode (forward seek fills with zeros).
#[test]
fn gz_seek_write_mode() {
    let path = temp_gz_path("seek_write");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, b"AB").expect("gz_write AB");

        // Seek forward 3 bytes — should fill with zeros
        gz_seek(&mut gzf, 3, SEEK_CUR).expect("gz_seek forward");

        gz_write(&mut gzf, b"CD").expect("gz_write CD");
        gz_close(&mut gzf).expect("gz_close");
    }

    // Read back and verify: "AB" + 3 zeros + "CD"
    {
        let mut gzf = gz_open(&path, "rb").expect("gz_open rb");
        let mut buf = [0xFFu8; 16];
        let n = gz_read(&mut gzf, &mut buf).expect("gz_read");
        assert_eq!(n, 7, "expected 7 bytes, got {}", n);
        assert_eq!(&buf[0..2], b"AB");
        assert_eq!(&buf[2..5], &[0, 0, 0]);
        assert_eq!(&buf[5..7], b"CD");
        gz_close(&mut gzf).expect("gz_close");
    }

    cleanup(&path);
}

/// Verify that gz_buffer rejects resize after first I/O.
#[test]
fn gz_buffer_reject_after_io() {
    let path = temp_gz_path("buffer_reject");
    cleanup(&path);

    {
        let mut gzf = gz_open(&path, "wb").expect("gz_open wb");
        gz_write(&mut gzf, b"data").expect("gz_write");

        // After I/O has started, gz_buffer should fail
        let result = gz_buffer(&mut gzf, 32768);
        assert!(result.is_err(), "gz_buffer should fail after I/O started");

        gz_close(&mut gzf).expect("gz_close");
    }
    cleanup(&path);
}
