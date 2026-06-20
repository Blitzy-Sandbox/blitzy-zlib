//! Per-compression-level configuration table, ported verbatim from the
//! `configuration_table` in `deflate.c` (L112-124).
//!
//! # Byte-exactness anchor
//!
//! Each of the ten compression levels (`0`..=`9`) selects a tuned set of LZ77
//! match-search parameters. Reproducing these values *exactly* is the single
//! most important byte-exactness anchor for DEFLATE output (AAP §0.6.6,
//! §0.7.1): the `{good_length, max_lazy, nice_length, max_chain}` tuple dictates
//! the lazy-match thresholds, the hash-chain search depth, and the greedy/lazy
//! match selection. **Any** deviation — even by one — changes which matches the
//! encoder picks and therefore changes the emitted bitstream, breaking
//! compatibility with canonical C zlib. The numbers below are transcribed
//! character-for-character from the C source and are guarded by a verbatim
//! audit test (the in-module `config_table_is_verbatim_c_configuration_table`).
//!
//! # What this module does *not* do
//!
//! Selecting *which* compression routine runs for a given level/strategy —
//! `deflate_stored` for level `0`, `deflate_fast` for levels `1`..=`3`,
//! `deflate_slow` for levels `4`..=`9`, and `deflate_huff` / `deflate_rle`
//! for the `Z_HUFFMAN_ONLY` / `Z_RLE` strategies — is performed by the
//! `deflate()` driver, mirroring the dispatch ternary in `deflate.c`
//! (L1217-1220). That selection is **not** owned by this module: it depends on
//! the live per-call I/O context and the header-emission state machine, which
//! belong to the deflate engine driver.
//!
//! Consequently — exactly as the C `configuration_table` is declared `local`
//! (file-private) and exactly as [`DeflateState::lm_init`] documents — this
//! module owns only the numeric search-parameter table. [`CompressionConfig`]
//! carries only the four `ush` parameters from the C `config` struct and omits
//! the `compress_func` pointer; the table is `pub(crate)` to match the C
//! `local` linkage. `lm_init` copies the four parameters for the active level
//! into the live engine state ([`max_lazy_match`], [`good_match`],
//! [`nice_match`], [`max_chain_length`]).
//!
//! ```text
//!         good lazy nice chain
//! /* 0 */ {0,    0,  0,    0}   store only           (deflate_stored)
//! /* 1 */ {4,    4,  8,    4}   max speed, no lazy    (deflate_fast)
//! /* 2 */ {4,    5, 16,    8}                         (deflate_fast)
//! /* 3 */ {4,    6, 32,   32}                         (deflate_fast)
//! /* 4 */ {4,    4, 16,   16}   lazy matches          (deflate_slow)
//! /* 5 */ {8,   16, 32,   32}                         (deflate_slow)
//! /* 6 */ {8,   16, 128, 128}   default level         (deflate_slow)
//! /* 7 */ {8,   32, 128, 256}                         (deflate_slow)
//! /* 8 */ {32, 128, 258, 1024}                        (deflate_slow)
//! /* 9 */ {32, 258, 258, 4096}  max compression       (deflate_slow)
//! ```
//!
//! [`DeflateState::lm_init`]: crate::deflate::state::DeflateState::lm_init
//! [`max_lazy_match`]: crate::deflate::state::DeflateState::max_lazy_match
//! [`good_match`]: crate::deflate::state::DeflateState::good_match
//! [`nice_match`]: crate::deflate::state::DeflateState::nice_match
//! [`max_chain_length`]: crate::deflate::state::DeflateState::max_chain_length

/// The LZ77 match-search parameters for a single compression level.
///
/// Mirrors the numeric members of the C `config` struct (`deflate.c`
/// L98-104):
///
/// ```text
/// typedef struct config_s {
///    ush good_length; /* reduce lazy search above this match length */
///    ush max_lazy;    /* do not perform lazy search above this match length */
///    ush nice_length; /* quit search above this match length */
///    ush max_chain;
///    compress_func func;   // <- selected by the deflate() driver, not stored here
/// } config;
/// ```
///
/// All four fields are `u16` to match the C `ush` declarations exactly; the
/// engine widens them to `usize` at the point of use in
/// [`DeflateState::lm_init`](crate::deflate::state::DeflateState::lm_init). The
/// struct is `Copy` so a level's configuration can be pulled out of
/// [`CONFIG_TABLE`] by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CompressionConfig {
    /// `good_length`: reduce the lazy-match search once the previous match is
    /// at least this long.
    ///
    /// Used by [`deflate_slow`](crate::deflate::slow::deflate_slow) (levels
    /// `4`..=`9`). Per the C note (`deflate.c` L128), it is **ignored** by
    /// [`deflate_fast`](crate::deflate::fast::deflate_fast) (levels `≤ 3`).
    pub good_length: u16,

    /// `max_lazy`: do not perform a lazy-match search above this match length.
    ///
    /// For [`deflate_fast`](crate::deflate::fast::deflate_fast) this field is
    /// overloaded as `max_insert_length` (the C
    /// `#define max_insert_length max_lazy_match`, `deflate.h` L186), so it has
    /// a different meaning at levels `≤ 3`. `lm_init` copies the raw value into
    /// `DeflateState::max_lazy_match`, which serves both roles.
    pub max_lazy: u16,

    /// `nice_length`: stop the match search as soon as a match at least this
    /// long is found (never exceeds `MAX_MATCH` = 258).
    pub nice_length: u16,

    /// `max_chain`: the maximum number of hash-chain links to follow when
    /// searching for the longest match.
    pub max_chain: u16,
}

/// The ten-level compression configuration table.
///
/// Indexed by the **resolved** compression level (`0`..=`9`).
/// `Z_DEFAULT_COMPRESSION` (`-1`) is mapped to level `6` by
/// [`resolve_level`](crate::constants::resolve_level) in the
/// `deflateInit2_` / `deflateParams` path *before* this table is ever indexed,
/// so the index is always in range.
///
/// The values reproduce `deflate.c`'s `configuration_table[10]` (L112-124)
/// verbatim. Named-field struct literals are used deliberately — rather than a
/// positional constructor — so a transposed column would be a visible,
/// reviewable error rather than a silent byte-level regression.
pub(crate) const CONFIG_TABLE: [CompressionConfig; 10] = [
    // /* 0 */ {0, 0, 0, 0, deflate_stored}  -- store only
    CompressionConfig {
        good_length: 0,
        max_lazy: 0,
        nice_length: 0,
        max_chain: 0,
    },
    // /* 1 */ {4, 4, 8, 4, deflate_fast}  -- max speed, no lazy matches
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 8,
        max_chain: 4,
    },
    // /* 2 */ {4, 5, 16, 8, deflate_fast}
    CompressionConfig {
        good_length: 4,
        max_lazy: 5,
        nice_length: 16,
        max_chain: 8,
    },
    // /* 3 */ {4, 6, 32, 32, deflate_fast}
    CompressionConfig {
        good_length: 4,
        max_lazy: 6,
        nice_length: 32,
        max_chain: 32,
    },
    // /* 4 */ {4, 4, 16, 16, deflate_slow}  -- lazy matches
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 16,
        max_chain: 16,
    },
    // /* 5 */ {8, 16, 32, 32, deflate_slow}
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 32,
        max_chain: 32,
    },
    // /* 6 */ {8, 16, 128, 128, deflate_slow}  -- default level
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 128,
        max_chain: 128,
    },
    // /* 7 */ {8, 32, 128, 256, deflate_slow}
    CompressionConfig {
        good_length: 8,
        max_lazy: 32,
        nice_length: 128,
        max_chain: 256,
    },
    // /* 8 */ {32, 128, 258, 1024, deflate_slow}
    CompressionConfig {
        good_length: 32,
        max_lazy: 128,
        nice_length: 258,
        max_chain: 1024,
    },
    // /* 9 */ {32, 258, 258, 4096, deflate_slow}  -- max compression
    CompressionConfig {
        good_length: 32,
        max_lazy: 258,
        nice_length: 258,
        max_chain: 4096,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim cross-check of every value against `deflate.c`
    /// `configuration_table[10]` (L112-124). This is the byte-exactness guard:
    /// if any number drifts, DEFLATE output stops matching C zlib, and this
    /// test fails loudly.
    #[test]
    fn config_table_is_verbatim_c_configuration_table() {
        // (good_length, max_lazy, nice_length, max_chain) for levels 0..=9,
        // transcribed directly from deflate.c.
        const EXPECTED: [(u16, u16, u16, u16); 10] = [
            (0, 0, 0, 0),         // 0
            (4, 4, 8, 4),         // 1
            (4, 5, 16, 8),        // 2
            (4, 6, 32, 32),       // 3
            (4, 4, 16, 16),       // 4
            (8, 16, 32, 32),      // 5
            (8, 16, 128, 128),    // 6
            (8, 32, 128, 256),    // 7
            (32, 128, 258, 1024), // 8
            (32, 258, 258, 4096), // 9
        ];

        for (level, &(good, lazy, nice, chain)) in EXPECTED.iter().enumerate() {
            let cfg = CONFIG_TABLE[level];
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
        }
    }

    /// The table must have exactly ten entries (levels `0`..=`9`); the driver
    /// relies on `CONFIG_TABLE[level]` being in range for every resolved level.
    #[test]
    fn config_table_has_ten_levels() {
        assert_eq!(CONFIG_TABLE.len(), 10);
    }

    /// Level `0` is the store-only sentinel: all four search parameters are
    /// zero. The actual "store, don't compress" behavior is realized by the
    /// `deflate()` driver dispatching to `deflate_stored`, not by these
    /// numbers — but the zero sentinel is part of the verbatim C contract.
    #[test]
    fn level_zero_is_the_store_only_sentinel() {
        assert_eq!(
            CONFIG_TABLE[0],
            CompressionConfig {
                good_length: 0,
                max_lazy: 0,
                nice_length: 0,
                max_chain: 0
            }
        );
    }

    /// Sanity-checks the matching invariants the C source documents at
    /// `deflate.c` L127-129: for every level that actually performs matching
    /// (`1`..=`9`), `max_lazy >= MIN_MATCH` and `max_chain >= 4`. Level `0`
    /// (store only) is exempt because it never runs the match finder.
    #[test]
    fn config_table_respects_c_matching_invariants() {
        const MIN_MATCH: u16 = 3; // crate::deflate::state::MIN_MATCH
        const MAX_MATCH: u16 = 258; // crate::deflate::state::MAX_MATCH

        // Skip level 0 (store only): it never runs the match finder, so the
        // `max_lazy >= MIN_MATCH` / `max_chain >= 4` requirement does not apply.
        for (level, cfg) in CONFIG_TABLE.iter().enumerate().skip(1) {
            assert!(
                cfg.max_lazy >= MIN_MATCH,
                "level {level}: max_lazy ({}) must be >= MIN_MATCH ({MIN_MATCH})",
                cfg.max_lazy
            );
            assert!(
                cfg.max_chain >= 4,
                "level {level}: max_chain ({}) must be >= 4",
                cfg.max_chain
            );
        }

        // nice_length must never promise a match longer than MAX_MATCH for any
        // level (the match finder caps matches at MAX_MATCH).
        for (level, cfg) in CONFIG_TABLE.iter().enumerate() {
            assert!(
                cfg.nice_length <= MAX_MATCH,
                "level {level}: nice_length ({}) must be <= MAX_MATCH ({MAX_MATCH})",
                cfg.nice_length
            );
        }
    }
}
