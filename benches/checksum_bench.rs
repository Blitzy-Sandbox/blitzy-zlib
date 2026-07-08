//! Criterion throughput benchmarks for the `zlib-rs` checksum functions.
//!
//! Measures Adler-32 and CRC-32 throughput across a range of buffer sizes.
//! The CRC-32 hot path is backed by `crc32fast`; to compare the scalar
//! fallback against the SIMD-accelerated implementation, run this benchmark
//! with and without the crate's `simd` feature:
//!
//! - `cargo bench --bench checksum_bench`
//! - `cargo bench --bench checksum_bench --features simd`
//!
//! Registered in `Cargo.toml` as `[[bench]] name = "checksum_bench"` with
//! `harness = false`, so this file supplies its own entry point via
//! `criterion_group!` / `criterion_main!` (not the libtest harness).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use zlib_rs::{adler32, crc32};

/// Buffer sizes exercised by every checksum benchmark: 1 KiB, 16 KiB, 256 KiB.
const SIZES: [usize; 3] = [1 << 10, 1 << 14, 1 << 18];

/// Deterministic, dependency-free pseudo-random byte generator (xorshift64).
/// Produces high-entropy (incompressible-looking) data so the checksum loop is
/// exercised on realistic input without pulling in the `rand` crate.
fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
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

fn bench_adler32(c: &mut Criterion) {
    // Correctness guard using the canonical zlib Adler-32 identity vector.
    assert_eq!(adler32(1, b""), 1, "adler32 identity vector");

    let mut group = c.benchmark_group("adler32");
    for &size in &SIZES {
        let data = pseudo_random_bytes(size, 0x51ED_5EED);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            b.iter(|| black_box(adler32(1, black_box(data.as_slice()))));
        });
    }
    group.finish();
}

fn bench_crc32(c: &mut Criterion) {
    // Correctness guard using the canonical "123456789" CRC-32 check value.
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926, "crc32 check vector");

    let mut group = c.benchmark_group("crc32");
    for &size in &SIZES {
        let data = pseudo_random_bytes(size, 0xC0FF_EE00);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            b.iter(|| black_box(crc32(0, black_box(data.as_slice()))));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_adler32, bench_crc32);
criterion_main!(benches);
