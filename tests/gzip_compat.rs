// gzip file-format compatibility tests — the Rust port of zlib's `minigzip.c`
// example/test driver (`test/minigzip.c`, AAP §0.4.1 "gzip file-format
// compatibility"; §0.7.1 RFC 1952 gzip conformance).
//
// The ENTIRE file is gated on the `gz-io` Cargo feature. That feature provides
// the gzip FILE-I/O layer (`zlib_rs::gz::*`, the `GzState` handle) and, per the
// manifest, implies `std` + `gzip` (mirroring the C `#ifndef NO_GZCOMPRESS`
// build, AAP §0.5.2). `gz-io` is ON by default, so `cargo test --test
// gzip_compat` runs every test below. Under `--no-default-features` the inner
// `#![cfg(feature = "gz-io")]` attribute makes the whole module empty — the
// integration-test binary compiles to zero tests with no errors, exactly as
// the C build omits the gz layer when `NO_GZCOMPRESS` is defined.
#![cfg(feature = "gz-io")]

//! # gzip file-format compatibility (`minigzip.c` port)
//!
//! These integration tests exercise zlib-rs's gzip file-I/O layer end-to-end
//! through its public, C-style free-function surface — `gzopen` / `gzwrite` /
//! `gzread` / `gzputc` / `gzgetc` / `gzputs` / `gzgets` / `gzprintf` /
//! `gzungetc` / `gzseek` / `gztell` / `gzeof` / `gzdirect` / `gzbuffer` /
//! `gzflush` / `gzerror` / `gzclose` — reproducing the behaviors of the C
//! `minigzip` driver (`gz_compress` / `gz_uncompress` / `file_compress` /
//! `file_uncompress`).
//!
//! ## What is validated
//!
//! * **Round-trips** through real temp files for small, large (multi-buffer),
//!   and empty payloads, and across compression levels (`wb1`/`wb6`/`wb9`).
//! * **Character / line APIs** (`gzputc`/`gzgetc`, `gzputs`/`gzgets`,
//!   `gzprintf`, `gzungetc`) byte-for-byte against their inputs.
//! * **Positioning / status** (`gztell`, `gzseek`, `gzeof`, `gzdirect`).
//! * **RFC 1952 framing** — the produced files start with the gzip magic
//!   `1f 8b 08` and carry a correct `ISIZE` trailer.
//! * **Error / edge behavior** — opening a missing file for reading fails, and
//!   a clean session reports no error.
//!
//! ## Constraints honored
//!
//! * **No `unsafe`** anywhere in this file (AAP §0.6.2).
//! * **Only `zlib_rs` + `std`** — no `flate2`. These tests validate *our* gz
//!   layer against itself (write with zlib-rs, read back with zlib-rs); the
//!   byte-for-byte C-zlib oracle comparison lives in `tests/interop.rs`.
//! * **All temp files** live under [`std::env::temp_dir`] with unique,
//!   parallel-safe names and are removed by a RAII guard ([`TempPath`]); the
//!   repository tree is never written to and no user file is ever unlinked
//!   (unlike the C `minigzip`, which deletes the file it compresses).
//!
//! ## API note (verified against the materialized `src/gz/*`)
//!
//! The gzip file functions are **not** re-exported at the crate root; they live
//! under `zlib_rs::gz::{open, read, write, close, state}`. The opaque handle is
//! a `Box<`[`GzState`]`>` (there is no `GzFile` type alias), and the consuming
//! [`gzclose`] takes it by value as `Option<Box<GzState>>` — moving the handle
//! in and dropping it, the compile-time analogue of C's "the handle is freed by
//! gzclose". The read-/write-specific closers (`gzclose_r`/`gzclose_w`) are not
//! all public, so — exactly like the unified C `gzclose` — every handle here is
//! closed through the top-level [`gzclose`].

use zlib_rs::gz::close::gzclose;
use zlib_rs::gz::open::{gzbuffer, gzeof, gzerror, gzopen, gzseek, gztell};
use zlib_rs::gz::read::{gzdirect, gzgetc, gzgets, gzread, gzungetc};
use zlib_rs::gz::state::GzState;
use zlib_rs::gz::write::{gzflush, gzprintf, gzputc, gzputs, gzwrite};
use zlib_rs::{SEEK_CUR, SEEK_SET, Z_OK, Z_SYNC_FLUSH};

use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ===========================================================================
// Constants ported from minigzip.c
// ===========================================================================

/// Working buffer length used by the `gz_compress` / `gz_uncompress` loops
/// (C `minigzip.c` `#define BUFLEN 16384`). Chosen so that the "large" payload
/// crosses several buffer boundaries on both the write and read sides.
const BUFLEN: usize = 16384;

// ===========================================================================
// RAII temp-file management (replaces minigzip's fixed names + unlink())
// ===========================================================================

/// A unique temporary file path that deletes its backing file on drop.
///
/// The C `minigzip` uses fixed output names derived from the input filename and
/// `unlink()`s the original after (de)compression. That is unsafe for a
/// parallel test runner and would risk touching real files, so this port
/// instead allocates a process-/thread-unique path under
/// [`std::env::temp_dir`]. The name embeds the process id, a high-resolution
/// timestamp, and a monotonically increasing counter so that concurrent
/// `cargo test` threads never collide. Cleanup is best-effort and runs even if
/// a test assertion panics (RAII), so no temp files are leaked and the repo is
/// never written to.
struct TempPath {
    path: PathBuf,
}

impl TempPath {
    /// Build a fresh, unique temp path. `tag` is a short human-readable label
    /// woven into the filename to aid debugging if a file is ever inspected.
    fn new(tag: &str) -> TempPath {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = env::temp_dir();
        path.push(format!("zlibrs_gzip_compat_{tag}_{pid}_{nanos}_{n}.gz"));
        TempPath { path }
    }

    /// Borrow the path for passing to `gzopen` / `std::fs` calls.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best-effort removal: a missing file (e.g. a write that never created
        // it) is not an error.
        let _ = std::fs::remove_file(&self.path);
    }
}

// ===========================================================================
// Helper patterns ported from minigzip.c
// ===========================================================================

/// Compress everything readable from `input` into the gzip handle `out`, then
/// close it — the port of C `gz_compress` (`minigzip.c` L369-392).
///
/// Reads up to [`BUFLEN`] bytes at a time and writes each chunk with `gzwrite`,
/// asserting the full chunk was accepted (the C `if (gzwrite(...) != len)
/// error(...)` guard). The handle is **consumed**: ownership of the
/// `Box<GzState>` moves in and is closed via the unified [`gzclose`] (the C
/// `gz_compress` likewise closes `out`). Returns the `gzclose` status so the
/// caller can assert [`Z_OK`].
fn gz_compress(input: &mut impl Read, mut out: Box<GzState>) -> i32 {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = input
            .read(&mut buf)
            .expect("reading uncompressed input failed");
        if len == 0 {
            break; // EOF
        }
        // `&mut out` (a `&mut Box<GzState>`) deref-coerces to the `&mut GzState`
        // that `gzwrite` expects.
        let written = gzwrite(&mut out, &buf[..len]);
        assert_eq!(
            written, len as i32,
            "gzwrite accepted {written} of {len} bytes (short write)"
        );
    }
    gzclose(Some(out))
}

/// Decompress the entire gzip handle `inp` into the sink `out`, then close the
/// handle — the port of C `gz_uncompress` (`minigzip.c` L397-414).
///
/// Reads with `gzread` into a [`BUFLEN`] buffer until it returns `0` (clean end
/// of data), asserting it never returns a negative error code (the C `if (len <
/// 0) error(...)`), and forwards every chunk to `out`. The handle is consumed
/// and closed via [`gzclose`]; the returned status lets the caller assert
/// [`Z_OK`].
fn gz_uncompress(mut inp: Box<GzState>, out: &mut impl Write) -> i32 {
    let mut buf = [0u8; BUFLEN];
    loop {
        let len = gzread(&mut inp, &mut buf);
        assert!(len >= 0, "gzread returned error code {len}");
        if len == 0 {
            break; // end of decompressed data
        }
        out.write_all(&buf[..len as usize])
            .expect("writing decompressed output failed");
    }
    gzclose(Some(inp))
}

/// Compress the file at `src` into the gzip file at `gz_out` using `mode` — the
/// port of C `file_compress` (`minigzip.c` L421-448), with the destructive
/// `unlink(file)` deliberately omitted.
///
/// C derives the `.gz` output name from the input name and removes the source
/// afterwards. This port takes an explicit, caller-owned `gz_out` temp path
/// instead: it keeps the test parallel-safe (no shared fixed names) and never
/// deletes a source file. Asserts the close result is [`Z_OK`].
fn file_compress(src: &Path, gz_out: &Path, mode: &str) {
    let mut input = File::open(src).expect("opening source file for reading failed");
    let out = gzopen(gz_out, mode).expect("gzopen of .gz output for writing failed");
    assert_eq!(
        gz_compress(&mut input, out),
        Z_OK,
        "gz_compress close != Z_OK"
    );
}

/// Decompress the gzip file at `gz_in` into the plain file at `dst` — the port
/// of C `file_uncompress` (`minigzip.c` L454-492), again without the
/// destructive `unlink`.
///
/// As with [`file_compress`], the C name derivation (strip/append `.gz`) is
/// replaced by an explicit caller-owned `dst` path for parallel safety. Asserts
/// the close result is [`Z_OK`].
fn file_uncompress(gz_in: &Path, dst: &Path) {
    let inp = gzopen(gz_in, "rb").expect("gzopen of .gz input for reading failed");
    let mut out = File::create(dst).expect("creating decompressed output file failed");
    assert_eq!(
        gz_uncompress(inp, &mut out),
        Z_OK,
        "gz_uncompress close != Z_OK"
    );
}

// ===========================================================================
// Shared in-memory round-trip helpers (built on the ported helpers above)
// ===========================================================================

/// Write `payload` to a fresh temp `.gz` with the given `mode`, then read it
/// back through the gz layer and return the decompressed bytes.
///
/// Drives the full ported pipeline: `gzopen(mode)` → [`gz_compress`] (writes +
/// closes), then `gzopen("rb")` → [`gz_uncompress`] (reads + closes). The temp
/// file is removed when the returned-from guard drops at end of scope.
fn roundtrip_mem(payload: &[u8], mode: &str) -> Vec<u8> {
    let tmp = TempPath::new("rt");

    // --- compress (write side) ---
    let out = gzopen(tmp.path(), mode).expect("gzopen for writing failed");
    let mut reader: &[u8] = payload; // `&[u8]: Read`
    assert_eq!(
        gz_compress(&mut reader, out),
        Z_OK,
        "write-side gzclose != Z_OK"
    );

    // --- decompress (read side) ---
    let inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let mut got = Vec::new();
    assert_eq!(
        gz_uncompress(inp, &mut got),
        Z_OK,
        "read-side gzclose != Z_OK"
    );

    got
}

/// Deterministically generate `n` bytes of "mixed text" — printable lines of
/// varying length with embedded newlines — suitable for crossing buffer
/// boundaries while remaining compressible (so the DEFLATE engine is actually
/// exercised rather than storing).
fn mixed_text(n: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog",
    ];
    let mut out = Vec::with_capacity(n + 16);
    let mut i = 0usize;
    while out.len() < n {
        let word = WORDS[i % WORDS.len()];
        out.extend_from_slice(word.as_bytes());
        // Insert spaces and periodic newlines to vary structure.
        if i % 9 == 8 {
            out.push(b'\n');
        } else {
            out.push(b' ');
        }
        i += 1;
    }
    out.truncate(n);
    out
}

/// Drain a read handle completely with `gzread`, returning all decompressed
/// bytes. Asserts `gzread` never reports a negative error code.
fn read_all(inp: &mut GzState) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; BUFLEN];
    loop {
        let n = gzread(inp, &mut buf);
        assert!(n >= 0, "gzread returned error code {n}");
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n as usize]);
    }
    got
}

// ===========================================================================
// A. Core file round-trips (the heart of the minigzip port)
// ===========================================================================

/// Small payload round-trip via `gzopen("wb")` → `gzwrite` → `gzclose`, then
/// `gzopen("rb")` → `gzread` → `gzclose`. Mirrors the canonical minigzip
/// compress-then-decompress flow on a short text buffer (including an embedded
/// NUL, which gzip handles as ordinary binary data).
#[test]
fn gzip_file_round_trip_small() {
    let payload: &[u8] = b"hello, hello!\0 and a little more text to deflate";
    let got = roundtrip_mem(payload, "wb");
    assert_eq!(got.as_slice(), payload, "small round-trip mismatch");
}

/// Large (multi-buffer) payload round-trip exercising the full
/// file → `.gz` → file pipeline via [`file_compress`] + [`file_uncompress`].
///
/// ~100 KB crosses several [`BUFLEN`] (16384) boundaries on both the write and
/// read sides, so the `gz_compress`/`gz_uncompress` chunking loops are fully
/// driven.
#[test]
fn gzip_file_round_trip_large() {
    let payload = mixed_text(100 * 1024);

    let src = TempPath::new("large_src");
    let gz = TempPath::new("large_gz");
    let dst = TempPath::new("large_dst");

    std::fs::write(src.path(), &payload).expect("seeding source file failed");
    file_compress(src.path(), gz.path(), "wb");
    file_uncompress(gz.path(), dst.path());

    let got = std::fs::read(dst.path()).expect("reading decompressed output failed");
    assert_eq!(got, payload, "large file round-trip mismatch");
}

/// Zero-byte payload produces a valid (empty) gzip member that reads back as
/// `0` bytes, with `gzeof` reporting end-of-data.
///
/// Writing nothing and closing must still emit a complete gzip wrapper
/// (header + empty deflate block + trailer). On read-back the first `gzread`
/// returns `0` and — because the request could not be satisfied at end of
/// input — `gzeof` becomes `true`.
#[test]
fn gzip_round_trip_empty() {
    let tmp = TempPath::new("empty");

    // Write side: open for writing and immediately close (no payload).
    let out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzclose(Some(out)),
        Z_OK,
        "closing empty write handle != Z_OK"
    );

    // Read side: a single gzread yields 0 bytes and gzeof is set.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let mut buf = [0u8; 64];
    let n = gzread(&mut inp, &mut buf);
    assert_eq!(n, 0, "empty gzip member should decompress to 0 bytes");
    assert!(
        gzeof(&inp),
        "gzeof should be true after reading an empty member"
    );
    assert_eq!(
        gzclose(Some(inp)),
        Z_OK,
        "closing empty read handle != Z_OK"
    );
}

/// Round-trip at the fastest compression level (`wb1`).
#[test]
fn gzip_round_trip_level_wb1() {
    let payload = mixed_text(40 * 1024);
    let got = roundtrip_mem(&payload, "wb1");
    assert_eq!(got, payload, "level-1 round-trip mismatch");
}

/// Round-trip at the default compression level (`wb6`).
#[test]
fn gzip_round_trip_level_wb6() {
    let payload = mixed_text(40 * 1024);
    let got = roundtrip_mem(&payload, "wb6");
    assert_eq!(got, payload, "level-6 round-trip mismatch");
}

/// Round-trip at the best compression level (`wb9`).
#[test]
fn gzip_round_trip_level_wb9() {
    let payload = mixed_text(40 * 1024);
    let got = roundtrip_mem(&payload, "wb9");
    assert_eq!(got, payload, "level-9 round-trip mismatch");
}

// ===========================================================================
// B. minigzip-style character / line APIs
// ===========================================================================

/// `gzputc` / `gzgetc` byte-at-a-time round-trip, including a NUL and a high
/// (`0xff`) byte to confirm binary transparency, ending with the `-1`
/// end-of-file sentinel from `gzgetc`.
///
/// `gzputc` returns the written byte (`c & 0xff`); `gzgetc` returns the byte in
/// `0..=255`, or `-1` at end of data.
#[test]
fn gzputc_gzgetc_round_trip() {
    let tmp = TempPath::new("putc");
    let data: &[u8] = b"ABCxyz\x00\xff\x01\x7f0123456789";

    // Write each byte individually.
    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    for &b in data {
        let r = gzputc(&mut out, i32::from(b));
        assert_eq!(
            r,
            i32::from(b),
            "gzputc returned the wrong value for {b:#x}"
        );
    }
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    // Read each byte back individually, then confirm EOF.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    for &b in data {
        let c = gzgetc(&mut inp);
        assert_eq!(c, i32::from(b), "gzgetc mismatch for {b:#x}");
    }
    assert_eq!(
        gzgetc(&mut inp),
        -1,
        "gzgetc should return -1 at end of file"
    );
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}

/// `gzputs` / `gzgets` line round-trip. Each `gzgets` returns the byte count it
/// wrote (up to and including the newline); a final `gzgets` past the data
/// returns `0`.
///
/// The safe `gzgets` core fills the provided slice with the raw line bytes
/// (newline included) and returns the count — it does **not** append a C NUL
/// terminator (that belongs to the FFI layer), so the comparison is against the
/// exact line bytes.
#[test]
fn gzputs_gzgets_round_trip() {
    let tmp = TempPath::new("puts");
    let lines = ["alpha\n", "beta\n", "gamma\n"];

    // Write the lines.
    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    for line in &lines {
        let r = gzputs(&mut out, line);
        assert_eq!(r, line.len() as i32, "gzputs short write for {line:?}");
    }
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    // Read them back one line at a time.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    for line in &lines {
        let mut buf = [0u8; 64];
        let n = gzgets(&mut inp, &mut buf);
        assert_eq!(&buf[..n], line.as_bytes(), "gzgets line mismatch");
    }
    // Past the final line there is nothing left to read.
    let mut buf = [0u8; 64];
    assert_eq!(
        gzgets(&mut inp, &mut buf),
        0,
        "gzgets past end of data should return 0"
    );
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}

/// `gzprintf` writes formatted text (accepting [`core::fmt::Arguments`], built
/// here with [`format_args!`]) that reads back verbatim.
///
/// `gzprintf` returns the number of formatted bytes written.
#[test]
fn gzprintf_formats() {
    let tmp = TempPath::new("printf");
    let expected = "hello, world!";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    let n = gzprintf(&mut out, format_args!("{}, {}!", "hello", "world"));
    assert_eq!(
        n,
        expected.len() as i32,
        "gzprintf returned the wrong byte count"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let got = read_all(&mut inp);
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
    assert_eq!(
        got.as_slice(),
        expected.as_bytes(),
        "gzprintf round-trip mismatch"
    );
}

/// `gzungetc` pushes a byte back so the next `gzgetc` returns it again — the
/// gzio pushback behavior exercised by `example.c`.
#[test]
fn gzungetc_pushback() {
    let tmp = TempPath::new("ungetc");
    let payload: &[u8] = b"hello!";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");

    // Read the first byte, push it back, and confirm the next read returns it.
    let c = gzgetc(&mut inp);
    assert_eq!(c, i32::from(b'h'), "first gzgetc");
    assert_eq!(
        gzungetc(&mut inp, c),
        c,
        "gzungetc should return the pushed char"
    );
    assert_eq!(gzgetc(&mut inp), i32::from(b'h'), "gzgetc after gzungetc");

    // The remainder of the stream is intact.
    let rest = read_all(&mut inp);
    assert_eq!(
        rest.as_slice(),
        b"ello!",
        "remainder after pushback mismatch"
    );
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}

// ===========================================================================
// C. Positioning / status: gzseek, gztell, gzeof, gzdirect
// ===========================================================================

/// `gztell` on a write handle advances by exactly the number of bytes written.
#[test]
fn gztell_after_write() {
    let tmp = TempPath::new("tell_w");
    let payload: &[u8] = b"0123456789abcdef"; // 16 bytes

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(gztell(&out), 0, "gztell should be 0 before any write");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(
        gztell(&out),
        payload.len() as i64,
        "gztell should equal the number of bytes written"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "gzclose != Z_OK");
}

/// `gzseek` + `gztell` on a read handle: absolute (`SEEK_SET`) and relative
/// (`SEEK_CUR`) positioning land at the right uncompressed offset, and the
/// subsequent `gzread` returns the bytes from that offset.
///
/// Mirrors the `example.c` `test_gzio` seek check: with the payload
/// `"hello, hello!"`, seeking to offset 7 leaves `"hello!"` to be read.
#[test]
fn gzseek_gztell_read() {
    let tmp = TempPath::new("seek_r");
    let payload: &[u8] = b"hello, hello!"; // indices 7.. == "hello!"

    // Produce the gzip file.
    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    // Absolute seek (SEEK_SET) to offset 7.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    assert_eq!(gzseek(&mut inp, 7, SEEK_SET), 7, "gzseek(SEEK_SET, 7)");
    assert_eq!(gztell(&inp), 7, "gztell after SEEK_SET");
    assert_eq!(
        read_all(&mut inp).as_slice(),
        b"hello!",
        "data after SEEK_SET"
    );
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");

    // Relative seek (SEEK_CUR): SEEK_SET to 2, then +5 -> absolute offset 7.
    let mut inp2 = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    assert_eq!(gzseek(&mut inp2, 2, SEEK_SET), 2, "gzseek(SEEK_SET, 2)");
    assert_eq!(gzseek(&mut inp2, 5, SEEK_CUR), 7, "gzseek(SEEK_CUR, +5)");
    assert_eq!(gztell(&inp2), 7, "gztell after SEEK_CUR");
    assert_eq!(
        read_all(&mut inp2).as_slice(),
        b"hello!",
        "data after SEEK_SET + SEEK_CUR"
    );
    assert_eq!(gzclose(Some(inp2)), Z_OK, "read-side gzclose != Z_OK");
}

/// `gzeof` is `false` before reading and becomes `true` only after a read has
/// reached past the end of the uncompressed data (the classic zlib
/// `past`-vs-`eof` distinction).
#[test]
fn gzeof_at_end() {
    let tmp = TempPath::new("eof");
    let payload: &[u8] = b"some bytes to read back and then hit end of file";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    assert!(!gzeof(&inp), "gzeof should be false before any read");
    let got = read_all(&mut inp);
    assert_eq!(got.as_slice(), payload, "data round-trip mismatch");
    assert!(gzeof(&inp), "gzeof should be true after reading past end");
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}

/// `gzdirect` reports `0` (not transparent) for a real gzip file, because the
/// stream is being decompressed rather than copied verbatim — and reading still
/// returns the original data after the `gzdirect` header probe.
#[test]
fn gzdirect_on_gzip() {
    let tmp = TempPath::new("direct");
    let payload: &[u8] = b"gzip wrapped content, definitely not transparent";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    assert_eq!(
        gzdirect(&mut inp),
        0,
        "gzdirect should be 0 (decompressing) for a gzip stream"
    );
    // The header probe must not disturb the data.
    let got = read_all(&mut inp);
    assert_eq!(
        got.as_slice(),
        payload,
        "data after gzdirect probe mismatch"
    );
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}

// ===========================================================================
// D. Compatibility & framing (RFC 1952), gzbuffer, gzflush
// ===========================================================================

/// A file produced by the gz write path begins with the gzip magic and the
/// DEFLATE compression-method byte — `1f 8b 08` — confirming RFC 1952 framing.
#[test]
fn gzip_stream_has_valid_header() {
    let tmp = TempPath::new("magic");
    let payload: &[u8] = b"content for the framing/magic check";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "gzclose != Z_OK");

    let raw = std::fs::read(tmp.path()).expect("reading raw .gz bytes failed");
    assert!(
        raw.len() >= 10,
        "gzip stream is too short to contain a 10-byte header"
    );
    assert!(
        raw.starts_with(&[0x1f, 0x8b, 0x08]),
        "stream must start with gzip magic 1f 8b and deflate method 08 (RFC 1952), got {:02x?}",
        &raw[..raw.len().min(3)]
    );
}

/// Full RFC 1952 framing: the produced member carries the gzip magic + method
/// header *and* a correct `ISIZE` trailer (the uncompressed length, mod 2^32,
/// little-endian in the final four bytes), and still round-trips.
///
/// The gzip wrapper format requires the `gzip` feature; `gz-io` implies it, so
/// this test runs in the normal `cargo test` invocation. It is `cfg`-gated to
/// be precise about the dependency. (The gz file-I/O layer does not expose a
/// custom-header set/get API, so this validates the framing the layer produces
/// rather than user-supplied `GzHeader` fields.)
#[cfg(feature = "gzip")]
#[test]
fn gzip_framing_rfc1952() {
    let tmp = TempPath::new("rfc1952");
    let payload = mixed_text(5000);

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, &payload),
        payload.len() as i32,
        "gzwrite short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let raw = std::fs::read(tmp.path()).expect("reading raw .gz bytes failed");
    // Header (RFC 1952 §2.3.1): ID1 ID2 CM = 1f 8b 08, plus the fixed-size
    // 10-byte header and 8-byte trailer.
    assert!(
        raw.len() >= 18,
        "gzip member too short for header + trailer"
    );
    assert!(
        raw.starts_with(&[0x1f, 0x8b, 0x08]),
        "gzip magic + compression method"
    );

    // Trailer (RFC 1952 §2.3.1): ISIZE = input size mod 2^32, little-endian, in
    // the last 4 bytes.
    let tail = &raw[raw.len() - 4..];
    let isize_le = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    assert_eq!(
        isize_le,
        payload.len() as u32,
        "gzip ISIZE trailer must equal the uncompressed length (mod 2^32)"
    );

    // And the member still decompresses to the original bytes.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let got = read_all(&mut inp);
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
    assert_eq!(got, payload, "round-trip after framing check");
}

/// `gzbuffer` overrides the internal buffer size when called before the first
/// I/O, and the round-trip remains correct through the resized buffers.
#[test]
fn gzbuffer_then_round_trip() {
    let tmp = TempPath::new("buffer");
    let payload = mixed_text(50 * 1024);

    // Write side: set the buffer size before the first write.
    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzbuffer(&mut out, 8192),
        0,
        "gzbuffer before the first write should succeed"
    );
    let mut reader: &[u8] = &payload;
    assert_eq!(
        gz_compress(&mut reader, out),
        Z_OK,
        "write-side gzclose != Z_OK"
    );

    // Read side: set the buffer size before the first read.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    assert_eq!(
        gzbuffer(&mut inp, 8192),
        0,
        "gzbuffer before the first read should succeed"
    );
    let got = read_all(&mut inp);
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
    assert_eq!(
        got, payload,
        "round-trip with a custom buffer size mismatched"
    );
}

/// `gzflush(Z_SYNC_FLUSH)` between two writes does not lose or corrupt data:
/// reading back yields the concatenation of both write segments.
#[test]
fn gzflush_partial() {
    let tmp = TempPath::new("flush");
    let part1: &[u8] = b"first part written before the sync flush; ";
    let part2: &[u8] = b"second part written after the sync flush.";

    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, part1),
        part1.len() as i32,
        "gzwrite part1 short"
    );
    assert_eq!(
        gzflush(&mut out, Z_SYNC_FLUSH),
        Z_OK,
        "gzflush(Z_SYNC_FLUSH) should return Z_OK"
    );
    assert_eq!(
        gzwrite(&mut out, part2),
        part2.len() as i32,
        "gzwrite part2 short"
    );
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let got = read_all(&mut inp);
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");

    let mut expected = Vec::new();
    expected.extend_from_slice(part1);
    expected.extend_from_slice(part2);
    assert_eq!(
        got, expected,
        "data across a Z_SYNC_FLUSH boundary mismatched"
    );
}

// ===========================================================================
// E. Error / edge behavior
// ===========================================================================

/// Opening a non-existent file for reading returns `None` (the safe analogue of
/// C `gzopen` returning a `NULL` `gzFile`).
#[test]
fn gzopen_nonexistent_read_fails() {
    // `TempPath::new` only reserves a unique name; no file is created.
    let tmp = TempPath::new("missing");
    assert!(
        !tmp.path().exists(),
        "precondition: the temp path must not exist yet"
    );

    let handle = gzopen(tmp.path(), "rb");
    assert!(
        handle.is_none(),
        "gzopen of a missing file in read mode should return None"
    );
}

/// After a clean write-then-read round-trip, `gzerror` reports no error
/// (`Z_OK` with no message) on both the write and read handles.
#[test]
fn gzerror_clean_on_success() {
    let tmp = TempPath::new("err_clean");
    let payload: &[u8] = b"a fully successful round-trip leaves no error behind";

    // Write side: error state stays clean after a successful write.
    let mut out = gzopen(tmp.path(), "wb").expect("gzopen for writing failed");
    assert_eq!(
        gzwrite(&mut out, payload),
        payload.len() as i32,
        "gzwrite short"
    );
    let (wcode, wmsg) = gzerror(&out);
    assert_eq!(wcode, Z_OK, "write handle should report Z_OK");
    assert!(wmsg.is_none(), "write handle should have no error message");
    assert_eq!(gzclose(Some(out)), Z_OK, "write-side gzclose != Z_OK");

    // Read side: error state stays clean after reading to a clean EOF.
    let mut inp = gzopen(tmp.path(), "rb").expect("gzopen for reading failed");
    let got = read_all(&mut inp);
    assert_eq!(got.as_slice(), payload, "round-trip mismatch");
    let (rcode, rmsg) = gzerror(&inp);
    assert_eq!(
        rcode, Z_OK,
        "read handle should report Z_OK after a clean read"
    );
    assert!(rmsg.is_none(), "read handle should have no error message");
    assert_eq!(gzclose(Some(inp)), Z_OK, "read-side gzclose != Z_OK");
}
