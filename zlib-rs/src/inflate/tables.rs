//! Huffman decode-table construction for the `inflate` decompressor.
//!
//! This is a safe, `no_std`-clean Rust port of zlib's `inftrees.h` and
//! `inftrees.c` (zlib 1.3.2.1). It is the foundational leaf of the `inflate`
//! module: it defines the [`Code`] decode-table entry and the [`inflate_table`]
//! builder that `fixed.rs`, `state.rs`, `fast.rs`, and `back.rs` all consume.
//!
//! # Bit-exactness
//!
//! The table-building algorithm reproduces the C implementation step-for-step so
//! that the generated decode tables are *byte-identical* to those produced by C
//! zlib. Table layout drives every downstream decode decision, so any deviation
//! would silently corrupt decompression. The `tests` module reconstructs the
//! fixed literal/length and distance tables and cross-checks every entry against
//! the canonical `inffixed.h` contents — the strongest possible local correctness
//! assertion.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`** (the crate declares
//! `#![forbid(unsafe_code)]`). The C `code **table` "running fill cursor" idiom is
//! translated to an index-offset scheme over a caller-owned `&mut [Code]` slice:
//! the function returns the number of entries it consumed (the amount by which the
//! caller advances its fill cursor), and every stored table-link `val` is an
//! offset *relative to the start of the provided slice*, exactly as in C
//! (`val = next - *table`).

/// A single Huffman decode-table entry.
///
/// Each entry either describes the operation requested by the code that indexed
/// it (literal, length, distance, end-of-block, or invalid code) or links to a
/// sub-table that decodes more bits of the code. This mirrors the C `code` struct
/// (`inftrees.h`) and is `#[repr(C)]` and exactly four bytes wide so its layout
/// matches the decode tables shared across the FFI boundary.
///
/// `op` bit-encoding (as set by [`inflate_table`], from `inftrees.h`):
///
/// - `00000000` — literal
/// - `0000tttt` — table link; `tttt != 0` is the number of sub-table index bits
/// - `0001eeee` — length or distance; `eeee` is the number of extra bits
/// - `01100000` (`0x60`) — end of block
/// - `01000000` (`0x40`) — invalid code
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Code {
    /// operation, extra bits, table bits
    pub op: u8,
    /// bits in this part of the code
    pub bits: u8,
    /// offset in table or code value
    pub val: u16,
}

// `Code` must be exactly four bytes to match the C decode-table layout
// (`inftrees.h`: "Each entry is four bytes.").
const _: () = assert!(core::mem::size_of::<Code>() == 4);

/// The kind of canonical Huffman code [`inflate_table`] should build.
///
/// Ports the C `codetype` enum (`inftrees.h`). The order is not an ABI contract;
/// it only selects the base/extra tables and the literal/length/distance split
/// inside the builder.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CodeType {
    /// The 19-symbol code-length meta-code (`CODES`).
    Codes,
    /// The literal/length code (`LENS`).
    Lens,
    /// The distance code (`DISTS`).
    Dists,
}

/// Maximum number of literal/length decode-table entries.
///
/// Found via `enough 286 9 15` using zlib's `examples/enough.c`: with a root
/// table of 9 index bits and a maximum code length of 15, no more than 852
/// entries are ever required. This depends on the root table size (9) selected by
/// the literal/length `inflate_table` calls in `state.rs`/`back.rs`.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of distance decode-table entries.
///
/// Found via `enough 30 6 15`: with a root table of 6 index bits and a maximum
/// code length of 15, no more than 592 entries are ever required. This depends on
/// the root table size (6) selected by the distance `inflate_table` calls in
/// `state.rs`/`back.rs`.
pub const ENOUGH_DISTS: usize = 592;

/// Maximum size of the combined dynamic decode table
/// (`ENOUGH_LENS + ENOUGH_DISTS` = 1444).
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

/// Maximum supported code length, in bits (`inftrees.c`: `#define MAXBITS 15`).
const MAXBITS: usize = 15;

/// Length codes 257..285 base values (`inftrees.c`). The trailing two zeros pad
/// the table to a 31-entry width and are never read for a valid length symbol.
static LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// Length codes 257..285 extra-bit op bytes (`inftrees.c`). Values `16..=21`
/// encode `0x10 | extra_bits`; the trailing `16, 68, 193` are the over-length
/// sentinels for the reserved symbols 286/287 (intentional, not typos).
static LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Distance codes 0..29 base values (`inftrees.c`). The trailing two zeros pad the
/// table to a 32-entry width and are never read for a valid distance symbol.
static DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Distance codes 0..29 extra-bit op bytes (`inftrees.c`). Values `16..=29` encode
/// `0x10 | extra_bits`; the trailing `64, 64` are invalid-code sentinels for the
/// reserved distance symbols 30/31.
static DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

/// Error returned by [`inflate_table`].
///
/// Maps the C integer return codes other than success: the C function returns
/// `-1` for a malformed code and `+1` when the requested table would exceed the
/// `ENOUGH` budget. Callers (`state.rs`/`back.rs`) map *both* variants to the
/// `BAD` inflate mode (`Z_DATA_ERROR`); the two variants are retained to aid
/// debugging.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InflateTableError {
    /// The supplied code lengths describe an over-subscribed code, or an
    /// illegally incomplete one (incomplete codes are rejected for `CODES`, and
    /// for `LENS`/`DISTS` with more than a single one-bit code). Corresponds to
    /// the C return value `-1`.
    Invalid,
    /// Building the requested table would require more than `ENOUGH_LENS` /
    /// `ENOUGH_DISTS` entries. Corresponds to the C return value `+1`.
    NotEnough,
}

/// Build a set of tables to decode the provided canonical Huffman code.
///
/// Direct port of C `inflate_table` (`inftrees.c`). The code lengths are
/// `lens[0..codes]`, each in `0..=15` (a length of 0 means "symbol absent"); the
/// caller must guarantee this range, exactly as the C routine assumes but does not
/// check. `work` is scratch space of at least `codes` entries used to sort the
/// symbols by length.
///
/// `table` is the destination slice **beginning at the current fill position**
/// (the caller passes `&mut codes[next..]`). On success the function returns the
/// number of entries it consumed; the caller advances its fill cursor (`next`) by
/// that amount. The requested root index-bit count is read from `*bits`, and the
/// *actual* root index-bit count is written back to `*bits` (it differs when the
/// request exceeds the longest code or is below the shortest code).
///
/// Every table-link `val` written into the root table is an offset *relative to
/// `table[0]`* (matching C's `next - *table`), because the caller sets its decode
/// base index to the same `table[0]` position before calling.
///
/// # Errors
///
/// Returns [`InflateTableError::Invalid`] for an over-subscribed or illegally
/// incomplete code, and [`InflateTableError::NotEnough`] if the table would exceed
/// the `ENOUGH` budget for its type.
pub fn inflate_table(
    code_type: CodeType,
    lens: &[u16],
    codes: usize,
    table: &mut [Code],
    bits: &mut usize,
    work: &mut [u16],
) -> Result<usize, InflateTableError> {
    // --- Defensive input validation (security hardening) ---
    //
    // `inflate_table` is a public, safe function and is ultimately reachable
    // with attacker-controlled code lengths (the dynamic-Huffman header of a
    // compressed stream). It must therefore reject every malformed input by
    // *returning* an error rather than panicking on a bounds-checked index or
    // an arithmetic overflow — either of which would be a remote
    // denial-of-service. The C routine documents these as caller preconditions
    // and never checks them; we validate them up front, before any indexing or
    // arithmetic.
    //
    // The per-type symbol-count ceiling matches the DEFLATE alphabets: 19
    // code-length symbols, up to 288 literal/length symbols (286 used plus the
    // two reserved 286/287 that the fixed table includes), and up to 32 distance
    // symbols (30 used plus two reserved). Bounding `codes` also keeps the
    // per-length `count` tallies within `u16`.
    let max_codes = match code_type {
        CodeType::Codes => 19,
        CodeType::Lens => 288,
        CodeType::Dists => 32,
    };
    if codes > max_codes || codes > lens.len() || work.len() < codes {
        return Err(InflateTableError::Invalid);
    }
    // Every supplied code length must be in `0..=MAXBITS`; a longer length would
    // index past the `count` / `offs` arrays below.
    if lens[..codes].iter().any(|&len| len as usize > MAXBITS) {
        return Err(InflateTableError::Invalid);
    }

    // Accumulate a count of codes of each length. Both preconditions the C code
    // merely assumes — `len <= MAXBITS` and `codes <= lens.len()` — are enforced
    // above, so the indexing here cannot panic.
    let mut count = [0u16; MAXBITS + 1];
    for &len_val in &lens[..codes] {
        count[len_val as usize] += 1;
    }

    // Bound the requested root index bits to the actual range of code lengths.
    // `max` is the longest code length present (0 if there are no codes at all).
    let mut root = *bits;
    let max = (1..=MAXBITS)
        .rev()
        .find(|&len| count[len] != 0)
        .unwrap_or(0);
    if root > max {
        root = max;
    }
    if max == 0 {
        // No symbols to code at all: build a two-entry table whose entries force
        // an "invalid code" error, but wait for decoding to report it. This
        // mirrors the empty-code special case in the C source.
        if table.len() < 2 {
            return Err(InflateTableError::NotEnough);
        }
        let here = Code {
            op: 64, // invalid code marker
            bits: 1,
            val: 0,
        };
        table[0] = here;
        table[1] = here;
        *bits = 1;
        return Ok(2);
    }
    // `min` is the shortest code length present (defaulting to `max` when every
    // length below `max` is absent, mirroring the C loop's terminal value).
    let mut min = (1..max).find(|&len| count[len] != 0).unwrap_or(max);
    if root < min {
        root = min;
    }

    // Check for an over-subscribed or incomplete set of lengths. `left` is the
    // number of prefix codes still available and *must* be signed: it legitimately
    // goes negative for an over-subscribed code.
    let mut left: i32 = 1;
    for &cnt in &count[1..=MAXBITS] {
        left <<= 1;
        left -= cnt as i32;
        if left < 0 {
            return Err(InflateTableError::Invalid); // over-subscribed
        }
    }
    if left > 0 && (code_type == CodeType::Codes || max != 1) {
        // Incomplete set. The `max != 1` exception deliberately permits an
        // incomplete LENS/DISTS code that consists of a single one-bit code,
        // which occurs in valid raw/edge streams.
        return Err(InflateTableError::Invalid);
    }

    // Generate offsets into the symbol table for each length, for sorting.
    let mut offs = [0u16; MAXBITS + 1];
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }

    // Sort symbols by length, preserving symbol order within each length.
    for (sym, &len_code) in lens.iter().enumerate().take(codes) {
        let len = len_code as usize;
        if len != 0 {
            work[offs[len] as usize] = sym as u16;
            offs[len] += 1;
        }
    }

    // Set up the base/extra tables and the literal/length/distance split point for
    // this code type. For `Codes`, `match_ == 20` and every code-length symbol is
    // `< 20`, so the length/distance arm is never taken and the empty base/extra
    // slices are never indexed.
    let (base, extra, match_): (&[u16], &[u16], usize) = match code_type {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // Initialize state for the fill loop.
    let mut huff: u32 = 0; // starting code
    let mut sym = 0usize; // starting code symbol
    let mut len = min; // starting code length
    let mut next = 0usize; // offset of current sub-table (relative to table[0])
    let mut curr = root; // current table index bits
    let mut drop_bits = 0usize; // current bits to drop from the code for an index
    let mut low: u32 = u32::MAX; // trigger a new sub-table when len > root
    let mut used = 1usize << root; // root table entries consumed so far
    let mask: u32 = (used - 1) as u32; // mask for comparing the low root bits

    // Check available table space against the ENOUGH budget for this type, and
    // against the actual capacity of the caller-supplied `table`. The latter
    // guards every `table[..]` write below: all of them use an index `< used`.
    if (code_type == CodeType::Lens && used > ENOUGH_LENS)
        || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
        || used > table.len()
    {
        return Err(InflateTableError::NotEnough);
    }

    // Process all codes and make table entries.
    loop {
        // Create the table entry for the current symbol.
        let w = work[sym];
        let sym_val = w as usize;
        let here_bits = (len - drop_bits) as u8;
        let (here_op, here_val): (u8, u16) = if sym_val + 1 < match_ {
            (0, w) // literal
        } else if sym_val >= match_ {
            // length or distance: op carries the extra-bit count, val the base.
            (extra[sym_val - match_] as u8, base[sym_val - match_])
        } else {
            (32 + 64, 0) // end of block
        };
        let here = Code {
            op: here_op,
            bits: here_bits,
            val: here_val,
        };

        // Replicate the entry for every index whose low `len` bits equal `huff`.
        let mut incr = 1u32 << (len - drop_bits);
        let mut fill = 1u32 << curr;
        min = fill as usize; // reuse `min` to save the size of the current table
        loop {
            fill -= incr;
            table[next + ((huff >> drop_bits) + fill) as usize] = here;
            if fill == 0 {
                break;
            }
        }

        // Backwards-increment the `len`-bit code `huff`.
        incr = 1u32 << (len - 1);
        while (huff & incr) != 0 {
            incr >>= 1;
        }
        if incr != 0 {
            huff &= incr - 1;
            huff += incr;
        } else {
            huff = 0;
        }

        // Advance to the next symbol, updating the length count and current length.
        sym += 1;
        count[len] -= 1;
        if count[len] == 0 {
            if len == max {
                break;
            }
            len = lens[work[sym] as usize] as usize;
        }

        // Create a new sub-table if the current code is longer than the root table
        // and we have moved to a new root-index group.
        if len > root && (huff & mask) != low {
            // On the first transition, start dropping the root bits.
            if drop_bits == 0 {
                drop_bits = root;
            }

            // Advance past the table just filled (`min` holds `1 << curr`).
            next += min;

            // Determine the length of the next sub-table by looking ahead at the
            // remaining length counts.
            curr = len - drop_bits;
            left = 1i32 << curr;
            while curr + drop_bits < max {
                left -= count[curr + drop_bits] as i32;
                if left <= 0 {
                    break;
                }
                curr += 1;
                left <<= 1;
            }

            // Check that the growing table still fits the ENOUGH budget and the
            // caller-supplied `table` capacity (sub-table writes also index
            // `< used`).
            used += 1usize << curr;
            if (code_type == CodeType::Lens && used > ENOUGH_LENS)
                || (code_type == CodeType::Dists && used > ENOUGH_DISTS)
                || used > table.len()
            {
                return Err(InflateTableError::NotEnough);
            }

            // Point the entry in the root table at the new sub-table. `val` is the
            // sub-table offset relative to table[0].
            low = huff & mask;
            table[low as usize].op = curr as u8;
            table[low as usize].bits = root as u8;
            table[low as usize].val = next as u16;
        }
    }

    // Fill in the one remaining entry if the code is incomplete. It is guaranteed
    // to be at most a single entry, because an incomplete code that reaches here
    // has a maximum allowed length of one bit.
    if huff != 0 {
        let here = Code {
            op: 64, // invalid code marker
            bits: (len - drop_bits) as u8,
            val: 0,
        };
        table[next + huff as usize] = here;
    }

    // Set the return parameters: write back the actual root bits and report the
    // total number of entries consumed.
    *bits = root;
    Ok(used)
}

// NOTE ON SCOPE BOUNDARY (AAP §0.2.3):
//
// The C `inflate_fixed()` helper (inftrees.c L364-372) merely assigns the
// precomputed fixed tables and their index sizes into an `inflate_state`; it
// belongs to `state.rs`/`back.rs`, which own that struct, and is therefore NOT
// defined here. Likewise the `BUILDFIXED`/`MAKEFIXED`/`buildtables`/`main`
// build-time table generator (inftrees.c L313-424) is explicitly out of scope —
// the fixed `LENFIX`/`DISTFIX` tables are carried verbatim in `fixed.rs`. This
// module deliberately contains only the decode-table entry type and the
// `inflate_table` builder.

#[cfg(test)]
mod tests {
    use super::*;

    /// Concise constructor for an expected [`Code`] entry, matching the
    /// `{op, bits, val}` triples printed by zlib's `makefixed()`.
    const fn c(op: u8, bits: u8, val: u16) -> Code {
        Code { op, bits, val }
    }

    // Expected fixed literal/length decode table.
    //
    // These are the values produced by the *real* C `inflate_table` for the fixed
    // literal/length code, verified by compiling zlib's own `inftrees.c` and
    // dumping every entry. They equal the canonical `inffixed.h` `lenfix[512]`
    // table at every index EXCEPT the four positions where `(index & 127) == 99`
    // (indices 99, 227, 355, 483). At those four positions `makefixed()`
    // deliberately overrides the op byte to 64 via the
    // `(low & 127) == 99 ? 64 : ...` expression in `inftrees.c`, whereas the live
    // `inflate_table` emits the length-code over-length sentinels for the reserved
    // symbols 286/287 (op 68 and 193, from `LEXT[29]`/`LEXT[30]`). This table
    // therefore asserts byte-for-byte parity with the live C `inflate_table`.
    const LENFIX_EXPECTED: [Code; 512] = [
        c(96, 7, 0),
        c(0, 8, 80),
        c(0, 8, 16),
        c(20, 8, 115),
        c(18, 7, 31),
        c(0, 8, 112),
        c(0, 8, 48),
        c(0, 9, 192),
        c(16, 7, 10),
        c(0, 8, 96),
        c(0, 8, 32),
        c(0, 9, 160),
        c(0, 8, 0),
        c(0, 8, 128),
        c(0, 8, 64),
        c(0, 9, 224),
        c(16, 7, 6),
        c(0, 8, 88),
        c(0, 8, 24),
        c(0, 9, 144),
        c(19, 7, 59),
        c(0, 8, 120),
        c(0, 8, 56),
        c(0, 9, 208),
        c(17, 7, 17),
        c(0, 8, 104),
        c(0, 8, 40),
        c(0, 9, 176),
        c(0, 8, 8),
        c(0, 8, 136),
        c(0, 8, 72),
        c(0, 9, 240),
        c(16, 7, 4),
        c(0, 8, 84),
        c(0, 8, 20),
        c(21, 8, 227),
        c(19, 7, 43),
        c(0, 8, 116),
        c(0, 8, 52),
        c(0, 9, 200),
        c(17, 7, 13),
        c(0, 8, 100),
        c(0, 8, 36),
        c(0, 9, 168),
        c(0, 8, 4),
        c(0, 8, 132),
        c(0, 8, 68),
        c(0, 9, 232),
        c(16, 7, 8),
        c(0, 8, 92),
        c(0, 8, 28),
        c(0, 9, 152),
        c(20, 7, 83),
        c(0, 8, 124),
        c(0, 8, 60),
        c(0, 9, 216),
        c(18, 7, 23),
        c(0, 8, 108),
        c(0, 8, 44),
        c(0, 9, 184),
        c(0, 8, 12),
        c(0, 8, 140),
        c(0, 8, 76),
        c(0, 9, 248),
        c(16, 7, 3),
        c(0, 8, 82),
        c(0, 8, 18),
        c(21, 8, 163),
        c(19, 7, 35),
        c(0, 8, 114),
        c(0, 8, 50),
        c(0, 9, 196),
        c(17, 7, 11),
        c(0, 8, 98),
        c(0, 8, 34),
        c(0, 9, 164),
        c(0, 8, 2),
        c(0, 8, 130),
        c(0, 8, 66),
        c(0, 9, 228),
        c(16, 7, 7),
        c(0, 8, 90),
        c(0, 8, 26),
        c(0, 9, 148),
        c(20, 7, 67),
        c(0, 8, 122),
        c(0, 8, 58),
        c(0, 9, 212),
        c(18, 7, 19),
        c(0, 8, 106),
        c(0, 8, 42),
        c(0, 9, 180),
        c(0, 8, 10),
        c(0, 8, 138),
        c(0, 8, 74),
        c(0, 9, 244),
        c(16, 7, 5),
        c(0, 8, 86),
        c(0, 8, 22),
        c(68, 8, 0),
        c(19, 7, 51),
        c(0, 8, 118),
        c(0, 8, 54),
        c(0, 9, 204),
        c(17, 7, 15),
        c(0, 8, 102),
        c(0, 8, 38),
        c(0, 9, 172),
        c(0, 8, 6),
        c(0, 8, 134),
        c(0, 8, 70),
        c(0, 9, 236),
        c(16, 7, 9),
        c(0, 8, 94),
        c(0, 8, 30),
        c(0, 9, 156),
        c(20, 7, 99),
        c(0, 8, 126),
        c(0, 8, 62),
        c(0, 9, 220),
        c(18, 7, 27),
        c(0, 8, 110),
        c(0, 8, 46),
        c(0, 9, 188),
        c(0, 8, 14),
        c(0, 8, 142),
        c(0, 8, 78),
        c(0, 9, 252),
        c(96, 7, 0),
        c(0, 8, 81),
        c(0, 8, 17),
        c(21, 8, 131),
        c(18, 7, 31),
        c(0, 8, 113),
        c(0, 8, 49),
        c(0, 9, 194),
        c(16, 7, 10),
        c(0, 8, 97),
        c(0, 8, 33),
        c(0, 9, 162),
        c(0, 8, 1),
        c(0, 8, 129),
        c(0, 8, 65),
        c(0, 9, 226),
        c(16, 7, 6),
        c(0, 8, 89),
        c(0, 8, 25),
        c(0, 9, 146),
        c(19, 7, 59),
        c(0, 8, 121),
        c(0, 8, 57),
        c(0, 9, 210),
        c(17, 7, 17),
        c(0, 8, 105),
        c(0, 8, 41),
        c(0, 9, 178),
        c(0, 8, 9),
        c(0, 8, 137),
        c(0, 8, 73),
        c(0, 9, 242),
        c(16, 7, 4),
        c(0, 8, 85),
        c(0, 8, 21),
        c(16, 8, 258),
        c(19, 7, 43),
        c(0, 8, 117),
        c(0, 8, 53),
        c(0, 9, 202),
        c(17, 7, 13),
        c(0, 8, 101),
        c(0, 8, 37),
        c(0, 9, 170),
        c(0, 8, 5),
        c(0, 8, 133),
        c(0, 8, 69),
        c(0, 9, 234),
        c(16, 7, 8),
        c(0, 8, 93),
        c(0, 8, 29),
        c(0, 9, 154),
        c(20, 7, 83),
        c(0, 8, 125),
        c(0, 8, 61),
        c(0, 9, 218),
        c(18, 7, 23),
        c(0, 8, 109),
        c(0, 8, 45),
        c(0, 9, 186),
        c(0, 8, 13),
        c(0, 8, 141),
        c(0, 8, 77),
        c(0, 9, 250),
        c(16, 7, 3),
        c(0, 8, 83),
        c(0, 8, 19),
        c(21, 8, 195),
        c(19, 7, 35),
        c(0, 8, 115),
        c(0, 8, 51),
        c(0, 9, 198),
        c(17, 7, 11),
        c(0, 8, 99),
        c(0, 8, 35),
        c(0, 9, 166),
        c(0, 8, 3),
        c(0, 8, 131),
        c(0, 8, 67),
        c(0, 9, 230),
        c(16, 7, 7),
        c(0, 8, 91),
        c(0, 8, 27),
        c(0, 9, 150),
        c(20, 7, 67),
        c(0, 8, 123),
        c(0, 8, 59),
        c(0, 9, 214),
        c(18, 7, 19),
        c(0, 8, 107),
        c(0, 8, 43),
        c(0, 9, 182),
        c(0, 8, 11),
        c(0, 8, 139),
        c(0, 8, 75),
        c(0, 9, 246),
        c(16, 7, 5),
        c(0, 8, 87),
        c(0, 8, 23),
        c(193, 8, 0),
        c(19, 7, 51),
        c(0, 8, 119),
        c(0, 8, 55),
        c(0, 9, 206),
        c(17, 7, 15),
        c(0, 8, 103),
        c(0, 8, 39),
        c(0, 9, 174),
        c(0, 8, 7),
        c(0, 8, 135),
        c(0, 8, 71),
        c(0, 9, 238),
        c(16, 7, 9),
        c(0, 8, 95),
        c(0, 8, 31),
        c(0, 9, 158),
        c(20, 7, 99),
        c(0, 8, 127),
        c(0, 8, 63),
        c(0, 9, 222),
        c(18, 7, 27),
        c(0, 8, 111),
        c(0, 8, 47),
        c(0, 9, 190),
        c(0, 8, 15),
        c(0, 8, 143),
        c(0, 8, 79),
        c(0, 9, 254),
        c(96, 7, 0),
        c(0, 8, 80),
        c(0, 8, 16),
        c(20, 8, 115),
        c(18, 7, 31),
        c(0, 8, 112),
        c(0, 8, 48),
        c(0, 9, 193),
        c(16, 7, 10),
        c(0, 8, 96),
        c(0, 8, 32),
        c(0, 9, 161),
        c(0, 8, 0),
        c(0, 8, 128),
        c(0, 8, 64),
        c(0, 9, 225),
        c(16, 7, 6),
        c(0, 8, 88),
        c(0, 8, 24),
        c(0, 9, 145),
        c(19, 7, 59),
        c(0, 8, 120),
        c(0, 8, 56),
        c(0, 9, 209),
        c(17, 7, 17),
        c(0, 8, 104),
        c(0, 8, 40),
        c(0, 9, 177),
        c(0, 8, 8),
        c(0, 8, 136),
        c(0, 8, 72),
        c(0, 9, 241),
        c(16, 7, 4),
        c(0, 8, 84),
        c(0, 8, 20),
        c(21, 8, 227),
        c(19, 7, 43),
        c(0, 8, 116),
        c(0, 8, 52),
        c(0, 9, 201),
        c(17, 7, 13),
        c(0, 8, 100),
        c(0, 8, 36),
        c(0, 9, 169),
        c(0, 8, 4),
        c(0, 8, 132),
        c(0, 8, 68),
        c(0, 9, 233),
        c(16, 7, 8),
        c(0, 8, 92),
        c(0, 8, 28),
        c(0, 9, 153),
        c(20, 7, 83),
        c(0, 8, 124),
        c(0, 8, 60),
        c(0, 9, 217),
        c(18, 7, 23),
        c(0, 8, 108),
        c(0, 8, 44),
        c(0, 9, 185),
        c(0, 8, 12),
        c(0, 8, 140),
        c(0, 8, 76),
        c(0, 9, 249),
        c(16, 7, 3),
        c(0, 8, 82),
        c(0, 8, 18),
        c(21, 8, 163),
        c(19, 7, 35),
        c(0, 8, 114),
        c(0, 8, 50),
        c(0, 9, 197),
        c(17, 7, 11),
        c(0, 8, 98),
        c(0, 8, 34),
        c(0, 9, 165),
        c(0, 8, 2),
        c(0, 8, 130),
        c(0, 8, 66),
        c(0, 9, 229),
        c(16, 7, 7),
        c(0, 8, 90),
        c(0, 8, 26),
        c(0, 9, 149),
        c(20, 7, 67),
        c(0, 8, 122),
        c(0, 8, 58),
        c(0, 9, 213),
        c(18, 7, 19),
        c(0, 8, 106),
        c(0, 8, 42),
        c(0, 9, 181),
        c(0, 8, 10),
        c(0, 8, 138),
        c(0, 8, 74),
        c(0, 9, 245),
        c(16, 7, 5),
        c(0, 8, 86),
        c(0, 8, 22),
        c(68, 8, 0),
        c(19, 7, 51),
        c(0, 8, 118),
        c(0, 8, 54),
        c(0, 9, 205),
        c(17, 7, 15),
        c(0, 8, 102),
        c(0, 8, 38),
        c(0, 9, 173),
        c(0, 8, 6),
        c(0, 8, 134),
        c(0, 8, 70),
        c(0, 9, 237),
        c(16, 7, 9),
        c(0, 8, 94),
        c(0, 8, 30),
        c(0, 9, 157),
        c(20, 7, 99),
        c(0, 8, 126),
        c(0, 8, 62),
        c(0, 9, 221),
        c(18, 7, 27),
        c(0, 8, 110),
        c(0, 8, 46),
        c(0, 9, 189),
        c(0, 8, 14),
        c(0, 8, 142),
        c(0, 8, 78),
        c(0, 9, 253),
        c(96, 7, 0),
        c(0, 8, 81),
        c(0, 8, 17),
        c(21, 8, 131),
        c(18, 7, 31),
        c(0, 8, 113),
        c(0, 8, 49),
        c(0, 9, 195),
        c(16, 7, 10),
        c(0, 8, 97),
        c(0, 8, 33),
        c(0, 9, 163),
        c(0, 8, 1),
        c(0, 8, 129),
        c(0, 8, 65),
        c(0, 9, 227),
        c(16, 7, 6),
        c(0, 8, 89),
        c(0, 8, 25),
        c(0, 9, 147),
        c(19, 7, 59),
        c(0, 8, 121),
        c(0, 8, 57),
        c(0, 9, 211),
        c(17, 7, 17),
        c(0, 8, 105),
        c(0, 8, 41),
        c(0, 9, 179),
        c(0, 8, 9),
        c(0, 8, 137),
        c(0, 8, 73),
        c(0, 9, 243),
        c(16, 7, 4),
        c(0, 8, 85),
        c(0, 8, 21),
        c(16, 8, 258),
        c(19, 7, 43),
        c(0, 8, 117),
        c(0, 8, 53),
        c(0, 9, 203),
        c(17, 7, 13),
        c(0, 8, 101),
        c(0, 8, 37),
        c(0, 9, 171),
        c(0, 8, 5),
        c(0, 8, 133),
        c(0, 8, 69),
        c(0, 9, 235),
        c(16, 7, 8),
        c(0, 8, 93),
        c(0, 8, 29),
        c(0, 9, 155),
        c(20, 7, 83),
        c(0, 8, 125),
        c(0, 8, 61),
        c(0, 9, 219),
        c(18, 7, 23),
        c(0, 8, 109),
        c(0, 8, 45),
        c(0, 9, 187),
        c(0, 8, 13),
        c(0, 8, 141),
        c(0, 8, 77),
        c(0, 9, 251),
        c(16, 7, 3),
        c(0, 8, 83),
        c(0, 8, 19),
        c(21, 8, 195),
        c(19, 7, 35),
        c(0, 8, 115),
        c(0, 8, 51),
        c(0, 9, 199),
        c(17, 7, 11),
        c(0, 8, 99),
        c(0, 8, 35),
        c(0, 9, 167),
        c(0, 8, 3),
        c(0, 8, 131),
        c(0, 8, 67),
        c(0, 9, 231),
        c(16, 7, 7),
        c(0, 8, 91),
        c(0, 8, 27),
        c(0, 9, 151),
        c(20, 7, 67),
        c(0, 8, 123),
        c(0, 8, 59),
        c(0, 9, 215),
        c(18, 7, 19),
        c(0, 8, 107),
        c(0, 8, 43),
        c(0, 9, 183),
        c(0, 8, 11),
        c(0, 8, 139),
        c(0, 8, 75),
        c(0, 9, 247),
        c(16, 7, 5),
        c(0, 8, 87),
        c(0, 8, 23),
        c(193, 8, 0),
        c(19, 7, 51),
        c(0, 8, 119),
        c(0, 8, 55),
        c(0, 9, 207),
        c(17, 7, 15),
        c(0, 8, 103),
        c(0, 8, 39),
        c(0, 9, 175),
        c(0, 8, 7),
        c(0, 8, 135),
        c(0, 8, 71),
        c(0, 9, 239),
        c(16, 7, 9),
        c(0, 8, 95),
        c(0, 8, 31),
        c(0, 9, 159),
        c(20, 7, 99),
        c(0, 8, 127),
        c(0, 8, 63),
        c(0, 9, 223),
        c(18, 7, 27),
        c(0, 8, 111),
        c(0, 8, 47),
        c(0, 9, 191),
        c(0, 8, 15),
        c(0, 8, 143),
        c(0, 8, 79),
        c(0, 9, 255),
    ];

    // Expected fixed distance decode table, identical to both the live C
    // `inflate_table` output and the canonical `inffixed.h` `distfix[32]` (the
    // distance table is not subject to any `makefixed()` override).
    const DISTFIX_EXPECTED: [Code; 32] = [
        c(16, 5, 1),
        c(23, 5, 257),
        c(19, 5, 17),
        c(27, 5, 4097),
        c(17, 5, 5),
        c(25, 5, 1025),
        c(21, 5, 65),
        c(29, 5, 16385),
        c(16, 5, 3),
        c(24, 5, 513),
        c(20, 5, 33),
        c(28, 5, 8193),
        c(18, 5, 9),
        c(26, 5, 2049),
        c(22, 5, 129),
        c(64, 5, 0),
        c(16, 5, 2),
        c(23, 5, 385),
        c(19, 5, 25),
        c(27, 5, 6145),
        c(17, 5, 7),
        c(25, 5, 1537),
        c(21, 5, 97),
        c(29, 5, 24577),
        c(16, 5, 4),
        c(24, 5, 769),
        c(20, 5, 49),
        c(28, 5, 12289),
        c(18, 5, 13),
        c(26, 5, 3073),
        c(22, 5, 193),
        c(64, 5, 0),
    ];

    /// Build the fixed literal/length code lengths (RFC 1951 section 3.2.6, also
    /// mirrored by `inftrees.c` `buildtables`): 8 bits for symbols 0..144, 9 bits
    /// for 144..256, 7 bits for 256..280, and 8 bits for 280..288.
    fn fixed_litlen_lengths() -> [u16; 288] {
        let mut lens = [0u16; 288];
        lens[0..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..288].fill(8);
        lens
    }

    #[test]
    fn code_layout_is_four_bytes_repr_c() {
        assert_eq!(core::mem::size_of::<Code>(), 4);
        assert_eq!(core::mem::align_of::<Code>(), 2);
        assert_eq!(Code::default(), c(0, 0, 0));
    }

    #[test]
    fn enough_constants_match_zlib() {
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
    }

    #[test]
    fn fixed_litlen_table_matches_c_inflate_table() {
        let lens = fixed_litlen_lengths();
        let mut table = [Code::default(); ENOUGH_LENS];
        let mut work = [0u16; 288];
        let mut bits = 9usize;
        let used = inflate_table(CodeType::Lens, &lens, 288, &mut table, &mut bits, &mut work)
            .expect("fixed literal/length table builds");
        assert_eq!(used, 512, "fixed lit/len table consumes 512 entries");
        assert_eq!(bits, 9, "fixed lit/len root index is 9 bits");
        assert_eq!(table[0], c(96, 7, 0));
        assert_eq!(table[511], c(0, 9, 255));
        assert_eq!(
            &table[..512],
            &LENFIX_EXPECTED[..],
            "reconstructed lit/len table must be byte-identical to the C inflate_table output"
        );
    }

    #[test]
    fn fixed_distance_table_matches_c_inflate_table() {
        let lens = [5u16; 32];
        let mut table = [Code::default(); ENOUGH_DISTS];
        let mut work = [0u16; 32];
        let mut bits = 5usize;
        let used = inflate_table(CodeType::Dists, &lens, 32, &mut table, &mut bits, &mut work)
            .expect("fixed distance table builds");
        assert_eq!(used, 32, "fixed distance table consumes 32 entries");
        assert_eq!(bits, 5, "fixed distance root index is 5 bits");
        assert_eq!(table[0], c(16, 5, 1));
        assert_eq!(table[31], c(64, 5, 0));
        assert_eq!(
            &table[..32],
            &DISTFIX_EXPECTED[..],
            "reconstructed distance table must be byte-identical to inffixed.h distfix"
        );
    }

    #[test]
    fn code_length_code_path_builds() {
        // A small, complete CODES code: four symbols of length 2.
        // Kraft sum = 4 * 2^-2 = 1, so the code is complete.
        let mut lens = [0u16; 19];
        lens[0..4].fill(2);
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let used = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work)
            .expect("code-length code builds");
        // The requested root (7) is bounded down to the maximum length (2).
        assert_eq!(bits, 2);
        assert_eq!(used, 4);
        // Every code-length symbol is below `match_` (20), so each entry is a
        // literal: op == 0, bits == 2, and val is one of the four symbols.
        for entry in &table[..used] {
            assert_eq!(entry.op, 0);
            assert_eq!(entry.bits, 2);
            assert!(entry.val < 4);
        }
    }

    #[test]
    fn over_subscribed_code_is_invalid() {
        // Three one-bit codes: Kraft sum = 3 * 2^-1 = 1.5 > 1 -> over-subscribed.
        let mut lens = [0u16; 19];
        lens[0..3].fill(1);
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn incomplete_codes_code_is_invalid() {
        // A single one-bit code under CODES is incomplete (left > 0) and must be
        // rejected (the `type == CODES` arm of the incomplete-set check).
        let mut lens = [0u16; 19];
        lens[0] = 1;
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn single_one_bit_lens_code_is_permitted() {
        // The deliberate `max != 1` exception: a single one-bit LENS code is an
        // incomplete code that is *allowed* (it occurs in valid raw/edge streams).
        let mut lens = [0u16; 288];
        lens[0] = 1;
        let mut table = [Code::default(); ENOUGH_LENS];
        let mut work = [0u16; 288];
        let mut bits = 9usize;
        let used = inflate_table(CodeType::Lens, &lens, 288, &mut table, &mut bits, &mut work)
            .expect("a single one-bit LENS code is permitted");
        assert_eq!(bits, 1, "root index forced down to 1 bit");
        assert_eq!(used, 2);
        assert_eq!(table[0], c(0, 1, 0), "literal symbol 0");
        assert_eq!(
            table[1],
            c(64, 1, 0),
            "invalid-code filler for the unused slot"
        );
    }

    #[test]
    fn empty_code_makes_forced_error_table() {
        // No symbols at all: the builder emits a two-entry table whose entries
        // force an "invalid code" error once decoding reaches them.
        let lens = [0u16; 19];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let used = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work)
            .expect("empty code returns a forced-error table");
        assert_eq!(used, 2);
        assert_eq!(bits, 1);
        assert_eq!(table[0], c(64, 1, 0));
        assert_eq!(table[1], c(64, 1, 0));
    }

    // ----- Malformed-input robustness (security hardening, finding #11) -----
    //
    // Each of the following exercises an input that, before validation was
    // added, would panic via a bounds-checked index or an arithmetic overflow.
    // Every one must now return an `InflateTableError` instead.

    #[test]
    fn codes_exceeding_lens_len_is_invalid() {
        // `codes` larger than the supplied `lens` slice must not panic on
        // `lens[..codes]`; it returns Invalid instead.
        let lens = [2u16; 4];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 5, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn work_smaller_than_codes_is_invalid() {
        // `work` must have room for at least `codes` entries (the sort step
        // indexes it up to the number of present symbols).
        let mut lens = [0u16; 19];
        lens[0..4].fill(2);
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 3]; // too small for codes == 19
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn length_above_maxbits_is_invalid() {
        // A code length greater than MAXBITS (15) would index past `count`; it
        // is rejected rather than panicking.
        let mut lens = [0u16; 19];
        lens[0] = (MAXBITS + 1) as u16; // 16
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn codes_beyond_type_maximum_is_invalid() {
        // CODES has only 19 symbols; a larger `codes` is rejected up front (this
        // also caps the per-length `count` tallies well within `u16`).
        let lens = [0u16; 64];
        let mut table = [Code::default(); ENOUGH];
        let mut work = [0u16; 64];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 20, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::Invalid));
    }

    #[test]
    fn table_too_small_is_not_enough() {
        // A valid complete code whose decode table does not fit the caller's
        // buffer must return NotEnough rather than panicking on a table write.
        let lens = fixed_litlen_lengths();
        let mut table = [Code::default(); 16]; // far smaller than the 512 needed
        let mut work = [0u16; 288];
        let mut bits = 9usize;
        let res = inflate_table(CodeType::Lens, &lens, 288, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::NotEnough));
    }

    #[test]
    fn empty_table_too_small_is_not_enough() {
        // The forced-error empty table needs two entries; a one-entry buffer is
        // NotEnough, not a panic.
        let lens = [0u16; 19];
        let mut table = [Code::default(); 1];
        let mut work = [0u16; 19];
        let mut bits = 7usize;
        let res = inflate_table(CodeType::Codes, &lens, 19, &mut table, &mut bits, &mut work);
        assert_eq!(res, Err(InflateTableError::NotEnough));
    }
}
