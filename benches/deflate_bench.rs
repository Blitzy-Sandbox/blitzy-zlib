//! DEFLATE compression-throughput benchmarks for the `zlib-rs` crate.
//!
//! This [criterion](https://docs.rs/criterion) harness measures how fast
//! `zlib_rs` compresses representative inputs and exposes the AAP §0.7.3
//! performance gate directly in the criterion report:
//!
//! > **Compression throughput must be ≥ 80% of C zlib.**
//!
//! To make that ratio readable at a glance, the crate-under-test and the
//! canonical C-zlib oracle are benchmarked over the *same* corpora at the
//! *same* levels and placed in the *same* criterion group (`deflate_levels`):
//!
//! * `deflate_levels/<corpus>_L<level>`        — `zlib_rs` (this crate)
//! * `deflate_levels/czlib_<corpus>_L<level>`  — C zlib via the `flate2`
//!   dev-dependency built with its `zlib` backend (the byte-compatible oracle)
//!
//! The gate is satisfied when, for each corpus/level pair,
//! `throughput(zlib_rs) / throughput(czlib) ≥ 0.80`.
//!
//! # Throughput convention
//!
//! Compression rate is *uncompressed* bytes processed per second, so every
//! group reports [`Throughput::Bytes`] sized by the **input** length
//! (`Throughput::Bytes(input.len())`), not the compressed output length.
//!
//! # Benchmark groups
//!
//! * [`bench_levels`] — the primary numbers, driven through the rock-solid
//!   one-shot [`compress2`] API across the representative levels `0/1/6/9`
//!   (store / best-speed / default / best-compression — the three engine code
//!   paths `deflate_stored` / `deflate_fast` / `deflate_slow`).
//! * [`bench_strategies`] — the five DEFLATE strategies at level 6 via the
//!   streaming [`Deflate`] API, so every `deflate_*` strategy routine is
//!   exercised.
//! * [`bench_baseline_czlib`] — the C-zlib reference numbers (the ≥ 80% gate
//!   denominator).
//!
//! The level matrix is deliberately bounded to `0/1/6/9` so a full
//! `cargo bench` run stays reasonable; those four points represent
//! store / fast / default / best respectively.
//!
//! Conceptual origin: `deflate.c` / `trees.c` at the repository root (the
//! compression engine being measured); the comparison oracle is C zlib via
//! `flate2`. `criterion` is the reference harness (AAP §0.4.1). This file is a
//! CREATE-from-scratch benchmark — it contains no `unsafe` and no `extern "C"`
//! and uses only the safe Rust public API (the `capi`/FFI feature is OFF for
//! benches, so the canonical C symbols do not collide with `flate2`'s C zlib).

use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};

// Primary crate-under-test surface. These all resolve at the crate root
// (re-exported by `src/lib.rs`): the one-shot `compress2` / `compress_bound` /
// `uncompress` helpers (`src/util/*`) and the streaming `Deflate` compressor
// with its typed `Strategy` / `FlushMode` / `ReturnCode` enums.
use zlib_rs::{Deflate, FlushMode, ReturnCode, Strategy, compress_bound, compress2, uncompress};

// ===========================================================================
// Fixed parameters
// ===========================================================================

/// Seed for every pseudo-random corpus, so benchmark inputs are byte-for-byte
/// reproducible across runs (and therefore comparable across commits).
const SEED: u64 = 0x5A4C_4942;

/// Primary input size: 1 MiB. Large enough to amortise per-call fixed costs and
/// to make the matcher/Huffman stages dominate the measured time.
const SIZE: usize = 1024 * 1024;

/// Representative compression levels. `0` = store (`deflate_stored`), `1` =
/// best-speed (`deflate_fast`), `6` = default (`deflate_slow`), `9` =
/// best-compression (`deflate_slow`, max chain). These four points cover all
/// three engine code paths while keeping the `cargo bench` matrix bounded.
const LEVELS: [i32; 4] = [0, 1, 6, 9];

/// The five DEFLATE strategies, exercised by [`bench_strategies`] so each
/// strategy routine (`deflate_slow`/`deflate_rle`/`deflate_huff` plus the
/// `Z_FILTERED`/`Z_FIXED` variants) is benchmarked at least once.
const STRATEGIES: [Strategy; 5] = [
    Strategy::Default,
    Strategy::Filtered,
    Strategy::HuffmanOnly,
    Strategy::Rle,
    Strategy::Fixed,
];

// ===========================================================================
// Deterministic corpora
// ===========================================================================

/// A highly compressible corpus: a fixed ASCII sentence repeated to fill
/// `SIZE`. Deterministic (no RNG). Mirrors the canonical zlib test input, which
/// uses a repeated phrase because repetition "stresses the compression code
/// better" (`test/example.c`). Best case for LZ77 matching.
fn highly_compressible() -> Vec<u8> {
    const PHRASE: &[u8] = b"the quick brown fox jumps over the lazy dog. ";
    let mut v = Vec::with_capacity(SIZE + PHRASE.len());
    while v.len() < SIZE {
        v.extend_from_slice(PHRASE);
    }
    v.truncate(SIZE);
    v
}

/// A text-like corpus: pseudo-English assembled by drawing words from a fixed
/// dictionary with a seeded PRNG. Deterministic for a fixed [`SEED`]; the word
/// stream has natural-language-like entropy (more varied than the repeated
/// phrase, far more structured than random bytes).
fn text_like() -> Vec<u8> {
    const WORDS: &[&str] = &[
        "the",
        "quick",
        "brown",
        "fox",
        "jumps",
        "over",
        "lazy",
        "dog",
        "zlib",
        "deflate",
        "inflate",
        "compression",
        "rust",
        "memory",
        "safe",
        "stream",
        "buffer",
        "window",
        "huffman",
        "checksum",
        "adler",
        "crc",
        "data",
        "byte",
        "block",
        "encode",
        "decode",
        "lossless",
        "format",
        "throughput",
    ];
    // A distinct seed offset keeps this corpus uncorrelated with the random one.
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x7E47_7E47);
    let mut v = Vec::with_capacity(SIZE + 16);
    while v.len() < SIZE {
        let idx = (rng.next_u32() as usize) % WORDS.len();
        v.extend_from_slice(WORDS[idx].as_bytes());
        v.push(b' ');
    }
    v.truncate(SIZE);
    v
}

/// An incompressible corpus: unstructured bytes from a seeded PRNG.
/// Deterministic for a fixed [`SEED`]. This is the matcher's worst case — the
/// DEFLATE output grows slightly past the input (stored-block fallback), which
/// stresses the block-type decision and the wrapper/checksum overhead.
fn incompressible() -> Vec<u8> {
    let mut v = vec![0u8; SIZE];
    StdRng::seed_from_u64(SEED).fill_bytes(&mut v);
    v
}

/// Assemble the labelled corpus set shared by the level-sweep groups. Built
/// once per benchmark function, entirely outside the timed sections.
fn build_corpora() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("repeated", highly_compressible()),
        ("text", text_like()),
        ("random", incompressible()),
    ]
}

// ===========================================================================
// Correctness guard (untimed)
// ===========================================================================

/// Verify, *outside* any timed region, that the encoder under test actually
/// works: it must produce a non-empty stream that round-trips back to the
/// original input. This guards against silently benchmarking a broken codec
/// (e.g. one that emits zero bytes or corrupts data) and reporting a
/// meaningless throughput number.
fn assert_codec_sane(data: &[u8]) {
    let packed = compress2(data, 6).expect("compress2 at level 6 must succeed");
    assert!(!packed.is_empty(), "compressed output must be non-empty");

    let mut restored = vec![0u8; data.len()];
    let produced = uncompress(&mut restored, &packed).expect("uncompress must succeed");
    assert_eq!(produced, data.len(), "round-trip length mismatch");
    assert_eq!(&restored[..produced], data, "round-trip content mismatch");
}

// ===========================================================================
// flate2 (C zlib) level mapping
// ===========================================================================

/// Map a zlib integer level to a [`flate2::Compression`] for the C-zlib oracle.
///
/// * `< 0` (`Z_DEFAULT_COMPRESSION`, i.e. `-1`) → the default level (6).
/// * `0` → no compression (store).
/// * `1..=9` → that level (clamped defensively in case [`LEVELS`] is extended).
fn czlib_compression(level: i32) -> Compression {
    if level < 0 {
        Compression::default()
    } else if level == 0 {
        Compression::none()
    } else {
        Compression::new(level.clamp(1, 9) as u32)
    }
}

// ===========================================================================
// Group 1 — level-sweep throughput (PRIMARY, one-shot `compress2`)
// ===========================================================================

/// Benchmark `zlib_rs` compression throughput across [`LEVELS`] and every
/// corpus, using the one-shot [`compress2`] entry point.
///
/// `compress2` allocates its own output `Vec` sized via [`compress_bound`];
/// that allocation is part of the measured one-shot cost. This is acceptable
/// and representative — it is exactly what a caller invoking `compress2` pays —
/// and keeps these primary numbers on the rock-solid one-shot API. The
/// streaming, buffer-reuse path is measured separately by [`bench_strategies`].
fn bench_levels(c: &mut Criterion) {
    let corpora = build_corpora();

    // Untimed sanity check: every corpus must round-trip before we time it.
    for (_name, data) in &corpora {
        assert_codec_sane(data);
    }

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        // Compression rate is reported against the *uncompressed* input size.
        group.throughput(Throughput::Bytes(data.len() as u64));
        for level in LEVELS {
            let id = BenchmarkId::new(format!("{name}_L{level}"), data.len());
            group.bench_with_input(id, data.as_slice(), |b, input| {
                b.iter(|| {
                    let out = compress2(black_box(input), level).expect("compress2");
                    black_box(out)
                });
            });
        }
    }
    group.finish();

    // GATE (AAP §0.7.3): each `deflate_levels/<name>_L<level>` throughput here
    // must be ≥ 80% of the matching `deflate_levels/czlib_<name>_L<level>`
    // baseline produced by `bench_baseline_czlib`.
}

// ===========================================================================
// Group 2 — strategy coverage (streaming `Deflate`, single Finish)
// ===========================================================================

/// Benchmark all five [`STRATEGIES`] at a fixed level (6), window (15) and
/// memory level (8) over the text-like corpus, using the streaming [`Deflate`]
/// API so each strategy routine is exercised.
///
/// The output buffer is sized once via [`compress_bound`] and hoisted *out* of
/// the timed loop so these numbers measure compression rather than per-call
/// allocation (the level sweep above already covers the allocate-each-call
/// cost). A `dest` of `compress_bound(len)` guarantees the single `Finish`
/// completes in one call — this is exactly what the crate's own
/// `compress2_to_buf` relies on. The cursor loop below is therefore purely
/// defensive: it advances the input/output offsets and re-enters `compress`
/// only if the engine ever returns before [`ReturnCode::StreamEnd`], and bails
/// out if no forward progress is made so the benchmark can never hang.
fn bench_strategies(c: &mut Criterion) {
    let data = text_like();
    assert_codec_sane(&data);

    let mut group = c.benchmark_group("deflate_strategies");
    group.throughput(Throughput::Bytes(data.len() as u64));
    for strat in STRATEGIES {
        group.bench_with_input(
            BenchmarkId::new("strategy", format!("{strat:?}")),
            data.as_slice(),
            |b, input| {
                // Allocated once, reused across iterations: we only ever read
                // back `out_off` bytes, so stale tail data is irrelevant.
                let cap = compress_bound(input.len());
                let mut out = vec![0u8; cap];
                b.iter(|| {
                    let mut d =
                        Deflate::with_options(6, 15, 8, strat).expect("deflate init must succeed");
                    let mut in_off = 0usize;
                    let mut out_off = 0usize;
                    loop {
                        let outcome = d
                            .compress(
                                black_box(&input[in_off..]),
                                &mut out[out_off..],
                                FlushMode::Finish,
                            )
                            .expect("deflate must succeed");
                        in_off += outcome.consumed;
                        out_off += outcome.produced;
                        if outcome.code == ReturnCode::StreamEnd {
                            break;
                        }
                        // Defensive: with `compress_bound` sizing this is
                        // unreachable, but never spin if the stream stalls.
                        if outcome.consumed == 0 && outcome.produced == 0 {
                            break;
                        }
                    }
                    black_box(out_off)
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Group 3 — C-zlib baseline (the ≥ 80% gate denominator)
// ===========================================================================

/// Benchmark canonical C zlib (via `flate2`'s `zlib` backend) over the *same*
/// corpora and levels as [`bench_levels`], inside the *same* `deflate_levels`
/// group. Placing both side by side lets the ≥ 80% gate be read as a direct
/// ratio of the `<name>_L<level>` (this crate) to `czlib_<name>_L<level>`
/// (C zlib) entries.
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        group.throughput(Throughput::Bytes(data.len() as u64));
        for level in LEVELS {
            let comp = czlib_compression(level);
            let id = BenchmarkId::new(format!("czlib_{name}_L{level}"), data.len());
            group.bench_with_input(id, data.as_slice(), |b, input| {
                b.iter(|| {
                    let mut e = ZlibEncoder::new(Vec::new(), comp);
                    e.write_all(black_box(input))
                        .expect("write to Vec is infallible");
                    black_box(e.finish().expect("flate2 finish"))
                });
            });
        }
    }
    group.finish();
}

// ===========================================================================
// Harness wiring — criterion provides `main` (Cargo `[[bench]] harness = false`)
// ===========================================================================

criterion_group!(
    benches,
    bench_levels,
    bench_strategies,
    bench_baseline_czlib
);
criterion_main!(benches);
