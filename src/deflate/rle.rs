//! `Z_RLE` strategy compressor — run-length encoding (`deflate_rle`).
//!
//! This module is the safe-Rust port of the C function `deflate_rle()` in
//! `deflate.c` (upstream lines 2084-2149). It implements the DEFLATE
//! compression strategy selected when the caller requests
//! [`Strategy::Rle`](crate::constants::Strategy): the encoder looks **only**
//! for runs of the byte immediately preceding the current position, so every
//! match it emits has a fixed distance of exactly `1`. No hash table is
//! consulted or maintained, which makes this strategy ideal for data such as
//! PNG image rows where matches are dominated by short repeats at distance 1.
//!
//! Conceptually `deflate_rle` is LZ77 restricted to distance 1: instead of
//! searching the window for the longest match at an arbitrary distance, it
//! simply counts how many times the previous byte repeats, caps that count at
//! [`MAX_MATCH`], and tallies a length/distance pair with distance `1`. Bytes
//! that do not begin a run of at least [`MIN_MATCH`] are emitted as literals.
//!
//! # Byte-exact output
//!
//! The crate guarantees byte-identical output to canonical C zlib for the same
//! input, level, strategy, and window configuration (AAP §0.6.7, §0.7.1). The
//! only non-trivial part of this port is reproducing the run-length boundary
//! exactly. The C code scans with an unrolled pointer loop and computes
//! `match_length = MAX_MATCH - (strend - scan)`; the safe `while`-based scan
//! used here is **provably equal** to that computation (see the detailed proof
//! in [`deflate_rle`]). Everything else — the literal/match tally, the block
//! flushing, and the loop tail — mirrors the other strategy functions exactly.
//!
//! # Safety and `no_std`
//!
//! This module contains **no `unsafe`** (AAP §0.6.2) and is `no_std`-clean: it
//! refers only to crate-internal types and the always-available `core`/`alloc`
//! machinery reachable through [`DeflateState`](crate::deflate::state) (the
//! sliding window is an owned `Vec<u8>`). All window accesses use safe slice
//! indexing; the upstream "wild scan" pointer arithmetic is replaced by bounded
//! index arithmetic that the window-padding invariant keeps in range.

use crate::constants::FlushMode;
use crate::deflate::flush_block;
use crate::deflate::state::{BlockState, DeflateStream, MAX_MATCH, MIN_MATCH};

/// Compress the current input using the `Z_RLE` strategy (run-length encoding).
///
/// Ported from `deflate.c` `deflate_rle()` (lines 2084-2149). The encoder
/// repeatedly:
///
/// 1. ensures the window holds enough look-ahead (at least `MAX_MATCH` bytes,
///    unless the end of input has been reached);
/// 2. measures the run of the immediately preceding byte (distance 1), capped
///    at [`MAX_MATCH`] and then at the remaining look-ahead;
/// 3. tallies either a length/distance match with distance `1` (when the run
///    is at least [`MIN_MATCH`] long) or a single literal byte; and
/// 4. flushes the current block whenever the symbol buffer fills.
///
/// # Parameters
///
/// * `s` — the per-call deflate context wrapping the persistent engine state
///   and the input/output cursors.
/// * `flush` — the active [`FlushMode`]; controls whether the function may
///   return [`BlockState::NeedMore`] to request more input ([`FlushMode::NoFlush`])
///   and whether the stream is being finished ([`FlushMode::Finish`]).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — ran out of look-ahead before reaching the end
///   of the block and more input may still arrive (`Z_NO_FLUSH`).
/// * [`BlockState::FinishDone`] — the stream was finished and the final block
///   has been emitted.
/// * [`BlockState::BlockDone`] — a block boundary was reached for a non-finish
///   flush.
///
/// An early return carrying the value produced by [`flush_block`] occurs when
/// the output buffer fills mid-block, exactly mirroring the C `FLUSH_BLOCK`
/// macro's `return (last) ? finish_done : need_more;` behavior.
pub(crate) fn deflate_rle(s: &mut DeflateStream<'_>, flush: FlushMode) -> BlockState {
    loop {
        // Make sure that we always have enough look-ahead, except at the end of
        // the input file. We need `MAX_MATCH` bytes for the longest run, plus
        // one for the unrolled loop in the C original.
        if s.state.lookahead <= MAX_MATCH {
            s.fill_window();
            if s.state.lookahead <= MAX_MATCH && flush == FlushMode::NoFlush {
                return BlockState::NeedMore;
            }
            if s.state.lookahead == 0 {
                // Out of input: flush whatever block is pending below.
                break;
            }
        }

        // See how many times the previous byte repeats. A run requires at least
        // `MIN_MATCH` bytes of look-ahead and a predecessor byte to match
        // against (`strstart > 0`), mirroring the C guard exactly.
        s.state.match_length = 0;
        if s.state.lookahead >= MIN_MATCH && s.state.strstart > 0 {
            let strstart = s.state.strstart;
            // `prev` is the byte at distance one — the value every byte in the
            // run must equal. `strstart > 0` guarantees `strstart - 1` is valid.
            let prev = s.state.window[strstart - 1];

            // Gate exactly as the C code's three `prev == *++scan` checks do:
            // the first three bytes of the candidate run must equal `prev`.
            if prev == s.state.window[strstart]
                && prev == s.state.window[strstart + 1]
                && prev == s.state.window[strstart + 2]
            {
                // A run of at least three bytes is confirmed; extend it up to
                // `MAX_MATCH`.
                //
                // Byte-exactness proof. The C original sets
                // `scan = window + strstart - 1`, advances `scan` past the three
                // confirmed bytes, then loops `while (prev == *++scan ... &&
                // scan < strend)` with `strend = window + strstart + MAX_MATCH`,
                // finally computing `match_length = MAX_MATCH - (strend - scan)`.
                // Substituting `strend`'s definition, that equals
                // `scan_index - strstart`, i.e. the count of consecutive bytes
                // equal to `prev` starting at `window[strstart]`, capped at
                // `MAX_MATCH`. Because `MAX_MATCH - 2 = 256` is an exact multiple
                // of the 8-way unroll, `scan` lands precisely on `strend` for a
                // full run (no overshoot), so the cap is `MAX_MATCH` exactly.
                // The loop below starts at `run = 3` (the three bytes already
                // confirmed) and increments while bytes keep matching and
                // `run < MAX_MATCH`, yielding the identical capped count without
                // any boundary shift.
                let mut run: usize = 3;
                while run < MAX_MATCH && s.state.window[strstart + run] == prev {
                    run += 1;
                }
                s.state.match_length = run;
                // Never claim more than the data that is actually available; the
                // scan may have read into the zero-padded high-water region.
                if s.state.match_length > s.state.lookahead {
                    s.state.match_length = s.state.lookahead;
                }
            }
        }

        // Emit a match if we have a run of `MIN_MATCH` or longer, else a literal.
        let bflush = if s.state.match_length >= MIN_MATCH {
            // The distance is always the literal `1` for RLE; the length code is
            // `(match_length - MIN_MATCH)`. `match_length` is in
            // `MIN_MATCH..=MAX_MATCH` here, so the subtraction cannot underflow
            // and the result fits in a `u8` (`MAX_MATCH - MIN_MATCH == 255`).
            let len = (s.state.match_length - MIN_MATCH) as u8;
            let flush_now = s.state.tr_tally_dist(1, len);
            s.state.lookahead -= s.state.match_length;
            s.state.strstart += s.state.match_length;
            s.state.match_length = 0;
            flush_now
        } else {
            // No run: output the current byte as a literal (note this reads
            // `window[strstart]`, not `window[strstart - 1]`).
            let lc = s.state.window[s.state.strstart];
            let flush_now = s.state.tr_tally_lit(lc);
            s.state.lookahead -= 1;
            s.state.strstart += 1;
            flush_now
        };

        if bflush {
            // Mirrors C `FLUSH_BLOCK(s, 0)`: flush the pending block and, if the
            // output buffer is now empty, return `need_more` immediately.
            if let Some(state) = flush_block(s, false) {
                return state;
            }
        }
    }

    // RLE never seeds the hash table for future matches, so the deferred-insert
    // count is cleared before finishing the block.
    s.state.insert = 0;

    if flush == FlushMode::Finish {
        // Mirrors C `FLUSH_BLOCK(s, 1); return finish_done;`.
        if let Some(state) = flush_block(s, true) {
            return state;
        }
        return BlockState::FinishDone;
    }

    if s.state.sym_next != 0 {
        // Mirrors C `if (s->sym_next) FLUSH_BLOCK(s, 0);`.
        if let Some(state) = flush_block(s, false) {
            return state;
        }
    }

    BlockState::BlockDone
}
