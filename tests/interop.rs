// The optional strict byte-identity checks below are gated on
// `#[cfg(feature = "real-zlib-interop")]`. That feature is INTENTIONALLY not
// declared in `Cargo.toml`: it is an opt-in switch a developer enables together
// with a genuine zlib backend for `flate2` (`flate2/zlib` or `flate2/zlib-ng`)
// solely for "bug-for-bug interop verification" (AAP §0.5.2). Because the
// feature is undeclared, `rustc`/`clippy` would otherwise emit the
// `unexpected_cfgs` lint (an error under `-D warnings`); this crate-level
// `allow` documents the intent and keeps the default, C-toolchain-free run
// clean. The gated code is simply absent from every default build.
#![allow(unexpected_cfgs)]

//! Byte-identity and wire-format cross-validation against a reference DEFLATE
//! implementation ([`flate2`]).
//!
//! This integration test is the authoritative **wire-format compatibility**
//! gate for `zlib-rs` (AAP §0.6.7 / §0.7.2). It proves that the crate's zlib
//! (RFC 1950), raw DEFLATE (RFC 1951), and gzip (RFC 1952) streams interoperate
//! with an established, independently-authored codec — the dev-dependency
//! `flate2`, built with its **default `miniz_oxide` backend** (pure Rust, no C
//! toolchain, so continuous integration needs no C compiler).
//!
//! # Testing strategy
//!
//! Byte-for-byte equality between two DEFLATE *encoders* only holds when they
//! implement the *same* match-finding heuristics. `zlib-rs` is a faithful port
//! of the C zlib coder, whereas `flate2`'s default `miniz_oxide` backend is a
//! *different* encoder and need not produce identical compressed bytes at every
//! level. The suite is therefore structured in two tiers:
//!
//! 1. **Decode-compatibility (always-on, primary).** Both directions are
//!    asserted for every framing:
//!    * `flate2` decodes what `zlib-rs` produced, and
//!    * `zlib-rs` decodes what `flate2` produced.
//!
//!    Because each library successfully decodes the *other's* output, these
//!    assertions prove RFC 1950/1951/1952 wire-format conformance without
//!    depending on encoder-identical output. They run across every compression
//!    level and each of the five deflate strategies.
//!
//! 2. **Strict byte-identity (opt-in).** Exact byte equality of the compressed
//!    stream is asserted only against a *genuine* zlib reference, guarded behind
//!    `#[cfg(feature = "real-zlib-interop")]` (see the file-level `allow` note
//!    above). It is never run under the default `miniz_oxide` configuration,
//!    where a byte mismatch is expected and benign.
//!
//! All assertions are black-box over the public `zlib_rs` API; the file
//! contains zero `unsafe`.

// --- Public `zlib-rs` surface (crate-root re-exports + public engine modules) -
// One-call whole-buffer wrappers, the error/strategy enums, and the streaming
// stream handle are crate-root re-exports. The `deflate`/`inflate` streaming
// drivers and the flush/method constants live in the crate's public engine
// modules (`pub mod deflate`, `pub mod inflate`, `pub mod constants`); the raw
// and gzip framings are only reachable through that streaming API, since the
// one-call `compress`/`uncompress` wrappers are zlib-framed only.
use zlib_rs::constants::{DEF_MEM_LEVEL, Z_DEFLATED, Z_FINISH, Z_NO_FLUSH};
use zlib_rs::deflate::{deflate, deflate_end, deflate_init2};
use zlib_rs::inflate::{inflate, inflate_end, inflate_init2};
use zlib_rs::{ReturnCode, Strategy, ZStream, compress_bound, compress2, uncompress};

// --- Reference implementation (`flate2`, default `miniz_oxide` backend) -------
use flate2::Compression;
#[cfg(feature = "gzip")]
use flate2::read::GzDecoder;
use flate2::read::{DeflateDecoder, ZlibDecoder};
#[cfg(feature = "gzip")]
use flate2::write::GzEncoder;
use flate2::write::{DeflateEncoder, ZlibEncoder};
use std::io::{Read, Write};

// ===========================================================================
// windowBits framing selectors (AAP §0.6.4 overloading contract) + sizes
// ===========================================================================

/// zlib wrapper (RFC 1950): 2-byte header + trailing Adler-32.
const WBITS_ZLIB: i32 = 15;
/// Raw DEFLATE (RFC 1951): no wrapper, no checksum.
const WBITS_RAW: i32 = -15;
/// gzip wrapper (RFC 1952): gzip header + trailing CRC-32 and length.
#[cfg(feature = "gzip")]
const WBITS_GZIP: i32 = 31;
/// Auto-detect a zlib or gzip wrapper on inflate (`32 + 15`).
const WBITS_AUTO: i32 = 47;

/// Large-buffer size mirroring `test/example.c`'s `uncomprLen = 20000`.
const LARGE_LEN: usize = 20_000;

/// The canonical zlib exerciser literal from `test/example.c` (`hello[]`).
const HELLO: &[u8] = b"hello, hello!";

/// Every `zlib-rs` compression level: the ten explicit levels plus the
/// `Z_DEFAULT_COMPRESSION` sentinel (`-1`).
const ZLIB_RS_LEVELS: [i32; 11] = [-1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

// ===========================================================================
// Test-data generators — a representative spread of compressibility profiles
// ===========================================================================

/// A run of `n` zero bytes: maximally compressible (mirrors the zero-filled
/// large-buffer cases in `test/example.c`'s `test_large_*`).
fn zeros(n: usize) -> Vec<u8> {
    vec![0u8; n]
}

/// `n` bytes of repeated natural-language text: moderately compressible, with
/// abundant back-references for the match finder to exploit.
fn repetitive(n: usize) -> Vec<u8> {
    const PHRASE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let take = (n - out.len()).min(PHRASE.len());
        out.extend_from_slice(&PHRASE[..take]);
    }
    out
}

/// `n` bytes of deterministic high-entropy pseudo-random data: effectively
/// incompressible, exercising the stored-block fallback.
///
/// A self-contained `xorshift64*` generator with a fixed seed is used instead
/// of the `rand` crate so the corpus is perfectly reproducible and the test has
/// no dependency on any external RNG API surface.
fn incompressible(n: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // xorshift64* — a well-distributed, fully deterministic sequence.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let mixed = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.push((mixed >> 24) as u8);
    }
    out
}

/// The representative input corpus shared by the interop tests: the canonical
/// literal, an empty buffer (streaming edge case), and the three
/// compressibility profiles at the `example.c` large-buffer size.
fn sample_inputs() -> Vec<Vec<u8>> {
    vec![
        HELLO.to_vec(),
        Vec::new(),
        zeros(LARGE_LEN),
        repetitive(LARGE_LEN / 2),
        incompressible(LARGE_LEN),
    ]
}

// ===========================================================================
// `zlib-rs` streaming helpers (black-box over the public API)
// ===========================================================================

/// Compress `data` with `zlib-rs` at `level`, `strategy`, and the framing
/// selected by `window_bits`, returning the exact compressed stream.
///
/// Drives the public streaming `deflate` engine to completion with a single
/// `Z_FINISH` pass, growing the destination only in the unlikely event a pass
/// reports `Ok` with the buffer full. `deflate_end` is called explicitly to
/// mirror the C `deflateEnd` contract (the owned engine state would also be
/// released on drop).
fn zlib_rs_deflate_strategy(
    data: &[u8],
    level: i32,
    window_bits: i32,
    strategy: Strategy,
) -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        window_bits,
        DEF_MEM_LEVEL,
        strategy,
    )
    .expect("deflate_init2 must succeed for valid parameters");

    // Size the destination with the zlib bound plus slack for a gzip
    // header/trailer; the loop still grows it if a pass needs more room.
    let mut output = vec![0u8; compress_bound(data.len()) + 128];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let outcome = deflate(&mut strm, &data[in_pos..], &mut output[out_pos..], Z_FINISH);
        in_pos += outcome.consumed;
        out_pos += outcome.produced;
        match outcome.code {
            ReturnCode::StreamEnd => break,
            ReturnCode::Ok => {
                if out_pos == output.len() {
                    // Ran out of room before finishing — grow and continue.
                    output.resize(output.len() * 2, 0);
                } else if outcome.consumed == 0 && outcome.produced == 0 {
                    panic!("zlib-rs deflate stalled with Z_FINISH before StreamEnd");
                }
            }
            other => panic!("zlib-rs deflate returned {other:?}"),
        }
    }

    deflate_end(&mut strm).expect("deflate_end must succeed");
    output.truncate(out_pos);
    output
}

/// Compress `data` with `zlib-rs` using the default strategy — the common case
/// for the decode-compatibility tests.
fn zlib_rs_deflate(data: &[u8], level: i32, window_bits: i32) -> Vec<u8> {
    zlib_rs_deflate_strategy(data, level, window_bits, Strategy::Default)
}

/// Decompress `data` with `zlib-rs` using the framing selected by
/// `window_bits`, returning the recovered bytes.
///
/// Drives the public streaming `inflate` engine, accumulating output into a
/// growable buffer via a reused fixed-size chunk, until `StreamEnd`. A stalled
/// pass (no progress without reaching the end) panics so a truncated or corrupt
/// stream fails the test loudly rather than hanging.
fn zlib_rs_inflate(data: &[u8], window_bits: i32) -> Vec<u8> {
    let mut strm = ZStream::new();
    inflate_init2(&mut strm, window_bits).expect("inflate_init2 must succeed");

    let mut output = Vec::new();
    let mut chunk = vec![0u8; 32 * 1024];
    let mut in_pos = 0usize;

    loop {
        let outcome = inflate(&mut strm, &data[in_pos..], &mut chunk, Z_NO_FLUSH);
        in_pos += outcome.consumed;
        output.extend_from_slice(&chunk[..outcome.produced]);
        match outcome.code {
            ReturnCode::StreamEnd => break,
            ReturnCode::Ok => {
                if outcome.consumed == 0 && outcome.produced == 0 {
                    panic!("zlib-rs inflate stalled before StreamEnd (truncated input?)");
                }
            }
            other => panic!("zlib-rs inflate returned {other:?}"),
        }
    }

    inflate_end(&mut strm).expect("inflate_end must succeed");
    output
}

// ===========================================================================
// `flate2` (miniz_oxide) helpers — the independent reference codec
// ===========================================================================

/// Compress `data` to a zlib (RFC 1950) stream with `flate2` at `level`.
fn flate2_compress_zlib(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).expect("flate2 zlib write_all");
    encoder.finish().expect("flate2 zlib finish")
}

/// Compress `data` to a raw DEFLATE (RFC 1951) stream with `flate2` at `level`.
fn flate2_compress_raw(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).expect("flate2 raw write_all");
    encoder.finish().expect("flate2 raw finish")
}

/// Decompress a `flate2`-encoded zlib (RFC 1950) stream.
fn flate2_decompress_zlib(data: &[u8]) -> Vec<u8> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("flate2 zlib read_to_end");
    out
}

/// Decompress a `flate2`-encoded raw DEFLATE (RFC 1951) stream.
fn flate2_decompress_raw(data: &[u8]) -> Vec<u8> {
    let mut decoder = DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("flate2 raw read_to_end");
    out
}

/// Compress `data` to a gzip (RFC 1952) stream with `flate2` at `level`.
#[cfg(feature = "gzip")]
fn flate2_compress_gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).expect("flate2 gzip write_all");
    encoder.finish().expect("flate2 gzip finish")
}

/// Decompress a `flate2`-encoded gzip (RFC 1952) stream.
#[cfg(feature = "gzip")]
fn flate2_decompress_gzip(data: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("flate2 gzip read_to_end");
    out
}

// ===========================================================================
// Tier 1 — decode-compatibility: `flate2` decodes what `zlib-rs` produced
// ===========================================================================

/// `flate2` must decode every zlib (RFC 1950) stream `zlib-rs` produces, at
/// every compression level and across the full input corpus.
#[test]
fn flate2_decodes_zlib_rs_zlib() {
    for input in sample_inputs() {
        for &level in &ZLIB_RS_LEVELS {
            let compressed = zlib_rs_deflate(&input, level, WBITS_ZLIB);
            let restored = flate2_decompress_zlib(&compressed);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "flate2 failed to decode zlib-rs zlib stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

/// `flate2` must decode every raw DEFLATE (RFC 1951) stream `zlib-rs` produces.
#[test]
fn flate2_decodes_zlib_rs_raw() {
    for input in sample_inputs() {
        for &level in &ZLIB_RS_LEVELS {
            let compressed = zlib_rs_deflate(&input, level, WBITS_RAW);
            let restored = flate2_decompress_raw(&compressed);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "flate2 failed to decode zlib-rs raw stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

/// `flate2` must decode every gzip (RFC 1952) stream `zlib-rs` produces,
/// validating `zlib-rs`'s gzip header/trailer emission.
#[cfg(feature = "gzip")]
#[test]
fn flate2_decodes_zlib_rs_gzip() {
    for input in sample_inputs() {
        for &level in &ZLIB_RS_LEVELS {
            let compressed = zlib_rs_deflate(&input, level, WBITS_GZIP);
            let restored = flate2_decompress_gzip(&compressed);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "flate2 failed to decode zlib-rs gzip stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

// ===========================================================================
// Tier 1 — decode-compatibility: `zlib-rs` decodes what `flate2` produced
// ===========================================================================

/// `zlib-rs` must decode every zlib (RFC 1950) stream `flate2` produces.
#[test]
fn zlib_rs_decodes_flate2_zlib() {
    for input in sample_inputs() {
        for level in 0u32..=9 {
            let compressed = flate2_compress_zlib(&input, level);
            let restored = zlib_rs_inflate(&compressed, WBITS_ZLIB);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "zlib-rs failed to decode flate2 zlib stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

/// `zlib-rs` must decode every raw DEFLATE (RFC 1951) stream `flate2` produces.
#[test]
fn zlib_rs_decodes_flate2_raw() {
    for input in sample_inputs() {
        for level in 0u32..=9 {
            let compressed = flate2_compress_raw(&input, level);
            let restored = zlib_rs_inflate(&compressed, WBITS_RAW);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "zlib-rs failed to decode flate2 raw stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

/// `zlib-rs` must decode every gzip (RFC 1952) stream `flate2` produces,
/// validating `zlib-rs`'s gzip header/trailer parsing against an independent
/// gzip writer.
#[cfg(feature = "gzip")]
#[test]
fn zlib_rs_decodes_flate2_gzip() {
    for input in sample_inputs() {
        for level in 0u32..=9 {
            let compressed = flate2_compress_gzip(&input, level);
            let restored = zlib_rs_inflate(&compressed, WBITS_GZIP);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "zlib-rs failed to decode flate2 gzip stream at level {level} (input len {})",
                input.len()
            );
        }
    }
}

// ===========================================================================
// Every deflate strategy produces a valid, interoperable wire format
// ===========================================================================

/// Each of the five deflate strategies (`Z_DEFAULT_STRATEGY`, `Z_FILTERED`,
/// `Z_HUFFMAN_ONLY`, `Z_RLE`, `Z_FIXED`) must emit a fully compliant DEFLATE
/// stream: `flate2` (an independent decoder) decodes it, and `zlib-rs`
/// round-trips its own output.
///
/// Per-strategy *byte-identity* against a reference is intentionally not
/// asserted here — `flate2`'s high-level API does not expose a strategy knob,
/// so there is no strategy-matched reference encoder under the default backend.
/// Decode-compatibility plus self round-trip is the robust, always-on proof
/// that every strategy's wire format is correct.
#[test]
fn all_strategies_wire_format_compatible() {
    let strategies = [
        Strategy::Default,
        Strategy::Filtered,
        Strategy::HuffmanOnly,
        Strategy::Rle,
        Strategy::Fixed,
    ];
    let inputs = [
        HELLO.to_vec(),
        repetitive(4096),
        zeros(4096),
        incompressible(4096),
    ];

    for strategy in strategies {
        for input in &inputs {
            // zlib framing at the default level, using this strategy.
            let compressed = zlib_rs_deflate_strategy(input, 6, WBITS_ZLIB, strategy);

            // An independent decoder must accept the stream.
            let via_flate2 = flate2_decompress_zlib(&compressed);
            assert_eq!(
                via_flate2.as_slice(),
                input.as_slice(),
                "flate2 failed to decode zlib-rs {strategy:?} stream (input len {})",
                input.len()
            );

            // And zlib-rs must decode its own strategy-specific output.
            let via_zlib_rs = zlib_rs_inflate(&compressed, WBITS_ZLIB);
            assert_eq!(
                via_zlib_rs.as_slice(),
                input.as_slice(),
                "zlib-rs failed to round-trip {strategy:?} stream (input len {})",
                input.len()
            );
        }
    }
}

// ===========================================================================
// One-call crate-root re-exports (`compress2` / `compress_bound` / `uncompress`)
// ===========================================================================

/// Exercise the idiomatic one-call re-exports from the crate root and
/// cross-validate them with `flate2` in both directions. This binds directly to
/// `compress2`, `compress_bound`, and `uncompress` as re-exported by
/// `src/lib.rs`, confirming the `compressBound` sizing contract (AAP §0.6.4) is
/// sufficient for the emitted stream.
#[test]
fn one_call_reexports_cross_validate() {
    for input in sample_inputs() {
        // Compress with `compress2` at best level, sized via `compress_bound`.
        let mut compressed = vec![0u8; compress_bound(input.len())];
        let produced = compress2(&mut compressed, &input, 9).expect("compress2 must succeed");
        assert!(
            produced <= compressed.len(),
            "compress2 output ({produced}) overran compress_bound ({})",
            compressed.len()
        );
        compressed.truncate(produced);

        // The reference decoder accepts the one-call zlib stream.
        let via_flate2 = flate2_decompress_zlib(&compressed);
        assert_eq!(via_flate2.as_slice(), input.as_slice());

        // The one-call `uncompress` decodes a flate2-produced zlib stream. The
        // destination is sized to the known original length (at least one byte,
        // so an empty payload still has valid output room).
        let flate2_stream = flate2_compress_zlib(&input, 6);
        let mut restored = vec![0u8; input.len().max(1)];
        let written = uncompress(&mut restored, &flate2_stream).expect("uncompress must succeed");
        restored.truncate(written);
        assert_eq!(restored.as_slice(), input.as_slice());
    }
}

// ===========================================================================
// Auto-detect windowBits (40..=47) transparently accepts zlib and gzip wrappers
// ===========================================================================

/// `windowBits = 47` (auto-detect) must transparently decode a zlib (RFC 1950)
/// wrapper — the inflate-only auto-detection path of the overloading contract.
#[test]
fn auto_detect_accepts_zlib() {
    for input in sample_inputs() {
        let zlib_stream = zlib_rs_deflate(&input, 6, WBITS_ZLIB);
        let restored = zlib_rs_inflate(&zlib_stream, WBITS_AUTO);
        assert_eq!(
            restored.as_slice(),
            input.as_slice(),
            "auto-detect failed on a zlib stream (input len {})",
            input.len()
        );
    }
}

/// `windowBits = 47` (auto-detect) must also transparently decode a gzip
/// (RFC 1952) wrapper.
#[cfg(feature = "gzip")]
#[test]
fn auto_detect_accepts_gzip() {
    for input in sample_inputs() {
        let gzip_stream = zlib_rs_deflate(&input, 6, WBITS_GZIP);
        let restored = zlib_rs_inflate(&gzip_stream, WBITS_AUTO);
        assert_eq!(
            restored.as_slice(),
            input.as_slice(),
            "auto-detect failed on a gzip stream (input len {})",
            input.len()
        );
    }
}

// ===========================================================================
// `zlib-rs` self round-trip across framings and levels (streaming-API sanity)
// ===========================================================================

/// `zlib-rs` must round-trip its own zlib and raw output at every level — a
/// direct exercise of the streaming `deflate`/`inflate` drivers used by the
/// cross-decode tests.
#[test]
fn zlib_rs_self_roundtrip_zlib_and_raw() {
    for input in sample_inputs() {
        for &(window_bits, name) in &[(WBITS_ZLIB, "zlib"), (WBITS_RAW, "raw")] {
            for &level in &ZLIB_RS_LEVELS {
                let compressed = zlib_rs_deflate(&input, level, window_bits);
                let restored = zlib_rs_inflate(&compressed, window_bits);
                assert_eq!(
                    restored.as_slice(),
                    input.as_slice(),
                    "self round-trip failed: {name} framing, level {level} (input len {})",
                    input.len()
                );
            }
        }
    }
}

/// `zlib-rs` must round-trip its own gzip output at every level.
#[cfg(feature = "gzip")]
#[test]
fn zlib_rs_self_roundtrip_gzip() {
    for input in sample_inputs() {
        for &level in &ZLIB_RS_LEVELS {
            let compressed = zlib_rs_deflate(&input, level, WBITS_GZIP);
            let restored = zlib_rs_inflate(&compressed, WBITS_GZIP);
            assert_eq!(
                restored.as_slice(),
                input.as_slice(),
                "gzip self round-trip failed at level {level} (input len {})",
                input.len()
            );
        }
    }
}

// ===========================================================================
// Tier 2 — OPT-IN strict byte-identity against a GENUINE zlib reference
// ===========================================================================

/// Strict byte-for-byte equality of the compressed stream against a *real* zlib
/// encoder.
///
/// This module is compiled ONLY under `--features real-zlib-interop`, an opt-in
/// switch enabled together with a genuine zlib backend for `flate2`
/// (`flate2/zlib` or `flate2/zlib-ng`) for "bug-for-bug interop verification"
/// (AAP §0.5.2). It is deliberately absent from the default `miniz_oxide` run,
/// where `flate2`'s different match-finding heuristics make a byte mismatch
/// expected and benign. See the file-level `#![allow(unexpected_cfgs)]` note.
#[cfg(feature = "real-zlib-interop")]
mod strict_byte_identity {
    use super::*;

    /// Reference zlib compression. Byte-identical to `zlib-rs` **only** when
    /// `flate2` is built against a real zlib backend, not `miniz_oxide`.
    fn real_zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
        flate2_compress_zlib(data, level)
    }

    /// For the default strategy, `zlib-rs`'s compressed output must be
    /// byte-identical to the reference zlib encoder at every level.
    #[test]
    fn zlib_rs_matches_real_zlib_default_strategy() {
        let inputs = [HELLO.to_vec(), repetitive(8192), zeros(8192)];
        for input in &inputs {
            for level in 1u32..=9 {
                let ours = zlib_rs_deflate(input, level as i32, WBITS_ZLIB);
                let reference = real_zlib_compress(input, level);
                assert_eq!(
                    ours,
                    reference,
                    "byte-identity mismatch vs real zlib at level {level} (input len {})",
                    input.len()
                );
            }
        }
    }
}
