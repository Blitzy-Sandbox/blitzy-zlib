//! `deflate_rle` — the `Z_RLE` (run-length encoding) compression strategy.
//!
//! A faithful, 100% safe-Rust port of C zlib's `deflate_rle()`
//! (`deflate.c` L2084-2149). `Z_RLE` is the degenerate case of LZ77 in which
//! the match distance is constrained to **exactly 1**: instead of searching the
//! hash chains for the longest match anywhere in the window, the engine only
//! ever extends a run of the single byte immediately preceding `strstart`
//! (`window[strstart - 1]`). This is precisely run-length encoding expressed in
//! the DEFLATE symbol alphabet — a length/distance pair whose distance code is
//! always the one for distance 1.
//!
//! `Z_RLE` is dispatched by `strategy.rs::deflate_dispatch` whenever the caller
//! sets `strategy == Z_RLE`. It is fast (it performs no hash-chain search) and
//! is the strategy intended for data such as PNG image-row filters, where the
//! productive matches are almost always at distance 1.
//!
//! # Byte-identical output (AAP §0.6.7, §0.7.1)
//!
//! The run-length boundary and the distance-always-1 tally are reproduced
//! **mechanically** from C so that the emitted bitstream is byte-for-byte
//! identical to C zlib for the same input under `Z_RLE`. The only non-trivial
//! translation is the run-length scan; the inline equivalence proof on that
//! loop shows that the safe `while`-scan computes exactly the same
//! `match_length` as C's unrolled pointer scan, with the boundary preserved
//! down to the last byte.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`** (AAP §0.6.2) and is `no_std`-clean:
//! it uses only safe slice indexing on the owned `window` buffer and the safe
//! tally/flush helpers from sibling modules. Out-of-bounds reads are impossible
//! because every window index is provably within `window.len() == 2 * w_size`
//! (see the bounds note on the scan loop).

use crate::constants::FlushMode;
use crate::deflate::flush_block;
use crate::deflate::state::{BlockState, DeflateStream, MAX_MATCH, MIN_MATCH};
use crate::deflate::trees;

/// Compress the available input using the `Z_RLE` strategy, emitting matches
/// whose distance is always exactly 1 (run-length encoding).
///
/// This is the safe-Rust port of C `deflate_rle` (`deflate.c` L2084-2149).
///
/// The function loops over the input window, and for each position:
///
/// 1. Refills the window via [`DeflateStream::fill_window`] when the lookahead
///    drops to `MAX_MATCH` or below, bailing out with [`BlockState::NeedMore`]
///    if more input is required and the caller did not request a flush.
/// 2. Measures the run of the byte immediately preceding `strstart`
///    (distance 1), capped at `MAX_MATCH` and then at the remaining
///    `lookahead`.
/// 3. Tallies either a distance-1 match (when the run reaches `MIN_MATCH`) or a
///    single literal, advancing `strstart`/`lookahead` accordingly.
/// 4. Flushes the current block whenever the symbol buffer fills.
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — ran out of input/output before completing the
///   block and no flush was requested.
/// * [`BlockState::FinishStarted`] / [`BlockState::NeedMore`] — propagated from
///   [`flush_block`] when the output buffer fills mid-block.
/// * [`BlockState::FinishDone`] — `flush == Z_FINISH` and the final block was
///   emitted in full.
/// * [`BlockState::BlockDone`] — a non-final flush completed (or there was
///   nothing left to do).
pub(crate) fn deflate_rle(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // `true` when tallying the current symbol filled the symbol buffer, so the
    // current block must be flushed (C `int bflush`).
    let mut bflush: bool;

    loop {
        // Make sure we always have enough lookahead, except at the end of the
        // input. We need MAX_MATCH bytes for the longest run, plus one for C's
        // unrolled scan loop. (deflate.c L2090-2100)
        if s.state.lookahead <= MAX_MATCH {
            s.fill_window();
            if s.state.lookahead <= MAX_MATCH && flush == FlushMode::NoFlush {
                return BlockState::NeedMore;
            }
            if s.state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // See how many times the previous byte repeats. (deflate.c L2102-2121)
        s.state.match_length = 0;
        if s.state.lookahead >= MIN_MATCH && s.state.strstart > 0 {
            let strstart = s.state.strstart;
            // `prev` is the byte at distance one (C `prev = *scan`, where
            // `scan = window + strstart - 1`). `strstart > 0` makes the index
            // valid; the three reads below are valid because lookahead has at
            // least MIN_MATCH (3) bytes here.
            let prev = s.state.window[strstart - 1];
            if prev == s.state.window[strstart]
                && prev == s.state.window[strstart + 1]
                && prev == s.state.window[strstart + 2]
            {
                // A run of at least MIN_MATCH is confirmed; extend it up to
                // MAX_MATCH.
                //
                // Equivalence with C (deflate.c L2108-2115): C sets
                //   strend = window + strstart + MAX_MATCH
                // advances `scan` past every byte equal to `prev`, then sets
                //   match_length = MAX_MATCH - (strend - scan).
                // That value equals the number of consecutive bytes == `prev`
                // starting at `window[strstart]`, capped at MAX_MATCH — exactly
                // what `run` counts here. The boundary is identical: for a full
                // run C lands `scan` precisely on `strend` (MAX_MATCH - 3 byte
                // steps after the three gating comparisons) and the trailing
                // `scan < strend` test caps the result at MAX_MATCH, so this
                // loop must NOT shift the boundary by one.
                //
                // Bounds: `run` reaches at most MAX_MATCH - 1 = 257, so the
                // largest index read is `strstart + 257`. With `strstart`
                // bounded by `2 * w_size - MIN_LOOKAHEAD` (the fill_window
                // invariant) and `window.len() == 2 * w_size`, this is always
                // in bounds. `fill_window` zero-initializes the window tail up
                // to WIN_INIT bytes past the data, so reads beyond the valid
                // lookahead are deterministic — and the `lookahead` cap below
                // discards any run measured into that padding, exactly as C
                // relies on the same window padding.
                let mut run = MIN_MATCH; // window[strstart..strstart + 3] already matched
                while run < MAX_MATCH && s.state.window[strstart + run] == prev {
                    run += 1;
                }
                s.state.match_length = run;
                if s.state.match_length > s.state.lookahead {
                    s.state.match_length = s.state.lookahead;
                }
            }
        }

        // Emit a match if we have a run of MIN_MATCH or longer, else a literal.
        // (deflate.c L2123-2138)
        if s.state.match_length >= MIN_MATCH {
            // Distance is always the literal 1 for RLE; the length code is
            // `match_length - MIN_MATCH`. The length is hoisted into a local so
            // the `&mut s.state` reborrow passed to `tr_tally_dist` does not
            // overlap the read of `s.state.match_length` in the same call.
            let length_code = (s.state.match_length - MIN_MATCH) as u8;
            bflush = trees::tr_tally_dist(s.state, 1, length_code);

            s.state.lookahead -= s.state.match_length;
            s.state.strstart += s.state.match_length;
            s.state.match_length = 0;
        } else {
            // No run of MIN_MATCH: output a single literal byte. This is
            // `window[strstart]` (the current byte), NOT `window[strstart - 1]`.
            let lc = s.state.window[s.state.strstart];
            bflush = trees::tr_tally_lit(s.state, lc);
            s.state.lookahead -= 1;
            s.state.strstart += 1;
        }

        // FLUSH_BLOCK(s, 0): emit the pending block; if the output buffer filled
        // (`flush_block` returns `Some`), bail out with that block state. The
        // `&&` short-circuit keeps `flush_block` from running unless `bflush`,
        // exactly like the C `if (bflush) FLUSH_BLOCK(s, 0);`.
        if bflush && let Some(bs) = flush_block(s, false) {
            return bs;
        }
    }

    // RLE never seeds the hash table for future matches. (deflate.c L2141)
    s.state.insert = 0;

    if flush == FlushMode::Finish {
        // FLUSH_BLOCK(s, 1): a `Some` return means the output filled mid-flush
        // (C returns finish_started); otherwise the stream is fully finished.
        if let Some(bs) = flush_block(s, true) {
            return bs;
        }
        return BlockState::FinishDone;
    }

    // Flush a trailing partial block if any symbols are still pending. The `&&`
    // short-circuit mirrors C's `if (s->sym_next) FLUSH_BLOCK(s, 0);`.
    if s.state.sym_next != 0
        && let Some(bs) = flush_block(s, false)
    {
        return bs;
    }

    BlockState::BlockDone
}
