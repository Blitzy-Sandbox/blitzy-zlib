//! Fast-path inflate decode loop for DEFLATE decompression.
//!
//! This module contains the performance-critical inner loop of the inflate
//! engine, ported from C zlib's `inffast.c` (321 lines). It is called when
//! sufficient input (`>= 6` bytes) and output space (`>= 258` bytes) are
//! available, allowing the decode loop to run without per-byte boundary
//! checks for maximum throughput.
//!
//! # Safety
//!
//! This is the **one file** in the inflate module where `unsafe` blocks are
//! permitted (per AAP §0.7.2). Unsafe code is concentrated in the match-copy
//! inner loop where pointer-based buffer access is essential for matching
//! C zlib's decompression throughput. Every `unsafe` block has a `// SAFETY:`
//! comment documenting its invariants.

use core::ptr;

use super::state::{InflateMode, InflateState};
use super::tables::Code;

/// Copies `len` bytes from `output[from..]` to `output[*out_pos..]`,
/// handling overlapping regions correctly by using forward byte-by-byte copy
/// when the source and destination overlap, or bulk `copy_nonoverlapping`
/// when they do not.
///
/// When `dist < len`, the overlapping forward copy intentionally produces
/// repeating byte patterns, matching the LZ77 run-length encoding semantics
/// required by the DEFLATE specification.
///
/// # Safety
///
/// The caller **must** guarantee all of the following:
/// - `from + len <= output.len()`
/// - `*out_pos + len <= output.len()`
/// - `from < *out_pos` (source region precedes destination)
#[allow(dead_code)] // Called by inflate_fast; unused until mod.rs calls inflate_fast
#[inline(always)]
unsafe fn copy_match(output: &mut [u8], from: usize, out_pos: &mut usize, dist: usize, len: usize) {
    // SAFETY: All pointer arithmetic and dereferences below are valid because
    // the caller guarantees:
    //   - from + len <= output.len()
    //   - *out_pos + len <= output.len()
    //   - from < *out_pos
    // These invariants ensure all .add() offsets stay within the allocation
    // and all pointer dereferences access initialized memory.
    unsafe {
        let src = output.as_ptr().add(from);
        let dst = output.as_mut_ptr().add(*out_pos);
        if dist >= len {
            // Non-overlapping: source [from..from+len] and destination
            // [out_pos..out_pos+len] do not overlap (dist = out_pos - from >= len).
            ptr::copy_nonoverlapping(src, dst, len);
        } else {
            // Overlapping forward copy — intentionally produces repeating byte
            // patterns per LZ77 semantics. Forward iteration is correct because
            // each byte at offset i is read from src+i before being written to
            // dst+i, and dist >= 1 guarantees src+i != dst+i for all i.
            for i in 0..len {
                *dst.add(i) = *src.add(i);
            }
        }
    }
    *out_pos += len;
}

/// Fast-path inflate decode loop.
///
/// Decodes literals, lengths, and distances in a tight inner loop without
/// per-byte boundary checks, matching C zlib's `inffast.c` performance
/// characteristics. This function is called from the main `inflate()` state
/// machine when sufficient input and output buffering is available.
///
/// This function modifies `state.mode` directly on termination:
/// - [`InflateMode::Type`] — end-of-block marker decoded successfully
/// - [`InflateMode::Bad`] — invalid code encountered; error message stored
///   in `state.msg`
///
/// Literals decoded by the fast path are output directly without transitioning
/// through [`InflateMode::Lit`]; that mode is used only by the slow-path
/// decoder in `inflate()`.
///
/// # Arguments
///
/// * `state`  — Mutable reference to the inflate decompression state,
///   containing the Huffman decode tables, sliding window, and bit accumulator
/// * `input`  — Input byte slice containing compressed DEFLATE data
/// * `output` — Output byte slice for writing decompressed data
/// * `in_pos` — Current read position in `input` (updated on return)
/// * `out_pos` — Current write position in `output` (updated on return)
/// * `start`  — Initial output position when `inflate()` was called, used to
///   compute how many bytes have been written so far for distance-back-reference
///   window calculations
///
/// # Preconditions
///
/// The caller **must** ensure all of the following before calling:
/// - `input.len() - *in_pos >= 6` — enough input for the maximum first-level
///   code length plus extra bits without needing boundary checks
/// - `output.len() - *out_pos >= 258` — enough output space for the maximum
///   match length (`MAX_MATCH = 258`)
/// - State was in `Len` mode when the fast-path decision was made
/// - `state.bits < 8` — the hold buffer has fewer than 8 bits remaining
///
/// **Callers MUST guarantee all preconditions are satisfied before calling.**
/// The main decode loop uses a do-while pattern (condition checked at the
/// bottom), so if preconditions are violated, one full iteration may execute
/// before the boundary check terminates the loop, potentially reading or
/// writing out of bounds.
#[allow(dead_code)] // Will be called from mod.rs once the inflate state machine is complete
pub(crate) fn inflate_fast(
    state: &mut InflateState,
    input: &[u8],
    output: &mut [u8],
    in_pos: &mut usize,
    out_pos: &mut usize,
    start: usize,
) {
    // --- Debug-only precondition validation ---
    debug_assert!(
        !state.codes.is_empty(),
        "inflate_fast: Huffman code tables must be initialized before calling"
    );
    debug_assert!(
        state.lencode_idx < state.codes.len(),
        "inflate_fast: lencode_idx out of bounds"
    );
    debug_assert!(
        state.distcode_idx < state.codes.len(),
        "inflate_fast: distcode_idx out of bounds"
    );

    // --- Copy state fields to local variables for tight-loop performance ---
    // The C implementation (inffast.c lines 80-96) copies stream and state
    // fields into locals to help the compiler keep them in registers.
    let mut hold: u64 = state.hold;
    let mut bits: u32 = state.bits;
    let wsize: usize = state.wsize;
    let whave: usize = state.whave;
    let wnext: usize = state.wnext;
    let lmask: u64 = (1u64 << state.lenbits) - 1;
    let dmask: u64 = (1u64 << state.distbits) - 1;

    // --- Calculate loop boundary positions ---
    // `*in_pos < last` guarantees at least 5 more bytes of input are available
    // (6 total), enough for the maximum first-level code (15 bits = 2 bytes
    // read) plus extra bits (up to 2 more bytes read) in one iteration.
    let last: usize = input.len().saturating_sub(5);
    // `*out_pos < end` guarantees at least 257 more bytes of output space
    // (258 total), enough for the maximum match length (MAX_MATCH = 258).
    let end: usize = output.len().saturating_sub(257);

    // ===================================================================
    // Main decode loop — runs while sufficient input and output remain.
    // Structure mirrors C inffast.c lines 99-296 with labeled loops
    // replacing C's `goto dolen` / `goto dodist` patterns.
    // ===================================================================
    'outer: loop {
        // --- Bit refill: ensure at least 15 bits in hold ---
        // (inffast.c lines 101-106)
        // 15 bits is the maximum first-level Huffman code length.
        // We always read exactly 2 bytes because preconditions guarantee
        // sufficient input.
        if bits < 15 {
            hold |= (input[*in_pos] as u64) << bits;
            *in_pos += 1;
            bits += 8;
            hold |= (input[*in_pos] as u64) << bits;
            *in_pos += 1;
            bits += 8;
        }

        // --- First-level length/literal code lookup ---
        // (inffast.c lines 108-110)
        let mut here: Code = state.len_code((hold & lmask) as usize);

        // ===========================================================
        // Length/literal dispatch loop ('dolen).
        // Replaces C's `goto dolen` for 2nd-level sub-table re-lookup.
        // (inffast.c lines 112-290)
        // ===========================================================
        'dolen: loop {
            // Consume the bits for this code entry
            let code_bits = here.bits as u32;
            hold >>= code_bits;
            bits -= code_bits;
            let op = here.op as u32;

            // ---- Case 1: Literal byte (op == 0) ----
            // (inffast.c lines 113-121)
            if op == 0 {
                output[*out_pos] = here.val as u8;
                *out_pos += 1;
                break 'dolen; // back to outer loop for next code
            }

            // ---- Case 2: Length code (op & 16 != 0) ----
            // (inffast.c lines 123-260)
            if op & 16 != 0 {
                let mut len = here.val as usize; // base length
                let extra = op & 15; // number of extra bits

                // Read length extra bits if needed (up to 5 extra bits)
                if extra != 0 {
                    if bits < extra {
                        hold |= (input[*in_pos] as u64) << bits;
                        *in_pos += 1;
                        bits += 8;
                        if bits < extra {
                            hold |= (input[*in_pos] as u64) << bits;
                            *in_pos += 1;
                            bits += 8;
                        }
                    }
                    len += (hold & ((1u64 << extra) - 1)) as usize;
                    hold >>= extra;
                    bits -= extra;
                }

                // Refill hold for distance code lookup (need >= 15 bits)
                // (inffast.c lines 135-140)
                if bits < 15 {
                    hold |= (input[*in_pos] as u64) << bits;
                    *in_pos += 1;
                    bits += 8;
                    hold |= (input[*in_pos] as u64) << bits;
                    *in_pos += 1;
                    bits += 8;
                }

                // First-level distance code lookup
                here = state.dist_code((hold & dmask) as usize);

                // =======================================================
                // Distance dispatch loop ('dodist).
                // Replaces C's `goto dodist` for 2nd-level sub-table
                // re-lookup on distance codes.
                // (inffast.c lines 143-276)
                // =======================================================
                'dodist: loop {
                    let code_bits = here.bits as u32;
                    hold >>= code_bits;
                    bits -= code_bits;
                    let op = here.op as u32;

                    // ---- Distance base + extra bits (op & 16 != 0) ----
                    // (inffast.c lines 145-254)
                    if op & 16 != 0 {
                        let mut dist = here.val as usize;
                        let extra = op & 15;

                        // Read distance extra bits (up to 13 bits)
                        if bits < extra {
                            hold |= (input[*in_pos] as u64) << bits;
                            *in_pos += 1;
                            bits += 8;
                            if bits < extra {
                                hold |= (input[*in_pos] as u64) << bits;
                                *in_pos += 1;
                                bits += 8;
                            }
                        }
                        dist += (hold & ((1u64 << extra) - 1)) as usize;
                        hold >>= extra;
                        bits -= extra;

                        // Validate maximum distance (INFLATE_STRICT)
                        if state.sane && dist > state.dmax as usize {
                            state.msg = Some("invalid distance too far back".into());
                            state.mode = InflateMode::Bad;
                            break 'outer;
                        }

                        // How many bytes have been written to output so far
                        let written = *out_pos - start;

                        if dist > written {
                            // ================================================
                            // Distance reaches back into the sliding window.
                            // Three sub-cases based on window wrap state.
                            // (inffast.c lines 163-253)
                            // ================================================
                            let op = dist - written; // bytes back into window

                            if op > whave {
                                // Distance exceeds available window data —
                                // always an error (safe Rust cannot access
                                // memory beyond window bounds regardless of
                                // the sane flag).
                                state.msg = Some("invalid distance too far back".into());
                                state.mode = InflateMode::Bad;
                                break 'outer;
                            }

                            copy_from_window(state, output, out_pos, dist, len, op, wsize, wnext);
                        } else {
                            // ================================================
                            // Common fast path: copy from already-written
                            // output (no window access needed).
                            // (inffast.c lines 249-262)
                            // ================================================
                            let from = *out_pos - dist;

                            // SAFETY: The core invariant is that both the
                            // source range [from..from+len] and destination
                            // range [out_pos..out_pos+len] lie within
                            // `output[0..output.len()]`.  Specifically:
                            //
                            // 1. `from = out_pos - dist` where `dist <= written
                            //    = out_pos - start`, so `from >= start >= 0`.
                            //    The source starts within already-written output.
                            // 2. The loop condition `out_pos < end` where
                            //    `end = output.len() - 257` guarantees
                            //    `output.len() - out_pos >= 258 >= len`,
                            //    so `out_pos + len <= output.len()`.
                            // 3. `from < out_pos` and (2) together imply
                            //    `from + len <= output.len()`.
                            // 4. When `dist < len` (overlapping), `copy_match`
                            //    uses forward byte-by-byte copy, correctly
                            //    replicating the LZ77 run-length pattern.
                            unsafe {
                                copy_match(output, from, out_pos, dist, len);
                            }
                        }

                        break 'dodist;
                    }

                    // ---- 2nd-level distance code (op & 64 == 0) ----
                    // (inffast.c lines 264-267)
                    if op & 64 == 0 {
                        here = state
                            .dist_code(here.val as usize + ((hold & ((1u64 << op) - 1)) as usize));
                        continue 'dodist;
                    }

                    // ---- Invalid distance code ----
                    // (inffast.c lines 268-272)
                    state.msg = Some("invalid distance code".into());
                    state.mode = InflateMode::Bad;
                    break 'outer;
                } // end 'dodist

                break 'dolen;
            }

            // ---- Case 3: 2nd-level length code (op & 64 == 0) ----
            // (inffast.c lines 274-277)
            if op & 64 == 0 {
                here = state.len_code(here.val as usize + ((hold & ((1u64 << op) - 1)) as usize));
                continue 'dolen;
            }

            // ---- Case 4: End-of-block (op & 32 != 0) ----
            // (inffast.c lines 278-281)
            if op & 32 != 0 {
                state.mode = InflateMode::Type;
                break 'outer;
            }

            // ---- Case 5: Invalid literal/length code ----
            // (inffast.c lines 283-287)
            state.msg = Some("invalid literal/length code".into());
            state.mode = InflateMode::Bad;
            break 'outer;
        } // end 'dolen

        // --- Loop boundary check ---
        // (inffast.c line 288: `while (in < last && out < end)`)
        if *in_pos >= last || *out_pos >= end {
            break 'outer;
        }
    } // end 'outer

    // ===================================================================
    // State restoration: return unused bytes to the input stream.
    // (inffast.c lines 290-304)
    //
    // The hold buffer may contain complete bytes that were read but not
    // consumed during the last iteration. We "unread" them by backing up
    // in_pos and adjusting bits/hold accordingly.
    // ===================================================================
    let put_back = (bits >> 3) as usize;
    *in_pos -= put_back;
    bits -= (put_back as u32) << 3;
    hold &= (1u64 << bits) - 1;

    // Write locals back to state
    state.hold = hold;
    state.bits = bits;
}

/// Copies match data from the sliding window to the output buffer, handling
/// three distinct sub-cases based on the window's circular buffer state.
///
/// This function is called when a back-reference distance extends beyond the
/// data already written to the output buffer during the current inflate call,
/// requiring access to previously decompressed data stored in the sliding
/// window.
///
/// # Arguments
///
/// * `state`   — Inflate state (for window access)
/// * `output`  — Output buffer
/// * `out_pos` — Current write position in output (updated)
/// * `dist`    — Total match distance
/// * `len`     — Total match length (bytes to copy)
/// * `op`      — Bytes back into window (`dist - written`)
/// * `wsize`   — Window size
/// * `wnext`   — Window write index (next position to be written)
#[allow(dead_code)] // Called by inflate_fast; unused until mod.rs calls inflate_fast
#[allow(clippy::too_many_arguments)] // Matches C inffast.c window-copy structure
#[inline(always)]
fn copy_from_window(
    state: &InflateState,
    output: &mut [u8],
    out_pos: &mut usize,
    dist: usize,
    len: usize,
    op: usize,
    wsize: usize,
    wnext: usize,
) {
    if wnext == 0 {
        // ---- Sub-case A: No wrap-around (wnext == 0) ----
        // (inffast.c lines 175-202)
        // All valid window data is in [wsize-whave .. wsize-1].
        // The match source starts at window[wsize - op].
        let mut win_pos = wsize - op;
        if op < len {
            // Partial from window, then rest from output
            let win_copy = op;
            let remaining = len - win_copy;
            for _ in 0..win_copy {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
            // Remaining bytes come from already-written output
            let from = *out_pos - dist;
            // SAFETY: from + remaining <= output.len() because from < out_pos
            // and out_pos + remaining <= output.len() (avail_out >= 258).
            // dist >= remaining is not guaranteed, so overlapping copy is needed.
            unsafe {
                copy_match(output, from, out_pos, dist, remaining);
            }
        } else {
            // Entire match is within window
            for _ in 0..len {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
        }
    } else if wnext < op {
        // ---- Sub-case B: Wrap-around (wnext < op) ----
        // (inffast.c lines 204-230)
        // Window has wrapped. Source starts at end segment:
        //   window[wsize + wnext - op .. wsize-1], then
        //   window[0 .. wnext-1], then output if needed.
        let mut win_pos = wsize + wnext - op;
        let end_avail = op - wnext; // bytes available from end segment

        if end_avail < len {
            // Copy from end of window
            let mut remaining = len - end_avail;
            for _ in 0..end_avail {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
            // Continue from start of window
            win_pos = 0;
            if wnext < remaining {
                // Copy from start segment, then from output
                let start_copy = wnext;
                remaining -= start_copy;
                for _ in 0..start_copy {
                    output[*out_pos] = state.window[win_pos];
                    *out_pos += 1;
                    win_pos += 1;
                }
                // Rest from output (self-referencing match)
                let from = *out_pos - dist;
                // SAFETY: Same bounds guarantees as Sub-case A above.
                unsafe {
                    copy_match(output, from, out_pos, dist, remaining);
                }
            } else {
                // All remaining bytes from start of window
                for _ in 0..remaining {
                    output[*out_pos] = state.window[win_pos];
                    *out_pos += 1;
                    win_pos += 1;
                }
            }
        } else {
            // Entire match from end segment of window
            for _ in 0..len {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
        }
    } else {
        // ---- Sub-case C: Contiguous in window (wnext >= op) ----
        // (inffast.c lines 232-253)
        // No wrap needed: source at window[wnext - op].
        let mut win_pos = wnext - op;
        if op < len {
            // Partial from window, then from output
            let win_copy = op;
            let remaining = len - win_copy;
            for _ in 0..win_copy {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
            let from = *out_pos - dist;
            // SAFETY: Same bounds guarantees as Sub-case A above.
            unsafe {
                copy_match(output, from, out_pos, dist, remaining);
            }
        } else {
            // Entire match in window
            for _ in 0..len {
                output[*out_pos] = state.window[win_pos];
                *out_pos += 1;
                win_pos += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- copy_match unit tests ----

    /// Verify that copy_match handles non-overlapping copies correctly.
    #[test]
    fn test_copy_match_non_overlapping() {
        let mut buf = vec![1u8, 2, 3, 4, 5, 0, 0, 0, 0, 0];
        let mut out_pos = 5usize;
        // SAFETY: from(0)+len(3)=3 <= 10, out_pos(5)+len(3)=8 <= 10,
        // from(0) < out_pos(5), and dist(5) >= len(3) so no overlap issue.
        unsafe {
            copy_match(&mut buf, 0, &mut out_pos, 5, 3);
        }
        assert_eq!(out_pos, 8);
        assert_eq!(&buf[5..8], &[1, 2, 3]);
    }

    /// Verify that copy_match handles overlapping copies (repeating pattern).
    #[test]
    fn test_copy_match_overlapping_repeat() {
        let mut buf = vec![0u8; 20];
        buf[0] = b'A';
        let mut out_pos = 1usize;
        // SAFETY: from(0)+len(9)=9 <= 20, out_pos(1)+len(9)=10 <= 20,
        // from(0) < out_pos(1). Overlapping copy (dist=1) is intentional to
        // produce a repeating pattern; copy_match handles byte-by-byte copy
        // for dist < len.
        unsafe {
            copy_match(&mut buf, 0, &mut out_pos, 1, 9);
        }
        assert_eq!(out_pos, 10);
        assert_eq!(&buf[0..10], &[b'A'; 10]);
    }

    /// Verify that copy_match handles overlapping copies with pattern dist=2.
    #[test]
    fn test_copy_match_overlapping_pattern() {
        let mut buf = vec![0u8; 20];
        buf[0] = b'A';
        buf[1] = b'B';
        let mut out_pos = 2usize;
        // SAFETY: from(0)+len(6)=6 <= 20, out_pos(2)+len(6)=8 <= 20,
        // from(0) < out_pos(2). Overlapping copy (dist=2 < len=6) is
        // intentional to verify the repeating "AB" pattern; copy_match
        // handles byte-by-byte copy for dist < len.
        unsafe {
            copy_match(&mut buf, 0, &mut out_pos, 2, 6);
        }
        assert_eq!(out_pos, 8);
        assert_eq!(&buf[0..8], b"ABABABAB");
    }

    // ---- inflate_fast integration tests ----

    /// Helper: build a minimal InflateState with custom Huffman code tables.
    fn make_state(
        len_codes: &[Code],
        dist_codes: &[Code],
        lenbits: u32,
        distbits: u32,
    ) -> InflateState {
        let mut state = InflateState::new(15).unwrap();
        let total = len_codes.len() + dist_codes.len();
        state.codes = vec![Code::default(); total];
        state.lencode_idx = 0;
        state.distcode_idx = len_codes.len();
        state.lenbits = lenbits;
        state.distbits = distbits;
        state.codes[..len_codes.len()].copy_from_slice(len_codes);
        state.codes[len_codes.len()..total].copy_from_slice(dist_codes);
        state.mode = InflateMode::Len;
        state
    }

    /// Decode a single literal followed by end-of-block.
    #[test]
    fn test_inflate_fast_literal() {
        // In real inflate tables, EOB has op = 32|64 = 96 (set by inflate_table).
        let len_codes = vec![
            Code::new(0, 1, b'X' as u16), // code 0 => literal 'X'
            Code::new(96, 1, 0),          // code 1 => EOB (op=32+64)
        ];
        let dist_codes = vec![Code::new(0, 1, 0); 2];
        let mut state = make_state(&len_codes, &dist_codes, 1, 1);

        // bits LSB-first: 0 (literal X), 1 (EOB) = byte 0x02
        let input = vec![0x02u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(out_pos, 1);
        assert_eq!(output[0], b'X');
        assert_eq!(state.mode, InflateMode::Type);
    }

    /// Decode multiple literals followed by end-of-block.
    #[test]
    fn test_inflate_fast_multiple_literals() {
        let len_codes = vec![
            Code::new(0, 2, b'A' as u16),
            Code::new(0, 2, b'B' as u16),
            Code::new(0, 2, b'C' as u16),
            Code::new(96, 2, 0), // EOB (op=32+64)
        ];
        let dist_codes = vec![Code::new(0, 1, 0); 2];
        let mut state = make_state(&len_codes, &dist_codes, 2, 1);

        // A(00), B(01), C(10), EOB(11) LSB-first = 0b_11_10_01_00 = 0xE4
        let input = vec![0xE4u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(out_pos, 3);
        assert_eq!(&output[..3], b"ABC");
        assert_eq!(state.mode, InflateMode::Type);
    }

    /// Immediate end-of-block produces no output.
    #[test]
    fn test_inflate_fast_immediate_eob() {
        let len_codes = vec![
            Code::new(96, 1, 0), // code 0 => EOB (op=32+64)
            Code::new(0, 1, 0),
        ];
        let dist_codes = vec![Code::new(0, 1, 0); 2];
        let mut state = make_state(&len_codes, &dist_codes, 1, 1);

        let input = vec![0u8; 12];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(out_pos, 0);
        assert_eq!(state.mode, InflateMode::Type);
    }

    /// Invalid literal/length code triggers Bad mode.
    #[test]
    fn test_inflate_fast_invalid_length_code() {
        let len_codes = vec![
            Code::new(64, 1, 0), // op=64 => invalid
            Code::new(0, 1, 0),
        ];
        let dist_codes = vec![Code::new(0, 1, 0); 2];
        let mut state = make_state(&len_codes, &dist_codes, 1, 1);

        let input = vec![0u8; 12];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Bad);
        assert!(
            state
                .msg
                .as_ref()
                .unwrap()
                .contains("invalid literal/length code")
        );
    }

    /// State restoration puts bits < 8 back into hold.
    #[test]
    fn test_inflate_fast_state_restoration() {
        // With 0xFF input, hold & 1 = 1, so code index 1 is looked up.
        // Put EOB at index 1 so it's hit immediately.
        let len_codes = vec![
            Code::new(0, 1, 0),  // code 0 => literal (not reached)
            Code::new(96, 1, 0), // code 1 => EOB (op=32+64)
        ];
        let dist_codes = vec![Code::new(0, 1, 0); 2];
        let mut state = make_state(&len_codes, &dist_codes, 1, 1);
        state.hold = 0;
        state.bits = 0;

        let input = vec![0xFFu8; 12];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Type);
        assert!(
            state.bits < 8,
            "bits should be < 8 after restoration, got {}",
            state.bits
        );
    }

    /// Window copy sub-case A: wnext == 0 (no wrap).
    #[test]
    fn test_inflate_fast_window_copy_no_wrap() {
        // Length code: op=16 (length, 0 extra), bits=2, val=3 (base=3)
        let len_codes = vec![
            Code::new(16, 2, 3), // code 00 => length 3
            Code::new(96, 2, 0), // code 01 => EOB (op=32+64)
            Code::new(0, 2, 0),
            Code::new(0, 2, 0),
        ];
        // Distance code: op=16 (dist, 0 extra), bits=1, val=1 (dist=1)
        let dist_codes = vec![Code::new(16, 1, 1), Code::new(16, 1, 2)];
        let mut state = make_state(&len_codes, &dist_codes, 2, 1);
        state.wsize = 32768;
        state.whave = 5;
        state.wnext = 0;
        state.window = vec![0u8; 32768];
        state.window[32768 - 1] = b'Z';

        // Bit stream (LSB first): length(00) + distance(0) + EOB(01)
        // b0=0,b1=0, b2=0, b3=1,b4=0 → 0b00001000 = 0x08
        let input = vec![0x08u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Type);
        assert_eq!(out_pos, 3);
        assert_eq!(&output[..3], b"ZZZ");
    }

    /// Window copy sub-case C: wnext >= op (contiguous).
    #[test]
    fn test_inflate_fast_window_copy_contiguous() {
        let len_codes = vec![
            Code::new(16, 2, 3), // code 00 => length 3
            Code::new(96, 2, 0), // code 01 => EOB (op=32+64)
            Code::new(0, 2, 0),
            Code::new(0, 2, 0),
        ];
        let dist_codes = vec![
            Code::new(16, 1, 2), // dist=2
            Code::new(16, 1, 3),
        ];
        let mut state = make_state(&len_codes, &dist_codes, 2, 1);
        state.wsize = 32768;
        state.whave = 100;
        state.wnext = 50; // wnext (50) >= op (2), sub-case C
        state.window = vec![0u8; 32768];
        state.window[48] = b'Q'; // wnext - op = 50 - 2 = 48
        state.window[49] = b'R';

        // dist=2, written=0, op=2-0=2. wnext=50 >= op=2 → sub-case C.
        // win_pos = 50-2 = 48. op (2) < len (3): copy 2 from window, 1 from output.
        // output[0] = window[48] = 'Q', output[1] = window[49] = 'R'
        // Then from output: from = 2 - 2 = 0, output[2] = output[0] = 'Q'
        // Bit stream: length(00) + distance(0) + EOB(01) = 0x08
        let input = vec![0x08u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Type);
        assert_eq!(out_pos, 3);
        assert_eq!(output[0], b'Q');
        assert_eq!(output[1], b'R');
        assert_eq!(output[2], b'Q');
    }

    /// Invalid distance (too far back) triggers Bad mode.
    #[test]
    fn test_inflate_fast_invalid_distance_too_far() {
        let len_codes = vec![
            Code::new(16, 2, 3), // code 00 => length 3
            Code::new(96, 2, 0), // EOB (op=32+64)
            Code::new(0, 2, 0),
            Code::new(0, 2, 0),
        ];
        // Distance = 100, but whave = 5 and written = 0, so op = 100 > whave
        let dist_codes = vec![
            Code::new(16, 1, 100), // dist=100
            Code::new(16, 1, 1),
        ];
        let mut state = make_state(&len_codes, &dist_codes, 2, 1);
        state.wsize = 32768;
        state.whave = 5;
        state.wnext = 0;
        state.window = vec![0u8; 32768];

        // Bit stream: length(00) + distance(0) = 0x08 (EOB won't be reached)
        let input = vec![0x08u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Bad);
        assert!(
            state
                .msg
                .as_ref()
                .unwrap()
                .contains("invalid distance too far back")
        );
    }

    /// Direct output copy (common fast path, no window access).
    #[test]
    fn test_inflate_fast_direct_output_copy() {
        // First write a literal, then do a length/distance referencing it.
        // code 00 => literal 'M'
        // code 01 => length 3, 0 extra
        // code 10 => EOB (op=32+64)
        let len_codes = vec![
            Code::new(0, 2, b'M' as u16), // literal
            Code::new(16, 2, 3),          // length 3
            Code::new(96, 2, 0),          // EOB (op=32+64)
            Code::new(0, 2, 0),
        ];
        // dist code 0 => distance 1
        let dist_codes = vec![Code::new(16, 1, 1), Code::new(16, 1, 2)];
        let mut state = make_state(&len_codes, &dist_codes, 2, 1);
        state.wsize = 0; // no window
        state.whave = 0;
        state.wnext = 0;
        state.window = vec![];

        // Bit stream (LSB first): literal(00) + length(01) + dist(0) + EOB(10)
        // b0=0,b1=0, b2=1,b3=0, b4=0, b5=0,b6=1 → 0b01000100 = 0x44
        let input = vec![0x44u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Type);
        assert_eq!(out_pos, 4);
        // First byte: literal 'M'
        // Then length=3, dist=1: repeat last byte 3 times => "MMM"
        assert_eq!(&output[..4], b"MMMM");
    }

    /// Invalid distance code triggers Bad mode.
    #[test]
    fn test_inflate_fast_invalid_distance_code() {
        // Length code: length 3
        let len_codes = vec![
            Code::new(16, 1, 3), // length 3
            Code::new(32, 1, 0),
        ];
        // Distance code: op=64 (invalid)
        let dist_codes = vec![
            Code::new(64, 1, 0), // invalid distance
            Code::new(64, 1, 0),
        ];
        let mut state = make_state(&len_codes, &dist_codes, 1, 1);
        state.window = vec![0u8; 32768];
        state.wsize = 32768;
        state.whave = 32768;
        state.wnext = 0;

        let input = vec![0x00u8; 12];
        let mut output = vec![0u8; 300];
        let mut in_pos = 0;
        let mut out_pos = 0;

        inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(state.mode, InflateMode::Bad);
        assert!(
            state
                .msg
                .as_ref()
                .unwrap()
                .contains("invalid distance code")
        );
    }
}
