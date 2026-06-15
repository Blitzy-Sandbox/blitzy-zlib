//! `deflate_slow` — the lazy-matching DEFLATE strategy for levels 4-9.
//!
//! This module ports the C `deflate_slow()` function (`deflate.c`,
//! L1956-2076) verbatim. It is the **default** compression path: the
//! `CONFIG_TABLE` selects `deflate_slow` for every level from 4 through 9, and
//! `Z_DEFAULT_COMPRESSION` (-1) resolves to level 6, so the overwhelming
//! majority of real-world `deflate()` calls run through this function. That
//! makes it the single most important correctness gate for byte-identical
//! output (AAP §0.6.6, §0.6.7, §0.7.1).
//!
//! # Lazy matching
//!
//! Unlike the greedy [`deflate_fast`](super::fast) strategy, the lazy matcher
//! defers the decision to emit each match by one byte: after finding a match at
//! position `strstart`, it advances one byte and asks whether the *next*
//! position yields a strictly longer match. If it does, the previous match is
//! truncated to a single literal and the longer match is taken; otherwise the
//! previous match is emitted. This classic zlib lazy-evaluation produces denser
//! output at the cost of one extra `longest_match` probe per position.
//!
//! The deferral is carried across loop iterations by three pieces of engine
//! state — `prev_length`, `prev_match`, and the `match_available` flag — which
//! must be threaded exactly as the C code does for the emitted bitstream to
//! match canonical zlib byte-for-byte.
//!
//! # Byte-exact behaviours (must match C exactly)
//!
//! Four subtle behaviours determine the emitted bytes and are reproduced here
//! literally; none may be "simplified":
//!
//! 1. **The lazy decision** `prev_length >= MIN_MATCH && match_length <=
//!    prev_length`. The `<=` (not `<`) is intentional: on a tie the *earlier*
//!    (already-deferred) match wins, which avoids the longer-distance match and
//!    changes the output.
//! 2. **Short-match suppression** for `match_length <= 5`: under
//!    [`Strategy::Filtered`] *any* such match is dropped, and otherwise a
//!    length-`MIN_MATCH` match farther than [`TOO_FAR`] (4096) bytes away is
//!    dropped. Either case resets `match_length` to `MIN_MATCH - 1`.
//! 3. **Deferred-literal handling** via `match_available`: a single literal is
//!    emitted one position late, and a final deferred literal is flushed in the
//!    epilogue.
//! 4. **Flush ordering** in the `match_available` branch: it uses
//!    `FLUSH_BLOCK_ONLY` (flush without an early return), *then* advances
//!    `strstart`/`lookahead`, *then* returns [`BlockState::NeedMore`] if the
//!    output filled up. The ordering is behaviour-critical.
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; `unsafe`
//!   lives only in `crate::ffi` and `crate::inflate::fast` (AAP §0.6.2). This
//!   file contains zero `unsafe`, raw pointers, `transmute`, or `MaybeUninit`.
//! * **`no_std`-clean.** Only `crate`-internal items and `core` primitives are
//!   referenced; `std` is never named.

use crate::constants::{FlushMode, Strategy};
use crate::deflate::state::{BlockState, DeflateStream, MIN_LOOKAHEAD, MIN_MATCH, NIL, TOO_FAR};
use crate::deflate::{flush_block, flush_block_only};

/// Compress with lazy matching, the strategy used for levels 4-9 (and the
/// default level 6). Direct port of C `deflate_slow()` (`deflate.c`
/// L1956-2076).
///
/// `flush` is the flush mode passed to the enclosing `deflate()` call; it
/// governs whether the function may stall waiting for more input
/// ([`FlushMode::NoFlush`]) or must drain the current block
/// ([`FlushMode::Finish`] and the trailing-block logic). The return value tells
/// the `deflate()` driver how far the block-level work progressed:
///
/// * [`BlockState::NeedMore`] — ran out of input or output mid-block.
/// * [`BlockState::FinishDone`] — the final block was fully emitted.
/// * [`BlockState::FinishStarted`] / [`BlockState::NeedMore`] — propagated from
///   `flush_block` when the output buffer filled before the block completed.
/// * [`BlockState::BlockDone`] — a block boundary was reached for a non-final
///   flush.
pub(crate) fn deflate_slow(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // Head of the hash chain for the current string (C `IPos hash_head`).
    let mut hash_head: u16;
    // Set when the current block must be flushed (C `int bflush`).
    let mut bflush: bool;

    // Process the input block.
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
        // and set hash_head to the head of the hash chain.
        hash_head = NIL;
        if s.state.lookahead >= MIN_MATCH {
            hash_head = s.state.insert_string(s.state.strstart);
        }

        // Find the longest match, discarding those <= prev_length.
        s.state.prev_length = s.state.match_length;
        s.state.prev_match = s.state.match_start as u16;
        s.state.match_length = MIN_MATCH - 1;

        // `strstart - hash_head` mirrors C's unsigned `IPos` subtraction:
        // `hash_head` is always an earlier window position (`< strstart`) when
        // it is not `NIL`, so `wrapping_sub` equals an ordinary subtraction
        // here; should it ever exceed `strstart` the wrap produces a huge value
        // that fails the `<= MAX_DIST` test, exactly as the C unsigned compare
        // does, and never panics.
        if hash_head != NIL
            && s.state.prev_length < s.state.max_lazy_match
            && s.state.strstart.wrapping_sub(hash_head as usize) <= s.state.w_size - MIN_LOOKAHEAD
        {
            // To simplify the code, we prevent matches with the string of
            // window index 0 (in particular we have to avoid a match of the
            // string with itself at the start of the input file).
            // `longest_match()` sets `match_start`.
            s.state.match_length = s.longest_match(hash_head as usize);

            // Ignore a length-3 match if it is too distant, or any short match
            // under Z_FILTERED. (`TOO_FAR <= 32767`, so the distance arm is
            // always compiled in, matching the C `#if`.) `strstart -
            // match_start` again mirrors C unsigned arithmetic: when
            // `prev_match` is also `MIN_MATCH` the C comment notes `match_start`
            // may be garbage, but the match is ignored regardless, so the wrap
            // is harmless and panic-free.
            if s.state.match_length <= 5
                && (s.state.strategy == Strategy::Filtered as i32
                    || (s.state.match_length == MIN_MATCH
                        && s.state.strstart.wrapping_sub(s.state.match_start) > TOO_FAR))
            {
                s.state.match_length = MIN_MATCH - 1;
            }
        }

        // If there was a match at the previous step and the current match is
        // not better, output the previous match.
        if s.state.prev_length >= MIN_MATCH && s.state.match_length <= s.state.prev_length {
            // Do not insert strings in the hash table beyond this.
            let max_insert = s.state.strstart + s.state.lookahead - MIN_MATCH;

            // Emit the deferred match. The distance and length-code are computed
            // from the pre-mutation state, exactly where C evaluates the
            // `_tr_tally_dist` arguments, before `lookahead`/`prev_length`/
            // `strstart` are advanced below. The distance is a true (non-wrapping)
            // value: `prev_match` is the earlier `match_start` of a real match at
            // `strstart - 1`, so `strstart - 1 - prev_match >= 1`.
            let dist = s.state.strstart - 1 - (s.state.prev_match as usize);
            let len = (s.state.prev_length - MIN_MATCH) as u8;
            bflush = s.state.tr_tally_dist(dist, len);

            // Insert in the hash table all strings up to the end of the match.
            // strstart - 1 and strstart are already inserted. If there is not
            // enough lookahead, the last two strings are not inserted. This
            // reproduces the C `do { ... } while (--prev_length != 0)` exactly:
            // the initial `prev_length -= 2` plus the loop's trailing decrement
            // and zero-test match the post-decrement `while` condition, and the
            // final `strstart += 1` matches the post-loop `s->strstart++`.
            s.state.lookahead -= s.state.prev_length - 1;
            s.state.prev_length -= 2;
            loop {
                s.state.strstart += 1;
                if s.state.strstart <= max_insert {
                    // The returned chain head is intentionally discarded here;
                    // the next outer iteration recomputes `hash_head`.
                    s.state.insert_string(s.state.strstart);
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
                // C `FLUSH_BLOCK(s, 0)`: flush and, if the output filled, return.
                if let Some(bs) = flush_block(s, false) {
                    return bs;
                }
            }
        } else if s.state.match_available {
            // There was no match at the previous position (or the current match
            // is longer): output the single deferred literal. `strstart >= 1`
            // here because `match_available` is only set after `strstart` was
            // advanced at least once, so `strstart - 1` never underflows.
            let lc = s.state.window[s.state.strstart - 1];
            bflush = s.state.tr_tally_lit(lc);
            if bflush {
                // C `FLUSH_BLOCK_ONLY(s, 0)`: flush WITHOUT an early return. The
                // `avail_out == 0` check below — after advancing the cursors —
                // is what may return, and this ordering is byte/behaviour
                // critical.
                flush_block_only(s, false);
            }
            s.state.strstart += 1;
            s.state.lookahead -= 1;
            if s.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // There is no previous match to compare with; wait for the next step
            // to decide.
            s.state.match_available = true;
            s.state.strstart += 1;
            s.state.lookahead -= 1;
        }
    }

    // The loop only breaks once all input is consumed (`lookahead == 0`), which
    // under `Z_NO_FLUSH` is unreachable (we return `NeedMore` first); this
    // mirrors the C `Assert(flush != Z_NO_FLUSH, "no flush?")`.
    debug_assert!(flush != FlushMode::NoFlush, "no flush?");

    // Emit any remaining deferred literal.
    if s.state.match_available {
        let lc = s.state.window[s.state.strstart - 1];
        // The flush flag is intentionally discarded: the trailing `flush_block`
        // below drains the block regardless.
        let _ = s.state.tr_tally_lit(lc);
        s.state.match_available = false;
    }

    // Record how many bytes at the window end still need hashing on the next
    // call (C `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1`).
    s.state.insert = if s.state.strstart < MIN_MATCH - 1 {
        s.state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == FlushMode::Finish {
        // C `FLUSH_BLOCK(s, 1); return finish_done;`
        if let Some(bs) = flush_block(s, true) {
            return bs;
        }
        return BlockState::FinishDone;
    }

    if s.state.sym_next != 0 {
        // C `if (s->sym_next) FLUSH_BLOCK(s, 0);`
        if let Some(bs) = flush_block(s, false) {
            return bs;
        }
    }

    BlockState::BlockDone
}
