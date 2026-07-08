//! Huffman tree construction, static tables, and DEFLATE block output.
//!
//! This module is a faithful, `100%` safe-Rust port of the C baseline `trees.c`
//! together with the pre-generated static tables from `trees.h`. It produces the
//! DEFLATE (RFC 1951) block structure: stored / static / dynamic block-type
//! selection, canonical Huffman code generation, and the compressed token stream.
//!
//! # Byte-exact fidelity
//!
//! Every constant, table, and algorithm reproduces the reference-zlib behavior
//! bit-for-bit, so the emitted DEFLATE token stream is byte-identical to reference
//! zlib for the same input, compression level, and strategy. The block-type
//! selection heuristic, the Huffman code assignment, and the bit-level emission
//! order all mirror the C implementation precisely.
//!
//! # Safety
//!
//! This file contains zero `unsafe` code (enforced by `#![deny(unsafe_code)]`).
//! Every table is a `const`/`static` array and all tree access uses safe slice /
//! array indexing. The low-level bit primitives (`send_bits`, `send_code`,
//! `put_byte`, `put_short`, `bi_flush`, `bi_windup`) live on [`DeflateState`] in
//! `state.rs` and are invoked here as inherent methods.
//!
//! # Arithmetic
//!
//! The C code accumulates `s->opt_len` / `s->static_len` in `ulg` (unsigned long)
//! and relies on defined wrap-around during the temporary underflow inside
//! `build_tree` (the "force at least two codes" step). This port uses
//! `wrapping_add` / `wrapping_sub` on `usize` to reproduce that behavior exactly.

#![deny(unsafe_code)]

use crate::constants::{DataType, Strategy};
use crate::deflate::state::{
    BL_CODES, CtData, D_CODES, DeflateState, END_BLOCK, HEAP_SIZE, L_CODES, LENGTH_CODES, LITERALS,
    MAX_BITS, MAX_MATCH, MIN_MATCH, TreeKind,
};

// ---------------------------------------------------------------------------
// Section 1: Constants (trees.c) — reproduced exactly.
// ---------------------------------------------------------------------------

/// Bit length of the "bit length" codes must not exceed this value.
///
/// Note: `state.rs` also exports a `MAX_BL_BITS`; this module keeps its own
/// private copy to avoid glob-import ambiguity. Both hold the value `7`.
const MAX_BL_BITS: usize = 7;

/// Block type: a "stored" (uncompressed) block.
pub(crate) const STORED_BLOCK: i32 = 0;
/// Block type: a block compressed with the fixed (static) Huffman trees.
pub(crate) const STATIC_TREES: i32 = 1;
/// Block type: a block compressed with dynamic Huffman trees.
pub(crate) const DYN_TREES: i32 = 2;

/// Repeat the previous bit length 3..=6 times (2 extra bits encode the count).
pub(crate) const REP_3_6: usize = 16;
/// Repeat a zero bit length 3..=10 times (3 extra bits encode the count).
pub(crate) const REPZ_3_10: usize = 17;
/// Repeat a zero bit length 11..=138 times (7 extra bits encode the count).
pub(crate) const REPZ_11_138: usize = 18;

/// Index of the least-frequent node kept at the root of the heap.
const SMALLEST: usize = 1;

/// Length of the `_dist_code` mapping table.
const DIST_CODE_LEN: usize = 512;

// ---------------------------------------------------------------------------
// Section 2: Runtime helper tables (trees.c) — reproduced exactly.
// ---------------------------------------------------------------------------

/// Extra bits for each length code.
const EXTRA_LBITS: [i32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each distance code.
const EXTRA_DBITS: [i32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits for each bit-length code.
const EXTRA_BLBITS: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// The order in which the bit-length code lengths are transmitted (see RFC 1951).
const BL_ORDER: [u8; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ---------------------------------------------------------------------------
// Section 3: Static Huffman tables (trees.h) — transcribed exactly.
//
// These arrays are injected verbatim from the C `trees.h` header so they match
// reference zlib bit-for-bit. The `tests` module recomputes them from the
// canonical `tr_static_init` algorithm and asserts equality as a guard against
// transcription errors.
// ---------------------------------------------------------------------------

/// The fixed (static) literal/length Huffman tree, transcribed from `trees.h`.
static STATIC_LTREE: [CtData; L_CODES + 2] = [
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
/// The fixed (static) distance Huffman tree, transcribed from `trees.h`.
static STATIC_DTREE: [CtData; D_CODES] = [
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
/// Maps a (possibly reduced) distance to its distance code (`_dist_code`).
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
/// Maps `match_length - MIN_MATCH` to its length code (`_length_code`).
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
/// First length (minus MIN_MATCH) for each length code (`base_length`).
static BASE_LENGTH: [i32; LENGTH_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224, 0,
];
/// First distance for each distance code (`base_dist`).
static BASE_DIST: [i32; D_CODES] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576,
];

// ---------------------------------------------------------------------------
// Section 4: `d_code` helper (deflate.h macro).
// ---------------------------------------------------------------------------

/// Maps a match distance to its distance code.
///
/// Mirrors the C `d_code` macro:
/// `((dist) < 256 ? _dist_code[dist] : _dist_code[256 + ((dist) >> 7)])`.
#[inline]
pub(crate) fn d_code(dist: usize) -> usize {
    if dist < 256 {
        DIST_CODE[dist] as usize
    } else {
        DIST_CODE[256 + (dist >> 7)] as usize
    }
}

// ---------------------------------------------------------------------------
// Section 6: `bi_reverse` — reverse the low `len` bits of `code`.
// ---------------------------------------------------------------------------

/// Reverses the low `len` bits of `code`.
///
/// Mirrors the C do/while loop exactly:
/// `do { res |= code & 1; code >>= 1; res <<= 1; } while (--len > 0); return res >> 1;`
fn bi_reverse(mut code: u32, mut len: i32) -> u32 {
    let mut res: u32 = 0;
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

// ---------------------------------------------------------------------------
// Section 5: Static tree descriptors (trees.c static_l_desc/d_desc/bl_desc).
//
// The C `static_tree_desc` structs bundle the static tree pointer, the extra-bit
// table, the base index for extra bits, the element count, and the maximum code
// length. Here we key the descriptor on `TreeKind` and return the data by value;
// the per-tree static Huffman table is referenced only when `has_static_tree`.
// ---------------------------------------------------------------------------

/// Static descriptor data for one of the three DEFLATE trees.
struct StaticDescriptor {
    /// Whether a fixed (static) Huffman table backs this tree (literal/distance).
    has_static_tree: bool,
    /// Extra-bits table for this tree.
    extra_bits: &'static [i32],
    /// Base index into the alphabet at which `extra_bits` starts applying.
    extra_base: usize,
    /// Number of elements (alphabet size) for this tree.
    elems: usize,
    /// Maximum code length allowed for this tree.
    max_length: usize,
}

/// Returns the static descriptor for the requested tree kind.
///
/// Mirrors the C `static_l_desc` / `static_d_desc` / `static_bl_desc` globals.
fn stat_desc(kind: TreeKind) -> StaticDescriptor {
    match kind {
        TreeKind::Literal => StaticDescriptor {
            has_static_tree: true,
            extra_bits: &EXTRA_LBITS,
            extra_base: LITERALS + 1,
            elems: L_CODES,
            max_length: MAX_BITS,
        },
        TreeKind::Distance => StaticDescriptor {
            has_static_tree: true,
            extra_bits: &EXTRA_DBITS,
            extra_base: 0,
            elems: D_CODES,
            max_length: MAX_BITS,
        },
        TreeKind::BitLength => StaticDescriptor {
            has_static_tree: false,
            extra_bits: &EXTRA_BLBITS,
            extra_base: 0,
            elems: BL_CODES,
            max_length: MAX_BL_BITS,
        },
    }
}

// ---------------------------------------------------------------------------
// Tree-field accessors.
//
// The three dynamic trees (`dyn_ltree`, `dyn_dtree`, `bl_tree`) and the per-tree
// `max_code` live as fields on `DeflateState`. These small helpers select the
// correct field by `TreeKind` and read/write a single `CtData` member, returning
// `Copy` scalars so callers never hold overlapping borrows of `s`.
// ---------------------------------------------------------------------------

/// Reads `tree[i].Freq` for the selected dynamic tree.
#[inline]
fn tree_freq(s: &DeflateState, kind: TreeKind, i: usize) -> u16 {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].freq(),
        TreeKind::Distance => s.dyn_dtree[i].freq(),
        TreeKind::BitLength => s.bl_tree[i].freq(),
    }
}

/// Writes `tree[i].Freq` for the selected dynamic tree.
#[inline]
fn set_tree_freq(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].set_freq(v),
        TreeKind::Distance => s.dyn_dtree[i].set_freq(v),
        TreeKind::BitLength => s.bl_tree[i].set_freq(v),
    }
}

/// Reads `tree[i].Len` for the selected dynamic tree.
#[inline]
fn tree_len(s: &DeflateState, kind: TreeKind, i: usize) -> u16 {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].len(),
        TreeKind::Distance => s.dyn_dtree[i].len(),
        TreeKind::BitLength => s.bl_tree[i].len(),
    }
}

/// Writes `tree[i].Len` for the selected dynamic tree.
#[inline]
fn set_tree_len(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].set_len(v),
        TreeKind::Distance => s.dyn_dtree[i].set_len(v),
        TreeKind::BitLength => s.bl_tree[i].set_len(v),
    }
}

/// Reads `tree[i].Dad` for the selected dynamic tree.
#[inline]
fn tree_dad(s: &DeflateState, kind: TreeKind, i: usize) -> u16 {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].dad(),
        TreeKind::Distance => s.dyn_dtree[i].dad(),
        TreeKind::BitLength => s.bl_tree[i].dad(),
    }
}

/// Writes `tree[i].Dad` for the selected dynamic tree.
#[inline]
fn set_tree_dad(s: &mut DeflateState, kind: TreeKind, i: usize, v: u16) {
    match kind {
        TreeKind::Literal => s.dyn_ltree[i].set_dad(v),
        TreeKind::Distance => s.dyn_dtree[i].set_dad(v),
        TreeKind::BitLength => s.bl_tree[i].set_dad(v),
    }
}

/// Reads `static_tree[i].Len` for a tree that has a static counterpart.
///
/// Only literal and distance trees have static tables; `BitLength` returns `0`
/// and is never queried (guarded by `StaticDescriptor::has_static_tree`).
#[inline]
fn static_tree_len(kind: TreeKind, i: usize) -> u16 {
    match kind {
        TreeKind::Literal => STATIC_LTREE[i].len(),
        TreeKind::Distance => STATIC_DTREE[i].len(),
        TreeKind::BitLength => 0,
    }
}

/// Reads the per-tree `max_code` field for the selected tree.
#[inline]
fn max_code(s: &DeflateState, kind: TreeKind) -> usize {
    match kind {
        TreeKind::Literal => s.l_max_code,
        TreeKind::Distance => s.d_max_code,
        TreeKind::BitLength => s.bl_max_code,
    }
}

/// Writes the per-tree `max_code` field for the selected tree.
#[inline]
fn set_max_code(s: &mut DeflateState, kind: TreeKind, v: usize) {
    match kind {
        TreeKind::Literal => s.l_max_code = v,
        TreeKind::Distance => s.d_max_code = v,
        TreeKind::BitLength => s.bl_max_code = v,
    }
}

// ---------------------------------------------------------------------------
// Section 9: Heap operations (trees.c smaller / pqdownheap / pqremove).
// ---------------------------------------------------------------------------

/// Compares two heap nodes by frequency, breaking ties by (smaller) depth.
///
/// Mirrors the C `smaller` macro:
/// `tree[n].Freq < tree[m].Freq || (tree[n].Freq == tree[m].Freq && depth[n] <= depth[m])`.
#[inline]
fn smaller(s: &DeflateState, kind: TreeKind, n: usize, m: usize) -> bool {
    let fn_ = tree_freq(s, kind, n);
    let fm = tree_freq(s, kind, m);
    fn_ < fm || (fn_ == fm && s.depth[n] <= s.depth[m])
}

/// Restores the heap property by sifting element `k_start` down.
///
/// Mirrors the C `pqdownheap`. `heap[SMALLEST]` is the root (least-frequent node).
/// Node numbers are read into `Copy` locals before each `smaller` call so that no
/// borrow of `s` overlaps the mutation of `s.heap`.
fn pqdownheap(s: &mut DeflateState, kind: TreeKind, k_start: usize) {
    let v = s.heap[k_start];
    let mut k = k_start;
    let mut j = k_start << 1;
    while j <= s.heap_len {
        // Set j to the least of the two sons.
        if j < s.heap_len {
            let right = s.heap[j + 1] as usize;
            let left = s.heap[j] as usize;
            if smaller(s, kind, right, left) {
                j += 1;
            }
        }
        // Exit if v is smaller than both sons.
        let child = s.heap[j] as usize;
        if smaller(s, kind, v as usize, child) {
            break;
        }
        // Exchange v with the smallest son.
        s.heap[k] = s.heap[j];
        k = j;
        // Continue down the tree, setting j to the left son of k.
        j <<= 1;
    }
    s.heap[k] = v;
}

/// Removes the least-frequent node from the heap and re-heapifies.
///
/// Mirrors the C `pqremove` macro, returning the removed (top) node.
fn pqremove(s: &mut DeflateState, kind: TreeKind) -> i32 {
    let top = s.heap[SMALLEST];
    s.heap[SMALLEST] = s.heap[s.heap_len];
    s.heap_len -= 1;
    pqdownheap(s, kind, SMALLEST);
    top
}

// ---------------------------------------------------------------------------
// Section 11: `gen_codes` (trees.c) — assign canonical codes from bit lengths.
// ---------------------------------------------------------------------------

/// Generates the canonical Huffman codes for `tree[0..=max_code]` given the code
/// lengths already stored in each `tree[n].Len` and the length histogram
/// `bl_count`.
///
/// The distribution of codes is the standard canonical assignment described in
/// RFC 1951 (Section 3.2.2): the first code for each bit length is derived from
/// the count of codes of the previous length, and codes are bit-reversed so the
/// most-significant bit is transmitted first.
fn gen_codes(tree: &mut [CtData], max_code: usize, bl_count: &[u16]) {
    // next_code[bits] = the next code value to assign for length `bits`.
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    // The distribution counts are first used to generate the code values without
    // bit reversal.
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }
    // Iterate `tree[0..=max_code]` in order; each entry is indexed only by its
    // position, so an iterator is used directly (clippy `needless_range_loop`).
    for entry in tree.iter_mut().take(max_code + 1) {
        let len = entry.len() as usize;
        if len == 0 {
            continue;
        }
        // Reverse the bits so the code is transmitted MSB-first.
        let rev = bi_reverse(next_code[len] as u32, len as i32) as u16;
        entry.set_code(rev);
        next_code[len] = next_code[len].wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// Section 10: `gen_bitlen` (trees.c) — optimal bit lengths + overflow fixup.
// ---------------------------------------------------------------------------

/// Computes the optimal bit lengths for the tree rooted at `s.heap[s.heap_max]`.
///
/// Accumulates `s.opt_len` and (when a static counterpart exists) `s.static_len`,
/// builds the `s.bl_count` length histogram, and performs the overflow
/// redistribution loop that guarantees no code exceeds `max_length` bits. Ported
/// line-for-line from the C `gen_bitlen`.
fn gen_bitlen(s: &mut DeflateState, kind: TreeKind) {
    let desc = stat_desc(kind);
    let mc = max_code(s, kind);
    let extra = desc.extra_bits;
    let base = desc.extra_base;
    let max_length = desc.max_length;
    let mut overflow: i32 = 0;

    for bits in 0..=MAX_BITS {
        s.bl_count[bits] = 0;
    }

    // In a first pass, compute the optimal bit lengths (which may overflow the
    // maximum code length for the tree). The root of the heap has no bits.
    let root = s.heap[s.heap_max] as usize;
    set_tree_len(s, kind, root, 0);

    for h in (s.heap_max + 1)..HEAP_SIZE {
        let n = s.heap[h] as usize;
        let dad = tree_dad(s, kind, n) as usize;
        let mut bits = tree_len(s, kind, dad) as i32 + 1;
        if bits > max_length as i32 {
            bits = max_length as i32;
            overflow += 1;
        }
        // The father node has already been processed, so tree[n].Len is final.
        set_tree_len(s, kind, n, bits as u16);

        // Overwrite tree[n].Dad; unused by the caller from here on.
        if n > mc {
            continue; // not a leaf node
        }

        s.bl_count[bits as usize] += 1;
        let mut xbits = 0i32;
        if n >= base {
            xbits = extra[n - base];
        }
        let f = tree_freq(s, kind, n) as usize;
        s.opt_len = s
            .opt_len
            .wrapping_add(f.wrapping_mul((bits + xbits) as usize));
        if desc.has_static_tree {
            let slen = static_tree_len(kind, n) as i32;
            s.static_len = s
                .static_len
                .wrapping_add(f.wrapping_mul((slen + xbits) as usize));
        }
    }
    if overflow == 0 {
        return;
    }

    // Find the first bit length which could increase.
    loop {
        let mut bits = max_length - 1;
        while s.bl_count[bits] == 0 {
            bits -= 1;
        }
        s.bl_count[bits] -= 1; // move one leaf down the tree
        s.bl_count[bits + 1] += 2; // move one overflow item as its brother
        s.bl_count[max_length] -= 1;
        // The brother of the overflow item also moves one step up, but this does
        // not affect bl_count[max_length].
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // Now recompute all bit lengths, scanning in increasing frequency. `h` is
    // still equal to HEAP_SIZE (the recompute walks the heap in reverse). The
    // leaves for the longest codes are at the start of the internal heap.
    let mut h = HEAP_SIZE;
    for bits in (1..=max_length).rev() {
        let mut n = s.bl_count[bits] as i32;
        while n != 0 {
            h -= 1;
            let m = s.heap[h] as usize;
            if m > mc {
                continue;
            }
            let tlen = tree_len(s, kind, m) as usize;
            if tlen != bits {
                let f = tree_freq(s, kind, m) as usize;
                s.opt_len = s
                    .opt_len
                    .wrapping_add(bits.wrapping_sub(tlen).wrapping_mul(f));
                set_tree_len(s, kind, m, bits as u16);
            }
            n -= 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Section 12: `build_tree` (trees.c) — the core Huffman tree builder.
// ---------------------------------------------------------------------------

/// Constructs one Huffman tree in place and assigns its codes.
///
/// Follows the C `build_tree` exactly:
/// 1. Build the initial heap from all nodes with nonzero frequency, recording the
///    largest code index in the per-tree `max_code`. Nodes with zero frequency get
///    length `0`.
/// 2. Force at least two nonzero codes to exist (pseudo-nodes) so a code exists
///    even for empty or degenerate input — this is critical for byte-exactness.
/// 3. Repeatedly remove the two least-frequent nodes and create a parent whose
///    frequency is their sum, until a single node remains.
/// 4. Compute the bit lengths (`gen_bitlen`) and the canonical codes (`gen_codes`).
fn build_tree(s: &mut DeflateState, kind: TreeKind) {
    let desc = stat_desc(kind);
    let elems = desc.elems;
    let mut mc: i32 = -1; // largest code with nonzero frequency

    // Construct the initial heap, with the least-frequent element in
    // heap[SMALLEST]. The sons of heap[n] are heap[2*n] and heap[2*n+1]. heap[0]
    // is not used.
    s.heap_len = 0;
    s.heap_max = HEAP_SIZE;

    for n in 0..elems {
        if tree_freq(s, kind, n) != 0 {
            s.heap_len += 1;
            s.heap[s.heap_len] = n as i32;
            mc = n as i32;
            s.depth[n] = 0;
        } else {
            set_tree_len(s, kind, n, 0);
        }
    }

    // The pkzip format requires that at least one distance code exists, and that
    // at least one bit should be sent even if there is only one possible code. So
    // to avoid special checks later on we force at least two codes of nonzero
    // frequency.
    while s.heap_len < 2 {
        let node: i32 = if mc < 2 {
            mc += 1;
            mc
        } else {
            0
        };
        s.heap_len += 1;
        s.heap[s.heap_len] = node;
        set_tree_freq(s, kind, node as usize, 1);
        s.depth[node as usize] = 0;
        s.opt_len = s.opt_len.wrapping_sub(1);
        if desc.has_static_tree {
            let sl = static_tree_len(kind, node as usize) as usize;
            s.static_len = s.static_len.wrapping_sub(sl);
        }
        // node is 0 or 1 so it does not have extra bits.
    }
    set_max_code(s, kind, mc as usize);

    // The elements heap[heap_len/2 + 1 .. heap_len] are leaves of the tree,
    // establish sub-heaps of increasing lengths.
    let mut n = s.heap_len / 2;
    while n >= 1 {
        pqdownheap(s, kind, n);
        n -= 1;
    }

    // Construct the Huffman tree by repeatedly combining the two least-frequent
    // nodes. `node` is the next internal node of the tree.
    let mut node = elems;
    loop {
        let n1 = pqremove(s, kind); // n = node of least frequency
        let m = s.heap[SMALLEST]; // m = node of next least frequency

        s.heap_max -= 1;
        s.heap[s.heap_max] = n1; // keep the nodes sorted by frequency
        s.heap_max -= 1;
        s.heap[s.heap_max] = m;

        // Create a new node father of n and m.
        let fn_ = tree_freq(s, kind, n1 as usize);
        let fm = tree_freq(s, kind, m as usize);
        set_tree_freq(s, kind, node, fn_.wrapping_add(fm));
        let dn = s.depth[n1 as usize];
        let dm = s.depth[m as usize];
        let d = if dn >= dm { dn } else { dm };
        s.depth[node] = d.wrapping_add(1);
        set_tree_dad(s, kind, n1 as usize, node as u16);
        set_tree_dad(s, kind, m as usize, node as u16);

        // and insert the new node in the heap.
        s.heap[SMALLEST] = node as i32;
        node += 1;
        pqdownheap(s, kind, SMALLEST);

        if s.heap_len < 2 {
            break;
        }
    }

    s.heap_max -= 1;
    s.heap[s.heap_max] = s.heap[SMALLEST];

    // At this point, the fields Freq and Dad are set. We can now generate the bit
    // lengths.
    gen_bitlen(s, kind);

    // The field Len is now set, we can generate the bit codes.
    let mc_final = max_code(s, kind);
    match kind {
        TreeKind::Literal => gen_codes(&mut s.dyn_ltree, mc_final, &s.bl_count),
        TreeKind::Distance => gen_codes(&mut s.dyn_dtree, mc_final, &s.bl_count),
        TreeKind::BitLength => gen_codes(&mut s.bl_tree, mc_final, &s.bl_count),
    }
}

// ---------------------------------------------------------------------------
// Section 8: `init_block` (trees.c) — reset all frequency counts.
// ---------------------------------------------------------------------------

/// Initializes a new block: clears the frequency counts of all three trees and
/// resets the optimal / static lengths and the symbol buffer cursor.
fn init_block(s: &mut DeflateState) {
    for n in 0..L_CODES {
        s.dyn_ltree[n].set_freq(0);
    }
    for n in 0..D_CODES {
        s.dyn_dtree[n].set_freq(0);
    }
    for n in 0..BL_CODES {
        s.bl_tree[n].set_freq(0);
    }
    s.dyn_ltree[END_BLOCK].set_freq(1);
    s.opt_len = 0;
    s.static_len = 0;
    s.sym_next = 0;
    s.matches = 0;
}

// ---------------------------------------------------------------------------
// Section 7: `_tr_init` (trees.c) — initialize the tree state for a stream.
// ---------------------------------------------------------------------------

/// Initializes the tree data structures for a new zlib stream.
///
/// In C, `tr_static_init` lazily builds the static tables at runtime; here the
/// static tables are compile-time `static` arrays, so `tr_static_init` is a no-op
/// and only the bit buffer plus the first block are initialized.
pub(crate) fn _tr_init(s: &mut DeflateState) {
    // tr_static_init(): no-op — the static tables are `const` in Rust.
    s.bi_buf = 0;
    s.bi_valid = 0;
    s.bi_used = 0;
    init_block(s);
}

// ---------------------------------------------------------------------------
// Section 13: `scan_tree` (trees.c) — count bit-length code frequencies.
// ---------------------------------------------------------------------------

/// Scans a tree, run-length encoding the code lengths, and accumulates the
/// resulting bit-length code frequencies into `bl_tree`.
///
/// Uses the same prevlen / curlen / nextlen / count / max_count / min_count state
/// machine as `send_tree`, but counts rather than emits. `tree[max_code + 1]` is
/// set to a sentinel length of `0xffff` to guarantee the final run is flushed.
fn scan_tree(s: &mut DeflateState, kind: TreeKind, max_code: usize) {
    let mut prevlen: i32 = -1; // last emitted length
    let mut nextlen = tree_len(s, kind, 0) as i32; // length of next code
    let mut count = 0i32; // repeat count of the current code
    let mut max_count = 7i32; // max repeat count
    let mut min_count = 4i32; // min repeat count

    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    // Guarantee that the last length is flushed.
    set_tree_len(s, kind, max_code + 1, 0xffff);

    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree_len(s, kind, n + 1) as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            let f = s.bl_tree[curlen as usize].freq();
            s.bl_tree[curlen as usize].set_freq(f.wrapping_add(count as u16));
        } else if curlen != 0 {
            if curlen != prevlen {
                let f = s.bl_tree[curlen as usize].freq();
                s.bl_tree[curlen as usize].set_freq(f.wrapping_add(1));
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

// ---------------------------------------------------------------------------
// Section 14: `send_tree` (trees.c) — emit the run-length-encoded bit lengths.
// ---------------------------------------------------------------------------

/// Sends a single bit-length code from `bl_tree` via the state bit buffer.
///
/// Copies the `CtData` out by value (it is `Copy`) so that no borrow of
/// `s.bl_tree` overlaps the `&mut s` call to `send_bits`.
#[inline]
fn send_bl_code(s: &mut DeflateState, c: usize) {
    let e = s.bl_tree[c];
    s.send_bits(e.code() as i32, e.len() as i32);
}

/// Emits the RLE-encoded bit lengths of a tree using the `bl_tree` codes.
///
/// Mirrors `scan_tree`'s state machine, but emits codes instead of counting them:
/// short runs repeat the literal code, and long runs use `REP_3_6` / `REPZ_3_10`
/// / `REPZ_11_138` with the appropriate number of extra bits.
fn send_tree(s: &mut DeflateState, kind: TreeKind, max_code: usize) {
    let mut prevlen: i32 = -1;
    let mut nextlen = tree_len(s, kind, 0) as i32;
    let mut count = 0i32;
    let mut max_count = 7i32;
    let mut min_count = 4i32;

    // tree[max_code + 1].Len was already set to the sentinel by scan_tree.
    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree_len(s, kind, n + 1) as i32;
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
            s.send_bits(count - 3, 2);
        } else if count <= 10 {
            send_bl_code(s, REPZ_3_10);
            s.send_bits(count - 3, 3);
        } else {
            send_bl_code(s, REPZ_11_138);
            s.send_bits(count - 11, 7);
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

// ---------------------------------------------------------------------------
// Section 15: `build_bl_tree` (trees.c) — build the bit-length tree.
// ---------------------------------------------------------------------------

/// Constructs the tree of bit lengths and returns the index of the last
/// bit-length code that must be transmitted (`max_blindex`).
///
/// Scans the literal and distance trees to populate the `bl_tree` frequencies,
/// builds `bl_tree`, then trims trailing zero-length bit-length codes down to a
/// minimum of four (per RFC 1951). Also updates `s.opt_len` with the cost of the
/// bit-length tree header.
fn build_bl_tree(s: &mut DeflateState) -> usize {
    // Determine the bit length frequencies for literal and distance trees.
    let lmc = max_code(s, TreeKind::Literal);
    scan_tree(s, TreeKind::Literal, lmc);
    let dmc = max_code(s, TreeKind::Distance);
    scan_tree(s, TreeKind::Distance, dmc);

    // Build the bit length tree.
    build_tree(s, TreeKind::BitLength);
    // opt_len now includes the length of the tree representations, except the
    // lengths of the bit lengths codes and the 5 + 5 + 4 bits for the counts.

    // Determine the number of bit length codes to send. The pkzip format requires
    // that at least 4 bit length codes be sent (see RFC 1951).
    let mut max_blindex = BL_CODES - 1;
    while max_blindex >= 3 {
        let bl_idx = BL_ORDER[max_blindex] as usize;
        if s.bl_tree[bl_idx].len() != 0 {
            break;
        }
        max_blindex -= 1;
    }
    // Update opt_len to include the bit length tree and counts.
    s.opt_len = s.opt_len.wrapping_add(3 * (max_blindex + 1) + 5 + 5 + 4);
    max_blindex
}

// ---------------------------------------------------------------------------
// Section 16: `send_all_trees` (trees.c) — send the header and both trees.
// ---------------------------------------------------------------------------

/// Sends the header for a block using dynamic Huffman trees: the counts, the
/// bit-length code lengths (in `BL_ORDER`), and then the literal and distance
/// trees themselves.
fn send_all_trees(s: &mut DeflateState, lcodes: usize, dcodes: usize, blcodes: usize) {
    s.send_bits((lcodes - 257) as i32, 5); // not +255 as stated in appnote.txt
    s.send_bits((dcodes - 1) as i32, 5);
    s.send_bits((blcodes - 4) as i32, 4); // not -3 as stated in appnote.txt
    // Send the bit-length code lengths in the canonical `BL_ORDER` sequence.
    for &bl in BL_ORDER.iter().take(blcodes) {
        let bl_idx = bl as usize;
        let len = s.bl_tree[bl_idx].len() as i32;
        s.send_bits(len, 3);
    }
    send_tree(s, TreeKind::Literal, lcodes - 1); // literal tree
    send_tree(s, TreeKind::Distance, dcodes - 1); // distance tree
}

// ---------------------------------------------------------------------------
// Section 17: `_tr_stored_block` (trees.c) — emit an uncompressed block.
// ---------------------------------------------------------------------------

/// Sends a stored (uncompressed) block.
///
/// `buf` is an optional offset into `s.window` naming the first byte of the
/// block; `None` denotes an empty block (used by `Z_SYNC_FLUSH` /
/// `Z_FULL_FLUSH`). This mirrors the C `_tr_stored_block`, whose `charf *buf` may
/// be `NULL`. The stored length and its one's-complement are written LSB-first
/// after the bit buffer is aligned to a byte boundary.
pub(crate) fn _tr_stored_block(
    s: &mut DeflateState,
    buf: Option<usize>,
    stored_len: usize,
    last: bool,
) {
    // send block type
    s.send_bits((STORED_BLOCK << 1) + i32::from(last), 3);
    s.bi_windup(); // align on byte boundary
    s.put_short(stored_len as u16);
    s.put_short(!(stored_len as u16));
    if stored_len != 0 {
        if let Some(off) = buf {
            // Copy window[off .. off + stored_len] into the pending buffer. The
            // two slices are disjoint fields of `s`, so this borrows safely.
            let p = s.pending;
            let (dst, src) = (
                &mut s.pending_buf[p..p + stored_len],
                &s.window[off..off + stored_len],
            );
            dst.copy_from_slice(src);
        }
    }
    s.pending += stored_len;
}

// ---------------------------------------------------------------------------
// Section 18: `_tr_flush_bits` / `_tr_align` (trees.c).
// ---------------------------------------------------------------------------

/// Flushes the bit buffer to the pending output (thin wrapper over `bi_flush`).
///
/// Provided as a free function for parity with the C `_tr_flush_bits(s)` call
/// form; `DeflateState` also exposes an inherent method of the same name, so this
/// free function may be unused depending on which form callers prefer.
#[allow(dead_code)]
pub(crate) fn _tr_flush_bits(s: &mut DeflateState) {
    s.bi_flush();
}

/// Aligns the output to a byte boundary using one empty static block.
///
/// Used by `Z_PARTIAL_FLUSH`: sends an empty static block consisting of just the
/// end-of-block code, then flushes the bit buffer.
pub(crate) fn _tr_align(s: &mut DeflateState) {
    s.send_bits(STATIC_TREES << 1, 3);
    s.send_code(END_BLOCK, &STATIC_LTREE);
    s.bi_flush();
}

// ---------------------------------------------------------------------------
// Section 19: `compress_block` (trees.c) — emit the token stream for a block.
// ---------------------------------------------------------------------------

/// Selects which pair of Huffman trees `compress_block` emits with.
#[derive(Clone, Copy)]
enum TreeSet {
    /// The fixed (static) literal/length and distance trees.
    Static,
    /// The dynamic literal/length and distance trees held on `DeflateState`.
    Dynamic,
}

/// Sends one code (`idx`) from the selected literal (`is_dist == false`) or
/// distance (`is_dist == true`) tree.
///
/// The `CtData` is copied out by value before the `&mut s` call to `send_bits`,
/// avoiding any overlapping borrow of `s.dyn_ltree` / `s.dyn_dtree`.
#[inline]
fn send_tree_code(s: &mut DeflateState, trees: TreeSet, is_dist: bool, idx: usize) {
    let e = match (trees, is_dist) {
        (TreeSet::Static, false) => STATIC_LTREE[idx],
        (TreeSet::Static, true) => STATIC_DTREE[idx],
        (TreeSet::Dynamic, false) => s.dyn_ltree[idx],
        (TreeSet::Dynamic, true) => s.dyn_dtree[idx],
    };
    s.send_bits(e.code() as i32, e.len() as i32);
}

/// Emits the literal / length / distance token stream held in `s.sym_buf`.
///
/// Each symbol occupies three bytes in `sym_buf`: the low and high bytes of the
/// match distance (both zero for a literal) followed by the literal byte or the
/// length code. This is the non-`LIT_MEM` buffer layout. The block is terminated
/// with the end-of-block code. Ported exactly from the C `compress_block`.
fn compress_block(s: &mut DeflateState, trees: TreeSet) {
    let mut sx = 0usize; // running index in sym_buf
    if s.sym_next != 0 {
        loop {
            let mut dist = (s.sym_buf[sx] as usize) & 0xff;
            sx += 1;
            dist += (s.sym_buf[sx] as usize & 0xff) << 8;
            sx += 1;
            let lc = s.sym_buf[sx] as usize;
            sx += 1;
            if dist == 0 {
                send_tree_code(s, trees, false, lc); // send a literal byte
            } else {
                // Here, lc is the match length - MIN_MATCH.
                let code = LENGTH_CODE[lc] as usize;
                send_tree_code(s, trees, false, code + LITERALS + 1); // length code
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    let lc2 = lc - BASE_LENGTH[code] as usize;
                    s.send_bits(lc2 as i32, extra); // send the extra length bits
                }
                let dist = dist - 1; // dist is now the match distance - 1
                let dcode = d_code(dist);
                send_tree_code(s, trees, true, dcode); // send the distance code
                let extrad = EXTRA_DBITS[dcode];
                if extrad != 0 {
                    let d = dist - BASE_DIST[dcode] as usize;
                    s.send_bits(d as i32, extrad); // send the extra distance bits
                }
            }
            // Sanity check: the C code asserts sx <= sym_next here.
            if sx >= s.sym_next {
                break;
            }
        }
    }
    send_tree_code(s, trees, false, END_BLOCK);
}

// ---------------------------------------------------------------------------
// Section 20: `detect_data_type` (trees.c) — heuristic text/binary detection.
// ---------------------------------------------------------------------------

/// Classifies the block data as text or binary using the same heuristic as C.
///
/// The block is deemed binary if it contains any control character other than
/// TAB (9), LF (10), or CR (13), or any of the "black-listed" bytes 0..=31 whose
/// bit is set in the mask `0xf3ff_c07f`. Otherwise it is text.
fn detect_data_type(s: &DeflateState) -> DataType {
    // block_mask has a 1 for each byte value < 32 that is considered "binary".
    // Bytes 9 (TAB), 10 (LF), 13 (CR) are excluded from the mask.
    let mut block_mask: u32 = 0xf3ff_c07f;
    for n in 0..32 {
        if (block_mask & 1) != 0 && s.dyn_ltree[n].freq() != 0 {
            return DataType::Binary;
        }
        block_mask >>= 1;
    }

    // Check for textual ("white-listed") bytes.
    if s.dyn_ltree[9].freq() != 0 || s.dyn_ltree[10].freq() != 0 || s.dyn_ltree[13].freq() != 0 {
        return DataType::Text;
    }
    for n in 32..LITERALS {
        if s.dyn_ltree[n].freq() != 0 {
            return DataType::Text;
        }
    }

    // There are no "black-listed" or "white-listed" bytes: this block is either
    // empty or contains only "grey-listed" bytes, and is therefore binary.
    DataType::Binary
}

// ---------------------------------------------------------------------------
// Section 21: `_tr_flush_block` (trees.c) — choose block type and emit it.
// ---------------------------------------------------------------------------

/// Determines the best encoding for the current block and emits it.
///
/// `buf` is an optional offset into `s.window` naming the block start (`None`
/// means no stored representation is available, e.g. a flush sync block).
/// `stored_len` is the length of the uncompressed input for this block, and
/// `last` marks the final block of the stream. The block-type decision (stored
/// vs. static vs. dynamic) is byte-exact with reference zlib.
pub(crate) fn _tr_flush_block(
    s: &mut DeflateState,
    buf: Option<usize>,
    stored_len: usize,
    last: bool,
) {
    let mut opt_lenb: usize; // opt_len and static_len in bytes
    let static_lenb: usize;
    let max_blindex: usize; // index of last bit length code of nonzero freq

    // Build the Huffman trees unless a stored block is forced.
    if s.level > 0 {
        // Check if the file is binary or text.
        if s.data_type == DataType::Unknown {
            s.data_type = detect_data_type(s);
        }

        // Construct the literal and distance trees.
        build_tree(s, TreeKind::Literal);
        build_tree(s, TreeKind::Distance);
        // At this point, opt_len and static_len are the total bit lengths of the
        // compressed block data, excluding the tree representations.

        // Build the bit length tree for the above two trees, and get the index in
        // bl_order of the last bit length code to send.
        max_blindex = build_bl_tree(s);

        // Determine the best encoding. Compute the block lengths in bytes.
        opt_lenb = s.opt_len.wrapping_add(3 + 7) >> 3;
        static_lenb = s.static_len.wrapping_add(3 + 7) >> 3;

        // Force a static or stored block when static is at least as good, or when
        // Z_FIXED forces the fixed trees.
        if static_lenb <= opt_lenb || s.strategy == Strategy::Fixed {
            opt_lenb = static_lenb;
        }
    } else {
        // No trees; the block is stored, so its length is known.
        opt_lenb = stored_len + 5;
        static_lenb = stored_len + 5;
        max_blindex = 0;
    }

    if stored_len + 4 <= opt_lenb && buf.is_some() {
        // The test buf != NULL is only necessary if LIT_BUFSIZE > WSIZE. Otherwise
        // we can t have processed more than WSIZE input bytes since the last block
        // flush, because compression would have been successful. If LIT_BUFSIZE <=
        // WSIZE, it is never too late to transform a block into a stored block.
        _tr_stored_block(s, buf, stored_len, last);
    } else if static_lenb == opt_lenb {
        s.send_bits((STATIC_TREES << 1) + i32::from(last), 3);
        compress_block(s, TreeSet::Static);
    } else {
        s.send_bits((DYN_TREES << 1) + i32::from(last), 3);
        let lcodes = max_code(s, TreeKind::Literal) + 1;
        let dcodes = max_code(s, TreeKind::Distance) + 1;
        send_all_trees(s, lcodes, dcodes, max_blindex + 1);
        compress_block(s, TreeSet::Dynamic);
    }
    // The above check is made mod 2^32, for files larger than 512 MB and unsigned
    // long implemented on 32 bits.

    init_block(s);

    if last {
        s.bi_windup();
    }
}

// ---------------------------------------------------------------------------
// Section 22: `_tr_tally` and inline tally helpers (trees.c + deflate.h).
//
// Each symbol occupies three bytes in `sym_buf`: (dist_low, dist_high, lc).
// A literal is encoded with dist == 0; a match with dist != 0 where `lc` holds
// the match length minus MIN_MATCH. All three helpers return `true` when the
// symbol buffer is full and the block must be flushed.
// ---------------------------------------------------------------------------

/// Records a literal byte `c` in the symbol buffer.
///
/// Mirrors the inline `_tr_tally_lit` macro from `deflate.h` (non-`LIT_MEM`
/// layout). Returns `true` when the symbol buffer is full.
pub(crate) fn _tr_tally_lit(s: &mut DeflateState, c: u8) -> bool {
    let n = s.sym_next;
    s.sym_buf[n] = 0;
    s.sym_buf[n + 1] = 0;
    s.sym_buf[n + 2] = c;
    s.sym_next = n + 3;
    let cc = c as usize;
    let f = s.dyn_ltree[cc].freq();
    s.dyn_ltree[cc].set_freq(f.wrapping_add(1));
    s.sym_next == s.sym_end
}

/// Records a (distance, length) match in the symbol buffer.
///
/// `dist` is the match distance and `len` is the match length minus MIN_MATCH.
/// Mirrors the inline `_tr_tally_dist` macro from `deflate.h` (non-`LIT_MEM`
/// layout). Note: the inline macro does not bump `s.matches` (only the
/// `_tr_tally` function form does); this is faithful to the C source and has no
/// observable effect because `matches` is read only at level 0, where the tally
/// path is never taken. Returns `true` when the symbol buffer is full.
pub(crate) fn _tr_tally_dist(s: &mut DeflateState, dist: usize, len: u8) -> bool {
    let n = s.sym_next;
    s.sym_buf[n] = (dist & 0xff) as u8;
    s.sym_buf[n + 1] = (dist >> 8) as u8;
    s.sym_buf[n + 2] = len;
    s.sym_next = n + 3;
    let dist = dist - 1;
    let li = LENGTH_CODE[len as usize] as usize + LITERALS + 1;
    let f = s.dyn_ltree[li].freq();
    s.dyn_ltree[li].set_freq(f.wrapping_add(1));
    let di = d_code(dist);
    let fd = s.dyn_dtree[di].freq();
    s.dyn_dtree[di].set_freq(fd.wrapping_add(1));
    s.sym_next == s.sym_end
}

/// Records a literal (`dist == 0`) or a match (`dist != 0`) in the symbol buffer.
///
/// This is the function form of the C `_tr_tally`. When `dist != 0`, `lc` holds
/// the match length minus MIN_MATCH and `s.matches` is incremented (matching the
/// C function form). Returns `true` when the symbol buffer is full and the
/// current block should be flushed.
///
/// The compression engines normally use the faster inline [`_tr_tally_lit`] /
/// [`_tr_tally_dist`] helpers, so this function form is provided for API parity
/// and may be unused by some builds.
#[allow(dead_code)]
pub(crate) fn _tr_tally(s: &mut DeflateState, dist: usize, lc: usize) -> bool {
    let n = s.sym_next;
    s.sym_buf[n] = (dist & 0xff) as u8;
    s.sym_buf[n + 1] = (dist >> 8) as u8;
    s.sym_buf[n + 2] = lc as u8;
    s.sym_next = n + 3;
    if dist == 0 {
        // lc is the unmatched char.
        let f = s.dyn_ltree[lc].freq();
        s.dyn_ltree[lc].set_freq(f.wrapping_add(1));
    } else {
        s.matches = s.matches.wrapping_add(1);
        // Here, lc is the match length - MIN_MATCH.
        let dist = dist - 1; // dist = match distance - 1
        let li = LENGTH_CODE[lc] as usize + LITERALS + 1;
        let f = s.dyn_ltree[li].freq();
        s.dyn_ltree[li].set_freq(f.wrapping_add(1));
        let di = d_code(dist);
        let fd = s.dyn_dtree[di].freq();
        s.dyn_dtree[di].set_freq(fd.wrapping_add(1));
    }
    s.sym_next == s.sym_end
}

// ---------------------------------------------------------------------------
// Tests
//
// These tests are self-contained: they exercise only this module's private
// helpers plus the imported `CtData` and constants. In particular, the
// `static_tables_match_tr_static_init` test recomputes every static table using
// the canonical C `tr_static_init` algorithm and asserts equality with the
// transcribed tables, guarding against transcription errors. No `DeflateState`
// construction or external crates are required, so these run as part of the
// crate's normal `cargo test`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bi_reverse_known_values() {
        // Reversing within 1 bit is the identity on the single bit.
        assert_eq!(bi_reverse(0, 1), 0);
        assert_eq!(bi_reverse(1, 1), 1);
        // 5-bit reversals (used by the static distance tree).
        assert_eq!(bi_reverse(0b00001, 5), 0b10000);
        assert_eq!(bi_reverse(0b10000, 5), 0b00001);
        assert_eq!(bi_reverse(0b10110, 5), 0b01101);
        assert_eq!(bi_reverse(0b11111, 5), 0b11111);
        // 9-bit reversal sanity.
        assert_eq!(bi_reverse(0b000000001, 9), 0b100000000);
    }

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(MAX_BL_BITS, 7);
        assert_eq!(STORED_BLOCK, 0);
        assert_eq!(STATIC_TREES, 1);
        assert_eq!(DYN_TREES, 2);
        assert_eq!(REP_3_6, 16);
        assert_eq!(REPZ_3_10, 17);
        assert_eq!(REPZ_11_138, 18);
        assert_eq!(SMALLEST, 1);
        assert_eq!(DIST_CODE_LEN, 512);
        // Table sizes match the C alphabet sizes.
        assert_eq!(STATIC_LTREE.len(), L_CODES + 2);
        assert_eq!(STATIC_DTREE.len(), D_CODES);
        assert_eq!(DIST_CODE.len(), DIST_CODE_LEN);
        assert_eq!(LENGTH_CODE.len(), MAX_MATCH - MIN_MATCH + 1);
        assert_eq!(BASE_LENGTH.len(), LENGTH_CODES);
        assert_eq!(BASE_DIST.len(), D_CODES);
    }

    #[test]
    fn d_code_boundaries() {
        // Below 256 the distance indexes DIST_CODE directly.
        assert_eq!(d_code(0), DIST_CODE[0] as usize);
        assert_eq!(d_code(255), DIST_CODE[255] as usize);
        // At/above 256 the reduced index 256 + (dist >> 7) is used.
        assert_eq!(d_code(256), DIST_CODE[256 + (256 >> 7)] as usize);
        assert_eq!(d_code(32767), DIST_CODE[256 + (32767 >> 7)] as usize);
    }

    #[test]
    fn gen_codes_small_canonical() {
        // A tiny alphabet with lengths [2, 1, 3, 3] yields the canonical
        // assignment. With bl_count = {1: 1, 2: 1, 3: 2}, the first codes per
        // length are: len1 -> 0, len2 -> 10b, len3 -> 110b/111b. gen_codes stores
        // the bit-reversed codes.
        let mut tree = [CtData::default(); 4];
        tree[0].set_len(2);
        tree[1].set_len(1);
        tree[2].set_len(3);
        tree[3].set_len(3);
        let mut bl_count = [0u16; MAX_BITS + 1];
        bl_count[1] = 1;
        bl_count[2] = 1;
        bl_count[3] = 2;
        gen_codes(&mut tree, 3, &bl_count);
        // Expected canonical (pre-reversal) codes: sym1=0 (len1), sym0=10b (len2),
        // sym2=110b, sym3=111b (len3). Stored codes are bit-reversed.
        assert_eq!(tree[1].code(), bi_reverse(0, 1) as u16);
        assert_eq!(tree[0].code(), bi_reverse(0b10, 2) as u16);
        assert_eq!(tree[2].code(), bi_reverse(0b110, 3) as u16);
        assert_eq!(tree[3].code(), bi_reverse(0b111, 3) as u16);
    }

    /// Recomputes every static table using the canonical C `tr_static_init`
    /// algorithm and asserts equality with the transcribed tables. This is the
    /// transcription guard demanded by the file specification.
    #[test]
    fn static_tables_match_tr_static_init() {
        // ---- length code / base_length mapping ----
        let mut length_code = [0u8; MAX_MATCH - MIN_MATCH + 1];
        let mut base_length = [0i32; LENGTH_CODES];
        let mut length: usize = 0;
        for code in 0..(LENGTH_CODES - 1) {
            base_length[code] = length as i32;
            for _ in 0..(1usize << EXTRA_LBITS[code] as usize) {
                length_code[length] = code as u8;
                length += 1;
            }
        }
        assert_eq!(length, 256);
        // length 255 (match length 258) uses the better encoding (code 28).
        length_code[length - 1] = (LENGTH_CODES - 1) as u8;
        assert_eq!(length_code, LENGTH_CODE, "LENGTH_CODE mismatch");
        assert_eq!(base_length, BASE_LENGTH, "BASE_LENGTH mismatch");

        // ---- distance code / base_dist mapping ----
        let mut dist_code = [0u8; DIST_CODE_LEN];
        let mut base_dist = [0i32; D_CODES];
        let mut dist: usize = 0;
        for code in 0..16 {
            base_dist[code] = dist as i32;
            for _ in 0..(1usize << EXTRA_DBITS[code] as usize) {
                dist_code[dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256);
        dist >>= 7; // from now on, all distances are divided by 128
        for code in 16..D_CODES {
            base_dist[code] = (dist << 7) as i32;
            for _ in 0..(1usize << ((EXTRA_DBITS[code] - 7) as usize)) {
                dist_code[256 + dist] = code as u8;
                dist += 1;
            }
        }
        assert_eq!(dist, 256, "tr_static_init: 256 + dist != 512");
        assert_eq!(dist_code, DIST_CODE, "DIST_CODE mismatch");
        assert_eq!(base_dist, BASE_DIST, "BASE_DIST mismatch");

        // ---- static literal/length tree ----
        let mut ltree = [CtData::default(); L_CODES + 2];
        let mut bl_count = [0u16; MAX_BITS + 1];
        for (n, e) in ltree.iter_mut().enumerate() {
            let len: u16 = if n <= 143 {
                8
            } else if n <= 255 {
                9
            } else if n <= 279 {
                7
            } else {
                8
            };
            e.set_len(len);
            bl_count[len as usize] += 1;
        }
        // Codes 286 and 287 do not exist but are included so the tree is a
        // canonical Huffman tree (longest code all ones); gen_codes is called with
        // max_code = L_CODES + 1.
        gen_codes(&mut ltree, L_CODES + 1, &bl_count);
        for n in 0..(L_CODES + 2) {
            assert_eq!(
                ltree[n].len(),
                STATIC_LTREE[n].len(),
                "static_ltree Len[{n}]"
            );
            assert_eq!(
                ltree[n].code(),
                STATIC_LTREE[n].code(),
                "static_ltree Code[{n}]"
            );
        }

        // ---- static distance tree ----
        let mut dtree = [CtData::default(); D_CODES];
        for (n, e) in dtree.iter_mut().enumerate() {
            e.set_len(5);
            e.set_code(bi_reverse(n as u32, 5) as u16);
        }
        for n in 0..D_CODES {
            assert_eq!(
                dtree[n].len(),
                STATIC_DTREE[n].len(),
                "static_dtree Len[{n}]"
            );
            assert_eq!(
                dtree[n].code(),
                STATIC_DTREE[n].code(),
                "static_dtree Code[{n}]"
            );
        }
    }
}
