//! C-compatible FFI wrappers for zlib one-call compress/uncompress utility
//! functions.
//!
//! This module provides `#[unsafe(no_mangle)] extern "C"` wrapper functions that
//! bridge between the C zlib compress/uncompress API (using `c_ulong`, `z_size_t`,
//! raw pointers, and C integer types) and the safe Rust implementations in
//! [`zlib_rs::compress`].
//!
//! # Exported Symbols
//!
//! All 10 compress/uncompress symbols from `win32/zlib.def` lines 35–44:
//!
//! ## Compression
//! - [`compress`] — One-call compress with `uLong` lengths (def line 35)
//! - [`compress2`] — One-call compress with explicit level, `uLong` (def line 36)
//! - [`compress_z`] — One-call compress with `z_size_t` lengths (def line 37)
//! - [`compress2_z`] — One-call compress with level, `z_size_t` (def line 38)
//! - [`compressBound`] — Upper bound on compressed size, `uLong` (def line 39)
//! - [`compressBound_z`] — Upper bound on compressed size, `z_size_t` (def line 40)
//!
//! ## Decompression
//! - [`uncompress`] — One-call decompress with `uLong` lengths (def line 41)
//! - [`uncompress2`] — One-call decompress, `sourceLen` is in/out `uLong*` (def line 42)
//! - [`uncompress_z`] — One-call decompress with `z_size_t` lengths (def line 43)
//! - [`uncompress2_z`] — One-call decompress, `sourceLen` is in/out `z_size_t*` (def line 44)
//!
//! # Delegation Pattern
//!
//! The implementation mirrors the C source's delegation hierarchy:
//!
//! - `compress2_z` is the canonical compression implementation (C: `compress.c` lines 24–66)
//! - `compress2` delegates to `compress2_z` with `c_ulong` ↔ `z_size_t` conversion
//! - `compress_z` delegates to `compress2_z` with `Z_DEFAULT_COMPRESSION`
//! - `compress` delegates to `compress2` with `Z_DEFAULT_COMPRESSION`
//!
//! - `uncompress2_z` is the canonical decompression implementation (C: `uncompr.c` lines 29–82)
//! - `uncompress2` delegates to `uncompress2_z` with `c_ulong` ↔ `z_size_t` conversion
//! - `uncompress_z` delegates to `uncompress2_z` with value-to-pointer `sourceLen`
//! - `uncompress` delegates to `uncompress2` with value-to-pointer `sourceLen`
//!
//! # Safety Contract
//!
//! Functions accepting raw pointers are marked `unsafe extern "C"`. All pointer
//! parameters are validated for null before dereferencing:
//!
//! - `dest` is validated when `*destLen > 0`
//! - `source` is validated when `sourceLen > 0`
//! - `destLen` pointer is always validated (must not be null)
//! - For `uncompress2`/`uncompress2_z`, `sourceLen` pointer is also validated
//!
//! Null pointer violations return [`Z_STREAM_ERROR`] (`-2`), matching the C
//! behavior in `compress.c` lines 31–33 and `uncompr.c` lines 36–38.
//!
//! [`compressBound`] and [`compressBound_z`] accept only scalar parameters and
//! are safe `extern "C"` functions — no `unsafe` qualifier.
//!
//! All functions use `extern "C"` (not `extern "C-unwind"`) to abort on panic
//! at the FFI boundary rather than triggering undefined behavior through stack
//! unwinding into C code (per AAP Section 0.7.3).

use libc::{c_int, c_ulong};

use crate::types::*;

// =============================================================================
// Compression Functions
// =============================================================================

/// Compress source data into a destination buffer with a specified compression
/// level, using `z_size_t` (`size_t`) length parameters.
///
/// This is the canonical compression implementation. All other `compress*`
/// variants delegate to this function after appropriate type conversion.
///
/// FFI wrapper delegating to [`zlib_rs::compress::compress2`].
///
/// C signature (`zlib.h` lines 1291–1293):
/// ```c
/// int compress2_z(Bytef *dest, z_size_t *destLen,
///                 const Bytef *source, z_size_t sourceLen, int level);
/// ```
///
/// # Pointer Validation (matching `compress.c` lines 31–33)
///
/// Returns [`Z_STREAM_ERROR`] if:
/// - `dest_len` is null
/// - `source_len > 0` and `source` is null
/// - `*dest_len > 0` and `dest` is null
///
/// On entry, `*dest_len` is the total size of the destination buffer. On
/// successful exit, `*dest_len` is the actual size of the compressed data.
/// On error, `*dest_len` is set to `0`.
///
/// # Returns
///
/// - [`Z_OK`] on success
/// - [`Z_MEM_ERROR`](crate::types::Z_MEM_ERROR) if there was not enough memory
/// - [`Z_BUF_ERROR`](crate::types::Z_BUF_ERROR) if dest buffer was too small
/// - [`Z_STREAM_ERROR`] if level is invalid or pointers are null
///
/// # Safety
///
/// - If `source` is non-null, it must point to at least `source_len` readable
///   bytes.
/// - If `dest` is non-null, it must point to at least `*dest_len` writable
///   bytes.
/// - `dest_len` must point to a valid, writable `z_size_t`.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn compress2_z(
    dest: *mut u8,
    dest_len: *mut z_size_t,
    source: *const u8,
    source_len: z_size_t,
    level: c_int,
) -> c_int {
    // Null pointer validation matching C compress.c lines 31–33:
    //   if ((sourceLen > 0 && source == NULL) ||
    //       destLen == NULL || (*destLen > 0 && dest == NULL))
    //       return Z_STREAM_ERROR;
    if dest_len.is_null() {
        return Z_STREAM_ERROR;
    }

    let dest_capacity = unsafe { *dest_len };

    if (source_len > 0 && source.is_null()) || (dest_capacity > 0 && dest.is_null()) {
        return Z_STREAM_ERROR;
    }

    // C behavior: set *destLen = 0 initially (compress.c line 36).
    // On success it will be updated to the actual compressed size.
    unsafe {
        *dest_len = 0;
    }

    // Build a safe source slice from the raw pointer.
    let source_slice = if source_len == 0 || source.is_null() {
        &[]
    } else {
        // SAFETY: source is non-null and the caller guarantees at least
        // source_len readable bytes.
        unsafe { std::slice::from_raw_parts(source, source_len) }
    };

    // Delegate to the safe Rust core implementation.
    // The core function allocates its own internal output buffer (sized to
    // compress_bound) and returns the compressed data in a Vec<u8>.
    let mut output = Vec::new();
    match zlib_rs::compress::compress2(&mut output, source_slice, level) {
        Ok(()) => {
            // Verify the compressed output fits in the caller's buffer.
            // The core function always succeeds with its own compress_bound-sized
            // buffer, but the caller may have provided a smaller destination.
            if output.len() > dest_capacity {
                return zlib_rs::error::ReturnCode::BufError as c_int;
            }

            // Copy compressed data to the caller's buffer.
            if !output.is_empty() && !dest.is_null() {
                // SAFETY: dest is non-null and points to at least dest_capacity
                // writable bytes. output.len() <= dest_capacity is verified above.
                unsafe {
                    std::ptr::copy_nonoverlapping(output.as_ptr(), dest, output.len());
                }
            }

            // Update *destLen to the actual compressed size.
            unsafe {
                *dest_len = output.len();
            }

            Z_OK
        }
        Err(code) => code as c_int,
    }
}

/// Compress source data into a destination buffer with a specified compression
/// level, using `c_ulong` (`uLong`) length parameters.
///
/// Delegates to [`compress2_z`] with `c_ulong` → `z_size_t` widening on entry
/// and `z_size_t` → `c_ulong` narrowing on exit, matching the C implementation
/// in `compress.c` lines 67–74.
///
/// C signature (`zlib.h` lines 1288–1290):
/// ```c
/// int compress2(Bytef *dest, uLongf *destLen,
///               const Bytef *source, uLong sourceLen, int level);
/// ```
///
/// # Safety
///
/// Same safety requirements as [`compress2_z`].
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn compress2(
    dest: *mut u8,
    dest_len: *mut c_ulong,
    source: *const u8,
    source_len: c_ulong,
    level: c_int,
) -> c_int {
    // Validate destLen is not null before reading through it.
    if dest_len.is_null() {
        return Z_STREAM_ERROR;
    }

    // Convert c_ulong → z_size_t (widening, always lossless) and delegate.
    // C equivalent (compress.c lines 70–73):
    //   z_size_t got = *destLen;
    //   ret = compress2_z(dest, &got, source, sourceLen, level);
    //   *destLen = (uLong)got;
    let mut got: z_size_t = unsafe { *dest_len } as z_size_t;

    let ret = unsafe {
        compress2_z(dest, &mut got, source, source_len as z_size_t, level)
    };

    // Narrow z_size_t → c_ulong. On LP64 this is identity; on LLP64 (Windows)
    // this truncates, matching the C cast `*destLen = (uLong)got`.
    unsafe {
        *dest_len = got as c_ulong;
    }

    ret
}

/// Compress source data with the default compression level, using `z_size_t`
/// length parameters.
///
/// Equivalent to [`compress2_z`] with `level = Z_DEFAULT_COMPRESSION`.
///
/// C signature (`zlib.h` lines 1273–1274):
/// ```c
/// int compress_z(Bytef *dest, z_size_t *destLen,
///                const Bytef *source, z_size_t sourceLen);
/// ```
///
/// Ported from `compress.c` lines 77–81.
///
/// # Safety
///
/// Same safety requirements as [`compress2_z`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress_z(
    dest: *mut u8,
    dest_len: *mut z_size_t,
    source: *const u8,
    source_len: z_size_t,
) -> c_int {
    // C equivalent: return compress2_z(dest, destLen, source, sourceLen,
    //                                  Z_DEFAULT_COMPRESSION);
    unsafe { compress2_z(dest, dest_len, source, source_len, Z_DEFAULT_COMPRESSION) }
}

/// Compress source data with the default compression level, using `c_ulong`
/// length parameters.
///
/// Equivalent to [`compress2`] with `level = Z_DEFAULT_COMPRESSION`.
///
/// C signature (`zlib.h` lines 1271–1272):
/// ```c
/// int compress(Bytef *dest, uLongf *destLen,
///              const Bytef *source, uLong sourceLen);
/// ```
///
/// Ported from `compress.c` lines 82–85.
///
/// # Safety
///
/// Same safety requirements as [`compress2`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress(
    dest: *mut u8,
    dest_len: *mut c_ulong,
    source: *const u8,
    source_len: c_ulong,
) -> c_int {
    // C equivalent: return compress2(dest, destLen, source, sourceLen,
    //                                Z_DEFAULT_COMPRESSION);
    unsafe { compress2(dest, dest_len, source, source_len, Z_DEFAULT_COMPRESSION) }
}

// =============================================================================
// Compress Bound Functions
// =============================================================================

/// Return an upper bound on the compressed size for `source_len` input bytes,
/// using `z_size_t` (`size_t`) parameter.
///
/// This function does NOT accept any pointers and is therefore a safe
/// `extern "C"` function (no `unsafe` required).
///
/// FFI wrapper delegating to [`zlib_rs::compress::compress_bound`].
///
/// C signature (`zlib.h` line 1308):
/// ```c
/// z_size_t compressBound_z(z_size_t sourceLen);
/// ```
///
/// Ported from `compress.c` lines 91–95.
#[unsafe(no_mangle)]
pub extern "C" fn compressBound_z(source_len: z_size_t) -> z_size_t {
    zlib_rs::compress::compress_bound(source_len)
}

/// Return an upper bound on the compressed size for `source_len` input bytes,
/// using `c_ulong` (`uLong`) parameter.
///
/// Delegates to [`compressBound_z`] and narrows the result to `c_ulong`. If the
/// bound exceeds `c_ulong::MAX` (possible on LLP64 platforms where `c_ulong` is
/// 32-bit but `z_size_t` is 64-bit), returns `c_ulong::MAX` (equivalent to the
/// C expression `(uLong)-1`).
///
/// This function does NOT accept any pointers and is therefore a safe
/// `extern "C"` function (no `unsafe` required).
///
/// C signature (`zlib.h` line 1307):
/// ```c
/// uLong compressBound(uLong sourceLen);
/// ```
///
/// Ported from `compress.c` lines 96–99.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation)]
pub extern "C" fn compressBound(source_len: c_ulong) -> c_ulong {
    // C equivalent (compress.c lines 97–98):
    //   z_size_t bound = compressBound_z(sourceLen);
    //   return (uLong)bound != bound ? (uLong)-1 : (uLong)bound;
    let bound = compressBound_z(source_len as z_size_t);

    // Check if narrowing to c_ulong would lose information.
    // On LP64: c_ulong is u64 = z_size_t, so this always passes.
    // On LLP64: c_ulong is u32, so very large bounds overflow.
    if bound as c_ulong as z_size_t != bound {
        c_ulong::MAX
    } else {
        bound as c_ulong
    }
}

// =============================================================================
// Decompression Functions
// =============================================================================

/// Decompress source data into a destination buffer, with `z_size_t`
/// in/out length parameters for both destination and source.
///
/// This is the canonical decompression implementation. All other `uncompress*`
/// variants delegate to this function after appropriate type conversion.
///
/// FFI wrapper delegating to [`zlib_rs::compress::uncompress2`].
///
/// C signature (`zlib.h` lines 1337–1338):
/// ```c
/// int uncompress2_z(Bytef *dest, z_size_t *destLen,
///                   const Bytef *source, z_size_t *sourceLen);
/// ```
///
/// # Pointer Validation (matching `uncompr.c` lines 36–38)
///
/// Returns [`Z_STREAM_ERROR`] if:
/// - `source_len` is null
/// - `dest_len` is null
/// - `*source_len > 0` and `source` is null
/// - `*dest_len > 0` and `dest` is null
///
/// On entry:
/// - `*dest_len` is the total size of the destination buffer
/// - `*source_len` is the total size of the source data
///
/// On successful exit:
/// - `*dest_len` is the actual size of the decompressed data
/// - `*source_len` is the number of source bytes consumed
///
/// # Returns
///
/// - [`Z_OK`] on success
/// - [`Z_MEM_ERROR`](crate::types::Z_MEM_ERROR) if there was not enough memory
/// - [`Z_BUF_ERROR`](crate::types::Z_BUF_ERROR) if dest buffer was too small
/// - [`Z_DATA_ERROR`](crate::types::Z_DATA_ERROR) if input data was corrupted
///   or incomplete
/// - [`Z_STREAM_ERROR`] if pointers are null
///
/// # Safety
///
/// - If `source` is non-null, it must point to at least `*source_len` readable
///   bytes.
/// - If `dest` is non-null, it must point to at least `*dest_len` writable
///   bytes.
/// - `dest_len` must point to a valid, writable `z_size_t`.
/// - `source_len` must point to a valid, writable `z_size_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2_z(
    dest: *mut u8,
    dest_len: *mut z_size_t,
    source: *const u8,
    source_len: *mut z_size_t,
) -> c_int {
    // Null pointer validation matching C uncompr.c lines 36–38:
    //   if (sourceLen == NULL || (*sourceLen > 0 && source == NULL) ||
    //       destLen == NULL || (*destLen > 0 && dest == NULL))
    //       return Z_STREAM_ERROR;
    if source_len.is_null() || dest_len.is_null() {
        return Z_STREAM_ERROR;
    }

    let src_len = unsafe { *source_len };
    let dest_capacity = unsafe { *dest_len };

    if (src_len > 0 && source.is_null()) || (dest_capacity > 0 && dest.is_null()) {
        return Z_STREAM_ERROR;
    }

    // Build a safe source slice from the raw pointer.
    let source_slice = if src_len == 0 || source.is_null() {
        &[]
    } else {
        // SAFETY: source is non-null and the caller guarantees at least
        // src_len readable bytes.
        unsafe { std::slice::from_raw_parts(source, src_len) }
    };

    // Create a destination Vec with the caller's specified capacity.
    // The core uncompress2 function uses dest.len() as the output capacity.
    let mut dest_vec = vec![0u8; dest_capacity];

    // Delegate to the safe Rust core implementation.
    match zlib_rs::compress::uncompress2(&mut dest_vec, source_slice) {
        Ok((dest_used, source_consumed)) => {
            // Copy decompressed data to the caller's buffer.
            if dest_used > 0 && !dest.is_null() {
                // SAFETY: dest is non-null and points to at least dest_capacity
                // writable bytes. dest_used <= dest_capacity is guaranteed by
                // the core function (it can't produce more output than the
                // buffer it was given).
                unsafe {
                    std::ptr::copy_nonoverlapping(dest_vec.as_ptr(), dest, dest_used);
                }
            }

            // Update output size parameters.
            unsafe {
                *dest_len = dest_used;
                *source_len = source_consumed;
            }

            Z_OK
        }
        Err(code) => {
            // On error, set lengths to 0. The C implementation reports partial
            // progress on error via stream tracking; the Rust core API does not
            // expose partial progress on failure, so zero is the safe default.
            unsafe {
                *dest_len = 0;
                *source_len = 0;
            }
            code as c_int
        }
    }
}

/// Decompress source data into a destination buffer, with `c_ulong` in/out
/// length parameters for both destination and source.
///
/// Delegates to [`uncompress2_z`] with `c_ulong` → `z_size_t` widening on
/// entry and `z_size_t` → `c_ulong` narrowing on exit, matching the C
/// implementation in `uncompr.c` lines 83–91.
///
/// C signature (`zlib.h` lines 1335–1336):
/// ```c
/// int uncompress2(Bytef *dest, uLongf *destLen,
///                 const Bytef *source, uLong *sourceLen);
/// ```
///
/// # Safety
///
/// Same safety requirements as [`uncompress2_z`], with `c_ulong` pointers
/// instead of `z_size_t`.
#[unsafe(no_mangle)]
#[allow(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn uncompress2(
    dest: *mut u8,
    dest_len: *mut c_ulong,
    source: *const u8,
    source_len: *mut c_ulong,
) -> c_int {
    // Validate both length pointers before reading through them.
    if dest_len.is_null() || source_len.is_null() {
        return Z_STREAM_ERROR;
    }

    // Convert c_ulong → z_size_t (widening, always lossless) and delegate.
    // C equivalent (uncompr.c lines 86–89):
    //   z_size_t got = *destLen, used = *sourceLen;
    //   ret = uncompress2_z(dest, &got, source, &used);
    //   *sourceLen = (uLong)used;
    //   *destLen = (uLong)got;
    let mut got: z_size_t = unsafe { *dest_len } as z_size_t;
    let mut used: z_size_t = unsafe { *source_len } as z_size_t;

    let ret = unsafe { uncompress2_z(dest, &mut got, source, &mut used) };

    // Narrow z_size_t → c_ulong, matching C truncation casts.
    unsafe {
        *source_len = used as c_ulong;
        *dest_len = got as c_ulong;
    }

    ret
}

/// Decompress source data into a destination buffer, using `z_size_t` length
/// parameters with `sourceLen` as a value (not a pointer).
///
/// Delegates to [`uncompress2_z`] by converting the `source_len` value into a
/// local variable's address, matching the C implementation in `uncompr.c`
/// lines 92–96.
///
/// C signature (`zlib.h` lines 1317–1318):
/// ```c
/// int uncompress_z(Bytef *dest, z_size_t *destLen,
///                  const Bytef *source, z_size_t sourceLen);
/// ```
///
/// # Safety
///
/// Same safety requirements as [`uncompress2_z`], except `source_len` is a
/// value rather than a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress_z(
    dest: *mut u8,
    dest_len: *mut z_size_t,
    source: *const u8,
    source_len: z_size_t,
) -> c_int {
    // C equivalent (uncompr.c lines 94–95):
    //   z_size_t used = sourceLen;
    //   return uncompress2_z(dest, destLen, source, &used);
    let mut used: z_size_t = source_len;
    unsafe { uncompress2_z(dest, dest_len, source, &mut used) }
}

/// Decompress source data into a destination buffer, using `c_ulong` length
/// parameters with `sourceLen` as a value (not a pointer).
///
/// Delegates to [`uncompress2`] by converting the `source_len` value into a
/// local variable's address, matching the C implementation in `uncompr.c`
/// lines 97–101.
///
/// C signature (`zlib.h` lines 1315–1316):
/// ```c
/// int uncompress(Bytef *dest, uLongf *destLen,
///                const Bytef *source, uLong sourceLen);
/// ```
///
/// # Safety
///
/// Same safety requirements as [`uncompress2`], except `source_len` is a
/// value rather than a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress(
    dest: *mut u8,
    dest_len: *mut c_ulong,
    source: *const u8,
    source_len: c_ulong,
) -> c_int {
    // C equivalent (uncompr.c lines 99–100):
    //   uLong used = sourceLen;
    //   return uncompress2(dest, destLen, source, &used);
    let mut used: c_ulong = source_len;
    unsafe { uncompress2(dest, dest_len, source, &mut used) }
}
