// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib

//! Adler-32 and CRC-32 throughput benchmarks.
//!
//! This benchmark suite measures the performance of the zlib-rs checksum
//! implementations across multiple buffer sizes (1 KB, 4 KB, 64 KB, 1 MB),
//! covering one-shot computation, incremental (streaming) computation, and
//! checksum combine operations.
//!
//! These benchmarks validate the AAP §0.8.3 performance expectations:
//! - CRC-32: SIMD-accelerated via `crc32fast`, matching or exceeding C zlib's
//!   braided table performance.
//! - Adler-32: Competitive with C zlib's DO16 unrolled implementation.
//!
//! Run with: `cargo bench --bench checksum_bench`

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use zlib_rs::checksum::{adler32, adler32_combine, adler32_z, crc32, crc32_combine, crc32_z};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Buffer sizes to benchmark across.
///
/// - 1 KB  (1024):    small buffer, tests per-call overhead
/// - 4 KB  (4096):    typical I/O block size
/// - 64 KB (65536):   large buffer, approaches steady-state throughput
/// - 1 MB  (1048576): bulk data, measures sustained throughput
const SIZES: &[usize] = &[1024, 4096, 65536, 1_048_576];

/// Chunk size for incremental benchmarks (1 KB).
///
/// Simulates a streaming workload where data arrives in 1 KB blocks,
/// exercising the overhead of repeated function calls with partial updates.
const INCREMENTAL_CHUNK: usize = 1024;

// ---------------------------------------------------------------------------
// Deterministic test data generation
// ---------------------------------------------------------------------------

/// Generate a deterministic pseudo-random test data buffer of the given size.
///
/// Uses a Knuth multiplicative hash (golden ratio constant 2654435761) applied
/// to the byte index to produce a well-distributed byte pattern. This is
/// reproducible across runs and platforms, which is essential for benchmarks
/// (the `rand` crate is reserved for tests, not benchmarks).
///
/// The resulting data has good byte distribution, avoiding pathological patterns
/// that would give misleading checksum performance numbers (e.g., all-zeros
/// data exercises a minimum-work path in Adler-32).
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 16) as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Adler-32 benchmarks
// ---------------------------------------------------------------------------

/// Benchmark Adler-32 checksum computation across multiple buffer sizes and
/// access patterns.
///
/// Sub-benchmarks:
/// - `adler32/compute/{size}` — Full buffer one-shot computation
/// - `adler32/incremental/{size}` — Streaming computation in 1 KB chunks
/// - `adler32/single_byte` — Single-byte update throughput (exercises the
///   fast path at adler32.c line 70: `if (len == 1)`)
fn bench_adler32(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32");

    // --- One-shot computation at each buffer size ---
    for &size in SIZES {
        let data = generate_test_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        // Full-buffer one-shot computation using adler32_z.
        group.bench_with_input(BenchmarkId::new("compute", size), &data, |b, data| {
            b.iter(|| adler32_z(1, data));
        });

        // Incremental computation in INCREMENTAL_CHUNK-byte blocks, measuring
        // the overhead of repeated function calls with partial updates.
        group.bench_with_input(
            BenchmarkId::new("incremental", size),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut checksum: u32 = 1;
                    for chunk in data.chunks(INCREMENTAL_CHUNK) {
                        checksum = adler32_z(checksum, chunk);
                    }
                    checksum
                });
            },
        );
    }

    // --- Single-byte update throughput ---
    // The Adler-32 implementation has a dedicated fast path for single-byte
    // updates (the `if buf.len() == 1` branch in adler32_z). Benchmarking
    // this path reveals per-call overhead and branch predictor efficiency.
    group.throughput(Throughput::Bytes(1));
    group.bench_function("single_byte", |b| {
        b.iter(|| adler32(1, &[0x42]));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// CRC-32 benchmarks
// ---------------------------------------------------------------------------

/// Benchmark CRC-32 checksum computation across multiple buffer sizes and
/// access patterns.
///
/// When the `simd` feature is enabled (default), CRC-32 computation is
/// hardware-accelerated via `crc32fast` using x86 SSE/PCLMULQDQ and
/// AArch64 CRC instructions.
///
/// Sub-benchmarks:
/// - `crc32/compute/{size}` — Full buffer one-shot computation
/// - `crc32/incremental/{size}` — Streaming computation in 1 KB chunks
/// - `crc32/single_byte` — Single-byte update throughput
fn bench_crc32(c: &mut Criterion) {
    let mut group = c.benchmark_group("crc32");

    // --- One-shot computation at each buffer size ---
    for &size in SIZES {
        let data = generate_test_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        // Full-buffer one-shot computation using crc32_z.
        group.bench_with_input(BenchmarkId::new("compute", size), &data, |b, data| {
            b.iter(|| crc32_z(0, data));
        });

        // Incremental computation in INCREMENTAL_CHUNK-byte blocks.
        group.bench_with_input(
            BenchmarkId::new("incremental", size),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut checksum: u32 = 0;
                    for chunk in data.chunks(INCREMENTAL_CHUNK) {
                        checksum = crc32_z(checksum, chunk);
                    }
                    checksum
                });
            },
        );
    }

    // --- Single-byte update throughput ---
    group.throughput(Throughput::Bytes(1));
    group.bench_function("single_byte", |b| {
        b.iter(|| crc32(0, &[0x42]));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Combine operation benchmarks
// ---------------------------------------------------------------------------

/// Benchmark the checksum combine operations for both Adler-32 and CRC-32.
///
/// Combine operations allow the checksum of the concatenation of two byte
/// sequences to be computed from the individual checksums and the length of
/// the second sequence, without re-processing any data. This is valuable for
/// parallel and incremental checksum computation over large datasets.
///
/// - Adler-32 combine uses modular arithmetic over ℤ/65521ℤ
///   (adler32.c `adler32_combine_`)
/// - CRC-32 combine uses GF(2) matrix multiplication
///   (crc32.c `crc32_combine` via `multmodp`)
///
/// The CRC-32 combine is expected to be more expensive than Adler-32 combine
/// due to the GF(2) polynomial multiplication requiring up to 32 iterations.
fn bench_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("combine");

    // Pre-compute checksums on a 64 KB buffer split in half (32 KB each).
    let data = generate_test_data(65536);
    let half = data.len() / 2;

    let adler_a = adler32_z(1, &data[..half]);
    let adler_b = adler32_z(1, &data[half..]);
    let crc_a = crc32_z(0, &data[..half]);
    let crc_b = crc32_z(0, &data[half..]);

    let half_i64 = half as i64;

    // Adler-32 combine: fast modular arithmetic operation.
    group.bench_function("adler32_combine", |b| {
        b.iter(|| adler32_combine(adler_a, adler_b, half_i64));
    });

    // CRC-32 combine: GF(2) polynomial multiplication (more compute-intensive).
    group.bench_function("crc32_combine", |b| {
        b.iter(|| crc32_combine(crc_a, crc_b, half_i64));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness setup
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_adler32, bench_crc32, bench_combine);
criterion_main!(benches);
