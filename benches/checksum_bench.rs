//! Throughput benchmarks for the `zlib-rs` checksum engines.
//!
//! This is the foundational criterion harness in `benches/`. It measures the
//! bytes-per-second throughput of the crate's public, safe-Rust checksum API —
//! [`zlib_rs::crc32`] and [`zlib_rs::adler32`] — and exists primarily to
//! demonstrate the AAP §0.7.3 performance gate:
//!
//! > **CRC-32 (SIMD path) ≥ 3× the scalar implementation.**
//!
//! # Why this file ships its own scalar CRC-32
//!
//! Under the crate's default feature set the `simd` feature is **on**, so the
//! public [`zlib_rs::crc32`] dispatches to the hardware-accelerated `crc32fast`
//! backend. The crate's *internal* scalar and SIMD routines (`crc32_scalar` /
//! `crc32_simd`) are module-private and therefore unreachable from this
//! benchmark, which compiles as a separate crate that can only observe `pub`
//! items. To obtain an honest "SIMD ÷ scalar" ratio this file implements its
//! own table-driven scalar CRC-32 baseline ([`crc32_scalar_local`]) using the
//! reflected IEEE polynomial `0xEDB8_8320` — the same polynomial zlib's
//! `crc32.c` uses (`POLY 0xedb88320`). A startup assertion proves the local
//! scalar baseline and the public SIMD path compute *identical* CRC values, so
//! the throughput comparison is apples-to-apples.
//!
//! CI and maintainers read the two CRC-32 throughput numbers emitted by
//! criterion (`crc32/simd_public/...` and `crc32/scalar_local/...`) and verify
//! that `simd_public` is at least 3× the bytes/s of `scalar_local`. On a noisy
//! shared CI VM the absolute numbers vary, but the ratio is the gate.
//!
//! The harness contains no `unsafe`, no FFI, and no emoji; all correctness
//! assertions live in untimed setup so the timed closures measure only the
//! checksum computation.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::hint::black_box;
use zlib_rs::{adler32, crc32};

// ---------------------------------------------------------------------------
// Local scalar CRC-32 baseline (the ≥3× denominator)
// ---------------------------------------------------------------------------

/// Build the canonical 256-entry single-byte CRC-32 lookup table for the
/// reflected IEEE polynomial `0xEDB8_8320`, at compile time.
///
/// This mirrors the table that the crate's `build.rs` generates for the scalar
/// path; computing it here in a `const fn` keeps the baseline self-contained
/// and pins it to the same polynomial as upstream zlib (`crc32.c`).
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

/// The compile-time-computed scalar CRC-32 table. Held as a `static` so it is a
/// single shared instance rather than re-materialized at every use site.
static SCALAR_CRC_TABLE: [u32; 256] = build_crc_table();

/// Update a running CRC-32 over `buf` using a slice-by-1 table lookup.
///
/// Applies zlib's pre/post one's-complement conditioning (`crc ^ 0xFFFF_FFFF`
/// on entry and exit), so seeding with `0` yields the standard IEEE CRC-32 and
/// the result is identical to [`zlib_rs::crc32`] for the same input. This is
/// the explicit scalar baseline against which the SIMD path is measured.
fn crc32_scalar_local(crc: u32, buf: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in buf {
        c = (c >> 8) ^ SCALAR_CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize];
    }
    !c
}

// ---------------------------------------------------------------------------
// Deterministic corpora
// ---------------------------------------------------------------------------

/// Fixed RNG seed ("ZLIB" in ASCII) so every run produces identical corpora and
/// the throughput numbers are reproducible and comparable across runs.
const SEED: u64 = 0x5A4C_4942;

/// Buffer sizes exercised by every group: 64 KiB and 1 MiB. The small size
/// keeps the working set in cache; the large size amortizes per-call overhead
/// and better reflects steady-state throughput.
const SIZES: [usize; 2] = [64 * 1024, 1024 * 1024];

/// Incompressible random bytes — the representative worst case for checksum
/// table locality and a realistic stand-in for already-compressed payloads.
fn incompressible(size: usize) -> Vec<u8> {
    let mut v = vec![0u8; size];
    let mut rng = StdRng::seed_from_u64(SEED);
    rng.fill_bytes(&mut v);
    v
}

/// Text-like bytes: deterministic random data folded into a small printable
/// ASCII alphabet. Uses only the stable `rand` 0.9 APIs (`seed_from_u64` +
/// `fill_bytes`); it deliberately avoids `gen`/`gen_range`/`random_range`.
fn text_like(size: usize) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz 0123456789";
    let mut v = vec![0u8; size];
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x00FF_00FF);
    rng.fill_bytes(&mut v);
    for byte in &mut v {
        *byte = ALPHABET[usize::from(*byte) % ALPHABET.len()];
    }
    v
}

/// The named corpora benchmarked by every group, built once per size. Each
/// entry is a `(label, bytes)` pair; the label becomes part of the criterion
/// `BenchmarkId` parameter so each throughput number records its input shape.
fn corpora(size: usize) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("incompressible", incompressible(size)),
        ("text_like", text_like(size)),
    ]
}

// ---------------------------------------------------------------------------
// Startup correctness assertions (apples-to-apples proof)
// ---------------------------------------------------------------------------

/// Verify, once and untimed, that the local scalar baseline and the public
/// (SIMD) path compute the same function and that the canonical known-answer
/// tests hold. If any of these fail the benchmark panics immediately, because a
/// throughput ratio between two functions that disagree would be meaningless.
fn assert_correctness() {
    // Known-answer tests for the public API (the values zlib itself produces).
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926, "crc32 KAT mismatch");
    assert_eq!(crc32(0, b""), 0, "crc32 empty-input mismatch");
    assert_eq!(
        adler32(1, b"123456789"),
        0x091E_01DE,
        "adler32 KAT mismatch"
    );
    assert_eq!(adler32(1, b""), 1, "adler32 empty-input mismatch");

    // The local scalar baseline must reproduce the same known-answer tests.
    assert_eq!(
        crc32_scalar_local(0, b"123456789"),
        0xCBF4_3926,
        "local scalar crc32 KAT mismatch"
    );
    assert_eq!(
        crc32_scalar_local(0, b""),
        0,
        "local scalar crc32 empty-input mismatch"
    );

    // Documented invariants of the IEEE reflected single-byte table; these also
    // match row 0 of the crate's build.rs-generated upstream table.
    assert_eq!(SCALAR_CRC_TABLE[1], 0x7707_3096, "crc table[1] mismatch");
    assert_eq!(SCALAR_CRC_TABLE[2], 0xEE0E_612C, "crc table[2] mismatch");
    assert_eq!(SCALAR_CRC_TABLE[3], 0x9909_51BA, "crc table[3] mismatch");

    // Parity on a representative random sample: the SIMD path and the scalar
    // baseline must agree, so the measured ratio compares like for like.
    let sample = incompressible(4096);
    assert_eq!(
        crc32_scalar_local(0, &sample),
        crc32(0, &sample),
        "local scalar and public (SIMD) CRC-32 disagree on a random sample"
    );

    // Rolling/seeded continuation: CRC over a split must equal CRC over the
    // whole, confirming the public path threads a non-zero running CRC correctly.
    let (head, tail) = sample.split_at(1500);
    assert_eq!(
        crc32(crc32(0, head), tail),
        crc32(0, &sample),
        "rolling crc32 continuation mismatch"
    );
}

// ---------------------------------------------------------------------------
// CRC-32 throughput benchmark group (THE GATE)
// ---------------------------------------------------------------------------

/// Benchmark CRC-32 throughput for the public SIMD path versus the local scalar
/// baseline across every (corpus, size) combination.
///
/// Throughput is reported in bytes/s via [`Throughput::Bytes`], which is only
/// available on a `BenchmarkGroup` (the bare `bench_function` interface cannot
/// report throughput). The performance gate (AAP §0.7.3) requires the
/// `simd_public` bytes/s to be **≥ 3×** the `scalar_local` bytes/s for matching
/// inputs; maintainers read both numbers from criterion's output to verify it.
fn bench_crc32(c: &mut Criterion) {
    // Runs once, untimed, before any measurement begins.
    assert_correctness();

    let mut group = c.benchmark_group("crc32");
    for &size in &SIZES {
        // Corpora are built here, OUTSIDE `b.iter`, so timing measures only the
        // checksum computation, never buffer allocation or RNG fill.
        for (label, data) in corpora(size) {
            let param = format!("{label}_{size}");
            group.throughput(Throughput::Bytes(data.len() as u64));

            // Public SIMD path (`zlib_rs::crc32` -> crc32fast under default features).
            group.bench_with_input(BenchmarkId::new("simd_public", &param), &data, |b, d| {
                b.iter(|| black_box(crc32(0, black_box(d))));
            });

            // Local scalar baseline — the ≥3× denominator.
            group.bench_with_input(BenchmarkId::new("scalar_local", &param), &data, |b, d| {
                b.iter(|| black_box(crc32_scalar_local(0, black_box(d))));
            });
        }
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Adler-32 throughput benchmark group
// ---------------------------------------------------------------------------

/// Benchmark Adler-32 throughput of the public [`zlib_rs::adler32`] across every
/// (corpus, size) combination, reported in bytes/s.
fn bench_adler32(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32");
    for &size in &SIZES {
        for (label, data) in corpora(size) {
            let param = format!("{label}_{size}");
            group.throughput(Throughput::Bytes(data.len() as u64));
            group.bench_with_input(BenchmarkId::new("adler32", &param), &data, |b, d| {
                b.iter(|| black_box(adler32(1, black_box(d))));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_crc32, bench_adler32);
criterion_main!(benches);
