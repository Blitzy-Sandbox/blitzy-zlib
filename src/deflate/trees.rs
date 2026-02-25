//! Huffman tree construction, block output, and bit-level I/O for DEFLATE.
//!
//! This module ports `trees.c` (1,119 lines) and `trees.h` (128 lines) from
//! C zlib to idiomatic Rust.  It provides:
//!
//! - Static data tables for Huffman encoding (literal/length/distance codes)
//! - Static tree descriptors linking dynamic trees to their static counterparts
//! - Tree construction (`build_tree`, `gen_bitlen`, `gen_codes`)
//! - Tree serialisation for dynamic headers (`scan_tree`, `send_tree`)
//! - Block output selection (stored / static / dynamic Huffman)
//! - Tally helpers for recording literals and distance/length pairs
//! - Bit-level output buffer management (`send_bits`, `bi_flush`, etc.)

use crate::constants::{
    BL_CODES, BUF_SIZE, D_CODES, DYN_TREES, HEAP_SIZE, L_CODES, LENGTH_CODES, LITERALS, MAX_BITS,
    MAX_MATCH, MIN_MATCH, STATIC_TREES, STORED_BLOCK, Z_BINARY, Z_FIXED, Z_TEXT,
};
use crate::deflate::state::{DeflateState, END_BLOCK, HuffmanNode, StaticTreeDesc, TreeType};

// ===========================================================================
// Local constants (from trees.c lines 47–81)
// ===========================================================================

/// Maximum bit length for bit-length codes.
const MAX_BL_BITS: usize = 7;

/// Repeat previous bit length 3–6 times (2 bits of repeat count).
const REP_3_6: usize = 16;

/// Repeat a zero length 3–10 times (3 bits of repeat count).
const REPZ_3_10: usize = 17;

/// Repeat a zero length 11–138 times (7 bits of repeat count).
const REPZ_11_138: usize = 18;

/// Size of the distance-code lookup table.
const DIST_CODE_LEN: usize = 512;

/// Index of the smallest element in the heap (heap[0] is unused).
const SMALLEST: usize = 1;

// ===========================================================================
// Extra-bits tables  (trees.c lines 62–69)
// NOTE: typed as `[u8; _]` to match `StaticTreeDesc.extra_bits: &'static [u8]`
// ===========================================================================

/// Extra bits for each length code (0–28).
const EXTRA_LBITS: [u8; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each distance code (0–29).
const EXTRA_DBITS: [u8; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits for each bit-length code.
const EXTRA_BLBITS: [u8; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Order of the bit-length code lengths when transmitted in the compressed
/// block header.  Per RFC 1951 §3.2.7.
const BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ===========================================================================
// Static literal/length Huffman tree — 288 entries  (trees.h)
// Each entry stores (code, bitlen) in the HuffmanNode (fc, dl) layout.
// ===========================================================================

/// Pre-computed static literal/length Huffman tree.
///
/// Codes   0–143 : 8-bit codes 0x030–0x0BF (reversed)
/// Codes 144–255 : 9-bit codes 0x190–0x1FF (reversed)
/// Codes 256–279 : 7-bit codes 0x000–0x017 (reversed)
/// Codes 280–287 : 8-bit codes 0x0C0–0x0C7 (reversed)
pub(crate) const STATIC_LTREE: [HuffmanNode; L_CODES + 2] = [
    HuffmanNode { fc: 12, dl: 8 },
    HuffmanNode { fc: 140, dl: 8 },
    HuffmanNode { fc: 76, dl: 8 },
    HuffmanNode { fc: 204, dl: 8 },
    HuffmanNode { fc: 44, dl: 8 },
    HuffmanNode { fc: 172, dl: 8 },
    HuffmanNode { fc: 108, dl: 8 },
    HuffmanNode { fc: 236, dl: 8 },
    HuffmanNode { fc: 28, dl: 8 },
    HuffmanNode { fc: 156, dl: 8 },
    HuffmanNode { fc: 92, dl: 8 },
    HuffmanNode { fc: 220, dl: 8 },
    HuffmanNode { fc: 60, dl: 8 },
    HuffmanNode { fc: 188, dl: 8 },
    HuffmanNode { fc: 124, dl: 8 },
    HuffmanNode { fc: 252, dl: 8 },
    HuffmanNode { fc: 2, dl: 8 },
    HuffmanNode { fc: 130, dl: 8 },
    HuffmanNode { fc: 66, dl: 8 },
    HuffmanNode { fc: 194, dl: 8 },
    HuffmanNode { fc: 34, dl: 8 },
    HuffmanNode { fc: 162, dl: 8 },
    HuffmanNode { fc: 98, dl: 8 },
    HuffmanNode { fc: 226, dl: 8 },
    HuffmanNode { fc: 18, dl: 8 },
    HuffmanNode { fc: 146, dl: 8 },
    HuffmanNode { fc: 82, dl: 8 },
    HuffmanNode { fc: 210, dl: 8 },
    HuffmanNode { fc: 50, dl: 8 },
    HuffmanNode { fc: 178, dl: 8 },
    HuffmanNode { fc: 114, dl: 8 },
    HuffmanNode { fc: 242, dl: 8 },
    HuffmanNode { fc: 10, dl: 8 },
    HuffmanNode { fc: 138, dl: 8 },
    HuffmanNode { fc: 74, dl: 8 },
    HuffmanNode { fc: 202, dl: 8 },
    HuffmanNode { fc: 42, dl: 8 },
    HuffmanNode { fc: 170, dl: 8 },
    HuffmanNode { fc: 106, dl: 8 },
    HuffmanNode { fc: 234, dl: 8 },
    HuffmanNode { fc: 26, dl: 8 },
    HuffmanNode { fc: 154, dl: 8 },
    HuffmanNode { fc: 90, dl: 8 },
    HuffmanNode { fc: 218, dl: 8 },
    HuffmanNode { fc: 58, dl: 8 },
    HuffmanNode { fc: 186, dl: 8 },
    HuffmanNode { fc: 122, dl: 8 },
    HuffmanNode { fc: 250, dl: 8 },
    HuffmanNode { fc: 6, dl: 8 },
    HuffmanNode { fc: 134, dl: 8 },
    HuffmanNode { fc: 70, dl: 8 },
    HuffmanNode { fc: 198, dl: 8 },
    HuffmanNode { fc: 38, dl: 8 },
    HuffmanNode { fc: 166, dl: 8 },
    HuffmanNode { fc: 102, dl: 8 },
    HuffmanNode { fc: 230, dl: 8 },
    HuffmanNode { fc: 22, dl: 8 },
    HuffmanNode { fc: 150, dl: 8 },
    HuffmanNode { fc: 86, dl: 8 },
    HuffmanNode { fc: 214, dl: 8 },
    HuffmanNode { fc: 54, dl: 8 },
    HuffmanNode { fc: 182, dl: 8 },
    HuffmanNode { fc: 118, dl: 8 },
    HuffmanNode { fc: 246, dl: 8 },
    HuffmanNode { fc: 14, dl: 8 },
    HuffmanNode { fc: 142, dl: 8 },
    HuffmanNode { fc: 78, dl: 8 },
    HuffmanNode { fc: 206, dl: 8 },
    HuffmanNode { fc: 46, dl: 8 },
    HuffmanNode { fc: 174, dl: 8 },
    HuffmanNode { fc: 110, dl: 8 },
    HuffmanNode { fc: 238, dl: 8 },
    HuffmanNode { fc: 30, dl: 8 },
    HuffmanNode { fc: 158, dl: 8 },
    HuffmanNode { fc: 94, dl: 8 },
    HuffmanNode { fc: 222, dl: 8 },
    HuffmanNode { fc: 62, dl: 8 },
    HuffmanNode { fc: 190, dl: 8 },
    HuffmanNode { fc: 126, dl: 8 },
    HuffmanNode { fc: 254, dl: 8 },
    HuffmanNode { fc: 1, dl: 8 },
    HuffmanNode { fc: 129, dl: 8 },
    HuffmanNode { fc: 65, dl: 8 },
    HuffmanNode { fc: 193, dl: 8 },
    HuffmanNode { fc: 33, dl: 8 },
    HuffmanNode { fc: 161, dl: 8 },
    HuffmanNode { fc: 97, dl: 8 },
    HuffmanNode { fc: 225, dl: 8 },
    HuffmanNode { fc: 17, dl: 8 },
    HuffmanNode { fc: 145, dl: 8 },
    HuffmanNode { fc: 81, dl: 8 },
    HuffmanNode { fc: 209, dl: 8 },
    HuffmanNode { fc: 49, dl: 8 },
    HuffmanNode { fc: 177, dl: 8 },
    HuffmanNode { fc: 113, dl: 8 },
    HuffmanNode { fc: 241, dl: 8 },
    HuffmanNode { fc: 9, dl: 8 },
    HuffmanNode { fc: 137, dl: 8 },
    HuffmanNode { fc: 73, dl: 8 },
    HuffmanNode { fc: 201, dl: 8 },
    HuffmanNode { fc: 41, dl: 8 },
    HuffmanNode { fc: 169, dl: 8 },
    HuffmanNode { fc: 105, dl: 8 },
    HuffmanNode { fc: 233, dl: 8 },
    HuffmanNode { fc: 25, dl: 8 },
    HuffmanNode { fc: 153, dl: 8 },
    HuffmanNode { fc: 89, dl: 8 },
    HuffmanNode { fc: 217, dl: 8 },
    HuffmanNode { fc: 57, dl: 8 },
    HuffmanNode { fc: 185, dl: 8 },
    HuffmanNode { fc: 121, dl: 8 },
    HuffmanNode { fc: 249, dl: 8 },
    HuffmanNode { fc: 5, dl: 8 },
    HuffmanNode { fc: 133, dl: 8 },
    HuffmanNode { fc: 69, dl: 8 },
    HuffmanNode { fc: 197, dl: 8 },
    HuffmanNode { fc: 37, dl: 8 },
    HuffmanNode { fc: 165, dl: 8 },
    HuffmanNode { fc: 101, dl: 8 },
    HuffmanNode { fc: 229, dl: 8 },
    HuffmanNode { fc: 21, dl: 8 },
    HuffmanNode { fc: 149, dl: 8 },
    HuffmanNode { fc: 85, dl: 8 },
    HuffmanNode { fc: 213, dl: 8 },
    HuffmanNode { fc: 53, dl: 8 },
    HuffmanNode { fc: 181, dl: 8 },
    HuffmanNode { fc: 117, dl: 8 },
    HuffmanNode { fc: 245, dl: 8 },
    HuffmanNode { fc: 13, dl: 8 },
    HuffmanNode { fc: 141, dl: 8 },
    HuffmanNode { fc: 77, dl: 8 },
    HuffmanNode { fc: 205, dl: 8 },
    HuffmanNode { fc: 45, dl: 8 },
    HuffmanNode { fc: 173, dl: 8 },
    HuffmanNode { fc: 109, dl: 8 },
    HuffmanNode { fc: 237, dl: 8 },
    HuffmanNode { fc: 29, dl: 8 },
    HuffmanNode { fc: 157, dl: 8 },
    // --- entries 144–255 (9-bit codes) ---
    HuffmanNode { fc: 93, dl: 8 },
    HuffmanNode { fc: 221, dl: 8 },
    HuffmanNode { fc: 61, dl: 8 },
    HuffmanNode { fc: 189, dl: 8 },
    HuffmanNode { fc: 125, dl: 8 },
    HuffmanNode { fc: 253, dl: 8 },
    HuffmanNode { fc: 19, dl: 9 },
    HuffmanNode { fc: 275, dl: 9 },
    HuffmanNode { fc: 147, dl: 9 },
    HuffmanNode { fc: 403, dl: 9 },
    HuffmanNode { fc: 83, dl: 9 },
    HuffmanNode { fc: 339, dl: 9 },
    HuffmanNode { fc: 211, dl: 9 },
    HuffmanNode { fc: 467, dl: 9 },
    HuffmanNode { fc: 51, dl: 9 },
    HuffmanNode { fc: 307, dl: 9 },
    HuffmanNode { fc: 179, dl: 9 },
    HuffmanNode { fc: 435, dl: 9 },
    HuffmanNode { fc: 115, dl: 9 },
    HuffmanNode { fc: 371, dl: 9 },
    HuffmanNode { fc: 243, dl: 9 },
    HuffmanNode { fc: 499, dl: 9 },
    HuffmanNode { fc: 11, dl: 9 },
    HuffmanNode { fc: 267, dl: 9 },
    HuffmanNode { fc: 139, dl: 9 },
    HuffmanNode { fc: 395, dl: 9 },
    HuffmanNode { fc: 75, dl: 9 },
    HuffmanNode { fc: 331, dl: 9 },
    HuffmanNode { fc: 203, dl: 9 },
    HuffmanNode { fc: 459, dl: 9 },
    HuffmanNode { fc: 43, dl: 9 },
    HuffmanNode { fc: 299, dl: 9 },
    HuffmanNode { fc: 171, dl: 9 },
    HuffmanNode { fc: 427, dl: 9 },
    HuffmanNode { fc: 107, dl: 9 },
    HuffmanNode { fc: 363, dl: 9 },
    HuffmanNode { fc: 235, dl: 9 },
    HuffmanNode { fc: 491, dl: 9 },
    HuffmanNode { fc: 27, dl: 9 },
    HuffmanNode { fc: 283, dl: 9 },
    HuffmanNode { fc: 155, dl: 9 },
    HuffmanNode { fc: 411, dl: 9 },
    HuffmanNode { fc: 91, dl: 9 },
    HuffmanNode { fc: 347, dl: 9 },
    HuffmanNode { fc: 219, dl: 9 },
    HuffmanNode { fc: 475, dl: 9 },
    HuffmanNode { fc: 59, dl: 9 },
    HuffmanNode { fc: 315, dl: 9 },
    HuffmanNode { fc: 187, dl: 9 },
    HuffmanNode { fc: 443, dl: 9 },
    HuffmanNode { fc: 123, dl: 9 },
    HuffmanNode { fc: 379, dl: 9 },
    HuffmanNode { fc: 251, dl: 9 },
    HuffmanNode { fc: 507, dl: 9 },
    HuffmanNode { fc: 7, dl: 9 },
    HuffmanNode { fc: 263, dl: 9 },
    HuffmanNode { fc: 135, dl: 9 },
    HuffmanNode { fc: 391, dl: 9 },
    HuffmanNode { fc: 71, dl: 9 },
    HuffmanNode { fc: 327, dl: 9 },
    HuffmanNode { fc: 199, dl: 9 },
    HuffmanNode { fc: 455, dl: 9 },
    HuffmanNode { fc: 39, dl: 9 },
    HuffmanNode { fc: 295, dl: 9 },
    HuffmanNode { fc: 167, dl: 9 },
    HuffmanNode { fc: 423, dl: 9 },
    HuffmanNode { fc: 103, dl: 9 },
    HuffmanNode { fc: 359, dl: 9 },
    HuffmanNode { fc: 231, dl: 9 },
    HuffmanNode { fc: 487, dl: 9 },
    HuffmanNode { fc: 23, dl: 9 },
    HuffmanNode { fc: 279, dl: 9 },
    HuffmanNode { fc: 151, dl: 9 },
    HuffmanNode { fc: 407, dl: 9 },
    HuffmanNode { fc: 87, dl: 9 },
    HuffmanNode { fc: 343, dl: 9 },
    HuffmanNode { fc: 215, dl: 9 },
    HuffmanNode { fc: 471, dl: 9 },
    HuffmanNode { fc: 55, dl: 9 },
    HuffmanNode { fc: 311, dl: 9 },
    HuffmanNode { fc: 183, dl: 9 },
    HuffmanNode { fc: 439, dl: 9 },
    HuffmanNode { fc: 119, dl: 9 },
    HuffmanNode { fc: 375, dl: 9 },
    HuffmanNode { fc: 247, dl: 9 },
    HuffmanNode { fc: 503, dl: 9 },
    HuffmanNode { fc: 15, dl: 9 },
    HuffmanNode { fc: 271, dl: 9 },
    HuffmanNode { fc: 143, dl: 9 },
    HuffmanNode { fc: 399, dl: 9 },
    HuffmanNode { fc: 79, dl: 9 },
    HuffmanNode { fc: 335, dl: 9 },
    HuffmanNode { fc: 207, dl: 9 },
    HuffmanNode { fc: 463, dl: 9 },
    HuffmanNode { fc: 47, dl: 9 },
    HuffmanNode { fc: 303, dl: 9 },
    HuffmanNode { fc: 175, dl: 9 },
    HuffmanNode { fc: 431, dl: 9 },
    HuffmanNode { fc: 111, dl: 9 },
    HuffmanNode { fc: 367, dl: 9 },
    HuffmanNode { fc: 239, dl: 9 },
    HuffmanNode { fc: 495, dl: 9 },
    HuffmanNode { fc: 31, dl: 9 },
    HuffmanNode { fc: 287, dl: 9 },
    HuffmanNode { fc: 159, dl: 9 },
    HuffmanNode { fc: 415, dl: 9 },
    HuffmanNode { fc: 95, dl: 9 },
    HuffmanNode { fc: 351, dl: 9 },
    HuffmanNode { fc: 223, dl: 9 },
    HuffmanNode { fc: 479, dl: 9 },
    HuffmanNode { fc: 63, dl: 9 },
    HuffmanNode { fc: 319, dl: 9 },
    HuffmanNode { fc: 191, dl: 9 },
    HuffmanNode { fc: 447, dl: 9 },
    HuffmanNode { fc: 127, dl: 9 },
    HuffmanNode { fc: 383, dl: 9 },
    HuffmanNode { fc: 255, dl: 9 },
    HuffmanNode { fc: 511, dl: 9 },
    // --- entries 256–279 (7-bit codes) ---
    HuffmanNode { fc: 0, dl: 7 },
    HuffmanNode { fc: 64, dl: 7 },
    HuffmanNode { fc: 32, dl: 7 },
    HuffmanNode { fc: 96, dl: 7 },
    HuffmanNode { fc: 16, dl: 7 },
    HuffmanNode { fc: 80, dl: 7 },
    HuffmanNode { fc: 48, dl: 7 },
    HuffmanNode { fc: 112, dl: 7 },
    HuffmanNode { fc: 8, dl: 7 },
    HuffmanNode { fc: 72, dl: 7 },
    HuffmanNode { fc: 40, dl: 7 },
    HuffmanNode { fc: 104, dl: 7 },
    HuffmanNode { fc: 24, dl: 7 },
    HuffmanNode { fc: 88, dl: 7 },
    HuffmanNode { fc: 56, dl: 7 },
    HuffmanNode { fc: 120, dl: 7 },
    HuffmanNode { fc: 4, dl: 7 },
    HuffmanNode { fc: 68, dl: 7 },
    HuffmanNode { fc: 36, dl: 7 },
    HuffmanNode { fc: 100, dl: 7 },
    HuffmanNode { fc: 20, dl: 7 },
    HuffmanNode { fc: 84, dl: 7 },
    HuffmanNode { fc: 52, dl: 7 },
    HuffmanNode { fc: 116, dl: 7 },
    // --- entries 280–287 (8-bit codes) ---
    HuffmanNode { fc: 3, dl: 8 },
    HuffmanNode { fc: 131, dl: 8 },
    HuffmanNode { fc: 67, dl: 8 },
    HuffmanNode { fc: 195, dl: 8 },
    HuffmanNode { fc: 35, dl: 8 },
    HuffmanNode { fc: 163, dl: 8 },
    HuffmanNode { fc: 99, dl: 8 },
    HuffmanNode { fc: 227, dl: 8 },
];

// ===========================================================================
// Static distance Huffman tree - 30 entries  (trees.h)
// All 5-bit codes, bit-reversed.
// ===========================================================================

/// Pre-computed static distance Huffman tree (30 entries, all 5-bit codes).
pub(crate) const STATIC_DTREE: [HuffmanNode; D_CODES] = [
    HuffmanNode { fc: 0, dl: 5 },
    HuffmanNode { fc: 16, dl: 5 },
    HuffmanNode { fc: 8, dl: 5 },
    HuffmanNode { fc: 24, dl: 5 },
    HuffmanNode { fc: 4, dl: 5 },
    HuffmanNode { fc: 20, dl: 5 },
    HuffmanNode { fc: 12, dl: 5 },
    HuffmanNode { fc: 28, dl: 5 },
    HuffmanNode { fc: 2, dl: 5 },
    HuffmanNode { fc: 18, dl: 5 },
    HuffmanNode { fc: 10, dl: 5 },
    HuffmanNode { fc: 26, dl: 5 },
    HuffmanNode { fc: 6, dl: 5 },
    HuffmanNode { fc: 22, dl: 5 },
    HuffmanNode { fc: 14, dl: 5 },
    HuffmanNode { fc: 30, dl: 5 },
    HuffmanNode { fc: 1, dl: 5 },
    HuffmanNode { fc: 17, dl: 5 },
    HuffmanNode { fc: 9, dl: 5 },
    HuffmanNode { fc: 25, dl: 5 },
    HuffmanNode { fc: 5, dl: 5 },
    HuffmanNode { fc: 21, dl: 5 },
    HuffmanNode { fc: 13, dl: 5 },
    HuffmanNode { fc: 29, dl: 5 },
    HuffmanNode { fc: 3, dl: 5 },
    HuffmanNode { fc: 19, dl: 5 },
    HuffmanNode { fc: 11, dl: 5 },
    HuffmanNode { fc: 27, dl: 5 },
    HuffmanNode { fc: 7, dl: 5 },
    HuffmanNode { fc: 23, dl: 5 },
];

// ===========================================================================
// Distance-code lookup table - 512 entries  (trees.h _dist_code)
// ===========================================================================

/// Maps a distance value (or distance >> 7 + 256) to a distance code (0-29).
pub(crate) const DIST_CODE: [u8; DIST_CODE_LEN] = [
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

// ===========================================================================
// Length-code lookup table - 256 entries  (trees.h _length_code)
// Maps (match_length - MIN_MATCH) to length code (0-28).
// ===========================================================================

/// Maps (match_length - MIN_MATCH) to a length code (0-28).
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

// ===========================================================================
// Base length / distance tables  (trees.h)
// ===========================================================================

/// Base length value for each length code (0-28).
pub(crate) const BASE_LENGTH: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224, 0,
];

/// Base distance value for each distance code (0-29).
pub(crate) const BASE_DIST: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

// ===========================================================================
// Static tree descriptors
// ===========================================================================

/// Static tree descriptor for the literal/length tree.
pub(crate) static STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_LTREE),
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// Static tree descriptor for the distance tree.
pub(crate) static STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&STATIC_DTREE),
    extra_bits: &EXTRA_DBITS,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// Static tree descriptor for the bit-length tree.
pub(crate) static STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_BLBITS,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

// ===========================================================================
// Tree-access helpers - abstract over TreeType to avoid borrow conflicts
// ===========================================================================

/// Read the fc field (frequency/code) of tree node idx.
#[inline(always)]
fn tree_fc(state: &DeflateState, tt: TreeType, idx: usize) -> u16 {
    match tt {
        TreeType::Literal => state.dyn_ltree[idx].fc,
        TreeType::Distance => state.dyn_dtree[idx].fc,
        TreeType::BitLength => state.bl_tree[idx].fc,
    }
}

/// Write the fc field (frequency/code) of tree node idx.
#[inline(always)]
fn set_tree_fc(state: &mut DeflateState, tt: TreeType, idx: usize, val: u16) {
    match tt {
        TreeType::Literal => state.dyn_ltree[idx].fc = val,
        TreeType::Distance => state.dyn_dtree[idx].fc = val,
        TreeType::BitLength => state.bl_tree[idx].fc = val,
    }
}

/// Read the dl field (dad/len) of tree node idx.
#[inline(always)]
fn tree_dl(state: &DeflateState, tt: TreeType, idx: usize) -> u16 {
    match tt {
        TreeType::Literal => state.dyn_ltree[idx].dl,
        TreeType::Distance => state.dyn_dtree[idx].dl,
        TreeType::BitLength => state.bl_tree[idx].dl,
    }
}

/// Write the dl field (dad/len) of tree node idx.
#[inline(always)]
fn set_tree_dl(state: &mut DeflateState, tt: TreeType, idx: usize, val: u16) {
    match tt {
        TreeType::Literal => state.dyn_ltree[idx].dl = val,
        TreeType::Distance => state.dyn_dtree[idx].dl = val,
        TreeType::BitLength => state.bl_tree[idx].dl = val,
    }
}

/// Increment fc (frequency) of tree node idx by one, wrapping on overflow.
#[allow(dead_code)]
#[inline(always)]
fn inc_tree_fc(state: &mut DeflateState, tt: TreeType, idx: usize) {
    match tt {
        TreeType::Literal => state.dyn_ltree[idx].fc = state.dyn_ltree[idx].fc.wrapping_add(1),
        TreeType::Distance => state.dyn_dtree[idx].fc = state.dyn_dtree[idx].fc.wrapping_add(1),
        TreeType::BitLength => state.bl_tree[idx].fc = state.bl_tree[idx].fc.wrapping_add(1),
    }
}

// ===========================================================================
// Bit-level output helpers
// ===========================================================================

/// Append a single byte to the pending output buffer.
#[inline(always)]
fn put_byte(state: &mut DeflateState, c: u8) {
    state.pending_buf[state.pending] = c;
    state.pending += 1;
}

/// Append a 16-bit value in little-endian order to the pending output buffer.
#[inline(always)]
fn put_short(state: &mut DeflateState, w: u16) {
    put_byte(state, (w & 0xff) as u8);
    put_byte(state, (w >> 8) as u8);
}

/// Reverse the first len bits of code.
fn bi_reverse(mut code: u32, mut len: usize) -> u32 {
    let mut res: u32 = 0;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            break;
        }
    }
    res >> 1
}

/// Send length low-order bits of value to the output stream.
///
/// When the bit buffer overflows (bi_valid + length > 16), the bottom 16 bits
/// are flushed as a little-endian short and the remainder is stored for the
/// next call.
#[inline(always)]
fn send_bits(state: &mut DeflateState, value: u32, length: usize) {
    let total = state.bi_valid as usize + length;
    if total <= BUF_SIZE {
        state.bi_buf |= (value as u16) << state.bi_valid;
        state.bi_valid += length as i32;
    } else {
        let combined = state.bi_buf as u32 | (value << state.bi_valid as u32);
        put_short(state, combined as u16);
        state.bi_buf = (value >> (BUF_SIZE - state.bi_valid as usize)) as u16;
        state.bi_valid += length as i32 - BUF_SIZE as i32;
    }
}

/// Send a Huffman code from a tree slice.
#[inline(always)]
fn send_code(state: &mut DeflateState, c: usize, tree: &[HuffmanNode]) {
    let code = tree[c].fc as u32;
    let len = tree[c].dl as usize;
    send_bits(state, code, len);
}

/// Send a Huffman code from the bl_tree stored in state.
#[inline(always)]
fn send_code_bl(state: &mut DeflateState, c: usize) {
    let code = state.bl_tree[c].fc as u32;
    let len = state.bl_tree[c].dl as usize;
    send_bits(state, code, len);
}

/// Flush the bit output buffer. If more than 8 valid bits, write a 16-bit
/// short; if 1-8 valid bits, write one byte.
fn bi_flush(state: &mut DeflateState) {
    if state.bi_valid == 16 {
        put_short(state, state.bi_buf);
        state.bi_buf = 0;
        state.bi_valid = 0;
    } else if state.bi_valid >= 8 {
        put_byte(state, (state.bi_buf & 0xff) as u8);
        state.bi_buf >>= 8;
        state.bi_valid -= 8;
    }
}

/// Flush the bit buffer and align output to the next byte boundary.
fn bi_windup(state: &mut DeflateState) {
    if state.bi_valid > 8 {
        put_short(state, state.bi_buf);
    } else if state.bi_valid > 0 {
        put_byte(state, (state.bi_buf & 0xff) as u8);
    }
    state.bi_buf = 0;
    state.bi_valid = 0;
}

/// Public wrapper around bi_flush.
pub fn tr_flush_bits(state: &mut DeflateState) {
    bi_flush(state);
}

/// Copy a stored block, prepending a 4-byte LEN/NLEN header if header is
/// true. Aligns output to a byte boundary first.
fn copy_block(state: &mut DeflateState, buf: &[u8], header: bool) {
    bi_windup(state);
    if header {
        let len = buf.len() as u16;
        put_short(state, len);
        put_short(state, !len);
    }
    let start = state.pending;
    let end = start + buf.len();
    state.pending_buf[start..end].copy_from_slice(buf);
    state.pending += buf.len();
}

// ===========================================================================
// Huffman tree construction
// ===========================================================================

/// Generate canonical Huffman codes from a vector of bit-lengths (stored in
/// tree[n].dl for n = 0 .. max_code), using the algorithm from RFC 1951
/// section 3.2.2. The result codes are stored back in tree[n].fc.
fn gen_codes(state: &mut DeflateState, tt: TreeType, max_code: usize) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;

    #[allow(clippy::needless_range_loop)]
    for bits in 1..=MAX_BITS {
        code = (code + state.bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }

    for n in 0..=max_code {
        let len = tree_dl(state, tt, n) as usize;
        if len == 0 {
            continue;
        }
        let reversed = bi_reverse(next_code[len] as u32, len);
        set_tree_fc(state, tt, n, reversed as u16);
        next_code[len] += 1;
    }
}

/// Compare two tree nodes using the "smaller" predicate used for the priority
/// queue. Node a is smaller than b if it has a lower frequency, or the
/// same frequency but lesser depth (to break ties deterministically).
#[inline(always)]
fn smaller(state: &DeflateState, tt: TreeType, a: usize, b: usize) -> bool {
    let fa = tree_fc(state, tt, a);
    let fb = tree_fc(state, tt, b);
    fa < fb || (fa == fb && state.depth[a] <= state.depth[b])
}

/// Restore the heap property by sifting element k downward.
fn pqdownheap(state: &mut DeflateState, tt: TreeType, mut k: usize) {
    let v = state.heap[k];
    let mut j = k << 1;
    while j <= state.heap_len as usize {
        if j < state.heap_len as usize {
            let left = state.heap[j] as usize;
            let right = state.heap[j + 1] as usize;
            if smaller(state, tt, right, left) {
                j += 1;
            }
        }
        let hj = state.heap[j] as usize;
        if smaller(state, tt, v as usize, hj) {
            break;
        }
        state.heap[k] = state.heap[j];
        k = j;
        j <<= 1;
    }
    state.heap[k] = v;
}

/// Compute the optimal bit lengths for a tree, honouring the max_length
/// constraint. Updates bl_count, opt_len, and static_len in state.
fn gen_bitlen(state: &mut DeflateState, tt: TreeType) {
    let stat_desc: &'static StaticTreeDesc = match tt {
        TreeType::Literal => state.l_desc.stat_desc,
        TreeType::Distance => state.d_desc.stat_desc,
        TreeType::BitLength => state.bl_desc.stat_desc,
    };
    let max_code = match tt {
        TreeType::Literal => state.l_desc.max_code,
        TreeType::Distance => state.d_desc.max_code,
        TreeType::BitLength => state.bl_desc.max_code,
    };
    let stree = stat_desc.static_tree;
    let extra = stat_desc.extra_bits;
    let base = stat_desc.extra_base;
    let max_length = stat_desc.max_length;

    for b in 0..=MAX_BITS {
        state.bl_count[b] = 0;
    }

    // Root of the heap gets bit-length = 0.
    let root = state.heap[state.heap_max as usize] as usize;
    set_tree_dl(state, tt, root, 0);

    let mut overflow: i32 = 0;

    for h in (state.heap_max as usize + 1)..HEAP_SIZE {
        let n = state.heap[h] as usize;
        let parent_len = tree_dl(state, tt, tree_dl(state, tt, n) as usize);
        let mut bits = parent_len as i32 + 1;
        if bits > max_length as i32 {
            bits = max_length as i32;
            overflow += 1;
        }
        set_tree_dl(state, tt, n, bits as u16);

        if n > max_code as usize {
            continue;
        }

        state.bl_count[bits as usize] += 1;
        let xbits = if n >= base { extra[n - base] as u32 } else { 0 };
        let f = tree_fc(state, tt, n) as u64;
        // Use wrapping arithmetic — C zlib relies on unsigned modular math
        // when filler nodes cause opt_len to underflow temporarily.
        state.opt_len = state
            .opt_len
            .wrapping_add(f.wrapping_mul((bits as u32 + xbits) as u64));
        if let Some(st) = stree {
            state.static_len = state
                .static_len
                .wrapping_add(f.wrapping_mul((st[n].dl as u32 + xbits) as u64));
        }
    }

    if overflow == 0 {
        return;
    }

    // Redistribute the excess bits.
    loop {
        let mut bits = max_length as i32 - 1;
        while state.bl_count[bits as usize] == 0 {
            bits -= 1;
        }
        state.bl_count[bits as usize] -= 1;
        state.bl_count[(bits + 1) as usize] += 2;
        state.bl_count[max_length] -= 1;
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // Reassign bit lengths to match the redistributed counts.
    let mut h = HEAP_SIZE;
    for bits in (1..=max_length).rev() {
        let mut n = state.bl_count[bits] as i32;
        while n != 0 {
            h -= 1;
            let m = state.heap[h] as usize;
            if m > max_code as usize {
                continue;
            }
            let cur_len = tree_dl(state, tt, m) as u64;
            if cur_len != bits as u64 {
                let adj = (bits as i64 - cur_len as i64) * tree_fc(state, tt, m) as i64;
                state.opt_len = (state.opt_len as i64).wrapping_add(adj) as u64;
                set_tree_dl(state, tt, m, bits as u16);
            }
            n -= 1;
        }
    }
}

/// Build a Huffman tree for the given tree type. The tree data resides in the
/// DeflateState (dyn_ltree / dyn_dtree / bl_tree). After this call, the tree
/// contains canonical Huffman codes and the descriptor's max_code is updated.
fn build_tree(state: &mut DeflateState, tt: TreeType) {
    let stat_desc: &'static StaticTreeDesc = match tt {
        TreeType::Literal => state.l_desc.stat_desc,
        TreeType::Distance => state.d_desc.stat_desc,
        TreeType::BitLength => state.bl_desc.stat_desc,
    };
    let elems = stat_desc.elems;
    let stree = stat_desc.static_tree;

    let mut max_code: i32 = -1;
    state.heap_len = 0;
    state.heap_max = HEAP_SIZE as i32;

    // Build the initial heap from non-zero frequency nodes.
    for n in 0..elems {
        if tree_fc(state, tt, n) != 0 {
            state.heap_len += 1;
            state.heap[state.heap_len as usize] = n as i32;
            max_code = n as i32;
            state.depth[n] = 0;
        } else {
            set_tree_dl(state, tt, n, 0);
        }
    }

    // Ensure we have at least 2 codes so the tree is non-degenerate.
    while state.heap_len < 2 {
        state.heap_len += 1;
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        state.heap[state.heap_len as usize] = node as i32;
        set_tree_fc(state, tt, node, 1);
        state.depth[node] = 0;
        // Use wrapping subtraction — C zlib relies on unsigned wrap-around
        // when opt_len is 0 at this point; the arithmetic corrects itself
        // later in gen_bitlen when the bit cost for this filler node is added.
        state.opt_len = state.opt_len.wrapping_sub(1);
        if let Some(st) = stree {
            state.static_len = state.static_len.wrapping_sub(st[node].dl as u64);
        }
    }

    // Store max_code in the descriptor.
    match tt {
        TreeType::Literal => state.l_desc.max_code = max_code,
        TreeType::Distance => state.d_desc.max_code = max_code,
        TreeType::BitLength => state.bl_desc.max_code = max_code,
    }

    // Establish the heap property.
    let half = state.heap_len / 2;
    for n in (1..=half as usize).rev() {
        pqdownheap(state, tt, n);
    }

    // Combine the two least-frequent nodes until only the root remains.
    let mut node = elems;
    loop {
        // Extract minimum.
        let n = state.heap[SMALLEST];
        state.heap[SMALLEST] = state.heap[state.heap_len as usize];
        state.heap_len -= 1;
        pqdownheap(state, tt, SMALLEST);

        let m = state.heap[SMALLEST]; // second-minimum

        // Store the two extracted nodes as children of a new internal node.
        state.heap_max -= 1;
        state.heap[state.heap_max as usize] = n;
        state.heap_max -= 1;
        state.heap[state.heap_max as usize] = m;

        // Create the new internal node.
        let freq_n = tree_fc(state, tt, n as usize);
        let freq_m = tree_fc(state, tt, m as usize);
        set_tree_fc(state, tt, node, freq_n.wrapping_add(freq_m));
        let d = core::cmp::max(state.depth[n as usize], state.depth[m as usize]);
        state.depth[node] = d + 1;
        set_tree_dl(state, tt, n as usize, node as u16);
        set_tree_dl(state, tt, m as usize, node as u16);

        // Replace the root of the heap with the new node and sift down.
        state.heap[SMALLEST] = node as i32;
        node += 1;
        pqdownheap(state, tt, SMALLEST);

        if state.heap_len < 2 {
            break;
        }
    }

    state.heap_max -= 1;
    state.heap[state.heap_max as usize] = state.heap[SMALLEST];

    // Compute optimal bit-lengths and generate canonical codes.
    gen_bitlen(state, tt);
    let mc = match tt {
        TreeType::Literal => state.l_desc.max_code,
        TreeType::Distance => state.d_desc.max_code,
        TreeType::BitLength => state.bl_desc.max_code,
    };
    gen_codes(state, tt, mc as usize);
}

// ===========================================================================
// Initialisation
// ===========================================================================

/// Initialise a new deflate block: zero all tree frequencies, set the
/// END_BLOCK marker, and reset block statistics.
fn init_block(state: &mut DeflateState) {
    for n in 0..L_CODES {
        state.dyn_ltree[n].fc = 0;
    }
    for n in 0..D_CODES {
        state.dyn_dtree[n].fc = 0;
    }
    for n in 0..BL_CODES {
        state.bl_tree[n].fc = 0;
    }

    state.dyn_ltree[END_BLOCK].fc = 1;
    state.opt_len = 0;
    state.static_len = 0;
    state.sym_next = 0;
    state.matches = 0;
}

/// Initialise the tree module: wire up static descriptors and prepare the
/// first block.
pub fn tr_init(state: &mut DeflateState) {
    state.l_desc.stat_desc = &STATIC_L_DESC;
    state.d_desc.stat_desc = &STATIC_D_DESC;
    state.bl_desc.stat_desc = &STATIC_BL_DESC;

    state.bi_buf = 0;
    state.bi_valid = 0;

    init_block(state);
}

// ===========================================================================
// Tree serialisation for dynamic block headers
// ===========================================================================

/// Scan a tree to determine the frequency of each bit-length code.
/// Sets a sentinel at tree[max_code + 1].dl = 0xffff as a guard.
fn scan_tree(state: &mut DeflateState, tt: TreeType, max_code: usize) {
    set_tree_dl(state, tt, max_code + 1, 0xffff); // sentinel

    let mut prevlen: i32 = -1;
    let mut nextlen: i32 = tree_dl(state, tt, 0) as i32;
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree_dl(state, tt, n + 1) as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        }
        if count < min_count {
            let old = state.bl_tree[curlen as usize].fc;
            state.bl_tree[curlen as usize].fc = old.wrapping_add(count as u16);
        } else if curlen != 0 {
            if curlen != prevlen {
                let old = state.bl_tree[curlen as usize].fc;
                state.bl_tree[curlen as usize].fc = old.wrapping_add(1);
            }
            let old = state.bl_tree[REP_3_6].fc;
            state.bl_tree[REP_3_6].fc = old.wrapping_add(1);
        } else if count <= 10 {
            let old = state.bl_tree[REPZ_3_10].fc;
            state.bl_tree[REPZ_3_10].fc = old.wrapping_add(1);
        } else {
            let old = state.bl_tree[REPZ_11_138].fc;
            state.bl_tree[REPZ_11_138].fc = old.wrapping_add(1);
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

/// Send a literal/distance tree encoded with the bit-length tree.
fn send_tree(state: &mut DeflateState, tt: TreeType, max_code: usize) {
    let mut prevlen: i32 = -1;
    let mut nextlen: i32 = tree_dl(state, tt, 0) as i32;
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree_dl(state, tt, n + 1) as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        }
        if count < min_count {
            for _ in 0..count {
                send_code_bl(state, curlen as usize);
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                send_code_bl(state, curlen as usize);
                count -= 1;
            }
            debug_assert!((3..=6).contains(&count));
            send_code_bl(state, REP_3_6);
            send_bits(state, (count - 3) as u32, 2);
        } else if count <= 10 {
            send_code_bl(state, REPZ_3_10);
            send_bits(state, (count - 3) as u32, 3);
        } else {
            send_code_bl(state, REPZ_11_138);
            send_bits(state, (count - 11) as u32, 7);
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

/// Build the bit-length tree and determine max_blindex (the highest
/// index in BL_ORDER that has a non-zero bit-length code).
fn build_bl_tree(state: &mut DeflateState) -> usize {
    // Scan the literal and distance trees to compute bl_tree frequencies.
    let lmc = state.l_desc.max_code as usize;
    scan_tree(state, TreeType::Literal, lmc);
    let dmc = state.d_desc.max_code as usize;
    scan_tree(state, TreeType::Distance, dmc);

    // Build the bit-length Huffman tree.
    build_tree(state, TreeType::BitLength);

    // Determine the number of bit-length codes to send. The DEFLATE format
    // requires at least 4.
    let mut max_blindex = BL_CODES - 1;
    while max_blindex >= 3 {
        if state.bl_tree[BL_ORDER[max_blindex] as usize].dl != 0 {
            break;
        }
        max_blindex -= 1;
    }

    state.opt_len += 3 * (max_blindex as u64 + 1) + 5 + 5 + 4;
    max_blindex
}

/// Send the three Huffman trees (literal, distance, bit-length) that comprise
/// a dynamic block header.
fn send_all_trees(state: &mut DeflateState, lcodes: usize, dcodes: usize, blcodes: usize) {
    debug_assert!((257..=286).contains(&lcodes));
    debug_assert!((1..=30).contains(&dcodes));
    debug_assert!((4..=19).contains(&blcodes));

    send_bits(state, (lcodes - 257) as u32, 5);
    send_bits(state, (dcodes - 1) as u32, 5);
    send_bits(state, (blcodes - 4) as u32, 4);

    for &bl in BL_ORDER.iter().take(blcodes) {
        send_bits(state, state.bl_tree[bl as usize].dl as u32, 3);
    }

    let lmc = state.l_desc.max_code as usize;
    send_tree(state, TreeType::Literal, lmc);
    let dmc = state.d_desc.max_code as usize;
    send_tree(state, TreeType::Distance, dmc);
}

// ===========================================================================
// Data-type detection
// ===========================================================================

/// Detect whether the data being compressed is predominantly text or binary by
/// examining the frequency of non-text bytes in the literal tree. Returns one
/// of Z_TEXT, Z_BINARY, or Z_UNKNOWN.
fn detect_data_type(state: &DeflateState) -> i32 {
    // Check for non-text bytes (control characters outside of common
    // whitespace). Byte values 0-6, 14-31, 127 are considered "binary".
    let mut black_mask: u64 = 0;
    for n in 0..7_usize {
        black_mask |= state.dyn_ltree[n].fc as u64;
    }
    if black_mask != 0 {
        return Z_BINARY;
    }
    for n in 14..32_usize {
        black_mask |= state.dyn_ltree[n].fc as u64;
    }
    if black_mask != 0 {
        return Z_BINARY;
    }
    if state.dyn_ltree[127].fc != 0 {
        return Z_BINARY;
    }

    // Check for at least one "text" byte (ASCII printable or common
    // whitespace).
    for n in 32..128_usize {
        if state.dyn_ltree[n].fc != 0 {
            return Z_TEXT;
        }
    }
    if state.dyn_ltree[9].fc != 0 || state.dyn_ltree[10].fc != 0 || state.dyn_ltree[13].fc != 0 {
        return Z_TEXT;
    }

    Z_BINARY
}

/// Detect whether the data is text or binary. Returns the detected type as
/// one of Z_TEXT, Z_BINARY, or Z_UNKNOWN.
pub fn set_data_type(state: &mut DeflateState) -> i32 {
    detect_data_type(state)
}

// ===========================================================================
// Block output
// ===========================================================================

/// Compress the current block using the given literal and distance trees.
///
/// The trees are passed as slices so that either the static trees or copies of
/// the dynamic trees can be supplied (avoiding borrow-checker conflicts).
pub fn compress_block(state: &mut DeflateState, ltree: &[HuffmanNode], dtree: &[HuffmanNode]) {
    let mut sx: usize = 0;

    if state.sym_next != 0 {
        loop {
            let dist_lo = state.sym_buf[sx] as u32;
            sx += 1;
            let dist_hi = state.sym_buf[sx] as u32;
            sx += 1;
            let lc = state.sym_buf[sx] as u32;
            sx += 1;
            let dist = dist_lo | (dist_hi << 8);

            if dist == 0 {
                // Literal byte.
                send_code(state, lc as usize, ltree);
            } else {
                // Distance/length pair.
                let code = LENGTH_CODE[lc as usize] as usize;
                send_code(state, code + LITERALS + 1, ltree);
                let extra = EXTRA_LBITS[code] as usize;
                if extra != 0 {
                    let lc_adj = lc as i32 - BASE_LENGTH[code];
                    send_bits(state, lc_adj as u32, extra);
                }
                let d = dist - 1;
                let dcode = d_code(d) as usize;
                send_code(state, dcode, dtree);
                let dextra = EXTRA_DBITS[dcode] as usize;
                if dextra != 0 {
                    let d_adj = d as i32 - BASE_DIST[dcode];
                    send_bits(state, d_adj as u32, dextra);
                }
            }

            if sx >= state.sym_next {
                break;
            }
        }
    }

    send_code(state, END_BLOCK, ltree);
}

/// Output a stored (uncompressed) block.
pub fn tr_stored_block(state: &mut DeflateState, buf: &[u8], stored_len: u64, last: bool) {
    let last_val = if last { 1u32 } else { 0u32 };
    send_bits(state, ((STORED_BLOCK as u32) << 1) | last_val, 3);
    copy_block(state, &buf[..stored_len as usize], true);
}

/// Send a static-block alignment consisting of an empty static block with
/// STATIC_TREES type and the END_BLOCK code. Used for Z_PARTIAL_FLUSH.
pub fn tr_align(state: &mut DeflateState) {
    let last_val = 0u32; // not last block
    send_bits(state, ((STATIC_TREES as u32) << 1) | last_val, 3);
    send_code(state, END_BLOCK, &STATIC_LTREE);
    bi_flush(state);
}

/// Determine the best block type (stored, static, or dynamic Huffman),
/// output the block, and reset for the next one.
pub fn tr_flush_block(state: &mut DeflateState, buf: Option<&[u8]>, stored_len: u64, last: bool) {
    let opt_lenb: u64;
    let static_lenb: u64;
    let mut max_blindex: usize = 0;

    if state.level > 0 {
        // Build the Huffman trees unless Z_FIXED forces static trees.
        if state.strategy != Z_FIXED {
            build_tree(state, TreeType::Literal);
            build_tree(state, TreeType::Distance);
            max_blindex = build_bl_tree(state);
        }

        opt_lenb = (state.opt_len + 3 + 7) >> 3;
        static_lenb = (state.static_len + 3 + 7) >> 3;
    } else {
        opt_lenb = stored_len + 5;
        static_lenb = stored_len + 5;
    }

    let best_static = if static_lenb <= opt_lenb {
        static_lenb
    } else {
        opt_lenb
    };

    if let Some(b) = buf {
        if stored_len + 4 <= best_static {
            // Stored block is the best or only option.
            tr_stored_block(state, b, stored_len, last);
            init_block(state);
            if last {
                bi_windup(state);
            }
            return;
        }
    }

    if state.strategy == Z_FIXED || static_lenb <= opt_lenb {
        // Static trees.
        let last_val = if last { 1u32 } else { 0u32 };
        send_bits(state, ((STATIC_TREES as u32) << 1) | last_val, 3);
        let lt = STATIC_LTREE;
        let dt = STATIC_DTREE;
        compress_block(state, &lt, &dt);
    } else {
        // Dynamic trees.
        let last_val = if last { 1u32 } else { 0u32 };
        send_bits(state, ((DYN_TREES as u32) << 1) | last_val, 3);
        let lcodes = (state.l_desc.max_code + 1) as usize;
        let dcodes = (state.d_desc.max_code + 1) as usize;
        send_all_trees(state, lcodes, dcodes, max_blindex + 1);
        // Copy dynamic trees to locals to avoid borrowing state while mutating it.
        let dyn_lt = state.dyn_ltree;
        let dyn_dt = state.dyn_dtree;
        compress_block(state, &dyn_lt, &dyn_dt);
    }

    init_block(state);

    if last {
        bi_windup(state);
    }
}

// ===========================================================================
// Tally functions
// ===========================================================================

/// Record a literal byte. Returns true when the symbol buffer is full and
/// a block flush is needed.
pub fn tally_lit(state: &mut DeflateState, c: u8) -> bool {
    state.sym_buf[state.sym_next] = 0;
    state.sym_buf[state.sym_next + 1] = 0;
    state.sym_buf[state.sym_next + 2] = c;
    state.sym_next += 3;
    let old = state.dyn_ltree[c as usize].fc;
    state.dyn_ltree[c as usize].fc = old.wrapping_add(1);
    state.sym_next == state.sym_end
}

/// Record a distance/length pair. dist is the 1-based match distance,
/// len is the match length minus MIN_MATCH. Returns true when the
/// symbol buffer is full and a block flush is needed.
pub fn tally_dist(state: &mut DeflateState, dist: u32, len: u32) -> bool {
    state.sym_buf[state.sym_next] = (dist & 0xff) as u8;
    state.sym_buf[state.sym_next + 1] = ((dist >> 8) & 0xff) as u8;
    state.sym_buf[state.sym_next + 2] = len as u8;
    state.sym_next += 3;

    let d = dist - 1;
    let lc = LENGTH_CODE[len as usize] as usize + LITERALS + 1;
    let old_l = state.dyn_ltree[lc].fc;
    state.dyn_ltree[lc].fc = old_l.wrapping_add(1);

    let dc = d_code(d) as usize;
    let old_d = state.dyn_dtree[dc].fc;
    state.dyn_dtree[dc].fc = old_d.wrapping_add(1);

    state.matches += 1;
    state.sym_next == state.sym_end
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Map a distance value to a distance code (0-29).
///
/// dist is the 0-based distance (i.e. match_distance - 1).
#[inline(always)]
pub fn d_code(dist: u32) -> u8 {
    if dist < 256 {
        DIST_CODE[dist as usize]
    } else {
        DIST_CODE[256 + (dist >> 7) as usize]
    }
}

// ===========================================================================
// Unit tests (access pub(crate) items)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_ltree_size() {
        assert_eq!(STATIC_LTREE.len(), L_CODES + 2);
    }

    #[test]
    fn test_static_dtree_all_5_bits() {
        for (i, node) in STATIC_DTREE.iter().enumerate() {
            assert_eq!(node.dl, 5, "STATIC_DTREE[{i}] should have dl=5");
        }
    }

    #[test]
    fn test_dist_code_size() {
        assert_eq!(DIST_CODE.len(), 512);
    }

    #[test]
    fn test_length_code_size() {
        assert_eq!(LENGTH_CODE.len(), 256);
    }

    #[test]
    fn test_base_length_values() {
        assert_eq!(BASE_LENGTH[0], 0);
        assert_eq!(BASE_LENGTH[1], 1);
        assert_eq!(BASE_LENGTH[27], 224);
        assert_eq!(BASE_LENGTH[28], 0);
    }

    #[test]
    fn test_base_dist_values() {
        assert_eq!(BASE_DIST[0], 0);
        assert_eq!(BASE_DIST[1], 1);
        assert_eq!(BASE_DIST[29], 24576);
    }

    #[test]
    fn test_static_l_desc() {
        assert!(STATIC_L_DESC.static_tree.is_some());
        assert_eq!(STATIC_L_DESC.elems, L_CODES);
        assert_eq!(STATIC_L_DESC.max_length, MAX_BITS);
        assert_eq!(STATIC_L_DESC.extra_base, LITERALS + 1);
    }

    #[test]
    fn test_static_d_desc() {
        assert!(STATIC_D_DESC.static_tree.is_some());
        assert_eq!(STATIC_D_DESC.elems, D_CODES);
        assert_eq!(STATIC_D_DESC.max_length, MAX_BITS);
    }

    #[test]
    fn test_static_bl_desc() {
        assert!(STATIC_BL_DESC.static_tree.is_none());
        assert_eq!(STATIC_BL_DESC.elems, BL_CODES);
        assert_eq!(STATIC_BL_DESC.max_length, MAX_BL_BITS);
    }

    #[test]
    fn test_static_ltree_bit_lengths() {
        // RFC 1951 fixed Huffman table:
        // 0-143: 8 bits, 144-255: 9 bits, 256-279: 7 bits, 280-287: 8 bits
        for (i, entry) in STATIC_LTREE.iter().enumerate().take(144) {
            assert_eq!(entry.dl, 8, "STATIC_LTREE[{i}] should be 8 bits");
        }
        for (i, entry) in STATIC_LTREE.iter().enumerate().take(256).skip(144) {
            assert_eq!(entry.dl, 9, "STATIC_LTREE[{i}] should be 9 bits");
        }
        for (i, entry) in STATIC_LTREE.iter().enumerate().take(280).skip(256) {
            assert_eq!(entry.dl, 7, "STATIC_LTREE[{i}] should be 7 bits");
        }
        for (i, entry) in STATIC_LTREE.iter().enumerate().take(288).skip(280) {
            assert_eq!(entry.dl, 8, "STATIC_LTREE[{i}] should be 8 bits");
        }
    }

    #[test]
    fn test_bi_reverse() {
        assert_eq!(bi_reverse(0b1010, 4), 0b0101);
        assert_eq!(bi_reverse(0b1100, 4), 0b0011);
        assert_eq!(bi_reverse(0b1, 1), 0b1);
        assert_eq!(bi_reverse(0b10, 2), 0b01);
    }

    #[test]
    fn test_length_code_range() {
        for &c in LENGTH_CODE.iter() {
            assert!(c <= 28, "LENGTH_CODE value {c} out of range");
        }
    }

    #[test]
    fn test_dist_code_range() {
        for &c in DIST_CODE.iter() {
            assert!(c <= 29, "DIST_CODE value {c} out of range");
        }
    }
}
