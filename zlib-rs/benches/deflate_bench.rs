//! DEFLATE compression-throughput benchmarks for the `zlib-rs` crate.
//!
//! This Criterion benchmark measures the compressor across two axes:
//!
//! * **Compression level** (`bench_levels`) — the guaranteed deliverable.
//!   Sweeps levels `0..=9` over four input profiles that exercise distinct
//!   compressibility regimes (`text`, `binary`, `repetitive`, `random`),
//!   reporting throughput in **bytes/sec** (via Criterion [`Throughput`]) and
//!   printing the achieved **compression ratio** (`output / input`) for each
//!   `(profile, level)` pair.
//! * **Compression strategy** (`bench_strategies`) — wired on the public
//!   streaming deflate API (`deflate_init2` + `deflate`), which the core does
//!   expose. It sweeps the five `Strategy` variants at a fixed level over the
//!   text profile. (Had the streaming API not been part of the stable public
//!   surface, this axis would have been deferred — the one-shot `compress2`
//!   path takes only a level, not a strategy.)
//! * **C-zlib baseline** (`bench_compress_vs_czlib`) — the head-to-head axis
//!   that makes the AAP "compression throughput >= 80% of C zlib" target
//!   directly measurable. For each profile at level 6 it benchmarks the Rust
//!   one-shot `compress2` (`rust/<profile>`) against canonical C zlib
//!   (`c-zlib/<profile>`) on **identical** input, with identical
//!   [`Throughput`], so the ratio of the two reported throughputs is the target
//!   metric. The C side is reached through `flate2` configured with
//!   `default-features = false, features = ["zlib"]`, which routes to the
//!   system C zlib via `libz-sys` — the very implementation the port must
//!   match. `flate2`'s low-level [`Compress`] compresses into a caller-provided,
//!   `compress_bound`-sized buffer in a single `Finish` pass, the direct analog
//!   of the Rust `compress2` call, so the comparison is apples-to-apples (the
//!   thin `flate2` wrapper over the C `z_stream` adds negligible per-call
//!   overhead). Because the port is byte-identical to C zlib at level 6, both
//!   sides also emit the same number of compressed bytes (reported alongside).
//!
//! # Harness
//!
//! The crate wires this file as `[[bench]] name = "deflate_bench"` with
//! `harness = false`, so the file supplies its own `main` through
//! [`criterion_main!`]; there is deliberately no `fn main`, `#[bench]`, or
//! `#[test]` here. It builds with the crate's default features
//! (`["std", "simd"]` plus their transitive `gzip` / `gz-io` companions when
//! enabled) and requires no extra feature flags.
//!
//! Algorithmic lineage: the C `deflate.c` / `compress.c` translation units
//! (`inflate.c` for round-trip context), now rewritten under
//! `zlib-rs/src/deflate/` and `zlib-rs/src/util/`.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// C-zlib baseline (Issue 7). `flate2` is configured in `[dev-dependencies]` with
// `default-features = false, features = ["zlib"]`, so it links the canonical C
// zlib through `libz-sys`. The low-level `Compress` type compresses into a
// caller-provided buffer (the direct analog of the Rust one-shot `compress2`),
// giving a fair, allocation-free-in-the-hot-loop throughput comparison.
use flate2::{Compress, Compression, FlushCompress, Status};

// One-shot compression surface (`compress.c` port). These live under the
// `util` module of the core crate; that is the stable, verified path (the
// crate root does not re-export them).
use zlib_rs::util::{compress_bound, compress2};

// Streaming deflate surface, used only by the strategy axis (`bench_strategies`)
// to compress at a chosen `Strategy` — something the one-shot `compress2` API
// cannot express. All of these are part of the crate's public API.
use zlib_rs::constants::{DEF_MEM_LEVEL, Flush, MAX_WBITS, Strategy, Z_DEFLATED};
use zlib_rs::deflate::{deflate, deflate_init2};
use zlib_rs::error::ReturnCode;
use zlib_rs::stream::ZStream;

// ---------------------------------------------------------------------------
// Input corpora (deterministic)
// ---------------------------------------------------------------------------

/// Fixed RNG seed so the `random` corpus is byte-for-byte reproducible across
/// runs and machines (only [`make_random`] consumes it).
const SEED: u64 = 0x5EED_C0DE_1234_5678;

/// Per-profile input size: 1 MiB. Large enough for stable throughput numbers
/// while keeping a full `cargo bench` run reasonable.
const INPUT_LEN: usize = 1 << 20;

/// Highly compressible, text-like data: a fixed English sentence repeated and
/// truncated to `len`. Representative of natural-language / source-code input.
fn make_text(len: usize) -> Vec<u8> {
    const SENTENCE: &[u8] = b"The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
    let mut buf = Vec::with_capacity(len + SENTENCE.len());
    while buf.len() < len {
        buf.extend_from_slice(SENTENCE);
    }
    buf.truncate(len);
    buf
}

/// Moderately compressible structured binary: a stream of little-endian `u32`
/// counters. The low bytes cycle quickly while the high bytes change slowly,
/// giving the matcher real but limited redundancy to find.
fn make_binary(len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len + 4);
    let mut counter: u32 = 0;
    while buf.len() < len {
        buf.extend_from_slice(&counter.to_le_bytes());
        counter = counter.wrapping_add(1);
    }
    buf.truncate(len);
    buf
}

/// Best-case compression: a short fixed pattern repeated to `len`, which the
/// LZ77 stage collapses almost entirely into back-references.
fn make_repetitive(len: usize) -> Vec<u8> {
    const PATTERN: &[u8] = b"zlib-rs!";
    let mut buf = Vec::with_capacity(len + PATTERN.len());
    while buf.len() < len {
        buf.extend_from_slice(PATTERN);
    }
    buf.truncate(len);
    buf
}

/// Incompressible data: `len` bytes drawn from a seeded PRNG. DEFLATE cannot
/// shrink this, so the output approaches the (slightly inflated) input size —
/// the worst case for the compressor.
fn make_random(len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut buf = vec![0u8; len];
    rng.fill(&mut buf[..]);
    buf
}

/// The four benchmark corpora, each labelled for use in Criterion benchmark
/// ids and the ratio report.
fn profiles() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("text", make_text(INPUT_LEN)),
        ("binary", make_binary(INPUT_LEN)),
        ("repetitive", make_repetitive(INPUT_LEN)),
        ("random", make_random(INPUT_LEN)),
    ]
}

// ---------------------------------------------------------------------------
// Level sweep (guaranteed deliverable)
// ---------------------------------------------------------------------------

/// Benchmark one-shot compression throughput across all levels (`0..=9`) and
/// all input profiles.
fn bench_levels(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_levels");

    for (name, input) in profiles() {
        // Report results in bytes/sec relative to the *input* size.
        group.throughput(Throughput::Bytes(input.len() as u64));
        let bound = compress_bound(input.len());

        for level in 0..=9i32 {
            // Compression ratio is reported outside the timed region so it
            // never perturbs the throughput measurement. `print_stdout` is an
            // allow-by-default restriction lint, so this is clippy-clean.
            let mut probe = vec![0u8; bound];
            let n = compress2(&mut probe, &input, level).expect("compress2 probe failed");
            let ratio = n as f64 / input.len() as f64;
            println!(
                "[deflate] axis=level profile={name} level={level} bytes={n} ratio={ratio:.4}"
            );

            group.bench_with_input(BenchmarkId::new(name, level), &level, |b, &level| {
                // Allocate the destination once, outside `iter`, so buffer
                // allocation is excluded from the timed body; `compress2`
                // overwrites it in full on every call.
                let mut dst = vec![0u8; bound];
                b.iter(|| {
                    let n = compress2(&mut dst, black_box(&input), level).unwrap();
                    black_box(n);
                });
            });
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Strategy sweep (streaming deflate API)
// ---------------------------------------------------------------------------

/// Drive the streaming compressor to completion at a fixed `level` and
/// `strategy`, returning the number of compressed bytes written to `dst`.
///
/// This mirrors the one-shot `compress2` loop but goes through `deflate_init2`
/// so a non-default [`Strategy`] can be selected. Cleanup is by RAII: dropping
/// `strm` frees the compressor state (the `deflateEnd` equivalent), exactly as
/// the `compress2` port does.
fn deflate_with_strategy(dst: &mut [u8], src: &[u8], level: i32, strategy: Strategy) -> usize {
    let mut strm = ZStream::new();
    deflate_init2(
        &mut strm,
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        strategy,
    )
    .expect("deflate_init2 failed");

    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let (rc, consumed, produced) = deflate(
            &mut strm,
            &src[in_pos..],
            &mut dst[out_pos..],
            Flush::Finish,
        );
        in_pos += consumed;
        out_pos += produced;
        match rc {
            // All input compressed and the trailer flushed.
            Ok(ReturnCode::StreamEnd) => break,
            // More work to do; guard against a stall (would mean `dst` is too
            // small, which cannot happen here since it is `compress_bound`d).
            Ok(_) => assert!(
                consumed != 0 || produced != 0,
                "deflate stalled (destination too small?)"
            ),
            Err(e) => panic!("deflate failed: {e:?}"),
        }
    }

    out_pos
}

/// Benchmark compression throughput across the five compression strategies at
/// a fixed level over the text profile.
fn bench_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_strategies");

    let input = make_text(INPUT_LEN);
    let bound = compress_bound(input.len());
    let level = 6i32;
    group.throughput(Throughput::Bytes(input.len() as u64));

    let strategies = [
        ("default", Strategy::Default),
        ("filtered", Strategy::Filtered),
        ("huffman_only", Strategy::HuffmanOnly),
        ("rle", Strategy::Rle),
        ("fixed", Strategy::Fixed),
    ];

    for (name, strategy) in strategies {
        // Ratio report, outside the timed region (see `bench_levels`).
        let mut probe = vec![0u8; bound];
        let n = deflate_with_strategy(&mut probe, &input, level, strategy);
        let ratio = n as f64 / input.len() as f64;
        println!(
            "[deflate] axis=strategy strategy={name} level={level} bytes={n} ratio={ratio:.4}"
        );

        group.bench_with_input(
            BenchmarkId::new("strategy", name),
            &strategy,
            |b, &strategy| {
                let mut dst = vec![0u8; bound];
                b.iter(|| {
                    let n = deflate_with_strategy(&mut dst, black_box(&input), level, strategy);
                    black_box(n);
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// C-zlib baseline (compression throughput parity, the >= 80% target)
// ---------------------------------------------------------------------------

/// Compress `src` into `dst` using canonical **C zlib** (via `flate2`'s
/// `["zlib"]` backend), returning the number of bytes written.
///
/// This is the direct analog of the Rust one-shot [`compress2`]: a freshly
/// initialised compressor (matching `compress2`, which sets up state per call),
/// a single `Finish` pass into a `compress_bound`-sized output buffer, and the
/// total bytes produced. `zlib_header = true` selects the zlib container so the
/// produced stream matches the Rust `compress2` wire format byte-for-byte.
fn czlib_compress(dst: &mut [u8], src: &[u8], level: u32) -> usize {
    let mut comp = Compress::new(Compression::new(level), true);
    let status = comp
        .compress(src, dst, FlushCompress::Finish)
        .expect("flate2 (C zlib) compress failed");
    // `dst` is `compress_bound`-sized, so a single Finish pass always completes.
    assert_eq!(
        status,
        Status::StreamEnd,
        "flate2 (C zlib) did not finish in one pass (destination too small?)"
    );
    comp.total_out() as usize
}

/// Head-to-head compression throughput: Rust `compress2` versus canonical C
/// zlib over identical inputs at level 6. The two reported throughputs share a
/// group and `Throughput`, so their ratio is the AAP ">= 80% of C zlib" metric.
fn bench_compress_vs_czlib(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_vs_czlib");
    let level = 6i32;

    for (name, input) in profiles() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        let bound = compress_bound(input.len());

        // Report both sides' compressed sizes outside the timed region. At level
        // 6 the port is byte-identical to C zlib, so these should agree.
        let mut probe_rust = vec![0u8; bound];
        let rust_bytes =
            compress2(&mut probe_rust, &input, level).expect("rust compress2 probe failed");
        let mut probe_czlib = vec![0u8; bound];
        let czlib_bytes = czlib_compress(&mut probe_czlib, &input, level as u32);
        println!(
            "[deflate] axis=vs_czlib profile={name} level={level} \
             rust_bytes={rust_bytes} czlib_bytes={czlib_bytes}"
        );

        // Rust one-shot compress2.
        group.bench_with_input(BenchmarkId::new("rust", name), &input, |b, input| {
            let mut dst = vec![0u8; bound];
            b.iter(|| {
                let n = compress2(&mut dst, black_box(input), level).unwrap();
                black_box(n);
            });
        });

        // Canonical C zlib (flate2 `["zlib"]` backend).
        group.bench_with_input(BenchmarkId::new("c-zlib", name), &input, |b, input| {
            let mut dst = vec![0u8; bound];
            b.iter(|| {
                let n = czlib_compress(&mut dst, black_box(input), level as u32);
                black_box(n);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_levels,
    bench_strategies,
    bench_compress_vs_czlib
);
criterion_main!(benches);
