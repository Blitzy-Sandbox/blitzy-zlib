//! INFLATE decompression-throughput benchmarks for the `zlib_rs` crate.
//!
//! This criterion harness measures the *decompression* (INFLATE) throughput of
//! `zlib_rs` and validates the AAP section 0.7.3 performance gate:
//! decompression throughput must be >= C zlib (parity or better). The
//! conceptual comparison targets are the C decode engines `inflate.c`,
//! `inffast.c`, `inftrees.c`, and `infback.c`; canonical C zlib is linked
//! (development-only) through the `flate2` `zlib` backend and benched
//! side-by-side as the parity oracle.
//!
//! Design rule: ALL compression happens in untimed setup. Every timed body
//! decompresses only -- the destination buffer is pre-allocated once (sized to
//! the original length) and reused across iterations, so the measured cost is
//! pure decode throughput, isolated from allocation and from the encoder.
//!
//! Three benchmark functions are registered:
//! - `bench_uncompress`: one-shot `uncompress` (the PRIMARY numbers), group `inflate`.
//! - `bench_inflate_stream`: streaming `Inflate` state machine, group `inflate_stream`.
//! - `bench_baseline_czlib`: flate2 / C-zlib oracle, group `inflate` (side-by-side).
//!
//! Throughput is reported on the *uncompressed (output)* size, so the ratio
//! `inflate/zlibrs_<corpus>` over `inflate/czlib_<corpus>` reads directly as
//! the parity gate: it must be >= 1.0.
//!
//! Safe Rust only -- no `unsafe`, no `extern "C"`. The `capi` FFI feature stays
//! OFF for benches so the `flate2` C-zlib oracle never collides with the
//! crate's own (gated) C-ABI exports.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::read::ZlibDecoder;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::hint::black_box;
use std::io::Read;
use zlib_rs::util::{compress2, uncompress};
use zlib_rs::{Inflate, Z_NO_FLUSH, Z_STREAM_END};

/// Fixed RNG seed -- keeps every corpus byte-for-byte reproducible across runs
/// so successive benchmark measurements compare like with like.
const SEED: u64 = 0x5A4C_4942;

/// Corpus size: 1 MiB. Large enough that the steady-state decode loop, rather
/// than per-call fixed overhead, dominates the measured throughput.
const SIZE: usize = 1024 * 1024;

/// Compression level used to PREPARE the decode inputs (untimed). Level 6 is
/// zlib's default (`Z_DEFAULT_COMPRESSION`) and produces a standard zlib stream
/// (window bits 15) that both `zlib_rs` and flate2 / C zlib decode.
const SETUP_LEVEL: i32 = 6;

/// Printable alphabet for the `text_like` corpus. Random bytes are folded onto
/// this set, yielding text-shaped, moderately compressible data (dynamic
/// Huffman blocks) -- distinct from both the highly redundant and the random
/// corpora.
const ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 .,;:\n\t";

/// Highly compressible corpus: a fixed phrase repeated to fill `size` bytes.
/// Exercises the long-match / back-reference-heavy decode path.
fn highly_compressible(size: usize) -> Vec<u8> {
    const PHRASE: &[u8] = b"the quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        let remaining = size - data.len();
        let take = remaining.min(PHRASE.len());
        data.extend_from_slice(&PHRASE[..take]);
    }
    data
}

/// Text-like corpus: deterministic random bytes mapped onto a printable
/// alphabet. Produces dynamic-Huffman-dominated streams.
fn text_like(size: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut data = vec![0u8; size];
    rng.fill_bytes(&mut data);
    for byte in &mut data {
        *byte = ALPHABET[(*byte as usize) % ALPHABET.len()];
    }
    data
}

/// Incompressible corpus: deterministic raw random bytes. zlib emits mostly
/// stored blocks, so the decode path is dominated by literal copies.
fn incompressible(size: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut data = vec![0u8; size];
    rng.fill_bytes(&mut data);
    data
}

/// A prepared decode input: a human-readable corpus name, the *compressed*
/// zlib stream (produced in untimed setup), and the original (decompressed)
/// length the timed bodies decode back to.
struct Corpus {
    name: &'static str,
    compressed: Vec<u8>,
    original_len: usize,
}

/// Build every corpus and pre-compress it in UNTIMED setup.
///
/// Each corpus is compressed once with `compress2` (level 6 -> standard zlib
/// stream, window bits 15) and then sanity-decoded with `uncompress`, so the
/// harness never benchmarks against a broken decoder: a setup-time panic here
/// means the round-trip itself is wrong (a true bug), not a benchmarking
/// artifact. None of this work is timed -- it runs before any `b.iter`.
fn build_corpora() -> Vec<Corpus> {
    let raw: [(&'static str, Vec<u8>); 3] = [
        ("highly_compressible", highly_compressible(SIZE)),
        ("text_like", text_like(SIZE)),
        ("incompressible", incompressible(SIZE)),
    ];

    raw.into_iter()
        .map(|(name, original)| {
            let original_len = original.len();
            let compressed = compress2(&original, SETUP_LEVEL).expect("setup compress2");

            // Untimed correctness guard: confirm the decoder reproduces the
            // exact original bytes before any timing happens.
            let mut check = vec![0u8; original_len];
            let produced = uncompress(&mut check, &compressed).expect("setup uncompress");
            assert_eq!(
                produced, original_len,
                "setup decode length mismatch ({name})"
            );
            assert_eq!(check, original, "setup decode content mismatch ({name})");

            Corpus {
                name,
                compressed,
                original_len,
            }
        })
        .collect()
}

/// PRIMARY: one-shot `uncompress` decode throughput.
///
/// The destination buffer is allocated once (sized to the original length) and
/// reused across iterations -- `uncompress` overwrites it on every call, so the
/// measured cost is pure decode throughput, free of allocation. Throughput is
/// reported on the uncompressed size so it is directly comparable to the
/// flate2 / C-zlib baseline registered in the same `inflate` group.
fn bench_uncompress(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("zlibrs_{name}"), original_len),
            &corpus.compressed,
            move |b, src| {
                let mut dest = vec![0u8; original_len];
                b.iter(|| {
                    let produced =
                        uncompress(&mut dest, black_box(src.as_slice())).expect("uncompress");
                    black_box(produced)
                });
            },
        );
    }

    // GATE (AAP 0.7.3): each `inflate/zlibrs_<corpus>` throughput must be >= the
    // matching `inflate/czlib_<corpus>` throughput emitted by
    // `bench_baseline_czlib` (parity or better).
    group.finish();
}

/// Streaming decode coverage via the raw-`i32` `Inflate` state-machine API.
///
/// ASYMMETRY (do not confuse with the deflate side): `Inflate::inflate` takes a
/// raw `i32` flush (`Z_NO_FLUSH` == 0) and returns a raw
/// `(code, consumed, produced)` tuple -- it is NOT a `Result` and NOT a
/// `FlushMode`. `code == Z_STREAM_END` (1) signals a complete decode.
/// `Inflate::new()` configures the zlib wrapper at window bits 15, matching the
/// `compress2` output exactly (equivalently, `Inflate::with_window_bits(15)`).
///
/// A fresh `Inflate` is constructed per iteration, so this path intentionally
/// includes state-machine init and teardown. The decode is wrapped in a cursor
/// loop that advances the input/output offsets until `Z_STREAM_END`; for a
/// known-good stream decoded into a buffer sized to the original length a
/// single call completes, but the loop makes the harness robust to partial
/// progress and guards against an unexpected stall.
fn bench_inflate_stream(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate_stream");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new("stream", name),
            &corpus.compressed,
            move |b, src| {
                let mut dest = vec![0u8; original_len];
                b.iter(|| {
                    // Inflate::new() == zlib wrapper, window bits 15 (matches
                    // the compress2 output above).
                    let mut inf = Inflate::new().expect("inflate init");
                    let mut in_off = 0usize;
                    let mut out_off = 0usize;
                    loop {
                        let (code, consumed, produced) = inf.inflate(
                            black_box(&src[in_off..]),
                            &mut dest[out_off..],
                            Z_NO_FLUSH,
                        );
                        in_off += consumed;
                        out_off += produced;
                        if code == Z_STREAM_END {
                            break;
                        }
                        // Defensive: a known-good stream into a correctly sized
                        // buffer reaches Z_STREAM_END; if a call makes no
                        // progress at all, stop rather than spin forever.
                        if consumed == 0 && produced == 0 {
                            break;
                        }
                    }
                    debug_assert_eq!(out_off, original_len, "stream decode length mismatch");
                    black_box(out_off)
                });
            },
        );
    }

    group.finish();
}

/// Parity reference: decompress the SAME compressed bytes through canonical
/// C zlib (flate2's `zlib` backend). Registered in the same `inflate` group as
/// `bench_uncompress`, so `zlibrs_<corpus>` and `czlib_<corpus>` appear
/// side-by-side per corpus. The output `Vec` is allocated once with capacity
/// and `clear()`-ed each iteration (capacity is retained), keeping allocation
/// out of the timed loop.
fn bench_baseline_czlib(c: &mut Criterion) {
    let corpora = build_corpora();
    let mut group = c.benchmark_group("inflate");

    for corpus in &corpora {
        let name = corpus.name;
        let original_len = corpus.original_len;
        group.throughput(Throughput::Bytes(original_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("czlib_{name}"), original_len),
            &corpus.compressed,
            move |b, src| {
                let mut out = Vec::with_capacity(original_len);
                b.iter(|| {
                    out.clear();
                    let mut decoder = ZlibDecoder::new(black_box(src.as_slice()));
                    decoder.read_to_end(&mut out).expect("flate2 zlib decode");
                    black_box(out.len())
                });
            },
        );
    }

    // GATE (AAP 0.7.3): the ratio zlib_rs throughput / C-zlib throughput must be
    // >= 1.0 (parity or better) for every corpus.
    group.finish();
}

criterion_group!(
    benches,
    bench_uncompress,
    bench_inflate_stream,
    bench_baseline_czlib
);
criterion_main!(benches);
