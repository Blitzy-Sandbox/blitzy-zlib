//! Criterion throughput benchmark for the deflate (compression) engine.
//!
//! Measures whole-buffer `compress2` throughput across the fastest, default,
//! and best compression levels. Uses the public `zlib_rs` one-call API only, so
//! it exercises the same surface a downstream Rust consumer would.
//!
//! Run with: `cargo bench --bench deflate_bench`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use zlib_rs::{Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, compress_bound, compress2};

/// Number of input bytes fed to each compression run.
const INPUT_LEN: usize = 64 * 1024;

/// Builds `len` bytes of semi-compressible test data: a repeating English-text
/// pattern lightly perturbed by a cheap LCG so the deflate engine finds real
/// back-references without the input being trivially compressible.
fn test_data(len: usize) -> Vec<u8> {
    const PATTERN: &[u8] = b"the quick brown fox jumps over the lazy dog. ";
    let mut out = Vec::with_capacity(len);
    let mut state: u32 = 0x1234_5678;
    while out.len() < len {
        for &byte in PATTERN {
            if out.len() >= len {
                break;
            }
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // ~1-in-16 bytes is perturbed; the rest reproduce the pattern.
            let noise = if (state >> 28) == 0 {
                (state & 0xff) as u8
            } else {
                0
            };
            out.push(byte ^ noise);
        }
    }
    out
}

/// Benchmarks `compress2` at three representative levels.
fn bench_deflate(c: &mut Criterion) {
    let input = test_data(INPUT_LEN);
    let cap = compress_bound(input.len());

    let mut group = c.benchmark_group("deflate");
    group.throughput(Throughput::Bytes(input.len() as u64));
    for level in [Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, Z_BEST_COMPRESSION] {
        group.bench_with_input(BenchmarkId::from_parameter(level), &level, |b, &level| {
            let mut dest = vec![0u8; cap];
            b.iter(|| {
                let written = compress2(black_box(&mut dest), black_box(&input), level)
                    .expect("compress2 should succeed with a compress_bound-sized buffer");
                black_box(written)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_deflate);
criterion_main!(benches);
