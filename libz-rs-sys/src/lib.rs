//! C-compatible FFI bindings for zlib-rs, a pure Rust zlib implementation.
//!
//! This crate provides `extern "C"` function wrappers that expose the zlib API
//! through C-compatible symbols, enabling use as a drop-in replacement for
//! the C zlib library (`libz.so` / `zlib1.dll`).
//!
//! All 105 symbols from `win32/zlib.def` are re-exported at the crate root so
//! that they are visible to the linker when this crate is used as a dependency
//! (e.g., by the `libz-rs-sys-cdylib` shared library crate). Symbol names
//! match the C API exactly — no Rust module path prefixing.
//!
//! # Modules
//!
//! - [`types`]    — `#[repr(C)]` type definitions: [`z_stream`], [`gz_header`],
//!                  function pointer typedefs, type aliases, and all zlib
//!                  C-ABI constants (`Z_OK` through `Z_VERSION_ERROR`, flush
//!                  modes, strategies, levels, etc.).
//! - [`deflate`]  — Deflate (compression) functions (16 symbols).
//! - [`inflate`]  — Inflate (decompression) functions (16 symbols).
//! - [`checksum`] — Adler-32 and CRC-32 checksum functions (11 symbols).
//! - [`compress`] — One-call compress/uncompress wrappers (10 symbols).
//! - [`version`]  — Version info and undocumented utility functions (10 symbols).
//! - [`gz`]       — Gzip file I/O functions (33 symbols). Conditionally compiled
//!                  with the `gz-io` Cargo feature (corresponding to the C
//!                  `Z_SOLO` exclusion pattern).
//!
//! # Version Constants
//!
//! Crate-level constants ([`ZLIB_VERSION`], [`ZLIB_VERNUM`], etc.) match the
//! values from `zlib.h` lines 44–49 for ABI compatibility checking by the
//! `deflateInit_` / `inflateInit_` family of functions.
//!
//! # Safety
//!
//! This crate is the **only** crate in the workspace permitted to contain
//! `unsafe` code. All `unsafe` usage is confined to FFI boundary operations:
//!
//! 1. Every `extern "C"` function validates all pointer arguments for null
//!    before dereferencing.
//! 2. Slice reconstruction from raw pointers uses `std::slice::from_raw_parts`
//!    with length validation.
//! 3. The `z_stream` `#[repr(C)]` struct is layout-verified via compile-time
//!    `size_of` assertions (see [`types::z_stream`]).
//! 4. All functions use `extern "C"` (not `extern "C-unwind"`) to abort on
//!    panic at the FFI boundary rather than triggering undefined behavior
//!    through stack unwinding into C code.
//! 5. The `libz-rs-sys-cdylib` crate sets `panic = "abort"` in its release
//!    profile as an additional safety measure.

// ---------------------------------------------------------------------------
// Crate-level lint configuration
// ---------------------------------------------------------------------------
//
// Workspace-level `[workspace.lints.clippy]` sets `all = "deny"` and
// `pedantic = "deny"`. For the FFI crate we downgrade `pedantic` to `warn`
// because FFI bindings inherently contain many patterns flagged by pedantic
// lints (e.g., wildcard re-exports, c_int return types, module name
// repetitions in `deflate::deflate`, etc.).
//
// The `non_snake_case` allow is required because the C API uses camelCase
// function names (deflateInit, inflateEnd, compressBound, etc.).
// The `non_camel_case_types` allow is required because C types use snake_case
// (z_stream, gz_header, alloc_func, in_func, out_func, gzFile, etc.).
#![warn(clippy::pedantic)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::wildcard_imports)]

// ===========================================================================
// Module declarations
// ===========================================================================
//
// Each sub-module contains `#[unsafe(no_mangle)] extern "C"` wrapper functions
// that bridge between the C zlib API (raw pointers, c_int return codes, and
// the #[repr(C)] z_stream struct) and the safe Rust implementations in the
// `zlib-rs` core crate.

/// `#[repr(C)]` FFI type definitions for C ABI compatibility with zlib.
///
/// Contains [`z_stream`], [`gz_header`], [`gzFile_s`]/[`gzFile`], function
/// pointer typedefs ([`alloc_func`], [`free_func`], [`in_func`], [`out_func`]),
/// type aliases ([`z_off_t`], [`z_off64_t`], [`z_size_t`], [`z_streamp`],
/// [`gz_headerp`]), and all zlib C-ABI constants (return codes, flush modes,
/// compression levels, strategies, data type hints, and configuration limits).
pub mod types;

/// Deflate (compression) FFI wrappers.
///
/// All 16 deflate symbols from `win32/zlib.def`:
/// `deflateInit_`, `deflateInit2_`, `deflate`, `deflateEnd`,
/// `deflateSetDictionary`, `deflateGetDictionary`, `deflateCopy`,
/// `deflateReset`, `deflateParams`, `deflateTune`, `deflateBound`,
/// `deflateBound_z`, `deflatePending`, `deflateUsed`, `deflatePrime`,
/// `deflateSetHeader`.
pub mod deflate;

/// Inflate (decompression) FFI wrappers.
///
/// All 16 inflate symbols from `win32/zlib.def`:
/// `inflateInit_`, `inflateInit2_`, `inflate`, `inflateEnd`,
/// `inflateSetDictionary`, `inflateGetDictionary`, `inflateSync`,
/// `inflateCopy`, `inflateReset`, `inflateReset2`, `inflatePrime`,
/// `inflateMark`, `inflateGetHeader`, `inflateBack`, `inflateBackInit_`,
/// `inflateBackEnd`.
pub mod inflate;

/// Adler-32 and CRC-32 checksum FFI wrappers.
///
/// All 11 checksum symbols from `win32/zlib.def`:
/// `adler32`, `adler32_z`, `adler32_combine`, `adler32_combine64`,
/// `crc32`, `crc32_z`, `crc32_combine`, `crc32_combine64`,
/// `crc32_combine_gen`, `crc32_combine_gen64`, `crc32_combine_op`.
pub mod checksum;

/// One-call compress/uncompress FFI wrappers.
///
/// All 10 compress/uncompress symbols from `win32/zlib.def`:
/// `compress`, `compress2`, `compress_z`, `compress2_z`,
/// `compressBound`, `compressBound_z`,
/// `uncompress`, `uncompress2`, `uncompress_z`, `uncompress2_z`.
pub mod compress;

/// Version, informational, and undocumented utility FFI wrappers.
///
/// All 10 version/info symbols from `win32/zlib.def`:
/// `zlibVersion`, `zlibCompileFlags`, `zError`,
/// `inflateSyncPoint`, `get_crc_table`, `inflateUndermine`,
/// `inflateValidate`, `inflateCodesUsed`,
/// `inflateResetKeep`, `deflateResetKeep`.
pub mod version;

/// Gzip file I/O FFI wrappers.
///
/// All 33 `gz*` symbols from `win32/zlib.def`. Conditionally compiled with
/// the `gz-io` Cargo feature, corresponding to the C `Z_SOLO` exclusion
/// pattern from `gzguts.h`. When `Z_SOLO` was defined in the C library,
/// gzip file I/O was excluded; likewise, when the `gz-io` feature is
/// *disabled*, this module is not compiled.
///
/// Symbols: `gzopen`, `gzopen64`, `gzopen_w`, `gzdopen`, `gzbuffer`,
/// `gzsetparams`, `gzread`, `gzfread`, `gzwrite`, `gzfwrite`, `gzprintf`,
/// `gzvprintf`, `gzputs`, `gzgets`, `gzputc`, `gzgetc`, `gzgetc_`,
/// `gzungetc`, `gzflush`, `gzseek`, `gzseek64`, `gzrewind`, `gztell`,
/// `gztell64`, `gzoffset`, `gzoffset64`, `gzeof`, `gzdirect`, `gzclose`,
/// `gzclose_r`, `gzclose_w`, `gzerror`, `gzclearerr`.
#[cfg(feature = "gz-io")]
pub mod gz;

// ===========================================================================
// Re-exports — make all symbols visible at the crate root
// ===========================================================================
//
// Using `pub use module::*` for each sub-module ensures that every
// `#[no_mangle] pub extern "C"` function is accessible from the crate root.
// This is required so that:
//
// 1. The `libz-rs-sys-cdylib` crate can re-export all symbols by simply
//    depending on this crate.
// 2. Symbol names match the C API exactly (no Rust module path prefixing)
//    when the shared library is produced.
// 3. External crates can import any symbol directly from `libz_rs_sys::*`.

/// Re-export all `#[repr(C)]` types, type aliases, and constants.
pub use types::*;

/// Re-export all deflate (compression) FFI functions.
pub use deflate::*;

/// Re-export all inflate (decompression) FFI functions.
pub use inflate::*;

/// Re-export all checksum (Adler-32, CRC-32) FFI functions.
pub use checksum::*;

/// Re-export all one-call compress/uncompress FFI functions.
pub use compress::*;

/// Re-export all version/info FFI functions.
pub use version::*;

/// Re-export all gzip file I/O FFI functions (when the `gz-io` feature is
/// enabled).
#[cfg(feature = "gz-io")]
pub use gz::*;

// ===========================================================================
// Version constants — match zlib.h lines 44–49 exactly
// ===========================================================================
//
// These constants are used by the FFI init functions (`deflateInit_`,
// `inflateInit_`, etc.) for ABI version compatibility checks. The version
// string is null-terminated so it can be safely cast to `*const c_char`
// for the `zlibVersion()` return value.

/// zlib version string.
///
/// Null-terminated byte string matching the C `ZLIB_VERSION` macro from
/// `zlib.h` line 44. Used by `zlibVersion()` to return the library version,
/// and by `deflateInit_`/`inflateInit_` for ABI compatibility checks.
///
/// The null terminator is included so this can be directly cast to
/// `*const c_char` for C interoperability.
pub const ZLIB_VERSION: &[u8] = b"1.3.2.1-motley\0";

/// zlib version number as a packed integer.
///
/// Matches the C `ZLIB_VERNUM` macro from `zlib.h` line 45.
/// Format: `0xMNRP` where M=major, N=minor, R=revision, P=sub-revision.
/// For version 1.3.2.1: `0x1321`.
pub const ZLIB_VERNUM: u32 = 0x1321;

/// Major version number (1).
///
/// Matches `ZLIB_VER_MAJOR` from `zlib.h` line 46.
pub const ZLIB_VER_MAJOR: u32 = 1;

/// Minor version number (3).
///
/// Matches `ZLIB_VER_MINOR` from `zlib.h` line 47.
pub const ZLIB_VER_MINOR: u32 = 3;

/// Revision number (2).
///
/// Matches `ZLIB_VER_REVISION` from `zlib.h` line 48.
pub const ZLIB_VER_REVISION: u32 = 2;

/// Sub-revision number (1).
///
/// Matches `ZLIB_VER_SUBREVISION` from `zlib.h` line 49.
pub const ZLIB_VER_SUBREVISION: u32 = 1;

// ===========================================================================
// Compile-time version verification
// ===========================================================================
//
// Ensure the packed version number is consistent with the individual
// version components. This catches any accidental mismatch if someone
// updates one constant but not the others.

/// Compile-time assertion verifying that [`ZLIB_VERNUM`] is consistent
/// with [`ZLIB_VER_MAJOR`], [`ZLIB_VER_MINOR`], [`ZLIB_VER_REVISION`],
/// and [`ZLIB_VER_SUBREVISION`].
const _: () = {
    let expected = (ZLIB_VER_MAJOR << 12)
        | (ZLIB_VER_MINOR << 8)
        | (ZLIB_VER_REVISION << 4)
        | ZLIB_VER_SUBREVISION;
    assert!(
        ZLIB_VERNUM == expected,
        "ZLIB_VERNUM does not match version components"
    );
};

/// Compile-time assertion verifying that the version string starts with
/// the correct major.minor.revision.subrevision prefix.
const _: () = {
    // "1.3.2.1-motley\0"
    //  ^ major
    assert!(ZLIB_VERSION[0] == b'0' + ZLIB_VER_MAJOR as u8);
    assert!(ZLIB_VERSION[1] == b'.');
    assert!(ZLIB_VERSION[2] == b'0' + ZLIB_VER_MINOR as u8);
    assert!(ZLIB_VERSION[3] == b'.');
    assert!(ZLIB_VERSION[4] == b'0' + ZLIB_VER_REVISION as u8);
    assert!(ZLIB_VERSION[5] == b'.');
    assert!(ZLIB_VERSION[6] == b'0' + ZLIB_VER_SUBREVISION as u8);
    // Verify null terminator
    assert!(ZLIB_VERSION[ZLIB_VERSION.len() - 1] == 0);
};
