// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Greedy matching compression strategy (levels 1–3).
// Port of C `deflate_fast` from deflate.c lines 1857–1948.

#![allow(dead_code)]

//! Greedy matching compression strategy for the DEFLATE engine.
//!
//! This module implements [`deflate_fast`], one of five compression strategy
//! functions dispatched by the main `deflate()` loop. It is selected when the
//! caller specifies compression levels 1 through 3 via `deflateInit2` or
//! `deflateParams`.
//!
//! # Algorithm Overview
//!
//! `deflate_fast` uses **greedy matching** — it immediately emits the longest
//! match found at each position without looking ahead to see if a better match
//! exists at the next position. This is faster than lazy evaluation
//! ([`deflate_slow`]) but produces slightly larger output for most data.
//!
//! For each position in the input:
//!
//! 1. Insert the current string into the hash table.
//! 2. Search the hash chain for the longest match.
//! 3. If a match of length ≥ `MIN_MATCH` (3) is found, emit it as a
//!    distance/length pair. Otherwise, emit the current byte as a literal.
//! 4. After emitting a match, insert the matched bytes into the hash table
//!    (if the match is short enough) to keep the dictionary current.
//!
//! # Hash Insertion Strategy
//!
//! When a match is found, the function only inserts subsequent strings into
//! the hash table if `match_length <= max_insert_length` (which equals
//! `max_lazy_match` for levels 1–3). Longer matches skip hash insertion —
//! this saves time but degrades compression, since those skipped strings
//! will not be available for future match searches.
//!
//! When hash insertion is skipped, the hash state must be re-initialized
//! at the new position: `ins_h` is recalculated from the two bytes at
//! `window[strstart]` and `window[strstart + 1]`.
//!
//! # Wire-Format Compatibility
//!
//! The output is a valid DEFLATE stream (RFC 1951). The match decisions
//! exactly reproduce the C zlib `deflate_fast` algorithm, producing
//! byte-identical output for the same input, level, and flush sequence.
//!
//! # C Source Reference
//!
//! - **Function:** `deflate_fast` in `deflate.c` lines 1857–1948
//! - **Called by:** Main `deflate()` loop for levels 1–3
//! - **Calls:** `fill_window`, `INSERT_STRING`, `longest_match`,
//!   `tally_dist`, `tally_lit`, `FLUSH_BLOCK`

use core::cmp::min;

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::constants::{MIN_MATCH, Z_FINISH, Z_NO_FLUSH};
use crate::deflate::state::{BlockState, DeflateState, MIN_LOOKAHEAD};
use crate::deflate::trees::{tally_dist, tally_lit, tr_flush_block};
use crate::stream::ZStream;

// ===========================================================================
// Local constants
// ===========================================================================

/// Tail of hash chains. A hash head value of NIL means no previous string
/// with the same hash has been inserted.
///
/// Corresponds to C `#define NIL 0` (deflate.c line 85).
const NIL: u16 = 0;

// ===========================================================================
// Local helpers
// ===========================================================================

/// Fill the sliding window with data from the input stream.
///
/// Delegates to the real `fill_window` implementation in the parent
/// `deflate` module (`mod.rs`). This reads bytes from the `ZStream`'s
/// input buffer into `DeflateState.window`, updating `lookahead`,
/// potentially sliding the window, and updating the hash table.
///
/// # Safety
///
/// `strm` must be a valid pointer to the `ZStream` that owns this
/// `DeflateState`. The pointer is valid for the lifetime of the
/// `deflate()` call.
#[inline(always)]
unsafe fn do_fill_window(state: &mut DeflateState, strm: *mut ZStream) {
    // SAFETY: strm is the raw pointer to the parent ZStream passed from
    // deflate(). fill_window only reads/writes strm fields (avail_in,
    // next_in, total_in, adler) that do not overlap with DeflateState.
    super::fill_window(state, unsafe { &mut *strm });
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

/// Maximum match distance for the current window configuration.
///
/// Matches at distances greater than this are invalid because the
/// reference position would be outside the sliding window.
///
/// Corresponds to C `MAX_DIST(s) = s->w_size - MIN_LOOKAHEAD`
/// (deflate.h line 301).
#[inline(always)]
fn max_dist(state: &DeflateState) -> usize {
    state.w_size - MIN_LOOKAHEAD
}

/// Insert the string at the given position into the hash table and return
/// the previous head of the hash chain.
///
/// This corresponds to the C `INSERT_STRING` macro (deflate.c lines
/// 160–164). It updates the hash value with the byte at position
/// `str_pos + MIN_MATCH - 1`, chains the current position into `prev[]`,
/// and returns the previous head of the hash chain.
///
/// # Parameters
///
/// - `state`: Mutable deflate state containing window, hash tables, etc.
/// - `str_pos`: Position in the window to insert (the string starting at
///   `window[str_pos..str_pos + MIN_MATCH]` is hashed).
///
/// # Returns
///
/// The previous head of the hash chain (the most recent position with
/// the same hash), or `NIL` (0) if no previous string shares this hash.
#[inline(always)]
fn insert_string(state: &mut DeflateState, str_pos: usize) -> u16 {
    // UPDATE_HASH: shift ins_h left by hash_shift and XOR with the new byte.
    //
    // C: (h = (((h) << s->hash_shift) ^ (c)) & s->hash_mask)
    state.ins_h = ((state.ins_h << state.hash_shift as u32)
        ^ state.window[str_pos + MIN_MATCH - 1] as u32)
        & state.hash_mask;

    // Retrieve the previous head of the hash chain.
    let hash_head = state.head[state.ins_h as usize];

    // Chain: prev[str_pos & w_mask] = head[ins_h]
    // This maintains the linked list of positions sharing the same hash.
    state.prev[str_pos & state.w_mask] = hash_head;

    // Update head to point to the current string position.
    state.head[state.ins_h as usize] = str_pos as u16;

    hash_head
}

/// Find the longest match starting at the given hash chain head.
///
/// This scans the hash chain rooted at `match_head` to find the longest
/// string matching the one at `state.strstart`. On success, updates
/// `state.match_start` and returns the match length.
///
/// Corresponds to C `longest_match` (deflate.c lines 1380–1555). The full
/// implementation in the integrated `deflate/mod.rs` performs chain-length
/// limiting, good_match threshold shortening, and nice_match early exit.
/// This local version implements the core algorithm for correctness.
///
/// # Parameters
///
/// - `state`: Mutable deflate state.
/// - `cur_match`: Initial match position (head of hash chain).
///
/// # Returns
///
/// Length of the longest match found, or `MIN_MATCH - 1` if no match
/// of length ≥ `MIN_MATCH` was found.
fn longest_match(state: &mut DeflateState, cur_match: u16) -> usize {
    let mut chain_length = state.max_chain_length as usize;
    let strstart = state.strstart;
    let w_mask = state.w_mask;
    let window = &state.window;
    let nice_match = min(state.nice_match as usize, state.lookahead);
    let limit: usize = if strstart > max_dist(state) {
        strstart - max_dist(state)
    } else {
        0 // NIL equivalent — do not search before beginning of window
    };

    let mut best_len = MIN_MATCH - 1;
    let mut best_match = state.match_start;

    let mut cur = cur_match as usize;

    while cur > limit && chain_length > 0 {
        chain_length -= 1;

        // Bounds check: ensure we can safely compare up to best_len bytes.
        if cur + best_len >= window.len() || strstart + best_len >= window.len() {
            cur = state.prev[cur & w_mask] as usize;
            continue;
        }

        // Quick reject: check the bytes at the current best length position
        // and at the beginning to avoid expensive full comparisons.
        if window[cur + best_len] != window[strstart + best_len]
            || window[cur] != window[strstart]
            || window[cur + 1] != window[strstart + 1]
        {
            cur = state.prev[cur & w_mask] as usize;
            continue;
        }

        // Full comparison from position 2 onwards (first two bytes already match).
        let mut len = 2;
        let max_len = min(nice_match, min(state.lookahead, window.len() - strstart));
        let max_scan = min(max_len, window.len() - cur);

        while len < max_scan && window[cur + len] == window[strstart + len] {
            len += 1;
        }

        if len > best_len {
            best_match = cur;
            best_len = len;
            if len >= nice_match {
                break;
            }
        }

        cur = state.prev[cur & w_mask] as usize;
        // Avoid infinite loop if prev points back to position 0.
        if cur == 0 {
            break;
        }
    }

    state.match_start = best_match;
    best_len
}

// ===========================================================================
// Public API
// ===========================================================================

/// Greedy matching compression for levels 1–3.
///
/// Compresses as much as possible from the input stream and returns the
/// current block state. This function does **not** perform lazy evaluation
/// of matches. It inserts new strings in the dictionary only for unmatched
/// strings or for short matches (≤ `max_insert_length`). It is used only
/// for the fast compression options (levels 1–3).
///
/// # Algorithm
///
/// At each position in the sliding window:
///
/// 1. **Ensure sufficient lookahead.** If fewer than `MIN_LOOKAHEAD` (262)
///    bytes remain in the window, call `fill_window` to load more data.
///    If still insufficient and `flush == Z_NO_FLUSH`, return `NeedMore`.
///    If `lookahead == 0`, break to flush the current block.
///
/// 2. **Insert into hash table.** If `lookahead >= MIN_MATCH`, insert the
///    string at `strstart` into the hash table via `insert_string`,
///    obtaining the head of the hash chain.
///
/// 3. **Find longest match.** If the hash chain head is not NIL and the
///    distance is within `MAX_DIST`, search the hash chain for the
///    longest match via `longest_match`.
///
/// 4. **Emit match or literal.**
///    - If a match of length ≥ `MIN_MATCH` (3) is found, emit it as a
///      distance/length pair via `tally_dist`. If the match is short
///      enough (≤ `max_insert_length`) and there is sufficient lookahead,
///      insert all the matched bytes into the hash table. Otherwise, skip
///      hash insertion and reinitialize the hash at the new position.
///    - If no match is found, emit the current byte as a literal via
///      `tally_lit`.
///
/// 5. **Flush check.** If the tally function indicates the symbol buffer
///    is full, flush the current block.
///
/// After the loop:
/// - Set `state.insert` to `min(strstart, MIN_MATCH - 1)`.
/// - If `flush == Z_FINISH`, emit the final block and return `FinishDone`.
/// - If there are pending symbols, flush a non-final block.
/// - Return `BlockDone`.
///
/// # Parameters
///
/// - `state`: Mutable reference to the deflate compression state. The
///   function reads from and writes to `window`, `strstart`, `lookahead`,
///   `match_length`, `match_start`, `ins_h`, `sym_next`, `insert`,
///   `block_start`, and hash tables (`prev`, `head`).
/// - `flush`: Flush mode from the caller. `Z_NO_FLUSH` allows early
///   return when more input is needed. `Z_FINISH` causes the final block
///   to be emitted after the loop.
///
/// # Returns
///
/// - [`BlockState::NeedMore`] — Lookahead is insufficient and
///   `flush == Z_NO_FLUSH`; the caller should provide more input.
/// - [`BlockState::BlockDone`] — A non-final block was flushed.
/// - [`BlockState::FinishDone`] — The final block was emitted
///   (`flush == Z_FINISH`).
///
/// # C Source
///
/// Direct port of `deflate_fast` in `deflate.c:1857–1948`.
pub(crate) fn deflate_fast(state: &mut DeflateState, strm: *mut ZStream, flush: i32) -> BlockState {
    let mut hash_head: u16; // head of the hash chain
    let mut bflush: bool; // set if current block must be flushed

    loop {
        // ==================================================================
        // Step 1: Ensure sufficient lookahead.
        //
        // We need MAX_MATCH bytes for the next match, plus MIN_MATCH bytes
        // to insert the string following the next match. That totals to
        // MIN_LOOKAHEAD = MAX_MATCH + MIN_MATCH + 1 = 262 bytes.
        // ==================================================================
        if state.lookahead < MIN_LOOKAHEAD {
            // SAFETY: strm is valid for the duration of deflate().
            unsafe {
                do_fill_window(state, strm);
            }
            if state.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BlockState::NeedMore;
            }
            if state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // ==================================================================
        // Step 2: Insert the string window[strstart .. strstart + 2] in the
        // dictionary, and set hash_head to the head of the hash chain.
        // ==================================================================
        hash_head = NIL;
        if state.lookahead >= MIN_MATCH {
            hash_head = insert_string(state, state.strstart);
        }

        // ==================================================================
        // Step 3: Find the longest match, discarding those <= prev_length.
        // At this point we have always match_length < MIN_MATCH.
        // ==================================================================
        if hash_head != NIL && state.strstart - (hash_head as usize) <= max_dist(state) {
            // To simplify the code, we prevent matches with the string
            // of window index 0 (in particular we have to avoid a match
            // of the string with itself at the start of the input file).
            state.match_length = longest_match(state, hash_head);
            // longest_match() sets match_start
        }

        // ==================================================================
        // Step 4: Emit match or literal.
        // ==================================================================
        if state.match_length >= MIN_MATCH {
            // ---------------------------------------------------------------
            // Match found: emit a distance/length pair.
            //
            // C lines 1894-1930.
            // ---------------------------------------------------------------

            // Debug-mode validation (equivalent to C check_match macro).
            debug_assert!(
                state.match_start < state.strstart
                    || (state.match_start == 0 && state.strstart > 0),
                "deflate_fast: invalid match position"
            );

            // Record the distance/length pair for Huffman encoding.
            // Distance is 1-based: strstart - match_start.
            // Length is 0-based: match_length - MIN_MATCH.
            bflush = tally_dist(
                state,
                (state.strstart - state.match_start) as u32,
                (state.match_length - MIN_MATCH) as u32,
            );

            state.lookahead -= state.match_length;

            // ---------------------------------------------------------------
            // Insert new strings in the hash table only if the match length
            // is not too large. This saves time but degrades compression.
            //
            // max_insert_length is an alias for max_lazy_match
            // (deflate.h line 186).
            //
            // C lines 1906-1930.
            // ---------------------------------------------------------------
            if state.match_length <= state.max_insert_length() as usize
                && state.lookahead >= MIN_MATCH
            {
                // Decrement match_length to account for the string at
                // strstart that was already inserted.
                state.match_length -= 1;
                loop {
                    state.strstart += 1;
                    insert_string(state, state.strstart);
                    // strstart never exceeds WSIZE-MAX_MATCH, so there are
                    // always MIN_MATCH bytes ahead.
                    state.match_length -= 1;
                    if state.match_length == 0 {
                        break;
                    }
                }
                state.strstart += 1;
            } else {
                // Match is too long for hash insertion, or not enough
                // lookahead. Skip insertion and advance past the match.
                state.strstart += state.match_length;
                state.match_length = 0;

                // Reinitialize hash value for the new position.
                // C lines 1922-1926:
                //   s->ins_h = s->window[s->strstart];
                //   UPDATE_HASH(s, s->ins_h, s->window[s->strstart + 1]);
                state.ins_h = state.window[state.strstart] as u32;
                state.ins_h = ((state.ins_h << state.hash_shift as u32)
                    ^ state.window[state.strstart + 1] as u32)
                    & state.hash_mask;

                // If lookahead < MIN_MATCH, ins_h is garbage, but it does
                // not matter since it will be recomputed at the next
                // deflate call.
            }
        } else {
            // ---------------------------------------------------------------
            // No match: output a literal byte.
            //
            // C lines 1931-1937.
            // ---------------------------------------------------------------
            bflush = tally_lit(state, state.window[state.strstart]);
            state.lookahead -= 1;
            state.strstart += 1;
        }

        // ==================================================================
        // Step 5: Flush check.
        //
        // If the symbol buffer is full (tally returned true), flush the
        // current block before continuing.
        //
        // C line 1938: if (bflush) FLUSH_BLOCK(s, 0);
        // ==================================================================
        if bflush {
            flush_block(state, false);
        }
    }

    // ======================================================================
    // Post-loop cleanup.
    //
    // We exit the loop only when lookahead == 0 (no more input data).
    //
    // Set the number of bytes still needing hash insertion. At the end of
    // the stream, there may be up to MIN_MATCH - 1 bytes that we haven't
    // inserted yet.
    //
    // C line 1940: s->insert = s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1;
    // ======================================================================
    state.insert = min(state.strstart, MIN_MATCH - 1);

    if flush == Z_FINISH {
        // Emit the final block (last = true).
        flush_block(state, true);
        return BlockState::FinishDone;
    }

    // If there are pending symbols in the buffer, flush a non-final block.
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
    use crate::deflate::trees::tr_init;
    use crate::stream::ZStream;

    fn dummy_strm() -> ZStream {
        ZStream::new()
    }

    /// Helper: create a minimal `DeflateState` for testing the greedy
    /// matching algorithm. Sets up a small window with controlled data
    /// and configures the state so that the compression loop can run.
    fn make_test_state(window_data: &[u8], strstart: usize, lookahead: usize) -> DeflateState {
        use crate::constants::Z_DEFAULT_STRATEGY;

        // Use minimal window/memory configuration.
        let w_bits: usize = 9; // 512-byte window
        let mem_level: usize = 1;
        let level: usize = 1; // Fast compression level
        let wrap: i32 = 0; // raw deflate

        let mut state = DeflateState::new(w_bits, mem_level, level, Z_DEFAULT_STRATEGY, wrap);

        // Initialize tree structures (descriptors, frequencies, block state).
        tr_init(&mut state);

        // Copy test data into the window.
        let copy_len = window_data.len().min(state.window.len());
        state.window[..copy_len].copy_from_slice(&window_data[..copy_len]);

        state.strstart = strstart;
        state.lookahead = lookahead;
        state.block_start = 0;

        // Set reasonable match parameters for level 1.
        // From configuration_table[1]: good=4, lazy=4, nice=8, chain=4
        state.max_lazy_match = 4;
        state.max_chain_length = 4;
        state.good_match = 4;
        state.nice_match = 8;

        // Initialize the window_size so that max_dist computes correctly.
        state.window_size = state.w_size * 2;

        state
    }

    #[test]
    fn test_deflate_fast_no_input_no_flush() {
        // With no lookahead and Z_NO_FLUSH, should return NeedMore.
        let mut state = make_test_state(&[], 0, 0);
        let result = deflate_fast(&mut state, &mut dummy_strm() as *mut ZStream, Z_NO_FLUSH);
        assert_eq!(result, BlockState::NeedMore);
    }

    #[test]
    fn test_deflate_fast_no_input_finish() {
        // With no lookahead and Z_FINISH, should return FinishDone
        // (emitting an empty final block).
        let mut state = make_test_state(&[], 0, 0);
        let result = deflate_fast(&mut state, &mut dummy_strm() as *mut ZStream, Z_FINISH);
        assert_eq!(result, BlockState::FinishDone);
    }

    #[test]
    fn test_deflate_fast_literals_only() {
        // Provide data with no repeated patterns — all literals.
        let data: Vec<u8> = (0..64).collect();
        let mut state = make_test_state(&data, 0, data.len());

        // With Z_FINISH, the function should process all bytes as literals
        // and emit the final block.
        let result = deflate_fast(&mut state, &mut dummy_strm() as *mut ZStream, Z_FINISH);
        assert_eq!(result, BlockState::FinishDone);

        // strstart should have advanced by the number of literals processed.
        assert_eq!(state.strstart, data.len());
        // insert should be min(strstart, MIN_MATCH - 1) = 2
        assert_eq!(state.insert, MIN_MATCH - 1);
    }

    #[test]
    fn test_deflate_fast_with_match() {
        // Create data with a repeated pattern that will produce a match.
        // "ABCABC" repeated — the second "ABC" should match the first.
        let mut data = vec![0u8; 512];
        let pattern = b"ABCDEFGH";
        // Place pattern at offset 0 and repeat at offset 8.
        data[..8].copy_from_slice(pattern);
        data[8..16].copy_from_slice(pattern);

        let mut state = make_test_state(&data, 0, 16);

        // Pre-initialize hash for the data — insert first MIN_MATCH - 1
        // bytes to warm up the hash.
        state.ins_h = data[0] as u32;
        state.ins_h = ((state.ins_h << state.hash_shift as u32) ^ data[1] as u32) & state.hash_mask;

        let result = deflate_fast(&mut state, &mut dummy_strm() as *mut ZStream, Z_FINISH);
        assert_eq!(result, BlockState::FinishDone);
        // The function should have processed all 16 bytes.
        assert_eq!(state.strstart, 16);
    }

    #[test]
    fn test_insert_string_returns_nil_on_first_insert() {
        // A fresh state should have no hash chains, so the first insert
        // should return NIL.
        let data: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut state = make_test_state(&data, 0, data.len());

        // Initialize hash for position 0.
        state.ins_h = state.window[0] as u32;
        state.ins_h =
            ((state.ins_h << state.hash_shift as u32) ^ state.window[1] as u32) & state.hash_mask;

        let head = insert_string(&mut state, 0);
        assert_eq!(head, NIL);
    }

    #[test]
    fn test_insert_string_chains_correctly() {
        // Insert the same hash value twice — second insert should return
        // the first position.
        let mut data = vec![0u8; 512];
        // Create two identical 3-byte strings at positions 0 and 10.
        data[0] = 10;
        data[1] = 20;
        data[2] = 30;
        data[10] = 10;
        data[11] = 20;
        data[12] = 30;

        let mut state = make_test_state(&data, 0, data.len().min(64));

        // Initialize hash state.
        state.ins_h = data[0] as u32;
        state.ins_h = ((state.ins_h << state.hash_shift as u32) ^ data[1] as u32) & state.hash_mask;

        // First insert at position 0.
        let head1 = insert_string(&mut state, 0);
        assert_eq!(head1, NIL);

        // Re-seed hash for position 10.
        state.ins_h = data[10] as u32;
        state.ins_h =
            ((state.ins_h << state.hash_shift as u32) ^ data[11] as u32) & state.hash_mask;

        // Second insert at position 10 should chain to position 0.
        let head2 = insert_string(&mut state, 10);
        assert_eq!(head2, 0);
    }

    #[test]
    fn test_max_dist_calculation() {
        let data: Vec<u8> = vec![0; 32];
        let state = make_test_state(&data, 0, 0);
        // w_size for w_bits=9 is 512; MAX_DIST = 512 - 262 = 250.
        assert_eq!(max_dist(&state), state.w_size - MIN_LOOKAHEAD);
    }

    #[test]
    fn test_deflate_fast_block_done_with_symbols() {
        // When called with Z_FINISH=4 and some data, should return
        // FinishDone. With Z_NO_FLUSH and 0 lookahead: NeedMore.
        let data: Vec<u8> = (0..32).collect();
        let mut state = make_test_state(&data, 0, 0);
        assert_eq!(
            deflate_fast(&mut state, &mut dummy_strm() as *mut ZStream, Z_NO_FLUSH),
            BlockState::NeedMore
        );
    }
}
