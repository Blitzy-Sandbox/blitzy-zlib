//! DEFLATE compression engine — the top-level `deflate` module.
//!
//! This module is a faithful, **100% safe** Rust port of the C `deflate()`
//! driver and every public `deflate*` API function from `deflate.c`, together
//! with the zlib (RFC 1950) and gzip (RFC 1952) stream header/trailer framing.
//! It is the root of the `src/deflate/` tree: it declares the engine
//! submodules, re-exports the public deflate API, and orchestrates the block
//! producers (`stored`, `fast`, `slow`, `rle`, `huff`) through the shared
//! [`DeflateState`] and [`IoContext`].
//!
//! # Byte-exact fidelity
//!
//! The observable output of this module is **byte-identical** to reference
//! zlib for the same input, level, and strategy. The header bytes (including
//! the zlib FCHECK adjustment and every gzip header field), the empty-block
//! flush markers, and the trailers (Adler-32 for a zlib wrapper, CRC-32 plus
//! ISIZE for a gzip wrapper) are reproduced exactly. Every early-return
//! checkpoint of the C driver — where a full output buffer causes an early
//! `Z_OK` return with `last_flush == -1` — is preserved so that streaming with
//! small output buffers behaves identically.
//!
//! # Safety and portability
//!
//! There is **zero `unsafe`** anywhere in this module (and the whole
//! `deflate/` tree): manual `zcalloc`/`zcfree` allocation is replaced by owned
//! [`alloc::vec::Vec`] buffers and `Box`-owned state, and buffer accesses use
//! bounds-checked slice indexing. The module is `no_std` + `alloc`. gzip
//! framing is gated behind the `gzip` cargo feature; the crate also compiles
//! and runs with gzip disabled (raw and zlib framing only). Targets Rust 2024
//! edition, MSRV 1.85.0.
//!
//! # Idiomatic wins over C
//!
//! * `Drop` (via owned `Vec`s inside [`DeflateState`]) subsumes `deflateEnd`'s
//!   manual reverse-order frees; [`deflate_end`] only reports the residual
//!   `Z_DATA_ERROR`-if-busy status.
//! * `#[derive(Clone)]` on [`DeflateState`] subsumes `deflateCopy`'s pointer
//!   fix-ups: [`deflate_copy`] is a deep clone with no manual re-basing.
//! * The integer state tags become the [`DeflateStatus`] enum with exhaustive
//!   `match`, and the C `configuration_table` of function pointers becomes the
//!   [`CompressFunc`] enum dispatched through a `match`.

// ---------------------------------------------------------------------------
// Submodule declarations.
//
// These are declared `pub mod` because this file *is* `deflate/mod.rs` and the
// block producers reference their siblings by absolute path (for example
// `use crate::deflate::state::DeflateState;` and `use crate::deflate::trees;`),
// and `src/stream.rs` reaches `crate::deflate::state::DeflateState`.
// ---------------------------------------------------------------------------
pub mod fast;
pub mod huff;
pub mod rle;
pub mod slow;
pub mod state;
pub mod stored;
pub mod strategy;
pub mod trees;

// Public re-exports: the state/status/IoContext types the rest of the crate
// consumes, and the strategy dispatch types.
pub use state::{DeflateState, DeflateStatus, IoContext};
pub use strategy::{BlockState, CONFIGURATION_TABLE, CompressFunc};

// ---------------------------------------------------------------------------
// Imports.
// ---------------------------------------------------------------------------
use alloc::boxed::Box;

use crate::checksum::adler32;
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{
    DEF_MEM_LEVEL, MAX_WBITS, Strategy, WrapMode, Z_BLOCK, Z_DEFAULT_COMPRESSION, Z_DEFLATED,
    Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH, Z_PARTIAL_FLUSH, parse_window_bits,
};
use crate::error::{ReturnCode, ZlibError};
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;
use crate::stream::{Allocator, ZStream};

use crate::deflate::state::{BUF_SIZE, MIN_MATCH, NIL};
use crate::deflate::strategy::rank;

// ---------------------------------------------------------------------------
// Local constants — mirrors of `zutil.h` macros that no other module exports.
// ---------------------------------------------------------------------------

/// `PRESET_DICT` (`zutil.h` L96): the preset-dictionary flag bit set in the
/// zlib header FLG byte when the stream was primed with a dictionary.
const PRESET_DICT: u32 = 0x20;

/// `OS_CODE` (`zutil.h` L188): the operating-system byte written into the gzip
/// header. `3` is the "Unix" code, the portable default used by zlib on all
/// non-legacy platforms.
#[cfg(feature = "gzip")]
const OS_CODE: u8 = 3;

/// The internal `Result` alias used throughout the deflate API surface: `Ok`
/// carries a success [`ReturnCode`] (typically [`ReturnCode::Ok`] or
/// [`ReturnCode::StreamEnd`]), while `Err` carries a [`ZlibError`]. The FFI
/// boundary flattens both arms back to the integer C return codes.
type DeflateResult = Result<ReturnCode, ZlibError>;

// ===========================================================================
// DeflateConfig — the Builder replacing the multi-argument deflateInit2_.
// ===========================================================================

/// Compression parameters for initializing a deflate stream.
///
/// This is the idiomatic Rust replacement for the positional
/// `deflateInit2_(level, method, windowBits, memLevel, strategy, …)` call. It
/// is a plain builder: start from [`DeflateConfig::default`] (or
/// [`DeflateConfig::new`]) and override individual fields with the chained
/// setters, then hand it to [`DeflateConfig::init`].
///
/// ```ignore
/// use zlib_rs::deflate::DeflateConfig;
/// use zlib_rs::constants::Strategy;
/// let mut strm = zlib_rs::stream::ZStream::new();
/// DeflateConfig::new()
///     .level(6)
///     .strategy(Strategy::Default)
///     .init(&mut strm)
///     .unwrap();
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeflateConfig {
    /// Compression level: `-1` ([`Z_DEFAULT_COMPRESSION`],
    /// resolved to `6`) or `0..=9`.
    pub level: i32,
    /// Compression method — must be [`Z_DEFLATED`] (`8`).
    pub method: i32,
    /// The base-2 logarithm of the window size, overloaded to also select the
    /// wrapper: `8..=15` = zlib, `-8..=-15` = raw, `24..=31` = gzip.
    pub window_bits: i32,
    /// The memory level (`1..=9`, default `8`) controlling the hash-table and
    /// symbol-buffer sizes.
    pub mem_level: i32,
    /// The compression [`Strategy`].
    pub strategy: Strategy,
}

impl Default for DeflateConfig {
    fn default() -> Self {
        Self {
            level: Z_DEFAULT_COMPRESSION,
            method: Z_DEFLATED,
            window_bits: MAX_WBITS,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        }
    }
}

impl DeflateConfig {
    /// Creates a configuration with the zlib default settings (level `-1`,
    /// method DEFLATE, `windowBits` 15, `memLevel` 8, default strategy).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the compression level (`-1` or `0..=9`).
    #[must_use]
    pub fn level(mut self, level: i32) -> Self {
        self.level = level;
        self
    }

    /// Sets the compression method (must be [`Z_DEFLATED`]).
    #[must_use]
    pub fn method(mut self, method: i32) -> Self {
        self.method = method;
        self
    }

    /// Sets the `windowBits` value (also selects zlib/raw/gzip framing).
    #[must_use]
    pub fn window_bits(mut self, window_bits: i32) -> Self {
        self.window_bits = window_bits;
        self
    }

    /// Sets the memory level (`1..=9`).
    #[must_use]
    pub fn mem_level(mut self, mem_level: i32) -> Self {
        self.mem_level = mem_level;
        self
    }

    /// Sets the compression [`Strategy`].
    #[must_use]
    pub fn strategy(mut self, strategy: Strategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Initializes `strm` for compression with these parameters.
    ///
    /// Equivalent to calling [`deflate_init2`] with the individual fields; see
    /// that function for the exact validation and error semantics.
    ///
    /// # Errors
    ///
    /// Returns [`ZlibError::StreamError`] for invalid parameters and
    /// [`ZlibError::MemError`] if state allocation fails.
    pub fn init<A: Allocator>(self, strm: &mut ZStream<A>) -> DeflateResult {
        deflate_init2(
            strm,
            self.level,
            self.method,
            self.window_bits,
            self.mem_level,
            self.strategy,
        )
    }
}

// ===========================================================================
// DeflateOutcome — return value of the streaming driver.
// ===========================================================================

/// The result of a streaming [`deflate`] (or [`deflate_params`]) call.
///
/// Because [`ZStream`] carries no `next_in`/`next_out`/`avail_*` cursor fields
/// (the input and output buffers are passed as slices per call), progress is
/// reported explicitly: `consumed` bytes were read from `input` and `produced`
/// bytes were written to `output`. The FFI boundary uses these to advance the
/// caller's `z_stream` cursors. `code` is the zlib return code for the call
/// (typically [`ReturnCode::Ok`], [`ReturnCode::StreamEnd`],
/// [`ReturnCode::BufError`], or [`ReturnCode::StreamError`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeflateOutcome {
    /// The zlib return code produced by the call.
    pub code: ReturnCode,
    /// Number of input bytes consumed from the supplied `input` slice.
    pub consumed: usize,
    /// Number of output bytes written to the supplied `output` slice.
    pub produced: usize,
}

// ===========================================================================
// Small helpers.
// ===========================================================================

/// Reproduces C `putShortMSB` (`deflate.c` L939): appends a 16-bit value to
/// the pending buffer in **most-significant-byte-first** order. Used for the
/// zlib header CMF/FLG word, the preset-dictionary Adler-32, and the zlib
/// Adler-32 trailer.
fn put_short_msb(s: &mut DeflateState, b: u16) {
    s.put_byte((b >> 8) as u8);
    s.put_byte((b & 0xff) as u8);
}

/// Reproduces the C `CLEAR_HASH(s)` macro (`deflate.c` L170): resets every
/// hash-head entry to [`NIL`]. Because `NIL == 0`, clearing every entry is
/// equivalent to the C form that writes `head[hash_size - 1] = NIL` and zeroes
/// the remainder. Also clears the `slid` flag. Invoked on `Z_FULL_FLUSH` and
/// by [`deflate_params`] when leaving level 0.
fn clear_hash(s: &mut DeflateState) {
    s.head.iter_mut().for_each(|h| *h = NIL);
    s.slid = false;
}

/// Reproduces the C `HCRC_UPDATE(beg)` macro (`deflate.c` L973): when the gzip
/// header CRC is requested, folds the pending-buffer bytes in `beg..pending`
/// into the running header CRC (`io.adler`). A no-op when `hcrc` is false or
/// no new bytes were appended since `beg`.
#[cfg(feature = "gzip")]
fn hcrc_update(s: &mut DeflateState, io: &mut IoContext, hcrc: bool, beg: usize) {
    if hcrc && s.pending > beg {
        io.adler = crc32(io.adler, &s.pending_buf[beg..s.pending]);
    }
}

// ===========================================================================
// Initialization — deflateInit2_ / deflateInit_.
// ===========================================================================

/// Initializes `strm` for compression with the full parameter set.
///
/// Port of C `deflateInit2_` (`deflate.c` L387-L533), minus the FFI-only
/// version/`stream_size` guard (that check belongs to the `ffi` layer). The
/// `windowBits` value is decoded into a wrapper selector and a positive window
/// size via [`parse_window_bits`]:
///
/// * `8..=15` → zlib wrapper (`wrap = 1`)
/// * `-8..=-15` → raw DEFLATE, no wrapper (`wrap = 0`)
/// * `24..=31` → gzip wrapper (`wrap = 2`, requires the `gzip` feature)
/// * `40..=47` (auto-detect) is inflate-only and rejected here
///
/// All remaining parameter validation (memory level, method, window range,
/// level range, and the `windowBits == 8 && wrap != 1` rejection), the
/// `level == -1 → 6` default resolution, and the 8-bit-window bump to 9 are
/// performed by [`DeflateState::new`]. On success the freshly constructed
/// state is installed and the stream is reset (mirroring the C
/// `return deflateReset(strm);`).
///
/// # Errors
///
/// * [`ZlibError::StreamError`] — invalid `windowBits`, a gzip request without
///   the `gzip` feature, an auto-detect request, or any parameter rejected by
///   [`DeflateState::new`].
/// * [`ZlibError::MemError`] — state allocation failed.
pub fn deflate_init2<A: Allocator>(
    strm: &mut ZStream<A>,
    level: i32,
    method: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: Strategy,
) -> DeflateResult {
    strm.clear_msg();

    // Decode windowBits → (WrapMode, positive w_bits).
    let (wrap_mode, w_bits) = parse_window_bits(window_bits).ok_or(ZlibError::StreamError)?;

    // Map the wrapper to the C `wrap` integer. Gzip requires the `gzip`
    // feature; auto-detect is inflate-only and invalid for deflate.
    let wrap: i32 = match wrap_mode {
        WrapMode::Zlib => 1,
        WrapMode::Raw => 0,
        #[cfg(feature = "gzip")]
        WrapMode::Gzip => 2,
        #[cfg(not(feature = "gzip"))]
        WrapMode::Gzip => return Err(ZlibError::StreamError),
        WrapMode::Auto => return Err(ZlibError::StreamError),
    };

    // `DeflateState::new_in` performs the full deflateInit2_ validation,
    // resolves the default level, and runs the state-side reset + lm_init.
    // The allocator hook (the caller's `zalloc`/`zfree` under the FFI
    // `CAllocator`, or a no-op under the global allocator) is threaded through
    // so every working buffer is routed through the caller's allocator when one
    // is installed (AAP §0.6.3 has-hook clause; QA FINDING-3).
    let hook = strm.allocator().hook();
    let state = DeflateState::new_in(
        hook,
        level,
        method,
        w_bits as i32,
        mem_level,
        strategy,
        wrap,
    )?;
    strm.set_deflate_state(state);

    // Finish exactly as C does: `return deflateReset(strm);`.
    deflate_reset(strm)
}

/// Initializes `strm` for compression at the given `level` using the zlib
/// defaults (method DEFLATE, `windowBits` 15, `memLevel` 8, default strategy).
///
/// Port of C `deflateInit_` (`deflate.c` L379-L385), which forwards to
/// `deflateInit2_` with those defaults.
///
/// # Errors
///
/// See [`deflate_init2`].
pub fn deflate_init<A: Allocator>(strm: &mut ZStream<A>, level: i32) -> DeflateResult {
    deflate_init2(
        strm,
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        Strategy::Default,
    )
}

// ===========================================================================
// Reset — deflateResetKeep / deflateReset.
// ===========================================================================

/// Resets the stream I/O counters, checksum seed, and header state while
/// keeping the allocated buffers and configuration.
///
/// Port of C `deflateResetKeep` (`deflate.c` L644-L677). The state-side reset
/// (status, pending, `last_flush = -2`, bit buffer) and the I/O-side reset
/// (`total_in`/`total_out = 0`, `adler` seeded) are delegated to
/// [`DeflateState::reset_keep`]; because that method deliberately does **not**
/// perform the tree-frequency initialization, this function invokes
/// `trees::_tr_init` afterwards. Finally the caller-visible stream fields are
/// written back and the message is cleared.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_reset_keep<A: Allocator>(strm: &mut ZStream<A>) -> DeflateResult {
    let seed;
    {
        let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;
        // A throwaway IoContext carries the counter/checksum reset; there are
        // no real buffers to process here.
        let mut empty: [u8; 0] = [];
        let mut io = IoContext::new(&[], &mut empty);
        s.reset_keep(&mut io);
        trees::_tr_init(s);
        seed = io.adler;
    }
    strm.total_in = 0;
    strm.total_out = 0;
    strm.adler = seed;
    strm.data_type = crate::constants::DataType::Unknown.as_c_int();
    strm.clear_msg();
    Ok(ReturnCode::Ok)
}

/// Fully resets the stream, ready to begin a new compression from scratch.
///
/// Port of C `deflateReset` (`deflate.c` L704-L712): a [`deflate_reset_keep`]
/// followed by [`DeflateState::lm_init`] to re-establish the match-finder
/// window bookkeeping and per-level tuning parameters.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_reset<A: Allocator>(strm: &mut ZStream<A>) -> DeflateResult {
    deflate_reset_keep(strm)?;
    let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;
    s.lm_init();
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Dictionary — deflateSetDictionary / deflateGetDictionary.
// ===========================================================================

/// Initializes the compression dictionary from `dictionary`.
///
/// Port of C `deflateSetDictionary` (`deflate.c` L559-L622). May be called
/// only before any data has been processed: for a zlib wrapper the header must
/// not yet have been emitted (`status == Init`), and for any wrapper the
/// look-ahead must be empty; a gzip wrapper (`wrap == 2`) cannot take a
/// dictionary. For a zlib wrapper the dictionary bytes are folded into the
/// stream's running Adler-32 (over the *full* dictionary, before any
/// truncation). If the dictionary is at least a window in size, only its
/// trailing `w_size` bytes are retained. The bytes are then slid into the
/// window and every position is inserted into the hash chains, exactly as the
/// C `fill_window` + `INSERT_STRING` loop does.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if a dictionary cannot be set in the current
/// state, or if `strm` has no deflate state installed.
pub fn deflate_set_dictionary<A: Allocator>(
    strm: &mut ZStream<A>,
    dictionary: &[u8],
) -> DeflateResult {
    // Read the running checksum out first (Copy) so we can fold the dictionary
    // into it without holding a borrow of `strm` across the state work.
    let mut running_adler = strm.adler;
    {
        let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;
        let wrap = s.wrap;

        // Cannot set a dictionary for gzip, for a zlib stream whose header was
        // already written, or once any input has entered the window.
        if wrap == 2 || (wrap == 1 && s.status != DeflateStatus::Init) || s.lookahead != 0 {
            return Err(ZlibError::StreamError);
        }

        // For a zlib wrapper compute the Adler-32 of the (full) dictionary.
        if wrap == 1 {
            running_adler = adler32(running_adler, dictionary);
        }
        // Avoid re-checksumming while sliding the dictionary through read_buf.
        s.wrap = 0;

        // If the dictionary would fill the window, keep only its tail; when
        // there was no wrapper, the (already empty) history is reset per C.
        let mut dict = dictionary;
        if dict.len() >= s.w_size {
            if wrap == 0 {
                clear_hash(s);
                s.strstart = 0;
                s.block_start = 0;
                s.insert = 0;
            }
            dict = &dict[dict.len() - s.w_size..];
        }

        // Slide the dictionary into the window and insert every position into
        // the hash chains. The IoContext presents `dict` as input with an empty
        // output; `s.wrap == 0` here so read_buf leaves the checksum untouched.
        let mut empty: [u8; 0] = [];
        let mut io = IoContext::new(dict, &mut empty);
        s.fill_window(&mut io);
        while s.lookahead >= MIN_MATCH {
            let mut str_idx = s.strstart;
            let n = s.lookahead - (MIN_MATCH - 1);
            for _ in 0..n {
                s.insert_string(str_idx);
                str_idx += 1;
            }
            s.strstart = str_idx;
            s.lookahead = MIN_MATCH - 1;
            s.fill_window(&mut io);
        }
        s.strstart += s.lookahead;
        s.block_start = s.strstart as isize;
        s.insert = s.lookahead;
        s.lookahead = 0;
        s.match_length = MIN_MATCH - 1;
        s.prev_length = MIN_MATCH - 1;
        s.match_available = false;
        s.wrap = wrap; // restore the original wrapper
    }
    strm.adler = running_adler;
    Ok(ReturnCode::Ok)
}

/// Returns a copy of the compression dictionary currently in the window and
/// its length.
///
/// Port of C `deflateGetDictionary` (`deflate.c` L625-L641). The returned
/// length is `min(strstart + lookahead, w_size)`. When `dictionary` is
/// `Some(dst)`, the most recent `len` window bytes are copied into `dst`; the
/// caller must ensure `dst` is at least the returned length. Pass `None` to
/// query only the length.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_get_dictionary<A: Allocator>(
    strm: &ZStream<A>,
    dictionary: Option<&mut [u8]>,
) -> Result<usize, ZlibError> {
    let s = strm.deflate_state().ok_or(ZlibError::StreamError)?;
    let mut len = s.strstart + s.lookahead;
    if len > s.w_size {
        len = s.w_size;
    }
    if let Some(dst) = dictionary {
        if len != 0 {
            let start = s.strstart + s.lookahead - len;
            dst[..len].copy_from_slice(&s.window[start..start + len]);
        }
    }
    Ok(len)
}

// ===========================================================================
// gzip header — deflateSetHeader.
// ===========================================================================

/// Provides the gzip header fields to be written when the stream uses gzip
/// framing.
///
/// Port of C `deflateSetHeader` (`deflate.c` L714-L719). Requires a gzip
/// wrapper (`wrap == 2`); passing `None` restores the default (all-zero) gzip
/// header. Available only when the `gzip` feature is enabled.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if the stream is not a gzip stream or has no
/// deflate state installed.
#[cfg(feature = "gzip")]
pub fn deflate_set_header<A: Allocator>(
    strm: &mut ZStream<A>,
    head: Option<GzHeader>,
) -> DeflateResult {
    let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;
    if s.wrap != 2 {
        return Err(ZlibError::StreamError);
    }
    s.gzhead = head;
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// Introspection — deflatePending / deflateUsed.
// ===========================================================================

/// Returns the number of bytes and bits of output that have been generated but
/// not yet emitted: `(pending_bytes, pending_bits)`.
///
/// Port of C `deflatePending` (`deflate.c` L722-L734). The C truncation guard
/// (which returns `Z_BUF_ERROR` when the `size_t` pending count does not fit
/// the `unsigned` out-parameter) has no analogue at this `usize`-typed
/// boundary and is applied by the FFI layer if needed.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_pending<A: Allocator>(strm: &ZStream<A>) -> Result<(usize, i32), ZlibError> {
    let s = strm.deflate_state().ok_or(ZlibError::StreamError)?;
    Ok((s.pending, s.bi_valid))
}

/// Returns the number of bits used in the last byte written to the output.
///
/// Port of C `deflateUsed` (`deflate.c` L737-L742).
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_used<A: Allocator>(strm: &ZStream<A>) -> Result<i32, ZlibError> {
    let s = strm.deflate_state().ok_or(ZlibError::StreamError)?;
    Ok(s.bi_used)
}

// ===========================================================================
// deflatePrime.
// ===========================================================================

/// Inserts `bits` low-order bits of `value` into the pending output ahead of
/// the compressed data.
///
/// Port of C `deflatePrime` (`deflate.c` L745-L771). `bits` must be in
/// `0..=16`. The C room check guards against the compressed data catching up
/// to the (overlaid) symbol buffer; in this crate `sym_buf` is a separate
/// allocation, so the equivalent guard is expressed against `lit_bufsize`:
/// there must be at least `(Buf_size + 7) / 8` bytes of head-room before the
/// symbol region, i.e. `lit_bufsize >= pending_out + 2`.
///
/// # Errors
///
/// * [`ZlibError::BufError`] — `bits` is out of range or there is insufficient
///   head-room.
/// * [`ZlibError::StreamError`] — `strm` has no deflate state installed.
pub fn deflate_prime<A: Allocator>(strm: &mut ZStream<A>, bits: i32, value: i32) -> DeflateResult {
    let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;

    if !(0..=16).contains(&bits) || s.lit_bufsize < s.pending_out + ((BUF_SIZE as usize + 7) >> 3) {
        return Err(ZlibError::BufError);
    }

    let mut bits = bits;
    let mut value = value;
    loop {
        let mut put = BUF_SIZE - s.bi_valid;
        if put > bits {
            put = bits;
        }
        // Extract the low `put` bits of `value` and OR them in at `bi_valid`.
        // The shift is computed in `u32` and truncated to `u16`, reproducing
        // the C `(ush)(...)` truncation with no overflow panic.
        let masked = (value & ((1 << put) - 1)) as u32;
        s.bi_buf |= (masked << s.bi_valid) as u16;
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
// deflateTune.
// ===========================================================================

/// Fine-tunes the internal match-finder parameters directly.
///
/// Port of C `deflateTune` (`deflate.c` L819-L830). For advanced use; the
/// values override those loaded from the configuration table for the current
/// level.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `strm` has no deflate state installed.
pub fn deflate_tune<A: Allocator>(
    strm: &mut ZStream<A>,
    good_length: i32,
    max_lazy: i32,
    nice_length: i32,
    max_chain: i32,
) -> DeflateResult {
    let s = strm.deflate_state_mut().ok_or(ZlibError::StreamError)?;
    s.good_match = good_length as usize;
    s.max_lazy_match = max_lazy as usize;
    s.nice_match = nice_length;
    s.max_chain_length = max_chain as usize;
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// deflateBound / deflateBound_z.
// ===========================================================================

/// Returns an upper bound on the compressed size of `source_len` input bytes
/// for the current stream configuration.
///
/// Port of C `deflateBound_z` (`deflate.c` L856-L927), reproduced formula for
/// formula so that callers who pre-allocate output buffers observe an
/// identical bound. All overflow cases (`(z_size_t)-1` in C) are reproduced
/// with saturating arithmetic, which caps at [`usize::MAX`].
///
/// * Without an installed state, the larger of the fixed-block and
///   stored-block conservative bounds plus a full 18-byte wrapper is returned.
/// * For non-default window/hash sizes, one of the two conservative bounds is
///   returned (fixed when `w_bits <= hash_bits && level != 0`, else stored),
///   plus the wrapper length.
/// * For the default `windowBits == 15` / `memLevel == 8`, the tight bound
///   `source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25)
///   + 7 + wraplen` is returned.
///
/// `wraplen` is `0` for raw, `6 + (strstart ? 4 : 0)` for zlib, and `18` plus
/// any user gzip-header fields for gzip.
#[must_use]
pub fn deflate_bound_z<A: Allocator>(strm: &ZStream<A>, source_len: usize) -> usize {
    // Upper bound for fixed blocks with 9-bit literals (~13% + a constant).
    let fixedlen = source_len
        .saturating_add(source_len >> 3)
        .saturating_add(source_len >> 8)
        .saturating_add(source_len >> 9)
        .saturating_add(4);

    // Upper bound for stored blocks with length 127 (~4% + a constant).
    let storelen = source_len
        .saturating_add(source_len >> 5)
        .saturating_add(source_len >> 7)
        .saturating_add(source_len >> 11)
        .saturating_add(7);

    // If we cannot read parameters, return the larger bound plus a wrapper.
    let s = match strm.deflate_state() {
        Some(s) => s,
        None => return fixedlen.max(storelen).saturating_add(18),
    };

    // Compute the wrapper length from the (sign-normalized) wrap mode.
    let wrap_abs = if s.wrap < 0 { -s.wrap } else { s.wrap };
    let wraplen: usize = match wrap_abs {
        0 => 0,                                       // raw deflate
        1 => 6 + if s.strstart != 0 { 4 } else { 0 }, // zlib wrapper
        #[cfg(feature = "gzip")]
        2 => {
            // gzip wrapper: fixed 18 plus any user-supplied header fields.
            let mut w = 18usize;
            if let Some(h) = s.gzhead.as_ref() {
                if let Some(extra) = h.extra.as_ref() {
                    w += 2 + extra.len();
                }
                if let Some(name) = h.name.as_ref() {
                    w += name.len() + 1; // field bytes plus the NUL terminator
                }
                if let Some(comment) = h.comment.as_ref() {
                    w += comment.len() + 1;
                }
                if h.hcrc {
                    w += 2;
                }
            }
            w
        }
        _ => 18, // for compiler happiness (matches the C default arm)
    };

    // If not the default parameters, return a conservative bound plus wrapper.
    if s.w_bits != 15 || s.hash_bits != 15 {
        let bound = if s.w_bits <= s.hash_bits && s.level != 0 {
            fixedlen
        } else {
            storelen
        };
        return bound.saturating_add(wraplen);
    }

    // Default settings: the tight bound. C writes `+ 13 - 6` (net `+ 7`).
    source_len
        .saturating_add(source_len >> 12)
        .saturating_add(source_len >> 14)
        .saturating_add(source_len >> 25)
        .saturating_add(7)
        .saturating_add(wraplen)
}

/// Returns an upper bound on the compressed size of `source_len` input bytes.
///
/// Port of C `deflateBound` (`deflate.c` L929-L932), the classic `uLong`-typed
/// entry point. It forwards to [`deflate_bound_z`]; at this `usize`-typed
/// boundary the two are identical. The C version additionally clamps the
/// result to the `uLong` range — that clamp is a property of the C integer
/// width and is applied by the FFI layer, not here.
#[must_use]
pub fn deflate_bound<A: Allocator>(strm: &ZStream<A>, source_len: usize) -> usize {
    deflate_bound_z(strm, source_len)
}

// ===========================================================================
// The deflate() driver.
// ===========================================================================

/// The core deflate state machine, operating directly on a [`DeflateState`]
/// and an [`IoContext`].
///
/// Statement-by-statement port of C `deflate()` (`deflate.c` L981-L1290),
/// excluding the outer state-presence and flush-range validation (performed by
/// the public [`deflate`] wrapper). It advances through the header stages
/// (zlib or gzip), dispatches the appropriate block producer, and — on
/// `Z_FINISH` — writes the trailer. Every early-return checkpoint of the C
/// driver is reproduced so that streaming with a small output buffer behaves
/// identically.
fn deflate_run(s: &mut DeflateState, io: &mut IoContext, flush: i32) -> ReturnCode {
    // The status/flush combination and output availability are checked first
    // (the null-pointer checks of C are unrepresentable with safe slices).
    if s.status == DeflateStatus::Finish && flush != Z_FINISH {
        return ReturnCode::StreamError;
    }
    if io.avail_out == 0 {
        return ReturnCode::BufError;
    }

    let old_flush = s.last_flush;
    s.last_flush = flush;

    // Flush as much pending output as possible.
    if s.pending != 0 {
        s.flush_pending(io);
        if io.avail_out == 0 {
            // Return OK (not BUF_ERROR) so the next call with more output space
            // is not mistaken for a useless repeat.
            s.last_flush = -1;
            return ReturnCode::Ok;
        }
    } else if io.avail_in == 0 && rank(flush) <= rank(old_flush) && flush != Z_FINISH {
        // Avoid duplicate consecutive flushes; keep returning STREAM_END rather
        // than BUF_ERROR for repeated Z_FINISH.
        return ReturnCode::BufError;
    }

    // No more input is permitted after the first Z_FINISH.
    if s.status == DeflateStatus::Finish && io.avail_in != 0 {
        return ReturnCode::BufError;
    }

    // ---------------------------- zlib header ----------------------------
    if s.status == DeflateStatus::Init && s.wrap == 0 {
        s.status = DeflateStatus::Busy;
    }
    if s.status == DeflateStatus::Init {
        // CMF/FLG: method + window size, then level flags and the preset bit.
        let mut header: u32 = (Z_DEFLATED as u32 + ((s.w_bits - 8) << 4)) << 8;
        let level_flags: u32 =
            if s.strategy.as_c_int() >= Strategy::HuffmanOnly.as_c_int() || s.level < 2 {
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
        // FCHECK: make the 16-bit header a multiple of 31.
        header += 31 - (header % 31);
        put_short_msb(s, header as u16);

        // Emit the Adler-32 of the preset dictionary, if one was set.
        if s.strstart != 0 {
            put_short_msb(s, (io.adler >> 16) as u16);
            put_short_msb(s, (io.adler & 0xffff) as u16);
        }
        // The stream checksum now covers only the uncompressed data.
        io.adler = DeflateState::initial_adler(s.wrap);
        s.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer.
        s.flush_pending(io);
        if s.pending != 0 {
            s.last_flush = -1;
            return ReturnCode::Ok;
        }
    }

    // ---------------------------- gzip header ----------------------------
    #[cfg(feature = "gzip")]
    {
        if s.status == DeflateStatus::Gzip {
            // The gzip header checksum is a CRC-32 over the header bytes.
            io.adler = crc32(0, &[]);
            s.put_byte(31);
            s.put_byte(139);
            s.put_byte(8);

            // Extract the header fields as owned/Copy values so `s` can be
            // mutated freely below without holding a borrow of `s.gzhead`.
            let head_info = s.gzhead.as_ref().map(|h| {
                (
                    h.text,
                    h.hcrc,
                    h.extra.is_some(),
                    h.name.is_some(),
                    h.comment.is_some(),
                    h.time,
                    h.os,
                    h.extra.as_ref().map_or(0usize, |e| e.len()),
                )
            });
            let xfl: u8 = if s.level == 9 {
                2
            } else if s.strategy.as_c_int() >= Strategy::HuffmanOnly.as_c_int() || s.level < 2 {
                4
            } else {
                0
            };

            match head_info {
                None => {
                    // Default header: zero MTIME/flags, XFL, then OS.
                    s.put_byte(0);
                    s.put_byte(0);
                    s.put_byte(0);
                    s.put_byte(0);
                    s.put_byte(0);
                    s.put_byte(xfl);
                    s.put_byte(OS_CODE);
                    s.status = DeflateStatus::Busy;
                    s.flush_pending(io);
                    if s.pending != 0 {
                        s.last_flush = -1;
                        return ReturnCode::Ok;
                    }
                }
                Some((text, hcrc, has_extra, has_name, has_comment, time, os, extra_len)) => {
                    let flag: u8 = (if text { 1 } else { 0 })
                        + (if hcrc { 2 } else { 0 })
                        + (if has_extra { 4 } else { 0 })
                        + (if has_name { 8 } else { 0 })
                        + (if has_comment { 16 } else { 0 });
                    s.put_byte(flag);
                    s.put_byte((time & 0xff) as u8);
                    s.put_byte(((time >> 8) & 0xff) as u8);
                    s.put_byte(((time >> 16) & 0xff) as u8);
                    s.put_byte(((time >> 24) & 0xff) as u8);
                    s.put_byte(xfl);
                    s.put_byte((os & 0xff) as u8);
                    if has_extra {
                        // The 2-byte little-endian XLEN (low 16 bits).
                        s.put_byte((extra_len & 0xff) as u8);
                        s.put_byte(((extra_len >> 8) & 0xff) as u8);
                    }
                    if hcrc {
                        io.adler = crc32(io.adler, &s.pending_buf[..s.pending]);
                    }
                    s.gzindex = 0;
                    s.status = DeflateStatus::Extra;
                }
            }
        }

        if s.status == DeflateStatus::Extra {
            // Clone the "extra" bytes so the pending buffer (a field of `s`) can
            // be mutated while we read the source data.
            if let Some(extra) = s.gzhead.as_ref().and_then(|h| h.extra.clone()) {
                let gz_hcrc = s.gzhead.as_ref().is_some_and(|h| h.hcrc);
                let extra_len = extra.len() & 0xffff;
                let mut beg = s.pending;
                let mut left = extra_len - s.gzindex;
                while s.pending + left > s.pending_buf_size {
                    let copy = s.pending_buf_size - s.pending;
                    s.pending_buf[s.pending..s.pending + copy]
                        .copy_from_slice(&extra[s.gzindex..s.gzindex + copy]);
                    s.pending = s.pending_buf_size;
                    hcrc_update(s, io, gz_hcrc, beg);
                    s.gzindex += copy;
                    s.flush_pending(io);
                    if s.pending != 0 {
                        s.last_flush = -1;
                        return ReturnCode::Ok;
                    }
                    beg = 0;
                    left -= copy;
                }
                s.pending_buf[s.pending..s.pending + left]
                    .copy_from_slice(&extra[s.gzindex..s.gzindex + left]);
                s.pending += left;
                hcrc_update(s, io, gz_hcrc, beg);
                s.gzindex = 0;
            }
            s.status = DeflateStatus::Name;
        }

        if s.status == DeflateStatus::Name {
            if let Some(name) = s.gzhead.as_ref().and_then(|h| h.name.clone()) {
                let gz_hcrc = s.gzhead.as_ref().is_some_and(|h| h.hcrc);
                let mut beg = s.pending;
                loop {
                    if s.pending == s.pending_buf_size {
                        hcrc_update(s, io, gz_hcrc, beg);
                        s.flush_pending(io);
                        if s.pending != 0 {
                            s.last_flush = -1;
                            return ReturnCode::Ok;
                        }
                        beg = 0;
                    }
                    // C reads bytes (including the C-string NUL) until it hits
                    // 0. Our stored name has no NUL, so emit a 0 past its end.
                    let val = if s.gzindex < name.len() {
                        name[s.gzindex]
                    } else {
                        0
                    };
                    s.gzindex += 1;
                    s.put_byte(val);
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s, io, gz_hcrc, beg);
                s.gzindex = 0;
            }
            s.status = DeflateStatus::Comment;
        }

        if s.status == DeflateStatus::Comment {
            if let Some(comment) = s.gzhead.as_ref().and_then(|h| h.comment.clone()) {
                let gz_hcrc = s.gzhead.as_ref().is_some_and(|h| h.hcrc);
                let mut beg = s.pending;
                loop {
                    if s.pending == s.pending_buf_size {
                        hcrc_update(s, io, gz_hcrc, beg);
                        s.flush_pending(io);
                        if s.pending != 0 {
                            s.last_flush = -1;
                            return ReturnCode::Ok;
                        }
                        beg = 0;
                    }
                    let val = if s.gzindex < comment.len() {
                        comment[s.gzindex]
                    } else {
                        0
                    };
                    s.gzindex += 1;
                    s.put_byte(val);
                    if val == 0 {
                        break;
                    }
                }
                hcrc_update(s, io, gz_hcrc, beg);
            }
            s.status = DeflateStatus::Hcrc;
        }

        if s.status == DeflateStatus::Hcrc {
            let gz_hcrc = s.gzhead.as_ref().is_some_and(|h| h.hcrc);
            if gz_hcrc {
                if s.pending + 2 > s.pending_buf_size {
                    s.flush_pending(io);
                    if s.pending != 0 {
                        s.last_flush = -1;
                        return ReturnCode::Ok;
                    }
                }
                s.put_byte((io.adler & 0xff) as u8);
                s.put_byte(((io.adler >> 8) & 0xff) as u8);
                io.adler = crc32(0, &[]);
            }
            s.status = DeflateStatus::Busy;

            // Compression must start with an empty pending buffer.
            s.flush_pending(io);
            if s.pending != 0 {
                s.last_flush = -1;
                return ReturnCode::Ok;
            }
        }
    }

    // ------------------------- block production --------------------------
    if io.avail_in != 0
        || s.lookahead != 0
        || (flush != Z_NO_FLUSH && s.status != DeflateStatus::Finish)
    {
        // Dispatch precedence (deflate.c L1217-L1220): level 0 forces stored
        // (overriding strategy); then Huffman-only, then RLE, then the
        // per-level configured producer.
        let cf = if s.level == 0 {
            CompressFunc::Stored
        } else if s.strategy == Strategy::HuffmanOnly {
            CompressFunc::Huff
        } else if s.strategy == Strategy::Rle {
            CompressFunc::Rle
        } else {
            CONFIGURATION_TABLE[s.level as usize].func
        };
        let bstate = match cf {
            CompressFunc::Stored => stored::deflate_stored(s, io, flush),
            CompressFunc::Fast => fast::deflate_fast(s, io, flush),
            CompressFunc::Slow => slow::deflate_slow(s, io, flush),
            CompressFunc::Rle => rle::deflate_rle(s, io, flush),
            CompressFunc::Huff => huff::deflate_huff(s, io, flush),
        };

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            s.status = DeflateStatus::Finish;
        }
        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if io.avail_out == 0 {
                s.last_flush = -1; // avoid BUF_ERROR on the next call
            }
            // For a partial block with no output room, the next call reuses the
            // same flush parameter to complete it; no empty block is emitted
            // here, bounding tiny-buffer output to at most one empty block.
            return ReturnCode::Ok;
        }
        if bstate == BlockState::BlockDone {
            if flush == Z_PARTIAL_FLUSH {
                trees::_tr_align(s);
            } else if flush != Z_BLOCK {
                // FULL_FLUSH or SYNC_FLUSH: emit an empty stored block. For a
                // full flush inflate_sync() treats this as a resync marker.
                trees::_tr_stored_block(s, None, 0, false);
                if flush == Z_FULL_FLUSH {
                    clear_hash(s); // forget the match history
                    if s.lookahead == 0 {
                        s.strstart = 0;
                        s.block_start = 0;
                        s.insert = 0;
                    }
                }
            }
            s.flush_pending(io);
            if io.avail_out == 0 {
                s.last_flush = -1;
                return ReturnCode::Ok;
            }
        }
    }

    if flush != Z_FINISH {
        return ReturnCode::Ok;
    }
    if s.wrap <= 0 {
        return ReturnCode::StreamEnd;
    }

    // ------------------------------ trailer ------------------------------
    if s.wrap == 2 {
        // gzip: CRC-32 then ISIZE (total_in mod 2^32), both least-significant
        // byte first.
        s.put_byte((io.adler & 0xff) as u8);
        s.put_byte(((io.adler >> 8) & 0xff) as u8);
        s.put_byte(((io.adler >> 16) & 0xff) as u8);
        s.put_byte(((io.adler >> 24) & 0xff) as u8);
        s.put_byte((io.total_in & 0xff) as u8);
        s.put_byte(((io.total_in >> 8) & 0xff) as u8);
        s.put_byte(((io.total_in >> 16) & 0xff) as u8);
        s.put_byte(((io.total_in >> 24) & 0xff) as u8);
    } else {
        // zlib: Adler-32, most-significant 16 bits first.
        put_short_msb(s, (io.adler >> 16) as u16);
        put_short_msb(s, (io.adler & 0xffff) as u16);
    }
    s.flush_pending(io);
    // Negate wrap so the trailer is written only once.
    if s.wrap > 0 {
        s.wrap = -s.wrap;
    }
    if s.pending != 0 {
        ReturnCode::Ok
    } else {
        ReturnCode::StreamEnd
    }
}

/// Compresses `input` into `output`, advancing the stream by one call.
///
/// This is the streaming entry point, the idiomatic form of C `deflate()`.
/// Because [`ZStream`] carries no cursor fields, the input and output buffers
/// are supplied per call and progress is returned in a [`DeflateOutcome`]
/// (`consumed` input bytes, `produced` output bytes, and the zlib `code`). The
/// running totals, checksum, and detected data type are read from and written
/// back to `strm` on every return path.
///
/// `flush` is one of the raw flush constants
/// [`Z_NO_FLUSH`] ..=
/// [`Z_BLOCK`]; a value outside `0..=5` yields
/// [`ReturnCode::StreamError`].
#[must_use]
pub fn deflate<A: Allocator>(
    strm: &mut ZStream<A>,
    input: &[u8],
    output: &mut [u8],
    flush: i32,
) -> DeflateOutcome {
    // Flush range check (C: `flush > Z_BLOCK || flush < 0`).
    if !(0..=Z_BLOCK).contains(&flush) {
        return DeflateOutcome {
            code: ReturnCode::StreamError,
            consumed: 0,
            produced: 0,
        };
    }

    // Build the IoContext seeded from the stream's running totals + checksum.
    let mut io = IoContext::new(input, output);
    io.total_in = strm.total_in;
    io.total_out = strm.total_out;
    io.adler = strm.adler;

    let code = match strm.deflate_state_mut() {
        Some(s) => deflate_run(s, &mut io, flush),
        None => ReturnCode::StreamError,
    };

    // Write back stream-visible state on every path (mirrors z_stream fields).
    strm.total_in = io.total_in;
    strm.total_out = io.total_out;
    strm.adler = io.adler;
    if let Some(dt) = strm.deflate_state().map(|s| s.data_type.as_c_int()) {
        strm.data_type = dt;
    }

    DeflateOutcome {
        code,
        consumed: io.next_in,
        produced: io.next_out,
    }
}

// ===========================================================================
// deflateParams.
// ===========================================================================

/// Dynamically updates the compression `level` and `strategy` mid-stream.
///
/// Port of C `deflateParams` (`deflate.c` L774-L816). When the change would
/// alter the active block producer and some data has already been processed
/// (`last_flush != -2`), the pending block is first flushed via an internal
/// [`deflate`] call with [`Z_BLOCK`] — hence the
/// `input`/`output` buffers are required here too. If input remains, or the
/// window still holds unflushed data after that flush, the switch cannot
/// complete and [`ReturnCode::BufError`] is returned. Switching *away* from
/// level 0 slides or clears the hash table as C does.
///
/// The returned [`DeflateOutcome`] carries any I/O performed by the internal
/// flush (`consumed`/`produced` are `0` when no flush was needed).
#[must_use]
pub fn deflate_params<A: Allocator>(
    strm: &mut ZStream<A>,
    input: &[u8],
    output: &mut [u8],
    level: i32,
    strategy: Strategy,
) -> DeflateOutcome {
    // Resolve default level and validate the range (strategy is validated by
    // construction of the `Strategy` enum).
    let level = if level == Z_DEFAULT_COMPRESSION {
        6
    } else {
        level
    };
    if !(0..=9).contains(&level) {
        return DeflateOutcome {
            code: ReturnCode::StreamError,
            consumed: 0,
            produced: 0,
        };
    }

    let (cur_level, cur_strategy, last_flush) = match strm.deflate_state() {
        Some(s) => (s.level, s.strategy, s.last_flush),
        None => {
            return DeflateOutcome {
                code: ReturnCode::StreamError,
                consumed: 0,
                produced: 0,
            };
        }
    };
    let func_cur = CONFIGURATION_TABLE[cur_level as usize].func;
    let func_new = CONFIGURATION_TABLE[level as usize].func;

    let mut consumed = 0usize;
    let mut produced = 0usize;

    if (strategy != cur_strategy || func_cur != func_new) && last_flush != -2 {
        // Flush the current block before switching producers.
        let outcome = deflate(strm, input, output, Z_BLOCK);
        consumed = outcome.consumed;
        produced = outcome.produced;
        if outcome.code == ReturnCode::StreamError {
            return outcome;
        }
        let leftover_in = input.len() - consumed;
        let window_pending = match strm.deflate_state() {
            Some(s) => (s.strstart as isize - s.block_start) as usize + s.lookahead,
            None => {
                return DeflateOutcome {
                    code: ReturnCode::StreamError,
                    consumed,
                    produced,
                };
            }
        };
        if leftover_in != 0 || window_pending != 0 {
            return DeflateOutcome {
                code: ReturnCode::BufError,
                consumed,
                produced,
            };
        }
    }

    // Apply the change.
    let s = match strm.deflate_state_mut() {
        Some(s) => s,
        None => {
            return DeflateOutcome {
                code: ReturnCode::StreamError,
                consumed,
                produced,
            };
        }
    };
    if s.level != level {
        if s.level == 0 && s.matches != 0 {
            // Leaving level 0: reconcile the hash tables with the data that was
            // stored (matches == 1 → slide, otherwise clear).
            if s.matches == 1 {
                s.slide_hash();
            } else {
                clear_hash(s);
            }
            s.matches = 0;
        }
        s.level = level;
        let cfg = &CONFIGURATION_TABLE[level as usize];
        s.max_lazy_match = cfg.max_lazy as usize;
        s.good_match = cfg.good_length as usize;
        s.nice_match = cfg.nice_length as i32;
        s.max_chain_length = cfg.max_chain as usize;
    }
    s.strategy = strategy;

    DeflateOutcome {
        code: ReturnCode::Ok,
        consumed,
        produced,
    }
}

// ===========================================================================
// Teardown & copy — deflateEnd / deflateCopy.
// ===========================================================================

/// Frees the compression state associated with `strm`.
///
/// Port of C `deflateEnd` (`deflate.c` L1293-L1310). Thanks to RAII, dropping
/// the boxed [`DeflateState`] frees its owned buffers (window, hash tables,
/// pending/symbol buffers) automatically in reverse allocation order — the
/// explicit `TRY_FREE` sequence of C is unnecessary. The observable return
/// code is preserved: [`ZlibError::DataError`] if the stream was still busy
/// (freed prematurely), otherwise [`ReturnCode::Ok`].
///
/// # Errors
///
/// * [`ZlibError::StreamError`] — `strm` has no deflate state installed.
/// * [`ZlibError::DataError`] — the stream was mid-compression (`Busy`).
pub fn deflate_end<A: Allocator>(strm: &mut ZStream<A>) -> DeflateResult {
    let status = match strm.deflate_state() {
        Some(s) => s.status,
        None => return Err(ZlibError::StreamError),
    };
    // Dropping the state frees all owned buffers (RAII replaces TRY_FREE).
    strm.clear_state();
    if status == DeflateStatus::Busy {
        Err(ZlibError::DataError)
    } else {
        Ok(ReturnCode::Ok)
    }
}

/// Copies the compression state of `source` into `dest`, producing an
/// independent stream that continues identically.
///
/// Port of C `deflateCopy` (`deflate.c` L1317-L1377). Because [`DeflateState`]
/// derives [`Clone`] and holds its buffers as owned `Vec`s (and its tree
/// arrays are `Copy`), a deep copy is a single `clone()` with no manual
/// pointer re-basing — the large `zmemcpy`/pointer-fix-up body of C collapses
/// away. The stream-level bookkeeping (totals, checksum, data type, message)
/// is copied to match C's whole-`z_stream` copy.
///
/// # Errors
///
/// [`ZlibError::StreamError`] if `source` has no deflate state installed.
pub fn deflate_copy<A: Allocator>(dest: &mut ZStream<A>, source: &ZStream<A>) -> DeflateResult {
    let cloned = {
        let src = source.deflate_state().ok_or(ZlibError::StreamError)?;
        Box::new(src.clone())
    };
    dest.total_in = source.total_in;
    dest.total_out = source.total_out;
    dest.adler = source.adler;
    dest.data_type = source.data_type;
    dest.msg = source.msg;
    dest.set_deflate_state(cloned);
    Ok(ReturnCode::Ok)
}
