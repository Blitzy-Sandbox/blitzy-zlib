//! Hash chain management, window filling, and longest-match search.
//!
//! Ports the LZ77 dictionary matching engine from `deflate.c`:
//!
//! - **Hash table operations** — [`update_hash`], [`insert_string`],
//!   [`clear_hash`], [`slide_hash`] manage the rolling hash used to locate
//!   repeated byte sequences in the sliding window.
//! - **Input reading** — [`read_buf`] is the sole input path; every byte that
//!   enters the deflate engine passes through this function (with optional
//!   checksum update).
//! - **Window management** — [`fill_window`] keeps the sliding window populated,
//!   triggering a slide when the upper half overflows and maintaining the high
//!   water mark for safe `longest_match` scanning.
//! - **String matching** — [`longest_match`] traverses the hash chain to find
//!   the longest matching string in the dictionary, returning the match length
//!   so that the caller can decide between emitting a literal or a
//!   length-distance pair.
//!
//! # Safety
//!
//! This module contains **zero** `unsafe` blocks. All C pointer arithmetic has
//! been replaced with safe Rust slice indexing and the `copy_within` method for
//! in-place window sliding.
//!
//! # Source References
//!
//! | Function          | C Source                          |
//! |-------------------|-----------------------------------|
//! | `NIL`             | `deflate.c` line 85               |
//! | `update_hash`     | `deflate.c` line 141 (macro)      |
//! | `insert_string`   | `deflate.c` lines 160–164 (macro) |
//! | `clear_hash`      | `deflate.c` lines 170–175 (macro) |
//! | `slide_hash`      | `deflate.c` lines 187–210         |
//! | `read_buf`        | `deflate.c` lines 219–240         |
//! | `fill_window`     | `deflate.c` lines 252–376         |
//! | `longest_match`   | `deflate.c` lines 1389–1530       |

use crate::checksum::adler32::adler32;
use crate::checksum::crc32::crc32;
use crate::constants::{MAX_MATCH, MIN_MATCH};
use crate::stream::ZStream;

use super::state::DeflateState;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Sentinel value marking the end of a hash chain.
///
/// A head or prev entry equal to `NIL` indicates "no match found" — the
/// chain has been exhausted. In C zlib this is `#define NIL 0`
/// (`deflate.c` line 85).
pub(crate) const NIL: u16 = 0;

/// Distance threshold for discarding short (3-byte) matches.
///
/// Matches of exactly `MIN_MATCH` (3) bytes are discarded when the distance
/// between the current position and the match exceeds `TOO_FAR`, because
/// the encoding overhead outweighs the savings. From `deflate.c` line 91.
const TOO_FAR: usize = 4096;

/// Minimum amount of lookahead, except at the end of the input.
///
/// Defined as `MAX_MATCH + MIN_MATCH + 1` in `deflate.h` line 296. The
/// deflate engine calls [`fill_window`] whenever `lookahead` drops below
/// this threshold.
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes after the end of current data to zero-initialize in the
/// window, preventing uninitialised memory reads by [`longest_match`].
///
/// Set to [`MAX_MATCH`] (258) per `deflate.h` line 306.
const WIN_INIT: usize = MAX_MATCH;

// ─── Hash Computation ─────────────────────────────────────────────────────────

/// Update a rolling hash value with one input byte.
///
/// Implements the `UPDATE_HASH` macro from `deflate.c` line 141:
///
/// ```text
/// h = ((h << hash_shift) ^ c) & hash_mask
/// ```
///
/// The hash is incrementally computed from consecutive input bytes so that
/// after `MIN_MATCH` steps the oldest byte no longer participates in the
/// hash key.
///
/// # Parameters
///
/// * `state` — Provides `hash_shift` and `hash_mask`.
/// * `h` — Current hash accumulator value.
/// * `c` — Next input byte to fold into the hash.
///
/// # Returns
///
/// The updated hash value, guaranteed to be in `0..hash_size`.
#[inline]
pub(crate) fn update_hash(state: &DeflateState, h: u32, c: u8) -> u32 {
    ((h << state.hash_shift) ^ u32::from(c)) & state.hash_mask
}

// ─── Hash Chain Insertion ─────────────────────────────────────────────────────

/// Insert the string starting at `str_pos` into the hash table and return the
/// head of the previous chain with the same hash value.
///
/// Ports the non-`FASTEST` `INSERT_STRING` macro from `deflate.c`
/// lines 160–164. The algorithm:
///
/// 1. Updates `state.ins_h` by hashing the byte at `str_pos + MIN_MATCH - 1`.
/// 2. Links the current position into the `prev` chain for this hash bucket.
/// 3. Stores the current position as the new head of the bucket.
/// 4. Returns the **previous** head (the most recent earlier position with
///    the same hash), which is the starting point for [`longest_match`].
///
/// # Parameters
///
/// * `state` — Mutable deflate state (updates `ins_h`, `prev`, `head`).
/// * `str_pos` — Window position of the string to insert.
///
/// # Returns
///
/// The previous head of the hash chain (may be [`NIL`] if the bucket was
/// empty).
#[inline]
pub(crate) fn insert_string(state: &mut DeflateState, str_pos: usize) -> u16 {
    // Update the running hash with the third byte of the trigram
    // (positions 0 and 1 were already folded in by prior calls).
    state.ins_h = update_hash(
        state,
        state.ins_h,
        state.window[str_pos + MIN_MATCH - 1],
    );

    let hash_idx = state.ins_h as usize;

    // Retrieve the current head — this is the start of the chain we
    // will search in longest_match().
    let match_head = state.head[hash_idx];

    // Link: prev[str_pos & w_mask] = old head
    state.prev[str_pos & state.w_mask] = match_head;

    // This position becomes the new head.
    state.head[hash_idx] = str_pos as u16;

    match_head
}

// ─── Hash Table Clearing ──────────────────────────────────────────────────────

/// Zero every entry in the hash table and reset the `slid` flag.
///
/// Ports the `CLEAR_HASH` macro from `deflate.c` lines 170–175. Called by
/// `lm_init()` during deflate reset to prepare the hash table for a fresh
/// compression session. The `prev` array is not cleared because it will be
/// lazily overwritten as new strings are inserted.
pub(crate) fn clear_hash(state: &mut DeflateState) {
    // Fill all head entries with NIL (0).
    state.head[..state.hash_size].fill(NIL);
    state.slid = false;
}

// ─── Hash Table Sliding ───────────────────────────────────────────────────────

/// Adjust all hash chain pointers after the sliding window has moved down
/// by `w_size` positions.
///
/// Ports `slide_hash()` from `deflate.c` lines 187–210. For each entry in
/// `head[0..hash_size]` and `prev[0..w_size]`:
///
/// * If the stored position ≥ `w_size`, subtract `w_size` to reflect the
///   new window origin.
/// * Otherwise set the entry to [`NIL`] — the referenced position has
///   slid out of the window and is no longer reachable.
///
/// The function also sets `state.slid = true` so that `deflateParams` can
/// detect whether the hash table contents are stale.
pub(crate) fn slide_hash(state: &mut DeflateState) {
    let wsize = state.w_size as u16;

    for entry in state.head[..state.hash_size].iter_mut() {
        *entry = if *entry >= wsize {
            *entry - wsize
        } else {
            NIL
        };
    }

    for entry in state.prev[..state.w_size].iter_mut() {
        *entry = if *entry >= wsize {
            *entry - wsize
        } else {
            NIL
        };
    }

    state.slid = true;
}

// ─── Input Reading ────────────────────────────────────────────────────────────

/// Read input bytes from the stream into `buf`, updating the running
/// checksum if a wrapper format is active.
///
/// Ports `read_buf()` from `deflate.c` lines 219–240. This is the **sole
/// input path** — every byte that enters the deflate engine passes through
/// this function.
///
/// # Algorithm
///
/// 1. Determine `len = min(stream.avail_in, buf.len())`.
/// 2. Copy `len` bytes from the stream's input buffer into `buf`.
/// 3. Update the running checksum based on `wrap`:
///    - `1` (zlib format): Adler-32 via [`adler32`].
///    - `2` (gzip format): CRC-32 via [`crc32`].
///    - `0` or other: no checksum update.
/// 4. Advance the stream's input position and increment `total_in`.
///
/// # Parameters
///
/// * `stream` — The compression stream providing input data.
/// * `buf` — Destination slice (typically a region of the sliding window).
/// * `wrap` — Wrapper format selector copied from `DeflateState::wrap` to
///   avoid simultaneous mutable borrows of state and stream.
///
/// # Returns
///
/// The number of bytes actually read (may be zero if no input is available).
pub(crate) fn read_buf(stream: &mut ZStream, buf: &mut [u8], wrap: i32) -> usize {
    let avail = stream.avail_in();
    let len = avail.min(buf.len());
    if len == 0 {
        return 0;
    }

    // Copy input data into the destination buffer.
    let src = &stream.input_remaining()[..len];
    buf[..len].copy_from_slice(src);

    // Update running checksum based on wrapper format.
    if wrap == 1 {
        stream.adler = adler32(stream.adler, &buf[..len]);
    } else if wrap == 2 {
        stream.adler = crc32(stream.adler, &buf[..len]);
    }

    // Advance stream position — this also increments total_in.
    // We've verified len <= avail_in above, so this cannot fail.
    let _ = stream.advance_input(len);

    len
}

// ─── Window Filling ───────────────────────────────────────────────────────────

/// Fill the sliding window with input data until the lookahead reaches
/// [`MIN_LOOKAHEAD`] or the input is exhausted.
///
/// Ports `fill_window()` from `deflate.c` lines 252–376. This is a
/// critical function called by every compression strategy function
/// (`deflate_stored`, `deflate_fast`, `deflate_slow`, `deflate_rle`,
/// `deflate_huff`).
///
/// # Algorithm Overview
///
/// 1. **Main loop** — while `lookahead < MIN_LOOKAHEAD` and input remains:
///    a. Compute free space at the window tail.
///    b. If `strstart` has advanced past `w_size + MAX_DIST`, slide the
///       upper window half down and adjust all position fields.
///    c. Read input via [`read_buf`] into the window.
///    d. Insert newly-available bytes into the hash table for strings
///       that are pending insertion.
///
/// 2. **High-water-mark zeroing** — zero uninitialised bytes ahead of
///    current data up to `WIN_INIT` bytes, preventing
///    [`longest_match`] from reading garbage and tripping memory sanitisers.
///
/// # Panics
///
/// Debug-mode assertion fires if the post-condition
/// `strstart <= window_size − MIN_LOOKAHEAD` is violated.
pub(crate) fn fill_window(state: &mut DeflateState, stream: &mut ZStream) {
    let wsize = state.w_size;

    debug_assert!(
        state.lookahead < MIN_LOOKAHEAD,
        "fill_window called with sufficient lookahead"
    );

    loop {
        // ── 1. Free space at the window tail ────────────────────────────
        let mut more = state.window_size - state.lookahead - state.strstart;

        // ── 2. Window sliding ───────────────────────────────────────────
        // MAX_DIST = w_size - MIN_LOOKAHEAD
        let max_dist = wsize.saturating_sub(MIN_LOOKAHEAD);
        if state.strstart >= wsize + max_dist {
            // Move the upper half of the window to the lower half.
            // The number of valid bytes in the upper half is
            // strstart + lookahead - wsize.
            let copy_len = wsize.saturating_sub(more);
            state.window.copy_within(wsize..wsize + copy_len, 0);

            if state.match_start >= wsize {
                state.match_start -= wsize;
            } else {
                state.match_start = 0;
            }
            state.strstart -= wsize;
            state.block_start -= wsize as i64;

            if state.insert > state.strstart {
                state.insert = state.strstart;
            }

            slide_hash(state);
            more += wsize;
        }

        // ── 3. Break if no input remains ────────────────────────────────
        if stream.avail_in() == 0 {
            break;
        }

        // ── 4. Read input into the window ───────────────────────────────
        debug_assert!(more >= 2, "fill_window: more < 2");

        let wrap = state.wrap;
        let offset = state.strstart + state.lookahead;
        let n = read_buf(stream, &mut state.window[offset..offset + more], wrap);
        state.lookahead += n;

        // ── 5. Hash insertion for pending bytes ─────────────────────────
        // If we have enough bytes, initialise or continue the hash for
        // positions that haven't been inserted yet.
        if state.lookahead + state.insert >= MIN_MATCH {
            let mut str_idx = state.strstart.saturating_sub(state.insert);

            // Bootstrap: seed ins_h with the first two bytes.
            state.ins_h = u32::from(state.window[str_idx]);
            state.ins_h = update_hash(state, state.ins_h, state.window[str_idx + 1]);

            while state.insert > 0 {
                state.ins_h = update_hash(
                    state,
                    state.ins_h,
                    state.window[str_idx + MIN_MATCH - 1],
                );

                // Link into prev chain (non-FASTEST path).
                let hash_idx = state.ins_h as usize;
                state.prev[str_idx & state.w_mask] = state.head[hash_idx];
                state.head[hash_idx] = str_idx as u16;

                str_idx += 1;
                state.insert -= 1;

                if state.lookahead + state.insert < MIN_MATCH {
                    break;
                }
            }
        }

        // ── 6. Loop condition ───────────────────────────────────────────
        if state.lookahead >= MIN_LOOKAHEAD || stream.avail_in() == 0 {
            break;
        }
    }

    // ── 7. High-water-mark zeroing ──────────────────────────────────────
    // Zero uninitialised bytes ahead of the current data so that
    // longest_match cannot read garbage beyond strstart + lookahead.
    if state.high_water < state.window_size {
        let curr = state.strstart + state.lookahead;

        if state.high_water < curr {
            // Previous high water below current data — zero WIN_INIT
            // bytes or up to the end of the window, whichever is less.
            let init = (state.window_size - curr).min(WIN_INIT);
            for byte in &mut state.window[curr..curr + init] {
                *byte = 0;
            }
            state.high_water = curr + init;
        } else if state.high_water < curr + WIN_INIT {
            // High water at or above current data but below
            // current data + WIN_INIT — extend the zeroed region.
            let init =
                (curr + WIN_INIT - state.high_water).min(state.window_size - state.high_water);
            for byte in &mut state.window[state.high_water..state.high_water + init] {
                *byte = 0;
            }
            state.high_water += init;
        }
    }

    debug_assert!(
        state.strstart <= state.window_size.saturating_sub(MIN_LOOKAHEAD)
            || stream.avail_in() == 0,
        "fill_window: not enough room for search"
    );
}

// ─── Longest Match ────────────────────────────────────────────────────────────

/// Find the longest matching string in the dictionary for the string
/// starting at `state.strstart`.
///
/// Ports the standard (non-`FASTEST`, non-`UNALIGNED_OK`) `longest_match()`
/// from `deflate.c` lines 1389–1530. This is the performance-critical
/// inner routine of the deflate engine.
///
/// # Algorithm
///
/// 1. Walk the hash chain starting from `cur_match`, comparing the
///    candidate string against the string at `strstart`.
/// 2. Use heuristic bytes at the end of the best match so far for
///    early rejection — most chain entries are eliminated without a full
///    byte comparison.
/// 3. Compare bytes 2..MAX_MATCH (bytes 0–1 are guaranteed equal by
///    the hash function when `hash_bits ≥ 8`).
/// 4. Stop when the chain is exhausted, `nice_match` is reached, or the
///    traversal limit (`max_chain_length`) is hit.
///
/// # Parameters
///
/// * `state` — Mutable deflate state. On a successful match longer than
///   `prev_length`, `match_start` is updated to the winning position.
/// * `cur_match` — Head of the hash chain to search (from
///   [`insert_string`]).
///
/// # Returns
///
/// The length of the best match found, clamped to `lookahead`.
pub(crate) fn longest_match(state: &mut DeflateState, cur_match: u32) -> usize {
    let mut chain_length = state.max_chain_length;
    let scan = state.strstart;
    let mut best_len = state.prev_length;
    let mut nice_match = state.nice_match;
    let wmask = state.w_mask;

    // MAX_DIST = w_size - MIN_LOOKAHEAD
    let max_dist = state.w_size.saturating_sub(MIN_LOOKAHEAD);
    let limit: usize = if scan > max_dist { scan - max_dist } else { NIL as usize };

    // Optimisation: halve the chain length when the previous match was
    // already good (deflate.c lines 1423–1424).
    if state.prev_length >= state.good_match {
        chain_length >>= 2;
    }

    // Do not look for matches beyond the end of the input. This is
    // necessary to make deflate deterministic (deflate.c line 1429).
    if nice_match > state.lookahead {
        nice_match = state.lookahead;
    }

    // Ensure we don't read beyond the window during comparisons.
    // strend marks the absolute upper bound for scanning.
    let strend = scan + MAX_MATCH;

    // Guard: if there's not enough data for even a MIN_MATCH comparison,
    // bail out early.
    if best_len >= state.lookahead {
        return state.lookahead;
    }

    // Heuristic bytes for early rejection (deflate.c lines 1413–1414).
    let mut scan_end1 = state.window[scan + best_len - 1];
    let mut scan_end = state.window[scan + best_len];

    let mut cur = cur_match as usize;

    loop {
        let match_pos = cur;

        // ── Early rejection ─────────────────────────────────────────────
        // Check bytes at the end of the current best match first; if they
        // don't agree, this candidate cannot beat the current best and we
        // skip the expensive full comparison (deflate.c lines 1482–1485).
        if match_pos + best_len < state.window.len()
            && state.window[match_pos + best_len] == scan_end
            && state.window[match_pos + best_len - 1] == scan_end1
            && state.window[match_pos] == state.window[scan]
            && state.window[match_pos + 1] == state.window[scan + 1]
        {
            // Bytes 0 and 1 match (guaranteed by hash for hash_bits ≥ 8).
            // Compare from byte 2 onwards up to MAX_MATCH.
            let cmp_end = strend.min(state.window.len());
            let match_end = (match_pos + MAX_MATCH).min(state.window.len());

            let scan_slice = &state.window[scan + 2..cmp_end];
            let match_slice = &state.window[match_pos + 2..match_end];

            let matched = scan_slice
                .iter()
                .zip(match_slice.iter())
                .take_while(|(a, b)| a == b)
                .count()
                + 2; // +2 for the two bytes already verified

            if matched > best_len {
                state.match_start = cur;
                best_len = matched;

                if matched >= nice_match {
                    break;
                }

                // Update heuristic bytes for the new best length.
                scan_end1 = state.window[scan + best_len - 1];
                scan_end = state.window[scan + best_len];
            }
        }

        // ── Follow hash chain ───────────────────────────────────────────
        cur = state.prev[cur & wmask] as usize;
        chain_length -= 1;

        if cur <= limit || chain_length == 0 {
            break;
        }
    }

    // The match length must not exceed lookahead (deflate.c line 1528).
    if best_len <= state.lookahead {
        best_len
    } else {
        state.lookahead
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::ZStream;

    /// Helper: build a `DeflateState` with window_size initialised.
    fn make_state() -> DeflateState {
        let mut s = DeflateState::new(9, 1, 8, 0, 6, 1);
        s.window_size = 2 * s.w_size;
        s
    }

    // ── NIL ──

    #[test]
    fn test_nil_is_zero() {
        assert_eq!(NIL, 0u16);
    }

    // ── update_hash ──

    #[test]
    fn test_update_hash_formula() {
        let state = DeflateState::new(15, 8, 8, 0, 6, 1);
        assert_eq!(state.hash_shift, 5);
        let h = update_hash(&state, 0, b'a');
        let expected = ((0u32 << 5) ^ u32::from(b'a')) & state.hash_mask;
        assert_eq!(h, expected);
    }

    #[test]
    fn test_update_hash_chained() {
        let state = make_state();
        let h1 = update_hash(&state, 0, b'a');
        let h2 = update_hash(&state, h1, b'b');
        let h3 = update_hash(&state, h2, b'c');
        assert!(h1 < state.hash_size as u32);
        assert!(h2 < state.hash_size as u32);
        assert!(h3 < state.hash_size as u32);
    }

    #[test]
    fn test_update_hash_stays_in_range() {
        let state = make_state();
        for c in 0u8..=255 {
            let h = update_hash(&state, 0xFFFF_FFFF, c);
            assert!(h <= state.hash_mask);
        }
    }

    // ── insert_string ──

    #[test]
    fn test_insert_string_returns_nil_on_empty() {
        let mut state = make_state();
        state.window[0] = b'a';
        state.window[1] = b'b';
        state.window[2] = b'c';
        state.ins_h = u32::from(state.window[0]);
        state.ins_h = update_hash(&state, state.ins_h, state.window[1]);
        let head = insert_string(&mut state, 0);
        assert_eq!(head, NIL);
    }

    #[test]
    fn test_insert_string_chains() {
        let mut state = make_state();
        let pos0: usize = 0;
        let pos1: usize = state.w_size / 2;
        for i in 0..4 {
            state.window[pos0 + i] = b'A' + i as u8;
            state.window[pos1 + i] = b'A' + i as u8;
        }
        state.ins_h = u32::from(state.window[pos0]);
        state.ins_h = update_hash(&state, state.ins_h, state.window[pos0 + 1]);
        let h0 = insert_string(&mut state, pos0);
        assert_eq!(h0, NIL);
        state.ins_h = u32::from(state.window[pos1]);
        state.ins_h = update_hash(&state, state.ins_h, state.window[pos1 + 1]);
        let h1 = insert_string(&mut state, pos1);
        assert_eq!(h1, pos0 as u16);
    }

    // ── clear_hash ──

    #[test]
    fn test_clear_hash() {
        let mut state = make_state();
        state.head[0] = 42;
        state.head[10] = 99;
        state.slid = true;
        clear_hash(&mut state);
        assert!(state.head[..state.hash_size].iter().all(|&v| v == NIL));
        assert!(!state.slid);
    }

    // ── slide_hash ──

    #[test]
    fn test_slide_hash() {
        let mut state = make_state();
        let wsize = state.w_size as u16;
        state.head[0] = wsize + 5;
        state.head[1] = wsize;
        state.head[2] = wsize - 1;
        state.prev[0] = wsize + 100;
        state.prev[1] = 3;

        slide_hash(&mut state);

        assert_eq!(state.head[0], 5);
        assert_eq!(state.head[1], 0);
        assert_eq!(state.head[2], NIL);
        assert_eq!(state.prev[0], 100);
        assert_eq!(state.prev[1], NIL);
        assert!(state.slid);
    }

    // ── read_buf ──

    #[test]
    fn test_read_buf_copies_data() {
        let mut stream = ZStream::new();
        stream.set_input(b"Hello, World!");
        let mut buf = vec![0u8; 5];
        let n = read_buf(&mut stream, &mut buf, 0);
        assert_eq!(n, 5);
        assert_eq!(&buf[..], b"Hello");
        assert_eq!(stream.avail_in(), 8);
    }

    #[test]
    fn test_read_buf_adler32_wrap1() {
        let mut stream = ZStream::new();
        stream.set_input(b"abc");
        stream.adler = 1;
        let mut buf = vec![0u8; 3];
        let n = read_buf(&mut stream, &mut buf, 1);
        assert_eq!(n, 3);
        assert_ne!(stream.adler, 1);
    }

    #[test]
    fn test_read_buf_crc32_wrap2() {
        let mut stream = ZStream::new();
        stream.set_input(b"abc");
        stream.adler = 0;
        let mut buf = vec![0u8; 3];
        let n = read_buf(&mut stream, &mut buf, 2);
        assert_eq!(n, 3);
        assert_ne!(stream.adler, 0);
    }

    #[test]
    fn test_read_buf_empty() {
        let mut stream = ZStream::new();
        stream.set_input(b"");
        let mut buf = vec![0u8; 10];
        let n = read_buf(&mut stream, &mut buf, 0);
        assert_eq!(n, 0);
    }

    #[test]
    fn test_read_buf_respects_buf_size() {
        let mut stream = ZStream::new();
        stream.set_input(b"Hello, World!");
        let mut buf = vec![0u8; 3];
        let n = read_buf(&mut stream, &mut buf, 0);
        assert_eq!(n, 3);
        assert_eq!(&buf[..], b"Hel");
    }

    #[test]
    fn test_read_buf_increments_total_in() {
        let mut stream = ZStream::new();
        stream.set_input(b"data");
        let mut buf = vec![0u8; 4];
        read_buf(&mut stream, &mut buf, 0);
        assert_eq!(stream.total_in, 4);
    }

    // ── fill_window ──

    #[test]
    fn test_fill_window_populates_lookahead() {
        let mut state = make_state();
        state.strstart = 0;
        state.lookahead = 0;
        state.insert = 0;
        state.high_water = 0;
        state.block_start = 0;

        let mut stream = ZStream::new();
        let data = vec![b'A'; 300];
        stream.set_input(&data);

        fill_window(&mut state, &mut stream);

        assert!(state.lookahead > 0);
        assert_eq!(stream.avail_in() + state.lookahead, 300);
    }

    #[test]
    fn test_fill_window_reads_all_small_input() {
        let mut state = make_state();
        state.strstart = 0;
        state.lookahead = 0;
        state.insert = 0;
        state.high_water = 0;
        state.block_start = 0;

        let mut stream = ZStream::new();
        stream.set_input(b"Hello, World!");
        fill_window(&mut state, &mut stream);

        assert_eq!(stream.avail_in(), 0);
        assert_eq!(state.lookahead, 13);
    }

    // ── longest_match ──

    #[test]
    fn test_longest_match_full_match() {
        let mut state = make_state();
        state.strstart = 300;
        state.lookahead = 258;
        state.prev_length = 2;
        state.good_match = 32;
        state.nice_match = 258;
        state.max_chain_length = 64;

        for i in 0..258 {
            state.window[100 + i] = b'X';
            state.window[300 + i] = b'X';
        }
        state.prev[100 & state.w_mask] = NIL;

        let result = longest_match(&mut state, 100);
        assert_eq!(result, 258);
        assert_eq!(state.match_start, 100);
    }

    #[test]
    fn test_longest_match_bounded_by_lookahead() {
        let mut state = make_state();
        state.strstart = 300;
        state.lookahead = 50;
        state.prev_length = 2;
        state.good_match = 32;
        state.nice_match = 258;
        state.max_chain_length = 64;

        for i in 0..258 {
            state.window[100 + i] = b'Q';
            state.window[300 + i] = b'Q';
        }
        state.prev[100 & state.w_mask] = NIL;

        let result = longest_match(&mut state, 100);
        assert!(result <= 50);
    }

    #[test]
    fn test_longest_match_chain_traversal() {
        // Use a larger window (w_bits=12 → w_size=4096) so that limit
        // doesn't exclude our candidate positions.
        let mut state = {
            let mut s = DeflateState::new(12, 4, 8, 0, 6, 1);
            s.window_size = 2 * s.w_size;
            s
        };
        state.strstart = 400;
        state.lookahead = 258;
        state.prev_length = 2;
        state.good_match = 32;
        state.nice_match = 258;
        state.max_chain_length = 64;

        // Position 200: 10 matching bytes then divergence
        for i in 0..10 {
            state.window[200 + i] = b'A';
            state.window[400 + i] = b'A';
        }
        state.window[200 + 10] = b'Z';
        state.window[400 + 10] = b'A';
        for i in 10..258 {
            state.window[400 + i] = b'A';
        }

        // Position 100: 20 matching bytes
        for i in 0..20 {
            state.window[100 + i] = b'A';
        }
        state.window[100 + 20] = b'Y';

        // Chain: head -> 200 -> 100 -> NIL
        state.prev[200 & state.w_mask] = 100;
        state.prev[100 & state.w_mask] = NIL;

        let result = longest_match(&mut state, 200);
        assert!(result >= 20, "expected >= 20, got {result}");
        assert_eq!(state.match_start, 100);
    }

    #[test]
    fn test_longest_match_halves_chain_for_good() {
        let mut state = make_state();
        state.strstart = 300;
        state.lookahead = 258;
        state.prev_length = 32;
        state.good_match = 32;
        state.nice_match = 258;
        state.max_chain_length = 4;

        for i in 0..258 {
            state.window[100 + i] = b'Y';
            state.window[300 + i] = b'Y';
        }
        state.prev[100 & state.w_mask] = NIL;

        let result = longest_match(&mut state, 100);
        assert!(result > 0);
    }

    #[test]
    fn test_longest_match_prev_length_ge_lookahead() {
        let mut state = make_state();
        state.strstart = 300;
        state.lookahead = 10;
        state.prev_length = 20; // >= lookahead
        state.good_match = 4;
        state.nice_match = 258;
        state.max_chain_length = 64;

        // Should return lookahead immediately since
        // best_len(=prev_length=20) >= lookahead(=10).
        let result = longest_match(&mut state, 100);
        assert_eq!(result, 10);
    }
}
