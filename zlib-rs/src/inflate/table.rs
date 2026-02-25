//! Huffman code table construction for the inflate decompressor.
//!
//! This module ports `inftrees.c` (424 lines) and `inftrees.h` (64 lines) from
//! the C zlib library into safe, idiomatic Rust. It defines the [`Code`] struct
//! representing a single Huffman lookup table entry, the [`CodeType`] enum for
//! selecting the type of code being built, the `ENOUGH` family of constants for
//! table sizing, and the [`inflate_table`] function that builds Huffman decoding
//! tables from canonical code lengths.
//!
//! Every other inflate module depends on the types and constants defined here.
//!
//! # Table sizing
//!
//! The maximum number of code structures is 1444, which is the sum of 852 for
//! literal/length codes and 592 for distance codes. These values were found by
//! exhaustive searches using the program `examples/enough.c` found in the zlib
//! distribution. The arguments to that program are the number of symbols, the
//! initial root table size, and the maximum bit length of a code.
//! `"enough 286 9 15"` for literal/length codes returns 852, and
//! `"enough 30 6 15"` for distance codes returns 592.

use core::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of bits in any Huffman code (RFC 1951 Section 3.2.2).
///
/// DEFLATE limits code lengths to 15 bits for both literal/length and distance
/// codes. This value is used as the upper bound for bit-length counting,
/// code validation, and table construction loops.
pub(crate) const MAXBITS: u32 = 15;

/// Maximum size of the lookup table for length/literal codes.
///
/// The maximum number of table entries for length/literal codes when the
/// root table index bits is 9. Computed by the `enough` program
/// (`examples/enough.c`) with root = 9. The value 852 guarantees that
/// [`inflate_table`] will never exceed this bound for valid DEFLATE length
/// code sets.
pub const ENOUGH_LENS: usize = 852;

/// Maximum size of the lookup table for distance codes.
///
/// The maximum number of table entries for distance codes when the root
/// table index bits is 6. Computed by the `enough` program with root = 6.
pub const ENOUGH_DISTS: usize = 592;

/// Maximum total table entries needed for both code tables.
///
/// `ENOUGH = ENOUGH_LENS + ENOUGH_DISTS = 852 + 592 = 1444`.
/// Used to pre-allocate the `codes[]` array in `InflateState`.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

// Compile-time verification that ENOUGH is consistent.
const _: () = assert!(ENOUGH == ENOUGH_LENS + ENOUGH_DISTS);
const _: () = assert!(ENOUGH == 1444);

/// Copyright string embedded in the binary per zlib convention.
///
/// If you use the zlib library in a product, an acknowledgment is welcome
/// in the documentation of your product. If for some reason you cannot
/// include such an acknowledgment, we appreciate that you keep this
/// copyright string in the executable of your product.
pub(crate) const INFLATE_COPYRIGHT: &str = " inflate 1.3.2.1 Copyright 1995-2026 Mark Adler ";

// ---------------------------------------------------------------------------
// Code struct — Huffman table entry
// ---------------------------------------------------------------------------

/// A single entry in a Huffman lookup table.
///
/// This 4-byte struct represents one entry in the root or secondary
/// Huffman code table used by the inflate decoder.
///
/// The `op` field encodes multiple purposes via bit flags:
///
/// | Bit pattern   | Meaning                                        |
/// |---------------|------------------------------------------------|
/// | `0000_0000`   | Literal byte — `val` is the byte value         |
/// | `0000_tttt`   | Sub-table link (`tttt ≠ 0`): `tttt` index bits |
/// | `0001_eeee`   | Length or distance: `eeee` extra bits to read   |
/// | `0110_0000`   | End-of-block marker (96 = 32 + 64)             |
/// | `0100_0000`   | Invalid code marker (64)                       |
///
/// For lengths and distances, `val` holds the base value and the low 4 bits
/// of `op` give the number of extra bits to read from the input stream.
/// For sub-table links, `val` is the offset from the root table start to
/// the sub-table, `bits` is the root table bits consumed, and `op` gives
/// the sub-table index bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Code {
    /// Operation, extra bits count, or sub-table index bits.
    pub op: u8,
    /// Number of bits in this code or sub-code.
    pub bits: u8,
    /// Offset to sub-table, literal value, base length, or base distance.
    pub val: u16,
}

impl Code {
    /// Create a new `Code` with the given operation, bit count, and value.
    #[inline]
    #[must_use]
    pub const fn new(op: u8, bits: u8, val: u16) -> Self {
        Self { op, bits, val }
    }
}

impl Default for Code {
    #[inline]
    fn default() -> Self {
        Self {
            op: 0,
            bits: 0,
            val: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// CodeType enum
// ---------------------------------------------------------------------------

/// Type of Huffman code being built by [`inflate_table`].
///
/// Determines how symbols are interpreted during table construction:
///
/// - **`Codes`** — Code lengths for the code length alphabet (used during
///   dynamic block header decoding, symbols 0–18). No base/extra table lookup.
/// - **`Lens`** — Length/literal codes (symbols 0–285). Symbols 0–255 are
///   literals, 256 is end-of-block, 257–285 are lengths with base values and
///   extra bits from the length tables.
/// - **`Dists`** — Distance codes (symbols 0–29). Each symbol maps to a base
///   backward distance with extra bits from the distance tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeType {
    /// Code lengths for the code length code alphabet.
    Codes,
    /// Length/literal codes (symbols 0..285).
    Lens,
    /// Distance codes (symbols 0..29).
    Dists,
}

// ---------------------------------------------------------------------------
// InflateTableError
// ---------------------------------------------------------------------------

/// Errors from Huffman table construction via [`inflate_table`].
///
/// Maps to the C return codes: `-1` → [`InvalidCodeSet`](Self::InvalidCodeSet),
/// `+1` → [`NotEnoughSpace`](Self::NotEnoughSpace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateTableError {
    /// Over-subscribed or incomplete set of code lengths.
    ///
    /// The supplied code lengths do not form a valid prefix-free code.
    /// This corresponds to `return -1` in the C implementation.
    InvalidCodeSet,
    /// Not enough table space (`ENOUGH` exceeded).
    ///
    /// The generated tables would exceed the pre-computed maximum size
    /// (`ENOUGH_LENS` or `ENOUGH_DISTS`). This should never happen for
    /// valid DEFLATE streams but is checked defensively.
    /// Corresponds to `return 1` in the C implementation.
    NotEnoughSpace,
}

impl fmt::Display for InflateTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCodeSet => {
                f.write_str("over-subscribed or incomplete set of code lengths")
            }
            Self::NotEnoughSpace => f.write_str("not enough table space (ENOUGH exceeded)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Static lookup tables — exact copies from inftrees.c lines 69–82
// ---------------------------------------------------------------------------

/// Base values for length codes 257..285.
///
/// Index `i` gives the base match length for length code `257 + i`.
/// The last two entries (codes 286, 287) are unused and set to 0.
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// Extra bits for length codes 257..285.
///
/// These use the C zlib encoding where the low 4 bits represent the actual
/// extra bit count and higher bits encode flags:
/// - Values 16 (`0x10`): 0 extra bits, length flag set (codes 257–264)
/// - Values 17–21: 1–5 extra bits respectively
/// - Value 68 (`0x44`): invalid code marker (code 286)
/// - Value 193 (`0xC1`): invalid code marker (code 287)
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Base values for distance codes 0..29.
///
/// Index `i` gives the base backward distance for distance code `i`.
/// The last two entries (codes 30, 31) are unused and set to 0.
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Extra bits for distance codes 0..29.
///
/// Same encoding as [`LEXT`] — low 4 bits are the extra bit count, high bits
/// are flags. Values 64 at indices 30 and 31 mark invalid codes.
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

// ---------------------------------------------------------------------------
// inflate_table — core Huffman table builder
// ---------------------------------------------------------------------------

/// Build a set of Huffman decoding tables from canonical code lengths.
///
/// Constructs a root table and optional secondary sub-tables for fast Huffman
/// decoding. The code lengths are `lens[0..codes-1]`. The result is written
/// into `table` starting at index `table_start`.
///
/// # Parameters
///
/// - `code_type` — Type of code being built ([`Codes`](CodeType::Codes),
///   [`Lens`](CodeType::Lens), or [`Dists`](CodeType::Dists)).
/// - `lens` — Code lengths for each symbol (0 = symbol not used). Must have
///   at least `codes` elements, each in the range `0..=MAXBITS`.
/// - `codes` — Number of symbols (length of `lens` to consider).
/// - `table` — Output buffer for [`Code`] entries. Must have capacity ≥
///   `ENOUGH_LENS` or `ENOUGH_DISTS` depending on code type.
/// - `table_start` — Starting index in `table` for this code set.
/// - `bits` — On entry, requested root table index bits. On return, actual
///   root table index bits (may differ if clamped to code length range).
/// - `work` — Work area for symbol sorting (must be at least `codes` entries).
///
/// # Returns
///
/// - `Ok(used)` — Number of table entries used, starting at `table_start`.
/// - `Err(InflateTableError::InvalidCodeSet)` — Over-subscribed or incomplete
///   set of code lengths.
/// - `Err(InflateTableError::NotEnoughSpace)` — Table space exceeded `ENOUGH`.
///
/// # Algorithm
///
/// The function processes canonical Huffman code lengths into a two-level
/// lookup table. Short codes (up to `root` bits) are in the root table.
/// Longer codes are in sub-tables linked from root entries. The tables
/// use bit-reversed codes so the decoder can index directly from the low
/// bits of the bit accumulator.
#[allow(clippy::too_many_lines)]
pub(crate) fn inflate_table(
    code_type: CodeType,
    lens: &[u16],
    codes: usize,
    table: &mut [Code],
    table_start: usize,
    bits: &mut u32,
    work: &mut [u16],
) -> Result<usize, InflateTableError> {
    // ------------------------------------------------------------------
    // Step 1: Accumulate lengths for codes.
    // Assumes lens[] values are all in 0..MAXBITS (caller must assure this).
    // ------------------------------------------------------------------
    let mut count = [0u16; MAXBITS as usize + 1];
    for sym in 0..codes {
        count[lens[sym] as usize] += 1;
    }

    // ------------------------------------------------------------------
    // Step 2: Bound code lengths, force root to be within code lengths.
    // ------------------------------------------------------------------
    let mut root = *bits;

    // Find maximum code length.
    let mut max = MAXBITS;
    while max >= 1 && count[max as usize] == 0 {
        max -= 1;
    }
    if root > max {
        root = max;
    }

    // Handle empty code set — no symbols to code at all.
    if max == 0 {
        // Make a table with two entries that force an error on decode.
        table[table_start] = Code::new(64, 1, 0);
        table[table_start + 1] = Code::new(64, 1, 0);
        *bits = 1;
        return Ok(2);
    }

    // Find minimum code length.
    let mut min = 1u32;
    while min < max && count[min as usize] == 0 {
        min += 1;
    }
    if root < min {
        root = min;
    }

    // ------------------------------------------------------------------
    // Step 3: Check for an over-subscribed or incomplete set of lengths.
    // ------------------------------------------------------------------
    let mut left: i32 = 1;
    for len in 1..=MAXBITS {
        left <<= 1;
        left -= i32::from(count[len as usize]);
        if left < 0 {
            // Over-subscribed.
            return Err(InflateTableError::InvalidCodeSet);
        }
    }
    if left > 0 && (code_type == CodeType::Codes || max != 1) {
        // Incomplete set.
        return Err(InflateTableError::InvalidCodeSet);
    }

    // ------------------------------------------------------------------
    // Step 4: Generate offsets into symbol table for each length for sorting.
    // ------------------------------------------------------------------
    let mut offs = [0u16; MAXBITS as usize + 1];
    for len in 1..MAXBITS as usize {
        offs[len + 1] = offs[len] + count[len];
    }

    // ------------------------------------------------------------------
    // Step 5: Sort symbols by length, by symbol order within each length.
    // ------------------------------------------------------------------
    for sym in 0..codes {
        if lens[sym] != 0 {
            let idx = offs[lens[sym] as usize] as usize;
            // Symbol index always fits in u16 (max is 287 for LENS, 31 for DISTS).
            #[allow(clippy::cast_possible_truncation)]
            {
                work[idx] = sym as u16;
            }
            offs[lens[sym] as usize] += 1;
        }
    }

    // ------------------------------------------------------------------
    // Step 6: Set up for code type — select base/extra tables and the
    // threshold value that distinguishes literals from length/distance codes.
    //
    // For CODES: match_val=20, no base/extra (code lengths are 0..18)
    // For LENS:  match_val=257, uses LBASE/LEXT (literals < 257, end-of-block = 256)
    // For DISTS: match_val=0, uses DBASE/DEXT (all symbols use tables)
    // ------------------------------------------------------------------
    let (base_table, extra_table, match_val): (&[u16], &[u16], u32) = match code_type {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // ------------------------------------------------------------------
    // Step 7: Initialize state for the main table-building loop.
    // ------------------------------------------------------------------
    let mut huff: u32 = 0; // starting code
    let mut sym: usize = 0; // starting code symbol index into work[]
    let mut len = min; // starting code length
    let mut next = table_start; // current write position in table[]
    let mut curr = root; // current table index bits
    let mut drop_bits: u32 = 0; // code bits to drop for sub-table
    // low = trigger new sub-table when (huff & mask) != low.
    // C initializes to (unsigned)(-1) so it never matches on first iteration.
    let mut low: u32 = u32::MAX;
    let mut used: u32 = 1u32 << root; // entries used so far
    let mask: u32 = used - 1; // mask for low root bits

    // ------------------------------------------------------------------
    // Step 8: Check available table space.
    // ------------------------------------------------------------------
    if (code_type == CodeType::Lens && used as usize > ENOUGH_LENS)
        || (code_type == CodeType::Dists && used as usize > ENOUGH_DISTS)
    {
        return Err(InflateTableError::NotEnoughSpace);
    }

    // ------------------------------------------------------------------
    // Step 9: Process all codes and make table entries.
    //
    // In this loop, the table being filled starts at `next` and has `curr`
    // index bits. The code being used is `huff` with length `len`. That code
    // is converted to an index by dropping `drop_bits` bits off the bottom.
    // For codes where `len < drop_bits + curr`, the top bits are replicated.
    //
    // `root` is the number of index bits for the root table. When `len`
    // exceeds `root`, sub-tables are created pointed to by the root entry
    // with an index of the low `root` bits of `huff`.
    // ------------------------------------------------------------------
    loop {
        // -- 9a: Create table entry --
        #[allow(clippy::cast_possible_truncation)]
        let here_bits = (len - drop_bits) as u8;

        let work_sym = u32::from(work[sym]);
        let (here_op, here_val): (u8, u16) = if work_sym.wrapping_add(1) < match_val {
            // Literal or short code — op is 0, val is the symbol.
            (0, work[sym])
        } else if work_sym >= match_val {
            // Length or distance code — look up base value and extra bits.
            // Safety: base_table and extra_table are non-empty for LENS/DISTS,
            // and for CODES this branch is unreachable (all symbols < match_val=20).
            let idx = (work_sym - match_val) as usize;
            #[allow(clippy::cast_possible_truncation)]
            let op = if idx < extra_table.len() {
                extra_table[idx] as u8
            } else {
                64 // invalid code marker (defensive)
            };
            let val = if idx < base_table.len() {
                base_table[idx]
            } else {
                0
            };
            (op, val)
        } else {
            // End of block (symbol 256 for LENS type, work[sym]+1 == match_val).
            (32 + 64, 0)
        };

        let here = Code::new(here_op, here_bits, here_val);

        // -- 9b: Replicate for those indices with low len bits equal to huff --
        let incr_fill = 1u32 << (len - drop_bits);
        let mut fill = 1u32 << curr;
        // Save current table size for later sub-table offset calculation.
        // After this point, `fill` is modified by the replication loop but
        // `table_size` preserves the original 1 << curr value.
        let table_size = fill;
        loop {
            fill -= incr_fill;
            table[next + ((huff >> drop_bits) + fill) as usize] = here;
            if fill == 0 {
                break;
            }
        }

        // -- 9c: Backwards-increment the len-bit Huffman code --
        // DEFLATE stores codes with the bit order reversed from the natural
        // integer increment ordering; hence we increment backwards.
        let mut incr = 1u32 << (len - 1);
        while huff & incr != 0 {
            incr >>= 1;
        }
        if incr != 0 {
            huff &= incr - 1;
            huff += incr;
        } else {
            huff = 0;
        }

        // -- 9d: Go to next symbol, update count and len --
        sym += 1;
        count[len as usize] -= 1;
        if count[len as usize] == 0 {
            if len == max {
                break;
            }
            // Get the code length of the next symbol to process.
            len = u32::from(lens[work[sym] as usize]);
        }

        // -- 9e: Create new sub-table if needed --
        if len > root && (huff & mask) != low {
            // If first time entering sub-table territory, transition from
            // the root table to sub-tables.
            if drop_bits == 0 {
                drop_bits = root;
            }

            // Increment past the last table.
            next += table_size as usize;

            // Determine the length of the next sub-table.
            curr = len - drop_bits;
            let mut left_sub = 1i32 << curr;
            while curr + drop_bits < max {
                left_sub -= i32::from(count[(curr + drop_bits) as usize]);
                if left_sub <= 0 {
                    break;
                }
                curr += 1;
                left_sub <<= 1;
            }

            // Check for enough space.
            used += 1u32 << curr;
            if (code_type == CodeType::Lens && used as usize > ENOUGH_LENS)
                || (code_type == CodeType::Dists && used as usize > ENOUGH_DISTS)
            {
                return Err(InflateTableError::NotEnoughSpace);
            }

            // Point root entry to sub-table.
            low = huff & mask;
            #[allow(clippy::cast_possible_truncation)]
            {
                table[table_start + low as usize] =
                    Code::new(curr as u8, root as u8, (next - table_start) as u16);
            }
        }
    }

    // ------------------------------------------------------------------
    // Step 10: Fill in remaining table entry if code is incomplete.
    //
    // Guaranteed to have at most one remaining entry, since if the code
    // is incomplete, the maximum code length allowed to get this far is
    // one bit.
    // ------------------------------------------------------------------
    if huff != 0 {
        #[allow(clippy::cast_possible_truncation)]
        let remaining = Code::new(64, (len - drop_bits) as u8, 0);
        table[next + huff as usize] = remaining;
    }

    // ------------------------------------------------------------------
    // Step 11: Set return parameters.
    // ------------------------------------------------------------------
    *bits = root;
    Ok(used as usize)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_correct() {
        assert_eq!(MAXBITS, 15);
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);
    }

    #[test]
    fn code_default() {
        let c = Code::default();
        assert_eq!(c.op, 0);
        assert_eq!(c.bits, 0);
        assert_eq!(c.val, 0);
    }

    #[test]
    fn code_new() {
        let c = Code::new(96, 7, 256);
        assert_eq!(c.op, 96);
        assert_eq!(c.bits, 7);
        assert_eq!(c.val, 256);
    }

    #[test]
    fn code_size_is_four_bytes() {
        assert_eq!(core::mem::size_of::<Code>(), 4);
    }

    #[test]
    fn code_type_variants() {
        // Ensure all three variants exist and are distinct.
        assert_ne!(CodeType::Codes, CodeType::Lens);
        assert_ne!(CodeType::Lens, CodeType::Dists);
        assert_ne!(CodeType::Codes, CodeType::Dists);
    }

    #[test]
    fn inflate_table_error_display() {
        let e1 = InflateTableError::InvalidCodeSet;
        let e2 = InflateTableError::NotEnoughSpace;
        assert!(!e1.to_string().is_empty());
        assert!(!e2.to_string().is_empty());
    }

    /// Verify that the static lookup tables match the C originals exactly.
    #[test]
    fn static_tables_match_c() {
        // LBASE — length base values for codes 257..285
        assert_eq!(LBASE.len(), 31);
        assert_eq!(
            LBASE,
            [
                3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83,
                99, 115, 131, 163, 195, 227, 258, 0, 0,
            ]
        );

        // LEXT — length extra bits for codes 257..285
        assert_eq!(LEXT.len(), 31);
        assert_eq!(
            LEXT,
            [
                16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20,
                20, 20, 20, 21, 21, 21, 21, 16, 68, 193,
            ]
        );

        // DBASE — distance base values for codes 0..29
        assert_eq!(DBASE.len(), 32);
        assert_eq!(
            DBASE,
            [
                1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769,
                1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
            ]
        );

        // DEXT — distance extra bits for codes 0..29
        assert_eq!(DEXT.len(), 32);
        assert_eq!(
            DEXT,
            [
                16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25,
                25, 26, 26, 27, 27, 28, 28, 29, 29, 64, 64,
            ]
        );
    }

    /// Empty code set (max == 0) should produce 2 invalid-marker entries.
    #[test]
    fn inflate_table_empty_code_set() {
        let lens = [0u16; 10];
        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 9u32;
        let mut work = vec![0u16; 10];

        let result = inflate_table(
            CodeType::Codes,
            &lens,
            10,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        assert_eq!(result, Ok(2));
        assert_eq!(bits, 1);
        assert_eq!(table[0].op, 64);
        assert_eq!(table[0].bits, 1);
        assert_eq!(table[1].op, 64);
        assert_eq!(table[1].bits, 1);
    }

    /// Over-subscribed code set should return InvalidCodeSet.
    #[test]
    fn inflate_table_oversubscribed() {
        // Two symbols both with 1-bit codes would need 2 codes in 1 bit,
        // but 1 bit can only represent 2 codes. Adding a third with 1 bit
        // over-subscribes.
        let lens = [1u16, 1, 1];
        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 9u32;
        let mut work = vec![0u16; 3];

        let result = inflate_table(
            CodeType::Codes,
            &lens,
            3,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        assert_eq!(result, Err(InflateTableError::InvalidCodeSet));
    }

    /// Build fixed length/literal table (used by RFC 1951 Section 3.2.6).
    /// Verifies the function runs without errors for the standard fixed codes.
    #[test]
    fn inflate_table_fixed_length_codes() {
        // Build the fixed literal/length code lengths per RFC 1951 3.2.6:
        //   0–143: 8 bits, 144–255: 9 bits, 256–279: 7 bits, 280–287: 8 bits
        let mut lens = [0u16; 288];
        for item in lens.iter_mut().take(144) {
            *item = 8;
        }
        for item in lens.iter_mut().take(256).skip(144) {
            *item = 9;
        }
        for item in lens.iter_mut().take(280).skip(256) {
            *item = 7;
        }
        for item in lens.iter_mut().take(288).skip(280) {
            *item = 8;
        }

        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 9u32;
        let mut work = vec![0u16; 288];

        let result = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        assert!(result.is_ok());
        let used = result.expect("table build should succeed");
        assert_eq!(bits, 9);
        // The fixed length table should use exactly 512 entries (2^9 root table).
        assert_eq!(used, 512);
    }

    /// Build fixed distance table.
    #[test]
    fn inflate_table_fixed_distance_codes() {
        // All 32 distance codes have length 5 per RFC 1951 3.2.6.
        let lens = [5u16; 32];
        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 5u32;
        let mut work = vec![0u16; 32];

        let result = inflate_table(
            CodeType::Dists,
            &lens,
            32,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        assert!(result.is_ok());
        let used = result.expect("table build should succeed");
        assert_eq!(bits, 5);
        // With 32 codes all at 5 bits, root=5, we get exactly 32 entries.
        assert_eq!(used, 32);
    }

    /// Single-symbol code should succeed.
    #[test]
    fn inflate_table_single_symbol() {
        // A single symbol with code length 1 is valid for distance codes
        // (left > 0 but max == 1, so the incomplete check passes).
        let lens = [1u16, 0];
        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 6u32;
        let mut work = vec![0u16; 2];

        let result = inflate_table(
            CodeType::Dists,
            &lens,
            2,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        // This should succeed: LENS/DISTS allow incomplete codes when max==1.
        assert!(result.is_ok());
    }

    /// Build a simple CODES table (for code length alphabet).
    #[test]
    fn inflate_table_simple_codes() {
        // Two symbols (0 and 1) each with code length 1.
        let lens = [1u16, 1];
        let mut table = vec![Code::default(); ENOUGH];
        let mut bits = 7u32;
        let mut work = vec![0u16; 2];

        let result = inflate_table(
            CodeType::Codes,
            &lens,
            2,
            &mut table,
            0,
            &mut bits,
            &mut work,
        );

        assert!(result.is_ok());
        let used = result.expect("table build should succeed");
        assert_eq!(bits, 1);
        assert_eq!(used, 2);
        // Symbol 0 should be at index 0, symbol 1 at index 1 (or vice versa).
        assert_eq!(table[0].op, 0);
        assert_eq!(table[1].op, 0);
        // Values should be the symbols themselves.
        let vals: Vec<u16> = vec![table[0].val, table[1].val];
        assert!(vals.contains(&0));
        assert!(vals.contains(&1));
    }

    /// Verify the copyright string matches the C original.
    #[test]
    fn copyright_string() {
        assert!(INFLATE_COPYRIGHT.contains("1.3.2.1"));
        assert!(INFLATE_COPYRIGHT.contains("Mark Adler"));
    }
}
