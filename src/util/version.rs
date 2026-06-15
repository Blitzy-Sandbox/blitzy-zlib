//! zlib version and compile-time diagnostics — a safe-Rust port of the
//! version/diagnostic surface of C `zutil.c` (zlib 1.3.2.1-motley).
//!
//! This is the foundational module of [`crate::util`]: it has **no
//! intra-folder dependencies** and provides the three externally observable
//! "diagnostic" entry points of the C library:
//!
//! | This module            | C origin (`zutil.c`)                      |
//! |------------------------|-------------------------------------------|
//! | [`zlib_version`]       | `const char *zlibVersion(void)` (L27-29)  |
//! | [`zlib_compile_flags`] | `uLong zlibCompileFlags(void)` (L31-121)  |
//! | [`z_error`]            | `const char *zError(int)` (L139-141)      |
//!
//! [`zlib_version_num`] additionally exposes the numeric `ZLIB_VERNUM`
//! companion of the version string for the FFI layer.
//!
//! # Purity and portability
//!
//! This module is **`no_std`-clean** and **100% safe**: it uses `core` only
//! (no `std`, no `alloc`) and contains **no `unsafe`**. The C-ABI integer
//! widths are obtained through [`core::ffi`], so the reported
//! [`zlib_compile_flags`] match the target's real `unsigned int` /
//! `unsigned long` / pointer sizes exactly, as the C original does at compile
//! time.
//!
//! # Delegation of error strings
//!
//! [`z_error`] does **not** carry its own copy of the message table; it
//! delegates to [`crate::error::err_msg`], the single canonical reproduction
//! of the C `z_errmsg[10]` table and the `ERR_MSG` indexing macro. No
//! message literal appears in this file — the table is single-sourced in
//! [`crate::error`].
//!
//! # C constructs intentionally not ported
//!
//! Several `zutil.c` artifacts have no place in the safe-Rust crate and are
//! deliberately omitted from this module:
//!
//! * `zcalloc` / `zcfree` (the `malloc`/`calloc`/`free` shims) — superseded by
//!   Rust ownership and the `Allocator` trait used by `crate::stream`.
//! * `zmemcpy` / `zmemcmp` / `zmemzero` — superseded by slice operations
//!   (`copy_from_slice`, `==`, `fill`).
//! * `z_verbose` and the `ZLIB_DEBUG`-only `z_error(char *)` fatal logger — a
//!   debug-build artifact with no safe-Rust analogue.
//! * the message table itself — it lives canonically in [`crate::error`]; this
//!   module only delegates to it.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::util::version::{zlib_version, zlib_version_num};
//!
//! assert_eq!(zlib_version(), "1.3.2.1-motley");
//! assert_eq!(zlib_version_num(), 0x1321);
//! ```

use crate::constants::{ZLIB_VERNUM, ZLIB_VERSION};
use crate::error::err_msg;
use core::ffi::{c_uint, c_ulong, c_void};
use core::mem::size_of;

/// Encodes a type's byte size into the 2-bit field used by
/// [`zlib_compile_flags`].
///
/// This mirrors the per-`switch` arithmetic of C `zlibCompileFlags`, where the
/// size of each C-ABI type is folded into two bits: `2 -> 0`, `4 -> 1`,
/// `8 -> 2`, and anything else `-> 3`. The caller shifts the returned code into
/// the appropriate field position.
const fn size_code(size: usize) -> c_ulong {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// Returns the zlib version string this crate is API/format compatible with.
///
/// Mirrors C `const char *zlibVersion(void)` (`zutil.c` L27-29), which returns
/// the `ZLIB_VERSION` macro. The value is the Rust [`str`] form of that macro;
/// the FFI layer is responsible for materializing the NUL-terminated C string.
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

/// Returns the numeric zlib version (`ZLIB_VERNUM`), packed as `0xMMNRS0`.
///
/// This is the numeric companion of [`zlib_version`] (C `ZLIB_VERNUM`), used by
/// the FFI layer and by version-compatibility checks. For zlib
/// 1.3.2.1-motley the value is `0x1321`.
///
/// # Examples
///
/// ```
/// assert_eq!(zlib_rs::util::version::zlib_version_num(), 0x1321);
/// ```
#[must_use]
pub fn zlib_version_num() -> i32 {
    ZLIB_VERNUM
}

/// Maps a zlib integer return code to its human-readable message.
///
/// Mirrors C `const char *zError(int err)` (`zutil.c` L139-141), which expands
/// the `ERR_MSG` macro. This function simply delegates to
/// [`crate::error::err_msg`] — the single canonical reproduction of the C
/// message table — so the mapping can never drift from the rest of the crate.
/// `Z_OK` and any out-of-range code yield the empty string `""`.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::z_error;
///
/// assert_eq!(z_error(0), ""); // Z_OK has no message
/// assert!(!z_error(-3).is_empty()); // Z_DATA_ERROR has a message
/// assert_eq!(z_error(-100), ""); // out-of-range -> empty
/// ```
#[must_use]
pub fn z_error(err: i32) -> &'static str {
    err_msg(err)
}

/// Returns a bit-encoded summary of the compile-time configuration, matching
/// C `uLong zlibCompileFlags(void)` (`zutil.c` L31-121).
///
/// The low byte encodes the sizes of four C-ABI types as 2-bit fields (see
/// [`size_code`]):
///
/// | Bits    | C type    | Rust type modeled      |
/// |---------|-----------|------------------------|
/// | `[1:0]` | `uInt`    | [`c_uint`]             |
/// | `[3:2]` | `uLong`   | [`c_ulong`]            |
/// | `[5:4]` | `voidpf`  | `*const` [`c_void`]    |
/// | `[7:6]` | `z_off_t` | [`i64`] (8-byte `off_t`) |
///
/// The remaining option bits (`ZLIB_DEBUG`, `ZLIB_WINAPI`, `BUILDFIXED`,
/// `DYNAMIC_CRC_TABLE`, `PKZIP_BUG_WORKAROUND`, `FASTEST`, and the
/// `snprintf`/`vsnprintf` feature bits) are all `0` for this crate's modern,
/// safe build: there is no debug logger, the CRC tables are generated
/// statically by `build.rs` (so `DYNAMIC_CRC_TABLE` is never set), and a
/// conformant `vsnprintf` is assumed. The only option bits this crate can set
/// are the two gzip-suppression bits, driven by Cargo features:
///
/// * bit 16 `NO_GZCOMPRESS` — set only when the `gz-io` feature is **off**.
/// * bit 17 `NO_GZIP` — set only when the `gzip` feature is **off**.
///
/// On a typical 64-bit Unix (LP64) target with the default features enabled,
/// the result is `0xA9` (`1 | (2 << 2) | (2 << 4) | (2 << 6)`).
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::zlib_compile_flags;
///
/// let flags = zlib_compile_flags();
/// // This crate uses build-time static CRC tables, so the DYNAMIC_CRC_TABLE
/// // bit (13) is always clear.
/// assert_eq!(flags & (1 << 13), 0);
/// ```
#[must_use]
pub fn zlib_compile_flags() -> c_ulong {
    let mut flags: c_ulong = 0;

    // Type-size fields (low byte), computed from the C-ABI types used at the
    // FFI boundary. `z_off_t` is `off_t` with large-file support (8 bytes),
    // modeled as `i64` to match the crate's 64-bit `*_combine64` offsets.
    flags |= size_code(size_of::<c_uint>()); // uInt -> [1:0]
    flags |= size_code(size_of::<c_ulong>()) << 2; // uLong -> [3:2]
    flags |= size_code(size_of::<*const c_void>()) << 4; // voidpf -> [5:4]
    flags |= size_code(size_of::<i64>()) << 6; // z_off_t -> [7:6]

    // Option bits. Every C `#ifdef`-gated bit other than the two gzip bits
    // below is unconditionally 0 in this crate's modern, safe build:
    //   bit 8  ZLIB_DEBUG           = 0
    //   bit 10 ZLIB_WINAPI          = 0
    //   bit 12 BUILDFIXED           = 0
    //   bit 13 DYNAMIC_CRC_TABLE    = 0  (build.rs emits static tables)
    //   bit 20 PKZIP_BUG_WORKAROUND = 0
    //   bit 21 FASTEST              = 0
    //   bits 24-27 snprintf flags   = 0  (conformant vsnprintf assumed)
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
        assert_eq!(zlib_version(), "1.3.2.1-motley");
    }

    #[test]
    fn version_num_is_exact() {
        assert_eq!(zlib_version_num(), 0x1321);
    }

    #[test]
    fn z_error_delegates_to_err_msg() {
        // Assert exact delegation to the canonical table for every in-range
        // code (and just beyond both ends) without embedding any message
        // literal in this file. This both proves the delegation and keeps the
        // message table single-sourced in `crate::error`.
        for code in -7..=3 {
            assert_eq!(z_error(code), err_msg(code));
        }
    }

    #[test]
    fn z_error_boundaries_are_empty() {
        // Z_OK and out-of-range codes carry no message.
        assert_eq!(z_error(0), ""); // Z_OK
        assert_eq!(z_error(3), ""); // above Z_NEED_DICT -> out of range
        assert_eq!(z_error(-7), ""); // below Z_VERSION_ERROR -> out of range
    }

    #[test]
    fn z_error_in_range_codes_have_messages() {
        // Codes 2, 1 and -1..=-6 map to non-empty messages (0 is empty and is
        // covered by `z_error_boundaries_are_empty`).
        for code in [2, 1, -1, -2, -3, -4, -5, -6] {
            assert!(
                !z_error(code).is_empty(),
                "code {code} should have a non-empty message"
            );
        }
    }

    #[test]
    fn compile_flags_low_byte_encodes_type_sizes() {
        let flags = zlib_compile_flags();
        let expected_low = size_code(size_of::<c_uint>())
            | (size_code(size_of::<c_ulong>()) << 2)
            | (size_code(size_of::<*const c_void>()) << 4)
            | (size_code(size_of::<i64>()) << 6);
        // The low byte is exactly the four 2-bit type-size fields.
        assert_eq!(flags & 0xFF, expected_low);
        // The uInt field occupies bits [1:0].
        assert_eq!(flags & 0b11, size_code(size_of::<c_uint>()));
    }

    #[test]
    fn compile_flags_low_byte_is_0xa9_on_lp64() {
        let flags = zlib_compile_flags();
        // LP64 (64-bit Unix): uInt=4, uLong=8, pointer=8, z_off_t=8 => 0xA9.
        if size_of::<c_ulong>() == 8 && size_of::<*const c_void>() == 8 {
            assert_eq!(flags & 0xFF, 0xA9);
        }
    }

    #[test]
    fn compile_flags_dynamic_crc_table_bit_is_clear() {
        // bit 13 (DYNAMIC_CRC_TABLE) is never set: the tables are static.
        assert_eq!(zlib_compile_flags() & (1 << 13), 0);
    }

    #[cfg(feature = "gz-io")]
    #[test]
    fn compile_flags_no_gzcompress_bit_clear_with_gz_io() {
        // With `gz-io` enabled, NO_GZCOMPRESS (bit 16) is clear.
        assert_eq!(zlib_compile_flags() & (1 << 16), 0);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn compile_flags_no_gzip_bit_clear_with_gzip() {
        // With `gzip` enabled, NO_GZIP (bit 17) is clear.
        assert_eq!(zlib_compile_flags() & (1 << 17), 0);
    }
}
