//! DEFLATE engine module root — shared types and submodule wiring.
//!
//! This is the module root for `crate::deflate`, the safe-Rust port of zlib's
//! compression engine (`deflate.c` + `deflate.h`). It declares the engine's
//! foundation submodules and defines the header-emission state machine
//! ([`DeflateStatus`]) that every part of the engine shares.
//!
//! # Layout
//!
//! | Submodule           | C source     | Responsibility                                            |
//! |---------------------|--------------|-----------------------------------------------------------|
//! | [`mod@state`]       | `deflate.h`  | [`DeflateState`](state::DeflateState) + LZ77 plumbing      |
//! | [`mod@strategy`]    | `deflate.c`  | Per-level [`CONFIG_TABLE`](strategy::CONFIG_TABLE) sizing  |
//! | [`mod@trees`]       | `trees.c`    | Huffman tree build/emit, static tables, bit-level output  |
//!
//! The per-level strategy inner loops (`deflate_stored`/`deflate_fast`/
//! `deflate_slow`/`deflate_rle`/`deflate_huff`), the strategy dispatch over
//! [`CONFIG_TABLE`](strategy::CONFIG_TABLE),
//! the `flush_block` helpers, and the public `deflate*` orchestration
//! (`deflate()`/`deflateInit2_`/`deflateEnd`, AAP §0.4.1) build on the
//! foundation declared here. They are layered on top of this root as the
//! deflate engine is completed; this file owns only the cross-cutting state
//! enum and the submodule declarations they all depend on.
//!
//! # `DeflateStatus` — the header-emission state machine
//!
//! The C implementation tracks header emission with an integer `status` field
//! on `deflate_state`, compared against eight sentinel `#define`s in
//! `deflate.h` (`INIT_STATE`=42 … `FINISH_STATE`=666). Per AAP §0.6.1 this
//! becomes the exhaustive [`DeflateStatus`] enum: an integer `status` can hold
//! an out-of-range value (which C's `deflateStateCheck` must guard against),
//! whereas a `DeflateStatus` is *always* one of the legal states by
//! construction, so the status-validity check
//! ([`DeflateState::is_valid_status`](state::DeflateState::is_valid_status))
//! becomes trivially true.
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; `unsafe`
//!   lives only in `crate::ffi` and `crate::inflate::fast` (AAP §0.6.2).
//! * **`no_std`-clean.** Only `core`/`alloc` are referenced, never `std`.

pub mod state;
pub mod strategy;
pub mod trees;

/// Header-emission state machine for the DEFLATE engine — the safe-Rust
/// replacement for the integer `status` field and its `*_STATE` sentinel
/// `#define`s in `deflate.h` (AAP §0.6.1).
///
/// The discriminants are pinned to the canonical zlib sentinel values so the
/// state semantics line up exactly with the C engine (and so the values are
/// recognizable when debugging against a C reference). The enum is evaluated
/// with exhaustive `match`, which both removes the need for C's
/// `deflateStateCheck` status guard (an enum can never hold an out-of-range
/// value) and lets the compiler prove every state transition is handled.
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
// Phase I — Strategy submodule declarations + `deflate()` orchestration
// ===========================================================================
//
// The five per-level / per-strategy inner loops (`deflate_stored`/`fast`/
// `slow`/`rle`/`huff`) live in sibling files and drive a [`DeflateStream`]
// directly. They are declared here so they compile into the crate, and they
// consume the [`flush_block`] / [`flush_block_only`] helpers defined just below
// (the safe-Rust replacements for `deflate.c`'s `FLUSH_BLOCK` / `FLUSH_BLOCK_ONLY`
// macros).
//
// On top of the strategy functions sits the public [`Deflate`] handle, which
// ports `deflate.c`'s `deflateInit2_` / `deflate()` / `deflateEnd` trio
// (AAP §0.4.1): construction validates the parameters and allocates the engine
// state, [`Deflate::compress`] runs the header → payload → trailer state
// machine, and `Drop` reclaims every buffer (RAII, no `deflateEnd`). Driving
// the same `CONFIG_TABLE` parameters and strategy functions in the same order
// as C yields byte-identical output (AAP §0.7.1).

pub mod fast;
pub mod huff;
pub mod rle;
pub mod slow;
pub mod stored;

use crate::constants::{
    DEF_MEM_LEVEL, FlushMode, MAX_MEM_LEVEL, MAX_WBITS, Strategy, Z_DEFLATED, Z_FIXED,
    Z_HUFFMAN_ONLY, Z_RLE, resolve_level,
};
use crate::deflate::state::{BlockState, DeflateState, DeflateStream, PRESET_DICT};
use crate::error::{ReturnCode, ZlibError};
use crate::stream::StreamState;
use alloc::boxed::Box;

/// Flush the current block to the pending buffer and on to the output, without
/// the early-return check — the safe-Rust port of C's `FLUSH_BLOCK_ONLY` macro
/// (`deflate.c`).
///
/// It hands the window region `[block_start, strstart)` to
/// [`DeflateState::tr_flush_block`] (which selects the smallest of the stored /
/// static / dynamic encodings), advances `block_start`, then drains as much of
/// the pending buffer to the output as fits.
///
/// `tr_flush_block` takes the stored-block source as `Option<&[u8]>` while also
/// borrowing the rest of the state mutably. To pass a slice of `state.window`
/// without copying — and without a borrow-checker conflict — the window `Vec`
/// is temporarily moved out (leaving an empty placeholder) and moved back
/// afterwards. This is sound because `trees.rs` never reads `state.window`.
pub(crate) fn flush_block_only(s: &mut DeflateStream<'_>, last: bool) {
    let block_start = s.state.block_start;
    let strstart = s.state.strstart;
    // stored_len = strstart - block_start (C `(ulg)((long)s->strstart -
    // s->block_start)`). `block_start` can briefly be negative while the window
    // slides, so the subtraction is performed in `isize`.
    let stored_len = (strstart as isize - block_start) as usize;

    if block_start >= 0 {
        let start = block_start as usize;
        let window = core::mem::take(&mut s.state.window);
        s.state
            .tr_flush_block(Some(&window[start..]), stored_len, last);
        s.state.window = window;
    } else {
        // C passes `Z_NULL` for the buffer when `block_start < 0`; the stored
        // path is then unreachable for this block.
        s.state.tr_flush_block(None, stored_len, last);
    }

    s.state.block_start = strstart as isize;
    s.flush_pending();
}

/// Flush the current block and signal whether the strategy loop must return —
/// the safe-Rust port of C's `FLUSH_BLOCK` macro (`deflate.c`).
///
/// After [`flush_block_only`], if the output buffer is now full the caller must
/// stop and return: `Some(`[`BlockState::FinishStarted`]`)` when this was the
/// final (`last`) block, otherwise `Some(`[`BlockState::NeedMore`]`)`. `None`
/// means there is still output room and the strategy loop may continue. The
/// strategy functions use it as `if let Some(bs) = flush_block(s, last) { return
/// bs; }`.
pub(crate) fn flush_block(s: &mut DeflateStream<'_>, last: bool) -> Option<BlockState> {
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

/// The flush "rank" used to suppress duplicate consecutive flushes, mirroring
/// C's `#define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))` (`deflate.c`).
#[inline]
fn rank(flush: i32) -> i32 {
    flush * 2 - if flush > 4 { 9 } else { 0 }
}

/// Append the 4-byte big-endian Adler-32 zlib (RFC 1950) trailer to the pending
/// buffer (`deflate.c` `putShortMSB(adler >> 16); putShortMSB(adler & 0xffff)`).
#[inline]
fn put_zlib_trailer(s: &mut DeflateStream<'_>) {
    let adler = s.state.adler;
    s.state.put_short_msb((adler >> 16) as u16);
    s.state.put_short_msb((adler & 0xffff) as u16);
}

/// Run one pass of the DEFLATE state machine over `s`, returning the
/// non-negative completion code. Direct port of C `deflate()` (`deflate.c`
/// L981-1291); errors are surfaced as [`ZlibError`] instead of negative codes.
///
/// The control flow mirrors C exactly so the emitted byte stream is identical:
/// drain pending output, emit the wrapper header, dispatch to the level/strategy
/// inner loop, apply the per-flush block epilogue, and finally emit the wrapper
/// trailer once `Z_FINISH` completes.
pub(crate) fn run_deflate(
    s: &mut DeflateStream<'_>,
    flush: FlushMode,
) -> Result<ReturnCode, ZlibError> {
    // deflate.c 984-986: `flush > Z_BLOCK` is rejected. The only `FlushMode`
    // above `Z_BLOCK` is `Z_TREES`; the status is always valid by construction.
    if flush == FlushMode::Trees {
        return Err(ZlibError::StreamError);
    }
    // deflate.c 988-993: with slices there are no null pointers; the surviving
    // guard is "no input is accepted once `Z_FINISH` has been started".
    if s.state.status == DeflateStatus::Finish && flush != FlushMode::Finish {
        return Err(ZlibError::StreamError);
    }
    // deflate.c 996: an empty output buffer cannot make progress.
    if s.avail_out() == 0 {
        return Err(ZlibError::BufError);
    }

    let old_flush = s.state.last_flush;
    s.state.last_flush = flush as i32;

    // deflate.c 1001-1024: drain pending output first; otherwise reject a
    // useless repeated flush that has no input and would do nothing.
    if s.state.pending != 0 {
        s.flush_pending();
        if s.avail_out() == 0 {
            // Output filled mid-drain: returning `Z_OK` (not `Z_BUF_ERROR`) on
            // the next call is ensured by parking `last_flush` at -1.
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    } else if s.avail_in() == 0
        && rank(flush as i32) <= rank(old_flush)
        && flush != FlushMode::Finish
    {
        return Err(ZlibError::BufError);
    }

    // deflate.c 1027-1029: no input may follow the first `Z_FINISH`.
    if s.state.status == DeflateStatus::Finish && s.avail_in() != 0 {
        return Err(ZlibError::BufError);
    }

    // --- Wrapper header (deflate.c 1031-1210) ---
    if s.state.status == DeflateStatus::Init && s.state.wrap == 0 {
        s.state.status = DeflateStatus::Busy;
    }
    if s.state.status == DeflateStatus::Init {
        // zlib (RFC 1950) two-byte header. CMF = (CM=8 | CINFO=w_bits-8);
        // FLG carries the level hint and a checksum so `header % 31 == 0`.
        let mut header: u32 = ((Z_DEFLATED as u32) + ((s.state.w_bits - 8) << 4)) << 8;
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

        // A preset dictionary (set before the first call) prefixes its Adler-32.
        if s.state.strstart != 0 {
            let adler = s.state.adler;
            s.state.put_short_msb((adler >> 16) as u16);
            s.state.put_short_msb((adler & 0xffff) as u16);
        }
        // Re-seed the running checksum for the payload (`adler32(0, &[]) == 1`).
        s.state.adler = 1;
        s.state.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer.
        s.flush_pending();
        if s.state.pending != 0 {
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }
    #[cfg(feature = "gzip")]
    if s.state.status == DeflateStatus::Gzip {
        // Minimal gzip (RFC 1952) header. This engine never installs a user
        // `gz_header` (it exposes no setter), so only C's `gzhead == Z_NULL`
        // branch is reachable: a fixed 10-byte header (deflate.c 1093-1131).
        s.state.adler = 0; // crc32(0, &[]) == 0
        s.state.put_byte(31); // ID1
        s.state.put_byte(139); // ID2
        s.state.put_byte(8); // CM = deflate
        s.state.put_byte(0); // FLG
        s.state.put_byte(0); // MTIME[0]
        s.state.put_byte(0); // MTIME[1]
        s.state.put_byte(0); // MTIME[2]
        s.state.put_byte(0); // MTIME[3]
        // XFL: 2 for max compression, 4 for fastest, 0 otherwise.
        let xfl: u8 = if s.state.level == 9 {
            2
        } else if s.state.strategy >= Z_HUFFMAN_ONLY || s.state.level < 2 {
            4
        } else {
            0
        };
        s.state.put_byte(xfl);
        s.state.put_byte(crate::util::OS_CODE); // OS = Unix (3)
        s.state.status = DeflateStatus::Busy;

        s.flush_pending();
        if s.state.pending != 0 {
            s.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }

    // --- Payload (deflate.c 1212-1262) ---
    // Start or continue a block when there is input, buffered look-ahead, or a
    // flush to honour before the stream has finished.
    if s.avail_in() != 0
        || s.state.lookahead != 0
        || (flush != FlushMode::NoFlush && s.state.status != DeflateStatus::Finish)
    {
        // Strategy dispatch (deflate.c 1216-1219): level 0 stores; the
        // Huffman-only and RLE strategies have dedicated loops; otherwise the
        // `CONFIG_TABLE` selects greedy matching (levels 1-3) or lazy matching
        // (levels 4-9).
        let bstate = if s.state.level == 0 {
            stored::deflate_stored(s, flush)
        } else if s.state.strategy == Z_HUFFMAN_ONLY {
            huff::deflate_huff(s, flush)
        } else if s.state.strategy == Z_RLE {
            rle::deflate_rle(s, flush)
        } else if s.state.level < 4 {
            fast::deflate_fast(s, flush)
        } else {
            slow::deflate_slow(s, flush)
        };

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            s.state.status = DeflateStatus::Finish;
        }
        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if s.avail_out() == 0 {
                s.state.last_flush = -1; // avoid a spurious BUF_ERROR next call
            }
            // More output room is needed before the block can complete.
            return Ok(ReturnCode::Ok);
        }
        if bstate == BlockState::BlockDone {
            // Honour the requested flush at the block boundary.
            if flush == FlushMode::PartialFlush {
                s.state.tr_align();
            } else if flush != FlushMode::Block {
                // Z_SYNC_FLUSH / Z_FULL_FLUSH: emit an empty stored block so the
                // decoder can resynchronize.
                s.state.tr_stored_block(None, 0, false);
                if flush == FlushMode::FullFlush {
                    s.state.clear_hash(); // forget the match history
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

    // --- Wrapper trailer (deflate.c 1264-1291) ---
    if flush != FlushMode::Finish {
        return Ok(ReturnCode::Ok);
    }
    if s.state.wrap <= 0 {
        // Raw DEFLATE: finished as soon as the final block is emitted.
        return Ok(ReturnCode::StreamEnd);
    }

    #[cfg(feature = "gzip")]
    {
        if s.state.wrap == 2 {
            // gzip trailer: little-endian CRC-32 then little-endian ISIZE
            // (input length modulo 2^32).
            let crc = s.state.adler;
            s.state.put_byte((crc & 0xff) as u8);
            s.state.put_byte(((crc >> 8) & 0xff) as u8);
            s.state.put_byte(((crc >> 16) & 0xff) as u8);
            s.state.put_byte(((crc >> 24) & 0xff) as u8);
            let isize_le = s.state.total_in as u32;
            s.state.put_byte((isize_le & 0xff) as u8);
            s.state.put_byte(((isize_le >> 8) & 0xff) as u8);
            s.state.put_byte(((isize_le >> 16) & 0xff) as u8);
            s.state.put_byte(((isize_le >> 24) & 0xff) as u8);
        } else {
            put_zlib_trailer(s);
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        put_zlib_trailer(s);
    }

    s.flush_pending();
    // Flip `wrap` negative so the trailer is emitted only once (deflate.c 1288).
    if s.state.wrap > 0 {
        s.state.wrap = -s.state.wrap;
    }
    if s.state.pending != 0 {
        Ok(ReturnCode::Ok)
    } else {
        Ok(ReturnCode::StreamEnd)
    }
}

/// Saturating `base + a + b + c + k`, returning [`u64::MAX`] on overflow — the
/// equivalent of C `deflateBound`'s `(z_size_t)-1` overflow sentinel.
#[inline]
fn bound_saturating(base: u64, a: u64, b: u64, c: u64, k: u64) -> u64 {
    base.saturating_add(a)
        .saturating_add(b)
        .saturating_add(c)
        .saturating_add(k)
}

/// Outcome of a single [`Deflate::compress`] call.
///
/// Bundles the non-negative completion [`ReturnCode`] with the number of input
/// bytes consumed and output bytes produced during the call — the owned-value
/// equivalent of the deltas a C caller reads back from the `z_stream`
/// `avail_in`/`avail_out` counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeflateOutcome {
    /// [`ReturnCode::Ok`] while more work remains, or [`ReturnCode::StreamEnd`]
    /// once a [`FlushMode::Finish`] has fully drained the stream (header,
    /// payload, and trailer).
    pub code: ReturnCode,
    /// Number of bytes consumed from the `input` slice this call.
    pub consumed: usize,
    /// Number of bytes written to the `output` slice this call.
    pub produced: usize,
}

/// A safe, owning DEFLATE compressor — the idiomatic Rust counterpart of a C
/// `z_stream` initialized with `deflateInit2_`.
///
/// `Deflate` owns its engine state as a `Box<DeflateState>`; every working
/// buffer (window, hash chains, pending/symbol buffer) is owned by that state,
/// so dropping a `Deflate` reclaims them deterministically — the RAII
/// replacement for C `deflateEnd` (AAP §0.6.3). The compressor is **100% safe
/// Rust**: no `unsafe`, no raw pointers, and no manual free path.
///
/// # Byte-identical output
///
/// `Deflate` drives the same LZ77/Huffman strategy functions with the same
/// `CONFIG_TABLE` parameters, in the same order, as C zlib, so for the same
/// input, level, strategy, and window it produces **byte-identical** compressed
/// output (AAP §0.7.1). It is the engine behind the one-shot
/// [`crate::util::compress`] helpers.
///
/// # Examples
///
/// ```
/// use zlib_rs::deflate::Deflate;
/// use zlib_rs::FlushMode;
///
/// let mut d = Deflate::new(6).unwrap();
/// let mut out = [0u8; 64];
/// let outcome = d.compress(b"hello hello hello hello", &mut out, FlushMode::Finish).unwrap();
/// assert_eq!(out[0], 0x78); // zlib CMF byte for windowBits 15
/// assert!(outcome.produced > 2);
/// ```
pub struct Deflate {
    /// The owned engine state. Boxed so the large `DeflateState` lives on the
    /// heap (as in C, where `deflate_state` is heap-allocated) and the handle
    /// itself stays cheap to move.
    state: Box<DeflateState>,
}

impl Deflate {
    /// Create a zlib (RFC 1950) compressor at `level`, exactly like C
    /// `deflateInit(strm, level)` — i.e. `deflateInit2` with `method =
    /// Z_DEFLATED`, `windowBits = MAX_WBITS` (15), `memLevel = DEF_MEM_LEVEL`
    /// (8), and `strategy = Z_DEFAULT_STRATEGY`.
    ///
    /// `level` is `0..=9` or [`Z_DEFAULT_COMPRESSION`](crate::Z_DEFAULT_COMPRESSION)
    /// (`-1`, which resolves to level 6).
    ///
    /// # Errors
    ///
    /// [`ZlibError::StreamError`] if `level` is outside `-1..=9`.
    pub fn new(level: i32) -> Result<Self, ZlibError> {
        Self::with_options(level, MAX_WBITS, DEF_MEM_LEVEL, Strategy::Default)
    }

    /// Create a compressor with explicit `window_bits`, `mem_level`, and
    /// `strategy`, porting the parameter handling of C `deflateInit2_`.
    ///
    /// `window_bits` follows zlib's overloading convention: `8..=15` selects the
    /// zlib wrapper, `-15..=-8` selects raw DEFLATE (no wrapper), and — with the
    /// `gzip` feature — `24..=31` selects the gzip wrapper. `mem_level` is
    /// `1..=9`; `level` is `0..=9` or `-1`.
    ///
    /// # Errors
    ///
    /// [`ZlibError::StreamError`] if any parameter is out of range (including a
    /// gzip `window_bits` requested without the `gzip` feature).
    pub fn with_options(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
    ) -> Result<Self, ZlibError> {
        let strategy_i = strategy as i32;
        // Z_DEFAULT_COMPRESSION (-1) resolves to level 6 (deflate.c 416-419).
        let level = resolve_level(level);

        // windowBits overloading (deflate.c 421-432).
        let mut wrap: i32 = 1;
        let mut window_bits = window_bits;
        if window_bits < 0 {
            // Raw DEFLATE: suppress the zlib wrapper.
            wrap = 0;
            if window_bits < -15 {
                return Err(ZlibError::StreamError);
            }
            window_bits = -window_bits;
        } else if window_bits > 15 {
            #[cfg(feature = "gzip")]
            {
                wrap = 2; // gzip wrapper
                window_bits -= 16;
            }
            #[cfg(not(feature = "gzip"))]
            {
                return Err(ZlibError::StreamError);
            }
        }

        // Parameter validation (deflate.c 434-438). `method` is implicitly
        // `Z_DEFLATED` here, so that clause of the C test is always satisfied.
        // `window_bits` has already been normalized to its positive magnitude
        // (raw/gzip forms folded above), so the C ranges become the inclusive
        // ranges below.
        if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
            || !(8..=15).contains(&window_bits)
            || !(0..=9).contains(&level)
            || !(0..=Z_FIXED).contains(&strategy_i)
            || (window_bits == 8 && wrap != 1)
        {
            return Err(ZlibError::StreamError);
        }
        // windowBits == 8 is promoted to 9 (deflate.c 439: 256-byte window bug).
        if window_bits == 8 {
            window_bits = 9;
        }

        let w_bits = window_bits as u32;
        let hash_bits = mem_level as u32 + 7;

        // Allocate + size the engine (deflate.c 440-531), then run the
        // `deflateReset` tail (`_tr_init` + `lm_init`) via `StreamState::reset`,
        // which also sets the correct initial header status for the wrapper.
        let mut state = DeflateState::new_allocated(
            w_bits,
            hash_bits,
            mem_level as u32,
            level,
            strategy_i,
            Z_DEFLATED as u8,
            wrap,
        );
        state.reset();

        Ok(Self {
            state: Box::new(state),
        })
    }

    /// Compress `input` into `output` with the given `flush` mode, porting C
    /// `deflate()`.
    ///
    /// Returns a [`DeflateOutcome`] with the bytes consumed/produced and the
    /// completion [`ReturnCode`]. A [`FlushMode::Finish`] call into an output
    /// buffer large enough to hold the whole stream (size it with
    /// [`crate::util::compress_bound`]) completes in a single call and reports
    /// [`ReturnCode::StreamEnd`].
    ///
    /// # Errors
    ///
    /// * [`ZlibError::BufError`] — `output` is empty, or no progress is possible
    ///   for a redundant flush.
    /// * [`ZlibError::StreamError`] — an invalid flush (e.g.
    ///   [`FlushMode::Trees`]) or input supplied after a completed `Finish`.
    pub fn compress(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        flush: FlushMode,
    ) -> Result<DeflateOutcome, ZlibError> {
        let mut ds = DeflateStream::new(&mut self.state, input, output);
        let code = run_deflate(&mut ds, flush)?;
        Ok(DeflateOutcome {
            code,
            consumed: ds.in_next,
            produced: ds.out_next,
        })
    }

    /// Upper bound on the compressed size of `source_len` input bytes for this
    /// compressor's configuration, porting C `deflateBound`.
    ///
    /// For the default zlib settings (`windowBits = 15`, `memLevel = 8`) this is
    /// the tight `compressBound`-style bound; other configurations fall back to
    /// the conservative fixed-/stored-block bounds plus the wrapper length.
    /// Overflow saturates to [`u64::MAX`] (C's `(z_size_t)-1`).
    #[must_use]
    pub fn bound(&mut self, source_len: u64) -> u64 {
        // Conservative fixed-block bound (~13% overhead + a small constant).
        let fixedlen = bound_saturating(
            source_len,
            source_len >> 3,
            source_len >> 8,
            source_len >> 9,
            4,
        );
        // Conservative stored-block bound (~4% overhead + a small constant).
        let storelen = bound_saturating(
            source_len,
            source_len >> 5,
            source_len >> 7,
            source_len >> 11,
            7,
        );

        // Wrapper length (deflate.c 884-915). `wrap` is driven negative once a
        // stream finishes; its magnitude selects the format. A user gzip header
        // is never installed through this API, so the bare 18-byte gzip wrapper
        // applies.
        let wrap = if self.state.wrap < 0 {
            -self.state.wrap
        } else {
            self.state.wrap
        };
        let wraplen: u64 = match wrap {
            0 => 0,
            1 => 6 + if self.state.strstart != 0 { 4 } else { 0 },
            _ => 18,
        };

        // Non-default parameters → one of the conservative bounds (deflate.c
        // 918-923).
        if self.state.w_bits != 15 || self.state.hash_bits != 8 + 7 {
            let bound = if self.state.w_bits <= self.state.hash_bits && self.state.level != 0 {
                fixedlen
            } else {
                storelen
            };
            return bound.saturating_add(wraplen);
        }

        // Default settings → tight bound (deflate.c 925-927): the compressBound
        // formula with its baked-in 6-byte zlib wrapper removed, plus the actual
        // wrapper length.
        let bound = source_len
            .wrapping_add(source_len >> 12)
            .wrapping_add(source_len >> 14)
            .wrapping_add(source_len >> 25)
            .wrapping_add(13 - 6)
            .wrapping_add(wraplen);
        if bound < source_len { u64::MAX } else { bound }
    }
}
