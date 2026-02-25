//! CRC-32 checksum computation.
//!
//! Implements the CRC-32 algorithm using the polynomial
//! `x^32+x^26+x^23+x^22+x^16+x^12+x^11+x^10+x^8+x^7+x^5+x^4+x^2+x+1`
//! (reflected representation: `0xEDB88320`).
//!
//! The lookup table is a compile-time `const` array, eliminating the need for
//! the C library's `DYNAMIC_CRC_TABLE` runtime initialization pattern.
//!
//! Provides [`crc32`], [`crc32_z`], [`crc32_combine`], [`crc32_combine_op`],
//! [`crc32_combine_gen`], and [`get_crc_table`] functions matching the C zlib
//! API semantics.
//!
//! Ported from `crc32.c` and `crc32.h` (zlib 1.3.2.1-motley).

// Function names must match the C API exactly, which causes repetition
// with the module name. This is intentional for API compatibility.
#![allow(clippy::module_name_repetitions)]

/// CRC-32 polynomial in reflected (bit-reversed) form, with x^32 implied.
///
/// This is the standard CRC-32 polynomial used in Ethernet, gzip, PNG, etc.:
/// `x^32+x^26+x^23+x^22+x^16+x^12+x^11+x^10+x^8+x^7+x^5+x^4+x^2+x+1`
const POLY: u32 = 0xedb8_8320;

/// Generate the 256-entry CRC-32 lookup table at compile time.
///
/// Each entry `table[i]` is the CRC-32 of the single byte `i`, computed
/// by shifting through all 8 bits with polynomial reduction. This matches
/// the algorithm in `crc32.c` lines 248–252.
const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i: u32 = 0;
    while i < 256 {
        let mut p = i;
        let mut j = 0;
        while j < 8 {
            p = if p & 1 != 0 { (p >> 1) ^ POLY } else { p >> 1 };
            j += 1;
        }
        table[i as usize] = p;
        i += 1;
    }
    table
}

/// Pre-computed CRC-32 lookup table (256 entries).
///
/// Each entry `CRC_TABLE[i]` is the CRC-32 of the single byte `i`.
/// Generated at compile time from the polynomial `POLY = 0xEDB88320`.
const CRC_TABLE: [u32; 256] = make_crc_table();

/// Multiply polynomials `a(x)` and `b(x)` modulo `p(x)` over GF(2),
/// where `p(x)` is the CRC-32 polynomial in reflected form.
///
/// This is a `const fn` so it can be used for compile-time table generation.
/// Port of `multmodp()` from `crc32.c` lines 163–178.
///
/// # Preconditions
///
/// `a` must not be zero.
#[inline]
const fn multmodp(a: u32, b: u32) -> u32 {
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    let mut b = b;
    loop {
        if a & m != 0 {
            p ^= b;
            if a & (m.wrapping_sub(1)) == 0 {
                break;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
    p
}

/// Generate the `x^(2^k)` table at compile time.
///
/// Entry `k` contains `x^(2^k) mod p(x)` for `k = 0..31`.
/// These are computed by repeatedly squaring starting from `x^1`.
///
/// The table wraps at index 32 due to the Frobenius endomorphism:
/// in GF(2^32), `x^(2^32) = x`, so `x^(2^(32+k)) = x^(2^k)`.
/// Port of table generation from `crc32.c` lines 258–262.
const fn make_x2n_table() -> [u32; 32] {
    let mut table = [0u32; 32];
    // x^1 in reflected representation is bit 30 (= 0x40000000)
    let mut p: u32 = 1 << 30;
    table[0] = p;
    let mut n: usize = 1;
    while n < 32 {
        p = multmodp(p, p); // square to get x^(2^n)
        table[n] = p;
        n += 1;
    }
    table
}

/// Pre-computed table of powers of x modulo the CRC-32 polynomial.
///
/// Entry `k` contains `x^(2^k) mod p(x)` for `k = 0..31`.
/// Used by [`crc32_combine`] and related combination operations.
const X2N_TABLE: [u32; 32] = make_x2n_table();

/// Compute `x^(n * 2^k) mod p(x)`.
///
/// Uses the pre-computed [`X2N_TABLE`] for efficient modular exponentiation
/// via binary decomposition of `n`. Port of `x2nmodp()` from `crc32.c`
/// lines 184–195.
///
/// # Preconditions
///
/// `n` must not be negative (caller ensures this).
#[inline]
fn x2nmodp(n: i64, k: u32) -> u32 {
    let mut p: u32 = 1 << 31; // x^0 == 1 in reflected representation
    let mut n = n;
    let mut k = k;
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(X2N_TABLE[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

/// Returns a reference to the CRC-32 lookup table.
///
/// In the Rust implementation, the table is a compile-time constant and
/// always available. This function exists for API compatibility with the
/// C library, where it also served to trigger dynamic table generation.
///
/// The returned array has 256 entries, where entry `i` is the CRC-32
/// of the single byte `i`.
#[must_use]
pub fn get_crc_table() -> &'static [u32; 256] {
    &CRC_TABLE
}

/// Compute the CRC-32 checksum of a byte buffer.
///
/// Takes an initial CRC value and a byte slice, returning the updated
/// CRC-32 checksum. To start a new checksum, pass `0` as the initial CRC.
///
/// Returns `0` if the buffer is empty (matching C zlib behavior for
/// `NULL` input). Port of `crc32_z()` from `crc32.c` lines 626–941
/// (byte-wise table-lookup path at lines 922–940).
///
/// The CRC is pre-conditioned by inverting all bits (XOR with
/// `0xFFFF_FFFF`), then post-conditioned by inverting again, per the
/// standard CRC-32 algorithm.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // index is always 0-255 from & 0xff mask
pub fn crc32_z(crc: u32, buf: &[u8]) -> u32 {
    // Return initial CRC if no data provided (C line 628: NULL check)
    if buf.is_empty() {
        return 0;
    }

    // Pre-condition the CRC by inverting all bits (C line 635)
    let mut crc = !crc;

    // Process bytes using table lookup.
    // Unrolled 8 bytes at a time for performance (matching C lines 923–933),
    // with a simple loop for the remainder (C lines 934–937).
    let mut remaining = buf;

    while remaining.len() >= 8 {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[0])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[1])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[2])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[3])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[4])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[5])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[6])) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(remaining[7])) & 0xff) as usize];
        remaining = &remaining[8..];
    }

    for &byte in remaining {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
    }

    // Post-condition: invert all bits (C line 940)
    !crc
}

/// Compute the CRC-32 of a byte buffer.
///
/// Equivalent to [`crc32_z`] — the separate function exists for API
/// compatibility with the C library where the length parameter type
/// differs (`uInt` vs `z_size_t`). In Rust, slices carry their own
/// length, so this is a convenience alias.
#[must_use]
pub fn crc32(crc: u32, buf: &[u8]) -> u32 {
    crc32_z(crc, buf)
}

/// Pre-compute the operator for combining CRC-32 values for a second
/// buffer of length `len2` bytes.
///
/// The returned opaque value can be passed to [`crc32_combine_op`] to
/// efficiently combine CRC values when the second buffer length is
/// known in advance.
///
/// Returns `0` if `len2` is negative.
///
/// # Mathematical Basis
///
/// Computes `x^(len2 * 8) mod p(x)` where `p(x)` is the CRC-32
/// polynomial. The factor of 8 converts byte length to bit length.
///
/// Port of `crc32_combine_gen64()` from `crc32.c` lines 954–966.
#[must_use]
pub fn crc32_combine_gen(len2: i64) -> u32 {
    if len2 < 0 {
        return 0;
    }
    x2nmodp(len2, 3)
}

/// Combine two CRC-32 values using a pre-computed operator from
/// [`crc32_combine_gen`].
///
/// Given `crc1` of buffer A and `crc2` of buffer B, returns the CRC-32
/// of the concatenation A+B, where `op` was computed for `len(B)`.
///
/// Returns `0` if `op` is zero (indicating an invalid or zero-length
/// combine operation).
///
/// Port of `crc32_combine_op()` from `crc32.c` lines 969–973.
#[must_use]
pub fn crc32_combine_op(crc1: u32, crc2: u32, op: u32) -> u32 {
    if op == 0 {
        return 0;
    }
    multmodp(op, crc1) ^ crc2
}

/// Combine two CRC-32 checksums as if their source data were concatenated.
///
/// Given `crc1` of buffer A and `crc2` of buffer B (of length `len2`),
/// returns the CRC-32 of the concatenation A+B.
///
/// This is equivalent to calling [`crc32_combine_gen`] followed by
/// [`crc32_combine_op`], but in a single step.
///
/// Port of `crc32_combine64()` / `crc32_combine()` from `crc32.c`
/// lines 976–983.
#[must_use]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine_op(crc1, crc2, crc32_combine_gen(len2))
}
