//! DEFLATE compression-throughput benchmarks for the `zlib_rs` crate.
//!
//! This is a [criterion] harness (the REFERENCE harness mandated by AAP sec. 0.4.1;
//! there is no 1:1 C source). It measures the throughput of `zlib_rs`'s DEFLATE
//! encoder -- conceptually the `deflate.c` / `trees.c` engine -- across the
//! representative compression levels, the five [`Strategy`] variants, and a set
//! of deterministic corpora, and it demonstrates the AAP sec. 0.7.3 performance
//! gate:
//!
//! > **compression throughput >= 80 % of C zlib.**
//!
//! To make that gate directly readable from the criterion report, the
//! [`bench_baseline_czlib`] group compresses the *same* corpora at the *same*
//! levels through canonical **C zlib** via the `flate2` dev-dependency (built
//! with its `zlib` feature, which links `libz-sys`). Its results land in the
//! same `deflate_levels` benchmark group as the `zlib_rs` results so that, for
//! every corpus/level pair, the ratio
//!
//! ```text
//! deflate_levels/<name>_L<level>        (zlib_rs)
//! -----------------------------------------------  >= 0.80
//! deflate_levels/czlib_<name>_L<level>  (C zlib)
//! ```
//!
//! can be read off side by side.
//!
//! # Throughput convention
//!
//! Compression throughput is reported against the **uncompressed input** length
//! (`Throughput::Bytes(input.len())`), i.e. uncompressed bytes processed per
//! second -- the natural rate for a compressor and the basis of the >= 80 % gate.
//!
//! # API surface used
//!
//! The primary, "rock-solid" numbers come from the one-shot
//! [`compress2`](zlib_rs::util::compress2): it owns its allocation and reaches
//! `StreamEnd` in a single call, so it cannot drift in shape. The streaming
//! [`Deflate`] API is used only for [`Strategy`] coverage, where a per-strategy
//! handle is required. Output buffers for the streaming path are sized with the
//! stable [`compress_bound`](zlib_rs::util::compress_bound) `const fn` rather
//! than `Deflate::bound` (whose signature has drifted across drafts).
//!
//! Note on import paths: the one-shot helpers resolve under `zlib_rs::util::`
//! (re-exported there by `src/util/mod.rs`); the streaming `Deflate`/`FlushMode`
//! /`Strategy`/`ReturnCode` items resolve at the crate root. This matches the
//! actual compiled crate.
//!
//! # Constraints honored
//!
//! * **No `unsafe`** and **no `extern "C"`** -- only the safe Rust public API
//!   (the `capi`/FFI feature is off for benches, so there is no symbol
//!   collision with `flate2`'s C zlib).
//! * Deterministic corpora built from a fixed [`SEED`] for reproducible runs.
//! * The level matrix is kept small ([`LEVELS`]) to bound `cargo bench` runtime
//!   while still spanning store / best-speed / default / best-compression.

use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use zlib_rs::util::{compress_bound, compress2};
use zlib_rs::{Deflate, FlushMode, ReturnCode, Strategy};

// ===========================================================================
// Benchmark configuration
// ===========================================================================

/// Fixed PRNG seed so every run builds byte-identical corpora -- essential for
/// the `zlib_rs`-vs-C-zlib ratio to compare the two encoders on the same input.
const SEED: u64 = 0x5A4C_4942;

/// Primary corpus size: 1 MiB. Large enough that per-call fixed costs are
/// amortized and the throughput number reflects steady-state encoding.
const SIZE: usize = 1024 * 1024;

/// Representative compression levels swept by [`bench_levels`] and mirrored by
/// [`bench_baseline_czlib`]. These four points stand in for the full `0..=9`
/// range -- `0` = stored (no compression), `1` = best speed (`deflate_fast`),
/// `6` = default (`deflate_slow`), `9` = best compression -- keeping the matrix
/// (and therefore `cargo bench` wall-time) bounded while still exercising every
/// distinct strategy path.
const LEVELS: [i32; 4] = [0, 1, 6, 9];

// ===========================================================================
// Deterministic corpora
// ===========================================================================

/// Highly compressible input: a fixed ASCII phrase repeated to fill [`SIZE`].
///
/// Mirrors the `test/example.c` choice of a repeated phrase (`"hello, hello!"`)
/// that deliberately stresses the LZ77 matcher with long, frequent matches.
/// No RNG is involved, so the result is deterministic by construction.
fn highly_compressible() -> Vec<u8> {
    const PHRASE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";
    let mut v = Vec::with_capacity(SIZE + PHRASE.len());
    while v.len() < SIZE {
        v.extend_from_slice(PHRASE);
    }
    v.truncate(SIZE);
    v
}

/// Text-like input: pseudo-English assembled by drawing words from a fixed
/// vocabulary with a seeded PRNG, joined by spaces, until [`SIZE`] is reached.
///
/// Deterministic via [`SEED`]; representative of natural-language/source-code
/// data where the matcher finds frequent but short, scattered matches.
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
        "pack",
        "my",
        "box",
        "with",
        "five",
        "dozen",
        "liquor",
        "jugs",
        "how",
        "razorback",
        "frogs",
        "can",
        "level",
        "six",
        "piqued",
        "gymnasts",
        "compression",
        "deflate",
        "inflate",
        "stream",
        "buffer",
        "window",
        "huffman",
        "checksum",
        "throughput",
        "benchmark",
        "encoder",
        "decoder",
    ];
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut v = Vec::with_capacity(SIZE + 16);
    while v.len() < SIZE {
        // `next_u32` (RngCore) gives a deterministic index into the vocabulary.
        let word = WORDS[(rng.next_u32() as usize) % WORDS.len()];
        v.extend_from_slice(word.as_bytes());
        v.push(b' ');
    }
    v.truncate(SIZE);
    v
}

/// Incompressible input: [`SIZE`] bytes of seeded pseudo-random data -- the
/// worst case for the matcher (no useful matches, so output ~= input + overhead).
///
/// Deterministic via [`SEED`] so the random bytes are identical on every run.
fn incompressible() -> Vec<u8> {
    let mut v = vec![0u8; SIZE];
    let mut rng = StdRng::seed_from_u64(SEED);
    rng.fill_bytes(&mut v);
    v
}

/// Build the labeled corpus set once (outside any timed region). The labels
/// become part of the criterion benchmark IDs.
fn build_corpora() -> [(&'static str, Vec<u8>); 3] {
    [
        ("repeated", highly_compressible()),
        ("text", text_like()),
        ("random", incompressible()),
    ]
}

// ===========================================================================
// Streaming-deflate helper (Strategy coverage)
// ===========================================================================

/// Compress `input` into `output` through the streaming [`Deflate`] API at a
/// fixed level (6) / window (15) / memory level (8) with the given `strategy`,
/// returning the total number of bytes produced.
///
/// `output` must be sized with [`compress_bound`](zlib_rs::util::compress_bound)
/// so the whole stream fits; given such a buffer a single
/// [`FlushMode::Finish`] completes the stream in one call. The surrounding loop
/// is **defensive**: it keeps calling `compress` -- advancing the input/output
/// cursors -- until [`ReturnCode::StreamEnd`], so the benchmark stays correct
/// even if some future build chooses to buffer output internally and return
/// `Ok` before finishing. The loop is bounded by a hard progress assertion: a
/// nonterminal call that makes no forward progress (unreachable with a
/// `compress_bound`-sized buffer) fails the benchmark rather than silently
/// returning a partial byte count.
///
/// The [`Deflate`] handle is constructed inside this helper (and therefore
/// inside the timed body when called from `b.iter`); its initialization and the
/// caller's buffer allocation are part of the measured per-iteration cost -- a
/// deliberate, representative choice that is identical across strategies, so the
/// relative comparison between strategies remains fair.
fn deflate_finish(input: &[u8], output: &mut [u8], strategy: Strategy) -> usize {
    let mut deflate = Deflate::with_options(6, 15, 8, strategy).expect("deflate init");
    let mut consumed = 0usize;
    let mut produced = 0usize;
    loop {
        let outcome = deflate
            .compress(
                &input[consumed..],
                &mut output[produced..],
                FlushMode::Finish,
            )
            .expect("deflate");
        consumed += outcome.consumed;
        produced += outcome.produced;
        if outcome.code == ReturnCode::StreamEnd {
            break;
        }
        // Hard (release-mode) gate: a nonterminal Finish call MUST make forward
        // progress. With a `compress_bound`-sized `output` a single Finish
        // completes the stream, so a stall here is a bug. Assert rather than
        // silently `break`-ing with a partial byte count that would then be
        // benchmarked as a fast "success" in optimized criterion runs.
        assert!(
            outcome.consumed != 0 || outcome.produced != 0,
            "deflate_finish stalled with no progress before StreamEnd"
        );
    }
    produced
}

// ===========================================================================
// Benchmark 1 (PRIMARY) -- level sweep via the one-shot `compress2`
// ===========================================================================

/// Sweep the one-shot [`compress2`](zlib_rs::util::compress2) encoder across
/// [`LEVELS`] for every corpus, reporting throughput against the uncompressed
/// input size.
///
/// These are the primary, authoritative `zlib_rs` numbers. The matching C-zlib
/// baseline is produced by [`bench_baseline_czlib`] into the *same*
/// `deflate_levels` group, so each `<name>_L<level>` (zlib_rs) entry sits next
/// to its `czlib_<name>_L<level>` (C zlib) counterpart; the >= 80 % gate is the
/// ratio of the two. `compress2` allocates its own output `Vec` on each call --
/// that allocation is part of the measured one-shot cost and is representative
/// of real one-shot usage.
fn bench_levels(c: &mut Criterion) {
    let corpora = build_corpora();

    // Untimed correctness spot-check: guard against benchmarking a broken
    // encoder. Every corpus must compress without error and yield a non-empty
    // stream at the default level before we start timing.
    for (name, data) in &corpora {
        let packed = compress2(data, 6).expect("compress2 level 6 must succeed in setup");
        assert!(
            !packed.is_empty(),
            "compressed output for corpus `{name}` must be non-empty"
        );
    }

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        // Throughput is measured against the uncompressed input length.
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
}

// ===========================================================================
// Benchmark 2 -- strategy coverage via the streaming `Deflate` API
// ===========================================================================

/// Exercise each of the five [`Strategy`] variants at a fixed level (6), window
/// (15), and memory level (8) so every `deflate_*` strategy path is benchmarked.
///
/// Uses the streaming [`Deflate`] API (the only way to select a strategy) with a
/// single [`FlushMode::Finish`] over a `compress_bound`-sized output buffer; see
/// [`deflate_finish`] for the defensive Finish-loop. The text-like corpus is
/// chosen as representative real-world input.
fn bench_strategies(c: &mut Criterion) {
    let data = text_like();
    let cap = compress_bound(data.len());

    let mut group = c.benchmark_group("deflate_strategies");
    group.throughput(Throughput::Bytes(data.len() as u64));
    for strategy in [
        Strategy::Default,
        Strategy::Filtered,
        Strategy::HuffmanOnly,
        Strategy::Rle,
        Strategy::Fixed,
    ] {
        let id = BenchmarkId::new("strategy", format!("{strategy:?}"));
        group.bench_with_input(id, data.as_slice(), |b, input| {
            b.iter(|| {
                // A fresh output buffer per iteration (sized so a single Finish
                // completes the stream); its allocation is intentionally part of
                // the measured cost and is identical across strategies.
                let mut out = vec![0u8; cap];
                let produced = deflate_finish(black_box(input), &mut out, strategy);
                black_box(produced)
            });
        });
    }
    group.finish();
}

// ===========================================================================
// Benchmark 3 -- canonical C-zlib baseline (the >= 80 % reference)
// ===========================================================================

/// Compress the same corpora at the same [`LEVELS`] through canonical **C zlib**
/// (via `flate2` built with its `zlib` feature), into the *same*
/// `deflate_levels` group as [`bench_levels`].
///
/// Co-locating the baseline lets the gate be read directly: for each corpus and
/// level, `zlib_rs`'s `deflate_levels/<name>_L<level>` throughput divided by
/// this baseline's `deflate_levels/czlib_<name>_L<level>` throughput must be
/// **>= 0.80** to satisfy AAP sec. 0.7.3.
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();

    let mut group = c.benchmark_group("deflate_levels");
    for (name, data) in &corpora {
        group.throughput(Throughput::Bytes(data.len() as u64));
        for level in LEVELS {
            // `flate2` levels are 0..=9; `level.max(0)` maps a hypothetical
            // `Z_DEFAULT_COMPRESSION` (-1) to flate2's default (6) and leaves
            // 0 (`Compression::none()`-equivalent) and 1..=9 unchanged.
            let compression = Compression::new(level.max(0) as u32);
            let id = BenchmarkId::new(format!("czlib_{name}_L{level}"), data.len());
            group.bench_with_input(id, data.as_slice(), |b, input| {
                b.iter(|| {
                    let mut encoder = ZlibEncoder::new(Vec::new(), compression);
                    encoder
                        .write_all(black_box(input))
                        .expect("flate2 write_all");
                    black_box(encoder.finish().expect("flate2 finish"))
                });
            });
        }
    }
    group.finish();
}

// ===========================================================================
// Harness wiring (criterion supplies `main`; `harness = false` in Cargo.toml)
// ===========================================================================

criterion_group!(
    benches,
    bench_levels,
    bench_strategies,
    bench_baseline_czlib
);
criterion_main!(benches);
