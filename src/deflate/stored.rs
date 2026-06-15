//! Level-0 (store-only) DEFLATE strategy.
//!
//! This module is a faithful, memory-safe Rust port of `deflate_stored()` from
//! the C zlib reference implementation (`deflate.c`, upstream lines 1668-1848).
//! It implements the compression "strategy" selected for compression level
//! `Z_NO_COMPRESSION` (0): the input is emitted verbatim as a sequence of
//! DEFLATE *stored* blocks (RFC 1951 §3.2.4) — no LZ77 matching and no Huffman
//! coding are performed.
//!
//! # Why this function is more involved than "just copy the bytes"
//!
//! Although stored blocks carry no compression, `deflate_stored` is the most
//! intricate of the five strategy functions because it must:
//!
//! * frame the copied data into stored blocks whose 5-byte headers (the 3-bit
//!   block-type prefix, byte alignment, the little-endian 16-bit length and its
//!   one's complement) are **bit-for-bit identical** to C zlib;
//! * limit each block to the available input *and* output, never emitting an
//!   empty block while flushing (that is `deflate()`'s job), and never emitting
//!   a block smaller than `min_block` unless it is the exact finishing block;
//! * keep the sliding window and the hash-insertion bookkeeping
//!   (`strstart`, `block_start`, `insert`, `matches`, `high_water`) correct so
//!   that a later [`deflateParams`]-style switch to a non-zero compression
//!   level still sees the proper match history. `matches == 2` records that a
//!   full hash clear is pending, while `matches += 1` records a single pending
//!   `slide_hash()`.
//!
//! Getting any of those wrong would still "work" in the sense of producing
//! decompressible output, but the bytes would diverge from canonical zlib and
//! violate the project's byte-identical-output guarantee (AAP §0.6.6, §0.7.1).
//!
//! # Safety and portability
//!
//! * Contains **no `unsafe`** (AAP §0.6.2). Every window↔output and
//!   window↔pending copy is expressed as a disjoint-field
//!   [`copy_from_slice`](slice::copy_from_slice) /
//!   [`copy_within`](slice::copy_within), which the borrow checker proves
//!   non-aliasing.
//! * Uses only `core` facilities, so it is `no_std`-clean.
//!
//! [`deflateParams`]: https://www.zlib.net/manual.html

use crate::constants::FlushMode;
use crate::deflate::state::{BlockState, DeflateStream, MAX_STORED};

/// Copy pending input directly to the output as DEFLATE stored blocks
/// (level-0 / `Z_NO_COMPRESSION`).
///
/// This is the Rust equivalent of C `deflate_stored(deflate_state *s, int
/// flush)`. It is registered in `strategy::CONFIG_TABLE` as the `compress`
/// function for level 0, so its signature **must** remain exactly
/// `fn(&mut DeflateStream<'_>, FlushMode) -> BlockState`.
///
/// # Behavior
///
/// `deflate_stored` is written to minimize the number of times an input byte is
/// copied; it is most efficient with large input and output buffers, which
/// maximize the opportunity for a single copy straight from the input to the
/// output. The function:
///
/// 1. Copies as many `min_block`-or-larger stored blocks as possible directly
///    from the window/input to the output (the do-while loop).
/// 2. Updates the sliding window with the last `w_size` bytes of copied data
///    (or appends all copied data when fewer than `w_size` bytes were copied),
///    maintaining the hash-insertion counters for a possible level switch.
/// 3. Refills the window with any remaining input.
/// 4. As a fallback, when there was not enough output room for a complete
///    worthy/flushed block, writes a stored block into the *pending* buffer
///    instead.
///
/// # Returns
///
/// * [`BlockState::FinishDone`] — the final block was written to the output.
/// * [`BlockState::BlockDone`] — flushing and all input has been consumed.
/// * [`BlockState::FinishStarted`] — the final block was written to *pending*
///   but not yet fully flushed to the output.
/// * [`BlockState::NeedMore`] — more input and/or output space is required.
///
/// # Implementation note — the `tr_stored_block` call shape
///
/// The C source emits the fallback block with a single call
/// `_tr_stored_block(s, s->window + s->block_start, len, last)`, passing a
/// pointer into the window as the block payload. The Rust
/// [`DeflateStream::state`] field holds the window, so a literal translation
/// `s.state.tr_stored_block(Some(&s.state.window[..]), ..)` would borrow
/// `*s.state` both mutably (the receiver) and immutably (the slice argument)
/// across the call, which the borrow checker rejects (E0502). Instead we use
/// the same technique the direct-copy loop above already uses: emit the header
/// with a *dummy* zero length (`tr_stored_block(None, 0, last)`), patch the four
/// length bytes in place, then copy the window payload into `pending_buf`
/// ourselves via a disjoint-field copy. The emitted bytes are identical to the
/// single-call form because the patched bytes equal what `put_short(len)` /
/// `put_short(!len)` would have written, and the payload copy is byte-for-byte
/// the same — but no aliasing borrow and no temporary allocation are required.
pub(crate) fn deflate_stored(s: &mut DeflateStream<'_>, flush: FlushMode) -> BlockState {
    // -- Prologue (deflate.c 1668-1681) ------------------------------------
    //
    // Smallest worthy block size when not flushing or finishing. By default
    // this is 32K; it can be as small as 507 bytes for memLevel == 1. For large
    // input and output buffers, the stored block size will be larger.
    let mut min_block = (s.state.pending_buf_size - 5).min(s.state.w_size);

    // Whether the final (BFINAL) block of the stream has been written.
    let mut last = false;

    // C: `unsigned used = s->strm->avail_in;` — snapshot of the input available
    // on entry, used after the loop to compute how much was consumed.
    let used_start = s.avail_in();

    // -- Direct-copy do-while loop (deflate.c 1682-1751) -------------------
    //
    // Copy as many `min_block`-or-larger stored blocks directly to the output
    // as possible. While flushing, copy the remaining available input to the
    // output as stored blocks if there is room. Mirrors the C `do { ... }
    // while (last == 0);` exactly: the trailing `if last { break; }` reproduces
    // the loop condition.
    loop {
        // Maximum DEFLATE stored block length.
        let mut len: usize = MAX_STORED;

        // Bytes needed in the output for the block header (including any
        // currently buffered bits): `(bi_valid + 42) >> 3`.
        let mut have = ((s.state.bi_valid as usize) + 42) >> 3;
        if s.avail_out() < have {
            // Not enough room even for the header.
            break;
        }
        // Maximum stored block length that will fit in the remaining output.
        have = s.avail_out() - have;

        // Bytes currently sitting in the window between `block_start` and
        // `strstart`. `block_start` is signed (it can briefly go negative when
        // the window slides), so the subtraction is performed in `isize`.
        let mut left = (s.state.strstart as isize - s.state.block_start) as usize;
        if len > left + s.avail_in() {
            len = left + s.avail_in(); // limit `len` to the available input
        }
        if len > have {
            len = have; // limit `len` to the available output
        }

        // If the stored block would be shorter than `min_block`, or if we
        // cannot copy all of the available input while flushing, then try
        // copying to the window and the pending buffer instead. Also never
        // write an empty block while flushing — `deflate()` handles that.
        if len < min_block
            && ((len == 0 && flush != FlushMode::Finish)
                || flush == FlushMode::NoFlush
                || len != left + s.avail_in())
        {
            break;
        }

        // This is the final block iff we are finishing and `len` covers every
        // remaining byte (window + input).
        last = flush == FlushMode::Finish && len == left + s.avail_in();

        // Emit a dummy stored block into `pending` to lay down the header bytes
        // (block type, byte alignment, and placeholder length words). This also
        // advances the debugging counts in the underlying primitive.
        s.state.tr_stored_block(None, 0, last);

        // Replace the dummy length words with the real `len`: little-endian
        // 16-bit length followed by its one's complement (deflate.c 1716-1719).
        // `!len` is computed in `usize` then masked, matching C `(Bytef)~len`.
        let p = s.state.pending;
        s.state.pending_buf[p - 4] = (len & 0xff) as u8;
        s.state.pending_buf[p - 3] = ((len >> 8) & 0xff) as u8;
        s.state.pending_buf[p - 2] = (!len & 0xff) as u8;
        s.state.pending_buf[p - 1] = ((!len >> 8) & 0xff) as u8;

        // Write the stored block header bytes to the output.
        s.flush_pending();

        // Copy uncompressed bytes from the window to the output.
        if left != 0 {
            if left > len {
                left = len;
            }
            let bs = s.state.block_start as usize;
            let o = s.out_next;
            // `output` and `state` are disjoint fields of the stream, so the
            // exclusive and shared borrows coexist without aliasing.
            s.output[o..o + left].copy_from_slice(&s.state.window[bs..bs + left]);
            s.out_next += left;
            s.state.total_out += left as u64;
            s.state.block_start += left as isize;
            len -= left;
        }

        // Copy uncompressed bytes directly from the input to the output,
        // updating the running check value. `read_buf_output` advances the
        // input/output cursors and `total_in`/`adler`; it does *not* touch
        // `total_out`, so we add `len` here (deflate.c 1745-1747). Because
        // `len <= avail_in` at this point, exactly `len` bytes are copied.
        if len != 0 {
            s.read_buf_output(len);
            s.state.total_out += len as u64;
        }

        if last {
            break; // C: `while (last == 0)`
        }
    }

    // -- Update the sliding window (deflate.c 1759-1787) -------------------
    //
    // Update the window with the last `w_size` bytes of the copied data, or
    // append all of the copied data when fewer than `w_size` bytes were copied.
    // Also update `insert` (bytes still to insert into the hash tables) so that
    // a later `deflateParams()` switch to a non-zero level has correct history.
    let used = used_start - s.avail_in(); // input bytes copied directly
    if used != 0 {
        // Any input that was used implies no unused input remains in the
        // window, therefore `block_start == strstart`.
        if used >= s.state.w_size {
            // Supplant the previous history with the last `w_size` input bytes.
            s.state.matches = 2; // clear hash (two pending slides == a clear)
            let wsize = s.state.w_size;
            let src = s.in_next - wsize;
            s.state.window[0..wsize].copy_from_slice(&s.input[src..src + wsize]);
            s.state.strstart = wsize;
            s.state.insert = s.state.strstart;
        } else {
            if s.state.window_size - s.state.strstart <= used {
                // Slide the window down by `w_size`.
                s.state.strstart -= s.state.w_size;
                let wsize = s.state.w_size;
                let ss = s.state.strstart;
                s.state.window.copy_within(wsize..wsize + ss, 0);
                if s.state.matches < 2 {
                    s.state.matches += 1; // add a pending slide_hash()
                }
                if s.state.insert > s.state.strstart {
                    s.state.insert = s.state.strstart;
                }
            }
            let used_src = s.in_next - used;
            let ss = s.state.strstart;
            s.state.window[ss..ss + used].copy_from_slice(&s.input[used_src..used_src + used]);
            s.state.strstart += used;
            s.state.insert += used.min(s.state.w_size - s.state.insert);
        }
        s.state.block_start = s.state.strstart as isize;
    }
    if s.state.high_water < s.state.strstart {
        s.state.high_water = s.state.strstart;
    }

    // -- Early returns (deflate.c 1789-1798) -------------------------------
    //
    // If the final block was written to the output, we are done.
    if last {
        s.state.bi_used = 8;
        return BlockState::FinishDone;
    }
    // If flushing (but not finishing) and all input has been consumed, done.
    if flush != FlushMode::NoFlush
        && flush != FlushMode::Finish
        && s.avail_in() == 0
        && (s.state.strstart as isize) == s.state.block_start
    {
        return BlockState::BlockDone;
    }

    // -- Fill the window with any remaining input (deflate.c 1800-1821) ----
    let mut have = s.state.window_size - s.state.strstart;
    if s.avail_in() > have && s.state.block_start >= s.state.w_size as isize {
        // Slide the window down to make room.
        s.state.block_start -= s.state.w_size as isize;
        s.state.strstart -= s.state.w_size;
        let wsize = s.state.w_size;
        let ss = s.state.strstart;
        s.state.window.copy_within(wsize..wsize + ss, 0);
        if s.state.matches < 2 {
            s.state.matches += 1; // add a pending slide_hash()
        }
        have += s.state.w_size; // more space now
        if s.state.insert > s.state.strstart {
            s.state.insert = s.state.strstart;
        }
    }
    if have > s.avail_in() {
        have = s.avail_in();
    }
    if have != 0 {
        let ss = s.state.strstart;
        s.read_buf_window(ss, have);
        s.state.strstart += have;
        s.state.insert += have.min(s.state.w_size - s.state.insert);
    }
    if s.state.high_water < s.state.strstart {
        s.state.high_water = s.state.strstart;
    }

    // -- Pending-buffer stored-block fallback (deflate.c 1828-1842) --------
    //
    // There was not enough output room to write a complete worthy or flushed
    // stored block. Write a stored block into the pending buffer instead, if we
    // have enough input for a worthy block, or if flushing and the remaining
    // input fits as a stored block in the pending buffer.
    let mut have = ((s.state.bi_valid as usize) + 42) >> 3; // bytes in header
    // Maximum stored block length that will fit in the pending buffer.
    have = (s.state.pending_buf_size - have).min(MAX_STORED);
    min_block = have.min(s.state.w_size);
    let left = (s.state.strstart as isize - s.state.block_start) as usize;
    if left >= min_block
        || ((left != 0 || flush == FlushMode::Finish)
            && flush != FlushMode::NoFlush
            && s.avail_in() == 0
            && left <= have)
    {
        let len = left.min(have);
        last = flush == FlushMode::Finish && s.avail_in() == 0 && len == left;

        // Equivalent to C `_tr_stored_block(s, s->window + s->block_start, len,
        // last)` but split to avoid aliasing `*s.state` (see the function-level
        // note). Emit the header with a dummy length, patch the real length,
        // then copy the window payload into `pending_buf` ourselves.
        s.state.tr_stored_block(None, 0, last);
        let p = s.state.pending;
        s.state.pending_buf[p - 4] = (len & 0xff) as u8;
        s.state.pending_buf[p - 3] = ((len >> 8) & 0xff) as u8;
        s.state.pending_buf[p - 2] = (!len & 0xff) as u8;
        s.state.pending_buf[p - 1] = ((!len >> 8) & 0xff) as u8;
        if len != 0 {
            let bs = s.state.block_start as usize;
            let pp = s.state.pending;
            // `pending_buf` (mutable) and `window` (shared) are disjoint fields
            // of `*s.state`, so this copy needs no `unsafe` and no temporary.
            s.state.pending_buf[pp..pp + len].copy_from_slice(&s.state.window[bs..bs + len]);
            s.state.pending += len;
        }
        s.state.block_start += len as isize;
        s.flush_pending();
    }

    // -- Final return (deflate.c 1845-1847) --------------------------------
    //
    // We have done all we can with the available input and output.
    if last {
        s.state.bi_used = 8;
    }
    if last {
        BlockState::FinishStarted
    } else {
        BlockState::NeedMore
    }
}
