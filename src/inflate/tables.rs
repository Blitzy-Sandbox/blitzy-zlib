//! Huffman decode-table construction for the inflate engine.
//!
//! This module is a safe-Rust port of the C file `inftrees.c` (and its header
//! `inftrees.h`) from zlib 1.3.2.1. It is **foundational** for the entire
//! inflate engine: it owns the canonical decode-table entry type [`Code`], the
//! table builder `inflate_table`, and the fixed-table installer
//! `inflate_fixed`.
//!
//! # Byte-identical construction
//!
//! The tables produced here must be **bit-for-bit identical** to the tables
//! produced by canonical C zlib, because the hot decode loops in
//! `crate::inflate::fast`, `crate::inflate::mod`, and `crate::inflate::back`
//! index directly into them. Any divergence — a different entry value, a
//! different fill order, or a different sub-table link — would corrupt every
//! dynamic-block and fixed-block decode. The algorithm below therefore mirrors
//! the C implementation step for step, with the C `code **table`
//! pointer-to-pointer replaced by an integer cursor (`next`) into the caller's
//! shared `codes` buffer.
//!
//! # Safety
//!
//! This module is **100% safe Rust** — there are no `unsafe` blocks. The C
//! pointer arithmetic is expressed entirely with bounds-checked slice indexing
//! and fixed-size stack arrays, so it is also `no_std`-clean (it uses only
//! `core`, never `std`, and performs no heap allocation).

/// Decoding-table entry.
///
/// `#[repr(C)]` guarantees the in-memory layout matches the C `code` struct
/// exactly (four bytes: `op`, `bits`, then a two-byte `val`) so that:
///
/// * constructed tables are bit-identical to zlib's, and
/// * the FFI boundary (`src/ffi.rs`) can reference this type directly.
///
/// The original C definition is:
///
/// ```c
/// typedef struct {
///     unsigned char op;     /* operation, extra bits, table bits */
///     unsigned char bits;   /* bits in this part of the code */
///     unsigned short val;   /* offset in table or code value */
/// } code;
/// ```
///
/// ## `op` value encoding (as set by `inflate_table`)
///
/// | bit pattern | meaning |
/// |-------------|---------|
/// | `00000000`  | literal |
/// | `0000tttt`  | table link, `tttt` (≠ 0) = number of table index bits |
/// | `0001eeee`  | length or distance, `eeee` = number of extra bits (the `0x10` bit marks a length/distance base) |
/// | `01100000` (= 96) | end of block |
/// | `01000000` (= 64) | invalid code |
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Code {
    /// operation, extra bits, table bits
    pub op: u8,
    /// bits in this part of the code
    pub bits: u8,
    /// offset in table or code value
    pub val: u16,
}

impl Code {
    /// Construct a decode-table entry.
    ///
    /// Declared `const` so the fixed Huffman tables in `crate::inflate::fixed`
    /// can be built as compile-time `const` arrays of [`Code`].
    #[inline]
    #[must_use]
    pub const fn new(op: u8, bits: u8, val: u16) -> Code {
        Code { op, bits, val }
    }
}

/// Maximum supported code length, in bits (C `#define MAXBITS 15`).
pub const MAXBITS: usize = 15;

/// Maximum number of [`Code`] entries a literal/length table can occupy.
///
/// Found by exhaustive search (`enough 286 9 15`) as documented in
/// `inftrees.h`. The root table for literal/length codes uses 9 index bits.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of [`Code`] entries a distance table can occupy.
///
/// Found by exhaustive search (`enough 30 6 15`) as documented in
/// `inftrees.h`. The root table for distance codes uses 6 index bits.
pub const ENOUGH_DISTS: usize = 592;

/// Maximum size of the combined dynamic decode table, in [`Code`] entries.
///
/// This is the sum [`ENOUGH_LENS`] + [`ENOUGH_DISTS`] = 1444. The shared
/// `codes` buffer in the inflate state is sized to hold this many entries.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

/// The kind of canonical Huffman code being built by `inflate_table`.
///
/// Mirrors the C `codetype` enum (`CODES`, `LENS`, `DISTS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeType {
    /// Code-length codes (the 19-symbol alphabet used to encode the dynamic
    /// literal/length and distance code lengths). No base/extra tables apply.
    Codes,
    /// Literal/length codes (286-symbol alphabet). Uses the length base/extra
    /// tables (`LBASE`/`LEXT`).
    Lens,
    /// Distance codes (30-symbol alphabet). Uses the distance base/extra tables
    /// (`DBASE`/`DEXT`).
    Dists,
}

/// Inflate copyright string, preserved verbatim from `inftrees.c`.
///
/// Kept in the binary as an acknowledgment of the original zlib implementation,
/// matching the upstream `const char inflate_copyright[]`.
#[allow(dead_code)]
pub(crate) const INFLATE_COPYRIGHT: &str = " inflate 1.3.2.1 Copyright 1995-2026 Mark Adler ";

/// Length codes 257..285 base values (C `lbase`).
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// Length codes 257..285 extra bits (C `lext`).
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Distance codes 0..29 base values (C `dbase`).
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Distance codes 0..29 extra bits (C `dext`).
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

/// Build a set of two-level lookup tables to decode the provided canonical
/// Huffman code.
///
/// This is a faithful port of C `inflate_table()`. The code lengths are
/// `lens[0..codes]` (each in the range `0..=MAXBITS`; `0` means the symbol does
/// not occur). The constructed tables are written into `codes_buf`, which is the
/// caller's shared `codes` buffer (`&mut state.codes[..]`).
///
/// # Pointer-to-index model
///
/// The C signature takes `code **table` — a pointer to the caller's "next free
/// slot" pointer. Here that becomes `next: &mut usize`, an **index** into
/// `codes_buf`:
///
/// * On entry, `*next` is the base index of the root table to fill.
/// * On return, `*next` has been advanced by the number of entries used, so the
///   caller can build a second table (e.g. the distance table after the
///   literal/length table) immediately after the first.
///
/// `bits` is the requested number of root-table index bits on entry; on return
/// it holds the actual root bits (which may be smaller if the request exceeds
/// the longest code, or larger toward the shortest code).
///
/// `work` is a scratch array of at least `lens.len()` `u16` values used to sort
/// the symbols.
///
/// # Returns
///
/// * `0`  — success.
/// * `-1` — the code lengths describe an over-subscribed or (illegally)
///   incomplete set.
/// * `1`  — the table would exceed [`ENOUGH_LENS`]/[`ENOUGH_DISTS`] (should
///   never happen for valid root sizes; guards against constant drift).
pub(crate) fn inflate_table(
    type_: CodeType,
    lens: &[u16],
    codes: usize,
    codes_buf: &mut [Code],
    next: &mut usize,
    bits: &mut u32,
    work: &mut [u16],
) -> i32 {
    // Number of codes of each length, and offsets into the sorted symbol table.
    let mut count = [0u16; MAXBITS + 1];
    let mut offs = [0u16; MAXBITS + 1];

    // Accumulate the count of codes for each length (assumes lens[] all in
    // 0..=MAXBITS, which the caller must guarantee).
    for &len in &lens[..codes] {
        count[len as usize] += 1;
    }

    // Bound code lengths, forcing the root to lie within the code lengths.
    let mut root: u32 = *bits;

    // Highest non-zero code length.
    let mut max: u32 = MAXBITS as u32;
    while max >= 1 {
        if count[max as usize] != 0 {
            break;
        }
        max -= 1;
    }
    if root > max {
        root = max;
    }

    // No symbols to code at all: build a one-entry "table" of two invalid
    // markers to force the decoder to report an error, and report 1 root bit.
    if max == 0 {
        let here = Code::new(64, 1, 0); // invalid code marker
        codes_buf[*next] = here;
        *next += 1;
        codes_buf[*next] = here;
        *next += 1;
        *bits = 1;
        return 0;
    }

    // Lowest non-zero code length.
    let mut min: u32 = 1;
    while min < max {
        if count[min as usize] != 0 {
            break;
        }
        min += 1;
    }
    if root < min {
        root = min;
    }

    // Check for an over-subscribed or incomplete set of lengths. `left` is the
    // running count of available prefix codes and MUST be signed: it goes
    // negative exactly when the set is over-subscribed.
    let mut left: i32 = 1;
    for &c in &count[1..=MAXBITS] {
        left <<= 1;
        left -= c as i32;
        if left < 0 {
            return -1; // over-subscribed
        }
    }
    if left > 0 && (type_ == CodeType::Codes || max != 1) {
        return -1; // incomplete set
    }

    // Generate offsets into the symbol table for each length, for sorting.
    offs[1] = 0;
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }

    // Sort symbols by length, and by symbol order within each length.
    for (sym, &len) in lens[..codes].iter().enumerate() {
        if len != 0 {
            let li = len as usize;
            work[offs[li] as usize] = sym as u16;
            offs[li] += 1;
        }
    }

    // Select the base/extra tables and the `match` threshold for the code type.
    // For `Codes`, base/extra are never indexed (every symbol is a literal), so
    // empty slices are safe.
    let (base, extra, match_): (&[u16], &[u16], u32) = match type_ {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // Capture the root base index. In C this is `*table`, which stays fixed
    // until the final `*table += used`. Sub-table links are stored relative to
    // it.
    let root_base: usize = *next;

    // Initialize state for the fill loop.
    let mut huff: u32 = 0; // starting code
    let mut sym: usize = 0; // starting code symbol
    let mut len: u32 = min; // starting code length
    let mut cur_base: usize = root_base; // current table being filled (C `next`)
    let mut curr: u32 = root; // current table index bits
    let mut drop: u32 = 0; // current bits to drop from code for index
    let mut low: u32 = u32::MAX; // trigger a new sub-table when len > root
    let mut used: usize = 1usize << root; // root table entries in use
    let mask: u32 = (1u32 << root) - 1; // mask for comparing low root bits

    // Guard the root table size against ENOUGH (constant-drift protection).
    if (type_ == CodeType::Lens && used > ENOUGH_LENS)
        || (type_ == CodeType::Dists && used > ENOUGH_DISTS)
    {
        return 1;
    }

    // Process all codes and make table entries.
    loop {
        // Create the table entry for the current symbol.
        let w = work[sym] as u32;
        let here = if w + 1 < match_ {
            // literal
            Code::new(0, (len - drop) as u8, work[sym])
        } else if w >= match_ {
            // length or distance base
            Code::new(
                extra[(w - match_) as usize] as u8,
                (len - drop) as u8,
                base[(w - match_) as usize],
            )
        } else {
            // end of block (op = 32 | 64 = 96)
            Code::new(96, (len - drop) as u8, 0)
        };

        // Replicate the entry for all indices whose low `len` bits equal `huff`.
        // `table_size` (= `1 << curr`, C's reused `min`) is the size of the
        // current (sub)table and the amount to advance past it when a new
        // sub-table begins.
        let incr_fill = 1u32 << (len - drop);
        let table_size = 1u32 << curr;
        let mut fill = table_size;
        loop {
            fill -= incr_fill;
            codes_buf[cur_base + ((huff >> drop) + fill) as usize] = here;
            if fill == 0 {
                break;
            }
        }

        // Backwards-increment the `len`-bit code `huff`.
        let mut incr = 1u32 << (len - 1);
        while (huff & incr) != 0 {
            incr >>= 1;
        }
        if incr != 0 {
            huff &= incr - 1;
            huff += incr;
        } else {
            huff = 0;
        }

        // Advance to the next symbol; update the length count and current length.
        sym += 1;
        count[len as usize] -= 1;
        if count[len as usize] == 0 {
            if len == max {
                break;
            }
            len = lens[work[sym] as usize] as u32;
        }

        // Create a new sub-table if needed.
        if len > root && (huff & mask) != low {
            // On the first sub-table, transition `drop` from the root.
            if drop == 0 {
                drop = root;
            }

            // Move past the table just filled (`table_size` holds `1 << curr`).
            cur_base += table_size as usize;

            // Determine the length of the next sub-table by looking ahead at the
            // remaining length counts.
            curr = len - drop;
            let mut left2: i32 = 1i32 << curr;
            while curr + drop < max {
                left2 -= count[(curr + drop) as usize] as i32;
                if left2 <= 0 {
                    break;
                }
                curr += 1;
                left2 <<= 1;
            }

            // Check for enough space.
            used += 1usize << curr;
            if (type_ == CodeType::Lens && used > ENOUGH_LENS)
                || (type_ == CodeType::Dists && used > ENOUGH_DISTS)
            {
                return 1;
            }

            // Point the entry in the root table to the new sub-table.
            low = huff & mask;
            let r = root_base + low as usize;
            codes_buf[r].op = curr as u8;
            codes_buf[r].bits = root as u8;
            codes_buf[r].val = (cur_base - root_base) as u16;
        }
    }

    // Fill in the remaining table entry if the code is incomplete (guaranteed to
    // be at most one remaining entry, since an incomplete code that reaches here
    // has a maximum length of one bit).
    if huff != 0 {
        codes_buf[cur_base + huff as usize] = Code::new(64, (len - drop) as u8, 0);
    }

    // Set return parameters.
    *next = root_base + used;
    *bits = root;
    0
}

/// Index/bit parameters for the fixed Huffman tables, as produced by
/// [`inflate_fixed`].
///
/// In C these four values are assigned directly onto the inflate state
/// (`state->lencode`, `state->lenbits`, `state->distcode`, `state->distbits`).
/// Here they are returned as a small value so this module stays independent of
/// the inflate-state definition.
pub(crate) struct FixedTables {
    /// Index of the literal/length root table within the `codes` buffer.
    pub lencode: usize,
    /// Number of root index bits for the literal/length table (always 9).
    pub lenbits: u32,
    /// Index of the distance root table within the `codes` buffer.
    pub distcode: usize,
    /// Number of root index bits for the distance table (always 5).
    pub distbits: u32,
}

/// Install the fixed Huffman decode tables into `codes_buf` and return their
/// index/bit parameters.
///
/// This mirrors C `inflate_fixed()` on its default (non-`BUILDFIXED`) path,
/// which simply points `lencode`/`distcode` at the static `lenfix`/`distfix`
/// arrays from `inffixed.h` (with `lenbits = 9`, `distbits = 5`). To keep a
/// single uniform index model — where `lencode`/`distcode` are indices into the
/// shared `codes` buffer for both fixed and dynamic blocks — this copies the
/// fixed tables into `codes_buf` instead of aliasing a separate array.
///
/// The output is **byte-identical** to C: the table *contents* are exactly the
/// `LENFIX`/`DISTFIX` arrays; only the storage location (inside `codes`)
/// differs, which is not observable by the decoder.
///
/// The literal/length table occupies entries `0..512` and the distance table
/// occupies entries `512..544`; this fits comfortably because
/// [`ENOUGH`] (1444) ≥ 544. Subsequent dynamic blocks reset the caller's
/// `state.next` to `0` and rebuild, overwriting these entries — which is safe
/// across block-type transitions.
///
/// # Caller contract
///
/// Callers (the `TYPE`/`TYPEDO` state in `crate::inflate::mod` and the
/// fixed-block case in `crate::inflate::back`) must assign the four returned
/// fields onto the inflate state but **must not advance `state.next`**. Leaving
/// `state.next` unchanged for fixed blocks preserves the correct
/// `inflateCodesUsed()` semantics (in C, `next` stays at `codes` for fixed
/// blocks).
pub(crate) fn inflate_fixed(codes_buf: &mut [Code]) -> FixedTables {
    codes_buf[0..512].copy_from_slice(&crate::inflate::fixed::LENFIX);
    codes_buf[512..544].copy_from_slice(&crate::inflate::fixed::DISTFIX);
    FixedTables {
        lencode: 0,
        lenbits: 9,
        distcode: 512,
        distbits: 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::fixed::{DISTFIX, LENFIX};

    /// Canonical fixed literal/length code lengths (RFC 1951 §3.2.6):
    /// 8 bits for 0..144, 9 for 144..256, 7 for 256..280, 8 for 280..288.
    fn fixed_lit_len_lengths() -> [u16; 288] {
        let mut lens = [0u16; 288];
        let mut sym = 0;
        while sym < 144 {
            lens[sym] = 8;
            sym += 1;
        }
        while sym < 256 {
            lens[sym] = 9;
            sym += 1;
        }
        while sym < 280 {
            lens[sym] = 7;
            sym += 1;
        }
        while sym < 288 {
            lens[sym] = 8;
            sym += 1;
        }
        lens
    }

    #[test]
    fn code_layout_matches_c() {
        // #[repr(C)] gives the same 4-byte/2-byte-aligned layout as C `code`.
        assert_eq!(core::mem::size_of::<Code>(), 4);
        assert_eq!(core::mem::align_of::<Code>(), 2);
    }

    #[test]
    fn enough_constants_match_header() {
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(MAXBITS, 15);
    }

    #[test]
    fn code_new_is_const_constructor() {
        const C: Code = Code::new(0x10, 7, 258);
        assert_eq!(C.op, 0x10);
        assert_eq!(C.bits, 7);
        assert_eq!(C.val, 258);
    }

    #[test]
    fn builds_fixed_litlen_table_matches_c() {
        // Build the literal/length table exactly as C's `inflate_table` does for
        // the fixed code. The raw output is byte-identical to C's `inflate_table`
        // (verified against the C source: idx 99/355 -> {68,8,0}, idx 227/483 ->
        // {193,8,0}).
        //
        // The *static* `lenfix` table in `inffixed.h` additionally applies the
        // `MAKEFIXED` generator's special case (inftrees.c line ~405):
        // entries at table indices with `(low & 127) == 99` — the non-existent
        // fixed-code length symbols 286/287 — are forced to the invalid-code
        // marker `{op:64, bits:8, val:0}`. We reproduce that override here, which
        // is the link between the runtime builder and the published table.
        let lens = fixed_lit_len_lengths();
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 288];
        let mut next = 0usize;
        let mut bits = 9u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        assert_eq!(bits, 9);
        // The fixed literal/length code fits entirely in the 9-bit root table.
        assert_eq!(next, 512);

        for (low, entry) in codes_buf.iter_mut().enumerate().take(512) {
            if (low & 127) == 99 {
                // Raw `inflate_table` emits a length code for the invalid symbols
                // 286/287 here: extra-bit op 68 (`LEXT[29]`) or 193 (`LEXT[30]`),
                // with base 0. This matches C's runtime `inflate_table` exactly.
                assert!(
                    entry.op == 68 || entry.op == 193,
                    "idx {low}: unexpected raw op {}",
                    entry.op
                );
                assert_eq!(entry.bits, 8);
                assert_eq!(entry.val, 0);
                // Apply the MAKEFIXED override to match the published static table.
                entry.op = 64;
            }
        }
        // Every other entry must be byte-identical to the published table; this
        // assertion fails on any divergence outside the four override indices.
        assert_eq!(&codes_buf[0..512], &LENFIX[..]);
    }

    #[test]
    fn builds_fixed_dist_table_byte_identical() {
        let lens = [5u16; 32];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 32];
        let mut next = 0usize;
        let mut bits = 5u32;
        let ret = inflate_table(
            CodeType::Dists,
            &lens,
            32,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        assert_eq!(bits, 5);
        assert_eq!(next, 32);
        assert_eq!(&codes_buf[0..32], &DISTFIX[..]);
    }

    #[test]
    fn inflate_fixed_installs_byte_identical_tables() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let ft = inflate_fixed(&mut codes_buf);
        assert_eq!(ft.lencode, 0);
        assert_eq!(ft.lenbits, 9);
        assert_eq!(ft.distcode, 512);
        assert_eq!(ft.distbits, 5);
        assert_eq!(&codes_buf[0..512], &LENFIX[..]);
        assert_eq!(&codes_buf[512..544], &DISTFIX[..]);
    }

    #[test]
    fn over_subscribed_returns_minus_one() {
        // Three length-1 codes: 3 * 2^-1 > 1, so the set is over-subscribed.
        let lens = [1u16, 1, 1];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 7u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            3,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, -1);
    }

    #[test]
    fn incomplete_lens_returns_minus_one() {
        // A single length-2 code leaves the set incomplete; max != 1, so it is
        // rejected for LENS.
        let lens = [2u16];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 7u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            1,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, -1);
    }

    #[test]
    fn single_code_exception_is_accepted() {
        // A single length-1 code is incomplete, but the max == 1 exception
        // permits it for DISTS (the "one distance code" case).
        let lens = [1u16];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 5u32;
        let ret = inflate_table(
            CodeType::Dists,
            &lens,
            1,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        // Root bits forced to 1; two entries used: the code plus one invalid
        // marker so the decoder reports an error if the unused slot is hit.
        assert_eq!(bits, 1);
        assert_eq!(next, 2);
        assert_eq!(codes_buf[0], Code::new(16, 1, 1)); // distance base 1, 16 extra
        assert_eq!(codes_buf[1], Code::new(64, 1, 0)); // invalid marker
    }

    #[test]
    fn codes_type_has_no_single_code_exception() {
        // For CODES there is no max == 1 exception: an incomplete set is always
        // rejected.
        let lens = [1u16];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 7u32;
        let ret = inflate_table(
            CodeType::Codes,
            &lens,
            1,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, -1);
    }

    #[test]
    fn no_symbols_builds_two_invalid_markers() {
        // An all-zero length set has no symbols; the builder emits two invalid
        // markers and reports a 1-bit root, matching C.
        let lens = [0u16; 4];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 7u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            4,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        assert_eq!(bits, 1);
        assert_eq!(next, 2);
        assert_eq!(codes_buf[0], Code::new(64, 1, 0));
        assert_eq!(codes_buf[1], Code::new(64, 1, 0));
    }

    #[test]
    fn builds_sub_tables_when_code_exceeds_root() {
        // A complete code [1,2,3,3] with a 1-bit root forces sub-tables for the
        // length-2 and length-3 codes, exercising the sub-table-linking path.
        let lens = [1u16, 2, 3, 3];
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let mut next = 0usize;
        let mut bits = 1u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            4,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        assert_eq!(bits, 1);
        // More than the bare 1<<1 root entries were used (a sub-table was added).
        assert!(next > (1usize << 1));
        // At least one root entry is a table link: op encodes the sub-table
        // index-bit count (1..=15), i.e. nonzero with the 0x10 length/distance
        // bit clear.
        let root_links = codes_buf[0..(1usize << 1)]
            .iter()
            .filter(|c| c.op != 0 && c.op < 16)
            .count();
        assert!(root_links > 0);
    }
}
