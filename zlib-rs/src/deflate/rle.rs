//! The `Z_RLE` deflate strategy: run-length encoding at distance one only.
//!
//! This module is a safe-Rust translation of `deflate_rle` from the C zlib
//! source (`deflate.c`, lines 2084-2149). The strategy looks *only* for runs of
//! the immediately preceding byte — that is, matches at distance one — which
//! makes it very fast and a good fit for data such as PNG image scanlines that
//! contain long runs of identical bytes. It is selected by the `mod.rs`
//! strategy dispatch whenever the public compression strategy is
//! [`Strategy::Rle`](crate::constants::Strategy::Rle), independently of the
//! requested compression level.
//!
//! # Relationship to the C implementation
//!
//! The C function operates directly on a `deflate_state *s` and reaches the
//! stream's input/output through the `s->strm` back-pointer. As described by
//! the crate's design note D1 (see [`super::state`]), the Rust port threads the
//! stream-level scalars and the input/output buffers through an *engine
//! context* rather than storing a back-pointer on the state. That engine
//! context — named `DeflateContext` here — together with the [`fill_window`]
//! and [`flush_block`] helpers is owned by the deflate engine module
//! ([`super`]). This file consumes that contract; it never defines it.
//!
//! The mapping of the C control-flow macros onto the engine helpers is:
//!
//! * `fill_window(s)` → [`fill_window`]`(cx)` — refills the sliding window from
//!   the stream's input.
//! * `FLUSH_BLOCK(s, last)` → [`flush_block`]`(cx, last)` — flushes the current
//!   block. The C macro can early-`return` `need_more`/`finish_started` when the
//!   output buffer is full; the Rust helper signals that by returning
//!   `Some(BlockState)`, which this function propagates as its own return value.
//!
//! # Safety and portability
//!
//! This translation contains **zero** `unsafe` code. The C run scan uses raw
//! pointer arithmetic (an eight-times-unrolled `*++scan == *++match` loop);
//! here it becomes bounds-checked slice indexing over `s.window`, computing a
//! byte-for-byte identical match length. The module relies only on `core`/
//! `alloc` facilities (slice indexing and the owned `window` buffer), so it
//! participates in the crate's `no_std`-capable build.

use super::state::{BlockState, DeflateState};
use super::{DeflateContext, fill_window, flush_block};
use crate::constants::{Flush, MAX_MATCH, MIN_MATCH};

/// Compute the length of the run of the preceding byte starting at `strstart`.
///
/// This is the bounds-checked equivalent of the pointer-arithmetic run scan in
/// `deflate_rle` (`deflate.c`, lines 2104-2128). It returns the match length
/// already clamped to the available lookahead, or `0` when there is no run of at
/// least [`MIN_MATCH`] bytes (in which case the caller emits a literal).
///
/// ## Equivalence with the C scan
///
/// The C code positions `scan` at `s->window + s->strstart - 1`, reads
/// `prev = *scan`, then advances `scan` while bytes keep matching up to
/// `strend = s->window + s->strstart + MAX_MATCH`, and finally computes
/// `match_length = MAX_MATCH - (strend - scan)`. Because `strend` is exactly
/// `strstart + MAX_MATCH`, that expression is algebraically identical to
/// `scan - strstart`, which is what this function returns. The eight-times
/// unrolling in C is purely a performance optimization and does not change the
/// computed length, so a single straightforward loop reproduces it exactly.
///
/// ## Bounds
///
/// The highest index read is `strstart + MAX_MATCH - 1` (i.e.
/// `strstart + 257`). The deflate engine guarantees, exactly as the C library
/// does via its `WIN_INIT` window initialization, that the window is valid and
/// zero-initialized through `strstart + MAX_MATCH`, so every access is in
/// bounds. Bytes read beyond the live lookahead are the same zero padding the C
/// code reads, and the result is clamped to `lookahead` below, keeping the
/// emitted match length bit-for-bit identical to C.
fn longest_run(s: &DeflateState) -> usize {
    // A run requires at least MIN_MATCH lookahead bytes and a preceding byte to
    // repeat, so `strstart` must be greater than zero. This mirrors the C guard
    // `if (s->lookahead >= MIN_MATCH && s->strstart > 0)`.
    if s.lookahead < MIN_MATCH || s.strstart == 0 {
        return 0;
    }

    let window = &s.window;
    let strstart = s.strstart;

    // `prev` is the byte at distance one — the byte immediately before
    // `strstart`. C: `scan = s->window + s->strstart - 1; prev = *scan;`.
    let prev = window[strstart - 1];

    // The C code first checks the three bytes at `strstart`, `strstart + 1`, and
    // `strstart + 2` with `if (prev == *++scan && prev == *++scan && prev ==
    // *++scan)`. If any of them differs from `prev`, there cannot be a run of
    // MIN_MATCH (== 3) bytes, so no match is possible.
    if prev != window[strstart] || prev != window[strstart + 1] || prev != window[strstart + 2] {
        return 0;
    }

    // Scan the rest of the run. In C this is the unrolled
    // `do { } while (prev == *++scan ... && scan < strend)` loop with
    // `strend = s->window + s->strstart + MAX_MATCH`. We start `scan` at
    // `strstart + 2` (where the C `scan` pointer rests after the three-byte
    // check) and advance while bytes keep matching, never reading at or past
    // `strend`.
    let strend = strstart + MAX_MATCH;
    let mut scan = strstart + 2;
    while scan < strend && window[scan] == prev {
        scan += 1;
    }

    // C: `s->match_length = MAX_MATCH - (uInt)(strend - scan);`. With
    // `strend == strstart + MAX_MATCH` this is simply `scan - strstart`. Because
    // the three-byte check above already succeeded, the run is always at least
    // MIN_MATCH long here (never 1 or 2).
    let mut match_length = scan - strstart;

    // C: `if (s->match_length > s->lookahead) s->match_length = s->lookahead;`.
    // The run may extend into the zero padding past the live input; clamp it so
    // we never claim to match more bytes than are actually available.
    if match_length > s.lookahead {
        match_length = s.lookahead;
    }

    match_length
}

/// Compress as much as possible from the input window using the `Z_RLE`
/// strategy, emitting matches only at distance one.
///
/// This is the safe-Rust translation of the C `deflate_rle` function
/// (`deflate.c`, lines 2084-2149). It drives the same engine input/output
/// context used by the other deflate strategies (design D1) and returns the
/// resulting [`BlockState`], which tells the engine whether more input/output is
/// needed, a block was completed, or finishing has started/completed.
///
/// Every emitted match has distance one, so the only back-references produced
/// are RLE-style runs; all other bytes are emitted as literals.
pub(crate) fn deflate_rle(cx: &mut DeflateContext, flush: Flush) -> BlockState {
    loop {
        // Make sure that we always have enough lookahead, except at the end of
        // the input. A run needs up to MAX_MATCH bytes, plus one for the
        // preceding byte, so the threshold here is `<= MAX_MATCH` (not the
        // `MIN_LOOKAHEAD` threshold used by the fast/slow strategies).
        // C: lines 2092-2102.
        if cx.state.lookahead <= MAX_MATCH {
            fill_window(cx);
            if cx.state.lookahead <= MAX_MATCH && flush == Flush::NoFlush {
                // Not enough lookahead and no flush requested: ask for more
                // input. C: `return need_more;`.
                return BlockState::NeedMore;
            }
            if cx.state.lookahead == 0 {
                // No more input at all: flush the current block. C: `break;`.
                break;
            }
        }

        // See how many times the previous byte repeats. `longest_run` performs
        // the bounds-checked equivalent of the C pointer scan and already
        // clamps its result to the available lookahead. When there is no run it
        // returns 0, matching the C code's `s->match_length = 0;` default.
        // C: lines 2104-2128.
        cx.state.match_length = longest_run(&cx.state);

        // Emit a match if we have a run of MIN_MATCH or longer, otherwise emit a
        // single literal byte. C: lines 2130-2139.
        let bflush = if cx.state.match_length >= MIN_MATCH {
            let match_length = cx.state.match_length;
            // C: `_tr_tally_dist(s, 1, s->match_length - MIN_MATCH, bflush);`.
            // The distance is always 1; the stored length is the match length
            // biased by MIN_MATCH and fits in a byte (range 0..=255).
            let bflush = cx.state.tr_tally_dist(1, (match_length - MIN_MATCH) as u8);
            cx.state.lookahead -= match_length;
            cx.state.strstart += match_length;
            cx.state.match_length = 0;
            bflush
        } else {
            // No match: output a literal byte.
            // C: `_tr_tally_lit(s, s->window[s->strstart], bflush);`.
            let literal = cx.state.window[cx.state.strstart];
            let bflush = cx.state.tr_tally_lit(literal);
            cx.state.lookahead -= 1;
            cx.state.strstart += 1;
            bflush
        };

        // C: `if (bflush) FLUSH_BLOCK(s, 0);`. `flush_block` returns
        // `Some(BlockState)` when the output buffer filled up, in which case the
        // C macro would `return need_more`; propagate that early return.
        if bflush {
            if let Some(state) = flush_block(cx, false) {
                return state;
            }
        }
    }

    // End of input reached. C: lines 2141-2148.
    //
    // There is nothing to insert for future matches because the RLE strategy
    // does not use the hash chains. C: `s->insert = 0;`.
    cx.state.insert = 0;

    if flush == Flush::Finish {
        // C: `FLUSH_BLOCK(s, 1); return finish_done;`. The flush may early-return
        // `finish_started` if the output buffer is full; otherwise the stream is
        // finished.
        if let Some(state) = flush_block(cx, true) {
            return state;
        }
        return BlockState::FinishDone;
    }

    if cx.state.sym_next != 0 {
        // There is buffered symbol data still to emit. C:
        // `if (s->sym_next) FLUSH_BLOCK(s, 0);`.
        if let Some(state) = flush_block(cx, false) {
            return state;
        }
    }

    BlockState::BlockDone
}
