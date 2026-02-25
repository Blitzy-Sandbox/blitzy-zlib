//! Error types and return codes for zlib operations.
//!
//! This module ports the zlib return codes from `zlib.h` (lines 181–189) and the
//! error message array from `zutil.c` (lines 13–24) into idiomatic Rust error types.
//!
//! Two primary types are provided:
//!
//! - [`ReturnCode`] — An enum mapping 1:1 to the C `Z_OK`, `Z_STREAM_END`, etc.
//!   constants, with `#[repr(i32)]` for FFI compatibility.
//! - [`ZlibError`] — A rich error wrapper combining a [`ReturnCode`] with an
//!   optional context message, implementing the standard [`std::error::Error`] trait.

use std::fmt;

/// Return codes for compression/decompression operations.
///
/// These codes are returned by functions such as `deflate()`, `inflate()`, and
/// related utility functions. Negative values are errors; positive values indicate
/// special but normal events.
///
/// The discriminant values are identical to the C constants defined in `zlib.h`
/// lines 181–189, and `#[repr(i32)]` guarantees the in-memory representation
/// matches the C ABI.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ReturnCode;
///
/// let code = ReturnCode::Ok;
/// assert!(!code.is_error());
/// assert_eq!(code.message(), "");
///
/// let err = ReturnCode::DataError;
/// assert!(err.is_error());
/// assert_eq!(err.message(), "data error");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ReturnCode {
    /// No error, operation completed successfully (`Z_OK = 0`).
    Ok = 0,
    /// End of compressed stream reached (`Z_STREAM_END = 1`).
    StreamEnd = 1,
    /// A preset dictionary is needed for decompression (`Z_NEED_DICT = 2`).
    NeedDict = 2,
    /// File I/O error (`Z_ERRNO = -1`).
    Errno = -1,
    /// Inconsistent stream state or invalid parameter (`Z_STREAM_ERROR = -2`).
    StreamError = -2,
    /// Compressed data corrupted (`Z_DATA_ERROR = -3`).
    DataError = -3,
    /// Insufficient memory (`Z_MEM_ERROR = -4`).
    MemError = -4,
    /// Output buffer too small (`Z_BUF_ERROR = -5`).
    BufError = -5,
    /// zlib library version mismatch (`Z_VERSION_ERROR = -6`).
    VersionError = -6,
}

impl ReturnCode {
    /// Returns `true` if this code represents an error (negative value).
    ///
    /// Non-error codes are [`Ok`](ReturnCode::Ok), [`StreamEnd`](ReturnCode::StreamEnd),
    /// and [`NeedDict`](ReturnCode::NeedDict).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert!(!ReturnCode::Ok.is_error());
    /// assert!(!ReturnCode::StreamEnd.is_error());
    /// assert!(ReturnCode::StreamError.is_error());
    /// assert!(ReturnCode::MemError.is_error());
    /// ```
    #[must_use]
    pub const fn is_error(&self) -> bool {
        (*self as i32) < 0
    }

    /// Returns the human-readable message string for this return code.
    ///
    /// The strings are character-for-character identical to the C `z_errmsg[]`
    /// array defined in `zutil.c` lines 13–24. The mapping is:
    ///
    /// | Code | Message |
    /// |------|---------|
    /// | `Ok` | `""` |
    /// | `StreamEnd` | `"stream end"` |
    /// | `NeedDict` | `"need dictionary"` |
    /// | `Errno` | `"file error"` |
    /// | `StreamError` | `"stream error"` |
    /// | `DataError` | `"data error"` |
    /// | `MemError` | `"insufficient memory"` |
    /// | `BufError` | `"buffer error"` |
    /// | `VersionError` | `"incompatible version"` |
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
    /// assert_eq!(ReturnCode::BufError.message(), "buffer error");
    /// ```
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::Ok => "",
            Self::StreamEnd => "stream end",
            Self::NeedDict => "need dictionary",
            Self::Errno => "file error",
            Self::StreamError => "stream error",
            Self::DataError => "data error",
            Self::MemError => "insufficient memory",
            Self::BufError => "buffer error",
            Self::VersionError => "incompatible version",
        }
    }
}

impl fmt::Display for ReturnCode {
    /// Formats the return code using its human-readable message string.
    ///
    /// For `ReturnCode::Ok`, which maps to an empty message, the display
    /// output is `"ok"` to provide a meaningful representation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = self.message();
        if msg.is_empty() {
            f.write_str("ok")
        } else {
            f.write_str(msg)
        }
    }
}

impl std::error::Error for ReturnCode {}

impl TryFrom<i32> for ReturnCode {
    type Error = i32;

    /// Converts a C integer return code to the corresponding [`ReturnCode`] variant.
    ///
    /// Returns `Err(value)` if the integer does not correspond to any known
    /// return code.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::try_from(0), Ok(ReturnCode::Ok));
    /// assert_eq!(ReturnCode::try_from(-3), Ok(ReturnCode::DataError));
    /// assert_eq!(ReturnCode::try_from(99), Err(99));
    /// ```
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Result::Ok(Self::Ok),
            1 => Result::Ok(Self::StreamEnd),
            2 => Result::Ok(Self::NeedDict),
            -1 => Result::Ok(Self::Errno),
            -2 => Result::Ok(Self::StreamError),
            -3 => Result::Ok(Self::DataError),
            -4 => Result::Ok(Self::MemError),
            -5 => Result::Ok(Self::BufError),
            -6 => Result::Ok(Self::VersionError),
            other => Err(other),
        }
    }
}

/// Error type for zlib operations, wrapping a [`ReturnCode`] with an optional
/// context message.
///
/// This type implements [`std::error::Error`] and [`std::fmt::Display`], making
/// it compatible with Rust's error handling ecosystem and the `?` operator.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{ZlibError, ReturnCode};
///
/// let err = ZlibError::new(ReturnCode::DataError);
/// assert_eq!(err.code(), ReturnCode::DataError);
///
/// let err_with_msg = ZlibError::with_message(
///     ReturnCode::StreamError,
///     "invalid window size",
/// );
/// assert_eq!(err_with_msg.code(), ReturnCode::StreamError);
/// ```
#[derive(Debug, Clone)]
pub struct ZlibError {
    /// The underlying return code identifying the error category.
    code: ReturnCode,
    /// An optional human-readable message providing additional context.
    message: Option<String>,
}

impl ZlibError {
    /// Creates a new `ZlibError` from the given return code without a context
    /// message.
    ///
    /// The display representation will use the return code's default message
    /// from [`ReturnCode::message()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::{ZlibError, ReturnCode};
    ///
    /// let err = ZlibError::new(ReturnCode::MemError);
    /// assert_eq!(err.code(), ReturnCode::MemError);
    /// ```
    #[must_use]
    pub fn new(code: ReturnCode) -> Self {
        Self {
            code,
            message: None,
        }
    }

    /// Creates a new `ZlibError` from the given return code with an additional
    /// context message.
    ///
    /// The context message is displayed alongside the return code's debug
    /// representation, providing richer diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::{ZlibError, ReturnCode};
    ///
    /// let err = ZlibError::with_message(
    ///     ReturnCode::DataError,
    ///     "invalid block type",
    /// );
    /// assert_eq!(err.code(), ReturnCode::DataError);
    /// assert!(format!("{err}").contains("invalid block type"));
    /// ```
    #[must_use]
    pub fn with_message(code: ReturnCode, msg: impl Into<String>) -> Self {
        Self {
            code,
            message: Some(msg.into()),
        }
    }

    /// Returns the [`ReturnCode`] associated with this error.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::{ZlibError, ReturnCode};
    ///
    /// let err = ZlibError::new(ReturnCode::BufError);
    /// assert_eq!(err.code(), ReturnCode::BufError);
    /// ```
    #[must_use]
    pub fn code(&self) -> ReturnCode {
        self.code
    }
}

impl fmt::Display for ZlibError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.message {
            Some(msg) => write!(f, "zlib error: {msg} ({:?})", self.code),
            None => write!(f, "zlib error: {:?}", self.code),
        }
    }
}

impl std::error::Error for ZlibError {}

impl From<ReturnCode> for ZlibError {
    /// Converts a [`ReturnCode`] into a [`ZlibError`] without a context message.
    ///
    /// This enables ergonomic conversion with the `?` operator and
    /// [`Into`] trait usage.
    fn from(code: ReturnCode) -> Self {
        Self::new(code)
    }
}
