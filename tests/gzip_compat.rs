//! gzip file-I/O parity tests — a Rust port of the library-exercising behavior
//! of the C reference client `test/minigzip.c`.
//!
//! `minigzip.c` is zlib's minimal `gzip`/`gunzip`/`zcat` client. Its core
//! library exercise is a pair of loops:
//!
//! * `gz_compress(FILE *in, gzFile out)` — read the plaintext in `BUFLEN`-sized
//!   chunks and feed each to `gzwrite`, then finalize with `gzclose`.
//! * `gz_uncompress(gzFile in, FILE *out)` — read decompressed bytes back from
//!   `gzread` in `BUFLEN`-sized chunks until end of file, erroring on a negative
//!   return.
//!
//! plus `file_compress`/`file_uncompress`, which drive those loops through real
//! `<name>` / `<name>.gz` files on disk. This module reproduces that
//! *library-exercising* behavior (it deliberately does **not** port the CLI
//! argument parsing) to prove that the [`zlib_rs`] gz file-I/O layer
//! (`src/gz/`) produces and consumes real gzip files per RFC 1952:
//!
//! 1. a write → read round trip through actual files must reproduce the input
//!    byte-for-byte (several payloads, including one spanning many `BUFLEN`
//!    chunks and an incompressible one);
//! 2. a `zlib_rs`-produced `.gz` must decode with an independent reference
//!    decoder (`flate2`'s pure-Rust `miniz_oxide` backend), and a
//!    `flate2`-produced gzip file must be readable by `zlib_rs`;
//! 3. the error diagnostics of `gz_uncompress` are reproduced — a corrupt gzip
//!    stream is reported as an error, and opening a missing file fails.
//!
//! The whole module is gated behind the `gz-io` Cargo feature (which implies
//! `std` + `gzip`); without it the gz layer does not exist and this file
//! compiles to an empty test set. All test logic is safe Rust — there is **zero
//! `unsafe`** here — and only `std` is used for temporary files (no `tempfile`
//! dependency).

#![cfg(feature = "gz-io")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs, process};

use zlib_rs::ReturnCode;
use zlib_rs::gz::{
    GzState, gzbuffer, gzclose, gzclose_r, gzclose_w, gzerror, gzopen, gzread, gzwrite,
};

// ===========================================================================
// Constants mirrored from `minigzip.c`.
// ===========================================================================

/// Working-buffer size for the chunked compress/uncompress loops, mirroring the
/// C `#define BUFLEN 16384` (`minigzip.c` L144). Using this exact size means the
/// large payloads below flow through several `gzwrite`/`gzread` iterations, just
/// as they do in the C client.
const BUFLEN: usize = 16384;

/// gzip filename suffix appended by the `file_compress` path, mirroring the C
/// `#define GZ_SUFFIX ".gz"` (`minigzip.c` L140).
const GZ_SUFFIX: &str = ".gz";

/// zlib's `Z_OK` success code, as returned by the integer-valued gz entry points
/// (`gzclose*`, `gzbuffer`). Named locally so the assertions read like the C
/// checks (`if (gzclose(out) != Z_OK) ...`).
const Z_OK: i32 = 0;

// ===========================================================================
// Temporary-file scaffolding (std only — no `tempfile` dependency).
// ===========================================================================

/// A uniquely named temporary directory for a single test, removed on drop.
///
/// Paths are built under [`std::env::temp_dir`] and made unique with the process
/// id, a per-process monotonic counter, and a nanosecond timestamp, so parallel
/// test binaries — and the parallel test threads within one binary — never
/// collide. Cleanup is best-effort: [`Drop`] removes the whole directory tree,
/// and any error (e.g. a test already removed a file) is ignored.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// Creates a fresh, uniquely named scratch directory tagged with `tag`.
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!(
            "zlib_rs_gzip_compat_{tag}_{}_{nanos}_{seq}",
            process::id()
        ));
        fs::create_dir_all(&dir).expect("create unique scratch directory");
        Self { dir }
    }

    /// Returns the path to `name` inside this scratch directory.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best-effort cleanup; a failure here must never mask a test result.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ===========================================================================
// Payload generators.
// ===========================================================================

/// Builds a highly compressible, repetitive text payload of exactly `target_len`
/// bytes. Used to force multi-chunk (`> BUFLEN`) streaming through the gz layer.
fn repetitive_payload(target_len: usize) -> Vec<u8> {
    const UNIT: &[u8] = b"The quick brown fox jumps over the lazy dog. ";

    let mut v = Vec::with_capacity(target_len + UNIT.len());
    while v.len() < target_len {
        v.extend_from_slice(UNIT);
    }
    v.truncate(target_len);
    v
}

/// Builds a deterministic, effectively incompressible buffer of `len` bytes from
/// a fixed `seed`, using the `rand` dev-dependency (`rand 0.9`). A fixed seed
/// keeps the test hermetic and reproducible across runs.
fn incompressible_payload(len: usize, seed: u64) -> Vec<u8> {
    use rand::RngCore;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    let mut rng = StdRng::seed_from_u64(seed);
    let mut v = vec![0u8; len];
    rng.fill_bytes(&mut v);
    v
}

// ===========================================================================
// gz compress / uncompress helpers (ports of minigzip's inner loops).
// ===========================================================================

/// Port of `minigzip.c`'s `gz_compress` inner loop (`minigzip.c` L369-L392):
/// feed `input` to an already-open gz writer in `BUFLEN`-sized chunks via
/// `gzwrite`, asserting each chunk is fully accepted (`gzwrite` returns the
/// number of uncompressed bytes written, or `0` on error).
///
/// The caller owns opening and closing the handle; closing with `gzclose_w`
/// (not dropping) is what emits the `Z_FINISH` flush and the gzip trailer that
/// finalize a valid member.
fn gz_write_all(out: &mut GzState, input: &[u8]) {
    for chunk in input.chunks(BUFLEN) {
        let written = gzwrite(out, chunk);
        assert_eq!(
            written,
            chunk.len() as i32,
            "gzwrite accepted {written} of {} bytes",
            chunk.len()
        );
    }
}

/// Port of `minigzip.c`'s `gz_uncompress` inner loop (`minigzip.c` L397-L414):
/// read from an already-open gz reader in `BUFLEN`-sized chunks via `gzread`
/// until end of file (`0`), returning the concatenated plaintext. A negative
/// `gzread` return is a decode error and fails the test, mirroring the C
/// `if (len < 0) error(gzerror(in, &err));`.
fn gz_read_all(inp: &mut GzState) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUFLEN];
    loop {
        let n = gzread(inp, &mut buf);
        assert!(n >= 0, "gzread reported an error (returned {n})");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

/// End-to-end round trip through a real file: compress `payload` to `path` with
/// the given open `mode` (via [`gz_write_all`] + `gzclose_w`), then read it back
/// (via [`gz_read_all`] + `gzclose_r`), returning the decompressed bytes. This is
/// the `gz_compress` + `gz_uncompress` pairing of `minigzip.c`, run against disk.
fn round_trip(path: &Path, mode: &str, payload: &[u8]) -> Vec<u8> {
    {
        let mut out = gzopen(path, mode).expect("gzopen for writing should succeed");
        gz_write_all(&mut out, payload);
        assert_eq!(gzclose_w(out), Z_OK, "gzclose_w should finalize cleanly");
    }

    let mut inp = gzopen(path, "rb").expect("gzopen for reading should succeed");
    let restored = gz_read_all(&mut inp);
    assert_eq!(gzclose_r(inp), Z_OK, "gzclose_r should close cleanly");
    restored
}

// ===========================================================================
// Phase 2 — in-memory (through-file) gz round trips (core parity).
// ===========================================================================

/// Core parity: write payloads to a `.gz` file and read them back, asserting the
/// decompressed bytes exactly equal the originals. Covers the canonical
/// `minigzip` literal, an empty payload (a valid empty gzip member), a large
/// repetitive payload spanning multiple `BUFLEN` chunks, and an incompressible
/// random buffer (which forces stored/near-stored DEFLATE blocks).
#[test]
fn gz_write_then_read_round_trip() {
    let scratch = Scratch::new("round_trip");

    // Deliberately non-`BUFLEN`-aligned so the final short chunk is exercised.
    let large = repetitive_payload(BUFLEN * 3 + 123);
    let random = incompressible_payload(40_000, 0x5EED_1234_C0FF_EE01);

    let cases: &[(&str, &[u8])] = &[
        ("hello", &b"hello, hello!"[..]),
        ("empty", &b""[..]),
        ("large", &large[..]),
        ("random", &random[..]),
    ];

    for &(name, payload) in cases {
        let path = scratch.path(&format!("{name}{GZ_SUFFIX}"));
        let restored = round_trip(&path, "wb", payload);
        assert_eq!(
            &restored[..],
            payload,
            "round trip changed the '{name}' payload ({} bytes)",
            payload.len()
        );
    }
}

/// Mirrors `minigzip`'s `gzbuffer` usage: set a non-default internal buffer size
/// on a freshly opened handle (before any I/O allocates the buffers) and confirm
/// the round trip still holds. Also asserts the C contract that `gzbuffer` after
/// I/O has begun is rejected (`-1`).
#[test]
fn gz_buffer_setting() {
    let scratch = Scratch::new("buffer");
    let path = scratch.path("buffered.gz");
    let payload = repetitive_payload(BUFLEN * 2 + 7);

    // A non-default buffer size (smaller than the 8192 default), valid because it
    // is >= 8 and can be doubled without overflow.
    let custom = 4096u32;

    {
        let mut out = gzopen(&path, "wb").expect("gzopen for writing");
        assert_eq!(
            gzbuffer(&mut out, custom),
            0,
            "gzbuffer before any I/O must succeed"
        );
        gz_write_all(&mut out, &payload);
        // The buffers are now allocated, so a second call must be rejected
        // (the C `state->size != 0` guard).
        assert_eq!(
            gzbuffer(&mut out, custom),
            -1,
            "gzbuffer after I/O has begun must fail with -1"
        );
        assert_eq!(gzclose_w(out), Z_OK, "finalize buffered writer");
    }

    let mut inp = gzopen(&path, "rb").expect("gzopen for reading");
    assert_eq!(
        gzbuffer(&mut inp, custom),
        0,
        "gzbuffer on a fresh reader must succeed"
    );
    let restored = gz_read_all(&mut inp);
    assert_eq!(gzclose_r(inp), Z_OK, "close buffered reader");

    assert_eq!(&restored[..], &payload[..], "buffered round trip mismatch");
}

// ===========================================================================
// Phase 3 — named-file compress / uncompress (file_compress / file_uncompress).
// ===========================================================================

/// Port of `file_compress` + `file_uncompress` (`minigzip.c` L421-L484): write a
/// plaintext file `foo`, compress it to `foo.gz` (appending [`GZ_SUFFIX`]), then
/// decompress `foo.gz` back to `foo2`, asserting the final bytes equal the
/// original.
///
/// Deviation from `minigzip`: the C client `unlink`s the source between steps;
/// this test keeps every file so it can compare against the original plaintext
/// (held in memory as well), then relies on [`Scratch`]'s drop for cleanup. The
/// compress step is run at several open "mode" strings carrying a level digit —
/// default (`"wb"`), best speed (`"wb1"`), and best compression (`"wb9"`) — each
/// of which must produce a valid gzip file that round-trips identically.
#[test]
fn file_compress_then_uncompress() {
    let scratch = Scratch::new("file");

    // Plaintext source `foo`, with a distinctive tail so a truncated/misaligned
    // round trip would be caught.
    let original = {
        let mut v = repetitive_payload(BUFLEN + 4096);
        v.extend_from_slice(b"\n-- trailing distinct bytes --\n");
        v
    };
    let src = scratch.path("foo");
    fs::write(&src, &original).expect("write plaintext source `foo`");

    for mode in ["wb", "wb1", "wb9"] {
        // file_compress equivalent: read `foo`, produce `foo.gz`.
        let gz_path = scratch.path(&format!("foo{GZ_SUFFIX}"));
        let input = fs::read(&src).expect("read plaintext source");
        {
            let mut out = gzopen(&gz_path, mode).expect("gzopen `foo.gz` for writing");
            gz_write_all(&mut out, &input);
            assert_eq!(gzclose_w(out), Z_OK, "finalize `foo.gz` at mode {mode}");
        }

        // file_uncompress equivalent: read `foo.gz`, produce `foo2`. Closed via
        // the `gzclose` dispatcher (which routes a reader to `gzclose_r`).
        let dst = scratch.path("foo2");
        let mut inp = gzopen(&gz_path, "rb").expect("gzopen `foo.gz` for reading");
        let restored = gz_read_all(&mut inp);
        assert_eq!(gzclose(inp), Z_OK, "close `foo.gz` reader at mode {mode}");
        fs::write(&dst, &restored).expect("write decompressed `foo2`");

        let final_bytes = fs::read(&dst).expect("read decompressed `foo2`");
        assert_eq!(
            final_bytes, original,
            "named-file round trip mismatch at mode {mode}"
        );

        // Drop the intermediate `.gz` before the next mode (the scratch dir is
        // removed on drop regardless).
        let _ = fs::remove_file(&gz_path);
    }
}

// ===========================================================================
// Phase 4 — interop: a zlib_rs `.gz` is valid gzip, and vice versa.
// ===========================================================================

/// Proves RFC 1952 gzip-format correctness against an independent reference: a
/// `zlib_rs`-produced `.gz` must decode with `flate2`'s `GzDecoder` (its
/// pure-Rust `miniz_oxide` backend, so no C toolchain is involved), and a
/// `flate2`-produced gzip file must be readable by the `zlib_rs` gz reader.
#[test]
fn gz_output_is_valid_gzip() {
    use flate2::Compression;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    let scratch = Scratch::new("interop");
    let payload = repetitive_payload(BUFLEN * 2 + 321);

    // Forward: zlib_rs writes the `.gz`, flate2 decodes it.
    let forward = scratch.path("forward.gz");
    {
        let mut out = gzopen(&forward, "wb").expect("gzopen for writing");
        gz_write_all(&mut out, &payload);
        assert_eq!(gzclose_w(out), Z_OK, "finalize zlib_rs `.gz`");
    }
    let mut decoded = Vec::new();
    {
        let f = fs::File::open(&forward).expect("open zlib_rs `.gz`");
        let mut dec = GzDecoder::new(f);
        dec.read_to_end(&mut decoded)
            .expect("flate2 must decode the zlib_rs `.gz`");
    }
    assert_eq!(
        decoded, payload,
        "flate2 did not reproduce the payload from the zlib_rs `.gz`"
    );

    // Reverse: flate2 writes the `.gz`, zlib_rs reads it.
    let reverse = scratch.path("reverse.gz");
    {
        let f = fs::File::create(&reverse).expect("create flate2 `.gz`");
        let mut enc = GzEncoder::new(f, Compression::best());
        enc.write_all(&payload).expect("flate2 encode payload");
        enc.finish().expect("flate2 finalize gzip stream");
    }
    let mut inp = gzopen(&reverse, "rb").expect("gzopen flate2 `.gz` for reading");
    let restored = gz_read_all(&mut inp);
    assert_eq!(gzclose_r(inp), Z_OK, "close flate2 `.gz` reader");
    assert_eq!(
        &restored[..],
        &payload[..],
        "zlib_rs did not reproduce the payload from the flate2 `.gz`"
    );
}

// ===========================================================================
// Phase 5 — error / edge behavior (mirror minigzip diagnostics).
// ===========================================================================

/// Mirrors `gz_uncompress`'s negative-return diagnostic: a corrupt gzip stream
/// must be reported as an error by the reader.
///
/// A purely random file cannot be used — the gz reader treats input without the
/// gzip magic as a *transparent* stream and copies it through verbatim (not an
/// error) — and a data error before any output is produced is accepted as
/// trailing junk. So we take a fully valid `zlib_rs`-produced `.gz` and corrupt
/// its trailing CRC-32 integrity field: the payload decodes in full (marking the
/// member as genuine, not junk), and then the integrity check fails, yielding a
/// real `Z_DATA_ERROR`. The gzip trailer is the last 8 bytes — CRC-32 then ISIZE,
/// each 4-byte little-endian — so flipping a byte at `len - 8` corrupts the CRC.
#[test]
fn gzread_on_truncated_is_error() {
    let scratch = Scratch::new("corrupt");

    // Produce a fully valid gzip file first (large enough that output is emitted
    // well before the trailer is reached).
    let good = scratch.path("good.gz");
    let payload = repetitive_payload(BUFLEN + 1000);
    {
        let mut out = gzopen(&good, "wb").expect("gzopen for writing");
        gz_write_all(&mut out, &payload);
        assert_eq!(gzclose_w(out), Z_OK, "finalize valid `.gz`");
    }

    // Corrupt the CRC-32 field in the trailer.
    let mut bytes = fs::read(&good).expect("read valid `.gz`");
    assert!(
        bytes.len() > 8,
        "a valid gzip stream must carry an 8-byte trailer"
    );
    let crc_pos = bytes.len() - 8;
    bytes[crc_pos] ^= 0xFF;
    let bad = scratch.path("bad.gz");
    fs::write(&bad, &bytes).expect("write corrupted `.gz`");

    // Reading must surface an error: some call to `gzread` returns a negative
    // value, exactly as `gz_uncompress` detects with `if (len < 0)`.
    let mut inp = gzopen(&bad, "rb").expect("gzopen corrupted `.gz`");
    let mut buf = vec![0u8; BUFLEN];
    let mut saw_error = false;
    loop {
        let n = gzread(&mut inp, &mut buf);
        if n < 0 {
            saw_error = true;
            break;
        }
        if n == 0 {
            break; // clean end of file — no error observed
        }
    }
    assert!(
        saw_error,
        "a corrupted gzip trailer was not reported as a read error"
    );

    // The error must also be observable through `gzerror` (minigzip prints the
    // message via `gzerror(in, &err)`): a non-zero code and a non-empty message.
    let mut errnum = 0i32;
    let msg = gzerror(&inp, Some(&mut errnum));
    assert_eq!(
        errnum,
        ReturnCode::DataError.as_c_int(),
        "expected Z_DATA_ERROR from the corrupted stream, got code {errnum} ({msg:?})"
    );
    assert!(!msg.is_empty(), "gzerror should provide an error message");

    // A read handle whose last error is a data error still closes as Z_OK (only a
    // pending Z_BUF_ERROR is preserved across close).
    assert_eq!(gzclose_r(inp), Z_OK, "close corrupted-stream reader");
}

/// Mirrors `minigzip`'s NULL-handle check after `gzopen`: opening a nonexistent
/// path for reading fails. The idiomatic API returns `Err(ReturnCode::ErrNo)`
/// (the C API returns `NULL` after a failed `open`).
#[test]
fn open_nonexistent_is_error() {
    let scratch = Scratch::new("missing");
    let missing = scratch.path("does_not_exist.gz");
    assert!(
        !missing.exists(),
        "precondition: the target file must be absent"
    );

    match gzopen(&missing, "rb") {
        Ok(_handle) => panic!("gzopen on a nonexistent path unexpectedly succeeded"),
        Err(code) => assert_eq!(
            code,
            ReturnCode::ErrNo,
            "expected Z_ERRNO for a missing file, got {code:?}"
        ),
    }
}
