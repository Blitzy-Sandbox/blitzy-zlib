// DEFLATE compression engine — module root and public API.
//
// This is the module root for the DEFLATE compression engine, porting the
// public deflate API from `deflate.c` (2185 lines) of the zlib 1.3.2.1-motley
// C library.
//
// The module declares five submodules containing the internal engine
// components and re-exports key types for ergonomic access.  It implements
// all public deflate functions.
//
// # Safety
//
// This module contains **zero** `unsafe` blocks.  All buffer management uses
// safe Rust indexing and owned collections.

// ─── Submodule Declarations ──────────────────────────────────────────────────

/// Internal deflate engine state and supporting types.
pub mod state;

/// Five DEFLATE compression strategy functions.
pub mod algorithm;

/// Huffman tree construction and block encoding.
pub mod trees;

/// Hash chain management, window filling, and longest-match search.
pub mod hash;

/// Compression configuration table mapping levels to tuning parameters.
pub mod params;

// ─── Re-exports ──────────────────────────────────────────────────────────────

pub use self::params::CompressionConfig;
pub use self::state::{DeflateState, DeflateStatus};

// ─── Imports ─────────────────────────────────────────────────────────────────

use crate::checksum::adler32;
use crate::checksum::crc32;
use crate::constants::{
    DEF_MEM_LEVEL, MAX_MEM_LEVEL, MAX_WBITS, MIN_MATCH,
    PRESET_DICT, Z_BLOCK, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY,
    Z_DEFLATED, Z_FINISH, Z_FIXED, Z_FULL_FLUSH, Z_HUFFMAN_ONLY,
    Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_RLE, Z_UNKNOWN,
};
use crate::error::ReturnCode;
use crate::stream::{GzHeader, ZStream};
use crate::util;

use self::algorithm::{
    deflate_fast, deflate_huff, deflate_rle, deflate_slow, deflate_stored,
    BlockState,
};
use self::params::{rank, CompressionFunc, CONFIGURATION_TABLE};

// ─── Internal Helpers ────────────────────────────────────────────────────────

/// Validate that a `ZStream` has a properly initialized deflate state.
///
/// Returns `true` if the state is **invalid** (check fails), `false` if OK.
fn deflate_state_check(stream: &ZStream) -> bool {
    let Some(ref state) = stream.state else {
        return true;
    };
    let Some(s) = state.downcast_ref::<DeflateState>() else {
        return true;
    };
    // All valid status variants mean the state is OK.
    match s.status {
        DeflateStatus::Init
        | DeflateStatus::GzipHeader
        | DeflateStatus::Extra
        | DeflateStatus::Name
        | DeflateStatus::Comment
        | DeflateStatus::Hcrc
        | DeflateStatus::Busy
        | DeflateStatus::Finish => false,
    }
}

/// Get a shared reference to the [`DeflateState`] stored inside a `ZStream`.
fn get_state(stream: &ZStream) -> Option<&DeflateState> {
    stream
        .state
        .as_ref()
        .and_then(|s| s.downcast_ref::<DeflateState>())
}

/// Get a mutable reference to the [`DeflateState`] stored inside a `ZStream`.
fn get_state_mut(stream: &mut ZStream) -> Option<&mut DeflateState> {
    stream
        .state
        .as_mut()
        .and_then(|s| s.downcast_mut::<DeflateState>())
}

/// Put a single byte into the pending buffer.
#[inline]
fn put_byte(state: &mut DeflateState, c: u8) {
    if state.pending < state.pending_buf.len() {
        state.pending_buf[state.pending] = c;
        state.pending += 1;
    }
}

/// Put a 16-bit value in MSB (big-endian) order into the pending buffer.
#[inline]
fn put_short_msb(state: &mut DeflateState, b: u32) {
    #[allow(clippy::cast_possible_truncation)]
    put_byte(state, ((b >> 8) & 0xff) as u8);
    #[allow(clippy::cast_possible_truncation)]
    put_byte(state, (b & 0xff) as u8);
}

/// Flush as much pending output as possible.
///
/// This version works when state has already been taken out of `stream`.
fn flush_pending_inner(state: &mut DeflateState, stream: &mut ZStream) {
    trees::tr_flush_bits(state);

    let len = state.pending.min(stream.avail_out());
    if len == 0 {
        return;
    }

    let out = stream.output_remaining_mut();
    out[..len].copy_from_slice(&state.pending_buf[..len]);
    let _ = stream.advance_output(len);

    // Shift remaining pending data to the front of the buffer.
    if len < state.pending {
        state.pending_buf.copy_within(len..state.pending, 0);
    }
    state.pending -= len;
}

/// Update the header CRC with the bytes `pending_buf[beg..pending]`.
fn hcrc_update(state: &DeflateState, stream_adler: u32, beg: usize) -> u32 {
    if let Some(ref gzhead) = state.gzip_header {
        if gzhead.hcrc && state.pending > beg {
            return crc32::crc32_z(
                stream_adler,
                &state.pending_buf[beg..state.pending],
            );
        }
    }
    stream_adler
}

/// Initialize the "longest match" routines for a new zlib stream.
fn lm_init(state: &mut DeflateState) {
    state.window_size = 2 * state.w_size;

    hash::clear_hash(state);

    #[allow(clippy::cast_sign_loss)]
    let level_idx = if (0..=9).contains(&state.level) {
        state.level as usize
    } else {
        0
    };
    let config = &CONFIGURATION_TABLE[level_idx];
    #[allow(clippy::cast_sign_loss)]
    {
        state.max_lazy_match = config.max_lazy as usize;
        state.good_match = config.good_length as usize;
        state.nice_match = config.nice_length as usize;
        state.max_chain_length = config.max_chain as usize;
    }

    state.strstart = 0;
    state.block_start = 0;
    state.lookahead = 0;
    state.insert = 0;
    state.match_length = MIN_MATCH - 1;
    state.prev_length = MIN_MATCH - 1;
    state.match_available = false;
    state.ins_h = 0;
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Initialize a deflate stream with default parameters.
///
/// Equivalent to calling [`deflate_init2`] with `method = Z_DEFLATED`,
/// `window_bits = MAX_WBITS`, `mem_level = DEF_MEM_LEVEL`,
/// `strategy = Z_DEFAULT_STRATEGY`.
pub fn deflate_init(stream: &mut ZStream, level: i32) -> ReturnCode {
    deflate_init2(
        stream,
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    )
}

/// Initialize a deflate stream with full control over all parameters.
///
/// # Parameters
///
/// - `stream` — The stream to initialize.
/// - `level` — Compression level (0–9, or `Z_DEFAULT_COMPRESSION`).
/// - `method` — Must be `Z_DEFLATED`.
/// - `window_bits` — Window size encoding:
///   - 8–15: zlib format
///   - −8 to −15: raw deflate
///   - 24–31 (8–15 + 16): gzip format
/// - `mem_level` — Memory level (1–9).
/// - `strategy` — Compression strategy.
pub fn deflate_init2(
    stream: &mut ZStream,
    mut level: i32,
    method: i32,
    mut window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> ReturnCode {
    stream.msg = None;

    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }

    // Determine wrap format from `windowBits` overloading.
    let mut wrap: i32 = 1;
    if window_bits < 0 {
        wrap = 0;
        if window_bits < -15 {
            return ReturnCode::StreamError;
        }
        window_bits = -window_bits;
    } else if window_bits > 15 {
        wrap = 2;
        window_bits -= 16;
    }

    // Parameter validation.
    if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
        || method != Z_DEFLATED
        || !(8..=15).contains(&window_bits)
        || !(0..=9).contains(&level)
        || !(0..=Z_FIXED).contains(&strategy)
        || (window_bits == 8 && wrap != 1)
    {
        return ReturnCode::StreamError;
    }

    // Promote `windowBits == 8` to 9 (historical bug workaround).
    if window_bits == 8 {
        window_bits = 9;
    }

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let state = DeflateState::new(
        window_bits as usize,
        mem_level as usize,
        method as u8,
        strategy,
        level,
        wrap,
    );

    stream.state = Some(Box::new(state));
    deflate_reset(stream)
}

/// Reset the deflate stream state, keeping allocated buffers.
pub fn deflate_reset_keep(stream: &mut ZStream) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }

    stream.total_in = 0;
    stream.total_out = 0;
    stream.msg = None;
    stream.data_type = Z_UNKNOWN;

    let Some(mut state_box) = stream.state.take() else {
        return ReturnCode::StreamError;
    };
    let Some(state) = state_box.downcast_mut::<DeflateState>() else {
        stream.state = Some(state_box);
        return ReturnCode::StreamError;
    };

    state.pending = 0;

    if state.wrap < 0 {
        state.wrap = -state.wrap;
    }

    state.status = if state.wrap == 2 {
        DeflateStatus::GzipHeader
    } else {
        DeflateStatus::Init
    };

    let adler_val = if state.wrap == 2 {
        crc32::crc32(0, &[])
    } else {
        adler32::adler32(0, &[])
    };
    stream.adler = adler_val;

    state.last_flush = -2;

    trees::tr_init(state);

    stream.state = Some(state_box);
    ReturnCode::Ok
}

/// Reset the deflate stream and re-initialize longest-match parameters.
pub fn deflate_reset(stream: &mut ZStream) -> ReturnCode {
    let ret = deflate_reset_keep(stream);
    if ret != ReturnCode::Ok {
        return ret;
    }

    let Some(mut state_box) = stream.state.take() else {
        return ReturnCode::StreamError;
    };
    if let Some(state) = state_box.downcast_mut::<DeflateState>() {
        lm_init(state);
    }
    stream.state = Some(state_box);
    ReturnCode::Ok
}

/// Set gzip header fields for a gzip-format stream.
pub fn deflate_set_header(stream: &mut ZStream, head: GzHeader) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }
    let Some(state) = get_state_mut(stream) else {
        return ReturnCode::StreamError;
    };
    if state.wrap != 2 {
        return ReturnCode::StreamError;
    }
    state.gzip_header = Some(head);
    ReturnCode::Ok
}

/// Query the number of pending bytes and bits in the deflate output buffer.
///
/// Returns `(pending_bytes, pending_bits)` on success.
///
/// # Errors
///
/// Returns [`ReturnCode::StreamError`] if the stream is not properly initialized.
/// Returns [`ReturnCode::BufError`] if the pending count overflows `u32`.
pub fn deflate_pending(stream: &ZStream) -> Result<(u32, i32), ReturnCode> {
    if deflate_state_check(stream) {
        return Err(ReturnCode::StreamError);
    }
    let state = get_state(stream).ok_or(ReturnCode::StreamError)?;

    #[allow(clippy::cast_possible_truncation)]
    let pending_u32 = state.pending as u32;
    if pending_u32 as usize != state.pending {
        return Err(ReturnCode::BufError);
    }

    Ok((pending_u32, state.bi_valid))
}

/// Query the number of used bits at the last byte boundary.
///
/// # Errors
///
/// Returns [`ReturnCode::StreamError`] if the stream is not properly initialized.
pub fn deflate_used(stream: &ZStream) -> Result<i32, ReturnCode> {
    if deflate_state_check(stream) {
        return Err(ReturnCode::StreamError);
    }
    let state = get_state(stream).ok_or(ReturnCode::StreamError)?;
    Ok(state.bi_used)
}

/// Insert bits in the deflate output stream.
pub fn deflate_prime(
    stream: &mut ZStream,
    mut bits: i32,
    mut value: i32,
) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }

    let Some(mut state_box) = stream.state.take() else {
        return ReturnCode::StreamError;
    };
    let Some(state) = state_box.downcast_mut::<DeflateState>() else {
        stream.state = Some(state_box);
        return ReturnCode::StreamError;
    };

    if !(0..=16).contains(&bits) || state.lit_bufsize < 2 {
        stream.state = Some(state_box);
        return ReturnCode::BufError;
    }

    while bits > 0 {
        let put = (16 - state.bi_valid).min(bits);
        if put <= 0 {
            break;
        }
        #[allow(clippy::cast_sign_loss)]
        let mask = (1_u32 << put as u32) - 1;
        #[allow(clippy::cast_sign_loss)]
        let shift = state.bi_valid as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            state.bi_buf |= (((value as u32) & mask) << shift) as u16;
        }
        state.bi_valid += put;
        trees::tr_flush_bits(state);
        value >>= put;
        bits -= put;
    }

    stream.state = Some(state_box);
    ReturnCode::Ok
}

/// Dynamically update the compression level and strategy.
pub fn deflate_params(
    stream: &mut ZStream,
    mut level: i32,
    strategy: i32,
) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }

    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }
    if !(0..=9).contains(&level) || !(0..=Z_FIXED).contains(&strategy) {
        return ReturnCode::StreamError;
    }

    let need_flush = {
        let Some(state) = get_state(stream) else {
            return ReturnCode::StreamError;
        };
        #[allow(clippy::cast_sign_loss)]
        let cur_func = CONFIGURATION_TABLE[state.level as usize].func;
        #[allow(clippy::cast_sign_loss)]
        let new_func = CONFIGURATION_TABLE[level as usize].func;
        (strategy != state.strategy || cur_func != new_func)
            && state.last_flush != -2
    };

    if need_flush {
        let err = deflate_inner(stream, Z_BLOCK);
        if err == ReturnCode::StreamError {
            return err;
        }
        let Some(state) = get_state(stream) else {
            return ReturnCode::StreamError;
        };
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_wrap,
            clippy::cast_possible_truncation
        )]
        let pending_input = (state.strstart as i64 - state.block_start) as usize
            + state.lookahead;
        if stream.avail_in() != 0 || pending_input != 0 {
            return ReturnCode::BufError;
        }
    }

    let Some(state) = get_state_mut(stream) else {
        return ReturnCode::StreamError;
    };

    if state.level != level {
        if state.level == 0 && state.matches != 0 {
            if state.matches == 1 {
                hash::slide_hash(state);
            } else {
                hash::clear_hash(state);
            }
            state.matches = 0;
        }
        state.level = level;
        #[allow(clippy::cast_sign_loss)]
        let cfg = &CONFIGURATION_TABLE[level as usize];
        #[allow(clippy::cast_sign_loss)]
        {
            state.max_lazy_match = cfg.max_lazy as usize;
            state.good_match = cfg.good_length as usize;
            state.nice_match = cfg.nice_length as usize;
            state.max_chain_length = cfg.max_chain as usize;
        }
    }
    state.strategy = strategy;
    ReturnCode::Ok
}

/// Fine-tune the deflate match-engine parameters.
pub fn deflate_tune(
    stream: &mut ZStream,
    good_length: i32,
    max_lazy: i32,
    nice_length: i32,
    max_chain: i32,
) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }
    let Some(state) = get_state_mut(stream) else {
        return ReturnCode::StreamError;
    };
    #[allow(clippy::cast_sign_loss)]
    {
        state.good_match = good_length as usize;
        state.max_lazy_match = max_lazy as usize;
        state.nice_match = nice_length as usize;
        state.max_chain_length = max_chain as usize;
    }
    ReturnCode::Ok
}

/// Compute an upper bound on the compressed output size.
#[must_use]
pub fn deflate_bound(stream: &ZStream, source_len: usize) -> usize {
    let fixedlen = source_len
        .saturating_add(source_len >> 3)
        .saturating_add(source_len >> 8)
        .saturating_add(source_len >> 9)
        .saturating_add(4);

    let storelen = source_len
        .saturating_add(source_len >> 5)
        .saturating_add(source_len >> 7)
        .saturating_add(source_len >> 11)
        .saturating_add(7);

    if deflate_state_check(stream) {
        return fixedlen.max(storelen).saturating_add(18);
    }

    let Some(state) = get_state(stream) else {
        return fixedlen.max(storelen).saturating_add(18);
    };

    #[allow(clippy::cast_possible_wrap)]
    let abs_wrap = state.wrap.unsigned_abs() as i32;
    let wraplen: usize = match abs_wrap {
        0 => 0,
        1 => 6 + if state.strstart != 0 { 4 } else { 0 },
        2 => {
            let mut wl: usize = 18;
            if let Some(ref gzhead) = state.gzip_header {
                if let Some(ref extra) = gzhead.extra {
                    wl += 2 + extra.len();
                }
                if let Some(ref name) = gzhead.name {
                    wl += name.len() + 1;
                }
                if let Some(ref comment) = gzhead.comment {
                    wl += comment.len() + 1;
                }
                if gzhead.hcrc {
                    wl += 2;
                }
            }
            wl
        }
        _ => 18,
    };

    if state.w_bits != 15 || state.hash_bits != 15 {
        let bound = if state.w_bits <= state.hash_bits && state.level != 0 {
            fixedlen
        } else {
            storelen
        };
        return bound.saturating_add(wraplen);
    }

    let bound = source_len
        .saturating_add(source_len >> 12)
        .saturating_add(source_len >> 14)
        .saturating_add(source_len >> 25)
        .saturating_add(13)
        .saturating_sub(6)
        .saturating_add(wraplen);
    if bound < source_len {
        usize::MAX
    } else {
        bound
    }
}

/// Set the preset dictionary for compression.
pub fn deflate_set_dictionary(
    stream: &mut ZStream,
    dictionary: &[u8],
) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }

    let (wrap, w_size) = {
        let Some(state) = get_state(stream) else {
            return ReturnCode::StreamError;
        };
        if state.wrap == 2
            || (state.wrap == 1 && state.status != DeflateStatus::Init)
            || state.lookahead != 0
        {
            return ReturnCode::StreamError;
        }
        (state.wrap, state.w_size)
    };

    // Compute Adler-32 of dictionary for zlib wrapper.
    if wrap == 1 {
        stream.adler = adler32::adler32(stream.adler, dictionary);
    }

    // Temporarily disable checksum computation in `read_buf`.
    if let Some(state) = get_state_mut(stream) {
        state.wrap = 0;
    }

    let dict_length = dictionary.len();
    let effective_dict = if dict_length >= w_size {
        if wrap == 0 {
            if let Some(state) = get_state_mut(stream) {
                hash::clear_hash(state);
                state.strstart = 0;
                state.block_start = 0;
                state.insert = 0;
            }
        }
        &dictionary[dict_length - w_size..]
    } else {
        dictionary
    };

    // Save current input, feed dictionary as input.
    let saved_input = stream.input_remaining().to_vec();
    let saved_avail = stream.avail_in();
    stream.set_input(effective_dict);

    // Take state out to work with `fill_window` / `insert_string`.
    let Some(mut state_box) = stream.state.take() else {
        return ReturnCode::StreamError;
    };
    let Some(state) = state_box.downcast_mut::<DeflateState>() else {
        stream.state = Some(state_box);
        return ReturnCode::StreamError;
    };

    hash::fill_window(state, stream);

    while state.lookahead >= MIN_MATCH {
        let mut str_pos = state.strstart;
        let mut n = state.lookahead - (MIN_MATCH - 1);
        while n > 0 {
            let _ = hash::insert_string(state, str_pos);
            str_pos += 1;
            n -= 1;
        }
        state.strstart = str_pos;
        state.lookahead = MIN_MATCH - 1;
        hash::fill_window(state, stream);
    }

    state.strstart += state.lookahead;
    #[allow(clippy::cast_possible_wrap)]
    {
        state.block_start = state.strstart as i64;
    }
    state.insert = state.lookahead;
    state.lookahead = 0;
    state.match_length = MIN_MATCH - 1;
    state.prev_length = MIN_MATCH - 1;
    state.match_available = false;

    // Restore wrap and input.
    state.wrap = wrap;
    stream.state = Some(state_box);

    if saved_avail <= saved_input.len() {
        stream.set_input(&saved_input[saved_input.len() - saved_avail..]);
    } else {
        stream.set_input(&[]);
    }

    ReturnCode::Ok
}

/// Retrieve the current dictionary from the deflate state.
///
/// Returns the number of bytes written to `dictionary`.
///
/// # Errors
///
/// Returns [`ReturnCode::StreamError`] if the stream is not properly initialized.
pub fn deflate_get_dictionary(
    stream: &ZStream,
    dictionary: &mut [u8],
) -> Result<usize, ReturnCode> {
    if deflate_state_check(stream) {
        return Err(ReturnCode::StreamError);
    }
    let state = get_state(stream).ok_or(ReturnCode::StreamError)?;

    let mut len = state.strstart + state.lookahead;
    if len > state.w_size {
        len = state.w_size;
    }

    if len > 0 && !dictionary.is_empty() {
        let copy_len = len.min(dictionary.len());
        let start = state.strstart + state.lookahead - len;
        let end = start + copy_len;
        if end <= state.window.len() {
            dictionary[..copy_len]
                .copy_from_slice(&state.window[start..end]);
        }
    }

    Ok(len)
}

/// Copy the deflate stream state from source to dest.
pub fn deflate_copy(dest: &mut ZStream, source: &ZStream) -> ReturnCode {
    if deflate_state_check(source) {
        return ReturnCode::StreamError;
    }

    let Some(source_state) = get_state(source) else {
        return ReturnCode::StreamError;
    };

    let new_state = source_state.clone();

    dest.total_in = source.total_in;
    dest.total_out = source.total_out;
    dest.adler = source.adler;
    dest.data_type = source.data_type;
    dest.msg = source.msg;

    dest.state = Some(Box::new(new_state));
    ReturnCode::Ok
}

/// End the deflate stream and free all resources.
pub fn deflate_end(stream: &mut ZStream) -> ReturnCode {
    if deflate_state_check(stream) {
        return ReturnCode::StreamError;
    }

    let status = {
        let Some(state) = get_state(stream) else {
            return ReturnCode::StreamError;
        };
        state.status
    };

    // Drop the state (RAII cleanup).
    stream.state = None;

    if status == DeflateStatus::Busy {
        ReturnCode::DataError
    } else {
        ReturnCode::Ok
    }
}

/// The main deflate compression function.
///
/// Port of `deflate()` from `deflate.c` lines 981–1290.
pub fn deflate(stream: &mut ZStream, flush: i32) -> ReturnCode {
    deflate_inner(stream, flush)
}

// ─── Main Compression Implementation ─────────────────────────────────────────

/// Inner deflate implementation.  Separated from `deflate` to allow
/// `deflate_params` to call `deflate(Z_BLOCK)` without name collision.
fn deflate_inner(stream: &mut ZStream, flush: i32) -> ReturnCode {
    // ── Validate ──
    if deflate_state_check(stream)
        || !(0..=Z_BLOCK).contains(&flush)
    {
        return ReturnCode::StreamError;
    }
    {
        let Some(state) = get_state(stream) else {
            return ReturnCode::StreamError;
        };
        if stream.avail_out() == 0 {
            return ReturnCode::BufError;
        }
        if state.status == DeflateStatus::Finish && flush != Z_FINISH {
            return ReturnCode::BufError;
        }
    }

    // ── Take state out for the remainder of this function ──
    let Some(mut state_box) = stream.state.take() else {
        return ReturnCode::StreamError;
    };
    let Some(state) = state_box.downcast_mut::<DeflateState>() else {
        stream.state = Some(state_box);
        return ReturnCode::StreamError;
    };

    let old_flush = state.last_flush;
    state.last_flush = flush;

    // ── Flush existing pending output ──
    if state.pending != 0 {
        flush_pending_inner(state, stream);
        if stream.avail_out() == 0 {
            state.last_flush = -1;
            stream.state = Some(state_box);
            return ReturnCode::Ok;
        }
    } else if stream.avail_in() == 0
        && rank(flush) <= rank(old_flush)
        && flush != Z_FINISH
    {
        stream.state = Some(state_box);
        return ReturnCode::BufError;
    }

    // User must not provide more input after the first FINISH.
    if state.status == DeflateStatus::Finish && stream.avail_in() != 0 {
        stream.state = Some(state_box);
        return ReturnCode::BufError;
    }

    // ── Write the header ──
    if let Some(code) = write_header_phase(state, stream) {
        stream.state = Some(state_box);
        return code;
    }

    // ── Compression dispatch ──
    if stream.avail_in() != 0
        || state.lookahead != 0
        || (flush != Z_NO_FLUSH && state.status != DeflateStatus::Finish)
    {
        #[allow(clippy::cast_sign_loss)]
        let bstate = if state.level == 0 {
            deflate_stored(state, stream, flush)
        } else if state.strategy == Z_HUFFMAN_ONLY {
            deflate_huff(state, stream, flush)
        } else if state.strategy == Z_RLE {
            deflate_rle(state, stream, flush)
        } else {
            let level_idx = state.level as usize;
            let func = if level_idx < CONFIGURATION_TABLE.len() {
                CONFIGURATION_TABLE[level_idx].func
            } else {
                CompressionFunc::Slow
            };
            match func {
                CompressionFunc::Stored => deflate_stored(state, stream, flush),
                CompressionFunc::Fast => deflate_fast(state, stream, flush),
                CompressionFunc::Slow => deflate_slow(state, stream, flush),
            }
        };

        if bstate == BlockState::FinishStarted
            || bstate == BlockState::FinishDone
        {
            state.status = DeflateStatus::Finish;
        }

        if bstate == BlockState::NeedMore
            || bstate == BlockState::FinishStarted
        {
            if stream.avail_out() == 0 {
                state.last_flush = -1;
            }
            stream.state = Some(state_box);
            return ReturnCode::Ok;
        }

        if bstate == BlockState::BlockDone {
            if flush == Z_PARTIAL_FLUSH {
                trees::tr_align(state);
            } else if flush != Z_BLOCK {
                trees::tr_stored_block(state, None, 0, false);
                if flush == Z_FULL_FLUSH {
                    hash::clear_hash(state);
                    if state.lookahead == 0 {
                        state.strstart = 0;
                        state.block_start = 0;
                        state.insert = 0;
                    }
                }
            }
            flush_pending_inner(state, stream);
            if stream.avail_out() == 0 {
                state.last_flush = -1;
                stream.state = Some(state_box);
                return ReturnCode::Ok;
            }
        }
    }

    // ── Check if we should write the trailer ──
    if flush != Z_FINISH {
        stream.state = Some(state_box);
        return ReturnCode::Ok;
    }
    if state.wrap <= 0 {
        stream.state = Some(state_box);
        return ReturnCode::StreamEnd;
    }

    // ── Write trailer ──
    write_trailer_impl(state, stream);
    flush_pending_inner(state, stream);

    if state.wrap > 0 {
        state.wrap = -state.wrap;
    }

    let result = if state.pending != 0 {
        ReturnCode::Ok
    } else {
        ReturnCode::StreamEnd
    };

    stream.state = Some(state_box);
    result
}

// ─── Header Writing ──────────────────────────────────────────────────────────

/// Write the stream header (zlib or gzip) across all sub-states.
///
/// Returns `Some(ReturnCode)` if the caller should return early,
/// `None` if header writing is complete and the caller can proceed to
/// the compression dispatch.
fn write_header_phase(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    // Raw deflate with no wrapper: skip directly to Busy.
    if state.status == DeflateStatus::Init && state.wrap == 0 {
        state.status = DeflateStatus::Busy;
    }

    // ── INIT_STATE → zlib header ──
    if state.status == DeflateStatus::Init {
        return write_zlib_header_impl(state, stream);
    }

    // ── GZIP_STATE → gzip header ──
    if state.status == DeflateStatus::GzipHeader {
        if let Some(code) = write_gzip_header_impl(state, stream) {
            return Some(code);
        }
    }

    // ── EXTRA_STATE → gzip extra field ──
    if state.status == DeflateStatus::Extra {
        if let Some(code) = write_gzip_extra_impl(state, stream) {
            return Some(code);
        }
    }

    // ── NAME_STATE → gzip file name ──
    if state.status == DeflateStatus::Name {
        if let Some(code) = write_gzip_name_impl(state, stream) {
            return Some(code);
        }
    }

    // ── COMMENT_STATE → gzip comment ──
    if state.status == DeflateStatus::Comment {
        if let Some(code) = write_gzip_comment_impl(state, stream) {
            return Some(code);
        }
    }

    // ── HCRC_STATE → gzip header CRC ──
    if state.status == DeflateStatus::Hcrc {
        return write_gzip_hcrc_impl(state, stream);
    }

    None
}

/// Write the zlib header (CMF + FLG + optional FDICT).
fn write_zlib_header_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let w_bits_adj = (state.w_bits as i32) - 8;
    #[allow(clippy::cast_sign_loss)]
    let header_base = ((Z_DEFLATED + (w_bits_adj << 4)) << 8) as u32;

    let level_flags: u32 =
        if state.strategy >= Z_HUFFMAN_ONLY || state.level < 2 {
            0
        } else if state.level < 6 {
            1
        } else if state.level == 6 {
            2
        } else {
            3
        };

    let mut header = header_base | (level_flags << 6);
    if state.strstart != 0 {
        header |= PRESET_DICT;
    }
    header += 31 - (header % 31);

    put_short_msb(state, header);

    if state.strstart != 0 {
        let adler = stream.adler;
        put_short_msb(state, adler >> 16);
        put_short_msb(state, adler & 0xffff);
    }
    stream.adler = adler32::adler32(0, &[]);
    state.status = DeflateStatus::Busy;

    flush_pending_inner(state, stream);
    if state.pending != 0 {
        state.last_flush = -1;
        return Some(ReturnCode::Ok);
    }
    None
}

/// Write the gzip header start (ID1, ID2, CM, FLG, MTIME, XFL, OS).
fn write_gzip_header_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    stream.adler = crc32::crc32(0, &[]);
    put_byte(state, 31);  // ID1
    put_byte(state, 139); // ID2
    put_byte(state, 8);   // CM = deflate

    if state.gzip_header.is_none() {
        // Minimal header — no user-supplied fields.
        put_byte(state, 0); // FLG
        put_byte(state, 0);
        put_byte(state, 0);
        put_byte(state, 0);
        put_byte(state, 0); // MTIME[0..3]
        let xfl = if state.level == 9 {
            2_u8
        } else if state.strategy >= Z_HUFFMAN_ONLY || state.level < 2 {
            4
        } else {
            0
        };
        put_byte(state, xfl);
        put_byte(state, util::OS_CODE);
        state.status = DeflateStatus::Busy;

        flush_pending_inner(state, stream);
        if state.pending != 0 {
            state.last_flush = -1;
            return Some(ReturnCode::Ok);
        }
        return None;
    }

    // User-supplied gzip header.
    // Clone the header to avoid borrowing `state.gzip_header` during writes.
    let Some(gz) = state.gzip_header.clone() else {
        state.status = DeflateStatus::Busy;
        return None;
    };

    let flg: u8 = u8::from(gz.text)
        + (u8::from(gz.hcrc) << 1)
        + (if gz.extra.is_some() { 4 } else { 0 })
        + (if gz.name.is_some() { 8 } else { 0 })
        + (if gz.comment.is_some() { 16 } else { 0 });
    put_byte(state, flg);
    #[allow(clippy::cast_possible_truncation)]
    {
        put_byte(state, (gz.time & 0xff) as u8);
        put_byte(state, ((gz.time >> 8) & 0xff) as u8);
        put_byte(state, ((gz.time >> 16) & 0xff) as u8);
        put_byte(state, ((gz.time >> 24) & 0xff) as u8);
    }
    let xfl = if state.level == 9 {
        2_u8
    } else if state.strategy >= Z_HUFFMAN_ONLY || state.level < 2 {
        4
    } else {
        0
    };
    put_byte(state, xfl);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    put_byte(state, (gz.os & 0xff) as u8);
    if let Some(ref extra) = gz.extra {
        #[allow(clippy::cast_possible_truncation)]
        {
            put_byte(state, (extra.len() & 0xff) as u8);
            put_byte(state, ((extra.len() >> 8) & 0xff) as u8);
        }
    }
    if gz.hcrc {
        stream.adler =
            crc32::crc32_z(stream.adler, &state.pending_buf[..state.pending]);
    }
    state.gzip_index = 0;
    state.status = DeflateStatus::Extra;
    None
}

/// Write the gzip extra field.
fn write_gzip_extra_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    let Some(extra) =
        state.gzip_header.as_ref().and_then(|g| g.extra.clone())
    else {
        state.status = DeflateStatus::Name;
        return None;
    };
    let total_len = extra.len();

    while state.gzip_index < total_len {
        let beg = state.pending;
        let room = state.pending_buf_size.saturating_sub(state.pending);
        if room == 0 {
            stream.adler = hcrc_update(state, stream.adler, beg);
            flush_pending_inner(state, stream);
            if state.pending != 0 {
                state.last_flush = -1;
                return Some(ReturnCode::Ok);
            }
            continue;
        }
        let left = total_len - state.gzip_index;
        let copy = left.min(room);
        state.pending_buf[state.pending..state.pending + copy]
            .copy_from_slice(&extra[state.gzip_index..state.gzip_index + copy]);
        state.pending += copy;
        state.gzip_index += copy;
        stream.adler = hcrc_update(state, stream.adler, beg);
    }

    state.gzip_index = 0;
    state.status = DeflateStatus::Name;
    None
}

/// Write the gzip file name (null-terminated).
fn write_gzip_name_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    let Some(name) =
        state.gzip_header.as_ref().and_then(|g| g.name.clone())
    else {
        state.status = DeflateStatus::Comment;
        return None;
    };

    loop {
        let beg = state.pending;
        if state.pending >= state.pending_buf_size {
            stream.adler = hcrc_update(state, stream.adler, beg);
            flush_pending_inner(state, stream);
            if state.pending != 0 {
                state.last_flush = -1;
                return Some(ReturnCode::Ok);
            }
            continue;
        }

        let val = if state.gzip_index < name.len() {
            let v = name[state.gzip_index];
            state.gzip_index += 1;
            v
        } else {
            state.gzip_index += 1;
            0_u8
        };
        put_byte(state, val);
        stream.adler = hcrc_update(state, stream.adler, beg);
        if val == 0 {
            break;
        }
    }

    state.gzip_index = 0;
    state.status = DeflateStatus::Comment;
    None
}

/// Write the gzip comment (null-terminated).
fn write_gzip_comment_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    let Some(comment) =
        state.gzip_header.as_ref().and_then(|g| g.comment.clone())
    else {
        state.status = DeflateStatus::Hcrc;
        return None;
    };

    loop {
        let beg = state.pending;
        if state.pending >= state.pending_buf_size {
            stream.adler = hcrc_update(state, stream.adler, beg);
            flush_pending_inner(state, stream);
            if state.pending != 0 {
                state.last_flush = -1;
                return Some(ReturnCode::Ok);
            }
            continue;
        }

        let val = if state.gzip_index < comment.len() {
            let v = comment[state.gzip_index];
            state.gzip_index += 1;
            v
        } else {
            state.gzip_index += 1;
            0_u8
        };
        put_byte(state, val);
        stream.adler = hcrc_update(state, stream.adler, beg);
        if val == 0 {
            break;
        }
    }

    state.gzip_index = 0;
    state.status = DeflateStatus::Hcrc;
    None
}

/// Write the gzip header CRC-16 and transition to Busy.
fn write_gzip_hcrc_impl(
    state: &mut DeflateState,
    stream: &mut ZStream,
) -> Option<ReturnCode> {
    let has_hcrc = state
        .gzip_header
        .as_ref()
        .is_some_and(|gz| gz.hcrc);

    if has_hcrc {
        if state.pending + 2 > state.pending_buf_size {
            flush_pending_inner(state, stream);
            if state.pending != 0 {
                state.last_flush = -1;
                return Some(ReturnCode::Ok);
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        {
            put_byte(state, (stream.adler & 0xff) as u8);
            put_byte(state, ((stream.adler >> 8) & 0xff) as u8);
        }
        stream.adler = crc32::crc32(0, &[]);
    }
    state.status = DeflateStatus::Busy;

    flush_pending_inner(state, stream);
    if state.pending != 0 {
        state.last_flush = -1;
        return Some(ReturnCode::Ok);
    }
    None
}

// ─── Trailer Writing ─────────────────────────────────────────────────────────

/// Write the stream trailer (zlib Adler-32 or gzip CRC-32 + ISIZE).
fn write_trailer_impl(state: &mut DeflateState, stream: &mut ZStream) {
    if state.wrap == 2 {
        // Gzip: 4-byte CRC32 + 4-byte original size (little-endian).
        let adler = stream.adler;
        #[allow(clippy::cast_possible_truncation)]
        {
            put_byte(state, (adler & 0xff) as u8);
            put_byte(state, ((adler >> 8) & 0xff) as u8);
            put_byte(state, ((adler >> 16) & 0xff) as u8);
            put_byte(state, ((adler >> 24) & 0xff) as u8);
        }
        #[allow(clippy::cast_possible_truncation)]
        let total_in = stream.total_in as u32;
        #[allow(clippy::cast_possible_truncation)]
        {
            put_byte(state, (total_in & 0xff) as u8);
            put_byte(state, ((total_in >> 8) & 0xff) as u8);
            put_byte(state, ((total_in >> 16) & 0xff) as u8);
            put_byte(state, ((total_in >> 24) & 0xff) as u8);
        }
    } else {
        // Zlib: 4-byte Adler-32 (big-endian).
        let adler = stream.adler;
        put_short_msb(state, adler >> 16);
        put_short_msb(state, adler & 0xffff);
    }
}
