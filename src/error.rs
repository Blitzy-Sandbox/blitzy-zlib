//! Crate-wide error vocabulary for the `zlib-rs` compression library.
//!
//! This is the lowest-level, dependency-free foundation of the crate. Every
//! other layer — the `deflate` compression engine, the `inflate` decompression
//! engine, the `gz` file-I/O layer, the `util` one-call wrappers, and the
//! C-compatible `ffi` boundary — shares the three types defined here:
//!
//! * [`ReturnCode`] — a `#[repr(i32)]` mirror of the C `Z_*` return codes. The
//!   discriminants are byte-exact with `zlib.h` and *are* the FFI ABI: the
//!   `ffi` layer emits these integers verbatim across the C boundary.
//! * [`ZlibError`] — the idiomatic error half of the crate. Internal engines
//!   return [`Result`] (an alias for `Result<T, ZlibError>`); the integer `Z_*`
//!   codes are re-materialized only when crossing back into C.
//! * [`Result`] — the crate-internal result alias, `Result<T, ZlibError>`.
//!
//! # `no_std`
//!
//! The module is `no_std`-clean: it depends only on `core::fmt`. The single
//! `std`-only trait implementation (`std::error::Error`) is gated behind the
//! `std` cargo feature, so the types remain usable in `no_std` builds.
//!
//! # Source mapping
//!
//! The values are ported verbatim from the C baseline of zlib `1.3.2.1-motley`:
//!
//! * the return-code integers come from `zlib.h` (`Z_OK` … `Z_VERSION_ERROR`);
//! * the message strings come from the `z_errmsg` table in `zutil.c`, which the
//!   C code addresses through the
//!   `ERR_MSG(err) = z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]` macro
//!   declared in `zutil.h`. [`ReturnCode::message`] reproduces that mapping
//!   exactly (including the empty string for [`ReturnCode::Ok`]).

use core::fmt;

/// The complete set of zlib return codes, mirroring the C `Z_*` integer
/// constants defined in `zlib.h`.
///
/// The `#[repr(i32)]` discriminants are a **hard compatibility requirement**:
/// they are the exact integers returned across the C ABI, so they must never
/// diverge from `zlib.h`. Negative values denote errors, `0` denotes success,
/// and the positive values denote special-but-normal events.
///
/// Use [`ReturnCode::as_c_int`] to obtain the raw integer for the FFI boundary,
/// and [`ReturnCode::from_c_int`] (or the [`TryFrom<i32>`](ReturnCode::try_from)
/// implementation) to normalize an integer received from C back into a
/// `ReturnCode`.
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReturnCode {
    /// `Z_OK` (`0`): the operation completed successfully.
    Ok = 0,
    /// `Z_STREAM_END` (`1`): the end of the compressed stream was reached.
    StreamEnd = 1,
    /// `Z_NEED_DICT` (`2`): a preset dictionary is required to continue
    /// decompression.
    NeedDict = 2,
    /// `Z_ERRNO` (`-1`): an underlying file-system / `errno` error occurred.
    ErrNo = -1,
    /// `Z_STREAM_ERROR` (`-2`): the stream state was inconsistent or the
    /// supplied parameters were invalid.
    StreamError = -2,
    /// `Z_DATA_ERROR` (`-3`): the input data was corrupted or incomplete.
    DataError = -3,
    /// `Z_MEM_ERROR` (`-4`): there was not enough memory to perform the
    /// operation.
    MemError = -4,
    /// `Z_BUF_ERROR` (`-5`): no progress was possible; more output space or more
    /// input is required.
    BufError = -5,
    /// `Z_VERSION_ERROR` (`-6`): the caller's zlib version is incompatible with
    /// the library version.
    VersionError = -6,
}

impl ReturnCode {
    /// Returns the raw C integer (the `#[repr(i32)]` discriminant) for this
    /// return code.
    ///
    /// This is the value emitted verbatim at the FFI boundary, so it is
    /// guaranteed to equal the matching `Z_*` constant from `zlib.h`.
    #[inline]
    #[must_use]
    pub const fn as_c_int(self) -> i32 {
        self as i32
    }

    /// Converts a raw C integer into a [`ReturnCode`], returning [`None`] if the
    /// value is not one of the nine defined `Z_*` codes.
    ///
    /// This is the inverse of [`ReturnCode::as_c_int`] and is used by the `ffi`
    /// layer to normalize an integer received from C.
    #[inline]
    #[must_use]
    pub const fn from_c_int(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::StreamEnd),
            2 => Some(Self::NeedDict),
            -1 => Some(Self::ErrNo),
            -2 => Some(Self::StreamError),
            -3 => Some(Self::DataError),
            -4 => Some(Self::MemError),
            -5 => Some(Self::BufError),
            -6 => Some(Self::VersionError),
            _ => None,
        }
    }

    /// Returns the human-readable message associated with this return code.
    ///
    /// The strings are byte-identical to the C `z_errmsg` table addressed via
    /// the `ERR_MSG` macro (and therefore identical to the value returned by the
    /// C `zError` function). [`ReturnCode::Ok`] maps to the empty string,
    /// exactly as in the C table.
    #[inline]
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NeedDict => "need dictionary",
            Self::StreamEnd => "stream end",
            Self::Ok => "",
            Self::ErrNo => "file error",
            Self::StreamError => "stream error",
            Self::DataError => "data error",
            Self::MemError => "insufficient memory",
            Self::BufError => "buffer error",
            Self::VersionError => "incompatible version",
        }
    }

    /// Returns `true` if this code represents an error (a negative `Z_*` value).
    ///
    /// Note that [`ReturnCode::Ok`], [`ReturnCode::StreamEnd`], and
    /// [`ReturnCode::NeedDict`] are **not** errors — they are success or
    /// special-but-normal events.
    #[inline]
    #[must_use]
    pub const fn is_error(self) -> bool {
        (self as i32) < 0
    }

    /// Returns `true` if this code is [`ReturnCode::Ok`] (`Z_OK`).
    #[inline]
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

impl From<ReturnCode> for i32 {
    /// Converts a [`ReturnCode`] into its raw C integer via
    /// [`ReturnCode::as_c_int`].
    #[inline]
    fn from(code: ReturnCode) -> Self {
        code.as_c_int()
    }
}

impl TryFrom<i32> for ReturnCode {
    type Error = InvalidReturnCode;

    /// Converts a raw C integer into a [`ReturnCode`], failing with
    /// [`InvalidReturnCode`] if the value is not one of the nine defined `Z_*`
    /// codes. This is the fallible, trait-based counterpart to
    /// [`ReturnCode::from_c_int`].
    #[inline]
    fn try_from(value: i32) -> core::result::Result<Self, Self::Error> {
        match Self::from_c_int(value) {
            Some(code) => Ok(code),
            None => Err(InvalidReturnCode(value)),
        }
    }
}

/// Error returned by the [`TryFrom<i32>`](ReturnCode::try_from) implementation
/// for [`ReturnCode`] when the integer does not correspond to any defined zlib
/// return code.
///
/// The wrapped [`i32`] is the offending value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InvalidReturnCode(pub i32);

impl fmt::Display for InvalidReturnCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown zlib return code: {}", self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for InvalidReturnCode {}

/// The idiomatic error type for the crate, covering every non-success zlib
/// return code.
///
/// Internal engine code returns [`Result`] (an alias for `Result<T, ZlibError>`);
/// the corresponding integer `Z_*` code is recovered with
/// [`ZlibError::as_return_code`] (or the [`From<ZlibError>`] implementations for
/// [`ReturnCode`] and [`i32`]) only when crossing the FFI boundary back into C.
///
/// Every variant corresponds one-to-one with a non-[`Ok`](ReturnCode::Ok)
/// [`ReturnCode`], so the mapping between the two types is total over the
/// non-success set. `Z_STREAM_END` and `Z_NEED_DICT` are *normal* positive
/// events in zlib rather than failures; they are modelled here so that any
/// non-`Ok` code can be represented as a `ZlibError` when an internal API
/// chooses to surface it through an `Err`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ZlibError {
    /// Mirrors [`ReturnCode::StreamEnd`] (`Z_STREAM_END`, `1`).
    StreamEnd,
    /// Mirrors [`ReturnCode::NeedDict`] (`Z_NEED_DICT`, `2`).
    NeedDict,
    /// Mirrors [`ReturnCode::ErrNo`] (`Z_ERRNO`, `-1`).
    ErrNo,
    /// Mirrors [`ReturnCode::StreamError`] (`Z_STREAM_ERROR`, `-2`).
    StreamError,
    /// Mirrors [`ReturnCode::DataError`] (`Z_DATA_ERROR`, `-3`).
    DataError,
    /// Mirrors [`ReturnCode::MemError`] (`Z_MEM_ERROR`, `-4`).
    MemError,
    /// Mirrors [`ReturnCode::BufError`] (`Z_BUF_ERROR`, `-5`).
    BufError,
    /// Mirrors [`ReturnCode::VersionError`] (`Z_VERSION_ERROR`, `-6`).
    VersionError,
}

impl ZlibError {
    /// Returns the [`ReturnCode`] that corresponds to this error.
    ///
    /// This is a total mapping: every [`ZlibError`] variant has exactly one
    /// matching non-[`Ok`](ReturnCode::Ok) return code.
    #[inline]
    #[must_use]
    pub const fn as_return_code(self) -> ReturnCode {
        match self {
            Self::StreamEnd => ReturnCode::StreamEnd,
            Self::NeedDict => ReturnCode::NeedDict,
            Self::ErrNo => ReturnCode::ErrNo,
            Self::StreamError => ReturnCode::StreamError,
            Self::DataError => ReturnCode::DataError,
            Self::MemError => ReturnCode::MemError,
            Self::BufError => ReturnCode::BufError,
            Self::VersionError => ReturnCode::VersionError,
        }
    }

    /// Returns the human-readable message for this error, identical to the C
    /// `z_errmsg` string for the corresponding [`ReturnCode`].
    #[inline]
    #[must_use]
    pub const fn message(self) -> &'static str {
        self.as_return_code().message()
    }

    /// Converts a [`ReturnCode`] into the matching [`ZlibError`], returning
    /// [`None`] for [`ReturnCode::Ok`] because success is not an error.
    ///
    /// This is the inverse of [`ZlibError::as_return_code`] over the non-`Ok`
    /// set.
    #[inline]
    #[must_use]
    pub const fn from_return_code(code: ReturnCode) -> Option<Self> {
        match code {
            ReturnCode::Ok => None,
            ReturnCode::StreamEnd => Some(Self::StreamEnd),
            ReturnCode::NeedDict => Some(Self::NeedDict),
            ReturnCode::ErrNo => Some(Self::ErrNo),
            ReturnCode::StreamError => Some(Self::StreamError),
            ReturnCode::DataError => Some(Self::DataError),
            ReturnCode::MemError => Some(Self::MemError),
            ReturnCode::BufError => Some(Self::BufError),
            ReturnCode::VersionError => Some(Self::VersionError),
        }
    }
}

impl fmt::Display for ZlibError {
    /// Writes the C `z_errmsg` string for this error.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl From<ZlibError> for ReturnCode {
    /// Converts a [`ZlibError`] into its matching [`ReturnCode`] via
    /// [`ZlibError::as_return_code`].
    #[inline]
    fn from(error: ZlibError) -> Self {
        error.as_return_code()
    }
}

impl From<ZlibError> for i32 {
    /// Converts a [`ZlibError`] directly into the raw C integer return code —
    /// the value surfaced at the FFI boundary.
    #[inline]
    fn from(error: ZlibError) -> Self {
        error.as_return_code().as_c_int()
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ZlibError {}

/// The crate-internal result type: `Result<T, ZlibError>`.
///
/// Internal engines typically return `Result<ReturnCode>` or `Result<()>`; the
/// concrete integer `Z_*` code is recovered at the FFI boundary via the
/// [`From<ZlibError>`] implementations for [`ReturnCode`] and [`i32`].
pub type Result<T> = core::result::Result<T, ZlibError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Single source of truth: every return code with its canonical C integer
    /// (from `zlib.h`) and its `z_errmsg` string (from `zutil.c`).
    const ALL: &[(ReturnCode, i32, &str)] = &[
        (ReturnCode::Ok, 0, ""),
        (ReturnCode::StreamEnd, 1, "stream end"),
        (ReturnCode::NeedDict, 2, "need dictionary"),
        (ReturnCode::ErrNo, -1, "file error"),
        (ReturnCode::StreamError, -2, "stream error"),
        (ReturnCode::DataError, -3, "data error"),
        (ReturnCode::MemError, -4, "insufficient memory"),
        (ReturnCode::BufError, -5, "buffer error"),
        (ReturnCode::VersionError, -6, "incompatible version"),
    ];

    /// Every non-success error with its return code and negative/positive C
    /// integer.
    const ERRORS: &[(ZlibError, ReturnCode, i32)] = &[
        (ZlibError::StreamEnd, ReturnCode::StreamEnd, 1),
        (ZlibError::NeedDict, ReturnCode::NeedDict, 2),
        (ZlibError::ErrNo, ReturnCode::ErrNo, -1),
        (ZlibError::StreamError, ReturnCode::StreamError, -2),
        (ZlibError::DataError, ReturnCode::DataError, -3),
        (ZlibError::MemError, ReturnCode::MemError, -4),
        (ZlibError::BufError, ReturnCode::BufError, -5),
        (ZlibError::VersionError, ReturnCode::VersionError, -6),
    ];

    #[test]
    fn discriminants_match_c_abi() {
        // The #[repr(i32)] discriminants ARE the C ABI return values and must be
        // byte-exact with zlib.h.
        assert_eq!(ReturnCode::Ok as i32, 0);
        assert_eq!(ReturnCode::StreamEnd as i32, 1);
        assert_eq!(ReturnCode::NeedDict as i32, 2);
        assert_eq!(ReturnCode::ErrNo as i32, -1);
        assert_eq!(ReturnCode::StreamError as i32, -2);
        assert_eq!(ReturnCode::DataError as i32, -3);
        assert_eq!(ReturnCode::MemError as i32, -4);
        assert_eq!(ReturnCode::BufError as i32, -5);
        assert_eq!(ReturnCode::VersionError as i32, -6);
    }

    #[test]
    fn as_c_int_matches_table() {
        for &(code, int, _) in ALL {
            assert_eq!(code.as_c_int(), int);
            assert_eq!(i32::from(code), int);
        }
    }

    #[test]
    fn from_c_int_round_trips_all_variants() {
        for &(code, int, _) in ALL {
            assert_eq!(ReturnCode::from_c_int(int), Some(code));
            // as_c_int / from_c_int are exact inverses.
            assert_eq!(ReturnCode::from_c_int(code.as_c_int()), Some(code));
            assert_eq!(ReturnCode::try_from(int), Ok(code));
        }
    }

    #[test]
    fn from_c_int_rejects_unknown() {
        for bad in [3, 7, -7, -100, 100, i32::MIN, i32::MAX] {
            assert_eq!(ReturnCode::from_c_int(bad), None);
            assert_eq!(ReturnCode::try_from(bad), Err(InvalidReturnCode(bad)));
        }
    }

    #[test]
    fn message_matches_z_errmsg() {
        for &(code, _, msg) in ALL {
            assert_eq!(code.message(), msg);
        }
    }

    #[test]
    fn is_error_and_is_ok() {
        for &(code, int, _) in ALL {
            assert_eq!(code.is_error(), int < 0);
            assert_eq!(code.is_ok(), int == 0);
        }
    }

    #[test]
    fn zlib_error_maps_to_return_code_and_int() {
        for &(err, code, int) in ERRORS {
            assert_eq!(err.as_return_code(), code);
            assert_eq!(ReturnCode::from(err), code);
            assert_eq!(i32::from(err), int);
            assert_eq!(err.message(), code.message());
        }
    }

    #[test]
    fn from_return_code_covers_non_ok_set() {
        // Ok is success, never an error.
        assert_eq!(ZlibError::from_return_code(ReturnCode::Ok), None);
        // Every other code round-trips through ZlibError.
        for &(code, _, _) in ALL {
            match ZlibError::from_return_code(code) {
                None => assert_eq!(code, ReturnCode::Ok),
                Some(err) => assert_eq!(err.as_return_code(), code),
            }
        }
    }

    #[test]
    fn display_matches_message() {
        for &(err, _, _) in ERRORS {
            assert_eq!(format!("{err}"), err.message());
            assert_eq!(err.to_string(), err.message());
        }
    }

    #[test]
    fn invalid_return_code_display() {
        assert_eq!(
            format!("{}", InvalidReturnCode(42)),
            "unknown zlib return code: 42"
        );
    }

    #[test]
    fn result_alias_is_usable() {
        fn returns_ok() -> Result<u8> {
            Ok(7)
        }
        fn returns_err() -> Result<u8> {
            Err(ZlibError::DataError)
        }
        assert_eq!(returns_ok(), Ok(7));
        assert_eq!(returns_err(), Err(ZlibError::DataError));
    }
}
