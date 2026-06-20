//! Canonical public constants for the `zlib-rs` crate.
//!
//! This module is the single source of truth for every public `zlib` value:
//! the library version identifiers, the `Z_*` flush modes, the `Z_*` return /
//! error codes, the compression levels, the compression strategies, the
//! `data_type` hints, the deflate method, the `Z_NULL` sentinel, and the
//! window / memory-level limits taken from `zconf.h` and `zutil.h`.
//!
//! Every numeric value here is **frozen**: it reproduces the exact value of the
//! corresponding C `#define` in upstream zlib `1.3.2.1-motley`. The wire format
//! (RFC 1950 / 1951 / 1952) and the C ABI both depend on these values being
//! bit-for-bit identical to canonical zlib, so they must never be "modernized"
//! or otherwise changed. The constants are declared as [`i32`] to mirror the C
//! `int` type; the FFI shim ([`crate::ffi`]) casts them to
//! [`core::ffi::c_int`] at the boundary.
//!
//! In addition to the raw constants — which the FFI layer and any direct
//! numeric call sites use — this module exposes idiomatic, `#[repr(i32)]`
//! enumerations ([`FlushMode`], [`Strategy`], [`DataType`]) for exhaustive
//! `match` in the safe Rust core. Each enum's discriminants are exactly equal
//! to the matching raw constants, and conversions in both directions
//! (`TryFrom<i32>` and `From<Enum> for i32`) are provided.
//!
//! # `no_std`
//!
//! This module is `no_std`-clean: it performs no `std` imports and uses only
//! `core` facilities, so the constants are always available — including under
//! the crate's `no-std` feature (the C `Z_SOLO` analogue).
//!
//! # Examples
//!
//! ```
//! use zlib_rs::constants::{resolve_level, FlushMode, Z_DEFAULT_COMPRESSION, Z_FINISH};
//!
//! // Raw constants match the C `#define` values exactly.
//! assert_eq!(Z_FINISH, 4);
//!
//! // `Z_DEFAULT_COMPRESSION` (-1) resolves to the effective level 6.
//! assert_eq!(resolve_level(Z_DEFAULT_COMPRESSION), 6);
//!
//! // The idiomatic enums agree with the raw constants in both directions.
//! assert_eq!(FlushMode::Finish as i32, Z_FINISH);
//! assert_eq!(FlushMode::try_from(Z_FINISH), Ok(FlushMode::Finish));
//! ```

// ===========================================================================
// Phase A — Library version identifiers (zlib.h L44-L49)
// ===========================================================================

/// Human-readable zlib version string, matching the C `ZLIB_VERSION` macro.
///
/// Kept as a Rust string slice here; the FFI layer turns it into a
/// NUL-terminated C string (`b"1.3.2.1-motley\0"`) when implementing
/// `zlibVersion`.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// Packed numeric version, matching the C `ZLIB_VERNUM` macro.
///
/// The nibbles encode `0xMMNNRRSS` (major / minor / revision / sub-revision):
/// `0x1321` corresponds to major 1, minor 3, revision 2, sub-revision 1.
pub const ZLIB_VERNUM: i32 = 0x1321;

/// Major component of the library version (the `1` in `1.3.2.1`).
pub const ZLIB_VER_MAJOR: i32 = 1;

/// Minor component of the library version (the `3` in `1.3.2.1`).
pub const ZLIB_VER_MINOR: i32 = 3;

/// Revision component of the library version (the `2` in `1.3.2.1`).
pub const ZLIB_VER_REVISION: i32 = 2;

/// Sub-revision component of the library version (the trailing `1`).
pub const ZLIB_VER_SUBREVISION: i32 = 1;

// ===========================================================================
// Phase B — Flush modes (zlib.h L172-L178)
// ===========================================================================
//
// Allowed `flush` values for `deflate()` and `inflate()`. See the idiomatic
// [`FlushMode`] enum below for an exhaustive, type-safe representation.

/// No forced flush; allow `deflate` to accumulate data and decide block
/// boundaries on its own. This is the normal streaming mode.
pub const Z_NO_FLUSH: i32 = 0;

/// Flush all pending output to the byte boundary, then continue. Equivalent to
/// `Z_BLOCK` followed by aligning the bit buffer, allowing a decompressor to
/// see all the data so far without forcing a full reset.
pub const Z_PARTIAL_FLUSH: i32 = 1;

/// Flush all pending output and align it to a byte boundary, emitting an empty
/// stored block so the output ends on a byte boundary. The compression state is
/// otherwise preserved.
pub const Z_SYNC_FLUSH: i32 = 2;

/// Like [`Z_SYNC_FLUSH`], but also resets the compression dictionary so the
/// decompressor can restart from this point (useful for error recovery in data
/// streams). Degrades the compression ratio if used too often.
pub const Z_FULL_FLUSH: i32 = 3;

/// Finish the stream: process all remaining input and flush all output. The
/// next operation after a successful finish must reset or end the stream.
pub const Z_FINISH: i32 = 4;

/// Boundary mode for `inflate`/`deflate`: stop (or emit) at block boundaries,
/// allowing fine-grained control over DEFLATE block emission and decoding.
pub const Z_BLOCK: i32 = 5;

/// Like [`Z_BLOCK`], but for `deflate` it also emits the Huffman tree headers
/// so the output can be split immediately after the tree descriptions.
pub const Z_TREES: i32 = 6;

// ===========================================================================
// Phase C — Return / error codes (zlib.h L181-L189)
// ===========================================================================
//
// Return codes for the compression / decompression functions. Negative values
// are errors; zero and positive values denote normal (possibly special)
// events.

/// Operation completed successfully with no errors.
pub const Z_OK: i32 = 0;

/// The end of the compressed stream has been reached. For `inflate` this means
/// a complete stream was decoded; for `deflate` it confirms a successful
/// `Z_FINISH`.
pub const Z_STREAM_END: i32 = 1;

/// A preset dictionary is required to continue decompression. The caller should
/// supply it (via the dictionary API) before calling `inflate` again. Signalled
/// by the `DICTID` handshake in the zlib header.
pub const Z_NEED_DICT: i32 = 2;

/// A file/system error occurred; consult the platform `errno` for details.
pub const Z_ERRNO: i32 = -1;

/// The stream state was inconsistent — for example, an uninitialized stream, an
/// invalid parameter, or a misordered call sequence.
pub const Z_STREAM_ERROR: i32 = -2;

/// The input data was corrupted or did not conform to the expected format
/// (invalid or incomplete DEFLATE/zlib/gzip data).
pub const Z_DATA_ERROR: i32 = -3;

/// Memory could not be allocated for the operation.
pub const Z_MEM_ERROR: i32 = -4;

/// No progress was possible: the output buffer or available input was
/// exhausted without completing the operation. The caller should supply more
/// input or output space and retry.
pub const Z_BUF_ERROR: i32 = -5;

/// The zlib library version supplied to an init function is incompatible with
/// the runtime library version.
pub const Z_VERSION_ERROR: i32 = -6;

// ===========================================================================
// Phase D — Compression levels (zlib.h L194-L197)
// ===========================================================================
//
// Valid compression levels are the integers `0..=9` plus the sentinel
// [`Z_DEFAULT_COMPRESSION`]. Use [`resolve_level`] to map the sentinel to its
// effective level.

/// No compression: input is stored verbatim in DEFLATE stored blocks.
pub const Z_NO_COMPRESSION: i32 = 0;

/// Fastest compression with the lowest ratio (level 1).
pub const Z_BEST_SPEED: i32 = 1;

/// Slowest compression with the highest ratio (level 9).
pub const Z_BEST_COMPRESSION: i32 = 9;

/// Sentinel requesting the library default level. Resolves to level 6 (see
/// [`resolve_level`]), matching `deflate.c`.
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

// ===========================================================================
// Phase E — Compression strategies (zlib.h L200-L204)
// ===========================================================================
//
// The strategy tunes how `deflate` chooses between literal/length/distance
// coding. See the idiomatic [`Strategy`] enum below.

/// Tuned for data produced by a filter (or predictor): force more Huffman
/// coding and less string matching. Intended for data consisting largely of
/// small values with a fairly random distribution.
pub const Z_FILTERED: i32 = 1;

/// Use Huffman coding only, with no string matching at all. Equivalent to a
/// fast, low-ratio entropy coder.
pub const Z_HUFFMAN_ONLY: i32 = 2;

/// Run-length encoding: limit match distances to one, so only runs of the
/// previous byte are encoded as matches. Designed for PNG image data.
pub const Z_RLE: i32 = 3;

/// Prevent the use of dynamic Huffman codes, forcing fixed (static) codes. This
/// simplifies the decoder at a small ratio cost.
pub const Z_FIXED: i32 = 4;

/// Default strategy for ordinary data; uses the full LZ77 + dynamic-Huffman
/// pipeline.
pub const Z_DEFAULT_STRATEGY: i32 = 0;

// ===========================================================================
// Phase F — Data types (zlib.h L207-L210)
// ===========================================================================
//
// Possible values for the `data_type` field of the stream. See the idiomatic
// [`DataType`] enum below.

/// The data is believed to be binary (non-text).
pub const Z_BINARY: i32 = 0;

/// The data is believed to be text.
pub const Z_TEXT: i32 = 1;

/// Legacy alias of [`Z_TEXT`], kept for compatibility with zlib 1.2.2 and
/// earlier.
pub const Z_ASCII: i32 = Z_TEXT;

/// The data type could not be determined.
pub const Z_UNKNOWN: i32 = 2;

// ===========================================================================
// Phase G — Compression method and the null sentinel (zlib.h L213-L216)
// ===========================================================================

/// The DEFLATE compression method — the only method supported by zlib.
pub const Z_DEFLATED: i32 = 8;

/// Null sentinel used when initializing the `zalloc`, `zfree`, and `opaque`
/// fields of a stream at the FFI boundary (the C `Z_NULL` macro). A zero
/// `zalloc`/`zfree` instructs the library to use its built-in allocator.
pub const Z_NULL: i32 = 0;

// ===========================================================================
// Phase H — Window / memory limits and seek origins (zconf.h, zutil.h)
// ===========================================================================

/// Maximum `windowBits` for `deflateInit2`/`inflateInit2` — a 32 KiB LZ77
/// window (`zconf.h`). The base-2 logarithm of the window size, so `15` means
/// `1 << 15 == 32768` bytes.
pub const MAX_WBITS: i32 = 15;

/// Maximum `memLevel` for `deflateInit2` on the default (non-`MAXSEG_64K`)
/// build (`zconf.h`). Higher values use more memory for better speed and
/// compression. Note that the *default* memory level is [`DEF_MEM_LEVEL`]
/// (`8`), which is intentionally lower than this maximum.
pub const MAX_MEM_LEVEL: i32 = 9;

/// Default `windowBits` used for decompression when the caller does not specify
/// one (`zutil.h`); equal to [`MAX_WBITS`]. (`MAX_WBITS` itself is the maximum
/// for compression.)
pub const DEF_WBITS: i32 = MAX_WBITS;

/// Default `memLevel` for `deflateInit` (`zutil.h`). This is `8` even though
/// [`MAX_MEM_LEVEL`] is `9`, matching the upstream default desktop build.
pub const DEF_MEM_LEVEL: i32 = 8;

/// Seek origin: from the beginning of the file. Mirrors the C `SEEK_SET`
/// constant (`zconf.h`) for use by the FFI `gzseek` shim. Idiomatic Rust call
/// sites in the `gz` layer should prefer `std::io::SeekFrom` instead.
pub const SEEK_SET: i32 = 0;

/// Seek origin: from the current file position. Mirrors the C `SEEK_CUR`
/// constant (`zconf.h`).
pub const SEEK_CUR: i32 = 1;

/// Seek origin: from the end of the file. Mirrors the C `SEEK_END` constant
/// (`zconf.h`).
pub const SEEK_END: i32 = 2;

// ===========================================================================
// Level resolution helper (AAP §0.6.6; deflate.c)
// ===========================================================================

/// Resolve a requested compression level into its effective level.
///
/// The sentinel [`Z_DEFAULT_COMPRESSION`] (`-1`) maps to level `6`, matching
/// `deflate.c`; every other value (including the valid range `0..=9`) is
/// returned unchanged. Range validation of non-sentinel values is performed by
/// the deflate initialization path, not here.
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::{resolve_level, Z_DEFAULT_COMPRESSION};
///
/// assert_eq!(resolve_level(Z_DEFAULT_COMPRESSION), 6);
/// assert_eq!(resolve_level(0), 0);
/// assert_eq!(resolve_level(9), 9);
/// ```
#[inline]
#[must_use]
pub const fn resolve_level(level: i32) -> i32 {
    if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    }
}

// ===========================================================================
// Phase I — Idiomatic enumerations
// ===========================================================================
//
// These `#[repr(i32)]` enums provide exhaustive, type-safe representations of
// the corresponding raw constants for use in the safe Rust core (the FFI layer
// continues to operate on the raw `i32` constants). Each enum's discriminants
// are exactly equal to the matching constants, and conversions are provided in
// both directions:
//
//   * `TryFrom<i32>` — fallible parse of a raw value into the enum, returning
//     [`UnknownValue`] for unrecognized integers.
//   * `From<Enum> for i32` — infallible widening to the raw value (equivalent
//     to `enum as i32`, which is also available because of `#[repr(i32)]`).

/// Error returned when an integer does not correspond to any variant of one of
/// the idiomatic constant enums ([`FlushMode`], [`Strategy`], [`DataType`]).
///
/// The offending value is preserved so callers (and the FFI layer, which maps
/// such failures to [`Z_STREAM_ERROR`]) can report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownValue {
    /// The integer value that did not match any known variant.
    pub value: i32,
}

impl UnknownValue {
    /// Construct a new [`UnknownValue`] wrapping the unrecognized integer.
    #[inline]
    #[must_use]
    pub const fn new(value: i32) -> Self {
        Self { value }
    }
}

impl core::fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "value {} does not correspond to a known zlib constant",
            self.value
        )
    }
}

impl core::error::Error for UnknownValue {}

/// Type-safe representation of the seven DEFLATE flush modes.
///
/// Discriminants equal the raw `Z_*` flush constants exactly, so
/// `FlushMode::X as i32` yields the canonical C value.
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::{FlushMode, Z_SYNC_FLUSH};
///
/// assert_eq!(FlushMode::SyncFlush as i32, Z_SYNC_FLUSH);
/// assert_eq!(FlushMode::try_from(Z_SYNC_FLUSH), Ok(FlushMode::SyncFlush));
/// assert!(FlushMode::try_from(99).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum FlushMode {
    /// See [`Z_NO_FLUSH`].
    NoFlush = Z_NO_FLUSH,
    /// See [`Z_PARTIAL_FLUSH`].
    PartialFlush = Z_PARTIAL_FLUSH,
    /// See [`Z_SYNC_FLUSH`].
    SyncFlush = Z_SYNC_FLUSH,
    /// See [`Z_FULL_FLUSH`].
    FullFlush = Z_FULL_FLUSH,
    /// See [`Z_FINISH`].
    Finish = Z_FINISH,
    /// See [`Z_BLOCK`].
    Block = Z_BLOCK,
    /// See [`Z_TREES`].
    Trees = Z_TREES,
}

impl TryFrom<i32> for FlushMode {
    type Error = UnknownValue;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            Z_NO_FLUSH => Ok(Self::NoFlush),
            Z_PARTIAL_FLUSH => Ok(Self::PartialFlush),
            Z_SYNC_FLUSH => Ok(Self::SyncFlush),
            Z_FULL_FLUSH => Ok(Self::FullFlush),
            Z_FINISH => Ok(Self::Finish),
            Z_BLOCK => Ok(Self::Block),
            Z_TREES => Ok(Self::Trees),
            other => Err(UnknownValue::new(other)),
        }
    }
}

impl From<FlushMode> for i32 {
    #[inline]
    fn from(value: FlushMode) -> Self {
        value as Self
    }
}

/// Type-safe representation of the five compression strategies.
///
/// Discriminants equal the raw `Z_*` strategy constants exactly.
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::{Strategy, Z_RLE};
///
/// assert_eq!(Strategy::Rle as i32, Z_RLE);
/// assert_eq!(Strategy::try_from(Z_RLE), Ok(Strategy::Rle));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Strategy {
    /// See [`Z_DEFAULT_STRATEGY`].
    Default = Z_DEFAULT_STRATEGY,
    /// See [`Z_FILTERED`].
    Filtered = Z_FILTERED,
    /// See [`Z_HUFFMAN_ONLY`].
    HuffmanOnly = Z_HUFFMAN_ONLY,
    /// See [`Z_RLE`].
    Rle = Z_RLE,
    /// See [`Z_FIXED`].
    Fixed = Z_FIXED,
}

impl TryFrom<i32> for Strategy {
    type Error = UnknownValue;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            Z_DEFAULT_STRATEGY => Ok(Self::Default),
            Z_FILTERED => Ok(Self::Filtered),
            Z_HUFFMAN_ONLY => Ok(Self::HuffmanOnly),
            Z_RLE => Ok(Self::Rle),
            Z_FIXED => Ok(Self::Fixed),
            other => Err(UnknownValue::new(other)),
        }
    }
}

impl From<Strategy> for i32 {
    #[inline]
    fn from(value: Strategy) -> Self {
        value as Self
    }
}

/// Type-safe representation of the `data_type` hint.
///
/// Discriminants equal the raw `Z_BINARY` / `Z_TEXT` / `Z_UNKNOWN` constants
/// exactly. (The legacy alias [`Z_ASCII`] equals [`Z_TEXT`], so it maps to
/// [`DataType::Text`].)
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::{DataType, Z_ASCII, Z_TEXT};
///
/// assert_eq!(DataType::Text as i32, Z_TEXT);
/// // The legacy ASCII alias is the same value as TEXT.
/// assert_eq!(DataType::try_from(Z_ASCII), Ok(DataType::Text));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DataType {
    /// See [`Z_BINARY`].
    Binary = Z_BINARY,
    /// See [`Z_TEXT`].
    Text = Z_TEXT,
    /// See [`Z_UNKNOWN`].
    Unknown = Z_UNKNOWN,
}

impl TryFrom<i32> for DataType {
    type Error = UnknownValue;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        // NOTE: `Z_ASCII == Z_TEXT == 1`, so it is intentionally absent as a
        // distinct match arm (it would be an unreachable duplicate pattern).
        match value {
            Z_BINARY => Ok(Self::Binary),
            Z_TEXT => Ok(Self::Text),
            Z_UNKNOWN => Ok(Self::Unknown),
            other => Err(UnknownValue::new(other)),
        }
    }
}

impl From<DataType> for i32 {
    #[inline]
    fn from(value: DataType) -> Self {
        value as Self
    }
}

// ===========================================================================
// Tests — assert every value matches canonical C zlib (bit-exact contract).
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
    fn method_and_null_constants_match_c() {
        assert_eq!(Z_DEFLATED, 8);
        assert_eq!(Z_NULL, 0);
    }

    #[test]
    fn window_and_memory_limits_match_c() {
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(DEF_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(DEF_MEM_LEVEL, 8);
    }

    #[test]
    fn seek_constants_match_c() {
        assert_eq!(SEEK_SET, 0);
        assert_eq!(SEEK_CUR, 1);
        assert_eq!(SEEK_END, 2);
    }

    #[test]
    fn resolve_level_maps_default_to_six() {
        assert_eq!(resolve_level(Z_DEFAULT_COMPRESSION), 6);
        assert_eq!(resolve_level(-1), 6);
    }

    #[test]
    fn resolve_level_passes_through_explicit_levels() {
        assert_eq!(resolve_level(0), 0);
        assert_eq!(resolve_level(1), 1);
        assert_eq!(resolve_level(6), 6);
        assert_eq!(resolve_level(9), 9);
        // Out-of-range values pass through unchanged; validation happens in the
        // deflate init path, not in `resolve_level`.
        assert_eq!(resolve_level(42), 42);
    }

    #[test]
    fn flush_mode_discriminants_match_constants() {
        assert_eq!(FlushMode::NoFlush as i32, Z_NO_FLUSH);
        assert_eq!(FlushMode::PartialFlush as i32, Z_PARTIAL_FLUSH);
        assert_eq!(FlushMode::SyncFlush as i32, Z_SYNC_FLUSH);
        assert_eq!(FlushMode::FullFlush as i32, Z_FULL_FLUSH);
        assert_eq!(FlushMode::Finish as i32, Z_FINISH);
        assert_eq!(FlushMode::Block as i32, Z_BLOCK);
        assert_eq!(FlushMode::Trees as i32, Z_TREES);
    }

    #[test]
    fn flush_mode_round_trips_through_i32() {
        for (raw, mode) in [
            (Z_NO_FLUSH, FlushMode::NoFlush),
            (Z_PARTIAL_FLUSH, FlushMode::PartialFlush),
            (Z_SYNC_FLUSH, FlushMode::SyncFlush),
            (Z_FULL_FLUSH, FlushMode::FullFlush),
            (Z_FINISH, FlushMode::Finish),
            (Z_BLOCK, FlushMode::Block),
            (Z_TREES, FlushMode::Trees),
        ] {
            assert_eq!(FlushMode::try_from(raw), Ok(mode));
            assert_eq!(i32::from(mode), raw);
        }
        assert_eq!(FlushMode::try_from(7), Err(UnknownValue::new(7)));
        assert_eq!(FlushMode::try_from(-1), Err(UnknownValue::new(-1)));
    }

    #[test]
    fn strategy_discriminants_match_constants() {
        assert_eq!(Strategy::Default as i32, Z_DEFAULT_STRATEGY);
        assert_eq!(Strategy::Filtered as i32, Z_FILTERED);
        assert_eq!(Strategy::HuffmanOnly as i32, Z_HUFFMAN_ONLY);
        assert_eq!(Strategy::Rle as i32, Z_RLE);
        assert_eq!(Strategy::Fixed as i32, Z_FIXED);
    }

    #[test]
    fn strategy_round_trips_through_i32() {
        for (raw, strategy) in [
            (Z_DEFAULT_STRATEGY, Strategy::Default),
            (Z_FILTERED, Strategy::Filtered),
            (Z_HUFFMAN_ONLY, Strategy::HuffmanOnly),
            (Z_RLE, Strategy::Rle),
            (Z_FIXED, Strategy::Fixed),
        ] {
            assert_eq!(Strategy::try_from(raw), Ok(strategy));
            assert_eq!(i32::from(strategy), raw);
        }
        assert_eq!(Strategy::try_from(5), Err(UnknownValue::new(5)));
    }

    #[test]
    fn data_type_discriminants_match_constants() {
        assert_eq!(DataType::Binary as i32, Z_BINARY);
        assert_eq!(DataType::Text as i32, Z_TEXT);
        assert_eq!(DataType::Unknown as i32, Z_UNKNOWN);
    }

    #[test]
    fn data_type_round_trips_through_i32() {
        for (raw, data_type) in [
            (Z_BINARY, DataType::Binary),
            (Z_TEXT, DataType::Text),
            (Z_UNKNOWN, DataType::Unknown),
        ] {
            assert_eq!(DataType::try_from(raw), Ok(data_type));
            assert_eq!(i32::from(data_type), raw);
        }
        // The legacy ASCII alias decodes to `Text` because it shares the value.
        assert_eq!(DataType::try_from(Z_ASCII), Ok(DataType::Text));
        assert_eq!(DataType::try_from(3), Err(UnknownValue::new(3)));
    }

    #[test]
    fn unknown_value_reports_offending_integer() {
        let err = UnknownValue::new(123);
        assert_eq!(err.value, 123);
        // `UnknownValue` is a usable `core::error::Error`.
        let as_error: &dyn core::error::Error = &err;
        let rendered = std::format!("{as_error}");
        assert!(rendered.contains("123"));
    }
}
