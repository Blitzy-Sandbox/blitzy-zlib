// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Compression configuration table and strategy dispatch.
// Port of C `configuration_table`, `compress_func`, and RANK macro
// from deflate.c lines 63–134.

#![allow(dead_code)]

//! Compression configuration table and strategy dispatch for the DEFLATE engine.
//!
//! This module defines:
//!
//! - [`CompressionConfig`] — configuration parameters for a compression level
//!   (good_length, max_lazy, nice_length, max_chain, strategy).
//! - [`CompressionStrategy`] — an enum replacing the C function pointer
//!   `compress_func` with type-safe dispatch to one of five strategy functions:
//!   stored, fast, slow, Huffman-only, or run-length encoding.
//! - [`CONFIG_TABLE`] — a const array of 10 entries (levels 0–9), directly
//!   ported from C `configuration_table` in deflate.c lines 112–124.
//! - [`flush_rank`] — port of the C `RANK(f)` macro for ordering flush values.
//!
//! # C Source Reference
//!
//! - **C struct:** `config` (deflate.c lines 98–104)
//! - **C table:** `configuration_table[10]` (deflate.c lines 112–124)
//! - **C macro:** `RANK(f) = ((f) * 2) - ((f) > 4 ? 9 : 0)` (deflate.c line 133)
//! - **C typedef:** `compress_func = block_state (*)(deflate_state *, int)` (deflate.c line 70)
//!
//! # Strategy Pattern (Enum Dispatch)
//!
//! The C implementation uses a function pointer (`compress_func`) stored in
//! each entry of `configuration_table` to dispatch to the correct compression
//! function for a given level. In Rust, this is replaced by the
//! [`CompressionStrategy`] enum with a [`compress()`](CompressionStrategy::compress)
//! method that uses `match` dispatch — eliminating the need for function
//! pointers while retaining the same runtime behavior.
//!
//! # Wire-Format Compatibility
//!
//! The CONFIG_TABLE values are identical to the C original. Given the same
//! compression level, strategy, and input, the Rust implementation will
//! produce byte-identical DEFLATE output to C zlib.

use crate::deflate::state::{BlockState, DeflateState};
use crate::deflate::fast::deflate_fast;
use crate::deflate::slow::deflate_slow;
use crate::deflate::stored::deflate_stored;
use crate::deflate::huff::deflate_huff;
use crate::deflate::rle::deflate_rle;

// ===========================================================================
// CompressionStrategy — replaces C function pointer `compress_func`
// ===========================================================================

/// Compression strategy function selector.
///
/// Replaces the C function pointer `compress_func` dispatch via
/// `configuration_table[level].func`. Each variant maps to exactly one
/// of the five compression functions defined in sibling modules.
///
/// Per AAP §0.4.3: *"Strategy Pattern (Enum Dispatch): The 5 compression
/// functions selected via `configuration_table[level].func` will be replaced
/// by a `CompressionStrategy` enum with a `compress_block` method that
/// dispatches to the appropriate implementation."*
///
/// # Variants
///
/// | Variant   | C Function       | Levels  | Description                          |
/// |-----------|------------------|---------|--------------------------------------|
/// | `Stored`  | `deflate_stored` | 0       | No compression, stored blocks only   |
/// | `Fast`    | `deflate_fast`   | 1–3     | Greedy matching, no lazy evaluation  |
/// | `Slow`    | `deflate_slow`   | 4–9     | Lazy matching for better compression |
/// | `Huff`    | `deflate_huff`   | (any)   | Huffman-only, no LZ77 matching       |
/// | `Rle`     | `deflate_rle`    | (any)   | Run-length encoding (distance=1)     |
///
/// `Huff` and `Rle` are used when the caller explicitly selects
/// `Z_HUFFMAN_ONLY` or `Z_RLE` strategy via `deflateInit2` or
/// `deflateParams`, overriding the level-based default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressionStrategy {
    /// Level 0: No compression, stored blocks only.
    ///
    /// Data is passed through in stored blocks with 5-byte headers per
    /// RFC 1951 §3.2.4. No LZ77 matching or Huffman coding is performed.
    Stored,

    /// Levels 1–3: Greedy matching, no lazy evaluation.
    ///
    /// At each position, the longest match found is immediately emitted.
    /// Fastest LZ77-based strategy but produces larger output than `Slow`.
    Fast,

    /// Levels 4–9: Lazy matching for better compression.
    ///
    /// After finding a match at position P, the algorithm checks position
    /// P+1 for a longer match before committing. Slower but produces
    /// smaller output than `Fast`.
    Slow,

    /// Z_HUFFMAN_ONLY: Huffman encoding without LZ77 matching.
    ///
    /// Every input byte is emitted as a literal using dynamically
    /// constructed Huffman codes. No back-references are generated. Useful
    /// when the caller has already performed redundancy elimination.
    Huff,

    /// Z_RLE: Run-length encoding (distance 1 matches only).
    ///
    /// Only searches for runs of identical consecutive bytes (distance=1).
    /// No hash table maintenance is required. Highly efficient for data
    /// with many repeated bytes.
    Rle,
}

impl CompressionStrategy {
    /// Execute the compression strategy function for the given state and
    /// flush mode, returning the resulting block state.
    ///
    /// This replaces the C pattern:
    /// ```c
    /// configuration_table[s->level].func(s, flush)
    /// ```
    ///
    /// Each arm dispatches to the corresponding module-level function:
    ///
    /// | Variant  | Dispatches to                        |
    /// |----------|--------------------------------------|
    /// | Stored   | [`deflate_stored`](super::stored::deflate_stored) |
    /// | Fast     | [`deflate_fast`](super::fast::deflate_fast)       |
    /// | Slow     | [`deflate_slow`](super::slow::deflate_slow)       |
    /// | Huff     | [`deflate_huff`](super::huff::deflate_huff)       |
    /// | Rle      | [`deflate_rle`](super::rle::deflate_rle)          |
    ///
    /// # Parameters
    ///
    /// - `self` — the strategy variant to execute.
    /// - `state` — mutable reference to the compression state containing the
    ///   sliding window, hash tables, match state, and pending output buffer.
    /// - `flush` — flush mode as a raw integer (`Z_NO_FLUSH` through
    ///   `Z_TREES`). The strategy function uses this to decide whether to
    ///   flush output blocks or wait for more input.
    ///
    /// # Returns
    ///
    /// A [`BlockState`] indicating the result of the compression step:
    ///
    /// - `NeedMore` — block not completed, need more input or output space.
    /// - `BlockDone` — block flush performed.
    /// - `FinishStarted` — finish started, need only more output at next call.
    /// - `FinishDone` — finish done, accept no more input or output.
    #[inline]
    pub(crate) fn compress(self, state: &mut DeflateState, flush: i32) -> BlockState {
        match self {
            CompressionStrategy::Stored => deflate_stored(state, flush),
            CompressionStrategy::Fast => deflate_fast(state, flush),
            CompressionStrategy::Slow => deflate_slow(state, flush),
            CompressionStrategy::Huff => deflate_huff(state, flush),
            CompressionStrategy::Rle => deflate_rle(state, flush),
        }
    }
}

// ===========================================================================
// CompressionConfig — replaces C `config` struct
// ===========================================================================

/// Configuration parameters for a single compression level.
///
/// Maps to the C `config` struct (deflate.c lines 98–104):
///
/// ```c
/// typedef struct config_s {
///    ush good_length; /* reduce lazy search above this match length */
///    ush max_lazy;    /* do not perform lazy search above this match length */
///    ush nice_length; /* quit search above this match length */
///    ush max_chain;
///    compress_func func;
/// } config;
/// ```
///
/// The `func` field (C function pointer) is replaced by [`strategy`](Self::strategy),
/// a [`CompressionStrategy`] enum variant.
///
/// # Field Semantics
///
/// These values control the speed/compression tradeoff at each level:
///
/// - **`good_length`:** If the previous match is at least this long,
///   halve the maximum hash chain search length. This speeds up
///   compression for data that already has long matches. Used by
///   `deflate_slow`; ignored by `deflate_fast`.
///
/// - **`max_lazy`:** Do not attempt lazy match evaluation if the
///   current match is at least this long (it's already "good enough").
///   For levels 1–3, this value is repurposed as `max_insert_length`
///   (via C `#define max_insert_length max_lazy_match`): strings are
///   only inserted into the hash table if the match length does not
///   exceed this threshold.
///
/// - **`nice_length`:** Stop searching the hash chain immediately if a
///   match of at least this length is found. Limits worst-case search
///   time. Capped to `lookahead` at runtime.
///
/// - **`max_chain`:** Maximum number of hash chain entries to examine
///   before giving up. Higher values yield better compression at the
///   cost of speed.
///
/// - **`strategy`:** The compression function to use for this level.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompressionConfig {
    /// Reduce lazy search above this match length.
    ///
    /// When the previous match is at least `good_length` bytes long,
    /// the maximum chain length is halved to speed up the search.
    /// This field only affects `deflate_slow` (levels 4–9).
    pub good_length: u16,

    /// Do not perform lazy search above this match length.
    ///
    /// For `deflate_slow` (levels 4–9): if the current match is at
    /// least `max_lazy` bytes, skip lazy evaluation — the match is
    /// "good enough."
    ///
    /// For `deflate_fast` (levels 1–3): repurposed as
    /// `max_insert_length` — new strings are only inserted into the
    /// hash table when `match_length <= max_lazy`.
    pub max_lazy: u16,

    /// Quit search above this match length.
    ///
    /// If a match of at least `nice_length` bytes is found during hash
    /// chain traversal, the search is terminated early. This bounds the
    /// worst-case search time while allowing longer chains to be
    /// explored for shorter matches.
    pub nice_length: u16,

    /// Maximum hash chain length to search.
    ///
    /// Higher values improve compression ratio at the cost of CPU time.
    /// Level 9 uses 4096 entries; level 1 uses only 4.
    pub max_chain: u16,

    /// Compression strategy function to use for this level.
    ///
    /// Replaces the C `compress_func` function pointer with a type-safe
    /// enum variant that dispatches via `CompressionStrategy::compress()`.
    pub strategy: CompressionStrategy,
}

// ===========================================================================
// CONFIG_TABLE — replaces C `configuration_table[10]`
// ===========================================================================

/// Configuration table for compression levels 0–9.
///
/// Each entry specifies the match search parameters and compression
/// strategy function for a single compression level. The values are
/// directly ported from C `configuration_table` (deflate.c lines 112–124):
///
/// ```text
/// Level  good  lazy  nice  chain  strategy
///   0      0     0     0      0   Stored
///   1      4     4     8      4   Fast
///   2      4     5    16      8   Fast
///   3      4     6    32     32   Fast
///   4      4     4    16     16   Slow
///   5      8    16    32     32   Slow
///   6      8    16   128    128   Slow    (default level)
///   7      8    32   128    256   Slow
///   8     32   128   258   1024   Slow
///   9     32   258   258   4096   Slow    (max compression)
/// ```
///
/// # Invariants preserved from C
///
/// - `max_lazy >= MIN_MATCH` for all levels (required by `deflate()`)
/// - `max_chain >= 4` for all levels (minimum meaningful chain length)
/// - `nice_length <= MAX_MATCH` (258 bytes, the DEFLATE maximum)
/// - Level 0 → `Stored`, Levels 1–3 → `Fast`, Levels 4–9 → `Slow`
///
/// # Note on level 0
///
/// Level 0 has all zero parameters because `deflate_stored` does not
/// perform any LZ77 matching — it simply passes data through in stored
/// blocks. The zero values prevent any accidental match search.
pub(crate) const CONFIG_TABLE: [CompressionConfig; 10] = [
    // Level 0: store only (no compression)
    CompressionConfig {
        good_length: 0,
        max_lazy: 0,
        nice_length: 0,
        max_chain: 0,
        strategy: CompressionStrategy::Stored,
    },
    // Level 1: maximum speed, no lazy matches
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 8,
        max_chain: 4,
        strategy: CompressionStrategy::Fast,
    },
    // Level 2
    CompressionConfig {
        good_length: 4,
        max_lazy: 5,
        nice_length: 16,
        max_chain: 8,
        strategy: CompressionStrategy::Fast,
    },
    // Level 3
    CompressionConfig {
        good_length: 4,
        max_lazy: 6,
        nice_length: 32,
        max_chain: 32,
        strategy: CompressionStrategy::Fast,
    },
    // Level 4: lazy matches begin
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 16,
        max_chain: 16,
        strategy: CompressionStrategy::Slow,
    },
    // Level 5
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 32,
        max_chain: 32,
        strategy: CompressionStrategy::Slow,
    },
    // Level 6: default compression level
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 128,
        max_chain: 128,
        strategy: CompressionStrategy::Slow,
    },
    // Level 7
    CompressionConfig {
        good_length: 8,
        max_lazy: 32,
        nice_length: 128,
        max_chain: 256,
        strategy: CompressionStrategy::Slow,
    },
    // Level 8
    CompressionConfig {
        good_length: 32,
        max_lazy: 128,
        nice_length: 258,
        max_chain: 1024,
        strategy: CompressionStrategy::Slow,
    },
    // Level 9: maximum compression
    CompressionConfig {
        good_length: 32,
        max_lazy: 258,
        nice_length: 258,
        max_chain: 4096,
        strategy: CompressionStrategy::Slow,
    },
];

// ===========================================================================
// flush_rank — replaces C RANK(f) macro
// ===========================================================================

/// Rank flush values for comparison, ordering `Z_BLOCK` between
/// `Z_NO_FLUSH` and `Z_PARTIAL_FLUSH`.
///
/// Port of the C `RANK(f)` macro (deflate.c line 133):
///
/// ```c
/// #define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))
/// ```
///
/// This mapping allows the main `deflate()` function to compare the
/// current flush mode against the previous flush mode using a simple
/// `>=` comparison on rank values, without needing special-case logic
/// for `Z_BLOCK` and `Z_TREES` which have non-sequential raw values.
///
/// # Rank mapping
///
/// | Flush constant | Raw value | Rank |
/// |----------------|-----------|------|
/// | `Z_NO_FLUSH`      | 0     | 0    |
/// | `Z_BLOCK`          | 5     | 1    |
/// | `Z_PARTIAL_FLUSH`  | 1     | 2    |
/// | `Z_TREES`          | 6     | 3    |
/// | `Z_SYNC_FLUSH`     | 2     | 4    |
/// | `Z_FULL_FLUSH`     | 3     | 6    |
/// | `Z_FINISH`         | 4     | 8    |
///
/// # Parameters
///
/// - `f` — flush mode as a raw integer (`Z_NO_FLUSH` through `Z_TREES`).
///
/// # Returns
///
/// Integer rank value used for flush comparison.
///
/// # Examples
///
/// ```rust
/// # use zlib_rs::deflate::strategy::flush_rank;
/// assert_eq!(flush_rank(0), 0);  // Z_NO_FLUSH
/// assert_eq!(flush_rank(5), 1);  // Z_BLOCK
/// assert_eq!(flush_rank(1), 2);  // Z_PARTIAL_FLUSH
/// assert_eq!(flush_rank(6), 3);  // Z_TREES
/// assert_eq!(flush_rank(2), 4);  // Z_SYNC_FLUSH
/// assert_eq!(flush_rank(3), 6);  // Z_FULL_FLUSH
/// assert_eq!(flush_rank(4), 8);  // Z_FINISH
/// ```
#[inline(always)]
pub(crate) fn flush_rank(f: i32) -> i32 {
    (f * 2) - if f > 4 { 9 } else { 0 }
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CONFIG_TABLE validation
    // -----------------------------------------------------------------------

    /// Verify CONFIG_TABLE has exactly 10 entries (levels 0–9).
    #[test]
    fn config_table_has_10_entries() {
        assert_eq!(CONFIG_TABLE.len(), 10);
    }

    /// Verify level 0 maps to Stored strategy with all-zero parameters.
    #[test]
    fn config_level_0_is_stored() {
        let c = &CONFIG_TABLE[0];
        assert_eq!(c.good_length, 0);
        assert_eq!(c.max_lazy, 0);
        assert_eq!(c.nice_length, 0);
        assert_eq!(c.max_chain, 0);
        assert_eq!(c.strategy, CompressionStrategy::Stored);
    }

    /// Verify levels 1–3 map to Fast strategy.
    #[test]
    fn config_levels_1_to_3_are_fast() {
        for level in 1..=3 {
            assert_eq!(
                CONFIG_TABLE[level].strategy,
                CompressionStrategy::Fast,
                "Level {} should use Fast strategy",
                level
            );
        }
    }

    /// Verify levels 4–9 map to Slow strategy.
    #[test]
    fn config_levels_4_to_9_are_slow() {
        for level in 4..=9 {
            assert_eq!(
                CONFIG_TABLE[level].strategy,
                CompressionStrategy::Slow,
                "Level {} should use Slow strategy",
                level
            );
        }
    }

    /// Verify exact values for each level match C `configuration_table`.
    #[test]
    fn config_table_exact_values() {
        // (good_length, max_lazy, nice_length, max_chain) from deflate.c
        let expected: [(u16, u16, u16, u16); 10] = [
            (0, 0, 0, 0),       // Level 0
            (4, 4, 8, 4),       // Level 1
            (4, 5, 16, 8),      // Level 2
            (4, 6, 32, 32),     // Level 3
            (4, 4, 16, 16),     // Level 4
            (8, 16, 32, 32),    // Level 5
            (8, 16, 128, 128),  // Level 6 (default)
            (8, 32, 128, 256),  // Level 7
            (32, 128, 258, 1024), // Level 8
            (32, 258, 258, 4096), // Level 9
        ];
        for (level, &(good, lazy, nice, chain)) in expected.iter().enumerate() {
            let c = &CONFIG_TABLE[level];
            assert_eq!(
                c.good_length, good,
                "Level {} good_length: expected {}, got {}",
                level, good, c.good_length
            );
            assert_eq!(
                c.max_lazy, lazy,
                "Level {} max_lazy: expected {}, got {}",
                level, lazy, c.max_lazy
            );
            assert_eq!(
                c.nice_length, nice,
                "Level {} nice_length: expected {}, got {}",
                level, nice, c.nice_length
            );
            assert_eq!(
                c.max_chain, chain,
                "Level {} max_chain: expected {}, got {}",
                level, chain, c.max_chain
            );
        }
    }

    /// Verify nice_length never exceeds MAX_MATCH (258).
    #[test]
    fn config_nice_length_within_max_match() {
        for (level, config) in CONFIG_TABLE.iter().enumerate() {
            assert!(
                config.nice_length <= 258,
                "Level {} nice_length {} exceeds MAX_MATCH (258)",
                level,
                config.nice_length
            );
        }
    }

    /// Verify max_chain is non-decreasing within each strategy group.
    ///
    /// The C configuration table resets parameters at the Fast→Slow
    /// boundary (level 3→4), so max_chain may decrease across that
    /// boundary. Within each strategy group the values are non-decreasing.
    #[test]
    fn config_max_chain_non_decreasing_within_strategy_groups() {
        // Fast group: levels 1–3
        for level in 2..=3 {
            assert!(
                CONFIG_TABLE[level].max_chain >= CONFIG_TABLE[level - 1].max_chain,
                "Fast group: max_chain at level {} ({}) < level {} ({})",
                level,
                CONFIG_TABLE[level].max_chain,
                level - 1,
                CONFIG_TABLE[level - 1].max_chain
            );
        }
        // Slow group: levels 4–9
        for level in 5..=9 {
            assert!(
                CONFIG_TABLE[level].max_chain >= CONFIG_TABLE[level - 1].max_chain,
                "Slow group: max_chain at level {} ({}) < level {} ({})",
                level,
                CONFIG_TABLE[level].max_chain,
                level - 1,
                CONFIG_TABLE[level - 1].max_chain
            );
        }
    }

    // -----------------------------------------------------------------------
    // flush_rank validation
    // -----------------------------------------------------------------------

    /// Verify flush_rank produces correct values for all 7 flush modes.
    #[test]
    fn flush_rank_all_modes() {
        // Z_NO_FLUSH(0)=0, Z_PARTIAL_FLUSH(1)=2, Z_SYNC_FLUSH(2)=4,
        // Z_FULL_FLUSH(3)=6, Z_FINISH(4)=8, Z_BLOCK(5)=1, Z_TREES(6)=3
        assert_eq!(flush_rank(0), 0, "Z_NO_FLUSH rank");
        assert_eq!(flush_rank(1), 2, "Z_PARTIAL_FLUSH rank");
        assert_eq!(flush_rank(2), 4, "Z_SYNC_FLUSH rank");
        assert_eq!(flush_rank(3), 6, "Z_FULL_FLUSH rank");
        assert_eq!(flush_rank(4), 8, "Z_FINISH rank");
        assert_eq!(flush_rank(5), 1, "Z_BLOCK rank");
        assert_eq!(flush_rank(6), 3, "Z_TREES rank");
    }

    /// Verify that BLOCK ranks between NO_FLUSH and PARTIAL_FLUSH.
    #[test]
    fn flush_rank_block_between_noflush_and_partial() {
        let no_flush = flush_rank(0);
        let block = flush_rank(5);
        let partial = flush_rank(1);
        assert!(
            no_flush < block && block < partial,
            "BLOCK rank ({}) should be between NO_FLUSH ({}) and PARTIAL_FLUSH ({})",
            block,
            no_flush,
            partial
        );
    }

    /// Verify TREES ranks between PARTIAL_FLUSH and SYNC_FLUSH.
    #[test]
    fn flush_rank_trees_between_partial_and_sync() {
        let partial = flush_rank(1);
        let trees = flush_rank(6);
        let sync = flush_rank(2);
        assert!(
            partial < trees && trees < sync,
            "TREES rank ({}) should be between PARTIAL_FLUSH ({}) and SYNC_FLUSH ({})",
            trees,
            partial,
            sync
        );
    }

    /// Verify flush rank ordering: NO_FLUSH < BLOCK < PARTIAL < TREES < SYNC < FULL < FINISH.
    #[test]
    fn flush_rank_ordering() {
        let ranks = [
            flush_rank(0), // NO_FLUSH = 0
            flush_rank(5), // BLOCK = 1
            flush_rank(1), // PARTIAL = 2
            flush_rank(6), // TREES = 3
            flush_rank(2), // SYNC = 4
            flush_rank(3), // FULL = 6
            flush_rank(4), // FINISH = 8
        ];
        for i in 0..ranks.len() - 1 {
            assert!(
                ranks[i] < ranks[i + 1],
                "Rank ordering violated at position {}: {} >= {}",
                i,
                ranks[i],
                ranks[i + 1]
            );
        }
    }

    // -----------------------------------------------------------------------
    // CompressionStrategy enum checks
    // -----------------------------------------------------------------------

    /// Verify all five strategy variants exist and are distinct.
    #[test]
    fn strategy_variants_are_distinct() {
        let variants = [
            CompressionStrategy::Stored,
            CompressionStrategy::Fast,
            CompressionStrategy::Slow,
            CompressionStrategy::Huff,
            CompressionStrategy::Rle,
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "Strategy variants {:?} and {:?} should be distinct",
                    variants[i], variants[j]
                );
            }
        }
    }

    /// Verify CompressionStrategy implements Clone, Copy, Debug, PartialEq, Eq.
    #[test]
    fn strategy_derives() {
        let s = CompressionStrategy::Fast;
        let s2 = s; // Copy
        let s3 = s.clone(); // Clone
        assert_eq!(s2, s3); // PartialEq + Eq
        let _ = format!("{:?}", s); // Debug
    }

    /// Verify CompressionConfig implements Clone, Copy, Debug.
    #[test]
    fn config_derives() {
        let c = CONFIG_TABLE[6];
        let c2 = c; // Copy
        let c3 = c.clone(); // Clone
        let _ = format!("{:?}", c2); // Debug
        let _ = format!("{:?}", c3); // Debug (suppress unused warning)
    }
}
