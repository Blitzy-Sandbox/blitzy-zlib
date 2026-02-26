//! FFI symbol presence and signature validation tests.
//!
//! Validates that all 96 unique symbol names from the canonical `win32/zlib.def`
//! are present in the `libz-rs-sys` FFI crate with correct `extern "C"` signatures.
//! This ensures the Rust library functions as a drop-in replacement for `libz.so` /
//! `zlib1.dll`.
//!
//! Each test verifies a symbol's existence by coercing its name to the expected
//! function pointer type — a compile-time check that catches missing or
//! mistyped signatures. Selected tests also exercise runtime behavior (e.g.
//! `zlibVersion` returning a valid string, `compressBound` producing sane
//! bounds, checksum functions returning known values).
//!
//! # Symbol categories (per `win32/zlib.def`)
//!
//! | Category              | Count | Tests                                  |
//! |-----------------------|------:|----------------------------------------|
//! | Basic functions       |     5 | `test_ffi_zlib_version` … `_inflate_end` |
//! | Advanced functions    |    24 | `test_ffi_deflate_set_dictionary` … `_zlib_compile_flags` |
//! | Utility functions     |    37 | `test_ffi_compress` … `_gzclearerr`    |
//! | Large file functions  |     7 | `test_ffi_gzopen64` … `_crc32_combine_gen64` |
//! | Checksum functions    |     8 | `test_ffi_adler32` … `_crc32_combine_op` |
//! | Various hacks         |    15 | `test_ffi_deflate_init_` … `_gzopen_w` |
//! | **Total**             | **96**|                                        |
//!
//! # References
//!
//! - `win32/zlib.def` — Complete list of exported DLL symbols
//! - `zlib.map` — GNU symbol versioning (14 version milestones)
//! - `zlib.h` — Public API declarations, parameter types, return types

// ---------------------------------------------------------------------------
// Lint configuration
// ---------------------------------------------------------------------------
// Wildcard imports are required for `use libz_rs_sys::*;` which re-exports
// all 96 FFI symbols plus #[repr(C)] types and constants.
#![allow(clippy::wildcard_imports)]
// Many FFI functions are `unsafe extern "C"` — assigning them to typed
// variables triggers `trivially_copy_pass_by_ref` and similar lints.
#![allow(clippy::missing_safety_doc)]
// The test file deliberately uses short identifiers for conciseness.
#![allow(clippy::similar_names)]
// Tests use explicit casts for layout verification.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

use std::ffi::CStr;
use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_long;
use std::ffi::c_uchar;
use std::ffi::c_uint;
use std::ffi::c_ulong;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};

use libz_rs_sys::*;

// ===========================================================================
// Section 1: Basic Functions (5 symbols — zlib.def lines 4–8)
// ===========================================================================

/// Verify `zlibVersion` returns a valid, non-null version string starting
/// with "1.3.2".
#[test]
fn test_ffi_zlib_version() {
    // Compile-time signature check.
    let _: extern "C" fn() -> *const c_char = zlibVersion;

    // Runtime behavioral check.
    let ptr = zlibVersion();
    assert!(!ptr.is_null(), "zlibVersion() returned null");
    let version_str = unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .expect("zlibVersion() returned invalid UTF-8");
    assert!(
        version_str.starts_with("1.3.2"),
        "unexpected version string: {version_str}"
    );
}

/// Verify `deflate` symbol exists with the correct signature.
#[test]
fn test_ffi_deflate() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int) -> c_int = deflate;
}

/// Verify `deflateEnd` symbol exists with the correct signature.
#[test]
fn test_ffi_deflate_end() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = deflateEnd;
}

/// Verify `inflate` symbol exists with the correct signature.
#[test]
fn test_ffi_inflate() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int) -> c_int = inflate;
}

/// Verify `inflateEnd` symbol exists with the correct signature.
#[test]
fn test_ffi_inflate_end() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateEnd;
}

// ===========================================================================
// Section 2: Advanced Deflate Functions (12 symbols — zlib.def lines 10–21)
// ===========================================================================

/// Verify `deflateSetDictionary` symbol and signature.
#[test]
fn test_ffi_deflate_set_dictionary() {
    let _: unsafe extern "C" fn(*mut z_stream, *const u8, c_uint) -> c_int = deflateSetDictionary;
}

/// Verify `deflateGetDictionary` symbol and signature.
#[test]
fn test_ffi_deflate_get_dictionary() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut u8, *mut c_uint) -> c_int =
        deflateGetDictionary;
}

/// Verify `deflateCopy` symbol and signature.
#[test]
fn test_ffi_deflate_copy() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut z_stream) -> c_int = deflateCopy;
}

/// Verify `deflateReset` symbol and signature.
#[test]
fn test_ffi_deflate_reset() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = deflateReset;
}

/// Verify `deflateParams` symbol and signature.
#[test]
fn test_ffi_deflate_params() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int = deflateParams;
}

/// Verify `deflateTune` symbol and signature.
#[test]
fn test_ffi_deflate_tune() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, c_int, c_int, c_int) -> c_int = deflateTune;
}

/// Verify `deflateBound` symbol and signature.
#[test]
fn test_ffi_deflate_bound() {
    let _: unsafe extern "C" fn(*mut z_stream, c_ulong) -> c_ulong = deflateBound;
}

/// Verify `deflateBound_z` symbol and signature.
#[test]
fn test_ffi_deflate_bound_z() {
    let _: unsafe extern "C" fn(*mut z_stream, z_size_t) -> z_size_t = deflateBound_z;
}

/// Verify `deflatePending` symbol and signature.
#[test]
fn test_ffi_deflate_pending() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut c_uint, *mut c_int) -> c_int = deflatePending;
}

/// Verify `deflateUsed` symbol and signature.
#[test]
fn test_ffi_deflate_used() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut c_int) -> c_int = deflateUsed;
}

/// Verify `deflatePrime` symbol and signature.
#[test]
fn test_ffi_deflate_prime() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int = deflatePrime;
}

/// Verify `deflateSetHeader` symbol and signature.
#[test]
fn test_ffi_deflate_set_header() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut gz_header) -> c_int = deflateSetHeader;
}

// ===========================================================================
// Section 3: Advanced Inflate Functions (12 symbols + zlibCompileFlags
//            — zlib.def lines 22–33)
// ===========================================================================

/// Verify `inflateSetDictionary` symbol and signature.
#[test]
fn test_ffi_inflate_set_dictionary() {
    let _: unsafe extern "C" fn(*mut z_stream, *const u8, c_uint) -> c_int = inflateSetDictionary;
}

/// Verify `inflateGetDictionary` symbol and signature.
#[test]
fn test_ffi_inflate_get_dictionary() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut u8, *mut c_uint) -> c_int =
        inflateGetDictionary;
}

/// Verify `inflateSync` symbol and signature.
#[test]
fn test_ffi_inflate_sync() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateSync;
}

/// Verify `inflateCopy` symbol and signature.
#[test]
fn test_ffi_inflate_copy() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut z_stream) -> c_int = inflateCopy;
}

/// Verify `inflateReset` symbol and signature.
#[test]
fn test_ffi_inflate_reset() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateReset;
}

/// Verify `inflateReset2` symbol and signature.
#[test]
fn test_ffi_inflate_reset2() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int) -> c_int = inflateReset2;
}

/// Verify `inflatePrime` symbol and signature.
#[test]
fn test_ffi_inflate_prime() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int = inflatePrime;
}

/// Verify `inflateMark` symbol and signature.
#[test]
fn test_ffi_inflate_mark() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_long = inflateMark;
}

/// Verify `inflateGetHeader` symbol and signature.
#[test]
fn test_ffi_inflate_get_header() {
    let _: unsafe extern "C" fn(*mut z_stream, *mut gz_header) -> c_int = inflateGetHeader;
}

/// Verify `inflateBack` symbol and signature.
#[test]
fn test_ffi_inflate_back() {
    let _: unsafe extern "C" fn(
        *mut z_stream,
        in_func,
        *mut c_void,
        out_func,
        *mut c_void,
    ) -> c_int = inflateBack;
}

/// Verify `inflateBackEnd` symbol and signature.
#[test]
fn test_ffi_inflate_back_end() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateBackEnd;
}

/// Verify `zlibCompileFlags` symbol and signature, plus basic runtime check.
#[test]
fn test_ffi_zlib_compile_flags() {
    let _: extern "C" fn() -> c_ulong = zlibCompileFlags;
    // Runtime: the flags value should be non-zero on any real implementation
    // (it always encodes at least type-size information).
    let flags = zlibCompileFlags();
    assert_ne!(flags, 0, "zlibCompileFlags() returned zero");
}

// ===========================================================================
// Section 4: Utility / Compress Functions (10 symbols — zlib.def lines 35–44)
// ===========================================================================

/// Verify `compress` symbol and signature.
#[test]
fn test_ffi_compress() {
    let _: unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong) -> c_int = compress;
}

/// Verify `compress2` symbol and signature.
#[test]
fn test_ffi_compress2() {
    let _: unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong, c_int) -> c_int =
        compress2;
}

/// Verify `compress_z` symbol and signature.
#[test]
fn test_ffi_compress_z() {
    let _: unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t) -> c_int = compress_z;
}

/// Verify `compress2_z` symbol and signature.
#[test]
fn test_ffi_compress2_z() {
    let _: unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t, c_int) -> c_int =
        compress2_z;
}

/// Verify `compressBound` symbol and signature, plus sane runtime output.
#[test]
fn test_ffi_compress_bound() {
    let _: extern "C" fn(c_ulong) -> c_ulong = compressBound;

    // Runtime: bound for a 1000-byte input must be > 1000 (header overhead).
    let bound = compressBound(1000);
    assert!(
        bound > 1000,
        "compressBound(1000) = {bound}, expected > 1000"
    );
}

/// Verify `compressBound_z` symbol and signature.
#[test]
fn test_ffi_compress_bound_z() {
    let _: extern "C" fn(z_size_t) -> z_size_t = compressBound_z;

    let bound = compressBound_z(1000);
    assert!(
        bound > 1000,
        "compressBound_z(1000) = {bound}, expected > 1000"
    );
}

/// Verify `uncompress` symbol and signature.
#[test]
fn test_ffi_uncompress() {
    let _: unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong) -> c_int = uncompress;
}

/// Verify `uncompress2` symbol and signature.
#[test]
fn test_ffi_uncompress2() {
    let _: unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, *mut c_ulong) -> c_int =
        uncompress2;
}

/// Verify `uncompress_z` symbol and signature.
#[test]
fn test_ffi_uncompress_z() {
    let _: unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t) -> c_int =
        uncompress_z;
}

/// Verify `uncompress2_z` symbol and signature.
#[test]
fn test_ffi_uncompress2_z() {
    let _: unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, *mut z_size_t) -> c_int =
        uncompress2_z;
}

// ===========================================================================
// Section 5: Gz Functions (27 symbols — zlib.def lines 45–71)
// ===========================================================================

/// Verify `gzopen` symbol and signature.
#[test]
fn test_ffi_gzopen() {
    let _: unsafe extern "C" fn(*const c_char, *const c_char) -> gzFile = gzopen;
}

/// Verify `gzdopen` symbol and signature.
#[test]
fn test_ffi_gzdopen() {
    let _: unsafe extern "C" fn(c_int, *const c_char) -> gzFile = gzdopen;
}

/// Verify `gzbuffer` symbol and signature.
#[test]
fn test_ffi_gzbuffer() {
    let _: unsafe extern "C" fn(gzFile, c_uint) -> c_int = gzbuffer;
}

/// Verify `gzsetparams` symbol and signature.
#[test]
fn test_ffi_gzsetparams() {
    let _: unsafe extern "C" fn(gzFile, c_int, c_int) -> c_int = gzsetparams;
}

/// Verify `gzread` symbol and signature.
#[test]
fn test_ffi_gzread() {
    let _: unsafe extern "C" fn(gzFile, *mut c_void, c_uint) -> c_int = gzread;
}

/// Verify `gzfread` symbol and signature.
#[test]
fn test_ffi_gzfread() {
    let _: unsafe extern "C" fn(*mut c_void, z_size_t, z_size_t, gzFile) -> z_size_t = gzfread;
}

/// Verify `gzwrite` symbol and signature.
#[test]
fn test_ffi_gzwrite() {
    let _: unsafe extern "C" fn(gzFile, *const c_void, c_uint) -> c_int = gzwrite;
}

/// Verify `gzfwrite` symbol and signature.
#[test]
fn test_ffi_gzfwrite() {
    let _: unsafe extern "C" fn(*const c_void, z_size_t, z_size_t, gzFile) -> z_size_t = gzfwrite;
}

/// Verify `gzprintf` symbol and signature.
///
/// Note: the Rust FFI version is non-variadic (accepts only the format
/// string). Full C variadic support requires nightly Rust.
#[test]
fn test_ffi_gzprintf() {
    let _: unsafe extern "C" fn(gzFile, *const c_char) -> c_int = gzprintf;
}

/// Verify `gzvprintf` symbol and signature.
///
/// The `va_list` parameter is represented as `*mut c_void` on stable Rust.
/// Full `va_list` support requires nightly with the `c_variadic` feature.
#[test]
fn test_ffi_gzvprintf() {
    let _: unsafe extern "C" fn(gzFile, *const c_char, *mut c_void) -> c_int = gzvprintf;
}

/// Verify `gzputs` symbol and signature.
#[test]
fn test_ffi_gzputs() {
    let _: unsafe extern "C" fn(gzFile, *const c_char) -> c_int = gzputs;
}

/// Verify `gzgets` symbol and signature.
#[test]
fn test_ffi_gzgets() {
    let _: unsafe extern "C" fn(gzFile, *mut c_char, c_int) -> *mut c_char = gzgets;
}

/// Verify `gzputc` symbol and signature.
#[test]
fn test_ffi_gzputc() {
    let _: unsafe extern "C" fn(gzFile, c_int) -> c_int = gzputc;
}

/// Verify `gzgetc` symbol and signature.
#[test]
fn test_ffi_gzgetc() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzgetc;
}

/// Verify `gzungetc` symbol and signature.
#[test]
fn test_ffi_gzungetc() {
    let _: unsafe extern "C" fn(c_int, gzFile) -> c_int = gzungetc;
}

/// Verify `gzflush` symbol and signature.
#[test]
fn test_ffi_gzflush() {
    let _: unsafe extern "C" fn(gzFile, c_int) -> c_int = gzflush;
}

/// Verify `gzseek` symbol and signature.
#[test]
fn test_ffi_gzseek() {
    let _: unsafe extern "C" fn(gzFile, z_off_t, c_int) -> z_off_t = gzseek;
}

/// Verify `gzrewind` symbol and signature.
#[test]
fn test_ffi_gzrewind() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzrewind;
}

/// Verify `gztell` symbol and signature.
#[test]
fn test_ffi_gztell() {
    let _: unsafe extern "C" fn(gzFile) -> z_off_t = gztell;
}

/// Verify `gzoffset` symbol and signature.
#[test]
fn test_ffi_gzoffset() {
    let _: unsafe extern "C" fn(gzFile) -> z_off_t = gzoffset;
}

/// Verify `gzeof` symbol and signature.
#[test]
fn test_ffi_gzeof() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzeof;
}

/// Verify `gzdirect` symbol and signature.
#[test]
fn test_ffi_gzdirect() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzdirect;
}

/// Verify `gzclose` symbol and signature.
#[test]
fn test_ffi_gzclose() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzclose;
}

/// Verify `gzclose_r` symbol and signature.
#[test]
fn test_ffi_gzclose_r() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzclose_r;
}

/// Verify `gzclose_w` symbol and signature.
#[test]
fn test_ffi_gzclose_w() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzclose_w;
}

/// Verify `gzerror` symbol and signature.
#[test]
fn test_ffi_gzerror() {
    let _: unsafe extern "C" fn(gzFile, *mut c_int) -> *const c_char = gzerror;
}

/// Verify `gzclearerr` symbol and signature.
#[test]
fn test_ffi_gzclearerr() {
    let _: unsafe extern "C" fn(gzFile) = gzclearerr;
}

// ===========================================================================
// Section 6: Large File Functions (7 symbols — zlib.def lines 73–79)
// ===========================================================================

/// Verify `gzopen64` symbol and signature.
#[test]
fn test_ffi_gzopen64() {
    let _: unsafe extern "C" fn(*const c_char, *const c_char) -> gzFile = gzopen64;
}

/// Verify `gzseek64` symbol and signature.
#[test]
fn test_ffi_gzseek64() {
    let _: unsafe extern "C" fn(gzFile, z_off64_t, c_int) -> z_off64_t = gzseek64;
}

/// Verify `gztell64` symbol and signature.
#[test]
fn test_ffi_gztell64() {
    let _: unsafe extern "C" fn(gzFile) -> z_off64_t = gztell64;
}

/// Verify `gzoffset64` symbol and signature.
#[test]
fn test_ffi_gzoffset64() {
    let _: unsafe extern "C" fn(gzFile) -> z_off64_t = gzoffset64;
}

/// Verify `adler32_combine64` symbol and signature.
#[test]
fn test_ffi_adler32_combine64() {
    let _: extern "C" fn(c_ulong, c_ulong, z_off64_t) -> c_ulong = adler32_combine64;
}

/// Verify `crc32_combine64` symbol and signature.
#[test]
fn test_ffi_crc32_combine64() {
    let _: extern "C" fn(c_ulong, c_ulong, z_off64_t) -> c_ulong = crc32_combine64;
}

/// Verify `crc32_combine_gen64` symbol and signature.
#[test]
fn test_ffi_crc32_combine_gen64() {
    let _: extern "C" fn(z_off64_t) -> c_ulong = crc32_combine_gen64;
}

// ===========================================================================
// Section 7: Checksum Functions (8 symbols — zlib.def lines 81–88)
// ===========================================================================

/// Verify `adler32` symbol and signature, plus known value check.
#[test]
fn test_ffi_adler32() {
    let _: unsafe extern "C" fn(c_ulong, *const u8, c_uint) -> c_ulong = adler32;

    // adler32(1, NULL, 0) should return 1 (the Adler-32 initial value).
    let result = unsafe { adler32(1, null(), 0) };
    assert_eq!(result, 1, "adler32(1, NULL, 0) = {result}, expected 1");
}

/// Verify `adler32_z` symbol and signature.
#[test]
fn test_ffi_adler32_z() {
    let _: unsafe extern "C" fn(c_ulong, *const u8, usize) -> c_ulong = adler32_z;
}

/// Verify `crc32` symbol and signature, plus known value check.
#[test]
fn test_ffi_crc32() {
    let _: unsafe extern "C" fn(c_ulong, *const u8, c_uint) -> c_ulong = crc32;

    // crc32(0, NULL, 0) should return 0.
    let result = unsafe { crc32(0, null(), 0) };
    assert_eq!(result, 0, "crc32(0, NULL, 0) = {result}, expected 0");
}

/// Verify `crc32_z` symbol and signature.
#[test]
fn test_ffi_crc32_z() {
    let _: unsafe extern "C" fn(c_ulong, *const u8, usize) -> c_ulong = crc32_z;
}

/// Verify `adler32_combine` symbol and signature.
#[test]
fn test_ffi_adler32_combine() {
    let _: extern "C" fn(c_ulong, c_ulong, z_off_t) -> c_ulong = adler32_combine;
}

/// Verify `crc32_combine` symbol and signature.
#[test]
fn test_ffi_crc32_combine() {
    let _: extern "C" fn(c_ulong, c_ulong, z_off_t) -> c_ulong = crc32_combine;
}

/// Verify `crc32_combine_gen` symbol and signature.
#[test]
fn test_ffi_crc32_combine_gen() {
    let _: extern "C" fn(z_off_t) -> c_ulong = crc32_combine_gen;
}

/// Verify `crc32_combine_op` symbol and signature.
#[test]
fn test_ffi_crc32_combine_op() {
    let _: extern "C" fn(c_ulong, c_ulong, c_ulong) -> c_ulong = crc32_combine_op;
}

// ===========================================================================
// Section 8: Init / Misc "Hacks" Functions (15 symbols — zlib.def lines 90–104)
// ===========================================================================

/// Verify `deflateInit_` symbol and signature.
#[test]
fn test_ffi_deflate_init_() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, *const c_char, c_int) -> c_int = deflateInit_;
}

/// Verify `deflateInit2_` symbol and signature.
#[test]
fn test_ffi_deflate_init2_() {
    let _: unsafe extern "C" fn(
        *mut z_stream,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        *const c_char,
        c_int,
    ) -> c_int = deflateInit2_;
}

/// Verify `inflateInit_` symbol and signature.
#[test]
fn test_ffi_inflate_init_() {
    let _: unsafe extern "C" fn(*mut z_stream, *const c_char, c_int) -> c_int = inflateInit_;
}

/// Verify `inflateInit2_` symbol and signature.
#[test]
fn test_ffi_inflate_init2_() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, *const c_char, c_int) -> c_int =
        inflateInit2_;
}

/// Verify `inflateBackInit_` symbol and signature.
#[test]
fn test_ffi_inflate_back_init_() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int, *mut c_uchar, *const c_char, c_int) -> c_int =
        inflateBackInit_;
}

/// Verify `gzgetc_` symbol and signature.
#[test]
fn test_ffi_gzgetc_() {
    let _: unsafe extern "C" fn(gzFile) -> c_int = gzgetc_;
}

/// Verify `zError` symbol and signature.
#[test]
fn test_ffi_z_error() {
    let _: extern "C" fn(c_int) -> *const c_char = zError;

    // Runtime: zError(Z_OK) should return a non-null string.
    let ptr = zError(Z_OK);
    assert!(!ptr.is_null(), "zError(Z_OK) returned null");
}

/// Verify `inflateSyncPoint` symbol and signature.
#[test]
fn test_ffi_inflate_sync_point() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateSyncPoint;
}

/// Verify `get_crc_table` symbol and signature.
#[test]
fn test_ffi_get_crc_table() {
    let _: extern "C" fn() -> *const u32 = get_crc_table;

    // Runtime: should return a non-null pointer to the CRC table.
    let ptr = get_crc_table();
    assert!(!ptr.is_null(), "get_crc_table() returned null");
}

/// Verify `inflateUndermine` symbol and signature.
#[test]
fn test_ffi_inflate_undermine() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int) -> c_int = inflateUndermine;
}

/// Verify `inflateValidate` symbol and signature.
#[test]
fn test_ffi_inflate_validate() {
    let _: unsafe extern "C" fn(*mut z_stream, c_int) -> c_int = inflateValidate;
}

/// Verify `inflateCodesUsed` symbol and signature.
#[test]
fn test_ffi_inflate_codes_used() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_ulong = inflateCodesUsed;
}

/// Verify `inflateResetKeep` symbol and signature.
#[test]
fn test_ffi_inflate_reset_keep() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = inflateResetKeep;
}

/// Verify `deflateResetKeep` symbol and signature.
#[test]
fn test_ffi_deflate_reset_keep() {
    let _: unsafe extern "C" fn(*mut z_stream) -> c_int = deflateResetKeep;
}

/// Verify `gzopen_w` symbol and signature.
///
/// On non-Windows targets, the path parameter is `*const c_void` (stub).
/// On Windows, it is `*const wchar_t`.
#[test]
fn test_ffi_gzopen_w() {
    #[cfg(not(target_os = "windows"))]
    {
        let _: unsafe extern "C" fn(*const c_void, *const c_char) -> gzFile = gzopen_w;
    }
    #[cfg(target_os = "windows")]
    {
        let _: unsafe extern "C" fn(*const libc::wchar_t, *const c_char) -> gzFile = gzopen_w;
    }
}

// ===========================================================================
// Section 9: Comprehensive Symbol Count Validation (Meta-test)
// ===========================================================================

/// Meta-test verifying that all 96 symbols from `win32/zlib.def` are
/// accessible as Rust function identifiers.
///
/// Each symbol is referenced by assigning it to a typed variable. Missing
/// symbols cause a compile-time error. At runtime, the count is verified
/// to match the expected total.
#[test]
#[allow(clippy::too_many_lines)]
fn test_ffi_all_96_symbols_present() {
    // We count symbols by category to match the zlib.def file organization.
    // Any missing symbol will cause a compile error before the test runs.
    let mut count: u32 = 0;

    // --- Basic functions (5) -----------------------------------------------
    let _ = zlibVersion as extern "C" fn() -> *const c_char;
    count += 1;
    let _ = deflate as unsafe extern "C" fn(*mut z_stream, c_int) -> c_int;
    count += 1;
    let _ = deflateEnd as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = inflate as unsafe extern "C" fn(*mut z_stream, c_int) -> c_int;
    count += 1;
    let _ = inflateEnd as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;

    // --- Advanced functions (24) -------------------------------------------
    let _ = deflateSetDictionary as unsafe extern "C" fn(*mut z_stream, *const u8, c_uint) -> c_int;
    count += 1;
    let _ =
        deflateGetDictionary as unsafe extern "C" fn(*mut z_stream, *mut u8, *mut c_uint) -> c_int;
    count += 1;
    let _ = deflateCopy as unsafe extern "C" fn(*mut z_stream, *mut z_stream) -> c_int;
    count += 1;
    let _ = deflateReset as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = deflateParams as unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int;
    count += 1;
    let _ = deflateTune as unsafe extern "C" fn(*mut z_stream, c_int, c_int, c_int, c_int) -> c_int;
    count += 1;
    let _ = deflateBound as unsafe extern "C" fn(*mut z_stream, c_ulong) -> c_ulong;
    count += 1;
    let _ = deflateBound_z as unsafe extern "C" fn(*mut z_stream, z_size_t) -> z_size_t;
    count += 1;
    let _ = deflatePending as unsafe extern "C" fn(*mut z_stream, *mut c_uint, *mut c_int) -> c_int;
    count += 1;
    let _ = deflateUsed as unsafe extern "C" fn(*mut z_stream, *mut c_int) -> c_int;
    count += 1;
    let _ = deflatePrime as unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int;
    count += 1;
    let _ = deflateSetHeader as unsafe extern "C" fn(*mut z_stream, *mut gz_header) -> c_int;
    count += 1;
    let _ = inflateSetDictionary as unsafe extern "C" fn(*mut z_stream, *const u8, c_uint) -> c_int;
    count += 1;
    let _ =
        inflateGetDictionary as unsafe extern "C" fn(*mut z_stream, *mut u8, *mut c_uint) -> c_int;
    count += 1;
    let _ = inflateSync as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = inflateCopy as unsafe extern "C" fn(*mut z_stream, *mut z_stream) -> c_int;
    count += 1;
    let _ = inflateReset as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = inflateReset2 as unsafe extern "C" fn(*mut z_stream, c_int) -> c_int;
    count += 1;
    let _ = inflatePrime as unsafe extern "C" fn(*mut z_stream, c_int, c_int) -> c_int;
    count += 1;
    let _ = inflateMark as unsafe extern "C" fn(*mut z_stream) -> c_long;
    count += 1;
    let _ = inflateGetHeader as unsafe extern "C" fn(*mut z_stream, *mut gz_header) -> c_int;
    count += 1;
    let _ = inflateBack
        as unsafe extern "C" fn(
            *mut z_stream,
            in_func,
            *mut c_void,
            out_func,
            *mut c_void,
        ) -> c_int;
    count += 1;
    let _ = inflateBackEnd as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = zlibCompileFlags as extern "C" fn() -> c_ulong;
    count += 1;

    // --- Utility functions (10) -------------------------------------------
    let _ = compress as unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong) -> c_int;
    count += 1;
    let _ = compress2
        as unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong, c_int) -> c_int;
    count += 1;
    let _ =
        compress_z as unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t) -> c_int;
    count += 1;
    let _ = compress2_z
        as unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t, c_int) -> c_int;
    count += 1;
    let _ = compressBound as extern "C" fn(c_ulong) -> c_ulong;
    count += 1;
    let _ = compressBound_z as extern "C" fn(z_size_t) -> z_size_t;
    count += 1;
    let _ = uncompress as unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, c_ulong) -> c_int;
    count += 1;
    let _ = uncompress2
        as unsafe extern "C" fn(*mut u8, *mut c_ulong, *const u8, *mut c_ulong) -> c_int;
    count += 1;
    let _ =
        uncompress_z as unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, z_size_t) -> c_int;
    count += 1;
    let _ = uncompress2_z
        as unsafe extern "C" fn(*mut u8, *mut z_size_t, *const u8, *mut z_size_t) -> c_int;
    count += 1;

    // --- Gz functions (27) ------------------------------------------------
    let _ = gzopen as unsafe extern "C" fn(*const c_char, *const c_char) -> gzFile;
    count += 1;
    let _ = gzdopen as unsafe extern "C" fn(c_int, *const c_char) -> gzFile;
    count += 1;
    let _ = gzbuffer as unsafe extern "C" fn(gzFile, c_uint) -> c_int;
    count += 1;
    let _ = gzsetparams as unsafe extern "C" fn(gzFile, c_int, c_int) -> c_int;
    count += 1;
    let _ = gzread as unsafe extern "C" fn(gzFile, *mut c_void, c_uint) -> c_int;
    count += 1;
    let _ = gzfread as unsafe extern "C" fn(*mut c_void, z_size_t, z_size_t, gzFile) -> z_size_t;
    count += 1;
    let _ = gzwrite as unsafe extern "C" fn(gzFile, *const c_void, c_uint) -> c_int;
    count += 1;
    let _ = gzfwrite as unsafe extern "C" fn(*const c_void, z_size_t, z_size_t, gzFile) -> z_size_t;
    count += 1;
    let _ = gzprintf as unsafe extern "C" fn(gzFile, *const c_char) -> c_int;
    count += 1;
    let _ = gzvprintf as unsafe extern "C" fn(gzFile, *const c_char, *mut c_void) -> c_int;
    count += 1;
    let _ = gzputs as unsafe extern "C" fn(gzFile, *const c_char) -> c_int;
    count += 1;
    let _ = gzgets as unsafe extern "C" fn(gzFile, *mut c_char, c_int) -> *mut c_char;
    count += 1;
    let _ = gzputc as unsafe extern "C" fn(gzFile, c_int) -> c_int;
    count += 1;
    let _ = gzgetc as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzungetc as unsafe extern "C" fn(c_int, gzFile) -> c_int;
    count += 1;
    let _ = gzflush as unsafe extern "C" fn(gzFile, c_int) -> c_int;
    count += 1;
    let _ = gzseek as unsafe extern "C" fn(gzFile, z_off_t, c_int) -> z_off_t;
    count += 1;
    let _ = gzrewind as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gztell as unsafe extern "C" fn(gzFile) -> z_off_t;
    count += 1;
    let _ = gzoffset as unsafe extern "C" fn(gzFile) -> z_off_t;
    count += 1;
    let _ = gzeof as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzdirect as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzclose as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzclose_r as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzclose_w as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = gzerror as unsafe extern "C" fn(gzFile, *mut c_int) -> *const c_char;
    count += 1;
    let _ = gzclearerr as unsafe extern "C" fn(gzFile);
    count += 1;

    // --- Large file functions (7) -----------------------------------------
    let _ = gzopen64 as unsafe extern "C" fn(*const c_char, *const c_char) -> gzFile;
    count += 1;
    let _ = gzseek64 as unsafe extern "C" fn(gzFile, z_off64_t, c_int) -> z_off64_t;
    count += 1;
    let _ = gztell64 as unsafe extern "C" fn(gzFile) -> z_off64_t;
    count += 1;
    let _ = gzoffset64 as unsafe extern "C" fn(gzFile) -> z_off64_t;
    count += 1;
    let _ = adler32_combine64 as extern "C" fn(c_ulong, c_ulong, z_off64_t) -> c_ulong;
    count += 1;
    let _ = crc32_combine64 as extern "C" fn(c_ulong, c_ulong, z_off64_t) -> c_ulong;
    count += 1;
    let _ = crc32_combine_gen64 as extern "C" fn(z_off64_t) -> c_ulong;
    count += 1;

    // --- Checksum functions (8) -------------------------------------------
    let _ = adler32 as unsafe extern "C" fn(c_ulong, *const u8, c_uint) -> c_ulong;
    count += 1;
    let _ = adler32_z as unsafe extern "C" fn(c_ulong, *const u8, usize) -> c_ulong;
    count += 1;
    let _ = crc32 as unsafe extern "C" fn(c_ulong, *const u8, c_uint) -> c_ulong;
    count += 1;
    let _ = crc32_z as unsafe extern "C" fn(c_ulong, *const u8, usize) -> c_ulong;
    count += 1;
    let _ = adler32_combine as extern "C" fn(c_ulong, c_ulong, z_off_t) -> c_ulong;
    count += 1;
    let _ = crc32_combine as extern "C" fn(c_ulong, c_ulong, z_off_t) -> c_ulong;
    count += 1;
    let _ = crc32_combine_gen as extern "C" fn(z_off_t) -> c_ulong;
    count += 1;
    let _ = crc32_combine_op as extern "C" fn(c_ulong, c_ulong, c_ulong) -> c_ulong;
    count += 1;

    // --- Various hacks (15) -----------------------------------------------
    let _ =
        deflateInit_ as unsafe extern "C" fn(*mut z_stream, c_int, *const c_char, c_int) -> c_int;
    count += 1;
    let _ = deflateInit2_
        as unsafe extern "C" fn(
            *mut z_stream,
            c_int,
            c_int,
            c_int,
            c_int,
            c_int,
            *const c_char,
            c_int,
        ) -> c_int;
    count += 1;
    let _ = inflateInit_ as unsafe extern "C" fn(*mut z_stream, *const c_char, c_int) -> c_int;
    count += 1;
    let _ =
        inflateInit2_ as unsafe extern "C" fn(*mut z_stream, c_int, *const c_char, c_int) -> c_int;
    count += 1;
    let _ = inflateBackInit_
        as unsafe extern "C" fn(*mut z_stream, c_int, *mut c_uchar, *const c_char, c_int) -> c_int;
    count += 1;
    let _ = gzgetc_ as unsafe extern "C" fn(gzFile) -> c_int;
    count += 1;
    let _ = zError as extern "C" fn(c_int) -> *const c_char;
    count += 1;
    let _ = inflateSyncPoint as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = get_crc_table as extern "C" fn() -> *const u32;
    count += 1;
    let _ = inflateUndermine as unsafe extern "C" fn(*mut z_stream, c_int) -> c_int;
    count += 1;
    let _ = inflateValidate as unsafe extern "C" fn(*mut z_stream, c_int) -> c_int;
    count += 1;
    let _ = inflateCodesUsed as unsafe extern "C" fn(*mut z_stream) -> c_ulong;
    count += 1;
    let _ = inflateResetKeep as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    let _ = deflateResetKeep as unsafe extern "C" fn(*mut z_stream) -> c_int;
    count += 1;
    // gzopen_w — platform-conditional signature
    count += 1;
    let _ = gzopen_w; // compile-time presence check

    assert_eq!(
        count, 96,
        "Expected 96 symbols from win32/zlib.def, counted {count}"
    );
}

// ===========================================================================
// Section 10: Basic Smoke Tests Through FFI
// ===========================================================================

/// End-to-end compress → uncompress round-trip via FFI functions.
///
/// Uses `compress` and `uncompress` directly to verify the C-compatible API
/// produces correct output.
#[test]
fn test_ffi_compress_decompress_smoke() {
    let source = b"Hello, zlib-rs FFI compatibility test! \
                   This string exercises the compress/uncompress round-trip.";
    let source_len = source.len() as c_ulong;

    // Determine the upper bound for the compressed output.
    let bound = compressBound(source_len);
    let mut compressed = vec![0u8; bound as usize];
    let mut compressed_len: c_ulong = bound;

    // Compress.
    let rc = unsafe {
        compress(
            compressed.as_mut_ptr(),
            &mut compressed_len,
            source.as_ptr(),
            source_len,
        )
    };
    assert_eq!(rc, Z_OK, "compress() returned {rc}, expected Z_OK (0)");
    assert!(
        compressed_len > 0 && compressed_len < bound,
        "unexpected compressed length: {compressed_len}"
    );

    // Decompress.
    let mut decompressed = vec![0u8; source.len()];
    let mut decompressed_len: c_ulong = source.len() as c_ulong;

    let rc = unsafe {
        uncompress(
            decompressed.as_mut_ptr(),
            &mut decompressed_len,
            compressed.as_ptr(),
            compressed_len,
        )
    };
    assert_eq!(rc, Z_OK, "uncompress() returned {rc}, expected Z_OK (0)");
    assert_eq!(decompressed_len, source_len, "decompressed length mismatch");
    assert_eq!(
        &decompressed[..decompressed_len as usize],
        &source[..],
        "decompressed data does not match original"
    );
}

/// Streaming deflate → inflate round-trip via FFI `deflateInit_` / `deflate`
/// / `deflateEnd` / `inflateInit_` / `inflate` / `inflateEnd`.
#[test]
fn test_ffi_streaming_deflate_inflate_smoke() {
    let input = b"The quick brown fox jumps over the lazy dog. \
                  Pack my box with five dozen liquor jugs.";
    let input_len = input.len();

    // --- Compress ---
    let mut strm = z_stream::zeroed();
    let version = ZLIB_VERSION.as_ptr().cast::<c_char>();
    let stream_size = size_of::<z_stream>() as c_int;

    let rc = unsafe { deflateInit_(&mut strm, Z_DEFAULT_COMPRESSION, version, stream_size) };
    assert_eq!(rc, Z_OK, "deflateInit_ failed: {rc}");

    let bound = unsafe { deflateBound(&mut strm, input_len as c_ulong) };
    let mut compressed = vec![0u8; bound as usize];

    strm.next_in = input.as_ptr();
    strm.avail_in = input_len as c_uint;
    strm.next_out = compressed.as_mut_ptr();
    strm.avail_out = compressed.len() as c_uint;

    let rc = unsafe { deflate(&mut strm, Z_FINISH) };
    assert_eq!(
        rc, Z_STREAM_END,
        "deflate() returned {rc}, expected Z_STREAM_END"
    );

    let compressed_len = strm.total_out as usize;
    let rc = unsafe { deflateEnd(&mut strm) };
    assert_eq!(rc, Z_OK, "deflateEnd failed: {rc}");

    // --- Decompress ---
    let mut strm = z_stream::zeroed();
    let rc = unsafe { inflateInit_(&mut strm, version, stream_size) };
    assert_eq!(rc, Z_OK, "inflateInit_ failed: {rc}");

    let mut decompressed = vec![0u8; input_len + 64];
    strm.next_in = compressed.as_ptr();
    strm.avail_in = compressed_len as c_uint;
    strm.next_out = decompressed.as_mut_ptr();
    strm.avail_out = decompressed.len() as c_uint;

    let rc = unsafe { inflate(&mut strm, Z_FINISH) };
    assert_eq!(
        rc, Z_STREAM_END,
        "inflate() returned {rc}, expected Z_STREAM_END"
    );

    let decompressed_len = strm.total_out as usize;
    let rc = unsafe { inflateEnd(&mut strm) };
    assert_eq!(rc, Z_OK, "inflateEnd failed: {rc}");

    assert_eq!(decompressed_len, input_len, "decompressed size mismatch");
    assert_eq!(
        &decompressed[..decompressed_len],
        &input[..],
        "decompressed data does not match original"
    );
}

// ===========================================================================
// Section 11: Struct Layout Verification
// ===========================================================================

/// Verify `z_stream` struct has the expected `#[repr(C)]` layout.
///
/// The struct size depends on the target platform:
/// - LP64  (64-bit Unix/Linux/macOS): `c_ulong` = 8, pointer = 8 → 112 bytes
/// - LLP64 (64-bit Windows):          `c_ulong` = 4, pointer = 8 → 88 bytes
/// - ILP32 (32-bit):                   `c_ulong` = 4, pointer = 4 → 56 bytes
#[test]
fn test_ffi_z_stream_layout() {
    // Platform-specific total size checks.
    #[cfg(all(target_pointer_width = "64", not(target_os = "windows")))]
    {
        assert_eq!(
            size_of::<z_stream>(),
            112,
            "z_stream size on LP64 should be 112 bytes"
        );
    }

    #[cfg(all(target_pointer_width = "64", target_os = "windows"))]
    {
        assert_eq!(
            size_of::<z_stream>(),
            88,
            "z_stream size on LLP64 should be 88 bytes"
        );
    }

    #[cfg(target_pointer_width = "32")]
    {
        assert_eq!(
            size_of::<z_stream>(),
            56,
            "z_stream size on ILP32 should be 56 bytes"
        );
    }

    // Verify that z_stream has the expected number of fields by checking
    // that a zeroed instance can be created and its fields are accessible.
    let strm = z_stream::zeroed();
    assert!(strm.next_in.is_null());
    assert_eq!(strm.avail_in, 0);
    assert_eq!(strm.total_in, 0);
    assert!(strm.next_out.is_null());
    assert_eq!(strm.avail_out, 0);
    assert_eq!(strm.total_out, 0);
    assert!(strm.msg.is_null());
    assert!(strm.state.is_null());
    assert!(strm.zalloc.is_none());
    assert!(strm.zfree.is_none());
    assert!(strm.opaque.is_null());
    assert_eq!(strm.data_type, 0);
    assert_eq!(strm.adler, 0);
    assert_eq!(strm.reserved, 0);
}

/// Verify `gz_header` struct is accessible and zeroed correctly.
#[test]
fn test_ffi_gz_header_layout() {
    // gz_header is a #[repr(C)] struct with 13 fields.
    let hdr = gz_header {
        text: 0,
        time: 0,
        xflags: 0,
        os: 0,
        extra: null_mut(),
        extra_len: 0,
        extra_max: 0,
        name: null_mut(),
        name_max: 0,
        comment: null_mut(),
        comm_max: 0,
        hcrc: 0,
        done: 0,
    };
    assert_eq!(hdr.text, 0);
    assert!(hdr.extra.is_null());
    assert!(hdr.name.is_null());
    assert!(hdr.comment.is_null());
}

// ===========================================================================
// Section 12: Constant Validation
// ===========================================================================

/// Verify that the FFI crate exports the expected zlib constants.
#[test]
fn test_ffi_constants() {
    // Return codes (zlib.h lines 181–189).
    assert_eq!(Z_OK, 0);
    assert_eq!(Z_STREAM_END, 1);

    // Flush modes (zlib.h lines 172–178).
    assert_eq!(Z_NO_FLUSH, 0);
    assert_eq!(Z_FINISH, 4);

    // Compression method.
    assert_eq!(Z_DEFLATED, 8);

    // Configuration limits.
    assert_eq!(MAX_WBITS, 15);
    assert_eq!(DEF_MEM_LEVEL, 8);

    // Version.
    assert_eq!(ZLIB_VERNUM, 0x1321);
    assert!(
        ZLIB_VERSION.starts_with(b"1.3.2.1"),
        "unexpected ZLIB_VERSION bytes"
    );
}

/// Verify null-pointer rejection by selected FFI functions.
///
/// Per AAP Section 0.7.3: "Every extern 'C' function must validate all
/// pointer arguments for null before dereferencing."
#[test]
fn test_ffi_null_pointer_rejection() {
    // deflateEnd with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { deflateEnd(null_mut()) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "deflateEnd(null) should return Z_STREAM_ERROR, got {rc}"
    );

    // inflateEnd with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { inflateEnd(null_mut()) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "inflateEnd(null) should return Z_STREAM_ERROR, got {rc}"
    );

    // deflateReset with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { deflateReset(null_mut()) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "deflateReset(null) should return Z_STREAM_ERROR, got {rc}"
    );

    // inflateReset with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { inflateReset(null_mut()) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "inflateReset(null) should return Z_STREAM_ERROR, got {rc}"
    );

    // deflateSetDictionary with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { deflateSetDictionary(null_mut(), null(), 0) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "deflateSetDictionary(null) should return Z_STREAM_ERROR, got {rc}"
    );

    // inflateSetDictionary with null stream should return Z_STREAM_ERROR.
    let rc = unsafe { inflateSetDictionary(null_mut(), null(), 0) };
    assert_eq!(
        rc, Z_STREAM_ERROR,
        "inflateSetDictionary(null) should return Z_STREAM_ERROR, got {rc}"
    );
}

/// Verify the version constants match between the version string and the
/// packed version number.
#[test]
fn test_ffi_version_consistency() {
    let version_str = {
        let ptr = zlibVersion();
        assert!(!ptr.is_null());
        unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .expect("invalid UTF-8 in version string")
            .to_owned()
    };

    // The version string should contain the major.minor.revision prefix
    // matching ZLIB_VERNUM = 0x1321.
    assert!(
        version_str.starts_with("1.3.2"),
        "version string '{version_str}' does not start with '1.3.2'"
    );

    // The packed version number should have correct major/minor/revision.
    let major = (ZLIB_VERNUM >> 12) & 0xF;
    let minor = (ZLIB_VERNUM >> 8) & 0xF;
    let revision = (ZLIB_VERNUM >> 4) & 0xF;
    assert_eq!(major, 1, "major version from ZLIB_VERNUM");
    assert_eq!(minor, 3, "minor version from ZLIB_VERNUM");
    assert_eq!(revision, 2, "revision from ZLIB_VERNUM");
}
