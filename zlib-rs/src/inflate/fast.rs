//! Fast-path inflate decoder for literal, length, and distance codes.
//!
//! This module ports `inffast.c` (321 lines) and `inffast.h` (11 lines) from
//! the C zlib library into safe Rust. The `inflate_fast` function is the
//! **performance-critical inner decode loop** of the inflate engine. When
//! sufficient input (≥ 6 bytes) and output (≥ 258 bytes) are available, the
//! main `inflate()` function dispatches here to process literal, length, and
//! distance codes at maximum speed, avoiding per-byte bounds checking.
//!
//! More than 95% of inflate execution time is spent in this routine when
//! buffers are large enough.
//!
//! # Entry Assumptions
//!
//! The caller must ensure the following invariants before calling
//! `inflate_fast`:
//!
//! - `state.mode == InflateMode::Len`
//! - At least 6 input bytes available beyond `in_pos`
//! - At least 258 output bytes available beyond `out_pos`
//! - `start <= out_pos` (start is the initial output position from `inflate()`)
//! - `state.bits < 8`
//!
//! # Return Modes
//!
//! On return, `state.mode` is one of:
//!
//! - [`InflateMode::Len`] — ran out of enough output space or input data
//! - [`InflateMode::Type`] — reached end-of-block code; `inflate()` interprets
//!   the next block
//! - [`InflateMode::Bad`] — error in block data (error message returned)
//!
//! # Performance Notes
//!
//! The implementation preserves the C zlib performance optimizations:
//!
//! - Local copies of state fields minimize struct indirection
//! - Bit loading is batched (two bytes at a time to ensure ≥ 15 bits)
//! - Match copy loops are unrolled (3 bytes per iteration)
//! - The maximum input bits used by a length/distance pair is 15 bits for
//!   the length code, 5 bits for the length extra, 15 bits for the distance
//!   code, and 13 bits for the distance extra — totaling 48 bits (6 bytes).
//!   Therefore, if at least 6 input bytes are available, no per-byte input
//!   bounds checks are needed.
//! - The maximum bytes a single length/distance pair can output is 258 bytes
//!   (the maximum match length). Therefore, if at least 258 output bytes are
//!   available, no per-byte output bounds checks are needed.
//!
//! # Safety
//!
//! This module contains zero `unsafe` blocks. All C pointer arithmetic is
//! replaced with safe Rust slice indexing.

use super::state::{InflateMode, InflateState};
use super::table::Code;

/// Fast-path decode of literal, length, and distance codes.
///
/// Called when at least 6 input bytes and 258 output bytes are available.
/// Processes codes without per-byte bounds checking for maximum throughput.
///
/// # Parameters
///
/// - `state` — Mutable reference to the inflate decompression state. On
///   entry, `state.mode` must be [`InflateMode::Len`]. On return, the mode
///   will be one of `Len`, `Type`, or `Bad`.
/// - `input` — The full input byte buffer.
/// - `output` — The full output byte buffer.
/// - `in_pos` — Current read position in `input`. Updated on return to
///   reflect consumed bytes (some bytes may be returned to the stream).
/// - `out_pos` — Current write position in `output`. Updated on return.
/// - `in_end` — Last safe input position, typically
///   `in_pos_start + (avail_in - 5)`. The loop reads up to 5 bytes ahead
///   without individual bounds checks.
/// - `out_end` — Last safe output position, typically
///   `out_pos_start + (avail_out - 257)`. The loop writes up to 258 bytes
///   ahead without individual bounds checks.
/// - `start` — The output position where `inflate()` began writing. Used to
///   determine the maximum backward distance available in the output buffer
///   (bytes before `start` are not valid for match copying).
///
/// # Returns
///
/// `None` if decoding completed without error (mode is `Len` or `Type`).
/// `Some(msg)` if a data error was detected (mode set to `Bad`), where `msg`
/// is a static diagnostic string matching the C zlib error messages exactly.
#[inline]
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn inflate_fast(
    state: &mut InflateState,
    input: &[u8],
    output: &mut [u8],
    in_pos: &mut usize,
    out_pos: &mut usize,
    in_end: usize,
    out_end: usize,
    start: usize,
) -> Option<&'static str> {
    // ── Copy state to local variables for speed ────────────────────────────
    // This mirrors inffast.c lines 77–96 where state fields are cached in
    // register-friendly locals to minimize struct indirection in the hot loop.
    let mut hold: u64 = state.hold;
    let mut bits: u32 = state.bits;
    let wsize: usize = state.wsize as usize;
    let whave: usize = state.whave as usize;
    let wnext: usize = state.wnext as usize;

    // Masks for root-level code table lookup. Length codes use `lenbits`
    // index bits; distance codes use `distbits` index bits. A mask of
    // `(1 << N) - 1` extracts the low N bits of the hold register.
    let lmask: u64 = (1u64 << state.lenbits) - 1;
    let dmask: u64 = (1u64 << state.distbits) - 1;

    let mut error_msg: Option<&'static str> = None;

    // ── Main decode loop ───────────────────────────────────────────────────
    // Decode literals and length/distance pairs until end-of-block, or until
    // not enough input data or output space remains. This is a `do-while`
    // equivalent: the condition is checked at the bottom.
    //
    // The loop structure uses Rust labeled loops to replace the C `goto`
    // labels `dolen` and `dodist` with `continue 'dolen` and `continue
    // 'dodist` for multi-level code table re-lookups.
    'outer: loop {
        // Ensure at least 15 bits in the accumulator for the length/literal
        // code lookup (maximum root table bits). Load two bytes.
        if bits < 15 {
            hold |= u64::from(input[*in_pos]) << bits;
            *in_pos += 1;
            bits += 8;
            hold |= u64::from(input[*in_pos]) << bits;
            *in_pos += 1;
            bits += 8;
        }

        // Look up the length/literal code from the root table.
        let mut here: Code = state.len_code((hold & lmask) as u32);

        // `'dolen` loop: process the length/literal code entry. If a
        // 2nd-level table lookup is needed (op & 64 == 0 and op != 0),
        // `continue 'dolen` re-enters with the updated `here`.
        'dolen: loop {
            // Drop the bits consumed by this code entry.
            let drop_bits = u32::from(here.bits);
            hold >>= drop_bits;
            bits -= drop_bits;
            let mut op = u32::from(here.op);

            if op == 0 {
                // ── Literal byte ───────────────────────────────────────
                // op == 0 means this is a literal; val is the byte value.
                // Write it directly to the output buffer.
                output[*out_pos] = here.val as u8;
                *out_pos += 1;
                break 'dolen;
            }

            if op & 16 != 0 {
                // ── Length base code ────────────────────────────────────
                // op bit 4 set (op & 16) means this is a length code.
                // val is the base match length; low 4 bits of op give
                // the number of extra bits to read.
                let mut len: u32 = u32::from(here.val);
                op &= 15; // number of extra bits for length
                if op != 0 {
                    if bits < op {
                        hold |= u64::from(input[*in_pos]) << bits;
                        *in_pos += 1;
                        bits += 8;
                    }
                    len += (hold as u32) & ((1u32 << op) - 1);
                    hold >>= op;
                    bits -= op;
                }

                // Ensure at least 15 bits for the distance code lookup.
                if bits < 15 {
                    hold |= u64::from(input[*in_pos]) << bits;
                    *in_pos += 1;
                    bits += 8;
                    hold |= u64::from(input[*in_pos]) << bits;
                    *in_pos += 1;
                    bits += 8;
                }

                // Look up the distance code from the root table.
                here = state.dist_code((hold & dmask) as u32);

                // `'dodist` loop: process the distance code entry. If a
                // 2nd-level table lookup is needed, `continue 'dodist`
                // re-enters with the updated `here`.
                'dodist: loop {
                    let drop_bits = u32::from(here.bits);
                    hold >>= drop_bits;
                    bits -= drop_bits;
                    op = u32::from(here.op);

                    if op & 16 != 0 {
                        // ── Distance base code ─────────────────────────
                        // op bit 4 set means this is a distance code.
                        // val is the base distance; low 4 bits of op
                        // give the number of extra bits.
                        let mut dist: u32 = u32::from(here.val);
                        op &= 15; // number of extra bits for distance
                        if bits < op {
                            hold |= u64::from(input[*in_pos]) << bits;
                            *in_pos += 1;
                            bits += 8;
                            if bits < op {
                                hold |= u64::from(input[*in_pos]) << bits;
                                *in_pos += 1;
                                bits += 8;
                            }
                        }
                        dist += (hold as u32) & ((1u32 << op) - 1);
                        hold >>= op;
                        bits -= op;

                        // Convert distance to usize for safe indexing.
                        let dist = dist as usize;

                        // Maximum backward distance available in the
                        // already-written output buffer.
                        let out_dist = *out_pos - start;

                        if dist > out_dist {
                            // ── Copy from sliding window ───────────────
                            // The match source extends before the current
                            // output, so some (or all) bytes must come
                            // from the sliding window buffer.
                            let window_back = dist - out_dist;

                            if window_back > whave {
                                if state.sane {
                                    state.mode = InflateMode::Bad;
                                    error_msg = Some("invalid distance too far back");
                                    break 'outer;
                                }
                                // If !sane, invalid distance is tolerated
                                // (testing mode via inflateUndermine).
                                // Not entering this path keeps behaviour
                                // safe — we simply break.
                                break 'outer;
                            }

                            let mut remaining = len as usize;

                            // Determine window copy source and count.
                            // Three sub-cases handle the circular window:
                            //   1. wnext == 0: data at end of window
                            //   2. wnext < window_back: wrap-around
                            //   3. else: contiguous in window
                            //
                            // After sub-case logic, `from_window` and
                            // `src_idx` describe where the shared unrolled
                            // copy should read remaining bytes from.
                            let (from_window, mut src_idx) = copy_from_window_subcases(
                                state,
                                output,
                                out_pos,
                                &mut remaining,
                                window_back,
                                wsize,
                                wnext,
                                dist,
                            );

                            // Shared unrolled copy for the remaining bytes
                            // (3 bytes per iteration, then 1–2 remainder).
                            if from_window {
                                unrolled_copy_from_window(
                                    state,
                                    output,
                                    out_pos,
                                    &mut src_idx,
                                    remaining,
                                );
                            } else {
                                unrolled_copy_from_output(output, out_pos, &mut src_idx, remaining);
                            }
                        } else {
                            // ── Copy direct from output ────────────────
                            // The match source is entirely within the
                            // already-written output. This is the most
                            // common case for small distances.
                            let mut from = *out_pos - dist;
                            let mut remaining = len as usize;
                            // Minimum match length is 3 (MIN_MATCH).
                            // Use do-while semantics (always ≥ 1 iter).
                            loop {
                                output[*out_pos] = output[from];
                                *out_pos += 1;
                                from += 1;
                                output[*out_pos] = output[from];
                                *out_pos += 1;
                                from += 1;
                                output[*out_pos] = output[from];
                                *out_pos += 1;
                                from += 1;
                                remaining -= 3;
                                if remaining <= 2 {
                                    break;
                                }
                            }
                            if remaining > 0 {
                                output[*out_pos] = output[from];
                                *out_pos += 1;
                                from += 1;
                                if remaining > 1 {
                                    output[*out_pos] = output[from];
                                    *out_pos += 1;
                                }
                            }
                        }
                        break 'dodist;
                    }

                    if op & 64 == 0 {
                        // ── 2nd-level distance code ────────────────────
                        // The entry is a sub-table link. val is the offset
                        // to the sub-table; the low `op` bits of hold
                        // index into it.
                        here = state
                            .dist_code(u32::from(here.val) + ((hold as u32) & ((1u32 << op) - 1)));
                        continue 'dodist;
                    }

                    // ── Invalid distance code ──────────────────────────
                    state.mode = InflateMode::Bad;
                    error_msg = Some("invalid distance code");
                    break 'outer;
                }

                break 'dolen;
            }

            if op & 64 == 0 {
                // ── 2nd-level length code ──────────────────────────────
                // The entry is a sub-table link. val is the offset to the
                // sub-table; the low `op` bits of hold index into it.
                here = state.len_code(u32::from(here.val) + ((hold as u32) & ((1u32 << op) - 1)));
                continue 'dolen;
            }

            if op & 32 != 0 {
                // ── End-of-block ───────────────────────────────────────
                // Transition to Type mode so inflate() processes the next
                // block header.
                state.mode = InflateMode::Type;
                break 'outer;
            }

            // ── Invalid literal/length code ────────────────────────────
            state.mode = InflateMode::Bad;
            error_msg = Some("invalid literal/length code");
            break 'outer;
        }

        // ── do-while condition ─────────────────────────────────────────────
        // Continue while there is enough input and output remaining.
        if *in_pos >= in_end || *out_pos >= out_end {
            break 'outer;
        }
    }

    // ── Post-loop cleanup ──────────────────────────────────────────────────
    // Return unused bytes to the input stream. On entry, bits < 8, so
    // in_pos won't retreat past the beginning of the consumed data.
    let unused = (bits >> 3) as usize;
    *in_pos -= unused;
    bits -= (unused as u32) << 3;
    hold &= (1u64 << bits) - 1;

    // Write cached locals back to state.
    state.hold = hold;
    state.bits = bits;
    state.back = 0;

    error_msg
}

// ── Helper: Window sub-case copy logic ─────────────────────────────────────
//
// Extracted from the match copy section to keep the main function's nesting
// depth manageable. Handles the three circular-window sub-cases from
// inffast.c lines 197–236 and returns the source descriptor for the shared
// unrolled copy.

/// Perform the per-byte copy from the sliding window for the three circular
/// buffer sub-cases, returning the source descriptor for the remaining
/// unrolled copy.
///
/// Returns `(from_window, src_idx)`:
/// - `from_window == true`: remaining bytes should be read from
///   `state.window[src_idx..]`.
/// - `from_window == false`: remaining bytes should be read from
///   `output[src_idx..]`.
///
/// `remaining` is updated to reflect bytes already copied by this function.
#[inline]
fn copy_from_window_subcases(
    state: &InflateState,
    output: &mut [u8],
    out_pos: &mut usize,
    remaining: &mut usize,
    window_back: usize,
    wsize: usize,
    wnext: usize,
    dist: usize,
) -> (bool, usize) {
    if wnext == 0 {
        // ── Sub-case A: wnext == 0 (very common) ──────────────────────
        // The window write pointer hasn't wrapped yet. Valid data occupies
        // window[wsize - window_back .. wsize].
        let win_start = wsize - window_back;
        if window_back < *remaining {
            // Partial from window, rest from output.
            *remaining -= window_back;
            for i in 0..window_back {
                output[*out_pos] = state.window[win_start + i];
                *out_pos += 1;
            }
            (false, *out_pos - dist)
        } else {
            // All remaining bytes come from window.
            (true, win_start)
        }
    } else if wnext < window_back {
        // ── Sub-case B: wnext < window_back (wrap around) ─────────────
        // The needed data spans the end and the start of the circular
        // window buffer:
        //   end portion:   window[wsize + wnext - window_back .. wsize]
        //   start portion: window[0 .. wnext]
        let end_start = wsize + wnext - window_back;
        let end_chunk = window_back - wnext;

        if end_chunk < *remaining {
            // Copy from end of window.
            *remaining -= end_chunk;
            for i in 0..end_chunk {
                output[*out_pos] = state.window[end_start + i];
                *out_pos += 1;
            }
            // Now consider bytes from start of window.
            if wnext < *remaining {
                // Partial from start of window, rest from output.
                *remaining -= wnext;
                for i in 0..wnext {
                    output[*out_pos] = state.window[i];
                    *out_pos += 1;
                }
                (false, *out_pos - dist)
            } else {
                // All remaining bytes come from start of window.
                (true, 0)
            }
        } else {
            // All remaining bytes come from end of window.
            (true, end_start)
        }
    } else {
        // ── Sub-case C: contiguous in window ──────────────────────────
        // window_back ≤ wnext, so the data is a single contiguous
        // range: window[wnext - window_back .. wnext].
        let win_start = wnext - window_back;
        if window_back < *remaining {
            *remaining -= window_back;
            for i in 0..window_back {
                output[*out_pos] = state.window[win_start + i];
                *out_pos += 1;
            }
            (false, *out_pos - dist)
        } else {
            (true, win_start)
        }
    }
}

// ── Helper: Unrolled copy from window ──────────────────────────────────────

/// Copy `count` bytes from `state.window[src..]` into `output[out_pos..]`,
/// using an unrolled loop that processes 3 bytes per iteration.
///
/// This matches the unrolled copy pattern at inffast.c lines 237–247.
#[inline]
fn unrolled_copy_from_window(
    state: &InflateState,
    output: &mut [u8],
    out_pos: &mut usize,
    src: &mut usize,
    mut count: usize,
) {
    while count > 2 {
        output[*out_pos] = state.window[*src];
        *out_pos += 1;
        *src += 1;
        output[*out_pos] = state.window[*src];
        *out_pos += 1;
        *src += 1;
        output[*out_pos] = state.window[*src];
        *out_pos += 1;
        *src += 1;
        count -= 3;
    }
    if count > 0 {
        output[*out_pos] = state.window[*src];
        *out_pos += 1;
        *src += 1;
        if count > 1 {
            output[*out_pos] = state.window[*src];
            *out_pos += 1;
        }
    }
}

// ── Helper: Unrolled copy from output ──────────────────────────────────────

/// Copy `count` bytes from `output[src..]` into `output[out_pos..]`,
/// using an unrolled loop that processes 3 bytes per iteration.
///
/// Handles overlapping copies correctly (when `dist < len`) because each
/// byte is read before being written — the source index is always less than
/// the destination index.
///
/// This matches the unrolled copy pattern at inffast.c lines 237–247 and
/// lines 250–261.
#[inline]
fn unrolled_copy_from_output(
    output: &mut [u8],
    out_pos: &mut usize,
    src: &mut usize,
    mut count: usize,
) {
    while count > 2 {
        output[*out_pos] = output[*src];
        *out_pos += 1;
        *src += 1;
        output[*out_pos] = output[*src];
        *out_pos += 1;
        *src += 1;
        output[*out_pos] = output[*src];
        *out_pos += 1;
        *src += 1;
        count -= 3;
    }
    if count > 0 {
        output[*out_pos] = output[*src];
        *out_pos += 1;
        *src += 1;
        if count > 1 {
            output[*out_pos] = output[*src];
            *out_pos += 1;
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::state::CodeTableRef;
    use crate::inflate::table::Code;

    /// Build a minimal `InflateState` configured for testing `inflate_fast`.
    fn make_test_state(
        lencode: &'static [Code],
        distcode: &'static [Code],
        lenbits: u32,
        distbits: u32,
    ) -> InflateState {
        let mut s = InflateState::new();
        s.mode = InflateMode::Len;
        s.lencode = CodeTableRef::Fixed(lencode);
        s.distcode = CodeTableRef::Fixed(distcode);
        s.lenbits = lenbits;
        s.distbits = distbits;
        s.hold = 0;
        s.bits = 0;
        s.sane = true;
        s.back = -1;
        s
    }

    // A minimal length code table: all entries map to literal byte 0x41 ('A').
    // op=0 means literal, bits=8, val=0x41.
    static LITERAL_A_TABLE: [Code; 512] = [Code::new(0, 8, 0x41); 512];

    // A minimal distance code table (unused for literal-only tests).
    static DUMMY_DIST: [Code; 32] = [Code::new(64, 5, 0); 32];

    /// Helper: call `inflate_fast` with pre-computed end positions to avoid
    /// borrow-checker issues with `&mut output` and `output.len()`.
    fn call_inflate_fast(
        state: &mut InflateState,
        input: &[u8],
        output: &mut [u8],
        in_pos: &mut usize,
        out_pos: &mut usize,
        start: usize,
    ) -> Option<&'static str> {
        let in_end = input.len().saturating_sub(5);
        let out_end = output.len().saturating_sub(257);
        inflate_fast(
            state, input, output, in_pos, out_pos, in_end, out_end, start,
        )
    }

    #[test]
    fn decode_single_literal() {
        let mut state = make_test_state(&LITERAL_A_TABLE, &DUMMY_DIST, 9, 5);
        // Input: one byte 0x41 (will be consumed as code bits) + padding.
        // We need at least 6 bytes of input and 258 bytes of output.
        let input = [0x41u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = [0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;

        let msg = call_inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert!(msg.is_none(), "Expected no error, got: {msg:?}");
        // At least one literal should have been written.
        assert!(out_pos > 0, "Expected output, got out_pos=0");
        assert_eq!(output[0], 0x41, "First output byte should be 'A'");
    }

    #[test]
    fn end_of_block_sets_type_mode() {
        // Build a code table where entry 0 is end-of-block:
        // op = 32 + 64 = 96 (end-of-block), bits = 7, val = 256
        let mut eob_table = [Code::new(0, 8, 0x41); 512];
        eob_table[0] = Code::new(96, 7, 256);
        // Leak into static for test (acceptable in test context).
        let table: &'static [Code] = Box::leak(Box::new(eob_table));

        let mut state = make_test_state(table, &DUMMY_DIST, 9, 5);
        // Input bytes where the low 9 bits of hold decode to index 0.
        let input = [0u8; 10];
        let mut output = [0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;

        let msg = call_inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert!(msg.is_none());
        assert_eq!(state.mode, InflateMode::Type);
    }

    #[test]
    fn invalid_length_code_sets_bad_mode() {
        // Build a code table where entry 0 is invalid:
        // op = 64 (invalid, not end-of-block which requires bit 5 = 32)
        let mut bad_table = [Code::new(0, 8, 0); 512];
        bad_table[0] = Code::new(64, 7, 0);
        let table: &'static [Code] = Box::leak(Box::new(bad_table));

        let mut state = make_test_state(table, &DUMMY_DIST, 9, 5);
        let input = [0u8; 10];
        let mut output = [0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;

        let msg = call_inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(msg, Some("invalid literal/length code"));
        assert_eq!(state.mode, InflateMode::Bad);
    }

    #[test]
    fn state_fields_restored_after_decode() {
        let mut state = make_test_state(&LITERAL_A_TABLE, &DUMMY_DIST, 9, 5);
        state.hold = 0;
        state.bits = 0;
        state.back = -1;

        let input = [0x41u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut output = [0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;

        call_inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        // back should be reset to 0 by inflate_fast.
        assert_eq!(state.back, 0);
        // bits should be less than 8 (post-loop unused byte return).
        assert!(state.bits < 8, "bits should be < 8, got {}", state.bits);
    }

    #[test]
    fn invalid_distance_too_far_back() {
        // Create a length code: op=16 (length flag), bits=8, val=3 (len=3),
        // no extra bits needed (op & 15 == 0).
        let mut len_table = [Code::new(0, 8, 0x41); 512];
        len_table[0] = Code::new(16, 8, 3); // length=3, 0 extra bits

        // Distance code: op=16 (distance flag), bits=5, val=100 (dist=100),
        // no extra bits (op & 15 == 0).
        let mut dist_table = [Code::new(64, 5, 0); 32];
        dist_table[0] = Code::new(16, 5, 100); // dist=100, 0 extra bits

        let len_ref: &'static [Code] = Box::leak(Box::new(len_table));
        let dist_ref: &'static [Code] = Box::leak(Box::new(dist_table));

        let mut state = make_test_state(len_ref, dist_ref, 9, 5);
        state.sane = true;
        // No window data — whave = 0, wsize = 0
        state.whave = 0;
        state.wsize = 0;

        let input = [0u8; 20];
        let mut output = [0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;

        let msg = call_inflate_fast(
            &mut state,
            &input,
            &mut output,
            &mut in_pos,
            &mut out_pos,
            0,
        );

        assert_eq!(msg, Some("invalid distance too far back"));
        assert_eq!(state.mode, InflateMode::Bad);
    }
}
