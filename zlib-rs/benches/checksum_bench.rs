//! Checksum throughput benchmark.
//!
//! Measures the two zlib checksums — [`zlib_rs::crc32`] (IEEE CRC-32, seeded
//! with `0`) and [`zlib_rs::adler32`] (seeded with the Adler-32 initial value
//! `1`) — over a fixed buffer. Declared with `harness = false` in `Cargo.toml`
//! so the criterion harness drives it.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// Build a deterministic byte buffer of `len` bytes.
///
/// Checksum throughput is independent of the data's compressibility, so a cheap
/// xorshift fill is sufficient and keeps the buffer identical across runs.
fn make_buffer(len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    let mut state: u32 = 0x9e37_79b9;
    while data.len() < len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        data.push((state & 0xff) as u8);
    }
    data
}

/// Benchmark CRC-32 and Adler-32 over a 64 KiB buffer.
fn bench_checksums(c: &mut Criterion) {
    let data = make_buffer(64 * 1024);

    let mut group = c.benchmark_group("checksum");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // IEEE CRC-32 — a fresh computation starts from a `0` running value.
    group.bench_function("crc32", |b| {
        b.iter(|| black_box(zlib_rs::crc32(black_box(0), black_box(&data))));
    });

    // Adler-32 — a fresh computation starts from the initial value `1`.
    group.bench_function("adler32", |b| {
        b.iter(|| black_box(zlib_rs::adler32(black_box(1), black_box(&data))));
    });

    group.finish();
}

criterion_group!(benches, bench_checksums);
criterion_main!(benches);
