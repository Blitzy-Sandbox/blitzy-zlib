//! CRC-32 checksum computation module.
//!
//! This module provides CRC-32 (IEEE 802.3) checksum computation, translating
//! the C `crc32.c` and `crc32.h` from the original zlib library to idiomatic Rust.
//!
//! The CRC-32 algorithm uses the polynomial 0xEDB88320 (reflected representation
//! of the standard IEEE 802.3 polynomial). It is used in gzip format headers and
//! trailers per RFC 1952, as well as in many other protocols and file formats.
//!
//! # Features
//!
//! - **SIMD acceleration**: When the `simd` feature is enabled, CRC-32 computation
//!   delegates to the [`crc32fast`] crate for hardware-accelerated computation using
//!   x86 SSE/PCLMULQDQ and AArch64 CRC instructions.
//! - **Software fallback**: When `simd` is disabled, a byte-wise table-lookup
//!   implementation is used with an unrolled inner loop.
//! - **Combine operations**: Efficiently combine CRC-32 checksums of concatenated
//!   data segments using GF(2) polynomial arithmetic.
//! - **Streaming support**: The [`Crc32`] struct provides incremental update
//!   capabilities for streaming data.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::checksum::crc32::{crc32, crc32_z, Crc32};
//!
//! // One-shot CRC-32 computation
//! let checksum = crc32(0, b"Hello, World!");
//! assert_ne!(checksum, 0);
//!
//! // Streaming computation
//! let mut hasher = Crc32::new();
//! hasher.update(b"Hello, ");
//! hasher.update(b"World!");
//! assert_eq!(hasher.finalize(), checksum);
//! ```

// ---------------------------------------------------------------------------
// CRC-32 polynomial constant
// ---------------------------------------------------------------------------

/// CRC-32 polynomial in reflected (reversed) representation (IEEE 802.3).
///
/// The standard polynomial is:
/// x^32+x^26+x^23+x^22+x^16+x^12+x^11+x^10+x^8+x^7+x^5+x^4+x^2+x+1
///
/// In reflected form with x^32 implied, this becomes 0xEDB88320.
const POLY: u32 = 0xedb88320;

// ---------------------------------------------------------------------------
// Byte-wise CRC-32 lookup table (256 entries)
// ---------------------------------------------------------------------------

/// Standard byte-wise CRC-32 lookup table (256 entries).
///
/// Generated from the polynomial 0xEDB88320. Each entry `CRC_TABLE[i]` is the
/// CRC-32 of the single byte value `i`. This table is used by the software
/// fallback implementation when SIMD acceleration is not available.
///
/// These values are copied verbatim from the generated `crc32.h` in the
/// original C zlib source.
#[rustfmt::skip]
const CRC_TABLE: [u32; 256] = [
    0x00000000, 0x77073096, 0xee0e612c, 0x990951ba, 0x076dc419,
    0x706af48f, 0xe963a535, 0x9e6495a3, 0x0edb8832, 0x79dcb8a4,
    0xe0d5e91e, 0x97d2d988, 0x09b64c2b, 0x7eb17cbd, 0xe7b82d07,
    0x90bf1d91, 0x1db71064, 0x6ab020f2, 0xf3b97148, 0x84be41de,
    0x1adad47d, 0x6ddde4eb, 0xf4d4b551, 0x83d385c7, 0x136c9856,
    0x646ba8c0, 0xfd62f97a, 0x8a65c9ec, 0x14015c4f, 0x63066cd9,
    0xfa0f3d63, 0x8d080df5, 0x3b6e20c8, 0x4c69105e, 0xd56041e4,
    0xa2677172, 0x3c03e4d1, 0x4b04d447, 0xd20d85fd, 0xa50ab56b,
    0x35b5a8fa, 0x42b2986c, 0xdbbbc9d6, 0xacbcf940, 0x32d86ce3,
    0x45df5c75, 0xdcd60dcf, 0xabd13d59, 0x26d930ac, 0x51de003a,
    0xc8d75180, 0xbfd06116, 0x21b4f4b5, 0x56b3c423, 0xcfba9599,
    0xb8bda50f, 0x2802b89e, 0x5f058808, 0xc60cd9b2, 0xb10be924,
    0x2f6f7c87, 0x58684c11, 0xc1611dab, 0xb6662d3d, 0x76dc4190,
    0x01db7106, 0x98d220bc, 0xefd5102a, 0x71b18589, 0x06b6b51f,
    0x9fbfe4a5, 0xe8b8d433, 0x7807c9a2, 0x0f00f934, 0x9609a88e,
    0xe10e9818, 0x7f6a0dbb, 0x086d3d2d, 0x91646c97, 0xe6635c01,
    0x6b6b51f4, 0x1c6c6162, 0x856530d8, 0xf262004e, 0x6c0695ed,
    0x1b01a57b, 0x8208f4c1, 0xf50fc457, 0x65b0d9c6, 0x12b7e950,
    0x8bbeb8ea, 0xfcb9887c, 0x62dd1ddf, 0x15da2d49, 0x8cd37cf3,
    0xfbd44c65, 0x4db26158, 0x3ab551ce, 0xa3bc0074, 0xd4bb30e2,
    0x4adfa541, 0x3dd895d7, 0xa4d1c46d, 0xd3d6f4fb, 0x4369e96a,
    0x346ed9fc, 0xad678846, 0xda60b8d0, 0x44042d73, 0x33031de5,
    0xaa0a4c5f, 0xdd0d7cc9, 0x5005713c, 0x270241aa, 0xbe0b1010,
    0xc90c2086, 0x5768b525, 0x206f85b3, 0xb966d409, 0xce61e49f,
    0x5edef90e, 0x29d9c998, 0xb0d09822, 0xc7d7a8b4, 0x59b33d17,
    0x2eb40d81, 0xb7bd5c3b, 0xc0ba6cad, 0xedb88320, 0x9abfb3b6,
    0x03b6e20c, 0x74b1d29a, 0xead54739, 0x9dd277af, 0x04db2615,
    0x73dc1683, 0xe3630b12, 0x94643b84, 0x0d6d6a3e, 0x7a6a5aa8,
    0xe40ecf0b, 0x9309ff9d, 0x0a00ae27, 0x7d079eb1, 0xf00f9344,
    0x8708a3d2, 0x1e01f268, 0x6906c2fe, 0xf762575d, 0x806567cb,
    0x196c3671, 0x6e6b06e7, 0xfed41b76, 0x89d32be0, 0x10da7a5a,
    0x67dd4acc, 0xf9b9df6f, 0x8ebeeff9, 0x17b7be43, 0x60b08ed5,
    0xd6d6a3e8, 0xa1d1937e, 0x38d8c2c4, 0x4fdff252, 0xd1bb67f1,
    0xa6bc5767, 0x3fb506dd, 0x48b2364b, 0xd80d2bda, 0xaf0a1b4c,
    0x36034af6, 0x41047a60, 0xdf60efc3, 0xa867df55, 0x316e8eef,
    0x4669be79, 0xcb61b38c, 0xbc66831a, 0x256fd2a0, 0x5268e236,
    0xcc0c7795, 0xbb0b4703, 0x220216b9, 0x5505262f, 0xc5ba3bbe,
    0xb2bd0b28, 0x2bb45a92, 0x5cb36a04, 0xc2d7ffa7, 0xb5d0cf31,
    0x2cd99e8b, 0x5bdeae1d, 0x9b64c2b0, 0xec63f226, 0x756aa39c,
    0x026d930a, 0x9c0906a9, 0xeb0e363f, 0x72076785, 0x05005713,
    0x95bf4a82, 0xe2b87a14, 0x7bb12bae, 0x0cb61b38, 0x92d28e9b,
    0xe5d5be0d, 0x7cdcefb7, 0x0bdbdf21, 0x86d3d2d4, 0xf1d4e242,
    0x68ddb3f8, 0x1fda836e, 0x81be16cd, 0xf6b9265b, 0x6fb077e1,
    0x18b74777, 0x88085ae6, 0xff0f6a70, 0x66063bca, 0x11010b5c,
    0x8f659eff, 0xf862ae69, 0x616bffd3, 0x166ccf45, 0xa00ae278,
    0xd70dd2ee, 0x4e048354, 0x3903b3c2, 0xa7672661, 0xd06016f7,
    0x4969474d, 0x3e6e77db, 0xaed16a4a, 0xd9d65adc, 0x40df0b66,
    0x37d83bf0, 0xa9bcae53, 0xdebb9ec5, 0x47b2cf7f, 0x30b5ffe9,
    0xbdbdf21c, 0xcabac28a, 0x53b39330, 0x24b4a3a6, 0xbad03605,
    0xcdd70693, 0x54de5729, 0x23d967bf, 0xb3667a2e, 0xc4614ab8,
    0x5d681b02, 0x2a6f2b94, 0xb40bbe37, 0xc30c8ea1, 0x5a05df1b,
    0x2d02ef8d,
];

// ---------------------------------------------------------------------------
// Powers-of-x table for CRC combine operations (32 entries)
// ---------------------------------------------------------------------------

/// Table of powers of x for combining CRC-32 checksums.
///
/// `X2N_TABLE[k]` = x^(2^k) mod p(x), where p(x) is the CRC-32 polynomial.
/// Used by [`crc32_combine`] and related functions to efficiently combine
/// CRC-32 checksums of concatenated data segments via GF(2) polynomial
/// multiplication.
///
/// These values are copied verbatim from `crc32.h` lines 9439–9446 in the
/// original C zlib source.
#[rustfmt::skip]
const X2N_TABLE: [u32; 32] = [
    0x40000000, 0x20000000, 0x08000000, 0x00800000, 0x00008000,
    0xedb88320, 0xb1e6b092, 0xa06a2517, 0xed627dae, 0x88d14467,
    0xd7bbfe6a, 0xec447f11, 0x8e7ea170, 0x6427800e, 0x4d47bae0,
    0x09fe548f, 0x83852d0f, 0x30362f1a, 0x7b5a9cc3, 0x31fec169,
    0x9fec022a, 0x6c8dedc4, 0x15d6874d, 0x5fde7a4e, 0xbad90e37,
    0x2e4e5eef, 0x4eaba214, 0xa8a472c0, 0x429a969e, 0x148d302a,
    0xc40ba6d0, 0xc4e22c3c,
];

// ---------------------------------------------------------------------------
// GF(2) polynomial arithmetic
// ---------------------------------------------------------------------------

/// Multiply `a(x)` by `b(x)` modulo `p(x)` over GF(2), where `p(x)` is the
/// CRC-32 polynomial in reflected representation.
///
/// Both `a` and `b` are in reflected representation. `a` must not be zero.
///
/// Translation of C `multmodp(uLong a, uLong b)` from `crc32.c` lines 163–178.
///
/// The algorithm iterates from the MSB of `a` downward: for each set bit,
/// it XORs the current value of `b` into the accumulator `p`. After checking
/// each bit, `b` is shifted right by one (i.e. multiplied by x), with an XOR
/// of the polynomial if the low bit was set (reflecting the modular reduction).
fn multmodp(a: u32, mut b: u32) -> u32 {
    let mut m: u32 = 1u32 << 31;
    let mut p: u32 = 0;
    loop {
        if a & m != 0 {
            p ^= b;
            if a & m.wrapping_sub(1) == 0 {
                break;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
    p
}

/// Return x^(n × 2^k) modulo p(x), using the pre-computed [`X2N_TABLE`].
///
/// `n` must be non-negative (the function handles this via the signed type
/// to match the C API where `z_off64_t` is signed). `k` is the starting
/// power-of-two exponent index into `X2N_TABLE`.
///
/// Translation of C `x2nmodp(z_off64_t n, unsigned k)` from `crc32.c`
/// lines 184–195.
fn x2nmodp(mut n: i64, mut k: u32) -> u32 {
    // x^0 == 1 in reflected representation
    let mut p: u32 = 1 << 31;
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(X2N_TABLE[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

// ---------------------------------------------------------------------------
// Software (non-SIMD) CRC-32 computation
// ---------------------------------------------------------------------------

/// Software CRC-32 computation using byte-wise table lookup with an unrolled
/// inner loop processing 8 bytes per iteration.
///
/// This is the fallback implementation used when the `simd` feature is
/// disabled. It translates the byte-wise computation path from `crc32.c`
/// lines 922–941.
///
/// The CRC is pre-conditioned (bitwise NOT) before processing and
/// post-conditioned (bitwise NOT) before returning, matching the standard
/// CRC-32 convention.
#[cfg(any(not(feature = "simd"), test))]
fn crc32_software(crc: u32, buf: &[u8]) -> u32 {
    // Pre-condition: invert all bits
    let mut crc = !crc;

    // Process 8 bytes at a time (unrolled inner loop matching C lines 923–933)
    let mut chunks = buf.chunks_exact(8);
    for chunk in &mut chunks {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[0] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[1] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[2] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[3] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[4] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[5] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[6] as u32) & 0xff) as usize];
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ chunk[7] as u32) & 0xff) as usize];
    }

    // Process remaining bytes (C lines 934–937)
    for &byte in chunks.remainder() {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ byte as u32) & 0xff) as usize];
    }

    // Post-condition: invert all bits (C line 940: return crc ^ 0xffffffff)
    !crc
}

// ---------------------------------------------------------------------------
// Primary CRC-32 computation functions
// ---------------------------------------------------------------------------

/// Compute the CRC-32 checksum of `buf`, starting with `crc` as the initial
/// value.
///
/// Equivalent to C zlib's `crc32_z(uLong crc, const Bytef *buf, z_size_t len)`.
///
/// When the `simd` feature is enabled, this delegates to the [`crc32fast`]
/// crate for SIMD-accelerated computation (x86 SSE/PCLMULQDQ and AArch64 CRC
/// instructions). Otherwise, it uses a byte-wise software implementation with
/// the standard [`CRC_TABLE`].
///
/// # Arguments
///
/// * `crc` — Initial CRC value. Use `0` for a new computation, or a
///   previously returned CRC value for incremental updates.
/// * `buf` — Input byte slice to process.
///
/// # Returns
///
/// The updated CRC-32 checksum value.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::crc32_z;
///
/// // Known test vector: CRC-32 of "123456789" is 0xCBF43926
/// assert_eq!(crc32_z(0, b"123456789"), 0xCBF43926);
///
/// // Incremental computation
/// let crc = crc32_z(0, b"1234");
/// let crc = crc32_z(crc, b"56789");
/// assert_eq!(crc, 0xCBF43926);
/// ```
pub fn crc32_z(crc: u32, buf: &[u8]) -> u32 {
    #[cfg(feature = "simd")]
    {
        let mut hasher = crc32fast::Hasher::new_with_initial(crc);
        hasher.update(buf);
        hasher.finalize()
    }

    #[cfg(not(feature = "simd"))]
    {
        crc32_software(crc, buf)
    }
}

/// Compute the CRC-32 checksum with C API-compatible naming.
///
/// Equivalent to C zlib's `crc32(uLong crc, const Bytef *buf, uInt len)`.
/// This is a thin wrapper around [`crc32_z`] for API naming compatibility
/// with the original C zlib library. In Rust, both functions have identical
/// signatures since slices carry their own length.
///
/// # Arguments
///
/// * `crc` — Initial CRC value (use `0` for a new computation).
/// * `buf` — Input byte slice to process.
///
/// # Returns
///
/// The updated CRC-32 checksum value.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::crc32;
///
/// let data = b"Hello, World!";
/// let checksum = crc32(0, data);
/// assert_ne!(checksum, 0);
///
/// // Empty input returns the initial CRC unchanged
/// assert_eq!(crc32(0, b""), 0);
/// ```
#[inline]
pub fn crc32(crc: u32, buf: &[u8]) -> u32 {
    crc32_z(crc, buf)
}

// ---------------------------------------------------------------------------
// CRC-32 combine operations
// ---------------------------------------------------------------------------

/// Pre-compute an operator for [`crc32_combine_op`] at a fixed length.
///
/// Equivalent to C zlib's `crc32_combine_gen64(z_off64_t len2)`.
///
/// Returns a pre-computed operator that can be used with [`crc32_combine_op`]
/// to efficiently combine CRC-32 checksums when the second segment length is
/// known in advance and reused across multiple combine operations. This avoids
/// recomputing the GF(2) exponentiation on every call.
///
/// Returns `0` if `len2` is negative.
///
/// # Arguments
///
/// * `len2` — Length of the second data segment in bytes.
///
/// # Returns
///
/// A pre-computed operator value, or `0` if `len2` is negative.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::{crc32, crc32_combine_gen64, crc32_combine_op};
///
/// let data1 = b"Hello, ";
/// let data2 = b"World!";
/// let crc1 = crc32(0, data1);
/// let crc2 = crc32(0, data2);
/// let op = crc32_combine_gen64(data2.len() as i64);
/// let combined = crc32_combine_op(crc1, crc2, op);
/// assert_eq!(combined, crc32(0, b"Hello, World!"));
/// ```
pub fn crc32_combine_gen64(len2: i64) -> u32 {
    if len2 < 0 {
        return 0;
    }
    x2nmodp(len2, 3)
}

/// Pre-compute an operator for [`crc32_combine_op`] (i64 length variant).
///
/// Equivalent to C zlib's `crc32_combine_gen(z_off_t len2)`.
/// This is identical to [`crc32_combine_gen64`] since Rust uses `i64` for
/// both `z_off_t` and `z_off64_t`.
///
/// # Arguments
///
/// * `len2` — Length of the second data segment in bytes.
///
/// # Returns
///
/// A pre-computed operator value, or `0` if `len2` is negative.
#[inline]
pub fn crc32_combine_gen(len2: i64) -> u32 {
    crc32_combine_gen64(len2)
}

/// Combine two CRC-32 checksums using a pre-computed operator.
///
/// Equivalent to C zlib's `crc32_combine_op(uLong crc1, uLong crc2, uLong op)`.
///
/// The operator `op` must have been generated by [`crc32_combine_gen`] or
/// [`crc32_combine_gen64`]. This function is faster than [`crc32_combine`]
/// when the same second-segment length is used repeatedly, since the operator
/// only needs to be computed once.
///
/// Returns `0` if `op` is `0` (indicating a negative length was passed to
/// `crc32_combine_gen`).
///
/// # Arguments
///
/// * `crc1` — CRC-32 of the first data segment.
/// * `crc2` — CRC-32 of the second data segment.
/// * `op` — Pre-computed operator from [`crc32_combine_gen`].
///
/// # Returns
///
/// CRC-32 of the concatenated data.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::{crc32, crc32_combine_gen, crc32_combine_op};
///
/// let crc1 = crc32(0, b"ABC");
/// let crc2 = crc32(0, b"DEF");
/// let op = crc32_combine_gen(3);
/// let combined = crc32_combine_op(crc1, crc2, op);
/// assert_eq!(combined, crc32(0, b"ABCDEF"));
/// ```
pub fn crc32_combine_op(crc1: u32, crc2: u32, op: u32) -> u32 {
    if op == 0 {
        return 0;
    }
    multmodp(op, crc1) ^ crc2
}

/// Combine two CRC-32 checksums for concatenated data.
///
/// Equivalent to C zlib's `crc32_combine64(uLong crc1, uLong crc2, z_off64_t len2)`.
///
/// Given the CRC-32 of two byte sequences, returns the CRC-32 of their
/// concatenation. `crc1` is the CRC of the first sequence, `crc2` is the CRC
/// of the second sequence, and `len2` is the length of the second sequence in
/// bytes.
///
/// This uses GF(2) polynomial arithmetic: the combined CRC equals
/// `crc1 × x^(8×len2) ⊕ crc2` modulo the CRC polynomial.
///
/// # Arguments
///
/// * `crc1` — CRC-32 of the first data segment.
/// * `crc2` — CRC-32 of the second data segment.
/// * `len2` — Length of the second data segment in bytes.
///
/// # Returns
///
/// CRC-32 of the concatenated data, or `0` if `len2` is negative.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::{crc32, crc32_combine64};
///
/// let part1 = b"Hello, ";
/// let part2 = b"World!";
/// let crc1 = crc32(0, part1);
/// let crc2 = crc32(0, part2);
/// let combined = crc32_combine64(crc1, crc2, part2.len() as i64);
/// assert_eq!(combined, crc32(0, b"Hello, World!"));
/// ```
pub fn crc32_combine64(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine_op(crc1, crc2, crc32_combine_gen64(len2))
}

/// Combine two CRC-32 checksums for concatenated data (i64 length variant).
///
/// Equivalent to C zlib's `crc32_combine(uLong crc1, uLong crc2, z_off_t len2)`.
/// This is identical to [`crc32_combine64`] since Rust uses `i64` for both
/// `z_off_t` and `z_off64_t`.
///
/// # Arguments
///
/// * `crc1` — CRC-32 of the first data segment.
/// * `crc2` — CRC-32 of the second data segment.
/// * `len2` — Length of the second data segment in bytes.
///
/// # Returns
///
/// CRC-32 of the concatenated data, or `0` if `len2` is negative.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::{crc32, crc32_combine};
///
/// let crc1 = crc32(0, b"Hello");
/// let crc2 = crc32(0, b"World");
/// let combined = crc32_combine(crc1, crc2, 5);
/// assert_eq!(combined, crc32(0, b"HelloWorld"));
/// ```
#[inline]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine64(crc1, crc2, len2)
}

// ---------------------------------------------------------------------------
// CRC table access
// ---------------------------------------------------------------------------

/// Returns a reference to the CRC-32 lookup table (256 entries).
///
/// Equivalent to C zlib's `get_crc_table(void)`.
///
/// In the original C implementation, this function serves to force table
/// generation in threaded applications. In Rust, the table is a compile-time
/// constant, so no initialization is needed — this simply returns a reference
/// to the static [`CRC_TABLE`] array.
///
/// This can be used by external code that needs access to the raw CRC-32
/// table values.
///
/// # Returns
///
/// A `&'static` reference to the 256-entry CRC-32 lookup table.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::get_crc_table;
///
/// let table = get_crc_table();
/// assert_eq!(table.len(), 256);
/// // First entry is always 0 (CRC of byte 0x00)
/// assert_eq!(table[0], 0x00000000);
/// // Last entry
/// assert_eq!(table[255], 0x2d02ef8d);
/// ```
pub fn get_crc_table() -> &'static [u32; 256] {
    &CRC_TABLE
}

// ---------------------------------------------------------------------------
// Idiomatic Rust Crc32 struct
// ---------------------------------------------------------------------------

/// CRC-32 checksum computer with streaming update support.
///
/// Provides an idiomatic Rust API wrapping the CRC-32 computation. Supports
/// incremental (streaming) checksum computation, allowing data to be fed in
/// arbitrarily sized chunks.
///
/// # Examples
///
/// ```
/// use zlib_rs::checksum::crc32::Crc32;
///
/// // Basic usage
/// let mut crc = Crc32::new();
/// crc.update(b"Hello, ");
/// crc.update(b"World!");
///
/// // The streamed result matches one-shot computation
/// use zlib_rs::checksum::crc32::crc32;
/// assert_eq!(crc.finalize(), crc32(0, b"Hello, World!"));
/// ```
///
/// ```
/// use zlib_rs::checksum::crc32::Crc32;
///
/// // With initial value
/// let mut crc = Crc32::new_with_initial(0x12345678);
/// crc.update(b"data");
/// let result = crc.finalize();
///
/// // Reset and reuse
/// let mut crc = Crc32::new();
/// crc.update(b"first");
/// crc.reset();
/// crc.update(b"second");
/// // Only "second" contributes to the final CRC
/// assert_eq!(crc.finalize(), zlib_rs::checksum::crc32::crc32(0, b"second"));
/// ```
#[derive(Debug, Clone)]
pub struct Crc32 {
    /// Current running CRC-32 value.
    value: u32,
}

impl Crc32 {
    /// Creates a new CRC-32 computer with initial value `0`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::Crc32;
    /// let crc = Crc32::new();
    /// assert_eq!(crc.finalize(), 0);
    /// ```
    #[inline]
    pub fn new() -> Self {
        Self { value: 0 }
    }

    /// Creates a new CRC-32 computer with a specified initial CRC value.
    ///
    /// This is useful for continuing a CRC-32 computation that was previously
    /// interrupted, or for testing with non-zero initial values.
    ///
    /// # Arguments
    ///
    /// * `crc` — The initial CRC-32 value.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::Crc32;
    /// let crc = Crc32::new_with_initial(0xDEADBEEF);
    /// assert_eq!(crc.finalize(), 0xDEADBEEF);
    /// ```
    #[inline]
    pub fn new_with_initial(crc: u32) -> Self {
        Self { value: crc }
    }

    /// Updates the running CRC-32 with additional input data.
    ///
    /// This method can be called repeatedly to compute the CRC-32 of a data
    /// stream incrementally.
    ///
    /// # Arguments
    ///
    /// * `buf` — Input byte slice to incorporate into the CRC.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::Crc32;
    /// let mut crc = Crc32::new();
    /// crc.update(b"Hello");
    /// crc.update(b", World!");
    /// assert_ne!(crc.finalize(), 0);
    /// ```
    #[inline]
    pub fn update(&mut self, buf: &[u8]) {
        self.value = crc32_z(self.value, buf);
    }

    /// Returns the current CRC-32 value without consuming the computer.
    ///
    /// The computer can continue to be updated after calling `finalize`.
    ///
    /// # Returns
    ///
    /// The current CRC-32 checksum value.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::Crc32;
    /// let mut crc = Crc32::new();
    /// crc.update(b"123456789");
    /// assert_eq!(crc.finalize(), 0xCBF43926);
    /// ```
    #[inline]
    pub fn finalize(&self) -> u32 {
        self.value
    }

    /// Resets the CRC-32 computer to initial state (value `0`).
    ///
    /// After calling `reset`, the computer behaves as if newly created with
    /// [`Crc32::new()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::Crc32;
    /// let mut crc = Crc32::new();
    /// crc.update(b"data");
    /// crc.reset();
    /// assert_eq!(crc.finalize(), 0);
    /// ```
    #[inline]
    pub fn reset(&mut self) {
        self.value = 0;
    }

    /// Combines this CRC with another CRC for the concatenation of the
    /// underlying data.
    ///
    /// Given two CRC-32 computers that have processed separate byte segments,
    /// this returns the CRC-32 that would result from processing the
    /// concatenation of both segments.
    ///
    /// # Arguments
    ///
    /// * `other` — The CRC-32 computer for the second data segment.
    /// * `len2` — The length (in bytes) of the data processed by `other`.
    ///
    /// # Returns
    ///
    /// The CRC-32 of the concatenated data.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::checksum::crc32::{crc32, Crc32};
    ///
    /// let mut crc1 = Crc32::new();
    /// crc1.update(b"Hello, ");
    /// let mut crc2 = Crc32::new();
    /// crc2.update(b"World!");
    /// let combined = crc1.combine(&crc2, 6);
    /// assert_eq!(combined, crc32(0, b"Hello, World!"));
    /// ```
    #[inline]
    pub fn combine(&self, other: &Crc32, len2: i64) -> u32 {
        crc32_combine64(self.value, other.value, len2)
    }
}

impl Default for Crc32 {
    /// Creates a default CRC-32 computer with initial value `0`.
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
    fn test_crc32_empty() {
        // CRC-32 of empty data with initial CRC 0 should return 0.
        assert_eq!(crc32(0, b""), 0);
        assert_eq!(crc32_z(0, b""), 0);
    }

    #[test]
    fn test_crc32_known_vector() {
        // Standard test vector: CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(crc32(0, b"123456789"), 0xCBF43926);
        assert_eq!(crc32_z(0, b"123456789"), 0xCBF43926);
    }

    #[test]
    fn test_crc32_incremental() {
        // Incremental computation must match one-shot.
        let full = crc32(0, b"123456789");
        let crc = crc32(0, b"1234");
        let crc = crc32(crc, b"56789");
        assert_eq!(crc, full);
    }

    #[test]
    fn test_crc32_software_known_vector() {
        // Directly test the software fallback path.
        assert_eq!(crc32_software(0, b"123456789"), 0xCBF43926);
    }

    #[test]
    fn test_crc32_software_empty() {
        assert_eq!(crc32_software(0, b""), 0);
    }

    #[test]
    fn test_crc32_combine_basic() {
        let data1 = b"Hello, ";
        let data2 = b"World!";
        let full_crc = crc32(0, b"Hello, World!");
        let crc1 = crc32(0, data1);
        let crc2 = crc32(0, data2);
        let combined = crc32_combine(crc1, crc2, data2.len() as i64);
        assert_eq!(combined, full_crc);
    }

    #[test]
    fn test_crc32_combine64_basic() {
        let crc1 = crc32(0, b"ABC");
        let crc2 = crc32(0, b"DEF");
        let full = crc32(0, b"ABCDEF");
        assert_eq!(crc32_combine64(crc1, crc2, 3), full);
    }

    #[test]
    fn test_crc32_combine_op_matches_combine() {
        let data1 = b"Test";
        let data2 = b"Data";
        let crc1 = crc32(0, data1);
        let crc2 = crc32(0, data2);
        let len2 = data2.len() as i64;

        let via_combine = crc32_combine(crc1, crc2, len2);
        let op = crc32_combine_gen(len2);
        let via_op = crc32_combine_op(crc1, crc2, op);
        assert_eq!(via_combine, via_op);
    }

    #[test]
    fn test_crc32_combine_gen_matches_gen64() {
        assert_eq!(crc32_combine_gen(100), crc32_combine_gen64(100));
        assert_eq!(crc32_combine_gen(0), crc32_combine_gen64(0));
        assert_eq!(crc32_combine_gen(-1), crc32_combine_gen64(-1));
    }

    #[test]
    fn test_crc32_combine_negative_length() {
        // Negative length should return 0 from gen, and 0 from combine_op.
        assert_eq!(crc32_combine_gen64(-1), 0);
        assert_eq!(crc32_combine_op(0x12345678, 0xABCDEF01, 0), 0);
    }

    #[test]
    fn test_crc32_combine_zero_length() {
        // Combining with a zero-length second segment: result should equal crc1 ^ crc2
        // because x^0 mod p(x) in the reflected domain is 1<<31.
        let crc1 = crc32(0, b"Hello");
        let crc2 = crc32(0, b"");
        let combined = crc32_combine(crc1, crc2, 0);
        // crc2 of empty is 0, so combined should equal crc1.
        assert_eq!(combined, crc1);
    }

    #[test]
    fn test_get_crc_table() {
        let table = get_crc_table();
        assert_eq!(table.len(), 256);
        assert_eq!(table[0], 0x00000000);
        assert_eq!(table[255], 0x2d02ef8d);
        // Spot-check a few values from the middle
        assert_eq!(table[128], 0xedb88320); // CRC_TABLE[128] = POLY
    }

    #[test]
    fn test_crc_table_size() {
        assert_eq!(CRC_TABLE.len(), 256);
        assert_eq!(X2N_TABLE.len(), 32);
    }

    #[test]
    fn test_crc32_struct_basic() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finalize(), 0xCBF43926);
    }

    #[test]
    fn test_crc32_struct_streaming() {
        let mut crc = Crc32::new();
        crc.update(b"Hello, ");
        crc.update(b"World!");
        assert_eq!(crc.finalize(), crc32(0, b"Hello, World!"));
    }

    #[test]
    fn test_crc32_struct_with_initial() {
        let crc = Crc32::new_with_initial(0xDEADBEEF);
        assert_eq!(crc.finalize(), 0xDEADBEEF);
    }

    #[test]
    fn test_crc32_struct_reset() {
        let mut crc = Crc32::new();
        crc.update(b"data");
        assert_ne!(crc.finalize(), 0);
        crc.reset();
        assert_eq!(crc.finalize(), 0);
    }

    #[test]
    fn test_crc32_struct_combine() {
        let mut crc1 = Crc32::new();
        crc1.update(b"Hello, ");
        let mut crc2 = Crc32::new();
        crc2.update(b"World!");
        let combined = crc1.combine(&crc2, 6);
        assert_eq!(combined, crc32(0, b"Hello, World!"));
    }

    #[test]
    fn test_crc32_struct_default() {
        let crc = Crc32::default();
        assert_eq!(crc.finalize(), 0);
    }

    #[test]
    fn test_crc32_single_byte_values() {
        // CRC of single byte 0x00
        let crc = crc32(0, &[0x00]);
        assert_eq!(crc, 0xD202EF8D);

        // CRC of single byte 0xFF
        let crc = crc32(0, &[0xFF]);
        assert_eq!(crc, 0xFF000000);
    }

    #[test]
    fn test_multmodp_basic() {
        // x2n_table[0] should be 0x40000000 (x^1 mod p(x))
        assert_eq!(X2N_TABLE[0], 0x40000000);
        // x2n_table[5] should be POLY (x^32 mod p(x))
        assert_eq!(X2N_TABLE[5], POLY);
    }

    #[test]
    fn test_crc32_various_lengths() {
        // Test with various data lengths to exercise the unrolled loop
        // and remainder handling.
        for len in 0..=32 {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let crc_one_shot = crc32(0, &data);

            // Incremental byte-by-byte should match
            let mut crc_inc = 0u32;
            for &byte in &data {
                crc_inc = crc32(crc_inc, &[byte]);
            }
            assert_eq!(
                crc_one_shot, crc_inc,
                "Mismatch for length {len}"
            );
        }
    }
}
