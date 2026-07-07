//! `deflate_stored` — copy input to output as *stored* (uncompressed) DEFLATE
//! blocks (`deflate.c` L1668-L1848).
//!
//! This is the block producer selected at compression level `0`
//! ([`Z_NO_COMPRESSION`](crate::constants::Z_NO_COMPRESSION)). No compression is
//! performed: the input is framed into stored blocks (RFC 1951 §3.2.4) whose
//! payload bytes are copied verbatim. The emitted byte stream is **identical**
//! to the one produced by reference zlib for the same input, `flush` argument,
//! and stream wrapper, which is the defining acceptance criterion for the port.
//!
//! # Why this is more than a `memcpy`
//!
//! `deflate_stored` is written to *minimize the number of times an input byte is
//! copied*. Whenever the caller supplies both a large input buffer and a large
//! output buffer it copies straight from `next_in` to `next_out`, bypassing both
//! the sliding window and the pending buffer. Only the leftover tail that cannot
//! form a full worthy block is staged through the window and then written out of
//! the pending buffer.
//!
//! The sliding window is still maintained even though nothing is compressed, so
//! that a later `deflateParams` switch to a non-zero level has correct match
//! history. The [`matches`](crate::deflate::state::DeflateState::matches) field
//! doubles as a counter of pending hash-table slides for exactly that
//! transition: `0` means none pending, `1` means one `slide_hash`, and `2` (the
//! maximum tracked here) means a full hash-table clear — because two or more
//! slides are equivalent to a clear.
//!
//! # Safety
//!
//! Zero `unsafe`. Every buffer move is a bounds-checked slice operation
//! ([`copy_from_slice`](slice::copy_from_slice) for disjoint buffers and
//! [`copy_within`](slice::copy_within) for the in-place window slides); the
//! module is compiled under `#![deny(unsafe_code)]`.

#![deny(unsafe_code)]

use crate::constants::{Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{DeflateState, IoContext};
use crate::deflate::strategy::BlockState;
use crate::deflate::trees;
use core::cmp::min;

/// Maximum stored block length in the DEFLATE format, excluding the 5-byte
/// header (`deflate.c` L1648: `#define MAX_STORED 65535`).
const MAX_STORED: usize = 65535;

/// Copies as much of the input as possible to the output as stored
/// (uncompressed) DEFLATE blocks, returning the resulting [`BlockState`].
///
/// Faithful port of the C `deflate_stored` (`deflate.c` L1668-L1848). `s` holds
/// the deflate state (window, pending buffer, bit accumulator); `io` threads the
/// `z_stream` input/output cursors, byte counters, and running checksum that the
/// C function reaches through `s->strm`; `flush` is the flush mode requested by
/// the current `deflate` call (one of the `Z_*` flush constants).
///
/// # Behavior
///
/// * **Direct copy.** While a full worthy stored block (at least `min_block`
///   bytes, or all remaining input when flushing) fits in the output, the
///   5-byte block header is emitted through the pending buffer and the payload
///   is copied directly from `io.input` to `io.output`, updating the running
///   checksum over the copied bytes.
/// * **Window maintenance.** Input bytes that were directly copied are also
///   mirrored into the sliding window (sliding it down by one window when full)
///   so match history is preserved for a possible later level change;
///   `s.matches` accumulates the number of pending hash slides.
/// * **Tail staging.** Any remaining input that cannot form a worthy direct
///   block is read into the window and then emitted as a stored block through
///   the pending buffer.
///
/// # Returns
///
/// * [`BlockState::FinishDone`] — the final block was written straight to the
///   output; the stream is complete.
/// * [`BlockState::FinishStarted`] — finishing, but the last block could only be
///   staged into the pending buffer, so more output space is needed to drain it.
/// * [`BlockState::BlockDone`] — a non-`Z_FINISH` flush consumed all input and
///   reached a block boundary.
/// * [`BlockState::NeedMore`] — otherwise; more input and/or output is required.
///
/// # Preconditions
///
/// The pending buffer must be empty on entry, matching the C contract that
/// "compression must start with an empty pending buffer" (`deflate.c` L1084):
/// the deflate driver flushes any stream-wrapper header before invoking a block
/// producer. The direct-copy output accounting relies on this invariant.
pub fn deflate_stored(s: &mut DeflateState, io: &mut IoContext, flush: i32) -> BlockState {
    // `w_size` never changes during the call; cache it once so the window-slide
    // arithmetic below reads a plain local instead of repeatedly borrowing `s`.
    let wsize = s.w_size;

    // Smallest worthy block size when not flushing or finishing. By default this
    // is the window size; it can be as small as 507 bytes for `mem_level == 1`.
    // For large input and output buffers the stored block will be larger.
    // (`deflate.c` L1673-L1676.)
    let mut min_block = min(s.pending_buf_size - 5, wsize);

    // Copy as many `min_block` or larger stored blocks directly to `next_out` as
    // possible. If flushing, copy the remaining available input to `next_out` as
    // stored blocks, if there is enough space.
    let mut last = false;
    let used_start = io.avail_in; // C: `unsigned used = s->strm->avail_in;`

    loop {
        // Set `len` to the maximum size block we can copy directly with the
        // available input data and output space. Set `left` to how much of that
        // would be copied from what is left in the window.
        let mut len = MAX_STORED; // maximum deflate stored block length
        let mut have = ((s.bi_valid as u32 + 42) >> 3) as usize; // bytes in header
        if io.avail_out < have {
            break; // need room for header
        }
        // Maximum stored block length that will fit in `avail_out`:
        have = io.avail_out - have;
        let mut left = (s.strstart as isize - s.block_start) as usize; // window bytes
        if len > left + io.avail_in {
            len = left + io.avail_in; // limit len to the input
        }
        if len > have {
            len = have; // limit len to the output
        }

        // If the stored block would be shorter than `min_block`, or if unable to
        // copy all of the available input when flushing, then try copying to the
        // window and the pending buffer instead. Also do not write an empty
        // block when flushing — `deflate()` does that.
        if len < min_block
            && ((len == 0 && flush != Z_FINISH) || flush == Z_NO_FLUSH || len != left + io.avail_in)
        {
            break;
        }

        // Make a dummy stored block in pending to get the header bytes,
        // including any pending bits. This also aligns the bit buffer.
        last = flush == Z_FINISH && len == left + io.avail_in;
        trees::_tr_stored_block(s, None, 0, last);

        // Replace the length placeholders in the dummy stored block with `len`:
        // the two-byte LEN followed by the two-byte NLEN (`~LEN`), each written
        // least-significant byte first. (`deflate.c` L1717-L1720.)
        let p = s.pending;
        s.pending_buf[p - 4] = len as u8;
        s.pending_buf[p - 3] = (len >> 8) as u8;
        s.pending_buf[p - 2] = !(len as u8);
        s.pending_buf[p - 1] = !((len >> 8) as u8);

        // Write the stored block header bytes.
        s.flush_pending(io);

        // Copy uncompressed bytes from the window to `next_out`.
        if left != 0 {
            if left > len {
                left = len;
            }
            let bs = s.block_start as usize;
            let no = io.next_out;
            io.output[no..no + left].copy_from_slice(&s.window[bs..bs + left]);
            io.next_out += left;
            io.avail_out -= left;
            io.total_out += left as u64;
            s.block_start += left as isize;
            len -= left;
        }

        // Copy uncompressed bytes directly from `next_in` to `next_out`,
        // updating the check value. `read_buf_into` advances the input cursor,
        // `avail_in`, `total_in`, and the checksum; the output-side counters are
        // advanced here, exactly as the C code does.
        if len != 0 {
            let no = io.next_out;
            DeflateState::read_buf_into(
                io.input,
                &mut io.next_in,
                &mut io.avail_in,
                &mut io.total_in,
                &mut io.adler,
                s.wrap,
                &mut io.output[no..no + len],
            );
            io.next_out += len;
            io.avail_out -= len;
            io.total_out += len as u64;
        }

        if last {
            break;
        }
    }

    // Update the sliding window with the last `w_size` bytes of the copied data,
    // or append all of the copied data to the existing window if fewer than
    // `w_size` bytes were copied. Also update the number of bytes to insert into
    // the hash tables, in the event that `deflateParams()` switches to a
    // non-zero compression level.
    let used = used_start - io.avail_in; // number of input bytes directly copied
    if used != 0 {
        // If any input was used then no unused input remains in the window,
        // therefore `s.block_start == s.strstart`.
        if used >= wsize {
            // Supplant the previous history.
            s.matches = 2; // clear hash
            let src = io.next_in - wsize;
            s.window[..wsize].copy_from_slice(&io.input[src..src + wsize]);
            s.strstart = wsize;
            s.insert = s.strstart;
        } else {
            if s.window_size - s.strstart <= used {
                // Slide the window down.
                s.strstart -= wsize;
                let ss = s.strstart;
                s.window.copy_within(wsize..wsize + ss, 0);
                if s.matches < 2 {
                    s.matches += 1; // add a pending slide_hash()
                }
                if s.insert > s.strstart {
                    s.insert = s.strstart;
                }
            }
            let src = io.next_in - used;
            let ss = s.strstart;
            s.window[ss..ss + used].copy_from_slice(&io.input[src..src + used]);
            s.strstart += used;
            s.insert += min(used, wsize - s.insert);
        }
        s.block_start = s.strstart as isize;
    }
    if s.high_water < s.strstart {
        s.high_water = s.strstart;
    }

    // If the last block was written to `next_out`, then done.
    if last {
        s.bi_used = 8;
        return BlockState::FinishDone;
    }

    // If flushing and all input has been consumed, then done.
    if flush != Z_NO_FLUSH
        && flush != Z_FINISH
        && io.avail_in == 0
        && s.strstart as isize == s.block_start
    {
        return BlockState::BlockDone;
    }

    // Fill the window with any remaining input.
    let mut have = s.window_size - s.strstart;
    if io.avail_in > have && s.block_start >= wsize as isize {
        // Slide the window down.
        s.block_start -= wsize as isize;
        s.strstart -= wsize;
        let ss = s.strstart;
        s.window.copy_within(wsize..wsize + ss, 0);
        if s.matches < 2 {
            s.matches += 1; // add a pending slide_hash()
        }
        have += wsize; // more space now
        if s.insert > s.strstart {
            s.insert = s.strstart;
        }
    }
    if have > io.avail_in {
        have = io.avail_in;
    }
    if have != 0 {
        let ss = s.strstart;
        DeflateState::read_buf_into(
            io.input,
            &mut io.next_in,
            &mut io.avail_in,
            &mut io.total_in,
            &mut io.adler,
            s.wrap,
            &mut s.window[ss..ss + have],
        );
        s.strstart += have;
        s.insert += min(have, wsize - s.insert);
    }
    if s.high_water < s.strstart {
        s.high_water = s.strstart;
    }

    // There was not enough `avail_out` to write a complete worthy or flushed
    // stored block to `next_out`. Write a stored block to pending instead, if we
    // have enough input for a worthy block, or if flushing and there is enough
    // room for the remaining input as a stored block in the pending buffer.
    have = ((s.bi_valid as u32 + 42) >> 3) as usize; // bytes in header
    // Maximum stored block length that will fit in pending:
    have = min(s.pending_buf_size - have, MAX_STORED);
    min_block = min(have, wsize);
    let left = (s.strstart as isize - s.block_start) as usize;
    if left >= min_block
        || ((left != 0 || flush == Z_FINISH)
            && flush != Z_NO_FLUSH
            && io.avail_in == 0
            && left <= have)
    {
        let len = min(left, have);
        last = flush == Z_FINISH && io.avail_in == 0 && len == left;
        let bs = s.block_start as usize;
        trees::_tr_stored_block(s, Some(bs), len, last);
        s.block_start += len as isize;
        s.flush_pending(io);
    }

    // We've done all we can with the available input and output.
    if last {
        s.bi_used = 8;
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}
