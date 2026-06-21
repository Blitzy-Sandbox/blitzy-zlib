//! `deflate_slow` — lazy LZ77 matching for compression levels 4–9.
//!
//! This module is a faithful, 100% safe-Rust port of the C `deflate_slow()`
//! function (`deflate.c` L1956-2076). It implements the classic zlib
//! *lazy-evaluation* match strategy: at each position the engine defers the
//! decision to emit a match by one byte, so that it can first check whether the
//! *next* position yields a strictly longer match. If it does, the previous
//! (shorter) match is dropped to a single literal and the longer match is taken
//! instead; otherwise the previous match is emitted.
//!
//! `deflate_slow` is selected by the per-level [`CONFIG_TABLE`] for levels 4
//! through 9 — including the default level 6 — so it is by far the
//! highest-traffic compression path. That makes it the *byte-exactness linchpin*
//! of the whole crate: any divergence from C zlib here would corrupt the
//! produced bitstream for the most common configuration (AAP §0.6.6, §0.6.7,
//! §0.7.1).
//!
//! [`CONFIG_TABLE`]: crate::deflate::strategy
//!
//! # Byte-identical output
//!
//! Four subtle behaviours must reproduce C zlib *exactly*, and are ported here
//! verbatim — the control flow is deliberately **not** refactored:
//!
//! 1. **The lazy-match decision** `prev_length >= MIN_MATCH && match_length <=
//!    prev_length`. The `<=` (not `<`) is intentional: on a tie the earlier,
//!    already-found match wins, which avoids choosing an equally-long match at a
//!    greater distance.
//! 2. **The short-match suppression** for `Z_FILTERED` and for length-3 matches
//!    that are farther than [`TOO_FAR`]; a suppressed match is reset to
//!    `MIN_MATCH - 1` so it is ignored.
//! 3. **The deferred-literal (`match_available`) handling**, including the use
//!    of `FLUSH_BLOCK_ONLY` (flush *without* an early return) followed by the
//!    `avail_out == 0` check — the ordering is behaviour-critical.
//! 4. **The hash-insertion `do`-`while`** that inserts every string up to the
//!    end of the previous match.
//!
//! # Safety
//!
//! There is **no `unsafe`** in this module (AAP §0.6.2), and it is
//! `no_std`-clean: it touches no `std::` item directly, operating entirely
//! through the safe slice/owned-buffer abstractions provided by
//! [`DeflateStream`]/`DeflateState`.

use crate::constants::{FlushMode, Strategy};
use crate::deflate::state::{BlockState, DeflateStream, MIN_LOOKAHEAD, MIN_MATCH, NIL, TOO_FAR};
use crate::deflate::trees;
use crate::deflate::{flush_block, flush_block_only};

/// Compress as much as possible from the input stream using **lazy** matching,
/// returning the [`BlockState`] reached.
///
/// This is a literal port of C `deflate_slow()` (`deflate.c` L1956-2076). It is
/// driven by `deflate()` in `mod.rs` for compression levels 4–9 (the default
/// level 6 included), and it is the safe-Rust counterpart that replaces the
/// integer-state-plus-`goto` control flow of the C original with ordinary Rust
/// loops and early `return`s.
///
/// # Algorithm
///
/// The function processes the window one position at a time. At each step it:
///
/// 1. Refills the lookahead via [`DeflateStream::fill_window`] when it drops
///    below [`MIN_LOOKAHEAD`], bailing out with [`BlockState::NeedMore`] if more
///    input is needed and the caller did not request a flush.
/// 2. Inserts the 3-byte string at `strstart` into the hash table, recording the
///    previous head of its hash chain.
/// 3. Saves the previous match (`prev_length`/`prev_match`) and searches for a
///    new, possibly longer match via [`DeflateStream::longest_match`].
/// 4. Applies the lazy decision: if the *previous* match was at least
///    `MIN_MATCH` long and is not bettered by the current one, the previous
///    match is emitted via [`trees::tr_tally_dist`]; otherwise a single literal
///    is emitted (or the decision is deferred one more byte).
///
/// # Parameters
///
/// * `s` — the per-call deflate I/O context (engine state plus input/output
///   cursors).
/// * `flush` — the flush mode requested by the caller; controls whether a short
///   lookahead forces an early [`BlockState::NeedMore`] return and whether the
///   final block is emitted as the last block.
///
/// # Returns
///
/// The [`BlockState`] describing how far the current block progressed:
/// [`NeedMore`](BlockState::NeedMore), [`BlockDone`](BlockState::BlockDone),
/// [`FinishStarted`](BlockState::FinishStarted) (via the flush helpers), or
/// [`FinishDone`](BlockState::FinishDone).
pub(crate) fn deflate_slow(s: &mut DeflateStream<'_>, flush: FlushMode) -> BlockState {
    // Process the input block. (C `for (;;)`.)
    loop {
        // Make sure that we always have enough lookahead, except at the end of
        // the input file. We need MAX_MATCH bytes for the next match, plus
        // MIN_MATCH bytes to insert the string following the next match.
        if s.state.lookahead < MIN_LOOKAHEAD {
            s.fill_window();
            if s.state.lookahead < MIN_LOOKAHEAD && flush == FlushMode::NoFlush {
                return BlockState::NeedMore;
            }
            if s.state.lookahead == 0 {
                // Flush the current block.
                break;
            }
        }

        // Insert the string window[strstart .. strstart + 2] in the dictionary,
        // and set `hash_head` to the head of the hash chain. (C `INSERT_STRING`,
        // whose returned previous head is the value we need.)
        let mut hash_head: u16 = NIL;
        if s.state.lookahead >= MIN_MATCH {
            let pos = s.state.strstart;
            hash_head = s.state.insert_string(pos);
        }

        // Find the longest match, discarding those <= prev_length. First save
        // the previous match and reset the current match length to the
        // "no match" sentinel.
        s.state.prev_length = s.state.match_length;
        s.state.prev_match = s.state.match_start;
        s.state.match_length = MIN_MATCH - 1;

        if hash_head != NIL
            && s.state.prev_length < s.state.max_lazy_match
            && s.state.strstart - (hash_head as usize) <= s.state.w_size - MIN_LOOKAHEAD
        {
            // To simplify the code, we prevent matches with the string of
            // window index 0 (in particular we have to avoid a match of the
            // string with itself at the start of the input file).
            //
            // `longest_match` sets `match_start` as a side effect.
            s.state.match_length = s.longest_match(hash_head as usize);

            // Ignore a length-3 match if it is too distant, or any short match
            // under `Z_FILTERED`. `TOO_FAR <= 32767` always holds, so the
            // distance test is unconditional (the C `#if TOO_FAR <= 32767`
            // guard is always true here, `TOO_FAR == 4096`).
            if s.state.match_length <= 5
                && (s.state.strategy == Strategy::Filtered as i32
                    || (s.state.match_length == MIN_MATCH
                        && s.state.strstart - s.state.match_start > TOO_FAR))
            {
                // If `prev_match` is also `MIN_MATCH`, `match_start` is garbage
                // but we will ignore the current match anyway.
                s.state.match_length = MIN_MATCH - 1;
            }
        }

        // If there was a match at the previous step and the current match is
        // not better, output the previous match. The `<=` tie-break is
        // intentional and must be preserved verbatim.
        if s.state.prev_length >= MIN_MATCH && s.state.match_length <= s.state.prev_length {
            // Do not insert strings in the hash table beyond this point.
            let max_insert = s.state.strstart + s.state.lookahead - MIN_MATCH;

            // C `check_match()` is a debug-only consistency assertion that
            // compiles to nothing in release builds; it emits no output bytes
            // and is intentionally omitted from this port.

            // Tally the previous match: the distance is `strstart - 1 -
            // prev_match` and the transmitted length code is `prev_length -
            // MIN_MATCH`. `wrapping_sub` mirrors C's unsigned `uInt` arithmetic
            // so that, when `fill_window` has just slid the window (decrementing
            // both `strstart` and `prev_match` by `w_size`, with `prev_match`
            // possibly wrapping), the modular subtraction still yields the exact
            // true distance — byte-identical to canonical zlib.
            let dist = s
                .state
                .strstart
                .wrapping_sub(1)
                .wrapping_sub(s.state.prev_match);
            let len = (s.state.prev_length - MIN_MATCH) as u8;
            let bflush = trees::tr_tally_dist(s.state, dist, len);

            // Insert in the hash table all strings up to the end of the match.
            // `strstart - 1` and `strstart` are already inserted. If there is
            // not enough lookahead, the last two strings are not inserted into
            // the hash table.
            //
            // This reproduces the C `do { ... } while (--s->prev_length != 0)`
            // exactly: decrement `prev_length` by 2 up front (it has already
            // been "spent" on the two inserted strings), then loop, advancing
            // `strstart`, inserting while within `max_insert`, and decrementing
            // until it reaches zero.
            s.state.lookahead -= s.state.prev_length - 1;
            s.state.prev_length -= 2;
            loop {
                s.state.strstart += 1;
                if s.state.strstart <= max_insert {
                    // The returned previous head is intentionally discarded
                    // here; the next outer iteration recomputes `hash_head`.
                    let pos = s.state.strstart;
                    s.state.insert_string(pos);
                }
                s.state.prev_length -= 1;
                if s.state.prev_length == 0 {
                    break;
                }
            }
            s.state.match_available = false;
            s.state.match_length = MIN_MATCH - 1;
            s.state.strstart += 1;

            if bflush {
                // C `FLUSH_BLOCK(s, 0)`: flush, and return `need_more` early if
                // the output buffer filled up.
                if let Some(bs) = flush_block(s, false) {
                    return bs;
                }
            }
        } else if s.state.match_available {
            // There was no match at the previous position, so output a single
            // literal. If there was a match but the current match is longer, the
            // previous match is likewise truncated to a single literal.
            let lc = s.state.window[s.state.strstart - 1];
            let bflush = trees::tr_tally_lit(s.state, lc);
            if bflush {
                // C `FLUSH_BLOCK_ONLY(s, 0)`: flush WITHOUT an early return. The
                // `avail_out == 0` check happens *after* advancing the cursors
                // below — this ordering is byte/behaviour-critical, so we must
                // use `flush_block_only` here, never `flush_block`.
                flush_block_only(s, false);
            }
            s.state.strstart += 1;
            s.state.lookahead -= 1;
            if s.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // There is no previous match to compare with; wait for the next step
            // to decide. Defer this position as a possible literal.
            s.state.match_available = true;
            s.state.strstart += 1;
            s.state.lookahead -= 1;
        }
    }

    // C `Assert (flush != Z_NO_FLUSH, "no flush?")`: reaching here with
    // `Z_NO_FLUSH` is impossible, because a short lookahead under `NoFlush`
    // always returns `NeedMore` above rather than breaking out of the loop.
    debug_assert_ne!(
        flush,
        FlushMode::NoFlush,
        "deflate_slow reached the block tail with Z_NO_FLUSH"
    );

    // Emit any remaining deferred literal (the last byte may still be pending as
    // a lazy single literal). The flush result is intentionally discarded — the
    // block flush below handles emitting the tallied symbols.
    if s.state.match_available {
        let lc = s.state.window[s.state.strstart - 1];
        let _ = trees::tr_tally_lit(s.state, lc);
        s.state.match_available = false;
    }

    // Record how many bytes at the end of the window still need to be inserted
    // into the hash table on the next call (`s->insert`).
    s.state.insert = if s.state.strstart < MIN_MATCH - 1 {
        s.state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == FlushMode::Finish {
        // C `FLUSH_BLOCK(s, 1)`: emit the final block, returning early if the
        // output filled up; otherwise the finish is complete.
        if let Some(bs) = flush_block(s, true) {
            return bs;
        }
        return BlockState::FinishDone;
    }

    if s.state.sym_next != 0 {
        // There are buffered symbols to emit before reporting the block done.
        if let Some(bs) = flush_block(s, false) {
            return bs;
        }
    }

    BlockState::BlockDone
}
