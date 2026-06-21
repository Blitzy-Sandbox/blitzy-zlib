// Whole-file feature gate (AAP §0.5.2): the gzip FILE-I/O layer (`gz::*`,
// `GzState`) is compiled only under the `gz-io` feature — the Rust analogue of
// the C `#ifndef NO_GZCOMPRESS` / `NO_GZIP` build. `gz-io` implies `std + gzip`
// and is ON by default, so `cargo test --test gzip_compat` runs every test here.
// Under `--no-default-features` (gz-io absent) this attribute excludes the
// ENTIRE file, leaving an empty test binary with zero tests and zero errors.
//
// NOTE: integration tests are separate `std` binaries built against the crate's
// default features (doc/project-guide §2.4), so in the normal run `gz-io` is
// always present and all of these tests are active.
#![cfg(feature = "gz-io")]

//! # `tests/gzip_compat.rs` — gzip file-format compatibility
//!
//! A faithful Rust port of zlib's `test/minigzip.c`, validating the safe-Rust
//! gzip FILE-I/O layer (`zlib_rs::gz::*`) end-to-end: the `gz_compress` /
//! `gz_uncompress` / `file_compress` / `file_uncompress` patterns, plus the
//! `gzopen` / `gzread` / `gzwrite` / `gzclose` family and the character, line,
//! seek, and status helpers. The focus is **RFC 1952 (gzip)** wrapper
//! conformance and round-trip fidelity (AAP §0.4.1, §0.7.1).
//!
//! ## What this exercises (and what it does not)
//!
//! These tests drive **our** gzip layer only — they never link `flate2` or any
//! C zlib (which `tests/interop.rs` owns). A gzip stream written by
//! [`gzwrite`](zlib_rs::gz::gzwrite) and friends is read back by
//! [`gzread`](zlib_rs::gz::gzread); the on-disk bytes are additionally checked
//! for the RFC 1952 framing (`1f 8b 08`). Adler-32/CRC-32 known-answer vectors
//! live in `tests/checksum.rs`; raw-DEFLATE interop lives in `tests/interop.rs`.
//!
//! ## API adaptation note
//!
//! The materialized gz surface lives under `zlib_rs::gz::*` (it is **not**
//! re-exported at the crate root), the owned handle is a `Box<GzState>` (there
//! is no `GzFile` type alias), and the top-level [`gzclose`](zlib_rs::gz::gzclose)
//! **consumes** that box. The C-style free functions take `&mut GzState` /
//! `&GzState`; a `Box<GzState>` auto-derefs to those at the call site. These
//! tests adapt to that materialized reality exactly while preserving
//! minigzip.c's observable behaviour.
//!
//! ## Safety & hygiene
//!
//! * No `unsafe`, no third-party crates — only `zlib_rs` and `std`.
//! * Every temporary file lives under [`std::env::temp_dir`] with a
//!   process-unique, clone-unique name and is removed by a [`Drop`] guard
//!   ([`TempPath`]); the repository tree and any user files are never touched
//!   (in particular, unlike minigzip.c, **no source file is ever unlinked**).

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use zlib_rs::gz::state::GzState;
use zlib_rs::gz::{
    gzbuffer, gzclose, gzdirect, gzeof, gzerror, gzflush, gzgetc, gzgets, gzopen, gzprintf, gzputc,
    gzputs, gzread, gzseek, gztell, gzungetc, gzwrite,
};
use zlib_rs::{SEEK_CUR, SEEK_SET, Z_OK, Z_SYNC_FLUSH};

// ===========================================================================
// Constants (mirroring minigzip.c)
// ===========================================================================

/// minigzip.c `BUFLEN` — the staging buffer size for the compress/uncompress
/// loops. 16 KiB so multi-buffer payloads cross several boundaries, exactly as
/// in the C original.
const BUFLEN: usize = 16384;

/// minigzip.c `GZ_SUFFIX` — the extension appended to a compressed file. (The
/// C `MAX_NAME_LEN = 1024` filename-bound is a CLI nicety with no analogue in
/// these temp-file-only tests and is intentionally omitted.)
const GZ_SUFFIX: &str = ".gz";

// ===========================================================================
// Temp-file management — a tiny RAII guard (replaces minigzip's fixed names +
// `unlink`). Names are process-unique AND clone-unique so parallel `cargo test`
// threads and parallel repository clones never collide.
// ===========================================================================

/// An owned temporary path that deletes its file on [`Drop`] (best-effort).
struct TempPath(PathBuf);

impl TempPath {
    /// A unique temp path ending in `suffix` (e.g. `".gz"` or `""`).
    ///
    /// Uniqueness comes from a monotonic per-process counter, the process id,
    /// and the `CLONE_INDEX` environment variable (set per parallel clone), so
    /// no two invocations — across threads or clones — ever produce the same
    /// path.
    fn with_suffix(tag: &str, suffix: &str) -> TempPath {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let clone = std::env::var("CLONE_INDEX").unwrap_or_else(|_| "x".to_string());
        let pid = std::process::id();
        let name = format!("blitzy_gzip_compat_{tag}_{clone}_{pid}_{n}{suffix}");
        TempPath(std::env::temp_dir().join(name))
    }

    /// The common case: a unique temp path carrying the gzip `.gz` suffix.
    fn new(tag: &str) -> TempPath {
        Self::with_suffix(tag, GZ_SUFFIX)
    }

    /// Borrow the underlying filesystem path.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best-effort cleanup confined to the system temp directory; never
        // panic from a destructor, and never touch anything outside temp.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Append [`GZ_SUFFIX`] to `path` (the minigzip `file -> file.gz` mapping).
fn append_gz(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(GZ_SUFFIX);
    PathBuf::from(s)
}

/// Strip a trailing [`GZ_SUFFIX`] from `path` (the inverse `file.gz -> file`).
fn strip_gz(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_suffix(GZ_SUFFIX) {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}

/// Build `len` bytes of deterministic, *compressible* mixed text.
///
/// A pure run of identical bytes would not exercise real LZ77 matching, and a
/// pure random stream would not compress; this mixes natural-language fragments
/// with a varying byte so the data both compresses meaningfully and crosses the
/// [`BUFLEN`] boundary — a faithful stand-in for minigzip's real-file input.
fn make_payload(len: usize) -> Vec<u8> {
    const WORDS: [&[u8]; 4] = [
        b"the quick brown fox ",
        b"jumps over the lazy dog. ",
        b"pack my box with five dozen liquor jugs. ",
        b"0123456789 ",
    ];
    let mut out = Vec::with_capacity(len + 64);
    let mut i = 0usize;
    while out.len() < len {
        out.extend_from_slice(WORDS[i % WORDS.len()]);
        out.push((i % 251) as u8);
        i += 1;
    }
    out.truncate(len);
    out
}

// ===========================================================================
// minigzip.c helper ports
// ===========================================================================
//
// Faithful ports of minigzip's `gz_compress` / `gz_uncompress` / `file_compress`
// / `file_uncompress`. One deliberate, ownership-driven adaptation: in C these
// helpers `gzclose()` the handle themselves, so the Rust ports take the OWNED
// `Box<GzState>` *by value* and call the consuming [`gzclose`] internally,
// returning its `Z_*` status — mirroring the C control flow precisely while
// honouring Rust move semantics (the handle cannot be used after the close).

/// Port of minigzip `gz_compress`: stream every byte of `input` into the gzip
/// writer `out` in [`BUFLEN`] chunks, then close it.
///
/// Each [`gzwrite`] must report the full chunk length (the C `!= len` error
/// check); reading stops at end of input. Returns the [`gzclose`] status.
fn gz_compress(input: &mut impl Read, mut out: Box<GzState>) -> i32 {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = input.read(&mut buf).expect("read from input source");
        if len == 0 {
            break; // C `if (len == 0) break;`
        }
        // C `if (gzwrite(out, buf, len) != len) error(...)`.
        let written = gzwrite(&mut out, &buf[..len]);
        assert_eq!(
            written as usize, len,
            "gzwrite must accept the entire {len}-byte chunk"
        );
    }
    // C `if (gzclose(out) != Z_OK) error("failed gzclose");` — the caller asserts.
    gzclose(Some(out))
}

/// Port of minigzip `gz_uncompress`: stream every byte from the gzip reader
/// `inp` into `output` in [`BUFLEN`] chunks, then close it.
///
/// A negative [`gzread`] is a hard error (the C `if (len < 0) error(...)`); a
/// zero return is end of data. Returns the [`gzclose`] status.
fn gz_uncompress(mut inp: Box<GzState>, output: &mut impl Write) -> i32 {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = gzread(&mut inp, &mut buf);
        assert!(len >= 0, "gzread reported an error during decompression");
        if len == 0 {
            break; // C `if (len == 0) break;`
        }
        output
            .write_all(&buf[..len as usize])
            .expect("write to output sink");
    }
    gzclose(Some(inp))
}

/// Port of minigzip `file_compress`: compress the on-disk file `file` to
/// `file` + [`GZ_SUFFIX`] using the gz mode string `mode`, returning the path
/// of the `.gz` file produced.
///
/// Unlike the C original this does **not** `unlink` the source — these tests
/// operate exclusively on temp files and never remove user files.
fn file_compress(file: &Path, mode: &str) -> PathBuf {
    let outfile = append_gz(file);
    let mut input = File::open(file).expect("open source file for reading");
    let out = gzopen(&outfile, mode).expect("gzopen output .gz for writing");
    let rc = gz_compress(&mut input, out);
    assert_eq!(rc, Z_OK, "gzclose after file_compress must report Z_OK");
    outfile
}

/// Port of minigzip `file_uncompress`: if `file` ends in [`GZ_SUFFIX`] the
/// output name is the stripped form, otherwise the input is `file` +
/// [`GZ_SUFFIX`] and the output is `file`. Returns the path written.
///
/// As with [`file_compress`], the source `.gz` is never unlinked.
fn file_uncompress(file: &Path) -> PathBuf {
    let name = file.to_string_lossy();
    let (infile, outfile) = if name.ends_with(GZ_SUFFIX) {
        (file.to_path_buf(), strip_gz(file))
    } else {
        (append_gz(file), file.to_path_buf())
    };
    let inp = gzopen(&infile, "rb").expect("gzopen input .gz for reading");
    let mut output = File::create(&outfile).expect("create uncompressed output file");
    let rc = gz_uncompress(inp, &mut output);
    assert_eq!(rc, Z_OK, "gzclose after file_uncompress must report Z_OK");
    outfile
}

/// Convenience in-memory round-trip: compress `payload` with `mode` into a temp
/// `.gz`, read it straight back, and return the decompressed bytes. Drives the
/// same [`gz_compress`] / [`gz_uncompress`] loops as the file-level helpers but
/// without intermediate plaintext files.
fn roundtrip(payload: &[u8], mode: &str) -> Vec<u8> {
    let gz = TempPath::new("roundtrip");
    {
        let writer = gzopen(gz.path(), mode).expect("gzopen for writing");
        let rc = gz_compress(&mut Cursor::new(payload), writer);
        assert_eq!(rc, Z_OK, "compress-side gzclose must report Z_OK");
    }
    let mut decoded = Vec::new();
    {
        let reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
        let rc = gz_uncompress(reader, &mut decoded);
        assert_eq!(rc, Z_OK, "uncompress-side gzclose must report Z_OK");
    }
    decoded
}

// ===========================================================================
// A. In-memory-driven file round-trips (the core)
// ===========================================================================

/// A small text payload (with an embedded NUL, as in minigzip's examples)
/// must survive a gzip write/read round-trip byte-for-byte at the default
/// compression level (`"wb"`).
#[test]
fn gzip_file_round_trip_small() {
    let payload = b"hello, hello! a small gzip round-trip payload.\0".to_vec();
    assert_eq!(
        roundtrip(&payload, "wb"),
        payload,
        "small payload must round-trip exactly"
    );
}

/// A ~100 KiB payload crosses many [`BUFLEN`] boundaries and exercises the full
/// file -> file path (`file_compress` then `file_uncompress`), exactly as
/// minigzip would when (de)compressing a real file.
#[test]
fn gzip_file_round_trip_large() {
    let payload = make_payload(100 * 1024);

    // `base` is the plaintext source; `file_compress` writes `<base>.gz`. Guard
    // both temp artifacts so they are removed even if an assertion fails.
    let base = TempPath::with_suffix("filebig", "");
    let gz_artifact = TempPath(append_gz(base.path()));
    std::fs::write(base.path(), &payload).expect("write source payload");

    let produced_gz = file_compress(base.path(), "wb");
    assert_eq!(
        produced_gz.as_path(),
        gz_artifact.path(),
        "file_compress appends .gz to the source name"
    );

    // `file_uncompress` strips `.gz`, overwriting `base` with the decoded data.
    let produced_out = file_uncompress(&produced_gz);
    assert_eq!(
        produced_out.as_path(),
        base.path(),
        "file_uncompress strips .gz from the input name"
    );

    let decoded = std::fs::read(&produced_out).expect("read decompressed output");
    assert_eq!(
        decoded, payload,
        "large file -> file gzip round trip must match the source"
    );
}

/// Round-trip at the fastest level (`"wb1"`, `Z_BEST_SPEED`).
#[test]
fn gzip_round_trip_level_1() {
    let payload = make_payload(40_000);
    assert_eq!(
        roundtrip(&payload, "wb1"),
        payload,
        "level 1 must round-trip"
    );
}

/// Round-trip at the default level (`"wb6"`, the `Z_DEFAULT_COMPRESSION` value).
#[test]
fn gzip_round_trip_level_6() {
    let payload = make_payload(40_000);
    assert_eq!(
        roundtrip(&payload, "wb6"),
        payload,
        "level 6 must round-trip"
    );
}

/// Round-trip at the best-compression level (`"wb9"`, `Z_BEST_COMPRESSION`).
#[test]
fn gzip_round_trip_level_9() {
    let payload = make_payload(40_000);
    assert_eq!(
        roundtrip(&payload, "wb9"),
        payload,
        "level 9 must round-trip"
    );
}

/// A zero-byte payload must produce a valid (non-empty) gzip member that reads
/// back as zero bytes, with [`gzeof`] reporting end-of-data on the short read.
#[test]
fn gzip_round_trip_empty() {
    let gz = TempPath::new("empty");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        let empty: &[u8] = &[];
        assert_eq!(
            gz_compress(&mut Cursor::new(empty), writer),
            Z_OK,
            "closing an empty write stream must succeed"
        );
    }

    // The on-disk file is a real gzip member (header + empty DEFLATE + trailer).
    let raw = std::fs::read(gz.path()).expect("read empty .gz");
    assert!(
        raw.starts_with(&[0x1f, 0x8b, 0x08]),
        "even an empty payload yields a framed gzip member"
    );

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let mut buf = [0u8; 64];
    let n = gzread(&mut reader, &mut buf);
    assert_eq!(n, 0, "empty member reads back zero bytes");
    assert!(
        gzeof(&reader),
        "gzeof is true after reading the empty member"
    );
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

// ===========================================================================
// B. minigzip-style character / line APIs
// ===========================================================================

/// Bytes written one-at-a-time with [`gzputc`] must read back in order via
/// [`gzgetc`], which returns `-1` at end of data. Includes a NUL (`0x00`) and a
/// high byte (`0xff`) to prove the `0..=255` value range is distinct from the
/// `-1` EOF sentinel.
#[test]
fn gzputc_gzgetc_round_trip() {
    let data: [u8; 6] = [b'A', 0x00, b'Z', 0xff, b'9', b'!'];
    let gz = TempPath::new("putc_getc");
    {
        let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        for &b in &data {
            // gzputc returns the byte written (`c & 0xff`).
            assert_eq!(gzputc(&mut writer, i32::from(b)), i32::from(b));
        }
        assert_eq!(gzclose(Some(writer)), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let mut got = Vec::new();
    loop {
        let c = gzgetc(&mut reader);
        if c < 0 {
            break; // -1 == end of data
        }
        got.push(c as u8);
    }
    assert_eq!(got.as_slice(), &data, "byte-by-byte sequence must match");
    assert!(gzeof(&reader), "gzeof is true once gzgetc hits the end");
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

/// Lines written with [`gzputs`] must read back line-by-line with [`gzgets`].
///
/// The materialized [`gzgets`] returns the **number of bytes** copied (up to and
/// including the newline), not a `char*`, and yields `0` at end of data; this
/// safe core does not NUL-terminate (that is the FFI shim's job). Each returned
/// span must therefore equal the original line including its `\n`.
#[test]
fn gzputs_gzgets_round_trip() {
    let lines: [&[u8]; 3] = [b"first line\n", b"second line\n", b"third and last\n"];
    let gz = TempPath::new("puts_gets");
    {
        let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        for line in lines {
            // gzputs returns the number of bytes written.
            assert_eq!(gzputs(&mut writer, line), line.len() as i32);
        }
        assert_eq!(gzclose(Some(writer)), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let mut buf = [0u8; 128];
    for expected in lines {
        let n = gzgets(&mut reader, &mut buf);
        assert_eq!(
            &buf[..n],
            expected,
            "gzgets must return the line including its newline"
        );
    }
    // A further gzgets at end of data returns zero bytes.
    assert_eq!(
        gzgets(&mut reader, &mut buf),
        0,
        "gzgets returns 0 at end of data"
    );
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

/// [`gzprintf`] takes pre-typed [`core::fmt::Arguments`] (built with
/// [`format_args!`]) and writes the rendered bytes, returning the count. The
/// formatted text must read back verbatim.
#[test]
fn gzprintf_formats() {
    let gz = TempPath::new("printf");
    {
        let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        let n = gzprintf(&mut writer, format_args!("{}, {}!", "hello", "world"));
        assert_eq!(n, 13, "gzprintf reports the formatted byte count");
        assert_eq!(gzclose(Some(writer)), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let mut buf = [0u8; 64];
    let n = gzread(&mut reader, &mut buf);
    assert!(n >= 0, "gzread must not error");
    assert_eq!(&buf[..n as usize], b"hello, world!");
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

/// After a [`gzgetc`], pushing the byte back with [`gzungetc`] (argument order
/// `(c, state)`, matching C) must make the next [`gzgetc`] return that same
/// byte before the read resumes normally — the gzio push-back contract from
/// example.c.
#[test]
fn gzungetc_pushback() {
    let data = b"XYZ123";
    let gz = TempPath::new("ungetc");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&data[..]), writer), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let first = gzgetc(&mut reader);
    assert_eq!(first, i32::from(b'X'));
    // gzungetc returns the pushed byte (note the C-style `(c, state)` order).
    assert_eq!(
        gzungetc(first, &mut reader),
        first,
        "gzungetc echoes the byte"
    );
    assert_eq!(
        gzgetc(&mut reader),
        i32::from(b'X'),
        "pushed byte is re-read"
    );
    assert_eq!(
        gzgetc(&mut reader),
        i32::from(b'Y'),
        "then the stream resumes"
    );
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

// ===========================================================================
// C. Seek / tell / eof / direct
// ===========================================================================

/// On a write stream [`gztell`] must report the running uncompressed position,
/// advancing by exactly the number of bytes handed to [`gzwrite`].
#[test]
fn gztell_after_write() {
    let gz = TempPath::new("tell_write");
    let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
    assert_eq!(
        gztell(&writer),
        0,
        "a fresh write stream starts at position 0"
    );

    let chunk = b"0123456789";
    assert_eq!(gzwrite(&mut writer, chunk) as usize, chunk.len());
    assert_eq!(
        gztell(&writer),
        chunk.len() as i64,
        "position advances by the bytes written"
    );

    assert_eq!(gzwrite(&mut writer, chunk) as usize, chunk.len());
    assert_eq!(
        gztell(&writer),
        (2 * chunk.len()) as i64,
        "position keeps advancing across writes"
    );
    assert_eq!(gzclose(Some(writer)), Z_OK);
}

/// Seeking a gzip read stream then reading must deliver the bytes at that
/// offset (mirrors example.c's `test_gzio` seek). Covers both `SEEK_SET`
/// (absolute) and `SEEK_CUR` (relative forward skip); positions are confirmed
/// with [`gztell`].
#[test]
fn gzseek_gztell_read() {
    // "hello, hello!" — byte 6 is the space before the second "hello!".
    let payload = b"hello, hello!";
    let gz = TempPath::new("seek_read");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    // (a) absolute seek to offset 6, then read the remainder " hello!".
    {
        let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
        assert_eq!(
            gzseek(&mut reader, 6, SEEK_SET),
            6,
            "gzseek returns the new absolute position"
        );
        assert_eq!(gztell(&reader), 6, "gztell reflects the sought position");
        let mut buf = [0u8; 32];
        let n = gzread(&mut reader, &mut buf);
        assert!(n > 0, "read after seek must deliver bytes");
        assert_eq!(&buf[..n as usize], b" hello!");
        assert_eq!(gzclose(Some(reader)), Z_OK);
    }

    // (b) read "hello" (5 bytes), then SEEK_CUR +1 over the comma, then read
    //     " hello!" — exercises the relative-seek (deferred forward skip) path.
    {
        let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
        let mut head = [0u8; 5];
        assert_eq!(gzread(&mut reader, &mut head), 5);
        assert_eq!(&head, b"hello");
        assert_eq!(
            gzseek(&mut reader, 1, SEEK_CUR),
            6,
            "SEEK_CUR folds in the +1 skip to reach offset 6"
        );
        assert_eq!(gztell(&reader), 6);
        let mut buf = [0u8; 32];
        let n = gzread(&mut reader, &mut buf);
        assert_eq!(&buf[..n as usize], b" hello!");
        assert_eq!(gzclose(Some(reader)), Z_OK);
    }
}

/// [`gzeof`] follows zlib's subtle contract: it is **false** while sitting
/// exactly at the end of the data, and becomes **true** only once a further
/// read fails to deliver any bytes.
#[test]
fn gzeof_at_end() {
    let payload = b"end-of-file probe: a short gzip payload";
    let gz = TempPath::new("eof");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");

    // Read exactly the whole payload: the buffer is filled, so no read has yet
    // gone *past* the end — gzeof must still be false.
    let mut exact = vec![0u8; payload.len()];
    let n = gzread(&mut reader, &mut exact);
    assert_eq!(n as usize, payload.len(), "read the whole payload");
    assert_eq!(exact.as_slice(), &payload[..]);
    assert!(
        !gzeof(&reader),
        "gzeof is false while sitting exactly at end"
    );

    // One more read returns zero and trips the past-end flag.
    let mut tail = [0u8; 8];
    assert_eq!(gzread(&mut reader, &mut tail), 0, "no bytes beyond the end");
    assert!(gzeof(&reader), "gzeof is true after a read past the end");
    assert_eq!(gzclose(Some(reader)), Z_OK);
}

/// [`gzdirect`] must report `0` (not transparent) for a genuine gzip stream:
/// the reader is decompressing a gzip wrapper, not copying raw bytes.
#[test]
fn gzdirect_on_gzip() {
    let payload = make_payload(2048);
    let gz = TempPath::new("direct");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    // gzdirect forces the gzip auto-detect, which sees the magic and decides to
    // decompress -> direct == 0.
    assert_eq!(
        gzdirect(&mut reader),
        0,
        "gzdirect is 0 (false) for a gzip-wrapped file"
    );

    // And the stream still decompresses correctly afterwards.
    let mut decoded = Vec::new();
    assert_eq!(gz_uncompress(reader, &mut decoded), Z_OK);
    assert_eq!(decoded, payload, "decompression after gzdirect is intact");
}

// ===========================================================================
// D. Compatibility & framing
// ===========================================================================

/// The bytes our writer emits must begin with the RFC 1952 gzip framing: the
/// magic `1f 8b` followed by the DEFLATE compression method `08` (the result of
/// the gzip-wrapper `windowBits = 31`).
#[test]
fn gzip_stream_has_valid_header() {
    let payload = make_payload(4096);
    let gz = TempPath::new("header");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    let raw = std::fs::read(gz.path()).expect("read raw .gz bytes");
    assert!(raw.len() > 3, "a finished gzip member is non-trivial");
    // RFC 1952: ID1 = 0x1f, ID2 = 0x8b, CM = 0x08 (deflate).
    assert_eq!(
        &raw[..3],
        &[0x1f, 0x8b, 0x08],
        "gzip magic + DEFLATE method byte"
    );
}

/// [`gzbuffer`] overrides the internal buffer size and must be accepted before
/// any I/O (returning `0`); the subsequent round-trip must remain correct on
/// both the write and read sides.
#[test]
fn gzbuffer_then_round_trip() {
    let payload = make_payload(50_000);
    let gz = TempPath::new("buffer");
    {
        let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        // gzbuffer must be called before the first I/O (size must still be 0).
        assert_eq!(
            gzbuffer(&mut writer, 8192),
            0,
            "gzbuffer succeeds before the first write"
        );
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    let mut decoded = Vec::new();
    {
        let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
        assert_eq!(
            gzbuffer(&mut reader, 8192),
            0,
            "gzbuffer succeeds on a fresh read stream"
        );
        assert_eq!(gz_uncompress(reader, &mut decoded), Z_OK);
    }
    assert_eq!(
        decoded, payload,
        "round trip is correct after a gzbuffer override"
    );
}

/// Data written, then flushed mid-stream with `Z_SYNC_FLUSH`, then followed by
/// more data must all survive the round-trip — the flush inserts a sync point
/// without losing or reordering bytes.
#[test]
fn gzflush_partial() {
    let part1 = b"first part before the sync flush; ".to_vec();
    let part2 = b"second part written after the flush.".to_vec();
    let mut expected = part1.clone();
    expected.extend_from_slice(&part2);

    let gz = TempPath::new("flush");
    {
        let mut writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gzwrite(&mut writer, &part1) as usize, part1.len());
        assert_eq!(
            gzflush(&mut writer, Z_SYNC_FLUSH),
            Z_OK,
            "Z_SYNC_FLUSH must succeed"
        );
        assert_eq!(gzwrite(&mut writer, &part2) as usize, part2.len());
        assert_eq!(gzclose(Some(writer)), Z_OK);
    }

    let mut decoded = Vec::new();
    {
        let reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
        assert_eq!(gz_uncompress(reader, &mut decoded), Z_OK);
    }
    assert_eq!(
        decoded, expected,
        "all bytes survive a mid-stream sync flush"
    );
}

// ===========================================================================
// E. Error / edge cases
// ===========================================================================

/// Opening a non-existent file for reading must fail by returning [`None`]
/// (the safe-Rust spelling of the C `gzopen` returning `NULL`).
#[test]
fn gzopen_nonexistent_read_fails() {
    // A unique path we deliberately never create.
    let missing = TempPath::new("missing");
    let _ = std::fs::remove_file(missing.path()); // ensure it is truly absent
    let handle = gzopen(missing.path(), "rb");
    assert!(
        handle.is_none(),
        "gzopen of a missing file in read mode returns None"
    );
}

/// After a clean round-trip read, [`gzerror`] must report no error: code
/// `Z_OK` and no message — the `(Z_OK, None)` analogue of the C
/// `gzerror(file, &err)` returning an empty string with `err == Z_OK`.
#[test]
fn gzerror_clean_on_success() {
    let payload = make_payload(1024);
    let gz = TempPath::new("err_clean");
    {
        let writer = gzopen(gz.path(), "wb").expect("gzopen for writing");
        assert_eq!(gz_compress(&mut Cursor::new(&payload[..]), writer), Z_OK);
    }

    let mut reader = gzopen(gz.path(), "rb").expect("gzopen for reading");
    let mut buf = vec![0u8; payload.len()];
    let n = gzread(&mut reader, &mut buf);
    assert_eq!(n as usize, payload.len(), "the full payload reads back");
    assert_eq!(buf, payload, "and matches the source");

    let (code, msg) = gzerror(&reader);
    assert_eq!(code, Z_OK, "no error code after a clean read");
    assert!(msg.is_none(), "no error message after a clean read");
    assert_eq!(gzclose(Some(reader)), Z_OK);
}
