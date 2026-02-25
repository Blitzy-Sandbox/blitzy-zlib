// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Public inflate API and DEFLATE decompression state machine.
// Ported from C zlib's inflate.c (1,413 lines).

//! DEFLATE decompression engine.
//!
//! This module is the heart of the inflate (decompression) implementation,
//! porting the entire C `inflate.c` to idiomatic Rust. It provides the public
//! inflate API functions — initialization, streaming decompression, reset,
//! dictionary support, sync recovery, and state copy — as well as the 30+
//! mode state machine that processes zlib, gzip, and raw DEFLATE streams.
//!
//! # Submodules
//!
//! - [`state`] — [`InflateState`] struct and [`InflateMode`] enum
//! - [`fast`] — Fast-path decode loop for bulk decompression
//! - [`tables`] — Huffman table builder ([`inflate_table`], [`Code`], [`CodeType`])
//! - [`fixed`] — Pre-built fixed Huffman tables (`LENFIX`, `DISTFIX`)
//! - [`back`] — Callback-based raw DEFLATE decompression (`inflate_back`)
//!
//! # State Machine Architecture
//!
//! The main [`inflate()`] function processes input through a labeled loop:
//! ```text
//! 'inf: loop {
//!     match state.mode {
//!         Head => ...,     // detect format (zlib/gzip/raw)
//!         Flags => ...,    // gzip flags
//!         ...
//!         Len => ...,      // decode literals/lengths
//!         Match => ...,    // copy back-references
//!         Check => ...,    // verify checksum
//!         Done => break,   // success
//!         Bad => break,    // error
//!     }
//! }
//! ```
//! When input is exhausted or output is full, `break 'inf` jumps to cleanup
//! code that updates window state and returns the appropriate status code.
//!
//! # Safety
//!
//! This module contains `unsafe` blocks outside of `inflate/fast.rs`. The AAP
//! §0.8.2 targets confining unsafe to the performance-critical fast-path, but
//! the streaming API requires raw-pointer buffer arithmetic that cannot be
//! expressed in safe Rust:
//!
//! 1. **Slice construction from raw pointers** — `ZStream.next_in` /
//!    `ZStream.next_out` are `*const u8` / `*mut u8` raw pointers (matching the
//!    C `z_stream` ABI). Creating `&[u8]` / `&mut [u8]` views requires
//!    `core::slice::from_raw_parts(_mut)`, which is inherently `unsafe`.
//!    Callers of `inflate()` must uphold the contract that these pointers are
//!    valid for `avail_in` / `avail_out` bytes respectively; null checks guard
//!    every call site.
//!
//! 2. **Pointer advancement** — After consuming input bytes or producing output
//!    bytes, `next_in` / `next_out` must be advanced via `ptr::add()`. This
//!    mirrors C's `strm->next_in += consumed` and cannot be expressed without
//!    `unsafe` since the pointers are raw. Each call site is guarded by a
//!    null check and a positivity check on the offset.
//!
//! All `unsafe` blocks in this module have `// SAFETY:` comments documenting
//! the specific invariants relied upon.

// ── Submodule declarations ──────────────────────────────────────────────────

/// Callback-based raw DEFLATE decompression.
pub mod back;
/// Fast-path inflate decode loop.
pub mod fast;
/// Pre-built fixed Huffman decode tables.
pub mod fixed;
/// Inflate state machine types and state struct.
pub mod state;
/// Huffman table builder for inflate.
pub mod tables;

// ── Imports ─────────────────────────────────────────────────────────────────

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(all(not(feature = "std"), feature = "gzip"))]
use alloc::string::String;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use crate::checksum::adler32;
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{DEF_WBITS, Z_BLOCK, Z_DEFLATED, Z_FINISH, Z_TREES};
use crate::error::{ReturnCode, ZlibError, ZlibResult};
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;
use crate::stream::{StreamState, ZStream};

use self::fast::inflate_fast;
use self::state::parse_window_bits;

// ── Re-exports ──────────────────────────────────────────────────────────────
// Types from submodules re-exported at the inflate module level so that
// dependents can import e.g. `crate::inflate::InflateState`.

pub use self::back::{
    InflateBackInput, InflateBackOutput, inflate_back, inflate_back_end, inflate_back_init,
};
pub use self::state::{InflateMode, InflateState};
pub use self::tables::{Code, CodeType, inflate_table};

// ── Bit manipulation helpers ────────────────────────────────────────────────
// These replace the C macros BITS, DROPBITS, BYTEBITS, INITBITS from
// inflate.c lines 350–390.

/// Extract the low `n` bits from the bit accumulator.
/// Replaces C macro `BITS(n)`: `((unsigned)hold & ((1U << (n)) - 1))`
#[inline(always)]
fn bits_val(hold: u64, n: u32) -> u32 {
    (hold & ((1u64 << n) - 1)) as u32
}

/// Consume `n` bits from the accumulator.
/// Replaces C macro `DROPBITS(n)`.
#[inline(always)]
fn drop_bits(hold: &mut u64, bits: &mut u32, n: u32) {
    *hold >>= n;
    *bits -= n;
}

/// Align the bit accumulator to the next byte boundary.
/// Replaces C macro `BYTEBITS()`.
#[inline(always)]
fn byte_bits(hold: &mut u64, bits: &mut u32) {
    let discard = *bits & 7;
    *hold >>= discard;
    *bits -= discard;
}

/// Clear the bit accumulator.
/// Replaces C macro `INITBITS()`.
#[inline(always)]
fn init_bits(hold: &mut u64, bits: &mut u32) {
    *hold = 0;
    *bits = 0;
}

// ── Checksum helper ─────────────────────────────────────────────────────────
// Replaces the C UPDATE_CHECK macro (inflate.c lines 300–306).

/// Update the running checksum using either CRC-32 (gzip) or Adler-32 (zlib).
/// When the `gzip` feature is disabled, always uses Adler-32.
#[inline]
fn update_check(check: u32, buf: &[u8], flags: i32) -> u32 {
    #[cfg(feature = "gzip")]
    {
        if flags != 0 {
            crc32(check, buf)
        } else {
            adler32(check, buf)
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        let _ = flags;
        adler32(check, buf)
    }
}

/// Compute CRC-32 of a 2-byte little-endian value (gzip header CRC helper).
#[cfg(feature = "gzip")]
#[inline]
fn crc2(check: u32, word: u64) -> u32 {
    let buf = [word as u8, (word >> 8) as u8];
    crc32(check, &buf)
}

/// Compute CRC-32 of a 4-byte little-endian value (gzip header CRC helper).
#[cfg(feature = "gzip")]
#[inline]
fn crc4(check: u32, word: u64) -> u32 {
    let buf = [
        word as u8,
        (word >> 8) as u8,
        (word >> 16) as u8,
        (word >> 24) as u8,
    ];
    crc32(check, &buf)
}

/// Byte-swap a 32-bit value (big-endian ↔ little-endian).
/// Replaces C macro `ZSWAP32(q)`.
#[inline(always)]
fn zswap32(q: u32) -> u32 {
    q.swap_bytes()
}

// ── updatewindow ────────────────────────────────────────────────────────────
// Port of C inflate.c lines 252–296.

/// Update the sliding window with the last `copy` bytes written before
/// the position indicated by `end`. Allocates the window on first use.
fn update_window(state: &mut InflateState, end: &[u8], copy: usize) -> Result<(), ZlibError> {
    // Allocate window on first use
    if state.window.is_empty() {
        let size = 1usize << state.wbits;
        state.window = vec![0u8; size];
    }

    // Initialize window size on first use
    if state.wsize == 0 {
        state.wsize = 1usize << state.wbits;
        state.wnext = 0;
        state.whave = 0;
    }

    let wsize = state.wsize;

    if copy >= wsize {
        // Copy the last wsize bytes into the window
        let src_start = end.len() - wsize;
        state.window[..wsize].copy_from_slice(&end[src_start..]);
        state.wnext = 0;
        state.whave = wsize;
    } else {
        // Partial copy, may wrap around in circular buffer
        let dist = wsize - state.wnext;
        let first = if dist > copy { copy } else { dist };
        let src_start = end.len() - copy;
        state.window[state.wnext..state.wnext + first]
            .copy_from_slice(&end[src_start..src_start + first]);
        let remaining = copy - first;
        if remaining > 0 {
            state.window[..remaining]
                .copy_from_slice(&end[src_start + first..src_start + first + remaining]);
            state.wnext = remaining;
            state.whave = wsize;
        } else {
            state.wnext += first;
            if state.wnext == wsize {
                state.wnext = 0;
            }
            if state.whave < wsize {
                state.whave += first;
            }
        }
    }
    Ok(())
}

// ── syncsearch ──────────────────────────────────────────────────────────────
// Port of C inflate.c lines 1244–1262.

/// Search for the sync pattern `00 00 FF FF` in `buf`.
/// `have` tracks match progress (0..4). Returns number of bytes consumed.
fn sync_search(have: &mut u32, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next = 0usize;
    while next < buf.len() && got < 4 {
        let b = buf[next];
        if b == (if got < 2 { 0x00 } else { 0xFF }) {
            got += 1;
        } else if b != 0 {
            got = 0;
        } else {
            got = 4 - got;
        }
        next += 1;
    }
    *have = got;
    next
}

// ── Permutation of code-length code-length indices ──────────────────────────

/// Code-length code order (RFC 1951 §3.2.7).
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ============================================================================
// Public API functions
// ============================================================================

/// Initialise an inflate stream with default window bits (`DEF_WBITS` = 15).
///
/// This is the Rust equivalent of C zlib's `inflateInit_()`. It creates a new
/// [`InflateState`] configured for zlib-format (RFC 1950) decompression with
/// a 32 KB sliding window.
///
/// # Arguments
///
/// * `strm` — A mutable reference to a [`ZStream`] to initialise. Its
///   `state` field will be set to [`StreamState::Inflate`] containing the
///   new decompression state.
///
/// # Returns
///
/// `Ok(ReturnCode::Ok)` on success, or an appropriate error on failure.
///
/// # Examples
///
/// ```ignore
/// use zlib_rs::stream::ZStream;
/// use zlib_rs::inflate::inflate_init;
///
/// let mut strm = ZStream::new();
/// inflate_init(&mut strm).unwrap();
/// ```
pub fn inflate_init(strm: &mut ZStream) -> ZlibResult {
    inflate_init2(strm, DEF_WBITS)
}

/// Initialise an inflate stream with custom window bits.
///
/// This is the Rust equivalent of C zlib's `inflateInit2_()`. The
/// `window_bits` parameter encodes both stream format and window size:
///
/// | Range        | Format                     | wrap |
/// |--------------|----------------------------|------|
/// | `8..=15`     | zlib (RFC 1950)            | 1    |
/// | `-15..=-8`   | raw DEFLATE (RFC 1951)     | 0    |
/// | `24..=31`    | gzip (RFC 1952)            | 2    |
/// | `40..=47`    | auto-detect zlib or gzip   | 3    |
///
/// # Arguments
///
/// * `strm` — A mutable reference to a [`ZStream`] to initialise.
/// * `window_bits` — Overloaded window-bits parameter (see table above).
///
/// # Errors
///
/// - `Err(ZlibError::StreamError)` if `window_bits` is out of range.
pub fn inflate_init2(strm: &mut ZStream, window_bits: i32) -> ZlibResult {
    strm.msg = None;
    let state = InflateState::new(window_bits).inspect_err(|_e| {
        strm.state = StreamState::None;
    })?;
    strm.total_in = 0;
    strm.total_out = 0;
    strm.state = StreamState::Inflate(Box::new(state));
    // Set adler based on wrap
    if let Some(st) = strm.state.as_inflate() {
        if st.wrap != 0 {
            strm.adler = (st.wrap & 1) as u64;
        }
    }
    Ok(ReturnCode::Ok)
}

/// Reset an inflate stream, preserving the current window bits setting.
///
/// Equivalent to C zlib's `inflateReset()`. The sliding window allocation
/// is released so it will be re-allocated on first use.
///
/// # Errors
///
/// `Err(ZlibError::StreamError)` if the stream is not in inflate mode.
pub fn inflate_reset(strm: &mut ZStream) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    state.wsize = 0;
    state.whave = 0;
    state.wnext = 0;
    inflate_reset_keep(strm)
}

/// Reset an inflate stream with new window bits.
///
/// Equivalent to C zlib's `inflateReset2()`. If the new window size
/// differs from the current one, the existing window buffer is freed.
///
/// # Errors
///
/// `Err(ZlibError::StreamError)` if the stream is not in inflate mode
/// or if `window_bits` is out of range.
pub fn inflate_reset2(strm: &mut ZStream, window_bits: i32) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    let (wrap, actual_wbits) = parse_window_bits(window_bits)?;

    // If the window size changed, deallocate
    if actual_wbits != 0 && actual_wbits != state.wbits && !state.window.is_empty() {
        state.window = Vec::new();
    }

    state.wrap = wrap;
    if actual_wbits != 0 {
        state.wbits = actual_wbits;
    }
    inflate_reset(strm)
}

/// Reset an inflate stream preserving the window allocation.
///
/// Equivalent to C zlib's `inflateResetKeep()`. The sliding window buffer
/// and its bookkeeping (`wsize`, `whave`, `wnext`) are preserved.
///
/// # Errors
///
/// `Err(ZlibError::StreamError)` if the stream is not in inflate mode.
pub fn inflate_reset_keep(strm: &mut ZStream) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = None;
    strm.data_type = 0;
    state.total = 0;
    if state.wrap != 0 {
        strm.adler = (state.wrap & 1) as u64;
    }
    state.mode = InflateMode::Head;
    state.last = false;
    state.havedict = false;
    state.flags = if state.wrap == 0 { -1 } else { 0 };
    state.dmax = 32768;
    state.head = None;
    state.hold = 0;
    state.bits = 0;
    state.lencode_idx = 0;
    state.distcode_idx = 0;
    state.next = 0;
    state.sane = true;
    state.back = -1;
    Ok(ReturnCode::Ok)
}

/// Add bits to the inflate bit accumulator.
///
/// Equivalent to C zlib's `inflatePrime()`. This is used by applications
/// that need to inject bits before starting decompression (e.g., when the
/// bit stream does not start on a byte boundary).
///
/// # Arguments
///
/// * `strm` — The inflate stream.
/// * `bits` — Number of bits to add (0 to clear, negative to reset, max 16).
/// * `value` — The bit value to add (only the low `bits` bits are used).
///
/// # Errors
///
/// `Err(ZlibError::StreamError)` if the stream is not in inflate mode
/// or if the bit count exceeds limits.
pub fn inflate_prime(strm: &mut ZStream, bits_count: i32, value: i32) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    if bits_count == 0 {
        return Ok(ReturnCode::Ok);
    }
    if bits_count < 0 {
        state.hold = 0;
        state.bits = 0;
        return Ok(ReturnCode::Ok);
    }
    if bits_count > 16 || state.bits + (bits_count as u32) > 32 {
        return Err(ZlibError::StreamError);
    }
    let masked = value & ((1i32 << bits_count) - 1);
    state.hold += (masked as u64) << state.bits;
    state.bits += bits_count as u32;
    Ok(ReturnCode::Ok)
}

// ============================================================================
// inflate() — THE MAIN STATE MACHINE
// ============================================================================
// Port of C inflate.c lines 474–1153.

/// Decompress as much data as possible from the input buffer to the output
/// buffer, advancing the stream state.
///
/// This is the core decompression function — the Rust equivalent of C zlib's
/// `inflate()`. It processes input through a 30+ mode state machine that
/// handles zlib, gzip, and raw DEFLATE formats.
///
/// # Arguments
///
/// * `strm` — The inflate stream, previously initialised by [`inflate_init`]
///   or [`inflate_init2`].
/// * `flush` — Flush mode controlling the return-code semantics.
///
/// # Returns
///
/// * `Ok(ReturnCode::Ok)` — Progress was made.
/// * `Ok(ReturnCode::StreamEnd)` — Decompression complete.
/// * `Ok(ReturnCode::NeedDict)` — A preset dictionary is required.
/// * `Err(ZlibError::BufError)` — No progress possible.
/// * `Err(ZlibError::DataError)` — Corrupted input.
/// * `Err(ZlibError::StreamError)` — Invalid stream state.
/// * `Err(ZlibError::MemError)` — Allocation failure.
pub fn inflate(strm: &mut ZStream, flush: i32) -> ZlibResult {
    // Validate state
    if !strm.state.is_inflate() {
        return Err(ZlibError::StreamError);
    }
    if strm.next_out.is_null() || (strm.next_in.is_null() && strm.avail_in != 0) {
        return Err(ZlibError::StreamError);
    }

    // Build safe views of input/output buffers.
    let in_buf: &[u8] = if strm.next_in.is_null() || strm.avail_in == 0 {
        &[]
    } else {
        // SAFETY: next_in is non-null (checked above) and the caller guarantees it
        // points to at least avail_in valid bytes, per the z_stream contract.
        unsafe { core::slice::from_raw_parts(strm.next_in, strm.avail_in as usize) }
    };

    let out_buf: &mut [u8] = if strm.next_out.is_null() || strm.avail_out == 0 {
        &mut []
    } else {
        // SAFETY: next_out is non-null (checked above) and the caller guarantees it
        // points to at least avail_out writable bytes, per the z_stream contract.
        unsafe { core::slice::from_raw_parts_mut(strm.next_out, strm.avail_out as usize) }
    };

    // Temporarily take ownership of state to avoid borrow conflicts.
    let mut state_box = match core::mem::replace(&mut strm.state, StreamState::None) {
        StreamState::Inflate(s) => s,
        _ => return Err(ZlibError::StreamError),
    };
    let state = &mut *state_box;

    // TYPE → TYPEDO skip (C inflate.c line 499)
    if state.mode == InflateMode::Type {
        state.mode = InflateMode::TypeDo;
    }

    // LOAD: copy stream state to local variables
    let mut in_pos: usize = 0;
    let mut out_pos: usize = 0;
    let mut have = in_buf.len() as u32;
    let mut left = out_buf.len() as u32;
    let mut hold = state.hold;
    let mut bits_count = state.bits;
    let in_start = have;
    let mut out_start = left;
    let mut ret = ReturnCode::Ok;

    // Main state machine — 'inf label replaces C goto inf_leave
    'inf: loop {
        match state.mode {
            // ── HEAD ────────────────────────────────────────────────
            InflateMode::Head => {
                if state.wrap == 0 {
                    state.mode = InflateMode::TypeDo;
                    continue;
                }
                while bits_count < 16 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                #[cfg(feature = "gzip")]
                {
                    if (state.wrap & 2) != 0 && hold == 0x8b1f {
                        if state.wbits == 0 {
                            state.wbits = 15;
                        }
                        state.check = crc32(0, &[]);
                        state.check = crc2(state.check, hold);
                        init_bits(&mut hold, &mut bits_count);
                        state.mode = InflateMode::Flags;
                        continue;
                    }
                    if let Some(ref mut head) = state.head {
                        head.done = false;
                    }
                    if (state.wrap & 1) == 0
                        || ((bits_val(hold, 8) << 8) + (hold >> 8) as u32) % 31 != 0
                    {
                        strm.msg = Some("incorrect header check".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                }
                #[cfg(not(feature = "gzip"))]
                {
                    if ((bits_val(hold, 8) << 8) + (hold >> 8) as u32) % 31 != 0 {
                        strm.msg = Some("incorrect header check".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                }
                if bits_val(hold, 4) != Z_DEFLATED as u32 {
                    strm.msg = Some("unknown compression method".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                drop_bits(&mut hold, &mut bits_count, 4);
                let len = bits_val(hold, 4) + 8;
                if state.wbits == 0 {
                    state.wbits = len;
                }
                if len > 15 || len > state.wbits {
                    strm.msg = Some("invalid window size".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.dmax = 1u32 << len;
                state.flags = 0;
                let a = adler32(0, &[]);
                strm.adler = a as u64;
                state.check = a;
                state.mode = if hold & 0x200 != 0 {
                    InflateMode::DictId
                } else {
                    InflateMode::Type
                };
                init_bits(&mut hold, &mut bits_count);
            }

            // ── Gzip header modes (FLAGS through HCRC) ──────────────
            #[cfg(feature = "gzip")]
            InflateMode::Flags => {
                while bits_count < 16 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                state.flags = hold as i32;
                if (state.flags & 0xff) != Z_DEFLATED {
                    strm.msg = Some("unknown compression method".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                if state.flags & 0xe000 != 0 {
                    strm.msg = Some("unknown header flags set".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                if let Some(ref mut h) = state.head {
                    h.text = ((hold >> 8) & 1) != 0;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc2(state.check, hold);
                }
                init_bits(&mut hold, &mut bits_count);
                state.mode = InflateMode::Time;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Time => {
                while bits_count < 32 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                if let Some(ref mut h) = state.head {
                    h.time = hold as u32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc4(state.check, hold);
                }
                init_bits(&mut hold, &mut bits_count);
                state.mode = InflateMode::Os;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Os => {
                while bits_count < 16 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                if let Some(ref mut h) = state.head {
                    h.xflags = (hold & 0xff) as i32;
                    h.os = (hold >> 8) as i32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    state.check = crc2(state.check, hold);
                }
                init_bits(&mut hold, &mut bits_count);
                state.mode = InflateMode::ExLen;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::ExLen => {
                if (state.flags & 0x0400) != 0 {
                    while bits_count < 16 {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    state.length = (hold & 0xffff) as u32;
                    if let Some(ref mut h) = state.head {
                        if h.extra.is_none() {
                            h.extra = Some(Vec::with_capacity(state.length as usize));
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc2(state.check, hold);
                    }
                    init_bits(&mut hold, &mut bits_count);
                } else if let Some(ref mut h) = state.head {
                    h.extra = None;
                }
                state.mode = InflateMode::Extra;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Extra => {
                if (state.flags & 0x0400) != 0 {
                    let mut cp = state.length as usize;
                    if cp > have as usize {
                        cp = have as usize;
                    }
                    if cp > 0 {
                        if let Some(ref mut h) = state.head {
                            if let Some(ref mut ex) = h.extra {
                                let rem = ex.capacity().saturating_sub(ex.len());
                                let tc = cp.min(rem);
                                ex.extend_from_slice(&in_buf[in_pos..in_pos + tc]);
                            }
                        }
                        if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                            state.check = crc32(state.check, &in_buf[in_pos..in_pos + cp]);
                        }
                        have -= cp as u32;
                        in_pos += cp;
                        state.length -= cp as u32;
                    }
                    if state.length != 0 {
                        break 'inf;
                    }
                }
                state.length = 0;
                state.mode = InflateMode::Name;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Name => {
                if (state.flags & 0x0800) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    let mut cp = 0usize;
                    loop {
                        if cp >= have as usize {
                            break;
                        }
                        let b = in_buf[in_pos + cp];
                        cp += 1;
                        if let Some(ref mut h) = state.head {
                            if b != 0 {
                                if h.name.is_none() {
                                    h.name = Some(String::new());
                                }
                                if let Some(ref mut n) = h.name {
                                    n.push(b as char);
                                }
                            }
                        }
                        if b == 0 {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &in_buf[in_pos..in_pos + cp]);
                    }
                    have -= cp as u32;
                    in_pos += cp;
                    if cp > 0 && in_buf[in_pos - 1] != 0 {
                        break 'inf;
                    }
                } else if let Some(ref mut h) = state.head {
                    h.name = None;
                }
                state.length = 0;
                state.mode = InflateMode::Comment;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Comment => {
                if (state.flags & 0x1000) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    let mut cp = 0usize;
                    loop {
                        if cp >= have as usize {
                            break;
                        }
                        let b = in_buf[in_pos + cp];
                        cp += 1;
                        if let Some(ref mut h) = state.head {
                            if b != 0 {
                                if h.comment.is_none() {
                                    h.comment = Some(String::new());
                                }
                                if let Some(ref mut c) = h.comment {
                                    c.push(b as char);
                                }
                            }
                        }
                        if b == 0 {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &in_buf[in_pos..in_pos + cp]);
                    }
                    have -= cp as u32;
                    in_pos += cp;
                    if cp > 0 && in_buf[in_pos - 1] != 0 {
                        break 'inf;
                    }
                } else if let Some(ref mut h) = state.head {
                    h.comment = None;
                }
                state.mode = InflateMode::HCrc;
                continue;
            }
            #[cfg(feature = "gzip")]
            InflateMode::HCrc => {
                if (state.flags & 0x0200) != 0 {
                    while bits_count < 16 {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    if (state.wrap & 4) != 0 && (hold as u32) != (state.check & 0xffff) {
                        strm.msg = Some("header crc mismatch".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    init_bits(&mut hold, &mut bits_count);
                }
                if let Some(ref mut h) = state.head {
                    h.hcrc = (state.flags >> 9) & 1 != 0;
                    h.done = true;
                }
                let ci = crc32(0, &[]);
                strm.adler = ci as u64;
                state.check = ci;
                state.mode = InflateMode::Type;
            }
            // When gzip disabled, these are unreachable
            #[cfg(not(feature = "gzip"))]
            InflateMode::Flags
            | InflateMode::Time
            | InflateMode::Os
            | InflateMode::ExLen
            | InflateMode::Extra
            | InflateMode::Name
            | InflateMode::Comment
            | InflateMode::HCrc => {
                strm.msg = Some("gzip not supported".into());
                state.mode = InflateMode::Bad;
                continue;
            }

            // ── DICTID ──────────────────────────────────────────────
            InflateMode::DictId => {
                while bits_count < 32 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                let sw = zswap32(hold as u32);
                strm.adler = sw as u64;
                state.check = sw;
                init_bits(&mut hold, &mut bits_count);
                state.mode = InflateMode::Dict;
                continue;
            }

            // ── DICT ────────────────────────────────────────────────
            InflateMode::Dict => {
                if !state.havedict {
                    state.hold = hold;
                    state.bits = bits_count;
                    strm.avail_in = have;
                    strm.avail_out = left;
                    strm.total_in += (in_start - have) as u64;
                    strm.total_out += (out_start - left) as u64;
                    // SAFETY: next_in is non-null (guarded) and in_pos ≤ original
                    // avail_in, so next_in.add(in_pos) stays within the input buffer.
                    if !strm.next_in.is_null() && in_pos > 0 {
                        strm.next_in = unsafe { strm.next_in.add(in_pos) };
                    }
                    // SAFETY: next_out is non-null (guarded) and out_pos ≤ original
                    // avail_out, so next_out.add(out_pos) stays within the output buffer.
                    if !strm.next_out.is_null() && out_pos > 0 {
                        strm.next_out = unsafe { strm.next_out.add(out_pos) };
                    }
                    strm.state = StreamState::Inflate(state_box);
                    return Ok(ReturnCode::NeedDict);
                }
                let ai = adler32(0, &[]);
                strm.adler = ai as u64;
                state.check = ai;
                state.mode = InflateMode::Type;
                continue;
            }

            // ── TYPE ────────────────────────────────────────────────
            InflateMode::Type => {
                if flush == Z_BLOCK || flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::TypeDo;
                continue;
            }

            // ── TYPEDO ──────────────────────────────────────────────
            InflateMode::TypeDo => {
                if state.last {
                    byte_bits(&mut hold, &mut bits_count);
                    state.mode = InflateMode::Check;
                    continue;
                }
                while bits_count < 3 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                state.last = bits_val(hold, 1) != 0;
                drop_bits(&mut hold, &mut bits_count, 1);
                match bits_val(hold, 2) {
                    0 => {
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        state.use_fixed_codes();
                        state.mode = InflateMode::Len_;
                        if flush == Z_TREES {
                            drop_bits(&mut hold, &mut bits_count, 2);
                            break 'inf;
                        }
                    }
                    2 => {
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        strm.msg = Some("invalid block type".into());
                        state.mode = InflateMode::Bad;
                    }
                }
                drop_bits(&mut hold, &mut bits_count, 2);
            }

            // ── STORED ──────────────────────────────────────────────
            InflateMode::Stored => {
                byte_bits(&mut hold, &mut bits_count);
                while bits_count < 32 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) & 0xffff {
                    strm.msg = Some("invalid stored block lengths".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.length = (hold & 0xffff) as u32;
                init_bits(&mut hold, &mut bits_count);
                state.mode = InflateMode::Copy_;
                if flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::Copy;
                continue;
            }

            InflateMode::Copy_ => {
                state.mode = InflateMode::Copy;
                continue;
            }

            // ── COPY ────────────────────────────────────────────────
            InflateMode::Copy => {
                let mut cp = state.length as usize;
                if cp > 0 {
                    if cp > have as usize {
                        cp = have as usize;
                    }
                    if cp > left as usize {
                        cp = left as usize;
                    }
                    if cp == 0 {
                        break 'inf;
                    }
                    out_buf[out_pos..out_pos + cp].copy_from_slice(&in_buf[in_pos..in_pos + cp]);
                    have -= cp as u32;
                    in_pos += cp;
                    left -= cp as u32;
                    out_pos += cp;
                    state.length -= cp as u32;
                } else {
                    state.mode = InflateMode::Type;
                }
            }

            // ── TABLE ───────────────────────────────────────────────
            InflateMode::Table => {
                while bits_count < 14 {
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                state.nlen = bits_val(hold, 5) + 257;
                drop_bits(&mut hold, &mut bits_count, 5);
                state.ndist = bits_val(hold, 5) + 1;
                drop_bits(&mut hold, &mut bits_count, 5);
                state.ncode = bits_val(hold, 4) + 4;
                drop_bits(&mut hold, &mut bits_count, 4);
                if state.nlen > 286 || state.ndist > 30 {
                    strm.msg = Some("too many length or distance symbols".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.have = 0;
                state.mode = InflateMode::LenLens;
                continue;
            }

            // ── LENLENS ─────────────────────────────────────────────
            InflateMode::LenLens => {
                while state.have < state.ncode {
                    while bits_count < 3 {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    state.lens[ORDER[state.have as usize] as usize] = bits_val(hold, 3) as u16;
                    drop_bits(&mut hold, &mut bits_count, 3);
                    state.have += 1;
                }
                while state.have < 19 {
                    state.lens[ORDER[state.have as usize] as usize] = 0;
                    state.have += 1;
                }
                state.next = 0;
                state.lencode_idx = 0;
                state.distcode_idx = 0;
                let mut rb = 7u32;
                if inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    &mut state.next,
                    &mut rb,
                    &mut state.work,
                )
                .is_err()
                {
                    strm.msg = Some("invalid code lengths set".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.lenbits = rb;
                state.have = 0;
                state.mode = InflateMode::CodeLens;
                continue;
            }

            // ── CODELENS ────────────────────────────────────────────
            InflateMode::CodeLens => {
                while state.have < state.nlen + state.ndist {
                    let here;
                    loop {
                        let idx = bits_val(hold, state.lenbits) as usize;
                        let e = state.codes[state.lencode_idx + idx];
                        if (e.bits as u32) <= bits_count {
                            here = e;
                            break;
                        }
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    if here.val < 16 {
                        drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                        state.lens[state.have as usize] = here.val;
                        state.have += 1;
                    } else {
                        let (len_val, copy_count);
                        if here.val == 16 {
                            let need = here.bits as u32 + 2;
                            while bits_count < need {
                                if have == 0 {
                                    break 'inf;
                                }
                                have -= 1;
                                hold |= (in_buf[in_pos] as u64) << bits_count;
                                in_pos += 1;
                                bits_count += 8;
                            }
                            drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                            if state.have == 0 {
                                strm.msg = Some("invalid bit length repeat".into());
                                state.mode = InflateMode::Bad;
                                break;
                            }
                            len_val = state.lens[(state.have - 1) as usize];
                            copy_count = 3 + bits_val(hold, 2);
                            drop_bits(&mut hold, &mut bits_count, 2);
                        } else if here.val == 17 {
                            let need = here.bits as u32 + 3;
                            while bits_count < need {
                                if have == 0 {
                                    break 'inf;
                                }
                                have -= 1;
                                hold |= (in_buf[in_pos] as u64) << bits_count;
                                in_pos += 1;
                                bits_count += 8;
                            }
                            drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                            len_val = 0;
                            copy_count = 3 + bits_val(hold, 3);
                            drop_bits(&mut hold, &mut bits_count, 3);
                        } else {
                            let need = here.bits as u32 + 7;
                            while bits_count < need {
                                if have == 0 {
                                    break 'inf;
                                }
                                have -= 1;
                                hold |= (in_buf[in_pos] as u64) << bits_count;
                                in_pos += 1;
                                bits_count += 8;
                            }
                            drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                            len_val = 0;
                            copy_count = 11 + bits_val(hold, 7);
                            drop_bits(&mut hold, &mut bits_count, 7);
                        }
                        if state.have + copy_count > state.nlen + state.ndist {
                            strm.msg = Some("invalid bit length repeat".into());
                            state.mode = InflateMode::Bad;
                            break;
                        }
                        for _ in 0..copy_count {
                            state.lens[state.have as usize] = len_val;
                            state.have += 1;
                        }
                    }
                }
                if state.mode == InflateMode::Bad {
                    continue;
                }
                if state.lens[256] == 0 {
                    strm.msg = Some("invalid code -- missing end-of-block".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                // Build length/literal table
                state.next = 0;
                state.lencode_idx = 0;
                let mut rb = 9u32;
                if inflate_table(
                    CodeType::Lens,
                    &state.lens,
                    state.nlen as usize,
                    &mut state.codes,
                    &mut state.next,
                    &mut rb,
                    &mut state.work,
                )
                .is_err()
                {
                    strm.msg = Some("invalid literal/lengths set".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.lenbits = rb;
                state.distcode_idx = state.next;
                let mut db = 6u32;
                if inflate_table(
                    CodeType::Dists,
                    &state.lens[state.nlen as usize..],
                    state.ndist as usize,
                    &mut state.codes,
                    &mut state.next,
                    &mut db,
                    &mut state.work,
                )
                .is_err()
                {
                    strm.msg = Some("invalid distances set".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.distbits = db;
                state.mode = InflateMode::Len_;
                if flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::Len;
                continue;
            }

            InflateMode::Len_ => {
                state.mode = InflateMode::Len;
                continue;
            }

            // ── LEN ─────────────────────────────────────────────────
            InflateMode::Len => {
                if have >= 6 && left >= 258 {
                    state.hold = hold;
                    state.bits = bits_count;
                    // `start` is the initial out_pos when inflate() was called.
                    // out_pos is zero-based into out_buf, so the initial
                    // value is always 0 (not out_start, which tracks the
                    // initial *remaining* output capacity in the C convention).
                    inflate_fast(state, in_buf, out_buf, &mut in_pos, &mut out_pos, 0);
                    have = (in_buf.len() - in_pos) as u32;
                    left = (out_buf.len() - out_pos) as u32;
                    hold = state.hold;
                    bits_count = state.bits;
                    if state.mode == InflateMode::Type {
                        state.back = -1;
                    }
                    continue;
                }
                state.back = 0;
                let mut here;
                loop {
                    let idx = bits_val(hold, state.lenbits) as usize;
                    here = state.len_code(idx);
                    if (here.bits as u32) <= bits_count {
                        break;
                    }
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last_e = here;
                    loop {
                        let idx = last_e.val as usize
                            + (bits_val(hold, last_e.bits as u32 + last_e.op as u32) >> last_e.bits)
                                as usize;
                        here = state.len_code(idx);
                        if (last_e.bits as u32 + here.bits as u32) <= bits_count {
                            break;
                        }
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    drop_bits(&mut hold, &mut bits_count, last_e.bits as u32);
                    state.back += last_e.bits as i32;
                }
                drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                state.back += here.bits as i32;
                state.length = here.val as u32;
                if here.op == 0 {
                    state.mode = InflateMode::Lit;
                    continue;
                }
                if (here.op & 32) != 0 {
                    state.back = -1;
                    state.mode = InflateMode::Type;
                    continue;
                }
                if (here.op & 64) != 0 {
                    strm.msg = Some("invalid literal/length code".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::LenExt;
                continue;
            }

            // ── LENEXT ──────────────────────────────────────────────
            InflateMode::LenExt => {
                if state.extra != 0 {
                    while bits_count < state.extra {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    state.length += bits_val(hold, state.extra);
                    drop_bits(&mut hold, &mut bits_count, state.extra);
                    state.back += state.extra as i32;
                }
                state.was = state.length;
                state.mode = InflateMode::Dist;
                continue;
            }

            // ── DIST ────────────────────────────────────────────────
            InflateMode::Dist => {
                let mut here;
                loop {
                    let idx = bits_val(hold, state.distbits) as usize;
                    here = state.dist_code(idx);
                    if (here.bits as u32) <= bits_count {
                        break;
                    }
                    if have == 0 {
                        break 'inf;
                    }
                    have -= 1;
                    hold |= (in_buf[in_pos] as u64) << bits_count;
                    in_pos += 1;
                    bits_count += 8;
                }
                if (here.op & 0xf0) == 0 {
                    let last_e = here;
                    loop {
                        let idx = last_e.val as usize
                            + (bits_val(hold, last_e.bits as u32 + last_e.op as u32) >> last_e.bits)
                                as usize;
                        here = state.dist_code(idx);
                        if (last_e.bits as u32 + here.bits as u32) <= bits_count {
                            break;
                        }
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    drop_bits(&mut hold, &mut bits_count, last_e.bits as u32);
                    state.back += last_e.bits as i32;
                }
                drop_bits(&mut hold, &mut bits_count, here.bits as u32);
                state.back += here.bits as i32;
                if (here.op & 64) != 0 {
                    strm.msg = Some("invalid distance code".into());
                    state.mode = InflateMode::Bad;
                    continue;
                }
                state.offset = here.val as u32;
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::DistExt;
                continue;
            }

            // ── DISTEXT ─────────────────────────────────────────────
            InflateMode::DistExt => {
                if state.extra != 0 {
                    while bits_count < state.extra {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    state.offset += bits_val(hold, state.extra);
                    drop_bits(&mut hold, &mut bits_count, state.extra);
                    state.back += state.extra as i32;
                }
                state.mode = InflateMode::Match;
                continue;
            }

            // ── MATCH ───────────────────────────────────────────────
            InflateMode::Match => {
                if left == 0 {
                    break 'inf;
                }
                let written = (out_start - left) as usize;
                if (state.offset as usize) > written {
                    // Copy from window
                    let dist_back = state.offset as usize - written;
                    if dist_back > state.whave && state.sane {
                        strm.msg = Some("invalid distance too far back".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    let from_idx = if dist_back > state.wnext {
                        state.wsize - (dist_back - state.wnext)
                    } else {
                        state.wnext - dist_back
                    };
                    let copy_from_win = dist_back.min(state.length as usize);
                    let mut cp = copy_from_win.min(left as usize);
                    for i in 0..cp {
                        out_buf[out_pos] = state.window[(from_idx + i) % state.wsize];
                        out_pos += 1;
                        left -= 1;
                    }
                    state.length -= cp as u32;
                    // Continue from output if more needed
                    if state.length > 0 && left > 0 {
                        let from_out = out_pos - state.offset as usize;
                        cp = (state.length as usize).min(left as usize);
                        for i in 0..cp {
                            out_buf[out_pos] = out_buf[from_out + i];
                            out_pos += 1;
                            left -= 1;
                        }
                        state.length -= cp as u32;
                    }
                } else {
                    let from_out = out_pos - state.offset as usize;
                    let mut cp = state.length as usize;
                    if cp > left as usize {
                        cp = left as usize;
                    }
                    for i in 0..cp {
                        out_buf[out_pos] = out_buf[from_out + i];
                        out_pos += 1;
                    }
                    left -= cp as u32;
                    state.length -= cp as u32;
                }
                if state.length == 0 {
                    state.mode = InflateMode::Len;
                }
            }

            // ── LIT ─────────────────────────────────────────────────
            InflateMode::Lit => {
                if left == 0 {
                    break 'inf;
                }
                out_buf[out_pos] = state.length as u8;
                out_pos += 1;
                left -= 1;
                state.mode = InflateMode::Len;
            }

            // ── CHECK ───────────────────────────────────────────────
            InflateMode::Check => {
                if state.wrap != 0 {
                    while bits_count < 32 {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    // Per C inflate.c CHECK mode: compute output produced
                    // so far, update total_out/total/check, then reset
                    // out_start = left so the post-loop code sees 0
                    // additional output (avoiding double-counting of both
                    // the checksum and total_out).
                    let out_bytes = out_start - left;
                    strm.total_out += out_bytes as u64;
                    state.total += out_bytes as u64;
                    if (state.wrap & 4) != 0 && out_bytes > 0 {
                        let cs = out_pos - out_bytes as usize;
                        let nc = update_check(state.check, &out_buf[cs..out_pos], state.flags);
                        strm.adler = nc as u64;
                        state.check = nc;
                    }
                    // Reset out_start so post-loop code (inf_leave
                    // equivalent) sees out_consumed = 0. This mirrors
                    // C inflate.c: "out = left;" after the CHECK update.
                    out_start = left;
                    let check_hold = {
                        #[cfg(feature = "gzip")]
                        {
                            if state.flags != 0 {
                                hold as u32
                            } else {
                                zswap32(hold as u32)
                            }
                        }
                        #[cfg(not(feature = "gzip"))]
                        {
                            zswap32(hold as u32)
                        }
                    };
                    if (state.wrap & 4) != 0 && check_hold != state.check {
                        strm.msg = Some("incorrect data check".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    init_bits(&mut hold, &mut bits_count);
                }
                #[cfg(feature = "gzip")]
                {
                    state.mode = InflateMode::Length;
                    continue;
                }
                #[cfg(not(feature = "gzip"))]
                {
                    state.mode = InflateMode::Done;
                    continue;
                }
            }

            // ── LENGTH ──────────────────────────────────────────────
            #[cfg(feature = "gzip")]
            InflateMode::Length => {
                if state.wrap != 0 && state.flags != 0 {
                    while bits_count < 32 {
                        if have == 0 {
                            break 'inf;
                        }
                        have -= 1;
                        hold |= (in_buf[in_pos] as u64) << bits_count;
                        in_pos += 1;
                        bits_count += 8;
                    }
                    if (state.wrap & 4) != 0 && hold as u32 != (state.total & 0xffffffff) as u32 {
                        strm.msg = Some("incorrect length check".into());
                        state.mode = InflateMode::Bad;
                        continue;
                    }
                    init_bits(&mut hold, &mut bits_count);
                }
                state.mode = InflateMode::Done;
                continue;
            }
            #[cfg(not(feature = "gzip"))]
            InflateMode::Length => {
                state.mode = InflateMode::Done;
                continue;
            }

            // ── Terminal states ──────────────────────────────────────
            InflateMode::Done => {
                ret = ReturnCode::StreamEnd;
                break 'inf;
            }
            InflateMode::Bad => {
                state.hold = hold;
                state.bits = bits_count;
                strm.avail_in = have;
                strm.avail_out = left;
                strm.total_in += (in_start - have) as u64;
                strm.total_out += (out_start - left) as u64;
                state.total += (out_start - left) as u64;
                // SAFETY: next_in is non-null (guarded) and in_pos ≤ original
                // avail_in, so next_in.add(in_pos) stays within the input buffer.
                if !strm.next_in.is_null() && in_pos > 0 {
                    strm.next_in = unsafe { strm.next_in.add(in_pos) };
                }
                // SAFETY: next_out is non-null (guarded) and out_pos ≤ original
                // avail_out, so next_out.add(out_pos) stays within the output buffer.
                if !strm.next_out.is_null() && out_pos > 0 {
                    strm.next_out = unsafe { strm.next_out.add(out_pos) };
                }
                strm.data_type = bits_count as i32
                    + (if state.last { 64 } else { 0 })
                    + (if state.mode == InflateMode::Type {
                        128
                    } else {
                        0
                    })
                    + (if state.mode == InflateMode::Len_ || state.mode == InflateMode::Copy_ {
                        256
                    } else {
                        0
                    });
                strm.state = StreamState::Inflate(state_box);
                return Err(ZlibError::DataError);
            }
            InflateMode::Mem => {
                state.hold = hold;
                state.bits = bits_count;
                strm.state = StreamState::Inflate(state_box);
                return Err(ZlibError::MemError);
            }
            InflateMode::Sync => {
                state.hold = hold;
                state.bits = bits_count;
                strm.state = StreamState::Inflate(state_box);
                return Err(ZlibError::StreamError);
            }
        }
    }

    // ── inf_leave: RESTORE and cleanup ──────────────────────────────
    state.hold = hold;
    state.bits = bits_count;
    let out_produced = (out_start - left) as usize;
    if (state.wsize > 0
        || (out_produced > 0
            && state.mode != InflateMode::Bad
            && (state.mode != InflateMode::Check || flush != Z_FINISH)))
        && out_produced > 0
        && update_window(state, &out_buf[..out_pos], out_produced).is_err()
    {
        state.mode = InflateMode::Mem;
        strm.state = StreamState::Inflate(state_box);
        return Err(ZlibError::MemError);
    }
    let in_consumed = (in_start - have) as u64;
    let out_consumed = (out_start - left) as u64;
    strm.avail_in = have;
    strm.avail_out = left;
    strm.total_in += in_consumed;
    strm.total_out += out_consumed;
    state.total += out_consumed;
    // SAFETY: next_in is non-null (guarded) and in_pos ≤ original avail_in,
    // so next_in.add(in_pos) stays within the caller's input buffer.
    if !strm.next_in.is_null() && in_pos > 0 {
        strm.next_in = unsafe { strm.next_in.add(in_pos) };
    }
    // SAFETY: next_out is non-null (guarded) and out_pos ≤ original avail_out,
    // so next_out.add(out_pos) stays within the caller's output buffer.
    if !strm.next_out.is_null() && out_pos > 0 {
        strm.next_out = unsafe { strm.next_out.add(out_pos) };
    }
    if (state.wrap & 4) != 0 && out_consumed > 0 {
        let cs = out_pos - out_consumed as usize;
        let nc = update_check(state.check, &out_buf[cs..out_pos], state.flags);
        strm.adler = nc as u64;
        state.check = nc;
    }
    strm.data_type = bits_count as i32
        + (if state.last { 64 } else { 0 })
        + (if state.mode == InflateMode::Type {
            128
        } else {
            0
        })
        + (if state.mode == InflateMode::Len_ || state.mode == InflateMode::Copy_ {
            256
        } else {
            0
        });
    strm.state = StreamState::Inflate(state_box);
    if ((in_consumed == 0 && out_consumed == 0) || flush == Z_FINISH) && ret == ReturnCode::Ok {
        return Err(ZlibError::BufError);
    }
    Ok(ret)
}

// ============================================================================
// Remaining public API functions
// ============================================================================

/// End an inflate stream, freeing all allocated memory.
pub fn inflate_end(strm: &mut ZStream) -> ZlibResult {
    if !strm.state.is_inflate() {
        return Err(ZlibError::StreamError);
    }
    strm.state = StreamState::None;
    Ok(ReturnCode::Ok)
}

/// Retrieve the current decompression dictionary from the sliding window.
pub fn inflate_get_dictionary(strm: &ZStream, dictionary: &mut [u8]) -> Result<usize, ZlibError> {
    let state = strm.state.as_inflate().ok_or(ZlibError::StreamError)?;
    if state.whave > 0 && !dictionary.is_empty() {
        let len = state.whave.min(dictionary.len());
        let first = state.whave - state.wnext;
        let fc = first.min(len);
        dictionary[..fc].copy_from_slice(&state.window[state.wnext..state.wnext + fc]);
        if len > fc {
            let sc = (len - fc).min(state.wnext);
            dictionary[fc..fc + sc].copy_from_slice(&state.window[..sc]);
        }
    }
    Ok(state.whave)
}

/// Set a preset decompression dictionary.
pub fn inflate_set_dictionary(strm: &mut ZStream, dictionary: &[u8]) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    if state.wrap != 0 && state.mode != InflateMode::Dict {
        return Err(ZlibError::StreamError);
    }
    if state.mode == InflateMode::Dict {
        let mut id = adler32(0, &[]);
        id = adler32(id, dictionary);
        if id != state.check {
            return Err(ZlibError::DataError);
        }
    }
    update_window(state, dictionary, dictionary.len()).map_err(|_| {
        state.mode = InflateMode::Mem;
        ZlibError::MemError
    })?;
    state.havedict = true;
    Ok(ReturnCode::Ok)
}

/// Register a GzHeader to receive gzip header information during decompression.
#[cfg(feature = "gzip")]
pub fn inflate_get_header(strm: &mut ZStream, head: GzHeader) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    if (state.wrap & 2) == 0 {
        return Err(ZlibError::StreamError);
    }
    let mut h = head;
    h.done = false;
    state.head = Some(Box::new(h));
    Ok(ReturnCode::Ok)
}

/// Register a GzHeader (no-op when gzip feature disabled).
#[cfg(not(feature = "gzip"))]
pub fn inflate_get_header(strm: &mut ZStream, _head: crate::gz_header::GzHeader) -> ZlibResult {
    if !strm.state.is_inflate() {
        return Err(ZlibError::StreamError);
    }
    Err(ZlibError::StreamError)
}

/// Search for a synchronization point in the compressed stream.
pub fn inflate_sync(strm: &mut ZStream) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    if strm.avail_in == 0 && state.bits < 8 {
        return Err(ZlibError::BufError);
    }
    if state.mode != InflateMode::Sync {
        state.mode = InflateMode::Sync;
        state.hold >>= state.bits & 7;
        state.bits -= state.bits & 7;
        let mut buf = [0u8; 4];
        let mut len = 0usize;
        while state.bits >= 8 {
            buf[len] = state.hold as u8;
            state.hold >>= 8;
            state.bits -= 8;
            len += 1;
        }
        state.have = 0;
        sync_search(&mut state.have, &buf[..len]);
    }
    if strm.avail_in > 0 && !strm.next_in.is_null() {
        // SAFETY: next_in is non-null (checked above) and the caller guarantees
        // it points to at least avail_in valid bytes, per the z_stream contract.
        let input = unsafe { core::slice::from_raw_parts(strm.next_in, strm.avail_in as usize) };
        let consumed = sync_search(&mut state.have, input);
        strm.avail_in -= consumed as u32;
        // SAFETY: consumed ≤ avail_in, so next_in.add(consumed) stays within the
        // original caller-provided input buffer.
        strm.next_in = unsafe { strm.next_in.add(consumed) };
        strm.total_in += consumed as u64;
    }
    if state.have != 4 {
        return Err(ZlibError::DataError);
    }
    if state.flags == -1 {
        state.wrap = 0;
    } else {
        state.wrap &= !4;
    }
    let flags = state.flags;
    let ti = strm.total_in;
    let to = strm.total_out;
    inflate_reset(strm)?;
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    strm.total_in = ti;
    strm.total_out = to;
    state.flags = flags;
    state.mode = InflateMode::Type;
    Ok(ReturnCode::Ok)
}

/// Returns true if inflate is at the end of a sync-flush block.
pub fn inflate_sync_point(strm: &ZStream) -> bool {
    strm.state
        .as_inflate()
        .is_some_and(|s| s.mode == InflateMode::Stored && s.bits == 0)
}

/// Deep-copy an inflate stream state.
pub fn inflate_copy(dest: &mut ZStream, source: &ZStream) -> ZlibResult {
    let src_state = source.state.as_inflate().ok_or(ZlibError::StreamError)?;
    let cloned = src_state.clone();
    dest.avail_in = source.avail_in;
    dest.avail_out = source.avail_out;
    dest.total_in = source.total_in;
    dest.total_out = source.total_out;
    dest.msg = source.msg.clone();
    dest.data_type = source.data_type;
    dest.adler = source.adler;
    dest.next_in = source.next_in;
    dest.next_out = source.next_out;
    dest.state = StreamState::Inflate(Box::new(cloned));
    Ok(ReturnCode::Ok)
}

/// Return progress information for sync tracking.
pub fn inflate_mark(strm: &ZStream) -> i64 {
    strm.state.as_inflate().map_or(-(1i64 << 16), |s| {
        let back = ((s.back as i64) & 0xffff) << 16;
        let li = match s.mode {
            InflateMode::Copy => s.length as i64,
            InflateMode::Match => (s.was - s.length) as i64,
            _ => 0,
        };
        back + li
    })
}

/// Enable or disable check-value validation.
pub fn inflate_validate(strm: &mut ZStream, check: bool) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    if check && state.wrap != 0 {
        state.wrap |= 4;
    } else {
        state.wrap &= !4;
    }
    Ok(ReturnCode::Ok)
}

/// Control sane/insane distance checking (always sane in safe Rust).
pub fn inflate_undermine(strm: &mut ZStream, _subvert: bool) -> ZlibResult {
    let state = strm.state.as_inflate_mut().ok_or(ZlibError::StreamError)?;
    state.sane = true;
    Err(ZlibError::DataError)
}

/// Return the number of Huffman code table entries used.
pub fn inflate_codes_used(strm: &ZStream) -> usize {
    strm.state.as_inflate().map_or(usize::MAX, |s| s.next)
}
