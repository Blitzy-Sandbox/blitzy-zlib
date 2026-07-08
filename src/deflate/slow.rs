//! The `deflate_slow` lazy-match compressor (used by levels 4-9).
//!
//! This module is a faithful, **`100%` safe-Rust** port of the C
//! `deflate_slow` function (`deflate.c` L1955-L2076) together with the
//! `FLUSH_BLOCK` / `FLUSH_BLOCK_ONLY` macros (`deflate.c` L1630-L1645) that it
//! relies on. It is the highest-compression-ratio block producer in zlib and is
//! selected for compression levels 4 through 9 (see
//! [`CONFIGURATION_TABLE`](crate::deflate::strategy::CONFIGURATION_TABLE)).
//!
//! # Lazy evaluation
//!
//! Unlike [`deflate_fast`], `deflate_slow` uses *lazy* match evaluation: after
//! finding a match at the current position it does **not** emit it immediately.
//! Instead it advances one position and searches again; the earlier match is
//! only adopted (emitted) if the next position does not yield a strictly longer
//! match. This one-position deferral is what earns the extra compression, and it
//! is the single most subtle part of the algorithm — a pending literal is
//! carried in [`match_available`](crate::deflate::state::DeflateState::match_available)
//! and emitted one position late.
//!
//! # Byte-exact fidelity
//!
//! The produced DEFLATE token stream is **byte-identical** to reference zlib for
//! the same input, compression level, and strategy. Three details are
//! load-bearing for that guarantee and are reproduced exactly:
//!
//! 1. The distance of the *previous* match is `strstart - 1 - prev_match` (note
//!    the `- 1`), and its length code argument is `prev_length - MIN_MATCH`.
//! 2. The hash-insertion loop that runs after emitting a match uses the bound
//!    `max_insert = strstart + lookahead - MIN_MATCH` and only inserts while
//!    `strstart <= max_insert`, mirroring the C
//!    `do { if (++strstart <= max_insert) INSERT_STRING(...); } while (--prev_length != 0);`
//!    structure precisely (with the `prev_length -= 2` pre-decrement).
//! 3. The match filter — reject a match of length `<= 5` when the strategy is
//!    [`Strategy::Filtered`] *or* when it is exactly `MIN_MATCH` long and reaches
//!    farther than [`TOO_FAR`] (4096) bytes back — is what makes `Z_FILTERED`
//!    differ from the default strategy.
//!
//! # Safety
//!
//! This file contains **zero** `unsafe` code, enforced by the module-level
//! `#![deny(unsafe_code)]` below. All window and buffer access uses safe slice
//! indexing on the owned `Vec` buffers held by
//! [`DeflateState`], and match finding /
//! bit output are delegated to the safe methods on that state.
//!
//! [`deflate_fast`]: crate::deflate::fast
//! [`Strategy::Filtered`]: crate::constants::Strategy::Filtered

#![deny(unsafe_code)]

use crate::constants::{Strategy, Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{DeflateState, IoContext, MIN_LOOKAHEAD, MIN_MATCH, NIL, TOO_FAR};
use crate::deflate::strategy::BlockState;
use crate::deflate::trees;

/// Emits the current block and drains the pending output, **without** the
/// premature-exit check.
///
/// This is the port of the C `FLUSH_BLOCK_ONLY(s, last)` macro (`deflate.c`
/// L1630-L1640). It:
///
/// 1. Calls [`trees::_tr_flush_block`] with the window offset of the block start
///    (`None` when `block_start` is negative, matching the C `Z_NULL`), the
///    number of uncompressed input bytes in the block (`strstart - block_start`),
///    and the end-of-stream `last` flag.
/// 2. Advances `block_start` to the current `strstart`.
/// 3. Flushes as many pending bytes as possible into the caller's output buffer
///    via [`DeflateState::flush_pending`].
///
/// The IN assertion of the C macro (`strstart` is at the end of the current
/// match) is upheld by every call site below.
fn flush_block_only(s: &mut DeflateState, io: &mut IoContext, last: bool) {
    // C: (s->block_start >= 0L ? &s->window[s->block_start] : Z_NULL)
    let buf = if s.block_start >= 0 {
        Some(s.block_start as usize)
    } else {
        None
    };
    // C: (ulg)((long)s->strstart - s->block_start). `strstart >= block_start`
    // whenever `block_start >= 0`, and when `block_start` is negative the
    // difference is even larger, so the result is always non-negative.
    let stored_len = (s.strstart as isize - s.block_start) as usize;

    trees::_tr_flush_block(s, buf, stored_len, last);

    s.block_start = s.strstart as isize;
    s.flush_pending(io);
}

/// Emits the current block, drains pending output, and forces a premature exit
/// if the output buffer filled up.
///
/// This is the port of the C `FLUSH_BLOCK(s, last)` macro (`deflate.c`
/// L1642-L1645): it performs a [`flush_block_only`] and then, if no output space
/// remains, signals the caller to return early. The returned [`BlockState`] is
/// [`BlockState::FinishStarted`] when `last` is `true` and
/// [`BlockState::NeedMore`] otherwise, exactly matching the macro's
/// `return (last) ? finish_started : need_more;`.
///
/// Returns `None` when there is still output space, meaning the caller should
/// continue.
fn flush_block(s: &mut DeflateState, io: &mut IoContext, last: bool) -> Option<BlockState> {
    flush_block_only(s, io, last);
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

/// Compresses the input using lazy match evaluation (levels 4-9).
///
/// This is the safe-Rust port of the C `deflate_slow` function (`deflate.c`
/// L1955-L2076). It repeatedly:
///
/// 1. Refills the sliding window when the lookahead drops below
///    [`MIN_LOOKAHEAD`], returning [`BlockState::NeedMore`] if more input is
///    required and none is forthcoming (`flush == Z_NO_FLUSH`).
/// 2. Inserts the current string into the hash table and searches for the
///    longest match, discarding matches no longer than the previous one.
/// 3. Applies the `Z_FILTERED` / `TOO_FAR` match filter.
/// 4. Adopts the *previous* match (lazy deferral) when it was at least
///    [`MIN_MATCH`] long and the current match is no better, otherwise emits a
///    deferred single literal, otherwise defers the decision one more position.
///
/// On `Z_FINISH` it flushes the final block and returns
/// [`BlockState::FinishDone`] (or [`BlockState::FinishStarted`] if the output
/// buffer filled first). Otherwise it flushes any buffered symbols and returns
/// [`BlockState::BlockDone`].
///
/// # Parameters
///
/// * `s` — the live deflate state (window, hash tables, match bookkeeping, and
///   symbol/pending buffers).
/// * `io` — the input/output context wrapping the caller's `next_in`/`next_out`
///   buffers and the `avail_out` counter.
/// * `flush` — the flush mode from the current `deflate` call (`Z_NO_FLUSH`,
///   `Z_FINISH`, etc.).
///
/// # Byte-exactness
///
/// Every arithmetic expression, branch condition, and flush ordering is a
/// statement-by-statement translation of the C source, so the emitted token
/// stream is identical to reference zlib. See the module documentation for the
/// three load-bearing details.
pub fn deflate_slow(s: &mut DeflateState, io: &mut IoContext, flush: i32) -> BlockState {
    // Process the input block.
    loop {
        // Make sure that we always have enough lookahead, except at the end of
        // the input file. We need MAX_MATCH bytes for the next match, plus
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

        // Insert the string window[strstart .. strstart + 2] in the dictionary,
        // and set hash_head to the head of the hash chain.
        let mut hash_head: usize = NIL as usize;
        if s.lookahead >= MIN_MATCH {
            let str_idx = s.strstart;
            hash_head = s.insert_string(str_idx) as usize;
        }

        // Find the longest match, discarding those <= prev_length.
        s.prev_length = s.match_length;
        s.prev_match = s.match_start as u16;
        s.match_length = MIN_MATCH - 1;

        if hash_head != NIL as usize
            && s.prev_length < s.max_lazy_match
            && s.strstart - hash_head <= s.max_dist()
        {
            // To simplify the code, we prevent matches with the string of
            // window index 0 (in particular we have to avoid a match of the
            // string with itself at the start of the input file).
            //
            // `longest_match` sets `s.match_start` as a side effect.
            s.match_length = s.longest_match(hash_head);

            if s.match_length <= 5
                && (s.strategy == Strategy::Filtered
                    || (s.match_length == MIN_MATCH && s.strstart - s.match_start > TOO_FAR))
            {
                // If prev_match is also MIN_MATCH, match_start is garbage but we
                // will ignore the current match anyway.
                s.match_length = MIN_MATCH - 1;
            }
        }

        // If there was a match at the previous step and the current match is not
        // better, output the previous match.
        if s.prev_length >= MIN_MATCH && s.match_length <= s.prev_length {
            // Do not insert strings in hash table beyond this.
            let max_insert = s.strstart + s.lookahead - MIN_MATCH;

            // check_match(s, s.strstart - 1, s.prev_match, s.prev_length) is a
            // debug-only assertion in the C source (a no-op in non-debug
            // builds) and is intentionally omitted here.

            // Emit the previous match. Its distance is `strstart - 1 -
            // prev_match` (the match started one position back) and its length
            // code is `prev_length - MIN_MATCH`.
            let dist = s.strstart - 1 - (s.prev_match as usize);
            let len_code = (s.prev_length - MIN_MATCH) as u8;
            let bflush = trees::_tr_tally_dist(s, dist, len_code);

            // Insert in the hash table all strings up to the end of the match.
            // strstart - 1 and strstart are already inserted. If there is not
            // enough lookahead, the last two strings are not inserted in the
            // hash table.
            s.lookahead -= s.prev_length - 1;
            s.prev_length -= 2;
            loop {
                s.strstart += 1;
                if s.strstart <= max_insert {
                    let str_idx = s.strstart;
                    s.insert_string(str_idx);
                }
                s.prev_length -= 1;
                if s.prev_length == 0 {
                    break;
                }
            }
            s.match_available = false;
            s.match_length = MIN_MATCH - 1;
            s.strstart += 1;

            if bflush {
                if let Some(rc) = flush_block(s, io, false) {
                    return rc;
                }
            }
        } else if s.match_available {
            // If there was no match at the previous position, output a single
            // literal. If there was a match but the current match is longer,
            // truncate the previous match to a single literal.
            let cc = s.window[s.strstart - 1];
            let bflush = trees::_tr_tally_lit(s, cc);
            if bflush {
                // C uses FLUSH_BLOCK_ONLY here (not FLUSH_BLOCK): the block is
                // flushed but the premature-exit check is deferred until after
                // strstart/lookahead are advanced below.
                flush_block_only(s, io, false);
            }
            s.strstart += 1;
            s.lookahead -= 1;
            if io.avail_out == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // There is no previous match to compare with, wait for the next step
            // to decide.
            s.match_available = true;
            s.strstart += 1;
            s.lookahead -= 1;
        }
    }

    // C: Assert(flush != Z_NO_FLUSH, "no flush?"). Reaching this point requires
    // `lookahead == 0`, which under `Z_NO_FLUSH` would already have returned
    // `NeedMore`, so `flush` is necessarily not `Z_NO_FLUSH` here.
    debug_assert_ne!(flush, Z_NO_FLUSH, "no flush?");

    // Emit the last deferred literal, if any.
    if s.match_available {
        let cc = s.window[s.strstart - 1];
        let _ = trees::_tr_tally_lit(s, cc);
        s.match_available = false;
    }

    // Number of bytes at the end of the window still to be inserted into the
    // hash on the next call (identical to the deflate_fast trailing computation).
    s.insert = if s.strstart < MIN_MATCH - 1 {
        s.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == Z_FINISH {
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
