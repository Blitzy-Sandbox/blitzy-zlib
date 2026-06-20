//! Per-compression-level configuration table, ported verbatim from the
//! `configuration_table` in `deflate.c` (L107-124).
//!
//! Each of the ten compression levels (0-9) selects a tuned set of LZ77
//! match-search parameters. Reproducing these values *exactly* is required for
//! byte-identical compressed output (AAP §0.6.6, §0.7.1): the parameters drive
//! which matches the encoder selects, and any deviation changes the emitted
//! bitstream.
//!
//! The per-level *compress function* (`deflate_stored` for level 0,
//! `deflate_fast` for levels 1-3, `deflate_slow` for levels 4-9) is selected
//! separately by the `deflate()` driver, so — unlike the C `config` struct —
//! [`CompressionConfig`] deliberately omits the function pointer and carries
//! only the four numeric search parameters (matching the four fields the
//! engine state reads in `DeflateState::lm_init`).

/// The LZ77 match-search parameters for a single compression level.
///
/// Mirrors the numeric members of the C `config` struct (`deflate.c` L98-104).
/// All four are `u16` to match the C `ush` declarations; the engine widens them
/// to `usize` at the point of use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompressionConfig {
    /// `good_length`: reduce lazy-match search above this match length.
    ///
    /// Ignored by `deflate_fast` (levels ≤ 3); used by `deflate_slow`.
    pub good_length: u16,

    /// `max_lazy`: do not perform a lazy-match search above this match length.
    ///
    /// For `deflate_fast` this field instead serves as `max_insert_length`
    /// (the C `#define max_insert_length max_lazy_match`).
    pub max_lazy: u16,

    /// `nice_length`: stop the match search once a match at least this long is
    /// found.
    pub nice_length: u16,

    /// `max_chain`: the maximum number of hash-chain links to follow when
    /// searching for the longest match.
    pub max_chain: u16,
}

impl CompressionConfig {
    /// Constructs a configuration from its four search parameters.
    ///
    /// Provided so the [`CONFIG_TABLE`] entries read in the same
    /// `good / lazy / nice / chain` column order as the C source.
    #[inline]
    #[must_use]
    pub const fn new(good_length: u16, max_lazy: u16, nice_length: u16, max_chain: u16) -> Self {
        Self {
            good_length,
            max_lazy,
            nice_length,
            max_chain,
        }
    }
}

/// The ten-level compression configuration table.
///
/// Indexed by compression level (0-9); `Z_DEFAULT_COMPRESSION` (-1) is resolved
/// to level 6 by the driver before indexing. The values reproduce
/// `deflate.c`'s `configuration_table[10]` exactly:
///
/// ```text
///         good lazy nice chain
/// /* 0 */ {0,    0,  0,    0}   store only
/// /* 1 */ {4,    4,  8,    4}   max speed, no lazy matches
/// /* 2 */ {4,    5, 16,    8}
/// /* 3 */ {4,    6, 32,   32}
/// /* 4 */ {4,    4, 16,   16}   lazy matches
/// /* 5 */ {8,   16, 32,   32}
/// /* 6 */ {8,   16, 128, 128}
/// /* 7 */ {8,   32, 128, 256}
/// /* 8 */ {32, 128, 258, 1024}
/// /* 9 */ {32, 258, 258, 4096}  max compression
/// ```
pub const CONFIG_TABLE: [CompressionConfig; 10] = [
    CompressionConfig::new(0, 0, 0, 0),         // 0: store only
    CompressionConfig::new(4, 4, 8, 4),         // 1: max speed, no lazy matches
    CompressionConfig::new(4, 5, 16, 8),        // 2
    CompressionConfig::new(4, 6, 32, 32),       // 3
    CompressionConfig::new(4, 4, 16, 16),       // 4: lazy matches
    CompressionConfig::new(8, 16, 32, 32),      // 5
    CompressionConfig::new(8, 16, 128, 128),    // 6
    CompressionConfig::new(8, 32, 128, 256),    // 7
    CompressionConfig::new(32, 128, 258, 1024), // 8
    CompressionConfig::new(32, 258, 258, 4096), // 9: max compression
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_table_matches_c_configuration_table() {
        // Verbatim cross-check against `deflate.c` configuration_table[10].
        let expected: [(u16, u16, u16, u16); 10] = [
            (0, 0, 0, 0),
            (4, 4, 8, 4),
            (4, 5, 16, 8),
            (4, 6, 32, 32),
            (4, 4, 16, 16),
            (8, 16, 32, 32),
            (8, 16, 128, 128),
            (8, 32, 128, 256),
            (32, 128, 258, 1024),
            (32, 258, 258, 4096),
        ];
        for (level, &(good, lazy, nice, chain)) in expected.iter().enumerate() {
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

    #[test]
    fn config_table_has_ten_levels() {
        assert_eq!(CONFIG_TABLE.len(), 10);
    }
}
