//! Public and internal constants for the zlib compression library.
//!
//! This module contains all constants ported from the original C zlib headers:
//! - Version information (`ZLIB_VERSION`, `ZLIB_VERNUM`, etc.) from `zlib.h`
//! - Flush mode constants (`Z_NO_FLUSH` through `Z_TREES`) from `zlib.h`
//! - Return code constants (`Z_OK` through `Z_VERSION_ERROR`) from `zlib.h`
//! - Compression level constants from `zlib.h`
//! - Compression strategy constants from `zlib.h`
//! - Data type constants from `zlib.h`
//! - Window and memory configuration constants from `zconf.h` and `zutil.h`
//! - Internal block type and match length constants from `zutil.h`
//! - Internal Huffman tree size constants from `deflate.h`
//!
//! All values are exact ports of their C `#define` equivalents. Public constants
//! use `pub` visibility for external crate access; internal implementation
//! constants use `pub(crate)` visibility.

// =============================================================================
// Version Constants
// Ported from zlib.h lines 44–49
// =============================================================================

/// The zlib library version string.
///
/// This must match the version expected by callers for compatibility checks
/// performed by `deflateInit` and `inflateInit`. Corresponds to the C macro
/// `ZLIB_VERSION` defined in `zlib.h`.
pub const ZLIB_VERSION: &str = "1.3.2.1-motley";

/// The zlib version number encoded as a hexadecimal constant.
///
/// The encoding is `0xMNRR` where `M` is the major version, `N` is the minor
/// version, and `RR` is the revision and sub-revision. For version 1.3.2.1
/// this yields `0x1321`. Corresponds to the C macro `ZLIB_VERNUM`.
pub const ZLIB_VERNUM: u32 = 0x1321;

/// Major version number of the zlib library.
pub const ZLIB_VER_MAJOR: u32 = 1;

/// Minor version number of the zlib library.
pub const ZLIB_VER_MINOR: u32 = 3;

/// Revision number of the zlib library.
pub const ZLIB_VER_REVISION: u32 = 2;

/// Sub-revision number of the zlib library.
pub const ZLIB_VER_SUBREVISION: u32 = 1;

// =============================================================================
// Flush Mode Constants
// Ported from zlib.h lines 172–178
// =============================================================================

/// No flush — accumulate input data without producing output.
///
/// The compressor may buffer data internally until enough is available to
/// produce a complete block. This is the default flush mode for streaming
/// compression. Corresponds to `Z_NO_FLUSH` in C zlib.
pub const Z_NO_FLUSH: i32 = 0;

/// Partial flush (deprecated — use [`Z_SYNC_FLUSH`] instead).
///
/// Retained for backward compatibility with older zlib versions. Corresponds
/// to `Z_PARTIAL_FLUSH` in C zlib.
pub const Z_PARTIAL_FLUSH: i32 = 1;

/// Sync flush — emit a sync point marker in the compressed output.
///
/// All pending output is flushed, and the output is aligned on a byte
/// boundary. The sync point marker (`00 00 FF FF`) enables
/// [`inflate_sync`](crate::inflate) recovery after data corruption.
/// Corresponds to `Z_SYNC_FLUSH` in C zlib.
pub const Z_SYNC_FLUSH: i32 = 2;

/// Full flush — reset compression state after emitting a sync point.
///
/// Like [`Z_SYNC_FLUSH`], but the compression state is reset so that
/// decompression can restart from this point if the previous compressed data
/// has been damaged. Corresponds to `Z_FULL_FLUSH` in C zlib.
pub const Z_FULL_FLUSH: i32 = 3;

/// Finish — signal the end of the input stream.
///
/// All pending input is processed and all pending output is flushed. If there
/// is enough output space, `deflate` will return [`Z_STREAM_END`]. Corresponds
/// to `Z_FINISH` in C zlib.
pub const Z_FINISH: i32 = 4;

/// Block mode — stop at the next deflate block boundary.
///
/// Used for fine-grained control of the compressed output, allowing the caller
/// to inspect or manipulate individual deflate blocks. Corresponds to
/// `Z_BLOCK` in C zlib.
pub const Z_BLOCK: i32 = 5;

/// Trees mode — emit raw Huffman tree data at the next block boundary.
///
/// When used with `inflate`, returns once the end-of-block code and Huffman
/// codes for the next block have been received. Corresponds to `Z_TREES` in
/// C zlib.
pub const Z_TREES: i32 = 6;

// =============================================================================
// Return Code Constants
// Ported from zlib.h lines 181–189
// These integer constants complement the ReturnCode enum for FFI compatibility.
// =============================================================================

/// Operation completed successfully.
///
/// Corresponds to `Z_OK` (0) in C zlib.
pub const Z_OK: i32 = 0;

/// End of the compressed stream has been reached.
///
/// Corresponds to `Z_STREAM_END` (1) in C zlib.
pub const Z_STREAM_END: i32 = 1;

/// A preset dictionary is needed for decompression.
///
/// The decompressor encountered a zlib stream that requires a preset
/// dictionary. The `adler` field of the stream contains the Adler-32 checksum
/// of the required dictionary. Corresponds to `Z_NEED_DICT` (2) in C zlib.
pub const Z_NEED_DICT: i32 = 2;

/// A file I/O error occurred.
///
/// Only returned by gzip file operations. The application should check `errno`
/// for the specific error. Corresponds to `Z_ERRNO` (-1) in C zlib.
pub const Z_ERRNO: i32 = -1;

/// Inconsistent stream state or invalid parameter.
///
/// Indicates that the stream structure was inconsistent (for example, `next_in`
/// or `next_out` was null, or the state was corrupted), or that a parameter
/// was invalid. Corresponds to `Z_STREAM_ERROR` (-2) in C zlib.
pub const Z_STREAM_ERROR: i32 = -2;

/// The compressed data was corrupted.
///
/// The input data does not conform to the expected compressed format, or the
/// checksum verification failed. Corresponds to `Z_DATA_ERROR` (-3) in C zlib.
pub const Z_DATA_ERROR: i32 = -3;

/// Insufficient memory to complete the operation.
///
/// Corresponds to `Z_MEM_ERROR` (-4) in C zlib.
pub const Z_MEM_ERROR: i32 = -4;

/// The output buffer was too small.
///
/// No progress was possible because the output buffer was full, the input
/// buffer was empty, or an internal limitation was reached. The application
/// should provide more output space or more input data and retry. Corresponds
/// to `Z_BUF_ERROR` (-5) in C zlib.
pub const Z_BUF_ERROR: i32 = -5;

/// The zlib library version is incompatible.
///
/// The version of the zlib library does not match the version expected by the
/// caller. Typically returned when the header file version (`ZLIB_VERSION`)
/// does not match the library version. Corresponds to `Z_VERSION_ERROR` (-6)
/// in C zlib.
pub const Z_VERSION_ERROR: i32 = -6;

// =============================================================================
// Compression Level Constants
// Ported from zlib.h lines 194–197
// =============================================================================

/// No compression — input data is stored without compression.
///
/// Corresponds to `Z_NO_COMPRESSION` (0) in C zlib.
pub const Z_NO_COMPRESSION: i32 = 0;

/// Best speed — fastest compression with lowest compression ratio.
///
/// Equivalent to compression level 1. Corresponds to `Z_BEST_SPEED` (1) in
/// C zlib.
pub const Z_BEST_SPEED: i32 = 1;

/// Best compression — highest compression ratio at the cost of speed.
///
/// Equivalent to compression level 9. Corresponds to `Z_BEST_COMPRESSION` (9)
/// in C zlib.
pub const Z_BEST_COMPRESSION: i32 = 9;

/// Default compression level — a compromise between speed and compression.
///
/// Currently equivalent to level 6 in the zlib implementation. Corresponds to
/// `Z_DEFAULT_COMPRESSION` (-1) in C zlib.
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

// =============================================================================
// Compression Strategy Constants
// Ported from zlib.h lines 200–204
// =============================================================================

/// Filtered data strategy.
///
/// Optimized for data produced by a filter or predictor. Filtered data
/// consists mostly of small values with a somewhat random distribution. This
/// strategy forces more Huffman coding and less string matching. Corresponds
/// to `Z_FILTERED` (1) in C zlib.
pub const Z_FILTERED: i32 = 1;

/// Huffman-only strategy — no string matching.
///
/// Forces Huffman encoding only, with no string matching at all. This is
/// useful for data that is already transformed (e.g., by a separate LZ77
/// stage). Corresponds to `Z_HUFFMAN_ONLY` (2) in C zlib.
pub const Z_HUFFMAN_ONLY: i32 = 2;

/// Run-length encoding strategy.
///
/// Designed for data with many runs of identical bytes. Limits match distances
/// to one, producing output comparable to run-length encoding. Corresponds to
/// `Z_RLE` (3) in C zlib.
pub const Z_RLE: i32 = 3;

/// Fixed Huffman codes strategy.
///
/// Prevents the use of dynamic Huffman codes, using only the pre-defined
/// fixed Huffman code tables. This can be useful for very short messages where
/// the overhead of transmitting dynamic tables exceeds the compression benefit.
/// Corresponds to `Z_FIXED` (4) in C zlib.
pub const Z_FIXED: i32 = 4;

/// Default compression strategy.
///
/// Suitable for general-purpose compression of typical data. Uses a
/// combination of string matching and Huffman coding. Corresponds to
/// `Z_DEFAULT_STRATEGY` (0) in C zlib.
pub const Z_DEFAULT_STRATEGY: i32 = 0;

// =============================================================================
// Data Type Constants
// Ported from zlib.h lines 207–210
// =============================================================================

/// Binary data type.
///
/// Indicates that the data being compressed is binary. Used as a possible
/// value of the `data_type` field in the stream. Corresponds to `Z_BINARY` (0)
/// in C zlib.
pub const Z_BINARY: i32 = 0;

/// Text data type.
///
/// Indicates that the data being compressed is text. Used as a possible value
/// of the `data_type` field in the stream. Corresponds to `Z_TEXT` (1) in
/// C zlib.
pub const Z_TEXT: i32 = 1;

/// Unknown data type.
///
/// Indicates that the data type has not yet been determined. Used as a possible
/// value of the `data_type` field in the stream. Corresponds to `Z_UNKNOWN` (2)
/// in C zlib.
pub const Z_UNKNOWN: i32 = 2;

/// ASCII data type alias — for compatibility with zlib versions 1.2.2 and
/// earlier.
///
/// This is an alias for [`Z_TEXT`]. Corresponds to `Z_ASCII` in C zlib, which
/// is defined as `Z_TEXT` for backward compatibility.
pub const Z_ASCII: i32 = Z_TEXT;

// =============================================================================
// Method and Miscellaneous Constants
// Ported from zlib.h lines 213–216
// =============================================================================

/// The deflate compression method.
///
/// This is the only compression method supported by zlib. The value 8
/// identifies the deflate algorithm as specified in RFC 1951. Corresponds to
/// `Z_DEFLATED` (8) in C zlib.
pub const Z_DEFLATED: i32 = 8;

/// Null value for zlib pointer and handle fields.
///
/// Used as a sentinel for initializing allocator function pointers (`zalloc`,
/// `zfree`) and the opaque data pointer in the stream structure. Corresponds
/// to `Z_NULL` (0) in C zlib.
pub const Z_NULL: i32 = 0;

// =============================================================================
// Window and Memory Configuration Constants
// Ported from zconf.h lines 272–288 and zutil.h lines 71–84
// =============================================================================

/// Maximum window size in bits (log2 of the LZ77 sliding window size).
///
/// The default and maximum value is 15, corresponding to a 32 KiB sliding
/// window. Valid values range from 9 (512 bytes) to 15 (32 KiB). The
/// `windowBits` parameter also encodes the container format:
/// - Positive (1–15): zlib format (RFC 1950)
/// - Negative (-1 to -15): raw deflate (RFC 1951)
/// - Positive + 16 (17–31): gzip format (RFC 1952)
/// - Positive + 32 (33–47): auto-detect zlib or gzip
///
/// Corresponds to `MAX_WBITS` (15) in C zlib.
pub const MAX_WBITS: i32 = 15;

/// Maximum memory level for internal compression state.
///
/// Controls how much memory the deflate engine allocates for internal data
/// structures. The value 9 uses maximum memory for best compression.
/// Valid values range from 1 (minimum memory, slower, lower compression)
/// to 9 (maximum memory, faster, better compression). Corresponds to
/// `MAX_MEM_LEVEL` (9) in C zlib.
pub const MAX_MEM_LEVEL: i32 = 9;

/// Default memory level for internal compression state.
///
/// A good balance between memory usage and compression performance. At
/// `MAX_WBITS` = 15 and `DEF_MEM_LEVEL` = 8, deflate uses approximately
/// 256 KiB of memory (128 KiB window + 128 KiB hash/match structures).
/// Corresponds to `DEF_MEM_LEVEL` (8) in C zlib.
pub const DEF_MEM_LEVEL: i32 = 8;

/// Default window bits for decompression.
///
/// Set to [`MAX_WBITS`] (15), which allows decompression of any standard
/// zlib stream. Corresponds to `DEF_WBITS` in C zlib, defined in `zutil.h`.
pub const DEF_WBITS: i32 = MAX_WBITS;

// =============================================================================
// Block Type Constants (Internal)
// Ported from zutil.h lines 87–89
// =============================================================================

/// Stored block type — data is stored without compression.
///
/// The block contains raw uncompressed data preceded by a 16-bit length
/// and its one's complement. Corresponds to `STORED_BLOCK` (0) in C zlib.
pub(crate) const STORED_BLOCK: i32 = 0;

/// Static trees block type — block uses pre-defined fixed Huffman tables.
///
/// The block is compressed using the fixed Huffman code tables defined in
/// RFC 1951, Section 3.2.6. No code table data is stored in the block.
/// Corresponds to `STATIC_TREES` (1) in C zlib.
pub(crate) const STATIC_TREES: i32 = 1;

/// Dynamic trees block type — block uses custom Huffman tables.
///
/// The block is compressed using Huffman code tables that are computed
/// specifically for the data and stored at the beginning of the block.
/// Corresponds to `DYN_TREES` (2) in C zlib.
pub(crate) const DYN_TREES: i32 = 2;

// =============================================================================
// Match Length Constants (Internal)
// Ported from zutil.h lines 92–93
// =============================================================================

/// Minimum match length in the DEFLATE algorithm.
///
/// A match of fewer than 3 bytes is not encoded as a back-reference because
/// the overhead of the length-distance pair exceeds the savings. This value
/// is mandated by the DEFLATE specification (RFC 1951). Corresponds to
/// `MIN_MATCH` (3) in C zlib.
pub(crate) const MIN_MATCH: usize = 3;

/// Maximum match length in the DEFLATE algorithm.
///
/// The longest allowed back-reference in a single length-distance pair is
/// 258 bytes, as defined by the DEFLATE specification (RFC 1951). Corresponds
/// to `MAX_MATCH` (258) in C zlib.
pub(crate) const MAX_MATCH: usize = 258;

// =============================================================================
// Header Flag Constants (Internal)
// Ported from zutil.h line 96
// =============================================================================

/// Preset dictionary flag in the zlib header.
///
/// When this bit (0x20) is set in the `FLG` byte of a zlib header, the stream
/// was compressed using a preset dictionary. The Adler-32 checksum of the
/// dictionary follows the header. Corresponds to `PRESET_DICT` (0x20) in
/// C zlib.
pub(crate) const PRESET_DICT: u32 = 0x20;

// =============================================================================
// Huffman Tree and Deflate State Constants (Internal)
// Ported from deflate.h lines 34–56
// =============================================================================

/// Number of length codes, not counting the special `END_BLOCK` code.
///
/// The DEFLATE specification defines 29 length codes (codes 257–285) that
/// represent match lengths from 3 to 258. Corresponds to `LENGTH_CODES` (29)
/// in C zlib.
pub(crate) const LENGTH_CODES: usize = 29;

/// Number of literal byte values (0–255).
///
/// The 256 literal byte values are encoded directly in the Huffman tree
/// alongside length codes and the end-of-block marker. Corresponds to
/// `LITERALS` (256) in C zlib.
pub(crate) const LITERALS: usize = 256;

/// Total number of literal or length codes, including the `END_BLOCK` code.
///
/// Calculated as `LITERALS` (256) + 1 (end-of-block) + `LENGTH_CODES` (29) =
/// 286. Corresponds to `L_CODES` in C zlib.
pub(crate) const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes in the DEFLATE specification.
///
/// The 30 distance codes represent back-reference distances from 1 to 32768
/// bytes. Corresponds to `D_CODES` (30) in C zlib.
pub(crate) const D_CODES: usize = 30;

/// Number of bit-length codes used to encode the Huffman tree itself.
///
/// When transmitting dynamic Huffman tables, the bit lengths of the code
/// entries are themselves Huffman-encoded using up to 19 code-length codes.
/// Corresponds to `BL_CODES` (19) in C zlib.
pub(crate) const BL_CODES: usize = 19;

/// Maximum Huffman tree heap size.
///
/// The heap used for building Huffman trees can hold at most `2 * L_CODES + 1`
/// entries (573). This accommodates all literal/length codes plus internal
/// tree nodes. Corresponds to `HEAP_SIZE` in C zlib.
pub(crate) const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// Maximum number of bits in any Huffman code.
///
/// All codes in the DEFLATE format must not exceed 15 bits, as mandated by
/// the specification (RFC 1951). Corresponds to `MAX_BITS` (15) in C zlib.
pub(crate) const MAX_BITS: usize = 15;

/// Size of the bit buffer in bits.
///
/// The pending output bit buffer holds 16 bits before being flushed to the
/// output byte stream. Corresponds to `Buf_size` (16) in C zlib (`deflate.h`).
pub(crate) const BUF_SIZE: usize = 16;
