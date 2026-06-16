//! Adler-32 / CRC-32 known-answer and `*_combine` integration tests.
//!
//! This is a Cargo **integration test** for the C→Rust rewrite of zlib
//! (`zlib-rs`, import path `zlib_rs`). It links the crate exactly as an external
//! consumer would — through `use zlib_rs::...` — and exercises the public
//! checksum surface with **known-answer tests (KATs)** plus the combine family.
//!
//! Conceptually sourced from `test/example.c`, which seeds zlib streams' Adler
//! with the literal `1` and tracks the running value in `strm.adler`. The values
//! pinned below are the **frozen wire-format contract** (AAP §0.7.1): the
//! checksums are part of the zlib (RFC 1950) and gzip (RFC 1952) stream
//! trailers, so each one must hold bit-exactly against canonical C zlib.
//!
//! All checksum entry points are always available (no feature gate) and are
//! `no_std`-safe in the library; the test harness itself is, of course, `std`.
//! There is no `unsafe` anywhere in this file.
//!
//! Run with: `cargo test --test checksum`.

#![forbid(unsafe_code)]

// The eight checksum functions re-exported at the crate root (`src/lib.rs`).
use zlib_rs::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_z,
};
// The remaining checksum functions are re-exported from the `checksum` module
// root (`src/checksum/mod.rs`) but not lifted to the crate root.
use zlib_rs::checksum::{crc32_combine_gen, crc32_combine_op, get_crc_table};

// ===========================================================================
// Private helpers (kept local to this file per the file spec — no shared module)
// ===========================================================================

/// Deterministic pseudo-random buffer generator.
///
/// Mirrors the Python oracle generator `bytes((i*131 + 17) & 0xff ...)` used to
/// pin the large-buffer KATs below, so the values this test asserts are exactly
/// what canonical C zlib produces for the same inputs.
fn gen_buf(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 131 + 17) & 0xff) as u8).collect()
}

/// Split indices (in `0..=len`) to exercise for a combine identity.
///
/// For small inputs every split point is tested exhaustively. For large inputs a
/// bounded, representative sample — including the Adler-32 `NMAX` = 5552 block
/// boundary — keeps the `O(splits * len)` work reasonable while still covering
/// the per-block modulo-reduction cadence.
fn combine_splits(len: usize) -> Vec<usize> {
    if len <= 512 {
        (0..=len).collect()
    } else {
        let candidates = [
            0usize,
            1,
            2,
            15,
            16,
            17,
            255,
            256,
            5551,
            5552,
            5553,
            len / 2,
            len - 1,
            len,
        ];
        let mut points: Vec<usize> = candidates.into_iter().filter(|&k| k <= len).collect();
        points.sort_unstable();
        points.dedup();
        points
    }
}

/// Assert the CRC-32 split-and-combine identity at every sampled split point.
///
/// For `data` split into `a = data[..k]` and `b = data[k..]`, this checks that
/// `crc32_combine(crc32(0, a), crc32(0, b), b.len())` reproduces the one-shot
/// `crc32(0, data)`, and verifies that the 64-bit-length variant and the
/// precomputed-operator (`gen` + `op`) path agree with it.
fn check_combine_crc(data: &[u8]) {
    let total = data.len();
    let whole = crc32(0, data);
    for k in combine_splits(total) {
        let (a, b) = data.split_at(k);
        let crc_a = crc32(0, a);
        let crc_b = crc32(0, b);
        let len2 = b.len() as i64;
        assert_eq!(
            crc32_combine(crc_a, crc_b, len2),
            whole,
            "crc32_combine mismatch at split k={k} (len={total})"
        );
        assert_eq!(
            crc32_combine64(crc_a, crc_b, len2),
            whole,
            "crc32_combine64 mismatch at split k={k} (len={total})"
        );
        let op = crc32_combine_gen(len2);
        assert_eq!(
            crc32_combine_op(crc_a, crc_b, op),
            whole,
            "crc32_combine_op mismatch at split k={k} (len={total})"
        );
    }
}

/// Assert the Adler-32 split-and-combine identity at every sampled split point.
///
/// For `data` split into `a = data[..k]` and `b = data[k..]`, this checks that
/// `adler32_combine(adler32(1, a), adler32(1, b), b.len())` reproduces the
/// one-shot `adler32(1, data)`, and that the 64-bit-length variant agrees.
fn check_combine_adler(data: &[u8]) {
    let total = data.len();
    let whole = adler32(1, data);
    for k in combine_splits(total) {
        let (a, b) = data.split_at(k);
        let adler_a = adler32(1, a);
        let adler_b = adler32(1, b);
        let len2 = b.len() as i64;
        assert_eq!(
            adler32_combine(adler_a, adler_b, len2),
            whole,
            "adler32_combine mismatch at split k={k} (len={total})"
        );
        assert_eq!(
            adler32_combine64(adler_a, adler_b, len2),
            whole,
            "adler32_combine64 mismatch at split k={k} (len={total})"
        );
    }
}

// ===========================================================================
// CRC-32 known-answer tests (IEEE reflected, POLY = 0xEDB88320)
// ===========================================================================

#[test]
fn crc32_check_value() {
    // The canonical CRC-32 check value for the standard "123456789" message.
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
}

#[test]
fn crc32_empty() {
    // An empty buffer with a zero seed is zero.
    assert_eq!(crc32(0, b""), 0);
}

#[test]
fn crc32_empty_continuation() {
    // Feeding nothing after an empty start keeps the running CRC at zero.
    assert_eq!(crc32(crc32(0, b""), b""), 0);
}

#[test]
fn crc32_single_zero_byte() {
    // The CRC-32 of a single zero byte.
    assert_eq!(crc32(0, &[0u8]), 0xD202_EF8D);
}

#[test]
fn crc32_quick_brown_fox() {
    assert_eq!(
        crc32(0, b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
}

#[test]
fn crc32_short_a() {
    assert_eq!(crc32(0, b"a"), 0xE8B7_BE43);
}

#[test]
fn crc32_short_abc() {
    assert_eq!(crc32(0, b"abc"), 0x3524_41C2);
}

#[test]
fn crc32_incremental_two_chunks() {
    // Streaming the check message as two chunks equals the single-call result.
    let (first, second) = b"123456789".split_at(4);
    assert_eq!(crc32(crc32(0, first), second), 0xCBF4_3926);
}

#[test]
fn crc32_incremental_many_splits() {
    // Every two-chunk split of the check message reproduces the check value.
    let data = b"123456789";
    for k in 0..=data.len() {
        let (first, second) = data.split_at(k);
        assert_eq!(
            crc32(crc32(0, first), second),
            0xCBF4_3926,
            "split at k={k}"
        );
    }
}

#[test]
fn crc32_incremental_single_bytes() {
    // Feeding one byte at a time equals the one-shot CRC.
    let mut crc = 0u32;
    for &byte in b"123456789" {
        crc = crc32(crc, &[byte]);
    }
    assert_eq!(crc, 0xCBF4_3926);
}

#[test]
fn crc32_incremental_large_buffer() {
    // Splitting a large buffer anywhere yields the same CRC as one shot.
    let data = gen_buf(6000);
    let one_shot = crc32(0, &data);
    for k in combine_splits(data.len()) {
        let (first, second) = data.split_at(k);
        assert_eq!(crc32(crc32(0, first), second), one_shot, "split at k={k}");
    }
}

#[test]
fn crc32_z_matches_crc32() {
    // The `_z` variant differs only in the C length type; results must match.
    let buffers: [&[u8]; 4] = [
        b"",
        b"a",
        b"123456789",
        b"The quick brown fox jumps over the lazy dog",
    ];
    for buf in buffers {
        assert_eq!(crc32_z(0, buf), crc32(0, buf));
    }
    let big = gen_buf(6000);
    assert_eq!(crc32_z(0, &big), crc32(0, &big));
}

#[test]
fn crc32_z_check_value() {
    assert_eq!(crc32_z(0, b"123456789"), 0xCBF4_3926);
}

#[test]
fn crc32_large_buffer_pinned() {
    // Pinned against canonical C zlib for the deterministic generator buffer.
    assert_eq!(crc32(0, &gen_buf(6000)), 0x3BD4_4EF0);
    assert_eq!(crc32(0, &gen_buf(40_000)), 0x26D6_62ED);
}

#[test]
fn crc32_table_entries() {
    // The canonical single-byte CRC-32 lookup table (zlib `get_crc_table`).
    let table = get_crc_table();
    assert_eq!(table[0], 0x0000_0000);
    assert_eq!(table[1], 0x7707_3096);
    assert_eq!(table[2], 0xEE0E_612C);
    assert_eq!(table[3], 0x9909_51BA);
    assert_eq!(table[255], 0x2D02_EF8D);
}

#[test]
fn crc32_table_length() {
    // `get_crc_table` returns the full 256-entry single-byte table.
    assert_eq!(get_crc_table().len(), 256);
}

// ===========================================================================
// CRC-32 combine tests (the *_combine family)
// ===========================================================================

#[test]
fn crc32_combine_123456789_all_splits() {
    let data = b"123456789";
    assert_eq!(crc32(0, data), 0xCBF4_3926);
    // For len <= 512, check_combine_crc exhaustively covers every split point.
    check_combine_crc(data);
}

#[test]
fn crc32_combine_large() {
    // A larger fixed buffer combined at a representative sample of split points.
    check_combine_crc(&gen_buf(6000));
}

#[test]
fn crc32_combine_very_large() {
    // Spanning many bytes stresses the repeated-squaring zero-advance machinery.
    check_combine_crc(&gen_buf(40_000));
}

#[test]
fn crc32_combine64_matches_combine() {
    // The 64-bit-length entry point must match the 32-bit one for in-range lens.
    let data = gen_buf(2048);
    for k in [0usize, 1, 1000, 2047, 2048] {
        let (a, b) = data.split_at(k);
        let crc_a = crc32(0, a);
        let crc_b = crc32(0, b);
        let len2 = b.len() as i64;
        assert_eq!(
            crc32_combine64(crc_a, crc_b, len2),
            crc32_combine(crc_a, crc_b, len2),
            "k={k}"
        );
    }
}

#[test]
fn crc32_combine_gen_op_path() {
    // Precomputing the operator and applying it equals the direct combine.
    let (first, second) = b"123456789".split_at(4);
    let crc_a = crc32(0, first);
    let crc_b = crc32(0, second);
    let len2 = second.len() as i64;
    let op = crc32_combine_gen(len2);
    assert_eq!(crc32_combine_op(crc_a, crc_b, op), 0xCBF4_3926);
    assert_eq!(
        crc32_combine_op(crc_a, crc_b, op),
        crc32_combine(crc_a, crc_b, len2)
    );
}

#[test]
fn crc32_combine_op_zero_returns_zero() {
    // A zero operator is the documented invalid sentinel and returns 0, guarding
    // the polynomial multiply against an infinite loop on misuse.
    assert_eq!(crc32_combine_op(0xDEAD_BEEF, 0x1234_5678, 0), 0);
}

#[test]
fn crc32_combine_empty_identity() {
    // Combining with an empty second part (crc32(0, b"") == 0, len2 == 0) is the
    // identity: it returns the first CRC unchanged.
    let crc = crc32(0, b"some leading data");
    assert_eq!(crc32_combine(crc, crc32(0, b""), 0), crc);
}

#[test]
fn crc32_combine_two_part_pinned() {
    // "1234" ++ "56789" recombines to the canonical check value.
    let left = crc32(0, b"1234");
    let right = crc32(0, b"56789");
    assert_eq!(crc32_combine(left, right, 5), 0xCBF4_3926);
}

#[test]
fn crc32_combine_three_way() {
    // Associativity: combining three parts left-to-right equals the whole.
    let whole = crc32(0, b"abcdefghij");
    let prefix = crc32_combine(crc32(0, b"abc"), crc32(0, b"def"), 3);
    let all = crc32_combine(prefix, crc32(0, b"ghij"), 4);
    assert_eq!(all, whole);
}

// ===========================================================================
// Adler-32 known-answer tests (modulo BASE = 65521, NMAX = 5552)
// ===========================================================================

#[test]
fn adler32_empty_seed1() {
    // The C initial seed for Adler-32 is the literal 1; an empty update keeps 1.
    assert_eq!(adler32(1, b""), 1);
}

#[test]
fn adler32_empty_seed0_quirk() {
    // EMPTY-SLICE QUIRK: adler32 returns its `adler` argument unchanged for an
    // empty buffer, so a zero seed stays 0 — the function does NOT inject the
    // initial 1. This documents that the *initial value* must be 1 (callers and
    // the stream engine pass 1), not that the function itself supplies it.
    assert_eq!(adler32(0, b""), 0);
    assert_eq!(adler32(1, b""), 1);
}

#[test]
fn adler32_seed_passthrough_empty() {
    // For any already-reduced (valid) seed, an empty buffer returns it intact.
    let seed = adler32(1, b"prior data");
    assert_eq!(adler32(seed, b""), seed);
}

#[test]
fn adler32_check_value() {
    assert_eq!(adler32(1, b"123456789"), 0x091E_01DE);
}

#[test]
fn adler32_hello_hello() {
    // The "hello, hello!" fixture from test/example.c. Derived from the C
    // algorithm — low = (1 + sum(bytes)) mod 65521; high = (sum of running low)
    // mod 65521; adler = (high << 16) | low — and pinned against canonical zlib.
    assert_eq!(adler32(1, b"hello, hello!"), 0x2170_0496);
}

#[test]
fn adler32_wikipedia() {
    // Well-known Adler-32 test vector.
    assert_eq!(adler32(1, b"Wikipedia"), 0x11E6_0398);
}

#[test]
fn adler32_short_a() {
    assert_eq!(adler32(1, b"a"), 0x0062_0062);
}

#[test]
fn adler32_hello() {
    assert_eq!(adler32(1, b"hello"), 0x062C_0215);
}

#[test]
fn adler32_quick_brown_fox() {
    assert_eq!(
        adler32(1, b"The quick brown fox jumps over the lazy dog"),
        0x5BDC_0FDA
    );
}

#[test]
fn adler32_incremental_two_chunks() {
    // Streaming "hello, hello!" in two pieces equals the one-shot result.
    let (first, second) = b"hello, hello!".split_at(5);
    assert_eq!(adler32(adler32(1, first), second), 0x2170_0496);
}

#[test]
fn adler32_incremental_many_splits() {
    // Every two-chunk split of the check message reproduces the check value.
    let data = b"123456789";
    for k in 0..=data.len() {
        let (first, second) = data.split_at(k);
        assert_eq!(
            adler32(adler32(1, first), second),
            0x091E_01DE,
            "split at k={k}"
        );
    }
}

#[test]
fn adler32_incremental_single_bytes() {
    // Feeding one byte at a time equals the one-shot Adler-32.
    let mut adler = 1u32;
    for &byte in b"123456789" {
        adler = adler32(adler, &[byte]);
    }
    assert_eq!(adler, 0x091E_01DE);
}

#[test]
fn adler32_incremental_nmax_boundary() {
    // A 6000-byte buffer crosses the NMAX = 5552 block boundary, exercising the
    // per-block modulo reduction. Splitting anywhere must not change the result.
    let data = gen_buf(6000);
    let one_shot = adler32(1, &data);
    for k in [0usize, 1, 16, 3000, 5551, 5552, 5553, 6000] {
        let (first, second) = data.split_at(k);
        assert_eq!(
            adler32(adler32(1, first), second),
            one_shot,
            "split at k={k}"
        );
    }
}

#[test]
fn adler32_z_matches_adler32() {
    // The `_z` variant differs only in the C length type; results must match.
    let buffers: [&[u8]; 4] = [b"", b"a", b"123456789", b"Wikipedia"];
    for buf in buffers {
        assert_eq!(adler32_z(1, buf), adler32(1, buf));
    }
    let big = gen_buf(6000);
    assert_eq!(adler32_z(1, &big), adler32(1, &big));
}

#[test]
fn adler32_z_check_value() {
    assert_eq!(adler32_z(1, b"123456789"), 0x091E_01DE);
}

#[test]
fn adler32_large_buffer_pinned() {
    // Pinned against canonical C zlib; spans many NMAX blocks plus a remainder.
    assert_eq!(adler32(1, &gen_buf(6000)), 0x3B74_AB6E);
    assert_eq!(adler32(1, &gen_buf(50_000)), 0x0D33_4C88);
}

// ===========================================================================
// Adler-32 combine tests
// ===========================================================================

#[test]
fn adler32_combine_basic() {
    let data = b"123456789";
    assert_eq!(adler32(1, data), 0x091E_01DE);
    // For len <= 512, check_combine_adler exhaustively covers every split point.
    check_combine_adler(data);
}

#[test]
fn adler32_combine_large() {
    check_combine_adler(&gen_buf(6000));
}

#[test]
fn adler32_combine_nmax_boundary() {
    // gen_buf(40000) has len2 > BASE at many splits, exercising the `rem`
    // (len2 mod BASE) reduction inside the combine helper.
    check_combine_adler(&gen_buf(40_000));
}

#[test]
fn adler32_combine64_matches_combine() {
    // The 64-bit-length entry point must match the 32-bit one for in-range lens.
    let data = gen_buf(2048);
    for k in [0usize, 1, 1000, 2047, 2048] {
        let (a, b) = data.split_at(k);
        let adler_a = adler32(1, a);
        let adler_b = adler32(1, b);
        let len2 = b.len() as i64;
        assert_eq!(
            adler32_combine64(adler_a, adler_b, len2),
            adler32_combine(adler_a, adler_b, len2),
            "k={k}"
        );
    }
}

#[test]
fn adler32_combine_empty_identity() {
    // Appending an empty second buffer (adler32(1, b"") == 1, len2 == 0) is the
    // identity: it returns the first Adler-32 unchanged.
    let adler = adler32(1, b"some leading data");
    assert_eq!(adler32_combine(adler, adler32(1, b""), 0), adler);
}

#[test]
fn adler32_combine_three_way() {
    // Associativity: combining three parts left-to-right equals the whole.
    let whole = adler32(1, b"abcdefghij");
    let prefix = adler32_combine(adler32(1, b"abc"), adler32(1, b"def"), 3);
    let all = adler32_combine(prefix, adler32(1, b"ghij"), 4);
    assert_eq!(all, whole);
}

#[test]
fn adler32_combine_negative_len_consistency() {
    // Documented C quirk: in C, `adler32_combine` wraps a negative `len2` to a
    // huge unsigned value, so the result is well-defined-but-garbage. We
    // therefore do NOT pin any specific value for a negative length; we only
    // assert that the two length-width entry points agree (both delegate to one
    // helper) and that the call is total — it must not panic.
    let adler1 = 0x1234_5678;
    let adler2 = 0x9ABC_DEF0;
    assert_eq!(
        adler32_combine(adler1, adler2, -1),
        adler32_combine64(adler1, adler2, -1)
    );
}
