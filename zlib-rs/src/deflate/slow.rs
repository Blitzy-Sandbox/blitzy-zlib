//! The lazy-evaluation LZ77 compression strategy (`deflate_slow`).
//!
//! This module is the safe-Rust translation of the C `deflate_slow` function
//! (`deflate.c` lines 1956-2076), the match-search strategy selected for
//! compression levels 4 through 9 (`configuration_table[4..=9].func ==
//! deflate_slow`). Unlike the greedy [`deflate_fast`](super::fast) strategy,
//! `deflate_slow` performs **lazy matching**: when it finds a match at the
//! current position it does *not* emit it immediately. Instead it advances one
//! byte and looks for a longer match starting at the next position; only if the
//! new match is no better does it emit the previous ("lazy") match. This
//! one-byte look-ahead consistently improves the compression ratio, which is
//! why it backs the default level (6) and every high level.
//!
//! # Bit-exact fidelity
//!
//! The emitted bitstream must be byte-identical to the C reference for every
//! `(level, strategy, windowBits, flush)` combination this strategy serves.
//! The lazy decision logic — the interplay of `match_available`, `prev_length`,
//! and `prev_match`, the `Z_FILTERED` / [`TOO_FAR`] match filters, and the
//! [`FLUSH_BLOCK`](#flush_block-vs-flush_block_only) versus
//! `FLUSH_BLOCK_ONLY` distinction in the two emit branches — is reproduced
//! exactly. Any deviation (an off-by-one in `strstart - 1`, the
//! `prev_length -= 2` insert loop, or which flush macro is used) would change
//! the sequence of literals and matches and diverge the output from C zlib.
//!
//! # Safety and portability
//!
//! This module contains **no `unsafe`** (honoring the crate-wide
//! `#![forbid(unsafe_code)]`) and uses only bounds-checked slice indexing. It
//! references nothing outside `core`, so it participates cleanly in the crate's
//! `no_std` build path.
//!
//! # Integration contract with [`mod`](super) (design decision D1)
//!
//! Following design D1, the per-call *engine I/O context* — the bundle of the
//! owned [`DeflateState`] together with the borrowed input/output slices and
//! the `z_stream` scalars (`total_in`/`total_out`/`adler`) — is the
//! [`DeflateContext`](super::DeflateContext) defined in the module root. The C
//! `deflate_state *s` parameter (which carries a `strm` back-pointer) becomes
//! `cx: &mut DeflateContext`, and the C `s->field` accesses become
//! `cx.state.field`. This strategy relies on the following items provided by
//! the module root (`mod.rs`); they are the deflate *engine* primitives shared
//! by every strategy and therefore live with the driver rather than being
//! duplicated here:
//!
//! * [`fill_window`](super::fill_window) — refill the sliding window from the
//!   input slice (C `fill_window`).
//! * [`longest_match`](super::longest_match) — find the longest hash-chain
//!   match for the current string and set `cx.state.match_start` (C
//!   `longest_match`). Shared with [`deflate_fast`](super::fast).
//! * [`flush_block_only`](super::flush_block_only) — emit the current block via
//!   [`DeflateState::tr_flush_block`], advance `block_start`, and flush pending
//!   output to the output slice (the C `FLUSH_BLOCK_ONLY` macro). It lives in
//!   the root because emitting the block borrows `state.window` while the tally
//!   buffers on `state` are mutated, a self-borrow most naturally resolved
//!   where the driver owns the window.
//! * [`DeflateContext::avail_out`](super::DeflateContext) — the number of
//!   bytes of room remaining in the output slice (C `strm->avail_out`).
//!
//! Everything else this strategy needs is self-contained: the symbol-tallying
//! [`DeflateState::tr_tally_lit`] / [`DeflateState::tr_tally_dist`] live in
//! [`trees`](super::trees), and the hash-chain insertion ([`insert_string`],
//! the C `INSERT_STRING` macro) and the [`TOO_FAR`] threshold are defined
//! locally because they touch only [`DeflateState`] fields.

use super::state::{BlockState, DeflateState, MIN_LOOKAHEAD, Pos};
use super::{DeflateContext, fill_window, flush_block_only, longest_match};
use crate::constants::{Flush, MIN_MATCH, Strategy as CompressionStrategy};

/// Matches of length [`MIN_MATCH`] (3) whose distance exceeds this threshold
/// are discarded, because a literal run usually encodes more compactly than a
/// short, far match (C `#define TOO_FAR 4096`, `deflate.c` line 89).
///
/// The upstream guard `#if TOO_FAR <= 32767` is satisfied (`4096 <= 32767`), so
/// the distance branch of the match filter is always active, exactly as in the
/// reference build.
const TOO_FAR: usize = 4096;

/// Insert the three-byte string at `str_pos` into the hash table and return the
/// previous head of its hash chain.
///
/// This is the safe translation of the C `INSERT_STRING` macro (`deflate.c`
/// lines 159-165) together with the `UPDATE_HASH` it expands
/// (`deflate.c` line 141). It rolls `ins_h` forward by one input byte
/// (`window[str_pos + MIN_MATCH - 1]`), links the new position at the head of
/// the chain (`prev[str_pos & w_mask] = head[ins_h]`), and records it as the
/// new head (`head[ins_h] = str_pos`). The returned value is the old chain
/// head — the most recent earlier position with the same hash — which the
/// caller uses as the starting point for [`longest_match`].
///
/// All indexing is bounds-checked and touches only [`DeflateState`] fields, so
/// the helper is self-contained (no engine context required).
///
/// # Panics
///
/// Indexing panics only if the caller violates the C precondition that the
/// first `MIN_MATCH` bytes at `str_pos` are valid window bytes; the
/// `lookahead >= MIN_MATCH` guard at every call site upholds it.
#[inline]
fn insert_string(state: &mut DeflateState, str_pos: usize) -> Pos {
    // UPDATE_HASH(s, s->ins_h, s->window[str + MIN_MATCH - 1])
    let byte = state.window[str_pos + (MIN_MATCH - 1)] as usize;
    state.ins_h = ((state.ins_h << state.hash_shift) ^ byte) & state.hash_mask;

    // match_head = s->prev[str & s->w_mask] = s->head[s->ins_h]
    let hash_index = state.ins_h;
    let head = state.head[hash_index];
    let prev_index = str_pos & state.w_mask;
    state.prev[prev_index] = head;

    // s->head[s->ins_h] = (Pos)str
    state.head[hash_index] = str_pos as Pos;

    head
}

/// Compress as much as possible from the input using lazy match evaluation,
/// the strategy for compression levels 4-9.
///
/// This is the direct translation of the C `deflate_slow` (`deflate.c` lines
/// 1956-2076). It tallies literals and `(distance, length)` matches into the
/// block's symbol buffer (via [`DeflateState::tr_tally_lit`] /
/// [`DeflateState::tr_tally_dist`]) and flushes complete blocks to the output
/// slice. The deferred-match state (`match_available`, `prev_length`,
/// `prev_match`) is preserved across invocations on [`DeflateState`], so a
/// block interrupted by a full output buffer resumes seamlessly.
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — more input or output room is required to
///   continue (returned when lookahead is short under `Z_NO_FLUSH`, or when a
///   block flush filled the output buffer).
/// * [`BlockState::FinishStarted`] — `flush == Z_FINISH` and the final block
///   was emitted but the output buffer filled before all pending bytes were
///   written; the caller must drain and call again.
/// * [`BlockState::FinishDone`] — `flush == Z_FINISH` and the stream is fully
///   flushed.
/// * [`BlockState::BlockDone`] — a (non-finishing) flush completed.
pub(crate) fn deflate_slow(cx: &mut DeflateContext, flush: Flush) -> BlockState {
    // Process the input block.
    loop {
        // Make sure we always have enough lookahead, except at the end of the
        // input file. We need MAX_MATCH bytes for the next match, plus
        // MIN_MATCH bytes to insert the string following the next match.
        if cx.state.lookahead < MIN_LOOKAHEAD {
            fill_window(cx);
            if cx.state.lookahead < MIN_LOOKAHEAD && flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }
            if cx.state.lookahead == 0 {
                // Flush the current block.
                break;
            }
        }

        // Insert the string window[strstart .. strstart + 2] in the dictionary
        // and set `hash_head` to the head of the hash chain (or NIL = 0 when no
        // insertion is performed). C `IPos hash_head`.
        let mut hash_head: Pos = 0; // NIL
        if cx.state.lookahead >= MIN_MATCH {
            let str_pos = cx.state.strstart;
            hash_head = insert_string(cx.state, str_pos);
        }

        // Find the longest match, discarding those <= prev_length. Save the
        // previous match as the lazy candidate first.
        cx.state.prev_length = cx.state.match_length;
        cx.state.prev_match = cx.state.match_start as Pos;
        cx.state.match_length = MIN_MATCH - 1;

        if hash_head != 0
            && cx.state.prev_length < cx.state.max_lazy_match
            && cx.state.strstart - (hash_head as usize) <= cx.state.w_size - MIN_LOOKAHEAD
        {
            // To simplify the code, we prevent matches with the string of
            // window index 0 (in particular we have to avoid a match of the
            // string with itself at the start of the input file).
            //
            // `longest_match` sets `cx.state.match_start`. Bind the returned
            // length to a local first to avoid borrowing `cx` mutably twice.
            let match_len = longest_match(cx, hash_head);
            cx.state.match_length = match_len;

            // Filter out short matches that compress worse than literals: any
            // match of length <= 5 under Z_FILTERED, and a minimum-length match
            // whose distance exceeds TOO_FAR.
            if cx.state.match_length <= 5
                && (cx.state.strategy == CompressionStrategy::Filtered
                    || (cx.state.match_length == MIN_MATCH
                        && cx.state.strstart - cx.state.match_start > TOO_FAR))
            {
                // If prev_match is also MIN_MATCH, match_start is garbage but
                // we will ignore the current match anyway.
                cx.state.match_length = MIN_MATCH - 1;
            }
        }

        // If there was a match at the previous step and the current match is
        // not better, output the previous match.
        if cx.state.prev_length >= MIN_MATCH && cx.state.match_length <= cx.state.prev_length {
            // Do not insert strings in the hash table beyond this point.
            let max_insert = cx.state.strstart + cx.state.lookahead - MIN_MATCH;

            // _tr_tally_dist(s, strstart - 1 - prev_match, prev_length - MIN_MATCH)
            let distance = (cx.state.strstart - 1 - cx.state.prev_match as usize) as u16;
            let length = (cx.state.prev_length - MIN_MATCH) as u8;
            let bflush = cx.state.tr_tally_dist(distance, length);

            // Insert in the hash table all strings up to the end of the match.
            // strstart - 1 and strstart are already inserted. If there is not
            // enough lookahead, the last two strings are not inserted.
            cx.state.lookahead -= cx.state.prev_length - 1;
            cx.state.prev_length -= 2;
            // C: do { if (++s->strstart <= max_insert) INSERT_STRING(...); }
            //    while (--s->prev_length != 0);  — runs `prev_length` times,
            // i.e. (original prev_length - 2) iterations.
            loop {
                cx.state.strstart += 1;
                if cx.state.strstart <= max_insert {
                    let str_pos = cx.state.strstart;
                    insert_string(cx.state, str_pos);
                }
                cx.state.prev_length -= 1;
                if cx.state.prev_length == 0 {
                    break;
                }
            }
            cx.state.match_available = false;
            cx.state.match_length = MIN_MATCH - 1;
            cx.state.strstart += 1;

            if bflush {
                // FLUSH_BLOCK(cx, false): flush the block and return early if
                // the output buffer is now full.
                flush_block_only(cx, false);
                if cx.avail_out() == 0 {
                    return BlockState::NeedMore;
                }
            }
        } else if cx.state.match_available {
            // There was no match at the previous position (or the current match
            // is longer, truncating the previous one to a single literal):
            // output a single literal for the previous byte.
            let prev_byte = cx.state.window[cx.state.strstart - 1];
            let bflush = cx.state.tr_tally_lit(prev_byte);
            if bflush {
                // FLUSH_BLOCK_ONLY: flush the block but do NOT early-return on
                // it; the explicit avail_out check below handles back-pressure.
                flush_block_only(cx, false);
            }
            cx.state.strstart += 1;
            cx.state.lookahead -= 1;
            if cx.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // There is no previous match to compare with: wait for the next
            // step to decide.
            cx.state.match_available = true;
            cx.state.strstart += 1;
            cx.state.lookahead -= 1;
        }
    }

    // End of input: emit the last deferred literal, if any.
    if cx.state.match_available {
        let prev_byte = cx.state.window[cx.state.strstart - 1];
        cx.state.tr_tally_lit(prev_byte);
        cx.state.match_available = false;
    }

    // Record how many bytes at the block start still need inserting into the
    // hash table when compression resumes (C `s->insert`).
    cx.state.insert = if cx.state.strstart < MIN_MATCH - 1 {
        cx.state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == Flush::Finish {
        // FLUSH_BLOCK(cx, true): emit the final block; if output fills, the
        // caller must drain and call again.
        flush_block_only(cx, true);
        if cx.avail_out() == 0 {
            return BlockState::FinishStarted;
        }
        return BlockState::FinishDone;
    }

    if cx.state.sym_next != 0 {
        // FLUSH_BLOCK(cx, false): emit the accumulated (non-final) block.
        flush_block_only(cx, false);
        if cx.avail_out() == 0 {
            return BlockState::NeedMore;
        }
    }

    BlockState::BlockDone
}
