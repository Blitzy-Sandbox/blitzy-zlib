// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Decompression (inflate) throughput benchmarks.
//
// This benchmark suite measures the DEFLATE decompression throughput of the
// zlib-rs inflate engine across multiple dimensions: compression levels,
// data types (zeros, text, binary), window sizes, streaming vs. one-call API,
// and wrapper formats (zlib, raw DEFLATE, gzip).
//
// All throughput measurements report bytes per second of *uncompressed* data,
// since that is the metric that matters for decompression workloads.
//
// AAP §0.8.3: "Decompression throughput must match or exceed C zlib due to
// Rust's bounds-checking elision optimizations."

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use zlib_rs::constants::{
    DEF_MEM_LEVEL, MAX_WBITS, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::{
    ZStream, compress_bound, compress2, deflate, deflate_end, deflate_init2, inflate, inflate_end,
    inflate_init2, uncompress,
};

// ===========================================================================
// Test data generators
// ===========================================================================

/// Highly compressible data: all zeros.
///
/// Achieves extreme compression ratios (~1000:1 at level 9) and exercises the
/// RLE-like and stored-block paths most heavily.
fn zeros_data(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

/// Text-like data: repeating sentence with variation.
///
/// This exercises mid-range compression where LZ77 string matching finds
/// moderate-length matches at moderate distances. Represents a realistic
/// workload for compressing textual content (logs, JSON, HTML).
fn text_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    pattern.iter().cycle().take(size).copied().collect()
}

/// Binary-like data: pseudo-random but deterministic.
///
/// Uses a multiplicative hash (Knuth's constant 2654435761) to generate
/// uniformly-distributed byte values that compress poorly, stressing the
/// Huffman-only code paths and exercising inflate's literal decoding.
///
/// IMPORTANT: No `rand` crate here — deterministic data ensures reproducible
/// benchmark results across runs.
fn binary_data(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 16) as u8)
        .collect()
}

// ===========================================================================
// Pre-compression helpers
// ===========================================================================

/// Pre-compress data at the given level using the one-call `compress2` API
/// (zlib wrapper format, default window bits).
///
/// Returns a `Vec<u8>` containing only the compressed bytes (truncated to
/// exact length). Panics if compression fails — benchmark setup must succeed.
fn pre_compress(data: &[u8], level: i32) -> Vec<u8> {
    let bound = compress_bound(data.len());
    let mut compressed = vec![0u8; bound];
    let compressed_len = compress2(&mut compressed, data, level)
        .expect("Pre-compression with compress2 must succeed");
    compressed.truncate(compressed_len);
    compressed
}

/// Pre-compress data with a specific window size using the streaming
/// `deflate_init2` / `deflate` / `deflate_end` API.
///
/// The `wbits` parameter controls both the window size and the wrapper format:
/// - 9..=15   → zlib wrapper (RFC 1950)
/// - -15..=-9 → raw DEFLATE (RFC 1951)
/// - 25..=31  → gzip wrapper (RFC 1952)
///
/// Returns a `Vec<u8>` containing only the compressed bytes. Panics if
/// compression fails.
fn pre_compress_with_window(data: &[u8], level: i32, wbits: i32) -> Vec<u8> {
    let mut stream = ZStream::new();
    deflate_init2(
        &mut stream,
        level,
        Z_DEFLATED,
        wbits,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    )
    .expect("deflate_init2 must succeed in benchmark setup");

    // Compute a generous upper bound for the compressed output.
    // This uses the same formula as compressBound plus extra headroom for
    // gzip headers when wbits > 15.
    let bound = data.len() + (data.len() >> 12) + (data.len() >> 14) + (data.len() >> 25) + 64;
    let mut compressed = vec![0u8; bound];

    stream.set_input(data);
    stream.set_output(&mut compressed);

    let result = deflate(&mut stream, Z_FINISH);
    assert!(
        matches!(result, Ok(ReturnCode::StreamEnd)),
        "deflate with Z_FINISH must return StreamEnd, got: {result:?}"
    );

    let written = stream.total_out as usize;
    let _ = deflate_end(&mut stream);
    compressed.truncate(written);
    compressed
}

// ===========================================================================
// Benchmark: One-call inflate via `uncompress`
// ===========================================================================

/// Benchmarks the one-call `uncompress` API across compression levels 1, 6,
/// and 9 with 64 KB of text data.
///
/// This measures the simplest decompression path: a single call that performs
/// `inflate_init → inflate → inflate_end` internally. Throughput is reported
/// in bytes per second of the *original* (uncompressed) data.
fn bench_inflate_oneshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_oneshot");

    let original_size: usize = 65536; // 64 KB
    let data = text_data(original_size);

    for &level in &[1, 6, 9] {
        let compressed = pre_compress(&data, level);

        // Throughput is measured against the uncompressed data size, which is
        // the metric that matters for decompression throughput.
        group.throughput(Throughput::Bytes(original_size as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("level_{level}"), original_size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut output = vec![0u8; original_size];
                    let result = uncompress(&mut output, compressed);
                    assert!(result.is_ok(), "uncompress failed: {result:?}");
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Benchmark: Streaming inflate API
// ===========================================================================

/// Benchmarks the streaming `inflate_init2` / `inflate` / `inflate_end` API
/// with default compression level across multiple data sizes (4 KB, 64 KB,
/// 1 MB).
///
/// This exercises the full streaming lifecycle per iteration, feeding all
/// compressed data at once and requesting decompression with `Z_FINISH`.
fn bench_inflate_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_streaming");

    let original_sizes: &[usize] = &[4096, 65536, 1_048_576]; // 4 KB, 64 KB, 1 MB

    for &orig_size in original_sizes {
        let data = text_data(orig_size);
        let compressed = pre_compress(&data, Z_DEFAULT_COMPRESSION);

        group.throughput(Throughput::Bytes(orig_size as u64));
        group.bench_with_input(
            BenchmarkId::new("default_level", orig_size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut stream = ZStream::new();
                    inflate_init2(&mut stream, MAX_WBITS).expect("inflate_init2 must succeed");

                    let mut output = vec![0u8; orig_size];
                    stream.set_input(compressed);
                    stream.set_output(&mut output);

                    let result = inflate(&mut stream, Z_FINISH);
                    assert!(
                        matches!(result, Ok(ReturnCode::StreamEnd)),
                        "inflate must return StreamEnd, got: {result:?}"
                    );

                    let _ = inflate_end(&mut stream);
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Benchmark: Inflate throughput by compression level
// ===========================================================================

/// Pre-compresses 64 KB of text data at each level 1–9, then measures inflate
/// throughput to show how compression level affects decompression speed.
///
/// Higher compression levels produce denser output with more complex Huffman
/// codes and longer back-references, which may affect decode speed. This
/// benchmark quantifies that relationship.
fn bench_inflate_by_level(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_by_level");

    let original_size: usize = 65536; // 64 KB
    let data = text_data(original_size);

    for level in 1..=9 {
        let compressed = pre_compress(&data, level);

        group.throughput(Throughput::Bytes(original_size as u64));
        group.bench_with_input(
            BenchmarkId::new("text_64k", level),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut output = vec![0u8; original_size];
                    let result = uncompress(&mut output, compressed);
                    assert!(result.is_ok(), "uncompress failed: {result:?}");
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Benchmark: Inflate throughput by data type
// ===========================================================================

/// Measures decompression speed across three data characteristics:
///
/// - **zeros**: Extremely compressible, exercises stored/RLE decode paths.
/// - **text**: Moderate compression, exercises LZ77 match copy paths.
/// - **binary**: Poor compression, exercises Huffman literal decode paths.
///
/// Each dataset is 64 KB compressed at the default level.
fn bench_inflate_by_data_type(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_data_type");
    let size: usize = 65536; // 64 KB

    let datasets: Vec<(&str, Vec<u8>)> = vec![
        ("zeros", zeros_data(size)),
        ("text", text_data(size)),
        ("binary", binary_data(size)),
    ];

    for (name, data) in &datasets {
        let compressed = pre_compress(data, Z_DEFAULT_COMPRESSION);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new(*name, size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut output = vec![0u8; size];
                    let result = uncompress(&mut output, compressed);
                    assert!(result.is_ok(), "uncompress failed: {result:?}");
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Benchmark: Inflate by window size
// ===========================================================================

/// Tests window sizes 9–15 (512 bytes through 32 KB) to measure their impact
/// on decompression speed.
///
/// Smaller windows limit the maximum back-reference distance, which can affect
/// the number of Huffman table entries and the copy loop behaviour in
/// `inflate_fast`. Larger windows allow longer-distance matches, potentially
/// improving compression ratio but requiring more memory.
fn bench_inflate_window_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_window");

    let original_size: usize = 65536; // 64 KB
    let data = text_data(original_size);

    for wbits in 9..=MAX_WBITS {
        // Pre-compress with streaming API using the specific window size.
        let compressed = pre_compress_with_window(&data, Z_DEFAULT_COMPRESSION, wbits);

        group.throughput(Throughput::Bytes(original_size as u64));
        group.bench_with_input(
            BenchmarkId::new("wbits", wbits),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut stream = ZStream::new();
                    inflate_init2(&mut stream, wbits).expect("inflate_init2 must succeed");

                    let mut output = vec![0u8; original_size];
                    stream.set_input(compressed);
                    stream.set_output(&mut output);

                    let result = inflate(&mut stream, Z_FINISH);
                    assert!(
                        matches!(result, Ok(ReturnCode::StreamEnd)),
                        "inflate must return StreamEnd for wbits={wbits}, got: {result:?}"
                    );

                    let _ = inflate_end(&mut stream);
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Benchmark: Inflate by wrapper format
// ===========================================================================

/// Compares decompression speed across the three supported wrapper formats:
///
/// | Format       | deflate wbits | inflate wbits | Description               |
/// |--------------|---------------|---------------|---------------------------|
/// | zlib         | 15            | 15            | RFC 1950 wrapper          |
/// | raw DEFLATE  | -15           | -15           | RFC 1951, no wrapper      |
/// | gzip         | 31 (15+16)    | 31 (15+16)    | RFC 1952 wrapper          |
/// | auto-detect  | 15 (zlib)     | 47 (15+32)    | auto-detect zlib or gzip  |
///
/// The "auto-detect" benchmark inflates zlib-wrapped data using windowBits=47,
/// which triggers format sniffing in the inflate engine's Head state.
fn bench_inflate_formats(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_formats");

    let original_size: usize = 65536; // 64 KB
    let data = text_data(original_size);

    // (format_name, deflate_wbits, inflate_wbits)
    let formats: &[(&str, i32, i32)] = &[
        ("zlib", 15, 15),
        ("raw_deflate", -15, -15),
        ("gzip", 15 + 16, 15 + 16),
        ("auto_detect", 15, 15 + 32), // compress as zlib, inflate with auto-detect
    ];

    for &(name, def_wbits, inf_wbits) in formats {
        let compressed = pre_compress_with_window(&data, Z_DEFAULT_COMPRESSION, def_wbits);

        group.throughput(Throughput::Bytes(original_size as u64));
        group.bench_with_input(
            BenchmarkId::new(name, original_size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut stream = ZStream::new();
                    inflate_init2(&mut stream, inf_wbits).expect("inflate_init2 must succeed");

                    let mut output = vec![0u8; original_size];
                    stream.set_input(compressed);
                    stream.set_output(&mut output);

                    let result = inflate(&mut stream, Z_FINISH);
                    assert!(
                        matches!(result, Ok(ReturnCode::StreamEnd)),
                        "inflate must return StreamEnd for {name}, got: {result:?}"
                    );

                    let _ = inflate_end(&mut stream);
                    output
                });
            },
        );
    }
    group.finish();
}

// ===========================================================================
// Criterion group and main
// ===========================================================================

criterion_group!(
    benches,
    bench_inflate_oneshot,
    bench_inflate_streaming,
    bench_inflate_by_level,
    bench_inflate_by_data_type,
    bench_inflate_window_sizes,
    bench_inflate_formats,
);
criterion_main!(benches);
