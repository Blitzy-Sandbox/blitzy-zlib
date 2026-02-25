//! Adler-32 checksum computation.
//!
//! This module implements the Adler-32 checksum algorithm as used in the
//! zlib compressed data format (RFC 1950). The Adler-32 checksum is faster
//! to compute than CRC-32 but provides weaker error detection.
//!
//! The checksum is computed as two 16-bit sums (s1 and s2) modulo 65521
//! (the largest prime less than 65536). s1 is the sum of all bytes plus 1,
//! and s2 is the running sum of s1 values. The final checksum is
//! `(s2 << 16) | s1`.
//!
//! # Algorithm
//!
//! The Adler-32 value starts at 1 (not 0). For each input byte `b`:
//! - `s1 = (s1 + b) mod 65521`
//! - `s2 = (s2 + s1) mod 65521`
//!
//! The modular reductions are deferred and batched in groups of up to
//! [`NMAX`] (5552) bytes to avoid overflow of the 32-bit accumulator,
//! since `255 * 5552 * (5552 + 1) / 2 + (5552 + 1) * (65521 - 1)` fits
//! within `u32::MAX`.
//!
//! # Compatibility
//!
//! This implementation produces byte-identical Adler-32 values to C zlib's
//! `adler32()` and `adler32_z()` functions, as translated from `adler32.c`.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::checksum::adler32::adler32;
//!
//! // Compute checksum of some data
//! let checksum = adler32(1, b"Hello, World!");
//! assert_ne!(checksum, 0);
//!
//! // Initial Adler-32 of empty data is 1
//! let initial = adler32(1, b"");
//! assert_eq!(initial, 1);
//! ```

// ---------------------------------------------------------------------------
// Constants — must match adler32.c lines 10-11 exactly
// ---------------------------------------------------------------------------

/// Largest prime smaller than 65536.
/// Used as the modulo base for Adler-32 computation.
/// From `adler32.c` line 10: `#define BASE 65521U`.
const BASE: u32 = 65521;

/// Largest `n` such that `255*n*(n+1)/2 + (n+1)*(BASE-1) <= 2^32 - 1`.
///
/// This is the maximum number of bytes that can be processed in one
/// loop iteration before a modulo reduction is required to prevent
/// overflow of the 32-bit accumulator. The value 5552 is evenly
/// divisible by 16 (5552 / 16 = 347), which simplifies the unrolled
/// inner loop.
///
/// From `adler32.c` line 11: `#define NMAX 5552`.
const NMAX: usize = 5552;

// ---------------------------------------------------------------------------
// DO16-equivalent inline helper
// ---------------------------------------------------------------------------

/// Process 16 bytes from `chunk` into the running Adler-32 sums, equivalent
/// to the C `DO16(buf)` macro that expands to `DO8(buf,0); DO8(buf,8)`.
///
/// Each step adds the next byte to `adler` (s1) and then adds the updated
/// `adler` to `sum2` (s2). This hand-unrolled loop avoids per-byte loop
/// overhead while preserving the sequential dependency between s1 and s2.
#[inline(always)]
fn do16(chunk: &[u8], adler: &mut u32, sum2: &mut u32) {
    // chunk is guaranteed to be 16 bytes by callers (chunks_exact(16)).
    *adler += chunk[0] as u32;
    *sum2 += *adler;
    *adler += chunk[1] as u32;
    *sum2 += *adler;
    *adler += chunk[2] as u32;
    *sum2 += *adler;
    *adler += chunk[3] as u32;
    *sum2 += *adler;
    *adler += chunk[4] as u32;
    *sum2 += *adler;
    *adler += chunk[5] as u32;
    *sum2 += *adler;
    *adler += chunk[6] as u32;
    *sum2 += *adler;
    *adler += chunk[7] as u32;
    *sum2 += *adler;
    *adler += chunk[8] as u32;
    *sum2 += *adler;
    *adler += chunk[9] as u32;
    *sum2 += *adler;
    *adler += chunk[10] as u32;
    *sum2 += *adler;
    *adler += chunk[11] as u32;
    *sum2 += *adler;
    *adler += chunk[12] as u32;
    *sum2 += *adler;
    *adler += chunk[13] as u32;
    *sum2 += *adler;
    *adler += chunk[14] as u32;
    *sum2 += *adler;
    *adler += chunk[15] as u32;
    *sum2 += *adler;
}

// ---------------------------------------------------------------------------
// Core computation — adler32_z
// ---------------------------------------------------------------------------

/// Compute the Adler-32 checksum of `buf`, starting with `adler` as the
/// initial value.
///
/// This is the primary Adler-32 computation function, equivalent to C zlib's
/// `adler32_z(uLong adler, const Bytef *buf, z_size_t len)` (adler32.c
/// lines 61–125).
///
/// The Adler-32 checksum is composed of two 16-bit sums:
/// - `s1` (lower 16 bits): running sum of input bytes + 1
/// - `s2` (upper 16 bits): running sum of `s1` values
///
/// Both sums are reduced modulo [`BASE`] (65521).
///
/// # Arguments
///
/// * `adler` — Initial Adler-32 value.  Use `1` for a fresh computation.
/// * `buf`   — Input byte slice.
///
/// # Returns
///
/// Updated Adler-32 value incorporating the bytes of `buf`.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::adler32_z;
///
/// // Fresh computation
/// let result = adler32_z(1, b"Hello");
/// assert_ne!(result, 0);
///
/// // Incremental update
/// let partial = adler32_z(1, b"He");
/// let full    = adler32_z(partial, b"llo");
/// assert_eq!(full, result);
///
/// // Empty buffer always returns 1 (matching C adler32(x, NULL, 0) == 1)
/// assert_eq!(adler32_z(42, b""), 1);
/// ```
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Empty buffer: return the initial Adler-32 value.
    // C (line 81-82): `if (buf == Z_NULL) return 1L;`
    // The C implementation returns 1 for any NULL-pointer call regardless of
    // the `adler` argument.  In Rust, an empty slice is the idiomatic
    // equivalent of `(NULL, 0)`.  Returning 1 here preserves the canonical
    // "get initial value" semantic: `adler32(0, &[]) == 1`.
    if buf.is_empty() {
        return 1;
    }

    // Split the composite Adler-32 value into its two 16-bit component sums.
    // C lines 66-67:
    //   sum2 = (adler >> 16) & 0xffff;
    //   adler &= 0xffff;
    let mut sum2: u32 = (adler >> 16) & 0xffff;
    let mut adler: u32 = adler & 0xffff;

    // -----------------------------------------------------------------------
    // Fast path: single byte (C lines 70-78)
    // -----------------------------------------------------------------------
    if buf.len() == 1 {
        adler += buf[0] as u32;
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 += adler;
        if sum2 >= BASE {
            sum2 -= BASE;
        }
        return adler | (sum2 << 16);
    }

    // -----------------------------------------------------------------------
    // Short buffer path: fewer than 16 bytes (C lines 85-94)
    // -----------------------------------------------------------------------
    if buf.len() < 16 {
        for &byte in buf {
            adler += byte as u32;
            sum2 += adler;
        }
        if adler >= BASE {
            adler -= BASE;
        }
        // MOD28 equivalent — for short buffers at most 15 bytes can be added,
        // so sum2 cannot exceed 2 * BASE before this reduction.
        sum2 %= BASE;
        return adler | (sum2 << 16);
    }

    let mut remaining = buf;

    // -----------------------------------------------------------------------
    // Main loop: process NMAX-sized blocks (C lines 97-106)
    // Each NMAX block allows exactly one modular reduction because NMAX is
    // chosen so that the 32-bit accumulators cannot overflow within a block.
    // NMAX (5552) is divisible by 16, so chunks_exact(16) has no remainder.
    // -----------------------------------------------------------------------
    while remaining.len() >= NMAX {
        let (block, rest) = remaining.split_at(NMAX);
        remaining = rest;

        // Process 16 bytes at a time — DO16 unrolled loop (C lines 100-103).
        for chunk in block.chunks_exact(16) {
            do16(chunk, &mut adler, &mut sum2);
        }
        // NMAX is divisible by 16, so no remainder bytes within this block.

        // Modular reduction after each NMAX block (C lines 104-105).
        adler %= BASE;
        sum2 %= BASE;
    }

    // -----------------------------------------------------------------------
    // Tail: process remaining bytes (< NMAX) — C lines 109-121
    // Only one modular reduction is needed because len < NMAX guarantees
    // no overflow.
    // -----------------------------------------------------------------------
    if !remaining.is_empty() {
        // Process 16-byte chunks.
        let mut chunks = remaining.chunks_exact(16);
        for chunk in &mut chunks {
            do16(chunk, &mut adler, &mut sum2);
        }

        // Process leftover bytes that don't fill a 16-byte chunk
        // (C lines 115-118).
        for &byte in chunks.remainder() {
            adler += byte as u32;
            sum2 += adler;
        }

        // Single modular reduction for the tail (C lines 119-120).
        adler %= BASE;
        sum2 %= BASE;
    }

    // Return the recombined composite value (C line 124).
    adler | (sum2 << 16)
}

// ---------------------------------------------------------------------------
// C-API compatible wrapper — adler32
// ---------------------------------------------------------------------------

/// Compute the Adler-32 checksum with a `u32`-length interface (C API
/// compatible).
///
/// This is a thin wrapper around [`adler32_z`] that exists purely for API
/// naming parity with C zlib's
/// `adler32(uLong adler, const Bytef *buf, uInt len)` (adler32.c lines
/// 128–130). In Rust, slice lengths are platform-native `usize`, so the two
/// functions are identical.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::adler32;
///
/// let data = b"Hello, World!";
/// let checksum = adler32(1, data);
/// assert_ne!(checksum, 0);
/// ```
#[inline]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

// ---------------------------------------------------------------------------
// Combine operations — adler32_combine_, adler32_combine, adler32_combine64
// ---------------------------------------------------------------------------

/// Internal implementation of Adler-32 combine.
///
/// Given the Adler-32 checksums of two contiguous sequences and the *length*
/// of the second sequence, returns the Adler-32 of their concatenation
/// using modular arithmetic over ℤ/65521ℤ.
///
/// This is a direct translation of C `adler32_combine_(uLong adler1,
/// uLong adler2, z_off64_t len2)` from `adler32.c` lines 133–155.
///
/// # Overflow safety
///
/// The product `rem * sum1` can reach up to 65520 × 65535 ≈ 4.29 × 10⁹,
/// which overflows `u32::MAX` (4,294,967,295).  We perform the
/// multiplication in `u64` and reduce modulo `BASE` before truncating back
/// to `u32`.  C zlib relies on `unsigned long` being 64-bit on modern
/// LP64 platforms, but Rust's `u32` is strictly 32-bit, so the widening is
/// mandatory.
fn adler32_combine_(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // For negative len, return 0xFFFFFFFF as a sentinel for debugging
    // (C line 139).
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // MOD63 equivalent: reduce len2 modulo BASE.
    // C line 143: `MOD63(len2);`
    // In Rust, the `%` operator on i64 handles 64-bit values correctly.
    let rem = (len2 % BASE as i64) as u32;

    // Extract s1 component of adler1 (C line 145).
    let sum1_init = adler1 & 0xffff;

    // Multiply rem × sum1 in u64 to avoid u32 overflow, then reduce
    // modulo BASE (C lines 146-147: `sum2 = rem * sum1; MOD(sum2);`).
    let mut sum2: u32 = ((rem as u64 * sum1_init as u64) % BASE as u64) as u32;

    // C line 148: `sum1 += (adler2 & 0xffff) + BASE - 1;`
    let mut sum1: u32 = sum1_init + (adler2 & 0xffff) + BASE - 1;

    // C line 149:
    // `sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;`
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Conditional subtractions to keep values in [0, BASE) without a full
    // modulo operation (C lines 150-153).
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum2 >= BASE << 1 {
        sum2 -= BASE << 1;
    }
    if sum2 >= BASE {
        sum2 -= BASE;
    }

    // Recombine the two sums (C line 154).
    sum1 | (sum2 << 16)
}

/// Combine two Adler-32 checksums for concatenated data.
///
/// Given the Adler-32 checksums of two sequences, returns the Adler-32
/// checksum of their concatenation.
///
/// * `adler1` — Adler-32 of the first sequence.
/// * `adler2` — Adler-32 of the second sequence.
/// * `len2`   — Length (in bytes) of the second sequence.
///
/// Equivalent to C zlib's `adler32_combine(uLong, uLong, z_off_t)`
/// (adler32.c lines 158–160).
///
/// Returns `0xFFFFFFFF` if `len2` is negative.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::{adler32, adler32_combine};
///
/// let data1 = b"Hello, ";
/// let data2 = b"World!";
/// let a1 = adler32(1, data1);
/// let a2 = adler32(1, data2);
/// let combined = adler32_combine(a1, a2, data2.len() as i64);
/// assert_eq!(combined, adler32(1, b"Hello, World!"));
/// ```
#[inline]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

/// Combine two Adler-32 checksums for concatenated data (`i64` length
/// variant).
///
/// Equivalent to C zlib's `adler32_combine64(uLong, uLong, z_off64_t)`
/// (adler32.c lines 162–164).
///
/// In Rust this is identical to [`adler32_combine`] because Rust's `i64`
/// is always 64-bit. The C distinction exists because `z_off_t` may be
/// 32-bit on some platforms while `z_off64_t` is always 64-bit.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::{adler32, adler32_combine64};
///
/// let a1 = adler32(1, b"Hello, ");
/// let a2 = adler32(1, b"World!");
/// let combined = adler32_combine64(a1, a2, 6);
/// assert_eq!(combined, adler32(1, b"Hello, World!"));
/// ```
#[inline]
pub fn adler32_combine64(adler1: u32, adler2: u32, len2: i64) -> u32 {
    adler32_combine_(adler1, adler2, len2)
}

// ---------------------------------------------------------------------------
// Idiomatic Rust struct — Adler32
// ---------------------------------------------------------------------------

/// Adler-32 checksum computer with streaming update support.
///
/// Provides an idiomatic Rust API wrapping the Adler-32 computation.
/// Supports incremental (streaming) checksum computation — call
/// [`update`](Adler32::update) repeatedly with successive chunks and
/// retrieve the cumulative result via [`finalize`](Adler32::finalize).
///
/// The initial Adler-32 value is **1** (not 0), matching C zlib's
/// convention where `adler32(0, Z_NULL, 0)` returns 1.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::adler32::Adler32;
///
/// let mut adler = Adler32::new();
/// adler.update(b"Hello, ");
/// adler.update(b"World!");
///
/// // Streaming result matches single-shot computation
/// let expected = zlib_rs::checksum::adler32::adler32(1, b"Hello, World!");
/// assert_eq!(adler.finalize(), expected);
/// ```
///
/// ```
/// use zlib_rs::checksum::adler32::Adler32;
///
/// // Combine two independent checksums
/// let mut a = Adler32::new();
/// a.update(b"first part");
///
/// let mut b = Adler32::new();
/// b.update(b"second part");
///
/// let combined = a.combine(&b, b"second part".len() as i64);
/// let expected = zlib_rs::checksum::adler32::adler32(1, b"first partsecond part");
/// assert_eq!(combined, expected);
/// ```
#[derive(Debug, Clone)]
pub struct Adler32 {
    /// Current composite Adler-32 value: `(s2 << 16) | s1`.
    value: u32,
}

impl Adler32 {
    /// Creates a new Adler-32 computer with the standard initial value of 1.
    ///
    /// Adler-32 starts at 1 (not 0) — the s1 component is initialized to 1
    /// and s2 to 0, giving a composite value of `0x0000_0001`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    /// let adler = Adler32::new();
    /// assert_eq!(adler.finalize(), 1);
    /// ```
    #[inline]
    pub fn new() -> Self {
        Self { value: 1 }
    }

    /// Creates a new Adler-32 computer with a specified initial value.
    ///
    /// Useful for resuming a previously saved Adler-32 computation.
    ///
    /// # Arguments
    ///
    /// * `adler` — A previously computed Adler-32 value.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    /// let saved = 0x0015_002a_u32;
    /// let adler = Adler32::new_with_initial(saved);
    /// assert_eq!(adler.finalize(), saved);
    /// ```
    #[inline]
    pub fn new_with_initial(adler: u32) -> Self {
        Self { value: adler }
    }

    /// Updates the Adler-32 with additional data.
    ///
    /// May be called repeatedly; the checksum accumulates across calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    /// let mut a = Adler32::new();
    /// a.update(b"abc");
    /// a.update(b"def");
    /// assert_eq!(
    ///     a.finalize(),
    ///     zlib_rs::checksum::adler32::adler32(1, b"abcdef"),
    /// );
    /// ```
    #[inline]
    pub fn update(&mut self, buf: &[u8]) {
        self.value = adler32_z(self.value, buf);
    }

    /// Returns the current Adler-32 value without consuming the computer.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    /// let adler = Adler32::new();
    /// assert_eq!(adler.finalize(), 1);
    /// ```
    #[inline]
    pub fn finalize(&self) -> u32 {
        self.value
    }

    /// Resets the Adler-32 computer to its initial state (value = 1).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    /// let mut adler = Adler32::new();
    /// adler.update(b"some data");
    /// adler.reset();
    /// assert_eq!(adler.finalize(), 1);
    /// ```
    #[inline]
    pub fn reset(&mut self) {
        self.value = 1;
    }

    /// Combines this Adler-32 with another, returning the Adler-32 of the
    /// concatenation of the underlying data sequences.
    ///
    /// * `other` — Adler-32 of the second (appended) sequence.
    /// * `len2`  — Length in bytes of the second sequence.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::adler32::Adler32;
    ///
    /// let mut a = Adler32::new();
    /// a.update(b"Hello, ");
    ///
    /// let mut b = Adler32::new();
    /// b.update(b"World!");
    ///
    /// let combined = a.combine(&b, b"World!".len() as i64);
    /// let full = zlib_rs::checksum::adler32::adler32(1, b"Hello, World!");
    /// assert_eq!(combined, full);
    /// ```
    #[inline]
    pub fn combine(&self, other: &Adler32, len2: i64) -> u32 {
        adler32_combine_(self.value, other.value, len2)
    }
}

impl Default for Adler32 {
    /// Returns a new Adler-32 computer with the standard initial value of 1.
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_value() {
        // Adler-32 of empty buffer with initial 1 returns 1.
        assert_eq!(adler32_z(1, b""), 1);
        assert_eq!(adler32(1, b""), 1);
    }

    #[test]
    fn test_single_byte() {
        // Hand-calculated: s1 = 1 + 0x61 = 98; s2 = 0 + 98 = 98
        // Result = (98 << 16) | 98 = 0x00620062
        let result = adler32(1, b"a");
        assert_eq!(result, 0x00620062);
    }

    #[test]
    fn test_known_vectors() {
        // "abc" → s1 = 1+97+98+99 = 295, s2 = 1 + (1+97) + (1+97+98) + (1+97+98+99) = 786
        // Actually, let's trace carefully:
        // Start: s1=1, s2=0
        // 'a' (97): s1 = 1+97 = 98;   s2 = 0 + 98 = 98
        // 'b' (98): s1 = 98+98 = 196;  s2 = 98 + 196 = 294
        // 'c' (99): s1 = 196+99 = 295; s2 = 294 + 295 = 589
        // Result = (589 << 16) | 295 = 0x024d0127
        assert_eq!(adler32(1, b"abc"), 0x024d0127);
    }

    #[test]
    fn test_incremental_matches_single_shot() {
        let single = adler32(1, b"Hello, World!");
        let partial = adler32(1, b"Hello, ");
        let full = adler32(partial, b"World!");
        assert_eq!(full, single);
    }

    #[test]
    fn test_combine_basic() {
        let data1 = b"Hello, ";
        let data2 = b"World!";
        let a1 = adler32(1, data1);
        let a2 = adler32(1, data2);
        let combined = adler32_combine(a1, a2, data2.len() as i64);
        let expected = adler32(1, b"Hello, World!");
        assert_eq!(combined, expected);
    }

    #[test]
    fn test_combine64_matches_combine() {
        let a1 = adler32(1, b"part1");
        let a2 = adler32(1, b"part2");
        assert_eq!(adler32_combine(a1, a2, 5), adler32_combine64(a1, a2, 5),);
    }

    #[test]
    fn test_combine_negative_len() {
        assert_eq!(adler32_combine(1, 1, -1), 0xffff_ffff);
        assert_eq!(adler32_combine64(1, 1, -100), 0xffff_ffff);
    }

    #[test]
    fn test_struct_streaming() {
        let mut adler = Adler32::new();
        assert_eq!(adler.finalize(), 1);

        adler.update(b"Hello, ");
        adler.update(b"World!");
        let expected = adler32(1, b"Hello, World!");
        assert_eq!(adler.finalize(), expected);
    }

    #[test]
    fn test_struct_reset() {
        let mut adler = Adler32::new();
        adler.update(b"some data");
        assert_ne!(adler.finalize(), 1);
        adler.reset();
        assert_eq!(adler.finalize(), 1);
    }

    #[test]
    fn test_struct_default() {
        let adler = Adler32::default();
        assert_eq!(adler.finalize(), 1);
    }

    #[test]
    fn test_struct_new_with_initial() {
        let adler = Adler32::new_with_initial(0x1234_5678);
        assert_eq!(adler.finalize(), 0x1234_5678);
    }

    #[test]
    fn test_struct_combine() {
        let mut a = Adler32::new();
        a.update(b"Hello, ");
        let mut b = Adler32::new();
        b.update(b"World!");
        let combined = a.combine(&b, b"World!".len() as i64);
        let expected = adler32(1, b"Hello, World!");
        assert_eq!(combined, expected);
    }

    #[test]
    fn test_large_buffer() {
        // Test with a buffer larger than NMAX to exercise the main loop.
        let data = vec![0xABu8; 10000];
        let result = adler32(1, &data);
        // Verify incrementally.
        let mut expected = 1u32;
        for chunk in data.chunks(1000) {
            expected = adler32(expected, chunk);
        }
        assert_eq!(result, expected);
    }

    #[test]
    fn test_all_byte_values() {
        // Compute checksum over all 256 byte values.
        let data: Vec<u8> = (0..=255).collect();
        let result = adler32(1, &data);
        // Verify incrementally byte by byte.
        let mut expected = 1u32;
        for &byte in &data {
            expected = adler32(expected, &[byte]);
        }
        assert_eq!(result, expected);
    }

    #[test]
    fn test_combine_with_zero_length() {
        let a1 = adler32(1, b"test");
        let a2 = adler32(1, b"");
        // Combining with an empty second part: adler2 with initial=1 and
        // len2=0 should return adler1 unchanged.
        let combined = adler32_combine(a1, a2, 0);
        assert_eq!(combined, a1);
    }

    #[test]
    fn test_short_buffers() {
        // Test buffers of length 2 through 15 (short path).
        for len in 2..16 {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let single = adler32(1, &data);
            // Verify via byte-by-byte incremental computation.
            let mut incremental = 1u32;
            for &b in &data {
                incremental = adler32(incremental, &[b]);
            }
            assert_eq!(single, incremental, "failed for len={}", len);
        }
    }

    #[test]
    fn test_exactly_nmax_buffer() {
        // Buffer of exactly NMAX bytes to exercise the boundary.
        let data = vec![42u8; NMAX];
        let result = adler32(1, &data);
        // Incremental verification.
        let mut expected = 1u32;
        for chunk in data.chunks(347) {
            expected = adler32(expected, chunk);
        }
        assert_eq!(result, expected);
    }
}
