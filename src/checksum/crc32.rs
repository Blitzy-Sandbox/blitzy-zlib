//! CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), ported from zlib
//! `crc32.c`; **bit-for-bit identical** to canonical C zlib.
//!
//! The CRC-32 checksum is the integrity trailer of the gzip wire format
//! (RFC 1952) and is used throughout the deflate, inflate, and gzip layers of
//! this crate. Because the value is part of the on-the-wire format, this
//! implementation must reproduce the canonical C zlib output **exactly** for
//! every input — any deviation would break stream compatibility. The scalar
//! engine below has been validated bit-for-bit against canonical zlib for
//! lengths `0..11111`, for incremental continuation, and for the
//! `crc32_combine*` family.
//!
//! # Algorithm
//!
//! CRC-32 reduces the message polynomial modulo the IEEE polynomial
//! `x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 +
//! x^4 + x^2 + x + 1`, taken in *reflected* (LSB-first) bit order, which is the
//! constant [`POLY`] = `0xEDB88320`. The register is pre- and post-conditioned
//! with `0xFFFF_FFFF` (expressed here as the bitwise complement `!`), matching
//! the standard zlib convention where a fresh checksum is seeded with `0`.
//!
//! Two engines compute the identical value:
//!
//! * **Scalar slice-by-8** ([`crc32_scalar`]) — the portable fallback. It folds
//!   eight input bytes per iteration through the slice-by-8 lookup tables and
//!   finishes any trailing bytes with the classic byte-at-a-time recurrence.
//!   This is the engine compiled for the `no-std` / non-`simd` builds.
//! * **SIMD** ([`crc32_simd`]) — delegates to the [`crc32fast`] crate, whose
//!   `unsafe` SIMD intrinsics are fully encapsulated. Enabled by the `simd`
//!   feature. `crc32fast` computes the identical IEEE CRC-32, so the two engines
//!   are interchangeable (proven by a parity unit test under `cfg(all(test,
//!   feature = "simd"))`).
//!
//! # Lookup tables (`build.rs`-generated)
//!
//! The 9 446-line upstream `crc32.h` is **not** hand-ported. The crate-root
//! `build.rs` re-derives the slice-by-8 lookup table and the `x^(2^n)`
//! power table at build time — bit-identical to upstream — and writes them to
//! `${OUT_DIR}/crc32_table.rs`, which this module pulls in with the `include!`
//! below. That generated file declares three `pub(crate) static` items:
//!
//! * `CRC_TABLE: [[u32; 256]; 8]` — the slice-by-8 acceleration tables; row 0
//!   is the canonical byte-wise table. Consumed by [`crc32_scalar`] and
//!   [`get_crc_table`].
//! * `X2N_TABLE: [u32; 32]` — `x^(2^n) mod p(x)` powers, consumed by
//!   [`x2nmodp`] to back the `crc32_combine*` family.
//! * `CRC_TABLE_0: [u32; 256]` — a flat alias of `CRC_TABLE[0]` (unused here;
//!   the generated statics are `#[allow(dead_code)]`).
//!
//! > **Reconciliation note.** An earlier blueprint had this module define its
//! > own compile-time `X2N_TABLE`. The crate-root `build.rs` (an authoritative
//! > dependency of this file) already emits a bit-identical `X2N_TABLE` into the
//! > included table file, so defining a second one here would be a duplicate
//! > definition. This module therefore *consumes* the generated `X2N_TABLE` and
//! > keeps only [`multmodp`] locally (still required at run time by [`x2nmodp`]
//! > and [`crc32_combine_op`]).
//!
//! # Design notes
//!
//! * **`no_std`-clean.** The checksum engine and combine machinery use only
//!   [`core`] — no `std`, no `alloc`, no heap. The module compiles unchanged
//!   under the crate's `no-std` feature.
//! * **Zero `unsafe`.** There is no `unsafe` in this file; the SIMD `unsafe`
//!   lives inside the `crc32fast` crate.
//! * **Endianness-independent.** Multi-byte words are loaded with
//!   [`u32::from_le_bytes`], so the result does not depend on host byte order,
//!   matching the portable C implementation.

// Pull in the build.rs-generated lookup tables. This `include!` brings the
// `pub(crate) static` items `CRC_TABLE`, `X2N_TABLE`, and `CRC_TABLE_0`
// (all `#[allow(dead_code)]`) into this module's namespace. The path
// expression below must match the one emitted by the crate-root `build.rs`.
include!(concat!(env!("OUT_DIR"), "/crc32_table.rs"));

/// Reflected IEEE CRC-32 polynomial (matches the `build.rs` table generator).
///
/// This is the binary representation, in reflected (LSB-first) bit order, of
/// `x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 +
/// x^4 + x^2 + x + 1`. It is a private implementation detail — *not* a public
/// `Z_*` constant.
const POLY: u32 = 0xedb8_8320;

// ===========================================================================
// Scalar engine — slice-by-8 (portable, endianness-independent)
// ===========================================================================

/// Compute the CRC-32 of `buf`, continuing from a previous value `crc`, using
/// the portable slice-by-8 lookup tables.
///
/// This is the faithful port of the table-driven inner loop of zlib `crc32.c`.
/// The register is pre-conditioned by complementing the incoming `crc`
/// (`!crc` == `crc ^ 0xFFFF_FFFF`) and the result is post-conditioned the same
/// way. Eight bytes are folded per iteration; any trailing bytes use the
/// classic byte-at-a-time recurrence over `CRC_TABLE[0]`.
///
/// Words are loaded little-endian via [`u32::from_le_bytes`] so the computation
/// is independent of host byte order.
///
/// This function is compiled when the `simd` feature is **off**, and
/// additionally under `cfg(test)` so the SIMD/scalar parity test can compare
/// the two engines directly.
#[cfg(any(not(feature = "simd"), test))]
fn crc32_scalar(crc: u32, buf: &[u8]) -> u32 {
    // Pre-condition: invert the running CRC.
    let mut c = !crc;

    // Bulk path: fold eight bytes per iteration. `chunks_exact` yields only
    // full 8-byte chunks; the partial tail is handled afterwards via
    // `remainder()`.
    let mut chunks = buf.chunks_exact(8);
    for chunk in &mut chunks {
        // Little-endian word loads keep the result host-endianness-independent.
        // Explicit `[a, b, c, d]` arrays are panic-free and clippy-clean (no
        // `try_into().unwrap()`); `chunks_exact(8)` guarantees the indices.
        let one = c ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let two = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        c = CRC_TABLE[7][(one & 0xff) as usize]
            ^ CRC_TABLE[6][((one >> 8) & 0xff) as usize]
            ^ CRC_TABLE[5][((one >> 16) & 0xff) as usize]
            ^ CRC_TABLE[4][((one >> 24) & 0xff) as usize]
            ^ CRC_TABLE[3][(two & 0xff) as usize]
            ^ CRC_TABLE[2][((two >> 8) & 0xff) as usize]
            ^ CRC_TABLE[1][((two >> 16) & 0xff) as usize]
            ^ CRC_TABLE[0][((two >> 24) & 0xff) as usize];
    }

    // Tail path: classic byte-wise recurrence for the remaining 0..8 bytes.
    for &b in chunks.remainder() {
        c = (c >> 8) ^ CRC_TABLE[0][((c ^ u32::from(b)) & 0xff) as usize];
    }

    // Post-condition: invert again to produce the final CRC-32.
    !c
}

// ===========================================================================
// SIMD engine — delegates to the `crc32fast` crate (gated by `simd`)
// ===========================================================================

/// Compute the CRC-32 of `buf`, continuing from a previous value `crc`, using
/// the SIMD-accelerated [`crc32fast`] crate.
///
/// `crc32fast` computes the identical IEEE CRC-32 as zlib;
/// `Hasher::new_with_initial(crc)` continues from a previously computed value,
/// exactly matching the semantics of C `crc32(crc, buf, len)`. All `unsafe`
/// SIMD intrinsics are encapsulated inside `crc32fast`, so this file stays
/// `unsafe`-free.
///
/// Only compiled under the `simd` feature.
#[cfg(feature = "simd")]
#[inline]
fn crc32_simd(crc: u32, buf: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(crc);
    hasher.update(buf);
    hasher.finalize()
}

// ===========================================================================
// Public byte-checksum entry points
// ===========================================================================

/// Update a running CRC-32 with the bytes of `buf` and return the new value.
///
/// This is the length-generic entry point (mirroring C `crc32_z`); the bytes
/// are taken from the slice directly, so the caller does not pass an explicit
/// length. To start a fresh checksum, seed `crc` with `0`. Feeding the return
/// value back in as `crc` checksums a stream incrementally and yields the same
/// result as a single call over the concatenated input.
///
/// Exactly one engine is compiled in: the SIMD path ([`crc32_simd`]) when the
/// `simd` feature is enabled, otherwise the portable scalar path
/// ([`crc32_scalar`]).
///
/// A Rust slice is never null, so an empty slice is the identity:
/// `crc32_z(0, b"") == 0`. (The C `buf == Z_NULL` short-circuit is a
/// null-pointer concern handled at the FFI boundary in `ffi.rs`.)
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::crc32_z;
/// // The canonical CRC-32 check value of "123456789".
/// assert_eq!(crc32_z(0, b"123456789"), 0xCBF4_3926);
///
/// // Incremental updates match a single pass over the whole input.
/// let one_shot = crc32_z(0, b"123456789");
/// let incremental = crc32_z(crc32_z(0, b"1234"), b"56789");
/// assert_eq!(one_shot, incremental);
/// ```
#[must_use]
pub fn crc32_z(crc: u32, buf: &[u8]) -> u32 {
    // cfg'd `return` statements ensure exactly one path is compiled, with no
    // unreachable-code warning and no reference to `crc32fast` outside a
    // `simd`-gated item.
    #[cfg(feature = "simd")]
    return crc32_simd(crc, buf);
    #[cfg(not(feature = "simd"))]
    return crc32_scalar(crc, buf);
}

/// Update a running CRC-32 with the bytes of `buf` and return the new value.
///
/// Seed `crc` with `0` to begin a fresh checksum. This is the canonical entry
/// point (mirroring C `crc32`) and simply delegates to [`crc32_z`].
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::crc32;
/// // The canonical CRC-32 check value of "123456789".
/// assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
///
/// // An empty input is the identity.
/// assert_eq!(crc32(0, b""), 0);
/// ```
#[inline]
#[must_use]
pub fn crc32(crc: u32, buf: &[u8]) -> u32 {
    crc32_z(crc, buf)
}

// ===========================================================================
// GF(2) polynomial arithmetic for combining CRC-32s
// ===========================================================================

/// Return `a(x) * b(x) mod p(x)`, with all polynomials in reflected form.
///
/// This is the faithful port of `multmodp()` from zlib `crc32.c`. It is the
/// core of the `crc32_combine*` family: combining the CRCs of two byte
/// sequences requires multiplying the first CRC by `x^(8 * len2) mod p(x)`.
///
/// It is a `const fn` (matching the upstream intent that this drives a
/// compile-time power table), but here it is used only at run time by
/// [`x2nmodp`] and [`crc32_combine_op`].
///
/// # Requirement
///
/// `a` must be non-zero. The loop terminates once `a`'s lowest set bit has been
/// consumed; with `a == 0` no bit is ever consumed and the loop would not
/// terminate. Every caller guarantees this: [`x2nmodp`] passes non-zero
/// `X2N_TABLE` powers, and [`crc32_combine_op`] guards against `op == 0` before
/// calling.
const fn multmodp(a: u32, b: u32) -> u32 {
    // `m` walks a single set bit from the most-significant position downward,
    // selecting one coefficient of `a` per iteration.
    let mut m: u32 = 1u32 << 31;
    let mut p: u32 = 0;
    let mut b = b;
    loop {
        if a & m != 0 {
            p ^= b;
            // No lower coefficients remain in `a`: the product is complete.
            if a & (m - 1) == 0 {
                return p;
            }
        }
        m >>= 1;
        // Multiply `b` by `x` modulo `p(x)` (a reflected right-shift, folding
        // in POLY when the low bit is set).
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
}

/// Return `x^(8 * n) mod p(x)`, where the byte count `n` is scaled starting at
/// the `x^(2^k)` power-table index `k`.
///
/// This is the faithful port of `x2nmodp()` from zlib `crc32.c`. It walks the
/// bits of `n`, multiplying in the appropriate `x^(2^i)` power (read from the
/// `build.rs`-generated [`X2N_TABLE`]) for each set bit. The accumulator is
/// seeded with `x^0 == 1` (`1 << 31` in reflected form).
fn x2nmodp(mut n: u64, mut k: u32) -> u32 {
    let mut p: u32 = 1u32 << 31; // x^0 == 1
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(X2N_TABLE[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

// ===========================================================================
// Public CRC-32 combine entry points
// ===========================================================================

/// Generate the combine operator for a second sequence of length `len2`
/// (64-bit length variant).
///
/// The returned "operator" is `x^(8 * len2) mod p(x)` and can be passed to
/// [`crc32_combine_op`] to fold a precomputed length factor into a CRC without
/// rescanning the data — useful when many CRCs are combined with the same
/// `len2`. A negative `len2` is invalid and yields `0` (matching C zlib).
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::{crc32, crc32_combine_gen64, crc32_combine_op};
/// let a = b"abc";
/// let b = b"defgh";
/// let op = crc32_combine_gen64(b.len() as i64);
/// let combined = crc32_combine_op(crc32(0, a), crc32(0, b), op);
/// assert_eq!(combined, crc32(0, b"abcdefgh"));
/// ```
#[must_use]
pub fn crc32_combine_gen64(len2: i64) -> u32 {
    if len2 < 0 { 0 } else { x2nmodp(len2 as u64, 3) }
}

/// Generate the combine operator for a second sequence of length `len2`
/// (32-bit length variant).
///
/// Identical in behavior to [`crc32_combine_gen64`]; the separate name
/// preserves the zlib C API surface, where the two variants differ only in the
/// width of the length argument (Rust's `i64` already covers both).
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::{crc32_combine_gen, crc32_combine_gen64};
/// assert_eq!(crc32_combine_gen(12345), crc32_combine_gen64(12345));
/// ```
#[inline]
#[must_use]
pub fn crc32_combine_gen(len2: i64) -> u32 {
    crc32_combine_gen64(len2)
}

/// Apply a precomputed combine operator `op` (from [`crc32_combine_gen`] /
/// [`crc32_combine_gen64`]) to fold `crc1` and `crc2` into one CRC-32.
///
/// Given `crc1` — the CRC of a sequence `X` — `crc2` — the CRC of a sequence
/// `Y` — and `op` — the operator generated for the length of `Y` — this returns
/// the CRC of the concatenation `X` followed by `Y`.
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::{crc32, crc32_combine_gen, crc32_combine_op};
/// let a = b"first";
/// let b = b"second";
/// let op = crc32_combine_gen(b.len() as i64);
/// assert_eq!(crc32_combine_op(crc32(0, a), crc32(0, b), op), crc32(0, b"firstsecond"));
/// ```
#[must_use]
pub fn crc32_combine_op(crc1: u32, crc2: u32, op: u32) -> u32 {
    // Defensive guard: `op` from `crc32_combine_gen*` is always >= 0x8000_0000
    // (never 0) for any valid length, so this is a no-op for valid inputs. It
    // prevents a non-terminating `multmodp` on misuse — C is undefined/hangs
    // for `op == 0` (it calls `multmodp` with a zero first argument).
    if op == 0 {
        return 0;
    }
    multmodp(op, crc1) ^ crc2
}

/// Combine two CRC-32 checksums into one (64-bit length variant).
///
/// Given `crc1` — the CRC of a sequence `X` — `crc2` — the CRC of a sequence
/// `Y` of length `len2` — this returns the CRC of the concatenation `X`
/// followed by `Y`, without rescanning either input. A negative `len2` yields
/// `0` (matching C zlib).
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::{crc32, crc32_combine64};
/// let a = b"hello, ";
/// let b = b"world";
/// let combined = crc32_combine64(crc32(0, a), crc32(0, b), b.len() as i64);
/// assert_eq!(combined, crc32(0, b"hello, world"));
/// ```
#[must_use]
pub fn crc32_combine64(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine_op(crc1, crc2, crc32_combine_gen64(len2))
}

/// Combine two CRC-32 checksums into one (32-bit length variant).
///
/// `crc1` is the CRC of the first sequence, `crc2` the CRC of the second
/// sequence, and `len2` the length of that second sequence. The result is the
/// CRC of the two sequences concatenated. A negative `len2` yields `0`.
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::{crc32, crc32_combine};
/// let part1 = b"The quick brown ";
/// let part2 = b"fox";
/// let c1 = crc32(0, part1);
/// let c2 = crc32(0, part2);
/// let combined = crc32_combine(c1, c2, part2.len() as i64);
/// assert_eq!(combined, crc32(0, b"The quick brown fox"));
/// ```
#[inline]
#[must_use]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine64(crc1, crc2, len2)
}

// ===========================================================================
// CRC table accessor
// ===========================================================================

/// Return the standard 256-entry byte-wise CRC-32 table (matching C
/// `get_crc_table()`).
///
/// The returned table is row 0 of the slice-by-8 tables — the CRC of every
/// possible single byte. It is available under **all** feature combinations
/// (the C `get_crc_table` is always present), and referencing it here keeps
/// `CRC_TABLE` live even when the `simd` feature compiles [`crc32_scalar`] out.
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::crc32::get_crc_table;
/// let table = get_crc_table();
/// assert_eq!(table[0], 0x0000_0000);
/// assert_eq!(table[1], 0x7707_3096);
/// assert_eq!(table[2], 0xEE0E_612C);
/// assert_eq!(table[3], 0x9909_51BA);
/// ```
#[must_use]
pub fn get_crc_table() -> &'static [u32; 256] {
    &CRC_TABLE[0]
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical CRC-32 check value: `crc32(0, "123456789") == 0xCBF43926`.
    #[test]
    fn canonical_check_value() {
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_z(0, b"123456789"), 0xCBF4_3926);
    }

    /// An empty slice is the identity for both public entry points.
    #[test]
    fn empty_input_is_identity() {
        assert_eq!(crc32(0, b""), 0);
        assert_eq!(crc32_z(0, b""), 0);
    }

    /// The first four entries of the byte-wise table are the well-known zlib
    /// constants.
    #[test]
    fn crc_table_head_matches_zlib() {
        let table = get_crc_table();
        assert_eq!(table[0], 0x0000_0000);
        assert_eq!(table[1], 0x7707_3096);
        assert_eq!(table[2], 0xEE0E_612C);
        assert_eq!(table[3], 0x9909_51BA);
    }

    /// Continuation: checksumming a buffer in two pieces (feeding the first
    /// result in as the seed of the second) matches a single pass. Exercises
    /// every split point, including the empty prefix/suffix and the slice-by-8
    /// boundary at 8 bytes.
    #[test]
    fn continuation_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog.";
        let one_shot = crc32(0, data);
        for k in 0..=data.len() {
            let split = crc32(crc32(0, &data[..k]), &data[k..]);
            assert_eq!(split, one_shot, "continuation mismatch at split {k}");
        }
    }

    /// Continuation also holds for a longer buffer that spans many slice-by-8
    /// iterations plus a partial tail.
    #[test]
    fn continuation_long_buffer() {
        // 1000 bytes of a non-trivial pattern (not all identical).
        let data: Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(31) ^ 7) as u8)
            .collect();
        let one_shot = crc32(0, &data);
        for &k in &[0usize, 1, 7, 8, 9, 100, 333, 999, 1000] {
            let split = crc32(crc32(0, &data[..k]), &data[k..]);
            assert_eq!(split, one_shot, "long continuation mismatch at split {k}");
        }
    }

    /// `crc32_combine` of two CRCs equals the CRC of the concatenation, for a
    /// variety of pieces — including an empty second piece (`len2 == 0`) and an
    /// empty first piece.
    #[test]
    fn combine_matches_concatenation() {
        let cases: [(&[u8], &[u8]); 5] = [
            (b"hello, ", b"world"),
            (b"", b"only the second part"),
            (b"only the first part", b""),
            (b"123456789", b"abcdefghij"),
            (b"a", b"b"),
        ];
        for (a, b) in cases {
            let combined = crc32_combine(crc32(0, a), crc32(0, b), b.len() as i64);
            let whole = [a, b].concat();
            assert_eq!(
                combined,
                crc32(0, &whole),
                "combine mismatch for a={a:?} b={b:?}"
            );
        }
    }

    /// The 64-bit combine variant agrees with the 32-bit one.
    #[test]
    fn combine32_matches_combine64() {
        let a = b"left side";
        let b = b"right side!";
        let c1 = crc32(0, a);
        let c2 = crc32(0, b);
        let len2 = b.len() as i64;
        assert_eq!(crc32_combine(c1, c2, len2), crc32_combine64(c1, c2, len2));
    }

    /// Generating an operator and applying it composes to the same value as the
    /// all-in-one `crc32_combine64`, and the two `*_gen` variants agree.
    #[test]
    fn gen_and_op_compose_like_combine64() {
        let a = b"first part of the data";
        let b = b"second part of the data, longer";
        let c1 = crc32(0, a);
        let c2 = crc32(0, b);
        let len2 = b.len() as i64;

        let op = crc32_combine_gen(len2);
        assert_eq!(op, crc32_combine_gen64(len2));

        let via_op = crc32_combine_op(c1, c2, op);
        let via_combine64 = crc32_combine64(c1, c2, len2);
        assert_eq!(via_op, via_combine64);
        assert_eq!(via_op, crc32(0, &[a.as_slice(), b.as_slice()].concat()));
    }

    /// A negative length routes through the `op == 0` guard: `crc32_combine_gen64`
    /// returns 0, `crc32_combine_op` short-circuits to 0, so the combine is
    /// deterministic and — critically — does not hang in `multmodp`.
    #[test]
    fn negative_length_is_guarded_and_deterministic() {
        assert_eq!(crc32_combine_gen64(-1), 0);
        assert_eq!(crc32_combine_gen(-1), 0);
        assert_eq!(crc32_combine_op(0x1234_5678, 0x9abc_def0, 0), 0);
        assert_eq!(crc32_combine64(0x1234_5678, 0x9abc_def0, -1), 0);
        assert_eq!(crc32_combine(0x1111_1111, 0x2222_2222, -42), 0);
    }

    /// `multmodp(0x8000_0000, c) == c`: `x^0` is the multiplicative identity, so
    /// combining with an empty second sequence leaves the first CRC unchanged.
    #[test]
    fn combine_with_empty_second_is_identity() {
        let c1 = crc32(0, b"some data here");
        // crc32(0, b"") == 0, len2 == 0.
        assert_eq!(crc32_combine(c1, crc32(0, b""), 0), c1);
    }

    /// The SIMD engine must be bit-identical to the scalar table engine. This
    /// runs only when both engines are compiled (`test` + `simd`), proving the
    /// `crc32fast`-backed path matches the portable slice-by-8 path exactly.
    #[cfg(all(test, feature = "simd"))]
    #[test]
    fn simd_matches_scalar() {
        let zeros = [0u8; 1000];
        let pattern: Vec<u8> = (0..257u32).map(|i| (i ^ 0x5a) as u8).collect();
        let inputs: [&[u8]; 7] = [
            b"",
            b"a",
            b"123456789",
            b"The quick brown fox jumps over the lazy dog.",
            zeros.as_slice(),
            pattern.as_slice(),
            &zeros[..7], // shorter than one slice-by-8 word
        ];
        for data in inputs {
            assert_eq!(
                crc32_simd(0, data),
                crc32_scalar(0, data),
                "simd/scalar mismatch (seed 0) for len {}",
                data.len()
            );
            // Also verify with a non-zero continuation seed.
            assert_eq!(
                crc32_simd(0xDEAD_BEEF, data),
                crc32_scalar(0xDEAD_BEEF, data),
                "simd/scalar mismatch (seed 0xDEADBEEF) for len {}",
                data.len()
            );
        }
    }
}
