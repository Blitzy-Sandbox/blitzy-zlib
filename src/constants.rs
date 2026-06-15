//! Public zlib constants — the canonical, bit-exact contract layer of `zlib_rs`.
//!
//! This module is the single source of truth for every public numeric constant
//! defined by the C zlib API. Each value is reproduced **exactly** as it appears
//! in the upstream C headers (`zlib.h`, `zconf.h`, `zutil.h`), because the
//! on-the-wire DEFLATE/zlib/gzip formats and the C ABI both depend on these
//! values being byte- and bit-identical to canonical zlib. They are therefore
//! **frozen**: they must never be "modernized" or otherwise altered.
//!
//! # Organization
//!
//! * **Raw integer constants** (`pub const … : i32`) mirror the C `#define`s
//!   one-for-one and are what the `crate::ffi` shim and direct numeric callers
//!   use. They are declared as [`i32`] to match C `int`; the FFI layer casts to
//!   `core::ffi::c_int` (which is `i32` on every platform zlib targets).
//! * **Idiomatic `#[repr(i32)]` enums** ([`FlushMode`], [`Strategy`],
//!   [`DataType`]) provide exhaustive-`match` ergonomics for the safe core.
//!   Their discriminants are identical to the corresponding raw constants, so
//!   converting between the two representations is lossless: `enum as i32`
//!   yields the raw value and [`TryFrom<i32>`] is its checked inverse.
//!
//! # `no_std`
//!
//! This module is `no_std`-clean: it references only [`core`] (never `std`), so
//! it compiles unchanged under the crate's `no-std` feature. It deliberately
//! does not carry a crate-level `#![no_std]` attribute — that belongs to the
//! crate root (`src/lib.rs`), which gates it behind the feature.
//!
//! # Source mapping
//!
//! | Group                  | Upstream origin        |
//! |------------------------|------------------------|
//! | Version                | `zlib.h` L44-49        |
//! | Flush modes            | `zlib.h` L172-178      |
//! | Return codes           | `zlib.h` L181-189      |
//! | Compression levels     | `zlib.h` L194-197      |
//! | Strategies             | `zlib.h` L200-204      |
//! | Data types             | `zlib.h` L207-210      |
//! | Method / null sentinel | `zlib.h` L213-216      |
//! | Window / memory limits | `zconf.h` + `zutil.h`  |
//! | Seek whence            | `zconf.h` L513-515     |

// ===========================================================================
// Phase A — Library version (zlib.h L44-49)
// ===========================================================================

/// Human-readable zlib version string, matching the C `ZLIB_VERSION` macro.
///
/// Held here as a Rust string slice; the `crate::ffi` layer is responsible for
/// materializing the NUL-terminated C string (`b"1.3.2.1-motley\0"`) expected
/// by `zlibVersion()` at the FFI boundary.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// Numeric version in `0xMMNRS0`-style packing (major/minor/revision/sub),
/// equal to the C `ZLIB_VERNUM` macro.
pub const ZLIB_VERNUM: i32 = 0x1321;

/// Major version component (the `1` in `1.3.2.1`).
pub const ZLIB_VER_MAJOR: i32 = 1;

/// Minor version component (the `3` in `1.3.2.1`).
pub const ZLIB_VER_MINOR: i32 = 3;

/// Revision version component (the `2` in `1.3.2.1`).
pub const ZLIB_VER_REVISION: i32 = 2;

/// Sub-revision version component (the `1` in `1.3.2.1`).
pub const ZLIB_VER_SUBREVISION: i32 = 1;

// ===========================================================================
// Phase B — Flush modes (zlib.h L172-178)
// ===========================================================================
// Allowed `flush` values; see `deflate()` and `inflate()` for the precise
// semantics of each mode.

/// No flushing: allow `deflate` to decide how much output to accumulate before
/// producing it, maximizing compression (`Z_NO_FLUSH`).
pub const Z_NO_FLUSH: i32 = 0;

/// Flush all pending output and align the output to a byte boundary, without
/// resetting the compression dictionary (`Z_PARTIAL_FLUSH`).
pub const Z_PARTIAL_FLUSH: i32 = 1;

/// Flush all pending output and align to a byte boundary by emitting an empty
/// stored block, leaving the compression state intact (`Z_SYNC_FLUSH`).
pub const Z_SYNC_FLUSH: i32 = 2;

/// Like [`Z_SYNC_FLUSH`], but also reset the compression dictionary so that the
/// stream can be decompressed independently from this point onward, at some
/// cost to compression ratio (`Z_FULL_FLUSH`).
pub const Z_FULL_FLUSH: i32 = 3;

/// Finish the stream: signals that no further input will be supplied and the
/// remaining output (including the trailer) should be produced (`Z_FINISH`).
pub const Z_FINISH: i32 = 4;

/// Return control to the caller at the next deflate block boundary, and on the
/// inflate side stop at block boundaries for fine-grained control (`Z_BLOCK`).
pub const Z_BLOCK: i32 = 5;

/// Like [`Z_BLOCK`], but on inflate also return at the end of each block header,
/// before the block data, so the caller can inspect the Huffman trees
/// (`Z_TREES`).
pub const Z_TREES: i32 = 6;

// ===========================================================================
// Phase C — Return / error codes (zlib.h L181-189)
// ===========================================================================
// Negative values denote errors; non-negative values denote special but normal
// events.

/// Operation completed successfully (`Z_OK`).
pub const Z_OK: i32 = 0;

/// The end of the compressed stream has been reached (`Z_STREAM_END`).
pub const Z_STREAM_END: i32 = 1;

/// A preset dictionary is required before decompression can continue; see the
/// `inflateSetDictionary` handshake (`Z_NEED_DICT`).
pub const Z_NEED_DICT: i32 = 2;

/// A file-system or `errno`-style error occurred during a gzip file operation
/// (`Z_ERRNO`).
pub const Z_ERRNO: i32 = -1;

/// The stream state was inconsistent, or one or more parameters were invalid
/// (`Z_STREAM_ERROR`).
pub const Z_STREAM_ERROR: i32 = -2;

/// The input data was corrupted or did not conform to the expected format
/// (`Z_DATA_ERROR`).
pub const Z_DATA_ERROR: i32 = -3;

/// Memory could not be allocated (`Z_MEM_ERROR`).
pub const Z_MEM_ERROR: i32 = -4;

/// No progress is possible: the call needs more input or more output room to
/// continue (`Z_BUF_ERROR`). This is a recoverable, non-fatal condition.
pub const Z_BUF_ERROR: i32 = -5;

/// The version of zlib the caller compiled against is incompatible with the
/// version of this library (`Z_VERSION_ERROR`).
pub const Z_VERSION_ERROR: i32 = -6;

// ===========================================================================
// Phase D — Compression levels (zlib.h L194-197)
// ===========================================================================

/// Store the data only; perform no compression at all (`Z_NO_COMPRESSION`,
/// level 0).
pub const Z_NO_COMPRESSION: i32 = 0;

/// Fastest compression with the least effort (`Z_BEST_SPEED`, level 1).
pub const Z_BEST_SPEED: i32 = 1;

/// Slowest compression producing the smallest output (`Z_BEST_COMPRESSION`,
/// level 9).
pub const Z_BEST_COMPRESSION: i32 = 9;

/// Sentinel requesting the library's default compression level
/// (`Z_DEFAULT_COMPRESSION`). It resolves to level 6; see [`resolve_level`].
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

// ===========================================================================
// Phase E — Compression strategies (zlib.h L200-204)
// ===========================================================================
// Strategy tunes the trade-off between match-finding and Huffman coding; it
// affects compression ratio and speed but never correctness of the output.

/// Bias toward filtered data — data produced by a filter (or predictor),
/// consisting of mostly small values with a fairly random distribution. Forces
/// more Huffman coding and less string matching (`Z_FILTERED`).
pub const Z_FILTERED: i32 = 1;

/// Force Huffman encoding only, disabling string matching entirely
/// (`Z_HUFFMAN_ONLY`).
pub const Z_HUFFMAN_ONLY: i32 = 2;

/// Limit match distances to one to favor run-length encoding; designed to be
/// almost as fast as [`Z_HUFFMAN_ONLY`] but with better compression for
/// PNG-style image data (`Z_RLE`).
pub const Z_RLE: i32 = 3;

/// Prevent the use of dynamic Huffman codes, forcing fixed codes for simpler,
/// more predictable decoding (`Z_FIXED`).
pub const Z_FIXED: i32 = 4;

/// The default strategy, appropriate for ordinary data (`Z_DEFAULT_STRATEGY`).
pub const Z_DEFAULT_STRATEGY: i32 = 0;

// ===========================================================================
// Phase F — Data types (zlib.h L207-210)
// ===========================================================================
// Possible values of the `data_type` field that `deflate()` reports to describe
// the most recently processed input.

/// The data appears to be binary (`Z_BINARY`).
pub const Z_BINARY: i32 = 0;

/// The data appears to be text (`Z_TEXT`).
pub const Z_TEXT: i32 = 1;

/// Historical alias of [`Z_TEXT`], retained for compatibility with zlib 1.2.2
/// and earlier (`Z_ASCII`). It carries the same numeric value as [`Z_TEXT`].
pub const Z_ASCII: i32 = Z_TEXT;

/// The data type could not be determined (`Z_UNKNOWN`).
pub const Z_UNKNOWN: i32 = 2;

// ===========================================================================
// Phase G — Compression method + null sentinel (zlib.h L213-216)
// ===========================================================================

/// The DEFLATE compression method — the only method supported by this library
/// (`Z_DEFLATED`).
pub const Z_DEFLATED: i32 = 8;

/// Null sentinel used to initialize the `zalloc`, `zfree`, and `opaque`
/// allocator hooks of a `z_stream` at the FFI boundary (`Z_NULL`).
pub const Z_NULL: i32 = 0;

// ===========================================================================
// Phase H — Window / memory limits (zconf.h + zutil.h) and seek whence
// ===========================================================================

/// Maximum `windowBits`: the base-2 logarithm of the largest LZ77 window size,
/// i.e. a 32 KiB window (`MAX_WBITS`, `zconf.h`).
pub const MAX_WBITS: i32 = 15;

/// Maximum `memLevel` accepted by `deflateInit2` on the default (non
/// 64K-segment) build (`MAX_MEM_LEVEL`, `zconf.h`).
pub const MAX_MEM_LEVEL: i32 = 9;

/// Default `windowBits` used for decompression — equal to [`MAX_WBITS`]
/// (`DEF_WBITS`, `zutil.h`). `MAX_WBITS` itself is only relevant to
/// compression.
pub const DEF_WBITS: i32 = MAX_WBITS;

/// Default `memLevel` used for compression (`DEF_MEM_LEVEL`, `zutil.h`). It is
/// `8` even though [`MAX_MEM_LEVEL`] is `9`, matching the upstream default
/// desktop build (256 KiB total deflate footprint at the default window).
pub const DEF_MEM_LEVEL: i32 = 8;

/// `whence` value meaning "seek from the beginning of the file" (`SEEK_SET`,
/// `zconf.h`). Used by the gzip `gzseek` FFI shim; idiomatic Rust callers
/// should prefer `std::io::SeekFrom`.
pub const SEEK_SET: i32 = 0;

/// `whence` value meaning "seek from the current file position" (`SEEK_CUR`,
/// `zconf.h`).
pub const SEEK_CUR: i32 = 1;

/// `whence` value meaning "seek relative to the end of the file" (`SEEK_END`,
/// `zconf.h`).
pub const SEEK_END: i32 = 2;

// ===========================================================================
// Phase I — Idiomatic enums, conversions, and helpers
// ===========================================================================
//
// The raw `pub const` values above are what the FFI layer and direct numeric
// callers use. For the safe Rust core, the following `#[repr(i32)]` enums give
// exhaustive-`match` ergonomics while keeping each discriminant identical to
// the corresponding C value, so the two representations always agree.

/// Error returned when an [`i32`] does not correspond to any variant of one of
/// this module's idiomatic enums ([`FlushMode`], [`Strategy`], [`DataType`]).
///
/// The wrapped value is the offending integer, preserved for diagnostics. This
/// type is `no_std`-compatible and implements both [`core::fmt::Display`] and
/// [`core::error::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidConstant(pub i32);

impl core::fmt::Display for InvalidConstant {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid zlib constant value: {}", self.0)
    }
}

impl core::error::Error for InvalidConstant {}

/// Flush behavior requested of `deflate`/`inflate`, mirroring the `Z_*_FLUSH`
/// family of constants ([`Z_NO_FLUSH`] through [`Z_TREES`]).
///
/// The `#[repr(i32)]` attribute guarantees that each variant's discriminant is
/// exactly the matching raw constant, so `mode as i32` is an exact conversion
/// and [`FlushMode::try_from`] is its checked inverse.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushMode {
    /// No flushing; see [`Z_NO_FLUSH`].
    NoFlush = 0,
    /// Partial flush; see [`Z_PARTIAL_FLUSH`].
    PartialFlush = 1,
    /// Sync flush; see [`Z_SYNC_FLUSH`].
    SyncFlush = 2,
    /// Full flush; see [`Z_FULL_FLUSH`].
    FullFlush = 3,
    /// Finish the stream; see [`Z_FINISH`].
    Finish = 4,
    /// Stop at a block boundary; see [`Z_BLOCK`].
    Block = 5,
    /// Stop at a block header; see [`Z_TREES`].
    Trees = 6,
}

impl TryFrom<i32> for FlushMode {
    type Error = InvalidConstant;

    /// Converts a raw flush constant into a [`FlushMode`], returning
    /// [`InvalidConstant`] if the value is not one of `0..=6`.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            Z_NO_FLUSH => Ok(Self::NoFlush),
            Z_PARTIAL_FLUSH => Ok(Self::PartialFlush),
            Z_SYNC_FLUSH => Ok(Self::SyncFlush),
            Z_FULL_FLUSH => Ok(Self::FullFlush),
            Z_FINISH => Ok(Self::Finish),
            Z_BLOCK => Ok(Self::Block),
            Z_TREES => Ok(Self::Trees),
            other => Err(InvalidConstant(other)),
        }
    }
}

impl From<FlushMode> for i32 {
    /// Returns the raw flush constant backing this [`FlushMode`].
    fn from(value: FlushMode) -> Self {
        value as Self
    }
}

/// Compression strategy passed to `deflateInit2`, mirroring the `Z_*` strategy
/// family ([`Z_DEFAULT_STRATEGY`], [`Z_FILTERED`], [`Z_HUFFMAN_ONLY`],
/// [`Z_RLE`], [`Z_FIXED`]).
///
/// As with [`FlushMode`], `strategy as i32` yields the raw constant and
/// [`Strategy::try_from`] is its checked inverse.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Default strategy; see [`Z_DEFAULT_STRATEGY`].
    Default = 0,
    /// Filtered-data strategy; see [`Z_FILTERED`].
    Filtered = 1,
    /// Huffman-only strategy; see [`Z_HUFFMAN_ONLY`].
    HuffmanOnly = 2,
    /// Run-length-encoding strategy; see [`Z_RLE`].
    Rle = 3,
    /// Fixed-Huffman strategy; see [`Z_FIXED`].
    Fixed = 4,
}

impl TryFrom<i32> for Strategy {
    type Error = InvalidConstant;

    /// Converts a raw strategy constant into a [`Strategy`], returning
    /// [`InvalidConstant`] if the value is not one of `0..=4`.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            Z_DEFAULT_STRATEGY => Ok(Self::Default),
            Z_FILTERED => Ok(Self::Filtered),
            Z_HUFFMAN_ONLY => Ok(Self::HuffmanOnly),
            Z_RLE => Ok(Self::Rle),
            Z_FIXED => Ok(Self::Fixed),
            other => Err(InvalidConstant(other)),
        }
    }
}

impl From<Strategy> for i32 {
    /// Returns the raw strategy constant backing this [`Strategy`].
    fn from(value: Strategy) -> Self {
        value as Self
    }
}

/// Classification reported in the `data_type` field of a deflate stream,
/// mirroring [`Z_BINARY`], [`Z_TEXT`], and [`Z_UNKNOWN`].
///
/// Note that [`Z_ASCII`] is a historical alias of [`Z_TEXT`] and therefore maps
/// to the same [`DataType::Text`] variant.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// Binary data; see [`Z_BINARY`].
    Binary = 0,
    /// Text data; see [`Z_TEXT`] (and its alias [`Z_ASCII`]).
    Text = 1,
    /// Indeterminate data type; see [`Z_UNKNOWN`].
    Unknown = 2,
}

impl TryFrom<i32> for DataType {
    type Error = InvalidConstant;

    /// Converts a raw data-type constant into a [`DataType`], returning
    /// [`InvalidConstant`] if the value is not one of `0..=2`.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            Z_BINARY => Ok(Self::Binary),
            Z_TEXT => Ok(Self::Text),
            Z_UNKNOWN => Ok(Self::Unknown),
            other => Err(InvalidConstant(other)),
        }
    }
}

impl From<DataType> for i32 {
    /// Returns the raw data-type constant backing this [`DataType`].
    fn from(value: DataType) -> Self {
        value as Self
    }
}

/// Resolves a requested compression level to its effective numeric level.
///
/// [`Z_DEFAULT_COMPRESSION`] (`-1`) maps to level `6`, matching `deflate.c`;
/// every other value is returned unchanged. This is a `const fn` so it can be
/// used in const contexts, such as constructing the per-level configuration
/// table in the deflate engine. For example, `resolve_level(Z_DEFAULT_COMPRESSION)`
/// is `6` while `resolve_level(9)` is `9`.
#[must_use]
pub const fn resolve_level(level: i32) -> i32 {
    if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    }
}

// ===========================================================================
// Tests — assert every value is bit-exact with the C sources and that the
// idiomatic enums agree with the raw constants in both directions.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constants_match_c() {
        assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
        assert_eq!(ZLIB_VERNUM, 0x1321);
        assert_eq!(ZLIB_VER_MAJOR, 1);
        assert_eq!(ZLIB_VER_MINOR, 3);
        assert_eq!(ZLIB_VER_REVISION, 2);
        assert_eq!(ZLIB_VER_SUBREVISION, 1);
    }

    #[test]
    fn flush_constants_match_c() {
        assert_eq!(Z_NO_FLUSH, 0);
        assert_eq!(Z_PARTIAL_FLUSH, 1);
        assert_eq!(Z_SYNC_FLUSH, 2);
        assert_eq!(Z_FULL_FLUSH, 3);
        assert_eq!(Z_FINISH, 4);
        assert_eq!(Z_BLOCK, 5);
        assert_eq!(Z_TREES, 6);
    }

    #[test]
    fn return_code_constants_match_c() {
        assert_eq!(Z_OK, 0);
        assert_eq!(Z_STREAM_END, 1);
        assert_eq!(Z_NEED_DICT, 2);
        assert_eq!(Z_ERRNO, -1);
        assert_eq!(Z_STREAM_ERROR, -2);
        assert_eq!(Z_DATA_ERROR, -3);
        assert_eq!(Z_MEM_ERROR, -4);
        assert_eq!(Z_BUF_ERROR, -5);
        assert_eq!(Z_VERSION_ERROR, -6);
    }

    #[test]
    fn level_constants_match_c() {
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);
    }

    #[test]
    fn strategy_constants_match_c() {
        assert_eq!(Z_FILTERED, 1);
        assert_eq!(Z_HUFFMAN_ONLY, 2);
        assert_eq!(Z_RLE, 3);
        assert_eq!(Z_FIXED, 4);
        assert_eq!(Z_DEFAULT_STRATEGY, 0);
    }

    #[test]
    fn data_type_constants_match_c() {
        assert_eq!(Z_BINARY, 0);
        assert_eq!(Z_TEXT, 1);
        assert_eq!(Z_ASCII, Z_TEXT);
        assert_eq!(Z_ASCII, 1);
        assert_eq!(Z_UNKNOWN, 2);
    }

    #[test]
    fn method_and_null_match_c() {
        assert_eq!(Z_DEFLATED, 8);
        assert_eq!(Z_NULL, 0);
    }

    #[test]
    fn limit_constants_match_c() {
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(DEF_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(DEF_MEM_LEVEL, 8);
        assert_eq!(SEEK_SET, 0);
        assert_eq!(SEEK_CUR, 1);
        assert_eq!(SEEK_END, 2);
    }

    #[test]
    fn flush_mode_enum_matches_constants() {
        assert_eq!(FlushMode::NoFlush as i32, Z_NO_FLUSH);
        assert_eq!(FlushMode::PartialFlush as i32, Z_PARTIAL_FLUSH);
        assert_eq!(FlushMode::SyncFlush as i32, Z_SYNC_FLUSH);
        assert_eq!(FlushMode::FullFlush as i32, Z_FULL_FLUSH);
        assert_eq!(FlushMode::Finish as i32, Z_FINISH);
        assert_eq!(FlushMode::Block as i32, Z_BLOCK);
        assert_eq!(FlushMode::Trees as i32, Z_TREES);
    }

    #[test]
    fn flush_mode_try_from_round_trips() {
        for raw in Z_NO_FLUSH..=Z_TREES {
            let mode = FlushMode::try_from(raw).expect("0..=6 are all valid flush modes");
            assert_eq!(mode as i32, raw);
            assert_eq!(i32::from(mode), raw);
        }
        assert_eq!(FlushMode::try_from(7), Err(InvalidConstant(7)));
        assert_eq!(FlushMode::try_from(-1), Err(InvalidConstant(-1)));
    }

    #[test]
    fn strategy_enum_matches_constants() {
        assert_eq!(Strategy::Default as i32, Z_DEFAULT_STRATEGY);
        assert_eq!(Strategy::Filtered as i32, Z_FILTERED);
        assert_eq!(Strategy::HuffmanOnly as i32, Z_HUFFMAN_ONLY);
        assert_eq!(Strategy::Rle as i32, Z_RLE);
        assert_eq!(Strategy::Fixed as i32, Z_FIXED);
    }

    #[test]
    fn strategy_try_from_round_trips() {
        for raw in Z_DEFAULT_STRATEGY..=Z_FIXED {
            let strategy = Strategy::try_from(raw).expect("0..=4 are all valid strategies");
            assert_eq!(i32::from(strategy), raw);
        }
        assert_eq!(Strategy::try_from(5), Err(InvalidConstant(5)));
        assert_eq!(Strategy::try_from(-1), Err(InvalidConstant(-1)));
    }

    #[test]
    fn data_type_enum_matches_constants() {
        assert_eq!(DataType::Binary as i32, Z_BINARY);
        assert_eq!(DataType::Text as i32, Z_TEXT);
        assert_eq!(DataType::Text as i32, Z_ASCII);
        assert_eq!(DataType::Unknown as i32, Z_UNKNOWN);
    }

    #[test]
    fn data_type_try_from_round_trips() {
        assert_eq!(DataType::try_from(Z_BINARY), Ok(DataType::Binary));
        assert_eq!(DataType::try_from(Z_TEXT), Ok(DataType::Text));
        assert_eq!(DataType::try_from(Z_UNKNOWN), Ok(DataType::Unknown));
        assert_eq!(DataType::try_from(3), Err(InvalidConstant(3)));
    }

    #[test]
    fn resolve_level_maps_default_to_six() {
        assert_eq!(resolve_level(Z_DEFAULT_COMPRESSION), 6);
        assert_eq!(resolve_level(0), 0);
        assert_eq!(resolve_level(1), 1);
        assert_eq!(resolve_level(6), 6);
        assert_eq!(resolve_level(9), 9);
    }

    #[test]
    fn invalid_constant_reports_offending_value() {
        let err = FlushMode::try_from(42).unwrap_err();
        assert_eq!(err, InvalidConstant(42));
        assert_eq!(err.0, 42);
    }
}
