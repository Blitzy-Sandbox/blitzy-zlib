// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Compression throughput benchmarks across DEFLATE levels 0-9.
//
// This benchmark file measures the performance of the DEFLATE compression
// engine across all 10 compression levels, all 5 strategies, multiple data
// types, input sizes, streaming chunk sizes, and window sizes.  It is the
// primary benchmark for validating AAP §0.8.3: "Compression throughput must
// be within 80% of C zlib at equivalent compression levels."
//
// Reference: deflate.c configuration_table — levels 0-9 map to different
// strategy functions (deflate_stored for level 0, deflate_fast for levels
// 1-3, deflate_slow for levels 4-9).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use zlib_rs::constants::{
    DEF_MEM_LEVEL, MAX_WBITS, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED,
    Z_FINISH, Z_FIXED, Z_HUFFMAN_ONLY, Z_NO_FLUSH, Z_RLE,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::{compress2, compress_bound, deflate, deflate_bound, deflate_end, deflate_init2, ZStream};

// ===========================================================================
// Test data generators
// ===========================================================================
// All generators are deterministic (no `rand` crate) for reproducible
// benchmarks across runs and platforms.

/// Highly compressible data: all zeros.
///
/// Tests stored/fast path efficiency — level 0 simply stores these as-is,
/// while higher levels achieve near-infinite compression ratios.
fn zeros_data(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

/// Text-like data: English-ish repeating pattern.
///
/// Good for testing LZ77 match finding (deflate_fast / deflate_slow).
/// The 46-byte sentence cycles, producing many matchable substrings at
/// varying distances — a realistic proxy for natural-language text.
fn text_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    pattern.iter().cycle().take(size).copied().collect()
}

/// Binary/random-looking data: hard to compress.
///
/// Uses a Knuth multiplicative hash to generate a pseudo-random byte
/// sequence.  Tests worst-case compression overhead where every level
/// essentially falls back to stored blocks.
fn binary_data(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| ((i as u64).wrapping_mul(2_654_435_761) >> 16) as u8)
        .collect()
}

/// Run-length data: long runs of the same byte (ideal for RLE strategy).
///
/// Each run is 64–255 bytes of the same byte value, cycling through all
/// 256 values.  This is the best-case scenario for `Z_RLE` and also
/// compresses well with `deflate_fast` at low levels.
fn rle_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut byte = 0u8;
    let mut remaining = size;
    while remaining > 0 {
        let run_len = std::cmp::min(remaining, 64 + (byte as usize % 192));
        data.extend(std::iter::repeat_n(byte, run_len));
        remaining -= run_len;
        byte = byte.wrapping_add(1);
    }
    data
}

// ===========================================================================
// Benchmark: Compression levels 0-9 (primary benchmark)
// ===========================================================================
// Reference: deflate.c configuration_table (lines 98-123):
//   Level 0: deflate_stored — no compression
//   Levels 1-3: deflate_fast — greedy matching (good_length=4, max_lazy=4-6)
//   Levels 4-9: deflate_slow — lazy matching (good_length=4-32, max_chain=16-4096)

fn bench_deflate_by_level(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_level");

    // 64 KB of text — representative workload for level comparison.
    let data = text_data(65536);

    for level in 0..=9 {
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("text_64k", level), &data, |b, data| {
            b.iter(|| {
                let bound = compress_bound(data.len());
                let mut compressed = vec![0u8; bound];
                compress2(&mut compressed, data, level).expect("compression must succeed")
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: All 5 strategies (AAP §0.8.1)
// ===========================================================================
// Tests: DEFAULT=0, FILTERED=1, HUFFMAN_ONLY=2, RLE=3, FIXED=4
//
// Reference: deflate.c strategy selection —
//   Z_HUFFMAN_ONLY → deflate_huff
//   Z_RLE          → deflate_rle
//   DEFAULT/FILTERED/FIXED → deflate_stored/fast/slow based on level

fn bench_deflate_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_strategy");

    // 64 KB text at level 6 (default) — exercises deflate_slow.
    let data = text_data(65536);
    let level = 6;

    let strategies: &[(&str, i32)] = &[
        ("default", Z_DEFAULT_STRATEGY),
        ("filtered", Z_FILTERED),
        ("huffman_only", Z_HUFFMAN_ONLY),
        ("rle", Z_RLE),
        ("fixed", Z_FIXED),
    ];

    for &(name, strategy) in strategies {
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function(BenchmarkId::new(name, "text_64k"), |b| {
            b.iter(|| {
                let mut stream = ZStream::new();
                deflate_init2(
                    &mut stream,
                    level,
                    Z_DEFLATED,
                    MAX_WBITS,     // 15 — zlib format
                    DEF_MEM_LEVEL, // 8
                    strategy,
                )
                .expect("deflate_init2 must succeed");

                let bound = deflate_bound(&stream, data.len());
                let mut compressed = vec![0u8; bound];
                stream.set_input(&data);
                stream.set_output(&mut compressed);

                let result = deflate(&mut stream, Z_FINISH);
                assert!(
                    matches!(result, Ok(ReturnCode::StreamEnd)),
                    "deflate must finish: {result:?}"
                );

                let written = stream.total_out as usize;
                let _ = deflate_end(&mut stream);
                compressed.truncate(written);
                compressed
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: Data type impact
// ===========================================================================
// Shows how data characteristics affect compression throughput.

fn bench_deflate_data_types(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_data_type");

    let size: usize = 65536; // 64 KB
    let level = Z_DEFAULT_COMPRESSION; // resolves to level 6

    let datasets: Vec<(&str, Vec<u8>)> = vec![
        ("zeros", zeros_data(size)),
        ("text", text_data(size)),
        ("binary", binary_data(size)),
        ("rle", rle_data(size)),
    ];

    for (name, data) in &datasets {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new(*name, size), data, |b, data| {
            b.iter(|| {
                let bound = compress_bound(data.len());
                let mut compressed = vec![0u8; bound];
                compress2(&mut compressed, data, level).expect("compression must succeed")
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: Input size scaling (1 KB – 1 MB)
// ===========================================================================
// Measures how compression throughput scales with input size.

fn bench_deflate_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_size");

    let level = Z_DEFAULT_COMPRESSION;

    // 1 KB, 4 KB, 16 KB, 64 KB, 256 KB, 1 MB
    let sizes: &[usize] = &[1024, 4096, 16384, 65536, 262144, 1_048_576];

    for &size in sizes {
        let data = text_data(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("text", size), &data, |b, data| {
            b.iter(|| {
                let bound = compress_bound(data.len());
                let mut compressed = vec![0u8; bound];
                compress2(&mut compressed, data, level).expect("compression must succeed")
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: Streaming deflate with controlled chunk sizes
// ===========================================================================
// Models real-world usage where data arrives in chunks.  Matches the
// streaming pattern from test/example.c test_deflate (avail_in/avail_out
// controlled per call).

fn bench_deflate_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_streaming");

    // 64 KB of text data.
    let data = text_data(65536);

    // Test different input chunk sizes — from small (256 B) to full-buffer.
    let chunk_sizes: &[usize] = &[256, 1024, 4096, 16384, 65536];

    for &chunk_size in chunk_sizes {
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("chunk", chunk_size), &data, |b, data| {
            b.iter(|| {
                let mut stream = ZStream::new();
                deflate_init2(
                    &mut stream,
                    Z_DEFAULT_COMPRESSION,
                    Z_DEFLATED,
                    MAX_WBITS,
                    DEF_MEM_LEVEL,
                    Z_DEFAULT_STRATEGY,
                )
                .expect("deflate_init2 must succeed");

                let bound = deflate_bound(&stream, data.len());
                let mut compressed = vec![0u8; bound];
                let mut in_offset: usize = 0;
                let mut out_offset: usize = 0;

                // Feed data in chunks, mimicking streaming callers.
                while in_offset < data.len() {
                    let remaining_in = data.len() - in_offset;
                    let this_chunk = std::cmp::min(remaining_in, chunk_size);
                    let is_last = in_offset + this_chunk >= data.len();
                    let flush = if is_last { Z_FINISH } else { Z_NO_FLUSH };

                    stream.set_input(&data[in_offset..in_offset + this_chunk]);
                    stream.set_output(&mut compressed[out_offset..]);

                    let result = deflate(&mut stream, flush);
                    in_offset += this_chunk - stream.avail_in as usize;
                    out_offset = stream.total_out as usize;

                    if matches!(result, Ok(ReturnCode::StreamEnd)) {
                        break;
                    }
                }

                let _ = deflate_end(&mut stream);
                compressed.truncate(out_offset);
                compressed
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: Window sizes 9-15 (512 B – 32 KB)
// ===========================================================================
// Reference: deflate.c — windowBits controls window size (2^wbits bytes).
// Larger windows allow longer matches but use more memory.

fn bench_deflate_window_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_window");

    // 64 KB of text data.
    let data = text_data(65536);

    // Window sizes 9–15 (512 bytes to 32 KB).
    for wbits in 9..=15 {
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("wbits", wbits), &data, |b, data| {
            b.iter(|| {
                let mut stream = ZStream::new();
                deflate_init2(
                    &mut stream,
                    Z_DEFAULT_COMPRESSION,
                    Z_DEFLATED,
                    wbits,
                    DEF_MEM_LEVEL,
                    Z_DEFAULT_STRATEGY,
                )
                .expect("deflate_init2 must succeed");

                let bound = deflate_bound(&stream, data.len());
                let mut compressed = vec![0u8; bound];
                stream.set_input(data);
                stream.set_output(&mut compressed);

                let result = deflate(&mut stream, Z_FINISH);
                assert!(
                    matches!(result, Ok(ReturnCode::StreamEnd)),
                    "deflate must finish: {result:?}"
                );

                let written = stream.total_out as usize;
                let _ = deflate_end(&mut stream);
                compressed.truncate(written);
                compressed
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Benchmark: compressBound verification (constant-time formula)
// ===========================================================================
// Reference: compress.c compressBound formula:
//   sourceLen + (sourceLen >> 12) + (sourceLen >> 14) + (sourceLen >> 25) + 13
//
// This benchmark verifies the function is effectively free (constant time).

fn bench_compress_bound(c: &mut Criterion) {
    c.bench_function("compress_bound", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for size in [0, 1, 100, 1_000, 10_000, 100_000, 1_000_000] {
                total += compress_bound(size);
            }
            total
        });
    });
}

// ===========================================================================
// Criterion main wiring
// ===========================================================================

criterion_group!(
    benches,
    bench_deflate_by_level,
    bench_deflate_strategies,
    bench_deflate_data_types,
    bench_deflate_sizes,
    bench_deflate_streaming,
    bench_deflate_window_sizes,
    bench_compress_bound,
);

criterion_main!(benches);
