//! Fixed (static) Huffman decode tables for the inflate engine.
//!
//! This module is the safe-Rust port of zlib's `inffixed.h`. Both arrays are
//! produced upstream by zlib's `makefixed()` generator (the `#ifdef MAKEFIXED`
//! path in `inftrees.c`); they are the canonical fixed-code decode tables
//! mandated by RFC 1951 §3.2.6 (the "compressed with fixed Huffman codes" block
//! type) and are reproduced here **byte-for-byte**. A single wrong entry would
//! corrupt every fixed-block decode and break both RFC 1951 conformance and
//! byte-identical interoperability with C zlib.
//!
//! [`LENFIX`] is the 512-entry literal/length table (indexed with 9 root bits)
//! and [`DISTFIX`] is the 32-entry distance table (indexed with 5 root bits).
//! [`crate::inflate::tables::inflate_fixed`] copies these into the shared
//! `codes` buffer for fixed blocks, setting `lenbits = 9` / `distbits = 5`
//! accordingly; the main inflate state machine (`mod.rs`) and `inflateBack`
//! (`back.rs`) then decode fixed blocks directly from them.
//!
//! The entire module is pure `const` data: **zero `unsafe`**, no logic, and no
//! allocation. It is therefore trivially **`no_std`-clean** and compiles under
//! `--no-default-features --features no-std`.
//!
//! # Provenance
//!
//! These tables must never be edited by hand other than as a faithful
//! transcription of `inffixed.h`. They were transcribed verbatim, entry for
//! entry, from the C baseline and are continuously verified against the
//! canonical builder: [`crate::inflate::tables::inflate_table`] rebuilds them
//! from the fixed code-length pattern and asserts byte-identity (see the
//! cross-check tests in `tables.rs`).

use crate::inflate::tables::Code;

/// Fixed literal/length decode table (RFC 1951 §3.2.6), indexed with 9 root
/// bits (`512 == 1 << 9` entries).
///
/// Ported verbatim from `inffixed.h` `lenfix[512]`. Installed by
/// [`crate::inflate::tables::inflate_fixed`] at `codes[0..512]` to decode
/// fixed-Huffman blocks.
///
/// Each [`Code`] is `{ op, bits, val }`: `op == 0` marks a literal (`val` is the
/// output byte), `op & 0x10` marks a length base (`op & 0x0f` is the extra-bit
/// count and `val` the base length), `op == 96` (`0x60`) is end-of-block, and
/// `op == 64` (`0x40`) is an invalid-code marker (present at indices 99, 227,
/// 355 and 483).
#[rustfmt::skip]
pub const LENFIX: [Code; 512] = [
    Code::new(96, 7, 0), Code::new(0, 8, 80), Code::new(0, 8, 16), Code::new(20, 8, 115),
    Code::new(18, 7, 31), Code::new(0, 8, 112), Code::new(0, 8, 48), Code::new(0, 9, 192),
    Code::new(16, 7, 10), Code::new(0, 8, 96), Code::new(0, 8, 32), Code::new(0, 9, 160),
    Code::new(0, 8, 0), Code::new(0, 8, 128), Code::new(0, 8, 64), Code::new(0, 9, 224),
    Code::new(16, 7, 6), Code::new(0, 8, 88), Code::new(0, 8, 24), Code::new(0, 9, 144),
    Code::new(19, 7, 59), Code::new(0, 8, 120), Code::new(0, 8, 56), Code::new(0, 9, 208),
    Code::new(17, 7, 17), Code::new(0, 8, 104), Code::new(0, 8, 40), Code::new(0, 9, 176),
    Code::new(0, 8, 8), Code::new(0, 8, 136), Code::new(0, 8, 72), Code::new(0, 9, 240),
    Code::new(16, 7, 4), Code::new(0, 8, 84), Code::new(0, 8, 20), Code::new(21, 8, 227),
    Code::new(19, 7, 43), Code::new(0, 8, 116), Code::new(0, 8, 52), Code::new(0, 9, 200),
    Code::new(17, 7, 13), Code::new(0, 8, 100), Code::new(0, 8, 36), Code::new(0, 9, 168),
    Code::new(0, 8, 4), Code::new(0, 8, 132), Code::new(0, 8, 68), Code::new(0, 9, 232),
    Code::new(16, 7, 8), Code::new(0, 8, 92), Code::new(0, 8, 28), Code::new(0, 9, 152),
    Code::new(20, 7, 83), Code::new(0, 8, 124), Code::new(0, 8, 60), Code::new(0, 9, 216),
    Code::new(18, 7, 23), Code::new(0, 8, 108), Code::new(0, 8, 44), Code::new(0, 9, 184),
    Code::new(0, 8, 12), Code::new(0, 8, 140), Code::new(0, 8, 76), Code::new(0, 9, 248),
    Code::new(16, 7, 3), Code::new(0, 8, 82), Code::new(0, 8, 18), Code::new(21, 8, 163),
    Code::new(19, 7, 35), Code::new(0, 8, 114), Code::new(0, 8, 50), Code::new(0, 9, 196),
    Code::new(17, 7, 11), Code::new(0, 8, 98), Code::new(0, 8, 34), Code::new(0, 9, 164),
    Code::new(0, 8, 2), Code::new(0, 8, 130), Code::new(0, 8, 66), Code::new(0, 9, 228),
    Code::new(16, 7, 7), Code::new(0, 8, 90), Code::new(0, 8, 26), Code::new(0, 9, 148),
    Code::new(20, 7, 67), Code::new(0, 8, 122), Code::new(0, 8, 58), Code::new(0, 9, 212),
    Code::new(18, 7, 19), Code::new(0, 8, 106), Code::new(0, 8, 42), Code::new(0, 9, 180),
    Code::new(0, 8, 10), Code::new(0, 8, 138), Code::new(0, 8, 74), Code::new(0, 9, 244),
    Code::new(16, 7, 5), Code::new(0, 8, 86), Code::new(0, 8, 22), Code::new(64, 8, 0),
    Code::new(19, 7, 51), Code::new(0, 8, 118), Code::new(0, 8, 54), Code::new(0, 9, 204),
    Code::new(17, 7, 15), Code::new(0, 8, 102), Code::new(0, 8, 38), Code::new(0, 9, 172),
    Code::new(0, 8, 6), Code::new(0, 8, 134), Code::new(0, 8, 70), Code::new(0, 9, 236),
    Code::new(16, 7, 9), Code::new(0, 8, 94), Code::new(0, 8, 30), Code::new(0, 9, 156),
    Code::new(20, 7, 99), Code::new(0, 8, 126), Code::new(0, 8, 62), Code::new(0, 9, 220),
    Code::new(18, 7, 27), Code::new(0, 8, 110), Code::new(0, 8, 46), Code::new(0, 9, 188),
    Code::new(0, 8, 14), Code::new(0, 8, 142), Code::new(0, 8, 78), Code::new(0, 9, 252),
    Code::new(96, 7, 0), Code::new(0, 8, 81), Code::new(0, 8, 17), Code::new(21, 8, 131),
    Code::new(18, 7, 31), Code::new(0, 8, 113), Code::new(0, 8, 49), Code::new(0, 9, 194),
    Code::new(16, 7, 10), Code::new(0, 8, 97), Code::new(0, 8, 33), Code::new(0, 9, 162),
    Code::new(0, 8, 1), Code::new(0, 8, 129), Code::new(0, 8, 65), Code::new(0, 9, 226),
    Code::new(16, 7, 6), Code::new(0, 8, 89), Code::new(0, 8, 25), Code::new(0, 9, 146),
    Code::new(19, 7, 59), Code::new(0, 8, 121), Code::new(0, 8, 57), Code::new(0, 9, 210),
    Code::new(17, 7, 17), Code::new(0, 8, 105), Code::new(0, 8, 41), Code::new(0, 9, 178),
    Code::new(0, 8, 9), Code::new(0, 8, 137), Code::new(0, 8, 73), Code::new(0, 9, 242),
    Code::new(16, 7, 4), Code::new(0, 8, 85), Code::new(0, 8, 21), Code::new(16, 8, 258),
    Code::new(19, 7, 43), Code::new(0, 8, 117), Code::new(0, 8, 53), Code::new(0, 9, 202),
    Code::new(17, 7, 13), Code::new(0, 8, 101), Code::new(0, 8, 37), Code::new(0, 9, 170),
    Code::new(0, 8, 5), Code::new(0, 8, 133), Code::new(0, 8, 69), Code::new(0, 9, 234),
    Code::new(16, 7, 8), Code::new(0, 8, 93), Code::new(0, 8, 29), Code::new(0, 9, 154),
    Code::new(20, 7, 83), Code::new(0, 8, 125), Code::new(0, 8, 61), Code::new(0, 9, 218),
    Code::new(18, 7, 23), Code::new(0, 8, 109), Code::new(0, 8, 45), Code::new(0, 9, 186),
    Code::new(0, 8, 13), Code::new(0, 8, 141), Code::new(0, 8, 77), Code::new(0, 9, 250),
    Code::new(16, 7, 3), Code::new(0, 8, 83), Code::new(0, 8, 19), Code::new(21, 8, 195),
    Code::new(19, 7, 35), Code::new(0, 8, 115), Code::new(0, 8, 51), Code::new(0, 9, 198),
    Code::new(17, 7, 11), Code::new(0, 8, 99), Code::new(0, 8, 35), Code::new(0, 9, 166),
    Code::new(0, 8, 3), Code::new(0, 8, 131), Code::new(0, 8, 67), Code::new(0, 9, 230),
    Code::new(16, 7, 7), Code::new(0, 8, 91), Code::new(0, 8, 27), Code::new(0, 9, 150),
    Code::new(20, 7, 67), Code::new(0, 8, 123), Code::new(0, 8, 59), Code::new(0, 9, 214),
    Code::new(18, 7, 19), Code::new(0, 8, 107), Code::new(0, 8, 43), Code::new(0, 9, 182),
    Code::new(0, 8, 11), Code::new(0, 8, 139), Code::new(0, 8, 75), Code::new(0, 9, 246),
    Code::new(16, 7, 5), Code::new(0, 8, 87), Code::new(0, 8, 23), Code::new(64, 8, 0),
    Code::new(19, 7, 51), Code::new(0, 8, 119), Code::new(0, 8, 55), Code::new(0, 9, 206),
    Code::new(17, 7, 15), Code::new(0, 8, 103), Code::new(0, 8, 39), Code::new(0, 9, 174),
    Code::new(0, 8, 7), Code::new(0, 8, 135), Code::new(0, 8, 71), Code::new(0, 9, 238),
    Code::new(16, 7, 9), Code::new(0, 8, 95), Code::new(0, 8, 31), Code::new(0, 9, 158),
    Code::new(20, 7, 99), Code::new(0, 8, 127), Code::new(0, 8, 63), Code::new(0, 9, 222),
    Code::new(18, 7, 27), Code::new(0, 8, 111), Code::new(0, 8, 47), Code::new(0, 9, 190),
    Code::new(0, 8, 15), Code::new(0, 8, 143), Code::new(0, 8, 79), Code::new(0, 9, 254),
    Code::new(96, 7, 0), Code::new(0, 8, 80), Code::new(0, 8, 16), Code::new(20, 8, 115),
    Code::new(18, 7, 31), Code::new(0, 8, 112), Code::new(0, 8, 48), Code::new(0, 9, 193),
    Code::new(16, 7, 10), Code::new(0, 8, 96), Code::new(0, 8, 32), Code::new(0, 9, 161),
    Code::new(0, 8, 0), Code::new(0, 8, 128), Code::new(0, 8, 64), Code::new(0, 9, 225),
    Code::new(16, 7, 6), Code::new(0, 8, 88), Code::new(0, 8, 24), Code::new(0, 9, 145),
    Code::new(19, 7, 59), Code::new(0, 8, 120), Code::new(0, 8, 56), Code::new(0, 9, 209),
    Code::new(17, 7, 17), Code::new(0, 8, 104), Code::new(0, 8, 40), Code::new(0, 9, 177),
    Code::new(0, 8, 8), Code::new(0, 8, 136), Code::new(0, 8, 72), Code::new(0, 9, 241),
    Code::new(16, 7, 4), Code::new(0, 8, 84), Code::new(0, 8, 20), Code::new(21, 8, 227),
    Code::new(19, 7, 43), Code::new(0, 8, 116), Code::new(0, 8, 52), Code::new(0, 9, 201),
    Code::new(17, 7, 13), Code::new(0, 8, 100), Code::new(0, 8, 36), Code::new(0, 9, 169),
    Code::new(0, 8, 4), Code::new(0, 8, 132), Code::new(0, 8, 68), Code::new(0, 9, 233),
    Code::new(16, 7, 8), Code::new(0, 8, 92), Code::new(0, 8, 28), Code::new(0, 9, 153),
    Code::new(20, 7, 83), Code::new(0, 8, 124), Code::new(0, 8, 60), Code::new(0, 9, 217),
    Code::new(18, 7, 23), Code::new(0, 8, 108), Code::new(0, 8, 44), Code::new(0, 9, 185),
    Code::new(0, 8, 12), Code::new(0, 8, 140), Code::new(0, 8, 76), Code::new(0, 9, 249),
    Code::new(16, 7, 3), Code::new(0, 8, 82), Code::new(0, 8, 18), Code::new(21, 8, 163),
    Code::new(19, 7, 35), Code::new(0, 8, 114), Code::new(0, 8, 50), Code::new(0, 9, 197),
    Code::new(17, 7, 11), Code::new(0, 8, 98), Code::new(0, 8, 34), Code::new(0, 9, 165),
    Code::new(0, 8, 2), Code::new(0, 8, 130), Code::new(0, 8, 66), Code::new(0, 9, 229),
    Code::new(16, 7, 7), Code::new(0, 8, 90), Code::new(0, 8, 26), Code::new(0, 9, 149),
    Code::new(20, 7, 67), Code::new(0, 8, 122), Code::new(0, 8, 58), Code::new(0, 9, 213),
    Code::new(18, 7, 19), Code::new(0, 8, 106), Code::new(0, 8, 42), Code::new(0, 9, 181),
    Code::new(0, 8, 10), Code::new(0, 8, 138), Code::new(0, 8, 74), Code::new(0, 9, 245),
    Code::new(16, 7, 5), Code::new(0, 8, 86), Code::new(0, 8, 22), Code::new(64, 8, 0),
    Code::new(19, 7, 51), Code::new(0, 8, 118), Code::new(0, 8, 54), Code::new(0, 9, 205),
    Code::new(17, 7, 15), Code::new(0, 8, 102), Code::new(0, 8, 38), Code::new(0, 9, 173),
    Code::new(0, 8, 6), Code::new(0, 8, 134), Code::new(0, 8, 70), Code::new(0, 9, 237),
    Code::new(16, 7, 9), Code::new(0, 8, 94), Code::new(0, 8, 30), Code::new(0, 9, 157),
    Code::new(20, 7, 99), Code::new(0, 8, 126), Code::new(0, 8, 62), Code::new(0, 9, 221),
    Code::new(18, 7, 27), Code::new(0, 8, 110), Code::new(0, 8, 46), Code::new(0, 9, 189),
    Code::new(0, 8, 14), Code::new(0, 8, 142), Code::new(0, 8, 78), Code::new(0, 9, 253),
    Code::new(96, 7, 0), Code::new(0, 8, 81), Code::new(0, 8, 17), Code::new(21, 8, 131),
    Code::new(18, 7, 31), Code::new(0, 8, 113), Code::new(0, 8, 49), Code::new(0, 9, 195),
    Code::new(16, 7, 10), Code::new(0, 8, 97), Code::new(0, 8, 33), Code::new(0, 9, 163),
    Code::new(0, 8, 1), Code::new(0, 8, 129), Code::new(0, 8, 65), Code::new(0, 9, 227),
    Code::new(16, 7, 6), Code::new(0, 8, 89), Code::new(0, 8, 25), Code::new(0, 9, 147),
    Code::new(19, 7, 59), Code::new(0, 8, 121), Code::new(0, 8, 57), Code::new(0, 9, 211),
    Code::new(17, 7, 17), Code::new(0, 8, 105), Code::new(0, 8, 41), Code::new(0, 9, 179),
    Code::new(0, 8, 9), Code::new(0, 8, 137), Code::new(0, 8, 73), Code::new(0, 9, 243),
    Code::new(16, 7, 4), Code::new(0, 8, 85), Code::new(0, 8, 21), Code::new(16, 8, 258),
    Code::new(19, 7, 43), Code::new(0, 8, 117), Code::new(0, 8, 53), Code::new(0, 9, 203),
    Code::new(17, 7, 13), Code::new(0, 8, 101), Code::new(0, 8, 37), Code::new(0, 9, 171),
    Code::new(0, 8, 5), Code::new(0, 8, 133), Code::new(0, 8, 69), Code::new(0, 9, 235),
    Code::new(16, 7, 8), Code::new(0, 8, 93), Code::new(0, 8, 29), Code::new(0, 9, 155),
    Code::new(20, 7, 83), Code::new(0, 8, 125), Code::new(0, 8, 61), Code::new(0, 9, 219),
    Code::new(18, 7, 23), Code::new(0, 8, 109), Code::new(0, 8, 45), Code::new(0, 9, 187),
    Code::new(0, 8, 13), Code::new(0, 8, 141), Code::new(0, 8, 77), Code::new(0, 9, 251),
    Code::new(16, 7, 3), Code::new(0, 8, 83), Code::new(0, 8, 19), Code::new(21, 8, 195),
    Code::new(19, 7, 35), Code::new(0, 8, 115), Code::new(0, 8, 51), Code::new(0, 9, 199),
    Code::new(17, 7, 11), Code::new(0, 8, 99), Code::new(0, 8, 35), Code::new(0, 9, 167),
    Code::new(0, 8, 3), Code::new(0, 8, 131), Code::new(0, 8, 67), Code::new(0, 9, 231),
    Code::new(16, 7, 7), Code::new(0, 8, 91), Code::new(0, 8, 27), Code::new(0, 9, 151),
    Code::new(20, 7, 67), Code::new(0, 8, 123), Code::new(0, 8, 59), Code::new(0, 9, 215),
    Code::new(18, 7, 19), Code::new(0, 8, 107), Code::new(0, 8, 43), Code::new(0, 9, 183),
    Code::new(0, 8, 11), Code::new(0, 8, 139), Code::new(0, 8, 75), Code::new(0, 9, 247),
    Code::new(16, 7, 5), Code::new(0, 8, 87), Code::new(0, 8, 23), Code::new(64, 8, 0),
    Code::new(19, 7, 51), Code::new(0, 8, 119), Code::new(0, 8, 55), Code::new(0, 9, 207),
    Code::new(17, 7, 15), Code::new(0, 8, 103), Code::new(0, 8, 39), Code::new(0, 9, 175),
    Code::new(0, 8, 7), Code::new(0, 8, 135), Code::new(0, 8, 71), Code::new(0, 9, 239),
    Code::new(16, 7, 9), Code::new(0, 8, 95), Code::new(0, 8, 31), Code::new(0, 9, 159),
    Code::new(20, 7, 99), Code::new(0, 8, 127), Code::new(0, 8, 63), Code::new(0, 9, 223),
    Code::new(18, 7, 27), Code::new(0, 8, 111), Code::new(0, 8, 47), Code::new(0, 9, 191),
    Code::new(0, 8, 15), Code::new(0, 8, 143), Code::new(0, 8, 79), Code::new(0, 9, 255),
];

/// Fixed distance decode table (RFC 1951 §3.2.6), indexed with 5 root bits
/// (`32 == 1 << 5` entries).
///
/// Ported verbatim from `inffixed.h` `distfix[32]`. Installed by
/// [`crate::inflate::tables::inflate_fixed`] at `codes[512..544]`.
///
/// Each entry's `op & 0x10` marks a distance base (`op & 0x0f` is the extra-bit
/// count and `val` the base distance); `op == 64` (`0x40`) marks the two
/// invalid distance codes (symbols 30 and 31) at indices 15 and 31.
#[rustfmt::skip]
pub const DISTFIX: [Code; 32] = [
    Code::new(16, 5, 1), Code::new(23, 5, 257), Code::new(19, 5, 17), Code::new(27, 5, 4097),
    Code::new(17, 5, 5), Code::new(25, 5, 1025), Code::new(21, 5, 65), Code::new(29, 5, 16385),
    Code::new(16, 5, 3), Code::new(24, 5, 513), Code::new(20, 5, 33), Code::new(28, 5, 8193),
    Code::new(18, 5, 9), Code::new(26, 5, 2049), Code::new(22, 5, 129), Code::new(64, 5, 0),
    Code::new(16, 5, 2), Code::new(23, 5, 385), Code::new(19, 5, 25), Code::new(27, 5, 6145),
    Code::new(17, 5, 7), Code::new(25, 5, 1537), Code::new(21, 5, 97), Code::new(29, 5, 24577),
    Code::new(16, 5, 4), Code::new(24, 5, 769), Code::new(20, 5, 49), Code::new(28, 5, 12289),
    Code::new(18, 5, 13), Code::new(26, 5, 3073), Code::new(22, 5, 193), Code::new(64, 5, 0),
];

#[cfg(test)]
mod tests {
    use super::{DISTFIX, LENFIX};
    use crate::inflate::tables::Code;

    /// The table sizes are fixed by RFC 1951: the literal/length root table is
    /// indexed with 9 bits (`1 << 9 == 512`) and the distance root table with 5
    /// bits (`1 << 5 == 32`).
    #[test]
    fn table_lengths_are_canonical() {
        assert_eq!(LENFIX.len(), 512);
        assert_eq!(DISTFIX.len(), 32);
    }

    /// The first seven and the final entry of `LENFIX`, verbatim from
    /// `inffixed.h` `lenfix[512]`.
    #[test]
    fn lenfix_boundary_anchors_match_inffixed_h() {
        assert_eq!(LENFIX[0], Code::new(96, 7, 0));
        assert_eq!(LENFIX[1], Code::new(0, 8, 80));
        assert_eq!(LENFIX[2], Code::new(0, 8, 16));
        assert_eq!(LENFIX[3], Code::new(20, 8, 115));
        assert_eq!(LENFIX[4], Code::new(18, 7, 31));
        assert_eq!(LENFIX[5], Code::new(0, 8, 112));
        assert_eq!(LENFIX[6], Code::new(0, 8, 48));
        assert_eq!(LENFIX[511], Code::new(0, 9, 255));
    }

    /// zlib's `makefixed()` emits the invalid-code marker `{64,8,0}` at exactly
    /// these four slots (the indices `i` where `(i & 127) == 99`).
    #[test]
    fn lenfix_invalid_code_markers_at_canonical_indices() {
        for &i in &[99_usize, 227, 355, 483] {
            assert_eq!(LENFIX[i], Code::new(64, 8, 0), "slot {i}");
        }
    }

    /// The maximum match length (258) is encoded as `{16,8,258}`. In the
    /// canonical `inffixed.h` table it appears exactly twice, at indices 163 and
    /// 419. (The AAP prose cited "e.g. 169/425" only as an approximation; the
    /// verbatim positions in `inffixed.h` are 163 and 419.)
    #[test]
    fn lenfix_encodes_max_length_258_exactly_twice() {
        let count = LENFIX
            .iter()
            .filter(|&&c| c == Code::new(16, 8, 258))
            .count();
        assert_eq!(count, 2);
        assert_eq!(LENFIX[163], Code::new(16, 8, 258));
        assert_eq!(LENFIX[419], Code::new(16, 8, 258));
    }

    /// Boundary and invalid-marker anchors of `DISTFIX`, verbatim from
    /// `inffixed.h` `distfix[32]`. The two invalid distance codes (symbols 30
    /// and 31) are the `{64,5,0}` markers at indices 15 and 31.
    #[test]
    fn distfix_anchors_match_inffixed_h() {
        assert_eq!(DISTFIX[0], Code::new(16, 5, 1));
        assert_eq!(DISTFIX[1], Code::new(23, 5, 257));
        assert_eq!(DISTFIX[15], Code::new(64, 5, 0));
        assert_eq!(DISTFIX[31], Code::new(64, 5, 0));
    }
}
