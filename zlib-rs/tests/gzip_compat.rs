//! gzip wire-format compatibility tests — a black-box port of the reference
//! gzip client `test/minigzip.c`, validated against RFC 1952.
//!
//! `minigzip` is the canonical "gzip / gunzip / zcat" demo that ships with
//! zlib: it compresses to and decompresses from `.gz` files through the buffered
//! `gz*` file API. This suite ports that driver behavior into integration tests
//! that exercise the **public idiomatic `gz*` API** of the `zlib-rs` core
//! (`zlib_rs::gz::*`) — never the raw deflate/inflate engine and never the
//! `extern "C"` shim (which lives in `libz-rs-sys` and is out of scope here).
//!
//! Three independent properties are checked:
//!
//! 1. **Round-trip losslessness** — data written through `gzwrite` and read back
//!    through `gzread` is recovered byte-for-byte, across every compression
//!    level and strategy and at the empty / single-byte edges (Phase A).
//! 2. **Wire-format correctness** — the bytes written form a valid RFC 1952
//!    gzip container: the `1f 8b 08` signature, a sane flag byte, and a correct
//!    little-endian CRC-32 / ISIZE trailer (Phase B).
//! 3. **Cross-decoder interop** — the output is decodable by an independent
//!    reference decoder (`flate2`, which links canonical C zlib) and, in the
//!    other direction, `zlib-rs` reads a `flate2`-produced gzip file, including
//!    concatenated multi-member files (Phase C).
//!
//! Phases D–F cover the text/char API (`gzputs`/`gzgets`/`gzputc`/`gzgetc`/
//! `gzungetc`/`gzprintf`), the positional API (`gzseek`/`gztell`/`gzrewind`/
//! `gzoffset`/`gzeof`/`gzdirect`), and the error surface (missing files,
//! transparent pass-through of non-gzip input).
//!
//! # Feature gate
//!
//! The entire file is gated behind the `gz-io` Cargo feature (the `gz*` API
//! depends on `std::fs` / `std::io` and is feature-gated in the core). With
//! `--no-default-features` the file compiles to an empty test binary with zero
//! tests. The `flate2` dev-dependency used as the reference oracle also requires
//! `std`.

// `#![cfg(...)]` as the first inner attribute conditionally compiles the WHOLE
// file: under `--no-default-features` (no `gz-io`) the file becomes empty and
// the test binary links with zero tests, exactly as the validation gate
// requires. When `gz-io` is on, everything below — including the safety
// contract — is compiled.
#![cfg(feature = "gz-io")]
// This suite, like the core it tests, contains no `unsafe`.
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Reference oracle: `flate2` links canonical C zlib, so it is an independent
// implementation of the gzip container for the interop checks in Phase C.
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

// The system under test: the public, idiomatic `gz*` surface of the core, plus
// the public CRC-32 used to verify the gzip trailer and the strategy enum used
// to drive `gzsetparams`.
use zlib_rs::checksum::crc32;
use zlib_rs::constants::{Flush, Strategy};
use zlib_rs::gz::{
    GzFile, SEEK_CUR, SEEK_SET, gzbuffer, gzclose, gzclose_r, gzclose_w, gzdirect, gzeof, gzflush,
    gzgetc, gzgets, gzoffset, gzopen, gzprintf, gzputc, gzputs, gzread, gzrewind, gzseek,
    gzsetparams, gztell, gzungetc, gzwrite,
};

// ===========================================================================
// Shared fixtures and helpers
// ===========================================================================

/// `BUFLEN` from `minigzip.c` (line 144): the chunk size its `gz_compress` /
/// `gz_uncompress` loops use. Reused here so the read/write helpers mirror the
/// reference driver exactly.
const BUFLEN: usize = 16384;

/// RAII guard owning a unique temporary `.gz` path, removed on `Drop`.
///
/// The path is built in [`std::env::temp_dir`] from a caller-supplied tag, the
/// process id, and a per-process atomic counter, so concurrently running tests
/// (libtest runs them in parallel) never collide and the suite leaves no
/// residue behind. No external `tempfile` crate is used — only `std`.
struct TempGz(PathBuf);

impl TempGz {
    /// Build a fresh, unique temp path tagged with `tag` (typically the test
    /// name). Any stale file left by a previously crashed run is removed up
    /// front so a reused path starts clean.
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zlibrs_gzip_compat_{tag}_{}_{n}.gz",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }

    /// The owned temporary path.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempGz {
    fn drop(&mut self) {
        // Best-effort cleanup; a missing file (e.g. a read-only negative test)
        // is fine.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deterministic, reproducible test payload of exactly `len` bytes.
///
/// The data interleaves a repeating English phrase (highly compressible, so
/// real LZ77 match-finding is exercised) with a sprinkle of `xorshift32`
/// pseudo-random bytes (so the stream is not trivially compressible and spans
/// multiple internal buffers). It uses no external RNG crate, so the same bytes
/// are produced on every run and platform.
fn make_payload(len: usize) -> Vec<u8> {
    let phrase: &[u8] = b"The quick brown fox jumps over the lazy dog. \
        Pack my box with five dozen liquor jugs. ";
    let mut out = Vec::with_capacity(len + phrase.len() + 2);
    let mut state: u32 = 0x9E37_79B9; // arbitrary non-zero seed
    while out.len() < len {
        out.extend_from_slice(phrase);
        // xorshift32 — cheap, deterministic entropy.
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.push((state >> 24) as u8);
        out.push((state >> 8) as u8);
    }
    out.truncate(len);
    out
}

/// Mirror `minigzip`'s `gz_compress` (lines 369-392): open `path` with `mode`,
/// write `payload` in `BUFLEN` chunks via `gzwrite`, then finish with
/// `gzclose_w`. Every step is asserted, so a failure pinpoints the exact stage.
fn write_gz(path: &Path, mode: &str, payload: &[u8]) {
    // Explicit `GzFile` annotation documents the handle type returned by the
    // idiomatic open call.
    let mut file: GzFile = gzopen(path, mode).expect("gzopen (write) returned None");
    let mut off = 0usize;
    while off < payload.len() {
        let end = (off + BUFLEN).min(payload.len());
        let chunk = &payload[off..end];
        let written = gzwrite(&mut file, chunk);
        assert_eq!(
            written,
            chunk.len() as i32,
            "gzwrite reported {written} for a {}-byte chunk",
            chunk.len()
        );
        off = end;
    }
    assert_eq!(gzclose_w(file), 0, "gzclose_w did not return Z_OK");
}

/// Mirror `minigzip`'s `gz_uncompress` (lines 397-414): open `path` for reading
/// and loop `gzread` over `BUFLEN`-byte chunks until it returns 0, returning the
/// concatenated output, then finish with `gzclose_r`.
fn read_gz(path: &Path) -> Vec<u8> {
    let mut file = gzopen(path, "rb").expect("gzopen (read) returned None");
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUFLEN];
    loop {
        let n = gzread(&mut file, &mut buf);
        assert!(n >= 0, "gzread reported an error ({n})");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(gzclose_r(file), 0, "gzclose_r did not return Z_OK");
    out
}

/// Decode `bytes` (a complete single-member gzip container) with the
/// independent `flate2` reference decoder, returning the plaintext.
fn flate2_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("flate2 GzDecoder failed to decode zlib-rs output");
    out
}

/// Encode `payload` into a single-member gzip container using the independent
/// `flate2` reference encoder.
fn flate2_encode(payload: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).expect("flate2 GzEncoder write");
    encoder.finish().expect("flate2 GzEncoder finish")
}

// ===========================================================================
// Phase A — gz_compress / gz_uncompress round-trip (core minigzip behavior)
// ===========================================================================

/// A multi-KiB payload written with the default mode and read back must be
/// recovered byte-for-byte. The 200 KiB size spans many internal `gz*` buffers
/// (default 8 KiB) and several deflate blocks, mirroring `minigzip`'s default
/// `"wb6"` compression path.
#[test]
fn gz_round_trip_default() {
    let tmp = TempGz::new("round_trip_default");
    let payload = make_payload(200 * 1024);

    write_gz(tmp.path(), "wb", &payload);
    let recovered = read_gz(tmp.path());

    assert_eq!(recovered.len(), payload.len(), "recovered length mismatch");
    assert_eq!(recovered, payload, "round-trip is not lossless");
}

/// Every explicit compression level (`"wb0"`..=`"wb9"`) and the default `"wb"`
/// must round-trip losslessly. Level 0 is the stored (uncompressed) gzip path;
/// level 9 is maximum compression — all must reproduce the input exactly.
#[test]
fn gz_round_trip_all_levels() {
    let payload = make_payload(96 * 1024);

    for level in 0..=9 {
        let mode = format!("wb{level}");
        let tmp = TempGz::new(&format!("level{level}"));
        write_gz(tmp.path(), &mode, &payload);
        let recovered = read_gz(tmp.path());
        assert_eq!(recovered, payload, "level {level} round-trip failed");
    }

    // The bare default mode (no explicit level => Z_DEFAULT_COMPRESSION).
    let tmp = TempGz::new("level_default");
    write_gz(tmp.path(), "wb", &payload);
    assert_eq!(
        read_gz(tmp.path()),
        payload,
        "default-level round-trip failed"
    );
}

/// Each deflate strategy must round-trip losslessly, driven two ways that
/// mirror `minigzip`'s `-f` / `-h` / `-r` options: first via the zlib mode-string
/// strategy suffixes (`"wb6f"` filtered, `"wb6h"` Huffman-only, `"wb6R"` RLE),
/// then via an explicit `gzsetparams` call after opening.
#[test]
fn gz_round_trip_strategies() {
    let payload = make_payload(64 * 1024);

    // (a) strategy selected by the mode-string suffix.
    for suffix in ['f', 'h', 'R'] {
        let mode = format!("wb6{suffix}");
        let tmp = TempGz::new(&format!("strategy_mode_{suffix}"));
        write_gz(tmp.path(), &mode, &payload);
        assert_eq!(
            read_gz(tmp.path()),
            payload,
            "strategy suffix '{suffix}' round-trip failed"
        );
    }

    // (b) strategy selected by gzsetparams after gzopen, before any write.
    for strategy in [Strategy::Filtered, Strategy::HuffmanOnly, Strategy::Rle] {
        let tmp = TempGz::new("strategy_setparams");
        let mut file = gzopen(tmp.path(), "wb").expect("gzopen (write)");
        assert_eq!(
            gzsetparams(&mut file, 6, strategy.as_i32()),
            0,
            "gzsetparams({strategy:?}) did not return Z_OK"
        );
        assert_eq!(gzwrite(&mut file, &payload), payload.len() as i32);
        assert_eq!(gzclose_w(file), 0);
        assert_eq!(
            read_gz(tmp.path()),
            payload,
            "gzsetparams strategy {strategy:?} round-trip failed"
        );
    }
}

/// The degenerate sizes round-trip correctly: an empty payload must still
/// produce a complete, valid (empty) gzip member, and a single byte must come
/// back unchanged.
#[test]
fn gz_empty_and_tiny() {
    // Empty: gzopen + gzclose with no writes still emits a valid empty member.
    let tmp_empty = TempGz::new("empty");
    write_gz(tmp_empty.path(), "wb", b"");
    let recovered = read_gz(tmp_empty.path());
    assert!(
        recovered.is_empty(),
        "empty payload did not round-trip to empty"
    );
    // The empty member is still standards-decodable.
    let raw = std::fs::read(tmp_empty.path()).expect("read empty .gz");
    assert!(
        flate2_decode(&raw).is_empty(),
        "flate2 could not decode empty member"
    );

    // Single byte.
    let tmp_one = TempGz::new("tiny");
    write_gz(tmp_one.path(), "wb", b"Z");
    assert_eq!(
        read_gz(tmp_one.path()),
        b"Z",
        "1-byte payload round-trip failed"
    );
}

/// The generic `gzclose` routes to the correct close path by stream mode: a
/// write handle is finalized like `gzclose_w` and a read handle like
/// `gzclose_r`, each returning Z_OK. (`minigzip` always calls the mode-agnostic
/// `gzclose`.)
#[test]
fn gz_generic_close_routes_by_mode() {
    let tmp = TempGz::new("generic_close");
    let payload = make_payload(8 * 1024);

    // Write handle finalized via the generic gzclose.
    let mut wf = gzopen(tmp.path(), "wb").expect("gzopen wb");
    assert_eq!(gzwrite(&mut wf, &payload), payload.len() as i32, "gzwrite");
    assert_eq!(gzclose(wf), 0, "generic gzclose on a write handle");

    // Read handle finalized via the generic gzclose.
    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen rb");
    let mut buf = vec![0u8; payload.len()];
    assert_eq!(gzread(&mut rf, &mut buf), payload.len() as i32, "gzread");
    assert_eq!(buf, payload, "round-trip via generic close path failed");
    assert_eq!(gzclose(rf), 0, "generic gzclose on a read handle");
}

/// `gzbuffer` (custom buffer size, set before any I/O) and a mid-stream
/// `gzflush(Z_SYNC_FLUSH)` must both succeed and leave the stream losslessly
/// recoverable and still a single standards-decodable gzip member.
#[test]
fn gz_buffer_and_flush_round_trip() {
    let tmp = TempGz::new("buffer_flush");
    let payload = make_payload(64 * 1024);
    let half = payload.len() / 2;

    // Write with a non-default buffer size and a sync flush in the middle.
    let mut file = gzopen(tmp.path(), "wb").expect("gzopen (write)");
    assert_eq!(gzbuffer(&mut file, 4096), 0, "gzbuffer (write) before I/O");
    assert_eq!(gzwrite(&mut file, &payload[..half]), half as i32);
    // Z_SYNC_FLUSH flushes pending output without ending the gzip member.
    assert_eq!(
        gzflush(&mut file, Flush::SyncFlush.as_i32()),
        0,
        "gzflush(Z_SYNC_FLUSH) did not return Z_OK"
    );
    assert_eq!(
        gzwrite(&mut file, &payload[half..]),
        (payload.len() - half) as i32
    );
    assert_eq!(gzclose_w(file), 0);

    // Read back with a custom buffer size too.
    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen (read)");
    assert_eq!(gzbuffer(&mut rf, 4096), 0, "gzbuffer (read) before I/O");
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUFLEN];
    loop {
        let n = gzread(&mut rf, &mut buf);
        assert!(n >= 0, "gzread error ({n})");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(gzclose_r(rf), 0);
    assert_eq!(out, payload, "buffer+flush round-trip is not lossless");

    // The flushed stream is still one valid gzip member for an external decoder.
    let raw = std::fs::read(tmp.path()).expect("read .gz");
    assert_eq!(
        flate2_decode(&raw),
        payload,
        "flate2 could not decode flushed member"
    );
}

// ===========================================================================
// Phase B — gzip container wire-format verification (RFC 1952, no oracle)
// ===========================================================================

/// The first three bytes must be the non-negotiable gzip signature: ID1 = 0x1f,
/// ID2 = 0x8b, and CM = 0x08 (the "deflate" method) — RFC 1952 §2.3.1.
#[test]
fn gzip_magic_and_method() {
    let tmp = TempGz::new("magic");
    write_gz(tmp.path(), "wb", &make_payload(4096));

    let raw = std::fs::read(tmp.path()).expect("read .gz");
    assert!(raw.len() >= 10, "gzip file shorter than its 10-byte header");
    assert_eq!(&raw[0..2], &[0x1f, 0x8b], "bad gzip magic (ID1/ID2)");
    assert_eq!(raw[2], 0x08, "CM byte is not 8 (deflate)");
}

/// The 8-byte trailer holds the CRC-32 of the uncompressed data and its size
/// mod 2^32, both **little-endian** (RFC 1952 §2.3.1) — the opposite of zlib's
/// big-endian Adler-32 trailer, a classic porting bug this test guards against.
#[test]
fn gzip_trailer_crc_and_isize() {
    let tmp = TempGz::new("trailer");
    let payload = make_payload(12_345);
    write_gz(tmp.path(), "wb", &payload);

    let raw = std::fs::read(tmp.path()).expect("read .gz");
    let len = raw.len();
    assert!(len >= 18, "gzip file too short for header + trailer");

    let crc = u32::from_le_bytes(raw[len - 8..len - 4].try_into().expect("4-byte CRC field"));
    let isize_field = u32::from_le_bytes(raw[len - 4..len].try_into().expect("4-byte ISIZE field"));

    assert_eq!(
        crc,
        crc32(0, &payload),
        "trailer CRC-32 mismatch (little-endian)"
    );
    assert_eq!(
        isize_field,
        payload.len() as u32,
        "trailer ISIZE mismatch (little-endian)"
    );
}

/// The flag byte (FLG, the 4th header byte) must have no reserved bits set
/// (RFC 1952 §2.3.1.2: bits 5–7 are reserved and must be zero). For a default
/// write the `gz` layer supplies no name / comment / extra / header-CRC fields,
/// so FLG is exactly 0 (verified against the deflate header emitter).
#[test]
fn gzip_header_flags_sane() {
    let tmp = TempGz::new("flags");
    write_gz(tmp.path(), "wb", &make_payload(2048));

    let raw = std::fs::read(tmp.path()).expect("read .gz");
    assert!(raw.len() >= 10, "gzip file shorter than its 10-byte header");
    let flg = raw[3];

    // Reserved bits 5,6,7 (mask 0xE0) MUST be zero per RFC 1952.
    assert_eq!(flg & 0xE0, 0, "reserved FLG bits are set (0x{flg:02x})");
    // The default gz write sets none of FTEXT/FHCRC/FEXTRA/FNAME/FCOMMENT.
    assert_eq!(
        flg, 0,
        "default gz write should produce FLG = 0 (got 0x{flg:02x})"
    );
}

// ===========================================================================
// Phase C — cross-decoder interop (reference oracle, both directions)
// ===========================================================================

/// A `.gz` produced by the `gz*` API must be decodable by the independent
/// `flate2` decoder (canonical C zlib), proving the container is
/// standards-conformant.
#[test]
fn flate2_decodes_zlibrs_gz() {
    let tmp = TempGz::new("flate2_decodes");
    let payload = make_payload(128 * 1024);
    write_gz(tmp.path(), "wb", &payload);

    let raw = std::fs::read(tmp.path()).expect("read .gz");
    let decoded = flate2_decode(&raw);
    assert_eq!(decoded, payload, "flate2 decode of zlib-rs output mismatch");
}

/// A `.gz` produced by `flate2` (canonical C zlib) must be readable by the
/// `gz*` API, proving `zlib-rs` parses externally-produced gzip headers and
/// trailers.
#[test]
fn zlibrs_reads_flate2_gz() {
    let tmp = TempGz::new("reads_flate2");
    let payload = make_payload(128 * 1024);

    let gz_bytes = flate2_encode(&payload);
    std::fs::write(tmp.path(), &gz_bytes).expect("write flate2 .gz");

    let recovered = read_gz(tmp.path());
    assert_eq!(recovered, payload, "zlib-rs read of flate2 output mismatch");
}

/// Two independently produced gzip members concatenated into one file must be
/// read back as the concatenation of both payloads — RFC 1952 multi-member
/// support, which `minigzip` handles via its `inflateReset`-on-`Z_STREAM_END`
/// loop and the `gz` read layer handles transparently.
#[test]
fn gz_concatenated_members() {
    let p1 = make_payload(5_000);
    // A distinct second payload (xor-masked) so the concatenation is unambiguous.
    let p2: Vec<u8> = make_payload(7_000).iter().map(|b| b ^ 0x5A).collect();

    // Produce each member with the gz* API (exactly one member per session),
    // reading its raw bytes back from a temp file.
    let m1 = {
        let t = TempGz::new("concat_m1");
        write_gz(t.path(), "wb", &p1);
        std::fs::read(t.path()).expect("read member 1")
    };
    let m2 = {
        let t = TempGz::new("concat_m2");
        write_gz(t.path(), "wb6", &p2);
        std::fs::read(t.path()).expect("read member 2")
    };

    // Concatenate the two members into one file.
    let mut combined = m1;
    combined.extend_from_slice(&m2);
    let tmp = TempGz::new("concat_combined");
    std::fs::write(tmp.path(), &combined).expect("write combined .gz");

    // gzread must transparently return both payloads back-to-back.
    let recovered = read_gz(tmp.path());
    let mut expected = p1;
    expected.extend_from_slice(&p2);
    assert_eq!(recovered, expected, "concatenated-member read mismatch");
}

// ===========================================================================
// Phase D — text / char API (minigzip's line-oriented path)
// ===========================================================================

/// `gzputs` writes whole lines; `gzgets` reads them back one at a time,
/// stopping after the newline and NUL-terminating the caller's buffer just past
/// the returned bytes (zlib `gzgets` semantics).
#[test]
fn gz_puts_gets_round_trip() {
    let tmp = TempGz::new("puts_gets");
    let lines: [&[u8]; 3] = [b"first line\n", b"second line\n", b"third and last\n"];

    let mut file = gzopen(tmp.path(), "wb").expect("gzopen wb");
    for line in lines {
        let n = gzputs(&mut file, line);
        assert_eq!(n, line.len() as i32, "gzputs returned wrong length");
    }
    assert_eq!(gzclose_w(file), 0, "gzclose_w");

    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen rb");
    let mut buf = [0u8; 128];
    for expected in lines {
        let got = gzgets(&mut rf, &mut buf).expect("gzgets returned None unexpectedly");
        assert_eq!(got, expected, "gzgets line mismatch");
        // gzgets stops *after* the newline.
        assert_eq!(
            got.last(),
            Some(&b'\n'),
            "gzgets did not stop at the newline"
        );
        // The returned slice borrows `buf`; reading buf[n] is a second shared
        // borrow (allowed). zlib appends a NUL just past the returned bytes.
        let n = got.len();
        assert_eq!(buf[n], 0, "gzgets must NUL-terminate the buffer");
    }
    // Past the final line, gzgets reports end-of-data with None.
    assert!(gzgets(&mut rf, &mut buf).is_none(), "expected None at EOF");
    assert_eq!(gzclose_r(rf), 0, "gzclose_r");
}

/// `gzputc` writes single bytes; `gzgetc` reads them back; `gzungetc` pushes a
/// byte back so the next `gzgetc` returns it again. `gzeof` flips to true only
/// after a read attempt runs past the last byte (lazy EOF).
#[test]
fn gz_putc_getc_ungetc() {
    let tmp = TempGz::new("putc_getc");
    let bytes: &[u8] = b"ABCDEXYZ";

    let mut file = gzopen(tmp.path(), "wb").expect("gzopen wb");
    for &b in bytes {
        assert_eq!(
            gzputc(&mut file, b as i32),
            b as i32,
            "gzputc returned wrong value"
        );
    }
    assert_eq!(gzclose_w(file), 0, "gzclose_w");

    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen rb");

    // Read the first byte, push it back, then re-read it.
    assert_eq!(gzgetc(&mut rf), bytes[0] as i32, "first gzgetc");
    assert_eq!(
        gzungetc(bytes[0] as i32, &mut rf),
        bytes[0] as i32,
        "gzungetc"
    );
    assert_eq!(gzgetc(&mut rf), bytes[0] as i32, "re-read after gzungetc");

    // Read the remaining bytes.
    for &b in &bytes[1..] {
        assert_eq!(gzgetc(&mut rf), b as i32, "gzgetc mismatch");
    }

    // Reading exactly all bytes leaves gzeof false (lazy EOF).
    assert_eq!(
        gzeof(&rf),
        0,
        "gzeof should be false before reading past end"
    );
    // The next read hits end-of-stream and returns -1.
    assert_eq!(gzgetc(&mut rf), -1, "expected EOF (-1) from gzgetc");
    // Now gzeof reports true.
    assert_eq!(gzeof(&rf), 1, "gzeof should be true after reading past end");

    assert_eq!(gzclose_r(rf), 0, "gzclose_r");
}

/// `gzprintf` formats through `core::fmt::Arguments` (so callers pass
/// `format_args!(...)`), returns the number of bytes written, and the formatted
/// text reads back byte-identically.
#[test]
fn gz_printf_formats() {
    let tmp = TempGz::new("printf");
    let expected = "n 42";

    let mut file = gzopen(tmp.path(), "wb").expect("gzopen wb");
    let n = gzprintf(&mut file, format_args!("{} {}", "n", 42));
    assert_eq!(n, expected.len() as i32, "gzprintf returned wrong length");
    assert_eq!(gzclose_w(file), 0, "gzclose_w");

    let recovered = read_gz(tmp.path());
    assert_eq!(
        recovered.as_slice(),
        expected.as_bytes(),
        "gzprintf output mismatch"
    );
}

// ===========================================================================
// Phase E — positional API
// ===========================================================================

/// `gztell` tracks the uncompressed offset; `gzseek` (forward = decompress and
/// discard) lands at the requested offset for both `SEEK_SET` and `SEEK_CUR`;
/// `gzoffset` reports a plausible compressed offset; `gzrewind` returns to the
/// start; and `gzdirect` is false for a real gzip file.
#[test]
fn gz_seek_tell_rewind() {
    let tmp = TempGz::new("seek");
    let payload = make_payload(4096);
    write_gz(tmp.path(), "wb", &payload);

    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen rb");

    // A genuine gzip stream is not "direct" (transparent).
    assert_eq!(gzdirect(&mut rf), 0, "gzdirect should be 0 for a gzip file");

    // Read 100 bytes; gztell reports the uncompressed offset.
    let mut buf = [0u8; 100];
    assert_eq!(gzread(&mut rf, &mut buf), 100, "initial gzread");
    assert_eq!(&buf[..], &payload[..100], "first 100 bytes mismatch");
    assert_eq!(gztell(&rf), 100, "gztell after reading 100 bytes");

    // Absolute forward seek to offset 1000 (skip = decompress-and-discard).
    assert_eq!(
        gzseek(&mut rf, 1000, SEEK_SET),
        1000,
        "gzseek SEEK_SET result"
    );
    assert_eq!(gztell(&rf), 1000, "gztell after SEEK_SET");
    let mut buf2 = [0u8; 50];
    assert_eq!(gzread(&mut rf, &mut buf2), 50, "gzread after seek");
    assert_eq!(
        &buf2[..],
        &payload[1000..1050],
        "data at offset 1000 mismatch"
    );
    assert_eq!(gztell(&rf), 1050, "gztell after reading at offset 1000");

    // Relative forward seek by 200 → offset 1250.
    assert_eq!(
        gzseek(&mut rf, 200, SEEK_CUR),
        1250,
        "gzseek SEEK_CUR result"
    );
    assert_eq!(gztell(&rf), 1250, "gztell after SEEK_CUR");

    // gzoffset reports a plausible compressed-stream position.
    let comp_len = std::fs::metadata(tmp.path()).expect("metadata").len() as i64;
    let off = gzoffset(&mut rf);
    assert!(
        (0..=comp_len).contains(&off),
        "gzoffset {off} out of range 0..={comp_len}"
    );

    // Rewind returns to the start of the uncompressed stream.
    assert_eq!(gzrewind(&mut rf), 0, "gzrewind");
    assert_eq!(gztell(&rf), 0, "gztell after rewind");
    let mut buf3 = [0u8; 100];
    assert_eq!(gzread(&mut rf, &mut buf3), 100, "gzread after rewind");
    assert_eq!(&buf3[..], &payload[..100], "data after rewind mismatch");

    assert_eq!(gzclose_r(rf), 0, "gzclose_r");
}

/// `gzeof` follows zlib's lazy semantics: it stays false until a read attempt
/// actually runs past the end of the stream.
#[test]
fn gz_eof_semantics() {
    let tmp = TempGz::new("eof");
    let k = 1000usize;
    let payload = make_payload(k);
    write_gz(tmp.path(), "wb", &payload);

    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen rb");
    assert_eq!(gzeof(&rf), 0, "gzeof before any read");

    // Read exactly the remaining bytes — lazy EOF keeps gzeof false.
    let mut buf = vec![0u8; k];
    assert_eq!(
        gzread(&mut rf, &mut buf),
        k as i32,
        "gzread of full payload"
    );
    assert_eq!(buf, payload, "payload mismatch");
    assert_eq!(
        gzeof(&rf),
        0,
        "gzeof should stay false after reading exactly all bytes"
    );

    // A further read returns 0 and now trips EOF.
    let mut tail = [0u8; 16];
    assert_eq!(
        gzread(&mut rf, &mut tail),
        0,
        "expected EOF read of 0 bytes"
    );
    assert_eq!(gzeof(&rf), 1, "gzeof should be true after reading past end");

    assert_eq!(gzclose_r(rf), 0, "gzclose_r");
}

// ===========================================================================
// Phase F — error surface
// ===========================================================================

/// `gzopen` on a non-existent path in read mode returns `None` (never panics).
#[test]
fn gzopen_missing_file_is_none() {
    let tmp = TempGz::new("missing");
    // TempGz::new already removed any stale file; ensure it is absent.
    let _ = std::fs::remove_file(tmp.path());
    assert!(
        gzopen(tmp.path(), "rb").is_none(),
        "gzopen on a missing file should return None"
    );
}

/// Per zlib semantics, `gzread` on a plain (non-gzip) file copies the raw bytes
/// through transparently, and `gzdirect` reports true (1) for such input —
/// never an error.
#[test]
fn gzread_on_non_gzip_is_transparent() {
    let tmp = TempGz::new("plain");
    let plain: &[u8] = b"This is not a gzip file, just plain text.\n";
    std::fs::write(tmp.path(), plain).expect("write plain file");

    let mut rf = gzopen(tmp.path(), "rb").expect("gzopen plain rb");
    let mut buf = vec![0u8; 256];
    let n = gzread(&mut rf, &mut buf);
    assert!(n >= 0, "gzread on plain file must not error");
    assert_eq!(&buf[..n as usize], plain, "transparent read mismatch");

    // A non-gzip stream is read "directly" (transparently).
    assert_eq!(
        gzdirect(&mut rf),
        1,
        "gzdirect should be 1 for non-gzip input"
    );

    assert_eq!(gzclose_r(rf), 0, "gzclose_r");
}
