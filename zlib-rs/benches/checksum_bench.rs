//! Criterion-based benchmark suite for the Adler-32 and CRC-32 checksum engines.
//!
//! Measures throughput across various buffer sizes, data patterns, and usage modes
//! (single-call, incremental, combine). Key boundaries tested:
//!
//! - Adler-32 NMAX = 5552 block optimization (deferred modulo)
//! - CRC-32 table-based lookup with 8-byte unrolled inner loop
//! - Combine operations (algebraic Adler-32, GF(2) polynomial CRC-32)
//!
//! Run with: `cargo bench -p zlib-rs --bench checksum_bench`

// Benchmark binaries do not require documentation on every item.
// The `criterion_group!` and `criterion_main!` macros generate functions
// that cannot have doc-comments attached.
#![allow(missing_docs)]
#![allow(clippy::all)]
#![allow(clippy::pedantic)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use zlib_rs::checksum::adler32::{adler32, adler32_combine};
use zlib_rs::checksum::crc32::{crc32, crc32_combine};

// ---------------------------------------------------------------------------
// Test data generators
// ---------------------------------------------------------------------------

/// Generate a buffer filled with cycling byte values `0..=255` for realistic
/// checksum benchmarking. This pattern exercises all 256 byte values uniformly.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

/// Generate a zero-filled buffer. This is a degenerate case where Adler-32's
/// `s1` accumulator does not change, stressing only the `s2 += s1` path.
fn generate_zero_data(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

/// Generate pseudo-random data using a linear congruential generator (LCG)
/// for reproducible benchmarks. The constants match the classic POSIX `rand()`
/// parameters, and the output byte is taken from bits 16–23 to avoid the
/// low-bit correlation typical of multiplicative LCGs.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut state: u32 = 0x1234_5678;
    for _ in 0..size {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        data.push((state >> 16) as u8);
    }
    data
}

// ---------------------------------------------------------------------------
// Benchmark 1: Adler-32 across buffer sizes
// ---------------------------------------------------------------------------

/// Benchmark Adler-32 across buffer sizes that exercise every major code path:
///
/// | Size      | Path exercised                                       |
/// |-----------|------------------------------------------------------|
/// | 1 byte    | Single-byte fast path (`if buf.len() == 1`)          |
/// | 15 bytes  | Short-length path (`if buf.len() < 16`)              |
/// | 256 bytes | Small buffer, single NMAX block (no modulo needed)   |
/// | 5 552     | Exact NMAX boundary — one full block, one modulo     |
/// | 5 553     | Just above NMAX — triggers a second modulo pass      |
/// | 65 536    | 64 KB typical I/O buffer                             |
/// | 1 048 576 | 1 MB sustained throughput                            |
fn bench_adler32_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32_sizes");

    // Sizes chosen to exercise specific code paths in the Adler-32 implementation.
    let sizes: &[usize] = &[
        1,         // Single byte fast path
        15,        // Below 16-byte threshold
        256,       // Small buffer
        5552,      // NMAX boundary (adler32.c line 11: NMAX = 5552)
        5553,      // Just above NMAX — triggers second modulo pass
        65_536,    // 64 KB typical buffer
        1_048_576, // 1 MB sustained throughput
    ];

    for &size in sizes {
        let data = generate_test_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("bytes", size), &data, |b, data| {
            b.iter(|| {
                // Initial adler value is 1 per zlib convention (s1=1, s2=0)
                adler32(1, data)
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 2: CRC-32 across buffer sizes
// ---------------------------------------------------------------------------

/// Benchmark CRC-32 across buffer sizes spanning single-byte to 16 MB.
///
/// The CRC-32 implementation uses an 8-byte unrolled inner loop. Sizes are
/// chosen to test alignment handling, page-boundary behavior, and sustained
/// throughput on large buffers where CPU cache effects dominate.
fn bench_crc32_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("crc32_sizes");

    let sizes: &[usize] = &[
        1,          // Single byte
        16,         // One unrolled iteration (8 bytes) + 8 remaining
        256,        // Small buffer
        4096,       // Page size
        65_536,     // 64 KB
        1_048_576,  // 1 MB
        16_777_216, // 16 MB — sustained throughput, exercises cache hierarchy
    ];

    for &size in sizes {
        let data = generate_test_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("bytes", size), &data, |b, data| {
            b.iter(|| {
                // Initial CRC value is 0 per zlib convention
                crc32(0, data)
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 3: Adler-32 NMAX boundary
// ---------------------------------------------------------------------------

/// Specifically test the NMAX = 5552 block optimization boundary.
///
/// The Adler-32 algorithm accumulates `s2 += s1` which grows rapidly. To
/// prevent `u32` overflow, a modulo by BASE (65 521) is performed once per
/// NMAX-sized block. This benchmark measures throughput at, below, and above
/// the NMAX boundary to verify the Rust implementation achieves the same
/// deferred-modulo optimization as the C original.
///
/// | Size   | Blocks | Description                     |
/// |--------|--------|---------------------------------|
/// | 5 000  | 0 + 1  | Below NMAX — single partial     |
/// | 5 552  | 1      | Exactly NMAX — one full block   |
/// | 5 600  | 1 + 1  | Just above — two passes         |
/// | 11 104 | 2      | Exactly 2×NMAX                  |
/// | 55 520 | 10     | 10×NMAX — amortized overhead    |
fn bench_adler32_nmax(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32_nmax");

    let sizes: &[usize] = &[
        5000,  // Below NMAX — single pass, no full-block modulo
        5552,  // Exactly NMAX — one full block
        5600,  // Just above NMAX — two passes required
        11104, // Exactly 2 × NMAX — two full blocks
        55520, // 10 × NMAX — ten full blocks
    ];

    for &size in sizes {
        let data = generate_test_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("bytes", size), &data, |b, data| {
            b.iter(|| adler32(1, data));
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 4: Checksum combine operations
// ---------------------------------------------------------------------------

/// Benchmark the `adler32_combine()` and `crc32_combine()` functions.
///
/// These functions combine two independently computed checksums for
/// concatenated data without re-processing the original bytes:
///
/// - `adler32_combine` uses modular arithmetic over BASE = 65 521.
/// - `crc32_combine` uses GF(2) polynomial multiplication via `multmodp()`.
///
/// The combine operation cost is independent of data size (depends only on
/// `len2`), so this measures the fixed overhead of the algebraic combination.
fn bench_checksum_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_combine");

    // Prepare two 32 KB data chunks and their pre-computed checksums.
    let chunk1 = generate_test_data(32_768);
    let chunk2 = generate_test_data(32_768);
    let adler1 = adler32(1, &chunk1);
    let adler2 = adler32(1, &chunk2);
    let crc1 = crc32(0, &chunk1);
    let crc2 = crc32(0, &chunk2);
    let len2 = chunk2.len() as i64;

    group.bench_function("adler32_combine", |b| {
        b.iter(|| adler32_combine(adler1, adler2, len2));
    });

    group.bench_function("crc32_combine", |b| {
        b.iter(|| crc32_combine(crc1, crc2, len2));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 5: Data pattern impact
// ---------------------------------------------------------------------------

/// Test checksum performance with different data patterns at a fixed 64 KB size.
///
/// Data patterns can affect branch prediction and CPU micro-architecture behavior
/// (though checksums are largely data-independent in terms of operations):
///
/// - **sequential** — cycling 0..255 values (typical text-like entropy)
/// - **zeros** — all-zero buffer (degenerate Adler-32 case: s1 constant)
/// - **random** — pseudo-random bytes (maximum entropy)
fn bench_checksum_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_patterns");
    let size: usize = 65_536;
    group.throughput(Throughput::Bytes(size as u64));

    let patterns: Vec<(&str, Vec<u8>)> = vec![
        ("sequential", generate_test_data(size)),
        ("zeros", generate_zero_data(size)),
        ("random", generate_random_data(size)),
    ];

    for (name, data) in &patterns {
        group.bench_with_input(BenchmarkId::new("adler32", *name), data, |b, data| {
            b.iter(|| adler32(1, data))
        });

        group.bench_with_input(BenchmarkId::new("crc32", *name), data, |b, data| {
            b.iter(|| crc32(0, data))
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 6: Incremental / streaming checksum updates
// ---------------------------------------------------------------------------

/// Benchmark incremental (streaming) checksum computation.
///
/// In real-world compression, checksums are updated incrementally as data
/// arrives in chunks of varying sizes. This benchmark measures the amortized
/// cost of repeated `adler32()` and `crc32()` calls on a fixed 64 KB buffer
/// split into chunks of 1 B, 16 B, 256 B, 4 KB, and 64 KB.
///
/// Small chunks stress function-call overhead and the short-path optimizations.
/// Large chunks maximize throughput of the inner unrolled loops.
fn bench_checksum_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_incremental");
    let data = generate_test_data(65_536);
    group.throughput(Throughput::Bytes(data.len() as u64));

    let chunk_sizes: &[usize] = &[1, 16, 256, 4096, 65_536];

    for &chunk_size in chunk_sizes {
        group.bench_with_input(
            BenchmarkId::new("adler32_chunk", chunk_size),
            &chunk_size,
            |b, &cs| {
                b.iter(|| {
                    let mut checksum: u32 = 1; // Adler-32 initial value
                    for chunk in data.chunks(cs) {
                        checksum = adler32(checksum, chunk);
                    }
                    checksum
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("crc32_chunk", chunk_size),
            &chunk_size,
            |b, &cs| {
                b.iter(|| {
                    let mut checksum: u32 = 0; // CRC-32 initial value
                    for chunk in data.chunks(cs) {
                        checksum = crc32(checksum, chunk);
                    }
                    checksum
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness registration
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_adler32_sizes,
    bench_crc32_sizes,
    bench_adler32_nmax,
    bench_checksum_combine,
    bench_checksum_patterns,
    bench_checksum_incremental,
);
criterion_main!(benches);
