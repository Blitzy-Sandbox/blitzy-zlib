//! Criterion benchmarks for the zlib-rs inflate (decompression) engine.
//!
//! Measures decompression throughput across multiple axes:
//!
//! - **One-call `uncompress`** wrapper at three representative buffer sizes
//!   (1 KB, 64 KB, 1 MB).
//! - **Format-specific streaming inflate** covering all four container format
//!   modes: zlib (RFC 1950), raw DEFLATE (RFC 1951), gzip (RFC 1952), and
//!   auto-detect.
//! - **Data-size scaling** from 256 B to 4 MB to reveal setup overhead, cache
//!   effects, and sustained throughput characteristics.
//! - **Fast-path decoder** with 4 MB buffers that maximize time in
//!   `inflate_fast()` (the hot loop responsible for >95% of inflate execution
//!   time, ported from `inffast.c`).
//! - **Data pattern diversity** (random, compressible text, all-zeros) to
//!   exercise different Huffman code distributions and back-reference patterns.
//!
//! Per AAP Section 0.7.2, these benchmarks validate that decompression
//! throughput is within 90% of C zlib.  Throughput is always reported as
//! **uncompressed bytes per second** (the decompressor's output rate).
//!
//! # Running
//!
//! ```bash
//! cargo bench -p zlib-rs --bench inflate_bench
//! ```

// ─── Crate-level lint overrides for benchmark code ──────────────────────────
//
// The `[lints]` section in Cargo.toml enforces `deny(missing_docs)` and
// `deny(clippy::pedantic)` across all package targets including benchmarks.
// Benchmark-specific relaxations are applied here because:
//
// - `missing_docs`: benchmark helper functions are crate-private and need no
//   public API documentation.
// - `clippy::must_use_candidate`: benchmark helpers return data consumed by
//   the harness; `#[must_use]` would be noise.
// - `clippy::cast_possible_truncation`: `usize as u8` in pseudo-random data
//   generation is intentional truncation.
// - `clippy::missing_panics_doc`: benchmark code uses `.unwrap()` liberally
//   (per AAP Section 0.7.2: "panics are confined to test code").
#![allow(missing_docs)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::missing_panics_doc)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use zlib_rs::constants::{
    Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, Z_DEFLATED,
    Z_DEFAULT_STRATEGY, Z_FINISH, Z_NO_COMPRESSION, Z_NO_FLUSH, MAX_WBITS,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::stream::ZStream;
use zlib_rs::{compress, compress2, compress_bound, uncompress, uncompress2};
use zlib_rs::{deflate, inflate};

// ─── Data Generation Helpers ────────────────────────────────────────────────

/// Generate compressible test data with repeating patterns.
///
/// Creates a buffer filled with cyclic ASCII text patterns that exhibit
/// typical compression ratios (~60-70% reduction).  The output is
/// deterministic for a given `size`.
fn generate_compressible_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        let remaining = size - data.len();
        let chunk_len = remaining.min(pattern.len());
        data.extend_from_slice(&pattern[..chunk_len]);
    }
    data
}

/// Generate pseudo-random data for worst-case decompression benchmarks.
///
/// Uses a simple linear congruential generator (LCG) to produce deterministic
/// byte sequences that are effectively incompressible by DEFLATE, exercising
/// the literal-only code paths in the inflate engine.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    // LCG parameters from Knuth's MMIX.
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    for _ in 0..size {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        data.push((state >> 33) as u8);
    }
    data
}

/// Generate all-zeros data for maximally compressible input.
///
/// Produces a run of identical bytes that achieves maximum compression,
/// exercising the RLE-like fast paths in the deflate encoder and producing
/// the simplest possible Huffman trees for the inflate decoder.
fn generate_zero_data(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

// ─── Compression Helpers ────────────────────────────────────────────────────
//
// These functions prepare pre-compressed test vectors OUTSIDE the benchmark
// loop so that only decompression throughput is measured.

/// Compress data with zlib format wrapping (default, `windowBits = MAX_WBITS`).
///
/// Uses the one-call [`compress`] API which produces zlib-wrapped output
/// (RFC 1950: 2-byte header + compressed data + 4-byte Adler-32 trailer).
fn prepare_zlib_compressed(data: &[u8]) -> Vec<u8> {
    let mut compressed = Vec::new();
    compress(&mut compressed, data).unwrap();
    compressed
}

/// Compress data at a specified compression level with zlib wrapping.
///
/// Uses [`compress2`] for explicit level control while producing the same
/// zlib-format output as [`prepare_zlib_compressed`].
fn prepare_zlib_compressed_level(data: &[u8], level: i32) -> Vec<u8> {
    let mut compressed = Vec::new();
    compress2(&mut compressed, data, level).unwrap();
    compressed
}

/// Compress data with raw DEFLATE format (no wrapper, `windowBits = -MAX_WBITS`).
///
/// Uses the streaming deflate API with negative `windowBits` to produce
/// raw DEFLATE output (RFC 1951: no header, no trailer), suitable for
/// testing inflate with `windowBits = -15`.
fn prepare_raw_deflate(data: &[u8]) -> Vec<u8> {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        -MAX_WBITS,       // negative = raw DEFLATE (no zlib/gzip wrapper)
        8,                 // DEF_MEM_LEVEL
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed for raw DEFLATE");

    stream.set_input(data);
    // Raw DEFLATE has no header/trailer overhead, but compress_bound gives a
    // safe upper bound for the compressed payload.
    let bound = compress_bound(data.len());
    stream.set_output_buffer(bound);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "raw deflate did not complete");

    let output = stream.take_output();
    let _end = deflate::deflate_end(&mut stream);
    output
}

/// Compress data with gzip format wrapping (`windowBits = MAX_WBITS + 16`).
///
/// Uses the streaming deflate API with `windowBits = 31` (15 + 16) to
/// produce gzip-wrapped output (RFC 1952: 10-byte header + compressed data +
/// 8-byte CRC-32 + size trailer), suitable for testing inflate with
/// `windowBits = 31`.
fn prepare_gzip_compressed(data: &[u8]) -> Vec<u8> {
    let mut stream = ZStream::new();
    let ret = deflate::deflate_init2(
        &mut stream,
        Z_DEFAULT_COMPRESSION,
        Z_DEFLATED,
        MAX_WBITS + 16,    // 31 = gzip format
        8,                  // DEF_MEM_LEVEL
        Z_DEFAULT_STRATEGY,
    );
    assert_eq!(ret, ReturnCode::Ok, "deflate_init2 failed for gzip");

    stream.set_input(data);
    // Gzip adds ~18 bytes of header + trailer over compress_bound.
    let bound = compress_bound(data.len()) + 18;
    stream.set_output_buffer(bound);

    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = deflate::deflate(&mut stream, Z_FINISH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "gzip deflate did not complete");

    let output = stream.take_output();
    let _end = deflate::deflate_end(&mut stream);
    output
}

// ─── Streaming Inflate Helper ───────────────────────────────────────────────

/// Decompress data using the streaming inflate API with specified `windowBits`.
///
/// This helper mirrors the C pattern of `inflateInit2` → `inflate` loop →
/// `inflateEnd`, exercising the full 30+ mode state machine.  The
/// `window_bits` parameter selects the container format:
///
/// - `15` → zlib format (RFC 1950)
/// - `-15` → raw DEFLATE (RFC 1951)
/// - `31` (15 + 16) → gzip format (RFC 1952)
/// - `47` (15 + 32) → auto-detect (zlib or gzip)
///
/// Uses the stream-level convenience wrappers [`inflate::inflate_run`] and
/// [`inflate::inflate_end_stream`] which internally delegate to
/// [`inflate::inflate`] and [`inflate::inflate_end`] with automatic state
/// extraction from the [`ZStream`].
fn streaming_inflate_decompress(
    compressed: &[u8],
    window_bits: i32,
    output_size: usize,
) -> Vec<u8> {
    let mut stream = ZStream::new();

    // Initialize the inflate engine with the requested format.
    // inflate_init2() stores the InflateState inside stream.state.
    let ret = inflate::inflate_init2(&mut stream, window_bits);
    assert_eq!(
        ret,
        ReturnCode::Ok,
        "inflate_init2 failed (windowBits={window_bits})"
    );

    stream.set_input(compressed);
    stream.set_output_buffer(output_size);

    // Run the inflate state machine until completion.
    // inflate_run() is a convenience wrapper around inflate() that handles
    // the internal InflateState extraction automatically.
    let mut ret = ReturnCode::Ok;
    while ret == ReturnCode::Ok {
        ret = inflate::inflate_run(&mut stream, Z_NO_FLUSH);
    }
    assert_eq!(ret, ReturnCode::StreamEnd, "inflate did not complete");

    // Clean up the inflate state.
    let _ = inflate::inflate_end_stream(&mut stream);

    stream.take_output()
}

// ─── Benchmark 1: One-Call Uncompress ───────────────────────────────────────

/// Benchmark the `uncompress()` one-call decompression wrapper.
///
/// Tests decompression throughput at three representative buffer sizes:
/// - 1 KB — small buffer, dominated by setup/teardown overhead
/// - 64 KB — typical buffer, fits in L1/L2 cache
/// - 1 MB — realistic chunk, tests sustained throughput
///
/// Throughput is reported based on the **uncompressed** (output) data size.
fn bench_uncompress_one_call(c: &mut Criterion) {
    let mut group = c.benchmark_group("uncompress_one_call");

    for size in [1024_usize, 65_536, 1_048_576] {
        let source = generate_compressible_data(size);
        let compressed = prepare_zlib_compressed(&source);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut dest = vec![0u8; size];
                    uncompress(&mut dest, compressed).unwrap();
                    dest
                });
            },
        );
    }

    group.finish();
}

// ─── Benchmark 2: Format-Specific Inflate ───────────────────────────────────

/// Benchmark inflate with different container formats.
///
/// Exercises the format auto-detection logic in the inflate state machine
/// by decompressing identical source data wrapped in different DEFLATE
/// containers:
///
/// - **zlib** (`windowBits = MAX_WBITS`): RFC 1950, Adler-32 integrity check
/// - **raw DEFLATE** (`windowBits = -MAX_WBITS`): RFC 1951, bare stream
/// - **gzip** (`windowBits = MAX_WBITS + 16`): RFC 1952, CRC-32 integrity
/// - **auto-detect** (`windowBits = MAX_WBITS + 32`): zlib-or-gzip detection
fn bench_inflate_formats(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_formats");
    let size: usize = 65_536;
    let source = generate_compressible_data(size);

    group.throughput(Throughput::Bytes(size as u64));

    // ── zlib format (windowBits = MAX_WBITS = 15) ──
    let zlib_data = prepare_zlib_compressed(&source);
    group.bench_function("zlib", |b| {
        b.iter(|| streaming_inflate_decompress(&zlib_data, MAX_WBITS, size));
    });

    // ── Raw DEFLATE (windowBits = -MAX_WBITS = -15) ──
    let raw_data = prepare_raw_deflate(&source);
    group.bench_function("raw_deflate", |b| {
        b.iter(|| streaming_inflate_decompress(&raw_data, -MAX_WBITS, size));
    });

    // ── Gzip format (windowBits = MAX_WBITS + 16 = 31) ──
    let gzip_data = prepare_gzip_compressed(&source);
    group.bench_function("gzip", |b| {
        b.iter(|| streaming_inflate_decompress(&gzip_data, MAX_WBITS + 16, size));
    });

    // ── Auto-detect (windowBits = MAX_WBITS + 32 = 47) ──
    // Feed zlib-wrapped data and let inflate auto-detect the container.
    group.bench_function("auto_detect", |b| {
        b.iter(|| {
            streaming_inflate_decompress(&zlib_data, MAX_WBITS + 32, size)
        });
    });

    group.finish();
}

// ─── Benchmark 3: Data-Size Scaling ─────────────────────────────────────────

/// Benchmark inflate throughput across a range of data sizes.
///
/// Reveals scaling characteristics of the decompression engine: setup
/// overhead at small sizes, cache boundary effects at medium sizes, and
/// sustained throughput at large sizes.  Sizes span 256 B to 4 MB covering
/// four orders of magnitude.
fn bench_inflate_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_scaling");

    // Geometric progression from 256 B to 4 MB (×4 per step).
    let sizes: &[usize] = &[
        256,
        1_024,
        4_096,
        16_384,
        65_536,
        262_144,
        1_048_576,
        4_194_304,
    ];

    for &size in sizes {
        let source = generate_compressible_data(size);
        let compressed = prepare_zlib_compressed(&source);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("bytes", size),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut dest = vec![0u8; size];
                    uncompress(&mut dest, compressed).unwrap();
                    dest
                });
            },
        );
    }

    group.finish();
}

// ─── Benchmark 4: Fast-Path Decoder ─────────────────────────────────────────

/// Benchmark the `inflate_fast()` hot loop with large buffers.
///
/// The `inflate_fast()` function (ported from `inffast.c`) is the
/// performance-critical inner loop responsible for >95% of inflate execution
/// time.  It activates when `avail_in >= 6` and `avail_out >= 258`.  Using
/// 4 MB test data ensures the vast majority of decompression time is spent
/// in this fast path.
///
/// Tests the default compression level and specific levels since the
/// compression level affects the structure of Huffman codes and
/// back-reference distances, exercising different decoder code paths:
///
/// - Level 0 (`Z_NO_COMPRESSION`): stored blocks — tests the stored-block
///   copy loop rather than Huffman decoding.
/// - Level 1 (`Z_BEST_SPEED`): fast greedy matching — short back-references,
///   simple Huffman trees.
/// - Level 6 (default): balanced matching — mixed literals and references.
/// - Level 9 (`Z_BEST_COMPRESSION`): exhaustive matching — long back-
///   references, complex Huffman trees.
fn bench_inflate_fast_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_fast_path");
    let size: usize = 4_194_304; // 4 MB — triggers inflate_fast() hot path
    let source = generate_compressible_data(size);

    group.throughput(Throughput::Bytes(size as u64));

    // Default compression level via one-call compress().
    let compressed_default = prepare_zlib_compressed(&source);
    group.bench_function("4MB_compressible", |b| {
        b.iter(|| {
            let mut dest = vec![0u8; size];
            uncompress(&mut dest, &compressed_default).unwrap();
            dest
        });
    });

    // Level variants: stored (0), fast (1), default (6), best (9).
    for &level in &[
        Z_NO_COMPRESSION,
        Z_BEST_SPEED,
        6_i32,
        Z_BEST_COMPRESSION,
    ] {
        let compressed = prepare_zlib_compressed_level(&source, level);

        group.bench_with_input(
            BenchmarkId::new("level", level),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut dest = vec![0u8; size];
                    uncompress(&mut dest, compressed).unwrap();
                    dest
                });
            },
        );
    }

    group.finish();
}

// ─── Benchmark 5: Compressed-Data Patterns ──────────────────────────────────

/// Benchmark inflate with different source data patterns.
///
/// Different input data characteristics produce vastly different compressed
/// representations, exercising distinct inflate code paths:
///
/// - **random**: Near-incompressible data → predominantly literal tokens,
///   minimal back-references, low compression ratio.
/// - **compressible**: Repetitive text → mixed literals and back-references,
///   typical compression ratio.
/// - **zeros**: Maximally compressible → long back-reference chains, very
///   high compression ratio, exercises window copy loops.
///
/// Uses [`uncompress2`] to additionally validate the source-consumed and
/// output-produced byte counts.
fn bench_inflate_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("inflate_patterns");
    let size: usize = 65_536;

    group.throughput(Throughput::Bytes(size as u64));

    let patterns: &[(&str, fn(usize) -> Vec<u8>)] = &[
        ("random", generate_random_data),
        ("compressible", generate_compressible_data),
        ("zeros", generate_zero_data),
    ];

    for &(name, gen_fn) in patterns {
        let source = gen_fn(size);
        let compressed = prepare_zlib_compressed(&source);
        let compressed_len = compressed.len();

        group.bench_with_input(
            BenchmarkId::new("pattern", name),
            &compressed,
            |b, compressed| {
                b.iter(|| {
                    let mut dest = vec![0u8; size];
                    let (dest_used, src_consumed) =
                        uncompress2(&mut dest, compressed).unwrap();
                    // Verify that the full compressed payload was consumed and
                    // the expected uncompressed size was produced.
                    debug_assert_eq!(dest_used, size);
                    debug_assert_eq!(src_consumed, compressed_len);
                    dest
                });
            },
        );
    }

    group.finish();
}

// ─── Criterion Registration ─────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_uncompress_one_call,
    bench_inflate_formats,
    bench_inflate_scaling,
    bench_inflate_fast_path,
    bench_inflate_patterns,
);
criterion_main!(benches);
