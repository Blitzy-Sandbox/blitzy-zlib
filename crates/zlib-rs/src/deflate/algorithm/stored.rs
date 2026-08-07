//! `deflate_stored`: store the input without compressing it. The level-0 compressor.
//!
//! Port of `deflate_stored` (`deflate.c` L1668-L1848), the strategy `deflate()` selects
//! first, before the strategy is even consulted: `bstate = s->level == 0 ?
//! deflate_stored(s, flush) : ...` (`deflate.c` L1217-L1220). It is also row 0 of
//! `configuration_table`, `{0, 0, 0, 0, deflate_stored}` (`deflate.c` L114), which is
//! [`crate::deflate::config_table::CONFIGURATION_TABLE`] here.
//!
//! The function's own header comment (`deflate.c` L1653-L1666) states both its purpose and
//! its design goal:
//!
//! ```text
//! Copy without compression as much as possible from the input stream, return
//! the current block state.
//!
//! In case deflateParams() is used to later switch to a non-zero compression
//! level, s->matches (otherwise unused when storing) keeps track of the number
//! of hash table slides to perform. If s->matches is 1, then one hash table
//! slide will be done when switching. If s->matches is 2, the maximum value
//! allowed here, then the hash table will be cleared, since two or more slides
//! is the same as a clear.
//!
//! deflate_stored() is written to minimize the number of times an input byte is
//! copied. It is most efficient with large input and output buffers, which
//! maximizes the opportunities to have a single copy from next_in to next_out.
//! ```
//!
//! # The odd one out
//!
//! Every other compressor in this directory finds matches: it calls `fill_window` when the
//! lookahead runs short, walks the hash chains through `longest_match`, and tallies symbols
//! for the Huffman coder. This one does none of that, and the omissions are the algorithm
//! rather than an incomplete port:
//!
//! * **`fill_window`** (`deflate.c` L252) -- this function manages the window itself, with
//!   two direct `read_buf` calls and four explicit copies. It never needs a lookahead,
//!   because it never looks ahead.
//! * **`longest_match`** (`deflate.c` L1389) -- there are no matches in a stored block.
//! * **`INSERT_STRING` / `UPDATE_HASH` / `CLEAR_HASH`** -- the hash chains are left exactly
//!   as they were found. `matches` records how many slides they *would* need, for a later
//!   `deflateParams`; see [`deflate_stored`].
//! * **`_tr_tally_lit` / `_tr_tally_dist`** (`deflate.h` L311-L317) -- a stored block carries
//!   raw bytes, not symbols. The only trees entry point used is
//!   [`crate::trees::_tr_stored_block`].
//! * **`check_match`** (`deflate.c` L1592-L1621) -- `ZLIB_DEBUG`-only, and about matches, of
//!   which there are none.
//! * **`FLUSH_BLOCK` / `FLUSH_BLOCK_ONLY`** (`deflate.c` L1630-L1646) -- both wrap
//!   `_tr_flush_block`, which codes symbols. This function writes its blocks itself, so that
//!   it can control their exact lengths.
//!
//! # Why this is the most delicate of the five
//!
//! It decides stored-block *boundaries and lengths*, and those appear in the output as the
//! `LEN`/`NLEN` pair of every block (`doc/rfc1951.txt` section 3.2.4). Three details are
//! easy to "tidy" and impossible to tidy safely:
//!
//! 1. **`min_block` is computed twice, with different formulas.** Once before the copy loop
//!    as `MIN(pending_buf_size - 5, w_size)` (`deflate.c` L1673) and once after it as
//!    `MIN(have, w_size)` where `have` is itself
//!    `MIN(pending_buf_size - header_bytes, MAX_STORED)` (`deflate.c` L1828-L1831). They are
//!    two distinct thresholds for two distinct decisions and must not be shared.
//! 2. **`last` is computed twice, with different conditions.** `flush == Z_FINISH && len ==
//!    left + avail_in` for a block copied straight to the output (`deflate.c` L1712), and
//!    `flush == Z_FINISH && avail_in == 0 && len == left` for a block written to pending
//!    (`deflate.c` L1837-L1838).
//! 3. **The header is written by emitting a dummy block and then patching it.**
//!    `_tr_stored_block(s, NULL, 0, last)` produces a correct, byte-aligned, empty header,
//!    and the four length bytes it wrote are then overwritten in place with the real length
//!    (`deflate.c` L1713-L1719).
//!
//! Beyond those, every clamp in the chain at `deflate.c` L1687-L1697 and every disjunct of
//! the two emit tests must be reproduced as written. A chain that is "equivalent" for the
//! cases one happens to test will split blocks differently on the cases one does not, and a
//! different split is a different byte stream.
//!
//! # Where the bytes go
//!
//! There are two destinations, and the whole function is the choice between them:
//!
//! * **Straight to the caller's output buffer.** The header goes into `pending_buf` and is
//!   flushed, then the payload is copied from the window and from `next_in` directly to
//!   `next_out`, never passing through `pending_buf`. This is the fast path the design goal
//!   above describes, and it is the loop at `deflate.c` L1682-L1751.
//! * **Into the pending buffer.** When there was not enough output space for a whole worthy
//!   block, the remaining window bytes are written as a stored block into `pending_buf`
//!   instead, and flushed from there. This is `deflate.c` L1828-L1842.
//!
//! Between the two, the window is brought up to date so that a later `deflateParams` switch
//! to a compressing level has a valid history to match against (`deflate.c` L1753-L1821).
//!
//! # Deliberately not implemented
//!
//! Only the default compile-time configuration, and none of the following is offered as a
//! runtime option or a Cargo feature, because each one changes the emitted bytes:
//!
//! * `ZLIB_DEBUG` -- the `s->compressed_len += len << 3` and `s->bits_sent += len << 3`
//!   counters at `deflate.c` L1724-L1728 have no counterpart, and neither does `Tracev`.
//! * `FASTEST` (`deflate.c` L106) -- replaces `configuration_table` with two entries; this
//!   port always has all ten levels, so level 0 always reaches this function.
//! * `LIT_MEM` (`deflate.h` L28, commented out) -- would change `LIT_BUFS` from 4 to 5 and
//!   split the symbol buffer in two. `crate::weak_slice::LIT_BUFS` is 4.
//! * `FORCE_STORED` (`trees.c` L1044) -- overrides block-type selection in `_tr_flush_block`.
//!   It is not introduced, here or anywhere.

use core::cmp::min;
use core::mem::size_of;

// Where each of these comes from, since none of it is a choice:
//
// * `BlockState`, `Flush`, `StreamCursors` and `MAX_STORED` are the shared dispatch
//   vocabulary of the parent module, `deflate/algorithm.rs`. `StreamCursors` is this port's
//   stand-in for C's `s->strm` back-pointer, which a `&mut DeflateState` cannot hold without
//   aliasing itself; see that module's documentation.
// * `Allocator` and `DeflateState` cannot be named without each other:
//   `DeflateState<'a, A: Allocator<'a>>`.
// * `flush_pending` is `deflate.c` L1722 and L1841, and `flush_bits` is
//   `crate::trees::_tr_flush_bits`, which `flush_pending` takes as an argument rather than
//   importing -- see `deflate/pending.rs` for why.
// * `read_buf` is `deflate.c` L1816, writing into the window; `read_buf_into_output` is the
//   whole of `deflate.c` L1745-L1750, writing into the caller's output buffer. Both are the
//   one shared port, so the wrap-dispatched check-value update happens identically at both
//   sites -- and for the bytes that go straight to `next_out` this is the *only* place their
//   check value is ever accumulated, because they never enter the window.
// * `_tr_stored_block` is `deflate.c` L1713 (the dummy header) and L1839 (the real block).
use crate::deflate::algorithm::{BlockState, Flush, StreamCursors, MAX_STORED};
use crate::deflate::pending::flush_pending;
use crate::deflate::state::{Allocator, DeflateState};
use crate::read_buf::{read_buf, read_buf_into_output};
use crate::trees::{_tr_stored_block, flush_bits};

/// This module converts byte counts to the `u64` that [`crate::trees::_tr_stored_block`]
/// takes and that `total_out` is held in, and the port's standard forbids a cast that could
/// truncate.
///
/// `crate::deflate::algorithm`, `crate::deflate::pending` and `crate::read_buf` assert the
/// same relationship for the same reason, at the same cost of nothing: on a hypothetical
/// target with a `usize` wider than `u64` this crate fails to compile rather than silently
/// mis-sizing a stored block.
const _: () = assert!(
    size_of::<usize>() <= size_of::<u64>(),
    "deflate_stored converts usize byte counts to u64; usize must not be the wider type"
);

/// `have = ((unsigned)s->bi_valid + 42) >> 3` -- "bytes in header"
/// (`deflate.c` L1688 and L1828).
///
/// The reference's estimate of how much room a stored block's header needs, and it is used
/// twice with two different meanings: at L1688-L1692 it is subtracted from `avail_out` to
/// find the largest payload that will fit in the caller's buffer, and at L1828-L1830 it is
/// subtracted from `pending_buf_size` to find the largest payload that will fit in the
/// pending buffer.
///
/// The constant 42 is not derived from anything a reader can recompute, and it is
/// deliberately reproduced rather than re-derived: it covers the three block-type bits, the
/// padding to the next byte boundary, the four `LEN`/`NLEN` bytes and the slack the
/// reference chose to leave. Simplifying it -- to `5 + (bi_valid + 7) / 8`, say -- would
/// shift the `avail_out` threshold and therefore change where blocks are split.
///
/// # The cast
///
/// C casts the `int` field to `unsigned` before adding. `bi_valid` is "the number of valid
/// bits in `bi_buf`" (`deflate.h` L270-L273) and lives in `0..=16`: `bi_windup` clears it
/// (`trees.c` L182-L188), `bi_flush` reduces it by 8 or to 0 (`trees.c` L166-L179), and
/// `send_bits` keeps it below `Buf_size` (`trees.c` L253-L268). On that domain the cast is
/// the identity and the result is 5, 6 or 7. It is written as a reinterpretation anyway,
/// with wrapping addition, so that the function is total and matches C bit for bit even on a
/// value neither implementation should ever see.
// `bi_valid` is `int` in C (`deflate.h` L270) and `i32` here, and `(unsigned)` is a
// bit-pattern reinterpretation; `clippy::pedantic` flags exactly that as `cast_sign_loss`.
// The relaxation is scoped to this three-line function rather than to any caller.
#[allow(clippy::cast_sign_loss)]
#[inline]
const fn header_bytes(bi_valid: i32) -> usize {
    // `(unsigned)s->bi_valid`
    let bits = bi_valid as u32;

    // `+ 42` in C's `unsigned` arithmetic, which wraps rather than trapping, then `>> 3`.
    // Unreachable on the real domain, where `bits <= 16`.
    let bytes = bits.wrapping_add(42) >> 3;

    // Widening on every target with a `usize` of at least 32 bits, which is every target this
    // crate supports; the value itself is at most 7 on the real domain.
    bytes as usize
}

/// Copies the input to the output without compressing it, and reports the block state.
///
/// Port of `local block_state deflate_stored(deflate_state *s, int flush)`
/// (`deflate.c` L1668-L1848) -- the level-0, store-only compression strategy. `local` in C
/// and `pub(crate)` here: it is an internal of the implementation, it does not appear in
/// `zlib.map`, and it must never become an exported symbol.
///
/// # Parameters
///
/// `state` is C's `deflate_state *s`. `cursors` is everything C reaches through `s->strm`;
/// this port has no such back-pointer, for the reason the parent module's documentation
/// gives. `flush` is C's `int flush`, already narrowed to the six values `deflate()` accepts
/// (`Z_TREES` is rejected at `deflate.c` L985, before dispatch).
///
/// # Return value
///
/// One of the four `block_state` values (`deflate.c` L63-L68), and which one is returned is
/// part of the contract with `deflate()`:
///
/// * [`BlockState::FinishDone`] -- the final block reached `next_out`; nothing is left to do
///   (`deflate.c` L1790-L1793).
/// * [`BlockState::BlockDone`] -- flushing, and all input has been consumed
///   (`deflate.c` L1796-L1798).
/// * [`BlockState::FinishStarted`] -- the final block was written to `pending_buf` but has not
///   all reached the caller yet (`deflate.c` L1847).
/// * [`BlockState::NeedMore`] -- more input or more output space is needed
///   (`deflate.c` L1847).
///
/// Nothing here is fallible in the [`crate::error::ReturnCode`] sense and nothing panics: an
/// error is expressed by the returned [`BlockState`] and by the state the function leaves
/// behind, exactly as in C.
///
/// # The `matches` counter
///
/// `s->matches` is "number of string matches in current block" (`deflate.h` L258) and is
/// meaningless while storing, so this function borrows it as a *pending hash-table slide*
/// counter for a later `deflateParams` switch to a compressing level
/// (`deflate.c` L1657-L1662): 1 means one `slide_hash` will be performed at the switch,
/// 2 means the hash table will be cleared instead, and **2 is the maximum value allowed
/// here** because two or more slides is the same as a clear. `deflateParams` reads it at
/// `deflate.c` L801-L806.
///
/// So it is set to 2 outright when the copied input supplanted the whole history
/// (`deflate.c` L1765), incremented -- but never past 2 -- on each of the two window slides
/// (`deflate.c` L1775-L1776 and L1807-L1808), and never reset to zero. The hash chains
/// themselves are not touched.
///
/// # `bi_used`
///
/// Set to 8 on both completion paths, `deflate.c` L1791 and L1846, because a stored block
/// always ends on a byte boundary. It is what `deflateUsed` reports to the caller
/// (`deflate.c` L737-L743), so omitting either assignment would be observable through the
/// public API.
// `_tr_stored_block` keeps the underscore-prefixed C spelling of `deflate.h` L317-L318 for
// oracle traceability, which `clippy::pedantic` flags at the call site. The same relaxation,
// for the same reason, appears in `deflate/algorithm.rs` and `trees/mod.rs`.
//
// The function is long and deeply nested because the reference function is, and reproducing
// its control flow statement for statement is the only way to reproduce its block splits.
// `clippy.toml` raises `too-many-lines-threshold` to 400 and
// `cognitive-complexity-threshold` to 60 naming this function, so no restructuring is
// required or wanted; the second relaxation below is belt and braces for a build whose
// counted line total differs.
#[allow(clippy::used_underscore_items, clippy::too_many_lines)]
pub(crate) fn deflate_stored<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // ```c
    // /* Smallest worthy block size when not flushing or finishing. By default
    //  * this is 32K. This can be as small as 507 bytes for memLevel == 1. For
    //  * large input and output buffers, the stored block size will be larger.
    //  */
    // unsigned min_block = (unsigned)(MIN(s->pending_buf_size - 5, s->w_size));
    // ```
    // -- `deflate.c` L1669-L1673. FIRST of the two `min_block` formulas; the second, at
    // L1831, is a different expression for a different decision and is written out there.
    //
    // The `- 5` is the four `LEN`/`NLEN` bytes plus one for the byte holding the three
    // block-type bits. `saturating_sub` cannot saturate: `pending_buf_size` is
    // `lit_bufsize * LIT_BUFS` with `lit_bufsize >= 128` (`crate::weak_slice::MIN_LIT_BUFSIZE`,
    // from `1 << (memLevel + 6)` at `deflate.c` L464), so it is at least 512.
    let mut min_block = min(state.pending_buf_size().saturating_sub(5), state.w_size());

    // `int last = 0;`  (`deflate.c` L1679)
    //
    // A `bool`: C only ever stores 0 or 1 in it and only ever tests it for truth, and
    // `_tr_stored_block` takes the `BFINAL` bit as a `bool` in this port.
    let mut last = false;

    // `unsigned used = s->strm->avail_in;`  (`deflate.c` L1681)
    //
    // Captured BEFORE the loop, and turned into "bytes actually consumed" by
    // `used -= s->strm->avail_in` at L1759. It must not be recomputed inside the loop: the
    // whole point is to measure how much input the loop copied straight through to the
    // output, which is the only record of it -- those bytes never passed through the window.
    let mut used = cursors.avail_in();

    // `s->w_size` and `s->window_size` are read at eight places below and can no longer
    // change: both are fixed by `deflateInit2_` (`deflate.c` L450 and L683) and nothing this
    // function calls reconfigures the stream.
    let w_size = state.w_size();
    let window_size = state.window_size();

    // `(long)s->w_size` for the `block_start` adjustment at `deflate.c` L1804, converted
    // once. Exact for every window this port accepts: `w_bits <= MAX_WBITS` gives
    // `w_size <= 32768`. The fallback is unreachable, and is written rather than asserted so
    // that this function has no panicking path.
    let w_size_signed = isize::try_from(w_size).unwrap_or(isize::MAX);

    // `strm->state->wrap`, which `read_buf` consults on every call (`deflate.c` L228 and
    // L232) to decide between Adler-32, CRC-32 and no check value at all.
    //
    // Read once, before the loop, for a borrow-checker reason with a real justification
    // behind it: the destination handed to `read_buf` at L1816 is a mutable borrow of
    // `state.window`, so `wrap` cannot be read out of `state` while that borrow is live.
    // Hoisting is sound because nothing between here and the last `read_buf` call can change
    // it -- `wrap` is assigned only by `deflateInit2_` (L447), `deflateResetKeep` (L660),
    // `deflateSetDictionary` (L577 and L620) and `deflate()` itself (L1288), none of which
    // this function or anything it calls can reach.
    let wrap = state.wrap;

    // ```c
    // /* Copy as many min_block or larger stored blocks directly to next_out as
    //  * possible. If flushing, copy the remaining available input to next_out as
    //  * stored blocks, if there is enough space.
    //  */
    // do {
    // ```
    // -- `deflate.c` L1675-L1682. Rust has no `do`/`while`, so the continuation test is the
    // last statement of the body; the body therefore always runs at least once, as C's does.
    loop {
        // `len = MAX_STORED;  /* maximum deflate stored block length */`  (L1687)
        //
        // The clamp chain starts here and narrows in exactly this order: `MAX_STORED`, then
        // the available input, then the available output. Each step is its own `if` in C and
        // its own `if` here. Collapsing them into one `min` of three terms would give the same
        // answer for these three particular clamps -- and would be a standing invitation to
        // reorder or merge a future fourth, which would not.
        let mut len = MAX_STORED;

        // `have = ((unsigned)s->bi_valid + 42) >> 3;   /* bytes in header */`  (L1688)
        let header = header_bytes(state.bi_valid);

        // ```c
        // if (s->strm->avail_out < have)          /* need room for header */
        //     break;
        // ```
        // -- L1689-L1690. Without room for the header there is no point computing anything
        // else; the tail of the function will write a block into `pending_buf` instead.
        if cursors.avail_out() < header {
            break;
        }

        // ```c
        //     /* maximum stored block length that will fit in avail_out: */
        // have = s->strm->avail_out - have;
        // ```
        // -- L1691-L1692. `have` changes meaning here, from "header bytes" to "payload bytes
        // that will fit"; the reference reuses the variable and so does this port, under a
        // separate name so that both meanings are visible at once. Exact rather than merely
        // saturating: the test above established `avail_out >= header`.
        let have = cursors.avail_out().saturating_sub(header);

        // `left = (unsigned)(s->strstart - s->block_start);    /* window bytes */`  (L1693)
        //
        // The window bytes belonging to the current block and not yet written out.
        // `bytes_since_block_start` performs the subtraction without the unsigned wraparound
        // C's cast relies on, reporting [`None`] where `block_start` is ahead of `strstart`;
        // that cannot happen on any path this function can reach, and 0 is the value that
        // makes the clamps below decline to emit rather than emit something wrong.
        let mut left = state.window.bytes_since_block_start().unwrap_or(0);

        // `left + s->strm->avail_in` -- everything that could go into a block right now,
        // whether it is already in the window or still in the caller's input buffer. C
        // computes this three times, at L1694, L1706 and L1712; the operands do not change
        // between them, because `left` is not modified until L1733 and `avail_in` not until
        // L1746, so it is computed once here.
        //
        // # A deliberate width difference, and why it is unreachable
        //
        // C evaluates this sum in `ulg` at L1694 but in `unsigned` -- 32-bit -- at L1706 and
        // L1712, where it can therefore wrap. Here it is `usize`, so it cannot. The two
        // implementations can only disagree when `left + avail_in` exceeds 2^32, which needs
        // an `avail_in` within 64 KiB of 4 GiB in a single call *and* unflushed window bytes;
        // on that input C wraps the sum to a small number and can conclude from L1712 that
        // the stream is finished having consumed almost none of it, which is a defect in the
        // reference rather than behaviour to reproduce. Every input the differential matrix
        // and every real caller produce is orders of magnitude below that bound, so the
        // emitted bytes are identical.
        let available = left.saturating_add(cursors.avail_in());

        // ```c
        // if (len > (ulg)left + s->strm->avail_in)
        //     len = left + s->strm->avail_in;     /* limit len to the input */
        // ```
        // -- L1694-L1695.
        if len > available {
            len = available;
        }

        // ```c
        // if (len > have)
        //     len = have;                         /* limit len to the output */
        // ```
        // -- L1696-L1697.
        if len > have {
            len = have;
        }

        // ```c
        // /* If the stored block would be less than min_block in length, or if
        //  * unable to copy all of the available input when flushing, then try
        //  * copying to the window and the pending buffer instead. Also don't
        //  * write an empty block when flushing -- deflate() does that.
        //  */
        // if (len < min_block && ((len == 0 && flush != Z_FINISH) ||
        //                         flush == Z_NO_FLUSH ||
        //                         len != left + s->strm->avail_in))
        //     break;
        // ```
        // -- L1699-L1707. A three-way disjunction inside a conjunction, and each part is
        // load-bearing, in the order the comment above explains them:
        //
        // * `len < min_block` gates the whole test: a block at least `min_block` long is
        //   always worth writing, whatever the flush mode.
        // * `len == 0 && flush != Z_FINISH` -- an empty block is never written on a flush,
        //   because `deflate()` writes the flush marker itself (`deflate.c` L1238-L1250).
        //   `Z_FINISH` is excluded because a zero-length final block is exactly what an empty
        //   stream needs.
        // * `flush == Z_NO_FLUSH` -- with no flush requested there is no reason to emit a
        //   short block at all; the input is better accumulated in the window.
        // * `len != left + avail_in` -- when flushing, a block that would leave some of the
        //   available input behind is refused, so that the flush actually flushes.
        if len < min_block
            && ((len == 0 && flush != Flush::Finish) || flush == Flush::NoFlush || len != available)
        {
            break;
        }

        // ```c
        // /* Make a dummy stored block in pending to get the header bytes,
        //  * including any pending bits. This also updates the debugging counts.
        //  */
        // last = flush == Z_FINISH && len == left + s->strm->avail_in ? 1 : 0;
        // _tr_stored_block(s, (char *)0, 0L, last);
        // ```
        // -- L1709-L1713. FIRST of the two `last` computations; the second, at L1837-L1838,
        // adds `avail_in == 0` and compares against `left` rather than `available`.
        //
        // The call is the dummy: a null buffer and a zero length, purely so that the bit
        // writer emits a correct three-bit block header, flushes whatever partial byte was in
        // `bi_buf` and pads to the byte boundary that `doc/rfc1951.txt` section 3.2.4
        // requires, and then writes a `LEN`/`NLEN` pair -- which the four lines below
        // overwrite. Passing the real `len` here instead would be wrong, not merely different:
        // `_tr_stored_block` would try to copy `len` payload bytes out of the window, and the
        // payload may be partly in the window and partly still in the caller's input buffer.
        last = flush == Flush::Finish && len == available;
        _tr_stored_block(state, None, 0, last);

        // ```c
        // /* Replace the lengths in the dummy stored block with len. */
        // s->pending_buf[s->pending - 4] = (Bytef)len;
        // s->pending_buf[s->pending - 3] = (Bytef)(len >> 8);
        // s->pending_buf[s->pending - 2] = (Bytef)~len;
        // s->pending_buf[s->pending - 1] = (Bytef)(~len >> 8);
        // ```
        // -- L1715-L1719, which is
        // [`crate::weak_slice::PendingBuf::patch_tail`]`(4, &[lo, hi, !lo, !hi])`: the four
        // bytes ending at the write cursor, bounds-checked against it rather than indexed
        // backwards from it.
        //
        // `NLEN` is the one's complement of `LEN`, and getting it wrong makes every stored
        // block invalid. C complements the full 32-bit `unsigned` and then truncates each
        // byte; complementing after truncation gives the same two bytes, because `!` is
        // bitwise and truncation keeps the low bits -- so `!lo` and `!hi` are exactly
        // `(Bytef)~len` and `(Bytef)(~len >> 8)`.
        //
        // The masks make both conversions infallible, which is why they are written rather
        // than left to a cast: `len <= MAX_STORED` (65535) at this point, so the two bytes
        // carry it exactly, and `unwrap_or` never fires.
        let lo = u8::try_from(len & 0xff).unwrap_or(0);
        let hi = u8::try_from((len >> 8) & 0xff).unwrap_or(0);
        let patched = state.pending.patch_tail(4, &[lo, hi, !lo, !hi]);
        // `_tr_stored_block` has just appended at least five bytes -- one or more for the
        // header and exactly four for the lengths -- so the four-byte window is always inside
        // the pending output. Handled rather than asserted in release builds so that no path
        // here can panic; a refused patch would leave the dummy's zero lengths in place, which
        // a decompressor rejects loudly instead of silently mis-parsing.
        debug_assert!(
            patched,
            "stored block length patch outside pending_buf (deflate.c L1716-L1719)"
        );

        // `/* Write the stored block header bytes. */ flush_pending(s->strm);`  (L1721-L1722)
        //
        // Before the payload copies, so that the header reaches `next_out` ahead of the bytes
        // it describes. `crate::trees::flush_bits` is `_tr_flush_bits`, the step
        // `flush_pending` performs first; it is a parameter there rather than an import, and
        // there is exactly one correct value for it in the library.
        let _flushed = flush_pending(
            state,
            &mut cursors.output,
            &mut cursors.total_out,
            flush_bits,
        );

        // The `#ifdef ZLIB_DEBUG` counters at L1724-L1728 -- `s->compressed_len += len << 3`
        // and `s->bits_sent += len << 3` -- are debug-only accounting with no counterpart in
        // this port, which carries neither field.

        // ```c
        // /* Copy uncompressed bytes from the window to next_out. */
        // if (left) {
        //     if (left > len)
        //         left = len;
        //     zmemcpy(s->strm->next_out, s->window + s->block_start, left);
        //     s->strm->next_out += left;
        //     s->strm->avail_out -= left;
        //     s->strm->total_out += left;
        //     s->block_start += left;
        //     len -= left;
        // }
        // ```
        // -- L1730-L1740. Note that `left` is *mutated* by the clamp before the copy and that
        // `len` is reduced by it afterwards, so that the second copy below moves only the
        // remainder. Both matter: `left` is the block's window part and `len - left` is its
        // input part.
        if left != 0 {
            if left > len {
                left = len;
            }

            // `zmemcpy(s->strm->next_out, s->window + s->block_start, left)` (L1734) together
            // with `next_out += left` and `avail_out -= left` (L1735-L1736), which the output
            // cursor holds as one number.
            //
            // `block_region` is `s->window + s->block_start` with the length attached and the
            // range checked; it declines a negative `block_start`, where C would read in front
            // of the window. `block_start` is not negative on any path that reaches here --
            // `deflateParams` flushes the current block before switching to level 0
            // (`deflate.c` L789-L797), which sets `block_start = strstart` -- and the range
            // always fits, because `block_start + left <= strstart <= window_size`.
            let copied = match state.window.block_region(left) {
                Some(bytes) => cursors.output.push_slice(bytes),
                None => 0,
            };
            // `push_slice` copies only what fits. It always fits here: `avail_out` was at
            // least `header + len` before `flush_pending`, and `_tr_stored_block` writes
            // exactly `header` bytes -- three block-type bits padded to a byte boundary plus
            // four length bytes -- into a pending buffer that `deflate()` guarantees was empty
            // (`deflate.c` L1202-L1207). So at least `len >= left` bytes of output remain.
            // Using the returned count rather than `left` means that a caller who drove the
            // core with a non-empty pending buffer gets a short, consistent copy instead of
            // the buffer overrun C would produce.
            debug_assert_eq!(
                copied, left,
                "stored block window copy did not fit in avail_out (deflate.c L1734)"
            );

            // `s->strm->total_out += left;`  (L1737). Widening guarded by the compile-time
            // assertion above, so it cannot truncate; wrapping because `total_out` is an
            // unsigned C type and overflows by wrapping.
            cursors.total_out = cursors.total_out.wrapping_add(copied as u64);

            // `s->block_start += left;`  (L1738). The copied window bytes are now the
            // caller's, so the block restarts after them.
            state.window.block_start = state
                .window
                .block_start
                .saturating_add(isize::try_from(copied).unwrap_or(0));

            // `len -= left;`  (L1739)
            len = len.saturating_sub(copied);
        }

        // ```c
        // /* Copy uncompressed bytes directly from next_in to next_out, updating
        //  * the check value.
        //  */
        // if (len) {
        //     read_buf(s->strm, s->strm->next_out, len);
        //     s->strm->next_out += len;
        //     s->strm->avail_out -= len;
        //     s->strm->total_out += len;
        // }
        // ```
        // -- L1742-L1750, which is exactly [`read_buf_into_output`]; those five statements are
        // pure cursor plumbing around one `read_buf`, and bundling them is what makes
        // "copy through `next_out` but forget to advance it" unrepresentable.
        //
        // This is the only place the check value of these bytes is ever accumulated. They go
        // from the caller's input buffer straight to the caller's output buffer without
        // entering the window, so no later pass can see them; `read_buf` dispatching on `wrap`
        // -- 1 for Adler-32, 2 for CRC-32, anything else for none -- is what keeps the zlib and
        // gzip trailers correct for a level-0 stream.
        if len != 0 {
            let copied = read_buf_into_output(
                &mut cursors.input,
                &mut cursors.output,
                len,
                wrap,
                &mut cursors.check,
                &mut cursors.total_in,
                &mut cursors.total_out,
            );
            // `len` was clamped to `left + avail_in` and then to the output space, and `left`
            // has just been subtracted from it, so exactly `len` bytes of input and of output
            // room remain. As above, a short copy is reported rather than assumed away.
            debug_assert_eq!(
                copied, len,
                "stored block input copy was short (deflate.c L1746)"
            );
        }

        // `} while (last == 0);`  (L1751)
        if last {
            break;
        }
    }

    // ```c
    // /* Update the sliding window with the last s->w_size bytes of the copied
    //  * data, or append all of the copied data to the existing window if less
    //  * than s->w_size bytes were copied. Also update the number of bytes to
    //  * insert in the hash tables, in the event that deflateParams() switches to
    //  * a non-zero compression level.
    //  */
    // used -= s->strm->avail_in;      /* number of input bytes directly copied */
    // ```
    // -- L1753-L1759. `used` was `avail_in` on entry, so this turns it into the number of
    // bytes the loop above copied straight from `next_in` to `next_out`. Those bytes bypassed
    // the window entirely, and the window has to be brought up to date with them -- otherwise
    // a later `deflateParams` switch to a compressing level would match against stale history
    // and emit distances that point at the wrong bytes.
    //
    // `saturating_sub` cannot saturate: `avail_in` only ever decreases here.
    used = used.saturating_sub(cursors.avail_in());

    // ```c
    // if (used) {
    //     /* If any input was used, then no unused input remains in the window,
    //      * therefore s->block_start == s->strstart.
    //      */
    // ```
    // -- L1760-L1763.
    if used != 0 {
        if used >= w_size {
            // ```c
            // if (used >= s->w_size) {    /* supplant the previous history */
            //     s->matches = 2;         /* clear hash */
            //     zmemcpy(s->window, s->strm->next_in - s->w_size, s->w_size);
            //     s->strstart = s->w_size;
            //     s->insert = s->strstart;
            // }
            // ```
            // -- L1764-L1769. A whole window's worth of input went past, so every byte of the
            // old history is now out of reach and the window is rebuilt from scratch out of
            // the last `w_size` bytes that were copied. Two or more slides is the same as a
            // clear, so `matches` goes straight to its maximum.
            state.matches = 2;

            // `s->strm->next_in - s->w_size` reads *backwards* from the input cursor, into
            // input that has already been consumed. Those bytes are still in the caller's
            // buffer -- the caller must not touch it until the call returns -- so the read is
            // legitimate; in C it is unchecked pointer arithmetic that walks off the front of
            // the buffer whenever fewer than `w_size` bytes have been read.
            // `InputCursor::consumed_tail` is the checked form of exactly that expression.
            let history = cursors.input.consumed_tail(w_size);
            // `used >= w_size` and `used` bytes have just been consumed, so at least `w_size`
            // bytes lie behind the cursor. Handled rather than asserted so that this path
            // cannot panic; leaving the window untouched keeps it self-consistent, and the
            // cursors below still describe only bytes that were written.
            debug_assert!(
                history.is_some(),
                "fewer than w_size input bytes consumed (deflate.c L1766)"
            );
            if let Some(bytes) = history {
                let written = state.window.write_at(0, bytes);
                // The window buffer is `2 * w_size` bytes (`deflate.c` L458 and L683), so a
                // `w_size`-byte write at offset 0 always fits.
                debug_assert!(
                    written,
                    "history copy did not fit in the window (deflate.c L1766)"
                );
            }

            // `s->strstart = s->w_size;`  (L1767) and `s->insert = s->strstart;`  (L1768).
            // The whole window is history to be inserted into the hash chains at a switch.
            state.window.strstart = w_size;
            state.window.insert = state.window.strstart;
        } else {
            // ```c
            // if (s->window_size - s->strstart <= used) {
            //     /* Slide the window down. */
            //     s->strstart -= s->w_size;
            //     zmemcpy(s->window, s->window + s->w_size, s->strstart);
            //     if (s->matches < 2)
            //         s->matches++;   /* add a pending slide_hash() */
            //     if (s->insert > s->strstart)
            //         s->insert = s->strstart;
            // }
            // ```
            // -- L1771-L1779. Less than a window was copied, so the existing history is kept
            // and the new bytes are appended to it -- after sliding the upper half down if
            // they would not otherwise fit.
            if window_size.saturating_sub(state.window.strstart) <= used {
                // `s->strstart -= s->w_size;`  (L1773). Exact: the branch condition gives
                // `window_size - strstart <= used < w_size`, and `window_size == 2 * w_size`,
                // so `strstart > w_size`.
                state.window.strstart = state.window.strstart.saturating_sub(w_size);

                // `zmemcpy(s->window, s->window + s->w_size, s->strstart);`  (L1774). Source
                // and destination are regions of one buffer `w_size` apart, and `strstart` is
                // now at most `w_size`, so they do not overlap and `slide_down`'s
                // `copy_within` -- which has `memmove` semantics -- produces byte for byte
                // what C's `memcpy` produces.
                let count = state.window.strstart;
                let slid = state.window.slide_down(count);
                // `slide_down` refuses only when `w_size + count` exceeds the buffer, and
                // `count <= w_size` with a buffer of `2 * w_size`.
                debug_assert!(slid, "window slide was refused (deflate.c L1774)");

                // `if (s->matches < 2) s->matches++;`  (L1775-L1776). One more pending
                // `slide_hash`, capped at 2 -- see this function's documentation.
                if state.matches < 2 {
                    state.matches += 1;
                }

                // `if (s->insert > s->strstart) s->insert = s->strstart;`  (L1777-L1778)
                state.window.clamp_insert_to_strstart();
            }

            // `zmemcpy(s->window + s->strstart, s->strm->next_in - used, used);`  (L1780).
            // The second backwards read; see the one at L1766 above.
            let strstart = state.window.strstart;
            let copied_input = cursors.input.consumed_tail(used);
            // `used` bytes were consumed by the loop above, so `used` bytes lie behind the
            // cursor by construction.
            debug_assert!(
                copied_input.is_some(),
                "fewer than `used` input bytes consumed (deflate.c L1780)"
            );
            if let Some(bytes) = copied_input {
                let written = state.window.write_at(strstart, bytes);
                // `used < w_size` and, after any slide above, `window_size - strstart > used`,
                // so the append always fits.
                debug_assert!(
                    written,
                    "appended input did not fit in the window (deflate.c L1780)"
                );
            }

            // `s->strstart += used;`  (L1781)
            state.window.strstart = strstart.saturating_add(used);

            // `s->insert += MIN(used, s->w_size - s->insert);`  (L1782). The `MIN` is what
            // keeps `insert` from exceeding `w_size`: it counts bytes still to be inserted
            // into the hash chains, and the chains only reach back one window.
            state.window.insert = state
                .window
                .insert
                .saturating_add(min(used, w_size.saturating_sub(state.window.insert)));
        }

        // `s->block_start = s->strstart;`  (L1784), inside `if (used)` and nowhere else: the
        // copied bytes have already been written out, so the next block starts after them.
        // Saturating rather than unwrapping keeps the function total; `strstart` is at most
        // `window_size`, 65536, so the conversion is exact on every target this crate builds
        // for.
        state.window.block_start = isize::try_from(state.window.strstart).unwrap_or(isize::MAX);
    }

    // `if (s->high_water < s->strstart) s->high_water = s->strstart;`  (L1786-L1787).
    //
    // The FIRST of two identical raises; the second is at L1820-L1821 and both are needed,
    // because `strstart` can advance either side of the early returns below. `high_water`
    // records how far the window has been written, which is what lets `fill_window` know how
    // much of the over-scan region past the data still has to be zeroed
    // (`deflate.c` L340-L372) -- so under-reporting it would let a compressing level read
    // never-written bytes after a `deflateParams` switch.
    state.window.raise_high_water_to_strstart();

    // ```c
    // /* If the last block was written to next_out, then done. */
    // if (last) {
    //     s->bi_used = 8;
    //     return finish_done;
    // }
    // ```
    // -- L1789-L1793. `bi_used` is not optional: it is what `deflateUsed` reports
    // (`deflate.c` L737-L743), and a stored block always ends on a byte boundary, so all
    // eight bits of the last byte are used.
    if last {
        state.bi_used = 8;
        return BlockState::FinishDone;
    }

    // ```c
    // /* If flushing and all input has been consumed, then done. */
    // if (flush != Z_NO_FLUSH && flush != Z_FINISH &&
    //     s->strm->avail_in == 0 && (long)s->strstart == s->block_start)
    //     return block_done;
    // ```
    // -- L1795-L1798. All four conditions, and none is redundant: `Z_NO_FLUSH` has nothing to
    // finish, `Z_FINISH` is handled by the `last` path above and by the tail below, unconsumed
    // input means there is more to do, and `strstart == block_start` is what "nothing is left
    // in the window" means. `bytes_since_block_start() == Some(0)` is the signed comparison
    // `(long)s->strstart == s->block_start` without a cast: it is [`Some`]`(0)` exactly when
    // the two are equal.
    if flush != Flush::NoFlush
        && flush != Flush::Finish
        && cursors.avail_in() == 0
        && state.window.bytes_since_block_start() == Some(0)
    {
        return BlockState::BlockDone;
    }

    // `/* Fill the window with any remaining input. */`
    // `have = (unsigned)(s->window_size - s->strstart);`  (L1800-L1801)
    let mut have = window_size.saturating_sub(state.window.strstart);

    // ```c
    // if (s->strm->avail_in > have && s->block_start >= (long)s->w_size) {
    //     /* Slide the window down. */
    //     s->block_start -= s->w_size;
    //     s->strstart -= s->w_size;
    //     zmemcpy(s->window, s->window + s->w_size, s->strstart);
    //     if (s->matches < 2)
    //         s->matches++;           /* add a pending slide_hash() */
    //     have += s->w_size;          /* more space now */
    //     if (s->insert > s->strstart)
    //         s->insert = s->strstart;
    // }
    // ```
    // -- L1802-L1812. The second slide, and note the extra statement the first one does not
    // have: `block_start` moves down too, because unlike at L1771 there may still be
    // unflushed window bytes belonging to the current block. The `block_start >= w_size` guard
    // is what keeps that subtraction from going negative.
    if cursors.avail_in() > have && state.window.block_start >= w_size_signed {
        // `s->block_start -= s->w_size;`  (L1804). Exact by the guard above.
        state.window.block_start = state.window.block_start.saturating_sub(w_size_signed);

        // `s->strstart -= s->w_size;`  (L1805). Exact: `block_start <= strstart` always holds
        // here -- L1784 set them equal and only L1738 has moved `block_start`, forward and no
        // further than `strstart` -- so the guard `block_start >= w_size` gives
        // `strstart >= w_size`.
        state.window.strstart = state.window.strstart.saturating_sub(w_size);

        // `zmemcpy(s->window, s->window + s->w_size, s->strstart);`  (L1806). Non-overlapping
        // for the same reason as at L1774.
        let count = state.window.strstart;
        let slid = state.window.slide_down(count);
        debug_assert!(slid, "window slide was refused (deflate.c L1806)");

        // `if (s->matches < 2) s->matches++;`  (L1807-L1808)
        if state.matches < 2 {
            state.matches += 1;
        }

        // `have += s->w_size;  /* more space now */`  (L1809)
        have = have.saturating_add(w_size);

        // `if (s->insert > s->strstart) s->insert = s->strstart;`  (L1810-L1811)
        state.window.clamp_insert_to_strstart();
    }

    // ```c
    // if (have > s->strm->avail_in)
    //     have = s->strm->avail_in;
    // ```
    // -- L1813-L1814.
    if have > cursors.avail_in() {
        have = cursors.avail_in();
    }

    // ```c
    // if (have) {
    //     read_buf(s->strm, s->window + s->strstart, have);
    //     s->strstart += have;
    //     s->insert += MIN(have, s->w_size - s->insert);
    // }
    // ```
    // -- L1815-L1819. The second `read_buf` call, and the destination is the WINDOW this time,
    // not the caller's output buffer as at L1746. Both go through the one shared port, so the
    // wrap-dispatched check value is accumulated identically whichever way a byte travels --
    // which is what makes a level-0 stream's trailer independent of how the caller happened to
    // size its buffers.
    if have != 0 {
        let strstart = state.window.strstart;
        let copied = match state.window.region_mut(strstart, have) {
            Some(dest) => read_buf(
                &mut cursors.input,
                dest,
                wrap,
                &mut cursors.check,
                &mut cursors.total_in,
            ),
            // Unreachable: `have` was derived from `window_size - strstart` -- adjusted by
            // exactly `w_size` when the slide above moved `strstart` down by `w_size` -- and
            // then only reduced, so `strstart + have <= window_size`. Declining the read keeps
            // the cursors consistent instead of advancing `strstart` over bytes that were never
            // written, which is what C's unconditional `s->strstart += have` would do.
            None => 0,
        };
        debug_assert_eq!(copied, have, "window refill was short (deflate.c L1816)");

        // `s->strstart += have;`  (L1817)
        state.window.strstart = strstart.saturating_add(copied);

        // `s->insert += MIN(have, s->w_size - s->insert);`  (L1818), the same clamp as L1782.
        state.window.insert = state
            .window
            .insert
            .saturating_add(min(copied, w_size.saturating_sub(state.window.insert)));
    }

    // `if (s->high_water < s->strstart) s->high_water = s->strstart;`  (L1820-L1821). The
    // second raise; `strstart` has just advanced over freshly written window bytes.
    state.window.raise_high_water_to_strstart();

    // ```c
    // /* There was not enough avail_out to write a complete worthy or flushed
    //  * stored block to next_out. Write a stored block to pending instead, if we
    //  * have enough input for a worthy block, or if flushing and there is enough
    //  * room for the remaining input as a stored block in the pending buffer.
    //  */
    // have = ((unsigned)s->bi_valid + 42) >> 3;   /* bytes in header */
    //     /* maximum stored block length that will fit in pending: */
    // have = (unsigned)MIN(s->pending_buf_size - have, MAX_STORED);
    // min_block = MIN(have, s->w_size);
    // left = (unsigned)(s->strstart - s->block_start);
    // ```
    // -- L1823-L1832.
    //
    // SECOND of the two `min_block` formulas, and it is a different expression from L1673's:
    // there `min_block` was `MIN(pending_buf_size - 5, w_size)`, sized against the caller's
    // output buffer; here it is `MIN(have, w_size)` where `have` is itself
    // `MIN(pending_buf_size - header_bytes, MAX_STORED)`, sized against the pending buffer and
    // additionally capped at the largest length a `LEN` field can express. Sharing one value
    // between the two decisions -- or hoisting either computation -- changes which blocks are
    // written and therefore where they are split.
    let header = header_bytes(state.bi_valid);
    have = min(state.pending_buf_size().saturating_sub(header), MAX_STORED);
    min_block = min(have, w_size);
    let left = state.window.bytes_since_block_start().unwrap_or(0);

    // ```c
    // if (left >= min_block ||
    //     ((left || flush == Z_FINISH) && flush != Z_NO_FLUSH &&
    //      s->strm->avail_in == 0 && left <= have)) {
    // ```
    // -- L1833-L1835. A disjunction whose second arm is a four-way conjunction, reproduced
    // exactly:
    //
    // * `left >= min_block` -- a worthy block has accumulated in the window; write it whatever
    //   the flush mode.
    // * otherwise, all four of: something to write or a `Z_FINISH` that needs its final block
    //   even when empty (`left || flush == Z_FINISH`); a flush was actually requested
    //   (`flush != Z_NO_FLUSH`); the caller's input is exhausted, so this really is everything
    //   there is (`avail_in == 0`); and it fits in the pending buffer (`left <= have`).
    if left >= min_block
        || ((left != 0 || flush == Flush::Finish)
            && flush != Flush::NoFlush
            && cursors.avail_in() == 0
            && left <= have)
    {
        // `len = MIN(left, have);`  (L1836)
        let len = min(left, have);

        // ```c
        // last = flush == Z_FINISH && s->strm->avail_in == 0 &&
        //        len == left ? 1 : 0;
        // ```
        // -- L1837-L1838. SECOND of the two `last` computations, and deliberately not the
        // same test as L1712: it adds `avail_in == 0` and compares `len` against `left` rather
        // than against `left + avail_in`, because this block is drawn only from the window.
        last = flush == Flush::Finish && cursors.avail_in() == 0 && len == left;

        // `_tr_stored_block(s, (charf *)s->window + s->block_start, len, last);`  (L1839).
        //
        // The REAL block, not the dummy of L1713: the buffer is the window at `block_start`
        // and the length is `len`, so `_tr_stored_block` writes the header *and* copies the
        // payload, and no patching is needed. `usize::try_from(...).ok()` is C's
        // `s->window + s->block_start` guarded against a negative `block_start`, where C would
        // hand `_tr_stored_block` a pointer in front of the window; [`None`] is its `Z_NULL`,
        // and it is paired with the `len` the callee's own assertion checks.
        let buf = usize::try_from(state.window.block_start).ok();
        _tr_stored_block(state, buf, len as u64, last);

        // `s->block_start += len;`  (L1840)
        state.window.block_start = state
            .window
            .block_start
            .saturating_add(isize::try_from(len).unwrap_or(0));

        // `flush_pending(s->strm);`  (L1841). Moves as much of the block just written as fits
        // into the caller's output buffer; whatever does not fit stays pending for the next
        // call, which is exactly why [`BlockState::FinishStarted`] rather than
        // [`BlockState::FinishDone`] is returned below.
        let _flushed = flush_pending(
            state,
            &mut cursors.output,
            &mut cursors.total_out,
            flush_bits,
        );
    }

    // ```c
    // /* We've done all we can with the available input and output. */
    // if (last)
    //     s->bi_used = 8;
    // return last ? finish_started : need_more;
    // ```
    // -- L1844-L1847. The second of the two `bi_used = 8` assignments; see the one at L1791.
    //
    // `finish_started` and not `finish_done`: the final block has been written, but it went
    // into `pending_buf` and may not have reached the caller in full, so `deflate()` must be
    // called again. The driver treats the two alike in moving the stream to `FINISH_STATE`
    // (`deflate.c` L1222-L1224) but only `finish_started` also makes it return `Z_OK`
    // immediately (L1225-L1237).
    if last {
        state.bi_used = 8;
    }
    if last {
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
// `unwrap`, `panic` and indexing are already relaxed inside tests by `clippy.toml`;
// `used_underscore_items` is not, and these tests call `_tr_init`, whose name is the C
// spelling of `deflate.h` L311.
#[allow(clippy::used_underscore_items)]
mod tests {
    use super::{deflate_stored, header_bytes};
    use crate::deflate::algorithm::{BlockState, Flush, StreamCursors, MAX_STORED};
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL,
        MIN_MEM_LEVEL,
    };
    use crate::read_buf::{InputCursor, OutputCursor};
    use crate::trees::_tr_init;
    use alloc::vec;
    use alloc::vec::Vec;

    // -------------------------------------------------------------------------
    //  Fixtures
    // -------------------------------------------------------------------------

    /// Raw DEFLATE with the largest window: `windowBits = -15`, so `w_size` is 32768 and
    /// `window_size` is 65536, and nothing but the blocks themselves reaches the output.
    const RAW_WBITS: i32 = -15;

    /// `Z_UNKNOWN` (`zlib.h` L165). `deflate_stored` never reads or writes `data_type` --
    /// only `_tr_flush_block` does, and this function never calls it -- so the value is
    /// carried purely to build a complete [`StreamCursors`].
    const Z_UNKNOWN: i32 = 2;

    /// A stream configured as `deflateInit2(&strm, level, Z_DEFLATED, RAW_WBITS, mem_level,
    /// Z_DEFAULT_STRATEGY)` and wired up by `_tr_init`, which is the state a compressor is
    /// first entered with.
    ///
    /// In a debug build every block the allocator hands back is filled with `0xa5`, the byte
    /// `test/infcover.c` L87 uses, so nothing below can pass by accident on zeroed memory.
    fn new_state(level: i32, mem_level: i32) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level,
            method: Method::Deflated,
            window_bits: RAW_WBITS,
            mem_level,
            strategy: Strategy::Default,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        state
    }

    /// The level-0 stream at the default memory level: `min_block` is
    /// `MIN(65536 - 5, 32768)`, i.e. 32768.
    fn level_zero() -> DeflateState<'static, GlobalAllocator> {
        new_state(0, DEF_MEM_LEVEL)
    }

    /// Cursors over `input` and `output` with the scalars a freshly reset raw stream carries.
    fn new_cursors<'i, 'o>(input: &'i [u8], output: &'o mut [u8]) -> StreamCursors<'i, 'o> {
        StreamCursors {
            input: InputCursor::new(input),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// `n` bytes of a deterministic, incompressible-looking pattern.
    ///
    /// A multiplicative generator rather than a repeating byte, so that a copy landing at the
    /// wrong window offset shows up as a mismatch instead of comparing equal by luck.
    fn pattern(n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut x: u32 = 0x1234_5678;
        for _ in 0..n {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.push((x >> 24) as u8);
        }
        out
    }

    /// One stored block as it appears on the wire (`doc/rfc1951.txt` section 3.2.4).
    #[derive(Debug)]
    struct StoredBlock<'b> {
        /// The `BFINAL` bit.
        last: bool,
        /// The `LEN` field.
        len: usize,
        /// The `LEN` literal bytes that follow `NLEN`.
        payload: &'b [u8],
    }

    /// Splits a raw DEFLATE stream of stored blocks into its blocks, checking every header.
    ///
    /// Each block is `BFINAL` plus `BTYPE = 00` in the low three bits of a fresh byte -- fresh
    /// because `_tr_stored_block` calls `bi_windup`, which leaves `bi_valid` at zero -- then
    /// `LEN` and `NLEN` as little-endian 16-bit values, then `LEN` bytes of literal data.
    ///
    /// ★ `NLEN` is asserted to be the one's complement of `LEN` for every block, which is the
    /// property `deflate.c` L1718-L1719 exists to produce and the one a wrong complement
    /// silently destroys.
    fn parse_stored_blocks(bytes: &[u8]) -> Vec<StoredBlock<'_>> {
        let mut blocks = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            assert!(
                at + 5 <= bytes.len(),
                "truncated stored block header at offset {at}"
            );
            let header = bytes[at];
            assert_eq!(
                header & 0b110,
                0,
                "BTYPE is not 00 (stored) at offset {at}: {header:#04x}"
            );
            let last = header & 1 == 1;
            let len = usize::from(u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]));
            let nlen = u16::from_le_bytes([bytes[at + 3], bytes[at + 4]]);
            assert_eq!(
                nlen,
                !(u16::try_from(len).unwrap()),
                "NLEN is not the one's complement of LEN at offset {at}"
            );
            let start = at + 5;
            assert!(
                start + len <= bytes.len(),
                "truncated stored block payload at offset {start}"
            );
            blocks.push(StoredBlock {
                last,
                len,
                payload: &bytes[start..start + len],
            });
            at = start + len;
        }
        blocks
    }

    /// Adler-32 computed from RFC 1950's definition, as an independent oracle for the check
    /// value `read_buf` accumulates on the pass-through path.
    fn adler32_reference(data: &[u8]) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    // -------------------------------------------------------------------------
    //  header_bytes -- deflate.c L1688 and L1828
    // -------------------------------------------------------------------------

    /// `((unsigned)bi_valid + 42) >> 3` over the field's whole real domain, computed by hand.
    #[test]
    fn header_bytes_matches_the_c_expression() {
        for bi_valid in 0..=16_i32 {
            let expected = usize::try_from((bi_valid + 42) >> 3).unwrap();
            assert_eq!(header_bytes(bi_valid), expected, "bi_valid = {bi_valid}");
        }
        // The three values it can actually take, spelled out: five header bytes with an empty
        // bit buffer, six once a partial byte is pending, seven at the widest.
        assert_eq!(header_bytes(0), 5);
        assert_eq!(header_bytes(7), 6);
        assert_eq!(header_bytes(15), 7);
    }

    // -------------------------------------------------------------------------
    //  The direct-copy loop -- deflate.c L1682-L1751
    // -------------------------------------------------------------------------

    /// An empty stream finished in one call: a single empty final stored block.
    ///
    /// This is the whole of raw level-0 output for no input, `01 00 00 ff ff`, and it exercises
    /// the one case the `len == 0 && flush != Z_FINISH` disjunct at `deflate.c` L1704
    /// deliberately lets through.
    #[test]
    fn empty_input_with_finish_emits_one_empty_final_block() {
        let mut state = level_zero();
        let mut output = [0_u8; 64];
        let mut cursors = new_cursors(&[], &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(state.bi_used, 8, "deflate.c L1791");
        let emitted = cursors.output.written_slice();
        assert_eq!(emitted, &[0x01, 0x00, 0x00, 0xff, 0xff]);
        let blocks = parse_stored_blocks(emitted);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].last);
        assert_eq!(blocks[0].len, 0);
    }

    /// A short input finished in one call: header straight to pending, payload straight from
    /// `next_in` to `next_out`.
    ///
    /// The payload never enters the window on its way out (`deflate.c` L1745-L1750), so this
    /// also pins the window bookkeeping that `deflate.c` L1759-L1787 performs afterwards.
    #[test]
    fn short_input_with_finish_is_copied_straight_through() {
        let data = b"hello, hello!";
        let mut state = level_zero();
        let mut output = [0_u8; 64];
        let mut cursors = new_cursors(data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        let emitted = cursors.output.written_slice();
        assert_eq!(emitted.len(), 5 + data.len());
        let blocks = parse_stored_blocks(emitted);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].last);
        assert_eq!(blocks[0].len, data.len());
        assert_eq!(blocks[0].payload, data);

        // `deflate.c` L1759-L1784: all 13 bytes were used, fewer than `w_size`, so they are
        // appended to the window and the block restarts after them.
        assert_eq!(cursors.avail_in(), 0);
        assert_eq!(cursors.total_in, 13);
        assert_eq!(cursors.total_out, 18);
        assert_eq!(state.window.strstart, 13);
        assert_eq!(state.window.block_start, 13);
        assert_eq!(state.window.insert, 13);
        assert_eq!(state.window.high_water(), 13);
        assert_eq!(state.matches, 0, "no slide is pending after a plain append");
        assert_eq!(state.window.region(0, 13).unwrap(), data);
    }

    /// The pass-through bytes are folded into the check value, and this is the only place that
    /// happens for them.
    ///
    /// `wrap == 1` selects Adler-32 (`deflate.c` L228-L229). The bytes go from the caller's
    /// input buffer to the caller's output buffer without entering the window, so if
    /// `read_buf` were bypassed here the zlib trailer would be wrong and nothing else would
    /// notice.
    #[test]
    fn pass_through_bytes_update_the_check_value() {
        let data = pattern(4096);
        let mut state = level_zero();
        // `s->wrap` for a zlib container (`deflate.c` L423); the window bits are negative in
        // the fixture, so it is set directly rather than through the configuration.
        state.wrap = 1;
        let mut output = [0_u8; 4200];
        let mut cursors = new_cursors(&data, &mut output);
        // `adler32(0, Z_NULL, 0)` is 1 (`deflate.c` L664-L666).
        cursors.check = 1;

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(cursors.check, adler32_reference(&data));
    }

    /// An input larger than `MAX_STORED` is split into 65535-byte blocks plus a remainder,
    /// which is what makes the `do`/`while (last == 0)` loop at `deflate.c` L1751 run more
    /// than once.
    ///
    /// The final block, and only the final block, carries `BFINAL`.
    #[test]
    fn input_larger_than_max_stored_is_split_into_several_blocks() {
        let data = pattern(200_000);
        let mut state = level_zero();
        let mut output = vec![0_u8; 200_100];
        let mut cursors = new_cursors(&data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        let blocks = parse_stored_blocks(cursors.output.written_slice());
        assert_eq!(blocks.len(), 4, "three full blocks and a remainder");
        assert_eq!(
            blocks.iter().map(|b| b.len).collect::<Vec<_>>(),
            vec![MAX_STORED, MAX_STORED, MAX_STORED, 200_000 - 3 * MAX_STORED]
        );
        assert!(!blocks[0].last && !blocks[1].last && !blocks[2].last);
        assert!(blocks[3].last, "only the last block sets BFINAL");

        // Reassembling the payloads must give the input back.
        let mut round_trip = Vec::new();
        for block in &blocks {
            round_trip.extend_from_slice(block.payload);
        }
        assert_eq!(round_trip, data);

        // `deflate.c` L1764-L1768: more than a window went past, so the previous history is
        // supplanted by the final `w_size` bytes and the hash table is marked for a clear.
        let w_size = state.w_size();
        assert_eq!(state.matches, 2, "deflate.c L1765");
        assert_eq!(state.window.strstart, w_size);
        assert_eq!(state.window.insert, w_size);
        assert_eq!(state.window.block_start, 32768);
        assert_eq!(
            state.window.region(0, w_size).unwrap(),
            &data[200_000 - w_size..]
        );
    }

    /// `Z_NO_FLUSH` with an input shorter than `min_block` emits nothing and buffers the input
    /// in the window instead.
    ///
    /// This is the `flush == Z_NO_FLUSH` disjunct of the "don't emit" test at
    /// `deflate.c` L1704-L1707, followed by the window refill at `deflate.c` L1815-L1819 --
    /// the second `read_buf` call, whose destination is the window rather than the output.
    #[test]
    fn no_flush_below_min_block_writes_nothing_and_fills_the_window() {
        let data = b"hello, hello!";
        let mut state = level_zero();
        let mut output = [0_u8; 64];
        let mut cursors = new_cursors(data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::NoFlush);

        assert_eq!(bstate, BlockState::NeedMore);
        assert_eq!(cursors.output.written(), 0, "no block was emitted");
        assert_eq!(state.pending_bytes(), 0, "and none was written to pending");
        // The input was consumed into the window, not to the output.
        assert_eq!(cursors.avail_in(), 0);
        assert_eq!(cursors.total_in, 13);
        assert_eq!(cursors.total_out, 0);
        assert_eq!(state.window.strstart, 13);
        assert_eq!(state.window.insert, 13);
        assert_eq!(state.window.region(0, 13).unwrap(), data);
        // `block_start` is untouched, because `used` was zero: nothing was copied straight
        // through, so `deflate.c` L1784 did not run.
        assert_eq!(state.window.block_start, 0);
    }

    /// Window bytes buffered by an earlier `Z_NO_FLUSH` call are copied out of the window on
    /// the next call, and the result is the same stream as a single `Z_FINISH` call would
    /// produce.
    ///
    /// This is the only test that exercises the window-to-output copy at
    /// `deflate.c` L1730-L1740, including the `if (left > len) left = len;` clamp and the
    /// `len -= left` that follows it.
    #[test]
    fn buffered_window_bytes_are_flushed_from_the_window() {
        let data = b"hello, hello!";
        let mut state = level_zero();
        let mut first = [0_u8; 64];
        let mut cursors = new_cursors(data, &mut first);
        assert_eq!(
            deflate_stored(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );
        assert_eq!(cursors.output.written(), 0);
        assert_eq!(state.window.strstart, 13);
        assert_eq!(state.window.block_start, 0, "13 window bytes are unflushed");

        // Second call: no new input, so the block is drawn entirely from the window.
        let mut second = [0_u8; 64];
        let mut cursors = new_cursors(&[], &mut second);
        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(state.bi_used, 8);
        let emitted = cursors.output.written_slice();
        assert_eq!(emitted.len(), 18);
        assert_eq!(&emitted[..5], &[0x01, 0x0d, 0x00, 0xf2, 0xff]);
        let blocks = parse_stored_blocks(emitted);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].last);
        assert_eq!(blocks[0].payload, data);
        // `deflate.c` L1738: the copied window bytes are now the caller's.
        assert_eq!(state.window.block_start, 13);
        assert_eq!(cursors.total_out, 18);
    }

    // -------------------------------------------------------------------------
    //  The two breaks out of the loop -- deflate.c L1689 and L1704
    // -------------------------------------------------------------------------

    /// Too little output space for even a block header takes the `deflate.c` L1689 break, and
    /// the block is then written to `pending_buf` instead (`deflate.c` L1828-L1842).
    ///
    /// Only part of it reaches the caller, so the return is [`BlockState::FinishStarted`] --
    /// "finish started, need only more output at next deflate" -- and not
    /// [`BlockState::FinishDone`].
    #[test]
    fn output_too_small_for_a_header_writes_the_block_to_pending() {
        let data = b"hello, hello!";
        let mut state = level_zero();
        // Four bytes: one less than the five `header_bytes` reports for `bi_valid == 0`.
        let mut output = [0_u8; 4];
        let mut cursors = new_cursors(data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishStarted, "deflate.c L1847");
        assert_eq!(state.bi_used, 8, "deflate.c L1846");
        assert_eq!(cursors.output.written(), 4, "all four bytes were taken");
        // The header plus the 13-byte payload is 18 bytes; four of them were flushed.
        assert_eq!(state.pending_bytes(), 18 - 4);
        // The four bytes that did reach the caller are the start of a final stored block of
        // length 13.
        assert_eq!(cursors.output.written_slice(), &[0x01, 0x0d, 0x00, 0xf2]);
        // The input went through the window this time (`deflate.c` L1816), so `block_start`
        // has advanced over the block that was written to pending (`deflate.c` L1840).
        assert_eq!(state.window.strstart, 13);
        assert_eq!(state.window.block_start, 13);
        assert_eq!(state.window.region(0, 13).unwrap(), data);
    }

    /// A `Z_FINISH` whose block would be shorter than `min_block` *and* would leave input
    /// behind takes the `deflate.c` L1704 break through its `len != left + avail_in` disjunct.
    ///
    /// The output buffer is deliberately sized so that `len` is limited by `have` rather than
    /// by the input, which is what makes that disjunct true.
    #[test]
    fn a_short_output_limited_block_is_not_written_directly() {
        let data = pattern(2000);
        let mut state = level_zero();
        // Room for a header and 600 payload bytes: `len` becomes 600, well below the default
        // `min_block` of 32768, and 600 != 2000.
        let mut output = [0_u8; 605];
        let mut cursors = new_cursors(&data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        // Nothing was copied straight through; the whole input went into the window and a
        // block was written to pending instead.
        assert_eq!(bstate, BlockState::FinishStarted);
        assert_eq!(cursors.avail_in(), 0);
        assert_eq!(cursors.total_in, 2000);
        assert_eq!(state.window.strstart, 2000);
        // The block that was started is final and covers all 2000 bytes, even though only 605
        // of its 2005 bytes have been handed over.
        assert_eq!(state.pending_bytes(), 2005 - 605);
        let emitted = cursors.output.written_slice();
        assert_eq!(emitted.len(), 605, "the output buffer was filled");
        assert_eq!(emitted[0], 0x01, "BFINAL set, BTYPE stored");
        assert_eq!(u16::from_le_bytes([emitted[1], emitted[2]]), 2000);
        assert_eq!(u16::from_le_bytes([emitted[3], emitted[4]]), !2000_u16);
        assert_eq!(&emitted[5..], &data[..600]);
    }

    // -------------------------------------------------------------------------
    //  min_block, both formulas -- deflate.c L1673 and L1831
    // -------------------------------------------------------------------------

    /// `memLevel == 1` shrinks `min_block` from 32768 to 507, and that changes the emitted
    /// blocks for an input the default memory level would decline to copy directly.
    ///
    /// Same input, same output space, same flush mode as
    /// [`a_short_output_limited_block_is_not_written_directly`]; only `memLevel` differs. At
    /// the default level `min_block` is `MIN(65536 - 5, 32768)` = 32768 and the 600-byte block
    /// is refused; at `memLevel == 1` it is `MIN(512 - 5, 32768)` = 507 and the same block is
    /// written straight to the output.
    ///
    /// The second `min_block`, `MIN(have, w_size)` with `have = MIN(pending_buf_size -
    /// header, MAX_STORED)` (`deflate.c` L1828-L1831), is 507 as well here, and it is what
    /// caps the follow-up block written to pending at 507 rather than at the 1400 bytes
    /// available.
    #[test]
    fn min_block_is_507_at_memory_level_one() {
        let data = pattern(2000);
        let mut state = new_state(0, MIN_MEM_LEVEL);
        assert_eq!(state.pending_buf_size(), 512);
        let mut output = [0_u8; 605];
        let mut cursors = new_cursors(&data, &mut output);

        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        // 600 >= 507, so the block went straight out -- and it is not final, because it does
        // not cover all of the input (`deflate.c` L1712).
        assert_eq!(bstate, BlockState::NeedMore);
        let emitted = cursors.output.written_slice();
        assert_eq!(emitted.len(), 605);
        assert_eq!(emitted[0], 0x00, "BFINAL clear, BTYPE stored");
        assert_eq!(u16::from_le_bytes([emitted[1], emitted[2]]), 600);
        assert_eq!(u16::from_le_bytes([emitted[3], emitted[4]]), !600_u16);
        assert_eq!(&emitted[5..], &data[..600]);

        // The 600 copied bytes were appended to the window, and the remaining 1400 were read
        // into it as well (`deflate.c` L1780 and L1816).
        assert_eq!(cursors.avail_in(), 0);
        assert_eq!(state.window.strstart, 2000);
        assert_eq!(state.window.region(0, 2000).unwrap(), &data[..]);
        // The pending block is capped by the SECOND `min_block`/`have` pair at 507 bytes, out
        // of the 1400 that were available: 5 header bytes plus 507 of payload.
        assert_eq!(state.pending_bytes(), 512);
        assert_eq!(state.window.block_start, 600 + 507);
    }

    // -------------------------------------------------------------------------
    //  The two window slides -- deflate.c L1771-L1779 and L1802-L1812
    // -------------------------------------------------------------------------

    /// Copying input through when the window is nearly full slides the window down first
    /// (`deflate.c` L1771-L1779) and books one pending `slide_hash`.
    #[test]
    fn a_nearly_full_window_slides_down_before_the_append() {
        let history = pattern(40_000);
        let data = pattern(30_000);
        let mut state = level_zero();
        // A window holding 40000 bytes with nothing unflushed, which is the state a run of
        // `Z_NO_FLUSH` calls leaves behind.
        assert!(state.window.write_at(0, &history));
        state.window.strstart = 40_000;
        state.window.block_start = 40_000;
        state.window.insert = 40_000;

        let mut output = vec![0_u8; 30_100];
        let mut cursors = new_cursors(&data, &mut output);
        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishDone);
        let blocks = parse_stored_blocks(cursors.output.written_slice());
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].last);
        assert_eq!(blocks[0].payload, &data[..]);
        drop(blocks);

        // `window_size - strstart` was 25536, which is at most the 30000 bytes used, so the
        // upper half moved down by `w_size` = 32768.
        let w_size = state.w_size();
        assert_eq!(state.matches, 1, "deflate.c L1775-L1776");
        assert_eq!(state.window.strstart, 40_000 - w_size + 30_000);
        assert_eq!(
            state.window.block_start,
            isize::try_from(state.window.strstart).unwrap()
        );
        // `insert` was clamped to the post-slide `strstart` and then advanced, capped at
        // `w_size` (`deflate.c` L1777-L1778 and L1782).
        assert_eq!(state.window.insert, w_size);
        // The slid history is the tail of what was there, and the new bytes follow it.
        assert_eq!(
            state.window.region(0, 40_000 - w_size).unwrap(),
            &history[w_size..]
        );
        assert_eq!(
            state.window.region(40_000 - w_size, 30_000).unwrap(),
            &data[..]
        );
    }

    /// Refilling the window when there is more input than room slides it down as well, and
    /// this slide moves `block_start` too (`deflate.c` L1802-L1812).
    #[test]
    fn the_window_refill_slides_down_and_moves_block_start() {
        let history = pattern(50_000);
        let data = pattern(30_000);
        let mut state = level_zero();
        // 50000 bytes in the window with the last 10000 still unflushed, so `block_start` is
        // at 40000 and is at least `w_size`, which is the refill slide's guard.
        assert!(state.window.write_at(0, &history));
        state.window.strstart = 50_000;
        state.window.block_start = 40_000;
        state.window.insert = 50_000;

        // Four bytes of output, so the direct-copy loop breaks at `deflate.c` L1689 and the
        // refill below is reached with all 30000 input bytes still available.
        let mut output = [0_u8; 4];
        let mut cursors = new_cursors(&data, &mut output);
        let bstate = deflate_stored(&mut state, &mut cursors, Flush::NoFlush);

        assert_eq!(bstate, BlockState::NeedMore);
        let w_size = state.w_size();
        assert_eq!(state.matches, 1, "deflate.c L1807-L1808");
        // `have` was 15536, less than the 30000 available, and `block_start` was at least
        // `w_size`, so both cursors moved down by `w_size` before the refill.
        assert_eq!(state.window.strstart, 50_000 - w_size + 30_000);
        assert_eq!(cursors.avail_in(), 0, "all of it fitted after the slide");
        assert_eq!(cursors.total_in, 30_000);
        assert_eq!(
            state.window.region(0, 50_000 - w_size).unwrap(),
            &history[w_size..]
        );
        assert_eq!(
            state.window.region(50_000 - w_size, 30_000).unwrap(),
            &data[..]
        );
        // A worthy block accumulated, so it was written to pending and `block_start` advanced
        // over it (`deflate.c` L1833-L1840): 40000 - 32768 = 7232, plus 40000 bytes of block.
        assert_eq!(state.pending_bytes(), 5 + 40_000 - 4);
        assert_eq!(state.window.block_start, 7232 + 40_000);
    }

    // -------------------------------------------------------------------------
    //  block_done -- deflate.c L1795-L1798
    // -------------------------------------------------------------------------

    /// A flush that consumed everything and left nothing in the window reports
    /// [`BlockState::BlockDone`], so that `deflate()` goes on to emit its own flush marker.
    #[test]
    fn a_completed_flush_reports_block_done() {
        let data = pattern(600);
        let mut state = new_state(0, MIN_MEM_LEVEL);
        let mut output = vec![0_u8; 700];
        let mut cursors = new_cursors(&data, &mut output);

        // `Z_BLOCK`: neither `Z_NO_FLUSH` nor `Z_FINISH`, which are the two modes
        // `deflate.c` L1796 excludes.
        let bstate = deflate_stored(&mut state, &mut cursors, Flush::Block);

        assert_eq!(bstate, BlockState::BlockDone);
        // `bi_used` is whatever `bi_windup` left, `((bi_valid - 1) & 7) + 1` with the three
        // header bits in an empty bit buffer (`trees.c` L187), and NOT the 8 the two finish
        // paths force (`deflate.c` L1791 and L1846), neither of which this call takes.
        assert_eq!(state.bi_used, 3);
        let blocks = parse_stored_blocks(cursors.output.written_slice());
        assert_eq!(blocks.len(), 1);
        assert!(!blocks[0].last, "a flush marker is not a final block");
        assert_eq!(blocks[0].payload, &data[..]);
        drop(blocks);
        // All input consumed and nothing left in the window is what `block_done` asserts.
        assert_eq!(cursors.avail_in(), 0);
        assert_eq!(
            state.window.bytes_since_block_start(),
            Some(0),
            "deflate.c L1797"
        );
    }

    /// Every flush mode, every window state: the function always terminates, never panics, and
    /// returns a block state consistent with what it wrote.
    ///
    /// A coarse sweep rather than a proof, but it covers the combinations of flush mode, input
    /// size and output size that the differential matrix varies, and it is the cheapest guard
    /// against a clamp change that turns the `do`/`while` into an infinite loop.
    #[test]
    fn every_flush_and_buffer_size_terminates_and_stays_consistent() {
        const SIZES: [usize; 7] = [0, 1, 5, 507, 4096, 32_769, 70_000];
        const OUT_SIZES: [usize; 6] = [1, 4, 5, 605, 40_000, 80_000];
        for flush in [
            Flush::NoFlush,
            Flush::PartialFlush,
            Flush::SyncFlush,
            Flush::FullFlush,
            Flush::Finish,
            Flush::Block,
        ] {
            for &in_len in &SIZES {
                for &out_len in &OUT_SIZES {
                    let data = pattern(in_len);
                    let mut state = level_zero();
                    let mut output = vec![0_u8; out_len];
                    let mut cursors = new_cursors(&data, &mut output);

                    let bstate = deflate_stored(&mut state, &mut cursors, flush);

                    let written = cursors.output.written();
                    // With nothing left pending, every byte of every block that was started
                    // has reached the caller, so the output is a whole number of stored
                    // blocks and can be parsed -- which asserts a correct `NLEN` in every
                    // header. A non-empty pending buffer means the last block was only partly
                    // flushed, which is legitimate and is exactly what `finish_started` and
                    // `need_more` report.
                    if state.pending_bytes() == 0 {
                        let blocks = parse_stored_blocks(cursors.output.written_slice());
                        // A block only claims to be final for `Z_FINISH`.
                        for block in &blocks {
                            assert!(
                                !block.last || flush == Flush::Finish,
                                "BFINAL set for {flush:?}"
                            );
                        }
                    }
                    // The finish states are reachable only under `Z_FINISH`.
                    if matches!(bstate, BlockState::FinishDone | BlockState::FinishStarted) {
                        assert_eq!(flush, Flush::Finish, "finish state for {flush:?}");
                        assert_eq!(state.bi_used, 8, "deflate.c L1791 and L1846");
                    }
                    // Accounting is consistent: every byte is either still unread, in the
                    // window, or written out.
                    assert_eq!(
                        usize::try_from(cursors.total_in).unwrap(),
                        in_len - cursors.avail_in()
                    );
                    assert_eq!(usize::try_from(cursors.total_out).unwrap(), written);
                    assert!(written <= out_len);
                    assert!(state.window.strstart <= state.window_size());
                    assert!(state.window.insert <= state.w_size());
                    assert!(state.matches <= 2, "deflate.c L1660-L1662");
                }
            }
        }
    }
}
