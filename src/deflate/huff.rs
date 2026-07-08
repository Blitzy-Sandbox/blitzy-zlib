//! `deflate_huff` — the Huffman-only compression engine (`Z_HUFFMAN_ONLY`).
//!
//! This is the safe-Rust port of the C `deflate_huff` block producer
//! (`deflate.c` L2155-L2185 in **zlib 1.3.2.1-motley**). It is selected by
//! [`Strategy::HuffmanOnly`](crate::constants::Strategy) and is the simplest of
//! the five deflate engines: it **never** searches for string matches and
//! **never** maintains the hash table. Every input byte is emitted as a literal
//! and tallied into the symbol buffer; Huffman coding of those literals is then
//! performed by the block-flush machinery in [`crate::deflate::trees`].
//!
//! Because it forgoes match finding entirely, this engine trades compression
//! ratio for simple, predictable behaviour. The zlib comment notes that the
//! hash table "will be regenerated if this run of deflate switches away from
//! Huffman", which is precisely why this engine deliberately leaves it
//! untouched.
//!
//! # Byte-for-byte compatibility
//!
//! The emitted token stream is identical to reference zlib for the same input:
//! the engine tallies exactly one literal per input byte, in order, and defers
//! every block-type and Huffman decision to `trees::_tr_flush_block`, which is
//! itself a faithful port. See AAP §0.6.4 (bit-exact wire-format).
//!
//! # `FLUSH_BLOCK` / `FLUSH_BLOCK_ONLY`
//!
//! The C macros `FLUSH_BLOCK_ONLY` and `FLUSH_BLOCK` (`deflate.c` L1630-L1645)
//! fuse "emit the current block" with an *early return* when the caller's output
//! buffer fills. Rust has no early-returning macro that reads naturally here, so
//! the two macros are reproduced as the free functions `flush_block_only` and
//! `flush_block`. `flush_block` returns `Option<BlockState>`: `Some(state)`
//! means "the output buffer is full — return `state` to the driver
//! immediately", and `None` means "there is room; continue". Each call site uses
//! the `if let Some(rc) = flush_block(..) { return rc; }` idiom, which is the
//! direct Rust equivalent of the C macro's embedded `return`.
//!
//! # Safety
//!
//! This module contains **zero** `unsafe` code, satisfying the AAP constraint
//! that the compression core be entirely safe (§0.6.2). It is `no_std`
//! compatible: it uses only the types and methods provided by its sibling
//! modules and performs no direct allocation.

use crate::deflate::state::{DeflateState, IoContext};
use crate::deflate::strategy::BlockState;
use crate::deflate::trees;

/// `Z_NO_FLUSH` (`zlib.h` L172): the caller has not requested a flush, so the
/// engine may return [`BlockState::NeedMore`] and wait for more input.
///
/// This mirrors [`crate::constants::Z_NO_FLUSH`]. It is kept as a private
/// constant so this engine depends only on its declared sibling modules
/// (`state`, `strategy`, `trees`); the value is fixed by the zlib wire ABI and
/// can never change.
const Z_NO_FLUSH: i32 = 0;

/// `Z_FINISH` (`zlib.h` L176): no more input will be provided, so the engine
/// must complete the stream and emit the final block.
///
/// Mirrors [`crate::constants::Z_FINISH`]; see [`Z_NO_FLUSH`] for why it is a
/// private constant here.
const Z_FINISH: i32 = 4;

/// Emit the current block, mirroring the C `FLUSH_BLOCK_ONLY(s, last)` macro
/// (`deflate.c` L1630-L1640).
///
/// This hands the block to [`trees::_tr_flush_block`], which chooses the block
/// type (stored / static / dynamic), writes it into the state's pending buffer,
/// and — when `last` is set — flushes the final partial byte. The block-start
/// cursor is then advanced to the current `strstart`, and any freshly produced
/// bytes are copied out to the caller via [`DeflateState::flush_pending`].
///
/// The `buf` argument passed to [`trees::_tr_flush_block`] names the window
/// offset of the block start, or `None` when `block_start` is negative (the
/// window has slid so far that no in-window stored representation is available).
/// [`usize::try_from`] reproduces the C ternary
/// `s->block_start >= 0L ? &s->window[block_start] : Z_NULL` exactly: it yields
/// `Some(offset)` for a non-negative `block_start` and `None` for a negative
/// one. The `stored_len` argument is the number of input bytes covered by the
/// block, `strstart - block_start`, computed with signed arithmetic exactly as
/// the C `(ulg)((long)s->strstart - s->block_start)` cast does.
fn flush_block_only(s: &mut DeflateState, io: &mut IoContext, last: bool) {
    // C: s->block_start >= 0L ? (charf *)&s->window[block_start] : Z_NULL
    let buf = usize::try_from(s.block_start).ok();
    // C: (ulg)((long)s->strstart - s->block_start)
    let stored_len = (s.strstart as isize - s.block_start) as usize;

    trees::_tr_flush_block(s, buf, stored_len, last);
    s.block_start = s.strstart as isize;
    s.flush_pending(io);
}

/// Emit the current block and signal an early return if the output buffer is now
/// full, mirroring the C `FLUSH_BLOCK(s, last)` macro (`deflate.c` L1642-L1645).
///
/// Returns `Some(state)` when `avail_out` has reached `0` after the flush — the
/// caller must immediately return `state` to the deflate driver:
/// [`BlockState::FinishStarted`] when finishing the stream (`last == true`) or
/// [`BlockState::NeedMore`] otherwise. Returns `None` when output space remains
/// and the engine may continue.
#[must_use]
fn flush_block(s: &mut DeflateState, io: &mut IoContext, last: bool) -> Option<BlockState> {
    flush_block_only(s, io, last);
    // C: if (s->strm->avail_out == 0) return (last) ? finish_started : need_more;
    if io.avail_out_remaining() == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}

/// Compress the current input using Huffman coding only, without any string
/// matching (`Z_HUFFMAN_ONLY`).
///
/// Faithful port of the C `deflate_huff` (`deflate.c` L2155-L2185). It repeatedly
/// ensures a literal is available (refilling the window from `io` via
/// [`DeflateState::fill_window`] when the lookahead is exhausted) and tallies
/// that literal into the symbol buffer with `trees::_tr_tally_lit`. Whenever
/// the symbol buffer fills, the current block is flushed. On `Z_FINISH` the
/// final block is emitted and the stream is completed.
///
/// # Parameters
///
/// * `s` — the live deflate state; its window, `lookahead`, `strstart`,
///   `sym_next`, and tree-frequency counters are advanced in place.
/// * `io` — the I/O context supplying input (for [`DeflateState::fill_window`])
///   and receiving compressed output (for [`DeflateState::flush_pending`]).
/// * `flush` — the flush mode requested by the caller. Only `Z_NO_FLUSH` and
///   `Z_FINISH` alter the control flow here; every other mode behaves like a
///   block-completing flush and yields [`BlockState::BlockDone`].
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — no further progress is possible without more
///   input (only when `flush == Z_NO_FLUSH`) or more output space.
/// * [`BlockState::FinishStarted`] — finishing began but the output buffer
///   filled before the final block was fully written.
/// * [`BlockState::FinishDone`] — the stream was finished (`flush == Z_FINISH`).
/// * [`BlockState::BlockDone`] — the pending symbols were flushed for a
///   non-finishing flush request.
///
/// # Note on `match_length`
///
/// `s.match_length` is reset to `0` before every literal, exactly as the C
/// source does. The engine never assigns it any other value because it performs
/// no match finding.
pub fn deflate_huff(s: &mut DeflateState, io: &mut IoContext, flush: i32) -> BlockState {
    loop {
        // Make sure that we have a literal to write.
        if s.lookahead == 0 {
            s.fill_window(io);
            if s.lookahead == 0 {
                if flush == Z_NO_FLUSH {
                    return BlockState::NeedMore;
                }
                break; // flush the current block
            }
        }

        // Output a literal byte. `cc` is copied out of the window before the
        // mutable borrow of `s` in `_tr_tally_lit`, so no aliasing occurs.
        s.match_length = 0;
        let cc = s.window[s.strstart];
        let bflush = trees::_tr_tally_lit(s, cc);
        s.lookahead -= 1;
        s.strstart += 1;
        if bflush {
            // C: if (bflush) FLUSH_BLOCK(s, 0);
            if let Some(rc) = flush_block(s, io, false) {
                return rc;
            }
        }
    }

    s.insert = 0;

    if flush == Z_FINISH {
        // C: FLUSH_BLOCK(s, 1); return finish_done;
        if let Some(rc) = flush_block(s, io, true) {
            return rc;
        }
        return BlockState::FinishDone;
    }

    if s.sym_next != 0 {
        // C: if (s->sym_next) FLUSH_BLOCK(s, 0);
        if let Some(rc) = flush_block(s, io, false) {
            return rc;
        }
    }

    BlockState::BlockDone
}
