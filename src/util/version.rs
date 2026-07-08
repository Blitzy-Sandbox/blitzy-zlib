//! Version and compile-flags reporting — a safe-Rust port of the public
//! reporting functions from the C `zutil.c` baseline of zlib
//! `1.3.2.1-motley`.
//!
//! This module reproduces the three side-effect-free, publicly exported
//! reporting entry points of the C library:
//!
//! * [`zlib_version`] ← C `zlibVersion` (`zutil.c` L27-29)
//! * [`zlib_compile_flags`] ← C `zlibCompileFlags` (`zutil.c` L31-121)
//! * [`z_error`] ← C `zError` (`zutil.c` L139-141)
//!
//! # Naming
//!
//! `src/lib.rs` re-exports the historical camelCase C names (`zlibVersion`,
//! `zlibCompileFlags`, `zError`) so Rust callers see the identifiers
//! documented in `zlib.h`. To keep the crate `clippy`-clean under the
//! `non_snake_case` lint, the *canonical* implementations use idiomatic
//! snake_case names, and each is paired with a thin `#[allow(non_snake_case)]`
//! camelCase wrapper that forwards to it.
//!
//! # Single source of truth
//!
//! This module deliberately **bridges** rather than **duplicates**:
//!
//! * the version string is owned by the crate root ([`crate::ZLIB_VERSION`]);
//! * the error-message table is owned by [`ReturnCode::message`], which
//!   already ports the C `z_errmsg` array.
//!
//! # `no_std`
//!
//! The module is `no_std`-clean: it uses only `core` (namely
//! [`core::mem::size_of`] and the [`core::ffi`] C-ABI integer types) together
//! with other crate modules. It contains no `unsafe` code.

use crate::error::ReturnCode;
use core::ffi::{c_long, c_uint, c_ulong, c_void};

/// Returns the zlib version string, exactly as the C `zlibVersion` function
/// does.
///
/// The returned string is the crate-wide single source of truth
/// [`crate::ZLIB_VERSION`] (`"1.3.2.1-motley"`); this function never introduces
/// a second string literal, guaranteeing the FFI `zlibVersion` shim and any
/// idiomatic Rust caller observe an identical value.
#[inline]
#[must_use]
pub fn zlib_version() -> &'static str {
    crate::ZLIB_VERSION
}

/// C-compatible camelCase alias for [`zlib_version`].
///
/// Provided so `src/lib.rs` can re-export the historical `zlibVersion` name
/// from `zlib.h`. It forwards verbatim to [`zlib_version`].
#[allow(non_snake_case)]
#[inline]
#[must_use]
pub fn zlibVersion() -> &'static str {
    zlib_version()
}

/// Converts a zlib return code into its human-readable message, exactly as the
/// C `zError` function does.
///
/// This reproduces the C `ERR_MSG(err)` macro semantics precisely by bridging
/// to [`ReturnCode::message`] instead of re-declaring the `z_errmsg` table:
///
/// * for the nine defined codes in `-6..=2`, [`ReturnCode::from_c_int`] yields
///   `Some(code)` and the matching `z_errmsg` string is returned (with
///   [`ReturnCode::Ok`] mapping to the empty string, just like `z_errmsg[2]`);
/// * for any out-of-range code, [`ReturnCode::from_c_int`] yields `None` and
///   the empty string is returned, matching the C macro's out-of-range index
///   (`z_errmsg[9] == ""`).
#[inline]
#[must_use]
pub fn z_error(err: i32) -> &'static str {
    ReturnCode::from_c_int(err)
        .map(|rc| rc.message())
        .unwrap_or("")
}

/// C-compatible camelCase alias for [`z_error`].
///
/// Provided so `src/lib.rs` can re-export the historical `zError` name from
/// `zlib.h`. It forwards verbatim to [`z_error`].
#[allow(non_snake_case)]
#[inline]
#[must_use]
pub fn zError(err: i32) -> &'static str {
    z_error(err)
}

/// Maps a C-ABI type size (in bytes) to the two-bit code the C
/// `zlibCompileFlags` bitfield uses to describe it: `2 -> 0`, `4 -> 1`,
/// `8 -> 2`, and any other width `-> 3`.
///
/// This mirrors the four `switch ((int)(sizeof(...)))` blocks in the C
/// `zlibCompileFlags` implementation, whose `case 2/4/8` arms add `0/1/2` and
/// whose `default` arm adds `3`.
#[inline]
fn size_code(n: usize) -> u32 {
    match n {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// Returns a bitfield describing the compile-time configuration of the
/// library, mirroring the C `zlibCompileFlags` function.
///
/// The concrete numeric value is a faithful *reconstruction* for the Rust
/// build rather than a fixed magic number: the **bit layout matches the C
/// `zlibCompileFlags` layout exactly**, and the type-size bits are computed
/// from the platform's actual C-ABI type widths via [`core::mem::size_of`].
/// All defined bits fit in a `u32` (the highest documented bit is 27); the FFI
/// layer widens the result to `c_ulong`.
///
/// # Layout
///
/// | Bits  | Meaning                                              |
/// |-------|------------------------------------------------------|
/// | 0-1   | `sizeof(uInt)`   -> [`c_uint`] width                  |
/// | 2-3   | `sizeof(uLong)`  -> [`c_ulong`] width                 |
/// | 4-5   | `sizeof(voidpf)` -> data-pointer width                |
/// | 6-7   | `sizeof(z_off_t)` -> [`c_long`] width                 |
/// | 8     | `ZLIB_DEBUG` (mirrors `debug_assertions`)            |
/// | 16    | `NO_GZCOMPRESS` (set when the `gz-io` feature is off) |
/// | 17    | `NO_GZIP` (set when the `gzip` feature is off)        |
/// | 27    | `gzprintf()` returns an error (always set in this build)   |
///
/// Bit 27 mirrors C `zutil.c`'s `flags += 1L << 27`, which C sets in the
/// `NO_vsnprintf && !ZLIB_INSECURE` case — a build whose `gzprintf`/`gzvprintf`
/// exist as symbols but return `Z_STREAM_ERROR` because no secure `*printf` was
/// available (`zlib.h`: bit 27 "1 means gzprintf() returns an error"). Rendering
/// a C `va_list` from Rust requires the unstable (nightly-only) `c_variadic`
/// feature, so this crate always ships exactly that error-returning stub for
/// `gzprintf`/`gzvprintf` and therefore always sets this bit.
///
/// Every other bit the C function can set (9-15, 18-26, and 28-31) is `0` in
/// this build: there is no assembler variant, no Windows API, no runtime-built
/// fixed or CRC tables, no PKZIP bug workaround, and no `FASTEST` mode.
#[inline]
#[must_use]
pub fn zlib_compile_flags() -> u32 {
    let mut flags: u32 = 0;

    // Type-size bits (0-7): computed from the real C-ABI widths of the Rust
    // `core::ffi` types standing in for zlib's `uInt`, `uLong`, `voidpf`, and
    // `z_off_t`, mirroring the four `switch (sizeof(...))` blocks in C.
    flags |= size_code(core::mem::size_of::<c_uint>());
    flags |= size_code(core::mem::size_of::<c_ulong>()) << 2;
    flags |= size_code(core::mem::size_of::<*const c_void>()) << 4;
    flags |= size_code(core::mem::size_of::<c_long>()) << 6;

    // bit 8: ZLIB_DEBUG. Mirror the Rust `debug_assertions` build profile so
    // the flag reflects whether debug checks are compiled in.
    if cfg!(debug_assertions) {
        flags |= 1 << 8;
    }

    // bit 16: NO_GZCOMPRESS. Set when the gz file-I/O layer is not built,
    // i.e. when the `gz-io` cargo feature is disabled.
    #[cfg(not(feature = "gz-io"))]
    {
        flags |= 1 << 16;
    }

    // bit 17: NO_GZIP. Set when gzip framing is not built, i.e. when the
    // `gzip` cargo feature is disabled.
    #[cfg(not(feature = "gzip"))]
    {
        flags |= 1 << 17;
    }

    // bit 27: gzprintf() returns an error. C sets `1L << 27` for a build with
    // no secure `vsnprintf`/`snprintf` (`NO_vsnprintf && !ZLIB_INSECURE`), i.e.
    // one whose `gzprintf`/`gzvprintf` are present as symbols but return
    // `Z_STREAM_ERROR`. A functional `gzprintf` in this crate would require
    // Rust's unstable (nightly-only) C-variadic support, so this crate always
    // ships the documented error-returning stubs and sets this bit to match C.
    flags |= 1 << 27;

    // Bits 9-15, 18-26, and 28-31 are unconditionally 0 in this build (see the
    // function's doc comment for the rationale).
    flags
}

/// C-compatible camelCase alias for [`zlib_compile_flags`].
///
/// Provided so `src/lib.rs` can re-export the historical `zlibCompileFlags`
/// name from `zlib.h`. It forwards verbatim to [`zlib_compile_flags`].
#[allow(non_snake_case)]
#[inline]
#[must_use]
pub fn zlibCompileFlags() -> u32 {
    zlib_compile_flags()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_exact_and_matches_crate_constant() {
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version(), crate::ZLIB_VERSION);
    }

    #[test]
    fn z_error_matches_c_err_msg_table() {
        // The nine defined codes in -6..=2, byte-identical to the C `z_errmsg`
        // table addressed via the `ERR_MSG` macro.
        assert_eq!(z_error(2), "need dictionary"); // Z_NEED_DICT
        assert_eq!(z_error(1), "stream end"); // Z_STREAM_END
        assert_eq!(z_error(0), ""); // Z_OK
        assert_eq!(z_error(-1), "file error"); // Z_ERRNO
        assert_eq!(z_error(-2), "stream error"); // Z_STREAM_ERROR
        assert_eq!(z_error(-3), "data error"); // Z_DATA_ERROR
        assert_eq!(z_error(-4), "insufficient memory"); // Z_MEM_ERROR
        assert_eq!(z_error(-5), "buffer error"); // Z_BUF_ERROR
        assert_eq!(z_error(-6), "incompatible version"); // Z_VERSION_ERROR
    }

    #[test]
    fn z_error_out_of_range_is_empty() {
        // Matches the C `ERR_MSG` out-of-range index (`z_errmsg[9] == ""`).
        assert_eq!(z_error(3), "");
        assert_eq!(z_error(-7), "");
        assert_eq!(z_error(100), "");
        assert_eq!(z_error(i32::MIN), "");
        assert_eq!(z_error(i32::MAX), "");
    }

    #[test]
    fn z_error_bridges_return_code_message() {
        // Cross-check every in-range code against the `ReturnCode` source of
        // truth to prove this module bridges rather than duplicates.
        for err in -6..=2 {
            let expected = ReturnCode::from_c_int(err)
                .map(|rc| rc.message())
                .unwrap_or("");
            assert_eq!(z_error(err), expected);
        }
    }

    #[test]
    fn compile_flags_type_size_bits_are_consistent() {
        // Cross-platform: assert the type-size bits agree with `size_of` for
        // the current target rather than hard-coding one platform's value.
        let flags = zlib_compile_flags();
        assert_eq!(flags & 0b11, size_code(core::mem::size_of::<c_uint>()));
        assert_eq!(
            (flags >> 2) & 0b11,
            size_code(core::mem::size_of::<c_ulong>())
        );
        assert_eq!(
            (flags >> 4) & 0b11,
            size_code(core::mem::size_of::<*const c_void>())
        );
        assert_eq!(
            (flags >> 6) & 0b11,
            size_code(core::mem::size_of::<c_long>())
        );
    }

    #[test]
    fn compile_flags_option_bits_match_build() {
        let flags = zlib_compile_flags();
        // bit 8: ZLIB_DEBUG mirrors `debug_assertions`.
        assert_eq!((flags >> 8) & 1, u32::from(cfg!(debug_assertions)));
        // bit 16: NO_GZCOMPRESS is set iff the `gz-io` feature is disabled.
        assert_eq!((flags >> 16) & 1, u32::from(!cfg!(feature = "gz-io")));
        // bit 17: NO_GZIP is set iff the `gzip` feature is disabled.
        assert_eq!((flags >> 17) & 1, u32::from(!cfg!(feature = "gzip")));
        // bit 27: gzprintf-returns-error is always set — the crate ships the
        // error-returning `gzprintf`/`gzvprintf` stubs (C-variadics are nightly
        // only), matching a zlib built without a secure `*printf`.
        assert_eq!((flags >> 27) & 1, 1);
    }

    #[test]
    fn compile_flags_reserved_bits_are_zero() {
        // Every bit the C function can set but this build never does: bits
        // 9-15 (0xFE00) and 18-31 EXCEPT bit 27 (gzprintf status, checked in
        // `compile_flags_option_bits_match_build`). 0xFFFC_0000 with bit 27
        // (0x0800_0000) cleared is 0xF7FC_0000.
        const RESERVED: u32 = 0xF7FC_FE00;
        assert_eq!(zlib_compile_flags() & RESERVED, 0);
    }

    #[test]
    fn compile_flags_low_byte_on_lp64() {
        // On an LP64 target (c_uint=4, c_ulong=8, pointer=8, c_long=8) the low
        // byte is 0xA9 — exactly as the C `zlibCompileFlags` produces:
        // uInt->1, uLong->2<<2=8, ptr->2<<4=32, z_off_t->2<<6=128 => 169.
        let lp64 = core::mem::size_of::<c_uint>() == 4
            && core::mem::size_of::<c_ulong>() == 8
            && core::mem::size_of::<*const c_void>() == 8
            && core::mem::size_of::<c_long>() == 8;
        if lp64 {
            assert_eq!(zlib_compile_flags() & 0xFF, 0xA9);
        }
    }

    #[test]
    fn size_code_maps_widths() {
        assert_eq!(size_code(2), 0);
        assert_eq!(size_code(4), 1);
        assert_eq!(size_code(8), 2);
        // Any other width falls through to the C `default` arm.
        assert_eq!(size_code(1), 3);
        assert_eq!(size_code(16), 3);
        assert_eq!(size_code(0), 3);
    }

    #[test]
    fn camelcase_aliases_delegate() {
        assert_eq!(zlibVersion(), zlib_version());
        assert_eq!(zlibCompileFlags(), zlib_compile_flags());
        for err in [-7, -6, -1, 0, 1, 2, 3, 100] {
            assert_eq!(zError(err), z_error(err));
        }
    }
}
