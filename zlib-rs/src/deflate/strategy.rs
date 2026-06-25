//! DEFLATE per-level configuration table and internal block-producer dispatch.
//!
//! This module is the safe-Rust translation of three tightly-related pieces of
//! the C compressor:
//!
//! * the `compress_func` typedef (`deflate.c` line 70,
//!   `block_state (*)(deflate_state *s, int flush)`) — the C function pointer
//!   that selects which per-block producer runs;
//! * the `config_s` struct together with the `configuration_table[10]` array
//!   (`deflate.c` lines 98-124) — the per-level tuning parameters
//!   (`good_length`, `max_lazy`, `nice_length`, `max_chain`) plus the block
//!   producer chosen for that level; and
//! * the per-call producer-selection expression (`deflate.c` lines 1217-1220).
//!
//! # Two distinct `Strategy` types (binding)
//!
//! Rust has *two* unrelated "strategy" concepts here and they must never be
//! conflated:
//!
//! * [`Strategy`] (this module) is the **internal block-producer selector**. It
//!   has exactly the three variants that appear in the C `configuration_table` —
//!   [`Strategy::Stored`], [`Strategy::Fast`], and [`Strategy::Slow`] — and
//!   replaces the C `compress_func` pointer with a statically-dispatched enum.
//! * [`crate::constants::Strategy`] (imported here as `CompressionStrategy`) is
//!   the **public-API compression strategy** (`Z_DEFAULT_STRATEGY`,
//!   `Z_FILTERED`, `Z_HUFFMAN_ONLY`, `Z_RLE`, `Z_FIXED`). The Huffman-only and
//!   RLE producers are *level-independent overrides* keyed off this public
//!   value; they are deliberately **not** entries in [`CONFIG_TABLE`].
//!
//! # Bit-exactness (binding)
//!
//! The numeric values in [`CONFIG_TABLE`] are correctness-critical, not merely
//! performance hints: the engine copies them into `good_match`,
//! `max_lazy_match`, `nice_match`, and `max_chain_length`, which together
//! control exactly which LZ77 matches are chosen at each level. Any deviation
//! changes the compressed bytes and breaks the byte-for-byte compatibility
//! guarantee with C zlib (AAP §0.6.4). The table is reproduced verbatim from
//! `configuration_table[10]` (`deflate.c` lines 112-124).
//!
//! The whole module is part of the crate-wide `#![forbid(unsafe_code)]` core and
//! relies only on `core` (no `std`), so it builds under the `no_std`
//! configuration that mirrors the C `Z_SOLO` mode.

use super::DeflateContext;
use super::state::BlockState;
use crate::constants::{Flush, Strategy as CompressionStrategy};

/// The internal block producer chosen for a given compression level.
///
/// This is the safe-Rust replacement for the C `compress_func` function pointer
/// stored in `configuration_table[level].func` (`deflate.c` lines 70, 103). It
/// names only the three producers that can appear in the table; the
/// Huffman-only and RLE producers are level-independent overrides selected
/// separately (see [`select`]).
///
/// Static dispatch through this enum (rather than a boxed `fn` pointer or a
/// `dyn Fn`) keeps the hot deflate loop allocation-free and inlinable while
/// honoring the crate-wide `#![forbid(unsafe_code)]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strategy {
    /// `deflate_stored` — level 0, no compression (stored blocks only).
    Stored,
    /// `deflate_fast` — levels 1-3, greedy matching with no lazy evaluation.
    Fast,
    /// `deflate_slow` — levels 4-9, lazy match evaluation for tighter output.
    Slow,
}

/// Per-level tuning parameters — the safe translation of the C `config_s`
/// struct (`deflate.c` lines 98-104).
///
/// Each field mirrors a C `ush` (16-bit unsigned) member exactly, so the stored
/// width here is the narrow `u16` the table literals require. The engine widens
/// these values when copying them into the running
/// [`crate::deflate::state::DeflateState`] (`good_match`, `max_lazy_match`,
/// `nice_match`, `max_chain_length`).
///
/// `PartialEq`/`Eq`/`Debug` are derived in addition to the `Copy` semantics so
/// the transcription-guard tests can compare and print table entries; they have
/// no effect on the compressed output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// C `good_length` — reduce the lazy search above this match length.
    pub good_length: u16,
    /// C `max_lazy` — do not perform a lazy search above this match length.
    ///
    /// For `deflate_fast` (levels ≤ 3) the C note at `deflate.c` lines 127-129
    /// documents the overloaded meaning: `good` is ignored and `lazy` bounds the
    /// greedily-emitted match length instead.
    pub max_lazy: u16,
    /// C `nice_length` — stop the match search once a match this long is found.
    pub nice_length: u16,
    /// C `max_chain` — upper bound on hash-chain traversal length.
    pub max_chain: u16,
    /// The block producer used at this level (replaces the C `compress_func`
    /// pointer).
    pub func: Strategy,
}

/// The per-level configuration table — a verbatim translation of the C
/// `configuration_table[10]` (`deflate.c` lines 112-124).
///
/// Indexed by compression level `0..=9`. **These values are binding and
/// bit-exact**: they determine the LZ77 match decisions at every level, so they
/// must match the C source exactly to preserve byte-for-byte output
/// compatibility (AAP §0.6.4).
///
/// The hand-aligned tabular layout (preserved with `#[rustfmt::skip]`, matching
/// the convention used for the fixed-Huffman tables in
/// [`crate::inflate::fixed`]) mirrors the C `configuration_table` for easy
/// side-by-side review.
#[rustfmt::skip]
pub static CONFIG_TABLE: [Config; 10] = [
    //       good_length  max_lazy  nice_length  max_chain  func
    Config { good_length: 0,  max_lazy: 0,   nice_length: 0,   max_chain: 0,    func: Strategy::Stored }, // 0 store only
    Config { good_length: 4,  max_lazy: 4,   nice_length: 8,   max_chain: 4,    func: Strategy::Fast },   // 1 max speed, no lazy matches
    Config { good_length: 4,  max_lazy: 5,   nice_length: 16,  max_chain: 8,    func: Strategy::Fast },   // 2
    Config { good_length: 4,  max_lazy: 6,   nice_length: 32,  max_chain: 32,   func: Strategy::Fast },   // 3
    Config { good_length: 4,  max_lazy: 4,   nice_length: 16,  max_chain: 16,   func: Strategy::Slow },   // 4 lazy matches
    Config { good_length: 8,  max_lazy: 16,  nice_length: 32,  max_chain: 32,   func: Strategy::Slow },   // 5
    Config { good_length: 8,  max_lazy: 16,  nice_length: 128, max_chain: 128,  func: Strategy::Slow },   // 6
    Config { good_length: 8,  max_lazy: 32,  nice_length: 128, max_chain: 256,  func: Strategy::Slow },   // 7
    Config { good_length: 32, max_lazy: 128, nice_length: 258, max_chain: 1024, func: Strategy::Slow },   // 8
    Config { good_length: 32, max_lazy: 258, nice_length: 258, max_chain: 4096, func: Strategy::Slow },   // 9 max compression
];

// Compile-time guards against transcription errors in CONFIG_TABLE. These are
// evaluated during const-eval and cost nothing at runtime; a wrong value fails
// the build.
const _: () = assert!(CONFIG_TABLE.len() == 10);
// Level 0 is the store-only configuration with all-zero parameters.
const _: () = assert!(CONFIG_TABLE[0].max_lazy == 0 && CONFIG_TABLE[0].max_chain == 0);
// Level 1 is the canonical "max speed" fast configuration.
const _: () = assert!(CONFIG_TABLE[1].good_length == 4 && CONFIG_TABLE[1].max_chain == 4);
// Level 6 is the library default level; verify its well-known chain length.
const _: () = assert!(CONFIG_TABLE[6].nice_length == 128 && CONFIG_TABLE[6].max_chain == 128);
// Level 9 is max compression with the deepest hash chain.
const _: () = assert!(CONFIG_TABLE[9].max_chain == 4096 && CONFIG_TABLE[9].nice_length == 258);

/// The concrete per-block producer chosen for a single `deflate` call.
///
/// This unifies the five C block functions reachable from the dispatch
/// expression at `deflate.c` lines 1217-1220 (`deflate_stored`, `deflate_fast`,
/// `deflate_slow`, `deflate_huff`, `deflate_rle`). Unlike the internal
/// [`Strategy`] (which describes only the *table* entries), this enum also
/// carries the two level-independent overrides ([`BlockProducer::Huff`] and
/// [`BlockProducer::Rle`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlockProducer {
    /// `deflate_stored` — copy input through as stored blocks (level 0).
    Stored,
    /// `deflate_fast` — greedy match search (levels 1-3).
    Fast,
    /// `deflate_slow` — lazy match search (levels 4-9).
    Slow,
    /// `deflate_huff` — Huffman coding only (`Z_HUFFMAN_ONLY`).
    Huff,
    /// `deflate_rle` — run-length encoding only (`Z_RLE`).
    Rle,
}

/// Select the block producer for a `deflate` call, translating the C dispatch
/// expression at `deflate.c` lines 1217-1220:
///
/// ```text
/// bstate = s->level == 0            ? deflate_stored
///        : s->strategy == Z_HUFFMAN_ONLY ? deflate_huff
///        : s->strategy == Z_RLE          ? deflate_rle
///        : configuration_table[s->level].func;
/// ```
///
/// The evaluation order is significant and preserved exactly: a level of `0`
/// selects [`BlockProducer::Stored`] **regardless of strategy**, after which the
/// Huffman-only and RLE overrides take precedence over the level's table entry.
///
/// # Parameters
/// * `level` — the compression level; the engine guarantees this is in `0..=9`
///   (the public `-1` default is resolved to `6` before this point).
/// * `strategy` — the public-API compression strategy
///   ([`crate::constants::Strategy`]).
///
/// # Panics
/// In debug builds this panics if `level` is outside `0..=9` (a caller-contract
/// violation). Release builds rely on the same bounds check performed by the
/// `CONFIG_TABLE` index, which is a safe panic rather than the C code's
/// out-of-bounds read.
pub(crate) fn select(level: i32, strategy: CompressionStrategy) -> BlockProducer {
    debug_assert!(
        (0..=9).contains(&level),
        "compression level must be in 0..=9 (got {level})"
    );

    // C `s->level == 0 ? deflate_stored : ...` — level 0 is store-only and
    // overrides any requested strategy, so it must be tested first.
    if level == 0 {
        return BlockProducer::Stored;
    }

    match strategy {
        // Level-independent overrides, applied before the per-level table.
        CompressionStrategy::HuffmanOnly => BlockProducer::Huff,
        CompressionStrategy::Rle => BlockProducer::Rle,
        // Default / Filtered / Fixed defer to the per-level table entry.
        CompressionStrategy::Default
        | CompressionStrategy::Filtered
        | CompressionStrategy::Fixed => match CONFIG_TABLE[level as usize].func {
            Strategy::Stored => BlockProducer::Stored,
            Strategy::Fast => BlockProducer::Fast,
            Strategy::Slow => BlockProducer::Slow,
        },
    }
}

impl BlockProducer {
    /// Run the selected block producer for one `deflate` call.
    ///
    /// This is a thin, statically-dispatched `match` that forwards to the
    /// concrete block function in the sibling module — the safe-Rust equivalent
    /// of invoking the C `compress_func` pointer
    /// (`(*(configuration_table[level].func))(s, flush)`, `deflate.c` line 1220).
    /// Every producer shares the same `(cx, flush) -> BlockState` signature
    /// (design decision D1: all per-call stream scalars are threaded through the
    /// [`DeflateContext`] engine I/O context rather than stored on the state).
    pub(crate) fn run(self, cx: &mut DeflateContext<'_>, flush: Flush) -> BlockState {
        match self {
            BlockProducer::Stored => super::stored::deflate_stored(cx, flush),
            BlockProducer::Fast => super::fast::deflate_fast(cx, flush),
            BlockProducer::Slow => super::slow::deflate_slow(cx, flush),
            BlockProducer::Huff => super::huff::deflate_huff(cx, flush),
            BlockProducer::Rle => super::rle::deflate_rle(cx, flush),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockProducer, CONFIG_TABLE, Strategy, select};
    use crate::constants::Strategy as CompressionStrategy;

    /// Transcription guard: assert every [`CONFIG_TABLE`] row matches the C
    /// `configuration_table[10]` tuple list (`deflate.c` lines 112-124) exactly.
    /// The expected values are written here as independent tuples so a typo in
    /// either transcription is caught.
    #[test]
    fn config_table_matches_c_source() {
        // (good_length, max_lazy, nice_length, max_chain, func)
        const EXPECTED: [(u16, u16, u16, u16, Strategy); 10] = [
            (0, 0, 0, 0, Strategy::Stored),
            (4, 4, 8, 4, Strategy::Fast),
            (4, 5, 16, 8, Strategy::Fast),
            (4, 6, 32, 32, Strategy::Fast),
            (4, 4, 16, 16, Strategy::Slow),
            (8, 16, 32, 32, Strategy::Slow),
            (8, 16, 128, 128, Strategy::Slow),
            (8, 32, 128, 256, Strategy::Slow),
            (32, 128, 258, 1024, Strategy::Slow),
            (32, 258, 258, 4096, Strategy::Slow),
        ];

        assert_eq!(CONFIG_TABLE.len(), EXPECTED.len());
        for (level, (config, expected)) in CONFIG_TABLE.iter().zip(EXPECTED.iter()).enumerate() {
            let (good, lazy, nice, chain, func) = *expected;
            assert_eq!(
                config.good_length, good,
                "good_length mismatch at level {level}"
            );
            assert_eq!(config.max_lazy, lazy, "max_lazy mismatch at level {level}");
            assert_eq!(
                config.nice_length, nice,
                "nice_length mismatch at level {level}"
            );
            assert_eq!(
                config.max_chain, chain,
                "max_chain mismatch at level {level}"
            );
            assert_eq!(config.func, func, "func mismatch at level {level}");
        }
    }

    /// Level 0 uses the stored producer, levels 1-3 the fast producer, and
    /// levels 4-9 the slow producer in the table itself.
    #[test]
    fn config_table_func_partition() {
        for (level, config) in CONFIG_TABLE.iter().enumerate() {
            let expected = match level {
                0 => Strategy::Stored,
                1..=3 => Strategy::Fast,
                _ => Strategy::Slow,
            };
            assert_eq!(config.func, expected, "func mismatch at level {level}");
        }
    }

    /// Dispatch parity with the C expression (`deflate.c` lines 1217-1220) for
    /// the default strategy: level 0 → Stored, 1-3 → Fast, 4-9 → Slow.
    #[test]
    fn select_default_strategy_by_level() {
        assert_eq!(
            select(0, CompressionStrategy::Default),
            BlockProducer::Stored
        );
        for level in 1..=3 {
            assert_eq!(
                select(level, CompressionStrategy::Default),
                BlockProducer::Fast,
                "level {level}"
            );
        }
        for level in 4..=9 {
            assert_eq!(
                select(level, CompressionStrategy::Default),
                BlockProducer::Slow,
                "level {level}"
            );
        }
    }

    /// Huffman-only and RLE are level-independent overrides for every non-zero
    /// level.
    #[test]
    fn select_strategy_overrides() {
        for level in 1..=9 {
            assert_eq!(
                select(level, CompressionStrategy::HuffmanOnly),
                BlockProducer::Huff,
                "HuffmanOnly at level {level}"
            );
            assert_eq!(
                select(level, CompressionStrategy::Rle),
                BlockProducer::Rle,
                "Rle at level {level}"
            );
        }
    }

    /// Level 0 overrides every strategy (the C `s->level == 0` test comes first,
    /// before the strategy comparisons).
    #[test]
    fn select_level_zero_overrides_strategy() {
        for strat in [
            CompressionStrategy::Default,
            CompressionStrategy::Filtered,
            CompressionStrategy::HuffmanOnly,
            CompressionStrategy::Rle,
            CompressionStrategy::Fixed,
        ] {
            assert_eq!(
                select(0, strat),
                BlockProducer::Stored,
                "level 0 with {strat:?}"
            );
        }
    }

    /// The Filtered and Fixed strategies defer to the per-level table entry, so
    /// they follow the same fast/slow partition as the default strategy.
    #[test]
    fn select_filtered_and_fixed_use_table() {
        assert_eq!(
            select(2, CompressionStrategy::Filtered),
            BlockProducer::Fast
        );
        assert_eq!(
            select(7, CompressionStrategy::Filtered),
            BlockProducer::Slow
        );
        assert_eq!(select(2, CompressionStrategy::Fixed), BlockProducer::Fast);
        assert_eq!(select(7, CompressionStrategy::Fixed), BlockProducer::Slow);
    }
}
