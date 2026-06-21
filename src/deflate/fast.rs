//! Greedy LZ77 matching — the `deflate_fast` strategy (compression levels 1–3).
//!
//! This is a direct, behavior-preserving port of C zlib's `deflate_fast()`
//! (`deflate.c` L1857-1948). It implements the *greedy* match strategy: at each
//! window position it asks [`DeflateStream::longest_match`] for the longest
//! match starting there and, if that match is at least [`MIN_MATCH`] bytes long,
//! emits a `(distance, length)` pair; otherwise it emits a single literal byte.
//! Unlike [`deflate_slow`](crate::deflate::slow), it performs **no** lazy
//! evaluation — it never defers a match to check whether the next position
//! yields a better one. The per-level search parameters (`good_length`,
//! `max_lazy`, `nice_length`, `max_chain`) come from
//! [`CONFIG_TABLE`](crate::deflate::strategy) rows 1–3.
//!
//! # Byte-exact parity (AAP §0.6.6, §0.7.1)
//!
//! Every compressed byte this function ultimately produces must be identical to
//! the C implementation for the same input/level/strategy/window. That fidelity
//! hinges on three details, each reproduced here verbatim from C:
//!
//! 1. **Match-acceptance guard.** `match_length` is recomputed *only* inside the
//!    `hash_head != NIL && strstart - hash_head <= MAX_DIST` guard. On the
//!    literal path it deliberately keeps whatever value the previous guarded
//!    pass left in it — exactly as C does (C only assigns
//!    `s->match_length = longest_match(...)` inside the `if`). No `else` branch
//!    resets it.
//! 2. **The insert-string `do { … } while(--match_length)` loop.** The match
//!    length is decremented *before* the zero test (a C `do`/`while`), so after
//!    the initial `match_length -= 1` ("the string at `strstart` is already in
//!    the table") the body runs `match_length` times. Each iteration inserts the
//!    next string into the hash chains for their side effects; the previous
//!    head returned by [`insert_string`](crate::deflate::state::DeflateState::insert_string)
//!    is discarded (C sets `hash_head` there but never reads it).
//! 3. **The two-byte post-match hash update.** When the match is *not* inserted
//!    string-by-string, only `ins_h = window[strstart]` followed by a single
//!    `UPDATE_HASH(window[strstart + 1])` is performed (`MIN_MATCH == 3`, so the
//!    third byte is folded in by the next `INSERT_STRING`). This is not strictly
//!    necessary for correctness but is required to match the reference output.
//!
//! # Constraints
//!
//! * **No `unsafe`** (AAP §0.6.2): the entire body is safe Rust — pointer
//!   arithmetic becomes slice indexing and the raw state pointer becomes the
//!   borrowed [`DeflateStream`].
//! * **`no_std`-clean**: only `core`/the borrowed buffers are used here.

use crate::constants::FlushMode;
use crate::deflate::flush_block;
use crate::deflate::state::{BlockState, DeflateStream, MIN_LOOKAHEAD, MIN_MATCH, NIL, max_dist};
use crate::deflate::trees;

/// Compress as much of the input as possible using greedy matching, returning
/// the resulting [`BlockState`].
///
/// This conforms to the `CompressFn` contract shared by all five strategy
/// functions: `fn(&mut DeflateStream, FlushMode) -> BlockState`. It is selected
/// for compression levels 1–3 via the strategy dispatch table.
///
/// Ports `deflate_fast()` (`deflate.c` L1857-1948).
pub(crate) fn deflate_fast(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // Head of the hash chain for the string currently at `strstart`.
    let mut hash_head: u16;
    // Set by the symbol-tally helpers when the current block must be flushed.
    let mut bflush: bool;

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
                break; // flush the current block
            }
        }

        // Insert the string window[strstart .. strstart + 2] in the dictionary,
        // and set `hash_head` to the head of the hash chain.
        hash_head = NIL;
        if s.state.lookahead >= MIN_MATCH {
            hash_head = s.state.insert_string(s.state.strstart);
        }

        // Find the longest match, discarding those <= prev_length. At this
        // point we always have match_length < MIN_MATCH.
        if hash_head != NIL && s.state.strstart - (hash_head as usize) <= max_dist(s.state.w_size) {
            // To simplify the code, we prevent matches with the string of
            // window index 0 (in particular we have to avoid a match of the
            // string with itself at the start of the input file).
            s.state.match_length = s.longest_match(hash_head as usize);
            // longest_match() sets s.state.match_start.
        }
        if s.state.match_length >= MIN_MATCH {
            // check_match() is a debug-only assertion in C; omitted here.

            // Tally a (distance, length) match. The length argument is
            // `match_length - MIN_MATCH` in 0..=255 (fits a u8); the distance is
            // the full `strstart - match_start`. Both are computed into locals
            // first so the `&mut DeflateState` borrow for the tally call does
            // not overlap the shared reads of the same state.
            let dist = s.state.strstart - s.state.match_start;
            let len = (s.state.match_length - MIN_MATCH) as u8;
            bflush = trees::tr_tally_dist(s.state, dist, len);

            s.state.lookahead -= s.state.match_length;

            // Insert new strings in the hash table only if the match length is
            // not too large. This saves time but degrades compression.
            // (`max_lazy_match` doubles as C's `max_insert_length`.)
            if s.state.match_length <= s.state.max_lazy_match as usize
                && s.state.lookahead >= MIN_MATCH
            {
                s.state.match_length -= 1; // string at strstart already in table
                // C `do { strstart++; INSERT_STRING(...); } while(--match_length)`:
                // decrement happens before the zero test, so this runs the body
                // `match_length` times.
                loop {
                    s.state.strstart += 1;
                    // Insert for side effects only; the returned previous chain
                    // head is intentionally discarded (C sets `hash_head` here
                    // but never reads it). strstart never exceeds
                    // WSIZE - MAX_MATCH, so there are always MIN_MATCH bytes
                    // ahead.
                    s.state.insert_string(s.state.strstart);
                    s.state.match_length -= 1;
                    if s.state.match_length == 0 {
                        break;
                    }
                }
                s.state.strstart += 1;
            } else {
                s.state.strstart += s.state.match_length;
                s.state.match_length = 0;
                s.state.ins_h = s.state.window[s.state.strstart] as usize;
                let next_byte = s.state.window[s.state.strstart + 1];
                s.state.ins_h = s.state.update_hash(s.state.ins_h, next_byte);
                // UPDATE_HASH is intentionally not called for the third byte
                // (MIN_MATCH == 3); the next INSERT_STRING folds it in. If
                // lookahead < MIN_MATCH, `ins_h` is garbage, but it does not
                // matter since it is recomputed at the next deflate() call.
            }
        } else {
            // No match: output a single literal byte.
            let lc = s.state.window[s.state.strstart];
            bflush = trees::tr_tally_lit(s.state, lc);
            s.state.lookahead -= 1;
            s.state.strstart += 1;
        }
        if bflush {
            if let Some(bs) = flush_block(s, false) {
                return bs;
            }
        }
    }

    // Remember how many bytes at the end of the window still need to be
    // inserted into the hash table on the next call.
    s.state.insert = if s.state.strstart < MIN_MATCH - 1 {
        s.state.strstart
    } else {
        MIN_MATCH - 1
    };
    if flush == FlushMode::Finish {
        if let Some(bs) = flush_block(s, true) {
            return bs;
        }
        return BlockState::FinishDone;
    }
    if s.state.sym_next != 0 {
        if let Some(bs) = flush_block(s, false) {
            return bs;
        }
    }
    BlockState::BlockDone
}
