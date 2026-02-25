// Five DEFLATE compression strategy functions and their shared helpers.
//
// Ported from `deflate.c` lines 63–68 (block_state enum), 1626–1651 (macros),
// and 1668–2185 (the five compression functions) of the zlib 1.3.2.1-motley C
// library.
//
// Each function implements a different speed/compression trade-off:
//
// | Function          | C Lines      | Strategy          | Description                         |
// |-------------------|-------------|-------------------|-------------------------------------|
// | `deflate_stored`  | 1668–1848   | Level 0           | Copy data verbatim in stored blocks |
// | `deflate_fast`    | 1857–1948   | Levels 1–3        | Greedy matching without lazy eval   |
// | `deflate_slow`    | 1956–2077   | Levels 4–9        | Lazy match evaluation for best ratio|
// | `deflate_rle`     | 2084–2149   | `Z_RLE`           | Run-length encoding (distance = 1)  |
// | `deflate_huff`    | 2155–2185   | `Z_HUFFMAN_ONLY`  | Huffman-only, no string matching    |
//
// # Safety
//
// This module contains **zero** `unsafe` blocks. All C pointer arithmetic
// has been replaced with safe Rust slice indexing and `copy_from_slice` /
// `copy_within` operations.

use crate::constants::{MAX_MATCH, MIN_MATCH, Z_FILTERED, Z_FINISH, Z_NO_FLUSH};
use crate::stream::ZStream;

use super::hash;
use super::state::DeflateState;
use super::trees;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Minimum amount of lookahead, except at the end of the input.
///
/// Defined as `MAX_MATCH + MIN_MATCH + 1` in `deflate.h` line 296. The
/// deflate engine calls [`hash::fill_window`] whenever `lookahead` drops
/// below this threshold.
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Maximum stored block length in deflate format (not including header).
///
/// Ported from `deflate.c` line 1648: `#define MAX_STORED 65535`.
const MAX_STORED: usize = 65535;

/// Distance threshold for discarding short (3-byte) matches.
///
/// Matches of exactly `MIN_MATCH` (3) bytes are discarded by
/// [`deflate_slow`] when the distance exceeds this value, because the
/// encoding overhead outweighs the savings. From `deflate.c` line 89.
const TOO_FAR: usize = 4096;

// ─── BlockState ───────────────────────────────────────────────────────────────

/// Internal block state returned by the five compression strategy functions.
///
/// Ports the C `block_state` enum from `deflate.c` lines 63–68. The deflate
/// engine's main loop (`deflate()`) inspects this return value to decide
/// whether to continue processing, return to the caller for more I/O space,
/// or finalize the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockState {
    /// Block not completed — need more input or more output space.
    NeedMore,
    /// Block flush performed successfully.
    BlockDone,
    /// Finish started — need only more output at the next `deflate` call.
    FinishStarted,
    /// Finish done — accept no more input or output.
    FinishDone,
}

// ─── Helper: flush_pending ────────────────────────────────────────────────────

/// Flush pending compressed bytes from `state.pending_buf` to the stream
/// output buffer.
///
/// Ports `flush_pending()` from `deflate.c` lines 950–969. This function:
/// 1. Flushes any partial bits in the bit buffer via [`trees::tr_flush_bits`].
/// 2. Copies up to `min(state.pending, stream.avail_out())` bytes from the
///    pending buffer to the stream output.
/// 3. Shifts any remaining pending data to the front of the buffer so that
///    subsequent writes start at index `state.pending`.
fn flush_pending(state: &mut DeflateState, stream: &mut ZStream) {
    trees::tr_flush_bits(state);

    let len = state.pending.min(stream.avail_out());
    if len == 0 {
        return;
    }

    // Copy pending bytes to stream output.
    let out = stream.output_remaining_mut();
    out[..len].copy_from_slice(&state.pending_buf[..len]);
    let _ = stream.advance_output(len);

    // Shift remaining pending data to the front of the buffer.
    if len < state.pending {
        state.pending_buf.copy_within(len..state.pending, 0);
    }
    state.pending -= len;
}

// ─── Helper: flush_block_only ─────────────────────────────────────────────────

/// Flush the current block, with the given end-of-file flag.
///
/// Ports the `FLUSH_BLOCK_ONLY` macro from `deflate.c` lines 1630–1639.
/// This helper:
/// 1. Determines the window slice for the current block (if `block_start ≥ 0`).
/// 2. Computes the stored length of the block.
/// 3. Calls [`trees::tr_flush_block`] to encode the block using the best
///    encoding (stored, static Huffman, or dynamic Huffman).
/// 4. Advances `block_start` to the current `strstart`.
/// 5. Calls [`flush_pending`] to write compressed output.
fn flush_block_only(state: &mut DeflateState, stream: &mut ZStream, last: bool) {
    // Copy the block data from the window into a temporary buffer to avoid
    // holding an immutable borrow on `state.window` while passing `&mut state`
    // to `tr_flush_block`.
    let buf = if state.block_start >= 0 {
        let start = state.block_start as usize;
        let end = state.strstart;
        Some(state.window[start..end].to_vec())
    } else {
        None
    };
    let stored_len = (state.strstart as i64 - state.block_start) as u64;
    trees::tr_flush_block(state, buf.as_deref(), stored_len, last);
    state.block_start = state.strstart as i64;
    flush_pending(state, stream);
}

/// Flush the current block and return early if the output buffer is full.
///
/// Ports the `FLUSH_BLOCK` macro from `deflate.c` lines 1642–1645.
/// Returns `Some(BlockState)` if the output buffer is exhausted (the caller
/// should return that value), or `None` if processing can continue.
#[inline]
fn flush_block(
    state: &mut DeflateState,
    stream: &mut ZStream,
    last: bool,
) -> Option<BlockState> {
    flush_block_only(state, stream, last);
    if stream.avail_out() == 0 {
        return Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        });
    }
    None
}

// ─── Helper: max_dist ─────────────────────────────────────────────────────────

/// Compute the maximum match distance for the current window size.
///
/// Equivalent to the C macro `MAX_DIST(s)` = `w_size - MIN_LOOKAHEAD`.
#[inline]
fn max_dist(state: &DeflateState) -> usize {
    state.w_size.saturating_sub(MIN_LOOKAHEAD)
}

// ─── deflate_stored ───────────────────────────────────────────────────────────

/// Copy without compression as much as possible from the input stream.
///
/// Ports `deflate_stored()` from `deflate.c` lines 1668–1848. This function
/// is used when the compression level is 0 (no compression). Data is emitted
/// as DEFLATE stored blocks: a 5-byte header (type + length + complement)
/// followed by the raw data.
///
/// In case `deflateParams()` later switches to a non-zero compression level,
/// `state.matches` keeps track of the number of hash table slides to
/// perform (0 = none, 1 = one slide, 2 = clear hash).
///
/// # Algorithm
///
/// The function operates in two phases:
///
/// 1. **Main loop** — emit complete stored blocks directly to the output
///    stream, bypassing the pending buffer for the payload data. Only the
///    5-byte block header passes through `pending_buf`.
///
/// 2. **Fallback** — if the output buffer is too small for a direct block,
///    buffer data in the sliding window and emit a stored block through
///    `pending_buf` when possible.
pub(crate) fn deflate_stored(
    state: &mut DeflateState,
    stream: &mut ZStream,
    flush: i32,
) -> BlockState {
    // Smallest worthy block size when not flushing or finishing. By default
    // this is 32K. This can be as small as 507 bytes for memLevel == 1.
    let mut min_block = state.pending_buf_size.saturating_sub(5).min(state.w_size);

    // Save initial avail_in to compute how much input was consumed later.
    let saved_avail_in = stream.avail_in();
    // Collect input bytes consumed in the main loop for later window update.
    let mut consumed_input: Vec<u8> = Vec::new();

    // --- Phase 1: Main loop — copy stored blocks directly to output ---
    let mut last: bool = false;
    loop {
        // Set len to the maximum size block that we can copy directly with
        // the available input data and output space.
        let mut len: usize = MAX_STORED;
        let have_header = ((state.bi_valid as u32 + 42) >> 3) as usize;
        if stream.avail_out() < have_header {
            break; // need room for header
        }
        let have = stream.avail_out() - have_header;

        // left = window bytes in the current block not yet output
        let left_full =
            (state.strstart as i64 - state.block_start).max(0) as usize;
        let left = left_full;

        if len as u64 > left as u64 + stream.avail_in() as u64 {
            len = left + stream.avail_in(); // limit to available input
        }
        if len > have {
            len = have; // limit to available output space
        }

        // Skip too-small blocks unless flushing or finishing.
        if len < min_block
            && ((len == 0 && flush != Z_FINISH)
                || flush == Z_NO_FLUSH
                || len != left + stream.avail_in())
        {
            break;
        }

        // Determine if this is the last block.
        last = flush == Z_FINISH && len == left + stream.avail_in();

        // Write a dummy stored block header to pending_buf (length = 0).
        trees::tr_stored_block(state, None, 0, last);

        // Patch the dummy length fields with the actual length.
        let p = state.pending;
        if p >= 4 {
            state.pending_buf[p - 4] = (len & 0xFF) as u8;
            state.pending_buf[p - 3] = ((len >> 8) & 0xFF) as u8;
            let nlen = !len;
            state.pending_buf[p - 2] = (nlen & 0xFF) as u8;
            state.pending_buf[p - 1] = ((nlen >> 8) & 0xFF) as u8;
        }

        // Flush the stored block header to the output stream.
        flush_pending(state, stream);

        // Copy uncompressed bytes from the window to the output.
        if left > 0 {
            let copy_len = left.min(len);
            let win_start = state.block_start as usize;
            let out = stream.output_remaining_mut();
            out[..copy_len].copy_from_slice(
                &state.window[win_start..win_start + copy_len],
            );
            let _ = stream.advance_output(copy_len);
            state.block_start += copy_len as i64;
            len -= copy_len;
            let _ = left - copy_len;
        }

        // Copy uncompressed bytes directly from input to output, updating
        // the check value.
        if len > 0 {
            let wrap = state.wrap;
            let mut temp = vec![0u8; len];
            let n = hash::read_buf(stream, &mut temp, wrap);
            if n > 0 {
                let out = stream.output_remaining_mut();
                out[..n].copy_from_slice(&temp[..n]);
                let _ = stream.advance_output(n);
                consumed_input.extend_from_slice(&temp[..n]);
            }
        }

        if last {
            break;
        }
    }

    // --- Update sliding window with consumed input data ---
    let used = saved_avail_in - stream.avail_in();
    if used > 0 {
        if used >= state.w_size {
            // Supplant the previous history — fill entire window from the
            // end of consumed input.
            state.matches = 2; // signal clear hash
            let start = consumed_input.len().saturating_sub(state.w_size);
            let copy_len = state.w_size.min(consumed_input.len() - start);
            state.window[..copy_len]
                .copy_from_slice(&consumed_input[start..start + copy_len]);
            state.strstart = state.w_size;
            state.insert = state.strstart;
        } else {
            if state.window_size - state.strstart <= used {
                // Slide the window down.
                state.strstart -= state.w_size;
                state.window
                    .copy_within(state.w_size..state.w_size + state.strstart, 0);
                if state.matches < 2 {
                    state.matches += 1; // add a pending slide_hash()
                }
                if state.insert > state.strstart {
                    state.insert = state.strstart;
                }
            }
            let start = consumed_input.len().saturating_sub(used);
            let copy_len = used.min(consumed_input.len() - start);
            state.window[state.strstart..state.strstart + copy_len]
                .copy_from_slice(&consumed_input[start..start + copy_len]);
            state.strstart += copy_len;
            state.insert += copy_len.min(state.w_size.saturating_sub(state.insert));
        }
        state.block_start = state.strstart as i64;
    }

    if state.high_water < state.strstart {
        state.high_water = state.strstart;
    }

    // If the last block was written to output, we are done.
    if last {
        state.bi_used = 8;
        return BlockState::FinishDone;
    }

    // If flushing and all input has been consumed, return block_done.
    if flush != Z_NO_FLUSH
        && flush != Z_FINISH
        && stream.avail_in() == 0
        && state.strstart as i64 == state.block_start
    {
        return BlockState::BlockDone;
    }

    // --- Phase 2: Fill window with remaining input, try pending block ---
    let mut have = state.window_size.saturating_sub(state.strstart);
    if stream.avail_in() > have && state.block_start >= state.w_size as i64 {
        // Slide the window down.
        state.block_start -= state.w_size as i64;
        state.strstart -= state.w_size;
        state.window
            .copy_within(state.w_size..state.w_size + state.strstart, 0);
        if state.matches < 2 {
            state.matches += 1;
        }
        have += state.w_size;
        if state.insert > state.strstart {
            state.insert = state.strstart;
        }
    }
    let have = have.min(stream.avail_in());
    if have > 0 {
        let wrap = state.wrap;
        let offset = state.strstart;
        let n = hash::read_buf(stream, &mut state.window[offset..offset + have], wrap);
        state.strstart += n;
        state.insert += n.min(state.w_size.saturating_sub(state.insert));
    }
    if state.high_water < state.strstart {
        state.high_water = state.strstart;
    }

    // Try to write a stored block to pending if there is enough data.
    let have_header2 = ((state.bi_valid as u32 + 42) >> 3) as usize;
    let have_pending = state
        .pending_buf_size
        .saturating_sub(have_header2)
        .min(MAX_STORED);
    min_block = have_pending.min(state.w_size);
    let left = (state.strstart as i64 - state.block_start).max(0) as usize;
    if left >= min_block
        || ((left > 0 || flush == Z_FINISH)
            && flush != Z_NO_FLUSH
            && stream.avail_in() == 0
            && left <= have_pending)
    {
        let len = left.min(have_pending);
        let last_pending = flush == Z_FINISH
            && stream.avail_in() == 0
            && len == left;
        let block_start_usize = state.block_start.max(0) as usize;
        let buf = state.window[block_start_usize..block_start_usize + len].to_vec();
        trees::tr_stored_block(state, Some(&buf), len as u64, last_pending);
        state.block_start += len as i64;
        flush_pending(state, stream);
        if last_pending {
            state.bi_used = 8;
        }
        if last_pending {
            return BlockState::FinishStarted;
        }
    }

    // We have done all we can with the available input and output.
    if flush == Z_FINISH && stream.avail_in() == 0 {
        let left_now = (state.strstart as i64 - state.block_start).max(0) as usize;
        if left_now == 0 {
            state.bi_used = 8;
            return BlockState::FinishStarted;
        }
    }
    BlockState::NeedMore
}

// ─── deflate_fast ─────────────────────────────────────────────────────────────

/// Compress as much as possible from the input stream using greedy matching.
///
/// Ports `deflate_fast()` from `deflate.c` lines 1857–1948. This function
/// does **not** perform lazy evaluation of matches and inserts new strings in
/// the dictionary only for unmatched strings or for short matches. It is used
/// for compression levels 1–3.
///
/// # Algorithm
///
/// For each position in the window:
/// 1. Insert the string into the hash table.
/// 2. If the hash chain head is non-NIL and within range, search for the
///    longest match via [`hash::longest_match`].
/// 3. If the match length ≥ `MIN_MATCH`, emit a length/distance pair and
///    insert hash entries for the matched bytes.
/// 4. Otherwise, emit a literal byte.
/// 5. Flush the block when the symbol buffer is full.
pub(crate) fn deflate_fast(
    state: &mut DeflateState,
    stream: &mut ZStream,
    flush: i32,
) -> BlockState {
    loop {
        // Ensure enough lookahead, except at the end of input.
        if state.lookahead < MIN_LOOKAHEAD {
            hash::fill_window(state, stream);
            if state.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BlockState::NeedMore;
            }
            if state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // Insert the string window[strstart..strstart+2] in the dictionary
        // and set hash_head to the head of the hash chain.
        let mut hash_head: u16 = hash::NIL;
        if state.lookahead >= MIN_MATCH {
            hash_head = hash::insert_string(state, state.strstart);
        }

        // Find the longest match, discarding those <= prev_length.
        if hash_head != hash::NIL
            && state.strstart.saturating_sub(hash_head as usize) <= max_dist(state)
        {
            state.match_length =
                hash::longest_match(state, u32::from(hash_head));
            // longest_match() sets match_start
        }

        if state.match_length >= MIN_MATCH {
            // Tally the distance/length pair.
            let dist = state.strstart - state.match_start;
            let bflush = trees::tr_tally_dist(
                state,
                dist as u32,
                state.match_length as u32,
            );

            state.lookahead -= state.match_length;

            // Insert new strings in the hash table only if the match length
            // is not too large. This saves time but degrades compression.
            if state.match_length <= state.max_insert_length()
                && state.lookahead >= MIN_MATCH
            {
                state.match_length -= 1; // string at strstart already in table
                while state.match_length > 0 {
                    state.strstart += 1;
                    let _ = hash::insert_string(state, state.strstart);
                    state.match_length -= 1;
                }
                state.strstart += 1;
            } else {
                state.strstart += state.match_length;
                state.match_length = 0;
                state.ins_h = u32::from(state.window[state.strstart]);
                state.ins_h = hash::update_hash(
                    state,
                    state.ins_h,
                    state.window[state.strstart + 1],
                );
                // If lookahead < MIN_MATCH, ins_h is garbage, but it does
                // not matter since it will be recomputed at the next call.
            }

            if bflush {
                if let Some(result) = flush_block(state, stream, false) {
                    return result;
                }
            }
        } else {
            // No match — output a literal byte.
            let bflush =
                trees::tr_tally_lit(state, state.window[state.strstart]);
            state.lookahead -= 1;
            state.strstart += 1;

            if bflush {
                if let Some(result) = flush_block(state, stream, false) {
                    return result;
                }
            }
        }
    }

    // Set the number of bytes to insert into the hash table on the next
    // call (prevents reading past the end of the window).
    state.insert = if state.strstart < MIN_MATCH - 1 {
        state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == Z_FINISH {
        if let Some(result) = flush_block(state, stream, true) {
            return result;
        }
        return BlockState::FinishDone;
    }
    if state.sym_next != 0 {
        if let Some(result) = flush_block(state, stream, false) {
            return result;
        }
    }
    BlockState::BlockDone
}

// ─── deflate_slow ─────────────────────────────────────────────────────────────

/// Compress with lazy match evaluation for best compression ratio.
///
/// Ports `deflate_slow()` from `deflate.c` lines 1956–2077. This function
/// achieves better compression than [`deflate_fast`] by delaying the output
/// of a match until the next position is examined: if a longer match is
/// found, the previous match is discarded and the longer one is emitted
/// instead. Used for compression levels 4–9.
///
/// # Algorithm
///
/// For each position:
/// 1. Save the previous match length and start position.
/// 2. Insert the string and search for a longer match.
/// 3. **Decision point:**
///    - If the previous match ≥ `MIN_MATCH` and the current match is not
///      better, output the previous match and insert hash entries.
///    - Else if a match is available from the previous step, output it as a
///      literal.
///    - Else record that a match is available and advance.
pub(crate) fn deflate_slow(
    state: &mut DeflateState,
    stream: &mut ZStream,
    flush: i32,
) -> BlockState {
    loop {
        // Ensure enough lookahead, except at the end of input.
        if state.lookahead < MIN_LOOKAHEAD {
            hash::fill_window(state, stream);
            if state.lookahead < MIN_LOOKAHEAD && flush == Z_NO_FLUSH {
                return BlockState::NeedMore;
            }
            if state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // Insert the string and get the hash chain head.
        let mut hash_head: u16 = hash::NIL;
        if state.lookahead >= MIN_MATCH {
            hash_head = hash::insert_string(state, state.strstart);
        }

        // Save the previous match for lazy evaluation.
        state.prev_length = state.match_length;
        state.prev_match = state.match_start;
        state.match_length = MIN_MATCH - 1;

        // Try to find a longer match.
        if hash_head != hash::NIL
            && state.prev_length < state.max_lazy_match
            && state.strstart.saturating_sub(hash_head as usize) <= max_dist(state)
        {
            state.match_length =
                hash::longest_match(state, u32::from(hash_head));
            // longest_match() sets match_start

            // Discard short matches if the strategy is Z_FILTERED and the
            // match is too far away (TOO_FAR threshold for 3-byte matches).
            if state.match_length <= 5
                && (state.strategy == Z_FILTERED
                    || (state.match_length == MIN_MATCH
                        && state.strstart - state.match_start > TOO_FAR))
            {
                state.match_length = MIN_MATCH - 1;
            }
        }

        // Decision: output previous match, literal, or defer.
        if state.prev_length >= MIN_MATCH
            && state.match_length <= state.prev_length
        {
            // The previous match is at least as good as the current one —
            // output the previous match.
            let max_insert = state.strstart + state.lookahead - MIN_MATCH;

            let dist = state.strstart - 1 - state.prev_match;
            let bflush = trees::tr_tally_dist(
                state,
                dist as u32,
                state.prev_length as u32,
            );

            // Insert hash entries for all strings up to the end of the match.
            state.lookahead -= state.prev_length - 1;
            state.prev_length -= 2;
            loop {
                state.strstart += 1;
                if state.strstart <= max_insert {
                    let _ = hash::insert_string(state, state.strstart);
                }
                if state.prev_length == 0 {
                    break;
                }
                state.prev_length -= 1;
            }
            state.match_available = false;
            state.match_length = MIN_MATCH - 1;
            state.strstart += 1;

            if bflush {
                if let Some(result) = flush_block(state, stream, false) {
                    return result;
                }
            }
        } else if state.match_available {
            // No better match found — output the previous position as a
            // single literal. Use flush_block_only (not flush_block) to
            // avoid premature exit on full output buffer.
            let bflush =
                trees::tr_tally_lit(state, state.window[state.strstart - 1]);
            if bflush {
                flush_block_only(state, stream, false);
            }
            state.strstart += 1;
            state.lookahead -= 1;
            if stream.avail_out() == 0 {
                return BlockState::NeedMore;
            }
        } else {
            // No previous match to compare with — wait for the next step.
            state.match_available = true;
            state.strstart += 1;
            state.lookahead -= 1;
        }
    }

    // Output the remaining available match before finishing.
    if state.match_available {
        let _ = trees::tr_tally_lit(state, state.window[state.strstart - 1]);
        state.match_available = false;
    }

    state.insert = if state.strstart < MIN_MATCH - 1 {
        state.strstart
    } else {
        MIN_MATCH - 1
    };

    if flush == Z_FINISH {
        if let Some(result) = flush_block(state, stream, true) {
            return result;
        }
        return BlockState::FinishDone;
    }
    if state.sym_next != 0 {
        if let Some(result) = flush_block(state, stream, false) {
            return result;
        }
    }
    BlockState::BlockDone
}

// ─── deflate_rle ──────────────────────────────────────────────────────────────

/// Compress using run-length encoding (distance-1 matches only).
///
/// Ports `deflate_rle()` from `deflate.c` lines 2084–2149. For the `Z_RLE`
/// strategy, this function looks for runs of repeated bytes and generates
/// matches with distance = 1 only. No hash table is maintained.
///
/// # Algorithm
///
/// For each position:
/// 1. Ensure enough lookahead (`MAX_MATCH + 1` bytes).
/// 2. Compare the byte at `strstart - 1` against consecutive bytes to find
///    the run length (using an unrolled 8-at-a-time comparison loop).
/// 3. If the run ≥ `MIN_MATCH`, emit a distance-1 / length pair.
/// 4. Otherwise, emit a literal byte.
pub(crate) fn deflate_rle(
    state: &mut DeflateState,
    stream: &mut ZStream,
    flush: i32,
) -> BlockState {
    loop {
        // Ensure enough lookahead for the longest possible run plus one byte
        // for the unrolled loop overshoot.
        if state.lookahead <= MAX_MATCH {
            hash::fill_window(state, stream);
            if state.lookahead <= MAX_MATCH && flush == Z_NO_FLUSH {
                return BlockState::NeedMore;
            }
            if state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // See how many times the previous byte repeats.
        state.match_length = 0;
        if state.lookahead >= MIN_MATCH && state.strstart > 0 {
            let prev = state.window[state.strstart - 1];
            let scan_start = state.strstart;
            let scan_limit = scan_start + state.lookahead.min(MAX_MATCH);

            // Check the first 3 bytes explicitly.
            if prev == state.window[scan_start]
                && prev == state.window[scan_start + 1]
                && prev == state.window[scan_start + 2]
            {
                // Unrolled comparison: 8 bytes at a time.
                let mut scan = scan_start + 3;
                while scan + 7 < scan_limit {
                    if prev != state.window[scan]
                        || prev != state.window[scan + 1]
                        || prev != state.window[scan + 2]
                        || prev != state.window[scan + 3]
                        || prev != state.window[scan + 4]
                        || prev != state.window[scan + 5]
                        || prev != state.window[scan + 6]
                        || prev != state.window[scan + 7]
                    {
                        break;
                    }
                    scan += 8;
                }
                // Byte-by-byte tail.
                while scan < scan_limit && state.window[scan] == prev {
                    scan += 1;
                }
                state.match_length = scan - scan_start;
                if state.match_length > state.lookahead {
                    state.match_length = state.lookahead;
                }
            }
        }

        // Emit match or literal.
        if state.match_length >= MIN_MATCH {
            let bflush =
                trees::tr_tally_dist(state, 1, state.match_length as u32);

            state.lookahead -= state.match_length;
            state.strstart += state.match_length;
            state.match_length = 0;

            if bflush {
                if let Some(result) = flush_block(state, stream, false) {
                    return result;
                }
            }
        } else {
            // No run — output a literal byte.
            let bflush =
                trees::tr_tally_lit(state, state.window[state.strstart]);
            state.lookahead -= 1;
            state.strstart += 1;

            if bflush {
                if let Some(result) = flush_block(state, stream, false) {
                    return result;
                }
            }
        }
    }

    state.insert = 0;

    if flush == Z_FINISH {
        if let Some(result) = flush_block(state, stream, true) {
            return result;
        }
        return BlockState::FinishDone;
    }
    if state.sym_next != 0 {
        if let Some(result) = flush_block(state, stream, false) {
            return result;
        }
    }
    BlockState::BlockDone
}

// ─── deflate_huff ─────────────────────────────────────────────────────────────

/// Compress using Huffman encoding only — no string matching.
///
/// Ports `deflate_huff()` from `deflate.c` lines 2155–2185. For the
/// `Z_HUFFMAN_ONLY` strategy, every input byte is emitted as a Huffman-coded
/// literal. No hash table is maintained and no back-references are generated.
///
/// This is the simplest compression strategy and produces the largest output
/// (roughly the input size plus Huffman overhead). It is useful when the data
/// has already been transformed by an external LZ77 stage and only entropy
/// coding is desired.
pub(crate) fn deflate_huff(
    state: &mut DeflateState,
    stream: &mut ZStream,
    flush: i32,
) -> BlockState {
    loop {
        // Ensure we have at least one literal to write.
        if state.lookahead == 0 {
            hash::fill_window(state, stream);
            if state.lookahead == 0 {
                if flush == Z_NO_FLUSH {
                    return BlockState::NeedMore;
                }
                break; // flush the current block
            }
        }

        // Output every byte as a literal — no matching.
        state.match_length = 0;
        let bflush =
            trees::tr_tally_lit(state, state.window[state.strstart]);
        state.lookahead -= 1;
        state.strstart += 1;

        if bflush {
            if let Some(result) = flush_block(state, stream, false) {
                return result;
            }
        }
    }

    state.insert = 0;

    if flush == Z_FINISH {
        if let Some(result) = flush_block(state, stream, true) {
            return result;
        }
        return BlockState::FinishDone;
    }
    if state.sym_next != 0 {
        if let Some(result) = flush_block(state, stream, false) {
            return result;
        }
    }
    BlockState::BlockDone
}


