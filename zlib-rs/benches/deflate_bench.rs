// Suppress crate-level lints that are not applicable to benchmark binaries.
// The `missing_docs` lint is denied in `Cargo.toml` for the library, but
// Criterion's `criterion_group!` and `criterion_main!` macros expand to items
// that cannot carry doc comments. Clippy pedantic lints are also relaxed for
// benchmark-specific patterns (e.g., `cast_possible_truncation` in LCG
// generators, `similar_names` in benchmark parameter handling).
#![allow(missing_docs)]
#![allow(clippy::cast_possible_truncation)]

//! DEFLATE compression benchmarks for the zlib-rs library.
//!
//! This benchmark suite measures compression throughput across:
//!
//! - All 10 compression levels (0–9) plus the default level (`Z_DEFAULT_COMPRESSION`)
//! - All 5 compression strategies (default, filtered, huffman-only, RLE, fixed)
//! - Multiple data patterns (random, compressible, text-like, zeros)
//! - Multiple data sizes (1 KB, 64 KB, 1 MB)
//! - Window sizes from 9 to 15 bits
//!
//! Results report throughput in bytes/second via [`Throughput::Bytes`],
//! enabling validation of the performance requirement that compression
//! throughput is within 90% of C zlib (AAP Section 0.7.2).
//!
//! # Configuration
//!
//! This benchmark binary uses `criterion = "0.5"` with `harness = false` set
//! in `zlib-rs/Cargo.toml`. Criterion provides its own `main` function via
//! the [`criterion_main!`] macro.
//!
//! # Running
//!
//! ```bash
//! cargo bench -p zlib-rs --bench deflate_bench
//! ```

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use zlib_rs::constants::{
    Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED,
    Z_FILTERED, Z_FINISH, Z_FIXED, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_RLE,
};
use zlib_rs::deflate;
use zlib_rs::error::ReturnCode;
use zlib_rs::stream::ZStream;
use zlib_rs::{compress, compress_bound, compress2};

// ─── Data Generation Helpers ────────────────────────────────────────────────
//
// These functions produce deterministic test data for reproducible benchmarks.
// Each function generates a specific data pattern that exercises different
// compression code paths in the DEFLATE engine.

/// Generates pseudo-random bytes using a linear congruential generator.
///
/// Produces deterministic, hard-to-compress data that exercises the worst-case
/// code path in the DEFLATE algorithm where few string matches are found.
/// The LCG parameters (a = 6364136223846793005, c = 1442695040888963407) are
/// from Knuth's MMIX and produce a full-period sequence over 64-bit state.
///
/// The fixed seed ensures benchmark reproducibility across runs.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    // Fixed seed for reproducibility.
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    for _ in 0..size {
        // Knuth MMIX LCG: state = a * state + c  (mod 2^64)
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // Use upper bits for better statistical distribution.
        data.push((state >> 33) as u8);
    }
    data
}

/// Generates highly compressible data with a short repeated pattern.
///
/// Cycles through the ASCII characters `'A'` through `'J'` (10-byte period).
/// This creates data with moderate entropy and short match distances,
/// exercising the typical compression performance of the DEFLATE engine.
fn generate_compressible_data(size: usize) -> Vec<u8> {
    let pattern = b"ABCDEFGHIJ";
    (0..size).map(|i| pattern[i % pattern.len()]).collect()
}

/// Generates text-like data simulating English character distribution.
///
/// Uses a deterministic LCG to produce bytes drawn from an alphabet weighted
/// toward lowercase letters and common punctuation, approximating the byte
/// distribution of English text.  This exercises the most common real-world
/// compression use case.
fn generate_text_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
    // Weighted alphabet: common English letters and space appear multiple times.
    let alphabet = b"etaoinshrdlu cmfgypwbvkjxqz ETAOINSHRDLU.,'\"!? ";
    for _ in 0..size {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let idx = ((state >> 33) as usize) % alphabet.len();
        data.push(alphabet[idx]);
    }
    data
}

/// Generates zero-filled data (maximally compressible).
///
/// All-zero input represents the best-case compression scenario, testing
/// the deflate engine's handling of maximal runs of identical bytes.
/// At level 0, this also benchmarks stored-block framing overhead.
fn generate_zero_data(size: usize) -> Vec<u8> {
    vec![0u8; size]
}

// ─── Streaming Compression Helper ───────────────────────────────────────────

/// Compresses data using the streaming deflate API with full parameter control.
///
/// Wraps the `deflate_init2` → `deflate` → `deflate_end` sequence required
/// by the strategy and window-size benchmarks. Returns the compressed output.
///
/// # Parameters
///
/// * `data` — Uncompressed input bytes.
/// * `level` — Compression level (0–9 or `Z_DEFAULT_COMPRESSION`).
/// * `window_bits` — Window size in bits (9–15 for zlib wrapping).
/// * `mem_level` — Memory allocation level (1–9).
/// * `strategy` — Compression strategy constant.
///
/// # Panics
///
/// Panics if any deflate operation returns an unexpected error code.
/// This is acceptable in benchmark/test code (AAP Section 0.7.2).
fn streaming_compress(
    data: &[u8],
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Vec<u8> {
    let mut stream = ZStream::new();

    // Initialize the deflate engine with the requested parameters.
    // `Z_DEFLATED` is the only supported compression method.
    let ret = deflate::deflate_init2(
        &mut stream,
        level,
        Z_DEFLATED,
        window_bits,
        mem_level,
        strategy,
    );
    assert!(ret == ReturnCode::Ok, "deflate_init2 failed with {ret:?}");

    // Provide all input at once and allocate sufficient output space.
    stream.set_input(data);
    stream.set_output_buffer(compress_bound(data.len()));

    // Compress everything in a single pass with `Z_FINISH`.
    let ret = deflate::deflate(&mut stream, Z_FINISH);
    assert!(
        ret == ReturnCode::StreamEnd,
        "deflate did not complete (returned {ret:?})"
    );

    // Capture the compressed output before releasing engine state.
    let output = stream.take_output();

    // Release all internal deflate state.
    let ret = deflate::deflate_end(&mut stream);
    assert!(ret == ReturnCode::Ok, "deflate_end failed with {ret:?}");

    output
}

// ─── Benchmark Group 1: One-Call Compress ───────────────────────────────────

/// Benchmarks the one-call [`compress()`] wrapper at multiple data sizes.
///
/// Tests the default compression level (`Z_DEFAULT_COMPRESSION`, mapped to
/// level 6 internally) across three input sizes:
///
/// - **1 KB** (1024 bytes) — small payloads and setup overhead
/// - **64 KB** (65 536 bytes) — fits in L1/L2 cache, typical chunk size
/// - **1 MB** (1 048 576 bytes) — sustained throughput measurement
///
/// Throughput is reported via [`Throughput::Bytes`] for MB/s display.
///
/// Derived from `compress.c` lines 77–84 (the C `compress()` function).
fn bench_compress_one_call(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress_one_call");

    let sizes: &[usize] = &[1024, 65_536, 1_048_576];

    for &size in sizes {
        let data = generate_compressible_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("default", size), &data, |b, data| {
            b.iter(|| {
                let mut dest = Vec::new();
                compress(&mut dest, data).unwrap();
                dest
            });
        });
    }

    group.finish();
}

// ─── Benchmark Group 2: Compression Levels ──────────────────────────────────

/// Benchmarks all 10 compression levels (0–9) plus `Z_DEFAULT_COMPRESSION`.
///
/// Level-to-strategy mapping from `deflate.c` `configuration_table`
/// (lines 112–124):
///
/// | Level | Strategy Function | Description |
/// |-------|-------------------|-------------|
/// | 0 | `deflate_stored` | No compression, just framing |
/// | 1–3 | `deflate_fast` | Greedy matching, no lazy evaluation |
/// | 4–9 | `deflate_slow` | Lazy matching with increasing chain depth |
///
/// `Z_DEFAULT_COMPRESSION` (-1) is resolved to level 6 internally.
///
/// The named level constants from `zlib.h` (lines 194–197) are used
/// to label the level-0, level-1, level-9, and default benchmarks.
fn bench_compression_levels(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_levels");
    let data = generate_compressible_data(65_536);
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Use the public named constants where applicable to ensure they are
    // exercised by the benchmark suite:
    // Z_NO_COMPRESSION = 0, Z_BEST_SPEED = 1, Z_BEST_COMPRESSION = 9
    let named_levels: &[(i32, &str)] = &[
        (Z_NO_COMPRESSION, "0_no_compression"),
        (Z_BEST_SPEED, "1_best_speed"),
        (2, "2"),
        (3, "3"),
        (4, "4"),
        (5, "5"),
        (6, "6_default_equivalent"),
        (7, "7"),
        (8, "8"),
        (Z_BEST_COMPRESSION, "9_best_compression"),
        (Z_DEFAULT_COMPRESSION, "default_neg1"),
    ];

    for &(level, name) in named_levels {
        group.bench_with_input(BenchmarkId::new("level", name), &level, |b, &level| {
            b.iter(|| {
                let mut dest = Vec::new();
                compress2(&mut dest, &data, level).unwrap();
                dest
            });
        });
    }

    group.finish();
}

// ─── Benchmark Group 3: Compression Strategies ──────────────────────────────

/// Benchmarks all 5 DEFLATE compression strategies via the streaming API.
///
/// Strategies from `zlib.h` lines 200–204, described in the `deflateInit2`
/// documentation (lines 593–606):
///
/// | Constant | Value | Description |
/// |----------|-------|-------------|
/// | `Z_DEFAULT_STRATEGY` | 0 | General-purpose balance |
/// | `Z_FILTERED` | 1 | More Huffman, less string matching |
/// | `Z_HUFFMAN_ONLY` | 2 | No string matching at all |
/// | `Z_RLE` | 3 | Run-length encoding (distance ≤ 1) |
/// | `Z_FIXED` | 4 | Fixed (pre-computed) Huffman tables |
///
/// Uses [`deflate_init2()`](deflate::deflate_init2) with level 6,
/// `windowBits = 15`, and `memLevel = 8` to isolate strategy effects.
fn bench_compression_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_strategies");
    let data = generate_compressible_data(65_536);
    group.throughput(Throughput::Bytes(data.len() as u64));

    let strategies: &[(&str, i32)] = &[
        ("default", Z_DEFAULT_STRATEGY),
        ("filtered", Z_FILTERED),
        ("huffman_only", Z_HUFFMAN_ONLY),
        ("rle", Z_RLE),
        ("fixed", Z_FIXED),
    ];

    for &(name, strategy) in strategies {
        group.bench_function(BenchmarkId::new("strategy", name), |b| {
            b.iter(|| streaming_compress(&data, 6, 15, 8, strategy));
        });
    }

    group.finish();
}

// ─── Benchmark Group 4: Data Patterns ───────────────────────────────────────

/// Benchmarks compression across four distinct input data patterns.
///
/// All patterns are 64 KB (65 536 bytes), which fits in L1/L2 cache for
/// consistent measurement. The patterns exercise different compression
/// engine code paths:
///
/// - **random** — pseudo-random bytes; worst-case for string matching
/// - **compressible** — repeated 10-byte ASCII pattern; typical performance
/// - **text** — simulated English text; the common real-world use case
/// - **zeros** — all zeros; best-case for maximum compression ratio
fn bench_data_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_patterns");
    let size: usize = 65_536;
    group.throughput(Throughput::Bytes(size as u64));

    let patterns: Vec<(&str, Vec<u8>)> = vec![
        ("random", generate_random_data(size)),
        ("compressible", generate_compressible_data(size)),
        ("text", generate_text_data(size)),
        ("zeros", generate_zero_data(size)),
    ];

    for (name, data) in &patterns {
        group.bench_with_input(BenchmarkId::new("pattern", *name), data, |b, data| {
            b.iter(|| {
                let mut dest = Vec::new();
                compress(&mut dest, data).unwrap();
                dest
            });
        });
    }

    group.finish();
}

// ─── Benchmark Group 5: Window Sizes ────────────────────────────────────────

/// Benchmarks sliding window sizes from 9 to 15 bits.
///
/// The `windowBits` parameter controls the LZ77 sliding window size:
/// `window_size = 2^windowBits` bytes.
///
/// | `windowBits` | Window Size | Approx. Deflate Memory |
/// |-------------|-------------|------------------------|
/// | 9 | 512 B | ~10 KB |
/// | 10 | 1 KB | ~18 KB |
/// | 11 | 2 KB | ~34 KB |
/// | 12 | 4 KB | ~66 KB |
/// | 13 | 8 KB | ~130 KB |
/// | 14 | 16 KB | ~194 KB |
/// | 15 (default) | 32 KB | ~256 KB |
///
/// Larger windows find longer matches but consume more memory.  Positive
/// `windowBits` values select zlib format wrapping (RFC 1950).
///
/// Uses [`deflate_init2()`](deflate::deflate_init2) with level 6,
/// `memLevel = 8`, and `strategy = Z_DEFAULT_STRATEGY` to isolate the
/// effect of window size on throughput.
fn bench_window_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("deflate_window");
    let data = generate_compressible_data(65_536);
    group.throughput(Throughput::Bytes(data.len() as u64));

    for window_bits in 9..=15_i32 {
        group.bench_with_input(
            BenchmarkId::new("windowBits", window_bits),
            &window_bits,
            |b, &wb| {
                b.iter(|| streaming_compress(&data, 6, wb, 8, Z_DEFAULT_STRATEGY));
            },
        );
    }

    group.finish();
}

// ─── Criterion Registration ─────────────────────────────────────────────────
//
// All five benchmark groups are registered with Criterion and a main function
// is generated via the `criterion_main!` macro. No manual `fn main()` is
// defined — Criterion provides it.

criterion_group!(
    benches,
    bench_compress_one_call,
    bench_compression_levels,
    bench_compression_strategies,
    bench_data_patterns,
    bench_window_sizes,
);

criterion_main!(benches);
