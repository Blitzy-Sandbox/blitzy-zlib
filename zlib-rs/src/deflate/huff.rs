//! The `Z_HUFFMAN_ONLY` compression strategy (`deflate_huff`).
//!
//! Safe-Rust translation of the C `deflate_huff` block producer
//! (`deflate.c` lines 2155-2185). This is the simplest of the five DEFLATE
//! strategies: when the caller selects
//! [`Strategy::HuffmanOnly`](crate::constants::Strategy::HuffmanOnly) the
//! compressor performs **no LZ77 string matching at all** — it does not call
//! `longest_match`, it does not maintain or consult the hash chains, and it
//! never emits a length/distance pair. Every input byte is tallied as a single
//! literal and the resulting block relies solely on Huffman coding for its
//! compression. It is selected by the strategy dispatch in `mod.rs` whenever the
//! public-API strategy is `HuffmanOnly`, independently of the compression level.
//!
//! Because no matches are produced, the emitted bitstream contains literal codes
//! and the end-of-block code only — there are never any distance codes. This is
//! a structural guarantee of the function (it only ever calls
//! [`tr_tally_lit`](super::state::DeflateState::tr_tally_lit), which records a
//! literal with distance `0`), not merely a runtime expectation.
//!
//! # Relationship to the parent module
//!
//! Like every per-strategy block producer (`deflate_stored`, `deflate_fast`,
//! `deflate_slow`, `deflate_rle`), `deflate_huff` operates on the shared engine
//! I/O context `DeflateContext` (design decision D1: the stream-level scalars
//! and the borrowed `next_in`/`next_out` buffers are threaded through the
//! context rather than stored on the
//! [`DeflateState`](super::state::DeflateState)). It relies on three items
//! provided by the parent [`crate::deflate`] module (`mod.rs`):
//!
//! * `DeflateContext` — the engine context, whose `state` field is the owned
//!   [`DeflateState`](super::state::DeflateState);
//! * [`fill_window`] — refills the sliding window from the input buffer; and
//! * the `flush_block!` macro — the safe-Rust counterpart of the C `FLUSH_BLOCK`
//!   macro. It flushes the current block to the pending buffer, advances
//!   `block_start`, drains the pending buffer to `next_out`, and — crucially —
//!   performs an early `return` from the enclosing function when the output
//!   buffer becomes full (`avail_out == 0`), exactly mirroring the C macro's
//!   `return (last) ? finish_started : need_more;`. These semantics are
//!   identical across all strategies, so the macro is defined once in `mod.rs`
//!   and reused here via textual macro scope.
//!
//! # Safety and portability
//!
//! This module contains no `unsafe` and no raw pointers (honoring the crate-wide
//! `#![forbid(unsafe_code)]`) and uses only `core` facilities, so it builds
//! under the crate's `no_std` path. The single window access is a
//! bounds-checked slice index rather than the C pointer dereference.

use crate::constants::Flush;

use super::state::BlockState;
use super::{DeflateContext, fill_window};

/// Compress the current input under the `Z_HUFFMAN_ONLY` strategy: emit every
/// byte as a literal and never search for matches (C `deflate_huff`,
/// `deflate.c` lines 2155-2185).
///
/// Each iteration guarantees one literal is available (refilling the window via
/// [`fill_window`] when the lookahead is exhausted), zeroes the state's
/// [`match_length`](super::state::DeflateState::match_length) so the bitstream
/// emitter treats the symbol as a literal, tallies the byte at
/// [`strstart`](super::state::DeflateState::strstart), and advances. When the
/// symbol buffer fills, the block is flushed through the `flush_block!` macro,
/// which may force an early return if the output buffer is full.
///
/// # Parameters
///
/// * `cx` — the engine I/O context wrapping the owned
///   [`DeflateState`](super::state::DeflateState) together with the borrowed
///   input/output buffers and the stream-level scalars.
/// * `flush` — the flush mode requested by the current `deflate` call; only
///   [`Flush::NoFlush`] (keep accumulating) and [`Flush::Finish`] (emit the
///   final block) change the control flow here, matching the C source.
///
/// # Returns
///
/// A [`BlockState`] describing how the call terminated:
///
/// * [`BlockState::NeedMore`] — input is exhausted and `flush` is
///   [`Flush::NoFlush`], so more input is required before a block can be closed;
/// * [`BlockState::FinishDone`] — `flush` is [`Flush::Finish`] and the final
///   block has been emitted;
/// * [`BlockState::BlockDone`] — a (non-final) block boundary was reached.
///
/// The `flush_block!` macro may additionally return [`BlockState::NeedMore`] or
/// [`BlockState::FinishStarted`] directly from this function when the output
/// buffer fills mid-flush.
pub(crate) fn deflate_huff(cx: &mut DeflateContext, flush: Flush) -> BlockState {
    // C `for (;;)` — loop until the input is exhausted or the block is flushed.
    loop {
        // Make sure that we have a literal to write (C lines 2163-2170).
        if cx.state.lookahead == 0 {
            // Refill the window from the input buffer.
            fill_window(cx);
            if cx.state.lookahead == 0 {
                // Still nothing: with no pending flush we must wait for more
                // input; otherwise fall out of the loop to flush what we have.
                if flush == Flush::NoFlush {
                    return BlockState::NeedMore;
                }
                break; // flush the current block
            }
        }

        // Output a literal byte (C lines 2172-2178).
        //
        // `match_length = 0` marks this symbol as an unmatched literal for the
        // block emitter — `deflate_huff` never produces an LZ77 match.
        cx.state.match_length = 0;

        // The byte at `strstart` is guaranteed to be initialized because the
        // lookahead is non-zero here, so this bounds-checked index never panics
        // and reads the same byte the C code dereferences via
        // `s->window[s->strstart]`.
        let lc = cx.state.window[cx.state.strstart];

        // `_tr_tally_lit` records the literal and reports whether the symbol
        // buffer is now full (the C `bflush` flag).
        let bflush = cx.state.tr_tally_lit(lc);

        // Consume the byte. `lookahead` is non-zero on this path, so the
        // decrement cannot underflow.
        cx.state.lookahead -= 1;
        cx.state.strstart += 1;

        // Flush a full block. `flush_block!` returns early from this function if
        // the output buffer is exhausted (C `FLUSH_BLOCK(s, 0)`).
        if bflush {
            flush_block!(cx, false);
        }
    }

    // End of input (C lines 2180-2184).
    //
    // No bytes remain to be inserted into the hash table — and since this
    // strategy never touches the hash table, `insert` is simply cleared so a
    // later switch to a match-based strategy rebuilds it from scratch.
    cx.state.insert = 0;

    if flush == Flush::Finish {
        // Emit the final block and report completion.
        flush_block!(cx, true);
        return BlockState::FinishDone;
    }

    // Flush any buffered symbols that have not yet been written as a block.
    if cx.state.sym_next != 0 {
        flush_block!(cx, false);
    }

    BlockState::BlockDone
}
