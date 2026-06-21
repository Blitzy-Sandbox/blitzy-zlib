//! Adler-32 checksum (RFC 1950), ported from zlib `adler32.c`; bit-for-bit
//! identical to C zlib.
//!
//! The Adler-32 checksum is a rolling checksum used by the zlib stream format
//! (RFC 1950) as the integrity trailer and is tracked in `strm.adler` while a
//! stream is processed. Because the value is part of the on-the-wire format,
//! this implementation must reproduce the canonical C zlib output **exactly**
//! for every input — any deviation would break stream compatibility.
//!
//! The checksum is a 32-bit value composed of two 16-bit running sums computed
//! modulo `BASE` (65521, the largest prime below 2¹⁶):
//!
//! * `s1` — the sum of every byte plus one, taken modulo `BASE`.
//! * `s2` — the sum of every intermediate `s1` value, taken modulo `BASE`.
//!
//! The final checksum is `(s2 << 16) | s1`. Seeding with `1` (so that `s1`
//! starts at 1 and `s2` at 0) begins a fresh checksum.
//!
//! # Design notes
//!
//! * **`no_std`-clean.** This module uses only [`core`] — no `std`, no `alloc`,
//!   and no heap allocation. It compiles unchanged under the crate's `no-std`
//!   feature.
//! * **Zero `unsafe`.** The entire engine is safe Rust: raw pointer arithmetic
//!   from the C original is replaced by slice iteration and slicing.
//! * **Endianness-independent.** Only integer arithmetic and bit shifts are
//!   used, so the result does not depend on the host byte order
//!   (matching the portable C implementation).

/// Largest prime smaller than 65536 — the Adler-32 modulus.
const BASE: u32 = 65521;

/// Largest `n` such that `255*n*(n+1)/2 + (n+1)*(BASE-1) <= 2^32-1`.
///
/// This bound guarantees the `u32` accumulators cannot overflow while summing a
/// single `NMAX`-byte block, so the (relatively expensive) modulo reduction is
/// only required once per block. `NMAX` is a multiple of 16, matching the C
/// implementation's 16-way unrolled inner loop.
const NMAX: usize = 5552;

/// Update a running Adler-32 checksum with the bytes of `buf` and return the
/// updated value.
///
/// This is the length-generic entry point (mirroring C `adler32_z`); the bytes
/// are taken from the slice directly, so the caller does not pass an explicit
/// length. [`adler32`] is a thin wrapper over this function.
///
/// To start a fresh checksum, seed `adler` with `1`. The function can be called
/// repeatedly to checksum a stream incrementally — feeding the return value
/// back in as `adler` yields the same result as a single call over the
/// concatenated input.
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::adler32::adler32_z;
/// // A fresh checksum is seeded with 1.
/// assert_eq!(adler32_z(1, b"123456789"), 0x091e_01de);
///
/// // Incremental updates match a single pass over the whole input.
/// let one_shot = adler32_z(1, b"123456789");
/// let incremental = adler32_z(adler32_z(1, b"1234"), b"56789");
/// assert_eq!(one_shot, incremental);
/// ```
#[must_use]
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Split the incoming checksum into its two component sums.
    let mut sum2 = (adler >> 16) & 0xffff;
    let mut adler = adler & 0xffff;

    // Fast path for callers that feed one byte at a time: a single add and a
    // conditional subtract for each sum avoids the modulo entirely.
    if buf.len() == 1 {
        adler += u32::from(buf[0]);
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 += adler;
        if sum2 >= BASE {
            sum2 -= BASE;
        }
        return adler | (sum2 << 16);
    }

    // Empty input. A Rust slice is never null, so this matches C's *non-NULL*
    // zero-length case, which falls through to the `len < 16` path and
    // normalizes the (unchanged) running sums before recombining: a single
    // conditional subtract for `adler` and a modulo for `sum2`. This is a no-op
    // for a well-formed seed (both 16-bit components already below `BASE`) and
    // reproduces canonical zlib exactly for a denormalized seed. The separate
    // C `buf == Z_NULL` short-circuit that returns 1 is a null-pointer concern
    // handled at the FFI boundary, where a null pointer is detectable.
    if buf.is_empty() {
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 %= BASE;
        return adler | (sum2 << 16);
    }

    // Somewhat-fast path for short inputs: accumulate every byte, then a single
    // conditional subtract suffices for `adler` (the running sum cannot reach
    // `2 * BASE`) while `sum2` needs a full modulo.
    if buf.len() < 16 {
        for &byte in buf {
            adler += u32::from(byte);
            sum2 += adler;
        }
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 %= BASE; // only so many BASEs were ever added (C: MOD28)
        return adler | (sum2 << 16);
    }

    // Bulk path: consume the input in `NMAX`-byte blocks, reducing modulo `BASE`
    // exactly once per block. `NMAX` is the largest block size for which the
    // `u32` accumulators provably cannot overflow, so the sums stay correct
    // without widening to `u64` (which would diverge from the C design).
    let mut remaining = buf;
    while remaining.len() >= NMAX {
        let (block, rest) = remaining.split_at(NMAX);
        remaining = rest;
        for &byte in block {
            adler += u32::from(byte);
            sum2 += adler;
        }
        adler %= BASE;
        sum2 %= BASE;
    }

    // Trailing bytes (fewer than `NMAX`): still just one modulo reduction.
    if !remaining.is_empty() {
        for &byte in remaining {
            adler += u32::from(byte);
            sum2 += adler;
        }
        adler %= BASE;
        sum2 %= BASE;
    }

    // Recombine the two sums into the 32-bit checksum.
    adler | (sum2 << 16)
}

/// Update a running Adler-32 checksum with the bytes of `buf` and return the
/// updated value.
///
/// Seed `adler` with `1` to begin a fresh checksum. This is the canonical
/// entry point and simply delegates to [`adler32_z`].
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::adler32::adler32;
/// // Seed with 1 for a fresh checksum.
/// let a = adler32(1, b"hello");
/// assert_eq!(a, adler32(1, b"hello"));
///
/// // The Adler-32 of the canonical test vector "123456789".
/// assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
/// ```
#[inline]
#[must_use]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

/// Combine two Adler-32 checksums into one.
///
/// Given `adler1` — the Adler-32 of a sequence `X` — and `adler2` — the
/// Adler-32 of a sequence `Y` of length `len2` — this returns the Adler-32 of
/// the concatenation `X` followed by `Y`, without rescanning either input. Both
/// `adler1` and `adler2` must have been seeded with `1`.
///
/// A negative `len2` is invalid and yields the sentinel `0xffff_ffff` (matching
/// C zlib), which serves as a debugging clue rather than a valid checksum.
fn adler32_combine_(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // For a negative length, return an invalid Adler-32 as a debugging clue.
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // rem = len2 mod BASE (the default C build uses a plain modulo here).
    let rem = (len2 % i64::from(BASE)) as u32;

    let mut sum1 = adler1 & 0xffff;
    // `rem * sum1` is computed in `u64` to mirror C's `unsigned long`, then
    // reduced modulo `BASE` before narrowing back to `u32`.
    let mut sum2 = ((u64::from(rem) * u64::from(sum1)) % u64::from(BASE)) as u32;

    sum1 += (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Reduce the recombined sums. The bounded ranges above guarantee at most
    // two subtractions for `sum1` and a `2*BASE` then `BASE` step for `sum2`.
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum2 >= (BASE << 1) {
        sum2 -= BASE << 1;
    }
    if sum2 >= BASE {
        sum2 -= BASE;
    }

    sum1 | (sum2 << 16)
}

/// Combine two Adler-32 checksums into one (32-bit length variant).
///
/// `adler1` is the checksum of the first sequence, `adler2` the checksum of the
/// second sequence, and `len2` the length of that second sequence. The result
/// is the Adler-32 of the two sequences concatenated. A negative `len2` returns
/// the sentinel `0xffff_ffff`.
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::adler32::{adler32, adler32_combine};
/// let part1 = b"hello, ";
/// let part2 = b"world";
/// let a1 = adler32(1, part1);
/// let a2 = adler32(1, part2);
/// let combined = adler32_combine(a1, a2, part2.len() as i64);
/// assert_eq!(combined, adler32(1, b"hello, world"));
/// ```
#[inline]
#[must_use]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

/// Combine two Adler-32 checksums into one (64-bit length variant).
///
/// Identical in behavior to [`adler32_combine`]; the separate name preserves
/// the zlib C API surface, where the 32-bit and 64-bit variants differ only in
/// the width of the length argument (Rust's `i64` already covers both).
///
/// # Examples
///
/// ```
/// # use zlib_rs::checksum::adler32::adler32_combine64;
/// // Combining with an empty second chunk leaves the first checksum unchanged.
/// assert_eq!(adler32_combine64(1, 1, 0), 1);
/// ```
#[inline]
#[must_use]
pub fn adler32_combine64(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a deterministic test buffer of `n` bytes.
    ///
    /// Mirrors the Python generator `bytes((i*131 + 7) & 0xff for i in
    /// range(n))` that produced the pinned known-answer values below, so the
    /// expected checksums can be regenerated with canonical C zlib at any time.
    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 131 + 7) & 0xff) as u8).collect()
    }

    #[test]
    fn empty_seed_one_is_identity() {
        // C zlib returns 1 for an empty/NULL buffer seeded with 1.
        assert_eq!(adler32(1, b""), 1);
        assert_eq!(adler32_z(1, b""), 1);
    }

    #[test]
    fn empty_preserves_well_formed_seed() {
        // For a well-formed seed (both 16-bit components < BASE), an empty
        // update returns the seed unchanged.
        for seed in [0u32, 1, 0x0001_2345, 0x1234_5678, 0x091e_01de] {
            assert_eq!(adler32(seed, b""), seed);
            assert_eq!(adler32_z(seed, b""), seed);
        }
    }

    #[test]
    fn empty_normalizes_denormalized_seed_like_c() {
        // For a denormalized seed, an empty update normalizes exactly as C zlib
        // does for a non-NULL zero-length buffer. Pinned against canonical zlib.
        assert_eq!(adler32(0xffff_ffff, b""), 0x000e_000e);
        assert_eq!(adler32(0xffff_0000, b""), 0x000e_0000);
        assert_eq!(adler32(0x0000_ffff, b""), 0x0000_000e);
        assert_eq!(adler32_z(0xffff_ffff, b""), 0x000e_000e);
    }

    #[test]
    fn known_answer_test_vector() {
        // Canonical zlib value for "123456789".
        assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
        assert_eq!(adler32_z(1, b"123456789"), 0x091e_01de);
    }

    #[test]
    fn short_string_known_answers() {
        // Pinned against Python's canonical C zlib.
        assert_eq!(adler32(1, b"hello"), 0x062c_0215);
        assert_eq!(adler32(1, b"world!!"), 0x0b5b_026b);
        assert_eq!(adler32(1, b"helloworld!!"), 0x2013_047f);
    }

    #[test]
    fn pinned_pattern_known_answers() {
        // (length, expected) pairs validated bit-for-bit against canonical
        // C zlib. The lengths exercise every code path: the single-byte fast
        // path, the `< 16` short path, the 16-byte boundary, the exact NMAX
        // block size and NMAX+1, two NMAX blocks (11104) and NMAX*2+1, and a
        // large multi-block buffer.
        const KATS: &[(usize, u32)] = &[
            (1, 0x0008_0008),
            (2, 0x009a_0092),
            (3, 0x0139_009f),
            (15, 0x25e7_0525),
            (16, 0x2bc0_05d9),
            (17, 0x31d0_0610),
            (5552, 0xf13f_cd5f),
            (5553, 0xbec4_cd76),
            (11104, 0xfe2e_99cc),
            (11105, 0x9830_99f3),
            (50000, 0x5058_4b68),
        ];
        for &(len, expected) in KATS {
            let data = pattern(len);
            assert_eq!(
                adler32(1, &data),
                expected,
                "adler32 mismatch for length {len}"
            );
            assert_eq!(
                adler32_z(1, &data),
                expected,
                "adler32_z mismatch for length {len}"
            );
        }
    }

    #[test]
    fn adler32_matches_adler32_z() {
        // The two entry points must always agree, across all length classes
        // and for a non-trivial seed.
        let data = pattern(12_000);
        for len in [
            0usize, 1, 2, 3, 15, 16, 17, 255, 5551, 5552, 5553, 11104, 11105, 12000,
        ] {
            let slice = &data[..len];
            assert_eq!(adler32(1, slice), adler32_z(1, slice), "len {len}");
            assert_eq!(
                adler32(0x091e_01de, slice),
                adler32_z(0x091e_01de, slice),
                "seeded len {len}"
            );
        }
    }

    #[test]
    fn continuation_equals_one_shot() {
        // Feeding the running checksum back in must equal a single pass over
        // the concatenated input, for split points spanning every code path.
        let data = pattern(12_000);
        let one_shot = adler32(1, &data);
        for k in [
            0usize, 1, 2, 15, 16, 17, 100, 5552, 5553, 6000, 11104, 12000,
        ] {
            let chained = adler32(adler32(1, &data[..k]), &data[k..]);
            assert_eq!(chained, one_shot, "continuation mismatch at split {k}");
        }
    }

    #[test]
    fn combine_equals_concatenation() {
        // adler32_combine(adler(X), adler(Y), len(Y)) == adler(X ++ Y).
        let cases: &[(usize, usize)] = &[
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (5, 7),
            (16, 16),
            (5000, 7000),
            (11105, 9),
        ];
        for &(la, lb) in cases {
            let a = pattern(la);
            // Offset the second buffer so it differs from the first.
            let b: Vec<u8> = (0..lb).map(|i| ((i * 197 + 41) & 0xff) as u8).collect();
            let mut concat = a.clone();
            concat.extend_from_slice(&b);

            let combined = adler32_combine(adler32(1, &a), adler32(1, &b), lb as i64);
            assert_eq!(
                combined,
                adler32(1, &concat),
                "combine mismatch ({la}, {lb})"
            );

            // The 64-bit variant must behave identically.
            let combined64 = adler32_combine64(adler32(1, &a), adler32(1, &b), lb as i64);
            assert_eq!(combined64, combined, "combine64 disagreed ({la}, {lb})");
        }
    }

    #[test]
    fn combine_concat_property_readable() {
        // A readable spelling of the same property for "hello" ++ "world!!".
        let combined = adler32_combine(adler32(1, b"hello"), adler32(1, b"world!!"), 7);
        assert_eq!(combined, adler32(1, b"helloworld!!"));
    }

    #[test]
    fn combine_pinned_known_answers() {
        // Raw combine values pinned against canonical C zlib output.
        assert_eq!(adler32_combine(0x062c_0215, 0x0b5b_026b, 7), 0x2013_047f);
        assert_eq!(adler32_combine(0xcb10_b954, 0xd289_9ec8, 7000), 0x3c33_582a);
        assert_eq!(adler32_combine(1, 0x0079_0079, 1), 0x0079_0079);
        assert_eq!(adler32_combine(0x0072_0072, 1, 0), 0x0072_0072);
    }

    #[test]
    fn combine_empty_second_chunk_is_noop() {
        // Combining with a zero-length second chunk returns the first checksum.
        for &x in &[b"".as_slice(), b"a", b"abc", b"123456789"] {
            let ax = adler32(1, x);
            assert_eq!(adler32_combine(ax, 1, 0), ax);
            assert_eq!(adler32_combine64(ax, 1, 0), ax);
        }
    }

    #[test]
    fn combine_negative_length_sentinel() {
        // A negative length is invalid and must return the debugging sentinel.
        assert_eq!(adler32_combine(123, 456, -1), 0xffff_ffff);
        assert_eq!(adler32_combine64(123, 456, -1), 0xffff_ffff);
        assert_eq!(adler32_combine(0, 0, i64::MIN), 0xffff_ffff);
    }

    #[test]
    fn combine_handles_large_lengths() {
        // Lengths beyond 32 bits must reduce modulo BASE without overflow and
        // still satisfy the concatenation property at a representative point.
        let a = pattern(1000);
        let b = pattern(2000);
        let mut concat = a.clone();
        concat.extend_from_slice(&b);
        let combined = adler32_combine64(adler32(1, &a), adler32(1, &b), b.len() as i64);
        assert_eq!(combined, adler32(1, &concat));

        // Exercise the modulo path with a huge (but valid) length argument: it
        // must not panic, and both 16-bit component sums of the result must be
        // valid residues modulo BASE.
        let huge = adler32_combine64(0x1234_5678, 0x9abc_def0, i64::MAX);
        assert!((huge & 0xffff) < BASE, "sum1 component must be < BASE");
        assert!((huge >> 16) < BASE, "sum2 component must be < BASE");
    }
}
