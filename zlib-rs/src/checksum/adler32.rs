//! Adler-32 running checksum (RFC 1950) — safe, no_std, bit-identical to C zlib adler32.c.
//!
//! The Adler-32 checksum is the integrity trailer of the zlib wire format
//! (RFC 1950). It is a pair of 16-bit running sums packed into a single
//! `u32`: the low half (`adler`) is one plus the sum of all input bytes
//! modulo [`BASE`], and the high half (`sum2`) is the sum of every
//! intermediate `adler` value modulo [`BASE`].
//!
//! This module is a faithful, allocation-free port of the upstream C
//! `adler32.c`. It is a **foundational leaf**: it depends on nothing else in
//! the crate and is consumed by the `deflate`, `inflate`, `gz`, and `util`
//! layers. Every function returns the raw recombined `u32` checksum; callers
//! are responsible for serializing it (the zlib Adler-32 trailer is written
//! big-endian) — this module never touches endianness.
//!
//! # Bit-exactness
//!
//! Output is byte-identical to C zlib for every input. The two correctness
//! invariants that must never change are:
//!
//! 1. **Accumulation order** — for each byte: `adler += byte; sum2 += adler;`.
//! 2. **Modulo placement** — the modular reduction by [`BASE`] happens once per
//!    [`NMAX`]-byte block and once for the trailing partial block, exactly as
//!    in the C reference. [`NMAX`] is chosen so the `u32` accumulators provably
//!    cannot overflow within a block, so plain (checked, non-wrapping) addition
//!    is correct and never panics in debug builds.
//!
//! # Safety / `no_std`
//!
//! The crate declares `#![forbid(unsafe_code)]` and is `no_std`-capable, so
//! this file contains no `unsafe`, performs no heap allocation, and uses only
//! `core` integer arithmetic and slice iteration — no `std`/`alloc` imports.

/// Largest prime smaller than 65536. Both Adler-32 component sums are reduced
/// modulo this value (`#define BASE 65521U` in C `adler32.c`).
const BASE: u32 = 65521;

/// Largest number of bytes that can be accumulated into the `u32` sums between
/// two modular reductions without overflowing — i.e. the largest `n` such that
/// `255*n*(n+1)/2 + (n+1)*(BASE-1) <= 2^32-1` (`#define NMAX 5552` in C).
///
/// `NMAX` is divisible by 16, mirroring the C `DO16` unrolling.
const NMAX: usize = 5552;

/// Accumulate every byte of `buf` into the running `(adler, sum2)` component
/// sums and return the updated pair.
///
/// No modular reduction is performed here; the caller reduces modulo [`BASE`]
/// at the correct [`NMAX`] block boundaries so the `u32` accumulators cannot
/// overflow. Bytes are visited in order, sixteen at a time (mirroring the C
/// `DO16` macro) followed by the trailing remainder. The 16-byte grouping is
/// purely an iteration detail: it does not affect the result, because every
/// byte is still visited exactly once, in order, performing the canonical
/// `adler += byte; sum2 += adler` step.
#[inline]
fn accumulate(mut adler: u32, mut sum2: u32, buf: &[u8]) -> (u32, u32) {
    let mut chunks = buf.chunks_exact(16);
    for chunk in chunks.by_ref() {
        for &byte in chunk {
            adler += u32::from(byte);
            sum2 += adler;
        }
    }
    for &byte in chunks.remainder() {
        adler += u32::from(byte);
        sum2 += adler;
    }
    (adler, sum2)
}

/// Update a running Adler-32 checksum with the bytes in `buf` and return the
/// new checksum.
///
/// This is the safe-slice port of C `adler32_z`. To start a fresh checksum,
/// pass `1` as `adler` (the RFC 1950 initial value); an empty `buf` returns
/// `adler` unchanged. The C `len == 1` and `buf == Z_NULL` fast paths are
/// intentionally omitted: they are an FFI-boundary micro-optimization that
/// produces results identical to the general path for a slice (the
/// `NULL → return 1` case is handled in the `libz-rs-sys` shim).
///
/// # Examples
///
/// The running checksum is associative: feeding two slices sequentially yields
/// the same result as feeding their concatenation in one call.
///
/// ```
/// use zlib_rs::adler32_z;
/// let whole = adler32_z(1, b"hello, world");
/// let split = adler32_z(adler32_z(1, b"hello, "), b"world");
/// assert_eq!(whole, split);
/// ```
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Split the incoming Adler-32 into its two 16-bit component sums.
    let mut sum2 = (adler >> 16) & 0xffff;
    let mut adler = adler & 0xffff;

    // Short input: accumulate directly and reduce once at the end. `adler`
    // grows by at most 255 per byte across fewer than 16 bytes, so it can
    // exceed `BASE` by at most one multiple — a single conditional
    // subtraction suffices, while `sum2` needs one modulo.
    if buf.len() < 16 {
        (adler, sum2) = accumulate(adler, sum2, buf);
        if adler >= BASE {
            adler -= BASE;
        }
        sum2 %= BASE; // only added so many BASE's
        return adler | (sum2 << 16);
    }

    // Walk the input in NMAX-sized blocks. NMAX is the largest run length for
    // which the u32 accumulators provably cannot overflow, so exactly one
    // modulo per block keeps them bounded.
    let mut remaining = buf;
    while remaining.len() >= NMAX {
        let (block, rest) = remaining.split_at(NMAX);
        (adler, sum2) = accumulate(adler, sum2, block);
        adler %= BASE;
        sum2 %= BASE;
        remaining = rest;
    }

    // Final partial block (fewer than NMAX bytes): still a single modulo.
    if !remaining.is_empty() {
        (adler, sum2) = accumulate(adler, sum2, remaining);
        adler %= BASE;
        sum2 %= BASE;
    }

    // Recombine the two component sums into the packed checksum.
    adler | (sum2 << 16)
}

/// Update a running Adler-32 checksum with the bytes in `buf` and return the
/// new checksum.
///
/// Thin wrapper that mirrors C `adler32`, which simply forwards to
/// `adler32_z`. Both entry points exist for API/FFI parity: C distinguishes a
/// `uInt` length (`adler32`) from a `z_size_t` length (`adler32_z`), whereas in
/// Rust both take a slice. To begin a new checksum, pass `1` as `adler`.
#[inline]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

/// Combine two Adler-32 checksums into one, as if the data behind `adler2`
/// (whose length is `len2` bytes) had been appended to the data behind
/// `adler1`.
///
/// This is a faithful port of C `adler32_combine_`, to which both
/// `adler32_combine` (32-bit length) and `adler32_combine64` (64-bit length)
/// delegate. The C "negative length → `0xFFFFFFFF`" debugging sentinel is a
/// signed `z_off_t` concern handled at the FFI boundary; the safe core takes an
/// unsigned `u64` length, so the single signature serves both C entry points.
///
/// The inputs are assumed to be valid Adler-32 checksums (each 16-bit component
/// already reduced modulo [`BASE`]), matching the C contract.
///
/// # Examples
///
/// Combining the checksums of two slices equals the checksum of their
/// concatenation.
///
/// ```
/// use zlib_rs::{adler32, adler32_combine};
/// let a = adler32(1, b"hello, ");
/// let b = adler32(1, b"world");
/// let combined = adler32_combine(a, b, b"world".len() as u64);
/// assert_eq!(combined, adler32(1, b"hello, world"));
/// ```
pub fn adler32_combine(adler1: u32, adler2: u32, len2: u64) -> u32 {
    // The amount of data covered by `adler2`, reduced modulo BASE.
    let rem = (len2 % u64::from(BASE)) as u32;

    let mut sum1 = adler1 & 0xffff;
    // `rem * sum1` can reach ~65520 * 65520, which overflows u32, so compute
    // the product in u64 and reduce modulo BASE before narrowing back.
    let mut sum2 = ((u64::from(rem) * u64::from(sum1)) % u64::from(BASE)) as u32;

    sum1 += (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Reduce without division: `sum1` can need up to two subtractions of BASE,
    // and `sum2` up to one subtraction of `2*BASE` then one of `BASE`.
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

#[cfg(test)]
mod tests {
    use super::{BASE, NMAX, adler32, adler32_combine, adler32_z};

    /// The two compile-time constants must match the C `adler32.c` definitions
    /// exactly, and `NMAX` must be a multiple of 16 (the `DO16` unroll width).
    #[test]
    fn constants_match_c_reference() {
        assert_eq!(BASE, 65521, "BASE must be the largest prime below 65536");
        assert_eq!(NMAX, 5552, "NMAX must match the C reference");
        assert_eq!(NMAX % 16, 0, "NMAX must be divisible by 16");
    }

    /// Known-answer vectors verified byte-for-byte against canonical C zlib.
    #[test]
    fn known_answer_vectors() {
        // Initial value (1) and empty input: the checksum is returned unchanged.
        assert_eq!(adler32(1, b""), 1);
        assert_eq!(adler32_z(1, b""), 1);

        // Canonical small vectors.
        assert_eq!(adler32(1, b"a"), 0x0062_0062);
        assert_eq!(adler32(1, b"abc"), 0x024D_0127);
        assert_eq!(adler32(1, b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(1, b"hello, world"), 0x1D54_0489);
        assert_eq!(adler32(1, b"\x00"), 0x0001_0001);

        // Exactly sixteen 0xFF bytes exercises the `len >= 16` boundary.
        assert_eq!(adler32(1, &[0xFFu8; 16]), 0x8788_0FF1);
    }

    /// `adler32` is a thin wrapper around `adler32_z`; both the short (`< 16`)
    /// and the unrolled (`>= 16`) paths must agree, and a non-1 continuation
    /// seed must compose associatively.
    #[test]
    fn wrapper_matches_z_and_paths_compose() {
        // Mix of lengths straddling the 16-byte short/long path boundary.
        let cases: [&[u8]; 6] = [
            b"",
            b"x",
            b"short",
            b"fifteen bytes!!",  // 15 bytes -> short (< 16) path
            b"exactly16bytes!!", // 16 bytes -> unrolled (>= 16) path
            b"a slightly longer than sixteen bytes input",
        ];
        for buf in cases {
            assert_eq!(adler32(1, buf), adler32_z(1, buf));
        }

        // Continuation: checksum of the first part fed as the seed for the
        // second part equals the checksum of the concatenation.
        let part = adler32(1, b"fifteen bytes!!"); // short path, canonical result
        assert_eq!(adler32(part, b"X"), adler32(1, b"fifteen bytes!!X"));
    }

    /// Exercise the full `NMAX` block path plus the trailing partial block
    /// entirely on the stack (no heap), so this also holds under `no_std`.
    #[test]
    fn long_input_nmax_block_no_heap() {
        // 6000 bytes (> NMAX = 5552): one full NMAX block followed by a tail.
        let mut data = [0u8; 6000];
        for (i, slot) in data.iter_mut().enumerate() {
            *slot = i as u8; // deterministic pattern: byte = i mod 256
        }

        // Absolute value pinned against canonical C zlib.
        assert_eq!(adler32(1, &data), 0x5AF3_8D6E);

        // Associativity across the NMAX boundary: streaming in two pieces
        // equals a single pass, for split points before, at, and after NMAX.
        let single = adler32(1, &data);
        for k in [0usize, 1, 16, 3000, 5551, 5552, 5553, 6000] {
            let streamed = adler32(adler32(1, &data[..k]), &data[k..]);
            assert_eq!(streamed, single, "split at {k} must match single-shot");
        }
    }

    /// `adler32_combine` must equal the checksum of the concatenated data,
    /// using only string literals so this test needs no heap (`no_std`-safe).
    #[test]
    fn combine_matches_concatenation_literals() {
        // combine(adler(A), adler(B), |B|) == adler(A ++ B).
        assert_eq!(
            adler32_combine(adler32(1, b"hello, "), adler32(1, b"world"), 5),
            adler32(1, b"hello, world"),
        );
        assert_eq!(
            adler32_combine(adler32(1, b"Wiki"), adler32(1, b"pedia"), 5),
            adler32(1, b"Wikipedia"),
        );

        // Absolute pin against C zlib for the combined "Wikipedia" value.
        assert_eq!(
            adler32_combine(adler32(1, b"Wiki"), adler32(1, b"pedia"), 5),
            0x11E6_0398,
        );

        // Empty second slice: combine returns the first checksum unchanged.
        assert_eq!(
            adler32_combine(adler32(1, b"prefix"), adler32(1, b""), 0),
            adler32(1, b"prefix"),
        );

        // Empty first slice: combine returns the second checksum unchanged.
        assert_eq!(
            adler32_combine(adler32(1, b""), adler32(1, b"data"), 4),
            adler32(1, b"data"),
        );
    }

    /// Large-input streaming and split-point associativity. Uses a heap
    /// `Vec`, so it is gated on the `std` feature per the crate's `no_std`
    /// build contract.
    #[cfg(feature = "std")]
    #[test]
    fn large_input_streams_and_matches_reference() {
        // 100_000 bytes; byte = (i*37 + 11) mod 256 (matches the C-zlib oracle).
        let data: Vec<u8> = (0..100_000u32)
            .map(|i| i.wrapping_mul(37).wrapping_add(11) as u8)
            .collect();

        let single = adler32(1, &data);
        // Absolute value pinned against canonical C zlib.
        assert_eq!(single, 0x9070_97AF);

        // Many split points, including the NMAX boundary and the extremes.
        for k in [
            0usize, 1, 15, 16, 17, 5551, 5552, 5553, 11_104, 50_000, 99_999, 100_000,
        ] {
            let streamed = adler32(adler32(1, &data[..k]), &data[k..]);
            assert_eq!(streamed, single, "split at {k} must match single-shot");
        }
    }

    /// `adler32_combine` against a runtime-built concatenation (heap `Vec`),
    /// gated on `std` for the same reason as above.
    #[cfg(feature = "std")]
    #[test]
    fn combine_matches_runtime_concatenation() {
        let a = b"The quick brown fox ";
        let b = b"jumps over the lazy dog";

        let mut ab = Vec::with_capacity(a.len() + b.len());
        ab.extend_from_slice(a);
        ab.extend_from_slice(b);

        let combined = adler32_combine(adler32(1, a), adler32(1, b), b.len() as u64);
        assert_eq!(combined, adler32(1, &ab));
    }
}
