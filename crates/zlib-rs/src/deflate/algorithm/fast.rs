//! The greedy compressor: `deflate_fast`, the strategy for levels 1 to 3.
//!
//! Mirrors `deflate_fast` (`deflate.c` L1857-L1948), whose own header comment states the
//! contract (L1850-L1856):
//!
//! ```text
//! Compress as much as possible from the input stream, return the current
//! block state.
//! This function does not perform lazy evaluation of matches and inserts
//! new strings in the dictionary only for unmatched strings or for short
//! matches. It is used only for the fast compression options.
//! ```
//!
//! It is reached through rows 1, 2 and 3 of `configuration_table` -- `{4, 4, 8, 4}`,
//! `{4, 5, 16, 8}` and `{4, 6, 32, 32}` (L115-L117) -- which
//! `crate::deflate::config_table` holds and
//! [`CompressFunc::select`](crate::deflate::algorithm::CompressFunc::select) dispatches on
//! (L1217-L1220). Levels 4 to 9 use `deflate_slow` instead, whose header comment introduces
//! it as "Same as above, but achieves better compression" (L1951-L1954) -- "above" being this
//! function, which is why the two share a skeleton and must nevertheless not be unified. See
//! *Not the same as `deflate_slow`* below.
//!
//! # ⚠ THIS FILE IS PART OF THE OUTPUT CONTRACT -- DO NOT "IMPROVE" IT ⚠
//!
//! RFC 1951 constrains the *format* of a DEFLATE stream, not the *choices* an encoder makes
//! within it, and this implementation is required to emit the same bytes as the reference for every
//! configuration. Four of those choices are made here, and every one of them is observable in
//! the compressed output:
//!
//! * **Whether a candidate is even considered** -- the two-condition test at L1886, which
//!   rejects a chain head of `NIL` and a distance above `MAX_DIST`. The reference records why
//!   the `NIL` test exists: "To simplify the code, we prevent matches with the string of
//!   window index 0 (in particular we have to avoid a match of the string with itself at the
//!   start of the input file)" (L1887-L1890).
//! * **Whether a found match is adopted** -- `match_length >= MIN_MATCH` at L1894, taken
//!   *without* resetting `match_length` first. See *`match_length` is not reset* below.
//! * **Whether the matched span enters the hash chains** -- `match_length <= max_insert_length`
//!   at L1906. This one perturbs *future* match finding, not merely the current symbol: the
//!   positions it inserts, or declines to insert, are the candidates every later
//!   `longest_match` walk can see.
//! * **Where the block ends** -- `if (bflush) FLUSH_BLOCK(s, 0)` at L1938 and the three-way
//!   tail at L1940-L1947.
//!
//! The reference is explicit that the third of those is a deliberate trade rather than an
//! oversight: "Insert new strings in the hash table only if the match length is not too large.
//! This saves time but degrades compression" (L1902-L1904). A strictly better encoder fails
//! the acceptance criterion, so that trade is reproduced exactly.
//!
//! # What this function does, statement for statement
//!
//! | C | `deflate.c` | Here |
//! |---|---|---|
//! | lookahead top-up | L1867-L1873 | [`fill_window`] behind two flat sibling tests |
//! | `INSERT_STRING(s, s->strstart, hash_head)` | L1878-L1881 | [`insert_string`], whose result *is* `hash_head` |
//! | candidate range test | L1886 | [`IPos::match_index`] plus `wrapping_sub` against `max_dist` |
//! | `longest_match` | L1891 | [`longest_match`], which sets `match_start` itself |
//! | `_tr_tally_dist` | L1897-L1898 | [`_tr_tally`] with a non-zero distance |
//! | insert the matched span | L1905-L1917 | the `do`/`while` implementation, with `insert_string_no_head` |
//! | skip the matched span | L1919-L1930 | `strstart += match_length` plus [`seed_hash`] |
//! | `_tr_tally_lit` | L1934 | [`_tr_tally`] with distance zero |
//! | `FLUSH_BLOCK(s, 0)` | L1938 | [`flush_block!`] |
//! | `s->insert = ...` | L1940 | `strstart.min(MIN_MATCH - 1)` |
//! | `check_match` | L1895 | nothing: `ZLIB_DEBUG` is undefined, and L1623 defines the macro away |
//! | `Tracevv` | L1933 | nothing, for the same reason |
//!
//! # Not the same as `deflate_slow`
//!
//! Three differences are load-bearing, and each is easy to lose because the two functions are
//! otherwise so alike that a reader may assume one is a copy of the other:
//!
//! * **The candidate test has two conditions here and three there.** `deflate_slow` adds
//!   `prev_length < max_lazy_match` (L1988-L1989), which is meaningless without lazy
//!   evaluation. The same struct field is read here under its other name -- see below.
//! * **`match_length` is not reset.** `deflate_slow` assigns
//!   `s->prev_length = s->match_length, s->prev_match = s->match_start;` and then
//!   `s->match_length = MIN_MATCH-1;` before its own candidate test (L1985-L1986). This
//!   function does not, so when the test at L1886 fails, `match_length` still holds whatever
//!   the previous iteration left: `0` after the skip path at L1921, `0` after the insertion
//!   loop at L1915, and `MIN_MATCH - 1` from `lm_init` (L698) on the very first call. All
//!   three are below `MIN_MATCH`, which is what makes the reference correct without the reset
//!   -- and adding a defensive one anyway would be a behavioural change, not a safety
//!   improvement.
//! * **The lookahead test is shaped differently in all five leaves.** This one is
//!   `lookahead < MIN_LOOKAHEAD` followed by two *sibling* tests (L1867-L1873);
//!   `deflate_rle` tests `lookahead <= MAX_MATCH` (L2094) and `deflate_huff` tests
//!   `lookahead == 0` with the second test *nested* (L2160-L2166). They must not be
//!   harmonised.
//!
//! # `max_insert_length` is `max_lazy_match`
//!
//! `deflate.h` L186 is `#define max_insert_length max_lazy_match`: one field with two names.
//! The reference reads it as `max_lazy_match` for levels 4 and above ("Attempt to find a
//! better match only when the current match is strictly smaller than this value", L182-L184)
//! and as `max_insert_length` for levels 3 and below ("Insert new strings in the hash table
//! only if the match length is not greater than this length", L187-L189). This implementation has one
//! field, `DeflateState::max_lazy_match`, and
//! [`DeflateState::max_insert_length`](crate::deflate::state::DeflateState::max_insert_length)
//! is the accessor that lets the code below be written the way the C is.
//!
//! # Uninitialised memory is the normal case
//!
//! The window arrives from `malloc`, not `calloc` (`zutil.c` L299-L303 selects `malloc`
//! whenever `sizeof(uInt) > 2`), and `deflateInit2_` zeroes only the state struct
//! (`deflate.c` L442) -- never `window`, `prev`, `head` or `pending_buf`. Nothing here may
//! assume a zeroed byte. Two reads deliberately go past the data: `window[strstart + 1]` at
//! L1923 and, inside `longest_match`, the scan past `strstart + lookahead`. Both are bounded
//! by `fill_window`'s `WIN_INIT` zeroing of the region after the data end (L340-L375), which
//! is the invariant this function relies on instead of clamping -- a private clamp would
//! change which matches are found. `test/infcover.c` L87 fills every allocation with `0xa5`
//! precisely to catch code that assumes otherwise, and the debug builds of this crate do the
//! same.
//!
//! # What is deliberately not implemented
//!
//! Only the default compile-time configuration, and none of the following is offered as a
//! runtime option or a Cargo feature, because each one changes the emitted bytes:
//!
//! * **`FASTEST`** (L106). It would replace the ten-entry configuration table with two, drop
//!   `deflate_slow` entirely, use the chain-less `INSERT_STRING` of L155-L158 and the
//!   alternative `longest_match` of L1537-L1586. Note that the `#ifndef FASTEST` at L1905 is
//!   *inside* this function and is therefore the branch that **is** implemented: it is the live
//!   default path.
//! * **`LIT_MEM`** (`deflate.h` L28, commented out). The symbol buffer here is the single
//!   `sym_buf` of `LIT_BUFS` 4, not the split `d_buf`/`l_buf` of `LIT_BUFS` 5.
//! * **`UNALIGNED_OK`**, whose word-at-a-time comparison lives in `longest_match` and is not
//!   part of this implementation either.
//! * **`ZLIB_DEBUG`**, so `check_match` (L1623) and every `Trace*` compile to nothing, and
//!   the tally macros of `deflate.h` L357-L375 are the live definitions rather than the
//!   function fallback at L378-L380.

use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::hash_chain::{insert_string, insert_string_no_head, seed_hash};
use crate::deflate::longest_match::longest_match;
use crate::deflate::state::{Allocator, DeflateState, IPos, MIN_LOOKAHEAD, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Compresses as much as possible from `cursors.input`, returning the resulting block state.
///
/// Mirrors `local block_state deflate_fast(deflate_state *s, int flush)`
/// (`deflate.c` L1857-L1948): the greedy, non-lazy compressor used for levels 1 to 3. Every
/// decision it makes is part of the compressed output; see this module's documentation for
/// the four that matter and for why none of them may be changed.
///
/// `cursors` stands in for C's `s->strm`, which this implementation cannot hold as a field --
/// see [`StreamCursors`]. `flush` is never `Z_TREES`: `deflate()` rejects that before it
/// dispatches (L985), and
/// [`CompressFunc::call`](crate::deflate::algorithm::CompressFunc::call) asserts as much.
///
/// # Return value
///
/// * [`BlockState::NeedMore`] -- more input or more output is required (L1870, and from
///   [`flush_block!`] when the output buffer filled).
/// * [`BlockState::BlockDone`] -- the input is exhausted and a block boundary was reached
///   (L1947).
/// * [`BlockState::FinishStarted`] -- from [`flush_block!`] when the final block did not fit
///   in the caller's output buffer.
/// * [`BlockState::FinishDone`] -- `Z_FINISH` completed (L1943).
///
/// Errors do not exist on this path: no failure mode the reference reports flows through
/// here, so the block state and the fields of `state` carry everything the driver needs, and
/// nothing panics.
// `_tr_tally` keeps the underscore-prefixed C spelling of `deflate.h` L312 so that a reader can
// grep from either language to the other, which is what `pedantic`'s `used_underscore_items`
// targets. Carried on every site in this crate that calls one of the six `_tr_*` functions --
// `deflate/algorithm.rs`, `trees/mod.rs` and the test modules of `trees/build.rs` and
// `deflate/window.rs` -- so that the relaxation does not depend on which clippy release is
// running.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items)]
pub(crate) fn deflate_fast<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {`  (L1861)
    loop {
        // "We need MAX_MATCH bytes for the next match, plus MIN_MATCH bytes to insert the
        // string following the next match" (L1863-L1865), which is what `MIN_LOOKAHEAD`
        // (262) counts.
        if state.window.lookahead < MIN_LOOKAHEAD {
            // `fill_window(s);`  (L1868). The IN assertion it makes -- that the lookahead is
            // genuinely short -- is exactly the test above.
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            // `if (s->lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH) return need_more;`
            // (L1869-L1871). Still short after a top-up and the caller is not asking for a
            // block: wait for more input rather than compress with a truncated lookahead,
            // which would find shorter matches than the reference does.
            if state.window.lookahead < MIN_LOOKAHEAD && flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }

            // `if (s->lookahead == 0) break;`  (L1872) -- "flush the current block".
            //
            // A *sibling* of the test above, not nested inside it: the two together are what
            // lets a `Z_FINISH` with a short tail fall through to the loop body and compress
            // it. `deflate_huff` nests its equivalent (L2160-L2166); the shapes differ on
            // purpose.
            if state.window.lookahead == 0 {
                break;
            }
        }

        // "Insert the string window[strstart .. strstart + 2] in the dictionary, and set
        // hash_head to the head of the hash chain" (L1875-L1877).
        //
        // `IPos hash_head;` (L1858) followed by `hash_head = NIL;` (L1878). The `NIL` default
        // is what makes the test at L1886 fail when the string is too short to hash, so it is
        // assigned before the conditional rather than inside it.
        let mut hash_head = IPos::NIL;

        // `if (s->lookahead >= MIN_MATCH) { INSERT_STRING(s, s->strstart, hash_head); }`
        // (L1879-L1881). `insert_string` performs all four steps of the macro and returns the
        // value C's chained assignment leaves in `hash_head`: the *previous* head of the
        // chain, read before this position was pushed onto it.
        if state.window.lookahead >= MIN_MATCH {
            let str_pos = state.window.strstart;
            hash_head = insert_string(state, str_pos);
        }

        // `if (hash_head != NIL && s->strstart - hash_head <= MAX_DIST(s))`  (L1886)
        //
        // Exactly two conditions -- `deflate_slow` has three (L1988-L1989) and the two tests
        // must not be unified. `IPos::match_index` is the `!= NIL` half: it yields [`None`]
        // for a chain terminator, which is also window index 0, the position the reference
        // deliberately makes unmatchable (L1887-L1890).
        //
        // `wrapping_sub` reproduces C's unsigned subtraction. `hash_head` never exceeds
        // `strstart` for a live stream -- `fill_window` slides the chain entries with the
        // window -- and where it somehow did, C's wrap would produce a value far above
        // `MAX_DIST` and fail the test. Wrapping here does the same on a `usize`, whereas a
        // plain subtraction would panic in a debug build on input C merely declines.
        //
        // `MAX_DIST(s)` is `w_size - MIN_LOOKAHEAD` (`deflate.h` L301), read out before the
        // test only so that the closure borrows two integers rather than the whole state.
        let strstart = state.window.strstart;
        let max_dist = state.max_dist();
        let in_range = hash_head
            .match_index()
            .is_some_and(|candidate| strstart.wrapping_sub(candidate) <= max_dist);

        if in_range {
            // `s->match_length = longest_match (s, hash_head);`  (L1891)
            //
            // "longest_match() sets match_start" (L1892) -- it writes the field itself, and
            // this function must not, because `longest_match` only writes it when it actually
            // improves on `prev_length`.
            state.match_length = longest_match(state, hash_head);
        }

        // `int bflush;` (L1859): "set if current block must be flushed". Left without an
        // initialiser exactly as C leaves it, because both arms below assign it and the
        // compiler proves that; a placeholder value would be a dead store that could hide a
        // missing assignment.
        let bflush;

        // `if (s->match_length >= MIN_MATCH) {`  (L1894)
        //
        // Note what is *not* here: `match_length` is never reset before this test. See the
        // module documentation for why the three values it can hold are all below
        // `MIN_MATCH`, and why adding a reset would change the output.
        if state.match_length >= MIN_MATCH {
            // `check_match(s, s->strstart, s->match_start, (int)s->match_length);` (L1895)
            // is a `ZLIB_DEBUG`-only validation that L1623 defines away in the shipped
            // build. Nothing to implement.

            // `_tr_tally_dist(s, s->strstart - s->match_start,`
            // `               s->match_length - MIN_MATCH, bflush);`  (L1897-L1898)
            //
            // The distance is C's unsigned subtraction, wrapping for the same reason as the
            // range test above; `match_start` is behind `strstart` for every match
            // `longest_match` reports. The length is normalised by `MIN_MATCH`, which the
            // branch condition proves cannot underflow.
            let distance = state.window.strstart.wrapping_sub(state.window.match_start);
            let length = state.match_length.saturating_sub(MIN_MATCH);

            // `ush dist = (ush)(distance);` and `uch len = (uch)(length);`
            // (`deflate.h` L366-L367): the two narrowing casts the macro performs *before*
            // the symbol is recorded. Written as a mask plus an infallible conversion so
            // that the truncation is C's and the fallback is unreachable rather than
            // panicking: a distance is at most `MAX_DIST` (32506) and a normalised length at
            // most `MAX_MATCH - MIN_MATCH` (255), so neither mask discards a bit.
            let dist = u16::try_from(distance & 0xffff).unwrap_or(0);
            let len = u8::try_from(length & 0xff).unwrap_or(0);

            // The rest of `_tr_tally_dist`: the three `sym_buf` writes, `dist--`, and the two
            // frequency increments, in that order -- `sym_buf` records the *raw* distance and
            // `d_code` receives the decremented one. `crate::trees::_tr_tally` is the single
            // implementation of both spellings, since `deflate.h` L378-L380 defines the macro
            // *as* a call to it; its `matches` counter is the one field the inline form does
            // not touch, and it is unobservable here because `s->matches` is read only for a
            // level-0 stream (`deflate.c` L801), which never reaches this compressor.
            bflush = _tr_tally(state, dist, len);

            // `s->lookahead -= s->match_length;`  (L1900)
            //
            // Exact rather than merely safe: `longest_match` never reports a length above the
            // lookahead (L1528-L1529). `saturating_sub` is used so that no arithmetic here
            // can panic even in principle.
            state.window.lookahead = state.window.lookahead.saturating_sub(state.match_length);

            // `#ifndef FASTEST`  (L1905) -- the live default path.
            //
            // "Insert new strings in the hash table only if the match length is not too
            // large. This saves time but degrades compression." (L1902-L1904)
            if state.match_length <= state.max_insert_length()
                && state.window.lookahead >= MIN_MATCH
            {
                // `s->match_length--;` (L1908) -- "string at strstart already in table",
                // inserted by the `INSERT_STRING` at L1880. The field is then used
                // destructively as the loop counter and ends at zero, which the skip path
                // below assigns explicitly; both routes leave it below `MIN_MATCH` for the
                // next iteration's test at L1894.
                //
                // Saturating, and the saturation is unreachable: the branch at L1894 gives
                // `match_length >= MIN_MATCH`, so this cannot reach zero.
                state.match_length = state.match_length.saturating_sub(1);

                // -- L1909-L1915, a `do`/`while`: the body runs first, then the decrement,
                // then the test. Rust has no such loop, so it is a `loop` whose exit test is
                // last, which is the same control flow.
                loop {
                    // `s->strstart++;`  (L1910)
                    //
                    // "strstart never exceeds WSIZE-MAX_MATCH, so there are always MIN_MATCH
                    // bytes ahead" (L1912-L1914), which is what makes the insertion below
                    // legal. `saturating_add` cannot saturate: `strstart` stays below
                    // `window_size`, at most 65536.
                    state.window.strstart = state.window.strstart.saturating_add(1);

                    // `INSERT_STRING(s, s->strstart, hash_head);`  (L1911)
                    //
                    // C's macro assigns its third argument, so each iteration overwrites
                    // `hash_head`; nothing reads it again before L1878 assigns it afresh on
                    // the next iteration of the outer loop. `insert_string_no_head` is
                    // `insert_string` with that dead result dropped -- the head is still read
                    // internally, because `prev[str & w_mask]` is assigned from it -- which
                    // keeps the four steps in one place without a store Rust would flag as
                    // never read.
                    let str_pos = state.window.strstart;
                    insert_string_no_head(state, str_pos);

                    // `while (--s->match_length != 0);`  (L1915)
                    state.match_length = state.match_length.saturating_sub(1);
                    if state.match_length == 0 {
                        break;
                    }
                }

                // `s->strstart++;`  (L1916) -- the extra advance past the last inserted
                // position, outside the loop. Together with the pre-decrement at L1908 this
                // moves `strstart` by exactly the original `match_length` while inserting
                // `match_length - 1` positions; an off-by-one here changes which positions
                // are hashed, and therefore every subsequent match.
                state.window.strstart = state.window.strstart.saturating_add(1);
            } else {
                // The "not worth it" path -- L1919-L1930. Skip the matched span without
                // inserting any of it, and rebuild the running hash key from scratch at the
                // new position.

                // `s->strstart += s->match_length;`  (L1920)
                state.window.strstart = state.window.strstart.saturating_add(state.match_length);

                // `s->match_length = 0;`  (L1921)
                state.match_length = 0;

                // `s->ins_h = s->window[s->strstart];`  (L1922)
                // `UPDATE_HASH(s, s->ins_h, s->window[s->strstart + 1]);`  (L1923)
                //
                // `seed_hash` is those two lines; `fill_window` open-codes the identical pair
                // at L317-L318, which is why there is one implementation of them.
                //
                // The `#if MIN_MATCH != 3` block at L1924-L1926 is dead -- `MIN_MATCH` is 3
                // (`zutil.h` L92) -- and contains deliberately non-compiling text, so nothing
                // is implemented for it.
                //
                // "If lookahead < MIN_MATCH, ins_h is garbage, but it does not matter since
                // it will be recomputed at next deflate call" (L1927-L1929): the read at
                // `strstart + 1` may land past the data, in the region `fill_window` zeroes.
                // No guard is added here, because a guard would leave a *different* key
                // behind than the reference does.
                let str_pos = state.window.strstart;
                seed_hash(state, str_pos);
            }
        } else {
            // "No match, output a literal byte" -- L1931-L1937. The `Tracevv` at L1933 is
            // `ZLIB_DEBUG`-only.

            // `_tr_tally_lit(s, s->window[s->strstart], bflush);`  (L1934)
            //
            // The read is always in range: the loop only reaches this point with
            // `lookahead > 0`, and a non-zero lookahead means that many valid bytes start at
            // `strstart`. The `unwrap_or` keeps the function total; taking it would tally a
            // zero literal rather than panic, and it is unreachable.
            let literal = state.window.byte(state.window.strstart).unwrap_or(0);
            bflush = _tr_tally(state, 0, literal);

            // `s->lookahead--;` and `s->strstart++;`  (L1935-L1936). Exact: the lookahead is
            // non-zero here, as argued above.
            state.window.lookahead = state.window.lookahead.saturating_sub(1);
            state.window.strstart = state.window.strstart.saturating_add(1);
        }

        // `if (bflush) FLUSH_BLOCK(s, 0);`  (L1938)
        //
        // The symbol buffer is full, so the block must be emitted now. `flush_block!` is
        // `FLUSH_BLOCK` including its early `return`, which fires when the flush could not
        // empty the caller's output buffer.
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;`  (L1940)
    //
    // The number of positions at the end of the window whose strings are not yet in the hash
    // chains, which the next `fill_window` inserts once it has the bytes that follow them.
    // The conditional form is `min`, and it is specific to this compressor and
    // `deflate_slow`: `deflate_rle` (L2141) and `deflate_huff` (L2177) assign a plain zero.
    state.window.insert = state.window.strstart.min(MIN_MATCH - 1);

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L1941-L1944)
    if flush == Flush::Finish {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L1945-L1946) -- emit the partial block only if
    // it holds something.
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L1947)
    BlockState::BlockDone
}

#[cfg(test)]
// The panic-prone lints are relaxed because a test asserts, and an assertion panics; the
// fixtures below also index corpora at constant offsets and unwrap window reads whose bounds
// the line above them establishes. `clippy.toml` already exempts test code from these through
// `allow-unwrap-in-tests` and its siblings; stating them here as well is what
// `deflate/mod.rs`, `deflate/window.rs` and `trees/build.rs` do, so the relaxation does not
// depend on that configuration being read.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    //! Unit tests for the greedy compressor.
    //!
    //! Two kinds, and the first is the one that matters.
    //!
    //! **Byte identity against the reference.** The `EXPECTED_*` constants below are the exact
    //! output of the C `deflate()` in this repository, produced by compiling the in-tree
    //! sources and compressing each corpus with the same parameters. Only a comparison against
    //! the reference proves the requirement this file exists to satisfy: a round trip that
    //! merely decompresses proves nothing about byte identity, and RFC 1951 conformance is a
    //! far weaker property. These are a committed subset, not a substitute for
    //! the planned `crates/zlib-rs-differential/tests/byte_identical.rs`, which is to sweep the
    //! whole matrix and has not landed yet.
    //!
    //! **The individual decisions, observed directly.** A compressor called with `Z_NO_FLUSH`
    //! and a pre-loaded window returns [`BlockState::NeedMore`] *without* flushing, which
    //! leaves the symbol buffer and the hash chains intact for inspection. That is what lets
    //! the tests below assert which positions entered the chains -- the one decision here that
    //! byte identity on a short corpus does not pin down sharply, because its effect is on
    //! *future* matches.

    use super::deflate_fast;
    use crate::config::Z_UNKNOWN;
    use crate::crc32::crc32;
    use crate::deflate::algorithm::{BlockState, Flush, StreamCursors};
    use crate::deflate::hash_chain::{seed_hash, update_hash};
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Pos, DEF_MEM_LEVEL, MIN_LOOKAHEAD,
    };
    use crate::deflate::{deflate, deflate_bound_z, deflate_end, deflate_init2, deflate_reset};
    use crate::error::ReturnCode;
    use crate::read_buf::{InputCursor, OutputCursor};
    use alloc::vec;
    use alloc::vec::Vec;

    /// `Z_FINISH` (`zlib.h` L176), as `deflate()` takes it.
    const FINISH: i32 = 4;
    /// `Z_DEFLATED` (`zlib.h` L184), the only method.
    const DEFLATED: i32 = 8;
    /// `Z_DEFAULT_STRATEGY` (`zlib.h` L196).
    const DEFAULT_STRATEGY: i32 = 0;

    /// 300 copies of `b'a'`: one long run, so the first match saturates `MAX_MATCH` and the
    /// `max_insert_length` test at L1906 rejects it.
    fn long_run() -> Vec<u8> {
        vec![b'a'; 300]
    }

    /// `abcabcabc...`, 600 bytes: a three-byte period, so every position matches at distance
    /// three and the run of matches never ends.
    fn three_byte_period() -> Vec<u8> {
        (0..600u32).map(|i| b'a' + (i % 3) as u8).collect()
    }

    /// English text with one repeated sentence: short matches mixed with literals, which is
    /// the regime where the insertion loop at L1909-L1915 runs.
    const TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog. \
The quick brown fox jumps over the lazy dog. \
Pack my box with five dozen liquor jugs.";

    /// `abc` three times at increasing distances, with unique filler between: every match is
    /// exactly three bytes long, which is the shortest one `MIN_MATCH` admits.
    const SHORT_MATCH: &[u8] = b"abcdefabcqrstuvwxyzabcmnop";

    /// A linear-congruential byte stream: incompressible, so every symbol is a literal and
    /// `_tr_flush_block` falls back to stored blocks.
    ///
    /// `x = x * 1103515245 + 12345` over `u32`, taking the top byte -- the same recurrence,
    /// with the same seed and the same 32-bit wraparound, that generated the reference output.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut x: u32 = 12345;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 24) as u8
            })
            .collect()
    }

    /// 70000 bytes: a 4096-byte incompressible block repeated, which is more than twice the
    /// 32 KiB window and therefore forces `fill_window` to slide and `slide_hash` to run,
    /// while every repeat offers a long match at distance 4096.
    fn window_crossing() -> Vec<u8> {
        let block = incompressible(4096);
        (0..70_000).map(|i| block[i % block.len()]).collect()
    }

    /// A state configured as `deflateInit2(&strm, level, Z_DEFLATED, window_bits, mem_level,
    /// strategy)` and reset, which is the state `deflate()` first enters a compressor with.
    fn open(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: i32,
    ) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig::from_raw(level, DEFLATED, window_bits, mem_level, strategy)
            .expect("the fixture parameters are all in range");
        let mut state = deflate_init2(config, GlobalAllocator).expect("allocation");
        let _reset = deflate_reset(&mut state);
        state
    }

    /// Cursors over no input and `output`, with the scalars a freshly reset raw stream carries.
    fn cursors_over(output: &mut [u8]) -> StreamCursors<'static, '_> {
        StreamCursors {
            input: InputCursor::new(&[]),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// Puts `bytes` in the window exactly as `fill_window` would have: at offset zero, with
    /// `strstart` behind them, the running hash key primed from the first two, and the
    /// `WIN_INIT` tail past the data zeroed (`deflate.c` L317-L318 and L340-L375).
    ///
    /// This is what lets a test call [`deflate_fast`] directly with `Z_NO_FLUSH` and no input:
    /// the compressor consumes the window and returns [`BlockState::NeedMore`] as soon as the
    /// lookahead falls below `MIN_LOOKAHEAD`, without ever flushing, so the symbol buffer and
    /// the hash chains still describe what it decided.
    fn load_window(state: &mut DeflateState<'static, GlobalAllocator>, bytes: &[u8]) {
        assert!(state.window.write_at(0, bytes), "corpus fits the window");
        state.window.strstart = 0;
        state.window.lookahead = bytes.len();
        state.window.block_start = 0;
        state.window.insert = 0;
        state.window.initialize_win_init_tail();
        if bytes.len() >= 2 {
            seed_hash(state, 0);
        }
    }

    /// The hash key of the three-byte string at `pos`, built the way `fill_window` and
    /// `INSERT_STRING` build it: the first byte unmasked, then two folds.
    fn key_of(state: &DeflateState<'static, GlobalAllocator>, pos: usize) -> usize {
        let shift = state.hash_shift;
        let mask = state.hash_mask();
        let mut key = usize::from(state.window.byte(pos).unwrap());
        key = update_hash(shift, mask, key, state.window.byte(pos + 1).unwrap());
        update_hash(shift, mask, key, state.window.byte(pos + 2).unwrap())
    }

    /// The symbols tallied so far, as `(distance, length_or_literal)` pairs; a distance of zero
    /// marks a literal (`trees.c` L917-L918).
    fn symbols(state: &DeflateState<'static, GlobalAllocator>) -> Vec<(u16, u8)> {
        (0..state.pending.symbol_count())
            .map(|index| state.pending.symbol_at(index * 3).unwrap())
            .collect()
    }

    /// Compresses `input` in one `Z_FINISH` call through the real driver and returns the bytes.
    fn compress(
        input: &[u8],
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: i32,
    ) -> Vec<u8> {
        let mut state = open(level, window_bits, mem_level, strategy);
        let mut out = vec![0u8; deflate_bound_z(Some(&state), input.len()) + 64];
        let produced = {
            let mut stream = crate::deflate::DeflateStream::new(input, &mut out);
            stream.apply_reset(deflate_reset(&mut state));
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END,
                "a one-call Z_FINISH must complete within deflate_bound"
            );
            stream.next_out
        };
        assert_eq!(deflate_end(state), ReturnCode::OK);
        out.truncate(produced);
        out
    }

    /// Reference output of `deflate_fast` for `long_run()` at levels 1, 2 and 3, raw with
    /// `memLevel` 8. Two literals then one match of length 258 at distance 1, then a second
    /// match, then the end-of-block code.
    const EXPECTED_LONG_RUN: &[u8] = &[0x4b, 0x4c, 0x1c, 0x05, 0xc4, 0x86, 0x00, 0x00];

    /// Reference output for `three_byte_period()` at levels 1, 2 and 3, raw, `memLevel` 8.
    const EXPECTED_THREE_BYTE_PERIOD: &[u8] = &[
        0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x45, 0xa3, 0x21, 0x40, 0xed, 0x10, 0x00, 0x00,
    ];

    /// Reference output for [`TEXT`] at levels 1, 2 and 3, raw, `memLevel` 8.
    const EXPECTED_TEXT: &[u8] = &[
        0x95, 0xcd, 0xc9, 0x15, 0x80, 0x20, 0x10, 0x44, 0xc1, 0x54, 0x3a, 0x02, 0x62, 0xf1, 0x60,
        0x02, 0xa0, 0x6c, 0x2e, 0x8c, 0xec, 0x42, 0xf4, 0x4e, 0x0a, 0x9e, 0xfb, 0xbf, 0xea, 0xd5,
        0x69, 0xc4, 0xea, 0xb7, 0x13, 0x2a, 0x51, 0x0f, 0x30, 0xf4, 0xe2, 0xa8, 0xf7, 0x93, 0x41,
        0x4d, 0x27, 0x14, 0x9e, 0x2f, 0x39, 0x07, 0x76, 0xb2, 0x02, 0xeb, 0x9f, 0x78, 0x91, 0x8c,
        0xde, 0x03, 0x8a, 0xc5, 0xee, 0x8b, 0x83, 0xf1, 0x4d, 0xb3, 0x33, 0x75, 0xc0, 0xe5, 0x63,
        0xa5, 0xc4, 0x47, 0x36, 0x8b, 0x0f,
    ];

    /// Reference output for [`SHORT_MATCH`] at levels 1, 2 and 3, raw, `memLevel` 8.
    const EXPECTED_SHORT_MATCH: &[u8] = &[
        0x4b, 0x4c, 0x4a, 0x4e, 0x49, 0x4d, 0x4b, 0x4c, 0x4a, 0x2e, 0x2c, 0x2a, 0x2e, 0x29, 0x2d,
        0x2b, 0xaf, 0xa8, 0xac, 0x02, 0x72, 0x72, 0xf3, 0xf2, 0x0b, 0x00,
    ];

    /// The four corpora whose reference output is identical at levels 1, 2 and 3: the tuning
    /// parameters that differ between those rows of `configuration_table` -- `max_lazy` 4/5/6,
    /// `nice_length` 8/16/32 and `max_chain` 4/8/32 (`deflate.c` L115-L117) -- happen not to
    /// change any decision on inputs this regular. The corpora in
    /// [`window_crossing_and_incompressible_corpora_are_byte_identical_by_digest`] are the ones
    /// that do discriminate between the three levels.
    #[test]
    fn every_level_that_uses_this_compressor_matches_the_reference_byte_for_byte() {
        for level in 1..=3 {
            assert_eq!(
                compress(&long_run(), level, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
                EXPECTED_LONG_RUN,
                "long run at level {level}"
            );
            assert_eq!(
                compress(
                    &three_byte_period(),
                    level,
                    -15,
                    DEF_MEM_LEVEL,
                    DEFAULT_STRATEGY
                ),
                EXPECTED_THREE_BYTE_PERIOD,
                "three-byte period at level {level}"
            );
            assert_eq!(
                compress(TEXT, level, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
                EXPECTED_TEXT,
                "text at level {level}"
            );
            assert_eq!(
                compress(SHORT_MATCH, level, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
                EXPECTED_SHORT_MATCH,
                "short matches at level {level}"
            );
        }
    }

    /// The container, the window size, the memory level and the two strategies that still
    /// reach this compressor do not change what it decides -- only what is wrapped around it.
    ///
    /// `Z_FILTERED` (1) and `Z_FIXED` (4) both fall through to the level's table entry in
    /// `CompressFunc::select`, so they run `deflate_fast`; `Z_HUFFMAN_ONLY` and `Z_RLE` do not,
    /// and are therefore not tested here.
    #[test]
    fn wrappers_and_parameters_leave_the_deflate_stream_byte_identical() {
        // `deflateInit2(&strm, 1, Z_DEFLATED, 15, 8, 0)`: the two-byte RFC 1950 header and the
        // four-byte Adler-32 trailer around the same eight bytes.
        assert_eq!(
            compress(&long_run(), 1, 15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[0x78, 0x01, 0x4b, 0x4c, 0x1c, 0x05, 0xc4, 0x86, 0x00, 0x00, 0xd8, 0xa8, 0x71, 0xad],
            "zlib container"
        );

        // `windowBits` 31: the ten-byte RFC 1952 header and the CRC-32 plus length trailer.
        assert_eq!(
            compress(&long_run(), 1, 31, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[
                0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x03, 0x4b, 0x4c, 0x1c, 0x05,
                0xc4, 0x86, 0x00, 0x00, 0x09, 0x19, 0x97, 0x89, 0x2c, 0x01, 0x00, 0x00
            ],
            "gzip container"
        );

        // `memLevel` 1: a 128-symbol buffer instead of 16384, which changes only when the
        // block is flushed -- and this corpus is far too short to fill either.
        assert_eq!(
            compress(&long_run(), 1, -15, 1, DEFAULT_STRATEGY),
            EXPECTED_LONG_RUN,
            "memory level 1"
        );

        // `windowBits` 9: a 512-byte window, so `MAX_DIST` is 250 rather than 32506 and the
        // three-byte period is coded with a different distance mix.
        assert_eq!(
            compress(&three_byte_period(), 1, -9, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[
                0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x45, 0x49, 0xc9, 0x89, 0xa3, 0x28, 0x29, 0x39, 0x91,
                0x7a, 0x08, 0x00
            ],
            "window bits 9"
        );

        assert_eq!(
            compress(&three_byte_period(), 1, -15, DEF_MEM_LEVEL, 1),
            EXPECTED_THREE_BYTE_PERIOD,
            "Z_FILTERED"
        );
        assert_eq!(
            compress(&long_run(), 1, -15, DEF_MEM_LEVEL, 4),
            EXPECTED_LONG_RUN,
            "Z_FIXED"
        );
    }

    /// The degenerate inputs: nothing at all, and less than `MIN_MATCH`, where the string is
    /// too short to hash and every byte must come out as a literal.
    #[test]
    fn inputs_shorter_than_min_match_match_the_reference() {
        assert_eq!(
            compress(&[], 1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[0x03, 0x00],
            "empty input"
        );
        assert_eq!(
            compress(b"Q", 1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[0x0b, 0x04, 0x00],
            "one byte"
        );
        assert_eq!(
            compress(b"Qz", 1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY),
            &[0x0b, 0xac, 0x02, 0x00],
            "two bytes"
        );
    }

    /// Two corpora too large to embed, compared against the reference by length and CRC-32.
    ///
    /// The `memLevel` 1 rows are the discriminating case: 13519, 6545 and 4921 bytes at levels
    /// 1, 2 and 3 respectively. A short symbol buffer forces frequent blocks, and the
    /// different `max_lazy` -- which this compressor reads as `max_insert_length`
    /// (`deflate.h` L186) -- then changes which positions enter the hash chains and therefore
    /// which matches every later block finds. Getting the test at L1906 wrong, or the
    /// insertion loop at L1909-L1915 off by one, moves these numbers.
    #[test]
    fn window_crossing_and_incompressible_corpora_are_byte_identical_by_digest() {
        // (level, expected length, expected CRC-32 of the output)
        const INCOMPRESSIBLE: [(i32, usize, u32); 3] = [
            (1, 4101, 0x0446_ae0f),
            (2, 4101, 0x0446_ae0f),
            (3, 4101, 0x0446_ae0f),
        ];
        // (level, windowBits, memLevel, expected length, expected CRC-32 of the output)
        const CROSSING: [(i32, i32, i32, usize, u32); 15] = [
            (1, -15, 8, 4860, 0xc4a3_604b),
            (2, -15, 8, 4860, 0xc4a3_604b),
            (3, -15, 8, 4860, 0xc4a3_604b),
            (1, -15, 9, 4860, 0xc4a3_604b),
            (2, -15, 9, 4860, 0xc4a3_604b),
            (3, -15, 9, 4860, 0xc4a3_604b),
            (1, -15, 2, 5837, 0x0aab_7018),
            (2, -15, 2, 4822, 0xe9c4_a469),
            (3, -15, 2, 4825, 0x3eb2_dfa4),
            (1, -15, 1, 13519, 0x8281_d155),
            (2, -15, 1, 6545, 0xdcba_d3d0),
            (3, -15, 1, 4921, 0x3f00_2a30),
            (1, -9, 8, 70086, 0x3c4a_3556),
            (2, -9, 8, 70086, 0x3c4a_3556),
            (3, -9, 8, 70086, 0x3c4a_3556),
        ];

        let incompressible = incompressible(4096);
        for (level, len, crc) in INCOMPRESSIBLE {
            let out = compress(&incompressible, level, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
            assert_eq!(out.len(), len, "incompressible length at level {level}");
            assert_eq!(crc32(0, &out), crc, "incompressible bytes at level {level}");
        }

        let crossing = window_crossing();
        for (level, window_bits, mem_level, len, crc) in CROSSING {
            let out = compress(&crossing, level, window_bits, mem_level, DEFAULT_STRATEGY);
            assert_eq!(
                out.len(),
                len,
                "crossing length at level {level}, windowBits {window_bits}, memLevel {mem_level}"
            );
            assert_eq!(
                crc32(0, &out),
                crc,
                "crossing bytes at level {level}, windowBits {window_bits}, memLevel {mem_level}"
            );
        }
    }

    /// The first three symbols of a long run, and the state the skip path leaves behind.
    ///
    /// Position 0 cannot match -- the chain is empty -- and position 1 finds only position 0,
    /// which is `NIL` and therefore rejected by the first half of L1886. Position 2 finds
    /// position 1 and matches `MAX_MATCH` bytes, which is far above `max_insert_length`, so
    /// L1919-L1930 runs: the span is skipped whole, nothing in it enters the chains, and the
    /// running key is rebuilt from the two bytes at the new `strstart`.
    #[test]
    fn a_long_run_gives_two_literals_then_one_maximal_match() {
        let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
        let corpus = long_run();
        load_window(&mut state, &corpus);
        let key = key_of(&state, 0);
        let shift = state.hash_shift;
        let mask = state.hash_mask();

        let mut out = vec![0u8; 4096];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore,
            "Z_NO_FLUSH with a short lookahead and no input returns need_more"
        );

        assert_eq!(
            symbols(&state),
            [(0, b'a'), (0, b'a'), (1, 255)],
            "two literals, then 258 bytes at distance 1 with the length normalised by MIN_MATCH"
        );
        assert_eq!(state.window.strstart, 260, "2 + the 258-byte match");
        assert_eq!(state.window.lookahead, 300 - 260);
        assert_eq!(state.match_length, 0, "L1921 zeroes it on the skip path");
        assert_eq!(
            state.hash.head_at(key),
            Pos::new(2),
            "no position inside the skipped span entered the chains"
        );
        assert_eq!(
            state.ins_h,
            update_hash(shift, mask, usize::from(b'a'), b'a'),
            "L1922-L1923 reseeds the key from window[strstart] and window[strstart + 1]"
        );
    }

    /// A match no longer than `max_insert_length` takes the other branch: every position of
    /// the span except the first is inserted, and `strstart` still advances by exactly the
    /// match length.
    ///
    /// The window is built by hand so that the match is exactly `MIN_MATCH` long and the chain
    /// holds exactly one candidate: `xyz` at position 100 and at position 200, with the fourth
    /// bytes deliberately different, and the lookahead chosen so the compressor performs one
    /// iteration and then asks for more input.
    #[test]
    fn a_short_match_inserts_every_position_of_its_span() {
        let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
        let mut corpus = incompressible(600);
        corpus[100..104].copy_from_slice(b"xyz\x00");
        corpus[200..204].copy_from_slice(b"xyz\xff");
        load_window(&mut state, &corpus);

        // One iteration only: 264 - MIN_MATCH is 261, one below `MIN_LOOKAHEAD`.
        state.window.strstart = 200;
        state.window.lookahead = MIN_LOOKAHEAD + 2;
        seed_hash(&mut state, 200);

        // The chain `insert_string` will read at L1880: head is position 100, and that
        // position's `prev` terminates the walk, so `longest_match` sees one candidate.
        let key = key_of(&state, 200);
        state.hash.set_head_at(key, Pos::new(100));
        state.hash.set_prev_at(100, Pos::NIL);

        let mut out = vec![0u8; 4096];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );

        assert_eq!(
            symbols(&state),
            [(100, 0)],
            "a three-byte match at distance 100 normalises to length code 0"
        );
        assert_eq!(
            state.window.strstart, 203,
            "L1908's pre-decrement and L1916's extra increment together advance by match_length"
        );
        assert_eq!(state.match_length, 0, "the loop counter ends at zero");
        assert_eq!(
            state.hash.head_at(key_of(&state, 201)),
            Pos::new(201),
            "the second position of the span entered the chains"
        );
        assert_eq!(
            state.hash.head_at(key_of(&state, 202)),
            Pos::new(202),
            "and so did the third"
        );
    }

    /// The distance test at L1886 is inclusive: a candidate exactly `MAX_DIST` away is taken,
    /// and one byte further is not.
    ///
    /// `MAX_DIST` is `w_size - MIN_LOOKAHEAD`, so 32506 for a 32 KiB window
    /// (`deflate.h` L301). With the candidate at position 1 -- position 0 being unmatchable by
    /// construction -- `strstart` 32507 is exactly at the limit and 32508 is one beyond.
    #[test]
    fn the_candidate_exactly_max_dist_away_is_the_last_one_accepted() {
        for (strstart, expect_match) in [(32507usize, true), (32508, false)] {
            let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
            assert_eq!(state.max_dist(), 32506);

            // `pqr` at position 1 and at `strstart`, with differing fourth bytes so that the
            // match, if it is taken at all, is exactly three bytes long.
            assert!(state.window.write_at(1, b"pqr\x11"));
            assert!(state.window.write_at(strstart, b"pqr\x22"));
            state.window.strstart = strstart;
            // Exactly `MIN_LOOKAHEAD`, so the compressor performs one iteration and then asks
            // for more input whichever way L1886 goes: a match consumes three bytes and a
            // literal one, and both leave the lookahead short.
            state.window.lookahead = MIN_LOOKAHEAD;
            state.window.block_start = 0;
            state.window.insert = 0;
            state.window.initialize_win_init_tail();
            seed_hash(&mut state, strstart);

            let key = key_of(&state, strstart);
            state.hash.set_head_at(key, Pos::new(1));
            state.hash.set_prev_at(1, Pos::NIL);

            let mut out = vec![0u8; 4096];
            let mut cursors = cursors_over(&mut out);
            assert_eq!(
                deflate_fast(&mut state, &mut cursors, Flush::NoFlush),
                BlockState::NeedMore
            );

            let distance = u16::try_from(strstart - 1).unwrap();
            if expect_match {
                assert_eq!(
                    symbols(&state),
                    [(distance, 0)],
                    "a candidate exactly MAX_DIST away must be matched"
                );
            } else {
                assert_eq!(
                    symbols(&state),
                    [(0, b'p')],
                    "a candidate one byte beyond MAX_DIST must be rejected, leaving a literal"
                );
            }
        }
    }

    /// `Z_NO_FLUSH` with less than `MIN_LOOKAHEAD` available and no input to top it up returns
    /// `need_more` before tallying anything (L1869-L1871).
    #[test]
    fn a_short_lookahead_under_z_no_flush_tallies_nothing() {
        let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
        load_window(&mut state, b"abcabcabc");

        let mut out = vec![0u8; 256];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );

        assert_eq!(state.sym_next(), 0, "nothing was tallied");
        assert_eq!(state.window.strstart, 0, "and nothing was consumed");
        assert_eq!(cursors.total_out, 0, "and nothing was emitted");
    }

    /// The tail at L1940 records the positions the next `fill_window` still has to hash:
    /// `strstart` itself while it is below `MIN_MATCH - 1`, and `MIN_MATCH - 1` thereafter.
    #[test]
    fn insert_is_strstart_clamped_to_min_match_minus_one() {
        for (corpus, expected_insert) in [(&b""[..], 0), (b"Q", 1), (b"Qz", 2), (b"Qzy", 2)] {
            let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
            load_window(&mut state, corpus);

            let mut out = vec![0u8; 256];
            let mut cursors = cursors_over(&mut out);
            assert_eq!(
                deflate_fast(&mut state, &mut cursors, Flush::Finish),
                BlockState::FinishDone,
                "Z_FINISH with room in the output buffer completes"
            );
            assert_eq!(
                state.window.insert,
                expected_insert,
                "insert for a {}-byte corpus",
                corpus.len()
            );
        }
    }

    /// A full symbol buffer flushes the block from inside the loop (L1938) rather than at the
    /// tail: with `memLevel` 1 the buffer holds 127 symbols, so an incompressible corpus fills
    /// it several times over and output appears before the compressor returns.
    #[test]
    fn a_full_symbol_buffer_flushes_the_block_mid_loop() {
        let mut state = open(1, -15, 1, DEFAULT_STRATEGY);
        assert_eq!(state.sym_end(), 381, "127 symbols of three bytes each");

        let corpus = incompressible(1024);
        load_window(&mut state, &corpus);

        let mut out = vec![0u8; 8192];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );

        assert!(
            cursors.total_out > 0,
            "the mid-loop FLUSH_BLOCK must have emitted at least one block"
        );
        assert!(
            state.sym_next() < state.sym_end(),
            "and the symbol buffer must have been restarted"
        );
    }

    /// A `Z_FINISH` whose final block does not fit in the caller's output buffer reports
    /// `finish_started`, which is `FLUSH_BLOCK(s, 1)`'s premature exit (L1644).
    #[test]
    fn a_finish_with_no_output_room_reports_finish_started() {
        let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
        load_window(&mut state, &long_run());

        let mut out = [0u8; 0];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishStarted
        );
    }

    /// With `Z_BLOCK` and an exhausted lookahead the compressor emits the partial block and
    /// reports `block_done` (L1945-L1947).
    #[test]
    fn an_exhausted_lookahead_reports_block_done_and_flushes_the_partial_block() {
        let mut state = open(1, -15, DEF_MEM_LEVEL, DEFAULT_STRATEGY);
        load_window(&mut state, b"hello, hello!");

        let mut out = vec![0u8; 256];
        let mut cursors = cursors_over(&mut out);
        assert_eq!(
            deflate_fast(&mut state, &mut cursors, Flush::Block),
            BlockState::BlockDone
        );

        assert_eq!(state.window.strstart, 13, "the whole corpus was consumed");
        assert_eq!(state.window.lookahead, 0);
        assert_eq!(
            state.sym_next(),
            0,
            "the block was flushed, restarting sym_buf"
        );
        assert!(cursors.total_out > 0, "and its bytes reached the caller");
    }
}
