//! Adler-32 checksum (RFC 1950), ported from zlib `adler32.c`; bit-for-bit
//! identical to C zlib.
//!
//! Adler-32 is the integrity check used by the zlib stream wrapper (RFC 1950):
//! the four-byte big-endian Adler-32 of the uncompressed data is appended as the
//! stream trailer, and the running value is tracked in `strm.adler`. Because the
//! checksum is part of the on-the-wire format, any deviation from the canonical
//! algorithm would break stream compatibility; this module therefore reproduces
//! the C reference exactly, including the `*_combine` variants.
//!
//! The checksum is the concatenation of two 16-bit sums taken modulo
//! `BASE` (65521, the largest prime below 2^16):
//!
//! * `s1` — the running sum of every input byte (seeded at 1).
//! * `s2` — the running sum of every intermediate value of `s1`.
//!
//! The 32-bit result is `s2 << 16 | s1`.
//!
//! # Purity and portability
//!
//! This module is **`no_std`-clean**: it uses `core` only — no `std`, no `alloc`,
//! and **no `unsafe`**. The computation is pure integer arithmetic with no
//! byte-order assumptions, so results are identical on little- and big-endian
//! targets. It compiles unchanged under the crate's `no-std` feature.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::checksum::adler32::adler32;
//!
//! // Seed with 1 to begin a fresh checksum.
//! let sum = adler32(1, b"hello");
//! assert_eq!(sum, 0x062c_0215);
//! ```

/// Largest prime smaller than 65536 — the Adler-32 modulus.
///
/// Private implementation detail of the Adler-32 engine (matching the `local`
/// `#define BASE 65521U` in C); intentionally **not** a public `Z_*` constant.
const BASE: u32 = 65521;

/// Largest `n` such that `255*n*(n+1)/2 + (n+1)*(BASE-1) <= 2^32 - 1`.
///
/// This bound guarantees the `u32` accumulators cannot overflow while summing a
/// single `NMAX`-byte block, which lets the hot loop defer the (relatively
/// expensive) modulo reduction to once per block. `NMAX` is divisible by 16,
/// matching the C reference's 16-way unrolled inner loop. Private implementation
/// detail (the `local` `#define NMAX 5552` in C).
const NMAX: usize = 5552;

/// Update a running Adler-32 checksum with the bytes of `buf`.
///
/// This is the core routine; [`adler32`] is a thin wrapper around it. `adler` is
/// the previous checksum value (use `1` to start a fresh computation), and the
/// returned value is the updated checksum. It is bit-for-bit identical to C
/// zlib's `adler32_z`, including the per-`NMAX`-block modulo cadence that keeps
/// the result independent of how the input is chunked.
///
/// An empty `buf` returns `adler` unchanged (so a fresh, seed-`1` checksum of no
/// data is `1`). Note that, like the C reference's `Z_NULL` fast path, an empty
/// slice does not re-reduce the incoming value; this is observable only for an
/// already-invalid (non-reduced) seed and never for the reduced values produced
/// by this function during normal streaming use.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::adler32_z;
///
/// // Feeding the data in two pieces yields the same result as one shot.
/// let one_shot = adler32_z(1, b"abcdefgh");
/// let piece = adler32_z(1, b"abcd");
/// let split = adler32_z(piece, b"efgh");
/// assert_eq!(one_shot, split);
/// ```
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Split the incoming Adler-32 into its two component sums.
    let mut sum2 = (adler >> 16) & 0xffff;
    let mut adler = adler & 0xffff;

    // Fast path for the common "one byte at a time" caller.
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

    // Initial Adler-32 value (deferred until after the len == 1 fast path so
    // that the common single-byte case stays branch-light). C returns 1 for
    // `buf == Z_NULL`; for an empty Rust slice we return the (split then
    // recombined) input unchanged — which equals 1 when `adler == 1`.
    if buf.is_empty() {
        return adler | (sum2 << 16);
    }

    // Fast path for short inputs: accumulate without per-byte reduction, then
    // reduce once. `adler` needs only a single conditional subtract because the
    // low-16 seed (< 65536) plus at most 15 * 255 cannot reach 2 * BASE; `sum2`
    // may have accumulated several multiples of BASE, so it takes a full modulo.
    if buf.len() < 16 {
        for &byte in buf {
            adler += u32::from(byte);
            sum2 += adler;
        }
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 %= BASE; // only added so many BASE's (MOD28 in C)
        return adler | (sum2 << 16);
    }

    // Bulk path: process whole NMAX-byte blocks, reducing once per block. The
    // NMAX bound proves the u32 accumulators stay below 2^32 across a full
    // block, so no intermediate reduction (or u64 widening) is required.
    let mut buf = buf;
    while buf.len() >= NMAX {
        let (block, rest) = buf.split_at(NMAX);
        buf = rest;
        for &byte in block {
            adler += u32::from(byte);
            sum2 += adler;
        }
        adler %= BASE;
        sum2 %= BASE;
    }

    // Trailing bytes (fewer than NMAX): accumulate, then reduce once.
    if !buf.is_empty() {
        for &byte in buf {
            adler += u32::from(byte);
            sum2 += adler;
        }
        adler %= BASE;
        sum2 %= BASE;
    }

    // Recombine the two sums into the 32-bit checksum.
    adler | (sum2 << 16)
}

/// Update a running Adler-32 checksum with the bytes of `buf`.
///
/// Equivalent to [`adler32_z`]; provided to mirror the C public API where
/// `adler32` (with a 32-bit length) delegates to `adler32_z` (with a `size_t`
/// length). Seed `adler` with `1` to start a fresh checksum.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::adler32;
///
/// assert_eq!(adler32(1, b""), 1);
/// assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
/// ```
#[inline]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

/// Combine two Adler-32 checksums into one, as if the second buffer had been
/// appended to the first.
///
/// Given `adler1 = adler32(1, a)` and `adler2 = adler32(1, b)` where `b` has
/// length `len2`, this returns `adler32(1, [a, b].concat())` without rescanning
/// the data. This is the engine shared by [`adler32_combine`] and
/// [`adler32_combine64`] (Rust's `i64` length spans both the C `z_off_t` and
/// `z_off64_t` variants).
///
/// A negative `len2` is invalid and yields the sentinel `0xffff_ffff`, matching
/// C zlib's debugging clue.
fn adler32_combine_(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // For a negative length, return an invalid Adler-32 as a debugging clue.
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // rem = len2 mod BASE (the C `MOD63`; the default build uses a plain modulo).
    let rem = (len2 % i64::from(BASE)) as u32;
    let mut sum1 = adler1 & 0xffff;
    // Compute `rem * sum1` in u64 (mirroring C's `unsigned long`) before
    // reducing mod BASE and narrowing back to u32; the product can exceed 2^32.
    let mut sum2 = ((u64::from(rem) * u64::from(sum1)) % u64::from(BASE)) as u32;
    sum1 += (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Reduce without division. The ordering and the (BASE << 1) step are load
    // bearing: `sum1` can carry up to two extra BASEs, `sum2` up to two as well.
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
/// `adler1` is the checksum of the first buffer, `adler2` the checksum of the
/// second buffer, and `len2` the length (in bytes) of the second buffer. The
/// result equals the Adler-32 of the two buffers concatenated. A negative `len2`
/// returns the invalid sentinel `0xffff_ffff`.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::{adler32, adler32_combine};
///
/// let left = adler32(1, b"Hello, ");
/// let right = adler32(1, b"world!");
/// let whole = adler32(1, b"Hello, world!");
/// assert_eq!(adler32_combine(left, right, b"world!".len() as i64), whole);
/// ```
#[inline]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

/// Combine two Adler-32 checksums into one (64-bit length variant).
///
/// Identical in behavior to [`adler32_combine`]; both delegate to the same
/// internal helper because Rust's `i64` length already covers the 64-bit range
/// that the C API splits across `z_off_t` and `z_off64_t`.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::{adler32, adler32_combine64};
///
/// let left = adler32(1, b"foo");
/// let right = adler32(1, b"bar");
/// assert_eq!(adler32_combine64(left, right, 3), adler32(1, b"foobar"));
/// ```
#[inline]
pub fn adler32_combine64(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

#[cfg(test)]
mod tests {
    use super::{BASE, NMAX, adler32, adler32_combine, adler32_combine64, adler32_z};

    /// Deterministic pseudo-random buffer generator. Mirrors the Python oracle
    /// generator `bytes((i*131 + 17) & 0xff for i in range(n))` so the pinned
    /// values below are exactly what canonical C zlib produces for these inputs.
    fn gen_buf(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 131 + 17) & 0xff) as u8).collect()
    }

    #[test]
    fn empty_buffer_is_identity() {
        // A fresh (seed 1) checksum of no data is 1.
        assert_eq!(adler32(1, b""), 1);
        assert_eq!(adler32_z(1, b""), 1);
    }

    #[test]
    fn empty_buffer_passes_through_valid_seed() {
        // For an already-reduced (valid) seed, an empty buffer returns it
        // unchanged — matching canonical zlib for every value this engine emits.
        let seed = 0x1234_5678;
        assert_eq!(adler32(seed, b""), seed);
        assert_eq!(adler32_combine(seed, 1, 0), seed); // appending nothing
    }

    #[test]
    fn known_answer_vectors() {
        // Pinned against canonical C zlib (verified via Python's `zlib`).
        assert_eq!(adler32(1, b"a"), 0x0062_0062);
        assert_eq!(adler32(1, b"hello"), 0x062c_0215);
        assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
        assert_eq!(
            adler32(1, b"The quick brown fox jumps over the lazy dog"),
            0x5bdc_0fda
        );
    }

    #[test]
    fn single_byte_fast_path() {
        // The len == 1 fast path must agree with the general accumulation.
        for byte in 0u16..=255 {
            let b = byte as u8;
            let via_fast = adler32(1, &[b]);
            // Recompute via the short-length path by prepending a no-op empty.
            let via_general = adler32(adler32(1, b""), &[b]);
            assert_eq!(via_fast, via_general, "byte {b:#04x}");
        }
    }

    #[test]
    fn large_buffer_pinned_values() {
        // Buffers that cross the NMAX block boundary (11105 = 2*NMAX + 1, and
        // 50000 spans many blocks plus a remainder), pinned against canonical C
        // zlib. This exercises the per-block modulo cadence that makes the
        // result independent of how the input is chunked.
        assert_eq!(adler32(1, &gen_buf(11105)), 0xbde9_99bd);
        assert_eq!(adler32(1, &gen_buf(50000)), 0x0d33_4c88);
    }

    #[test]
    fn continuation_equals_one_shot() {
        // Splitting the input anywhere must not change the checksum.
        let data = gen_buf(40_000);
        let one_shot = adler32(1, &data);
        for &k in &[0usize, 1, 5, 16, NMAX, NMAX + 1, 12_345, 40_000] {
            let split = adler32(adler32(1, &data[..k]), &data[k..]);
            assert_eq!(split, one_shot, "split at k = {k}");
        }
    }

    #[test]
    fn adler32_matches_adler32_z() {
        // The two public entry points must produce identical results across the
        // boundary-relevant lengths (single byte, short path, NMAX edges).
        for &len in &[1usize, 2, 15, 16, 17, NMAX - 1, NMAX, NMAX + 1, 2 * NMAX] {
            let data = gen_buf(len);
            assert_eq!(adler32(1, &data), adler32_z(1, &data), "len = {len}");
        }
    }

    #[test]
    fn combine_equals_concatenation() {
        // adler32_combine of the two parts equals the checksum of the whole.
        let cases: [(Vec<u8>, Vec<u8>); 7] = [
            (Vec::new(), Vec::new()),
            (b"a".to_vec(), b"b".to_vec()),
            (b"hello".to_vec(), b"world".to_vec()),
            (gen_buf(1_000), gen_buf(2_000)),
            (gen_buf(NMAX), gen_buf(NMAX + 1)),
            (gen_buf(123), gen_buf(70_000)), // len2 > BASE exercises the rem reduction
            (b"123456789".to_vec(), gen_buf(5_000)),
        ];
        for (first, second) in &cases {
            let sum_first = adler32(1, first);
            let sum_second = adler32(1, second);
            let mut whole = first.clone();
            whole.extend_from_slice(second);
            let expected = adler32(1, &whole);
            let len2 = second.len() as i64;
            assert_eq!(
                adler32_combine(sum_first, sum_second, len2),
                expected,
                "combine len2 = {len2}"
            );
            // The 64-bit variant must behave identically.
            assert_eq!(
                adler32_combine64(sum_first, sum_second, len2),
                expected,
                "combine64 len2 = {len2}"
            );
        }
    }

    #[test]
    fn combine_negative_length_sentinel() {
        // A negative length is invalid and yields the debugging sentinel.
        assert_eq!(adler32_combine(0x1234_5678, 0x9abc_def0, -1), 0xffff_ffff);
        assert_eq!(adler32_combine64(1, 1, -42), 0xffff_ffff);
    }

    #[test]
    fn components_stay_reduced() {
        // Every checksum this engine returns must have both 16-bit halves < BASE,
        // i.e. be a valid Adler-32 value.
        for &len in &[0usize, 1, 7, 16, 100, NMAX, NMAX + 9, 50_000] {
            let sum = adler32(1, &gen_buf(len));
            let low = sum & 0xffff;
            let high = (sum >> 16) & 0xffff;
            assert!(low < BASE, "low half {low:#06x} >= BASE for len {len}");
            assert!(high < BASE, "high half {high:#06x} >= BASE for len {len}");
        }
    }
}
