//! `deflate_fast` -- greedy matching with no lazy evaluation.
//!
//! A verbatim port of `deflate_fast` (`deflate.c` L1857-L1954), the compressor
//! `configuration_table` names for levels 1 to 3 (`deflate.c` L114-L116). Per the comment at
//! L1851-L1856 it "does not perform lazy evaluation of matches and inserts new strings in the
//! dictionary only for unmatched strings or for short matches", which is exactly the trade that
//! makes it fast: every match found is emitted immediately, and long matches skip the hash
//! insertions that would let later positions find them.
//!
//! # The three decisions that determine the emitted bytes
//!
//! * **The candidate guard** (L1885): `hash_head != NIL && s->strstart - hash_head <= MAX_DIST(s)`.
//!   `INSERT_STRING` has already made `strstart` the front of its chain, so `hash_head` is the
//!   *previous* occupant -- the newest strictly older position with the same three-byte key. A
//!   `NIL` head means the chain was empty, and window index 0 is excluded from being a match
//!   deliberately, "in particular we have to avoid a match of the string with itself at the
//!   start of the input file" (L1886-L1889). [`crate::weak_slice::IPos::match_index`] encodes
//!   that rule, and `MAX_DIST(s)` is `w_size - MIN_LOOKAHEAD` (`deflate.h` L301).
//! * **The insertion cutoff** (L1906-L1907): a match is only walked into the hash chains when
//!   `match_length <= s->max_insert_length && s->lookahead >= MIN_MATCH`. `max_insert_length` is
//!   `max_lazy_match` under another name (`deflate.h` L186-L189, "used only for compression
//!   levels <= 3"), so the per-level `configuration_table` entry decides it. Which positions
//!   enter the chains changes which matches *later* positions can find, so this branch perturbs
//!   the whole rest of the stream -- it is not a local optimisation.
//! * **The rolling-hash reseed** in the other branch (L1919-L1921): after skipping a long match
//!   the key is rebuilt from the two bytes at the new `strstart`, which is
//!   [`crate::deflate::hash_chain::seed_hash`]. C notes at L1923-L1925 that a short lookahead
//!   leaves `ins_h` "garbage, but it does not matter since it will be recomputed at next deflate
//!   call".
//!
//! # The insertion loop, exactly as C counts it
//!
//! ```text
//! s->match_length--;           /* string at strstart already in table */
//! do {
//!     s->strstart++;
//!     INSERT_STRING(s, s->strstart, hash_head);
//! } while (--s->match_length != 0);
//! s->strstart++;
//! ```
//!
//! -- L1908-L1916. The pre-decrement accounts for the position already inserted at the top of
//! the iteration, so the body runs `match_length - 1` times and the trailing `strstart++`
//! supplies the last step: the cursor advances by exactly `match_length` either way, and
//! `match_length` is left at zero. Restructuring this into a plain `for` over `match_length`
//! positions would insert one string too many.
//!
//! # Safety posture
//!
//! No `unsafe`, no raw pointer, no unchecked index. `check_match` (L1894) and the `Tracevv` at
//! L1933 are `ZLIB_DEBUG`-only in C and have no counterpart; every window read here goes through
//! [`crate::weak_slice::Window::byte`], and every cursor update saturates, so no arithmetic in
//! this file can panic.

use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::hash_chain::{insert_string, insert_string_no_head, seed_hash};
use crate::deflate::longest_match::longest_match;
use crate::deflate::state::{Allocator, DeflateState, IPos, MIN_LOOKAHEAD, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Compresses greedily: emit each match as soon as it is found.
///
/// Port of `deflate_fast` (`deflate.c` L1857-L1954).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] -- the lookahead is short under `Z_NO_FLUSH`, or the output buffer
///   filled while flushing a block.
/// * [`BlockState::FinishStarted`] -- `Z_FINISH` was requested and the final block did not fit.
/// * [`BlockState::FinishDone`] -- `Z_FINISH` was requested and the final block was written.
/// * [`BlockState::BlockDone`] -- a block boundary was reached for one of the other flush modes.
// `_tr_tally` keeps the C spelling of `deflate.h` L317; see `trees/mod.rs` for the same
// relaxation.
#[allow(clippy::used_underscore_items)]
pub(crate) fn deflate_fast<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {`  (L1861)
    loop {
        // "Make sure that we always have enough lookahead, except at the end of the input
        // file. We need MAX_MATCH bytes for the next match, plus MIN_MATCH bytes to insert the
        // string following the next match."  (L1862-L1872)
        //
        // ```text
        // if (s->lookahead < MIN_LOOKAHEAD) {
        //     fill_window(s);
        //     if (s->lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH) {
        //         return need_more;
        //     }
        //     if (s->lookahead == 0) break; /* flush the current block */
        // }
        // ```
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
        // hash_head to the head of the hash chain"  (L1874-L1880)
        //
        // ```text
        // hash_head = NIL;
        // if (s->lookahead >= MIN_MATCH) {
        //     INSERT_STRING(s, s->strstart, hash_head);
        // }
        // ```
        let mut hash_head = IPos::NIL;
        if state.window.lookahead >= MIN_MATCH {
            hash_head = insert_string(state, state.window.strstart);
        }

        // "Find the longest match, discarding those <= prev_length. At this point we have
        // always match_length < MIN_MATCH"  (L1882-L1892)
        //
        // ```text
        // if (hash_head != NIL && s->strstart - hash_head <= MAX_DIST(s)) {
        //     s->match_length = longest_match (s, hash_head);
        //     /* longest_match() sets match_start */
        // }
        // ```
        //
        // `match_index` is `hash_head != NIL` plus the window-index-0 exclusion; the
        // subtraction cannot wrap because a chain head is always an older position than
        // `strstart`, so `saturating_sub` agrees exactly with C's unsigned arithmetic.
        if let Some(head_index) = hash_head.match_index() {
            if state.window.strstart.saturating_sub(head_index) <= state.max_dist() {
                state.match_length = longest_match(state, hash_head);
            }
        }

        // `if (s->match_length >= MIN_MATCH) {`  (L1893)
        let bflush = if state.match_length >= MIN_MATCH {
            // `check_match(s, s->strstart, s->match_start, (int)s->match_length);` (L1894) is
            // `ZLIB_DEBUG`-only and has no counterpart.
            //
            // `_tr_tally_dist(s, s->strstart - s->match_start, s->match_length - MIN_MATCH,
            //                 bflush);`  (L1896-L1897)
            //
            // The distance is at most `MAX_DIST(s)` (32 506 for a 32 KiB window) so it fits a
            // `u16`, and the length operand is at most `MAX_MATCH - MIN_MATCH` (255) so it fits
            // a `u8`. Both saturating fallbacks are unreachable and are the ceilings the
            // conversions would produce anyway.
            let distance = state
                .window
                .strstart
                .saturating_sub(state.window.match_start);
            let bflush = _tr_tally(
                state,
                u16::try_from(distance).unwrap_or(u16::MAX),
                u8::try_from(state.match_length.saturating_sub(MIN_MATCH)).unwrap_or(u8::MAX),
            );

            // `s->lookahead -= s->match_length;`  (L1899)
            state.window.lookahead = state.window.lookahead.saturating_sub(state.match_length);

            // "Insert new strings in the hash table only if the match length is not too large.
            // This saves time but degrades compression."  (L1901-L1918)
            //
            // The `#ifndef FASTEST` guard around this branch (L1904 and L1917) selects the
            // shipped configuration, which is the one ported; `FASTEST` has no counterpart
            // anywhere in this crate.
            if state.match_length <= state.max_insert_length()
                && state.window.lookahead >= MIN_MATCH
            {
                // `s->match_length--; do { s->strstart++; INSERT_STRING(...); }
                //  while (--s->match_length != 0); s->strstart++;`  (L1908-L1916)
                //
                // See the module documentation for why the count is `match_length - 1` body
                // executions plus one trailing step. `insert_string_no_head` is
                // `INSERT_STRING` with the chain head discarded, which is what C does here: it
                // assigns `hash_head` and never reads it again.
                state.match_length = state.match_length.saturating_sub(1);
                while state.match_length != 0 {
                    state.window.strstart = state.window.strstart.saturating_add(1);
                    insert_string_no_head(state, state.window.strstart);
                    state.match_length = state.match_length.saturating_sub(1);
                }
                state.window.strstart = state.window.strstart.saturating_add(1);
            } else {
                // ```text
                // s->strstart += s->match_length;
                // s->match_length = 0;
                // s->ins_h = s->window[s->strstart];
                // UPDATE_HASH(s, s->ins_h, s->window[s->strstart + 1]);
                // ```
                // -- L1919-L1921. The two statements that rebuild the rolling key are exactly
                // `seed_hash`; `MIN_MATCH` is 3, so the `#if MIN_MATCH != 3` escape hatch at
                // L1922-L1924 is inert, as it is in the reference build.
                state.window.strstart = state.window.strstart.saturating_add(state.match_length);
                state.match_length = 0;
                seed_hash(state, state.window.strstart);
            }

            bflush
        } else {
            // "No match, output a literal byte"  (L1931-L1937)
            //
            // ```text
            // _tr_tally_lit(s, s->window[s->strstart], bflush);
            // s->lookahead--;
            // s->strstart++;
            // ```
            //
            // The read cannot fail: `lookahead` is non-zero on every path that reaches here.
            // Declining to emit leaves the block untouched and falls through to the flush
            // decision, rather than inventing a literal.
            let Some(literal) = state.window.byte(state.window.strstart) else {
                break;
            };
            let bflush = _tr_tally(state, 0, literal);

            state.window.lookahead = state.window.lookahead.saturating_sub(1);
            state.window.strstart = state.window.strstart.saturating_add(1);

            bflush
        };

        // `if (bflush) FLUSH_BLOCK(s, 0);`  (L1938)
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;`  (L1940)
    //
    // How many strings at the tail of the window still have to be inserted once more input
    // arrives. Two at most, because a three-byte key needs three bytes.
    state.window.insert = state.window.strstart.min(MIN_MATCH.saturating_sub(1));

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L1941-L1944)
    if matches!(flush, Flush::Finish) {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L1945-L1946)
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L1947)
    BlockState::BlockDone
}
