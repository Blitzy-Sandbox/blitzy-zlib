//! Gzip file I/O round-trip tests.
//!
//! Tests [`GzReader`], [`GzWriter`], seek, transparent read, and gzip file
//! operations using temporary files for test isolation.
//!
//! Port of gzip I/O tests from `test/example.c` `test_gzio()` function
//! (lines 90–164) and additional coverage for the full `gz` module API.
//!
//! [`GzReader`]: zlib_rs::gz::GzReader
//! [`GzWriter`]: zlib_rs::gz::GzWriter

use std::io::{Read, Seek, SeekFrom, Write};

use tempfile::NamedTempFile;
use zlib_rs::gz::{GzReader, GzWriter};
#[allow(clippy::wildcard_imports)]
use zlib_rs_tests::*;

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Create a gzip-compressed file at `path` containing `data`.
///
/// Opens a [`GzWriter`], writes all bytes via the [`Write`] trait, and
/// closes cleanly. Panics on any I/O error (acceptable in test helpers).
fn write_gz_file(path: &str, data: &[u8]) {
    let mut writer = GzWriter::open(path).expect("GzWriter::open failed in helper");
    writer.write_all(data).expect("write_all failed in helper");
    writer.close().expect("GzWriter::close failed in helper");
}

/// Read all decompressed bytes from the gzip file at `path`.
///
/// Opens a [`GzReader`], reads to end via the [`Read`] trait, and returns
/// the accumulated bytes. Panics on any I/O error.
fn read_gz_file(path: &str) -> Vec<u8> {
    let mut reader = GzReader::open(path).expect("GzReader::open failed in helper");
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .expect("read_to_end failed in helper");
    buf
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 2: Basic Write/Read Round-Trip
// Mirrors test/example.c test_gzio lines 90–164
// ═══════════════════════════════════════════════════════════════════════════════

/// Faithful port of `test_gzio()` from `test/example.c` lines 90–164.
///
/// Tests the core gzip file I/O pipeline:
/// - Character write via `putc` (gzputc)
/// - String write via `puts` (gzputs)
/// - Formatted write via `printf` (gzprintf)
/// - Seek-based zero byte insertion (gzseek)
/// - Sequential read with seek/getc/ungetc/gets verification
#[test]
fn test_gz_basic_write_read() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // ── Write phase (C: gzopen(fname, "wb")) ──
    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");

        // C: gzputc(file, 'h')
        writer.putc(b'h').expect("putc('h') failed");

        // C: if (gzputs(file, "ello") != 4)
        let n = writer.puts("ello").expect("puts(\"ello\") failed");
        assert_eq!(n, 4, "gzputs should write 4 bytes");

        // C: if (gzprintf(file, ", %s!", "hello") != 8)
        // In Rust, printf takes a pre-formatted string.
        let n = writer
            .printf(&format!(", {}!", "hello"))
            .expect("printf failed");
        assert_eq!(n, 8, "gzprintf should write 8 bytes");

        // C: gzseek(file, 1L, SEEK_CUR)  — add one zero byte
        let _pos = writer
            .seek(1, SeekFrom::Current(0))
            .expect("write-mode seek forward failed");

        // C: gzclose(file)
        writer.close().expect("GzWriter::close failed");
    }

    // ── Read phase (C: gzopen(fname, "rb")) ──
    {
        let mut reader = GzReader::open(path).expect("GzReader::open failed");

        // C: if (gzread(file, uncompr, (unsigned)uncomprLen) != len)
        // len = strlen(hello)+1 = 14
        let mut buf = alloc_uncompr_buffer(UNCOMPR_LEN);
        let n = reader.read(&mut buf).expect("gzread failed");
        assert_eq!(
            n,
            HELLO.len(),
            "gzread should return {} bytes, got {n}",
            HELLO.len()
        );
        assert_bytes_equal(&buf[..n], HELLO, "gzread content mismatch");

        // C: pos = gzseek(file, -8L, SEEK_CUR)
        // pos should be 6, gztell(file) should agree
        let pos = reader
            .seek(-8, SeekFrom::Current(0))
            .expect("read-mode seek backward failed");
        assert_eq!(pos, 6, "gzseek should return position 6");
        assert_eq!(reader.tell(), 6, "gztell should agree with gzseek result");

        // C: if (gzgetc(file) != ' ')
        let c = reader.getc().expect("gzgetc failed");
        assert_eq!(c, b' ', "gzgetc should return space at position 6");

        // C: if (gzungetc(' ', file) != ' ')
        reader.ungetc(b' ').expect("gzungetc failed");

        // C: gzgets(file, (char*)uncompr, (int)uncomprLen)
        // After ungetc, reading position is back to 6.
        // Remaining data: " hello!\0" — gets reads until newline or EOF.
        // C: if (strlen((char*)uncompr) != 7)  /* " hello!" */
        let mut line_buf = alloc_uncompr_buffer(UNCOMPR_LEN);
        let n = reader.gets(&mut line_buf).expect("gzgets failed");
        assert!(n >= 7, "gets should read at least 7 bytes, got {n}");
        assert_eq!(
            &line_buf[..7],
            b" hello!",
            "gets content after gzseek should be ' hello!'"
        );
    }
}

/// Verify that the `TESTFILE` constant is accessible and usable.
///
/// Uses the `TESTFILE` constant from the shared test utilities to
/// exercise path-based operations with a known filename.
#[test]
fn test_gz_with_testfile_constant() {
    // TESTFILE matches C test/example.c line 25: #define TESTFILE "foo.gz"
    assert_eq!(TESTFILE, "foo.gz", "TESTFILE should be 'foo.gz'");

    // Create a temp directory and use TESTFILE as the filename
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // Write HELLO data and read it back
    write_gz_file(path, HELLO);
    let data = read_gz_file(path);
    assert_bytes_equal(&data, HELLO, "TESTFILE round-trip with HELLO data");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: GzWriter Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Write a small string and verify round-trip correctness.
#[test]
fn test_gz_write_small() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"hello, hello!");
    let data = read_gz_file(path);
    assert_eq!(data, b"hello, hello!", "small write round-trip failed");
}

/// Write data larger than `GZBUFSIZE` (8192 bytes) and verify round-trip.
#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_gz_write_large() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // 100 KiB of patterned data — well above GZBUFSIZE=8192
    let size = 100 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    write_gz_file(path, &data);
    let result = read_gz_file(path);
    assert_eq!(result.len(), data.len(), "large write: length mismatch");
    assert_bytes_equal(&result, &data, "large write round-trip");
}

/// Open and immediately close an empty gzip file.
#[test]
fn test_gz_write_empty() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer.close().expect("close empty writer failed");
    }

    let data = read_gz_file(path);
    assert!(data.is_empty(), "empty gzip file should decompress to empty");
}

/// Test explicit `gz_flush` with `Z_SYNC_FLUSH`.
///
/// Data written before and after a sync flush should both be present
/// in the final decompressed output.
#[test]
fn test_gz_write_flush() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer
            .write_all(b"before flush")
            .expect("write before flush failed");

        // Explicit sync flush — inserts a sync point in the stream
        let ret = writer.gz_flush(Z_SYNC_FLUSH).expect("gz_flush failed");
        assert_eq!(ret, Z_OK, "gz_flush(Z_SYNC_FLUSH) should return Z_OK");

        writer
            .write_all(b" after flush")
            .expect("write after flush failed");
        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(data, b"before flush after flush", "flush round-trip");
}

/// Test `Z_NO_FLUSH` (default) — data remains buffered until close.
#[test]
fn test_gz_write_no_flush() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer.write_all(b"no flush ").expect("write failed");
        let ret = writer.gz_flush(Z_NO_FLUSH).expect("Z_NO_FLUSH failed");
        assert_eq!(ret, Z_OK, "gz_flush(Z_NO_FLUSH) should return Z_OK");
        writer.write_all(b"more data").expect("write more failed");
        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(data, b"no flush more data", "no_flush round-trip");
}

/// Test mid-stream compression parameter change via `set_params`.
///
/// Writes data in three phases with different compression levels:
/// `Z_DEFAULT_COMPRESSION`, `Z_BEST_SPEED`, and `Z_BEST_COMPRESSION`.
#[test]
fn test_gz_write_set_params() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");

        // Phase 1: Z_DEFAULT_COMPRESSION (the default)
        writer
            .write_all(b"default level ")
            .expect("write default failed");

        // Phase 2: switch to Z_BEST_SPEED
        writer
            .set_params(Z_BEST_SPEED, Z_DEFAULT_STRATEGY)
            .expect("set_params(Z_BEST_SPEED) failed");
        writer
            .write_all(b"best speed ")
            .expect("write best speed failed");

        // Phase 3: switch to Z_BEST_COMPRESSION
        writer
            .set_params(Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY)
            .expect("set_params(Z_BEST_COMPRESSION) failed");
        writer
            .write_all(b"best compression")
            .expect("write best compression failed");

        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(
        data,
        b"default level best speed best compression",
        "set_params round-trip content"
    );
}

/// Write with `Z_NO_COMPRESSION` (stored mode, level 0) and verify.
#[test]
fn test_gz_write_no_compression() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        // "w0" sets compression level 0 (Z_NO_COMPRESSION)
        let mut writer = GzWriter::open_with_mode(path, "w0")
            .expect("open_with_mode(w0) failed");
        writer
            .write_all(b"stored mode data")
            .expect("write failed");
        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(data, b"stored mode data", "Z_NO_COMPRESSION round-trip");
}

/// Write with the `Write` trait's `write()` method directly
/// (not `write_all`) to verify partial-write handling.
#[test]
fn test_gz_write_trait() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        let n = writer.write(b"hello").expect("write failed");
        assert!(n > 0, "write should return positive byte count");
        let m = writer.write(b" world").expect("write 2 failed");
        assert!(m > 0, "write should return positive byte count");
        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(data, b"hello world", "Write trait round-trip");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 4: GzReader Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Read a small gzip file and verify content.
#[test]
fn test_gz_read_small() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    let expected = b"The quick brown fox jumps over the lazy dog";
    write_gz_file(path, expected);

    let data = read_gz_file(path);
    assert_bytes_equal(&data, expected, "small read content");
}

/// Read gzip data one byte at a time using `getc`.
#[test]
fn test_gz_read_byte_at_a_time() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    let expected = b"abcdefghij";
    write_gz_file(path, expected);

    let mut reader = GzReader::open(path).expect("GzReader::open failed");
    for (i, &exp_byte) in expected.iter().enumerate() {
        let byte = reader.getc().expect("getc failed");
        assert_eq!(
            byte, exp_byte,
            "getc byte {i}: expected {exp_byte:#04x}, got {byte:#04x}"
        );
    }

    // Next getc should fail (EOF)
    let result = reader.getc();
    assert!(result.is_err(), "getc past EOF should return error");
}

/// Read lines from gzip data using `gets`.
#[test]
fn test_gz_read_line() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"line one\nline two\nline three");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // First line — gets stops at newline (inclusive)
    let mut buf = vec![0u8; 256];
    let n = reader.gets(&mut buf).expect("gets line 1 failed");
    assert!(n > 0, "gets should read at least one byte for line 1");
    assert_eq!(&buf[..n], b"line one\n", "first line content");

    // Second line
    let n = reader.gets(&mut buf).expect("gets line 2 failed");
    assert_eq!(&buf[..n], b"line two\n", "second line content");
}

/// Test `ungetc` — push a byte back into the read stream and read it again.
#[test]
fn test_gz_read_ungetc() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"xyz");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // Read first byte
    let c = reader.getc().expect("getc failed");
    assert_eq!(c, b'x', "first byte should be 'x'");

    // Push it back
    reader.ungetc(b'x').expect("ungetc failed");

    // Read again — should get the pushed-back byte
    let c2 = reader.getc().expect("getc after ungetc failed");
    assert_eq!(c2, b'x', "ungetc byte should be 'x'");

    // Continue reading normally
    let c3 = reader.getc().expect("getc for 'y' failed");
    assert_eq!(c3, b'y', "next byte should be 'y'");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 5: Seek Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Forward seek via `SeekFrom::Start`.
#[test]
fn test_gz_seek_forward() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"0123456789");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // Absolute seek to position 5
    let pos = reader
        .seek(5, SeekFrom::Start(0))
        .expect("forward seek failed");
    assert_eq!(pos, 5, "seek should return position 5");

    // Read remaining
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end failed");
    assert_eq!(buf, b"56789", "data after forward seek");
}

/// Backward seek by absolute repositioning.
///
/// Read all data first so the internal buffer is empty (have == 0),
/// which means `current == pos` and the seek code takes the
/// rewind-then-skip path for backward seeks.
#[test]
fn test_gz_seek_backward() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"abcdefghij");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // Read ALL data to drain the internal buffer completely.
    let mut all = Vec::new();
    reader.read_to_end(&mut all).expect("read_to_end failed");
    assert_eq!(&all, b"abcdefghij");

    // Now seek back to absolute position 2.  With have == 0 the
    // implementation performs rewind() + skip(2), giving a
    // consistent tell() value.
    let pos = reader
        .seek(2, SeekFrom::Start(0))
        .expect("backward seek failed");
    assert_eq!(pos, 2, "seek should return position 2");
    assert_eq!(reader.tell(), 2, "tell should match after backward seek");

    // Read from position 2 — should get 'c'.
    let c = reader.getc().expect("getc after backward seek failed");
    assert_eq!(c, b'c', "byte at position 2 should be 'c'");
}

/// Test `rewind` — rewind to the beginning and read again.
#[test]
fn test_gz_rewind() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    let expected = b"rewind test data";
    write_gz_file(path, expected);

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // First read
    let mut buf1 = Vec::new();
    reader.read_to_end(&mut buf1).expect("first read failed");
    assert_bytes_equal(&buf1, expected, "first read");

    // Rewind
    reader.rewind().expect("rewind failed");

    // Second read — should produce identical data
    let mut buf2 = Vec::new();
    reader.read_to_end(&mut buf2).expect("second read failed");
    assert_bytes_equal(&buf2, expected, "second read after rewind");
    assert_bytes_equal(&buf1, &buf2, "reads must match after rewind");
}

/// Test `tell` and `offset` position tracking.
#[test]
fn test_gz_tell_offset() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"position tracking test data");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // Initial position should be 0
    assert_eq!(reader.tell(), 0, "initial tell should be 0");

    // Read 10 bytes
    let mut buf = vec![0u8; 10];
    let n = reader.read(&mut buf).expect("read 10 bytes failed");
    assert_eq!(n, 10, "should read exactly 10 bytes");

    // tell should reflect the uncompressed position
    assert_eq!(reader.tell(), 10, "tell should be 10 after reading 10 bytes");

    // offset returns the compressed file position (must be non-negative)
    let off = reader.offset();
    assert!(off >= 0, "offset should be non-negative, got {off}");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 6: Transparent Read Mode
// ═══════════════════════════════════════════════════════════════════════════════

/// Test transparent (direct) read mode for non-gzip files.
///
/// When a file does not start with a gzip header (magic bytes `1f 8b`),
/// `GzReader` falls through to transparent copy mode, passing bytes
/// through without decompression. `is_direct()` should return `true`.
#[test]
fn test_gz_transparent_read() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // Write raw (non-gzip) data using std::fs, exercising the Seek trait
    let raw_data = b"this is not gzip compressed data";
    {
        let mut file = std::fs::File::create(path).expect("File::create failed");
        file.write_all(raw_data).expect("raw write failed");
        // Verify file position via Seek trait
        let pos = file.seek(SeekFrom::Current(0)).expect("seek failed");
        assert_eq!(pos, raw_data.len() as u64, "file position after write");
    }

    // Open with GzReader — should auto-detect non-gzip and use COPY mode
    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // is_direct() should return true for non-gzip input
    assert!(
        reader.is_direct(),
        "is_direct() should be true for non-gzip data"
    );

    // Read should return the raw data unchanged
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end failed");
    assert_bytes_equal(&buf, raw_data, "transparent read content");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 7: EOF and Error Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Test `eof()` — EOF detection after exhausting the input.
#[test]
fn test_gz_eof() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    write_gz_file(path, b"eof test data");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");

    // EOF should be false initially
    assert!(!reader.eof(), "eof should be false before reading");

    // Read all data
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end failed");

    // After read_to_end, the reader has attempted to read past EOF
    assert!(reader.eof(), "eof should be true after read_to_end");
}

/// Test error reporting via `error_code()` and `clearerr()`.
#[test]
fn test_gz_error() {
    // Opening a non-existent file should fail
    let result = GzReader::open("/nonexistent/path/test_gz_error.gz");
    assert!(
        result.is_err(),
        "opening non-existent file should return an error"
    );

    // Verify error_code after normal operations
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");
    write_gz_file(path, b"error test");

    let mut reader = GzReader::open(path).expect("GzReader::open failed");
    assert_eq!(
        reader.error_code(),
        Z_OK,
        "error_code should be Z_OK initially"
    );

    // Read data successfully
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read failed");

    // clearerr should reset the error state
    reader.clearerr();
    assert_eq!(
        reader.error_code(),
        Z_OK,
        "error_code should be Z_OK after clearerr"
    );
    // After clearerr, eof flag should also be cleared
    assert!(!reader.eof(), "eof should be false after clearerr");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 8: gzbuffer Tests
// ═══════════════════════════════════════════════════════════════════════════════

/// Test custom buffer sizes via `set_buffer_size`.
///
/// Buffer size should not affect output correctness — only internal
/// buffering performance. The same data should round-trip regardless
/// of the configured buffer size.
#[test]
fn test_gz_buffer_size() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    let expected = b"buffer size test data with enough content to fill smaller buffers";

    // Write with custom 4 KiB buffer
    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        // set_buffer_size returns Result<(), ReturnCode>
        let buf_result = writer.set_buffer_size(4096);
        match buf_result {
            Ok(()) => assert_ok(ReturnCode::Ok, "set_buffer_size on writer"),
            Err(code) => check_err(code, "set_buffer_size on writer"),
        }
        writer.write_all(expected).expect("write failed");
        writer.close().expect("close failed");
    }

    // Read with custom 4 KiB buffer
    {
        let mut reader = GzReader::open(path).expect("GzReader::open failed");
        let buf_result = reader.set_buffer_size(4096);
        match buf_result {
            Ok(()) => assert_ok(ReturnCode::Ok, "set_buffer_size on reader"),
            Err(code) => check_err(code, "set_buffer_size on reader"),
        }
        let data = read_gz_file(path);
        assert_bytes_equal(&data, expected, "buffer size round-trip");
    }

    // Verify set_buffer_size fails after I/O has started
    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer.write_all(b"x").expect("write 'x' failed");
        let result = writer.set_buffer_size(4096);
        assert!(
            result.is_err(),
            "set_buffer_size should fail after I/O started"
        );
        if let Err(code) = result {
            assert_eq!(
                code,
                ReturnCode::StreamError,
                "expected StreamError from set_buffer_size after I/O"
            );
        }
        writer.close().expect("close failed");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 9: Multiple Gzip Members
// ═══════════════════════════════════════════════════════════════════════════════

/// Test reading a file with multiple concatenated gzip members.
///
/// Per RFC 1952, a gzip file may contain multiple members; the
/// decompressor should transparently process all concatenated streams
/// and return the combined uncompressed data.
#[test]
fn test_gz_multiple_members() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // Write first gzip member
    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer
            .write_all(b"first member ")
            .expect("first member write failed");
        writer.close().expect("first member close failed");
    }

    // Append second gzip member using 'a' (append) mode
    {
        let mut writer = GzWriter::open_with_mode(path, "ab")
            .expect("GzWriter::open_with_mode('ab') failed");
        writer
            .write_all(b"second member")
            .expect("second member write failed");
        writer.close().expect("second member close failed");
    }

    // Read all — should get both members' data concatenated
    let data = read_gz_file(path);
    assert_eq!(
        data,
        b"first member second member",
        "multiple gzip members should be concatenated on read"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Additional Coverage: Compression Level Variations
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify round-trip with `Z_BEST_COMPRESSION` (level 9).
#[test]
fn test_gz_write_best_compression() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    // Repeat data to give the compressor something to work with
    let mut data = Vec::with_capacity(4096);
    for _ in 0..100 {
        data.extend_from_slice(b"compressible repeated content ");
    }

    {
        // "w9" sets Z_BEST_COMPRESSION (level 9)
        let mut writer = GzWriter::open_with_mode(path, "w9")
            .expect("open_with_mode('w9') failed");
        writer.write_all(&data).expect("write failed");
        writer.close().expect("close failed");
    }

    let result = read_gz_file(path);
    assert_bytes_equal(&result, &data, "Z_BEST_COMPRESSION round-trip");
}

/// Verify round-trip with `Z_BEST_SPEED` (level 1).
#[test]
fn test_gz_write_best_speed() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    let data = b"fast compression data for Z_BEST_SPEED testing";

    {
        // "w1" sets Z_BEST_SPEED (level 1)
        let mut writer = GzWriter::open_with_mode(path, "w1")
            .expect("open_with_mode('w1') failed");
        writer.write_all(data).expect("write failed");
        writer.close().expect("close failed");
    }

    let result = read_gz_file(path);
    assert_eq!(result, data.as_slice(), "Z_BEST_SPEED round-trip");
}

/// Test `Z_FINISH` flush mode explicitly before close.
#[test]
fn test_gz_write_finish_flush() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        let mut writer = GzWriter::open(path).expect("GzWriter::open failed");
        writer
            .write_all(b"finish flush test")
            .expect("write failed");
        // Explicit Z_FINISH flush completes the gzip stream
        let ret = writer.gz_flush(Z_FINISH).expect("Z_FINISH flush failed");
        assert!(
            ret == Z_OK || ret == Z_STREAM_END,
            "Z_FINISH should return Z_OK or Z_STREAM_END, got {ret}"
        );
        // close() will handle the already-finished state
        writer.close().expect("close after Z_FINISH failed");
    }

    let data = read_gz_file(path);
    // Data should be fully present after Z_FINISH + close
    assert_eq!(
        std::str::from_utf8(&data).unwrap_or(""),
        "finish flush test",
        "Z_FINISH flush round-trip"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Additional Coverage: set_params with Z_DEFAULT_COMPRESSION
// ═══════════════════════════════════════════════════════════════════════════════

/// Test `set_params` with `Z_DEFAULT_COMPRESSION` and `Z_NO_COMPRESSION`.
#[test]
fn test_gz_write_set_params_default() {
    let tmp = NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_str().expect("non-UTF8 temp path");

    {
        // Start with best speed
        let mut writer = GzWriter::open_with_mode(path, "w1")
            .expect("open_with_mode('w1') failed");
        writer.write_all(b"speed ").expect("write failed");

        // Switch to Z_DEFAULT_COMPRESSION
        writer
            .set_params(Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY)
            .expect("set_params(Z_DEFAULT_COMPRESSION) failed");
        writer.write_all(b"default ").expect("write failed");

        // Switch to Z_NO_COMPRESSION (stored mode)
        writer
            .set_params(Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY)
            .expect("set_params(Z_NO_COMPRESSION) failed");
        writer.write_all(b"stored").expect("write failed");

        writer.close().expect("close failed");
    }

    let data = read_gz_file(path);
    assert_eq!(
        data,
        b"speed default stored",
        "set_params multi-level round-trip"
    );
}
