//! Byte-identity and wire-format cross-validation for `zlib-rs`.
//!
//! This integration test is the authoritative **byte-identity and wire-format
//! compatibility** gate for `zlib-rs` (AAP §0.6.4 / §0.6.7 / §0.7.2 and user
//! rule R1). It proves two distinct, complementary properties, in two tiers:
//!
//! # Testing strategy
//!
//! 1. **Strict byte-identity (always-on, release gate).** The crate's compressed
//!    output must be **byte-for-byte identical** to the reference C zlib
//!    (`1.3.2.1-motley`) for the same input, level, strategy, and framing — the
//!    defining acceptance criterion of the migration (R1). This is enforced by
//!    the [`byte_identity`] module below against **deterministic oracle
//!    vectors** baked from the genuine C encoder (via `deflateInit2` +
//!    `deflate(Z_FINISH)`, `memLevel = 8`). The vectors span every compression
//!    level (`-1..=9`), all five deflate strategies, and the zlib / raw / gzip /
//!    small-window framings. Because the reference bytes are precomputed
//!    constants, this gate runs **by default in CI with no C toolchain**.
//!
//! 2. **Decode-compatibility (always-on).** The crate's zlib (RFC 1950), raw
//!    DEFLATE (RFC 1951), and gzip (RFC 1952) streams must interoperate with an
//!    independently-authored codec — the dev-dependency [`flate2`], built with
//!    its **default `miniz_oxide` backend** (pure Rust, no C toolchain). Both
//!    directions are asserted for every framing (`flate2` decodes what `zlib-rs`
//!    produced, and `zlib-rs` decodes what `flate2` produced), across every
//!    level and strategy. Because `miniz_oxide` is a *different* encoder with
//!    different match-finding heuristics, these tests prove RFC wire-format
//!    conformance but are **not** treated as satisfying byte-identity; that
//!    property is proven exclusively by tier 1.
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
///
/// Auto-detection lives in the `40..=47` windowBits range, which the inflate
/// engine only accepts when gzip support is compiled in (`inflate_reset2`
/// masks the request with `& 15` under `#[cfg(feature = "gzip")]`; without it
/// the value fails the `8..=15` bounds test, matching C built without
/// `GUNZIP`). The constant — and the tests that use it — are therefore gated
/// on the `gzip` feature.
#[cfg(feature = "gzip")]
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
///
/// Auto-detect requires gzip framing support (the `40..=47` windowBits range
/// is only accepted when the `gzip` feature is enabled), so this test is
/// gated to match.
#[cfg(feature = "gzip")]
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
// Tier 1 — strict byte-identity vs GENUINE C zlib 1.3.2.1-motley
// (deterministic oracle vectors; always-on release gate for user rule R1)
// ===========================================================================

/// Strict byte-for-byte equality of `zlib-rs`'s compressed output against the
/// genuine C zlib `1.3.2.1-motley` encoder — the defining acceptance criterion
/// of the migration (AAP §0.6.4 / §0.7.2, user rule R1).
///
/// The oracle vectors below were produced by the reference C library via
/// `deflateInit2` + `deflate(Z_FINISH)` + `deflateEnd` (method `Z_DEFLATED`,
/// `memLevel = 8`), mirroring [`zlib_rs_deflate_strategy`] exactly. Both the
/// input corpus ([`BI_INPUTS`]) and the expected compressed bytes
/// ([`BI_VECTORS`] / [`BI_VECTORS_GZIP`]) are baked as hex in compact
/// whitespace-delimited tables, so this gate runs by default in CI with **no C
/// toolchain** and no cross-language input-reproduction risk. The matrix spans
/// every compression level (`-1..=9`), all five deflate strategies, and the
/// zlib / raw / gzip / small-window framings.
///
/// Regenerate with the byte-identity oracle generator against the exact target
/// zlib version whenever the reference is bumped.
mod byte_identity {
    use super::*;

    /// Decode an even-length lowercase hex string to bytes.
    fn unhex(s: &str) -> Vec<u8> {
        assert!(s.len() % 2 == 0, "odd-length hex string: {s:?}");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex byte"))
            .collect()
    }

    /// Map a strategy id (identical for the C `Z_*` values and the `zlib-rs`
    /// [`Strategy`] discriminants) to the enum variant.
    fn strat_from_id(id: u8) -> Strategy {
        match id {
            0 => Strategy::Default,
            1 => Strategy::Filtered,
            2 => Strategy::HuffmanOnly,
            3 => Strategy::Rle,
            4 => Strategy::Fixed,
            other => panic!("unknown strategy id {other}"),
        }
    }

    const BI_INPUTS: &str = "\
      # empty buffer (streaming edge case)
    68656c6c6f2c2068656c6c6f21  # the canonical example.c literal
    54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220  # repetitive natural-language text (256 B, long matches)
    000000000000000000000000000000000101010101010101010101010101010102020202020202020202020202020202030303030303030303030303030303030404040404040404040404040404040405050505050505050505050505050505060606060606060606060606060606060707070707070707070707070707070708080808080808080808080808080808090909090909090909090909090909090a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f  # byte-run pattern (256 B, RLE/short-match paths)
    9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186  # deterministic pseudo-random data (128 B, incompressible)
    ";

    const BI_VECTORS: &str = "\
    0 -1 15 0 789c030000000001
    0 0 15 0 7801010000ffff00000001
    0 1 15 0 7801030000000001
    0 2 15 0 785e030000000001
    0 3 15 0 785e030000000001
    0 4 15 0 785e030000000001
    0 5 15 0 785e030000000001
    0 6 15 0 789c030000000001
    0 7 15 0 78da030000000001
    0 8 15 0 78da030000000001
    0 9 15 0 78da030000000001
    0 -1 -15 0 0300
    0 0 -15 0 010000ffff
    0 1 -15 0 0300
    0 2 -15 0 0300
    0 3 -15 0 0300
    0 4 -15 0 0300
    0 5 -15 0 0300
    0 6 -15 0 0300
    0 7 -15 0 0300
    0 8 -15 0 0300
    0 9 -15 0 0300
    0 -1 9 0 1895030000000001
    0 0 9 0 1819010000ffff00000001
    0 1 9 0 1819030000000001
    0 2 9 0 1857030000000001
    0 3 9 0 1857030000000001
    0 4 9 0 1857030000000001
    0 5 9 0 1857030000000001
    0 6 9 0 1895030000000001
    0 7 9 0 18d3030000000001
    0 8 9 0 18d3030000000001
    0 9 9 0 18d3030000000001
    0 6 15 1 789c030000000001
    0 6 15 2 7801030000000001
    0 6 15 3 7801030000000001
    0 6 15 4 7801030000000001
    0 6 -15 1 0300
    0 6 -15 2 0300
    0 6 -15 3 0300
    0 6 -15 4 0300
    0 6 9 1 1895030000000001
    0 6 9 2 1819030000000001
    0 6 9 3 1819030000000001
    0 6 9 4 1819030000000001
    1 -1 15 0 789ccb48cdc9c9d751c800518a0021700496
    1 0 15 0 7801010d00f2ff68656c6c6f2c2068656c6c6f2121700496
    1 1 15 0 7801cb48cdc9c9d751c800518a0021700496
    1 2 15 0 785ecb48cdc9c9d751c800518a0021700496
    1 3 15 0 785ecb48cdc9c9d751c800518a0021700496
    1 4 15 0 785ecb48cdc9c9d751c800518a0021700496
    1 5 15 0 785ecb48cdc9c9d751c800518a0021700496
    1 6 15 0 789ccb48cdc9c9d751c800518a0021700496
    1 7 15 0 78dacb48cdc9c9d751c800518a0021700496
    1 8 15 0 78dacb48cdc9c9d751c800518a0021700496
    1 9 15 0 78dacb48cdc9c9d751c800518a0021700496
    1 -1 -15 0 cb48cdc9c9d751c800518a00
    1 0 -15 0 010d00f2ff68656c6c6f2c2068656c6c6f21
    1 1 -15 0 cb48cdc9c9d751c800518a00
    1 2 -15 0 cb48cdc9c9d751c800518a00
    1 3 -15 0 cb48cdc9c9d751c800518a00
    1 4 -15 0 cb48cdc9c9d751c800518a00
    1 5 -15 0 cb48cdc9c9d751c800518a00
    1 6 -15 0 cb48cdc9c9d751c800518a00
    1 7 -15 0 cb48cdc9c9d751c800518a00
    1 8 -15 0 cb48cdc9c9d751c800518a00
    1 9 -15 0 cb48cdc9c9d751c800518a00
    1 -1 9 0 1895cb48cdc9c9d751c800518a0021700496
    1 0 9 0 1819010d00f2ff68656c6c6f2c2068656c6c6f2121700496
    1 1 9 0 1819cb48cdc9c9d751c800518a0021700496
    1 2 9 0 1857cb48cdc9c9d751c800518a0021700496
    1 3 9 0 1857cb48cdc9c9d751c800518a0021700496
    1 4 9 0 1857cb48cdc9c9d751c800518a0021700496
    1 5 9 0 1857cb48cdc9c9d751c800518a0021700496
    1 6 9 0 1895cb48cdc9c9d751c800518a0021700496
    1 7 9 0 18d3cb48cdc9c9d751c800518a0021700496
    1 8 9 0 18d3cb48cdc9c9d751c800518a0021700496
    1 9 9 0 18d3cb48cdc9c9d751c800518a0021700496
    1 6 15 1 789ccb48cdc9c9d751c848cdc9c957040021700496
    1 6 15 2 7801cb48cdc9c9d751c848cdc9c957040021700496
    1 6 15 3 7801cb48cdc9c9d751c848cdc9c957040021700496
    1 6 15 4 7801cb48cdc9c9d751c800518a0021700496
    1 6 -15 1 cb48cdc9c9d751c848cdc9c9570400
    1 6 -15 2 cb48cdc9c9d751c848cdc9c9570400
    1 6 -15 3 cb48cdc9c9d751c848cdc9c9570400
    1 6 -15 4 cb48cdc9c9d751c800518a00
    1 6 9 1 1895cb48cdc9c9d751c848cdc9c957040021700496
    1 6 9 2 1819cb48cdc9c9d751c848cdc9c957040021700496
    1 6 9 3 1819cb48cdc9c9d751c848cdc9c957040021700496
    1 6 9 4 1819cb48cdc9c9d751c800518a0021700496
    2 -1 15 0 789c0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 0 15 0 7801010001fffe54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f7665722050fc5c22
    2 1 15 0 78010bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 2 15 0 785e0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 3 15 0 785e0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 4 15 0 785e0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 5 15 0 785e0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 6 15 0 789c0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 7 15 0 78da0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 8 15 0 78da0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 9 15 0 78da0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 -1 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 0 -15 0 010001fffe54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220
    2 1 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 2 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 3 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 4 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 5 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 6 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 7 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 8 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 9 -15 0 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 -1 9 0 18950bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 0 9 0 1819010001fffe54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f7665722050fc5c22
    2 1 9 0 18190bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 2 9 0 18570bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 3 9 0 18570bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 4 9 0 18570bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 5 9 0 18570bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 6 9 0 18950bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 7 9 0 18d30bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 8 9 0 18d30bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 9 9 0 18d30bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 6 15 1 789c0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228c94855c849acaa5448c94fd75308197e8a0150fc5c22
    2 6 15 2 780105c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f50fc5c22
    2 6 15 3 780105c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f50fc5c22
    2 6 15 4 78010bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    2 6 -15 1 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228c94855c849acaa5448c94fd75308197e8a01
    2 6 -15 2 05c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f
    2 6 -15 3 05c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f
    2 6 -15 4 0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500
    2 6 9 1 18950bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228c94855c849acaa5448c94fd75308197e8a0150fc5c22
    2 6 9 2 181905c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f50fc5c22
    2 6 9 3 181905c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60f50fc5c22
    2 6 9 4 18190bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc50050fc5c22
    3 -1 15 0 789c5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 0 15 0 7801010001fffe000000000000000000000000000000000101010101010101010101010101010102020202020202020202020202020202030303030303030303030303030303030404040404040404040404040404040405050505050505050505050505050505060606060606060606060606060606060707070707070707070707070707070708080808080808080808080808080808090909090909090909090909090909090a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f70de0781
    3 1 15 0 78015dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 2 15 0 785e5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 3 15 0 785e5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 4 15 0 785e5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 5 15 0 785e5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 6 15 0 789c5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 7 15 0 78da5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 8 15 0 78da5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 9 15 0 78da5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 -1 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 0 -15 0 010001fffe000000000000000000000000000000000101010101010101010101010101010102020202020202020202020202020202030303030303030303030303030303030404040404040404040404040404040405050505050505050505050505050505060606060606060606060606060606060707070707070707070707070707070708080808080808080808080808080808090909090909090909090909090909090a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f
    3 1 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 2 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 3 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 4 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 5 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 6 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 7 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 8 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 9 -15 0 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 -1 9 0 18955dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 0 9 0 1819010001fffe000000000000000000000000000000000101010101010101010101010101010102020202020202020202020202020202030303030303030303030303030303030404040404040404040404040404040405050505050505050505050505050505060606060606060606060606060606060707070707070707070707070707070708080808080808080808080808080808090909090909090909090909090909090a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f70de0781
    3 1 9 0 18195dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 2 9 0 18575dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 3 9 0 18575dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 4 9 0 18575dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 5 9 0 18575dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 6 9 0 18955dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 7 9 0 18d35dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 8 9 0 18d35dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 9 9 0 18d35dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 6 15 1 789c5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 6 15 2 780105c187010020080020776effff3600000000000000004044444444444444242222222222222262666666666666661611111111111111515555555555555535333333333333337befbdf7de7befbdf7de73777777777777778f88888888888888c8ccccccccccccccacaaaaaaaaaaaaaaeaeeeeeeeeeeeeee9e99999999999999d9ddddddddddddddbdbbbbbbbbbbbbbbfb70de0781
    3 6 15 3 78015dc1b701c0200000207b37f9ff5b7720202221a3a0a2a163606261e3e0e2c38f0770de0781
    3 6 15 4 7801636040058c6880090d30a3011634c08a06d8d0003b1ae040039c68800b0d70a3011e34c08b06f8d0003f1a000070de0781
    3 6 -15 1 5dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e
    3 6 -15 2 05c187010020080020776effff3600000000000000004044444444444444242222222222222262666666666666661611111111111111515555555555555535333333333333337befbdf7de7befbdf7de73777777777777778f88888888888888c8ccccccccccccccacaaaaaaaaaaaaaaeaeeeeeeeeeeeeee9e99999999999999d9ddddddddddddddbdbbbbbbbbbbbbbbfb
    3 6 -15 3 5dc1b701c0200000207b37f9ff5b7720202221a3a0a2a163606261e3e0e2c38f07
    3 6 -15 4 636040058c6880090d30a3011634c08a06d8d0003b1ae040039c68800b0d70a3011e34c08b06f8d0003f1a0000
    3 6 9 1 18955dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7e70de0781
    3 6 9 2 181905c187010020080020776effff3600000000000000004044444444444444242222222222222262666666666666661611111111111111515555555555555535333333333333337befbdf7de7befbdf7de73777777777777778f88888888888888c8ccccccccccccccacaaaaaaaaaaaaaaeaeeeeeeeeeeeeee9e99999999999999d9ddddddddddddddbdbbbbbbbbbbbbbbfb70de0781
    3 6 9 3 18195dc1b701c0200000207b37f9ff5b7720202221a3a0a2a163606261e3e0e2c38f0770de0781
    3 6 9 4 1819636040058c6880090d30a3011634c08a06d8d0003b1ae040039c68800b0d70a3011e34c08b06f8d0003f1a000070de0781
    4 -1 15 0 789c0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 0 15 0 78010180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 1 15 0 78010180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 2 15 0 785e0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 3 15 0 785e0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 4 15 0 785e0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 5 15 0 785e0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 15 0 789c0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 7 15 0 78da0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 8 15 0 78da0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 9 15 0 78da0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 -1 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 0 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 1 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 2 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 3 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 4 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 5 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 6 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 7 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 8 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 9 -15 0 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 -1 9 0 18950180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 0 9 0 18190180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 1 9 0 18190180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 2 9 0 18570180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 3 9 0 18570180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 4 9 0 18570180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 5 9 0 18570180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 9 0 18950180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 7 9 0 18d30180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 8 9 0 18d30180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 9 9 0 18d30180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 15 1 789c0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 15 2 78010180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 15 3 78010180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 15 4 78010180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 -15 1 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 6 -15 2 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 6 -15 3 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 6 -15 4 0180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186
    4 6 9 1 18950180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 9 2 18190180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 9 3 18190180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    4 6 9 4 18190180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f18661334005
    ";

    #[cfg(feature = "gzip")]
    const BI_VECTORS_GZIP: &str = "\
    0 -1 31 0 1f8b080000000000000303000000000000000000
    0 0 31 0 1f8b0800000000000403010000ffff0000000000000000
    0 1 31 0 1f8b080000000000040303000000000000000000
    0 2 31 0 1f8b080000000000000303000000000000000000
    0 3 31 0 1f8b080000000000000303000000000000000000
    0 4 31 0 1f8b080000000000000303000000000000000000
    0 5 31 0 1f8b080000000000000303000000000000000000
    0 6 31 0 1f8b080000000000000303000000000000000000
    0 7 31 0 1f8b080000000000000303000000000000000000
    0 8 31 0 1f8b080000000000000303000000000000000000
    0 9 31 0 1f8b080000000000020303000000000000000000
    0 6 31 1 1f8b080000000000000303000000000000000000
    0 6 31 2 1f8b080000000000040303000000000000000000
    0 6 31 3 1f8b080000000000040303000000000000000000
    0 6 31 4 1f8b080000000000040303000000000000000000
    1 -1 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 0 31 0 1f8b0800000000000403010d00f2ff68656c6c6f2c2068656c6c6f219bdc9ab30d000000
    1 1 31 0 1f8b0800000000000403cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 2 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 3 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 4 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 5 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 6 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 7 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 8 31 0 1f8b0800000000000003cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 9 31 0 1f8b0800000000000203cb48cdc9c9d751c800518a009bdc9ab30d000000
    1 6 31 1 1f8b0800000000000003cb48cdc9c9d751c848cdc9c95704009bdc9ab30d000000
    1 6 31 2 1f8b0800000000000403cb48cdc9c9d751c848cdc9c95704009bdc9ab30d000000
    1 6 31 3 1f8b0800000000000403cb48cdc9c9d751c848cdc9c95704009bdc9ab30d000000
    1 6 31 4 1f8b0800000000000403cb48cdc9c9d751c800518a009bdc9ab30d000000
    2 -1 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 0 31 0 1f8b0800000000000403010001fffe54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f672e2054686520717569636b2062726f776e20666f78206a756d7073206f76657220aff02d1b00010000
    2 1 31 0 1f8b08000000000004030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 2 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 3 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 4 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 5 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 6 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 7 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 8 31 0 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 9 31 0 1f8b08000000000002030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    2 6 31 1 1f8b08000000000000030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228c94855c849acaa5448c94fd75308197e8a01aff02d1b00010000
    2 6 31 2 1f8b080000000000040305c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60faff02d1b00010000
    2 6 31 3 1f8b080000000000040305c1890180200c00b1556e02a761019f2a3e50a916d1e94d42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac61d85a3ff5e265d3a42148aafe3ce60fa64666d6c9ece0bad62dc5138faef65d2a52344a1f83aee0ca64f66d6c6e6e9bcd02ac60faff02d1b00010000
    2 6 31 4 1f8b08000000000004030bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29840c3fc500aff02d1b00010000
    3 -1 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 0 31 0 1f8b0800000000000403010001fffe000000000000000000000000000000000101010101010101010101010101010102020202020202020202020202020202030303030303030303030303030303030404040404040404040404040404040405050505050505050505050505050505060606060606060606060606060606060707070707070707070707070707070708080808080808080808080808080808090909090909090909090909090909090a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0fddd8be2200010000
    3 1 31 0 1f8b08000000000004035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 2 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 3 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 4 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 5 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 6 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 7 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 8 31 0 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 9 31 0 1f8b08000000000002035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 6 31 1 1f8b08000000000000035dc1c901c010000030a56e65ff6dfb962484db838884171905150d1d03130b1f360e7eddd8be2200010000
    3 6 31 2 1f8b080000000000040305c187010020080020776effff3600000000000000004044444444444444242222222222222262666666666666661611111111111111515555555555555535333333333333337befbdf7de7befbdf7de73777777777777778f88888888888888c8ccccccccccccccacaaaaaaaaaaaaaaeaeeeeeeeeeeeeee9e99999999999999d9ddddddddddddddbdbbbbbbbbbbbbbbfbddd8be2200010000
    3 6 31 3 1f8b08000000000004035dc1b701c0200000207b37f9ff5b7720202221a3a0a2a163606261e3e0e2c38f07ddd8be2200010000
    3 6 31 4 1f8b0800000000000403636040058c6880090d30a3011634c08a06d8d0003b1ae040039c68800b0d70a3011e34c08b06f8d0003f1a0000ddd8be2200010000
    4 -1 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 0 31 0 1f8b08000000000004030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 1 31 0 1f8b08000000000004030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 2 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 3 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 4 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 5 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 6 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 7 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 8 31 0 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 9 31 0 1f8b08000000000002030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 6 31 1 1f8b08000000000000030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 6 31 2 1f8b08000000000004030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 6 31 3 1f8b08000000000004030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    4 6 31 4 1f8b08000000000004030180007fff9af1008aa16d4009af18aed8b47fe55826740d5b18a0333d822483ef7c795f816b2f319474121bc4f695d4dc7a57581d900aed323378e83cbac64e8e8b46fd0606f7971cdeca95a6f736740589a2c13e1bbe5be5f84cff654babb7efbab0a330a2b3443f7b5e24e3066861e68dff1fee5b65f54dc2eb10869b795eeea503f186f920fde580000000
    ";

    /// Parse [`BI_INPUTS`] into the ordered input corpus (one hex string per
    /// line; text after `#` is a descriptive comment, and the empty-hex line is
    /// the empty input).
    fn parse_inputs() -> Vec<Vec<u8>> {
        BI_INPUTS
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| unhex(l.split('#').next().unwrap().trim()))
            .collect()
    }

    /// Assert every vector line in `table` reproduces the reference bytes
    /// exactly. Line format: `<input-index> <level> <windowBits> <strategy-id>
    /// <hex-expected>`.
    fn check_all(table: &str) {
        let inputs = parse_inputs();
        let mut count = 0usize;
        for line in table.lines().filter(|l| !l.trim().is_empty()) {
            let mut f = line.split_whitespace();
            let idx: usize = f.next().unwrap().parse().unwrap();
            let level: i32 = f.next().unwrap().parse().unwrap();
            let wbits: i32 = f.next().unwrap().parse().unwrap();
            let strat: u8 = f.next().unwrap().parse().unwrap();
            let expected = unhex(f.next().unwrap());
            assert!(
                f.next().is_none(),
                "unexpected trailing field in vector line: {line:?}"
            );
            let input = &inputs[idx];
            let produced = zlib_rs_deflate_strategy(input, level, wbits, strat_from_id(strat));
            assert_eq!(
                produced,
                expected,
                "byte-identity mismatch vs C zlib 1.3.2.1-motley: input #{idx} \
                 (len {}), level {level}, windowBits {wbits}, strategy {:?}",
                input.len(),
                strat_from_id(strat),
            );
            count += 1;
        }
        assert!(count > 0, "no oracle vectors were parsed from the table");
    }

    /// zlib / raw / small-window framings across every level and strategy.
    #[test]
    fn matches_reference_zlib_plain_framings() {
        check_all(BI_VECTORS);
    }

    /// gzip framing (RFC 1952) across every level and strategy. Gated on the
    /// `gzip` feature, which is required to emit a gzip wrapper.
    #[cfg(feature = "gzip")]
    #[test]
    fn matches_reference_zlib_gzip_framing() {
        check_all(BI_VECTORS_GZIP);
    }
}
