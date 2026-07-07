//! Criterion throughput benchmark for the checksum layer.
//!
//! Measures per-byte throughput of the running `crc32` and `adler32` checksums
//! over a fixed buffer. Uses the public `zlib_rs` API only; with the default
//! `simd` feature the CRC-32 hot path is delegated to `crc32fast`.
//!
//! Run with: `cargo bench --bench checksum_bench`.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use zlib_rs::{adler32, crc32};

/// Number of bytes checksummed per iteration.
const INPUT_LEN: usize = 64 * 1024;

/// Builds `len` bytes of deterministic pseudo-random data. Checksums are
/// content-insensitive for throughput purposes, so a cheap LCG-driven fill is
/// sufficient and keeps the benchmark reproducible across runs.
fn test_data(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u32 = 0x1234_5678;
    while out.len() < len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((state >> 24) as u8);
    }
    out
}

/// Benchmarks the running `crc32` and `adler32` checksum functions.
fn bench_checksum(c: &mut Criterion) {
    let input = test_data(INPUT_LEN);

    let mut group = c.benchmark_group("checksum");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.bench_function("crc32", |b| {
        // CRC-32 seeds from 0 per the zlib external convention.
        b.iter(|| black_box(crc32(black_box(0), black_box(&input))));
    });
    group.bench_function("adler32", |b| {
        // Adler-32 seeds from 1 per the zlib external convention.
        b.iter(|| black_box(adler32(black_box(1), black_box(&input))));
    });
    group.finish();
}

criterion_group!(benches, bench_checksum);
criterion_main!(benches);
