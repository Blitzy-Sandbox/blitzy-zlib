// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Core data types for the DEFLATE compression engine.
// Translates C `internal_state` from deflate.h, `ct_data` union,
// `tree_desc` struct, `block_state` enum, and state constants.

//! Deflate state, Huffman node, tree descriptors, and deflate-specific constants.
//!
//! This module is the foundational type layer for the DEFLATE compression
//! engine. Nearly every other file in `src/deflate/` depends on it.
//!
//! # C-to-Rust Mapping
//!
//! | C Type | Rust Equivalent |
//! |--------|-----------------|
//! | `internal_state` (deflate.h:104-288) | [`DeflateState`] |
//! | `ct_data` union (deflate.h:72-81) | [`HuffmanNode`] |
//! | `tree_desc` (deflate.h:90-94) | [`TreeDesc`] |
//! | `static_tree_desc` (deflate.h:88) | [`StaticTreeDesc`] |
//! | `block_state` enum (deflate.c:63-68) | [`BlockState`] |
//! | State constants (INIT_STATE..FINISH_STATE) | [`DeflateStatus`] |

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(not(feature = "std"))]
use alloc::{vec, vec::Vec};

use crate::constants::{MAX_MATCH, MIN_MATCH, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_UNKNOWN};
use crate::gz_header::GzHeader;

// ===========================================================================
// Deflate-specific constants (from deflate.h lines 34–56)
// ===========================================================================

/// Number of length codes, not counting the special `END_BLOCK` code.
/// DEFLATE defines 29 length codes (codes 257–285) that encode match
/// lengths from 3 to 258.
pub const LENGTH_CODES: usize = 29;

/// Number of literal byte values (0–255).
pub const LITERALS: usize = 256;

/// Total number of literal or length codes in the DEFLATE alphabet,
/// including the `END_BLOCK` code (256).
/// Equal to `LITERALS + 1 + LENGTH_CODES` = 286.
pub const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes in the DEFLATE alphabet.
/// DEFLATE defines 30 distance codes (0–29).
pub const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths of the dynamic
/// Huffman code trees.
pub const BL_CODES: usize = 19;

/// Maximum heap size for Huffman tree construction.
/// Equal to `2 * L_CODES + 1` = 573.
pub const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// Maximum number of bits in a Huffman code. Per RFC 1951, no code
/// may exceed 15 bits in length.
pub const MAX_BITS: usize = 15;

/// Size of the bit buffer (`bi_buf`) in bits.
pub const BUF_SIZE: usize = 16;

/// Multiplier for `pending_buf` allocation (non-`LIT_MEM` mode).
/// `pending_buf_size = lit_bufsize * LIT_BUFS`.
pub const LIT_BUFS: usize = 4;

/// Minimum amount of lookahead, except at the end of the input file.
/// Equal to `MAX_MATCH + MIN_MATCH + 1` = 262.
pub const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// End-of-block literal code (code 256 in the DEFLATE alphabet).
pub const END_BLOCK: usize = 256;

// ---------------------------------------------------------------------------
// Additional size constants for tree arrays
// ---------------------------------------------------------------------------

/// Size of the `dyn_ltree` array (literal/length tree). Equal to
/// [`HEAP_SIZE`] = 573.
pub const DYN_LTREE_SIZE: usize = HEAP_SIZE;

/// Size of the `dyn_dtree` array (distance tree). Equal to
/// `2 * D_CODES + 1` = 61.
pub const DYN_DTREE_SIZE: usize = 2 * D_CODES + 1;

/// Size of the `bl_tree` array (bit-length tree). Equal to
/// `2 * BL_CODES + 1` = 39.
pub const BL_TREE_SIZE: usize = 2 * BL_CODES + 1;

// ===========================================================================
// Enums
// ===========================================================================

/// Result of a single compression step returned by strategy functions.
///
/// Maps to C `block_state` enum (deflate.c lines 63-68).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// Block not completed, need more input or more output.
    NeedMore,
    /// Block flush performed.
    BlockDone,
    /// Finish started, need only more output at next deflate call.
    FinishStarted,
    /// Finish done, accept no more input or output.
    FinishDone,
}

/// Deflate engine state-machine phase.
///
/// Replaces the integer state constants from deflate.h lines 58-67
/// (`INIT_STATE`=42 through `FINISH_STATE`=666) with a type-safe enum
/// that the compiler enforces via exhaustive `match`.
///
/// All variants are always present regardless of feature flags. The
/// gzip-related state transitions are gated by `#[cfg(feature = "gzip")]`
/// on the *code paths* that enter gzip states, not on the variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeflateStatus {
    /// Initial state, ready to write zlib header (was `INIT_STATE` = 42).
    Init,
    /// Ready to write gzip header (was `GZIP_STATE` = 57).
    Gzip,
    /// Writing gzip extra field (was `EXTRA_STATE` = 69).
    Extra,
    /// Writing gzip file name (was `NAME_STATE` = 73).
    Name,
    /// Writing gzip comment (was `COMMENT_STATE` = 91).
    Comment,
    /// Writing gzip header CRC (was `HCRC_STATE` = 103).
    HCrc,
    /// Compression in progress (was `BUSY_STATE` = 113).
    Busy,
    /// Stream complete, only trailer remains (was `FINISH_STATE` = 666).
    Finish,
}

/// Identifies which dynamic tree a [`TreeDesc`] references.
///
/// Used instead of a raw pointer into the `DeflateState` tree arrays,
/// since the actual tree data lives in `DeflateState.dyn_ltree`,
/// `DeflateState.dyn_dtree`, or `DeflateState.bl_tree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeType {
    /// Literal/length tree (`dyn_ltree`).
    Literal,
    /// Distance tree (`dyn_dtree`).
    Distance,
    /// Bit-length tree (`bl_tree`).
    BitLength,
}

// ===========================================================================
// HuffmanNode — replaces C `ct_data` union
// ===========================================================================

/// Huffman tree node, replacing C `ct_data` union (deflate.h:72-81).
///
/// During tree construction, `fc` stores the frequency count and `dl`
/// stores the parent node index. After code generation, `fc` stores the
/// Huffman code and `dl` stores the code bit length.
///
/// The dual-meaning is maintained for compatibility with the Huffman
/// algorithm. Accessor methods provide semantic clarity — the C code
/// uses macros `Freq`, `Code`, `Dad`, `Len` to access the same
/// underlying union fields; our accessors serve the same purpose.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct HuffmanNode {
    /// Frequency count (during tree build) OR Huffman code (after
    /// generation). C: `fc.freq` / `fc.code`.
    pub fc: u16,
    /// Parent node in Huffman tree (during build) OR bit string length
    /// (after generation). C: `dl.dad` / `dl.len`.
    pub dl: u16,
}

impl HuffmanNode {
    /// Returns the frequency count (tree-building phase).
    #[inline(always)]
    pub fn freq(&self) -> u16 {
        self.fc
    }

    /// Sets the frequency count (tree-building phase).
    #[inline(always)]
    pub fn set_freq(&mut self, val: u16) {
        self.fc = val;
    }

    /// Returns the Huffman code (code-generation phase).
    #[inline(always)]
    pub fn code(&self) -> u16 {
        self.fc
    }

    /// Sets the Huffman code (code-generation phase).
    #[inline(always)]
    pub fn set_code(&mut self, val: u16) {
        self.fc = val;
    }

    /// Returns the parent node index (tree-building phase).
    #[inline(always)]
    pub fn dad(&self) -> u16 {
        self.dl
    }

    /// Sets the parent node index (tree-building phase).
    #[inline(always)]
    pub fn set_dad(&mut self, val: u16) {
        self.dl = val;
    }

    /// Returns the bit string length (code-generation phase).
    ///
    /// Note: this is *not* a container-length method — it returns a Huffman
    /// code's bit-length, so `is_empty()` is semantically meaningless.
    #[inline(always)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        self.dl
    }

    /// Sets the bit string length (code-generation phase).
    #[inline(always)]
    pub fn set_len(&mut self, val: u16) {
        self.dl = val;
    }
}

// ===========================================================================
// StaticTreeDesc and TreeDesc
// ===========================================================================

/// Static tree descriptor holding references to pre-computed Huffman data.
///
/// The actual static instances (`STATIC_L_DESC`, `STATIC_D_DESC`,
/// `STATIC_BL_DESC`) are defined in `trees.rs`. This struct is the type
/// definition referenced by [`TreeDesc::stat_desc`].
///
/// Replaces C `static_tree_desc` (forward-declared in deflate.h:88,
/// defined in trees.c).
#[derive(Debug, Clone)]
pub struct StaticTreeDesc {
    /// Static tree data (`None` for the bit-length tree which has no
    /// pre-computed static representation).
    pub static_tree: Option<&'static [HuffmanNode]>,
    /// Extra bits for each code in this tree.
    pub extra_bits: &'static [u8],
    /// Base index for extra bits (e.g., `LITERALS + 1` for lengths).
    pub extra_base: usize,
    /// Number of elements (codes) in this tree.
    pub elems: usize,
    /// Maximum allowed bit length for codes in this tree.
    pub max_length: usize,
}

/// Descriptor linking a dynamic Huffman tree to its static counterpart.
///
/// Replaces C `tree_desc` (deflate.h:90-94). Instead of a raw pointer
/// to the dynamic tree array, we use a [`TreeType`] tag — the actual
/// tree data lives in the `dyn_ltree`, `dyn_dtree`, or `bl_tree` fields
/// of [`DeflateState`].
#[derive(Debug, Clone)]
pub struct TreeDesc {
    /// Which dynamic tree this descriptor references.
    pub tree_type: TreeType,
    /// Largest code with non-zero frequency.
    pub max_code: i32,
    /// The corresponding static tree descriptor.
    pub stat_desc: &'static StaticTreeDesc,
}

// ===========================================================================
// Placeholder static descriptor (used by new() before tr_init)
// ===========================================================================

/// Placeholder static tree descriptor used during `DeflateState::new()`
/// before `trees::tr_init()` assigns the real descriptors.
static PLACEHOLDER_STATIC_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &[],
    extra_base: 0,
    elems: 0,
    max_length: 0,
};

// ===========================================================================
// Lookup tables for tally_dist / d_code — imported from trees.rs
// (Single authoritative copy; eliminates duplication per review finding #5)
// ===========================================================================
use super::trees::{DIST_CODE, LENGTH_CODE};

// ===========================================================================
// DeflateState — the main compression state struct
// ===========================================================================

/// Internal compression state for the DEFLATE engine.
///
/// This struct owns all buffers and tracking data for a single compression
/// stream. It is the Rust equivalent of C `internal_state` (deflate.h:104-288),
/// with ~80 fields organized by functional group.
///
/// # Ownership Model
///
/// `DeflateState` is heap-allocated inside a `Box` and owned by the
/// [`ZStream`]. All internal buffers (`window`, `prev`, `head`,
/// `pending_buf`, `sym_buf`) are `Vec`-owned, so dropping `DeflateState`
/// automatically releases all memory.
///
/// # C-to-Rust Translation Notes
///
/// - C `Bytef *` + `ulg size` pairs → Rust `Vec<u8>` (owns memory, tracks length)
/// - C `sym_buf` overlaying `pending_buf` → separate `Vec<u8>` (no mutable aliasing)
/// - C integer flags `match_available`, `slid` → Rust `bool`
/// - C `gz_headerp` (nullable pointer) → `Option<GzHeader>`
/// - C fixed-size arrays → Rust arrays inside `Box<DeflateState>` on the heap
pub struct DeflateState {
    // === Stream Management ===
    /// Compression status/phase (was `status` integer in C).
    pub status: DeflateStatus,

    /// Output still pending — bytes waiting to be flushed to the caller.
    /// Size = `lit_bufsize * LIT_BUFS` (was `pending_buf: *mut u8`).
    pub pending_buf: Vec<u8>,

    /// Size of `pending_buf` in bytes (= `lit_bufsize * LIT_BUFS`).
    pub pending_buf_size: usize,

    /// Index of next pending byte to output
    /// (was `pending_out` pointer offset into `pending_buf`).
    pub pending_out: usize,

    /// Number of bytes in the pending buffer (was `pending: ulg`).
    pub pending: usize,

    /// Wrapping mode: 0 = raw DEFLATE, 1 = zlib, 2 = gzip.
    /// Becomes negative after `Z_FINISH` to prevent double trailer writing.
    pub wrap: i32,

    /// Gzip header information to write (was `gzhead: gz_headerp`).
    /// `None` when no gzip header is configured.
    pub gzhead: Option<GzHeader>,

    /// Index into gzhead extra/name/comment during header writing
    /// (was `gzindex: ulg`).
    pub gzindex: usize,

    /// Compression method, always `Z_DEFLATED` = 8 (was `method: Byte`).
    pub method: u8,

    /// Value of flush param for previous `deflate()` call
    /// (was `last_flush: int`). -2 means never called.
    pub last_flush: i32,

    // === Sliding Window (LZ77) ===
    /// LZ77 window size in bytes (32K by default) (was `w_size: uInt`).
    pub w_size: usize,

    /// log₂(`w_size`), range 8..16 (was `w_bits: uInt`).
    pub w_bits: usize,

    /// `w_size - 1`, used as a bit mask (was `w_mask: uInt`).
    pub w_mask: usize,

    /// Sliding window buffer, size = `2 * w_size` bytes.
    /// Input bytes are read into the second half, then moved to the first
    /// half to keep a dictionary of at least `w_size` bytes.
    pub window: Vec<u8>,

    /// Actual window size: `2 * w_size` except when the user input buffer
    /// is directly used (was `window_size: ulg`).
    pub window_size: usize,

    // === Hash Chains ===
    /// Link to older string with same hash index. An index in this array
    /// is a window index modulo `w_size`. Size = `w_size` entries.
    pub prev: Vec<u16>,

    /// Heads of the hash chains or NIL. Size = `hash_size` entries.
    pub head: Vec<u16>,

    /// Hash index of string to be inserted (was `ins_h: uInt`).
    pub ins_h: u32,

    /// Number of elements in hash table (was `hash_size: uInt`).
    pub hash_size: usize,

    /// log₂(`hash_size`) (was `hash_bits: uInt`).
    pub hash_bits: usize,

    /// `hash_size - 1` (was `hash_mask: uInt`).
    pub hash_mask: u32,

    /// Number of bits to shift `ins_h` at each input step.
    /// Satisfies: `hash_shift * MIN_MATCH >= hash_bits`.
    pub hash_shift: usize,

    // === Match Finding ===
    /// Window position at the beginning of the current output block.
    /// Gets negative when the window is moved backwards.
    pub block_start: i64,

    /// Length of best match (was `match_length: uInt`).
    pub match_length: usize,

    /// Previous match position (was `prev_match: IPos`).
    pub prev_match: u32,

    /// Set if previous match exists, for lazy matching
    /// (was `match_available: int`).
    pub match_available: bool,

    /// Start of string to insert / current window position
    /// (was `strstart: uInt`).
    pub strstart: usize,

    /// Start of matching string (was `match_start: uInt`).
    pub match_start: usize,

    /// Number of valid bytes ahead in window (was `lookahead: uInt`).
    pub lookahead: usize,

    /// Length of the best match at previous step. Matches not greater
    /// than this are discarded in lazy match evaluation.
    pub prev_length: usize,

    /// Maximum hash chain length, from `configuration_table`.
    pub max_chain_length: u32,

    /// Attempt to find a better match only when current match is strictly
    /// smaller than this value. In C, `max_insert_length` is `#define`d
    /// to `max_lazy_match`.
    pub max_lazy_match: u32,

    // === Compression Parameters ===
    /// Compression level 0-9 (was `level: int`).
    pub level: usize,

    /// Compression strategy (was `strategy: int`).
    pub strategy: i32,

    /// Use faster search when prev match is longer than this.
    pub good_match: u32,

    /// Stop searching when current match exceeds this.
    pub nice_match: i32,

    // === Huffman Trees (used by trees.rs) ===
    /// Literal and length tree. Size = [`DYN_LTREE_SIZE`] = 573 entries.
    pub dyn_ltree: [HuffmanNode; DYN_LTREE_SIZE],

    /// Distance tree. Size = [`DYN_DTREE_SIZE`] = 61 entries.
    pub dyn_dtree: [HuffmanNode; DYN_DTREE_SIZE],

    /// Huffman tree for bit lengths. Size = [`BL_TREE_SIZE`] = 39 entries.
    pub bl_tree: [HuffmanNode; BL_TREE_SIZE],

    /// Descriptor for literal tree.
    pub l_desc: TreeDesc,

    /// Descriptor for distance tree.
    pub d_desc: TreeDesc,

    /// Descriptor for bit length tree.
    pub bl_desc: TreeDesc,

    /// Number of codes at each bit length for an optimal tree.
    /// `MAX_BITS + 1` = 16 entries.
    pub bl_count: [u16; MAX_BITS + 1],

    // === Heap for Tree Building ===
    /// Heap used to build Huffman trees. `heap[0]` is not used.
    /// Sons of `heap[n]` are `heap[2*n]` and `heap[2*n+1]`.
    pub heap: [i32; HEAP_SIZE],

    /// Number of elements in the heap.
    pub heap_len: i32,

    /// Element of largest frequency.
    pub heap_max: i32,

    /// Depth of each subtree, used as tie breaker for equal frequency.
    pub depth: [u8; HEAP_SIZE],

    // === Symbol Buffer ===
    /// Buffer for distances and literals/lengths.
    /// Each symbol is 3 bytes: distance (2 bytes LE) + literal/length
    /// (1 byte). For a literal, distance = 0 and the third byte is the
    /// literal value.
    pub sym_buf: Vec<u8>,

    /// Size of match buffer for literals/lengths.
    pub lit_bufsize: usize,

    /// Running index in symbol buffer.
    pub sym_next: usize,

    /// Symbol table full when `sym_next` reaches this value.
    /// Equal to `(lit_bufsize - 1) * 3`.
    pub sym_end: usize,

    // === Block Statistics ===
    /// Bit length of current block with optimal trees.
    pub opt_len: u64,

    /// Bit length of current block with static trees.
    pub static_len: u64,

    /// Number of string matches in current block.
    pub matches: u32,

    /// Bytes at end of window left to insert into hash table.
    pub insert: usize,

    // === Bit Output Buffer ===
    /// Output bit buffer. Bits are inserted starting at the LSB.
    pub bi_buf: u16,

    /// Number of valid bits in `bi_buf` (0..16).
    pub bi_valid: i32,

    /// Last number of used bits when going to a byte boundary.
    pub bi_used: i32,

    // === Window Tracking ===
    /// High water mark offset in window for initialized bytes.
    pub high_water: usize,

    /// True if the hash table has been slid since it was cleared.
    pub slid: bool,
}

// ===========================================================================
// DeflateState implementation
// ===========================================================================

impl DeflateState {
    /// Creates a new `DeflateState` with all buffers allocated and fields
    /// initialized.
    ///
    /// This mirrors the allocation portion of C `deflateInit2_`
    /// (deflate.c:387-533). Tree descriptors are initialized with
    /// placeholder static descriptors; call `trees::tr_init()` after
    /// construction to assign the real static tree data.
    ///
    /// # Parameters
    ///
    /// - `w_bits`: log₂(window size), range 8..=15.
    /// - `mem_level`: memory level, range 1..=9.
    /// - `level`: compression level 0..=9.
    /// - `strategy`: compression strategy (e.g., `Z_DEFAULT_STRATEGY`).
    /// - `wrap`: wrapping mode (0 = raw, 1 = zlib, 2 = gzip).
    pub fn new(w_bits: usize, mem_level: usize, level: usize, strategy: i32, wrap: i32) -> Self {
        // Derived window parameters
        let w_size: usize = 1 << w_bits;
        let w_mask: usize = w_size - 1;

        // Derived hash parameters
        let hash_bits: usize = mem_level + 7;
        let hash_size: usize = 1 << hash_bits;
        let hash_mask: u32 = (hash_size - 1) as u32;
        let hash_shift: usize = hash_bits.div_ceil(MIN_MATCH);

        // Symbol / pending buffer sizing
        let lit_bufsize: usize = 1 << (mem_level + 6);
        let pending_buf_size: usize = lit_bufsize * LIT_BUFS;
        let sym_end: usize = (lit_bufsize - 1) * 3;

        // Initial status depends on wrapping mode
        let status = if wrap == 2 {
            DeflateStatus::Gzip
        } else {
            DeflateStatus::Init
        };

        DeflateState {
            // Stream management
            status,
            pending_buf: vec![0u8; pending_buf_size],
            pending_buf_size,
            pending_out: 0,
            pending: 0,
            wrap,
            gzhead: None,
            gzindex: 0,
            method: Z_DEFLATED as u8,
            last_flush: -2, // not called yet

            // Sliding window
            w_size,
            w_bits,
            w_mask,
            window: vec![0u8; w_size * 2],
            window_size: 0, // set by lm_init → 2 * w_size

            // Hash chains
            prev: vec![0u16; w_size],
            head: vec![0u16; hash_size],
            ins_h: 0,
            hash_size,
            hash_bits,
            hash_mask,
            hash_shift,

            // Match finding
            block_start: 0,
            match_length: 0,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: 0,
            max_chain_length: 0,
            max_lazy_match: 0,

            // Compression parameters
            level,
            strategy,
            good_match: 0,
            nice_match: 0,

            // Huffman trees (zeroed; tr_init fills them)
            dyn_ltree: [HuffmanNode::default(); DYN_LTREE_SIZE],
            dyn_dtree: [HuffmanNode::default(); DYN_DTREE_SIZE],
            bl_tree: [HuffmanNode::default(); BL_TREE_SIZE],

            // Tree descriptors (placeholders; tr_init assigns real ones)
            l_desc: TreeDesc {
                tree_type: TreeType::Literal,
                max_code: 0,
                stat_desc: &PLACEHOLDER_STATIC_DESC,
            },
            d_desc: TreeDesc {
                tree_type: TreeType::Distance,
                max_code: 0,
                stat_desc: &PLACEHOLDER_STATIC_DESC,
            },
            bl_desc: TreeDesc {
                tree_type: TreeType::BitLength,
                max_code: 0,
                stat_desc: &PLACEHOLDER_STATIC_DESC,
            },
            bl_count: [0u16; MAX_BITS + 1],

            // Heap
            heap: [0i32; HEAP_SIZE],
            heap_len: 0,
            heap_max: 0,
            depth: [0u8; HEAP_SIZE],

            // Symbol buffer (separate allocation for Rust safety)
            sym_buf: vec![0u8; lit_bufsize * 3],
            lit_bufsize,
            sym_next: 0,
            sym_end,

            // Block statistics
            opt_len: 0,
            static_len: 0,
            matches: 0,
            insert: 0,

            // Bit output buffer
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,

            // Window tracking
            high_water: 0,
            slid: false,
        }
    }

    /// Returns `max_lazy_match`, mirroring
    /// C `#define max_insert_length max_lazy_match`.
    ///
    /// For compression levels ≤ 3, this limits hash insertion length
    /// (strings longer than this are not inserted into the hash table).
    #[inline(always)]
    pub fn max_insert_length(&self) -> u32 {
        self.max_lazy_match
    }

    /// Records a literal byte in the symbol buffer and updates the dynamic
    /// literal tree frequency count.
    ///
    /// Returns `true` when the symbol buffer is full and a block flush is
    /// needed.
    ///
    /// Port of C `_tr_tally_lit` macro (deflate.h:357-364, non-`LIT_MEM`
    /// variant).
    #[inline]
    pub fn tally_lit(&mut self, c: u8) -> bool {
        // Distance = 0 means literal
        self.sym_buf[self.sym_next] = 0;
        self.sym_buf[self.sym_next + 1] = 0;
        self.sym_buf[self.sym_next + 2] = c;
        self.sym_next += 3;
        // Increment frequency of this literal byte
        let freq = self.dyn_ltree[c as usize].freq();
        self.dyn_ltree[c as usize].set_freq(freq.wrapping_add(1));
        // Return true when buffer is full
        self.sym_next == self.sym_end
    }

    /// Records a distance/length match pair in the symbol buffer and updates
    /// the dynamic literal and distance tree frequency counts.
    ///
    /// Returns `true` when the symbol buffer is full and a block flush is
    /// needed.
    ///
    /// - `distance`: match distance (1-based, as stored in the stream).
    /// - `length`: match length minus `MIN_MATCH` (0-based length code index).
    ///
    /// Port of C `_tr_tally_dist` macro (deflate.h:365-375, non-`LIT_MEM`
    /// variant).
    #[inline]
    pub fn tally_dist(&mut self, distance: u16, length: u8) -> bool {
        // Store distance (2 bytes LE) and length (1 byte)
        self.sym_buf[self.sym_next] = (distance & 0xff) as u8;
        self.sym_buf[self.sym_next + 1] = (distance >> 8) as u8;
        self.sym_buf[self.sym_next + 2] = length;
        self.sym_next += 3;

        // Update literal/length tree: LENGTH_CODE maps length to code index
        let lc = LENGTH_CODE[length as usize] as usize + LITERALS + 1;
        let freq_l = self.dyn_ltree[lc].freq();
        self.dyn_ltree[lc].set_freq(freq_l.wrapping_add(1));

        // Update distance tree: d_code maps (distance - 1) to distance code
        let dist = (distance - 1) as usize;
        let dc = Self::d_code(dist);
        let freq_d = self.dyn_dtree[dc].freq();
        self.dyn_dtree[dc].set_freq(freq_d.wrapping_add(1));

        // Return true when buffer is full
        self.sym_next == self.sym_end
    }

    /// Maps a distance value to a distance code.
    ///
    /// `dist` is the distance minus 1 (0-based). This function has no side
    /// effects.
    ///
    /// Port of C `d_code(dist)` macro (deflate.h:320-321).
    #[inline(always)]
    pub fn d_code(dist: usize) -> usize {
        if dist < 256 {
            DIST_CODE[dist] as usize
        } else {
            DIST_CODE[256 + (dist >> 7)] as usize
        }
    }
}

// ===========================================================================
// Manual Clone — needed by `deflate_copy`.
// ===========================================================================

impl Clone for DeflateState {
    fn clone(&self) -> Self {
        DeflateState {
            status: self.status,
            pending_buf: self.pending_buf.clone(),
            pending_buf_size: self.pending_buf_size,
            pending_out: self.pending_out,
            pending: self.pending,
            wrap: self.wrap,
            gzhead: self.gzhead.clone(),
            gzindex: self.gzindex,
            method: self.method,
            last_flush: self.last_flush,

            w_size: self.w_size,
            w_bits: self.w_bits,
            w_mask: self.w_mask,
            window: self.window.clone(),
            window_size: self.window_size,

            prev: self.prev.clone(),
            head: self.head.clone(),
            ins_h: self.ins_h,
            hash_size: self.hash_size,
            hash_bits: self.hash_bits,
            hash_mask: self.hash_mask,
            hash_shift: self.hash_shift,

            block_start: self.block_start,
            match_length: self.match_length,
            prev_match: self.prev_match,
            match_available: self.match_available,
            strstart: self.strstart,
            match_start: self.match_start,
            lookahead: self.lookahead,
            prev_length: self.prev_length,
            max_chain_length: self.max_chain_length,
            max_lazy_match: self.max_lazy_match,

            level: self.level,
            strategy: self.strategy,
            good_match: self.good_match,
            nice_match: self.nice_match,

            dyn_ltree: self.dyn_ltree,
            dyn_dtree: self.dyn_dtree,
            bl_tree: self.bl_tree,

            l_desc: self.l_desc.clone(),
            d_desc: self.d_desc.clone(),
            bl_desc: self.bl_desc.clone(),
            bl_count: self.bl_count,

            heap: self.heap,
            heap_len: self.heap_len,
            heap_max: self.heap_max,
            depth: self.depth,

            sym_buf: self.sym_buf.clone(),
            lit_bufsize: self.lit_bufsize,
            sym_next: self.sym_next,
            sym_end: self.sym_end,

            opt_len: self.opt_len,
            static_len: self.static_len,
            matches: self.matches,
            insert: self.insert,

            bi_buf: self.bi_buf,
            bi_valid: self.bi_valid,
            bi_used: self.bi_used,

            high_water: self.high_water,
            slid: self.slid,
        }
    }
}

// ===========================================================================
// Default impl — convenience constructor with standard settings.
// ===========================================================================

impl Default for DeflateState {
    /// Creates a `DeflateState` with the standard default configuration:
    /// - `w_bits = 15` (32K window, the standard default)
    /// - `mem_level = 8` (default memory level)
    /// - `level = 6` (default compression level)
    /// - `strategy = Z_DEFAULT_STRATEGY`
    /// - `wrap = 1` (zlib format)
    ///
    /// The initial `strategy` field is set to [`Z_DEFAULT_STRATEGY`] (0).
    fn default() -> Self {
        Self::new(15, 8, 6, Z_DEFAULT_STRATEGY, 1)
    }
}

/// Initial data-type classification value used by `_tr_init` / `set_data_type`.
///
/// This mirrors C `s->strm->data_type = Z_UNKNOWN` in `_tr_init` (trees.c).
/// Exported so other deflate submodules can set the stream's `data_type`.
pub const INITIAL_DATA_TYPE: i32 = Z_UNKNOWN;

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants_values() {
        assert_eq!(LENGTH_CODES, 29);
        assert_eq!(LITERALS, 256);
        assert_eq!(L_CODES, 286);
        assert_eq!(D_CODES, 30);
        assert_eq!(BL_CODES, 19);
        assert_eq!(HEAP_SIZE, 573);
        assert_eq!(MAX_BITS, 15);
        assert_eq!(BUF_SIZE, 16);
        assert_eq!(LIT_BUFS, 4);
        assert_eq!(MIN_LOOKAHEAD, 262); // 258 + 3 + 1
        assert_eq!(END_BLOCK, 256);
        assert_eq!(DYN_LTREE_SIZE, 573);
        assert_eq!(DYN_DTREE_SIZE, 61);
        assert_eq!(BL_TREE_SIZE, 39);
    }

    #[test]
    fn test_deflate_status_variants_exhaustive() {
        // Verify all 8 variants compile and are distinct
        let statuses = [
            DeflateStatus::Init,
            DeflateStatus::Gzip,
            DeflateStatus::Extra,
            DeflateStatus::Name,
            DeflateStatus::Comment,
            DeflateStatus::HCrc,
            DeflateStatus::Busy,
            DeflateStatus::Finish,
        ];
        assert_eq!(statuses.len(), 8);
        // Each pair must be distinct
        for i in 0..statuses.len() {
            for j in (i + 1)..statuses.len() {
                assert_ne!(statuses[i], statuses[j]);
            }
        }
    }

    #[test]
    fn test_block_state_variants() {
        assert_ne!(BlockState::NeedMore, BlockState::BlockDone);
        assert_ne!(BlockState::FinishStarted, BlockState::FinishDone);
        assert_ne!(BlockState::NeedMore, BlockState::FinishStarted);
        assert_ne!(BlockState::BlockDone, BlockState::FinishDone);
    }

    #[test]
    fn test_block_state_copy() {
        let a = BlockState::NeedMore;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn test_huffman_node_default() {
        let node = HuffmanNode::default();
        assert_eq!(node.fc, 0);
        assert_eq!(node.dl, 0);
        assert_eq!(node.freq(), 0);
        assert_eq!(node.code(), 0);
        assert_eq!(node.dad(), 0);
        assert_eq!(node.len(), 0);
    }

    #[test]
    fn test_huffman_node_accessors() {
        let mut node = HuffmanNode::default();

        // freq / code share the fc field
        node.set_freq(42);
        assert_eq!(node.freq(), 42);
        assert_eq!(node.code(), 42);

        node.set_code(99);
        assert_eq!(node.freq(), 99);
        assert_eq!(node.code(), 99);

        // dad / len share the dl field
        node.set_dad(7);
        assert_eq!(node.dad(), 7);
        assert_eq!(node.len(), 7);

        node.set_len(15);
        assert_eq!(node.dad(), 15);
        assert_eq!(node.len(), 15);
    }

    #[test]
    fn test_huffman_node_repr_c_size() {
        // ct_data in C is 4 bytes (two u16 fields)
        assert_eq!(core::mem::size_of::<HuffmanNode>(), 4);
        assert_eq!(core::mem::align_of::<HuffmanNode>(), 2);
    }

    #[test]
    fn test_tree_type_variants() {
        assert_ne!(TreeType::Literal, TreeType::Distance);
        assert_ne!(TreeType::Distance, TreeType::BitLength);
        assert_ne!(TreeType::Literal, TreeType::BitLength);
    }

    #[test]
    fn test_static_tree_desc_placeholder() {
        assert!(PLACEHOLDER_STATIC_DESC.static_tree.is_none());
        assert!(PLACEHOLDER_STATIC_DESC.extra_bits.is_empty());
        assert_eq!(PLACEHOLDER_STATIC_DESC.extra_base, 0);
        assert_eq!(PLACEHOLDER_STATIC_DESC.elems, 0);
        assert_eq!(PLACEHOLDER_STATIC_DESC.max_length, 0);
    }

    #[test]
    fn test_tree_desc_creation() {
        let desc = TreeDesc {
            tree_type: TreeType::Literal,
            max_code: 285,
            stat_desc: &PLACEHOLDER_STATIC_DESC,
        };
        assert_eq!(desc.tree_type, TreeType::Literal);
        assert_eq!(desc.max_code, 285);
    }

    #[test]
    fn test_deflate_state_new_default() {
        // Default settings: w_bits=15, mem_level=8, level=6, strategy=0, wrap=1
        let state = DeflateState::new(15, 8, 6, Z_DEFAULT_STRATEGY, 1);

        // Window parameters
        assert_eq!(state.w_size, 32768);
        assert_eq!(state.w_bits, 15);
        assert_eq!(state.w_mask, 32767);
        assert_eq!(state.window.len(), 65536);
        assert_eq!(state.window_size, 0); // set later by lm_init

        // Hash parameters
        assert_eq!(state.hash_bits, 15);
        assert_eq!(state.hash_size, 32768);
        assert_eq!(state.hash_mask, 32767);
        assert_eq!(state.hash_shift, 5); // (15 + 3 - 1) / 3
        assert_eq!(state.prev.len(), 32768);
        assert_eq!(state.head.len(), 32768);

        // Buffer sizing
        let lit_bufsize = 1 << (8 + 6); // 16384
        assert_eq!(state.lit_bufsize, lit_bufsize);
        assert_eq!(state.pending_buf_size, lit_bufsize * 4);
        assert_eq!(state.pending_buf.len(), lit_bufsize * 4);
        assert_eq!(state.sym_buf.len(), lit_bufsize * 3);
        assert_eq!(state.sym_end, (lit_bufsize - 1) * 3);

        // Stream state
        assert_eq!(state.status, DeflateStatus::Init);
        assert_eq!(state.wrap, 1);
        assert_eq!(state.method, Z_DEFLATED as u8);
        assert_eq!(state.last_flush, -2);
        assert_eq!(state.level, 6);
        assert_eq!(state.strategy, Z_DEFAULT_STRATEGY);
        assert!(!state.match_available);
        assert!(!state.slid);
        assert!(state.gzhead.is_none());

        // Tree descriptors
        assert_eq!(state.l_desc.tree_type, TreeType::Literal);
        assert_eq!(state.d_desc.tree_type, TreeType::Distance);
        assert_eq!(state.bl_desc.tree_type, TreeType::BitLength);
    }

    #[test]
    fn test_deflate_state_new_gzip() {
        let state = DeflateState::new(15, 8, 6, 0, 2);
        assert_eq!(state.status, DeflateStatus::Gzip);
        assert_eq!(state.wrap, 2);
    }

    #[test]
    fn test_deflate_state_new_raw() {
        let state = DeflateState::new(15, 8, 6, 0, 0);
        assert_eq!(state.status, DeflateStatus::Init);
        assert_eq!(state.wrap, 0);
    }

    #[test]
    fn test_deflate_state_new_small_window() {
        let state = DeflateState::new(9, 1, 1, 0, 1);
        assert_eq!(state.w_size, 512);
        assert_eq!(state.window.len(), 1024);
        assert_eq!(state.prev.len(), 512);
        assert_eq!(state.hash_bits, 8);
        assert_eq!(state.hash_size, 256);
        assert_eq!(state.head.len(), 256);
        let lit_bufsize = 1 << (1 + 6); // 128
        assert_eq!(state.lit_bufsize, lit_bufsize);
        assert_eq!(state.sym_buf.len(), lit_bufsize * 3);
    }

    #[test]
    fn test_max_insert_length() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);
        state.max_lazy_match = 42;
        assert_eq!(state.max_insert_length(), 42);
        state.max_lazy_match = 0;
        assert_eq!(state.max_insert_length(), 0);
    }

    #[test]
    fn test_d_code_small() {
        // Distances < 256 use the direct table
        assert_eq!(DeflateState::d_code(0), DIST_CODE[0] as usize);
        assert_eq!(DeflateState::d_code(1), DIST_CODE[1] as usize);
        assert_eq!(DeflateState::d_code(255), DIST_CODE[255] as usize);
    }

    #[test]
    fn test_d_code_large() {
        // Distances >= 256 use 256 + (dist >> 7)
        assert_eq!(
            DeflateState::d_code(256),
            DIST_CODE[256 + (256 >> 7)] as usize
        );
        assert_eq!(
            DeflateState::d_code(32767),
            DIST_CODE[256 + (32767 >> 7)] as usize
        );
    }

    #[test]
    fn test_tally_lit_basic() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);
        assert_eq!(state.sym_next, 0);
        assert_eq!(state.dyn_ltree[b'A' as usize].freq(), 0);

        let full = state.tally_lit(b'A');
        assert!(!full);
        assert_eq!(state.sym_next, 3);
        assert_eq!(state.sym_buf[0], 0); // dist low = 0
        assert_eq!(state.sym_buf[1], 0); // dist high = 0
        assert_eq!(state.sym_buf[2], b'A'); // literal
        assert_eq!(state.dyn_ltree[b'A' as usize].freq(), 1);
    }

    #[test]
    fn test_tally_lit_multiple() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);

        state.tally_lit(b'A');
        state.tally_lit(b'A');
        state.tally_lit(b'B');

        assert_eq!(state.sym_next, 9);
        assert_eq!(state.dyn_ltree[b'A' as usize].freq(), 2);
        assert_eq!(state.dyn_ltree[b'B' as usize].freq(), 1);
    }

    #[test]
    fn test_tally_dist_basic() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);

        // distance=5, length=0 (meaning match length = MIN_MATCH + 0 = 3)
        let full = state.tally_dist(5, 0);
        assert!(!full);
        assert_eq!(state.sym_next, 3);
        assert_eq!(state.sym_buf[0], 5); // dist low byte
        assert_eq!(state.sym_buf[1], 0); // dist high byte
        assert_eq!(state.sym_buf[2], 0); // length (0-based)

        // Verify length code frequency updated
        let lc = LENGTH_CODE[0] as usize + LITERALS + 1;
        assert_eq!(state.dyn_ltree[lc].freq(), 1);

        // Verify distance code frequency updated
        let dc = DeflateState::d_code(4); // dist - 1 = 4
        assert_eq!(state.dyn_dtree[dc].freq(), 1);
    }

    #[test]
    fn test_tally_dist_large_distance() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);

        // distance = 1000 (stored as LE bytes), length = 10
        let full = state.tally_dist(1000, 10);
        assert!(!full);
        assert_eq!(state.sym_buf[0], (1000 & 0xff) as u8); // 232
        assert_eq!(state.sym_buf[1], (1000 >> 8) as u8); // 3
        assert_eq!(state.sym_buf[2], 10);
    }

    #[test]
    fn test_tally_lit_fills_buffer() {
        // Use smallest possible buffers: mem_level=1
        let mut state = DeflateState::new(9, 1, 1, 0, 1);
        // lit_bufsize = 128, sym_end = (128-1)*3 = 381
        assert_eq!(state.sym_end, 381);

        let mut filled = false;
        for i in 0..127 {
            let full = state.tally_lit((i & 0xff) as u8);
            if full {
                filled = true;
                break;
            }
        }
        assert!(filled);
        assert_eq!(state.sym_next, state.sym_end);
    }

    #[test]
    fn test_clone_deep_copy() {
        let mut state = DeflateState::new(15, 8, 6, 0, 1);
        state.tally_lit(b'X');
        state.status = DeflateStatus::Busy;
        state.level = 9;

        let cloned = state.clone();

        // Verify deep copy
        assert_eq!(cloned.w_size, state.w_size);
        assert_eq!(cloned.pending_buf.len(), state.pending_buf.len());
        assert_eq!(cloned.window.len(), state.window.len());
        assert_eq!(cloned.sym_buf.len(), state.sym_buf.len());
        assert_eq!(cloned.level, 9);
        assert_eq!(cloned.status, DeflateStatus::Busy);
        assert_eq!(cloned.sym_next, 3); // one symbol tallied
        assert_eq!(cloned.dyn_ltree[b'X' as usize].freq(), 1);

        // Verify independence: modifying clone doesn't affect original
        let mut cloned = cloned;
        cloned.tally_lit(b'Y');
        assert_eq!(cloned.sym_next, 6);
        assert_eq!(state.sym_next, 3); // original unchanged
    }

    #[test]
    fn test_dist_code_table_sanity() {
        // DIST_CODE should have 512 entries
        assert_eq!(DIST_CODE.len(), 512);
        // First entries: code 0 for dist 0,1; code 1 for dist 1
        // (exact values from the C trees.h table)
        assert_eq!(DIST_CODE[0], 0);
        assert_eq!(DIST_CODE[1], 1);
        assert_eq!(DIST_CODE[2], 2);
        assert_eq!(DIST_CODE[3], 3);
    }

    #[test]
    fn test_length_code_table_sanity() {
        // LENGTH_CODE should have MAX_MATCH - MIN_MATCH + 1 = 256 entries
        assert_eq!(LENGTH_CODE.len(), MAX_MATCH - MIN_MATCH + 1);
        // First entry: length 0 (= match of length MIN_MATCH) → code 0
        assert_eq!(LENGTH_CODE[0], 0);
    }

    #[test]
    fn test_all_tree_array_sizes() {
        let state = DeflateState::new(15, 8, 6, 0, 1);
        assert_eq!(state.dyn_ltree.len(), DYN_LTREE_SIZE);
        assert_eq!(state.dyn_dtree.len(), DYN_DTREE_SIZE);
        assert_eq!(state.bl_tree.len(), BL_TREE_SIZE);
        assert_eq!(state.bl_count.len(), MAX_BITS + 1);
        assert_eq!(state.heap.len(), HEAP_SIZE);
        assert_eq!(state.depth.len(), HEAP_SIZE);
    }
}
