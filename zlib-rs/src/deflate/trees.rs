// Huffman tree construction and DEFLATE block encoding.
//
// This module ports the functionality from C `trees.c` (1119 lines) and
// `trees.h` (128 lines) of the zlib 1.3.2.1-motley C library.
//
// It implements:
// - Pre-computed static Huffman tree data (from `trees.h`)
// - Bit-level output encoding (`send_bits`, `bi_flush`, `bi_windup`)
// - Huffman tree building with bit-length limiting (`build_tree`, `gen_bitlen`)
// - Block type selection: stored, static Huffman, or dynamic Huffman
// - Symbol encoding (`compress_block`)
// - The public API: `tr_init`, `tr_flush_block`, `tr_tally`, etc.
//
// # Safety
//
// This module contains **zero** `unsafe` blocks. All buffer management
// uses safe Rust indexing and owned collections.

use super::state::DeflateState;
use crate::constants::{
    BL_CODES, BUF_SIZE, D_CODES, DYN_TREES, HEAP_SIZE, L_CODES, LENGTH_CODES,
    LITERALS, MAX_BITS, MAX_MATCH, MIN_MATCH, STATIC_TREES, STORED_BLOCK,
    Z_BINARY, Z_FIXED, Z_TEXT, Z_UNKNOWN,
};

// ─── CtData (trees.rs compact representation) ────────────────────────────────

/// Compact Huffman code table entry for **static** tree constant tables.
///
/// This two-field struct mirrors the C `ct_data` union from `deflate.h`
/// lines 72–81, where a single pair of 16-bit values serves dual purposes:
///
/// - **Tree building phase:** `freq_or_code` stores the symbol frequency;
///   `dad_or_len` stores the parent node index.
/// - **Encoding phase:** `freq_or_code` stores the Huffman bit-string code;
///   `dad_or_len` stores the bit-string length.
///
/// Accessor methods (`freq()` / `code()` and `dad()` / `len()`) provide
/// phase-appropriate naming, making the dual-use explicit and readable.
///
/// This type is used for the pre-computed `STATIC_LTREE` and `STATIC_DTREE`
/// constant arrays.  The mutable per-stream tree arrays in [`DeflateState`]
/// use the four-field [`super::state::CtData`] instead.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct CtData {
    /// During tree building: symbol frequency count.
    /// During encoding: the Huffman bit-string code.
    pub freq_or_code: u16,
    /// During tree building: parent node index.
    /// During encoding: the bit-string length in bits.
    pub dad_or_len: u16,
}

impl core::fmt::Debug for CtData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CtData")
            .field("freq_or_code", &self.freq_or_code)
            .field("dad_or_len", &self.dad_or_len)
            .finish()
    }
}

impl Default for CtData {
    /// Returns a zeroed `CtData` entry (frequency = 0, length = 0).
    #[inline]
    fn default() -> Self {
        Self {
            freq_or_code: 0,
            dad_or_len: 0,
        }
    }
}

impl CtData {
    /// Creates a new `CtData` with the given code and length values.
    #[inline]
    const fn new(code: u16, len: u16) -> Self {
        Self {
            freq_or_code: code,
            dad_or_len: len,
        }
    }

    /// Returns the frequency (tree-building interpretation of `freq_or_code`).
    #[inline]
    pub fn freq(&self) -> u16 {
        self.freq_or_code
    }

    /// Sets the frequency (tree-building interpretation of `freq_or_code`).
    #[inline]
    pub fn set_freq(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Returns the Huffman code (encoding interpretation of `freq_or_code`).
    #[inline]
    pub fn code(&self) -> u16 {
        self.freq_or_code
    }

    /// Sets the Huffman code (encoding interpretation of `freq_or_code`).
    #[inline]
    pub fn set_code(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Returns the parent index (tree-building interpretation of `dad_or_len`).
    #[inline]
    pub fn dad(&self) -> u16 {
        self.dad_or_len
    }

    /// Sets the parent index (tree-building interpretation of `dad_or_len`).
    #[inline]
    pub fn set_dad(&mut self, v: u16) {
        self.dad_or_len = v;
    }

    /// Returns the bit-string length (encoding interpretation of `dad_or_len`).
    #[inline]
    pub fn len(&self) -> u16 {
        self.dad_or_len
    }

    /// Sets the bit-string length (encoding interpretation of `dad_or_len`).
    #[inline]
    pub fn set_len(&mut self, v: u16) {
        self.dad_or_len = v;
    }
}

// ─── TreeKind (internal dispatch enum) ───────────────────────────────────────

/// Identifies which dynamic tree array within [`DeflateState`] to operate on.
///
/// Used by tree-building and encoding functions that need to be polymorphic
/// over the literal/length, distance, and bit-length trees without requiring
/// separate mutable borrows of different `DeflateState` fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TreeKind {
    /// The dynamic literal/length tree (`dyn_ltree`).
    Literal,
    /// The dynamic distance tree (`dyn_dtree`).
    Distance,
    /// The bit-length tree (`bl_tree`).
    BitLength,
}

// ─── Tree access helpers ─────────────────────────────────────────────────────
// These small inline functions dispatch on TreeKind to access the appropriate
// tree array within DeflateState, keeping the main logic readable.

/// Read the frequency of tree element at `idx`.
#[inline]
fn tree_freq(state: &DeflateState, kind: TreeKind, idx: usize) -> u16 {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].freq,
        TreeKind::Distance => state.dyn_dtree[idx].freq,
        TreeKind::BitLength => state.bl_tree[idx].freq,
    }
}

/// Write the frequency of tree element at `idx`.
#[inline]
fn set_tree_freq(state: &mut DeflateState, kind: TreeKind, idx: usize, val: u16) {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].freq = val,
        TreeKind::Distance => state.dyn_dtree[idx].freq = val,
        TreeKind::BitLength => state.bl_tree[idx].freq = val,
    }
}

/// Read the Huffman code of tree element at `idx`.
#[inline]
fn tree_code(state: &DeflateState, kind: TreeKind, idx: usize) -> u16 {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].code,
        TreeKind::Distance => state.dyn_dtree[idx].code,
        TreeKind::BitLength => state.bl_tree[idx].code,
    }
}

/// Write the Huffman code of tree element at `idx`.
#[inline]
fn set_tree_code(state: &mut DeflateState, kind: TreeKind, idx: usize, val: u16) {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].code = val,
        TreeKind::Distance => state.dyn_dtree[idx].code = val,
        TreeKind::BitLength => state.bl_tree[idx].code = val,
    }
}

/// Read the parent-node index (dad) of tree element at `idx`.
#[inline]
fn tree_dad(state: &DeflateState, kind: TreeKind, idx: usize) -> u16 {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].dad,
        TreeKind::Distance => state.dyn_dtree[idx].dad,
        TreeKind::BitLength => state.bl_tree[idx].dad,
    }
}

/// Write the parent-node index (dad) of tree element at `idx`.
#[inline]
fn set_tree_dad(state: &mut DeflateState, kind: TreeKind, idx: usize, val: u16) {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].dad = val,
        TreeKind::Distance => state.dyn_dtree[idx].dad = val,
        TreeKind::BitLength => state.bl_tree[idx].dad = val,
    }
}

/// Read the bit-length of tree element at `idx`.
#[inline]
fn tree_len(state: &DeflateState, kind: TreeKind, idx: usize) -> u16 {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].len,
        TreeKind::Distance => state.dyn_dtree[idx].len,
        TreeKind::BitLength => state.bl_tree[idx].len,
    }
}

/// Write the bit-length of tree element at `idx`.
#[inline]
fn set_tree_len(state: &mut DeflateState, kind: TreeKind, idx: usize, val: u16) {
    match kind {
        TreeKind::Literal => state.dyn_ltree[idx].len = val,
        TreeKind::Distance => state.dyn_dtree[idx].len = val,
        TreeKind::BitLength => state.bl_tree[idx].len = val,
    }
}

/// Read the `max_code` for a tree kind from `DeflateState`.
#[inline]
fn get_max_code(state: &DeflateState, kind: TreeKind) -> i32 {
    match kind {
        TreeKind::Literal => state.l_desc.max_code,
        TreeKind::Distance => state.d_desc.max_code,
        TreeKind::BitLength => state.bl_desc.max_code,
    }
}

/// Write the `max_code` for a tree kind into `DeflateState`.
#[inline]
fn set_max_code(state: &mut DeflateState, kind: TreeKind, val: i32) {
    match kind {
        TreeKind::Literal => state.l_desc.max_code = val,
        TreeKind::Distance => state.d_desc.max_code = val,
        TreeKind::BitLength => state.bl_desc.max_code = val,
    }
}

// ─── StaticTreeDesc ──────────────────────────────────────────────────────────

/// Immutable descriptor for a static Huffman tree.
///
/// Replaces the C `static_tree_desc_s` struct from `trees.c` lines 101–108.
/// Each descriptor links a pre-computed static tree, extra-bits table,
/// element count, and maximum code length.
pub(crate) struct StaticTreeDesc {
    /// Pre-computed static tree (`None` for the bit-length tree).
    pub static_tree: Option<&'static [CtData]>,
    /// Extra bits for each code beyond `extra_base`.
    pub extra_bits: &'static [u8],
    /// First code with extra bits.
    pub extra_base: usize,
    /// Number of elements in the tree.
    pub elems: usize,
    /// Maximum code bit-length (15 for lit/dist, 7 for bit-length tree).
    pub max_length: usize,
}

// ─── Module-private constants ────────────────────────────────────────────────

/// Maximum bit-length for bit-length codes (from `trees.c` line 69).
const MAX_BL_BITS: usize = 7;

/// End of block literal code (from `trees.c` line 52).
const END_BLOCK: usize = 256;

/// Repeat previous bit-length 3–6 times (2 extra bits).
const REP_3_6: usize = 16;

/// Repeat a zero bit-length 3–10 times (3 extra bits).
const REPZ_3_10: usize = 17;

/// Repeat a zero bit-length 11–138 times (7 extra bits).
const REPZ_11_138: usize = 18;

/// Index of the smallest element in the 1-based heap (heap[0] is unused).
const SMALLEST: usize = 1;

// ─── Extra-bits tables (from trees.c lines 70–87) ───────────────────────────

/// Extra bits for each length code (from `trees.c` line 72).
pub(crate) const EXTRA_LBITS: [u8; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2,
    3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each distance code (from `trees.c` line 75).
pub(crate) const EXTRA_DBITS: [u8; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
    7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
];

/// Extra bits for each bit-length code (from `trees.c` line 78).
const EXTRA_BLBITS: [u8; BL_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7,
];

/// Transmission order for bit-length codes (from `trees.c` line 83).
const BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ─── Static literal/length tree (from trees.h) ──────────────────────────────

/// Pre-computed static literal/length Huffman tree (L_CODES + 2 = 288 entries).
/// Values copied exactly from `trees.h` `static_ltree`.
///
/// Note: L_CODES is 286, so L_CODES + 2 = 288.  The array contains codes
/// 0–287 plus 2 zero-sentinels, but the sentinels are at indices 286 and 287
/// which gives exactly 288 elements total.
#[allow(clippy::unreadable_literal)]
pub(crate) const STATIC_LTREE: [CtData; L_CODES + 2] = [
    // codes 0–143: 8-bit
    CtData::new( 12, 8), CtData::new(140, 8), CtData::new( 76, 8), CtData::new(204, 8),
    CtData::new( 44, 8), CtData::new(172, 8), CtData::new(108, 8), CtData::new(236, 8),
    CtData::new( 28, 8), CtData::new(156, 8), CtData::new( 92, 8), CtData::new(220, 8),
    CtData::new( 60, 8), CtData::new(188, 8), CtData::new(124, 8), CtData::new(252, 8),
    CtData::new(  2, 8), CtData::new(130, 8), CtData::new( 66, 8), CtData::new(194, 8),
    CtData::new( 34, 8), CtData::new(162, 8), CtData::new( 98, 8), CtData::new(226, 8),
    CtData::new( 18, 8), CtData::new(146, 8), CtData::new( 82, 8), CtData::new(210, 8),
    CtData::new( 50, 8), CtData::new(178, 8), CtData::new(114, 8), CtData::new(242, 8),
    CtData::new( 10, 8), CtData::new(138, 8), CtData::new( 74, 8), CtData::new(202, 8),
    CtData::new( 42, 8), CtData::new(170, 8), CtData::new(106, 8), CtData::new(234, 8),
    CtData::new( 26, 8), CtData::new(154, 8), CtData::new( 90, 8), CtData::new(218, 8),
    CtData::new( 58, 8), CtData::new(186, 8), CtData::new(122, 8), CtData::new(250, 8),
    CtData::new(  6, 8), CtData::new(134, 8), CtData::new( 70, 8), CtData::new(198, 8),
    CtData::new( 38, 8), CtData::new(166, 8), CtData::new(102, 8), CtData::new(230, 8),
    CtData::new( 22, 8), CtData::new(150, 8), CtData::new( 86, 8), CtData::new(214, 8),
    CtData::new( 54, 8), CtData::new(182, 8), CtData::new(118, 8), CtData::new(246, 8),
    CtData::new( 14, 8), CtData::new(142, 8), CtData::new( 78, 8), CtData::new(206, 8),
    CtData::new( 46, 8), CtData::new(174, 8), CtData::new(110, 8), CtData::new(238, 8),
    CtData::new( 30, 8), CtData::new(158, 8), CtData::new( 94, 8), CtData::new(222, 8),
    CtData::new( 62, 8), CtData::new(190, 8), CtData::new(126, 8), CtData::new(254, 8),
    CtData::new(  1, 8), CtData::new(129, 8), CtData::new( 65, 8), CtData::new(193, 8),
    CtData::new( 33, 8), CtData::new(161, 8), CtData::new( 97, 8), CtData::new(225, 8),
    CtData::new( 17, 8), CtData::new(145, 8), CtData::new( 81, 8), CtData::new(209, 8),
    CtData::new( 49, 8), CtData::new(177, 8), CtData::new(113, 8), CtData::new(241, 8),
    CtData::new(  9, 8), CtData::new(137, 8), CtData::new( 73, 8), CtData::new(201, 8),
    CtData::new( 41, 8), CtData::new(169, 8), CtData::new(105, 8), CtData::new(233, 8),
    CtData::new( 25, 8), CtData::new(153, 8), CtData::new( 89, 8), CtData::new(217, 8),
    CtData::new( 57, 8), CtData::new(185, 8), CtData::new(121, 8), CtData::new(249, 8),
    CtData::new(  5, 8), CtData::new(133, 8), CtData::new( 69, 8), CtData::new(197, 8),
    CtData::new( 37, 8), CtData::new(165, 8), CtData::new(101, 8), CtData::new(229, 8),
    CtData::new( 21, 8), CtData::new(149, 8), CtData::new( 85, 8), CtData::new(213, 8),
    CtData::new( 53, 8), CtData::new(181, 8), CtData::new(117, 8), CtData::new(245, 8),
    CtData::new( 13, 8), CtData::new(141, 8), CtData::new( 77, 8), CtData::new(205, 8),
    CtData::new( 45, 8), CtData::new(173, 8), CtData::new(109, 8), CtData::new(237, 8),
    CtData::new( 29, 8), CtData::new(157, 8), CtData::new( 93, 8), CtData::new(221, 8),
    CtData::new( 61, 8), CtData::new(189, 8), CtData::new(125, 8), CtData::new(253, 8),
    // codes 144–255: 9-bit
    CtData::new( 19, 9), CtData::new(275, 9), CtData::new(147, 9), CtData::new(403, 9),
    CtData::new( 83, 9), CtData::new(339, 9), CtData::new(211, 9), CtData::new(467, 9),
    CtData::new( 51, 9), CtData::new(307, 9), CtData::new(179, 9), CtData::new(435, 9),
    CtData::new(115, 9), CtData::new(371, 9), CtData::new(243, 9), CtData::new(499, 9),
    CtData::new( 11, 9), CtData::new(267, 9), CtData::new(139, 9), CtData::new(395, 9),
    CtData::new( 75, 9), CtData::new(331, 9), CtData::new(203, 9), CtData::new(459, 9),
    CtData::new( 43, 9), CtData::new(299, 9), CtData::new(171, 9), CtData::new(427, 9),
    CtData::new(107, 9), CtData::new(363, 9), CtData::new(235, 9), CtData::new(491, 9),
    CtData::new( 27, 9), CtData::new(283, 9), CtData::new(155, 9), CtData::new(411, 9),
    CtData::new( 91, 9), CtData::new(347, 9), CtData::new(219, 9), CtData::new(475, 9),
    CtData::new( 59, 9), CtData::new(315, 9), CtData::new(187, 9), CtData::new(443, 9),
    CtData::new(123, 9), CtData::new(379, 9), CtData::new(251, 9), CtData::new(507, 9),
    CtData::new(  7, 9), CtData::new(263, 9), CtData::new(135, 9), CtData::new(391, 9),
    CtData::new( 71, 9), CtData::new(327, 9), CtData::new(199, 9), CtData::new(455, 9),
    CtData::new( 39, 9), CtData::new(295, 9), CtData::new(167, 9), CtData::new(423, 9),
    CtData::new(103, 9), CtData::new(359, 9), CtData::new(231, 9), CtData::new(487, 9),
    CtData::new( 23, 9), CtData::new(279, 9), CtData::new(151, 9), CtData::new(407, 9),
    CtData::new( 87, 9), CtData::new(343, 9), CtData::new(215, 9), CtData::new(471, 9),
    CtData::new( 55, 9), CtData::new(311, 9), CtData::new(183, 9), CtData::new(439, 9),
    CtData::new(119, 9), CtData::new(375, 9), CtData::new(247, 9), CtData::new(503, 9),
    CtData::new( 15, 9), CtData::new(271, 9), CtData::new(143, 9), CtData::new(399, 9),
    CtData::new( 79, 9), CtData::new(335, 9), CtData::new(207, 9), CtData::new(463, 9),
    CtData::new( 47, 9), CtData::new(303, 9), CtData::new(175, 9), CtData::new(431, 9),
    CtData::new(111, 9), CtData::new(367, 9), CtData::new(239, 9), CtData::new(495, 9),
    CtData::new( 31, 9), CtData::new(287, 9), CtData::new(159, 9), CtData::new(415, 9),
    CtData::new( 95, 9), CtData::new(351, 9), CtData::new(223, 9), CtData::new(479, 9),
    CtData::new( 63, 9), CtData::new(319, 9), CtData::new(191, 9), CtData::new(447, 9),
    CtData::new(127, 9), CtData::new(383, 9), CtData::new(255, 9), CtData::new(511, 9),
    // codes 256–279: 7-bit
    CtData::new(  0, 7), CtData::new( 64, 7), CtData::new( 32, 7), CtData::new( 96, 7),
    CtData::new( 16, 7), CtData::new( 80, 7), CtData::new( 48, 7), CtData::new(112, 7),
    CtData::new(  8, 7), CtData::new( 72, 7), CtData::new( 40, 7), CtData::new(104, 7),
    CtData::new( 24, 7), CtData::new( 88, 7), CtData::new( 56, 7), CtData::new(120, 7),
    CtData::new(  4, 7), CtData::new( 68, 7), CtData::new( 36, 7), CtData::new(100, 7),
    CtData::new( 20, 7), CtData::new( 84, 7), CtData::new( 52, 7), CtData::new(116, 7),
    // codes 280–287: 8-bit
    CtData::new(  3, 8), CtData::new(131, 8), CtData::new( 67, 8), CtData::new(195, 8),
    CtData::new( 35, 8), CtData::new(163, 8), CtData::new( 99, 8), CtData::new(227, 8),
];

/// Pre-computed static distance Huffman tree (30 entries, all 5-bit).
/// Values copied exactly from `trees.h` `static_dtree`.
pub(crate) const STATIC_DTREE: [CtData; D_CODES] = [
    CtData::new( 0, 5), CtData::new(16, 5), CtData::new( 8, 5), CtData::new(24, 5),
    CtData::new( 4, 5), CtData::new(20, 5), CtData::new(12, 5), CtData::new(28, 5),
    CtData::new( 2, 5), CtData::new(18, 5), CtData::new(10, 5), CtData::new(26, 5),
    CtData::new( 6, 5), CtData::new(22, 5), CtData::new(14, 5), CtData::new(30, 5),
    CtData::new( 1, 5), CtData::new(17, 5), CtData::new( 9, 5), CtData::new(25, 5),
    CtData::new( 5, 5), CtData::new(21, 5), CtData::new(13, 5), CtData::new(29, 5),
    CtData::new( 3, 5), CtData::new(19, 5), CtData::new(11, 5), CtData::new(27, 5),
    CtData::new( 7, 5), CtData::new(23, 5),
];

/// Distance code lookup table (512 entries, from `trees.h` `_dist_code`).
pub(crate) const DIST_CODE: [u8; 512] = [
     0,  1,  2,  3,  4,  4,  5,  5,  6,  6,  6,  6,  7,  7,  7,  7,
     8,  8,  8,  8,  8,  8,  8,  8,  9,  9,  9,  9,  9,  9,  9,  9,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
    13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
     0,  0, 16, 17, 18, 18, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21,
    22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
];

/// Length code lookup table (256 entries, from `trees.h` `_length_code`).
pub(crate) const LENGTH_CODE: [u8; MAX_MATCH - MIN_MATCH + 1] = [
     0,  1,  2,  3,  4,  5,  6,  7,  8,  8,  9,  9, 10, 10, 11, 11,
    12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15,
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17,
    18, 18, 18, 18, 18, 18, 18, 18, 19, 19, 19, 19, 19, 19, 19, 19,
    20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20,
    21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21,
    22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22,
    23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 28,
];

/// Base match lengths for each length code (from `trees.h` `base_length`).
pub(crate) const BASE_LENGTH: [u32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28,
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 0,
];

/// Base distances for each distance code (from `trees.h` `base_dist`).
pub(crate) const BASE_DIST: [u32; D_CODES] = [
        0,     1,     2,     3,     4,     6,     8,    12,    16,    24,
       32,    48,    64,    96,   128,   192,   256,   384,   512,   768,
     1024,  1536,  2048,  3072,  4096,  6144,  8192, 12288, 16384, 24576,
];

// ─── Static tree descriptors ────────────────────────────────────────────────

/// Descriptor for the static literal/length tree.
pub(crate) static STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_LTREE),
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the static distance tree.
pub(crate) static STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_DTREE),
    extra_bits: &EXTRA_DBITS,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the bit-length tree (no associated static tree).
pub(crate) static STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_BLBITS,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

// =============================================================================
//  Bit-level output functions
// =============================================================================

/// Append two bytes (little-endian) to the pending output buffer.
///
/// The pending buffer is always pre-sized by `deflateInit2` to accommodate
/// the worst-case output for any single block (see `lit_bufsize * 4`
/// allocation in `DeflateState::new`).  A full buffer here indicates a
/// logic error in the caller's block flushing sequence.
#[inline]
fn put_short(state: &mut DeflateState, val: u16) {
    let p = state.pending;
    debug_assert!(
        p + 1 < state.pending_buf.len(),
        "put_short: pending buffer overflow (pending={}, capacity={})",
        p,
        state.pending_buf.len()
    );
    if p + 1 < state.pending_buf.len() {
        state.pending_buf[p] = val as u8;
        state.pending_buf[p + 1] = (val >> 8) as u8;
        state.pending += 2;
    }
}

/// Append a single byte to the pending output buffer.
///
/// The pending buffer is always pre-sized to accommodate worst-case output
/// for any single block.  A full buffer here indicates a logic error in the
/// caller's block flushing sequence.
#[inline]
fn put_byte(state: &mut DeflateState, val: u8) {
    let p = state.pending;
    debug_assert!(
        p < state.pending_buf.len(),
        "put_byte: pending buffer overflow (pending={}, capacity={})",
        p,
        state.pending_buf.len()
    );
    if p < state.pending_buf.len() {
        state.pending_buf[p] = val;
        state.pending += 1;
    }
}

/// Send `length` bits of `value` to the output stream.
///
/// Ported from `trees.c` lines 250–286.  When the bit buffer would overflow
/// its 16-bit width, the low 16 bits are flushed as a short and the
/// remaining upper bits become the new bit buffer contents.
pub(crate) fn send_bits(state: &mut DeflateState, value: u32, length: u32) {
    let bi_valid = state.bi_valid;
    let buf_size = BUF_SIZE as i32; // 16
    if bi_valid > buf_size - length as i32 {
        state.bi_buf |= (value << bi_valid as u32) as u16;
        put_short(state, state.bi_buf);
        // Carry the upper bits of `value` that did not fit into the 16-bit
        // buffer.  `right_shift` is `buf_size - bi_valid` (0..16).  A shift
        // of 0 means the entire value overflows into the next word — this is
        // legal in Rust (unlike C where shifting by >= width is UB).  Only
        // guard against shifts ≥ 32 which would discard the value entirely.
        let right_shift = (buf_size - bi_valid) as u32;
        state.bi_buf = if right_shift < 32 {
            (value >> right_shift) as u16
        } else {
            0
        };
        state.bi_valid = bi_valid + length as i32 - buf_size;
    } else {
        state.bi_buf |= (value << bi_valid as u32) as u16;
        state.bi_valid = bi_valid + length as i32;
    }
}

/// Send a Huffman code from a dynamic tree through the bit buffer.
#[inline]
fn send_code_dyn(state: &mut DeflateState, c: usize, kind: TreeKind) {
    let code = tree_code(state, kind, c) as u32;
    let len = tree_len(state, kind, c) as u32;
    send_bits(state, code, len);
}

/// Send a Huffman code from a static (const) tree through the bit buffer.
#[inline]
fn send_code_static(state: &mut DeflateState, c: usize, tree: &[CtData]) {
    send_bits(state, tree[c].code() as u32, tree[c].len() as u32);
}

/// Reverse the `len` lowest bits of `code`.
fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res: u32 = 0;
    while len > 0 {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
    }
    res >> 1
}

/// Flush the bit buffer, writing whole bytes to the pending buffer.
pub(crate) fn bi_flush(state: &mut DeflateState) {
    if state.bi_valid == 16 {
        put_short(state, state.bi_buf);
        state.bi_buf = 0;
        state.bi_valid = 0;
    } else if state.bi_valid >= 8 {
        put_byte(state, state.bi_buf as u8);
        state.bi_buf >>= 8;
        state.bi_valid -= 8;
    }
}

/// Flush the bit buffer and align the output on a byte boundary.
pub(crate) fn bi_windup(state: &mut DeflateState) {
    if state.bi_valid > 8 {
        put_short(state, state.bi_buf);
    } else if state.bi_valid > 0 {
        put_byte(state, state.bi_buf as u8);
    }
    state.bi_buf = 0;
    state.bi_valid = 0;
}

/// Map a distance value to a distance code using the `DIST_CODE` table.
#[inline]
pub(crate) fn d_code(dist: u32) -> u32 {
    if dist < 256 {
        DIST_CODE[dist as usize] as u32
    } else {
        DIST_CODE[256 + (dist >> 7) as usize] as u32
    }
}

// =============================================================================
//  Tree building functions
// =============================================================================

/// Reset all tree frequency counters and bookkeeping for a new block.
pub(crate) fn init_block(state: &mut DeflateState) {
    for n in 0..L_CODES {
        state.dyn_ltree[n].freq = 0;
    }
    for n in 0..D_CODES {
        state.dyn_dtree[n].freq = 0;
    }
    for n in 0..BL_CODES {
        state.bl_tree[n].freq = 0;
    }
    state.dyn_ltree[END_BLOCK].freq = 1;
    state.opt_len = 0;
    state.static_len = 0;
    state.sym_next = 0;
    state.matches = 0;
}

/// Initialise the tree module for a new deflate stream.
pub(crate) fn tr_init(state: &mut DeflateState) {
    state.l_desc.max_code = 0;
    state.d_desc.max_code = 0;
    state.bl_desc.max_code = 0;
    state.bi_buf = 0;
    state.bi_valid = 0;
    state.bi_used = 0;
    init_block(state);
}

/// Compare two tree elements for the heap ordering.
///
/// Returns `true` when element `n` is "smaller" than element `m`
/// (lower frequency, or same frequency but shallower depth).
#[inline]
fn smaller(state: &DeflateState, kind: TreeKind, n: usize, m: usize) -> bool {
    let fn_ = tree_freq(state, kind, n);
    let fm = tree_freq(state, kind, m);
    fn_ < fm || (fn_ == fm && state.depth[n] <= state.depth[m])
}

/// Restore the heap property by sifting element `k` downward.
///
/// Ported from `trees.c` lines 509–528.
fn pqdownheap(state: &mut DeflateState, kind: TreeKind, mut k: usize) {
    let v = state.heap[k];
    let mut j = k << 1;
    while j <= state.heap_len {
        if j < state.heap_len {
            let hj = state.heap[j] as usize;
            let hj1 = state.heap[j + 1] as usize;
            if smaller(state, kind, hj1, hj) {
                j += 1;
            }
        }
        let hj = state.heap[j] as usize;
        if smaller(state, kind, v as usize, hj) {
            break;
        }
        state.heap[k] = state.heap[j];
        k = j;
        j <<= 1;
    }
    state.heap[k] = v;
}

/// Compute optimal bit lengths for the tree and update `opt_len` / `static_len`.
///
/// Ported from `trees.c` lines 540–613.
fn gen_bitlen(state: &mut DeflateState, kind: TreeKind, stat_desc: &StaticTreeDesc) {
    let extra = stat_desc.extra_bits;
    let base = stat_desc.extra_base;
    let max_length = stat_desc.max_length;
    let max_code = get_max_code(state, kind);

    for bits in 0..=MAX_BITS {
        state.bl_count[bits] = 0;
    }

    // Set the root bit-length to zero.
    let root = state.heap[state.heap_max] as usize;
    set_tree_len(state, kind, root, 0);

    let mut overflow: i32 = 0;
    let start = state.heap_max + 1;
    for h in start..HEAP_SIZE {
        let n = state.heap[h] as usize;
        let dad_of_n = tree_dad(state, kind, n) as usize;
        let mut bits = tree_len(state, kind, dad_of_n) as usize + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        set_tree_len(state, kind, n, bits as u16);

        // Skip internal nodes above max_code.
        if n as i32 > max_code {
            continue;
        }
        state.bl_count[bits] += 1;
        let xbits: u32 = if n >= base && (n - base) < extra.len() {
            extra[n - base] as u32
        } else {
            0
        };
        let f = tree_freq(state, kind, n) as u64;
        state.opt_len = state.opt_len.wrapping_add(f.wrapping_mul(bits as u64 + xbits as u64));
        if let Some(stree) = stat_desc.static_tree {
            if n < stree.len() {
                state.static_len = state.static_len.wrapping_add(
                    f.wrapping_mul(stree[n].len() as u64 + xbits as u64),
                );
            }
        }
    }
    if overflow == 0 {
        return;
    }

    // Redistribute bit lengths to satisfy max_length constraint.
    loop {
        let mut bits = max_length - 1;
        while bits > 0 && state.bl_count[bits] == 0 {
            bits -= 1;
        }
        state.bl_count[bits] -= 1;
        state.bl_count[bits + 1] += 2;
        state.bl_count[max_length] -= 1;
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // Recompute all bit lengths in increasing frequency order.
    let mut h = HEAP_SIZE;
    for bits in (1..=max_length).rev() {
        let mut n_remaining = state.bl_count[bits] as i32;
        while n_remaining > 0 {
            h -= 1;
            if h < 1 {
                break;
            }
            let m = state.heap[h] as usize;
            if m as i32 > max_code {
                continue;
            }
            let old_len = tree_len(state, kind, m) as u64;
            if old_len != bits as u64 {
                let freq_m = tree_freq(state, kind, m) as u64;
                state.opt_len = state
                    .opt_len
                    .wrapping_add((bits as u64).wrapping_mul(freq_m))
                    .wrapping_sub(old_len.wrapping_mul(freq_m));
                set_tree_len(state, kind, m, bits as u16);
            }
            n_remaining -= 1;
        }
    }
}

/// Generate canonical Huffman codes from the computed bit lengths.
///
/// Ported from `trees.c` lines 209–245.
fn gen_codes(state: &mut DeflateState, kind: TreeKind, max_code: usize) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u16 = 0;
    for bits in 1..=MAX_BITS {
        code = (code.wrapping_add(state.bl_count[bits - 1])) << 1;
        next_code[bits] = code;
    }
    for n in 0..=max_code {
        let len = tree_len(state, kind, n) as usize;
        if len == 0 {
            continue;
        }
        let reversed = bi_reverse(u32::from(next_code[len]), len as u32) as u16;
        set_tree_code(state, kind, n, reversed);
        next_code[len] += 1;
    }
}

/// Build an optimal Huffman tree from symbol frequencies.
///
/// Ported from `trees.c` lines 627–706.
fn build_tree(state: &mut DeflateState, kind: TreeKind, stat_desc: &StaticTreeDesc) {
    let elems = stat_desc.elems;
    state.heap_len = 0;
    state.heap_max = HEAP_SIZE;

    let mut max_code: i32 = -1;
    for n in 0..elems {
        if tree_freq(state, kind, n) != 0 {
            state.heap_len += 1;
            state.heap[state.heap_len] = n as i32;
            max_code = n as i32;
            state.depth[n] = 0;
        } else {
            set_tree_len(state, kind, n, 0);
        }
    }

    // Ensure at least 2 codes with non-zero frequency (pkzip requirement).
    while state.heap_len < 2 {
        state.heap_len += 1;
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        state.heap[state.heap_len] = node as i32;
        set_tree_freq(state, kind, node, 1);
        state.depth[node] = 0;
        state.opt_len = state.opt_len.wrapping_sub(1);
        if let Some(stree) = stat_desc.static_tree {
            if node < stree.len() {
                state.static_len = state.static_len.wrapping_sub(stree[node].len() as u64);
            }
        }
    }
    set_max_code(state, kind, max_code);

    // Build sub-heaps from bottom up.
    let half = state.heap_len / 2;
    for n in (1..=half).rev() {
        pqdownheap(state, kind, n);
    }

    // Combine two least-frequent nodes repeatedly.
    let mut node = elems;
    loop {
        // pqremove: extract smallest
        let n = state.heap[SMALLEST] as usize;
        state.heap[SMALLEST] = state.heap[state.heap_len];
        state.heap_len -= 1;
        pqdownheap(state, kind, SMALLEST);

        let m = state.heap[SMALLEST] as usize;

        state.heap_max -= 1;
        state.heap[state.heap_max] = n as i32;
        state.heap_max -= 1;
        state.heap[state.heap_max] = m as i32;

        let combined = tree_freq(state, kind, n).wrapping_add(tree_freq(state, kind, m));
        set_tree_freq(state, kind, node, combined);
        let d = core::cmp::max(state.depth[n], state.depth[m]);
        state.depth[node] = d + 1;
        set_tree_dad(state, kind, n, node as u16);
        set_tree_dad(state, kind, m, node as u16);

        state.heap[SMALLEST] = node as i32;
        node += 1;
        pqdownheap(state, kind, SMALLEST);

        if state.heap_len < 2 {
            break;
        }
    }

    state.heap_max -= 1;
    state.heap[state.heap_max] = state.heap[SMALLEST];

    gen_bitlen(state, kind, stat_desc);
    gen_codes(state, kind, max_code as usize);
}

// =============================================================================
//  Block encoding helpers
// =============================================================================

/// Scan a dynamic tree to determine bit-length tree frequencies.
///
/// Ported from `trees.c` lines 712–747.
fn scan_tree(state: &mut DeflateState, kind: TreeKind, max_code: usize) {
    let mut prevlen: i32 = -1;
    let mut curlen: u16 = 0;
    let mut nextlen: u16 = tree_len(state, kind, 0);
    let mut count: u32 = 0;
    let mut max_count: u32 = 7;
    let mut min_count: u32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    // Guard sentinel: set the element just past max_code to 0xFFFF so the
    // last real element is always flushed.
    set_tree_len(state, kind, max_code + 1, 0xFFFF);

    for n in 0..=max_code {
        curlen = nextlen;
        nextlen = tree_len(state, kind, n + 1);
        count += 1;
        if count < max_count && nextlen == curlen {
            continue;
        }
        if count < min_count {
            state.bl_tree[curlen as usize].freq =
                state.bl_tree[curlen as usize].freq.wrapping_add(count as u16);
        } else if curlen != 0 {
            if curlen as i32 != prevlen {
                state.bl_tree[curlen as usize].freq =
                    state.bl_tree[curlen as usize].freq.wrapping_add(1);
            }
            state.bl_tree[REP_3_6].freq = state.bl_tree[REP_3_6].freq.wrapping_add(1);
        } else if count <= 10 {
            state.bl_tree[REPZ_3_10].freq = state.bl_tree[REPZ_3_10].freq.wrapping_add(1);
        } else {
            state.bl_tree[REPZ_11_138].freq = state.bl_tree[REPZ_11_138].freq.wrapping_add(1);
        }
        count = 0;
        prevlen = curlen as i32;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Send the bit-length encoded representation of a tree using the bl_tree.
///
/// Ported from `trees.c` lines 753–794.
fn send_tree(state: &mut DeflateState, kind: TreeKind, max_code: usize) {
    let mut prevlen: i32 = -1;
    let mut curlen: u16 = 0;
    let mut nextlen: u16 = tree_len(state, kind, 0);
    let mut count: u32 = 0;
    let mut max_count: u32 = 7;
    let mut min_count: u32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    for n in 0..=max_code {
        curlen = nextlen;
        nextlen = tree_len(state, kind, n + 1);
        count += 1;
        if count < max_count && nextlen == curlen {
            continue;
        }
        if count < min_count {
            for _ in 0..count {
                send_code_dyn(state, curlen as usize, TreeKind::BitLength);
            }
        } else if curlen != 0 {
            if curlen as i32 != prevlen {
                send_code_dyn(state, curlen as usize, TreeKind::BitLength);
                count -= 1;
            }
            send_code_dyn(state, REP_3_6, TreeKind::BitLength);
            send_bits(state, count - 3, 2);
        } else if count <= 10 {
            send_code_dyn(state, REPZ_3_10, TreeKind::BitLength);
            send_bits(state, count - 3, 3);
        } else {
            send_code_dyn(state, REPZ_11_138, TreeKind::BitLength);
            send_bits(state, count - 11, 7);
        }
        count = 0;
        prevlen = curlen as i32;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Build the bit-length tree from the literal and distance trees.
///
/// Returns `max_blindex` — the last non-zero bit-length code index
/// in `BL_ORDER`, used to determine how many codes to transmit.
fn build_bl_tree(state: &mut DeflateState) -> usize {
    let l_max = get_max_code(state, TreeKind::Literal) as usize;
    let d_max = get_max_code(state, TreeKind::Distance) as usize;

    scan_tree(state, TreeKind::Literal, l_max);
    scan_tree(state, TreeKind::Distance, d_max);
    build_tree(state, TreeKind::BitLength, &STATIC_BL_DESC);

    let mut max_blindex = BL_CODES - 1;
    while max_blindex >= 3 {
        if state.bl_tree[BL_ORDER[max_blindex] as usize].len != 0 {
            break;
        }
        max_blindex -= 1;
    }
    state.opt_len = state.opt_len.wrapping_add(
        3u64.wrapping_mul((max_blindex as u64) + 1)
            .wrapping_add(5 + 5 + 4),
    );
    max_blindex
}

/// Send the header for a dynamic Huffman block.
///
/// Ported from `trees.c` lines 833–855.
fn send_all_trees(state: &mut DeflateState, lcodes: usize, dcodes: usize, blcodes: usize) {
    send_bits(state, (lcodes - 257) as u32, 5);
    send_bits(state, (dcodes - 1) as u32, 5);
    send_bits(state, (blcodes - 4) as u32, 4);
    for rank in 0..blcodes {
        send_bits(state, state.bl_tree[BL_ORDER[rank] as usize].len as u32, 3);
    }
    let l_max = get_max_code(state, TreeKind::Literal) as usize;
    let d_max = get_max_code(state, TreeKind::Distance) as usize;
    send_tree(state, TreeKind::Literal, l_max);
    send_tree(state, TreeKind::Distance, d_max);
}

/// Encode the current block's symbols through the given trees.
///
/// When `use_static` is `true`, symbols are encoded through the
/// pre-computed `STATIC_LTREE` / `STATIC_DTREE` const tables.
/// Otherwise the per-block dynamic trees in `DeflateState` are used.
///
/// Ported from `trees.c` lines 900–951.
fn compress_block(state: &mut DeflateState, use_static: bool) {
    let sym_end = state.sym_next;
    let mut sx: usize = 0;

    if sym_end != 0 {
        while sx < sym_end {
            // Each symbol is stored as 3 bytes: dist_lo, dist_hi, lc.
            let dist_lo = state.sym_buf[sx] as u32;
            let dist_hi = state.sym_buf[sx + 1] as u32;
            let lc = state.sym_buf[sx + 2] as u32;
            sx += 3;
            let dist = dist_lo | (dist_hi << 8);

            if dist == 0 {
                // Literal byte.
                if use_static {
                    send_code_static(state, lc as usize, &STATIC_LTREE);
                } else {
                    send_code_dyn(state, lc as usize, TreeKind::Literal);
                }
            } else {
                // Length/distance pair.
                let lc_idx = lc as usize;
                let code = LENGTH_CODE[lc_idx] as usize;
                let l_code = code + LITERALS + 1;
                if use_static {
                    send_code_static(state, l_code, &STATIC_LTREE);
                } else {
                    send_code_dyn(state, l_code, TreeKind::Literal);
                }
                let extra = EXTRA_LBITS[code] as u32;
                if extra != 0 {
                    send_bits(state, lc - BASE_LENGTH[code], extra);
                }
                let dist_adjusted = dist - 1;
                let dcode = d_code(dist_adjusted) as usize;
                if use_static {
                    send_code_static(state, dcode, &STATIC_DTREE);
                } else {
                    send_code_dyn(state, dcode, TreeKind::Distance);
                }
                let dextra = EXTRA_DBITS[dcode] as u32;
                if dextra != 0 {
                    send_bits(state, dist_adjusted - BASE_DIST[dcode], dextra);
                }
            }
        }
    }

    // Send end-of-block code.
    if use_static {
        send_code_static(state, END_BLOCK, &STATIC_LTREE);
    } else {
        send_code_dyn(state, END_BLOCK, TreeKind::Literal);
    }
}

/// Classify the block data as binary or text by analysing literal frequencies.
///
/// Ported from `trees.c` lines 966–991.
fn detect_data_type(state: &DeflateState) -> i32 {
    // Bit-mask of control characters that indicate binary data.
    let mut block_mask: u64 = 0xf3ff_c07f;
    for n in 0..32usize {
        if (block_mask & 1) != 0 && state.dyn_ltree[n].freq != 0 {
            return Z_BINARY;
        }
        block_mask >>= 1;
    }
    // Whitespace characters — if present, data could be text.
    if state.dyn_ltree[9].freq != 0
        || state.dyn_ltree[10].freq != 0
        || state.dyn_ltree[13].freq != 0
    {
        return Z_TEXT;
    }
    for n in 32..LITERALS {
        if state.dyn_ltree[n].freq != 0 {
            return Z_TEXT;
        }
    }
    Z_BINARY
}

// =============================================================================
//  Public interface functions
// =============================================================================

/// Emit a stored (uncompressed) block.
///
/// Ported from `trees.c` lines 860–875 (`_tr_stored_block`).
pub(crate) fn tr_stored_block(
    state: &mut DeflateState,
    buf: Option<&[u8]>,
    stored_len: u64,
    last: bool,
) {
    let header = (STORED_BLOCK << 1) as u32 | u32::from(last);
    send_bits(state, header, 3);
    bi_windup(state);
    let len = stored_len as u16;
    put_short(state, len);
    put_short(state, !len);
    if let Some(data) = buf {
        let copy_len = stored_len as usize;
        if copy_len > 0 {
            let dst_start = state.pending;
            let dst_end = dst_start + copy_len;
            if dst_end <= state.pending_buf.len() && copy_len <= data.len() {
                state.pending_buf[dst_start..dst_end].copy_from_slice(&data[..copy_len]);
                state.pending += copy_len;
            }
        }
    }
}

/// Flush the bit buffer (convenience wrapper over [`bi_flush`]).
pub(crate) fn tr_flush_bits(state: &mut DeflateState) {
    bi_flush(state);
}

/// Emit a static-trees block containing only the end-of-block code.
///
/// Ported from `trees.c` lines 888–895 (`_tr_align`).
pub(crate) fn tr_align(state: &mut DeflateState) {
    let header = (STATIC_TREES << 1) as u32;
    send_bits(state, header, 3);
    send_code_static(state, END_BLOCK, &STATIC_LTREE);
    bi_flush(state);
}

/// Determine the best encoding for the current block and emit it.
///
/// This is the main block encoding decision function.
/// Ported from `trees.c` lines 997–1089 (`_tr_flush_block`).
pub(crate) fn tr_flush_block(
    state: &mut DeflateState,
    buf: Option<&[u8]>,
    stored_len: u64,
    last: bool,
) {
    let opt_lenb: u64;
    let static_lenb: u64;
    let mut max_blindex: usize = 0;

    if state.level > 0 {
        // Auto-detect data type if not yet determined (matches C's
        // `if (s->strm->data_type == Z_UNKNOWN) s->strm->data_type = detect_data_type(s);`
        // in `_tr_flush_block`, `trees.c` line 1010). The result is stored
        // on `DeflateState.data_type` and propagated to `stream.data_type`
        // by the caller (`flush_block_only` in `algorithm.rs`).
        if state.wrap != 2 && state.data_type == Z_UNKNOWN {
            state.data_type = detect_data_type(state);
        }

        build_tree(state, TreeKind::Literal, &STATIC_L_DESC);
        build_tree(state, TreeKind::Distance, &STATIC_D_DESC);
        max_blindex = build_bl_tree(state);

        opt_lenb = (state.opt_len + 3 + 7) >> 3;
        static_lenb = (state.static_len + 3 + 7) >> 3;
    } else {
        // level == 0 ⇒ force stored block.
        let padded = stored_len + 5;
        opt_lenb = padded;
        static_lenb = padded;
    }

    if stored_len + 4 <= opt_lenb && buf.is_some() {
        tr_stored_block(state, buf, stored_len, last);
    } else if static_lenb <= opt_lenb || state.strategy == Z_FIXED {
        let header = ((STATIC_TREES << 1) | i32::from(last)) as u32;
        send_bits(state, header, 3);
        compress_block(state, true);
    } else {
        let header = ((DYN_TREES << 1) | i32::from(last)) as u32;
        send_bits(state, header, 3);
        let l_codes_count = get_max_code(state, TreeKind::Literal) as usize + 1;
        let d_codes_count = get_max_code(state, TreeKind::Distance) as usize + 1;
        send_all_trees(state, l_codes_count, d_codes_count, max_blindex + 1);
        compress_block(state, false);
    }

    init_block(state);
    if last {
        bi_windup(state);
    }
}

/// Record a literal or length/distance pair and return `true` when the
/// symbol buffer is full.
///
/// Ported from `trees.c` lines 1095–1119 (`_tr_tally`).
pub(crate) fn tr_tally(state: &mut DeflateState, dist: u32, lc: u32) -> bool {
    let sx = state.sym_next;
    if sx + 2 < state.sym_buf.len() {
        state.sym_buf[sx] = dist as u8;
        state.sym_buf[sx + 1] = (dist >> 8) as u8;
        state.sym_buf[sx + 2] = lc as u8;
        state.sym_next += 3;
    }

    if dist == 0 {
        // Literal byte.
        let idx = lc as usize;
        if idx < state.dyn_ltree.len() {
            state.dyn_ltree[idx].freq = state.dyn_ltree[idx].freq.wrapping_add(1);
        }
    } else {
        // Length/distance pair.
        state.matches += 1;
        let d = dist - 1;
        let lc_idx = lc as usize;
        if lc_idx < LENGTH_CODE.len() {
            let l_code = LENGTH_CODE[lc_idx] as usize + LITERALS + 1;
            if l_code < state.dyn_ltree.len() {
                state.dyn_ltree[l_code].freq = state.dyn_ltree[l_code].freq.wrapping_add(1);
            }
        }
        let dc = d_code(d) as usize;
        if dc < state.dyn_dtree.len() {
            state.dyn_dtree[dc].freq = state.dyn_dtree[dc].freq.wrapping_add(1);
        }
    }
    state.sym_next == state.sym_end
}

/// Convenience wrapper: tally a single literal byte.
#[inline]
pub(crate) fn tr_tally_lit(state: &mut DeflateState, c: u8) -> bool {
    tr_tally(state, 0, u32::from(c))
}

/// Convenience wrapper: tally a length/distance match.
#[inline]
pub(crate) fn tr_tally_dist(state: &mut DeflateState, dist: u32, len: u32) -> bool {
    tr_tally(state, dist, len - MIN_MATCH as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_data_default() {
        let ct = CtData::default();
        assert_eq!(ct.freq(), 0);
        assert_eq!(ct.code(), 0);
        assert_eq!(ct.dad(), 0);
        assert_eq!(ct.len(), 0);
    }

    #[test]
    fn ct_data_new_and_accessors() {
        let ct = CtData::new(42, 8);
        assert_eq!(ct.freq(), 42);
        assert_eq!(ct.code(), 42);
        assert_eq!(ct.dad(), 8);
        assert_eq!(ct.len(), 8);
    }

    #[test]
    fn ct_data_set_accessors() {
        let mut ct = CtData::default();
        ct.set_freq(100);
        assert_eq!(ct.freq(), 100);
        ct.set_code(200);
        assert_eq!(ct.freq(), 200);
        ct.set_dad(50);
        assert_eq!(ct.dad(), 50);
        ct.set_len(99);
        assert_eq!(ct.len(), 99);
    }

    #[test]
    fn static_ltree_size() {
        assert_eq!(STATIC_LTREE.len(), L_CODES + 2);
    }

    #[test]
    fn static_ltree_first_entry() {
        assert_eq!(STATIC_LTREE[0].code(), 12);
        assert_eq!(STATIC_LTREE[0].len(), 8);
    }

    #[test]
    fn static_ltree_code_144_is_9bit() {
        assert_eq!(STATIC_LTREE[144].len(), 9);
    }

    #[test]
    fn static_ltree_code_256_end_block() {
        assert_eq!(STATIC_LTREE[256].code(), 0);
        assert_eq!(STATIC_LTREE[256].len(), 7);
    }

    #[test]
    fn static_ltree_code_280_is_8bit() {
        assert_eq!(STATIC_LTREE[280].len(), 8);
    }

    #[test]
    fn static_dtree_size_and_all_5bit() {
        assert_eq!(STATIC_DTREE.len(), D_CODES);
        for (i, entry) in STATIC_DTREE.iter().enumerate() {
            assert_eq!(entry.len(), 5, "STATIC_DTREE[{i}] should be 5-bit");
        }
    }

    #[test]
    fn dist_code_table_size() {
        assert_eq!(DIST_CODE.len(), 512);
    }

    #[test]
    fn dist_code_first_entries() {
        assert_eq!(DIST_CODE[0], 0);
        assert_eq!(DIST_CODE[1], 1);
        assert_eq!(DIST_CODE[2], 2);
        assert_eq!(DIST_CODE[3], 3);
    }

    #[test]
    fn length_code_table_size() {
        assert_eq!(LENGTH_CODE.len(), MAX_MATCH - MIN_MATCH + 1);
    }

    #[test]
    fn base_length_values() {
        assert_eq!(BASE_LENGTH.len(), LENGTH_CODES);
        for i in 0..8 {
            assert_eq!(BASE_LENGTH[i], i as u32);
        }
        assert_eq!(BASE_LENGTH[8], 8);
        assert_eq!(BASE_LENGTH[9], 10);
    }

    #[test]
    fn base_dist_values() {
        assert_eq!(BASE_DIST.len(), D_CODES);
        assert_eq!(BASE_DIST[0], 0);
        assert_eq!(BASE_DIST[29], 24576);
    }

    #[test]
    fn extra_bits_sizes() {
        assert_eq!(EXTRA_LBITS.len(), LENGTH_CODES);
        assert_eq!(EXTRA_DBITS.len(), D_CODES);
    }

    #[test]
    fn static_descriptors() {
        assert!(STATIC_L_DESC.static_tree.is_some());
        assert_eq!(STATIC_L_DESC.elems, L_CODES);
        assert_eq!(STATIC_L_DESC.max_length, MAX_BITS);

        assert!(STATIC_D_DESC.static_tree.is_some());
        assert_eq!(STATIC_D_DESC.elems, D_CODES);

        assert!(STATIC_BL_DESC.static_tree.is_none());
        assert_eq!(STATIC_BL_DESC.elems, BL_CODES);
        assert_eq!(STATIC_BL_DESC.max_length, MAX_BL_BITS);
    }

    #[test]
    fn d_code_small() {
        assert_eq!(d_code(0), 0);
        assert_eq!(d_code(1), 1);
        assert_eq!(d_code(2), 2);
        assert_eq!(d_code(3), 3);
    }

    #[test]
    fn d_code_large() {
        // For dist >= 256: use DIST_CODE[256 + (dist >> 7)]
        assert_eq!(d_code(256), DIST_CODE[256 + (256 >> 7)] as u32);
    }

    #[test]
    fn bi_reverse_works() {
        // Reverse 3 bits of 0b101 = 0b101 reversed = 0b101
        assert_eq!(bi_reverse(0b101, 3), 0b101);
        // Reverse 4 bits of 0b1010 = 0b0101
        assert_eq!(bi_reverse(0b1010, 4), 0b0101);
        // Reverse 1 bit
        assert_eq!(bi_reverse(1, 1), 1);
        assert_eq!(bi_reverse(0, 1), 0);
    }
}
