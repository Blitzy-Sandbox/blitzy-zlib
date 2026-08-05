//! Huffman decode-table construction: the Rust port of `inftrees.c` and `inftrees.h`.
//!
//! A DEFLATE stream describes its Huffman codes by code *length* alone (RFC 1951
//! §3.2.2). Before a single symbol can be decoded, those lengths have to be turned
//! into a canonical code and then into a table the decoder can index with raw bits
//! pulled from the stream. [`inflate_table`] is that transformation, and it is the
//! only place in the crate where a decode table is created.
//!
//! The module is folded into the `inflate` tree rather than standing on its own,
//! which mirrors the reference implementation exactly: `inflate.c`, `inffast.c`,
//! `inftrees.c` and `infback.c` all include the identical four-header set
//! (`zutil.h`, `inftrees.h`, `inflate.h`, `inffast.h`), so `inftrees` is a private
//! part of the decompressor rather than a separable unit.
//!
//! # Correspondence with the reference implementation
//!
//! * [`Code`] is `struct code` from `inftrees.h` L24-L28, and its `op` encoding is
//!   the table documented at `inftrees.h` L30-L36.
//! * [`CodeType`] is `codetype` from `inftrees.h` L54-L58.
//! * [`ENOUGH_LENS`], [`ENOUGH_DISTS`] and [`ENOUGH`] are the macros at
//!   `inftrees.h` L49-L51, and [`MAXBITS`] is the macro at `inftrees.c` L23.
//! * The private `LBASE`, `LEXT`, `DBASE` and `DEXT` tables are the four
//!   `static const unsigned short` arrays at `inftrees.c` L69-L82, transcribed
//!   element for element.
//! * [`inflate_table`] is `inflate_table` from `inftrees.c` L46-L311.
//! * [`inflate_fixed`] is `inflate_fixed` from `inftrees.c` L364-L372, in its
//!   default (non-`BUILDFIXED`) form.
//! * [`CodeTables`] holds the four fields of `struct inflate_state` at
//!   `inflate.h` L110-L113 that name the tables currently in use.
//!
//! Two parts of `inftrees.c` are deliberately **not** ported. The `BUILDFIXED`
//! block at L313-L352 builds the fixed tables lazily behind a `z_once_t`; the
//! shipped configuration takes the other branch at L351 and `#include`s the
//! committed tables from `inffixed.h`, so this module has no lazy initialisation of
//! any kind -- no `static mut`, no `Once`, no `OnceLock`. That is also why the
//! facade reports `zlibCompileFlags` bit 12 (`BUILDFIXED`) as clear. The `MAKEFIXED`
//! `main()` at L374-L424 is a code generator that writes `inffixed.h`, not library
//! code, and has no Rust counterpart either.
//!
//! # Visibility
//!
//! [`inflate_table`] and [`inflate_fixed`] are `pub(crate)` and must stay that way.
//! Both are hidden symbols in the C shared library: the version script lists
//! `inflate_table` in the `local:` block of `ZLIB_1.2.0` (`zlib.map` L13) and
//! `inflate_fixed` in the `local:` block of `ZLIB_1.3.2` (`zlib.map` L115), so
//! neither is part of the ABI a consumer may link against. Promoting either to `pub`
//! would offer the facade crate a symbol it is not allowed to export.
//!
//! [`Code`], [`CodeType`], [`CodeTables`] and the capacity constants are `pub`
//! because they are types and data rather than exported functions: `state.rs` sizes
//! its arena from [`ENOUGH`], `inffast.rs` reads [`Code`]'s fields directly, and
//! `fixed_tables.rs` is typed in terms of [`Code`].
//!
//! # The in/out table pointer becomes an index cursor
//!
//! C hands `inflate_table` a `code **table`: on entry it points at the next free
//! slot of the caller's `codes[ENOUGH]` arena, and on return it has been advanced
//! past the entries the call consumed (`inftrees.c` L308). The caller reads the
//! *old* value as the base of the table just built -- that is precisely what
//! `inflate.c` L889 and L898 do when they set `lencode` and `distcode`.
//!
//! This crate forbids `unsafe`, so there is no pointer to bump. [`inflate_table`]
//! takes the whole arena as a `&mut [Code]` plus a `&mut usize` cursor, and every
//! position the C code expresses as a pointer becomes an offset from the start of
//! that arena:
//!
//! | C | Rust |
//! |---|---|
//! | `next = *table` (L208) | `next_base = *table_index` |
//! | `next[i] = here` (L243) | `table[next_base + i] = here` |
//! | `next += min` (L271) | `next_base += next_table_offset` |
//! | `(*table)[low] = ...` (L291-L293) | `table[entry_base + low] = ...` |
//! | `next - *table` (L293) | `next_base - entry_base` |
//! | `*table += used` (L308) | `*table_index = entry_base + used` |
//!
//! The `val` of a table-link entry therefore stays **relative to the base of the
//! table that holds the link**, exactly as in C. The decoder depends on that: the
//! second-level lookups at `inflate.c` L930 and `inffast.c` L264/L274 index
//! `lencode[last.val + ...]`, i.e. they add `val` to the base of the current table,
//! not to the base of the arena.
//!
//! # Fixed tables without a pointer comparison
//!
//! C tells a fixed table from a dynamic one by comparing pointers: `inflateCopy`
//! asks whether `lencode` points inside `state->codes` (`inflate.c` L1357-L1361) and
//! rebases it into the copy only then. Safe Rust has no pointer to interrogate, so
//! the distinction is made explicit instead -- [`CodeTableSource`] is either
//! [`CodeTableSource::Fixed`] or a [`CodeTableSource::Dynamic`] offset into the
//! arena. [`inflate_fixed`] selects the first; the `inflate_table` call sites select
//! the second. Copying a stream then needs no rebasing at all, because an offset
//! means the same thing in the copy as it did in the original.
//!
//! # Panic and safety posture
//!
//! `lens` reaches [`inflate_table`] straight from the compressed stream (the
//! `CODELENS` state of `inflate()`), so every value in it is attacker controlled.
//! The function is written to be total over that input: it contains no `unsafe`, no
//! `unwrap`, no `expect` and no indexing operator, every table write goes through
//! `get_mut`, every subtraction that is not provably non-negative goes through
//! `checked_sub`, and every loop is bounded by [`MAXBITS`] or by the symbol count.
//! A malformed length set is reported as [`TABLE_INVALID_CODE`] or
//! [`TABLE_NOT_ENOUGH`]; it can neither panic nor loop forever.
//!
//! `inftrees.c` L97-L100 records a precondition -- that every entry of `lens` is in
//! `0..=MAXBITS` -- which the C code assumes but does not check. This port checks
//! it, because relying on a caller's promise for memory safety is exactly what the
//! rewrite exists to eliminate. Honest callers are unaffected: the check is one
//! comparison per symbol, on a path that already touches every symbol once.
//!
//! # Examples
//!
//! ```ignore
//! // The 19-symbol code-length code of RFC 1951 §3.2.7, all lengths 4.
//! let mut lens = [0u16; 19];
//! for len in lens.iter_mut().take(16) {
//!     *len = 4;
//! }
//!
//! let mut arena = [Code::ZERO; ENOUGH];
//! let mut work = [0u16; 19];
//! let mut cursor = 0;
//! let mut bits = 7; // the root size `inflate.c` L812 asks for
//!
//! let status = inflate_table(
//!     CodeType::Codes, &lens, 19, &mut arena, &mut cursor, &mut bits, &mut work,
//! );
//!
//! assert_eq!(status, TABLE_OK);
//! assert_eq!(bits, 4); // clamped down: no code is longer than 4 bits
//! assert_eq!(cursor, 16); // 2^4 entries consumed
//! ```

// -----------------------------------------------------------------------------
//  Limits
// -----------------------------------------------------------------------------

/// The longest code length a DEFLATE Huffman code may use.
///
/// RFC 1951 §3.2.7 caps code lengths at 15 for both the literal/length and the
/// distance alphabet, and the code-length alphabet that describes them is itself
/// capped at 7. One bound therefore covers every alphabet, and it sizes the
/// per-length bookkeeping arrays inside [`inflate_table`].
///
/// Ported from `inftrees.c` L23 (`MAXBITS`).
pub const MAXBITS: usize = 15;

/// Maximum number of [`Code`] entries a literal/length table can occupy.
///
/// Ported from `inftrees.h` L49 (`ENOUGH_LENS`).
///
/// As `inftrees.h` L38-L48 explains, this is not a guess: it was found by
/// exhaustive search with `examples/enough.c`, where `enough 286 9 15` reports 852.
/// The `9` is the root table size that `inflate.c` L890 requests for a
/// literal/length table, so the constant is only valid *for that root size*.
/// Changing the root would require recomputing this number, which is why
/// `inflate.c` L885-L887 carries an explicit warning against changing it.
pub const ENOUGH_LENS: usize = 852;

/// Maximum number of [`Code`] entries a distance table can occupy.
///
/// Ported from `inftrees.h` L50 (`ENOUGH_DISTS`).
///
/// The companion of [`ENOUGH_LENS`], found the same way: `enough 30 6 15` reports
/// 592, where `6` is the root table size `inflate.c` L899 requests for a distance
/// table. It is likewise invalid for any other root size.
pub const ENOUGH_DISTS: usize = 592;

/// Size of the per-stream decode-table arena: one literal/length table plus one
/// distance table, worst case.
///
/// Ported from `inftrees.h` L51 (`ENOUGH`).
///
/// This is the length of `codes[ENOUGH]` in `struct inflate_state`
/// (`inflate.h` L122) and therefore the length of the arena passed to
/// [`inflate_table`]. `test/infcover.c` includes `inftrees.h` directly and sizes a
/// fixture from this value, so it must stay exactly 1444.
pub const ENOUGH: usize = ENOUGH_LENS + ENOUGH_DISTS;

// -----------------------------------------------------------------------------
//  Table entries
// -----------------------------------------------------------------------------

/// One entry of a Huffman decode table: four bytes describing what to do with the
/// code that indexed it.
///
/// Ported from `inftrees.h` L24-L28 (`struct code`), with the explanation at
/// `inftrees.h` L11-L23: each entry either carries the information needed to act on
/// the code that selected it, or points at another table that indexes more bits of
/// that code. For a table link, the low four bits of `op` are the index width of
/// the table pointed to. For a length or a distance, the low four bits of `op` are
/// the number of extra bits to read after the code. `bits` is how many bits of the
/// bit buffer this entry accounts for, and `val` is the literal byte, the base
/// length or distance, or the offset from the current table to the next one.
///
/// # `op` encodings
///
/// Reproduced from `inftrees.h` L30-L36:
///
/// | `op` | Meaning |
/// |---|---|
/// | `00000000` | literal |
/// | `0000tttt` | table link; `tttt != 0` is the number of table index bits |
/// | `0001eeee` | length or distance; `eeee` is the number of extra bits |
/// | `01100000` | end of block (96 = 32 + 64) |
/// | `01000000` | invalid code (64) |
///
/// # Layout
///
/// The fields are `pub` and are read as raw bits rather than through an
/// abstraction, because that is how the decoders use them: `inffast.c` tests
/// `op & 16` (L119), `op & 64` (L264, L274) and `op & 32` (L278) directly, and the
/// predicates below exist to name those tests, not to replace them.
///
/// This type is deliberately **not** `#[repr(C)]`. Nothing in this crate crosses
/// the C ABI -- that is the facade crate's job -- and no C caller ever sees a
/// `Code`, because `inftrees.h` is a private header. Rust's default layout for
/// `{u8, u8, u16}` is nevertheless the same four bytes as the C struct, which the
/// tests assert, since `struct inflate_state` budgets `4 * ENOUGH` bytes for the
/// arena and `test/infcover.c` accounts for that budget when it limits allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Code {
    /// Operation, extra bits, or table index bits -- see the `op` table above.
    pub op: u8,
    /// Number of bits of the bit buffer this part of the code consumes.
    pub bits: u8,
    /// Literal value, base length or distance, or offset to the next table.
    pub val: u16,
}

impl Code {
    /// The all-zero entry.
    ///
    /// Useful for initialising an arena before it is filled, which is the only
    /// thing a zeroed entry means: read as a real entry it would decode as the
    /// literal `0` consuming no bits. C leaves `state->codes` uninitialised for the
    /// same reason -- nothing may read an entry [`inflate_table`] has not written.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Assembles an entry from its three fields.
    ///
    /// The argument order is the declaration order of the C struct
    /// (`inftrees.h` L24-L28), which is also the order `makefixed()` prints them in
    /// (`inftrees.c` L405) and therefore the order the committed tables in
    /// `inffixed.h` read.
    #[must_use]
    pub const fn new(op: u8, bits: u8, val: u16) -> Self {
        Self { op, bits, val }
    }

    /// Returns `true` when this entry is a literal byte, i.e. `op == 0`.
    ///
    /// Mirrors the `00000000` row of `inftrees.h` L31.
    #[must_use]
    pub const fn is_literal(self) -> bool {
        self.op == 0
    }

    /// Returns `true` when this entry links to a second-level table.
    ///
    /// Mirrors the test `here.op && (here.op & 0xf0) == 0` at `inflate.c` L930: a
    /// non-zero `op` whose high nibble is clear is the `0000tttt` row of
    /// `inftrees.h` L32.
    #[must_use]
    pub const fn is_table_link(self) -> bool {
        self.op != 0 && self.op & 0xf0 == 0
    }

    /// Returns `true` when this entry carries a length or distance base value.
    ///
    /// Mirrors `op & 16` at `inffast.c` L119 and L144, i.e. the `0001eeee` row of
    /// `inftrees.h` L33.
    #[must_use]
    pub const fn is_length_or_distance(self) -> bool {
        self.op & 16 != 0
    }

    /// Returns `true` when this entry is the end-of-block symbol.
    ///
    /// Mirrors `op & 32` at `inffast.c` L278. `inflate_table` writes `32 + 64` for
    /// end of block (`inftrees.c` L233), so bit 5 alone identifies it.
    #[must_use]
    pub const fn is_end_of_block(self) -> bool {
        self.op & 32 != 0
    }

    /// Returns `true` when this entry marks an invalid code.
    ///
    /// Mirrors the final `else` of the decoder's cascade: not a length or distance
    /// (`op & 16 == 0`), not a table link (`op & 64 != 0`) and not end of block
    /// (`op & 32 == 0`) -- `inffast.c` L119, L274 and L278.
    ///
    /// This accepts more than the literal 64 that `inftrees.c` L127 and L301 write,
    /// and that is intentional: the `LEXT` sentinels 68 and 193 and the `DEXT`
    /// sentinels 64 also land here, which is exactly what makes the symbols they
    /// stand for undecodable.
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.op & 16 == 0 && self.op & 64 != 0 && self.op & 32 == 0
    }

    /// Number of extra bits to read after this length or distance code.
    ///
    /// Only meaningful when [`Self::is_length_or_distance`] holds. Mirrors
    /// `op & 15` at `inffast.c` L120 and L145.
    #[must_use]
    pub const fn extra_bits(self) -> u8 {
        self.op & 15
    }

    /// Index width, in bits, of the table this entry links to.
    ///
    /// Only meaningful when [`Self::is_table_link`] holds. Mirrors the use of
    /// `last.op` as a shift width at `inflate.c` L932 and `inffast.c` L266.
    #[must_use]
    pub const fn table_bits(self) -> u8 {
        self.op & 15
    }
}

/// Which of the three DEFLATE alphabets [`inflate_table`] is building a table for.
///
/// Ported from `inftrees.h` L54-L58 (`codetype`), in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeType {
    /// The code-length code: the 19-symbol alphabet of RFC 1951 §3.2.7 that
    /// describes the other two codes. Built with a root of 7 at `inflate.c` L812.
    ///
    /// Its symbols are neither literals nor lengths, so no base or extra table
    /// applies, and an incomplete set is always an error.
    Codes,
    /// The literal/length code: the merged 0..=285 alphabet of RFC 1951 §3.2.5.
    /// Built with a root of 9 at `inflate.c` L890.
    Lens,
    /// The distance code: the 0..=29 alphabet of RFC 1951 §3.2.5. Built with a
    /// root of 6 at `inflate.c` L899.
    Dists,
}

// -----------------------------------------------------------------------------
//  Base and extra-bit tables (RFC 1951 §3.2.5)
// -----------------------------------------------------------------------------
//
// The four arrays below are transcribed element for element from `inftrees.c`
// L69-L82. Their lengths are load-bearing and their tail values are deliberate:
// symbols 286 and 287 of the literal/length alphabet, and symbols 30 and 31 of the
// distance alphabet, are legal to *describe* in a code but can never occur in the
// data (RFC 1951 §3.2.6, "Literal/length values 286-287 will never actually occur"
// and "Note that distance codes 30-31 will never actually occur"). Rather than
// special-case them, `inflate_table` reads their entries straight out of these
// tables, and the values stored there make the resulting entry reject the symbol.
// Nothing here may be "tidied up".

/// Base match length for each length code, i.e. symbols 257..=285 of the
/// literal/length alphabet.
///
/// Ported from `inftrees.c` L69-L71 (`lbase[31]`). Indexed by `symbol - 257`, so
/// index 0 is code 257 (length 3) and index 28 is code 285 (length 258), matching
/// the table at RFC 1951 §3.2.5. The two trailing zeros are placeholders for the
/// never-occurring symbols 286 and 287.
const LBASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258, 0, 0,
];

/// `op` value for each length code: `16 + extra_bits`, or an invalid-code marker.
///
/// Ported from `inftrees.c` L72-L74 (`lext[31]`). Bit 4 (16) marks the entry as a
/// length, and the low nibble carries the extra-bit count of RFC 1951 §3.2.5, so
/// index 0 is `16` (code 257, no extra bits) and index 8 is `17` (code 265, one
/// extra bit).
///
/// The last two entries are **not** of that form and must not be normalised: `68`
/// (index 29, symbol 286) and `193` (index 30, symbol 287) both have bit 6 (64) set
/// and bits 4 and 5 clear, which is precisely the combination the decoders classify
/// as an invalid code (`inffast.c` L119, L274, L278). `inffixed.h` rewrites these
/// two `op` values to a plain 64 in the committed fixed table -- see the printf at
/// `inftrees.c` L405 -- but the effect on decoding is the same either way.
const LEXT: [u16; 31] = [
    16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20,
    21, 21, 21, 21, 16, 68, 193,
];

/// Base match distance for each distance code, i.e. symbols 0..=29 of the distance
/// alphabet.
///
/// Ported from `inftrees.c` L75-L78 (`dbase[32]`). Indexed by the symbol itself, so
/// index 0 is distance 1 and index 29 is distance 24577, matching the table at
/// RFC 1951 §3.2.5. The two trailing zeros are placeholders for the never-occurring
/// symbols 30 and 31.
const DBASE: [u16; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577, 0, 0,
];

/// `op` value for each distance code: `16 + extra_bits`, or an invalid-code marker.
///
/// Ported from `inftrees.c` L79-L82 (`dext[32]`). Same encoding as [`LEXT`], so
/// index 0 is `16` (distance 1, no extra bits) and index 29 is `29` (distances
/// 24577..=32768, thirteen extra bits).
///
/// The last two entries are the invalid-code marker `64` for the never-occurring
/// symbols 30 and 31, and must not be normalised to `16 + something`.
const DEXT: [u16; 32] = [
    16, 16, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26,
    27, 27, 28, 28, 29, 29, 64, 64,
];

// -----------------------------------------------------------------------------
//  Status codes
// -----------------------------------------------------------------------------
//
// `inflate_table` reports a tri-state, described at `inftrees.c` L39-L41: "On
// return, zero is success, -1 is an invalid code, and +1 means that ENOUGH isn't
// enough." The numeric values are part of the contract because `inflate.c` and
// `infback.c` branch on them, so they are named here rather than changed. These are
// deliberately NOT `crate::error::ReturnCode` values: -1 and +1 happen to coincide
// with `Z_ERRNO` and `Z_STREAM_END` numerically while meaning something completely
// different, and conflating the two would be a bug waiting to happen.

/// [`inflate_table`] succeeded; the table is built and the cursor advanced.
///
/// Ported from the `return 0` at `inftrees.c` L133 and L310.
pub(crate) const TABLE_OK: i32 = 0;

/// [`inflate_table`] rejected the code: the length set is over-subscribed, or it is
/// incomplete in a position where an incomplete code is not allowed.
///
/// Ported from the `return -1` at `inftrees.c` L144 and L147. Both call sites in
/// `inflate.c` (L815, L893, L902) turn any non-zero status into a data error, so the
/// distinction from [`TABLE_NOT_ENOUGH`] matters to diagnostics rather than to
/// control flow.
pub(crate) const TABLE_INVALID_CODE: i32 = -1;

/// [`inflate_table`] ran out of table space: the code needs more entries than
/// [`ENOUGH_LENS`] or [`ENOUGH_DISTS`] allows.
///
/// Ported from the `return 1` at `inftrees.c` L218 and L287. Real streams cannot
/// provoke this, because the two maxima were computed to cover every code the format
/// permits at the root sizes `inflate.c` uses; `test/infcover.c` `cover_trees()`
/// (L618-L637) reaches it by calling `inflate_table` directly with a root of 15.
pub(crate) const TABLE_NOT_ENOUGH: i32 = 1;

// -----------------------------------------------------------------------------
//  Decode-table construction
// -----------------------------------------------------------------------------

/// Reads one element of a fixed-length bookkeeping array without the possibility of
/// a panic, treating an out-of-range index as the default value.
///
/// Every index this is called with is provably in `0..=MAXBITS`, because
/// [`inflate_table`] rejects a length outside that range before any of these arrays
/// is consulted. The fallback exists so that the compiler, and not a comment, is
/// what rules out a panic: the workspace denies `clippy::indexing_slicing` precisely
/// so that no bounds proof rests on review alone.
#[inline]
fn get_or_default<T: Copy + Default>(items: &[T], index: usize) -> T {
    items.get(index).copied().unwrap_or_default()
}

/// Whether a table of `used` entries exceeds the space guaranteed for `code_type`.
///
/// Ported from the two identical space checks at `inftrees.c` L216-L218 and
/// L285-L287. [`CodeType::Codes`] deliberately has no bound, exactly as in C: the
/// code-length code is built with a root of 7 (`inflate.c` L812) from lengths that
/// are 3-bit values, so no code can exceed the root and no sub-table can ever be
/// created. The `ENOUGH_*` derivation at `inftrees.h` L38-L48 covers only the
/// literal/length and distance roots for the same reason.
#[inline]
const fn exceeds_enough(code_type: CodeType, used: usize) -> bool {
    match code_type {
        CodeType::Codes => false,
        CodeType::Lens => used > ENOUGH_LENS,
        CodeType::Dists => used > ENOUGH_DISTS,
    }
}

/// Builds the decode tables for one canonical Huffman code.
///
/// Ported from `inflate_table`, `inftrees.c` L46-L311.
///
/// The code is given by its lengths, `lens[0..codes]`, where a length of 0 means
/// the symbol does not occur. The tables are written into `table` starting at
/// `*table_index`, and `work` is scratch space of at least `codes` entries. As
/// `inftrees.c` L34-L45 puts it: `bits` is the requested root table index width on
/// entry and the actual width on return, and the two differ when the request is
/// greater than the longest code or less than the shortest one.
///
/// # Returns
///
/// * [`TABLE_OK`] -- the table was built.
/// * [`TABLE_INVALID_CODE`] -- the length set is over-subscribed, or incomplete
///   where an incomplete code is not permitted.
/// * [`TABLE_NOT_ENOUGH`] -- the code needs more entries than [`ENOUGH_LENS`] or
///   [`ENOUGH_DISTS`] allows.
///
/// # How the code is constructed
///
/// Transcribed from `inftrees.c` L84-L113. A canonical Huffman code is generated by
/// first sorting the symbols by length from short to long, retaining symbol order
/// among codes of equal length. The code then starts with all zero bits for the
/// first code of the shortest length, codes of the same length are integer
/// increments, and zeros are appended as the length increases. For the DEFLATE
/// format these bits are stored backwards from their natural integer ordering, so
/// the loop below increments the code *backwards* -- see the comment on the
/// increment step.
///
/// The sort itself is a counting sort: count the codes of each length, turn those
/// counts into a starting index per length, then write each symbol into its slot in
/// `work`. The counts are then reused for several other purposes -- finding the
/// shortest and longest code, deciding whether the set is valid at all, and looking
/// ahead to size sub-tables.
///
/// # How the tables are filled
///
/// Transcribed from `inftrees.c` L158-L187. The table being filled starts at
/// `next_base` and is indexed with `curr` bits. The code in hand is `huff`, of
/// length `len`, and it is turned into an index by dropping `drop_bits` bits off the
/// bottom. Where `len` is less than `drop_bits + curr`, the top
/// `drop_bits + curr - len` bits are stepped through every value, filling the table
/// with replicated entries.
///
/// `root` is the index width of the root table. When `len` exceeds `root`,
/// sub-tables are created, pointed at by the root entry whose index is the low
/// `root` bits of `huff`; that index is remembered in `low` so the next code can
/// tell whether it still belongs to the same sub-table. `drop_bits` is zero while
/// the root table is being filled and `root` afterwards. Sizing a new sub-table
/// requires looking ahead in the length counts, which is why the counts are
/// decremented as codes are consumed. `used` tracks how many entries have been
/// taken from `table`, and is what the [`ENOUGH_LENS`] and [`ENOUGH_DISTS`] checks
/// test.
///
/// `sym` steps through every symbol and the loop ends once all codes of length
/// `max` have been placed. Incomplete codes are permitted in one narrow case, so a
/// final step after the loop fills the one remaining entry with an invalid-code
/// marker.
///
/// # Preconditions, and what happens when they are broken
///
/// `inftrees.c` L97-L100 states that the routine "assumes, but does not check, that
/// all of the entries in `lens[]` are in the range `0..MAXBITS`", and that the
/// caller must assure it. `inflate()` does: the `CODELENS` state can only produce
/// lengths of 0..=15. This port nevertheless checks, because `lens` is filled from
/// the compressed stream and a safe-Rust core must not depend on a caller's promise
/// to stay in bounds. A violated precondition -- an out-of-range length, a `lens` or
/// `work` slice shorter than `codes`, or a `*table_index` past the end of `table` --
/// is reported as an error rather than trusted, and no input of any shape can make
/// this function panic or fail to terminate.
// Narrowing casts below are `as` rather than `try_from` because every one of them is
// provably in range and the C original is an unchecked truncation in the same place:
// `len`, `drop_bits`, `curr` and `root` are all bounded by MAXBITS (15); `LEXT` and
// `DEXT` hold no value above 193 (`inftrees.c` L74, L82); the symbol written as a
// literal came from a `u16`; and the sub-table offset is explicitly range-checked
// against `u16::MAX` before it is cast.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn inflate_table(
    code_type: CodeType,
    lens: &[u16],
    codes: usize,
    table: &mut [Code],
    table_index: &mut usize,
    bits: &mut usize,
    work: &mut [u16],
) -> i32 {
    // Defensive precondition checks. C states these as caller obligations; here they
    // are the reason every later index is provably in range. Once `entry_base` is
    // known not to exceed `table.len()`, and `table.len()` cannot exceed
    // `isize::MAX`, no `entry_base + small` sum below can overflow.
    if lens.len() < codes || work.len() < codes {
        return TABLE_INVALID_CODE;
    }
    let entry_base = *table_index;
    if entry_base > table.len() {
        return TABLE_NOT_ENOUGH;
    }

    // Accumulate lengths for codes (`inftrees.c` L115-L119). C's `count` is an array
    // of `unsigned short`, which is wide enough for the at most 320 symbols any real
    // caller passes; `usize` is used here so that no symbol count can overflow it,
    // whatever the caller does. The `else` arm is the precondition check: a length
    // above MAXBITS has no counter, and in C would have written past the array.
    let mut count = [0_usize; MAXBITS + 1];
    for &length in lens.iter().take(codes) {
        let Some(counter) = count.get_mut(usize::from(length)) else {
            return TABLE_INVALID_CODE;
        };
        *counter += 1;
    }

    // Bound code lengths and force the root to lie within them
    // (`inftrees.c` L121-L137). `max` ends at 0 when no symbol has a length, which
    // is how C's `for (max = MAXBITS; max >= 1; max--)` leaves it.
    let mut root = *bits;
    let mut max = 0;
    for length in (1..=MAXBITS).rev() {
        if get_or_default(&count, length) != 0 {
            max = length;
            break;
        }
    }
    if root > max {
        root = max;
    }
    if max == 0 {
        // No symbols to code at all (`inftrees.c` L126-L134). C writes the same
        // invalid-code entry twice through `*(*table)++ = here`, sets the root width
        // to 1 and returns success: the caller gets a table that is guaranteed to
        // fail at decode time, which is where the error belongs. A one-entry table
        // would be indexed with zero bits, so two entries is the smallest table the
        // decoder can be handed.
        let marker = Code::new(64, 1, 0);
        for offset in 0..2 {
            let Some(slot) = table.get_mut(entry_base + offset) else {
                return TABLE_NOT_ENOUGH;
            };
            *slot = marker;
        }
        *table_index = entry_base + 2;
        *bits = 1;
        return TABLE_OK;
    }
    let mut min = max;
    for length in 1..max {
        if get_or_default(&count, length) != 0 {
            min = length;
            break;
        }
    }
    if root < min {
        root = min;
    }

    // Check for an over-subscribed or incomplete set of lengths
    // (`inftrees.c` L139-L147). C keeps a signed count of available prefix codes and
    // tests `left < 0` after subtracting; testing `taken > left` before subtracting
    // is the same predicate in unsigned arithmetic, and cannot overflow or wrap.
    // `left` stays within `0..=1 << MAXBITS`.
    let mut left = 1_usize;
    for length in 1..=MAXBITS {
        left <<= 1;
        let taken = get_or_default(&count, length);
        if taken > left {
            return TABLE_INVALID_CODE;
        }
        left -= taken;
    }
    if left > 0 && (code_type == CodeType::Codes || max != 1) {
        // An incomplete set is an error everywhere except for a literal/length or
        // distance code consisting of a single one-bit code, which the format does
        // permit; the loop below completes such a table with an invalid-code marker.
        return TABLE_INVALID_CODE;
    }

    // Generate offsets into the symbol table for each length, for sorting
    // (`inftrees.c` L149-L152). `offs[1]` is 0, which the zero-initialised array
    // already provides; `offs[0]` is never read, exactly as in C.
    let mut offs = [0_usize; MAXBITS + 1];
    for length in 1..MAXBITS {
        let running = get_or_default(&offs, length) + get_or_default(&count, length);
        let Some(slot) = offs.get_mut(length + 1) else {
            return TABLE_INVALID_CODE;
        };
        *slot = running;
    }

    // Sort symbols by length, and by symbol order within each length
    // (`inftrees.c` L154-L156).
    for (symbol, &length) in lens.iter().enumerate().take(codes) {
        if length == 0 {
            continue;
        }
        let position = get_or_default(&offs, usize::from(length));
        let Some(slot) = work.get_mut(position) else {
            return TABLE_INVALID_CODE;
        };
        // Truncating exactly as C's `(unsigned short)sym` does. Symbol indices are
        // bounded by `codes`, which is at most 320 for every caller in the crate.
        *slot = symbol as u16;
        let Some(next_position) = offs.get_mut(usize::from(length)) else {
            return TABLE_INVALID_CODE;
        };
        *next_position = position + 1;
    }

    // -----------------------------------------------------------------------------
    //  Create and fill in the decoding tables
    // -----------------------------------------------------------------------------

    // Set up for the code type (`inftrees.c` L189-L202). Note that the `DISTS` arm
    // of C's switch has no `break` and never assigns `match`, so it falls out of the
    // switch leaving the initialiser from L66 in place. That is reproduced
    // deliberately: with `match_symbol` at 0 every distance symbol takes the
    // base/extra branch below, which is exactly what a distance code needs.
    //
    // `Codes` keeps both tables empty because it must never consult them, and it
    // never can: `match_symbol` is 20 while the alphabet has 19 symbols, so
    // `symbol + 1 < match_symbol` holds for every symbol and the literal branch is
    // the only reachable one.
    let (base, extra, match_symbol): (&[u16], &[u16], usize) = match code_type {
        CodeType::Codes => (&[], &[], 20),
        CodeType::Lens => (&LBASE, &LEXT, 257),
        CodeType::Dists => (&DBASE, &DEXT, 0),
    };

    // Initialise state for the loop (`inftrees.c` L204-L218).
    let mut huff = 0; // starting code, stored backwards
    let mut sym = 0; // index into the sorted symbol list
    let mut len = min; // length of the code in hand
    let mut next_base = entry_base; // start of the table being filled
    let mut curr = root; // index width of that table
    let mut drop_bits = 0; // C's `drop`: code bits dropped to index that table
    let mut low = usize::MAX; // C's `(unsigned)(-1)`: no sub-table started yet
    let mut used = 1_usize << root; // entries taken from `table`
    let mask = used - 1; // mask selecting the root index out of `huff`

    if exceeds_enough(code_type, used) {
        // Checked before anything is written, which `test/infcover.c`
        // `cover_trees()` depends on: it hands in a 592-entry fixture and a root of
        // 15, so a write here would run off the end of the caller's array.
        return TABLE_NOT_ENOUGH;
    }

    loop {
        // Create the table entry for the current symbol (`inftrees.c` L222-L235).
        let Some(&symbol) = work.get(sym) else {
            return TABLE_INVALID_CODE;
        };
        let entry_bits = (len - drop_bits) as u8;
        let here = if usize::from(symbol) + 1 < match_symbol {
            // A literal: `op` 0, and the value is the byte itself.
            Code::new(0, entry_bits, symbol)
        } else if usize::from(symbol) >= match_symbol {
            // A length or a distance: take the extra-bit count and the base value
            // from the tables selected above.
            let index = usize::from(symbol) - match_symbol;
            let (Some(&op), Some(&val)) = (extra.get(index), base.get(index)) else {
                // Unreachable for every caller in the crate, because `inflate.c`
                // L878 rejects `nlen > 286` and `ndist > 30` before building a
                // table. C would have read past the end of `lext`/`dext` here.
                return TABLE_INVALID_CODE;
            };
            Code::new(op as u8, entry_bits, val)
        } else {
            // End of block: `32 + 64`, per `inftrees.c` L233.
            Code::new(32 + 64, entry_bits, 0)
        };

        // Replicate the entry for every index whose low `len` bits equal `huff`
        // (`inftrees.c` L237-L244).
        let increment = 1_usize << (len - drop_bits);
        let mut fill = 1_usize << curr;
        // C reuses `min` to carry the size of the table being filled down to the
        // sub-table step (assigned at `inftrees.c` L240, consumed at L271). Both
        // uses are in the same iteration, so this is a per-iteration binding here;
        // reusing the name `min` would be actively confusing in Rust, where the
        // minimum code length is still live.
        let next_table_offset = fill;
        loop {
            let Some(next_fill) = fill.checked_sub(increment) else {
                // Unreachable: `len - drop_bits <= curr` holds for every entry
                // placed in a table of `curr` index bits, which is what the
                // sub-table sizing below guarantees. Checked rather than asserted
                // so that the guarantee cannot become a panic.
                return TABLE_INVALID_CODE;
            };
            fill = next_fill;
            let Some(slot) = table.get_mut(next_base + (huff >> drop_bits) + fill) else {
                return TABLE_NOT_ENOUGH;
            };
            *slot = here;
            if fill == 0 {
                break;
            }
        }

        // Increment the `len`-bit code `huff` backwards (`inftrees.c` L246-L255).
        // DEFLATE stores Huffman codes with the bits reversed, so the natural
        // integer increment runs from the top bit down: find the highest bit not
        // already set, clear everything below it, and set it.
        let mut increment = 1_usize << (len - 1);
        while huff & increment != 0 {
            increment >>= 1;
        }
        if increment == 0 {
            huff = 0;
        } else {
            huff &= increment - 1;
            huff += increment;
        }

        // Move to the next symbol, and to its length once this length is exhausted
        // (`inftrees.c` L257-L262).
        sym += 1;
        let Some(counter) = count.get_mut(len) else {
            return TABLE_INVALID_CODE;
        };
        let Some(remaining) = counter.checked_sub(1) else {
            // Unreachable: a length is only ever decremented once per symbol of
            // that length. Checked so that a broken invariant cannot underflow.
            return TABLE_INVALID_CODE;
        };
        *counter = remaining;
        if remaining == 0 {
            if len == max {
                break;
            }
            let Some(&next_symbol) = work.get(sym) else {
                return TABLE_INVALID_CODE;
            };
            let Some(&next_len) = lens.get(usize::from(next_symbol)) else {
                return TABLE_INVALID_CODE;
            };
            len = usize::from(next_len);
        }

        // Create a new sub-table if this code no longer fits the root table and does
        // not belong to the sub-table already in progress (`inftrees.c` L264-L294).
        if len > root && (huff & mask) != low {
            if drop_bits == 0 {
                // First time here: switch from filling the root table to filling
                // sub-tables.
                drop_bits = root;
            }

            // Step past the table just finished.
            next_base += next_table_offset;

            // Size the new table by looking ahead at how many codes are still to
            // come. C subtracts and tests `left <= 0`; testing `taken >= left`
            // before subtracting keeps `left` positive and cannot wrap.
            curr = len - drop_bits;
            let mut left = 1_usize << curr;
            while curr + drop_bits < max {
                let taken = get_or_default(&count, curr + drop_bits);
                if taken >= left {
                    break;
                }
                left -= taken;
                curr += 1;
                left <<= 1;
            }

            used += 1_usize << curr;
            if exceeds_enough(code_type, used) {
                return TABLE_NOT_ENOUGH;
            }

            // Point the root entry at the sub-table. The offset is relative to the
            // base of the table holding the link -- `next - *table` at
            // `inftrees.c` L293 -- because that is how the decoder follows it
            // (`inflate.c` L932, `inffast.c` L266).
            low = huff & mask;
            let link_offset = next_base - entry_base;
            if link_offset > usize::from(u16::MAX) {
                // Unreachable while `used` stays within ENOUGH; guarding it keeps a
                // silent truncation from ever producing a table that decodes wrong.
                return TABLE_NOT_ENOUGH;
            }
            let Some(slot) = table.get_mut(entry_base + low) else {
                return TABLE_NOT_ENOUGH;
            };
            *slot = Code::new(curr as u8, root as u8, link_offset as u16);
        }
    }

    // Fill in the one remaining entry if the code is incomplete
    // (`inftrees.c` L297-L305). There can be at most one, because the only
    // incomplete code that gets this far is a single one-bit code.
    if huff != 0 {
        let Some(slot) = table.get_mut(next_base + huff) else {
            return TABLE_NOT_ENOUGH;
        };
        *slot = Code::new(64, (len - drop_bits) as u8, 0);
    }

    // Return parameters (`inftrees.c` L307-L310): advance the caller's cursor past
    // the entries this call took, and report the root width actually used.
    *table_index = entry_base + used;
    *bits = root;
    TABLE_OK
}

// -----------------------------------------------------------------------------
//  Which tables the decoder is currently using
// -----------------------------------------------------------------------------

/// Root index width of the fixed literal/length table.
///
/// Ported from `state->lenbits = 9` at `inftrees.c` L369. It is also the exponent
/// behind the length of `lenfix[512]` at `inffixed.h` L10.
pub const FIXED_LENBITS: usize = 9;

/// Root index width of the fixed distance table.
///
/// Ported from `state->distbits = 5` at `inftrees.c` L371. It is also the exponent
/// behind the length of `distfix[32]` at `inffixed.h` L87.
pub const FIXED_DISTBITS: usize = 5;

/// Where the decode table a stream is using actually lives.
///
/// This replaces a pointer comparison. In C, `lencode` and `distcode` are bare
/// `code *` pointers that may point either into the immutable fixed tables of
/// `inffixed.h` or into the stream's own `codes[ENOUGH]` arena, and the only way to
/// tell which is to test the pointer against the bounds of the arena -- which is
/// what `inflateCopy` does at `inflate.c` L1357-L1361, so that it can rebase the
/// pointers into the copy's arena.
///
/// Naming the two cases makes that test unnecessary: [`Self::Dynamic`] holds an
/// offset, and an offset is equally valid in a copy, so copying a stream needs no
/// fix-up at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTableSource {
    /// The immutable fixed tables of RFC 1951 §3.2.6, as committed in `inffixed.h`
    /// and selected by [`inflate_fixed`]. Shared by every stream and never written.
    Fixed,
    /// A table built by [`inflate_table`], starting at this offset into the
    /// stream's own arena.
    ///
    /// The offset is the value the cursor held when [`inflate_table`] was called,
    /// which is exactly what `inflate.c` L889 and L898 record.
    Dynamic {
        /// Index of the table's first [`Code`] within the arena.
        offset: usize,
    },
}

impl CodeTableSource {
    /// Returns the arena offset when this is a dynamic table, or `None` for the
    /// fixed tables.
    ///
    /// This is the safe-Rust form of the range test at `inflate.c` L1357-L1358: a
    /// `Some` answer is the case in which C would have rebased the pointer.
    #[must_use]
    pub const fn dynamic_offset(self) -> Option<usize> {
        match self {
            Self::Fixed => None,
            Self::Dynamic { offset } => Some(offset),
        }
    }
}

impl Default for CodeTableSource {
    /// The state a freshly reset stream is in: the start of its own arena.
    ///
    /// Mirrors `state->lencode = state->distcode = state->next = state->codes` at
    /// `inflate.c` L118.
    fn default() -> Self {
        Self::Dynamic { offset: 0 }
    }
}

/// The four fields of `struct inflate_state` that say which decode tables are in
/// use and how wide their root indices are.
///
/// Ported from `inflate.h` L110-L113 (`lencode`, `distcode`, `lenbits`,
/// `distbits`). They are grouped into one type because they are always written
/// together -- by [`inflate_fixed`] for a fixed block, and by the
/// [`inflate_table`] call sites for a dynamic one -- and because grouping them
/// keeps the bit width next to the table it belongs to.
///
/// The C fields are `unsigned`; `usize` is used here because both values are shift
/// widths and slice indices in Rust, and both are bounded by [`MAXBITS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeTables {
    /// Where the literal/length table starts.
    pub lencode: CodeTableSource,
    /// Root index width of the literal/length table, i.e. how many bits
    /// `lencode` is indexed with.
    pub lenbits: usize,
    /// Where the distance table starts.
    pub distcode: CodeTableSource,
    /// Root index width of the distance table.
    pub distbits: usize,
}

impl CodeTables {
    /// The selection [`inflate_fixed`] installs: both fixed tables, with the root
    /// widths of RFC 1951 §3.2.6.
    ///
    /// Ported from the body of `inflate_fixed`, `inftrees.c` L368-L371.
    pub const FIXED: Self = Self {
        lencode: CodeTableSource::Fixed,
        lenbits: FIXED_LENBITS,
        distcode: CodeTableSource::Fixed,
        distbits: FIXED_DISTBITS,
    };

    /// The selection a freshly reset stream starts with: the base of its own arena,
    /// with root widths not yet meaningful.
    ///
    /// Mirrors `inflate.c` L118, which points both tables at `state->codes` without
    /// touching `lenbits` or `distbits` -- they are always assigned immediately
    /// before the [`inflate_table`] call that gives them meaning (`inflate.c` L812,
    /// L890, L899).
    pub const RESET: Self = Self {
        lencode: CodeTableSource::Dynamic { offset: 0 },
        lenbits: 0,
        distcode: CodeTableSource::Dynamic { offset: 0 },
        distbits: 0,
    };

    /// Returns `true` when both tables are the shared fixed ones.
    ///
    /// The two fields always agree in practice, because [`inflate_fixed`] is the
    /// only writer of [`CodeTableSource::Fixed`] and it writes both; the test is
    /// written over both so that a future partial update cannot be misread.
    #[must_use]
    pub const fn is_fixed(self) -> bool {
        matches!(self.lencode, CodeTableSource::Fixed)
            && matches!(self.distcode, CodeTableSource::Fixed)
    }
}

impl Default for CodeTables {
    /// [`Self::RESET`] -- see `inflate.c` L118.
    fn default() -> Self {
        Self::RESET
    }
}

/// Points the decoder at the fixed literal/length and distance tables of
/// RFC 1951 §3.2.6.
///
/// Ported from `inflate_fixed`, `inftrees.c` L364-L372. In the shipped
/// configuration that function is four assignments, and so is this one: the tables
/// themselves are compile-time constants (`inffixed.h`, included at `inftrees.c`
/// L351), so there is nothing to build and nothing to synchronise. The
/// `BUILDFIXED` variant at `inftrees.c` L313-L349, which constructs them on first
/// use behind a `z_once_t`, is deliberately not ported -- see the module
/// documentation.
///
/// `inflate()` calls this from the fixed-block arm of its block-type switch
/// (`inflate.c` L727), which is the only caller besides `inflateBack`.
pub(crate) fn inflate_fixed(tables: &mut CodeTables) {
    *tables = CodeTables::FIXED;
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
// The workspace denies the panic family in library code, which is the whole point of
// this module; a test that cannot assert is useless, so the harness opts back in
// here only. Indexing is allowed too, because the expectations below are written
// against literal, known-good indices.
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        inflate_fixed, inflate_table, Code, CodeTableSource, CodeTables, CodeType, DBASE, DEXT,
        ENOUGH, ENOUGH_DISTS, ENOUGH_LENS, FIXED_DISTBITS, FIXED_LENBITS, LBASE, LEXT, MAXBITS,
        TABLE_INVALID_CODE, TABLE_NOT_ENOUGH, TABLE_OK,
    };
    use core::mem::{align_of, size_of};

    /// Length of the scratch areas, matching `state->lens[320]` and
    /// `state->work[288]` at `inflate.h` L120-L121. 320 covers both.
    const SCRATCH: usize = 320;

    /// Everything a call to [`inflate_table`] reports back, kept together so a test
    /// can assert on the table and on the in/out parameters at once.
    struct Built {
        status: i32,
        arena: [Code; ENOUGH],
        cursor: usize,
        bits: usize,
    }

    /// Runs [`inflate_table`] over a freshly zeroed arena with the cursor starting at
    /// `start`, taking `codes` from the length slice.
    fn build_from(
        code_type: CodeType,
        lens: &[u16],
        codes: usize,
        root: usize,
        start: usize,
    ) -> Built {
        let mut arena = [Code::ZERO; ENOUGH];
        let mut work = [0_u16; SCRATCH];
        let mut cursor = start;
        let mut bits = root;
        let status = inflate_table(
            code_type,
            lens,
            codes,
            &mut arena,
            &mut cursor,
            &mut bits,
            &mut work,
        );
        Built {
            status,
            arena,
            cursor,
            bits,
        }
    }

    /// The common case: the whole length slice, starting at offset 0.
    fn build(code_type: CodeType, lens: &[u16], root: usize) -> Built {
        build_from(code_type, lens, lens.len(), root, 0)
    }

    /// The `op` byte an entry carries when it was built from one of the base/extra
    /// tables, which hold `u16` values because C declares them `unsigned short`.
    fn op_of(extra: u16) -> u8 {
        u8::try_from(extra).unwrap()
    }

    /// The fixed literal/length code lengths of RFC 1951 §3.2.6, built exactly as
    /// `inftrees.c` L332-L341 builds them.
    fn fixed_literal_length_lens() -> [u16; 288] {
        // The four runs of `inftrees.c` L334-L337, in the same order.
        let mut lens = [0_u16; 288];
        lens[..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..].fill(8);
        lens
    }

    /// The 16-symbol length set `test/infcover.c` `cover_trees()` uses (L626-L628):
    /// one code of every length from 1 to 15, plus a second 15-bit code, which is a
    /// complete code whose longest codes are the longest the format allows.
    fn one_of_every_length() -> [u16; 16] {
        let mut lens = [0_u16; 16];
        for (index, len) in lens.iter_mut().enumerate() {
            *len = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;
        lens
    }

    // -------------------------------------------------------------------------
    //  The ported types and tables
    // -------------------------------------------------------------------------

    #[test]
    fn code_has_the_layout_of_the_c_struct() {
        // `inftrees.h` L23: "Each entry is four bytes." `struct inflate_state`
        // budgets `4 * ENOUGH` bytes for its arena on that basis, and
        // `test/infcover.c` limits allocation against that budget.
        assert_eq!(size_of::<Code>(), 4);
        assert_eq!(align_of::<Code>(), 2);
        assert_eq!(size_of::<[Code; ENOUGH]>(), 4 * 1444);
    }

    #[test]
    fn code_predicates_match_the_op_encodings() {
        // The five rows of `inftrees.h` L30-L36.
        let literal = Code::new(0, 8, 65);
        let link = Code::new(9, 6, 64);
        let length = Code::new(16 + 5, 7, 131);
        let end_of_block = Code::new(32 + 64, 7, 0);
        let invalid = Code::new(64, 1, 0);

        assert!(literal.is_literal());
        assert!(!literal.is_table_link());
        assert!(!literal.is_invalid());

        assert!(link.is_table_link());
        assert!(!link.is_literal());
        assert!(!link.is_invalid());
        assert_eq!(link.table_bits(), 9);

        assert!(length.is_length_or_distance());
        assert!(!length.is_invalid());
        assert_eq!(length.extra_bits(), 5);

        assert!(end_of_block.is_end_of_block());
        assert!(!end_of_block.is_invalid());

        assert!(invalid.is_invalid());
        assert!(!invalid.is_end_of_block());
        assert!(!invalid.is_table_link());

        // A zeroed entry is only ever scratch, but it must still classify.
        assert!(Code::ZERO.is_literal());
        assert_eq!(Code::default(), Code::ZERO);
    }

    #[test]
    fn capacity_constants_match_the_c_header() {
        // `inftrees.h` L49-L51 and `inftrees.c` L23.
        assert_eq!(ENOUGH_LENS, 852);
        assert_eq!(ENOUGH_DISTS, 592);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(MAXBITS, 15);
        // `inffixed.h` L10 and L87 size the fixed tables from these.
        assert_eq!(1_usize << FIXED_LENBITS, 512);
        assert_eq!(1_usize << FIXED_DISTBITS, 32);
    }

    #[test]
    fn base_and_extra_tables_match_the_c_arrays() {
        // Shapes, from `inftrees.c` L69-L82. The lengths are load-bearing: the
        // sub-table lookahead indexes them by `symbol - match`.
        assert_eq!(LBASE.len(), 31);
        assert_eq!(LEXT.len(), 31);
        assert_eq!(DBASE.len(), 32);
        assert_eq!(DEXT.len(), 32);

        // Ends of the real ranges, cross-checked against RFC 1951 §3.2.5: code 257
        // is length 3, code 285 is length 258, distance code 0 is 1 and code 29 is
        // 24577.
        assert_eq!(LBASE[0], 3);
        assert_eq!(LBASE[28], 258);
        assert_eq!(DBASE[0], 1);
        assert_eq!(DBASE[29], 24577);
        assert_eq!(LEXT[0], 16);
        assert_eq!(LEXT[28], 16);
        assert_eq!(DEXT[0], 16);
        assert_eq!(DEXT[29], 29);

        // A middle row of each table, to catch a transcription slip that happens to
        // preserve both ends: code 265 has one extra bit and base 11; distance code
        // 6 has two extra bits and base 9.
        assert_eq!((LBASE[8], LEXT[8]), (11, 17));
        assert_eq!((DBASE[6], DEXT[6]), (9, 18));

        // The extra-bit counts are exactly `op - 16` over the real ranges, which is
        // what makes `Code::extra_bits` meaningful.
        for (index, &op) in LEXT.iter().enumerate().take(29) {
            assert_eq!(op & 16, 16, "LEXT[{index}] must mark a length");
            assert!(op & 15 <= 5, "no length code has more than five extra bits");
        }
        for (index, &op) in DEXT.iter().enumerate().take(30) {
            assert_eq!(op & 16, 16, "DEXT[{index}] must mark a distance");
            assert!(
                op & 15 <= 13,
                "no distance code has more than 13 extra bits"
            );
        }
    }

    #[test]
    fn never_occurring_symbols_are_marked_invalid() {
        // RFC 1951 §3.2.6: literal/length values 286-287 and distance codes 30-31
        // "will never actually occur in the compressed data". The tail values of
        // `LEXT` and `DEXT` are what make an entry built for them undecodable, so
        // they must not be normalised away (`inftrees.c` L74, L82).
        assert_eq!(LEXT[29], 68);
        assert_eq!(LEXT[30], 193);
        assert_eq!(DEXT[30], 64);
        assert_eq!(DEXT[31], 64);
        assert_eq!(LBASE[29], 0);
        assert_eq!(LBASE[30], 0);
        assert_eq!(DBASE[30], 0);
        assert_eq!(DBASE[31], 0);

        // The property that actually matters: an entry carrying one of these `op`
        // values is rejected by the decoder's cascade.
        for &op in &[LEXT[29], LEXT[30], DEXT[30], DEXT[31]] {
            let entry = Code::new(u8::try_from(op).unwrap(), 8, 0);
            assert!(entry.is_invalid(), "op {op} must decode as an invalid code");
            assert!(!entry.is_length_or_distance());
            assert!(!entry.is_end_of_block());
            assert!(!entry.is_table_link());
        }
    }

    // -------------------------------------------------------------------------
    //  Reproducing the committed fixed tables
    // -------------------------------------------------------------------------

    /// `lenfix[512]` exactly as committed at `inffixed.h` L10-L85, as
    /// `(op, bits, val)` triples in table order.
    ///
    /// Transcribed mechanically from the header. Four of these entries carry a
    /// deliberate substitution -- see
    /// [`fixed_literal_length_table_matches_inffixed_h`].
    const LENFIX: [(u8, u8, u16); 512] = [
        (96, 7, 0),
        (0, 8, 80),
        (0, 8, 16),
        (20, 8, 115),
        (18, 7, 31),
        (0, 8, 112),
        (0, 8, 48),
        (0, 9, 192),
        (16, 7, 10),
        (0, 8, 96),
        (0, 8, 32),
        (0, 9, 160),
        (0, 8, 0),
        (0, 8, 128),
        (0, 8, 64),
        (0, 9, 224),
        (16, 7, 6),
        (0, 8, 88),
        (0, 8, 24),
        (0, 9, 144),
        (19, 7, 59),
        (0, 8, 120),
        (0, 8, 56),
        (0, 9, 208),
        (17, 7, 17),
        (0, 8, 104),
        (0, 8, 40),
        (0, 9, 176),
        (0, 8, 8),
        (0, 8, 136),
        (0, 8, 72),
        (0, 9, 240),
        (16, 7, 4),
        (0, 8, 84),
        (0, 8, 20),
        (21, 8, 227),
        (19, 7, 43),
        (0, 8, 116),
        (0, 8, 52),
        (0, 9, 200),
        (17, 7, 13),
        (0, 8, 100),
        (0, 8, 36),
        (0, 9, 168),
        (0, 8, 4),
        (0, 8, 132),
        (0, 8, 68),
        (0, 9, 232),
        (16, 7, 8),
        (0, 8, 92),
        (0, 8, 28),
        (0, 9, 152),
        (20, 7, 83),
        (0, 8, 124),
        (0, 8, 60),
        (0, 9, 216),
        (18, 7, 23),
        (0, 8, 108),
        (0, 8, 44),
        (0, 9, 184),
        (0, 8, 12),
        (0, 8, 140),
        (0, 8, 76),
        (0, 9, 248),
        (16, 7, 3),
        (0, 8, 82),
        (0, 8, 18),
        (21, 8, 163),
        (19, 7, 35),
        (0, 8, 114),
        (0, 8, 50),
        (0, 9, 196),
        (17, 7, 11),
        (0, 8, 98),
        (0, 8, 34),
        (0, 9, 164),
        (0, 8, 2),
        (0, 8, 130),
        (0, 8, 66),
        (0, 9, 228),
        (16, 7, 7),
        (0, 8, 90),
        (0, 8, 26),
        (0, 9, 148),
        (20, 7, 67),
        (0, 8, 122),
        (0, 8, 58),
        (0, 9, 212),
        (18, 7, 19),
        (0, 8, 106),
        (0, 8, 42),
        (0, 9, 180),
        (0, 8, 10),
        (0, 8, 138),
        (0, 8, 74),
        (0, 9, 244),
        (16, 7, 5),
        (0, 8, 86),
        (0, 8, 22),
        (64, 8, 0),
        (19, 7, 51),
        (0, 8, 118),
        (0, 8, 54),
        (0, 9, 204),
        (17, 7, 15),
        (0, 8, 102),
        (0, 8, 38),
        (0, 9, 172),
        (0, 8, 6),
        (0, 8, 134),
        (0, 8, 70),
        (0, 9, 236),
        (16, 7, 9),
        (0, 8, 94),
        (0, 8, 30),
        (0, 9, 156),
        (20, 7, 99),
        (0, 8, 126),
        (0, 8, 62),
        (0, 9, 220),
        (18, 7, 27),
        (0, 8, 110),
        (0, 8, 46),
        (0, 9, 188),
        (0, 8, 14),
        (0, 8, 142),
        (0, 8, 78),
        (0, 9, 252),
        (96, 7, 0),
        (0, 8, 81),
        (0, 8, 17),
        (21, 8, 131),
        (18, 7, 31),
        (0, 8, 113),
        (0, 8, 49),
        (0, 9, 194),
        (16, 7, 10),
        (0, 8, 97),
        (0, 8, 33),
        (0, 9, 162),
        (0, 8, 1),
        (0, 8, 129),
        (0, 8, 65),
        (0, 9, 226),
        (16, 7, 6),
        (0, 8, 89),
        (0, 8, 25),
        (0, 9, 146),
        (19, 7, 59),
        (0, 8, 121),
        (0, 8, 57),
        (0, 9, 210),
        (17, 7, 17),
        (0, 8, 105),
        (0, 8, 41),
        (0, 9, 178),
        (0, 8, 9),
        (0, 8, 137),
        (0, 8, 73),
        (0, 9, 242),
        (16, 7, 4),
        (0, 8, 85),
        (0, 8, 21),
        (16, 8, 258),
        (19, 7, 43),
        (0, 8, 117),
        (0, 8, 53),
        (0, 9, 202),
        (17, 7, 13),
        (0, 8, 101),
        (0, 8, 37),
        (0, 9, 170),
        (0, 8, 5),
        (0, 8, 133),
        (0, 8, 69),
        (0, 9, 234),
        (16, 7, 8),
        (0, 8, 93),
        (0, 8, 29),
        (0, 9, 154),
        (20, 7, 83),
        (0, 8, 125),
        (0, 8, 61),
        (0, 9, 218),
        (18, 7, 23),
        (0, 8, 109),
        (0, 8, 45),
        (0, 9, 186),
        (0, 8, 13),
        (0, 8, 141),
        (0, 8, 77),
        (0, 9, 250),
        (16, 7, 3),
        (0, 8, 83),
        (0, 8, 19),
        (21, 8, 195),
        (19, 7, 35),
        (0, 8, 115),
        (0, 8, 51),
        (0, 9, 198),
        (17, 7, 11),
        (0, 8, 99),
        (0, 8, 35),
        (0, 9, 166),
        (0, 8, 3),
        (0, 8, 131),
        (0, 8, 67),
        (0, 9, 230),
        (16, 7, 7),
        (0, 8, 91),
        (0, 8, 27),
        (0, 9, 150),
        (20, 7, 67),
        (0, 8, 123),
        (0, 8, 59),
        (0, 9, 214),
        (18, 7, 19),
        (0, 8, 107),
        (0, 8, 43),
        (0, 9, 182),
        (0, 8, 11),
        (0, 8, 139),
        (0, 8, 75),
        (0, 9, 246),
        (16, 7, 5),
        (0, 8, 87),
        (0, 8, 23),
        (64, 8, 0),
        (19, 7, 51),
        (0, 8, 119),
        (0, 8, 55),
        (0, 9, 206),
        (17, 7, 15),
        (0, 8, 103),
        (0, 8, 39),
        (0, 9, 174),
        (0, 8, 7),
        (0, 8, 135),
        (0, 8, 71),
        (0, 9, 238),
        (16, 7, 9),
        (0, 8, 95),
        (0, 8, 31),
        (0, 9, 158),
        (20, 7, 99),
        (0, 8, 127),
        (0, 8, 63),
        (0, 9, 222),
        (18, 7, 27),
        (0, 8, 111),
        (0, 8, 47),
        (0, 9, 190),
        (0, 8, 15),
        (0, 8, 143),
        (0, 8, 79),
        (0, 9, 254),
        (96, 7, 0),
        (0, 8, 80),
        (0, 8, 16),
        (20, 8, 115),
        (18, 7, 31),
        (0, 8, 112),
        (0, 8, 48),
        (0, 9, 193),
        (16, 7, 10),
        (0, 8, 96),
        (0, 8, 32),
        (0, 9, 161),
        (0, 8, 0),
        (0, 8, 128),
        (0, 8, 64),
        (0, 9, 225),
        (16, 7, 6),
        (0, 8, 88),
        (0, 8, 24),
        (0, 9, 145),
        (19, 7, 59),
        (0, 8, 120),
        (0, 8, 56),
        (0, 9, 209),
        (17, 7, 17),
        (0, 8, 104),
        (0, 8, 40),
        (0, 9, 177),
        (0, 8, 8),
        (0, 8, 136),
        (0, 8, 72),
        (0, 9, 241),
        (16, 7, 4),
        (0, 8, 84),
        (0, 8, 20),
        (21, 8, 227),
        (19, 7, 43),
        (0, 8, 116),
        (0, 8, 52),
        (0, 9, 201),
        (17, 7, 13),
        (0, 8, 100),
        (0, 8, 36),
        (0, 9, 169),
        (0, 8, 4),
        (0, 8, 132),
        (0, 8, 68),
        (0, 9, 233),
        (16, 7, 8),
        (0, 8, 92),
        (0, 8, 28),
        (0, 9, 153),
        (20, 7, 83),
        (0, 8, 124),
        (0, 8, 60),
        (0, 9, 217),
        (18, 7, 23),
        (0, 8, 108),
        (0, 8, 44),
        (0, 9, 185),
        (0, 8, 12),
        (0, 8, 140),
        (0, 8, 76),
        (0, 9, 249),
        (16, 7, 3),
        (0, 8, 82),
        (0, 8, 18),
        (21, 8, 163),
        (19, 7, 35),
        (0, 8, 114),
        (0, 8, 50),
        (0, 9, 197),
        (17, 7, 11),
        (0, 8, 98),
        (0, 8, 34),
        (0, 9, 165),
        (0, 8, 2),
        (0, 8, 130),
        (0, 8, 66),
        (0, 9, 229),
        (16, 7, 7),
        (0, 8, 90),
        (0, 8, 26),
        (0, 9, 149),
        (20, 7, 67),
        (0, 8, 122),
        (0, 8, 58),
        (0, 9, 213),
        (18, 7, 19),
        (0, 8, 106),
        (0, 8, 42),
        (0, 9, 181),
        (0, 8, 10),
        (0, 8, 138),
        (0, 8, 74),
        (0, 9, 245),
        (16, 7, 5),
        (0, 8, 86),
        (0, 8, 22),
        (64, 8, 0),
        (19, 7, 51),
        (0, 8, 118),
        (0, 8, 54),
        (0, 9, 205),
        (17, 7, 15),
        (0, 8, 102),
        (0, 8, 38),
        (0, 9, 173),
        (0, 8, 6),
        (0, 8, 134),
        (0, 8, 70),
        (0, 9, 237),
        (16, 7, 9),
        (0, 8, 94),
        (0, 8, 30),
        (0, 9, 157),
        (20, 7, 99),
        (0, 8, 126),
        (0, 8, 62),
        (0, 9, 221),
        (18, 7, 27),
        (0, 8, 110),
        (0, 8, 46),
        (0, 9, 189),
        (0, 8, 14),
        (0, 8, 142),
        (0, 8, 78),
        (0, 9, 253),
        (96, 7, 0),
        (0, 8, 81),
        (0, 8, 17),
        (21, 8, 131),
        (18, 7, 31),
        (0, 8, 113),
        (0, 8, 49),
        (0, 9, 195),
        (16, 7, 10),
        (0, 8, 97),
        (0, 8, 33),
        (0, 9, 163),
        (0, 8, 1),
        (0, 8, 129),
        (0, 8, 65),
        (0, 9, 227),
        (16, 7, 6),
        (0, 8, 89),
        (0, 8, 25),
        (0, 9, 147),
        (19, 7, 59),
        (0, 8, 121),
        (0, 8, 57),
        (0, 9, 211),
        (17, 7, 17),
        (0, 8, 105),
        (0, 8, 41),
        (0, 9, 179),
        (0, 8, 9),
        (0, 8, 137),
        (0, 8, 73),
        (0, 9, 243),
        (16, 7, 4),
        (0, 8, 85),
        (0, 8, 21),
        (16, 8, 258),
        (19, 7, 43),
        (0, 8, 117),
        (0, 8, 53),
        (0, 9, 203),
        (17, 7, 13),
        (0, 8, 101),
        (0, 8, 37),
        (0, 9, 171),
        (0, 8, 5),
        (0, 8, 133),
        (0, 8, 69),
        (0, 9, 235),
        (16, 7, 8),
        (0, 8, 93),
        (0, 8, 29),
        (0, 9, 155),
        (20, 7, 83),
        (0, 8, 125),
        (0, 8, 61),
        (0, 9, 219),
        (18, 7, 23),
        (0, 8, 109),
        (0, 8, 45),
        (0, 9, 187),
        (0, 8, 13),
        (0, 8, 141),
        (0, 8, 77),
        (0, 9, 251),
        (16, 7, 3),
        (0, 8, 83),
        (0, 8, 19),
        (21, 8, 195),
        (19, 7, 35),
        (0, 8, 115),
        (0, 8, 51),
        (0, 9, 199),
        (17, 7, 11),
        (0, 8, 99),
        (0, 8, 35),
        (0, 9, 167),
        (0, 8, 3),
        (0, 8, 131),
        (0, 8, 67),
        (0, 9, 231),
        (16, 7, 7),
        (0, 8, 91),
        (0, 8, 27),
        (0, 9, 151),
        (20, 7, 67),
        (0, 8, 123),
        (0, 8, 59),
        (0, 9, 215),
        (18, 7, 19),
        (0, 8, 107),
        (0, 8, 43),
        (0, 9, 183),
        (0, 8, 11),
        (0, 8, 139),
        (0, 8, 75),
        (0, 9, 247),
        (16, 7, 5),
        (0, 8, 87),
        (0, 8, 23),
        (64, 8, 0),
        (19, 7, 51),
        (0, 8, 119),
        (0, 8, 55),
        (0, 9, 207),
        (17, 7, 15),
        (0, 8, 103),
        (0, 8, 39),
        (0, 9, 175),
        (0, 8, 7),
        (0, 8, 135),
        (0, 8, 71),
        (0, 9, 239),
        (16, 7, 9),
        (0, 8, 95),
        (0, 8, 31),
        (0, 9, 159),
        (20, 7, 99),
        (0, 8, 127),
        (0, 8, 63),
        (0, 9, 223),
        (18, 7, 27),
        (0, 8, 111),
        (0, 8, 47),
        (0, 9, 191),
        (0, 8, 15),
        (0, 8, 143),
        (0, 8, 79),
        (0, 9, 255),
    ];

    /// `distfix[32]` exactly as committed at `inffixed.h` L87-L93.
    const DISTFIX: [(u8, u8, u16); 32] = [
        (16, 5, 1),
        (23, 5, 257),
        (19, 5, 17),
        (27, 5, 4097),
        (17, 5, 5),
        (25, 5, 1025),
        (21, 5, 65),
        (29, 5, 16385),
        (16, 5, 3),
        (24, 5, 513),
        (20, 5, 33),
        (28, 5, 8193),
        (18, 5, 9),
        (26, 5, 2049),
        (22, 5, 129),
        (64, 5, 0),
        (16, 5, 2),
        (23, 5, 385),
        (19, 5, 25),
        (27, 5, 6145),
        (17, 5, 7),
        (25, 5, 1537),
        (21, 5, 97),
        (29, 5, 24577),
        (16, 5, 4),
        (24, 5, 769),
        (20, 5, 49),
        (28, 5, 12289),
        (18, 5, 13),
        (26, 5, 3073),
        (22, 5, 193),
        (64, 5, 0),
    ];

    #[test]
    fn fixed_literal_length_table_matches_inffixed_h() {
        // The cheapest high-signal check there is: the committed table was generated
        // by running this very algorithm (`inftrees.c` L332-L341 feeding L401-L410),
        // so reproducing it exercises the whole construction against a known answer.
        let lens = fixed_literal_length_lens();
        let built = build(CodeType::Lens, &lens, FIXED_LENBITS);

        assert_eq!(built.status, TABLE_OK);
        assert_eq!(
            built.bits, FIXED_LENBITS,
            "9 is both the shortest bound and the longest"
        );
        assert_eq!(
            built.cursor, 512,
            "a 9-bit root with no sub-table is 2^9 entries"
        );

        for (index, &(op, bits, val)) in LENFIX.iter().enumerate() {
            let entry = built.arena[index];
            assert_eq!((entry.bits, entry.val), (bits, val), "at index {index}");

            if index & 127 == 99 {
                // `makefixed()` does not print the table it built verbatim: the
                // printf at `inftrees.c` L405 substitutes an `op` of 64 at every
                // index where `(low & 127) == 99`. Those four entries stand for the
                // never-occurring symbols 286 and 287, whose real ops are
                // LEXT[29] = 68 and LEXT[30] = 193, so the committed header
                // normalises them to the plain invalid-code marker. Both forms are
                // rejected by the decoder, which is what makes the substitution
                // harmless -- and what makes this the one place the built table and
                // the committed one legitimately differ.
                assert_eq!(
                    op, 64,
                    "inffixed.h must carry the substituted op at {index}"
                );
                assert!(
                    entry.op == op_of(LEXT[29]) || entry.op == op_of(LEXT[30]),
                    "index {index} must hold a never-occurring symbol, got op {}",
                    entry.op
                );
                assert!(entry.is_invalid(), "at index {index}");
            } else {
                assert_eq!(entry.op, op, "at index {index}");
            }
        }

        assert!(
            built.arena[512..].iter().all(|&entry| entry == Code::ZERO),
            "nothing past the table may be written"
        );
    }

    #[test]
    fn fixed_distance_table_matches_inffixed_h() {
        // 32 codes of five bits each (RFC 1951 §3.2.6), built as `inftrees.c`
        // L344-L348 builds them. Unlike `lenfix`, `distfix` is printed verbatim by
        // `makefixed()`, so this comparison is exact everywhere.
        let lens = [5_u16; 32];
        let built = build(CodeType::Dists, &lens, FIXED_DISTBITS);

        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, FIXED_DISTBITS);
        assert_eq!(built.cursor, 32);

        for (index, &(op, bits, val)) in DISTFIX.iter().enumerate() {
            assert_eq!(
                built.arena[index],
                Code { op, bits, val },
                "at index {index}"
            );
        }
        assert!(built.arena[32..].iter().all(|&entry| entry == Code::ZERO));
    }

    // -------------------------------------------------------------------------
    //  The tri-state status contract
    // -------------------------------------------------------------------------

    #[test]
    fn no_symbols_at_all_yields_two_invalid_entries() {
        // `inftrees.c` L126-L134: success, a root width of 1, and two identical
        // invalid-code entries so that the error surfaces at decode time.
        let marker = Code::new(64, 1, 0);

        for (name, lens, codes) in [
            ("all lengths zero", &[0_u16; 288][..], 288),
            ("no symbols at all", &[][..], 0),
            ("codes shorter than lens", &[0_u16; 19][..], 0),
        ] {
            let built = build_from(CodeType::Lens, lens, codes, 9, 0);
            assert_eq!(built.status, TABLE_OK, "{name}");
            assert_eq!(built.bits, 1, "{name}");
            assert_eq!(built.cursor, 2, "{name}");
            assert_eq!(built.arena[0], marker, "{name}");
            assert_eq!(built.arena[1], marker, "{name}");
            assert_eq!(built.arena[2], Code::ZERO, "{name}");
        }

        // The cursor is advanced from wherever it stood, not from zero.
        let built = build_from(CodeType::Dists, &[0_u16; 30], 30, 6, 700);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 702);
        assert_eq!(built.arena[700], marker);
        assert_eq!(built.arena[701], marker);
        assert_eq!(built.arena[699], Code::ZERO);
    }

    #[test]
    fn over_subscribed_lengths_are_rejected() {
        // Three one-bit codes need a code space of 1.5 (`inftrees.c` L144).
        let mut lens = [0_u16; 32];
        lens[0] = 1;
        lens[1] = 1;
        lens[2] = 1;
        for code_type in [CodeType::Codes, CodeType::Lens, CodeType::Dists] {
            let built = build_from(code_type, &lens, 19, 6, 0);
            assert_eq!(built.status, TABLE_INVALID_CODE, "{code_type:?}");
            assert_eq!(built.cursor, 0, "a rejected code must not consume entries");
            assert_eq!(built.arena[0], Code::ZERO, "{code_type:?}");
        }

        // A subtler over-subscription: 17 codes of four bits, where 16 fit.
        let built = build(CodeType::Lens, &[4_u16; 17], 9);
        assert_eq!(built.status, TABLE_INVALID_CODE);
    }

    #[test]
    fn incomplete_lengths_are_rejected_except_for_a_single_one_bit_code() {
        // `inftrees.c` L146-L147: an incomplete set is an error unless it is a
        // literal/length or distance code whose longest code is one bit.
        let mut single = [0_u16; 32];
        single[0] = 1;

        let built = build_from(CodeType::Codes, &single, 19, 7, 0);
        assert_eq!(
            built.status, TABLE_INVALID_CODE,
            "CODES never permits an incomplete set"
        );

        // The permitted case, and the only path that reaches the incomplete-tail
        // step at `inftrees.c` L297-L305.
        let built = build(CodeType::Dists, &single, 6);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(
            built.bits, 1,
            "the root is clamped to the single one-bit code"
        );
        assert_eq!(built.cursor, 2);
        assert_eq!(built.arena[0], Code::new(op_of(DEXT[0]), 1, DBASE[0]));
        assert_eq!(built.arena[1], Code::new(64, 1, 0), "the tail marker");

        // The same single code over the full literal/length alphabet: symbol 0 is a
        // literal there rather than a distance, so `val` is the byte, not a base.
        let mut single_literal = [0_u16; 288];
        single_literal[0] = 1;
        let built = build(CodeType::Lens, &single_literal, 9);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, 1);
        assert_eq!(built.cursor, 2);
        assert_eq!(built.arena[0], Code::new(0, 1, 0), "symbol 0 is a literal");
        assert_eq!(built.arena[1], Code::new(64, 1, 0));

        // Two one-bit codes are complete, so no tail entry is written.
        let mut pair = [0_u16; 32];
        pair[0] = 1;
        pair[1] = 1;
        let built = build(CodeType::Dists, &pair, 6);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 2);
        assert_eq!(built.arena[0], Code::new(op_of(DEXT[0]), 1, DBASE[0]));
        assert_eq!(built.arena[1], Code::new(op_of(DEXT[1]), 1, DBASE[1]));

        // An incomplete set with codes longer than one bit stays an error even for
        // the types that allow the single-code case.
        let mut deeper = [0_u16; 32];
        deeper[0] = 1;
        deeper[1] = 3;
        let built = build(CodeType::Dists, &deeper, 6);
        assert_eq!(built.status, TABLE_INVALID_CODE);
    }

    #[test]
    fn enough_is_reported_when_the_table_would_not_fit() {
        // The two calls `test/infcover.c` `cover_trees()` makes (L626-L636), which
        // exist precisely to reach this return. Both must report the shortage
        // *before* writing, because the fixture there is only ENOUGH_DISTS long.
        let lens = one_of_every_length();

        let built = build(CodeType::Dists, &lens, 15);
        assert_eq!(
            built.status, TABLE_NOT_ENOUGH,
            "a 15-bit root needs 32768 entries"
        );
        assert_eq!(built.cursor, 0);
        assert_eq!(built.arena[0], Code::ZERO, "nothing may be written first");

        let built = build(CodeType::Dists, &lens, 1);
        assert_eq!(
            built.status, TABLE_NOT_ENOUGH,
            "sub-tables overflow ENOUGH_DISTS"
        );

        // The same code fits comfortably at the root width `inflate.c` L899 uses.
        let built = build(CodeType::Dists, &lens, 6);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 576);
        assert!(built.cursor <= ENOUGH_DISTS);

        // A literal/length table has more room, and the same code fits there too.
        let built = build(CodeType::Lens, &lens, 9);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 576);
        assert!(built.cursor <= ENOUGH_LENS);

        // CODES has no ENOUGH guard at all, exactly as in C.
        let built = build(CodeType::Codes, &lens, 7);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 384);
    }

    #[test]
    fn the_root_width_comes_back_bounded_by_the_code() {
        // `inftrees.c` L34-L45: the returned width differs from the requested one
        // when the request exceeds the longest code or falls short of the shortest.
        let mut lens = [0_u16; 19];
        for length in lens.iter_mut().take(16) {
            *length = 4;
        }

        // Requested wider than the longest code: clamped down to 4.
        let built = build(CodeType::Codes, &lens, 7);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, 4);
        assert_eq!(built.cursor, 16);

        // Requested narrower than the shortest code: raised to 4.
        let built = build(CodeType::Codes, &lens, 2);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, 4);
        assert_eq!(built.cursor, 16);

        // Requested exactly: unchanged.
        let built = build(CodeType::Codes, &lens, 4);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, 4);

        // A zero request is legal input and is raised to the shortest code.
        let built = build(CodeType::Codes, &lens, 0);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.bits, 4);
    }

    #[test]
    fn the_cursor_advances_by_the_entries_used() {
        // The in/out cursor is what lets `inflate.c` L888-L901 build a literal/length
        // table and a distance table back to back in one arena.
        let lens = one_of_every_length();
        let start = 100;
        let built = build_from(CodeType::Dists, &lens, lens.len(), 6, start);

        assert_eq!(built.status, TABLE_OK);
        assert_eq!(
            built.cursor,
            start + 576,
            "advanced by exactly the entries used"
        );
        assert!(
            built.arena[..start]
                .iter()
                .all(|&entry| entry == Code::ZERO),
            "entries before the cursor belong to the previous table"
        );
        assert_ne!(
            built.arena[start],
            Code::ZERO,
            "the new table starts at the cursor"
        );
        assert!(built.arena[start + 576..]
            .iter()
            .all(|&entry| entry == Code::ZERO));
    }

    #[test]
    fn sub_table_links_are_relative_to_the_table_base() {
        // The invariant `inffast.c` L264-L266 and `inflate.c` L930-L932 depend on:
        // `val` is an offset from the base of the table holding the link, not from
        // the base of the arena. Building the same code at two different cursors
        // must therefore produce the same `val`.
        let lens = one_of_every_length();

        for start in [0, 1, 251] {
            let built = build_from(CodeType::Dists, &lens, lens.len(), 6, start);
            assert_eq!(built.status, TABLE_OK, "start {start}");

            // With a root of 6 this code has exactly one second-level table, hung
            // off the last root entry.
            let links: usize = built.arena[start..start + 64]
                .iter()
                .filter(|entry| entry.is_table_link())
                .count();
            assert_eq!(links, 1, "start {start}");

            let link = built.arena[start + 63];
            assert_eq!(link, Code::new(9, 6, 64), "start {start}");
            assert_eq!(
                usize::from(link.val),
                1 << 6,
                "the sub-table follows the root table"
            );

            // Follow it exactly as the decoder does.
            let sub_base = start + usize::from(link.val);
            assert_eq!(
                built.arena[sub_base],
                Code::new(op_of(DEXT[6]), 1, DBASE[6]),
                "start {start}"
            );
            assert_eq!(
                built.arena[sub_base + 1],
                Code::new(op_of(DEXT[7]), 2, DBASE[7])
            );
            assert_eq!(
                built.arena[sub_base + 2],
                Code::new(op_of(DEXT[6]), 1, DBASE[6])
            );
            assert_eq!(
                built.arena[sub_base + 3],
                Code::new(op_of(DEXT[8]), 3, DBASE[8])
            );

            // Every entry of the sub-table is written, and nothing past it is.
            let end = sub_base + (1 << link.table_bits());
            assert_eq!(end, start + 576);
            assert!(built.arena[sub_base..end]
                .iter()
                .all(|&entry| entry != Code::ZERO));
        }
    }

    // -------------------------------------------------------------------------
    //  Per-type behaviour
    // -------------------------------------------------------------------------

    #[test]
    fn distance_tables_take_every_symbol_from_base_and_extra() {
        // C's `case DISTS:` falls out of the switch without assigning `match`
        // (`inftrees.c` L199-L202), leaving it at 0 so that every symbol takes the
        // base/extra branch. If that fall-through were "fixed" to 257 like LENS, or
        // to 20 like CODES, distance symbol 0 would be built as a literal with
        // `val = 0` instead of a distance with `val = 1`.
        let built = build(CodeType::Dists, &[5_u16; 32], 5);
        assert_eq!(built.status, TABLE_OK);

        for (index, &(op, _, val)) in DISTFIX.iter().enumerate() {
            let entry = built.arena[index];
            assert_ne!(
                (entry.op, entry.val),
                (0, 0),
                "index {index} must not have been built as a literal"
            );
            assert_eq!((entry.op, entry.val), (op, val), "at index {index}");
        }

        // Symbols 30 and 31 are the never-occurring ones, and land at the two
        // indices whose entries are invalid-code markers.
        let invalid: usize = built.arena[..32]
            .iter()
            .filter(|entry| entry.is_invalid())
            .count();
        assert_eq!(invalid, 2);
    }

    #[test]
    fn the_code_length_code_is_built_from_literals_only() {
        // With `match` at 20 and 19 symbols, `symbol + 1 < match` always holds, so
        // the base and extra tables are never consulted -- which is why passing
        // empty slices for them is sound (`inftrees.c` L191-L193).
        let mut lens = [0_u16; 19];
        for length in lens.iter_mut().take(16) {
            *length = 4;
        }
        let built = build(CodeType::Codes, &lens, 7);
        assert_eq!(built.status, TABLE_OK);
        assert_eq!(built.cursor, 16);

        // The first six entries, in the bit-reversed order DEFLATE stores codes in.
        let expected = [0_u16, 8, 4, 12, 2, 10];
        for (index, &val) in expected.iter().enumerate() {
            assert_eq!(
                built.arena[index],
                Code {
                    op: 0,
                    bits: 4,
                    val
                },
                "at index {index}"
            );
        }

        // Every one of the sixteen symbols appears exactly once, as a literal.
        let mut seen = [false; 16];
        for entry in &built.arena[..16] {
            assert!(entry.is_literal());
            assert_eq!(entry.bits, 4);
            seen[usize::from(entry.val)] = true;
        }
        assert!(seen.iter().all(|&hit| hit));

        // The 19-symbol alphabet is exercised at its real root width too: a set of
        // 19 codes that is complete only if all 19 participate.
        let mut full = [3_u16; 19];
        for length in full.iter_mut().skip(8) {
            *length = 4;
        }
        let built = build(CodeType::Codes, &full, 7);
        assert_eq!(
            built.status, TABLE_INVALID_CODE,
            "8 threes and 11 fours is incomplete"
        );
    }

    #[test]
    fn inflate_fixed_selects_the_fixed_tables() {
        // `inftrees.c` L364-L372 in its default form: four assignments, no lazy
        // initialisation, no allocation.
        let mut tables = CodeTables::RESET;
        inflate_fixed(&mut tables);

        assert_eq!(tables, CodeTables::FIXED);
        assert_eq!(tables.lencode, CodeTableSource::Fixed);
        assert_eq!(tables.distcode, CodeTableSource::Fixed);
        assert_eq!(tables.lenbits, 9);
        assert_eq!(tables.distbits, 5);
        assert!(tables.is_fixed());

        // Calling it again is idempotent, which matters because `inflate()` calls it
        // once per fixed block rather than once per stream (`inflate.c` L727).
        inflate_fixed(&mut tables);
        assert_eq!(tables, CodeTables::FIXED);
    }

    #[test]
    fn code_table_sources_distinguish_fixed_from_dynamic() {
        // The safe-Rust replacement for the pointer-range test at `inflate.c`
        // L1357-L1361: a dynamic table reports its offset, a fixed one reports none,
        // so a copied stream needs no rebasing.
        assert_eq!(CodeTableSource::Fixed.dynamic_offset(), None);
        assert_eq!(
            CodeTableSource::Dynamic { offset: 0 }.dynamic_offset(),
            Some(0)
        );
        assert_eq!(
            CodeTableSource::Dynamic { offset: 852 }.dynamic_offset(),
            Some(852)
        );

        // The reset state points both tables at the base of the arena, mirroring
        // `inflate.c` L118.
        let reset = CodeTables::default();
        assert_eq!(reset, CodeTables::RESET);
        assert_eq!(reset.lencode.dynamic_offset(), Some(0));
        assert_eq!(reset.distcode.dynamic_offset(), Some(0));
        assert!(!reset.is_fixed());

        // A dynamic pair, as `inflate.c` L889 and L898 record it: the literal/length
        // table at the base of the arena and the distance table just past it.
        let dynamic = CodeTables {
            lencode: CodeTableSource::Dynamic { offset: 0 },
            lenbits: 9,
            distcode: CodeTableSource::Dynamic { offset: 512 },
            distbits: 6,
        };
        assert!(!dynamic.is_fixed());
        assert_ne!(dynamic, CodeTables::FIXED);
    }

    // -------------------------------------------------------------------------
    //  Totality: broken preconditions and random input
    // -------------------------------------------------------------------------

    #[test]
    fn broken_preconditions_are_reported_not_trusted() {
        // Every case below is a caller obligation in C (`inftrees.c` L97-L100), and
        // in C every one of them reads or writes out of bounds. Here each is
        // reported, and none can panic.
        let mut arena = [Code::ZERO; ENOUGH];
        let mut work = [0_u16; SCRATCH];
        let mut bits = 6;

        // A length above MAXBITS has no counter slot; C would write past `count[16]`.
        for bad in [16_u16, 17, 255, u16::MAX] {
            let lens = [1_u16, 1, bad, 0];
            let mut cursor = 0;
            let status = inflate_table(
                CodeType::Dists,
                &lens,
                4,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work,
            );
            assert_eq!(status, TABLE_INVALID_CODE, "length {bad}");
            assert_eq!(cursor, 0, "length {bad}");
        }

        // More symbols claimed than the length slice holds.
        let mut cursor = 0;
        assert_eq!(
            inflate_table(
                CodeType::Dists,
                &[1_u16, 1],
                32,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work
            ),
            TABLE_INVALID_CODE
        );

        // A work area smaller than the symbol count.
        let mut small_work = [0_u16; 1];
        let mut cursor = 0;
        assert_eq!(
            inflate_table(
                CodeType::Dists,
                &[1_u16, 1],
                2,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut small_work
            ),
            TABLE_INVALID_CODE
        );

        // A cursor already past the end of the arena.
        let mut cursor = ENOUGH + 1;
        assert_eq!(
            inflate_table(
                CodeType::Dists,
                &[1_u16, 1],
                2,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work
            ),
            TABLE_NOT_ENOUGH
        );

        // An arena too small for the root table asked for, and too small even for the
        // two-entry table the no-symbols case writes.
        let mut tiny = [Code::ZERO; 1];
        let mut cursor = 0;
        let mut root = 1;
        assert_eq!(
            inflate_table(
                CodeType::Dists,
                &[1_u16, 1],
                2,
                &mut tiny,
                &mut cursor,
                &mut root,
                &mut work
            ),
            TABLE_NOT_ENOUGH
        );
        let mut cursor = 0;
        assert_eq!(
            inflate_table(
                CodeType::Lens,
                &[0_u16; 4],
                4,
                &mut tiny,
                &mut cursor,
                &mut bits,
                &mut work
            ),
            TABLE_NOT_ENOUGH
        );
    }

    /// Deterministic xorshift64\* generator: a failing round is reproducible from the
    /// seed, and the crate keeps its empty dependency table.
    struct Rng(u64);

    impl Rng {
        /// Advances the state and returns the next output word.
        fn next_u64(&mut self) -> u64 {
            let mut state = self.0;
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            self.0 = state;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        /// Returns a value in `0..bound`, or 0 when `bound` is 0.
        fn below(&mut self, bound: usize) -> usize {
            match u64::try_from(bound) {
                Ok(0) | Err(_) => 0,
                Ok(limit) => usize::try_from(self.next_u64() % limit).unwrap(),
            }
        }
    }

    /// Number of random rounds. Miri interprets rather than executes, so the count
    /// drops to something that still covers every branch without taking minutes.
    fn rounds() -> usize {
        if cfg!(miri) {
            40
        } else {
            4000
        }
    }

    #[test]
    fn random_length_sets_never_panic() {
        // `lens` reaches this function straight out of the compressed stream, so it
        // is adversarial input. Half of these rounds also violate the documented
        // `0..=MAXBITS` precondition, which C does not check at all.
        let mut rng = Rng(0x1234_5678_9abc_def0);
        let mut arena = [Code::ZERO; ENOUGH];
        let mut work = [0_u16; SCRATCH];
        let mut lens = [0_u16; SCRATCH];

        for round in 0..rounds() {
            let code_type = match round % 3 {
                0 => CodeType::Codes,
                1 => CodeType::Lens,
                _ => CodeType::Dists,
            };
            let codes = match code_type {
                CodeType::Codes => 19,
                CodeType::Lens => rng.below(289),
                CodeType::Dists => rng.below(33),
            };
            let ceiling = if round % 2 == 0 { MAXBITS + 1 } else { 256 };
            for slot in lens.iter_mut().take(codes) {
                *slot = u16::try_from(rng.below(ceiling)).unwrap();
            }

            arena.fill(Code::ZERO);
            let start = rng.below(ENOUGH);
            let mut cursor = start;
            let mut bits = rng.below(MAXBITS + 1);
            let status = inflate_table(
                code_type,
                &lens,
                codes,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work,
            );

            assert!(
                status == TABLE_OK || status == TABLE_INVALID_CODE || status == TABLE_NOT_ENOUGH,
                "round {round} returned {status}"
            );
            if status == TABLE_OK {
                assert!(
                    (1..=MAXBITS).contains(&bits),
                    "round {round} reported bits {bits}"
                );
                assert!(cursor > start, "round {round} consumed nothing");
                assert!(
                    cursor <= ENOUGH,
                    "round {round} left the cursor at {cursor}"
                );
                assert!(
                    arena[..start].iter().all(|&entry| entry == Code::ZERO),
                    "round {round} wrote before the cursor"
                );
                assert!(
                    arena[cursor..].iter().all(|&entry| entry == Code::ZERO),
                    "round {round} wrote past the entries it claimed"
                );
            } else {
                assert_eq!(
                    cursor, start,
                    "round {round} moved the cursor after failing"
                );
            }
        }
    }

    /// Builds a random *complete* length set into `lens`, returning the number of
    /// symbols that ended up with a code.
    ///
    /// One leaf covering the whole code space is split repeatedly, and splitting a
    /// leaf of length `n` into two of length `n + 1` leaves the Kraft sum at exactly
    /// 1, so the result always passes the validity test at `inftrees.c` L139-L147.
    /// Leaves already at [`MAXBITS`] cannot be split, so the loop gives up after a
    /// bounded number of attempts and the remaining symbols stay absent.
    fn random_complete_code(rng: &mut Rng, symbols: usize, lens: &mut [u16]) -> usize {
        lens[..symbols].fill(0);
        let ceiling = u16::try_from(MAXBITS).unwrap();
        let mut leaves = 1;
        let mut attempts = 0;
        while leaves < symbols && attempts < symbols * 8 {
            attempts += 1;
            let pick = rng.below(leaves);
            if lens[pick] < ceiling {
                lens[pick] += 1;
                lens[leaves] = lens[pick];
                leaves += 1;
            }
        }
        // Shuffle, so that symbol order varies independently of length order and the
        // counting sort is exercised rather than fed pre-sorted input.
        for index in (1..symbols).rev() {
            let other = rng.below(index + 1);
            lens.swap(index, other);
        }
        leaves
    }

    #[test]
    fn random_complete_codes_stay_within_the_enough_bounds() {
        // [`ENOUGH_LENS`] and [`ENOUGH_DISTS`] were found by exhaustive search over
        // exactly these two shapes -- at most 286 symbols at a root of 9, and at most
        // 30 symbols at a root of 6 (`inftrees.h` L38-L48). Sampling that space is
        // the strongest cheap evidence that this port allocates entries the way C
        // does: an implementation that sized sub-tables differently would either
        // exceed the bound and report a shortage, or refuse a valid code.
        let mut rng = Rng(0x0bad_c0de_dead_beef);
        let mut arena = [Code::ZERO; ENOUGH];
        let mut work = [0_u16; SCRATCH];
        let mut lens = [0_u16; SCRATCH];

        for round in 0..rounds() {
            let symbols = 2 + rng.below(285);
            let covered = random_complete_code(&mut rng, symbols, &mut lens);
            assert!(covered >= 2, "round {round}");

            let mut cursor = 0;
            let mut bits = 9;
            let status = inflate_table(
                CodeType::Lens,
                &lens,
                symbols,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work,
            );
            assert_eq!(
                status, TABLE_OK,
                "round {round}: a complete code must be accepted"
            );
            assert!(cursor <= ENOUGH_LENS, "round {round} used {cursor} entries");
            assert!((1..=MAXBITS).contains(&bits), "round {round}");

            // The distance table is built into the same arena, starting where the
            // literal/length table ended, exactly as `inflate.c` L888-L901 does it.
            let split = cursor;
            let symbols = 2 + rng.below(29);
            random_complete_code(&mut rng, symbols, &mut lens);
            let mut bits = 6;
            let status = inflate_table(
                CodeType::Dists,
                &lens,
                symbols,
                &mut arena,
                &mut cursor,
                &mut bits,
                &mut work,
            );
            assert_eq!(
                status, TABLE_OK,
                "round {round}: a complete code must be accepted"
            );
            assert!(
                cursor - split <= ENOUGH_DISTS,
                "round {round} used {} entries",
                cursor - split
            );
            assert!(
                cursor <= ENOUGH,
                "round {round}: both tables must fit one arena"
            );
        }
    }
}
