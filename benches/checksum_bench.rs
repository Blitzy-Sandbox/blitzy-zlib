//! Checksum throughput benchmarks for the `zlib-rs` crate.
//!
//! This [`criterion`] harness measures the throughput of the crate's public,
//! safe-Rust checksum entry points — [`zlib_rs::crc32`] and
//! [`zlib_rs::adler32`] — and exists primarily to **prove the AAP §0.7.3
//! performance gate: the CRC-32 SIMD path must achieve at least `3×` the
//! throughput of a scalar CRC-32 implementation.**
//!
//! # Why a *local* scalar CRC-32 baseline?
//!
//! Under the crate's default feature set the `simd` feature is **on**, so the
//! public [`zlib_rs::crc32`] dispatches to the `crc32fast` SIMD engine. The
//! crate's own *scalar* fallback (`crc32_scalar`) and *SIMD* wrapper
//! (`crc32_simd`) are **module-private** and therefore unreachable from this
//! benchmark, which compiles as a *separate* crate that can only observe `pub`
//! items. To obtain an apples-to-apples `≥3×` denominator this file
//! re-implements a self-contained, table-driven scalar CRC-32 (the reflected
//! IEEE polynomial `0xEDB88320`) and compares its throughput against the
//! public SIMD path. A startup assertion proves both compute the *identical*
//! CRC value, so the throughput ratio is meaningful.
//!
//! # Reading the gate
//!
//! For every `(corpus, size)` pair the `crc32` group reports two throughput
//! numbers in bytes/s: `simd_public` (the crate's public SIMD path) and
//! `scalar_local` (this file's scalar baseline). The gate holds when
//! `simd_public ≥ 3 × scalar_local`. Maintainers and CI read the two numbers
//! straight from the criterion output; on a noisy shared CI VM the measured
//! ratio may compress, so the absolute throughputs are always emitted for
//! inspection rather than asserted here.
//!
//! # Determinism
//!
//! Every corpus is generated from a fixed RNG seed via the stable rand 0.9 API
//! ([`StdRng::seed_from_u64`] + [`RngCore::fill_bytes`]), so runs are
//! reproducible and the two CRC-32 implementations are always measured over
//! byte-identical inputs.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::hint::black_box;
use zlib_rs::{adler32, crc32};

// ===========================================================================
// Local scalar CRC-32 baseline — the `≥3×` gate denominator
// ===========================================================================

/// Build the 256-entry byte-wise CRC-32 lookup table at compile time.
///
/// Uses the reflected IEEE polynomial `0xEDB88320`, identical to the row-0
/// table the crate's `build.rs` generates and to zlib's `crc32.h`. Implemented
/// as a `const fn` (with `while` loops, since `for` is not yet permitted in a
/// `const fn`) so the table is materialized entirely at compile time with no
/// runtime initialization cost.
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

/// Compile-time byte-wise CRC-32 table (reflected IEEE polynomial `0xEDB88320`).
static SCALAR_CRC_TABLE: [u32; 256] = build_crc_table();

/// Self-contained, table-driven scalar CRC-32 (byte-at-a-time recurrence).
///
/// Pre- and post-conditioned with `0xFFFF_FFFF` (expressed as the bitwise
/// complement `!`), matching zlib's convention where a fresh checksum is seeded
/// with `0`. This is the explicit baseline whose throughput forms the `≥3×`
/// gate denominator; it is intentionally the simple scalar form (not
/// slice-by-8) so it represents a portable scalar lower bound.
fn crc32_scalar_local(crc: u32, buf: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in buf {
        c = (c >> 8) ^ SCALAR_CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize];
    }
    !c
}

// ===========================================================================
// Deterministic corpora
// ===========================================================================

/// RNG seed (`"ZLIB"` in ASCII) — fixes the generated corpora across runs so
/// the scalar baseline and the SIMD path are always measured on identical bytes.
const SEED: u64 = 0x5A4C_4942;

/// Buffer sizes exercised by every group: 64 KiB and 1 MiB.
const SIZES: [usize; 2] = [64 * 1024, 1024 * 1024];

/// Printable alphabet used to fold the `text_like` corpus into a realistic,
/// low-entropy distribution (ASCII letters, digits, space, newline).
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 \n";

/// Build the named corpora for a given `size`.
///
/// Returns `(name, bytes)` pairs, all derived from [`SEED`] so the bytes are
/// identical on every run:
///
/// * `incompressible` — uniform random bytes; the representative worst case for
///   checksum table locality and the most honest throughput measurement.
/// * `text_like` — random bytes folded into the small printable [`ALPHABET`],
///   approximating natural-language / source-code input.
///
/// The buffers are built **once per size** by the caller, outside the timing
/// loop, so the measured time is pure checksum computation.
fn corpora(size: usize) -> [(&'static str, Vec<u8>); 2] {
    let mut rng = StdRng::seed_from_u64(SEED);

    let mut incompressible = vec![0u8; size];
    rng.fill_bytes(&mut incompressible);

    let mut text_like = vec![0u8; size];
    rng.fill_bytes(&mut text_like);
    for byte in &mut text_like {
        *byte = ALPHABET[usize::from(*byte) % ALPHABET.len()];
    }

    [("incompressible", incompressible), ("text_like", text_like)]
}

// ===========================================================================
// Startup correctness assertions (apples-to-apples proof)
// ===========================================================================

/// Assert the known-answer tests (KATs) and prove the local scalar baseline
/// computes the *same* function as the public SIMD path.
///
/// Runs once, untimed, at the top of the first benchmark; it panics early if
/// any invariant is violated so a broken build can never report meaningless
/// throughput numbers for the gate.
fn startup_assertions() {
    // Documented invariants of the reflected IEEE table (row 0). These match
    // the crate's `build.rs`-generated table and zlib's `crc32.h`.
    assert_eq!(SCALAR_CRC_TABLE[1], 0x7707_3096, "CRC table entry 1");
    assert_eq!(SCALAR_CRC_TABLE[2], 0xEE0E_612C, "CRC table entry 2");
    assert_eq!(SCALAR_CRC_TABLE[3], 0x9909_51BA, "CRC table entry 3");

    // CRC-32 KATs: the canonical "123456789" check value plus the empty-input
    // identity, for both the scalar baseline and the public (SIMD) path.
    assert_eq!(crc32_scalar_local(0, b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32_scalar_local(0, b""), 0);
    assert_eq!(crc32(0, b""), 0);

    // The local scalar baseline and the public SIMD path must agree on a
    // representative random sample, so the throughput ratio is apples-to-apples.
    let mut sample = vec![0u8; 4096];
    StdRng::seed_from_u64(SEED).fill_bytes(&mut sample);
    assert_eq!(
        crc32_scalar_local(0, &sample),
        crc32(0, &sample),
        "scalar baseline and public SIMD path disagree",
    );

    // Adler-32 KATs: the canonical "123456789" check value and the empty-input
    // identity (a fresh Adler-32 is seeded with `1`).
    assert_eq!(adler32(1, b"123456789"), 0x091E_01DE);
    assert_eq!(adler32(1, b""), 1);
}

// ===========================================================================
// CRC-32 throughput group — THE GATE (AAP §0.7.3: SIMD ≥ 3× scalar)
// ===========================================================================

/// Benchmark the public SIMD CRC-32 against the local scalar baseline.
///
/// For each `(corpus, size)` the group emits two byte/s throughput numbers,
/// `simd_public` and `scalar_local`. The AAP §0.7.3 performance gate requires
/// `simd_public ≥ 3 × scalar_local`; the ratio is verified by reading the two
/// throughput figures from the criterion report (it is not asserted here, to
/// avoid false failures on noisy CI hardware).
fn bench_crc32(c: &mut Criterion) {
    // Untimed: prove correctness and apples-to-apples equivalence before any
    // measurement begins.
    startup_assertions();

    let mut group = c.benchmark_group("crc32");
    for size in SIZES {
        // Built once per size, outside `b.iter`, so timing is pure checksum work.
        for (name, buf) in corpora(size) {
            group.throughput(Throughput::Bytes(buf.len() as u64));

            // Numerator: the crate's public path == SIMD under default features.
            group.bench_with_input(
                BenchmarkId::new("simd_public", format!("{name}/{size}")),
                &buf,
                |b, data| b.iter(|| black_box(crc32(0, black_box(data)))),
            );

            // Denominator: this file's portable scalar baseline.
            group.bench_with_input(
                BenchmarkId::new("scalar_local", format!("{name}/{size}")),
                &buf,
                |b, data| b.iter(|| black_box(crc32_scalar_local(0, black_box(data)))),
            );
        }
    }
    group.finish();
}

// ===========================================================================
// Adler-32 throughput group
// ===========================================================================

/// Benchmark the public Adler-32 entry point across every corpus and size.
///
/// Adler-32 has no SIMD/scalar split in this crate; this group simply records
/// the throughput of [`zlib_rs::adler32`] for completeness alongside the CRC-32
/// gate.
fn bench_adler32(c: &mut Criterion) {
    let mut group = c.benchmark_group("adler32");
    for size in SIZES {
        for (name, buf) in corpora(size) {
            group.throughput(Throughput::Bytes(buf.len() as u64));
            group.bench_with_input(
                BenchmarkId::new("adler32", format!("{name}/{size}")),
                &buf,
                |b, data| b.iter(|| black_box(adler32(1, black_box(data)))),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_crc32, bench_adler32);
criterion_main!(benches);
