//! Byte-for-byte interoperability tests against canonical C zlib.
//!
//! This is the **strongest correctness assertion in the suite** (AAP §0.3.3 /
//! §0.6.4): it proves that `zlib-rs` produces output that is *byte-for-byte
//! identical* to canonical C zlib and that the two implementations decode each
//! other's streams. The reference oracle is the [`flate2`] crate, which — when
//! built against its C-zlib backend — links the canonical zlib library and is
//! therefore the ground truth the port must match.
//!
//! The "drop-in bit-exactness" risk concentrates here: real-world experience
//! (Firefox / zlib-ng) shows that a nominal "drop-in" replacement can differ in
//! output at specific compression levels because per-level match/Huffman
//! decisions differ. Because bit-exact output across **every**
//! `(level, strategy, windowBits)` tuple is a hard requirement, `zlib-rs` must
//! replicate zlib's *exact* LZ77 and Huffman decisions — not a zlib-ng-style
//! variant. The wire-format compliance targets are RFC 1950 (zlib container) and
//! RFC 1951 (raw DEFLATE), with the gzip container governed by RFC 1952.
//!
//! # REQUIRES: flate2 with a C-zlib backend (the `"zlib"` feature)
//!
//! The **byte-for-byte** assertions (Tier 2 below) are only valid when `flate2`
//! is built against a **C-zlib** backend. `flate2`'s *default* backend is the
//! pure-Rust `miniz_oxide`, whose output is **not** byte-identical to C zlib at
//! all levels. The workspace `zlib-rs/Cargo.toml` therefore pins the dev
//! dependency as
//! `flate2 = { version = "1.1.9", default-features = false, features = ["zlib"] }`,
//! which drops the `miniz_oxide` default and routes `flate2` through `libz-sys`
//! to the system zlib — exactly the canonical implementation the port must
//! match. If that backend is ever changed, the Tier-2 tests would be comparing
//! against `miniz_oxide` and could legitimately differ; the Tier-1 tests below
//! remain valid regardless.
//!
//! # Two tiers of assertions
//!
//! * **Tier 1 — backend-independent ground truth.** Hard-coded known-answer
//!   wire vectors (verified against canonical C zlib) and cross-decode
//!   compatibility (each implementation decodes the other's output). These hold
//!   regardless of `flate2`'s backend and are the non-negotiable assertions.
//! * **Tier 2 — requires the C-zlib backend.** Byte-for-byte equality of
//!   `zlib-rs` vs `flate2` compressed output across the level matrix. A mismatch
//!   here is a **real** bit-exactness defect in the core, not a test bug.
//!
//! # Feature gating
//!
//! The whole file requires `std` (both `flate2` and the [`std::io`] `Read` /
//! `Write` traits need it), so it is gated with `#![cfg(feature = "std")]`;
//! under `--no-default-features` the test crate is simply empty. gzip-container
//! helpers and tests are additionally gated behind `#[cfg(feature = "gzip")]`,
//! because gzip is not a default feature and the gzip engine path is
//! feature-gated in the core — run them with `--features gzip`.

// `flate2` and the std I/O traits are unavailable without the standard library;
// gate the entire test crate on `std` so `--no-default-features` builds compile
// (as an empty crate) rather than failing to resolve `flate2`/`std::io`.
#![cfg(feature = "std")]
// Binding convention for the `tests/` folder: integration tests are safe-Rust
// that touch only the public API and carry `#![forbid(unsafe_code)]` (this file
// included). The sole, documented exception is `interop_oracle.rs`, whose only
// `unsafe` is the unavoidable low-level C-zlib (`deflateInit2_`) FFI it uses as
// a byte-for-byte oracle for the full `(level, strategy, windowBits)` matrix
// that `flate2`'s high-level API cannot drive.
#![forbid(unsafe_code)]

use std::io::Read;

use flate2::read::{DeflateDecoder, ZlibDecoder};
use flate2::{Compress, Compression, FlushCompress, Status};

use zlib_rs::checksum::adler32;
use zlib_rs::constants::{DEF_MEM_LEVEL, Flush, Strategy, Z_DEFLATED};
use zlib_rs::deflate::{deflate, deflate_init2};
use zlib_rs::error::{ReturnCode, ZlibError};
use zlib_rs::inflate::InflateState;
use zlib_rs::stream::ZStream;
use zlib_rs::util::{compress_bound, compress2, uncompress};

// gzip-container support is feature-gated in the core; pull the gzip oracle
// encoder/decoder (and the `Write` trait it needs) in only when the matching
// tests are compiled. The non-gzip oracle uses the one-shot `Compress` API and
// the streaming-read decoders, neither of which needs `Write`.
#[cfg(feature = "gzip")]
use flate2::read::GzDecoder;
#[cfg(feature = "gzip")]
use flate2::write::GzEncoder;
#[cfg(feature = "gzip")]
use std::io::Write;

// ===========================================================================
// Standard test inputs (reused across every phase)
// ===========================================================================

/// A natural-language paragraph: enough redundancy to exercise LZ77 matches,
/// enough variety to exercise literals and a non-trivial Huffman tree.
const TEXT_PARAGRAPH: &str = "The DEFLATE compressed data format combines the \
LZ77 duplicate-string-elimination algorithm with Huffman coding. Each block is \
preceded by a pair of Huffman trees; matches are encoded as length/distance \
pairs while novel bytes are emitted as literals. zlib wraps DEFLATE in a small \
container with an Adler-32 trailer, gzip wraps it with a CRC-32 trailer, and \
raw DEFLATE has no wrapper at all. The quick brown fox jumps over the lazy dog.";

/// `b"hello, hello!"` — the canonical 13-byte wire-vector input (no trailing
/// NUL). Used by the known-answer vectors in Phase B as well as the matrix
/// phases.
const HELLO: &[u8] = b"hello, hello!";

/// Builds the standard `(label, data)` input set exercised by every phase.
///
/// The labels are carried alongside the bytes so that a byte-for-byte mismatch
/// can name precisely which input diverged.
fn standard_inputs() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("hello", HELLO.to_vec()),
        ("paragraph", TEXT_PARAGRAPH.as_bytes().to_vec()),
        ("zeros_4k", vec![0u8; 4096]),
        ("repetitive", repetitive_buffer()),
        ("pseudo_random_64k", pseudo_random_buffer(64 * 1024)),
    ]
}

/// A highly repetitive buffer (a short phrase repeated): maximally compressible,
/// stresses long-distance matching and the dynamic Huffman path.
fn repetitive_buffer() -> Vec<u8> {
    // ~2 KiB of a single repeated phrase.
    b"The quick brown fox jumps over the lazy dog. "
        .iter()
        .copied()
        .cycle()
        .take(2048)
        .collect()
}

/// A deterministic pseudo-random (effectively incompressible) buffer of `len`
/// bytes, generated with Knuth's multiplicative hash constant
/// (`0x9E3779B1 == 2_654_435_761`). Determinism keeps the test reproducible
/// while avoiding a `rand` dependency; incompressibility forces the compressor
/// down its stored-block path.
fn pseudo_random_buffer(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(2_654_435_761) & 0xFF) as u8)
        .collect()
}

// ===========================================================================
// zlib-rs helpers (the implementation under test)
// ===========================================================================
//
// Tuple-order reminder (AAP §0.6 gotcha): `zlib_rs::deflate::deflate` returns
// `(result, consumed, produced)` whereas `InflateState::inflate` reports
// `InflateResult { consumed, produced, status }`. The helpers below are the one
// place that order matters, so it is handled here once and never leaked.

/// Compress `data` into a zlib (RFC 1950) container at `level` using the one-shot
/// `zlib_rs::util::compress2`. This matches `flate2`'s `ZlibEncoder` parameters
/// exactly (default method/`windowBits = 15`/`memLevel = 8`/default strategy).
fn zrs_zlib(data: &[u8], level: i32) -> Vec<u8> {
    let mut dest = vec![0u8; compress_bound(data.len()) + 64];
    let written = compress2(&mut dest, data, level).expect("zrs compress2 (zlib) should succeed");
    dest.truncate(written);
    dest
}

/// Drive the streaming compressor to completion over the whole `data` buffer at
/// the requested `level`, `window_bits`, and `strategy`, returning the produced
/// bytes. Cleanup is by RAII: dropping `strm` frees the owned compressor state
/// (the `deflateEnd` equivalent), matching the convention in
/// `crate::util::compress`.
fn zrs_deflate_engine(data: &[u8], level: i32, window_bits: i32, strategy: Strategy) -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        window_bits,
        DEF_MEM_LEVEL,
        strategy,
    )
    .expect("deflate_init2 should succeed");

    // `+ 64` margin over the zlib-wrapped bound covers the larger gzip wrapper
    // (10-byte header + 8-byte trailer) and any raw/stored framing slack.
    let mut out = vec![0u8; compress_bound(data.len()) + 64];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let (rc, consumed, produced) = deflate(
            &mut strm,
            &data[in_pos..],
            &mut out[out_pos..],
            Flush::Finish,
        );
        in_pos += consumed;
        out_pos += produced;

        match rc {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(_) => assert!(
                consumed != 0 || produced != 0,
                "deflate stalled with no progress (output buffer too small?)"
            ),
            Err(e) => panic!("deflate engine error: {e:?}"),
        }
    }

    out.truncate(out_pos);
    out
}

/// Compress `data` into raw DEFLATE (RFC 1951, no wrapper) at `level` via the
/// engine with `windowBits = -15`.
fn zrs_raw(data: &[u8], level: i32) -> Vec<u8> {
    zrs_deflate_engine(data, level, -15, Strategy::Default)
}

/// Compress `data` into a gzip (RFC 1952) container at `level` via the engine
/// with `windowBits = 31`.
#[cfg(feature = "gzip")]
fn zrs_gzip(data: &[u8], level: i32) -> Vec<u8> {
    zrs_deflate_engine(data, level, 31, Strategy::Default)
}

/// One-shot decompress a zlib (RFC 1950) stream via `zlib_rs::util::uncompress`,
/// returning a `Result` so the negative-interop tests can assert failures.
/// `cap` is an upper bound on the decompressed size.
fn zrs_try_unzlib(comp: &[u8], cap: usize) -> Result<Vec<u8>, ZlibError> {
    let mut out = vec![0u8; cap];
    let produced = uncompress(&mut out, comp)?;
    out.truncate(produced);
    Ok(out)
}

/// Infallible zlib decode (panics on error) for the positive round-trip phases.
fn zrs_unzlib(comp: &[u8], cap: usize) -> Vec<u8> {
    zrs_try_unzlib(comp, cap).expect("zrs uncompress (zlib) should succeed")
}

/// Decompress a stream through the streaming inflate engine at the given
/// `window_bits` (`-15` raw, `31` gzip, `47` auto-detect), returning a `Result`.
/// `cap` bounds the decompressed size. Cleanup is by RAII (the owned
/// `InflateState` is dropped on return).
fn zrs_inflate_engine(comp: &[u8], window_bits: i32, cap: usize) -> Result<Vec<u8>, ZlibError> {
    let mut state = InflateState::new(window_bits)?;
    let mut out = vec![0u8; cap];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let r = state.inflate(&comp[in_pos..], &mut out[out_pos..], Flush::NoFlush);
        in_pos += r.consumed;
        out_pos += r.produced;

        match r.status {
            Ok(ReturnCode::StreamEnd) => {
                out.truncate(out_pos);
                return Ok(out);
            }
            // A one-shot caller cannot satisfy a preset-dictionary request.
            Ok(ReturnCode::NeedDict) => return Err(ZlibError::DataError),
            Ok(_) => {
                // No forward progress means the stream is truncated (all input
                // consumed) or the output buffer is full.
                if r.consumed == 0 && r.produced == 0 {
                    return Err(stall_error(in_pos, comp.len()));
                }
            }
            Err(ZlibError::BufError) => return Err(stall_error(in_pos, comp.len())),
            Err(e) => return Err(e),
        }
    }
}

/// Classify a no-progress / `Z_BUF_ERROR` condition exactly as the C
/// `uncompress` does: if every input byte has been consumed the stream is
/// truncated / incomplete ([`ZlibError::DataError`]); otherwise the output
/// buffer is merely full ([`ZlibError::BufError`]).
fn stall_error(consumed: usize, source_len: usize) -> ZlibError {
    if consumed == source_len {
        ZlibError::DataError
    } else {
        ZlibError::BufError
    }
}

/// Infallible raw-DEFLATE decode (`windowBits = -15`).
fn zrs_unraw(comp: &[u8], cap: usize) -> Vec<u8> {
    zrs_inflate_engine(comp, -15, cap).expect("zrs inflate (raw) should succeed")
}

/// Infallible gzip decode (`windowBits = 31`).
#[cfg(feature = "gzip")]
fn zrs_ungzip(comp: &[u8], cap: usize) -> Vec<u8> {
    zrs_inflate_engine(comp, 31, cap).expect("zrs inflate (gzip) should succeed")
}

// ===========================================================================
// flate2 helpers (the canonical C-zlib oracle)
// ===========================================================================

/// Compress `data` in a SINGLE pass into one large output buffer via flate2's
/// low-level [`Compress`] (C zlib) API.
///
/// This one-shot `avail_out` convention is what makes the byte-for-byte
/// comparison valid. Stored-block (level 0) boundaries are an `avail_out`
/// *chunking* artifact: the C `deflate_stored` length is
/// `min(MAX_STORED, input, avail_out - header)`, so an oracle that drains output
/// in small chunks (flate2's *streaming* `ZlibEncoder`/`DeflateEncoder`) splits
/// large stored data differently (e.g. 65531 + 5) than a one-shot caller
/// (65535 + 1). The implementation under test (`zlib_rs::util::compress2` and
/// the streaming engine driven to completion over the whole buffer) uses the
/// one-shot convention — exactly like the C `compress2()` — so the oracle must
/// too. Levels 1-9 are `avail_out`-independent (block boundaries are
/// algorithm-determined), so only level 0 is sensitive to this.
///
/// `zlib_header = true` selects the zlib container (RFC 1950); `false` selects
/// raw DEFLATE (RFC 1951).
fn f2_compress_oneshot(data: &[u8], level: u32, zlib_header: bool) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(level), zlib_header);
    let mut out = vec![0u8; compress_bound(data.len()) + 64];
    let status = c
        .compress(data, &mut out, FlushCompress::Finish)
        .expect("flate2 Compress::compress should succeed");
    assert_eq!(
        status,
        Status::StreamEnd,
        "flate2 one-shot compress must finish in a single call (output buffer is large enough)"
    );
    let produced = c.total_out() as usize;
    out.truncate(produced);
    out
}

/// Canonical C-zlib **one-shot** zlib-container compression (byte-for-byte
/// oracle).
fn f2_zlib(data: &[u8], level: u32) -> Vec<u8> {
    f2_compress_oneshot(data, level, true)
}

/// Canonical C-zlib **one-shot** raw-DEFLATE compression (byte-for-byte oracle).
fn f2_raw(data: &[u8], level: u32) -> Vec<u8> {
    f2_compress_oneshot(data, level, false)
}

/// Compress `data` into a gzip container at `level` using `flate2`.
#[cfg(feature = "gzip")]
fn f2_gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).expect("flate2 gzip write_all");
    e.finish().expect("flate2 gzip finish")
}

/// Decompress a zlib stream using `flate2`.
fn f2_unzlib(comp: &[u8]) -> Vec<u8> {
    let mut d = ZlibDecoder::new(comp);
    let mut out = Vec::new();
    d.read_to_end(&mut out).expect("flate2 zlib read_to_end");
    out
}

/// Decompress a raw-DEFLATE stream using `flate2`.
fn f2_unraw(comp: &[u8]) -> Vec<u8> {
    let mut d = DeflateDecoder::new(comp);
    let mut out = Vec::new();
    d.read_to_end(&mut out).expect("flate2 raw read_to_end");
    out
}

/// Decompress a gzip stream using `flate2`.
#[cfg(feature = "gzip")]
fn f2_ungzip(comp: &[u8]) -> Vec<u8> {
    let mut d = GzDecoder::new(comp);
    let mut out = Vec::new();
    d.read_to_end(&mut out).expect("flate2 gzip read_to_end");
    out
}

// ===========================================================================
// Diagnostics
// ===========================================================================

/// Assert that two compressed buffers are byte-for-byte identical, panicking
/// with a diagnosable message (container, input label, level, first differing
/// offset, and both byte values) on mismatch.
fn assert_identical(container: &str, label: &str, level: u32, zrs: &[u8], f2: &[u8]) {
    if zrs == f2 {
        return;
    }

    let detail = match zrs.iter().zip(f2.iter()).position(|(a, b)| a != b) {
        Some(off) => format!(
            "first differ at offset {off}: zrs=0x{:02X} f2=0x{:02X}",
            zrs[off], f2[off]
        ),
        None => "common prefix is equal; lengths differ".to_string(),
    };

    panic!(
        "byte-for-byte mismatch [{container}] input='{label}' level={level}: {detail} \
         (zrs.len={}, f2.len={})",
        zrs.len(),
        f2.len()
    );
}

// ===========================================================================
// Phase B — Tier 1: known-answer wire vectors (backend-independent)
// ===========================================================================
//
// These exact bytes were verified against canonical C zlib (system zlib 1.3.1)
// for the 13-byte input `b"hello, hello!"`. They do not involve `flate2` at all
// and therefore hold regardless of `flate2`'s backend; they are the
// backend-independent safety net for the byte-for-byte oracle.

/// `zrs_zlib(b"hello, hello!", 6)` must equal the canonical 18-byte zlib stream:
/// header `78 9c` (level-6 default zlib), the 12-byte DEFLATE payload, and the
/// big-endian Adler-32 trailer `21 70 04 96`.
#[test]
fn zlib_level6_known_answer() {
    const EXPECTED: [u8; 18] = [
        0x78, 0x9C, 0xCB, 0x48, 0xCD, 0xC9, 0xC9, 0xD7, 0x51, 0xC8, 0x00, 0x51, 0x8A, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];
    assert_eq!(zrs_zlib(HELLO, 6), EXPECTED);
}

/// `zrs_raw(b"hello, hello!", 6)` must equal the canonical 12-byte raw DEFLATE
/// stream (RFC 1951; no header, no trailer).
#[test]
fn raw_level6_known_answer() {
    const EXPECTED: [u8; 12] = [
        0xCB, 0x48, 0xCD, 0xC9, 0xC9, 0xD7, 0x51, 0xC8, 0x00, 0x51, 0x8A, 0x00,
    ];
    assert_eq!(zrs_raw(HELLO, 6), EXPECTED);
}

/// The last 4 bytes of a zlib stream, read big-endian (RFC 1950 stores the
/// Adler-32 "most-significant-byte first"), must equal `adler32(1, input)`.
#[test]
fn zlib_trailer_is_adler_be() {
    for (label, data) in standard_inputs() {
        let stream = zrs_zlib(&data, 6);
        assert!(
            stream.len() >= 6,
            "[{label}] a zlib stream is at least header(2) + trailer(4) bytes"
        );

        let trailer = &stream[stream.len() - 4..];
        let trailer_be = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let expected = adler32(1, &data);
        assert_eq!(
            trailer_be, expected,
            "[{label}] zlib Adler-32 trailer (big-endian) must equal adler32(1, input): \
             got 0x{trailer_be:08X}, expected 0x{expected:08X}"
        );
    }
}

// ===========================================================================
// Phase C — Tier 1: cross-decode compatibility (backend-independent)
// ===========================================================================
//
// Each implementation must decode the other's output. These hold regardless of
// `flate2`'s backend: even if `flate2` used `miniz_oxide`, both produce valid
// RFC 1950 / 1951 / 1952 streams, so the *other* decoder must accept them.

/// `zlib-rs` decodes every `flate2` zlib stream (all levels, all inputs).
#[test]
fn zlibrs_decodes_flate2_zlib() {
    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let comp = f2_zlib(&data, level);
            let round = zrs_unzlib(&comp, data.len());
            assert_eq!(
                round, data,
                "[{label}] level={level}: zlib-rs failed to decode flate2's zlib stream"
            );
        }
    }
}

/// `flate2` decodes every `zlib-rs` zlib stream (all levels, all inputs).
#[test]
fn flate2_decodes_zlibrs_zlib() {
    for (label, data) in standard_inputs() {
        for level in 0i32..=9 {
            let comp = zrs_zlib(&data, level);
            let round = f2_unzlib(&comp);
            assert_eq!(
                round, data,
                "[{label}] level={level}: flate2 failed to decode zlib-rs's zlib stream"
            );
        }
    }
}

/// `zlib-rs` decodes every `flate2` raw-DEFLATE stream (all levels, all inputs).
#[test]
fn zlibrs_decodes_flate2_raw() {
    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let comp = f2_raw(&data, level);
            let round = zrs_unraw(&comp, data.len());
            assert_eq!(
                round, data,
                "[{label}] level={level}: zlib-rs failed to decode flate2's raw stream"
            );
        }
    }
}

/// `flate2` decodes every `zlib-rs` raw-DEFLATE stream (all levels, all inputs).
#[test]
fn flate2_decodes_zlibrs_raw() {
    for (label, data) in standard_inputs() {
        for level in 0i32..=9 {
            let comp = zrs_raw(&data, level);
            let round = f2_unraw(&comp);
            assert_eq!(
                round, data,
                "[{label}] level={level}: flate2 failed to decode zlib-rs's raw stream"
            );
        }
    }
}

/// `zlib-rs` decodes every `flate2` gzip stream — proves the gzip header/trailer
/// emitted by C zlib is parsed correctly by the port.
#[cfg(feature = "gzip")]
#[test]
fn zlibrs_decodes_flate2_gzip() {
    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let comp = f2_gzip(&data, level);
            let round = zrs_ungzip(&comp, data.len());
            assert_eq!(
                round, data,
                "[{label}] level={level}: zlib-rs failed to decode flate2's gzip stream"
            );
        }
    }
}

/// `flate2` decodes every `zlib-rs` gzip stream — proves the gzip header/trailer
/// emitted by the port is parsed correctly by C zlib.
#[cfg(feature = "gzip")]
#[test]
fn flate2_decodes_zlibrs_gzip() {
    for (label, data) in standard_inputs() {
        for level in 0i32..=9 {
            let comp = zrs_gzip(&data, level);
            let round = f2_ungzip(&comp);
            assert_eq!(
                round, data,
                "[{label}] level={level}: flate2 failed to decode zlib-rs's gzip stream"
            );
        }
    }
}

/// Auto-detect (`windowBits = 47`) decodes BOTH a `flate2` zlib stream and a
/// `flate2` gzip stream back to the original — proves the 40–47 auto-detect
/// path selects the right container.
#[cfg(feature = "gzip")]
#[test]
fn zlibrs_autodetect() {
    for (label, data) in standard_inputs() {
        let zlib_stream = f2_zlib(&data, 6);
        let from_zlib = zrs_inflate_engine(&zlib_stream, 47, data.len())
            .expect("auto-detect should decode a zlib stream");
        assert_eq!(
            from_zlib, data,
            "[{label}]: windowBits=47 auto-detect failed on a zlib stream"
        );

        let gzip_stream = f2_gzip(&data, 6);
        let from_gzip = zrs_inflate_engine(&gzip_stream, 47, data.len())
            .expect("auto-detect should decode a gzip stream");
        assert_eq!(
            from_gzip, data,
            "[{label}]: windowBits=47 auto-detect failed on a gzip stream"
        );
    }
}

// ===========================================================================
// Phase D — Tier 2: byte-for-byte equality vs the C-zlib oracle
// ===========================================================================
//
// REQUIRES the C-zlib backend (see the top-of-file note). A mismatch here is a
// REAL bit-exactness defect in the core — the assertion must NOT be relaxed.

/// `zlib-rs` zlib output must be byte-for-byte identical to `flate2` (C zlib) at
/// every level for every standard input.
#[test]
fn bytewise_zlib_all_levels() {
    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let zrs = zrs_zlib(&data, level as i32);
            let f2 = f2_zlib(&data, level);
            assert_identical("zlib", label, level, &zrs, &f2);
        }
    }
}

/// `zlib-rs` raw-DEFLATE output must be byte-for-byte identical to `flate2`
/// (C zlib) at every level for every standard input.
#[test]
fn bytewise_raw_all_levels() {
    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let zrs = zrs_raw(&data, level as i32);
            let f2 = f2_raw(&data, level);
            assert_identical("raw", label, level, &zrs, &f2);
        }
    }
}

/// gzip byte-for-byte caveat: the 10-byte gzip header carries an MTIME and
/// OS/XFL byte that legitimately differ between producers, so full-container
/// equality is NOT asserted. Instead this strips the fixed 10-byte header and
/// 8-byte trailer from both producers and asserts:
///
/// * the DEFLATE **payload** bytes are identical (the real bit-exactness proof),
/// * the **CRC-32** trailer (little-endian) equals `crc32(0, input)`, and
/// * the **ISIZE** trailer (little-endian) equals `input.len()` mod 2^32.
///
/// This proves gzip wire-format compliance (RFC 1952) without depending on the
/// cosmetic header bytes.
#[cfg(feature = "gzip")]
#[test]
fn bytewise_gzip_payload_and_trailer() {
    const GZIP_HEADER_LEN: usize = 10;
    const GZIP_TRAILER_LEN: usize = 8;

    for (label, data) in standard_inputs() {
        for level in 0u32..=9 {
            let zrs = zrs_gzip(&data, level as i32);

            assert!(
                zrs.len() >= GZIP_HEADER_LEN + GZIP_TRAILER_LEN,
                "[{label}] level={level}: zlib-rs gzip stream is too short to contain \
                 header + trailer"
            );

            // The gzip container wraps an *identical* raw-DEFLATE stream, so the
            // payload between the fixed 10-byte header and the 8-byte trailer
            // must be byte-for-byte equal to the canonical C-zlib one-shot raw
            // DEFLATE of the same data/level. (The 10-byte gzip header carries
            // MTIME / OS / XFL bytes that legitimately differ between producers,
            // so it is intentionally NOT compared.)
            let zrs_payload = &zrs[GZIP_HEADER_LEN..zrs.len() - GZIP_TRAILER_LEN];
            let f2_payload = f2_raw(&data, level);
            assert_identical("gzip-payload", label, level, zrs_payload, &f2_payload);

            // CRC-32 trailer: bytes [len-8 .. len-4], little-endian (RFC 1952).
            let zrs_trailer = &zrs[zrs.len() - GZIP_TRAILER_LEN..];
            let crc = u32::from_le_bytes([
                zrs_trailer[0],
                zrs_trailer[1],
                zrs_trailer[2],
                zrs_trailer[3],
            ]);
            let expected_crc = zlib_rs::checksum::crc32(0, &data);
            assert_eq!(
                crc, expected_crc,
                "[{label}] level={level}: gzip CRC-32 trailer (little-endian) must equal \
                 crc32(0, input): got 0x{crc:08X}, expected 0x{expected_crc:08X}"
            );

            // ISIZE trailer: bytes [len-4 .. len], little-endian = input length
            // modulo 2^32 (RFC 1952).
            let isize_field = u32::from_le_bytes([
                zrs_trailer[4],
                zrs_trailer[5],
                zrs_trailer[6],
                zrs_trailer[7],
            ]);
            // RFC 1952 ISIZE is the input length modulo 2^32; a `u32` cast is
            // exactly that truncation (test inputs are well under 4 GiB).
            let expected_isize = data.len() as u32;
            assert_eq!(
                isize_field, expected_isize,
                "[{label}] level={level}: gzip ISIZE trailer (little-endian) must equal \
                 input length mod 2^32: got {isize_field}, expected {expected_isize}"
            );
        }
    }
}

// ===========================================================================
// Phase E — Tier 2: strategy & windowBits coverage
// ===========================================================================

/// `flate2`'s high-level API exposes only the level, not the strategy, so a
/// byte-for-byte strategy comparison vs C zlib is not achievable through it.
/// Instead, for each strategy, prove that the `zlib-rs` output is a *valid*
/// RFC 1950 / 1951 stream by having the C-zlib oracle decode it back to the
/// input. Byte-exactness vs C zlib for the *default* strategy is separately
/// proven by the level matrix (Phase D) and the known-answer vectors (Phase B);
/// byte-exactness for *every* strategy (and `windowBits`) is proven by the
/// low-level C `deflateInit2_` oracle in `tests/interop_oracle.rs`.
#[test]
fn strategy_outputs_are_valid() {
    let strategies = [
        Strategy::Default,
        Strategy::Filtered,
        Strategy::HuffmanOnly,
        Strategy::Rle,
        Strategy::Fixed,
    ];

    for (label, data) in standard_inputs() {
        for strategy in strategies {
            // zlib container (windowBits = 15) at the default level.
            let comp = zrs_deflate_engine(&data, 6, 15, strategy);
            let round = f2_unzlib(&comp);
            assert_eq!(
                round, data,
                "[{label}] strategy={strategy:?}: flate2 failed to decode zlib-rs output, \
                 so the stream is not valid RFC 1950/1951"
            );
        }
    }
}

/// For zlib `windowBits` 9..=15, a `zlib-rs`-compressed stream must round-trip
/// through both `flate2` (which reads the window size from the CMF byte) and
/// `zlib-rs` itself. Uses the repetitive input so smaller windows meaningfully
/// constrain match distances. (Exact byte-for-byte equality vs C zlib across
/// the full `windowBits` range is proven separately in `tests/interop_oracle.rs`.)
#[test]
fn windowbits_cross_decode() {
    let data = repetitive_buffer();
    for window_bits in 9i32..=15 {
        let comp = zrs_deflate_engine(&data, 6, window_bits, Strategy::Default);

        // flate2 (C zlib) reads the window size from the zlib header.
        let via_flate2 = f2_unzlib(&comp);
        assert_eq!(
            via_flate2, data,
            "windowBits={window_bits}: flate2 failed to decode zlib-rs output"
        );

        // zlib-rs round-trips its own output at the same window size.
        let via_zrs = zrs_unzlib(&comp, data.len());
        assert_eq!(
            via_zrs, data,
            "windowBits={window_bits}: zlib-rs failed to round-trip its own output"
        );
    }
}

// ===========================================================================
// Phase F — robustness / negative interop
// ===========================================================================

/// A truncated `flate2` zlib stream must be REJECTED with an `Err`, never a
/// panic: there is not enough input to finish, so the decoder reports a
/// data/buffer error.
#[test]
fn zlibrs_rejects_truncated_flate2_stream() {
    let data = TEXT_PARAGRAPH.as_bytes();
    let full = f2_zlib(data, 6);
    assert!(
        full.len() > 8,
        "need a non-trivial stream to truncate meaningfully"
    );

    // Drop the last 5 bytes: this removes the Adler-32 trailer and bites into
    // the DEFLATE data, so inflate exhausts its input before stream end.
    let truncated = &full[..full.len() - 5];
    let result = zrs_try_unzlib(truncated, data.len());
    assert!(
        result.is_err(),
        "a truncated zlib stream must be rejected, got Ok({:?})",
        result.as_ref().map(Vec::len)
    );
}

/// A `flate2` zlib stream whose Adler-32 trailer has been corrupted must be
/// reported as a data error ("incorrect data check"): the DEFLATE data decodes
/// fully, but the trailing checksum no longer matches.
#[test]
fn zlibrs_rejects_corrupted_trailer() {
    let data = TEXT_PARAGRAPH.as_bytes();
    let mut stream = f2_zlib(data, 6);
    let last = stream.len() - 1;

    // Flip the low byte of the big-endian Adler-32 trailer.
    stream[last] ^= 0xFF;

    let result = zrs_try_unzlib(&stream, data.len());
    assert_eq!(
        result,
        Err(ZlibError::DataError),
        "a corrupted Adler-32 trailer must yield a data error (incorrect data check)"
    );
}
