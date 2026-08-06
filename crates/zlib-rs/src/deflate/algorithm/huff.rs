//! `deflate_huff` -- Huffman coding only, with no string matching at all.
//!
//! A verbatim port of `deflate_huff` (`deflate.c` L2155-L2185), the compressor
//! `Z_HUFFMAN_ONLY` selects. It is the simplest of the five: it emits every input byte as a
//! literal and lets the Huffman coder do all the work, so it maintains no hash table and never
//! consults [`crate::deflate::longest_match`]. The comment at `deflate.c` L2151-L2154 records
//! the consequence -- the hash table "will be regenerated if this run of deflate switches away
//! from Huffman", which `deflateParams` arranges through `DeflateState::matches`.
//!
//! # Why the strategy reaches this and the level cannot
//!
//! `configuration_table` (`deflate.c` L112-L124) never names `deflate_huff`; the only route to
//! it is the strategy test in `deflate()`'s dispatch chain (L1217-L1220), reproduced by
//! `CompressFunc::select`. Level 0 is tested *first* there, so a level-0 stream with
//! `Z_HUFFMAN_ONLY` stores its input rather than reaching this function.
//!
//! # Byte-for-byte fidelity
//!
//! Three details of the loop are load-bearing, and none of them is incidental:
//!
//! * **`match_length` is cleared on every iteration** (L2167), not once before the loop.
//!   `deflateParams` and the block-boundary paths read that field, and `deflate_huff` is
//!   reachable mid-stream after a strategy change, so a stale non-zero value would survive into
//!   the next compressor.
//! * **`lookahead` is decremented before `strstart` is incremented** (L2169-L2170). The order
//!   is unobservable in isolation, but it is preserved so the port reads like its oracle.
//! * **The exit tests run in C's order** (L2176-L2184): `insert = 0`, then `Z_FINISH`, then a
//!   non-empty symbol buffer. Emitting the final block before clearing `insert` would leave
//!   the hash-insertion cursor pointing into a block that has already been flushed.
//!
//! # Safety posture
//!
//! No `unsafe`, no raw pointer and no unchecked index: the crate root's
//! `#![forbid(unsafe_code)]` covers this file, the single window read goes through
//! [`crate::weak_slice::Window::byte`], and every cursor update uses a saturating operation so
//! that no arithmetic can panic even on a state this function is never given.

use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::state::{Allocator, DeflateState};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Compresses by Huffman coding alone: every byte becomes a literal.
///
/// Port of `deflate_huff` (`deflate.c` L2155-L2185).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] -- the input is exhausted under `Z_NO_FLUSH`, or the output
///   buffer filled while flushing a block (`flush_block!`).
/// * [`BlockState::FinishStarted`] -- `Z_FINISH` was requested and the final block did not fit
///   in the caller's output buffer.
/// * [`BlockState::FinishDone`] -- `Z_FINISH` was requested and the final block was written.
/// * [`BlockState::BlockDone`] -- a block boundary was reached for one of the other flush modes.
// `_tr_tally` keeps the underscore-prefixed C spelling of `deflate.h` L317 for oracle
// traceability, which `clippy::pedantic` flags at the call site; the same relaxation, for the
// same reason, appears in `trees/mod.rs` and in `deflate/algorithm.rs`.
#[allow(clippy::used_underscore_items)]
pub(crate) fn deflate_huff<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {`  (L2159)
    loop {
        // "Make sure that we have a literal to write."  (L2160-L2168)
        //
        // ```text
        // if (s->lookahead == 0) {
        //     fill_window(s);
        //     if (s->lookahead == 0) {
        //         if (flush == Z_NO_FLUSH)
        //             return need_more;
        //         break;      /* flush the current block */
        //     }
        // }
        // ```
        //
        // A single byte is all this compressor needs, which is why the test is `== 0` here
        // rather than the `< MIN_LOOKAHEAD` of `deflate_fast` and `deflate_slow`: with no
        // match finding there is no lookahead requirement beyond the byte being emitted.
        if state.window.lookahead == 0 {
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            if state.window.lookahead == 0 {
                if matches!(flush, Flush::NoFlush) {
                    return BlockState::NeedMore;
                }
                break;
            }
        }

        // `s->match_length = 0;`  (L2167 in the reference's numbering of the emit block)
        //
        // Cleared every iteration, exactly as C does. See the module documentation: a stale
        // value would outlive a strategy change.
        state.match_length = 0;

        // `_tr_tally_lit(s, s->window[s->strstart], bflush);`  (L2172)
        //
        // `deflate.h` L378 expands the macro to `flush = _tr_tally(s, 0, c)`, so the distance
        // is zero and the "length" slot carries the literal byte.
        //
        // The read cannot fail: `lookahead` is non-zero, and `fill_window` maintains
        // `strstart + lookahead <= window_size` (`deflate.c` L374-L375), so `strstart` indexes
        // a byte that exists. Declining to emit anything is nevertheless the right answer for
        // the unreachable case -- it leaves the block exactly as it was and falls through to
        // the flush decision below, rather than inventing a literal.
        let Some(literal) = state.window.byte(state.window.strstart) else {
            break;
        };
        let bflush = _tr_tally(state, 0, literal);

        // `s->lookahead--; s->strstart++;`  (L2173-L2174)
        //
        // Saturating because library code here does not panic: `lookahead` was just proved
        // non-zero, and `strstart` is bounded by `window_size` (at most 65536), so on the
        // reachable domain both agree exactly with C's plain `--` and `++`.
        state.window.lookahead = state.window.lookahead.saturating_sub(1);
        state.window.strstart = state.window.strstart.saturating_add(1);

        // `if (bflush) FLUSH_BLOCK(s, 0);`  (L2175)
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // `s->insert = 0;`  (L2177)
    //
    // Unconditionally zero, unlike `deflate_fast` and `deflate_slow`, which leave up to
    // `MIN_MATCH - 1` bytes pending: this compressor inserted nothing into the hash table, so
    // there is nothing for a later `deflateParams` switch to catch up on.
    state.window.insert = 0;

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L2178-L2181)
    if matches!(flush, Flush::Finish) {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L2182-L2183)
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L2184)
    BlockState::BlockDone
}
