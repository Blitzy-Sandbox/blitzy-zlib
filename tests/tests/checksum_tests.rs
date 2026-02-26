#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::uninlined_format_args
)]
//! Adler-32 and CRC-32 checksum correctness tests.
//!
//! Validates the checksum implementations in `zlib-rs` against known test vectors
//! and cross-validates CRC-32 against the `crc` crate reference implementation.
//!
//! # Test Vector Sources
//!
//! - Adler-32 vectors are mathematically derived from the algorithm definition
//!   (RFC 1950 Section 9): `s1 = (1 + sum of bytes) mod 65521`,
//!   `s2 = (sum of all s1 values) mod 65521`, result = `(s2 << 16) | s1`.
//! - CRC-32 vectors use the canonical ISO-HDLC polynomial `0xEDB88320`
//!   (reflected form). The canonical check value for `"123456789"` is
//!   `0xCBF43926`.
//! - Cross-validation uses the `crc` crate (version 3.2) as specified in
//!   AAP Section 0.6.1.
//!
//! # Source Derivation
//!
//! - `adler32.c` (164 lines) — Adler-32 with `NMAX`=5552, `BASE`=65521
//! - `crc32.c` (983 lines) — CRC-32 with table-based computation

use crc::{CRC_32_ISO_HDLC, Crc};
use zlib_rs::checksum::adler32::{adler32, adler32_combine};
use zlib_rs::checksum::crc32::{crc32, crc32_combine};
#[allow(unused_imports)]
use zlib_rs_tests::*;

// =============================================================================
// Helper: Reference Adler-32 computation for cross-validation
// =============================================================================

/// Compute Adler-32 using a simple reference implementation for cross-validation.
///
/// This mirrors the mathematical definition from RFC 1950 Section 9 exactly,
/// with no optimizations, to serve as an independent oracle.
fn reference_adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &byte in data {
        s1 = (s1 + u32::from(byte)) % BASE;
        s2 = (s2 + s1) % BASE;
    }
    (s2 << 16) | s1
}

// =============================================================================
// Phase 2: Adler-32 Basic Correctness Tests
// =============================================================================

/// Adler-32 of empty data should return 1 (the initial value).
///
/// C reference: `adler32(0L, Z_NULL, 0)` returns 1.
/// The initial Adler-32 value has s1=1, s2=0, yielding `(0 << 16) | 1 = 1`.
#[test]
fn test_adler32_empty() {
    // Passing 0 with empty buf returns the initial value
    assert_eq!(adler32(0, &[]), 1);
    // Passing the initial value with empty buf also returns 1
    assert_eq!(adler32(1, &[]), 1);
}

/// Adler-32 of single bytes — verify fundamental accumulation logic.
///
/// For byte `0x00`: s1 = 1+0 = 1, s2 = 0+1 = 1 → (1 << 16) | 1 = `0x00010001`
///
/// Wait — the implementation passes `adler` as the running value. When we call
/// `adler32(1, &[0])`, s1 starts at 1 and s2 starts at 0:
///   s1 = (1 + 0) = 1, s2 = (0 + 1) = 1 → `0x0001_0001`
///
/// For byte `0x01`: s1 = 1+1 = 2, s2 = 0+2 = 2 → (2 << 16) | 2 = `0x0002_0002`
///
/// Wait, that's not right either. Let me trace through the C code:
///   adler = 1 (passed in), sum2 = (adler >> 16) & `0xffff` = 0
///   adler &= `0xffff` → adler = 1
///   For single byte path: adler += buf[0], sum2 += adler
///
/// Byte `0x00`: adler = 1+0 = 1, sum2 = 0+1 = 1 → 1 | (1 << 16) = `0x0001_0001`
/// Byte `0x01`: adler = 1+1 = 2, sum2 = 0+2 = 2 → 2 | (2 << 16) = `0x0002_0002`
///
/// Hmm that doesn't match the spec doc. Let me re-derive:
///
/// Actually from the code: `adler += buf[0]` then `sum2 += adler`.
/// So for byte `0x00`: adler = 1, sum2 = 0+1 = 1 → `0x0001_0001`
/// For byte `0x01`: adler = 2, sum2 = 0+2 = 2 → `0x0002_0002`
///
/// But the spec says `0x00020001` and `0x00030002`. Let me look again at the spec:
///   "For byte `0x00`: adler32(1, [0]) should be ... (2 << 16) | 1 = `0x00020001`"
///
/// The spec says s2 = (1+1) = 2 for the high bits. But from the actual C code,
/// sum2 = (adler >> 16) & `0xffff` = 0, then sum2 += adler (after adler was updated).
/// For byte `0x00`: adler stays 1, sum2 = 0+1 = 1 → `0x0001_0001`.
///
/// The spec doc had an error in the expected values for single byte tests. Let's
/// verify against the reference implementation and the actual C algorithm.
#[test]
fn test_adler32_single_byte() {
    // Byte 0x00: s1 = 1+0 = 1, s2 = 0+1 = 1
    let result_00 = adler32(1, &[0x00]);
    assert_eq!(result_00, reference_adler32(&[0x00]));

    // Byte 0x01: s1 = 1+1 = 2, s2 = 0+2 = 2
    let result_01 = adler32(1, &[0x01]);
    assert_eq!(result_01, reference_adler32(&[0x01]));

    // Byte 0xFF: s1 = 1+255 = 256, s2 = 0+256 = 256
    let result_ff = adler32(1, &[0xFF]);
    assert_eq!(result_ff, reference_adler32(&[0xFF]));

    // Verify specific numeric values from the C algorithm trace
    assert_eq!(result_00, 0x0001_0001_u32); // s1=1, s2=1
    assert_eq!(result_01, 0x0002_0002_u32); // s1=2, s2=2
    assert_eq!(result_ff, 0x0100_0100_u32); // s1=256, s2=256
}

/// Adler-32 of the standard test string "hello, hello!\0" (the `HELLO` constant).
#[test]
fn test_adler32_hello() {
    let result = adler32(1, HELLO);
    let expected = reference_adler32(HELLO);
    assert_eq!(result, expected, "Adler-32 of HELLO mismatch");
}

/// Adler-32 against well-known test vectors.
///
/// These are mathematically derived from the Adler-32 definition:
///   `s1` = (1 + `sum_of_bytes`) mod 65521
///   `s2` = (`sum_of_running_s1`) mod 65521
///   result = (`s2` << 16) | `s1`
#[test]
fn test_adler32_known_vectors() {
    // Empty input returns initial value 1
    assert_eq!(adler32(1, b""), 1);

    // "a" (0x61 = 97): s1 = 1+97 = 98, s2 = 0+98 = 98
    assert_eq!(adler32(1, b"a"), 0x0062_0062);

    // "abc": s1=98→196→295, s2=98→294→589
    assert_eq!(adler32(1, b"abc"), 0x024d_0127);

    // Cross-validate all vectors against the reference implementation
    let vectors: &[&[u8]] = &[
        b"",
        b"a",
        b"abc",
        b"message digest",
        b"abcdefghijklmnopqrstuvwxyz",
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        b"123456789",
    ];

    for &data in vectors {
        let actual = adler32(1, data);
        let expected = reference_adler32(data);
        assert_eq!(
            actual,
            expected,
            "Adler-32 mismatch for {:?}",
            std::str::from_utf8(data).unwrap_or("<binary>")
        );
    }
}

/// Verify incremental Adler-32 computation matches one-shot computation.
///
/// This tests the `adler` parameter chaining: computing the checksum in two
/// halves and feeding the intermediate result as the initial value for the
/// second half must produce the same result as computing over the full data.
#[test]
fn test_adler32_incremental() {
    let data = b"The quick brown fox jumps over the lazy dog";
    let mid = data.len() / 2;

    // One-shot computation
    let one_shot = adler32(1, data);

    // Incremental: first half, then second half
    let first_half = adler32(1, &data[..mid]);
    let incremental = adler32(first_half, &data[mid..]);

    assert_eq!(
        one_shot, incremental,
        "Incremental Adler-32 does not match one-shot"
    );

    // Also test with three-way split
    let third = data.len() / 3;
    let part1 = adler32(1, &data[..third]);
    let part2 = adler32(part1, &data[third..2 * third]);
    let part3 = adler32(part2, &data[2 * third..]);
    assert_eq!(one_shot, part3, "Three-way incremental mismatch");
}

// =============================================================================
// Phase 3: Adler-32 NMAX Boundary Tests
// =============================================================================

/// Test Adler-32 around the `NMAX`=5552 boundary.
///
/// `NMAX` is the maximum number of bytes that can be processed in a single block
/// before a modulo reduction is required. Testing at and around this boundary
/// exercises the block-processing modulo path in the optimized loop.
#[test]
fn test_adler32_nmax_boundary() {
    // NMAX boundary: 5551, 5552, 5553 bytes of 0xFF (maximum accumulator stress)
    for &len in &[5551_usize, 5552, 5553] {
        let data = vec![0xFF_u8; len];
        let result = adler32(1, &data);
        let expected = reference_adler32(&data);
        assert_eq!(
            result, expected,
            "Adler-32 NMAX boundary test failed for len={}",
            len
        );
    }

    // Test with exactly 2*NMAX to exercise two full blocks
    let data = vec![0xFF_u8; 5552 * 2];
    let result = adler32(1, &data);
    let expected = reference_adler32(&data);
    assert_eq!(result, expected, "Adler-32 for 2*NMAX failed");

    // Test with NMAX + 16 to exercise both the full-block and remainder paths
    let data = vec![0xAB_u8; 5552 + 16];
    let result = adler32(1, &data);
    let expected = reference_adler32(&data);
    assert_eq!(result, expected, "Adler-32 for NMAX+16 failed");
}

/// Test Adler-32 with large input (64KB+) to exercise the `NMAX`-optimized
/// block processing loop from `adler32.c`.
#[test]
fn test_adler32_large_data() {
    // 64KB of a repeating pattern
    let mut data = vec![0u8; 65536];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 251) as u8; // Use a prime period for variety
    }

    let result = adler32(1, &data);
    let expected = reference_adler32(&data);
    assert_eq!(result, expected, "Adler-32 for 64KB data mismatch");

    // 256KB — exercises many NMAX blocks
    let mut large = vec![0u8; 262_144];
    for (i, byte) in large.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(137) % 256) as u8;
    }
    let result_large = adler32(1, &large);
    let expected_large = reference_adler32(&large);
    assert_eq!(result_large, expected_large, "Adler-32 for 256KB mismatch");
}

// =============================================================================
// Phase 4: Adler-32 Combine Tests
// =============================================================================

/// Test `adler32_combine()` — combining two Adler-32 checksums.
///
/// Given data split into two halves with individual Adler-32 checksums,
/// `adler32_combine` should produce the same result as computing the
/// Adler-32 of the full concatenated data.
#[test]
fn test_adler32_combine_basic() {
    let data = b"The quick brown fox jumps over the lazy dog";
    let mid = data.len() / 2;

    // Full checksum
    let full = adler32(1, data);

    // Individual checksums of each half
    let adler1 = adler32(1, &data[..mid]);
    let adler2 = adler32(1, &data[mid..]);

    // Combine
    let combined = adler32_combine(adler1, adler2, (data.len() - mid) as i64);

    assert_eq!(
        full, combined,
        "adler32_combine result does not match full checksum"
    );
}

/// Combine with an empty second part should return the first checksum.
#[test]
fn test_adler32_combine_empty() {
    let data = b"hello";
    let adler1 = adler32(1, data);

    // Combining with initial value (1) and length 0 should return adler1
    let combined = adler32_combine(adler1, 1, 0);
    assert_eq!(combined, adler1, "Combine with empty second part failed");
}

/// Test `adler32_combine()` at every possible split point for a small vector.
#[test]
fn test_adler32_combine_various_splits() {
    let data = b"abcdefghijklmnop"; // 16 bytes — small enough for exhaustive splits
    let full = adler32(1, data);

    for split in 1..data.len() {
        let adler1 = adler32(1, &data[..split]);
        let adler2 = adler32(1, &data[split..]);
        let len2 = (data.len() - split) as i64;
        let combined = adler32_combine(adler1, adler2, len2);

        assert_eq!(
            full, combined,
            "adler32_combine mismatch at split point {}",
            split
        );
    }
}

/// Negative length to `adler32_combine` should return `0xFFFFFFFF`.
///
/// C reference: `adler32_combine_` returns `0xffffffffUL` for negative `len2`.
#[test]
fn test_adler32_combine_negative_length() {
    let result = adler32_combine(0x0001_0001, 0x0001_0001, -1);
    assert_eq!(
        result, 0xFFFF_FFFF,
        "Negative len2 should return 0xFFFFFFFF"
    );
}

// =============================================================================
// Phase 5: CRC-32 Basic Correctness Tests
// =============================================================================

/// CRC-32 of empty data should return 0.
///
/// C reference: `crc32(0L, Z_NULL, 0)` returns 0.
#[test]
fn test_crc32_empty() {
    assert_eq!(crc32(0, &[]), 0);
}

/// CRC-32 against well-known test vectors.
///
/// These are the canonical CRC-32/ISO-HDLC test values (polynomial `0xEDB88320`
/// in reflected form, also known as CRC-32, CRC-32/ADCCP, CRC-32/V-42,
/// CRC-32/XZ, PKZIP CRC-32).
#[test]
fn test_crc32_known_vectors() {
    // Empty input
    assert_eq!(crc32(0, b""), 0);

    // Canonical check value: CRC-32 of "123456789" = 0xCBF43926
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);

    // Single character "a"
    assert_eq!(crc32(0, b"a"), 0xe8b7_be43);

    // "abc"
    assert_eq!(crc32(0, b"abc"), 0x3524_41c2);

    // "message digest"
    assert_eq!(crc32(0, b"message digest"), 0x2015_9d7f);

    // "abcdefghijklmnopqrstuvwxyz"
    assert_eq!(crc32(0, b"abcdefghijklmnopqrstuvwxyz"), 0x4c27_50bd);
}

/// Verify incremental CRC-32 computation matches one-shot computation.
///
/// The CRC-32 algorithm supports incremental computation by passing the
/// previous CRC as the initial value for the next chunk.
#[test]
fn test_crc32_incremental() {
    let data = b"The quick brown fox jumps over the lazy dog";
    let mid = data.len() / 2;

    // One-shot computation
    let one_shot = crc32(0, data);

    // Incremental: first half, then second half using previous CRC
    let first_half = crc32(0, &data[..mid]);
    let incremental = crc32(first_half, &data[mid..]);

    assert_eq!(
        one_shot, incremental,
        "Incremental CRC-32 does not match one-shot"
    );

    // Three-way split
    let third = data.len() / 3;
    let part1 = crc32(0, &data[..third]);
    let part2 = crc32(part1, &data[third..2 * third]);
    let part3 = crc32(part2, &data[2 * third..]);
    assert_eq!(one_shot, part3, "Three-way incremental CRC-32 mismatch");
}

// =============================================================================
// Phase 6: CRC-32 Cross-Validation Against `crc` Crate
// =============================================================================

/// Cross-validate CRC-32 against the `crc` crate reference implementation.
///
/// Per AAP Section 0.6.1, the `crc` crate (v3.2) provides an independent
/// CRC-32/ISO-HDLC implementation for verifying that `zlib-rs` produces
/// identical checksums.
#[test]
fn test_crc32_cross_validate() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    // Test with various input patterns
    let test_inputs: Vec<Vec<u8>> = vec![
        // Empty
        vec![],
        // Single byte
        vec![0x00],
        vec![0xFF],
        vec![0x61], // 'a'
        // Small strings
        b"abc".to_vec(),
        b"123456789".to_vec(),
        b"hello, hello!".to_vec(),
        // Medium data: 1KB of repeating pattern
        (0..1024).map(|i| (i % 256) as u8).collect(),
        // All zeros (1KB)
        vec![0x00; 1024],
        // All ones (1KB)
        vec![0xFF; 1024],
        // Pseudo-random deterministic pattern (1KB)
        (0..1024_u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect(),
    ];

    for (idx, input) in test_inputs.iter().enumerate() {
        let zlib_result = crc32(0, input);
        let ref_result = crc_engine.checksum(input);
        assert_eq!(
            zlib_result,
            ref_result,
            "CRC-32 cross-validation mismatch for test input #{} (len={})",
            idx,
            input.len()
        );
    }
}

/// Large data cross-validation: 1MB of pseudo-deterministic data.
#[test]
fn test_crc32_cross_validate_large() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    // Generate 1MB of deterministic data using a simple PRNG-like pattern
    let data: Vec<u8> = (0..1_048_576_u32)
        .map(|i| {
            // Knuth's multiplicative hash for deterministic pseudo-random bytes
            (i.wrapping_mul(2_654_435_761) >> 24) as u8
        })
        .collect();

    let zlib_result = crc32(0, &data);
    let ref_result = crc_engine.checksum(&data);
    assert_eq!(
        zlib_result, ref_result,
        "CRC-32 cross-validation mismatch for 1MB data"
    );
}

// =============================================================================
// Phase 7: CRC-32 Combine Tests
// =============================================================================

/// Test `crc32_combine()` — combining two CRC-32 checksums.
///
/// Given data split into two halves, the CRC of each half computed independently
/// (second half starting from CRC=0), `crc32_combine` should produce the CRC
/// of the full concatenated data.
#[test]
fn test_crc32_combine_basic() {
    let data = b"The quick brown fox jumps over the lazy dog";
    let mid = data.len() / 2;

    // Full CRC
    let full = crc32(0, data);

    // Individual CRCs (second half starts fresh with CRC=0)
    let crc1 = crc32(0, &data[..mid]);
    let crc2 = crc32(0, &data[mid..]);

    // Combine
    let combined = crc32_combine(crc1, crc2, (data.len() - mid) as i64);

    assert_eq!(
        full, combined,
        "crc32_combine result does not match full CRC"
    );
}

/// Test `crc32_combine()` at various split points.
#[test]
fn test_crc32_combine_various_splits() {
    let data = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let full = crc32(0, data);

    for split in 1..data.len() {
        let crc1 = crc32(0, &data[..split]);
        let crc2 = crc32(0, &data[split..]);
        let len2 = (data.len() - split) as i64;
        let combined = crc32_combine(crc1, crc2, len2);

        assert_eq!(
            full, combined,
            "crc32_combine mismatch at split point {}",
            split
        );
    }
}

/// Test `crc32_combine()` with empty second part.
#[test]
fn test_crc32_combine_empty_second() {
    let data = b"hello";
    let crc1 = crc32(0, data);
    let crc2 = crc32(0, &[]); // CRC of empty = 0

    let combined = crc32_combine(crc1, crc2, 0);
    assert_eq!(combined, crc1, "Combine with empty second part failed");
}

/// Test `crc32_combine()` with large data to stress the GF(2) polynomial math.
#[test]
fn test_crc32_combine_large() {
    let mut data = vec![0u8; 100_000];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(31) % 256) as u8;
    }

    let full = crc32(0, &data);
    let mid = 50_000;

    let crc1 = crc32(0, &data[..mid]);
    let crc2 = crc32(0, &data[mid..]);
    let combined = crc32_combine(crc1, crc2, (data.len() - mid) as i64);

    assert_eq!(full, combined, "crc32_combine for 100KB data mismatch");
}

// =============================================================================
// Phase 8: Edge Cases
// =============================================================================

/// Adler-32 of a large buffer of all zeros.
///
/// For all-zero data, s1 = 1 (never changes) and s2 = len (each step adds 1).
/// After modulo: s2 = len % 65521.
#[test]
fn test_adler32_all_zeros() {
    for &len in &[1_usize, 100, 1000, 10000, 65521, 100_000] {
        let data = vec![0u8; len];
        let result = adler32(1, &data);
        let expected = reference_adler32(&data);
        assert_eq!(
            result, expected,
            "Adler-32 all-zeros mismatch for len={}",
            len
        );
    }
}

/// Adler-32 of a large buffer of all `0xFF` bytes.
///
/// This stresses the accumulator because `0xFF`=255 is the maximum byte value,
/// causing the fastest growth of s1 and s2 and exercising the modulo reduction
/// path most aggressively.
#[test]
fn test_adler32_all_ones() {
    for &len in &[1_usize, 100, 1000, 5552, 10000, 100_000] {
        let data = vec![0xFF_u8; len];
        let result = adler32(1, &data);
        let expected = reference_adler32(&data);
        assert_eq!(
            result, expected,
            "Adler-32 all-0xFF mismatch for len={}",
            len
        );
    }
}

/// CRC-32 of a large buffer of all zeros.
#[test]
fn test_crc32_all_zeros() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    for &len in &[1_usize, 100, 1000, 10000, 100_000] {
        let data = vec![0u8; len];
        let result = crc32(0, &data);
        let expected = crc_engine.checksum(&data);
        assert_eq!(
            result, expected,
            "CRC-32 all-zeros mismatch for len={}",
            len
        );
    }
}

/// CRC-32 of a large buffer of all `0xFF` bytes.
#[test]
fn test_crc32_all_ones() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    for &len in &[1_usize, 100, 1000, 10000, 100_000] {
        let data = vec![0xFF_u8; len];
        let result = crc32(0, &data);
        let expected = crc_engine.checksum(&data);
        assert_eq!(result, expected, "CRC-32 all-0xFF mismatch for len={}", len);
    }
}

/// CRC-32 of each individual byte value `0x00`–`0xFF`.
///
/// This validates the entire first row of the CRC lookup table, since
/// CRC-32 of a single byte is effectively a direct table lookup.
#[test]
fn test_crc32_single_byte_all_values() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    for byte_val in 0x00_u8..=0xFF {
        let data = [byte_val];
        let result = crc32(0, &data);
        let expected = crc_engine.checksum(&data);
        assert_eq!(
            result, expected,
            "CRC-32 single-byte mismatch for 0x{:02X}",
            byte_val
        );
    }
}

/// Adler-32 with the HELLO test constant used throughout the test suite.
///
/// Cross-validates against the reference implementation and ensures the
/// standard test string produces a consistent checksum.
#[test]
fn test_adler32_hello_constant() {
    let result = adler32(1, HELLO);
    let expected = reference_adler32(HELLO);
    assert_eq!(result, expected, "Adler-32 of HELLO constant mismatch");

    // Verify it's non-trivial (not 0 or 1)
    assert_ne!(result, 0);
    assert_ne!(result, 1);
}

/// CRC-32 with the HELLO test constant.
#[test]
fn test_crc32_hello_constant() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    let result = crc32(0, HELLO);
    let expected = crc_engine.checksum(HELLO);
    assert_eq!(result, expected, "CRC-32 of HELLO constant mismatch");

    // Verify it's non-trivial
    assert_ne!(result, 0);
}

/// Adler-32 combine with various data patterns to stress the combine formula.
#[test]
fn test_adler32_combine_stress() {
    let patterns: Vec<Vec<u8>> = vec![
        // Ascending bytes
        (0..200_u8).collect(),
        // Descending bytes
        (0..200_u8).rev().collect(),
        // Alternating 0x00 and 0xFF
        (0..200)
            .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
            .collect(),
        // Large data
        vec![0x42_u8; 10_000],
    ];

    for (idx, data) in patterns.iter().enumerate() {
        let full = adler32(1, data);

        // Split at 1/3
        let split = data.len() / 3;
        let adler1 = adler32(1, &data[..split]);
        let adler2 = adler32(1, &data[split..]);
        let combined = adler32_combine(adler1, adler2, (data.len() - split) as i64);

        assert_eq!(
            full,
            combined,
            "adler32_combine stress test #{} failed (len={})",
            idx,
            data.len()
        );
    }
}

/// CRC-32 combine stress test with various data patterns.
#[test]
fn test_crc32_combine_stress() {
    let patterns: Vec<Vec<u8>> = vec![
        (0..200_u8).collect(),
        (0..200_u8).rev().collect(),
        (0..200)
            .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
            .collect(),
        vec![0x42_u8; 10_000],
    ];

    for (idx, data) in patterns.iter().enumerate() {
        let full = crc32(0, data);

        let split = data.len() / 3;
        let crc1 = crc32(0, &data[..split]);
        let crc2 = crc32(0, &data[split..]);
        let combined = crc32_combine(crc1, crc2, (data.len() - split) as i64);

        assert_eq!(
            full,
            combined,
            "crc32_combine stress test #{} failed (len={})",
            idx,
            data.len()
        );
    }
}

/// Adler-32 short input path: inputs of length 2–15 exercise the
/// `len < 16` fast path that avoids `NMAX` blocking.
#[test]
fn test_adler32_short_inputs() {
    for len in 2..16_usize {
        let data: Vec<u8> = (0..len as u8).collect();
        let result = adler32(1, &data);
        let expected = reference_adler32(&data);
        assert_eq!(
            result, expected,
            "Adler-32 short input mismatch for len={}",
            len
        );
    }
}

/// Adler-32 exactly 16 bytes — boundary between short path and chunked path.
#[test]
fn test_adler32_exactly_16_bytes() {
    let data: Vec<u8> = (0..16_u8).collect();
    let result = adler32(1, &data);
    let expected = reference_adler32(&data);
    assert_eq!(result, expected, "Adler-32 for exactly 16 bytes mismatch");
}

/// CRC-32 incremental with byte-at-a-time processing.
///
/// Processes each byte individually through the CRC function and verifies
/// the result matches one-shot computation.
#[test]
fn test_crc32_byte_at_a_time() {
    let data = b"abcdefghijklmnopqrstuvwxyz";

    // One-shot
    let one_shot = crc32(0, data);

    // Byte-at-a-time
    let mut running_crc = 0_u32;
    for &byte in data {
        running_crc = crc32(running_crc, &[byte]);
    }

    assert_eq!(
        one_shot, running_crc,
        "Byte-at-a-time CRC-32 does not match one-shot"
    );
}

/// Adler-32 incremental with byte-at-a-time processing.
#[test]
fn test_adler32_byte_at_a_time() {
    let data = b"abcdefghijklmnopqrstuvwxyz";

    // One-shot
    let one_shot = adler32(1, data);

    // Byte-at-a-time
    let mut running = 1_u32; // Initial Adler-32 value
    for &byte in data {
        running = adler32(running, &[byte]);
    }

    assert_eq!(
        one_shot, running,
        "Byte-at-a-time Adler-32 does not match one-shot"
    );
}

/// CRC-32 cross-validation with medium-sized deterministic patterns.
#[test]
fn test_crc32_cross_validate_medium() {
    let crc_engine = Crc::<u32>::new(&CRC_32_ISO_HDLC);

    // 10KB of ascending bytes (wrapping at 256)
    let ascending: Vec<u8> = (0..10_240_u32).map(|i| (i % 256) as u8).collect();
    assert_eq!(
        crc32(0, &ascending),
        crc_engine.checksum(&ascending),
        "CRC-32 mismatch for 10KB ascending"
    );

    // 10KB of descending bytes
    let descending: Vec<u8> = (0..10_240_u32).map(|i| (255 - (i % 256)) as u8).collect();
    assert_eq!(
        crc32(0, &descending),
        crc_engine.checksum(&descending),
        "CRC-32 mismatch for 10KB descending"
    );

    // 10KB of alternating 0x55/0xAA (binary checkerboard)
    let checker: Vec<u8> = (0..10_240)
        .map(|i| if i % 2 == 0 { 0x55 } else { 0xAA })
        .collect();
    assert_eq!(
        crc32(0, &checker),
        crc_engine.checksum(&checker),
        "CRC-32 mismatch for 10KB checkerboard"
    );
}
