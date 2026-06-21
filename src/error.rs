//! Idiomatic error and return-code types for the `zlib-rs` crate.
//!
//! This module is the bridge between the crate's idiomatic, type-safe Rust API
//! and the **frozen** integer status codes of the C `zlib` ABI. C `zlib`
//! reports the outcome of every operation through a single `int` return value
//! (`Z_OK`, `Z_STREAM_END`, `Z_DATA_ERROR`, …); the safe Rust core instead
//! splits that one integer into two complementary, exhaustive enumerations:
//!
//! * [`ReturnCode`] — the **non-error** outcomes: success ([`ReturnCode::Ok`]),
//!   end-of-stream ([`ReturnCode::StreamEnd`]), and the "a preset dictionary is
//!   required" handshake ([`ReturnCode::NeedDict`]). These are the values a
//!   successful call can legitimately return, so they live on the `Ok` side of
//!   a [`Result`].
//! * [`ZlibError`] — the six **failure** outcomes (`Z_ERRNO` … `Z_VERSION_ERROR`).
//!   These live on the `Err` side of a [`Result`].
//!
//! The idiomatic API therefore returns [`Result<ReturnCode>`](Result) (i.e.
//! `core::result::Result<ReturnCode, ZlibError>`), and the FFI shim
//! (`crate::ffi`) converts that back into the exact C `int` code via
//! [`result_to_code`]. The inverse, [`code_to_result`], lets internal helpers
//! that still speak in raw codes be lifted into the idiomatic world.
//!
//! # Bit-exact C ABI
//!
//! Every discriminant here is **frozen** to the canonical C value (the numeric
//! `Z_*` constants in [`crate::constants`], which in turn mirror `zlib.h`
//! lines 181-189). The FFI return values must be bit-identical to C `zlib`
//! (AAP §0.7.1), so:
//!
//! ```
//! use zlib_rs::error::{ReturnCode, ZlibError};
//!
//! assert_eq!(ReturnCode::Ok as i32, 0);
//! assert_eq!(ReturnCode::StreamEnd as i32, 1);
//! assert_eq!(ReturnCode::NeedDict as i32, 2);
//!
//! assert_eq!(ZlibError::Errno as i32, -1);
//! assert_eq!(ZlibError::DataError as i32, -3);
//! assert_eq!(ZlibError::VersionError as i32, -6);
//! ```
//!
//! # Error messages
//!
//! C `zlib` exposes a 10-entry `z_errmsg` table (`zutil.c`) indexed by
//! `2 - err`, surfaced to callers through `ERR_MSG(err)` (`zutil.h`) and the
//! public `zError()` function (`zutil.c`). [`err_msg`] is the **single
//! canonical** Rust reproduction of that lookup; `crate::util::version`'s
//! `zError` and the FFI layer delegate to it so the strings can never diverge.
//!
//! ```
//! use zlib_rs::error::err_msg;
//!
//! assert_eq!(err_msg(2), "need dictionary");   // Z_NEED_DICT
//! assert_eq!(err_msg(1), "stream end");        // Z_STREAM_END
//! assert_eq!(err_msg(0), "");                  // Z_OK
//! assert_eq!(err_msg(-3), "data error");       // Z_DATA_ERROR
//! assert_eq!(err_msg(-100), "");               // out of range
//! ```
//!
//! # End-to-end round-trip
//!
//! ```
//! use zlib_rs::error::{code_to_result, result_to_code, ReturnCode, ZlibError};
//!
//! // A raw C code becomes an idiomatic `Result` …
//! assert_eq!(code_to_result(0), Ok(ReturnCode::Ok));
//! assert_eq!(code_to_result(-4), Err(ZlibError::MemError));
//!
//! // … and an idiomatic `Result` becomes the raw C code again.
//! assert_eq!(result_to_code(Ok(ReturnCode::StreamEnd)), 1);
//! assert_eq!(result_to_code(Err(ZlibError::MemError)), -4);
//! ```
//!
//! # `no_std`
//!
//! This module is `no_std`-clean: the default path imports only [`core`] (it
//! uses [`core::fmt`] for [`Display`](core::fmt::Display), never `std::fmt`).
//! The [`std::error::Error`] implementation for [`ZlibError`] is the **only**
//! `std`-dependent item and is gated behind the crate's `std` feature, so the
//! module compiles unchanged under the `no-std` feature (the C `Z_SOLO`
//! analogue).

use crate::constants::{
    UnknownValue, Z_BUF_ERROR, Z_DATA_ERROR, Z_ERRNO, Z_MEM_ERROR, Z_NEED_DICT, Z_OK, Z_STREAM_END,
    Z_STREAM_ERROR, Z_VERSION_ERROR,
};

// ===========================================================================
// ReturnCode — the non-error ("ok" / informational) outcomes.
// ===========================================================================

/// The non-error outcomes a `zlib` operation can report.
///
/// These are the values that appear on the `Ok` side of the crate's
/// [`Result`]; the failure outcomes are modelled separately by [`ZlibError`].
/// Each discriminant equals the canonical C constant exactly
/// (`#[repr(i32)]`), so `ReturnCode::X as i32` yields the value C `zlib`
/// would return:
///
/// | Variant                | C constant      | Value |
/// |------------------------|-----------------|-------|
/// | [`ReturnCode::Ok`]      | `Z_OK`          | `0`   |
/// | [`ReturnCode::StreamEnd`] | `Z_STREAM_END`  | `1`   |
/// | [`ReturnCode::NeedDict`] | `Z_NEED_DICT`   | `2`   |
///
/// Note that `Z_NEED_DICT` is **not** an error: the inflate path returns
/// `Ok(ReturnCode::NeedDict)` to signal that a preset dictionary must be
/// supplied, and the FFI layer converts it to the C value `2`.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ReturnCode;
///
/// assert_eq!(ReturnCode::StreamEnd as i32, 1);
/// assert_eq!(ReturnCode::from_c_int(2), Some(ReturnCode::NeedDict));
/// assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ReturnCode {
    /// Operation completed successfully (`Z_OK`, `0`).
    Ok = Z_OK,
    /// The end of the compressed stream was reached (`Z_STREAM_END`, `1`).
    StreamEnd = Z_STREAM_END,
    /// A preset dictionary is required to continue decompression
    /// (`Z_NEED_DICT`, `2`). Signalled by the `DICTID` handshake in the zlib
    /// header; the caller must supply the dictionary before calling `inflate`
    /// again.
    NeedDict = Z_NEED_DICT,
}

impl ReturnCode {
    /// Return the C `int` value of this code (its `#[repr(i32)]` discriminant).
    ///
    /// Equivalent to `self as i32`, but named for clarity at FFI call sites.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::Ok.as_c_int(), 0);
    /// assert_eq!(ReturnCode::StreamEnd.as_c_int(), 1);
    /// assert_eq!(ReturnCode::NeedDict.as_c_int(), 2);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_c_int(self) -> i32 {
        self as i32
    }

    /// Parse a raw C `int` code into a [`ReturnCode`].
    ///
    /// Returns `Some` for the three non-error codes (`0`, `1`, `2`) and `None`
    /// for everything else — including the error codes `-1..=-6`, which belong
    /// to [`ZlibError`] (use [`ZlibError::from_c_int`] or [`code_to_result`]
    /// for those).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::from_c_int(0), Some(ReturnCode::Ok));
    /// assert_eq!(ReturnCode::from_c_int(2), Some(ReturnCode::NeedDict));
    /// assert_eq!(ReturnCode::from_c_int(-3), None); // an error code
    /// assert_eq!(ReturnCode::from_c_int(42), None); // out of range
    /// ```
    #[inline]
    pub fn from_c_int(v: i32) -> Option<Self> {
        match v {
            Z_OK => Some(Self::Ok),
            Z_STREAM_END => Some(Self::StreamEnd),
            Z_NEED_DICT => Some(Self::NeedDict),
            _ => None,
        }
    }

    /// Return the human-readable message for this code.
    ///
    /// The strings mirror the corresponding entries of the C `z_errmsg` table
    /// exactly: [`ReturnCode::Ok`] maps to the empty string (`zlib` reports no
    /// message for success), [`ReturnCode::StreamEnd`] to `"stream end"`, and
    /// [`ReturnCode::NeedDict`] to `"need dictionary"`. See [`err_msg`] for the
    /// numeric-code entry point that the FFI / `zError` layer uses.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ReturnCode;
    ///
    /// assert_eq!(ReturnCode::Ok.message(), "");
    /// assert_eq!(ReturnCode::StreamEnd.message(), "stream end");
    /// assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
    /// ```
    #[inline]
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Ok => "",
            Self::StreamEnd => "stream end",
            Self::NeedDict => "need dictionary",
        }
    }
}

impl From<ReturnCode> for i32 {
    /// Widen a [`ReturnCode`] to its raw C `int` value (its discriminant).
    #[inline]
    fn from(value: ReturnCode) -> Self {
        value as Self
    }
}

impl TryFrom<i32> for ReturnCode {
    type Error = UnknownValue;

    /// Parse a raw C `int` into a [`ReturnCode`], mirroring
    /// [`ReturnCode::from_c_int`] but reporting the offending value through
    /// [`UnknownValue`] (consistent with the idiomatic enums in
    /// [`crate::constants`]). Error codes `-1..=-6` are *not* return codes and
    /// therefore fail here.
    #[inline]
    fn try_from(value: i32) -> core::result::Result<Self, Self::Error> {
        Self::from_c_int(value).ok_or(UnknownValue::new(value))
    }
}

// ===========================================================================
// ZlibError — the failure outcomes.
// ===========================================================================

/// The failure outcomes a `zlib` operation can report.
///
/// These are the six negative C return codes; they appear on the `Err` side of
/// the crate's [`Result`]. Each discriminant equals the canonical C constant
/// exactly (`#[repr(i32)]`), so `ZlibError::X as i32` yields the value C
/// `zlib` would return:
///
/// | Variant                  | C constant        | Value |
/// |--------------------------|-------------------|-------|
/// | [`ZlibError::Errno`]        | `Z_ERRNO`         | `-1`  |
/// | [`ZlibError::StreamError`]  | `Z_STREAM_ERROR`  | `-2`  |
/// | [`ZlibError::DataError`]    | `Z_DATA_ERROR`    | `-3`  |
/// | [`ZlibError::MemError`]     | `Z_MEM_ERROR`     | `-4`  |
/// | [`ZlibError::BufError`]     | `Z_BUF_ERROR`     | `-5`  |
/// | [`ZlibError::VersionError`] | `Z_VERSION_ERROR` | `-6`  |
///
/// [`ZlibError`] implements [`Display`](core::fmt::Display) (writing
/// [`message`](ZlibError::message)) on every target, and
/// [`std::error::Error`] when the crate's `std` feature is enabled.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ZlibError;
///
/// assert_eq!(ZlibError::DataError as i32, -3);
/// assert_eq!(ZlibError::from_c_int(-4), Some(ZlibError::MemError));
/// assert_eq!(ZlibError::MemError.message(), "insufficient memory");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ZlibError {
    /// A file/system error occurred (`Z_ERRNO`, `-1`); consult the platform
    /// `errno` for details.
    Errno = Z_ERRNO,
    /// The stream state was inconsistent (`Z_STREAM_ERROR`, `-2`) — e.g. an
    /// uninitialized stream, an invalid parameter, or a misordered call.
    StreamError = Z_STREAM_ERROR,
    /// The input data was corrupted or did not conform to the expected format
    /// (`Z_DATA_ERROR`, `-3`).
    DataError = Z_DATA_ERROR,
    /// Memory could not be allocated for the operation (`Z_MEM_ERROR`, `-4`).
    MemError = Z_MEM_ERROR,
    /// No progress was possible (`Z_BUF_ERROR`, `-5`): the output buffer or
    /// available input was exhausted without completing the operation. The
    /// caller should supply more space/input and retry.
    BufError = Z_BUF_ERROR,
    /// The `zlib` version supplied to an init function is incompatible with the
    /// runtime library version (`Z_VERSION_ERROR`, `-6`).
    VersionError = Z_VERSION_ERROR,
}

impl ZlibError {
    /// Return the C `int` value of this error (its `#[repr(i32)]`
    /// discriminant, always negative).
    ///
    /// Equivalent to `self as i32`, but named for clarity at FFI call sites.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    ///
    /// assert_eq!(ZlibError::DataError.as_c_int(), -3);
    /// assert_eq!(ZlibError::VersionError.as_c_int(), -6);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_c_int(self) -> i32 {
        self as i32
    }

    /// Parse a raw C `int` code into a [`ZlibError`].
    ///
    /// Returns `Some` for the six error codes (`-1..=-6`) and `None` for every
    /// non-error code — including `0`, `1`, and `2`, which belong to
    /// [`ReturnCode`] (use [`ReturnCode::from_c_int`] or [`code_to_result`] for
    /// those).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    ///
    /// assert_eq!(ZlibError::from_c_int(-1), Some(ZlibError::Errno));
    /// assert_eq!(ZlibError::from_c_int(-6), Some(ZlibError::VersionError));
    /// assert_eq!(ZlibError::from_c_int(0), None);   // a success code
    /// assert_eq!(ZlibError::from_c_int(-7), None);  // out of range
    /// ```
    #[inline]
    pub fn from_c_int(v: i32) -> Option<Self> {
        match v {
            Z_ERRNO => Some(Self::Errno),
            Z_STREAM_ERROR => Some(Self::StreamError),
            Z_DATA_ERROR => Some(Self::DataError),
            Z_MEM_ERROR => Some(Self::MemError),
            Z_BUF_ERROR => Some(Self::BufError),
            Z_VERSION_ERROR => Some(Self::VersionError),
            _ => None,
        }
    }

    /// Return the human-readable message for this error.
    ///
    /// The strings mirror the corresponding entries of the C `z_errmsg` table
    /// (`zutil.c`) **exactly**, so error reporting is bit-for-bit identical to
    /// C `zlib`. See [`err_msg`] for the numeric-code entry point that the FFI
    /// / `zError` layer uses.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    ///
    /// assert_eq!(ZlibError::Errno.message(), "file error");
    /// assert_eq!(ZlibError::StreamError.message(), "stream error");
    /// assert_eq!(ZlibError::DataError.message(), "data error");
    /// assert_eq!(ZlibError::MemError.message(), "insufficient memory");
    /// assert_eq!(ZlibError::BufError.message(), "buffer error");
    /// assert_eq!(ZlibError::VersionError.message(), "incompatible version");
    /// ```
    #[inline]
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Errno => "file error",
            Self::StreamError => "stream error",
            Self::DataError => "data error",
            Self::MemError => "insufficient memory",
            Self::BufError => "buffer error",
            Self::VersionError => "incompatible version",
        }
    }
}

impl core::fmt::Display for ZlibError {
    /// Write the error's [`message`](ZlibError::message). The output is exactly
    /// the C `z_errmsg` string for this error code (never empty, since all six
    /// error codes have non-empty messages).
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message())
    }
}

// `std::error::Error` is the ONLY `std`-dependent item in this module; it is
// gated behind the `std` feature so the module stays `no_std`-clean under the
// `no-std` build (the C `Z_SOLO` analogue). The blanket impl uses the default
// trait methods — `ZlibError` is a leaf error with no `source`.
#[cfg(feature = "std")]
impl std::error::Error for ZlibError {}

impl From<ZlibError> for i32 {
    /// Widen a [`ZlibError`] to its raw C `int` value (its discriminant).
    #[inline]
    fn from(value: ZlibError) -> Self {
        value as Self
    }
}

impl TryFrom<i32> for ZlibError {
    type Error = UnknownValue;

    /// Parse a raw C `int` into a [`ZlibError`], mirroring
    /// [`ZlibError::from_c_int`] but reporting the offending value through
    /// [`UnknownValue`] (consistent with the idiomatic enums in
    /// [`crate::constants`]). Non-error codes (`0`, `1`, `2`, and any value
    /// outside `-6..=-1`) fail here.
    #[inline]
    fn try_from(value: i32) -> core::result::Result<Self, Self::Error> {
        Self::from_c_int(value).ok_or(UnknownValue::new(value))
    }
}

// ===========================================================================
// `z_errmsg` reproduction — the single canonical error-string lookup.
// ===========================================================================

/// The C `z_errmsg` message table (`zutil.c`), reproduced verbatim.
///
/// The table has ten entries and is indexed by `2 - err` (see [`err_msg`]).
/// The entry order — descending by return code from `Z_NEED_DICT` (`2`) down
/// to `Z_VERSION_ERROR` (`-6`), bracketed by two empty sentinels — is part of
/// the C ABI and must not be reordered.
const Z_ERRMSG: [&str; 10] = [
    "need dictionary",      // index 0  — Z_NEED_DICT       ( 2)
    "stream end",           // index 1  — Z_STREAM_END      ( 1)
    "",                     // index 2  — Z_OK              ( 0)
    "file error",           // index 3  — Z_ERRNO           (-1)
    "stream error",         // index 4  — Z_STREAM_ERROR    (-2)
    "data error",           // index 5  — Z_DATA_ERROR      (-3)
    "insufficient memory",  // index 6  — Z_MEM_ERROR       (-4)
    "buffer error",         // index 7  — Z_BUF_ERROR       (-5)
    "incompatible version", // index 8  — Z_VERSION_ERROR   (-6)
    "",                     // index 9  — out-of-range sentinel
];

/// Look up the human-readable message for a raw C return code.
///
/// This is the **single canonical** Rust reproduction of the C
/// `ERR_MSG(err)` macro (`zutil.h`) over the `z_errmsg` table (`zutil.c`):
///
/// ```text
/// z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// In-range codes `-6..=2` map to their specific message; every out-of-range
/// code maps to the empty string (the index-`9` sentinel). The crate's public
/// `zError` (in `crate::util::version`) and the FFI layer delegate here, so
/// there is exactly one definition of these strings and they can never
/// diverge from C `zlib`.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::err_msg;
///
/// assert_eq!(err_msg(2), "need dictionary");      // Z_NEED_DICT
/// assert_eq!(err_msg(1), "stream end");           // Z_STREAM_END
/// assert_eq!(err_msg(0), "");                     // Z_OK
/// assert_eq!(err_msg(-1), "file error");          // Z_ERRNO
/// assert_eq!(err_msg(-3), "data error");          // Z_DATA_ERROR
/// assert_eq!(err_msg(-6), "incompatible version"); // Z_VERSION_ERROR
/// assert_eq!(err_msg(3), "");                     // out of range (> 2)
/// assert_eq!(err_msg(-7), "");                    // out of range (< -6)
/// assert_eq!(err_msg(-100), "");                  // out of range
/// ```
#[inline]
#[must_use]
pub const fn err_msg(code: i32) -> &'static str {
    // Mirror `ERR_MSG`: clamp out-of-range codes to the empty sentinel at
    // index 9, otherwise index by `2 - code`.
    let index = if code < -6 || code > 2 {
        9
    } else {
        (2 - code) as usize
    };
    Z_ERRMSG[index]
}

// ===========================================================================
// `Result` alias and code <-> Result conversions.
// ===========================================================================

/// The crate-wide result type: a [`core::result::Result`] whose error is always
/// a [`ZlibError`].
///
/// The success type `T` is usually [`ReturnCode`] (the idiomatic API surface),
/// but the alias is generic so helpers can return other success payloads (for
/// example a byte count) while still funnelling failures through [`ZlibError`].
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{Result, ReturnCode, ZlibError};
///
/// fn finish() -> Result<ReturnCode> {
///     Ok(ReturnCode::StreamEnd)
/// }
///
/// assert_eq!(finish(), Ok(ReturnCode::StreamEnd));
///
/// let oops: Result<ReturnCode> = Err(ZlibError::BufError);
/// assert!(oops.is_err());
/// ```
pub type Result<T> = core::result::Result<T, ZlibError>;

/// Convert a raw C `int` return code into an idiomatic [`Result`].
///
/// The non-error codes `0`, `1`, `2` map to `Ok(ReturnCode::…)`; the error
/// codes `-1..=-6` map to `Err(ZlibError::…)`. Any value outside the canonical
/// `-6..=2` range cannot be produced by a correct `zlib` operation; it is
/// treated as an inconsistent state and mapped to `Err(ZlibError::StreamError)`
/// (the same `Z_STREAM_ERROR` C reports for a corrupt/invalid stream state).
///
/// This is the inverse of [`result_to_code`] over the valid code range:
/// `result_to_code(code_to_result(c)) == c` for every `c` in `-6..=2`.
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
/// assert_eq!(code_to_result(-4), Err(ZlibError::MemError));
///
/// // Out-of-range codes collapse to a stream error.
/// assert_eq!(code_to_result(99), Err(ZlibError::StreamError));
/// ```
#[inline]
pub fn code_to_result(code: i32) -> Result<ReturnCode> {
    if let Some(rc) = ReturnCode::from_c_int(code) {
        Ok(rc)
    } else if let Some(err) = ZlibError::from_c_int(code) {
        Err(err)
    } else {
        // Unexpected/out-of-range code: report an inconsistent stream state.
        Err(ZlibError::StreamError)
    }
}

/// Convert an idiomatic [`Result`] back into the raw C `int` return code.
///
/// This is the conversion the FFI shim (`crate::ffi`) uses to produce the C
/// return value: `Ok(rc)` becomes `rc.as_c_int()` and `Err(e)` becomes
/// `e.as_c_int()`. It is the inverse of [`code_to_result`].
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{result_to_code, ReturnCode, ZlibError};
///
/// assert_eq!(result_to_code(Ok(ReturnCode::Ok)), 0);
/// assert_eq!(result_to_code(Ok(ReturnCode::StreamEnd)), 1);
/// assert_eq!(result_to_code(Ok(ReturnCode::NeedDict)), 2);
/// assert_eq!(result_to_code(Err(ZlibError::MemError)), -4);
/// assert_eq!(result_to_code(Err(ZlibError::VersionError)), -6);
/// ```
#[inline]
#[must_use]
pub fn result_to_code(r: Result<ReturnCode>) -> i32 {
    match r {
        Ok(rc) => rc.as_c_int(),
        Err(err) => err.as_c_int(),
    }
}

// ===========================================================================
// Tests — assert every value/string matches canonical C zlib (frozen ABI).
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`ReturnCode`] variant, for exhaustive iteration in tests.
    const ALL_RETURN_CODES: [ReturnCode; 3] =
        [ReturnCode::Ok, ReturnCode::StreamEnd, ReturnCode::NeedDict];

    /// Every [`ZlibError`] variant, for exhaustive iteration in tests.
    const ALL_ERRORS: [ZlibError; 6] = [
        ZlibError::Errno,
        ZlibError::StreamError,
        ZlibError::DataError,
        ZlibError::MemError,
        ZlibError::BufError,
        ZlibError::VersionError,
    ];

    // -- Discriminants are frozen to the canonical C return codes -----------

    #[test]
    fn return_code_discriminants_match_c() {
        assert_eq!(ReturnCode::Ok as i32, 0);
        assert_eq!(ReturnCode::StreamEnd as i32, 1);
        assert_eq!(ReturnCode::NeedDict as i32, 2);
    }

    #[test]
    fn zlib_error_discriminants_match_c() {
        assert_eq!(ZlibError::Errno as i32, -1);
        assert_eq!(ZlibError::StreamError as i32, -2);
        assert_eq!(ZlibError::DataError as i32, -3);
        assert_eq!(ZlibError::MemError as i32, -4);
        assert_eq!(ZlibError::BufError as i32, -5);
        assert_eq!(ZlibError::VersionError as i32, -6);
    }

    #[test]
    fn as_c_int_equals_cast() {
        for rc in ALL_RETURN_CODES {
            assert_eq!(rc.as_c_int(), rc as i32);
        }
        for err in ALL_ERRORS {
            assert_eq!(err.as_c_int(), err as i32);
        }
        // Spot-check the exact values called out by the spec.
        assert_eq!(ReturnCode::StreamEnd.as_c_int(), 1);
        assert_eq!(ZlibError::DataError.as_c_int(), -3);
    }

    // -- `from_c_int` parsing and round-trips -------------------------------

    #[test]
    fn return_code_from_c_int_round_trips() {
        for rc in ALL_RETURN_CODES {
            assert_eq!(ReturnCode::from_c_int(rc.as_c_int()), Some(rc));
        }
    }

    #[test]
    fn zlib_error_from_c_int_round_trips() {
        for err in ALL_ERRORS {
            assert_eq!(ZlibError::from_c_int(err.as_c_int()), Some(err));
        }
    }

    #[test]
    fn return_code_from_c_int_rejects_non_return_codes() {
        // Error codes belong to `ZlibError`, not `ReturnCode`.
        for code in [-1, -2, -3, -4, -5, -6] {
            assert_eq!(ReturnCode::from_c_int(code), None);
        }
        // Out-of-range values.
        for code in [3, 42, -7, -100, i32::MAX, i32::MIN] {
            assert_eq!(ReturnCode::from_c_int(code), None);
        }
    }

    #[test]
    fn zlib_error_from_c_int_rejects_non_errors() {
        // Success/informational codes belong to `ReturnCode`, not `ZlibError`.
        for code in [0, 1, 2] {
            assert_eq!(ZlibError::from_c_int(code), None);
        }
        // Out-of-range values.
        for code in [3, 42, -7, -100, i32::MAX, i32::MIN] {
            assert_eq!(ZlibError::from_c_int(code), None);
        }
    }

    // -- Message strings mirror `z_errmsg` exactly --------------------------

    #[test]
    fn return_code_messages_match_c() {
        assert_eq!(ReturnCode::Ok.message(), "");
        assert_eq!(ReturnCode::StreamEnd.message(), "stream end");
        assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
    }

    #[test]
    fn zlib_error_messages_match_c() {
        assert_eq!(ZlibError::Errno.message(), "file error");
        assert_eq!(ZlibError::StreamError.message(), "stream error");
        assert_eq!(ZlibError::DataError.message(), "data error");
        assert_eq!(ZlibError::MemError.message(), "insufficient memory");
        assert_eq!(ZlibError::BufError.message(), "buffer error");
        assert_eq!(ZlibError::VersionError.message(), "incompatible version");
    }

    /// `err_msg` is the single canonical lookup; every enum's `message()` must
    /// agree with it so the FFI / `zError` path can never diverge.
    #[test]
    fn message_methods_agree_with_err_msg() {
        for rc in ALL_RETURN_CODES {
            assert_eq!(rc.message(), err_msg(rc.as_c_int()));
        }
        for err in ALL_ERRORS {
            assert_eq!(err.message(), err_msg(err.as_c_int()));
        }
    }

    // -- `err_msg` reproduces the full C `ERR_MSG`/`z_errmsg` lookup ---------

    #[test]
    fn err_msg_matches_c_table() {
        // The exact assertions called out by the file spec.
        assert_eq!(err_msg(-3), "data error");
        assert_eq!(err_msg(0), "");
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(-100), "");
        assert_eq!(err_msg(1), "stream end");

        // Full in-range table, from `Z_NEED_DICT` (2) down to
        // `Z_VERSION_ERROR` (-6).
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(1), "stream end");
        assert_eq!(err_msg(0), "");
        assert_eq!(err_msg(-1), "file error");
        assert_eq!(err_msg(-2), "stream error");
        assert_eq!(err_msg(-3), "data error");
        assert_eq!(err_msg(-4), "insufficient memory");
        assert_eq!(err_msg(-5), "buffer error");
        assert_eq!(err_msg(-6), "incompatible version");
    }

    #[test]
    fn err_msg_out_of_range_is_empty() {
        // Just past both ends of the valid `-6..=2` range, plus extremes.
        for code in [3, 4, 100, -7, -8, -100, i32::MAX, i32::MIN] {
            assert_eq!(err_msg(code), "", "code {code} should map to empty");
        }
    }

    // -- `Result` conversions -----------------------------------------------

    #[test]
    fn code_to_result_maps_known_codes() {
        assert_eq!(code_to_result(0), Ok(ReturnCode::Ok));
        assert_eq!(code_to_result(1), Ok(ReturnCode::StreamEnd));
        assert_eq!(code_to_result(2), Ok(ReturnCode::NeedDict));
        assert_eq!(code_to_result(-1), Err(ZlibError::Errno));
        assert_eq!(code_to_result(-2), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(-3), Err(ZlibError::DataError));
        assert_eq!(code_to_result(-4), Err(ZlibError::MemError));
        assert_eq!(code_to_result(-5), Err(ZlibError::BufError));
        assert_eq!(code_to_result(-6), Err(ZlibError::VersionError));
    }

    #[test]
    fn code_to_result_out_of_range_is_stream_error() {
        for code in [3, 42, -7, -100, i32::MAX, i32::MIN] {
            assert_eq!(code_to_result(code), Err(ZlibError::StreamError));
        }
    }

    #[test]
    fn result_to_code_maps_both_arms() {
        assert_eq!(result_to_code(Ok(ReturnCode::Ok)), 0);
        assert_eq!(result_to_code(Ok(ReturnCode::StreamEnd)), 1);
        assert_eq!(result_to_code(Ok(ReturnCode::NeedDict)), 2);
        assert_eq!(result_to_code(Err(ZlibError::Errno)), -1);
        assert_eq!(result_to_code(Err(ZlibError::StreamError)), -2);
        assert_eq!(result_to_code(Err(ZlibError::DataError)), -3);
        assert_eq!(result_to_code(Err(ZlibError::MemError)), -4);
        assert_eq!(result_to_code(Err(ZlibError::BufError)), -5);
        assert_eq!(result_to_code(Err(ZlibError::VersionError)), -6);
    }

    /// `result_to_code(code_to_result(c)) == c` for every canonical code.
    #[test]
    fn code_result_round_trip_over_valid_range() {
        for code in -6..=2 {
            assert_eq!(result_to_code(code_to_result(code)), code);
        }
    }

    // -- `From` / `TryFrom` ergonomics --------------------------------------

    #[test]
    fn from_enum_for_i32() {
        assert_eq!(i32::from(ReturnCode::NeedDict), 2);
        assert_eq!(i32::from(ZlibError::BufError), -5);
        for rc in ALL_RETURN_CODES {
            assert_eq!(i32::from(rc), rc.as_c_int());
        }
        for err in ALL_ERRORS {
            assert_eq!(i32::from(err), err.as_c_int());
        }
    }

    #[test]
    fn try_from_i32_for_return_code() {
        for rc in ALL_RETURN_CODES {
            assert_eq!(ReturnCode::try_from(rc.as_c_int()), Ok(rc));
        }
        // A non-return code fails and preserves the offending value.
        assert_eq!(ReturnCode::try_from(-3), Err(UnknownValue::new(-3)));
        assert_eq!(ReturnCode::try_from(99), Err(UnknownValue::new(99)));
    }

    #[test]
    fn try_from_i32_for_zlib_error() {
        for err in ALL_ERRORS {
            assert_eq!(ZlibError::try_from(err.as_c_int()), Ok(err));
        }
        // A non-error code fails and preserves the offending value.
        assert_eq!(ZlibError::try_from(0), Err(UnknownValue::new(0)));
        assert_eq!(ZlibError::try_from(2), Err(UnknownValue::new(2)));
        assert_eq!(ZlibError::try_from(99), Err(UnknownValue::new(99)));
    }

    // -- `Display` -----------------------------------------------------------

    #[test]
    fn display_writes_message() {
        for err in ALL_ERRORS {
            assert_eq!(std::format!("{err}"), err.message());
        }
        // Spot-check a concrete rendering.
        assert_eq!(std::format!("{}", ZlibError::DataError), "data error");
    }

    // -- `std::error::Error` is present ONLY with the `std` feature ----------

    #[cfg(feature = "std")]
    #[test]
    fn zlib_error_implements_std_error_with_std() {
        // Compiles only because `impl std::error::Error for ZlibError` exists
        // under the `std` feature.
        let err = ZlibError::DataError;
        let as_error: &dyn std::error::Error = &err;
        assert_eq!(as_error.to_string(), "data error");
        // Leaf error: there is no underlying source.
        assert!(as_error.source().is_none());
    }
}
