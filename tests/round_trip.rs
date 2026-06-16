//! Property-based round-trip tests for `zlib-rs`, driven by `quickcheck`.
//!
//! Where [`tests/regression.rs`] ports the fixed cases from the upstream
//! `test/example.c`, this suite generalizes `example.c`'s round-trip behavior
//! into *properties* that must hold for **every** input: whatever the data,
//! the compression level, the strategy, or the window configuration, anything
//! this crate compresses must decompress back to exactly the original bytes.
//! `quickcheck` searches the input space (and shrinks any counterexample to a
//! minimal witness), complementing the deterministic regression vectors with
//! broad, randomized coverage (AAP §0.4.1: "quickcheck property round-trips";
//! tech-spec §6.6: property-based tests verify round-trip correctness).
//!
//! # What is exercised
//!
//! * **One-shot path** — [`compress2`] → [`uncompress`], the in-memory
//!   convenience API, across all ten levels plus `Z_DEFAULT_COMPRESSION`.
//! * **Streaming path** — the [`Deflate`]/[`Inflate`] engines, across all five
//!   [`Strategy`] values and both the zlib and raw framings.
//! * **Chunked feeding** — both engines fed in arbitrarily small slices, which
//!   stresses the partial-progress (`consumed`/`produced`) bookkeeping.
//! * **Determinism and the size bound** — identical input ⇒ identical output,
//!   and [`compress_bound`] is always a sufficient upper bound.
//!
//! # API asymmetry honored here
//!
//! The two engines expose deliberately different shapes, and these tests use
//! each as designed:
//!
//! * [`Deflate::compress`] takes a [`FlushMode`] enum and returns a
//!   `Result<DeflateOutcome, _>` whose `code`/`consumed`/`produced` fields
//!   describe the call.
//! * [`Inflate::inflate`] takes a raw `i32` flush and returns the bare tuple
//!   `(ret, consumed, produced)`.
//!
//! # Window-framing coverage
//!
//! Only the zlib (positive `windowBits`, 8..=15) and raw (negative, -15..=-8)
//! framings are exercised here, since both are available regardless of the
//! crate's feature set. The gzip framing (`windowBits` 24..=31 / 40..=47)
//! depends on the `gzip` feature and is covered by the gzip-focused suites
//! (`tests/interop.rs`, `tests/gzip_compat.rs`), so it is intentionally left
//! out of this file to keep it feature-independent (per the file's design
//! note in AAP §0.4.1).
//!
//! [`tests/regression.rs`]: ./regression.rs

// This is a pure-safe test module: there is no FFI and no raw-pointer work, so
// forbid `unsafe` outright to mechanically guarantee the "no unsafe" constraint
// (AAP §0.6.2) for everything in this file.
#![forbid(unsafe_code)]

use quickcheck::{TestResult, quickcheck};
use rand::Rng;
use zlib_rs::util::compress::{compress_bound, compress2};
use zlib_rs::util::uncompress::uncompress;
use zlib_rs::{
    DEF_MEM_LEVEL, Deflate, FlushMode, Inflate, MAX_WBITS, ReturnCode, Strategy,
    Z_DEFAULT_COMPRESSION, Z_NO_FLUSH, Z_OK, Z_STREAM_END,
};

// ===========================================================================
// Private helpers (no `pub`, no shared module — all local to this test file).
// ===========================================================================

/// One-shot round-trip identity: compress `data` at `level` with [`compress2`]
/// and decompress it with [`uncompress`] into an exactly-sized buffer, then
/// check the recovered bytes equal the original.
///
/// The decompressed length is the original `data.len()` — known out-of-band, as
/// the C `uncompress` contract requires — so the destination is sized to it
/// exactly. An empty input is handled naturally: a zero-length destination is
/// valid and `uncompress` of an empty-payload stream yields zero bytes.
fn oneshot_roundtrip_ok(data: &[u8], level: i32) -> bool {
    let packed = match compress2(data, level) {
        Ok(packed) => packed,
        Err(_) => return false,
    };
    let mut restored = vec![0u8; data.len()];
    match uncompress(&mut restored, &packed) {
        Ok(produced) => produced == data.len() && restored[..produced] == *data,
        Err(_) => false,
    }
}

/// Compress the whole of `data` through the streaming [`Deflate`] engine with a
/// single [`FlushMode::Finish`] drive loop, returning the complete stream.
///
/// `wb` selects the framing: positive values request the zlib wrapper, negative
/// values request raw DEFLATE (the `windowBits` overloading convention). The
/// output buffer is sized with [`compress_bound`] — which is computed for the
/// zlib wrapper and therefore bounds the raw framing too, since raw has no
/// wrapper/trailer overhead — plus a small margin so a single `Finish`
/// completes; the loop nonetheless grows the buffer defensively if it ever
/// fills before the stream ends.
///
/// Panics (failing the test with a clear message) if the engine rejects the
/// options or returns an error for a valid buffer, both of which would be bugs.
fn deflate_all(data: &[u8], level: i32, strategy: Strategy, wb: i32) -> Vec<u8> {
    let mut engine = Deflate::with_options(level, wb, DEF_MEM_LEVEL, strategy)
        .expect("Deflate::with_options should accept valid level/window/strategy");
    let mut out = vec![0u8; compress_bound(data.len()) + 64];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let outcome = engine
            .compress(&data[in_pos..], &mut out[out_pos..], FlushMode::Finish)
            .expect("deflate must not error with a sufficiently sized buffer");
        in_pos += outcome.consumed;
        out_pos += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        // Never expected with a bound-sized buffer, but keep the stream able to
        // complete if the output somehow filled first.
        if out_pos == out.len() {
            out.resize(out.len() * 2, 0);
        }
    }
    out.truncate(out_pos);
    out
}

/// Decompress a complete `packed` stream produced for window configuration `wb`
/// through the streaming [`Inflate`] engine, returning the recovered bytes, or
/// [`None`] if the engine reported an error or stalled with no progress.
///
/// `wb` must match the framing the stream was produced with (raw↔raw,
/// zlib↔zlib). `expected_len` sizes the output buffer to the known original
/// length. Each call is given the entire remaining input and output, looping
/// until [`Z_STREAM_END`].
fn inflate_all(packed: &[u8], wb: i32, expected_len: usize) -> Option<Vec<u8>> {
    let mut engine = Inflate::with_window_bits(wb).ok()?;
    let mut out = vec![0u8; expected_len];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let (ret, in_used, out_used) =
            engine.inflate(&packed[in_pos..], &mut out[out_pos..], Z_NO_FLUSH);
        in_pos += in_used;
        out_pos += out_used;
        if ret == Z_STREAM_END {
            break;
        }
        if ret != Z_OK {
            return None;
        }
        // A `Z_OK` that made no forward progress cannot make progress on a
        // further identical call; treat it as a failure rather than spin.
        if in_used == 0 && out_used == 0 {
            return None;
        }
    }
    out.truncate(out_pos);
    Some(out)
}

/// Streaming round-trip identity: [`deflate_all`] then [`inflate_all`] under the
/// same framing, checking the recovered bytes equal `data`.
fn stream_roundtrip_ok(data: &[u8], level: i32, strategy: Strategy, wb: i32) -> bool {
    let packed = deflate_all(data, level, strategy, wb);
    match inflate_all(&packed, wb, data.len()) {
        Some(restored) => restored.as_slice() == data,
        None => false,
    }
}

/// Round-trip identity when the deflater is fed in `chunk`-sized input slices
/// with [`FlushMode::NoFlush`], then finished. Exercises the consumed/produced
/// bookkeeping across many small `deflate` calls (the small-buffer pattern from
/// `test/example.c`'s `test_deflate`), decoding the assembled stream in one shot.
fn chunked_deflate_ok(data: &[u8], chunk: usize) -> bool {
    let chunk = chunk.max(1);
    let mut engine = Deflate::new(6).expect("level 6 is valid");
    let mut out = vec![0u8; compress_bound(data.len()) + 64];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;

    // Feed the input in chunks with NoFlush; the bound-sized output buffer means
    // each NoFlush call consumes its whole chunk.
    while in_pos < data.len() {
        let end = (in_pos + chunk).min(data.len());
        let outcome = engine
            .compress(&data[in_pos..end], &mut out[out_pos..], FlushMode::NoFlush)
            .expect("deflate NoFlush must not error");
        in_pos += outcome.consumed;
        out_pos += outcome.produced;
        if out_pos == out.len() {
            out.resize(out.len() + compress_bound(data.len()) + 64, 0);
        }
        // A NoFlush call on a non-empty chunk with output room always consumes
        // at least one byte; a zero-consumption call would be a stall (bug).
        if outcome.consumed == 0 {
            return false;
        }
    }

    // Finish the stream (no further input).
    loop {
        let outcome = engine
            .compress(&[], &mut out[out_pos..], FlushMode::Finish)
            .expect("deflate Finish must not error");
        out_pos += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        if out_pos == out.len() {
            out.resize(out.len() + 64, 0);
        }
    }
    out.truncate(out_pos);

    let mut restored = vec![0u8; data.len()];
    match uncompress(&mut restored, &out) {
        Ok(produced) => produced == data.len() && restored[..produced] == *data,
        Err(_) => false,
    }
}

/// Round-trip identity when a valid stream is inflated through a `chunk`-sized
/// output window while the full remaining input is always offered. Exercises
/// the inflate-side output bookkeeping (the small-buffer pattern from
/// `test/example.c`'s `test_inflate`).
///
/// Offering the full input avoids any deadlock: once all `data.len()` bytes are
/// produced, the output window is momentarily empty, but `inflate` can still
/// consume the trailer from the available input and report [`Z_STREAM_END`].
fn chunked_inflate_ok(data: &[u8], chunk: usize) -> bool {
    let chunk = chunk.max(1);
    let packed = match compress2(data, 6) {
        Ok(packed) => packed,
        Err(_) => return false,
    };
    let mut engine = Inflate::new().expect("default inflate init");
    let mut restored = vec![0u8; data.len()];
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let out_end = (out_pos + chunk).min(restored.len());
        let (ret, in_used, out_used) = engine.inflate(
            &packed[in_pos..],
            &mut restored[out_pos..out_end],
            Z_NO_FLUSH,
        );
        in_pos += in_used;
        out_pos += out_used;
        if ret == Z_STREAM_END {
            break;
        }
        if ret != Z_OK {
            return false;
        }
        if in_used == 0 && out_used == 0 {
            return false;
        }
    }
    out_pos == data.len() && restored[..out_pos] == *data
}

// ===========================================================================
// Section A — One-shot compress → uncompress identity (the headline property)
// ===========================================================================
//
// Each `#[test]` pins a level inside a local `fn` (quickcheck's `Testable` is
// implemented for bare `fn` pointers, not closures), then runs the identity
// property over 100 generated inputs per level.

/// `Z_DEFAULT_COMPRESSION` (-1, resolves to level 6).
#[test]
fn roundtrip_default() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, Z_DEFAULT_COMPRESSION)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Level 0 — `Z_NO_COMPRESSION` (stored blocks only).
#[test]
fn roundtrip_level_0() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, 0)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Level 1 — `Z_BEST_SPEED` (fast strategy).
#[test]
fn roundtrip_level_1() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, 1)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Level 2 — still in the `deflate_fast` band.
#[test]
fn roundtrip_level_2() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, 2)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Level 6 — the default (`deflate_slow` lazy matching).
#[test]
fn roundtrip_level_6() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, 6)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Level 9 — `Z_BEST_COMPRESSION` (maximum effort).
#[test]
fn roundtrip_level_9() {
    fn prop(data: Vec<u8>) -> bool {
        oneshot_roundtrip_ok(&data, 9)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// The identity must hold for *any* valid level. A generated `i8` is filtered
/// to the valid `-1..=9` range with [`TestResult::discard`] so only in-range
/// levels are asserted (out-of-range values are rejected up front by
/// `Deflate::new` and are not the subject of this property).
#[test]
fn roundtrip_arbitrary_level() {
    fn prop(data: Vec<u8>, lvl: i8) -> TestResult {
        let level = i32::from(lvl);
        if !(-1..=9).contains(&level) {
            return TestResult::discard();
        }
        TestResult::from_bool(oneshot_roundtrip_ok(&data, level))
    }
    quickcheck(prop as fn(Vec<u8>, i8) -> TestResult);
}

// ===========================================================================
// Section B — Streaming deflate → inflate identity across strategies & windows
// ===========================================================================
//
// Strategy never changes the *correctness* of the output, only how matches and
// Huffman codes are chosen, so every strategy must still round-trip. Each test
// uses the default level (6) and the default zlib window unless the test name
// says otherwise.

/// `Strategy::Default`.
#[test]
fn stream_roundtrip_strategy_default() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Default, MAX_WBITS)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// `Strategy::Filtered`.
#[test]
fn stream_roundtrip_strategy_filtered() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Filtered, MAX_WBITS)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// `Strategy::HuffmanOnly` (no string matching).
#[test]
fn stream_roundtrip_strategy_huffman_only() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::HuffmanOnly, MAX_WBITS)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// `Strategy::Rle` (match distances limited to one).
#[test]
fn stream_roundtrip_strategy_rle() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Rle, MAX_WBITS)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// `Strategy::Fixed` (fixed Huffman codes, no dynamic trees).
#[test]
fn stream_roundtrip_strategy_fixed() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Fixed, MAX_WBITS)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// zlib framing with the maximum 32 KiB window (`windowBits` = 15).
#[test]
fn stream_roundtrip_window_zlib15() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Default, 15)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// zlib framing with a small 512-byte window (`windowBits` = 9).
#[test]
fn stream_roundtrip_window_zlib9() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Default, 9)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Raw DEFLATE (no wrapper/trailer) with the maximum window (`windowBits` = -15).
#[test]
fn stream_roundtrip_window_raw15() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Default, -15)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

/// Raw DEFLATE with a small 512-byte window (`windowBits` = -9).
#[test]
fn stream_roundtrip_window_raw9() {
    fn prop(data: Vec<u8>) -> bool {
        stream_roundtrip_ok(&data, 6, Strategy::Default, -9)
    }
    quickcheck(prop as fn(Vec<u8>) -> bool);
}

// ===========================================================================
// Section C — Chunked-feeding invariance (streaming robustness)
// ===========================================================================
//
// The chunk size is generated and clamped to >= 1 inside the helpers, so these
// properties sweep many feed granularities over many inputs.

/// Feeding the deflater in arbitrarily small input chunks must still produce a
/// stream that decompresses back to the original.
#[test]
fn chunked_deflate() {
    fn prop(data: Vec<u8>, chunk: u16) -> bool {
        chunked_deflate_ok(&data, chunk as usize)
    }
    quickcheck(prop as fn(Vec<u8>, u16) -> bool);
}

/// Inflating through an arbitrarily small output window must reconstruct the
/// original bytes.
#[test]
fn chunked_inflate() {
    fn prop(data: Vec<u8>, chunk: u16) -> bool {
        chunked_inflate_ok(&data, chunk as usize)
    }
    quickcheck(prop as fn(Vec<u8>, u16) -> bool);
}

// ===========================================================================
// Section D — Determinism & size-bound properties
// ===========================================================================

/// Compressing the same input at the same level twice must yield byte-identical
/// output — the determinism that underpins zlib's byte-for-byte reproducibility.
/// The level is derived from a generated `u8` mapped onto the valid `-1..=9`
/// range so every case is exercised without discards.
#[test]
fn deterministic() {
    fn prop(data: Vec<u8>, lvl: u8) -> bool {
        let level = (i32::from(lvl) % 11) - 1; // 0..=255 -> -1..=9
        match (compress2(&data, level), compress2(&data, level)) {
            (Ok(first), Ok(second)) => first == second,
            _ => false,
        }
    }
    quickcheck(prop as fn(Vec<u8>, u8) -> bool);
}

/// [`compress_bound`] must always be at least the actual compressed size, for
/// every input and level. (`compress2` sizes its own buffer with
/// `compress_bound`, so this also confirms that contract holds end to end.)
#[test]
fn compress_bound_sufficient() {
    fn prop(data: Vec<u8>, lvl: u8) -> bool {
        let level = (i32::from(lvl) % 11) - 1; // 0..=255 -> -1..=9
        match compress2(&data, level) {
            Ok(packed) => packed.len() <= compress_bound(data.len()),
            Err(_) => false,
        }
    }
    quickcheck(prop as fn(Vec<u8>, u8) -> bool);
}

/// The bound for an empty input is the constant wrapper/overhead term, 13 bytes
/// (the exact zlib `compressBound(0)` value).
#[test]
fn compress_bound_zero() {
    assert_eq!(compress_bound(0), 13);
}

// ===========================================================================
// Fixed-vector edge cases (deterministic boundary inputs)
// ===========================================================================
//
// These pin down the corners the random generator reaches rarely or never:
// the empty input, a single byte, an input that crosses the 32 KiB window
// boundary, and genuinely incompressible random data. Each is run through both
// the one-shot and streaming (zlib + raw) paths.

/// Empty input must round-trip through every path.
#[test]
fn empty() {
    let data: &[u8] = b"";
    assert!(oneshot_roundtrip_ok(data, Z_DEFAULT_COMPRESSION));
    assert!(oneshot_roundtrip_ok(data, 0));
    assert!(oneshot_roundtrip_ok(data, 9));
    assert!(stream_roundtrip_ok(data, 6, Strategy::Default, MAX_WBITS));
    assert!(stream_roundtrip_ok(data, 6, Strategy::Default, -15));
    assert!(chunked_deflate_ok(data, 1));
    assert!(chunked_inflate_ok(data, 1));
}

/// A single byte must round-trip through every path.
#[test]
fn single_byte() {
    let data: &[u8] = b"Z";
    for level in [Z_DEFAULT_COMPRESSION, 0, 1, 6, 9] {
        assert!(oneshot_roundtrip_ok(data, level), "level {level}");
    }
    assert!(stream_roundtrip_ok(data, 6, Strategy::Default, MAX_WBITS));
    assert!(stream_roundtrip_ok(data, 6, Strategy::Fixed, -15));
    assert!(chunked_deflate_ok(data, 1));
    assert!(chunked_inflate_ok(data, 1));
}

/// A highly repetitive 70 KiB buffer crosses the 32 KiB window boundary,
/// exercising long back-references and window wrap-around. Run through every
/// level, both framings, and the chunked paths.
#[test]
fn highly_repetitive() {
    let data = vec![b'a'; 70_000];
    for level in 0..=9 {
        assert!(oneshot_roundtrip_ok(&data, level), "level {level}");
    }
    assert!(oneshot_roundtrip_ok(&data, Z_DEFAULT_COMPRESSION));
    for &wb in &[15, 9, -15, -9] {
        assert!(
            stream_roundtrip_ok(&data, 6, Strategy::Default, wb),
            "windowBits {wb}"
        );
    }
    // Cross window boundaries while feeding in odd-sized chunks.
    assert!(chunked_deflate_ok(&data, 333));
    assert!(chunked_inflate_ok(&data, 257));
}

/// Genuinely incompressible random data (where stored blocks dominate and the
/// output may slightly exceed the input) must still round-trip, and must stay
/// within [`compress_bound`]. Uses the rand 0.9 API (`rand::rng()` + `fill`).
#[test]
fn incompressible_random() {
    let mut rng = rand::rng();
    let mut data = vec![0u8; 8_192];
    rng.fill(&mut data[..]);

    for level in [Z_DEFAULT_COMPRESSION, 0, 1, 6, 9] {
        assert!(oneshot_roundtrip_ok(&data, level), "level {level}");
        let packed = compress2(&data, level).expect("valid level compresses");
        assert!(
            packed.len() <= compress_bound(data.len()),
            "compressed size exceeded compress_bound at level {level}"
        );
    }
    assert!(stream_roundtrip_ok(&data, 6, Strategy::Default, MAX_WBITS));
    assert!(stream_roundtrip_ok(&data, 6, Strategy::Default, -15));
    assert!(chunked_deflate_ok(&data, 64));
    assert!(chunked_inflate_ok(&data, 64));
}
