//! DEFLATE **decompression** throughput benchmarks for the `zlib-rs` core.
//!
//! This Criterion harness measures *steady-state decode* speed — bytes of
//! **decompressed** output produced per second — when inflating representative,
//! pre-compressed corpora. Two framings are exercised:
//!
//! * **zlib** (RFC 1950) — the GUARANTEED measurement. Corpora are produced with
//!   the stable one-shot compressor [`zlib_rs::util::compress2`] and decoded with
//!   the stable one-shot decompressor [`zlib_rs::util::uncompress2`].
//! * **raw DEFLATE** (RFC 1951, no wrapper) — wired through the streaming
//!   [`zlib_rs::inflate::InflateState`] API with negative `windowBits`
//!   (`InflateState::new(-15)`). Corpora are produced by `flate2`'s raw
//!   [`flate2::write::DeflateEncoder`], giving an independent producer so the
//!   bench doubles as a cross-implementation decode check.
//!
//! Each case is benched over two input densities so decode is observed across
//! realistic stream shapes: a *compressible* text-like buffer (rich in LZ77
//! back-references, which exercises the inflate fast-path match-copy loop) and a
//! *random* incompressible buffer (stored/literal-dominated, closer to a
//! `memcpy` lower bound). [`criterion::Throughput::Bytes`] is always set to the
//! **decompressed** (original) length, so the reported figure is decode
//! throughput regardless of compression ratio.
//!
//! A **gzip** (RFC 1952) decode case is intentionally *not* included here: the
//! gzip container decoder is gated behind the `gzip` Cargo feature, which is
//! absent from this crate's default feature set (`default = ["std", "simd"]`),
//! and `cargo bench` builds with default features only. See the note above
//! [`criterion_group!`] at the bottom of the file.
//!
//! # Harness
//!
//! This file is wired in `zlib-rs/Cargo.toml` as
//! `[[bench]] name = "inflate_bench"` with **`harness = false`**, so it provides
//! its own `main` via [`criterion_group!`] + [`criterion_main!`]; there is no
//! `fn main`, `#[bench]`, or libtest harness here.

use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use zlib_rs::constants::Flush;
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::InflateState;
use zlib_rs::util::{compress_bound, compress2, uncompress2};

// ---------------------------------------------------------------------------
// Tunables (all fixed for fully deterministic, reproducible corpora)
// ---------------------------------------------------------------------------

/// Fixed RNG seed: every run regenerates byte-identical corpora, so benchmark
/// numbers are comparable across invocations and machines.
const SEED: u64 = 0x5EED_C0FF_EE15_900D;

/// Per-buffer original (decompressed) length: 1 MiB. Large enough to amortize
/// stream setup and reach steady-state decode throughput, small enough to keep
/// Criterion's sampling loop snappy.
const INPUT_LEN: usize = 1 << 20;

/// Compression level used when *producing* the decode corpora. The decode
/// throughput we measure is essentially insensitive to the level used to build
/// the stream, so a single mid-range level keeps corpus setup cheap. (Level is
/// part of stream *production*, which happens once in setup, outside timing.)
const CORPUS_LEVEL: i32 = 6;

/// A small, fixed vocabulary used to synthesize realistic, compressible
/// text-like input. Whitespace-separated common words yield a stream with both
/// Huffman-codeable literals and frequent LZ77 matches — the workload the
/// inflate match-copy hot path is tuned for.
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
    "stream",
    "buffer",
    "window",
    "huffman",
    "compression",
];

// ---------------------------------------------------------------------------
// Deterministic source-buffer generators (decompressed inputs)
// ---------------------------------------------------------------------------

/// Build a compressible, text-like buffer of exactly `len` bytes.
///
/// Words are drawn from [`WORDS`] using a fixed-seed [`StdRng`] and joined with
/// single spaces; the result is truncated to `len`. Highly (but not trivially)
/// compressible, exercising both literal coding and back-reference copies on
/// decode.
fn make_text_like(len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut buf = Vec::with_capacity(len + 16);
    while buf.len() < len {
        let word = WORDS[rng.random_range(0..WORDS.len())];
        buf.extend_from_slice(word.as_bytes());
        buf.push(b' ');
    }
    buf.truncate(len);
    buf
}

/// Build an incompressible, uniformly random buffer of exactly `len` bytes.
///
/// Uses a distinct fixed seed (so it is independent of [`make_text_like`]) and
/// fills the whole buffer with random bytes via [`Rng::fill`]. DEFLATE cannot
/// shrink this, so the produced stream is stored/literal-dominated and decode
/// approaches a raw-copy lower bound.
fn make_random(len: usize) -> Vec<u8> {
    // Golden-ratio odd constant decorrelates this seed from the text seed.
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x9E37_79B9_7F4A_7C15);
    let mut buf = vec![0u8; len];
    rng.fill(&mut buf[..]);
    buf
}

/// The set of decompressed source buffers every bench decodes, paired with a
/// stable label used as the Criterion parameter id.
fn source_buffers() -> [(&'static str, Vec<u8>); 2] {
    [
        ("text", make_text_like(INPUT_LEN)),
        ("random", make_random(INPUT_LEN)),
    ]
}

// ---------------------------------------------------------------------------
// Corpus producers (run once in setup, OUTSIDE the timed region)
// ---------------------------------------------------------------------------

/// Produce a **zlib** (RFC 1950) stream of `original` at `level` using the
/// stable one-shot compressor. Mirrors the documented sizing recipe:
/// allocate [`compress_bound`] bytes, compress, then truncate to the exact
/// produced length.
fn deflate_zlib(original: &[u8], level: i32) -> Vec<u8> {
    let mut compressed = vec![0u8; compress_bound(original.len())];
    let produced = compress2(&mut compressed, original, level).expect("compress2 (zlib) failed");
    compressed.truncate(produced);
    compressed
}

/// Produce a **raw DEFLATE** (RFC 1951, no zlib/gzip wrapper) stream of
/// `original` at `level` using `flate2`'s raw encoder. The exact backend is
/// irrelevant — any conformant raw-DEFLATE bytes decode under
/// `InflateState::new(-15)`.
fn deflate_raw(original: &[u8], level: i32) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level as u32));
    encoder
        .write_all(original)
        .expect("flate2 DeflateEncoder write_all failed");
    encoder
        .finish()
        .expect("flate2 DeflateEncoder finish failed")
}

/// Decode a complete **raw DEFLATE** stream `compressed` into `out` via the
/// streaming [`InflateState`] API, returning the number of decompressed bytes
/// written.
///
/// A fresh raw decoder (`windowBits = -15`, i.e. no header/trailer) is created
/// per call — mirroring how the one-shot [`uncompress2`] allocates its state
/// per invocation, so the zlib and raw benches measure comparable end-to-end
/// decode work. `out` must already be sized to the known decompressed length;
/// it is overwritten from the front and never reallocated here.
fn raw_inflate(compressed: &[u8], out: &mut [u8]) -> usize {
    let mut state = InflateState::new(-15).expect("raw InflateState::new(-15) failed");
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let result = state.inflate(&compressed[in_pos..], &mut out[out_pos..], Flush::NoFlush);
        in_pos += result.consumed;
        out_pos += result.produced;
        match result.status {
            // Final block decoded (raw streams carry no trailer to validate).
            Ok(ReturnCode::StreamEnd) => break,
            // Forward progress on a healthy stream: keep decoding. A no-progress
            // `Ok` cannot occur for a complete corpus, but guard against it so
            // the loop is provably terminating.
            Ok(_) => {
                if result.consumed == 0 && result.produced == 0 {
                    break;
                }
            }
            // Unreachable for the valid corpora produced in setup; break rather
            // than panic inside a benchmark.
            Err(_) => break,
        }
    }
    out_pos
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// **zlib-framed decode** throughput — the guaranteed measurement.
///
/// For each source buffer a zlib (RFC 1950) corpus is produced once in setup,
/// then [`uncompress2`] is timed decoding it into a pre-allocated output buffer.
/// The output buffer is sized to the known original length and allocated
/// *outside* `b.iter`, so allocation is excluded from the measured decode time;
/// `uncompress2` overwrites it on each iteration.
fn bench_zlib_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_zlib");
    for (label, original) in source_buffers() {
        // Setup (outside timing): build the zlib stream and capture the
        // decompressed length as a plain `usize` to size `out` and report
        // throughput without holding a borrow of `original` in the closure.
        let compressed = deflate_zlib(&original, CORPUS_LEVEL);
        let original_len = original.len();

        // Throughput is keyed on the DECOMPRESSED size ⇒ decode bytes/sec.
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &compressed,
            |b, comp| {
                let mut out = vec![0u8; original_len];
                b.iter(|| {
                    // `uncompress2` -> Ok((produced, consumed)); keep `produced`.
                    let (produced, _consumed) = uncompress2(&mut out, black_box(comp.as_slice()))
                        .expect("uncompress2 failed");
                    black_box(produced);
                });
            },
        );
    }
    group.finish();
}

/// **raw-DEFLATE decode** throughput (RFC 1951, no wrapper).
///
/// Corpora are produced by `flate2`'s raw [`DeflateEncoder`] in setup, then
/// decoded with the streaming [`InflateState`] (`windowBits = -15`) via
/// [`raw_inflate`]. As with the zlib bench, the output buffer is allocated
/// outside `b.iter` and throughput is keyed on the decompressed length.
fn bench_raw_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_raw");
    for (label, original) in source_buffers() {
        // Setup (outside timing): produce a raw-DEFLATE corpus from an
        // independent encoder and capture the decompressed length.
        let compressed = deflate_raw(&original, CORPUS_LEVEL);
        let original_len = original.len();

        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &compressed,
            |b, comp| {
                let mut out = vec![0u8; original_len];
                b.iter(|| {
                    let produced = raw_inflate(black_box(comp.as_slice()), &mut out);
                    black_box(produced);
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Harness wiring (mandatory: this bench is `harness = false`)
// ---------------------------------------------------------------------------
//
// A gzip-framed (RFC 1952) decode bench is deliberately NOT registered here.
// Gzip-container decoding is gated behind the `gzip` Cargo feature, which is
// not part of this crate's default feature set (`default = ["std", "simd"]`);
// `cargo bench` builds with default features, under which `InflateState`
// rejects the gzip `windowBits` range (>= 24). The zlib and raw-DEFLATE paths
// above are fully available under default features and cover decode throughput.
// A gzip decode bench can be added once it is exercised under `--features gzip`.
criterion_group!(benches, bench_zlib_decode, bench_raw_decode);
criterion_main!(benches);
