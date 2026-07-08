//! Checksum conformance tests: Adler-32 and CRC-32 known-answer vectors plus
//! `*_combine` parity.
//!
//! This integration test locks down the **bit-exact** behavior of the ported
//! checksum layer ([`zlib_rs::checksum`], derived from the semantics of the C
//! `adler32.c` and `crc32.c`) against canonical known-answer vectors, and
//! verifies that the stream-combining operations (`adler32_combine`,
//! `crc32_combine`, and the `crc32_combine_gen`/`crc32_combine_op` pair)
//! reproduce the checksum of a concatenated stream. Byte-identical checksum
//! output is a defining acceptance criterion of the C -> Rust migration
//! (AAP 0.6.4 / 0.7.1: `adler32_combine`/`crc32_combine` must match reference
//! zlib exactly).
//!
//! All expected constants are real, independently verified checksums, e.g. the
//! CRC-32/IEEE "check" value `crc32(0, b"123456789") == 0xCBF4_3926` and the
//! classic Adler-32 example `adler32(1, b"Wikipedia") == 0x11E6_0398`.
//!
//! The tests are pure black-box exercises over the public API: no `unsafe`, no
//! internal (`crate::`) paths, and fully deterministic (the randomized
//! cross-checks are driven by a fixed RNG seed so every run is reproducible).

use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use zlib_rs::checksum::{
    adler32, adler32_combine, adler32_z, crc32, crc32_combine, crc32_combine_gen, crc32_combine_op,
    crc32_z,
};

/// Adler-32 identity / seed. The C idiom `adler = adler32(0L, Z_NULL, 0)`
/// returns `1`, so `1` is the value threaded into the first real update.
const ADLER_SEED: u32 = 1;

/// CRC-32 identity / seed. The C idiom `crc = crc32(0L, Z_NULL, 0)` returns
/// `0`.
const CRC_SEED: u32 = 0;

/// `NMAX` from `adler32.c` — the largest run length Adler-32 accumulates before
/// it must reduce modulo `BASE` (65521). Buffers larger than this exercise the
/// block-boundary path in `adler32` and the `len2 % BASE` reduction inside
/// `adler32_combine`.
const NMAX: usize = 5552;

/// Builds the concatenation `a || b` on the heap for the combine cross-checks.
fn cat(a: &[u8], b: &[u8]) -> Vec<u8> {
    [a, b].concat()
}

/// Produces `len` deterministic pseudo-random bytes from a fixed seed, keeping
/// every run of the randomized cross-checks byte-for-byte reproducible.
///
/// The combine identity asserted by the callers holds for *any* byte content,
/// so the test does not depend on the particular sequence `StdRng` emits — only
/// on it being reproducible for a given seed.
fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut buf = vec![0u8; len];
    rng.fill_bytes(&mut buf);
    buf
}

/// Asserts the Adler-32 stream-combine identity for operands `a` and `b`:
/// combining the two independently seeded checksums with `len2 = b.len()` must
/// equal the one-shot checksum of `a || b`, which in turn must equal the
/// streaming update of `b` onto the running checksum of `a`.
fn assert_adler_combine(a: &[u8], b: &[u8]) {
    let ca = adler32(ADLER_SEED, a);
    let cb = adler32(ADLER_SEED, b);
    let whole = adler32(ADLER_SEED, &cat(a, b));

    // Streaming update over the same bytes must match the one-shot result.
    assert_eq!(
        adler32(ca, b),
        whole,
        "adler32 streaming update mismatch (|a|={}, |b|={})",
        a.len(),
        b.len()
    );

    // The combine of the two component checksums must equal the whole-stream
    // checksum, using only `ca`, `cb`, and the length of `b`.
    assert_eq!(
        adler32_combine(ca, cb, b.len() as i64),
        whole,
        "adler32_combine mismatch (|a|={}, |b|={})",
        a.len(),
        b.len()
    );
}

/// Asserts the CRC-32 stream-combine identity for operands `a` and `b`,
/// mirroring [`assert_adler_combine`], and additionally checks that the
/// precomputed-operator form (`crc32_combine_gen` + `crc32_combine_op`) agrees
/// with the length form.
fn assert_crc_combine(a: &[u8], b: &[u8]) {
    let ca = crc32(CRC_SEED, a);
    let cb = crc32(CRC_SEED, b);
    let whole = crc32(CRC_SEED, &cat(a, b));
    let len2 = b.len() as i64;

    // Streaming update over the same bytes must match the one-shot result.
    assert_eq!(
        crc32(ca, b),
        whole,
        "crc32 streaming update mismatch (|a|={}, |b|={})",
        a.len(),
        b.len()
    );

    // The combine of the two component checksums must equal the whole-stream
    // checksum.
    assert_eq!(
        crc32_combine(ca, cb, len2),
        whole,
        "crc32_combine mismatch (|a|={}, |b|={})",
        a.len(),
        b.len()
    );

    // The precomputed-operator form must agree with the length form.
    let op = crc32_combine_gen(len2);
    assert_eq!(
        crc32_combine_op(ca, cb, op),
        crc32_combine(ca, cb, len2),
        "crc32_combine_op vs crc32_combine mismatch (|b|={})",
        b.len()
    );
}

// ===========================================================================
// Adler-32 known-answer vectors
// ===========================================================================

/// `adler32(1, b"")` is the seed value `1` (C: `adler32(0, Z_NULL, 0) == 1`).
#[test]
fn adler32_empty() {
    assert_eq!(adler32(ADLER_SEED, b""), 1);
}

/// The classic Adler-32 worked example: `"Wikipedia" -> 0x11E6_0398`.
#[test]
fn adler32_wikipedia() {
    assert_eq!(adler32(ADLER_SEED, b"Wikipedia"), 0x11E6_0398);
}

/// `"hello, hello!"` ties to the `hello` literal reused across the test suite;
/// the expected value is cross-checked against reference zlib.
#[test]
fn adler32_hello() {
    assert_eq!(adler32(ADLER_SEED, b"hello, hello!"), 0x2170_0496);
}

/// Feeding a buffer in chunks (chained running updates) must equal the one-shot
/// checksum — the streaming contract of `adler32.c`.
#[test]
fn adler32_incremental_equals_oneshot() {
    let whole = b"The quick brown fox jumps over the lazy dog";
    let (p1, rest) = whole.split_at(10);
    let (p2, p3) = rest.split_at(15);

    let chunked = adler32(adler32(adler32(ADLER_SEED, p1), p2), p3);
    assert_eq!(chunked, adler32(ADLER_SEED, whole));
}

/// `adler32_z` (the `size_t`-length C variant) must agree with `adler32`.
#[test]
fn adler32_z_matches_adler32() {
    let inputs: [&[u8]; 3] = [b"", b"Wikipedia", b"hello, hello!"];
    for input in inputs {
        assert_eq!(adler32_z(ADLER_SEED, input), adler32(ADLER_SEED, input));
    }
}

// ===========================================================================
// CRC-32 known-answer vectors
// ===========================================================================

/// `crc32(0, b"")` is the seed value `0` (C: `crc32(0, Z_NULL, 0) == 0`).
#[test]
fn crc32_empty() {
    assert_eq!(crc32(CRC_SEED, b""), 0);
}

/// The standard CRC-32/IEEE "check" value for the ASCII string `"123456789"`.
#[test]
fn crc32_check_value() {
    assert_eq!(crc32(CRC_SEED, b"123456789"), 0xCBF4_3926);
}

/// A second widely published CRC-32/IEEE vector.
#[test]
fn crc32_quick_brown_fox() {
    assert_eq!(
        crc32(CRC_SEED, b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
}

/// Chunked running updates must equal the one-shot checksum (mirrors the
/// `crc32_z` accumulation contract).
#[test]
fn crc32_incremental_equals_oneshot() {
    let whole = b"The quick brown fox jumps over the lazy dog";
    let (p1, rest) = whole.split_at(10);
    let (p2, p3) = rest.split_at(15);

    let chunked = crc32(crc32(crc32(CRC_SEED, p1), p2), p3);
    assert_eq!(chunked, crc32(CRC_SEED, whole));
}

/// `crc32_z` (the `size_t`-length C variant) must agree with `crc32`.
#[test]
fn crc32_z_matches_crc32() {
    let inputs: [&[u8]; 3] = [
        b"",
        b"123456789",
        b"The quick brown fox jumps over the lazy dog",
    ];
    for input in inputs {
        assert_eq!(crc32_z(CRC_SEED, input), crc32(CRC_SEED, input));
    }
}

// ===========================================================================
// Combine parity — the highest-value assertions (subtle porting bugs hide here)
// ===========================================================================

/// `adler32_combine` over several operand shapes: empty first, empty second,
/// both empty, and a range of non-trivial sizes. Combining the component
/// checksums must reproduce the checksum of the concatenation.
#[test]
fn adler32_combine_matches_concat() {
    assert_adler_combine(b"", b"");
    assert_adler_combine(b"", b"second operand only");
    assert_adler_combine(b"first operand only", b"");
    assert_adler_combine(b"abc", b"defgh");
    assert_adler_combine(b"The quick brown fox ", b"jumps over the lazy dog");
    assert_adler_combine(b"hello", b", hello!");
}

/// `crc32_combine` over the same operand shapes as
/// [`adler32_combine_matches_concat`]; also exercises the operator form via
/// [`assert_crc_combine`].
#[test]
fn crc32_combine_matches_concat() {
    assert_crc_combine(b"", b"");
    assert_crc_combine(b"", b"second operand only");
    assert_crc_combine(b"first operand only", b"");
    assert_crc_combine(b"abc", b"defgh");
    assert_crc_combine(b"The quick brown fox ", b"jumps over the lazy dog");
    assert_crc_combine(b"hello", b", hello!");
}

/// Precomputing the operator for a fixed length with `crc32_combine_gen` and
/// applying it via `crc32_combine_op` must equal calling `crc32_combine` with
/// that length — and both must equal the direct checksum of the concatenation.
#[test]
fn crc32_combine_op_matches_gen() {
    let a = b"operator-form parity check, part one";
    let b = b"operator-form parity check, the second part";
    let ca = crc32(CRC_SEED, a);
    let cb = crc32(CRC_SEED, b);
    let len2 = b.len() as i64;

    let op = crc32_combine_gen(len2);
    assert_eq!(crc32_combine_op(ca, cb, op), crc32_combine(ca, cb, len2));
    assert_eq!(crc32_combine_op(ca, cb, op), crc32(CRC_SEED, &cat(a, b)));
}

/// Combining with a zero-length second stream must return the first checksum
/// unchanged. The second checksum here is the *seed* of an empty buffer
/// (Adler-32 -> 1, CRC-32 -> 0), NOT a bare zero — passing a raw `0` as the
/// second Adler-32 checksum would be incorrect.
#[test]
fn combine_len2_zero_is_identity() {
    let a = b"some leading data for the identity check";
    let ca_adler = adler32(ADLER_SEED, a);
    let ca_crc = crc32(CRC_SEED, a);

    assert_eq!(
        adler32_combine(ca_adler, adler32(ADLER_SEED, b""), 0),
        ca_adler
    );
    assert_eq!(crc32_combine(ca_crc, crc32(CRC_SEED, b""), 0), ca_crc);
}

/// A second buffer longer than `NMAX` exercises both the block-boundary path in
/// `adler32` and the `len2 % BASE` reduction in `adler32_combine`; the same
/// large operands are cross-checked for CRC-32.
#[test]
fn combine_large_len2_crosses_nmax() {
    let a = deterministic_bytes(0x0102_0304_0506_0708, 4096);
    let b = deterministic_bytes(0x1122_3344_5566_7788, 3 * NMAX + 17);
    assert!(
        b.len() > NMAX,
        "second operand must cross the NMAX boundary"
    );

    assert_adler_combine(&a, &b);
    assert_crc_combine(&a, &b);
}

/// Negative lengths are meaningless; reference zlib returns debugging
/// sentinels — `adler32_combine` yields the invalid checksum `0xFFFF_FFFF`,
/// `crc32_combine_gen` yields `0`, and a zero operator makes
/// `crc32_combine_op`/`crc32_combine` yield `0`.
#[test]
fn combine_negative_len2_sentinels() {
    assert_eq!(adler32_combine(0x1234_5678, ADLER_SEED, -1), 0xFFFF_FFFF);
    assert_eq!(crc32_combine_gen(-1), 0);
    assert_eq!(crc32_combine(0xDEAD_BEEF, CRC_SEED, -1), 0);
    assert_eq!(crc32_combine_op(0xDEAD_BEEF, CRC_SEED, 0), 0);
}

// ===========================================================================
// Deterministic randomized cross-checks (fixed seed => reproducible)
// ===========================================================================

/// Random content across a fixed set of size pairs (including one crossing
/// `NMAX`) must satisfy the Adler-32 combine identity.
#[test]
fn adler32_combine_random_deterministic() {
    let size_pairs: [(usize, usize); 8] = [
        (1, 1),
        (7, 9),
        (16, 16),
        (31, 33),
        (100, 250),
        (1024, 1024),
        (NMAX, 64),
        (64, NMAX + 4096),
    ];
    for (i, (la, lb)) in size_pairs.into_iter().enumerate() {
        let a = deterministic_bytes(0xA11E_0000 ^ i as u64, la);
        let b = deterministic_bytes(0xB22F_0000 ^ i as u64, lb);
        assert_adler_combine(&a, &b);
    }
}

/// Random content across a fixed set of size pairs must satisfy the CRC-32
/// combine identity (length form and operator form).
#[test]
fn crc32_combine_random_deterministic() {
    let size_pairs: [(usize, usize); 8] = [
        (1, 1),
        (7, 9),
        (16, 16),
        (31, 33),
        (100, 250),
        (1024, 1024),
        (4096, 64),
        (64, 8192),
    ];
    for (i, (la, lb)) in size_pairs.into_iter().enumerate() {
        let a = deterministic_bytes(0xC33A_0000 ^ i as u64, la);
        let b = deterministic_bytes(0xD44B_0000 ^ i as u64, lb);
        assert_crc_combine(&a, &b);
    }
}
