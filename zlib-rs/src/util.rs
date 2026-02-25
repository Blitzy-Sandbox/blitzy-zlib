//! Internal utility functions, error messages, and helper routines.
//!
//! This module ports the functionality of `zutil.c` (312 lines) and `zutil.h`
//! (331 lines) from the original C zlib library into idiomatic Rust. It provides:
//!
//! - Error message constants matching the C `z_errmsg[]` array
//! - Version and compile-flags query functions
//! - Copyright strings used by deflate and inflate modules
//! - Platform-specific OS code constants for gzip headers
//!
//! The C memory management helpers (`ZALLOC`, `ZFREE`, `zcalloc`, `zcfree`) are
//! not ported — Rust's ownership model and `Vec`/`Box` types replace all manual
//! memory management. The C diagnostic macros (`Assert`, `Trace*`) are not ported
//! — Rust's `debug_assert!` and standard logging crates provide equivalent
//! functionality natively.

use std::mem;

use cfg_if::cfg_if;

use crate::constants;
use crate::error::ReturnCode;

// =============================================================================
// Error Message Array
// Ported from zutil.c lines 13–24
// =============================================================================

/// Error messages indexed by `2 - error_code` for error codes in the range
/// `[-6, 2]`.
///
/// This array is a direct port of the C `z_errmsg[10]` array from `zutil.c`
/// lines 13–24. The indexing scheme maps the integer return code to a message:
/// `Z_ERRMSG[(2 - err) as usize]`. Entry 9 is an empty sentinel used for
/// out-of-range codes.
///
/// The messages are character-for-character identical to the C originals.
pub(crate) const Z_ERRMSG: [&str; 10] = [
    "need dictionary",      // Z_NEED_DICT       2  → index 0
    "stream end",           // Z_STREAM_END      1  → index 1
    "",                     // Z_OK              0  → index 2
    "file error",           // Z_ERRNO         (-1) → index 3
    "stream error",         // Z_STREAM_ERROR  (-2) → index 4
    "data error",           // Z_DATA_ERROR    (-3) → index 5
    "insufficient memory",  // Z_MEM_ERROR     (-4) → index 6
    "buffer error",         // Z_BUF_ERROR     (-5) → index 7
    "incompatible version", // Z_VERSION_ERROR (-6) → index 8
    "",                     // sentinel              → index 9
];

// Compile-time validation: ensure the [`ReturnCode`] enum discriminants align
// with the [`Z_ERRMSG`] array indexing scheme (index = 2 - error_code).
const _: () = {
    assert!(Z_ERRMSG.len() == 10);
    assert!(ReturnCode::NeedDict as i32 == 2);
    assert!(ReturnCode::StreamEnd as i32 == 1);
    assert!(ReturnCode::Ok as i32 == 0);
    assert!(ReturnCode::Errno as i32 == -1);
    assert!(ReturnCode::StreamError as i32 == -2);
    assert!(ReturnCode::DataError as i32 == -3);
    assert!(ReturnCode::MemError as i32 == -4);
    assert!(ReturnCode::BufError as i32 == -5);
    assert!(ReturnCode::VersionError as i32 == -6);
};

// =============================================================================
// Error Message Function
// Ported from zutil.h line 65: ERR_MSG macro
// =============================================================================

/// Returns the error message string for the given integer error code.
///
/// Maps the integer error code (as used by the C API) to the corresponding
/// human-readable message from the [`Z_ERRMSG`] array. The error code must be
/// in the range `[-6, 2]` (corresponding to [`ReturnCode`] discriminants);
/// out-of-range values return an empty string.
///
/// This is a direct port of the C `ERR_MSG(err)` macro defined in `zutil.h`
/// line 65:
/// ```c
/// #define ERR_MSG(err) z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
#[must_use]
pub(crate) fn err_msg(err: i32) -> &'static str {
    if (-6..=2).contains(&err) {
        // The value (2 - err) is guaranteed to be in [0, 8] since err is in
        // [-6, 2]. The cast to usize is therefore safe with no sign loss.
        #[allow(clippy::cast_sign_loss)]
        let index = (2 - err) as usize;
        Z_ERRMSG[index]
    } else {
        Z_ERRMSG[9]
    }
}

// =============================================================================
// Version Function
// Ported from zutil.c lines 27–29: zlibVersion()
// =============================================================================

/// Returns the zlib library version string.
///
/// This is the Rust equivalent of the C `zlibVersion()` function from `zutil.c`
/// line 27. It returns the [`ZLIB_VERSION`](crate::constants::ZLIB_VERSION)
/// constant string identifying the library version.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::zlib_version;
///
/// assert!(zlib_version().starts_with("1.3.2"));
/// ```
#[must_use]
pub fn zlib_version() -> &'static str {
    constants::ZLIB_VERSION
}

// =============================================================================
// Compile Flags Function
// Ported from zutil.c lines 31–121: zlibCompileFlags()
// =============================================================================

/// Encodes a type size in bytes as a 2-bit flag value for
/// [`zlib_compile_flags`].
///
/// Maps the byte size of a type to the encoding used by the compile flags:
/// - 2 bytes → `0b00` (0)
/// - 4 bytes → `0b01` (1)
/// - 8 bytes → `0b10` (2)
/// - Other   → `0b11` (3)
const fn size_to_flag(size: usize) -> u64 {
    match size {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

/// Returns a bit field encoding the compile-time configuration of the library.
///
/// This is the Rust equivalent of the C `zlibCompileFlags()` function from
/// `zutil.c` lines 31–121. The returned value encodes type sizes and feature
/// flags as individual bits:
///
/// | Bits  | Meaning |
/// |-------|---------|
/// | 0–1   | Size of `u32` (C `uInt` equivalent): 0=2B, 1=4B, 2=8B, 3=other |
/// | 2–3   | Size of `u64` (C `uLong` equivalent) |
/// | 4–5   | Size of `*const ()` (C `voidpf` equivalent) |
/// | 6–7   | Size of `i64` (C `z_off_t` equivalent) |
/// | 8     | Debug assertions enabled |
/// | 9     | Assembly optimizations (always 0 in Rust) |
/// | 10    | Windows API calling convention (always 0 in Rust) |
/// | 12    | `BUILDFIXED` — fixed tables built at runtime (always 0; const in Rust) |
/// | 13    | `DYNAMIC_CRC_TABLE` — CRC computed at runtime (always 0; const in Rust) |
/// | 16    | `NO_GZCOMPRESS` — gzip compression disabled (`gz-io` feature off) |
/// | 17    | `NO_GZIP` — gzip support disabled (`gzip` feature off) |
/// | 20    | `PKZIP_BUG_WORKAROUND` (always 0 in Rust) |
/// | 21    | `FASTEST` mode (always 0 in Rust) |
/// | 24–27 | `vsnprintf` availability (always 0; Rust has safe formatting) |
///
/// # Examples
///
/// ```
/// use zlib_rs::util::zlib_compile_flags;
///
/// let flags = zlib_compile_flags();
/// // u32 is always 4 bytes in Rust → bits 0–1 = 1
/// assert_eq!(flags & 0x03, 1);
/// ```
#[must_use]
pub fn zlib_compile_flags() -> u64 {
    let mut flags: u64 = 0;

    // Bits 0–1: size of u32 (C uInt equivalent, always 4 bytes in Rust).
    flags |= size_to_flag(mem::size_of::<u32>());

    // Bits 2–3: size of u64 (C uLong equivalent, always 8 bytes in Rust).
    flags |= size_to_flag(mem::size_of::<u64>()) << 2;

    // Bits 4–5: size of pointer (C voidpf equivalent, 4 or 8 bytes).
    flags |= size_to_flag(mem::size_of::<*const ()>()) << 4;

    // Bits 6–7: size of i64 (C z_off_t equivalent, always 8 bytes in Rust).
    flags |= size_to_flag(mem::size_of::<i64>()) << 6;

    // Bit 8: debug mode (equivalent to C ZLIB_DEBUG).
    if cfg!(debug_assertions) {
        flags |= 1 << 8;
    }

    // Bit 9: assembly optimizations — not applicable in Rust (LLVM manages).
    // Bit 10: ZLIB_WINAPI — not applicable in Rust.

    // Bit 12: BUILDFIXED — false in Rust (Huffman tables are compile-time const).
    // Bit 13: DYNAMIC_CRC_TABLE — false in Rust (CRC tables are compile-time const).

    // Bit 16: NO_GZCOMPRESS — set when the gz-io feature is disabled.
    if !cfg!(feature = "gz-io") {
        flags |= 1 << 16;
    }

    // Bit 17: NO_GZIP — set when the gzip feature is disabled.
    if !cfg!(feature = "gzip") {
        flags |= 1 << 17;
    }

    // Bit 20: PKZIP_BUG_WORKAROUND — not enabled in Rust.
    // Bit 21: FASTEST — not applicable (level is runtime configuration).
    // Bits 24–27: vsnprintf availability — Rust has safe formatting (all clear).

    flags
}

// =============================================================================
// Copyright Strings
// Declared in zutil.h lines 39–40; defined in trees.c and inflate.c
// =============================================================================

/// Copyright string embedded in the deflate compression engine.
///
/// This string is embedded in compiled output for attribution and is used by
/// the deflate module. It matches the C `deflate_copyright[]` array declared
/// in `zutil.h` line 39 and defined in `trees.c`.
pub(crate) const DEFLATE_COPYRIGHT: &str =
    " deflate 1.3.2.1 Copyright 1995-2026 Jean-loup Gailly and Mark Adler ";

/// Copyright string embedded in the inflate decompression engine.
///
/// This string is embedded in compiled output for attribution and is used by
/// the inflate module. It matches the C `inflate_copyright[]` array declared
/// in `zutil.h` line 40 and defined in `inflate.c`.
pub(crate) const INFLATE_COPYRIGHT: &str =
    " inflate 1.3.2.1 Copyright 1995-2026 Mark Adler ";

// =============================================================================
// OS Code Constant
// Ported from zutil.h lines 100–189: platform-specific OS code detection
// =============================================================================

cfg_if! {
    if #[cfg(target_os = "windows")] {
        /// OS code for the current platform, used in gzip headers (RFC 1952).
        ///
        /// Windows is identified as OS code 10. Corresponds to the
        /// `WIN32 && !__CYGWIN__` case in C `zutil.h` line 157.
        pub(crate) const OS_CODE: u8 = 10;
    } else if #[cfg(target_vendor = "apple")] {
        /// OS code for the current platform, used in gzip headers (RFC 1952).
        ///
        /// Apple platforms (macOS, iOS, tvOS, watchOS) are identified as OS
        /// code 19. Corresponds to the `__APPLE__` case in C `zutil.h` line 169.
        pub(crate) const OS_CODE: u8 = 19;
    } else if #[cfg(target_os = "haiku")] {
        /// OS code for the current platform, used in gzip headers (RFC 1952).
        ///
        /// Haiku (successor to BeOS) is identified as OS code 16. Corresponds
        /// to the `_BEOS_` case in C `zutil.h` line 161.
        pub(crate) const OS_CODE: u8 = 16;
    } else {
        /// OS code for the current platform, used in gzip headers (RFC 1952).
        ///
        /// Unix and all other platforms default to OS code 3. Corresponds to
        /// the default case in C `zutil.h` line 188.
        pub(crate) const OS_CODE: u8 = 3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Z_ERRMSG tests
    // =========================================================================

    #[test]
    fn z_errmsg_has_correct_length() {
        assert_eq!(Z_ERRMSG.len(), 10);
    }

    #[test]
    fn z_errmsg_entries_match_c_originals() {
        assert_eq!(Z_ERRMSG[0], "need dictionary");
        assert_eq!(Z_ERRMSG[1], "stream end");
        assert_eq!(Z_ERRMSG[2], "");
        assert_eq!(Z_ERRMSG[3], "file error");
        assert_eq!(Z_ERRMSG[4], "stream error");
        assert_eq!(Z_ERRMSG[5], "data error");
        assert_eq!(Z_ERRMSG[6], "insufficient memory");
        assert_eq!(Z_ERRMSG[7], "buffer error");
        assert_eq!(Z_ERRMSG[8], "incompatible version");
        assert_eq!(Z_ERRMSG[9], "");
    }

    // =========================================================================
    // err_msg tests
    // =========================================================================

    #[test]
    fn err_msg_maps_z_ok() {
        assert_eq!(err_msg(0), "");
    }

    #[test]
    fn err_msg_maps_z_stream_end() {
        assert_eq!(err_msg(1), "stream end");
    }

    #[test]
    fn err_msg_maps_z_need_dict() {
        assert_eq!(err_msg(2), "need dictionary");
    }

    #[test]
    fn err_msg_maps_z_errno() {
        assert_eq!(err_msg(-1), "file error");
    }

    #[test]
    fn err_msg_maps_z_stream_error() {
        assert_eq!(err_msg(-2), "stream error");
    }

    #[test]
    fn err_msg_maps_z_data_error() {
        assert_eq!(err_msg(-3), "data error");
    }

    #[test]
    fn err_msg_maps_z_mem_error() {
        assert_eq!(err_msg(-4), "insufficient memory");
    }

    #[test]
    fn err_msg_maps_z_buf_error() {
        assert_eq!(err_msg(-5), "buffer error");
    }

    #[test]
    fn err_msg_maps_z_version_error() {
        assert_eq!(err_msg(-6), "incompatible version");
    }

    #[test]
    fn err_msg_returns_empty_for_out_of_range_positive() {
        assert_eq!(err_msg(3), "");
        assert_eq!(err_msg(100), "");
        assert_eq!(err_msg(i32::MAX), "");
    }

    #[test]
    fn err_msg_returns_empty_for_out_of_range_negative() {
        assert_eq!(err_msg(-7), "");
        assert_eq!(err_msg(-100), "");
        assert_eq!(err_msg(i32::MIN), "");
    }

    // =========================================================================
    // Copyright string tests
    // =========================================================================

    #[test]
    fn deflate_copyright_contains_version() {
        assert!(DEFLATE_COPYRIGHT.contains("1.3.2.1"));
    }

    #[test]
    fn deflate_copyright_contains_authors() {
        assert!(DEFLATE_COPYRIGHT.contains("Jean-loup Gailly"));
        assert!(DEFLATE_COPYRIGHT.contains("Mark Adler"));
    }

    #[test]
    fn inflate_copyright_contains_version() {
        assert!(INFLATE_COPYRIGHT.contains("1.3.2.1"));
    }

    #[test]
    fn inflate_copyright_contains_author() {
        assert!(INFLATE_COPYRIGHT.contains("Mark Adler"));
    }

    // =========================================================================
    // OS_CODE tests
    // =========================================================================

    #[test]
    fn os_code_is_valid_gzip_os_value() {
        // Valid OS codes per RFC 1952 section 2.3.1: 0–13, 255 (unknown)
        // Extended codes used by zlib: 16 (BeOS), 18 (OS/400), 19 (Apple)
        let valid_codes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 13, 16, 18, 19];
        assert!(
            valid_codes.contains(&OS_CODE),
            "OS_CODE {OS_CODE} is not a recognized gzip OS code"
        );
    }

    #[test]
    fn os_code_matches_current_platform() {
        if cfg!(target_os = "linux") {
            assert_eq!(OS_CODE, 3, "Linux should have Unix OS code 3");
        }
    }
}
