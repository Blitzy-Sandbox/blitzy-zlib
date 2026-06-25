//! Huffman trees, static lookup tables, and DEFLATE block emission.
//!
//! Safe-Rust translation of zlib's `trees.c` (Huffman tree construction and
//! block emission), `trees.h` (the precomputed static literal/distance trees
//! and the code/length lookup tables), and the `ct_data` union plus the
//! `_tr_tally_*` macros from `deflate.h`.
//!
//! This module is the bit-exactness keystone of the compressor: it produces
//! the actual DEFLATE bitstream. Every constant, table value, bit-accumulation
//! order, and rounding decision is reproduced verbatim from the C sources so
//! that the emitted bytes are identical to those produced by the reference C
//! zlib for every `(level, strategy, windowBits)` tuple.
//!
//! # Design notes
//!
//! * **`ct_data` union.** C overlays `{freq, code}` and `{dad, len}` onto a
//!   single 4-byte record. [`CtData`] models this with two `u16` fields `fc`
//!   (freq/code) and `dl` (dad/len) plus dual accessors, preserving the exact
//!   reuse pattern (frequency during counting becomes the code afterwards; the
//!   parent link during construction becomes the bit length afterwards).
//! * **`tr_static_init` is omitted (design D9).** The static tables are
//!   embedded as `const`/`static` arrays copied verbatim from `trees.h`, so no
//!   runtime table generation is required. A `#[cfg(test)]` block regenerates
//!   the tables algorithmically (per RFC 1951 §3.2.6 and the C
//!   `tr_static_init` procedure) and asserts they match, guarding against
//!   transcription typos.
//! * **Borrow discipline.** The Huffman algorithms operate on multiple state
//!   arrays at once. Rather than aliasing `self`, the tree-construction methods
//!   destructure [`DeflateState`] into disjoint field borrows and select the
//!   active tree with a [`TreeId`]; the emission helpers copy the (Copy) tree
//!   entry out of `self` before calling `send_bits`, so no slice borrow is held
//!   across a `&mut self` call.
//! * **`bi_buf` width.** The bit accumulator is a `u16` exactly like C's
//!   `ush bi_buf`; every shift in [`DeflateState::send_bits`] truncates through
//!   `as u16`, mirroring C's implicit `(ush)` truncation.

use super::state::DeflateState;
use crate::constants::{
    DYN_TREES, DataType, MAX_MATCH, MIN_MATCH, STATIC_TREES, STORED_BLOCK, Strategy,
};

// ---------------------------------------------------------------------------
// Phase 1 — tree-structure constants
// ---------------------------------------------------------------------------
//
// These mirror the `#define`s in `deflate.h`/`trees.c`. They are the canonical
// home for the tree dimensions; the values are fixed by the DEFLATE format and
// therefore cannot drift from the copies maintained in `state.rs`.

/// Number of length codes, not counting the special `END_BLOCK` code
/// (C `LENGTH_CODES`).
pub(crate) const LENGTH_CODES: usize = 29;
/// Number of literal bytes, 0..=255 (C `LITERALS`).
pub(crate) const LITERALS: usize = 256;
/// Number of literal/length codes, including the `END_BLOCK` code
/// (C `L_CODES` = `LITERALS + 1 + LENGTH_CODES` = 286).
pub(crate) const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
/// Number of distance codes (C `D_CODES`).
pub(crate) const D_CODES: usize = 30;
/// Number of bit-length codes (C `BL_CODES`).
pub(crate) const BL_CODES: usize = 19;
/// Maximum heap size used during Huffman construction
/// (C `HEAP_SIZE` = `2 * L_CODES + 1` = 573).
pub(crate) const HEAP_SIZE: usize = 2 * L_CODES + 1;
/// Maximum bit length of any literal/length or distance code (C `MAX_BITS`).
pub(crate) const MAX_BITS: usize = 15;
/// Maximum bit length of any bit-length code (C `MAX_BL_BITS`).
pub(crate) const MAX_BL_BITS: usize = 7;
/// End-of-block literal/length code (C `END_BLOCK`).
pub(crate) const END_BLOCK: usize = 256;
/// Bit-length repeat code: repeat previous length 3..=6 times (C `REP_3_6`).
pub(crate) const REP_3_6: usize = 16;
/// Bit-length repeat code: repeat a zero length 3..=10 times (C `REPZ_3_10`).
pub(crate) const REPZ_3_10: usize = 17;
/// Bit-length repeat code: repeat a zero length 11..=138 times
/// (C `REPZ_11_138`).
pub(crate) const REPZ_11_138: usize = 18;
/// Length of the [`DIST_CODE`] distance-to-code mapping (C `DIST_CODE_LEN`).
pub(crate) const DIST_CODE_LEN: usize = 512;

/// Bit width of the [`DeflateState::bi_buf`] accumulator (C `Buf_size`).
///
/// Named in upper case (rather than the C `Buf_size`) to satisfy the
/// `non_upper_case_globals` lint under `-D warnings`.
const BUF_SIZE: i32 = 16;

/// Index of the smallest element on the Huffman heap (C `SMALLEST`).
///
/// `heap[SMALLEST]` is the root (minimum-frequency element); the two sons of
/// `heap[n]` are `heap[2*n]` and `heap[2*n+1]`.
const SMALLEST: usize = 1;

// ---------------------------------------------------------------------------
// Phase 2 — `CtData`, the `ct_data` union translation
// ---------------------------------------------------------------------------

/// One entry of a Huffman tree, translating the C `ct_data` union.
///
/// In C, `ct_data` is `union { struct {ush freq; ush code} ; struct {ush dad;
/// ush len} }` — two 16-bit fields whose meaning depends on the construction
/// phase. We model the two overlaid slots with [`fc`](Self::fc)
/// (frequency *then* code) and [`dl`](Self::dl) (parent/`dad` *then* bit
/// `len`gth). The same field is reused across phases exactly as the C union
/// does, which keeps the representation — and therefore the emitted codes —
/// bit-identical.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct CtData {
    /// Frequency slot during counting; Huffman code after code assignment
    /// (C `fc.freq` / `fc.code`).
    pub fc: u16,
    /// Parent (`dad`) slot during tree construction; bit length after
    /// `gen_bitlen` (C `dl.dad` / `dl.len`).
    pub dl: u16,
}

impl CtData {
    /// Frequency of this symbol (C `Freq`, i.e. `fc.freq`).
    #[inline]
    pub fn freq(&self) -> u16 {
        self.fc
    }
    /// Huffman code assigned to this symbol (C `Code`, i.e. `fc.code`).
    #[inline]
    pub fn code(&self) -> u16 {
        self.fc
    }
    /// Parent node index during construction (C `Dad`, i.e. `dl.dad`).
    #[inline]
    pub fn dad(&self) -> u16 {
        self.dl
    }
    /// Bit length of this symbol's code (C `Len`, i.e. `dl.len`).
    ///
    /// This is a Huffman code *bit length*, not a collection length, so the
    /// `is_empty` companion that `clippy::len_without_is_empty` expects is not
    /// meaningful here.
    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        self.dl
    }
    /// Set the frequency slot (C `Freq = v`).
    #[inline]
    pub fn set_freq(&mut self, v: u16) {
        self.fc = v;
    }
    /// Set the code slot (C `Code = v`).
    #[inline]
    pub fn set_code(&mut self, v: u16) {
        self.fc = v;
    }
    /// Set the parent slot (C `Dad = v`).
    #[inline]
    pub fn set_dad(&mut self, v: u16) {
        self.dl = v;
    }
    /// Set the bit-length slot (C `Len = v`).
    #[inline]
    pub fn set_len(&mut self, v: u16) {
        self.dl = v;
    }
}

// `ct_data` is two `ush` fields = 4 bytes; the `#[repr(C)]` layout above must
// match so the in-memory tree representation is identical to C.
const _: () = assert!(core::mem::size_of::<CtData>() == 4);

// ---------------------------------------------------------------------------
// Phase 3 — `StaticTreeDesc` (the C `static_tree_desc` records)
// ---------------------------------------------------------------------------

/// Immutable descriptor of a static Huffman tree (C `static_tree_desc`).
///
/// Used as the `stat_desc` of each [`crate::deflate::state::TreeDesc`]. The
/// bit-length tree has no associated static tree, hence `static_tree` is
/// `Option`.
pub struct StaticTreeDesc {
    /// The static tree, or `None` for the bit-length tree
    /// (C `const ct_data *static_tree`).
    pub static_tree: Option<&'static [CtData]>,
    /// Extra bits for each code, indexed from `extra_base`
    /// (C `const intf *extra_bits`).
    pub extra_bits: Option<&'static [i32]>,
    /// Base index from which `extra_bits` applies (C `int extra_base`).
    pub extra_base: i32,
    /// Maximum number of elements in the tree (C `int elems`).
    pub elems: usize,
    /// Maximum bit length for codes in this tree (C `int max_length`).
    pub max_length: i32,
}

/// Static descriptor for the literal/length tree (C `static_l_desc`).
pub static STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_LTREE),
    extra_bits: Some(&EXTRA_LBITS),
    extra_base: (LITERALS + 1) as i32,
    elems: L_CODES,
    max_length: MAX_BITS as i32,
};

/// Static descriptor for the distance tree (C `static_d_desc`).
pub static STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_DTREE),
    extra_bits: Some(&EXTRA_DBITS),
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS as i32,
};

/// Static descriptor for the bit-length tree (C `static_bl_desc`).
pub static STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: Some(&EXTRA_BLBITS),
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS as i32,
};

// ---------------------------------------------------------------------------
// Phase 4 — static tables (copied verbatim from `trees.h`)
// ---------------------------------------------------------------------------
//
// The following six tables are reproduced byte-for-byte from `trees.h`. They
// are the single highest transcription risk in the project; the `#[cfg(test)]`
// block at the end of this file regenerates each one algorithmically and
// asserts equality.

pub static STATIC_LTREE: [CtData; L_CODES + 2] = [
    CtData { fc: 12, dl: 8 },
    CtData { fc: 140, dl: 8 },
    CtData { fc: 76, dl: 8 },
    CtData { fc: 204, dl: 8 },
    CtData { fc: 44, dl: 8 },
    CtData { fc: 172, dl: 8 },
    CtData { fc: 108, dl: 8 },
    CtData { fc: 236, dl: 8 },
    CtData { fc: 28, dl: 8 },
    CtData { fc: 156, dl: 8 },
    CtData { fc: 92, dl: 8 },
    CtData { fc: 220, dl: 8 },
    CtData { fc: 60, dl: 8 },
    CtData { fc: 188, dl: 8 },
    CtData { fc: 124, dl: 8 },
    CtData { fc: 252, dl: 8 },
    CtData { fc: 2, dl: 8 },
    CtData { fc: 130, dl: 8 },
    CtData { fc: 66, dl: 8 },
    CtData { fc: 194, dl: 8 },
    CtData { fc: 34, dl: 8 },
    CtData { fc: 162, dl: 8 },
    CtData { fc: 98, dl: 8 },
    CtData { fc: 226, dl: 8 },
    CtData { fc: 18, dl: 8 },
    CtData { fc: 146, dl: 8 },
    CtData { fc: 82, dl: 8 },
    CtData { fc: 210, dl: 8 },
    CtData { fc: 50, dl: 8 },
    CtData { fc: 178, dl: 8 },
    CtData { fc: 114, dl: 8 },
    CtData { fc: 242, dl: 8 },
    CtData { fc: 10, dl: 8 },
    CtData { fc: 138, dl: 8 },
    CtData { fc: 74, dl: 8 },
    CtData { fc: 202, dl: 8 },
    CtData { fc: 42, dl: 8 },
    CtData { fc: 170, dl: 8 },
    CtData { fc: 106, dl: 8 },
    CtData { fc: 234, dl: 8 },
    CtData { fc: 26, dl: 8 },
    CtData { fc: 154, dl: 8 },
    CtData { fc: 90, dl: 8 },
    CtData { fc: 218, dl: 8 },
    CtData { fc: 58, dl: 8 },
    CtData { fc: 186, dl: 8 },
    CtData { fc: 122, dl: 8 },
    CtData { fc: 250, dl: 8 },
    CtData { fc: 6, dl: 8 },
    CtData { fc: 134, dl: 8 },
    CtData { fc: 70, dl: 8 },
    CtData { fc: 198, dl: 8 },
    CtData { fc: 38, dl: 8 },
    CtData { fc: 166, dl: 8 },
    CtData { fc: 102, dl: 8 },
    CtData { fc: 230, dl: 8 },
    CtData { fc: 22, dl: 8 },
    CtData { fc: 150, dl: 8 },
    CtData { fc: 86, dl: 8 },
    CtData { fc: 214, dl: 8 },
    CtData { fc: 54, dl: 8 },
    CtData { fc: 182, dl: 8 },
    CtData { fc: 118, dl: 8 },
    CtData { fc: 246, dl: 8 },
    CtData { fc: 14, dl: 8 },
    CtData { fc: 142, dl: 8 },
    CtData { fc: 78, dl: 8 },
    CtData { fc: 206, dl: 8 },
    CtData { fc: 46, dl: 8 },
    CtData { fc: 174, dl: 8 },
    CtData { fc: 110, dl: 8 },
    CtData { fc: 238, dl: 8 },
    CtData { fc: 30, dl: 8 },
    CtData { fc: 158, dl: 8 },
    CtData { fc: 94, dl: 8 },
    CtData { fc: 222, dl: 8 },
    CtData { fc: 62, dl: 8 },
    CtData { fc: 190, dl: 8 },
    CtData { fc: 126, dl: 8 },
    CtData { fc: 254, dl: 8 },
    CtData { fc: 1, dl: 8 },
    CtData { fc: 129, dl: 8 },
    CtData { fc: 65, dl: 8 },
    CtData { fc: 193, dl: 8 },
    CtData { fc: 33, dl: 8 },
    CtData { fc: 161, dl: 8 },
    CtData { fc: 97, dl: 8 },
    CtData { fc: 225, dl: 8 },
    CtData { fc: 17, dl: 8 },
    CtData { fc: 145, dl: 8 },
    CtData { fc: 81, dl: 8 },
    CtData { fc: 209, dl: 8 },
    CtData { fc: 49, dl: 8 },
    CtData { fc: 177, dl: 8 },
    CtData { fc: 113, dl: 8 },
    CtData { fc: 241, dl: 8 },
    CtData { fc: 9, dl: 8 },
    CtData { fc: 137, dl: 8 },
    CtData { fc: 73, dl: 8 },
    CtData { fc: 201, dl: 8 },
    CtData { fc: 41, dl: 8 },
    CtData { fc: 169, dl: 8 },
    CtData { fc: 105, dl: 8 },
    CtData { fc: 233, dl: 8 },
    CtData { fc: 25, dl: 8 },
    CtData { fc: 153, dl: 8 },
    CtData { fc: 89, dl: 8 },
    CtData { fc: 217, dl: 8 },
    CtData { fc: 57, dl: 8 },
    CtData { fc: 185, dl: 8 },
    CtData { fc: 121, dl: 8 },
    CtData { fc: 249, dl: 8 },
    CtData { fc: 5, dl: 8 },
    CtData { fc: 133, dl: 8 },
    CtData { fc: 69, dl: 8 },
    CtData { fc: 197, dl: 8 },
    CtData { fc: 37, dl: 8 },
    CtData { fc: 165, dl: 8 },
    CtData { fc: 101, dl: 8 },
    CtData { fc: 229, dl: 8 },
    CtData { fc: 21, dl: 8 },
    CtData { fc: 149, dl: 8 },
    CtData { fc: 85, dl: 8 },
    CtData { fc: 213, dl: 8 },
    CtData { fc: 53, dl: 8 },
    CtData { fc: 181, dl: 8 },
    CtData { fc: 117, dl: 8 },
    CtData { fc: 245, dl: 8 },
    CtData { fc: 13, dl: 8 },
    CtData { fc: 141, dl: 8 },
    CtData { fc: 77, dl: 8 },
    CtData { fc: 205, dl: 8 },
    CtData { fc: 45, dl: 8 },
    CtData { fc: 173, dl: 8 },
    CtData { fc: 109, dl: 8 },
    CtData { fc: 237, dl: 8 },
    CtData { fc: 29, dl: 8 },
    CtData { fc: 157, dl: 8 },
    CtData { fc: 93, dl: 8 },
    CtData { fc: 221, dl: 8 },
    CtData { fc: 61, dl: 8 },
    CtData { fc: 189, dl: 8 },
    CtData { fc: 125, dl: 8 },
    CtData { fc: 253, dl: 8 },
    CtData { fc: 19, dl: 9 },
    CtData { fc: 275, dl: 9 },
    CtData { fc: 147, dl: 9 },
    CtData { fc: 403, dl: 9 },
    CtData { fc: 83, dl: 9 },
    CtData { fc: 339, dl: 9 },
    CtData { fc: 211, dl: 9 },
    CtData { fc: 467, dl: 9 },
    CtData { fc: 51, dl: 9 },
    CtData { fc: 307, dl: 9 },
    CtData { fc: 179, dl: 9 },
    CtData { fc: 435, dl: 9 },
    CtData { fc: 115, dl: 9 },
    CtData { fc: 371, dl: 9 },
    CtData { fc: 243, dl: 9 },
    CtData { fc: 499, dl: 9 },
    CtData { fc: 11, dl: 9 },
    CtData { fc: 267, dl: 9 },
    CtData { fc: 139, dl: 9 },
    CtData { fc: 395, dl: 9 },
    CtData { fc: 75, dl: 9 },
    CtData { fc: 331, dl: 9 },
    CtData { fc: 203, dl: 9 },
    CtData { fc: 459, dl: 9 },
    CtData { fc: 43, dl: 9 },
    CtData { fc: 299, dl: 9 },
    CtData { fc: 171, dl: 9 },
    CtData { fc: 427, dl: 9 },
    CtData { fc: 107, dl: 9 },
    CtData { fc: 363, dl: 9 },
    CtData { fc: 235, dl: 9 },
    CtData { fc: 491, dl: 9 },
    CtData { fc: 27, dl: 9 },
    CtData { fc: 283, dl: 9 },
    CtData { fc: 155, dl: 9 },
    CtData { fc: 411, dl: 9 },
    CtData { fc: 91, dl: 9 },
    CtData { fc: 347, dl: 9 },
    CtData { fc: 219, dl: 9 },
    CtData { fc: 475, dl: 9 },
    CtData { fc: 59, dl: 9 },
    CtData { fc: 315, dl: 9 },
    CtData { fc: 187, dl: 9 },
    CtData { fc: 443, dl: 9 },
    CtData { fc: 123, dl: 9 },
    CtData { fc: 379, dl: 9 },
    CtData { fc: 251, dl: 9 },
    CtData { fc: 507, dl: 9 },
    CtData { fc: 7, dl: 9 },
    CtData { fc: 263, dl: 9 },
    CtData { fc: 135, dl: 9 },
    CtData { fc: 391, dl: 9 },
    CtData { fc: 71, dl: 9 },
    CtData { fc: 327, dl: 9 },
    CtData { fc: 199, dl: 9 },
    CtData { fc: 455, dl: 9 },
    CtData { fc: 39, dl: 9 },
    CtData { fc: 295, dl: 9 },
    CtData { fc: 167, dl: 9 },
    CtData { fc: 423, dl: 9 },
    CtData { fc: 103, dl: 9 },
    CtData { fc: 359, dl: 9 },
    CtData { fc: 231, dl: 9 },
    CtData { fc: 487, dl: 9 },
    CtData { fc: 23, dl: 9 },
    CtData { fc: 279, dl: 9 },
    CtData { fc: 151, dl: 9 },
    CtData { fc: 407, dl: 9 },
    CtData { fc: 87, dl: 9 },
    CtData { fc: 343, dl: 9 },
    CtData { fc: 215, dl: 9 },
    CtData { fc: 471, dl: 9 },
    CtData { fc: 55, dl: 9 },
    CtData { fc: 311, dl: 9 },
    CtData { fc: 183, dl: 9 },
    CtData { fc: 439, dl: 9 },
    CtData { fc: 119, dl: 9 },
    CtData { fc: 375, dl: 9 },
    CtData { fc: 247, dl: 9 },
    CtData { fc: 503, dl: 9 },
    CtData { fc: 15, dl: 9 },
    CtData { fc: 271, dl: 9 },
    CtData { fc: 143, dl: 9 },
    CtData { fc: 399, dl: 9 },
    CtData { fc: 79, dl: 9 },
    CtData { fc: 335, dl: 9 },
    CtData { fc: 207, dl: 9 },
    CtData { fc: 463, dl: 9 },
    CtData { fc: 47, dl: 9 },
    CtData { fc: 303, dl: 9 },
    CtData { fc: 175, dl: 9 },
    CtData { fc: 431, dl: 9 },
    CtData { fc: 111, dl: 9 },
    CtData { fc: 367, dl: 9 },
    CtData { fc: 239, dl: 9 },
    CtData { fc: 495, dl: 9 },
    CtData { fc: 31, dl: 9 },
    CtData { fc: 287, dl: 9 },
    CtData { fc: 159, dl: 9 },
    CtData { fc: 415, dl: 9 },
    CtData { fc: 95, dl: 9 },
    CtData { fc: 351, dl: 9 },
    CtData { fc: 223, dl: 9 },
    CtData { fc: 479, dl: 9 },
    CtData { fc: 63, dl: 9 },
    CtData { fc: 319, dl: 9 },
    CtData { fc: 191, dl: 9 },
    CtData { fc: 447, dl: 9 },
    CtData { fc: 127, dl: 9 },
    CtData { fc: 383, dl: 9 },
    CtData { fc: 255, dl: 9 },
    CtData { fc: 511, dl: 9 },
    CtData { fc: 0, dl: 7 },
    CtData { fc: 64, dl: 7 },
    CtData { fc: 32, dl: 7 },
    CtData { fc: 96, dl: 7 },
    CtData { fc: 16, dl: 7 },
    CtData { fc: 80, dl: 7 },
    CtData { fc: 48, dl: 7 },
    CtData { fc: 112, dl: 7 },
    CtData { fc: 8, dl: 7 },
    CtData { fc: 72, dl: 7 },
    CtData { fc: 40, dl: 7 },
    CtData { fc: 104, dl: 7 },
    CtData { fc: 24, dl: 7 },
    CtData { fc: 88, dl: 7 },
    CtData { fc: 56, dl: 7 },
    CtData { fc: 120, dl: 7 },
    CtData { fc: 4, dl: 7 },
    CtData { fc: 68, dl: 7 },
    CtData { fc: 36, dl: 7 },
    CtData { fc: 100, dl: 7 },
    CtData { fc: 20, dl: 7 },
    CtData { fc: 84, dl: 7 },
    CtData { fc: 52, dl: 7 },
    CtData { fc: 116, dl: 7 },
    CtData { fc: 3, dl: 8 },
    CtData { fc: 131, dl: 8 },
    CtData { fc: 67, dl: 8 },
    CtData { fc: 195, dl: 8 },
    CtData { fc: 35, dl: 8 },
    CtData { fc: 163, dl: 8 },
    CtData { fc: 99, dl: 8 },
    CtData { fc: 227, dl: 8 },
];

pub static STATIC_DTREE: [CtData; D_CODES] = [
    CtData { fc: 0, dl: 5 },
    CtData { fc: 16, dl: 5 },
    CtData { fc: 8, dl: 5 },
    CtData { fc: 24, dl: 5 },
    CtData { fc: 4, dl: 5 },
    CtData { fc: 20, dl: 5 },
    CtData { fc: 12, dl: 5 },
    CtData { fc: 28, dl: 5 },
    CtData { fc: 2, dl: 5 },
    CtData { fc: 18, dl: 5 },
    CtData { fc: 10, dl: 5 },
    CtData { fc: 26, dl: 5 },
    CtData { fc: 6, dl: 5 },
    CtData { fc: 22, dl: 5 },
    CtData { fc: 14, dl: 5 },
    CtData { fc: 30, dl: 5 },
    CtData { fc: 1, dl: 5 },
    CtData { fc: 17, dl: 5 },
    CtData { fc: 9, dl: 5 },
    CtData { fc: 25, dl: 5 },
    CtData { fc: 5, dl: 5 },
    CtData { fc: 21, dl: 5 },
    CtData { fc: 13, dl: 5 },
    CtData { fc: 29, dl: 5 },
    CtData { fc: 3, dl: 5 },
    CtData { fc: 19, dl: 5 },
    CtData { fc: 11, dl: 5 },
    CtData { fc: 27, dl: 5 },
    CtData { fc: 7, dl: 5 },
    CtData { fc: 23, dl: 5 },
];

static DIST_CODE: [u8; DIST_CODE_LEN] = [
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

static LENGTH_CODE: [u8; MAX_MATCH - MIN_MATCH + 1] = [
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

static BASE_LENGTH: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224, 0,
];

static BASE_DIST: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

/// Extra bits for each length code (C `extra_lbits`).
static EXTRA_LBITS: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each distance code (C `extra_dbits`).
static EXTRA_DBITS: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits for each bit-length code (C `extra_blbits`).
static EXTRA_BLBITS: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// The order in which bit-length-tree code lengths are transmitted
/// (C `bl_order`).
static BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ---------------------------------------------------------------------------
// Phase 5 — small helpers
// ---------------------------------------------------------------------------

/// Map a distance into its distance code (C macro `d_code`).
#[inline]
fn d_code(dist: usize) -> usize {
    if dist < 256 {
        DIST_CODE[dist] as usize
    } else {
        DIST_CODE[256 + (dist >> 7)] as usize
    }
}

/// Reverse the low `len` bits of `code` (C `bi_reverse`).
///
/// Used to convert a canonical (MSB-first) Huffman code into the LSB-first
/// order the DEFLATE bitstream requires.
fn bi_reverse(code: u32, len: i32) -> u32 {
    let mut res: u32 = 0;
    let mut code = code;
    let mut len = len;
    // C: `do { res |= code & 1; code >>= 1; res <<= 1; } while (--len > 0);`
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

/// Selects which of the three Huffman trees a tree-construction method operates
/// on. Used to obtain disjoint `&mut` borrows of `self`'s tree arrays without
/// aliasing (design D2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeId {
    /// The literal/length tree (`dyn_ltree` / `l_desc`).
    L,
    /// The distance tree (`dyn_dtree` / `d_desc`).
    D,
    /// The bit-length tree (`bl_tree` / `bl_desc`).
    Bl,
}

// ---------------------------------------------------------------------------
// Free Huffman helpers (operate on explicit slices to avoid aliasing `self`)
// ---------------------------------------------------------------------------

/// Compare two heap nodes by frequency, breaking ties by tree depth
/// (C `smaller` macro).
#[inline]
fn smaller(tree: &[CtData], n: i32, m: i32, depth: &[u8]) -> bool {
    let fn_ = tree[n as usize].freq();
    let fm = tree[m as usize].freq();
    fn_ < fm || (fn_ == fm && depth[n as usize] <= depth[m as usize])
}

/// Restore the heap property by sifting `heap[k]` down (C `pqdownheap`).
fn pqdownheap(tree: &[CtData], heap: &mut [i32], heap_len: usize, depth: &[u8], k: usize) {
    let mut k = k;
    let v = heap[k];
    let mut j = k << 1; // left son of k
    while j <= heap_len {
        // Set j to the smaller of the two sons.
        if j < heap_len && smaller(tree, heap[j + 1], heap[j], depth) {
            j += 1;
        }
        // Exit if v is smaller than both sons.
        if smaller(tree, v, heap[j], depth) {
            break;
        }
        // Exchange v with the smaller son.
        heap[k] = heap[j];
        k = j;
        // Continue down the tree, setting j to the left son of k.
        j <<= 1;
    }
    heap[k] = v;
}

/// Generate the Huffman codes for `tree` given the per-length counts
/// `bl_count` (C `gen_codes`).
#[allow(clippy::needless_range_loop)]
fn gen_codes(tree: &mut [CtData], max_code: i32, bl_count: &[u16]) {
    // next_code[len] = smallest code of length `len`.
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    // The distribution counts are first used to generate the code values
    // without bit reversal.
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }
    // Then assign the (bit-reversed) codes to each symbol.
    for n in 0..=max_code as usize {
        let len = tree[n].len() as usize;
        if len == 0 {
            continue;
        }
        // C: `tree[n].Code = bi_reverse(next_code[len]++, len);`
        let c = bi_reverse(next_code[len] as u32, len as i32);
        tree[n].set_code(c as u16);
        next_code[len] += 1;
    }
}

// ---------------------------------------------------------------------------
// Tree construction and block emission, implemented on `DeflateState`
// ---------------------------------------------------------------------------

impl DeflateState {
    // -- Phase 6: bit accumulator -------------------------------------------

    /// Output a 16-bit value to the pending buffer, least-significant byte
    /// first (C `put_short`).
    #[inline]
    fn put_short(&mut self, w: u16) {
        self.put_byte((w & 0xff) as u8);
        self.put_byte((w >> 8) as u8);
    }

    /// Send `length` bits (the low `length` bits of `value`) to the bit
    /// accumulator, flushing a full 16-bit word to the pending buffer when the
    /// accumulator overflows (C `send_bits`, non-debug variant).
    ///
    /// The shifts are performed in `u32` to avoid Rust shift-overflow panics
    /// and then truncated through `as u16`, which reproduces C's implicit
    /// `(ush)` truncation exactly. The caller guarantees `1 <= length <= 16`
    /// and that `value` fits in `length` bits.
    fn send_bits(&mut self, value: i32, length: i32) {
        let len = length;
        let v = value as u32;
        if self.bi_valid > BUF_SIZE - len {
            // C: `bi_buf |= (ush)(value << bi_valid);` — shift in 32 bits, then
            // truncate the OR target to 16 bits.
            self.bi_buf |= (v << self.bi_valid) as u16;
            let buf = self.bi_buf;
            self.put_short(buf);
            // C: `bi_buf = (ush)value >> (Buf_size - bi_valid);` — truncate
            // `value` to 16 bits *first*, then shift the remaining high bits
            // down into the fresh accumulator.
            self.bi_buf = ((v & 0xffff) >> (BUF_SIZE - self.bi_valid)) as u16;
            self.bi_valid += len - BUF_SIZE;
        } else {
            self.bi_buf |= (v << self.bi_valid) as u16;
            self.bi_valid += len;
        }
    }

    /// Send the Huffman code for symbol `c` taken from `tree` (C `send_code`).
    ///
    /// `tree` must not borrow from `self` (it is only ever a `'static` table
    /// here); dynamic trees are emitted through [`Self::send_code_lit`],
    /// [`Self::send_code_dist`], and [`Self::send_code_bl`] which copy the
    /// entry out of `self` first.
    #[inline]
    fn send_code(&mut self, c: usize, tree: &[CtData]) {
        self.send_bits(tree[c].code() as i32, tree[c].len() as i32);
    }

    /// Emit a literal/length symbol from either the static or the dynamic
    /// literal/length tree. The entry is copied out before `send_bits` so no
    /// borrow of `self`'s arrays is held across the `&mut self` call.
    #[inline]
    fn send_code_lit(&mut self, c: usize, static_trees: bool) {
        let e = if static_trees {
            STATIC_LTREE[c]
        } else {
            self.dyn_ltree[c]
        };
        self.send_bits(e.code() as i32, e.len() as i32);
    }

    /// Emit a distance symbol from either the static or the dynamic distance
    /// tree. See [`Self::send_code_lit`] for the borrow rationale.
    #[inline]
    fn send_code_dist(&mut self, c: usize, static_trees: bool) {
        let e = if static_trees {
            STATIC_DTREE[c]
        } else {
            self.dyn_dtree[c]
        };
        self.send_bits(e.code() as i32, e.len() as i32);
    }

    /// Emit a code from the (dynamic) bit-length tree. See
    /// [`Self::send_code_lit`] for the borrow rationale.
    #[inline]
    fn send_code_bl(&mut self, c: usize) {
        let e = self.bl_tree[c];
        self.send_bits(e.code() as i32, e.len() as i32);
    }

    /// Read the bit length of entry `idx` of the selected tree (a `&self`
    /// helper used by [`Self::send_tree`] so it never holds a tree borrow
    /// across `send_bits`).
    #[inline]
    fn tree_len(&self, tree_id: TreeId, idx: usize) -> u16 {
        match tree_id {
            TreeId::L => self.dyn_ltree[idx].len(),
            TreeId::D => self.dyn_dtree[idx].len(),
            TreeId::Bl => self.bl_tree[idx].len(),
        }
    }

    /// Flush whole bytes from the bit accumulator to the pending buffer,
    /// leaving 0..7 bits behind (C `bi_flush`).
    fn bi_flush(&mut self) {
        if self.bi_valid == 16 {
            let buf = self.bi_buf;
            self.put_short(buf);
            self.bi_buf = 0;
            self.bi_valid = 0;
        } else if self.bi_valid >= 8 {
            self.put_byte(self.bi_buf as u8);
            self.bi_buf >>= 8;
            self.bi_valid -= 8;
        }
    }

    /// Flush the bit accumulator to a byte boundary, padding with zero bits
    /// (C `bi_windup`). Also records `bi_used` (the number of valid bits in the
    /// final byte) which `deflate_stored` consults.
    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let buf = self.bi_buf;
            self.put_short(buf);
        } else if self.bi_valid > 0 {
            self.put_byte(self.bi_buf as u8);
        }
        self.bi_used = ((self.bi_valid - 1) & 7) + 1;
        self.bi_buf = 0;
        self.bi_valid = 0;
    }

    // -- Phase 8: heap operations & dynamic tree construction ---------------

    /// Compute the optimal bit lengths for the tree selected by `desc_id`,
    /// redistributing any lengths that exceed `max_length` (C `gen_bitlen`).
    #[allow(clippy::needless_range_loop)]
    fn gen_bitlen(&mut self, desc_id: TreeId) {
        // Pull the (static) descriptor scalars and max_code before borrowing
        // `self`'s body. `stat_desc` is a `&'static` reference, so reading it
        // does not borrow `self`.
        let (max_code, stat_desc) = match desc_id {
            TreeId::L => (self.l_desc.max_code, self.l_desc.stat_desc),
            TreeId::D => (self.d_desc.max_code, self.d_desc.stat_desc),
            TreeId::Bl => (self.bl_desc.max_code, self.bl_desc.stat_desc),
        };
        let stree = stat_desc.static_tree;
        let extra = stat_desc.extra_bits;
        let base = stat_desc.extra_base;
        let max_length = stat_desc.max_length;

        let DeflateState {
            dyn_ltree,
            dyn_dtree,
            bl_tree,
            heap,
            heap_max,
            bl_count,
            opt_len,
            static_len,
            ..
        } = self;

        let tree: &mut [CtData] = match desc_id {
            TreeId::L => dyn_ltree,
            TreeId::D => dyn_dtree,
            TreeId::Bl => bl_tree,
        };
        let heap_max = *heap_max;

        for bits in 0..=MAX_BITS {
            bl_count[bits] = 0;
        }

        // The root of the heap always has length 0 (it is the deepest node).
        tree[heap[heap_max] as usize].set_len(0);

        let mut overflow: i32 = 0;
        // First pass: compute optimal lengths, possibly overflowing for the
        // bit-length tree.
        for h in (heap_max + 1)..HEAP_SIZE {
            let n = heap[h] as usize;
            let dad = tree[n].dad() as usize;
            let mut bits = tree[dad].len() as i32 + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            // Overwrites tree[n].Dad, which is no longer needed.
            tree[n].set_len(bits as u16);

            if n as i32 > max_code {
                continue; // not a leaf node
            }

            bl_count[bits as usize] += 1;
            let mut xbits = 0i32;
            if n as i32 >= base
                && let Some(ex) = extra
            {
                xbits = ex[(n as i32 - base) as usize];
            }
            let f = tree[n].freq() as usize;
            *opt_len = opt_len.wrapping_add(f.wrapping_mul((bits + xbits) as usize));
            if let Some(st) = stree {
                *static_len =
                    static_len.wrapping_add(f.wrapping_mul((st[n].len() as i32 + xbits) as usize));
            }
        }
        if overflow == 0 {
            return;
        }

        // Find the first bit length which could increase, then move leaves
        // down the tree to absorb the overflow.
        loop {
            let mut bits = (max_length - 1) as usize;
            while bl_count[bits] == 0 {
                bits -= 1;
            }
            bl_count[bits] -= 1; // move one leaf down the tree
            bl_count[bits + 1] += 2; // move one overflow item as its brother
            bl_count[max_length as usize] -= 1;
            // The brother of the overflow item also moves one step up, but this
            // does not affect bl_count[max_length].
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }

        // Recompute all bit lengths, scanning in order of increasing frequency.
        // `h` is still equal to HEAP_SIZE (it is simpler to reconstruct all
        // lengths than to fix only the wrong ones).
        let mut h = HEAP_SIZE;
        for bits in (1..=max_length as usize).rev() {
            let mut n = bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = heap[h] as usize;
                if m as i32 > max_code {
                    continue;
                }
                let old_len = tree[m].len();
                if old_len as usize != bits {
                    // C: `opt_len += ((ulg)bits - tree[m].Len) * tree[m].Freq;`
                    // The subtraction underflows in `ulg` (64-bit) when a code
                    // shrinks; reproduce with wrapping arithmetic.
                    let freq = tree[m].freq();
                    *opt_len = opt_len.wrapping_add(
                        (bits as u64)
                            .wrapping_sub(old_len as u64)
                            .wrapping_mul(freq as u64) as usize,
                    );
                    tree[m].set_len(bits as u16);
                }
                n -= 1;
            }
        }
    }

    /// Construct the Huffman tree selected by `desc_id` from the symbol
    /// frequencies in the corresponding dynamic tree, then assign bit lengths
    /// and codes (C `build_tree`).
    #[allow(clippy::needless_range_loop)]
    fn build_tree(&mut self, desc_id: TreeId) {
        let stat_desc = match desc_id {
            TreeId::L => self.l_desc.stat_desc,
            TreeId::D => self.d_desc.stat_desc,
            TreeId::Bl => self.bl_desc.stat_desc,
        };
        let stree = stat_desc.static_tree;
        let elems = stat_desc.elems;

        let max_code: i32;
        {
            let DeflateState {
                dyn_ltree,
                dyn_dtree,
                bl_tree,
                heap,
                heap_len,
                heap_max,
                depth,
                opt_len,
                static_len,
                ..
            } = self;

            let tree: &mut [CtData] = match desc_id {
                TreeId::L => dyn_ltree,
                TreeId::D => dyn_dtree,
                TreeId::Bl => bl_tree,
            };

            // Construct the initial heap, with the least frequent element in
            // heap[SMALLEST]. heap[0] is unused.
            *heap_len = 0;
            *heap_max = HEAP_SIZE;
            let mut mc: i32 = -1; // largest code with non-zero frequency

            for n in 0..elems {
                if tree[n].freq() != 0 {
                    *heap_len += 1;
                    heap[*heap_len] = n as i32;
                    mc = n as i32;
                    depth[n] = 0;
                } else {
                    tree[n].set_len(0);
                }
            }

            // There must be at least two codes for valid Huffman trees. Create
            // dummy leaves if necessary, decrementing opt_len/static_len which
            // wrap (the dummy nodes are 0 or 1 and cost nothing).
            while *heap_len < 2 {
                let node = if mc < 2 {
                    mc += 1;
                    mc
                } else {
                    0
                };
                *heap_len += 1;
                heap[*heap_len] = node;
                tree[node as usize].set_freq(1);
                depth[node as usize] = 0;
                *opt_len = opt_len.wrapping_sub(1);
                if let Some(st) = stree {
                    *static_len = static_len.wrapping_sub(st[node as usize].len() as usize);
                }
            }
            max_code = mc;

            // The elements heap[heap_len/2 + 1 .. heap_len] are leaves of the
            // tree; establish the heap property for the rest.
            let mut n = *heap_len / 2;
            while n >= 1 {
                pqdownheap(tree, heap, *heap_len, depth, n);
                n -= 1;
            }

            // Construct the Huffman tree by repeatedly combining the two least
            // frequent nodes.
            let mut node = elems; // next internal node of the tree
            loop {
                // pqremove: n = heap[SMALLEST]; reheapify.
                let nn = heap[SMALLEST];
                heap[SMALLEST] = heap[*heap_len];
                *heap_len -= 1;
                pqdownheap(tree, heap, *heap_len, depth, SMALLEST);

                let m = heap[SMALLEST]; // m = node of next least frequency

                // Keep the nodes sorted by frequency.
                *heap_max -= 1;
                heap[*heap_max] = nn;
                *heap_max -= 1;
                heap[*heap_max] = m;

                // Create a new node father of nn and m.
                let fsum = tree[nn as usize]
                    .freq()
                    .wrapping_add(tree[m as usize].freq());
                tree[node].set_freq(fsum);
                let dn = depth[nn as usize];
                let dm = depth[m as usize];
                depth[node] = if dn >= dm { dn } else { dm }.wrapping_add(1);
                tree[nn as usize].set_dad(node as u16);
                tree[m as usize].set_dad(node as u16);
                // and insert the new node in the heap.
                heap[SMALLEST] = node as i32;
                node += 1;
                pqdownheap(tree, heap, *heap_len, depth, SMALLEST);

                if *heap_len < 2 {
                    break;
                }
            }

            *heap_max -= 1;
            heap[*heap_max] = heap[SMALLEST];
        }

        // Record the resulting max_code on the matching descriptor.
        match desc_id {
            TreeId::L => self.l_desc.max_code = max_code,
            TreeId::D => self.d_desc.max_code = max_code,
            TreeId::Bl => self.bl_desc.max_code = max_code,
        }

        // At this point, the fields freq and dad are set. Compute the optimal
        // bit lengths and the resulting codes.
        self.gen_bitlen(desc_id);

        let DeflateState {
            dyn_ltree,
            dyn_dtree,
            bl_tree,
            bl_count,
            ..
        } = self;
        let tree: &mut [CtData] = match desc_id {
            TreeId::L => dyn_ltree,
            TreeId::D => dyn_dtree,
            TreeId::Bl => bl_tree,
        };
        gen_codes(tree, max_code, bl_count);
    }

    // -- Phase 9: bit-length tree -------------------------------------------

    /// Scan a literal/length or distance tree, accumulating run-length
    /// frequencies into the bit-length tree (C `scan_tree`).
    fn scan_tree(&mut self, tree_id: TreeId, max_code: i32) {
        let DeflateState {
            dyn_ltree,
            dyn_dtree,
            bl_tree,
            ..
        } = self;
        let tree: &mut [CtData] = match tree_id {
            TreeId::L => dyn_ltree,
            TreeId::D => dyn_dtree,
            TreeId::Bl => unreachable!("scan_tree is only called for the L and D trees"),
        };

        let mut prevlen: i32 = -1; // last emitted length
        let mut nextlen: i32 = tree[0].len() as i32; // length of next code
        let mut curlen: i32;
        let mut count: i32 = 0; // repeat count of the current code
        let mut max_count: i32 = 7; // max repeat count
        let mut min_count: i32 = 4; // min repeat count

        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        // Guard sentinel: ensures the final length differs from `nextlen`.
        tree[(max_code + 1) as usize].set_len(0xffff);

        for n in 0..=max_code as usize {
            curlen = nextlen;
            nextlen = tree[n + 1].len() as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                bl_tree[curlen as usize].fc += count as u16;
            } else if curlen != 0 {
                if curlen != prevlen {
                    bl_tree[curlen as usize].fc += 1;
                }
                bl_tree[REP_3_6].fc += 1;
            } else if count <= 10 {
                bl_tree[REPZ_3_10].fc += 1;
            } else {
                bl_tree[REPZ_11_138].fc += 1;
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

    /// Send a literal/length or distance tree in compressed form, using the
    /// bit-length tree and the repeat codes (C `send_tree`).
    fn send_tree(&mut self, tree_id: TreeId, max_code: i32) {
        let mut prevlen: i32 = -1;
        let mut nextlen: i32 = self.tree_len(tree_id, 0) as i32;
        let mut curlen: i32;
        let mut count: i32 = 0;
        let mut max_count: i32 = 7;
        let mut min_count: i32 = 4;

        // (The guard sentinel tree[max_code+1].Len was already set by scan_tree.)
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }

        for n in 0..=max_code as usize {
            curlen = nextlen;
            nextlen = self.tree_len(tree_id, n + 1) as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                // C: `do { send_code(curlen); } while (--count != 0);`
                loop {
                    self.send_code_bl(curlen as usize);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_code_bl(curlen as usize);
                    count -= 1;
                }
                self.send_code_bl(REP_3_6);
                self.send_bits(count - 3, 2);
            } else if count <= 10 {
                self.send_code_bl(REPZ_3_10);
                self.send_bits(count - 3, 3);
            } else {
                self.send_code_bl(REPZ_11_138);
                self.send_bits(count - 11, 7);
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
    /// bit-length code that must be sent (C `build_bl_tree`).
    fn build_bl_tree(&mut self) -> i32 {
        // Determine the bit-length frequencies for literal and distance trees.
        self.scan_tree(TreeId::L, self.l_desc.max_code);
        self.scan_tree(TreeId::D, self.d_desc.max_code);

        // Build the bit-length tree.
        self.build_tree(TreeId::Bl);

        // Determine the number of bit-length codes to send. The pkzip format
        // requires that at least 4 bit-length codes be sent.
        let mut max_blindex: i32 = (BL_CODES - 1) as i32;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex as usize] as usize].len() != 0 {
                break;
            }
            max_blindex -= 1;
        }
        // Update opt_len to include the bit-length tree and counts.
        self.opt_len = self
            .opt_len
            .wrapping_add(3 * (max_blindex as usize + 1) + 5 + 5 + 4);

        max_blindex
    }

    /// Send the header for a block using dynamic Huffman trees: the counts, the
    /// bit-length codes, and the two compressed trees (C `send_all_trees`).
    fn send_all_trees(&mut self, lcodes: i32, dcodes: i32, blcodes: i32) {
        self.send_bits(lcodes - 257, 5); // not +255 as stated in appnote.txt
        self.send_bits(dcodes - 1, 5);
        self.send_bits(blcodes - 4, 4); // not -3 as stated in appnote.txt
        for &bl in BL_ORDER.iter().take(blcodes as usize) {
            let len = self.bl_tree[bl as usize].len() as i32;
            self.send_bits(len, 3);
        }
        self.send_tree(TreeId::L, lcodes - 1); // literal tree
        self.send_tree(TreeId::D, dcodes - 1); // distance tree
    }

    // -- Phase 10: block emission -------------------------------------------

    /// Initialize the tree data structures for a new stream (C `_tr_init`).
    ///
    /// `tr_static_init` is intentionally omitted (design D9): the static tables
    /// are embedded, so no runtime initialization of them is required.
    pub(crate) fn tr_init(&mut self) {
        self.l_desc.stat_desc = &STATIC_L_DESC;
        self.l_desc.max_code = 0;
        self.d_desc.stat_desc = &STATIC_D_DESC;
        self.d_desc.max_code = 0;
        self.bl_desc.stat_desc = &STATIC_BL_DESC;
        self.bl_desc.max_code = 0;

        self.bi_buf = 0;
        self.bi_valid = 0;
        self.bi_used = 0;

        self.init_block();
    }

    /// Reset the symbol frequencies and the per-block accounting before a new
    /// block (C `init_block`).
    fn init_block(&mut self) {
        for e in &mut self.dyn_ltree[..L_CODES] {
            e.fc = 0;
        }
        for e in &mut self.dyn_dtree[..D_CODES] {
            e.fc = 0;
        }
        for e in &mut self.bl_tree[..BL_CODES] {
            e.fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym_next = 0;
        self.matches = 0;
    }

    /// Record a literal byte `c` in the symbol buffer and bump its frequency
    /// (C macro `_tr_tally_lit`). Returns `true` when the symbol buffer is full
    /// and the block must be flushed.
    pub(crate) fn tr_tally_lit(&mut self, c: u8) -> bool {
        let cc = c;
        self.sym_buf[self.sym_next] = 0;
        self.sym_buf[self.sym_next + 1] = 0;
        self.sym_buf[self.sym_next + 2] = cc;
        self.sym_next += 3;
        self.dyn_ltree[cc as usize].fc += 1;
        self.sym_next == self.sym_end
    }

    /// Record a (distance, length) match in the symbol buffer and bump the
    /// length- and distance-code frequencies (C macro `_tr_tally_dist`).
    /// Returns `true` when the symbol buffer is full.
    ///
    /// `length` is the match length minus `MIN_MATCH`; `distance` is the match
    /// distance.
    pub(crate) fn tr_tally_dist(&mut self, distance: u16, length: u8) -> bool {
        let len = length;
        let dist = distance;
        self.sym_buf[self.sym_next] = (dist & 0xff) as u8;
        self.sym_buf[self.sym_next + 1] = (dist >> 8) as u8;
        self.sym_buf[self.sym_next + 2] = len;
        self.sym_next += 3;
        self.matches += 1;
        let d = dist - 1;
        self.dyn_ltree[LENGTH_CODE[len as usize] as usize + LITERALS + 1].fc += 1;
        self.dyn_dtree[d_code(d as usize)].fc += 1;
        self.sym_next == self.sym_end
    }

    /// Function form of the tally operation (C `_tr_tally`): `dist == 0` records
    /// an unmatched literal `lc`, otherwise records a match of length
    /// `lc + MIN_MATCH` at distance `dist`. Returns `true` when the symbol
    /// buffer is full.
    ///
    /// In the reference C library `_tr_tally` is only invoked when `ZLIB_DEBUG`
    /// is defined; the optimized build (and this port's block producers) use the
    /// split [`tr_tally_lit`](Self::tr_tally_lit) / [`tr_tally_dist`](Self::tr_tally_dist)
    /// fast paths instead. It is retained here as the faithful function-form
    /// counterpart of the C macro for API parity, hence `allow(dead_code)`.
    #[allow(dead_code)]
    pub(crate) fn tr_tally(&mut self, dist: u16, lc: u8) -> bool {
        self.sym_buf[self.sym_next] = (dist & 0xff) as u8;
        self.sym_buf[self.sym_next + 1] = (dist >> 8) as u8;
        self.sym_buf[self.sym_next + 2] = lc;
        self.sym_next += 3;
        if dist == 0 {
            self.dyn_ltree[lc as usize].fc += 1;
        } else {
            self.matches += 1;
            let d = dist - 1;
            self.dyn_ltree[LENGTH_CODE[lc as usize] as usize + LITERALS + 1].fc += 1;
            self.dyn_dtree[d_code(d as usize)].fc += 1;
        }
        self.sym_next == self.sym_end
    }

    /// Emit the literal/length and distance symbols accumulated in the symbol
    /// buffer using the given trees, then the end-of-block code
    /// (C `compress_block`).
    ///
    /// `static_trees` selects the static (`true`) or dynamic (`false`)
    /// literal/length and distance trees. The trees are read out of `self` one
    /// entry at a time (each `CtData` is `Copy`) rather than borrowed as
    /// slices, so no immutable borrow of `self`'s tree arrays is held across
    /// the `&mut self` `send_bits` calls. This is the safe-Rust adaptation of
    /// the C function's `ltree`/`dtree` pointer parameters (design D2).
    fn compress_block(&mut self, static_trees: bool) {
        let mut sx: usize = 0; // running index in sym_buf
        if self.sym_next != 0 {
            loop {
                let dist =
                    (self.sym_buf[sx] as u32 & 0xff) + ((self.sym_buf[sx + 1] as u32 & 0xff) << 8);
                let lc = self.sym_buf[sx + 2] as i32;
                sx += 3;
                if dist == 0 {
                    self.send_code_lit(lc as usize, static_trees); // literal byte
                } else {
                    // Here, lc is the match length - MIN_MATCH.
                    let code = LENGTH_CODE[lc as usize] as usize;
                    self.send_code_lit(code + LITERALS + 1, static_trees); // length code
                    let extra = EXTRA_LBITS[code];
                    let mut lc2 = lc;
                    if extra != 0 {
                        lc2 -= BASE_LENGTH[code];
                        self.send_bits(lc2, extra); // extra length bits
                    }
                    let mut d = dist - 1; // dist is now the match distance - 1
                    let dcode = d_code(d as usize);
                    self.send_code_dist(dcode, static_trees); // distance code
                    let extra_d = EXTRA_DBITS[dcode];
                    if extra_d != 0 {
                        d -= BASE_DIST[dcode] as u32;
                        self.send_bits(d as i32, extra_d); // extra distance bits
                    }
                }
                if sx >= self.sym_next {
                    break;
                }
            }
        }
        self.send_code_lit(END_BLOCK, static_trees);
    }

    /// Classify the current block as binary or text data (C
    /// `detect_data_type`), returning the [`DataType`] code.
    ///
    /// Note: the C library stores the result in `strm->data_type`. In this port
    /// the data-type is threaded through the engine layer (there is no
    /// `data_type` field on `DeflateState`), so this method only computes and
    /// returns the value; it never influences the emitted bitstream.
    pub(crate) fn detect_data_type(&self) -> i32 {
        // block_mask has a 1 for each non-text control character that should be
        // treated as a textually "black-listed" byte (0..31 except 9, 10, 13).
        let mut block_mask: u32 = 0xf3ffc07f;
        for n in 0..=31usize {
            if (block_mask & 1) != 0 && self.dyn_ltree[n].freq() != 0 {
                return DataType::Binary.as_i32();
            }
            block_mask >>= 1;
        }
        // Check for textual ("white-listed") bytes.
        if self.dyn_ltree[9].freq() != 0
            || self.dyn_ltree[10].freq() != 0
            || self.dyn_ltree[13].freq() != 0
        {
            return DataType::Text.as_i32();
        }
        for n in 32..LITERALS {
            if self.dyn_ltree[n].freq() != 0 {
                return DataType::Text.as_i32();
            }
        }
        // There are no "black-listed" or "white-listed" bytes: this stream
        // either is empty or has tolerated ("gray-listed") bytes only.
        DataType::Binary.as_i32()
    }

    /// Emit a stored (uncompressed) block (C `_tr_stored_block`).
    ///
    /// `buf` is the raw input block to store; `last` marks the final block.
    pub(crate) fn tr_stored_block(&mut self, buf: &[u8], last: bool) {
        let stored_len = buf.len();
        self.send_bits((STORED_BLOCK << 1) + last as i32, 3); // stored block, no compression
        self.bi_windup(); // align on byte boundary
        self.put_short(stored_len as u16);
        self.put_short(!(stored_len as u16));
        if stored_len != 0 {
            let p = self.pending;
            self.pending_buf[p..p + stored_len].copy_from_slice(buf);
            self.pending += stored_len;
        }
    }

    /// Flush the bit accumulator to the pending buffer at a byte boundary kept
    /// for the next call (C `_tr_flush_bits`).
    pub(crate) fn tr_flush_bits(&mut self) {
        self.bi_flush();
    }

    /// Send one empty static block to align the bitstream on a byte boundary
    /// for `Z_SYNC_FLUSH`/`Z_FULL_FLUSH` (C `_tr_align`).
    pub(crate) fn tr_align(&mut self) {
        self.send_bits(STATIC_TREES << 1, 3);
        self.send_code(END_BLOCK, &STATIC_LTREE);
        self.bi_flush();
    }

    /// Determine the best encoding for the current block (stored, fixed, or
    /// dynamic Huffman), emit it, and reset for the next block
    /// (C `_tr_flush_block`).
    ///
    /// `buf` is the input block (or `None` when it is no longer available),
    /// `stored_len` is its length in bytes, and `last` marks the final block.
    pub(crate) fn tr_flush_block(&mut self, buf: Option<&[u8]>, stored_len: usize, last: bool) {
        // opt_lenb / static_lenb: the optimal and static block lengths in bytes.
        let opt_lenb: usize;
        let static_lenb: usize;
        let mut max_blindex: i32 = 0; // index of last bit-length code of nonzero freq

        // Build the Huffman trees unless a stored block is forced.
        if self.level > 0 {
            // NOTE: C sets `strm->data_type` here from `detect_data_type()` when
            // it is `Z_UNKNOWN`. The data-type never influences the emitted
            // bitstream and there is no `data_type` field on `DeflateState`
            // (it is threaded through the engine), so the detection is performed
            // by the caller via `detect_data_type()` and omitted here to keep
            // the output byte-identical.

            // Construct the literal and distance trees.
            self.build_tree(TreeId::L);
            self.build_tree(TreeId::D);
            // At this point opt_len and static_len are the total bit lengths of
            // the compressed block data, excluding the tree representations.

            // Build the bit-length tree for the above two trees and get the
            // index in bl_order of the last bit-length code to send.
            max_blindex = self.build_bl_tree();

            // Determine the best encoding. Compute the block lengths in bytes.
            let mut ol = (self.opt_len + 3 + 7) >> 3;
            let sl = (self.static_len + 3 + 7) >> 3;
            if sl <= ol || self.strategy == Strategy::Fixed {
                ol = sl;
            }
            opt_lenb = ol;
            static_lenb = sl;
        } else {
            // force a stored block (level 0 or no compression)
            opt_lenb = stored_len + 5;
            static_lenb = stored_len + 5;
        }

        if stored_len + 4 <= opt_lenb
            && let Some(b) = buf
        {
            // 4: two words for the lengths. The test buf != NULL is only
            // necessary if LIT_BUFSIZE > WSIZE; here it guards the Option.
            self.tr_stored_block(b, last);
        } else if static_lenb == opt_lenb {
            self.send_bits((STATIC_TREES << 1) + last as i32, 3);
            self.compress_block(true);
        } else {
            self.send_bits((DYN_TREES << 1) + last as i32, 3);
            let lmax = self.l_desc.max_code + 1;
            let dmax = self.d_desc.max_code + 1;
            self.send_all_trees(lmax, dmax, max_blindex + 1);
            self.compress_block(false);
        }

        // The above check is made in the deflate() code; reinitialize the
        // block for the next round.
        self.init_block();

        if last {
            self.bi_windup();
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 11 — static-table integrity tests
// ---------------------------------------------------------------------------
//
// These tests regenerate every embedded static table algorithmically — using
// the canonical DEFLATE definitions (RFC 1951 §3.2.6) and the procedure from
// the C `tr_static_init` — and assert that the regenerated values match the
// embedded tables byte-for-byte. They are the primary guard against
// transcription typos in the verbatim tables, which are the single highest
// bit-exactness risk in the crate. None of these tests construct a
// `DeflateState`, so they require no allocation.

#[cfg(test)]
mod tests {
    use super::*;

    /// `CtData` must be exactly 4 bytes (two `u16`) so its in-memory layout
    /// matches the C `ct_data` union.
    #[test]
    fn ctdata_layout_and_accessors() {
        assert_eq!(core::mem::size_of::<CtData>(), 4);
        assert_eq!(
            core::mem::align_of::<CtData>(),
            core::mem::align_of::<u16>()
        );

        let mut c = CtData::default();
        assert_eq!(c.freq(), 0);
        assert_eq!(c.code(), 0);
        assert_eq!(c.dad(), 0);
        assert_eq!(c.len(), 0);

        // fc aliases freq/code.
        c.set_freq(1234);
        assert_eq!(c.freq(), 1234);
        assert_eq!(c.code(), 1234);
        c.set_code(4321);
        assert_eq!(c.code(), 4321);
        assert_eq!(c.freq(), 4321);

        // dl aliases dad/len.
        c.set_dad(11);
        assert_eq!(c.dad(), 11);
        assert_eq!(c.len(), 11);
        c.set_len(15);
        assert_eq!(c.len(), 15);
        assert_eq!(c.dad(), 15);
    }

    /// The embedded tree-structure constants must equal their canonical values.
    #[test]
    fn tree_structure_constants() {
        assert_eq!(LENGTH_CODES, 29);
        assert_eq!(LITERALS, 256);
        assert_eq!(L_CODES, 286);
        assert_eq!(D_CODES, 30);
        assert_eq!(BL_CODES, 19);
        assert_eq!(HEAP_SIZE, 573);
        assert_eq!(MAX_BITS, 15);
        assert_eq!(MAX_BL_BITS, 7);
        assert_eq!(END_BLOCK, 256);
        assert_eq!(REP_3_6, 16);
        assert_eq!(REPZ_3_10, 17);
        assert_eq!(REPZ_11_138, 18);
        assert_eq!(DIST_CODE_LEN, 512);
        assert_eq!(BUF_SIZE, 16);
        assert_eq!(STATIC_LTREE.len(), L_CODES + 2);
        assert_eq!(STATIC_DTREE.len(), D_CODES);
        assert_eq!(DIST_CODE.len(), DIST_CODE_LEN);
        assert_eq!(LENGTH_CODE.len(), MAX_MATCH - MIN_MATCH + 1);
    }

    /// `bi_reverse` must reverse the low `len` bits.
    #[test]
    fn bi_reverse_known_values() {
        assert_eq!(bi_reverse(0b000, 3), 0b000);
        assert_eq!(bi_reverse(0b001, 3), 0b100);
        assert_eq!(bi_reverse(0b100, 3), 0b001);
        assert_eq!(bi_reverse(0b110, 3), 0b011);
        assert_eq!(bi_reverse(0b1, 5), 0b10000);
        assert_eq!(bi_reverse(0b00010, 5), 0b01000);
        // Reversing twice is the identity for a fixed width.
        for v in 0u32..32 {
            assert_eq!(bi_reverse(bi_reverse(v, 5), 5), v);
        }
    }

    /// The static literal/length tree bit lengths follow RFC 1951 §3.2.6:
    /// symbols 0..=143 -> 8, 144..=255 -> 9, 256..=279 -> 7, 280..=287 -> 8.
    #[test]
    fn static_ltree_lengths_match_rfc1951() {
        for (n, e) in STATIC_LTREE.iter().enumerate().take(L_CODES + 2) {
            let expected = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8, // 280..=287
            };
            assert_eq!(e.len(), expected, "STATIC_LTREE[{n}] length");
        }
    }

    /// Regenerate the static literal/length codes from the canonical lengths
    /// (exactly as C `tr_static_init` does, via `gen_codes`) and compare.
    #[test]
    fn static_ltree_codes_match_generated() {
        let mut tmp = [CtData::default(); L_CODES + 2];
        let mut bl_count = [0u16; MAX_BITS + 1];
        let mut n = 0usize;
        while n <= 143 {
            tmp[n].set_len(8);
            bl_count[8] += 1;
            n += 1;
        }
        while n <= 255 {
            tmp[n].set_len(9);
            bl_count[9] += 1;
            n += 1;
        }
        while n <= 279 {
            tmp[n].set_len(7);
            bl_count[7] += 1;
            n += 1;
        }
        while n <= 287 {
            tmp[n].set_len(8);
            bl_count[8] += 1;
            n += 1;
        }
        gen_codes(&mut tmp, (L_CODES + 1) as i32, &bl_count);
        for (i, (got, want)) in tmp.iter().zip(STATIC_LTREE.iter()).enumerate() {
            assert_eq!(got.len(), want.len(), "STATIC_LTREE[{i}] len");
            assert_eq!(got.code(), want.code(), "STATIC_LTREE[{i}] code");
        }
    }

    /// The static distance tree is trivial: every code is 5 bits and its value
    /// is `bi_reverse(n, 5)`.
    #[test]
    fn static_dtree_matches_generated() {
        for (n, e) in STATIC_DTREE.iter().enumerate() {
            assert_eq!(e.len(), 5, "STATIC_DTREE[{n}] len");
            assert_eq!(
                e.code(),
                bi_reverse(n as u32, 5) as u16,
                "STATIC_DTREE[{n}] code"
            );
        }
    }

    /// Regenerate `LENGTH_CODE` and `BASE_LENGTH` from the extra-bit table, as
    /// C `tr_static_init` does, and compare against the embedded tables.
    #[test]
    fn length_code_and_base_length_match_generated() {
        let mut length_code = [0u8; MAX_MATCH - MIN_MATCH + 1];
        let mut base_length = [0i32; LENGTH_CODES];
        let mut length = 0usize;
        for code in 0..(LENGTH_CODES - 1) {
            base_length[code] = length as i32;
            for _ in 0..(1u32 << EXTRA_LBITS[code]) {
                length_code[length] = code as u8;
                length += 1;
            }
        }
        assert_eq!(length, 256);
        // Match length 258 (index 255) is best encoded as code 285 (= the last
        // length code), overwriting the value produced by the loop above.
        length_code[length - 1] = (LENGTH_CODES - 1) as u8;
        assert_eq!(length_code, LENGTH_CODE);
        assert_eq!(base_length, BASE_LENGTH);
    }

    /// Regenerate `DIST_CODE` and `BASE_DIST` from the extra-bit table, as C
    /// `tr_static_init` does, and compare against the embedded tables.
    #[test]
    fn dist_code_and_base_dist_match_generated() {
        let mut dist_code = [0u8; DIST_CODE_LEN];
        let mut base_dist = [0i32; D_CODES];
        let mut dist = 0usize;
        for code in 0..16 {
            base_dist[code] = dist as i32;
            for _ in 0..(1u32 << EXTRA_DBITS[code]) {
                dist_code[dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256);
        // From now on, all distances are divided by 128.
        let mut dist = dist >> 7;
        for code in 16..D_CODES {
            base_dist[code] = (dist << 7) as i32;
            for _ in 0..(1u32 << (EXTRA_DBITS[code] - 7)) {
                dist_code[256 + dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256);
        assert_eq!(dist_code, DIST_CODE);
        assert_eq!(base_dist, BASE_DIST);
    }

    /// `d_code` must agree with a direct lookup into `DIST_CODE`.
    #[test]
    fn d_code_matches_table() {
        for (dist, &dc) in DIST_CODE.iter().enumerate().take(256) {
            assert_eq!(d_code(dist), dc as usize, "d_code({dist})");
        }
        for dist in 256..32768usize {
            assert_eq!(
                d_code(dist),
                DIST_CODE[256 + (dist >> 7)] as usize,
                "d_code({dist})"
            );
        }
    }

    /// The three static descriptors must point at the right tables and carry
    /// the canonical scalar parameters.
    #[test]
    fn static_descriptors_wired_correctly() {
        assert!(STATIC_L_DESC.static_tree.is_some());
        assert_eq!(STATIC_L_DESC.extra_base, (LITERALS + 1) as i32);
        assert_eq!(STATIC_L_DESC.elems, L_CODES);
        assert_eq!(STATIC_L_DESC.max_length, MAX_BITS as i32);

        assert!(STATIC_D_DESC.static_tree.is_some());
        assert_eq!(STATIC_D_DESC.extra_base, 0);
        assert_eq!(STATIC_D_DESC.elems, D_CODES);
        assert_eq!(STATIC_D_DESC.max_length, MAX_BITS as i32);

        // The bit-length tree has no associated static tree.
        assert!(STATIC_BL_DESC.static_tree.is_none());
        assert_eq!(STATIC_BL_DESC.extra_base, 0);
        assert_eq!(STATIC_BL_DESC.elems, BL_CODES);
        assert_eq!(STATIC_BL_DESC.max_length, MAX_BL_BITS as i32);
    }
}
