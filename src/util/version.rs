//! zlib version reporting and compile-time / error-code diagnostics.
//!
//! This is the foundational module of the `util` layer: it has no intra-`util`
//! dependencies and is a safe-Rust port of the *version / diagnostics* portion
//! of the C translation unit `zutil.c` (zlib `1.3.2.1-motley`). It reproduces
//! the three public introspection entry points of the C API —
//!
//! * [`zlib_version`]       ← C `zlibVersion()`        (`zutil.c` L27–29)
//! * [`z_error`]            ← C `zError()`             (`zutil.c` L139–141)
//! * [`zlib_compile_flags`] ← C `zlibCompileFlags()`   (`zutil.c` L31–121)
//!
//! plus the convenience [`zlib_version_num`] — the packed numeric version that
//! the FFI layer reports alongside the string.
//!
//! # Relationship to the C source
//!
//! Only the *version / diagnostics* functions of `zutil.c` belong here. The
//! remainder of that translation unit has no place in safe Rust and is
//! intentionally **not** ported:
//!
//! * `zcalloc` / `zcfree` (the 16-bit and generic allocator shims, `zutil.c`
//!   L215+) — replaced by Rust ownership and the `Allocator` trait used by
//!   `crate::stream`.
//! * `zmemcpy` / `zmemcmp` / `zmemzero` — replaced by safe slice operations
//!   (`copy_from_slice`, `==`, `fill`).
//! * `z_verbose` / `z_error(char *)` (the `ZLIB_DEBUG`-only fatal logger,
//!   `zutil.c` L123–134) — a debug-build artifact with no production behavior.
//! * the 10-entry `z_errmsg` string table itself — it lives **canonically** in
//!   [`crate::error`]; [`z_error`] delegates to [`crate::error::err_msg`] so the
//!   error strings are defined in exactly one place and can never diverge from
//!   C `zlib`.
//!
//! # Safety & `no_std`
//!
//! The entire module is 100% safe Rust (no `unsafe`) and `no_std`-clean: it
//! imports only from [`core`] and from the crate's own `constants` / `error`
//! modules.

use crate::constants::{ZLIB_VERNUM, ZLIB_VERSION};
use crate::error::err_msg;
use core::ffi::{c_uint, c_ulong, c_void};
use core::mem::size_of;

/// Returns the zlib version string this crate is compatible with.
///
/// Mirrors the C `zlibVersion()` function, which returns the `ZLIB_VERSION`
/// macro verbatim. The value is the compile-time constant
/// [`crate::constants::ZLIB_VERSION`] and is always exactly `"1.3.2.1-motley"`.
///
/// # Examples
///
/// ```
/// assert_eq!(zlib_rs::util::version::zlib_version(), "1.3.2.1-motley");
/// ```
#[must_use]
pub fn zlib_version() -> &'static str {
    ZLIB_VERSION
}

/// Returns the packed numeric zlib version, matching the C `ZLIB_VERNUM` macro.
///
/// The nibbles encode `0xMMNNRRSS` (major / minor / revision / sub-revision);
/// for this baseline the value is `0x1321` (major 1, minor 3, revision 2,
/// sub-revision 1). The FFI layer reports this alongside [`zlib_version`].
#[must_use]
pub fn zlib_version_num() -> i32 {
    ZLIB_VERNUM
}

/// Maps a zlib integer return code to its human-readable message.
///
/// Mirrors the C `zError()` function, which is a thin wrapper over the
/// `ERR_MSG(err)` macro:
///
/// ```text
/// z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// In-range codes `-6..=2` map to their specific message; `Z_OK` (`0`) and
/// every out-of-range code map to the empty string. This function delegates
/// **directly** to the single canonical lookup [`crate::error::err_msg`]; the
/// error-string table is defined there and nowhere else, so the messages can
/// never diverge from C `zlib`.
#[must_use]
pub fn z_error(err: i32) -> &'static str {
    err_msg(err)
}

/// Encodes a type size into the 2-bit field used by [`zlib_compile_flags`].
///
/// Reproduces the per-`switch` mapping in C `zlibCompileFlags()`:
/// `2 -> 0`, `4 -> 1`, `8 -> 2`, and any other size `-> 3`.
const fn size_code(size: usize) -> c_ulong {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// Returns a bit-encoded summary of the compile-time configuration, matching
/// the C `zlibCompileFlags()` function (`zutil.c` L31–121).
///
/// # Layout
///
/// The low byte packs four 2-bit type-size fields, each produced by
/// `size_code` from the size of the corresponding C-ABI type:
///
/// | Bits  | C type     | Rust type used                |
/// |-------|------------|-------------------------------|
/// | `1:0` | `uInt`     | [`c_uint`]                    |
/// | `3:2` | `uLong`    | [`c_ulong`]                   |
/// | `5:4` | `voidpf`   | `*const` [`c_void`]           |
/// | `7:6` | `z_off_t`  | [`i64`] (`off64_t`, 8 bytes)  |
///
/// The remaining option bits (`ZLIB_DEBUG` bit 8, `ZLIB_WINAPI` bit 10,
/// `BUILDFIXED` bit 12, `DYNAMIC_CRC_TABLE` bit 13, `PKZIP_BUG_WORKAROUND`
/// bit 20, `FASTEST` bit 21, and the `snprintf`/`vsnprintf` feature bits
/// 24–27) are all `0` in this crate's modern, statically-tabled build. The two
/// gzip-related bits are feature-driven:
///
/// * bit 16 `NO_GZCOMPRESS` — set only when the `gz-io` feature is **off**.
/// * bit 17 `NO_GZIP` — set only when the `gzip` feature is **off**.
///
/// `DYNAMIC_CRC_TABLE` (bit 13) is always clear because the CRC-32 tables are
/// generated at build time by `build.rs` rather than lazily at runtime.
///
/// # Target dependence
///
/// On a typical 64-bit Unix (LP64) target with the default features the return
/// value's low byte is `0xA9` (`1 | (2 << 2) | (2 << 4) | (2 << 6)`).
#[must_use]
pub fn zlib_compile_flags() -> c_ulong {
    let mut flags: c_ulong = 0;

    // Type-size fields, computed from the C-ABI types used at the FFI boundary.
    flags |= size_code(size_of::<c_uint>()); // uInt   -> [1:0]
    flags |= size_code(size_of::<c_ulong>()) << 2; // uLong  -> [3:2]
    flags |= size_code(size_of::<*const c_void>()) << 4; // voidpf -> [5:4]
    flags |= size_code(size_of::<i64>()) << 6; // z_off_t (off64_t) -> [7:6]

    // Option bits. In this crate's default modern build, ALL are 0 except the
    // two feature-driven gzip bits below:
    //   bit 8  ZLIB_DEBUG            = 0
    //   bit 10 ZLIB_WINAPI           = 0
    //   bit 12 BUILDFIXED            = 0
    //   bit 13 DYNAMIC_CRC_TABLE     = 0  (build.rs generates STATIC tables)
    //   bit 20 PKZIP_BUG_WORKAROUND  = 0
    //   bit 21 FASTEST               = 0
    //   bits 24-27 snprintf flags    = 0  (modern STDC build, vsnprintf present)
    #[cfg(not(feature = "gz-io"))]
    {
        flags |= 1 << 16; // NO_GZCOMPRESS
    }
    #[cfg(not(feature = "gzip"))]
    {
        flags |= 1 << 17; // NO_GZIP
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_is_exact() {
        // Mirrors C `zlibVersion()`; must be byte-for-byte the baseline string.
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version(), ZLIB_VERSION);
    }

    #[test]
    fn version_num_is_packed_constant() {
        assert_eq!(zlib_version_num(), 0x1321);
        assert_eq!(zlib_version_num(), ZLIB_VERNUM);
    }

    #[test]
    fn z_error_is_total_delegation_to_err_msg() {
        // `z_error` must be a pure delegation to the canonical `err_msg` lookup
        // — the error-string table lives ONLY in `crate::error`, never here, so
        // this file contains no error-message literals. Verify across the full
        // in-range window (-6..=2) and well past both out-of-range boundaries;
        // the message text itself is asserted once, canonically, by the
        // `crate::error` test suite.
        for code in -20..=20 {
            assert_eq!(z_error(code), err_msg(code), "mismatch for code {code}");
        }
    }

    #[test]
    fn z_error_defined_codes_are_nonempty() {
        // Each defined status/error code in -6..=2, except `Z_OK` (0), maps to
        // a non-empty human-readable message; `Z_OK` maps to the empty string.
        for code in [2, 1, -1, -2, -3, -4, -5, -6] {
            assert!(
                !z_error(code).is_empty(),
                "code {code} should have a message"
            );
        }
        assert!(z_error(0).is_empty(), "Z_OK maps to the empty string");
    }

    #[test]
    fn z_error_out_of_range_is_empty() {
        // Codes `< -6` or `> 2` map to the empty sentinel (index 9 of the C
        // `z_errmsg` table); spot-check just inside and far outside the range.
        assert!(z_error(-7).is_empty());
        assert!(z_error(3).is_empty());
        assert!(z_error(-100).is_empty());
        assert!(z_error(100).is_empty());
    }

    #[test]
    fn size_code_matches_c_switch() {
        assert_eq!(size_code(2), 0);
        assert_eq!(size_code(4), 1);
        assert_eq!(size_code(8), 2);
        assert_eq!(size_code(1), 3);
        assert_eq!(size_code(16), 3);
    }

    #[test]
    fn compile_flags_type_size_fields_match_size_code() {
        // Portable across all targets: every 2-bit field equals `size_code`
        // of the corresponding C-ABI type.
        let flags = zlib_compile_flags();
        assert_eq!(flags & 0b11, size_code(size_of::<c_uint>()));
        assert_eq!((flags >> 2) & 0b11, size_code(size_of::<c_ulong>()));
        assert_eq!((flags >> 4) & 0b11, size_code(size_of::<*const c_void>()));
        assert_eq!((flags >> 6) & 0b11, size_code(size_of::<i64>()));
    }

    #[test]
    fn compile_flags_dynamic_crc_table_bit_is_clear() {
        // build.rs generates STATIC CRC-32 tables, so DYNAMIC_CRC_TABLE
        // (bit 13) is always 0.
        let flags = zlib_compile_flags();
        assert_eq!(flags & (1 << 13), 0);
    }

    // With the `gz-io` feature on (default), NO_GZCOMPRESS (bit 16) is clear.
    #[cfg(feature = "gz-io")]
    #[test]
    fn compile_flags_no_gzcompress_clear_with_gz_io() {
        assert_eq!(zlib_compile_flags() & (1 << 16), 0);
    }

    // With the `gzip` feature on (default), NO_GZIP (bit 17) is clear.
    #[cfg(feature = "gzip")]
    #[test]
    fn compile_flags_no_gzip_clear_with_gzip() {
        assert_eq!(zlib_compile_flags() & (1 << 17), 0);
    }

    // With `gz-io` off, NO_GZCOMPRESS (bit 16) must be set.
    #[cfg(not(feature = "gz-io"))]
    #[test]
    fn compile_flags_no_gzcompress_set_without_gz_io() {
        assert_eq!(zlib_compile_flags() & (1 << 16), 1 << 16);
    }

    // With `gzip` off, NO_GZIP (bit 17) must be set.
    #[cfg(not(feature = "gzip"))]
    #[test]
    fn compile_flags_no_gzip_set_without_gzip() {
        assert_eq!(zlib_compile_flags() & (1 << 17), 1 << 17);
    }

    // On a 64-bit Unix (LP64) target the low byte is exactly 0xA9.
    #[cfg(all(target_pointer_width = "64", not(windows)))]
    #[test]
    fn compile_flags_low_byte_is_0xa9_on_lp64() {
        let flags = zlib_compile_flags();
        assert_eq!(flags & 0xFF, 0xA9);
    }
}
