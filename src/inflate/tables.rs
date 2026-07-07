//! Huffman decode-table builder (`inflate_table`) and the [`Code`] decode-table
//! entry — a safe Rust port of `inftrees.c` / `inftrees.h`.
//!
//! This module is the foundation of the inflate decoder. It defines the
//! [`Code`] table-entry layout, the decode-table sizing constants
//! ([`ENOUGH`], [`ENOUGH_LENS`], [`ENOUGH_DISTS`], [`MAXBITS`]), the
//! [`CodeType`] selector, and [`inflate_table`], which builds a set of
//! canonical Huffman decoding tables from a list of code lengths.
//!
//! The output produced here — the exact byte layout of every [`Code`] entry
//! and the precise number of entries consumed — must match the reference C
//! implementation bit-for-bit, because the inflate fast/slow decode loops
//! index directly into the tables built by this routine. See the zlib DEFLATE
//! format references (RFC 1951) for the canonical-Huffman semantics reproduced
//! below.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`**. `inflate_table` is expressed purely
//! as index arithmetic over caller-provided slices, so it is entirely safe
//! Rust. It also does not use `std`; it depends only on `core` and is
//! `no_std`-compatible.

/// A single decode-table entry.
///
/// Each entry provides either the information needed to perform the operation
/// requested by the code that indexed the entry, or a pointer (offset) to
/// another table that decodes more bits of the code. `op` indicates whether
/// the entry is a link to another table, a literal, a length or distance, an
/// end-of-block, or an invalid code.
///
/// * For a **table link**, the low four bits of `op` are the number of index
///   bits of the sub-table.
/// * For a **length or distance**, the low four bits of `op` are the number of
///   extra bits to read after the code.
/// * `bits` is the number of bits in this code, or the part of the code to
///   drop off the bit buffer.
/// * `val` is the literal byte to output, the base length or distance, or the
///   offset from the current table to the next table.
///
/// The `op` field is a bit-encoded operation, as set by [`inflate_table`]:
///
/// ```text
/// 00000000 - literal
/// 0000tttt - table link, tttt != 0 is the number of table index bits
/// 0001eeee - length or distance, eeee is the number of extra bits
/// 01100000 - end of block  (96 = 32 | 64)
/// 01000000 - invalid code  (64)
/// ```
///
/// Each entry is exactly four bytes. The `#[repr(C)]` layout
/// `{ op: u8, bits: u8, val: u16 }` is required so that the FFI layer and the
/// statically generated fixed tables (`inffixed.h` → `fixed.rs`) share an
/// identical memory representation.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Code {
    /// operation, extra bits, or number of table index bits
    pub op: u8,
    /// bits in this part of the code
    pub bits: u8,
    /// offset in table, or the literal/length/distance base value
    pub val: u16,
}

/// Maximum bit length of any code (the DEFLATE hard limit).
pub const MAXBITS: usize = 15;

/// Alias for [`MAXBITS`] using the name referenced by the design spec.
///
/// [`MAXBITS`] (the original C name) remains canonical; this is provided only
/// as a convenience alias and carries the identical value.
pub const MAX_BITS: usize = MAXBITS;

/// Maximum number of code entries for a literal/length decode table.
///
/// Found by exhaustive search (`enough 286 9 15`) as documented in
/// `inftrees.h`.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of code entries for a distance decode table.
///
/// Found by exhaustive search (`enough 30 6 15`) as documented in
/// `inftrees.h`.
pub const ENOUGH_DISTS: usize = 592;

/// Maximum total number of decode-table entries: `ENOUGH_LENS + ENOUGH_DISTS`.
///
/// This equals **1444** (852 + 592). Decode-table arenas (e.g.
/// `InflateState.codes: [Code; ENOUGH]`) must be sized to this value to match
/// the C memory footprint and the table-overflow checks in [`inflate_table`].
///
/// NOTE (blueprint correction): `doc/technical-specifications.md` §0.5.1
/// erroneously states `ENOUGH = 2048`. The verified `inftrees.h` source value
/// is **1444**; that is what is used here so the overflow guards and memory
/// layout match reference zlib exactly.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

/// The type of code to build for [`inflate_table`].
///
/// This selects which base/extra tables and the `match` threshold the builder
/// uses when decoding symbol values. Mirrors the C `codetype` enum
/// (`CODES`, `LENS`, `DISTS`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeType {
    /// Code-length codes (the 19-symbol alphabet used to encode the dynamic
    /// literal/length and distance code lengths).
    Codes,
    /// Literal/length codes.
    Lens,
    /// Distance codes.
    Dists,
}

/// Error returned by [`inflate_table`].
///
/// These correspond to the two non-zero integer return values of the C
/// `inflate_table` routine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InflateTableError {
    /// The set of code lengths is over-subscribed or incomplete (C returns
    /// `-1`).
    Invalid,
    /// The decode tables would require more than `ENOUGH` entries (C returns
    /// `+1`).
    Enough,
}

// The following four tables map length/distance symbols to their base values
// and number of extra bits. They are transcribed verbatim from `inftrees.c`
// (the `lbase`/`lext`/`dbase`/`dext` static arrays). A single wrong entry
// would break wire-format compatibility, so they must never be altered.

/// Base values for length codes 257..285 (`lbase` in C).
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// Extra-bit counts for length codes 257..285 (`lext` in C).
///
/// The trailing `16, 68, 193` values are deliberate: `68` and `193` carry the
/// invalid-code marker bit (`64`) for the two unused symbols (286, 287) of the
/// fixed literal/length code.
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Base values for distance codes 0..29 (`dbase` in C).
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Extra-bit counts for distance codes 0..29 (`dext` in C).
///
/// The trailing `64, 64` values carry the invalid-code marker bit for the two
/// unused distance symbols (30, 31).
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

/// Build a set of Huffman decode tables from a canonical Huffman code.
///
/// The code lengths are `lens[0..codes]`; each length corresponds to symbol
/// `0..codes`. A length of `0` means the symbol does not occur in the code; a
/// length of `1..=MAXBITS` is that code's length. This routine assumes (but
/// does not check) that every entry in `lens` is in `0..=MAXBITS`; the caller
/// must guarantee that.
///
/// The resulting tables are written into `table` (the decode-table arena)
/// starting at offset `*table_index`. On return, `*table_index` is advanced by
/// the number of entries used, so a subsequent call appends after them. `work`
/// is a scratch area of at least `codes` `u16`s used to sort the symbols.
///
/// `bits` is the requested root-table index bits on entry; on success it is
/// overwritten with the actual root-table index bits (which differs from the
/// request when it exceeds the longest code or is below the shortest code).
///
/// # Returns
///
/// * `Ok(())` on success (the actual root index bits are written back through
///   `bits`).
/// * `Err(InflateTableError::Invalid)` for an over-subscribed or incomplete
///   set of code lengths (the C routine returns `-1`).
/// * `Err(InflateTableError::Enough)` when the tables would exceed the
///   `ENOUGH_LENS` / `ENOUGH_DISTS` space (the C routine returns `+1`).
///
/// # Panics
///
/// Panics (via slice bounds checks) if `table` is too small for the code being
/// built or if `work`/`lens` are shorter than `codes`. Callers size `table` as
/// `[Code; ENOUGH]` for `Lens`/`Dists` codes; the `Enough` error is the guard
/// that keeps a valid code within that arena.
pub fn inflate_table(
    code_type: CodeType,
    lens: &[u16],
    codes: usize,
    table: &mut [Code],
    table_index: &mut usize,
    bits: &mut usize,
    work: &mut [u16],
) -> Result<(), InflateTableError> {
    // `count[len]` = number of codes of each length; `offs[len]` = offset into
    // the sorted symbol table for each length. Indices 0..=MAXBITS are used.
    let mut count = [0u16; MAXBITS + 1];
    let mut offs = [0u16; MAXBITS + 1];

    // Accumulate lengths for codes (assumes lens[] are all in 0..=MAXBITS).
    // (inftrees.c L116-L119)
    for &len_val in lens.iter().take(codes) {
        count[len_val as usize] += 1;
    }

    // Bound code lengths, forcing root to lie within the code lengths.
    // (inftrees.c L122-L137)
    let mut root = *bits;

    // Find the maximum code length (`max`), or 0 if there are no codes at all.
    let mut max = MAXBITS;
    while max >= 1 && count[max] == 0 {
        max -= 1;
    }
    if root > max {
        root = max;
    }

    // No symbols to code at all: build a table containing a single invalid
    // code so that decoding reports an error, and return. (inftrees.c L126-L134)
    if max == 0 {
        let invalid = Code {
            op: 64, // invalid code marker
            bits: 1,
            val: 0,
        };
        let base = *table_index;
        table[base] = invalid;
        table[base + 1] = invalid;
        *table_index = base + 2;
        *bits = 1;
        return Ok(()); // no symbols, but wait for decoding to report the error
    }

    // Find the minimum code length (`min`) and raise `root` to it if needed.
    let mut min = 1;
    while min < max && count[min] == 0 {
        min += 1;
    }
    if root < min {
        root = min;
    }

    // Check for an over-subscribed or incomplete set of lengths.
    // (inftrees.c L139-L147)
    let mut left: i32 = 1;
    for &c in &count[1..=MAXBITS] {
        left <<= 1;
        left -= c as i32;
        if left < 0 {
            return Err(InflateTableError::Invalid); // over-subscribed
        }
    }
    if left > 0 && (code_type == CodeType::Codes || max != 1) {
        return Err(InflateTableError::Invalid); // incomplete set
    }

    // Generate offsets into the symbol table for each length, for sorting.
    // (inftrees.c L149-L152)
    offs[1] = 0;
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }

    // Sort symbols by length, and by symbol order within each length.
    // (inftrees.c L154-L156)
    for (sym, &len_val) in lens.iter().enumerate().take(codes) {
        if len_val != 0 {
            let l = len_val as usize;
            work[offs[l] as usize] = sym as u16;
            offs[l] += 1;
        }
    }

    // Set up the base value / extra bits tables and the `match` threshold for
    // the requested code type. For `Codes` there is no base/extra table; the
    // relevant branch of the entry-building code below is never reached for the
    // 19-symbol code-length alphabet, so empty slices are safe placeholders.
    // (inftrees.c L189-L202)
    let (base, extra, match_val): (&[u16], &[u16], usize) = match code_type {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // Initialize state for the fill loop. (inftrees.c L204-L213)
    //
    // `base_index` is the C `*table` base pointer expressed as an index into
    // `table`; `next` is the C `next` pointer (start of the table currently
    // being filled), also as an absolute index into `table`.
    let base_index = *table_index;
    let mut huff: usize = 0; // starting code
    let mut sym: usize = 0; // starting code symbol
    let mut len: usize = min; // starting code length
    let mut next: usize = base_index; // current table to fill in
    let mut curr: usize = root; // current table index bits
    let mut drop: usize = 0; // current bits to drop from code for index
    let mut low: usize = usize::MAX; // trigger a new sub-table when len > root
    let mut used: usize = 1usize << root; // number of root table entries in use
    let mask: usize = used - 1; // mask for comparing low bits

    // Check available table space for the root table. (inftrees.c L215-L218)
    if (code_type == CodeType::Lens && used > ENOUGH_LENS)
        || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
    {
        return Err(InflateTableError::Enough);
    }

    // Process all codes and make table entries. (inftrees.c L220-L295)
    loop {
        // Create the table entry `here` for the current symbol.
        // (inftrees.c L222-L235)
        let bits_field = (len - drop) as u8;
        let work_sym = work[sym] as usize;
        let here = if work_sym + 1 < match_val {
            // literal
            Code {
                op: 0,
                bits: bits_field,
                val: work[sym],
            }
        } else if work_sym >= match_val {
            // length or distance
            Code {
                op: extra[work_sym - match_val] as u8,
                bits: bits_field,
                val: base[work_sym - match_val],
            }
        } else {
            // end of block (32 | 64)
            Code {
                op: 96,
                bits: bits_field,
                val: 0,
            }
        };

        // Replicate the entry for all indices whose low `len` bits equal `huff`.
        // (inftrees.c L237-L244)
        let fill_incr = 1usize << (len - drop);
        let mut fill = 1usize << curr;
        // Save `1 << curr` (the size of the current table); the C code reuses
        // its `min` variable for this and later advances `next` by it.
        let next_table_size = fill;
        loop {
            fill -= fill_incr;
            table[next + ((huff >> drop) + fill)] = here;
            if fill == 0 {
                break;
            }
        }

        // Backwards-increment the `len`-bit code `huff`. (inftrees.c L246-L255)
        let mut huff_incr = 1usize << (len - 1);
        while huff & huff_incr != 0 {
            huff_incr >>= 1;
        }
        if huff_incr != 0 {
            huff &= huff_incr - 1;
            huff += huff_incr;
        } else {
            huff = 0;
        }

        // Advance to the next symbol; update the count and current length.
        // (inftrees.c L257-L262)
        sym += 1;
        count[len] -= 1;
        if count[len] == 0 {
            if len == max {
                break;
            }
            len = lens[work[sym] as usize] as usize;
        }

        // Create a new sub-table if needed. (inftrees.c L264-L294)
        if len > root && (huff & mask) != low {
            // On the first sub-table, transition drop from 0 to root.
            if drop == 0 {
                drop = root;
            }

            // Advance past the table just filled (C `next += min`, where here
            // `min` held `1 << curr`).
            next += next_table_size;

            // Determine the length of the next (sub-)table by looking ahead at
            // the remaining length counts. (inftrees.c L273-L281)
            curr = len - drop;
            let mut left2: i32 = 1i32 << curr;
            while curr + drop < max {
                left2 -= count[curr + drop] as i32;
                if left2 <= 0 {
                    break;
                }
                curr += 1;
                left2 <<= 1;
            }

            // Check for enough space. (inftrees.c L283-L287)
            used += 1usize << curr;
            if (code_type == CodeType::Lens && used > ENOUGH_LENS)
                || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
            {
                return Err(InflateTableError::Enough);
            }

            // Point the entry in the root table to this sub-table.
            // (inftrees.c L289-L293)
            low = huff & mask;
            table[base_index + low] = Code {
                op: curr as u8,
                bits: root as u8,
                val: (next - base_index) as u16,
            };
        }
    }

    // Fill in a remaining table entry if the code is incomplete. There is
    // guaranteed to be at most one such entry, because an incomplete code is
    // only permitted when the maximum code length is a single bit — in which
    // case `drop` is still 0. (inftrees.c L297-L305)
    if huff != 0 {
        table[next + (huff >> drop)] = Code {
            op: 64, // invalid code marker
            bits: (len - drop) as u8,
            val: 0,
        };
    }

    // Set the return parameters. (inftrees.c L307-L310)
    *table_index = base_index + used;
    *bits = root;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference fixed literal/length decode table, transcribed from the C
    // `inffixed.h` (`lenfix[512]`). NOTE: the C `MAKEFIXED` generator forces
    // `op = 64` at every index where `(index & 127) == 99` (indices 99, 227,
    // 355, 483 — the entries for the fixed code's unused symbols 286/287).
    // The raw `inflate_table` output at those indices instead carries the
    // extra-bit values 68 (`lext[29]`) or 193 (`lext[30]`), which also have the
    // invalid-code marker bit (64) set. The cross-check below accounts for
    // that difference.
    #[rustfmt::skip]
    const LENFIX_REF: [(u8, u8, u16); 512] = [
        (96, 7, 0), (0, 8, 80), (0, 8, 16), (20, 8, 115), (18, 7, 31), (0, 8, 112), (0, 8, 48), (0, 9, 192),
        (16, 7, 10), (0, 8, 96), (0, 8, 32), (0, 9, 160), (0, 8, 0), (0, 8, 128), (0, 8, 64), (0, 9, 224),
        (16, 7, 6), (0, 8, 88), (0, 8, 24), (0, 9, 144), (19, 7, 59), (0, 8, 120), (0, 8, 56), (0, 9, 208),
        (17, 7, 17), (0, 8, 104), (0, 8, 40), (0, 9, 176), (0, 8, 8), (0, 8, 136), (0, 8, 72), (0, 9, 240),
        (16, 7, 4), (0, 8, 84), (0, 8, 20), (21, 8, 227), (19, 7, 43), (0, 8, 116), (0, 8, 52), (0, 9, 200),
        (17, 7, 13), (0, 8, 100), (0, 8, 36), (0, 9, 168), (0, 8, 4), (0, 8, 132), (0, 8, 68), (0, 9, 232),
        (16, 7, 8), (0, 8, 92), (0, 8, 28), (0, 9, 152), (20, 7, 83), (0, 8, 124), (0, 8, 60), (0, 9, 216),
        (18, 7, 23), (0, 8, 108), (0, 8, 44), (0, 9, 184), (0, 8, 12), (0, 8, 140), (0, 8, 76), (0, 9, 248),
        (16, 7, 3), (0, 8, 82), (0, 8, 18), (21, 8, 163), (19, 7, 35), (0, 8, 114), (0, 8, 50), (0, 9, 196),
        (17, 7, 11), (0, 8, 98), (0, 8, 34), (0, 9, 164), (0, 8, 2), (0, 8, 130), (0, 8, 66), (0, 9, 228),
        (16, 7, 7), (0, 8, 90), (0, 8, 26), (0, 9, 148), (20, 7, 67), (0, 8, 122), (0, 8, 58), (0, 9, 212),
        (18, 7, 19), (0, 8, 106), (0, 8, 42), (0, 9, 180), (0, 8, 10), (0, 8, 138), (0, 8, 74), (0, 9, 244),
        (16, 7, 5), (0, 8, 86), (0, 8, 22), (64, 8, 0), (19, 7, 51), (0, 8, 118), (0, 8, 54), (0, 9, 204),
        (17, 7, 15), (0, 8, 102), (0, 8, 38), (0, 9, 172), (0, 8, 6), (0, 8, 134), (0, 8, 70), (0, 9, 236),
        (16, 7, 9), (0, 8, 94), (0, 8, 30), (0, 9, 156), (20, 7, 99), (0, 8, 126), (0, 8, 62), (0, 9, 220),
        (18, 7, 27), (0, 8, 110), (0, 8, 46), (0, 9, 188), (0, 8, 14), (0, 8, 142), (0, 8, 78), (0, 9, 252),
        (96, 7, 0), (0, 8, 81), (0, 8, 17), (21, 8, 131), (18, 7, 31), (0, 8, 113), (0, 8, 49), (0, 9, 194),
        (16, 7, 10), (0, 8, 97), (0, 8, 33), (0, 9, 162), (0, 8, 1), (0, 8, 129), (0, 8, 65), (0, 9, 226),
        (16, 7, 6), (0, 8, 89), (0, 8, 25), (0, 9, 146), (19, 7, 59), (0, 8, 121), (0, 8, 57), (0, 9, 210),
        (17, 7, 17), (0, 8, 105), (0, 8, 41), (0, 9, 178), (0, 8, 9), (0, 8, 137), (0, 8, 73), (0, 9, 242),
        (16, 7, 4), (0, 8, 85), (0, 8, 21), (16, 8, 258), (19, 7, 43), (0, 8, 117), (0, 8, 53), (0, 9, 202),
        (17, 7, 13), (0, 8, 101), (0, 8, 37), (0, 9, 170), (0, 8, 5), (0, 8, 133), (0, 8, 69), (0, 9, 234),
        (16, 7, 8), (0, 8, 93), (0, 8, 29), (0, 9, 154), (20, 7, 83), (0, 8, 125), (0, 8, 61), (0, 9, 218),
        (18, 7, 23), (0, 8, 109), (0, 8, 45), (0, 9, 186), (0, 8, 13), (0, 8, 141), (0, 8, 77), (0, 9, 250),
        (16, 7, 3), (0, 8, 83), (0, 8, 19), (21, 8, 195), (19, 7, 35), (0, 8, 115), (0, 8, 51), (0, 9, 198),
        (17, 7, 11), (0, 8, 99), (0, 8, 35), (0, 9, 166), (0, 8, 3), (0, 8, 131), (0, 8, 67), (0, 9, 230),
        (16, 7, 7), (0, 8, 91), (0, 8, 27), (0, 9, 150), (20, 7, 67), (0, 8, 123), (0, 8, 59), (0, 9, 214),
        (18, 7, 19), (0, 8, 107), (0, 8, 43), (0, 9, 182), (0, 8, 11), (0, 8, 139), (0, 8, 75), (0, 9, 246),
        (16, 7, 5), (0, 8, 87), (0, 8, 23), (64, 8, 0), (19, 7, 51), (0, 8, 119), (0, 8, 55), (0, 9, 206),
        (17, 7, 15), (0, 8, 103), (0, 8, 39), (0, 9, 174), (0, 8, 7), (0, 8, 135), (0, 8, 71), (0, 9, 238),
        (16, 7, 9), (0, 8, 95), (0, 8, 31), (0, 9, 158), (20, 7, 99), (0, 8, 127), (0, 8, 63), (0, 9, 222),
        (18, 7, 27), (0, 8, 111), (0, 8, 47), (0, 9, 190), (0, 8, 15), (0, 8, 143), (0, 8, 79), (0, 9, 254),
        (96, 7, 0), (0, 8, 80), (0, 8, 16), (20, 8, 115), (18, 7, 31), (0, 8, 112), (0, 8, 48), (0, 9, 193),
        (16, 7, 10), (0, 8, 96), (0, 8, 32), (0, 9, 161), (0, 8, 0), (0, 8, 128), (0, 8, 64), (0, 9, 225),
        (16, 7, 6), (0, 8, 88), (0, 8, 24), (0, 9, 145), (19, 7, 59), (0, 8, 120), (0, 8, 56), (0, 9, 209),
        (17, 7, 17), (0, 8, 104), (0, 8, 40), (0, 9, 177), (0, 8, 8), (0, 8, 136), (0, 8, 72), (0, 9, 241),
        (16, 7, 4), (0, 8, 84), (0, 8, 20), (21, 8, 227), (19, 7, 43), (0, 8, 116), (0, 8, 52), (0, 9, 201),
        (17, 7, 13), (0, 8, 100), (0, 8, 36), (0, 9, 169), (0, 8, 4), (0, 8, 132), (0, 8, 68), (0, 9, 233),
        (16, 7, 8), (0, 8, 92), (0, 8, 28), (0, 9, 153), (20, 7, 83), (0, 8, 124), (0, 8, 60), (0, 9, 217),
        (18, 7, 23), (0, 8, 108), (0, 8, 44), (0, 9, 185), (0, 8, 12), (0, 8, 140), (0, 8, 76), (0, 9, 249),
        (16, 7, 3), (0, 8, 82), (0, 8, 18), (21, 8, 163), (19, 7, 35), (0, 8, 114), (0, 8, 50), (0, 9, 197),
        (17, 7, 11), (0, 8, 98), (0, 8, 34), (0, 9, 165), (0, 8, 2), (0, 8, 130), (0, 8, 66), (0, 9, 229),
        (16, 7, 7), (0, 8, 90), (0, 8, 26), (0, 9, 149), (20, 7, 67), (0, 8, 122), (0, 8, 58), (0, 9, 213),
        (18, 7, 19), (0, 8, 106), (0, 8, 42), (0, 9, 181), (0, 8, 10), (0, 8, 138), (0, 8, 74), (0, 9, 245),
        (16, 7, 5), (0, 8, 86), (0, 8, 22), (64, 8, 0), (19, 7, 51), (0, 8, 118), (0, 8, 54), (0, 9, 205),
        (17, 7, 15), (0, 8, 102), (0, 8, 38), (0, 9, 173), (0, 8, 6), (0, 8, 134), (0, 8, 70), (0, 9, 237),
        (16, 7, 9), (0, 8, 94), (0, 8, 30), (0, 9, 157), (20, 7, 99), (0, 8, 126), (0, 8, 62), (0, 9, 221),
        (18, 7, 27), (0, 8, 110), (0, 8, 46), (0, 9, 189), (0, 8, 14), (0, 8, 142), (0, 8, 78), (0, 9, 253),
        (96, 7, 0), (0, 8, 81), (0, 8, 17), (21, 8, 131), (18, 7, 31), (0, 8, 113), (0, 8, 49), (0, 9, 195),
        (16, 7, 10), (0, 8, 97), (0, 8, 33), (0, 9, 163), (0, 8, 1), (0, 8, 129), (0, 8, 65), (0, 9, 227),
        (16, 7, 6), (0, 8, 89), (0, 8, 25), (0, 9, 147), (19, 7, 59), (0, 8, 121), (0, 8, 57), (0, 9, 211),
        (17, 7, 17), (0, 8, 105), (0, 8, 41), (0, 9, 179), (0, 8, 9), (0, 8, 137), (0, 8, 73), (0, 9, 243),
        (16, 7, 4), (0, 8, 85), (0, 8, 21), (16, 8, 258), (19, 7, 43), (0, 8, 117), (0, 8, 53), (0, 9, 203),
        (17, 7, 13), (0, 8, 101), (0, 8, 37), (0, 9, 171), (0, 8, 5), (0, 8, 133), (0, 8, 69), (0, 9, 235),
        (16, 7, 8), (0, 8, 93), (0, 8, 29), (0, 9, 155), (20, 7, 83), (0, 8, 125), (0, 8, 61), (0, 9, 219),
        (18, 7, 23), (0, 8, 109), (0, 8, 45), (0, 9, 187), (0, 8, 13), (0, 8, 141), (0, 8, 77), (0, 9, 251),
        (16, 7, 3), (0, 8, 83), (0, 8, 19), (21, 8, 195), (19, 7, 35), (0, 8, 115), (0, 8, 51), (0, 9, 199),
        (17, 7, 11), (0, 8, 99), (0, 8, 35), (0, 9, 167), (0, 8, 3), (0, 8, 131), (0, 8, 67), (0, 9, 231),
        (16, 7, 7), (0, 8, 91), (0, 8, 27), (0, 9, 151), (20, 7, 67), (0, 8, 123), (0, 8, 59), (0, 9, 215),
        (18, 7, 19), (0, 8, 107), (0, 8, 43), (0, 9, 183), (0, 8, 11), (0, 8, 139), (0, 8, 75), (0, 9, 247),
        (16, 7, 5), (0, 8, 87), (0, 8, 23), (64, 8, 0), (19, 7, 51), (0, 8, 119), (0, 8, 55), (0, 9, 207),
        (17, 7, 15), (0, 8, 103), (0, 8, 39), (0, 9, 175), (0, 8, 7), (0, 8, 135), (0, 8, 71), (0, 9, 239),
        (16, 7, 9), (0, 8, 95), (0, 8, 31), (0, 9, 159), (20, 7, 99), (0, 8, 127), (0, 8, 63), (0, 9, 223),
        (18, 7, 27), (0, 8, 111), (0, 8, 47), (0, 9, 191), (0, 8, 15), (0, 8, 143), (0, 8, 79), (0, 9, 255),
    ];

    // Reference fixed distance decode table (`distfix[32]` from `inffixed.h`).
    // Unlike `lenfix`, this table has no MAKEFIXED patch, so `inflate_table`
    // reproduces it exactly.
    #[rustfmt::skip]
    const DISTFIX_REF: [(u8, u8, u16); 32] = [
        (16, 5, 1), (23, 5, 257), (19, 5, 17), (27, 5, 4097), (17, 5, 5), (25, 5, 1025),
        (21, 5, 65), (29, 5, 16385), (16, 5, 3), (24, 5, 513), (20, 5, 33), (28, 5, 8193),
        (18, 5, 9), (26, 5, 2049), (22, 5, 129), (64, 5, 0), (16, 5, 2), (23, 5, 385),
        (19, 5, 25), (27, 5, 6145), (17, 5, 7), (25, 5, 1537), (21, 5, 97), (29, 5, 24577),
        (16, 5, 4), (24, 5, 769), (20, 5, 49), (28, 5, 12289), (18, 5, 13), (26, 5, 3073),
        (22, 5, 193), (64, 5, 0),
    ];

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(MAXBITS, 15);
        assert_eq!(MAX_BITS, MAXBITS);
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);
    }

    #[test]
    fn code_layout_is_four_bytes_repr_c() {
        // The FFI layer and the fixed tables rely on the exact 4-byte layout.
        assert_eq!(core::mem::size_of::<Code>(), 4);
        assert_eq!(core::mem::align_of::<Code>(), 2);
        assert_eq!(
            Code::default(),
            Code {
                op: 0,
                bits: 0,
                val: 0
            }
        );
    }

    #[test]
    fn fixed_literal_length_table_matches_inffixed() {
        // Fixed literal/length code lengths (see inftrees.c L332-L348):
        // 0..144 -> 8, 144..256 -> 9, 256..280 -> 7, 280..288 -> 8.
        let mut lens = [0u16; 288];
        lens[0..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..288].fill(8);

        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 288];
        let mut index = 0usize;
        let mut bits = 9usize;

        let res = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Ok(()));
        assert_eq!(bits, 9, "root bits should remain 9");
        assert_eq!(
            index, 512,
            "fixed lit/len table uses exactly 1 << 9 entries"
        );
        assert!(index <= ENOUGH_LENS, "must fit within ENOUGH_LENS");

        // Cross-check every entry against inffixed.h, accounting for the
        // MAKEFIXED op=64 patch at indices where (i & 127) == 99.
        for (i, &(op, b, val)) in LENFIX_REF.iter().enumerate() {
            assert_eq!(table[i].bits, b, "bits mismatch at index {i}");
            assert_eq!(table[i].val, val, "val mismatch at index {i}");
            if (i & 127) == 99 {
                assert_eq!(op, 64, "reference sanity: patched op at index {i}");
                assert!(
                    table[i].op == 68 || table[i].op == 193,
                    "expected raw invalid-marker op (68 or 193) at index {i}, got {}",
                    table[i].op
                );
                assert_ne!(table[i].op & 64, 0, "invalid-code bit must be set at {i}");
            } else {
                assert_eq!(table[i].op, op, "op mismatch at index {i}");
            }
        }
    }

    #[test]
    fn fixed_distance_table_matches_inffixed() {
        // Fixed distance code: 32 symbols, all length 5.
        let lens = [5u16; 32];

        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 32];
        let mut index = 0usize;
        let mut bits = 5usize;

        let res = inflate_table(
            CodeType::Dists,
            &lens,
            32,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Ok(()));
        assert_eq!(bits, 5, "root bits should remain 5");
        assert_eq!(
            index, 32,
            "fixed distance table uses exactly 1 << 5 entries"
        );
        assert!(index <= ENOUGH_DISTS, "must fit within ENOUGH_DISTS");

        // The distance table has no MAKEFIXED patch, so it matches exactly.
        for (i, &(op, b, val)) in DISTFIX_REF.iter().enumerate() {
            assert_eq!(
                table[i],
                Code { op, bits: b, val },
                "distfix mismatch at index {i}"
            );
        }
    }

    #[test]
    fn codes_type_builds_literal_only_table() {
        // A complete 4-symbol code, all length 2 (exercises the CODES literal
        // branch: every symbol is emitted as an op=0 literal entry).
        let lens = [2u16, 2, 2, 2];

        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 4];
        let mut index = 0usize;
        let mut bits = 7usize;

        let res = inflate_table(
            CodeType::Codes,
            &lens,
            4,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Ok(()));
        assert_eq!(bits, 2, "root bits clamped to the max code length");
        assert_eq!(index, 4, "a complete 2-bit code uses 1 << 2 entries");

        let mut seen = [false; 4];
        for entry in &table[0..4] {
            assert_eq!(entry.op, 0, "every entry must be a literal");
            assert_eq!(entry.bits, 2);
            seen[entry.val as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "all symbols 0..=3 must be present");
    }

    #[test]
    fn over_subscribed_code_is_invalid() {
        // Three symbols of length 1 over-subscribe the single available bit.
        let lens = [1u16, 1, 1];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 3];
        let mut index = 0usize;
        let mut bits = 7usize;

        let res = inflate_table(
            CodeType::Codes,
            &lens,
            3,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn incomplete_codes_code_is_invalid() {
        // A single length-1 code is incomplete; for CODES this is rejected.
        let lens = [1u16];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 1];
        let mut index = 0usize;
        let mut bits = 7usize;

        let res = inflate_table(
            CodeType::Codes,
            &lens,
            1,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn all_zero_lengths_yield_degenerate_table() {
        // No symbols at all: a two-entry invalid table is produced and Ok is
        // returned so that decoding reports the error later.
        let lens = [0u16; 4];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 4];
        let mut index = 0usize;
        let mut bits = 6usize;

        let res = inflate_table(
            CodeType::Lens,
            &lens,
            4,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Ok(()));
        assert_eq!(bits, 1, "degenerate table reports 1 root bit");
        assert_eq!(index, 2, "exactly two entries are written");

        let invalid = Code {
            op: 64,
            bits: 1,
            val: 0,
        };
        assert_eq!(table[0], invalid);
        assert_eq!(table[1], invalid);
    }

    #[test]
    fn incomplete_single_length_one_distance_code_is_allowed() {
        // A single distance code of length 1 is incomplete, but permitted for
        // non-CODES tables (max == 1). It exercises the incomplete-leftover
        // fill that writes a trailing invalid marker.
        let lens = [1u16];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 1];
        let mut index = 0usize;
        let mut bits = 5usize;

        let res = inflate_table(
            CodeType::Dists,
            &lens,
            1,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        );
        assert_eq!(res, Ok(()));
        assert_eq!(bits, 1);
        assert_eq!(index, 2);
        // Symbol 0 -> distance base 1 with 16 extra bits (dext[0]/dbase[0]).
        assert_eq!(
            table[0],
            Code {
                op: 16,
                bits: 1,
                val: 1
            }
        );
        // Trailing invalid-code marker for the unused half of the 1-bit space.
        assert_eq!(
            table[1],
            Code {
                op: 64,
                bits: 1,
                val: 0
            }
        );
    }

    #[test]
    fn table_index_is_advanced_for_appended_tables() {
        // Two consecutive builds must append: the second starts where the
        // first left off, mirroring the C `*table += used` contract.
        let dist_lens = [5u16; 32];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 32];
        let mut index = 0usize;

        let mut bits = 5usize;
        inflate_table(
            CodeType::Dists,
            &dist_lens,
            32,
            &mut table,
            &mut index,
            &mut bits,
            &mut work,
        )
        .unwrap();
        assert_eq!(index, 32);

        // A second small distance code appended after the first.
        let small = [1u16];
        let mut work2 = [0u16; 1];
        let mut bits2 = 5usize;
        inflate_table(
            CodeType::Dists,
            &small,
            1,
            &mut table,
            &mut index,
            &mut bits2,
            &mut work2,
        )
        .unwrap();
        // The first table (32 entries) is untouched; the second was written
        // starting at offset 32.
        assert_eq!(index, 34);
        assert_eq!(
            table[32],
            Code {
                op: 16,
                bits: 1,
                val: 1
            }
        );
        assert_eq!(
            table[33],
            Code {
                op: 64,
                bits: 1,
                val: 0
            }
        );
    }
}
