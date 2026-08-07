//! `deflate_slow`: the lazy-matching compressor, used for levels 4 to 9.
//!
//! Mirrors `local block_state deflate_slow(deflate_state *s, int flush)`
//! (`deflate.c` L1956-L2076), the `func` of rows 4 through 9 of
//! `configuration_table` (L119-L124) and therefore the compressor behind
//! **every default-level stream**, since `Z_DEFAULT_COMPRESSION` resolves to 6
//! (L784). `deflate/config_table.rs` names it [`CompressFunc::Slow`], which
//! `deflate()` reaches through the dispatch chain at L1217-L1220.
//!
//! The reference introduces it in three lines (L1951-L1954): "Same as above, but
//! achieves better compression. We use a lazy evaluation for matches: a match is
//! finally adopted only if there is no better match at the next window
//! position." `doc/algorithm.txt` L37-L44 spells the same mechanism out at
//! length: after a match of length N is found, a longer match is sought at the
//! next input byte; if one is found the earlier match is truncated to a single
//! literal, and otherwise the earlier match is kept and the next search happens
//! N steps later.
//!
//! # ★ Two of the eight byte-identity decision points live in this file
//!
//! RFC 1951 constrains the deflate *format*, not the encoder's *choices*. Which
//! of several valid matches to emit, and when to abandon one, are entirely
//! implementation-defined -- and zlib's particular answers are what a drop-in
//! replacement has to reproduce, because the acceptance criterion is that the
//! compressed bytes are *identical* to the reference's, not merely that they
//! decompress to the same input.
//!
//! * **Decision point #4** -- the `TOO_FAR` rejection (L1997-L2008). A
//!   three-byte match further away than 4096 bytes is thrown away, as is any
//!   match of five bytes or fewer under `Z_FILTERED`.
//! * **Decision point #5** -- the lazy-emit block (L2013-L2038). Which of two
//!   overlapping matches is adopted, and which window positions are entered into
//!   the hash chains as a result.
//!
//! Decision point #5 is the more dangerous of the two, because its effect is not
//! local: the set of positions inserted into the hash chains determines which
//! candidates *every later* `longest_match` call can even see. An off-by-one in
//! the insertion loop changes the rest of the stream, not one symbol.
//!
//! Every heuristic below is therefore transcribed rather than reasoned about.
//! Several of them look improvable -- the reference itself calls one "not always
//! a win" -- and improving any of them fails the acceptance criterion. **No
//! zlib-ng-style encoder change may be adopted here, however attractive.**
//!
//! # What is deliberately not implemented
//!
//! Only the default compile-time configuration. `deflate_slow` exists solely in
//! the `#ifndef FASTEST` build -- the guard opens at L1950 and closes at L2077 --
//! so `FASTEST` is out of scope by construction, along with its two-entry
//! configuration table (L107-L110), its `INSERT_STRING` variant (L155-L158) and
//! its alternative `longest_match` (L1537-L1588). `LIT_MEM` is commented out at
//! `deflate.h` L28, so the symbol buffer is the single `sym_buf` of `LIT_BUFS` 4
//! and the `d_buf`/`l_buf` tally macros at `deflate.h` L339-L355 are not implemented.
//! `UNALIGNED_OK` word-at-a-time comparison is not part of this implementation either.
//! `ZLIB_DEBUG` is undefined, which is what makes `check_match` (L2017) a no-op
//! (L1623) and what turns `Assert` into [`debug_assert!`] and `Tracevv`
//! (L2045, L2064) into nothing at all.
//!
//! [`CompressFunc::Slow`]: crate::deflate::config_table::CONFIGURATION_TABLE

// `crate::trees::_tr_tally` keeps the underscore-prefixed C spelling of
// `deflate.h` L311-L317 for oracle traceability, which `clippy::pedantic` flags
// at every call site. The same relaxation, for the same reason, appears in
// `deflate/mod.rs` and `deflate/algorithm.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#![allow(unknown_lints, clippy::used_underscore_items)]

// Everything here comes from the modules this file is allowed to depend on:
//
// * `deflate/algorithm.rs` -- the parent module, which owns the shared dispatch
//   vocabulary: the return type, the flush selector, the stream cursors that
//   stand in for C's `s->strm` back-pointer, and the two spellings of
//   `FLUSH_BLOCK`. `flush_block_only` and `flush_block!` are **both** imported
//   because this function is the one place in the implementation that needs both; see the
//   `match_available` branch.
// * `deflate/hash_chain.rs` -- `INSERT_STRING` (`deflate.c` L160-L163).
// * `deflate/longest_match.rs` -- `longest_match` (L1389).
// * `deflate/state.rs` -- the state itself, plus its re-exports of the sizing
//   constants and of `Strategy` and `IPos`.
// * `deflate/window.rs` -- `fill_window` (L252).
// * `trees/mod.rs` -- the tally, which is the body of both `_tr_tally_lit` and
//   `_tr_tally_dist` (`deflate.h` L357-L375).
use crate::deflate::algorithm::{flush_block, flush_block_only, BlockState, Flush, StreamCursors};
use crate::deflate::hash_chain::insert_string;
use crate::deflate::longest_match::longest_match;
use crate::deflate::state::{Allocator, DeflateState, IPos, Strategy, MIN_LOOKAHEAD, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Distance above which a three-byte match is discarded.
///
/// `deflate.c` L88-L91. This constant is used at exactly one place in the whole
/// reference implementation -- L2000, inside this function -- which is why it
/// lives here rather than beside the other sizing constants in
/// `crate::weak_slice`.
///
/// ★ It is a pure heuristic. Nothing in RFC 1951 mentions it, a three-byte match
/// at distance 4097 is a perfectly legal encoding, and emitting one instead of
/// three literals would produce a *smaller* stream in some cases. It exists
/// because a short match at a long distance usually costs more bits than the
/// literals it replaces, once the distance code is paid for. The exact threshold
/// is arbitrary, and reproducing it exactly is mandatory.
///
/// `usize` because the operands are `strstart` and `match_start`, both `usize`
/// in this implementation.
const TOO_FAR: usize = 4096;

/// The Rust counterpart of the `#if TOO_FAR <= 32767` guard at `deflate.c` L1998.
///
/// In C that guard decides whether the distance half of the rejection at
/// L1999-L2001 is *compiled at all*. It holds for the reference value, so the
/// clause is live code and [`deflate_slow`] implements it unconditionally. Making
/// it a `const` assertion rather than a runtime check reproduces the reference's
/// build-time semantics exactly: were `TOO_FAR` ever raised past 32767, C would
/// silently drop the clause while this implementation would keep applying it, and the two
/// implementations would diverge. Here that divergence cannot compile.
const _: () = assert!(
    TOO_FAR <= 32767,
    "deflate.c L1998 compiles the distance clause out above 32767; this port does not"
);

/// Longest match the lazy rejection at L1997 will consider throwing away.
///
/// The bare `5` in `if (s->match_length <= 5 && ...)` (`deflate.c` L1997). Named
/// only so that the two clauses it guards can be read without counting
/// parentheses; the value carries no derivation and must not be tuned.
const LAZY_REJECT_MAX_LENGTH: usize = 5;

/// C's `ush dist = (ush)(distance);` (`deflate.h` L367).
///
/// The truncation *is* the operation: `_tr_tally_dist` narrows its distance
/// argument to 16 bits before writing it to the symbol buffer, so the reference
/// records the low half and nothing else. For every real match the value is at
/// most `MAX_DIST(s)`, one window less the lookahead and so below 32768, and the
/// narrowing is lossless; it is spelled out here so that the unreachable case
/// keeps C's behaviour rather than this implementation's opinion of it.
// The lint fires on precisely the conversion this function exists to perform.
#[allow(clippy::cast_possible_truncation)]
const fn as_ush(value: usize) -> u16 {
    value as u16
}

/// C's `uch len = (uch)(length);` (`deflate.h` L366).
///
/// The companion of [`as_ush`] for the normalised match length. The largest
/// value the caller can produce is `MAX_MATCH - MIN_MATCH`, which is 255, so
/// again the narrowing is lossless in every reachable case and explicit for the
/// rest.
#[allow(clippy::cast_possible_truncation)]
const fn as_uch(value: usize) -> u8 {
    value as u8
}

/// `s->window[s->strstart - 1]`: the literal this function deferred by one
/// position.
///
/// Read at two places, `deflate.c` L2046 and L2065, both of which are guarded by
/// `s->match_available`. That guard is what makes the index sound: the only
/// place `match_available` is set (L2057) increments `strstart` immediately
/// afterwards, so a set flag implies `strstart >= 1`, and `fill_window`'s slide
/// subtracts a whole `w_size` from a `strstart` that was at least
/// `w_size + MAX_DIST(s)` (L289), which leaves it far above 1.
///
/// C would read `s->window[(unsigned)-1]` if that reasoning were ever wrong --
/// an out-of-bounds read, and undefined behaviour. This implementation cannot, so it
/// records the invariant as a [`debug_assert!`] and yields `0` for the
/// unreachable case: a total function, and one byte of a wrong literal in a
/// situation where the reference has no defined behaviour at all.
fn deferred_literal<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> u8 {
    debug_assert!(
        state.window.strstart >= 1,
        "match_available implies strstart >= 1 (deflate.c L2046 and L2065)"
    );

    state
        .window
        .strstart
        .checked_sub(1)
        .and_then(|index| state.window.byte(index))
        .unwrap_or(0)
}

/// Compresses as much as possible from the input stream with lazy match
/// evaluation, and returns the state of the block it was working on.
///
/// Mirrors `local block_state deflate_slow(deflate_state *s, int flush)`
/// (`deflate.c` L1956-L2076). The compressor for levels 4 to 9, and so for the
/// default level 6.
///
/// # How it differs from `deflate_fast`
///
/// `deflate_fast` (L1857) emits a match the moment it finds one. This function
/// defers the decision by one position: it remembers the match it found at
/// `strstart` in `prev_length`/`prev_match`, advances, searches again, and only
/// then decides which of the two to emit. Three concrete differences follow, and
/// none of them may be unified with the greedy version:
///
/// 1. The save-then-reset at L1985-L1986 carries the previous match forward.
/// 2. The candidate guard at L1988-L1989 has a third condition,
///    `prev_length < max_lazy_match`, which switches the second search off once
///    the match in hand is already long enough.
/// 3. The emit decision at L2013 compares the two lengths, and the literal
///    branch at L2040 flushes with the *non*-returning spelling of
///    `FLUSH_BLOCK`.
///
/// # Arguments
///
/// `state` is C's `deflate_state *s`. `cursors` is everything the reference
/// reaches through `s->strm`, which this implementation cannot hold as a field without
/// aliasing `state`; see the `deflate/algorithm.rs` module documentation.
/// `flush` is the caller's flush selector, already narrowed by `deflate()` to
/// the six values compression accepts (L985).
///
/// # Return value
///
/// A [`BlockState`], exactly as C's `block_state`:
/// [`NeedMore`](BlockState::NeedMore) when the loop ran out of input or output,
/// [`FinishDone`](BlockState::FinishDone) when a `Z_FINISH` completed, and
/// [`BlockDone`](BlockState::BlockDone) otherwise.
/// [`FinishStarted`](BlockState::FinishStarted) is never returned from here
/// directly -- it arrives through `flush_block!(state, cursors, true)`, which is
/// the `(last) ? finish_started : need_more` of L1644.
///
/// # Panics
///
/// Never, in a release build or a debug one. Every arithmetic operation below is
/// checked, saturating or wrapping, and every buffer access goes through an
/// accessor that returns [`Option`]. The two `Assert`s the reference makes are
/// [`debug_assert!`]s, which record invariants that callers establish rather
/// than conditions this function has to defend against.
#[allow(
    // `pedantic`'s `too_many_lines` counts the comments this implementation carries over
    // from `deflate.c`. Splitting the body would break the line-for-line
    // correspondence with L1956-L2076 that the byte-identity requirement is
    // verified against, and the two decision points are precisely the code that
    // must stay readable next to its source. The same relaxation, for the same
    // reason, appears in `deflate/window.rs` and `deflate/longest_match.rs`.
    clippy::too_many_lines
)]
pub(crate) fn deflate_slow<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `IPos hash_head;` (L1957) and `int bflush;` (L1958) are both declared
    // uninitialised in C and assigned before each use. Here each is a `let`
    // inside the loop body, which is where its value is actually produced --
    // nothing carries across an iteration, so nothing has to be initialised at
    // the top.

    // `/* Process the input block. */`
    // `for (;;) {`  (L1960-L1961)
    loop {
        //  Lookahead -- L1962-L1973
        //
        //  "Make sure that we always have enough lookahead, except at the end of
        //   the input file. We need MAX_MATCH bytes for the next match, plus
        //   MIN_MATCH bytes to insert the string following the next match."

        // `if (s->lookahead < MIN_LOOKAHEAD) {`  (L1967)
        if state.window.lookahead < MIN_LOOKAHEAD {
            // `fill_window(s);`  (L1968)
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            // Two flat sibling tests, exactly as C nests them -- the second is
            // *not* inside the first. `deflate_fast` (L1869-L1872) has the same
            // shape; `deflate_rle` (L2094) and `deflate_huff` (L2160) do not,
            // and neither may be copied here.
            //
            // `if (s->lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH) {`
            // `    return need_more;`
            // `}`  (L1969-L1971)
            if state.window.lookahead < MIN_LOOKAHEAD && flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }

            // `if (s->lookahead == 0) break; /* flush the current block */`
            // (L1972)
            if state.window.lookahead == 0 {
                break;
            }
        }

        //  Hash insertion -- L1975-L1981
        //
        //  "Insert the string window[strstart .. strstart + 2] in the
        //   dictionary, and set hash_head to the head of the hash chain"

        // The assignment and its conditional override are one expression here.
        // `insert_string` returns the head of the chain as it was *before* this
        // position was pushed onto it, which is what C's chained
        // `match_head = s->prev[...] = s->head[s->ins_h]` yields; the guard is
        // C's, and without it the third byte of the string would not exist.
        let hash_head = if state.window.lookahead >= MIN_MATCH {
            insert_string(state, state.window.strstart)
        } else {
            IPos::NIL
        };

        //  Save the previous match, then reset -- L1983-L1986
        //
        //  "Find the longest match, discarding those <= prev_length."

        // ★ These three assignments, in this order, are what makes the
        // evaluation lazy. L1985 is a single C comma-expression --
        // `s->prev_length = s->match_length, s->prev_match = s->match_start;` --
        // so both halves happen before L1986 resets `match_length`. The values
        // being saved are the ones the *previous* iteration produced; reordering
        // the three lines would compare a match against itself.
        state.prev_length = state.match_length;

        // `s->prev_match = s->match_start;` (L1985). `match_start` is a window
        // offset and `prev_match` is C's `IPos`, so the conversion is the type
        // change alone: every window offset is below `2 * w_size <= 65536` and
        // therefore fits a `u32` exactly. `IPos::NIL` for the unreachable case
        // is the value that makes the distance computed at L2019 obviously
        // wrong rather than subtly wrong, and it cannot be reached on any target
        // this crate builds for.
        state.prev_match = IPos::from_index(state.window.match_start).unwrap_or(IPos::NIL);

        // `s->match_length = MIN_MATCH-1;`  (L1986)
        state.match_length = MIN_MATCH - 1;

        // ★ THREE conditions, in this order. `deflate_fast` (L1886) has only the
        // first and the third; the middle one is the lazy-matching gate, and the
        // two guards must not be unified:
        //
        // ```text
        // if (hash_head != NIL && s->prev_length < s->max_lazy_match &&
        //     s->strstart - hash_head <= MAX_DIST(s)) {
        // ```
        //
        // * `hash_head != NIL` -- an empty chain has no candidate. The reference
        //   notes at L1990-L1992 that this also prevents "matches with the
        //   string of window index 0 (in particular we have to avoid a match of
        //   the string with itself at the start of the input file)", since index
        //   0 and `NIL` are the same value.
        // * `prev_length < s->max_lazy_match` -- once the match already in hand
        //   is at least `max_lazy` long, no second search is performed at all.
        //   This is the parameter `configuration_table` varies from 4 at level 4
        //   to 258 at level 9 (L119-L124), so it is per-level byte-identity
        //   critical.
        // * `strstart - hash_head <= MAX_DIST(s)` -- the candidate is close
        //   enough to be encodable. C computes the subtraction on `uInt`, so a
        //   stale chain entry above `strstart` wraps to a value near 2^32 and
        //   fails the comparison; `checked_sub` reporting [`None`] for exactly
        //   those operands, and the comparison then failing, is the same
        //   decision without depending on a width `usize` does not share with
        //   C's `unsigned`.
        // Written as one short-circuiting expression so that the three tests are
        // evaluated in C's order and, like C's, stop at the first failure.
        if !hash_head.is_nil()
            && state.prev_length < state.max_lazy_match
            && state
                .window
                .strstart
                .checked_sub(hash_head.to_index().unwrap_or(usize::MAX))
                .is_some_and(|distance| distance <= state.max_dist())
        {
            // `s->match_length = longest_match (s, hash_head);`  (L1994)
            //
            // "longest_match() sets match_start" (L1995) -- as a side effect,
            // and only when the match it found beat `prev_length`. It is never
            // assigned here.
            state.match_length = longest_match(state, hash_head);

            //  ★ DECISION POINT #4 -- the TOO_FAR lazy rejection (L1997-L2008)
            //
            //  A pure heuristic with no basis in RFC 1951, and one of the eight
            //  decisions that determine byte-identical output. Do not adjust the
            //  threshold, the disjunction, or the value assigned.
            //
            //  ★ The `#if TOO_FAR <= 32767` guard HOLDS -- 4096 is well under
            //  32767 -- so the inner clause is live code in every shipped build,
            //  not an option. It is easy to mistake for something conditional
            //  and drop.
            //
            //  The two clauses do different jobs. Under `Z_FILTERED` every match
            //  of five bytes or fewer is discarded, whatever its distance,
            //  because filtered data is expected to be mostly literals with the
            //  occasional long run. Otherwise only a match of exactly
            //  `MIN_MATCH` is discarded, and only when it reaches back further
            //  than [`TOO_FAR`].
            //
            //  The reset value is `MIN_MATCH - 1`, which is 2 -- not 0, and not
            //  `MIN_MATCH`. Two is what `lm_init` seeds `match_length` with
            //  (L698) and what L1986 resets it to, and it is below `MIN_MATCH`,
            //  so the emit test at L2013 treats it as "no match here".

            // `s->strstart - s->match_start`  (L2000)
            //
            // ★ `match_start` may be stale. The reference says so itself at
            // L2004-L2006: "If prev_match is also MIN_MATCH, match_start is
            // garbage but we will ignore the current match anyway" -- because
            // `longest_match` leaves the field untouched when nothing beat
            // `prev_length`. Reading it here is harmless by construction, and a
            // guard, an `Option` or a defensive initialisation to "fix" it would
            // add a branch C does not have. `wrapping_sub` is what reproduces
            // C's `uInt` arithmetic for a stale value above `strstart`: the
            // result is enormous either way, so the comparison against
            // [`TOO_FAR`] decides the same way in both languages.
            let match_distance = state.window.strstart.wrapping_sub(state.window.match_start);

            if state.match_length <= LAZY_REJECT_MAX_LENGTH
                && (state.strategy == Strategy::Filtered
                    || (state.match_length == MIN_MATCH && match_distance > TOO_FAR))
            {
                state.match_length = MIN_MATCH - 1;
            }
        }

        //  ★ DECISION POINT #5 -- lazy emit, hash insertion and the two literal
        //  branches (L2010-L2060)
        //
        //  The second of this file's two byte-identity decision points, and the
        //  more consequential one. It settles three things at once: which symbol
        //  is emitted, how far the cursors advance, and -- uniquely -- which
        //  window positions are entered into the hash chains. That last effect
        //  is not local: the chains are what `longest_match` walks, so a wrong
        //  insertion here changes which candidates every later search can see,
        //  and the divergence from the reference compounds for the rest of the
        //  stream rather than costing one symbol.
        //
        //  Every line below is transcribed. In particular do not "simplify" the
        //  `match_available` branch: it deliberately uses the non-returning
        //  spelling of `FLUSH_BLOCK`, and that is the single most likely subtle
        //  error in this file.

        // "If there was a match at the previous step and the current match is
        //  not better, output the previous match" (L2010-L2012).
        //
        // ★ `<=`, not `<`. On a tie the *previous* match wins, which is what
        // makes the earlier, longer-reaching encoding the one that is emitted.
        //
        // `if (s->prev_length >= MIN_MATCH && s->match_length <= s->prev_length) {`
        // (L2013)
        if state.prev_length >= MIN_MATCH && state.match_length <= state.prev_length {
            // `uInt max_insert = s->strstart + s->lookahead - MIN_MATCH;`
            // `/* Do not insert strings in hash table beyond this. */`
            // (L2014-L2015)
            //
            // ★ Computed here, at the top of the branch, because L2027 is about
            // to reduce `lookahead`. Reading it after that would raise the bound
            // and insert positions the reference does not.
            //
            // Wrapping, because C's is `uInt` arithmetic: if `strstart +
            // lookahead` were ever below `MIN_MATCH` the bound would become
            // enormous and every insertion would happen. That case is
            // unreachable -- `prev_length >= MIN_MATCH` implies a match was
            // found here, which implies `strstart >= 1` and a lookahead that was
            // at least `MIN_MATCH` -- and the wrapping form reproduces it
            // faithfully anyway. The width differs from C's, but only the
            // "enormous" half of the outcome is observable, and both widths
            // deliver it.
            let max_insert = state
                .window
                .strstart
                .wrapping_add(state.window.lookahead)
                .wrapping_sub(MIN_MATCH);

            // `check_match(s, s->strstart - 1, s->prev_match, (int)s->prev_length);`
            // (L2017) is `ZLIB_DEBUG`-only: L1623 defines it away in the shipped
            // build. Nothing to implement.

            // The distance and the length C hands to `_tr_tally_dist`, read
            // before the tally borrows the state.
            //
            // ★ `s->strstart - 1 - s->prev_match` (L2019). The `- 1` is the
            // whole point: the match being emitted started one position *back*,
            // at `strstart - 1`, because this iteration already advanced past it
            // in search of something better. Dropping the `- 1` shifts every
            // distance in the stream by one.
            //
            // Wrapping for the same reason as `max_insert`: C subtracts on
            // `uInt`, and `as_ush` then keeps the low 16 bits, which is what
            // reaches the symbol buffer.
            let prev_match = state.prev_match.to_index().unwrap_or(0);
            let distance = state
                .window
                .strstart
                .wrapping_sub(1)
                .wrapping_sub(prev_match);

            // `s->prev_length - MIN_MATCH` (L2020): the length normalised into
            // the 0..=255 range the length codes are indexed by. Cannot
            // underflow -- this branch tested `prev_length >= MIN_MATCH`.
            let normalised_length = state.prev_length - MIN_MATCH;

            // ```text
            // _tr_tally_dist(s, s->strstart - 1 - s->prev_match,
            //                s->prev_length - MIN_MATCH, bflush);
            // ```
            // (L2019-L2020)
            //
            // `crate::trees::_tr_tally` is the body of that macro: it writes the
            // three symbol bytes with the *raw* distance, decrements the
            // distance only afterwards for `d_code`, bumps the two tree
            // frequencies and returns `s->sym_next == s->sym_end`. The narrowing
            // casts are C's own, applied before the writes.
            let bflush = _tr_tally(state, as_ush(distance), as_uch(normalised_length));

            // "Insert in hash table all strings up to the end of the match.
            //  strstart - 1 and strstart are already inserted. If there is not
            //  enough lookahead, the last two strings are not inserted in the
            //  hash table." (L2022-L2025)

            // `s->lookahead -= s->prev_length - 1;`  (L2027)
            //
            // Before the `-= 2` below, and in this order: the two statements are
            // not independent, because L2028 destroys `prev_length`. Saturating
            // only so that no arithmetic here can underflow even in principle;
            // `longest_match`'s OUT assertion ("the match length is not greater
            // than s->lookahead") is what makes the subtraction exact.
            state.window.lookahead = state
                .window
                .lookahead
                .saturating_sub(state.prev_length.saturating_sub(1));

            // `s->prev_length -= 2;`  (L2028)
            //
            // ★ `prev_length` is now a *loop counter*, and it is consumed
            // destructively. The `- 2` is exactly what makes the count right:
            // the match covers `prev_length` positions, of which `strstart - 1`
            // and `strstart` are already in the chains, so `prev_length - 2`
            // more remain -- and the `do`/`while` form runs its body once before
            // testing, which accounts for one of them. An off-by-one here
            // changes which positions are searchable for the rest of the stream.
            state.prev_length = state.prev_length.saturating_sub(2);

            // `prev_length >= MIN_MATCH` was tested at L2013 and `MIN_MATCH` is
            // 3, so the counter is at least 1 and C's `do`/`while` terminates.
            debug_assert!(
                state.prev_length >= 1,
                "the insertion counter is prev_length - 2 with prev_length >= MIN_MATCH \
                 (deflate.c L2013 and L2028)"
            );

            // (L2029-L2033)
            //
            // ★ Note the asymmetry: `++s->strstart` is evaluated *before* the
            // comparison and therefore happens on every pass, while the
            // insertion happens only when the incremented cursor is still within
            // `max_insert`. `strstart` advances across the whole match either
            // way; only the chain updates are clipped, which is how "the last
            // two strings are not inserted" when the lookahead is short.
            //
            // A `loop` with the test at the bottom, because that is what a
            // `do`/`while` is. `while` would test first and could skip the body.
            loop {
                // `++s->strstart` -- unconditional.
                state.window.strstart = state.window.strstart.saturating_add(1);

                if state.window.strstart <= max_insert {
                    // `INSERT_STRING(s, s->strstart, hash_head);` -- C assigns
                    // the chain head into `hash_head` here, overwriting the
                    // value from L1980. Nothing reads it afterwards, in this
                    // iteration or any other, so the result is dropped rather
                    // than shadowing a live binding.
                    insert_string(state, state.window.strstart);
                }

                // `while (--s->prev_length != 0)`. Saturating rather than
                // wrapping: C would wrap a zero counter to `UINT_MAX` and spin
                // for four billion iterations, which the `debug_assert!` above
                // records as unreachable and which this form ends instead.
                state.prev_length = state.prev_length.saturating_sub(1);
                if state.prev_length == 0 {
                    break;
                }
            }

            // `s->match_available = 0;`  (L2034)
            state.match_available = false;

            // `s->match_length = MIN_MATCH-1;`  (L2035)
            state.match_length = MIN_MATCH - 1;

            // `s->strstart++;`  (L2036) -- the position after the match, which
            // the loop above stopped one short of.
            state.window.strstart = state.window.strstart.saturating_add(1);

            // `if (bflush) FLUSH_BLOCK(s, 0);`  (L2038)
            //
            // The *returning* spelling here, unlike the literal branch below:
            // the cursors are already where they belong, so giving up on a full
            // output buffer costs nothing.
            if bflush {
                flush_block!(state, cursors, false);
            }
        } else if state.match_available {
            // "If there was no match at the previous position, output a single
            //  literal. If there was a match but the current match is longer,
            //  truncate the previous match to a single literal." (L2041-L2044)

            // `Tracevv((stderr,"%c", s->window[s->strstart - 1]));` (L2045) is
            // `ZLIB_DEBUG`-only; nothing to implement.

            // `_tr_tally_lit(s, s->window[s->strstart - 1], bflush);`  (L2046)
            //
            // `strstart - 1`, the deferred position -- not `strstart`, which is
            // what `deflate_fast` tallies (L1934). A distance of zero is how the
            // symbol buffer spells a literal.
            let literal = deferred_literal(state);
            let bflush = _tr_tally(state, 0, literal);

            // ★★ `FLUSH_BLOCK_ONLY`, NOT `FLUSH_BLOCK`. This is the subtlest
            // point in the function.
            //
            // (L2047-L2052)
            //
            // `FLUSH_BLOCK` (L1642-L1645) is `FLUSH_BLOCK_ONLY` *plus* an early
            // `return` when `avail_out == 0`. Here the reference deliberately
            // wants the flush without that return, so that `strstart` and
            // `lookahead` advance past the literal it has just emitted and only
            // then is the output buffer checked. Collapsing the three steps into
            // `flush_block!` would return with the cursors still pointing at the
            // literal, and the next call would emit it a second time -- a
            // different stream, from a state the reference never reaches.
            //
            // Note also that the `avail_out` test is *outside* the `if bflush`:
            // it runs on every pass through this branch, flush or no flush.
            if bflush {
                flush_block_only(state, cursors, false);
            }

            // `s->strstart++;`  (L2050)
            state.window.strstart = state.window.strstart.saturating_add(1);

            // `s->lookahead--;`  (L2051). Saturating; the lookahead block at the
            // top of the loop guarantees `lookahead >= 1` at this point, since it
            // breaks out when the lookahead reaches zero.
            state.window.lookahead = state.window.lookahead.saturating_sub(1);

            // `if (s->strm->avail_out == 0) return need_more;`  (L2052)
            if cursors.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // "There is no previous match to compare with, wait for the next
            //  step to decide." (L2054-L2056)
            //
            // This deferral is what gives lazy matching its name: no symbol is
            // emitted, the byte at `strstart` is remembered only by the flag, and
            // the next iteration decides whether it becomes a literal or is
            // swallowed by a match.

            // `s->match_available = 1;`  (L2057)
            state.match_available = true;

            // `s->strstart++;`  (L2058)
            state.window.strstart = state.window.strstart.saturating_add(1);

            // `s->lookahead--;`  (L2059)
            state.window.lookahead = state.window.lookahead.saturating_sub(1);
        }
    }

    // `Assert (flush != Z_NO_FLUSH, "no flush?");`  (L2062)
    //
    // The loop above is left only by the `break` at L1972, which is reached when
    // the lookahead is exhausted -- and under `Z_NO_FLUSH` the test at L1969
    // returns before that can happen. So this records a property of the loop
    // rather than a condition to defend against, and it is `ZLIB_DEBUG`-only in
    // C, hence a `debug_assert!` and never a release panic.
    debug_assert!(flush != Flush::NoFlush, "no flush? (deflate.c L2062)");

    // (L2063-L2067)
    //
    // The literal the loop deferred and then ran out of input before deciding
    // about. C assigns `bflush` here and never tests it, which is correct rather
    // than an oversight: the `FLUSH_BLOCK` calls below emit the block
    // unconditionally, so a full symbol buffer needs no separate trigger. The
    // result is discarded here for exactly that reason.
    if state.match_available {
        let literal = deferred_literal(state);
        let _full = _tr_tally(state, 0, literal);
        state.match_available = false;
    }

    // `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;`
    // (L2068)
    //
    // ★ The conditional form, matching `deflate_fast` (L1940) -- `deflate_rle`
    // (L2141) and `deflate_huff` (L2177) assign a plain `0` instead. `insert`
    // records how many positions at the end of the window still have to be
    // entered into the hash chains, and `fill_window` picks the work up from
    // there (L316-L333); the value is capped at `MIN_MATCH - 1` because a
    // shorter tail cannot form a hashable string.
    state.window.insert = if state.window.strstart < MIN_MATCH - 1 {
        state.window.strstart
    } else {
        MIN_MATCH - 1
    };

    // (L2069-L2072)
    //
    // `flush_block!` returns `finish_started` from inside the macro when the
    // output buffer filled up, which is the `(last) ? finish_started :
    // need_more` of L1644; `finish_done` is reached only when the final block
    // went out whole.
    if flush == Flush::Finish {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // ```text
    // if (s->sym_next)
    //     FLUSH_BLOCK(s, 0);
    // ```
    // (L2073-L2074)
    //
    // Only when there is something to emit. `sym_next` is the byte offset into
    // the symbol buffer, so a non-zero value means at least one symbol was
    // tallied since the last flush.
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L2075)
    BlockState::BlockDone
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and
// wrong for a harness: a test asserts, and a failing assertion panics. Indexing is
// allowed because every index below is bounded by a length the line above it
// establishes. `used_underscore_items` is allowed because these tests call
// `_tr_init` and `_tr_tally`, whose names are the C spellings of `deflate.h`
// L311-L317. The same relaxation, for the same reasons, appears in
// `deflate/algorithm.rs`, `deflate/window.rs` and `trees/mod.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(
    unknown_lints,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests {
    use super::{deflate_slow, TOO_FAR};
    use crate::config::Z_UNKNOWN;
    use crate::deflate::algorithm::{BlockState, Flush, StreamCursors};
    use crate::deflate::lm_init;
    use crate::deflate::state::{
        Allocator, DeflateConfig, DeflateState, GlobalAllocator, Method, Pos, Strategy,
        DEF_MEM_LEVEL, MIN_LOOKAHEAD, MIN_MATCH, SYMBOL_BYTES,
    };
    use crate::read_buf::{InputCursor, OutputCursor};
    use crate::trees::{_tr_init, _tr_tally};
    use alloc::vec::Vec;

    /// A stream configured as
    /// `deflateInit2(&strm, level, Z_DEFLATED, -15, mem_level, strategy)`, which
    /// is the state a compressor is first entered with.
    ///
    /// ★ All three steps are required, and `lm_init` is the one that is easy to
    /// forget. `deflateInit2_` ends with `deflateReset(strm)` (`deflate.c` L533),
    /// which runs `lm_init` (L682-L701) -- and `lm_init` is what loads the four
    /// match-finding tuning parameters out of `configuration_table` (L689-L692).
    /// Without it `max_lazy_match` is zero, the candidate guard at L1988 can
    /// never pass, `longest_match` is never called, and the compressor silently
    /// emits nothing but literals: a fixture that would make every assertion
    /// about matching below vacuously wrong rather than failing loudly.
    ///
    /// Raw DEFLATE (negative `windowBits`) so that nothing but the block itself
    /// reaches the output. In a debug build every block the allocator hands back
    /// is filled with `0xa5`, the byte `test/infcover.c` L87 uses, so nothing
    /// below can pass by accident on zeroed memory -- which matters here, because
    /// `deflate.c` L442 zeroes only the state struct and never the window, the
    /// chains or the pending buffer.
    fn new_state(
        level: i32,
        mem_level: i32,
        strategy: Strategy,
    ) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level,
            method: Method::Deflated,
            window_bits: -15,
            mem_level,
            strategy,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        lm_init(&mut state);

        assert_ne!(
            state.max_lazy_match, 0,
            "lm_init must load configuration_table[level] (deflate.c L689-L692)"
        );
        assert_eq!(
            state.prev_length,
            MIN_MATCH - 1,
            "lm_init seeds prev_length"
        );
        state
    }

    /// A level-6 stream at the default memory level: what
    /// `Z_DEFAULT_COMPRESSION` resolves to (`deflate.c` L784), and therefore the
    /// configuration virtually every real caller of this compressor uses.
    fn default_state() -> DeflateState<'static, GlobalAllocator> {
        new_state(6, DEF_MEM_LEVEL, Strategy::Default)
    }

    /// Cursors over `input` and `output`, with the scalars a freshly reset raw
    /// stream carries (`deflate.c` L663-L668).
    fn cursors<'i, 'o>(input: &'i [u8], output: &'o mut [u8]) -> StreamCursors<'i, 'o> {
        StreamCursors {
            input: InputCursor::new(input),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// A 64 KiB output buffer, large enough that no test below ever fills it
    /// except the one that deliberately exhausts a one-byte buffer.
    ///
    /// Heap-allocated rather than `[0_u8; 1 << 16]` because 64 KiB is four times
    /// `clippy::large_stack_arrays`' 16 KiB threshold, and that lint is denied
    /// here through `clippy::pedantic` like every other. `alloc` rather than
    /// `std`, because the crate root is `no_std` unless the `std` feature is on.
    fn output_buffer() -> Vec<u8> {
        alloc::vec::from_elem(0_u8, 1 << 16)
    }

    /// The symbols tallied so far, as `(distance, length_or_literal)` pairs.
    ///
    /// A distance of zero is a literal, exactly as in `compress_block`
    /// (`trees.c` L916-L917), and the distance is the **raw** one -- the tally
    /// writes it to the symbol buffer before the `dist--` that feeds `d_code`
    /// (`deflate.h` L368-L371). So a match at distance `d` of length `n` appears
    /// here as `(d, n - MIN_MATCH)`, which is what lets the assertions below name
    /// both numbers exactly.
    fn tallied<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> Vec<(u16, u8)> {
        (0..state.sym_next())
            .step_by(SYMBOL_BYTES)
            .filter_map(|offset| state.pending.symbol_at(offset))
            .collect()
    }

    /// A byte sequence in which no three-byte window occurs twice, so the only
    /// match a compressor can find in it is one a test planted deliberately.
    ///
    /// A big-endian 16-bit counter: `00 00, 00 01, 00 02, ...`. Distinctness is
    /// asserted rather than argued -- see [`assert_no_repeated_trigram`] -- so a
    /// future change to this generator cannot quietly invalidate the tests that
    /// depend on it.
    fn unique_trigrams(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 1);
        let mut counter: u16 = 0;
        while out.len() < len {
            out.extend_from_slice(&counter.to_be_bytes());
            counter = counter.wrapping_add(1);
        }
        out.truncate(len);
        out
    }

    /// Fails unless every three-byte window of `data` is unique.
    ///
    /// `MIN_MATCH` is 3, so a repeated three-byte window is exactly what
    /// `longest_match` can find; proving there are none is what makes "the
    /// symbol list contains no match at distance `d`" a statement about the
    /// planted pattern rather than about the filler.
    fn assert_no_repeated_trigram(data: &[u8]) {
        let mut keys: Vec<u32> = data
            .windows(3)
            .map(|w| u32::from(w[0]) << 16 | u32::from(w[1]) << 8 | u32::from(w[2]))
            .collect();
        keys.sort_unstable();
        for pair in keys.windows(2) {
            assert_ne!(pair[0], pair[1], "the filler repeats a three-byte window");
        }
    }

    /// Where a planted pattern lives, and what the compressor should make of it.
    ///
    /// Built by [`plant`], whose documentation explains every field.
    struct Planted {
        /// The whole input, ready to hand to the compressor.
        data: Vec<u8>,
        /// The distance of the match the compressor is expected to find.
        distance: u16,
        /// That match's length, normalised by `MIN_MATCH` -- the second half of
        /// the `(distance, length_or_literal)` pair [`tallied`] reports.
        normalised_length: u8,
    }

    /// The first copy of the pattern. Deliberately **not** zero.
    ///
    /// ★ `longest_match` can never return window index 0: its walk stops at
    /// `limit`, which is `NIL` -- also 0 -- whenever `strstart <= MAX_DIST(s)`,
    /// and the reference explains why at `deflate.c` L1990-L1992, "we prevent
    /// matches with the string of window index 0 (in particular we have to avoid
    /// a match of the string with itself at the start of the input file)". A
    /// pattern planted at offset 0 therefore cannot be matched at all, and every
    /// assertion about matching below would hold vacuously. 100 is far enough in
    /// that the running hash is also fully primed: `INSERT_STRING` folds one byte
    /// per call (L161), so the key is the true three-byte hash only from window
    /// index 2 onwards.
    const PATTERN_SOURCE: usize = 100;

    /// `unique_trigrams(total)` with `len` bytes copied from [`PATTERN_SOURCE`] to
    /// `at`, and the byte just past the copy forced to differ.
    ///
    /// The result is a single plantable match: at window position `at` the
    /// longest match available is exactly `len` bytes back at distance
    /// `at - PATTERN_SOURCE`, and nowhere else in the input can any match be
    /// found, because [`unique_trigrams`] repeats no three-byte window.
    ///
    /// ★ The reported distance is the one **lazy** matching emits, and it takes
    /// one step of the algorithm to see why it is the same number. At position
    /// `at` the match is found but not adopted -- `prev_length` is still 2 there,
    /// so the test at L2013 fails and a literal goes out. At position `at + 1`
    /// the second search finds only `len - 1` bytes, which `longest_match`
    /// discards as no better, so it returns `prev_length` unchanged; the tie then
    /// adopts the previous match, and the emitted distance is
    /// `strstart - 1 - prev_match` = `(at + 1) - 1 - PATTERN_SOURCE`. That the
    /// `- 1` and the `+ 1` cancel is exactly what makes this fixture a check on
    /// the `- 1` of L2019: drop it and the distance comes out one too large.
    fn plant(total: usize, at: usize, len: usize) -> Planted {
        let mut data = unique_trigrams(total);
        assert_no_repeated_trigram(&data);

        for offset in 0..len {
            data[at + offset] = data[PATTERN_SOURCE + offset];
        }
        // Force the match to end exactly at `len`: without this the byte after
        // the copy might coincide and the match would run one longer.
        data[at + len] = data[PATTERN_SOURCE + len] ^ 0xff;

        Planted {
            data,
            distance: u16::try_from(at - PATTERN_SOURCE).unwrap(),
            normalised_length: u8::try_from(len - usize::from(u8::try_from(MIN_MATCH).unwrap()))
                .unwrap(),
        }
    }

    /// Just the match symbols of [`tallied`], for a failure message that fits on
    /// a screen: a few thousand literals say nothing about which match was
    /// chosen.
    fn matches_only<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> Vec<(u16, u8)> {
        tallied(state)
            .into_iter()
            .filter(|&(dist, _)| dist != 0)
            .collect()
    }

    /// Runs the compressor over `input` with `Z_NO_FLUSH` and a generous output
    /// buffer, and returns the state so the tally can be inspected.
    ///
    /// `Z_NO_FLUSH` is what keeps the symbol buffer readable: the loop returns
    /// [`BlockState::NeedMore`] at L1969-L1971 once the input is exhausted, so it
    /// never reaches the `FLUSH_BLOCK` calls in the tail, and `init_block`
    /// (`trees.c` L450) never resets `sym_next`.
    fn tally_with_no_flush(
        mut state: DeflateState<'static, GlobalAllocator>,
        input: &[u8],
    ) -> DeflateState<'static, GlobalAllocator> {
        let mut output = output_buffer();
        let mut cursors = cursors(input, &mut output);
        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore,
            "Z_NO_FLUSH must ask for more input once the buffer is drained"
        );
        state
    }

    #[test]
    fn too_far_is_the_reference_value() {
        // `#define TOO_FAR 4096` (`deflate.c` L89), and the `#if TOO_FAR <= 32767`
        // guard at L1998 that decides whether the second rejection clause is
        // compiled at all.
        assert_eq!(TOO_FAR, 4096);
        // The `#if TOO_FAR <= 32767` half of the guard is asserted at compile
        // time beside the constant itself, so the distance clause cannot be
        // compiled in while the reference would compile it out.
    }

    #[test]
    fn a_three_byte_match_beyond_too_far_is_rejected() {
        // A three-byte match at distance 5000, which is above TOO_FAR. C throws it
        // away and emits literals instead (L1999-L2007), so no symbol may carry
        // that distance -- and since the filler repeats no three-byte window, no
        // symbol may carry any distance at all.
        let planted = plant(5500, PATTERN_SOURCE + 5000, MIN_MATCH);
        assert!(
            usize::from(planted.distance) > TOO_FAR,
            "the fixture must actually exceed TOO_FAR"
        );

        let state = tally_with_no_flush(default_state(), &planted.data);

        assert_eq!(
            matches_only(&state),
            Vec::new(),
            "a three-byte match at distance {} survived the TOO_FAR rejection",
            planted.distance
        );
    }

    #[test]
    fn the_too_far_rejection_resets_match_length_to_min_match_minus_one() {
        // ★ The value assigned is `MIN_MATCH - 1`, which is 2 -- not 0, and not
        // `MIN_MATCH` (L2007). Two is what `lm_init` seeds `match_length` with
        // (`deflate.c` L698) and what L1986 resets it to on every pass, so the
        // rejection puts the field back exactly where "no match here" lives.
        //
        // Zero would satisfy the emit test at L2013 identically, which is why
        // this needs its own fixture rather than riding on the symbol stream:
        // what it changes is the `best_len` that the *next* call to
        // `longest_match` is seeded with (`deflate.c` L1394), and that decides
        // which candidates the chain walk rejects early. The field itself is the
        // direct read-out, so the fixture is sized to make the rejection the last
        // thing that happens: `Z_NO_FLUSH` stops once the lookahead falls below
        // `MIN_LOOKAHEAD`, so a total of exactly `at + MIN_LOOKAHEAD` lets the
        // iteration at `at` run and stops the one after it.
        let at = PATTERN_SOURCE + 5000;
        let total = at + MIN_LOOKAHEAD;
        let planted = plant(total, at, MIN_MATCH);
        assert!(
            usize::from(planted.distance) > TOO_FAR,
            "the fixture must be rejected"
        );

        let state = tally_with_no_flush(default_state(), &planted.data);

        assert_eq!(
            state.window.strstart,
            at + 1,
            "the rejection must be the last thing the loop did"
        );
        assert_eq!(
            state.match_length,
            MIN_MATCH - 1,
            "the TOO_FAR rejection assigns MIN_MATCH - 1 (deflate.c L2007)"
        );
        assert_eq!(
            matches_only(&state),
            Vec::new(),
            "and nothing was emitted as a match"
        );
    }

    #[test]
    fn a_three_byte_match_within_too_far_is_kept() {
        // The same pattern at distance 1000, which is within TOO_FAR, must be
        // emitted -- and emitted at exactly that distance, which is what checks
        // the `- 1` of `s->strstart - 1 - s->prev_match` (L2019).
        //
        // ★ It also exercises the `<=` of L2013, and not incidentally.
        // `longest_match` returns `prev_length` unchanged whenever nothing beats
        // it, so the adoption test sees `match_length == prev_length` -- a tie --
        // on essentially every match this compressor emits. With `<` in place of
        // `<=` the match below would be dropped in favour of a literal and this
        // assertion would fail.
        let planted = plant(1500, PATTERN_SOURCE + 1000, MIN_MATCH);
        assert!(
            usize::from(planted.distance) <= TOO_FAR,
            "the fixture must be within TOO_FAR"
        );

        let state = tally_with_no_flush(default_state(), &planted.data);

        assert_eq!(
            matches_only(&state),
            [(planted.distance, planted.normalised_length)],
            "the three-byte match at distance {} was not adopted",
            planted.distance
        );
    }

    #[test]
    fn filtered_rejects_a_short_match_at_any_distance() {
        // `s->strategy == Z_FILTERED` (L1997) discards every match of five bytes
        // or fewer, whatever its distance. This is byte for byte the fixture the
        // test above keeps under the default strategy, so the strategy is the
        // only thing that can account for the difference.
        let planted = plant(1500, PATTERN_SOURCE + 1000, MIN_MATCH);
        let state = tally_with_no_flush(
            new_state(6, DEF_MEM_LEVEL, Strategy::Filtered),
            &planted.data,
        );

        assert_eq!(
            matches_only(&state),
            Vec::new(),
            "Z_FILTERED must reject a match of MIN_MATCH bytes at distance {}",
            planted.distance
        );
    }

    #[test]
    fn a_long_match_beyond_too_far_is_kept() {
        // ★ The `<= 5` gate is what makes the rejection short-match-only. A
        // ten-byte match at distance 5000 fails that gate, so neither clause
        // applies and the match is adopted even though it reaches back further
        // than TOO_FAR. Contrast with the three-byte fixture at the same
        // distance, which is rejected.
        let planted = plant(5500, PATTERN_SOURCE + 5000, 10);
        assert!(usize::from(planted.distance) > TOO_FAR);
        assert_eq!(planted.normalised_length, 7, "10 - MIN_MATCH");

        let state = tally_with_no_flush(default_state(), &planted.data);

        assert_eq!(
            matches_only(&state),
            [(planted.distance, planted.normalised_length)],
            "the ten-byte match at distance {} was not adopted",
            planted.distance
        );
    }

    #[test]
    fn adopting_a_match_drives_prev_length_to_zero() {
        // ★ `prev_length` is the insertion loop's counter and it is consumed
        // destructively: `-= 2` (L2028) and then `while (--s->prev_length != 0)`
        // (L2033). Reaching *exactly* zero is observable, and it is relied upon --
        // `deflate/longest_match.rs` documents that the shipped build reaches it
        // with `prev_length == 0` after a `deflateParams()` switch from a lazy
        // level to a greedy one, precisely because of these two lines, and that
        // reproducing what C then does is required for byte-identical output.
        //
        // The window must be inspected in the iteration that adopts the match,
        // because the next iteration overwrites `prev_length` from
        // `match_length` (L1985). The fixture arranges for there to be no next
        // iteration: `total` is sized so the match is the last thing processed.
        // `Z_NO_FLUSH` stops as soon as the lookahead falls below `MIN_LOOKAHEAD`,
        // which is to say once `strstart` passes `total - MIN_LOOKAHEAD`, so
        //
        // * the adopting iteration, at `strstart == at + 1`, needs
        //   `at + 1 + MIN_LOOKAHEAD <= total`; and
        // * the iteration after it, at `strstart == at + len`, must not run,
        //   which needs `total < at + len + MIN_LOOKAHEAD`.
        //
        // With `len` at 10 that leaves a nine-byte window for `total - at`, and
        // `MIN_LOOKAHEAD + 3` sits inside it. Both bounds are asserted rather
        // than trusted.
        let len = 10;
        let at = PATTERN_SOURCE + 5000;
        let total = at + MIN_LOOKAHEAD + 3;
        assert!(at + 1 + MIN_LOOKAHEAD <= total, "the match must be reached");
        assert!(total < at + len + MIN_LOOKAHEAD, "and must be reached last");

        let planted = plant(total, at, len);
        let state = tally_with_no_flush(default_state(), &planted.data);

        assert_eq!(
            matches_only(&state),
            [(planted.distance, planted.normalised_length)],
            "the fixture must adopt exactly the planted match for this to mean anything"
        );
        assert_eq!(
            state.window.strstart,
            at + len,
            "the insertion loop must advance strstart across the whole match"
        );
        assert_eq!(
            state.prev_length, 0,
            "the insertion loop must leave prev_length at exactly zero"
        );
    }

    #[test]
    fn a_run_of_one_byte_emits_a_distance_of_one() {
        // The degenerate case the lazy path handles by way of the
        // `prev_length < s->max_lazy_match` gate (L1988): once the match in hand
        // is longer than `max_lazy` -- 16 at level 6 -- no second search happens
        // at all and the long match is adopted immediately.
        let mut data = Vec::new();
        data.extend(core::iter::repeat(b'a').take(600));
        data.extend(unique_trigrams(400));

        let state = tally_with_no_flush(default_state(), &data);

        assert!(
            tallied(&state).iter().any(|&(dist, _)| dist == 1),
            "a run of identical bytes must be coded as matches at distance 1; got {:?}",
            tallied(&state)
        );
    }

    #[test]
    fn max_lazy_match_stops_the_second_search_once_the_match_is_long_enough() {
        // ★ The third condition of the candidate guard, `s->prev_length <
        // s->max_lazy_match` (L1988) -- the one `deflate_fast`'s two-condition
        // guard (L1886) does not have, and the reason the two must not be
        // unified. `max_lazy` is 16 at level 6 and varies from 4 to 258 across
        // the six levels that reach this compressor (L119-L124), so this is a
        // per-level byte-identity decision.
        //
        // Making it observable needs a position where a *better* match exists but
        // must not be looked for: the guard is invisible whenever the second
        // search would have found nothing better, because `longest_match` then
        // returns `prev_length` and the same match is adopted either way. So the
        // fixture plants two overlapping patterns:
        //
        // * a 20-byte match at `at`, back to `SHORT_SOURCE`; 20 is above
        //   `max_lazy`, which is what arms the guard on the next position;
        // * a 25-byte match at `at + 1`, back to `LONG_SOURCE`.
        //
        // With the guard, the second search never happens: the 20-byte match is
        // adopted at `at + 1` and emitted at distance `at - SHORT_SOURCE`.
        // Without it, the 25-byte match is found, `match_length <= prev_length`
        // fails, a literal goes out instead, and the 25-byte match is emitted one
        // position later at distance `(at + 1) - LONG_SOURCE`. Both distances are
        // named below, one as required and one as forbidden.
        const SHORT_SOURCE: usize = 100;
        const LONG_SOURCE: usize = 2000;
        const AT: usize = 4000;
        const SHORT_LEN: usize = 20;
        const LONG_LEN: usize = 25;

        let mut data = unique_trigrams(5000);
        assert_no_repeated_trigram(&data);

        // The 20-byte pattern, and the byte that ends it.
        for offset in 0..SHORT_LEN {
            data[AT + offset] = data[SHORT_SOURCE + offset];
        }
        data[AT + SHORT_LEN] = data[SHORT_SOURCE + SHORT_LEN] ^ 0xff;

        // The 25-byte pattern is whatever now begins one byte later, copied back
        // to `LONG_SOURCE`; it therefore overlaps the 20-byte one by 19 bytes,
        // which is exactly the arrangement that makes the second search
        // worthwhile.
        for offset in 0..LONG_LEN {
            data[LONG_SOURCE + offset] = data[AT + 1 + offset];
        }
        data[LONG_SOURCE + LONG_LEN] = data[AT + 1 + LONG_LEN] ^ 0xff;

        let state = tally_with_no_flush(default_state(), &data);
        let emitted = matches_only(&state);

        let guarded = (
            u16::try_from(AT - SHORT_SOURCE).unwrap(),
            u8::try_from(SHORT_LEN - MIN_MATCH).unwrap(),
        );
        let unguarded = (
            u16::try_from(AT + 1 - LONG_SOURCE).unwrap(),
            u8::try_from(LONG_LEN - MIN_MATCH).unwrap(),
        );

        assert!(
            emitted.contains(&guarded),
            "max_lazy_match must suppress the second search, emitting {guarded:?}; got {emitted:?}"
        );
        assert!(
            !emitted.contains(&unguarded),
            "the longer match {unguarded:?} must never be looked for; got {emitted:?}"
        );
    }

    #[test]
    fn a_full_output_buffer_returns_need_more_after_advancing() {
        // ★★ The discriminating test for the subtlest line in this file: the
        // literal branch uses `FLUSH_BLOCK_ONLY` (L2048) and not `FLUSH_BLOCK`,
        // so `strstart` and `lookahead` advance (L2050-L2051) *before*
        // `avail_out` is consulted (L2052).
        //
        // The fixture makes all three things happen on one pass:
        //
        // * memory level 1 gives `lit_bufsize` 128 and therefore
        //   `sym_end = (128 - 1) * 3 = 381`, room for 127 symbols. Pre-tallying
        //   126 of them means the literal this branch emits is the one that fills
        //   the buffer, so `bflush` is true and the flush actually runs.
        // * the output buffer is empty, so `avail_out` is 0 both before and after
        //   the flush.
        //
        // A `flush_block!` here would return while `strstart` was still 1, and
        // the next call would emit the literal at `strstart - 1` a second time.
        // The correct code returns with `strstart == 2`.
        let mut state = new_state(6, 1, Strategy::Default);
        assert_eq!(state.sym_end(), 381, "memory level 1 gives 127 symbols");

        for _ in 0..126 {
            assert!(
                !_tr_tally(&mut state, 0, b'x'),
                "the buffer must not fill before the run"
            );
        }
        assert_eq!(state.sym_next(), 378);

        let data = unique_trigrams(400);
        let mut output = [];
        let mut cursors = cursors(&data, &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );
        assert_eq!(cursors.avail_out(), 0, "the fixture must leave no room");
        assert_eq!(
            state.sym_next(),
            0,
            "the symbol buffer filled, so the flush must have run and reset it"
        );
        assert_eq!(
            state.window.strstart, 2,
            "strstart must advance past the literal before avail_out is tested"
        );
        assert_eq!(
            state.window.lookahead,
            data.len() - 2,
            "lookahead must advance with strstart"
        );
        // ★ And the advance must happen *after* the flush, not merely before the
        // `avail_out` test. `FLUSH_BLOCK_ONLY` reads `strstart` twice -- once for
        // the block length `(long)s->strstart - s->block_start` (L1634) and once
        // for `s->block_start = s->strstart` (L1637) -- so hoisting `s->strstart++`
        // above the flush would close the block one byte longer and open the next
        // one a byte further on. `block_start` is where that is visible.
        assert_eq!(
            state.window.block_start, 1,
            "the block must be closed at strstart == 1, before the literal's advance"
        );
        assert!(
            state.match_available,
            "the literal branch does not clear match_available (deflate.c L2040-L2052)"
        );
    }

    #[test]
    fn no_input_under_no_flush_asks_for_more() {
        // `if (s->lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH) return
        // need_more;` (L1969-L1971), reached on the very first pass when there is
        // nothing to compress at all.
        let mut state = default_state();
        let mut output = [0_u8; 64];
        let mut cursors = cursors(&[], &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );
        assert_eq!(state.window.strstart, 0);
        assert_eq!(state.sym_next(), 0, "nothing may be tallied");
    }

    #[test]
    fn the_literals_emitted_are_the_deferred_ones() {
        // ★ Both literal tallies read `s->window[s->strstart - 1]` (L2046 and
        // L2065), one position *behind* the cursor -- unlike `deflate_fast`, which
        // tallies `s->window[s->strstart]` (L1934). The offset is what "lazy"
        // means at the byte level: the compressor has already advanced past the
        // byte it is deciding about.
        //
        // With input that contains no match at all, every pass takes a literal
        // branch, so the whole tally is readable as a sequence and can be
        // compared against the input directly. The first pass emits nothing --
        // it takes the `else` at L2053, which only sets `match_available` -- so
        // the literals are `data[0 .. strstart - 1]`. Reading at `strstart`
        // instead would shift the entire sequence by one byte.
        let data = unique_trigrams(600);
        assert_no_repeated_trigram(&data);

        let state = tally_with_no_flush(default_state(), &data);
        let strstart = state.window.strstart;
        assert!(strstart > 1, "the loop must have made progress");

        let symbols = tallied(&state);
        assert!(
            symbols.iter().all(|&(dist, _)| dist == 0),
            "the fixture must produce literals only; got {:?}",
            matches_only(&state)
        );

        let literals: Vec<u8> = symbols.iter().map(|&(_, lit)| lit).collect();
        assert_eq!(
            literals,
            data[..strstart - 1],
            "the literals must be the bytes one position behind the cursor"
        );
    }

    #[test]
    fn the_insert_guard_admits_exactly_a_three_byte_lookahead() {
        // `if (s->lookahead >= MIN_MATCH) { INSERT_STRING(...) }` (L1979-L1981).
        // The bound is inclusive, and it has to be: `INSERT_STRING` hashes
        // `window[str + MIN_MATCH - 1]` (L161), so three bytes of lookahead is
        // exactly enough and two is not.
        //
        // With no match anywhere the loop visits every position in turn, so the
        // lookahead at position `p` is `total - p`; the last position that may be
        // inserted is therefore `total - MIN_MATCH`, and the two after it may not.
        // As in the `max_insert` test, a sentinel in the never-initialised `prev`
        // array reads out which positions `INSERT_STRING` reached.
        let total = 600;
        let data = unique_trigrams(total);
        assert_no_repeated_trigram(&data);

        let mut state = default_state();
        let sentinel = Pos::new(u16::MAX);
        for index in [total - MIN_MATCH, total - 2, total - 1] {
            state.hash.set_prev_at(index, sentinel);
        }

        let mut output = output_buffer();
        let mut cursors = cursors(&data, &mut output);
        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(state.window.strstart, total);

        assert_ne!(
            state.hash.prev_at(total - MIN_MATCH),
            sentinel,
            "a lookahead of exactly MIN_MATCH must still be inserted"
        );
        assert_eq!(
            state.hash.prev_at(total - 2),
            sentinel,
            "a lookahead of MIN_MATCH - 1 must not be inserted"
        );
        assert_eq!(
            state.hash.prev_at(total - 1),
            sentinel,
            "nor may a lookahead of one"
        );
    }

    #[test]
    fn an_empty_stream_under_z_block_emits_nothing() {
        // The tail's `if (s->sym_next) FLUSH_BLOCK(s, 0);` (L2073-L2074), taking
        // the branch that does *not* flush. `Z_BLOCK` leaves the loop through the
        // `break` at L1972 with nothing tallied, and the guard is what keeps an
        // empty block out of the stream; without it the caller would receive a
        // block header describing no data.
        //
        // This is also the only path that reaches the tail without `Z_FINISH`, so
        // it is where the `debug_assert!` standing in for
        // `Assert(flush != Z_NO_FLUSH)` (L2062) is exercised in the affirmative.
        let mut state = default_state();
        let mut output = [0_u8; 64];
        let mut cursors = cursors(&[], &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Block),
            BlockState::BlockDone
        );
        assert_eq!(state.sym_next(), 0, "nothing was tallied");
        assert_eq!(
            cursors.total_out, 0,
            "and so nothing may be emitted (deflate.c L2073)"
        );
        assert_eq!(state.window.insert, 0, "insert is min(strstart, 2) == 0");
    }

    #[test]
    fn a_deferred_literal_is_tallied_and_flushed_by_z_block() {
        // The tail's trailing literal (L2063-L2067). A one-byte input is too
        // short to hash, so the loop takes the `else` at L2053, sets
        // `match_available` and runs out of lookahead with the byte still
        // undecided; the tail is the only thing that emits it.
        //
        // The pairing with `an_empty_stream_under_z_block_emits_nothing` is what
        // makes this precise. Both runs use `Z_BLOCK`, so both reach the
        // `if (s->sym_next)` test at L2073 rather than the `Z_FINISH` return
        // above it. The empty run must produce no output; this one must produce
        // some -- and it can only do so because the trailing literal advanced
        // `sym_next` first. Drop the tally and this run falls silent too.
        let mut state = default_state();
        let mut output = [0_u8; 64];
        let mut cursors = cursors(b"A", &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Block),
            BlockState::BlockDone
        );
        assert!(
            !state.match_available,
            "the tail must clear the flag it consumed (deflate.c L2066)"
        );
        assert_eq!(
            state.sym_next(),
            0,
            "the block went out, so the tally was reset by init_block"
        );
        assert!(
            cursors.total_out > 0,
            "the deferred literal must have been tallied and then flushed"
        );
        assert_eq!(state.window.insert, 1, "insert is min(strstart, 2) == 1");
    }

    #[test]
    fn a_single_byte_finish_sets_insert_to_strstart() {
        // `s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;`
        // (L2068), taking the first arm. One byte is too short to hash, so the
        // loop defers it as a literal (L2057), the lookahead reaches zero and the
        // tail emits it (L2063-L2067) with `strstart` at 1 -- below `MIN_MATCH -
        // 1`, which is 2.
        let mut state = default_state();
        let mut output = [0_u8; 64];
        let mut cursors = cursors(b"A", &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(state.window.strstart, 1);
        assert_eq!(state.window.insert, 1, "the conditional's first arm");
        assert!(
            !state.match_available,
            "the tail clears the deferred literal"
        );
        assert!(
            cursors.total_out > 0,
            "the final block must reach the output buffer"
        );
    }

    #[test]
    fn a_longer_finish_caps_insert_at_min_match_minus_one() {
        // The same line taking its second arm, which is every real stream.
        let mut state = default_state();
        let mut output = output_buffer();
        let data = unique_trigrams(1000);
        let mut cursors = cursors(&data, &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(state.window.strstart, data.len(), "all input consumed");
        assert_eq!(state.window.insert, 2, "capped at MIN_MATCH - 1");
        assert_eq!(state.sym_next(), 0, "the final flush reset the tally");
    }

    #[test]
    fn max_insert_leaves_the_last_two_positions_out_of_the_chains() {
        // ★ The conditional half of the insertion loop's asymmetry:
        // `if (++s->strstart <= max_insert)` (L2030), with `max_insert` fixed at
        // `s->strstart + s->lookahead - MIN_MATCH` *before* L2027 reduces the
        // lookahead (L2014). The reference states the consequence at
        // L2022-L2025: "Insert in hash table all strings up to the end of the
        // match. strstart - 1 and strstart are already inserted. If there is not
        // enough lookahead, the last two strings are not inserted in the hash
        // table."
        //
        // Not inserting them is not an optimisation, it is correctness:
        // `INSERT_STRING` hashes `window[str + MIN_MATCH - 1]` (L161), so
        // inserting position `p` requires `p + 2` to be real data. Insert past
        // `max_insert` and the chain entry is keyed on bytes that do not exist
        // yet, which puts that position on the wrong chain for every later
        // search.
        //
        // The fixture plants a ten-byte match ending exactly at the end of the
        // input and finishes the stream, which is the "not enough lookahead"
        // case: at the adopting position the lookahead is nine, so `max_insert`
        // lands on `total - 3` and the loop walks two positions past it.
        //
        // `prev` is deliberately never initialised -- "prev[] will be initialized
        // on the fly" (`deflate.h` L168), and `lm_init` clears only `head` --
        // so a sentinel written into the three positions of interest is a direct
        // read-out of which of them `INSERT_STRING` reached.
        const LEN: usize = 10;
        let total = 5000;
        let at = total - LEN;

        // Built here rather than by `plant`, which forces the byte after the
        // pattern to differ so the match ends where it says. This fixture wants
        // the match to end at the end of the *input*, which is what makes the
        // lookahead too short, so there is no byte after it to force -- the
        // lookahead bounds the match instead.
        let mut data = unique_trigrams(total);
        assert_no_repeated_trigram(&data);
        for offset in 0..LEN {
            data[at + offset] = data[PATTERN_SOURCE + offset];
        }

        let mut state = default_state();

        // A value `head[ins_h]` cannot hold here: the chain entries are window
        // offsets, and this stream never reaches one that large.
        let sentinel = Pos::new(u16::MAX);
        for index in [total - 3, total - 2, total - 1] {
            state.hash.set_prev_at(index, sentinel);
        }

        let mut output = output_buffer();
        let mut cursors = cursors(&data, &mut output);
        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(
            state.window.strstart, total,
            "the whole input must be consumed"
        );

        assert_ne!(
            state.hash.prev_at(total - 3),
            sentinel,
            "the position at max_insert must be inserted"
        );
        assert_eq!(
            state.hash.prev_at(total - 2),
            sentinel,
            "the second-to-last string must not be inserted (deflate.c L2024-L2025)"
        );
        assert_eq!(
            state.hash.prev_at(total - 1),
            sentinel,
            "the last string must not be inserted (deflate.c L2024-L2025)"
        );
    }

    #[test]
    fn a_finish_over_data_ending_in_a_long_match_completes() {
        // Exercises the `max_insert` clipping of L2030 for real: the match sits
        // at the very end of the input, so `strstart + lookahead - MIN_MATCH`
        // falls below the positions the insertion loop walks and "the last two
        // strings are not inserted in the hash table" (L2024-L2025).
        //
        // What is asserted here is completion and cursor arithmetic, not the
        // resulting bytes -- the byte-exact proof of this path is the
        // `crates/zlib-rs-differential` suite against the C oracle, since a
        // round trip that merely decompresses proves nothing about byte identity.
        let mut data = unique_trigrams(2000);
        let tail_start = data.len() - 40;
        for offset in 0..40 {
            data[tail_start + offset] = data[offset];
        }

        let mut state = default_state();
        let mut output = output_buffer();
        let mut cursors = cursors(&data, &mut output);

        assert_eq!(
            deflate_slow(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(state.window.strstart, data.len());
        assert_eq!(state.window.lookahead, 0);
        assert_eq!(cursors.avail_in(), 0);
    }

    #[test]
    fn every_level_that_dispatches_here_behaves_the_same_way() {
        // `configuration_table` rows 4 to 9 all name this function
        // (`deflate.c` L119-L124). The tuning parameters differ, so the emitted
        // bytes differ, but every level must complete a `Z_FINISH` and consume
        // its input -- the property that would break first if a level's
        // `max_lazy_match` or `max_chain_length` drove the loop off the end.
        let planted = plant(5500, PATTERN_SOURCE + 5000, 10);
        let data = planted.data;

        for level in 4..=9 {
            let mut state = new_state(level, DEF_MEM_LEVEL, Strategy::Default);
            let mut output = output_buffer();
            let mut cursors = cursors(&data, &mut output);

            assert_eq!(
                deflate_slow(&mut state, &mut cursors, Flush::Finish),
                BlockState::FinishDone,
                "level {level}"
            );
            assert_eq!(state.window.strstart, data.len(), "level {level}");
            assert_eq!(state.window.insert, 2, "level {level}");
            assert!(cursors.total_out > 0, "level {level}");
        }
    }
}
