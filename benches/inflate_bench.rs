//! Criterion throughput benchmark for the inflate (decompression) engine.
//!
//! Pre-compresses a fixed buffer once, then measures whole-buffer `uncompress`
//! throughput on the decode path. Uses the public `zlib_rs` one-call API only.
//!
//! Run with: `cargo bench --bench inflate_bench`.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use zlib_rs::{Z_DEFAULT_COMPRESSION, compress_bound, compress2, uncompress};

/// Number of uncompressed input bytes used to seed the decode benchmark.
const INPUT_LEN: usize = 64 * 1024;

/// Builds `len` bytes of semi-compressible test data: a repeating English-text
/// pattern lightly perturbed by a cheap LCG so the compressed stream fed to the
/// decoder has realistic structure (both literals and back-references).
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

/// Benchmarks `uncompress` on a pre-built compressed buffer.
fn bench_inflate(c: &mut Criterion) {
    let input = test_data(INPUT_LEN);

    // Compress once up front; only the decode path is timed below.
    let mut compressed = vec![0u8; compress_bound(input.len())];
    let written = compress2(&mut compressed, &input, Z_DEFAULT_COMPRESSION)
        .expect("compress2 should succeed with a compress_bound-sized buffer");
    compressed.truncate(written);

    let mut group = c.benchmark_group("inflate");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.bench_function("uncompress_64k", |b| {
        let mut dest = vec![0u8; input.len()];
        b.iter(|| {
            let produced = uncompress(black_box(&mut dest), black_box(&compressed))
                .expect("uncompress should succeed with an exactly-sized buffer");
            black_box(produced)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_inflate);
criterion_main!(benches);
