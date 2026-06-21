//! Adler-32 / CRC-32 known-answer (KAT) and `*_combine` integration tests.
//!
//! This is a Cargo *integration test* crate: it links the `zlib-rs` library
//! through its public surface (`use zlib_rs::...`) exactly as an external
//! consumer would, and exercises the checksum engine that backs the zlib
//! (RFC 1950, Adler-32) and gzip (RFC 1952, CRC-32) stream trailers.
//!
//! Conceptually sourced from the checksum usage in `test/example.c` (the
//! `hello, hello!` fixture and the Adler-32 dictionary id), but expressed here
//! as direct known-answer tests so the engine can be validated without first
//! standing up the full compression pipeline.
//!
//! # Why these values are frozen
//!
//! Both checksums travel on the wire as part of the container formats, so any
//! deviation from canonical C zlib — even by a single bit — would render a
//! stream unreadable by other conformant decoders. Every literal asserted
//! below is therefore a **frozen wire-format contract value** (AAP §0.7.1),
//! cross-checked against reference C zlib. The constants must not be "fixed up"
//! to match the implementation; if a value here ever disagrees with the code,
//! the code is wrong.
//!
//! # Coverage map
//!
//! * CRC-32: the canonical `0xCBF4_3926` check value, empty/single-byte/short
//!   inputs, the `_z` length-explicit variant, incremental == one-shot
//!   equivalence, the `build.rs`-generated lookup table, and the full
//!   `crc32_combine` / `crc32_combine64` / `crc32_combine_gen` /
//!   `crc32_combine_op` family.
//! * Adler-32: the `0x091E_01DE` check value, the `hello, hello!` and
//!   `Wikipedia` vectors, the documented empty-slice quirk (seed `0` stays `0`,
//!   seed `1` stays `1`), the `NMAX = 5552` modulo-reduction boundary, the `_z`
//!   variant, and the `adler32_combine` / `adler32_combine64` family.
//!
//! Run with: `cargo test --test checksum`.

// The crate-root re-exports cover the eight canonical entry points; the
// combine-operator helpers and the table accessor are reached through their
// module path (they are intentionally not lifted to the crate root). Every
// symbol imported here is verified to exist in the materialized `src/`.
use zlib_rs::checksum::crc32::{
    crc32_combine_gen, crc32_combine_gen64, crc32_combine_op, get_crc_table,
};
use zlib_rs::{
    adler32, adler32_combine, adler32_combine64, adler32_z, crc32, crc32_combine, crc32_combine64,
    crc32_z,
};

// ===========================================================================
// Private helpers (kept in-file per the "no shared helper module" constraint)
// ===========================================================================

/// The reflected IEEE CRC-32 polynomial (`0xEDB8_8320`).
///
/// Defined locally — *not* imported from the crate — so the lookup-table test
/// can regenerate the 256-entry table from first principles and confirm the
/// `build.rs`-generated table the library exposes matches an independent
/// computation.
const POLY: u32 = 0xEDB8_8320;

/// Build a deterministic pseudo-random test buffer of `n` bytes.
///
/// Uses the affine generator `byte[i] = (i * 131 + 7) mod 256`, the same
/// recipe the in-crate unit tests use, so the pinned known-answer checksums
/// for these buffers can be regenerated with canonical C zlib at any time. The
/// sizes chosen by callers straddle the Adler-32 `NMAX = 5552` block boundary
/// to exercise the bulk modulo-reduction path.
fn pattern(n: usize) -> Vec<u8> {
    // `as u8` keeps the low 8 bits, exactly matching the reference `& 0xff`.
    (0..n).map(|i| (i * 131 + 7) as u8).collect()
}

/// Assert the CRC-32 split-and-combine identity at a single split point `k`.
///
/// Splits `data` into `data[..k]` and `data[k..]`, then verifies that every
/// member of the combine family reconstructs the one-shot CRC of the whole
/// buffer:
///
/// * [`crc32_combine`] (the canonical entry point),
/// * [`crc32_combine64`] (the 64-bit-length variant), and
/// * the precomputed-operator path [`crc32_combine_gen`] + [`crc32_combine_op`].
fn assert_crc_combine_at(data: &[u8], k: usize) {
    let whole = crc32(0, data);
    let (a, b) = data.split_at(k);
    let crc_a = crc32(0, a);
    let crc_b = crc32(0, b);
    let len2 = b.len() as i64;

    assert_eq!(
        crc32_combine(crc_a, crc_b, len2),
        whole,
        "crc32_combine mismatch at split {k} (len {})",
        data.len()
    );
    assert_eq!(
        crc32_combine64(crc_a, crc_b, len2),
        whole,
        "crc32_combine64 mismatch at split {k} (len {})",
        data.len()
    );

    let op = crc32_combine_gen(len2);
    assert_eq!(
        crc32_combine_op(crc_a, crc_b, op),
        whole,
        "crc32_combine_gen+op mismatch at split {k} (len {})",
        data.len()
    );
}

/// Verify the CRC-32 combine identity across *every* split point of `data`.
///
/// Intended for small buffers (the work is `O(len^2)`); larger buffers use
/// [`assert_crc_combine_at`] at strategic split points instead.
fn check_combine_crc(data: &[u8]) {
    for k in 0..=data.len() {
        assert_crc_combine_at(data, k);
    }
}

/// Assert the Adler-32 split-and-combine identity at a single split point `k`.
///
/// Both halves are seeded with `1` (the canonical Adler-32 seed); the combined
/// result must equal the one-shot Adler-32 of the whole buffer for both
/// [`adler32_combine`] and its 64-bit-length sibling [`adler32_combine64`].
fn assert_adler_combine_at(data: &[u8], k: usize) {
    let whole = adler32(1, data);
    let (a, b) = data.split_at(k);
    let adler_a = adler32(1, a);
    let adler_b = adler32(1, b);
    let len2 = b.len() as i64;

    assert_eq!(
        adler32_combine(adler_a, adler_b, len2),
        whole,
        "adler32_combine mismatch at split {k} (len {})",
        data.len()
    );
    assert_eq!(
        adler32_combine64(adler_a, adler_b, len2),
        whole,
        "adler32_combine64 mismatch at split {k} (len {})",
        data.len()
    );
}

/// Verify the Adler-32 combine identity across *every* split point of `data`.
///
/// Intended for small buffers; larger buffers use [`assert_adler_combine_at`]
/// at strategic split points instead.
fn check_combine_adler(data: &[u8]) {
    for k in 0..=data.len() {
        assert_adler_combine_at(data, k);
    }
}

// ===========================================================================
// CRC-32 — known-answer tests (POLY = 0xEDB8_8320, IEEE reflected)
// ===========================================================================

/// The canonical CRC-32 check value: `crc32(0, "123456789") == 0xCBF4_3926`.
#[test]
fn crc32_check_value() {
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
}

/// An empty buffer with seed `0` leaves the CRC at `0`.
#[test]
fn crc32_empty() {
    assert_eq!(crc32(0, b""), 0);
}

/// The CRC-32 of a single zero byte is the well-known `0xD202_EF8D`.
#[test]
fn crc32_single_zero_byte() {
    assert_eq!(crc32(0, &[0u8]), 0xD202_EF8D);
}

/// The classic pangram check value.
#[test]
fn crc32_quick_brown_fox() {
    assert_eq!(
        crc32(0, b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
}

/// Single-character input `"a"`.
#[test]
fn crc32_short_a() {
    assert_eq!(crc32(0, b"a"), 0xE8B7_BE43);
}

/// Three-character input `"abc"`.
#[test]
fn crc32_short_abc() {
    assert_eq!(crc32(0, b"abc"), 0x3524_41C2);
}

/// The string `"Wikipedia"` (a CRC-32 companion to the Adler-32 vector).
#[test]
fn crc32_wikipedia() {
    assert_eq!(crc32(0, b"Wikipedia"), 0xADAA_C02E);
}

/// `crc32_z` produces the canonical check value (it differs from `crc32` only
/// in the C `z_size_t` length type; the Rust signature is identical).
#[test]
fn crc32_z_check_value() {
    assert_eq!(crc32_z(0, b"123456789"), 0xCBF4_3926);
}

/// `crc32_z` of an empty buffer is `0`.
#[test]
fn crc32_z_empty() {
    assert_eq!(crc32_z(0, b""), 0);
}

/// `crc32_z` agrees with `crc32` for a spread of buffers.
#[test]
fn crc32_z_matches_crc32() {
    let cases: &[&[u8]] = &[
        b"",
        b"a",
        b"abc",
        b"123456789",
        b"The quick brown fox jumps over the lazy dog",
    ];
    for &buf in cases {
        assert_eq!(
            crc32_z(0, buf),
            crc32(0, buf),
            "crc32_z != crc32 for {buf:?}"
        );
    }
    let big = pattern(6000);
    assert_eq!(crc32_z(0, &big), crc32(0, &big));
}

/// Streaming a checksum in two chunks equals the single-call result.
#[test]
fn crc32_incremental_two_chunks() {
    let (head, tail) = b"123456789".split_at(4);
    assert_eq!(crc32(crc32(0, head), tail), 0xCBF4_3926);
}

/// Incremental update equals one-shot across every split point.
#[test]
fn crc32_incremental_many_splits() {
    let data = b"123456789";
    for k in 0..=data.len() {
        let (head, tail) = data.split_at(k);
        assert_eq!(
            crc32(crc32(0, head), tail),
            0xCBF4_3926,
            "incremental crc32 mismatch at split {k}"
        );
    }
}

/// Feeding one byte at a time reproduces the one-shot CRC of a larger buffer.
#[test]
fn crc32_incremental_byte_by_byte() {
    let data = pattern(512);
    let one_shot = crc32(0, &data);
    let folded = data.iter().fold(0u32, |crc, &b| crc32(crc, &[b]));
    assert_eq!(folded, one_shot);
}

/// Pinned CRC-32 values for the deterministic pattern buffer, straddling the
/// Adler-32 `NMAX` boundary (the CRC engine has no such boundary, but reusing
/// the same buffers keeps the two algorithms' fixtures aligned).
#[test]
fn crc32_large_pattern_values() {
    assert_eq!(crc32(0, &pattern(5552)), 0x29CB_ED01);
    assert_eq!(crc32(0, &pattern(5553)), 0x26FF_9131);
    assert_eq!(crc32(0, &pattern(6000)), 0x1B2A_D98F);
    assert_eq!(crc32(0, &pattern(10000)), 0x29DB_AF90);
}

/// The byte-wise lookup table the crate exposes matches the well-known entries.
#[test]
fn crc32_table_known_entries() {
    let table = get_crc_table();
    assert_eq!(table[0], 0x0000_0000);
    assert_eq!(table[1], 0x7707_3096);
    assert_eq!(table[2], 0xEE0E_612C);
    assert_eq!(table[3], 0x9909_51BA);
    assert_eq!(table[255], 0x2D02_EF8D);
}

/// The exposed table equals an independent regeneration from `POLY`, proving
/// all 256 entries (not just the spot-checked ones) are correct.
#[test]
fn crc32_table_full_regenerated() {
    let table = get_crc_table();
    for (n, &entry) in table.iter().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
        }
        assert_eq!(
            entry, c,
            "crc table entry {n} disagrees with POLY regeneration"
        );
    }
}

// ===========================================================================
// CRC-32 — combine family (split-and-recombine identities)
// ===========================================================================

/// Combining the CRCs of `"123456789"` split at *every* point reconstructs the
/// canonical `0xCBF4_3926` (the helper also checks `combine64` and `gen`+`op`).
#[test]
fn crc32_combine_check_value_all_splits() {
    let data = b"123456789";
    check_combine_crc(data);
    // The reconstructed value is the canonical check value at every split.
    for k in 0..=data.len() {
        let (a, b) = data.split_at(k);
        assert_eq!(
            crc32_combine(crc32(0, a), crc32(0, b), b.len() as i64),
            0xCBF4_3926
        );
    }
}

/// The combine identity holds across every split point of a larger buffer.
#[test]
fn crc32_combine_large() {
    check_combine_crc(&pattern(1000));
}

/// The combine identity holds across every split point of the pangram.
#[test]
fn crc32_combine_quick_brown_fox() {
    check_combine_crc(b"The quick brown fox jumps over the lazy dog");
}

/// `crc32_combine64` matches `crc32_combine` for in-range lengths.
#[test]
fn crc32_combine64_matches_combine() {
    let data = pattern(777);
    for &k in &[0usize, 1, 100, 388, 776, 777] {
        let (a, b) = data.split_at(k);
        let crc_a = crc32(0, a);
        let crc_b = crc32(0, b);
        let len2 = b.len() as i64;
        assert_eq!(
            crc32_combine64(crc_a, crc_b, len2),
            crc32_combine(crc_a, crc_b, len2),
            "combine64 != combine at split {k}"
        );
    }
}

/// The precomputed-operator path (`crc32_combine_gen` + `crc32_combine_op`)
/// matches the direct `crc32_combine`.
#[test]
fn crc32_combine_gen_op_matches_direct() {
    let data = pattern(513);
    for &k in &[0usize, 1, 64, 256, 512, 513] {
        let (a, b) = data.split_at(k);
        let crc_a = crc32(0, a);
        let crc_b = crc32(0, b);
        let len2 = b.len() as i64;
        let op = crc32_combine_gen(len2);
        assert_eq!(
            crc32_combine_op(crc_a, crc_b, op),
            crc32_combine(crc_a, crc_b, len2),
            "gen+op != direct combine at split {k}"
        );
    }
}

/// `crc32_combine_gen` (32-bit length) equals `crc32_combine_gen64` for the
/// same in-range lengths.
#[test]
fn crc32_combine_gen_matches_gen64() {
    for len2 in [0i64, 1, 9, 256, 5552, 65_535, 1_000_000] {
        assert_eq!(
            crc32_combine_gen(len2),
            crc32_combine_gen64(len2),
            "gen != gen64 for len2 {len2}"
        );
    }
}

/// Combining with an empty second part is the identity:
/// `crc32_combine(c, crc32(0, ""), 0) == c`.
#[test]
fn crc32_combine_empty_second_identity() {
    for seed in [b"" as &[u8], b"a", b"123456789", b"The quick brown fox"] {
        let c = crc32(0, seed);
        assert_eq!(
            crc32_combine(c, crc32(0, b""), 0),
            c,
            "empty-tail identity for {seed:?}"
        );
    }
}

/// `crc32_combine_op` returns `0` for the `op == 0` guard value (misuse guard;
/// a valid operator from `crc32_combine_gen*` is never `0`).
#[test]
fn crc32_combine_op_zero_guard() {
    assert_eq!(crc32_combine_op(0xDEAD_BEEF, 0x1234_5678, 0), 0);
}

/// A negative `len2` is invalid for the operator generator, which yields `0`.
#[test]
fn crc32_combine_gen_negative_returns_zero() {
    assert_eq!(crc32_combine_gen(-1), 0);
    assert_eq!(crc32_combine_gen64(-1), 0);
}

// ===========================================================================
// Adler-32 — known-answer tests (BASE = 65521, NMAX = 5552)
// ===========================================================================

/// The canonical Adler-32 seed is the literal `1`, and an empty update keeps
/// it `1`.
#[test]
fn adler32_empty_seed1() {
    assert_eq!(adler32(1, b""), 1);
}

/// The canonical Adler-32 check value of `"123456789"`.
#[test]
fn adler32_check_value() {
    assert_eq!(adler32(1, b"123456789"), 0x091E_01DE);
}

/// The `hello, hello!` fixture from `test/example.c`. zlib streams seed the
/// running Adler-32 with `1`, so this is `adler32(1, ...)` over the bare bytes.
#[test]
fn adler32_hello_fixture() {
    assert_eq!(adler32(1, b"hello, hello!"), 0x2170_0496);
}

/// The well-known `"Wikipedia"` Adler-32 test vector.
#[test]
fn adler32_wikipedia() {
    assert_eq!(adler32(1, b"Wikipedia"), 0x11E6_0398);
}

/// The Adler-32 of the `test/example.c` dictionary `"hello"` — the `dictId`
/// value that stream-level preset-dictionary handling compares against.
#[test]
fn adler32_dictionary_hello() {
    assert_eq!(adler32(1, b"hello"), 0x062C_0215);
}

/// Single-character input `"a"`.
#[test]
fn adler32_short_a() {
    assert_eq!(adler32(1, b"a"), 0x0062_0062);
}

/// Three-character input `"abc"`.
#[test]
fn adler32_short_abc() {
    assert_eq!(adler32(1, b"abc"), 0x024D_0127);
}

/// The pangram, as an Adler-32 companion to the CRC-32 vector.
#[test]
fn adler32_quick_brown_fox() {
    assert_eq!(
        adler32(1, b"The quick brown fox jumps over the lazy dog"),
        0x5BDC_0FDA
    );
}

/// ★ Empty-slice quirk: the function returns its `adler` argument unchanged on
/// an empty buffer, so seed `0` stays `0` while seed `1` stays `1`. This
/// documents that the *initial value* must be `1` (callers/engine pass `1`) —
/// the function does not inject `1` for the empty case the way the C
/// null-pointer short-circuit does.
#[test]
fn adler32_empty_seed0_quirk() {
    assert_eq!(adler32(0, b""), 0, "seed 0 over empty must stay 0");
    assert_eq!(adler32(1, b""), 1, "seed 1 over empty must stay 1");
}

/// `adler32_z` produces the canonical check value (it differs from `adler32`
/// only in the C length type; the Rust signature is identical).
#[test]
fn adler32_z_check_value() {
    assert_eq!(adler32_z(1, b"123456789"), 0x091E_01DE);
}

/// `adler32_z` of an empty buffer with seed `1` stays `1`.
#[test]
fn adler32_z_empty_seed1() {
    assert_eq!(adler32_z(1, b""), 1);
}

/// `adler32_z` agrees with `adler32` for a spread of buffers, including one
/// that crosses the `NMAX` block boundary.
#[test]
fn adler32_z_matches_adler32() {
    let cases: &[&[u8]] = &[b"", b"a", b"abc", b"123456789", b"hello, hello!"];
    for &buf in cases {
        assert_eq!(
            adler32_z(1, buf),
            adler32(1, buf),
            "adler32_z != adler32 for {buf:?}"
        );
    }
    let big = pattern(6000);
    assert_eq!(adler32_z(1, &big), adler32(1, &big));
}

/// Streaming an Adler-32 in two chunks equals the single-call result.
#[test]
fn adler32_incremental_two_chunks() {
    let (head, tail) = b"123456789".split_at(4);
    assert_eq!(adler32(adler32(1, head), tail), 0x091E_01DE);
}

/// Incremental update equals one-shot across every split point.
#[test]
fn adler32_incremental_many_splits() {
    let data = b"hello, hello!";
    let one_shot = adler32(1, data);
    for k in 0..=data.len() {
        let (head, tail) = data.split_at(k);
        assert_eq!(
            adler32(adler32(1, head), tail),
            one_shot,
            "incremental adler32 mismatch at split {k}"
        );
    }
}

/// Feeding one byte at a time reproduces the one-shot Adler-32.
#[test]
fn adler32_incremental_byte_by_byte() {
    let data = pattern(300);
    let one_shot = adler32(1, &data);
    let folded = data.iter().fold(1u32, |adler, &b| adler32(adler, &[b]));
    assert_eq!(folded, one_shot);
}

/// Incremental update equals one-shot when the chunk boundary crosses the
/// `NMAX = 5552` modulo-reduction boundary (the bulk path). The `>= 6000`-byte
/// buffer guarantees at least one full `NMAX` block is reduced.
#[test]
fn adler32_incremental_nmax_boundary() {
    let data = pattern(6000);
    let one_shot = adler32(1, &data);
    for &k in &[0usize, 1, 5551, 5552, 5553, 3000, 6000] {
        let (head, tail) = data.split_at(k);
        assert_eq!(
            adler32(adler32(1, head), tail),
            one_shot,
            "incremental adler32 mismatch at NMAX-spanning split {k}"
        );
    }
}

/// Pinned Adler-32 values for the deterministic pattern buffer at and around
/// the `NMAX` boundary, exercising the bulk modulo-reduction path.
#[test]
fn adler32_large_pattern_values() {
    assert_eq!(adler32(1, &pattern(5552)), 0xF13F_CD5F);
    assert_eq!(adler32(1, &pattern(5553)), 0xBEC4_CD76);
    assert_eq!(adler32(1, &pattern(6000)), 0x8C35_AB0E);
    assert_eq!(adler32(1, &pattern(10000)), 0x4476_7376);
}

// ===========================================================================
// Adler-32 — combine family (split-and-recombine identities)
// ===========================================================================

/// Combining the Adler-32s of `"123456789"` split at *every* point
/// reconstructs the canonical `0x091E_01DE` value (the helper also checks
/// `adler32_combine64`).
#[test]
fn adler32_combine_check_value_all_splits() {
    let data = b"123456789";
    check_combine_adler(data);
    for k in 0..=data.len() {
        let (a, b) = data.split_at(k);
        assert_eq!(
            adler32_combine(adler32(1, a), adler32(1, b), b.len() as i64),
            0x091E_01DE
        );
    }
}

/// The combine identity holds across every split point of a larger buffer.
#[test]
fn adler32_combine_large() {
    check_combine_adler(&pattern(1000));
}

/// The combine identity holds when the second part's length crosses the
/// `NMAX = 5552` boundary (the operator uses `len2 mod BASE`, so a length
/// beyond `NMAX` — and beyond `BASE` — must still reconstruct correctly).
#[test]
fn adler32_combine_nmax_boundary() {
    let data = pattern(6000);
    for &k in &[0usize, 1, 447, 448, 3000, 5999, 6000] {
        assert_adler_combine_at(&data, k);
    }
}

/// `adler32_combine64` matches `adler32_combine` for in-range lengths.
#[test]
fn adler32_combine64_matches_combine() {
    let data = pattern(777);
    for &k in &[0usize, 1, 100, 388, 776, 777] {
        let (a, b) = data.split_at(k);
        let adler_a = adler32(1, a);
        let adler_b = adler32(1, b);
        let len2 = b.len() as i64;
        assert_eq!(
            adler32_combine64(adler_a, adler_b, len2),
            adler32_combine(adler_a, adler_b, len2),
            "combine64 != combine at split {k}"
        );
    }
}

/// Combining with an empty second part is the identity:
/// `adler32_combine(a, adler32(1, ""), 0) == a` for any well-formed `a`.
#[test]
fn adler32_combine_empty_second_identity() {
    for seed in [b"a" as &[u8], b"123456789", b"hello, hello!", b"Wikipedia"] {
        let a = adler32(1, seed);
        assert_eq!(
            adler32_combine(a, adler32(1, b""), 0),
            a,
            "empty-tail identity for {seed:?}"
        );
    }
}

/// Documented C quirk: a *negative* `len2` is invalid. C zlib wraps it to a
/// huge unsigned value (a well-defined-but-meaningless result); this crate
/// returns a sentinel instead. Either way the value is not part of the frozen
/// contract, so we deliberately do **not** pin a specific result for negative
/// lengths — we assert only that the call is total (never panics) and
/// deterministic. Only the non-negative identities above are contractual.
#[test]
fn adler32_combine_negative_len_deterministic() {
    let a1 = adler32(1, b"alpha");
    let a2 = adler32(1, b"beta");
    assert_eq!(
        adler32_combine(a1, a2, -1),
        adler32_combine(a1, a2, -1),
        "negative-len combine must be deterministic"
    );
    assert_eq!(
        adler32_combine64(a1, a2, -1),
        adler32_combine64(a1, a2, -1),
        "negative-len combine64 must be deterministic"
    );
}
