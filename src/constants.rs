//! Public zlib constants: flush modes, compression levels, strategies, the
//! `windowBits` framing contract, the compression method, data-type hints, and
//! the window / memory-level bounds.
//!
//! This module reifies the public integer `#define`s from the C headers
//! `zlib.h` and `zconf.h` of **zlib 1.3.2.1-motley** as idiomatic Rust enums
//! and plain constants. It is the single, authoritative *public constant
//! surface* of the crate and is re-exported from the crate root into the public
//! prelude (`use crate::constants::{FlushMode, Strategy, WrapMode, ...}`).
//!
//! # Two representations, one contract
//!
//! For each C constant group this module exposes **both**:
//!
//! 1. an idiomatic Rust `enum` (e.g. [`FlushMode`], [`Strategy`], [`DataType`],
//!    [`Method`]) — the ergonomic, type-safe API used throughout the crate; and
//! 2. plain `pub const` integers carrying the canonical C `Z_*` names (e.g.
//!    [`Z_NO_FLUSH`]) — the ABI-name bridge so the FFI shim layer (`src/ffi`)
//!    and any C-facing code can reference the exact names `zlib.h` publishes.
//!
//! The enum discriminants and the corresponding `pub const` values are
//! **numerically identical**; the unit tests assert this equivalence so the two
//! representations can never silently drift apart.
//!
//! # Guarantees
//!
//! * Every numeric value is part of the preserved public API and wire-format
//!   contract and is **bit-identical** to the corresponding C macro. The values
//!   must never be altered (see AAP §0.7.1).
//! * The module is `no_std`-compatible — it references only `core` and performs
//!   **zero** `unsafe` operations.
//!
//! # Out of scope for this module
//!
//! * The compression/decompression **return codes** (`Z_OK` …
//!   `Z_VERSION_ERROR`, `zlib.h` L181-L189) are intentionally *not* defined
//!   here; they belong to the crate's `error` module.
//! * deflate/inflate **internal** constants (`STORED_BLOCK`, `MIN_MATCH`,
//!   `MAX_MATCH`, `PRESET_DICT`, `HEAP_SIZE`, …) live in their respective engine
//!   modules; this file is strictly the *public* surface.

// ---------------------------------------------------------------------------
// Fallible-conversion error type
// ---------------------------------------------------------------------------

/// Error returned by the fallible [`TryFrom<i32>`] conversions of the constant
/// enums ([`FlushMode`], [`Strategy`], [`DataType`], [`Method`]) when the
/// supplied integer does not correspond to any defined discriminant.
///
/// The offending value is preserved so callers (notably the FFI boundary, which
/// must translate it into a `Z_STREAM_ERROR`) can report or log it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TryFromConstantError {
    /// The offending integer that failed to map to a valid discriminant.
    pub value: i32,
}

impl core::fmt::Display for TryFromConstantError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid zlib constant value: {}", self.value)
    }
}

// `core::error::Error` (stable since Rust 1.81) keeps the type usable as a
// first-class error even in `no_std` builds. MSRV is 1.85, so this is safe.
impl core::error::Error for TryFromConstantError {}

/// Generates the standard C-integer conversions for a `#[repr(i32)]` constant
/// enum: a `const fn as_c_int`, a `const fn from_c_int`, and a [`TryFrom<i32>`]
/// implementation whose error is [`TryFromConstantError`].
///
/// The `$value` tokens are integer literals identical to the enum discriminants
/// — keeping them in a single place makes the C ⇔ Rust mapping auditable
/// against `zlib.h` at one location per type.
macro_rules! impl_c_int_conversions {
    ($ty:ty; $( $variant:path => $value:literal ),+ $(,)? ) => {
        impl $ty {
            /// Returns the underlying C integer value of this variant, matching
            /// the corresponding `zlib.h` macro exactly.
            #[must_use]
            #[inline]
            pub const fn as_c_int(self) -> i32 {
                self as i32
            }

            /// Converts a raw C integer into this enum, returning [`None`] if
            /// the value does not correspond to any defined variant.
            #[must_use]
            #[inline]
            pub const fn from_c_int(value: i32) -> Option<Self> {
                match value {
                    $( $value => Some($variant), )+
                    _ => None,
                }
            }
        }

        impl core::convert::TryFrom<i32> for $ty {
            type Error = TryFromConstantError;

            #[inline]
            fn try_from(value: i32) -> Result<Self, Self::Error> {
                Self::from_c_int(value).ok_or(TryFromConstantError { value })
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Flush modes — `zlib.h` L172-L178
// ---------------------------------------------------------------------------

/// Flush directives passed to `deflate()` and `inflate()`.
///
/// Mirrors the `Z_*_FLUSH`/`Z_*` flush family of macros in `zlib.h` (L172-L178).
/// The discriminants are the exact C integer values, so the enum can be cast
/// straight to the C ABI at the FFI boundary.
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FlushMode {
    /// `Z_NO_FLUSH = 0`: accumulate input and let the codec decide how much to
    /// output; the normal streaming case.
    NoFlush = 0,
    /// `Z_PARTIAL_FLUSH = 1`: flush pending output without aligning to a byte
    /// boundary in a way that resets the compressor.
    PartialFlush = 1,
    /// `Z_SYNC_FLUSH = 2`: flush all pending output and align to a byte
    /// boundary, emitting an empty stored block.
    SyncFlush = 2,
    /// `Z_FULL_FLUSH = 3`: like [`FlushMode::SyncFlush`] but also resets the
    /// compression state so decompression can restart from this point.
    FullFlush = 3,
    /// `Z_FINISH = 4`: no more input will be provided; finish the stream.
    Finish = 4,
    /// `Z_BLOCK = 5`: stop when the next block boundary is reached (deflate) or
    /// return at block boundaries (inflate).
    Block = 5,
    /// `Z_TREES = 6`: like [`FlushMode::Block`], but on inflate also stops right
    /// after the block's Huffman tree header has been decoded.
    Trees = 6,
}

impl_c_int_conversions!(FlushMode;
    FlushMode::NoFlush => 0,
    FlushMode::PartialFlush => 1,
    FlushMode::SyncFlush => 2,
    FlushMode::FullFlush => 3,
    FlushMode::Finish => 4,
    FlushMode::Block => 5,
    FlushMode::Trees => 6,
);

/// `Z_NO_FLUSH` (`zlib.h` L172) — see [`FlushMode::NoFlush`].
pub const Z_NO_FLUSH: i32 = 0;
/// `Z_PARTIAL_FLUSH` (`zlib.h` L173) — see [`FlushMode::PartialFlush`].
pub const Z_PARTIAL_FLUSH: i32 = 1;
/// `Z_SYNC_FLUSH` (`zlib.h` L174) — see [`FlushMode::SyncFlush`].
pub const Z_SYNC_FLUSH: i32 = 2;
/// `Z_FULL_FLUSH` (`zlib.h` L175) — see [`FlushMode::FullFlush`].
pub const Z_FULL_FLUSH: i32 = 3;
/// `Z_FINISH` (`zlib.h` L176) — see [`FlushMode::Finish`].
pub const Z_FINISH: i32 = 4;
/// `Z_BLOCK` (`zlib.h` L177) — see [`FlushMode::Block`].
pub const Z_BLOCK: i32 = 5;
/// `Z_TREES` (`zlib.h` L178) — see [`FlushMode::Trees`].
pub const Z_TREES: i32 = 6;

// ---------------------------------------------------------------------------
// Compression levels — `zlib.h` L194-L197
// ---------------------------------------------------------------------------

/// `Z_NO_COMPRESSION = 0` (`zlib.h` L194): store only, no compression.
pub const Z_NO_COMPRESSION: i32 = 0;
/// `Z_BEST_SPEED = 1` (`zlib.h` L195): fastest, least effective compression.
pub const Z_BEST_SPEED: i32 = 1;
/// `Z_BEST_COMPRESSION = 9` (`zlib.h` L196): slowest, most effective
/// compression.
pub const Z_BEST_COMPRESSION: i32 = 9;
/// `Z_DEFAULT_COMPRESSION = -1` (`zlib.h` L197): request the library's default
/// space/time trade-off (equivalent to level 6 inside the deflate engine).
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

/// Returns `true` if `level` is a valid zlib compression level.
///
/// A level is valid when it is [`Z_DEFAULT_COMPRESSION`] (`-1`) or lies in the
/// inclusive range `0..=9` (`zlib.h` L194-L197). The mapping of
/// [`Z_DEFAULT_COMPRESSION`] to the concrete internal level is a deflate-engine
/// concern and is intentionally *not* performed here.
///
/// # Examples
///
/// ```
/// # use zlib_rs::constants::{is_valid_level, Z_DEFAULT_COMPRESSION};
/// assert!(is_valid_level(Z_DEFAULT_COMPRESSION));
/// assert!(is_valid_level(0));
/// assert!(is_valid_level(9));
/// assert!(!is_valid_level(-2));
/// assert!(!is_valid_level(10));
/// ```
#[must_use]
#[inline]
pub const fn is_valid_level(level: i32) -> bool {
    matches!(level, Z_DEFAULT_COMPRESSION | 0..=9)
}

// ---------------------------------------------------------------------------
// Compression strategies — `zlib.h` L200-L204
// ---------------------------------------------------------------------------

/// Compression strategy tuning the deflate encoder, passed to `deflateInit2()`
/// / `deflateParams()`.
///
/// Mirrors the `Z_*` strategy macros in `zlib.h` (L200-L204). Only the encoder
/// heuristics change; every strategy still produces a fully compliant DEFLATE
/// stream.
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// `Z_DEFAULT_STRATEGY = 0`: the general-purpose default strategy.
    Default = 0,
    /// `Z_FILTERED = 1`: tuned for data produced by a filter (small,
    /// fairly random distribution of values); forces more Huffman coding and
    /// less string matching.
    Filtered = 1,
    /// `Z_HUFFMAN_ONLY = 2`: force Huffman encoding only, with no string
    /// matching.
    HuffmanOnly = 2,
    /// `Z_RLE = 3`: limit match distances to one (run-length encoding), for
    /// fast compression of PNG-style image data.
    Rle = 3,
    /// `Z_FIXED = 4`: prevent the use of dynamic Huffman codes, allowing a
    /// simpler decoder for special applications.
    Fixed = 4,
}

impl_c_int_conversions!(Strategy;
    Strategy::Default => 0,
    Strategy::Filtered => 1,
    Strategy::HuffmanOnly => 2,
    Strategy::Rle => 3,
    Strategy::Fixed => 4,
);

/// `Z_DEFAULT_STRATEGY` (`zlib.h` L204) — see [`Strategy::Default`].
pub const Z_DEFAULT_STRATEGY: i32 = 0;
/// `Z_FILTERED` (`zlib.h` L200) — see [`Strategy::Filtered`].
pub const Z_FILTERED: i32 = 1;
/// `Z_HUFFMAN_ONLY` (`zlib.h` L201) — see [`Strategy::HuffmanOnly`].
pub const Z_HUFFMAN_ONLY: i32 = 2;
/// `Z_RLE` (`zlib.h` L202) — see [`Strategy::Rle`].
pub const Z_RLE: i32 = 3;
/// `Z_FIXED` (`zlib.h` L203) — see [`Strategy::Fixed`].
pub const Z_FIXED: i32 = 4;

// ---------------------------------------------------------------------------
// Data types (`data_type` field) — `zlib.h` L207-L210
// ---------------------------------------------------------------------------

/// Possible values of the `data_type` field of the stream, set by `deflate()`
/// as a best-guess classification of the input.
///
/// Mirrors the `Z_BINARY` / `Z_TEXT` / `Z_UNKNOWN` macros in `zlib.h`
/// (L207-L210). The historical alias `Z_ASCII` is provided as
/// [`DataType::ASCII`].
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DataType {
    /// `Z_BINARY = 0`: the data appears to be binary.
    Binary = 0,
    /// `Z_TEXT = 1`: the data appears to be text.
    Text = 1,
    /// `Z_UNKNOWN = 2`: the data type could not be determined.
    Unknown = 2,
}

impl DataType {
    /// `Z_ASCII` — a historical alias for [`DataType::Text`] retained for
    /// compatibility with zlib 1.2.2 and earlier (`zlib.h` L209:
    /// `#define Z_ASCII Z_TEXT`).
    pub const ASCII: Self = Self::Text;
}

impl_c_int_conversions!(DataType;
    DataType::Binary => 0,
    DataType::Text => 1,
    DataType::Unknown => 2,
);

/// `Z_BINARY` (`zlib.h` L207) — see [`DataType::Binary`].
pub const Z_BINARY: i32 = 0;
/// `Z_TEXT` (`zlib.h` L208) — see [`DataType::Text`].
pub const Z_TEXT: i32 = 1;
/// `Z_ASCII` (`zlib.h` L209): historical alias for [`Z_TEXT`], numerically `1`.
pub const Z_ASCII: i32 = 1;
/// `Z_UNKNOWN` (`zlib.h` L210) — see [`DataType::Unknown`].
pub const Z_UNKNOWN: i32 = 2;

// ---------------------------------------------------------------------------
// Compression method — `zlib.h` L213
// ---------------------------------------------------------------------------

/// The compression method carried in the stream header.
///
/// zlib supports exactly one method — DEFLATE — represented by the single
/// variant [`Method::Deflated`] (`Z_DEFLATED = 8`, `zlib.h` L213).
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// `Z_DEFLATED = 8`: the DEFLATE compression method (the only method
    /// supported by this library).
    Deflated = 8,
}

impl_c_int_conversions!(Method;
    Method::Deflated => 8,
);

/// `Z_DEFLATED = 8` (`zlib.h` L213): the DEFLATE compression method — the only
/// method supported. See [`Method::Deflated`].
pub const Z_DEFLATED: i32 = 8;

// ---------------------------------------------------------------------------
// Null sentinel — `zlib.h` L216
// ---------------------------------------------------------------------------

/// `Z_NULL = 0` (`zlib.h` L216): the sentinel used by the C API for
/// initializing the `zalloc`, `zfree`, and `opaque` fields of `z_stream`.
///
/// Modeled as a `usize` because in the C API it is used exclusively in pointer
/// contexts. Idiomatic Rust code should prefer [`Option`]/`None`; this constant
/// exists so the FFI layer can reproduce the exact C spelling.
pub const Z_NULL: usize = 0;

// ---------------------------------------------------------------------------
// Window and memory-level bounds — `zconf.h` / `zutil.h`
// ---------------------------------------------------------------------------

/// `MAX_WBITS = 15` (`zconf.h` L287): the maximum `windowBits`, i.e. a 32 KiB
/// (`1 << 15`) LZ77 sliding window — the largest window the DEFLATE format
/// permits.
pub const MAX_WBITS: i32 = 15;

/// `DEF_WBITS = MAX_WBITS = 15` (`zutil.h` L76): the default `windowBits` used
/// for **decompression** when the caller does not specify one.
pub const DEF_WBITS: i32 = MAX_WBITS;

/// `MAX_MEM_LEVEL = 9` (`zconf.h` L273-L279): the maximum `memLevel` accepted
/// by `deflateInit2()`.
///
/// The C header defines this conditionally:
///
/// ```text
/// #ifndef MAX_MEM_LEVEL
/// #  ifdef MAXSEG_64K
/// #    define MAX_MEM_LEVEL 8
/// #  else
/// #    define MAX_MEM_LEVEL 9
/// #  endif
/// #endif
/// ```
///
/// `MAXSEG_64K` is a 16-bit / segmented-memory (MS-DOS) build constraint whose
/// platform support is explicitly **out of scope** for this migration (see AAP
/// §0.2.2, which excludes `msdos/`, 16-bit, and Windows CE targets). On every
/// in-scope modern platform the C library therefore defines `MAX_MEM_LEVEL` as
/// `9`, and `deflateInit2()` accepts `memLevel` in `1..=9` (`deflate.c` L434:
/// `if (memLevel < 1 || memLevel > MAX_MEM_LEVEL ...) return Z_STREAM_ERROR;`).
/// This crate uses `9` so its accepted-`memLevel` range is byte-for-byte
/// identical to the reference C build (the default remains
/// [`DEF_MEM_LEVEL`] `= 8`).
pub const MAX_MEM_LEVEL: i32 = 9;

/// `DEF_MEM_LEVEL = 8` (`zutil.h` L81): the default `memLevel` — a good
/// space/speed trade-off used when the caller does not specify one.
pub const DEF_MEM_LEVEL: i32 = 8;

/// Additive offset applied to `windowBits` (`16 + windowBits`) to request a
/// **gzip** wrapper (`24..=31`). See [`parse_window_bits`] and AAP §0.6.4.
pub const GZIP_WRAP_OFFSET: i32 = 16;

/// Additive offset applied to `windowBits` (`32 + windowBits`) to request
/// **auto-detection** of a zlib or gzip wrapper on inflate (`40..=47`). See
/// [`parse_window_bits`] and AAP §0.6.4.
pub const AUTO_WRAP_OFFSET: i32 = 32;

// ---------------------------------------------------------------------------
// `windowBits` overloading contract — AAP §0.6.4
// ---------------------------------------------------------------------------

/// The stream framing selected by the overloaded `windowBits` argument.
///
/// zlib overloads a single `windowBits` integer to pick both the sliding-window
/// size *and* the container format wrapped around the raw DEFLATE data. This
/// enum names the four framing choices; [`parse_window_bits`] decodes a raw
/// `windowBits` value into a `WrapMode` plus the effective window size.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WrapMode {
    /// zlib wrapper (RFC 1950): a 2-byte header and a trailing Adler-32
    /// checksum. Selected by `windowBits` in `8..=15`.
    Zlib,
    /// Raw DEFLATE (RFC 1951): no wrapper, no checksum. Selected by a negative
    /// `windowBits` in `-15..=-8`.
    Raw,
    /// gzip wrapper (RFC 1952): a gzip header and a trailing CRC-32 plus length.
    /// Selected by `windowBits` in `24..=31` (i.e. `16 + 8..=15`).
    Gzip,
    /// Auto-detect a zlib or gzip wrapper on **inflate**. Selected by
    /// `windowBits` in `40..=47` (i.e. `32 + 8..=15`).
    Auto,
}

/// Decodes an overloaded `windowBits` value into its [`WrapMode`] and the
/// effective window size in bits (always in `8..=15`).
///
/// This is the crate's **single source of truth** for zlib/raw/gzip/auto
/// framing selection, used by both the deflate and inflate engines. The four
/// ranges below govern wire-format compatibility and must match zlib exactly
/// (AAP §0.6.4):
///
/// | `window_bits`  | [`WrapMode`]      | effective bits |
/// |----------------|-------------------|----------------|
/// | `8..=15`       | [`WrapMode::Zlib`] | `window_bits`         |
/// | `-15..=-8`     | [`WrapMode::Raw`]  | `-window_bits`        |
/// | `24..=31`      | [`WrapMode::Gzip`] | `window_bits - 16`    |
/// | `40..=47`      | [`WrapMode::Auto`] | `window_bits - 32`    |
/// | anything else  | —                 | returns [`None`]      |
///
/// # `windowBits == 0`
///
/// A value of `0` is **rejected here** (returns [`None`]). In the C API, `0` is
/// accepted only by `inflateInit2()` and means "use the window size recorded in
/// the stream's zlib header". That is an inflate-specific special case: the
/// inflate engine substitutes [`DEF_WBITS`] (and zlib/auto framing) *before*
/// consulting this decoder, keeping this function's contract free of
/// direction-specific behavior.
///
/// # Examples
///
/// ```
/// # use zlib_rs::constants::{parse_window_bits, WrapMode};
/// assert_eq!(parse_window_bits(15), Some((WrapMode::Zlib, 15)));
/// assert_eq!(parse_window_bits(-15), Some((WrapMode::Raw, 15)));
/// assert_eq!(parse_window_bits(31), Some((WrapMode::Gzip, 15)));
/// assert_eq!(parse_window_bits(47), Some((WrapMode::Auto, 15)));
/// assert_eq!(parse_window_bits(0), None);
/// assert_eq!(parse_window_bits(16), None);
/// ```
#[must_use]
#[inline]
pub const fn parse_window_bits(window_bits: i32) -> Option<(WrapMode, u8)> {
    match window_bits {
        8..=15 => Some((WrapMode::Zlib, window_bits as u8)),
        -15..=-8 => Some((WrapMode::Raw, (-window_bits) as u8)),
        24..=31 => Some((WrapMode::Gzip, (window_bits - GZIP_WRAP_OFFSET) as u8)),
        40..=47 => Some((WrapMode::Auto, (window_bits - AUTO_WRAP_OFFSET) as u8)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here uses only `core` facilities so the module's test surface
    /// stays valid regardless of the crate's `no_std` configuration.

    // ---- Flush modes ------------------------------------------------------

    #[test]
    fn flush_mode_values_match_c() {
        // Enum discriminants equal the exact `zlib.h` L172-L178 macro values.
        assert_eq!(FlushMode::NoFlush as i32, 0);
        assert_eq!(FlushMode::PartialFlush as i32, 1);
        assert_eq!(FlushMode::SyncFlush as i32, 2);
        assert_eq!(FlushMode::FullFlush as i32, 3);
        assert_eq!(FlushMode::Finish as i32, 4);
        assert_eq!(FlushMode::Block as i32, 5);
        assert_eq!(FlushMode::Trees as i32, 6);

        // Bridge constants equal the same values.
        assert_eq!(Z_NO_FLUSH, 0);
        assert_eq!(Z_PARTIAL_FLUSH, 1);
        assert_eq!(Z_SYNC_FLUSH, 2);
        assert_eq!(Z_FULL_FLUSH, 3);
        assert_eq!(Z_FINISH, 4);
        assert_eq!(Z_BLOCK, 5);
        assert_eq!(Z_TREES, 6);

        // Enum and bridge constant must be numerically identical.
        assert_eq!(FlushMode::NoFlush.as_c_int(), Z_NO_FLUSH);
        assert_eq!(FlushMode::PartialFlush.as_c_int(), Z_PARTIAL_FLUSH);
        assert_eq!(FlushMode::SyncFlush.as_c_int(), Z_SYNC_FLUSH);
        assert_eq!(FlushMode::FullFlush.as_c_int(), Z_FULL_FLUSH);
        assert_eq!(FlushMode::Finish.as_c_int(), Z_FINISH);
        assert_eq!(FlushMode::Block.as_c_int(), Z_BLOCK);
        assert_eq!(FlushMode::Trees.as_c_int(), Z_TREES);
    }

    #[test]
    fn flush_mode_roundtrip_and_rejects() {
        let all = [
            FlushMode::NoFlush,
            FlushMode::PartialFlush,
            FlushMode::SyncFlush,
            FlushMode::FullFlush,
            FlushMode::Finish,
            FlushMode::Block,
            FlushMode::Trees,
        ];
        for mode in all {
            let v = mode.as_c_int();
            assert_eq!(FlushMode::from_c_int(v), Some(mode));
            assert_eq!(FlushMode::try_from(v), Ok(mode));
        }
        // Out-of-range values map to None / Err.
        assert_eq!(FlushMode::from_c_int(7), None);
        assert_eq!(FlushMode::from_c_int(-1), None);
        assert_eq!(
            FlushMode::try_from(7),
            Err(TryFromConstantError { value: 7 })
        );
    }

    // ---- Strategies -------------------------------------------------------

    #[test]
    fn strategy_values_match_c() {
        assert_eq!(Strategy::Default as i32, 0);
        assert_eq!(Strategy::Filtered as i32, 1);
        assert_eq!(Strategy::HuffmanOnly as i32, 2);
        assert_eq!(Strategy::Rle as i32, 3);
        assert_eq!(Strategy::Fixed as i32, 4);

        assert_eq!(Z_DEFAULT_STRATEGY, 0);
        assert_eq!(Z_FILTERED, 1);
        assert_eq!(Z_HUFFMAN_ONLY, 2);
        assert_eq!(Z_RLE, 3);
        assert_eq!(Z_FIXED, 4);

        assert_eq!(Strategy::Default.as_c_int(), Z_DEFAULT_STRATEGY);
        assert_eq!(Strategy::Filtered.as_c_int(), Z_FILTERED);
        assert_eq!(Strategy::HuffmanOnly.as_c_int(), Z_HUFFMAN_ONLY);
        assert_eq!(Strategy::Rle.as_c_int(), Z_RLE);
        assert_eq!(Strategy::Fixed.as_c_int(), Z_FIXED);
    }

    #[test]
    fn strategy_roundtrip_and_rejects() {
        let all = [
            Strategy::Default,
            Strategy::Filtered,
            Strategy::HuffmanOnly,
            Strategy::Rle,
            Strategy::Fixed,
        ];
        for strat in all {
            let v = strat.as_c_int();
            assert_eq!(Strategy::from_c_int(v), Some(strat));
            assert_eq!(Strategy::try_from(v), Ok(strat));
        }
        assert_eq!(Strategy::from_c_int(5), None);
        assert_eq!(Strategy::from_c_int(-1), None);
        assert!(Strategy::try_from(99).is_err());
    }

    // ---- Data types -------------------------------------------------------

    #[test]
    fn data_type_values_match_c() {
        assert_eq!(DataType::Binary as i32, 0);
        assert_eq!(DataType::Text as i32, 1);
        assert_eq!(DataType::Unknown as i32, 2);

        assert_eq!(Z_BINARY, 0);
        assert_eq!(Z_TEXT, 1);
        assert_eq!(Z_UNKNOWN, 2);

        // `Z_ASCII` is the historical alias for `Z_TEXT` (both `1`).
        assert_eq!(Z_ASCII, Z_TEXT);
        assert_eq!(Z_ASCII, 1);
        assert_eq!(DataType::ASCII, DataType::Text);
        assert_eq!(DataType::ASCII.as_c_int(), Z_ASCII);
    }

    #[test]
    fn data_type_roundtrip_and_rejects() {
        for dt in [DataType::Binary, DataType::Text, DataType::Unknown] {
            let v = dt.as_c_int();
            assert_eq!(DataType::from_c_int(v), Some(dt));
            assert_eq!(DataType::try_from(v), Ok(dt));
        }
        assert_eq!(DataType::from_c_int(3), None);
        assert_eq!(DataType::from_c_int(-1), None);
    }

    // ---- Method -----------------------------------------------------------

    #[test]
    fn method_values_match_c() {
        assert_eq!(Method::Deflated as i32, 8);
        assert_eq!(Z_DEFLATED, 8);
        assert_eq!(Method::Deflated.as_c_int(), Z_DEFLATED);
        assert_eq!(Method::from_c_int(8), Some(Method::Deflated));
        assert_eq!(Method::from_c_int(0), None);
        assert_eq!(Method::from_c_int(9), None);
        assert_eq!(Method::try_from(8), Ok(Method::Deflated));
        assert!(Method::try_from(7).is_err());
    }

    // ---- Compression levels ----------------------------------------------

    #[test]
    fn level_constants_and_validation() {
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);

        // Valid: the sentinel default plus the full 0..=9 range.
        assert!(is_valid_level(Z_DEFAULT_COMPRESSION));
        for level in 0..=9 {
            assert!(is_valid_level(level), "level {level} should be valid");
        }
        // Invalid: everything outside {-1} ∪ 0..=9.
        for level in [-2, 10, 100, i32::MIN, i32::MAX] {
            assert!(!is_valid_level(level), "level {level} should be invalid");
        }
    }

    // ---- Window / memory bounds ------------------------------------------

    #[test]
    fn window_and_memory_bounds() {
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(DEF_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(DEF_MEM_LEVEL, 8);
        assert_eq!(GZIP_WRAP_OFFSET, 16);
        assert_eq!(AUTO_WRAP_OFFSET, 32);
        assert_eq!(Z_NULL, 0);
    }

    // ---- `windowBits` decoding -------------------------------------------

    #[test]
    fn parse_window_bits_zlib() {
        assert_eq!(parse_window_bits(8), Some((WrapMode::Zlib, 8)));
        assert_eq!(parse_window_bits(15), Some((WrapMode::Zlib, 15)));
        assert_eq!(parse_window_bits(12), Some((WrapMode::Zlib, 12)));
    }

    #[test]
    fn parse_window_bits_raw() {
        assert_eq!(parse_window_bits(-8), Some((WrapMode::Raw, 8)));
        assert_eq!(parse_window_bits(-15), Some((WrapMode::Raw, 15)));
        assert_eq!(parse_window_bits(-9), Some((WrapMode::Raw, 9)));
    }

    #[test]
    fn parse_window_bits_gzip() {
        assert_eq!(parse_window_bits(24), Some((WrapMode::Gzip, 8)));
        assert_eq!(parse_window_bits(31), Some((WrapMode::Gzip, 15)));
        assert_eq!(parse_window_bits(28), Some((WrapMode::Gzip, 12)));
    }

    #[test]
    fn parse_window_bits_auto() {
        assert_eq!(parse_window_bits(40), Some((WrapMode::Auto, 8)));
        assert_eq!(parse_window_bits(47), Some((WrapMode::Auto, 15)));
        assert_eq!(parse_window_bits(44), Some((WrapMode::Auto, 12)));
    }

    #[test]
    fn parse_window_bits_rejects_out_of_range() {
        // `0` is intentionally rejected here (inflate handles it separately);
        // the rest sit in the gaps between the four valid ranges or beyond them.
        for v in [
            0,
            7,
            16,
            17,
            23,
            32,
            39,
            48,
            63,
            -7,
            -16,
            -100,
            i32::MIN,
            i32::MAX,
        ] {
            assert_eq!(parse_window_bits(v), None, "expected None for {v}");
        }
    }

    #[test]
    fn parse_window_bits_effective_bits_always_in_range() {
        // Whatever the framing, the decoded window size is always 8..=15.
        for wb in 8..=15 {
            for &raw in &[wb, -wb, wb + GZIP_WRAP_OFFSET, wb + AUTO_WRAP_OFFSET] {
                let (_, bits) = parse_window_bits(raw).expect("range should decode");
                assert!(
                    (8..=15).contains(&bits),
                    "bits {bits} out of range for {raw}"
                );
            }
        }
    }

    // ---- Error type -------------------------------------------------------

    #[test]
    fn try_from_error_carries_offending_value() {
        let err = FlushMode::try_from(1234).unwrap_err();
        assert_eq!(err, TryFromConstantError { value: 1234 });
        assert_eq!(err.value, 1234);

        // Type-level confirmation that the error implements the standard
        // traits without requiring an allocator (keeps the module `no_std`).
        fn assert_traits<T: core::fmt::Display + core::fmt::Debug + core::error::Error>() {}
        assert_traits::<TryFromConstantError>();
    }
}
