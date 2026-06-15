//! Compression-level configuration table and per-call strategy dispatch — the
//! safe-Rust port of `deflate.c`'s `config` struct, its `configuration_table`,
//! and the strategy-selection ternary in `deflate()` (AAP §0.6.6).
//!
//! # Why this module is byte-critical
//!
//! zlib parameterizes its LZ77 match search by compression level through a
//! ten-entry table of four sizing parameters — `good_length`, `max_lazy`,
//! `nice_length`, and `max_chain`. Those four numbers per level decide the
//! exact lazy-match thresholds, the greedy-versus-lazy selection, and how many
//! hash-chain links the match search follows before giving up. Reproducing them
//! **verbatim** is precisely what makes this port emit byte-identical
//! compressed output to C zlib for the same input, level, strategy, and window
//! (AAP §0.6.7, §0.7.1). Not a single number in [`CONFIG_TABLE`] may differ
//! from `deflate.c`.
//!
//! # What this module provides
//!
//! * [`CompressFn`] — the signature every per-level inner loop shares; the C
//!   `compress_func` typedef (`deflate.c` L70).
//! * [`CompressionConfig`] — the Rust form of C's `config` struct (`config_s`,
//!   `deflate.c` L98-103): four sizing parameters plus the inner-loop selector.
//! * [`CONFIG_TABLE`] — the ten level configurations, transcribed verbatim from
//!   `deflate.c` `configuration_table[10]` (L112-124).
//! * [`deflate_dispatch`] — the per-call selector that maps the current
//!   level/strategy to the inner loop to run, preserving C's exact precedence
//!   (`deflate.c` L1217-1220).
//! * [`config_compress_fn`] / [`same_compress_fn`] — the table lookup and
//!   function-pointer comparison that `deflateParams` uses to decide whether
//!   the active inner loop changes across a reparametrize (`deflate.c`
//!   L786-799).
//!
//! # Constraints
//!
//! * **No `unsafe`** — this is core compression logic (AAP §0.6.2).
//! * **`no_std`-clean** — only `core` and sibling `crate` modules are used.

use crate::constants::{FlushMode, Strategy};
use crate::deflate::fast::deflate_fast;
use crate::deflate::huff::deflate_huff;
use crate::deflate::rle::deflate_rle;
use crate::deflate::slow::deflate_slow;
use crate::deflate::state::{BlockState, DeflateStream};
use crate::deflate::stored::deflate_stored;

/// Signature shared by every per-level DEFLATE inner loop — the safe-Rust port
/// of C's `typedef block_state (*compress_func)(deflate_state *s, int flush);`
/// (`deflate.c` L70).
///
/// Each of the five strategy compressors —
/// [`deflate_stored`](crate::deflate::stored::deflate_stored),
/// [`deflate_fast`](crate::deflate::fast::deflate_fast),
/// [`deflate_slow`](crate::deflate::slow::deflate_slow),
/// [`deflate_rle`](crate::deflate::rle::deflate_rle), and
/// [`deflate_huff`](crate::deflate::huff::deflate_huff) — has exactly this
/// type. Because [`CompressionConfig::compress_fn`] stores a `CompressFn`, the
/// const [`CONFIG_TABLE`] doubles as a *compile-time* guard that all five
/// implementations keep identical signatures: if any one drifts from
/// `fn(&mut DeflateStream, FlushMode) -> BlockState`, the table fails to
/// type-check.
pub(crate) type CompressFn = fn(&mut DeflateStream, FlushMode) -> BlockState;

/// The per-level LZ77 search-sizing parameters plus the inner-loop selector —
/// the safe-Rust port of `deflate.c`'s `config` struct (`config_s`, L98-103,
/// whose C fields are all `ush`, i.e. `u16`, followed by a `compress_func`).
///
/// The four numeric fields are indexed by compression level (0–9) in
/// [`CONFIG_TABLE`] and loaded into the engine's match-search bounds by
/// [`DeflateState::lm_init`](crate::deflate::state::DeflateState::lm_init).
///
/// The struct is `Copy` so a single level entry can be read out of the const
/// table by value (`let cfg = CONFIG_TABLE[level];`, as
/// [`lm_init`](crate::deflate::state::DeflateState::lm_init) does). It
/// deliberately does **not** derive `PartialEq`/`Eq`: a derived equality would
/// have to compare the [`compress_fn`](Self::compress_fn) function pointer with
/// `==`, which is unreliable (the optimizer may merge or duplicate identical
/// functions) and is rejected by clippy. [`same_compress_fn`] exists for the
/// one place that needs to compare these pointers, using
/// [`core::ptr::fn_addr_eq`].
#[derive(Clone, Copy)]
pub(crate) struct CompressionConfig {
    /// Reduce the lazy search above this match length (C `good_length`).
    pub(crate) good_length: u16,
    /// Do not perform a lazy search above this match length (C `max_lazy`).
    ///
    /// For level 0 the C source overloads this slot as `max_insert_length`; the
    /// raw value is preserved here and
    /// [`lm_init`](crate::deflate::state::DeflateState::lm_init) copies it into
    /// `DeflateState::max_lazy_match` unchanged.
    pub(crate) max_lazy: u16,
    /// Quit the search once a match at least this long is found (C
    /// `nice_length`).
    pub(crate) nice_length: u16,
    /// Maximum number of hash-chain links to follow per search (C `max_chain`).
    pub(crate) max_chain: u16,
    /// The inner-loop compressor selected for this level (C `func`).
    pub(crate) compress_fn: CompressFn,
}

/// The ten per-level configurations, transcribed **verbatim** from `deflate.c`
/// `configuration_table[10]` (the non-`FASTEST` build, L112-124).
///
/// | Level | good | lazy | nice | chain | inner loop       |
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
///
/// Level 0 stores without compression; levels 1–3 use the greedy
/// [`deflate_fast`] matcher; levels 4–9 use the lazy [`deflate_slow`] matcher.
/// Any deviation from these numbers changes the LZ77 match decisions and breaks
/// byte-for-byte compatibility with C zlib (AAP §0.6.6).
///
/// The table is laid out one entry per line, column-aligned to mirror the C
/// source so the verbatim audit is a straight column read; `#[rustfmt::skip]`
/// preserves that alignment.
#[rustfmt::skip]
pub(crate) const CONFIG_TABLE: [CompressionConfig; 10] = [
    //                   good       lazy        nice         chain          inner loop
    CompressionConfig { good_length:  0, max_lazy:   0, nice_length:   0, max_chain:    0, compress_fn: deflate_stored }, //  0  store only
    CompressionConfig { good_length:  4, max_lazy:   4, nice_length:   8, max_chain:    4, compress_fn: deflate_fast   }, //  1  max speed, no lazy
    CompressionConfig { good_length:  4, max_lazy:   5, nice_length:  16, max_chain:    8, compress_fn: deflate_fast   }, //  2
    CompressionConfig { good_length:  4, max_lazy:   6, nice_length:  32, max_chain:   32, compress_fn: deflate_fast   }, //  3
    CompressionConfig { good_length:  4, max_lazy:   4, nice_length:  16, max_chain:   16, compress_fn: deflate_slow   }, //  4  lazy matches
    CompressionConfig { good_length:  8, max_lazy:  16, nice_length:  32, max_chain:   32, compress_fn: deflate_slow   }, //  5
    CompressionConfig { good_length:  8, max_lazy:  16, nice_length: 128, max_chain:  128, compress_fn: deflate_slow   }, //  6
    CompressionConfig { good_length:  8, max_lazy:  32, nice_length: 128, max_chain:  256, compress_fn: deflate_slow   }, //  7
    CompressionConfig { good_length: 32, max_lazy: 128, nice_length: 258, max_chain: 1024, compress_fn: deflate_slow   }, //  8
    CompressionConfig { good_length: 32, max_lazy: 258, nice_length: 258, max_chain: 4096, compress_fn: deflate_slow   }, //  9  max compression
];

/// Select and run the per-call DEFLATE inner loop for the stream's current
/// level and strategy — the safe-Rust port of the strategy-dispatch ternary in
/// C `deflate()` (`deflate.c` L1217-1220):
///
/// ```c
/// bstate = s->level == 0 ? deflate_stored(s, flush) :
///          s->strategy == Z_HUFFMAN_ONLY ? deflate_huff(s, flush) :
///          s->strategy == Z_RLE ? deflate_rle(s, flush) :
///          (*(configuration_table[s->level].func))(s, flush);
/// ```
///
/// The precedence is **byte-significant** and reproduced exactly:
///
/// 1. **Level 0 first.** A level-0 stream stores without compression *even when*
///    a strategy such as `Z_HUFFMAN_ONLY` or `Z_RLE` is also set — the level
///    check wins over the strategy.
/// 2. Then `Z_HUFFMAN_ONLY` ([`Strategy::HuffmanOnly`]) → [`deflate_huff`].
/// 3. Then `Z_RLE` ([`Strategy::Rle`]) → [`deflate_rle`].
/// 4. Otherwise the level's table entry
///    ([`CONFIG_TABLE`]`[level].compress_fn`): [`deflate_fast`] for levels 1–3,
///    [`deflate_slow`] for levels 4–9.
///
/// `s.state.level` is the **resolved** level — `Z_DEFAULT_COMPRESSION` (`-1`)
/// has already been mapped to `6` by
/// [`resolve_level`](crate::constants::resolve_level) in `deflateInit2` /
/// `deflateParams` before any call reaches here — so it is always in `0..=9`
/// and indexing [`CONFIG_TABLE`] cannot go out of bounds. A `debug_assert!`
/// documents and (in debug builds) checks that invariant.
pub(crate) fn deflate_dispatch(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // Read the two selectors out as plain `i32`s up front. They are `Copy`, so
    // taking them by value ends the borrow of `s.state` before `s` is
    // reborrowed mutably for the inner-loop call below.
    let level = s.state.level;
    let strategy = s.state.strategy;

    // The resolved level is always 0..=9 (see the doc comment); guard it in
    // debug builds so a mis-wired caller is caught before the table index.
    debug_assert!(
        (0..=9).contains(&level),
        "deflate_dispatch requires a resolved level in 0..=9, got {level}"
    );

    if level == 0 {
        // Level 0 wins over any strategy: store only.
        deflate_stored(s, flush)
    } else if strategy == Strategy::HuffmanOnly as i32 {
        deflate_huff(s, flush)
    } else if strategy == Strategy::Rle as i32 {
        deflate_rle(s, flush)
    } else {
        (CONFIG_TABLE[level as usize].compress_fn)(s, flush)
    }
}

/// Return the inner-loop function configured for `level` — the table lookup
/// `configuration_table[level].func` that C `deflateParams` performs
/// (`deflate.c` L788 and L799).
///
/// `level` must be a resolved level in `0..=9` (see [`deflate_dispatch`]);
/// indexing [`CONFIG_TABLE`] out of range panics, matching the C contract that
/// `deflateParams` validates the level before this lookup.
#[inline]
pub(crate) fn config_compress_fn(level: i32) -> CompressFn {
    CONFIG_TABLE[level as usize].compress_fn
}

/// Compare two [`CompressFn`] pointers by address — returns `true` iff they
/// refer to the same inner-loop function.
///
/// This is the building block `deflateParams` uses to detect whether the active
/// compressor changes across a reparametrize (C `func != configuration_table[
/// level].func`, `deflate.c` L789). It uses [`core::ptr::fn_addr_eq`] — stable
/// since the crate's MSRV (Rust 1.85) — rather than `a == b`: a direct `==` on
/// function pointers is rejected by clippy's
/// `unpredictable_function_pointer_comparisons` lint, because the optimizer may
/// merge identical functions or duplicate one across codegen units, making raw
/// pointer equality unreliable. Address comparison is exactly what the C `!=`
/// test means here.
///
/// `deflateParams` (in `mod.rs`) computes the pre-reparametrize flush decision
/// the way `deflate.c` L789 does, for example:
///
/// ```ignore
/// let func = config_compress_fn(s.state.level);
/// let changed = new_strategy != s.state.strategy
///     || !same_compress_fn(func, config_compress_fn(new_level));
/// ```
#[inline]
pub(crate) fn same_compress_fn(a: CompressFn, b: CompressFn) -> bool {
    core::ptr::fn_addr_eq(a, b)
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_TABLE, CompressFn, config_compress_fn, same_compress_fn};
    use crate::deflate::fast::deflate_fast;
    use crate::deflate::slow::deflate_slow;
    use crate::deflate::stored::deflate_stored;

    /// The four sizing parameters per level, transcribed *independently* from
    /// `deflate.c` `configuration_table` (L112-124) so the test is a second,
    /// separate verbatim copy to diff against [`CONFIG_TABLE`].
    /// Order: `(good_length, max_lazy, nice_length, max_chain)`.
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

    #[test]
    fn config_table_has_ten_levels() {
        assert_eq!(CONFIG_TABLE.len(), 10);
    }

    /// Audit every one of the 50 numbers (10 levels × 5 columns) against the
    /// independently transcribed C values. This is the byte-exactness anchor.
    #[test]
    fn config_table_numbers_match_c_zlib_verbatim() {
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

    /// Every level maps to the inner loop C selects in `configuration_table`:
    /// level 0 → `deflate_stored`, levels 1–3 → the greedy `deflate_fast`, and
    /// levels 4–9 → the lazy `deflate_slow` (the table's fifth column).
    #[test]
    fn each_level_maps_to_expected_inner_loop() {
        for (level, cfg) in CONFIG_TABLE.iter().enumerate() {
            let expected: CompressFn = match level {
                0 => deflate_stored,
                1..=3 => deflate_fast,
                _ => deflate_slow, // levels 4..=9
            };
            assert!(
                same_compress_fn(cfg.compress_fn, expected),
                "level {level} maps to the wrong inner loop"
            );
        }
    }

    /// [`config_compress_fn`] returns exactly the table entry's function for
    /// every valid level.
    #[test]
    fn config_compress_fn_matches_table() {
        for level in 0..=9i32 {
            assert!(same_compress_fn(
                config_compress_fn(level),
                CONFIG_TABLE[level as usize].compress_fn
            ));
        }
    }

    /// [`same_compress_fn`] reports equality for the same underlying function
    /// reached through different table slots, and inequality for distinct
    /// functions — the property `deflateParams` relies on. Arguments are always
    /// distinct expressions so no trivial-equality lint applies.
    #[test]
    fn same_compress_fn_identity_and_discrimination() {
        // Same function via two different table slots → equal.
        assert!(same_compress_fn(
            CONFIG_TABLE[1].compress_fn,
            CONFIG_TABLE[3].compress_fn
        )); // both deflate_fast
        assert!(same_compress_fn(
            CONFIG_TABLE[4].compress_fn,
            CONFIG_TABLE[9].compress_fn
        )); // both deflate_slow

        // Distinct functions → not equal.
        assert!(!same_compress_fn(
            CONFIG_TABLE[0].compress_fn,
            CONFIG_TABLE[1].compress_fn
        )); // stored vs fast
        assert!(!same_compress_fn(
            CONFIG_TABLE[1].compress_fn,
            CONFIG_TABLE[4].compress_fn
        )); // fast vs slow
        assert!(!same_compress_fn(
            CONFIG_TABLE[0].compress_fn,
            CONFIG_TABLE[4].compress_fn
        )); // stored vs slow
    }
}
