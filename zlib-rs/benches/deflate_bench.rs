//! DEFLATE compression throughput benchmark.
//!
//! Measures one-shot [`zlib_rs::compress`] over a fixed, moderately
//! compressible corpus. Declared with `harness = false` in `Cargo.toml` so the
//! criterion harness (not libtest) drives it.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// Build a deterministic, moderately compressible test corpus of `len` bytes.
///
/// A short English-like phrase supplies redundancy for the LZ77 stage while a
/// cheap xorshift PRNG perturbs roughly one byte in eight to keep the entropy
/// (and therefore the Huffman stage) realistic. The output is identical on
/// every run, so benchmark numbers are comparable across invocations.
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
            // Mostly the phrase byte; occasionally a pseudo-random byte.
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

/// Benchmark default-level one-shot compression at a 64 KiB input size.
fn bench_compress(c: &mut Criterion) {
    let data = make_corpus(64 * 1024);
    // Sized once via the exact zlib bound so the destination never reallocates
    // and never overflows mid-measurement.
    let mut dest = vec![0u8; zlib_rs::compress_bound(data.len())];

    let mut group = c.benchmark_group("deflate");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("compress_64k", |b| {
        b.iter(|| {
            let written = zlib_rs::compress(black_box(&mut dest), black_box(&data))
                .expect("compress should succeed with a compress_bound-sized buffer");
            black_box(written)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_compress);
criterion_main!(benches);
