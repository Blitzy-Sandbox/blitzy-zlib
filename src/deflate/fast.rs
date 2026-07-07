//! `deflate_fast` — the greedy (non-lazy) match finder.
//!
//! Safe-Rust port of the C `deflate_fast` function (`deflate.c` L1857-L1948)
//! from **zlib 1.3.2.1-motley**. This is the block producer selected for
//! compression levels 1-3 ([`CompressFunc::Fast`](crate::deflate::strategy::CompressFunc::Fast)).
//! It performs a single *greedy* search at each input position and takes the
//! first match it finds, in contrast to the lazy-evaluating `deflate_slow`
//! used at levels 4-9, which may defer a match by one byte for a better result.
//!
//! # Byte-for-byte compatibility
//!
//! The emitted token stream — and therefore the compressed bytes — must be
//! identical to reference zlib for the same input, level and strategy
//! (AAP §0.6.4). Every decision that influences which symbols are tallied is
//! reproduced exactly:
//!
//! * a match is searched for only when the hash chain is non-empty *and* the
//!   candidate distance is within [`DeflateState::max_dist`];
//! * after a match is emitted, the strings covered by the matched bytes are
//!   re-inserted into the hash table only when the match is short enough
//!   (`match_length <= max_insert_length`) and enough lookahead remains — this
//!   is what lets later matches reference interior positions of an earlier
//!   match, and omitting it would change the token stream;
//! * the trailing [`DeflateState::insert`] bookkeeping value is computed with
//!   the same `strstart < MIN_MATCH - 1 ? strstart : MIN_MATCH - 1` rule.
//!
//! # Safety
//!
//! This module contains **zero** `unsafe` (AAP §0.6.2, §0.7.2): all window and
//! buffer access goes through bounds-checked indexing, and all cleanup is
//! handled by ownership in [`DeflateState`]. The two `usize` subtractions that
//! could in principle underflow — `strstart - hash_head` and
//! `strstart - match_start` — are only evaluated once their operands are known
//! to satisfy the ordering invariant, as documented at each call site. The
//! window reads in the "match too long" branch are proven in-bounds by the
//! `fill_window` invariant `strstart <= window_size - MIN_LOOKAHEAD`
//! (`deflate.c` L247, L374): since `match_length <= MAX_MATCH` and
//! `MIN_LOOKAHEAD == MAX_MATCH + MIN_MATCH + 1`, the advanced `strstart` never
//! exceeds `window_size - 4`, so `window[strstart + 1]` stays within the
//! `2 * w_size` window.
//!
//! # `no_std`
//!
//! The module relies only on `core` and the crate's internal modules; it needs
//! no direct heap allocation of its own.

use crate::constants::{Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{DeflateState, IoContext, MIN_LOOKAHEAD, MIN_MATCH, NIL};
use crate::deflate::strategy::BlockState;
use crate::deflate::trees;

/// Flush the current block and, when the output buffer is full, signal the
/// deflate driver to return to its caller.
///
/// This is the shared translation of the C `FLUSH_BLOCK` macro
/// (`deflate.c` L1641-L1644), which itself wraps `FLUSH_BLOCK_ONLY`
/// (`deflate.c` L1629-L1638). It:
///
/// 1. flushes the accumulated symbols as a DEFLATE block via
///    [`trees::_tr_flush_block`], passing the window offset of the block start
///    (or `None` when `block_start` is negative — i.e. the block's data lives
///    before the window origin after a slide);
/// 2. advances `block_start` to the current `strstart`;
/// 3. drains as many pending bytes as fit into the caller's output buffer with
///    [`DeflateState::flush_pending`].
///
/// It returns `Some(state)` when the output buffer has been completely filled
/// (`avail_out == 0`), meaning the producer must stop and hand control back:
/// [`BlockState::FinishStarted`] when this was the final block (`last == true`),
/// or [`BlockState::NeedMore`] otherwise. It returns `None` when output space
/// remains and compression may continue.
fn flush_block(s: &mut DeflateState, io: &mut IoContext, last: bool) -> Option<BlockState> {
    // FLUSH_BLOCK_ONLY: emit the block covering `window[block_start .. strstart]`.
    // C selects `&window[block_start]` when `block_start >= 0`, else `Z_NULL`;
    // the Rust `_tr_flush_block` takes that window position as `Option<usize>`.
    let buf = if s.block_start >= 0 {
        Some(s.block_start as usize)
    } else {
        None
    };
    // `stored_len = strstart - block_start`, computed in signed space exactly
    // like C's `(long)strstart - block_start`. `block_start <= strstart` always
    // holds, so the difference is non-negative and the `usize` cast is exact.
    let stored_len = (s.strstart as isize - s.block_start) as usize;
    trees::_tr_flush_block(s, buf, stored_len, last);
    s.block_start = s.strstart as isize;
    s.flush_pending(io);

    // FLUSH_BLOCK tail: a completely full output buffer forces a premature
    // return so the caller can drain output and re-enter.
    if io.avail_out == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}

/// Compress as much of the input as possible using a greedy match search,
/// returning the resulting [`BlockState`].
///
/// Faithful safe-Rust port of C `deflate_fast` (`deflate.c` L1857-L1948), the
/// block producer used for compression levels 1-3. Unlike `deflate_slow`, it
/// performs no lazy evaluation: the first match found at the current position
/// is adopted immediately. New strings are inserted into the hash table for
/// unmatched bytes and for the interior bytes of short matches only, which is
/// what keeps the fast levels fast while remaining byte-compatible with
/// reference zlib.
///
/// # Parameters
///
/// * `s` — the deflate state (sliding window, hash chains, symbol buffer,
///   Huffman trees, and all bookkeeping counters).
/// * `io` — the streaming I/O context (input cursor, output buffer, running
///   checksum, and byte counters).
/// * `flush` — the flush mode requested by the current `deflate` call. Only
///   [`Z_NO_FLUSH`] (keep buffering) and [`Z_FINISH`] (finish the stream) alter
///   the control flow here, exactly as in the C source.
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — more input, or more output space, is required
///   before progress can continue.
/// * [`BlockState::BlockDone`] — a block boundary was reached for a
///   non-finishing flush and all pending symbols were emitted.
/// * [`BlockState::FinishStarted`] — finishing began but the output buffer
///   filled before the final block could be completed.
/// * [`BlockState::FinishDone`] — the stream was fully finished.
pub fn deflate_fast(s: &mut DeflateState, io: &mut IoContext, flush: i32) -> BlockState {
    loop {
        // Make sure we always have enough lookahead, except at the end of the
        // input file. We need MAX_MATCH bytes for the next match, plus
        // MIN_MATCH bytes to insert the string following the next match.
        if s.lookahead < MIN_LOOKAHEAD {
            s.fill_window(io);
            if s.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BlockState::NeedMore;
            }
            if s.lookahead == 0 {
                break; // flush the current block
            }
        }

        // Insert the string `window[strstart .. strstart + MIN_MATCH]` into the
        // hash table and set `hash_head` to the head of its chain. The
        // insertion has a side effect (linking the current position into its
        // chain), so it must run whenever there are at least MIN_MATCH bytes of
        // lookahead — exactly as the C `INSERT_STRING` macro does. When fewer
        // than MIN_MATCH bytes remain, no string is inserted and `hash_head`
        // stays [`NIL`].
        let hash_head = if s.lookahead >= MIN_MATCH {
            usize::from(s.insert_string(s.strstart))
        } else {
            usize::from(NIL)
        };

        // Find the longest match, discarding those no longer than the previous
        // one. At this point `match_length < MIN_MATCH` unless `longest_match`
        // sets it below. The search runs only when a chain exists and the
        // candidate is within MAX_DIST.
        //
        // `hash_head` is either NIL or a window position previously recorded by
        // `insert_string`, which is always `<= strstart`; the `&&`
        // short-circuit means `strstart - hash_head` is evaluated only when
        // `hash_head != NIL`, so the subtraction can never underflow.
        if hash_head != usize::from(NIL) && s.strstart - hash_head <= s.max_dist() {
            // `longest_match` sets `s.match_start` and caps the returned length
            // at the available lookahead.
            s.match_length = s.longest_match(hash_head);
        }

        let bflush = if s.match_length >= MIN_MATCH {
            // Emit a (distance, length) match. `match_start < strstart` is
            // guaranteed by `longest_match` (a match is always a back
            // reference), so `strstart - match_start` cannot underflow. The
            // length-code argument is `match_length - MIN_MATCH`, which lies in
            // `0..=MAX_MATCH - MIN_MATCH` (i.e. `0..=255`) and so fits in `u8`.
            let dist = s.strstart - s.match_start;
            let len = (s.match_length - MIN_MATCH) as u8;
            let bflush = trees::_tr_tally_dist(s, dist, len);

            s.lookahead -= s.match_length;

            // Insert new strings in the hash table only if the match length is
            // not too large. This saves time but slightly degrades
            // compression, matching reference zlib at these levels.
            if s.match_length <= s.max_insert_length() && s.lookahead >= MIN_MATCH {
                // The string at `strstart` is already in the table, so re-insert
                // the next `match_length - 1` positions. Faithful port of C's
                // `match_length--; do { strstart++; INSERT_STRING(...); }
                // while (--match_length != 0); strstart++;`.
                s.match_length -= 1; // string at strstart already in table
                loop {
                    s.strstart += 1;
                    // The returned chain head is unused here (the C macro stores
                    // it into `hash_head`, which is never read again this
                    // iteration); only the insertion side effect matters.
                    s.insert_string(s.strstart);
                    // `strstart` never exceeds `window_size - MIN_LOOKAHEAD`
                    // here, so there are always MIN_MATCH bytes ahead to hash.
                    s.match_length -= 1;
                    if s.match_length == 0 {
                        break;
                    }
                }
                s.strstart += 1;
            } else {
                // The match is too long to re-insert, or too little lookahead
                // remains. Advance past the match and prime the rolling hash
                // from the two bytes at the new position without inserting a
                // full string.
                s.strstart += s.match_length;
                s.match_length = 0;
                s.ins_h = usize::from(s.window[s.strstart]);
                s.update_hash(s.window[s.strstart + 1]);
                // With MIN_MATCH == 3 only two bytes are folded into the hash
                // here (matching C, whose `#if MIN_MATCH != 3` branch is never
                // compiled). If `lookahead < MIN_MATCH` the resulting `ins_h` is
                // garbage, but it does not matter: it is recomputed at the next
                // `deflate` call.
            }
            bflush
        } else {
            // No match: output a single literal byte.
            let cc = s.window[s.strstart];
            let bflush = trees::_tr_tally_lit(s, cc);
            s.lookahead -= 1;
            s.strstart += 1;
            bflush
        };

        if bflush {
            if let Some(rc) = flush_block(s, io, false) {
                return rc;
            }
        }
    }

    // Record how many trailing bytes still need to be inserted into the hash on
    // the next call: `strstart < MIN_MATCH - 1 ? strstart : MIN_MATCH - 1`.
    s.insert = if s.strstart < MIN_MATCH - 1 {
        s.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == Z_FINISH {
        // Final block: a full output buffer means finishing has only started.
        if flush_block(s, io, true).is_some() {
            return BlockState::FinishStarted;
        }
        return BlockState::FinishDone;
    }

    if s.sym_next != 0 {
        if let Some(rc) = flush_block(s, io, false) {
            return rc;
        }
    }

    BlockState::BlockDone
}
