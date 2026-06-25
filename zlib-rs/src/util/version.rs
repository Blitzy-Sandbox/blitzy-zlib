//! Version, compile-flags, and error-string diagnostic helpers.
//!
//! This module is the safe-Rust port of the diagnostic surface of the C
//! `zutil.c` translation unit — `zlibVersion()`, `zlibCompileFlags()`, and
//! `zError()` — together with the version constants declared in `zlib.h`
//! (lines 44-49). It backs the public `zlibVersion` / `zlibCompileFlags` /
//! `zError` symbols that the thin `libz-rs-sys` FFI shim re-exposes with the
//! exact C ABI.
//!
//! # Single source of truth
//!
//! Every version value lives in [`crate::constants`]; this module only
//! *re-exports* the numeric constants and *wraps* the string constant, so the
//! version number is defined exactly once. Likewise the human-readable status
//! strings live in [`crate::error`]: [`z_error`] is a thin delegate to
//! [`crate::error::err_msg`] and never duplicates the `z_errmsg` table.
//!
//! # `no_std` and safety
//!
//! The module is allocation-free and references only `core`, so it compiles
//! unchanged under `#![no_std]` (`cargo build --no-default-features`). It
//! contains no `unsafe` — consistent with the crate-wide
//! `#![forbid(unsafe_code)]` — and none of its helpers can panic on any input.

use crate::constants::ZLIB_VERSION;
use crate::error::err_msg;
use core::ffi::{c_long, c_uint, c_ulong, c_void};

/// Re-export of the numeric version constants so they are reachable directly at
/// `crate::util::version::*`.
///
/// The FFI shim's `ZLIB_VERNUM` parity check and the parent `util` module's
/// public re-export both depend on these being visible here. The values are
/// fixed by the C baseline: [`ZLIB_VERNUM`] is `0x1321`, [`ZLIB_VER_MAJOR`] is
/// `1`, [`ZLIB_VER_MINOR`] is `3`, [`ZLIB_VER_REVISION`] is `2`, and
/// [`ZLIB_VER_SUBREVISION`] is `1`.
pub use crate::constants::{
    ZLIB_VER_MAJOR, ZLIB_VER_MINOR, ZLIB_VER_REVISION, ZLIB_VER_SUBREVISION, ZLIB_VERNUM,
};

/// Returns the zlib version string (for example `"1.3.2.1-motley"`).
///
/// This mirrors the C `zlibVersion()` function, which simply returns the
/// compile-time `ZLIB_VERSION` macro. Applications compare the returned string
/// against the [`ZLIB_VERSION`](crate::constants::ZLIB_VERSION) value they were
/// built with to detect a mismatched shared library at run time.
#[must_use]
pub const fn zlib_version() -> &'static str {
    // Delegate to the single source of truth in `crate::constants` rather than
    // hardcoding the literal, so the version string is defined exactly once.
    ZLIB_VERSION
}

/// Maps a zlib return/error code to its human-readable message.
///
/// This mirrors the C `zError(err)` function, which expands to the `ERR_MSG`
/// macro `z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]`. The
/// clamp-and-index logic and the exact message strings live in
/// [`crate::error::err_msg`]; this function is a thin delegate so the
/// `z_errmsg` table is never duplicated here.
///
/// Any code outside the inclusive range `-6..=2` returns the empty string,
/// exactly as the C macro indexes the trailing empty slot (`z_errmsg[9]`). For
/// example, `z_error(-3)` is `"data error"`, `z_error(0)` is `""`, and any
/// out-of-range code such as `z_error(42)` is also `""`.
#[must_use]
pub fn z_error(code: i32) -> &'static str {
    err_msg(code)
}

/// Returns a bitmask describing compile-time configuration, mirroring C
/// `zlibCompileFlags()`.
///
/// The low eight bits encode the sizes of the C scalar types used at the FFI
/// boundary, two bits per type, so that C callers (via `libz-rs-sys`) observe
/// flags consistent with the target platform's data model. The remaining
/// documented bits describe optional compile-time features; none are enabled in
/// this standard, safe-Rust build, so they are all zero.
///
/// The size fields are computed dynamically with [`core::mem::size_of`], so the
/// result is platform-dependent — for example a 64-bit Unix (LP64) build yields
/// `0xA9`, whereas a 64-bit Windows (LLP64) build yields `0x65`.
#[must_use]
pub fn zlib_compile_flags() -> u32 {
    /// Encodes a C type size into the 2-bit code used by `zlibCompileFlags`:
    /// `2 -> 0`, `4 -> 1`, `8 -> 2`, and any other size `-> 3`. This reproduces
    /// the C `switch ((int)sizeof(...))` ladder exactly.
    const fn size_code(size: usize) -> u32 {
        match size {
            2 => 0,
            4 => 1,
            8 => 2,
            _ => 3,
        }
    }

    let mut flags: u32 = 0;

    // bits 0-1: sizeof(uInt)   == sizeof(c_uint)
    flags |= size_code(core::mem::size_of::<c_uint>());
    // bits 2-3: sizeof(uLong)  == sizeof(c_ulong)
    flags |= size_code(core::mem::size_of::<c_ulong>()) << 2;
    // bits 4-5: sizeof(voidpf) == sizeof(*const c_void)
    flags |= size_code(core::mem::size_of::<*const c_void>()) << 4;
    // bits 6-7: sizeof(z_off_t); zlib's z_off_t defaults to the C `long` type.
    flags |= size_code(core::mem::size_of::<c_long>()) << 6;

    // The remaining flag bits describe optional compile-time features. None are
    // active in this standard, safe-Rust build, so each contributes 0:
    //
    //   bit  8  ZLIB_DEBUG            debug build               -> 0
    //   bit  9  ASMV / ASMINF         hand-written assembly     -> 0 (commented out in C)
    //   bit 10  ZLIB_WINAPI           WINAPI calling convention -> 0
    //   bit 12  BUILDFIXED            build fixed Huffman tables -> 0
    //   bit 13  DYNAMIC_CRC_TABLE     runtime CRC table         -> 0
    //   bit 16  NO_GZCOMPRESS         gzip write disabled       -> 0 (gz write supported)
    //   bit 17  NO_GZIP               gzip support disabled     -> 0 (gzip supported)
    //   bit 20  PKZIP_BUG_WORKAROUND  legacy PKZIP workaround   -> 0
    //   bit 21  FASTEST               fastest-only deflate      -> 0
    //   bits 24-27  (v)snprintf / sprintf availability:
    //           Rust's `core::fmt` is always present and bounds-checked, which
    //           corresponds to the C "STDC defined and vsnprintf available"
    //           case, so none of bits 24-27 are set.
    flags
}

/// Returns the `zlibCompileFlags()` bitmask for a given `z_off_t` size.
///
/// This is the parameterized variant of [`zlib_compile_flags`]: the low eight
/// bits encode the sizes of the C scalar types, two bits per type, and the
/// `z_off_t` field (bits 6-7) is supplied by the caller. The C ABI shim
/// (`libz-rs-sys`) maps `z_off_t` to `libc::off_t` and therefore calls this
/// with `size_of::<off_t>()`, which need not equal the C `long` width that the
/// pure-Rust [`zlib_compile_flags`] assumes (notably on LLP64 / large-file
/// builds). Keeping this in the core lets it stay `no_std` and `libc`-free.
#[must_use]
pub const fn zlib_compile_flags_for(z_off_t_size: usize) -> u32 {
    /// Encodes a C type size into the 2-bit code used by `zlibCompileFlags`:
    /// `2 -> 0`, `4 -> 1`, `8 -> 2`, and any other size `-> 3`. This reproduces
    /// the C `switch ((int)sizeof(...))` ladder exactly.
    const fn size_code(size: usize) -> u32 {
        match size {
            2 => 0,
            4 => 1,
            8 => 2,
            _ => 3,
        }
    }

    let mut flags: u32 = 0;

    // bits 0-1: sizeof(uInt)   == sizeof(c_uint)
    flags |= size_code(core::mem::size_of::<c_uint>());
    // bits 2-3: sizeof(uLong)  == sizeof(c_ulong)
    flags |= size_code(core::mem::size_of::<c_ulong>()) << 2;
    // bits 4-5: sizeof(voidpf) == sizeof(*const c_void)
    flags |= size_code(core::mem::size_of::<*const c_void>()) << 4;
    // bits 6-7: sizeof(z_off_t), supplied by the caller because the C ABI layer
    // maps `z_off_t` to `libc::off_t`, which need not equal the C `long`.
    flags |= size_code(z_off_t_size) << 6;

    // The remaining flag bits describe optional compile-time features. None are
    // active in this standard, safe-Rust build, so each contributes 0.
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_matches_baseline() {
        // `zlib_version` must surface the exact upstream baseline string and
        // must not hardcode it independently of `crate::constants`.
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version(), ZLIB_VERSION);
    }

    #[test]
    fn version_constants_match_baseline() {
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(ZLIB_VER_MAJOR, 1);
        assert_eq!(ZLIB_VER_MINOR, 3);
        assert_eq!(ZLIB_VER_REVISION, 2);
        assert_eq!(ZLIB_VER_SUBREVISION, 1);
    }

    #[test]
    fn z_error_matches_z_errmsg_table() {
        // Every in-range code maps to the exact C `z_errmsg[10]` string.
        assert_eq!(z_error(2), "need dictionary");
        assert_eq!(z_error(1), "stream end");
        assert_eq!(z_error(0), "");
        assert_eq!(z_error(-1), "file error");
        assert_eq!(z_error(-2), "stream error");
        assert_eq!(z_error(-3), "data error");
        assert_eq!(z_error(-4), "insufficient memory");
        assert_eq!(z_error(-5), "buffer error");
        assert_eq!(z_error(-6), "incompatible version");
    }

    #[test]
    fn z_error_out_of_range_is_empty() {
        // Anything outside -6..=2 indexes the trailing empty slot.
        assert_eq!(z_error(3), "");
        assert_eq!(z_error(-7), "");
        assert_eq!(z_error(100), "");
        assert_eq!(z_error(i32::MIN), "");
        assert_eq!(z_error(i32::MAX), "");
    }

    #[test]
    fn compile_flags_uint_field_is_four_bytes() {
        // `c_uint` is 4 bytes on every supported target, so its 2-bit code is 1.
        assert_eq!(zlib_compile_flags() & 0b11, 1);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn compile_flags_pointer_field_is_eight_bytes() {
        // On a 64-bit target a pointer is 8 bytes, so its 2-bit code is 2.
        assert_eq!((zlib_compile_flags() >> 4) & 0b11, 2);
    }

    #[test]
    fn compile_flags_high_bits_are_clear() {
        // No optional compile-time features are enabled, so every bit from bit 8
        // upward must be zero in this standard build.
        assert_eq!(zlib_compile_flags() & 0xFFFF_FF00, 0);
    }
}
