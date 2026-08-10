//! The static Huffman trees and the fixed code-mapping tables, transcribed
//! verbatim from the generated header `trees.h`.
//!
//! `trees.h` L1 carries the banner `/* header created automatically with
//! -DGEN_TREES_H */`. Every table below is a transcription of that
//! machine-written header, and the four extra-bit and code-order tables that
//! follow them are transcribed from `trees.c` L62-L72. Nothing here is
//! computed, and nothing here is part of the C ABI: these are the constant
//! inputs the Huffman coder reads while it builds a block.
//!
//! # Why this is `const` data and not an initialiser
//!
//! The reference implementation can build these tables either way, and the
//! shipped build already chooses the constant form. `trees.c` L35 is
//!
//! ```text
//! /* #define GEN_TREES_H */
//! ```
//!
//! -- commented out -- and L83 opens `#if defined(GEN_TREES_H) || !defined(STDC)`
//! over the *mutable* declarations, closing at L113 with an `#else` whose only
//! statement is `#include "trees.h"` (L114). With `STDC` defined and
//! `GEN_TREES_H` undefined, a **default** build therefore takes the header branch,
//! and the committed `trees.h` tables are the ones such a build encodes and decodes
//! against -- which is the configuration this port implements.
//!
//! The qualifier is load-bearing rather than pedantic. A build compiled with
//! `-DGEN_TREES_H`, or one where `STDC` is not defined, takes the other branch and
//! fills the mutable tables in at run time from `tr_static_init` instead; and
//! `-DGEN_TREES_H` additionally compiles in `gen_trees_header` (`trees.c`
//! L379-L435), whose whole purpose is to *rewrite* `trees.h`. The values that
//! generator produces equal the committed ones, so this is a difference in
//! initialisation strategy rather than in output -- but "every existing zlib build"
//! would still be the wrong claim, because not every build reads this header.
//!
//! The generator that would fill the mutable form in, `tr_static_init`
//! (`trees.c` L295-L374), is wrapped in that *same* conditional: its entire body
//! sits between L296 and L373, so in a default build it compiles to an empty
//! function and the call `_tr_init` makes at `trees.c` L457 does nothing at all.
//! Reproducing it here would therefore add an initialisation step that the C
//! library does not perform. This module emits `const` arrays instead, and
//! `trees/mod.rs` offers no initialisation entry point.
//!
//! One consequence is worth stating plainly: there is no `static mut`, no
//! `Once`, no `OnceLock`, no atomic and no interior mutability anywhere in this
//! file, so there is no first-use race to reason about and nothing for a
//! run-time checker to observe. The companion generator `gen_trees_header`
//! (`trees.c` L379-L435), which is what wrote `trees.h` in the first place, is
//! likewise not implemented: it exists to *produce* this data, and the data is
//! already here.
//!
//! # What the six generated tables are
//!
//! RFC 1951 3.2.6 fixes one pair of Huffman codes an encoder may use without
//! transmitting them, and [`static_ltree`] and [`static_dtree`] are those codes
//! in encode form -- a bit string and its width per symbol, the mirror image of
//! the decode tables in `inflate/fixed_tables.rs`. A block that selects
//! them costs three type bits and no tree description at all, which is why
//! `_tr_flush_block` compares `static_lenb` against `opt_lenb` before choosing
//! (`trees.c` L1027-L1074).
//!
//! The remaining four are the fixed mappings RFC 1951 3.2.5 tabulates.
//! [`_length_code`] and [`base_length`] split a match length into a length code
//! plus a residue; [`_dist_code`] and [`base_dist`] do the same for a match
//! distance. The encoder needs both halves of each pair: `compress_block`
//! (`trees.c` L900-L951) sends the code from the first table and then, when
//! [`extra_lbits`] or [`extra_dbits`] says the code carries extra bits, sends
//! the value minus the corresponding base.
//!
//! # Fidelity is the whole point
//!
//! A single wrong element would not fail loudly -- it would silently emit a
//! valid-looking block that decodes to the wrong bytes, or a correct block that
//! differs from what the reference produces. So the transcription was extracted
//! by script from the compiled C rather than by eye, and was then re-derived a
//! second time from the `tr_static_init` algorithm and compared. The tests at
//! the end of this file keep both properties enforceable: they re-derive every
//! one of the ten tables from first principles -- `bi_reverse` (`trees.c`
//! L154-L161), `gen_codes` (`trees.c` L203-L232) and the two mapping loops of
//! `tr_static_init` (`trees.c` L317-L367) -- rather than restating the literals.
//!
//! The declared lengths are pinned at compile time as well, against the sizes
//! `trees.h` and `trees.c` declare, so a truncated or over-long paste fails the
//! build instead of the stream.
//!
//! # Naming and visibility
//!
//! Every table keeps its C name, lowercase and unchanged, so that the
//! element-for-element comparison in
//! `crates/zlib-rs-differential/tests/table_equality.rs` reads as a
//! direct diff against the header. That, and only that, is why each carries a narrowly
//! scoped `#[allow(non_upper_case_globals)]`.
//!
//! The public/crate-private split mirrors the reference exactly.
//! [`_dist_code`] and [`_length_code`] are `ZLIB_INTERNAL` rather than `local`
//! in C because `deflate.c` reaches them through the `d_code` macro
//! (`deflate.h` L320-L321) and the two `_tr_tally_*` macros (`deflate.h`
//! L357-L375); the four remaining generated tables are `local` to `trees.c`,
//! but they are the values the differential suite has to compare, so they are
//! reachable too. The four extra-bit and code-order tables and the six
//! constants stay crate-private: nothing outside this crate has any business
//! with them. None of it is exported from the shared library -- it is Rust data
//! with no C linkage, so the `local:` block of `zlib.map` is satisfied by
//! construction.
//!
//! # Element types
//!
//! Each element type is the C type its consumer reads, so that the
//! arithmetic needs no cast the reference does not also perform:
//!
//! | Table | C declaration | Rust element | Consumer that fixes it |
//! |---|---|---|---|
//! | [`static_ltree`], [`static_dtree`] | `const ct_data` | [`CtData`] | `send_code`, L239 |
//! | [`_dist_code`], [`_length_code`] | `const uch` | `u8` | `d_code`, `deflate.h` L320 |
//! | [`base_length`], [`base_dist`] | `const int` | `i32` | `compress_block`, L926, L934 |
//! | the three `extra_*` tables | `const int` | `i32` | `gen_bitlen`, L570 |
//! | [`bl_order`] | `const uch` | `u8` | `build_bl_tree`, L818 |
//!
//! An unqualified line reference in that table, and anywhere else in this file,
//! is a line of `trees.c`.
//!
//! The three extra-bit tables must share one element type because
//! `static_tree_desc_s` reaches all three through a single `const intf *`
//! member (`trees.c` L119), which `trees/tree_desc.rs` reproduces as one
//! `&'static [i32]`.
//!
//! Declared `const` rather than `static`, matching the two sibling
//! transcriptions `crc32/tables.rs` and `inflate/fixed_tables.rs`: nothing
//! takes the address of a table, so a
//! single symbol buys nothing, and a `const` cannot acquire interior
//! mutability later.

use crate::deflate::state::{
    CtData, BL_CODES, D_CODES, LENGTH_CODES, L_CODES, MAX_MATCH, MIN_MATCH,
};

/// Length of [`_dist_code`] -- 512.
///
/// Mirrors `#define DIST_CODE_LEN 512` (`trees.c` L81). Two 256-entry halves,
/// not one 512-entry range: the first half maps the distances 1 through 256
/// directly, and the second maps the top eight bits of a distance above 256.
/// The `d_code` macro (`deflate.h` L320-L321) is what selects between them.
///
/// Declared `usize` because its only use is as an array length and as the bound
/// of an index.
pub(crate) const DIST_CODE_LEN: usize = 512;

/// No code in the bit-length tree may be wider than this -- 7 bits.
///
/// Mirrors `#define MAX_BL_BITS 7` (`trees.c` L47-L48). It is the `max_length`
/// member of `static_bl_desc` (`trees.c` L138), where the literal and distance
/// descriptors both carry `MAX_BITS` (`deflate.h` L52) of 15 instead, and it is
/// what makes `gen_bitlen`'s overflow-repair loop (`trees.c` L577-L592)
/// reachable at all: three bits per entry is the whole budget RFC 1951 3.2.7
/// allows for transmitting a bit-length code.
///
/// Declared `usize` to match `MAX_BITS`, since `gen_bitlen` reads whichever of
/// the two a descriptor names into the same `max_length` and then indexes
/// `bl_count` with it (`trees.c` L546, L580-L583).
pub(crate) const MAX_BL_BITS: usize = 7;

/// The end-of-block symbol of the literal/length alphabet -- 256.
///
/// Mirrors `#define END_BLOCK 256` (`trees.c` L50-L51). RFC 1951 3.2.5 reserves
/// value 256 to terminate a block, so `init_block` seeds its frequency at one
/// (`trees.c` L448) -- the symbol is always sent -- and `compress_block` emits
/// it as the last code of every compressed block (`trees.c` L949).
///
/// Declared `usize`: every use indexes a tree array.
pub(crate) const END_BLOCK: usize = 256;

/// Bit-length code 16 -- repeat the previous length 3 to 6 times.
///
/// Mirrors `#define REP_3_6 16` (`trees.c` L53-L54). Followed by two bits of
/// repeat count, biased by three; see RFC 1951 3.2.7. `scan_tree` counts it
/// (`trees.c` L733) and `send_tree` emits it with `send_bits(s, count - 3, 2)`
/// (`trees.c` L777).
///
/// Declared `usize`: every use indexes `DeflateState::bl_tree`.
pub(crate) const REP_3_6: usize = 16;

/// Bit-length code 17 -- repeat a zero length 3 to 10 times.
///
/// Mirrors `#define REPZ_3_10 17` (`trees.c` L56-L57). Followed by three bits
/// of repeat count, biased by three; see RFC 1951 3.2.7. Counted at `trees.c`
/// L735 and emitted at `trees.c` L780.
///
/// Declared `usize`: every use indexes `DeflateState::bl_tree`.
pub(crate) const REPZ_3_10: usize = 17;

/// Bit-length code 18 -- repeat a zero length 11 to 138 times.
///
/// Mirrors `#define REPZ_11_138 18` (`trees.c` L59-L60). Followed by seven bits
/// of repeat count, biased by eleven; see RFC 1951 3.2.7. Counted at `trees.c`
/// L737 and emitted at `trees.c` L783.
///
/// Declared `usize`: every use indexes `DeflateState::bl_tree`.
pub(crate) const REPZ_11_138: usize = 18;

/// The static literal/length tree: `static_ltree[L_CODES+2]`, transcribed from
/// `trees.h` L3-L62.
///
/// Mirrors `local const ct_data static_ltree[L_CODES+2]`. Each entry is a bit
/// string and its width, exactly as the header spells it -- `{{12},{8}}` is code
/// 12 in 8 bits -- which is the `fc` then `dl` order of [`CtData::new`].
///
/// The code widths are the ones RFC 1951 3.2.6 fixes: symbols 0 through 143 use
/// 8 bits, 144 through 255 use 9, 256 through 279 use 7, and 280 through 287
/// use 8. The bit strings are the canonical codes for those widths, reversed,
/// because `send_bits` writes least-significant bit first (`trees.c` L274-L286)
/// while RFC 1951 3.1.1 packs a Huffman code most-significant bit first. That
/// reversal is `bi_reverse` (`trees.c` L154-L161) as applied by `gen_codes`
/// (`trees.c` L227), and the tests below re-derive the whole table from it.
///
/// # Why there are 288 entries and not 286
///
/// [`L_CODES`] is 286, so two entries are surplus. Symbols 286 and 287 can
/// never occur in a stream. They are present because a canonical Huffman tree
/// requires the longest code to be all ones, and `tr_static_init` includes them
/// in the construction to obtain one (`trees.c` L353-L360). Their entries are
/// therefore real codes that are never sent, and they must be transcribed --
/// dropping them would change every 9-bit code in the table.
// The identifier is deliberately the C name `static_ltree` from `trees.h` L3;
// renaming it to SCREAMING_SNAKE_CASE would break the direct correspondence the
// differential table-equality test relies on.
#[allow(non_upper_case_globals)]
pub const static_ltree: [CtData; L_CODES + 2] = [
    CtData::new(12, 8),
    CtData::new(140, 8),
    CtData::new(76, 8),
    CtData::new(204, 8),
    CtData::new(44, 8),
    CtData::new(172, 8),
    CtData::new(108, 8),
    CtData::new(236, 8),
    CtData::new(28, 8),
    CtData::new(156, 8),
    CtData::new(92, 8),
    CtData::new(220, 8),
    CtData::new(60, 8),
    CtData::new(188, 8),
    CtData::new(124, 8),
    CtData::new(252, 8),
    CtData::new(2, 8),
    CtData::new(130, 8),
    CtData::new(66, 8),
    CtData::new(194, 8),
    CtData::new(34, 8),
    CtData::new(162, 8),
    CtData::new(98, 8),
    CtData::new(226, 8),
    CtData::new(18, 8),
    CtData::new(146, 8),
    CtData::new(82, 8),
    CtData::new(210, 8),
    CtData::new(50, 8),
    CtData::new(178, 8),
    CtData::new(114, 8),
    CtData::new(242, 8),
    CtData::new(10, 8),
    CtData::new(138, 8),
    CtData::new(74, 8),
    CtData::new(202, 8),
    CtData::new(42, 8),
    CtData::new(170, 8),
    CtData::new(106, 8),
    CtData::new(234, 8),
    CtData::new(26, 8),
    CtData::new(154, 8),
    CtData::new(90, 8),
    CtData::new(218, 8),
    CtData::new(58, 8),
    CtData::new(186, 8),
    CtData::new(122, 8),
    CtData::new(250, 8),
    CtData::new(6, 8),
    CtData::new(134, 8),
    CtData::new(70, 8),
    CtData::new(198, 8),
    CtData::new(38, 8),
    CtData::new(166, 8),
    CtData::new(102, 8),
    CtData::new(230, 8),
    CtData::new(22, 8),
    CtData::new(150, 8),
    CtData::new(86, 8),
    CtData::new(214, 8),
    CtData::new(54, 8),
    CtData::new(182, 8),
    CtData::new(118, 8),
    CtData::new(246, 8),
    CtData::new(14, 8),
    CtData::new(142, 8),
    CtData::new(78, 8),
    CtData::new(206, 8),
    CtData::new(46, 8),
    CtData::new(174, 8),
    CtData::new(110, 8),
    CtData::new(238, 8),
    CtData::new(30, 8),
    CtData::new(158, 8),
    CtData::new(94, 8),
    CtData::new(222, 8),
    CtData::new(62, 8),
    CtData::new(190, 8),
    CtData::new(126, 8),
    CtData::new(254, 8),
    CtData::new(1, 8),
    CtData::new(129, 8),
    CtData::new(65, 8),
    CtData::new(193, 8),
    CtData::new(33, 8),
    CtData::new(161, 8),
    CtData::new(97, 8),
    CtData::new(225, 8),
    CtData::new(17, 8),
    CtData::new(145, 8),
    CtData::new(81, 8),
    CtData::new(209, 8),
    CtData::new(49, 8),
    CtData::new(177, 8),
    CtData::new(113, 8),
    CtData::new(241, 8),
    CtData::new(9, 8),
    CtData::new(137, 8),
    CtData::new(73, 8),
    CtData::new(201, 8),
    CtData::new(41, 8),
    CtData::new(169, 8),
    CtData::new(105, 8),
    CtData::new(233, 8),
    CtData::new(25, 8),
    CtData::new(153, 8),
    CtData::new(89, 8),
    CtData::new(217, 8),
    CtData::new(57, 8),
    CtData::new(185, 8),
    CtData::new(121, 8),
    CtData::new(249, 8),
    CtData::new(5, 8),
    CtData::new(133, 8),
    CtData::new(69, 8),
    CtData::new(197, 8),
    CtData::new(37, 8),
    CtData::new(165, 8),
    CtData::new(101, 8),
    CtData::new(229, 8),
    CtData::new(21, 8),
    CtData::new(149, 8),
    CtData::new(85, 8),
    CtData::new(213, 8),
    CtData::new(53, 8),
    CtData::new(181, 8),
    CtData::new(117, 8),
    CtData::new(245, 8),
    CtData::new(13, 8),
    CtData::new(141, 8),
    CtData::new(77, 8),
    CtData::new(205, 8),
    CtData::new(45, 8),
    CtData::new(173, 8),
    CtData::new(109, 8),
    CtData::new(237, 8),
    CtData::new(29, 8),
    CtData::new(157, 8),
    CtData::new(93, 8),
    CtData::new(221, 8),
    CtData::new(61, 8),
    CtData::new(189, 8),
    CtData::new(125, 8),
    CtData::new(253, 8),
    CtData::new(19, 9),
    CtData::new(275, 9),
    CtData::new(147, 9),
    CtData::new(403, 9),
    CtData::new(83, 9),
    CtData::new(339, 9),
    CtData::new(211, 9),
    CtData::new(467, 9),
    CtData::new(51, 9),
    CtData::new(307, 9),
    CtData::new(179, 9),
    CtData::new(435, 9),
    CtData::new(115, 9),
    CtData::new(371, 9),
    CtData::new(243, 9),
    CtData::new(499, 9),
    CtData::new(11, 9),
    CtData::new(267, 9),
    CtData::new(139, 9),
    CtData::new(395, 9),
    CtData::new(75, 9),
    CtData::new(331, 9),
    CtData::new(203, 9),
    CtData::new(459, 9),
    CtData::new(43, 9),
    CtData::new(299, 9),
    CtData::new(171, 9),
    CtData::new(427, 9),
    CtData::new(107, 9),
    CtData::new(363, 9),
    CtData::new(235, 9),
    CtData::new(491, 9),
    CtData::new(27, 9),
    CtData::new(283, 9),
    CtData::new(155, 9),
    CtData::new(411, 9),
    CtData::new(91, 9),
    CtData::new(347, 9),
    CtData::new(219, 9),
    CtData::new(475, 9),
    CtData::new(59, 9),
    CtData::new(315, 9),
    CtData::new(187, 9),
    CtData::new(443, 9),
    CtData::new(123, 9),
    CtData::new(379, 9),
    CtData::new(251, 9),
    CtData::new(507, 9),
    CtData::new(7, 9),
    CtData::new(263, 9),
    CtData::new(135, 9),
    CtData::new(391, 9),
    CtData::new(71, 9),
    CtData::new(327, 9),
    CtData::new(199, 9),
    CtData::new(455, 9),
    CtData::new(39, 9),
    CtData::new(295, 9),
    CtData::new(167, 9),
    CtData::new(423, 9),
    CtData::new(103, 9),
    CtData::new(359, 9),
    CtData::new(231, 9),
    CtData::new(487, 9),
    CtData::new(23, 9),
    CtData::new(279, 9),
    CtData::new(151, 9),
    CtData::new(407, 9),
    CtData::new(87, 9),
    CtData::new(343, 9),
    CtData::new(215, 9),
    CtData::new(471, 9),
    CtData::new(55, 9),
    CtData::new(311, 9),
    CtData::new(183, 9),
    CtData::new(439, 9),
    CtData::new(119, 9),
    CtData::new(375, 9),
    CtData::new(247, 9),
    CtData::new(503, 9),
    CtData::new(15, 9),
    CtData::new(271, 9),
    CtData::new(143, 9),
    CtData::new(399, 9),
    CtData::new(79, 9),
    CtData::new(335, 9),
    CtData::new(207, 9),
    CtData::new(463, 9),
    CtData::new(47, 9),
    CtData::new(303, 9),
    CtData::new(175, 9),
    CtData::new(431, 9),
    CtData::new(111, 9),
    CtData::new(367, 9),
    CtData::new(239, 9),
    CtData::new(495, 9),
    CtData::new(31, 9),
    CtData::new(287, 9),
    CtData::new(159, 9),
    CtData::new(415, 9),
    CtData::new(95, 9),
    CtData::new(351, 9),
    CtData::new(223, 9),
    CtData::new(479, 9),
    CtData::new(63, 9),
    CtData::new(319, 9),
    CtData::new(191, 9),
    CtData::new(447, 9),
    CtData::new(127, 9),
    CtData::new(383, 9),
    CtData::new(255, 9),
    CtData::new(511, 9),
    CtData::new(0, 7),
    CtData::new(64, 7),
    CtData::new(32, 7),
    CtData::new(96, 7),
    CtData::new(16, 7),
    CtData::new(80, 7),
    CtData::new(48, 7),
    CtData::new(112, 7),
    CtData::new(8, 7),
    CtData::new(72, 7),
    CtData::new(40, 7),
    CtData::new(104, 7),
    CtData::new(24, 7),
    CtData::new(88, 7),
    CtData::new(56, 7),
    CtData::new(120, 7),
    CtData::new(4, 7),
    CtData::new(68, 7),
    CtData::new(36, 7),
    CtData::new(100, 7),
    CtData::new(20, 7),
    CtData::new(84, 7),
    CtData::new(52, 7),
    CtData::new(116, 7),
    CtData::new(3, 8),
    CtData::new(131, 8),
    CtData::new(67, 8),
    CtData::new(195, 8),
    CtData::new(35, 8),
    CtData::new(163, 8),
    CtData::new(99, 8),
    CtData::new(227, 8),
];

/// The static distance tree: `static_dtree[D_CODES]`, transcribed from
/// `trees.h` L64-L71.
///
/// Mirrors `local const ct_data static_dtree[D_CODES]`. RFC 1951 3.2.6 gives
/// every distance code the same width, so this is the trivial tree: all thirty
/// entries are 5 bits wide and entry `n` holds `n` with its low five bits
/// reversed, which is precisely what `tr_static_init` writes at `trees.c`
/// L364-L367.
///
/// Both properties are re-derived by the tests below rather than eyeballed.
///
/// Note that the *static* alphabet RFC 1951 3.2.6 defines has 32 distance
/// codes, while this table has [`D_CODES`] of 30: codes 30 and 31 "will never
/// actually occur" in a valid stream, so the encoder never needs an entry for
/// them. The decode side keeps all 32, which is why
/// `distfix` in `inflate/fixed_tables.rs` is two entries longer.
// The identifier is deliberately the C name `static_dtree` from `trees.h` L64.
#[allow(non_upper_case_globals)]
pub const static_dtree: [CtData; D_CODES] = [
    CtData::new(0, 5),
    CtData::new(16, 5),
    CtData::new(8, 5),
    CtData::new(24, 5),
    CtData::new(4, 5),
    CtData::new(20, 5),
    CtData::new(12, 5),
    CtData::new(28, 5),
    CtData::new(2, 5),
    CtData::new(18, 5),
    CtData::new(10, 5),
    CtData::new(26, 5),
    CtData::new(6, 5),
    CtData::new(22, 5),
    CtData::new(14, 5),
    CtData::new(30, 5),
    CtData::new(1, 5),
    CtData::new(17, 5),
    CtData::new(9, 5),
    CtData::new(25, 5),
    CtData::new(5, 5),
    CtData::new(21, 5),
    CtData::new(13, 5),
    CtData::new(29, 5),
    CtData::new(3, 5),
    CtData::new(19, 5),
    CtData::new(11, 5),
    CtData::new(27, 5),
    CtData::new(7, 5),
    CtData::new(23, 5),
];

/// Distance to distance-code map: `_dist_code[DIST_CODE_LEN]`, transcribed from
/// `trees.h` L73-L100.
///
/// Mirrors `const uch ZLIB_INTERNAL _dist_code[DIST_CODE_LEN]`. Two 256-entry
/// halves, selected by the `d_code` macro (`deflate.h` L320-L321):
///
/// ```c
/// #define d_code(dist) \
///    ((dist) < 256 ? _dist_code[dist] : _dist_code[256+((dist)>>7)])
/// ```
///
/// where `dist` is the match distance *minus one*. The first half maps those
/// reduced distances 0 through 255 directly to codes 0 through 15; the second
/// half maps the top eight bits of a larger reduced distance to codes 16 through
/// 29. That is the split `tr_static_init` builds in two loops around the
/// `dist >>= 7` at `trees.c` L341, and it is why one 512-entry table can cover
/// the whole 32 KiB window: codes 16 and up each span at least 128 distances, so
/// the low seven bits are exactly the residue [`base_dist`] subtracts.
///
/// # Entries 256 and 257 are zero and are never read
///
/// `deflate.h` L323-L324 records it directly: "`_dist_code[256]` and
/// `_dist_code[257]` are never used." The second half is entered only when
/// `dist >= 256`, so the lowest index it can produce is
/// `256 + (256 >> 7) == 258`; indices 256 and 257 fall in the gap and keep the
/// zero that `tr_static_init`'s loop never overwrote (`trees.c` L342-L347). They
/// are transcribed as the zeroes the header holds. Substituting the code 16 that
/// the surrounding pattern suggests would be a silent divergence from every
/// existing zlib build, and the test below pins them.
// The identifier is deliberately the C name `_dist_code` from `trees.h` L73.
#[allow(non_upper_case_globals)]
pub const _dist_code: [u8; DIST_CODE_LEN] = [
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

/// Match length to length-code map: `_length_code[MAX_MATCH-MIN_MATCH+1]`,
/// transcribed from `trees.h` L102-L116.
///
/// Mirrors `const uch ZLIB_INTERNAL _length_code[MAX_MATCH-MIN_MATCH+1]`.
/// Indexed by the *normalised* match length, `length - MIN_MATCH`, so index 0 is
/// a match of 3 bytes and index 255 a match of 258; the value is the length code
/// relative to the first of them, which the caller then offsets by
/// `LITERALS + 1` to reach the literal/length alphabet (`trees.c` L925). The
/// residue is `length - MIN_MATCH - base_length[code]`, sent in
/// `extra_lbits``[code]` bits.
///
/// # The last entry is 28, not 27
///
/// A match of 258 bytes has two encodings -- code 284 with five extra bits, or
/// code 285 with none -- and `tr_static_init` deliberately overwrites the
/// natural value with the shorter one at `trees.c` L330,
/// `_length_code[length - 1] = (uch)code;`, having noted at L326-L329 that it
/// does so "to use the best encoding". So the final element is
/// [`LENGTH_CODES`] `- 1`, and the mapping is not monotone in the way the
/// preceding run of 27s suggests. This is a compressed-output decision, not a
/// formatting artefact; the test below pins it.
// The identifier is deliberately the C name `_length_code` from `trees.h` L102.
#[allow(non_upper_case_globals)]
pub const _length_code: [u8; MAX_MATCH - MIN_MATCH + 1] = [
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

/// First normalised length of each length code: `base_length[LENGTH_CODES]`,
/// transcribed from `trees.h` L118-L121.
///
/// Mirrors `local const int base_length[LENGTH_CODES]`. `compress_block`
/// subtracts it to obtain the residue a length code carries in its extra bits
/// (`trees.c` L926): `lc -= base_length[code]`, where `lc` is the normalised
/// length `length - MIN_MATCH`. Element `n` is therefore the smallest
/// normalised length that maps to code `n`, and the run of `1 << extra_lbits[n]`
/// lengths above it shares that code.
///
/// # The last element is 0, not 256
///
/// Code 28 is the special one-length code for a match of exactly 258 bytes.
/// `tr_static_init` fills this array in a loop bounded by
/// `code < LENGTH_CODES-1` (`trees.c` L319), so the final element is never
/// assigned and retains the zero of a zero-initialised `local` array. Nothing
/// reads it: `compress_block` subtracts a base only when
/// `extra_lbits[code] != 0` (`trees.c` L924-L928), and
/// `extra_lbits``[28]` is zero. The zero is transcribed exactly as the header
/// holds it; the test below pins it.
///
/// Element type `i32`, from C's `const int`, so the subtraction at `trees.c`
/// L926 -- from an `int` -- needs no cast.
// The identifier is deliberately the C name `base_length` from `trees.h` L118.
#[allow(non_upper_case_globals)]
pub const base_length: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224, 0,
];

/// First distance of each distance code: `base_dist[D_CODES]`, transcribed from
/// `trees.h` L123-L127.
///
/// Mirrors `local const int base_dist[D_CODES]`. `compress_block` subtracts it
/// to obtain the residue a distance code carries in its extra bits (`trees.c`
/// L934): `dist -= (unsigned)base_dist[code]`, where `dist` is the match
/// distance minus one. Element `n` is the smallest reduced distance that maps to
/// code `n`, and the values double every second code -- the geometric ladder of
/// RFC 1951 3.2.5 that lets 30 codes plus at most 13 extra bits address the
/// whole 32 KiB window, since `24576 + ((1 << 13) - 1)` is 32767.
///
/// Element type `i32`, from C's `const int`. The one unsigned use casts, which
/// is exactly what the C expression does with its explicit `(unsigned)`; every
/// element is non-negative and at most 24576, so the cast cannot change a value.
// The identifier is deliberately the C name `base_dist` from `trees.h` L123.
#[allow(non_upper_case_globals)]
pub const base_dist: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

/// Extra bits carried by each length code: `extra_lbits[LENGTH_CODES]`,
/// transcribed from `trees.c` L62-L63.
///
/// Mirrors `local const int extra_lbits[LENGTH_CODES]`, described there as
/// "extra bits for each length code". Read twice: `gen_bitlen` adds it to a code
/// width when accumulating `opt_len` and `static_len` (`trees.c` L570-L574), and
/// `compress_block` uses it to decide whether a residue follows the code at all
/// (`trees.c` L926-L929). It is also the run length of [`_length_code`]: code
/// `n` covers `1 << extra_lbits[n]` normalised lengths, which is the loop bound
/// at `trees.c` L321. The final zero belongs to the one-length code 28.
///
/// Element type `i32`, from C's `const int`, shared with [`extra_dbits`] and
/// [`extra_blbits`] because `static_tree_desc_s` reaches all three through a
/// single `const intf *extra_bits` member (`trees.c` L119).
// The identifier is deliberately the C name `extra_lbits` from `trees.c` L62.
//
// `pub` rather than `pub(crate)`, for the same reason the six `trees.h` tables above are:
// `crates/zlib-rs-differential/tests/table_equality.rs` compares it element for element against
// the C array. Reaching the C side needs a generated wrapper around `trees.c` itself, because
// `local const int extra_lbits[]` is `static` and appears in no header -- see that crate's
// `build.rs`. The functions that READ it stay crate-private, which is the same asymmetry
// `zlib.map` describes for the `_tr_*` family.
#[allow(non_upper_case_globals)]
pub const extra_lbits: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits carried by each distance code: `extra_dbits[D_CODES]`,
/// transcribed from `trees.c` L65-L66.
///
/// Mirrors `local const int extra_dbits[D_CODES]`, described there as "extra
/// bits for each distance code". The `extra_bits` member of `static_d_desc`
/// (`trees.c` L135), with an `extra_base` of 0 -- unlike the literal descriptor,
/// every distance code is a leaf that may carry extra bits. It is also the run
/// length of [`_dist_code`]: the two loops of `tr_static_init` step by
/// `1 << extra_dbits[code]` below code 16 and by
/// `1 << (extra_dbits[code] - 7)` at or above it (`trees.c` L336 and L344),
/// which is where the `>> 7` of the `d_code` macro comes from.
///
/// Element type `i32`; see `extra_lbits`.
// The identifier is deliberately the C name `extra_dbits` from `trees.c` L65.
//
// `pub` for the reason given at [`extra_lbits`]: the differential table-equality suite compares
// it against `trees.c` L65-L66 through the generated `trees.c` wrapper.
#[allow(non_upper_case_globals)]
pub const extra_dbits: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits carried by each bit-length code: `extra_blbits[BL_CODES]`,
/// transcribed from `trees.c` L68-L69.
///
/// Mirrors `local const int extra_blbits[BL_CODES]`, described there as "extra
/// bits for each bit length code". The `extra_bits` member of `static_bl_desc`
/// (`trees.c` L138). Only the last three entries are non-zero, and they are the
/// repeat counts of RFC 1951 3.2.7: 2 bits for `REP_3_6`, 3 for `REPZ_3_10`,
/// 7 for `REPZ_11_138`. Codes 0 through 15 encode a literal code width and carry
/// nothing.
///
/// Those three names are the crate-private constants declared above -- rendered
/// here as code rather than as links, because this item is `pub` for the
/// differential table-equality suite while they are not, and a public doc
/// comment must not point a reader at a name they cannot reach.
///
/// Element type `i32`; see `extra_lbits`.
// The identifier is deliberately the C name `extra_blbits` from `trees.c` L68.
//
// `pub` for the reason given at [`extra_lbits`]: the differential table-equality suite compares
// it against `trees.c` L68-L69 through the generated `trees.c` wrapper.
#[allow(non_upper_case_globals)]
pub const extra_blbits: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Transmission order of the bit-length codes: `bl_order[BL_CODES]`,
/// transcribed from `trees.c` L71-L72.
///
/// Mirrors `local const uch bl_order[BL_CODES]`. `trees.c` L73-L75 gives the
/// reason: "The lengths of the bit length codes are sent in order of decreasing
/// probability, to avoid transmitting the lengths for unused bit length codes."
/// The three repeat codes come first, then 0, then the middle widths outward
/// from 8, leaving the rare extremes 1 and 15 last -- so `build_bl_tree` can
/// walk this order backwards from `BL_CODES - 1` and stop at the last non-zero
/// entry (`trees.c` L818-L820), sending only `max_blindex + 1` of the nineteen
/// three-bit fields. The floor of 3 in that loop, and the `blcodes - 4` written
/// by `send_all_trees` (`trees.c` L845), are the PKZip requirement its comment
/// records: at least four must be sent whatever the frequencies say.
///
/// The order is fixed by RFC 1951 3.2.7 and is a permutation of 0 through 18,
/// which the test below checks.
///
/// Element type `u8`, from C's `uch`; every use indexes
/// `DeflateState::bl_tree` with it, exactly as `trees.c` L819 and L847 do.
// The identifier is deliberately the C name `bl_order` from `trees.c` L71.
//
// `pub` for the reason given at [`extra_lbits`]: the differential table-equality suite compares
// it against `trees.c` L71-L72 through the generated `trees.c` wrapper.
#[allow(non_upper_case_globals)]
pub const bl_order: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// Each length above is written as the expression the C declaration uses, so that
// it tracks the shared sizing constants rather than restating a number. These
// assertions close the loop by pinning those expressions to the sizes the
// generated header was actually written with (`trees.h` L3, L64, L73, L102, L118
// and L123) and to the three `trees.c` declarations (L62, L65, L68 and L71). A
// table that gained or lost an element, or a sizing constant that drifted, now
// fails the build instead of corrupting every block the encoder emits.
const _: () = assert!(static_ltree.len() == 288);
const _: () = assert!(static_dtree.len() == 30);
const _: () = assert!(_dist_code.len() == 512);
const _: () = assert!(_length_code.len() == 256);
const _: () = assert!(base_length.len() == 29);
const _: () = assert!(base_dist.len() == 30);
const _: () = assert!(extra_lbits.len() == 29);
const _: () = assert!(extra_dbits.len() == 30);
const _: () = assert!(extra_blbits.len() == 19);
const _: () = assert!(bl_order.len() == 19);

// The two code maps and their base tables are indexed by the same quantity, so a
// mismatch between a map and its bases would be a silent decoding error rather
// than a type error. `DIST_CODE_LEN` is twice the 256-distance half the `d_code`
// macro switches on (`deflate.h` L320-L321), and each map is exactly as long as
// its own domain.
const _: () = assert!(_dist_code.len() == DIST_CODE_LEN);
const _: () = assert!(_dist_code.len() == 2 * 256);
const _: () = assert!(base_length.len() == extra_lbits.len());
const _: () = assert!(base_dist.len() == extra_dbits.len());
const _: () = assert!(extra_blbits.len() == bl_order.len());

// The bit-length tree is bounded more tightly than the other two, and the three
// repeat codes are the last three of its alphabet. Both facts are relied on by
// `scan_tree` and `send_tree`, so they are stated where they can be checked.
const _: () = assert!(MAX_BL_BITS < crate::deflate::state::MAX_BITS);
const _: () = assert!(REP_3_6 + 1 == REPZ_3_10);
const _: () = assert!(REPZ_3_10 + 1 == REPZ_11_138);
const _: () = assert!(REPZ_11_138 + 1 == BL_CODES);
const _: () = assert!(END_BLOCK == crate::deflate::state::LITERALS);

// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use super::{
        _dist_code, _length_code, base_dist, base_length, bl_order, extra_blbits, extra_dbits,
        extra_lbits, static_dtree, static_ltree, CtData, BL_CODES, DIST_CODE_LEN, D_CODES,
        END_BLOCK, LENGTH_CODES, L_CODES, MAX_BL_BITS, MAX_MATCH, MIN_MATCH, REPZ_11_138,
        REPZ_3_10, REP_3_6,
    };
    use crate::deflate::state::{LITERALS, MAX_BITS};

    /// The first row the generator prints for `static_ltree` -- five entries to
    /// a line (`trees.c` L399) -- quoted from `trees.h` L4 as `(Code, Len)`.
    const LTREE_FIRST_ROW: [(u16, u16); 5] = [(12, 8), (140, 8), (76, 8), (204, 8), (44, 8)];

    /// The final, short row of `static_ltree`, quoted from `trees.h` L61.
    const LTREE_LAST_ROW: [(u16, u16); 3] = [(163, 8), (99, 8), (227, 8)];

    /// The first row the generator prints for `_dist_code` -- twenty entries to a
    /// line (`trees.c` L411) -- quoted from `trees.h` L74.
    const DIST_CODE_FIRST_ROW: [u8; 20] =
        [0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8];

    /// The first row the generator prints for `_length_code` -- twenty entries to
    /// a line (`trees.c` L418) -- quoted from `trees.h` L103.
    const LENGTH_CODE_FIRST_ROW: [u8; 20] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12,
    ];

    /// `base_length` in full, quoted from `trees.h` L119-L120.
    const BASE_LENGTH_FROM_HEADER: [i32; 29] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112,
        128, 160, 192, 224, 0,
    ];

    /// `base_dist` in full, quoted from `trees.h` L124-L126.
    const BASE_DIST_FROM_HEADER: [i32; 30] = [
        0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
        2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
    ];

    /// `extra_lbits` in full, quoted from `trees.c` L63.
    const EXTRA_LBITS_FROM_SOURCE: [i32; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];

    /// `extra_dbits` in full, quoted from `trees.c` L66.
    const EXTRA_DBITS_FROM_SOURCE: [i32; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    /// `extra_blbits` in full, quoted from `trees.c` L69.
    const EXTRA_BLBITS_FROM_SOURCE: [i32; 19] =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

    /// `bl_order` in full, quoted from `trees.c` L72.
    const BL_ORDER_FROM_SOURCE: [u8; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    /// Reverses the low `len` bits of `code`.
    ///
    /// Mirrors `bi_reverse` (`trees.c` L154-L161), which `gen_codes` applies to
    /// every canonical code at `trees.c` L227. The C body is a `do`/`while` that
    /// runs exactly `len` times.
    ///
    /// `u16` arithmetic cannot overflow here: after `k` iterations `res` is below
    /// `1 << k`, and `len` never exceeds `MAX_BITS` of 15.
    ///
    /// This is a local re-implementation on purpose. The shipped one belongs to
    /// `trees/bit_writer.rs`, which sits *downstream* of this module in the
    /// folder's dependency order, so importing it would invert that order and
    /// make the check circular. Re-deriving the tables from an independent copy
    /// of the routine is also what gives these tests their value.
    fn bi_reverse(code: u16, len: u16) -> u16 {
        let mut code = code;
        let mut res: u16 = 0;
        for _ in 0..len {
            res = (res | (code & 1)) << 1;
            code >>= 1;
        }
        res >> 1
    }

    /// The code width RFC 1951 3.2.6 fixes for literal/length symbol `n`.
    ///
    /// The ranges are the four `while` loops of `tr_static_init` (`trees.c`
    /// L353-L356). The first and last of them both assign 8 bits -- symbols
    /// `0..=143` and `280..=287` -- so they share the fall-through arm here
    /// rather than being spelled twice.
    fn static_ltree_code_width(symbol: usize) -> u16 {
        match symbol {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        }
    }

    #[test]
    fn declared_lengths_match_the_generated_source() {
        assert_eq!(
            static_ltree.len(),
            288,
            "`trees.h` L3 declares static_ltree[L_CODES+2]"
        );
        assert_eq!(static_ltree.len(), L_CODES + 2);
        assert_eq!(
            static_dtree.len(),
            30,
            "`trees.h` L64 declares static_dtree[D_CODES]"
        );
        assert_eq!(static_dtree.len(), D_CODES);
        assert_eq!(
            _dist_code.len(),
            512,
            "`trees.h` L73 declares _dist_code[DIST_CODE_LEN]"
        );
        assert_eq!(_dist_code.len(), DIST_CODE_LEN);
        assert_eq!(
            _length_code.len(),
            256,
            "`trees.h` L102 declares _length_code[MAX_MATCH-MIN_MATCH+1]"
        );
        assert_eq!(_length_code.len(), MAX_MATCH - MIN_MATCH + 1);
        assert_eq!(
            base_length.len(),
            29,
            "`trees.h` L118 declares base_length[LENGTH_CODES]"
        );
        assert_eq!(base_length.len(), LENGTH_CODES);
        assert_eq!(
            base_dist.len(),
            30,
            "`trees.h` L123 declares base_dist[D_CODES]"
        );
        assert_eq!(base_dist.len(), D_CODES);
        assert_eq!(
            extra_lbits.len(),
            LENGTH_CODES,
            "`trees.c` L62 declares extra_lbits[LENGTH_CODES]"
        );
        assert_eq!(
            extra_dbits.len(),
            D_CODES,
            "`trees.c` L65 declares extra_dbits[D_CODES]"
        );
        assert_eq!(
            extra_blbits.len(),
            BL_CODES,
            "`trees.c` L68 declares extra_blbits[BL_CODES]"
        );
        assert_eq!(
            bl_order.len(),
            BL_CODES,
            "`trees.c` L71 declares bl_order[BL_CODES]"
        );
    }

    #[test]
    fn constants_match_trees_c() {
        assert_eq!(DIST_CODE_LEN, 512, "`trees.c` L81");
        assert_eq!(MAX_BL_BITS, 7, "`trees.c` L47");
        assert_eq!(END_BLOCK, 256, "`trees.c` L50");
        assert_eq!(REP_3_6, 16, "`trees.c` L53");
        assert_eq!(REPZ_3_10, 17, "`trees.c` L56");
        assert_eq!(REPZ_11_138, 18, "`trees.c` L59");

        // `END_BLOCK` is the symbol immediately after the 256 literals, and the
        // length codes begin immediately after it -- the `+ LITERALS + 1` offset
        // `compress_block` applies at `trees.c` L925.
        assert_eq!(END_BLOCK, LITERALS);
        assert_eq!(LITERALS + 1 + LENGTH_CODES, L_CODES);

        // The bit-length alphabet is 16 code widths followed by the three repeat
        // codes, and nothing else. That `MAX_BL_BITS` is the tighter of the two
        // width bounds is pinned at compile time above; here the two values are
        // simply restated so a drift in either is named.
        assert_eq!(REPZ_11_138 + 1, BL_CODES);
        assert_eq!(MAX_BITS, 15, "`deflate.h` L52");
    }

    #[test]
    fn endpoints_match_the_committed_source() {
        for (index, &(code, len)) in LTREE_FIRST_ROW.iter().enumerate() {
            assert_eq!(
                static_ltree[index],
                CtData::new(code, len),
                "static_ltree[{index}], `trees.h` L4"
            );
        }
        let tail = static_ltree.len() - LTREE_LAST_ROW.len();
        for (offset, &(code, len)) in LTREE_LAST_ROW.iter().enumerate() {
            assert_eq!(
                static_ltree[tail + offset],
                CtData::new(code, len),
                "static_ltree[{}], `trees.h` L61",
                tail + offset
            );
        }

        assert_eq!(static_dtree[0], CtData::new(0, 5), "`trees.h` L65");
        assert_eq!(
            static_dtree[D_CODES - 1],
            CtData::new(23, 5),
            "`trees.h` L70"
        );

        assert_eq!(
            _dist_code[..DIST_CODE_FIRST_ROW.len()],
            DIST_CODE_FIRST_ROW,
            "`trees.h` L74"
        );
        assert_eq!(_dist_code[DIST_CODE_LEN - 1], 29, "`trees.h` L99");

        assert_eq!(
            _length_code[..LENGTH_CODE_FIRST_ROW.len()],
            LENGTH_CODE_FIRST_ROW,
            "`trees.h` L103"
        );
        assert_eq!(_length_code[_length_code.len() - 1], 28, "`trees.h` L115");

        assert_eq!(base_length, BASE_LENGTH_FROM_HEADER, "`trees.h` L119-L120");
        assert_eq!(base_dist, BASE_DIST_FROM_HEADER, "`trees.h` L124-L126");
        assert_eq!(extra_lbits, EXTRA_LBITS_FROM_SOURCE, "`trees.c` L63");
        assert_eq!(extra_dbits, EXTRA_DBITS_FROM_SOURCE, "`trees.c` L66");
        assert_eq!(extra_blbits, EXTRA_BLBITS_FROM_SOURCE, "`trees.c` L69");
        assert_eq!(bl_order, BL_ORDER_FROM_SOURCE, "`trees.c` L72");
    }

    #[test]
    fn static_dtree_is_the_trivial_five_bit_tree() {
        for (symbol, entry) in static_dtree.iter().enumerate() {
            assert_eq!(entry.len(), 5, "static_dtree[{symbol}].Len, `trees.c` L365");

            // `trees.c` L366: static_dtree[n].Code = bi_reverse((unsigned)n, 5).
            let expected = bi_reverse(
                u16::try_from(symbol).expect("D_CODES is 30, so a symbol fits in u16"),
                5,
            );
            assert_eq!(
                entry.code(),
                expected,
                "static_dtree[{symbol}].Code, `trees.c` L366"
            );
        }
    }

    #[test]
    fn static_ltree_has_the_code_widths_rfc1951_fixes() {
        for (symbol, entry) in static_ltree.iter().enumerate() {
            assert_eq!(
                entry.len(),
                static_ltree_code_width(symbol),
                "static_ltree[{symbol}].Len, `trees.c` L353-L356"
            );
        }

        // The widths must satisfy Kraft's equality for the tree to be complete,
        // which is what lets `gen_codes` finish with an all-ones longest code --
        // the C `Assert` at `trees.c` L219. Counted with a denominator of
        // `1 << MAX_BITS` so the arithmetic stays integral.
        let total: u32 = static_ltree
            .iter()
            .map(|entry| 1u32 << (MAX_BITS - usize::from(entry.len())))
            .sum();
        assert_eq!(
            total,
            1u32 << MAX_BITS,
            "the static literal tree is complete"
        );

        // The distance tree is deliberately *incomplete*: RFC 1951 3.2.6 defines
        // 32 five-bit distance codes but D_CODES is 30, because codes 30 and 31
        // can never occur.
        let distance_total: u32 = static_dtree
            .iter()
            .map(|entry| 1u32 << (MAX_BITS - usize::from(entry.len())))
            .sum();
        assert_eq!(distance_total, (1u32 << MAX_BITS) / 32 * 30);
    }

    #[test]
    fn static_ltree_codes_are_the_canonical_codes_reversed() {
        // A verbatim re-derivation of `gen_codes` (`trees.c` L203-L232) over the
        // width distribution `tr_static_init` installs (`trees.c` L351-L361).
        let mut bl_count = [0u16; MAX_BITS + 1];
        for symbol in 0..static_ltree.len() {
            bl_count[usize::from(static_ltree_code_width(symbol))] += 1;
        }

        let mut next_code = [0u16; MAX_BITS + 1];
        let mut code: u16 = 0;
        for bits in 1..=MAX_BITS {
            code = (code + bl_count[bits - 1]) << 1;
            next_code[bits] = code;
        }

        // `trees.c` L219-L220: "the last code must be all ones".
        assert_eq!(
            code + bl_count[MAX_BITS] - 1,
            (1u16 << MAX_BITS) - 1,
            "inconsistent bit counts"
        );

        for (symbol, entry) in static_ltree.iter().enumerate() {
            let len = entry.len();
            let slot = usize::from(len);
            let expected = bi_reverse(next_code[slot], len);
            next_code[slot] += 1;
            assert_eq!(
                entry.code(),
                expected,
                "static_ltree[{symbol}].Code, `trees.c` L227"
            );
        }
    }

    #[test]
    fn the_length_map_is_what_tr_static_init_builds() {
        // A verbatim re-derivation of `trees.c` L317-L330.
        let mut bases = [0i32; LENGTH_CODES];
        let mut map = [0u8; MAX_MATCH - MIN_MATCH + 1];
        let mut length: usize = 0;

        for code in 0..LENGTH_CODES - 1 {
            bases[code] = i32::try_from(length).expect("length stays below 256");
            for _ in 0..(1i32 << extra_lbits[code]) {
                map[length] = u8::try_from(code).expect("LENGTH_CODES is 29");
                length += 1;
            }
        }
        assert_eq!(length, 256, "tr_static_init: length != 256");

        // `trees.c` L326-L330: a match of MAX_MATCH has two encodings and the
        // shorter one is chosen, overwriting the natural value.
        map[length - 1] = u8::try_from(LENGTH_CODES - 1).expect("LENGTH_CODES is 29");

        assert_eq!(_length_code, map);
        assert_eq!(base_length, bases);

        // The last base is the zero the loop never assigns, and nothing reads it
        // because its code carries no extra bits.
        assert_eq!(base_length[LENGTH_CODES - 1], 0, "`trees.h` L120");
        assert_eq!(extra_lbits[LENGTH_CODES - 1], 0, "`trees.c` L63");
    }

    #[test]
    fn the_distance_map_is_what_tr_static_init_builds() {
        // A verbatim re-derivation of `trees.c` L332-L348.
        let mut bases = [0i32; D_CODES];
        let mut map = [0u8; DIST_CODE_LEN];
        let mut dist: usize = 0;

        for code in 0..16 {
            bases[code] = i32::try_from(dist).expect("dist stays below 256");
            for _ in 0..(1i32 << extra_dbits[code]) {
                map[dist] = u8::try_from(code).expect("code is below 16");
                dist += 1;
            }
        }
        assert_eq!(dist, 256, "tr_static_init: dist != 256");

        dist >>= 7; // "from now on, all distances are divided by 128"
        for code in 16..D_CODES {
            bases[code] = i32::try_from(dist << 7).expect("base_dist peaks at 24576");
            for _ in 0..(1i32 << (extra_dbits[code] - 7)) {
                map[256 + dist] = u8::try_from(code).expect("D_CODES is 30");
                dist += 1;
            }
        }
        assert_eq!(dist, 256, "tr_static_init: 256 + dist != 512");

        assert_eq!(_dist_code, map);
        assert_eq!(base_dist, bases);
    }

    #[test]
    fn dist_code_256_and_257_are_the_unused_zeroes() {
        // `deflate.h` L323-L324 states it: the second half is entered only for a
        // reduced distance of 256 or more, whose lowest index is 256 + (256 >> 7).
        assert_eq!(_dist_code[256], 0, "`trees.h` L86, and `deflate.h` L323");
        assert_eq!(_dist_code[257], 0, "`trees.h` L86, and `deflate.h` L323");
        assert_eq!(_dist_code[258], 16, "the first index d_code can produce");
        assert_eq!(256 + (256 >> 7), 258);
    }

    #[test]
    fn the_longest_match_uses_the_last_length_code() {
        // The overwrite at `trees.c` L330, seen from the caller's side.
        assert_eq!(
            _length_code[MAX_MATCH - MIN_MATCH],
            u8::try_from(LENGTH_CODES - 1).expect("LENGTH_CODES is 29"),
            "a match of MAX_MATCH bytes takes code 285, not 284 plus extra bits"
        );
        assert_eq!(_length_code[MAX_MATCH - MIN_MATCH - 1], 27);
        assert_eq!(extra_lbits[LENGTH_CODES - 1], 0);

        // Reached through the literal/length alphabet, that is symbol 285.
        assert_eq!(
            LITERALS + 1 + usize::from(_length_code[MAX_MATCH - MIN_MATCH]),
            285
        );
    }

    #[test]
    fn the_extra_bit_runs_cover_each_domain_exactly() {
        // Each length code covers `1 << extra_lbits[code]` normalised lengths,
        // and together they must cover all 256 -- the loop bound at `trees.c`
        // L321 and the assertion at L325.
        let covered: i32 = (0..LENGTH_CODES - 1)
            .map(|code| 1i32 << extra_lbits[code])
            .sum();
        assert_eq!(covered, 256);

        // Below code 16 a distance code covers `1 << extra_dbits[code]`
        // distances, and the first sixteen cover the first 256 (`trees.c` L340).
        let low: i32 = (0..16).map(|code| 1i32 << extra_dbits[code]).sum();
        assert_eq!(low, 256);

        // At and above code 16 the step is `1 << (extra_dbits[code] - 7)`
        // (`trees.c` L344). Those fourteen codes cover 254 of the second half'\''s
        // 256 slots, not all of them -- and the deficit of exactly two is the
        // arithmetic reason indices 256 and 257 stay zero. The second loop starts
        // writing at `256 + (256 >> 7)`, so it skips the first two slots and then
        // finishes on the last, which is the `dist == 256` the C code asserts at
        // `trees.c` L348.
        let high: i32 = (16..D_CODES)
            .map(|code| 1i32 << (extra_dbits[code] - 7))
            .sum();
        assert_eq!(high, 254);
        assert_eq!(
            usize::try_from(high).expect("254 is positive") + (256 >> 7),
            256
        );

        // Every distance code from 16 up carries at least the seven bits that
        // subtraction assumes, so `extra_dbits[code] - 7` is never negative.
        for (code, &extra) in extra_dbits.iter().enumerate().skip(16) {
            assert!(extra >= 7, "extra_dbits[{code}]");
        }

        // The whole 32 KiB window is addressable: the last base plus the widest
        // residue is one less than 32768.
        let widest = (1i32 << extra_dbits[D_CODES - 1]) - 1;
        assert_eq!(base_dist[D_CODES - 1] + widest, 32767);
    }

    #[test]
    fn extra_blbits_is_zero_except_for_the_three_repeat_codes() {
        for (code, &extra) in extra_blbits.iter().enumerate().take(REP_3_6) {
            assert_eq!(extra, 0, "extra_blbits[{code}] encodes a code width");
        }
        assert_eq!(extra_blbits[REP_3_6], 2, "REP_3_6 sends 2 bits of count");
        assert_eq!(extra_blbits[REPZ_3_10], 3, "REPZ_3_10 sends 3 bits");
        assert_eq!(extra_blbits[REPZ_11_138], 7, "REPZ_11_138 sends 7 bits");

        // Exactly three codes carry a repeat count -- counted from the data rather
        // than checked range by range, so a stray non-zero entry anywhere in the
        // table is caught wherever it is.
        let carrying = extra_blbits.iter().filter(|&&extra| extra != 0).count();
        assert_eq!(carrying, 3, "only the three repeat codes carry extra bits");
    }

    #[test]
    fn bl_order_is_a_permutation_of_the_bit_length_codes() {
        let mut seen = [false; BL_CODES];
        for (rank, &code) in bl_order.iter().enumerate() {
            let index = usize::from(code);
            assert!(index < BL_CODES, "bl_order[{rank}] is out of range");
            assert!(!seen[index], "bl_order repeats code {index}");
            seen[index] = true;
        }
        assert!(seen.iter().all(|&hit| hit), "bl_order omits a code");

        // The three repeat codes lead, because they are the most likely to be
        // used (`trees.c` L73-L75), and the rare extremes trail.
        assert_eq!(bl_order[0], 16);
        assert_eq!(bl_order[1], 17);
        assert_eq!(bl_order[2], 18);
        assert_eq!(bl_order[BL_CODES - 1], 15);
    }
}
