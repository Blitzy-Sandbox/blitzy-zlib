//! Adler-32 checksum computation.
//!
//! This module is a faithful, memory-safe Rust port of the reference zlib
//! `adler32.c`. It computes the Adler-32 checksum that zlib-format streams
//! (RFC 1950) carry as their trailing 4-byte integrity field, and it is also
//! exposed through the public API because it is useful to applications on its
//! own.
//!
//! The implementation is pure integer arithmetic. It contains **no `unsafe`**
//! code, performs **no heap allocation**, and depends only on `core`, so it is
//! usable in `no_std` builds (this file requires no imports at all). Its output
//! is bit-identical to reference zlib for every input, which is the defining
//! acceptance criterion for the migration.
//!
//! # Algorithm
//!
//! Adler-32 maintains two 16-bit running sums, conventionally called `s1` and
//! `s2`, packed into a single [`u32`] as `(s2 << 16) | s1`:
//!
//! * `s1` is `1` plus the sum of every input byte, taken modulo [`BASE`].
//! * `s2` is the running sum of `s1` after each byte, taken modulo [`BASE`].
//!
//! To keep the sums from overflowing a `u32`, the input is processed in blocks
//! of at most [`NMAX`] bytes and both sums are reduced modulo [`BASE`] after
//! each block. Within a block the per-byte updates are unrolled in groups of
//! sixteen, mirroring the reference C `DO16` macro in `adler32.c`. Because the
//! arithmetic result is invariant to *where* the reductions are inserted
//! (reducing `s1` by a multiple of `BASE` shifts every later `s2` increment by
//! a multiple of `BASE`, leaving both halves unchanged modulo `BASE`), this
//! unrolling is a pure throughput optimization and remains bit-exact with the
//! plain byte-at-a-time formulation.

/// Largest prime smaller than `65536`.
///
/// Both component sums of the Adler-32 checksum are kept modulo this value.
const BASE: u32 = 65521;

/// Largest block length `n` for which the running sums cannot overflow a
/// 32-bit accumulator before the next reduction modulo [`BASE`].
///
/// Formally, `NMAX` is the largest `n` satisfying
/// `255 * n * (n + 1) / 2 + (n + 1) * (BASE - 1) <= 2^32 - 1`. Processing the
/// input in blocks no larger than `NMAX` bytes is precisely what makes the
/// plain (non-wrapping) `u32` arithmetic in this module provably free of
/// overflow — and therefore free of debug-mode panics.
const NMAX: usize = 5552;

/// Updates a running Adler-32 checksum with the bytes in `buf` and returns the
/// updated checksum.
///
/// An Adler-32 value is in the range of a 32-bit unsigned integer. A fresh
/// checksum is started from the required initial value `1`; passing an empty
/// slice takes the same zero-length path as reference zlib, normalizing each
/// 16-bit half modulo [`BASE`]. A valid running checksum (both halves already
/// `< BASE`) is therefore returned unchanged, so `adler32(1, b"")` returns `1`.
///
/// An Adler-32 checksum is almost as reliable as a CRC-32 but can be computed
/// much faster.
///
/// This mirrors the C `adler32(adler, buf, len)` entry point. The C contract of
/// returning the initial value `1` for a `NULL` buffer is handled at the FFI
/// boundary, where a null pointer can occur; in this idiomatic slice-based API
/// there is no null, so an empty slice is treated exactly like C's non-null
/// zero-length buffer.
///
/// # Examples
///
/// ```ignore
/// let mut sum = adler32(1, b"");     // required initial value
/// sum = adler32(sum, b"123456789");  // fold in more data
/// ```
#[must_use]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

/// Updates a running Adler-32 checksum with the bytes in `buf` and returns the
/// updated checksum.
///
/// This is identical to [`adler32`] and exists to mirror the C `adler32_z`
/// entry point, which accepts a `size_t` length rather than an `unsigned int`
/// length. In this slice-based API both collapse to `&[u8]`, so [`adler32`]
/// simply delegates here. Both names are retained because the FFI layer exposes
/// them as distinct C symbols.
#[must_use]
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Split the checksum into its two 16-bit component sums.
    let mut sum2 = (adler >> 16) & 0xffff;
    let mut adler = adler & 0xffff;

    // NOTE: there is deliberately no early return for an empty slice. Reference
    // zlib normalizes a non-null zero-length update through its `len < 16`
    // path — it reduces `adler` with a single conditional subtraction and
    // `sum2` modulo `BASE` — rather than echoing the packed seed back. The
    // `buf.len() < 16` branch below runs its byte loop zero times and performs
    // exactly that reduction, so `adler32(1, b"")` stays `1`, a valid running
    // value is returned unchanged, and an arbitrary seed such as `0xffff_ffff`
    // normalizes to `0x000e_000e` — matching zlib for every public seed. (The
    // C `buf == Z_NULL` → `1` sentinel is a null-pointer concern handled at the
    // FFI boundary; a slice is never null.)

    // Single-byte fast path: at most one conditional subtraction is needed for
    // each sum, so the more expensive modulo operations are avoided entirely.
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

    // Short input (fewer than 16 bytes): accumulate directly, then reduce once.
    // `adler` grows by at most `15 * 255`, so a single conditional subtraction
    // reduces it; `sum2` can grow larger and needs a full modulo.
    if buf.len() < 16 {
        for &byte in buf {
            adler += u32::from(byte);
            sum2 += adler;
        }
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 %= BASE;
        return adler | (sum2 << 16);
    }

    // General case: process the input in blocks of at most `NMAX` bytes so the
    // per-byte updates accumulate without overflowing a `u32` before the
    // reduction at the end of each block (`NMAX` is chosen precisely so plain,
    // non-wrapping arithmetic never overflows and never panics). Within each
    // full block the updates are unrolled sixteen at a time via [`do16`],
    // mirroring the reference C `DO16` macro; `NMAX` is divisible by 16, so a
    // full block is an exact number of 16-byte groups.
    let mut rest = buf;
    while rest.len() >= NMAX {
        let (block, tail) = rest.split_at(NMAX);
        for group in block.chunks_exact(16) {
            do16(&mut adler, &mut sum2, group);
        }
        adler %= BASE;
        sum2 %= BASE;
        rest = tail;
    }

    // Remaining bytes (fewer than `NMAX`): the whole 16-byte groups are still
    // unrolled through `DO16`, and the final short tail (fewer than 16 bytes)
    // is folded one byte at a time. A single reduction of each sum then
    // suffices; it is skipped entirely when nothing remains, mirroring the C
    // `if (len)` guard that avoids a redundant modulo.
    if !rest.is_empty() {
        let mut groups = rest.chunks_exact(16);
        for group in groups.by_ref() {
            do16(&mut adler, &mut sum2, group);
        }
        for &byte in groups.remainder() {
            adler += u32::from(byte);
            sum2 += adler;
        }
        adler %= BASE;
        sum2 %= BASE;
    }

    // Recombine the two component sums into the packed checksum.
    adler | (sum2 << 16)
}

/// Combines two Adler-32 checksums into one.
///
/// Given two byte sequences `seq1` and `seq2` with lengths `len1` and `len2`
/// and Adler-32 checksums `adler1` and `adler2` respectively, this returns the
/// Adler-32 checksum of the concatenation `seq1 || seq2`, requiring only
/// `adler1`, `adler2`, and `len2` — the length of the second sequence.
///
/// `len2` is a signed integer to mirror the C `z_off_t` / `z_off64_t` contract
/// (a single Rust `i64` covers both C widths). A negative `len2` has no
/// meaning; as a debugging aid this returns the invalid checksum `0xffff_ffff`,
/// matching the reference implementation.
#[must_use]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // For a negative length, return an invalid Adler-32 value as a clue for
    // debugging (matches reference zlib behavior).
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // Reduce the length modulo BASE; `rem` is the effective offset of `seq2`.
    let rem = (len2 % i64::from(BASE)) as u32;

    let sum1_lo = adler1 & 0xffff;

    // Compute `rem * sum1_lo mod BASE` through a `u64` intermediate. The product
    // can reach `65520 * 65535 = 4_293_388_200`: under `2^32`, but close enough
    // that the wider multiply removes any risk of a debug-mode overflow panic.
    let mut sum2 = ((u64::from(rem) * u64::from(sum1_lo)) % u64::from(BASE)) as u32;

    let mut sum1 = sum1_lo + (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // These conditional reductions reproduce the reference derivation exactly.
    // The two consecutive `sum1` checks, and the `2 * BASE` then `BASE` checks
    // on `sum2`, are intentional and required across the full input domain.
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

/// Folds sixteen consecutive input bytes into the running Adler-32 component
/// sums with the update sequence fully unrolled, mirroring the reference C
/// `DO16` macro from `adler32.c` (which expands to `{ adler += buf[i]; sum2 +=
/// adler; }` for `i` in `0..16`).
///
/// `chunk` is always a 16-byte group produced by `chunks_exact(16)`; binding it
/// to a fixed-size array reference performs a single length check and then lets
/// the compiler prove every index is in bounds, eliding the per-byte bounds
/// checks on the hot path. This routine is pure integer arithmetic and contains
/// no `unsafe`.
#[inline(always)]
fn do16(adler: &mut u32, sum2: &mut u32, chunk: &[u8]) {
    let bytes: &[u8; 16] = chunk
        .try_into()
        .expect("adler32 DO16 group must be exactly 16 bytes");

    // Accumulate through locals so the sixteen dependent updates stay in
    // registers; the two out-parameters are written back once at the end.
    let mut a = *adler;
    let mut s = *sum2;
    a += u32::from(bytes[0]);
    s += a;
    a += u32::from(bytes[1]);
    s += a;
    a += u32::from(bytes[2]);
    s += a;
    a += u32::from(bytes[3]);
    s += a;
    a += u32::from(bytes[4]);
    s += a;
    a += u32::from(bytes[5]);
    s += a;
    a += u32::from(bytes[6]);
    s += a;
    a += u32::from(bytes[7]);
    s += a;
    a += u32::from(bytes[8]);
    s += a;
    a += u32::from(bytes[9]);
    s += a;
    a += u32::from(bytes[10]);
    s += a;
    a += u32::from(bytes[11]);
    s += a;
    a += u32::from(bytes[12]);
    s += a;
    a += u32::from(bytes[13]);
    s += a;
    a += u32::from(bytes[14]);
    s += a;
    a += u32::from(bytes[15]);
    s += a;
    *adler = a;
    *sum2 = s;
}

#[cfg(test)]
mod tests {
    use super::{BASE, NMAX, adler32, adler32_combine, adler32_z};

    // All expected values below were produced by reference zlib
    // (Python's `zlib.adler32`), which performs the identical half-split, so an
    // arbitrary initial value is directly comparable.

    #[test]
    fn empty_slice_returns_input_unchanged() {
        // The required initial value for a fresh checksum.
        assert_eq!(adler32(1, b""), 1);
        // A *valid* running value (both halves already < BASE) is returned
        // unchanged by an empty update.
        assert_eq!(adler32(0xdead_beef, b""), 0xdead_beef);
        assert_eq!(adler32_z(0, b""), 0);
    }

    #[test]
    fn empty_slice_normalizes_arbitrary_seed_like_zlib() {
        // Regression test for the byte-exact parity fix: an empty (non-null)
        // update must take reference zlib's zero-length path, which reduces
        // each 16-bit half modulo BASE, rather than echoing an unreduced seed.
        // Expected values are from reference zlib (`zlib.adler32(b"", seed)`).
        assert_eq!(adler32(0xffff_ffff, b""), 0x000e_000e);
        assert_eq!(adler32(0x0000_ffff, b""), 0x0000_000e);
        assert_eq!(adler32(0xffff_0000, b""), 0x000e_0000);
        assert_eq!(adler32_z(0xffff_ffff, b""), 0x000e_000e);
        // Feeding an already-normalized value back is a fixed point.
        assert_eq!(adler32(0x000e_000e, b""), 0x000e_000e);
    }

    #[test]
    fn known_reference_vectors() {
        assert_eq!(adler32(1, b"a"), 0x0062_0062);
        assert_eq!(adler32(1, b"abc"), 0x024d_0127);
        assert_eq!(adler32(1, b"123456789"), 0x091e_01de);
        assert_eq!(adler32(1, b"Wikipedia"), 0x11e6_0398);
        // 15 bytes exercises the `len < 16` short path.
        assert_eq!(adler32(1, b"abcdefghijklmno"), 0x2fb7_0619);
        // 16 bytes crosses into the general (block) path.
        assert_eq!(adler32(1, b"abcdefghijklmnop"), 0x3640_0689);
    }

    #[test]
    fn adler32_and_adler32_z_agree() {
        let data = b"The quick brown fox jumps over the lazy dog";
        for len in 0..=data.len() {
            assert_eq!(adler32(1, &data[..len]), adler32_z(1, &data[..len]));
        }
    }

    #[test]
    fn byte_at_a_time_matches_single_pass() {
        // Repeatedly exercising the single-byte fast path must compose to the
        // same result as one bulk call — this validates running updates.
        let data = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut running = 1;
        for &byte in data {
            running = adler32(running, &[byte]);
        }
        assert_eq!(running, adler32(1, data));
    }

    #[test]
    fn split_update_matches_whole() {
        // For any split buf = a || b: adler32(adler32(1, a), b) == adler32(1, buf).
        let whole = b"The quick brown fox jumps over the lazy dog";
        for split in 0..=whole.len() {
            let (a, b) = whole.split_at(split);
            assert_eq!(adler32(adler32(1, a), b), adler32(1, whole));
        }
        assert_eq!(adler32(1, whole), 0x5bdc_0fda);
    }

    #[test]
    fn large_buffer_matches_reference() {
        // 20_000 bytes (> NMAX) confirms block-boundary reductions are correct.
        let mut big = [0u8; 20_000];
        for (i, byte) in big.iter_mut().enumerate() {
            *byte = ((i * 37 + 11) & 0xff) as u8;
        }
        assert_eq!(adler32(1, &big), 0xdca8_ea4b);
    }

    #[test]
    fn block_boundary_no_overflow() {
        // Worst-case all-0xff blocks with an extreme initial value stress the
        // NMAX overflow bound. These must not panic and must stay bit-exact.
        let ff_nmax = [0xffu8; NMAX];
        assert_eq!(adler32(0xffff_ffff, &ff_nmax), 0x0bab_9b99);
        assert_eq!(adler32(1, &ff_nmax), 0xf18f_9b8c);

        // Crossing exactly one block boundary.
        let ff_cross = [0xffu8; NMAX + 100];
        assert_eq!(adler32(0xffff_ffff, &ff_cross), 0x7e65_ff35);

        // Crossing two block boundaries.
        let ff_two = [0xffu8; 2 * NMAX + 7];
        assert_eq!(adler32(1, &ff_two), 0x9d7b_3e1f);
    }

    #[test]
    fn general_path_do16_matches_reference() {
        // Directly exercises the DO16-unrolled general path at and around the
        // NMAX block boundary: exactly NMAX (whole 16-groups, no tail), a short
        // (<16) tail, one extra full group, a mid-size tail, exact multi-block,
        // multi-block with a tail, and three full blocks. Expected values are
        // reference zlib (`zlib.adler32`) for `((i*37+11) & 0xff)`.
        fn pattern(n: usize) -> Vec<u8> {
            (0..n).map(|i| ((i * 37 + 11) & 0xff) as u8).collect()
        }
        let cases: [(usize, u32); 7] = [
            (NMAX, 0x6b2c_cc6f),         // exactly one block; no remainder
            (NMAX + 15, 0xa4f9_d3d1),    // block + <16-byte tail
            (NMAX + 16, 0x797f_d477),    // block + one extra whole group
            (NMAX + 100, 0x6c4e_fee9),   // block + 6 groups + 4-byte tail
            (2 * NMAX, 0xdcf9_9aec),     // exactly two blocks
            (2 * NMAX + 9, 0x6546_9f63), // two blocks + short tail
            (3 * NMAX, 0x5276_6869),     // three blocks
        ];
        for (n, expected) in cases {
            let data = pattern(n);
            assert_eq!(adler32(1, &data), expected, "bulk mismatch at n={n}");
            // Self-consistency: folding the same bytes one at a time (the
            // single-byte fast path) must compose to the unrolled bulk result,
            // independent of any external reference.
            let mut running = 1;
            for &byte in &data {
                running = adler32(running, &[byte]);
            }
            assert_eq!(running, expected, "byte-at-a-time mismatch at n={n}");
        }
    }

    #[test]
    fn combine_matches_concatenation() {
        let whole = b"The quick brown fox jumps over the lazy dog";
        let (a, b) = whole.split_at(20);
        let c1 = adler32(1, a);
        let c2 = adler32(1, b);
        assert_eq!(c1, 0x4c62_0734);
        assert_eq!(c2, 0x69d6_08a7);
        assert_eq!(adler32_combine(c1, c2, b.len() as i64), adler32(1, whole));
        assert_eq!(adler32_combine(c1, c2, b.len() as i64), 0x5bdc_0fda);
    }

    #[test]
    fn combine_large_matches_concatenation() {
        let mut big = [0u8; 20_000];
        for (i, byte) in big.iter_mut().enumerate() {
            *byte = ((i * 37 + 11) & 0xff) as u8;
        }
        let (a, b) = big.split_at(8_000);
        let combined = adler32_combine(adler32(1, a), adler32(1, b), b.len() as i64);
        assert_eq!(combined, adler32(1, &big));
        assert_eq!(combined, 0xdca8_ea4b);
    }

    #[test]
    fn combine_with_empty_second_sequence_is_identity() {
        // Combining with a zero-length second sequence returns the first checksum.
        let c1 = adler32(1, b"The quick brown fox ");
        let empty = adler32(1, b"");
        assert_eq!(adler32_combine(c1, empty, 0), c1);
    }

    #[test]
    fn combine_negative_length_returns_invalid_marker() {
        assert_eq!(adler32_combine(0x4c62_0734, 0x69d6_08a7, -1), 0xffff_ffff);
        assert_eq!(adler32_combine(1, 1, -12345), 0xffff_ffff);
    }

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(BASE, 65_521);
        assert_eq!(NMAX, 5_552);
    }
}
