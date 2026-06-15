//! Per-compression-level LZ77 configuration — the safe-Rust port of
//! `deflate.c`'s `configuration_table` (AAP §0.6.6).
//!
//! zlib parameterizes its LZ77 match search by compression level through a
//! ten-entry table of four sizing parameters. Preserving these values verbatim
//! is what guarantees byte-identical LZ77 match decisions (and therefore
//! byte-identical compressed output) versus C zlib (AAP §0.6.7, §0.7.1).
//!
//! In C the table's fifth column is a `compress_func` function pointer
//! (`deflate_stored`/`deflate_fast`/`deflate_slow`) used to dispatch the
//! per-level inner loop. This port performs that dispatch from the level and
//! strategy directly (in the deflate orchestration), so the configuration
//! struct carries only the four numeric sizing fields that the match search
//! reads; the function-pointer column has no place in safe Rust and is not
//! reproduced.
//!
//! This module owns the configuration data that the engine state
//! ([`crate::deflate::state`]) loads in `lm_init`; the per-level strategy inner
//! loops (`deflate_fast`/`deflate_slow`/…) and their dispatch are layered on
//! top of this table as the deflate engine is completed.
//!
//! # Constraints
//!
//! * **No `unsafe`**, **`no_std`-clean** (`core` only) — AAP §0.6.2.

/// The four per-level LZ77 search-sizing parameters from `deflate.c`'s
/// `config` struct (all C `ush`, i.e. `u16`).
///
/// Indexed by compression level (0–9) in [`CONFIG_TABLE`]. Loaded into the
/// engine's match-search bounds by
/// [`DeflateState::lm_init`](crate::deflate::state::DeflateState::lm_init).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CompressionConfig {
    /// Reduce the lazy search above this match length (`good_length`).
    pub(crate) good_length: u16,
    /// Do not perform a lazy search above this match length (`max_lazy`).
    pub(crate) max_lazy: u16,
    /// Quit the search once a match at least this long is found (`nice_length`).
    pub(crate) nice_length: u16,
    /// Maximum number of hash-chain links to follow per search (`max_chain`).
    pub(crate) max_chain: u16,
}

impl CompressionConfig {
    /// Const constructor used to build the [`CONFIG_TABLE`] entries.
    const fn new(good_length: u16, max_lazy: u16, nice_length: u16, max_chain: u16) -> Self {
        Self {
            good_length,
            max_lazy,
            nice_length,
            max_chain,
        }
    }
}

/// The ten per-level configurations, transcribed verbatim from `deflate.c`'s
/// `configuration_table[10]` (the non-`FASTEST` build).
///
/// | Level | good | lazy | nice | chain | inner loop (C)   |
/// |-------|------|------|------|-------|------------------|
/// | 0     | 0    | 0    | 0    | 0     | `deflate_stored` |
/// | 1     | 4    | 4    | 8    | 4     | `deflate_fast`   |
/// | 2     | 4    | 5    | 16   | 8     | `deflate_fast`   |
/// | 3     | 4    | 6    | 32   | 32    | `deflate_fast`   |
/// | 4     | 4    | 4    | 16   | 16    | `deflate_slow`   |
/// | 5     | 8    | 16   | 32   | 32    | `deflate_slow`   |
/// | 6     | 8    | 16   | 128  | 128   | `deflate_slow`   |
/// | 7     | 8    | 32   | 128  | 256   | `deflate_slow`   |
/// | 8     | 32   | 128  | 258  | 1024  | `deflate_slow`   |
/// | 9     | 32   | 258  | 258  | 4096  | `deflate_slow`   |
pub(crate) const CONFIG_TABLE: [CompressionConfig; 10] = [
    /* 0: store only            */ CompressionConfig::new(0, 0, 0, 0),
    /* 1: max speed, no lazy    */ CompressionConfig::new(4, 4, 8, 4),
    /* 2                        */ CompressionConfig::new(4, 5, 16, 8),
    /* 3                        */ CompressionConfig::new(4, 6, 32, 32),
    /* 4: lazy matches          */ CompressionConfig::new(4, 4, 16, 16),
    /* 5                        */ CompressionConfig::new(8, 16, 32, 32),
    /* 6                        */ CompressionConfig::new(8, 16, 128, 128),
    /* 7                        */ CompressionConfig::new(8, 32, 128, 256),
    /* 8                        */ CompressionConfig::new(32, 128, 258, 1024),
    /* 9: max compression       */ CompressionConfig::new(32, 258, 258, 4096),
];

#[cfg(test)]
mod tests {
    use super::CONFIG_TABLE;

    #[test]
    fn config_table_matches_c_zlib() {
        // Spot-check the extremes and the default level against deflate.c.
        let l0 = CONFIG_TABLE[0];
        assert_eq!(
            (l0.good_length, l0.max_lazy, l0.nice_length, l0.max_chain),
            (0, 0, 0, 0)
        );
        let l6 = CONFIG_TABLE[6];
        assert_eq!(
            (l6.good_length, l6.max_lazy, l6.nice_length, l6.max_chain),
            (8, 16, 128, 128)
        );
        let l9 = CONFIG_TABLE[9];
        assert_eq!(
            (l9.good_length, l9.max_lazy, l9.nice_length, l9.max_chain),
            (32, 258, 258, 4096)
        );
    }

    #[test]
    fn config_table_has_ten_levels() {
        assert_eq!(CONFIG_TABLE.len(), 10);
    }
}
