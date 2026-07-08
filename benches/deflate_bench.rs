//! Criterion throughput benchmarks for the `zlib-rs` deflate (compression) engine.
//!
//! Exercises the one-call `compress2` API across all ten compression levels
//! (`0..=9`) and across several input profiles (compressible text,
//! incompressible high-entropy bytes, and highly repetitive data). Throughput
//! is reported over the *uncompressed* input size, matching how the compression
//! throughput goal is stated (>= 80% of C compression throughput, AAP 0.7.2).
//!
//! Strategy note: the one-call `compress2` API selects only the compression
//! *level*. Strategy selection (`Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE`,
//! `Z_FIXED`) is reached through the streaming `ZStream` API; strategy-specific
//! groups can be added here once that streaming surface is finalized. The
//! distinct data profiles below already stress the match-finder behavior that
//! the strategies target.
//!
//! Registered in `Cargo.toml` as `[[bench]] name = "deflate_bench"` with
//! `harness = false`.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use zlib_rs::compress2;
use zlib_rs::util::compress::compress_bound;

/// Payload size used by the deflate benchmarks (64 KiB).
const SIZE: usize = 64 * 1024;

/// Deterministic xorshift64 generator for incompressible, high-entropy input.
fn xorshift_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state >> 24) as u8);
    }
    out
}

/// Compressible, text-like data built by repeating an ASCII sentence.
fn text_like_bytes(len: usize) -> Vec<u8> {
    const SAMPLE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let take = SAMPLE.len().min(len - out.len());
        out.extend_from_slice(&SAMPLE[..take]);
    }
    out
}

/// Highly repetitive data built from a short repeating cycle.
fn repetitive_bytes(len: usize) -> Vec<u8> {
    const CYCLE: &[u8] = b"ABCD";
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let take = CYCLE.len().min(len - out.len());
        out.extend_from_slice(&CYCLE[..take]);
    }
    out
}

/// Compression throughput across all ten levels (`0..=9`) on compressible text.
fn bench_levels(c: &mut Criterion) {
    let data = text_like_bytes(SIZE);
    let bound = compress_bound(data.len());

    let mut group = c.benchmark_group("deflate_levels");
    group.throughput(Throughput::Bytes(data.len() as u64));
    for level in 0..=9 {
        group.bench_with_input(BenchmarkId::from_parameter(level), &level, |b, &level| {
            let mut dest = vec![0u8; bound];
            // Sanity: compression must succeed before we measure it.
            assert!(
                compress2(&mut dest, &data, level).is_ok(),
                "compress2 failed at level {level}"
            );
            b.iter(|| {
                let written = compress2(&mut dest, &data, level).expect("compress2 failed");
                black_box(written);
            });
        });
    }
    group.finish();
}

/// Compression throughput across input profiles at the default level (6).
fn bench_profiles(c: &mut Criterion) {
    const LEVEL: i32 = 6;
    let profiles: [(&str, Vec<u8>); 3] = [
        ("text", text_like_bytes(SIZE)),
        ("incompressible", xorshift_bytes(SIZE, 0xDEAD_BEEF)),
        ("repetitive", repetitive_bytes(SIZE)),
    ];

    let mut group = c.benchmark_group("deflate_profiles");
    for (name, data) in &profiles {
        let bound = compress_bound(data.len());
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("level6", *name), data, |b, data| {
            let mut dest = vec![0u8; bound];
            assert!(
                compress2(&mut dest, data, LEVEL).is_ok(),
                "compress2 failed for profile {name}"
            );
            b.iter(|| {
                let written = compress2(&mut dest, data, LEVEL).expect("compress2 failed");
                black_box(written);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_levels, bench_profiles);
criterion_main!(benches);
