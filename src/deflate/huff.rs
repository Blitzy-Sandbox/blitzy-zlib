//! `deflate_huff` — the `Z_HUFFMAN_ONLY` compression strategy.
//!
//! A behavior-preserving port of zlib's `deflate_huff()` (`deflate.c`
//! L2155-2185). This is the simplest of the five DEFLATE strategy functions: it
//! performs **no string matching at all**. Every input byte is emitted as a
//! literal, so all compression comes from the Huffman coding applied by the
//! block emitter in [`crate::deflate::trees`].
//!
//! This strategy is selected when the caller requests
//! [`Strategy::HuffmanOnly`](crate::constants::Strategy) and is dispatched by
//! the per-call strategy selector (`strategy.rs`). Because matches are never
//! sought, the hash table / dictionary is never touched; it is left to be
//! regenerated should a later `deflate()` call switch back to a matching
//! strategy, exactly as the C comment on `deflate_huff` notes.
//!
//! # Byte-exact output
//!
//! To keep the emitted bitstream byte-identical to canonical zlib
//! (AAP §0.6.7, §0.7.1), this port preserves the C control flow line for line:
//!
//! * `match_length` is cleared to `0` on every iteration, marking each symbol
//!   as a literal (no match) so the block emitter records it through the
//!   literal/length tree and nothing is inserted into the hash chains.
//! * The literal source is the **current** window byte `window[strstart]` (not
//!   `strstart - 1`).
//! * The fill-window prologue tests `lookahead == 0` (a single byte of
//!   lookahead suffices here), not `< MIN_LOOKAHEAD` as the matching strategies
//!   require.
//! * The block-flush decisions — including the `Z_FINISH` / `sym_next` tail
//!   shared with the other strategy functions — match `deflate.c` exactly.
//!
//! # Safety and `no_std`
//!
//! Contains **no `unsafe`** (the compression core is 100% safe Rust; `unsafe`
//! lives only in `ffi.rs` and `inflate/fast.rs`, AAP §0.6.2) and uses only
//! `core` constructs, so it is `no_std`-clean and allocation-free, operating
//! solely on the buffers already owned by [`DeflateState`].
//!
//! [`DeflateState`]: crate::deflate::state::DeflateState

use crate::constants::FlushMode;
use crate::deflate::flush_block;
use crate::deflate::state::{BlockState, DeflateStream};

/// Compress the stream `s` using the `Z_HUFFMAN_ONLY` strategy: emit every
/// available input byte as a literal, never searching for matches.
///
/// This is the Rust port of `deflate.c`'s `deflate_huff()` (L2155-2185). It is
/// driven once per `deflate()` call with the caller's `flush` request and
/// returns the resulting [`BlockState`], which tells the orchestrator
/// (`deflate/mod.rs`) what to do next:
///
/// * [`BlockState::NeedMore`] — ran out of input while `flush == NoFlush`
///   (or the output buffer filled): call again with more input/output space.
/// * [`BlockState::FinishStarted`] — `Z_FINISH` was requested but the output
///   buffer filled before the final block could be completely flushed.
/// * [`BlockState::FinishDone`] — `Z_FINISH` completed; the stream is done.
/// * [`BlockState::BlockDone`] — a non-finishing flush completed at a block
///   boundary.
///
/// # Parameters
///
/// * `s` — the per-call compression context: a mutable borrow of the
///   persistent [`DeflateState`] paired with this call's input/output buffers.
/// * `flush` — the flush mode requested by the current `deflate()` call.
///
/// [`DeflateState`]: crate::deflate::state::DeflateState
pub(crate) fn deflate_huff(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    loop {
        // Make sure that we have a literal to write. A single byte of lookahead
        // is enough for this strategy, so the prologue tests `lookahead == 0`
        // rather than `< MIN_LOOKAHEAD` (C `deflate.c` L2160-2168).
        if s.state.lookahead == 0 {
            s.fill_window();
            if s.state.lookahead == 0 {
                // Still nothing after refilling the window.
                if flush == FlushMode::NoFlush {
                    // No more input is forthcoming this call; ask for more.
                    return BlockState::NeedMore;
                }
                // A flush was requested: stop emitting and flush the block.
                break;
            }
        }

        // Output a literal byte (C `deflate.c` L2171-2178). Clearing
        // `match_length` to 0 marks the symbol as a literal, so `tr_tally_lit`
        // is used and no hash insertion occurs.
        s.state.match_length = 0;
        // Read the current window byte into a `Copy` value first so the
        // immutable borrow of `s.state` ends before the mutable tally call.
        let lc = s.state.window[s.state.strstart];
        let bflush = s.state.tr_tally_lit(lc);
        s.state.lookahead -= 1;
        s.state.strstart += 1;
        if bflush {
            // The symbol buffer is full: flush the current (non-final) block.
            // If the output buffer filled in the process, return immediately.
            if let Some(bstate) = flush_block(s, false) {
                return bstate;
            }
        }
    }

    // No matches were ever sought, so no bytes are deferred for later hash
    // insertion (C `deflate.c` L2180: `s->insert = 0;`).
    s.state.insert = 0;

    if flush == FlushMode::Finish {
        // Emit the final block. If the output buffer filled first, the caller
        // must call again to drain it (`finish_started`).
        if let Some(bstate) = flush_block(s, true) {
            return bstate;
        }
        return BlockState::FinishDone;
    }

    // Flush a trailing partial block if any symbols are buffered
    // (C `deflate.c` L2183: `if (s->sym_next) FLUSH_BLOCK(s, 0);`).
    if s.state.sym_next != 0 {
        if let Some(bstate) = flush_block(s, false) {
            return bstate;
        }
    }

    BlockState::BlockDone
}
