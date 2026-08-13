//! The `uLong` width of the combine family, compared against the C reference on this target.
//!
//! # Why this file exists separately from the other three
//!
//! `byte_identical.rs` compares emitted bytes, `roundtrip_interop.rs` compares whole streams and
//! `table_equality.rs` compares transcribed tables. None of them can see the defect this file
//! exists for, because it is not in a byte stream or a table: it is in the **width of an integer**.
//!
//! `adler32_combine` returns `uLong`, and `adler32.c` L133-L155 accumulates into `unsigned long`
//! throughout. On LP64 -- the only integer model this port is verified on -- that is 64 bits wide,
//! and for arguments that are not genuine Adler-32 checksums the reference genuinely uses 33 of
//! them: the internal `sum1` and `sum2` can each finish at `65_548`, so the packed word
//! `sum1 | (sum2 << 16)` overflows 32 bits and the two fields overlap. A port that computes the
//! packing in `u32` and widens the result answers with the low 32 bits, which is what a *32-bit*
//! `unsigned long` would answer and not what this target's C build does. Every stream-level and
//! table-level comparison in this crate passes either way, because a legitimate checksum never
//! reaches that region -- which is exactly why the divergence survived a green suite.
//!
//! So the comparison here is deliberately made **outside** the domain `zlib.h` L1837-L1846
//! documents, on the arguments where the two widths differ, against the reference build compiled
//! from the in-tree sources by this crate's `build.rs`. Both entry points are covered, because
//! `zlib.h` L2001 redefines `adler32_combine` to `adler32_combine64` for a caller compiled with
//! `_FILE_OFFSET_BITS == 64`, so which name a translation unit references is the caller's choice
//! and both must agree.
//!
//! The CRC-32 combines are compared too, on the same terms. They cannot exhibit the same defect --
//! `crc32.c`'s `crc32_combine_` returns `multmodp(...) ^ (crc2 & 0xffffffff)`, which is masked to
//! 32 bits by construction -- and comparing them is what turns that reasoning into a measurement
//! rather than an assertion.
//!
//! # How to run it
//!
//!     cargo test -p zlib-rs-differential --test checksum_width
//!
//! # Contract
//!
//! `#![forbid(unsafe_code)]`, like the other three files here: every oracle call goes through the
//! safe wrappers in `crates/zlib-rs-differential/src/oracle.rs`, which own the `extern "C"`
//! declarations and their obligations. Nothing in this file touches a pointer -- every argument and
//! every result is an integer.

// Assertions panic, which library code may not do; a test that cannot fail is not a test.
// `useless_conversion` is relaxed because this file's whole subject is the *width* of `uLong`:
// `u64::from(a_uLong)` is the identity on LP64, where the lint fires, and a genuine widening on
// i686 and LLP64 Windows, where it does not. Removing it would compile here and break there, and
// there is no spelling that suits both -- which is precisely the hazard the tests below measure.
#![allow(clippy::panic, clippy::unwrap_used, clippy::useless_conversion)]
#![forbid(unsafe_code)]

use libz_rs_sys::{adler32_combine, adler32_combine64, crc32_combine, crc32_combine64, uLong};
use zlib_rs_differential::oracle;

/// The value `adler32.c` L140 returns for a negative `len2`, written as C writes it.
const NEGATIVE_SENTINEL: u64 = 0xffff_ffff;

/// Widens a `uLong` to the width the comparison is stated in.
///
/// The two sides declare `uLong` through different paths -- `libz_rs_sys::uLong` and
/// `oracle::uLong` -- and both are `core::ffi::c_ulong`, so this is the identity on LP64 and a
/// widening on a 32-bit target. Comparing at 64 bits is what lets one expected value serve every
/// integer model: a 32-bit `unsigned long` build simply reports the low half, and the assertion
/// then compares the low half against the low half.
fn wide(value: uLong) -> u64 {
    u64::from(value)
}

/// The same, for the oracle's own alias.
fn wide_oracle(value: oracle::uLong) -> u64 {
    u64::from(value)
}

/// Every `(adler1, adler2)` pair the width comparison is made over.
///
/// The first block is in-domain -- both halves of each value below `BASE` -- and is what a
/// legitimate caller ever passes. The second is out of domain, and the last three entries of it are
/// the witnesses the port's own module documentation pins: both arguments carry a high half at or
/// above `BASE`, which is the only way the reference's `unsigned long` result can exceed 32 bits.
const CHECKSUM_PAIRS: &[(u32, u32)] = &[
    // In domain.
    (1, 1),
    (0x0001_0001, 0x0001_0001),
    (0x058c_0111, 0x02c1_0104),
    (0xffef_ffef, 0x0001_0001),
    (0x0001_0001, 0xffef_ffef),
    (0xffef_ffef, 0xffef_ffef),
    // Out of domain: at least one half at or above `BASE` (65_521 == 0xfff1).
    (0, 0),
    (0x0000_ffff, 0x0000_ffff),
    (0xffff_ffff, 0xffff_ffff),
    (0xfff0_fff0, 0xfff0_fff0),
    (0x1234_5678, 0x9abc_def0),
    // The three 33-bit witnesses.
    (0xffff_fff0, 0xffff_0000),
    (0xffff_fff0, 0xffff_ffff),
    (0xfffe_fff0, 0xffff_0000),
];

/// Every `len2` the width comparison is made over: the structural edges of the reduction, the
/// values that cross 32 bits, and the negative sentinel from both sides of zero.
const LENGTHS: &[i64] = &[
    0,
    1,
    2,
    65_520,
    65_521,
    65_522,
    131_041,
    131_042,
    131_043,
    1_000_000,
    1 << 31,
    1 << 32,
    1 << 40,
    i64::MAX,
    -1,
    -2,
    -65_521,
    i64::MIN,
];

/// `len2` as the narrow entry point's `z_off_t`, or [`None`] when it does not fit.
///
/// `z_off_t` is `c_long`: eight bytes on LP64, four on a 32-bit target. The wide vectors above are
/// deliberately larger than a 32-bit `off_t` can hold, so on such a target those cells are compared
/// through the `*64` face only -- which is the same restriction a C caller lives under.
fn as_off_t(len2: i64) -> Option<libz_rs_sys::z_off_t> {
    libz_rs_sys::z_off_t::try_from(len2).ok()
}

/// The same, for the oracle's alias.
fn as_off_t_oracle(len2: i64) -> Option<oracle::z_off_t> {
    oracle::z_off_t::try_from(len2).ok()
}

/// ★ **M-06.** `adler32_combine` and `adler32_combine64` agree with the C reference at full
/// `uLong` width, including on the arguments where that width has 33 significant bits.
#[test]
fn adler32_combine_agrees_with_the_reference_at_full_ulong_width() {
    let mut wide_results = 0_usize;

    for &(adler1, adler2) in CHECKSUM_PAIRS {
        for &len2 in LENGTHS {
            let port = wide(adler32_combine64(
                uLong::from(adler1),
                uLong::from(adler2),
                len2,
            ));
            let reference = wide_oracle(oracle::adler32_combine64(
                oracle::uLong::from(adler1),
                oracle::uLong::from(adler2),
                len2,
            ));
            assert_eq!(
                port, reference,
                "adler32_combine64({adler1:#010x}, {adler2:#010x}, {len2}): \
                 port {port:#x}, reference {reference:#x}"
            );
            if port > 0xffff_ffff {
                wide_results += 1;
            }

            // The narrow face, wherever `z_off_t` can carry the same length.
            if let (Some(narrow), Some(narrow_oracle)) = (as_off_t(len2), as_off_t_oracle(len2)) {
                let port_narrow = wide(adler32_combine(
                    uLong::from(adler1),
                    uLong::from(adler2),
                    narrow,
                ));
                let reference_narrow = wide_oracle(oracle::adler32_combine(
                    oracle::uLong::from(adler1),
                    oracle::uLong::from(adler2),
                    narrow_oracle,
                ));
                assert_eq!(
                    port_narrow, reference_narrow,
                    "adler32_combine({adler1:#010x}, {adler2:#010x}, {len2})"
                );
                assert_eq!(
                    port_narrow, port,
                    "the two faces of the same call must not disagree \
                     ({adler1:#010x}, {adler2:#010x}, {len2})"
                );
            }
        }
    }

    // The comparison is only evidence if it actually visited the region where the widths differ.
    // On a target whose `unsigned long` is 32 bits wide there is no such region, and the reference
    // itself reports the truncated value, so the count is legitimately zero there.
    if u64::from(uLong::MAX) > 0xffff_ffff {
        assert!(
            wide_results > 0,
            "no vector produced a result above 32 bits, so this test proved nothing about width"
        );
    }
}

/// The negative-length sentinel is the same value on both sides and at both faces.
#[test]
fn a_negative_length_yields_the_reference_sentinel() {
    for &len2 in &[-1_i64, -2, -65_521, i64::MIN] {
        let port = wide(adler32_combine64(1, 1, len2));
        let reference = wide_oracle(oracle::adler32_combine64(1, 1, len2));
        assert_eq!(port, NEGATIVE_SENTINEL, "len2 = {len2}");
        assert_eq!(reference, NEGATIVE_SENTINEL, "len2 = {len2}");
    }
}

/// The CRC-32 combines agree at full `uLong` width too.
///
/// They cannot show the Adler defect, because `crc32_combine_` masks its result to 32 bits, but
/// measuring that is what makes it a fact rather than a reading of the source.
#[test]
fn crc32_combine_agrees_with_the_reference_at_full_ulong_width() {
    for &(crc1, crc2) in CHECKSUM_PAIRS {
        for &len2 in LENGTHS {
            let port = wide(crc32_combine64(uLong::from(crc1), uLong::from(crc2), len2));
            let reference = wide_oracle(oracle::crc32_combine64(
                oracle::uLong::from(crc1),
                oracle::uLong::from(crc2),
                len2,
            ));
            assert_eq!(
                port, reference,
                "crc32_combine64({crc1:#010x}, {crc2:#010x}, {len2})"
            );
            assert!(
                port <= 0xffff_ffff,
                "a CRC-32 combine must never exceed 32 bits ({crc1:#010x}, {crc2:#010x}, {len2})"
            );

            if let (Some(narrow), Some(narrow_oracle)) = (as_off_t(len2), as_off_t_oracle(len2)) {
                let port_narrow = wide(crc32_combine(uLong::from(crc1), uLong::from(crc2), narrow));
                let reference_narrow = wide_oracle(oracle::crc32_combine(
                    oracle::uLong::from(crc1),
                    oracle::uLong::from(crc2),
                    narrow_oracle,
                ));
                assert_eq!(
                    port_narrow, reference_narrow,
                    "crc32_combine({crc1:#010x}, {crc2:#010x}, {len2})"
                );
            }
        }
    }
}

/// Bits at or above position 32 of the two checksum arguments are ignored, on both sides.
///
/// `adler32.c` L145-L149 reads only `adler & 0xffff` and `(adler >> 16) & 0xffff` from each
/// argument, so a `uLong` carrying anything above bit 31 must produce the same answer as the same
/// value with those bits cleared. That is what makes the facade's narrowing of `uLong` to the
/// core's `u32` faithful rather than merely convenient, and it is only observable on a target whose
/// `uLong` is wider than 32 bits.
#[test]
fn high_ulong_bits_are_ignored_by_both_implementations() {
    if u64::from(uLong::MAX) <= 0xffff_ffff {
        // A 32-bit `unsigned long` has no such bits; there is nothing to compare.
        return;
    }
    let high = uLong::try_from(0xdead_beef_0000_0000_u64).expect("uLong is 64 bits here");
    let high_oracle =
        oracle::uLong::try_from(0xdead_beef_0000_0000_u64).expect("uLong is 64 bits here");

    for &(adler1, adler2) in CHECKSUM_PAIRS {
        for &len2 in &[0_i64, 1, 65_521, 1 << 32] {
            let plain = wide(adler32_combine64(
                uLong::from(adler1),
                uLong::from(adler2),
                len2,
            ));
            let dirtied = wide(adler32_combine64(
                uLong::from(adler1) | high,
                uLong::from(adler2) | high,
                len2,
            ));
            let reference = wide_oracle(oracle::adler32_combine64(
                oracle::uLong::from(adler1) | high_oracle,
                oracle::uLong::from(adler2) | high_oracle,
                len2,
            ));
            assert_eq!(
                dirtied, plain,
                "high bits changed the port's answer ({adler1:#010x}, {adler2:#010x}, {len2})"
            );
            assert_eq!(
                dirtied, reference,
                "high bits disagree with the reference ({adler1:#010x}, {adler2:#010x}, {len2})"
            );
        }
    }
}
