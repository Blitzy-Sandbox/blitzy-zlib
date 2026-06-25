//! zlib public constants: flush modes, levels, strategies, data-type hints,
//! format limits, and version.
//!
//! This module is the single, self-contained source of truth for every
//! compile-time constant and small parameter enum that the zlib API exposes.
//! It replaces the C preprocessor `#define`s found in `zlib.h`, `zutil.h`, and
//! `zconf.h` with strongly typed Rust equivalents while preserving their exact
//! numeric values.
//!
//! # ABI contract
//!
//! The values defined here are part of the zlib ABI contract. The `libz-rs-sys`
//! FFI shim re-exposes them as the C `Z_*` `#define` constants, so any drift
//! from the upstream numbers would break drop-in compatibility. Every value is
//! therefore reproduced byte-for-byte from the C baseline
//! (`ZLIB_VERSION "1.3.2.1-motley"`, `ZLIB_VERNUM 0x1321`).
//!
//! The [`Flush`], [`Strategy`], and [`DataType`] enums are `#[repr(i32)]`, so a
//! plain `value as i32` cast yields the precise integer the C API uses, and the
//! [`Flush::try_from_i32`] / [`TryFrom`] conversions turn a raw C `int` back
//! into a typed value at the FFI boundary.
//!
//! # `no_std`
//!
//! This module is `no_std`-clean: it contains only `const` items and `enum`
//! definitions, performs no allocation, and references nothing outside the
//! `core` prelude. It is also free of `unsafe` and does not import any other
//! crate module — it is intentionally foundational and standalone.

// ===========================================================================
// Version
// ===========================================================================
//
// Mirrors the version macros in `zlib.h` (lines 44-49). These items are the
// single source of truth consumed by `util/version.rs` (which backs the public
// `zlibVersion()` / `zlibCompileFlags()` helpers) and by the FFI shim.

/// `ZLIB_VERSION` — the human-readable version string returned by
/// `zlibVersion()`.
///
/// Applications compare this against the value they were compiled with to
/// detect a mismatched shared library. The trailing `-motley` tag matches the
/// upstream baseline this crate was transformed from.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// `ZLIB_VERNUM` — the packed numeric version `0xMNRS` (major, minor, revision,
/// sub-revision nibbles): `0x1321` for version `1.3.2.1`.
pub const ZLIB_VERNUM: i32 = 0x1321;

/// `ZLIB_VER_MAJOR` — the major version component (`1`).
pub const ZLIB_VER_MAJOR: i32 = 1;

/// `ZLIB_VER_MINOR` — the minor version component (`3`).
pub const ZLIB_VER_MINOR: i32 = 3;

/// `ZLIB_VER_REVISION` — the revision version component (`2`).
pub const ZLIB_VER_REVISION: i32 = 2;

/// `ZLIB_VER_SUBREVISION` — the sub-revision version component (`1`).
pub const ZLIB_VER_SUBREVISION: i32 = 1;

// ===========================================================================
// Flush modes (`zlib.h` lines 172-178)
// ===========================================================================

/// Flush mode passed to `deflate` and `inflate`.
///
/// Mirrors the C `Z_*_FLUSH` family. The `#[repr(i32)]` discriminants are
/// byte-identical to the C `#define`s so the FFI shim can convert with a plain
/// `as i32` cast. The [`Default`] is [`Flush::NoFlush`] (`0`), matching the
/// usual `deflate`/`inflate` calling convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum Flush {
    /// `Z_NO_FLUSH` (0) — allow the codec to accumulate input before producing
    /// output; the normal mode for streaming.
    #[default]
    NoFlush = 0,
    /// `Z_PARTIAL_FLUSH` (1) — flush pending output without resetting state.
    PartialFlush = 1,
    /// `Z_SYNC_FLUSH` (2) — flush to a byte boundary and emit an empty stored
    /// block so all input so far is available to the decompressor.
    SyncFlush = 2,
    /// `Z_FULL_FLUSH` (3) — like [`Flush::SyncFlush`] but also reset the
    /// compression state so decompression can restart from this point.
    FullFlush = 3,
    /// `Z_FINISH` (4) — no more input will be provided; finish the stream.
    Finish = 4,
    /// `Z_BLOCK` (5) — complete and emit the current block without aligning to
    /// a byte boundary.
    Block = 5,
    /// `Z_TREES` (6) — like [`Flush::Block`] but also stop at the end of each
    /// block's deflate header.
    Trees = 6,
}

impl Flush {
    /// Return the C integer value (`Z_*_FLUSH`) for this flush mode.
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Convert a raw C integer into a [`Flush`], returning [`None`] for any
    /// value outside the documented `0..=6` range.
    #[inline]
    #[must_use]
    pub const fn try_from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::NoFlush),
            1 => Some(Self::PartialFlush),
            2 => Some(Self::SyncFlush),
            3 => Some(Self::FullFlush),
            4 => Some(Self::Finish),
            5 => Some(Self::Block),
            6 => Some(Self::Trees),
            _ => None,
        }
    }
}

impl TryFrom<i32> for Flush {
    /// The rejected integer is returned verbatim as the error value.
    type Error = i32;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::try_from_i32(value).ok_or(value)
    }
}

// ===========================================================================
// Compression strategy (`zlib.h` lines 200-204)
// ===========================================================================

/// Compression strategy passed to `deflateInit2` / `deflateParams`.
///
/// Mirrors the C `Z_*` strategy constants. The strategy tunes the trade-off
/// between Huffman coding and LZ77 string matching but never affects format
/// correctness. The [`Default`] is [`Strategy::Default`] (`Z_DEFAULT_STRATEGY`,
/// `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum Strategy {
    /// `Z_DEFAULT_STRATEGY` (0) — normal data; balanced matching and Huffman
    /// coding.
    #[default]
    Default = 0,
    /// `Z_FILTERED` (1) — data produced by a filter/predictor; forces more
    /// Huffman coding and less string matching.
    Filtered = 1,
    /// `Z_HUFFMAN_ONLY` (2) — Huffman coding only; no string matching.
    HuffmanOnly = 2,
    /// `Z_RLE` (3) — limit match distances to one (run-length encoding).
    Rle = 3,
    /// `Z_FIXED` (4) — default matching but no dynamic Huffman trees.
    Fixed = 4,
}

impl Strategy {
    /// Return the C integer value (`Z_*`) for this strategy.
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Convert a raw C integer into a [`Strategy`], returning [`None`] for any
    /// value outside the documented `0..=4` range.
    #[inline]
    #[must_use]
    pub const fn try_from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Default),
            1 => Some(Self::Filtered),
            2 => Some(Self::HuffmanOnly),
            3 => Some(Self::Rle),
            4 => Some(Self::Fixed),
            _ => None,
        }
    }
}

impl TryFrom<i32> for Strategy {
    /// The rejected integer is returned verbatim as the error value.
    type Error = i32;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::try_from_i32(value).ok_or(value)
    }
}

// ===========================================================================
// Data-type hint (`zlib.h` lines 207-210)
// ===========================================================================

/// The `data_type` field hint reported on a `z_stream`.
///
/// Mirrors the C `Z_BINARY` / `Z_TEXT` / `Z_UNKNOWN` constants. The legacy name
/// `Z_ASCII` is preserved as the [`Z_ASCII`] alias constant (it equals
/// [`DataType::Text`]). The [`Default`] is [`DataType::Unknown`] (`2`), matching
/// the value a freshly initialized `z_stream` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum DataType {
    /// `Z_BINARY` (0) — the data is (or is assumed to be) binary.
    Binary = 0,
    /// `Z_TEXT` (1) — the data is (or is assumed to be) text. Also reachable
    /// through the legacy [`Z_ASCII`] alias.
    Text = 1,
    /// `Z_UNKNOWN` (2) — the data type has not been determined.
    #[default]
    Unknown = 2,
}

impl DataType {
    /// Return the C integer value (`Z_BINARY` / `Z_TEXT` / `Z_UNKNOWN`).
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Convert a raw C integer into a [`DataType`], returning [`None`] for any
    /// value outside the documented `0..=2` range.
    #[inline]
    #[must_use]
    pub const fn try_from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Binary),
            1 => Some(Self::Text),
            2 => Some(Self::Unknown),
            _ => None,
        }
    }
}

impl TryFrom<i32> for DataType {
    /// The rejected integer is returned verbatim as the error value.
    type Error = i32;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::try_from_i32(value).ok_or(value)
    }
}

/// `Z_ASCII` — backwards-compatible alias for [`DataType::Text`] (value `1`),
/// retained for source compatibility with zlib 1.2.2 and earlier.
pub const Z_ASCII: DataType = DataType::Text;

// ===========================================================================
// Compression levels (`zlib.h` lines 194-197)
// ===========================================================================
//
// The level is an `i32` in the inclusive range `-1..=9`. The four named values
// below are kept as bare `i32` consts so the FFI shim and call sites can use
// them directly; the [`Level`] newtype offers an optional validated wrapper.

/// `Z_NO_COMPRESSION` (0) — store only; no compression is performed.
pub const Z_NO_COMPRESSION: i32 = 0;

/// `Z_BEST_SPEED` (1) — fastest compression, largest output.
pub const Z_BEST_SPEED: i32 = 1;

/// `Z_BEST_COMPRESSION` (9) — slowest compression, smallest output.
pub const Z_BEST_COMPRESSION: i32 = 9;

/// `Z_DEFAULT_COMPRESSION` (-1) — request the library default (level 6).
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

/// A validated DEFLATE compression level in the inclusive range `-1..=9`.
///
/// This newtype is an optional, ergonomic wrapper over the bare `Z_*`
/// compression-level constants. `-1` ([`Level::DEFAULT`]) requests the library
/// default, `0` disables compression, and `1..=9` trade speed for ratio. The
/// bare `i32` consts remain the canonical ABI values; this type simply makes a
/// validated level un-representable when out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Level(i32);

impl Level {
    /// No compression — level `0` ([`Z_NO_COMPRESSION`]).
    pub const NO_COMPRESSION: Self = Self(Z_NO_COMPRESSION);
    /// Fastest compression — level `1` ([`Z_BEST_SPEED`]).
    pub const BEST_SPEED: Self = Self(Z_BEST_SPEED);
    /// Best compression ratio — level `9` ([`Z_BEST_COMPRESSION`]).
    pub const BEST_COMPRESSION: Self = Self(Z_BEST_COMPRESSION);
    /// The library default — level `-1` ([`Z_DEFAULT_COMPRESSION`]).
    pub const DEFAULT: Self = Self(Z_DEFAULT_COMPRESSION);

    /// Construct a [`Level`] from a raw integer, returning [`None`] unless the
    /// value is in the valid inclusive range `-1..=9`.
    #[inline]
    #[must_use]
    pub const fn new(level: i32) -> Option<Self> {
        match level {
            -1..=9 => Some(Self(level)),
            _ => None,
        }
    }

    /// Return the underlying integer level (`-1..=9`).
    #[inline]
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl Default for Level {
    /// The default level is [`Level::DEFAULT`] (`Z_DEFAULT_COMPRESSION`, `-1`).
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ===========================================================================
// Compression method and the null sentinel (`zlib.h` lines 213, 216)
// ===========================================================================

/// `Z_DEFLATED` (8) — the only compression method defined by zlib.
pub const Z_DEFLATED: i32 = 8;

/// `Z_NULL` (0) — sentinel used to initialize `zalloc`, `zfree`, and `opaque`,
/// and to indicate absent optional pointers.
pub const Z_NULL: i32 = 0;

// ===========================================================================
// DEFLATE block types (`zutil.h` lines 87-89)
// ===========================================================================

/// `STORED_BLOCK` (0) — an uncompressed (stored) DEFLATE block.
pub const STORED_BLOCK: i32 = 0;

/// `STATIC_TREES` (1) — a DEFLATE block using the fixed (static) Huffman codes.
pub const STATIC_TREES: i32 = 1;

/// `DYN_TREES` (2) — a DEFLATE block using dynamic Huffman codes.
pub const DYN_TREES: i32 = 2;

// ===========================================================================
// Match-length and window limits (`zutil.h` lines 92-96, `zconf.h`)
// ===========================================================================

/// `MIN_MATCH` (3) — the shortest LZ77 match length DEFLATE can encode.
///
/// Expressed as `usize` because it is used pervasively as a length and as an
/// array dimension throughout the compressor.
pub const MIN_MATCH: usize = 3;

/// `MAX_MATCH` (258) — the longest LZ77 match length DEFLATE can encode.
///
/// Expressed as `usize` for the same reason as [`MIN_MATCH`]; note that
/// `MAX_MATCH - MIN_MATCH + 1 == 256` sizes the length-code lookup table.
pub const MAX_MATCH: usize = 258;

/// `MAX_WBITS` (15) — the maximum window-size exponent, i.e. a 32 KiB
/// (`1 << 15`) LZ77 sliding window.
pub const MAX_WBITS: i32 = 15;

/// `DEF_WBITS` (15) — the default `windowBits` for decompression; equal to
/// [`MAX_WBITS`].
pub const DEF_WBITS: i32 = MAX_WBITS;

/// `DEF_MEM_LEVEL` (8) — the default `memLevel` for `deflateInit2`.
pub const DEF_MEM_LEVEL: i32 = 8;

/// `MAX_MEM_LEVEL` (9) — the maximum `memLevel` for `deflateInit2` on modern
/// (non-16-bit) platforms.
pub const MAX_MEM_LEVEL: i32 = 9;

/// `PRESET_DICT` (0x20) — the preset-dictionary flag bit (FDICT) in the zlib
/// header's FLG byte.
pub const PRESET_DICT: i32 = 0x20;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_discriminants_match_c() {
        assert_eq!(Flush::NoFlush as i32, 0);
        assert_eq!(Flush::PartialFlush as i32, 1);
        assert_eq!(Flush::SyncFlush as i32, 2);
        assert_eq!(Flush::FullFlush as i32, 3);
        assert_eq!(Flush::Finish as i32, 4);
        assert_eq!(Flush::Block as i32, 5);
        assert_eq!(Flush::Trees as i32, 6);
        // `as_i32` must agree with the raw cast.
        assert_eq!(Flush::Finish.as_i32(), 4);
    }

    #[test]
    fn strategy_discriminants_match_c() {
        assert_eq!(Strategy::Default as i32, 0);
        assert_eq!(Strategy::Filtered as i32, 1);
        assert_eq!(Strategy::HuffmanOnly as i32, 2);
        assert_eq!(Strategy::Rle as i32, 3);
        assert_eq!(Strategy::Fixed as i32, 4);
        assert_eq!(Strategy::Rle.as_i32(), 3);
    }

    #[test]
    fn data_type_discriminants_match_c() {
        assert_eq!(DataType::Binary as i32, 0);
        assert_eq!(DataType::Text as i32, 1);
        assert_eq!(DataType::Unknown as i32, 2);
        // `Z_ASCII` is an intentional legacy alias for `Z_TEXT`.
        assert_eq!(Z_ASCII, DataType::Text);
        assert_eq!(Z_ASCII as i32, 1);
    }

    #[test]
    fn enum_defaults() {
        assert_eq!(Flush::default(), Flush::NoFlush);
        assert_eq!(Strategy::default(), Strategy::Default);
        assert_eq!(DataType::default(), DataType::Unknown);
        assert_eq!(Level::default(), Level::DEFAULT);
    }

    #[test]
    fn flush_try_from() {
        assert_eq!(Flush::try_from_i32(0), Some(Flush::NoFlush));
        assert_eq!(Flush::try_from_i32(6), Some(Flush::Trees));
        assert_eq!(Flush::try_from_i32(7), None);
        assert_eq!(Flush::try_from_i32(-1), None);
        // `TryFrom` returns the rejected value as the error.
        assert_eq!(Flush::try_from(4), Ok(Flush::Finish));
        assert_eq!(Flush::try_from(7), Err(7));
    }

    #[test]
    fn strategy_try_from() {
        assert_eq!(Strategy::try_from_i32(0), Some(Strategy::Default));
        assert_eq!(Strategy::try_from_i32(4), Some(Strategy::Fixed));
        assert!(Strategy::try_from_i32(99).is_none());
        assert_eq!(Strategy::try_from(3), Ok(Strategy::Rle));
        assert_eq!(Strategy::try_from(5), Err(5));
    }

    #[test]
    fn data_type_try_from() {
        assert_eq!(DataType::try_from_i32(0), Some(DataType::Binary));
        assert_eq!(DataType::try_from_i32(2), Some(DataType::Unknown));
        assert!(DataType::try_from_i32(3).is_none());
        assert_eq!(DataType::try_from(1), Ok(DataType::Text));
        assert_eq!(DataType::try_from(-5), Err(-5));
    }

    #[test]
    fn compression_level_constants() {
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);
    }

    #[test]
    fn level_newtype_validation() {
        assert_eq!(Level::new(-1), Some(Level::DEFAULT));
        assert_eq!(Level::new(0), Some(Level::NO_COMPRESSION));
        assert_eq!(Level::new(1), Some(Level::BEST_SPEED));
        assert_eq!(Level::new(9), Some(Level::BEST_COMPRESSION));
        // Out-of-range values are rejected.
        assert_eq!(Level::new(-2), None);
        assert_eq!(Level::new(10), None);
        // Round-trip through `get`.
        assert_eq!(Level::new(6).map(Level::get), Some(6));
        assert_eq!(Level::DEFAULT.get(), -1);
    }

    #[test]
    fn method_and_null() {
        assert_eq!(Z_DEFLATED, 8);
        assert_eq!(Z_NULL, 0);
    }

    #[test]
    fn block_types() {
        assert_eq!(STORED_BLOCK, 0);
        assert_eq!(STATIC_TREES, 1);
        assert_eq!(DYN_TREES, 2);
    }

    #[test]
    fn match_and_window_limits() {
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(MAX_MATCH, 258);
        // The length-code lookup table is sized by this difference.
        assert_eq!(MAX_MATCH - MIN_MATCH + 1, 256);
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(DEF_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(DEF_MEM_LEVEL, 8);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(PRESET_DICT, 0x20);
    }

    #[test]
    fn version_constants() {
        assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(ZLIB_VER_MAJOR, 1);
        assert_eq!(ZLIB_VER_MINOR, 3);
        assert_eq!(ZLIB_VER_REVISION, 2);
        assert_eq!(ZLIB_VER_SUBREVISION, 1);
    }
}
