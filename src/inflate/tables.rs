//! Huffman table builder for the inflate decompression engine.
//!
//! This module is a faithful port of C `inftrees.c` (424 lines) and `inftrees.h`
//! (64 lines) from zlib v1.3.2.1. It defines the foundational [`Code`] struct used
//! throughout the inflate module and the [`inflate_table`] function that builds
//! two-level Huffman decode tables from code length arrays.
//!
//! # Architecture
//!
//! The inflate decompressor uses two-level lookup tables for fast Huffman decoding.
//! The first level (root table) is indexed by the low `root_bits` of the bit
//! accumulator. If the code is longer than `root_bits`, the first-level entry
//! links to a second-level sub-table indexed by additional bits.
//!
//! This module builds these tables from arrays of code lengths, handling three
//! distinct table types:
//! - **Codes**: Code-length code lengths (for reading dynamic table descriptors)
//! - **Lens**: Literal/length code lengths (up to 286 symbols)
//! - **Dists**: Distance code lengths (up to 32 symbols)

use crate::error::ZlibError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum bit length for any Huffman code (per DEFLATE spec, RFC 1951 §3.2.2).
const MAXBITS: usize = 15;

/// Maximum entries for literal/length Huffman decode table.
///
/// Calculated by `enough(286, 9)` = 852 — see the `examples/enough.c` program
/// in the zlib distribution. The argument 286 is the number of literal/length
/// symbols and 9 is the root table index bits used for length codes.
pub const ENOUGH_LENS: usize = 852;

/// Maximum entries for distance Huffman decode table.
///
/// Calculated by `enough(30, 6)` = 592. The argument 30 is the number of
/// distance symbols and 6 is the root table index bits used for distance codes.
pub const ENOUGH_DISTS: usize = 592;

/// Total maximum entries for the combined Huffman decode tables.
///
/// `ENOUGH = ENOUGH_LENS + ENOUGH_DISTS = 1444`. This is the size of the
/// `codes[]` array pre-allocated in the inflate state.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

// ---------------------------------------------------------------------------
// Static base/extra tables (from inftrees.c lines 69–82)
// ---------------------------------------------------------------------------

/// Length codes 257..285 base length values (plus 2 sentinel entries).
///
/// Indexed by `symbol - 257`. For example, length code 257 has base length 3,
/// length code 258 has base length 4, etc. Codes 286–287 are sentinels (value 0).
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59,
    67, 83, 99, 115, 131, 163, 195, 227, 258, 0, 0,
];

/// Length codes 257..285 extra bits (encoded with op-field semantics).
///
/// Values 16–21 encode `(op & 16) != 0` (length flag) with `op & 15` extra bits.
/// Value 16 means 0 extra bits, 17 means 1 extra bit, etc.
/// Values 68 and 193 for codes 286–287 set the invalid code marker (`op & 64 != 0`).
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19,
    19, 19, 20, 20, 20, 20, 21, 21, 21, 21, 16, 68, 193,
];

/// Distance codes 0..29 base distance values (plus 2 sentinel entries).
///
/// Indexed directly by distance code symbol. For example, distance code 0 has
/// base distance 1, code 1 has base distance 2, etc. Codes 30–31 are sentinels.
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513,
    769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Distance codes 0..29 extra bits (encoded with op-field semantics).
///
/// Values 16–29 encode `(op & 16) != 0` (distance flag) with `op & 15` extra bits.
/// Value 64 for codes 30–31 marks them as invalid (`op & 64 != 0`).
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23,
    24, 24, 25, 25, 26, 26, 27, 27, 28, 28, 29, 29, 64, 64,
];

// ---------------------------------------------------------------------------
// Code struct (from inftrees.h lines 24–28)
// ---------------------------------------------------------------------------

/// Huffman code decode table entry.
///
/// This 4-byte struct is the fundamental unit of the inflate Huffman decode
/// tables. A two-level table lookup decodes literal/length and distance codes
/// by reading the `op`, `bits`, and `val` fields of the entry indexed by the
/// low bits of the bit accumulator.
///
/// # Field Semantics for `op`
///
/// The `op` field encodes the operation type using a bit-field convention
/// (from `inftrees.h` lines 30–36):
///
/// | Binary pattern | Meaning |
/// |----------------|---------|
/// | `00000000`     | Literal byte — `val` is the byte value (0–255) |
/// | `0000tttt`     | Sub-table link — `t` is the number of index bits for the next lookup; `val` is the offset to the sub-table |
/// | `0001eeee`     | Length or distance — `e` is the number of extra bits to read; `val` is the base value |
/// | `00100000`     | End of block (EOB) |
/// | `01000000`     | Invalid code |
///
/// The inflate fast-path tests these with bitmask operations:
/// - `op == 0`: literal
/// - `op & 16 != 0`: length or distance with extra bits
/// - `op & 32 != 0`: end of block
/// - `op & 64 != 0`: invalid code
/// - Otherwise (1–15, no high bits): sub-table link
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Code {
    /// Operation, extra bits, or sub-table index bits.
    pub op: u8,
    /// Number of bits consumed for this part of the code.
    pub bits: u8,
    /// Literal byte value, base length/distance, or sub-table offset.
    pub val: u16,
}

impl Code {
    /// Create a new [`Code`] entry with the given field values.
    ///
    /// This is a `const fn` so it can be used in static/const array initialisers
    /// (e.g. the pre-built fixed Huffman tables in `fixed.rs`).
    #[inline]
    pub const fn new(op: u8, bits: u8, val: u16) -> Self {
        Self { op, bits, val }
    }
}

impl Default for Code {
    /// Returns a zeroed [`Code`] entry (`op=0, bits=0, val=0`).
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
// CodeType enum (from inftrees.h lines 54–58)
// ---------------------------------------------------------------------------

/// Type of Huffman code table being built by [`inflate_table`].
///
/// Each variant corresponds to a different set of symbols and controls which
/// base/extra lookup tables are used during table construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeType {
    /// Code-length code lengths (for reading dynamic table descriptors).
    ///
    /// Up to 19 symbols (values 0–18). No base/extra tables are needed because
    /// each symbol directly represents a code-length value or a repeat indicator.
    Codes,
    /// Literal/length code lengths (up to 286 symbols: 0–255 literals, 256 EOB,
    /// 257–285 lengths).
    ///
    /// Uses `LBASE`/`LEXT` tables for length symbols 257+.
    Lens,
    /// Distance code lengths (up to 32 symbols: codes 0–31).
    ///
    /// Uses `DBASE`/`DEXT` tables for all distance symbols.
    Dists,
}

// ---------------------------------------------------------------------------
// inflate_table function (from inftrees.c lines 46–311)
// ---------------------------------------------------------------------------

/// Build a set of Huffman decode tables from an array of code lengths.
///
/// This function constructs a two-level lookup table for fast Huffman decoding.
/// The first level (root table) uses `root_bits` index bits. Any code longer
/// than `root_bits` creates a second-level sub-table linked from the root.
///
/// The algorithm is a faithful port of the C `inflate_table()` function in
/// `inftrees.c`. It:
///
/// 1. Counts code lengths to validate the Huffman tree
/// 2. Sorts symbols by code length
/// 3. Fills the root table with entries for short codes (≤ `root_bits`)
/// 4. Creates and links sub-tables for longer codes
///
/// # Arguments
///
/// * `code_type` — Type of table: [`CodeType::Codes`], [`CodeType::Lens`], or
///   [`CodeType::Dists`].
/// * `lens` — Array of code lengths for each symbol. `lens[i]` is the bit
///   length assigned to symbol `i`; zero means the symbol does not appear.
///   The caller must ensure all values are in `0..=MAXBITS` (15).
/// * `codes` — Number of symbols (the relevant prefix length of `lens`).
/// * `table` — Output slice into which [`Code`] entries are written. Must have
///   enough capacity for the root table plus all sub-tables.
/// * `table_offset` — Starting write offset within `table`. On return, updated
///   to point past the last entry written (i.e. the next free slot).
/// * `root_bits` — On entry, the *requested* root table index bits. On return,
///   the *actual* root bits used (clamped to `[min_code_len, max_code_len]`).
/// * `work` — Scratch workspace of at least `codes` entries, used for sorting.
///
/// # Returns
///
/// * `Ok(())` on success.
/// * `Err(ZlibError::DataError)` if the code lengths are over-subscribed
///   (impossible Huffman tree), represent an incomplete set (for `Lens`/`Dists`
///   types when `max > 1`), or exceed the `ENOUGH_LENS`/`ENOUGH_DISTS` space
///   limits.
///
/// # Panics
///
/// This function does not panic. All array accesses are bounded by the
/// algorithm's invariants (code lengths ≤ 15, table sizes ≤ ENOUGH).
pub fn inflate_table(
    code_type: CodeType,
    lens: &[u16],
    codes: usize,
    table: &mut [Code],
    table_offset: &mut usize,
    root_bits: &mut u32,
    work: &mut [u16],
) -> Result<(), ZlibError> {
    // ----- Phase 1: Count code lengths (inftrees.c lines 116–119) -----
    let mut count = [0u16; MAXBITS + 1];
    for sym in 0..codes {
        count[lens[sym] as usize] += 1;
    }

    // ----- Phase 2: Find min/max code lengths, clamp root (lines 121–137) -----
    let mut root = *root_bits as usize;

    // Find maximum code length (scan from MAXBITS down)
    let mut max: usize = MAXBITS;
    while max >= 1 {
        if count[max] != 0 {
            break;
        }
        max -= 1;
    }
    if root > max {
        root = max;
    }

    // No symbols to code at all — emit two invalid-code entries
    if max == 0 {
        let here = Code {
            op: 64,
            bits: 1,
            val: 0,
        };
        table[*table_offset] = here;
        table[*table_offset + 1] = here;
        *table_offset += 2;
        *root_bits = 1;
        return Ok(());
    }

    // Find minimum code length (scan from 1 up)
    let mut min: usize = 1;
    while min < max {
        if count[min] != 0 {
            break;
        }
        min += 1;
    }
    if root < min {
        root = min;
    }

    // ----- Phase 3: Check over-subscribed or incomplete (lines 139–147) -----
    let mut left: i32 = 1;
    for &c in &count[1..=MAXBITS] {
        left <<= 1;
        left -= c as i32;
        if left < 0 {
            // Over-subscribed set of code lengths
            return Err(ZlibError::DataError);
        }
    }
    if left > 0 && (code_type == CodeType::Codes || max != 1) {
        // Incomplete set of code lengths
        return Err(ZlibError::DataError);
    }

    // ----- Phase 4: Generate offsets for sorting (lines 149–152) -----
    let mut offs = [0u16; MAXBITS + 1];
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }

    // ----- Phase 5: Sort symbols by code length (lines 154–156) -----
    for sym in 0..codes {
        if lens[sym] != 0 {
            let off = offs[lens[sym] as usize] as usize;
            work[off] = sym as u16;
            offs[lens[sym] as usize] += 1;
        }
    }

    // ----- Phase 6: Select base/extra tables (lines 190–202) -----
    //
    // For CODES: base/extra are unused (match_val=20 guarantees the literal
    //   branch is always taken for code-length symbols 0–18).
    // For LENS:  base=LBASE, extra=LEXT, match_val=257.
    // For DISTS: base=DBASE, extra=DEXT, match_val=0.
    let (base, extra, match_val): (&[u16], &[u16], usize) = match code_type {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // ----- Phase 7: Build tables (lines 204–310) -----
    let table_start = *table_offset;
    let mut huff: u32 = 0; // starting code (bit-reversed)
    let mut sym: usize = 0; // index into sorted work[] array
    let mut len: usize = min; // current code length being processed
    let mut next_offset: usize = table_start; // current write position in table
    let mut curr: usize = root; // index bits for current table segment
    let mut drop: usize = 0; // bits to drop for sub-table indexing
    let mut low: u32 = u32::MAX; // low root-bits of previous code (trigger new sub-table)
    let mut used: usize = 1 << root; // total table entries allocated so far
    let mask: u32 = used as u32 - 1; // mask for comparing low root bits
    let mut min_val: usize; // size of current table segment (1 << curr)

    // Guard against exceeding pre-calculated table space limits
    if (code_type == CodeType::Lens && used > ENOUGH_LENS)
        || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
    {
        return Err(ZlibError::DataError);
    }

    // Process all codes and fill table entries
    loop {
        // ---- Create the table entry for the current symbol ----
        let bits_val = (len - drop) as u8;
        let work_sym = work[sym] as usize;

        let here = if work_sym + 1 < match_val {
            // Simple code: literal byte (LENS) or code-length value (CODES)
            Code {
                op: 0,
                bits: bits_val,
                val: work[sym],
            }
        } else if work_sym >= match_val {
            // Length or distance with base value and extra bits
            Code {
                op: extra[work_sym - match_val] as u8,
                bits: bits_val,
                val: base[work_sym - match_val],
            }
        } else {
            // End of block (symbol 256 for LENS type: work_sym + 1 == match_val)
            Code {
                op: 32 + 64, // op = 96
                bits: bits_val,
                val: 0,
            }
        };

        // ---- Replicate entry for all indices sharing the same low bits ----
        //
        // For a code of length `len - drop` in the current table segment of
        // size `1 << curr`, we fill every position that has the same low
        // `(len - drop)` bits equal to `(huff >> drop)`. The higher bits cycle
        // through all possible values.
        let incr_fill: usize = 1 << (len - drop);
        let fill_size: usize = 1 << curr;
        min_val = fill_size; // save for sub-table advancement
        let mut fill: usize = fill_size;
        loop {
            fill -= incr_fill;
            table[next_offset + (huff >> drop) as usize + fill] = here;
            if fill == 0 {
                break;
            }
        }

        // ---- Backwards increment the len-bit code huff (bit-reversal) ----
        //
        // Canonical Huffman codes are assigned in bit-reversed order. This
        // increment pattern counts upward in the bit-reversed domain.
        let mut incr: u32 = 1 << (len - 1);
        while huff & incr != 0 {
            incr >>= 1;
        }
        if incr != 0 {
            huff &= incr - 1;
            huff += incr;
        } else {
            huff = 0;
        }

        // ---- Advance to next symbol, update count and len ----
        sym += 1;
        count[len] -= 1;
        if count[len] == 0 {
            if len == max {
                break;
            }
            len = lens[work[sym] as usize] as usize;
        }

        // ---- Create new sub-table if needed (when len > root) ----
        if len > root && (huff & mask) != low {
            // First time entering sub-table territory: set drop to root
            if drop == 0 {
                drop = root;
            }

            // Advance past the previous table segment
            next_offset += min_val;

            // Determine the minimum number of index bits for the new sub-table
            // by looking ahead at remaining code length counts.
            curr = len - drop;
            let mut left_sub: i32 = 1 << curr;
            while curr + drop < max {
                left_sub -= count[curr + drop] as i32;
                if left_sub <= 0 {
                    break;
                }
                curr += 1;
                left_sub <<= 1;
            }

            // Check for enough table space
            used += 1 << curr;
            if (code_type == CodeType::Lens && used > ENOUGH_LENS)
                || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
            {
                return Err(ZlibError::DataError);
            }

            // Write a link entry in the root table pointing to this sub-table.
            // The link entry's `op` is the sub-table index bits, `bits` is the
            // root table bits, and `val` is the offset from table_start.
            low = huff & mask;
            table[table_start + low as usize] = Code {
                op: curr as u8,
                bits: root as u8,
                val: (next_offset - table_start) as u16,
            };
        }
    }

    // ---- Fill remaining entry for incomplete code sets ----
    //
    // If the code set was incomplete (allowed for CODES type, or LENS/DISTS
    // with max == 1), there may be one remaining unfilled entry. Mark it as
    // invalid so decoding will detect the error.
    if huff != 0 {
        let here = Code {
            op: 64, // invalid code marker
            bits: (len - drop) as u8,
            val: 0,
        };
        table[next_offset + huff as usize] = here;
    }

    // ---- Set return parameters ----
    *table_offset = table_start + used;
    *root_bits = root as u32;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_struct_size_is_4_bytes() {
        assert_eq!(core::mem::size_of::<Code>(), 4);
    }

    #[test]
    fn code_default_is_zero() {
        let c = Code::default();
        assert_eq!(c.op, 0);
        assert_eq!(c.bits, 0);
        assert_eq!(c.val, 0);
    }

    #[test]
    fn code_new_sets_fields() {
        let c = Code::new(16, 7, 258);
        assert_eq!(c.op, 16);
        assert_eq!(c.bits, 7);
        assert_eq!(c.val, 258);
    }

    #[test]
    fn code_is_copy() {
        let a = Code::new(1, 2, 3);
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn enough_constants() {
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);
    }

    #[test]
    fn static_table_lengths() {
        assert_eq!(LBASE.len(), 31);
        assert_eq!(LEXT.len(), 31);
        assert_eq!(DBASE.len(), 32);
        assert_eq!(DEXT.len(), 32);
    }

    #[test]
    fn lbase_spot_checks() {
        assert_eq!(LBASE[0], 3); // code 257 → base length 3
        assert_eq!(LBASE[12], 19); // code 269 → base length 19
        assert_eq!(LBASE[28], 258); // code 285 → base length 258
        assert_eq!(LBASE[29], 0); // sentinel
        assert_eq!(LBASE[30], 0); // sentinel
    }

    #[test]
    fn lext_spot_checks() {
        assert_eq!(LEXT[0], 16); // code 257 → op=16 (0 extra bits)
        assert_eq!(LEXT[8], 17); // code 265 → op=17 (1 extra bit)
        assert_eq!(LEXT[28], 16); // code 285 → op=16 (0 extra bits, special)
        assert_eq!(LEXT[29], 68); // sentinel → invalid (op & 64 != 0)
        assert_eq!(LEXT[30], 193); // sentinel → invalid (op & 64 != 0)
    }

    #[test]
    fn dbase_spot_checks() {
        assert_eq!(DBASE[0], 1); // code 0 → base distance 1
        assert_eq!(DBASE[4], 5); // code 4 → base distance 5
        assert_eq!(DBASE[29], 24577); // code 29 → base distance 24577
        assert_eq!(DBASE[30], 0); // sentinel
    }

    #[test]
    fn dext_spot_checks() {
        assert_eq!(DEXT[0], 16); // code 0 → op=16 (0 extra bits)
        assert_eq!(DEXT[4], 17); // code 4 → op=17 (1 extra bit)
        assert_eq!(DEXT[30], 64); // sentinel → invalid (op & 64 != 0)
        assert_eq!(DEXT[31], 64); // sentinel → invalid
    }

    /// Test inflate_table with all-zero lens (no codes): should produce 2
    /// invalid-marker entries and set root_bits to 1.
    #[test]
    fn inflate_table_no_codes() {
        let lens = [0u16; 19];
        let mut table = vec![Code::default(); 1024];
        let mut offset = 0usize;
        let mut bits = 7u32;
        let mut work = [0u16; 19];

        let result =
            inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut offset, &mut bits, &mut work);
        assert!(result.is_ok());
        assert_eq!(bits, 1);
        assert_eq!(offset, 2);
        // Both entries should be invalid markers
        assert_eq!(table[0].op, 64);
        assert_eq!(table[0].bits, 1);
        assert_eq!(table[1].op, 64);
        assert_eq!(table[1].bits, 1);
    }

    /// Test inflate_table with over-subscribed code lengths.
    #[test]
    fn inflate_table_oversubscribed() {
        // Two symbols both with length 1 → over-subscribed (only one 1-bit code exists)
        let lens = [1u16, 1, 1];
        let mut table = vec![Code::default(); 1024];
        let mut offset = 0usize;
        let mut bits = 7u32;
        let mut work = [0u16; 3];

        let result =
            inflate_table(CodeType::Codes, &lens, 3, &mut table, &mut offset, &mut bits, &mut work);
        assert!(result.is_err());
    }

    /// Test inflate_table with a simple complete code set.
    #[test]
    fn inflate_table_simple_codes() {
        // Two symbols: symbol 0 has length 1, symbol 1 has length 1
        // This is a complete Huffman tree: 0→sym0, 1→sym1
        let lens = [1u16, 1];
        let mut table = vec![Code::default(); 1024];
        let mut offset = 0usize;
        let mut bits = 7u32;
        let mut work = [0u16; 2];

        let result =
            inflate_table(CodeType::Codes, &lens, 2, &mut table, &mut offset, &mut bits, &mut work);
        assert!(result.is_ok());
        assert_eq!(bits, 1); // root clamped to max=1
        assert_eq!(offset, 2); // 1 << 1 = 2 entries

        // Table should have entries for symbols 0 and 1
        // Bit-reversed: code 0 (bit 0) → sym 0, code 1 (bit 1) → sym 1
        assert_eq!(table[0].op, 0); // literal
        assert_eq!(table[0].bits, 1);
        assert_eq!(table[0].val, 0); // symbol 0
        assert_eq!(table[1].op, 0); // literal
        assert_eq!(table[1].bits, 1);
        assert_eq!(table[1].val, 1); // symbol 1
    }

    /// Test building fixed Huffman length/literal table (RFC 1951 §3.2.6).
    ///
    /// This replicates what `buildtables()` does in inftrees.c lines 327–348.
    #[test]
    fn inflate_table_fixed_lens() {
        let mut lens = [0u16; 288];
        // Fixed Huffman code lengths per RFC 1951:
        //   0–143: 8 bits
        // 144–255: 9 bits
        // 256–279: 7 bits
        // 280–287: 8 bits
        for i in 0..144 {
            lens[i] = 8;
        }
        for i in 144..256 {
            lens[i] = 9;
        }
        for i in 256..280 {
            lens[i] = 7;
        }
        for i in 280..288 {
            lens[i] = 8;
        }

        let mut table = vec![Code::default(); ENOUGH];
        let mut offset = 0usize;
        let mut bits = 9u32;
        let mut work = [0u16; 288];

        let result = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut table,
            &mut offset,
            &mut bits,
            &mut work,
        );
        assert!(result.is_ok());
        assert_eq!(bits, 9); // root stays at 9
        assert!(offset <= ENOUGH_LENS);
        // 512 root entries (1 << 9)
        assert!(offset >= 512);
    }

    /// Test building fixed Huffman distance table.
    #[test]
    fn inflate_table_fixed_dists() {
        let mut lens = [0u16; 32];
        for i in 0..32 {
            lens[i] = 5;
        }

        let mut table = vec![Code::default(); ENOUGH];
        let mut offset = 0usize;
        let mut bits = 5u32;
        let mut work = [0u16; 32];

        let result = inflate_table(
            CodeType::Dists,
            &lens,
            32,
            &mut table,
            &mut offset,
            &mut bits,
            &mut work,
        );
        assert!(result.is_ok());
        assert_eq!(bits, 5);
        assert_eq!(offset, 32); // 1 << 5 = 32 entries, all same length → no sub-tables
    }

    /// Test incomplete code set for CODES type (returns error per C inftrees.c
    /// line 146: `left > 0 && (type == CODES || max != 1)` returns -1).
    #[test]
    fn inflate_table_incomplete_codes_error() {
        // Single 1-bit code among 3 symbols → incomplete → error for CODES
        let lens = [1u16, 0, 0];
        let mut table = vec![Code::default(); 1024];
        let mut offset = 0usize;
        let mut bits = 7u32;
        let mut work = [0u16; 3];

        let result =
            inflate_table(CodeType::Codes, &lens, 3, &mut table, &mut offset, &mut bits, &mut work);
        assert!(result.is_err());
    }

    /// Test that LENS with single 1-bit code (max==1) is allowed even if incomplete.
    ///
    /// Per C inftrees.c line 146: `left > 0 && (type == CODES || max != 1)`
    /// When type=LENS and max=1, the condition is false → allowed.
    #[test]
    fn inflate_table_incomplete_lens_max1_allowed() {
        // Single 1-bit code for symbol 0, all others zero → left > 0 but max == 1
        let mut lens = [0u16; 288];
        lens[0] = 1;

        let mut table = vec![Code::default(); ENOUGH];
        let mut offset = 0usize;
        let mut bits = 9u32;
        let mut work = [0u16; 288];

        let result = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut table,
            &mut offset,
            &mut bits,
            &mut work,
        );
        // max=1, type=LENS → (type == CODES || max != 1) is (false || false) = false
        // So even with left > 0, no error is returned
        assert!(result.is_ok());
    }

    /// Test incomplete code set for LENS type (error, unless max == 1).
    #[test]
    fn inflate_table_incomplete_lens_error() {
        // Single 2-bit code among 4 symbols → incomplete
        let lens = [2u16, 0, 0, 0];
        let mut table = vec![Code::default(); 1024];
        let mut offset = 0usize;
        let mut bits = 9u32;
        let mut work = [0u16; 4];

        let result =
            inflate_table(CodeType::Lens, &lens, 4, &mut table, &mut offset, &mut bits, &mut work);
        assert!(result.is_err());
    }

    /// Verify that a single 1-bit code for LENS type is accepted (max == 1 exception).
    #[test]
    fn inflate_table_single_code_lens_ok() {
        // One symbol with length 1, rest zero → max=1, left=1, allowed
        let mut lens = [0u16; 286];
        lens[0] = 1;

        let mut table = vec![Code::default(); ENOUGH];
        let mut offset = 0usize;
        let mut bits = 9u32;
        let mut work = [0u16; 286];

        let result = inflate_table(
            CodeType::Lens,
            &lens,
            286,
            &mut table,
            &mut offset,
            &mut bits,
            &mut work,
        );
        assert!(result.is_ok());
    }

    /// Ensure CodeType enum has exactly 3 variants by exhaustive match.
    #[test]
    fn codetype_exhaustive() {
        let types = [CodeType::Codes, CodeType::Lens, CodeType::Dists];
        for t in &types {
            match t {
                CodeType::Codes => {}
                CodeType::Lens => {}
                CodeType::Dists => {}
            }
        }
    }
}
