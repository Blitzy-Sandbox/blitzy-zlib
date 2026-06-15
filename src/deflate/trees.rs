//! Huffman tree construction, static tables, bit I/O, and DEFLATE block
//! emission — a behavior-preserving port of zlib's `trees.c` and `trees.h`.
//!
//! This module owns the encoder-side Huffman machinery:
//!
//! * [`HuffmanNode`] — the Rust replacement for the C `ct_data` union, holding
//!   either the (frequency, dad) pair during tree construction or the
//!   (code, length) pair afterwards.
//! * The pre-generated static literal/length and distance trees
//!   ([`STATIC_LTREE`], [`STATIC_DTREE`]) plus the distance- and length-code
//!   lookup tables ([`DIST_CODE`], [`LENGTH_CODE`]) — transcribed verbatim from
//!   `trees.h` so the emitted bitstream is byte-identical to canonical zlib.
//! * The bit accumulator I/O ([`DeflateState::tr_flush_bits`] and friends),
//!   the dynamic Huffman tree builder, and the DEFLATE block emitter
//!   ([`DeflateState::tr_flush_block`]).
//!
//! # Byte-exact output
//!
//! Every table value, loop bound, and comparison operator in this module is
//! load-bearing for the output bitstream (AAP §0.6.6, §0.6.7, §0.7.1). The code
//! is a mechanical translation of `trees.c`; the Huffman code construction
//! (frequency counting, canonical code assignment, tie-breaking), the
//! block-type selection (stored / static / dynamic), and the LSB-first
//! bit-packing order all match C zlib exactly.
//!
//! # Safety and `no_std`
//!
//! The module contains **no `unsafe`** (the compression core is 100% safe Rust;
//! `unsafe` lives only in `ffi.rs` and `inflate/fast.rs`, AAP §0.6.2) and uses
//! only `core` — it is `no_std`-clean and allocation-free, operating entirely on
//! the fixed-size arrays owned by [`DeflateState`] and on `const` tables.
//!
//! # Arithmetic note
//!
//! `opt_len`/`static_len` mirror C's modular `ulg` accumulators. `build_tree`
//! transiently underflows them when forcing two codes for a near-empty block
//! and `gen_bitlen` adds the deficit back, so all arithmetic on these fields
//! uses wrapping operations. Because the true values are far below `2^32`, the
//! 64-bit wrapping used here yields byte counts identical to C's `ulg`.

use crate::constants::{Strategy, Z_BINARY, Z_TEXT, Z_UNKNOWN};
use crate::deflate::state::{
    BL_CODES, BUF_SIZE, D_CODES, DeflateState, END_BLOCK, HEAP_SIZE, L_CODES, LENGTH_CODES,
    LITERALS, MAX_BITS, MAX_BL_BITS, MAX_MATCH, MIN_MATCH,
};

// ===========================================================================
// Phase A — HuffmanNode (replaces the C `ct_data` union)
// ===========================================================================
//
// The C type is
//
// ```c
// typedef struct ct_data_s {
//     union { ush freq; ush code; } fc;   // frequency / code
//     union { ush dad;  ush len;  } dl;   // father / length
// } ct_data;
// ```
//
// Both members are 16-bit and the union variant in use depends only on the
// phase of the algorithm: during `build_tree` the fields hold `freq`/`dad`,
// and after `gen_codes`/`gen_bitlen` they hold `code`/`len`. Modelling the two
// 16-bit words directly — with named accessors for each interpretation —
// reproduces the layout and the access pattern without any `unsafe` union
// punning.

/// A single Huffman tree entry, mirroring the C `ct_data` union.
///
/// The first word ([`freq_or_code`](Self::freq_or_code)) holds the symbol
/// *frequency* while a tree is being constructed and the canonical *code* once
/// [`gen_codes`] has run. The second word ([`dad_or_len`](Self::dad_or_len))
/// holds the *parent* index during construction and the assigned bit *length*
/// afterwards. Use the named accessors ([`freq`](Self::freq)/[`code`](Self::code)
/// and [`dad`](Self::dad)/[`len`](Self::len)) to make the intent explicit at
/// each call site.
///
/// `#[repr(C)]` guarantees a C-compatible two-`u16` layout (AAP §0.6.5).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct HuffmanNode {
    /// `fc` union: the symbol frequency during tree construction, or the
    /// canonical (bit-reversed) Huffman code afterwards.
    pub freq_or_code: u16,
    /// `dl` union: the parent node index during construction, or the assigned
    /// code bit-length afterwards.
    pub dad_or_len: u16,
}

impl HuffmanNode {
    /// Read the node frequency (`ct_data.Freq`).
    #[inline]
    pub fn freq(&self) -> u16 {
        self.freq_or_code
    }

    /// Set the node frequency (`ct_data.Freq`).
    #[inline]
    pub fn set_freq(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Read the canonical Huffman code (`ct_data.Code`).
    #[inline]
    pub fn code(&self) -> u16 {
        self.freq_or_code
    }

    /// Set the canonical Huffman code (`ct_data.Code`).
    #[inline]
    pub fn set_code(&mut self, v: u16) {
        self.freq_or_code = v;
    }

    /// Read the parent node index (`ct_data.Dad`).
    #[inline]
    pub fn dad(&self) -> u16 {
        self.dad_or_len
    }

    /// Set the parent node index (`ct_data.Dad`).
    #[inline]
    pub fn set_dad(&mut self, v: u16) {
        self.dad_or_len = v;
    }

    /// Read the assigned code bit-length (`ct_data.Len`).
    ///
    /// This is the Huffman *code length* in bits, not a container length;
    /// the `len`/`set_len` accessor names mirror the C `ct_data.Len` field
    /// (see the schema). `is_empty` is meaningless for a fixed two-word node,
    /// so the corresponding clippy lint is intentionally allowed here.
    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        self.dad_or_len
    }

    /// Set the assigned code bit-length (`ct_data.Len`).
    #[inline]
    pub fn set_len(&mut self, v: u16) {
        self.dad_or_len = v;
    }
}

/// `const` constructor used to build the static tables below. `fc` is the
/// `freq_or_code` word and `dl` the `dad_or_len` word; for the static trees
/// these are the canonical code and the bit length respectively.
const fn node(fc: u16, dl: u16) -> HuffmanNode {
    HuffmanNode {
        freq_or_code: fc,
        dad_or_len: dl,
    }
}

// ===========================================================================
// Phase B — Static Huffman tables (transcribed verbatim from `trees.h`)
// ===========================================================================
//
// These arrays are the canonical, C-generated DEFLATE static trees and the
// distance/length lookup tables. They are reproduced byte-for-byte from
// `trees.h` (the output of zlib's `-DGEN_TREES_H` build); every value is
// load-bearing for byte-identical compressed output and must not be altered.

/// The static literal/length tree (`trees.h` `static_ltree`). Indices 0..=143
/// have length 8, 144..=255 length 9, 256..=279 length 7, and 280..=287
/// length 8; each `code` is the canonical bit-reversed Huffman code. Codes
/// 286/287 do not occur in real data but are required to build a canonical
/// tree, hence the `L_CODES + 2` size.
#[rustfmt::skip]
pub(crate) const STATIC_LTREE: [HuffmanNode; L_CODES + 2] = [
    node(12, 8), node(140, 8), node(76, 8), node(204, 8), node(44, 8),
    node(172, 8), node(108, 8), node(236, 8), node(28, 8), node(156, 8),
    node(92, 8), node(220, 8), node(60, 8), node(188, 8), node(124, 8),
    node(252, 8), node(2, 8), node(130, 8), node(66, 8), node(194, 8),
    node(34, 8), node(162, 8), node(98, 8), node(226, 8), node(18, 8),
    node(146, 8), node(82, 8), node(210, 8), node(50, 8), node(178, 8),
    node(114, 8), node(242, 8), node(10, 8), node(138, 8), node(74, 8),
    node(202, 8), node(42, 8), node(170, 8), node(106, 8), node(234, 8),
    node(26, 8), node(154, 8), node(90, 8), node(218, 8), node(58, 8),
    node(186, 8), node(122, 8), node(250, 8), node(6, 8), node(134, 8),
    node(70, 8), node(198, 8), node(38, 8), node(166, 8), node(102, 8),
    node(230, 8), node(22, 8), node(150, 8), node(86, 8), node(214, 8),
    node(54, 8), node(182, 8), node(118, 8), node(246, 8), node(14, 8),
    node(142, 8), node(78, 8), node(206, 8), node(46, 8), node(174, 8),
    node(110, 8), node(238, 8), node(30, 8), node(158, 8), node(94, 8),
    node(222, 8), node(62, 8), node(190, 8), node(126, 8), node(254, 8),
    node(1, 8), node(129, 8), node(65, 8), node(193, 8), node(33, 8),
    node(161, 8), node(97, 8), node(225, 8), node(17, 8), node(145, 8),
    node(81, 8), node(209, 8), node(49, 8), node(177, 8), node(113, 8),
    node(241, 8), node(9, 8), node(137, 8), node(73, 8), node(201, 8),
    node(41, 8), node(169, 8), node(105, 8), node(233, 8), node(25, 8),
    node(153, 8), node(89, 8), node(217, 8), node(57, 8), node(185, 8),
    node(121, 8), node(249, 8), node(5, 8), node(133, 8), node(69, 8),
    node(197, 8), node(37, 8), node(165, 8), node(101, 8), node(229, 8),
    node(21, 8), node(149, 8), node(85, 8), node(213, 8), node(53, 8),
    node(181, 8), node(117, 8), node(245, 8), node(13, 8), node(141, 8),
    node(77, 8), node(205, 8), node(45, 8), node(173, 8), node(109, 8),
    node(237, 8), node(29, 8), node(157, 8), node(93, 8), node(221, 8),
    node(61, 8), node(189, 8), node(125, 8), node(253, 8), node(19, 9),
    node(275, 9), node(147, 9), node(403, 9), node(83, 9), node(339, 9),
    node(211, 9), node(467, 9), node(51, 9), node(307, 9), node(179, 9),
    node(435, 9), node(115, 9), node(371, 9), node(243, 9), node(499, 9),
    node(11, 9), node(267, 9), node(139, 9), node(395, 9), node(75, 9),
    node(331, 9), node(203, 9), node(459, 9), node(43, 9), node(299, 9),
    node(171, 9), node(427, 9), node(107, 9), node(363, 9), node(235, 9),
    node(491, 9), node(27, 9), node(283, 9), node(155, 9), node(411, 9),
    node(91, 9), node(347, 9), node(219, 9), node(475, 9), node(59, 9),
    node(315, 9), node(187, 9), node(443, 9), node(123, 9), node(379, 9),
    node(251, 9), node(507, 9), node(7, 9), node(263, 9), node(135, 9),
    node(391, 9), node(71, 9), node(327, 9), node(199, 9), node(455, 9),
    node(39, 9), node(295, 9), node(167, 9), node(423, 9), node(103, 9),
    node(359, 9), node(231, 9), node(487, 9), node(23, 9), node(279, 9),
    node(151, 9), node(407, 9), node(87, 9), node(343, 9), node(215, 9),
    node(471, 9), node(55, 9), node(311, 9), node(183, 9), node(439, 9),
    node(119, 9), node(375, 9), node(247, 9), node(503, 9), node(15, 9),
    node(271, 9), node(143, 9), node(399, 9), node(79, 9), node(335, 9),
    node(207, 9), node(463, 9), node(47, 9), node(303, 9), node(175, 9),
    node(431, 9), node(111, 9), node(367, 9), node(239, 9), node(495, 9),
    node(31, 9), node(287, 9), node(159, 9), node(415, 9), node(95, 9),
    node(351, 9), node(223, 9), node(479, 9), node(63, 9), node(319, 9),
    node(191, 9), node(447, 9), node(127, 9), node(383, 9), node(255, 9),
    node(511, 9), node(0, 7), node(64, 7), node(32, 7), node(96, 7),
    node(16, 7), node(80, 7), node(48, 7), node(112, 7), node(8, 7),
    node(72, 7), node(40, 7), node(104, 7), node(24, 7), node(88, 7),
    node(56, 7), node(120, 7), node(4, 7), node(68, 7), node(36, 7),
    node(100, 7), node(20, 7), node(84, 7), node(52, 7), node(116, 7),
    node(3, 8), node(131, 8), node(67, 8), node(195, 8), node(35, 8),
    node(163, 8), node(99, 8), node(227, 8),
];

/// The static distance tree (`trees.h` `static_dtree`). All 30 codes use 5
/// bits; each `code` is `bi_reverse(n, 5)`.
#[rustfmt::skip]
pub(crate) const STATIC_DTREE: [HuffmanNode; D_CODES] = [
    node(0, 5), node(16, 5), node(8, 5), node(24, 5), node(4, 5),
    node(20, 5), node(12, 5), node(28, 5), node(2, 5), node(18, 5),
    node(10, 5), node(26, 5), node(6, 5), node(22, 5), node(14, 5),
    node(30, 5), node(1, 5), node(17, 5), node(9, 5), node(25, 5),
    node(5, 5), node(21, 5), node(13, 5), node(29, 5), node(3, 5),
    node(19, 5), node(11, 5), node(27, 5), node(7, 5), node(23, 5),
];

/// Distance-code lookup (`trees.h` `_dist_code`, `DIST_CODE_LEN = 512`). The
/// first 256 entries map distances 1..=256 to their distance code; the last
/// 256 entries map the top bits of larger distances (see [`d_code`]).
#[rustfmt::skip]
pub(crate) const DIST_CODE: [u8; 512] = [
    0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8,
    8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11,
    11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 13, 13, 13, 13,
    13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
    13, 13, 13, 13, 13, 13, 13, 13, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 0, 0, 16, 17,
    18, 18, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 22,
    23, 23, 23, 23, 23, 23, 23, 23, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
    29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29,
];

/// Length-code lookup (`trees.h` `_length_code`), indexed by the normalized
/// match length `match_length - MIN_MATCH` (0..=255). Note `LENGTH_CODE[255]`
/// is `28`: match length 258 uses code 285 (no extra bits) rather than
/// code 284 + 5 extra bits.
#[rustfmt::skip]
pub(crate) const LENGTH_CODE: [u8; MAX_MATCH - MIN_MATCH + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12,
    13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16,
    17, 17, 17, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 18, 18, 19, 19, 19, 19,
    19, 19, 19, 19, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20,
    21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 22, 22, 22, 22,
    22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23,
    23, 23, 23, 23, 23, 23, 23, 23, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 28,
];

/// First (normalized) match length for each length code (`trees.h`
/// `base_length`; the unused final entry is 0).
#[rustfmt::skip]
pub(crate) const BASE_LENGTH: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56,
    64, 80, 96, 112, 128, 160, 192, 224, 0,
];

/// First distance for each distance code (`trees.h` `base_dist`).
#[rustfmt::skip]
pub(crate) const BASE_DIST: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24,
    32, 48, 64, 96, 128, 192, 256, 384, 512, 768,
    1024, 1536, 2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

// ===========================================================================
// Phase C — Extra-bit tables, transmission order, and block-type sentinels
// ===========================================================================

/// Number of extra bits carried after each length code (`trees.c`
/// `extra_lbits`).
#[rustfmt::skip]
pub(crate) const EXTRA_LBITS: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Number of extra bits carried after each distance code (`trees.c`
/// `extra_dbits`).
#[rustfmt::skip]
pub(crate) const EXTRA_DBITS: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Number of extra bits carried after each bit-length code (`trees.c`
/// `extra_blbits`).
#[rustfmt::skip]
pub(crate) const EXTRA_BLBITS: [i32; BL_CODES] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Order in which the bit-length-code lengths are transmitted (`trees.c`
/// `bl_order`). The most-probable codes are sent first so trailing unused
/// entries can be omitted.
#[rustfmt::skip]
pub(crate) const BL_ORDER: [u8; BL_CODES] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// DEFLATE block type: a stored (uncompressed) block (`trees.c` `STORED_BLOCK`).
pub(crate) const STORED_BLOCK: i32 = 0;
/// DEFLATE block type: a block using the fixed/static Huffman trees (`trees.c`
/// `STATIC_TREES`).
pub(crate) const STATIC_TREES: i32 = 1;
/// DEFLATE block type: a block using dynamic Huffman trees (`trees.c`
/// `DYN_TREES`).
pub(crate) const DYN_TREES: i32 = 2;

/// Bit-length-tree symbol: repeat the previous length 3–6 times (2 extra bits).
pub(crate) const REP_3_6: usize = 16;
/// Bit-length-tree symbol: repeat a zero length 3–10 times (3 extra bits).
pub(crate) const REPZ_3_10: usize = 17;
/// Bit-length-tree symbol: repeat a zero length 11–138 times (7 extra bits).
pub(crate) const REPZ_11_138: usize = 18;

/// Heap index of the least-frequent node. The Huffman heap is 1-based, so
/// `heap[0]` is unused (`trees.c` `SMALLEST`).
const SMALLEST: usize = 1;

// ===========================================================================
// Phase D — Static tree descriptors and the tree selector
// ===========================================================================
//
// The C `static_tree_desc` bundles a static tree with its extra-bit table and
// sizing parameters. The dynamic trees themselves are array fields on
// `DeflateState`; here we keep only the immutable per-tree descriptor as
// `const` data, selected at runtime by a `TreeKind`.

/// Immutable description of one Huffman tree kind (`trees.c`
/// `static_tree_desc`): its static tree (if any), the extra-bit table, the base
/// index into that table, the element count, and the maximum code length.
pub(crate) struct StaticTreeDesc {
    /// The static tree for this kind, or `None` for the bit-length tree (which
    /// has no static form).
    pub static_tree: Option<&'static [HuffmanNode]>,
    /// Extra bits carried after each code of this kind.
    pub extra_bits: &'static [i32],
    /// First code index at which `extra_bits` applies.
    pub extra_base: usize,
    /// Maximum number of elements in this tree.
    pub elems: usize,
    /// Maximum bit length any code of this kind may use.
    pub max_length: usize,
}

/// Descriptor for the literal/length tree (`trees.c` `static_l_desc`).
pub(crate) const STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_LTREE),
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the distance tree (`trees.c` `static_d_desc`).
pub(crate) const STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_DTREE),
    extra_bits: &EXTRA_DBITS,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// Descriptor for the bit-length tree (`trees.c` `static_bl_desc`); it has no
/// static tree.
pub(crate) const STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_BLBITS,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

/// Selects which of the three dynamic Huffman trees a routine operates on.
///
/// Passing a `TreeKind` (rather than a `&mut` slice into `DeflateState`) lets
/// the tree-building functions index the appropriate `DeflateState` array field
/// internally, sidestepping aliasing borrows while remaining `unsafe`-free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TreeKind {
    /// The literal/length tree (`dyn_ltree`).
    L,
    /// The distance tree (`dyn_dtree`).
    D,
    /// The bit-length tree (`bl_tree`).
    Bl,
}

/// Return the immutable static descriptor for a tree kind.
#[inline]
fn static_desc(kind: TreeKind) -> &'static StaticTreeDesc {
    match kind {
        TreeKind::L => &STATIC_L_DESC,
        TreeKind::D => &STATIC_D_DESC,
        TreeKind::Bl => &STATIC_BL_DESC,
    }
}

/// Read (by value) one node of the dynamic tree selected by `kind`.
///
/// `HuffmanNode` is `Copy`, so a caller can read a node into a local and then
/// mutate `DeflateState` (for example via `send_bits`) without holding an
/// aliasing borrow.
#[inline]
fn tnode(s: &DeflateState, kind: TreeKind, i: usize) -> HuffmanNode {
    match kind {
        TreeKind::L => s.dyn_ltree[i],
        TreeKind::D => s.dyn_dtree[i],
        TreeKind::Bl => s.bl_tree[i],
    }
}

/// Mutable access to one node of the dynamic tree selected by `kind`.
#[inline]
fn tnode_mut(s: &mut DeflateState, kind: TreeKind, i: usize) -> &mut HuffmanNode {
    match kind {
        TreeKind::L => &mut s.dyn_ltree[i],
        TreeKind::D => &mut s.dyn_dtree[i],
        TreeKind::Bl => &mut s.bl_tree[i],
    }
}

/// Read the current `max_code` for the tree selected by `kind` (the
/// `*_desc.max_code` fields on `DeflateState`).
#[inline]
fn max_code_of(s: &DeflateState, kind: TreeKind) -> i32 {
    match kind {
        TreeKind::L => s.l_desc_max_code,
        TreeKind::D => s.d_desc_max_code,
        TreeKind::Bl => s.bl_desc_max_code,
    }
}

/// Set the `max_code` for the tree selected by `kind`.
#[inline]
fn set_max_code(s: &mut DeflateState, kind: TreeKind, v: i32) {
    match kind {
        TreeKind::L => s.l_desc_max_code = v,
        TreeKind::D => s.d_desc_max_code = v,
        TreeKind::Bl => s.bl_desc_max_code = v,
    }
}

// ===========================================================================
// Phase E — Distance-code mapping and (no-op) static initialization
// ===========================================================================

/// Map a match distance to its distance code (`deflate.h` `d_code` macro).
///
/// For distances below 256 the code is read directly from [`DIST_CODE`]; larger
/// distances are indexed by their top bits.
#[inline]
pub(crate) fn d_code(dist: usize) -> usize {
    if dist < 256 {
        DIST_CODE[dist] as usize
    } else {
        DIST_CODE[256 + (dist >> 7)] as usize
    }
}

/// Initialize the constant tables (`trees.c` `tr_static_init`).
///
/// In C this lazily computes the static trees and the distance/length lookup
/// tables on first use. Here those tables are `const` data transcribed from
/// `trees.h`, so there is nothing to compute; this is a no-op kept to mirror the
/// call from [`DeflateState::tr_init`].
#[inline]
pub(crate) fn tr_static_init() {}

// ===========================================================================
// Phase F — Bit accumulator I/O
// ===========================================================================
//
// `bi_buf`/`bi_valid` form the bit accumulator. Bits are packed LSB-first and
// flushed to `pending_buf` in 16-bit (`put_short`) or 8-bit (`put_byte`)
// units. All values that read `s.bi_buf` first copy it into a local before the
// `put_*` call to keep the borrows unambiguous.

/// Reverse the low `len` bits of `code` (`trees.c` `bi_reverse`).
///
/// Used to turn the canonical (MSB-first) Huffman codes produced by the
/// counting pass into the LSB-first codes DEFLATE writes on the wire.
/// `1 <= len <= 15`.
#[inline]
fn bi_reverse(code: u32, len: i32) -> u32 {
    let mut res: u32 = 0;
    let mut code = code;
    let mut len = len;
    // C `do { ... } while (--len > 0)`: run the body once, then continue while
    // the post-decremented length is still positive.
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

/// Append the low `length` bits of `value` to the bit accumulator
/// (`trees.c` `send_bits`, non-debug form). `length <= 16` and `value` fits in
/// `length` bits.
///
/// `bi_buf` is a 16-bit accumulator (C `ush`). The shifts are computed in `u32`
/// and truncated to `u16`, exactly reproducing C's `ush` wraparound while never
/// shifting a `u16` by 16 (which would be rejected).
#[inline]
fn send_bits(s: &mut DeflateState, value: i32, length: i32) {
    let v: u32 = (value as u16) as u32;
    if s.bi_valid > BUF_SIZE - length {
        // Not enough room: fill the remaining bits of `bi_buf`, flush it, then
        // keep the overflow bits as the new accumulator contents.
        s.bi_buf |= (v << (s.bi_valid as u32)) as u16;
        let b = s.bi_buf;
        s.put_short(b);
        s.bi_buf = (v >> ((BUF_SIZE - s.bi_valid) as u32)) as u16;
        s.bi_valid += length - BUF_SIZE;
    } else {
        s.bi_buf |= (v << (s.bi_valid as u32)) as u16;
        s.bi_valid += length;
    }
}

/// Send a Huffman code that has already been read out of its tree
/// (`trees.c` `send_code`).
///
/// Callers copy the [`HuffmanNode`] (which is `Copy`) out of the relevant tree —
/// the static `const` tables or a `DeflateState` array — before calling this,
/// so the accumulator can be mutated without an aliasing borrow.
#[inline]
fn send_code_node(s: &mut DeflateState, node: HuffmanNode) {
    send_bits(s, node.code() as i32, node.len() as i32);
}

/// Flush the bit buffer, keeping at most 7 bits in it (`trees.c` `bi_flush`).
fn bi_flush(s: &mut DeflateState) {
    if s.bi_valid == 16 {
        let b = s.bi_buf;
        s.put_short(b);
        s.bi_buf = 0;
        s.bi_valid = 0;
    } else if s.bi_valid >= 8 {
        let b = s.bi_buf as u8;
        s.put_byte(b);
        s.bi_buf >>= 8;
        s.bi_valid -= 8;
    }
}

/// Flush the bit buffer and align output on a byte boundary
/// (`trees.c` `bi_windup`).
///
/// Also records `bi_used` — the number of bits used in the final emitted byte —
/// which the FFI boundary exposes via `deflatePending`. For `bi_valid == 0`,
/// `((0 - 1) & 7) + 1 == 8`, matching the C two's-complement arithmetic.
fn bi_windup(s: &mut DeflateState) {
    if s.bi_valid > 8 {
        let b = s.bi_buf;
        s.put_short(b);
    } else if s.bi_valid > 0 {
        let b = s.bi_buf as u8;
        s.put_byte(b);
    }
    s.bi_used = ((s.bi_valid - 1) & 7) + 1;
    s.bi_buf = 0;
    s.bi_valid = 0;
}

// ===========================================================================
// Phase G — Code generation and block/stream initialization
// ===========================================================================

/// Generate the canonical codes for a tree from its bit-length counts
/// (`trees.c` `gen_codes`).
///
/// The recurrence `code = (code + bl_count[bits - 1]) << 1` assigns the first
/// code at each length; each code is then bit-reversed (DEFLATE writes codes
/// LSB-first) and `next_code[len]` is post-incremented. Preserving this exact
/// recurrence and increment order is what makes the codes match C zlib.
fn gen_codes(tree: &mut [HuffmanNode], max_code: i32, bl_count: &[u16]) {
    // Next code value for each bit length.
    let mut next_code = [0u16; MAX_BITS + 1];
    // Running code value, kept in `u32` like the C `unsigned`.
    let mut code: u32 = 0;
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }
    // Assign a (bit-reversed) code to every element of non-zero length. The
    // signed loop mirrors C's `for (n = 0; n <= max_code; n++)` and is a no-op
    // when `max_code < 0`.
    let mut n: i32 = 0;
    while n <= max_code {
        let len = tree[n as usize].len() as usize;
        if len != 0 {
            let c = bi_reverse(next_code[len] as u32, len as i32) as u16;
            tree[n as usize].set_code(c);
            next_code[len] += 1;
        }
        n += 1;
    }
}

/// Initialize a new block (`trees.c` `init_block`): zero every tree frequency,
/// seed the end-of-block code with frequency 1, and reset the per-block length
/// accounting and symbol cursor.
fn init_block(s: &mut DeflateState) {
    for slot in s.dyn_ltree.iter_mut().take(L_CODES) {
        slot.set_freq(0);
    }
    for slot in s.dyn_dtree.iter_mut().take(D_CODES) {
        slot.set_freq(0);
    }
    for slot in s.bl_tree.iter_mut().take(BL_CODES) {
        slot.set_freq(0);
    }
    s.dyn_ltree[END_BLOCK].set_freq(1);
    s.opt_len = 0;
    s.static_len = 0;
    s.sym_next = 0;
    s.matches = 0;
}

impl DeflateState {
    /// Initialize the Huffman/bit-output state for a new stream (`trees.c`
    /// `_tr_init`), called from `deflateReset`.
    ///
    /// The C version wires up the `tree_desc` pointers; here the static
    /// descriptors are `const` data selected by [`TreeKind`], so this only
    /// resets the mutable per-tree `max_code` fields, clears the bit
    /// accumulator, and initializes the first block.
    pub(crate) fn tr_init(&mut self) {
        tr_static_init();

        self.l_desc_max_code = 0;
        self.d_desc_max_code = 0;
        self.bl_desc_max_code = 0;

        self.bi_buf = 0;
        self.bi_valid = 0;
        self.bi_used = 0;

        init_block(self);
    }
}

// ===========================================================================
// Phase H — Dynamic Huffman tree construction
// ===========================================================================
//
// The heap stored in `DeflateState::heap` is 1-based (`SMALLEST == 1`;
// `heap[0]` is unused) and holds node indices as `i32`, so every value read out
// of the heap is cast to `usize` before it indexes a tree or `depth` array.
// `heap_max` starts one past the end (`HEAP_SIZE`) and is always pre-decremented
// before use, so it never indexes out of bounds.

/// Heap-ordering predicate (`trees.c` `smaller`): node `n` precedes node `m`
/// when it has the smaller frequency, breaking ties by the smaller subtree
/// depth (`<=` so equal depths keep `n` first, matching C exactly).
#[inline]
fn smaller(tree: &[HuffmanNode], n: usize, m: usize, depth: &[u8]) -> bool {
    let freq_n = tree[n].freq();
    let freq_m = tree[m].freq();
    freq_n < freq_m || (freq_n == freq_m && depth[n] <= depth[m])
}

/// Restore the heap property after the element at index `k` (`trees.c`
/// `pqdownheap`): sift `heap[k]` down, always following the smaller of the two
/// children, until both children are larger (or it becomes a leaf).
///
/// Operates on disjoint borrows (`tree`, `depth`, `heap`) so it can be invoked
/// from [`pqdownheap_s`] without aliasing `DeflateState`.
fn pqdownheap(tree: &[HuffmanNode], depth: &[u8], heap: &mut [i32], heap_len: usize, mut k: usize) {
    let v = heap[k];
    let mut j = k << 1; // left child
    while j <= heap_len {
        // Pick the smaller of the two children.
        if j < heap_len && smaller(tree, heap[j + 1] as usize, heap[j] as usize, depth) {
            j += 1;
        }
        // Stop when `v` is smaller than the smaller child.
        if smaller(tree, v as usize, heap[j] as usize, depth) {
            break;
        }
        heap[k] = heap[j];
        k = j;
        j <<= 1;
    }
    heap[k] = v;
}

/// [`pqdownheap`] over the dynamic tree selected by `kind`, passing the three
/// disjoint `DeflateState` field borrows it needs.
#[inline]
fn pqdownheap_s(s: &mut DeflateState, kind: TreeKind, k: usize) {
    let heap_len = s.heap_len;
    match kind {
        TreeKind::L => pqdownheap(&s.dyn_ltree, &s.depth, &mut s.heap, heap_len, k),
        TreeKind::D => pqdownheap(&s.dyn_dtree, &s.depth, &mut s.heap, heap_len, k),
        TreeKind::Bl => pqdownheap(&s.bl_tree, &s.depth, &mut s.heap, heap_len, k),
    }
}

/// Compute the optimal bit lengths for a tree given its node structure
/// (`trees.c` `gen_bitlen`).
///
/// The first pass walks the heap from the root outward, setting each node's
/// length to its parent's length plus one, counting lengths in `bl_count`, and
/// accumulating the dynamic (`opt_len`) and static (`static_len`) costs. Codes
/// that would exceed `max_length` are clamped and counted as overflow.
///
/// If any code overflowed, the second pass redistributes the overflowing codes
/// to shorter lengths (the canonical "move a leaf down, promote its sibling"
/// loop) and then reassigns every code length in increasing-frequency order so
/// the resulting tree is still a valid prefix code of bounded depth.
///
/// Every term here — the `xbits` extra-bit contribution, the `freq * length`
/// products, and the static-length accumulation — feeds the static-vs-dynamic
/// block decision in [`DeflateState::tr_flush_block`], so the arithmetic is a
/// verbatim port.
fn gen_bitlen(s: &mut DeflateState, kind: TreeKind) {
    let desc = static_desc(kind);
    let stree = desc.static_tree;
    let extra = desc.extra_bits;
    let base = desc.extra_base;
    let max_length = desc.max_length;
    let max_code = max_code_of(s, kind);

    s.bl_count.fill(0);

    // The root of the heap (heaviest subtree) has length zero by definition.
    let root = s.heap[s.heap_max] as usize;
    tnode_mut(s, kind, root).set_len(0);

    let mut overflow: i32 = 0;

    // First pass: derive each node's length from its parent.
    //
    // `h` must outlive this loop — the length-reassignment pass below resumes
    // walking the heap from where this one stopped (`h == HEAP_SIZE`).
    let mut h = s.heap_max + 1;
    while h < HEAP_SIZE {
        let n = s.heap[h] as usize;
        let dad = tnode(s, kind, n).dad() as usize;
        let mut bits = tnode(s, kind, dad).len() as usize + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        // Set the node's length (this also overwrites the Dad slot, which is no
        // longer needed once the length is known).
        tnode_mut(s, kind, n).set_len(bits as u16);

        // Internal nodes (index > max_code) carry no output cost.
        if (n as i32) > max_code {
            h += 1;
            continue;
        }

        s.bl_count[bits] += 1;
        let xbits: usize = if n >= base {
            extra[n - base] as usize
        } else {
            0
        };
        let f = tnode(s, kind, n).freq() as usize;
        s.opt_len = s.opt_len.wrapping_add(f.wrapping_mul(bits + xbits));
        if let Some(st) = stree {
            let stlen = st[n].len() as usize;
            s.static_len = s.static_len.wrapping_add(f.wrapping_mul(stlen + xbits));
        }
        h += 1;
    }
    if overflow == 0 {
        return;
    }

    // Second pass (only when codes overflowed `max_length`): find a code of the
    // longest length below the maximum, demote it, and promote its sibling pair
    // up one level, repeating until the overflow is absorbed.
    loop {
        let mut bits = max_length - 1;
        while s.bl_count[bits] == 0 {
            bits -= 1;
        }
        s.bl_count[bits] -= 1; // one leaf moves down
        s.bl_count[bits + 1] += 2; // move its overflow brother
        s.bl_count[max_length] -= 1;
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // Now recompute the lengths: for every bit length from the maximum down,
    // assign that length to the corresponding number of the least-frequent
    // remaining nodes (walked from the tail of the heap that the first pass
    // left at `h == HEAP_SIZE`).
    for bits in (1..=max_length).rev() {
        let mut n = s.bl_count[bits] as i32;
        while n != 0 {
            h -= 1;
            let m = s.heap[h] as usize;
            if (m as i32) > max_code {
                // Internal node: skip without consuming a code (matches the C
                // `continue`, which bypasses the `n--`).
                continue;
            }
            let mlen = tnode(s, kind, m).len() as usize;
            if mlen != bits {
                let f = tnode(s, kind, m).freq() as usize;
                s.opt_len = s
                    .opt_len
                    .wrapping_add(bits.wrapping_sub(mlen).wrapping_mul(f));
                tnode_mut(s, kind, m).set_len(bits as u16);
            }
            n -= 1;
        }
    }
}

/// Construct one complete Huffman tree from the symbol frequencies
/// (`trees.c` `build_tree`).
///
/// Builds a min-heap of all symbols with non-zero frequency (forcing at least
/// two so the tree is always well-formed), then repeatedly removes the two
/// least-frequent nodes and replaces them with an internal parent until a
/// single tree remains. Finally derives the bit lengths ([`gen_bitlen`]) and the
/// canonical codes ([`gen_codes`]).
///
/// The forced-code step transiently decrements `opt_len`/`static_len` (possibly
/// below zero); [`gen_bitlen`] adds the cost back, hence the wrapping
/// arithmetic.
fn build_tree(s: &mut DeflateState, kind: TreeKind) {
    let desc = static_desc(kind);
    let stree = desc.static_tree;
    let elems = desc.elems;

    let mut max_code: i32 = -1;

    s.heap_len = 0;
    s.heap_max = HEAP_SIZE;

    // Seed the heap with every symbol that actually occurred.
    for n in 0..elems {
        if tnode(s, kind, n).freq() != 0 {
            s.heap_len += 1;
            s.heap[s.heap_len] = n as i32;
            max_code = n as i32;
            s.depth[n] = 0;
        } else {
            tnode_mut(s, kind, n).set_len(0);
        }
    }

    // The pkzip format requires at least two distinct codes. If fewer than two
    // symbols occurred, invent them (preferring the lowest unused symbol, then
    // symbol 0) and back out their phantom cost; `gen_bitlen` restores it.
    while s.heap_len < 2 {
        let node: usize = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        s.heap_len += 1;
        s.heap[s.heap_len] = node as i32;
        tnode_mut(s, kind, node).set_freq(1);
        s.depth[node] = 0;
        s.opt_len = s.opt_len.wrapping_sub(1);
        if let Some(st) = stree {
            s.static_len = s.static_len.wrapping_sub(st[node].len() as usize);
        }
    }
    set_max_code(s, kind, max_code);

    // Build the heap bottom-up: sift down every internal node, largest index
    // first, so the smallest element ends up at `SMALLEST`.
    for n in (1..=s.heap_len / 2).rev() {
        pqdownheap_s(s, kind, n);
    }

    // Combine the two least-frequent nodes repeatedly. `node` numbers the new
    // internal nodes, starting just past the leaves.
    let mut node = elems;
    loop {
        // pqremove: take the smallest node, then re-heapify.
        let n_val = s.heap[SMALLEST];
        s.heap[SMALLEST] = s.heap[s.heap_len];
        s.heap_len -= 1;
        pqdownheap_s(s, kind, SMALLEST);
        let n_idx = n_val as usize;

        // The next-smallest node is now at the top.
        let m_val = s.heap[SMALLEST];
        let m_idx = m_val as usize;

        // Stash both nodes at the (shrinking) top of the heap array; they are
        // emitted from there by `gen_bitlen`.
        s.heap_max -= 1;
        s.heap[s.heap_max] = n_val;
        s.heap_max -= 1;
        s.heap[s.heap_max] = m_val;

        // Create the parent: frequency is the sum, depth is one past the
        // deeper child.
        let freq_n = tnode(s, kind, n_idx).freq();
        let freq_m = tnode(s, kind, m_idx).freq();
        tnode_mut(s, kind, node).set_freq(freq_n.wrapping_add(freq_m));
        let dn = s.depth[n_idx];
        let dm = s.depth[m_idx];
        s.depth[node] = (dn.max(dm) as u32 + 1) as u8;
        tnode_mut(s, kind, n_idx).set_dad(node as u16);
        tnode_mut(s, kind, m_idx).set_dad(node as u16);

        // Put the new parent at the top of the heap and sift it down.
        s.heap[SMALLEST] = node as i32;
        node += 1;
        pqdownheap_s(s, kind, SMALLEST);

        if s.heap_len < 2 {
            break;
        }
    }

    // The last remaining node is the root.
    s.heap_max -= 1;
    let smallest_val = s.heap[SMALLEST];
    s.heap[s.heap_max] = smallest_val;

    // Derive bit lengths, then the canonical codes.
    gen_bitlen(s, kind);
    let mc = max_code_of(s, kind);
    match kind {
        TreeKind::L => gen_codes(&mut s.dyn_ltree, mc, &s.bl_count),
        TreeKind::D => gen_codes(&mut s.dyn_dtree, mc, &s.bl_count),
        TreeKind::Bl => gen_codes(&mut s.bl_tree, mc, &s.bl_count),
    }
}

// ===========================================================================
// Phase I — Bit-length tree scan and emission
// ===========================================================================

/// Add `by` to a node's frequency with wrapping (mirrors C's modular `ush`
/// arithmetic). Reading the current frequency into a local first keeps the
/// immutable read disjoint from the mutable write, so a single `HuffmanNode`
/// can be updated without an aliasing borrow.
#[inline]
fn bump_freq(node: &mut HuffmanNode, by: u16) {
    let f = node.freq().wrapping_add(by);
    node.set_freq(f);
}

/// Scan a literal or distance tree and accumulate the frequencies of the
/// bit-length codes used to describe it (`trees.c` `scan_tree`).
///
/// Runs of equal code lengths are encoded with the repeat codes `REP_3_6`
/// (3–6 copies of a non-zero length), `REPZ_3_10` (3–10 zero lengths), and
/// `REPZ_11_138` (11–138 zero lengths); short runs are emitted literally. This
/// pass only counts the resulting bit-length-tree symbols into `bl_tree`; the
/// run-length thresholds (`max_count`/`min_count`) are switched exactly as in C.
///
/// A guard length of `0xffff` is written one past the last code so the
/// look-ahead `nextlen` never matches a real length at the boundary.
fn scan_tree(s: &mut DeflateState, kind: TreeKind) {
    let max_code = max_code_of(s, kind);

    let mut prevlen: i32 = -1; // last emitted length
    let mut nextlen: i32 = tnode(s, kind, 0).len() as i32; // length of next code
    let mut curlen: i32; // length of current code
    let mut count: i32 = 0; // repeat count of the current code
    let mut max_count: i32 = 7; // max repeat count
    let mut min_count: i32 = 4; // min repeat count

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    // Guard sentinel one past the last code (read as `nextlen`, never indexed).
    tnode_mut(s, kind, (max_code + 1) as usize).set_len(0xffff);

    let mut n: i32 = 0;
    while n <= max_code {
        curlen = nextlen;
        nextlen = tnode(s, kind, (n + 1) as usize).len() as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            n += 1;
            continue;
        } else if count < min_count {
            bump_freq(&mut s.bl_tree[curlen as usize], count as u16);
        } else if curlen != 0 {
            if curlen != prevlen {
                bump_freq(&mut s.bl_tree[curlen as usize], 1);
            }
            bump_freq(&mut s.bl_tree[REP_3_6], 1);
        } else if count <= 10 {
            bump_freq(&mut s.bl_tree[REPZ_3_10], 1);
        } else {
            bump_freq(&mut s.bl_tree[REPZ_11_138], 1);
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
        n += 1;
    }
}

/// Emit a literal or distance tree in compressed form using the bit-length-tree
/// codes (`trees.c` `send_tree`).
///
/// Structurally identical to [`scan_tree`], but instead of counting symbols it
/// emits them: literal lengths via `send_code`, runs via the repeat codes
/// followed by the extra repeat-count bits (2, 3, or 7 bits respectively). The
/// guard sentinel set by [`scan_tree`] is still in place.
fn send_tree(s: &mut DeflateState, kind: TreeKind) {
    let max_code = max_code_of(s, kind);

    let mut prevlen: i32 = -1;
    let mut nextlen: i32 = tnode(s, kind, 0).len() as i32;
    let mut curlen: i32;
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    let mut n: i32 = 0;
    while n <= max_code {
        curlen = nextlen;
        nextlen = tnode(s, kind, (n + 1) as usize).len() as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            n += 1;
            continue;
        } else if count < min_count {
            // Emit `curlen` exactly `count` times.
            loop {
                let node = s.bl_tree[curlen as usize];
                send_code_node(s, node);
                count -= 1;
                if count == 0 {
                    break;
                }
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                let node = s.bl_tree[curlen as usize];
                send_code_node(s, node);
                count -= 1;
            }
            let node = s.bl_tree[REP_3_6];
            send_code_node(s, node);
            send_bits(s, count - 3, 2);
        } else if count <= 10 {
            let node = s.bl_tree[REPZ_3_10];
            send_code_node(s, node);
            send_bits(s, count - 3, 3);
        } else {
            let node = s.bl_tree[REPZ_11_138];
            send_code_node(s, node);
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
        n += 1;
    }
}

/// Build the bit-length tree and return the `bl_order` index of the last
/// bit-length code that must be transmitted (`trees.c` `build_bl_tree`).
///
/// Scans the literal/length and distance trees into `bl_tree`, builds that
/// tree, then trims trailing zero-length codes (the DEFLATE format requires at
/// least four bit-length codes). Finally adds the cost of the bit-length tree
/// header — `3` bits per transmitted code plus the three `5 + 5 + 4`-bit count
/// fields — to `opt_len`.
fn build_bl_tree(s: &mut DeflateState) -> i32 {
    // Determine the bit-length frequencies for the literal and distance trees.
    scan_tree(s, TreeKind::L);
    scan_tree(s, TreeKind::D);

    // Build the bit-length tree itself.
    build_tree(s, TreeKind::Bl);

    // Find the number of bit-length codes to send: drop trailing zero-length
    // codes but keep at least four (indices 0..=3 in `bl_order`).
    let mut max_blindex: i32 = (BL_CODES - 1) as i32;
    while max_blindex >= 3 {
        if s.bl_tree[BL_ORDER[max_blindex as usize] as usize].len() != 0 {
            break;
        }
        max_blindex -= 1;
    }

    // Account for the bit-length tree and the three count fields.
    s.opt_len = s
        .opt_len
        .wrapping_add(3 * (max_blindex as usize + 1) + 5 + 5 + 4);

    max_blindex
}

/// Send the dynamic-block header (`trees.c` `send_all_trees`): the three code
/// counts, the bit-length-code lengths in `bl_order`, then the literal/length
/// and distance trees in compressed form.
///
/// `lcodes >= 257`, `dcodes >= 1`, `blcodes >= 4`. The counts are biased
/// (`-257`, `-1`, `-4`) exactly as the DEFLATE format specifies.
fn send_all_trees(s: &mut DeflateState, lcodes: i32, dcodes: i32, blcodes: i32) {
    send_bits(s, lcodes - 257, 5); // literal/length code count
    send_bits(s, dcodes - 1, 5); // distance code count
    send_bits(s, blcodes - 4, 4); // bit-length code count

    // Bit-length code lengths, in the format-defined `bl_order` permutation.
    for rank in 0..blcodes {
        let len = s.bl_tree[BL_ORDER[rank as usize] as usize].len() as i32;
        send_bits(s, len, 3);
    }

    // The two trees, compressed via the bit-length tree (max codes come from
    // each tree's own descriptor, matching the C `lcodes - 1` / `dcodes - 1`).
    send_tree(s, TreeKind::L);
    send_tree(s, TreeKind::D);
}

// ===========================================================================
// Phase J — Block emission
// ===========================================================================

/// Selects which pair of Huffman trees [`compress_block`] emits with: the fixed
/// static tables or the dynamic trees owned by [`DeflateState`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockTrees {
    /// Use the static literal/length and distance tables.
    Static,
    /// Use the per-block dynamic trees.
    Dynamic,
}

/// Emit one literal/length code, reading it from the static table or the
/// dynamic literal/length tree as selected by `which`.
///
/// The node is copied out (it is `Copy`) before [`send_code_node`] borrows the
/// state mutably, so no aliasing borrow is held.
#[inline]
fn send_lit(s: &mut DeflateState, which: BlockTrees, idx: usize) {
    let node = match which {
        BlockTrees::Static => STATIC_LTREE[idx],
        BlockTrees::Dynamic => s.dyn_ltree[idx],
    };
    send_code_node(s, node);
}

/// Emit one distance code, reading it from the static table or the dynamic
/// distance tree as selected by `which`.
#[inline]
fn send_dist(s: &mut DeflateState, which: BlockTrees, idx: usize) {
    let node = match which {
        BlockTrees::Static => STATIC_DTREE[idx],
        BlockTrees::Dynamic => s.dyn_dtree[idx],
    };
    send_code_node(s, node);
}

/// Emit the buffered LZ77 symbols of the current block using the chosen trees
/// (`trees.c` `compress_block`).
///
/// Each symbol occupies three bytes in the overlaid symbol buffer
/// (distance low, distance high, length code), read back via
/// [`DeflateState::sym_read`]. A zero distance marks a literal; otherwise the
/// length code, its extra bits, the distance code, and its extra bits are
/// emitted in DEFLATE order. The block is terminated with the end-of-block
/// code.
fn compress_block(s: &mut DeflateState, which: BlockTrees) {
    let mut sx: usize = 0; // running byte index in the symbol buffer

    if s.sym_next != 0 {
        loop {
            let (dist, lc) = s.sym_read(sx);
            sx += 3;
            if dist == 0 {
                // Literal byte.
                send_lit(s, which, lc as usize);
            } else {
                // Match: `lc` is (length - MIN_MATCH); send the length code and
                // its extra bits, then the distance code and its extra bits.
                let code = LENGTH_CODE[lc as usize] as usize;
                send_lit(s, which, code + LITERALS + 1);
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    let lc_extra = lc as i32 - BASE_LENGTH[code];
                    send_bits(s, lc_extra, extra);
                }
                let dist = dist as usize - 1; // match distance - 1
                let code = d_code(dist);
                send_dist(s, which, code);
                let extra = EXTRA_DBITS[code];
                if extra != 0 {
                    let dist_extra = dist as i32 - BASE_DIST[code];
                    send_bits(s, dist_extra, extra);
                }
            }
            if sx >= s.sym_next {
                break;
            }
        }
    }

    send_lit(s, which, END_BLOCK);
}

/// Classify the block as text or binary from the literal frequencies
/// (`trees.c` `detect_data_type`).
///
/// Returns [`Z_BINARY`] if any "block-listed" control byte (positions selected
/// by the `0xf3ffc07f` mask: 0..6, 14..25, 28..31) occurred, [`Z_TEXT`] if any
/// "allow-listed" byte (TAB/LF/CR or 32..255) occurred, and [`Z_BINARY`]
/// otherwise (empty or only "gray-listed" bytes).
fn detect_data_type(s: &DeflateState) -> i32 {
    // Bit mask of block-listed bytes: bits 0..6, 14..25, and 28..31 set.
    let mut block_mask: u32 = 0xf3ff_c07f;

    // Any block-listed byte present => binary.
    for slot in &s.dyn_ltree[..32] {
        if (block_mask & 1) != 0 && slot.freq() != 0 {
            return Z_BINARY;
        }
        block_mask >>= 1;
    }

    // TAB (9), LF (10), or CR (13) present => text.
    if s.dyn_ltree[9].freq() != 0 || s.dyn_ltree[10].freq() != 0 || s.dyn_ltree[13].freq() != 0 {
        return Z_TEXT;
    }
    // Any other allow-listed byte (32..=255) present => text.
    for slot in &s.dyn_ltree[32..LITERALS] {
        if slot.freq() != 0 {
            return Z_TEXT;
        }
    }

    Z_BINARY
}

impl DeflateState {
    /// Emit a stored (uncompressed) block (`trees.c` `_tr_stored_block`).
    ///
    /// Writes the 3-bit block header, aligns to a byte boundary, then writes the
    /// 16-bit length and its one's complement followed by the raw bytes. `buf`
    /// is the input window slice to copy out; `pending` always advances by
    /// `stored_len` (matching C, even when `buf` is `None`).
    pub(crate) fn tr_stored_block(&mut self, buf: Option<&[u8]>, stored_len: usize, last: bool) {
        let last_bit = last as i32;
        send_bits(self, (STORED_BLOCK << 1) + last_bit, 3); // block type
        bi_windup(self); // align on byte boundary
        self.put_short(stored_len as u16);
        self.put_short(!(stored_len as u16)); // one's complement length
        if stored_len != 0 {
            if let Some(b) = buf {
                let p = self.pending;
                self.pending_buf[p..p + stored_len].copy_from_slice(&b[..stored_len]);
            }
        }
        self.pending += stored_len;
    }

    /// Flush the bit buffer to pending output, leaving at most 7 bits
    /// (`trees.c` `_tr_flush_bits`). Called from `deflate`'s flush handling.
    pub(crate) fn tr_flush_bits(&mut self) {
        bi_flush(self);
    }

    /// Emit one empty static block (`trees.c` `_tr_align`).
    ///
    /// Used by `Z_SYNC_FLUSH`/`Z_FULL_FLUSH` to give inflate enough look-ahead.
    /// Costs 10 bits, of which up to 7 may remain buffered.
    pub(crate) fn tr_align(&mut self) {
        send_bits(self, STATIC_TREES << 1, 3);
        let node = STATIC_LTREE[END_BLOCK];
        send_code_node(self, node); // end-of-block code
        bi_flush(self);
    }

    /// Choose the best encoding for the current block and write it
    /// (`trees.c` `_tr_flush_block`).
    ///
    /// At `level > 0` this builds the literal/length, distance, and bit-length
    /// trees, computes the dynamic and static block sizes (in bytes), and picks
    /// the smallest of {stored, static, dynamic}. The block-size arithmetic —
    /// the `(bits + 3 + 7) >> 3` rounding, the `static <= dynamic` tie going to
    /// static, the `Z_FIXED` strategy forcing static, and the
    /// `stored_len + 4 <= opt_lenb` stored-block preference — is reproduced
    /// verbatim because it determines the emitted block type.
    ///
    /// `buf` is the input slice for a possible stored block; `stored_len` is its
    /// length; `last` marks the final block of the stream.
    pub(crate) fn tr_flush_block(&mut self, buf: Option<&[u8]>, stored_len: usize, last: bool) {
        let opt_lenb: usize; // dynamic-block size in bytes
        let static_lenb: usize; // static-block size in bytes
        let mut max_blindex: i32 = 0; // last bit-length code index to send
        let last_bit = last as i32;

        // Build the Huffman trees unless a stored block is forced (level 0).
        if self.level > 0 {
            // Classify the data the first time only.
            if self.data_type == Z_UNKNOWN {
                self.data_type = detect_data_type(self);
            }

            // Build the literal/length and distance trees, then the bit-length
            // tree describing them.
            build_tree(self, TreeKind::L);
            build_tree(self, TreeKind::D);
            max_blindex = build_bl_tree(self);

            // Block sizes in bytes (round bits up: + block-type bits + 7).
            let opt_bytes = (self.opt_len + 3 + 7) >> 3;
            let static_bytes = (self.static_len + 3 + 7) >> 3;
            static_lenb = static_bytes;
            // Prefer static on a tie, or when Z_FIXED forces it.
            opt_lenb = if static_bytes <= opt_bytes || self.strategy == Strategy::Fixed as i32 {
                static_bytes
            } else {
                opt_bytes
            };
        } else {
            // Level 0: force a stored block.
            opt_lenb = stored_len + 5;
            static_lenb = stored_len + 5;
        }

        if stored_len + 4 <= opt_lenb && buf.is_some() {
            // A stored block is at least as small: emit it (4 = the two length
            // words).
            self.tr_stored_block(buf, stored_len, last);
        } else if static_lenb == opt_lenb {
            // Static trees are best (or tied).
            send_bits(self, (STATIC_TREES << 1) + last_bit, 3);
            compress_block(self, BlockTrees::Static);
        } else {
            // Dynamic trees are best: send their description, then the data.
            send_bits(self, (DYN_TREES << 1) + last_bit, 3);
            let lcodes = self.l_desc_max_code + 1;
            let dcodes = self.d_desc_max_code + 1;
            send_all_trees(self, lcodes, dcodes, max_blindex + 1);
            compress_block(self, BlockTrees::Dynamic);
        }

        // Reset the block accounting for the next block.
        init_block(self);

        if last {
            bi_windup(self);
        }
    }

    /// Record one LZ77 symbol and update the Huffman frequencies
    /// (`trees.c` `_tr_tally`).
    ///
    /// `dist == 0` records the literal byte `lc`; otherwise `lc` is
    /// `(match_length - MIN_MATCH)` and `dist` is the match distance. Returns
    /// `true` when the symbol buffer is full and the block must be flushed.
    pub(crate) fn tr_tally(&mut self, dist: usize, lc: usize) -> bool {
        self.sym_write_raw(dist as u16, lc as u8);
        if dist == 0 {
            // Literal byte: bump its frequency in the literal/length tree.
            bump_freq(&mut self.dyn_ltree[lc], 1);
        } else {
            self.matches += 1;
            let d = dist - 1; // match distance - 1
            let len_idx = LENGTH_CODE[lc] as usize + LITERALS + 1;
            bump_freq(&mut self.dyn_ltree[len_idx], 1);
            let dc = d_code(d);
            bump_freq(&mut self.dyn_dtree[dc], 1);
        }
        self.sym_next == self.sym_end
    }

    /// Record a literal byte (`deflate.h` `_tr_tally_lit`).
    #[inline]
    pub(crate) fn tr_tally_lit(&mut self, c: u8) -> bool {
        self.tr_tally(0, c as usize)
    }

    /// Record a length/distance match (`deflate.h` `_tr_tally_dist`); `len` is
    /// `(match_length - MIN_MATCH)`.
    #[inline]
    pub(crate) fn tr_tally_dist(&mut self, dist: usize, len: u8) -> bool {
        self.tr_tally(dist, len as usize)
    }
}
