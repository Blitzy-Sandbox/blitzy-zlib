// Compression configuration table for the DEFLATE engine.
//
// Ported from `deflate.c` lines 93–133 of the zlib 1.3.2.1-motley C library.
// Maps compression levels 0–9 to tuning parameters that control the trade-off
// between compression speed and compression ratio.
//
// The configuration table drives three aspects of the deflate engine:
//   1. **Match search aggressiveness** — `good_length`, `max_lazy`, `nice_length`
//      control when the engine stops looking for longer matches.
//   2. **Hash chain traversal depth** — `max_chain` limits how many positions
//      in the hash chain are inspected per match attempt.
//   3. **Compression function selection** — `func` selects between stored
//      (no compression), fast (no lazy evaluation), and slow (lazy evaluation)
//      compression strategies.
//
// # Binary Compatibility
//
// The values in `CONFIGURATION_TABLE` are copied verbatim from the C source.
// Altering any value will produce different compressed output and break
// byte-for-byte compatibility with reference C zlib.

/// Compression strategy function selector.
///
/// Determines which core compression algorithm is used for a given
/// compression level. This replaces the C `compress_func` function pointer
/// in the `config` struct from `deflate.c` line 103.
///
/// Note: `deflate_rle` and `deflate_huff` are **not** represented here.
/// Those strategies are selected at runtime via the `strategy` parameter
/// (`Z_RLE` or `Z_HUFFMAN_ONLY`) and bypass the configuration table entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionFunc {
    /// Level 0: store blocks verbatim with no compression (`deflate_stored`).
    Stored,
    /// Levels 1–3: fast compression without lazy match evaluation (`deflate_fast`).
    Fast,
    /// Levels 4–9: best compression with lazy match evaluation (`deflate_slow`).
    Slow,
}

/// Configuration parameters for a single compression level.
///
/// Each entry in [`CONFIGURATION_TABLE`] is an instance of this struct,
/// mapping a compression level (0–9) to the tuning knobs that the deflate
/// engine uses during LZ77 matching.
///
/// Corresponds to the C `config` struct defined in `deflate.c` lines 98–104:
///
/// ```c
/// typedef struct config_s {
///     ush good_length;
///     ush max_lazy;
///     ush nice_length;
///     ush max_chain;
///     compress_func func;
/// } config;
/// ```
#[derive(Debug, Clone, Copy)]
pub struct CompressionConfig {
    /// Reduce lazy search above this match length.
    ///
    /// When a match of at least `good_length` bytes is found, the engine
    /// halves the hash chain traversal depth on the assumption that
    /// a sufficiently good match has already been located.
    ///
    /// For `deflate_fast` (levels 1–3) this field is not used directly.
    pub good_length: u16,

    /// Do not perform lazy search above this match length.
    ///
    /// For `deflate_slow` (levels 4–9): if the current match length exceeds
    /// `max_lazy`, the engine outputs the match immediately without attempting
    /// to find a longer one at the next position.
    ///
    /// For `deflate_fast` (levels 1–3): this field has a different semantic —
    /// it controls the maximum match length that still triggers hash insertion
    /// for the matched bytes (see `max_insert_length` in the deflate state).
    pub max_lazy: u16,

    /// Quit search when a match of at least `nice_length` bytes is found.
    ///
    /// Once a match reaches this threshold during hash chain traversal,
    /// the engine immediately accepts it without inspecting further chain
    /// entries. Higher values yield better compression at the cost of speed.
    pub nice_length: u16,

    /// Maximum number of hash chain entries to traverse per match attempt.
    ///
    /// Longer chains increase the probability of finding a better match
    /// but cost more CPU time. This value is halved at runtime when the
    /// current best match already exceeds `good_length`.
    pub max_chain: u16,

    /// The compression function to dispatch for this level.
    ///
    /// Selects between [`CompressionFunc::Stored`] (level 0),
    /// [`CompressionFunc::Fast`] (levels 1–3), and
    /// [`CompressionFunc::Slow`] (levels 4–9).
    pub func: CompressionFunc,
}

/// Configuration table mapping compression levels 0–9 to their parameters.
///
/// This is the non-`FASTEST` path from `deflate.c` lines 112–124.
/// The table always contains all 10 levels; the `FASTEST` two-entry
/// shortcut from the C preprocessor is not needed in Rust.
///
/// # Invariants (from `deflate.c` lines 127–130)
///
/// - `max_lazy >= MIN_MATCH` for all levels where matching is performed.
/// - `max_chain >= 4` for all levels where hash chains are traversed.
///
/// # Level-to-Parameter Mapping
///
/// | Level | good | lazy | nice | chain | Function |
/// |-------|------|------|------|-------|----------|
/// |   0   |   0  |    0 |    0 |     0 | Stored   |
/// |   1   |   4  |    4 |    8 |     4 | Fast     |
/// |   2   |   4  |    5 |   16 |     8 | Fast     |
/// |   3   |   4  |    6 |   32 |    32 | Fast     |
/// |   4   |   4  |    4 |   16 |    16 | Slow     |
/// |   5   |   8  |   16 |   32 |    32 | Slow     |
/// |   6   |   8  |   16 |  128 |   128 | Slow     |
/// |   7   |   8  |   32 |  128 |   256 | Slow     |
/// |   8   |  32  |  128 |  258 |  1024 | Slow     |
/// |   9   |  32  |  258 |  258 |  4096 | Slow     |
///
/// Level 6 is the target of `Z_DEFAULT_COMPRESSION` (mapped before lookup).
pub const CONFIGURATION_TABLE: [CompressionConfig; 10] = [
    // Level 0: store only — no compression at all.
    CompressionConfig {
        good_length: 0,
        max_lazy: 0,
        nice_length: 0,
        max_chain: 0,
        func: CompressionFunc::Stored,
    },
    // Level 1: maximum speed, no lazy matches.
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 8,
        max_chain: 4,
        func: CompressionFunc::Fast,
    },
    // Level 2.
    CompressionConfig {
        good_length: 4,
        max_lazy: 5,
        nice_length: 16,
        max_chain: 8,
        func: CompressionFunc::Fast,
    },
    // Level 3.
    CompressionConfig {
        good_length: 4,
        max_lazy: 6,
        nice_length: 32,
        max_chain: 32,
        func: CompressionFunc::Fast,
    },
    // Level 4: lazy matches start here.
    CompressionConfig {
        good_length: 4,
        max_lazy: 4,
        nice_length: 16,
        max_chain: 16,
        func: CompressionFunc::Slow,
    },
    // Level 5.
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 32,
        max_chain: 32,
        func: CompressionFunc::Slow,
    },
    // Level 6 — default compression level (`Z_DEFAULT_COMPRESSION` maps here).
    CompressionConfig {
        good_length: 8,
        max_lazy: 16,
        nice_length: 128,
        max_chain: 128,
        func: CompressionFunc::Slow,
    },
    // Level 7.
    CompressionConfig {
        good_length: 8,
        max_lazy: 32,
        nice_length: 128,
        max_chain: 256,
        func: CompressionFunc::Slow,
    },
    // Level 8.
    CompressionConfig {
        good_length: 32,
        max_lazy: 128,
        nice_length: 258,
        max_chain: 1024,
        func: CompressionFunc::Slow,
    },
    // Level 9: maximum compression.
    CompressionConfig {
        good_length: 32,
        max_lazy: 258,
        nice_length: 258,
        max_chain: 4096,
        func: CompressionFunc::Slow,
    },
];

/// Returns a reference to the [`CompressionConfig`] for the given compression
/// level.
///
/// # Arguments
///
/// * `level` — A compression level in the range `0..=9`. The caller is
///   responsible for mapping `Z_DEFAULT_COMPRESSION` (`-1`) to level `6`
///   **before** invoking this function.
///
/// # Returns
///
/// A `&'static CompressionConfig` if `level` is in range, or `None` if the
/// level is outside the valid `0..=9` range.
///
/// # Examples
///
/// ```ignore
/// let cfg = get_config(6).expect("valid level");
/// assert_eq!(cfg.nice_length, 128);
/// ```
#[must_use]
pub fn get_config(level: i32) -> Option<&'static CompressionConfig> {
    if !(0..=9).contains(&level) {
        return None;
    }
    // SAFETY of cast: level is validated to be in 0..=9 above, so it is
    // non-negative and fits in usize on all supported platforms.
    #[allow(clippy::cast_sign_loss)]
    let index = level as usize;
    Some(&CONFIGURATION_TABLE[index])
}

/// Rank a flush value for ordered comparison.
///
/// Produces a ranking that places `Z_BLOCK` (value 5) between `Z_NO_FLUSH`
/// (value 0) and `Z_PARTIAL_FLUSH` (value 1), enabling the deflate engine
/// to correctly compare flush urgency.
///
/// Ported from the C `RANK` macro in `deflate.c` line 133:
///
/// ```c
/// #define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))
/// ```
///
/// # Ranking Table
///
/// | Flush mode        | Value (`f`) | `rank(f)` |
/// |-------------------|-------------|-----------|
/// | `Z_NO_FLUSH`      |           0 |         0 |
/// | `Z_PARTIAL_FLUSH`  |           1 |         2 |
/// | `Z_SYNC_FLUSH`    |           2 |         4 |
/// | `Z_FULL_FLUSH`    |           3 |         6 |
/// | `Z_FINISH`        |           4 |         8 |
/// | `Z_BLOCK`         |           5 |         1 |
/// | `Z_TREES`         |           6 |         3 |
///
/// The key insight is that `Z_BLOCK` (5) maps to rank 1, which sits between
/// `Z_NO_FLUSH` (rank 0) and `Z_PARTIAL_FLUSH` (rank 2). This allows the
/// deflate state machine to treat `Z_BLOCK` as "more urgent than no-flush
/// but less urgent than a partial flush".
#[inline]
#[must_use]
pub fn rank(f: i32) -> i32 {
    f * 2 - if f > 4 { 9 } else { 0 }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CompressionFunc enum tests
    // -----------------------------------------------------------------------

    #[test]
    fn compression_func_has_three_variants() {
        let variants = [
            CompressionFunc::Stored,
            CompressionFunc::Fast,
            CompressionFunc::Slow,
        ];
        assert_eq!(variants.len(), 3);
    }

    #[test]
    fn compression_func_equality() {
        assert_eq!(CompressionFunc::Stored, CompressionFunc::Stored);
        assert_eq!(CompressionFunc::Fast, CompressionFunc::Fast);
        assert_eq!(CompressionFunc::Slow, CompressionFunc::Slow);
        assert_ne!(CompressionFunc::Stored, CompressionFunc::Fast);
        assert_ne!(CompressionFunc::Fast, CompressionFunc::Slow);
        assert_ne!(CompressionFunc::Stored, CompressionFunc::Slow);
    }

    #[test]
    fn compression_func_is_copy_clone_debug() {
        let f = CompressionFunc::Fast;
        let f2 = f; // Copy
        let f3 = f; // Copy (implements Copy)
        assert_eq!(f, f2);
        assert_eq!(f, f3);
        // Debug
        let debug_str = format!("{f:?}");
        assert!(!debug_str.is_empty());
    }

    // -----------------------------------------------------------------------
    // CompressionConfig struct tests
    // -----------------------------------------------------------------------

    #[test]
    fn compression_config_has_five_fields() {
        let cfg = CompressionConfig {
            good_length: 1,
            max_lazy: 2,
            nice_length: 3,
            max_chain: 4,
            func: CompressionFunc::Fast,
        };
        assert_eq!(cfg.good_length, 1);
        assert_eq!(cfg.max_lazy, 2);
        assert_eq!(cfg.nice_length, 3);
        assert_eq!(cfg.max_chain, 4);
        assert_eq!(cfg.func, CompressionFunc::Fast);
    }

    #[test]
    fn compression_config_is_copy_clone_debug() {
        let cfg = CONFIGURATION_TABLE[6];
        let cfg2 = cfg; // Copy
        let cfg3 = cfg; // Copy (implements Copy)
        assert_eq!(cfg.good_length, cfg2.good_length);
        assert_eq!(cfg.good_length, cfg3.good_length);
        let debug_str = format!("{cfg:?}");
        assert!(!debug_str.is_empty());
    }

    // -----------------------------------------------------------------------
    // CONFIGURATION_TABLE value-by-value verification
    // -----------------------------------------------------------------------

    #[test]
    fn table_has_ten_entries() {
        assert_eq!(CONFIGURATION_TABLE.len(), 10);
    }

    #[test]
    fn table_level_0_store_only() {
        let c = &CONFIGURATION_TABLE[0];
        assert_eq!(c.good_length, 0);
        assert_eq!(c.max_lazy, 0);
        assert_eq!(c.nice_length, 0);
        assert_eq!(c.max_chain, 0);
        assert_eq!(c.func, CompressionFunc::Stored);
    }

    #[test]
    fn table_level_1() {
        let c = &CONFIGURATION_TABLE[1];
        assert_eq!(c.good_length, 4);
        assert_eq!(c.max_lazy, 4);
        assert_eq!(c.nice_length, 8);
        assert_eq!(c.max_chain, 4);
        assert_eq!(c.func, CompressionFunc::Fast);
    }

    #[test]
    fn table_level_2() {
        let c = &CONFIGURATION_TABLE[2];
        assert_eq!(c.good_length, 4);
        assert_eq!(c.max_lazy, 5);
        assert_eq!(c.nice_length, 16);
        assert_eq!(c.max_chain, 8);
        assert_eq!(c.func, CompressionFunc::Fast);
    }

    #[test]
    fn table_level_3() {
        let c = &CONFIGURATION_TABLE[3];
        assert_eq!(c.good_length, 4);
        assert_eq!(c.max_lazy, 6);
        assert_eq!(c.nice_length, 32);
        assert_eq!(c.max_chain, 32);
        assert_eq!(c.func, CompressionFunc::Fast);
    }

    #[test]
    fn table_level_4_lazy_start() {
        let c = &CONFIGURATION_TABLE[4];
        assert_eq!(c.good_length, 4);
        assert_eq!(c.max_lazy, 4);
        assert_eq!(c.nice_length, 16);
        assert_eq!(c.max_chain, 16);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_level_5() {
        let c = &CONFIGURATION_TABLE[5];
        assert_eq!(c.good_length, 8);
        assert_eq!(c.max_lazy, 16);
        assert_eq!(c.nice_length, 32);
        assert_eq!(c.max_chain, 32);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_level_6_default_compression() {
        let c = &CONFIGURATION_TABLE[6];
        assert_eq!(c.good_length, 8);
        assert_eq!(c.max_lazy, 16);
        assert_eq!(c.nice_length, 128);
        assert_eq!(c.max_chain, 128);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_level_7() {
        let c = &CONFIGURATION_TABLE[7];
        assert_eq!(c.good_length, 8);
        assert_eq!(c.max_lazy, 32);
        assert_eq!(c.nice_length, 128);
        assert_eq!(c.max_chain, 256);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_level_8() {
        let c = &CONFIGURATION_TABLE[8];
        assert_eq!(c.good_length, 32);
        assert_eq!(c.max_lazy, 128);
        assert_eq!(c.nice_length, 258);
        assert_eq!(c.max_chain, 1024);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_level_9_max_compression() {
        let c = &CONFIGURATION_TABLE[9];
        assert_eq!(c.good_length, 32);
        assert_eq!(c.max_lazy, 258);
        assert_eq!(c.nice_length, 258);
        assert_eq!(c.max_chain, 4096);
        assert_eq!(c.func, CompressionFunc::Slow);
    }

    #[test]
    fn table_levels_0_to_3_use_fast_or_stored() {
        assert_eq!(CONFIGURATION_TABLE[0].func, CompressionFunc::Stored);
        for level in 1..=3 {
            assert_eq!(
                CONFIGURATION_TABLE[level].func,
                CompressionFunc::Fast,
                "Level {level} should use Fast"
            );
        }
    }

    #[test]
    fn table_levels_4_to_9_use_slow() {
        for level in 4..=9 {
            assert_eq!(
                CONFIGURATION_TABLE[level].func,
                CompressionFunc::Slow,
                "Level {level} should use Slow"
            );
        }
    }

    // -----------------------------------------------------------------------
    // get_config() tests
    // -----------------------------------------------------------------------

    #[test]
    fn get_config_valid_levels() {
        for level in 0..=9 {
            let cfg = get_config(level);
            assert!(cfg.is_some(), "get_config({level}) should return Some");
            let cfg = cfg.unwrap();
            assert_eq!(
                cfg.good_length,
                CONFIGURATION_TABLE[level as usize].good_length
            );
            assert_eq!(cfg.max_lazy, CONFIGURATION_TABLE[level as usize].max_lazy);
            assert_eq!(
                cfg.nice_length,
                CONFIGURATION_TABLE[level as usize].nice_length
            );
            assert_eq!(cfg.max_chain, CONFIGURATION_TABLE[level as usize].max_chain);
            assert_eq!(cfg.func, CONFIGURATION_TABLE[level as usize].func);
        }
    }

    #[test]
    fn get_config_out_of_range_returns_none() {
        assert!(get_config(-2).is_none());
        assert!(get_config(-1).is_none());
        assert!(get_config(10).is_none());
        assert!(get_config(100).is_none());
        assert!(get_config(i32::MIN).is_none());
        assert!(get_config(i32::MAX).is_none());
    }

    #[test]
    fn get_config_returns_matching_values() {
        // Verify that the returned config matches the table entry.
        for level in 0..=9 {
            let cfg = get_config(level).unwrap();
            let expected = &CONFIGURATION_TABLE[level as usize];
            assert_eq!(
                cfg.good_length, expected.good_length,
                "good_length mismatch at level {level}"
            );
            assert_eq!(
                cfg.max_lazy, expected.max_lazy,
                "max_lazy mismatch at level {level}"
            );
            assert_eq!(
                cfg.nice_length, expected.nice_length,
                "nice_length mismatch at level {level}"
            );
            assert_eq!(
                cfg.max_chain, expected.max_chain,
                "max_chain mismatch at level {level}"
            );
            assert_eq!(cfg.func, expected.func, "func mismatch at level {level}");
        }
    }

    // -----------------------------------------------------------------------
    // rank() tests
    // -----------------------------------------------------------------------

    #[test]
    fn rank_known_flush_values() {
        // Z_NO_FLUSH = 0
        assert_eq!(rank(0), 0);
        // Z_PARTIAL_FLUSH = 1
        assert_eq!(rank(1), 2);
        // Z_SYNC_FLUSH = 2
        assert_eq!(rank(2), 4);
        // Z_FULL_FLUSH = 3
        assert_eq!(rank(3), 6);
        // Z_FINISH = 4
        assert_eq!(rank(4), 8);
        // Z_BLOCK = 5
        assert_eq!(rank(5), 1);
        // Z_TREES = 6
        assert_eq!(rank(6), 3);
    }

    #[test]
    fn rank_z_block_between_no_flush_and_partial_flush() {
        // The primary purpose of RANK: Z_BLOCK sits between Z_NO_FLUSH and
        // Z_PARTIAL_FLUSH in the ordering.
        assert!(
            rank(5) > rank(0),
            "Z_BLOCK should rank higher than Z_NO_FLUSH"
        );
        assert!(
            rank(5) < rank(1),
            "Z_BLOCK should rank lower than Z_PARTIAL_FLUSH"
        );
    }

    #[test]
    fn rank_z_trees_between_partial_and_sync() {
        assert!(
            rank(6) > rank(1),
            "Z_TREES should rank higher than Z_PARTIAL_FLUSH"
        );
        assert!(
            rank(6) < rank(2),
            "Z_TREES should rank lower than Z_SYNC_FLUSH"
        );
    }

    #[test]
    fn rank_ordering_is_monotonic_for_0_to_4() {
        // For the original flush values 0–4, ranking should be strictly increasing.
        for f in 0..4 {
            assert!(
                rank(f) < rank(f + 1),
                "rank({f}) should be less than rank({})",
                f + 1
            );
        }
    }

    #[test]
    fn rank_formula_matches_c_macro() {
        // Verify against the C RANK macro: ((f)*2) - ((f) > 4 ? 9 : 0)
        for f in -10..=20 {
            let expected = f * 2 - if f > 4 { 9 } else { 0 };
            assert_eq!(rank(f), expected, "rank({f}) should be {expected}");
        }
    }
}
