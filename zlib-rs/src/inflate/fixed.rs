//! Precomputed *fixed* (static) Huffman decode tables for `inflate`.
//!
//! This module is a safe, `no_std`-clean Rust port of zlib's `inffixed.h`
//! (zlib 1.3.2.1). It exposes exactly two items — [`LENFIX`] and [`DISTFIX`] —
//! the literal/length and distance decode tables used to inflate DEFLATE blocks
//! that carry the *fixed* Huffman codes (`BTYPE == 01`), as defined by
//! RFC 1951 §3.2.6. They are consumed by `state.rs` (the `TYPE`/`TYPEDO`
//! fixed-code branch) and by `back.rs`; the `lencode`/`lenbits`/`distcode`/
//! `distbits` assignment that C's `inflate_fixed()` performs lives in those
//! modules, not here.
//!
//! # Provenance and bit-exactness
//!
//! These tables are copied *verbatim* from the checked-in `inffixed.h`, which
//! zlib's `makefixed()` emitted from its `inflate_table()` builder. They are
//! therefore byte-for-byte identical to the canonical C header (the CP2
//! static-table-fidelity requirement). The live
//! [`crate::inflate::tables::inflate_table`] builder is reproduced
//! step-for-step by `tables.rs`, so the unit tests below reconstruct both
//! tables with that builder and assert byte-for-byte equality with the
//! constants here — agreeing at every entry except the four reserved-symbol
//! slots discussed below, which the test canonicalizes before comparing. A
//! single wrong entry would silently corrupt decoding of every fixed-Huffman
//! stream, so that reconstruction test is the canonical correctness proof.
//!
//! ## Note on the four reserved-symbol entries
//!
//! At the four positions whose index satisfies `index & 127 == 99` (indices 99,
//! 227, 355 and 483) the table decodes the reserved literal/length symbols 286
//! and 287, which RFC 1951 forbids from ever appearing in a valid stream.
//! [`LENFIX`] stores the canonical `{64, 8, 0}` invalid-code marker there,
//! copied verbatim from the checked-in `inffixed.h` (which `makefixed()`
//! produced by overriding the op byte to `64`). The live `inflate_table()`
//! builder instead writes this build's over-length sentinels `lext[29] = 68` /
//! `lext[30] = 193` at those slots. All three op bytes (64, 68, 193) set the
//! `0x40` "invalid code" bit and clear the `0x10` length and `0x20`
//! end-of-block bits, so every one of them rejects symbols 286/287 identically;
//! the decoder's observable behavior — and therefore wire-format compatibility —
//! is unaffected by the choice. Because `LENFIX` is kept byte-for-byte identical
//! to `inffixed.h`, the reconstruction test below normalizes the builder's
//! terminal-invalid markers to `64` before its byte-for-byte comparison, so it
//! still verifies fidelity at all 512 indices.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`** (the crate declares
//! `#![forbid(unsafe_code)]`). Both tables are immutable `static` arrays baked
//! into the binary — a single shared instance, mirroring the C `static const`.

use crate::inflate::tables::Code;

/// Fixed literal/length decode table (root index = 9 bits).
///
/// Ported from zlib's `inffixed.h` `lenfix[512]` and verified, entry-for-entry,
/// against [`crate::inflate::tables::inflate_table`] (see the module-level note
/// about the four reserved-symbol entries at indices 99, 227, 355 and 483).
/// Used to decode the literal/length alphabet of DEFLATE fixed-Huffman blocks
/// (`BTYPE == 01`).
#[rustfmt::skip]
pub static LENFIX: [Code; 512] = [
    Code { op: 96, bits: 7, val: 0 },
    Code { op: 0, bits: 8, val: 80 },
    Code { op: 0, bits: 8, val: 16 },
    Code { op: 20, bits: 8, val: 115 },
    Code { op: 18, bits: 7, val: 31 },
    Code { op: 0, bits: 8, val: 112 },
    Code { op: 0, bits: 8, val: 48 },
    Code { op: 0, bits: 9, val: 192 },
    Code { op: 16, bits: 7, val: 10 },
    Code { op: 0, bits: 8, val: 96 },
    Code { op: 0, bits: 8, val: 32 },
    Code { op: 0, bits: 9, val: 160 },
    Code { op: 0, bits: 8, val: 0 },
    Code { op: 0, bits: 8, val: 128 },
    Code { op: 0, bits: 8, val: 64 },
    Code { op: 0, bits: 9, val: 224 },
    Code { op: 16, bits: 7, val: 6 },
    Code { op: 0, bits: 8, val: 88 },
    Code { op: 0, bits: 8, val: 24 },
    Code { op: 0, bits: 9, val: 144 },
    Code { op: 19, bits: 7, val: 59 },
    Code { op: 0, bits: 8, val: 120 },
    Code { op: 0, bits: 8, val: 56 },
    Code { op: 0, bits: 9, val: 208 },
    Code { op: 17, bits: 7, val: 17 },
    Code { op: 0, bits: 8, val: 104 },
    Code { op: 0, bits: 8, val: 40 },
    Code { op: 0, bits: 9, val: 176 },
    Code { op: 0, bits: 8, val: 8 },
    Code { op: 0, bits: 8, val: 136 },
    Code { op: 0, bits: 8, val: 72 },
    Code { op: 0, bits: 9, val: 240 },
    Code { op: 16, bits: 7, val: 4 },
    Code { op: 0, bits: 8, val: 84 },
    Code { op: 0, bits: 8, val: 20 },
    Code { op: 21, bits: 8, val: 227 },
    Code { op: 19, bits: 7, val: 43 },
    Code { op: 0, bits: 8, val: 116 },
    Code { op: 0, bits: 8, val: 52 },
    Code { op: 0, bits: 9, val: 200 },
    Code { op: 17, bits: 7, val: 13 },
    Code { op: 0, bits: 8, val: 100 },
    Code { op: 0, bits: 8, val: 36 },
    Code { op: 0, bits: 9, val: 168 },
    Code { op: 0, bits: 8, val: 4 },
    Code { op: 0, bits: 8, val: 132 },
    Code { op: 0, bits: 8, val: 68 },
    Code { op: 0, bits: 9, val: 232 },
    Code { op: 16, bits: 7, val: 8 },
    Code { op: 0, bits: 8, val: 92 },
    Code { op: 0, bits: 8, val: 28 },
    Code { op: 0, bits: 9, val: 152 },
    Code { op: 20, bits: 7, val: 83 },
    Code { op: 0, bits: 8, val: 124 },
    Code { op: 0, bits: 8, val: 60 },
    Code { op: 0, bits: 9, val: 216 },
    Code { op: 18, bits: 7, val: 23 },
    Code { op: 0, bits: 8, val: 108 },
    Code { op: 0, bits: 8, val: 44 },
    Code { op: 0, bits: 9, val: 184 },
    Code { op: 0, bits: 8, val: 12 },
    Code { op: 0, bits: 8, val: 140 },
    Code { op: 0, bits: 8, val: 76 },
    Code { op: 0, bits: 9, val: 248 },
    Code { op: 16, bits: 7, val: 3 },
    Code { op: 0, bits: 8, val: 82 },
    Code { op: 0, bits: 8, val: 18 },
    Code { op: 21, bits: 8, val: 163 },
    Code { op: 19, bits: 7, val: 35 },
    Code { op: 0, bits: 8, val: 114 },
    Code { op: 0, bits: 8, val: 50 },
    Code { op: 0, bits: 9, val: 196 },
    Code { op: 17, bits: 7, val: 11 },
    Code { op: 0, bits: 8, val: 98 },
    Code { op: 0, bits: 8, val: 34 },
    Code { op: 0, bits: 9, val: 164 },
    Code { op: 0, bits: 8, val: 2 },
    Code { op: 0, bits: 8, val: 130 },
    Code { op: 0, bits: 8, val: 66 },
    Code { op: 0, bits: 9, val: 228 },
    Code { op: 16, bits: 7, val: 7 },
    Code { op: 0, bits: 8, val: 90 },
    Code { op: 0, bits: 8, val: 26 },
    Code { op: 0, bits: 9, val: 148 },
    Code { op: 20, bits: 7, val: 67 },
    Code { op: 0, bits: 8, val: 122 },
    Code { op: 0, bits: 8, val: 58 },
    Code { op: 0, bits: 9, val: 212 },
    Code { op: 18, bits: 7, val: 19 },
    Code { op: 0, bits: 8, val: 106 },
    Code { op: 0, bits: 8, val: 42 },
    Code { op: 0, bits: 9, val: 180 },
    Code { op: 0, bits: 8, val: 10 },
    Code { op: 0, bits: 8, val: 138 },
    Code { op: 0, bits: 8, val: 74 },
    Code { op: 0, bits: 9, val: 244 },
    Code { op: 16, bits: 7, val: 5 },
    Code { op: 0, bits: 8, val: 86 },
    Code { op: 0, bits: 8, val: 22 },
    Code { op: 64, bits: 8, val: 0 },
    Code { op: 19, bits: 7, val: 51 },
    Code { op: 0, bits: 8, val: 118 },
    Code { op: 0, bits: 8, val: 54 },
    Code { op: 0, bits: 9, val: 204 },
    Code { op: 17, bits: 7, val: 15 },
    Code { op: 0, bits: 8, val: 102 },
    Code { op: 0, bits: 8, val: 38 },
    Code { op: 0, bits: 9, val: 172 },
    Code { op: 0, bits: 8, val: 6 },
    Code { op: 0, bits: 8, val: 134 },
    Code { op: 0, bits: 8, val: 70 },
    Code { op: 0, bits: 9, val: 236 },
    Code { op: 16, bits: 7, val: 9 },
    Code { op: 0, bits: 8, val: 94 },
    Code { op: 0, bits: 8, val: 30 },
    Code { op: 0, bits: 9, val: 156 },
    Code { op: 20, bits: 7, val: 99 },
    Code { op: 0, bits: 8, val: 126 },
    Code { op: 0, bits: 8, val: 62 },
    Code { op: 0, bits: 9, val: 220 },
    Code { op: 18, bits: 7, val: 27 },
    Code { op: 0, bits: 8, val: 110 },
    Code { op: 0, bits: 8, val: 46 },
    Code { op: 0, bits: 9, val: 188 },
    Code { op: 0, bits: 8, val: 14 },
    Code { op: 0, bits: 8, val: 142 },
    Code { op: 0, bits: 8, val: 78 },
    Code { op: 0, bits: 9, val: 252 },
    Code { op: 96, bits: 7, val: 0 },
    Code { op: 0, bits: 8, val: 81 },
    Code { op: 0, bits: 8, val: 17 },
    Code { op: 21, bits: 8, val: 131 },
    Code { op: 18, bits: 7, val: 31 },
    Code { op: 0, bits: 8, val: 113 },
    Code { op: 0, bits: 8, val: 49 },
    Code { op: 0, bits: 9, val: 194 },
    Code { op: 16, bits: 7, val: 10 },
    Code { op: 0, bits: 8, val: 97 },
    Code { op: 0, bits: 8, val: 33 },
    Code { op: 0, bits: 9, val: 162 },
    Code { op: 0, bits: 8, val: 1 },
    Code { op: 0, bits: 8, val: 129 },
    Code { op: 0, bits: 8, val: 65 },
    Code { op: 0, bits: 9, val: 226 },
    Code { op: 16, bits: 7, val: 6 },
    Code { op: 0, bits: 8, val: 89 },
    Code { op: 0, bits: 8, val: 25 },
    Code { op: 0, bits: 9, val: 146 },
    Code { op: 19, bits: 7, val: 59 },
    Code { op: 0, bits: 8, val: 121 },
    Code { op: 0, bits: 8, val: 57 },
    Code { op: 0, bits: 9, val: 210 },
    Code { op: 17, bits: 7, val: 17 },
    Code { op: 0, bits: 8, val: 105 },
    Code { op: 0, bits: 8, val: 41 },
    Code { op: 0, bits: 9, val: 178 },
    Code { op: 0, bits: 8, val: 9 },
    Code { op: 0, bits: 8, val: 137 },
    Code { op: 0, bits: 8, val: 73 },
    Code { op: 0, bits: 9, val: 242 },
    Code { op: 16, bits: 7, val: 4 },
    Code { op: 0, bits: 8, val: 85 },
    Code { op: 0, bits: 8, val: 21 },
    Code { op: 16, bits: 8, val: 258 },
    Code { op: 19, bits: 7, val: 43 },
    Code { op: 0, bits: 8, val: 117 },
    Code { op: 0, bits: 8, val: 53 },
    Code { op: 0, bits: 9, val: 202 },
    Code { op: 17, bits: 7, val: 13 },
    Code { op: 0, bits: 8, val: 101 },
    Code { op: 0, bits: 8, val: 37 },
    Code { op: 0, bits: 9, val: 170 },
    Code { op: 0, bits: 8, val: 5 },
    Code { op: 0, bits: 8, val: 133 },
    Code { op: 0, bits: 8, val: 69 },
    Code { op: 0, bits: 9, val: 234 },
    Code { op: 16, bits: 7, val: 8 },
    Code { op: 0, bits: 8, val: 93 },
    Code { op: 0, bits: 8, val: 29 },
    Code { op: 0, bits: 9, val: 154 },
    Code { op: 20, bits: 7, val: 83 },
    Code { op: 0, bits: 8, val: 125 },
    Code { op: 0, bits: 8, val: 61 },
    Code { op: 0, bits: 9, val: 218 },
    Code { op: 18, bits: 7, val: 23 },
    Code { op: 0, bits: 8, val: 109 },
    Code { op: 0, bits: 8, val: 45 },
    Code { op: 0, bits: 9, val: 186 },
    Code { op: 0, bits: 8, val: 13 },
    Code { op: 0, bits: 8, val: 141 },
    Code { op: 0, bits: 8, val: 77 },
    Code { op: 0, bits: 9, val: 250 },
    Code { op: 16, bits: 7, val: 3 },
    Code { op: 0, bits: 8, val: 83 },
    Code { op: 0, bits: 8, val: 19 },
    Code { op: 21, bits: 8, val: 195 },
    Code { op: 19, bits: 7, val: 35 },
    Code { op: 0, bits: 8, val: 115 },
    Code { op: 0, bits: 8, val: 51 },
    Code { op: 0, bits: 9, val: 198 },
    Code { op: 17, bits: 7, val: 11 },
    Code { op: 0, bits: 8, val: 99 },
    Code { op: 0, bits: 8, val: 35 },
    Code { op: 0, bits: 9, val: 166 },
    Code { op: 0, bits: 8, val: 3 },
    Code { op: 0, bits: 8, val: 131 },
    Code { op: 0, bits: 8, val: 67 },
    Code { op: 0, bits: 9, val: 230 },
    Code { op: 16, bits: 7, val: 7 },
    Code { op: 0, bits: 8, val: 91 },
    Code { op: 0, bits: 8, val: 27 },
    Code { op: 0, bits: 9, val: 150 },
    Code { op: 20, bits: 7, val: 67 },
    Code { op: 0, bits: 8, val: 123 },
    Code { op: 0, bits: 8, val: 59 },
    Code { op: 0, bits: 9, val: 214 },
    Code { op: 18, bits: 7, val: 19 },
    Code { op: 0, bits: 8, val: 107 },
    Code { op: 0, bits: 8, val: 43 },
    Code { op: 0, bits: 9, val: 182 },
    Code { op: 0, bits: 8, val: 11 },
    Code { op: 0, bits: 8, val: 139 },
    Code { op: 0, bits: 8, val: 75 },
    Code { op: 0, bits: 9, val: 246 },
    Code { op: 16, bits: 7, val: 5 },
    Code { op: 0, bits: 8, val: 87 },
    Code { op: 0, bits: 8, val: 23 },
    Code { op: 64, bits: 8, val: 0 },
    Code { op: 19, bits: 7, val: 51 },
    Code { op: 0, bits: 8, val: 119 },
    Code { op: 0, bits: 8, val: 55 },
    Code { op: 0, bits: 9, val: 206 },
    Code { op: 17, bits: 7, val: 15 },
    Code { op: 0, bits: 8, val: 103 },
    Code { op: 0, bits: 8, val: 39 },
    Code { op: 0, bits: 9, val: 174 },
    Code { op: 0, bits: 8, val: 7 },
    Code { op: 0, bits: 8, val: 135 },
    Code { op: 0, bits: 8, val: 71 },
    Code { op: 0, bits: 9, val: 238 },
    Code { op: 16, bits: 7, val: 9 },
    Code { op: 0, bits: 8, val: 95 },
    Code { op: 0, bits: 8, val: 31 },
    Code { op: 0, bits: 9, val: 158 },
    Code { op: 20, bits: 7, val: 99 },
    Code { op: 0, bits: 8, val: 127 },
    Code { op: 0, bits: 8, val: 63 },
    Code { op: 0, bits: 9, val: 222 },
    Code { op: 18, bits: 7, val: 27 },
    Code { op: 0, bits: 8, val: 111 },
    Code { op: 0, bits: 8, val: 47 },
    Code { op: 0, bits: 9, val: 190 },
    Code { op: 0, bits: 8, val: 15 },
    Code { op: 0, bits: 8, val: 143 },
    Code { op: 0, bits: 8, val: 79 },
    Code { op: 0, bits: 9, val: 254 },
    Code { op: 96, bits: 7, val: 0 },
    Code { op: 0, bits: 8, val: 80 },
    Code { op: 0, bits: 8, val: 16 },
    Code { op: 20, bits: 8, val: 115 },
    Code { op: 18, bits: 7, val: 31 },
    Code { op: 0, bits: 8, val: 112 },
    Code { op: 0, bits: 8, val: 48 },
    Code { op: 0, bits: 9, val: 193 },
    Code { op: 16, bits: 7, val: 10 },
    Code { op: 0, bits: 8, val: 96 },
    Code { op: 0, bits: 8, val: 32 },
    Code { op: 0, bits: 9, val: 161 },
    Code { op: 0, bits: 8, val: 0 },
    Code { op: 0, bits: 8, val: 128 },
    Code { op: 0, bits: 8, val: 64 },
    Code { op: 0, bits: 9, val: 225 },
    Code { op: 16, bits: 7, val: 6 },
    Code { op: 0, bits: 8, val: 88 },
    Code { op: 0, bits: 8, val: 24 },
    Code { op: 0, bits: 9, val: 145 },
    Code { op: 19, bits: 7, val: 59 },
    Code { op: 0, bits: 8, val: 120 },
    Code { op: 0, bits: 8, val: 56 },
    Code { op: 0, bits: 9, val: 209 },
    Code { op: 17, bits: 7, val: 17 },
    Code { op: 0, bits: 8, val: 104 },
    Code { op: 0, bits: 8, val: 40 },
    Code { op: 0, bits: 9, val: 177 },
    Code { op: 0, bits: 8, val: 8 },
    Code { op: 0, bits: 8, val: 136 },
    Code { op: 0, bits: 8, val: 72 },
    Code { op: 0, bits: 9, val: 241 },
    Code { op: 16, bits: 7, val: 4 },
    Code { op: 0, bits: 8, val: 84 },
    Code { op: 0, bits: 8, val: 20 },
    Code { op: 21, bits: 8, val: 227 },
    Code { op: 19, bits: 7, val: 43 },
    Code { op: 0, bits: 8, val: 116 },
    Code { op: 0, bits: 8, val: 52 },
    Code { op: 0, bits: 9, val: 201 },
    Code { op: 17, bits: 7, val: 13 },
    Code { op: 0, bits: 8, val: 100 },
    Code { op: 0, bits: 8, val: 36 },
    Code { op: 0, bits: 9, val: 169 },
    Code { op: 0, bits: 8, val: 4 },
    Code { op: 0, bits: 8, val: 132 },
    Code { op: 0, bits: 8, val: 68 },
    Code { op: 0, bits: 9, val: 233 },
    Code { op: 16, bits: 7, val: 8 },
    Code { op: 0, bits: 8, val: 92 },
    Code { op: 0, bits: 8, val: 28 },
    Code { op: 0, bits: 9, val: 153 },
    Code { op: 20, bits: 7, val: 83 },
    Code { op: 0, bits: 8, val: 124 },
    Code { op: 0, bits: 8, val: 60 },
    Code { op: 0, bits: 9, val: 217 },
    Code { op: 18, bits: 7, val: 23 },
    Code { op: 0, bits: 8, val: 108 },
    Code { op: 0, bits: 8, val: 44 },
    Code { op: 0, bits: 9, val: 185 },
    Code { op: 0, bits: 8, val: 12 },
    Code { op: 0, bits: 8, val: 140 },
    Code { op: 0, bits: 8, val: 76 },
    Code { op: 0, bits: 9, val: 249 },
    Code { op: 16, bits: 7, val: 3 },
    Code { op: 0, bits: 8, val: 82 },
    Code { op: 0, bits: 8, val: 18 },
    Code { op: 21, bits: 8, val: 163 },
    Code { op: 19, bits: 7, val: 35 },
    Code { op: 0, bits: 8, val: 114 },
    Code { op: 0, bits: 8, val: 50 },
    Code { op: 0, bits: 9, val: 197 },
    Code { op: 17, bits: 7, val: 11 },
    Code { op: 0, bits: 8, val: 98 },
    Code { op: 0, bits: 8, val: 34 },
    Code { op: 0, bits: 9, val: 165 },
    Code { op: 0, bits: 8, val: 2 },
    Code { op: 0, bits: 8, val: 130 },
    Code { op: 0, bits: 8, val: 66 },
    Code { op: 0, bits: 9, val: 229 },
    Code { op: 16, bits: 7, val: 7 },
    Code { op: 0, bits: 8, val: 90 },
    Code { op: 0, bits: 8, val: 26 },
    Code { op: 0, bits: 9, val: 149 },
    Code { op: 20, bits: 7, val: 67 },
    Code { op: 0, bits: 8, val: 122 },
    Code { op: 0, bits: 8, val: 58 },
    Code { op: 0, bits: 9, val: 213 },
    Code { op: 18, bits: 7, val: 19 },
    Code { op: 0, bits: 8, val: 106 },
    Code { op: 0, bits: 8, val: 42 },
    Code { op: 0, bits: 9, val: 181 },
    Code { op: 0, bits: 8, val: 10 },
    Code { op: 0, bits: 8, val: 138 },
    Code { op: 0, bits: 8, val: 74 },
    Code { op: 0, bits: 9, val: 245 },
    Code { op: 16, bits: 7, val: 5 },
    Code { op: 0, bits: 8, val: 86 },
    Code { op: 0, bits: 8, val: 22 },
    Code { op: 64, bits: 8, val: 0 },
    Code { op: 19, bits: 7, val: 51 },
    Code { op: 0, bits: 8, val: 118 },
    Code { op: 0, bits: 8, val: 54 },
    Code { op: 0, bits: 9, val: 205 },
    Code { op: 17, bits: 7, val: 15 },
    Code { op: 0, bits: 8, val: 102 },
    Code { op: 0, bits: 8, val: 38 },
    Code { op: 0, bits: 9, val: 173 },
    Code { op: 0, bits: 8, val: 6 },
    Code { op: 0, bits: 8, val: 134 },
    Code { op: 0, bits: 8, val: 70 },
    Code { op: 0, bits: 9, val: 237 },
    Code { op: 16, bits: 7, val: 9 },
    Code { op: 0, bits: 8, val: 94 },
    Code { op: 0, bits: 8, val: 30 },
    Code { op: 0, bits: 9, val: 157 },
    Code { op: 20, bits: 7, val: 99 },
    Code { op: 0, bits: 8, val: 126 },
    Code { op: 0, bits: 8, val: 62 },
    Code { op: 0, bits: 9, val: 221 },
    Code { op: 18, bits: 7, val: 27 },
    Code { op: 0, bits: 8, val: 110 },
    Code { op: 0, bits: 8, val: 46 },
    Code { op: 0, bits: 9, val: 189 },
    Code { op: 0, bits: 8, val: 14 },
    Code { op: 0, bits: 8, val: 142 },
    Code { op: 0, bits: 8, val: 78 },
    Code { op: 0, bits: 9, val: 253 },
    Code { op: 96, bits: 7, val: 0 },
    Code { op: 0, bits: 8, val: 81 },
    Code { op: 0, bits: 8, val: 17 },
    Code { op: 21, bits: 8, val: 131 },
    Code { op: 18, bits: 7, val: 31 },
    Code { op: 0, bits: 8, val: 113 },
    Code { op: 0, bits: 8, val: 49 },
    Code { op: 0, bits: 9, val: 195 },
    Code { op: 16, bits: 7, val: 10 },
    Code { op: 0, bits: 8, val: 97 },
    Code { op: 0, bits: 8, val: 33 },
    Code { op: 0, bits: 9, val: 163 },
    Code { op: 0, bits: 8, val: 1 },
    Code { op: 0, bits: 8, val: 129 },
    Code { op: 0, bits: 8, val: 65 },
    Code { op: 0, bits: 9, val: 227 },
    Code { op: 16, bits: 7, val: 6 },
    Code { op: 0, bits: 8, val: 89 },
    Code { op: 0, bits: 8, val: 25 },
    Code { op: 0, bits: 9, val: 147 },
    Code { op: 19, bits: 7, val: 59 },
    Code { op: 0, bits: 8, val: 121 },
    Code { op: 0, bits: 8, val: 57 },
    Code { op: 0, bits: 9, val: 211 },
    Code { op: 17, bits: 7, val: 17 },
    Code { op: 0, bits: 8, val: 105 },
    Code { op: 0, bits: 8, val: 41 },
    Code { op: 0, bits: 9, val: 179 },
    Code { op: 0, bits: 8, val: 9 },
    Code { op: 0, bits: 8, val: 137 },
    Code { op: 0, bits: 8, val: 73 },
    Code { op: 0, bits: 9, val: 243 },
    Code { op: 16, bits: 7, val: 4 },
    Code { op: 0, bits: 8, val: 85 },
    Code { op: 0, bits: 8, val: 21 },
    Code { op: 16, bits: 8, val: 258 },
    Code { op: 19, bits: 7, val: 43 },
    Code { op: 0, bits: 8, val: 117 },
    Code { op: 0, bits: 8, val: 53 },
    Code { op: 0, bits: 9, val: 203 },
    Code { op: 17, bits: 7, val: 13 },
    Code { op: 0, bits: 8, val: 101 },
    Code { op: 0, bits: 8, val: 37 },
    Code { op: 0, bits: 9, val: 171 },
    Code { op: 0, bits: 8, val: 5 },
    Code { op: 0, bits: 8, val: 133 },
    Code { op: 0, bits: 8, val: 69 },
    Code { op: 0, bits: 9, val: 235 },
    Code { op: 16, bits: 7, val: 8 },
    Code { op: 0, bits: 8, val: 93 },
    Code { op: 0, bits: 8, val: 29 },
    Code { op: 0, bits: 9, val: 155 },
    Code { op: 20, bits: 7, val: 83 },
    Code { op: 0, bits: 8, val: 125 },
    Code { op: 0, bits: 8, val: 61 },
    Code { op: 0, bits: 9, val: 219 },
    Code { op: 18, bits: 7, val: 23 },
    Code { op: 0, bits: 8, val: 109 },
    Code { op: 0, bits: 8, val: 45 },
    Code { op: 0, bits: 9, val: 187 },
    Code { op: 0, bits: 8, val: 13 },
    Code { op: 0, bits: 8, val: 141 },
    Code { op: 0, bits: 8, val: 77 },
    Code { op: 0, bits: 9, val: 251 },
    Code { op: 16, bits: 7, val: 3 },
    Code { op: 0, bits: 8, val: 83 },
    Code { op: 0, bits: 8, val: 19 },
    Code { op: 21, bits: 8, val: 195 },
    Code { op: 19, bits: 7, val: 35 },
    Code { op: 0, bits: 8, val: 115 },
    Code { op: 0, bits: 8, val: 51 },
    Code { op: 0, bits: 9, val: 199 },
    Code { op: 17, bits: 7, val: 11 },
    Code { op: 0, bits: 8, val: 99 },
    Code { op: 0, bits: 8, val: 35 },
    Code { op: 0, bits: 9, val: 167 },
    Code { op: 0, bits: 8, val: 3 },
    Code { op: 0, bits: 8, val: 131 },
    Code { op: 0, bits: 8, val: 67 },
    Code { op: 0, bits: 9, val: 231 },
    Code { op: 16, bits: 7, val: 7 },
    Code { op: 0, bits: 8, val: 91 },
    Code { op: 0, bits: 8, val: 27 },
    Code { op: 0, bits: 9, val: 151 },
    Code { op: 20, bits: 7, val: 67 },
    Code { op: 0, bits: 8, val: 123 },
    Code { op: 0, bits: 8, val: 59 },
    Code { op: 0, bits: 9, val: 215 },
    Code { op: 18, bits: 7, val: 19 },
    Code { op: 0, bits: 8, val: 107 },
    Code { op: 0, bits: 8, val: 43 },
    Code { op: 0, bits: 9, val: 183 },
    Code { op: 0, bits: 8, val: 11 },
    Code { op: 0, bits: 8, val: 139 },
    Code { op: 0, bits: 8, val: 75 },
    Code { op: 0, bits: 9, val: 247 },
    Code { op: 16, bits: 7, val: 5 },
    Code { op: 0, bits: 8, val: 87 },
    Code { op: 0, bits: 8, val: 23 },
    Code { op: 64, bits: 8, val: 0 },
    Code { op: 19, bits: 7, val: 51 },
    Code { op: 0, bits: 8, val: 119 },
    Code { op: 0, bits: 8, val: 55 },
    Code { op: 0, bits: 9, val: 207 },
    Code { op: 17, bits: 7, val: 15 },
    Code { op: 0, bits: 8, val: 103 },
    Code { op: 0, bits: 8, val: 39 },
    Code { op: 0, bits: 9, val: 175 },
    Code { op: 0, bits: 8, val: 7 },
    Code { op: 0, bits: 8, val: 135 },
    Code { op: 0, bits: 8, val: 71 },
    Code { op: 0, bits: 9, val: 239 },
    Code { op: 16, bits: 7, val: 9 },
    Code { op: 0, bits: 8, val: 95 },
    Code { op: 0, bits: 8, val: 31 },
    Code { op: 0, bits: 9, val: 159 },
    Code { op: 20, bits: 7, val: 99 },
    Code { op: 0, bits: 8, val: 127 },
    Code { op: 0, bits: 8, val: 63 },
    Code { op: 0, bits: 9, val: 223 },
    Code { op: 18, bits: 7, val: 27 },
    Code { op: 0, bits: 8, val: 111 },
    Code { op: 0, bits: 8, val: 47 },
    Code { op: 0, bits: 9, val: 191 },
    Code { op: 0, bits: 8, val: 15 },
    Code { op: 0, bits: 8, val: 143 },
    Code { op: 0, bits: 8, val: 79 },
    Code { op: 0, bits: 9, val: 255 },
];

// The fixed literal/length root table is always exactly 512 entries (a complete
// 9-bit root table with no sub-tables, since the longest fixed code is 9 bits).
const _: () = assert!(LENFIX.len() == 512);

/// Fixed distance decode table (root index = 5 bits).
///
/// Ported verbatim from zlib's `inffixed.h` `distfix[32]` and verified
/// entry-for-entry against [`crate::inflate::tables::inflate_table`]. Used to
/// decode the distance alphabet of DEFLATE fixed-Huffman blocks. Indices 30 and
/// 31 hold `{64, 5, 0}` invalid-code markers for the two reserved distance codes.
#[rustfmt::skip]
pub static DISTFIX: [Code; 32] = [
    Code { op: 16, bits: 5, val: 1 },
    Code { op: 23, bits: 5, val: 257 },
    Code { op: 19, bits: 5, val: 17 },
    Code { op: 27, bits: 5, val: 4097 },
    Code { op: 17, bits: 5, val: 5 },
    Code { op: 25, bits: 5, val: 1025 },
    Code { op: 21, bits: 5, val: 65 },
    Code { op: 29, bits: 5, val: 16385 },
    Code { op: 16, bits: 5, val: 3 },
    Code { op: 24, bits: 5, val: 513 },
    Code { op: 20, bits: 5, val: 33 },
    Code { op: 28, bits: 5, val: 8193 },
    Code { op: 18, bits: 5, val: 9 },
    Code { op: 26, bits: 5, val: 2049 },
    Code { op: 22, bits: 5, val: 129 },
    Code { op: 64, bits: 5, val: 0 },
    Code { op: 16, bits: 5, val: 2 },
    Code { op: 23, bits: 5, val: 385 },
    Code { op: 19, bits: 5, val: 25 },
    Code { op: 27, bits: 5, val: 6145 },
    Code { op: 17, bits: 5, val: 7 },
    Code { op: 25, bits: 5, val: 1537 },
    Code { op: 21, bits: 5, val: 97 },
    Code { op: 29, bits: 5, val: 24577 },
    Code { op: 16, bits: 5, val: 4 },
    Code { op: 24, bits: 5, val: 769 },
    Code { op: 20, bits: 5, val: 49 },
    Code { op: 28, bits: 5, val: 12289 },
    Code { op: 18, bits: 5, val: 13 },
    Code { op: 26, bits: 5, val: 3073 },
    Code { op: 22, bits: 5, val: 193 },
    Code { op: 64, bits: 5, val: 0 },
];

// The fixed distance root table is always exactly 32 entries (a complete 5-bit
// root table; all 30 distance codes plus two reserved invalid slots).
const _: () = assert!(DISTFIX.len() == 32);

#[cfg(test)]
mod tests {
    use super::{DISTFIX, LENFIX};
    use crate::inflate::tables::{Code, CodeType, inflate_table};

    /// Reconstruct the fixed literal/length decode table with the live
    /// `inflate_table` builder and assert it is byte-identical to [`LENFIX`].
    ///
    /// This is exactly the computation zlib performs in a `BUILDFIXED` build, and
    /// is the canonical proof that the shipped table is correct. The fixed
    /// literal/length code lengths are fixed by RFC 1951 §3.2.6: 8 bits for
    /// symbols `0..144`, 9 bits for `144..256`, 7 bits for `256..280`, and 8 bits
    /// for `280..288`.
    #[test]
    fn lenfix_matches_builder() {
        let mut lens = [0u16; 288];
        lens[0..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..288].fill(8);

        let mut codes = [Code::default(); 512];
        let mut work = [0u16; 288];
        let mut bits = 9usize;
        let used = inflate_table(CodeType::Lens, &lens, 288, &mut codes, &mut bits, &mut work)
            .expect("fixed literal/length table builds");

        assert_eq!(used, 512, "fixed lit/len table consumes 512 entries");
        assert_eq!(bits, 9, "fixed lit/len root index is 9 bits");

        // Canonicalize the four reserved symbol-286/287 slots before comparing.
        //
        // The live `inflate_table` builder writes this build's `lext[29] = 68`
        // and `lext[30] = 193` over-length sentinels into those slots (indices
        // 99, 227, 355, 483), whereas the verbatim `inffixed.h` table — which
        // `LENFIX` now reproduces exactly — stores the canonical `op = 64`
        // invalid-code marker that zlib's `makefixed()` writes. All three op
        // bytes set the `0x40` "invalid code" bit and clear the `0x10` length and
        // `0x20` end-of-block bits, so they reject the reserved symbols
        // identically. Normalizing every such terminal-invalid marker to 64
        // tolerates only this benign, behavior-equivalent op-byte difference
        // while still proving byte-for-byte fidelity at every other index (any
        // genuinely wrong op — e.g. a literal or length marker — is not collapsed
        // and would still fail the comparison).
        for c in codes.iter_mut() {
            if c.op & 0x40 != 0 && c.op & 0x30 == 0 {
                c.op = 64;
            }
        }

        assert_eq!(
            &codes[..],
            &LENFIX[..],
            "reconstructed lit/len table must be byte-identical to LENFIX \
             (reserved-symbol invalid markers normalized to op = 64)",
        );
    }

    /// Reconstruct the fixed distance decode table with the live `inflate_table`
    /// builder and assert it is byte-identical to [`DISTFIX`]. Every one of the
    /// 30 distance codes (plus the two reserved slots) is 5 bits.
    #[test]
    fn distfix_matches_builder() {
        let lens = [5u16; 32];

        let mut codes = [Code::default(); 32];
        let mut work = [0u16; 32];
        let mut bits = 5usize;
        let used = inflate_table(CodeType::Dists, &lens, 32, &mut codes, &mut bits, &mut work)
            .expect("fixed distance table builds");

        assert_eq!(used, 32, "fixed distance table consumes 32 entries");
        assert_eq!(bits, 5, "fixed distance root index is 5 bits");
        assert_eq!(
            &codes[..],
            &DISTFIX[..],
            "reconstructed distance table must be byte-identical to DISTFIX",
        );
    }
}
