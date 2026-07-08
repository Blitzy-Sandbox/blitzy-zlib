//! Criterion throughput benchmarks for the `zlib-rs` inflate (decompression) engine.
//!
//! Pre-compresses representative inputs with `compress2`, then measures
//! `uncompress` throughput. Throughput is reported over the *decompressed*
//! (original) size, matching the decompression throughput goal
//! (>= C decompression throughput, AAP 0.7.2). Decompression cost can vary with
//! how the source was compressed, so both the source level (1 / 6 / 9) and the
//! input profile (text vs. incompressible) are varied. The larger,
//! match-heavy inputs drive the `inflate_fast` hot path.
//!
//! Registered in `Cargo.toml` as `[[bench]] name = "inflate_bench"` with
//! `harness = false`.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use zlib_rs::util::compress::compress_bound;
use zlib_rs::{compress2, uncompress};

/// Payload size used by the inflate benchmarks (64 KiB).
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

/// Compress `data` at `level` and return exactly the produced compressed bytes.
fn deflate_to_vec(data: &[u8], level: i32) -> Vec<u8> {
    let mut buf = vec![0u8; compress_bound(data.len())];
    let n = compress2(&mut buf, data, level).expect("setup: compress2 failed");
    buf.truncate(n);
    buf
}

/// Decompression throughput as a function of the source compression level.
fn bench_by_level(c: &mut Criterion) {
    let original = text_like_bytes(SIZE);
    let orig_len = original.len();

    let mut group = c.benchmark_group("inflate_by_level");
    group.throughput(Throughput::Bytes(orig_len as u64));
    for level in [1i32, 6, 9] {
        let compressed = deflate_to_vec(&original, level);
        group.bench_with_input(
            BenchmarkId::from_parameter(level),
            &compressed,
            |b, compressed| {
                let mut dest = vec![0u8; orig_len];
                assert!(
                    uncompress(&mut dest, compressed).is_ok(),
                    "uncompress failed for source level {level}"
                );
                b.iter(|| {
                    let produced = uncompress(&mut dest, compressed).expect("uncompress failed");
                    black_box(produced);
                });
            },
        );
    }
    group.finish();
}

/// Decompression throughput as a function of the input profile (source level 6).
fn bench_by_profile(c: &mut Criterion) {
    let profiles: [(&str, Vec<u8>); 2] = [
        ("text", text_like_bytes(SIZE)),
        ("incompressible", xorshift_bytes(SIZE, 0x1357_9BDF)),
    ];

    let mut group = c.benchmark_group("inflate_by_profile");
    for (name, original) in &profiles {
        let orig_len = original.len();
        let compressed = deflate_to_vec(original, 6);
        group.throughput(Throughput::Bytes(orig_len as u64));
        group.bench_with_input(
            BenchmarkId::new("level6", *name),
            &compressed,
            |b, compressed| {
                let mut dest = vec![0u8; orig_len];
                assert!(
                    uncompress(&mut dest, compressed).is_ok(),
                    "uncompress failed for profile {name}"
                );
                b.iter(|| {
                    let produced = uncompress(&mut dest, compressed).expect("uncompress failed");
                    black_box(produced);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_by_level, bench_by_profile);
criterion_main!(benches);
