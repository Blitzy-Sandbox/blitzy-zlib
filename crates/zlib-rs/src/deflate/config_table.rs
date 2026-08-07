//! The per-level match-finding tuning table, transcribed from `deflate.c`
//! L112-L124.
//!
//! An unqualified line reference in this file is a line of `deflate.c`.
//!
//! C introduces the table with its own explanation (L93-L96):
//!
//! ```text
//! /* Values for max_lazy_match, good_match and max_chain_length, depending on
//!  * the desired pack level (0..9). The values given below have been tuned to
//!  * exclude worst case performance for pathological files. Better values may be
//!  * found for specific files.
//!  */
//! ```
//!
//! and closes it with a caveat that is repeated on [`CONFIGURATION_TABLE`]
//! itself (L127-L130).
//!
//! # Why these forty numbers are the most load-bearing constants in the implementation
//!
//! RFC 1951 constrains the DEFLATE *format*; it says nothing about which of the
//! several valid encodings of a given input an encoder should choose. Those
//! choices are the reference implementation's own, they are explicitly
//! non-normative -- "better values may be found" -- and this table is where four
//! of them live. Together the four numbers of a row decide which candidate
//! matches `longest_match` examines and which it accepts, and therefore the
//! literal/length/distance symbols that reach the Huffman coder.
//!
//! A single altered digit does not make the encoder worse or better in any way a
//! decompressor could detect: it makes it emit *different bytes*, at that level,
//! for essentially every input. Since byte-identical output against the C
//! reference is an acceptance criterion of this port, a "tuned", interpolated or
//! formula-derived table is a defect even when it compresses better.
//! The planned `crates/zlib-rs-differential/tests/table_equality.rs` will compare
//! [`CONFIGURATION_TABLE`] against the C array element for element -- it has not landed, so
//! nothing checks the transcription automatically today -- and the
//! byte-identity matrix exercises all ten levels; the tests at the end of this
//! file restate the forty numbers independently so that a transcription slip
//! fails here, in the cheapest possible place, rather than in a compressed
//! stream.
//!
//! # Where C reads the table, and what each read decides
//!
//! Five sites, all in `deflate.c`, and every one of them is byte-identity
//! critical:
//!
//! * **`lm_init`, L689-L692.** Loads all four numbers into the fresh stream's
//!   `max_lazy_match`, `good_match`, `nice_match` and `max_chain_length`.
//! * **`deflateParams`, L789.** Reads the *current* level's `func`.
//! * **`deflateParams`, L791.** Compares that against the *requested* level's
//!   `func` **by identity**, to decide whether the level change must first flush
//!   the open block.
//! * **`deflateParams`, L809-L812.** Reloads the same four numbers when the
//!   level actually changes.
//! * **`deflate`, L1220.** Calls the level's `func`, as the last arm of the
//!   dispatch chain at L1217-L1220.
//!
//! The four numeric loads are `DeflateState::set_tuning`, which takes the
//! numbers rather than a level so that `deflateTune` (L825-L828) -- whose values
//! come from the caller, not from this table -- is expressible through the same
//! door. The two `func` reads are `CompressFunc`, whose discriminant equality
//! stands in for C's pointer comparison; see `Config::func` below.
//!
//! Neither `CompressFunc` nor `DeflateState` is named as a doc link here. They
//! are crate-private, and `rustdoc::private_intra_doc_links` rejects a link from
//! the public documentation of a public item to a private one -- the same
//! relaxation, for the same reason, that `deflate/algorithm.rs` records on its
//! own `Flush` re-export.
//!
//! # `FASTEST` is deliberately not implemented
//!
//! `deflate.c` declares **two** tables. L107-L110, under `#ifdef FASTEST`, is a
//! two-entry table; L112-L124, under the `#else`, is the ten-entry table this
//! module carries. Only the second is implemented, and `FASTEST` is not reachable as a
//! runtime option, an environment variable or a Cargo feature.
//!
//! That is not a simplification, it is a correctness requirement. `FASTEST` is a
//! *different encoder*: it compiles out `deflate_slow` altogether (L75-L77), it
//! substitutes a different `longest_match` (L1532-L1588, defined at L1537), it
//! stops maintaining hash chains entirely (L148-L149 and L154-L158), and it
//! rewrites the caller's request with `if (level != 0) level = 1` in both
//! `deflateInit2_` (L416-L417) and `deflateParams` (L781-L782). A build with
//! `FASTEST` defined therefore produces compressed output that differs from the
//! shipped library's at every level above zero. Offering it would mean offering a
//! second, incompatible byte stream from one library.
//!
//! For the same reason nothing here is configurable. There is no way to widen a
//! chain, raise a `nice_length` or supply a table of one's own: `deflateTune`
//! (L825-L828) is the reference implementation's own supported door for that, it
//! writes the four fields of `DeflateState` directly, and it bypasses this table
//! entirely.

// The five compressors are named through `CompressFunc`, whose variants stand in
// for the three function addresses `configuration_table` holds. The variants are
// imported unqualified purely so that the ten rows below stay one line each and
// keep the column alignment of `deflate.c` L112-L124 -- the tabular shape is
// what makes the transcription reviewable against the C source by eye.
use crate::deflate::algorithm::CompressFunc;
use crate::deflate::algorithm::CompressFunc::{Fast, Slow, Stored};

/// One row of [`CONFIGURATION_TABLE`]: the tuning for a single compression
/// level.
///
/// Mirrors `struct config_s` (L98-L104).
///
/// The four numeric members keep C's field names exactly, and their type is
/// [`u16`] because `ush` is `unsigned short` (`zutil.h` L45). They are declared
/// at the width C declares them at rather than at the width their consumers use
/// -- `DeflateState::good_match`, `max_lazy_match` and `max_chain_length` are
/// `usize` and `nice_match` is `i32`, matching the `uInt`/`int` members of
/// `deflate_state` (`deflate.h` L175, L181, L195, L198) -- so that this type
/// describes the *table*, and every widening happens at the one place that loads
/// it, `DeflateState::set_tuning`. Every value in the table is at most 4096, so
/// no load can lose information.
///
/// # Only ten of these ever exist
///
/// There is no constructor and no mutator. The ten values in
/// [`CONFIGURATION_TABLE`] are the only instances the crate ever creates, they
/// are `const` data, and `deflateTune` -- the reference implementation's own
/// supported way to override the four numbers (L825-L828) -- writes
/// `DeflateState` directly without going through a `Config` at all.
///
/// [`Eq`] is derived because `deflateParams` compares two rows' `func` fields
/// (L791); the four numeric fields make the whole row comparable at no cost,
/// which the tests use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Above this previous-match length the lazy search is cut short (L99).
    ///
    /// Loaded into `s->good_match` (`deflate.h` L195-L196). Under the comment "Do not waste too
    /// much time if we already have a good match", `longest_match` cuts its
    /// remaining chain budget to a quarter -- `chain_length >>= 2`, not a halving
    /// -- once the previous match already reached this length (L1422-L1425). It
    /// therefore trades ratio for speed on inputs that are already matching well.
    ///
    /// Ignored for levels 0 to 3, which never take the lazy path -- the caveat
    /// at L127-L130 -- but still transcribed for those levels, because the table
    /// is transcribed rather than reconstructed.
    pub good_length: u16,

    /// The match length at or above which no lazy search is attempted (L100).
    ///
    /// Loaded into `s->max_lazy_match` (`deflate.h` L181-L185): a better match is sought
    /// only while the current one is strictly shorter than this, which is the mechanism
    /// levels 4 and above use. Read by `deflate_slow` at L1988.
    ///
    /// C gives the same field a second name for the other three levels --
    /// `#define max_insert_length max_lazy_match` (`deflate.h` L186-L190) --
    /// under which `deflate_fast` reads it as an *insertion* threshold instead
    /// (L1906). One field, two meanings, selected by which compressor is
    /// running; that is precisely what "lazy has a different meaning" at L129
    /// refers to.
    pub max_lazy: u16,

    /// The match length at which the chain walk quits early (L101).
    ///
    /// Loaded into `s->nice_match` (`deflate.h` L198). `longest_match` breaks out of the
    /// chain walk as soon as it
    /// has a match this long -- `if (len >= nice_match) break;` (L1517) -- having
    /// first clamped the threshold down to the available lookahead, because not
    /// doing so would "make deflate deterministic" fail (L1426-L1429).
    ///
    /// It is an early-exit heuristic, not a limit on match length: `MAX_MATCH`
    /// (258) is that limit, and levels 8 and 9 set `nice_length` to exactly 258
    /// so that the early exit only ever fires on a maximal match.
    pub nice_length: u16,

    /// The hard limit on how far a hash chain is walked (L102) -- the one field the C
    /// table leaves uncommented.
    ///
    /// Loaded into `s->max_chain_length` (`deflate.h` L175-L179): a higher limit improves
    /// the compression ratio and costs speed. Copied into `longest_match`'s chain counter
    /// at L1390.
    ///
    /// It is the single strongest determinant of which match is found, because
    /// the hash chain is walked newest-first: truncating it earlier does not
    /// merely find a shorter match, it finds a *different* one, at a different
    /// distance. The caveat at L127-L128 requires it to be at least 4 wherever
    /// a search happens at all.
    pub max_chain: u16,

    /// `compress_func func` (L103) -- which compressor this level runs.
    ///
    /// C stores a function address; this implementation stores a `CompressFunc`
    /// discriminant, because a compressor here is generic over the stream's
    /// allocator and no single monomorphic `fn` pointer can name one. That
    /// decision, and the two properties it has to preserve, are documented on
    /// `CompressFunc` itself, in `deflate/algorithm.rs`.
    ///
    /// The table holds exactly three distinct values -- `Stored` at level 0,
    /// `Fast` at levels 1 to 3 and `Slow` at levels 4 to 9 -- and never `Rle` or
    /// `Huff`, which are reachable only through the strategy, in
    /// `CompressFunc::select`. Because the grouping is the same, comparing
    /// discriminants and comparing addresses agree exactly, which is what makes
    /// `deflateParams`' identity test at L791 reproduce C's behaviour: two
    /// levels compare equal here precisely when they name the same C function.
    /// Get the grouping wrong and `deflateParams` flushes the open block where C
    /// does not, or fails to where C does, and either changes the emitted bytes.
    ///
    /// `pub(crate)` rather than `pub` because `CompressFunc` is itself
    /// crate-private: a `pub` field of a crate-private type is the
    /// `private_interfaces` error, and promoting `CompressFunc` would export a
    /// type that `zlib.map` does not mention. The four numeric fields remain
    /// `pub` so that the differential harness can read them.
    pub(crate) func: CompressFunc,
}

/// The ten per-level tuning rows, transcribed digit for digit from L112-L124.
///
/// Mirrors the `#else` branch of the two tables at L106-L125.
///
/// C's own caveat on these values, from L127-L130.
///
/// Read that caveat as scoped, not as an invariant of the table: levels 0 to 3
/// carry `max_lazy` values of 0, 4, 5 and 6, three of which are below
/// `MIN_MATCH`, and level 0 carries `max_chain` 0. Those rows are not defective
/// -- they are the reason the note exists. Level 0 never searches at all, and
/// levels 1 to 3 run `deflate_fast`, which reads the same field under its other
/// name `max_insert_length` (`deflate.h` L186-L190) where a value below
/// `MIN_MATCH` is meaningful. The requirement binds exactly the six levels that
/// reach `deflate_slow`, and the tests below assert it over precisely those.
///
/// # Indexing
///
/// C indexes this array directly with `s->level` at all five of its read sites,
/// which is safe there because `deflateInit2_` (L434-L438) and `deflateParams`
/// (L786-L788) have already rejected anything outside `0..=9`. This implementation
/// establishes the same range in `crate::config::normalize_deflate_level`, and
/// then reaches the row through `config_for_level` so that no library path can
/// panic even if a future caller slipped past that check.
// `#[rustfmt::skip]` keeps the ten rows one line each, in C's column order and
// carrying C's own annotations, so the transcription can be diffed against
// L112-L124 by eye; the default formatting explodes every row onto seven lines
// and destroys exactly the property that makes a byte-identity-critical table
// reviewable. Four of the rows exceed the crate's 100-column norm as a result --
// a named-field struct literal is simply wider than C's positional one -- and
// that is the deliberate trade: fidelity of the data over width of the line.
// Nothing but this table is exempt from the formatter.
#[rustfmt::skip]
pub const CONFIGURATION_TABLE: [Config; 10] = [
    //      good lazy nice chain
    /* 0 */ Config { good_length:  0, max_lazy:   0, nice_length:   0, max_chain:    0, func: Stored }, // store only
    /* 1 */ Config { good_length:  4, max_lazy:   4, nice_length:   8, max_chain:    4, func: Fast   }, // max speed, no lazy matches
    /* 2 */ Config { good_length:  4, max_lazy:   5, nice_length:  16, max_chain:    8, func: Fast   },
    /* 3 */ Config { good_length:  4, max_lazy:   6, nice_length:  32, max_chain:   32, func: Fast   },

    /* 4 */ Config { good_length:  4, max_lazy:   4, nice_length:  16, max_chain:   16, func: Slow   }, // lazy matches
    /* 5 */ Config { good_length:  8, max_lazy:  16, nice_length:  32, max_chain:   32, func: Slow   },
    /* 6 */ Config { good_length:  8, max_lazy:  16, nice_length: 128, max_chain:  128, func: Slow   },
    /* 7 */ Config { good_length:  8, max_lazy:  32, nice_length: 128, max_chain:  256, func: Slow   },
    /* 8 */ Config { good_length: 32, max_lazy: 128, nice_length: 258, max_chain: 1024, func: Slow   },
    /* 9 */ Config { good_length: 32, max_lazy: 258, nice_length: 258, max_chain: 4096, func: Slow   }, // max compression
];

/// The [`Config`] row for a compression level, without a panicking index.
///
/// C writes `configuration_table[s->level]` at every one of its five read sites
/// (L689-L692, L789, L791, L809-L812, L1220) and that is sound there, because
/// `deflateInit2_` (L434-L438) and `deflateParams` (L786-L788) both reject a
/// level outside `0..=9` before any of them runs -- in this implementation, through
/// `crate::config::normalize_deflate_level`. This function preserves that
/// behaviour for every level C accepts while removing the panic path that a raw
/// `CONFIGURATION_TABLE[level]` would leave in a library that must not abort a
/// caller's process, and it is the only way the crate reads the table.
///
/// # Out-of-range levels
///
/// A level below 0 clamps to 0 and a level above 9 clamps to 9. Neither is
/// reachable through a validated stream; the clamp exists so that the function is
/// total, not to define new behaviour. Note that `Z_DEFAULT_COMPRESSION` is `-1`
/// and must be resolved to 6 by `normalize_deflate_level` *before* reaching
/// here -- exactly as C resolves it at L419 and L784 before indexing -- because
/// clamping it to 0 would silently select "store only" instead of the default
/// level.
#[inline]
#[must_use]
pub(crate) fn config_for_level(level: i32) -> &'static Config {
    // `&CONFIGURATION_TABLE` is subject to rvalue static promotion: the
    // initializer is a `const` with no interior mutability and no destructor, so
    // this borrows a `'static` allocation rather than a temporary. Binding it
    // first, instead of writing `CONFIGURATION_TABLE.get(..)`, is what makes the
    // promotion apply -- an autoref inside a method call would not be promoted
    // and the returned reference would not outlive the statement.
    let table: &'static [Config; 10] = &CONFIGURATION_TABLE;

    // Level 0's row, bound by destructuring so that the unreachable arm below
    // needs neither a panic nor a second copy of the data.
    let [store_only, ..] = table;

    // `usize::try_from` fails for exactly the negative levels, which is the clamp
    // at the bottom; `min` is the clamp at the top.
    let requested = usize::try_from(level).unwrap_or(0);
    let index = requested.min(table.len() - 1);

    match table.get(index) {
        Some(config) => config,
        // Unreachable: `index` was clamped into `0..table.len()` immediately
        // above. Returning level 0 rather than panicking keeps this total.
        None => store_only,
    }
}

// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use super::{config_for_level, CompressFunc, Config, CONFIGURATION_TABLE};
    use crate::deflate::state::{MAX_MATCH, MIN_MATCH};

    /// The forty numbers and ten compressors of L112-L124, restated
    /// independently of [`CONFIGURATION_TABLE`].
    ///
    /// This is deliberately a *second* transcription rather than a derivation
    /// from the first: a test that recomputed the table from the table would
    /// pass on any table. Column order is C's -- good, lazy, nice, chain, func.
    #[rustfmt::skip]
    const EXPECTED: [(u16, u16, u16, u16, CompressFunc); 10] = [
        /* 0 */ ( 0,   0,   0,    0, CompressFunc::Stored),
        /* 1 */ ( 4,   4,   8,    4, CompressFunc::Fast),
        /* 2 */ ( 4,   5,  16,    8, CompressFunc::Fast),
        /* 3 */ ( 4,   6,  32,   32, CompressFunc::Fast),
        /* 4 */ ( 4,   4,  16,   16, CompressFunc::Slow),
        /* 5 */ ( 8,  16,  32,   32, CompressFunc::Slow),
        /* 6 */ ( 8,  16, 128,  128, CompressFunc::Slow),
        /* 7 */ ( 8,  32, 128,  256, CompressFunc::Slow),
        /* 8 */ (32, 128, 258, 1024, CompressFunc::Slow),
        /* 9 */ (32, 258, 258, 4096, CompressFunc::Slow),
    ];

    /// The first level whose compressor is `deflate_slow`, and therefore the
    /// first level at which the `max_lazy >= MIN_MATCH` half of C's caveat at
    /// L127-L128 actually binds.
    const FIRST_LAZY_LEVEL: usize = 4;

    /// Every row of [`CONFIGURATION_TABLE`] equals the corresponding row of
    /// L112-L124, field for field.
    ///
    /// A single transposed digit here changes the compressed output at that level
    /// for essentially every input, so this is the cheapest and highest-signal
    /// check in the deflate subtree.
    #[test]
    fn every_row_matches_the_c_table() {
        for (level, &(good_length, max_lazy, nice_length, max_chain, func)) in
            EXPECTED.iter().enumerate()
        {
            let row = CONFIGURATION_TABLE[level];
            assert_eq!(row.good_length, good_length, "level {level} good_length");
            assert_eq!(row.max_lazy, max_lazy, "level {level} max_lazy");
            assert_eq!(row.nice_length, nice_length, "level {level} nice_length");
            assert_eq!(row.max_chain, max_chain, "level {level} max_chain");
            assert_eq!(row.func, func, "level {level} func");
        }
    }

    /// The table has exactly ten rows: one per compression level, `0..=9`.
    ///
    /// The declared type `[Config; 10]` already makes a truncated or over-long
    /// paste a compile error; this pins the count against the range
    /// `deflateInit2_` accepts (L434-L438) rather than against the declaration.
    #[test]
    fn the_table_has_exactly_ten_rows() {
        assert_eq!(CONFIGURATION_TABLE.len(), 10);
        assert_eq!(EXPECTED.len(), CONFIGURATION_TABLE.len());
    }

    /// Level 0 stores, levels 1 to 3 run `deflate_fast`, levels 4 to 9 run
    /// `deflate_slow` -- exactly the three distinct function addresses
    /// L112-L124 holds, on exactly those boundaries.
    #[test]
    fn level_zero_stores_one_to_three_are_fast_and_four_to_nine_are_slow() {
        for (level, config) in CONFIGURATION_TABLE.iter().enumerate() {
            let expected = match level {
                0 => CompressFunc::Stored,
                1..=3 => CompressFunc::Fast,
                _ => CompressFunc::Slow,
            };
            assert_eq!(config.func, expected, "level {level}");
        }
    }

    /// Neither `deflate_rle` nor `deflate_huff` appears in the table.
    ///
    /// They exist, but they are reachable only through the strategy, in
    /// `CompressFunc::select` (L1217-L1220). A row naming either of them would
    /// make a level select a compressor that C selects only for `Z_RLE` or
    /// `Z_HUFFMAN_ONLY`.
    #[test]
    fn neither_rle_nor_huff_appears_in_the_table() {
        for (level, config) in CONFIGURATION_TABLE.iter().enumerate() {
            assert_ne!(config.func, CompressFunc::Rle, "level {level}");
            assert_ne!(config.func, CompressFunc::Huff, "level {level}");
        }
    }

    /// `deflateParams` compares two rows' `func` fields by identity to decide
    /// whether a level change must first flush the open block (L789-L792).
    ///
    /// Comparing discriminants has to agree with comparing addresses: levels
    /// inside one group compare equal, levels across a group boundary do not.
    /// Getting this wrong flushes where C does not, or fails to where C does,
    /// and either changes the emitted bytes.
    #[test]
    fn the_identity_test_of_deflate_params_is_reproduced() {
        // Within a group: no flush.
        assert_eq!(CONFIGURATION_TABLE[1].func, CONFIGURATION_TABLE[3].func);
        assert_eq!(CONFIGURATION_TABLE[4].func, CONFIGURATION_TABLE[9].func);

        // Across a boundary: flush.
        assert_ne!(CONFIGURATION_TABLE[0].func, CONFIGURATION_TABLE[1].func);
        assert_ne!(CONFIGURATION_TABLE[3].func, CONFIGURATION_TABLE[4].func);
        assert_ne!(CONFIGURATION_TABLE[0].func, CONFIGURATION_TABLE[9].func);
    }

    /// "the `deflate()` code requires ... `max_chain` >= 4" (L127), over every
    /// level that searches the hash chain at all.
    ///
    /// Level 0 is excluded because it carries `max_chain` 0 and never searches:
    /// `deflate_stored` copies its input and calls neither `longest_match` nor
    /// `fill_window`'s match machinery.
    #[test]
    fn max_chain_is_at_least_four_for_every_searching_level() {
        // Level 0 is exempt: it carries `max_chain` 0 and never searches.
        assert_eq!(CONFIGURATION_TABLE[0].max_chain, 0);

        for (level, config) in CONFIGURATION_TABLE.iter().enumerate().skip(1) {
            let max_chain = config.max_chain;
            assert!(max_chain >= 4, "level {level} has max_chain {max_chain}");
        }
    }

    /// "the `deflate()` code requires `max_lazy` >= `MIN_MATCH`" (L127), over
    /// every level that reaches `deflate_slow`.
    #[test]
    fn max_lazy_is_at_least_min_match_for_every_lazy_level() {
        // `enumerate` before `skip`, so `level` stays the true table index.
        let table = &CONFIGURATION_TABLE;
        let lazy_levels = table.iter().enumerate().skip(FIRST_LAZY_LEVEL);

        for (level, config) in lazy_levels {
            let max_lazy = usize::from(config.max_lazy);
            assert_eq!(config.func, CompressFunc::Slow, "level {level}");
            assert!(max_lazy >= MIN_MATCH, "level {level} max_lazy {max_lazy}");
        }
    }

    /// Levels 0 to 3 do **not** satisfy `max_lazy >= MIN_MATCH`, and that is
    /// correct rather than a transcription error.
    ///
    /// This is why C scopes its own note: "For `deflate_fast()` (levels <= 3)
    /// good is ignored and lazy has a different meaning" (L128-L129). Those levels
    /// read the field under its second name, `max_insert_length`
    /// (`deflate.h` L186-L190), where 0, 4, 5 and 6 are all meaningful. Asserting
    /// the violation pins the scope of the requirement, so that a future reader
    /// cannot "repair" the low rows into conformance.
    #[test]
    fn the_fast_levels_deliberately_violate_the_lazy_precondition() {
        assert!(usize::from(CONFIGURATION_TABLE[0].max_lazy) < MIN_MATCH);
        assert!(usize::from(CONFIGURATION_TABLE[1].max_lazy) >= MIN_MATCH);
        assert!(usize::from(CONFIGURATION_TABLE[2].max_lazy) >= MIN_MATCH);
        assert!(usize::from(CONFIGURATION_TABLE[3].max_lazy) >= MIN_MATCH);

        // The four rows that are exempt are exactly the ones that never run
        // `deflate_slow`.
        let table = &CONFIGURATION_TABLE;
        let fast_levels = table.iter().enumerate().take(FIRST_LAZY_LEVEL);

        for (level, config) in fast_levels {
            assert_ne!(config.func, CompressFunc::Slow, "level {level}");
        }
    }

    /// No row asks `longest_match` to stop above the longest match the format
    /// can express.
    ///
    /// `nice_length` is an early-exit threshold, so a value above `MAX_MATCH`
    /// (258) would simply never fire; levels 8 and 9 sit exactly on the limit,
    /// which is the strongest setting that still has an effect.
    #[test]
    fn nice_length_never_exceeds_max_match() {
        for (level, config) in CONFIGURATION_TABLE.iter().enumerate() {
            let nice_length = usize::from(config.nice_length);
            assert!(nice_length <= MAX_MATCH, "level {level} nice {nice_length}");
        }

        // Levels 8 and 9 sit exactly on the limit.
        assert_eq!(usize::from(CONFIGURATION_TABLE[8].nice_length), MAX_MATCH);
        assert_eq!(usize::from(CONFIGURATION_TABLE[9].nice_length), MAX_MATCH);
    }

    /// Every value loads losslessly into the field it feeds.
    ///
    /// `DeflateState::set_tuning` takes `usize`, `usize`, `i32` and `usize`,
    /// matching the `uInt`/`int` members of `deflate_state` (`deflate.h` L175,
    /// L181, L195, L198), while the table declares `ush`. The widening can
    /// therefore never lose information, and this pins that so no future load
    /// site needs a checked conversion.
    #[test]
    fn every_value_widens_losslessly_into_its_deflate_state_field() {
        for (level, config) in CONFIGURATION_TABLE.iter().enumerate() {
            let good = usize::from(config.good_length);
            let lazy = usize::from(config.max_lazy);
            let nice = i32::from(config.nice_length);
            let chain = usize::from(config.max_chain);

            assert_eq!(u16::try_from(good), Ok(config.good_length), "level {level}");
            assert_eq!(u16::try_from(lazy), Ok(config.max_lazy), "level {level}");
            assert_eq!(u16::try_from(nice), Ok(config.nice_length), "level {level}");
            assert_eq!(u16::try_from(chain), Ok(config.max_chain), "level {level}");
            assert!(nice >= 0, "level {level}");
        }
    }

    /// For every level C accepts, the accessor returns exactly the row a direct
    /// index would.
    #[test]
    fn config_for_level_agrees_with_direct_indexing() {
        for (index, expected) in CONFIGURATION_TABLE.iter().enumerate() {
            let level = i32::try_from(index).unwrap();
            assert_eq!(config_for_level(level), expected, "level {level}");
        }
    }

    /// Levels outside `0..=9` clamp instead of panicking.
    ///
    /// None of these is reachable through a validated stream -- the range is
    /// established before the table is ever read -- but the accessor is total, so
    /// a slip cannot abort a C caller's process.
    #[test]
    fn config_for_level_clamps_levels_outside_the_valid_range() {
        let lowest = &CONFIGURATION_TABLE[0];
        let highest = &CONFIGURATION_TABLE[9];

        for level in [-1_i32, -6, i32::MIN] {
            assert_eq!(config_for_level(level), lowest, "level {level}");
        }
        for level in [10_i32, 99, i32::MAX] {
            assert_eq!(config_for_level(level), highest, "level {level}");
        }
    }

    /// The table is `const` data usable in a `const` context, with no runtime
    /// initialization of any kind.
    ///
    /// C's table is `local const config configuration_table[10]` (L112); nothing
    /// here may become a `static mut`, a lazily built value or otherwise
    /// observable as state.
    #[test]
    fn the_table_is_const_evaluable() {
        const LEVEL_SIX: Config = CONFIGURATION_TABLE[6];
        const NICE_AT_NINE: u16 = CONFIGURATION_TABLE[9].nice_length;

        assert_eq!(LEVEL_SIX.good_length, 8);
        assert_eq!(LEVEL_SIX.max_lazy, 16);
        assert_eq!(LEVEL_SIX.nice_length, 128);
        assert_eq!(LEVEL_SIX.max_chain, 128);
        assert_eq!(LEVEL_SIX.func, CompressFunc::Slow);
        assert_eq!(NICE_AT_NINE, 258);
    }
}
