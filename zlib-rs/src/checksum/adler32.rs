//! Adler-32 checksum computation.
//!
//! Implements the Adler-32 checksum algorithm as specified in RFC 1950 (Section 9).
//! The checksum is the concatenation of two 16-bit values `s1` and `s2`, where:
//!
//! - `s1 = (1 + sum of all bytes) mod 65521`
//! - `s2 = (sum of all s1 values) mod 65521`
//!
//! The result is stored as `(s2 << 16) | s1`.
//!
//! The initial Adler-32 value is `1` (i.e., `s1 = 1, s2 = 0`).
//!
//! Ported from `adler32.c` (zlib 1.3.2.1-motley).

/// Largest prime smaller than 65536.
///
/// This is the modulus used for both the `s1` and `s2` sums in the Adler-32
/// algorithm. Using a prime modulus provides good distribution properties
/// for error detection.
const BASE: u32 = 65521;

/// Largest `n` such that `255*n*(n+1)/2 + (n+1)*(BASE-1) <= 2^32 - 1`.
///
/// This is the maximum number of bytes that can be processed in a single
/// block before a modulo reduction is required to prevent `u32` overflow
/// of the running `s2` accumulator. Processing in `NMAX`-sized blocks
/// allows deferring the expensive modulo operation.
const NMAX: usize = 5552;

/// Update a running Adler-32 checksum with the bytes in `buf` and return the
/// updated checksum.
///
/// This is the main Adler-32 computation function with a size-typed length
/// parameter (via the slice). In the C library, this corresponds to `adler32_z`
/// which accepts a `z_size_t` length.
///
/// If `buf` is empty, this function returns the required initial value for the
/// checksum (`1`), matching the C zlib behavior of `adler32_z(adler, Z_NULL, 0)`.
///
/// # Arguments
///
/// * `adler` — The running Adler-32 checksum value.
/// * `buf` — The byte slice to process.
///
/// # Returns
///
/// The updated Adler-32 checksum as a `u32`.
///
/// # Algorithm
///
/// The input is processed in blocks of up to `NMAX` bytes. Within each block,
/// bytes are consumed 16 at a time (replacing the C library's `DO16` macro
/// unrolling). A modulo reduction is applied once per block, which is safe
/// because the `NMAX` bound guarantees no `u32` overflow within a block.
#[must_use]
#[inline]
#[allow(clippy::similar_names, clippy::module_name_repetitions)]
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Split Adler-32 into component sums (C: adler32.c lines 66-67)
    let mut s1 = adler & 0xffff;
    let mut s2 = (adler >> 16) & 0xffff;

    // Handle empty input — return the required initial Adler-32 value.
    // In C: `if (buf == Z_NULL) return 1L;` (adler32.c lines 81-82)
    if buf.is_empty() {
        return 1;
    }

    // Fast path for single-byte input (C: adler32.c lines 70-78).
    // This optimization benefits callers that process one byte at a time.
    if buf.len() == 1 {
        s1 += u32::from(buf[0]);
        if s1 >= BASE {
            s1 -= BASE;
        }
        s2 += s1;
        if s2 >= BASE {
            s2 -= BASE;
        }
        return s1 | (s2 << 16);
    }

    // Short input optimization — fewer than 16 bytes need no NMAX blocking
    // (C: adler32.c lines 85-94). A single modulo suffices since at most
    // 15 additions of at most 255 cannot overflow a u32.
    if buf.len() < 16 {
        for &byte in buf {
            s1 += u32::from(byte);
            s2 += s1;
        }
        if s1 >= BASE {
            s1 -= BASE;
        }
        // MOD28 in C — only a small number of BASE multiples could have
        // accumulated, but a full modulo is correct and equally fast.
        s2 %= BASE;
        return s1 | (s2 << 16);
    }

    // Main processing: NMAX-sized blocks with a single modulo per block.
    // This replaces the C DO1/DO2/DO4/DO8/DO16 macro unrolling with
    // idiomatic Rust iterator-based processing.
    //
    // The NMAX guarantee ensures that both `s1` and `s2` remain within
    // `u32` range throughout an entire block before the modulo reduction.
    let mut remaining = buf;

    // Process full NMAX-sized blocks (C: adler32.c lines 97-106)
    while remaining.len() >= NMAX {
        let (block, rest) = remaining.split_at(NMAX);
        remaining = rest;

        // Process 16 bytes at a time within the block (replaces DO16 macro).
        // NMAX (5552) is exactly divisible by 16, so chunks_exact produces
        // no remainder for a full NMAX block.
        for chunk in block.chunks_exact(16) {
            for &byte in chunk {
                s1 += u32::from(byte);
                s2 += s1;
            }
        }
        s1 %= BASE;
        s2 %= BASE;
    }

    // Process remaining bytes (< NMAX) with one final modulo reduction
    // (C: adler32.c lines 108-121)
    if !remaining.is_empty() {
        // Process 16 bytes at a time
        let chunks = remaining.chunks_exact(16);
        let tail = chunks.remainder();
        for chunk in chunks {
            for &byte in chunk {
                s1 += u32::from(byte);
                s2 += s1;
            }
        }
        // Process any trailing bytes that don't fill a complete 16-byte chunk
        for &byte in tail {
            s1 += u32::from(byte);
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
    }

    // Return recombined sums (C: adler32.c line 124)
    s1 | (s2 << 16)
}

/// Update a running Adler-32 checksum with the bytes in `buf` and return the
/// updated checksum.
///
/// This is a convenience wrapper for [`adler32_z`]. In the C library, the only
/// difference is the parameter type (`uInt` vs `z_size_t` for the length); in
/// Rust, slices carry their own length, so this is an exact alias.
///
/// # Arguments
///
/// * `adler` — The running Adler-32 checksum value.
/// * `buf` — The byte slice to process.
///
/// # Returns
///
/// The updated Adler-32 checksum.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::adler32;
///
/// // Get the required initial value
/// let init = adler32(0, &[]);
/// assert_eq!(init, 1);
///
/// // Compute checksum of "Hello"
/// let checksum = adler32(1, b"Hello");
/// ```
#[must_use]
#[inline]
#[allow(clippy::module_name_repetitions)]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

/// Combine two Adler-32 checksums into one.
///
/// Given two sequences of bytes, *seq1* and *seq2*, with lengths *len1* and
/// *len2*, for which Adler-32 checksums `adler1` and `adler2` were computed,
/// this function returns the Adler-32 checksum of the concatenation
/// *seq1* ∥ *seq2*, requiring only `adler1`, `adler2`, and `len2`.
///
/// This enables efficient checksum combination without needing the original
/// data — useful for parallel compression or appending to checksummed streams.
///
/// # Arguments
///
/// * `adler1` — Adler-32 checksum of the first sequence.
/// * `adler2` — Adler-32 checksum of the second sequence.
/// * `len2` — Length of the second sequence. Must be non-negative for a
///   meaningful result.
///
/// # Returns
///
/// The combined Adler-32 checksum, or `0xFFFF_FFFF` if `len2` is negative.
///
/// # Mathematical Basis
///
/// The combination formula exploits the linearity of the Adler-32 sums
/// modulo `BASE`. Given the individual checksums and the length of the
/// second sequence, the combined `s1` and `s2` values can be computed
/// algebraically without re-processing any data.
#[must_use]
#[allow(
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::module_name_repetitions
)]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // For negative len, return invalid adler32 as a debugging indicator
    // (C: adler32.c lines 139-140)
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // Reduce len2 modulo BASE (replaces C MOD63 macro).
    // The cast to u64 is safe because len2 >= 0 at this point.
    // After reduction, rem fits comfortably in a u32.
    let rem = (len2 as u64 % u64::from(BASE)) as u32;

    // Extract the lower half of the first checksum (C: adler32.c line 145)
    let mut sum1 = adler1 & 0xffff;

    // Compute rem * sum1 mod BASE using u64 intermediate to prevent overflow.
    // Maximum product: 65520 × 65535 = 4,293,852,200, which exceeds u32::MAX
    // (4,294,967,295) only marginally but we use u64 for safety and clarity.
    // (C: adler32.c lines 146-147: `sum2 = rem * sum1; MOD(sum2);`)
    let mut sum2 = ((u64::from(rem) * u64::from(sum1)) % u64::from(BASE)) as u32;

    // Combine the checksums using the mathematical derivation.
    // Adding BASE prevents underflow from the subtraction of 1 and rem.
    // (C: adler32.c lines 148-149)
    sum1 += (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Reduce sum1 — may need up to two subtractions since sum1 can reach
    // at most 65520 + 65535 + 65521 - 1 = 196575, which is < 3×BASE.
    // (C: adler32.c lines 150-151)
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum1 >= BASE {
        sum1 -= BASE;
    }

    // Reduce sum2 — may need one large and one small subtraction since sum2
    // can reach at most 65520 + 65520 + 65521 - 0 = 196561, which is < 3×BASE.
    // (C: adler32.c lines 152-153)
    if sum2 >= (BASE << 1) {
        sum2 -= BASE << 1;
    }
    if sum2 >= BASE {
        sum2 -= BASE;
    }

    sum1 | (sum2 << 16)
}

#[cfg(test)]
#[allow(clippy::cast_possible_wrap)]
mod tests {
    use super::*;

    #[test]
    fn initial_value_is_one() {
        // The initial Adler-32 value is 1 (s1=1, s2=0).
        // Calling with an empty buffer returns the initial value.
        assert_eq!(adler32_z(0, &[]), 1);
        assert_eq!(adler32_z(1, &[]), 1);
        assert_eq!(adler32(0, &[]), 1);
    }

    #[test]
    fn single_byte() {
        // For byte 'a' (97) with initial adler=1:
        //   s1 = 1, s2 = 0
        //   s1 += 97 → s1 = 98
        //   s2 += s1 → s2 = 98
        //   result = (98 << 16) | 98 = 0x0062_0062
        assert_eq!(adler32_z(1, b"a"), 0x0062_0062);
    }

    #[test]
    fn three_bytes_abc() {
        // For "abc" with initial adler=1:
        //   start: s1=1, s2=0
        //   after 'a'(97): s1=98,  s2=98
        //   after 'b'(98): s1=196, s2=294
        //   after 'c'(99): s1=295, s2=589
        //   result = (589 << 16) | 295 = 0x024d_0127
        assert_eq!(adler32_z(1, b"abc"), 0x024d_0127);
    }

    #[test]
    fn wrapper_matches_z() {
        // adler32 is just a thin wrapper over adler32_z
        let data = b"Hello, World!";
        assert_eq!(adler32(1, data), adler32_z(1, data));
    }

    #[test]
    fn short_input_under_16() {
        // Test the short-input path (< 16 bytes) but > 1 byte
        let data = b"short";
        let result = adler32_z(1, data);
        // Manually verify: s1=1, s2=0
        // 's'=115: s1=116, s2=116
        // 'h'=104: s1=220, s2=336
        // 'o'=111: s1=331, s2=667
        // 'r'=114: s1=445, s2=1112
        // 't'=116: s1=561, s2=1673
        // result = (1673 << 16) | 561 = 0x0689_0231
        assert_eq!(result, 0x0689_0231);
    }

    #[test]
    fn exactly_16_bytes() {
        // Test input of exactly 16 bytes — exercises the main 16-byte chunk path
        let data = b"0123456789abcdef";
        assert_eq!(data.len(), 16);
        let result = adler32_z(1, data);
        // Verify by computing manually or by cross-checking
        let mut s1: u32 = 1;
        let mut s2: u32 = 0;
        for &b in data {
            s1 += u32::from(b);
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
        assert_eq!(result, s1 | (s2 << 16));
    }

    #[test]
    fn large_input_over_nmax() {
        // Test input larger than NMAX (5552) to exercise the multi-block path
        let data = vec![0xABu8; NMAX + 100];
        let result = adler32_z(1, &data);

        // Verify by computing manually
        let mut s1: u32 = 1;
        let mut s2: u32 = 0;
        for &b in &data {
            s1 += u32::from(b);
            s2 += s1;
            // Reduce periodically (not optimized, just for correctness check)
            s1 %= BASE;
            s2 %= BASE;
        }
        assert_eq!(result, s1 | (s2 << 16));
    }

    #[test]
    fn streaming_computation() {
        // Verify that streaming (multiple calls) produces the same result
        // as a single call on the entire data.
        let data = b"Hello, World! This is a test of streaming Adler-32.";
        let single = adler32_z(1, data);

        let mid = data.len() / 2;
        let partial = adler32_z(1, &data[..mid]);
        let streamed = adler32_z(partial, &data[mid..]);
        assert_eq!(single, streamed);
    }

    #[test]
    fn combine_basic() {
        // Verify that adler32_combine produces the same result as computing
        // the checksum over the concatenated data.
        let data1 = b"Hello, ";
        let data2 = b"World!";

        let a1 = adler32_z(1, data1);
        let a2 = adler32_z(1, data2);
        let combined = adler32_combine(a1, a2, data2.len() as i64);

        let mut full_data = Vec::new();
        full_data.extend_from_slice(data1);
        full_data.extend_from_slice(data2);
        let full = adler32_z(1, &full_data);

        assert_eq!(combined, full);
    }

    #[test]
    fn combine_negative_len() {
        // Negative len2 should return 0xFFFF_FFFF
        assert_eq!(adler32_combine(1, 1, -1), 0xffff_ffff);
        assert_eq!(adler32_combine(1, 1, -100), 0xffff_ffff);
    }

    #[test]
    fn combine_zero_len() {
        // Combining with a zero-length second sequence should return the
        // first checksum unchanged (adler2 = initial value = 1).
        let a1 = adler32_z(1, b"test data");
        let combined = adler32_combine(a1, 1, 0);
        assert_eq!(combined, a1);
    }

    #[test]
    fn all_zeros() {
        // Test with a buffer of all zeros
        let data = vec![0u8; 1000];
        let result = adler32_z(1, &data);
        // s1 = 1 + 0*1000 = 1
        // s2 = 0 + 1*1000 = 1000
        // result = (1000 << 16) | 1 = 0x03e8_0001
        assert_eq!(result, 0x03e8_0001);
    }

    #[test]
    fn all_0xff() {
        // Test with a buffer of all 0xFF bytes
        let data = vec![0xFFu8; 100];
        let result = adler32_z(1, &data);
        // Compute expected value
        let mut s1: u32 = 1;
        let mut s2: u32 = 0;
        for _ in 0..100 {
            s1 += 255;
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
        assert_eq!(result, s1 | (s2 << 16));
    }

    #[test]
    fn combine_large_len2() {
        // Test combine with a large len2 to exercise the modulo reduction
        let data1 = b"first";
        let data2 = vec![42u8; 100_000];

        let a1 = adler32_z(1, data1);
        let a2 = adler32_z(1, &data2);
        let combined = adler32_combine(a1, a2, data2.len() as i64);

        let mut full = Vec::with_capacity(data1.len() + data2.len());
        full.extend_from_slice(data1);
        full.extend_from_slice(&data2);
        let expected = adler32_z(1, &full);

        assert_eq!(combined, expected);
    }
}
