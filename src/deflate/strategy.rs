//! Deflate strategy selection: block-state signalling, compress-function
//! dispatch tags, and the per-level heuristic configuration table.
//!
//! This module is the safe-Rust translation of three closely related pieces of
//! `deflate.c` from **zlib 1.3.2.1-motley**:
//!
//! * the `block_state` enumeration (`deflate.c` L63-L68) — the value every
//!   block producer returns to tell the `deflate()` driver how far it
//!   progressed;
//! * the `compress_func` function-pointer typedef (`deflate.c` L70) — replaced
//!   here by the [`CompressFunc`] tag enum so the driver can dispatch with an
//!   exhaustive `match` instead of an indirect call; and
//! * the `config` struct and its `configuration_table` (`deflate.c` L88-L124) —
//!   the ten tuning presets (one per compression level `0..=9`) that steer the
//!   match finder.
//!
//! # Strategy pattern, without function pointers
//!
//! The C library selects a block producer through a table of raw function
//! pointers (`compress_func`). Porting that verbatim would require `unsafe` and
//! would forfeit the compiler's exhaustiveness checking. Instead, each row of
//! [`CONFIGURATION_TABLE`] carries a [`CompressFunc`] *tag*; the deflate driver
//! in `mod.rs` turns that tag into a concrete call with a `match`. This keeps
//! the whole compression core free of `unsafe` (AAP §0.3.2, §0.6.2) while
//! preserving the exact selection behaviour of the original.
//!
//! # This module is intentionally data-only
//!
//! It contains **no** compression logic and does **not** call the block
//! producers. Its only dependency is [`crate::constants::Strategy`]. Keeping it
//! dependency-light lets it be compiled before the block producers — which all
//! return [`BlockState`] — without creating an import cycle.
//!
//! # Byte-for-byte compatibility
//!
//! The heuristic fields (`good_length`, `max_lazy`, `nice_length`, `max_chain`)
//! drive the lazy-match decisions that determine the emitted token stream. They
//! are reproduced **exactly** from `deflate.c`; altering any value would make
//! the compressed output diverge from reference zlib (AAP §0.6.4).
//!
//! # Safety and portability
//!
//! Zero `unsafe`, no heap allocation, and `no_std`-compatible: the module
//! relies only on `core`.

use crate::constants::Strategy;

/// Outcome reported by every block producer (`deflate_stored`, `deflate_fast`,
/// `deflate_slow`, `deflate_rle`, `deflate_huff`) back to the deflate driver.
///
/// This is the Rust translation of the C `block_state` enumeration
/// (`deflate.c` L63-L68). The discriminants are fixed to `0..=3` so that each
/// state maps one-to-one onto the original source; the driver in `mod.rs`
/// branches on these values to decide whether to return to the caller for more
/// input/output or to transition into the finishing state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockState {
    /// `need_more` (0): the block is not complete; the producer needs more
    /// input, or more output space, before it can continue.
    NeedMore = 0,
    /// `block_done` (1): the current block was completed and flushed.
    BlockDone = 1,
    /// `finish_started` (2): finishing has begun; only more *output* space is
    /// required to complete the stream at the next `deflate` call.
    FinishStarted = 2,
    /// `finish_done` (3): the stream is fully finished; no further input or
    /// output will be accepted.
    FinishDone = 3,
}

/// Tag identifying *which* block producer a compression level selects.
///
/// Replaces the C `compress_func` function-pointer typedef (`deflate.c` L70).
/// The deflate driver in `mod.rs` maps each tag to its implementation with an
/// exhaustive `match`, e.g. [`CompressFunc::Fast`] → `deflate_fast`.
///
/// # Which variants the table uses
///
/// [`CONFIGURATION_TABLE`] (and [`FASTEST_TABLE`]) reference only
/// [`Stored`](CompressFunc::Stored), [`Fast`](CompressFunc::Fast) and
/// [`Slow`](CompressFunc::Slow), exactly as the C `configuration_table` does.
/// [`Rle`](CompressFunc::Rle) and [`Huff`](CompressFunc::Huff) are never stored
/// in the table: the driver selects them directly from the requested
/// [`Strategy`] (`Z_RLE` → `deflate_rle`, `Z_HUFFMAN_ONLY` → `deflate_huff`;
/// `deflate.c` L1218-L1219). They are
/// included here so the enum names every block producer and so
/// [`select_compress_func`] can express the full selection logic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompressFunc {
    /// `deflate_stored`: no compression — the input is copied into stored
    /// (uncompressed) DEFLATE blocks. Selected for level 0.
    Stored,
    /// `deflate_fast`: greedy match finder with no lazy evaluation. Selected
    /// for levels 1-3.
    Fast,
    /// `deflate_slow`: lazy-matching finder that can defer a match by one byte
    /// for better compression. Selected for levels 4-9.
    Slow,
    /// `deflate_rle`: run-length encoding, limiting match distances to one.
    /// Selected directly by [`Strategy::Rle`], not via the table.
    Rle,
    /// `deflate_huff`: Huffman coding only, with no string matching. Selected
    /// directly by [`Strategy::HuffmanOnly`], not via the table.
    Huff,
}

/// One row of the deflate tuning table: the match-finder heuristics for a
/// single compression level, plus the block producer that level uses.
///
/// Mirrors the C `config` struct (`deflate.c` L98-L104), whose fields are
/// `ush good_length; ush max_lazy; ush nice_length; ush max_chain;
/// compress_func func;` — the four `ush` (unsigned short) heuristics become
/// `u16`, and the function pointer becomes a [`CompressFunc`] tag.
#[derive(Clone, Copy)]
pub struct Config {
    /// Reduce the lazy search once the current match is at least this long
    /// (`good_length`). Only consulted by [`CompressFunc::Slow`].
    pub good_length: u16,
    /// Do not start a lazy search when a match this long or longer has already
    /// been found (`max_lazy`). For [`CompressFunc::Fast`] this field has a
    /// different meaning (see `deflate.c` L127-L129).
    pub max_lazy: u16,
    /// Stop searching altogether once a match reaches this length
    /// (`nice_length`).
    pub nice_length: u16,
    /// Maximum number of hash-chain links to follow while searching for a
    /// match (`max_chain`).
    pub max_chain: u16,
    /// Block producer selected for this level.
    pub func: CompressFunc,
}

/// Per-level deflate tuning presets, indexed by compression level `0..=9`.
///
/// Faithful port of the C `configuration_table[10]` (`deflate.c` L112-L124).
/// The values were tuned by the zlib authors to avoid pathological worst-case
/// behaviour and **must not change**: together with the match finder they
/// determine the exact token stream, and therefore the exact compressed bytes,
/// produced at each level (AAP §0.6.4).
///
/// | Level | good | lazy | nice | chain | producer |
/// |------:|-----:|-----:|-----:|------:|----------|
/// |     0 |    0 |    0 |    0 |     0 | `Stored` |
/// |     1 |    4 |    4 |    8 |     4 | `Fast`   |
/// |     2 |    4 |    5 |   16 |     8 | `Fast`   |
/// |     3 |    4 |    6 |   32 |    32 | `Fast`   |
/// |     4 |    4 |    4 |   16 |    16 | `Slow`   |
/// |     5 |    8 |   16 |   32 |    32 | `Slow`   |
/// |     6 |    8 |   16 |  128 |   128 | `Slow`   |
/// |     7 |    8 |   32 |  128 |   256 | `Slow`   |
/// |     8 |   32 |  128 |  258 |  1024 | `Slow`   |
/// |     9 |   32 |  258 |  258 |  4096 | `Slow`   |
///
/// Level 6 is the library default. Note that `deflate.c` (L127) requires
/// `max_lazy >= MIN_MATCH` and `max_chain >= 4` for the actively compressing
/// levels.
#[rustfmt::skip]
pub const CONFIGURATION_TABLE: [Config; 10] = [
    // good_length      max_lazy       nice_length       max_chain              func
    Config { good_length:  0, max_lazy:   0, nice_length:   0, max_chain:    0, func: CompressFunc::Stored }, // 0 store only
    Config { good_length:  4, max_lazy:   4, nice_length:   8, max_chain:    4, func: CompressFunc::Fast   }, // 1 max speed, no lazy matches
    Config { good_length:  4, max_lazy:   5, nice_length:  16, max_chain:    8, func: CompressFunc::Fast   }, // 2
    Config { good_length:  4, max_lazy:   6, nice_length:  32, max_chain:   32, func: CompressFunc::Fast   }, // 3
    Config { good_length:  4, max_lazy:   4, nice_length:  16, max_chain:   16, func: CompressFunc::Slow   }, // 4 lazy matches
    Config { good_length:  8, max_lazy:  16, nice_length:  32, max_chain:   32, func: CompressFunc::Slow   }, // 5
    Config { good_length:  8, max_lazy:  16, nice_length: 128, max_chain:  128, func: CompressFunc::Slow   }, // 6 (default)
    Config { good_length:  8, max_lazy:  32, nice_length: 128, max_chain:  256, func: CompressFunc::Slow   }, // 7
    Config { good_length: 32, max_lazy: 128, nice_length: 258, max_chain: 1024, func: CompressFunc::Slow   }, // 8
    Config { good_length: 32, max_lazy: 258, nice_length: 258, max_chain: 4096, func: CompressFunc::Slow   }, // 9 max compression
];

/// Two-level tuning presets used only when zlib is built with `-DFASTEST`.
///
/// Port of the C `configuration_table[2]` guarded by `#ifdef FASTEST`
/// (`deflate.c` L107-L110). `FASTEST` forces the compression level to 1 and
/// maintains no hash chains, trading ratio for raw speed.
///
/// This mode is **off by default**; the standard build uses
/// [`CONFIGURATION_TABLE`]. The table is retained here for parity and
/// documentation and may be selected behind a build feature by the deflate
/// engine. Its two rows are identical to levels 0 and 1 of
/// [`CONFIGURATION_TABLE`].
#[rustfmt::skip]
pub const FASTEST_TABLE: [Config; 2] = [
    Config { good_length: 0, max_lazy: 0, nice_length: 0, max_chain: 0, func: CompressFunc::Stored }, // 0 store only
    Config { good_length: 4, max_lazy: 4, nice_length: 8, max_chain: 4, func: CompressFunc::Fast   }, // 1 max speed
];

/// Rank a flush mode for the flush-ordering comparison in `deflate()`.
///
/// Direct port of the C `RANK` macro (`deflate.c` L133),
/// `(((f) * 2) - ((f) > 4 ? 9 : 0))`. Its purpose (`deflate.c` L132) is to rank
/// `Z_BLOCK` (`5`) between `Z_NO_FLUSH` (`0`) and `Z_PARTIAL_FLUSH` (`1`) so the
/// driver can compare the requested flush mode against the previous one in the
/// correct order.
///
/// The resulting ranks for the seven flush modes are:
///
/// | flush | `Z_NO_FLUSH` (0) | `Z_PARTIAL_FLUSH` (1) | `Z_SYNC_FLUSH` (2) | `Z_FULL_FLUSH` (3) | `Z_FINISH` (4) | `Z_BLOCK` (5) | `Z_TREES` (6) |
/// |------:|:----------------:|:---------------------:|:------------------:|:------------------:|:--------------:|:-------------:|:-------------:|
/// | rank  |        0         |           2           |         4          |         6          |       8        |       1       |       3       |
#[must_use]
#[inline]
pub const fn rank(f: i32) -> i32 {
    // `f > 4` is true only for Z_BLOCK (5) and Z_TREES (6); the `- 9` pulls
    // their ranks down between Z_NO_FLUSH and Z_PARTIAL_FLUSH.
    let penalty = if f > 4 { 9 } else { 0 };
    f * 2 - penalty
}

/// Select the block producer for a given compression level and strategy.
///
/// Reproduces the dispatch precedence of the `deflate()` driver
/// (`deflate.c` L1217-L1220), which is:
///
/// 1. `level == 0` → [`CompressFunc::Stored`] (this wins over any strategy);
/// 2. otherwise [`Strategy::HuffmanOnly`] → [`CompressFunc::Huff`];
/// 3. otherwise [`Strategy::Rle`] → [`CompressFunc::Rle`];
/// 4. otherwise the producer recorded in [`CONFIGURATION_TABLE`] for that level
///    ([`Strategy::Default`], [`Strategy::Filtered`] and [`Strategy::Fixed`] all
///    fall through to the table).
///
/// `Filtered` and `Fixed` still use the level's table producer for match
/// finding; they alter *tree* behaviour elsewhere (`Fixed` forces static
/// blocks in `_tr_flush_block`; `Filtered` changes the match-acceptance filter
/// in `deflate_slow`) rather than which producer runs here.
///
/// # Preconditions
///
/// `level` must be a concrete, already-resolved compression level in the range
/// `0..=9`. The C API resolves `Z_DEFAULT_COMPRESSION` (`-1`) to `6` during
/// initialization, so by the time this dispatch runs the level is always in
/// range. In debug builds an out-of-range `level` (other than `0`) trips a
/// `debug_assert!`; in release builds it would panic on the table index — a
/// safe, defined alternative to the C code's out-of-bounds access.
#[must_use]
#[inline]
pub fn select_compress_func(level: i32, strategy: Strategy) -> CompressFunc {
    // deflate.c L1217: `s->level == 0 ? deflate_stored(...) : ...` — level 0 is
    // always stored, regardless of the requested strategy.
    if level == 0 {
        return CompressFunc::Stored;
    }

    // deflate.c L1218-L1219: HUFFMAN_ONLY and RLE are dispatched straight from
    // the strategy; every other strategy consults the configuration table.
    match strategy {
        Strategy::HuffmanOnly => CompressFunc::Huff,
        Strategy::Rle => CompressFunc::Rle,
        Strategy::Default | Strategy::Filtered | Strategy::Fixed => {
            debug_assert!(
                (1..=9).contains(&level),
                "select_compress_func: `level` must be a resolved value in 0..=9"
            );
            CONFIGURATION_TABLE[level as usize].func
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockState, CONFIGURATION_TABLE, CompressFunc, FASTEST_TABLE, rank, select_compress_func,
    };
    use crate::constants::Strategy;

    /// The canonical zlib per-level parameters, transcribed independently from
    /// `deflate.c` L112-L124 so this test fails loudly if `CONFIGURATION_TABLE`
    /// is ever edited to diverge from the reference.
    const EXPECTED: [(u16, u16, u16, u16, CompressFunc); 10] = [
        (0, 0, 0, 0, CompressFunc::Stored),
        (4, 4, 8, 4, CompressFunc::Fast),
        (4, 5, 16, 8, CompressFunc::Fast),
        (4, 6, 32, 32, CompressFunc::Fast),
        (4, 4, 16, 16, CompressFunc::Slow),
        (8, 16, 32, 32, CompressFunc::Slow),
        (8, 16, 128, 128, CompressFunc::Slow),
        (8, 32, 128, 256, CompressFunc::Slow),
        (32, 128, 258, 1024, CompressFunc::Slow),
        (32, 258, 258, 4096, CompressFunc::Slow),
    ];

    #[test]
    fn configuration_table_matches_zlib() {
        assert_eq!(CONFIGURATION_TABLE.len(), 10);
        for (level, cfg) in CONFIGURATION_TABLE.iter().enumerate() {
            let (good, lazy, nice, chain, func) = EXPECTED[level];
            assert_eq!(
                cfg.good_length, good,
                "good_length mismatch at level {level}"
            );
            assert_eq!(cfg.max_lazy, lazy, "max_lazy mismatch at level {level}");
            assert_eq!(
                cfg.nice_length, nice,
                "nice_length mismatch at level {level}"
            );
            assert_eq!(cfg.max_chain, chain, "max_chain mismatch at level {level}");
            assert_eq!(cfg.func, func, "func mismatch at level {level}");
        }
    }

    #[test]
    fn configuration_table_spot_checks() {
        // Cross-checks called out in the agent-prompt validation checklist.
        let l1 = CONFIGURATION_TABLE[1];
        assert_eq!(
            (
                l1.good_length,
                l1.max_lazy,
                l1.nice_length,
                l1.max_chain,
                l1.func
            ),
            (4, 4, 8, 4, CompressFunc::Fast)
        );
        let l6 = CONFIGURATION_TABLE[6];
        assert_eq!(
            (
                l6.good_length,
                l6.max_lazy,
                l6.nice_length,
                l6.max_chain,
                l6.func
            ),
            (8, 16, 128, 128, CompressFunc::Slow)
        );
        let l9 = CONFIGURATION_TABLE[9];
        assert_eq!(
            (
                l9.good_length,
                l9.max_lazy,
                l9.nice_length,
                l9.max_chain,
                l9.func
            ),
            (32, 258, 258, 4096, CompressFunc::Slow)
        );
    }

    #[test]
    fn fastest_table_matches_zlib() {
        assert_eq!(FASTEST_TABLE.len(), 2);
        assert_eq!(
            (
                FASTEST_TABLE[0].good_length,
                FASTEST_TABLE[0].max_lazy,
                FASTEST_TABLE[0].nice_length,
                FASTEST_TABLE[0].max_chain,
                FASTEST_TABLE[0].func,
            ),
            (0, 0, 0, 0, CompressFunc::Stored)
        );
        assert_eq!(
            (
                FASTEST_TABLE[1].good_length,
                FASTEST_TABLE[1].max_lazy,
                FASTEST_TABLE[1].nice_length,
                FASTEST_TABLE[1].max_chain,
                FASTEST_TABLE[1].func,
            ),
            (4, 4, 8, 4, CompressFunc::Fast)
        );
        // The FASTEST rows equal levels 0 and 1 of the standard table.
        assert_eq!(FASTEST_TABLE[0].func, CONFIGURATION_TABLE[0].func);
        assert_eq!(FASTEST_TABLE[1].func, CONFIGURATION_TABLE[1].func);
    }

    #[test]
    fn block_state_discriminants() {
        assert_eq!(BlockState::NeedMore as i32, 0);
        assert_eq!(BlockState::BlockDone as i32, 1);
        assert_eq!(BlockState::FinishStarted as i32, 2);
        assert_eq!(BlockState::FinishDone as i32, 3);
        // Equality behaves as expected.
        assert_ne!(BlockState::NeedMore, BlockState::FinishDone);
        assert_eq!(BlockState::BlockDone, BlockState::BlockDone);
    }

    #[test]
    fn compress_func_variants_are_distinct() {
        let all = [
            CompressFunc::Stored,
            CompressFunc::Fast,
            CompressFunc::Slow,
            CompressFunc::Rle,
            CompressFunc::Huff,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn rank_matches_c_macro() {
        // Independent reference implementation of the C RANK macro.
        fn c_rank(f: i32) -> i32 {
            f * 2 - if f > 4 { 9 } else { 0 }
        }
        for f in -3..=10 {
            assert_eq!(rank(f), c_rank(f), "rank mismatch at f={f}");
        }
        // The documented purpose: Z_BLOCK (5) ranks between Z_NO_FLUSH (0) and
        // Z_PARTIAL_FLUSH (1).
        assert_eq!(rank(0), 0);
        assert_eq!(rank(5), 1);
        assert_eq!(rank(1), 2);
        assert!(rank(0) < rank(5));
        assert!(rank(5) < rank(1));
    }

    #[test]
    fn select_level_zero_is_stored_for_every_strategy() {
        for s in [
            Strategy::Default,
            Strategy::Filtered,
            Strategy::HuffmanOnly,
            Strategy::Rle,
            Strategy::Fixed,
        ] {
            assert_eq!(
                select_compress_func(0, s),
                CompressFunc::Stored,
                "level 0 must be Stored regardless of strategy {s:?}"
            );
        }
    }

    #[test]
    fn select_strategy_overrides_for_nonzero_levels() {
        for level in 1..=9 {
            assert_eq!(
                select_compress_func(level, Strategy::HuffmanOnly),
                CompressFunc::Huff
            );
            assert_eq!(
                select_compress_func(level, Strategy::Rle),
                CompressFunc::Rle
            );
        }
    }

    #[test]
    fn select_table_strategies_use_level_func() {
        for level in 1..=9 {
            let expected = CONFIGURATION_TABLE[level as usize].func;
            assert_eq!(select_compress_func(level, Strategy::Default), expected);
            assert_eq!(select_compress_func(level, Strategy::Filtered), expected);
            assert_eq!(select_compress_func(level, Strategy::Fixed), expected);
        }
        // Concrete spot checks against the table.
        assert_eq!(
            select_compress_func(1, Strategy::Default),
            CompressFunc::Fast
        );
        assert_eq!(
            select_compress_func(3, Strategy::Filtered),
            CompressFunc::Fast
        );
        assert_eq!(
            select_compress_func(4, Strategy::Default),
            CompressFunc::Slow
        );
        assert_eq!(
            select_compress_func(6, Strategy::Default),
            CompressFunc::Slow
        );
        assert_eq!(select_compress_func(9, Strategy::Fixed), CompressFunc::Slow);
    }
}
