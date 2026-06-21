//! `deflate_stored` — the level-0 / store-only DEFLATE strategy.
//!
//! This module is a faithful, **100% safe-Rust** port of the C
//! `deflate_stored()` function (`deflate.c` L1668-1848), the block-compression
//! routine selected for compression level 0 (`Z_NO_COMPRESSION`). It is the
//! most intricate of the five strategy functions because it does **no**
//! compression yet must still:
//!
//! * frame the input as RFC 1951 *stored* (uncompressed) blocks — a 3-bit
//!   block header followed by a byte-aligned 16-bit length and its one's
//!   complement — emitted bit-for-bit identically to C zlib (AAP §0.6.6,
//!   §0.7.1);
//! * minimize the number of times an input byte is copied, preferring a single
//!   `next_in -> next_out` copy for large buffers; and
//! * keep the sliding window and the hash-table bookkeeping (`strstart`,
//!   `block_start`, `insert`, `matches`, `high_water`) perfectly consistent so
//!   that a later [`deflateParams`]-style switch to a non-zero level still has
//!   correct history. In particular `s.state.matches` (otherwise unused while
//!   storing) tracks the number of pending hash-table slides: `1` means one
//!   slide is owed, `2` (the maximum) means the table must be cleared, since
//!   two or more slides are equivalent to a clear (`deflate.c` L1657-1662).
//!
//! Unlike the other four strategies (`fast`/`slow`/`rle`/`huff`), which tally
//! symbols and defer block emission to the `FLUSH_BLOCK` helpers in `trees.rs`,
//! `deflate_stored` performs its **own** flushing: it calls
//! [`trees::tr_stored_block`] directly and drives [`DeflateStream::flush_pending`]
//! itself.
//!
//! # Memory safety
//!
//! Every buffer movement here is a disjoint-field slice copy
//! ([`slice::copy_from_slice`]) or an in-place window slide
//! ([`slice::copy_within`]); there is **no** `unsafe` and **no** raw pointer
//! arithmetic (AAP §0.6.2). The module is `no_std`-clean: it relies only on
//! `core::` (`core::cmp::min`, `core::mem::take`) and the sibling deflate
//! modules — never on `std::` or a named `alloc::` item.
//!
//! [`deflateParams`]: https://www.zlib.net/manual.html#Advanced

use crate::constants::FlushMode;
use crate::deflate::state::{BlockState, DeflateStream, MAX_STORED};
use crate::deflate::trees;

/// Copy as many input bytes as possible directly to the output as DEFLATE
/// *stored* blocks (compression level 0). Port of C `deflate_stored()`
/// (`deflate.c` L1668-1848).
///
/// This is the [`CompressFn`]-shaped routine
/// (`fn(&mut DeflateStream, FlushMode) -> BlockState`) installed at index 0 of
/// the per-level `strategy::CONFIG_TABLE`. It returns the same
/// [`BlockState`] discriminants as C `block_state`:
///
/// * [`BlockState::FinishDone`] — the final block was written straight to the
///   output; nothing more to do.
/// * [`BlockState::BlockDone`] — a flush completed and all input was consumed.
/// * [`BlockState::FinishStarted`] — finishing has begun but the final block
///   is buffered in `pending` awaiting output room.
/// * [`BlockState::NeedMore`] — more input or output room is required.
///
/// The produced byte stream — block headers, block boundaries, last-block
/// flag, and the verbatim payload — is identical to C zlib for the same input,
/// flush mode, and window configuration.
///
/// [`CompressFn`]: crate::deflate::state::DeflateStream
pub(crate) fn deflate_stored(s: &mut DeflateStream, flush: FlushMode) -> BlockState {
    // Smallest worthy block size when neither flushing nor finishing. By
    // default this is 32K; it can be as small as 507 bytes for memLevel == 1,
    // and grows toward MAX_STORED for large input/output buffers
    // (`deflate.c` L1669-1673).
    let mut min_block = core::cmp::min(s.state.pending_buf_size as usize - 5, s.state.w_size);

    // Whether the last (final) block has been emitted to the output.
    let mut last = false;
    // Snapshot of `avail_in` on entry, used afterward to compute how many input
    // bytes were copied directly to the output (`deflate.c` L1681: `unsigned
    // used = s->strm->avail_in;`).
    let used_start = s.avail_in();

    // -- Direct-copy loop (C do/while, L1682-1751) ------------------------
    //
    // Copy `min_block`-or-larger stored blocks straight to the output while
    // there is room. When flushing, also copy whatever input remains as a
    // stored block if it fits. The C form is `do { ... } while (last == 0);`,
    // reproduced here as a `loop` whose body ends with `if last { break; }`.
    loop {
        // Maximum DEFLATE stored block length (a 16-bit length field).
        let mut len: usize = MAX_STORED;
        // Bytes needed in the output just for this block's header: the up-to-2
        // buffered bits plus the 3-bit type and 32-bit length/complement,
        // rounded up to whole bytes (`(bi_valid + 42) >> 3`, L1688).
        let mut have = ((s.state.bi_valid as usize) + 42) >> 3;
        if s.avail_out() < have {
            // No room even for the header — stop the direct-copy loop.
            break;
        }
        // Remaining output space available for the block body (L1692).
        have = s.avail_out() - have;
        // Bytes still sitting in the window for the current block. `block_start`
        // is signed (it may go negative after a slide), so the subtraction is
        // performed in `isize` before narrowing (L1693).
        let mut left = (s.state.strstart as isize - s.state.block_start) as usize;
        if len > left + s.avail_in() {
            // Limit the block to the data we actually have (L1694-1695).
            len = left + s.avail_in();
        }
        if len > have {
            // Limit the block to the output room available (L1696-1697).
            len = have;
        }

        // If the block would be smaller than `min_block`, and we are either not
        // flushing, or cannot copy all the available input, then it is not
        // worth writing here — fall through to the window/pending path. Also
        // never write an empty block while flushing; `deflate()` handles that
        // case itself (`deflate.c` L1699-1707).
        if len < min_block
            && ((len == 0 && flush != FlushMode::Finish)
                || flush == FlushMode::NoFlush
                || len != left + s.avail_in())
        {
            break;
        }

        // This block is the last one iff we are finishing and it consumes every
        // remaining byte of window + input (L1712).
        last = flush == FlushMode::Finish && len == left + s.avail_in();

        // Emit a dummy stored-block header (buf = None, len = 0) to lay down the
        // 3-bit type, byte-align, and reserve the 4 length bytes — also updating
        // the debug counts. `tr_stored_block` advances `pending` by 0 here
        // (L1709-1713).
        trees::tr_stored_block(s.state, None, 0, last);

        // Overwrite the dummy 0/~0 length words with the real `len`: the 16-bit
        // little-endian length followed by its one's complement
        // (`deflate.c` L1715-1719). `len <= MAX_STORED` so it fits in 16 bits;
        // the `& 0xff` masks mirror the C `(Bytef)` truncating casts.
        let p = s.state.pending;
        s.state.pending_buf[p - 4] = (len & 0xff) as u8;
        s.state.pending_buf[p - 3] = ((len >> 8) & 0xff) as u8;
        s.state.pending_buf[p - 2] = (!len & 0xff) as u8;
        s.state.pending_buf[p - 1] = ((!len >> 8) & 0xff) as u8;

        // Write the stored-block header bytes to the output (L1722).
        s.flush_pending();

        // Copy the uncompressed bytes still held in the window to the output
        // (`deflate.c` L1730-1740). `output` and `state` are disjoint fields of
        // `DeflateStream`, so the slice copy borrows them simultaneously and
        // safely.
        if left != 0 {
            if left > len {
                left = len;
            }
            let on = s.out_next;
            let bs = s.state.block_start as usize;
            s.output[on..on + left].copy_from_slice(&s.state.window[bs..bs + left]);
            s.out_next += left;
            s.state.total_out += left as u64;
            s.state.block_start += left as isize;
            len -= left;
        }

        // Copy the remaining uncompressed bytes directly from the input to the
        // output, updating the running checksum (`deflate.c` L1742-1750).
        // `read_buf_output` advances `in_next`/`out_next`/`total_in`/`adler` but
        // — exactly as in C — leaves `total_out` to the caller. At this point
        // `len <= avail_in`, so precisely `len` bytes are copied.
        if len != 0 {
            s.read_buf_output(len);
            s.state.total_out += len as u64;
        }

        if last {
            break;
        }
    }

    // -- Update the sliding window with the copied data (C L1759-1787) -----
    //
    // Append the last `w_size` bytes of the directly-copied data to the window
    // (or all of it, if fewer than `w_size` bytes were copied), and update the
    // count of bytes still to be inserted into the hash table should
    // `deflateParams` later switch to a non-zero level.
    let used = used_start - s.avail_in(); // input bytes copied directly out
    if used != 0 {
        // Any input that was used means no unused input remains in the window,
        // so `block_start == strstart` by the end of this block.
        if used >= s.state.w_size {
            // The copied data is at least a full window: supplant the previous
            // history wholesale and clear the hash (two-or-more slides == clear).
            s.state.matches = 2;
            let ws = s.state.w_size;
            let src = s.in_next - ws;
            s.state.window[0..ws].copy_from_slice(&s.input[src..src + ws]);
            s.state.strstart = ws;
            s.state.insert = s.state.strstart;
        } else {
            if s.state.window_size - s.state.strstart <= used {
                // Not enough tail room: slide the window down by `w_size` and
                // record one more pending hash slide.
                s.state.strstart -= s.state.w_size;
                let ws = s.state.w_size;
                let ss = s.state.strstart;
                s.state.window.copy_within(ws..ws + ss, 0);
                if s.state.matches < 2 {
                    s.state.matches += 1;
                }
                if s.state.insert > s.state.strstart {
                    s.state.insert = s.state.strstart;
                }
            }
            // Append the `used` freshly-copied input bytes to the window.
            let src = s.in_next - used;
            let ss = s.state.strstart;
            s.state.window[ss..ss + used].copy_from_slice(&s.input[src..src + used]);
            s.state.strstart += used;
            s.state.insert += core::cmp::min(used, s.state.w_size - s.state.insert);
        }
        s.state.block_start = s.state.strstart as isize;
    }
    if s.state.high_water < s.state.strstart {
        s.state.high_water = s.state.strstart;
    }

    // -- Early returns (C L1789-1798) -------------------------------------

    // The final block was written straight to the output: we are done.
    if last {
        s.state.bi_used = 8;
        return BlockState::FinishDone;
    }
    // Flushing (but not finishing) and all input consumed with nothing buffered
    // in the window: the flush is complete.
    if flush != FlushMode::NoFlush
        && flush != FlushMode::Finish
        && s.avail_in() == 0
        && (s.state.strstart as isize) == s.state.block_start
    {
        return BlockState::BlockDone;
    }

    // -- Fill the window with any remaining input (C L1800-1821) ----------
    let mut have = s.state.window_size - s.state.strstart;
    if s.avail_in() > have && s.state.block_start >= s.state.w_size as isize {
        // Slide the window down to make room, recording a pending hash slide.
        s.state.block_start -= s.state.w_size as isize;
        s.state.strstart -= s.state.w_size;
        let ws = s.state.w_size;
        let ss = s.state.strstart;
        s.state.window.copy_within(ws..ws + ss, 0);
        if s.state.matches < 2 {
            s.state.matches += 1;
        }
        have += s.state.w_size;
        if s.state.insert > s.state.strstart {
            s.state.insert = s.state.strstart;
        }
    }
    if have > s.avail_in() {
        have = s.avail_in();
    }
    if have != 0 {
        // Read fresh input into `window[strstart..]`, updating the checksum.
        s.read_buf_window(s.state.strstart, have);
        s.state.strstart += have;
        s.state.insert += core::cmp::min(have, s.state.w_size - s.state.insert);
    }
    if s.state.high_water < s.state.strstart {
        s.state.high_water = s.state.strstart;
    }

    // -- Stored block to the pending buffer (C L1823-1842) ----------------
    //
    // There was not enough output room to write a complete worthy/flushed block
    // directly. Instead, write a stored block into `pending` if we now hold a
    // worthy block, or if flushing and the remaining input fits as a stored
    // block in the pending buffer.
    let mut have = ((s.state.bi_valid as usize) + 42) >> 3; // header bytes
    have = core::cmp::min(s.state.pending_buf_size as usize - have, MAX_STORED);
    min_block = core::cmp::min(have, s.state.w_size);
    let left = (s.state.strstart as isize - s.state.block_start) as usize;
    if left >= min_block
        || ((left != 0 || flush == FlushMode::Finish)
            && flush != FlushMode::NoFlush
            && s.avail_in() == 0
            && left <= have)
    {
        let len = core::cmp::min(left, have);
        last = flush == FlushMode::Finish && s.avail_in() == 0 && len == left;

        // C passes `s->window + s->block_start` as the source buffer, which
        // aliases the state. Since `tr_stored_block` copies that slice into
        // `pending_buf` and never reads `s.window`, temporarily move the window
        // out (an allocation-free `mem::take` that leaves an empty boxed slice), hand
        // the borrow checker a disjoint slice, then put the window back. This
        // is behavior-identical to the C aliasing copy.
        let bs = s.state.block_start as usize;
        let window = core::mem::take(&mut s.state.window);
        trees::tr_stored_block(s.state, Some(&window[bs..bs + len]), len, last);
        s.state.window = window;

        s.state.block_start += len as isize;
        s.flush_pending();
    }

    // -- Final return (C L1845-1847) --------------------------------------
    if last {
        s.state.bi_used = 8;
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}
