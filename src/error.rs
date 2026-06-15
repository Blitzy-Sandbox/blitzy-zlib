//! Idiomatic error and return-code types for `zlib_rs`.
//!
//! This module is the bridge between two worlds:
//!
//! * the **idiomatic safe Rust core**, which reports outcomes as
//!   [`Result<ReturnCode, ZlibError>`][Result] — i.e. an `Ok` carrying one of
//!   the informational [`ReturnCode`] values (`Z_OK`, `Z_STREAM_END`,
//!   `Z_NEED_DICT`) or an `Err` carrying one of the [`ZlibError`] failure
//!   variants; and
//! * the **frozen C ABI**, in which every public entry point returns a plain
//!   `int` status code. The `crate::ffi` shim converts the idiomatic
//!   [`Result`] back into that `int` with [`result_to_code`], and converts an
//!   incoming `int` into a [`Result`] with [`code_to_result`].
//!
//! Because the C ABI return values must be **bit-identical** to canonical zlib
//! (AAP §0.7.1), the integer discriminants of both enums are pinned to the
//! exact C constants from [`crate::constants`] and are verified against them by
//! a compile-time assertion block as well as by the unit tests. They must never
//! be "modernized" or renumbered.
//!
//! # The two outcome types
//!
//! C zlib multiplexes success, informational, and error outcomes onto a single
//! signed integer. This module splits that integer cleanly in two so the Rust
//! type system can enforce the distinction:
//!
//! | C code              | value | Rust representation              |
//! |---------------------|-------|----------------------------------|
//! | `Z_OK`              | `0`   | [`ReturnCode::Ok`]               |
//! | `Z_STREAM_END`      | `1`   | [`ReturnCode::StreamEnd`]        |
//! | `Z_NEED_DICT`       | `2`   | [`ReturnCode::NeedDict`]         |
//! | `Z_ERRNO`           | `-1`  | [`ZlibError::Errno`]             |
//! | `Z_STREAM_ERROR`    | `-2`  | [`ZlibError::StreamError`]       |
//! | `Z_DATA_ERROR`      | `-3`  | [`ZlibError::DataError`]         |
//! | `Z_MEM_ERROR`       | `-4`  | [`ZlibError::MemError`]          |
//! | `Z_BUF_ERROR`       | `-5`  | [`ZlibError::BufError`]          |
//! | `Z_VERSION_ERROR`   | `-6`  | [`ZlibError::VersionError`]      |
//!
//! Note that `Z_NEED_DICT` (`2`) is deliberately **not** an error: the inflate
//! path signals "a preset dictionary is required" by returning
//! `Ok(`[`ReturnCode::NeedDict`]`)`, and only the FFI boundary collapses it back
//! to the integer `2`.
//!
//! # Error messages
//!
//! [`err_msg`] is the single canonical reproduction of the C `z_errmsg` table
//! and the `ERR_MSG` indexing macro from `zutil.c` / `zutil.h`. Every consumer
//! that needs a human-readable description of a status code — most notably
//! `util::version::zError` and the FFI layer — must delegate here, so the
//! crate has exactly one source of truth for these strings. The per-variant
//! [`ZlibError::message`] and [`ReturnCode::message`] accessors return the same
//! strings, and a unit test asserts they never diverge from [`err_msg`].
//!
//! # `no_std`
//!
//! This module is `no_std`-clean: it references only [`core`] (never `std`), so
//! it compiles unchanged under the crate's `no-std` feature. In particular,
//! [`ZlibError`] implements [`core::error::Error`] unconditionally — that trait
//! has been available in `core` since Rust 1.81, comfortably below the crate's
//! 1.85.0 MSRV, and `std::error::Error` is merely a re-export of it, so the
//! single unconditional impl makes [`ZlibError`] usable as a standard error in
//! both `std` and `no_std` builds. This mirrors the established pattern of the
//! sibling [`crate::constants`] module.
//!
//! # Source mapping
//!
//! | Item                        | Upstream origin                         |
//! |-----------------------------|-----------------------------------------|
//! | [`err_msg`] / `Z_ERRMSG`    | `zutil.c` `z_errmsg[10]` + `zutil.h` `ERR_MSG` |
//! | Return / error discriminants| `zlib.h` L181-189 (via `crate::constants`)     |

use crate::constants::{
    InvalidConstant, Z_BUF_ERROR, Z_DATA_ERROR, Z_ERRNO, Z_MEM_ERROR, Z_NEED_DICT, Z_OK,
    Z_STREAM_END, Z_STREAM_ERROR, Z_VERSION_ERROR,
};

// ===========================================================================
// ReturnCode — the success / informational outcomes (non-negative C codes)
// ===========================================================================

/// The non-negative zlib status codes: successful or otherwise *normal*
/// outcomes that are **not** errors.
///
/// These are the values that appear in the `Ok` arm of the crate's
/// [`Result`] alias. Each discriminant is exactly the matching C constant
/// ([`Z_OK`], [`Z_STREAM_END`], [`Z_NEED_DICT`]); the `#[repr(i32)]` attribute
/// guarantees the in-memory representation matches C `int`, so
/// [`ReturnCode::as_c_int`] is a zero-cost, exact conversion and
/// [`ReturnCode::from_c_int`] is its checked inverse.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ReturnCode;
///
/// assert_eq!(ReturnCode::StreamEnd.as_c_int(), 1);
/// assert_eq!(ReturnCode::from_c_int(2), Some(ReturnCode::NeedDict));
/// assert_eq!(ReturnCode::from_c_int(-3), None); // an error code, not a ReturnCode
/// ```
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnCode {
    /// Operation completed successfully (`Z_OK`, `0`).
    Ok = 0,
    /// The end of the compressed stream has been reached (`Z_STREAM_END`, `1`).
    StreamEnd = 1,
    /// A preset dictionary is required before decompression can continue
    /// (`Z_NEED_DICT`, `2`). This is an informational signal, **not** an error;
    /// see the `inflateSetDictionary` handshake.
    NeedDict = 2,
}

impl ReturnCode {
    /// Returns the C `int` value backing this code.
    ///
    /// This is the discriminant itself (`self as i32`) and is therefore an
    /// exact, infallible, zero-cost conversion. It is the inverse of
    /// [`ReturnCode::from_c_int`] for every variant.
    #[must_use]
    pub const fn as_c_int(self) -> i32 {
        self as i32
    }

    /// Converts a raw C status code into a [`ReturnCode`].
    ///
    /// Returns [`None`] for any value that is not one of the non-negative zlib
    /// status codes — including the negative error codes, which belong to
    /// [`ZlibError`] instead (use [`ZlibError::from_c_int`] for those, or
    /// [`code_to_result`] to dispatch on the sign automatically).
    #[must_use]
    pub fn from_c_int(value: i32) -> Option<Self> {
        match value {
            Z_OK => Some(Self::Ok),
            Z_STREAM_END => Some(Self::StreamEnd),
            Z_NEED_DICT => Some(Self::NeedDict),
            _ => None,
        }
    }

    /// Returns the canonical human-readable message for this code.
    ///
    /// The strings reproduce the corresponding entries of the C `z_errmsg`
    /// table exactly (`Z_OK` maps to the empty string, matching upstream). The
    /// value is identical to `err_msg(self.as_c_int())`; a unit test enforces
    /// that the two never diverge.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Ok => "",
            Self::StreamEnd => "stream end",
            Self::NeedDict => "need dictionary",
        }
    }
}

// ===========================================================================
// ZlibError — the failure outcomes (negative C codes)
// ===========================================================================

/// The negative zlib status codes: the failure outcomes.
///
/// These are the values that appear in the `Err` arm of the crate's
/// [`Result`] alias. Each discriminant is exactly the matching C constant
/// ([`Z_ERRNO`] through [`Z_VERSION_ERROR`]); the `#[repr(i32)]` attribute
/// guarantees the representation matches C `int`, so [`ZlibError::as_c_int`]
/// is a zero-cost, exact conversion (`ZlibError::DataError as i32 == -3`) and
/// [`ZlibError::from_c_int`] is its checked inverse.
///
/// [`ZlibError`] implements [`core::fmt::Display`] (writing [`message`]) and
/// [`core::error::Error`], so it composes with the wider Rust error ecosystem
/// (the `?` operator, `Box<dyn Error>`, `anyhow`, …) while remaining
/// `no_std`-clean.
///
/// Note that [`Z_NEED_DICT`] (`2`) is intentionally absent: it is an
/// informational outcome modeled by [`ReturnCode::NeedDict`], not a failure.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::ZlibError;
///
/// assert_eq!(ZlibError::DataError.as_c_int(), -3);
/// assert_eq!(ZlibError::DataError.message(), "data error");
/// assert_eq!(ZlibError::from_c_int(-4), Some(ZlibError::MemError));
/// assert_eq!(ZlibError::from_c_int(0), None); // Z_OK is a ReturnCode, not an error
/// ```
///
/// [`message`]: ZlibError::message
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZlibError {
    /// A file-system or `errno`-style error occurred during a gzip file
    /// operation (`Z_ERRNO`, `-1`).
    Errno = -1,
    /// The stream state was inconsistent, or one or more parameters supplied to
    /// the call were invalid (`Z_STREAM_ERROR`, `-2`).
    StreamError = -2,
    /// The input data was corrupted or did not conform to the expected format
    /// (`Z_DATA_ERROR`, `-3`).
    DataError = -3,
    /// Memory could not be allocated (`Z_MEM_ERROR`, `-4`).
    MemError = -4,
    /// No progress is possible: the call needs more input or more output room
    /// to continue (`Z_BUF_ERROR`, `-5`). This is a recoverable, non-fatal
    /// condition.
    BufError = -5,
    /// The zlib version the caller compiled against is incompatible with this
    /// library (`Z_VERSION_ERROR`, `-6`).
    VersionError = -6,
}

impl ZlibError {
    /// Returns the C `int` value backing this error.
    ///
    /// This is the (negative) discriminant itself (`self as i32`) and is
    /// therefore an exact, infallible, zero-cost conversion. It is the inverse
    /// of [`ZlibError::from_c_int`] for every variant.
    #[must_use]
    pub const fn as_c_int(self) -> i32 {
        self as i32
    }

    /// Converts a raw C status code into a [`ZlibError`].
    ///
    /// Maps the six negative error codes `-1..=-6` to their variants and
    /// returns [`None`] for every other value. In particular the non-negative
    /// codes `0`, `1`, and `2` are **not** errors and yield [`None`]; convert
    /// those with [`ReturnCode::from_c_int`], or use [`code_to_result`] to
    /// dispatch on the sign automatically.
    #[must_use]
    pub fn from_c_int(value: i32) -> Option<Self> {
        match value {
            Z_ERRNO => Some(Self::Errno),
            Z_STREAM_ERROR => Some(Self::StreamError),
            Z_DATA_ERROR => Some(Self::DataError),
            Z_MEM_ERROR => Some(Self::MemError),
            Z_BUF_ERROR => Some(Self::BufError),
            Z_VERSION_ERROR => Some(Self::VersionError),
            _ => None,
        }
    }

    /// Returns the canonical human-readable message for this error.
    ///
    /// The strings reproduce the corresponding entries of the C `z_errmsg`
    /// table exactly. The value is identical to `err_msg(self.as_c_int())`; a
    /// unit test enforces that the two never diverge. This accessor is also
    /// what the [`core::fmt::Display`] implementation writes.
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

// ===========================================================================
// err_msg — the single canonical reproduction of the C `z_errmsg` table
// ===========================================================================

/// The C `z_errmsg` message table from `zutil.c`, reproduced verbatim and in
/// the same order.
///
/// The table is indexed by `2 - code` (see [`err_msg`]), so the rows run from
/// the most positive status code (`Z_NEED_DICT`, `+2`) down to the most
/// negative (`Z_VERSION_ERROR`, `-6`), with a trailing empty entry used as the
/// out-of-range sentinel.
const Z_ERRMSG: [&str; 10] = [
    "need dictionary",      // index 0 — Z_NEED_DICT       (+2)
    "stream end",           // index 1 — Z_STREAM_END      (+1)
    "",                     // index 2 — Z_OK               (0)
    "file error",           // index 3 — Z_ERRNO           (-1)
    "stream error",         // index 4 — Z_STREAM_ERROR    (-2)
    "data error",           // index 5 — Z_DATA_ERROR      (-3)
    "insufficient memory",  // index 6 — Z_MEM_ERROR       (-4)
    "buffer error",         // index 7 — Z_BUF_ERROR       (-5)
    "incompatible version", // index 8 — Z_VERSION_ERROR   (-6)
    "",                     // index 9 — out-of-range sentinel
];

/// Returns the canonical zlib message string for any C status `code`.
///
/// This is an exact reproduction of the C `ERR_MSG` macro from `zutil.h`:
///
/// ```c
/// #define ERR_MSG(err) z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// Codes in the canonical range `-6..=2` map to their descriptive string
/// (with `Z_OK` mapping to the empty string), and **any** out-of-range value
/// maps to the empty string via the sentinel at index `9`. This function is the
/// crate's single source of truth for status-code messages: `util::version`'s
/// `zError` and the FFI layer delegate here so the strings can never diverge.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::err_msg;
///
/// assert_eq!(err_msg(0), "");                    // Z_OK
/// assert_eq!(err_msg(1), "stream end");          // Z_STREAM_END
/// assert_eq!(err_msg(2), "need dictionary");     // Z_NEED_DICT
/// assert_eq!(err_msg(-3), "data error");         // Z_DATA_ERROR
/// assert_eq!(err_msg(-100), "");                 // out of range
/// ```
#[must_use]
pub const fn err_msg(code: i32) -> &'static str {
    // Mirror the C ternary exactly. The `2 - code` subtraction is evaluated
    // only when `-6 <= code <= 2`, so it can never overflow and the resulting
    // index is always within `0..=8`; out-of-range codes select the index-9
    // sentinel. The cast to `usize` is therefore always of a value in `0..=9`.
    let index = if code < -6 || code > 2 {
        9
    } else {
        (2 - code) as usize
    };
    Z_ERRMSG[index]
}

// ===========================================================================
// Trait implementations — Display and Error for ZlibError
// ===========================================================================

impl core::fmt::Display for ZlibError {
    /// Writes the canonical [`message`](ZlibError::message) for this error.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message())
    }
}

// `core::error::Error` is available since Rust 1.81 (well below the crate's
// 1.85.0 MSRV) and `std::error::Error` is a re-export of it. Implementing the
// `core` trait unconditionally keeps the module `no_std`-clean while still
// making `ZlibError` a fully-fledged `std::error::Error` in `std` builds. This
// matches the established convention of the sibling `crate::constants` module.
impl core::error::Error for ZlibError {}

// ===========================================================================
// Result alias and Result <-> C-code conversions
// ===========================================================================

/// The crate-wide result type: a [`core::result::Result`] whose error is
/// always [`ZlibError`].
///
/// The success type `T` is most often [`ReturnCode`] — the idiomatic safe API
/// surfaces outcomes as `Result<ReturnCode>` — but the alias is generic so
/// engine helpers can return richer success payloads (counts, byte buffers, …)
/// while still funneling failures through the single [`ZlibError`] type.
pub type Result<T> = core::result::Result<T, ZlibError>;

/// Translates a raw C status `code` into an idiomatic [`Result`].
///
/// The three non-negative codes become `Ok` carrying the matching
/// [`ReturnCode`]; the six canonical error codes `-1..=-6` become `Err`
/// carrying the matching [`ZlibError`]. Any value outside the canonical
/// `-6..=2` range indicates an unexpected/inconsistent status and is mapped to
/// `Err(`[`ZlibError::StreamError`]`)`, mirroring zlib's use of
/// `Z_STREAM_ERROR` for inconsistent state.
///
/// This is the inverse of [`result_to_code`] across the canonical range.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{code_to_result, ReturnCode, ZlibError};
///
/// assert_eq!(code_to_result(1), Ok(ReturnCode::StreamEnd));
/// assert_eq!(code_to_result(-4), Err(ZlibError::MemError));
/// ```
// Note: no `#[must_use]` here — the returned `Result` is already `#[must_use]`,
// so annotating the function would be redundant (clippy::double_must_use).
pub fn code_to_result(code: i32) -> Result<ReturnCode> {
    match code {
        Z_OK => Ok(ReturnCode::Ok),
        Z_STREAM_END => Ok(ReturnCode::StreamEnd),
        Z_NEED_DICT => Ok(ReturnCode::NeedDict),
        Z_ERRNO => Err(ZlibError::Errno),
        Z_STREAM_ERROR => Err(ZlibError::StreamError),
        Z_DATA_ERROR => Err(ZlibError::DataError),
        Z_MEM_ERROR => Err(ZlibError::MemError),
        Z_BUF_ERROR => Err(ZlibError::BufError),
        Z_VERSION_ERROR => Err(ZlibError::VersionError),
        // An unexpected code denotes an inconsistent internal state; surface it
        // as the generic stream error rather than panicking.
        _ => Err(ZlibError::StreamError),
    }
}

/// Collapses an idiomatic [`Result`] back into the C `int` status code.
///
/// This is exactly what the `crate::ffi` shim uses to produce the return value
/// of each `extern "C"` entry point: an `Ok(rc)` yields `rc.as_c_int()` and an
/// `Err(e)` yields `e.as_c_int()`, so the resulting integer is bit-identical to
/// what canonical C zlib would return.
///
/// # Examples
///
/// ```
/// use zlib_rs::error::{result_to_code, ReturnCode, ZlibError};
///
/// assert_eq!(result_to_code(Ok(ReturnCode::Ok)), 0);
/// assert_eq!(result_to_code(Err(ZlibError::MemError)), -4);
/// ```
#[must_use]
pub const fn result_to_code(result: Result<ReturnCode>) -> i32 {
    match result {
        Ok(code) => code.as_c_int(),
        Err(error) => error.as_c_int(),
    }
}

// ===========================================================================
// Ergonomic conversions: From<…> for i32 and TryFrom<i32> for the enums
// ===========================================================================

impl From<ReturnCode> for i32 {
    /// Returns the raw C status code backing this [`ReturnCode`].
    fn from(value: ReturnCode) -> Self {
        value.as_c_int()
    }
}

impl From<ZlibError> for i32 {
    /// Returns the raw (negative) C status code backing this [`ZlibError`].
    fn from(value: ZlibError) -> Self {
        value.as_c_int()
    }
}

impl TryFrom<i32> for ReturnCode {
    type Error = InvalidConstant;

    /// Converts a raw C status code into a [`ReturnCode`], returning
    /// [`InvalidConstant`] (carrying the offending value) when the code is not
    /// one of the non-negative status codes.
    ///
    /// The return type is written as a fully-qualified
    /// [`core::result::Result`] because this module defines a [`Result`] alias
    /// that fixes the error type to [`ZlibError`].
    fn try_from(value: i32) -> core::result::Result<Self, Self::Error> {
        Self::from_c_int(value).ok_or(InvalidConstant(value))
    }
}

impl TryFrom<i32> for ZlibError {
    type Error = InvalidConstant;

    /// Converts a raw C status code into a [`ZlibError`], returning
    /// [`InvalidConstant`] (carrying the offending value) when the code is not
    /// one of the negative error codes `-1..=-6`.
    ///
    /// The return type is written as a fully-qualified
    /// [`core::result::Result`] because this module defines a [`Result`] alias
    /// that fixes the error type to [`ZlibError`].
    fn try_from(value: i32) -> core::result::Result<Self, Self::Error> {
        Self::from_c_int(value).ok_or(InvalidConstant(value))
    }
}

// ===========================================================================
// Compile-time frozen-ABI guard
// ===========================================================================
//
// The FFI return values must be bit-identical to canonical C zlib (AAP
// §0.7.1). These assertions pin every enum discriminant to the corresponding
// `crate::constants` value at COMPILE time, so any accidental renumbering of an
// enum — or of a constant — becomes a hard build error rather than a silent
// ABI break that only manifests at run time.
const _: () = {
    assert!(ReturnCode::Ok.as_c_int() == Z_OK);
    assert!(ReturnCode::StreamEnd.as_c_int() == Z_STREAM_END);
    assert!(ReturnCode::NeedDict.as_c_int() == Z_NEED_DICT);
    assert!(ZlibError::Errno.as_c_int() == Z_ERRNO);
    assert!(ZlibError::StreamError.as_c_int() == Z_STREAM_ERROR);
    assert!(ZlibError::DataError.as_c_int() == Z_DATA_ERROR);
    assert!(ZlibError::MemError.as_c_int() == Z_MEM_ERROR);
    assert!(ZlibError::BufError.as_c_int() == Z_BUF_ERROR);
    assert!(ZlibError::VersionError.as_c_int() == Z_VERSION_ERROR);
};

// ===========================================================================
// Tests — verify the discriminants are frozen to the C codes, the message
// table mirrors `z_errmsg` exactly, and every conversion round-trips.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`ReturnCode`] variant, for exhaustive iteration in tests.
    const RETURN_CODES: [ReturnCode; 3] =
        [ReturnCode::Ok, ReturnCode::StreamEnd, ReturnCode::NeedDict];

    /// Every [`ZlibError`] variant, for exhaustive iteration in tests.
    const ZLIB_ERRORS: [ZlibError; 6] = [
        ZlibError::Errno,
        ZlibError::StreamError,
        ZlibError::DataError,
        ZlibError::MemError,
        ZlibError::BufError,
        ZlibError::VersionError,
    ];

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
    fn as_c_int_matches_constants() {
        assert_eq!(ReturnCode::Ok.as_c_int(), crate::constants::Z_OK);
        assert_eq!(
            ReturnCode::StreamEnd.as_c_int(),
            crate::constants::Z_STREAM_END
        );
        assert_eq!(
            ReturnCode::NeedDict.as_c_int(),
            crate::constants::Z_NEED_DICT
        );
        assert_eq!(ZlibError::Errno.as_c_int(), crate::constants::Z_ERRNO);
        assert_eq!(
            ZlibError::StreamError.as_c_int(),
            crate::constants::Z_STREAM_ERROR
        );
        assert_eq!(
            ZlibError::DataError.as_c_int(),
            crate::constants::Z_DATA_ERROR
        );
        assert_eq!(
            ZlibError::MemError.as_c_int(),
            crate::constants::Z_MEM_ERROR
        );
        assert_eq!(
            ZlibError::BufError.as_c_int(),
            crate::constants::Z_BUF_ERROR
        );
        assert_eq!(
            ZlibError::VersionError.as_c_int(),
            crate::constants::Z_VERSION_ERROR
        );
    }

    #[test]
    fn return_code_from_c_int_round_trips() {
        for code in RETURN_CODES {
            assert_eq!(ReturnCode::from_c_int(code.as_c_int()), Some(code));
        }
    }

    #[test]
    fn zlib_error_from_c_int_round_trips() {
        for error in ZLIB_ERRORS {
            assert_eq!(ZlibError::from_c_int(error.as_c_int()), Some(error));
        }
    }

    #[test]
    fn return_code_from_c_int_rejects_non_return_codes() {
        // Negative error codes are not return codes.
        for error in ZLIB_ERRORS {
            assert_eq!(ReturnCode::from_c_int(error.as_c_int()), None);
        }
        // Arbitrary out-of-range values.
        assert_eq!(ReturnCode::from_c_int(3), None);
        assert_eq!(ReturnCode::from_c_int(-7), None);
        assert_eq!(ReturnCode::from_c_int(i32::MAX), None);
        assert_eq!(ReturnCode::from_c_int(i32::MIN), None);
    }

    #[test]
    fn zlib_error_from_c_int_rejects_non_errors() {
        // The non-negative status codes are not errors.
        for code in RETURN_CODES {
            assert_eq!(ZlibError::from_c_int(code.as_c_int()), None);
        }
        // Values just outside the error range and far away.
        assert_eq!(ZlibError::from_c_int(-7), None);
        assert_eq!(ZlibError::from_c_int(3), None);
        assert_eq!(ZlibError::from_c_int(i32::MAX), None);
        assert_eq!(ZlibError::from_c_int(i32::MIN), None);
    }

    #[test]
    fn messages_match_c_table() {
        assert_eq!(ReturnCode::Ok.message(), "");
        assert_eq!(ReturnCode::StreamEnd.message(), "stream end");
        assert_eq!(ReturnCode::NeedDict.message(), "need dictionary");
        assert_eq!(ZlibError::Errno.message(), "file error");
        assert_eq!(ZlibError::StreamError.message(), "stream error");
        assert_eq!(ZlibError::DataError.message(), "data error");
        assert_eq!(ZlibError::MemError.message(), "insufficient memory");
        assert_eq!(ZlibError::BufError.message(), "buffer error");
        assert_eq!(ZlibError::VersionError.message(), "incompatible version");
    }

    #[test]
    fn err_msg_reproduces_z_errmsg() {
        // The exact known-answer vectors from the agent prompt.
        assert_eq!(err_msg(-3), "data error");
        assert_eq!(err_msg(0), "");
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(-100), "");
        assert_eq!(err_msg(1), "stream end");

        // The full canonical table, top (+2) to bottom (-6).
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(1), "stream end");
        assert_eq!(err_msg(0), "");
        assert_eq!(err_msg(-1), "file error");
        assert_eq!(err_msg(-2), "stream error");
        assert_eq!(err_msg(-3), "data error");
        assert_eq!(err_msg(-4), "insufficient memory");
        assert_eq!(err_msg(-5), "buffer error");
        assert_eq!(err_msg(-6), "incompatible version");

        // Out-of-range on both sides, including the exact boundaries.
        assert_eq!(err_msg(3), "");
        assert_eq!(err_msg(-7), "");
        assert_eq!(err_msg(i32::MAX), "");
        assert_eq!(err_msg(i32::MIN), "");
    }

    #[test]
    fn message_never_diverges_from_err_msg() {
        // The per-variant `message()` accessors must agree with the canonical
        // `err_msg` lookup for every variant — this is the anti-divergence
        // guarantee that lets the FFI / `zError` delegate to `err_msg`.
        for code in RETURN_CODES {
            assert_eq!(code.message(), err_msg(code.as_c_int()));
        }
        for error in ZLIB_ERRORS {
            assert_eq!(error.message(), err_msg(error.as_c_int()));
        }
    }

    #[test]
    fn code_to_result_maps_every_canonical_code() {
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
    fn code_to_result_maps_unexpected_codes_to_stream_error() {
        assert_eq!(code_to_result(3), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(-7), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(i32::MAX), Err(ZlibError::StreamError));
        assert_eq!(code_to_result(i32::MIN), Err(ZlibError::StreamError));
    }

    #[test]
    fn result_to_code_collapses_both_arms() {
        assert_eq!(result_to_code(Ok(ReturnCode::Ok)), 0);
        assert_eq!(result_to_code(Ok(ReturnCode::StreamEnd)), 1);
        assert_eq!(result_to_code(Ok(ReturnCode::NeedDict)), 2);
        assert_eq!(result_to_code(Err(ZlibError::MemError)), -4);
        assert_eq!(result_to_code(Err(ZlibError::VersionError)), -6);
    }

    #[test]
    fn code_and_result_round_trip_over_canonical_range() {
        // For every canonical code, `result_to_code(code_to_result(code))`
        // is the identity.
        for code in -6..=2 {
            assert_eq!(result_to_code(code_to_result(code)), code);
        }
    }

    // This test materializes the `Display` output into an owned `String`, which
    // requires `alloc`/`std`. The `Display` impl itself is `no_std`-clean (it
    // only calls `f.write_str`); gating the test keeps the test target
    // compilable across the whole feature matrix, including `--no-default-features`.
    #[cfg(feature = "std")]
    #[test]
    fn display_writes_canonical_message() {
        assert_eq!(format!("{}", ZlibError::DataError), "data error");
        assert_eq!(format!("{}", ZlibError::MemError), "insufficient memory");
        // Display and `message()` must agree for every error variant.
        for error in ZLIB_ERRORS {
            assert_eq!(error.to_string(), error.message());
        }
    }

    // Uses `to_string()` (and hence `alloc`); see the note on
    // `display_writes_canonical_message`. The `core::error::Error` impl under
    // test is itself `no_std`-clean.
    #[cfg(feature = "std")]
    #[test]
    fn usable_as_std_error_trait_object() {
        // Confirms the `core::error::Error` impl is present and composes with
        // trait objects and `Display`.
        let error: &dyn core::error::Error = &ZlibError::BufError;
        assert_eq!(error.to_string(), "buffer error");
        assert!(error.source().is_none());
    }

    #[test]
    fn propagates_through_question_mark_operator() {
        // The crate `Result` alias must interoperate with `?`.
        fn decode(code: i32) -> Result<ReturnCode> {
            let outcome = code_to_result(code)?;
            Ok(outcome)
        }
        assert_eq!(decode(1), Ok(ReturnCode::StreamEnd));
        assert_eq!(decode(-3), Err(ZlibError::DataError));
    }

    #[test]
    fn from_enum_into_i32() {
        assert_eq!(i32::from(ReturnCode::StreamEnd), 1);
        assert_eq!(i32::from(ZlibError::BufError), -5);
        // The generic `Into` form must agree.
        let code: i32 = ZlibError::DataError.into();
        assert_eq!(code, -3);
    }

    #[test]
    fn try_from_i32_for_return_code() {
        assert_eq!(ReturnCode::try_from(0), Ok(ReturnCode::Ok));
        assert_eq!(ReturnCode::try_from(2), Ok(ReturnCode::NeedDict));
        assert_eq!(ReturnCode::try_from(-3), Err(InvalidConstant(-3)));
        assert_eq!(ReturnCode::try_from(7), Err(InvalidConstant(7)));
    }

    #[test]
    fn try_from_i32_for_zlib_error() {
        assert_eq!(ZlibError::try_from(-3), Ok(ZlibError::DataError));
        assert_eq!(ZlibError::try_from(-6), Ok(ZlibError::VersionError));
        assert_eq!(ZlibError::try_from(0), Err(InvalidConstant(0)));
        assert_eq!(ZlibError::try_from(-7), Err(InvalidConstant(-7)));
    }

    #[test]
    fn enums_are_copy_and_eq() {
        // `Copy` (use after move) and `PartialEq`/`Eq` sanity.
        let error = ZlibError::DataError;
        let copied = error;
        assert_eq!(error, copied);
        assert_ne!(ZlibError::DataError, ZlibError::MemError);

        let code = ReturnCode::NeedDict;
        let copied_code = code;
        assert_eq!(code, copied_code);
        assert_ne!(ReturnCode::Ok, ReturnCode::StreamEnd);
    }

    #[test]
    fn const_contexts_compile() {
        // `as_c_int`, `message`, `err_msg`, and `result_to_code` are all
        // `const fn`; exercise them in genuine const contexts.
        const ERR_CODE: i32 = ZlibError::DataError.as_c_int();
        const OK_CODE: i32 = ReturnCode::Ok.as_c_int();
        const MSG: &str = ZlibError::MemError.message();
        const LOOKUP: &str = err_msg(-5);
        const COLLAPSED: i32 = result_to_code(Ok(ReturnCode::NeedDict));

        assert_eq!(ERR_CODE, -3);
        assert_eq!(OK_CODE, 0);
        assert_eq!(MSG, "insufficient memory");
        assert_eq!(LOOKUP, "buffer error");
        assert_eq!(COLLAPSED, 2);
    }
}
