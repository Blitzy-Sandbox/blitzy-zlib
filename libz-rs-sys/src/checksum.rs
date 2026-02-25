//! C-compatible FFI wrappers for zlib checksum functions (Adler-32 and CRC-32).
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrapper functions that bridge
//! between the C zlib checksum API (using `c_ulong`, raw pointers, and C integer
//! types) and the safe Rust implementations in [`zlib_rs::checksum`].
//!
//! # Exported Symbols
//!
//! All 11 checksum symbols from `win32/zlib.def` lines 77–88:
//!
//! ## Adler-32 (RFC 1950)
//! - [`adler32`] — Update Adler-32 with `uInt` length (def line 81)
//! - [`adler32_z`] — Update Adler-32 with `z_size_t` length (def line 82)
//! - [`adler32_combine`] — Combine two Adler-32 values, `z_off_t` (def line 85)
//! - [`adler32_combine64`] — Combine two Adler-32 values, `z_off64_t` (def line 77)
//!
//! ## CRC-32 (RFC 1952)
//! - [`crc32`] — Update CRC-32 with `uInt` length (def line 83)
//! - [`crc32_z`] — Update CRC-32 with `z_size_t` length (def line 84)
//! - [`crc32_combine`] — Combine two CRC-32 values, `z_off_t` (def line 86)
//! - [`crc32_combine64`] — Combine two CRC-32 values, `z_off64_t` (def line 78)
//! - [`crc32_combine_gen`] — Generate combine operator, `z_off_t` (def line 87)
//! - [`crc32_combine_gen64`] — Generate combine operator, `z_off64_t` (def line 79)
//! - [`crc32_combine_op`] — Combine using pre-generated operator (def line 88)
//!
//! # Safety Contract
//!
//! Functions accepting raw pointers (`*const u8`) are marked `unsafe extern "C"`.
//! A null buffer pointer is **not** an error — it is the standard C zlib convention
//! for requesting the initial checksum value:
//! - Adler-32 initial value: `1`
//! - CRC-32 initial value: `0`
//!
//! When `buf` is non-null but `len` is zero, the functions return the input
//! checksum unchanged (a no-op), matching exact C zlib semantics.
//!
//! Functions that accept only scalar parameters (combine, combine_gen,
//! combine_op) are safe `extern "C"` functions — no `unsafe` qualifier.
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) to abort on panic
//! at the FFI boundary rather than triggering undefined behavior through stack
//! unwinding into C code (per AAP Section 0.7.3).

use libc::{c_uint, c_ulong};

use crate::types::*;

// =============================================================================
// Adler-32 Functions
// =============================================================================

/// Update a running Adler-32 checksum with the bytes `buf[0..len-1]`.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::adler32::adler32`].
///
/// C signature (`zlib.h` line 1809):
/// ```c
/// uLong adler32(uLong adler, const Bytef *buf, uInt len);
/// ```
///
/// If `buf` is null, returns the required initial Adler-32 value (`1`),
/// matching the C convention `adler32(0L, Z_NULL, 0)`.
///
/// If `buf` is non-null but `len` is zero, returns `adler` unchanged,
/// matching the C behavior where the processing loop executes zero
/// iterations and the original value is recombined.
///
/// # Safety
///
/// If `buf` is non-null, it must point to at least `len` readable bytes.
/// The caller is responsible for ensuring the pointed-to memory is valid
/// for the duration of this call.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn adler32(adler: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    // Null buf → return initial Adler-32 value (C: adler32.c "if (buf == Z_NULL) return 1L;")
    if buf.is_null() {
        return 1;
    }

    // Non-null buf with zero length → no-op, return input unchanged.
    // The Rust core function returns `1` for empty input (conflating null and
    // empty), so we handle this case directly to match C semantics exactly.
    if len == 0 {
        return adler;
    }

    // SAFETY: `buf` is non-null and the caller guarantees at least `len`
    // readable bytes. The `len as usize` widening from c_uint (u32) to usize
    // is always lossless on supported platforms (32-bit and 64-bit).
    let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };

    // Delegate to the safe Rust core implementation.
    // The `adler as u32` truncation is safe because Adler-32 values are
    // inherently 32-bit; the upper bits of c_ulong (on LP64) are always zero.
    zlib_rs::checksum::adler32::adler32(adler as u32, slice) as c_ulong
}

/// Update a running Adler-32 checksum with `z_size_t` (`size_t`) length.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::adler32::adler32_z`].
///
/// C signature (`zlib.h` lines 1829–1830):
/// ```c
/// uLong adler32_z(uLong adler, const Bytef *buf, z_size_t len);
/// ```
///
/// Same semantics as [`adler32`] but uses `usize` for the length parameter,
/// supporting buffers larger than `uInt` (> 4 GiB on 64-bit platforms).
///
/// # Safety
///
/// If `buf` is non-null, it must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn adler32_z(adler: c_ulong, buf: *const u8, len: usize) -> c_ulong {
    // Null buf → return initial Adler-32 value.
    if buf.is_null() {
        return 1;
    }

    // Non-null buf with zero length → no-op, return input unchanged.
    if len == 0 {
        return adler;
    }

    // SAFETY: `buf` is non-null and the caller guarantees at least `len`
    // readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(buf, len) };

    zlib_rs::checksum::adler32::adler32_z(adler as u32, slice) as c_ulong
}

/// Combine two Adler-32 checksums into one.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::adler32::adler32_combine`].
///
/// C signature (`zlib.h` lines 1837–1838):
/// ```c
/// uLong adler32_combine(uLong adler1, uLong adler2, z_off_t len2);
/// ```
///
/// Given Adler-32 checksums `adler1` (of sequence `seq1`) and `adler2`
/// (of sequence `seq2` with length `len2`), returns the Adler-32 checksum
/// of the concatenation `seq1 ∥ seq2`.
///
/// Note that `z_off_t` is a **signed** integer type (like `off_t`). If
/// `len2` is negative, the result has no meaning or utility — the function
/// returns `0xFFFF_FFFF`.
#[unsafe(no_mangle)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::unnecessary_cast
)]
pub extern "C" fn adler32_combine(
    adler1: c_ulong,
    adler2: c_ulong,
    len2: z_off_t,
) -> c_ulong {
    // The z_off_t → i64 cast is lossless on LP64 (identity) and sign-extending
    // on ILP32. The core function always takes i64 for portability.
    zlib_rs::checksum::adler32::adler32_combine(adler1 as u32, adler2 as u32, len2 as i64)
        as c_ulong
}

/// Combine two Adler-32 checksums using an explicit 64-bit offset.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::adler32::adler32_combine`].
///
/// C signature (`zlib.h` line 1982):
/// ```c
/// uLong adler32_combine64(uLong, uLong, z_off64_t);
/// ```
///
/// Same semantics as [`adler32_combine`] but with an explicit `z_off64_t`
/// (`i64`) length parameter, avoiding platform-dependent `z_off_t` sizing.
/// Listed in `win32/zlib.def` line 77 under "large file functions".
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub extern "C" fn adler32_combine64(
    adler1: c_ulong,
    adler2: c_ulong,
    len2: z_off64_t,
) -> c_ulong {
    // z_off64_t is i64, which the core function accepts directly.
    zlib_rs::checksum::adler32::adler32_combine(adler1 as u32, adler2 as u32, len2) as c_ulong
}

// =============================================================================
// CRC-32 Functions
// =============================================================================

/// Update a running CRC-32 with the bytes `buf[0..len-1]`.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32`].
///
/// C signature (`zlib.h` line 1848):
/// ```c
/// uLong crc32(uLong crc, const Bytef *buf, uInt len);
/// ```
///
/// If `buf` is null, returns the required initial CRC-32 value (`0`),
/// matching the C convention `crc32(0L, Z_NULL, 0)`.
///
/// If `buf` is non-null but `len` is zero, returns `crc` unchanged.
///
/// Pre- and post-conditioning (one's complement) is performed internally,
/// so the application should not apply its own conditioning.
///
/// # Safety
///
/// If `buf` is non-null, it must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn crc32(crc: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    // Null buf → return initial CRC-32 value (C: crc32.c "if (buf == Z_NULL) return 0;")
    if buf.is_null() {
        return 0;
    }

    // Non-null buf with zero length → no-op, return input unchanged.
    if len == 0 {
        return crc;
    }

    // SAFETY: `buf` is non-null and the caller guarantees at least `len`
    // readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };

    zlib_rs::checksum::crc32::crc32(crc as u32, slice) as c_ulong
}

/// Update a running CRC-32 with `z_size_t` (`size_t`) length.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_z`].
///
/// C signature (`zlib.h` lines 1866–1867):
/// ```c
/// uLong crc32_z(uLong crc, const Bytef *buf, z_size_t len);
/// ```
///
/// Same semantics as [`crc32`] but uses `usize` for the length parameter.
///
/// # Safety
///
/// If `buf` is non-null, it must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub unsafe extern "C" fn crc32_z(crc: c_ulong, buf: *const u8, len: usize) -> c_ulong {
    // Null buf → return initial CRC-32 value.
    if buf.is_null() {
        return 0;
    }

    // Non-null buf with zero length → no-op, return input unchanged.
    if len == 0 {
        return crc;
    }

    // SAFETY: `buf` is non-null and the caller guarantees at least `len`
    // readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(buf, len) };

    zlib_rs::checksum::crc32::crc32_z(crc as u32, slice) as c_ulong
}

/// Combine two CRC-32 check values into one.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_combine`].
///
/// C signature (`zlib.h` line 1874):
/// ```c
/// uLong crc32_combine(uLong crc1, uLong crc2, z_off_t len2);
/// ```
///
/// Returns the CRC-32 of the concatenation of two sequences whose individual
/// CRC-32 values are `crc1` and `crc2`, where `len2` is the byte length of
/// the second sequence. `len2` must be non-negative; otherwise `0` is returned.
#[unsafe(no_mangle)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::unnecessary_cast
)]
pub extern "C" fn crc32_combine(crc1: c_ulong, crc2: c_ulong, len2: z_off_t) -> c_ulong {
    zlib_rs::checksum::crc32::crc32_combine(crc1 as u32, crc2 as u32, len2 as i64) as c_ulong
}

/// Combine two CRC-32 check values using an explicit 64-bit offset.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_combine`].
///
/// C signature (`zlib.h` line 1983):
/// ```c
/// uLong crc32_combine64(uLong, uLong, z_off64_t);
/// ```
///
/// Same semantics as [`crc32_combine`] with explicit 64-bit length.
/// Listed in `win32/zlib.def` line 78 under "large file functions".
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub extern "C" fn crc32_combine64(crc1: c_ulong, crc2: c_ulong, len2: z_off64_t) -> c_ulong {
    zlib_rs::checksum::crc32::crc32_combine(crc1 as u32, crc2 as u32, len2) as c_ulong
}

/// Generate the operator for efficient repeated CRC-32 combination.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_combine_gen`].
///
/// C signature (`zlib.h` line 1884):
/// ```c
/// uLong crc32_combine_gen(z_off_t len2);
/// ```
///
/// Returns an opaque operator value corresponding to `len2`, for use with
/// [`crc32_combine_op`]. This is faster than calling [`crc32_combine`]
/// directly when the same `len2` is used for multiple combinations.
///
/// If `len2` is negative, returns `0`.
#[unsafe(no_mangle)]
#[allow(clippy::cast_lossless, clippy::unnecessary_cast)]
pub extern "C" fn crc32_combine_gen(len2: z_off_t) -> c_ulong {
    zlib_rs::checksum::crc32::crc32_combine_gen(len2 as i64) as c_ulong
}

/// Generate the CRC-32 combination operator using an explicit 64-bit offset.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_combine_gen`].
///
/// C signature (`zlib.h` line 1984):
/// ```c
/// uLong crc32_combine_gen64(z_off64_t);
/// ```
///
/// Same semantics as [`crc32_combine_gen`] with explicit 64-bit length.
/// Listed in `win32/zlib.def` line 79 under "large file functions".
#[unsafe(no_mangle)]
#[allow(clippy::cast_lossless)]
pub extern "C" fn crc32_combine_gen64(len2: z_off64_t) -> c_ulong {
    zlib_rs::checksum::crc32::crc32_combine_gen(len2) as c_ulong
}

/// Combine two CRC-32 values using a pre-generated operator.
///
/// FFI wrapper delegating to [`zlib_rs::checksum::crc32::crc32_combine_op`].
///
/// C signature (`zlib.h` line 1890):
/// ```c
/// uLong crc32_combine_op(uLong crc1, uLong crc2, uLong op);
/// ```
///
/// Produces the same result as [`crc32_combine`], but uses `op` (previously
/// computed by [`crc32_combine_gen`]) in place of `len2`. This avoids
/// recomputing the polynomial exponentiation on every call, providing a
/// significant speed-up when combining CRC-32 values for buffers of the
/// same length repeatedly.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation, clippy::cast_lossless)]
pub extern "C" fn crc32_combine_op(crc1: c_ulong, crc2: c_ulong, op: c_ulong) -> c_ulong {
    zlib_rs::checksum::crc32::crc32_combine_op(crc1 as u32, crc2 as u32, op as u32) as c_ulong
}
