//! Round-trip integration tests: `uncompress(compress(x)) == x`.
//!
//! This is a standalone Cargo **integration test** (a separate crate that links
//! `zlib-rs` and exercises only its *public* API). It proves the library's
//! central correctness invariant — every input, once compressed, decompresses
//! back to exactly the original bytes — across the full configuration matrix the
//! DEFLATE/zlib/gzip formats expose:
//!
//! * **compression levels** `0..=9` and the `-1` default ([`Z_DEFAULT_COMPRESSION`]);
//! * the four **strategies** ([`Strategy::Default`], [`Strategy::Filtered`],
//!   [`Strategy::HuffmanOnly`], [`Strategy::Rle`], [`Strategy::Fixed`]); and
//! * the **`windowBits`** wrappers — `15` (zlib / RFC 1950), `-15` (raw DEFLATE /
//!   RFC 1951), and `31` (gzip / RFC 1952, `= 15 + 16`).
//!
//! Two complementary surfaces are tested:
//!
//! 1. The **one-shot helpers** ([`compress2`] / [`uncompress`] /
//!    [`compress_bound`]) — the most stable public API — drive the deterministic
//!    edge-case sweep (Phase C) and the [`quickcheck`]-based property fuzzing
//!    (Phase B). They always emit a *zlib*-wrapped stream at a chosen level.
//! 2. The **streaming engine** ([`deflate_init2`] + [`deflate`] on one side,
//!    [`InflateState::new`] + [`InflateState::inflate`] on the other) — the only
//!    way to select a *strategy* and a raw/gzip wrapper — drives the
//!    levels × strategies × `windowBits` matrix (Phase D).
//!
//! # Critical API contracts honored here
//!
//! * **Tuple-order divergence.** [`deflate`] returns `(result, consumed,
//!   produced)` while [`InflateState::inflate`] reports `consumed` / `produced`
//!   / `status` (in that field order on [`InflateResult`]). The two helpers
//!   below destructure each correctly; mixing them would silently corrupt every
//!   round-trip.
//! * **Matching `windowBits`.** A stream produced with `-15` (raw) must be
//!   inflated with `-15`; one produced with `31` (gzip) must be inflated with
//!   `31`. This file never relies on auto-detection (`47`) — that path is
//!   covered by the interop / coverage suites.
//! * **Sizing.** Compressed output is *never* sized as `data.len()`. It is sized
//!   with [`compress_bound`] (`compress_bound(0) == 13`), with a small margin on
//!   the engine path to absorb the larger gzip header/trailer; both helpers grow
//!   defensively if the engine ever needs more room.
//!
//! # Feature gating
//!
//! * The gzip wrapper (`windowBits = 31`) needs the crate's `gzip` feature; with
//!   it disabled both engines reject `windowBits >= 24`. The single
//!   [`window_bits_variants`] helper therefore includes `31` only under
//!   `#[cfg(feature = "gzip")]`, so the matrix and properties compile and pass
//!   unchanged with or without gzip.
//! * The property tests use the `quickcheck` and `rand` dev-dependencies, which
//!   require the standard library, so they live in a `#[cfg(feature = "std")]`
//!   module. Both validation configurations (`default` and
//!   `--no-default-features --features std`) keep `std` on, so coverage is not
//!   reduced; a pure `no_std` build simply omits them.
//!
//! Byte-for-byte equivalence with C zlib is asserted separately in `interop.rs`;
//! wire-format compliance in `gzip_compat.rs` / `regression.rs`. This file proves
//! the round-trip invariant generically. It is safe Rust only and uses the
//! standard libtest harness (no `harness = false`).

use zlib_rs::constants::{
    DEF_MEM_LEVEL, Flush, Strategy, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION,
    Z_DEFLATED, Z_NO_COMPRESSION,
};
use zlib_rs::deflate::{deflate, deflate_init2};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::InflateState;
use zlib_rs::stream::ZStream;
use zlib_rs::util::{compress_bound, compress2, uncompress};

// ===========================================================================
// Shared fixtures
// ===========================================================================

/// Every compression level the round-trip invariant must hold for: the `-1`
/// default plus the full `0..=9` range. Spelled with the named level constants
/// where they apply so the public level API is exercised directly.
const ALL_LEVELS: [i32; 11] = [
    Z_DEFAULT_COMPRESSION, // -1 (library default, resolves to 6)
    Z_NO_COMPRESSION,      // 0  (stored blocks only)
    Z_BEST_SPEED,          // 1
    2,
    3,
    4,
    5,
    6,
    7,
    8,
    Z_BEST_COMPRESSION, // 9
];

/// The five DEFLATE compression strategies. Strategy never affects format
/// correctness — only the match/Huffman trade-off — so every one must round-trip
/// identically.
const STRATEGIES: [Strategy; 5] = [
    Strategy::Default,
    Strategy::Filtered,
    Strategy::HuffmanOnly,
    Strategy::Rle,
    Strategy::Fixed,
];

/// The set of `windowBits` wrappers exercised by the engine matrix and the
/// windowBits property: `15` (zlib) and `-15` (raw) are always present; `31`
/// (gzip) is included only when the `gzip` feature is enabled, because both
/// engines reject `windowBits >= 24` without it.
fn window_bits_variants() -> Vec<i32> {
    // `unused_mut` fires when the gzip `push` below is compiled out.
    #[allow(unused_mut)]
    let mut variants = vec![15, -15];
    #[cfg(feature = "gzip")]
    variants.push(31);
    variants
}

/// The deterministic edge-case corpus mandated by the folder requirement and the
/// agent prompt: empty input, a single byte, highly repetitive data (long LZ77
/// matches and RLE), short repeating patterns, realistic ASCII text (including
/// the suite's `b"hello, hello!"`), and an all-zero buffer. Random/incompressible
/// data is generated separately (it needs `rand`, see the `prop_tests` module).
///
/// Each entry is `(name, bytes)`; the name is woven into assertion messages so a
/// failure pinpoints the offending input immediately.
fn deterministic_inputs() -> Vec<(&'static str, Vec<u8>)> {
    // A realistic ASCII paragraph: the classic pangram repeated to a few KiB so
    // it spans multiple blocks and produces a non-trivial dynamic Huffman tree.
    let mut paragraph = Vec::new();
    for _ in 0..64 {
        paragraph.extend_from_slice(b"The quick brown fox jumps over the lazy dog. ");
    }

    vec![
        ("empty", Vec::new()),
        ("single_byte", b"x".to_vec()),
        ("hello", b"hello, hello!".to_vec()),
        // Highly repetitive: 100 KiB of one byte exercises maximal-length LZ77
        // matches and the RLE path.
        ("repetitive_100k", vec![b'A'; 100_000]),
        // A short repeating pattern (30 KiB) — many short, close matches.
        ("short_pattern_30k", b"abc".repeat(10_000)),
        ("text_paragraph", paragraph),
        // Compresses extremely well; mirrors example.c's zero-buffer behavior.
        ("zeros_50k", vec![0u8; 50_000]),
    ]
}

// ===========================================================================
// Helpers — Phase A (one-shot) and Phase D (streaming engine)
// ===========================================================================

/// Compress `data` at `level` using only the one-shot [`compress2`] helper and
/// return the produced zlib stream. The destination is sized with
/// [`compress_bound`] (never `data.len()`), so even an empty input — whose
/// stream is the zlib header + an empty block + the Adler-32 trailer — fits.
fn compress_to_vec(data: &[u8], level: i32) -> Vec<u8> {
    let mut comp = vec![0u8; compress_bound(data.len())];
    let n = compress2(&mut comp, data, level).expect("compress2 failed");
    comp.truncate(n);
    comp
}

/// One-shot round-trip: compress `data` at `level`, then [`uncompress`] it back,
/// returning the recovered bytes. The output buffer is sized to the known
/// original length (with a one-byte floor so an *empty* expected output still
/// gives the decoder room to consume the header/trailer and reach
/// [`ReturnCode::StreamEnd`]); the result is truncated to the bytes actually
/// produced.
fn round_trip_level(data: &[u8], level: i32) -> Vec<u8> {
    let comp = compress_to_vec(data, level);
    let mut out = vec![0u8; data.len().max(1)];
    let m = uncompress(&mut out, &comp).expect("uncompress failed");
    out.truncate(m);
    out
}

/// Drive the streaming compressor to completion for one `(level, strategy,
/// window_bits)` configuration and return the produced bytes.
///
/// `window_bits` selects the wrapper: `8..=15` zlib, `-8..=-15` raw, `24..=31`
/// gzip. The loop feeds the whole input with [`Flush::Finish`] and advances by
/// the returned `(consumed, produced)` counts — recall [`deflate`]'s tuple is
/// `(result, consumed, produced)`. The output starts at [`compress_bound`] plus
/// a margin for the gzip header/trailer and grows if the engine ever needs more.
/// The compressor state is released by `Drop` when `strm` goes out of scope (the
/// RAII equivalent of `deflateEnd`).
fn deflate_to_vec(data: &[u8], level: i32, strategy: Strategy, window_bits: i32) -> Vec<u8> {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        window_bits,
        DEF_MEM_LEVEL,
        strategy,
    )
    .expect("deflate_init2 failed");

    let mut out = vec![0u8; compress_bound(data.len()) + 64];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        if out_pos == out.len() {
            // Output exhausted before the stream finished: grow and retry.
            out.resize(out.len() * 2 + 64, 0);
        }
        let (result, consumed, produced) = deflate(
            &mut strm,
            &data[in_pos..],
            &mut out[out_pos..],
            Flush::Finish,
        );
        in_pos += consumed;
        out_pos += produced;

        match result {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(_) => {
                // `Z_OK`: more work remains, which (with `Flush::Finish`) means
                // the output filled. The loop top grows it; assert there was no
                // genuine stall (no progress *and* room still available).
                assert!(
                    consumed != 0 || produced != 0 || out_pos == out.len(),
                    "deflate made no progress with output room available"
                );
            }
            Err(e) => panic!("deflate failed: {e:?}"),
        }
    }

    out.truncate(out_pos);
    out
}

/// Drive the streaming decompressor to completion and return the recovered
/// bytes. `window_bits` MUST match the value the stream was produced with
/// (raw↔raw, zlib↔zlib, gzip↔gzip). `expected_len` sizes the output buffer; a
/// one-byte floor lets a zero-length expected output still reach
/// [`ReturnCode::StreamEnd`]. Recall [`InflateState::inflate`] reports
/// `consumed` / `produced` / `status` (the reverse field order of [`deflate`]).
/// The decoder state is released by `Drop` (the RAII equivalent of
/// `inflateEnd`).
fn inflate_from_vec(comp: &[u8], window_bits: i32, expected_len: usize) -> Vec<u8> {
    let mut state = InflateState::new(window_bits).expect("inflate init failed");
    let mut out = vec![0u8; expected_len.max(1)];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    loop {
        let before = (in_pos, out_pos);
        let result = state.inflate(&comp[in_pos..], &mut out[out_pos..], Flush::NoFlush);
        in_pos += result.consumed;
        out_pos += result.produced;

        match result.status {
            Ok(ReturnCode::StreamEnd) => break,
            Ok(_) => {
                if (in_pos, out_pos) == before {
                    // No forward progress. If the output filled, grow it;
                    // otherwise the input is exhausted on an incomplete stream —
                    // stop and let the caller's assertion report the mismatch.
                    if out_pos == out.len() {
                        out.resize(out.len() * 2 + 16, 0);
                    } else {
                        break;
                    }
                }
            }
            Err(e) => panic!("inflate failed: {e:?}"),
        }
    }

    out.truncate(out_pos);
    out
}

/// Full streaming round-trip for one configuration: compress with the given
/// strategy/`windowBits`, then decompress with a *matching* `windowBits`.
fn round_trip_engine(data: &[u8], level: i32, strategy: Strategy, window_bits: i32) -> Vec<u8> {
    let comp = deflate_to_vec(data, level, strategy, window_bits);
    inflate_from_vec(&comp, window_bits, data.len())
}

// ===========================================================================
// Phase C — deterministic edge-case inputs at every level
// ===========================================================================

/// Every deterministic edge-case input must round-trip through the one-shot
/// zlib API at *every* level (`-1` and `0..=9`). This is the broad level sweep;
/// the strategy/`windowBits` dimensions are covered by the matrix below.
#[test]
fn round_trip_edge_cases() {
    for (name, input) in deterministic_inputs() {
        for &level in &ALL_LEVELS {
            let recovered = round_trip_level(&input, level);
            assert_eq!(
                recovered.as_slice(),
                input.as_slice(),
                "one-shot round-trip mismatch for input '{name}' at level {level}"
            );
        }
    }
}

/// The empty input is the trickiest DEFLATE edge case (an empty block plus a
/// trailer), so it is also asserted on its own — at every level, and through
/// every wrapper on the engine path — to make a regression here unmistakable.
#[test]
fn round_trip_empty_input() {
    for &level in &ALL_LEVELS {
        assert!(
            round_trip_level(b"", level).is_empty(),
            "empty one-shot round-trip did not recover 0 bytes at level {level}"
        );
    }
    for &wb in &window_bits_variants() {
        for &strategy in &STRATEGIES {
            assert!(
                round_trip_engine(b"", 6, strategy, wb).is_empty(),
                "empty engine round-trip did not recover 0 bytes: windowBits {wb}, strategy {strategy:?}"
            );
        }
    }
}

// ===========================================================================
// Phase D — engine matrix: levels × strategies × windowBits
// ===========================================================================

/// The full streaming matrix. Strategy and `windowBits` handling is level- and
/// size-orthogonal, so a representative level subset (`0, 1, 6, 9`) keeps the
/// runtime sane while every strategy and every wrapper is exercised against the
/// whole deterministic corpus. Each stream is decompressed with a *matching*
/// `windowBits`.
#[test]
fn round_trip_strategy_windowbits_matrix() {
    let levels = [Z_NO_COMPRESSION, Z_BEST_SPEED, 6, Z_BEST_COMPRESSION];
    let inputs = deterministic_inputs();

    for &window_bits in &window_bits_variants() {
        for &level in &levels {
            for &strategy in &STRATEGIES {
                for (name, input) in &inputs {
                    let recovered = round_trip_engine(input, level, strategy, window_bits);
                    assert_eq!(
                        recovered.as_slice(),
                        input.as_slice(),
                        "engine round-trip mismatch: input '{name}', level {level}, \
                         strategy {strategy:?}, windowBits {window_bits}"
                    );
                }
            }
        }
    }
}

/// `windowBits` *size* variation: round-trip a repetitive input through the
/// smaller zlib windows `9..=14`. Confirms both engines agree on the window size
/// and that matches still recover when the window is reduced.
#[test]
fn round_trip_small_windows() {
    // 64 KiB whose matches sit at distance 8 — well within even a 9-bit (512 B)
    // window, so the data must recover at every window size in the range.
    let input = b"abcdefgh".repeat(8192);
    for window_bits in 9..=14 {
        let recovered = round_trip_engine(&input, 6, Strategy::Default, window_bits);
        assert_eq!(
            recovered.as_slice(),
            input.as_slice(),
            "small-window round-trip failed at windowBits {window_bits}"
        );
    }
}

// ===========================================================================
// Phase E — asymmetry / sizing sanity properties
// ===========================================================================

/// The produced compressed length must never exceed [`compress_bound`] — the
/// `deflateBound`/`compressBound` sizing contract. Asserted for every
/// deterministic input at every level.
#[test]
fn compressed_is_not_larger_than_bound() {
    for (name, input) in deterministic_inputs() {
        let bound = compress_bound(input.len());
        for &level in &ALL_LEVELS {
            let mut comp = vec![0u8; bound];
            let n =
                compress2(&mut comp, &input, level).expect("compress2 within bound must succeed");
            assert!(
                n <= bound,
                "compressed length {n} exceeded compress_bound {bound} for '{name}' at level {level}"
            );
        }
    }
}

/// Sanity guard against a stuck/garbage encoder: two clearly different,
/// non-trivial inputs must not compress to identical byte streams (and each must
/// still round-trip). Deliberately lenient — applied only to substantial,
/// distinct inputs.
#[test]
fn distinct_inputs_distinct_streams() {
    let a = b"The quick brown fox jumps over the lazy dog.".repeat(16);
    let b = b"Pack my box with five dozen liquor jugs, please.".repeat(16);
    let level = 6;

    let ca = compress_to_vec(&a, level);
    let cb = compress_to_vec(&b, level);
    assert_ne!(
        ca, cb,
        "distinct inputs unexpectedly produced identical compressed streams"
    );

    assert_eq!(round_trip_level(&a, level).as_slice(), a.as_slice());
    assert_eq!(round_trip_level(&b, level).as_slice(), b.as_slice());
}

// ===========================================================================
// Phase B — property-based fuzzing + random/incompressible data
// ===========================================================================
//
// `quickcheck` and `rand` require the standard library, so everything that uses
// them lives behind `#[cfg(feature = "std")]`. Both validated build
// configurations (`default` and `--no-default-features --features std`) keep
// `std` enabled, so this module is compiled and run in both; a pure `no_std`
// build simply omits it. All helpers it relies on (`round_trip_level`,
// `round_trip_engine`, `window_bits_variants`, the `Strategy` constants) are the
// ungated top-level items, reached via `use super::*`.
#[cfg(feature = "std")]
mod prop_tests {
    use super::*;
    use quickcheck::{QuickCheck, quickcheck};
    use rand::rngs::StdRng;
    use rand::{RngCore, SeedableRng};

    /// Deterministic, reproducible pseudo-random bytes from a fixed seed. A
    /// seeded RNG (never an entropy-seeded one) keeps any failure reproducible.
    fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut data = vec![0u8; len];
        rng.fill_bytes(&mut data);
        data
    }

    /// Incompressible 64 KiB buffer (fixed seed) must round-trip even though it
    /// cannot shrink — this verifies the stored-block / level-0 expansion path
    /// and that [`compress_bound`] still bounds the output. Checked through the
    /// one-shot API at every level and through every wrapper on the engine path.
    #[test]
    fn round_trip_random_data() {
        let data = random_bytes(0xC0FFEE, 65_536);

        for &level in &ALL_LEVELS {
            assert_eq!(
                round_trip_level(&data, level).as_slice(),
                data.as_slice(),
                "random-data one-shot round-trip failed at level {level}"
            );
        }

        for &window_bits in &window_bits_variants() {
            for &strategy in &[Strategy::Default, Strategy::HuffmanOnly] {
                assert_eq!(
                    round_trip_engine(&data, 6, strategy, window_bits).as_slice(),
                    data.as_slice(),
                    "random-data engine round-trip failed: windowBits {window_bits}, \
                     strategy {strategy:?}"
                );
            }
        }
    }

    /// Wider coverage (1000 cases) for the default-level invariant; complements
    /// the macro-generated property below, which uses quickcheck's default count.
    #[test]
    fn prop_round_trip_default_extended() {
        fn prop(data: Vec<u8>) -> bool {
            round_trip_level(&data, Z_DEFAULT_COMPRESSION) == data
        }
        QuickCheck::new()
            .tests(1000)
            .quickcheck(prop as fn(Vec<u8>) -> bool);
    }

    quickcheck! {
        /// Arbitrary input round-trips at the default level. `quickcheck` shrinks
        /// any counterexample to a minimal failing `Vec<u8>`.
        fn prop_round_trip_default(data: Vec<u8>) -> bool {
            round_trip_level(&data, Z_DEFAULT_COMPRESSION) == data
        }

        /// Arbitrary input round-trips at any level. The generated level is
        /// clamped into `0..=9` with `rem_euclid` (it generates the level too).
        fn prop_round_trip_all_levels(data: Vec<u8>, raw_level: i8) -> bool {
            let level = i32::from(raw_level).rem_euclid(10);
            round_trip_level(&data, level) == data
        }

        /// Arbitrary input survives every `windowBits` wrapper (raw / zlib, plus
        /// gzip when enabled). The generated choice is reduced modulo the
        /// available variant set, and the decoder uses the matching `windowBits`.
        fn prop_round_trip_window_bits(data: Vec<u8>, choice: u8) -> bool {
            let variants = window_bits_variants();
            let window_bits = variants[usize::from(choice) % variants.len()];
            round_trip_engine(&data, 6, Strategy::Default, window_bits) == data
        }
    }
}
