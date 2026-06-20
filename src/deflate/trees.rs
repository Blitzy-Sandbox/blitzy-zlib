//! Huffman tree construction, static tables, bit I/O, and DEFLATE block
//! emission — the encoder-side half of the DEFLATE engine.
//!
//! This module is a faithful, **100% safe-Rust** port of:
//!
//! * **`trees.c`** — all tree-building (`build_tree`, `gen_bitlen`,
//!   `gen_codes`, `pqdownheap`), bit-output (`send_bits`, `bi_flush`,
//!   `bi_windup`, `bi_reverse`), and block-emission (`_tr_flush_block`,
//!   `_tr_stored_block`, `_tr_align`, `compress_block`, `_tr_tally`) routines.
//! * **`trees.h`** — the pre-generated static Huffman tables
//!   (`static_ltree`, `static_dtree`, `_dist_code`, `_length_code`,
//!   `base_length`, `base_dist`).
//!
//! # Byte-identical output
//!
//! Every table value, loop bound, comparison operator, and bit-packing step is
//! ported **mechanically** from C zlib. The Huffman code construction
//! (frequency counting, canonical code assignment with tie-breaking by subtree
//! depth) and the block-type selection (stored vs. static vs. dynamic) directly
//! determine the emitted bitstream, so nothing here is reordered or
//! "optimized". The static tables are transcribed verbatim from `trees.h`
//! (which *is* the C-generated output) and are additionally self-checked
//! against the `tr_static_init` generation algorithm in the test module below
//! (AAP §0.6.5, §0.6.6, §0.6.7, §0.7.1).
//!
//! # No `unsafe`, `no_std`-clean
//!
//! The entire module is safe Rust (`unsafe` lives only in `src/ffi.rs` and
//! `src/inflate/fast.rs`, per AAP §0.6.2) and uses only `core`/`alloc`
//! facilities — never `std`. The borrow-checker friction that arises because
//! several routines need a Huffman-tree array *and* `&mut DeflateState`
//! simultaneously is resolved by reading one [`HuffmanNode`] at a time (the
//! type is `Copy`) and by passing a [`TreeKind`] selector so each routine
//! indexes the correct `DeflateState` array internally — never with `unsafe`
//! and never with interior mutability.
//!
//! # Relationship with `state.rs`
//!
//! `state.rs` stores the three dynamic trees (`dyn_ltree`, `dyn_dtree`,
//! `bl_tree`), the build heap, and the bit accumulator inline on
//! [`DeflateState`]; it imports [`HuffmanNode`] from this module, while this
//! module imports [`DeflateState`] from it. That intra-crate cycle is fine
//! because the crate compiles as a single unit.

use crate::constants::{Strategy, Z_BINARY, Z_TEXT, Z_UNKNOWN};
use crate::deflate::state::{
    BL_CODES, BUF_SIZE, D_CODES, DeflateState, END_BLOCK, HEAP_SIZE, L_CODES, LENGTH_CODES,
    LITERALS, MAX_BITS, MAX_BL_BITS, MAX_MATCH, MIN_MATCH,
};

// ===========================================================================
// Phase A — HuffmanNode (replaces the C `ct_data` union)
//
// The C `ct_data` overlays two 16-bit fields:
//   union { ush freq; ush code; } fc;   // Freq during build, Code afterwards
//   union { ush dad;  ush len;  } dl;   // Dad  during build, Len  afterwards
// Rust has no C-style unions of equal-width scalars worth the `unsafe`, so we
// model the overlay explicitly with two `u16` slots and accessor pairs that
// document which "view" is live at each stage. `#[repr(C)]` guarantees the
// 4-byte, field-ordered layout required at the FFI boundary (AAP §0.6.5).
// ===========================================================================

/// A single node of a Huffman tree, mirroring the C `ct_data` union.
///
/// The two `u16` slots are each used for two different purposes at different
/// stages of tree construction, exactly as the C union is:
///
/// * `freq_or_code` holds the symbol **frequency** while the tree is being
///   built ([`build_tree`]) and the canonical **code** afterwards
///   ([`gen_codes`]).
/// * `dad_or_len` holds the **parent** (`dad`) index during the heap/bit-length
///   passes and the final code **length** afterwards.
///
/// Use the named accessors rather than the raw fields to make the live view
/// explicit at each call site.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct HuffmanNode {
    /// `fc` union slot: frequency during build, code after [`gen_codes`].
    pub freq_or_code: u16,
    /// `dl` union slot: parent (`dad`) during build, length after
    /// [`gen_bitlen`].
    pub dad_or_len: u16,
}

// `len()` here returns the Huffman code *bit length* (the C `ct_data` `Len`
// union view), not a collection length, so the `is_empty` companion that
// `clippy::len_without_is_empty` expects is semantically meaningless. The
// accessor name is fixed by the C source / the file's API contract.
#[allow(clippy::len_without_is_empty)]
impl HuffmanNode {
    /// Read the `Freq` view of the `fc` slot.
    #[inline]
    pub fn freq(&self) -> u16 {
        self.freq_or_code
    }

    /// Write the `Freq` view of the `fc` slot.
    #[inline]
    pub fn set_freq(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Read the `Code` view of the `fc` slot.
    #[inline]
    pub fn code(&self) -> u16 {
        self.freq_or_code
    }

    /// Write the `Code` view of the `fc` slot.
    #[inline]
    pub fn set_code(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Read the `Dad` view of the `dl` slot.
    #[inline]
    pub fn dad(&self) -> u16 {
        self.dad_or_len
    }

    /// Write the `Dad` view of the `dl` slot.
    #[inline]
    pub fn set_dad(&mut self, v: u16) {
        self.dad_or_len = v;
    }

    /// Read the `Len` view of the `dl` slot.
    #[inline]
    pub fn len(&self) -> u16 {
        self.dad_or_len
    }

    /// Write the `Len` view of the `dl` slot.
    #[inline]
    pub fn set_len(&mut self, v: u16) {
        self.dad_or_len = v;
    }
}

/// `const` initializer for the static tables: `node(code, len)` mirrors the
/// `{{code},{len}}` pairs emitted into `trees.h`.
#[inline]
const fn node(fc: u16, dl: u16) -> HuffmanNode {
    HuffmanNode {
        freq_or_code: fc,
        dad_or_len: dl,
    }
}

// ===========================================================================
// Phase C — Top-of-file constants (trees.c L47-72)
// ===========================================================================

/// Extra bits for each length code (`extra_lbits`, trees.c L62-63).
pub(crate) const EXTRA_LBITS: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each distance code (`extra_dbits`, trees.c L65-66).
pub(crate) const EXTRA_DBITS: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits for each bit-length code (`extra_blbits`, trees.c L68-69).
pub(crate) const EXTRA_BLBITS: [i32; BL_CODES] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Order in which bit-length-code lengths are transmitted (`bl_order`,
/// trees.c L71-72). They are sent in order of decreasing probability so the
/// lengths for unused bit-length codes can be omitted.
pub(crate) const BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Block-type tag for a stored (uncompressed) block (`STORED_BLOCK`).
pub(crate) const STORED_BLOCK: i32 = 0;
/// Block-type tag for a static-Huffman block (`STATIC_TREES`).
pub(crate) const STATIC_TREES: i32 = 1;
/// Block-type tag for a dynamic-Huffman block (`DYN_TREES`).
pub(crate) const DYN_TREES: i32 = 2;

/// "Repeat previous bit length 3-6 times" bit-length-code symbol
/// (`REP_3_6`, trees.c L53).
pub(crate) const REP_3_6: usize = 16;
/// "Repeat a zero length 3-10 times" bit-length-code symbol
/// (`REPZ_3_10`, trees.c L56).
pub(crate) const REPZ_3_10: usize = 17;
/// "Repeat a zero length 11-138 times" bit-length-code symbol
/// (`REPZ_11_138`, trees.c L59).
pub(crate) const REPZ_11_138: usize = 18;

/// Index within the heap of the least-frequent node (`SMALLEST`, trees.c
/// L480). `heap[0]` is unused, so the heap is 1-based.
pub(crate) const SMALLEST: usize = 1;

// ===========================================================================
// Phase B — Static Huffman tables (transcribed verbatim from trees.h)
//
// These are the C-generated (`-DGEN_TREES_H`) tables. They are byte-exact and
// load-bearing for the output bitstream; the `tests` module reproduces the
// `tr_static_init` generation algorithm and asserts these consts match it.
// ===========================================================================

/// Static literal/length tree (`static_ltree`, trees.h L3-62). Indices 0-143
/// use 8-bit codes, 144-255 use 9-bit, 256-279 use 7-bit, 280-287 use 8-bit;
/// codes 286/287 exist only to complete the canonical tree.
pub(crate) const STATIC_LTREE: [HuffmanNode; L_CODES + 2] = [
    node(12, 8),
    node(140, 8),
    node(76, 8),
    node(204, 8),
    node(44, 8),
    node(172, 8),
    node(108, 8),
    node(236, 8),
    node(28, 8),
    node(156, 8),
    node(92, 8),
    node(220, 8),
    node(60, 8),
    node(188, 8),
    node(124, 8),
    node(252, 8),
    node(2, 8),
    node(130, 8),
    node(66, 8),
    node(194, 8),
    node(34, 8),
    node(162, 8),
    node(98, 8),
    node(226, 8),
    node(18, 8),
    node(146, 8),
    node(82, 8),
    node(210, 8),
    node(50, 8),
    node(178, 8),
    node(114, 8),
    node(242, 8),
    node(10, 8),
    node(138, 8),
    node(74, 8),
    node(202, 8),
    node(42, 8),
    node(170, 8),
    node(106, 8),
    node(234, 8),
    node(26, 8),
    node(154, 8),
    node(90, 8),
    node(218, 8),
    node(58, 8),
    node(186, 8),
    node(122, 8),
    node(250, 8),
    node(6, 8),
    node(134, 8),
    node(70, 8),
    node(198, 8),
    node(38, 8),
    node(166, 8),
    node(102, 8),
    node(230, 8),
    node(22, 8),
    node(150, 8),
    node(86, 8),
    node(214, 8),
    node(54, 8),
    node(182, 8),
    node(118, 8),
    node(246, 8),
    node(14, 8),
    node(142, 8),
    node(78, 8),
    node(206, 8),
    node(46, 8),
    node(174, 8),
    node(110, 8),
    node(238, 8),
    node(30, 8),
    node(158, 8),
    node(94, 8),
    node(222, 8),
    node(62, 8),
    node(190, 8),
    node(126, 8),
    node(254, 8),
    node(1, 8),
    node(129, 8),
    node(65, 8),
    node(193, 8),
    node(33, 8),
    node(161, 8),
    node(97, 8),
    node(225, 8),
    node(17, 8),
    node(145, 8),
    node(81, 8),
    node(209, 8),
    node(49, 8),
    node(177, 8),
    node(113, 8),
    node(241, 8),
    node(9, 8),
    node(137, 8),
    node(73, 8),
    node(201, 8),
    node(41, 8),
    node(169, 8),
    node(105, 8),
    node(233, 8),
    node(25, 8),
    node(153, 8),
    node(89, 8),
    node(217, 8),
    node(57, 8),
    node(185, 8),
    node(121, 8),
    node(249, 8),
    node(5, 8),
    node(133, 8),
    node(69, 8),
    node(197, 8),
    node(37, 8),
    node(165, 8),
    node(101, 8),
    node(229, 8),
    node(21, 8),
    node(149, 8),
    node(85, 8),
    node(213, 8),
    node(53, 8),
    node(181, 8),
    node(117, 8),
    node(245, 8),
    node(13, 8),
    node(141, 8),
    node(77, 8),
    node(205, 8),
    node(45, 8),
    node(173, 8),
    node(109, 8),
    node(237, 8),
    node(29, 8),
    node(157, 8),
    node(93, 8),
    node(221, 8),
    node(61, 8),
    node(189, 8),
    node(125, 8),
    node(253, 8),
    node(19, 9),
    node(275, 9),
    node(147, 9),
    node(403, 9),
    node(83, 9),
    node(339, 9),
    node(211, 9),
    node(467, 9),
    node(51, 9),
    node(307, 9),
    node(179, 9),
    node(435, 9),
    node(115, 9),
    node(371, 9),
    node(243, 9),
    node(499, 9),
    node(11, 9),
    node(267, 9),
    node(139, 9),
    node(395, 9),
    node(75, 9),
    node(331, 9),
    node(203, 9),
    node(459, 9),
    node(43, 9),
    node(299, 9),
    node(171, 9),
    node(427, 9),
    node(107, 9),
    node(363, 9),
    node(235, 9),
    node(491, 9),
    node(27, 9),
    node(283, 9),
    node(155, 9),
    node(411, 9),
    node(91, 9),
    node(347, 9),
    node(219, 9),
    node(475, 9),
    node(59, 9),
    node(315, 9),
    node(187, 9),
    node(443, 9),
    node(123, 9),
    node(379, 9),
    node(251, 9),
    node(507, 9),
    node(7, 9),
    node(263, 9),
    node(135, 9),
    node(391, 9),
    node(71, 9),
    node(327, 9),
    node(199, 9),
    node(455, 9),
    node(39, 9),
    node(295, 9),
    node(167, 9),
    node(423, 9),
    node(103, 9),
    node(359, 9),
    node(231, 9),
    node(487, 9),
    node(23, 9),
    node(279, 9),
    node(151, 9),
    node(407, 9),
    node(87, 9),
    node(343, 9),
    node(215, 9),
    node(471, 9),
    node(55, 9),
    node(311, 9),
    node(183, 9),
    node(439, 9),
    node(119, 9),
    node(375, 9),
    node(247, 9),
    node(503, 9),
    node(15, 9),
    node(271, 9),
    node(143, 9),
    node(399, 9),
    node(79, 9),
    node(335, 9),
    node(207, 9),
    node(463, 9),
    node(47, 9),
    node(303, 9),
    node(175, 9),
    node(431, 9),
    node(111, 9),
    node(367, 9),
    node(239, 9),
    node(495, 9),
    node(31, 9),
    node(287, 9),
    node(159, 9),
    node(415, 9),
    node(95, 9),
    node(351, 9),
    node(223, 9),
    node(479, 9),
    node(63, 9),
    node(319, 9),
    node(191, 9),
    node(447, 9),
    node(127, 9),
    node(383, 9),
    node(255, 9),
    node(511, 9),
    node(0, 7),
    node(64, 7),
    node(32, 7),
    node(96, 7),
    node(16, 7),
    node(80, 7),
    node(48, 7),
    node(112, 7),
    node(8, 7),
    node(72, 7),
    node(40, 7),
    node(104, 7),
    node(24, 7),
    node(88, 7),
    node(56, 7),
    node(120, 7),
    node(4, 7),
    node(68, 7),
    node(36, 7),
    node(100, 7),
    node(20, 7),
    node(84, 7),
    node(52, 7),
    node(116, 7),
    node(3, 8),
    node(131, 8),
    node(67, 8),
    node(195, 8),
    node(35, 8),
    node(163, 8),
    node(99, 8),
    node(227, 8),
];

/// Static distance tree (`static_dtree`, trees.h L64-71): every code is 5 bits,
/// `Code == bi_reverse(n, 5)`.
pub(crate) const STATIC_DTREE: [HuffmanNode; D_CODES] = [
    node(0, 5),
    node(16, 5),
    node(8, 5),
    node(24, 5),
    node(4, 5),
    node(20, 5),
    node(12, 5),
    node(28, 5),
    node(2, 5),
    node(18, 5),
    node(10, 5),
    node(26, 5),
    node(6, 5),
    node(22, 5),
    node(14, 5),
    node(30, 5),
    node(1, 5),
    node(17, 5),
    node(9, 5),
    node(25, 5),
    node(5, 5),
    node(21, 5),
    node(13, 5),
    node(29, 5),
    node(3, 5),
    node(19, 5),
    node(11, 5),
    node(27, 5),
    node(7, 5),
    node(23, 5),
];

/// Distance-code lookup (`_dist_code`, trees.h L73-100). The first 256 entries
/// map distances 1..256; the last 256 map the top bits of larger distances
/// (indexed via `256 + (dist >> 7)`). See [`d_code`].
pub(crate) const DIST_CODE: [u8; 512] = [
    0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11,
    11, 11, 11, 11, 11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 13, 13, 13, 13, 13, 13, 13, 13,
    13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 0, 0, 16, 17, 18, 18, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21,
    22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
];

/// Length-code lookup (`_length_code`, trees.h L102-116) for each normalized
/// match length `0 == MIN_MATCH`. `LENGTH_CODE[255]` is `28` (the match length
/// 258 special case, set by `tr_static_init` after the table loop).
pub(crate) const LENGTH_CODE: [u8; MAX_MATCH - MIN_MATCH + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14,
    14, 15, 15, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 18, 18, 18,
    18, 18, 18, 18, 18, 19, 19, 19, 19, 19, 19, 19, 19, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20,
    20, 20, 20, 20, 20, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23, 23,
    23, 23, 23, 23, 23, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 28,
];

/// First normalized length for each length code (`base_length`, trees.h
/// L118-121); the final entry is `0` (code 28 has no base offset).
pub(crate) const BASE_LENGTH: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224, 0,
];

/// First normalized distance for each distance code (`base_dist`, trees.h
/// L123-127).
pub(crate) const BASE_DIST: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

// ===========================================================================
// Phase D — Static tree descriptors + TreeKind
//
// The C `static_tree_desc` couples a static tree (or NULL for the bit-length
// tree) with its extra-bit table, base index, element count, and max code
// length. Because the *dynamic* trees live inline on `DeflateState` and the
// per-tree `max_code` is a plain field, the build routines take a `TreeKind`
// selector instead of a mutable `tree_desc` pointer.
// ===========================================================================

/// Immutable description of one of the three Huffman trees, mirroring the C
/// `static_tree_desc_s` struct.
pub(crate) struct StaticTreeDesc {
    /// The static tree, or `None` for the bit-length tree (C `NULL`).
    pub static_tree: Option<&'static [HuffmanNode]>,
    /// Extra bits for each code.
    pub extra_bits: &'static [i32],
    /// Base index into `extra_bits` (the first code that carries extra bits).
    pub extra_base: usize,
    /// Maximum number of elements in the tree.
    pub elems: usize,
    /// Maximum bit length for the codes.
    pub max_length: usize,
}

/// Descriptor for the literal/length tree (`static_l_desc`, trees.c L131-132).
pub(crate) const STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_LTREE),
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the distance tree (`static_d_desc`, trees.c L134-135).
pub(crate) const STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_DTREE),
    extra_bits: &EXTRA_DBITS,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the bit-length tree (`static_bl_desc`, trees.c L137-138).
pub(crate) const STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_BLBITS,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

/// Selects which of the three Huffman trees a build/scan/send routine operates
/// on, replacing the C mutable `tree_desc *` argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TreeKind {
    /// The literal/length tree (`dyn_ltree` + `static_l_desc`).
    L,
    /// The distance tree (`dyn_dtree` + `static_d_desc`).
    D,
    /// The bit-length tree (`bl_tree` + `static_bl_desc`).
    Bl,
}

/// Return the static descriptor for the given tree kind.
#[inline]
fn static_desc(kind: TreeKind) -> &'static StaticTreeDesc {
    match kind {
        TreeKind::L => &STATIC_L_DESC,
        TreeKind::D => &STATIC_D_DESC,
        TreeKind::Bl => &STATIC_BL_DESC,
    }
}

/// Copy out the [`HuffmanNode`] at index `i` of the selected dynamic tree.
///
/// `HuffmanNode` is `Copy`, so this releases the borrow on `s` immediately,
/// which is what lets the build routines read a node and then mutate `s`
/// (e.g. call [`send_bits`]) without aliasing or `unsafe`.
#[inline]
fn tree_node(s: &DeflateState, kind: TreeKind, i: usize) -> HuffmanNode {
    match kind {
        TreeKind::L => s.dyn_ltree[i],
        TreeKind::D => s.dyn_dtree[i],
        TreeKind::Bl => s.bl_tree[i],
    }
}

/// Set the `Len` slot of node `i` in the selected dynamic tree.
#[inline]
fn set_len(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::L => s.dyn_ltree[i].set_len(v),
        TreeKind::D => s.dyn_dtree[i].set_len(v),
        TreeKind::Bl => s.bl_tree[i].set_len(v),
    }
}

/// Set the `Freq` slot of node `i` in the selected dynamic tree.
#[inline]
fn set_freq(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::L => s.dyn_ltree[i].set_freq(v),
        TreeKind::D => s.dyn_dtree[i].set_freq(v),
        TreeKind::Bl => s.bl_tree[i].set_freq(v),
    }
}

/// Set the `Dad` slot of node `i` in the selected dynamic tree.
#[inline]
fn set_dad(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::L => s.dyn_ltree[i].set_dad(v),
        TreeKind::D => s.dyn_dtree[i].set_dad(v),
        TreeKind::Bl => s.bl_tree[i].set_dad(v),
    }
}

/// Read the per-tree `max_code` field for the selected tree.
#[inline]
fn max_code_of(s: &DeflateState, kind: TreeKind) -> i32 {
    match kind {
        TreeKind::L => s.l_desc_max_code,
        TreeKind::D => s.d_desc_max_code,
        TreeKind::Bl => s.bl_desc_max_code,
    }
}

/// Write the per-tree `max_code` field for the selected tree.
#[inline]
fn set_max_code(s: &mut DeflateState, kind: TreeKind, v: i32) {
    match kind {
        TreeKind::L => s.l_desc_max_code = v,
        TreeKind::D => s.d_desc_max_code = v,
        TreeKind::Bl => s.bl_desc_max_code = v,
    }
}

// ===========================================================================
// Phase E — d_code + tr_static_init
// ===========================================================================

/// Map a match distance to its distance code (`d_code` macro, deflate.h).
///
/// Distances below 256 index `_dist_code` directly; larger distances index via
/// the top bits (`256 + (dist >> 7)`).
#[inline]
pub(crate) fn d_code(dist: usize) -> usize {
    if dist < 256 {
        DIST_CODE[dist] as usize
    } else {
        DIST_CODE[256 + (dist >> 7)] as usize
    }
}

/// Initialize the constant tables (`tr_static_init`, trees.c L295-374).
///
/// In C this lazily fills the static trees and the length/distance lookups on
/// first use. Here every one of those tables is a compile-time `const`
/// (transcribed verbatim from `trees.h`), so there is nothing to initialize and
/// this is a no-op retained only to mirror the C call sequence from
/// [`tr_init`]. The generation algorithm itself is exercised in the test
/// module, which asserts the consts equal what `tr_static_init` would produce.
#[inline]
pub(crate) fn tr_static_init() {}

// ===========================================================================
// Phase F — Bit accumulator I/O (trees.c L137-205, L1098-1127)
// ===========================================================================

/// Reverse the low `len` bits of `code` (`bi_reverse`, trees.c L154-161).
///
/// IN assertion: `1 <= len <= 15`.
#[inline]
fn bi_reverse(code: u32, len: i32) -> u32 {
    let mut res: u32 = 0;
    let mut code = code;
    let mut len = len;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len <= 0 {
            break;
        }
    }
    res >> 1
}

/// Send `length` bits of `value` (low bits first) to the bit accumulator,
/// flushing a full 16-bit word to `pending_buf` when the accumulator fills
/// (`send_bits`, trees.c L274-286, the non-`ZLIB_DEBUG` macro).
///
/// IN assertion: `length <= 16` and `value` fits in `length` bits. Because the
/// value fits in `length <= 16` bits it is non-negative, so the left/right
/// shifts and the truncating `as u16` casts reproduce the C `(ush)value`
/// arithmetic bit-for-bit (the bits that overflow a `ush` are exactly the bits
/// dropped here).
#[inline]
pub(crate) fn send_bits(s: &mut DeflateState, value: i32, length: i32) {
    if s.bi_valid > BUF_SIZE - length {
        s.bi_buf |= (value << s.bi_valid) as u16;
        s.put_short(s.bi_buf);
        s.bi_buf = (value >> (BUF_SIZE - s.bi_valid)) as u16;
        s.bi_valid += length - BUF_SIZE;
    } else {
        s.bi_buf |= (value << s.bi_valid) as u16;
        s.bi_valid += length;
    }
}

/// Send the code for symbol `c` of `tree` (`send_code` macro, trees.c L239).
///
/// `tree` must be an independent slice (a static `const` table), never a borrow
/// of a `DeflateState` field, so it does not alias the `&mut s` taken by
/// [`send_bits`]. For the dynamic trees, the call sites read a single
/// (`Copy`) [`HuffmanNode`] inline instead.
#[inline]
fn send_code(s: &mut DeflateState, c: usize, tree: &[HuffmanNode]) {
    send_bits(s, tree[c].code() as i32, tree[c].len() as i32);
}

/// Send the bit-length-tree code for symbol `c` (the `dyn`-tree analogue of
/// [`send_code`], reading `s.bl_tree[c]` as a `Copy` value first).
#[inline]
fn send_bl_code(s: &mut DeflateState, c: usize) {
    let bl = s.bl_tree[c];
    send_bits(s, bl.code() as i32, bl.len() as i32);
}

/// Flush the bit accumulator, keeping at most 7 bits in it (`bi_flush`,
/// trees.c L166-176).
pub(crate) fn bi_flush(s: &mut DeflateState) {
    if s.bi_valid == 16 {
        s.put_short(s.bi_buf);
        s.bi_buf = 0;
        s.bi_valid = 0;
    } else if s.bi_valid >= 8 {
        s.put_byte(s.bi_buf as u8);
        s.bi_buf >>= 8;
        s.bi_valid -= 8;
    }
}

/// Flush the bit accumulator and align output on a byte boundary
/// (`bi_windup`, trees.c L181-193). Also records the number of bits used in the
/// final byte (`bi_used`), which the FFI `deflatePending` shim reports.
pub(crate) fn bi_windup(s: &mut DeflateState) {
    if s.bi_valid > 8 {
        s.put_short(s.bi_buf);
    } else if s.bi_valid > 0 {
        s.put_byte(s.bi_buf as u8);
    }
    s.bi_used = ((s.bi_valid - 1) & 7) + 1;
    s.bi_buf = 0;
    s.bi_valid = 0;
}

// ===========================================================================
// Phase G — gen_codes + init_block + tr_init
// ===========================================================================

/// Generate the canonical codes for a tree from its per-length counts
/// (`gen_codes`, trees.c L203-232).
///
/// The running `code` value is accumulated in a `u32` (the C `unsigned`) and
/// truncated to `u16` per length; preserving the `(code + bl_count[bits-1]) <<
/// 1` recurrence and the `next_code[len]++` post-increment is what makes the
/// assigned codes canonical and byte-identical to C.
fn gen_codes(tree: &mut [HuffmanNode], max_code: i32, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }
    for tn in tree.iter_mut().take(max_code as usize + 1) {
        let len = tn.len() as usize;
        if len == 0 {
            continue;
        }
        let nc = next_code[len];
        tn.set_code(bi_reverse(nc as u32, len as i32) as u16);
        next_code[len] = nc + 1;
    }
}

/// Reset the dynamic trees for a new block (`init_block`, trees.c L440-451).
pub(crate) fn init_block(s: &mut DeflateState) {
    for e in &mut s.dyn_ltree[..L_CODES] {
        e.set_freq(0);
    }
    for e in &mut s.dyn_dtree[..D_CODES] {
        e.set_freq(0);
    }
    for e in &mut s.bl_tree[..BL_CODES] {
        e.set_freq(0);
    }
    s.dyn_ltree[END_BLOCK].set_freq(1);
    s.opt_len = 0;
    s.static_len = 0;
    s.sym_next = 0;
    s.matches = 0;
}

/// Initialize the tree data structures for a new stream (`_tr_init`, trees.c
/// L456-478). Called from `deflate_reset` (mod.rs).
pub(crate) fn tr_init(s: &mut DeflateState) {
    tr_static_init();

    // The static descriptors are `const`s selected by `TreeKind`, and the
    // dynamic trees live inline on `DeflateState`, so the only "wiring" left is
    // to reset the per-tree `max_code` fields.
    s.l_desc_max_code = 0;
    s.d_desc_max_code = 0;
    s.bl_desc_max_code = 0;

    s.bi_buf = 0;
    s.bi_valid = 0;
    s.bi_used = 0;

    init_block(s);
}

// ===========================================================================
// Phase H — Dynamic Huffman tree construction
// ===========================================================================

/// Frequency comparison with subtree-depth tie-break (`smaller` macro, trees.c
/// L499-501). Returns `true` if node `n` should sort before node `m`.
#[inline]
fn smaller(s: &DeflateState, kind: TreeKind, n: usize, m: usize) -> bool {
    let fn_ = tree_node(s, kind, n).freq();
    let fm = tree_node(s, kind, m).freq();
    fn_ < fm || (fn_ == fm && s.depth[n] <= s.depth[m])
}

/// Restore the heap property by sifting node `k` down (`pqdownheap`, trees.c
/// L509-528). Operates on `s.heap` / `s.heap_len` and the selected tree.
fn pqdownheap(s: &mut DeflateState, kind: TreeKind, k: usize) {
    let v = s.heap[k];
    let mut k = k;
    let mut j = k << 1; // left son of k
    while j <= s.heap_len {
        // Pick the smaller of the two sons.
        if j < s.heap_len && smaller(s, kind, s.heap[j + 1] as usize, s.heap[j] as usize) {
            j += 1;
        }
        // Stop if v is smaller than both sons.
        if smaller(s, kind, v as usize, s.heap[j] as usize) {
            break;
        }
        // Exchange v with the smaller son and continue down.
        s.heap[k] = s.heap[j];
        k = j;
        j <<= 1;
    }
    s.heap[k] = v;
}

/// Compute optimal bit lengths and update the block bit-length totals
/// (`gen_bitlen`, trees.c L540-613).
///
/// The `opt_len` / `static_len` accumulators use **wrapping** arithmetic to
/// reproduce the C `ulg` (unsigned-long) modular behavior exactly. This matters
/// for tiny/empty blocks where `build_tree` decrements `opt_len` below zero
/// (the C `s->opt_len--`); the wrapped value then feeds the byte-length
/// rounding in [`tr_flush_block`], so it must match C bit-for-bit (and never
/// panic in a debug build).
#[allow(clippy::needless_range_loop)] // index loop required: the body mutates
// other `DeflateState` fields, so a borrow of `s.heap` cannot be held across it.
fn gen_bitlen(s: &mut DeflateState, kind: TreeKind) {
    let desc = static_desc(kind);
    let stree = desc.static_tree;
    let extra = desc.extra_bits;
    let base = desc.extra_base;
    let max_length = desc.max_length;
    let max_code = max_code_of(s, kind);

    s.bl_count.fill(0);

    // First pass: compute optimal bit lengths (which may overflow for the
    // bit-length tree). The root of the heap gets length 0.
    let root = s.heap[s.heap_max] as usize;
    set_len(s, kind, root, 0);

    let mut overflow: i32 = 0;
    for h in (s.heap_max + 1)..HEAP_SIZE {
        let n = s.heap[h];
        let dad = tree_node(s, kind, n as usize).dad() as usize;
        let mut bits = tree_node(s, kind, dad).len() as usize + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        // We overwrite the Dad slot (now unused) with Len.
        set_len(s, kind, n as usize, bits as u16);

        if n > max_code {
            continue; // not a leaf node
        }
        s.bl_count[bits] += 1;
        let xbits: usize = if (n as usize) >= base {
            extra[(n as usize) - base] as usize
        } else {
            0
        };
        let f = tree_node(s, kind, n as usize).freq() as usize;
        s.opt_len = s.opt_len.wrapping_add(f.wrapping_mul(bits + xbits));
        if let Some(st) = stree {
            let slen = st[n as usize].len() as usize;
            s.static_len = s.static_len.wrapping_add(f.wrapping_mul(slen + xbits));
        }
    }
    if overflow == 0 {
        return;
    }

    // Find the first bit length that can increase, and move leaves around so no
    // code exceeds `max_length` (`do { ... } while (overflow > 0)`).
    loop {
        let mut bits = max_length - 1;
        while s.bl_count[bits] == 0 {
            bits -= 1;
        }
        s.bl_count[bits] = s.bl_count[bits].wrapping_sub(1); // one leaf down
        s.bl_count[bits + 1] = s.bl_count[bits + 1].wrapping_add(2); // its brother
        s.bl_count[max_length] = s.bl_count[max_length].wrapping_sub(1);
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // Recompute all bit lengths, scanning in increasing frequency. `h` is still
    // `HEAP_SIZE` from the first pass.
    let mut h = HEAP_SIZE;
    for bits in (1..=max_length).rev() {
        let mut n = s.bl_count[bits];
        while n != 0 {
            h -= 1;
            let m = s.heap[h];
            if m > max_code {
                continue;
            }
            let cur = tree_node(s, kind, m as usize).len() as usize;
            if cur != bits {
                let freq = tree_node(s, kind, m as usize).freq() as usize;
                s.opt_len = s
                    .opt_len
                    .wrapping_add(bits.wrapping_sub(cur).wrapping_mul(freq));
                set_len(s, kind, m as usize, bits as u16);
            }
            n -= 1;
        }
    }
}

/// Construct one Huffman tree and assign its codes and lengths (`build_tree`,
/// trees.c L627-706). Also updates `opt_len` / `static_len` and the per-tree
/// `max_code`.
fn build_tree(s: &mut DeflateState, kind: TreeKind) {
    let desc = static_desc(kind);
    let stree = desc.static_tree;
    let elems = desc.elems;
    let mut max_code: i32 = -1;

    // Build the initial heap from the non-zero-frequency elements; zero-freq
    // elements get length 0.
    s.heap_len = 0;
    s.heap_max = HEAP_SIZE;

    for n in 0..elems {
        if tree_node(s, kind, n).freq() != 0 {
            s.heap_len += 1;
            s.heap[s.heap_len] = n as i32;
            max_code = n as i32;
            s.depth[n] = 0;
        } else {
            set_len(s, kind, n, 0);
        }
    }

    // The pkzip format requires at least two codes of non-zero frequency, so we
    // force them if necessary (codes 0 and/or 1 have no extra bits).
    while s.heap_len < 2 {
        let new_node: i32 = if max_code < 2 {
            max_code += 1;
            max_code
        } else {
            0
        };
        s.heap_len += 1;
        s.heap[s.heap_len] = new_node;
        set_freq(s, kind, new_node as usize, 1);
        s.depth[new_node as usize] = 0;
        s.opt_len = s.opt_len.wrapping_sub(1);
        if let Some(st) = stree {
            s.static_len = s
                .static_len
                .wrapping_sub(st[new_node as usize].len() as usize);
        }
    }
    set_max_code(s, kind, max_code);

    // Establish sub-heaps of increasing lengths.
    for k in (1..=(s.heap_len / 2)).rev() {
        pqdownheap(s, kind, k);
    }

    // Repeatedly combine the two least-frequent nodes into a parent.
    let mut node_idx: i32 = elems as i32;
    loop {
        // pqremove: pop the least-frequent node `n`.
        let n_node = s.heap[SMALLEST];
        s.heap[SMALLEST] = s.heap[s.heap_len];
        s.heap_len -= 1;
        pqdownheap(s, kind, SMALLEST);

        let m_node = s.heap[SMALLEST]; // next least frequency

        // Keep the two nodes sorted by frequency at the top of the heap array.
        s.heap_max -= 1;
        s.heap[s.heap_max] = n_node;
        s.heap_max -= 1;
        s.heap[s.heap_max] = m_node;

        // Create their parent node.
        let fn_ = tree_node(s, kind, n_node as usize).freq();
        let fm = tree_node(s, kind, m_node as usize).freq();
        set_freq(s, kind, node_idx as usize, fn_.wrapping_add(fm));
        let dn = s.depth[n_node as usize];
        let dm = s.depth[m_node as usize];
        s.depth[node_idx as usize] = (if dn >= dm { dn } else { dm }).wrapping_add(1);
        set_dad(s, kind, n_node as usize, node_idx as u16);
        set_dad(s, kind, m_node as usize, node_idx as u16);

        // Insert the new node in the heap.
        s.heap[SMALLEST] = node_idx;
        node_idx += 1;
        pqdownheap(s, kind, SMALLEST);

        if s.heap_len < 2 {
            break;
        }
    }

    s.heap_max -= 1;
    s.heap[s.heap_max] = s.heap[SMALLEST];

    // Now the freq and dad fields are set; generate bit lengths, then codes.
    gen_bitlen(s, kind);

    match kind {
        TreeKind::L => gen_codes(&mut s.dyn_ltree, max_code, &s.bl_count),
        TreeKind::D => gen_codes(&mut s.dyn_dtree, max_code, &s.bl_count),
        TreeKind::Bl => gen_codes(&mut s.bl_tree, max_code, &s.bl_count),
    }
}

// ===========================================================================
// Phase I — Bit-length-tree scan / send
// ===========================================================================

/// Scan a literal or distance tree to count the bit-length-code frequencies
/// (`scan_tree`, trees.c L712-747). Writes the counts into `s.bl_tree[*].Freq`
/// and installs the `0xffff` guard at `tree[max_code + 1]`.
fn scan_tree(s: &mut DeflateState, kind: TreeKind) {
    let max_code = max_code_of(s, kind);
    let mut prevlen: i32 = -1;
    let mut nextlen: i32 = tree_node(s, kind, 0).len() as i32;
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    set_len(s, kind, (max_code + 1) as usize, 0xffff); // guard

    for n in 0..=(max_code as usize) {
        let curlen = nextlen;
        nextlen = tree_node(s, kind, n + 1).len() as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            let idx = curlen as usize;
            let f = s.bl_tree[idx].freq();
            s.bl_tree[idx].set_freq(f.wrapping_add(count as u16));
        } else if curlen != 0 {
            if curlen != prevlen {
                let idx = curlen as usize;
                let f = s.bl_tree[idx].freq();
                s.bl_tree[idx].set_freq(f.wrapping_add(1));
            }
            let f = s.bl_tree[REP_3_6].freq();
            s.bl_tree[REP_3_6].set_freq(f.wrapping_add(1));
        } else if count <= 10 {
            let f = s.bl_tree[REPZ_3_10].freq();
            s.bl_tree[REPZ_3_10].set_freq(f.wrapping_add(1));
        } else {
            let f = s.bl_tree[REPZ_11_138].freq();
            s.bl_tree[REPZ_11_138].set_freq(f.wrapping_add(1));
        }
        count = 0;
        prevlen = curlen;
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

/// Emit a literal or distance tree in compressed form using the bit-length-tree
/// codes (`send_tree`, trees.c L753-794). Mirrors [`scan_tree`] but sends codes
/// instead of counting them; relies on the `0xffff` guard set by `scan_tree`.
fn send_tree(s: &mut DeflateState, kind: TreeKind) {
    let max_code = max_code_of(s, kind);
    let mut prevlen: i32 = -1;
    let mut nextlen: i32 = tree_node(s, kind, 0).len() as i32;
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    for n in 0..=(max_code as usize) {
        let curlen = nextlen;
        nextlen = tree_node(s, kind, n + 1).len() as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            loop {
                send_bl_code(s, curlen as usize);
                count -= 1;
                if count == 0 {
                    break;
                }
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                send_bl_code(s, curlen as usize);
                count -= 1;
            }
            send_bl_code(s, REP_3_6);
            send_bits(s, count - 3, 2);
        } else if count <= 10 {
            send_bl_code(s, REPZ_3_10);
            send_bits(s, count - 3, 3);
        } else {
            send_bl_code(s, REPZ_11_138);
            send_bits(s, count - 11, 7);
        }
        count = 0;
        prevlen = curlen;
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

/// Build the bit-length tree and return the index in `bl_order` of the last
/// bit-length code to send (`build_bl_tree`, trees.c L800-826).
fn build_bl_tree(s: &mut DeflateState) -> i32 {
    // Determine the bit-length-code frequencies for the literal and distance
    // trees.
    scan_tree(s, TreeKind::L);
    scan_tree(s, TreeKind::D);

    // Build the bit-length tree itself.
    build_tree(s, TreeKind::Bl);

    // The pkzip format requires at least 4 bit-length codes be sent.
    let mut max_blindex: i32 = (BL_CODES - 1) as i32;
    while max_blindex >= 3 {
        if s.bl_tree[BL_ORDER[max_blindex as usize] as usize].len() != 0 {
            break;
        }
        max_blindex -= 1;
    }
    // Update opt_len to include the bit-length tree and the 5+5+4 count bits.
    s.opt_len = s
        .opt_len
        .wrapping_add(3 * (max_blindex as usize + 1) + 5 + 5 + 4);

    max_blindex
}

/// Send the dynamic-tree header: the three counts, the bit-length-code lengths
/// (in `bl_order`), and the two compressed trees (`send_all_trees`, trees.c
/// L833-855). IN assertion: `lcodes >= 257`, `dcodes >= 1`, `blcodes >= 4`.
fn send_all_trees(s: &mut DeflateState, lcodes: i32, dcodes: i32, blcodes: i32) {
    send_bits(s, lcodes - 257, 5); // not +255 as stated in appnote.txt
    send_bits(s, dcodes - 1, 5);
    send_bits(s, blcodes - 4, 4); // not -3 as stated in appnote.txt
    for &order in BL_ORDER.iter().take(blcodes as usize) {
        let len = s.bl_tree[order as usize].len() as i32;
        send_bits(s, len, 3);
    }
    send_tree(s, TreeKind::L); // literal tree (uses l_desc_max_code == lcodes-1)
    send_tree(s, TreeKind::D); // distance tree (uses d_desc_max_code == dcodes-1)
}

// ===========================================================================
// Phase J — Block emission (the public-facing trees API)
// ===========================================================================

/// Send a stored (uncompressed) block (`_tr_stored_block`, trees.c L860-875).
///
/// Writes the 3-bit block header, byte-aligns, emits the 16-bit length and its
/// one's complement, then copies `stored_len` bytes from `buf` into
/// `pending_buf`. `pending` is advanced by `stored_len` unconditionally, as in
/// C (callers always supply a non-empty `buf` when `stored_len != 0`).
pub(crate) fn tr_stored_block(
    s: &mut DeflateState,
    buf: Option<&[u8]>,
    stored_len: usize,
    last: bool,
) {
    send_bits(s, (STORED_BLOCK << 1) + last as i32, 3); // block type
    bi_windup(s); // align on byte boundary
    s.put_short(stored_len as u16);
    s.put_short(!(stored_len as u16)); // one's complement
    if stored_len != 0 {
        if let Some(b) = buf {
            let start = s.pending;
            s.pending_buf[start..start + stored_len].copy_from_slice(&b[..stored_len]);
        }
    }
    s.pending += stored_len;
}

/// Flush the bit buffer to pending output (`_tr_flush_bits`, trees.c L880-882).
pub(crate) fn tr_flush_bits(s: &mut DeflateState) {
    bi_flush(s);
}

/// Send one empty static block to give inflate enough lookahead (`_tr_align`,
/// trees.c L888-895). Costs 10 bits, of which up to 7 may stay buffered.
pub(crate) fn tr_align(s: &mut DeflateState) {
    send_bits(s, STATIC_TREES << 1, 3);
    send_code(s, END_BLOCK, &STATIC_LTREE);
    bi_flush(s);
}

/// Send the symbol buffer compressed with the given trees (`compress_block`,
/// trees.c L900-951).
///
/// `dynamic` selects between the per-block dynamic trees (`dyn_ltree` /
/// `dyn_dtree`) and the static trees (`STATIC_LTREE` / `STATIC_DTREE`). Each
/// node is read as a `Copy` value *before* calling [`send_bits`], so no borrow
/// of `s` is held across the mutation — no aliasing, no `unsafe`.
fn compress_block(s: &mut DeflateState, dynamic: bool) {
    let mut sx: usize = 0;
    if s.sym_next != 0 {
        loop {
            let (mut dist, lc) = s.sym_read(sx);
            sx += 3;
            if dist == 0 {
                // Literal byte.
                let ln = if dynamic {
                    s.dyn_ltree[lc as usize]
                } else {
                    STATIC_LTREE[lc as usize]
                };
                send_bits(s, ln.code() as i32, ln.len() as i32);
            } else {
                // Match: `lc` is the (length - MIN_MATCH); send the length code.
                let code = LENGTH_CODE[lc as usize] as usize;
                let li = code + LITERALS + 1;
                let ln = if dynamic {
                    s.dyn_ltree[li]
                } else {
                    STATIC_LTREE[li]
                };
                send_bits(s, ln.code() as i32, ln.len() as i32);
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    let lc2 = lc as i32 - BASE_LENGTH[code];
                    send_bits(s, lc2, extra);
                }
                dist -= 1; // dist is now the match distance - 1
                let dcode = d_code(dist as usize);
                let dn = if dynamic {
                    s.dyn_dtree[dcode]
                } else {
                    STATIC_DTREE[dcode]
                };
                send_bits(s, dn.code() as i32, dn.len() as i32);
                let extra = EXTRA_DBITS[dcode];
                if extra != 0 {
                    let dist2 = dist as i32 - BASE_DIST[dcode];
                    send_bits(s, dist2, extra);
                }
            }
            if sx >= s.sym_next {
                break;
            }
        }
    }

    let en = if dynamic {
        s.dyn_ltree[END_BLOCK]
    } else {
        STATIC_LTREE[END_BLOCK]
    };
    send_bits(s, en.code() as i32, en.len() as i32);
}

/// Classify the current block as `Z_BINARY` or `Z_TEXT` (`detect_data_type`,
/// trees.c L966-991), using the block-listed / allow-listed byte algorithm.
fn detect_data_type(s: &DeflateState) -> i32 {
    // block_mask flags the "block-listed" bytes 0..6, 14..25, 28..31.
    let mut block_mask: u32 = 0xf3ff_c07f;
    for freq_node in &s.dyn_ltree[0..=31] {
        if (block_mask & 1) != 0 && freq_node.freq() != 0 {
            return Z_BINARY;
        }
        block_mask >>= 1;
    }

    // Allow-listed bytes: TAB (9), LF (10), CR (13), then 32..=255.
    if s.dyn_ltree[9].freq() != 0 || s.dyn_ltree[10].freq() != 0 || s.dyn_ltree[13].freq() != 0 {
        return Z_TEXT;
    }
    for freq_node in &s.dyn_ltree[32..LITERALS] {
        if freq_node.freq() != 0 {
            return Z_TEXT;
        }
    }

    // Only "gray-listed" bytes (or empty): treat as binary.
    Z_BINARY
}

/// Determine the best encoding for the current block — stored, static, or
/// dynamic — and write it out (`_tr_flush_block`, trees.c L997-1089).
///
/// The byte-length rounding (`(opt_len + 3 + 7) >> 3`), the `static_lenb <=
/// opt_lenb` tie that favors static trees, the `Z_FIXED` force-static, the
/// `stored_len + 4 <= opt_lenb` stored-block preference, and the
/// `static_lenb == opt_lenb` equality test all directly select the emitted
/// block type and are reproduced verbatim. The `opt_len`/`static_len` byte
/// conversions use wrapping arithmetic to match C `ulg` semantics.
pub(crate) fn tr_flush_block(
    s: &mut DeflateState,
    buf: Option<&[u8]>,
    stored_len: usize,
    last: bool,
) {
    let opt_lenb: usize;
    let static_lenb: usize;
    let mut max_blindex: i32 = 0;

    // Build the Huffman trees unless a stored block is forced.
    if s.level > 0 {
        // Check whether the block is binary or text.
        if s.data_type == Z_UNKNOWN {
            s.data_type = detect_data_type(s);
        }

        // Construct the literal and distance trees.
        build_tree(s, TreeKind::L);
        build_tree(s, TreeKind::D);

        // Build the bit-length tree for the two trees above.
        max_blindex = build_bl_tree(s);

        // Determine the best encoding. Compute the block lengths in bytes.
        let mut ol = s.opt_len.wrapping_add(3 + 7) >> 3;
        let sl = s.static_len.wrapping_add(3 + 7) >> 3;
        if sl <= ol || s.strategy == Strategy::Fixed as i32 {
            ol = sl;
        }
        opt_lenb = ol;
        static_lenb = sl;
    } else {
        // Force a stored block.
        opt_lenb = stored_len + 5;
        static_lenb = stored_len + 5;
    }

    if stored_len + 4 <= opt_lenb && buf.is_some() {
        // 4: two 16-bit words for the lengths. Prefer a stored block when it is
        // no larger; with LIT_BUFSIZE <= WSIZE it is never too late to do so.
        tr_stored_block(s, buf, stored_len, last);
    } else if static_lenb == opt_lenb {
        send_bits(s, (STATIC_TREES << 1) + last as i32, 3);
        compress_block(s, false);
    } else {
        send_bits(s, (DYN_TREES << 1) + last as i32, 3);
        let lcodes = s.l_desc_max_code + 1;
        let dcodes = s.d_desc_max_code + 1;
        let blcodes = max_blindex + 1;
        send_all_trees(s, lcodes, dcodes, blcodes);
        compress_block(s, true);
    }

    // The above writes the block; reinitialize for the next one.
    init_block(s);

    if last {
        bi_windup(s);
    }
}

/// Save the match/literal info and tally the frequency counts (`_tr_tally`,
/// trees.c L1095-1119). Returns `true` when the symbol buffer is full and the
/// caller must flush the current block.
pub(crate) fn tr_tally(s: &mut DeflateState, dist: usize, lc: usize) -> bool {
    s.sym_write_raw(dist as u16, lc as u8);

    if dist == 0 {
        // `lc` is the unmatched literal byte.
        let f = s.dyn_ltree[lc].freq();
        s.dyn_ltree[lc].set_freq(f.wrapping_add(1));
    } else {
        s.matches += 1;
        // `lc` is the match length - MIN_MATCH; `dist` becomes distance - 1.
        let d = dist - 1;
        let li = LENGTH_CODE[lc] as usize + LITERALS + 1;
        let f = s.dyn_ltree[li].freq();
        s.dyn_ltree[li].set_freq(f.wrapping_add(1));
        let di = d_code(d);
        let f = s.dyn_dtree[di].freq();
        s.dyn_dtree[di].set_freq(f.wrapping_add(1));
    }

    s.sym_next == s.sym_end
}

/// Tally a literal byte (`_tr_tally_lit` macro). Returns `true` if the block
/// must be flushed.
#[inline]
pub(crate) fn tr_tally_lit(s: &mut DeflateState, c: u8) -> bool {
    tr_tally(s, 0, c as usize)
}

/// Tally a match of the given distance and (length - MIN_MATCH)
/// (`_tr_tally_dist` macro). Returns `true` if the block must be flushed.
#[inline]
pub(crate) fn tr_tally_dist(s: &mut DeflateState, dist: usize, len: u8) -> bool {
    tr_tally(s, dist, len as usize)
}

// ===========================================================================
// DeflateState convenience methods
//
// `state.rs` drives the trees layer through method-call syntax
// (`self.tr_init()` in `deflate_reset`, `self.state.tr_flush_bits()` in
// `flush_pending`). Because `DeflateState` lives in a sibling module of the
// same crate, these inherent methods can be defined here; each is a thin,
// `#[inline]` delegate to the corresponding free function above so there is a
// single implementation. The free functions remain the primary API the rest
// of the deflate engine (`mod.rs`, the strategy modules) calls directly.
// ===========================================================================

impl DeflateState {
    /// Initialize the tree data structures for a new stream — method form of
    /// [`tr_init`], invoked by `state.rs`'s deflate reset.
    #[inline]
    pub(crate) fn tr_init(&mut self) {
        tr_init(self);
    }

    /// Flush the bit accumulator to pending output — method form of
    /// [`tr_flush_bits`], invoked by `state.rs`'s `flush_pending`.
    #[inline]
    pub(crate) fn tr_flush_bits(&mut self) {
        tr_flush_bits(self);
    }
}

// ===========================================================================
// Tests — self-check the static tables against the `tr_static_init`
// generation algorithm (trees.c L295-368) and exercise the bit helpers.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_sizes_and_special_cases() {
        assert_eq!(STATIC_LTREE.len(), L_CODES + 2);
        assert_eq!(STATIC_DTREE.len(), D_CODES);
        assert_eq!(DIST_CODE.len(), 512);
        assert_eq!(LENGTH_CODE.len(), MAX_MATCH - MIN_MATCH + 1);
        assert_eq!(BASE_LENGTH.len(), LENGTH_CODES);
        assert_eq!(BASE_DIST.len(), D_CODES);
        // Match length 258 special case and the unused final base length.
        assert_eq!(LENGTH_CODE[255], 28);
        assert_eq!(BASE_LENGTH[LENGTH_CODES - 1], 0);
    }

    #[test]
    fn ltree_anchor_values() {
        assert_eq!(STATIC_LTREE[0], node(12, 8));
        assert_eq!(STATIC_LTREE[1], node(140, 8));
        assert_eq!(STATIC_LTREE[2], node(76, 8));
        assert_eq!(STATIC_LTREE[3], node(204, 8));
        assert_eq!(STATIC_LTREE[143], node(253, 8));
        assert_eq!(STATIC_LTREE[144], node(19, 9));
        assert_eq!(STATIC_LTREE[255], node(511, 9));
        assert_eq!(STATIC_LTREE[256], node(0, 7));
        assert_eq!(STATIC_LTREE[279], node(116, 7));
        assert_eq!(STATIC_LTREE[280], node(3, 8));
        assert_eq!(STATIC_LTREE[287], node(227, 8));
    }

    #[test]
    fn bi_reverse_matches_examples() {
        assert_eq!(bi_reverse(0b1, 1), 0b1);
        assert_eq!(bi_reverse(0b10, 2), 0b01);
        assert_eq!(bi_reverse(0b1011, 4), 0b1101);
        assert_eq!(bi_reverse(0, 5), 0);
    }

    /// Reproduce `tr_static_init` (trees.c L317-367) and assert every const
    /// table equals what the C generation algorithm produces.
    #[test]
    fn tr_static_init_reproduces_const_tables() {
        // --- length_code + base_length (trees.c L317-330) ---
        let mut length_code = [0u8; MAX_MATCH - MIN_MATCH + 1];
        let mut base_length = [0i32; LENGTH_CODES];
        let mut length = 0usize;
        for code in 0..(LENGTH_CODES - 1) {
            base_length[code] = length as i32;
            for _ in 0..(1 << EXTRA_LBITS[code]) {
                length_code[length] = code as u8;
                length += 1;
            }
        }
        assert_eq!(length, 256);
        // length 255 (match length 258) uses code 28 (== LENGTH_CODES - 1).
        length_code[length - 1] = (LENGTH_CODES - 1) as u8;
        assert_eq!(length_code, LENGTH_CODE);
        assert_eq!(base_length, BASE_LENGTH);

        // --- dist_code + base_dist (trees.c L332-348) ---
        let mut dist_code = [0u8; 512];
        let mut base_dist = [0i32; D_CODES];
        let mut dist = 0usize;
        for code in 0..16 {
            base_dist[code] = dist as i32;
            for _ in 0..(1 << EXTRA_DBITS[code]) {
                dist_code[dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256);
        dist >>= 7; // from now on all distances are divided by 128
        for code in 16..D_CODES {
            base_dist[code] = (dist as i32) << 7;
            for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                dist_code[256 + dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256);
        assert_eq!(dist_code, DIST_CODE);
        assert_eq!(base_dist, BASE_DIST);

        // --- static_ltree codes (trees.c L350-361) ---
        let mut bl_count = [0u16; MAX_BITS + 1];
        let mut ltree = [HuffmanNode::default(); L_CODES + 2];
        let mut n = 0usize;
        while n <= 143 {
            ltree[n].set_len(8);
            bl_count[8] += 1;
            n += 1;
        }
        while n <= 255 {
            ltree[n].set_len(9);
            bl_count[9] += 1;
            n += 1;
        }
        while n <= 279 {
            ltree[n].set_len(7);
            bl_count[7] += 1;
            n += 1;
        }
        while n <= 287 {
            ltree[n].set_len(8);
            bl_count[8] += 1;
            n += 1;
        }
        gen_codes(&mut ltree, (L_CODES + 1) as i32, &bl_count);
        for i in 0..(L_CODES + 2) {
            assert_eq!(ltree[i].len(), STATIC_LTREE[i].len(), "ltree len at {i}");
            assert_eq!(ltree[i].code(), STATIC_LTREE[i].code(), "ltree code at {i}");
        }

        // --- static_dtree (trees.c L363-367) ---
        for (i, dnode) in STATIC_DTREE.iter().enumerate() {
            assert_eq!(dnode.len(), 5);
            assert_eq!(dnode.code() as u32, bi_reverse(i as u32, 5));
        }
    }
}
