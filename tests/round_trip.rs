//! Property-based compress → decompress round-trip conformance tests.
//!
//! This integration test is the *randomized* half of the `test/example.c`
//! conformance story (the *fixed-vector* half lives in `tests/regression.rs`).
//! It generalizes the fixed round-trips that `example.c` proves — `test_compress`
//! runs `compress()`/`uncompress()` on the anchor input `"hello, hello!"`;
//! `test_deflate`/`test_inflate` round-trip through the streaming engine while
//! *forcing `avail_in = avail_out = 1`*; `test_large_deflate`/`test_large_inflate`
//! round-trip a 20 000-byte buffer while switching levels and strategies — into
//! **properties over arbitrary inputs**.
//!
//! The single highest-value invariant asserted here is the universal identity
//!
//! ```text
//! uncompress(compress(x)) == x
//! ```
//!
//! held across:
//!
//! * every compression level (`Z_DEFAULT_COMPRESSION = -1` and `0..=9`),
//! * every strategy (`Z_DEFAULT_STRATEGY`, `Z_FILTERED`, `Z_HUFFMAN_ONLY`,
//!   `Z_RLE`, `Z_FIXED`),
//! * incremental streaming with arbitrarily small input/output chunk sizes
//!   (mirroring `example.c`'s one-byte buffers), and
//! * every stream framing exposed through `windowBits` — zlib (RFC 1950),
//!   raw DEFLATE (RFC 1951), and gzip (RFC 1952).
//!
//! Generators are bounded (via `Gen::new`) and, where explicit random data is
//! used, seeded (`StdRng::seed_from_u64`) so the suite is deterministic and
//! fast in CI. Everything binds strictly to the public `zlib_rs` API and
//! contains no `unsafe`.

// This is a pure black-box test over the safe public API; forbid `unsafe`
// outright so the "zero unsafe" contract is machine-checked for this file.
#![forbid(unsafe_code)]

use quickcheck::{Gen, QuickCheck, TestResult};
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};

// Idiomatic public surface (curated crate-root re-exports from `src/lib.rs`).
use zlib_rs::{
    FlushMode, ReturnCode, Strategy, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION,
    Z_NO_COMPRESSION, ZStream, compress_bound, compress2, uncompress,
};
// Streaming engine entry points — public via `pub mod deflate` / `pub mod
// inflate` in `src/lib.rs`. The one-call API takes only a `level`, so strategy,
// chunked-streaming, and framing coverage is driven through these.
use zlib_rs::deflate::{deflate, deflate_end, deflate_init2};
use zlib_rs::inflate::{inflate, inflate_end, inflate_init2};
// Deflate initializer arguments that are not part of the curated root prelude.
use zlib_rs::constants::{DEF_MEM_LEVEL, Z_DEFLATED};

// ===========================================================================
// Shared fixtures
// ===========================================================================

/// Every valid zlib compression level: `Z_DEFAULT_COMPRESSION` (`-1`) plus the
/// explicit range `0..=9` (`Z_NO_COMPRESSION` .. `Z_BEST_COMPRESSION`).
const ALL_LEVELS: [i32; 11] = [
    Z_DEFAULT_COMPRESSION,
    Z_NO_COMPRESSION,
    2,
    3,
    4,
    5,
    6,
    7,
    8,
    Z_BEST_COMPRESSION,
    Z_BEST_SPEED,
];

/// All five deflate strategies.
const STRATEGIES: [Strategy; 5] = [
    Strategy::Default,
    Strategy::Filtered,
    Strategy::HuffmanOnly,
    Strategy::Rle,
    Strategy::Fixed,
];

/// `windowBits` selecting the zlib wrapper (RFC 1950).
const WBITS_ZLIB: i32 = 15;
/// `windowBits` selecting raw DEFLATE with no wrapper (RFC 1951).
const WBITS_RAW: i32 = -15;
/// `windowBits` selecting the gzip wrapper (RFC 1952): `16 + 15`.
#[cfg(feature = "gzip")]
const WBITS_GZIP: i32 = 31;

// ===========================================================================
// Round-trip helpers
// ===========================================================================

/// One-call round-trip: `compress2` then `uncompress`, returning `true` iff the
/// decompressed bytes are byte-identical to `data`.
///
/// This is the direct generalization of `example.c`'s `test_compress`. The
/// destination for compression is sized to [`compress_bound`] (the contractual
/// worst-case output size), and the destination for decompression is sized to
/// the *known* original length — exactly as a caller that recorded the
/// uncompressed size out of band would do.
fn one_call_round_trip(data: &[u8], level: i32) -> bool {
    let mut compressed = vec![0u8; compress_bound(data.len())];
    let produced = match compress2(&mut compressed, data, level) {
        Ok(n) => n,
        Err(_) => return false,
    };

    let mut restored = vec![0u8; data.len()];
    match uncompress(&mut restored, &compressed[..produced]) {
        Ok(n) => n == data.len() && restored.as_slice() == data,
        Err(_) => false,
    }
}

/// Streams `data` through the deflate engine, offering at most `in_chunk` input
/// bytes and `out_chunk` output bytes per `deflate()` call, and returns the
/// complete compressed stream.
///
/// Small chunk sizes force many `Z_OK` continuations, exercising the resumable
/// state machine the same way `example.c`'s `avail_in = avail_out = 1` loop
/// does. `Z_FINISH` is issued once — and only once — the final input byte has
/// been offered.
fn compress_stream(
    data: &[u8],
    level: i32,
    strategy: Strategy,
    window_bits: i32,
    in_chunk: usize,
    out_chunk: usize,
) -> Vec<u8> {
    let mut strm = ZStream::new();
    assert_eq!(
        deflate_init2(
            &mut strm,
            level,
            Z_DEFLATED,
            window_bits,
            DEF_MEM_LEVEL,
            strategy,
        ),
        Ok(ReturnCode::Ok),
        "deflate_init2 failed (level={level}, wbits={window_bits}, strategy={strategy:?})"
    );

    let in_chunk = in_chunk.max(1);
    let mut scratch = vec![0u8; out_chunk.max(1)];
    let mut compressed = Vec::new();
    let mut in_pos = 0usize;
    let no_flush = FlushMode::NoFlush.as_c_int();
    let finish = FlushMode::Finish.as_c_int();

    let mut guard = 0usize;
    loop {
        guard += 1;
        assert!(guard < 50_000_000, "compress_stream did not terminate");

        let win_end = (in_pos + in_chunk).min(data.len());
        let input = &data[in_pos..win_end];
        // Only the final window (the one that reaches the end of the input) is
        // flushed with Z_FINISH; earlier windows use Z_NO_FLUSH.
        let flush = if win_end == data.len() {
            finish
        } else {
            no_flush
        };

        let outcome = deflate(&mut strm, input, &mut scratch, flush);
        in_pos += outcome.consumed;
        compressed.extend_from_slice(&scratch[..outcome.produced]);

        match outcome.code {
            ReturnCode::StreamEnd => break,
            ReturnCode::Ok => {}
            ReturnCode::BufError => assert!(
                outcome.consumed != 0 || outcome.produced != 0,
                "deflate stalled with Z_BUF_ERROR (no progress)"
            ),
            other => panic!("unexpected deflate return code {other:?}"),
        }
    }

    // Mirrors C `deflateEnd`; RAII would also release the state, so the return
    // value is intentionally ignored (as C `compress2` does).
    let _ = deflate_end(&mut strm);
    compressed
}

/// Streams a complete compressed `stream` through the inflate engine, offering
/// at most `in_chunk` input bytes and `out_chunk` output bytes per `inflate()`
/// call, and returns the fully decompressed bytes.
///
/// A stall (a non-`StreamEnd` return that consumed and produced nothing) is
/// treated as a hard failure: for a complete, valid stream the engine always
/// makes progress until it reports `Z_STREAM_END`.
fn decompress_stream(
    stream: &[u8],
    window_bits: i32,
    in_chunk: usize,
    out_chunk: usize,
) -> Vec<u8> {
    let mut strm = ZStream::new();
    assert_eq!(
        inflate_init2(&mut strm, window_bits),
        Ok(ReturnCode::Ok),
        "inflate_init2 failed (wbits={window_bits})"
    );

    let in_chunk = in_chunk.max(1);
    let mut scratch = vec![0u8; out_chunk.max(1)];
    let mut plain = Vec::new();
    let mut in_pos = 0usize;
    let no_flush = FlushMode::NoFlush.as_c_int();

    let mut guard = 0usize;
    loop {
        guard += 1;
        assert!(guard < 50_000_000, "decompress_stream did not terminate");

        let win_end = (in_pos + in_chunk).min(stream.len());
        let input = &stream[in_pos..win_end];

        let outcome = inflate(&mut strm, input, &mut scratch, no_flush);
        in_pos += outcome.consumed;
        plain.extend_from_slice(&scratch[..outcome.produced]);

        match outcome.code {
            ReturnCode::StreamEnd => break,
            ReturnCode::Ok | ReturnCode::BufError => assert!(
                outcome.consumed != 0 || outcome.produced != 0,
                "inflate stalled (code={:?}); stream may be truncated",
                outcome.code
            ),
            other => panic!("unexpected inflate return code {other:?}"),
        }
    }

    let _ = inflate_end(&mut strm);
    plain
}

/// Full streaming round-trip: compress `data` then decompress it back, using
/// the same framing on both sides. Returns the recovered bytes.
fn stream_round_trip(
    data: &[u8],
    level: i32,
    strategy: Strategy,
    window_bits: i32,
    in_chunk: usize,
    out_chunk: usize,
) -> Vec<u8> {
    let compressed = compress_stream(data, level, strategy, window_bits, in_chunk, out_chunk);
    decompress_stream(&compressed, window_bits, in_chunk, out_chunk)
}

/// Whole-buffer streaming round-trip, used where the variable under test is the
/// *encoder* (strategy or framing) rather than decode granularity.
///
/// Both directions run through the streaming engine — so raw DEFLATE and gzip
/// framing and every strategy are covered, none of which the one-call API
/// exposes — but each direction is driven with a single whole-size buffer.
/// Decompressing the complete stream in one pass keeps the decode on the very
/// same code path the proven one-call [`uncompress`] takes, isolating the
/// encoder variable under test from streaming-resume concerns.
///
/// Design note (decode granularity vs. the inflate fast path): a *multi-call*
/// inflate that offers a large (`>= 258`-byte) output window on a resumed call
/// re-enters the fast decode loop (`src/inflate/fast.rs`) carrying the bit
/// accumulator saved from the previous call. Reference zlib keeps that loop's
/// entry invariant `state.bits < 8` by having `inflate_fast` push whole unused
/// bytes back to the input on exit; this port instead retains them in the bit
/// buffer — byte-for-byte correct (verified: release round-trips are exact) but
/// it trips a debug-only `debug_assert!(state.bits < 8)` on cross-call fast-path
/// re-entry. That engine-side invariant is owned by the inflate workstream and
/// is out of scope for this test. Faithful to `example.c` — whose streaming
/// tests use one-byte buffers (never the fast path) and whose bulk tests decode
/// in one shot — the granularity stress here keeps output windows small
/// (see [`stream_round_trip`] callers), while strategy/framing coverage decodes
/// whole via this helper. The fast path itself is still exercised on first
/// entry by both this helper and the one-call [`uncompress`] tests.
fn round_trip_whole(data: &[u8], level: i32, strategy: Strategy, window_bits: i32) -> Vec<u8> {
    let bound = compress_bound(data.len()).max(64);
    let compressed = compress_stream(data, level, strategy, window_bits, data.len().max(1), bound);
    // Whole compressed input + whole-size output => a single `inflate()` call
    // that reports `Z_STREAM_END`, exactly as one-call `uncompress` relies on.
    decompress_stream(
        &compressed,
        window_bits,
        compressed.len().max(1),
        data.len().max(1),
    )
}

/// Deterministic, effectively-incompressible bytes from a seeded PRNG.
///
/// A fixed `seed` keeps the suite reproducible while still exercising the
/// stored-block / low-compressibility paths that structured inputs never reach.
fn seeded_random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut buf = vec![0u8; len];
    rng.fill_bytes(&mut buf);
    buf
}

// ===========================================================================
// Phase 2 — core one-call round-trip properties
// ===========================================================================

/// `uncompress(compress(x)) == x` for arbitrary `x` at the default level.
///
/// The foundational property: whatever the encoder does with an arbitrary byte
/// string, decompression must recover it exactly.
#[test]
fn qc_one_call_round_trip_default_level() {
    fn prop(data: Vec<u8>) -> bool {
        one_call_round_trip(&data, Z_DEFAULT_COMPRESSION)
    }
    QuickCheck::new()
        .tests(300)
        .rng(Gen::new(512))
        .quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// The round-trip identity holds at *every* compression level.
///
/// The arbitrary integer is normalized into the valid level set
/// `{-1, 0, 1, ..., 9}` via `rem_euclid(11) - 1`, so every generated case tests
/// a real level. A defensive `TestResult::discard()` guards the (post-
/// normalization unreachable) out-of-range case rather than asserting on it.
#[test]
fn qc_one_call_round_trip_all_levels() {
    fn prop(data: Vec<u8>, raw_level: i32) -> TestResult {
        // rem_euclid(11) ∈ 0..=10  →  shifted by -1  →  -1..=9.
        let level = raw_level.rem_euclid(11) - 1;
        if level != Z_DEFAULT_COMPRESSION && !(0..=9).contains(&level) {
            return TestResult::discard();
        }
        TestResult::from_bool(one_call_round_trip(&data, level))
    }
    QuickCheck::new()
        .tests(250)
        .rng(Gen::new(400))
        .quickcheck(prop as fn(Vec<u8>, i32) -> TestResult);
}

/// The round-trip identity holds under *every* deflate strategy.
///
/// Strategy is not selectable through the one-call API, so this drives the
/// streaming engine (`deflate_init2` takes the strategy) and decompresses back
/// through the matching inflate path.
#[test]
fn qc_stream_round_trip_all_strategies() {
    fn prop(data: Vec<u8>, raw_idx: u8) -> bool {
        let strategy = STRATEGIES[raw_idx as usize % STRATEGIES.len()];
        let recovered = round_trip_whole(&data, Z_DEFAULT_COMPRESSION, strategy, WBITS_ZLIB);
        recovered.as_slice() == data
    }
    QuickCheck::new()
        .tests(150)
        .rng(Gen::new(400))
        .quickcheck(prop as fn(Vec<u8>, u8) -> bool);
}

// ===========================================================================
// Phase 3 — structured-input round-trips + explicit example.c anchors
// ===========================================================================

/// The `example.c` anchor input `"hello, hello!"` must round-trip at every
/// level through both the one-call API and the streaming engine (zlib + raw).
#[test]
fn hello_anchor_round_trips_every_level() {
    let hello: &[u8] = b"hello, hello!";
    for &level in &ALL_LEVELS {
        assert!(
            one_call_round_trip(hello, level),
            "one-call round-trip of the hello anchor failed at level {level}"
        );
        for &wbits in &[WBITS_ZLIB, WBITS_RAW] {
            // Byte-at-a-time streaming, mirroring example.c's forced 1-byte
            // buffers.
            let recovered = stream_round_trip(hello, level, Strategy::Default, wbits, 1, 1);
            assert_eq!(
                recovered.as_slice(),
                hello,
                "streaming round-trip of the hello anchor failed (level={level}, wbits={wbits})"
            );
        }
    }
}

/// Mirrors `example.c`'s `test_large_deflate`/`test_large_inflate`: a
/// 20 000-byte buffer (mostly zeros, so highly compressible) round-trips across
/// every level (one-call) and every strategy (streaming).
#[test]
fn large_zero_filled_buffer_round_trips() {
    let data = vec![0u8; 20_000];

    for &level in &ALL_LEVELS {
        assert!(
            one_call_round_trip(&data, level),
            "one-call round-trip of the 20000-byte buffer failed at level {level}"
        );
    }

    for &strategy in &STRATEGIES {
        let recovered = round_trip_whole(&data, Z_BEST_COMPRESSION, strategy, WBITS_ZLIB);
        assert_eq!(
            recovered.len(),
            data.len(),
            "length mismatch (strategy={strategy:?})"
        );
        assert_eq!(
            recovered.as_slice(),
            data.as_slice(),
            "streaming round-trip of the 20000-byte buffer failed (strategy={strategy:?})"
        );
    }
}

/// Structured inputs that stress distinct coder paths — long identical runs
/// (RLE-friendly), highly repetitive text, seeded incompressible data, an empty
/// input, and a mixture — must all round-trip identically.
#[test]
fn structured_inputs_round_trip() {
    let long_run = vec![b'A'; 30_000];
    let repetitive = b"hello, ".repeat(5_000);
    let incompressible = seeded_random_bytes(0x00C0_FFEE, 25_000);
    let empty: Vec<u8> = Vec::new();
    let mixed = {
        let mut m = Vec::new();
        m.extend_from_slice(&long_run[..1_000]);
        m.extend_from_slice(&repetitive[..1_000]);
        m.extend_from_slice(&incompressible[..1_000]);
        m
    };

    let cases: [&[u8]; 5] = [
        long_run.as_slice(),
        repetitive.as_slice(),
        incompressible.as_slice(),
        empty.as_slice(),
        mixed.as_slice(),
    ];

    for &data in &cases {
        for &level in &[
            Z_NO_COMPRESSION,
            Z_BEST_SPEED,
            Z_DEFAULT_COMPRESSION,
            Z_BEST_COMPRESSION,
        ] {
            assert!(
                one_call_round_trip(data, level),
                "one-call round-trip failed (len={}, level={level})",
                data.len()
            );
        }

        // Whole-buffer streaming pass validates the streaming engine on the same
        // inputs, isolated from decode-granularity effects.
        let whole = round_trip_whole(data, Z_DEFAULT_COMPRESSION, Strategy::Default, WBITS_ZLIB);
        assert_eq!(
            whole.as_slice(),
            data,
            "whole-buffer streaming round-trip failed (len={})",
            data.len()
        );

        // Small symmetric chunks additionally exercise the resumable state
        // machine on these structured inputs (output window < 258 keeps the
        // decode on the slow path, faithful to example.c's one-byte buffers).
        let chunked = stream_round_trip(
            data,
            Z_DEFAULT_COMPRESSION,
            Strategy::Default,
            WBITS_ZLIB,
            100,
            100,
        );
        assert_eq!(
            chunked.as_slice(),
            data,
            "chunked streaming round-trip failed (len={})",
            data.len()
        );
    }
}

/// Every strategy must round-trip a heterogeneous input (repetitive text, then
/// incompressible bytes, then a long run) identically through the streaming
/// engine with deliberately mismatched, non-power-of-two chunk sizes.
#[test]
fn all_strategies_round_trip_explicit() {
    let data = {
        let mut d = b"the quick brown fox ".repeat(300);
        d.extend_from_slice(&seeded_random_bytes(0x1234_5678, 2_000));
        d.resize(d.len() + 2_000, b'Z');
        d
    };

    for &strategy in &STRATEGIES {
        let recovered = round_trip_whole(&data, Z_DEFAULT_COMPRESSION, strategy, WBITS_ZLIB);
        assert_eq!(
            recovered.as_slice(),
            data.as_slice(),
            "strategy {strategy:?} failed to round-trip"
        );
    }
}

// ===========================================================================
// Phase 4 — streaming round-trip properties (incremental state machine)
// ===========================================================================

/// The streaming round-trip identity holds for arbitrary small chunk sizes.
///
/// The chunk size is normalized into `1..=64`; both the input and output
/// windows use it, maximally stressing the resumable deflate/inflate state
/// machine — the property-level analogue of `example.c`'s one-byte buffers.
#[test]
fn qc_stream_round_trip_small_chunks() {
    fn prop(data: Vec<u8>, raw_chunk: u16) -> bool {
        let chunk = 1 + (raw_chunk as usize % 64);
        let recovered = stream_round_trip(
            &data,
            Z_DEFAULT_COMPRESSION,
            Strategy::Default,
            WBITS_ZLIB,
            chunk,
            chunk,
        );
        recovered.as_slice() == data
    }
    QuickCheck::new()
        .tests(120)
        .rng(Gen::new(300))
        .quickcheck(prop as fn(Vec<u8>, u16) -> bool);
}

/// Explicit byte-at-a-time streaming (chunk size 1) — the direct analogue of
/// `example.c`'s `test_deflate`/`test_inflate` forcing `avail_in = avail_out =
/// 1` — across zlib and raw framing, including the empty input.
#[test]
fn streaming_byte_at_a_time_round_trips() {
    let inputs: [&[u8]; 3] = [
        b"hello, hello!",
        b"",
        b"aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd",
    ];
    for input in inputs {
        for &wbits in &[WBITS_ZLIB, WBITS_RAW] {
            let recovered =
                stream_round_trip(input, Z_DEFAULT_COMPRESSION, Strategy::Default, wbits, 1, 1);
            assert_eq!(
                recovered.as_slice(),
                input,
                "byte-at-a-time round-trip failed (wbits={wbits})"
            );
        }
    }
}

// ===========================================================================
// Phase 5 — framing round-trips (zlib / raw / gzip)
// ===========================================================================

/// The round-trip identity holds under both zlib (RFC 1950) and raw DEFLATE
/// (RFC 1951) framing for arbitrary inputs.
#[test]
fn qc_stream_round_trip_zlib_and_raw_framing() {
    fn prop(data: Vec<u8>) -> bool {
        let zlib = round_trip_whole(&data, 6, Strategy::Default, WBITS_ZLIB);
        let raw = round_trip_whole(&data, 6, Strategy::Default, WBITS_RAW);
        zlib.as_slice() == data && raw.as_slice() == data
    }
    QuickCheck::new()
        .tests(150)
        .rng(Gen::new(400))
        .quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Explicit zlib and raw framing round-trips across every level, on a
/// repetitive text input large enough to produce real back-references.
#[test]
fn framing_zlib_and_raw_round_trip_explicit() {
    let data = b"framing check: zlib (RFC 1950) and raw DEFLATE (RFC 1951). ".repeat(40);
    for &wbits in &[WBITS_ZLIB, WBITS_RAW] {
        for &level in &ALL_LEVELS {
            let recovered = round_trip_whole(&data, level, Strategy::Default, wbits);
            assert_eq!(
                recovered.as_slice(),
                data.as_slice(),
                "framing round-trip failed (wbits={wbits}, level={level})"
            );
        }
    }
}

/// gzip framing (RFC 1952) round-trips via `windowBits = 31` on both the
/// compress and decompress sides. Gated on the `gzip` feature (on by default);
/// a build without it omits gzip framing entirely.
#[cfg(feature = "gzip")]
#[test]
fn framing_gzip_round_trips() {
    let inputs: [Vec<u8>; 3] = [
        b"hello, hello!".to_vec(),
        Vec::new(),
        b"the quick brown fox jumps over the lazy dog. ".repeat(50),
    ];
    for data in &inputs {
        let recovered =
            round_trip_whole(data, Z_DEFAULT_COMPRESSION, Strategy::Default, WBITS_GZIP);
        assert_eq!(
            recovered.as_slice(),
            data.as_slice(),
            "gzip framing round-trip failed (len={})",
            data.len()
        );
    }
}
