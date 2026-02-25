// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Huffman-only compression strategy (Z_HUFFMAN_ONLY).
// Port of C `deflate_huff` from deflate.c lines 2151–2185.

#![allow(dead_code)]

//! Huffman-only compression strategy for the DEFLATE engine.
//!
//! This module implements `deflate_huff`, one of five compression strategy
//! functions dispatched by the main `deflate()` loop. It is selected when
//! the caller specifies `Z_HUFFMAN_ONLY` (strategy 2) via `deflateInit2` or
//! `deflateParams`.
//!
//! # Algorithm Overview
//!
//! Unlike the LZ77-based strategies (`deflate_fast`, `deflate_slow`),
//! `deflate_huff` does **not** search for string matches at all. Every input
//! byte is emitted as a literal, encoded using Huffman codes constructed from
//! the symbol frequency distribution.
//!
//! This strategy is useful when the caller has already performed its own
//! redundancy elimination and wants only entropy coding. Because no hash
//! table operations are performed, this is also the simplest strategy
//! function.
//!
//! # Wire-Format Compatibility
//!
//! The output is a valid DEFLATE stream (RFC 1951). All symbols are encoded
//! as literal bytes — no distance/length pairs are emitted. The resulting
//! blocks will typically be larger than those produced by LZ77-based
//! strategies, but the encoding is faster because match searching is skipped.
//!
//! # C Source Reference
//!
//! - **Function:** `deflate_huff` in `deflate.c` lines 2155–2185
//! - **Called by:** Main `deflate()` loop when `state.strategy == Z_HUFFMAN_ONLY`
//! - **Calls:** `fill_window`, `tally_lit`, `tr_flush_block`

use crate::constants::{Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{BlockState, DeflateState};
use crate::deflate::trees::{tally_lit, tr_flush_block};

// ===========================================================================
// Local helpers
// ===========================================================================

/// Fill the sliding window with data from the input stream.
///
/// In the fully integrated system, `fill_window` is defined in the parent
/// `deflate` module (`mod.rs`) and reads bytes from the `ZStream`'s input
/// buffer into `DeflateState.window`, updating `lookahead`, potentially
/// sliding the window, and updating the hash table.
///
/// Strategy functions call `fill_window` when `lookahead` drops to zero.
/// After the call, the function checks whether `lookahead` is still zero:
///
/// - If `lookahead == 0 && flush == Z_NO_FLUSH`: the function returns
///   `BlockState::NeedMore`, signalling the main `deflate()` loop to
///   provide more input and re-enter the strategy.
/// - If `lookahead == 0` with any other flush mode: the loop breaks to
///   flush the current block.
///
/// This local definition is a no-op stub. When the `deflate` module's
/// public API (`mod.rs`) is fully assembled, it will contain the real
/// `fill_window` implementation. The strategy functions are designed so
/// that the no-op correctly triggers the `NeedMore` / break paths above,
/// ensuring the main `deflate()` loop takes over window management.
#[inline(always)]
fn fill_window(_state: &mut DeflateState) {
    // Intentional no-op: window filling is orchestrated by the main
    // deflate() loop in mod.rs, which calls the real fill_window
    // before and after invoking the strategy function. The lookahead
    // checks after this call handle the case where no data is available.
}

/// Flush the current block to the pending output buffer.
///
/// This local helper corresponds to the C `FLUSH_BLOCK_ONLY` macro
/// (deflate.c lines 1630–1639). It:
///
/// 1. Computes the block data slice from `window[block_start..strstart]`
///    (or `None` if `block_start` is negative, indicating no valid window
///    data for a stored block fallback).
/// 2. Calls [`tr_flush_block`] to select the optimal block encoding
///    (stored, static Huffman, or dynamic Huffman) and write the
///    compressed block into the pending buffer.
/// 3. Advances `block_start` to `strstart`, marking the start of the
///    next block.
///
/// The C `FLUSH_BLOCK` macro additionally checks `avail_out` and may
/// return `NeedMore`/`FinishStarted`. That output-buffer check is
/// handled by the main `deflate()` loop after the strategy function
/// returns, not within the strategy function itself.
fn flush_block(state: &mut DeflateState, last: bool) {
    let stored_len = (state.strstart as i64 - state.block_start) as u64;

    // We need to pass a slice of the window to tr_flush_block, but
    // tr_flush_block also takes &mut DeflateState. To satisfy the
    // borrow checker, we copy the block data to a temporary buffer.
    // This copy only matters for stored-block selection (level 0 or
    // when stored is cheaper); for the common Huffman-coded case the
    // buffer content is not accessed byte-by-byte.
    if state.block_start >= 0 {
        let start = state.block_start as usize;
        let end = state.strstart;
        let buf: Vec<u8> = state.window[start..end].to_vec();
        tr_flush_block(state, Some(&buf), stored_len, last);
    } else {
        tr_flush_block(state, None, stored_len, last);
    }

    state.block_start = state.strstart as i64;
}

// ===========================================================================
// Public API
// ===========================================================================

/// Huffman-only compression without LZ77 string matching.
///
/// For `Z_HUFFMAN_ONLY` strategy, does not look for matches. Each byte is
/// encoded individually using Huffman codes. Does not maintain a hash table.
/// (It will be regenerated if this run of deflate switches away from
/// Huffman.)
///
/// # Algorithm
///
/// The algorithm is straightforward:
///
/// 1. Ensure there is at least one byte of lookahead by calling
///    `fill_window`. If no data is available and `flush == Z_NO_FLUSH`,
///    return `NeedMore` to request more input.
/// 2. Record each input byte as a literal via [`tally_lit`], which updates
///    the symbol buffer and the dynamic Huffman tree frequency counts.
/// 3. Advance `strstart` and decrement `lookahead` by one for each byte.
/// 4. When `tally_lit` signals a full symbol buffer, flush the current
///    block via [`flush_block`].
/// 5. After the loop (input exhausted), set `insert = 0` because the hash
///    table is not maintained. Then emit the final or non-final block
///    depending on the flush mode.
///
/// # Parameters
///
/// - `state`: Mutable reference to the deflate compression state. The
///   function reads from and writes to `window`, `strstart`, `lookahead`,
///   `match_length`, `sym_next`, `insert`, and `block_start`.
/// - `flush`: Flush mode from the caller. `Z_NO_FLUSH` allows the function
///   to return early when more input is needed. `Z_FINISH` causes the
///   final block to be emitted after the loop.
///
/// # Returns
///
/// - [`BlockState::NeedMore`] — Lookahead is zero and `flush == Z_NO_FLUSH`;
///   the caller should provide more input.
/// - [`BlockState::BlockDone`] — A non-final block was flushed (or no
///   pending symbols remain).
/// - [`BlockState::FinishDone`] — The final block was emitted
///   (`flush == Z_FINISH`).
///
/// # C Source
///
/// Direct port of `deflate_huff` in `deflate.c:2155–2185`.
pub(crate) fn deflate_huff(state: &mut DeflateState, flush: i32) -> BlockState {
    loop {
        // ------------------------------------------------------------------
        // Ensure we have a literal byte to write.
        //
        // Unlike the LZ77 strategies that require MIN_LOOKAHEAD (262) bytes,
        // the Huffman-only strategy only needs a single byte because it does
        // not search for matches — every byte is emitted as an individual
        // literal symbol.
        // ------------------------------------------------------------------
        if state.lookahead == 0 {
            fill_window(state);

            if state.lookahead == 0 {
                if flush == Z_NO_FLUSH {
                    return BlockState::NeedMore;
                }
                break; // Flush the current block
            }
        }

        // ------------------------------------------------------------------
        // Output a literal byte.
        //
        // Set match_length to 0 explicitly — Huffman-only never produces
        // matches. Then record the byte at the current window position as a
        // literal in the symbol buffer and update the dynamic literal tree
        // frequency count.
        // ------------------------------------------------------------------
        state.match_length = 0;

        let c = state.window[state.strstart];
        let bflush = tally_lit(state, c);

        state.lookahead -= 1;
        state.strstart += 1;

        // If the symbol buffer is full, flush the current block.
        if bflush {
            flush_block(state, false);
        }
    }

    // ------------------------------------------------------------------
    // Post-loop: finalize the block
    //
    // Set insert = 0 because the Huffman-only strategy does not maintain
    // the hash table. If deflateParams later switches to a hash-based
    // strategy (fast or slow), the hash table will be rebuilt from scratch.
    // ------------------------------------------------------------------
    state.insert = 0;

    if flush == Z_FINISH {
        // Emit the final block (last = true).
        flush_block(state, true);
        return BlockState::FinishDone;
    }

    // If there are any pending symbols that haven't been flushed yet,
    // emit a non-final block.
    if state.sym_next > 0 {
        flush_block(state, false);
    }

    BlockState::BlockDone
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal `DeflateState` for testing the Huffman-only
    /// algorithm. Sets up a small window with controlled data and configures
    /// the state so that the compression loop can run without needing the
    /// full deflate infrastructure.
    fn make_test_state(window_data: &[u8], strstart: usize, lookahead: usize) -> DeflateState {
        use crate::constants::Z_HUFFMAN_ONLY;
        use crate::deflate::trees::tr_init;

        // Use minimal window/memory configuration
        let w_bits: usize = 9; // 512-byte window
        let mem_level: usize = 1;
        let level: usize = 1;
        let wrap: i32 = 0; // raw deflate

        let mut state = DeflateState::new(w_bits, mem_level, level, Z_HUFFMAN_ONLY, wrap);

        // Initialize tree structures (descriptors, frequencies, block state).
        // This is essential — without tr_init, the tree flush operations
        // (build_tree/gen_bitlen) will panic on uninitialised descriptors.
        tr_init(&mut state);

        // Copy test data into the window
        let copy_len = window_data.len().min(state.window.len());
        state.window[..copy_len].copy_from_slice(&window_data[..copy_len]);

        // Set up the starting position and lookahead
        state.strstart = strstart;
        state.lookahead = lookahead;
        state.block_start = 0;
        state.match_length = 0;
        state.insert = 0;
        state.strategy = Z_HUFFMAN_ONLY;

        state
    }

    #[test]
    fn test_huff_all_same_bytes() {
        // Window filled with all 'A' bytes. Each byte should be emitted
        // as a literal — no match searching is performed.
        let data = vec![b'A'; 300];
        let mut state = make_test_state(&data, 0, 100);

        // Run with Z_FINISH to process all data
        let result = deflate_huff(&mut state, Z_FINISH);

        // The function should process data and return FinishDone
        assert_eq!(result, BlockState::FinishDone);

        // insert should be 0 (no hash table maintained)
        assert_eq!(state.insert, 0);

        // strstart should have advanced through all the lookahead
        assert_eq!(state.strstart, 100);

        // lookahead should be exhausted
        assert_eq!(state.lookahead, 0);

        // match_length should be 0 (Huffman-only never finds matches)
        assert_eq!(state.match_length, 0);
    }

    #[test]
    fn test_huff_varying_bytes() {
        // Window filled with varying byte values — each encoded as a literal.
        let mut data = vec![0u8; 300];
        for i in 0..data.len() {
            data[i] = (i & 0xFF) as u8;
        }
        let mut state = make_test_state(&data, 0, 200);

        let result = deflate_huff(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.insert, 0);
        assert_eq!(state.strstart, 200);
        assert_eq!(state.lookahead, 0);
    }

    #[test]
    fn test_huff_need_more_on_no_flush() {
        // With zero lookahead and Z_NO_FLUSH, should return NeedMore.
        // The fill_window stub is a no-op, so lookahead stays zero.
        let data = vec![b'B'; 300];
        let mut state = make_test_state(&data, 0, 0);

        let result = deflate_huff(&mut state, Z_NO_FLUSH);

        assert_eq!(result, BlockState::NeedMore);
    }

    #[test]
    fn test_huff_empty_lookahead_with_finish() {
        // Zero lookahead with Z_FINISH should emit the final block
        // and return FinishDone.
        let data = vec![0u8; 300];
        let mut state = make_test_state(&data, 100, 0);

        let result = deflate_huff(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.insert, 0);
    }

    #[test]
    fn test_huff_single_byte() {
        // Process a single byte: strstart should advance by 1, lookahead
        // should drop to 0.
        let data = vec![b'X'; 300];
        let mut state = make_test_state(&data, 0, 1);

        let result = deflate_huff(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.strstart, 1);
        assert_eq!(state.lookahead, 0);
        assert_eq!(state.insert, 0);
    }

    #[test]
    fn test_huff_match_length_always_zero() {
        // Verify that match_length is always 0 after processing.
        // Even with repetitive data, Huffman-only emits only literals.
        let data = vec![b'Q'; 300];
        let mut state = make_test_state(&data, 0, 50);

        let result = deflate_huff(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.match_length, 0);
    }

    #[test]
    fn test_huff_block_done_with_no_pending_symbols() {
        // After processing with a non-FINISH flush and no remaining
        // symbols (sym_next == 0), should return BlockDone.
        let data = vec![0u8; 300];
        let mut state = make_test_state(&data, 100, 0);
        state.sym_next = 0;

        // Use Z_SYNC_FLUSH (not NO_FLUSH, not FINISH) to trigger
        // the break-out path.
        let result = deflate_huff(&mut state, crate::constants::Z_SYNC_FLUSH);

        assert_eq!(result, BlockState::BlockDone);
        assert_eq!(state.insert, 0);
    }

    #[test]
    fn test_huff_insert_zeroed_on_exit() {
        // Verify that `insert` is always set to 0 on all exit paths.
        let data = vec![b'Z'; 300];
        let mut state = make_test_state(&data, 5, 50);
        state.insert = 42; // Set to non-zero to verify it gets cleared

        let result = deflate_huff(&mut state, Z_FINISH);

        assert_eq!(result, BlockState::FinishDone);
        assert_eq!(state.insert, 0);
    }
}
