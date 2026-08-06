//! `deflate_rle` -- run-length encoding: matches at distance one only.
//!
//! A verbatim port of `deflate_rle` (`deflate.c` L2084-L2153), the compressor `Z_RLE` selects.
//! It looks for runs of a repeated byte and emits them as matches whose distance is always 1,
//! which is what makes it useful for PNG-style filtered data; per the comment at
//! `deflate.c` L2080-L2083 it maintains no hash table, and the table "will be regenerated if
//! this run of deflate switches away from `Z_RLE`".
//!
//! # Why the strategy reaches this and the level cannot
//!
//! `configuration_table` (`deflate.c` L112-L124) never names `deflate_rle`; the only route to it
//! is the strategy test in `deflate()`'s dispatch chain (L1217-L1220), reproduced by
//! `CompressFunc::select`, and level 0 is tested first there.
//!
//! # The run scan, and why its shape is preserved exactly
//!
//! C walks the run with a pointer and an eight-way unrolled `do {} while` whose condition is one
//! short-circuiting `&&` chain (L2107-L2116):
//!
//! ```text
//! scan = s->window + s->strstart - 1;
//! prev = *scan;
//! if (prev == *++scan && prev == *++scan && prev == *++scan) {
//!     strend = s->window + s->strstart + MAX_MATCH;
//!     do {
//!     } while (prev == *++scan && prev == *++scan &&
//!              prev == *++scan && prev == *++scan &&
//!              prev == *++scan && prev == *++scan &&
//!              prev == *++scan && prev == *++scan &&
//!              scan < strend);
//!     s->match_length = MAX_MATCH - (uInt)(strend - scan);
//!     if (s->match_length > s->lookahead)
//!         s->match_length = s->lookahead;
//! }
//! ```
//!
//! The unrolling is not decoration: the length is computed as `MAX_MATCH - (strend - scan)`, so
//! it depends on precisely where the pointer stops, and the pointer stops at the *first*
//! comparison that fails because `&&` short-circuits. The port therefore reproduces both the
//! group of eight and the position of the `scan < strend` test at the end of the chain -- a
//! plain byte-at-a-time loop with the bound checked first would stop one place earlier at the
//! end of the window and shorten the final match of a stream.
//!
//! Two structural facts make the arithmetic exact. The scan resumes from `strstart + 2` after
//! the three opening comparisons, `strend` is `strstart + MAX_MATCH`, and
//! `MAX_MATCH - 2 == 256 == 8 * 32`, so a run that never breaks lands the cursor *exactly* on
//! `strend` and yields `MAX_MATCH`. And because a completed group always advances by eight from
//! `strstart + 2`, a mid-group mismatch can never carry the cursor past `strend` either. The
//! subtraction is consequently never negative, which is what C relies on when it casts to
//! `uInt`.
//!
//! Reads beyond `strstart + lookahead` are deliberate and are the reason
//! [`crate::weak_slice::Window::initialize_win_init_tail`] exists: the comment at
//! `deflate.c` L2090-L2093 asks for `MAX_MATCH` bytes "plus one for the unrolled loop", and the
//! zeroed tail guarantees the extra byte is defined rather than uninitialised. The match is
//! clamped back to `lookahead` immediately afterwards (L2115-L2116), so a run measured into that
//! tail can never be emitted.
//!
//! # Safety posture
//!
//! No `unsafe`, no raw pointer and no unchecked index. Every window read goes through
//! [`crate::weak_slice::Window::byte`], which returns [`None`] outside the buffer -- a case this
//! function treats as a mismatch, and which a live stream cannot reach because `fill_window`
//! maintains `strstart <= window_size - MIN_LOOKAHEAD` (`deflate.c` L374-L375), leaving 262
//! bytes of slack where the scan needs 259.

use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::state::{Allocator, DeflateState, MAX_MATCH, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;
use crate::weak_slice::Window;

/// How many bytes one iteration of C's unrolled `do {} while` compares (`deflate.c` L2111-L2115).
///
/// Eight, and it must stay eight: the run length is derived from where the cursor stops, and the
/// cursor can only stop inside a group of this size.
const UNROLL: usize = 8;

/// Measures the run of `prev` that starts at `strstart`, in the exact shape of C's scan.
///
/// Returns the cursor position C's `scan` would hold when its `&&` chain gives out, having
/// already performed the three opening comparisons; the caller turns that into a length. `base`
/// is `strstart`, `strend` is `base + MAX_MATCH`, and the returned index is always in
/// `base + 2 ..= strend`.
///
/// Ported from the `do {} while` at `deflate.c` L2109-L2115.
#[inline]
fn scan_run<S>(window: &Window<S>, base: usize, prev: u8) -> usize
where
    S: AsRef<[u8]> + AsMut<[u8]>,
{
    // `strend = s->window + s->strstart + MAX_MATCH;`  (L2109)
    let strend = base.saturating_add(MAX_MATCH);

    // The cursor after `prev == *++scan` has succeeded three times (L2108).
    let mut scan = base.saturating_add(2);

    loop {
        // One unrolled group: `prev == *++scan` eight times, short-circuiting.
        let mut matched_whole_group = true;
        for _ in 0..UNROLL {
            let next = scan.saturating_add(1);
            // `None` means the index left the window buffer, which a live stream cannot
            // reach (see the module documentation). Treating it as a mismatch stops the scan
            // where it stands, which shortens the match rather than inventing one.
            if window.byte(next) == Some(prev) {
                scan = next;
            } else {
                scan = next;
                matched_whole_group = false;
                break;
            }
        }

        // `&& scan < strend` -- the last term of the chain, evaluated only when all eight
        // comparisons succeeded, which is what keeps the cursor inside the group above.
        if !matched_whole_group || scan >= strend {
            return scan.min(strend);
        }
    }
}

/// Compresses by emitting runs as matches of distance one.
///
/// Port of `deflate_rle` (`deflate.c` L2084-L2153).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] -- the lookahead is short under `Z_NO_FLUSH`, or the output buffer
///   filled while flushing a block.
/// * [`BlockState::FinishStarted`] -- `Z_FINISH` was requested and the final block did not fit.
/// * [`BlockState::FinishDone`] -- `Z_FINISH` was requested and the final block was written.
/// * [`BlockState::BlockDone`] -- a block boundary was reached for one of the other flush modes.
// `_tr_tally` keeps the C spelling of `deflate.h` L317; see `trees/mod.rs` for the same
// relaxation.
#[allow(clippy::used_underscore_items)]
pub(crate) fn deflate_rle<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {`  (L2089)
    loop {
        // "Make sure that we always have enough lookahead, except at the end of the input
        // file. We need MAX_MATCH bytes for the longest run, plus one for the unrolled loop."
        // (L2090-L2100)
        //
        // ```text
        // if (s->lookahead <= MAX_MATCH) {
        //     fill_window(s);
        //     if (s->lookahead <= MAX_MATCH && flush == Z_NO_FLUSH) {
        //         return need_more;
        //     }
        //     if (s->lookahead == 0) break; /* flush the current block */
        // }
        // ```
        //
        // `<= MAX_MATCH` rather than `< MIN_LOOKAHEAD`: this is the "plus one" the unrolled
        // loop needs, and it is a *stronger* precondition than `fill_window`'s IN assertion
        // requires, since `MAX_MATCH` (258) is below `MIN_LOOKAHEAD` (262).
        if state.window.lookahead <= MAX_MATCH {
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            if state.window.lookahead <= MAX_MATCH && matches!(flush, Flush::NoFlush) {
                return BlockState::NeedMore;
            }
            if state.window.lookahead == 0 {
                break;
            }
        }

        // "See how many times the previous byte repeats"  (L2102-L2120)
        //
        // `s->match_length = 0;` first, unconditionally, so a run shorter than `MIN_MATCH`
        // leaves the field clear for the literal branch below.
        state.match_length = 0;

        // `if (s->lookahead >= MIN_MATCH && s->strstart > 0) {`  (L2104)
        //
        // `strstart > 0` is what makes `strstart - 1` -- the byte the run is measured against
        // -- a legitimate index; at the very start of a stream there is no previous byte.
        if state.window.lookahead >= MIN_MATCH && state.window.strstart > 0 {
            let base = state.window.strstart;

            // `scan = s->window + s->strstart - 1; prev = *scan;`  (L2105-L2106)
            //
            // The `saturating_sub` is exact here because `strstart > 0` was just tested, and
            // the read cannot fail because `strstart` itself indexes a live byte.
            let prev = state.window.byte(base.saturating_sub(1));

            // `if (prev == *++scan && prev == *++scan && prev == *++scan) {`  (L2107)
            //
            // The three opening comparisons, in C's order and with C's short-circuiting: a run
            // must reach `MIN_MATCH` before the unrolled loop is entered at all.
            let opens_a_run = prev.is_some()
                && state.window.byte(base) == prev
                && state.window.byte(base.saturating_add(1)) == prev
                && state.window.byte(base.saturating_add(2)) == prev;

            if let (true, Some(prev)) = (opens_a_run, prev) {
                let strend = base.saturating_add(MAX_MATCH);
                let scan = scan_run(&state.window, base, prev);

                // `s->match_length = MAX_MATCH - (uInt)(strend - scan);`  (L2117)
                //
                // `scan <= strend` always holds (see the module documentation), so the
                // subtraction is exact and `saturating_sub` only documents that fact.
                state.match_length = MAX_MATCH.saturating_sub(strend.saturating_sub(scan));

                // `if (s->match_length > s->lookahead) s->match_length = s->lookahead;`
                // (L2118-L2119)
                //
                // The clamp that keeps a run measured into the zeroed `WIN_INIT` tail from
                // being emitted.
                if state.match_length > state.window.lookahead {
                    state.match_length = state.window.lookahead;
                }
            }

            // The `Assert(scan <= s->window + (uInt)(s->window_size - 1), "wild scan")` at
            // L2121-L2122 has no counterpart: `Window::byte` enforces the same bound on every
            // read, which is the whole point of routing the scan through it.
        }

        // "Emit match if have run of MIN_MATCH or longer, else emit literal"  (L2124-L2138)
        let bflush = if state.match_length >= MIN_MATCH {
            // `check_match(s, s->strstart, s->strstart - 1, (int)s->match_length);` (L2126) is
            // `ZLIB_DEBUG`-only and has no counterpart here.
            //
            // `_tr_tally_dist(s, 1, s->match_length - MIN_MATCH, bflush);`  (L2128)
            //
            // The distance is the literal 1 that gives this strategy its name. The length code
            // operand is `match_length - MIN_MATCH`, at most `258 - 3`, so it fits a `u8`; the
            // conversion cannot fail because `match_length` was clamped to `lookahead` and can
            // never exceed `MAX_MATCH`; `u8::MAX` is that ceiling exactly, so the saturating
            // fallback is the same value the conversion would produce anyway.
            let length_code =
                u8::try_from(state.match_length.saturating_sub(MIN_MATCH)).unwrap_or(u8::MAX);
            let bflush = _tr_tally(state, 1, length_code);

            // `s->lookahead -= s->match_length; s->strstart += s->match_length;
            //  s->match_length = 0;`  (L2130-L2132)
            state.window.lookahead = state.window.lookahead.saturating_sub(state.match_length);
            state.window.strstart = state.window.strstart.saturating_add(state.match_length);
            state.match_length = 0;

            bflush
        } else {
            // "No match, output a literal byte"  (L2134-L2137)
            //
            // `_tr_tally_lit(s, s->window[s->strstart], bflush); s->lookahead--; s->strstart++;`
            //
            // The read cannot fail: `lookahead` is non-zero at this point in every path that
            // reaches here. Declining to emit leaves the block untouched and falls through to
            // the flush decision, rather than inventing a literal.
            let Some(literal) = state.window.byte(state.window.strstart) else {
                break;
            };
            let bflush = _tr_tally(state, 0, literal);

            state.window.lookahead = state.window.lookahead.saturating_sub(1);
            state.window.strstart = state.window.strstart.saturating_add(1);

            bflush
        };

        // `if (bflush) FLUSH_BLOCK(s, 0);`  (L2139)
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // `s->insert = 0;`  (L2141)
    //
    // Zero, not `MIN_MATCH - 1`: nothing was inserted into the hash table, so a later
    // `deflateParams` switch has no partial insertion to resume.
    state.window.insert = 0;

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L2142-L2145)
    if matches!(flush, Flush::Finish) {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L2146-L2147)
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L2148)
    BlockState::BlockDone
}
