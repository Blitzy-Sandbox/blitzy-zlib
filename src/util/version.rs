//! Version, compile-time configuration, and error message utilities.
//!
//! This module provides the Rust equivalents of the version and metadata
//! functions from C zlib's `zutil.c`:
//!
//! - [`ZLIB_VERSION`] — compile-time version string constant
//! - [`zlib_version`] — returns the library version string at runtime
//! - [`compile_flags`] — returns a bitmask of compile-time configuration
//! - [`error_message`] — maps a zlib return code to its human-readable message
//!
//! # Compatibility
//!
//! The version string and error messages are identical to C zlib v1.3.2.1-motley.
//! The `compile_flags` bitmask has been adapted for Rust's type system and Cargo
//! feature flags while preserving the same bit-field layout as C zlib's
//! `zlibCompileFlags()`.
//!
//! # no_std Support
//!
//! All items in this module are available in `#![no_std]` environments.

use core::mem;

// ---------------------------------------------------------------------------
// Version constant
// ---------------------------------------------------------------------------

/// The zlib-rs version string, compatible with C zlib v1.3.2.1-motley.
///
/// This constant mirrors the `ZLIB_VERSION` macro defined in the original
/// `zlib.h` (line 44). Applications can compare this value against the result
/// of [`zlib_version`] to verify that the header and library versions match,
/// analogous to the C pattern of comparing `ZLIB_VERSION` with `zlibVersion()`.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::ZLIB_VERSION;
/// assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
/// ```
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

// ---------------------------------------------------------------------------
// zlib_version()
// ---------------------------------------------------------------------------

/// Returns the zlib-rs library version string.
///
/// This function returns the same version string that was compiled into the
/// library.  Applications can verify version compatibility by comparing this
/// with the header-defined [`ZLIB_VERSION`] constant.
///
/// Equivalent to C zlib's `zlibVersion()`.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::zlib_version;
/// assert_eq!(zlib_version(), "1.3.2.1-motley");
/// ```
#[inline]
pub fn zlib_version() -> &'static str {
    ZLIB_VERSION
}

// ---------------------------------------------------------------------------
// compile_flags()
// ---------------------------------------------------------------------------

/// Returns a bitmask reporting the compile-time configuration of this library.
///
/// The bitmask layout is adapted from C zlib's `zlibCompileFlags()` (defined in
/// `zutil.c` lines 31-121).  The type-size fields (bits 0-7) encode Rust's
/// fixed-width types, while the feature-flag bits reflect Cargo feature state.
///
/// ## Bit layout
///
/// | Bits  | Meaning                                                       |
/// |-------|---------------------------------------------------------------|
/// | 0-1   | `sizeof(u32)` encoding (always **1** = 4 bytes in Rust)       |
/// | 2-3   | `sizeof(u64)` encoding (always **2** = 8 bytes in Rust)       |
/// | 4-5   | `sizeof(usize)` encoding (1 = 32-bit, 2 = 64-bit)            |
/// | 6-7   | `sizeof(u64)` for offset type (always **2** = 8 bytes)        |
/// | 8     | Debug assertions enabled (`cfg(debug_assertions)`)            |
/// | 16    | `gz-io` feature **disabled** (no gzip compression I/O)        |
/// | 17    | `gzip` feature **disabled** (no gzip format support)          |
///
/// The type-size encoding scheme matches C zlib: 0 = 2 bytes, 1 = 4 bytes,
/// 2 = 8 bytes, 3 = other.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::compile_flags;
/// let flags = compile_flags();
/// // Bits 0-1 encode sizeof(u32) = 4 → value 1
/// assert_eq!(flags & 0x3, 1);
/// // Bits 2-3 encode sizeof(u64) = 8 → value 2
/// assert_eq!((flags >> 2) & 0x3, 2);
/// ```
pub fn compile_flags() -> u64 {
    let mut flags: u64 = 0;

    // Bits 0-1: sizeof equivalent of C uInt (u32 in Rust = 4 bytes → encoding 1)
    flags |= size_encoding(mem::size_of::<u32>());

    // Bits 2-3: sizeof equivalent of C uLong (u64 in Rust = 8 bytes → encoding 2)
    flags |= size_encoding(mem::size_of::<u64>()) << 2;

    // Bits 4-5: sizeof pointer / usize (platform-dependent)
    flags |= size_encoding(mem::size_of::<usize>()) << 4;

    // Bits 6-7: sizeof z_off_t equivalent (u64 in Rust = 8 bytes → encoding 2)
    flags |= size_encoding(mem::size_of::<u64>()) << 6;

    // Bit 8: debug mode (equivalent to C ZLIB_DEBUG)
    if cfg!(debug_assertions) {
        flags |= 1 << 8;
    }

    // Bit 16: NO_GZCOMPRESS equivalent — set when gz-io feature is disabled
    if cfg!(not(feature = "gz-io")) {
        flags |= 1 << 16;
    }

    // Bit 17: NO_GZIP equivalent — set when gzip feature is disabled
    if cfg!(not(feature = "gzip")) {
        flags |= 1 << 17;
    }

    flags
}

/// Encodes a byte-size into the 2-bit scheme used by `compile_flags`:
/// 2 → 0, 4 → 1, 8 → 2, other → 3.
#[inline]
const fn size_encoding(bytes: usize) -> u64 {
    match bytes {
        2 => 0,
        4 => 1,
        8 => 2,
        _ => 3,
    }
}

// ---------------------------------------------------------------------------
// error_message()
// ---------------------------------------------------------------------------

/// Returns the error message string for a given zlib return code.
///
/// This function maps C zlib return codes to their canonical human-readable
/// error messages, exactly reproducing the `z_errmsg` table from `zutil.c`
/// (lines 13-24).  The lookup is equivalent to the C `ERR_MSG()` macro
/// defined in `zutil.h` (line 65): `z_errmsg[2 - err]`.
///
/// # Valid codes
///
/// | Code | Name              | Message               |
/// |------|-------------------|-----------------------|
/// |   2  | `Z_NEED_DICT`     | `"need dictionary"`   |
/// |   1  | `Z_STREAM_END`    | `"stream end"`        |
/// |   0  | `Z_OK`            | `""`                  |
/// |  -1  | `Z_ERRNO`         | `"file error"`        |
/// |  -2  | `Z_STREAM_ERROR`  | `"stream error"`      |
/// |  -3  | `Z_DATA_ERROR`    | `"data error"`        |
/// |  -4  | `Z_MEM_ERROR`     | `"insufficient memory"`|
/// |  -5  | `Z_BUF_ERROR`     | `"buffer error"`      |
/// |  -6  | `Z_VERSION_ERROR` | `"incompatible version"`|
///
/// Out-of-range codes return an empty string (`""`), matching C zlib's
/// `z_errmsg[9]` sentinel entry.
///
/// # Examples
///
/// ```
/// use zlib_rs::util::version::error_message;
/// assert_eq!(error_message(0), "");               // Z_OK
/// assert_eq!(error_message(1), "stream end");     // Z_STREAM_END
/// assert_eq!(error_message(2), "need dictionary"); // Z_NEED_DICT
/// assert_eq!(error_message(-3), "data error");    // Z_DATA_ERROR
/// assert_eq!(error_message(-6), "incompatible version"); // Z_VERSION_ERROR
/// assert_eq!(error_message(99), "");              // out of range
/// ```
pub fn error_message(code: i32) -> &'static str {
    // The C z_errmsg table is indexed by `2 - code` (zutil.h line 65).
    // Valid codes range from -6 (Z_VERSION_ERROR) to +2 (Z_NEED_DICT).
    // We use a direct match for clarity and exhaustiveness.
    match code {
        2 => "need dictionary",       // Z_NEED_DICT
        1 => "stream end",            // Z_STREAM_END
        0 => "",                      // Z_OK
        -1 => "file error",           // Z_ERRNO
        -2 => "stream error",         // Z_STREAM_ERROR
        -3 => "data error",           // Z_DATA_ERROR
        -4 => "insufficient memory",  // Z_MEM_ERROR
        -5 => "buffer error",         // Z_BUF_ERROR
        -6 => "incompatible version", // Z_VERSION_ERROR
        _ => "",                      // out of range → empty (z_errmsg[9])
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zlib_version_constant() {
        assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
    }

    #[test]
    fn test_zlib_version_function() {
        assert_eq!(zlib_version(), "1.3.2.1-motley");
        assert_eq!(zlib_version(), ZLIB_VERSION);
    }

    #[test]
    fn test_compile_flags_type_sizes() {
        let flags = compile_flags();

        // Bits 0-1: u32 = 4 bytes → encoding 1
        assert_eq!(flags & 0x3, 1);

        // Bits 2-3: u64 = 8 bytes → encoding 2
        assert_eq!((flags >> 2) & 0x3, 2);

        // Bits 4-5: usize = 4 or 8 bytes → encoding 1 or 2
        let usize_enc = (flags >> 4) & 0x3;
        if cfg!(target_pointer_width = "32") {
            assert_eq!(usize_enc, 1);
        } else if cfg!(target_pointer_width = "64") {
            assert_eq!(usize_enc, 2);
        }

        // Bits 6-7: u64 offset = 8 bytes → encoding 2
        assert_eq!((flags >> 6) & 0x3, 2);
    }

    #[test]
    fn test_compile_flags_nonzero() {
        // At minimum, the type-size bits are always set.
        assert_ne!(compile_flags(), 0);
    }

    #[test]
    fn test_compile_flags_debug_bit() {
        let flags = compile_flags();
        let debug_bit = (flags >> 8) & 1;
        if cfg!(debug_assertions) {
            assert_eq!(debug_bit, 1);
        } else {
            assert_eq!(debug_bit, 0);
        }
    }

    #[test]
    fn test_compile_flags_feature_bits() {
        let flags = compile_flags();

        // Bit 16: gz-io feature disabled
        let gz_io_disabled = (flags >> 16) & 1;
        if cfg!(not(feature = "gz-io")) {
            assert_eq!(gz_io_disabled, 1);
        } else {
            assert_eq!(gz_io_disabled, 0);
        }

        // Bit 17: gzip feature disabled
        let gzip_disabled = (flags >> 17) & 1;
        if cfg!(not(feature = "gzip")) {
            assert_eq!(gzip_disabled, 1);
        } else {
            assert_eq!(gzip_disabled, 0);
        }
    }

    #[test]
    fn test_error_message_all_valid_codes() {
        // Must match z_errmsg table from zutil.c lines 13-24 exactly.
        assert_eq!(error_message(2), "need dictionary");
        assert_eq!(error_message(1), "stream end");
        assert_eq!(error_message(0), "");
        assert_eq!(error_message(-1), "file error");
        assert_eq!(error_message(-2), "stream error");
        assert_eq!(error_message(-3), "data error");
        assert_eq!(error_message(-4), "insufficient memory");
        assert_eq!(error_message(-5), "buffer error");
        assert_eq!(error_message(-6), "incompatible version");
    }

    #[test]
    fn test_error_message_out_of_range() {
        // Out-of-range codes must return empty string.
        assert_eq!(error_message(3), "");
        assert_eq!(error_message(-7), "");
        assert_eq!(error_message(100), "");
        assert_eq!(error_message(-100), "");
        assert_eq!(error_message(i32::MAX), "");
        assert_eq!(error_message(i32::MIN), "");
    }

    #[test]
    fn test_size_encoding_helper() {
        assert_eq!(size_encoding(2), 0);
        assert_eq!(size_encoding(4), 1);
        assert_eq!(size_encoding(8), 2);
        assert_eq!(size_encoding(1), 3);
        assert_eq!(size_encoding(16), 3);
        assert_eq!(size_encoding(0), 3);
    }
}
