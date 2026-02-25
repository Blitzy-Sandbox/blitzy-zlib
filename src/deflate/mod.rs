// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// Public DEFLATE compression API and core internal helper functions.
// Port of C `deflate.c` (2,185 lines) with API signatures from `zlib.h`,
// type definitions from `deflate.h` / `zutil.h`.

//! DEFLATE compression engine — public API and core helpers.
//!
//! This module implements the complete DEFLATE compression algorithm as
//! specified in [RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951),
//! with zlib ([RFC 1950](https://datatracker.ietf.org/doc/html/rfc1950))
//! and gzip ([RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952))
//! framing support.
//!
//! # Architecture
//!
//! The deflate module is split into submodules:
//! - [`state`] — core data types: [`DeflateState`], [`DeflateStatus`],
//!   [`BlockState`], [`HuffmanNode`], [`TreeDesc`]
//! - [`trees`] — Huffman tree construction and block output
//! - [`strategy`] — configuration table and strategy dispatch
//! - [`fast`], [`slow`], [`stored`], [`huff`], [`rle`] — the five
//!   compression strategy functions
//!
//! # Public API
//!
//! The 14 public functions in this module directly map to C zlib's
//! `deflate*` family:
//!
//! | Rust function | C function |
//! |---------------|------------|
//! | [`deflate_init`] | `deflateInit_` |
//! | [`deflate_init2`] | `deflateInit2_` |
//! | [`deflate`] | `deflate` |
//! | [`deflate_end`] | `deflateEnd` |
//! | [`deflate_reset`] | `deflateReset` |
//! | [`deflate_params`] | `deflateParams` |
//! | [`deflate_tune`] | `deflateTune` |
//! | [`deflate_bound`] | `deflateBound` |
//! | [`deflate_pending`] | `deflatePending` |
//! | [`deflate_prime`] | `deflatePrime` |
//! | [`deflate_set_header`] | `deflateSetHeader` |
//! | [`deflate_set_dictionary`] | `deflateSetDictionary` |
//! | [`deflate_get_dictionary`] | `deflateGetDictionary` |
//! | [`deflate_copy`] | `deflateCopy` |

// Suppress dead-code warnings: items are consumed by sibling modules
// (inflate, gz, util) being created in parallel.
#![allow(dead_code)]

// ============================================================================
// Submodule declarations
// ============================================================================

pub mod fast;
pub mod huff;
pub mod rle;
pub mod slow;
pub mod state;
pub mod stored;
pub mod strategy;
pub mod trees;

// ============================================================================
// Imports — crate-level modules
// ============================================================================

use core::cmp::min;

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};

use crate::checksum::adler32::{adler32, adler32_z};
#[cfg(feature = "gzip")]
use crate::checksum::crc32::{crc32, crc32_z};
use crate::constants::{
    DEF_MEM_LEVEL, MAX_MATCH, MAX_MEM_LEVEL, MAX_WBITS, MIN_MATCH, PRESET_DICT, Z_BLOCK,
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH, Z_FIXED, Z_FULL_FLUSH,
    Z_HUFFMAN_ONLY, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_RLE, Z_UNKNOWN,
};
use crate::error::{ReturnCode, ZlibError};
use crate::gz_header::GzHeader;
use crate::stream::ZStream;

// ============================================================================
// Imports — local submodules
// ============================================================================

use self::fast::deflate_fast;
use self::huff::deflate_huff;
use self::rle::deflate_rle;
use self::slow::deflate_slow;
use self::state::{BUF_SIZE, BlockState, MIN_LOOKAHEAD};
use self::stored::deflate_stored;
use self::strategy::{CONFIG_TABLE, CompressionStrategy, flush_rank};
use self::trees::{tr_align, tr_flush_bits, tr_flush_block, tr_init, tr_stored_block};

// ============================================================================
// Re-exports
// ============================================================================

pub use self::state::{DeflateState, DeflateStatus};

// ============================================================================
// Constants (from deflate.c and deflate.h)
// ============================================================================

/// End of hash chains — matches C `#define NIL 0` (deflate.c:85).
pub const NIL: u16 = 0;

/// Matches of length 3 farther than this are too old and discarded.
/// Matches C `#define TOO_FAR 4096` (deflate.c:89).
pub const TOO_FAR: usize = 4096;

/// Maximum stored block length in deflate format (not including 5-byte header).
/// Equal to 65535 (0xFFFF).
pub const MAX_STORED: usize = 65535;

/// Number of bytes to initialize past the end of current data to avoid
/// memory checker warnings in `longest_match`. Equal to [`MAX_MATCH`] = 258.
pub const WIN_INIT: usize = MAX_MATCH;

/// OS code for the gzip header. Platform-dependent.
/// Unix = 3, Windows = 0, macOS = 7 (treated as Unix).
#[cfg(target_os = "windows")]
pub const OS_CODE: u8 = 0x00;

/// OS code for the gzip header on non-Windows platforms (Unix = 3).
#[cfg(not(target_os = "windows"))]
pub const OS_CODE: u8 = 0x03;

// ============================================================================
// Inline helper functions — converted from C macros
// ============================================================================

/// Appends a single byte to the pending output buffer.
///
/// Port of C `put_byte(s, c)` macro: `s->pending_buf[s->pending++] = c`.
#[inline(always)]
pub fn put_byte(s: &mut DeflateState, c: u8) {
    s.pending_buf[s.pending] = c;
    s.pending += 1;
}

/// Writes a 16-bit value in MSB (big-endian) order to pending_buf.
///
/// Port of C `putShortMSB` (deflate.c:939-942).
#[inline(always)]
pub fn put_short_msb(s: &mut DeflateState, b: u16) {
    put_byte(s, (b >> 8) as u8);
    put_byte(s, (b & 0xff) as u8);
}

/// Updates the running hash value with a new input byte.
///
/// Port of C `UPDATE_HASH` macro (deflate.c:141):
/// `h = ((h << s->hash_shift) ^ c) & s->hash_mask`
#[inline(always)]
pub fn update_hash(s: &mut DeflateState, c: u8) -> u32 {
    s.ins_h = ((s.ins_h << s.hash_shift) ^ (c as u32)) & s.hash_mask;
    s.ins_h
}

/// Inserts string at `str_pos` into the hash chain, returns head of chain.
///
/// Port of C `INSERT_STRING` macro (deflate.c:160-163).
#[inline(always)]
pub fn insert_string(s: &mut DeflateState, str_pos: usize) -> u16 {
    update_hash(s, s.window[str_pos + MIN_MATCH - 1]);
    let match_head = s.head[s.ins_h as usize];
    s.prev[str_pos & s.w_mask] = match_head;
    s.head[s.ins_h as usize] = str_pos as u16;
    match_head
}

/// Zeros all entries in the hash table head array.
///
/// Port of C `CLEAR_HASH` macro (deflate.c:170-175).
#[inline(always)]
pub fn clear_hash(s: &mut DeflateState) {
    for h in s.head[..s.hash_size].iter_mut() {
        *h = NIL;
    }
    s.slid = false;
}

/// Returns the maximum match distance for the current window.
///
/// Port of C `MAX_DIST(s)` macro from deflate.h.
/// Equal to `w_size - MIN_LOOKAHEAD`.
#[inline(always)]
pub fn max_dist(s: &DeflateState) -> usize {
    s.w_size.saturating_sub(MIN_LOOKAHEAD)
}

/// Flushes the current block via `tr_flush_block`, then flushes pending
/// bytes to the output buffer.
///
/// Port of C `FLUSH_BLOCK_ONLY` macro (deflate.c:1630-1639).
pub fn flush_block_only(s: &mut DeflateState, strm: &mut ZStream, last: bool) {
    let stored_len = (s.strstart as i64 - s.block_start) as u64;
    let block_start_offset = if s.block_start >= 0 {
        Some(s.block_start as usize)
    } else {
        None
    };
    // Copy block data to avoid overlapping borrows (we need &s.window and &mut s
    // simultaneously for tr_flush_block).
    if let Some(off) = block_start_offset {
        let end = off + stored_len as usize;
        let block_data: Vec<u8> = s.window[off..end].to_vec();
        tr_flush_block(s, Some(&block_data), stored_len, last);
    } else {
        tr_flush_block(s, None, stored_len, last);
    }
    s.block_start = s.strstart as i64;
    flush_pending_from_state(s, strm);
}

/// Flushes the current block; returns `Some(BlockState)` if avail_out is
/// exhausted after the flush, or `None` if no early return is needed.
///
/// Port of C `FLUSH_BLOCK` macro (deflate.c:1642-1645).
pub fn flush_block(s: &mut DeflateState, strm: &mut ZStream, last: bool) -> Option<BlockState> {
    flush_block_only(s, strm, last);
    if strm.avail_out == 0 {
        return Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        });
    }
    None
}

// ============================================================================
// Core internal functions
// ============================================================================

/// Reads bytes from the stream input buffer into `buf`.
///
/// Updates the running checksum (Adler-32 for zlib, CRC-32 for gzip)
/// based on the `wrap` mode of the deflate state.
///
/// Port of C `read_buf` (deflate.c:219-240).
///
/// # Parameters
///
/// - `strm` — the stream, providing input bytes via `next_in` / `avail_in`.
/// - `s` — the deflate state, providing `wrap` mode for checksum selection.
/// - `buf` — destination buffer to copy input bytes into.
/// - `size` — maximum number of bytes to read.
///
/// # Returns
///
/// Number of bytes actually read (may be 0 if no input is available).
pub fn read_buf(strm: &mut ZStream, wrap: i32, buf: &mut [u8], size: usize) -> usize {
    let len = min(strm.avail_in as usize, size);
    if len == 0 {
        return 0;
    }

    strm.avail_in -= len as u32;

    // Copy input bytes into buf
    // SAFETY: next_in is a valid pointer maintained by set_input()
    let src = unsafe { core::slice::from_raw_parts(strm.next_in, len) };
    buf[..len].copy_from_slice(src);

    // Update checksum based on wrap mode
    let abs_wrap = if wrap < 0 { -wrap } else { wrap };
    if abs_wrap == 1 {
        strm.adler = adler32_z(strm.adler as u32, &buf[..len]) as u64;
    }
    #[cfg(feature = "gzip")]
    {
        if abs_wrap == 2 {
            strm.adler = crc32_z(strm.adler as u32, &buf[..len]) as u64;
        }
    }

    // Advance input pointer and total
    // SAFETY: `len <= avail_in`, so `next_in.add(len)` stays within (or one-
    // past-end of) the caller's input buffer, satisfying `pointer::add`.
    strm.next_in = unsafe { strm.next_in.add(len) };
    strm.total_in += len as u64;

    len
}

/// Reads input bytes directly into the window at the current strstart position.
///
/// This is a simplified version of fill_window used by deflate_stored's
/// fallback path to load remaining input into the window without the
/// full hash table maintenance that fill_window performs.
///
/// # Safety
///
/// The caller must ensure `to_read` does not exceed the available space
/// in the window buffer beyond `strstart`.
pub fn fill_window_read(s: &mut DeflateState, strm: &mut ZStream, to_read: usize) {
    let start = s.strstart;
    let n = read_buf(strm, s.wrap, &mut s.window[start..start + to_read], to_read);
    s.strstart += n;
    s.insert += min(n, s.w_size.saturating_sub(s.insert));
}

/// Slides the hash table when the window slides down.
///
/// For each entry in `head` and `prev`, subtracts `w_size` if the value
/// is >= `w_size`, otherwise sets to NIL. This keeps hash chain positions
/// valid relative to the new window position.
///
/// Port of C `slide_hash` (deflate.c:187-210).
pub fn slide_hash(s: &mut DeflateState) {
    let wsize = s.w_size;

    // Slide head table
    for m in s.head[..s.hash_size].iter_mut() {
        *m = if *m >= wsize as u16 {
            *m - wsize as u16
        } else {
            NIL
        };
    }

    // Slide prev table
    for m in s.prev[..wsize].iter_mut() {
        *m = if *m >= wsize as u16 {
            *m - wsize as u16
        } else {
            NIL
        };
    }

    s.slid = true;
}

/// Fills the sliding window when lookahead becomes insufficient.
///
/// This is the main input-reading function. It reads input via `read_buf`,
/// slides the window when necessary, and inserts new strings into the hash
/// table.
///
/// Port of C `fill_window` (deflate.c:252-376).
pub fn fill_window(s: &mut DeflateState, strm: &mut ZStream) {
    let wsize = s.w_size;

    loop {
        // Amount of free space at the end of the window
        let more = s.window_size - s.lookahead - s.strstart;

        // If the window is almost full and there is insufficient lookahead,
        // move the upper half to the lower one to make room.
        if s.strstart >= wsize + max_dist(s) {
            // Copy upper half to lower half
            s.window.copy_within(wsize..wsize + wsize, 0);
            if s.match_start >= wsize {
                s.match_start -= wsize;
            } else {
                s.match_start = 0;
            }
            s.strstart -= wsize;
            s.block_start -= wsize as i64;
            if s.insert > s.strstart {
                s.insert = s.strstart;
            }
            slide_hash(s);
            // more now includes the freed upper half
            let _more = more + wsize;
            // Continue with the expanded `more` value
            let more = _more;
            if strm.avail_in == 0 {
                break;
            }
            // Read into window at strstart + lookahead
            let start = s.strstart + s.lookahead;
            let n = read_buf(strm, s.wrap, &mut s.window[start..start + more], more);
            s.lookahead += n;
        } else {
            if strm.avail_in == 0 {
                break;
            }
            // Read directly
            let start = s.strstart + s.lookahead;
            let n = read_buf(strm, s.wrap, &mut s.window[start..start + more], more);
            s.lookahead += n;
        }

        // Initialize the hash value now that we have some input
        if s.lookahead + s.insert >= MIN_MATCH {
            let mut str_pos = s.strstart.saturating_sub(s.insert);
            s.ins_h = s.window[str_pos] as u32;
            if str_pos + 1 < s.window.len() {
                update_hash(s, s.window[str_pos + 1]);
            }
            while s.insert > 0 {
                if str_pos + MIN_MATCH - 1 < s.window.len() {
                    update_hash(s, s.window[str_pos + MIN_MATCH - 1]);
                    s.prev[str_pos & s.w_mask] = s.head[s.ins_h as usize];
                    s.head[s.ins_h as usize] = str_pos as u16;
                }
                str_pos += 1;
                s.insert -= 1;
                if s.lookahead + s.insert < MIN_MATCH {
                    break;
                }
            }
        }

        // Check loop condition
        if s.lookahead >= MIN_LOOKAHEAD || strm.avail_in == 0 {
            break;
        }
    }

    // High water mark: zero WIN_INIT bytes past current data to avoid
    // uninitialized memory reads by longest_match.
    if s.high_water < s.window_size {
        let curr = s.strstart + s.lookahead;

        if s.high_water < curr {
            let init = min(s.window_size - curr, WIN_INIT);
            let start = curr;
            let end = start + init;
            if end <= s.window.len() {
                s.window[start..end].fill(0);
            }
            s.high_water = curr + init;
        } else if s.high_water < curr + WIN_INIT {
            let init = min(curr + WIN_INIT - s.high_water, s.window_size - s.high_water);
            let start = s.high_water;
            let end = start + init;
            if end <= s.window.len() {
                s.window[start..end].fill(0);
            }
            s.high_water += init;
        }
    }
}

/// Finds the longest match starting at the current string position.
///
/// Traverses the hash chain from `cur_match` backwards through older
/// positions, comparing bytes to find the best (longest) match.
///
/// Port of C `longest_match` (deflate.c:1389-1530).
///
/// # Parameters
///
/// - `s` — the deflate state with window, hash chains, and match parameters.
/// - `cur_match` — head of the hash chain for the current hash value.
///
/// # Returns
///
/// The length of the best match found, clamped to `lookahead`.
pub fn longest_match(s: &mut DeflateState, cur_match: u16) -> usize {
    let mut chain_length = s.max_chain_length;
    let scan = s.strstart;
    let mut best_len = s.prev_length;
    let mut nice_match = s.nice_match as usize;
    let limit: usize = if s.strstart > max_dist(s) {
        s.strstart - max_dist(s)
    } else {
        0 // NIL equivalent — prevents matches at position 0
    };
    let wmask = s.w_mask;

    // Do not waste too much time if we already have a good match
    if s.prev_length >= s.good_match as usize {
        chain_length >>= 2;
    }

    // Do not look for matches beyond the end of the input
    if nice_match > s.lookahead {
        nice_match = s.lookahead;
    }

    let strend = s.strstart + MAX_MATCH;
    let mut scan_end1 = if best_len > 0 && scan + best_len - 1 < s.window.len() {
        s.window[scan + best_len - 1]
    } else {
        0
    };
    let mut scan_end = if scan + best_len < s.window.len() {
        s.window[scan + best_len]
    } else {
        0
    };

    let mut cur = cur_match as usize;

    loop {
        let match_pos = cur;

        // Guard: match_pos must be in valid window range
        if match_pos + best_len >= s.window.len() || match_pos >= s.window.len() {
            break;
        }

        // Skip to next match if the match length cannot increase
        // or if the match length is less than 2
        if s.window[match_pos + best_len] != scan_end
            || s.window[match_pos + best_len.saturating_sub(1)] != scan_end1
            || s.window[match_pos] != s.window[scan]
        {
            // Follow chain
            cur = s.prev[cur & wmask] as usize;
            chain_length -= 1;
            if cur <= limit || chain_length == 0 {
                break;
            }
            continue;
        }

        // First byte matches; check second byte
        if match_pos + 1 >= s.window.len() || s.window[match_pos + 1] != s.window[scan + 1] {
            cur = s.prev[cur & wmask] as usize;
            chain_length -= 1;
            if cur <= limit || chain_length == 0 {
                break;
            }
            continue;
        }

        // Hash guarantees first 2 bytes match; compare from position 2 onwards
        let mut len = 2;
        while scan + len < strend
            && match_pos + len < s.window.len()
            && len < MAX_MATCH
            && s.window[scan + len] == s.window[match_pos + len]
        {
            len += 1;
        }

        if len > best_len {
            s.match_start = cur;
            best_len = len;
            if len >= nice_match {
                break;
            }
            // Update scan_end bytes for next iteration
            if scan + best_len < s.window.len() {
                scan_end = s.window[scan + best_len];
            }
            if best_len > 0 && scan + best_len - 1 < s.window.len() {
                scan_end1 = s.window[scan + best_len - 1];
            }
        }

        // Follow hash chain
        cur = s.prev[cur & wmask] as usize;
        chain_length -= 1;
        if cur <= limit || chain_length == 0 {
            break;
        }
    }

    min(best_len, s.lookahead)
}

/// Flushes pending output from `pending_buf` to the stream output buffer.
///
/// All deflate output (except some deflate_stored output) goes through
/// this function.
///
/// Port of C `flush_pending` (deflate.c:950-968).
pub fn flush_pending(strm: &mut ZStream) {
    let s = match strm.state.as_deflate_mut() {
        Some(s) => s as *mut DeflateState,
        None => return,
    };
    // SAFETY: We need split access to strm and state. The pointer is valid
    // for the duration of this function and we don't create aliasing refs.
    let s = unsafe { &mut *s };
    flush_pending_from_state(s, strm);
}

/// Internal version of `flush_pending` that takes state and stream separately.
///
/// This avoids borrowing issues when the state is already borrowed from the
/// stream. Used by helper functions like `flush_block_only` and by the
/// strategy-level `flush_block` helpers in `fast.rs`, `slow.rs`, `huff.rs`,
/// `rle.rs`, and `stored.rs` to drain the pending buffer to the output
/// between block emissions — matching C zlib's `FLUSH_BLOCK_ONLY` macro
/// which calls `flush_pending(s->strm)` after each `_tr_flush_block`.
pub(crate) fn flush_pending_from_state(s: &mut DeflateState, strm: &mut ZStream) {
    tr_flush_bits(s);
    let len = min(s.pending, strm.avail_out as usize);
    if len == 0 {
        return;
    }

    // Copy from pending_buf to next_out
    let src = &s.pending_buf[s.pending_out..s.pending_out + len];
    // SAFETY: next_out is a valid writable pointer maintained by set_output()
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), strm.next_out, len);
        strm.next_out = strm.next_out.add(len);
    }
    s.pending_out += len;
    strm.total_out += len as u64;
    strm.avail_out -= len as u32;
    s.pending -= len;
    if s.pending == 0 {
        s.pending_out = 0;
    }
}

// ============================================================
// Centralized safe wrappers for raw ZStream pointer access
//
// Strategy functions (stored.rs, fast.rs, slow.rs, huff.rs,
// rle.rs) receive a `*mut ZStream` raw pointer from `deflate()`
// to work around Rust's split-borrow limitation (DeflateState is
// owned by ZStream, so we cannot hold `&mut DeflateState` and
// `&mut ZStream` simultaneously). These wrappers centralize all
// unsafe pointer dereferences into this module, per AAP §0.7.2's
// goal of concentrating unsafe code in as few files as possible.
//
// Each wrapper:
//   1. Debug-asserts the pointer is non-null.
//   2. Contains a single `unsafe` block with a `// SAFETY:` comment.
//   3. Is `#[inline(always)]` to avoid call overhead in hot loops.
// ============================================================

/// Reads `avail_in` from a raw `ZStream` pointer.
///
/// Used by strategy functions to check how much input remains without
/// creating an `unsafe` block at each call site.
#[inline(always)]
pub(crate) fn strm_avail_in(strm: *const ZStream) -> u32 {
    debug_assert!(!strm.is_null(), "strm must be non-null");
    // SAFETY: `strm` is the raw pointer to the parent `ZStream` passed from
    // `deflate()`. The pointer is valid and non-null for the entire duration
    // of the `deflate()` call. The read-only dereference of the `avail_in`
    // field has no aliasing concerns because `DeflateState` (the only other
    // live mutable reference) does not contain `avail_in`.
    unsafe { (*strm).avail_in }
}

/// Reads `avail_out` from a raw `ZStream` pointer.
///
/// Used by strategy functions to check if the output buffer is exhausted
/// after flushing a block (the C `FLUSH_BLOCK` macro's `avail_out == 0`
/// check).
#[inline(always)]
pub(crate) fn strm_avail_out(strm: *const ZStream) -> u32 {
    debug_assert!(!strm.is_null(), "strm must be non-null");
    // SAFETY: Same guarantees as `strm_avail_in` — `strm` is valid for the
    // entire `deflate()` call. `avail_out` is a non-overlapping field.
    unsafe { (*strm).avail_out }
}

/// Fills the sliding window, accepting a raw `ZStream` pointer.
///
/// This is the safe-API entry point for strategy functions to call
/// [`fill_window`] without needing their own `unsafe` block.
#[inline(always)]
pub(crate) fn fill_window_via_ptr(s: &mut DeflateState, strm: *mut ZStream) {
    debug_assert!(!strm.is_null(), "strm must be non-null");
    // SAFETY: `strm` is the raw pointer to the parent `ZStream` passed from
    // `deflate()`. It is valid for the entire call. `fill_window` only
    // reads/writes non-overlapping `ZStream` fields (`avail_in`, `next_in`,
    // `total_in`, `adler`) that are distinct from the `DeflateState` fields
    // accessed via `s`.
    fill_window(s, unsafe { &mut *strm });
}

/// Flushes pending output bytes, accepting a raw `ZStream` pointer.
///
/// This is the safe-API entry point for strategy functions to call
/// [`flush_pending_from_state`] without needing their own `unsafe` block.
#[inline(always)]
pub(crate) fn flush_pending_via_ptr(s: &mut DeflateState, strm: *mut ZStream) {
    debug_assert!(!strm.is_null(), "strm must be non-null");
    // SAFETY: `strm` is the raw pointer to the parent `ZStream` passed from
    // `deflate()`. It is valid for the entire call. `flush_pending_from_state`
    // only reads/writes non-overlapping `ZStream` fields (`avail_out`,
    // `next_out`, `total_out`) that are distinct from the `DeflateState`
    // fields accessed via `s`.
    flush_pending_from_state(s, unsafe { &mut *strm });
}

/// Reads input directly into the window, accepting a raw `ZStream` pointer.
///
/// This is the safe-API entry point for `deflate_stored`'s fallback path
/// to call [`fill_window_read`] without needing its own `unsafe` block.
#[inline(always)]
pub(crate) fn fill_window_read_via_ptr(
    s: &mut DeflateState,
    strm: *mut ZStream,
    to_read: usize,
) {
    debug_assert!(!strm.is_null(), "strm must be non-null");
    // SAFETY: `strm` is the raw pointer to the parent `ZStream` passed from
    // `deflate()`. It is valid for the entire call. `fill_window_read` only
    // reads/writes non-overlapping `ZStream` fields via `read_buf`.
    fill_window_read(s, unsafe { &mut *strm }, to_read);
}

/// Initializes the "longest match" routines for a new zlib stream.
///
/// Sets window_size, clears hash, loads configuration parameters
/// from `CONFIG_TABLE`, and resets all match state.
///
/// Port of C `lm_init` (deflate.c:682-701).
pub fn lm_init(s: &mut DeflateState) {
    s.window_size = 2 * s.w_size;

    clear_hash(s);

    // Set the default configuration parameters from the config table
    let config = &CONFIG_TABLE[s.level];
    s.max_lazy_match = config.max_lazy as u32;
    s.good_match = config.good_length as u32;
    s.nice_match = config.nice_length as i32;
    s.max_chain_length = config.max_chain as u32;

    s.strstart = 0;
    s.block_start = 0;
    s.lookahead = 0;
    s.insert = 0;
    s.match_length = MIN_MATCH - 1;
    s.prev_length = MIN_MATCH - 1;
    s.match_available = false;
    s.ins_h = 0;
}

// ============================================================================
// Public API functions
// ============================================================================

/// Initializes the deflate compression engine with default parameters.
///
/// This is a convenience wrapper around [`deflate_init2`] using:
/// - method = `Z_DEFLATED` (8)
/// - window_bits = `MAX_WBITS` (15) — zlib wrapper
/// - mem_level = `DEF_MEM_LEVEL` (8)
/// - strategy = `Z_DEFAULT_STRATEGY` (0)
///
/// Port of C `deflateInit_` (deflate.c:378-383).
///
/// # Parameters
///
/// - `strm` — uninitialised stream to set up for compression.
/// - `level` — compression level (0–9, or -1 for default).
///
/// # Returns
///
/// `Ok(ReturnCode::Ok)` on success, or `Err(ZlibError)` on failure.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::ZStream;
/// use zlib_rs::deflate::deflate_init;
///
/// let mut strm = ZStream::new();
/// let result = deflate_init(&mut strm, 6);
/// assert!(result.is_ok());
/// ```
pub fn deflate_init(strm: &mut ZStream, level: i32) -> Result<ReturnCode, ZlibError> {
    deflate_init2(
        strm,
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    )
}

/// Initializes the deflate engine with full control over all parameters.
///
/// Port of C `deflateInit2_` (deflate.c:387-533).
///
/// # Parameters
///
/// - `strm` — uninitialised stream to set up for compression.
/// - `level` — compression level: 0 (no compression) to 9 (best), or -1 for default (6).
/// - `method` — must be `Z_DEFLATED` (8).
/// - `window_bits` — log₂ of the window size (8–15), with overloading:
///   - 8–15: zlib format wrapper
///   - -8 to -15: raw DEFLATE (no wrapper)
///   - 24–31 (+16): gzip format wrapper
/// - `mem_level` — memory usage level (1–9). Higher = more memory, better compression.
/// - `strategy` — compression strategy (0–4).
///
/// # Returns
///
/// `Ok(ReturnCode::Ok)` on success, or `Err(ZlibError::StreamError)` for
/// invalid parameters, or `Err(ZlibError::MemError)` for allocation failure.
pub fn deflate_init2(
    strm: &mut ZStream,
    mut level: i32,
    method: i32,
    mut window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Result<ReturnCode, ZlibError> {
    // Clear error message
    strm.msg = None;

    // Normalize default compression level
    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }

    // Determine wrapping mode from window_bits
    let mut wrap: i32 = 1;
    if window_bits < 0 {
        // Raw DEFLATE — no wrapper
        wrap = 0;
        if window_bits < -15 {
            return Err(ZlibError::StreamError);
        }
        window_bits = -window_bits;
    }
    #[cfg(feature = "gzip")]
    {
        if window_bits > 15 {
            // Gzip wrapper
            wrap = 2;
            window_bits -= 16;
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        if window_bits > 15 {
            return Err(ZlibError::StreamError);
        }
    }

    // Validate parameters
    if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
        || method != Z_DEFLATED
        || !(8..=15).contains(&window_bits)
        || !(0..=9).contains(&level)
        || !(0..=Z_FIXED).contains(&strategy)
        || (window_bits == 8 && wrap != 1)
    {
        return Err(ZlibError::StreamError);
    }

    // Bug compatibility: window_bits 8 → 9 for zlib wrapper
    if window_bits == 8 {
        window_bits = 9;
    }

    // Create the deflate state with all buffers
    let s = DeflateState::new(
        window_bits as usize,
        mem_level as usize,
        level as usize,
        strategy,
        wrap,
    );

    // Store state in stream
    strm.state = crate::stream::StreamState::Deflate(Box::new(s));

    // Reset the stream to complete initialization
    deflate_reset(strm)
}

/// Resets the deflate stream state, keeping allocated buffers.
///
/// After reset, the stream can be used for a new compression session
/// without re-allocating internal buffers.
///
/// Port of C `deflateReset` (deflate.c:704-711).
pub fn deflate_reset(strm: &mut ZStream) -> Result<ReturnCode, ZlibError> {
    deflate_reset_keep(strm)?;
    let s = strm.state.as_deflate_mut().ok_or(ZlibError::StreamError)?;
    lm_init(s);
    Ok(ReturnCode::Ok)
}

/// Resets stream totals and status without clearing hash or match state.
///
/// This is the lower-level reset used by both `deflate_reset` and
/// `deflate_params`.
///
/// Port of C `deflateResetKeep` (deflate.c:644-677).
pub fn deflate_reset_keep(strm: &mut ZStream) -> Result<ReturnCode, ZlibError> {
    let s = strm.state.as_deflate_mut().ok_or(ZlibError::StreamError)?;

    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = None;
    strm.data_type = Z_UNKNOWN;

    s.pending = 0;
    s.pending_out = 0;

    if s.wrap < 0 {
        // Was made negative by deflate(..., Z_FINISH); undo it
        s.wrap = -s.wrap;
    }

    #[cfg(feature = "gzip")]
    {
        s.status = if s.wrap == 2 {
            DeflateStatus::Gzip
        } else {
            DeflateStatus::Init
        };
    }
    #[cfg(not(feature = "gzip"))]
    {
        s.status = DeflateStatus::Init;
    }

    #[cfg(feature = "gzip")]
    {
        strm.adler = if s.wrap == 2 {
            crc32(0, &[]) as u64
        } else {
            adler32(0, &[]) as u64
        };
    }
    #[cfg(not(feature = "gzip"))]
    {
        strm.adler = adler32(0, &[]) as u64;
    }

    s.last_flush = -2;

    tr_init(s);

    Ok(ReturnCode::Ok)
}

/// Main compression function — compresses as much data as possible.
///
/// The caller must provide input via `strm.set_input()` and output space
/// via `strm.set_output()` before each call. The `flush` parameter
/// controls flushing behavior.
///
/// Port of C `deflate` (deflate.c:981-1290).
///
/// # Parameters
///
/// - `strm` — the initialized compression stream.
/// - `flush` — flush mode (0=NO_FLUSH, 1=PARTIAL, 2=SYNC, 3=FULL,
///   4=FINISH, 5=BLOCK, 6=TREES).
///
/// # Returns
///
/// - `Ok(ReturnCode::Ok)` — progress was made, call again with more input/output.
/// - `Ok(ReturnCode::StreamEnd)` — compression is complete.
/// - `Err(ZlibError::StreamError)` — invalid parameters or state.
/// - `Err(ZlibError::BufError)` — no progress possible.
pub fn deflate(strm: &mut ZStream, flush: i32) -> Result<ReturnCode, ZlibError> {
    // Validate flush parameter
    if !(0..=Z_BLOCK).contains(&flush) {
        return Err(ZlibError::StreamError);
    }

    // Extract state pointer for split borrow
    let s_ptr: *mut DeflateState = match strm.state.as_deflate_mut() {
        Some(s) => s as *mut DeflateState,
        None => return Err(ZlibError::StreamError),
    };
    // SAFETY: We use raw pointer to avoid double-borrow of strm/state.
    // The pointer is valid for the lifetime of this function.
    let s = unsafe { &mut *s_ptr };

    // Validate stream state
    if strm.next_out.is_null()
        || (strm.avail_in != 0 && strm.next_in.is_null())
        || (s.status == DeflateStatus::Finish && flush != Z_FINISH)
    {
        strm.msg = Some("stream error".to_string());
        return Err(ZlibError::StreamError);
    }
    if strm.avail_out == 0 {
        strm.msg = Some("buffer error".to_string());
        return Err(ZlibError::BufError);
    }

    let old_flush = s.last_flush;
    s.last_flush = flush;

    // Flush as much pending output as possible
    if s.pending != 0 {
        flush_pending_from_state(s, strm);
        if strm.avail_out == 0 {
            s.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    } else if strm.avail_in == 0 && flush_rank(flush) <= flush_rank(old_flush) && flush != Z_FINISH
    {
        strm.msg = Some("buffer error".to_string());
        return Err(ZlibError::BufError);
    }

    // User must not provide more input after the first FINISH
    if s.status == DeflateStatus::Finish && strm.avail_in != 0 {
        strm.msg = Some("buffer error".to_string());
        return Err(ZlibError::BufError);
    }

    // === Write the header ===
    if s.status == DeflateStatus::Init && s.wrap == 0 {
        s.status = DeflateStatus::Busy;
    }

    if s.status == DeflateStatus::Init {
        // Zlib header (RFC 1950)
        let mut header: u32 = (Z_DEFLATED as u32 + ((s.w_bits as u32 - 8) << 4)) << 8;
        let level_flags: u32 = if s.strategy >= Z_HUFFMAN_ONLY || (s.level < 2) {
            0
        } else if s.level < 6 {
            1
        } else if s.level == 6 {
            2
        } else {
            3
        };
        header |= level_flags << 6;
        if s.strstart != 0 {
            header |= PRESET_DICT;
        }
        header += 31 - (header % 31);

        put_short_msb(s, header as u16);

        // Save the Adler-32 of the preset dictionary
        if s.strstart != 0 {
            put_short_msb(s, (strm.adler >> 16) as u16);
            put_short_msb(s, (strm.adler & 0xffff) as u16);
        }
        strm.adler = adler32(0, &[]) as u64;
        s.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer
        flush_pending_from_state(s, strm);
        if s.pending != 0 {
            s.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }

    // === Gzip header writing ===
    #[cfg(feature = "gzip")]
    {
        if s.status == DeflateStatus::Gzip {
            strm.adler = crc32(0, &[]) as u64;
            put_byte(s, 31);
            put_byte(s, 139);
            put_byte(s, 8);
            if s.gzhead.is_none() {
                put_byte(s, 0);
                put_byte(s, 0);
                put_byte(s, 0);
                put_byte(s, 0);
                put_byte(s, 0);
                put_byte(
                    s,
                    if s.level == 9 {
                        2
                    } else if s.strategy >= Z_HUFFMAN_ONLY || s.level < 2 {
                        4
                    } else {
                        0
                    },
                );
                put_byte(s, OS_CODE);
                s.status = DeflateStatus::Busy;
                flush_pending_from_state(s, strm);
                if s.pending != 0 {
                    s.last_flush = -1;
                    return Ok(ReturnCode::Ok);
                }
            } else {
                // Extract gz header fields to local variables before put_byte
                // (avoids borrow conflict: &s.gzhead + &mut s.pending_buf).
                let gz = s.gzhead.as_ref().unwrap();
                let gz_text = gz.text;
                let gz_hcrc = gz.hcrc;
                let gz_has_extra = gz.extra.is_some();
                let gz_extra_len = gz.extra.as_ref().map_or(0, |e| e.len());
                let gz_has_name = gz.name.is_some();
                let gz_has_comment = gz.comment.is_some();
                let gz_time = gz.time;
                let gz_os = gz.os;

                let flg: u8 = (if gz_text { 1u8 } else { 0 })
                    + (if gz_hcrc { 2 } else { 0 })
                    + (if gz_has_extra { 4 } else { 0 })
                    + (if gz_has_name { 8 } else { 0 })
                    + (if gz_has_comment { 16 } else { 0 });
                put_byte(s, flg);
                put_byte(s, (gz_time & 0xff) as u8);
                put_byte(s, ((gz_time >> 8) & 0xff) as u8);
                put_byte(s, ((gz_time >> 16) & 0xff) as u8);
                put_byte(s, ((gz_time >> 24) & 0xff) as u8);
                put_byte(
                    s,
                    if s.level == 9 {
                        2
                    } else if s.strategy >= Z_HUFFMAN_ONLY || s.level < 2 {
                        4
                    } else {
                        0
                    },
                );
                put_byte(s, (gz_os & 0xff) as u8);
                if gz_has_extra {
                    put_byte(s, (gz_extra_len & 0xff) as u8);
                    put_byte(s, ((gz_extra_len >> 8) & 0xff) as u8);
                }
                if gz_hcrc {
                    strm.adler = crc32_z(strm.adler as u32, &s.pending_buf[..s.pending]) as u64;
                }
                s.gzindex = 0;
                s.status = DeflateStatus::Extra;
            }
        }

        if s.status == DeflateStatus::Extra {
            if let Some(ref extra) = s.gzhead.as_ref().and_then(|g| g.extra.clone()) {
                let mut left = extra.len() - s.gzindex;
                while left > 0 {
                    let beg = s.pending;
                    let copy = min(left, s.pending_buf_size - s.pending);
                    s.pending_buf[s.pending..s.pending + copy]
                        .copy_from_slice(&extra[s.gzindex..s.gzindex + copy]);
                    s.pending += copy;
                    // Update CRC if hcrc
                    if s.gzhead.as_ref().is_some_and(|g| g.hcrc) && s.pending > beg {
                        strm.adler =
                            crc32_z(strm.adler as u32, &s.pending_buf[beg..s.pending]) as u64;
                    }
                    s.gzindex += copy;
                    left -= copy;
                    if s.pending == s.pending_buf_size {
                        flush_pending_from_state(s, strm);
                        if s.pending != 0 {
                            s.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                    }
                }
            }
            s.gzindex = 0;
            s.status = DeflateStatus::Name;
        }

        if s.status == DeflateStatus::Name {
            if let Some(ref name) = s.gzhead.as_ref().and_then(|g| g.name.clone()) {
                let name_bytes = name.as_bytes();
                loop {
                    if s.pending == s.pending_buf_size {
                        if s.gzhead.as_ref().is_some_and(|g| g.hcrc) {
                            // beg = 0 for continuation
                            strm.adler =
                                crc32_z(strm.adler as u32, &s.pending_buf[..s.pending]) as u64;
                        }
                        flush_pending_from_state(s, strm);
                        if s.pending != 0 {
                            s.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                    }
                    let val = if s.gzindex < name_bytes.len() {
                        name_bytes[s.gzindex]
                    } else {
                        0
                    };
                    put_byte(s, val);
                    s.gzindex += 1;
                    if val == 0 {
                        break;
                    }
                }
                if s.gzhead.as_ref().is_some_and(|g| g.hcrc) {
                    strm.adler = crc32_z(strm.adler as u32, &s.pending_buf[..s.pending]) as u64;
                }
                s.gzindex = 0;
            }
            s.status = DeflateStatus::Comment;
        }

        if s.status == DeflateStatus::Comment {
            if let Some(ref comment) = s.gzhead.as_ref().and_then(|g| g.comment.clone()) {
                let comment_bytes = comment.as_bytes();
                loop {
                    if s.pending == s.pending_buf_size {
                        if s.gzhead.as_ref().is_some_and(|g| g.hcrc) {
                            strm.adler =
                                crc32_z(strm.adler as u32, &s.pending_buf[..s.pending]) as u64;
                        }
                        flush_pending_from_state(s, strm);
                        if s.pending != 0 {
                            s.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                    }
                    let val = if s.gzindex < comment_bytes.len() {
                        comment_bytes[s.gzindex]
                    } else {
                        0
                    };
                    put_byte(s, val);
                    s.gzindex += 1;
                    if val == 0 {
                        break;
                    }
                }
                if s.gzhead.as_ref().is_some_and(|g| g.hcrc) {
                    strm.adler = crc32_z(strm.adler as u32, &s.pending_buf[..s.pending]) as u64;
                }
            }
            s.status = DeflateStatus::HCrc;
        }

        if s.status == DeflateStatus::HCrc {
            if s.gzhead.as_ref().is_some_and(|g| g.hcrc) {
                if s.pending + 2 > s.pending_buf_size {
                    flush_pending_from_state(s, strm);
                    if s.pending != 0 {
                        s.last_flush = -1;
                        return Ok(ReturnCode::Ok);
                    }
                }
                put_byte(s, (strm.adler & 0xff) as u8);
                put_byte(s, ((strm.adler >> 8) & 0xff) as u8);
                strm.adler = crc32(0, &[]) as u64;
            }
            s.status = DeflateStatus::Busy;
            flush_pending_from_state(s, strm);
            if s.pending != 0 {
                s.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    } // end #[cfg(feature = "gzip")]

    // === Strategy dispatch — the core compression loop ===
    //
    // We pass a raw pointer to `strm` so that strategy functions can call
    // `fill_window(state, &mut *strm)` to read input from the stream.
    // This mirrors C zlib's `internal_state.strm` back-pointer.
    //
    // SAFETY: `strm` is valid for the lifetime of this function and
    // strategy functions only access non-overlapping fields through it.
    let strm_ptr: *mut ZStream = strm as *mut ZStream;

    if strm.avail_in != 0
        || s.lookahead != 0
        || (flush != Z_NO_FLUSH && s.status != DeflateStatus::Finish)
    {
        let bstate: BlockState = if s.level == 0 {
            deflate_stored(s, strm_ptr, flush)
        } else if s.strategy == Z_HUFFMAN_ONLY {
            deflate_huff(s, strm_ptr, flush)
        } else if s.strategy == Z_RLE {
            deflate_rle(s, strm_ptr, flush)
        } else {
            match CONFIG_TABLE[s.level].strategy {
                CompressionStrategy::Stored => deflate_stored(s, strm_ptr, flush),
                CompressionStrategy::Fast => deflate_fast(s, strm_ptr, flush),
                CompressionStrategy::Slow => deflate_slow(s, strm_ptr, flush),
                CompressionStrategy::Huff => deflate_huff(s, strm_ptr, flush),
                CompressionStrategy::Rle => deflate_rle(s, strm_ptr, flush),
            }
        };

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            s.status = DeflateStatus::Finish;
        }

        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if strm.avail_out == 0 {
                s.last_flush = -1;
            }
            return Ok(ReturnCode::Ok);
        }

        if bstate == BlockState::BlockDone {
            if flush == Z_PARTIAL_FLUSH {
                tr_align(s);
            } else if flush != Z_BLOCK {
                // FULL_FLUSH or SYNC_FLUSH — empty stored block as sync marker
                tr_stored_block(s, &[], 0, false);
                if flush == Z_FULL_FLUSH {
                    clear_hash(s);
                    if s.lookahead == 0 {
                        s.strstart = 0;
                        s.block_start = 0;
                        s.insert = 0;
                    }
                }
            }
            flush_pending_from_state(s, strm);
            if strm.avail_out == 0 {
                s.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    }

    // === Trailer writing ===
    if flush != Z_FINISH {
        return Ok(ReturnCode::Ok);
    }

    // Flush any remaining pending output produced by the strategy
    // function. In C zlib, the strategy functions call flush_pending()
    // via the FLUSH_BLOCK_ONLY macro after each _tr_flush_block, so
    // pending is normally empty here. This belt-and-suspenders call
    // ensures the pending buffer is drained for raw DEFLATE mode
    // (wrap <= 0) which returns early below without writing a trailer.
    flush_pending_from_state(s, strm);

    if s.wrap <= 0 {
        // Raw DEFLATE: no trailer to write. Return StreamEnd only if
        // all pending data has been flushed to the output buffer. If
        // the output buffer was too small, return Ok so the caller
        // provides more output space and calls deflate() again.
        if s.pending != 0 {
            return Ok(ReturnCode::Ok);
        }
        return Ok(ReturnCode::StreamEnd);
    }

    // Write trailer
    #[cfg(feature = "gzip")]
    {
        if s.wrap == 2 {
            // Gzip trailer: CRC32 (4 LE bytes) + total_in (4 LE bytes)
            put_byte(s, (strm.adler & 0xff) as u8);
            put_byte(s, ((strm.adler >> 8) & 0xff) as u8);
            put_byte(s, ((strm.adler >> 16) & 0xff) as u8);
            put_byte(s, ((strm.adler >> 24) & 0xff) as u8);
            put_byte(s, (strm.total_in & 0xff) as u8);
            put_byte(s, ((strm.total_in >> 8) & 0xff) as u8);
            put_byte(s, ((strm.total_in >> 16) & 0xff) as u8);
            put_byte(s, ((strm.total_in >> 24) & 0xff) as u8);
        } else {
            // Zlib trailer: Adler-32 as two MSB shorts
            put_short_msb(s, (strm.adler >> 16) as u16);
            put_short_msb(s, (strm.adler & 0xffff) as u16);
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        // Zlib trailer: Adler-32 as two MSB shorts
        put_short_msb(s, (strm.adler >> 16) as u16);
        put_short_msb(s, (strm.adler & 0xffff) as u16);
    }

    flush_pending_from_state(s, strm);

    // Write the trailer only once
    if s.wrap > 0 {
        s.wrap = -s.wrap;
    }

    if s.pending != 0 {
        Ok(ReturnCode::Ok)
    } else {
        Ok(ReturnCode::StreamEnd)
    }
}

/// Releases all dynamically allocated resources for the compression stream.
///
/// Port of C `deflateEnd` (deflate.c:1293-1310).
///
/// # Returns
///
/// - `Ok(ReturnCode::Ok)` on success.
/// - `Err(ZlibError::DataError)` if the stream was freed prematurely
///   (status was BUSY_STATE — data not fully flushed).
/// - `Err(ZlibError::StreamError)` if the stream was not initialized.
pub fn deflate_end(strm: &mut ZStream) -> Result<ReturnCode, ZlibError> {
    let status = {
        let s = strm.state.as_deflate().ok_or(ZlibError::StreamError)?;
        s.status
    };

    // Drop the state — Rust's Drop handles all Vec deallocations
    strm.state = crate::stream::StreamState::None;

    if status == DeflateStatus::Busy {
        Err(ZlibError::DataError)
    } else {
        Ok(ReturnCode::Ok)
    }
}

/// Sets a preset dictionary for compression.
///
/// The dictionary consists of a sequence of strings (byte sequences) that
/// are likely to occur in the data to be compressed. The most recently
/// added strings should be the most common.
///
/// Port of C `deflateSetDictionary` (deflate.c:558-621).
pub fn deflate_set_dictionary(
    strm: &mut ZStream,
    dictionary: &[u8],
) -> Result<ReturnCode, ZlibError> {
    let s_ptr: *mut DeflateState = match strm.state.as_deflate_mut() {
        Some(s) => s as *mut DeflateState,
        None => return Err(ZlibError::StreamError),
    };
    // SAFETY: s_ptr is derived from a valid &mut DeflateState; used to avoid
    // double-borrow of strm/state. The pointer is valid for this function's lifetime.
    let s = unsafe { &mut *s_ptr };

    let wrap = s.wrap;
    if wrap == 2 || (wrap == 1 && s.status != DeflateStatus::Init) || s.lookahead != 0 {
        return Err(ZlibError::StreamError);
    }

    // When using zlib wrappers, compute Adler-32 for provided dictionary
    if wrap == 1 {
        strm.adler = adler32_z(strm.adler as u32, dictionary) as u64;
    }
    s.wrap = 0; // avoid computing checksum in read_buf

    let dict_length = dictionary.len();

    // If dictionary would fill window, just replace the history
    if dict_length >= s.w_size {
        if wrap == 0 {
            clear_hash(s);
            s.strstart = 0;
            s.block_start = 0;
            s.insert = 0;
        }
        // Use the tail of the dictionary
        let tail_start = dict_length - s.w_size;
        let tail = &dictionary[tail_start..];
        s.window[..s.w_size].copy_from_slice(tail);
        s.strstart = s.w_size;
        s.insert = s.w_size;
        // Initialize hash from window content
        s.ins_h = s.window[0] as u32;
        update_hash(s, s.window[1]);
        for i in 0..s.w_size - MIN_MATCH + 1 {
            update_hash(s, s.window[i + MIN_MATCH - 1]);
            s.prev[i & s.w_mask] = s.head[s.ins_h as usize];
            s.head[s.ins_h as usize] = i as u16;
        }
    } else {
        // Dictionary fits in window — copy directly
        s.window[..dict_length].copy_from_slice(dictionary);
        s.strstart = dict_length;
        s.insert = dict_length;
        // Initialize hash
        s.ins_h = s.window[0] as u32;
        if dict_length >= 2 {
            update_hash(s, s.window[1]);
        }
        for i in 0..dict_length.saturating_sub(MIN_MATCH - 1) {
            if i + MIN_MATCH - 1 < dict_length {
                update_hash(s, s.window[i + MIN_MATCH - 1]);
                s.prev[i & s.w_mask] = s.head[s.ins_h as usize];
                s.head[s.ins_h as usize] = i as u16;
            }
        }
    }

    s.block_start = s.strstart as i64;
    s.lookahead = 0;
    s.match_length = MIN_MATCH - 1;
    s.prev_length = MIN_MATCH - 1;
    s.match_available = false;
    s.wrap = wrap;

    Ok(ReturnCode::Ok)
}

/// Retrieves the current dictionary from the compression state.
///
/// Port of C `deflateGetDictionary` (deflate.c:625-641).
///
/// # Returns
///
/// `Ok((ReturnCode::Ok, dict_length))` where `dict_length` is the number
/// of bytes in the current dictionary.
pub fn deflate_get_dictionary(
    strm: &ZStream,
    dictionary: Option<&mut [u8]>,
) -> Result<(ReturnCode, usize), ZlibError> {
    let s = strm.state.as_deflate().ok_or(ZlibError::StreamError)?;
    let len = min(s.strstart + s.lookahead, s.w_size);

    if let Some(dict) = dictionary {
        if len > 0 {
            let start = s.strstart + s.lookahead - len;
            let copy_len = min(len, dict.len());
            dict[..copy_len].copy_from_slice(&s.window[start..start + copy_len]);
        }
    }

    Ok((ReturnCode::Ok, len))
}

/// Dynamically changes the compression level and strategy.
///
/// If the compression level or strategy changes and the stream has already
/// compressed data, the current block is flushed first.
///
/// Port of C `deflateParams` (deflate.c:774-816).
pub fn deflate_params(
    strm: &mut ZStream,
    mut level: i32,
    strategy: i32,
) -> Result<ReturnCode, ZlibError> {
    // Get mutable state via raw pointer for split borrow
    let s_ptr: *mut DeflateState = match strm.state.as_deflate_mut() {
        Some(s) => s as *mut DeflateState,
        None => return Err(ZlibError::StreamError),
    };
    // SAFETY: s_ptr is derived from a valid &mut DeflateState; used to avoid
    // double-borrow of strm/state. The pointer is valid for this function's lifetime.
    let s = unsafe { &mut *s_ptr };

    // Normalize default level
    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }

    // Validate
    if !(0..=9).contains(&level) || !(0..=Z_FIXED).contains(&strategy) {
        return Err(ZlibError::StreamError);
    }

    let old_strategy = CONFIG_TABLE[s.level].strategy;
    let new_strategy = CONFIG_TABLE[level as usize].strategy;

    if (strategy != s.strategy || old_strategy != new_strategy) && s.last_flush != -2 {
        // Flush the last buffer
        deflate(strm, Z_BLOCK)?;
        // SAFETY: s_ptr remains valid — deflate() borrows strm but does not
        // invalidate the DeflateState allocation. Re-acquiring after deflate
        // returns is necessary because the prior `s` borrow was consumed.
        let s = unsafe { &mut *s_ptr };
        if strm.avail_in != 0 || ((s.strstart as i64 - s.block_start) as usize + s.lookahead) != 0 {
            return Err(ZlibError::BufError);
        }
    }

    // SAFETY: s_ptr is still valid; re-acquiring mutable reference after the
    // conditional block above may have consumed the prior `s` borrow.
    let s = unsafe { &mut *s_ptr };
    if s.level != level as usize {
        if s.level == 0 && s.matches != 0 {
            if s.matches == 1 {
                slide_hash(s);
            } else {
                clear_hash(s);
            }
            s.matches = 0;
        }
        s.level = level as usize;
        s.max_lazy_match = CONFIG_TABLE[level as usize].max_lazy as u32;
        s.good_match = CONFIG_TABLE[level as usize].good_length as u32;
        s.nice_match = CONFIG_TABLE[level as usize].nice_length as i32;
        s.max_chain_length = CONFIG_TABLE[level as usize].max_chain as u32;
    }
    s.strategy = strategy;

    Ok(ReturnCode::Ok)
}

/// Fine-tunes the deflate compressor's internal parameters.
///
/// Port of C `deflateTune` (deflate.c:819-830).
pub fn deflate_tune(
    strm: &mut ZStream,
    good_length: u32,
    max_lazy: u32,
    nice_length: i32,
    max_chain: u32,
) -> Result<ReturnCode, ZlibError> {
    let s = strm.state.as_deflate_mut().ok_or(ZlibError::StreamError)?;
    s.good_match = good_length;
    s.max_lazy_match = max_lazy;
    s.nice_match = nice_length;
    s.max_chain_length = max_chain;
    Ok(ReturnCode::Ok)
}

/// Returns an upper bound on the compressed size after deflation.
///
/// The bound is valid for any single call to `deflate` with `Z_FINISH`
/// (one-shot compression) or for the total compressed output.
///
/// Port of C `deflateBound_z` (deflate.c:856-932).
///
/// # Critical Formula (AAP §0.8.1)
///
/// For default parameters (windowBits=15, memLevel=8):
/// `source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13`
pub fn deflate_bound(strm: &ZStream, source_len: usize) -> usize {
    // Upper bound for fixed blocks with 9-bit literals and length 255 (~13%)
    let fixedlen = source_len
        .checked_add(source_len >> 3)
        .and_then(|v| v.checked_add(source_len >> 8))
        .and_then(|v| v.checked_add(source_len >> 9))
        .and_then(|v| v.checked_add(4));
    let fixedlen = fixedlen.unwrap_or(usize::MAX);

    // Upper bound for stored blocks with length 127 (~4%)
    let storelen = source_len
        .checked_add(source_len >> 5)
        .and_then(|v| v.checked_add(source_len >> 7))
        .and_then(|v| v.checked_add(source_len >> 11))
        .and_then(|v| v.checked_add(7));
    let storelen = storelen.unwrap_or(usize::MAX);

    // If can't get parameters, return larger bound plus a wrapper
    let s = match strm.state.as_deflate() {
        Some(s) => s,
        None => {
            let bound = if fixedlen > storelen {
                fixedlen
            } else {
                storelen
            };
            return bound.checked_add(18).unwrap_or(usize::MAX);
        }
    };

    // Compute wrapper length
    let abs_wrap = if s.wrap < 0 { -s.wrap } else { s.wrap };
    let wraplen: usize = match abs_wrap {
        0 => 0, // raw deflate
        1 => {
            // zlib wrapper: 2 header + 4 checksum + optional 4 for dict
            6 + if s.strstart != 0 { 4 } else { 0 }
        }
        #[cfg(feature = "gzip")]
        2 => {
            // gzip wrapper: 10 header + 8 trailer
            let mut wl: usize = 18;
            if let Some(ref gz) = s.gzhead {
                if gz.extra.is_some() {
                    wl += 2 + gz.extra_len();
                }
                if let Some(ref name) = gz.name {
                    wl += name.len() + 1;
                }
                if let Some(ref comment) = gz.comment {
                    wl += comment.len() + 1;
                }
                if gz.hcrc {
                    wl += 2;
                }
            }
            wl
        }
        _ => 18,
    };

    // If non-default parameters, return conservative bound
    if s.w_bits != 15 || s.hash_bits != 8 + 7 {
        let bound = if s.w_bits <= s.hash_bits && s.level > 0 {
            fixedlen
        } else {
            storelen
        };
        return bound.checked_add(wraplen).unwrap_or(usize::MAX);
    }

    // Default settings: tight bound (~0.03% overhead)
    let bound = source_len
        .checked_add(source_len >> 12)
        .and_then(|v| v.checked_add(source_len >> 14))
        .and_then(|v| v.checked_add(source_len >> 25))
        .and_then(|v| v.checked_add(13))
        .and_then(|v| v.checked_sub(6))
        .and_then(|v| v.checked_add(wraplen));

    bound.unwrap_or(usize::MAX)
}

/// Returns the number of pending output bytes and bits.
///
/// Port of C `deflatePending` (deflate.c:722-734).
///
/// # Returns
///
/// `Ok((pending_bytes, pending_bits))` on success.
pub fn deflate_pending(strm: &ZStream) -> Result<(u32, i32), ZlibError> {
    let s = strm.state.as_deflate().ok_or(ZlibError::StreamError)?;
    let pending = s.pending as u32;
    Ok((pending, s.bi_valid))
}

/// Inserts bits in the deflate output stream before compression starts.
///
/// Port of C `deflatePrime` (deflate.c:745-771).
pub fn deflate_prime(
    strm: &mut ZStream,
    mut bits: i32,
    mut value: i32,
) -> Result<ReturnCode, ZlibError> {
    let s = strm.state.as_deflate_mut().ok_or(ZlibError::StreamError)?;

    if !(0..=16).contains(&bits) {
        return Err(ZlibError::BufError);
    }

    // Check that there is enough space in pending_buf
    let sym_buf_start = s.lit_bufsize;
    let pending_out = s.pending_out;
    if sym_buf_start < pending_out + ((BUF_SIZE + 7) >> 3) {
        return Err(ZlibError::BufError);
    }

    while bits > 0 {
        let put = min(BUF_SIZE as i32 - s.bi_valid, bits);
        s.bi_buf |= ((value & ((1 << put) - 1)) as u16) << s.bi_valid;
        s.bi_valid += put;
        tr_flush_bits(s);
        value >>= put;
        bits -= put;
    }

    Ok(ReturnCode::Ok)
}

/// Sets the gzip header information for gzip-wrapped compression.
///
/// Port of C `deflateSetHeader` (deflate.c:714-719).
/// Only available when gzip wrapping is in use.
#[cfg(feature = "gzip")]
pub fn deflate_set_header(strm: &mut ZStream, head: &GzHeader) -> Result<ReturnCode, ZlibError> {
    let s = strm.state.as_deflate_mut().ok_or(ZlibError::StreamError)?;
    if s.wrap != 2 {
        return Err(ZlibError::StreamError);
    }
    s.gzhead = Some(head.clone());
    Ok(ReturnCode::Ok)
}

/// Sets the gzip header — no-op when gzip feature is disabled.
#[cfg(not(feature = "gzip"))]
pub fn deflate_set_header(_strm: &mut ZStream, _head: &GzHeader) -> Result<ReturnCode, ZlibError> {
    Err(ZlibError::StreamError)
}

/// Copies the compression state from one stream to another.
///
/// Deep-copies all internal buffers so the destination stream is
/// completely independent of the source.
///
/// Port of C `deflateCopy` (deflate.c:1317-1377).
pub fn deflate_copy(dest: &mut ZStream, source: &ZStream) -> Result<ReturnCode, ZlibError> {
    let ss = source.state.as_deflate().ok_or(ZlibError::StreamError)?;

    // Copy stream-level fields
    dest.avail_in = source.avail_in;
    dest.next_in = source.next_in;
    dest.total_in = source.total_in;
    dest.next_out = source.next_out;
    dest.avail_out = source.avail_out;
    dest.total_out = source.total_out;
    dest.msg = source.msg.clone();
    dest.data_type = source.data_type;
    dest.adler = source.adler;

    // Deep-copy the deflate state
    let mut ds = DeflateState::new(
        ss.w_bits,
        // Recover mem_level from hash_bits: hash_bits = mem_level + 7
        ss.hash_bits.saturating_sub(7),
        ss.level,
        ss.strategy,
        ss.wrap,
    );

    // Copy all state fields
    ds.status = ss.status;
    ds.pending_out = ss.pending_out;
    ds.pending = ss.pending;
    ds.wrap = ss.wrap;
    ds.gzhead = ss.gzhead.clone();
    ds.gzindex = ss.gzindex;
    ds.method = ss.method;
    ds.last_flush = ss.last_flush;

    ds.window_size = ss.window_size;
    ds.block_start = ss.block_start;
    ds.match_length = ss.match_length;
    ds.prev_match = ss.prev_match;
    ds.match_available = ss.match_available;
    ds.strstart = ss.strstart;
    ds.match_start = ss.match_start;
    ds.lookahead = ss.lookahead;
    ds.prev_length = ss.prev_length;
    ds.max_chain_length = ss.max_chain_length;
    ds.max_lazy_match = ss.max_lazy_match;
    ds.level = ss.level;
    ds.strategy = ss.strategy;
    ds.good_match = ss.good_match;
    ds.nice_match = ss.nice_match;
    ds.ins_h = ss.ins_h;

    // Copy buffer contents
    let hw = ss.high_water;
    if hw > 0 && hw <= ds.window.len() && hw <= ss.window.len() {
        ds.window[..hw].copy_from_slice(&ss.window[..hw]);
    }
    // Copy prev table
    let prev_copy = if ss.slid || (ss.strstart > ss.insert && ss.strstart - ss.insert > ds.w_size) {
        ds.w_size
    } else {
        ss.strstart.saturating_sub(ss.insert)
    };
    let prev_copy = min(prev_copy, ds.prev.len());
    let prev_copy = min(prev_copy, ss.prev.len());
    ds.prev[..prev_copy].copy_from_slice(&ss.prev[..prev_copy]);
    // Copy head table
    let head_copy = min(ds.hash_size, ss.head.len());
    ds.head[..head_copy].copy_from_slice(&ss.head[..head_copy]);

    // Copy pending buffer content
    let pend_copy = min(ss.pending, ds.pending_buf.len());
    let pend_copy = min(pend_copy, ss.pending_buf.len());
    if ss.pending_out + pend_copy <= ss.pending_buf.len() {
        ds.pending_buf[ds.pending_out..ds.pending_out + pend_copy]
            .copy_from_slice(&ss.pending_buf[ss.pending_out..ss.pending_out + pend_copy]);
    }

    // Copy symbol buffer
    let sym_copy = min(ss.sym_next, ds.sym_buf.len());
    let sym_copy = min(sym_copy, ss.sym_buf.len());
    ds.sym_buf[..sym_copy].copy_from_slice(&ss.sym_buf[..sym_copy]);
    ds.sym_next = ss.sym_next;

    // Copy tree data
    ds.dyn_ltree = ss.dyn_ltree;
    ds.dyn_dtree = ss.dyn_dtree;
    ds.bl_tree = ss.bl_tree;
    ds.l_desc.max_code = ss.l_desc.max_code;
    ds.l_desc.stat_desc = ss.l_desc.stat_desc;
    ds.d_desc.max_code = ss.d_desc.max_code;
    ds.d_desc.stat_desc = ss.d_desc.stat_desc;
    ds.bl_desc.max_code = ss.bl_desc.max_code;
    ds.bl_desc.stat_desc = ss.bl_desc.stat_desc;
    ds.bl_count = ss.bl_count;
    ds.heap = ss.heap;
    ds.heap_len = ss.heap_len;
    ds.heap_max = ss.heap_max;
    ds.depth = ss.depth;

    // Copy remaining statistics
    ds.opt_len = ss.opt_len;
    ds.static_len = ss.static_len;
    ds.matches = ss.matches;
    ds.insert = ss.insert;
    ds.bi_buf = ss.bi_buf;
    ds.bi_valid = ss.bi_valid;
    ds.bi_used = ss.bi_used;
    ds.high_water = ss.high_water;
    ds.slid = ss.slid;

    dest.state = crate::stream::StreamState::Deflate(Box::new(ds));

    Ok(ReturnCode::Ok)
}
