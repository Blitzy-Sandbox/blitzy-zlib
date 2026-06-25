//! `deflate_stored` — the level-0 (no compression) DEFLATE strategy.
//!
//! This module is the safe-Rust translation of the C `deflate_stored` routine
//! (`deflate.c` lines 1668-1848). It is the compression "strategy" selected when
//! the level is `0`: instead of running the LZ77 match search and emitting
//! Huffman-coded symbols, it copies the input verbatim into a sequence of
//! **stored** (type `00`, uncompressed) DEFLATE blocks (RFC 1951 §3.2.4).
//!
//! # Why this strategy is special
//!
//! `deflate_stored` is the only strategy that writes **directly to the caller's
//! output buffer**, bypassing the symbol buffer entirely. Whenever the available
//! input and output are large enough, it emits a 5-byte stored-block header into
//! the pending buffer and then copies the payload straight from the window (and
//! from `next_in`) into `next_out` with a single `copy_from_slice` each — exactly
//! mirroring the C code's goal of minimizing the number of times an input byte is
//! copied. The flip side is that its slice bookkeeping is the most intricate of
//! all the strategies: every `have` / `left` / `used` / `len` accounting step from
//! the C reference is reproduced verbatim, because a single off-by-one in the
//! header-byte accounting would desynchronize the stored-block `LEN`/`NLEN`
//! fields and diverge from C zlib's byte stream immediately.
//!
//! # Memory-safety model
//!
//! The C reference performs raw `memcpy` and pointer arithmetic against
//! `s->strm->next_out` / `next_in` and `s->window`. Here every transfer is a
//! bounds-checked slice copy routed through the engine I/O context
//! ([`DeflateContext`](super::DeflateContext)); there is **no `unsafe`** and the
//! module compiles under the crate-wide `#![forbid(unsafe_code)]`. Only `core`
//! and `alloc` are used, so the crate's `no_std` build path is preserved.
//!
//! # Stored-block wire format (RFC 1951 §3.2.4)
//!
//! Each stored block is:
//!
//! 1. a 3-bit block header — `BFINAL` (1 bit) followed by `BTYPE = 00` (2 bits),
//! 2. padding to the next byte boundary,
//! 3. `LEN` — the block length as a little-endian `u16`,
//! 4. `NLEN` — the one's-complement of `LEN` as a little-endian `u16`,
//! 5. the `LEN` raw data bytes.
//!
//! The header + `LEN` + `NLEN` are produced by
//! [`DeflateState::tr_stored_block`](super::state::DeflateState::tr_stored_block);
//! the direct-copy path emits a *dummy* zero-length block to lay down the header
//! bytes and then patches the `LEN`/`NLEN` fields in place before copying the
//! payload, precisely as the C code does.

use super::DeflateContext;
use super::state::BlockState;
use crate::constants::Flush;

/// Maximum stored-block payload length in the DEFLATE format (C `MAX_STORED`).
///
/// A stored block's `LEN` field is a 16-bit value, so a single block can carry at
/// most `65535` payload bytes; larger inputs are split across multiple blocks.
const MAX_STORED: usize = 65535;

/// Copy uncompressed data as one or more stored DEFLATE blocks (C `deflate_stored`).
///
/// This is the per-block producer selected for compression level `0`. It is a
/// direct, behavior-preserving port of `deflate.c` lines 1668-1848: the two
/// phases — the direct-to-`next_out` fast path and the window-fill / pending
/// buffered-block path — and every numeric comparison are reproduced exactly so
/// that the emitted byte stream is identical to C zlib for every `windowBits`
/// and flush mode.
///
/// # Parameters
///
/// * `cx` — the engine I/O context bundling the owned [`DeflateState`], the
///   borrowed input/output slices with their cursors, and the running
///   `total_in` / `total_out` / checksum scalars
///   ([`DeflateContext`](super::DeflateContext), design decision D1).
/// * `flush` — the flush mode requested by the current `deflate` call.
///
/// # Returns
///
/// The [`BlockState`] describing how far this invocation progressed:
///
/// * [`BlockState::FinishDone`] — the final block was written to `next_out`.
/// * [`BlockState::FinishStarted`] — finishing has begun; only more output room
///   is needed on the next call.
/// * [`BlockState::BlockDone`] — a flush boundary was reached and all input has
///   been consumed.
/// * [`BlockState::NeedMore`] — more input or output room is required to make
///   further progress.
///
/// [`DeflateState`]: super::state::DeflateState
pub(crate) fn deflate_stored(cx: &mut DeflateContext<'_>, flush: Flush) -> BlockState {
    // Smallest worthy block size when not flushing or finishing. By default this
    // is 32K; it can be as small as 507 bytes for memLevel == 1, and for large
    // input/output buffers the stored block size will be larger. This depends on
    // `pending_buf_size == lit_bufsize * 4` (set in `state.rs`); if that sizing
    // is wrong, block boundaries — and therefore the output — diverge.
    let min_block = core::cmp::min(cx.state.pending_buf_size - 5, cx.state.w_size);

    // `last` becomes true once the final (BFINAL) block has been emitted.
    let mut last = false;
    // `used` starts as the avail_in at entry; after the direct-copy loop we
    // subtract the avail_in that remains to learn how many input bytes were
    // copied straight through to the output.
    let used_at_entry = cx.avail_in();

    // -----------------------------------------------------------------------
    // Phase 1 — Direct-copy fast path (C do { ... } while (last == 0))
    //
    // Copy as many `min_block`-or-larger stored blocks directly to `next_out` as
    // possible. If flushing, copy the remaining available input to `next_out` as
    // stored blocks when there is enough space.
    // -----------------------------------------------------------------------
    loop {
        // `len` = the maximum size block we can copy directly given the available
        // input and output. Start at the format maximum and clamp downward.
        let mut len = MAX_STORED;

        // Header byte count, including any bits already pending in the bit
        // accumulator: `(bi_valid + 42) >> 3`. We need room for the header before
        // anything else.
        let header = ((cx.state.bi_valid + 42) >> 3) as usize;
        if cx.avail_out() < header {
            // Not enough output room even for the header bytes.
            break;
        }
        // Maximum stored-block length that will fit in the remaining output.
        let have = cx.avail_out() - header;

        // `left` = window bytes that belong to the current block but have not yet
        // been written out (`strstart - block_start`). `block_start` is signed in
        // C (`long`); in this path it is a non-negative window index.
        let mut left = (cx.state.strstart as isize - cx.state.block_start) as usize;
        let avail_in = cx.avail_in();

        // Limit `len` to the data actually available (window leftovers + input)…
        if len > left + avail_in {
            len = left + avail_in;
        }
        // …and to the room available in the output.
        if len > have {
            len = have;
        }

        // If the stored block would be smaller than `min_block`, or we cannot copy
        // all available input while flushing, fall back to the window + pending
        // buffered path below. Also never write an empty block while flushing —
        // `deflate()` itself emits the terminating empty block.
        if len < min_block
            && ((len == 0 && flush != Flush::Finish)
                || flush == Flush::NoFlush
                || len != left + avail_in)
        {
            break;
        }

        // This is the final block iff we are finishing and `len` covers every
        // remaining byte (window leftovers + all input).
        last = flush == Flush::Finish && len == left + avail_in;

        // Lay down a dummy zero-length stored block in `pending` to get the header
        // bytes (including any previously pending bits), then overwrite the dummy
        // `LEN`/`NLEN` with the real `len`. This mirrors the C call
        // `_tr_stored_block(s, (char *)0, 0L, last)` followed by the four-byte
        // length patch.
        cx.state.tr_stored_block(&[], last);
        let p = cx.state.pending;
        cx.state.pending_buf[p - 4] = len as u8; //  LEN  low byte
        cx.state.pending_buf[p - 3] = (len >> 8) as u8; //  LEN  high byte
        cx.state.pending_buf[p - 2] = !(len as u8); // ~LEN  low byte
        cx.state.pending_buf[p - 1] = !((len >> 8) as u8); // ~LEN  high byte

        // Flush the stored-block header bytes from `pending` to `next_out`.
        cx.flush_pending();

        // Copy the uncompressed leftover window bytes to `next_out` first.
        if left != 0 {
            if left > len {
                left = len;
            }
            let bs = cx.state.block_start as usize;
            let no = cx.next_out;
            cx.output[no..no + left].copy_from_slice(&cx.state.window[bs..bs + left]);
            cx.next_out += left;
            *cx.total_out += left as u64;
            cx.state.block_start += left as isize;
            len -= left;
        }

        // Copy the remaining bytes directly from `next_in` to `next_out`, updating
        // the running check value and `total_in` via the engine's `read_buf`. The
        // output cursor is advanced here (C's `read_buf` only manages the input
        // side), exactly as the reference does.
        if len != 0 {
            cx.read_buf_to_output(len);
            cx.next_out += len;
            *cx.total_out += len as u64;
        }

        if last {
            break;
        }
    }

    // -----------------------------------------------------------------------
    // Phase 2 — Update the sliding window with the copied data (C L1792-1840)
    //
    // Refresh the window with the last `w_size` bytes of the copied data (or
    // append all of it when fewer than `w_size` bytes were copied), and update
    // the count of bytes still to insert into the hash tables in case
    // `deflateParams()` later switches to a non-zero compression level.
    // -----------------------------------------------------------------------
    let used = used_at_entry - cx.avail_in(); // input bytes directly copied
    if used != 0 {
        // Any input that was used leaves no unused input in the window, so
        // `block_start == strstart` holds on entry to this branch.
        if used >= cx.state.w_size {
            // Supplant the entire previous history: copy the last `w_size` copied
            // input bytes into the window and mark the hash as needing a clear
            // (`matches = 2`).
            cx.state.matches = 2;
            let w = cx.state.w_size;
            let src_end = cx.next_in;
            let src_start = src_end - w;
            cx.state.window[..w].copy_from_slice(&cx.input[src_start..src_end]);
            cx.state.strstart = w;
            cx.state.insert = cx.state.strstart;
        } else {
            if cx.state.window_size - cx.state.strstart <= used {
                // Slide the window down by `w_size` to make room.
                cx.state.strstart -= cx.state.w_size;
                let w = cx.state.w_size;
                let ss = cx.state.strstart;
                cx.state.window.copy_within(w..w + ss, 0);
                if cx.state.matches < 2 {
                    cx.state.matches += 1; // schedule a pending slide_hash()
                }
                if cx.state.insert > cx.state.strstart {
                    cx.state.insert = cx.state.strstart;
                }
            }
            // Append the `used` copied bytes to the end of the window history.
            let ss = cx.state.strstart;
            let src_end = cx.next_in;
            let src_start = src_end - used;
            cx.state.window[ss..ss + used].copy_from_slice(&cx.input[src_start..src_end]);
            cx.state.strstart += used;
            let w = cx.state.w_size;
            cx.state.insert += core::cmp::min(used, w - cx.state.insert);
        }
        cx.state.block_start = cx.state.strstart as isize;
    }
    if cx.state.high_water < cx.state.strstart {
        cx.state.high_water = cx.state.strstart;
    }

    // If the final block was written directly to `next_out`, we are done.
    if last {
        cx.state.bi_used = 8;
        return BlockState::FinishDone;
    }

    // If flushing (but not NO_FLUSH/FINISH) and all input has been consumed, the
    // block is complete.
    if flush != Flush::NoFlush
        && flush != Flush::Finish
        && cx.avail_in() == 0
        && cx.state.strstart as isize == cx.state.block_start
    {
        return BlockState::BlockDone;
    }

    // -----------------------------------------------------------------------
    // Phase 3 — Fill the window with any remaining input (C L1808-1819)
    // -----------------------------------------------------------------------
    let mut have = cx.state.window_size - cx.state.strstart;
    if cx.avail_in() > have && cx.state.block_start >= cx.state.w_size as isize {
        // Slide the window down to expose more space.
        cx.state.block_start -= cx.state.w_size as isize;
        cx.state.strstart -= cx.state.w_size;
        let w = cx.state.w_size;
        let ss = cx.state.strstart;
        cx.state.window.copy_within(w..w + ss, 0);
        if cx.state.matches < 2 {
            cx.state.matches += 1; // schedule a pending slide_hash()
        }
        have += cx.state.w_size; // more space now
        if cx.state.insert > cx.state.strstart {
            cx.state.insert = cx.state.strstart;
        }
    }
    if have > cx.avail_in() {
        have = cx.avail_in();
    }
    if have != 0 {
        let at = cx.state.strstart;
        cx.read_buf_to_window(at, have);
        cx.state.strstart += have;
        let w = cx.state.w_size;
        cx.state.insert += core::cmp::min(have, w - cx.state.insert);
    }
    if cx.state.high_water < cx.state.strstart {
        cx.state.high_water = cx.state.strstart;
    }

    // -----------------------------------------------------------------------
    // Phase 4 — Write a buffered stored block to `pending` (C L1826-1840)
    //
    // There was not enough `avail_out` to write a complete worthy or flushed
    // stored block directly to `next_out`. Write one to `pending` instead, if we
    // have enough input for a worthy block, or if flushing and the remaining
    // input fits as a stored block in the pending buffer.
    // -----------------------------------------------------------------------
    let header = ((cx.state.bi_valid + 42) >> 3) as usize; // bytes in header
    // Maximum stored-block length that will fit in the pending buffer.
    let have = core::cmp::min(cx.state.pending_buf_size - header, MAX_STORED);
    let min_block = core::cmp::min(have, cx.state.w_size);
    let left = (cx.state.strstart as isize - cx.state.block_start) as usize;
    if left >= min_block
        || ((left != 0 || flush == Flush::Finish)
            && flush != Flush::NoFlush
            && cx.avail_in() == 0
            && left <= have)
    {
        let len = core::cmp::min(left, have);
        last = flush == Flush::Finish && cx.avail_in() == 0 && len == left;
        let bs = cx.state.block_start as usize;
        // `_tr_stored_block(s, s->window + s->block_start, len, last)` copies the
        // window slice into `pending`. The C call aliases `s->window` while
        // mutating `s`; under `#![forbid(unsafe_code)]` we cannot hold a shared
        // borrow of `state.window` across the `&mut self` call, so we first copy
        // the slice into an owned temporary. The data is bounded by the pending
        // buffer, and this path is the low-throughput buffered case, so the extra
        // copy is negligible.
        let block = cx.state.window[bs..bs + len].to_vec();
        cx.state.tr_stored_block(&block, last);
        cx.state.block_start += len as isize;
        cx.flush_pending();
    }

    // We've done all we can with the available input and output.
    if last {
        cx.state.bi_used = 8;
    }
    if last {
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}
