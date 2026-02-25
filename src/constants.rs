//! Constants for the zlib-rs compression library.
//!
//! This module centralizes all numeric constants, type definitions, and limits
//! that were defined across `zlib.h`, `zconf.h`, `zutil.h`, and `deflate.h` in
//! the original C zlib codebase. It is the most foundational module in the crate
//! and is depended upon by every other module.
//!
//! # Organization
//!
//! Constants are grouped by category:
//! - **Flush modes** — control how aggressively data is flushed during
//!   compression and decompression ([`Z_NO_FLUSH`] through [`Z_TREES`]).
//! - **Compression levels** — control the speed/ratio tradeoff
//!   ([`Z_NO_COMPRESSION`] through [`Z_BEST_COMPRESSION`]).
//! - **Compression strategies** — select the algorithm variant
//!   ([`Z_DEFAULT_STRATEGY`] through [`Z_FIXED`]).
//! - **Data types** — hint at the nature of the input data
//!   ([`Z_BINARY`], [`Z_TEXT`], [`Z_UNKNOWN`]).
//! - **Window / memory limits** — control buffer sizes and memory usage
//!   ([`MAX_WBITS`], [`MAX_MEM_LEVEL`], etc.).
//! - **Match constants** — LZ77 match length bounds
//!   ([`MIN_MATCH`], [`MAX_MATCH`]).
//! - **Block types** — DEFLATE block encoding types
//!   ([`STORED_BLOCK`], [`STATIC_TREES`], [`DYN_TREES`]).
//! - **Huffman tree constants** — internal tree construction limits.
//! - **Inflate table constants** — Huffman table size bounds for inflate.
//! - **Type-safe enums** — [`FlushMode`] and [`CompressionLevel`] provide
//!   idiomatic Rust alternatives to the raw integer constants.

// ---------------------------------------------------------------------------
// Flush mode constants (from zlib.h lines 172–178)
// ---------------------------------------------------------------------------

/// No flush: allow deflate to decide when to produce output. Provides maximum
/// compression by accumulating as much input as possible before flushing.
pub const Z_NO_FLUSH: i32 = 0;

/// Partial flush: flush all pending output but do not align to a byte boundary.
/// This mode is retained for backward compatibility; [`Z_SYNC_FLUSH`] is
/// generally preferred.
pub const Z_PARTIAL_FLUSH: i32 = 1;

/// Sync flush: flush all pending output to the output buffer and align the
/// output to a byte boundary. A four-byte empty stored block marker
/// (`00 00 FF FF`) is emitted so that the decompressor can synchronize.
pub const Z_SYNC_FLUSH: i32 = 2;

/// Full flush: like [`Z_SYNC_FLUSH`] but also resets the compression state so
/// that decompression can restart from this point if previous compressed data
/// has been damaged. Useful for random-access applications.
pub const Z_FULL_FLUSH: i32 = 3;

/// Finish: signal the compressor that all input has been provided. The stream
/// will be finalized and a terminating block will be emitted. After this flush,
/// the only successful return code is [`crate::error::ReturnCode`] stream end.
pub const Z_FINISH: i32 = 4;

/// Block flush: request that deflate stop just after completing the current
/// deflate block. The output is not byte-aligned. This is for advanced use
/// only, enabling inspection of intermediate compression state.
pub const Z_BLOCK: i32 = 5;

/// Trees flush: like [`Z_BLOCK`] but also returns once the Huffman trees for
/// the current block have been emitted, before any literal or match codes.
/// This is for advanced use only.
pub const Z_TREES: i32 = 6;

// ---------------------------------------------------------------------------
// Compression level constants (from zlib.h lines 194–197)
// ---------------------------------------------------------------------------

/// No compression (level 0). Input data is simply stored in blocks with no
/// compression applied. This is the fastest "compression" but produces the
/// largest output.
pub const Z_NO_COMPRESSION: i32 = 0;

/// Best speed (level 1). Fastest compression using greedy matching. Produces
/// larger output than higher levels but requires much less CPU time.
pub const Z_BEST_SPEED: i32 = 1;

/// Best compression (level 9). Slowest compression using exhaustive lazy
/// matching with the deepest hash chain search. Produces the smallest output.
pub const Z_BEST_COMPRESSION: i32 = 9;

/// Default compression level. When specified, the library selects a
/// compromise between speed and compression ratio (currently equivalent to
/// level 6). Encoded as -1 per the C API convention; the library internally
/// resolves this to level 6 during initialization.
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

// ---------------------------------------------------------------------------
// Compression strategy constants (from zlib.h lines 200–204)
// ---------------------------------------------------------------------------

/// Default strategy. Uses a combination of LZ77 string matching and Huffman
/// coding, suitable for most data types.
pub const Z_DEFAULT_STRATEGY: i32 = 0;

/// Filtered strategy. Optimized for data produced by a filter or predictor
/// (e.g., PNG image data). Uses shorter hash chains that are better suited to
/// data with mostly small match distances.
pub const Z_FILTERED: i32 = 1;

/// Huffman-only strategy. Disables LZ77 string matching entirely, using only
/// Huffman coding of individual bytes. Useful when the application has already
/// performed its own redundancy elimination.
pub const Z_HUFFMAN_ONLY: i32 = 2;

/// RLE (Run-Length Encoding) strategy. Limits match distance to 1, effectively
/// performing run-length encoding. Very fast for data with many repeated bytes.
pub const Z_RLE: i32 = 3;

/// Fixed strategy. Forces the use of pre-defined fixed Huffman codes rather
/// than computing dynamic codes per block. This eliminates the overhead of
/// transmitting the code trees at the cost of slightly worse compression on
/// most data.
pub const Z_FIXED: i32 = 4;

// ---------------------------------------------------------------------------
// Data type constants (from zlib.h lines 207–210)
// ---------------------------------------------------------------------------

/// Binary data type. The compressor's best guess is that the data is binary
/// (not text).
pub const Z_BINARY: i32 = 0;

/// Text data type. The compressor's best guess is that the data is textual
/// (ASCII-compatible).
pub const Z_TEXT: i32 = 1;

/// ASCII data type. Alias for [`Z_TEXT`], retained for compatibility with
/// zlib versions 1.2.2 and earlier.
pub const Z_ASCII: i32 = Z_TEXT;

/// Unknown data type. The compressor has not yet determined the data type.
pub const Z_UNKNOWN: i32 = 2;

// ---------------------------------------------------------------------------
// Compression method (from zlib.h line 213)
// ---------------------------------------------------------------------------

/// The deflate compression method. This is the only compression method
/// supported by zlib. The value 8 corresponds to the method field in the
/// zlib (RFC 1950) and gzip (RFC 1952) headers.
pub const Z_DEFLATED: i32 = 8;

// ---------------------------------------------------------------------------
// Null constant (from zlib.h line 216)
// ---------------------------------------------------------------------------

/// Null value for initializing allocator and opaque fields. In the original C
/// API this was used to initialize `zalloc`, `zfree`, and `opaque` pointers to
/// null. Retained for API compatibility.
pub const Z_NULL: i32 = 0;

// ---------------------------------------------------------------------------
// Window size and memory level constants (from zconf.h lines 277–287,
// zutil.h lines 72–84)
// ---------------------------------------------------------------------------

/// Maximum window bits (log₂ of the LZ77 sliding window size). Valid range
/// for the `window_bits` parameter is 9 through 15 (inclusive), corresponding
/// to window sizes of 512 bytes through 32 KB.
///
/// The `window_bits` parameter supports overloaded semantics:
/// - **8–15**: zlib format (RFC 1950) wrapper.
/// - **-8 to -15**: raw DEFLATE (RFC 1951), no wrapper.
/// - **24–31** (i.e., +16): gzip format (RFC 1952) wrapper.
/// - **40–47** (i.e., +32): automatic detection of zlib or gzip format.
pub const MAX_WBITS: i32 = 15;

/// Default window bits for decompression. Equal to [`MAX_WBITS`] (15),
/// producing a 32 KB sliding window.
pub const DEF_WBITS: i32 = MAX_WBITS;

/// Maximum memory level. Controls the amount of memory used for internal
/// compression state. Higher values use more memory but can improve
/// compression speed and ratio. The maximum value is 9.
///
/// Memory usage for deflate is approximately:
/// `(1 << (window_bits + 2)) + (1 << (mem_level + 9))` bytes.
pub const MAX_MEM_LEVEL: i32 = 9;

/// Default memory level. A reasonable compromise between memory usage and
/// compression performance. Equal to 8, which uses approximately 128 KB for
/// the internal hash table in addition to the sliding window.
pub const DEF_MEM_LEVEL: i32 = 8;

// ---------------------------------------------------------------------------
// LZ77 match length constants (from zutil.h lines 92–93)
// ---------------------------------------------------------------------------

/// Minimum match length for LZ77 string matching. Per the DEFLATE
/// specification (RFC 1951), the shortest encodable match is 3 bytes.
pub const MIN_MATCH: usize = 3;

/// Maximum match length for LZ77 string matching. Per the DEFLATE
/// specification (RFC 1951), the longest encodable match is 258 bytes.
pub const MAX_MATCH: usize = 258;

// ---------------------------------------------------------------------------
// Block type constants (from zutil.h lines 87–89)
// ---------------------------------------------------------------------------

/// Stored block type. No compression is applied; the block data is stored
/// verbatim with a simple length header.
pub const STORED_BLOCK: i32 = 0;

/// Static Huffman tree block type. The block uses pre-defined fixed Huffman
/// codes specified in RFC 1951 §3.2.6.
pub const STATIC_TREES: i32 = 1;

/// Dynamic Huffman tree block type. The block includes custom Huffman code
/// tables computed from the block's symbol frequencies.
pub const DYN_TREES: i32 = 2;

// ---------------------------------------------------------------------------
// Preset dictionary flag (from zutil.h line 96)
// ---------------------------------------------------------------------------

/// Preset dictionary flag in the zlib header (RFC 1950). When bit 5 of the
/// FLG byte is set, a four-byte dictionary identifier (Adler-32 of the
/// dictionary data) follows the header before the compressed data.
pub const PRESET_DICT: u32 = 0x20;

// ---------------------------------------------------------------------------
// Gzip I/O buffer size (from gzguts.h line 156)
// ---------------------------------------------------------------------------

/// Default gzip I/O buffer size in bytes. This is the initial buffer
/// allocation for gzip file read/write operations. The actual buffer may be
/// resized via `gz_buffer`.
pub const GZBUFSIZE: usize = 8192;

// ---------------------------------------------------------------------------
// Huffman tree and compression engine constants (from deflate.h lines 34–56)
// ---------------------------------------------------------------------------

/// Number of length codes, not counting the special `END_BLOCK` code.
/// DEFLATE defines 29 length codes (257–285) that encode match lengths from
/// 3 to 258.
pub const LENGTH_CODES: usize = 29;

/// Number of literal byte values (0–255). There are 256 possible literal
/// bytes in the DEFLATE alphabet.
pub const LITERALS: usize = 256;

/// Total number of literal or length codes in the DEFLATE alphabet, including
/// the `END_BLOCK` code (256). Equal to `LITERALS + 1 + LENGTH_CODES` = 286.
pub const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes in the DEFLATE alphabet. DEFLATE defines 30
/// distance codes (0–29) that encode match distances from 1 to 32768.
pub const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths of the dynamic Huffman
/// code trees. These 19 codes encode the code lengths for both the
/// literal/length and distance code alphabets.
pub const BL_CODES: usize = 19;

/// Maximum heap size for Huffman tree construction. The heap is used as a
/// priority queue during tree building. Equal to `2 * L_CODES + 1` = 573.
pub const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// Maximum number of bits in a Huffman code. Per the DEFLATE specification
/// (RFC 1951), no code may exceed 15 bits in length.
pub const MAX_BITS: usize = 15;

/// Size of the bit buffer (`bi_buf`) in bits. The bit buffer accumulates
/// output bits before flushing whole bytes to the pending output buffer.
pub const BUF_SIZE: usize = 16;

// ---------------------------------------------------------------------------
// Inflate Huffman table size constants (from inftrees.h lines 49–51)
// ---------------------------------------------------------------------------

/// Maximum number of entries in the length/literal Huffman decoding table for
/// inflate. Computed by exhaustive search (`enough 286 9 15` = 852). This
/// bounds the space needed for the two-level lookup table with a 9-bit root.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of entries in the distance Huffman decoding table for
/// inflate. Computed by exhaustive search (`enough 30 6 15` = 592). This
/// bounds the space needed for the two-level lookup table with a 6-bit root.
pub const ENOUGH_DISTS: usize = 592;

/// Total maximum code table entries for inflate. Equal to
/// `ENOUGH_LENS + ENOUGH_DISTS` = 1444. This is the size of the `codes[]`
/// array in `InflateState`.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

// ===========================================================================
// Type-safe enums
// ===========================================================================

/// Type-safe flush mode for `deflate()` and `inflate()` operations.
///
/// This enum provides a safe alternative to the raw integer flush mode
/// constants ([`Z_NO_FLUSH`] through [`Z_TREES`]). Each variant maps to
/// exactly one of the 7 allowed flush values.
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::FlushMode;
///
/// let mode = FlushMode::SyncFlush;
/// assert_eq!(mode.as_i32(), 2);
/// assert_eq!(FlushMode::from_i32(4), Some(FlushMode::Finish));
/// assert_eq!(FlushMode::from_i32(99), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum FlushMode {
    /// No flush — allow the compressor to accumulate input.
    NoFlush = 0,
    /// Partial flush — flush pending output, not necessarily byte-aligned.
    PartialFlush = 1,
    /// Sync flush — flush to byte boundary with a sync marker.
    SyncFlush = 2,
    /// Full flush — sync flush with compression state reset.
    FullFlush = 3,
    /// Finish — finalize the stream.
    Finish = 4,
    /// Block — complete the current block without byte alignment.
    Block = 5,
    /// Trees — complete the block and return after emitting Huffman trees.
    Trees = 6,
}

impl FlushMode {
    /// Converts a raw `i32` flush value to a [`FlushMode`], returning `None`
    /// if the value is not a valid flush mode (must be in 0..=6).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::FlushMode;
    ///
    /// assert_eq!(FlushMode::from_i32(0), Some(FlushMode::NoFlush));
    /// assert_eq!(FlushMode::from_i32(6), Some(FlushMode::Trees));
    /// assert_eq!(FlushMode::from_i32(-1), None);
    /// assert_eq!(FlushMode::from_i32(7), None);
    /// ```
    #[inline]
    pub fn from_i32(value: i32) -> Option<FlushMode> {
        match value {
            0 => Some(FlushMode::NoFlush),
            1 => Some(FlushMode::PartialFlush),
            2 => Some(FlushMode::SyncFlush),
            3 => Some(FlushMode::FullFlush),
            4 => Some(FlushMode::Finish),
            5 => Some(FlushMode::Block),
            6 => Some(FlushMode::Trees),
            _ => None,
        }
    }

    /// Returns the raw `i32` value corresponding to this flush mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::FlushMode;
    ///
    /// assert_eq!(FlushMode::NoFlush.as_i32(), 0);
    /// assert_eq!(FlushMode::Finish.as_i32(), 4);
    /// assert_eq!(FlushMode::Trees.as_i32(), 6);
    /// ```
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl core::fmt::Display for FlushMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FlushMode::NoFlush => write!(f, "no flush"),
            FlushMode::PartialFlush => write!(f, "partial flush"),
            FlushMode::SyncFlush => write!(f, "sync flush"),
            FlushMode::FullFlush => write!(f, "full flush"),
            FlushMode::Finish => write!(f, "finish"),
            FlushMode::Block => write!(f, "block"),
            FlushMode::Trees => write!(f, "trees"),
        }
    }
}

/// Type-safe compression level for `deflate_init()` and related functions.
///
/// This enum represents all 10 valid compression levels (0 through 9). The
/// [`CompressionLevel::Default`] variant corresponds to level 6, which is the
/// level used when `Z_DEFAULT_COMPRESSION` (-1) is passed to the C API.
///
/// # Examples
///
/// ```
/// use zlib_rs::constants::CompressionLevel;
///
/// let level = CompressionLevel::BestSpeed;
/// assert_eq!(level.as_i32(), 1);
///
/// // Z_DEFAULT_COMPRESSION (-1) maps to Default (level 6)
/// assert_eq!(CompressionLevel::from_i32(-1), Some(CompressionLevel::Default));
/// assert_eq!(CompressionLevel::from_i32(6), Some(CompressionLevel::Default));
///
/// // Out-of-range values return None
/// assert_eq!(CompressionLevel::from_i32(10), None);
/// assert_eq!(CompressionLevel::from_i32(-2), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionLevel {
    /// No compression (level 0). Data is stored in blocks without compression.
    None,
    /// Best speed (level 1). Fastest compression with lowest ratio.
    BestSpeed,
    /// Compression level 2. Slightly better compression than level 1.
    Level2,
    /// Compression level 3. Best of the "fast" compression levels.
    Level3,
    /// Compression level 4. First level to use lazy matching.
    Level4,
    /// Compression level 5. Moderate compression with reasonable speed.
    Level5,
    /// Default compression level (level 6). A good compromise between speed
    /// and compression ratio. This is the level used when
    /// `Z_DEFAULT_COMPRESSION` (-1) is specified.
    Default,
    /// Compression level 7. Higher compression with longer search.
    Level7,
    /// Compression level 8. Near-best compression.
    Level8,
    /// Best compression (level 9). Slowest compression with highest ratio.
    BestCompression,
}

impl CompressionLevel {
    /// Converts a raw `i32` compression level to a [`CompressionLevel`],
    /// returning `None` if the value is out of range.
    ///
    /// The special value `-1` ([`Z_DEFAULT_COMPRESSION`]) is accepted and
    /// maps to [`CompressionLevel::Default`] (level 6). Valid numeric levels
    /// are 0 through 9 inclusive.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::CompressionLevel;
    ///
    /// assert_eq!(CompressionLevel::from_i32(-1), Some(CompressionLevel::Default));
    /// assert_eq!(CompressionLevel::from_i32(0), Some(CompressionLevel::None));
    /// assert_eq!(CompressionLevel::from_i32(1), Some(CompressionLevel::BestSpeed));
    /// assert_eq!(CompressionLevel::from_i32(6), Some(CompressionLevel::Default));
    /// assert_eq!(CompressionLevel::from_i32(9), Some(CompressionLevel::BestCompression));
    /// assert_eq!(CompressionLevel::from_i32(10), None);
    /// assert_eq!(CompressionLevel::from_i32(-2), None);
    /// ```
    #[inline]
    pub fn from_i32(value: i32) -> Option<CompressionLevel> {
        match value {
            -1 | 6 => Some(CompressionLevel::Default),
            0 => Some(CompressionLevel::None),
            1 => Some(CompressionLevel::BestSpeed),
            2 => Some(CompressionLevel::Level2),
            3 => Some(CompressionLevel::Level3),
            4 => Some(CompressionLevel::Level4),
            5 => Some(CompressionLevel::Level5),
            7 => Some(CompressionLevel::Level7),
            8 => Some(CompressionLevel::Level8),
            9 => Some(CompressionLevel::BestCompression),
            _ => None,
        }
    }

    /// Returns the numeric compression level as an `i32` (0–9).
    ///
    /// Note that [`CompressionLevel::Default`] returns `6`, not `-1`. To
    /// obtain the sentinel value `-1`, use [`Z_DEFAULT_COMPRESSION`] directly.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::CompressionLevel;
    ///
    /// assert_eq!(CompressionLevel::None.as_i32(), 0);
    /// assert_eq!(CompressionLevel::BestSpeed.as_i32(), 1);
    /// assert_eq!(CompressionLevel::Default.as_i32(), 6);
    /// assert_eq!(CompressionLevel::BestCompression.as_i32(), 9);
    /// ```
    #[inline]
    pub const fn as_i32(self) -> i32 {
        match self {
            CompressionLevel::None => 0,
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::Level2 => 2,
            CompressionLevel::Level3 => 3,
            CompressionLevel::Level4 => 4,
            CompressionLevel::Level5 => 5,
            CompressionLevel::Default => 6,
            CompressionLevel::Level7 => 7,
            CompressionLevel::Level8 => 8,
            CompressionLevel::BestCompression => 9,
        }
    }
}

impl core::fmt::Display for CompressionLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CompressionLevel::None => write!(f, "no compression (level 0)"),
            CompressionLevel::BestSpeed => write!(f, "best speed (level 1)"),
            CompressionLevel::Level2 => write!(f, "level 2"),
            CompressionLevel::Level3 => write!(f, "level 3"),
            CompressionLevel::Level4 => write!(f, "level 4"),
            CompressionLevel::Level5 => write!(f, "level 5"),
            CompressionLevel::Default => write!(f, "default (level 6)"),
            CompressionLevel::Level7 => write!(f, "level 7"),
            CompressionLevel::Level8 => write!(f, "level 8"),
            CompressionLevel::BestCompression => write!(f, "best compression (level 9)"),
        }
    }
}

// ===========================================================================
// Compile-time assertions to verify constant correctness
// ===========================================================================

// Verify derived constants are correct.
const _: () = assert!(
    L_CODES == 286,
    "L_CODES must equal LITERALS + 1 + LENGTH_CODES = 286"
);
const _: () = assert!(
    HEAP_SIZE == 573,
    "HEAP_SIZE must equal 2 * L_CODES + 1 = 573"
);
const _: () = assert!(
    ENOUGH == 1444,
    "ENOUGH must equal ENOUGH_LENS + ENOUGH_DISTS = 1444"
);

// Verify flush mode constants match their enum discriminants.
const _: () = assert!(FlushMode::NoFlush as i32 == Z_NO_FLUSH);
const _: () = assert!(FlushMode::PartialFlush as i32 == Z_PARTIAL_FLUSH);
const _: () = assert!(FlushMode::SyncFlush as i32 == Z_SYNC_FLUSH);
const _: () = assert!(FlushMode::FullFlush as i32 == Z_FULL_FLUSH);
const _: () = assert!(FlushMode::Finish as i32 == Z_FINISH);
const _: () = assert!(FlushMode::Block as i32 == Z_BLOCK);
const _: () = assert!(FlushMode::Trees as i32 == Z_TREES);

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Flush mode constants
    // -------------------------------------------------------------------

    #[test]
    fn flush_mode_constants_match_c_zlib() {
        assert_eq!(Z_NO_FLUSH, 0);
        assert_eq!(Z_PARTIAL_FLUSH, 1);
        assert_eq!(Z_SYNC_FLUSH, 2);
        assert_eq!(Z_FULL_FLUSH, 3);
        assert_eq!(Z_FINISH, 4);
        assert_eq!(Z_BLOCK, 5);
        assert_eq!(Z_TREES, 6);
    }

    // -------------------------------------------------------------------
    // Compression level constants
    // -------------------------------------------------------------------

    #[test]
    fn compression_level_constants_match_c_zlib() {
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);
    }

    // -------------------------------------------------------------------
    // Strategy constants
    // -------------------------------------------------------------------

    #[test]
    fn strategy_constants_match_c_zlib() {
        assert_eq!(Z_DEFAULT_STRATEGY, 0);
        assert_eq!(Z_FILTERED, 1);
        assert_eq!(Z_HUFFMAN_ONLY, 2);
        assert_eq!(Z_RLE, 3);
        assert_eq!(Z_FIXED, 4);
    }

    // -------------------------------------------------------------------
    // Data type constants
    // -------------------------------------------------------------------

    #[test]
    fn data_type_constants_match_c_zlib() {
        assert_eq!(Z_BINARY, 0);
        assert_eq!(Z_TEXT, 1);
        assert_eq!(Z_ASCII, Z_TEXT);
        assert_eq!(Z_UNKNOWN, 2);
    }

    // -------------------------------------------------------------------
    // Miscellaneous constants
    // -------------------------------------------------------------------

    #[test]
    fn misc_constants_match_c_zlib() {
        assert_eq!(Z_DEFLATED, 8);
        assert_eq!(Z_NULL, 0);
    }

    // -------------------------------------------------------------------
    // Window and memory constants
    // -------------------------------------------------------------------

    #[test]
    fn window_and_memory_constants_match_c_zlib() {
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(DEF_MEM_LEVEL, 8);
    }

    // -------------------------------------------------------------------
    // Match length constants
    // -------------------------------------------------------------------

    #[test]
    fn match_constants_match_c_zlib() {
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(MAX_MATCH, 258);
    }

    // -------------------------------------------------------------------
    // Block type constants
    // -------------------------------------------------------------------

    #[test]
    fn block_type_constants_match_c_zlib() {
        assert_eq!(STORED_BLOCK, 0);
        assert_eq!(STATIC_TREES, 1);
        assert_eq!(DYN_TREES, 2);
    }

    // -------------------------------------------------------------------
    // Preset dictionary flag
    // -------------------------------------------------------------------

    #[test]
    fn preset_dict_matches_c_zlib() {
        assert_eq!(PRESET_DICT, 0x20);
    }

    // -------------------------------------------------------------------
    // Gzip buffer size
    // -------------------------------------------------------------------

    #[test]
    fn gzbufsize_matches_c_zlib() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    // -------------------------------------------------------------------
    // Deflate / Huffman tree constants
    // -------------------------------------------------------------------

    #[test]
    fn deflate_constants_match_c_zlib() {
        assert_eq!(LENGTH_CODES, 29);
        assert_eq!(LITERALS, 256);
        assert_eq!(L_CODES, 286);
        assert_eq!(D_CODES, 30);
        assert_eq!(BL_CODES, 19);
        assert_eq!(HEAP_SIZE, 573);
        assert_eq!(MAX_BITS, 15);
        assert_eq!(BUF_SIZE, 16);
    }

    // -------------------------------------------------------------------
    // Inflate table size constants
    // -------------------------------------------------------------------

    #[test]
    fn inflate_table_constants_match_c_zlib() {
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
    }

    // -------------------------------------------------------------------
    // FlushMode enum
    // -------------------------------------------------------------------

    #[test]
    fn flush_mode_from_i32_valid() {
        assert_eq!(FlushMode::from_i32(0), Some(FlushMode::NoFlush));
        assert_eq!(FlushMode::from_i32(1), Some(FlushMode::PartialFlush));
        assert_eq!(FlushMode::from_i32(2), Some(FlushMode::SyncFlush));
        assert_eq!(FlushMode::from_i32(3), Some(FlushMode::FullFlush));
        assert_eq!(FlushMode::from_i32(4), Some(FlushMode::Finish));
        assert_eq!(FlushMode::from_i32(5), Some(FlushMode::Block));
        assert_eq!(FlushMode::from_i32(6), Some(FlushMode::Trees));
    }

    #[test]
    fn flush_mode_from_i32_invalid() {
        assert_eq!(FlushMode::from_i32(-1), None);
        assert_eq!(FlushMode::from_i32(7), None);
        assert_eq!(FlushMode::from_i32(100), None);
        assert_eq!(FlushMode::from_i32(i32::MIN), None);
        assert_eq!(FlushMode::from_i32(i32::MAX), None);
    }

    #[test]
    fn flush_mode_as_i32() {
        assert_eq!(FlushMode::NoFlush.as_i32(), 0);
        assert_eq!(FlushMode::PartialFlush.as_i32(), 1);
        assert_eq!(FlushMode::SyncFlush.as_i32(), 2);
        assert_eq!(FlushMode::FullFlush.as_i32(), 3);
        assert_eq!(FlushMode::Finish.as_i32(), 4);
        assert_eq!(FlushMode::Block.as_i32(), 5);
        assert_eq!(FlushMode::Trees.as_i32(), 6);
    }

    #[test]
    fn flush_mode_round_trip() {
        for i in 0..=6 {
            let mode = FlushMode::from_i32(i).unwrap();
            assert_eq!(mode.as_i32(), i);
        }
    }

    #[test]
    fn flush_mode_display() {
        assert_eq!(format!("{}", FlushMode::NoFlush), "no flush");
        assert_eq!(format!("{}", FlushMode::Finish), "finish");
        assert_eq!(format!("{}", FlushMode::Trees), "trees");
    }

    #[test]
    fn flush_mode_clone_copy_eq() {
        let a = FlushMode::SyncFlush;
        let b = a;
        let c = a.clone();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_ne!(a, FlushMode::Finish);
    }

    #[test]
    fn flush_mode_debug() {
        // Verify Debug trait is implemented and produces reasonable output.
        let s = format!("{:?}", FlushMode::FullFlush);
        assert!(s.contains("FullFlush"));
    }

    // -------------------------------------------------------------------
    // CompressionLevel enum
    // -------------------------------------------------------------------

    #[test]
    fn compression_level_from_i32_valid() {
        assert_eq!(
            CompressionLevel::from_i32(-1),
            Some(CompressionLevel::Default)
        );
        assert_eq!(CompressionLevel::from_i32(0), Some(CompressionLevel::None));
        assert_eq!(
            CompressionLevel::from_i32(1),
            Some(CompressionLevel::BestSpeed)
        );
        assert_eq!(
            CompressionLevel::from_i32(2),
            Some(CompressionLevel::Level2)
        );
        assert_eq!(
            CompressionLevel::from_i32(3),
            Some(CompressionLevel::Level3)
        );
        assert_eq!(
            CompressionLevel::from_i32(4),
            Some(CompressionLevel::Level4)
        );
        assert_eq!(
            CompressionLevel::from_i32(5),
            Some(CompressionLevel::Level5)
        );
        assert_eq!(
            CompressionLevel::from_i32(6),
            Some(CompressionLevel::Default)
        );
        assert_eq!(
            CompressionLevel::from_i32(7),
            Some(CompressionLevel::Level7)
        );
        assert_eq!(
            CompressionLevel::from_i32(8),
            Some(CompressionLevel::Level8)
        );
        assert_eq!(
            CompressionLevel::from_i32(9),
            Some(CompressionLevel::BestCompression)
        );
    }

    #[test]
    fn compression_level_from_i32_invalid() {
        assert_eq!(CompressionLevel::from_i32(-2), None);
        assert_eq!(CompressionLevel::from_i32(10), None);
        assert_eq!(CompressionLevel::from_i32(100), None);
        assert_eq!(CompressionLevel::from_i32(i32::MIN), None);
        assert_eq!(CompressionLevel::from_i32(i32::MAX), None);
    }

    #[test]
    fn compression_level_as_i32() {
        assert_eq!(CompressionLevel::None.as_i32(), 0);
        assert_eq!(CompressionLevel::BestSpeed.as_i32(), 1);
        assert_eq!(CompressionLevel::Level2.as_i32(), 2);
        assert_eq!(CompressionLevel::Level3.as_i32(), 3);
        assert_eq!(CompressionLevel::Level4.as_i32(), 4);
        assert_eq!(CompressionLevel::Level5.as_i32(), 5);
        assert_eq!(CompressionLevel::Default.as_i32(), 6);
        assert_eq!(CompressionLevel::Level7.as_i32(), 7);
        assert_eq!(CompressionLevel::Level8.as_i32(), 8);
        assert_eq!(CompressionLevel::BestCompression.as_i32(), 9);
    }

    #[test]
    fn compression_level_round_trip() {
        for i in 0..=9 {
            let level = CompressionLevel::from_i32(i).unwrap();
            assert_eq!(level.as_i32(), i);
        }
    }

    #[test]
    fn compression_level_default_maps_to_6() {
        // Z_DEFAULT_COMPRESSION is -1, but resolves to level 6.
        let from_neg1 = CompressionLevel::from_i32(-1).unwrap();
        let from_6 = CompressionLevel::from_i32(6).unwrap();
        assert_eq!(from_neg1, from_6);
        assert_eq!(from_neg1.as_i32(), 6);
    }

    #[test]
    fn compression_level_display() {
        assert_eq!(
            format!("{}", CompressionLevel::None),
            "no compression (level 0)"
        );
        assert_eq!(
            format!("{}", CompressionLevel::BestSpeed),
            "best speed (level 1)"
        );
        assert_eq!(
            format!("{}", CompressionLevel::Default),
            "default (level 6)"
        );
        assert_eq!(
            format!("{}", CompressionLevel::BestCompression),
            "best compression (level 9)"
        );
    }

    #[test]
    fn compression_level_clone_copy_eq() {
        let a = CompressionLevel::BestCompression;
        let b = a;
        let c = a.clone();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_ne!(a, CompressionLevel::None);
    }

    #[test]
    fn compression_level_debug() {
        let s = format!("{:?}", CompressionLevel::Default);
        assert!(s.contains("Default"));
    }

    // -------------------------------------------------------------------
    // Derived constant correctness
    // -------------------------------------------------------------------

    #[test]
    fn derived_constants_are_correct() {
        // L_CODES = LITERALS + 1 + LENGTH_CODES = 256 + 1 + 29 = 286
        assert_eq!(L_CODES, LITERALS + 1 + LENGTH_CODES);
        // HEAP_SIZE = 2 * L_CODES + 1 = 2 * 286 + 1 = 573
        assert_eq!(HEAP_SIZE, 2 * L_CODES + 1);
        // ENOUGH = ENOUGH_LENS + ENOUGH_DISTS = 852 + 592 = 1444
        assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);
    }
}
