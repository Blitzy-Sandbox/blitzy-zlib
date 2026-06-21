//! The DEFLATE compression engine — the module root and public driver.
//!
//! This module is the capstone of the `deflate` subtree. It is ported from
//! `deflate.c` (the public `deflate*` API and the `deflate()` main loop) and
//! `deflate.h` (the `FLUSH_BLOCK` / `RANK` macros and the `*_STATE` status
//! sentinels). It ties together the persistent engine `state`, the Huffman
//! `trees` builder, the per-level `strategy` configuration table, and the
//! five per-level compress routines (`stored`, `fast`, `slow`, `rle`,
//! `huff`):
//!
//! * It defines the header-emission state machine (`DeflateStatus`).
//! * It defines the `flush_block` / `flush_block_only` helpers that the
//!   five strategy modules import (the C `FLUSH_BLOCK*` macros lived in
//!   `deflate.c` / `deflate.h`).
//! * It implements every public `deflate*` entry point — initialization,
//!   reset, dictionary handling, parameter tuning, bound estimation, priming,
//!   header injection, copy, and teardown.
//! * It exposes the idiomatic, safe [`Deflate`] wrapper that returns
//!   `Result`/`Option` rather than C integer codes.
//!
//! # Byte-exact wire format
//!
//! This file is the byte-exact authority for everything *outside* the
//! LZ77/Huffman core: the zlib (RFC 1950) and gzip (RFC 1952) framing, the
//! `FLG` check byte, the preset-dictionary handshake, the empty-block flush
//! markers, and the Adler-32 / CRC-32 trailers. Every byte written to
//! `pending_buf` here is part of the compressed stream, so the logic mirrors
//! `deflate.c` exactly.
//!
//! ## Checksum initialization subtlety
//!
//! C seeds its running checksum with `adler32(0L, Z_NULL, 0)` (== `1`) for the
//! zlib/raw wrappers and `crc32(0L, Z_NULL, 0)` (== `0`) for gzip. The Rust
//! [`adler32`](fn@crate::checksum::adler32) returns the *seed* for an empty non-null slice (so
//! `adler32(0, &[])` is `0`, **not** `1`). Therefore every such C
//! initialization is replaced here with the literal `1u32` / `0u32`; the
//! running checksum is only ever *updated* over non-empty input slices (in
//! `read_buf_*`). This is the single most likely silent-corruption bug, so it
//! is called out at every site.
//!
//! # Safety
//!
//! There is **no** `unsafe` code in this module (AAP §0.6.2). All buffers are
//! owned `Vec`/`Box` and all access is bounds-checked slice indexing. The
//! module is `no_std`-clean: it uses only `core::` and `alloc::` items.

pub(crate) mod fast;
pub(crate) mod huff;
pub(crate) mod rle;
pub(crate) mod slow;
pub(crate) mod state;
pub(crate) mod stored;
pub(crate) mod strategy;
pub(crate) mod trees;

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use crate::checksum::adler32;
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{
    DEF_MEM_LEVEL, FlushMode, MAX_MEM_LEVEL, MAX_WBITS, Strategy, Z_DEFAULT_COMPRESSION,
    Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FIXED, Z_HUFFMAN_ONLY, Z_RLE, Z_UNKNOWN,
};
use crate::deflate::state::{BUF_SIZE, BlockState, DeflateStream, MIN_MATCH, PRESET_DICT};
use crate::error::{ReturnCode, ZlibError};
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// Re-export the persistent engine state so `ffi.rs` / `lib.rs` and the
// idiomatic wrapper can name it through `crate::deflate::DeflateState`. This
// single `pub(crate) use` both re-exports the type and brings it into scope
// for the signatures below (it is `pub(crate)` in `state.rs`, so the re-export
// matches its visibility).
pub(crate) use state::DeflateState;

// ===========================================================================
// Phase B — DeflateStatus: the header-emission state machine
// ===========================================================================

/// The DEFLATE header-emission state machine.
///
/// Replaces the integer `status` field of the C `deflate_state` struct and the
/// `*_STATE` sentinel macros from `deflate.h` (L58-67). It tracks where a
/// deflate stream is within the wrapper-header emission sequence before the
/// main DEFLATE body ([`Busy`](DeflateStatus::Busy)) and after the final block
/// ([`Finish`](DeflateStatus::Finish)).
///
/// Transitions mirror the C source:
///
/// * [`Init`](DeflateStatus::Init) — a zlib header is pending →
///   [`Busy`](DeflateStatus::Busy).
/// * [`Gzip`](DeflateStatus::Gzip) — a gzip header is pending →
///   [`Busy`](DeflateStatus::Busy) (or [`Extra`](DeflateStatus::Extra) when the
///   caller supplied a [`GzHeader`]).
/// * [`Extra`](DeflateStatus::Extra) → [`Name`](DeflateStatus::Name) →
///   [`Comment`](DeflateStatus::Comment) → [`Hcrc`](DeflateStatus::Hcrc) — the
///   optional gzip extra/name/comment/header-CRC fields.
/// * [`Busy`](DeflateStatus::Busy) — emitting compressed blocks →
///   [`Finish`](DeflateStatus::Finish).
/// * [`Finish`](DeflateStatus::Finish) — the stream is complete.
///
/// # Discriminant values
///
/// The explicit discriminants reproduce the exact C sentinel constants. They
/// are deliberately *not* sequential — for example [`Finish`](DeflateStatus::Finish)
/// is `666` — and preserving them keeps the Rust engine bit-for-bit faithful
/// to any C logic that compares against the raw sentinels. Because `status` is
/// an enum it can never hold an out-of-range value, so the C
/// `deflateStateCheck` status-validity test is satisfied by construction (the
/// FFI layer performs the raw-pointer / NULL validation before a
/// [`DeflateStream`] is ever constructed).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(i32)]
pub(crate) enum DeflateStatus {
    /// `INIT_STATE` (42): a zlib wrapper header is pending.
    Init = 42,
    /// `GZIP_STATE` (57): a gzip wrapper header is pending.
    Gzip = 57,
    /// `EXTRA_STATE` (69): emitting the gzip `FEXTRA` field.
    Extra = 69,
    /// `NAME_STATE` (73): emitting the gzip `FNAME` field.
    Name = 73,
    /// `COMMENT_STATE` (91): emitting the gzip `FCOMMENT` field.
    Comment = 91,
    /// `HCRC_STATE` (103): emitting the gzip header CRC-16.
    Hcrc = 103,
    /// `BUSY_STATE` (113): emitting compressed DEFLATE blocks.
    Busy = 113,
    /// `FINISH_STATE` (666): the stream is complete.
    Finish = 666,
}

// ===========================================================================
// Phase C — small constants and helpers
// ===========================================================================

/// Operating-system code written into byte 9 of the gzip header.
///
/// `zutil.h` derives `OS_CODE` from the build platform, defaulting to `3`
/// (Unix). It is **fixed** at `3` here so the gzip header is byte-identical
/// across platforms (AAP §0.7.1 byte-exactness); a platform-dependent value
/// would break interoperability tests.
#[cfg(feature = "gzip")]
const OS_CODE: u8 = 3;

/// `deflate.h` `#define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))`.
///
/// Ranks the flush modes so that `Z_BLOCK` (`5`) sorts between `Z_NO_FLUSH`
/// (`0`) and `Z_PARTIAL_FLUSH` (`1`); used to detect duplicate consecutive
/// flushes in the [`deflate`] prologue.
#[inline]
fn rank(f: i32) -> i32 {
    f * 2 - if f > 4 { 9 } else { 0 }
}

/// Identifies which per-level compress routine a compression level selects.
///
/// In C, `deflateParams` compares the function pointers
/// `configuration_table[old_level].func` and `configuration_table[level].func`
/// to decide whether the active compression routine changed (`deflate.c`
/// L788, L791). The per-level mapping is fixed: level `0` → `deflate_stored`,
/// levels `1..=3` → `deflate_fast`, levels `4..=9` → `deflate_slow`. This
/// helper returns a stable discriminator (`0`/`1`/`2`) for that comparison.
#[inline]
fn config_compress_fn(level: i32) -> u8 {
    match level {
        0 => 0,
        1..=3 => 1,
        _ => 2,
    }
}

// ===========================================================================
// Phase D — flush_block / flush_block_only (imported by the strategy modules)
// ===========================================================================

/// Flush the current block and copy pending output, without the
/// output-exhaustion early return. Ports `deflate.h`'s `FLUSH_BLOCK_ONLY`
/// macro.
///
/// Emits the block spanning `[block_start, strstart)` of the window (the C
/// `_tr_flush_block(s, &window[block_start], strstart - block_start, last)`),
/// advances `block_start` to `strstart`, then flushes `pending_buf` to the
/// output via [`DeflateStream::flush_pending`].
///
/// `block_start` is signed (`isize`): a negative value means there is no valid
/// window source for the block (the C `block_start >= 0 ? &window[..] : Z_NULL`
/// ternary), which becomes `None` here.
pub(crate) fn flush_block_only(s: &mut DeflateStream, last: bool) {
    // Length of the block is `strstart - block_start` regardless of the sign of
    // `block_start` (C casts both to `long` before subtracting).
    let stored_len = (s.state.strstart as isize - s.state.block_start) as usize;

    if s.state.block_start >= 0 {
        // C passes `&s->window[block_start]` as the stored-block source, which
        // aliases the state. `tr_flush_block` only ever *reads* this slice (to
        // copy it into `pending_buf`), so temporarily move the window out with
        // an allocation-free `mem::take`, hand the borrow checker a disjoint
        // slice, then put the window back. Behavior-identical to the C alias.
        let bs = s.state.block_start as usize;
        let window = core::mem::take(&mut s.state.window);
        trees::tr_flush_block(
            s.state,
            Some(&window[bs..bs + stored_len]),
            stored_len,
            last,
        );
        s.state.window = window;
    } else {
        trees::tr_flush_block(s.state, None, stored_len, last);
    }

    s.state.block_start = s.state.strstart as isize;
    s.flush_pending();
}

/// Flush the current block, returning a [`BlockState`] when the output buffer
/// filled. Ports `deflate.h`'s `FLUSH_BLOCK` macro.
///
/// After [`flush_block_only`], if no output room remains the caller must stop:
/// it returns [`BlockState::FinishStarted`] when this was the final block and
/// [`BlockState::NeedMore`] otherwise. The five strategy modules use the
/// idiom `if let Some(bs) = flush_block(s, last) { return bs; }`.
pub(crate) fn flush_block(s: &mut DeflateStream, last: bool) -> Option<BlockState> {
    flush_block_only(s, last);
    if s.avail_out() == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}

/// Fold the just-written gzip-header bytes into the running header CRC.
///
/// Ports `deflate.c`'s `HCRC_UPDATE(beg)` macro (L973-978): when the caller's
/// gzip header requests a header CRC and bytes were written since `beg`, update
/// `adler` (which holds the gzip CRC-32 during header emission) over
/// `pending_buf[beg..pending]`. The immutable borrow of `pending_buf` ends
/// before `adler` is assigned, so no `unsafe` and no aliasing is involved.
#[cfg(feature = "gzip")]
#[inline]
fn hcrc_update(s: &mut DeflateState, beg: usize) {
    let hcrc = s.gzhead.as_ref().is_some_and(|h| h.hcrc);
    if hcrc && s.pending > beg {
        s.adler = crc32(s.adler, &s.pending_buf[beg..s.pending]);
    }
}

// ===========================================================================
// Phase F — deflate(): the compression driver (deflate.c L981-1290)
// ===========================================================================

/// The DEFLATE compression driver — the heart of the engine.
///
/// Consumes input from `s.input` and produces compressed output into
/// `s.output`, advancing through the header-emission state machine
/// ([`DeflateStatus`]), dispatching to the per-level compress routine, handling
/// the seven flush modes, and writing the wrapper trailer on `Z_FINISH`. This
/// is a near-literal port of `deflate.c` L981-1290; the inline comments cite
/// the corresponding C line ranges.
///
/// Returns [`ReturnCode::Ok`] for normal progress, [`ReturnCode::StreamEnd`]
/// when a `Z_FINISH` stream is fully written, [`ZlibError::BufError`] for a
/// no-progress / no-output-room situation, and [`ZlibError::StreamError`] for
/// an inconsistent call (post-finish with the wrong flush).
///
/// # Field mapping
///
/// `strm->avail_out` → [`DeflateStream::avail_out`]; `strm->avail_in` →
/// [`DeflateStream::avail_in`]; the C `deflate_state *s` → `s.state`; the C
/// `strm->adler` / `total_in` / `last_flush` live on `s.state`.
pub(crate) fn deflate(s: &mut DeflateStream, flush: FlushMode) -> Result<ReturnCode, ZlibError> {
    // --- Validation prologue (deflate.c L985-1026) ---------------------------
    // C also rejects `flush > Z_BLOCK || flush < 0` and NULL `next_out`/`next_in`;
    // those are unrepresentable here (`flush` is a validated `FlushMode`, and the
    // I/O buffers are slices that are never null — the FFI shim validates the raw
    // `int` flush via `FlushMode::try_from` before calling).
    if s.state.status == DeflateStatus::Finish && flush != FlushMode::Finish {
        return Err(ZlibError::StreamError);
    }
    if s.avail_out() == 0 {
        return Err(ZlibError::BufError);
    }

    let old_flush = s.state.last_flush;
    s.state.last_flush = flush as i32;

    // Flush as much pending output as possible (deflate.c L1003-1015).
    if s.state.pending != 0 {
        s.flush_pending();
        if s.avail_out() == 0 {
            // avail_out == 0: deflate will be called again with more output
            // room. Return Z_OK (not BUF_ERROR) so the next call is not mistaken
            // for a useless duplicate flush.
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    } else if s.avail_in() == 0
        && rank(flush as i32) <= rank(old_flush)
        && flush != FlushMode::Finish
    {
        // Nothing to do and a duplicate consecutive flush (deflate.c L1017-1020).
        return Err(ZlibError::BufError);
    }

    // The user must not provide more input after the first Z_FINISH (L1023-1025).
    if s.state.status == DeflateStatus::Finish && s.avail_in() != 0 {
        return Err(ZlibError::BufError);
    }

    // --- zlib header: INIT_STATE (deflate.c L1028-1064) ----------------------
    if s.state.status == DeflateStatus::Init && s.state.wrap == 0 {
        s.state.status = DeflateStatus::Busy;
    }
    if s.state.status == DeflateStatus::Init {
        // CMF/FLG: method + window size, then level hint, optional preset-dict
        // bit, then the modulo-31 check value.
        let mut header: u32 = (Z_DEFLATED as u32 + ((s.state.w_bits - 8) << 4)) << 8;
        let level_flags: u32 = if s.state.strategy >= Z_HUFFMAN_ONLY || s.state.level < 2 {
            0
        } else if s.state.level < 6 {
            1
        } else if s.state.level == 6 {
            2
        } else {
            3
        };
        header |= level_flags << 6;
        if s.state.strstart != 0 {
            header |= PRESET_DICT as u32;
        }
        header += 31 - (header % 31);

        s.state.put_short_msb(header as u16);

        // Save the Adler-32 of the preset dictionary, if one was set.
        if s.state.strstart != 0 {
            s.state.put_short_msb((s.state.adler >> 16) as u16);
            s.state.put_short_msb((s.state.adler & 0xffff) as u16);
        }
        // Restart the running checksum for the data body. C uses
        // `adler32(0L, Z_NULL, 0)` (== 1); use the literal — never `adler32`
        // over an empty slice (which would yield 0).
        s.state.adler = 1;
        s.state.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer.
        s.flush_pending();
        if s.state.pending != 0 {
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }

    // --- gzip header: GZIP_STATE .. HCRC_STATE (deflate.c L1065-1208) ---------
    // The gzip header references the `gzhead`/`gzindex` fields, which only exist
    // under the `gzip` feature; the whole block is therefore feature-gated. The
    // states are written as sequential `if` blocks (not a `match`) so that they
    // fall through within a single call exactly as the C source does.
    #[cfg(feature = "gzip")]
    {
        if s.state.status == DeflateStatus::Gzip {
            // Restart CRC for the data body. C: `crc32(0L, Z_NULL, 0)` == 0.
            s.state.adler = 0;
            s.state.put_byte(31);
            s.state.put_byte(139);
            s.state.put_byte(8);

            // The XFL byte: 2 for best compression, 4 for fastest, else 0.
            let xfl: u8 = if s.state.level == 9 {
                2
            } else if s.state.strategy >= Z_HUFFMAN_ONLY || s.state.level < 2 {
                4
            } else {
                0
            };

            // Snapshot the scalar header fields (the `gzhead` borrow must not be
            // held across the `put_byte` mutations).
            let head_info = s.state.gzhead.as_ref().map(|h| {
                (
                    h.flags(),
                    h.time,
                    h.os,
                    h.extra.is_some(),
                    h.extra.as_ref().map_or(0usize, |e| e.len()),
                )
            });

            match head_info {
                None => {
                    // No user header: emit MTIME=0, XFL, OS.
                    s.state.put_byte(0);
                    s.state.put_byte(0);
                    s.state.put_byte(0);
                    s.state.put_byte(0);
                    s.state.put_byte(0);
                    s.state.put_byte(xfl);
                    s.state.put_byte(OS_CODE);
                    s.state.status = DeflateStatus::Busy;

                    s.flush_pending();
                    if s.state.pending != 0 {
                        s.state.last_flush = -1;
                        return Ok(ReturnCode::Ok);
                    }
                }
                Some((flg, time, os, has_extra, extra_len)) => {
                    s.state.put_byte(flg);
                    s.state.put_byte((time & 0xff) as u8);
                    s.state.put_byte(((time >> 8) & 0xff) as u8);
                    s.state.put_byte(((time >> 16) & 0xff) as u8);
                    s.state.put_byte(((time >> 24) & 0xff) as u8);
                    s.state.put_byte(xfl);
                    s.state.put_byte((os & 0xff) as u8);
                    if has_extra {
                        // XLEN, little-endian (low 16 bits of the extra length).
                        s.state.put_byte((extra_len & 0xff) as u8);
                        s.state.put_byte(((extra_len >> 8) & 0xff) as u8);
                    }
                    // If a header CRC was requested, fold the header so far.
                    hcrc_update(s.state, 0);
                    s.state.gzindex = 0;
                    s.state.status = DeflateStatus::Extra;
                }
            }
        }

        // EXTRA_STATE (deflate.c L1117-1143): copy the FEXTRA field in chunks,
        // flushing whenever the pending buffer fills.
        if s.state.status == DeflateStatus::Extra {
            let extra = s.state.gzhead.as_ref().and_then(|h| h.extra.clone());
            if let Some(extra) = extra {
                let mut beg = s.state.pending;
                let mut left = (extra.len() & 0xffff) - s.state.gzindex;
                while s.state.pending + left > s.state.pending_buf_size {
                    let copy = s.state.pending_buf_size - s.state.pending;
                    let p = s.state.pending;
                    let gi = s.state.gzindex;
                    s.state.pending_buf[p..p + copy].copy_from_slice(&extra[gi..gi + copy]);
                    s.state.pending = s.state.pending_buf_size;
                    hcrc_update(s.state, beg);
                    s.state.gzindex += copy;
                    s.flush_pending();
                    if s.state.pending != 0 {
                        s.state.last_flush = -1;
                        return Ok(ReturnCode::Ok);
                    }
                    beg = 0;
                    left -= copy;
                }
                let p = s.state.pending;
                let gi = s.state.gzindex;
                s.state.pending_buf[p..p + left].copy_from_slice(&extra[gi..gi + left]);
                s.state.pending += left;
                hcrc_update(s.state, beg);
                s.state.gzindex = 0;
            }
            s.state.status = DeflateStatus::Name;
        }

        // NAME_STATE (deflate.c L1144-1165): emit the NUL-terminated FNAME. The
        // idiomatic `GzHeader` stores the name without a terminating NUL, so the
        // serializer appends it here (index `name.len()` yields the `0`).
        if s.state.status == DeflateStatus::Name {
            let name = s.state.gzhead.as_ref().and_then(|h| h.name.clone());
            if let Some(name) = name {
                let mut beg = s.state.pending;
                loop {
                    if s.state.pending == s.state.pending_buf_size {
                        hcrc_update(s.state, beg);
                        s.flush_pending();
                        if s.state.pending != 0 {
                            s.state.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                        beg = 0;
                    }
                    let gi = s.state.gzindex;
                    let val = if gi < name.len() { name[gi] } else { 0 };
                    s.state.gzindex += 1;
                    s.state.put_byte(val);
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s.state, beg);
                s.state.gzindex = 0;
            }
            s.state.status = DeflateStatus::Comment;
        }

        // COMMENT_STATE (deflate.c L1166-1186): symmetric to NAME for FCOMMENT.
        if s.state.status == DeflateStatus::Comment {
            let comment = s.state.gzhead.as_ref().and_then(|h| h.comment.clone());
            if let Some(comment) = comment {
                let mut beg = s.state.pending;
                loop {
                    if s.state.pending == s.state.pending_buf_size {
                        hcrc_update(s.state, beg);
                        s.flush_pending();
                        if s.state.pending != 0 {
                            s.state.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                        beg = 0;
                    }
                    let gi = s.state.gzindex;
                    let val = if gi < comment.len() { comment[gi] } else { 0 };
                    s.state.gzindex += 1;
                    s.state.put_byte(val);
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s.state, beg);
            }
            s.state.status = DeflateStatus::Hcrc;
        }

        // HCRC_STATE (deflate.c L1187-1208): emit the 2-byte header CRC-16.
        if s.state.status == DeflateStatus::Hcrc {
            let hcrc = s.state.gzhead.as_ref().is_some_and(|h| h.hcrc);
            if hcrc {
                if s.state.pending + 2 > s.state.pending_buf_size {
                    s.flush_pending();
                    if s.state.pending != 0 {
                        s.state.last_flush = -1;
                        return Ok(ReturnCode::Ok);
                    }
                }
                s.state.put_byte((s.state.adler & 0xff) as u8);
                s.state.put_byte(((s.state.adler >> 8) & 0xff) as u8);
                // Restart CRC for the data body. C: `crc32(0L, Z_NULL, 0)` == 0.
                s.state.adler = 0;
            }
            s.state.status = DeflateStatus::Busy;

            s.flush_pending();
            if s.state.pending != 0 {
                s.state.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    }

    // --- Start a new block or continue the current one (deflate.c L1210-1261) -
    if s.avail_in() != 0
        || s.state.lookahead != 0
        || (flush != FlushMode::NoFlush && s.state.status != DeflateStatus::Finish)
    {
        // The C dispatch ternary (deflate.c L1217-1220): level 0 stores;
        // Z_HUFFMAN_ONLY and Z_RLE select their strategy routines; otherwise
        // `configuration_table[level].func` picks deflate_fast (levels 1..=3) or
        // deflate_slow (levels 4..=9).
        let bstate = if s.state.level == 0 {
            stored::deflate_stored(s, flush)
        } else if s.state.strategy == Z_HUFFMAN_ONLY {
            huff::deflate_huff(s, flush)
        } else if s.state.strategy == Z_RLE {
            rle::deflate_rle(s, flush)
        } else if s.state.level <= 3 {
            fast::deflate_fast(s, flush)
        } else {
            slow::deflate_slow(s, flush)
        };

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            s.state.status = DeflateStatus::Finish;
        }
        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if s.avail_out() == 0 {
                s.state.last_flush = -1; // avoid BUF_ERROR on the next call
            }
            // For a non-NoFlush call with no output room, the next call must
            // reuse the same flush; we emit no empty block here.
            return Ok(ReturnCode::Ok);
        }
        if bstate == BlockState::BlockDone {
            if flush == FlushMode::PartialFlush {
                trees::tr_align(s.state);
            } else if flush != FlushMode::Block {
                // FULL_FLUSH or SYNC_FLUSH: emit an empty stored block, which
                // inflate_sync() recognizes as a marker.
                trees::tr_stored_block(s.state, None, 0, false);
                if flush == FlushMode::FullFlush {
                    s.state.clear_hash(); // forget the history
                    if s.state.lookahead == 0 {
                        s.state.strstart = 0;
                        s.state.block_start = 0;
                        s.state.insert = 0;
                    }
                }
            }
            s.flush_pending();
            if s.avail_out() == 0 {
                s.state.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    }

    // --- Trailer (deflate.c L1263-1289) --------------------------------------
    if flush != FlushMode::Finish {
        return Ok(ReturnCode::Ok);
    }
    if s.state.wrap <= 0 {
        // Raw DEFLATE (no wrapper) or the trailer was already written.
        return Ok(ReturnCode::StreamEnd);
    }

    // Write the wrapper trailer. The gzip branch uses only universally-present
    // fields (`adler` holds the gzip CRC-32, `total_in` the input length), so it
    // needs no feature gate — `wrap == 2` is simply never reached without the
    // gzip feature.
    if s.state.wrap == 2 {
        // gzip trailer: 4-byte CRC-32 (LSB-first) then 4-byte ISIZE (input
        // length modulo 2^32, LSB-first).
        let crc = s.state.adler;
        s.state.put_byte((crc & 0xff) as u8);
        s.state.put_byte(((crc >> 8) & 0xff) as u8);
        s.state.put_byte(((crc >> 16) & 0xff) as u8);
        s.state.put_byte(((crc >> 24) & 0xff) as u8);
        let total = s.state.total_in;
        s.state.put_byte((total & 0xff) as u8);
        s.state.put_byte(((total >> 8) & 0xff) as u8);
        s.state.put_byte(((total >> 16) & 0xff) as u8);
        s.state.put_byte(((total >> 24) & 0xff) as u8);
    } else {
        // zlib trailer: 4-byte Adler-32, most-significant byte first.
        let adler = s.state.adler;
        s.state.put_short_msb((adler >> 16) as u16);
        s.state.put_short_msb((adler & 0xffff) as u16);
    }

    s.flush_pending();
    // Write the trailer only once.
    if s.state.wrap > 0 {
        s.state.wrap = -s.state.wrap;
    }
    if s.state.pending != 0 {
        Ok(ReturnCode::Ok)
    } else {
        Ok(ReturnCode::StreamEnd)
    }
}

// ===========================================================================
// Phase G — deflate_init2 / deflate_init (deflate.c L387-533)
// ===========================================================================

/// Construct and fully initialize a deflate state. Ports `deflateInit2_`
/// (`deflate.c` L387-533), minus the C-only `version` / `stream_size` /
/// `z_stream`-NULL / `zalloc`/`zfree` checks (those belong to the FFI shim).
///
/// `window_bits` overloads the wrapper selection exactly as zlib does:
///
/// * `8..=15` → zlib wrapper (RFC 1950), `wrap = 1`.
/// * `-15..=-8` → raw DEFLATE (RFC 1951), no wrapper, `wrap = 0`.
/// * `24..=31` (i.e. `windowBits + 16`) → gzip wrapper (RFC 1952), `wrap = 2`
///   (only under the `gzip` feature; otherwise rejected by the range check).
///
/// Returns the heap-allocated [`DeflateState`] on success, or
/// [`ZlibError::StreamError`] for any out-of-range argument.
pub(crate) fn deflate_init2(
    level: i32,
    method: i32,
    mut window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Result<Box<DeflateState>, ZlibError> {
    // Resolve the default level (Z_DEFAULT_COMPRESSION == -1 → 6).
    let level = if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    };

    // Decode the wrapper selection encoded in the sign/magnitude of windowBits.
    let mut wrap = 1;
    if window_bits < 0 {
        // Negative: raw DEFLATE, no wrapper.
        wrap = 0;
        if window_bits < -15 {
            return Err(ZlibError::StreamError);
        }
        window_bits = -window_bits;
    } else {
        // `windowBits > 15` requests the gzip wrapper; only honored when the
        // `gzip` feature is enabled. When it is disabled, `window_bits` stays
        // out of range and is rejected by the validation below — matching a C
        // build compiled without `GZIP`.
        #[cfg(feature = "gzip")]
        if window_bits > 15 {
            wrap = 2;
            window_bits -= 16;
        }
    }

    // Validate every argument exactly as C does. `windowBits == 8` is only
    // legal for the zlib wrapper (`wrap == 1`). (Range checks are expressed
    // with `RangeInclusive::contains` to satisfy clippy; the bounds are
    // identical to the C `x < lo || x > hi` tests.)
    if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
        || method != Z_DEFLATED
        || !(8..=15).contains(&window_bits)
        || !(0..=9).contains(&level)
        || !(0..=Z_FIXED).contains(&strategy)
        || (window_bits == 8 && wrap != 1)
    {
        return Err(ZlibError::StreamError);
    }
    // A 256-byte window is promoted to 512 bytes (the historical zlib fix for
    // the 256-byte-window bug).
    if window_bits == 8 {
        window_bits = 9;
    }

    // `hash_bits = memLevel + 7` (deflate.c L455). `new_allocated` derives
    // `hash_size`/`hash_mask`/`hash_shift`, `lit_bufsize`, `pending_buf`, and
    // the overlaid `sym_buf`/`sym_end` from these post-validation values.
    let w_bits = window_bits as u32;
    let hash_bits = (mem_level + 7) as u32;

    let mut state = DeflateState::new_allocated(
        w_bits,
        hash_bits,
        mem_level,
        level,
        strategy,
        method as u8,
        wrap,
    );

    // Initialize the running state (status, checksum seed, trees, LZ match
    // engine). `new_allocated` performs allocation only.
    deflate_reset(&mut state)?;

    Ok(Box::new(state))
}

/// Construct a deflate state with the default method, window, memory level, and
/// strategy. Thin wrapper over [`deflate_init2`], mirroring the C `deflateInit_`
/// convenience entry point.
pub(crate) fn deflate_init(level: i32) -> Result<Box<DeflateState>, ZlibError> {
    deflate_init2(
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Z_DEFAULT_STRATEGY,
    )
}

// ===========================================================================
// Phase H — reset (deflate.c L644-711). `lm_init` is a DeflateState method.
// ===========================================================================

/// Reset the running stream state while keeping the allocated buffers and the
/// LZ match-engine configuration. Ports `deflateResetKeep` (`deflate.c`
/// L644-677), minus the C `deflateStateCheck` (an FFI concern).
///
/// Restores the wrapper-header status and re-seeds the running checksum. The
/// checksum seed is written as a **literal** (`1` for zlib/raw, `0` for gzip),
/// reproducing C's `adler32(0L, Z_NULL, 0)` / `crc32(0L, Z_NULL, 0)` without
/// calling the checksum routines over an empty slice (which would yield `0`).
pub(crate) fn deflate_reset_keep(s: &mut DeflateState) {
    s.total_in = 0;
    s.total_out = 0;
    s.msg = None;
    s.data_type = Z_UNKNOWN;

    s.pending = 0;
    s.pending_out = 0;

    // A negative `wrap` (set by a prior `deflate(..., Z_FINISH)` to mark the
    // trailer as written) is restored to positive on reset.
    if s.wrap < 0 {
        s.wrap = -s.wrap;
    }

    // The wrapper-dependent status and checksum seed. The gzip branch
    // (`wrap == 2`) is only reachable when the `gzip` feature is enabled.
    #[cfg(feature = "gzip")]
    {
        if s.wrap == 2 {
            s.status = DeflateStatus::Gzip;
            s.adler = 0; // == C crc32(0L, Z_NULL, 0)
        } else {
            s.status = DeflateStatus::Init;
            s.adler = 1; // == C adler32(0L, Z_NULL, 0)
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        s.status = DeflateStatus::Init;
        s.adler = 1; // == C adler32(0L, Z_NULL, 0)
    }

    s.last_flush = -2;

    trees::tr_init(s);
}

/// Fully reset a deflate stream to begin a fresh compression. Ports
/// `deflateReset` (`deflate.c` L704-711): [`deflate_reset_keep`] followed by the
/// LZ match-engine re-initialization ([`DeflateState::lm_init`], which reloads
/// the per-level parameters from the configuration table and clears the hash).
pub(crate) fn deflate_reset(s: &mut DeflateState) -> Result<ReturnCode, ZlibError> {
    deflate_reset_keep(s);
    s.lm_init();
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Phase I — dictionary (deflate.c L559-641)
// ===========================================================================

/// Initialize the compression dictionary from `dictionary`. Ports
/// `deflateSetDictionary` (`deflate.c` L559-622).
///
/// Must be called before any [`deflate`] call (and, for the zlib wrapper,
/// before the header is emitted). Returns [`ZlibError::StreamError`] if a gzip
/// wrapper is in use, if the zlib header was already written, or if any
/// lookahead has been buffered.
///
/// The dictionary is loaded into the sliding window and hash exactly as C does:
/// a temporary I/O context whose input is the dictionary (and whose output is
/// empty) is filled and hashed, so the subsequent compression sees the
/// dictionary as preceding history without it appearing in the output.
pub(crate) fn deflate_set_dictionary(
    s: &mut DeflateState,
    dictionary: &[u8],
) -> Result<ReturnCode, ZlibError> {
    let wrap = s.wrap;
    // Reject a gzip wrapper, a zlib wrapper whose header was already emitted, or
    // a non-empty lookahead (dictionary must be set on a pristine stream).
    if wrap == 2 || (wrap == 1 && s.status != DeflateStatus::Init) || s.lookahead != 0 {
        return Err(ZlibError::StreamError);
    }

    // For the zlib wrapper, the dictionary contributes to the stream's
    // Adler-32 (this is the only place a non-empty slice is folded here).
    if wrap == 1 {
        s.adler = adler32(s.adler, dictionary);
    }
    // Suppress checksum accumulation while the dictionary is read into the
    // window (the dictionary is history, not stream data).
    s.wrap = 0;

    // If the dictionary is at least a full window, only its tail matters.
    let mut dict = dictionary;
    if dict.len() >= s.w_size {
        if wrap == 0 {
            // Otherwise the history is already empty.
            s.clear_hash();
            s.strstart = 0;
            s.block_start = 0;
            s.insert = 0;
        }
        dict = &dict[dict.len() - s.w_size..];
    }

    // Run fill_window + the hash-insertion loop over a temporary stream whose
    // input is the dictionary and whose output buffer is empty. The borrow of
    // `s` is confined to this block so the field fix-ups afterwards are legal.
    {
        let mut empty: [u8; 0] = [];
        let mut stream = DeflateStream {
            state: s,
            input: dict,
            in_next: 0,
            output: &mut empty,
            out_next: 0,
        };
        stream.fill_window();
        while stream.state.lookahead >= MIN_MATCH {
            let mut str = stream.state.strstart;
            let mut n = stream.state.lookahead - (MIN_MATCH - 1);
            loop {
                // Equivalent to the C `UPDATE_HASH` + `prev`/`head` writes.
                stream.state.insert_string(str);
                str += 1;
                n -= 1;
                if n == 0 {
                    break;
                }
            }
            stream.state.strstart = str;
            stream.state.lookahead = MIN_MATCH - 1;
            stream.fill_window();
        }
    }

    // Finalize: the loaded dictionary becomes the preceding history.
    s.strstart += s.lookahead;
    s.block_start = s.strstart as isize;
    s.insert = s.lookahead;
    s.lookahead = 0;
    s.match_length = MIN_MATCH - 1;
    s.prev_length = MIN_MATCH - 1;
    s.match_available = false;
    // Restore the wrapper mode.
    s.wrap = wrap;

    Ok(ReturnCode::Ok)
}

/// Copy the current sliding-window history into `dictionary`. Ports
/// `deflateGetDictionary` (`deflate.c` L625-641).
///
/// Returns `(ReturnCode::Ok, len)` where `len` is the number of history bytes
/// (at most `w_size`). When `dictionary` is `Some`, the most recent `len` bytes
/// of the window are copied into it; when `None`, only the length is reported.
pub(crate) fn deflate_get_dictionary(
    s: &DeflateState,
    dictionary: Option<&mut [u8]>,
) -> (ReturnCode, usize) {
    let mut len = s.strstart + s.lookahead;
    if len > s.w_size {
        len = s.w_size;
    }
    if let Some(dict) = dictionary {
        if len != 0 {
            let start = s.strstart + s.lookahead - len;
            dict[..len].copy_from_slice(&s.window[start..start + len]);
        }
    }
    (ReturnCode::Ok, len)
}

// ===========================================================================
// Phase J — params / tune (deflate.c L774-830)
// ===========================================================================

/// Dynamically change the compression level and strategy mid-stream. Ports
/// `deflateParams` (`deflate.c` L774-816).
///
/// If the active compress routine or strategy would change and data has already
/// been processed (`last_flush != -2`), the pending block is first flushed with
/// `Z_BLOCK`; if any input or buffered data remains afterwards the change is
/// refused with [`ZlibError::BufError`] (the caller must drain it first). When
/// the level changes, the per-level LZ parameters are reloaded from the
/// configuration table, sliding or clearing the hash as needed if leaving the
/// store-only level 0.
pub(crate) fn deflate_params(
    s: &mut DeflateStream,
    level: i32,
    strategy: i32,
) -> Result<ReturnCode, ZlibError> {
    // Resolve the default level.
    let level = if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    };
    if !(0..=9).contains(&level) || !(0..=Z_FIXED).contains(&strategy) {
        return Err(ZlibError::StreamError);
    }

    // Compare the active compress routine (old level) against the requested
    // one (new level); this mirrors the C function-pointer comparison.
    let old_func = config_compress_fn(s.state.level);
    let new_func = config_compress_fn(level);

    if (strategy != s.state.strategy || old_func != new_func) && s.state.last_flush != -2 {
        // Flush the current block. C only aborts on Z_STREAM_ERROR; any other
        // outcome (Z_OK, Z_BUF_ERROR, ...) is tolerated and we fall through to
        // the drained-data check below.
        if let Err(ZlibError::StreamError) = deflate(s, FlushMode::Block) {
            return Err(ZlibError::StreamError);
        }
        // If input or buffered window data remains, the parameter change cannot
        // proceed (the existing block would mix old and new parameters).
        let buffered = s.state.strstart as isize - s.state.block_start + s.state.lookahead as isize;
        if s.avail_in() != 0 || buffered != 0 {
            return Err(ZlibError::BufError);
        }
    }

    if s.state.level != level {
        // Leaving level 0 (store-only): the hash was not maintained, so rebuild
        // or clear it depending on how much was stored.
        if s.state.level == 0 && s.state.matches != 0 {
            if s.state.matches == 1 {
                s.state.slide_hash();
            } else {
                s.state.clear_hash();
            }
            s.state.matches = 0;
        }
        s.state.level = level;
        let cfg = strategy::CONFIG_TABLE[level as usize];
        s.state.max_lazy_match = cfg.max_lazy as usize;
        s.state.good_match = cfg.good_length as usize;
        s.state.nice_match = cfg.nice_length as usize;
        s.state.max_chain_length = cfg.max_chain as usize;
    }
    s.state.strategy = strategy;
    Ok(ReturnCode::Ok)
}

/// Fine-tune the internal LZ match-search parameters. Ports `deflateTune`
/// (`deflate.c` L819-830).
///
/// For advanced callers who want to override the per-level heuristics directly.
/// All four values map straight onto the live match-engine fields.
pub(crate) fn deflate_tune(
    s: &mut DeflateState,
    good_length: i32,
    max_lazy: i32,
    nice_length: i32,
    max_chain: i32,
) -> Result<ReturnCode, ZlibError> {
    s.good_match = good_length as usize;
    s.max_lazy_match = max_lazy as usize;
    s.nice_match = nice_length as usize;
    s.max_chain_length = max_chain as usize;
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Phase K — bound / pending / used / prime (deflate.c L722-928)
// ===========================================================================

/// Report how much output is pending and how many bits are buffered. Ports
/// `deflatePending` (`deflate.c` L722-734).
///
/// Returns `(pending_bytes, valid_bits)`. The C overflow guard (pending
/// exceeding `unsigned`) cannot trigger here: `pending` never exceeds
/// `pending_buf_size` (`lit_bufsize * 4 ≤ 128 KiB`), which fits in a `u32`.
pub(crate) fn deflate_pending(s: &DeflateState) -> (u32, i32) {
    (s.pending as u32, s.bi_valid)
}

/// Report how many bits of the last byte were used. Ports `deflateUsed`
/// (`deflate.c` L737-742); part of the deflate API surface for callers that
/// bit-pack alongside a deflate stream.
pub(crate) fn deflate_used(s: &DeflateState) -> i32 {
    s.bi_used
}

/// Insert `bits` low-order bits of `value` ahead of the next compressed output.
/// Ports `deflatePrime` (`deflate.c` L745-771).
///
/// Used to byte-align or prepend custom bits to a raw stream. Returns
/// [`ZlibError::BufError`] if `bits` is out of range (`0..=16`) or if the bit
/// buffer would overrun into the symbol region of `pending_buf`.
pub(crate) fn deflate_prime(
    s: &mut DeflateState,
    mut bits: i32,
    mut value: i32,
) -> Result<ReturnCode, ZlibError> {
    // The symbol region begins at offset `lit_bufsize` in `pending_buf`; the
    // bit-flush below writes at `pending`, and must keep clear of it. C compares
    // the `sym_buf` pointer to `pending_out + ((Buf_size + 7) >> 3)`.
    let guard = s.pending_out + ((BUF_SIZE + 7) >> 3) as usize;
    if !(0..=16).contains(&bits) || s.lit_bufsize < guard {
        return Err(ZlibError::BufError);
    }
    loop {
        // Fill at most the remaining room in the 16-bit buffer.
        let mut put = BUF_SIZE - s.bi_valid;
        if put > bits {
            put = bits;
        }
        // `put + bi_valid <= 16`, so the shifted value always fits in `u16`.
        s.bi_buf |= ((value & ((1 << put) - 1)) << s.bi_valid) as u16;
        s.bi_valid += put;
        trees::tr_flush_bits(s);
        value >>= put;
        bits -= put;
        if bits == 0 {
            break;
        }
    }
    Ok(ReturnCode::Ok)
}

/// Compute an upper bound on the compressed size of `source_len` bytes. Ports
/// `deflateBound_z` (`deflate.c` L856-928).
///
/// When `s` is `None` (no stream parameters available), the larger of the two
/// worst-case bounds plus a maximal wrapper is returned. With a stream, the
/// wrapper length is computed exactly (including any user gzip header), and for
/// the default `windowBits == 15` / `memLevel == 8` a tight `~0.03%` bound is
/// returned; otherwise one of the conservative `~4%` / `~13%` bounds is used.
/// All arithmetic saturates to [`u64::MAX`] on overflow, matching C's
/// `(z_size_t)-1`.
pub(crate) fn deflate_bound(s: Option<&DeflateState>, source_len: u64) -> u64 {
    // Worst case for fixed blocks with 9-bit literals (~13% + constant).
    let mut fixedlen = source_len
        .wrapping_add(source_len >> 3)
        .wrapping_add(source_len >> 8)
        .wrapping_add(source_len >> 9)
        .wrapping_add(4);
    if fixedlen < source_len {
        fixedlen = u64::MAX;
    }
    // Worst case for stored blocks of length 127 (~4% + constant).
    let mut storelen = source_len
        .wrapping_add(source_len >> 5)
        .wrapping_add(source_len >> 7)
        .wrapping_add(source_len >> 11)
        .wrapping_add(7);
    if storelen < source_len {
        storelen = u64::MAX;
    }

    // Without stream parameters, return the larger bound plus a full wrapper.
    let s = match s {
        Some(s) => s,
        None => {
            let bound = if fixedlen > storelen {
                fixedlen
            } else {
                storelen
            };
            return bound.saturating_add(18);
        }
    };

    // Wrapper length depends on the framing.
    let wrap_abs = if s.wrap < 0 { -s.wrap } else { s.wrap };
    let wraplen: u64 = match wrap_abs {
        0 => 0,                                       // raw deflate
        1 => 6 + if s.strstart != 0 { 4 } else { 0 }, // zlib wrapper (+dict adler)
        #[cfg(feature = "gzip")]
        2 => {
            // gzip wrapper: 10-byte header + 8-byte trailer = 18, plus any
            // user-supplied optional fields.
            let mut w: u64 = 18;
            if let Some(ref h) = s.gzhead {
                if let Some(ref extra) = h.extra {
                    w += 2 + extra.len() as u64;
                }
                if let Some(ref name) = h.name {
                    // C counts every byte plus the terminating NUL.
                    w += name.len() as u64 + 1;
                }
                if let Some(ref comment) = h.comment {
                    w += comment.len() as u64 + 1;
                }
                if h.hcrc {
                    w += 2;
                }
            }
            w
        }
        _ => 18, // for completeness (and the gzip-disabled wrap==2 case)
    };

    // Non-default parameters: return one of the conservative bounds.
    if s.w_bits != 15 || s.hash_bits != 15 {
        let bound = if s.w_bits <= s.hash_bits && s.level != 0 {
            fixedlen
        } else {
            storelen
        };
        return bound.saturating_add(wraplen);
    }

    // Default settings: the tight bound (~0.03% overhead). The constant is
    // `13 - 6` (the +13 stream overhead, less the 6 already in the zlib base).
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13 - 6 + wraplen);
    if bound < source_len { u64::MAX } else { bound }
}

// ===========================================================================
// Phase L — header / copy / end (deflate.c L714-719, L1293-1377)
// ===========================================================================

/// Supply a gzip header to be written when a gzip stream is produced. Ports
/// `deflateSetHeader` (`deflate.c` L714-719).
///
/// Only valid for a gzip-wrapped stream (`wrap == 2`); returns
/// [`ZlibError::StreamError`] otherwise. Must be called before the first
/// [`deflate`] call so the header is emitted ahead of the compressed data.
#[cfg(feature = "gzip")]
pub(crate) fn deflate_set_header(
    s: &mut DeflateState,
    head: GzHeader,
) -> Result<ReturnCode, ZlibError> {
    if s.wrap != 2 {
        return Err(ZlibError::StreamError);
    }
    s.gzhead = Some(head);
    Ok(ReturnCode::Ok)
}

/// Duplicate a deflate stream's complete state. Ports `deflateCopy`
/// (`deflate.c` L1317-1377).
///
/// In C this hand-copies the struct and re-allocates and rewires every interior
/// buffer pointer. Here [`DeflateState`] owns its buffers as `Vec`/`Box` and
/// derives [`Clone`], so a deep, correctly-wired copy is a single `clone()`.
/// The clone copies the whole backing buffers (C copies only the live prefixes
/// such as `high_water` / `sym_next`); the surplus bytes are never read before
/// being overwritten, so the resulting compressed output is byte-identical.
pub(crate) fn deflate_copy(src: &DeflateState) -> Box<DeflateState> {
    Box::new(src.clone())
}

/// Finish using a deflate stream. Ports the status check of `deflateEnd`
/// (`deflate.c` L1293-1310).
///
/// The buffer deallocation that C performs explicitly is handled by
/// [`DeflateState`]'s [`Drop`] implementation (the caller drops the owning
/// `Box` after this returns). This function only reports the C status: ending a
/// stream that is still mid-block ([`DeflateStatus::Busy`]) yields
/// [`ZlibError::DataError`] (data was discarded), otherwise [`ReturnCode::Ok`].
pub(crate) fn deflate_end(s: &DeflateState) -> Result<ReturnCode, ZlibError> {
    if s.status == DeflateStatus::Busy {
        Err(ZlibError::DataError)
    } else {
        Ok(ReturnCode::Ok)
    }
}

// ===========================================================================
// Phase M — the idiomatic, safe `Deflate` wrapper (AAP §0.3.2)
// ===========================================================================

/// The result of a single [`Deflate::compress`] call.
///
/// Reports the deflate return code together with how many input bytes were
/// consumed and how many output bytes were produced during the call, so a
/// caller driving the streaming loop can advance its own cursors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeflateOutcome {
    /// The (non-negative) deflate status: [`ReturnCode::Ok`] for normal
    /// progress or [`ReturnCode::StreamEnd`] once a `Z_FINISH` stream is fully
    /// written.
    pub code: ReturnCode,
    /// Number of input bytes consumed from the `input` slice this call.
    pub consumed: usize,
    /// Number of output bytes written into the `output` slice this call.
    pub produced: usize,
}

/// A safe, owning DEFLATE compressor — the idiomatic Rust front end to the
/// engine (AAP §0.3.2).
///
/// `Deflate` wraps the heap-allocated engine `state` and exposes
/// `Result`/`Option`-returning methods instead of C integer codes. Buffers are
/// owned and freed automatically when the `Deflate` is dropped (RAII — there is
/// no explicit "end" call to remember). The compressed output is byte-identical
/// to C zlib for the same level, strategy, window, and flush sequence.
///
/// # Examples
///
/// ```
/// use zlib_rs::{Deflate, constants::FlushMode};
///
/// let mut d = Deflate::new(6).expect("level 6 is valid");
/// let input = b"hello, hello, hello, world!";
/// let mut output = vec![0u8; d.bound(input.len() as u64) as usize];
/// let out = d.compress(input, &mut output, FlushMode::Finish).expect("compress");
/// assert_eq!(out.consumed, input.len());
/// assert!(out.produced > 0);
/// ```
pub struct Deflate {
    state: Box<DeflateState>,
}

impl Deflate {
    /// Create a compressor at `level` (0–9, or `-1` for the default level 6)
    /// using the default method, window size, memory level, and strategy.
    ///
    /// Produces a zlib-wrapped (RFC 1950) stream. Returns
    /// [`ZlibError::StreamError`] for an out-of-range level.
    pub fn new(level: i32) -> Result<Self, ZlibError> {
        Ok(Self {
            state: deflate_init(level)?,
        })
    }

    /// Create a compressor with full control over `level`, `window_bits`,
    /// `mem_level`, and `strategy`.
    ///
    /// `window_bits` selects the framing exactly as zlib does: `8..=15` for the
    /// zlib wrapper, negative for raw DEFLATE, and `+16` for gzip (the latter
    /// only when the `gzip` feature is enabled). Returns
    /// [`ZlibError::StreamError`] for any out-of-range argument.
    pub fn with_options(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
    ) -> Result<Self, ZlibError> {
        Ok(Self {
            state: deflate_init2(level, Z_DEFLATED, window_bits, mem_level, strategy as i32)?,
        })
    }

    /// Compress `input` into `output`, applying `flush`.
    ///
    /// Returns a [`DeflateOutcome`] describing the return code and the number of
    /// bytes consumed/produced. Call repeatedly, advancing your own cursors by
    /// `consumed`/`produced`, until [`ReturnCode::StreamEnd`] is returned for a
    /// [`FlushMode::Finish`] stream. Returns [`ZlibError::BufError`] if no
    /// progress is possible (e.g. `output` is full).
    pub fn compress(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        flush: FlushMode,
    ) -> Result<DeflateOutcome, ZlibError> {
        let mut stream = DeflateStream {
            state: &mut self.state,
            input,
            in_next: 0,
            output,
            out_next: 0,
        };
        let code = deflate(&mut stream, flush)?;
        Ok(DeflateOutcome {
            code,
            consumed: stream.in_next,
            produced: stream.out_next,
        })
    }

    /// Reset the compressor to begin a fresh stream, keeping the allocated
    /// buffers and configuration. Equivalent to the C `deflateReset`.
    pub fn reset(&mut self) -> Result<(), ZlibError> {
        deflate_reset(&mut self.state)?;
        Ok(())
    }

    /// Set the compression dictionary (preset history). Must be called on a
    /// pristine stream before the first [`compress`](Self::compress). See
    /// `deflate_set_dictionary` for the exact preconditions.
    pub fn set_dictionary(&mut self, dictionary: &[u8]) -> Result<(), ZlibError> {
        deflate_set_dictionary(&mut self.state, dictionary)?;
        Ok(())
    }

    /// Copy up to `w_size` bytes of the current window history into
    /// `dictionary` (when `Some`), returning the number of history bytes
    /// available. Equivalent to the C `deflateGetDictionary`.
    pub fn get_dictionary(&self, dictionary: Option<&mut [u8]>) -> usize {
        let (_code, len) = deflate_get_dictionary(&self.state, dictionary);
        len
    }

    /// Change the compression `level` and `strategy` mid-stream.
    ///
    /// If a pending block must be flushed first, it is written into `output`;
    /// the number of bytes produced is returned. Returns
    /// [`ZlibError::BufError`] if `output` lacked room to flush the pending
    /// block (drain it and retry). Equivalent to the C `deflateParams`.
    pub fn params(
        &mut self,
        level: i32,
        strategy: i32,
        output: &mut [u8],
    ) -> Result<usize, ZlibError> {
        let empty_in: [u8; 0] = [];
        let mut stream = DeflateStream {
            state: &mut self.state,
            input: &empty_in,
            in_next: 0,
            output,
            out_next: 0,
        };
        deflate_params(&mut stream, level, strategy)?;
        Ok(stream.out_next)
    }

    /// Fine-tune the internal LZ match-search parameters. Equivalent to the C
    /// `deflateTune`.
    pub fn tune(
        &mut self,
        good_length: i32,
        max_lazy: i32,
        nice_length: i32,
        max_chain: i32,
    ) -> Result<(), ZlibError> {
        deflate_tune(
            &mut self.state,
            good_length,
            max_lazy,
            nice_length,
            max_chain,
        )?;
        Ok(())
    }

    /// Return an upper bound on the compressed size of `source_len` bytes for
    /// this stream's configuration. Equivalent to the C `deflateBound`.
    #[must_use]
    pub fn bound(&self, source_len: u64) -> u64 {
        deflate_bound(Some(&self.state), source_len)
    }

    /// Report `(pending_bytes, valid_bits)` — output bytes generated but not yet
    /// flushed, and bits in the partial byte. Equivalent to the C
    /// `deflatePending`.
    #[must_use]
    pub fn pending(&self) -> (u32, i32) {
        deflate_pending(&self.state)
    }

    /// Report how many bits of the last output byte were used. Equivalent to
    /// the C `deflateUsed`.
    #[must_use]
    pub fn used(&self) -> i32 {
        deflate_used(&self.state)
    }

    /// Insert `bits` low-order bits of `value` ahead of the next output, for
    /// bit-level stream composition. Equivalent to the C `deflatePrime`.
    pub fn prime(&mut self, bits: i32, value: i32) -> Result<(), ZlibError> {
        deflate_prime(&mut self.state, bits, value)?;
        Ok(())
    }

    /// Duplicate this compressor's complete state into an independent
    /// [`Deflate`]. Equivalent to the C `deflateCopy`.
    #[must_use]
    pub fn copy(&self) -> Self {
        Self {
            state: deflate_copy(&self.state),
        }
    }

    /// Report the engine status as a `Result`: `Err(ZlibError::DataError)` if
    /// the stream is ended while still mid-block (data would be discarded),
    /// otherwise `Ok(())`. Buffer deallocation happens via [`Drop`]; this mirrors
    /// only the status semantics of the C `deflateEnd`.
    pub fn end(&self) -> Result<(), ZlibError> {
        deflate_end(&self.state)?;
        Ok(())
    }

    /// Supply a gzip header to be emitted for a gzip-wrapped stream. Must be
    /// called before the first [`compress`](Self::compress). Equivalent to the
    /// C `deflateSetHeader`; only available under the `gzip` feature.
    #[cfg(feature = "gzip")]
    pub fn set_header(&mut self, head: GzHeader) -> Result<(), ZlibError> {
        deflate_set_header(&mut self.state, head)?;
        Ok(())
    }
}
