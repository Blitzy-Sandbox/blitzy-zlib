//! `deflate_slow` -- lazy match evaluation. **The byte-identity epicentre.**
//!
//! A verbatim port of `deflate_slow` (`deflate.c` L1956-L2082), the compressor
//! `configuration_table` names for levels 4 to 9 (`deflate.c` L117-L123) and therefore the one
//! that produces almost all zlib-compressed data in the world, level 6 being the default. Per
//! the comment at L1950-L1954 it "achieves better compression" by using "a lazy evaluation for
//! matches: a match is finally adopted only if there is no better match at the next window
//! position".
//!
//! # Why this file may not be improved
//!
//! RFC 1951 constrains the *format*, not the encoder's *choices*. Which of several equally valid
//! matches to emit, and when to abandon one, are implementation-defined -- and this function
//! contains four of the eight decisions that make zlib's compressed output what it is. A
//! strictly better lazy-matching heuristic would still be a regression here, because the
//! acceptance criterion is byte-identical output against the reference for every level, window
//! size, memory level, strategy and flush mode. The four decisions:
//!
//! 1. **The lazy-search guard** (L1985-L1987):
//!    `hash_head != NIL && s->prev_length < s->max_lazy_match && s->strstart - hash_head <= MAX_DIST(s)`.
//!    The middle term is what makes the search *lazy*: once the previous position already found
//!    a match at least `max_lazy_match` long, no search happens at all at this position, so the
//!    previous match is adopted unexamined. `max_lazy_match` is the per-level
//!    `configuration_table` entry.
//! 2. **The `TOO_FAR` rejection** (L1994-L2005). A match is thrown away -- reset to
//!    `MIN_MATCH - 1`, which is "no match" for the comparison below -- when it is at most five
//!    bytes long *and* either the strategy is `Z_FILTERED` or it is a bare three-byte match
//!    further than [`TOO_FAR`] away. Pure heuristic, with no basis in the format: a
//!    minimum-length match at a large distance costs more bits than it saves. The `<= 5`, the
//!    `== MIN_MATCH` and the strict `>` are all reproduced exactly.
//! 3. **The adoption test** (L2010): `s->prev_length >= MIN_MATCH && s->match_length <= s->prev_length`.
//!    Note `<=`, not `<`: a current match merely *equal* in length loses to the previous one,
//!    because the previous one starts a byte earlier and so covers one more byte overall.
//! 4. **`max_insert`** (L2011-L2012): `s->strstart + s->lookahead - MIN_MATCH`, computed
//!    *before* `lookahead` is reduced, bounds how far into the adopted match strings are entered
//!    into the hash chains. That decides which matches later positions can find, so it perturbs
//!    the whole remainder of the stream rather than just this block.
//!
//! # The two flush spellings are not interchangeable
//!
//! The adopted-match branch uses `FLUSH_BLOCK` (L2038), which returns immediately when the
//! output buffer fills. The single-literal branch deliberately uses the plain
//! `FLUSH_BLOCK_ONLY` (L2047-L2049) and only gives up *after* advancing `strstart` and
//! `lookahead` (L2050-L2052):
//!
//! ```text
//! _tr_tally_lit(s, s->window[s->strstart - 1], bflush);
//! if (bflush) {
//!     FLUSH_BLOCK_ONLY(s, 0);
//! }
//! s->strstart++;
//! s->lookahead--;
//! if (s->strm->avail_out == 0) return need_more;
//! ```
//!
//! Substituting `FLUSH_BLOCK` there would return before the two cursors moved, so the next call
//! would re-emit the literal at `strstart - 1` and corrupt the stream. This port calls
//! [`crate::deflate::algorithm::flush_block_only`] directly at that one site, exactly as C does.
//!
//! # The lag of one byte
//!
//! Because the decision is deferred, `strstart` runs one position *ahead* of the byte being
//! emitted: the literal is `window[strstart - 1]` (L2046 and L2065) and the adopted match's
//! distance is `strstart - 1 - prev_match` (L2019). `match_available` is the flag that says a
//! byte is being held back, and the tail of the function (L2063-L2067) is what stops the very
//! last held byte from being lost when the input ends.
//!
//! # Safety posture
//!
//! No `unsafe`, no raw pointer and no unchecked index; `check_match` (L2017) and the two
//! `Tracevv` calls are `ZLIB_DEBUG`-only in C and have no counterpart. Every window read goes
//! through [`crate::weak_slice::Window::byte`] and every cursor update saturates, so nothing
//! here can panic.

use crate::deflate::algorithm::{flush_block, flush_block_only, BlockState, Flush, StreamCursors};
use crate::deflate::hash_chain::{insert_string, insert_string_no_head};
use crate::deflate::longest_match::longest_match;
use crate::deflate::state::{Allocator, DeflateState, IPos, Strategy, MIN_LOOKAHEAD, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Matches of exactly `MIN_MATCH` bytes further away than this are discarded.
///
/// `#define TOO_FAR 4096` (`deflate.c` L88-L89), described there as "Matches of length 3 are
/// discarded if their distance exceeds `TOO_FAR`".
///
/// The reference wraps the second half of the rejection test in `#if TOO_FAR <= 32767`
/// (`deflate.c` L1996 and L2003), a guard that protects the `uInt` comparison on 16-bit targets.
/// 4096 satisfies it, so the clause is present in the shipped build and is present here; the
/// `#if` itself has no counterpart because this port does not target 16-bit platforms.
///
/// `usize` because it is compared against `strstart - match_start`, a window distance.
const TOO_FAR: usize = 4096;

/// The longest match the [`TOO_FAR`] rule is allowed to consider discarding.
///
/// The literal `5` of `if (s->match_length <= 5 && ...)` (`deflate.c` L1994). Named rather than
/// inlined so that the two halves of that condition are legible, and deliberately *not* derived
/// from anything: it is a tuned constant, not a consequence of the format.
const TOO_FAR_MAX_LENGTH: usize = 5;

/// Compresses with lazy match evaluation: adopt a match only if the next position has no better
/// one.
///
/// Port of `deflate_slow` (`deflate.c` L1956-L2082).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] -- the lookahead is short under `Z_NO_FLUSH`, or the output buffer
///   filled while flushing a block or right after emitting a held-back literal.
/// * [`BlockState::FinishStarted`] -- `Z_FINISH` was requested and the final block did not fit.
/// * [`BlockState::FinishDone`] -- `Z_FINISH` was requested and the final block was written.
/// * [`BlockState::BlockDone`] -- a block boundary was reached for one of the other flush modes.
// `_tr_tally` keeps the C spelling of `deflate.h` L317; see `trees/mod.rs` for the same
// relaxation. `too_many_lines` is not needed -- the body is well inside `clippy.toml`'s
// threshold of 400 -- and is deliberately not requested.
#[allow(clippy::used_underscore_items)]
pub(crate) fn deflate_slow<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // "Process the input block."  `for (;;) {`  (L1960-L1961)
    loop {
        // "Make sure that we always have enough lookahead, except at the end of the input
        // file. We need MAX_MATCH bytes for the next match, plus MIN_MATCH bytes to insert the
        // string following the next match."  (L1962-L1972)
        if state.window.lookahead < MIN_LOOKAHEAD {
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            if state.window.lookahead < MIN_LOOKAHEAD && matches!(flush, Flush::NoFlush) {
                return BlockState::NeedMore;
            }
            if state.window.lookahead == 0 {
                break;
            }
        }

        // "Insert the string window[strstart .. strstart + 2] in the dictionary, and set
        // hash_head to the head of the hash chain"  (L1974-L1980)
        let mut hash_head = IPos::NIL;
        if state.window.lookahead >= MIN_MATCH {
            hash_head = insert_string(state, state.window.strstart);
        }

        // "Find the longest match, discarding those <= prev_length."  (L1982-L1984)
        //
        // ```text
        // s->prev_length = s->match_length, s->prev_match = s->match_start;
        // s->match_length = MIN_MATCH-1;
        // ```
        //
        // The comma operator is two statements; the order between them does not matter, but
        // both must happen before `match_length` is overwritten. `prev_match` is the `IPos`
        // form of `match_start`, and the conversion cannot fail: a window index is at most
        // 65 535.
        state.prev_length = state.match_length;
        state.prev_match = IPos::from_index(state.window.match_start).unwrap_or(IPos::NIL);
        state.match_length = MIN_MATCH.saturating_sub(1);

        // ```text
        // if (hash_head != NIL && s->prev_length < s->max_lazy_match &&
        //     s->strstart - hash_head <= MAX_DIST(s)) {
        // ```
        // -- L1985-L1987, decision 1 of four. See the module documentation.
        if let Some(head_index) = hash_head.match_index() {
            if state.prev_length < state.max_lazy_match
                && state.window.strstart.saturating_sub(head_index) <= state.max_dist()
            {
                // `s->match_length = longest_match (s, hash_head);`  (L1992)
                // "longest_match() sets match_start"
                state.match_length = longest_match(state, hash_head);

                // ```text
                // if (s->match_length <= 5 && (s->strategy == Z_FILTERED
                //     || (s->match_length == MIN_MATCH &&
                //         s->strstart - s->match_start > TOO_FAR)
                //     )) {
                //     s->match_length = MIN_MATCH-1;
                // }
                // ```
                // -- L1994-L2005, decision 2 of four. The comment at L1999-L2001 explains the
                // reset rather than a flag: "If prev_match is also MIN_MATCH, match_start is
                // garbage but we will ignore the current match anyway."
                //
                // The distance subtraction cannot wrap: `longest_match` only reports a
                // `match_start` strictly below `strstart`.
                let distance = state
                    .window
                    .strstart
                    .saturating_sub(state.window.match_start);
                let too_far = state.match_length == MIN_MATCH && distance > TOO_FAR;

                if state.match_length <= TOO_FAR_MAX_LENGTH
                    && (matches!(state.strategy, Strategy::Filtered) || too_far)
                {
                    state.match_length = MIN_MATCH.saturating_sub(1);
                }
            }
        }

        // "If there was a match at the previous step and the current match is not better,
        // output the previous match"  (L2007-L2010)
        //
        // `if (s->prev_length >= MIN_MATCH && s->match_length <= s->prev_length) {` -- decision
        // 3 of four; the `<=` is the whole of the tie-breaking rule.
        if state.prev_length >= MIN_MATCH && state.match_length <= state.prev_length {
            // `uInt max_insert = s->strstart + s->lookahead - MIN_MATCH;`  (L2011-L2012)
            //
            // "Do not insert strings in hash table beyond this." Decision 4 of four, and it
            // must be evaluated here, before `lookahead` is reduced below.
            let max_insert = state
                .window
                .strstart
                .saturating_add(state.window.lookahead)
                .saturating_sub(MIN_MATCH);

            // `check_match(s, s->strstart - 1, s->prev_match, (int)s->prev_length);` (L2017) is
            // `ZLIB_DEBUG`-only and has no counterpart.
            //
            // ```text
            // _tr_tally_dist(s, s->strstart - 1 - s->prev_match,
            //                s->prev_length - MIN_MATCH, bflush);
            // ```
            // -- L2019-L2020. The distance is measured from `strstart - 1`, the position the
            // adopted match actually starts at, which is the one-byte lag lazy matching
            // creates. Both conversions are in range -- the distance is at most `MAX_DIST(s)`
            // and the length operand at most `MAX_MATCH - MIN_MATCH` -- so the saturating
            // fallbacks are unreachable ceilings.
            let prev_match_index = state.prev_match.to_index().unwrap_or(0);
            let distance = state
                .window
                .strstart
                .saturating_sub(1)
                .saturating_sub(prev_match_index);
            let bflush = _tr_tally(
                state,
                u16::try_from(distance).unwrap_or(u16::MAX),
                u8::try_from(state.prev_length.saturating_sub(MIN_MATCH)).unwrap_or(u8::MAX),
            );

            // "Insert in hash table all strings up to the end of the match. strstart - 1 and
            // strstart are already inserted. If there is not enough lookahead, the last two
            // strings are not inserted in the hash table."  (L2022-L2026)
            //
            // ```text
            // s->lookahead -= s->prev_length - 1;
            // s->prev_length -= 2;
            // do {
            //     if (++s->strstart <= max_insert) {
            //         INSERT_STRING(s, s->strstart, hash_head);
            //     }
            // } while (--s->prev_length != 0);
            // ```
            // -- L2027-L2033. The order matters twice over: `lookahead` is reduced by
            // `prev_length - 1` *before* `prev_length` is reduced by 2, and the loop body then
            // runs exactly `prev_length - 2` times, which with the trailing `strstart++` below
            // advances the cursor by `prev_length - 1` in total. `insert_string_no_head` is
            // `INSERT_STRING` with the returned chain head discarded, which is what C does
            // here: it assigns `hash_head` and never reads it again.
            state.window.lookahead = state
                .window
                .lookahead
                .saturating_sub(state.prev_length.saturating_sub(1));
            state.prev_length = state.prev_length.saturating_sub(2);
            while state.prev_length != 0 {
                state.window.strstart = state.window.strstart.saturating_add(1);
                if state.window.strstart <= max_insert {
                    insert_string_no_head(state, state.window.strstart);
                }
                state.prev_length = state.prev_length.saturating_sub(1);
            }

            // ```text
            // s->match_available = 0;
            // s->match_length = MIN_MATCH-1;
            // s->strstart++;
            // ```
            // -- L2034-L2036
            state.match_available = false;
            state.match_length = MIN_MATCH.saturating_sub(1);
            state.window.strstart = state.window.strstart.saturating_add(1);

            // `if (bflush) FLUSH_BLOCK(s, 0);`  (L2038)
            if bflush {
                flush_block!(state, cursors, false);
            }
        } else if state.match_available {
            // "If there was no match at the previous position, output a single literal. If
            // there was a match but the current match is longer, truncate the previous match to
            // a single literal."  (L2040-L2045)
            //
            // `_tr_tally_lit(s, s->window[s->strstart - 1], bflush);`  (L2046)
            //
            // `match_available` implies `strstart >= 1`: the flag is only ever set immediately
            // after a `strstart++`. The read therefore cannot fail; declining to emit would
            // leave the held byte unemitted, so the loop is left instead, which routes to the
            // tail below where the same literal is flushed.
            let Some(literal) = state.window.byte(state.window.strstart.saturating_sub(1)) else {
                break;
            };
            let bflush = _tr_tally(state, 0, literal);

            // `if (bflush) { FLUSH_BLOCK_ONLY(s, 0); }`  (L2047-L2049)
            //
            // Plain `FLUSH_BLOCK_ONLY`, NOT `FLUSH_BLOCK`. See the module documentation: the
            // two cursor updates below must happen before the function can give up.
            if bflush {
                flush_block_only(state, cursors, false);
            }

            // `s->strstart++; s->lookahead--;`  (L2050-L2051)
            state.window.strstart = state.window.strstart.saturating_add(1);
            state.window.lookahead = state.window.lookahead.saturating_sub(1);

            // `if (s->strm->avail_out == 0) return need_more;`  (L2052)
            if cursors.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // "There is no previous match to compare with, wait for the next step to decide."
            // (L2053-L2058)
            //
            // ```text
            // s->match_available = 1;
            // s->strstart++;
            // s->lookahead--;
            // ```
            state.match_available = true;
            state.window.strstart = state.window.strstart.saturating_add(1);
            state.window.lookahead = state.window.lookahead.saturating_sub(1);
        }
    }

    // `Assert (flush != Z_NO_FLUSH, "no flush?");`  (L2061)
    //
    // The loop can only be left through `break`, which is reached when `lookahead` hits zero,
    // and the `Z_NO_FLUSH` path returns `need_more` before that. Debug-only, as in C.
    debug_assert!(
        !matches!(flush, Flush::NoFlush),
        "no flush? (deflate.c L2061)"
    );

    // "Emit the byte still held back, if any."  (L2062-L2067)
    //
    // ```text
    // if (s->match_available) {
    //     Tracevv((stderr,"%c", s->window[s->strstart - 1]));
    //     _tr_tally_lit(s, s->window[s->strstart - 1], bflush);
    //     s->match_available = 0;
    // }
    // ```
    //
    // C assigns `bflush` here and then never reads it: the flush decisions below are made on
    // `flush` and `sym_next` instead, so a symbol buffer that filled on this very literal is
    // emptied by one of them regardless. The discard is therefore faithful, not an oversight,
    // and is spelled out with `let _` so that `#[must_use]` cannot be silently ignored.
    if state.match_available {
        if let Some(literal) = state.window.byte(state.window.strstart.saturating_sub(1)) {
            let _bflush = _tr_tally(state, 0, literal);
        }
        state.match_available = false;
    }

    // `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;`  (L2068)
    state.window.insert = state.window.strstart.min(MIN_MATCH.saturating_sub(1));

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L2069-L2072)
    if matches!(flush, Flush::Finish) {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L2073-L2074)
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L2075)
    BlockState::BlockDone
}
