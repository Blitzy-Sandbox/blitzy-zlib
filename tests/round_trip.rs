//! Property-based round-trip tests for `zlib-rs`.
//!
//! This integration test suite verifies the central correctness contract of the
//! library: **anything `zlib-rs` compresses, `zlib-rs` decompresses back to the
//! exact original bytes** — across randomized inputs, every compression level,
//! every strategy, and the zlib and raw window framings.
//!
//! # Provenance
//!
//! Conceptually derived from `test/example.c`'s round-trip behaviour
//! (`test_compress`/`test_deflate`/`test_inflate` and the large-buffer variants),
//! generalized from a handful of fixed inputs to thousands of randomized cases
//! using [`quickcheck`]. Where `example.c` forces one-byte "small buffers" to
//! stress the incremental engine, the chunked-feeding properties here do the
//! same with arbitrary chunk sizes (Property C). Run with:
//!
//! ```text
//! cargo test --test round_trip
//! ```
//!
//! # Properties verified
//!
//! * **A — one-shot identity:** [`compress2`] then [`uncompress`] reproduces the
//!   input, exercised once per level (`-1, 0, 1, 2, 6, 9`) plus an arbitrary
//!   clamped level.
//! * **B — streaming identity:** the [`Deflate`] → [`Inflate`] streaming path
//!   round-trips across all five [`Strategy`] values and the zlib (`15`, `9`) and
//!   raw (`-15`, `-9`) window-bit framings.
//! * **C — chunked invariance:** feeding either side in arbitrarily small chunks
//!   yields the same result, validating the `consumed`/`produced` bookkeeping.
//! * **D — determinism & bounds:** the same input/level always yields identical
//!   compressed bytes, and the output never exceeds [`compress_bound`].
//!
//! Each property is complemented by fixed-vector edge tests: the empty input, a
//! single byte, a 70 000-byte highly repetitive buffer (which crosses the 32 KiB
//! sliding-window boundary), and incompressible random data.
//!
//! # Flush asymmetry
//!
//! The two engines expose deliberately different shapes, faithfully reproduced
//! by the helpers below:
//!
//! * [`Deflate::compress`] takes a typed [`FlushMode`] and returns
//!   `Result<DeflateOutcome, ZlibError>` (with `code`/`consumed`/`produced`).
//! * [`Inflate::inflate`] takes a raw C `i32` flush and returns the raw
//!   `(code, consumed, produced)` triple.
//!
//! # Framing scope
//!
//! Only the always-available zlib (`windowBits` 8..=15) and raw (`-8..=-15`)
//! framings are exercised here. The gzip framing (`24..=31`, `40..=47`) requires
//! the `gzip` feature and is covered by `tests/gzip_compat.rs`, so it is
//! intentionally out of scope for this file (keeping every property unconditional
//! and free of `#[cfg(feature = ...)]` gating).

use quickcheck::{TestResult, quickcheck};
use rand::Rng;
use zlib_rs::constants::{DEF_MEM_LEVEL, Z_DEFAULT_COMPRESSION, Z_NO_FLUSH, Z_STREAM_END};
use zlib_rs::{
    Deflate, FlushMode, Inflate, ReturnCode, Strategy, compress_bound, compress2, uncompress,
};

// ===========================================================================
// Shared private fixtures
// ===========================================================================

/// Every compression strategy, enumerated for the matrix-style edge tests.
const STRATEGIES: [Strategy; 5] = [
    Strategy::Default,
    Strategy::Filtered,
    Strategy::HuffmanOnly,
    Strategy::Rle,
    Strategy::Fixed,
];

/// The window-bit framings exercised here: zlib `15`/`9` and raw `-15`/`-9`.
/// Deflate and inflate window-bit signs are always matched (zlib↔zlib,
/// raw↔raw); gzip framing is out of scope (see the module docs).
const WINDOW_BITS: [i32; 4] = [15, 9, -15, -9];

// ===========================================================================
// Private helpers — all confined to this file (no shared module).
// ===========================================================================

/// One-shot identity check: compress the whole `data` at `level`, decompress it,
/// and report whether the result equals `data` exactly.
///
/// Any engine error is treated as a property failure (`false`). The decompressed
/// length equals the original length, so the output buffer is sized exactly to
/// `data.len()` — with a one-byte floor so an empty input still presents a
/// non-empty, unambiguous destination to [`uncompress`] (whose empty-stream
/// result is `Ok(0)`).
fn oneshot_roundtrip(data: &[u8], level: i32) -> bool {
    let Ok(packed) = compress2(data, level) else {
        return false;
    };
    let mut out = vec![0u8; data.len().max(1)];
    match uncompress(&mut out, &packed) {
        Ok(produced) => &out[..produced] == data,
        Err(_) => false,
    }
}

/// Drive the DEFLATE engine to a complete, finished stream and return it.
///
/// `input` is fed in slices of at most `in_chunk` bytes ([`FlushMode::NoFlush`]
/// until the final slice, then [`FlushMode::Finish`]) and output is drained
/// through an `out_chunk`-sized scratch buffer, mirroring zlib's incremental
/// `deflate()` contract. `wb` selects the framing (8..=15 zlib, negative raw) and
/// `strat` the strategy. Both chunk sizes are floored at 1. Panics on any engine
/// error or a no-progress stall (each a genuine test failure rather than a hang).
fn deflate_stream(
    input: &[u8],
    level: i32,
    wb: i32,
    strat: Strategy,
    in_chunk: usize,
    out_chunk: usize,
) -> Vec<u8> {
    let in_chunk = in_chunk.max(1);
    let mut deflate = Deflate::with_options(level, wb, DEF_MEM_LEVEL, strat)
        .expect("Deflate::with_options must accept valid options");
    let mut compressed = Vec::new();
    let mut scratch = vec![0u8; out_chunk.max(1)];
    let mut in_pos = 0usize;

    loop {
        // The input window for this call; `saturating_add` lets callers pass
        // `usize::MAX` to mean "feed everything at once".
        let in_end = in_pos.saturating_add(in_chunk).min(input.len());
        let at_end = in_end == input.len();
        let flush = if at_end {
            FlushMode::Finish
        } else {
            FlushMode::NoFlush
        };

        let outcome = deflate
            .compress(&input[in_pos..in_end], &mut scratch, flush)
            .expect("deflate must not error on a valid stream");
        in_pos += outcome.consumed;
        compressed.extend_from_slice(&scratch[..outcome.produced]);

        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        // A non-terminal call that neither consumed nor produced anything would
        // spin forever; the engine's progress guarantee forbids it, so surface
        // the violation loudly instead of looping.
        if outcome.consumed == 0 && outcome.produced == 0 {
            panic!("deflate made no progress (stall) before Z_STREAM_END");
        }
    }
    compressed
}

/// Drive the INFLATE engine to a complete (`Z_STREAM_END`) decode and return the
/// decompressed bytes.
///
/// `compressed` is fed in slices of at most `in_chunk` bytes and output is
/// drained through an `out_chunk`-sized scratch buffer, mirroring zlib's
/// incremental `inflate()` contract (the flush argument is the raw C
/// [`Z_NO_FLUSH`]). `wb` must match the framing the stream was produced with
/// (sign-matched). `expected_len` only sizes the initial output capacity. Panics
/// on any engine error or a truncated stream.
fn inflate_stream(
    compressed: &[u8],
    wb: i32,
    expected_len: usize,
    in_chunk: usize,
    out_chunk: usize,
) -> Vec<u8> {
    let in_chunk = in_chunk.max(1);
    let mut inflate =
        Inflate::with_window_bits(wb).expect("Inflate::with_window_bits must accept valid bits");
    let mut decoded = Vec::with_capacity(expected_len);
    let mut scratch = vec![0u8; out_chunk.max(1)];
    let mut in_pos = 0usize;

    loop {
        let in_end = in_pos.saturating_add(in_chunk).min(compressed.len());
        let (code, consumed, produced) =
            inflate.inflate(&compressed[in_pos..in_end], &mut scratch, Z_NO_FLUSH);
        in_pos += consumed;
        decoded.extend_from_slice(&scratch[..produced]);

        if code == Z_STREAM_END {
            break;
        }
        assert!(code >= 0, "inflate reported error code {code}");
        // No progress with no input left means the stream ended prematurely.
        if consumed == 0 && produced == 0 && in_pos >= compressed.len() {
            panic!("inflate ran out of input before Z_STREAM_END (code {code})");
        }
    }
    decoded
}

/// Whole-buffer streaming round trip: deflate all of `data` in a single finishing
/// pass, then inflate it back in a single pass. Returns the decoded bytes for the
/// caller to compare against `data`.
fn stream_roundtrip(data: &[u8], level: i32, strat: Strategy, wb: i32) -> Vec<u8> {
    // `usize::MAX` chunk == "all at once"; scratch buffers are sized to hold the
    // whole compressed / decompressed payload so each direction is one call.
    let out_cap = compress_bound(data.len()) + 64;
    let compressed = deflate_stream(data, level, wb, strat, usize::MAX, out_cap);
    inflate_stream(
        &compressed,
        wb,
        data.len(),
        usize::MAX,
        data.len().max(1) + 64,
    )
}

/// Generate `n` bytes of high-entropy (effectively incompressible) data using the
/// thread-local RNG (rand 0.9 API: [`rand::rng`] + [`Rng::fill`]).
fn incompressible(n: usize) -> Vec<u8> {
    let mut rng = rand::rng();
    let mut data = vec![0u8; n];
    rng.fill(&mut data[..]);
    data
}

// ===========================================================================
// Property A — one-shot `compress2` → `uncompress` identity, per level.
//
// The headline property, run once per representative level so a failure points
// at the offending level. Each test wraps a fixed-level `bool` property in
// `quickcheck`, which drives it over 100 randomized inputs by default.
// ===========================================================================

#[test]
fn roundtrip_level_default() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, Z_DEFAULT_COMPRESSION)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_level_0() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, 0)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_level_1() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, 1)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_level_2() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, 2)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_level_6() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, 6)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_level_9() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip(&data, 9)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn roundtrip_arbitrary_level() {
    fn prop(data: Vec<u8>, lvl: i8) -> TestResult {
        // Map the arbitrary byte deterministically into the 11 valid levels
        // (`-1..=9`) so every generated case exercises a real level rather than
        // being discarded — this keeps quickcheck's generation budget productive
        // while still covering the whole level range over many runs.
        let level = i32::from(lvl).rem_euclid(11) - 1;
        TestResult::from_bool(oneshot_roundtrip(&data, level))
    }
    quickcheck(prop as fn(Vec<u8>, i8) -> TestResult);
}

// ===========================================================================
// Property B — streaming `Deflate` → `Inflate` identity.
//
// The per-strategy tests fix the zlib `windowBits = 15` framing at the most
// demanding level (9, lazy matching) and vary only the strategy. The per-window
// tests fix `Strategy::Default` at level 6 and vary only the framing, covering
// both the zlib and raw wrappers with matched signs.
// ===========================================================================

#[test]
fn stream_roundtrip_strategy_default() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 9, Strategy::Default, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_strategy_filtered() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 9, Strategy::Filtered, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_strategy_huffman_only() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 9, Strategy::HuffmanOnly, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_strategy_rle() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 9, Strategy::Rle, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_strategy_fixed() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 9, Strategy::Fixed, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_window_zlib15() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 6, Strategy::Default, 15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_window_zlib9() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 6, Strategy::Default, 9) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_window_raw15() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 6, Strategy::Default, -15) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

#[test]
fn stream_roundtrip_window_raw9() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip(&data, 6, Strategy::Default, -9) == data
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

// ===========================================================================
// Property C — chunked-feeding invariance.
//
// Splitting either side of the pipeline into arbitrarily small pieces must not
// change the result, exercising the incremental `consumed`/`produced`
// bookkeeping exactly as `example.c`'s one-byte "small buffers" do.
// ===========================================================================

#[test]
fn chunked_deflate() {
    fn prop(data: Vec<u8>, chunk: u8) -> bool {
        // Feed the compressor in small chunks (1..=256 bytes) with a tiny output
        // scratch, then inflate the whole stream in one pass.
        let in_chunk = usize::from(chunk) + 1;
        let compressed = deflate_stream(&data, 6, 15, Strategy::Default, in_chunk, 64);
        let decoded = inflate_stream(
            &compressed,
            15,
            data.len(),
            usize::MAX,
            data.len().max(1) + 64,
        );
        decoded == data
    }
    quickcheck(prop as fn(Vec<u8>, u8) -> bool);
}

#[test]
fn chunked_inflate() {
    fn prop(data: Vec<u8>, chunk: u8) -> bool {
        // Compress the whole input in one pass, then decompress it supplying both
        // input and output in small chunks (1..=256 bytes), looping until
        // `Z_STREAM_END`.
        let out_chunk = usize::from(chunk) + 1;
        let out_cap = compress_bound(data.len()) + 64;
        let compressed = deflate_stream(&data, 6, 15, Strategy::Default, usize::MAX, out_cap);
        let decoded = inflate_stream(&compressed, 15, data.len(), out_chunk, out_chunk);
        decoded == data
    }
    quickcheck(prop as fn(Vec<u8>, u8) -> bool);
}

// ===========================================================================
// Property D — determinism and `compress_bound` sufficiency.
//
// Byte-identical output is the project's central contract; determinism (same
// input + level ⇒ same bytes) is its observable consequence. `compress_bound`
// must always be a safe upper bound for the produced length.
// ===========================================================================

#[test]
fn deterministic() {
    fn prop(data: Vec<u8>, lvl: i8) -> bool {
        let level = i32::from(lvl).rem_euclid(11) - 1;
        match (compress2(&data, level), compress2(&data, level)) {
            (Ok(first), Ok(second)) => first == second,
            _ => false,
        }
    }
    quickcheck(prop as fn(Vec<u8>, i8) -> bool);
}

#[test]
fn compress_bound_sufficient() {
    fn prop(data: Vec<u8>, lvl: i8) -> bool {
        let level = i32::from(lvl).rem_euclid(11) - 1;
        match compress2(&data, level) {
            Ok(packed) => packed.len() <= compress_bound(data.len()),
            Err(_) => false,
        }
    }
    quickcheck(prop as fn(Vec<u8>, i8) -> bool);
}

#[test]
fn compress_bound_zero() {
    // The empty-input bound is the fixed 13-byte zlib-wrapper + Huffman overhead
    // (matching the C `compressBound(0)`).
    assert_eq!(compress_bound(0), 13);
}

// ===========================================================================
// Fixed-vector edge cases.
//
// quickcheck's default generator produces short inputs (≤ ~100 bytes), so these
// deterministic vectors pin down the corners it rarely reaches: the empty input,
// a single byte, a buffer large enough to cross the 32 KiB window, and
// incompressible high-entropy data.
// ===========================================================================

#[test]
fn empty() {
    let data: Vec<u8> = Vec::new();

    // One-shot identity across every level (including the default sentinel).
    for level in [Z_DEFAULT_COMPRESSION, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
        assert!(
            oneshot_roundtrip(&data, level),
            "empty input failed one-shot round trip at level {level}"
        );
    }

    // Streaming identity across both framings and every strategy: an empty input
    // must still produce a valid, decodable (empty-payload) stream.
    for &wb in &WINDOW_BITS {
        for strat in STRATEGIES {
            assert!(
                stream_roundtrip(&data, 6, strat, wb).is_empty(),
                "empty input failed streaming round trip (wb={wb}, strat={strat:?})"
            );
        }
    }
}

#[test]
fn single_byte() {
    for byte in [0u8, 1, 42, 0x80, 0xFF] {
        let data = vec![byte];

        for level in [Z_DEFAULT_COMPRESSION, 0, 1, 6, 9] {
            assert!(
                oneshot_roundtrip(&data, level),
                "single byte {byte} failed one-shot round trip at level {level}"
            );
        }

        for &wb in &WINDOW_BITS {
            for strat in STRATEGIES {
                assert_eq!(
                    stream_roundtrip(&data, 6, strat, wb),
                    data,
                    "single byte {byte} failed streaming round trip (wb={wb}, strat={strat:?})"
                );
            }
        }
    }
}

#[test]
fn highly_repetitive() {
    // 70 000 identical bytes far exceed the 32 KiB sliding window, exercising the
    // longest LZ77 back-references and multiple block boundaries.
    let data = vec![b'a'; 70_000];

    // One-shot identity at a spread of levels.
    for level in [Z_DEFAULT_COMPRESSION, 1, 6, 9] {
        assert!(
            oneshot_roundtrip(&data, level),
            "highly repetitive input failed one-shot round trip at level {level}"
        );
    }

    // Such input must compress by orders of magnitude (sanity on the LZ77 path).
    let packed = compress2(&data, 9).expect("compressing repetitive data must succeed");
    assert!(
        packed.len() < data.len() / 50,
        "repetitive data should compress dramatically (got {} bytes from {})",
        packed.len(),
        data.len()
    );

    // Streaming identity across both framings and every strategy.
    for &wb in &WINDOW_BITS {
        for strat in STRATEGIES {
            assert_eq!(
                stream_roundtrip(&data, 9, strat, wb),
                data,
                "highly repetitive input failed streaming round trip (wb={wb}, strat={strat:?})"
            );
        }
    }

    // The most demanding chunking: one byte in and one byte out, end to end, over
    // the whole large input — stressing window-boundary bookkeeping.
    let compressed = deflate_stream(&data, 6, 15, Strategy::Default, 1, 1);
    let decoded = inflate_stream(&compressed, 15, data.len(), 1, 1);
    assert_eq!(
        decoded, data,
        "highly repetitive input failed one-byte-chunk round trip"
    );
}

#[test]
fn incompressible_random() {
    // High-entropy data cannot shrink below its size plus overhead, yet must
    // still round-trip exactly. The sizes span sub-window and super-window.
    for &n in &[1usize, 100, 4_096, 40_000] {
        let data = incompressible(n);

        for level in [Z_DEFAULT_COMPRESSION, 0, 1, 6, 9] {
            assert!(
                oneshot_roundtrip(&data, level),
                "incompressible input (n={n}) failed one-shot round trip at level {level}"
            );
        }

        for &wb in &WINDOW_BITS {
            assert_eq!(
                stream_roundtrip(&data, 6, Strategy::Default, wb),
                data,
                "incompressible input (n={n}) failed streaming round trip (wb={wb})"
            );
        }

        // Even for incompressible data the produced length respects the bound.
        let packed = compress2(&data, 9).expect("compressing incompressible data must succeed");
        assert!(
            packed.len() <= compress_bound(data.len()),
            "incompressible input (n={n}) exceeded compress_bound"
        );
    }
}
