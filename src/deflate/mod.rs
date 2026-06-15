//! DEFLATE engine orchestrator — the module root that ties the compression
//! engine together and exposes the public `deflate*` API.
//!
//! This is the safe-Rust port of `deflate.c` (the public deflate API plus the
//! `deflate()` main loop) and the `FLUSH_BLOCK`/`RANK`/`HCRC_UPDATE` macros and
//! status sentinels of `deflate.h`. It wires up the foundation submodules
//! (`state`, `trees`, `strategy`) and the five per-level strategy modules
//! (`stored`/`fast`/`slow`/`rle`/`huff`), defines the header-emission state
//! machine ([`DeflateStatus`]), the `flush_block` helpers the strategy files
//! import, every public `deflate*` orchestration function, and the idiomatic
//! [`Deflate`] wrapper re-exported by the crate root.
//!
//! # Layout
//!
//! | Submodule        | C source     | Responsibility                                          |
//! |------------------|--------------|---------------------------------------------------------|
//! | [`mod@state`]    | `deflate.h`  | [`DeflateState`] + LZ77 plumbing (window/hash/read_buf)  |
//! | [`mod@strategy`] | `deflate.c`  | Per-level `CONFIG_TABLE` + strategy dispatch             |
//! | [`mod@trees`]    | `trees.c`    | Huffman tree build/emit, static tables, bit output       |
//! | [`mod@stored`]   | `deflate.c`  | `deflate_stored` — store-only inner loop (level 0)       |
//! | [`mod@fast`]     | `deflate.c`  | `deflate_fast` — greedy matcher (levels 1–3)             |
//! | [`mod@slow`]     | `deflate.c`  | `deflate_slow` — lazy matcher (levels 4–9)               |
//! | [`mod@rle`]      | `deflate.c`  | `deflate_rle` — run-length-only (`Z_RLE`)                |
//! | [`mod@huff`]     | `deflate.c`  | `deflate_huff` — Huffman-only (`Z_HUFFMAN_ONLY`)         |
//!
//! # Byte-exact wire-format authority
//!
//! This file is the byte-exact authority for everything *outside* the
//! LZ77/Huffman core: the zlib (RFC 1950) and gzip (RFC 1952) framing, the FLG
//! check byte, the preset-dictionary handshake, the `Z_SYNC_FLUSH`/
//! `Z_FULL_FLUSH` empty-block markers, and the stream trailers. A single wrong
//! byte here breaks interoperability with C zlib, so the header emission, block
//! dispatch, flush handling, and trailer emission mirror `deflate.c` exactly
//! (AAP §0.6.1, §0.6.7, §0.7.1).
//!
//! ## The Adler-32 / CRC-32 initialization quirk
//!
//! C seeds the running checksum with `adler32(0L, Z_NULL, 0)` (which returns
//! `1`) and `crc32(0L, Z_NULL, 0)` (which returns `0`). The sibling
//! [`crate::checksum`] functions return the *recombined seed* for an empty
//! slice — `adler32(0, &[])` is `0`, not `1` — so every checksum
//! re-initialization here uses the **literal** `1` (zlib) or `0` (gzip) rather
//! than an empty-slice call. Getting this wrong produces a wrong trailer that
//! only surfaces when a real decompressor validates the stream.
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; `unsafe`
//!   lives only in `crate::ffi` and `crate::inflate::fast` (AAP §0.6.2). This
//!   file contains zero `unsafe`, raw pointers, or pointer arithmetic.
//! * **`no_std`-clean.** Only `core` and `alloc` are referenced, never `std`.

// `Box` is in the `std` prelude for hosted builds; under `no_std` it must be
// pulled from `alloc` (brought into scope at the crate root by
// `extern crate alloc;`). Gating the import avoids a duplicate-import warning
// with the prelude, mirroring the sibling `state`/`stream` modules.
#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use crate::constants::{
    DEF_MEM_LEVEL, FlushMode, MAX_MEM_LEVEL, MAX_WBITS, Strategy, Z_DEFAULT_STRATEGY, Z_DEFLATED,
    Z_FIXED, Z_HUFFMAN_ONLY, resolve_level,
};
use crate::deflate::state::{BUF_SIZE, BlockState, DeflateStream, MIN_MATCH};
use crate::error::{ReturnCode, ZlibError};
use crate::stream::StreamState;

// Re-export the persistent engine state at the module root so it resolves as
// `crate::deflate::DeflateState` for the crate root and the (forthcoming) FFI
// shim, mirroring zlib.h's flat namespace. This is also the canonical name used
// within this file. (`pub(crate)` because `DeflateState` itself is crate-private
// internal plumbing — the public surface is the `Deflate` wrapper.) The
// `strategy`/`trees` submodules are reached through the `mod` declarations
// below, so they need no `use`.
pub(crate) use state::DeflateState;

#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// ===========================================================================
// Phase A — Module declarations
// ===========================================================================
//
// All submodules are `pub(crate)`: the engine is internal plumbing reached only
// through the idiomatic `Deflate` wrapper and (later) the FFI shim. `strategy`
// references `crate::deflate::stored::deflate_stored` etc., and the five
// strategy files import `crate::deflate::{flush_block, flush_block_only}` from
// here, so each must be visible crate-wide.

pub(crate) mod state;
pub(crate) mod strategy;
pub(crate) mod trees;

pub(crate) mod fast;
pub(crate) mod huff;
pub(crate) mod rle;
pub(crate) mod slow;
pub(crate) mod stored;

// ===========================================================================
// Phase B — DeflateStatus: the header-emission state machine (deflate.h)
// ===========================================================================

/// Header-emission state machine for the DEFLATE engine — the safe-Rust
/// replacement for the integer `status` field and its `*_STATE` sentinel
/// `#define`s in `deflate.h` (AAP §0.6.1).
///
/// The discriminants are pinned to the canonical zlib sentinel values so the
/// state semantics line up exactly with the C engine (and remain recognizable
/// when debugging against a C reference). The enum is evaluated with exhaustive
/// `match`, which both removes the need for C's `deflateStateCheck` status
/// guard (an enum can never hold an out-of-range value) and lets the compiler
/// prove every state transition is handled.
///
/// State flow (from `deflate.h`):
///
/// * [`Init`](DeflateStatus::Init) → [`Busy`](DeflateStatus::Busy) — emit the
///   zlib (RFC 1950) two-byte header, then compress.
/// * [`Gzip`](DeflateStatus::Gzip) →
///   [`Extra`](DeflateStatus::Extra)/[`Busy`](DeflateStatus::Busy) — emit the
///   gzip (RFC 1952) header, optionally followed by the extra/name/comment/HCRC
///   fields.
/// * [`Extra`](DeflateStatus::Extra) → [`Name`](DeflateStatus::Name) →
///   [`Comment`](DeflateStatus::Comment) → [`Hcrc`](DeflateStatus::Hcrc) →
///   [`Busy`](DeflateStatus::Busy) — the optional gzip header fields.
/// * [`Busy`](DeflateStatus::Busy) → [`Finish`](DeflateStatus::Finish) —
///   compression in progress, then the stream trailer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub(crate) enum DeflateStatus {
    /// `INIT_STATE` (42): about to emit the zlib wrapper header.
    Init = 42,
    /// `GZIP_STATE` (57): about to emit the gzip wrapper header.
    Gzip = 57,
    /// `EXTRA_STATE` (69): emitting the gzip header's optional extra field.
    Extra = 69,
    /// `NAME_STATE` (73): emitting the gzip header's optional file name.
    Name = 73,
    /// `COMMENT_STATE` (91): emitting the gzip header's optional comment.
    Comment = 91,
    /// `HCRC_STATE` (103): emitting the gzip header's optional CRC-16.
    Hcrc = 103,
    /// `BUSY_STATE` (113): header complete; compressing the payload.
    Busy = 113,
    /// `FINISH_STATE` (666): payload complete; emitting the stream trailer.
    Finish = 666,
}

// ===========================================================================
// Phase C — Small helpers and constants
// ===========================================================================

/// `FLG.FDICT` bit set in the zlib header when a preset dictionary is active
/// (`deflate.c` `PRESET_DICT`).
const PRESET_DICT: u32 = 0x20;

/// Originating operating-system byte written in the gzip header when no
/// user-supplied OS is given (`zutil.h` `OS_CODE`).
///
/// zlib derives `OS_CODE` from the build platform; this port fixes it at `3`
/// (Unix) so the emitted gzip header is byte-identical regardless of the host
/// the crate is compiled on (AAP §0.6.1 byte-exactness).
const OS_CODE: u8 = 3;

/// Rank a flush mode for the "avoid duplicate consecutive flushes" check —
/// the safe-Rust port of `deflate.h`'s `RANK(f)` macro:
///
/// ```c
/// #define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))
/// ```
///
/// This orders `Z_BLOCK` (5) between `Z_NO_FLUSH` (0) and `Z_PARTIAL_FLUSH`
/// (1) so that a `Z_BLOCK` request is not mistaken for a stronger flush than a
/// partial flush when `deflate()` decides whether a repeated call has anything
/// to do.
#[inline]
fn rank(f: i32) -> i32 {
    f * 2 - if f > 4 { 9 } else { 0 }
}

// ===========================================================================
// Phase D — flush_block / flush_block_only (imported by the 5 strategy files)
// ===========================================================================

/// `FLUSH_BLOCK_ONLY` — emit the current block and push the resulting bytes to
/// the output, **without** the output-exhaustion early return that
/// [`flush_block`] adds.
///
/// The safe-Rust port of C's `FLUSH_BLOCK_ONLY` macro (`deflate.c`):
///
/// ```c
/// #define FLUSH_BLOCK_ONLY(s, last) { \
///    _tr_flush_block(s, (s->block_start >= 0L ? \
///                    (charf *)&s->window[(unsigned)s->block_start] : \
///                    (charf *)Z_NULL), \
///                 (ulg)((long)s->strstart - s->block_start), \
///                 (last)); \
///    s->block_start = s->strstart; \
///    flush_pending(s->strm); \
/// }
/// ```
///
/// It (1) hands the just-accumulated block to
/// [`tr_flush_block`](state::DeflateState::tr_flush_block), which selects the
/// smallest of the {stored, static, dynamic} encodings and writes it into
/// `pending_buf`; (2) advances `block_start` to `strstart`, marking the start of
/// the next block; and (3) drains `pending_buf` into the caller's output buffer
/// via [`flush_pending`](state::DeflateStream::flush_pending).
///
/// `stored_len = strstart - block_start` is the length of the block's raw window
/// bytes. When `block_start >= 0` those bytes are the candidate payload for a
/// stored block; `tr_flush_block` copies them **only** if it actually selects a
/// stored block. C passes a pointer straight into `window`; because
/// `tr_flush_block` borrows the engine state mutably, the candidate payload is
/// copied into a short-lived buffer first so the shared window slice does not
/// alias the mutably-borrowed state — the emitted bytes are byte-identical
/// either way. A negative `block_start` is the C `-1` sentinel (no pending
/// window payload) and passes `None`, exactly as C passes `Z_NULL`.
pub(crate) fn flush_block_only(s: &mut DeflateStream, last: bool) {
    let block_start = s.state.block_start;
    let stored_len = (s.state.strstart as isize - block_start) as usize;

    if block_start >= 0 {
        let start = block_start as usize;
        // `tr_flush_block` borrows `*s.state` mutably and would also need a
        // shared borrow of `s.state.window` for the stored-block payload — an
        // alias C avoids only because it uses raw pointers. Copy the candidate
        // payload out first; `tr_flush_block` consumes it solely when it selects
        // a stored block, so this keeps the emitted bytes identical to C while
        // staying in safe Rust.
        let payload = s.state.window[start..start + stored_len].to_vec();
        s.state.tr_flush_block(Some(&payload), stored_len, last);
    } else {
        // C `-1` sentinel: no window payload is available for a stored block.
        s.state.tr_flush_block(None, stored_len, last);
    }

    // Mark the start of the next block, then push the encoded bytes out.
    s.state.block_start = s.state.strstart as isize;
    s.flush_pending();
}

/// `FLUSH_BLOCK` — [`flush_block_only`] followed by the output-exhaustion early
/// return; the helper the per-level inner loops
/// ([`fast`]/[`slow`]/[`rle`]/[`huff`]) invoke at every block boundary.
///
/// The safe-Rust port of C's `FLUSH_BLOCK` macro (`deflate.c`):
///
/// ```c
/// #define FLUSH_BLOCK(s, last) { \
///    FLUSH_BLOCK_ONLY(s, last); \
///    if (s->strm->avail_out == 0) return (last) ? finish_started : need_more; \
/// }
/// ```
///
/// Rust has no macro-level `return`, so the early exit is encoded in the return
/// value: `Some(bstate)` means "C's macro would have returned `bstate`" and the
/// caller must propagate it
/// (`if let Some(bs) = flush_block(s, last) { return bs; }`), while `None` means
/// "continue". When the output buffer is full after the flush, the result is
/// [`BlockState::FinishStarted`] for the final block and
/// [`BlockState::NeedMore`] otherwise.
#[must_use]
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

// ===========================================================================
// Phase E — hcrc_update (gzip header CRC folding)
// ===========================================================================

/// `HCRC_UPDATE(beg)` — fold the gzip header bytes written since offset `beg`
/// into the running header CRC, when the user header requested a header CRC.
///
/// The safe-Rust port of C's `HCRC_UPDATE` macro (`deflate.c` L973-978):
///
/// ```c
/// #define HCRC_UPDATE(beg) \
///     do { \
///         if (s->gzhead->hcrc && s->pending > (beg)) \
///             strm->adler = crc32_z(strm->adler, s->pending_buf + (beg), \
///                                   s->pending - (beg)); \
///     } while (0)
/// ```
///
/// The "fold this slice into the CRC" condition is evaluated up front (which
/// releases the borrow of `gzhead`) and only then is `adler` reassigned, so the
/// whole helper stays in safe Rust with no overlapping borrows.
#[cfg(feature = "gzip")]
#[inline]
fn hcrc_update(s: &mut DeflateState, beg: usize) {
    let do_update = matches!(&s.gzhead, Some(h) if h.hcrc) && s.pending > beg;
    if do_update {
        s.adler = crate::checksum::crc32(s.adler, &s.pending_buf[beg..s.pending]);
    }
}

// ===========================================================================
// Phase F — deflate(): the engine main loop (deflate.c L981-1290)
// ===========================================================================

/// Compress as much input as possible and produce as much output as the
/// caller's buffer allows — the safe-Rust port of C `deflate()`
/// (`deflate.c` L981-1290).
///
/// This is the heart of the engine: it validates the call, emits the zlib or
/// gzip wrapper header on the first invocation, dispatches to the per-level
/// inner loop ([`strategy::deflate_dispatch`]), handles the `Z_*_FLUSH`
/// block-boundary markers, and finally emits the stream trailer on `Z_FINISH`.
/// Every byte it writes to `pending_buf` is part of the wire format and matches
/// C zlib exactly (AAP §0.6.1).
///
/// # Returns
///
/// * `Ok(ReturnCode::Ok)` — progress was made (the C `Z_OK`).
/// * `Ok(ReturnCode::StreamEnd)` — the stream is complete (the C
///   `Z_STREAM_END`), only on `Z_FINISH` once the trailer is fully written.
/// * `Err(ZlibError::StreamError)` — inconsistent call, e.g. more input after a
///   prior `Z_FINISH` switched away (the C `Z_STREAM_ERROR`).
/// * `Err(ZlibError::BufError)` — no progress is possible: empty output buffer,
///   or a repeated no-op flush with no input and no pending output (the C
///   `Z_BUF_ERROR`).
///
/// The FFI shim maps these onto the exact C integer codes via
/// [`ReturnCode::as_c_int`]/[`ZlibError::as_c_int`].
pub(crate) fn deflate(s: &mut DeflateStream, flush: FlushMode) -> Result<ReturnCode, ZlibError> {
    // --- Validation prologue (deflate.c L985-1026) ------------------------
    //
    // The C `flush < 0 || flush > Z_BLOCK` and NULL-pointer checks are
    // discharged by the type system: `FlushMode` is always a valid mode, and
    // slices can never be null. The FFI shim performs the raw-pointer/NULL
    // validation before constructing a `DeflateStream`.
    if s.state.status == DeflateStatus::Finish && flush != FlushMode::Finish {
        return Err(ZlibError::StreamError);
    }
    if s.avail_out() == 0 {
        return Err(ZlibError::BufError);
    }

    let old_flush = s.state.last_flush;
    s.state.last_flush = flush as i32;

    // Flush as much pending output as possible.
    if s.state.pending != 0 {
        s.flush_pending();
        if s.avail_out() == 0 {
            // Since avail_out is 0, deflate will be called again, so set
            // last_flush to -1 to avoid the BUF_ERROR "no progress" path below.
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    } else if s.avail_in() == 0
        && rank(flush as i32) <= rank(old_flush)
        && flush != FlushMode::Finish
    {
        // No input, no pending output, and this flush is no stronger than the
        // previous one: there is nothing to do and no progress can be made.
        return Err(ZlibError::BufError);
    }

    // The user must not provide more input after the first FINISH.
    if s.state.status == DeflateStatus::Finish && s.avail_in() != 0 {
        return Err(ZlibError::BufError);
    }

    // --- zlib header: INIT_STATE (deflate.c L1029-1064) -------------------
    if s.state.status == DeflateStatus::Init && s.state.wrap == 0 {
        // Raw deflate (no wrapper): skip straight to compressing.
        s.state.status = DeflateStatus::Busy;
    }
    if s.state.status == DeflateStatus::Init {
        let w_bits = s.state.w_bits;
        let mut header: u32 = (Z_DEFLATED as u32 + ((w_bits - 8) << 4)) << 8;
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
            header |= PRESET_DICT;
        }
        // Make the 16-bit header a multiple of 31 (the FCHECK constraint).
        header += 31 - (header % 31);

        s.state.put_short_msb(header as u16);

        // If a preset dictionary is active, emit its Adler-32 (set by
        // `deflate_set_dictionary`), MSB-first.
        if s.state.strstart != 0 {
            let adler = s.state.adler;
            s.state.put_short_msb((adler >> 16) as u16);
            s.state.put_short_msb((adler & 0xffff) as u16);
        }
        // Re-seed the running Adler-32 for the payload. C calls
        // `adler32(0L, Z_NULL, 0)`, which returns 1 — use the literal (the
        // Adler/CRC init quirk; never an empty-slice call).
        s.state.adler = 1;
        s.state.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer, so push the
        // header bytes out now.
        s.flush_pending();
        if s.state.pending != 0 {
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }

    // --- gzip header: GZIP_STATE … HCRC_STATE (deflate.c L1065-1208) ------
    #[cfg(feature = "gzip")]
    {
        // GZIP_STATE (L1066-1116): the fixed 10-byte header, optionally
        // followed by the extra/name/comment/HCRC fields.
        if s.state.status == DeflateStatus::Gzip {
            // Re-seed the running CRC-32 for the gzip body. C calls
            // `crc32(0L, Z_NULL, 0)`, which returns 0 — literal (the quirk).
            s.state.adler = 0;
            s.state.put_byte(31);
            s.state.put_byte(139);
            s.state.put_byte(8);

            // XFL byte, shared by both header variants.
            let xfl: u8 = if s.state.level == 9 {
                2
            } else if s.state.strategy >= Z_HUFFMAN_ONLY || s.state.level < 2 {
                4
            } else {
                0
            };

            if s.state.gzhead.is_none() {
                // No user header: the minimal fixed gzip header.
                s.state.put_byte(0); // FLG
                s.state.put_byte(0); // MTIME, 4 bytes, all zero
                s.state.put_byte(0);
                s.state.put_byte(0);
                s.state.put_byte(0);
                s.state.put_byte(xfl);
                s.state.put_byte(OS_CODE);
                s.state.status = DeflateStatus::Busy;

                // Compression must start with an empty pending buffer.
                s.flush_pending();
                if s.state.pending != 0 {
                    s.state.last_flush = -1;
                    return Ok(ReturnCode::Ok);
                }
            } else {
                // User header: snapshot the scalar fields into locals so the
                // `put_byte`/fold calls don't hold a borrow of `gzhead` while
                // mutating the rest of the state. (The `& 0xffff` on the extra
                // length mirrors the C `uInt` length field exactly.)
                let (text, hcrc, time, os, has_extra, extra_len, has_name, has_comment) = {
                    let h = s.state.gzhead.as_ref().unwrap();
                    (
                        h.text,
                        h.hcrc,
                        h.time,
                        h.os,
                        h.extra.is_some(),
                        h.extra.as_ref().map_or(0usize, |e| e.len()),
                        h.name.is_some(),
                        h.comment.is_some(),
                    )
                };

                let flg: u8 = u8::from(text)
                    + (if hcrc { 2 } else { 0 })
                    + (if has_extra { 4 } else { 0 })
                    + (if has_name { 8 } else { 0 })
                    + (if has_comment { 16 } else { 0 });
                s.state.put_byte(flg);
                s.state.put_byte((time & 0xff) as u8);
                s.state.put_byte(((time >> 8) & 0xff) as u8);
                s.state.put_byte(((time >> 16) & 0xff) as u8);
                s.state.put_byte(((time >> 24) & 0xff) as u8);
                s.state.put_byte(xfl);
                s.state.put_byte((os & 0xff) as u8);
                if has_extra {
                    s.state.put_byte((extra_len & 0xff) as u8);
                    s.state.put_byte(((extra_len >> 8) & 0xff) as u8);
                }
                if hcrc {
                    // Fold the header written so far into the CRC.
                    let pending = s.state.pending;
                    s.state.adler =
                        crate::checksum::crc32(s.state.adler, &s.state.pending_buf[0..pending]);
                }
                s.state.gzindex = 0;
                s.state.status = DeflateStatus::Extra;
            }
        }

        // EXTRA_STATE (L1117-1143): the optional extra field.
        if s.state.status == DeflateStatus::Extra {
            let extra = s.state.gzhead.as_ref().and_then(|h| h.extra.clone());
            if let Some(extra) = extra {
                // C bounds the copy by the 16-bit length field.
                let extra_count = extra.len() & 0xffff;
                let mut beg = s.state.pending; // start of not-yet-hashed bytes
                while s.state.gzindex < extra_count {
                    if s.state.pending == s.state.pending_buf_size {
                        hcrc_update(s.state, beg);
                        s.flush_pending();
                        if s.state.pending != 0 {
                            s.state.last_flush = -1;
                            return Ok(ReturnCode::Ok);
                        }
                        beg = 0;
                    }
                    let b = extra[s.state.gzindex];
                    s.state.put_byte(b);
                    s.state.gzindex += 1;
                }
                hcrc_update(s.state, beg);
                s.state.gzindex = 0;
            }
            s.state.status = DeflateStatus::Name;
        }

        // NAME_STATE (L1144-1165): the optional NUL-terminated file name.
        if s.state.status == DeflateStatus::Name {
            let name = s.state.gzhead.as_ref().and_then(|h| h.name.clone());
            if let Some(name) = name {
                let name_len = name.len();
                let mut beg = s.state.pending;
                // do/while: emit name bytes, then a terminating NUL.
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
                    let val = if s.state.gzindex < name_len {
                        name[s.state.gzindex]
                    } else {
                        0
                    };
                    s.state.put_byte(val);
                    s.state.gzindex += 1;
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s.state, beg);
                s.state.gzindex = 0;
            }
            s.state.status = DeflateStatus::Comment;
        }

        // COMMENT_STATE (L1166-1186): the optional NUL-terminated comment.
        if s.state.status == DeflateStatus::Comment {
            let comment = s.state.gzhead.as_ref().and_then(|h| h.comment.clone());
            if let Some(comment) = comment {
                let comment_len = comment.len();
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
                    let val = if s.state.gzindex < comment_len {
                        comment[s.state.gzindex]
                    } else {
                        0
                    };
                    s.state.put_byte(val);
                    s.state.gzindex += 1;
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s.state, beg);
            }
            s.state.status = DeflateStatus::Hcrc;
        }

        // HCRC_STATE (L1187-1208): the optional 2-byte header CRC.
        if s.state.status == DeflateStatus::Hcrc {
            let hcrc = matches!(&s.state.gzhead, Some(h) if h.hcrc);
            if hcrc {
                if s.state.pending + 2 > s.state.pending_buf_size {
                    s.flush_pending();
                    if s.state.pending != 0 {
                        s.state.last_flush = -1;
                        return Ok(ReturnCode::Ok);
                    }
                }
                let adler = s.state.adler;
                s.state.put_byte((adler & 0xff) as u8);
                s.state.put_byte(((adler >> 8) & 0xff) as u8);
                // Re-seed the running CRC-32 for the gzip body (literal 0).
                s.state.adler = 0;
            }
            s.state.status = DeflateStatus::Busy;

            // As in the zlib-header path, ensure the header leaves the buffer.
            s.flush_pending();
            if s.state.pending != 0 {
                s.state.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    }

    // --- Block dispatch (deflate.c L1211-1261) ----------------------------
    //
    // Run the per-level inner loop while there is input, buffered lookahead, or
    // a non-NoFlush request that has not yet finished.
    if s.avail_in() != 0
        || s.state.lookahead != 0
        || (flush != FlushMode::NoFlush && s.state.status != DeflateStatus::Finish)
    {
        // `deflate_dispatch` encodes the C `?:` chain:
        // level==0 → stored, Z_HUFFMAN_ONLY → huff, Z_RLE → rle, else the
        // per-level `CONFIG_TABLE[level].func` (fast/slow).
        let bstate = strategy::deflate_dispatch(s, flush);

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            s.state.status = DeflateStatus::Finish;
        }
        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if s.avail_out() == 0 {
                s.state.last_flush = -1; // avoid the BUF_ERROR path on the next call
            }
            // With avail_out == 0 and flush != NoFlush the same flush must be
            // re-issued; emitting nothing here ensures even a tiny output
            // buffer eventually receives at least one (empty) block.
            return Ok(ReturnCode::Ok);
        }
        if bstate == BlockState::BlockDone {
            if flush == FlushMode::PartialFlush {
                s.state.tr_align();
            } else if flush != FlushMode::Block {
                // Z_FULL_FLUSH or Z_SYNC_FLUSH: emit an empty stored block.
                s.state.tr_stored_block(None, 0, false);
                if flush == FlushMode::FullFlush {
                    // Forget the history so the stream can be restarted here.
                    s.state.clear_hash();
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

    // --- Trailer (deflate.c L1263-1289) -----------------------------------
    if flush != FlushMode::Finish {
        return Ok(ReturnCode::Ok);
    }
    if s.state.wrap <= 0 {
        // Raw deflate (or trailer already written): nothing more to emit.
        return Ok(ReturnCode::StreamEnd);
    }

    // gzip trailer (wrap == 2): CRC-32 then ISIZE, both LSB-first.
    #[cfg(feature = "gzip")]
    {
        if s.state.wrap == 2 {
            let adler = s.state.adler;
            s.state.put_byte((adler & 0xff) as u8);
            s.state.put_byte(((adler >> 8) & 0xff) as u8);
            s.state.put_byte(((adler >> 16) & 0xff) as u8);
            s.state.put_byte(((adler >> 24) & 0xff) as u8);
            // ISIZE is total_in modulo 2^32, LSB-first.
            let total = s.state.total_in;
            s.state.put_byte((total & 0xff) as u8);
            s.state.put_byte(((total >> 8) & 0xff) as u8);
            s.state.put_byte(((total >> 16) & 0xff) as u8);
            s.state.put_byte(((total >> 24) & 0xff) as u8);

            s.flush_pending();
            // Write the trailer only once: flip `wrap` negative.
            if s.state.wrap > 0 {
                s.state.wrap = -s.state.wrap;
            }
            return if s.state.pending != 0 {
                Ok(ReturnCode::Ok)
            } else {
                Ok(ReturnCode::StreamEnd)
            };
        }
    }

    // zlib trailer (wrap == 1): Adler-32, MSB-first.
    let adler = s.state.adler;
    s.state.put_short_msb((adler >> 16) as u16);
    s.state.put_short_msb((adler & 0xffff) as u16);
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

/// Initialize a deflate engine with full control over the window, memory
/// level, and strategy — the safe-Rust port of C `deflateInit2_`
/// (`deflate.c` L387-533).
///
/// `window_bits` is overloaded exactly as in zlib (AAP §0.7.1):
///
/// * `8..=15` — zlib wrapper (RFC 1950), `wrap = 1`.
/// * `-15..=-8` — raw DEFLATE (RFC 1951), no wrapper, `wrap = 0`.
/// * `24..=31` (i.e. `8..=15 + 16`) — gzip wrapper (RFC 1952), `wrap = 2`
///   (requires the `gzip` feature).
///
/// On success the returned [`DeflateState`] is fully allocated and reset, ready
/// for the first [`deflate`] call. Allocation uses the global allocator, which
/// aborts rather than returns on OOM, so the C `Z_MEM_ERROR` path
/// ([`ZlibError::MemError`]) is unreachable here and is produced only at the
/// FFI boundary if a custom fallible allocator is supplied.
///
/// # Errors
///
/// Returns [`ZlibError::StreamError`] for any invalid parameter combination
/// (bad level, method, window/memory bounds, strategy, or the
/// `window_bits == 8` raw-stream corner case), mirroring C's `Z_STREAM_ERROR`.
pub(crate) fn deflate_init2(
    level: i32,
    method: i32,
    mut window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Result<Box<DeflateState>, ZlibError> {
    // Z_DEFAULT_COMPRESSION (-1) resolves to level 6.
    let level = resolve_level(level);

    // Decode the `window_bits` overloading into a wrapper selector.
    let mut wrap = 1;
    if window_bits < 0 {
        // Negative: suppress the zlib wrapper (raw DEFLATE).
        if window_bits < -15 {
            return Err(ZlibError::StreamError);
        }
        wrap = 0;
        window_bits = -window_bits;
    }
    #[cfg(feature = "gzip")]
    {
        // > 15: request the gzip wrapper instead of zlib. (When the first
        // branch ran, `window_bits` is now in 8..=15, so this is never taken —
        // matching C's `else if`.)
        if window_bits > 15 {
            wrap = 2;
            window_bits -= 16;
        }
    }

    // Validate every parameter exactly as C does; any failure is Z_STREAM_ERROR.
    if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
        || method != Z_DEFLATED
        || !(8..=15).contains(&window_bits)
        || !(0..=9).contains(&level)
        || !(0..=Z_FIXED).contains(&strategy)
        || (window_bits == 8 && wrap != 1)
    {
        return Err(ZlibError::StreamError);
    }
    if window_bits == 8 {
        // A 256-byte window is not supported (historical zlib limitation);
        // bump it to a 512-byte window.
        window_bits = 9;
    }

    // `hash_bits = memLevel + 7`; `new_allocated` derives the rest of the hash
    // and literal-buffer geometry and allocates the owned working buffers.
    let hash_bits = mem_level as u32 + 7;
    let mut state = DeflateState::new_allocated(
        window_bits as u32,
        hash_bits,
        mem_level as u32,
        level,
        strategy,
        method as u8,
        wrap,
    );

    // `new_allocated` always leaves `status == Init`; `reset()` selects the
    // correct header-emission start state for `wrap` and runs the equivalent of
    // C `deflateReset` (deflateResetKeep + tr_init + lm_init).
    state.reset();
    Ok(Box::new(state))
}

/// Initialize a deflate engine with zlib's defaults — the safe-Rust port of C
/// `deflateInit_` (`deflate.c` L375-385).
///
/// Equivalent to [`deflate_init2`] with `method = Z_DEFLATED`,
/// `window_bits = MAX_WBITS` (15), `mem_level = DEF_MEM_LEVEL` (8), and
/// `strategy = Z_DEFAULT_STRATEGY`.
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
// Phase H — deflate_reset (deflate.c L704-711)
// ===========================================================================

/// Reset an engine to its just-initialized state without reallocating — the
/// safe-Rust port of C `deflateReset` (`deflate.c` L704-711).
///
/// Delegates to [`DeflateState`]'s [`StreamState::reset`] implementation, which
/// ports `deflateResetKeep` (accounting + checksum seed + header start state)
/// followed by `trees::tr_init` and `lm_init` (match parameters + window/hash
/// state). The checksum re-seed uses the literal `1`/`0` per the Adler/CRC init
/// quirk.
pub(crate) fn deflate_reset(s: &mut DeflateState) -> Result<ReturnCode, ZlibError> {
    s.reset();
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Phase I — dictionary (deflate.c L559-641)
// ===========================================================================

/// Initialize the compression dictionary from `dictionary` — the safe-Rust
/// port of C `deflateSetDictionary` (`deflate.c` L559-622).
///
/// Must be called before any [`deflate`] (the engine must still be in its
/// initial state), and only on raw or zlib streams — never gzip. For a zlib
/// stream the dictionary contributes to the stream Adler-32 (signalled to the
/// decompressor via the `FDICT` header bit and the dictionary's Adler-32, both
/// emitted by [`deflate`]'s `INIT_STATE` path).
///
/// # Errors
///
/// [`ZlibError::StreamError`] if the stream is gzip-wrapped, a zlib stream that
/// has already started, or already has buffered lookahead — mirroring C.
pub(crate) fn deflate_set_dictionary(
    s: &mut DeflateState,
    dictionary: &[u8],
) -> Result<ReturnCode, ZlibError> {
    let wrap = s.wrap;

    // A dictionary cannot be set on a gzip stream, on a zlib stream that has
    // left INIT_STATE, or once any input has been buffered.
    if wrap == 2 || (wrap == 1 && s.status != DeflateStatus::Init) || s.lookahead != 0 {
        return Err(ZlibError::StreamError);
    }

    // For a zlib stream, fold the whole dictionary into the running Adler-32
    // before the (windowed) tail is inserted.
    if wrap == 1 {
        s.adler = crate::checksum::adler32(s.adler, dictionary);
    }

    // Disable wrapping so the fill_window/read_buf path below does not re-fold
    // the dictionary tail into the checksum (C sets `s->wrap = 0`).
    s.wrap = 0;

    // Only the trailing `w_size` bytes of an over-long dictionary can fit in
    // the window.
    let mut dictionary = dictionary;
    if dictionary.len() >= s.w_size {
        if wrap == 0 {
            // A raw stream starts fresh, so discard any partial-block state.
            s.clear_hash();
            s.strstart = 0;
            s.block_start = 0;
            s.insert = 0;
        }
        let skip = dictionary.len() - s.w_size;
        dictionary = &dictionary[skip..];
    }

    // Insert the dictionary into the window and hash chains via a temporary
    // stream whose input is the dictionary and whose output is empty (the
    // insert path never produces output).
    {
        let mut empty_out: [u8; 0] = [];
        let mut strm = DeflateStream::new(s, dictionary, &mut empty_out);
        strm.fill_window();
        while strm.state.lookahead >= MIN_MATCH {
            let mut str = strm.state.strstart;
            let mut n = strm.state.lookahead - (MIN_MATCH - 1);
            loop {
                strm.state.insert_string(str);
                str += 1;
                n -= 1;
                if n == 0 {
                    break;
                }
            }
            strm.state.strstart = str;
            strm.state.lookahead = MIN_MATCH - 1;
            strm.fill_window();
        }
        strm.state.strstart += strm.state.lookahead;
        strm.state.block_start = strm.state.strstart as isize;
        strm.state.insert = strm.state.lookahead;
        strm.state.lookahead = 0;
        strm.state.match_length = MIN_MATCH - 1;
        strm.state.prev_length = MIN_MATCH - 1;
        strm.state.match_available = false;
    }

    // Restore the wrapper mode (the temporary `total_in` increments from the
    // dictionary reads are left as-is, exactly as C does; they do not affect
    // the emitted bytes).
    s.wrap = wrap;
    Ok(ReturnCode::Ok)
}

/// Return up to `w_size` bytes of the current sliding-window history — the
/// safe-Rust port of C `deflateGetDictionary` (`deflate.c` L625-641).
///
/// When `dictionary` is `Some`, the most recent history bytes are copied into
/// it; the returned `usize` is the number of history bytes available (and
/// copied when a buffer was supplied). When `dictionary` is `None`, only the
/// available length is reported. The [`ReturnCode`] is always
/// [`ReturnCode::Ok`] (the C function cannot fail once the state is valid).
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

/// Dynamically change the compression `level` and `strategy` mid-stream — the
/// safe-Rust port of C `deflateParams` (`deflate.c` L774-816).
///
/// If the new level/strategy selects a different inner-loop function (or a
/// different strategy) and compression has already begun, the current block is
/// flushed with [`FlushMode::Block`] before the parameters change so that the
/// switch happens on a clean block boundary.
///
/// Because the pre-flush calls [`deflate`], this operates on a
/// [`DeflateStream`] (with its input/output buffers), not a bare
/// [`DeflateState`].
///
/// # Errors
///
/// * [`ZlibError::StreamError`] for an invalid `level`/`strategy`, or if the
///   pre-flush itself reports a stream error.
/// * [`ZlibError::BufError`] if the pre-flush could not consume all input and
///   drain the pending block (the caller must provide a larger output buffer).
pub(crate) fn deflate_params(
    s: &mut DeflateStream,
    level: i32,
    strategy: i32,
) -> Result<ReturnCode, ZlibError> {
    // Z_DEFAULT_COMPRESSION (-1) resolves to level 6.
    let level = resolve_level(level);
    if !(0..=9).contains(&level) || !(0..=Z_FIXED).contains(&strategy) {
        return Err(ZlibError::StreamError);
    }

    let func = strategy::config_compress_fn(s.state.level);

    if (strategy != s.state.strategy
        || !strategy::same_compress_fn(func, strategy::config_compress_fn(level)))
        && s.state.last_flush != -2
    {
        // Flush the current block on a clean boundary. C only aborts on a
        // stream error here; a Z_BUF_ERROR from the flush is intentionally
        // *ignored* (the dedicated avail_in/lookahead check below decides
        // whether progress was actually possible).
        if let Err(ZlibError::StreamError) = deflate(s, FlushMode::Block) {
            return Err(ZlibError::StreamError);
        }
        if s.avail_in() != 0
            || (s.state.strstart as isize - s.state.block_start) as usize + s.state.lookahead != 0
        {
            return Err(ZlibError::BufError);
        }
    }

    if s.state.level != level {
        // Switching away from store-only (level 0) after matches were emitted
        // requires rebuilding the hash chains for the deferred history.
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

/// Fine-tune the internal LZ77 match parameters — the safe-Rust port of C
/// `deflateTune` (`deflate.c` L819-830).
///
/// An expert knob for benchmarking and research; it overrides the per-level
/// `good_length`/`max_lazy`/`nice_length`/`max_chain` values chosen by the
/// configuration table. Always returns [`ReturnCode::Ok`] once the state is
/// valid.
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
// Phase K — bound / pending / prime (deflate.c L722-771, L856-928)
// ===========================================================================

/// Compute an upper bound on the compressed size of `source_len` input bytes —
/// the safe-Rust port of C `deflateBound` (`deflate.c` L856-928).
///
/// When `s` is `Some`, the bound accounts for the configured wrapper and
/// window/memory level. When `s` is `None` (no initialized state), the most
/// conservative bound across the fixed and stored encodings plus the largest
/// possible wrapper is returned. The saturating arithmetic mirrors C's
/// overflow handling exactly: any intermediate wrap clamps the result to
/// [`u64::MAX`] (the C `(uLong)-1`).
pub(crate) fn deflate_bound(s: Option<&DeflateState>, source_len: u64) -> u64 {
    // Upper bound when emitting fixed Huffman blocks.
    let mut fixedlen = source_len
        .wrapping_add(source_len >> 3)
        .wrapping_add(source_len >> 8)
        .wrapping_add(source_len >> 9)
        .wrapping_add(4);
    if fixedlen < source_len {
        fixedlen = u64::MAX;
    }
    // Upper bound when emitting stored (uncompressed) blocks.
    let mut storelen = source_len
        .wrapping_add(source_len >> 5)
        .wrapping_add(source_len >> 7)
        .wrapping_add(source_len >> 11)
        .wrapping_add(7);
    if storelen < source_len {
        storelen = u64::MAX;
    }

    // Without a state we cannot know the wrapper or window/memory level, so
    // return the larger conservative bound plus the maximum wrapper overhead.
    let s = match s {
        Some(s) => s,
        None => {
            let bound = fixedlen.max(storelen);
            return bound.saturating_add(18);
        }
    };

    // Wrapper overhead for the configured stream framing.
    let wrap = if s.wrap < 0 { -s.wrap } else { s.wrap };
    let wraplen: u64 = match wrap {
        0 => 0,
        1 => 6 + if s.strstart != 0 { 4 } else { 0 },
        #[cfg(feature = "gzip")]
        2 => gzip_wrap_len(s),
        _ => 18,
    };

    // A non-default window or memory level forces the conservative bound (the
    // tight default formula below is only valid for w_bits==15 && hash_bits==15).
    if s.w_bits != 15 || s.hash_bits != 15 {
        let bound = if s.w_bits <= s.hash_bits && s.level != 0 {
            fixedlen
        } else {
            storelen
        };
        return bound.saturating_add(wraplen);
    }

    // Tight default bound (matches `compressBound` minus the 6-byte zlib wrapper
    // already accounted for in `wraplen`).
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13 - 6)
        .wrapping_add(wraplen);
    if bound < source_len { u64::MAX } else { bound }
}

/// gzip wrapper-length contribution for [`deflate_bound`] — the safe-Rust port
/// of the `case 2` arm of C `deflateBound` (`deflate.c` L880-911).
///
/// The base gzip wrapper is 18 bytes (10-byte header + 8-byte trailer). A
/// user-supplied header adds: 2 bytes plus the extra field, the file name plus
/// its NUL terminator, the comment plus its NUL terminator, and 2 bytes when a
/// header CRC is requested.
#[cfg(feature = "gzip")]
fn gzip_wrap_len(s: &DeflateState) -> u64 {
    let mut wraplen: u64 = 18;
    if let Some(h) = s.gzhead.as_ref() {
        if let Some(extra) = h.extra.as_ref() {
            wraplen += 2 + extra.len() as u64;
        }
        if let Some(name) = h.name.as_ref() {
            // C counts each byte including the trailing NUL terminator.
            wraplen += name.len() as u64 + 1;
        }
        if let Some(comment) = h.comment.as_ref() {
            wraplen += comment.len() as u64 + 1;
        }
        if h.hcrc {
            wraplen += 2;
        }
    }
    wraplen
}

/// Report the number of bytes and bits of output generated but not yet flushed
/// — the safe-Rust port of C `deflatePending` (`deflate.c` L722-734).
///
/// Returns `(pending_bytes, pending_bits)` where `pending_bits` is the count of
/// bits in the partially-filled bit buffer (`bi_valid`). The FFI shim writes
/// these into the caller's out-parameters.
pub(crate) fn deflate_pending(s: &DeflateState) -> (u32, i32) {
    (s.pending as u32, s.bi_valid)
}

/// Insert `bits` low-order bits of `value` directly into the output bit buffer
/// — the safe-Rust port of C `deflatePrime` (`deflate.c` L745-771).
///
/// Used to inject bits ahead of the compressed data (for example by
/// `inflate`/`deflate` bridging tools). Bits are consumed `Buf_size`-at-a-time,
/// flushing whole bytes to `pending_buf` via
/// [`tr_flush_bits`](state::DeflateState::tr_flush_bits) as the buffer fills.
///
/// # Errors
///
/// [`ZlibError::BufError`] if `bits` is out of range (`0..=16`) or there is not
/// enough room left in the symbol buffer to safely flush the primed bits.
pub(crate) fn deflate_prime(
    s: &mut DeflateState,
    bits: i32,
    value: i32,
) -> Result<ReturnCode, ZlibError> {
    // `(Buf_size + 7) >> 3` == 2 bytes: the most a flush of the bit buffer can
    // append. Refuse if the symbol region (which starts at `lit_bufsize`) would
    // be overrun by `pending_out + 2`.
    if !(0..=16).contains(&bits) || s.lit_bufsize < s.pending_out + (((BUF_SIZE + 7) >> 3) as usize)
    {
        return Err(ZlibError::BufError);
    }

    let mut bits = bits;
    let mut value = value;
    loop {
        let put = core::cmp::min(BUF_SIZE - s.bi_valid, bits);
        s.bi_buf |= ((value & ((1 << put) - 1)) << s.bi_valid) as u16;
        s.bi_valid += put;
        s.tr_flush_bits();
        value >>= put;
        bits -= put;
        if bits == 0 {
            break;
        }
    }
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Phase L — header / copy / end (deflate.c L714-719, L1293-1377)
// ===========================================================================

/// Supply a gzip header for the stream — the safe-Rust port of C
/// `deflateSetHeader` (`deflate.c` L714-719).
///
/// Only valid on a gzip-wrapped stream (`wrap == 2`, i.e. created with
/// `window_bits` in `24..=31`). The header's optional extra/name/comment fields
/// and CRC flag are emitted by [`deflate`]'s gzip-header state machine on the
/// first call.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if the stream is not gzip-wrapped, mirroring C.
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

/// Duplicate a deflate engine, history and all — the safe-Rust port of C
/// `deflateCopy` (`deflate.c` L1317-1377).
///
/// C must hand-copy the state struct and rewire every internal pointer into the
/// freshly-allocated buffers. Here [`DeflateState`] owns its `Vec`/`Box`
/// buffers, so a derived `Clone` performs the deep copy correctly with no
/// pointers to fix up — making the notoriously bug-prone C routine a one-liner.
pub(crate) fn deflate_copy(src: &DeflateState) -> Box<DeflateState> {
    Box::new(src.clone())
}

/// Finalize a deflate engine — the safe-Rust port of C `deflateEnd`
/// (`deflate.c` L1293-1310).
///
/// The owned buffers are released by [`DeflateState`]'s `Drop` (the FFI shim
/// drops the `Box<DeflateState>` after calling this); this function only
/// reports C's "were we interrupted mid-compression?" status check.
///
/// # Errors
///
/// [`ZlibError::DataError`] if the engine was still in
/// [`DeflateStatus::Busy`] (a stream freed before `Z_FINISH`), mirroring C's
/// `Z_DATA_ERROR`.
pub(crate) fn deflate_end(s: &DeflateState) -> Result<ReturnCode, ZlibError> {
    if s.status == DeflateStatus::Busy {
        Err(ZlibError::DataError)
    } else {
        Ok(ReturnCode::Ok)
    }
}

// ===========================================================================
// Phase M — idiomatic Deflate wrapper (AAP §0.3.2)
// ===========================================================================

/// A safe, owning DEFLATE compressor — the idiomatic Rust front door to the
/// engine, re-exported at the crate root.
///
/// `Deflate` owns its [`DeflateState`] (a heap-allocated `Box`) and exposes a
/// `Result`/`Option`-based API in place of zlib's integer return codes. The
/// underlying owned buffers are released automatically when the `Deflate` is
/// dropped — there is no `deflateEnd` to remember to call (AAP §0.3.2,
/// §0.6.3).
///
/// # Examples
///
/// ```
/// use zlib_rs::{Deflate, FlushMode};
///
/// let mut c = Deflate::new(6).expect("level 6 is valid");
/// let input = b"hello, hello, hello, world!";
/// let mut out = [0u8; 64];
/// let outcome = c
///     .compress(input, &mut out, FlushMode::Finish)
///     .expect("compression succeeds");
/// assert_eq!(outcome.consumed, input.len());
/// assert!(outcome.produced > 0);
/// ```
pub struct Deflate {
    state: Box<DeflateState>,
}

/// The result of a single [`Deflate::compress`] (or [`Deflate::params`]) call.
///
/// Reports the engine's [`ReturnCode`] along with how many input bytes were
/// consumed and how many output bytes were produced during the call — the
/// idiomatic replacement for inspecting `avail_in`/`avail_out` deltas on a
/// `z_stream`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeflateOutcome {
    /// The non-negative outcome of the call ([`ReturnCode::Ok`] or
    /// [`ReturnCode::StreamEnd`]).
    pub code: ReturnCode,
    /// Number of input bytes consumed during the call.
    pub consumed: usize,
    /// Number of output bytes produced during the call.
    pub produced: usize,
}

impl Deflate {
    /// Create a compressor with the zlib wrapper and default window/memory
    /// settings at the given compression `level` (0..=9, or
    /// `Z_DEFAULT_COMPRESSION` = -1 for level 6).
    ///
    /// # Errors
    ///
    /// [`ZlibError::StreamError`] if `level` is out of range.
    pub fn new(level: i32) -> Result<Self, ZlibError> {
        Ok(Self {
            state: deflate_init2(
                level,
                Z_DEFLATED,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                Strategy::Default as i32,
            )?,
        })
    }

    /// Create a compressor with full control over the wrapper (`window_bits`),
    /// memory level, and [`Strategy`]. See [`deflate_init2`] for the
    /// `window_bits` overloading convention (zlib / raw / gzip).
    ///
    /// # Errors
    ///
    /// [`ZlibError::StreamError`] for any invalid parameter combination.
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

    /// Compress `input` into `output` with the given [`FlushMode`].
    ///
    /// Returns a [`DeflateOutcome`] describing how much was consumed/produced
    /// and the resulting [`ReturnCode`]. Drive a full stream by calling
    /// repeatedly until [`ReturnCode::StreamEnd`] is returned for
    /// [`FlushMode::Finish`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate`]'s errors ([`ZlibError::BufError`],
    /// [`ZlibError::StreamError`]).
    pub fn compress(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        flush: FlushMode,
    ) -> Result<DeflateOutcome, ZlibError> {
        let mut stream = DeflateStream::new(&mut self.state, input, output);
        let code = deflate(&mut stream, flush)?;
        Ok(DeflateOutcome {
            code,
            consumed: stream.in_next,
            produced: stream.out_next,
        })
    }

    /// Change the compression `level` and `strategy` mid-stream, flushing the
    /// current block through `input`/`output` if a strategy switch requires it.
    /// See [`deflate_params`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_params`]'s errors.
    pub fn params(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        level: i32,
        strategy: i32,
    ) -> Result<DeflateOutcome, ZlibError> {
        let mut stream = DeflateStream::new(&mut self.state, input, output);
        let code = deflate_params(&mut stream, level, strategy)?;
        Ok(DeflateOutcome {
            code,
            consumed: stream.in_next,
            produced: stream.out_next,
        })
    }

    /// Reset the compressor to its initial state, reusing the allocation. See
    /// [`deflate_reset`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_reset`]'s errors (none in practice).
    pub fn reset(&mut self) -> Result<(), ZlibError> {
        deflate_reset(&mut self.state)?;
        Ok(())
    }

    /// Set the compression dictionary (before the first [`compress`](Self::compress)).
    /// See [`deflate_set_dictionary`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_set_dictionary`]'s errors.
    pub fn set_dictionary(&mut self, dict: &[u8]) -> Result<(), ZlibError> {
        deflate_set_dictionary(&mut self.state, dict)?;
        Ok(())
    }

    /// Copy up to `w_size` bytes of sliding-window history into `dict` (when
    /// supplied) and return the number of history bytes available. See
    /// [`deflate_get_dictionary`].
    pub fn get_dictionary(&self, dict: Option<&mut [u8]>) -> usize {
        let (_, len) = deflate_get_dictionary(&self.state, dict);
        len
    }

    /// Return an upper bound on the compressed size of `source_len` input
    /// bytes for this stream's configuration. See [`deflate_bound`].
    pub fn bound(&self, source_len: u64) -> u64 {
        deflate_bound(Some(&self.state), source_len)
    }

    /// Fine-tune the internal LZ77 match parameters. See [`deflate_tune`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_tune`]'s errors (none in practice).
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

    /// Inject `bits` low-order bits of `value` into the output bit buffer. See
    /// [`deflate_prime`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_prime`]'s errors.
    pub fn prime(&mut self, bits: i32, value: i32) -> Result<(), ZlibError> {
        deflate_prime(&mut self.state, bits, value)?;
        Ok(())
    }

    /// Return the number of pending output `(bytes, bits)` not yet flushed.
    /// See [`deflate_pending`].
    #[must_use]
    pub fn pending(&self) -> (u32, i32) {
        deflate_pending(&self.state)
    }

    /// Supply a gzip header for the stream (gzip-wrapped streams only). See
    /// [`deflate_set_header`].
    ///
    /// # Errors
    ///
    /// Propagates [`deflate_set_header`]'s errors.
    #[cfg(feature = "gzip")]
    pub fn set_header(&mut self, head: GzHeader) -> Result<(), ZlibError> {
        deflate_set_header(&mut self.state, head)?;
        Ok(())
    }
}
