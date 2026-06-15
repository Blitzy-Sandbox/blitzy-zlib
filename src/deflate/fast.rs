//! Greedy LZ77 matcher for the fast compression levels (1–3).
//!
//! This module is the Rust port of the C `deflate_fast()` function
//! (`deflate.c`, L1857-1948). It implements the *greedy* match policy: at every
//! window position it asks [`longest_match`](DeflateStream::longest_match) for
//! the best match starting there and, if that match reaches `MIN_MATCH`, emits a
//! `(distance, length)` pair immediately; otherwise it emits a single literal.
//! Unlike [`crate::deflate::slow`], it performs **no lazy evaluation** — a match
//! found at the current position is never deferred in the hope of a longer match
//! one byte later. This is what makes it fast and is the policy selected by rows
//! 1–3 of the per-level configuration table
//! ([`crate::deflate::strategy`]); levels 4–9 use the lazy matcher instead.
//!
//! # Byte-identical output
//!
//! The whole point of this port is that, for the same input, level, strategy and
//! window configuration, the emitted DEFLATE bitstream is **byte-for-byte
//! identical** to canonical C zlib (AAP §0.6.6, §0.7.1). Byte exactness here
//! hinges on three details that are reproduced from C verbatim:
//!
//! 1. **Match-acceptance guard** — a candidate is considered only when the hash
//!    chain head is not `NIL` *and* its distance is within `MAX_DIST`. The
//!    `match_length` field is written **only** inside that guard; on the literal
//!    path it keeps whatever value the previous iteration left it. This control
//!    flow is mirrored exactly — there is deliberately no
//!    `else { match_length = MIN_MATCH - 1; }`.
//! 2. **`do { … } while (--match_length)` insert loop** — after a match the
//!    intervening strings are inserted into the hash with the C do-while
//!    decrement-before-test semantics, so the hash chains (and therefore future
//!    match decisions) are populated in exactly the same order as C.
//! 3. **Two-byte post-match hash update** — when a long match is *not* inserted
//!    string-by-string, only `window[strstart]` and `window[strstart + 1]` are
//!    folded into `ins_h`; the third byte is intentionally left for the next
//!    `INSERT_STRING`. This matches the reference output even though the C
//!    comment notes it is "not strictly necessary".
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; the only
//!   `unsafe` in the crate lives in `crate::ffi` and `crate::inflate::fast`
//!   (AAP §0.6.2). This file contains zero `unsafe`, raw pointers or transmutes.
//! * **`no_std`-clean.** Nothing here references `std`; the function operates
//!   purely on the borrowed [`DeflateStream`] state.

use crate::constants::FlushMode;
// `flush_block` is the shared `FLUSH_BLOCK` helper defined in the deflate module
// root (`mod.rs`). It flushes the current block, advances `block_start`, pushes
// the pending buffer to the output, and returns `Some(state)` when the call must
// bail out early because the output buffer is full — exactly mirroring the C
// `FLUSH_BLOCK` macro's `return (last) ? finish_started : need_more;`.
use crate::deflate::flush_block;
use crate::deflate::state::{BlockState, DeflateStream, MIN_LOOKAHEAD, MIN_MATCH, NIL, max_dist};

/// Compress as much of the input as possible with the greedy matcher, returning
/// the resulting [`BlockState`].
///
/// This is the direct port of C `deflate_fast()` and conforms to the
/// `CompressFn` contract — `fn(&mut DeflateStream, FlushMode) -> BlockState` —
/// shared by all five `deflate_*` strategy functions, so the engine in
/// `mod.rs` can dispatch to it through the same signature used for
/// [`crate::deflate::slow`], [`crate::deflate::stored`], etc.
///
/// The loop runs until it either consumes all the available lookahead (when no
/// further progress is possible without more input/output) or completes a
/// finish. Each iteration:
///
/// 1. refills the window when the lookahead drops below `MIN_LOOKAHEAD`;
/// 2. inserts the 3-byte string at `strstart` into the hash and reads the head
///    of its chain;
/// 3. runs [`longest_match`](DeflateStream::longest_match) when the chain head
///    is a usable, in-range candidate;
/// 4. emits either a length/distance pair (and advances `strstart` past the
///    match, inserting the covered strings) or a single literal; and
/// 5. flushes the block when the symbol buffer fills.
///
/// # Returns
///
/// * [`BlockState::NeedMore`] — ran out of input/output mid-block under
///   `Z_NO_FLUSH`; the caller should supply more and call again.
/// * [`BlockState::FinishStarted`] — a finishing flush could not be completed
///   because the output buffer filled; propagated from [`flush_block`].
/// * [`BlockState::FinishDone`] — the stream was finished (`flush == Finish`)
///   and the final block was emitted.
/// * [`BlockState::BlockDone`] — a (non-finishing) block boundary was reached.
pub(crate) fn deflate_fast(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // Head of the hash chain for the string at `strstart` (a window position, or
    // `NIL` when the chain is empty). `IPos`/`Pos` in C is a `u16`.
    let mut hash_head: u16;
    // Set when the symbol buffer filled and the current block must be flushed.
    let mut bflush: bool;

    loop {
        // Make sure that we always have enough lookahead, except at the end of
        // the input file. We need `MAX_MATCH` bytes for the next match, plus
        // `MIN_MATCH` bytes to insert the string following the next match.
        if s.state.lookahead < MIN_LOOKAHEAD {
            s.fill_window();
            if s.state.lookahead < MIN_LOOKAHEAD && flush == FlushMode::NoFlush {
                return BlockState::NeedMore;
            }
            if s.state.lookahead == 0 {
                // Flush the current block.
                break;
            }
        }

        // Insert the string `window[strstart .. strstart + 2]` in the
        // dictionary, and set `hash_head` to the head of the hash chain.
        hash_head = NIL;
        if s.state.lookahead >= MIN_MATCH {
            hash_head = s.state.insert_string(s.state.strstart);
        }

        // Find the longest match, discarding those `<= prev_length`. At this
        // point we always have `match_length < MIN_MATCH`.
        //
        // To simplify the code, we prevent matches with the string of window
        // index 0 (in particular we have to avoid a match of the string with
        // itself at the start of the input file). `match_length` is written
        // ONLY here, inside the guard; on the literal path below it retains the
        // value left by the previous iteration — exactly as in C.
        if hash_head != NIL && s.state.strstart - (hash_head as usize) <= max_dist(s.state.w_size) {
            // `longest_match()` also sets `s.state.match_start`.
            s.state.match_length = s.longest_match(hash_head as usize);
        }

        if s.state.match_length >= MIN_MATCH {
            // `check_match` is a debug-only consistency assertion in C; omitted.

            // Tally the length/distance pair. The distance is the full
            // `strstart - match_start`; the length code argument is
            // `match_length - MIN_MATCH`, which lies in `0..=255` and so fits a
            // `u8`. Both reads are hoisted into locals so the subsequent
            // `&mut self` tally call has no borrow overlap with the field reads.
            let dist = s.state.strstart - s.state.match_start;
            let len = (s.state.match_length - MIN_MATCH) as u8;
            bflush = s.state.tr_tally_dist(dist, len);

            s.state.lookahead -= s.state.match_length;

            // Insert new strings in the hash table only if the match length is
            // not too large. This saves time but degrades compression.
            // `max_lazy_match` doubles as C's `max_insert_length`.
            if s.state.match_length <= s.state.max_lazy_match && s.state.lookahead >= MIN_MATCH {
                // The string at `strstart` is already in the table.
                s.state.match_length -= 1;
                // Port of `do { strstart++; INSERT_STRING(...); } while
                // (--match_length != 0);`. The decrement-before-test do-while is
                // reproduced by decrementing and testing AFTER the body. Because
                // this branch is reached only when `match_length >= MIN_MATCH`
                // (>= 3), the pre-decrement leaves it >= 2, so the loop always
                // runs at least once and `match_length` reaches exactly 0 (never
                // underflowing the unsigned counter).
                loop {
                    s.state.strstart += 1;
                    // `strstart` never exceeds `WSIZE - MAX_MATCH`, so there are
                    // always `MIN_MATCH` bytes ahead. `insert_string` is called
                    // purely for its side effects here (updating `ins_h`, `prev`
                    // and `head`); the returned chain head is intentionally
                    // discarded, mirroring C where `INSERT_STRING`'s assignment
                    // to `hash_head` inside this loop is dead (the value is
                    // reset to `NIL` at the top of the outer loop before any
                    // read).
                    s.state.insert_string(s.state.strstart);
                    s.state.match_length -= 1;
                    if s.state.match_length == 0 {
                        break;
                    }
                }
                s.state.strstart += 1;
            } else {
                s.state.strstart += s.state.match_length;
                s.state.match_length = 0;
                // Recompute the rolling hash from the two bytes now at the new
                // `strstart`. Only TWO bytes are folded in (matching C); the
                // third byte is absorbed by the next `INSERT_STRING`. If
                // `lookahead < MIN_MATCH`, `ins_h` is garbage here, but it does
                // not matter since it is recomputed at the next `deflate` call.
                let strstart = s.state.strstart;
                s.state.ins_h = s.state.window[strstart] as usize;
                let next = s.state.window[strstart + 1];
                s.state.ins_h = s.state.update_hash(s.state.ins_h, next);
                // (`UPDATE_HASH` is not called for the third byte; for
                // `MIN_MATCH == 3` no further updates are required.)
            }
        } else {
            // No match; output a literal byte. The byte is read into a local
            // first so the `&mut self` tally call does not overlap the field
            // borrow.
            let lc = s.state.window[s.state.strstart];
            bflush = s.state.tr_tally_lit(lc);
            s.state.lookahead -= 1;
            s.state.strstart += 1;
        }

        if bflush {
            if let Some(bs) = flush_block(s, false) {
                return bs;
            }
        }
    }

    // Remember the bytes at the tail of the window that still need inserting
    // into the hash on the next call (`s->insert`).
    s.state.insert = if s.state.strstart < MIN_MATCH - 1 {
        s.state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == FlushMode::Finish {
        if let Some(bs) = flush_block(s, true) {
            return bs;
        }
        return BlockState::FinishDone;
    }

    if s.state.sym_next != 0 {
        if let Some(bs) = flush_block(s, false) {
            return bs;
        }
    }

    BlockState::BlockDone
}
