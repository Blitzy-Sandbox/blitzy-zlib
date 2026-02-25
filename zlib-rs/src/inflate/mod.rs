//! DEFLATE decompression engine — inflate module root.
//!
//! Complete Rust port of `inflate.c` (1413 lines) from zlib 1.3.2.1-motley.
//! This module declares all inflate submodules, exposes public API functions,
//! and contains the 30+ mode state machine for the inflate operation.
//!
//! # Architecture
//!
//! The inflate engine processes DEFLATE-compressed data (RFC 1951) optionally
//! wrapped in zlib (RFC 1950) or gzip (RFC 1952) headers/trailers. The core
//! state machine in [`inflate()`] drives decompression through 32 modes,
//! dispatching to [`inflate_fast()`](fast::inflate_fast) for performance-critical
//! inner loops and [`inflate_table()`](table::inflate_table) for Huffman table
//! construction.
//!
//! # Safety
//!
//! This module contains **zero** `unsafe` blocks. All buffer access uses safe
//! Rust slice operations with bounds checking.

// ─── Submodule declarations ──────────────────────────────────────────────────

/// Callback-based DEFLATE decompression (`inflateBack`).
pub mod back;
/// Performance-critical fast-path inflate decoder.
pub mod fast;
/// Pre-computed fixed Huffman tables for DEFLATE block type 1.
pub mod fixed;
/// Inflate decompression state definitions and mode enum.
pub mod state;
/// Huffman code table construction for the inflate decompressor.
pub mod table;

// ─── Re-exports ──────────────────────────────────────────────────────────────

pub use state::{CodeTableRef, InflateMode, InflateState};
pub use table::{Code, CodeType, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS};

// ─── Internal imports ────────────────────────────────────────────────────────

use crate::checksum::adler32::adler32;
use crate::checksum::crc32::crc32;
#[allow(unused_imports)]
use crate::constants::{
    DEF_WBITS, MAX_MATCH, MAX_WBITS, PRESET_DICT, Z_BINARY, Z_BLOCK, Z_BUF_ERROR,
    Z_DATA_ERROR, Z_DEFLATED, Z_FINISH, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_NULL, Z_OK,
    Z_STREAM_END, Z_STREAM_ERROR, Z_TEXT, Z_TREES, Z_UNKNOWN, Z_VERSION_ERROR,
};
use crate::error::ReturnCode;
use crate::stream::{GzHeader, ZStream};
use fast::inflate_fast;
use fixed::inflate_fixed;
use table::inflate_table;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Permutation of code lengths for dynamic block header decoding.
///
/// Specifies the order in which code length code lengths appear in the
/// DEFLATE dynamic block header (RFC 1951, section 3.2.7). Ported from
/// `inflate.c` lines 491–492.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ─── Helper functions (C macro equivalents) ──────────────────────────────────

/// Extract the low `n` bits from the bit accumulator `hold`.
///
/// Equivalent to the C macro `BITS(n)` in `inflate.c` line 375.
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn bits_val(hold: u64, n: u32) -> u32 {
    (hold as u32) & ((1u32 << n).wrapping_sub(1))
}

/// Byte-swap a 32-bit value (equivalent to C `ZSWAP32`).
///
/// Converts between big-endian and little-endian representations for
/// checksum comparison in the `Check` and `DictId` modes.
#[inline]
fn zswap32(val: u32) -> u32 {
    val.swap_bytes()
}

/// Compute the update checksum using `adler32` for zlib or `crc32` for gzip.
///
/// Equivalent to the C macro `UPDATE_CHECK` in `inflate.c` lines 301–306.
/// When `flags >= 0` (gzip mode), uses CRC-32; otherwise uses Adler-32.
#[inline]
fn update_check(check: u32, buf: &[u8], flags: i32) -> u32 {
    if flags != 0 {
        crc32(check, buf)
    } else {
        adler32(check, buf)
    }
}

/// Compute header CRC for a 2-byte value (equivalent to C `CRC2` macro).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn crc2(check: u32, word: u64) -> u32 {
    let hbuf = [word as u8, (word >> 8) as u8];
    crc32(check, &hbuf)
}

/// Compute header CRC for a 4-byte value (equivalent to C `CRC4` macro).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn crc4(check: u32, word: u64) -> u32 {
    let hbuf = [
        word as u8,
        (word >> 8) as u8,
        (word >> 16) as u8,
        (word >> 24) as u8,
    ];
    crc32(check, &hbuf)
}

// ─── inflate_state_check ─────────────────────────────────────────────────────

/// Validate that the inflate state is properly initialized.
///
/// Returns `true` if the state is valid, `false` otherwise.
/// Port of `inflateStateCheck()` from `inflate.c` lines 88–98.
#[inline]
fn inflate_state_check(state: &InflateState) -> bool {
    // In Rust, the state reference is always valid if we have it.
    // We only check the mode is in the valid range.
    let mode = state.mode as u32;
    mode >= InflateMode::Head as u32 && mode <= InflateMode::Sync as u32
}

// ─── inflate_reset_keep ──────────────────────────────────────────────────────

/// Reset inflate state keeping the window intact.
///
/// Resets the streaming counters and state machine while preserving the
/// sliding window buffer. This is the foundation for both [`inflate_reset`]
/// and [`inflate_reset2`].
///
/// Port of `inflateResetKeep()` from `inflate.c` lines 100–123.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state to reset.
/// * `stream` — Mutable reference to the associated stream.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if the state
/// is invalid.
#[allow(clippy::cast_sign_loss)]
pub fn inflate_reset_keep(state: &mut InflateState, stream: &mut ZStream) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    stream.total_in = 0;
    stream.total_out = 0;
    state.total = 0;
    stream.msg = None;
    stream.data_type = 0;
    if state.wrap != 0 {
        stream.adler = (state.wrap & 1) as u32;
    }
    state.mode = InflateMode::Head;
    state.last = false;
    state.havedict = false;
    state.flags = -1;
    state.dmax = 32_768;
    state.head = None;
    state.hold = 0;
    state.bits = 0;
    state.lencode = CodeTableRef::Dynamic(0);
    state.distcode = CodeTableRef::Dynamic(0);
    state.next = 0;
    state.sane = true;
    state.back = -1;
    ReturnCode::Ok
}

// ─── inflate_reset ───────────────────────────────────────────────────────────

/// Reset inflate state completely.
///
/// Resets the window tracking and then delegates to [`inflate_reset_keep`]
/// for the remaining state. The window buffer itself is preserved but its
/// tracking counters (wsize, whave, wnext) are zeroed.
///
/// Port of `inflateReset()` from `inflate.c` lines 125–134.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `stream` — Mutable reference to the associated stream.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] on error.
pub fn inflate_reset(state: &mut InflateState, stream: &mut ZStream) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    state.wsize = 0;
    state.whave = 0;
    state.wnext = 0;
    inflate_reset_keep(state, stream)
}

// ─── inflate_reset2 ──────────────────────────────────────────────────────────

/// Reset inflate state with new window bits parameter.
///
/// Handles the overloaded `windowBits` encoding:
/// - Negative → raw DEFLATE (no wrapper), `wrap = 0`
/// - `0..15` → zlib format, `wrap = 1`
/// - `16..31` → gzip only, `wrap = 2`
/// - `32..47` → auto-detect (zlib or gzip), `wrap = (windowBits >> 4) + 5`
///
/// Port of `inflateReset2()` from `inflate.c` lines 136–171.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `stream` — Mutable reference to the associated stream.
/// * `window_bits` — Window size parameter with format encoding.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] on invalid params.
#[allow(clippy::cast_sign_loss)]
pub fn inflate_reset2(
    state: &mut InflateState,
    stream: &mut ZStream,
    window_bits: i32,
) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }

    let wrap;
    let mut wb = window_bits;

    // Extract wrap request from windowBits parameter
    if wb < 0 {
        if wb < -15 {
            return ReturnCode::StreamError;
        }
        wrap = 0;
        wb = -wb;
    } else {
        wrap = (wb >> 4) + 5;
        // GUNZIP is always enabled in this Rust port
        if wb < 48 {
            wb &= 15;
        }
    }

    // Set number of window bits, free window if different
    if wb != 0 && !(8..=15).contains(&wb) {
        return ReturnCode::StreamError;
    }
    #[allow(clippy::cast_sign_loss)]
    let wb_u32 = wb as u32;
    if !state.window.is_empty() && state.wbits != wb_u32 {
        state.window = Vec::new();
    }

    // Update state and reset the rest
    state.wrap = wrap;
    state.wbits = wb_u32;
    inflate_reset(state, stream)
}

// ─── inflate_init2 ───────────────────────────────────────────────────────────

/// Initialize inflate state with specified window bits.
///
/// Allocates and configures a new [`InflateState`] for decompression.
/// The `window_bits` parameter controls both the window size and the
/// expected stream format (see [`inflate_reset2`] for encoding details).
///
/// Port of `inflateInit2_()` from `inflate.c` lines 173–212.
///
/// # Parameters
///
/// * `stream` — Mutable reference to the stream to initialize.
/// * `window_bits` — Window size parameter with format encoding.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, or an error code.
pub fn inflate_init2(stream: &mut ZStream, window_bits: i32) -> ReturnCode {
    stream.msg = None;

    let mut state = InflateState::new();
    state.mode = InflateMode::Head; // to pass state test in inflate_reset2()

    let ret = inflate_reset2(&mut state, stream, window_bits);
    if ret != ReturnCode::Ok {
        return ret;
    }

    // Store state inside the stream
    stream.state = Some(Box::new(state));
    ReturnCode::Ok
}

// ─── inflate_init ────────────────────────────────────────────────────────────

/// Initialize inflate state with default window bits.
///
/// Calls [`inflate_init2`] with [`DEF_WBITS`] (15) for standard zlib
/// format decompression.
///
/// Port of `inflateInit_()` from `inflate.c` lines 214–217.
///
/// # Parameters
///
/// * `stream` — Mutable reference to the stream to initialize.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, or an error code.
pub fn inflate_init(stream: &mut ZStream) -> ReturnCode {
    inflate_init2(stream, DEF_WBITS)
}

// ─── inflate_prime ───────────────────────────────────────────────────────────

/// Insert bits into the inflate input stream.
///
/// Allows injection of bits prior to the next `inflate()` call. This is useful
/// for random access applications or for providing bits that were not on a byte
/// boundary.
///
/// Port of `inflatePrime()` from `inflate.c` lines 219–236.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `bits` — Number of bits to insert (0 to clear, negative to reset).
/// * `value` — The bit values to insert (low `bits` bits are used).
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] on invalid params.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn inflate_prime(state: &mut InflateState, bits: i32, value: i32) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    if bits == 0 {
        return ReturnCode::Ok;
    }
    if bits < 0 {
        state.hold = 0;
        state.bits = 0;
        return ReturnCode::Ok;
    }
    if bits > 16 || state.bits + bits as u32 > 32 {
        return ReturnCode::StreamError;
    }
    let masked = (value as u64) & ((1u64 << bits) - 1);
    state.hold += masked << state.bits;
    state.bits += bits as u32;
    ReturnCode::Ok
}

// ─── update_window ───────────────────────────────────────────────────────────

/// Update the sliding window with recently written output data.
///
/// If the window does not exist yet, creates it. Copies the last `wsize`
/// bytes (or fewer) of the output into the circular sliding window buffer.
///
/// Port of `updatewindow()` from `inflate.c` lines 252–296.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `end` — Slice of recently written output data.
///
/// # Returns
///
/// `true` on success, `false` on memory allocation failure.
#[allow(clippy::cast_possible_truncation)]
fn update_window(state: &mut InflateState, end: &[u8]) -> bool {
    // Allocate window if needed
    if state.window.is_empty() && !state.ensure_window() {
        return false;
    }

    // Initialize window size on first use
    if state.wsize == 0 {
        state.wsize = 1u32.wrapping_shl(state.wbits);
        state.wnext = 0;
        state.whave = 0;
    }

    let copy = end.len();
    let wsize = state.wsize as usize;

    if copy >= wsize {
        // Copy last wsize bytes into window
        let start = copy - wsize;
        state.window[..wsize].copy_from_slice(&end[start..]);
        state.wnext = 0;
        state.whave = state.wsize;
    } else {
        // Partial copy with wrap-around
        let wnext = state.wnext as usize;
        let dist = (wsize - wnext).min(copy);
        state.window[wnext..wnext + dist].copy_from_slice(&end[..dist]);
        let remaining = copy - dist;
        if remaining > 0 {
            state.window[..remaining].copy_from_slice(&end[dist..]);
            state.wnext = remaining as u32;
            state.whave = state.wsize;
        } else {
            state.wnext += dist as u32;
            if state.wnext == state.wsize {
                state.wnext = 0;
            }
            if state.whave < state.wsize {
                state.whave += dist as u32;
            }
        }
    }
    true
}

// ─── inflate — Main decompression state machine ──────────────────────────────

/// Decompress data using the inflate algorithm.
///
/// This is the main decompression function implementing a 30+ mode state
/// machine. It reads compressed data from the stream's input buffer and
/// writes decompressed data to the stream's output buffer.
///
/// Port of `inflate()` from `inflate.c` lines 474–1153.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `stream` — Mutable reference to the I/O stream.
/// * `flush` — Flush mode (one of `Z_NO_FLUSH`, `Z_SYNC_FLUSH`, `Z_FINISH`,
///   `Z_BLOCK`, `Z_TREES`).
///
/// # Returns
///
/// A [`ReturnCode`] indicating the result:
/// - [`Ok`](ReturnCode::Ok) — progress was made
/// - [`StreamEnd`](ReturnCode::StreamEnd) — end of compressed stream reached
/// - [`NeedDict`](ReturnCode::NeedDict) — a preset dictionary is required
/// - [`DataError`](ReturnCode::DataError) — compressed data is corrupt
/// - [`MemError`](ReturnCode::MemError) — insufficient memory
/// - [`BufError`](ReturnCode::BufError) — no progress possible
/// - [`StreamError`](ReturnCode::StreamError) — invalid state or parameters
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::needless_late_init,
    clippy::identity_op
)]
pub fn inflate(state: &mut InflateState, stream: &mut ZStream, flush: i32) -> ReturnCode {
    // Validate state and stream
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }

    // Build local copies of input/output slices for efficient processing.
    // We work on the raw vectors of the stream by taking mutable references.
    let input = stream.input_remaining().to_vec();
    let in_len = input.len();

    // We need to know how much output space is available
    let out_capacity = stream.avail_out();
    // Pre-allocate a local output buffer
    let mut output_buf: Vec<u8> = vec![0u8; out_capacity];

    if out_capacity == 0 {
        return ReturnCode::StreamError;
    }

    // Local variables mirroring C's LOAD() macro
    let mut next: usize = 0; // index into input
    let mut have: usize = in_len; // bytes available in input
    let mut put: usize = 0; // index into output_buf
    let mut left: usize = out_capacity; // bytes available in output
    let mut hold: u64 = state.hold;
    let mut bits: u32 = state.bits;

    let in_start = have; // save starting available input
    let out_start = left; // save starting available output

    if state.mode == InflateMode::Type {
        state.mode = InflateMode::TypeDo; // skip check
    }

    let mut ret = ReturnCode::Ok;

    // Main state machine loop
    'inf_loop: loop {
        match state.mode {
            InflateMode::Head => {
                if state.wrap == 0 {
                    state.mode = InflateMode::TypeDo;
                    continue 'inf_loop;
                }
                // NEEDBITS(16)
                while bits < 16 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                // Check for gzip header (magic 0x1f 0x8b)
                if (state.wrap & 2) != 0 && hold == 0x8b1f {
                    if state.wbits == 0 {
                        state.wbits = 15;
                    }
                    state.check = crc32(0, &[]) as u64;
                    state.check = crc2(state.check as u32, hold) as u64;
                    hold = 0;
                    bits = 0;
                    state.mode = InflateMode::Flags;
                    continue 'inf_loop;
                }
                if let Some(ref mut head) = state.head {
                    head.done = false; // -1 in C, but we use bool
                }
                if (state.wrap & 1) == 0
                    || ((bits_val(hold, 8) << 8) + (hold >> 8) as u32) % 31 != 0
                {
                    stream.msg = Some("incorrect header check");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                if bits_val(hold, 4) != Z_DEFLATED as u32 {
                    stream.msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                hold >>= 4;
                bits -= 4;
                let len = bits_val(hold, 4) + 8;
                if state.wbits == 0 {
                    state.wbits = len;
                }
                if len > 15 || len > state.wbits {
                    stream.msg = Some("invalid window size");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                state.dmax = 1u32 << len;
                state.flags = 0; // indicate zlib header
                stream.adler = adler32(0, &[]);
                state.check = stream.adler as u64;
                state.mode = if hold & 0x200 != 0 {
                    InflateMode::DictId
                } else {
                    InflateMode::Type
                };
                hold = 0;
                bits = 0;
                continue 'inf_loop;
            }

            InflateMode::Flags => {
                // NEEDBITS(16)
                while bits < 16 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                state.flags = hold as i32;
                if (state.flags & 0xff) != Z_DEFLATED {
                    stream.msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                if state.flags & 0xe000 != 0 {
                    stream.msg = Some("unknown header flags set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                if let Some(ref mut head) = state.head {
                    head.text = ((hold >> 8) & 1) != 0;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc2(state.check as u32, hold) as u64;
                }
                hold = 0;
                bits = 0;
                state.mode = InflateMode::Time;
                // fallthrough
            }

            InflateMode::Time => {
                // NEEDBITS(32)
                while bits < 32 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                if let Some(ref mut head) = state.head {
                    head.time = hold as u32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc4(state.check as u32, hold) as u64;
                }
                hold = 0;
                bits = 0;
                state.mode = InflateMode::Os;
                // fallthrough
            }

            InflateMode::Os => {
                // NEEDBITS(16)
                while bits < 16 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                if let Some(ref mut head) = state.head {
                    head.xflags = (hold & 0xff) as i32;
                    head.os = (hold >> 8) as i32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc2(state.check as u32, hold) as u64;
                }
                hold = 0;
                bits = 0;
                state.mode = InflateMode::ExLen;
                // fallthrough
            }

            InflateMode::ExLen => {
                if state.flags & 0x0400 != 0 {
                    // NEEDBITS(16)
                    while bits < 16 {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    state.length = hold as u32;
                    if let Some(ref mut head) = state.head {
                        // store extra_len in the extra field capacity
                        // We'll collect extra bytes in the Extra mode
                        if head.extra.is_none() {
                            head.extra = Some(Vec::new());
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc2(state.check as u32, hold) as u64;
                    }
                    hold = 0;
                    bits = 0;
                } else if let Some(ref mut head) = state.head {
                    head.extra = None;
                }
                state.mode = InflateMode::Extra;
                // fallthrough
            }

            InflateMode::Extra => {
                if state.flags & 0x0400 != 0 {
                    let mut copy = state.length as usize;
                    if copy > have {
                        copy = have;
                    }
                    if copy > 0 {
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut extra) = head.extra {
                                extra.extend_from_slice(&input[next..next + copy]);
                            }
                        }
                        if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                            state.check =
                                crc32(state.check as u32, &input[next..next + copy]) as u64;
                        }
                        have -= copy;
                        next += copy;
                        state.length -= copy as u32;
                    }
                    if state.length != 0 {
                        break 'inf_loop;
                    }
                }
                state.length = 0;
                state.mode = InflateMode::Name;
                // fallthrough
            }

            InflateMode::Name => {
                if state.flags & 0x0800 != 0 {
                    if have == 0 {
                        break 'inf_loop;
                    }
                    let mut copy = 0usize;
                    loop {
                        let byte = input[next + copy];
                        copy += 1;
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut name) = head.name {
                                name.push(byte);
                            } else {
                                head.name = Some(vec![byte]);
                            }
                        }
                        if byte == 0 || copy >= have {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check =
                            crc32(state.check as u32, &input[next..next + copy]) as u64;
                    }
                    have -= copy;
                    next += copy;
                    let last_byte = if copy > 0 { input[next - 1] } else { 0 };
                    // Check if we haven't found the null terminator yet
                    if last_byte != 0 {
                        break 'inf_loop;
                    }
                } else if let Some(ref mut head) = state.head {
                    head.name = None;
                }
                state.length = 0;
                state.mode = InflateMode::Comment;
                // fallthrough
            }

            InflateMode::Comment => {
                if state.flags & 0x1000 != 0 {
                    if have == 0 {
                        break 'inf_loop;
                    }
                    let mut copy = 0usize;
                    loop {
                        let byte = input[next + copy];
                        copy += 1;
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut comment) = head.comment {
                                comment.push(byte);
                            } else {
                                head.comment = Some(vec![byte]);
                            }
                        }
                        if byte == 0 || copy >= have {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check =
                            crc32(state.check as u32, &input[next..next + copy]) as u64;
                    }
                    have -= copy;
                    next += copy;
                    let last_byte = if copy > 0 { input[next - 1] } else { 0 };
                    if last_byte != 0 {
                        break 'inf_loop;
                    }
                } else if let Some(ref mut head) = state.head {
                    head.comment = None;
                }
                state.mode = InflateMode::Hcrc;
                // fallthrough
            }

            InflateMode::Hcrc => {
                if state.flags & 0x0200 != 0 {
                    // NEEDBITS(16)
                    while bits < 16 {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    if (state.wrap & 4) != 0
                        && (hold as u32) != (state.check as u32 & 0xffff)
                    {
                        stream.msg = Some("header crc mismatch");
                        state.mode = InflateMode::Bad;
                        continue 'inf_loop;
                    }
                    hold = 0;
                    bits = 0;
                }
                if let Some(ref mut head) = state.head {
                    head.hcrc = (state.flags >> 9) & 1 != 0;
                    head.done = true;
                }
                stream.adler = crc32(0, &[]);
                state.check = stream.adler as u64;
                state.mode = InflateMode::Type;
                continue 'inf_loop;
            }

            InflateMode::DictId => {
                // NEEDBITS(32)
                while bits < 32 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                stream.adler = zswap32(hold as u32);
                state.check = stream.adler as u64;
                hold = 0;
                bits = 0;
                state.mode = InflateMode::Dict;
                // fallthrough
            }

            InflateMode::Dict => {
                if !state.havedict {
                    // RESTORE and return Z_NEED_DICT
                    state.hold = hold;
                    state.bits = bits;
                    // Write output back to stream
                    let written = put;
                    if written > 0 {
                        let out_space = stream.output_remaining_mut();
                        out_space[..written].copy_from_slice(&output_buf[..written]);
                        let _ = stream.advance_output(written);
                    }
                    let consumed = next;
                    if consumed > 0 {
                        let _ = stream.advance_input(consumed);
                    }
                    return ReturnCode::NeedDict;
                }
                stream.adler = adler32(0, &[]);
                state.check = stream.adler as u64;
                state.mode = InflateMode::Type;
                // fallthrough
            }

            InflateMode::Type => {
                if flush == Z_BLOCK || flush == Z_TREES {
                    break 'inf_loop;
                }
                state.mode = InflateMode::TypeDo;
                // fallthrough
            }

            InflateMode::TypeDo => {
                if state.last {
                    // BYTEBITS
                    hold >>= bits & 7;
                    bits -= bits & 7;
                    state.mode = InflateMode::Check;
                    continue 'inf_loop;
                }
                // NEEDBITS(3)
                while bits < 3 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                state.last = bits_val(hold, 1) != 0;
                hold >>= 1;
                bits -= 1;
                match bits_val(hold, 2) {
                    0 => {
                        // stored block
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        // fixed block
                        inflate_fixed(state);
                        state.mode = InflateMode::Len_;
                        if flush == Z_TREES {
                            hold >>= 2;
                            bits -= 2;
                            break 'inf_loop;
                        }
                    }
                    2 => {
                        // dynamic block
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        stream.msg = Some("invalid block type");
                        state.mode = InflateMode::Bad;
                    }
                }
                hold >>= 2;
                bits -= 2;
                continue 'inf_loop;
            }

            InflateMode::Stored => {
                // BYTEBITS — go to byte boundary
                hold >>= bits & 7;
                bits -= bits & 7;
                // NEEDBITS(32)
                while bits < 32 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    stream.msg = Some("invalid stored block lengths");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                state.length = (hold as u32) & 0xffff;
                hold = 0;
                bits = 0;
                state.mode = InflateMode::Copy_;
                if flush == Z_TREES {
                    break 'inf_loop;
                }
                // fallthrough to Copy_
            }

            InflateMode::Copy_ => {
                state.mode = InflateMode::Copy;
                // fallthrough
            }

            InflateMode::Copy => {
                let mut copy = state.length as usize;
                if copy > 0 {
                    if copy > have { copy = have; }
                    if copy > left { copy = left; }
                    if copy == 0 { break 'inf_loop; }
                    output_buf[put..put + copy].copy_from_slice(&input[next..next + copy]);
                    have -= copy;
                    next += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                    continue 'inf_loop;
                }
                state.mode = InflateMode::Type;
                continue 'inf_loop;
            }

            InflateMode::Table => {
                // NEEDBITS(14)
                while bits < 14 {
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
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
                    continue 'inf_loop;
                }
                state.have = 0;
                state.mode = InflateMode::LenLens;
                // fallthrough
            }

            InflateMode::LenLens => {
                while state.have < state.ncode {
                    // NEEDBITS(3)
                    while bits < 3 {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    state.lens[ORDER[state.have as usize] as usize] = bits_val(hold, 3) as u16;
                    state.have += 1;
                    hold >>= 3;
                    bits -= 3;
                }
                while state.have < 19 {
                    state.lens[ORDER[state.have as usize] as usize] = 0;
                    state.have += 1;
                }
                state.next = 0;
                state.lencode = CodeTableRef::Dynamic(0);
                state.distcode = CodeTableRef::Dynamic(0);
                let mut lenbits_val = 7u32;
                let result = inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    0,
                    &mut lenbits_val,
                    &mut state.work,
                );
                state.lenbits = lenbits_val;
                if result.is_err() {
                    stream.msg = Some("invalid code lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                state.have = 0;
                state.mode = InflateMode::CodeLens;
                // fallthrough
            }

            InflateMode::CodeLens => {
                'codelens: while state.have < state.nlen + state.ndist {
                    // Decode one code length
                    loop {
                        let here = state.len_code(
                            bits_val(hold, state.lenbits),
                        );
                        if u32::from(here.bits) <= bits {
                            // Got enough bits for this code
                            if here.val < 16 {
                                hold >>= u32::from(here.bits);
                                bits -= u32::from(here.bits);
                                state.lens[state.have as usize] = here.val;
                                state.have += 1;
                            } else {
                                let (len_val, copy_count);
                                if here.val == 16 {
                                    let need = u32::from(here.bits) + 2;
                                    // NEEDBITS(here.bits + 2)
                                    while bits < need {
                                        if have == 0 { break 'inf_loop; }
                                        have -= 1;
                                        hold += u64::from(input[next]) << bits;
                                        next += 1;
                                        bits += 8;
                                    }
                                    hold >>= u32::from(here.bits);
                                    bits -= u32::from(here.bits);
                                    if state.have == 0 {
                                        stream.msg = Some("invalid bit length repeat");
                                        state.mode = InflateMode::Bad;
                                        break 'codelens;
                                    }
                                    len_val = state.lens[(state.have - 1) as usize];
                                    copy_count = 3 + bits_val(hold, 2);
                                    hold >>= 2;
                                    bits -= 2;
                                } else if here.val == 17 {
                                    let need = u32::from(here.bits) + 3;
                                    while bits < need {
                                        if have == 0 { break 'inf_loop; }
                                        have -= 1;
                                        hold += u64::from(input[next]) << bits;
                                        next += 1;
                                        bits += 8;
                                    }
                                    hold >>= u32::from(here.bits);
                                    bits -= u32::from(here.bits);
                                    len_val = 0;
                                    copy_count = 3 + bits_val(hold, 3);
                                    hold >>= 3;
                                    bits -= 3;
                                } else {
                                    // here.val == 18
                                    let need = u32::from(here.bits) + 7;
                                    while bits < need {
                                        if have == 0 { break 'inf_loop; }
                                        have -= 1;
                                        hold += u64::from(input[next]) << bits;
                                        next += 1;
                                        bits += 8;
                                    }
                                    hold >>= u32::from(here.bits);
                                    bits -= u32::from(here.bits);
                                    len_val = 0;
                                    copy_count = 11 + bits_val(hold, 7);
                                    hold >>= 7;
                                    bits -= 7;
                                }
                                if state.have + copy_count > state.nlen + state.ndist {
                                    stream.msg = Some("invalid bit length repeat");
                                    state.mode = InflateMode::Bad;
                                    break 'codelens;
                                }
                                for _ in 0..copy_count {
                                    state.lens[state.have as usize] = len_val;
                                    state.have += 1;
                                }
                            }
                            break; // continue to next code length in 'codelens
                        }
                        // PULLBYTE
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                }

                // Handle error breaks in while
                if state.mode == InflateMode::Bad {
                    continue 'inf_loop;
                }

                // Check for end-of-block code (better have one)
                if state.lens[256] == 0 {
                    stream.msg = Some("invalid code -- missing end-of-block");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }

                // Build length/literal table
                state.next = 0;
                state.lencode = CodeTableRef::Dynamic(0);
                let mut lenbits_val = 9u32;
                let result = inflate_table(
                    CodeType::Lens,
                    &state.lens,
                    state.nlen as usize,
                    &mut state.codes,
                    0,
                    &mut lenbits_val,
                    &mut state.work,
                );
                state.lenbits = lenbits_val;
                if result.is_err() {
                    stream.msg = Some("invalid literal/lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                let len_table_used = result.unwrap_or_default();

                // Build distance table
                state.distcode = CodeTableRef::Dynamic(len_table_used);
                let mut distbits_val = 6u32;
                let dist_lens_start = state.nlen as usize;
                let dist_codes = state.ndist as usize;
                // Need to pass the lens slice starting from nlen
                // Build a temporary slice for distance lens
                let dist_result = {
                    let lens_copy: Vec<u16> = state.lens[dist_lens_start..dist_lens_start + dist_codes].to_vec();
                    inflate_table(
                        CodeType::Dists,
                        &lens_copy,
                        dist_codes,
                        &mut state.codes,
                        len_table_used,
                        &mut distbits_val,
                        &mut state.work,
                    )
                };
                state.distbits = distbits_val;
                if dist_result.is_err() {
                    stream.msg = Some("invalid distances set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_loop;
                }
                state.next = len_table_used + dist_result.unwrap_or_default();
                state.mode = InflateMode::Len_;
                if flush == Z_TREES {
                    break 'inf_loop;
                }
                // fallthrough
            }

            InflateMode::Len_ => {
                state.mode = InflateMode::Len;
                // fallthrough
            }

            InflateMode::Len => {
                // Fast path: call inflate_fast when enough I/O available
                if have >= 6 && left >= 258 {
                    // RESTORE — save local state back
                    state.hold = hold;
                    state.bits = bits;

                    // Write output so far to stream
                    let written = put;
                    if written > 0 {
                        let out_space = stream.output_remaining_mut();
                        let copy_len = written.min(out_space.len());
                        out_space[..copy_len].copy_from_slice(&output_buf[..copy_len]);
                        let _ = stream.advance_output(copy_len);
                    }
                    let consumed = next;
                    if consumed > 0 {
                        let _ = stream.advance_input(consumed);
                    }

                    // Rebuild input/output for inflate_fast
                    let fast_input = stream.input_remaining().to_vec();
                    let fast_avail_out = stream.avail_out();
                    let mut fast_output: Vec<u8> = vec![0u8; fast_avail_out];
                    // Copy existing output context for window distance refs
                    let mut in_pos: usize = 0;
                    let mut out_pos: usize = 0;
                    let in_end = if fast_input.len() >= 6 { fast_input.len() - 5 } else { 0 };
                    let out_end = if fast_avail_out >= 258 { fast_avail_out - 257 } else { 0 };

                    let err = inflate_fast(
                        state,
                        &fast_input,
                        &mut fast_output,
                        &mut in_pos,
                        &mut out_pos,
                        in_end,
                        out_end,
                        0, // output start for distance calculations
                    );
                    if let Some(msg) = err {
                        stream.msg = Some(msg);
                    }

                    // Write fast output to stream
                    if out_pos > 0 {
                        let out_space = stream.output_remaining_mut();
                        let copy_len = out_pos.min(out_space.len());
                        out_space[..copy_len].copy_from_slice(&fast_output[..copy_len]);
                        let _ = stream.advance_output(copy_len);
                    }
                    if in_pos > 0 {
                        let _ = stream.advance_input(in_pos);
                    }

                    // Reload hold/bits from state (fast path modifies them)
                    hold = state.hold;
                    bits = state.bits;

                    // After the fast path, our local buffers are out of sync
                    // with the stream. The simplest correct approach is to
                    // break and let inf_leave handle the bookkeeping.
                    if state.mode == InflateMode::Type {
                        state.back = -1;
                    }
                    break 'inf_loop;
                }

                // Slow path
                state.back = 0;
                // Decode one length/literal code
                loop {
                    let here = state.len_code(bits_val(hold, state.lenbits));
                    if u32::from(here.bits) <= bits {
                        break;
                    }
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                let here = state.len_code(bits_val(hold, state.lenbits));
                if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last_code = here;
                    loop {
                        let idx = u32::from(last_code.val)
                            + (bits_val(hold, u32::from(last_code.bits) + u32::from(last_code.op))
                                >> u32::from(last_code.bits));
                        let here2 = state.len_code(idx);
                        if u32::from(last_code.bits) + u32::from(here2.bits) <= bits {
                            hold >>= u32::from(last_code.bits);
                            bits -= u32::from(last_code.bits);
                            state.back += i32::from(last_code.bits);
                            // Continue with here2
                            hold >>= u32::from(here2.bits);
                            bits -= u32::from(here2.bits);
                            state.back += i32::from(here2.bits);
                            state.length = u32::from(here2.val);
                            if here2.op == 0 {
                                state.mode = InflateMode::Lit;
                                continue 'inf_loop;
                            }
                            if here2.op & 32 != 0 {
                                state.back = -1;
                                state.mode = InflateMode::Type;
                                continue 'inf_loop;
                            }
                            if here2.op & 64 != 0 {
                                stream.msg = Some("invalid literal/length code");
                                state.mode = InflateMode::Bad;
                                continue 'inf_loop;
                            }
                            state.extra = u32::from(here2.op) & 15;
                            state.mode = InflateMode::LenExt;
                            continue 'inf_loop;
                        }
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                } else {
                    hold >>= u32::from(here.bits);
                    bits -= u32::from(here.bits);
                    state.back += i32::from(here.bits);
                    state.length = u32::from(here.val);
                    if here.op == 0 {
                        state.mode = InflateMode::Lit;
                        continue 'inf_loop;
                    }
                    if here.op & 32 != 0 {
                        state.back = -1;
                        state.mode = InflateMode::Type;
                        continue 'inf_loop;
                    }
                    if here.op & 64 != 0 {
                        stream.msg = Some("invalid literal/length code");
                        state.mode = InflateMode::Bad;
                        continue 'inf_loop;
                    }
                    state.extra = u32::from(here.op) & 15;
                    state.mode = InflateMode::LenExt;
                    // fallthrough
                }
            }

            InflateMode::LenExt => {
                if state.extra != 0 {
                    // NEEDBITS(state.extra)
                    while bits < state.extra {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    state.length += bits_val(hold, state.extra);
                    hold >>= state.extra;
                    bits -= state.extra;
                    state.back += state.extra as i32;
                }
                state.was = state.length;
                state.mode = InflateMode::Dist;
                // fallthrough
            }

            InflateMode::Dist => {
                loop {
                    let here = state.dist_code(bits_val(hold, state.distbits));
                    if u32::from(here.bits) <= bits {
                        if (here.op & 0xf0) == 0 {
                            let last_code = here;
                            loop {
                                let idx = u32::from(last_code.val)
                                    + (bits_val(
                                        hold,
                                        u32::from(last_code.bits) + u32::from(last_code.op),
                                    ) >> u32::from(last_code.bits));
                                let here2 = state.dist_code(idx);
                                if u32::from(last_code.bits) + u32::from(here2.bits) <= bits {
                                    hold >>= u32::from(last_code.bits);
                                    bits -= u32::from(last_code.bits);
                                    state.back += i32::from(last_code.bits);
                                    hold >>= u32::from(here2.bits);
                                    bits -= u32::from(here2.bits);
                                    state.back += i32::from(here2.bits);
                                    if here2.op & 64 != 0 {
                                        stream.msg = Some("invalid distance code");
                                        state.mode = InflateMode::Bad;
                                        continue 'inf_loop;
                                    }
                                    state.offset = u32::from(here2.val);
                                    state.extra = u32::from(here2.op) & 15;
                                    state.mode = InflateMode::DistExt;
                                    break;
                                }
                                if have == 0 { break 'inf_loop; }
                                have -= 1;
                                hold += u64::from(input[next]) << bits;
                                next += 1;
                                bits += 8;
                            }
                            if state.mode == InflateMode::DistExt {
                                continue 'inf_loop;
                            }
                        } else {
                            hold >>= u32::from(here.bits);
                            bits -= u32::from(here.bits);
                            state.back += i32::from(here.bits);
                            if here.op & 64 != 0 {
                                stream.msg = Some("invalid distance code");
                                state.mode = InflateMode::Bad;
                                continue 'inf_loop;
                            }
                            state.offset = u32::from(here.val);
                            state.extra = u32::from(here.op) & 15;
                            state.mode = InflateMode::DistExt;
                            break;
                        }
                        break;
                    }
                    if have == 0 { break 'inf_loop; }
                    have -= 1;
                    hold += u64::from(input[next]) << bits;
                    next += 1;
                    bits += 8;
                }
                if state.mode != InflateMode::DistExt {
                    continue 'inf_loop;
                }
                // fallthrough
            }

            InflateMode::DistExt => {
                if state.extra != 0 {
                    while bits < state.extra {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    state.offset += bits_val(hold, state.extra);
                    hold >>= state.extra;
                    bits -= state.extra;
                    state.back += state.extra as i32;
                }
                state.mode = InflateMode::Match;
                // fallthrough
            }

            InflateMode::Match => {
                if left == 0 {
                    break 'inf_loop;
                }
                let out_written = out_start - left; // bytes written so far
                if state.offset > out_written as u32 {
                    // Need to copy from window
                    let mut copy = (state.offset as usize) - out_written;
                    if copy > state.whave as usize {
                        if state.sane {
                            stream.msg = Some("invalid distance too far back");
                            state.mode = InflateMode::Bad;
                            continue 'inf_loop;
                        }
                    }
                    let from_offset;
                    if copy > state.wnext as usize {
                        copy -= state.wnext as usize;
                        from_offset = (state.wsize as usize) - copy;
                    } else {
                        from_offset = (state.wnext as usize) - copy;
                    }
                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                    // Copy from window
                    let remaining = copy.min(left).min(state.length as usize);
                    let win_len = state.window.len();
                    let mut from = from_offset;
                    for _ in 0..remaining {
                        if from >= win_len {
                            from = 0;
                        }
                        output_buf[put] = state.window[from];
                        put += 1;
                        from += 1;
                        left -= 1;
                        state.length -= 1;
                    }
                    if state.length == 0 {
                        state.mode = InflateMode::Len;
                    }
                    continue 'inf_loop;
                }
                // Copy from output buffer
                let from_pos = put - state.offset as usize;
                let mut copy = state.length as usize;
                if copy > left {
                    copy = left;
                }
                for i in 0..copy {
                    output_buf[put] = output_buf[from_pos + i];
                    put += 1;
                }
                left -= copy;
                state.length -= copy as u32;
                if state.length == 0 {
                    state.mode = InflateMode::Len;
                }
                continue 'inf_loop;
            }

            InflateMode::Lit => {
                if left == 0 {
                    break 'inf_loop;
                }
                output_buf[put] = state.length as u8;
                put += 1;
                left -= 1;
                state.mode = InflateMode::Len;
                continue 'inf_loop;
            }

            InflateMode::Check => {
                if state.wrap != 0 {
                    // NEEDBITS(32)
                    while bits < 32 {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    let out_count = out_start - left;
                    stream.total_out += out_count as u64;
                    state.total += out_count as u64;
                    if (state.wrap & 4) != 0 && out_count > 0 {
                        let check_data = &output_buf[put - out_count..put];
                        stream.adler = update_check(state.check as u32, check_data, state.flags);
                        state.check = stream.adler as u64;
                    }
                    // Reset out tracking for inf_leave
                    // (C code does: out = left here to avoid double-counting)
                    let check_hold = if state.flags != 0 {
                        hold as u32
                    } else {
                        zswap32(hold as u32)
                    };
                    if (state.wrap & 4) != 0 && check_hold != state.check as u32 {
                        stream.msg = Some("incorrect data check");
                        state.mode = InflateMode::Bad;
                        continue 'inf_loop;
                    }
                    hold = 0;
                    bits = 0;
                }
                state.mode = InflateMode::Length;
                // fallthrough
            }

            InflateMode::Length => {
                if state.wrap != 0 && state.flags != 0 {
                    // NEEDBITS(32) for gzip length
                    while bits < 32 {
                        if have == 0 { break 'inf_loop; }
                        have -= 1;
                        hold += u64::from(input[next]) << bits;
                        next += 1;
                        bits += 8;
                    }
                    if (state.wrap & 4) != 0
                        && (hold as u32) != (state.total as u32 & 0xffff_ffff)
                    {
                        stream.msg = Some("incorrect length check");
                        state.mode = InflateMode::Bad;
                        continue 'inf_loop;
                    }
                    hold = 0;
                    bits = 0;
                }
                state.mode = InflateMode::Done;
                // fallthrough
            }

            InflateMode::Done => {
                ret = ReturnCode::StreamEnd;
                break 'inf_loop;
            }

            InflateMode::Bad => {
                ret = ReturnCode::DataError;
                break 'inf_loop;
            }

            InflateMode::Mem => {
                return ReturnCode::MemError;
            }

            InflateMode::Sync => {
                return ReturnCode::StreamError;
            }
        }
    }

    // ── inf_leave ────────────────────────────────────────────────────────────
    // Save local state back (RESTORE equivalent)
    state.hold = hold;
    state.bits = bits;

    // Write output_buf data to stream
    let written = put;
    if written > 0 {
        let out_space = stream.output_remaining_mut();
        let copy_len = written.min(out_space.len());
        out_space[..copy_len].copy_from_slice(&output_buf[..copy_len]);
        let _ = stream.advance_output(copy_len);
    }

    // Advance input in stream
    let consumed = next;
    if consumed > 0 {
        let _ = stream.advance_input(consumed);
    }

    // Update window if needed
    let out_produced = out_start.saturating_sub(left);
    if state.wsize > 0
        || (out_produced > 0
            && !matches!(state.mode, InflateMode::Bad | InflateMode::Mem)
            && (!matches!(state.mode, InflateMode::Check | InflateMode::Length | InflateMode::Done)
                || flush != Z_FINISH))
    {
        if out_produced > 0 {
            let end_data = &output_buf[..put];
            if !update_window(state, end_data) {
                state.mode = InflateMode::Mem;
                return ReturnCode::MemError;
            }
        }
    }

    // Update running totals
    let in_consumed = in_start.saturating_sub(have);
    let out_done = out_start.saturating_sub(left);
    // Note: total_in/total_out already updated by advance_input/advance_output
    // But we need to update state.total
    state.total += out_done as u64;

    // Update check value
    if (state.wrap & 4) != 0 && out_done > 0 {
        let check_data = &output_buf[put.saturating_sub(out_done)..put];
        stream.adler = update_check(state.check as u32, check_data, state.flags);
        state.check = stream.adler as u64;
    }

    // Set data_type
    stream.data_type = (state.bits as i32)
        + (if state.last { 64 } else { 0 })
        + (if state.mode == InflateMode::Type { 128 } else { 0 })
        + (if state.mode == InflateMode::Len_ || state.mode == InflateMode::Copy_ {
            256
        } else {
            0
        });

    // If no progress and flush != Z_FINISH, return Z_BUF_ERROR
    if (in_consumed == 0 && out_done == 0) || flush == Z_FINISH {
        if ret == ReturnCode::Ok {
            if in_consumed == 0 && out_done == 0 {
                ret = ReturnCode::BufError;
            }
        }
    }

    ret
}

// ─── inflate_end ─────────────────────────────────────────────────────────────

/// Deallocate all dynamically allocated structures for this stream.
///
/// In Rust, `Drop` handles cleanup automatically, but this function is provided
/// for API compatibility and to clear the stream's state reference.
///
/// Port of `inflateEnd()` from `inflate.c` lines 1155–1165.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state to be cleaned up.
/// * `stream` — Mutable reference to the I/O stream.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if state is invalid.
pub fn inflate_end(state: &mut InflateState, stream: &mut ZStream) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    // In Rust, the window Vec and codes Vec are dropped when InflateState drops.
    // Clear the stream's state reference so it can be reused.
    state.window = Vec::new();
    state.codes = Vec::new();
    stream.msg = None;
    ReturnCode::Ok
}

// ─── inflate_get_dictionary ──────────────────────────────────────────────────

/// Returns the sliding dictionary being maintained by inflate.
///
/// Port of `inflateGetDictionary()` from `inflate.c` lines 1167–1185.
///
/// # Parameters
///
/// * `state` — Reference to the inflate state.
/// * `dictionary` — Mutable buffer to receive dictionary bytes.
///
/// # Returns
///
/// A tuple of (`ReturnCode`, `bytes_written`). The dictionary bytes are copied
/// into the provided buffer. Returns [`ReturnCode::StreamError`] if state is invalid.
pub fn inflate_get_dictionary(
    state: &InflateState,
    dictionary: &mut [u8],
) -> (ReturnCode, usize) {
    // Validate state
    if state.mode == InflateMode::Mem {
        return (ReturnCode::StreamError, 0);
    }

    let whave = state.whave as usize;
    if whave == 0 {
        return (ReturnCode::Ok, 0);
    }

    let copy = whave.min(dictionary.len());

    // Copy from window starting after wnext (the oldest bytes)
    let wnext = state.wnext as usize;
    if wnext >= copy {
        // No wrap-around needed
        let start = wnext - copy;
        dictionary[..copy].copy_from_slice(&state.window[start..start + copy]);
    } else {
        // Wrap-around: copy from end of window, then from beginning
        let first_part = copy - wnext;
        let wsize = state.wsize as usize;
        let start = wsize - first_part;
        dictionary[..first_part].copy_from_slice(&state.window[start..start + first_part]);
        if wnext > 0 {
            dictionary[first_part..copy].copy_from_slice(&state.window[..wnext]);
        }
    }

    (ReturnCode::Ok, copy)
}

// ─── inflate_set_dictionary ──────────────────────────────────────────────────

/// Sets the inflation dictionary.
///
/// Initializes the decompression dictionary from the given byte sequence.
/// Must be called after [`inflate`] returns [`ReturnCode::NeedDict`].
/// The dictionary's Adler-32 checksum must match the one stored in the stream.
///
/// Port of `inflateSetDictionary()` from `inflate.c` lines 1187–1217.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `stream` — Mutable reference to the I/O stream.
/// * `dictionary` — The dictionary bytes to install.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if state is invalid,
/// [`ReturnCode::DataError`] if dictionary checksum doesn't match.
#[allow(clippy::cast_possible_truncation)]
pub fn inflate_set_dictionary(
    state: &mut InflateState,
    stream: &mut ZStream,
    dictionary: &[u8],
) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }

    if state.wrap != 0 && state.mode != InflateMode::Dict {
        return ReturnCode::StreamError;
    }

    // Check for correct dictionary identifier
    if state.mode == InflateMode::Dict {
        let dict_id = adler32(0, &[]);
        let dict_check = adler32(dict_id, dictionary);
        if dict_check != state.check as u32 {
            return ReturnCode::DataError;
        }
    }

    // Copy dictionary to window using update_window
    if !update_window(state, dictionary) {
        state.mode = InflateMode::Mem;
        return ReturnCode::MemError;
    }

    state.havedict = true;
    stream.adler = adler32(0, &[]);
    state.check = u64::from(stream.adler);
    ReturnCode::Ok
}

// ─── inflate_get_header ──────────────────────────────────────────────────────

/// Requests that gzip header information be stored in the provided `GzHeader`.
///
/// Must be called after `inflate_init2()` or `inflate_reset()`, and before
/// the first call to `inflate()`. The fields of `head` are filled in during
/// decompression when a gzip stream is detected.
///
/// Port of `inflateGetHeader()` from `inflate.c` lines 1219–1231.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `head` — The gzip header structure to populate during decompression.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if state invalid
/// or not in gzip mode.
pub fn inflate_get_header(state: &mut InflateState, head: GzHeader) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    if (state.wrap & 2) == 0 {
        return ReturnCode::StreamError;
    }
    state.head = Some(Box::new(head));
    if let Some(ref mut h) = state.head {
        h.done = false;
    }
    ReturnCode::Ok
}

// ─── syncsearch ──────────────────────────────────────────────────────────────

/// Search for the sync pattern (00 00 FF FF) in a buffer.
///
/// Port of `syncsearch()` from `inflate.c` lines 1244–1262.
///
/// # Parameters
///
/// * `have` — Number of sync marker bytes already matched (0–4).
/// * `buf` — Slice of bytes to search through.
///
/// # Returns
///
/// Number of bytes consumed from `buf`. `*have == 4` after return means the
/// full sync marker was found.
fn syncsearch(have: &mut u32, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next_idx = 0usize;

    while next_idx < buf.len() {
        let byte = buf[next_idx];
        next_idx += 1;
        if got < 2 {
            if byte == 0 {
                got += 1;
            } else {
                got = 0;
            }
        } else if byte == 0 {
            got = 2;
        } else if byte == 0xff {
            got += 1;
        } else {
            got = 0;
        }
        if got == 4 {
            break;
        }
    }

    *have = got;
    next_idx
}

// ─── inflate_sync ────────────────────────────────────────────────────────────

/// Skips invalid compressed data until a possible full flush point is found.
///
/// Searches for the sync marker pattern `00 00 FF FF`. After finding it,
/// the inflate state is reset and ready for decompression from that point.
///
/// Port of `inflateSync()` from `inflate.c` lines 1264–1310.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `stream` — Mutable reference to the I/O stream.
///
/// # Returns
///
/// [`ReturnCode::Ok`] if sync point was found, [`ReturnCode::BufError`] if not enough
/// input was available, [`ReturnCode::DataError`] if no sync point was found,
/// [`ReturnCode::StreamError`] if state is invalid.
pub fn inflate_sync(state: &mut InflateState, stream: &mut ZStream) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }

    let avail = stream.avail_in();
    if avail == 0 {
        return ReturnCode::BufError;
    }

    // If not already searching for sync, initialize search from hold bits
    if state.mode != InflateMode::Sync {
        state.mode = InflateMode::Sync;
        // Extract hold bytes for initial search
        let mut buf = [0u8; 4];
        let shift_bytes = (state.bits >> 3) as usize;
        #[allow(clippy::cast_possible_truncation)]
        for (i, byte) in buf.iter_mut().enumerate().take(shift_bytes.min(4)) {
            *byte = ((state.hold >> (i as u32 * 8)) & 0xff) as u8;
        }
        state.hold = 0;
        state.bits = 0;

        let mut have = 0u32;
        let _ = syncsearch(&mut have, &buf[..shift_bytes.min(4)]);

        // If found in hold bits
        if have == 4 {
            inflate_reset_keep(state, stream);
            return ReturnCode::Ok;
        }
        state.have = have;
    }

    // Continue searching in stream input
    let input = stream.input_remaining().to_vec();
    let mut have = state.have;
    let consumed = syncsearch(&mut have, &input);
    state.have = have;

    if consumed > 0 {
        let _ = stream.advance_input(consumed);
    }

    if have != 4 {
        return ReturnCode::DataError;
    }

    inflate_reset_keep(state, stream);
    ReturnCode::Ok
}

// ─── inflate_sync_point ──────────────────────────────────────────────────────

/// Returns `true` if inflate is currently at the end of a block generated
/// by `Z_SYNC_FLUSH` or `Z_FULL_FLUSH`.
///
/// Port of `inflateSyncPoint()` from `inflate.c` lines 1320–1326.
///
/// # Parameters
///
/// * `state` — Reference to the inflate state.
///
/// # Returns
///
/// `true` if inflate is currently at a sync point (stored block, no pending bits).
#[must_use]
pub fn inflate_sync_point(state: &InflateState) -> bool {
    state.mode == InflateMode::Stored && state.bits == 0
}

// ─── inflate_copy ────────────────────────────────────────────────────────────

/// Deep-copies the inflate state for independent decompression from the
/// same point.
///
/// Port of `inflateCopy()` from `inflate.c` lines 1328–1368.
///
/// # Parameters
///
/// * `source` — Reference to the source inflate state to clone.
///
/// # Returns
///
/// A new `InflateState` that is a deep copy, or `None` on allocation failure.
#[must_use]
pub fn inflate_copy(source: &InflateState) -> Option<InflateState> {
    let mut dest = InflateState::new();

    // Copy all scalar fields
    dest.mode = source.mode;
    dest.last = source.last;
    dest.wrap = source.wrap;
    dest.havedict = source.havedict;
    dest.flags = source.flags;
    dest.dmax = source.dmax;
    dest.check = source.check;
    dest.total = source.total;
    dest.wbits = source.wbits;
    dest.wsize = source.wsize;
    dest.whave = source.whave;
    dest.wnext = source.wnext;
    dest.hold = source.hold;
    dest.bits = source.bits;
    dest.length = source.length;
    dest.offset = source.offset;
    dest.extra = source.extra;
    dest.lenbits = source.lenbits;
    dest.distbits = source.distbits;
    dest.ncode = source.ncode;
    dest.nlen = source.nlen;
    dest.ndist = source.ndist;
    dest.have = source.have;
    dest.next = source.next;
    dest.sane = source.sane;
    dest.back = source.back;
    dest.was = source.was;

    // Deep copy arrays
    dest.lens = source.lens;
    dest.work = source.work;

    // Deep copy vectors
    dest.window.clone_from(&source.window);
    dest.codes.clone_from(&source.codes);

    // Deep copy the head (if present)
    dest.head = source.head.as_ref().map(|h| {
        Box::new(GzHeader {
            text: h.text,
            time: h.time,
            xflags: h.xflags,
            os: h.os,
            extra: h.extra.clone(),
            name: h.name.clone(),
            comment: h.comment.clone(),
            hcrc: h.hcrc,
            done: h.done,
        })
    });

    // Re-point lencode and distcode to cloned codes
    dest.lencode = source.lencode;
    dest.distcode = source.distcode;

    Some(dest)
}

// ─── inflate_undermine ───────────────────────────────────────────────────────

/// Toggle subversion of deflate stream integrity checks.
///
/// Intended for debugging and testing only. When `subvert` is `true`, the
/// inflate engine will not flag invalid distance-too-far-back errors,
/// allowing inspection of corrupted data.
///
/// Port of `inflateUndermine()` from `inflate.c` lines 1370–1383.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `subvert` — `true` to disable distance checks, `false` to re-enable.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if state is invalid.
pub fn inflate_undermine(state: &mut InflateState, subvert: bool) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    state.sane = !subvert;
    ReturnCode::Ok
}

// ─── inflate_validate ────────────────────────────────────────────────────────

/// Toggle whether inflate validates the check value (Adler-32 or CRC-32).
///
/// Port of `inflateValidate()` from `inflate.c` lines 1385–1395.
///
/// # Parameters
///
/// * `state` — Mutable reference to the inflate state.
/// * `check` — `true` to enable check validation, `false` to disable.
///
/// # Returns
///
/// [`ReturnCode::Ok`] on success, [`ReturnCode::StreamError`] if state is invalid.
pub fn inflate_validate(state: &mut InflateState, check: bool) -> ReturnCode {
    if !inflate_state_check(state) {
        return ReturnCode::StreamError;
    }
    if check {
        state.wrap |= 4;
    } else {
        state.wrap &= !4;
    }
    ReturnCode::Ok
}

// ─── inflate_mark ────────────────────────────────────────────────────────────

/// Returns progress information about the inflate operation.
///
/// The returned value has two parts:
/// - Bits 0–15: number of unprocessed bits in the last processed block header,
///   or the number of remaining stored bytes (for stored blocks).
/// - Bits 16–31: one if the last block bit was set, zero if not.
///
/// The return value is -65536 if the state is not valid or if the inflate
/// engine is not in the middle of decoding a block.
///
/// Port of `inflateMark()` from `inflate.c` lines 1397–1406.
///
/// # Parameters
///
/// * `state` — Reference to the inflate state.
///
/// # Returns
///
/// An `i64` encoding progress information.
#[must_use]
#[allow(clippy::cast_lossless)]
pub fn inflate_mark(state: &InflateState) -> i64 {
    let invalid_mark: i64 = -(1i64 << 16);

    let length_part: i64 = match state.mode {
        InflateMode::Copy | InflateMode::Copy_ => i64::from(state.length),
        InflateMode::Match => i64::from(state.was - state.length),
        _ => return invalid_mark,
    };

    let last_part: i64 = if state.last { 1i64 << 16 } else { 0i64 };

    last_part + length_part
}

// ─── inflate_codes_used ──────────────────────────────────────────────────────

/// Returns the number of codes table entries currently used by the inflate
/// state. This is useful for diagnostics and testing.
///
/// Port of `inflateCodesUsed()` from `inflate.c` lines 1408–1413.
///
/// # Parameters
///
/// * `state` — Reference to the inflate state.
///
/// # Returns
///
/// The number of `Code` entries in use.
#[must_use]
pub fn inflate_codes_used(state: &InflateState) -> usize {
    state.next
}
