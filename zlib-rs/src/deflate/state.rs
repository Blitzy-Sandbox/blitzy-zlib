// Deflate engine internal state and supporting types.
//
// Ported from `deflate.h` (lines 58–288) and the initialization sections of
// `deflate.c` (lines 385–533, 644–701) of the zlib 1.3.2.1-motley C library.
//
// This module contains:
//
// - [`DeflateStatus`] — The eight-state progression enum for the deflate
//   state machine, replacing the C integer constants `INIT_STATE` (42)
//   through `FINISH_STATE` (666).
//
// - [`DeflateState`] — The core data structure that owns **all** deflate
//   engine resources: sliding window, hash chains, pending output buffer,
//   Huffman trees, and compression parameters. All raw-pointer fields from
//   the C `deflate_state` are replaced with owned `Vec<u8>` / `Vec<u16>` /
//   `Vec<CtData>` buffers, providing automatic deallocation via `Drop`.
//
// - [`CtData`] — Huffman code/frequency node, replacing the C `ct_data`
//   union type.
//
// - [`TreeDescState`] — Per-stream mutable tree descriptor tracking the
//   maximum code index, replacing the C `tree_desc` struct.
//
// - [`Pos`] / [`IPos`] — Newtype wrappers over `u16` / `u32` for type-safe
//   window position indices, replacing the C `Pos` and `IPos` typedefs.
//
// # Safety
//
// This module contains **zero** `unsafe` blocks. All buffer management uses
// safe Rust indexing and owned collections.

use crate::constants::{
    BL_CODES, BUF_SIZE, D_CODES, HEAP_SIZE, L_CODES, MAX_BITS, MAX_MATCH, MIN_MATCH,
    Z_UNKNOWN,
};
use crate::stream::GzHeader;
use super::params::{CompressionConfig, CONFIGURATION_TABLE, get_config};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Size of the dynamic distance tree array.
///
/// Matches the C declaration `dyn_dtree[2*D_CODES+1]` in `deflate.h` line 203.
const DYN_DTREE_SIZE: usize = 2 * D_CODES + 1;

/// Size of the bit-length tree array.
///
/// Matches the C declaration `bl_tree[2*BL_CODES+1]` in `deflate.h` line 204.
const BL_TREE_SIZE: usize = 2 * BL_CODES + 1;

/// Number of bytes after end of data in window to initialize in order to avoid
/// memory checker errors from `longest_match()` routines.
///
/// Ported from `deflate.h` line 306: `#define WIN_INIT MAX_MATCH`.
// Retained for C API parity; currently unused because the Rust implementation
// relies on `Vec` zero-initialization instead of explicit `WIN_INIT` filling.
#[allow(dead_code)]
pub(crate) const WIN_INIT: usize = MAX_MATCH;

// ─── Compile-Time Invariant Assertions ────────────────────────────────────────
// These verify that the Huffman tree array sizes match the C definitions.

/// Verify that `HEAP_SIZE` == 2 * `L_CODES` + 1 (from `deflate.h` line 49).
const _HEAP_SIZE_CHECK: () = assert!(HEAP_SIZE == 2 * L_CODES + 1);

/// Verify that the bit buffer width matches the expected 16-bit value.
const _BUF_SIZE_CHECK: () = assert!(BUF_SIZE == 16);

// ─── DeflateStatus ────────────────────────────────────────────────────────────

/// Deflate stream status, tracking the state machine progression.
///
/// The deflate engine moves through these states sequentially during
/// compression. The C implementation uses integer constants scattered across
/// `deflate.h` lines 58–67; this enum replaces them with a type-safe
/// representation that makes invalid state transitions a compile-time error.
///
/// # C Constant Mapping
///
/// | Variant       | C Name          | C Value |
/// |---------------|-----------------|---------|
/// | `Init`        | `INIT_STATE`    |      42 |
/// | `GzipHeader`  | `GZIP_STATE`    |      57 |
/// | `Extra`       | `EXTRA_STATE`   |      69 |
/// | `Name`        | `NAME_STATE`    |      73 |
/// | `Comment`     | `COMMENT_STATE` |      91 |
/// | `Hcrc`        | `HCRC_STATE`    |     103 |
/// | `Busy`        | `BUSY_STATE`    |     113 |
/// | `Finish`      | `FINISH_STATE`  |     666 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeflateStatus {
    /// Initial state — zlib header pending.
    ///
    /// Entered after `deflateInit2` completes successfully for non-gzip
    /// streams. The next call to `deflate()` will emit the two-byte zlib
    /// header and transition to [`Busy`](DeflateStatus::Busy).
    Init,

    /// Gzip header pending.
    ///
    /// Entered after `deflateInit2` with `wrap == 2` (gzip wrapper). The
    /// engine will emit the 10-byte gzip header, then optionally transition
    /// through [`Extra`], [`Name`], [`Comment`], and [`Hcrc`] states before
    /// reaching [`Busy`](DeflateStatus::Busy).
    GzipHeader,

    /// Gzip extra field pending.
    ///
    /// The engine is writing the gzip `FEXTRA` data. Transitions to
    /// [`Name`](DeflateStatus::Name) when complete.
    Extra,

    /// Gzip file name pending.
    ///
    /// The engine is writing the gzip `FNAME` (original file name) field.
    /// Transitions to [`Comment`](DeflateStatus::Comment) when complete.
    Name,

    /// Gzip comment pending.
    ///
    /// The engine is writing the gzip `FCOMMENT` field. Transitions to
    /// [`Hcrc`](DeflateStatus::Hcrc) when complete.
    Comment,

    /// Gzip header CRC pending.
    ///
    /// The engine is writing the two-byte `FHCRC` CRC-16 of the gzip header.
    /// Transitions to [`Busy`](DeflateStatus::Busy) when complete.
    Hcrc,

    /// Actively compressing input data.
    ///
    /// The engine is processing input through the LZ77 matching and Huffman
    /// encoding pipeline. This is the main operational state.
    Busy,

    /// Stream complete — no more input accepted.
    ///
    /// Entered after `deflate()` is called with `Z_FINISH` and all output has
    /// been produced. The only valid operations on the stream after this point
    /// are `deflateEnd()` or `deflateReset()`.
    Finish,
}

impl DeflateStatus {
    /// Converts this status to its corresponding C integer constant.
    ///
    /// This is used at the FFI boundary to populate the `status` field of
    /// the C-compatible `internal_state` structure.
    ///
    /// # Returns
    ///
    /// The integer value matching the original C `#define`:
    /// - `Init` → 42, `GzipHeader` → 57, `Extra` → 69, `Name` → 73,
    ///   `Comment` → 91, `Hcrc` → 103, `Busy` → 113, `Finish` → 666.
    #[inline]
    #[must_use]
    pub fn as_c_int(self) -> i32 {
        match self {
            Self::Init => 42,
            Self::GzipHeader => 57,
            Self::Extra => 69,
            Self::Name => 73,
            Self::Comment => 91,
            Self::Hcrc => 103,
            Self::Busy => 113,
            Self::Finish => 666,
        }
    }

    /// Attempts to convert a C integer status code to a `DeflateStatus`.
    ///
    /// Returns `None` if the value does not correspond to any known state.
    #[inline]
    #[must_use]
    pub fn from_c_int(value: i32) -> Option<Self> {
        match value {
            42 => Some(Self::Init),
            57 => Some(Self::GzipHeader),
            69 => Some(Self::Extra),
            73 => Some(Self::Name),
            91 => Some(Self::Comment),
            103 => Some(Self::Hcrc),
            113 => Some(Self::Busy),
            666 => Some(Self::Finish),
            _ => None,
        }
    }
}

// ─── Pos / IPos Newtypes ──────────────────────────────────────────────────────

/// Window position index.
///
/// A newtype wrapper over [`u16`] that replaces the C `Pos` typedef
/// (`unsigned short`) from `deflate.h` line 96. Using a distinct type
/// prevents accidental mixing of window positions with other `u16` values
/// (e.g., hash values, code lengths).
///
/// The `u16` range limits the sliding window to at most 65 535 bytes, which
/// is sufficient for the maximum DEFLATE window size of 32 768 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Pos(pub u16);

/// Extended position for parameter passing.
///
/// A newtype wrapper over [`u32`] that replaces the C `IPos` typedef
/// (`unsigned int`) from `deflate.h` line 98. Used where a wider range is
/// needed than [`Pos`] provides, particularly in function parameters and
/// intermediate calculations during match searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct IPos(pub u32);

// ─── CtData ───────────────────────────────────────────────────────────────────

/// Huffman code table entry, replacing the C `ct_data` union type.
///
/// In the C implementation (`deflate.h` lines 72–81), `ct_data` uses two
/// unions — `fc` (holding either `freq` or `code`) and `dl` (holding either
/// `dad` or `len`). The union members are accessed at different phases:
///
/// - **Tree building phase:** `freq` (symbol frequency count) and `dad`
///   (parent node index in the Huffman tree).
/// - **Encoding phase:** `code` (Huffman bit string) and `len` (bit string
///   length).
///
/// In safe Rust, unions require `unsafe` for access. Instead, this struct
/// stores all four fields independently. The minor memory overhead (4 extra
/// bytes per entry compared to C's union layout) is negligible relative to
/// the total deflate state footprint (~300 KB).
///
/// # Memory Layout
///
/// | Field  | C Union Member | Phase          |
/// |--------|---------------|----------------|
/// | `freq` | `fc.freq`     | Tree building  |
/// | `code` | `fc.code`     | Encoding       |
/// | `dad`  | `dl.dad`      | Tree building  |
/// | `len`  | `dl.len`      | Encoding       |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtData {
    /// Frequency count of this symbol (tree building phase).
    ///
    /// Incremented for each occurrence of the symbol during block scanning.
    /// After tree construction, this field is stale — use [`code`](CtData::code)
    /// instead.
    pub freq: u16,

    /// Huffman bit string for this symbol (encoding phase).
    ///
    /// Assigned during tree construction. Before tree construction, this
    /// field is uninitialized — use [`freq`](CtData::freq) instead.
    pub code: u16,

    /// Parent node index in the Huffman tree (tree building phase).
    ///
    /// Used by the tree construction algorithm to track the tree structure.
    /// After tree construction, this field is stale — use [`len`](CtData::len)
    /// instead.
    pub dad: u16,

    /// Length of the Huffman bit string in bits (encoding phase).
    ///
    /// Assigned during tree construction. Before tree construction, this
    /// field is uninitialized — use [`dad`](CtData::dad) instead.
    pub len: u16,
}

impl Default for CtData {
    /// Returns a zeroed `CtData` entry.
    ///
    /// All fields are initialized to zero, matching the C behavior of
    /// `zmemzero` on the `ct_data` arrays during deflate state initialization.
    #[inline]
    fn default() -> Self {
        Self {
            freq: 0,
            code: 0,
            dad: 0,
            len: 0,
        }
    }
}

// ─── TreeDescState ────────────────────────────────────────────────────────────

/// Mutable per-stream tree descriptor.
///
/// Replaces the C `tree_desc` struct from `deflate.h` lines 90–94. In the
/// C version, this struct holds a pointer to the dynamic tree array, the
/// maximum code index, and a pointer to the corresponding static tree
/// descriptor. In the Rust version:
///
/// - The dynamic tree arrays (`dyn_ltree`, `dyn_dtree`, `bl_tree`) are stored
///   directly as fields of [`DeflateState`], not behind pointers.
/// - The static tree descriptors are compile-time constants in the `trees`
///   module and are selected by the tree-building functions based on context.
/// - Only `max_code` needs per-stream mutable tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeDescState {
    /// Largest code with non-zero frequency in the corresponding dynamic tree.
    ///
    /// Set during Huffman tree construction (`_tr_init` / `build_tree`) and
    /// used by the block flushing logic to determine the range of codes to
    /// emit. A value of `-1` indicates that no codes have been tallied yet.
    pub max_code: i32,
}

impl Default for TreeDescState {
    /// Returns a `TreeDescState` with `max_code` set to `-1` (no codes).
    #[inline]
    fn default() -> Self {
        Self { max_code: -1 }
    }
}

// ─── DeflateState ─────────────────────────────────────────────────────────────

/// Core internal state for the DEFLATE compression engine.
///
/// This struct owns **all** resources required by the deflate compressor:
/// the sliding window, hash chains for string matching, the pending output
/// buffer, Huffman tree arrays, and the symbol buffer. It replaces the C
/// `deflate_state` / `internal_state` struct defined in `deflate.h`
/// lines 104–288 (~50 fields).
///
/// # Ownership Model
///
/// In the C implementation, all buffers are allocated via the caller-supplied
/// `zalloc` hook and freed via `zfree` in `deflateEnd`. In this Rust port,
/// buffers are owned [`Vec`] collections whose lifetimes are tied to the
/// `DeflateState` scope. When the `DeflateState` is dropped, all buffers are
/// automatically freed — no explicit `deflateEnd` cleanup is required for
/// memory safety (though calling the equivalent reset or teardown function is
/// still needed for protocol-level correctness).
///
/// # Memory Footprint
///
/// At default settings (`windowBits = 15`, `memLevel = 8`), the heap
/// allocations total approximately 300 KiB:
///
/// | Buffer        | Size (bytes) | Calculation                      |
/// |---------------|-------------|----------------------------------|
/// | `window`      |      65 536 | `2 × 32 768`                    |
/// | `prev`        |      65 536 | `32 768 × 2` (u16)              |
/// | `head`        |      65 536 | `32 768 × 2` (u16)              |
/// | `pending_buf` |      65 536 | `16 384 × 4`                    |
/// | `sym_buf`     |      49 152 | `16 384 × 3`                    |
/// | Trees + heap  |       ~6 KB | Fixed arrays in struct           |
///
/// The Rust version allocates `pending_buf` and `sym_buf` independently
/// (the C version overlays them). This adds ~49 KiB vs. C's ~256 KiB total,
/// but simplifies the implementation by eliminating the overlaid-buffer
/// invariant analysis that makes the C version tricky to reason about.
///
/// # Thread Safety
///
/// `DeflateState` is `Send` but not `Sync` — it can be moved between threads
/// but must not be accessed concurrently. This matches the C zlib threading
/// model where each `z_stream` must be used from a single thread at a time.
#[derive(Debug, Clone)]
pub struct DeflateState {
    // ─── Stream Status ───────────────────────────────────────────────────

    /// Current state in the deflate state machine.
    ///
    /// Tracks progression from header emission through active compression
    /// to stream completion. See [`DeflateStatus`] for the full state diagram.
    pub status: DeflateStatus,

    // ─── Pending Output Buffer ───────────────────────────────────────────

    /// Output buffer for compressed data not yet flushed to the stream.
    ///
    /// Compressed bytes are appended here by the Huffman encoder and block
    /// flushing logic. The deflate module copies bytes from this buffer to
    /// the stream output when `flush_pending()` is called.
    pub pending_buf: Vec<u8>,

    /// Number of bytes in `pending_buf` waiting to be written to the output.
    ///
    /// After a call to `deflate()`, this many bytes starting from index 0
    /// of `pending_buf` contain valid compressed output not yet consumed by
    /// `flush_pending()`.
    pub pending: usize,

    /// Maximum capacity of `pending_buf` in bytes.
    ///
    /// Calculated as `lit_bufsize × 4` (the non-`LIT_MEM` path from
    /// `deflate.c` line 506). Set once during initialization; never changes.
    pub pending_buf_size: usize,

    // ─── Header / Trailer State ──────────────────────────────────────────

    /// Wrapper format selector.
    ///
    /// - `0` — raw DEFLATE (no header or trailer)
    /// - `1` — zlib format (RFC 1950)
    /// - `2` — gzip format (RFC 1952)
    ///
    /// May be temporarily negated by `deflate(Z_FINISH)` to signal that
    /// the trailer has been written; `reset()` restores the absolute value.
    pub wrap: i32,

    /// Gzip header fields to write, or `None` for zlib/raw streams.
    ///
    /// Set by `deflateSetHeader()`. The engine reads these fields during
    /// the `GzipHeader` → `Extra` → `Name` → `Comment` → `Hcrc` state
    /// transitions to emit the gzip header bytes.
    pub gzip_header: Option<GzHeader>,

    /// Index into the gzip header's variable-length fields during writing.
    ///
    /// Tracks how many bytes of the `extra`, `name`, or `comment` field
    /// have been emitted so far. Reset to zero when transitioning states.
    pub gzip_index: usize,

    // ─── Compression Method ──────────────────────────────────────────────

    /// Compression method identifier.
    ///
    /// Always `Z_DEFLATED` (8). Stored here for protocol header emission.
    pub method: u8,

    // ─── Flush Handling ──────────────────────────────────────────────────

    /// Value of the `flush` parameter from the previous `deflate()` call.
    ///
    /// Used to detect whether the caller has changed flush mode between
    /// calls, which affects block boundary decisions. Initialized to `-2`
    /// (a value that never matches any valid flush mode) during reset.
    pub last_flush: i32,

    // ─── Sliding Window ──────────────────────────────────────────────────

    /// Sliding window buffer.
    ///
    /// Input bytes are read into the second half of the window, then slid
    /// to the first half to maintain a dictionary of at least `w_size`
    /// bytes. Size is `2 × w_size` bytes.
    pub window: Vec<u8>,

    /// Actual allocated window size in bytes.
    ///
    /// Normally `2 × w_size`. Set to `2 × w_size` by `reset()`.
    pub window_size: usize,

    /// LZ77 window size — half the physical window buffer.
    ///
    /// Always a power of two (typically 32 768 for `windowBits = 15`).
    /// Matches are limited to distances of at most `w_size - MIN_LOOKAHEAD`.
    pub w_size: usize,

    /// `log2(w_size)` — the window bits parameter (8–15).
    pub w_bits: usize,

    /// `w_size - 1` — bit mask for fast modular window indexing.
    pub w_mask: usize,

    // ─── Hash Chains ─────────────────────────────────────────────────────

    /// Previous-position links for hash chains.
    ///
    /// `prev[pos & w_mask]` holds the previous window position that shares
    /// the same hash value as position `pos`. Size is `w_size` entries.
    pub prev: Vec<u16>,

    /// Hash table heads.
    ///
    /// `head[h]` holds the most recent window position whose three-byte
    /// string hashes to value `h`. Size is `hash_size` entries. A value of
    /// zero (`NIL`) means no string has hashed to this bucket yet.
    pub head: Vec<u16>,

    /// Running hash value for the three-byte string at `strstart`.
    ///
    /// Updated incrementally via the `UPDATE_HASH` equivalent as the window
    /// position advances.
    pub ins_h: u32,

    /// Hash table size — always a power of two.
    ///
    /// Calculated as `1 << hash_bits` (typically 32 768 for `memLevel = 8`).
    pub hash_size: usize,

    /// `log2(hash_size)` — typically `memLevel + 7`.
    pub hash_bits: usize,

    /// `hash_size - 1` — bit mask for fast modular hash indexing.
    pub hash_mask: u32,

    /// Number of bits to shift per hash step.
    ///
    /// Calculated as `(hash_bits + MIN_MATCH - 1) / MIN_MATCH` to ensure
    /// that after `MIN_MATCH` steps the oldest byte no longer participates
    /// in the hash key.
    pub hash_shift: u32,

    // ─── Match State ─────────────────────────────────────────────────────

    /// Number of valid bytes ahead of `strstart` in the window.
    pub lookahead: usize,

    /// Start of the string to insert into the hash table / to match against.
    pub strstart: usize,

    /// Start position of the best match found by the current search.
    pub match_start: usize,

    /// Length of the best match found by the current search.
    ///
    /// Initialized to `MIN_MATCH - 1` so the first match must be at least
    /// `MIN_MATCH` bytes to be accepted.
    pub match_length: usize,

    /// Length of the best match at the **previous** step (lazy evaluation).
    pub prev_length: usize,

    /// Window position of the previous match (for lazy evaluation).
    pub prev_match: usize,

    /// Whether a match from the previous step is pending output.
    ///
    /// Used by `deflate_slow()` for lazy match evaluation.
    pub match_available: bool,

    /// Number of bytes at the end of the window pending hash insertion.
    pub insert: usize,

    // ─── Block State ─────────────────────────────────────────────────────

    /// Window position at the beginning of the current output block.
    ///
    /// May become negative when the window is slid backwards, which is why
    /// this is an `i64` rather than `usize`.
    pub block_start: i64,

    /// Whether the hash table has been slid since the last `CLEAR_HASH`.
    pub slid: bool,

    // ─── Compression Configuration ───────────────────────────────────────

    /// Maximum hash chain length to search per match attempt.
    pub max_chain_length: usize,

    /// Reduce lazy search above this match length.
    pub good_match: usize,

    /// Quit search when a match of at least this length is found.
    pub nice_match: usize,

    /// Do not perform lazy search above this match length.
    ///
    /// For levels 1–3 this also serves as `max_insert_length` (the C
    /// preprocessor alias `#define max_insert_length max_lazy_match`).
    pub max_lazy_match: usize,

    /// Compression level (0–9).
    pub level: i32,

    /// Compression strategy (`Z_DEFAULT_STRATEGY`, `Z_FILTERED`, etc.).
    pub strategy: i32,

    // ─── Huffman Trees ───────────────────────────────────────────────────

    /// Dynamic literal/length Huffman tree.
    ///
    /// Size is `HEAP_SIZE` (573) entries, matching `dyn_ltree[HEAP_SIZE]`.
    pub dyn_ltree: Vec<CtData>,

    /// Dynamic distance Huffman tree.
    ///
    /// Size is `2 × D_CODES + 1` (61) entries.
    pub dyn_dtree: Vec<CtData>,

    /// Bit-length Huffman tree for encoding tree code lengths.
    ///
    /// Size is `2 × BL_CODES + 1` (39) entries.
    pub bl_tree: Vec<CtData>,

    /// Tree descriptor for the literal/length tree.
    pub l_desc: TreeDescState,

    /// Tree descriptor for the distance tree.
    pub d_desc: TreeDescState,

    /// Tree descriptor for the bit-length tree.
    pub bl_desc: TreeDescState,

    /// Bit length count for optimal Huffman tree construction.
    ///
    /// Array size is `MAX_BITS + 1` (16).
    pub bl_count: [u16; MAX_BITS + 1],

    /// Heap used to build Huffman trees (1-indexed).
    ///
    /// Array size is `HEAP_SIZE` (573).
    pub heap: [i32; HEAP_SIZE],

    /// Number of elements currently in the heap.
    pub heap_len: usize,

    /// Element of largest frequency in the heap.
    pub heap_max: usize,

    /// Depth of each subtree (tie-breaker for equal frequencies).
    ///
    /// Array size is `HEAP_SIZE` (573).
    pub depth: [u8; HEAP_SIZE],

    // ─── Symbol Buffer ───────────────────────────────────────────────────

    /// Buffer for match distances and literal bytes (3 bytes per symbol).
    ///
    /// Size is `lit_bufsize × 3`.
    pub sym_buf: Vec<u8>,

    /// Running index into `sym_buf` for the next symbol to write.
    pub sym_next: usize,

    /// Capacity limit — block must flush when `sym_next` reaches this.
    ///
    /// Set to `(lit_bufsize - 1) × 3`.
    pub sym_end: usize,

    /// Number of literal/length values that fit in the symbol buffer.
    ///
    /// Calculated as `1 << (memLevel + 6)`.
    pub lit_bufsize: usize,

    /// Number of distance matches in the current block.
    pub matches: u32,

    // ─── Bit Buffer ──────────────────────────────────────────────────────

    /// Pending output bit accumulator (16-bit width).
    pub bi_buf: u16,

    /// Number of valid bits in `bi_buf`.
    pub bi_valid: i32,

    /// Last number of used bits when going to a byte boundary.
    pub bi_used: i32,

    // ─── Bookkeeping ─────────────────────────────────────────────────────

    /// Bit length of the current block with optimal Huffman trees.
    pub opt_len: u64,

    /// Bit length of the current block with static Huffman trees.
    pub static_len: u64,

    /// High water mark for window initialization (zero-fill tracking).
    pub high_water: usize,

    // ─── Data Type Classification ────────────────────────────────────────

    /// Detected data type classification for the current block.
    ///
    /// Set by [`detect_data_type`](super::trees::detect_data_type) in
    /// `tr_flush_block` when the stream's `data_type` is `Z_UNKNOWN`.
    /// Propagated to `stream.data_type` by the block flushing logic so
    /// that the public API correctly reports `Z_BINARY`, `Z_TEXT`, or
    /// `Z_UNKNOWN` per the C zlib contract.
    ///
    /// Initialized to `Z_UNKNOWN` and updated once per compression session
    /// when the first block is flushed.
    pub data_type: i32,
}

// ─── DeflateState Implementation ──────────────────────────────────────────────

impl DeflateState {
    /// Creates a new `DeflateState` with all buffers allocated and fields
    /// initialized.
    ///
    /// This corresponds to the allocation and initialization logic in
    /// `deflateInit2_()` (`deflate.c` lines 440–532) combined with the
    /// initial field setup from `lm_init()` (lines 682–701).
    ///
    /// # Parameters
    ///
    /// - `w_bits` — Window size in bits (8–15). The physical window is
    ///   `2 × (1 << w_bits)` bytes.
    /// - `mem_level` — Memory level (1–9). Controls hash table and buffer
    ///   sizes. Higher values use more memory for better compression.
    /// - `method` — Compression method (always `Z_DEFLATED = 8`).
    /// - `strategy` — Compression strategy (`Z_DEFAULT_STRATEGY`, etc.).
    /// - `level` — Compression level (0–9). Must already be resolved from
    ///   `Z_DEFAULT_COMPRESSION` to `6` by the caller.
    /// - `wrap` — Wrapper format: 0 = raw, 1 = zlib, 2 = gzip.
    ///
    /// # Panics
    ///
    /// This function does not panic. Invalid `level` values outside 0–9
    /// will fall back to level 0 configuration.
    #[must_use]
    pub fn new(
        w_bits: usize,
        mem_level: usize,
        method: u8,
        strategy: i32,
        level: i32,
        wrap: i32,
    ) -> Self {
        // ── Window sizing ────────────────────────────────────────────────
        let w_size: usize = 1_usize << w_bits;
        let w_mask: usize = w_size - 1;

        // ── Hash table sizing ────────────────────────────────────────────
        let hash_bits: usize = mem_level + 7;
        let hash_size: usize = 1_usize << hash_bits;
        #[allow(clippy::cast_possible_truncation)]
        let hash_mask: u32 = (hash_size as u32).wrapping_sub(1);
        // hash_shift ensures oldest byte drops out after MIN_MATCH steps:
        //   hash_shift × MIN_MATCH >= hash_bits
        #[allow(clippy::cast_possible_truncation)]
        let hash_shift: u32 = hash_bits.div_ceil(MIN_MATCH) as u32;

        // ── Buffer sizing ────────────────────────────────────────────────
        let lit_bufsize: usize = 1_usize << (mem_level + 6);
        let pending_buf_size: usize = lit_bufsize * 4;
        let sym_end: usize = (lit_bufsize - 1) * 3;

        // ── Load compression config ──────────────────────────────────────
        // get_config returns None for levels outside 0..=9; fall back to
        // the level-0 (stored) configuration in that case.
        let config: &CompressionConfig = get_config(level)
            .unwrap_or(&CONFIGURATION_TABLE[0]);

        // ── Initial status ───────────────────────────────────────────────
        let status = if wrap == 2 {
            DeflateStatus::GzipHeader
        } else {
            DeflateStatus::Init
        };

        Self {
            status,

            // Pending output buffer
            pending_buf: vec![0u8; pending_buf_size],
            pending: 0,
            pending_buf_size,

            // Header / trailer
            wrap,
            gzip_header: None,
            gzip_index: 0,

            // Method
            method,

            // Flush handling — -2 is a sentinel that never matches valid
            // flush modes, ensuring the first call always detects a change.
            last_flush: -2,

            // Window — window_size is 0 until lm_init/reset sets it to
            // 2 * w_size, matching the C deflateInit2_ flow.
            window: vec![0u8; 2 * w_size],
            window_size: 0,
            w_size,
            w_bits,
            w_mask,

            // Hash chains
            prev: vec![0u16; w_size],
            head: vec![0u16; hash_size],
            ins_h: 0,
            hash_size,
            hash_bits,
            hash_mask,
            hash_shift,

            // Match state — match_length and prev_length start at
            // MIN_MATCH - 1 so the first match must beat this minimum.
            lookahead: 0,
            strstart: 0,
            match_start: 0,
            match_length: MIN_MATCH - 1,
            prev_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            insert: 0,

            // Block state
            block_start: 0,
            slid: false,

            // Compression configuration from CONFIGURATION_TABLE
            max_chain_length: config.max_chain as usize,
            good_match: config.good_length as usize,
            nice_match: config.nice_length as usize,
            max_lazy_match: config.max_lazy as usize,

            level,
            strategy,

            // Huffman trees — sizes match C exactly:
            //   dyn_ltree[HEAP_SIZE=573], dyn_dtree[61], bl_tree[39]
            dyn_ltree: vec![CtData::default(); HEAP_SIZE],
            dyn_dtree: vec![CtData::default(); DYN_DTREE_SIZE],
            bl_tree: vec![CtData::default(); BL_TREE_SIZE],

            l_desc: TreeDescState::default(),
            d_desc: TreeDescState::default(),
            bl_desc: TreeDescState::default(),

            bl_count: [0u16; MAX_BITS + 1],

            heap: [0i32; HEAP_SIZE],
            heap_len: 0,
            heap_max: 0,
            depth: [0u8; HEAP_SIZE],

            // Symbol buffer (separate from pending_buf in Rust)
            sym_buf: vec![0u8; lit_bufsize * 3],
            sym_next: 0,
            sym_end,
            lit_bufsize,
            matches: 0,

            // Bit buffer
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,

            // Bookkeeping
            opt_len: 0,
            static_len: 0,
            high_water: 0,

            // Data type classification — starts unknown, set on first block flush
            data_type: Z_UNKNOWN,
        }
    }

    /// Resets the deflate state for a new compression session without
    /// reallocating buffers.
    ///
    /// This combines the logic of `deflateResetKeep()` (`deflate.c`
    /// lines 644–677) and `lm_init()` (lines 682–701). Existing buffer
    /// allocations are reused, but all operational state is reset to
    /// initial values.
    ///
    /// After calling `reset()`, the state is ready for a new compression
    /// session with the same window size, memory level, and buffer capacities
    /// as the original initialization.
    pub fn reset(&mut self) {
        // ── Restore wrap sign (may have been negated by Z_FINISH) ────────
        if self.wrap < 0 {
            self.wrap = -self.wrap;
        }

        // ── Reset status based on wrap mode ──────────────────────────────
        self.status = if self.wrap == 2 {
            DeflateStatus::GzipHeader
        } else {
            DeflateStatus::Init
        };

        // ── Reset pending output ─────────────────────────────────────────
        self.pending = 0;

        // ── Reset flush tracking (sentinel value) ────────────────────────
        self.last_flush = -2;

        // ── Reset gzip header index ──────────────────────────────────────
        self.gzip_index = 0;

        // ── Reset window state (lm_init equivalent) ──────────────────────
        self.window_size = 2 * self.w_size;

        // Clear hash table — zero all head entries and reset slid flag.
        // This replaces the CLEAR_HASH macro from deflate.c lines 170–175.
        for entry in &mut self.head {
            *entry = 0;
        }
        self.slid = false;

        // ── Reload compression config from current level ─────────────────
        if let Some(config) = get_config(self.level) {
            self.max_lazy_match = config.max_lazy as usize;
            self.good_match = config.good_length as usize;
            self.nice_match = config.nice_length as usize;
            self.max_chain_length = config.max_chain as usize;
        }

        // ── Reset match state ────────────────────────────────────────────
        self.strstart = 0;
        self.block_start = 0;
        self.lookahead = 0;
        self.insert = 0;
        self.match_length = MIN_MATCH - 1;
        self.prev_length = MIN_MATCH - 1;
        self.prev_match = 0;
        self.match_available = false;
        self.match_start = 0;
        self.ins_h = 0;

        // ── Reset symbol buffer ──────────────────────────────────────────
        self.sym_next = 0;
        self.matches = 0;

        // ── Reset bit buffer ─────────────────────────────────────────────
        self.bi_buf = 0;
        self.bi_valid = 0;
        self.bi_used = 0;

        // ── Reset tree descriptors ───────────────────────────────────────
        self.l_desc = TreeDescState::default();
        self.d_desc = TreeDescState::default();
        self.bl_desc = TreeDescState::default();

        // ── Reset bookkeeping ────────────────────────────────────────────
        self.opt_len = 0;
        self.static_len = 0;
        self.high_water = 0;

        // ── Reset data type classification ───────────────────────────────
        self.data_type = Z_UNKNOWN;
    }

    /// Returns the maximum match length for hash insertion.
    ///
    /// In the C implementation, `max_insert_length` is a preprocessor alias
    /// for `max_lazy_match` (`deflate.h` line 186):
    ///
    /// ```c
    /// #define max_insert_length max_lazy_match
    /// ```
    ///
    /// For compression levels 1–3 (`deflate_fast`), this value controls the
    /// maximum match length that still triggers hash insertion for the
    /// matched bytes. For levels 4–9 (`deflate_slow`), the `max_lazy_match`
    /// semantic applies instead, but the numerical value is the same.
    #[inline]
    #[must_use]
    pub fn max_insert_length(&self) -> usize {
        self.max_lazy_match
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── DeflateStatus tests ─────────────────────────────────────────────

    #[test]
    fn status_c_int_round_trip() {
        let statuses = [
            (DeflateStatus::Init, 42),
            (DeflateStatus::GzipHeader, 57),
            (DeflateStatus::Extra, 69),
            (DeflateStatus::Name, 73),
            (DeflateStatus::Comment, 91),
            (DeflateStatus::Hcrc, 103),
            (DeflateStatus::Busy, 113),
            (DeflateStatus::Finish, 666),
        ];
        for (status, c_val) in &statuses {
            assert_eq!(status.as_c_int(), *c_val);
            assert_eq!(DeflateStatus::from_c_int(*c_val), Some(*status));
        }
    }

    #[test]
    fn status_from_c_int_invalid() {
        assert_eq!(DeflateStatus::from_c_int(0), None);
        assert_eq!(DeflateStatus::from_c_int(-1), None);
        assert_eq!(DeflateStatus::from_c_int(999), None);
    }

    #[test]
    fn status_all_eight_variants() {
        // Ensure exactly 8 variants exist by exhaustive matching
        let all = [
            DeflateStatus::Init,
            DeflateStatus::GzipHeader,
            DeflateStatus::Extra,
            DeflateStatus::Name,
            DeflateStatus::Comment,
            DeflateStatus::Hcrc,
            DeflateStatus::Busy,
            DeflateStatus::Finish,
        ];
        assert_eq!(all.len(), 8);
        for status in &all {
            let c = status.as_c_int();
            assert_eq!(DeflateStatus::from_c_int(c), Some(*status));
        }
    }

    // ─── Pos / IPos tests ────────────────────────────────────────────────

    #[test]
    fn pos_default_is_zero() {
        assert_eq!(Pos::default().0, 0);
    }

    #[test]
    fn ipos_default_is_zero() {
        assert_eq!(IPos::default().0, 0);
    }

    #[test]
    fn pos_ordering() {
        assert!(Pos(10) > Pos(5));
        assert!(Pos(0) < Pos(1));
        assert_eq!(Pos(7), Pos(7));
    }

    #[test]
    fn ipos_ordering() {
        assert!(IPos(100) > IPos(50));
        assert_eq!(IPos(42), IPos(42));
    }

    // ─── CtData tests ────────────────────────────────────────────────────

    #[test]
    fn ct_data_default_is_zeroed() {
        let ct = CtData::default();
        assert_eq!(ct.freq, 0);
        assert_eq!(ct.code, 0);
        assert_eq!(ct.dad, 0);
        assert_eq!(ct.len, 0);
    }

    #[test]
    fn ct_data_fields_independent() {
        let mut ct = CtData::default();
        ct.freq = 42;
        ct.code = 99;
        ct.dad = 7;
        ct.len = 15;
        assert_eq!(ct.freq, 42);
        assert_eq!(ct.code, 99);
        assert_eq!(ct.dad, 7);
        assert_eq!(ct.len, 15);
    }

    #[test]
    fn ct_data_copy_clone() {
        let ct = CtData { freq: 1, code: 2, dad: 3, len: 4 };
        let ct2 = ct;
        assert_eq!(ct, ct2);
    }

    // ─── TreeDescState tests ─────────────────────────────────────────────

    #[test]
    fn tree_desc_default_max_code_negative_one() {
        let desc = TreeDescState::default();
        assert_eq!(desc.max_code, -1);
    }

    #[test]
    fn tree_desc_copy() {
        let desc = TreeDescState { max_code: 42 };
        let desc2 = desc;
        assert_eq!(desc, desc2);
    }

    // ─── DeflateState::new() tests ───────────────────────────────────────

    #[test]
    fn new_default_settings() {
        // Default: windowBits=15, memLevel=8, method=8, strategy=0, level=6
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);

        // Window sizing
        assert_eq!(state.w_bits, 15);
        assert_eq!(state.w_size, 32768);
        assert_eq!(state.w_mask, 32767);
        assert_eq!(state.window.len(), 65536);

        // Hash sizing
        assert_eq!(state.hash_bits, 15);
        assert_eq!(state.hash_size, 32768);
        assert_eq!(state.hash_mask, 32767);
        // hash_shift = (15 + 3 - 1) / 3 = 17 / 3 = 5
        assert_eq!(state.hash_shift, 5);

        // Buffer sizing
        assert_eq!(state.lit_bufsize, 16384);
        assert_eq!(state.pending_buf_size, 65536);
        assert_eq!(state.sym_end, 49149); // (16384 - 1) * 3
        assert_eq!(state.sym_buf.len(), 49152); // 16384 * 3
        assert_eq!(state.pending_buf.len(), 65536);

        // Status
        assert_eq!(state.status, DeflateStatus::Init);
        assert_eq!(state.wrap, 1);
        assert_eq!(state.last_flush, -2);
        assert_eq!(state.method, 8);

        // Match state
        assert_eq!(state.match_length, MIN_MATCH - 1);
        assert_eq!(state.prev_length, MIN_MATCH - 1);
        assert_eq!(state.prev_match, 0);
        assert!(!state.match_available);
        assert_eq!(state.lookahead, 0);
        assert_eq!(state.strstart, 0);
        assert_eq!(state.insert, 0);

        // Level/strategy
        assert_eq!(state.level, 6);
        assert_eq!(state.strategy, 0);

        // Config for level 6: good=8, lazy=16, nice=128, chain=128
        assert_eq!(state.good_match, 8);
        assert_eq!(state.max_lazy_match, 16);
        assert_eq!(state.nice_match, 128);
        assert_eq!(state.max_chain_length, 128);

        // Tree arrays
        assert_eq!(state.dyn_ltree.len(), HEAP_SIZE); // 573
        assert_eq!(state.dyn_dtree.len(), DYN_DTREE_SIZE); // 61
        assert_eq!(state.bl_tree.len(), BL_TREE_SIZE); // 39

        // Tree descriptors
        assert_eq!(state.l_desc.max_code, -1);
        assert_eq!(state.d_desc.max_code, -1);
        assert_eq!(state.bl_desc.max_code, -1);

        // Bit buffer
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_used, 0);

        // Bookkeeping
        assert_eq!(state.opt_len, 0);
        assert_eq!(state.static_len, 0);
        assert_eq!(state.high_water, 0);
        assert_eq!(state.window_size, 0); // set by lm_init later
        assert!(!state.slid);
    }

    #[test]
    fn new_gzip_wrap() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 2);
        assert_eq!(state.status, DeflateStatus::GzipHeader);
        assert_eq!(state.wrap, 2);
    }

    #[test]
    fn new_raw_wrap() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 0);
        assert_eq!(state.status, DeflateStatus::Init);
        assert_eq!(state.wrap, 0);
    }

    #[test]
    fn new_level_zero() {
        let state = DeflateState::new(15, 8, 8, 0, 0, 1);
        assert_eq!(state.level, 0);
        assert_eq!(state.good_match, 0);
        assert_eq!(state.max_lazy_match, 0);
        assert_eq!(state.nice_match, 0);
        assert_eq!(state.max_chain_length, 0);
    }

    #[test]
    fn new_level_nine() {
        let state = DeflateState::new(15, 8, 8, 0, 9, 1);
        assert_eq!(state.level, 9);
        assert_eq!(state.good_match, 32);
        assert_eq!(state.max_lazy_match, 258);
        assert_eq!(state.nice_match, 258);
        assert_eq!(state.max_chain_length, 4096);
    }

    #[test]
    fn new_all_levels_load_correct_config() {
        for lvl in 0..=9 {
            let state = DeflateState::new(15, 8, 8, 0, lvl, 1);
            let config = &CONFIGURATION_TABLE[lvl as usize];
            assert_eq!(state.good_match, config.good_length as usize);
            assert_eq!(state.max_lazy_match, config.max_lazy as usize);
            assert_eq!(state.nice_match, config.nice_length as usize);
            assert_eq!(state.max_chain_length, config.max_chain as usize);
        }
    }

    #[test]
    fn new_small_window() {
        // windowBits=9, memLevel=1
        let state = DeflateState::new(9, 1, 8, 0, 6, 1);
        assert_eq!(state.w_size, 512);
        assert_eq!(state.window.len(), 1024);
        assert_eq!(state.prev.len(), 512);
        assert_eq!(state.hash_bits, 8);
        assert_eq!(state.hash_size, 256);
        assert_eq!(state.head.len(), 256);
        assert_eq!(state.lit_bufsize, 128); // 1 << (1 + 6) = 128
        assert_eq!(state.pending_buf_size, 512); // 128 * 4
        assert_eq!(state.sym_buf.len(), 384); // 128 * 3
        assert_eq!(state.sym_end, 381); // (128 - 1) * 3
    }

    #[test]
    fn new_invalid_level_falls_back() {
        // Level -1 should fall back to level 0 config
        let state = DeflateState::new(15, 8, 8, 0, -1, 1);
        assert_eq!(state.good_match, 0);
        assert_eq!(state.max_chain_length, 0);
    }

    // ─── max_insert_length tests ─────────────────────────────────────────

    #[test]
    fn max_insert_length_equals_max_lazy_match() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);
        assert_eq!(state.max_insert_length(), state.max_lazy_match);
        assert_eq!(state.max_insert_length(), 16);
    }

    #[test]
    fn max_insert_length_all_levels() {
        for lvl in 0..=9 {
            let state = DeflateState::new(15, 8, 8, 0, lvl, 1);
            assert_eq!(state.max_insert_length(), state.max_lazy_match);
        }
    }

    // ─── DeflateState::reset() tests ─────────────────────────────────────

    #[test]
    fn reset_restores_initial_state() {
        let mut state = DeflateState::new(15, 8, 8, 0, 6, 1);

        // Simulate some activity
        state.status = DeflateStatus::Busy;
        state.pending = 100;
        state.strstart = 500;
        state.lookahead = 200;
        state.match_available = true;
        state.match_length = 50;
        state.prev_length = 40;
        state.prev_match = 300;
        state.match_start = 400;
        state.sym_next = 300;
        state.matches = 42;
        state.bi_buf = 0xFF;
        state.bi_valid = 8;
        state.bi_used = 4;
        state.opt_len = 1000;
        state.static_len = 2000;
        state.high_water = 5000;
        state.block_start = 1000;
        state.ins_h = 12345;
        state.head[0] = 99;
        state.head[1] = 88;
        state.l_desc.max_code = 200;
        state.gzip_index = 50;

        state.reset();

        assert_eq!(state.status, DeflateStatus::Init);
        assert_eq!(state.pending, 0);
        assert_eq!(state.last_flush, -2);
        assert_eq!(state.gzip_index, 0);
        assert_eq!(state.window_size, 2 * state.w_size);
        assert_eq!(state.strstart, 0);
        assert_eq!(state.block_start, 0);
        assert_eq!(state.lookahead, 0);
        assert_eq!(state.insert, 0);
        assert_eq!(state.match_length, MIN_MATCH - 1);
        assert_eq!(state.prev_length, MIN_MATCH - 1);
        assert_eq!(state.prev_match, 0);
        assert_eq!(state.match_start, 0);
        assert!(!state.match_available);
        assert_eq!(state.ins_h, 0);
        assert_eq!(state.sym_next, 0);
        assert_eq!(state.matches, 0);
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_used, 0);
        assert_eq!(state.opt_len, 0);
        assert_eq!(state.static_len, 0);
        assert_eq!(state.high_water, 0);
        assert!(!state.slid);

        // Hash should be cleared
        assert_eq!(state.head[0], 0);
        assert_eq!(state.head[1], 0);

        // Tree descriptors reset
        assert_eq!(state.l_desc.max_code, -1);
        assert_eq!(state.d_desc.max_code, -1);
        assert_eq!(state.bl_desc.max_code, -1);

        // Buffers should still be allocated (no reallocation)
        assert_eq!(state.window.len(), 65536);
        assert_eq!(state.prev.len(), 32768);
        assert_eq!(state.head.len(), 32768);
        assert_eq!(state.pending_buf.len(), 65536);
        assert_eq!(state.sym_buf.len(), 49152);

        // Config should be reloaded from level 6
        assert_eq!(state.good_match, 8);
        assert_eq!(state.max_lazy_match, 16);
        assert_eq!(state.nice_match, 128);
        assert_eq!(state.max_chain_length, 128);
    }

    #[test]
    fn reset_gzip_wrap_restores_gzip_status() {
        let mut state = DeflateState::new(15, 8, 8, 0, 6, 2);
        state.status = DeflateStatus::Finish;
        state.reset();
        assert_eq!(state.status, DeflateStatus::GzipHeader);
    }

    #[test]
    fn reset_negative_wrap_restores_positive() {
        let mut state = DeflateState::new(15, 8, 8, 0, 6, 1);
        state.wrap = -1; // Simulates Z_FINISH negation
        state.reset();
        assert_eq!(state.wrap, 1);
        assert_eq!(state.status, DeflateStatus::Init);
    }

    #[test]
    fn reset_negative_wrap_gzip() {
        let mut state = DeflateState::new(15, 8, 8, 0, 6, 2);
        state.wrap = -2;
        state.reset();
        assert_eq!(state.wrap, 2);
        assert_eq!(state.status, DeflateStatus::GzipHeader);
    }

    // ─── Memory footprint verification ───────────────────────────────────

    #[test]
    fn memory_footprint_default_settings() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);

        let window_bytes = state.window.len();
        let prev_bytes = state.prev.len() * 2;
        let head_bytes = state.head.len() * 2;
        let pending_bytes = state.pending_buf.len();
        let sym_bytes = state.sym_buf.len();

        assert_eq!(window_bytes, 65536);
        assert_eq!(prev_bytes, 65536);
        assert_eq!(head_bytes, 65536);
        assert_eq!(pending_bytes, 65536);
        assert_eq!(sym_bytes, 49152);

        // Total heap allocation (major buffers only)
        let total = window_bytes + prev_bytes + head_bytes + pending_bytes + sym_bytes;
        // Should be approximately 311 KiB with separate sym_buf
        assert!(total < 350_000, "Total {total} exceeds 350 KB");
        assert!(total > 250_000, "Total {total} below 250 KB");
    }

    // ─── Hash computation verification ───────────────────────────────────

    #[test]
    fn hash_shift_values() {
        // memLevel=1: hash_bits=8, shift=(8+2)/3=3
        let state = DeflateState::new(15, 1, 8, 0, 6, 1);
        assert_eq!(state.hash_shift, 3);

        // memLevel=8: hash_bits=15, shift=(15+2)/3=5
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);
        assert_eq!(state.hash_shift, 5);

        // memLevel=9: hash_bits=16, shift=(16+2)/3=6
        let state = DeflateState::new(15, 9, 8, 0, 6, 1);
        assert_eq!(state.hash_shift, 6);
    }

    // ─── Clone tests ─────────────────────────────────────────────────────

    #[test]
    fn deflate_state_clone() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);
        let cloned = state.clone();
        assert_eq!(cloned.status, state.status);
        assert_eq!(cloned.w_size, state.w_size);
        assert_eq!(cloned.level, state.level);
        assert_eq!(cloned.window.len(), state.window.len());
    }
}
