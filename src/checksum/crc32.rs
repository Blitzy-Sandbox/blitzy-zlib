//! CRC-32/IEEE checksum computation.
//!
//! This module is a faithful, memory-safe Rust port of the reference zlib
//! `crc32.c`. It computes the CRC-32 with the reflected polynomial
//! `0xEDB88320` (the IEEE 802.3 / gzip CRC) that gzip-format streams
//! (RFC 1952) carry as their trailing 4-byte integrity field, and it is also
//! exposed through the public API because it is useful to applications on its
//! own.
//!
//! The implementation contains **no `unsafe`** code and depends only on
//! `core`, so it is usable in `no_std` builds. Its output is bit-identical to
//! reference zlib for every input, which is the defining acceptance criterion
//! for the migration.
//!
//! # Computation paths
//!
//! Two interchangeable bulk-CRC paths are provided, selected at compile time by
//! the `simd` Cargo feature; both produce identical output for every input:
//!
//! * **`simd` (enabled by default)** delegates the hot loop to the `crc32fast`
//!   crate, which uses hardware-accelerated carry-less multiplication where it
//!   is available. `crc32fast` follows the same external CRC convention as zlib
//!   (pre- and post-conditioning performed internally), so it is a bit-exact
//!   drop-in for the byte-wise algorithm.
//! * **The scalar fallback** (`--no-default-features`, and every `no_std`
//!   build) is the classic reflected, byte-wise table lookup driven by the
//!   generated `CRC_TABLE`. It is the guaranteed-correct baseline and needs no
//!   external crate.
//!
//! # Combining checksums
//!
//! The `crc32_combine*` family lets the CRC of a concatenation be computed from
//! the CRCs of the parts plus the length of the second part. These routines
//! are ported directly from `crc32.c` (they are *not* delegated to
//! `crc32fast`) so their results match reference zlib exactly and behave
//! identically in `no_std`. They rely on GF(2) polynomial arithmetic
//! (`multmodp` / `x2nmodp`) over the precomputed powers-of-x table `X2N_TABLE`.

// The CRC-32 lookup tables are generated at build time by `build.rs` (a safe
// Rust port of the C `make_crc_table()` routine) and written to
// `${OUT_DIR}/crc32_tables.rs`; the values are bit-identical to the checked-in
// C header `crc32.h`. They are pulled in through a private submodule that
// carries an `#[allow(dead_code)]`: the generated file also defines big-endian
// and braided table variants (plus the `CRC_BRAID_N` / `CRC_BRAID_W`
// constants) that this scalar-plus-SIMD implementation does not consume, and
// the module-level allow keeps every feature combination free of `dead_code`
// warnings without having to edit the generated output.
#[allow(dead_code)]
mod tables {
    include!(concat!(env!("OUT_DIR"), "/crc32_tables.rs"));
}
use tables::{CRC_TABLE, X2N_TABLE};

/// The CRC-32 polynomial, reflected, with the `x^32` term implied.
///
/// This is `0xEDB88320`, the reflected form of the IEEE 802.3 CRC-32
/// polynomial (`POLY` in `crc32.c`). It drives the GF(2) reductions performed
/// by `multmodp`, which in turn back the checksum-combining routines.
const POLY: u32 = 0xedb8_8320;

/// Updates a running CRC-32 with the bytes in `buf` and returns the updated
/// CRC-32.
///
/// A CRC-32 value is in the range of a 32-bit unsigned integer. A fresh
/// checksum is started from `0`; passing an empty slice leaves the running
/// value unchanged, so `crc32(0, b"")` returns `0`.
///
/// Pre- and post-conditioning (one's complement) is performed inside this
/// function, so the application must **not** do it.
///
/// This mirrors the C `crc32(crc, buf, len)` entry point. The C contract of
/// returning the required initial value for a `NULL` buffer is handled at the
/// FFI boundary, where a null pointer can occur; in this idiomatic slice-based
/// API there is no null, so an empty slice simply returns `crc` unchanged.
///
/// # Examples
///
/// ```ignore
/// let mut crc = crc32(0, b"");     // required initial value
/// crc = crc32(crc, b"123456789");  // fold in more data
/// assert_eq!(crc, 0xcbf4_3926);
/// ```
#[must_use]
pub fn crc32(crc: u32, buf: &[u8]) -> u32 {
    crc32_z(crc, buf)
}

/// Updates a running CRC-32 with the bytes in `buf` and returns the updated
/// CRC-32.
///
/// This is identical to [`crc32`] and exists to mirror the C `crc32_z` entry
/// point, which accepts a `size_t` length rather than an `unsigned int`
/// length. In this slice-based API both collapse to `&[u8]`, so [`crc32`]
/// simply delegates here. Both names are retained because the FFI layer
/// exposes them as distinct C symbols.
///
/// Pre- and post-conditioning (one's complement) is performed internally.
#[must_use]
pub fn crc32_z(crc: u32, buf: &[u8]) -> u32 {
    // Dispatch to whichever bulk implementation the active feature set selects.
    // Both implementations are bit-exact equivalents, so callers never observe
    // a difference; only throughput changes.
    crc32_bulk(crc, buf)
}

/// SIMD-accelerated bulk CRC-32 (active when the `simd` feature is enabled).
///
/// Delegates to `crc32fast`, which maintains the same external CRC convention
/// as zlib: `new_with_initial(crc)` seeds the running value, `update` folds in
/// the bytes, and `finalize` applies the trailing conditioning and returns the
/// updated CRC. This yields output identical to the scalar table loop below.
#[cfg(feature = "simd")]
fn crc32_bulk(crc: u32, buf: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(crc);
    hasher.update(buf);
    hasher.finalize()
}

/// Scalar, byte-wise bulk CRC-32 (active when the `simd` feature is disabled,
/// including every `no_std` build).
///
/// This is the classic reflected table algorithm from `crc32.c`: the CRC is
/// pre-conditioned with a one's complement, each input byte is folded through
/// `CRC_TABLE`, and the result is post-conditioned with a second one's
/// complement. It allocates nothing and is the guaranteed-correct baseline.
#[cfg(not(feature = "simd"))]
fn crc32_bulk(crc: u32, buf: &[u8]) -> u32 {
    // Pre-condition: work on the one's complement of the incoming CRC.
    let mut c = !crc;
    for &byte in buf {
        // Fold one byte at a time using the byte-wise reflected table.
        c = (c >> 8) ^ CRC_TABLE[((c ^ u32::from(byte)) & 0xff) as usize];
    }
    // Post-condition: undo the initial complement.
    !c
}

/// Returns `a(x)` multiplied by `b(x)` modulo `p(x)`, where `p(x)` is the
/// reflected CRC polynomial.
///
/// This is a carry-less (GF(2)) multiply-and-reduce, ported directly from
/// `multmodp()` in `crc32.c`. For speed the C routine requires that `a` is not
/// zero; every call site here upholds that invariant, so the loop is
/// guaranteed to terminate at the lowest set bit of `a`. (A zero `a` would
/// never satisfy the break condition and would loop until `m` underflowed;
/// callers must therefore never pass `a == 0`.)
///
/// All intermediate values fit in 32 bits, so `u32` arithmetic reproduces the
/// C `uLong` computation exactly.
fn multmodp(a: u32, mut b: u32) -> u32 {
    // `m` walks a single set bit from bit 31 down toward the lowest set bit
    // of `a`.
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    loop {
        if (a & m) != 0 {
            p ^= b;
            // Once the lowest set bit of `a` has been consumed, we are done.
            if (a & (m - 1)) == 0 {
                break;
            }
        }
        m >>= 1;
        // Multiply `b` by x modulo p(x): a right shift, reducing on carry-out.
        b = if (b & 1) != 0 {
            (b >> 1) ^ POLY
        } else {
            b >> 1
        };
    }
    p
}

/// Returns `x^(n * 2^k) modulo p(x)`.
///
/// Port of `x2nmodp()` in `crc32.c`, walking the precomputed `X2N_TABLE` of
/// squared powers of x. `n` is accepted as an `i64` to mirror the C
/// `z_off64_t` argument; callers guarantee it is non-negative (the public
/// combine entry points reject negative lengths before reaching here), so the
/// cast to `u64` performs a well-defined logical right shift over the bits of
/// `n`.
fn x2nmodp(n: i64, mut k: u32) -> u32 {
    let mut p: u32 = 1 << 31; // x^0 == 1, the identity element for multmodp.
    let mut n = n as u64;
    while n != 0 {
        if (n & 1) != 0 {
            p = multmodp(X2N_TABLE[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

/// Returns the operator corresponding to a second-sequence length of `len2`,
/// for repeated use with [`crc32_combine_op`].
///
/// `len2` must be non-negative; otherwise zero is returned. Computing the
/// operator once and reusing it across many [`crc32_combine_op`] calls is
/// faster than calling [`crc32_combine`] repeatedly with the same length.
///
/// A single `i64` length covers both the C `crc32_combine_gen` (`z_off_t`) and
/// `crc32_combine_gen64` (`z_off64_t`) entry points.
#[must_use]
pub fn crc32_combine_gen(len2: i64) -> u32 {
    // A negative length has no meaning; match reference zlib and return zero.
    if len2 < 0 {
        return 0;
    }
    x2nmodp(len2, 3)
}

/// Combines two CRC-32 values using a precomputed operator `op` in place of a
/// length.
///
/// Gives the same result as [`crc32_combine`], but takes `op` (produced by
/// [`crc32_combine_gen`]) instead of `len2`. This is faster than
/// [`crc32_combine`] when the same operator is applied more than once.
///
/// A zero operator is never produced for a valid length
/// (`crc32_combine_gen(0)` is `0x8000_0000`), so it is treated defensively as
/// an error and yields zero.
#[must_use]
pub fn crc32_combine_op(crc1: u32, crc2: u32, op: u32) -> u32 {
    if op == 0 {
        return 0;
    }
    multmodp(op, crc1) ^ crc2
}

/// Combines two CRC-32 check values into one.
///
/// Given two byte sequences `seq1` and `seq2` with lengths `len1` and `len2`
/// and CRC-32 values `crc1` and `crc2` respectively, this returns the CRC-32
/// of the concatenation `seq1 || seq2`, requiring only `crc1`, `crc2`, and
/// `len2` — the length of the second sequence.
///
/// `len2` must be non-negative; otherwise zero is returned. A single `i64`
/// length covers both the C `crc32_combine` (`z_off_t`) and `crc32_combine64`
/// (`z_off64_t`) entry points.
#[must_use]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine_op(crc1, crc2, crc32_combine_gen(len2))
}

/// Returns a reference to the 256-entry byte-wise CRC-32 lookup table.
///
/// This mirrors the C `get_crc_table()`, which returns `const z_crc_t *`. The
/// table is a pure mathematical constant derived from the reflected polynomial
/// `POLY` and is generated at build time; applications rarely need it, but it
/// is exposed for parity with the reference API (for example, assembly CRC
/// implementations and consumers that inspect the table directly).
#[must_use]
pub fn get_crc_table() -> &'static [u32; 256] {
    &CRC_TABLE
}

#[cfg(test)]
mod tests {
    use super::{
        crc32, crc32_combine, crc32_combine_gen, crc32_combine_op, crc32_z, get_crc_table,
    };

    // Every expected value below was produced by reference zlib (Python's
    // `zlib.crc32`), the canonical implementation this port must match
    // bit-for-bit. Because the `simd` and scalar paths are interchangeable,
    // these assertions validate whichever path the active feature set compiles.

    #[test]
    fn empty_slice_returns_input_unchanged() {
        // A fresh running CRC starts at 0; an empty update never changes it.
        assert_eq!(crc32(0, b""), 0);
        assert_eq!(crc32(0xdead_beef, b""), 0xdead_beef);
        assert_eq!(crc32_z(0, b""), 0);
    }

    #[test]
    fn known_reference_vectors() {
        // The canonical CRC-32/IEEE check value.
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(0, b"a"), 0xe8b7_be43);
        assert_eq!(crc32(0, b"abc"), 0x3524_41c2);
        assert_eq!(
            crc32(0, b"The quick brown fox jumps over the lazy dog"),
            0x414f_a339
        );
    }

    #[test]
    fn crc32_and_crc32_z_agree() {
        let data = b"The quick brown fox jumps over the lazy dog";
        for len in 0..=data.len() {
            assert_eq!(crc32(0, &data[..len]), crc32_z(0, &data[..len]));
        }
    }

    #[test]
    fn byte_at_a_time_matches_single_pass() {
        // Folding one byte at a time via running updates must equal one bulk
        // call — this exercises the incremental-update contract.
        let data = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut running = 0;
        for &byte in data {
            running = crc32(running, &[byte]);
        }
        assert_eq!(running, crc32(0, data));
    }

    #[test]
    fn split_update_matches_whole() {
        // For any split buf = a || b: crc32(crc32(0, a), b) == crc32(0, buf).
        let whole = b"The quick brown fox jumps over the lazy dog";
        for split in 0..=whole.len() {
            let (a, b) = whole.split_at(split);
            assert_eq!(crc32(crc32(0, a), b), crc32(0, whole));
        }
        assert_eq!(crc32(0, whole), 0x414f_a339);
    }

    #[test]
    fn large_buffer_matches_reference() {
        // 20_000 bytes exercises long inputs (and the block-based SIMD path).
        let mut big = [0u8; 20_000];
        for (i, byte) in big.iter_mut().enumerate() {
            *byte = ((i * 37 + 11) & 0xff) as u8;
        }
        assert_eq!(crc32(0, &big), 0x897e_9e86);
    }

    #[test]
    fn get_crc_table_reference_entries() {
        // Spot-check the generated table against the known reflected values.
        let table = get_crc_table();
        assert_eq!(table[0], 0x0000_0000);
        assert_eq!(table[1], 0x7707_3096);
        assert_eq!(table[2], 0xee0e_612c);
        assert_eq!(table[255], 0x2d02_ef8d);
    }

    #[test]
    fn combine_matches_concatenation() {
        let whole = b"The quick brown fox jumps over the lazy dog";
        let (a, b) = whole.split_at(20);
        let c1 = crc32(0, a);
        let c2 = crc32(0, b);
        assert_eq!(c1, 0x88b0_75e2);
        assert_eq!(c2, 0x1878_6794);
        assert_eq!(crc32_combine(c1, c2, b.len() as i64), crc32(0, whole));
        assert_eq!(crc32_combine(c1, c2, b.len() as i64), 0x414f_a339);
    }

    #[test]
    fn combine_large_matches_concatenation() {
        let mut big = [0u8; 20_000];
        for (i, byte) in big.iter_mut().enumerate() {
            *byte = ((i * 37 + 11) & 0xff) as u8;
        }
        let (a, b) = big.split_at(8_000);
        let combined = crc32_combine(crc32(0, a), crc32(0, b), b.len() as i64);
        assert_eq!(combined, crc32(0, &big));
        assert_eq!(combined, 0x897e_9e86);
    }

    #[test]
    fn combine_op_matches_combine() {
        // A precomputed operator must give the same result as crc32_combine.
        let whole = b"The quick brown fox jumps over the lazy dog";
        let (a, b) = whole.split_at(20);
        let c1 = crc32(0, a);
        let c2 = crc32(0, b);
        let len2 = b.len() as i64;
        let op = crc32_combine_gen(len2);
        assert_eq!(crc32_combine_op(c1, c2, op), crc32_combine(c1, c2, len2));
    }

    #[test]
    fn combine_zero_length_is_xor() {
        // crc32_combine_gen(0) == 0x8000_0000 (the multmodp identity), so
        // combining with a zero-length second sequence is exactly c1 ^ c2.
        let c1 = 0x88b0_75e2;
        let c2 = 0x1878_6794;
        assert_eq!(crc32_combine_gen(0), 0x8000_0000);
        assert_eq!(crc32_combine(c1, c2, 0), c1 ^ c2);
        assert_eq!(crc32_combine(c1, c2, 0), 0x90c8_1276);
    }

    #[test]
    fn combine_negative_length_returns_zero() {
        // A negative length has no meaning; the generator and both combiners
        // defensively return zero (matches reference zlib).
        assert_eq!(crc32_combine_gen(-1), 0);
        assert_eq!(crc32_combine(0x88b0_75e2, 0x1878_6794, -1), 0);
        assert_eq!(crc32_combine(1, 2, -12_345), 0);
    }

    #[test]
    fn combine_op_zero_operator_returns_zero() {
        // A zero operator is never produced for a valid length, so it is
        // treated as an error and yields zero.
        assert_eq!(crc32_combine_op(0x1234_5678, 0x9abc_def0, 0), 0);
    }
}
