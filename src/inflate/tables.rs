//! Huffman decode-table builder for the inflate engine.
//!
//! This module is the safe-Rust port of zlib's `inftrees.c` / `inftrees.h`.
//! It is *foundational* for the entire inflate engine: it owns the single
//! authoritative [`Code`] decode-table entry type (`#[repr(C)]`, indexed
//! directly by the inflate fast loop, the main inflate state machine, and
//! `inflateBack`, and exposed at the FFI boundary in `src/ffi.rs`), the
//! `inflate_table` builder that constructs the two-level lookup tables for a
//! canonical Huffman code, and the `inflate_fixed` helper that installs the
//! fixed (static) Huffman tables.
//!
//! The construction is a faithful, behaviour-preserving port: the produced
//! tables are **byte-identical** to those built by C zlib. Any structural
//! difference — different entry contents, fill order, sub-table linking, or
//! `ENOUGH` accounting — would corrupt every dynamic-block and fixed-block
//! decode, so the algorithm below mirrors the C reference statement for
//! statement, translating its raw `code **table` pointer protocol into an
//! equivalent *index* protocol over a caller-owned `codes` buffer.
//!
//! The entire module is **safe** Rust (zero `unsafe`) and **`no_std`-clean**:
//! it uses only `core` and fixed-size stack arrays, performing no heap
//! allocation. It therefore compiles under
//! `--no-default-features --features no-std`.

/// A single decoding-table entry.
///
/// `#[repr(C)]` guarantees the layout matches the C `code` struct exactly
/// (four bytes: `op`, `bits`, then a two-byte `val`), which is required both
/// for the FFI boundary (`src/ffi.rs` references this type) and so that the
/// tables constructed by `inflate_table` are bit-identical to those built by
/// C zlib.
///
/// # `op` value encoding (as set by `inflate_table`)
///
/// The `op` byte is interpreted bitwise, exactly as documented in
/// `inftrees.h`:
///
/// * `00000000` — literal.
/// * `0000tttt` — table link; `tttt` (non-zero) is the number of index bits of
///   the linked sub-table.
/// * `0001eeee` — length or distance base; `eeee` is the number of extra bits
///   to read after the code (i.e. the `0x10` bit marks a length/distance base).
/// * `01100000` (= 96) — end of block.
/// * `01000000` (= 64) — invalid code.
///
/// For a literal, `val` is the byte to output; for a length/distance base,
/// `val` is the base length or distance; for a table link, `val` is the offset
/// from the current table to the linked sub-table.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Code {
    /// Operation, extra bits, or table-index bits (see the type-level docs).
    pub op: u8,
    /// Number of bits in this code, or the part of the code consumed here.
    pub bits: u8,
    /// Offset to the next table, or the decoded code value (literal byte,
    /// base length, or base distance).
    pub val: u16,
}

impl Code {
    /// Construct a [`Code`] entry from its three fields.
    ///
    /// This is a `const fn` so that the fixed Huffman tables in
    /// `crate::inflate::fixed` can be expressed as `const` arrays of [`Code`].
    #[inline]
    #[must_use]
    pub const fn new(op: u8, bits: u8, val: u16) -> Code {
        Code { op, bits, val }
    }
}

/// Maximum supported code length in bits (C `#define MAXBITS 15`).
pub const MAXBITS: usize = 15;

/// Maximum number of [`Code`] entries a literal/length table may occupy.
///
/// Found by exhaustive search (`enough 286 9 15`) for a root table of 9 bits
/// and a maximum code length of 15 bits. See `inftrees.h`.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of [`Code`] entries a distance table may occupy.
///
/// Found by exhaustive search (`enough 30 6 15`) for a root table of 6 bits
/// and a maximum code length of 15 bits. See `inftrees.h`.
pub const ENOUGH_DISTS: usize = 592;

/// Total size of the shared dynamic decode buffer: `ENOUGH_LENS + ENOUGH_DISTS`.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

/// The kind of canonical Huffman code `inflate_table` is asked to build.
///
/// Mirrors the C `codetype` enum (`CODES`, `LENS`, `DISTS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeType {
    /// Code-length codes (the 19-symbol alphabet that encodes the dynamic
    /// literal/length and distance code lengths).
    Codes,
    /// Literal/length codes.
    Lens,
    /// Distance codes.
    Dists,
}

/// The zlib inflate copyright string, preserved verbatim from `inftrees.c`.
///
/// Retained (per the upstream request) as an acknowledgement string. It is not
/// referenced by the decode logic, hence `#[allow(dead_code)]`.
#[allow(dead_code)]
pub(crate) const INFLATE_COPYRIGHT: &str = " inflate 1.3.2.1 Copyright 1995-2026 Mark Adler ";

/// Base values for length codes 257..285 (`inftrees.c` `lbase`).
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// Extra-bit counts for length codes 257..285 (`inftrees.c` `lext`).
///
/// The two trailing pseudo-entries (68, 193) carry the `0x40` invalid bit and
/// produce invalid-code markers for the unused length symbols 286 and 287.
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Base values for distance codes 0..29 (`inftrees.c` `dbase`).
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// Extra-bit counts for distance codes 0..29 (`inftrees.c` `dext`).
///
/// The two trailing entries (64, 64) carry the `0x40` invalid bit for the
/// unused distance symbols 30 and 31.
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

/// Build a set of two-level lookup tables that decode the provided canonical
/// Huffman code.
///
/// This is a faithful port of C `inflate_table`. The C signature passes
/// `code **table` — a pointer to the caller's "next free slot" pointer. In
/// this safe-Rust design that pointer becomes an **index** (`next`) into the
/// caller's shared `codes` buffer (`codes_buf`):
///
/// * On entry, `*next` is the base index of the root table to fill; on return
///   it has been advanced by the number of entries used (`used`), exactly as
///   C does `*table += used`.
/// * On entry, `*bits` is the requested root-table index bits; on return it is
///   the actual root bits (which may shrink if the request exceeds the longest
///   code, or grow if it is below the shortest code).
/// * `lens[0..codes]` holds the code length (0..=`MAXBITS`) of each symbol.
/// * `work` is scratch space of at least `codes` `u16`s used to sort symbols.
///
/// # Returns
///
/// * `0` on success.
/// * `-1` if the code is invalid (over-subscribed, or incomplete for a type
///   that does not permit incomplete codes).
/// * `1` if the table would exceed `ENOUGH_LENS` / `ENOUGH_DISTS`.
///
/// The caller must assure that every entry of `lens[0..codes]` is in the range
/// `0..=MAXBITS`; this routine assumes but does not check that invariant.
pub(crate) fn inflate_table(
    type_: CodeType,
    lens: &[u16],
    codes: usize,
    codes_buf: &mut [Code],
    next: &mut usize,
    bits: &mut u32,
    work: &mut [u16],
) -> i32 {
    // The root table base index. C keeps `*table` fixed at the root base until
    // the final `*table += used`; we mirror that with `root_base`.
    let root_base = *next;

    // accumulate lengths for codes (assumes lens[] all in 0..=MAXBITS)
    let mut count = [0u16; MAXBITS + 1];
    for &len_val in &lens[..codes] {
        count[len_val as usize] += 1;
    }

    // bound code lengths, force root to be within code lengths
    let mut root = *bits;
    let mut max = MAXBITS as u32;
    while max >= 1 {
        if count[max as usize] != 0 {
            break;
        }
        max -= 1;
    }
    if root > max {
        root = max;
    }
    if max == 0 {
        // no symbols to code at all: emit a two-entry table that forces an
        // error during decoding (C `*(*table)++ = here;` twice).
        let here = Code::new(64, 1, 0);
        codes_buf[*next] = here;
        *next += 1;
        codes_buf[*next] = here;
        *next += 1;
        *bits = 1;
        return 0;
    }
    let mut min = 1u32;
    while min < max {
        if count[min as usize] != 0 {
            break;
        }
        min += 1;
    }
    if root < min {
        root = min;
    }

    // check for an over-subscribed or incomplete set of lengths. `left` MUST be
    // signed: it goes negative when the code is over-subscribed.
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

    // generate offsets into the symbol table for each length, for sorting
    let mut offs = [0u16; MAXBITS + 1];
    offs[1] = 0;
    for len_o in 1..MAXBITS {
        offs[len_o + 1] = offs[len_o] + count[len_o];
    }

    // sort symbols by length, and by symbol order within each length
    for (sym, &len_val) in lens.iter().enumerate().take(codes) {
        let l = len_val as usize;
        if l != 0 {
            work[offs[l] as usize] = sym as u16;
            offs[l] += 1;
        }
    }

    // set up base/extra/match for the requested code type. For `Codes`, every
    // symbol decodes as a literal, so base/extra are never indexed.
    let (base, extra, match_): (&[u16], &[u16], u32) = match type_ {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE[..], &LEXT[..], 257),
        CodeType::Dists => (&DBASE[..], &DEXT[..], 0),
    };

    // initialize state for the fill loop
    let mut huff: u32 = 0; // starting code
    let mut sym: usize = 0; // starting code symbol
    let mut len: u32 = min; // starting code length
    let mut cur_base = root_base; // current table to fill in (C `next`)
    let mut curr: u32 = root; // current table index bits
    let mut drop: u32 = 0; // current bits to drop from code for index
    let mut low: u32 = u32::MAX; // trigger a new sub-table when len > root
    let mut used: u32 = 1u32 << root; // root table entries in use
    let mask: u32 = used - 1; // mask for comparing low

    // check available table space
    if (type_ == CodeType::Lens && used as usize > ENOUGH_LENS)
        || (type_ == CodeType::Dists && used as usize > ENOUGH_DISTS)
    {
        return 1;
    }

    // process all codes and make table entries
    loop {
        // create the table entry for the current symbol
        let w = work[sym] as u32;
        let (op, val): (u8, u16) = if w + 1 < match_ {
            (0, work[sym]) // literal
        } else if w >= match_ {
            // length or distance base
            (
                extra[(w - match_) as usize] as u8,
                base[(w - match_) as usize],
            )
        } else {
            (96, 0) // 32 + 64 = end of block
        };
        let here = Code::new(op, (len - drop) as u8, val);

        // replicate the entry for all indices whose low `len` bits equal `huff`
        let mut incr = 1u32 << (len - drop);
        let mut fill = 1u32 << curr;
        min = fill; // save the offset to the next table (= 1 << curr)
        loop {
            fill -= incr;
            codes_buf[cur_base + ((huff >> drop) + fill) as usize] = here;
            if fill == 0 {
                break;
            }
        }

        // backwards-increment the len-bit code huff
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

        // advance to the next symbol, updating the running length counts
        sym += 1;
        count[len as usize] -= 1;
        if count[len as usize] == 0 {
            if len == max {
                break;
            }
            len = lens[work[sym] as usize] as u32;
        }

        // create a new sub-table if needed
        if len > root && (huff & mask) != low {
            // on the first overflow, transition from the root table to sub-tables
            if drop == 0 {
                drop = root;
            }

            // increment past the table just filled (here `min` is 1 << curr)
            cur_base += min as usize;

            // determine the length of the next sub-table (reusing `left`)
            curr = len - drop;
            left = 1i32 << curr;
            while curr + drop < max {
                left -= count[(curr + drop) as usize] as i32;
                if left <= 0 {
                    break;
                }
                curr += 1;
                left <<= 1;
            }

            // check for enough space
            used += 1u32 << curr;
            if (type_ == CodeType::Lens && used as usize > ENOUGH_LENS)
                || (type_ == CodeType::Dists && used as usize > ENOUGH_DISTS)
            {
                return 1;
            }

            // point the entry in the root table to this sub-table
            low = huff & mask;
            let r = root_base + low as usize;
            codes_buf[r].op = curr as u8;
            codes_buf[r].bits = root as u8;
            codes_buf[r].val = (cur_base - root_base) as u16;
        }
    }

    // fill in the one remaining entry if the code is incomplete (guaranteed to
    // be at most one entry, since an incomplete code that reaches this point
    // has a maximum length of one bit)
    if huff != 0 {
        codes_buf[cur_base + huff as usize] = Code::new(64, (len - drop) as u8, 0);
    }

    // set return parameters
    *next = root_base + used as usize;
    *bits = root;
    0
}

/// Index and bit parameters for the fixed Huffman decode tables, as produced
/// by `inflate_fixed`.
///
/// `lencode` / `distcode` are *indices* into the caller's `codes` buffer (the
/// uniform index model used throughout this crate), and `lenbits` / `distbits`
/// are the corresponding root-table index-bit counts.
pub(crate) struct FixedTables {
    /// Index of the literal/length root table within the `codes` buffer.
    pub lencode: usize,
    /// Root-table index bits for the literal/length table (always 9).
    pub lenbits: u32,
    /// Index of the distance root table within the `codes` buffer.
    pub distcode: usize,
    /// Root-table index bits for the distance table (always 5).
    pub distbits: u32,
}

/// Install the fixed (static) Huffman decode tables into `codes_buf` and return
/// their index/bit parameters.
///
/// This mirrors C `inflate_fixed()` on its default (non-`BUILDFIXED`) path,
/// which simply points `lencode`/`distcode` at the static `lenfix`/`distfix`
/// arrays from `inffixed.h`. Because this crate uses a uniform *index* model
/// (decode tables are referenced by index into the shared `codes` buffer), we
/// instead copy those static tables into `codes_buf`: `LENFIX` occupies
/// `[0..512)` and `DISTFIX` occupies `[512..544)`. The output is byte-identical
/// to C — only the storage location (inside `codes`) differs, which is not
/// observable. This fits comfortably because `ENOUGH` (1444) ≥ 544.
///
/// # Caller contract
///
/// Callers (the `mod.rs` `TYPE`/`TYPEDO` state and the `back.rs` fixed-block
/// case) must assign the four returned fields to the inflate state and **must
/// not advance `state.next`**. Leaving `state.next` unchanged preserves the
/// correct `inflateCodesUsed()` semantics for fixed blocks (in C, `next` stays
/// at `codes` for fixed blocks). A subsequent dynamic block resets
/// `state.next = 0` and rebuilds, harmlessly overwriting these entries.
///
/// `codes_buf` must have length at least 544.
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

    /// The `#[repr(C)]` layout must match the C `code` struct: four bytes,
    /// two-byte alignment (driven by the `u16` `val` field).
    #[test]
    fn code_layout_is_c_abi() {
        assert_eq!(core::mem::size_of::<Code>(), 4);
        assert_eq!(core::mem::align_of::<Code>(), 2);
    }

    /// The `ENOUGH*` sizing constants must match the real `inftrees.h` values
    /// (852 / 592 / 1444), not the erroneous "2048" quoted in AAP prose.
    #[test]
    fn enough_constants_match_header() {
        assert_eq!(MAXBITS, 15);
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS);
    }

    /// Fill `lens[0..288]` with the fixed literal/length code-length pattern
    /// (8 for 0..144, 9 for 144..256, 7 for 256..280, 8 for 280..288), exactly
    /// as C `buildtables()` does.
    fn fixed_litlen_lengths() -> [u16; 288] {
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

    /// Building the fixed literal/length table with `inflate_table` must yield
    /// the exact 512-entry `LENFIX` table generated by C zlib. This directly
    /// proves byte-identical construction.
    #[test]
    fn builds_fixed_literal_length_table_byte_identical() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 288];
        let lens = fixed_litlen_lengths();

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
        assert_eq!(bits, 9); // max code length (9) == requested root, so root stays 9
        assert_eq!(next, 512); // used == 1 << 9; no sub-tables (max len == root)

        // `inflate_table` naturally emits the raw extra-bit op for the two
        // *unused* length symbols 286 and 287 (`lext[29]` = 68, `lext[30]` =
        // 193) — both of which carry the 0x40 "invalid code" bit. zlib's
        // `makefixed()` generator rewrites exactly those four table slots
        // (indices `i` where `(i & 127) == 99`) to the canonical invalid marker
        // `op = 64` when it emits `inffixed.h` (see `inftrees.c`:
        // `(low & 127) == 99 ? 64 : op`). Apply that documented override here,
        // then assert byte-identity with the shipped `LENFIX`. At those slots
        // only `op` differs; `bits`/`val` already match — proving the builder
        // is byte-faithful to C's `inflate_table`.
        let mut overridden = 0;
        for i in 0..512 {
            if (i & 127) == 99 {
                assert!(
                    (codes_buf[i].op & 0x40) != 0,
                    "slot {i} must be an invalid-code marker before override"
                );
                assert_eq!(codes_buf[i].bits, LENFIX[i].bits);
                assert_eq!(codes_buf[i].val, LENFIX[i].val);
                codes_buf[i].op = 64;
                overridden += 1;
            }
        }
        assert_eq!(overridden, 4, "exactly four makefixed overrides expected");
        assert_eq!(&codes_buf[0..512], &LENFIX[..]);
    }

    /// Building the fixed distance table (32 codes, all length 5, root bits 5)
    /// immediately after the literal/length table — so it begins at index 512,
    /// exactly as C `buildtables()` lays it out — must yield the 32-entry
    /// `DISTFIX` table. This also exercises a non-zero `root_base`.
    #[test]
    fn builds_fixed_distance_table_byte_identical() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 288];
        let mut lens = fixed_litlen_lengths();

        let mut next = 0usize;
        let mut bits = 9u32;
        let r1 = inflate_table(
            CodeType::Lens,
            &lens,
            288,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(r1, 0);
        assert_eq!(next, 512);

        // distance table: 32 codes, each length 5, root bits 5
        for slot in lens.iter_mut().take(32) {
            *slot = 5;
        }
        let mut dbits = 5u32;
        let ret = inflate_table(
            CodeType::Dists,
            &lens,
            32,
            &mut codes_buf,
            &mut next,
            &mut dbits,
            &mut work,
        );

        assert_eq!(ret, 0);
        assert_eq!(dbits, 5);
        assert_eq!(next, 512 + 32);
        assert_eq!(&codes_buf[512..544], &DISTFIX[..]);
    }

    /// `inflate_fixed` must install byte-identical `LENFIX`/`DISTFIX` tables and
    /// report the canonical index/bit parameters, without advancing any cursor.
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

    /// An over-subscribed code (Kraft sum > 1) must be rejected with `-1`.
    #[test]
    fn rejects_over_subscribed_code() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let lens = [1u16, 1, 1]; // three 1-bit codes: Kraft sum 3/2 > 1
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

    /// An incomplete code with `max != 1` must be rejected with `-1`.
    #[test]
    fn rejects_incomplete_code() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let lens = [2u16, 2]; // two 2-bit codes: Kraft sum 1/2 < 1, max == 2
        let mut next = 0usize;
        let mut bits = 7u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            2,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, -1);
    }

    /// The single-code `max == 1` exception: an incomplete `Lens`/`Dists` code
    /// consisting of one 1-bit symbol is permitted (returns `0`).
    #[test]
    fn accepts_single_code_lens_exception() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let lens = [1u16];
        let mut next = 0usize;
        let mut bits = 9u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            1,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
    }

    /// The single-code exception does NOT apply to `Codes`: the same one 1-bit
    /// symbol is rejected with `-1`.
    #[test]
    fn rejects_single_code_for_codes_type() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let lens = [1u16];
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

    /// A complete canonical code whose maximum length (12) exceeds the root (9)
    /// must force sub-table allocation. Lengths 1,2,..,11,12,12 satisfy the
    /// Kraft equality (`sum 2^-len == 1`), so the set is complete and valid.
    /// This exercises the sub-table linking path and `ENOUGH` accounting.
    #[test]
    fn builds_sub_tables_for_deep_code() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 288];
        let mut lens = [0u16; 288];
        let lengths = [1u16, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12];
        lens[..lengths.len()].copy_from_slice(&lengths);

        let mut next = 0usize;
        let mut bits = 9u32;
        let ret = inflate_table(
            CodeType::Lens,
            &lens,
            lengths.len(),
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );

        assert_eq!(ret, 0);
        assert_eq!(bits, 9); // root unchanged (max 12 > requested 9)
        assert!(
            next > 512,
            "expected sub-tables to be allocated beyond the 512-entry root table"
        );
        assert!(next <= ENOUGH_LENS, "must not exceed ENOUGH_LENS");
    }

    /// The "no symbols" case (all-zero lengths) must emit a two-entry table of
    /// invalid-code markers, set root bits to 1, and return `0`.
    #[test]
    fn empty_code_emits_invalid_markers() {
        let mut codes_buf = [Code::default(); ENOUGH];
        let mut work = [0u16; 8];
        let lens = [0u16; 4];
        let mut next = 0usize;
        let mut bits = 9u32;
        let ret = inflate_table(
            CodeType::Codes,
            &lens,
            4,
            &mut codes_buf,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0);
        assert_eq!(bits, 1);
        assert_eq!(next, 2); // two invalid markers written
        assert_eq!(codes_buf[0], Code::new(64, 1, 0));
        assert_eq!(codes_buf[1], Code::new(64, 1, 0));
    }
}
