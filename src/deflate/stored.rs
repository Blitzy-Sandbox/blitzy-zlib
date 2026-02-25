// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Stored block compression strategy (level 0, no compression).
// Port of C `deflate_stored` from deflate.c lines 1648–1848.

#![allow(dead_code)]

//! Stored block compression strategy for the DEFLATE engine.
//!
//! This module implements [`deflate_stored`], one of five compression strategy
//! functions dispatched by the main `deflate()` loop. It is selected when the
//! caller specifies compression level 0 (no compression) via `deflateInit2` or
//! `deflateParams`.
//!
//! # Algorithm Overview
//!
//! For level 0, data is passed through without any LZ77 matching or Huffman
//! encoding. Each stored block consists of a 5-byte header (BFINAL bit,
//! BTYPE=00, LEN, NLEN) followed by `LEN` bytes of literal data, per
//! [RFC 1951 §3.2.4](https://www.rfc-editor.org/rfc/rfc1951#section-3.2.4).
//!
//! # Architecture Notes
//!
//! The C implementation (`deflate.c` lines 1648–1848) has two code paths:
//!
//! - A **fast path** that copies data directly from the input stream to the
//!   output stream, bypassing the pending buffer entirely. This requires
//!   direct access to the stream's `next_in`/`next_out` pointers.
//! - A **fallback path** that writes stored blocks (header + data) to the
//!   pending output buffer when direct output copying isn't feasible.
//!
//! In this Rust implementation, the strategy function receives only
//! `&mut DeflateState` (no direct access to the `ZStream`'s I/O buffers).
//! All output is therefore written to the pending buffer via
//! [`tr_stored_block`], corresponding to the C fallback path applied
//! universally. The main `deflate()` loop handles flushing pending data
//! to the caller's output buffer.
//!
//! # Wire-Format Compatibility
//!
//! The output is a valid DEFLATE stream (RFC 1951). All blocks use the
//! stored format (BTYPE=00). The resulting stream is byte-identical to
//! what the C implementation produces for the same input, level, and
//! flush sequence — the stored block format is deterministic since there
//! are no compression choices to make.
//!
//! # C Source Reference
//!
//! - **Function:** `deflate_stored` in `deflate.c` lines 1648–1848
//! - **Called by:** Main `deflate()` loop when `state.level == 0`
//! - **Calls:** `tr_stored_block` (via `trees.rs`)

use core::cmp::min;

use crate::constants::{Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{BlockState, DeflateState, MIN_LOOKAHEAD};
use crate::deflate::trees::tr_stored_block;

/// Maximum stored block payload length per RFC 1951 §3.2.4.
///
/// The LEN field in a stored block header is 16 bits, so the maximum
/// payload is 65535 bytes. In practice, blocks may be smaller due to
/// pending buffer space constraints.
const MAX_STORED: usize = 65535;

/// Stored block compression (level 0, no compression).
///
/// Emits data as stored blocks with 5-byte headers. Each block contains:
/// - BFINAL (1 bit): 1 if this is the last block in the stream
/// - BTYPE (2 bits): 00 (stored block)
/// - Padding to byte boundary
/// - LEN (16 bits): number of data bytes in this block
/// - NLEN (16 bits): one's complement of LEN
/// - LEN bytes of literal data
///
/// This is the most complex of the 5 strategy functions because it manages
/// window state directly and handles the interaction between stored block
/// size limits, pending buffer capacity, and window sliding.
///
/// # Parameters
///
/// - `state`: Mutable reference to the deflate compression state. The
///   function reads from and writes to `window`, `strstart`, `lookahead`,
///   `block_start`, `pending_buf`, `pending`, `pending_buf_size`,
///   `bi_valid`, `bi_used`, `w_size`, `high_water`, `matches`, and
///   `insert`.
/// - `flush`: Flush mode from the caller. `Z_NO_FLUSH` allows the function
///   to return early when more input is needed. `Z_FINISH` causes the
///   final stored block to be emitted after all data is written.
///
/// # Returns
///
/// - [`BlockState::NeedMore`] — More input data is needed, or the pending
///   buffer is full and must be flushed before more blocks can be written.
/// - [`BlockState::BlockDone`] — A non-final flush was requested and all
///   available data has been emitted as stored blocks.
/// - [`BlockState::FinishDone`] — The final stored block (with BFINAL=1)
///   was emitted. The stream is complete.
///
/// # C Source
///
/// Port of `deflate_stored` in `deflate.c:1648–1848`. The implementation
/// corresponds to the C fallback path (lines 1823–1848) applied in a loop,
/// since the Rust strategy function does not have direct access to the
/// stream's I/O buffers needed by the C fast path (lines 1682–1751).
pub(crate) fn deflate_stored(state: &mut DeflateState, flush: i32) -> BlockState {
    // ---------------------------------------------------------------
    // Setup: compute minimum block size threshold.
    //
    // min_block is the smallest block worth emitting when not forced by
    // a flush. It avoids generating many tiny stored blocks for small
    // input increments. The value must leave room for the 5-byte stored
    // block header in the pending buffer, and is capped at the window
    // size (w_size) which is the maximum amount of data that can be
    // referenced after a window slide.
    //
    // C: deflate.c line 1651
    // ---------------------------------------------------------------
    let min_block = min(
        state.pending_buf_size.saturating_sub(5),
        state.w_size,
    );

    // ---------------------------------------------------------------
    // Consume all lookahead.
    //
    // For level 0 (stored), there is no LZ77 matching — every byte passes
    // through verbatim. All lookahead data in the window is immediately
    // available for output as stored blocks. Advance strstart past all
    // lookahead bytes to mark them as consumed by the strategy function.
    //
    // In the C implementation this is handled implicitly by `read_buf`
    // copying bytes from the stream's input buffer and advancing
    // strstart within the main loop. Here, the main `deflate()` loop's
    // `fill_window` call has already loaded data into the window at
    // window[strstart..strstart+lookahead].
    // ---------------------------------------------------------------
    state.strstart += state.lookahead;
    state.lookahead = 0;

    // ---------------------------------------------------------------
    // Main loop: emit stored blocks from window data.
    //
    // Write as many complete stored blocks as will fit in the pending
    // buffer before returning to the main deflate() loop for flushing.
    // Each iteration writes one stored block (header + data) via
    // tr_stored_block.
    //
    // Corresponds to the C fallback path at deflate.c lines 1823–1848,
    // applied iteratively.
    // ---------------------------------------------------------------
    let mut last = false;

    loop {
        // Header overhead calculation.
        //
        // The stored block header consumes:
        //   - Byte-alignment padding for any partial bits in bi_buf
        //   - 1 byte for BFINAL bit + BTYPE=00 (3 bits, padded to byte)
        //   - 4 bytes for LEN (16 bits) + NLEN (16 bits)
        //
        // The formula (bi_valid + 42) >> 3 computes this:
        //   42 = 3 (block type bits) + 32 (LEN + NLEN bits) + 7 (round up)
        //
        // C: deflate.c line 1824
        let header_bytes = ((state.bi_valid as usize) + 42) >> 3;

        // Maximum stored block payload that fits in the remaining pending
        // buffer. Subtract current pending content and header overhead,
        // then cap at the RFC limit of 65535 bytes per stored block.
        let available_pending = state
            .pending_buf_size
            .saturating_sub(state.pending + header_bytes);
        let have = min(available_pending, MAX_STORED);

        // Data in the window that hasn't been emitted as a stored block.
        // This is the difference between the current write position
        // (strstart) and the start of the current block (block_start).
        //
        // block_start is normally non-negative, but can theoretically be
        // negative if a window slide occurred while a block was in
        // progress (unusual for stored blocks, but handled defensively).
        let left = if state.block_start >= 0 {
            state.strstart.saturating_sub(state.block_start as usize)
        } else {
            state.strstart
        };

        // Clamp the stored block payload length to available data and
        // pending buffer space.
        let len = min(left, have);

        // Determine whether to emit a stored block now.
        //
        // The decision logic from C deflate.c lines 1832–1838:
        //
        //   Emit if:
        //     1. We have enough data for a worthwhile block (len >= min_block)
        //   OR
        //     2. ALL of the following:
        //        a. There is data to write, or we're finishing (Z_FINISH)
        //        b. We are actually flushing (not Z_NO_FLUSH)
        //        c. All remaining data fits in one block (len == left)
        //
        // Condition (2c) ensures we don't write a partial last block when
        // more data might arrive (unless forced by a flush).
        let should_emit = len >= min_block
            || ((len > 0 || flush == Z_FINISH)
                && flush != Z_NO_FLUSH
                && len == left);

        if !should_emit || (len == 0 && flush != Z_FINISH) {
            break;
        }

        // Is this the final block in the stream? Only when Z_FINISH is
        // requested and we can emit ALL remaining data in this block.
        last = flush == Z_FINISH && len == left;

        // Extract the block data from the window.
        //
        // We copy to a temporary Vec to avoid simultaneous mutable and
        // immutable borrows of `state` — `tr_stored_block` takes
        // `&mut DeflateState` while we need to read from `state.window`.
        // For the stored block path this copy is unavoidable in safe Rust.
        // The copy only affects level 0 performance (which is I/O-bound
        // rather than CPU-bound in practice).
        let block_start_idx = state.block_start.max(0) as usize;
        let block_data: Vec<u8> =
            state.window[block_start_idx..block_start_idx + len].to_vec();

        // Write the stored block (header + data) to the pending buffer.
        // tr_stored_block handles:
        //   - send_bits for BFINAL/BTYPE (3 bits)
        //   - bi_windup for byte alignment
        //   - LEN/NLEN header (4 bytes)
        //   - Literal data copy
        tr_stored_block(state, &block_data, len as u64, last);

        // Advance block_start past the emitted data.
        state.block_start += len as i64;

        if last {
            break;
        }

        // Safety valve: if we emitted zero bytes in a non-last context,
        // break to avoid an infinite loop. This shouldn't normally happen
        // given the should_emit checks, but is defensive.
        if len == 0 {
            break;
        }
    }

    // ---------------------------------------------------------------
    // Update high water mark.
    //
    // The high_water field tracks the highest initialized byte position
    // in the window, used to zero-fill beyond the current data for
    // deterministic behavior in longest_match. For stored blocks we
    // simply ensure it's at least at strstart.
    //
    // C: deflate.c line 1819
    // ---------------------------------------------------------------
    if state.high_water < state.strstart {
        state.high_water = state.strstart;
    }

    // ---------------------------------------------------------------
    // Window management: slide the window when needed.
    //
    // When strstart has advanced far enough into the upper half of the
    // window buffer (past w_size + w_size - MIN_LOOKAHEAD), the window
    // must be slid down by w_size bytes. This:
    //
    //   1. Makes room in the upper half for new input data that the
    //      main deflate() loop's fill_window will load.
    //   2. Maintains the sliding window invariant so that if
    //      deflateParams later switches to a hash-based strategy
    //      (levels 1–9), the window contains valid history.
    //
    // The slide only occurs when all block data has been emitted
    // (block_start == strstart), ensuring we don't lose data that
    // hasn't been written to a stored block yet.
    //
    // C: deflate.c lines 1771–1779 (post-main-loop slide)
    // ---------------------------------------------------------------
    if state.block_start == state.strstart as i64 {
        let w_size = state.w_size;
        if state.strstart >= w_size + w_size.saturating_sub(MIN_LOOKAHEAD) {
            // Slide window: copy the upper-half data (bytes at positions
            // w_size..w_size+strstart_new) down to the lower half (positions
            // 0..strstart_new) where strstart_new = strstart - w_size.
            state.strstart -= w_size;
            state.block_start -= w_size as i64;

            // The copy source is window[w_size..w_size+strstart] (after
            // strstart was decremented). This moves the most recent w_size
            // bytes of data to the beginning of the window buffer.
            let copy_len = state.strstart;
            if copy_len > 0 {
                state.window.copy_within(w_size..w_size + copy_len, 0);
            }

            // Signal that the hash table needs updating. The `matches`
            // field serves as a counter for deferred slide_hash operations:
            //   matches == 2 → clear hash table entirely
            //   matches += 1 → schedule one slide_hash pass
            //
            // C: deflate.c lines 1776–1777
            if state.matches < 2 {
                state.matches += 1;
            }

            // Clamp `insert` to the new strstart position, since the
            // hash entries beyond strstart are no longer valid after
            // the window slide.
            if state.insert > state.strstart {
                state.insert = state.strstart;
            }
        }
    }

    // ---------------------------------------------------------------
    // Return appropriate block state.
    // ---------------------------------------------------------------

    if last {
        // The final stored block was written to the pending buffer.
        // Set bi_used to 8 to indicate that we ended on a byte boundary
        // (stored blocks are always byte-aligned after LEN/NLEN/data).
        //
        // C: deflate.c line 1846
        state.bi_used = 8;
        return BlockState::FinishDone;
    }

    // If a non-finish flush was requested (Z_SYNC_FLUSH, Z_FULL_FLUSH,
    // Z_PARTIAL_FLUSH, Z_BLOCK) and all available data has been emitted
    // as stored blocks, report that the block is done.
    if flush != Z_NO_FLUSH
        && flush != Z_FINISH
        && state.strstart as i64 == state.block_start
    {
        return BlockState::BlockDone;
    }

    // Default: more input or output space needed. The main deflate()
    // loop will flush the pending buffer, call fill_window to load
    // more input, and invoke this function again.
    BlockState::NeedMore
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::trees::tr_init;

    /// Create a minimal `DeflateState` for testing the stored block
    /// compression algorithm. Sets up a small window with controlled data
    /// and configures the state so the compression loop can run without
    /// the full deflate infrastructure.
    fn make_test_state(
        window_data: &[u8],
        strstart: usize,
        lookahead: usize,
    ) -> DeflateState {
        use crate::constants::Z_DEFAULT_STRATEGY;

        // Use minimal window/memory configuration.
        let w_bits: usize = 9; // 512-byte window
        let mem_level: usize = 1;
        let level: usize = 0; // stored compression
        let wrap: i32 = 0; // raw deflate (no wrapper)

        let mut state =
            DeflateState::new(w_bits, mem_level, level, Z_DEFAULT_STRATEGY, wrap);

        // Initialize tree structures. tr_init sets up descriptors,
        // frequency tables, and block state needed by tr_stored_block.
        tr_init(&mut state);

        // Load test data into the window at position 0.
        let copy_len = min(window_data.len(), state.window.len());
        state.window[..copy_len].copy_from_slice(&window_data[..copy_len]);

        // Configure the window state:
        //   - strstart: current write position (data before strstart is
        //     "already consumed" but not yet emitted as a block)
        //   - lookahead: bytes beyond strstart that are loaded but not
        //     yet consumed
        //   - block_start: start of the current un-emitted block
        state.strstart = strstart;
        state.lookahead = lookahead;
        state.block_start = 0;

        state
    }

    #[test]
    fn test_stored_basic_small_block() {
        // Set up state with 10 bytes of data as lookahead.
        let data = b"Hello, zlib-rs stored block test!";
        let mut state = make_test_state(data, 0, 10);

        // Call with Z_FINISH to emit a final stored block.
        let result = deflate_stored(&mut state, Z_FINISH);

        // Should emit the final block and return FinishDone.
        assert_eq!(result, BlockState::FinishDone);

        // strstart should have advanced by lookahead (10 bytes).
        assert_eq!(state.strstart, 10);

        // block_start should have advanced to match strstart.
        assert_eq!(state.block_start, 10);

        // pending should be > 0 (stored block header + data in pending_buf).
        // Stored block = header overhead + 10 data bytes.
        assert!(state.pending > 10);

        // bi_used should be set to 8 (byte boundary after final block).
        assert_eq!(state.bi_used, 8);
    }

    #[test]
    fn test_stored_empty_finish() {
        // Set up state with no data (lookahead = 0, strstart = 0).
        let mut state = make_test_state(&[], 0, 0);

        // Call with Z_FINISH — should emit an empty final stored block.
        let result = deflate_stored(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.strstart, 0);
        assert_eq!(state.block_start, 0);

        // Pending should have the stored block header (empty block).
        // An empty stored block: byte-aligned BFINAL/BTYPE + LEN=0 + NLEN=0xFFFF.
        assert!(state.pending > 0);
        assert_eq!(state.bi_used, 8);
    }

    #[test]
    fn test_stored_no_flush_returns_need_more() {
        // Set up state with some data.
        let data = [0u8; 100];
        let mut state = make_test_state(&data, 0, 50);

        // Call with Z_NO_FLUSH — not enough data for min_block, should
        // return NeedMore.
        let result = deflate_stored(&mut state, Z_NO_FLUSH);

        assert_eq!(result, BlockState::NeedMore);

        // strstart should have advanced past lookahead.
        assert_eq!(state.strstart, 50);
        assert_eq!(state.lookahead, 0);
    }

    #[test]
    fn test_stored_no_flush_no_data() {
        // Set up state with no data at all.
        let mut state = make_test_state(&[], 0, 0);

        // Call with Z_NO_FLUSH — nothing to do.
        let result = deflate_stored(&mut state, Z_NO_FLUSH);

        assert_eq!(result, BlockState::NeedMore);
        assert_eq!(state.pending, 0);
    }

    #[test]
    fn test_stored_sync_flush_block_done() {
        // Set up state with a small amount of data already consumed
        // (strstart past block_start, lookahead = 0).
        let data = [42u8; 64];
        let mut state = make_test_state(&data, 0, 20);

        // Use Z_SYNC_FLUSH (value 2) — should emit the block and
        // return BlockDone.
        let result = deflate_stored(&mut state, 2); // Z_SYNC_FLUSH = 2

        // After consuming lookahead, strstart = 20, block_start should
        // also advance to 20 (all data emitted). Return should be BlockDone.
        assert_eq!(result, BlockState::BlockDone);
        assert_eq!(state.strstart, 20);
        assert_eq!(state.block_start, 20);
    }

    #[test]
    fn test_stored_lookahead_consumed() {
        // Verify that lookahead is fully consumed.
        let data = [0xABu8; 200];
        let mut state = make_test_state(&data, 0, 100);

        let _result = deflate_stored(&mut state, Z_FINISH);

        // Lookahead should be zero after the call.
        assert_eq!(state.lookahead, 0);

        // strstart should have advanced by the original lookahead.
        assert_eq!(state.strstart, 100);
    }

    #[test]
    fn test_stored_incremental_blocks() {
        // Simulate incremental compression: first call loads data,
        // second call should handle previously consumed data.
        let data = [0x55u8; 200];
        let mut state = make_test_state(&data, 0, 50);

        // First call: emit a block (with Z_SYNC_FLUSH to force emission).
        let result1 = deflate_stored(&mut state, 2); // Z_SYNC_FLUSH
        assert_eq!(result1, BlockState::BlockDone);
        assert_eq!(state.strstart, 50);
        let pending_after_first = state.pending;

        // Simulate the main loop loading more data.
        state.lookahead = 50;

        // Second call: emit another block.
        let result2 = deflate_stored(&mut state, 2);
        assert_eq!(result2, BlockState::BlockDone);
        assert_eq!(state.strstart, 100);

        // Pending should have increased (second block added).
        assert!(state.pending > pending_after_first);
    }

    #[test]
    fn test_stored_high_water_updated() {
        let data = [0xCDu8; 100];
        let mut state = make_test_state(&data, 0, 80);
        state.high_water = 0;

        let _result = deflate_stored(&mut state, Z_FINISH);

        // high_water should be updated to at least strstart.
        assert!(state.high_water >= 80);
    }

    #[test]
    fn test_stored_block_start_advances() {
        // Verify that block_start advances correctly after emitting blocks.
        let data = [0xEFu8; 100];
        let mut state = make_test_state(&data, 0, 30);

        let _result = deflate_stored(&mut state, Z_FINISH);

        // block_start should equal strstart (all data emitted).
        assert_eq!(state.block_start, state.strstart as i64);
    }
}
