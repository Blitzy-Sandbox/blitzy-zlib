//! Integration tests for the `zlib-rs` Adler-32 and CRC-32 (IEEE) checksums.
//!
//! This is the foundational, fewest-dependency test binary in the crate's
//! `tests/` folder: it touches **only** the public checksum API — no streaming
//! engine, no I/O — and therefore serves as the canonical example of the test
//! conventions used by the rest of the suite.
//!
//! # What is asserted
//!
//! * **Known-answer vectors** for both [`adler32`] (RFC 1950 seed `1`) and
//!   [`crc32`] (gzip / RFC 1952 seed `0`), every value verified byte-for-byte
//!   against canonical C zlib 1.3.1.
//! * **Streaming/incremental consistency** — folding a checksum across arbitrary
//!   chunk boundaries (and byte-by-byte) reproduces the one-shot result. The
//!   Adler-32 buffer is sized past `NMAX` (5552) to force the internal
//!   modulo-blocking path; the CRC-32 buffer exercises the running accumulator
//!   on whichever back end is compiled (the `simd` `crc32fast` path or the
//!   scalar table fallback — both must agree).
//! * **`*_combine` correctness** — the Adler-32 and CRC-32 combine operations
//!   reconstruct the checksum of a concatenation from the two partial
//!   checksums, including the `crc32_combine_gen` / `crc32_combine_op` operator
//!   form and the empty-input identities.
//! * **`get_crc_table` shape and values** — the standard reflected-`0xedb88320`
//!   byte-wise table.
//!
//! # Conventions (shared by every test file in this folder)
//!
//! * **Public API only.** Compiled as an external crate, this imports
//!   exclusively through `use zlib_rs::checksum::...;` and never reaches into
//!   private internals.
//! * **Standard libtest harness.** Plain `#[test]` functions with
//!   `assert_eq!`/`assert!`; no `harness = false`, no `Cargo.toml` entries
//!   (Cargo auto-discovers `tests/*.rs`).
//! * **Safe Rust only** (enforced below by `#![forbid(unsafe_code)]`).
//! * **Determinism.** Every expectation is a hard-coded value precomputed
//!   against canonical C zlib; the only generated data is a deterministic
//!   `(i % 251)` byte pattern.

// This integration test is a separate crate and does not inherit the core
// crate's `#![forbid(unsafe_code)]`; we re-assert it here so the "zero unsafe in
// checksum tests" convention is a compile-time guarantee for this file too.
#![forbid(unsafe_code)]

// Public re-export path confirmed against `zlib-rs/src/lib.rs` (which exposes
// only `pub mod checksum;`, i.e. there are no crate-root re-exports) and
// `zlib-rs/src/checksum/mod.rs` (which re-exports the nine functions below).
use zlib_rs::checksum::{
    adler32, adler32_combine, adler32_z, crc32, crc32_combine, crc32_combine_gen, crc32_combine_op,
    crc32_z, get_crc_table,
};

// ---------------------------------------------------------------------------
// Shared fixtures / helpers
// ---------------------------------------------------------------------------

/// The classic pangram, length 43. Used for the `*_combine` concatenation tests
/// because its two checksum halves (split at index 10) have well-known values.
const PANGRAM: &[u8] = b"The quick brown fox jumps over the lazy dog";

/// Build a deterministic pseudo-pattern buffer of `n` bytes where byte `i` is
/// `(i % 251)`.
///
/// 251 is the largest prime below 256, so the pattern's period does not align
/// with any power-of-two chunk size used in the tests, making the streaming
/// folds a meaningful check. The value always fits in a `u8`, so the cast is
/// lossless.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

// ===========================================================================
// Phase A — Adler-32 known-answer vectors
// ===========================================================================

/// Adler-32 known-answer vectors (initial seed `1`, the RFC 1950 start value),
/// each verified byte-for-byte against canonical C zlib.
#[test]
fn adler32_known_answers() {
    // Empty buffer leaves the seed unchanged.
    assert_eq!(adler32(1, b""), 0x0000_0001);
    assert_eq!(adler32(1, b"123456789"), 0x091E_01DE);
    assert_eq!(adler32(1, b"Wikipedia"), 0x11E6_0398);
    // This is the zlib Adler-32 trailer value of the `hello` test string used
    // across the rest of the suite (interop / gzip_compat / regression).
    assert_eq!(adler32(1, b"hello, hello!"), 0x2170_0496);
    assert_eq!(adler32(1, b"hello"), 0x062C_0215);
}

/// The `_z` (`size_t`-length) entry point must produce results identical to
/// `adler32` for every vector — both are the same computation behind the two C
/// length-typed symbols.
#[test]
fn adler32_z_matches_adler32() {
    let vectors: [&[u8]; 5] = [b"", b"123456789", b"Wikipedia", b"hello, hello!", b"hello"];
    for buf in vectors {
        assert_eq!(
            adler32_z(1, buf),
            adler32(1, buf),
            "adler32_z disagrees for {buf:?}"
        );
    }
}

/// Seed and identity properties: the initial value is `1`, and feeding an empty
/// slice to any running value is a no-op.
#[test]
fn adler32_seed_and_identity() {
    assert_eq!(adler32(1, b""), 1);
    assert_eq!(adler32_z(1, b""), 1);
    // An empty update never changes a running checksum, whatever its value.
    assert_eq!(adler32(0x1234_5678, b""), 0x1234_5678);
    assert_eq!(adler32_z(0x1234_5678, b""), 0x1234_5678);
}

// ===========================================================================
// Phase B — Adler-32 streaming / incremental consistency (exercises NMAX blocking)
// ===========================================================================

/// Folding the Adler-32 across arbitrary chunk splits — and byte-by-byte — must
/// equal the one-shot result. The buffer length (10 000) is deliberately chosen
/// greater than `NMAX` (5552) so the internal per-block modular reduction in
/// `adler32.c` is exercised.
#[test]
fn adler32_incremental_matches_oneshot() {
    let data = pattern(10_000);
    let one_shot = adler32(1, &data);

    // Arbitrary fixed-size chunks (7 divides neither the length nor NMAX).
    let mut acc = 1u32;
    for chunk in data.chunks(7) {
        acc = adler32(acc, chunk);
    }
    assert_eq!(acc, one_shot, "chunked(7) fold must equal one-shot");

    // The most granular continuation possible: one byte at a time.
    let mut acc_bytes = 1u32;
    for &b in &data {
        acc_bytes = adler32(acc_bytes, &[b]);
    }
    assert_eq!(acc_bytes, one_shot, "byte-by-byte fold must equal one-shot");

    // A second, differently sized chunking through the `_z` entry point.
    let mut acc_z = 1u32;
    for chunk in data.chunks(1024) {
        acc_z = adler32_z(acc_z, chunk);
    }
    assert_eq!(
        acc_z, one_shot,
        "chunked(1024) via adler32_z must equal one-shot"
    );
}

// ===========================================================================
// Phase C — Adler-32 combine
// ===========================================================================

/// `adler32_combine(a1, a2, len2)` reconstructs the checksum of a concatenation
/// from the two partial checksums and the length of the second part. The
/// hard-coded values were precomputed against canonical C zlib for the pangram
/// split at index 10 and must not be "recomputed and adjusted": if the crate
/// disagrees, the crate's combine is wrong.
#[test]
fn adler32_combine_concatenation() {
    let data = PANGRAM;
    assert_eq!(data.len(), 43);

    // Primary split at index 10: part1 = 10 bytes, part2 = 33 bytes.
    let a1 = adler32(1, &data[..10]);
    let a2 = adler32(1, &data[10..]);
    let full = adler32(1, data);
    assert_eq!(a1, 0x13B4_037F);
    assert_eq!(a2, 0xD4DB_0C5C);
    assert_eq!(full, 0x5BDC_0FDA);
    assert_eq!(data[10..].len(), 33);

    // Combine reconstructs the full checksum from the two partial ones.
    assert_eq!(adler32_combine(a1, a2, 33), full);

    // A second split point (index 20, len2 = 23) strengthens the property: the
    // combined value is the same `full` regardless of where the data is split.
    let b1 = adler32(1, &data[..20]);
    let b2 = adler32(1, &data[20..]);
    assert_eq!(data[20..].len(), 23);
    assert_eq!(adler32_combine(b1, b2, 23), full);

    // Zero-length tail: combining with the checksum of an empty buffer (== 1,
    // the Adler identity element) and len2 == 0 returns the original.
    assert_eq!(adler32_combine(full, adler32(1, b""), 0), full);
}

// ===========================================================================
// Phase D — CRC-32 (IEEE) known-answer vectors
// ===========================================================================

/// CRC-32/IEEE known-answer vectors (initial seed `0`), each verified against
/// canonical C zlib.
#[test]
fn crc32_known_answers() {
    assert_eq!(crc32(0, b""), 0x0000_0000);
    // The universally published CRC-32/IEEE check value. A mismatch here is a
    // fundamental polynomial/reflection error in `crc32.rs`, not a bad vector.
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(0, b"Wikipedia"), 0xADAA_C02E);
    assert_eq!(crc32(0, b"hello, hello!"), 0xB39A_DC9B);
    assert_eq!(crc32(0, b"hello"), 0x3610_A686);
}

/// The `_z` (`size_t`-length) entry point must agree with `crc32` for every
/// vector.
#[test]
fn crc32_z_matches_crc32() {
    let vectors: [&[u8]; 5] = [b"", b"123456789", b"Wikipedia", b"hello, hello!", b"hello"];
    for buf in vectors {
        assert_eq!(
            crc32_z(0, buf),
            crc32(0, buf),
            "crc32_z disagrees for {buf:?}"
        );
    }
}

/// Identity: the initial value is `0`, and an empty input is a no-op on any
/// running CRC value.
#[test]
fn crc32_seed_and_identity() {
    assert_eq!(crc32(0, b""), 0);
    assert_eq!(crc32_z(0, b""), 0);
    assert_eq!(crc32(0xDEAD_BEEF, b""), 0xDEAD_BEEF);
    assert_eq!(crc32_z(0xDEAD_BEEF, b""), 0xDEAD_BEEF);
}

// ===========================================================================
// Phase E — CRC-32 streaming / incremental consistency
// ===========================================================================

/// Chunked and byte-by-byte folding of the CRC-32 must equal the one-shot
/// result. On the default `simd` build this exercises the `crc32fast` hardware
/// path; under `--no-default-features` it exercises the scalar table path. Both
/// back ends must yield identical values.
#[test]
fn crc32_incremental_matches_oneshot() {
    let data = pattern(8 * 1024);
    let one_shot = crc32(0, &data);

    let mut acc = 0u32;
    for chunk in data.chunks(7) {
        acc = crc32(acc, chunk);
    }
    assert_eq!(acc, one_shot, "chunked(7) fold must equal one-shot");

    let mut acc_bytes = 0u32;
    for &b in &data {
        acc_bytes = crc32(acc_bytes, &[b]);
    }
    assert_eq!(acc_bytes, one_shot, "byte-by-byte fold must equal one-shot");

    let mut acc_z = 0u32;
    for chunk in data.chunks(1024) {
        acc_z = crc32_z(acc_z, chunk);
    }
    assert_eq!(
        acc_z, one_shot,
        "chunked(1024) via crc32_z must equal one-shot"
    );
}

// ===========================================================================
// Phase F — get_crc_table sanity
// ===========================================================================

/// The byte-wise CRC-32 table is the standard reflected-`0xedb88320` table.
/// Verify its length, the canonical first five entries, and the last entry as
/// an end-anchor.
#[test]
fn crc_table_first_entries() {
    let t = get_crc_table();
    assert_eq!(
        t.len(),
        256,
        "the byte-wise CRC table must have 256 entries"
    );
    assert_eq!(t[0], 0x0000_0000);
    assert_eq!(t[1], 0x7707_3096);
    assert_eq!(t[2], 0xEE0E_612C);
    assert_eq!(t[3], 0x9909_51BA);
    assert_eq!(t[4], 0x076D_C419);
    // Last entry of the canonical reflected table — an end-anchor that catches
    // off-by-one or truncation bugs in the generated table.
    assert_eq!(t[255], 0x2D02_EF8D);
}

// ===========================================================================
// Phase G — CRC-32 combine (and the combine_gen / combine_op operator form)
// ===========================================================================

/// `crc32_combine(c1, c2, len2)` reconstructs the CRC of a concatenation. The
/// hard-coded values were precomputed against canonical C zlib for the pangram
/// split at index 10.
#[test]
fn crc32_combine_concatenation() {
    let data = PANGRAM;

    let c1 = crc32(0, &data[..10]);
    let c2 = crc32(0, &data[10..]);
    let full = crc32(0, data);
    assert_eq!(c1, 0xA3EC_1434);
    assert_eq!(c2, 0x8984_97C4);
    assert_eq!(full, 0x414F_A339);
    assert_eq!(data[10..].len(), 33);

    assert_eq!(crc32_combine(c1, c2, 33), full);
}

/// The generator/operator API must be consistent with the direct
/// `crc32_combine`: a precomputed operator from `crc32_combine_gen(len2)`,
/// applied via `crc32_combine_op`, equals the direct combine. The operator is a
/// pure function of `len2`, so the same operator reconstructs the full CRC for
/// any split whose second part has that length.
#[test]
fn crc32_combine_operator_form() {
    let data = PANGRAM;
    let c1 = crc32(0, &data[..10]);
    let c2 = crc32(0, &data[10..]);
    let full = crc32(0, data);

    // combine_op with the precomputed operator equals the direct combine.
    let op = crc32_combine_gen(33);
    assert_eq!(crc32_combine_op(c1, c2, op), full);
    assert_eq!(crc32_combine_op(c1, c2, op), crc32_combine(c1, c2, 33));

    // The operator depends only on `len2`, not on the data: a different split
    // whose second part is also 23 bytes is reconstructed by `gen(23)`.
    let d1 = crc32(0, &data[..20]);
    let d2 = crc32(0, &data[20..]);
    assert_eq!(data[20..].len(), 23);
    let op23 = crc32_combine_gen(23);
    assert_eq!(crc32_combine_op(d1, d2, op23), full);
    assert_eq!(crc32_combine(d1, d2, 23), full);
}

// ===========================================================================
// Phase H — edge cases & robustness
// ===========================================================================

/// Empty-input combines are identities: appending the checksum of an empty
/// second part (the CRC identity `0`, the Adler identity `1`) with `len2 == 0`
/// returns the running value unchanged.
#[test]
fn combine_empty_input_identities() {
    // CRC: `crc32(0, "") == 0`, so a zero-length second part is the identity on
    // the running CRC. This holds for every `u32` operand.
    for x in [0x0000_0000u32, 0xDEAD_BEEF, 0xB39A_DC9B, 0xFFFF_FFFF] {
        assert_eq!(
            crc32_combine(x, 0, 0),
            x,
            "crc combine identity for {x:#010x}"
        );
    }

    // Adler: `adler32(1, "") == 1` is the identity element. The operands are
    // genuine Adler-32 checksums (each 16-bit half < BASE), matching the C
    // contract that combine inputs are valid checksums.
    let bufs: [&[u8]; 4] = [b"", b"123456789", b"hello, hello!", b"hello"];
    for buf in bufs {
        let x = adler32(1, buf);
        assert_eq!(
            adler32_combine(x, adler32(1, b""), 0),
            x,
            "adler combine identity for {buf:?}"
        );
    }
}

/// A large single buffer (1 MiB) must not panic, and each checksum must equal
/// its chunked fold — a cheap regression guard against accumulator overflow or
/// block-boundary bugs in either implementation.
#[test]
fn large_buffer_matches_chunked_fold() {
    let data = pattern(1024 * 1024);

    let crc_one = crc32(0, &data);
    let mut crc_acc = 0u32;
    for chunk in data.chunks(4096) {
        crc_acc = crc32(crc_acc, chunk);
    }
    assert_eq!(
        crc_acc, crc_one,
        "1 MiB CRC chunked fold must equal one-shot"
    );

    let adler_one = adler32(1, &data);
    let mut adler_acc = 1u32;
    for chunk in data.chunks(4096) {
        adler_acc = adler32(adler_acc, chunk);
    }
    assert_eq!(
        adler_acc, adler_one,
        "1 MiB Adler chunked fold must equal one-shot"
    );
}

// NOTE: the safe-core combine signatures take an UNSIGNED `len2: u64`, so the C
// quirk where a negative `len2` yields `0xffffffff` (Adler) / `0` (CRC) — an
// `int`-overload behavior of the signed `z_off_t` C entry points — is NOT
// reachable through this safe API. That negative-length behavior belongs to the
// `libz-rs-sys` FFI shim's int-typed wrappers and is therefore out of scope for
// this file.
