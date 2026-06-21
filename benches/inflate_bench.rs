//! INFLATE (decompression) throughput benchmarks for the `zlib-rs` crate.
//!
//! This [criterion] harness measures the decompression rate of `zlib_rs`
//! across three deterministic corpora and proves the AAP §0.7.3 performance
//! gate: **decompression throughput ≥ C zlib (parity or better)**. To make the
//! gate directly readable, a `flate2` baseline group decompresses the *same*
//! compressed bytes through canonical C zlib (the `flate2` dev-dependency built
//! with its `zlib` backend) so the two appear side-by-side per corpus.
//!
//! The conceptual comparison target is the C decode engine
//! (`inflate.c` / `inffast.c` / `inftrees.c` / `infback.c` and the one-shot
//! `uncompr.c`); there is no 1:1 C source for this benchmark — `criterion` is
//! the reference harness (AAP §0.4.1).
//!
//! # Critical design rule
//!
//! **All compression needed to produce the decode inputs happens in untimed
//! setup** (before `b.iter`). The timed body decompresses *only*. This isolates
//! decompression throughput from the cost of producing the compressed inputs.
//!
//! Throughput is reported on the **uncompressed (output) size**
//! ([`Throughput::Bytes`]`(original_len)`) — i.e. useful bytes produced per
//! second — so the number is directly comparable to C zlib and to the
//! compression bench.
//!
//! # Gate interpretation
//!
//! For each corpus, the `inflate/zlibrs_<name>` throughput must be **≥** the
//! `inflate/czlib_<name>` (flate2 / C zlib) throughput. Equivalently, the ratio
//! `zlibrs ÷ czlib` must be ≥ 1.0 to satisfy the parity-or-better gate.
//!
//! # API asymmetry note
//!
//! [`Inflate::inflate`] uses a **raw `i32` flush** argument and returns a **raw
//! tuple `(code, consumed, produced)`** — it is *not* a `Result` and does *not*
//! take a `FlushMode` (unlike the deflate compressor's API). A returned
//! `code == Z_STREAM_END` (`1`) signals that decoding is complete.
//!
//! This benchmark uses only the safe public Rust API — no `unsafe`, no
//! `extern "C"` (the `capi`/FFI surface is off for benches, avoiding C-zlib
//! symbol collisions with the `flate2` oracle).

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

// Crate-root re-exports (verified against `src/lib.rs`): the one-shot decode
// (`uncompress`) and encode (`compress2`) helpers, the streaming `Inflate`
// decompressor, and the C-compatible flush/return constants.
use zlib_rs::{Inflate, Z_NO_FLUSH, Z_STREAM_END, compress2, uncompress};

// Deterministic random-data generation for the `text_like` and `incompressible`
// corpora. `fill_bytes` is used directly (per the project convention) rather
// than the `gen`/`random_range` helpers, keeping the corpora reproducible.
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};

// flate2 with its `zlib` backend links canonical C zlib; `ZlibDecoder` is the
// parity reference for the decompression gate.
use flate2::read::ZlibDecoder;
use std::io::Read;

// ===========================================================================
// Deterministic corpora
// ===========================================================================

/// Fixed RNG seed so every corpus (and therefore every run) is reproducible.
const SEED: u64 = 0x5A4C_4942;

/// Working corpus size: 1 MiB. Large enough that the timed decode dominates
/// any fixed per-call overhead, yet small enough to keep the suite quick.
const SIZE: usize = 1024 * 1024;

/// Phrase repeated to build the `highly_compressible` corpus. A short,
/// frequently repeated string is the easy case for LZ77 back-references, so it
/// stresses the inflate copy path (long matches from the sliding window).
const PHRASE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";

/// Alphabet the `text_like` corpus is mapped onto. Folding uniform random bytes
/// down to ~66 symbols yields a moderate-entropy, text-shaped corpus that sits
/// between the trivially compressible and the incompressible extremes.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,";

/// Builds the `highly_compressible` corpus: [`PHRASE`] repeated to exactly
/// [`SIZE`] bytes.
fn make_highly_compressible() -> Vec<u8> {
    let mut v = Vec::with_capacity(SIZE);
    while v.len() < SIZE {
        v.extend_from_slice(PHRASE);
    }
    v.truncate(SIZE);
    v
}

/// Builds the `text_like` corpus: [`SIZE`] deterministic bytes mapped onto
/// [`ALPHABET`]. A seed distinct from [`SEED`] keeps it independent from the
/// `incompressible` corpus while remaining fully reproducible.
fn make_text_like() -> Vec<u8> {
    let mut raw = vec![0u8; SIZE];
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x5A5A_5A5A);
    rng.fill_bytes(&mut raw);
    raw.iter()
        .map(|&b| ALPHABET[(b as usize) % ALPHABET.len()])
        .collect()
}

/// Builds the `incompressible` corpus: [`SIZE`] bytes of seeded random data.
/// DEFLATE cannot shrink this, so the compressed input is ~as large as the
/// output — the worst case for the inflate literal path.
fn make_incompressible() -> Vec<u8> {
    let mut v = vec![0u8; SIZE];
    let mut rng = StdRng::seed_from_u64(SEED);
    rng.fill_bytes(&mut v);
    v
}

/// Produces the decode inputs in **untimed setup**.
///
/// Each corpus is compressed once with [`compress2`] at level 6 (a standard
/// RFC 1950 zlib stream, window bits 15 — decodable by both `zlib_rs` and
/// flate2). A round-trip sanity check then asserts that [`uncompress`]
/// reproduces the original length *and* bytes, guarding against benchmarking a
/// broken decoder. Only `(name, compressed, original_len)` is returned; the
/// timed benchmarks never recompress.
fn build_corpora() -> Vec<(&'static str, Vec<u8>, usize)> {
    let mut corpora = Vec::new();
    for (name, original) in [
        ("highly_compressible", make_highly_compressible()),
        ("text_like", make_text_like()),
        ("incompressible", make_incompressible()),
    ] {
        let original_len = original.len();
        // Compression is SETUP ONLY — it never appears in a timed body.
        let compressed = compress2(&original, 6).expect("setup compress2");

        // Round-trip guard: decode of known-good input must reproduce the
        // original exactly, or every downstream number is meaningless.
        let mut check = vec![0u8; original_len];
        let produced = uncompress(&mut check, &compressed).expect("setup uncompress");
        assert_eq!(
            produced, original_len,
            "setup: decoded length mismatch for `{name}`"
        );
        assert_eq!(
            check, original,
            "setup: decoded bytes mismatch for `{name}`"
        );

        corpora.push((name, compressed, original_len));
    }
    corpora
}

// ===========================================================================
// Phase 3 — one-shot decode throughput (PRIMARY numbers)
// ===========================================================================

/// Benchmarks [`uncompress`] (the one-shot whole-buffer decode) on each corpus.
///
/// The destination buffer is allocated once (sized to `original_len`) and
/// reused across iterations — correct because `uncompress` overwrites it on
/// every call — so the measurement reflects decode cost, not allocation.
///
/// Gate: `inflate/zlibrs_<name>` here must be ≥ the `inflate/czlib_<name>`
/// throughput produced by [`bench_baseline_czlib`].
fn bench_uncompress(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");
    for entry in &corpora {
        let name = entry.0;
        let compressed = entry.1.as_slice();
        let original_len = entry.2;

        // Throughput is reported on the uncompressed output size.
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("zlibrs_{name}"), original_len),
            compressed,
            |b, src| {
                let mut dest = vec![0u8; original_len];
                b.iter(|| {
                    let n = uncompress(&mut dest, black_box(src)).expect("uncompress");
                    black_box(n)
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Phase 4 — streaming decode coverage (raw-i32 `Inflate` API)
// ===========================================================================

/// Benchmarks the streaming [`Inflate`] state machine on a representative
/// corpus, exercising the bit-reader, window handling, and per-call bookkeeping
/// that the one-shot path hides.
///
/// [`Inflate::new`] yields a zlib-wrapper decompressor at window bits 15, which
/// matches the stream produced by [`compress2`]. With `dest` sized to
/// `original_len` and the full input slice, a single `inflate` call is expected
/// to reach [`Z_STREAM_END`]; the loop below advances the input/output cursors
/// defensively so the bench stays correct even if the decoder returns partial
/// progress on some build.
fn bench_inflate_stream(c: &mut Criterion) {
    let corpora = build_corpora();

    // `text_like` is the representative middle case: a realistic mix of
    // literals and back-references through the inflate window.
    let entry = corpora
        .iter()
        .find(|c| c.0 == "text_like")
        .expect("text_like corpus present");
    let name = entry.0;
    let compressed = entry.1.as_slice();
    let original_len = entry.2;

    let mut group = c.benchmark_group("inflate_stream");
    group.throughput(Throughput::Bytes(original_len as u64));
    group.bench_with_input(BenchmarkId::new("stream", name), compressed, |b, src| {
        let mut dest = vec![0u8; original_len];
        b.iter(|| {
            // A fresh decompressor per iteration — its allocation is part of the
            // streaming-decode cost being measured, mirroring `uncompress`.
            let mut inf = Inflate::new().expect("inflate init");
            let mut in_pos = 0usize;
            let mut out_pos = 0usize;
            loop {
                // RAW API: `(code, consumed, produced)`; `Z_NO_FLUSH` == 0.
                let (code, consumed, produced) =
                    inf.inflate(black_box(&src[in_pos..]), &mut dest[out_pos..], Z_NO_FLUSH);
                in_pos += consumed;
                out_pos += produced;
                if code == Z_STREAM_END {
                    break;
                }
                // Decode of known-good input never errors or stalls; these
                // guards turn any such (true bug) into an immediate panic
                // rather than a corrupted measurement or an infinite loop.
                assert!(code >= 0, "inflate stream error: code={code}");
                assert!(
                    consumed != 0 || produced != 0,
                    "inflate stream made no progress: code={code}"
                );
            }
            black_box(out_pos)
        });
    });
    group.finish();
}

// ===========================================================================
// Phase 5 — flate2 (C zlib) baseline — the parity reference
// ===========================================================================

/// Decompresses the same compressed bytes through canonical C zlib (via
/// `flate2`'s `zlib` backend). Placed in the **same** `"inflate"` group as
/// [`bench_uncompress`] so `zlibrs_<name>` and `czlib_<name>` sit side-by-side
/// per corpus, making the parity gate immediately readable.
///
/// Gate: `zlibrs ÷ czlib` throughput must be ≥ 1.0 (parity or better).
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");
    for entry in &corpora {
        let name = entry.0;
        let compressed = entry.1.as_slice();
        let original_len = entry.2;

        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("czlib_{name}"), original_len),
            compressed,
            |b, src| {
                // Reuse one output buffer across iterations: `clear()` keeps the
                // capacity so only the first iteration allocates.
                let mut out = Vec::with_capacity(original_len);
                b.iter(|| {
                    out.clear();
                    let mut d = ZlibDecoder::new(black_box(src));
                    d.read_to_end(&mut out).expect("flate2 decode");
                    black_box(out.len())
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Harness wiring — exactly one `criterion_group!` and one `criterion_main!`.
// ===========================================================================

criterion_group!(
    benches,
    bench_uncompress,
    bench_inflate_stream,
    bench_baseline_czlib
);
criterion_main!(benches);
