//! DEFLATE decompression throughput benchmark.
//!
//! Pre-compresses a fixed corpus once, then measures one-shot
//! [`zlib_rs::uncompress`] over the resulting stream. Declared with
//! `harness = false` in `Cargo.toml` so the criterion harness drives it.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// Build a deterministic, moderately compressible test corpus of `len` bytes.
///
/// See `deflate_bench.rs` for the rationale; the same generator is used so the
/// compress and decompress benchmarks operate on identical data.
fn make_corpus(len: usize) -> Vec<u8> {
    const PHRASE: &[u8] = b"the quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(len);
    let mut state: u32 = 0x1234_5678;
    while data.len() < len {
        for &b in PHRASE {
            if data.len() >= len {
                break;
            }
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let byte = if state & 0x7 == 0 {
                (state & 0xff) as u8
            } else {
                b
            };
            data.push(byte);
        }
    }
    data
}

/// Benchmark one-shot decompression at a 64 KiB output size.
fn bench_uncompress(c: &mut Criterion) {
    let data = make_corpus(64 * 1024);

    // Compress once, outside the measured loop: the benchmark targets the
    // decompressor only.
    let mut compressed = vec![0u8; zlib_rs::compress_bound(data.len())];
    let clen = zlib_rs::compress(&mut compressed, &data)
        .expect("compress should succeed with a compress_bound-sized buffer");
    compressed.truncate(clen);

    // The uncompressed length is known, so the destination is sized exactly.
    let mut out = vec![0u8; data.len()];

    let mut group = c.benchmark_group("inflate");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("uncompress_64k", |b| {
        b.iter(|| {
            let written = zlib_rs::uncompress(black_box(&mut out), black_box(&compressed))
                .expect("uncompress should succeed for a valid zlib stream");
            black_box(written)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_uncompress);
criterion_main!(benches);
