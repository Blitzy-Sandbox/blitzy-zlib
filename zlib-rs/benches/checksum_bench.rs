//! Criterion micro-benchmarks for the `zlib-rs` integrity checksums.
//!
//! This benchmark measures the **throughput** (bytes per second) of the two
//! checksums that back the zlib and gzip wire formats:
//!
//! * **Adler-32** (RFC 1950 / zlib container) — [`zlib_rs::adler32`].
//! * **CRC-32 (IEEE 802.3)** (RFC 1952 / gzip container) — [`zlib_rs::crc32`].
//!
//! Each checksum is exercised over a small (1 KiB), medium (64 KiB), and large
//! (1 MiB) buffer of deterministic pseudo-random bytes, so Criterion can report
//! steady-state throughput across the cache hierarchy rather than fixed
//! per-call overhead.
//!
//! # Harness wiring
//!
//! This file is registered in `zlib-rs/Cargo.toml` as
//! `[[bench]] name = "checksum_bench"` with **`harness = false`**, which means
//! Criterion supplies the program entry point. The file therefore ends with
//! `criterion_group!` + `criterion_main!` and intentionally defines **no**
//! `fn main`, no `#[bench]`, and no libtest `#[test]`.
//!
//! # Running
//!
//! ```text
//! cargo bench -p zlib-rs --bench checksum_bench
//! ```
//!
//! # Demonstrating the SIMD CRC-32 speed-up (AAP §0.7.2)
//!
//! Exactly one CRC-32 implementation is compiled per binary, selected by the
//! crate's `simd` Cargo feature: with the default features the public
//! [`zlib_rs::crc32`] uses the hardware-accelerated `crc32fast` path; with
//! `--no-default-features` it falls back to the scalar byte-wise table. This
//! bench only ever calls the public `zlib_rs::crc32` (it cannot call `crc32fast`
//! directly — that crate is not a dependency here), so the ≥3× SIMD target from
//! AAP §0.7.2 is demonstrated by running the *same* bench both ways and
//! comparing Criterion baselines:
//!
//! ```text
//! # 1. Record the SIMD (default-feature) baseline under the name "simd":
//! cargo bench -p zlib-rs --bench checksum_bench -- --save-baseline simd
//!
//! # 2. Re-run with SIMD disabled (scalar fallback) and compare against it:
//! cargo bench -p zlib-rs --bench checksum_bench --no-default-features -- --baseline simd
//! ```
//!
//! The second run reports how much slower the scalar path is relative to the
//! saved `simd` baseline — i.e. the SIMD speed-up factor.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use zlib_rs::{adler32, crc32};

/// Fixed RNG seed for reproducible benchmark corpora.
///
/// Using a constant seed means every run — and every machine — checksums the
/// *same* pseudo-random bytes, so results are comparable across builds and
/// across the SIMD-vs-scalar baselines documented at the module level. The
/// specific value is an arbitrary fixed nonce (the SplitMix64 / golden-ratio
/// odd constant).
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Buffer sizes exercised by every checksum benchmark: 1 KiB, 64 KiB, 1 MiB.
///
/// The spread spans an L1-resident small buffer, a mid-size buffer, and a large
/// buffer that exceeds typical L2, so the reported throughput reflects the
/// steady-state per-byte cost rather than fixed per-call overhead.
const SIZES: [usize; 3] = [1 << 10, 64 << 10, 1 << 20];

/// Build a deterministic buffer of `len` pseudo-random bytes.
///
/// A fresh [`StdRng`] is seeded from the fixed [`SEED`] on every call, so the
/// contents depend only on `len` (identical bytes for identical sizes on every
/// invocation). Checksum throughput is independent of the actual byte values,
/// but using varied data prevents the optimizer from exploiting a constant
/// input.
fn random_bytes(len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut buf = vec![0u8; len];
    rng.fill(&mut buf[..]);
    buf
}

/// Benchmark Adler-32 throughput across [`SIZES`].
///
/// A fresh checksum uses the RFC 1950 initial value `1`. The input slice is
/// wrapped in [`black_box`] so the compiler cannot hoist or elide the work, and
/// [`Throughput::Bytes`] is set per buffer so Criterion reports bytes/sec.
fn bench_adler32(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32");
    for &len in &SIZES {
        let data = random_bytes(len);
        group.throughput(Throughput::Bytes(len as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &data, |b, data| {
            b.iter(|| adler32(1, black_box(&data[..])));
        });
    }
    group.finish();
}

/// Benchmark CRC-32 (IEEE) throughput across [`SIZES`].
///
/// A fresh CRC uses the initial value `0`. Before timing, a non-timed
/// [`debug_assert_eq!`] guards the canonical CRC-32/IEEE known-answer value, so
/// a miscompiled or mis-seeded checksum is caught immediately in debug builds
/// (the guard is compiled out of the release benches). The active
/// implementation — SIMD `crc32fast` vs the scalar table — is chosen by the
/// crate's `simd` feature; see the module-level docs for the comparison recipe.
fn bench_crc32(c: &mut Criterion) {
    // Known-answer sanity guard: the canonical CRC-32/ISO-HDLC check value over
    // the ASCII string "123456789". Not timed, and removed in release builds.
    debug_assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);

    let mut group = c.benchmark_group("crc32");
    for &len in &SIZES {
        let data = random_bytes(len);
        group.throughput(Throughput::Bytes(len as u64));
        group.bench_with_input(BenchmarkId::from_parameter(len), &data, |b, data| {
            b.iter(|| crc32(0, black_box(&data[..])));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_adler32, bench_crc32);
criterion_main!(benches);
