//! `deflate_stored` -- store the input uncompressed, copying it as few times as possible.
//!
//! A verbatim port of `deflate_stored` (`deflate.c` L1668-L1846), the compressor
//! `configuration_table` names for level 0 (`deflate.c` L113). It is the only one of the five
//! that never consults a Huffman tree and the only one that can hand bytes straight from the
//! caller's input buffer to the caller's output buffer, bypassing the window entirely: per the
//! comment at L1664-L1667 it "is written to minimize the number of times an input byte is copied"
//! and "is most efficient with large input and output buffers, which maximizes the opportunities
//! to have a single copy from `next_in` to `next_out`".
//!
//! # Three stages, in this order
//!
//! 1. **Direct copies** (L1681-L1750). A `do {} while (last == 0)` loop that, for as long as
//!    there is room for a header plus a worthwhile payload, emits a stored-block header into the
//!    pending buffer and then copies the payload directly to `next_out` -- first whatever is
//!    still unflushed in the window, then the rest straight from `next_in`.
//! 2. **Window catch-up** (L1752-L1821). Whatever was copied through must still end up in the
//!    window, because `deflateParams` may switch to a non-zero level at any moment and the
//!    history has to be there when it does. Then any input that is still unconsumed is read into
//!    the window for next time.
//! 3. **A stored block into pending** (L1823-L1841). When stage 1 could not write a whole block
//!    to `next_out`, a block is written to the pending buffer instead, which the driver will
//!    drain on a later call.
//!
//! # What `matches` means here, and why it is not compression state
//!
//! `s->matches` is unused while storing, so this function borrows it as a counter of *pending
//! hash-table slides* (L1657-L1662): one slide is recorded as 1, and 2 means "clear the table",
//! since two or more slides are equivalent to a clear. `deflateParams` reads it when the level
//! changes (`deflate.c` L800-L806) and either calls `slide_hash` once or clears the chains. Get
//! the counter wrong and a subsequent level switch searches stale chains, which changes the
//! matches found and therefore the emitted bytes.
//!
//! # Details that are load-bearing
//!
//! * **The header size estimate** `have = (s->bi_valid + 42) >> 3` (L1688 and L1823). Three bits
//!   of block type, up to seven bits already sitting in `bi_buf`, the padding to a byte boundary
//!   and the four bytes of `LEN`/`NLEN` -- rounded up by the `+ 42`. It is recomputed in stage 3
//!   because stage 1 may have changed `bi_valid`.
//! * **The dummy block, then the patch** (L1709-L1717). C emits `_tr_stored_block(s, NULL, 0, last)`
//!   purely to get correctly aligned header bytes into the pending buffer, and *then* overwrites
//!   the four zero length bytes with the real length. Writing the length up front is not an
//!   option: the payload is not in the pending buffer at all, so there is nothing for
//!   `_tr_stored_block` to copy.
//! * **`min_block`** (L1669-L1673): `MIN(pending_buf_size - 5, w_size)`, "the smallest worthy
//!   block size when not flushing or finishing", 32 KiB by default and as small as 507 bytes at
//!   `memLevel == 1`.
//! * **`last` is decided from `len == left + avail_in`** (L1707 and L1836), that is, from whether
//!   this block will consume everything there is. Only then may the `BFINAL` bit be set.
//! * **`bi_used = 8`** whenever a final block is written (L1785 and L1843). This tree carries
//!   `deflateUsed` (`zlib.h` L804), which reports how many bits of the last byte are in use; a
//!   stored block always ends byte-aligned, so all eight are.
//!
//! # Safety posture
//!
//! No `unsafe`, no raw pointer and no unchecked index, despite this being the function with the
//! most pointer arithmetic in `deflate.c`. Reading *behind* `next_in` -- C's
//! `s->strm->next_in - used`, which walks off the front of the caller's buffer whenever fewer
//! than `used` bytes have been read -- becomes
//! [`crate::read_buf::InputCursor::consumed_tail`], which reports [`None`] for exactly that case.
//! Every window access goes through a checked [`crate::weak_slice::Window`] projection and every
//! arithmetic step saturates.

use crate::deflate::algorithm::{BlockState, Flush, StreamCursors, MAX_STORED};
use crate::deflate::pending::flush_pending;
use crate::deflate::state::{Allocator, DeflateState};
use crate::read_buf::{read_buf, read_buf_into_output};
use crate::trees::{_tr_stored_block, flush_bits};

/// Bytes of stored-block header, given the bits already held in `bi_buf`.
///
/// `have = ((unsigned)s->bi_valid + 42) >> 3` (`deflate.c` L1688 and L1823). Three bits of block
/// type plus `bi_valid` pending bits, padded to a byte boundary, plus the four bytes of
/// `LEN`/`NLEN`: `(3 + 7 + 32) == 42`, and `>> 3` is the division that rounds that up to whole
/// bytes.
///
/// `bi_valid` is at most 15 (`trees.c` keeps `bi_buf` a `ush`), so the result is at most 7 and
/// the conversion cannot fail; a negative value is impossible for the same reason and saturates
/// to zero rather than wrapping.
#[inline]
fn header_bytes(bi_valid: i32) -> usize {
    let bits = bi_valid.saturating_add(42).max(0);
    usize::try_from(bits >> 3).unwrap_or(0)
}

/// Copies the input uncompressed, in stored blocks.
///
/// Port of `deflate_stored` (`deflate.c` L1668-L1846).
///
/// # Returns
///
/// * [`BlockState::FinishDone`] -- the final block was written straight to the caller's output
///   buffer in stage 1.
/// * [`BlockState::FinishStarted`] -- the final block was written to the pending buffer in stage
///   3 and still has to be drained.
/// * [`BlockState::BlockDone`] -- flushing, and everything available has been consumed.
/// * [`BlockState::NeedMore`] -- everything that could be done with the available input and
///   output has been done.
// `_tr_stored_block` keeps the underscore-prefixed C spelling of `deflate.h` L311-L312; see
// `trees/mod.rs` for the same relaxation.
#[allow(clippy::used_underscore_items)]
pub(crate) fn deflate_stored<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    let w_size = state.w_size();
    let window_size = state.window_size();
    let wrap = state.wrap();

    // "Smallest worthy block size when not flushing or finishing. By default this is 32K. This
    // can be as small as 507 bytes for memLevel == 1. For large input and output buffers, the
    // stored block size will be larger."  (L1669-L1674)
    //
    // `unsigned min_block = (unsigned)(MIN(s->pending_buf_size - 5, s->w_size));`
    let mut min_block = state.pending_buf_size().saturating_sub(5).min(w_size);

    // `int last = 0;` and `unsigned used = s->strm->avail_in;`  (L1680 and L1682)
    let mut last = false;
    let entry_avail_in = cursors.avail_in();

    // -------------------------------------------------------------------------
    //  Stage 1 -- copy whole stored blocks straight to next_out  (L1683-L1750)
    // -------------------------------------------------------------------------
    //
    // `do { ... } while (last == 0);`
    loop {
        // "Set len to the maximum size block that we can copy directly with the available input
        // data and output space. Set left to how much of that would be copied from what's left
        // in the window."  (L1684-L1687)
        //
        // `len = MAX_STORED;`  (L1688)
        let mut len = MAX_STORED;

        // `have = ((unsigned)s->bi_valid + 42) >> 3;`  and
        // `if (s->strm->avail_out < have) break;`  -- "need room for header"  (L1689-L1691)
        let header = header_bytes(state.bi_valid);
        if cursors.avail_out() < header {
            break;
        }

        // "maximum stored block length that will fit in avail_out"
        // `have = s->strm->avail_out - have;`  (L1692-L1693)
        let have = cursors.avail_out().saturating_sub(header);

        // `left = (unsigned)(s->strstart - s->block_start);` -- "window bytes"  (L1694)
        //
        // `None` means `block_start` is negative or ahead of `strstart`, which this compressor
        // cannot produce: it only ever assigns `block_start = strstart` or advances it by bytes
        // it has just flushed, and `deflateParams` refuses to switch to level 0 unless
        // `(strstart - block_start) + lookahead` is zero (`deflate.c` L797-L799). Zero is the
        // safe reading -- it stores nothing from the window rather than an unbounded run.
        let mut left = state.window.bytes_since_block_start().unwrap_or(0);

        // ```text
        // if (len > (ulg)left + s->strm->avail_in)
        //     len = left + s->strm->avail_in;     /* limit len to the input */
        // if (len > have)
        //     len = have;                         /* limit len to the output */
        // ```
        // -- L1695-L1699. C widens to `ulg` for the comparison to keep the sum from wrapping;
        // `saturating_add` is that widening.
        let available = left.saturating_add(cursors.avail_in());
        if len > available {
            len = available;
        }
        if len > have {
            len = have;
        }

        // "If the stored block would be less than min_block in length, or if unable to copy all
        // of the available input when flushing, then try copying to the window and the pending
        // buffer instead. Also don't write an empty block when flushing -- deflate() does that."
        // (L1701-L1709)
        //
        // ```text
        // if (len < min_block && ((len == 0 && flush != Z_FINISH) ||
        //                         flush == Z_NO_FLUSH ||
        //                         len != left + s->strm->avail_in))
        //     break;
        // ```
        if len < min_block
            && ((len == 0 && !matches!(flush, Flush::Finish))
                || matches!(flush, Flush::NoFlush)
                || len != available)
        {
            break;
        }

        // "Make a dummy stored block in pending to get the header bytes, including any pending
        // bits. This also updates the debugging counts."  (L1711-L1715)
        //
        // `last = flush == Z_FINISH && len == left + s->strm->avail_in ? 1 : 0;`
        // `_tr_stored_block(s, (char *)0, 0L, last);`
        last = matches!(flush, Flush::Finish) && len == available;
        _tr_stored_block(state, None, 0, last);

        // "Replace the lengths in the dummy stored block with len."  (L1717-L1721)
        //
        // ```text
        // s->pending_buf[s->pending - 4] = (Bytef)len;
        // s->pending_buf[s->pending - 3] = (Bytef)(len >> 8);
        // s->pending_buf[s->pending - 2] = (Bytef)~len;
        // s->pending_buf[s->pending - 1] = (Bytef)(~len >> 8);
        // ```
        //
        // `len` is at most `MAX_STORED`, so it fits 16 bits and the four bytes are the low and
        // high halves of `len` followed by their complements: for any `len <= 0xffff`,
        // `(Bytef)~len` is `!(len as u8)` and `(Bytef)(~len >> 8)` is `!((len >> 8) as u8)`.
        //
        // `patch_tail` reports `false` only if fewer than four bytes are pending, which cannot
        // happen: `_tr_stored_block` has just written `LEN` and `NLEN`. Declining the patch
        // would leave a zero-length block, which the `debug_assert!` reports rather than
        // silently emitting.
        let len16 = u16::try_from(len).unwrap_or(u16::MAX);
        #[allow(clippy::cast_possible_truncation)]
        let lengths = [
            len16 as u8,
            (len16 >> 8) as u8,
            !(len16 as u8),
            !((len16 >> 8) as u8),
        ];
        let patched = state.pending.patch_tail(4, &lengths);
        debug_assert!(
            patched,
            "deflate_stored could not patch LEN/NLEN (deflate.c L1717-L1721)"
        );

        // "Write the stored block header bytes."  `flush_pending(s->strm);`  (L1723-L1724)
        let _header_flushed = flush_pending(
            state,
            &mut cursors.output,
            &mut cursors.total_out,
            flush_bits,
        );

        // The `#ifdef ZLIB_DEBUG` counters at L1726-L1730 have no counterpart.

        // "Copy uncompressed bytes from the window to next_out."  (L1732-L1742)
        //
        // ```text
        // if (left) {
        //     if (left > len) left = len;
        //     zmemcpy(s->strm->next_out, s->window + s->block_start, left);
        //     s->strm->next_out += left; s->strm->avail_out -= left;
        //     s->strm->total_out += left; s->block_start += left; len -= left;
        // }
        // ```
        if left != 0 {
            if left > len {
                left = len;
            }

            // One checked projection of the window in place of the pointer, and one
            // cursor-advancing copy in place of the three manual updates. A short copy is
            // impossible -- `left` was clamped to `len`, which was clamped to `have`, which is
            // the output room left after the header -- and `push_slice` reports what it took, so
            // the accounting below cannot drift from the copy.
            let copied = match state.window.block_region(left) {
                Some(bytes) => cursors.output.push_slice(bytes),
                // Unreachable: `bytes_since_block_start` already proved this range is inside
                // the window. Copying nothing keeps the accounting consistent.
                None => 0,
            };
            debug_assert_eq!(
                copied, left,
                "deflate_stored short window copy (deflate.c L1734-L1739)"
            );

            cursors.total_out = cursors.total_out.saturating_add(copied as u64);
            state.window.block_start = state
                .window
                .block_start
                .saturating_add(isize::try_from(copied).unwrap_or(isize::MAX));
            len = len.saturating_sub(copied);
        }

        // "Copy uncompressed bytes directly from next_in to next_out, updating the check
        // value."  (L1744-L1750)
        //
        // ```text
        // if (len) {
        //     read_buf(s->strm, s->strm->next_out, len);
        //     s->strm->next_out += len; s->strm->avail_out -= len;
        //     s->strm->total_out += len;
        // }
        // ```
        //
        // `read_buf_into_output` is exactly that four-statement group: it reads through
        // `read_buf`, so the running Adler-32 or CRC-32 and `total_in` are updated where C
        // updates them, and it advances the output cursor and `total_out` itself.
        if len != 0 {
            let _copied = read_buf_into_output(
                &mut cursors.input,
                &mut cursors.output,
                len,
                wrap,
                &mut cursors.check,
                &mut cursors.total_in,
                &mut cursors.total_out,
            );
        }

        // `} while (last == 0);`  (L1751)
        if last {
            break;
        }
    }

    // -------------------------------------------------------------------------
    //  Stage 2 -- bring the window up to date  (L1753-L1821)
    // -------------------------------------------------------------------------
    //
    // "Update the sliding window with the last s->w_size bytes of the copied data, or append all
    // of the copied data to the existing window if less than s->w_size bytes were copied. Also
    // update the number of bytes to insert in the hash tables, in the event that deflateParams()
    // switches to a non-zero compression level."  (L1753-L1758)
    //
    // `used -= s->strm->avail_in;` -- "number of input bytes directly copied"  (L1759)
    let used = entry_avail_in.saturating_sub(cursors.avail_in());

    if used != 0 {
        // "If any input was used, then no unused input remains in the window, therefore
        // s->block_start == s->strstart."  (L1761-L1763)
        if used >= w_size {
            // "supplant the previous history"  (L1764-L1769)
            //
            // ```text
            // s->matches = 2;         /* clear hash */
            // zmemcpy(s->window, s->strm->next_in - s->w_size, s->w_size);
            // s->strstart = s->w_size;
            // s->insert = s->strstart;
            // ```
            state.matches = 2;

            // Reading behind the input cursor: legitimate, because those bytes are still in the
            // caller's buffer and the caller may not touch it until the call returns.
            // `consumed_tail` reports [`None`] where C's pointer arithmetic would walk off the
            // front of the buffer, which `used >= w_size` already rules out.
            if let Some(history) = cursors.input.consumed_tail(w_size) {
                let written = state.window.write_at(0, history);
                debug_assert!(
                    written,
                    "deflate_stored could not supplant history (deflate.c L1766)"
                );
            }

            state.window.strstart = w_size;
            state.window.insert = state.window.strstart;
        } else {
            // `if (s->window_size - s->strstart <= used) { ... }`  (L1771-L1779)
            if window_size.saturating_sub(state.window.strstart) <= used {
                // "Slide the window down."
                //
                // ```text
                // s->strstart -= s->w_size;
                // zmemcpy(s->window, s->window + s->w_size, s->strstart);
                // if (s->matches < 2) s->matches++;   /* add a pending slide_hash() */
                // if (s->insert > s->strstart) s->insert = s->strstart;
                // ```
                //
                // The `zmemcpy` count is the *new* `strstart`, so the subtraction comes first;
                // `slide_down` deliberately leaves the cursors alone because the three C sites
                // that slide adjust different subsets of them.
                state.window.strstart = state.window.strstart.saturating_sub(w_size);
                let slid = state.window.slide_down(state.window.strstart);
                debug_assert!(slid, "deflate_stored slide failed (deflate.c L1773-L1774)");

                if state.matches < 2 {
                    state.matches = state.matches.saturating_add(1);
                }
                state.window.clamp_insert_to_strstart();
            }

            // ```text
            // zmemcpy(s->window + s->strstart, s->strm->next_in - used, used);
            // s->strstart += used;
            // s->insert += MIN(used, s->w_size - s->insert);
            // ```
            // -- L1780-L1782
            if let Some(tail) = cursors.input.consumed_tail(used) {
                let written = state.window.write_at(state.window.strstart, tail);
                debug_assert!(
                    written,
                    "deflate_stored could not append copied input (deflate.c L1780)"
                );
            }

            state.window.strstart = state.window.strstart.saturating_add(used);
            let room = w_size.saturating_sub(state.window.insert);
            state.window.insert = state.window.insert.saturating_add(used.min(room));
        }

        // `s->block_start = s->strstart;`  (L1784)
        state.window.block_start = isize::try_from(state.window.strstart).unwrap_or(isize::MAX);
    }

    // `if (s->high_water < s->strstart) s->high_water = s->strstart;`  (L1786-L1787)
    state.window.raise_high_water_to_strstart();

    // "If the last block was written to next_out, then done."  (L1789-L1793)
    //
    // `if (last) { s->bi_used = 8; return finish_done; }`
    if last {
        state.bi_used = 8;
        return BlockState::FinishDone;
    }

    // "If flushing and all input has been consumed, then done."  (L1795-L1799)
    //
    // ```text
    // if (flush != Z_NO_FLUSH && flush != Z_FINISH &&
    //     s->strm->avail_in == 0 && (long)s->strstart == s->block_start)
    //     return block_done;
    // ```
    //
    // `bytes_since_block_start() == Some(0)` is `(long)s->strstart == s->block_start`: it is
    // `Some(0)` exactly when `block_start` is non-negative and equal to `strstart`, and C's
    // signed comparison is false for a negative `block_start` too.
    if !matches!(flush, Flush::NoFlush | Flush::Finish)
        && cursors.avail_in() == 0
        && state.window.bytes_since_block_start() == Some(0)
    {
        return BlockState::BlockDone;
    }

    // "Fill the window with any remaining input."  (L1801-L1821)
    //
    // `have = (unsigned)(s->window_size - s->strstart);`  (L1802)
    let mut have = window_size.saturating_sub(state.window.strstart);

    // ```text
    // if (s->strm->avail_in > have && s->block_start >= (long)s->w_size) {
    //     /* Slide the window down. */
    //     s->block_start -= s->w_size;
    //     s->strstart -= s->w_size;
    //     zmemcpy(s->window, s->window + s->w_size, s->strstart);
    //     if (s->matches < 2) s->matches++;   /* add a pending slide_hash() */
    //     have += s->w_size;                  /* more space now */
    //     if (s->insert > s->strstart) s->insert = s->strstart;
    // }
    // ```
    // -- L1803-L1812
    let w_size_signed = isize::try_from(w_size).unwrap_or(isize::MAX);
    if cursors.avail_in() > have && state.window.block_start >= w_size_signed {
        state.window.block_start = state.window.block_start.saturating_sub(w_size_signed);
        state.window.strstart = state.window.strstart.saturating_sub(w_size);
        let slid = state.window.slide_down(state.window.strstart);
        debug_assert!(slid, "deflate_stored slide failed (deflate.c L1806-L1807)");

        if state.matches < 2 {
            state.matches = state.matches.saturating_add(1);
        }
        have = have.saturating_add(w_size);
        state.window.clamp_insert_to_strstart();
    }

    // ```text
    // if (have > s->strm->avail_in) have = s->strm->avail_in;
    // if (have) {
    //     read_buf(s->strm, s->window + s->strstart, have);
    //     s->strstart += have;
    //     s->insert += MIN(have, s->w_size - s->insert);
    // }
    // ```
    // -- L1813-L1819
    if have > cursors.avail_in() {
        have = cursors.avail_in();
    }
    if have != 0 {
        // C's destination is `s->window + s->strstart` with room for `have` bytes; the checked
        // projection is the same range, and because a slice carries its own length it *is* the
        // `size` argument C passes separately. `deflate_stored` runs only at level 0, where
        // `lookahead` is always zero -- `deflateParams` refuses a level change while the window
        // holds anything (`deflate.c` L797-L799) -- so this range is also exactly the window's
        // free space.
        let start = state.window.strstart;
        let read = match state.window.region_mut(start, have) {
            Some(dest) => read_buf(
                &mut cursors.input,
                dest,
                wrap,
                &mut cursors.check,
                &mut cursors.total_in,
            ),
            // Unreachable: `have` was derived from `window_size - strstart` and then only
            // reduced. Reading nothing leaves the stream consistent.
            None => 0,
        };
        debug_assert_eq!(
            read, have,
            "deflate_stored short window fill (deflate.c L1816)"
        );

        state.window.strstart = state.window.strstart.saturating_add(read);
        let room = w_size.saturating_sub(state.window.insert);
        state.window.insert = state.window.insert.saturating_add(read.min(room));
    }

    // `if (s->high_water < s->strstart) s->high_water = s->strstart;`  (L1820-L1821)
    state.window.raise_high_water_to_strstart();

    // -------------------------------------------------------------------------
    //  Stage 3 -- a stored block into the pending buffer  (L1823-L1841)
    // -------------------------------------------------------------------------
    //
    // "There was not enough avail_out to write a complete worthy or flushed stored block to
    // next_out. Write a stored block to pending instead, if we have enough input for a worthy
    // block, or if flushing and there is enough room for the remaining input as a stored block
    // in the pending buffer."  (L1823-L1828)
    //
    // ```text
    // have = ((unsigned)s->bi_valid + 42) >> 3;   /* bytes in header */
    //     /* maximum stored block length that will fit in pending: */
    // have = (unsigned)MIN(s->pending_buf_size - have, MAX_STORED);
    // min_block = MIN(have, s->w_size);
    // left = (unsigned)(s->strstart - s->block_start);
    // ```
    // -- L1829-L1833. `bi_valid` is recomputed because stage 1 may have changed it.
    let header = header_bytes(state.bi_valid);
    let have = state
        .pending_buf_size()
        .saturating_sub(header)
        .min(MAX_STORED);
    min_block = have.min(w_size);
    let left = state.window.bytes_since_block_start().unwrap_or(0);

    // ```text
    // if (left >= min_block ||
    //     ((left || flush == Z_FINISH) && flush != Z_NO_FLUSH &&
    //      s->strm->avail_in == 0 && left <= have)) {
    // ```
    // -- L1834-L1836
    let worthy = left >= min_block;
    let flushing_the_remainder = (left != 0 || matches!(flush, Flush::Finish))
        && !matches!(flush, Flush::NoFlush)
        && cursors.avail_in() == 0
        && left <= have;

    if worthy || flushing_the_remainder {
        // ```text
        // len = MIN(left, have);
        // last = flush == Z_FINISH && s->strm->avail_in == 0 && len == left ? 1 : 0;
        // _tr_stored_block(s, (charf *)s->window + s->block_start, len, last);
        // s->block_start += len;
        // flush_pending(s->strm);
        // ```
        // -- L1837-L1841
        let len = left.min(have);
        last = matches!(flush, Flush::Finish) && cursors.avail_in() == 0 && len == left;

        // `(charf *)s->window + s->block_start` as a window offset. `block_start` is
        // non-negative here -- `left` came back as `Some` from `bytes_since_block_start`, which
        // requires it -- and `_tr_stored_block` asserts the offset is the block start, which is
        // exactly what is passed.
        let block_start = usize::try_from(state.window.block_start).ok();
        _tr_stored_block(state, block_start, len as u64, last);

        state.window.block_start = state
            .window
            .block_start
            .saturating_add(isize::try_from(len).unwrap_or(isize::MAX));

        let _flushed = flush_pending(
            state,
            &mut cursors.output,
            &mut cursors.total_out,
            flush_bits,
        );
    }

    // "We've done all we can with the available input and output."  (L1843-L1845)
    //
    // `if (last) s->bi_used = 8;` and `return last ? finish_started : need_more;`
    if last {
        state.bi_used = 8;
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}
