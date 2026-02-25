//! Callback-based DEFLATE decompression (`inflateBack`).
//!
//! This module provides a callback-driven interface for raw DEFLATE decompression.
//! Instead of using the streaming `ZStream` buffer protocol, the caller supplies
//! input and output callbacks. This is used by gzip utilities and unzip programs.
//!
//! In contrast to the standard `inflate()` function, `inflate_back()` only handles
//! raw DEFLATE blocks — no zlib or gzip headers/trailers are processed.
//!
//! # C Equivalent
//!
//! This module is a complete Rust port of `infback.c` from zlib 1.3.2.1-motley.
//! The C function pointer callbacks (`in_func`, `out_func`) are replaced by
//! `FnMut` closures for safety and ergonomics.

// MAX_MATCH (258) is the maximum DEFLATE match length; used for inflate_fast
// threshold check and match length validation bounds.
#[allow(unused_imports)]
use crate::constants::MAX_MATCH;
use crate::constants::MAX_WBITS;
use crate::error::ReturnCode;
use crate::stream::ZStream;
use super::fixed::inflate_fixed;
use super::state::{CodeTableRef, InflateMode, InflateState};
use super::table::{CodeType, inflate_table, ENOUGH};

// inflate_fast is imported for future optimization integration; the current
// implementation uses the complete slow-path decode to avoid aliasing issues
// between the window buffer (which serves as BOTH the sliding window and the
// output buffer in inflate_back) and the state's window reference that
// inflate_fast reads for backward distance copies.
#[allow(unused_imports)]
use super::fast::inflate_fast;
#[allow(unused_imports)]
use super::table::{Code, MAXBITS};

/// Code length alphabet order for dynamic block header decoding.
///
/// Specifies the order in which code length code lengths are stored
/// in the DEFLATE dynamic block header (RFC 1951, section 3.2.7).
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Extracts the low `n` bits from the bit accumulator `hold`.
///
/// Equivalent to the C macro `BITS(n)` in `infback.c`.
#[inline(always)]
fn bits_val(hold: u64, n: u32) -> u32 {
    (hold as u32) & ((1u32 << n).wrapping_sub(1))
}

/// Initialize inflate state for callback-based raw DEFLATE decompression.
///
/// Sets up the internal state for subsequent calls to [`inflate_back`].
/// The window buffer inside `state` is resized to `1 << window_bits` bytes
/// and serves as both the output buffer and the sliding window for backward
/// distance references.
///
/// # Arguments
///
/// * `state` — Mutable reference to the inflate state to initialize.
/// * `window_bits` — Base-2 logarithm of the sliding window size (8..=15).
///
/// # Returns
///
/// * [`ReturnCode::Ok`] on success.
/// * [`ReturnCode::StreamError`] if `window_bits` is outside the valid range.
///
/// # C Equivalent
///
/// `inflateBackInit_()` from `infback.c` lines 25–64.
pub fn inflate_back_init(state: &mut InflateState, window_bits: i32) -> ReturnCode {
    if window_bits < 8 || window_bits > MAX_WBITS as i32 {
        return ReturnCode::StreamError;
    }

    let wbits = window_bits as u32;
    let wsize = 1u32 << wbits;

    state.dmax = 32768;
    state.wbits = wbits;
    state.wsize = wsize;
    state.window.resize(wsize as usize, 0);
    state.wnext = 0;
    state.whave = 0;
    state.sane = true;
    state.back = 0;
    state.mode = InflateMode::Type;
    state.last = false;

    ReturnCode::Ok
}

/// Perform callback-based raw DEFLATE decompression.
///
/// Decompresses a raw DEFLATE stream using caller-supplied callbacks for
/// input and output. The caller's window buffer (inside `state`) is used
/// as both the output staging area and the sliding window for backward
/// distance references.
///
/// # Arguments
///
/// * `state` — Mutable reference to an initialized inflate state.
/// * `stream` — Mutable reference to the stream for error message reporting.
/// * `input_cb` — Input callback: returns a `Vec<u8>` of new input data.
///   An empty vector signals end-of-input (triggers `BufError`).
/// * `output_cb` — Output callback: receives a slice of decompressed data.
///   Returns `true` on success, `false` on write failure (triggers `BufError`).
///
/// # Returns
///
/// * [`ReturnCode::StreamEnd`] — Decompression completed successfully.
/// * [`ReturnCode::DataError`] — Corrupt or invalid deflate stream.
/// * [`ReturnCode::BufError`] — Input callback returned empty or output callback failed.
/// * [`ReturnCode::StreamError`] — Invalid internal state.
///
/// # C Equivalent
///
/// `inflateBack()` from `infback.c` lines 191–567.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub fn inflate_back(
    state: &mut InflateState,
    stream: &mut ZStream,
    input_cb: &mut dyn FnMut() -> Vec<u8>,
    output_cb: &mut dyn FnMut(&[u8]) -> bool,
) -> ReturnCode {
    // Reset error message
    stream.msg = None;

    // Ensure we start in a valid mode for inflate_back
    state.mode = InflateMode::Type;
    state.last = false;

    // Local copies of frequently accessed state (mirrors C LOAD macro)
    let wsize = state.wsize as usize;
    let mut hold: u64 = state.hold;
    let mut bits: u32 = state.bits;

    // Input buffer management
    let mut input_data: Vec<u8> = Vec::new();
    let mut input_pos: usize = 0;
    let mut have: usize = 0;

    // Output: write directly into state.window
    // put = current write index, left = remaining space
    let mut put: usize = 0;
    let mut left: usize = wsize;

    let mut ret;

    // Main decompression loop (replaces C switch/case with goto inf_leave)
    'main_loop: loop {
        match state.mode {
            // ----------------------------------------------------------------
            // TYPE: Determine and dispatch block type
            // C infback.c lines 228–260
            // ----------------------------------------------------------------
            InflateMode::Type => {
                if state.last {
                    // Last block — align to byte boundary and finish
                    let drop = bits & 7;
                    hold >>= drop;
                    bits -= drop;
                    state.mode = InflateMode::Done;
                    continue;
                }

                // NEEDBITS(3): need last-flag (1 bit) + block type (2 bits)
                while bits < 3 {
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }
                    hold |= u64::from(input_data[input_pos]) << bits;
                    input_pos += 1;
                    have -= 1;
                    bits += 8;
                }

                // Extract last-block flag
                state.last = (hold & 1) != 0;
                hold >>= 1;
                bits -= 1;

                // Extract block type (2 bits)
                match bits_val(hold, 2) {
                    0 => {
                        // Stored block
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        // Fixed Huffman block — set up fixed tables
                        inflate_fixed(state);
                        state.mode = InflateMode::Len;
                    }
                    2 => {
                        // Dynamic Huffman block
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        // Type 3 is invalid
                        stream.msg = Some("invalid block type");
                        state.mode = InflateMode::Bad;
                    }
                }
                hold >>= 2;
                bits -= 2;
            }

            // ----------------------------------------------------------------
            // STORED: Copy stored (uncompressed) block data
            // C infback.c lines 262–292
            // ----------------------------------------------------------------
            InflateMode::Stored => {
                // Align to byte boundary (BYTEBITS)
                let drop = bits & 7;
                hold >>= drop;
                bits -= drop;

                // NEEDBITS(32): read LEN and NLEN (4 bytes)
                while bits < 32 {
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }
                    hold |= u64::from(input_data[input_pos]) << bits;
                    input_pos += 1;
                    have -= 1;
                    bits += 8;
                }

                // Validate: LEN must equal ~NLEN
                let len = hold as u32 & 0xFFFF;
                if len != ((hold as u32 >> 16) ^ 0xFFFF) {
                    stream.msg = Some("invalid stored block lengths");
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.length = len;

                // Clear bit buffer (INITBITS)
                hold = 0;
                bits = 0;

                // Copy stored data using callbacks
                // Mirrors the C code flow: calculate copy → ROOM → PULL → recalculate → memcpy
                while state.length > 0 {
                    // ROOM: flush window if full (before pulling input)
                    if left == 0 {
                        state.whave = state.wsize;
                        if !output_cb(&state.window[..wsize]) {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                        put = 0;
                        left = wsize;
                    }

                    // PULL: get more input if needed
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }

                    // Calculate copy amount after ROOM/PULL
                    let mut copy = state.length as usize;
                    if copy > have {
                        copy = have;
                    }
                    if copy > left {
                        copy = left;
                    }

                    // Copy bytes from input to window
                    state.window[put..put + copy]
                        .copy_from_slice(&input_data[input_pos..input_pos + copy]);
                    have -= copy;
                    input_pos += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                }

                state.mode = InflateMode::Type;
            }

            // ----------------------------------------------------------------
            // TABLE: Read dynamic Huffman table descriptor and build tables
            // C infback.c lines 294–432 (includes inline code length decoding)
            // ----------------------------------------------------------------
            InflateMode::Table => {
                // NEEDBITS(14): nlen(5) + ndist(5) + ncode(4)
                while bits < 14 {
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }
                    hold |= u64::from(input_data[input_pos]) << bits;
                    input_pos += 1;
                    have -= 1;
                    bits += 8;
                }

                state.nlen = bits_val(hold, 5) + 257;
                hold >>= 5;
                bits -= 5;

                state.ndist = bits_val(hold, 5) + 1;
                hold >>= 5;
                bits -= 5;

                state.ncode = bits_val(hold, 4) + 4;
                hold >>= 4;
                bits -= 4;

                if state.nlen > 286 || state.ndist > 30 {
                    stream.msg = Some("too many length or distance symbols");
                    state.mode = InflateMode::Bad;
                    continue;
                }

                // Read code length code lengths (3 bits each, in ORDER)
                state.have = 0;
                while state.have < state.ncode {
                    // NEEDBITS(3)
                    while bits < 3 {
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }
                    state.lens[ORDER[state.have as usize] as usize] =
                        bits_val(hold, 3) as u16;
                    hold >>= 3;
                    bits -= 3;
                    state.have += 1;
                }

                // Zero remaining code length entries
                while state.have < 19 {
                    state.lens[ORDER[state.have as usize] as usize] = 0;
                    state.have += 1;
                }

                // Build the code lengths table
                state.next = 0;
                state.codes.resize(ENOUGH, super::table::Code { op: 0, bits: 0, val: 0 });
                let mut lenbits_temp = 7u32;
                let build_result = inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    0,
                    &mut lenbits_temp,
                    &mut state.work,
                );
                state.lenbits = lenbits_temp;
                state.lencode = CodeTableRef::Dynamic(0);

                match build_result {
                    Ok(used) => {
                        state.next = used;
                    }
                    Err(_) => {
                        stream.msg = Some("invalid code lengths set");
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                }

                // Decode length and distance code code lengths
                state.have = 0;
                let total_codes = (state.nlen + state.ndist) as u32;
                while state.have < total_codes {
                    // Decode using code length table
                    let here;
                    loop {
                        let idx = bits_val(hold, state.lenbits);
                        let entry = state.len_code(idx);
                        if u32::from(entry.bits) <= bits {
                            here = entry;
                            break;
                        }
                        // PULLBYTE
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }

                    let val = here.val;
                    if val < 16 {
                        // Literal code length
                        hold >>= u32::from(here.bits);
                        bits -= u32::from(here.bits);
                        state.lens[state.have as usize] = val;
                        state.have += 1;
                    } else {
                        // Repeat code
                        let (copy_val, copy_count_extra, min_have);
                        match val {
                            16 => {
                                // NEEDBITS(here.bits + 2)
                                let need = u32::from(here.bits) + 2;
                                while bits < need {
                                    if have == 0 {
                                        input_data = input_cb();
                                        have = input_data.len();
                                        input_pos = 0;
                                        if have == 0 {
                                            ret = ReturnCode::BufError;
                                            break 'main_loop;
                                        }
                                    }
                                    hold |= u64::from(input_data[input_pos]) << bits;
                                    input_pos += 1;
                                    have -= 1;
                                    bits += 8;
                                }
                                hold >>= u32::from(here.bits);
                                bits -= u32::from(here.bits);
                                if state.have == 0 {
                                    stream.msg = Some("invalid bit length repeat");
                                    state.mode = InflateMode::Bad;
                                    break; // break from while loop, continue main_loop
                                }
                                copy_val = state.lens[state.have as usize - 1];
                                copy_count_extra = 3 + bits_val(hold, 2) as usize;
                                hold >>= 2;
                                bits -= 2;
                                min_have = 0; // Not used in validation here
                            }
                            17 => {
                                // NEEDBITS(here.bits + 3)
                                let need = u32::from(here.bits) + 3;
                                while bits < need {
                                    if have == 0 {
                                        input_data = input_cb();
                                        have = input_data.len();
                                        input_pos = 0;
                                        if have == 0 {
                                            ret = ReturnCode::BufError;
                                            break 'main_loop;
                                        }
                                    }
                                    hold |= u64::from(input_data[input_pos]) << bits;
                                    input_pos += 1;
                                    have -= 1;
                                    bits += 8;
                                }
                                hold >>= u32::from(here.bits);
                                bits -= u32::from(here.bits);
                                copy_val = 0;
                                copy_count_extra = 3 + bits_val(hold, 3) as usize;
                                hold >>= 3;
                                bits -= 3;
                                min_have = 0;
                            }
                            _ => {
                                // val == 18
                                // NEEDBITS(here.bits + 7)
                                let need = u32::from(here.bits) + 7;
                                while bits < need {
                                    if have == 0 {
                                        input_data = input_cb();
                                        have = input_data.len();
                                        input_pos = 0;
                                        if have == 0 {
                                            ret = ReturnCode::BufError;
                                            break 'main_loop;
                                        }
                                    }
                                    hold |= u64::from(input_data[input_pos]) << bits;
                                    input_pos += 1;
                                    have -= 1;
                                    bits += 8;
                                }
                                hold >>= u32::from(here.bits);
                                bits -= u32::from(here.bits);
                                copy_val = 0;
                                copy_count_extra = 11 + bits_val(hold, 7) as usize;
                                hold >>= 7;
                                bits -= 7;
                                min_have = 0;
                            }
                        }

                        let _ = min_have; // suppress unused warning

                        // Check if copy exceeds available code count
                        if state.have + copy_count_extra as u32 > total_codes {
                            stream.msg = Some("invalid bit length repeat");
                            state.mode = InflateMode::Bad;
                            break; // break while, continue main_loop
                        }

                        // Fill the repeated code lengths
                        for _ in 0..copy_count_extra {
                            state.lens[state.have as usize] = copy_val;
                            state.have += 1;
                        }
                    }
                }

                // If we broke out due to Bad mode, continue the main loop
                if matches!(state.mode, InflateMode::Bad) {
                    continue;
                }

                // Check for end-of-block code (code 256 must be present)
                if state.lens[256] == 0 {
                    stream.msg = Some("invalid code -- missing end-of-block");
                    state.mode = InflateMode::Bad;
                    continue;
                }

                // Build literal/length table
                state.next = 0;
                let nlen = state.nlen as usize;
                let mut lenbits_temp2 = 9u32;
                let lens_build = inflate_table(
                    CodeType::Lens,
                    &state.lens[..nlen],
                    nlen,
                    &mut state.codes,
                    0,
                    &mut lenbits_temp2,
                    &mut state.work,
                );
                state.lenbits = lenbits_temp2;
                state.lencode = CodeTableRef::Dynamic(0);

                match lens_build {
                    Ok(used) => {
                        state.next = used;
                    }
                    Err(_) => {
                        stream.msg = Some("invalid literal/lengths set");
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                }

                // Build distance table
                let dist_start = state.next;
                let ndist = state.ndist as usize;
                let mut distbits_temp = 6u32;
                let dist_build = inflate_table(
                    CodeType::Dists,
                    &state.lens[nlen..nlen + ndist],
                    ndist,
                    &mut state.codes,
                    dist_start,
                    &mut distbits_temp,
                    &mut state.work,
                );
                state.distbits = distbits_temp;
                state.distcode = CodeTableRef::Dynamic(dist_start);

                match dist_build {
                    Ok(_used) => {}
                    Err(_) => {
                        stream.msg = Some("invalid distances set");
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                }

                // Tables built successfully — proceed to decoding
                state.mode = InflateMode::Len;
                continue;
            }

            // ----------------------------------------------------------------
            // LEN: Decode length/literal/distance codes
            // C infback.c lines 434–543
            //
            // This implements the complete slow-path decode. The C code
            // optionally calls inflate_fast() when have >= 6 && left >= 258
            // for performance, but for inflate_back the output buffer IS the
            // sliding window, creating an aliasing situation that requires
            // careful handling. The slow path produces identical results.
            // ----------------------------------------------------------------
            InflateMode::Len => {
                // Decode a literal, length, or end-of-block code
                let here;

                // First-level table lookup
                loop {
                    let idx = bits_val(hold, state.lenbits);
                    let entry = state.len_code(idx);
                    if u32::from(entry.bits) <= bits {
                        here = entry;
                        break;
                    }
                    // PULLBYTE
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }
                    hold |= u64::from(input_data[input_pos]) << bits;
                    input_pos += 1;
                    have -= 1;
                    bits += 8;
                }

                // Check for second-level table lookup
                let here = if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last = here;
                    let second;
                    loop {
                        let combined_bits = u32::from(last.bits) + u32::from(last.op);
                        let extra = bits_val(hold, combined_bits) >> u32::from(last.bits);
                        let idx = u32::from(last.val) + extra;
                        let entry = state.len_code(idx);
                        if u32::from(last.bits) + u32::from(entry.bits) <= bits {
                            second = entry;
                            break;
                        }
                        // PULLBYTE
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }
                    hold >>= u32::from(last.bits);
                    bits -= u32::from(last.bits);
                    second
                } else {
                    here
                };

                hold >>= u32::from(here.bits);
                bits -= u32::from(here.bits);
                state.length = u32::from(here.val);

                // Process literal byte (op == 0)
                if here.op == 0 {
                    // ROOM: flush window if full
                    if left == 0 {
                        state.whave = state.wsize;
                        if !output_cb(&state.window[..wsize]) {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                        put = 0;
                        left = wsize;
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        state.window[put] = state.length as u8;
                    }
                    put += 1;
                    left -= 1;
                    state.mode = InflateMode::Len;
                    continue;
                }

                // Process end-of-block (op & 32)
                if (here.op & 32) != 0 {
                    state.mode = InflateMode::Type;
                    continue;
                }

                // Invalid code (op & 64)
                if (here.op & 64) != 0 {
                    stream.msg = Some("invalid literal/length code");
                    state.mode = InflateMode::Bad;
                    continue;
                }

                // Length code — get extra bits for length
                state.extra = u32::from(here.op) & 15;
                if state.extra != 0 {
                    // NEEDBITS(state.extra)
                    while bits < state.extra {
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }
                    state.length += bits_val(hold, state.extra);
                    hold >>= state.extra;
                    bits -= state.extra;
                }

                // Get distance code
                let dist_here;
                loop {
                    let idx = bits_val(hold, state.distbits);
                    let entry = state.dist_code(idx);
                    if u32::from(entry.bits) <= bits {
                        dist_here = entry;
                        break;
                    }
                    // PULLBYTE
                    if have == 0 {
                        input_data = input_cb();
                        have = input_data.len();
                        input_pos = 0;
                        if have == 0 {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                    }
                    hold |= u64::from(input_data[input_pos]) << bits;
                    input_pos += 1;
                    have -= 1;
                    bits += 8;
                }

                // Second-level distance table lookup
                let dist_here = if (dist_here.op & 0xf0) == 0 {
                    let last = dist_here;
                    let second;
                    loop {
                        let combined_bits = u32::from(last.bits) + u32::from(last.op);
                        let extra = bits_val(hold, combined_bits) >> u32::from(last.bits);
                        let idx = u32::from(last.val) + extra;
                        let entry = state.dist_code(idx);
                        if u32::from(last.bits) + u32::from(entry.bits) <= bits {
                            second = entry;
                            break;
                        }
                        // PULLBYTE
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }
                    hold >>= u32::from(last.bits);
                    bits -= u32::from(last.bits);
                    second
                } else {
                    dist_here
                };

                hold >>= u32::from(dist_here.bits);
                bits -= u32::from(dist_here.bits);

                // Invalid distance code
                if (dist_here.op & 64) != 0 {
                    stream.msg = Some("invalid distance code");
                    state.mode = InflateMode::Bad;
                    continue;
                }

                state.offset = u32::from(dist_here.val);

                // Get distance extra bits
                state.extra = u32::from(dist_here.op) & 15;
                if state.extra != 0 {
                    // NEEDBITS(state.extra)
                    while bits < state.extra {
                        if have == 0 {
                            input_data = input_cb();
                            have = input_data.len();
                            input_pos = 0;
                            if have == 0 {
                                ret = ReturnCode::BufError;
                                break 'main_loop;
                            }
                        }
                        hold |= u64::from(input_data[input_pos]) << bits;
                        input_pos += 1;
                        have -= 1;
                        bits += 8;
                    }
                    state.offset += bits_val(hold, state.extra);
                    hold >>= state.extra;
                    bits -= state.extra;
                }

                // Validate distance against available window data
                // whave < wsize means window not yet fully populated
                let correction = if state.whave < state.wsize {
                    left
                } else {
                    0
                };
                let max_dist = wsize - correction;
                if state.offset as usize > max_dist {
                    stream.msg = Some("invalid distance too far back");
                    state.mode = InflateMode::Bad;
                    continue;
                }

                // Copy match from window to output
                // The window IS the output buffer, so backward references
                // read from earlier positions in the same buffer.
                let offset = state.offset as usize;
                while state.length > 0 {
                    // ROOM: flush window if full
                    if left == 0 {
                        state.whave = state.wsize;
                        if !output_cb(&state.window[..wsize]) {
                            ret = ReturnCode::BufError;
                            break 'main_loop;
                        }
                        put = 0;
                        left = wsize;
                    }

                    // Calculate source position for the copy
                    // copy = distance from end of window to match source
                    let copy_dist = wsize - offset;
                    let (mut from, mut copy);
                    if copy_dist < left {
                        // Source wraps around in the window (data from
                        // previous fill, positioned after current put)
                        from = put + copy_dist;
                        copy = left - copy_dist;
                    } else {
                        // Source is before current position
                        from = put - offset;
                        copy = left;
                    }

                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                    state.length -= copy as u32;
                    left -= copy;

                    // Byte-by-byte copy (may overlap for run-length encoding)
                    for _ in 0..copy {
                        state.window[put] = state.window[from];
                        put += 1;
                        from += 1;
                    }
                }

                state.mode = InflateMode::Len;
            }

            // ----------------------------------------------------------------
            // DONE: Decompression completed successfully
            // C infback.c lines 537–539
            // ----------------------------------------------------------------
            InflateMode::Done => {
                ret = ReturnCode::StreamEnd;
                break 'main_loop;
            }

            // ----------------------------------------------------------------
            // BAD: Error detected in stream
            // C infback.c lines 540–544
            // ----------------------------------------------------------------
            InflateMode::Bad => {
                ret = ReturnCode::DataError;
                break 'main_loop;
            }

            // Any other mode is an internal error
            _ => {
                ret = ReturnCode::StreamError;
                break 'main_loop;
            }
        }
    }

    // ====================================================================
    // inf_leave: flush remaining output and restore state
    // C infback.c lines 551–569
    // ====================================================================

    // If there's unflushed output data in the window, flush it now
    if left < wsize {
        let written = wsize - left;
        if !output_cb(&state.window[..written]) && ret == ReturnCode::StreamEnd {
            // Output callback failed on final flush — convert success to error
            ret = ReturnCode::BufError;
        }
    }

    // Save state back (mirrors C RESTORE macro)
    state.hold = hold;
    state.bits = bits;

    ret
}

/// Clean up after callback-based decompression.
///
/// Releases any resources held by the inflate state that were allocated
/// during [`inflate_back_init`]. In Rust, most cleanup is handled
/// automatically by `Drop` for owned types (`Vec<u8>`, etc.), but this
/// function provides an explicit cleanup point for FFI compatibility and
/// for callers who want deterministic resource release.
///
/// # Arguments
///
/// * `state` — Mutable reference to the inflate state to clean up.
///
/// # Returns
///
/// * [`ReturnCode::Ok`] on success.
///
/// # C Equivalent
///
/// `inflateBackEnd()` from `infback.c` lines 569–579.
pub fn inflate_back_end(state: &mut InflateState) -> ReturnCode {
    // Release the window buffer memory (shrink to zero capacity)
    state.window.clear();
    state.window.shrink_to_fit();

    // Release the codes table memory
    state.codes.clear();
    state.codes.shrink_to_fit();

    // Reset mode to indicate the state is no longer usable
    state.mode = InflateMode::Bad;

    ReturnCode::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `inflate_back_init` accepts valid window sizes.
    #[test]
    fn test_init_valid_window_bits() {
        let mut state = InflateState::default();
        for bits in 8..=15 {
            let result = inflate_back_init(&mut state, bits);
            assert_eq!(result, ReturnCode::Ok);
            assert_eq!(state.wbits, bits as u32);
            assert_eq!(state.wsize, 1u32 << bits);
            assert_eq!(state.window.len(), (1usize << bits));
            assert_eq!(state.whave, 0);
            assert_eq!(state.wnext, 0);
            assert!(state.sane);
        }
    }

    /// Verify that `inflate_back_init` rejects invalid window sizes.
    #[test]
    fn test_init_invalid_window_bits() {
        let mut state = InflateState::default();
        assert_eq!(inflate_back_init(&mut state, 7), ReturnCode::StreamError);
        assert_eq!(inflate_back_init(&mut state, 16), ReturnCode::StreamError);
        assert_eq!(inflate_back_init(&mut state, -1), ReturnCode::StreamError);
        assert_eq!(inflate_back_init(&mut state, 0), ReturnCode::StreamError);
    }

    /// Verify that `inflate_back_end` cleans up state.
    #[test]
    fn test_back_end_cleanup() {
        let mut state = InflateState::default();
        inflate_back_init(&mut state, 15);
        assert!(!state.window.is_empty());

        let result = inflate_back_end(&mut state);
        assert_eq!(result, ReturnCode::Ok);
        assert!(state.window.is_empty());
        assert!(state.codes.is_empty());
    }

    /// Verify that `inflate_back` returns `BufError` when input callback
    /// immediately returns empty data.
    #[test]
    fn test_back_empty_input() {
        let mut state = InflateState::default();
        inflate_back_init(&mut state, 15);

        let mut stream = ZStream::new();
        let mut input_cb = || Vec::new();
        let mut output_cb = |_data: &[u8]| -> bool { true };

        let result = inflate_back(&mut state, &mut stream, &mut input_cb, &mut output_cb);
        assert_eq!(result, ReturnCode::BufError);
    }

    /// Verify that `inflate_back` correctly decompresses a stored (uncompressed)
    /// DEFLATE block.
    #[test]
    fn test_back_stored_block() {
        let mut state = InflateState::default();
        inflate_back_init(&mut state, 15);

        // Construct a raw DEFLATE stream with a single stored block.
        // Block header: BFINAL=1, BTYPE=00 (stored) = 0b001 = 0x01
        // Then byte-aligned: LEN=0x0005, NLEN=0xFFFA, data = "hello"
        let mut deflate_data: Vec<u8> = Vec::new();
        deflate_data.push(0x01); // BFINAL=1, BTYPE=00
        // LEN = 5 (little-endian)
        deflate_data.push(0x05);
        deflate_data.push(0x00);
        // NLEN = ~5 = 0xFFFA (little-endian)
        deflate_data.push(0xFA);
        deflate_data.push(0xFF);
        // Data: "hello"
        deflate_data.extend_from_slice(b"hello");

        let mut input_called = false;
        let mut input_cb = move || -> Vec<u8> {
            if !input_called {
                input_called = true;
                deflate_data.clone()
            } else {
                Vec::new()
            }
        };

        let mut output_data = Vec::new();
        let mut output_cb = |data: &[u8]| -> bool {
            output_data.extend_from_slice(data);
            true
        };

        let mut stream = ZStream::new();
        let result = inflate_back(&mut state, &mut stream, &mut input_cb, &mut output_cb);
        assert_eq!(result, ReturnCode::StreamEnd);
        assert_eq!(&output_data, b"hello");
    }

    /// Verify the ORDER constant matches the specification.
    #[test]
    fn test_order_constant() {
        assert_eq!(
            ORDER,
            [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]
        );
    }

    /// Verify bits_val extraction helper.
    #[test]
    fn test_bits_val() {
        assert_eq!(bits_val(0xFF, 4), 0x0F);
        assert_eq!(bits_val(0xFF, 8), 0xFF);
        assert_eq!(bits_val(0x1234_5678, 16), 0x5678);
        assert_eq!(bits_val(0, 5), 0);
        assert_eq!(bits_val(0b1010_1010, 3), 0b010);
    }
}
