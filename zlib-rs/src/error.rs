//! Error and return-code types replacing zlib's integer codes and z_errmsg table.
//!
//! The C zlib library reports outcomes through a single set of signed integer
//! status codes (`Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT`, `Z_ERRNO`, …) and maps
//! the negative ones to human-readable strings through the
//! `z_errmsg[10]` table indexed by the `ERR_MSG(err)` macro
//! (`z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]`).
//!
//! This module re-expresses that contract idiomatically while preserving the
//! exact numeric values and message strings required for ABI parity:
//!
//! * [`ReturnCode`] — the *normal* (zero or positive) outcomes. Note that
//!   `Z_NEED_DICT` is a normal control-flow signal in zlib, **not** an error,
//!   so it lives here rather than in [`ZlibError`].
//! * [`ZlibError`] — the *failure* (negative) outcomes. `Z_BUF_ERROR` is a
//!   soft / retryable condition in zlib; it is kept as its own variant and is
//!   deliberately not collapsed into another error.
//! * [`Result`] — the crate-wide result alias, `Result<ReturnCode, ZlibError>`
//!   by default.
//!
//! The module is fully `no_std`-compatible: every message is a `&'static str`,
//! so no allocation is required, and the only `std`-dependent item — the
//! `std::error::Error` implementation — is gated behind the `std` feature.

use core::fmt;

/// Normal (zero or positive) zlib status codes.
///
/// These mirror the C macros `Z_OK`, `Z_STREAM_END`, and `Z_NEED_DICT`. The
/// `#[repr(i32)]` layout guarantees the discriminants are the exact integer
/// values used by the C API, so the FFI shim can recover them with a plain
/// cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ReturnCode {
    /// Operation completed successfully (`Z_OK`, `0`).
    Ok = 0,
    /// End of the compressed stream has been reached (`Z_STREAM_END`, `1`).
    StreamEnd = 1,
    /// A preset dictionary is required to continue (`Z_NEED_DICT`, `2`).
    ///
    /// This is a normal control-flow signal, not an error.
    NeedDict = 2,
}

impl ReturnCode {
    /// Returns the underlying C integer value (`Z_OK` = `0`, etc.).
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Returns the exact `z_errmsg` string associated with this code.
    ///
    /// `Ok` maps to the empty string, matching the C table where `Z_OK`
    /// carries no message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            ReturnCode::Ok => "",
            ReturnCode::StreamEnd => "stream end",
            ReturnCode::NeedDict => "need dictionary",
        }
    }

    /// Attempts to interpret a raw C integer as a [`ReturnCode`].
    ///
    /// Returns `Some` for `0`, `1`, and `2`; any other value (including the
    /// negative error codes) yields `None`.
    pub const fn try_from_i32(value: i32) -> Option<ReturnCode> {
        match value {
            0 => Some(ReturnCode::Ok),
            1 => Some(ReturnCode::StreamEnd),
            2 => Some(ReturnCode::NeedDict),
            _ => None,
        }
    }
}

/// Failure (negative) zlib status codes.
///
/// These mirror the C macros `Z_ERRNO` through `Z_VERSION_ERROR`. As with
/// [`ReturnCode`], the `#[repr(i32)]` discriminants are the exact C integer
/// values so conversions across the FFI boundary are lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ZlibError {
    /// File / errno-level I/O error (`Z_ERRNO`, `-1`).
    ErrNo = -1,
    /// The stream state was inconsistent or the parameters were invalid
    /// (`Z_STREAM_ERROR`, `-2`).
    StreamError = -2,
    /// The input data was corrupted or otherwise invalid (`Z_DATA_ERROR`,
    /// `-3`).
    DataError = -3,
    /// There was not enough memory to complete the operation (`Z_MEM_ERROR`,
    /// `-4`).
    MemError = -4,
    /// No progress was possible; more output room or input is required
    /// (`Z_BUF_ERROR`, `-5`).
    ///
    /// This is a soft / retryable condition rather than a fatal error.
    BufError = -5,
    /// The caller's zlib version is incompatible with the library
    /// (`Z_VERSION_ERROR`, `-6`).
    VersionError = -6,
}

impl ZlibError {
    /// Returns the underlying C integer value (`Z_ERRNO` = `-1`, etc.).
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Returns the exact `z_errmsg` string associated with this error.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            ZlibError::ErrNo => "file error",
            ZlibError::StreamError => "stream error",
            ZlibError::DataError => "data error",
            ZlibError::MemError => "insufficient memory",
            ZlibError::BufError => "buffer error",
            ZlibError::VersionError => "incompatible version",
        }
    }

    /// Attempts to interpret a raw C integer as a [`ZlibError`].
    ///
    /// Returns `Some` for `-1` through `-6`; any other value (including the
    /// normal codes `0`, `1`, and `2`) yields `None`.
    pub const fn from_i32(value: i32) -> Option<ZlibError> {
        match value {
            -1 => Some(ZlibError::ErrNo),
            -2 => Some(ZlibError::StreamError),
            -3 => Some(ZlibError::DataError),
            -4 => Some(ZlibError::MemError),
            -5 => Some(ZlibError::BufError),
            -6 => Some(ZlibError::VersionError),
            _ => None,
        }
    }
}

impl fmt::Display for ZlibError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

// The `std::error::Error` trait only exists with the standard library, so this
// implementation is gated behind the `std` feature. Under `no_std` builds the
// crate stays clean and never references `std`.
#[cfg(feature = "std")]
impl std::error::Error for ZlibError {}

/// The crate-wide result type returned by the safe `zlib-rs` core.
///
/// The success payload defaults to [`ReturnCode`], yielding the canonical
/// `Result<ReturnCode, ZlibError>` form from the design. The type parameter
/// lets other modules return domain-specific success values (for example
/// `Result<usize>`) while keeping [`ZlibError`] as the single, uniform error
/// type across the crate.
pub type Result<T = ReturnCode> = core::result::Result<T, ZlibError>;

impl From<ReturnCode> for i32 {
    /// Converts a normal status code back to its raw C integer value.
    fn from(code: ReturnCode) -> Self {
        code as i32
    }
}

impl From<ZlibError> for i32 {
    /// Converts an error code back to its raw C integer value.
    fn from(error: ZlibError) -> Self {
        error as i32
    }
}

/// Maps any raw zlib C integer onto the split [`ReturnCode`] / [`ZlibError`]
/// model.
///
/// * `0`, `1`, `2` become `Ok(ReturnCode::…)`.
/// * `-1` through `-6` become `Err(ZlibError::…)`.
/// * Any other (unrecognized) value is treated as a generic stream error and
///   becomes `Err(ZlibError::StreamError)`. This conservative fallback mirrors
///   how an unexpected status from a misbehaving caller is most safely
///   surfaced.
pub const fn return_code_from_i32(value: i32) -> Result<ReturnCode> {
    match ReturnCode::try_from_i32(value) {
        Some(code) => Ok(code),
        None => match ZlibError::from_i32(value) {
            Some(error) => Err(error),
            None => Err(ZlibError::StreamError),
        },
    }
}

/// Returns the `z_errmsg` message string for a raw C status code.
///
/// This reproduces the C `ERR_MSG(err)` macro / `zError()` function exactly,
/// including the clamping behavior: any code outside the inclusive range
/// `-6..=2` maps to the empty string, just as the C macro indexes the trailing
/// empty slot (`z_errmsg[9]`).
#[must_use]
pub const fn err_msg(code: i32) -> &'static str {
    match ReturnCode::try_from_i32(code) {
        Some(rc) => rc.message(),
        None => match ZlibError::from_i32(code) {
            Some(err) => err.message(),
            None => "",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny stack-backed [`core::fmt::Write`] sink so the [`fmt::Display`]
    /// implementation can be exercised without pulling in `alloc` or `std`.
    struct StackBuf {
        buf: [u8; 64],
        len: usize,
    }

    impl StackBuf {
        const fn new() -> Self {
            Self {
                buf: [0; 64],
                len: 0,
            }
        }

        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buf[..self.len]).expect("ascii message")
        }
    }

    impl fmt::Write for StackBuf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let bytes = s.as_bytes();
            let end = self.len + bytes.len();
            if end > self.buf.len() {
                return Err(fmt::Error);
            }
            self.buf[self.len..end].copy_from_slice(bytes);
            self.len = end;
            Ok(())
        }
    }

    #[test]
    fn return_code_integer_values_match_c() {
        assert_eq!(ReturnCode::Ok.as_i32(), 0);
        assert_eq!(ReturnCode::StreamEnd.as_i32(), 1);
        assert_eq!(ReturnCode::NeedDict.as_i32(), 2);
    }

    #[test]
    fn zlib_error_integer_values_match_c() {
        assert_eq!(ZlibError::ErrNo.as_i32(), -1);
        assert_eq!(ZlibError::StreamError.as_i32(), -2);
        assert_eq!(ZlibError::DataError.as_i32(), -3);
        assert_eq!(ZlibError::MemError.as_i32(), -4);
        assert_eq!(ZlibError::BufError.as_i32(), -5);
        assert_eq!(ZlibError::VersionError.as_i32(), -6);
    }

    #[test]
    fn messages_are_byte_identical_to_z_errmsg() {
        assert_eq!(ReturnCode::Ok.message(), "");
        assert_eq!(ReturnCode::StreamEnd.message(), "stream end");
        assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
        assert_eq!(ZlibError::ErrNo.message(), "file error");
        assert_eq!(ZlibError::StreamError.message(), "stream error");
        assert_eq!(ZlibError::DataError.message(), "data error");
        assert_eq!(ZlibError::MemError.message(), "insufficient memory");
        assert_eq!(ZlibError::BufError.message(), "buffer error");
        assert_eq!(ZlibError::VersionError.message(), "incompatible version");
    }

    #[test]
    fn err_msg_reproduces_c_err_msg_macro() {
        // In-range codes map through `2 - err`.
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(1), "stream end");
        assert_eq!(err_msg(0), "");
        assert_eq!(err_msg(-1), "file error");
        assert_eq!(err_msg(-2), "stream error");
        assert_eq!(err_msg(-3), "data error");
        assert_eq!(err_msg(-4), "insufficient memory");
        assert_eq!(err_msg(-5), "buffer error");
        assert_eq!(err_msg(-6), "incompatible version");
        // Out-of-range codes clamp to the empty string (the trailing slot).
        assert_eq!(err_msg(3), "");
        assert_eq!(err_msg(99), "");
        assert_eq!(err_msg(-7), "");
        assert_eq!(err_msg(i32::MIN), "");
        assert_eq!(err_msg(i32::MAX), "");
    }

    #[test]
    fn return_code_from_i32_splits_into_ok_and_err() {
        assert_eq!(return_code_from_i32(0), Ok(ReturnCode::Ok));
        assert_eq!(return_code_from_i32(1), Ok(ReturnCode::StreamEnd));
        assert_eq!(return_code_from_i32(2), Ok(ReturnCode::NeedDict));
        assert_eq!(return_code_from_i32(-1), Err(ZlibError::ErrNo));
        assert_eq!(return_code_from_i32(-3), Err(ZlibError::DataError));
        assert_eq!(return_code_from_i32(-6), Err(ZlibError::VersionError));
        // Unrecognized values fall back to a generic stream error.
        assert_eq!(return_code_from_i32(42), Err(ZlibError::StreamError));
        assert_eq!(return_code_from_i32(-99), Err(ZlibError::StreamError));
    }

    #[test]
    fn try_from_i32_and_from_i32_round_trip() {
        assert_eq!(ReturnCode::try_from_i32(0), Some(ReturnCode::Ok));
        assert_eq!(ReturnCode::try_from_i32(2), Some(ReturnCode::NeedDict));
        assert_eq!(ReturnCode::try_from_i32(-1), None);
        assert_eq!(ReturnCode::try_from_i32(3), None);
        assert_eq!(ZlibError::from_i32(-1), Some(ZlibError::ErrNo));
        assert_eq!(ZlibError::from_i32(-6), Some(ZlibError::VersionError));
        assert_eq!(ZlibError::from_i32(0), None);
        assert_eq!(ZlibError::from_i32(-7), None);

        // Every variant survives a round trip through its integer value.
        for code in [ReturnCode::Ok, ReturnCode::StreamEnd, ReturnCode::NeedDict] {
            assert_eq!(ReturnCode::try_from_i32(code.as_i32()), Some(code));
        }
        for error in [
            ZlibError::ErrNo,
            ZlibError::StreamError,
            ZlibError::DataError,
            ZlibError::MemError,
            ZlibError::BufError,
            ZlibError::VersionError,
        ] {
            assert_eq!(ZlibError::from_i32(error.as_i32()), Some(error));
        }
    }

    #[test]
    fn into_i32_conversions() {
        assert_eq!(i32::from(ReturnCode::NeedDict), 2);
        assert_eq!(i32::from(ZlibError::MemError), -4);
        let buf_err: i32 = ZlibError::BufError.into();
        assert_eq!(buf_err, -5);
        let stream_end: i32 = ReturnCode::StreamEnd.into();
        assert_eq!(stream_end, 1);
    }

    #[test]
    fn display_delegates_to_message() {
        use core::fmt::Write;
        let mut buf = StackBuf::new();
        write!(buf, "{}", ZlibError::DataError).expect("write fits buffer");
        assert_eq!(buf.as_str(), "data error");
        assert_eq!(buf.as_str(), ZlibError::DataError.message());
    }

    #[test]
    fn err_msg_agrees_with_enum_messages() {
        // The free `err_msg` helper must stay in lockstep with the enum
        // accessors for every valid code.
        for code in -6..=2 {
            let expected = match ReturnCode::try_from_i32(code) {
                Some(rc) => rc.message(),
                None => ZlibError::from_i32(code)
                    .expect("negative code in range")
                    .message(),
            };
            assert_eq!(err_msg(code), expected);
        }
    }
}
