//! Error handling types for the zlib-rs crate.
//!
//! This module provides the Rust equivalents of C zlib's integer return codes
//! (defined in `zlib.h` lines 181–192) and the `z_errmsg` error message table
//! (defined in `zutil.c` lines 13–24). Instead of raw integer codes, zlib-rs
//! uses two enums — [`ZlibError`] for error conditions (negative C codes) and
//! [`ReturnCode`] for success conditions (non-negative C codes) — combined via
//! the [`ZlibResult`] type alias.
//!
//! # C-to-Rust Mapping
//!
//! | C Constant        | Value | Rust Equivalent                  |
//! |-------------------|-------|----------------------------------|
//! | `Z_OK`            |   0   | `Ok(ReturnCode::Ok)`             |
//! | `Z_STREAM_END`    |   1   | `Ok(ReturnCode::StreamEnd)`      |
//! | `Z_NEED_DICT`     |   2   | `Ok(ReturnCode::NeedDict)`       |
//! | `Z_ERRNO`         |  -1   | `Err(ZlibError::Errno)`          |
//! | `Z_STREAM_ERROR`  |  -2   | `Err(ZlibError::StreamError)`    |
//! | `Z_DATA_ERROR`    |  -3   | `Err(ZlibError::DataError)`      |
//! | `Z_MEM_ERROR`     |  -4   | `Err(ZlibError::MemError)`       |
//! | `Z_BUF_ERROR`     |  -5   | `Err(ZlibError::BufError)`       |
//! | `Z_VERSION_ERROR` |  -6   | `Err(ZlibError::VersionError)`   |
//!
//! # Examples
//!
//! ```
//! use zlib_rs::error::{ZlibError, ReturnCode, ZlibResult, error_message, code_to_result};
//!
//! // Convert a C return code to a Rust Result
//! let result: ZlibResult = code_to_result(0);
//! assert_eq!(result, Ok(ReturnCode::Ok));
//!
//! let err_result: ZlibResult = code_to_result(-3);
//! assert_eq!(err_result, Err(ZlibError::DataError));
//!
//! // Look up error messages matching the C z_errmsg table
//! assert_eq!(error_message(-3), "data error");
//! assert_eq!(error_message(0), "");
//! ```

use core::fmt;

// ---------------------------------------------------------------------------
// ZlibError — maps to C zlib's negative return codes
// ---------------------------------------------------------------------------

/// Error types for zlib operations.
///
/// Each variant maps to one of C zlib's negative return codes as defined in
/// `zlib.h` (lines 184–189). The [`Display`](fmt::Display) implementation
/// returns the exact string from the C `z_errmsg` table (`zutil.c` lines
/// 13–24).
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ZlibError;
///
/// let err = ZlibError::DataError;
/// assert_eq!(err.to_c_code(), -3);
/// assert_eq!(format!("{err}"), "data error");
///
/// let round_trip = ZlibError::from_c_code(-3);
/// assert_eq!(round_trip, Some(ZlibError::DataError));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZlibError {
    /// `Z_ERRNO` (-1): A file I/O error occurred.
    ///
    /// When returned from gzip file operations, the caller should inspect
    /// `std::io::Error` (or the OS `errno`) for more detail.
    Errno,

    /// `Z_STREAM_ERROR` (-2): The stream state is inconsistent, or a
    /// parameter passed to a function was invalid.
    ///
    /// Common causes include calling a function with an uninitialised stream,
    /// or providing an out-of-range `window_bits` value.
    StreamError,

    /// `Z_DATA_ERROR` (-3): The input data was corrupted or does not conform
    /// to the expected compressed format.
    ///
    /// This error is returned by `inflate` when the compressed data fails
    /// checksum verification or contains invalid Huffman codes.
    DataError,

    /// `Z_MEM_ERROR` (-4): Insufficient memory to complete the operation.
    ///
    /// Allocation of internal buffers (sliding window, hash tables, Huffman
    /// trees) failed.
    MemError,

    /// `Z_BUF_ERROR` (-5): The output buffer is full or the input buffer is
    /// empty and no progress could be made.
    ///
    /// **Not fatal** — the operation can be retried after providing more
    /// output space or additional input data.
    BufError,

    /// `Z_VERSION_ERROR` (-6): The zlib library version is incompatible with
    /// the version expected by the caller.
    ///
    /// Typically triggered when `deflate_init` or `inflate_init` detects a
    /// version string mismatch.
    VersionError,
}

impl ZlibError {
    /// Converts a C-style zlib return code to its corresponding [`ZlibError`].
    ///
    /// Returns `Some(error)` for recognised negative codes (-1 through -6),
    /// or `None` for any other value (including non-negative success codes).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    ///
    /// assert_eq!(ZlibError::from_c_code(-1), Some(ZlibError::Errno));
    /// assert_eq!(ZlibError::from_c_code(-6), Some(ZlibError::VersionError));
    /// assert_eq!(ZlibError::from_c_code(0), None);   // Z_OK is not an error
    /// assert_eq!(ZlibError::from_c_code(-7), None);   // unknown code
    /// ```
    #[inline]
    pub fn from_c_code(code: i32) -> Option<Self> {
        match code {
            -1 => Some(ZlibError::Errno),
            -2 => Some(ZlibError::StreamError),
            -3 => Some(ZlibError::DataError),
            -4 => Some(ZlibError::MemError),
            -5 => Some(ZlibError::BufError),
            -6 => Some(ZlibError::VersionError),
            _ => None,
        }
    }

    /// Converts this error to the corresponding C-style integer return code.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    ///
    /// assert_eq!(ZlibError::Errno.to_c_code(), -1);
    /// assert_eq!(ZlibError::VersionError.to_c_code(), -6);
    /// ```
    #[inline]
    pub fn to_c_code(self) -> i32 {
        match self {
            ZlibError::Errno => -1,
            ZlibError::StreamError => -2,
            ZlibError::DataError => -3,
            ZlibError::MemError => -4,
            ZlibError::BufError => -5,
            ZlibError::VersionError => -6,
        }
    }
}

/// Displays the human-readable error message matching the C `z_errmsg` table.
///
/// The messages are identical to those in `zutil.c` lines 13–24:
///
/// | Variant        | Message                |
/// |----------------|------------------------|
/// | `Errno`        | `"file error"`         |
/// | `StreamError`  | `"stream error"`       |
/// | `DataError`    | `"data error"`         |
/// | `MemError`     | `"insufficient memory"`|
/// | `BufError`     | `"buffer error"`       |
/// | `VersionError` | `"incompatible version"`|
impl fmt::Display for ZlibError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            ZlibError::Errno => "file error",
            ZlibError::StreamError => "stream error",
            ZlibError::DataError => "data error",
            ZlibError::MemError => "insufficient memory",
            ZlibError::BufError => "buffer error",
            ZlibError::VersionError => "incompatible version",
        };
        write!(f, "{msg}")
    }
}

/// Implements the standard `Error` trait when the `std` feature is enabled.
///
/// This allows [`ZlibError`] to integrate with the broader Rust error
/// ecosystem (e.g. `anyhow`, `thiserror`, the `?` operator with
/// `Box<dyn Error>`).
#[cfg(feature = "std")]
impl std::error::Error for ZlibError {}

// ---------------------------------------------------------------------------
// ReturnCode — maps to C zlib's non-negative return codes
// ---------------------------------------------------------------------------

/// Successful (non-negative) return codes from zlib operations.
///
/// Each variant maps to one of C zlib's non-negative return codes as defined
/// in `zlib.h` (lines 181–183). These indicate the *type* of success rather
/// than an error.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ReturnCode;
///
/// let code = ReturnCode::StreamEnd;
/// assert_eq!(code.to_c_code(), 1);
/// assert_eq!(format!("{code}"), "stream end");
///
/// let round_trip = ReturnCode::from_c_code(1);
/// assert_eq!(round_trip, Some(ReturnCode::StreamEnd));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReturnCode {
    /// `Z_OK` (0): The operation completed successfully and more data may be
    /// processed.
    Ok,

    /// `Z_STREAM_END` (1): All input has been consumed and all output has
    /// been produced.
    ///
    /// Returned when `flush` is `Z_FINISH` and the compression or
    /// decompression has completed normally.
    StreamEnd,

    /// `Z_NEED_DICT` (2): A preset dictionary is required before
    /// decompression can continue.
    ///
    /// The `adler` field of the stream contains the Adler-32 checksum of the
    /// dictionary that should be supplied via `inflate_set_dictionary`.
    NeedDict,
}

impl ReturnCode {
    /// Converts a C-style zlib return code to its corresponding
    /// [`ReturnCode`].
    ///
    /// Returns `Some(code)` for recognised non-negative codes (0, 1, 2), or
    /// `None` for any other value (including negative error codes).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::from_c_code(0), Some(ReturnCode::Ok));
    /// assert_eq!(ReturnCode::from_c_code(2), Some(ReturnCode::NeedDict));
    /// assert_eq!(ReturnCode::from_c_code(-1), None);  // error, not a success code
    /// assert_eq!(ReturnCode::from_c_code(3), None);    // unknown code
    /// ```
    #[inline]
    pub fn from_c_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(ReturnCode::Ok),
            1 => Some(ReturnCode::StreamEnd),
            2 => Some(ReturnCode::NeedDict),
            _ => None,
        }
    }

    /// Converts this return code to the corresponding C-style integer value.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::Ok.to_c_code(), 0);
    /// assert_eq!(ReturnCode::NeedDict.to_c_code(), 2);
    /// ```
    #[inline]
    pub fn to_c_code(self) -> i32 {
        match self {
            ReturnCode::Ok => 0,
            ReturnCode::StreamEnd => 1,
            ReturnCode::NeedDict => 2,
        }
    }
}

/// Displays the human-readable message matching the C `z_errmsg` table.
///
/// | Variant     | Message              |
/// |-------------|----------------------|
/// | `Ok`        | `""`                 |
/// | `StreamEnd` | `"stream end"`       |
/// | `NeedDict`  | `"need dictionary"`  |
impl fmt::Display for ReturnCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            ReturnCode::Ok => "",
            ReturnCode::StreamEnd => "stream end",
            ReturnCode::NeedDict => "need dictionary",
        };
        write!(f, "{msg}")
    }
}

// ---------------------------------------------------------------------------
// ZlibResult — convenience type alias
// ---------------------------------------------------------------------------

/// Result type for zlib operations.
///
/// The `Ok` variant carries a [`ReturnCode`] indicating the specific success
/// condition (e.g. `Ok`, `StreamEnd`, or `NeedDict`). The `Err` variant
/// carries a [`ZlibError`] describing the failure.
///
/// This replaces the C convention of returning a single `int` where negative
/// values are errors and non-negative values are success indicators.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{ZlibResult, ReturnCode, ZlibError};
///
/// fn example_op() -> ZlibResult {
///     // Successful operation
///     Ok(ReturnCode::Ok)
/// }
///
/// fn failing_op() -> ZlibResult {
///     Err(ZlibError::DataError)
/// }
///
/// assert!(example_op().is_ok());
/// assert!(failing_op().is_err());
/// ```
pub type ZlibResult = Result<ReturnCode, ZlibError>;

// ---------------------------------------------------------------------------
// error_message — replacement for C z_errmsg table and ERR_MSG macro
// ---------------------------------------------------------------------------

/// Returns the error message string for a given C-style return code.
///
/// This is the Rust equivalent of indexing into the C `z_errmsg` table via the
/// `ERR_MSG(err)` macro defined in `zutil.h` line 65:
///
/// ```c
/// #define ERR_MSG(err) z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// The returned strings are **identical** to those in `zutil.c` lines 13–24.
/// Out-of-range codes return an empty string, matching the sentinel entry
/// `z_errmsg[9]`.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::error_message;
///
/// assert_eq!(error_message(2), "need dictionary");
/// assert_eq!(error_message(1), "stream end");
/// assert_eq!(error_message(0), "");
/// assert_eq!(error_message(-1), "file error");
/// assert_eq!(error_message(-2), "stream error");
/// assert_eq!(error_message(-3), "data error");
/// assert_eq!(error_message(-4), "insufficient memory");
/// assert_eq!(error_message(-5), "buffer error");
/// assert_eq!(error_message(-6), "incompatible version");
/// assert_eq!(error_message(-7), "");   // out of range
/// assert_eq!(error_message(99), "");   // out of range
/// ```
#[inline]
pub fn error_message(code: i32) -> &'static str {
    match code {
        2 => "need dictionary",
        1 => "stream end",
        0 => "",
        -1 => "file error",
        -2 => "stream error",
        -3 => "data error",
        -4 => "insufficient memory",
        -5 => "buffer error",
        -6 => "incompatible version",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// code_to_result — internal helper for C code → ZlibResult conversion
// ---------------------------------------------------------------------------

/// Converts a C-style integer return code into a [`ZlibResult`].
///
/// This is the primary bridge between the internal C-compatible integer codes
/// and Rust's `Result` type. It handles all documented return codes:
///
/// - `0` → `Ok(ReturnCode::Ok)`
/// - `1` → `Ok(ReturnCode::StreamEnd)`
/// - `2` → `Ok(ReturnCode::NeedDict)`
/// - `-1` through `-6` → the corresponding `Err(ZlibError::*)`
/// - Any other negative value → `Err(ZlibError::StreamError)` (fallback)
/// - Any other positive value → `Err(ZlibError::StreamError)` (invalid)
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{code_to_result, ReturnCode, ZlibError};
///
/// assert_eq!(code_to_result(0), Ok(ReturnCode::Ok));
/// assert_eq!(code_to_result(1), Ok(ReturnCode::StreamEnd));
/// assert_eq!(code_to_result(2), Ok(ReturnCode::NeedDict));
/// assert_eq!(code_to_result(-3), Err(ZlibError::DataError));
/// assert_eq!(code_to_result(-99), Err(ZlibError::StreamError)); // unknown negative
/// assert_eq!(code_to_result(42), Err(ZlibError::StreamError));  // unknown positive
/// ```
#[inline]
pub fn code_to_result(code: i32) -> ZlibResult {
    match code {
        0 => Ok(ReturnCode::Ok),
        1 => Ok(ReturnCode::StreamEnd),
        2 => Ok(ReturnCode::NeedDict),
        c if c < 0 => Err(ZlibError::from_c_code(c).unwrap_or(ZlibError::StreamError)),
        _ => Err(ZlibError::StreamError),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ZlibError tests --

    #[test]
    fn zlib_error_from_c_code_valid() {
        assert_eq!(ZlibError::from_c_code(-1), Some(ZlibError::Errno));
        assert_eq!(ZlibError::from_c_code(-2), Some(ZlibError::StreamError));
        assert_eq!(ZlibError::from_c_code(-3), Some(ZlibError::DataError));
        assert_eq!(ZlibError::from_c_code(-4), Some(ZlibError::MemError));
        assert_eq!(ZlibError::from_c_code(-5), Some(ZlibError::BufError));
        assert_eq!(ZlibError::from_c_code(-6), Some(ZlibError::VersionError));
    }

    #[test]
    fn zlib_error_from_c_code_invalid() {
        assert_eq!(ZlibError::from_c_code(0), None);
        assert_eq!(ZlibError::from_c_code(1), None);
        assert_eq!(ZlibError::from_c_code(-7), None);
        assert_eq!(ZlibError::from_c_code(100), None);
        assert_eq!(ZlibError::from_c_code(i32::MIN), None);
        assert_eq!(ZlibError::from_c_code(i32::MAX), None);
    }

    #[test]
    fn zlib_error_to_c_code() {
        assert_eq!(ZlibError::Errno.to_c_code(), -1);
        assert_eq!(ZlibError::StreamError.to_c_code(), -2);
        assert_eq!(ZlibError::DataError.to_c_code(), -3);
        assert_eq!(ZlibError::MemError.to_c_code(), -4);
        assert_eq!(ZlibError::BufError.to_c_code(), -5);
        assert_eq!(ZlibError::VersionError.to_c_code(), -6);
    }

    #[test]
    fn zlib_error_roundtrip() {
        let errors = [
            ZlibError::Errno,
            ZlibError::StreamError,
            ZlibError::DataError,
            ZlibError::MemError,
            ZlibError::BufError,
            ZlibError::VersionError,
        ];
        for err in &errors {
            let code = err.to_c_code();
            let restored = ZlibError::from_c_code(code);
            assert_eq!(restored, Some(*err), "Roundtrip failed for {err:?}");
        }
    }

    #[test]
    fn zlib_error_display_matches_z_errmsg() {
        // Messages must exactly match the C z_errmsg table (zutil.c lines 13-24)
        assert_eq!(format!("{}", ZlibError::Errno), "file error");
        assert_eq!(format!("{}", ZlibError::StreamError), "stream error");
        assert_eq!(format!("{}", ZlibError::DataError), "data error");
        assert_eq!(format!("{}", ZlibError::MemError), "insufficient memory");
        assert_eq!(format!("{}", ZlibError::BufError), "buffer error");
        assert_eq!(
            format!("{}", ZlibError::VersionError),
            "incompatible version"
        );
    }

    #[test]
    fn zlib_error_derive_traits() {
        // Debug
        let debug_str = format!("{:?}", ZlibError::DataError);
        assert_eq!(debug_str, "DataError");

        // Clone + Copy
        let err = ZlibError::MemError;
        let cloned = err;
        assert_eq!(err, cloned);

        // Eq + PartialEq
        assert_eq!(ZlibError::BufError, ZlibError::BufError);
        assert_ne!(ZlibError::BufError, ZlibError::Errno);

        // Hash — verify it doesn't panic
        use core::hash::{Hash, Hasher};
        struct DummyHasher(u64);
        impl Hasher for DummyHasher {
            fn finish(&self) -> u64 {
                self.0
            }
            fn write(&mut self, bytes: &[u8]) {
                for &b in bytes {
                    self.0 = self.0.wrapping_mul(31).wrapping_add(b as u64);
                }
            }
        }
        let mut h = DummyHasher(0);
        ZlibError::Errno.hash(&mut h);
        let h1 = h.finish();
        let mut h2 = DummyHasher(0);
        ZlibError::StreamError.hash(&mut h2);
        let h2_val = h2.finish();
        // Different variants should (typically) have different hashes
        assert_ne!(h1, h2_val);
    }

    // -- ReturnCode tests --

    #[test]
    fn return_code_from_c_code_valid() {
        assert_eq!(ReturnCode::from_c_code(0), Some(ReturnCode::Ok));
        assert_eq!(ReturnCode::from_c_code(1), Some(ReturnCode::StreamEnd));
        assert_eq!(ReturnCode::from_c_code(2), Some(ReturnCode::NeedDict));
    }

    #[test]
    fn return_code_from_c_code_invalid() {
        assert_eq!(ReturnCode::from_c_code(-1), None);
        assert_eq!(ReturnCode::from_c_code(3), None);
        assert_eq!(ReturnCode::from_c_code(-6), None);
        assert_eq!(ReturnCode::from_c_code(i32::MIN), None);
        assert_eq!(ReturnCode::from_c_code(i32::MAX), None);
    }

    #[test]
    fn return_code_to_c_code() {
        assert_eq!(ReturnCode::Ok.to_c_code(), 0);
        assert_eq!(ReturnCode::StreamEnd.to_c_code(), 1);
        assert_eq!(ReturnCode::NeedDict.to_c_code(), 2);
    }

    #[test]
    fn return_code_roundtrip() {
        let codes = [ReturnCode::Ok, ReturnCode::StreamEnd, ReturnCode::NeedDict];
        for code in &codes {
            let c_val = code.to_c_code();
            let restored = ReturnCode::from_c_code(c_val);
            assert_eq!(restored, Some(*code), "Roundtrip failed for {code:?}");
        }
    }

    #[test]
    fn return_code_display_matches_z_errmsg() {
        // Messages must match the C z_errmsg table exactly
        assert_eq!(format!("{}", ReturnCode::Ok), "");
        assert_eq!(format!("{}", ReturnCode::StreamEnd), "stream end");
        assert_eq!(format!("{}", ReturnCode::NeedDict), "need dictionary");
    }

    // -- error_message tests --

    #[test]
    fn error_message_full_table() {
        // Verify the complete z_errmsg table (zutil.c lines 13-24)
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
    fn error_message_out_of_range() {
        // C ERR_MSG macro maps out-of-range to z_errmsg[9] which is ""
        assert_eq!(error_message(-7), "");
        assert_eq!(error_message(3), "");
        assert_eq!(error_message(100), "");
        assert_eq!(error_message(-100), "");
        assert_eq!(error_message(i32::MIN), "");
        assert_eq!(error_message(i32::MAX), "");
    }

    // -- code_to_result tests --

    #[test]
    fn code_to_result_success_codes() {
        assert_eq!(code_to_result(0), Ok(ReturnCode::Ok));
        assert_eq!(code_to_result(1), Ok(ReturnCode::StreamEnd));
        assert_eq!(code_to_result(2), Ok(ReturnCode::NeedDict));
    }

    #[test]
    fn code_to_result_error_codes() {
        assert_eq!(code_to_result(-1), Err(ZlibError::Errno));
        assert_eq!(code_to_result(-2), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(-3), Err(ZlibError::DataError));
        assert_eq!(code_to_result(-4), Err(ZlibError::MemError));
        assert_eq!(code_to_result(-5), Err(ZlibError::BufError));
        assert_eq!(code_to_result(-6), Err(ZlibError::VersionError));
    }

    #[test]
    fn code_to_result_unknown_codes() {
        // Unknown negative codes fall back to StreamError
        assert_eq!(code_to_result(-7), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(-100), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(i32::MIN), Err(ZlibError::StreamError));

        // Unknown positive codes also fall back to StreamError
        assert_eq!(code_to_result(3), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(42), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(i32::MAX), Err(ZlibError::StreamError));
    }

    // -- ZlibResult usage tests --

    #[test]
    fn zlib_result_ok_variant() {
        let result: ZlibResult = Ok(ReturnCode::Ok);
        assert!(result.is_ok());
        assert_eq!(result, Ok(ReturnCode::Ok));
    }

    #[test]
    fn zlib_result_err_variant() {
        let result: ZlibResult = Err(ZlibError::DataError);
        assert!(result.is_err());
        assert_eq!(result, Err(ZlibError::DataError));
    }

    #[test]
    fn zlib_result_question_mark_propagation() {
        fn inner_ok() -> ZlibResult {
            let code = code_to_result(0)?;
            assert_eq!(code, ReturnCode::Ok);
            Ok(code)
        }

        fn inner_err() -> ZlibResult {
            let _code = code_to_result(-3)?;
            // Should not reach here
            unreachable!("should have propagated error");
        }

        assert_eq!(inner_ok(), Ok(ReturnCode::Ok));
        assert_eq!(inner_err(), Err(ZlibError::DataError));
    }

    // -- Consistency test: Display matches error_message --

    #[test]
    fn display_consistent_with_error_message() {
        // ZlibError Display must match error_message for corresponding codes
        assert_eq!(format!("{}", ZlibError::Errno), error_message(-1));
        assert_eq!(format!("{}", ZlibError::StreamError), error_message(-2));
        assert_eq!(format!("{}", ZlibError::DataError), error_message(-3));
        assert_eq!(format!("{}", ZlibError::MemError), error_message(-4));
        assert_eq!(format!("{}", ZlibError::BufError), error_message(-5));
        assert_eq!(format!("{}", ZlibError::VersionError), error_message(-6));

        // ReturnCode Display must match error_message for corresponding codes
        assert_eq!(format!("{}", ReturnCode::Ok), error_message(0));
        assert_eq!(format!("{}", ReturnCode::StreamEnd), error_message(1));
        assert_eq!(format!("{}", ReturnCode::NeedDict), error_message(2));
    }
}
