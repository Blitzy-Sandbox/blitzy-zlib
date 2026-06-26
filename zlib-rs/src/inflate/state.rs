//! Core `inflate` decompression engine.
//!
//! This module is the safe-Rust translation of zlib's `inflate.c`
//! (the `inflate()` driver, its lifecycle helpers, and the auxiliary
//! `inflate*` functions) together with `struct inflate_state` and the
//! `inflate_mode` enum from `inflate.h` (zlib 1.3.2.1). It is the largest and
//! most intricate module of `crate::inflate`.
//!
//! # What this module provides
//!
//! * [`InflateMode`] — the decompressor state-machine modes, a faithful port of
//!   the C `inflate_mode` enum (32 variants, first discriminant `Head = 16180`,
//!   the deliberately large corruption sentinel from the C source).
//! * [`InflateState`] — the owned decompressor state. The C struct's hand-managed
//!   sliding `window` becomes an owned [`Vec<u8>`]; the integer status codes
//!   become a [`InflateMode`] enum matched in a single labeled loop; and
//!   `inflateEnd`'s manual deallocation is realized by [`Drop`] of the owned
//!   buffers (there is no explicit `Drop` impl — the `Vec` frees itself).
//! * [`InflateResult`] — the value returned by [`InflateState::inflate`]: how
//!   many input bytes were consumed, how many output bytes were produced, and
//!   the typed [`crate::error::Result`] outcome.
//!
//! # Control-flow translation
//!
//! The C decompressor dispatches through a single `switch (state->mode)` and
//! exits the dispatch with 20 `goto inf_leave` statements when input or output
//! is exhausted. This port reproduces that exactly with a labeled
//! `'inf: loop { match self.mode { … } }`; every `goto inf_leave` becomes a
//! `break 'inf` that lands in the one shared epilogue after the loop, which
//! performs the window update, total/check bookkeeping, `data_type` computation,
//! and return-code selection — all in one place, just like C.
//!
//! The C `LOAD`/`RESTORE`/`PULLBYTE`/`NEEDBITS`/`BITS`/`DROPBITS`/`BYTEBITS`/
//! `INITBITS` macros are reproduced as function-local `macro_rules!` over the
//! local bit accumulator (`hold: u64`, `bits: u32`) and the input/output cursors,
//! preserving the exact fill/consume order required for bit-for-bit
//! compatibility.
//!
//! # Safety and `no_std`
//!
//! This module contains **zero `unsafe`** (the crate declares
//! `#![forbid(unsafe_code)]`): the sliding window is a bounds-checked
//! [`Vec<u8>`] and every table access is checked slice indexing. It uses only
//! `core`/`alloc`, so the crate's `no_std` build path is preserved.
//!
//! # Relationship to the stream layer and the FFI shim
//!
//! Following the same division of labor as the sibling `deflate` module, this
//! engine does **not** depend on `crate::stream`: the decompressor's public
//! `z_stream` scalars (`total_in`, `total_out`, `adler`, `msg`, `data_type`) are
//! exposed as fields on [`InflateState`] (see [`InflateState::check`] for the
//! running checksum that becomes `adler`), and the owning stream / FFI layer
//! copies them onto the public stream after each call. [`InflateState::inflate`]
//! returns the per-call consumed/produced counts so the caller can advance its
//! own `next_in`/`next_out`.
//!
//! # Note on the fast path
//!
//! zlib invokes the `inflate_fast()` inner loop (the sibling `fast.rs` module)
//! from the `LEN` state when `have >= 6 && left >= 258`. That routine is a pure
//! *performance* optimization: it and the per-code slow path that also lives
//! here produce byte-identical output. The `LEN` arm wires this in exactly as C
//! does — calling [`crate::inflate::fast::inflate_fast`] under those entry
//! conditions and falling back to the complete, self-contained slow per-code
//! decode whenever input or output is too small for the fast loop. Output is
//! bit-exact with C zlib either way; only peak throughput differs.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use crate::checksum::adler32;
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{DEF_WBITS, Flush, Z_DEFLATED};
use crate::error::{Result, ReturnCode, ZlibError};
use crate::inflate::fast::inflate_fast;
use crate::inflate::fixed::{DISTFIX, LENFIX};
use crate::inflate::tables::{Code, CodeType, ENOUGH, inflate_table};
use crate::stream::Allocator;

#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

/// Permutation of code-length-code lengths used while reading a dynamic block's
/// header (C `inflate()` local `order[19]`, `inflate.c`). The 19 code-length
/// codes are transmitted in this order so the most common ones come first.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Possible `inflate` modes between [`InflateState::inflate`] calls.
///
/// A faithful port of the C `inflate_mode` enum (`inflate.h` lines 20-53),
/// preserving declaration order so that the ordering comparisons the C engine
/// performs (`mode < BAD`, `mode < CHECK`) translate directly via the derived
/// [`Ord`]. The first discriminant is the C sentinel `HEAD = 16180`; the
/// remaining variants auto-increment from there. The discriminant *values* are
/// not an ABI contract (Rust matches on variants, not integers) — the explicit
/// `16180` only preserves the C corruption-detection intent.
///
/// Comment legend (from the C source): `i:` waiting for input, `o:` waiting for
/// output space.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InflateMode {
    // ---- header --------------------------------------------------------
    /// i: waiting for magic header. The sentinel `16180` aids corruption
    /// detection, exactly as in the C source.
    Head = 16180,
    /// i: waiting for method and flags (gzip).
    Flags,
    /// i: waiting for modification time (gzip).
    Time,
    /// i: waiting for extra flags and operating system (gzip).
    Os,
    /// i: waiting for extra length (gzip).
    Exlen,
    /// i: waiting for extra bytes (gzip).
    Extra,
    /// i: waiting for end of file name (gzip).
    Name,
    /// i: waiting for end of comment (gzip).
    Comment,
    /// i: waiting for header CRC (gzip).
    Hcrc,
    /// i: waiting for dictionary check value.
    DictId,
    /// waiting for an `inflateSetDictionary` call.
    Dict,
    // ---- block dispatch ------------------------------------------------
    /// i: waiting for type bits, including the last-flag bit.
    Type,
    /// i: same as [`InflateMode::Type`], but skip the check that exits
    /// `inflate` on a new block boundary.
    TypeDo,
    /// i: waiting for stored size (length and its complement).
    Stored,
    /// i/o: same as [`InflateMode::Copy`] below, but only the first time in
    /// (the C `COPY_` state).
    CopyUnder,
    /// i/o: waiting for input or output to copy a stored block.
    Copy,
    /// i: waiting for dynamic block table lengths.
    Table,
    /// i: waiting for code-length-code lengths.
    LenLens,
    /// i: waiting for length/literal and distance code lengths.
    CodeLens,
    // ---- code decoding -------------------------------------------------
    /// i: same as [`InflateMode::Len`] below, but only the first time in
    /// (the C `LEN_` state).
    LenUnder,
    /// i: waiting for a length/literal/end-of-block code.
    Len,
    /// i: waiting for length extra bits.
    LenExt,
    /// i: waiting for a distance code.
    Dist,
    /// i: waiting for distance extra bits.
    DistExt,
    /// o: waiting for output space to copy a matched string.
    Match,
    /// o: waiting for output space to write a literal.
    Lit,
    // ---- trailer -------------------------------------------------------
    /// i: waiting for the 32-bit check value.
    Check,
    /// i: waiting for the 32-bit length (gzip ISIZE).
    Length,
    /// finished check, done — remain here until reset.
    Done,
    /// got a data error — remain here until reset.
    Bad,
    /// got an `inflate` memory error — remain here until reset.
    Mem,
    /// looking for synchronization bytes to restart `inflate`.
    Sync,
}

// The C `inflate_mode` enum has exactly 32 modes (HEAD..SYNC). Keeping this in
// sync is a guard against an accidental insertion/deletion changing the
// `mode < BAD` / `mode < CHECK` ordering the epilogue relies on.
const _: () = assert!((InflateMode::Sync as isize) - (InflateMode::Head as isize) == 31);

/// The value returned by [`InflateState::inflate`].
///
/// `inflate` consumes a prefix of the supplied input slice and writes a prefix
/// of the supplied output slice. The caller advances its own `next_in`/
/// `next_out` cursors by [`consumed`](Self::consumed) / [`produced`](Self::produced)
/// and reads the running counters and checksum from the [`InflateState`]
/// directly (mirroring how the C engine updates `strm->total_in`,
/// `strm->total_out`, `strm->adler`, `strm->msg`, and `strm->data_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InflateResult {
    /// Number of input bytes consumed from the front of the input slice.
    pub consumed: usize,
    /// Number of output bytes written to the front of the output slice.
    pub produced: usize,
    /// The typed outcome: `Ok(ReturnCode::{Ok,StreamEnd,NeedDict})` for the
    /// normal control-flow signals, or `Err(ZlibError::{DataError,MemError,
    /// BufError,StreamError})` for failures, matching the integer codes the C
    /// `inflate()` returns.
    pub status: Result<ReturnCode>,
}

/// State maintained between [`InflateState::inflate`] calls.
///
/// A safe, owning translation of the C `struct inflate_state` (`inflate.h`
/// lines 82-126). Field names follow the C originals (in Rust `snake_case`) so
/// the port reads as a direct transliteration of the reference implementation.
///
/// Two groups of C members are handled differently here:
///
/// * The `strm` back-pointer is absent. The public `z_stream` scalars are
///   instead exposed as the [`total_in`](Self::total_in),
///   [`total_out`](Self::total_out), [`check`](Self::check) (the running
///   checksum that becomes `adler`), [`msg`](Self::msg), and
///   [`data_type`](Self::data_type) fields, which the owning stream / FFI layer
///   copies onto the public stream after each call.
/// * The decode tables `lencode`/`distcode`/`next` are raw pointers in C; here
///   they are `usize` **offsets** into the owned [`codes`](Self::codes) array,
///   so cloning the state (for `inflateCopy`) keeps them valid automatically.
///
/// `inflateEnd`'s deallocation is realized by the automatic [`Drop`] of the
/// owned [`window`](Self::window) `Vec` (and of the `Box<InflateState>` the
/// stream layer wraps this in); no explicit `Drop` impl is required.
#[derive(Clone)]
pub struct InflateState {
    // ---- public z_stream scalars (mirrored here, copied out by caller) ----
    /// Protected copy of the running check value; also the public `adler`
    /// (C `state->check`, and `strm->adler`). Adler-32 for zlib, CRC-32 for
    /// gzip.
    pub check: u32,
    /// Total input consumed across all calls (C `strm->total_in`).
    pub total_in: u64,
    /// Total output produced across all calls (C `strm->total_out`).
    pub total_out: u64,
    /// The most recent error/diagnostic message, if any (C `strm->msg`).
    pub msg: Option<&'static str>,
    /// The `data_type` hint computed on the most recent return
    /// (C `strm->data_type`).
    pub data_type: i32,

    // ---- core inflate_state fields ----------------------------------------
    /// Current inflate mode (C `state->mode`).
    pub mode: InflateMode,
    /// `true` if processing the last block (C `state->last`).
    pub last: bool,
    /// Wrap flags: bit 0 = expect zlib header, bit 1 = expect gzip header,
    /// bit 2 = validate the check value (C `state->wrap`).
    pub wrap: i32,
    /// `true` if a dictionary has been provided (C `state->havedict`).
    pub havedict: bool,
    /// gzip header method and flags; `0` for a zlib stream, or `-1` for raw or
    /// "no header seen yet" (C `state->flags`).
    pub flags: i32,
    /// zlib-header maximum match distance (C `state->dmax`).
    pub dmax: u32,
    /// Protected copy of the total output count, used for the gzip ISIZE check
    /// (C `state->total`).
    pub total: u64,
    /// Captured gzip header being populated, when [`inflate_get_header`] has
    /// been called (C `state->head`). `None` means "do not capture".
    #[cfg(feature = "gzip")]
    pub head: Option<GzHeader>,

    // ---- sliding window ----------------------------------------------------
    /// log base 2 of the requested window size (C `state->wbits`).
    pub wbits: u32,
    /// Window size, or `0` when no window is in use (C `state->wsize`).
    pub wsize: usize,
    /// Number of valid bytes in the window (C `state->whave`).
    pub whave: usize,
    /// Window write index (C `state->wnext`).
    pub wnext: usize,
    /// Owned sliding window; empty until lazily allocated (C `state->window`).
    pub window: Vec<u8>,

    // ---- bit accumulator (saved between calls) -----------------------------
    /// Input bit accumulator (C `state->hold`); a `u64` per the bit-exactness
    /// design.
    pub hold: u64,
    /// Number of bits currently in [`hold`](Self::hold) (C `state->bits`).
    pub bits: u32,

    // ---- string / stored-block copy ----------------------------------------
    /// Literal value, or length of data to copy (C `state->length`).
    pub length: u32,
    /// Distance back to copy a string from (C `state->offset`).
    pub offset: u32,

    // ---- table / code decoding ---------------------------------------------
    /// Extra bits needed (C `state->extra`).
    pub extra: u32,
    /// Offset into [`codes`](Self::codes) of the length/literal table
    /// (C `state->lencode`).
    pub lencode: usize,
    /// Offset into [`codes`](Self::codes) of the distance table
    /// (C `state->distcode`).
    pub distcode: usize,
    /// Index bits for the length/literal table (C `state->lenbits`).
    pub lenbits: usize,
    /// Index bits for the distance table (C `state->distbits`).
    pub distbits: usize,

    // ---- dynamic table building --------------------------------------------
    /// Number of code-length-code lengths (C `state->ncode`).
    pub ncode: usize,
    /// Number of length code lengths (C `state->nlen`).
    pub nlen: usize,
    /// Number of distance code lengths (C `state->ndist`).
    pub ndist: usize,
    /// Number of code lengths decoded into [`lens`](Self::lens) so far
    /// (C `state->have`).
    pub have: usize,
    /// Next free offset into [`codes`](Self::codes) (C `state->next`).
    pub next: usize,
    /// Temporary storage for code lengths (C `state->lens[320]`).
    pub lens: [u16; 320],
    /// Work area for code-table building (C `state->work[288]`).
    pub work: [u16; 288],
    /// Decode tables (C `state->codes[ENOUGH]`).
    pub codes: [Code; ENOUGH],

    // ---- misc --------------------------------------------------------------
    /// If `false`, allow an "invalid distance too far" (C `state->sane`);
    /// defaults to `true`.
    pub sane: bool,
    /// Bits back of the last unprocessed length/literal, `-1` when not set
    /// (C `state->back`).
    pub back: i32,
    /// Initial length of the current match (C `state->was`).
    pub was: u32,

    // ---- custom allocation extension point ---------------------------------
    /// Optional caller-installed [`Allocator`] used to obtain the sliding
    /// [`window`](Self::window) (the only heap buffer `inflate` allocates,
    /// lazily, in [`update_window`](Self::update_window)). `None` selects the
    /// global allocator via a fallible `try_reserve_exact`.
    ///
    /// This is the safe-core analogue of the C `zalloc`/`zfree` callbacks: a
    /// memory-limiting or pooling allocator is genuinely consulted, and its
    /// failure surfaces as `Z_MEM_ERROR` (the C `updatewindow` failure path).
    /// An [`Rc`] is used (rather than [`Box`]) so the state stays [`Clone`] for
    /// `inflateCopy` — the clone shares the same allocator handle.
    pub(crate) allocator: Option<Rc<dyn Allocator>>,
}

impl Default for InflateState {
    /// Returns a zeroed state in [`InflateMode::Head`] with no window allocated,
    /// equivalent to the freshly `zmemzero`'d struct that C `inflateInit2_`
    /// produces before calling `inflateReset2`. Most scalar fields are
    /// overwritten by [`InflateState::reset2`] before any decoding begins.
    fn default() -> Self {
        Self {
            check: 0,
            total_in: 0,
            total_out: 0,
            msg: None,
            data_type: 0,
            mode: InflateMode::Head,
            last: false,
            wrap: 0,
            havedict: false,
            flags: -1,
            dmax: 32768,
            total: 0,
            #[cfg(feature = "gzip")]
            head: None,
            wbits: 0,
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: Vec::new(),
            hold: 0,
            bits: 0,
            length: 0,
            offset: 0,
            extra: 0,
            lencode: 0,
            distcode: 0,
            lenbits: 0,
            distbits: 0,
            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            next: 0,
            lens: [0; 320],
            work: [0; 288],
            codes: [Code::default(); ENOUGH],
            sane: true,
            back: -1,
            was: 0,
            allocator: None,
        }
    }
}

// ===========================================================================
// Lifecycle: construction, reset, window-bits overloading, priming.
//
// These methods port the C `inflateInit2_`, `inflateResetKeep`, `inflateReset`,
// `inflateReset2`, `inflatePrime`, `inflate_fixed`, and `updatewindow`
// functions (inflate.c L120-L460, inftrees.c `inflate_fixed`). In the safe-Rust
// model the C "allocate the state via zalloc" step becomes `Box::new`, and the
// "free the window via zfree" step is performed automatically when the owned
// `window: Vec<u8>` is dropped (RAII replaces `inflateEnd`; see the `Drop`
// note at the end of this file).
// ===========================================================================

impl InflateState {
    /// Port of C `inflateInit2_` (inflate.c L120-L143).
    ///
    /// Allocates a fresh heap-owned state (the Rust `Box` is the single owner,
    /// replacing the C `zalloc`'d `internal_state*`), leaves the sliding window
    /// unallocated (C defers the window allocation until the first
    /// `updatewindow`), and then applies `reset2` to validate and record the
    /// `windowBits` overloading. Returns the boxed state ready to be parked in
    /// `stream.rs`'s `StreamState::Inflate(Box<InflateState>)` variant.
    ///
    /// `window_bits` follows the zlib overloading convention decoded by
    /// [`InflateState::reset2`]:
    /// * `8..=15`   — zlib container (RFC 1950),
    /// * `-8..=-15` — raw DEFLATE (RFC 1951, no header/trailer),
    /// * `24..=31`  — gzip container (RFC 1952),
    /// * `40..=47`  — automatic zlib/gzip detection.
    pub fn new(window_bits: i32) -> Result<Box<InflateState>> {
        // C `inflateInit2_` zeroes the struct, sets `strm->state`, then sets
        // `state->window = Z_NULL` and `state->wbits = 0` before delegating to
        // `inflateReset2`. `Self::default()` reproduces the zeroed struct with
        // `window = Vec::new()` (unallocated) and `wbits = 0`.
        let mut state = Box::new(Self::default());
        state.reset2(window_bits)?;
        Ok(state)
    }

    /// Port of C `inflateInit_` — initialize with the default window size
    /// ([`DEF_WBITS`] == 15, a 32 KiB zlib window).
    pub fn new_default() -> Result<Box<InflateState>> {
        Self::new(DEF_WBITS)
    }

    /// Like [`InflateState::new`], but installs a caller-supplied
    /// [`Allocator`] used for the lazily-allocated sliding window.
    ///
    /// This is the safe-core analogue of `inflateInit2_` with custom
    /// `zalloc`/`zfree` callbacks: the FFI shim (`libz-rs-sys`) builds an
    /// allocator that forwards to the C callbacks and passes it here, so a
    /// memory-limiting or pooling allocator is genuinely consulted when the
    /// window is allocated, and an allocation failure surfaces as
    /// `Z_MEM_ERROR`. Passing `None` is equivalent to [`InflateState::new`]
    /// (the global allocator). The allocator handle is shared (cloned) by
    /// `inflateCopy`.
    pub fn new_in(
        window_bits: i32,
        allocator: Option<Rc<dyn Allocator>>,
    ) -> Result<Box<InflateState>> {
        let mut state = Self::new(window_bits)?;
        state.allocator = allocator;
        Ok(state)
    }

    /// Port of C `inflateResetKeep` (inflate.c L120 region / the
    /// `inflateResetKeep` body). Resets all decoding bookkeeping to the
    /// start-of-stream state **without** discarding the sliding-window
    /// allocation or its `wsize`/`whave`/`wnext` bookkeeping, so a caller can
    /// resume decoding a fresh member while reusing the already-grown window.
    ///
    /// Mirrors the C field-by-field reset exactly:
    /// * `total_in = total_out = total = 0`, `msg = NULL`,
    /// * if `wrap != 0` then the reported running checksum (`strm->adler` in C,
    ///   our merged [`InflateState::check`]) is seeded with `wrap & 1`
    ///   (1 for zlib/auto, 0 for gzip),
    /// * `mode = HEAD`, `last = 0`, `havedict = 0`, `flags = -1` ("raw or no
    ///   header seen yet"), `dmax = 32768`, `head = NULL`,
    /// * the bit accumulator is cleared (`hold = 0`, `bits = 0`),
    /// * `lencode = distcode = next = codes` (offset 0 in the unified
    ///   `codes` arena), `sane = 1`, `back = -1`.
    pub fn reset_keep(&mut self) -> ReturnCode {
        self.total_in = 0;
        self.total_out = 0;
        self.total = 0;
        self.msg = None;
        self.data_type = 0;
        if self.wrap != 0 {
            // C: `if (state->wrap) strm->adler = state->wrap & 1;`
            self.check = (self.wrap & 1) as u32;
        }
        self.mode = InflateMode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = -1;
        self.dmax = 32768;
        #[cfg(feature = "gzip")]
        {
            self.head = None;
        }
        self.hold = 0;
        self.bits = 0;
        // `lencode = distcode = next = state->codes` -> offset 0 in our arena.
        self.lencode = 0;
        self.distcode = 0;
        self.next = 0;
        self.sane = true;
        self.back = -1;
        ReturnCode::Ok
    }

    /// Port of C `inflateReset` (inflate.c). Logically empties the sliding
    /// window (`wsize = whave = wnext = 0`) — the backing `Vec` allocation is
    /// retained for reuse, matching C keeping `state->window` allocated — and
    /// then defers to [`InflateState::reset_keep`].
    pub fn reset(&mut self) -> ReturnCode {
        self.wsize = 0;
        self.whave = 0;
        self.wnext = 0;
        self.reset_keep()
    }

    /// Port of C `inflateReset2` (inflate.c L150-L175). Decodes the
    /// `windowBits` overloading into the `wrap` flag-set and the raw window
    /// size, validates the resulting window size, discards an incompatibly
    /// sized window, then performs a full [`InflateState::reset`].
    ///
    /// `wrap` bit layout (matching C): bit0 = expect/emit a zlib check value,
    /// bit1 = gzip wrapping, bit2 = "validate the check value" (toggled by
    /// [`InflateState::validate`]).
    pub fn reset2(&mut self, mut window_bits: i32) -> Result<ReturnCode> {
        // Extract wrap request and window bits from the windowBits parameter.
        let wrap: i32;
        if window_bits < 0 {
            // Raw DEFLATE: a negative value selects no header/trailer. C does
            // not bound-check the negative side here beyond the 8..=15 window
            // check below, but a value below -15 cannot yield a legal window,
            // so reject it up front for a precise error.
            if window_bits < -15 {
                return Err(ZlibError::StreamError);
            }
            wrap = 0;
            window_bits = -window_bits;
        } else {
            // C: `wrap = (windowBits >> 4) + 5;` encodes zlib (bit0), gzip
            // (bit1) and auto-detect (bits0|1) into the wrap flag-set.
            wrap = (window_bits >> 4) + 5;
            #[cfg(feature = "gzip")]
            {
                // C (with GUNZIP defined): `if (windowBits < 48) windowBits &= 15;`
                if window_bits < 48 {
                    window_bits &= 15;
                }
            }
            // When the `gzip` feature is disabled the C `GUNZIP` define is
            // absent, so `windowBits` is left untouched here; the gzip/auto
            // ranges (>= 24) then fail the 8..=15 validation below, leaving
            // only plain zlib windowBits 8..=15 acceptable — exactly the
            // behavior of a zlib built without gzip support.
        }

        // Validate the window size: a non-zero window must be 8..=15 bits.
        if window_bits != 0 && !(8..=15).contains(&window_bits) {
            return Err(ZlibError::StreamError);
        }

        // If a window is already allocated under a different size, drop it so
        // the next `update_window` reallocates at the new size. C: free the
        // window and set `state->window = Z_NULL` when `wbits` changes.
        if !self.window.is_empty() && self.wbits != window_bits as u32 {
            self.window = Vec::new();
        }

        // Record the configuration and perform a full reset.
        self.wrap = wrap;
        self.wbits = window_bits as u32;
        Ok(self.reset())
    }

    /// Port of C `inflatePrime` (inflate.c). Inserts up to 16 bits of `value`
    /// into the bit accumulator (LSB-first, matching the `PULLBYTE` fill
    /// order), or flushes the accumulator when `bits < 0`.
    ///
    /// * `bits == 0` — no-op.
    /// * `bits < 0`  — clear the accumulator (`hold = 0`, `bits = 0`).
    /// * `bits > 16` or would overflow 32 accumulated bits — `Err(StreamError)`.
    pub fn prime(&mut self, bits: i32, value: i32) -> Result<ReturnCode> {
        if bits < 0 {
            // C: `state->hold = 0; state->bits = 0; return Z_OK;`
            self.hold = 0;
            self.bits = 0;
            return Ok(ReturnCode::Ok);
        }
        if bits == 0 {
            return Ok(ReturnCode::Ok);
        }
        if bits > 16 || self.bits + bits as u32 > 32 {
            return Err(ZlibError::StreamError);
        }
        // C: `value &= (1L << bits) - 1;` then `hold += value << bits_in_hold`.
        let masked = (value as u32) & ((1u32 << bits) - 1);
        self.hold += (masked as u64) << self.bits;
        self.bits += bits as u32;
        Ok(ReturnCode::Ok)
    }

    /// Port of C `inflate_fixed` (inftrees.c). Installs the fixed Huffman
    /// literal/length and distance tables for a `BTYPE == 01` block.
    ///
    /// The C implementation points `lencode`/`distcode` at the module-static
    /// `lenfix`/`distfix` arrays. Our unified-arena model instead **copies**
    /// the precomputed [`LENFIX`] (512 entries) and [`DISTFIX`] (32 entries)
    /// tables into the front of `self.codes`, then records their offsets and
    /// index widths. Both fixed tables are single-level (no second-level
    /// sub-table links, and their `val` fields are base-relative), so the copy
    /// is bit-exact and lets the slow decode path read `lencode`/`distcode`
    /// uniformly as offsets into `self.codes`.
    ///
    /// Like C `inflate_fixed`, this deliberately does **not** advance
    /// `self.next`: a fixed-only stream reports `inflateCodesUsed() == 0`,
    /// matching C where the fixed tables live outside the `codes` arena.
    fn fixed_tables(&mut self) {
        // LENFIX occupies entries [0, 512); DISTFIX occupies [512, 544).
        self.codes[0..512].copy_from_slice(&LENFIX);
        self.codes[512..544].copy_from_slice(&DISTFIX);
        self.lencode = 0;
        self.lenbits = 9;
        self.distcode = 512;
        self.distbits = 5;
    }

    /// Port of C `updatewindow` (inflate.c L420-L470). Copies the most recently
    /// produced `copy` output bytes — the `copy` bytes ending at the current
    /// output position, i.e. the tail of `buf` — into the circular sliding
    /// window, lazily allocating the window on first use.
    ///
    /// # Errors
    ///
    /// Returns `Err(())` if the sliding window cannot be allocated, faithfully
    /// reproducing the C `updatewindow` allocation-failure path (`state->mode =
    /// MEM; return Z_MEM_ERROR`). The window is obtained through the
    /// caller-installed [`Allocator`] when one is present (so a memory-limiting
    /// or pooling custom allocator is genuinely consulted), and otherwise
    /// through the global allocator via the fallible [`Vec::try_reserve_exact`]
    /// — so a true out-of-memory condition surfaces as `Z_MEM_ERROR` rather
    /// than aborting the process. Each caller maps `Err(())` to
    /// [`InflateMode::Mem`] and [`ZlibError::MemError`].
    fn update_window(&mut self, buf: &[u8], copy: usize) -> core::result::Result<(), ()> {
        // Lazily allocate the window, sized exactly `1 << wbits`, on first use,
        // through the installed allocator (or the global allocator). A failed
        // allocation is reported to the caller, which raises `Z_MEM_ERROR`.
        if self.window.is_empty() {
            let wsize = 1usize << self.wbits;
            let window = match &self.allocator {
                Some(allocator) => allocator.allocate_bytes(wsize).ok_or(())?,
                None => {
                    let mut window = Vec::new();
                    window.try_reserve_exact(wsize).map_err(|_| ())?;
                    window.resize(wsize, 0);
                    window
                }
            };
            self.window = window;
        }
        // First call after (re)initialization: record window geometry.
        if self.wsize == 0 {
            self.wsize = 1usize << self.wbits;
            self.wnext = 0;
            self.whave = 0;
        }

        // `src` is the `copy` freshly produced bytes ending at the output
        // position (C's `end - copy .. end`).
        let src = &buf[buf.len() - copy..];
        let mut remaining = copy;

        if remaining >= self.wsize {
            // More new data than the whole window: keep only the last wsize.
            let start = remaining - self.wsize;
            self.window[..self.wsize].copy_from_slice(&src[start..start + self.wsize]);
            self.wnext = 0;
            self.whave = self.wsize;
        } else {
            // Copy up to the end of the window buffer first ...
            let mut dist = self.wsize - self.wnext;
            if dist > remaining {
                dist = remaining;
            }
            self.window[self.wnext..self.wnext + dist].copy_from_slice(&src[..dist]);
            remaining -= dist;
            if remaining != 0 {
                // ... then wrap the remainder to the front of the window.
                self.window[..remaining].copy_from_slice(&src[dist..dist + remaining]);
                self.wnext = remaining;
                self.whave = self.wsize;
            } else {
                self.wnext += dist;
                if self.wnext == self.wsize {
                    self.wnext = 0;
                }
                if self.whave < self.wsize {
                    self.whave += dist;
                    if self.whave > self.wsize {
                        self.whave = self.wsize;
                    }
                }
            }
        }

        Ok(())
    }
}

// ===========================================================================
// Checksum helper
// ===========================================================================

impl InflateState {
    /// Safe-Rust analogue of the C `UPDATE_CHECK` macro (inflate.c). Folds
    /// `buf` into the running protected check value, selecting CRC-32 for gzip
    /// streams (`flags != 0`) and Adler-32 for zlib streams. With the `gzip`
    /// feature disabled only the Adler-32 path exists.
    #[inline]
    fn update_check(&self, buf: &[u8]) -> u32 {
        #[cfg(feature = "gzip")]
        {
            if self.flags != 0 {
                crc32(self.check, buf)
            } else {
                adler32(self.check, buf)
            }
        }
        #[cfg(not(feature = "gzip"))]
        {
            adler32(self.check, buf)
        }
    }
}

// ===========================================================================
// The inflate() driver — `'inf: loop { match self.mode { … } }`
//
// This is the safe-Rust port of the C `inflate(strm, flush)` engine
// (inflate.c L600-L1280) together with its shared `inf_leave:` epilogue. The
// C `switch (state->mode)` becomes an exhaustive `match self.mode`, every C
// `break;` (which re-dispatches the enclosing `for (;;)`) becomes a natural
// fall-off / `continue 'inf`, and every C `goto inf_leave;` becomes
// `break 'inf` to the single shared epilogue placed after the loop.
//
// Bit-accumulator registers (`hold`, `bits`) and the input/output cursors
// (`next`/`have`, `put`/`left`) live in locals during the loop — mirroring the
// C `LOAD()`/`RESTORE()` "registers in locals" idiom — and are written back to
// `self` at the epilogue. Per-state progress that must survive an input-starved
// early exit (e.g. `have`, `length`, `offset`, `extra`, `ncode`/`nlen`/`ndist`)
// is kept in `self`, never in a local, so the next `inflate()` call resumes at
// exactly the same match arm with the same accumulated bits.
//
// NOTE ON THE FAST PATH: the C engine calls `inflate_fast` whenever
// `have >= 6 && left >= 258`. That routine is a pure throughput optimization
// that produces byte-identical output to the per-code "slow" path. The `Len`
// arm below wires it in under exactly those entry conditions (matching C
// `case LEN`), then falls back to the safe per-code decode when the buffers
// are too small. Output is bit-exact with C zlib either way.
// ===========================================================================

impl InflateState {
    /// Decompress from `input` into `output`, returning how many input bytes
    /// were consumed, how many output bytes were produced, and the status.
    ///
    /// This is the safe-Rust equivalent of C `inflate(strm, flush)`. Instead of
    /// threading a `z_stream` with raw `next_in`/`next_out` pointers, the caller
    /// passes the input and output as borrowed slices and reads the byte counts
    /// back from the returned [`InflateResult`]. The running totals, protected
    /// check value, error message and `data_type` bits are updated on `self`
    /// exactly as C updates the corresponding `z_stream` fields.
    ///
    /// `flush` accepts the same modes as C; [`Flush::Block`] and [`Flush::Trees`]
    /// request a return at block / tree boundaries, and [`Flush::Finish`]
    /// affects only the `Z_BUF_ERROR` determination — it never changes the
    /// produced bytes.
    pub fn inflate(&mut self, input: &[u8], output: &mut [u8], flush: Flush) -> InflateResult {
        // --- LOAD(): copy the streaming registers into locals. ---
        let mut next: usize = 0; // index of next unread input byte
        let mut have: usize = input.len(); // input bytes still available
        let mut put: usize = 0; // index of next output byte to write
        let mut left: usize = output.len(); // output space still available
        let mut hold: u64 = self.hold; // bit accumulator
        let mut bits: u32 = self.bits; // valid bits in `hold`

        let in0 = have; // avail_in at entry (for total_in / progress)
        let out0 = left; // avail_out at entry (for produced / progress)
        let mut out_mark = left; // C `out`: avail_out at last checksum marker
        let mut ret: Result<ReturnCode> = Ok(ReturnCode::Ok);

        // -------------------------------------------------------------------
        // Bit-accumulator macros (ports of inflate.c L320-L390). Defined as
        // function-local `macro_rules!` so they can both mutate the loop
        // locals and `break 'inf` straight to the shared epilogue when input
        // is exhausted mid-symbol.
        // -------------------------------------------------------------------

        // PULLBYTE(): pull one byte into the accumulator, or leave on underrun.
        //
        // Loop labels are hygienic inside `macro_rules!`, so the enclosing
        // loop's label cannot be named literally here; it is threaded in as a
        // `:lifetime` metavariable (`$lab`) and every call site passes `'inf`.
        macro_rules! pull_byte {
            ($lab:lifetime) => {{
                if have == 0 {
                    break $lab;
                }
                hold += (input[next] as u64) << bits;
                next += 1;
                have -= 1;
                bits += 8;
            }};
        }
        // NEEDBITS(n): ensure at least `n` bits are buffered.
        macro_rules! need_bits {
            ($lab:lifetime, $n:expr) => {{
                while bits < ($n as u32) {
                    pull_byte!($lab);
                }
            }};
        }
        // BITS(n): the low `n` bits of the accumulator.
        macro_rules! bits_val {
            ($n:expr) => {
                ((hold & ((1u64 << ($n as u32)) - 1)) as u32)
            };
        }
        // DROPBITS(n): remove `n` bits from the bottom of the accumulator.
        macro_rules! drop_bits {
            ($n:expr) => {{
                let n = $n as u32;
                hold >>= n;
                bits -= n;
            }};
        }
        // INITBITS(): clear the accumulator.
        macro_rules! init_bits {
            () => {{
                hold = 0;
                bits = 0;
            }};
        }
        // BYTEBITS(): discard bits up to the next byte boundary.
        macro_rules! byte_bits {
            () => {{
                let r = bits & 7;
                hold >>= r;
                bits -= r;
            }};
        }

        // C: `if (state->mode == TYPE) state->mode = TYPEDO;` — skip the
        // "new block" leave check on the very first dispatch of this call.
        if self.mode == InflateMode::Type {
            self.mode = InflateMode::TypeDo;
        }

        'inf: loop {
            match self.mode {
                // -----------------------------------------------------------
                // Header detection
                // -----------------------------------------------------------
                InflateMode::Head => {
                    if self.wrap == 0 {
                        self.mode = InflateMode::TypeDo;
                        continue 'inf;
                    }
                    need_bits!('inf, 16);
                    #[cfg(feature = "gzip")]
                    {
                        // gzip magic 0x8b1f (little-endian in the bitstream).
                        if (self.wrap & 2) != 0 && hold == 0x8b1f {
                            if self.wbits == 0 {
                                self.wbits = 15;
                            }
                            // C seeds the gzip header CRC-32 with the literal
                            // initial value 0 (`crc32(0L, Z_NULL, 0)`), then
                            // folds in the two magic bytes via the `CRC2` macro.
                            // Use the literal 0 directly per the CP2
                            // checksum-init rule (review finding #6).
                            let hb = (hold as u32).to_le_bytes();
                            self.check = crc32(0, &hb[..2]);
                            init_bits!();
                            self.mode = InflateMode::Flags;
                            continue 'inf;
                        }
                        // Not gzip: if a header was requested, mark it absent.
                        if let Some(head) = self.head.as_mut() {
                            head.done = false;
                        }
                    }
                    // zlib header check: must be allowed and FCHECK-valid.
                    if (self.wrap & 1) == 0
                        || ((bits_val!(8) << 8) + ((hold >> 8) as u32)) % 31 != 0
                    {
                        self.msg = Some("incorrect header check");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    if bits_val!(4) != Z_DEFLATED as u32 {
                        self.msg = Some("unknown compression method");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    drop_bits!(4);
                    let len = bits_val!(4) + 8;
                    if self.wbits == 0 {
                        self.wbits = len;
                    }
                    if len > 15 || len > self.wbits {
                        self.msg = Some("invalid window size");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    self.dmax = 1u32 << len;
                    self.flags = 0; // indicate zlib header
                    // Seed the running Adler-32 to 1. C computes this as
                    // `adler32(0L, Z_NULL, 0)`, whose NULL special-case returns
                    // 1 (the Adler-32 identity). Our `adler32(0, &[])` returns
                    // 0 for an empty slice, so we set the seed directly.
                    self.check = 1;
                    self.mode = if (hold & 0x200) != 0 {
                        InflateMode::DictId
                    } else {
                        InflateMode::Type
                    };
                    init_bits!();
                }

                // -----------------------------------------------------------
                // gzip header fields (only present with the `gzip` feature)
                // -----------------------------------------------------------
                #[cfg(feature = "gzip")]
                InflateMode::Flags => {
                    need_bits!('inf, 16);
                    self.flags = (hold & 0xffff) as i32;
                    if (self.flags & 0xff) != Z_DEFLATED {
                        self.msg = Some("unknown compression method");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    if (self.flags & 0xe000) != 0 {
                        self.msg = Some("unknown header flags set");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    let text = ((hold >> 8) & 1) != 0;
                    if let Some(head) = self.head.as_mut() {
                        head.text = text;
                    }
                    if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                        let hb = (hold as u32).to_le_bytes();
                        self.check = crc32(self.check, &hb[..2]);
                    }
                    init_bits!();
                    self.mode = InflateMode::Time;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Time => {
                    need_bits!('inf, 32);
                    let t = hold as u32;
                    if let Some(head) = self.head.as_mut() {
                        head.time = t;
                    }
                    if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                        let hb = (hold as u32).to_le_bytes();
                        self.check = crc32(self.check, &hb);
                    }
                    init_bits!();
                    self.mode = InflateMode::Os;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Os => {
                    need_bits!('inf, 16);
                    let xfl = (hold & 0xff) as i32;
                    let os = ((hold >> 8) & 0xff) as i32;
                    if let Some(head) = self.head.as_mut() {
                        head.xflags = xfl;
                        head.os = os;
                    }
                    if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                        let hb = (hold as u32).to_le_bytes();
                        self.check = crc32(self.check, &hb[..2]);
                    }
                    init_bits!();
                    self.mode = InflateMode::Exlen;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Exlen => {
                    if (self.flags & 0x0400) != 0 {
                        need_bits!('inf, 16);
                        self.length = (hold & 0xffff) as u32;
                        if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                            let hb = (hold as u32).to_le_bytes();
                            self.check = crc32(self.check, &hb[..2]);
                        }
                        init_bits!();
                    } else if let Some(head) = self.head.as_mut() {
                        head.extra = None;
                    }
                    self.mode = InflateMode::Extra;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Extra => {
                    if (self.flags & 0x0400) != 0 {
                        let mut copy = self.length as usize;
                        if copy > have {
                            copy = have;
                        }
                        if copy != 0 {
                            if let Some(head) = self.head.as_mut() {
                                if let Some(extra) = head.extra.as_mut() {
                                    extra.extend_from_slice(&input[next..next + copy]);
                                }
                            }
                            if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                                self.check = crc32(self.check, &input[next..next + copy]);
                            }
                            have -= copy;
                            next += copy;
                            self.length -= copy as u32;
                        }
                        if self.length != 0 {
                            break 'inf;
                        }
                    }
                    self.length = 0;
                    self.mode = InflateMode::Name;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Name => {
                    if (self.flags & 0x0800) != 0 {
                        if have == 0 {
                            break 'inf;
                        }
                        let mut copy = 0usize;
                        let mut byte;
                        loop {
                            byte = input[next + copy];
                            copy += 1;
                            if byte != 0 {
                                if let Some(head) = self.head.as_mut() {
                                    if let Some(name) = head.name.as_mut() {
                                        name.push(byte);
                                    }
                                }
                            }
                            if byte == 0 || copy >= have {
                                break;
                            }
                        }
                        if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                            self.check = crc32(self.check, &input[next..next + copy]);
                        }
                        have -= copy;
                        next += copy;
                        if byte != 0 {
                            break 'inf;
                        }
                    } else if let Some(head) = self.head.as_mut() {
                        head.name = None;
                    }
                    self.length = 0;
                    self.mode = InflateMode::Comment;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Comment => {
                    if (self.flags & 0x1000) != 0 {
                        if have == 0 {
                            break 'inf;
                        }
                        let mut copy = 0usize;
                        let mut byte;
                        loop {
                            byte = input[next + copy];
                            copy += 1;
                            if byte != 0 {
                                if let Some(head) = self.head.as_mut() {
                                    if let Some(comment) = head.comment.as_mut() {
                                        comment.push(byte);
                                    }
                                }
                            }
                            if byte == 0 || copy >= have {
                                break;
                            }
                        }
                        if (self.flags & 0x0200) != 0 && (self.wrap & 4) != 0 {
                            self.check = crc32(self.check, &input[next..next + copy]);
                        }
                        have -= copy;
                        next += copy;
                        if byte != 0 {
                            break 'inf;
                        }
                    } else if let Some(head) = self.head.as_mut() {
                        head.comment = None;
                    }
                    self.mode = InflateMode::Hcrc;
                }
                #[cfg(feature = "gzip")]
                InflateMode::Hcrc => {
                    let fl = self.flags;
                    if (fl & 0x0200) != 0 {
                        need_bits!('inf, 16);
                        if (self.wrap & 4) != 0 && (hold as u32 & 0xffff) != (self.check & 0xffff) {
                            self.msg = Some("header crc mismatch");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                        init_bits!();
                    }
                    if let Some(head) = self.head.as_mut() {
                        head.hcrc = ((fl >> 9) & 1) != 0;
                        head.done = true;
                    }
                    // gzip running check restarts for the data CRC-32, seeded
                    // with the literal CRC-32 initial value 0 per the CP2
                    // checksum-init rule (`crc32(0L, Z_NULL, 0)`, finding #6).
                    self.check = 0;
                    self.mode = InflateMode::Type;
                }
                // When the `gzip` feature is disabled the eight gzip-header
                // states are unreachable (HEAD never transitions into them);
                // keep the match total by routing them to Bad defensively.
                #[cfg(not(feature = "gzip"))]
                InflateMode::Flags
                | InflateMode::Time
                | InflateMode::Os
                | InflateMode::Exlen
                | InflateMode::Extra
                | InflateMode::Name
                | InflateMode::Comment
                | InflateMode::Hcrc => {
                    self.msg = Some("gzip support disabled");
                    self.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // -----------------------------------------------------------
                // zlib preset-dictionary identification
                // -----------------------------------------------------------
                InflateMode::DictId => {
                    need_bits!('inf, 32);
                    // ZSWAP32(hold): the dictionary id is stored big-endian.
                    self.check = (hold as u32).swap_bytes();
                    init_bits!();
                    self.mode = InflateMode::Dict;
                }
                InflateMode::Dict => {
                    if !self.havedict {
                        // Save registers and ask the caller for a dictionary.
                        // Mirrors C `RESTORE(); return Z_NEED_DICT;` — the
                        // totals/window epilogue is intentionally skipped.
                        self.hold = hold;
                        self.bits = bits;
                        return InflateResult {
                            consumed: in0 - have,
                            produced: out0 - left,
                            status: Ok(ReturnCode::NeedDict),
                        };
                    }
                    // Re-seed the running Adler-32 to 1 for the data that
                    // follows the dictionary (C `adler32(0L, Z_NULL, 0)` == 1;
                    // see the HEAD branch for why we assign 1 directly).
                    self.check = 1;
                    self.mode = InflateMode::Type;
                }

                // -----------------------------------------------------------
                // Block type dispatch
                // -----------------------------------------------------------
                InflateMode::Type => {
                    if flush == Flush::Block || flush == Flush::Trees {
                        break 'inf;
                    }
                    self.mode = InflateMode::TypeDo;
                }
                InflateMode::TypeDo => {
                    if self.last {
                        byte_bits!();
                        self.mode = InflateMode::Check;
                        continue 'inf;
                    }
                    need_bits!('inf, 3);
                    self.last = bits_val!(1) != 0;
                    drop_bits!(1);
                    match bits_val!(2) {
                        0 => {
                            // stored block
                            self.mode = InflateMode::Stored;
                        }
                        1 => {
                            // fixed Huffman tables
                            self.fixed_tables();
                            self.mode = InflateMode::LenUnder;
                            if flush == Flush::Trees {
                                drop_bits!(2);
                                break 'inf;
                            }
                        }
                        2 => {
                            // dynamic Huffman tables
                            self.mode = InflateMode::Table;
                        }
                        _ => {
                            self.msg = Some("invalid block type");
                            self.mode = InflateMode::Bad;
                        }
                    }
                    drop_bits!(2);
                }

                // -----------------------------------------------------------
                // Stored (uncompressed) block
                // -----------------------------------------------------------
                InflateMode::Stored => {
                    byte_bits!(); // align to byte boundary
                    need_bits!('inf, 32);
                    if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                        self.msg = Some("invalid stored block lengths");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    self.length = (hold & 0xffff) as u32;
                    init_bits!();
                    self.mode = InflateMode::CopyUnder;
                    if flush == Flush::Trees {
                        break 'inf;
                    }
                }
                InflateMode::CopyUnder => {
                    self.mode = InflateMode::Copy;
                }
                InflateMode::Copy => {
                    let mut copy = self.length as usize;
                    if copy != 0 {
                        if copy > have {
                            copy = have;
                        }
                        if copy > left {
                            copy = left;
                        }
                        if copy == 0 {
                            break 'inf;
                        }
                        output[put..put + copy].copy_from_slice(&input[next..next + copy]);
                        have -= copy;
                        next += copy;
                        left -= copy;
                        put += copy;
                        self.length -= copy as u32;
                        continue 'inf;
                    }
                    self.mode = InflateMode::Type;
                }

                // -----------------------------------------------------------
                // Dynamic Huffman table construction
                // -----------------------------------------------------------
                InflateMode::Table => {
                    need_bits!('inf, 14);
                    self.nlen = (bits_val!(5) + 257) as usize;
                    drop_bits!(5);
                    self.ndist = (bits_val!(5) + 1) as usize;
                    drop_bits!(5);
                    self.ncode = (bits_val!(4) + 4) as usize;
                    drop_bits!(4);
                    if self.nlen > 286 || self.ndist > 30 {
                        self.msg = Some("too many length or distance symbols");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    self.have = 0;
                    self.mode = InflateMode::LenLens;
                }
                InflateMode::LenLens => {
                    while self.have < self.ncode {
                        need_bits!('inf, 3);
                        self.lens[ORDER[self.have] as usize] = bits_val!(3) as u16;
                        self.have += 1;
                        drop_bits!(3);
                    }
                    while self.have < 19 {
                        self.lens[ORDER[self.have] as usize] = 0;
                        self.have += 1;
                    }
                    self.next = 0;
                    self.lencode = 0;
                    self.distcode = 0;
                    self.lenbits = 7;
                    let res = inflate_table(
                        CodeType::Codes,
                        &self.lens,
                        19,
                        &mut self.codes,
                        &mut self.lenbits,
                        &mut self.work,
                    );
                    match res {
                        Ok(used) => {
                            self.next += used;
                        }
                        Err(_) => {
                            self.msg = Some("invalid code lengths set");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                    }
                    self.have = 0;
                    self.mode = InflateMode::CodeLens;
                }
                InflateMode::CodeLens => {
                    while self.have < self.nlen + self.ndist {
                        let here = loop {
                            let h = self.codes[self.lencode + bits_val!(self.lenbits) as usize];
                            if (h.bits as u32) <= bits {
                                break h;
                            }
                            pull_byte!('inf);
                        };
                        if here.val < 16 {
                            drop_bits!(here.bits as u32);
                            self.lens[self.have] = here.val;
                            self.have += 1;
                        } else {
                            let len_fill: u16;
                            let copy_count: usize;
                            if here.val == 16 {
                                need_bits!('inf, here.bits as u32 + 2);
                                drop_bits!(here.bits as u32);
                                if self.have == 0 {
                                    self.msg = Some("invalid bit length repeat");
                                    self.mode = InflateMode::Bad;
                                    break;
                                }
                                len_fill = self.lens[self.have - 1];
                                copy_count = 3 + bits_val!(2) as usize;
                                drop_bits!(2);
                            } else if here.val == 17 {
                                need_bits!('inf, here.bits as u32 + 3);
                                drop_bits!(here.bits as u32);
                                len_fill = 0;
                                copy_count = 3 + bits_val!(3) as usize;
                                drop_bits!(3);
                            } else {
                                need_bits!('inf, here.bits as u32 + 7);
                                drop_bits!(here.bits as u32);
                                len_fill = 0;
                                copy_count = 11 + bits_val!(7) as usize;
                                drop_bits!(7);
                            }
                            if self.have + copy_count > self.nlen + self.ndist {
                                self.msg = Some("invalid bit length repeat");
                                self.mode = InflateMode::Bad;
                                break;
                            }
                            let mut c = copy_count;
                            while c != 0 {
                                self.lens[self.have] = len_fill;
                                self.have += 1;
                                c -= 1;
                            }
                        }
                    }

                    // Propagate a BAD set inside the decode loop.
                    if self.mode == InflateMode::Bad {
                        continue 'inf;
                    }

                    // An end-of-block code (symbol 256) must be present.
                    if self.lens[256] == 0 {
                        self.msg = Some("invalid code -- missing end-of-block");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }

                    // Build the literal/length decode table.
                    self.next = 0;
                    self.lencode = 0;
                    self.lenbits = 9;
                    let nlen = self.nlen;
                    let res = inflate_table(
                        CodeType::Lens,
                        &self.lens,
                        nlen,
                        &mut self.codes,
                        &mut self.lenbits,
                        &mut self.work,
                    );
                    match res {
                        Ok(used) => {
                            self.next += used;
                        }
                        Err(_) => {
                            self.msg = Some("invalid literal/lengths set");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                    }

                    // Build the distance decode table after the length table.
                    self.distcode = self.next;
                    self.distbits = 6;
                    let dc = self.distcode;
                    let ndist = self.ndist;
                    let res = inflate_table(
                        CodeType::Dists,
                        &self.lens[nlen..],
                        ndist,
                        &mut self.codes[dc..],
                        &mut self.distbits,
                        &mut self.work,
                    );
                    match res {
                        Ok(used) => {
                            self.next += used;
                        }
                        Err(_) => {
                            self.msg = Some("invalid distances set");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                    }

                    self.mode = InflateMode::LenUnder;
                    if flush == Flush::Trees {
                        break 'inf;
                    }
                }

                // -----------------------------------------------------------
                // Length / literal / distance decode (per-code slow path)
                // -----------------------------------------------------------
                InflateMode::LenUnder => {
                    self.mode = InflateMode::Len;
                }
                InflateMode::Len => {
                    // Fast path (inflate.c `case LEN`): when at least 6 input
                    // bytes and 258 output bytes are buffered, hand control to
                    // the `inflate_fast` inner loop. It decodes whole
                    // literal/length/distance runs with minimal per-symbol
                    // overhead and produces byte-identical output to the
                    // per-code slow path below (review finding #1; this is the
                    // required hot-path architecture of AAP §0.6.4 / Rule 13).
                    if have >= 6 && left >= 258 {
                        // Sync the local bit accumulator into the state, since
                        // `inflate_fast` works on `self.hold`/`self.bits`.
                        self.hold = hold;
                        self.bits = bits;
                        // `start == out0` (avail_out at this call's entry) makes
                        // `inflate_fast`'s `beg == 0` — the output index where
                        // this call began writing (`put` starts at 0) — so a
                        // distance reaching no farther than the bytes produced
                        // this call is copied from `output`, and a deeper one
                        // from the (pre-call) sliding window. This mirrors C
                        // `inflate_fast(strm, out)`.
                        inflate_fast(self, input, &mut next, output, &mut put, out0);
                        // Reload the locals: `inflate_fast` advanced `next`/`put`
                        // (through the `&mut` cursors) and wrote back
                        // `self.hold`/`self.bits`. Recompute `have`/`left` from
                        // the updated cursors.
                        hold = self.hold;
                        bits = self.bits;
                        have = input.len() - next;
                        left = output.len() - put;
                        // C: a clean end-of-block leaves mode TYPE; record
                        // `back = -1` (not mid-code). When the fast path instead
                        // ran out of input/output, mode is still Len and the
                        // re-dispatch falls through to the slow path (now with
                        // `have < 6` or `left < 258`); on a bad code mode is Bad.
                        if self.mode == InflateMode::Type {
                            self.back = -1;
                        }
                        continue 'inf;
                    }
                    self.back = 0;
                    let mut here = loop {
                        let h = self.codes[self.lencode + bits_val!(self.lenbits) as usize];
                        if (h.bits as u32) <= bits {
                            break h;
                        }
                        pull_byte!('inf);
                    };
                    if here.op != 0 && (here.op & 0xf0) == 0 {
                        let last = here;
                        here = loop {
                            let idx = self.lencode
                                + last.val as usize
                                + (bits_val!(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize;
                            let h = self.codes[idx];
                            if (last.bits as u32 + h.bits as u32) <= bits {
                                break h;
                            }
                            pull_byte!('inf);
                        };
                        drop_bits!(last.bits as u32);
                        self.back += last.bits as i32;
                    }
                    drop_bits!(here.bits as u32);
                    self.back += here.bits as i32;
                    self.length = here.val as u32;
                    if here.op == 0 {
                        self.mode = InflateMode::Lit;
                        continue 'inf;
                    }
                    if (here.op & 32) != 0 {
                        // end of block
                        self.back = -1;
                        self.mode = InflateMode::Type;
                        continue 'inf;
                    }
                    if (here.op & 64) != 0 {
                        self.msg = Some("invalid literal/length code");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    self.extra = (here.op & 15) as u32;
                    self.mode = InflateMode::LenExt;
                }
                InflateMode::LenExt => {
                    if self.extra != 0 {
                        need_bits!('inf, self.extra);
                        self.length += bits_val!(self.extra);
                        drop_bits!(self.extra);
                        self.back += self.extra as i32;
                    }
                    self.was = self.length;
                    self.mode = InflateMode::Dist;
                }
                InflateMode::Dist => {
                    let mut here = loop {
                        let h = self.codes[self.distcode + bits_val!(self.distbits) as usize];
                        if (h.bits as u32) <= bits {
                            break h;
                        }
                        pull_byte!('inf);
                    };
                    if (here.op & 0xf0) == 0 {
                        let last = here;
                        here = loop {
                            let idx = self.distcode
                                + last.val as usize
                                + (bits_val!(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize;
                            let h = self.codes[idx];
                            if (last.bits as u32 + h.bits as u32) <= bits {
                                break h;
                            }
                            pull_byte!('inf);
                        };
                        drop_bits!(last.bits as u32);
                        self.back += last.bits as i32;
                    }
                    drop_bits!(here.bits as u32);
                    self.back += here.bits as i32;
                    if (here.op & 64) != 0 {
                        self.msg = Some("invalid distance code");
                        self.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    self.offset = here.val as u32;
                    self.extra = (here.op & 15) as u32;
                    self.mode = InflateMode::DistExt;
                }
                InflateMode::DistExt => {
                    if self.extra != 0 {
                        need_bits!('inf, self.extra);
                        self.offset += bits_val!(self.extra);
                        drop_bits!(self.extra);
                        self.back += self.extra as i32;
                    }
                    // INFLATE_STRICT dmax check is compiled out of stock zlib by
                    // default, so it is intentionally omitted here. The
                    // "distance too far back" guard against `whave` in MATCH is
                    // always active and preserved.
                    self.mode = InflateMode::Match;
                }
                InflateMode::Match => {
                    if left == 0 {
                        break 'inf;
                    }
                    let offset = self.offset as usize;
                    let mut copy: usize;
                    let mut from: usize;
                    let from_window;
                    if offset > put {
                        // Reach back into the sliding window (history before the
                        // current output buffer).
                        copy = offset - put;
                        // When `copy > whave` the distance reaches before the
                        // start of available history. With `sane == true` (the
                        // default) this is an error.
                        // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR is off by
                        // default; with `sane == false` we would zero-fill, but
                        // the default build never reaches that path.
                        if copy > self.whave && self.sane {
                            self.msg = Some("invalid distance too far back");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                        from_window = true;
                        if copy > self.wnext {
                            copy -= self.wnext;
                            from = self.wsize - copy;
                        } else {
                            from = self.wnext - copy;
                        }
                        if copy > self.length as usize {
                            copy = self.length as usize;
                        }
                    } else {
                        // Reach back within the current output buffer.
                        from = put - offset;
                        copy = self.length as usize;
                        from_window = false;
                    }
                    if copy > left {
                        copy = left;
                    }
                    left -= copy;
                    self.length -= copy as u32;
                    if from_window {
                        for _ in 0..copy {
                            let b = self.window[from];
                            output[put] = b;
                            put += 1;
                            from += 1;
                        }
                    } else {
                        for _ in 0..copy {
                            let b = output[from];
                            output[put] = b;
                            put += 1;
                            from += 1;
                        }
                    }
                    if self.length == 0 {
                        self.mode = InflateMode::Len;
                    }
                }
                InflateMode::Lit => {
                    if left == 0 {
                        break 'inf;
                    }
                    output[put] = self.length as u8;
                    put += 1;
                    left -= 1;
                    self.mode = InflateMode::Len;
                }

                // -----------------------------------------------------------
                // Trailer verification
                // -----------------------------------------------------------
                InflateMode::Check => {
                    if self.wrap != 0 {
                        need_bits!('inf, 32);
                        let produced = out_mark - left;
                        // C unsigned-counter parity: `total_out`/`total` are
                        // unsigned and wrap on overflow; `wrapping_add` matches
                        // that and avoids a debug overflow panic (finding #13).
                        self.total_out = self.total_out.wrapping_add(produced as u64);
                        self.total = self.total.wrapping_add(produced as u64);
                        if (self.wrap & 4) != 0 && produced != 0 {
                            let c = self.update_check(&output[put - produced..put]);
                            self.check = c;
                        }
                        out_mark = left;
                        if (self.wrap & 4) != 0 {
                            // gzip: CRC-32 stored little-endian -> compare hold
                            // directly. zlib: Adler-32 stored big-endian ->
                            // compare the byte-swapped hold.
                            let computed: u32;
                            #[cfg(feature = "gzip")]
                            {
                                computed = if self.flags != 0 {
                                    hold as u32
                                } else {
                                    (hold as u32).swap_bytes()
                                };
                            }
                            #[cfg(not(feature = "gzip"))]
                            {
                                computed = (hold as u32).swap_bytes();
                            }
                            if computed != self.check {
                                self.msg = Some("incorrect data check");
                                self.mode = InflateMode::Bad;
                                continue 'inf;
                            }
                        }
                        init_bits!();
                    }
                    self.mode = InflateMode::Length;
                }
                InflateMode::Length => {
                    if self.wrap != 0 && self.flags != 0 {
                        need_bits!('inf, 32);
                        if (self.wrap & 4) != 0 && (hold as u32) != (self.total as u32) {
                            self.msg = Some("incorrect length check");
                            self.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                        init_bits!();
                    }
                    self.mode = InflateMode::Done;
                }
                InflateMode::Done => {
                    ret = Ok(ReturnCode::StreamEnd);
                    break 'inf;
                }
                InflateMode::Bad => {
                    ret = Err(ZlibError::DataError);
                    break 'inf;
                }
                InflateMode::Mem => {
                    // Sticky out-of-memory state. C `return Z_MEM_ERROR;`
                    // without restoring registers — report no progress.
                    return InflateResult {
                        consumed: 0,
                        produced: 0,
                        status: Err(ZlibError::MemError),
                    };
                }
                InflateMode::Sync => {
                    // Sticky stream-error / sync-search state handled outside
                    // the driver. C `return Z_STREAM_ERROR;`.
                    return InflateResult {
                        consumed: 0,
                        produced: 0,
                        status: Err(ZlibError::StreamError),
                    };
                }
            }
        }

        // -------------------------------------------------------------------
        // inf_leave: shared epilogue (port of inflate.c `inf_leave:`).
        // -------------------------------------------------------------------

        // RESTORE(): write the bit registers back to the state.
        self.hold = hold;
        self.bits = bits;

        // Update the sliding window with the output produced since the marker.
        // The guard is C `updatewindow`'s call condition (a window already
        // exists, or there is fresh output to capture in a non-terminal mode);
        // the `&&` short-circuits so `update_window` is invoked only when that
        // guard holds, exactly as in C. C `updatewindow` can fail to allocate
        // the window, in which case it sets `state->mode = MEM` and returns
        // `Z_MEM_ERROR`; we reproduce that by transitioning to `Mem` and
        // overriding the return code with `Z_MEM_ERROR` (the `BUF_ERROR` check
        // below only fires when `ret == Ok`, so it never masks this).
        let copy = out_mark - left;
        if (self.wsize != 0
            || (copy != 0
                && self.mode < InflateMode::Bad
                && (self.mode < InflateMode::Check || flush != Flush::Finish)))
            && self.update_window(&output[..put], copy).is_err()
        {
            self.mode = InflateMode::Mem;
            ret = Err(ZlibError::MemError);
        }

        // Account consumed input and produced output (matching C exactly).
        let consumed = in0 - have;
        // C unsigned-counter parity: total_in/total_out/total wrap on overflow.
        self.total_in = self.total_in.wrapping_add(consumed as u64);
        self.total_out = self.total_out.wrapping_add(copy as u64);
        self.total = self.total.wrapping_add(copy as u64);
        if (self.wrap & 4) != 0 && copy != 0 {
            let c = self.update_check(&output[put - copy..put]);
            self.check = c;
        }

        // data_type: low 7 bits are the unused-bit count; the high bits encode
        // last-block / at-block-boundary / at-stored-or-len-table status.
        self.data_type = bits as i32
            + if self.last { 64 } else { 0 }
            + if self.mode == InflateMode::Type {
                128
            } else {
                0
            }
            + if self.mode == InflateMode::LenUnder || self.mode == InflateMode::CopyUnder {
                256
            } else {
                0
            };

        // Z_BUF_ERROR if no forward progress was possible (or on a fruitless
        // Z_FINISH), but never override a real status.
        if ((consumed == 0 && copy == 0) || flush == Flush::Finish) && ret == Ok(ReturnCode::Ok) {
            ret = Err(ZlibError::BufError);
        }

        InflateResult {
            consumed,
            produced: out0 - left,
            status: ret,
        }
    }
}

// ===========================================================================
// syncsearch — free helper used by `InflateState::sync`
// ===========================================================================

/// Port of the C file-static `syncsearch` (inflate.c L1244-L1262).
///
/// Searches `buf` for the 4-byte flush marker `00 00 FF FF`. `have` carries the
/// number of marker bytes matched so far (0..=3 on entry); on return it holds
/// the updated count (4 means the full marker was found). The return value is
/// the number of bytes scanned: the index just past the final marker byte when
/// found, otherwise `buf.len()`.
fn syncsearch(have: &mut usize, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next = 0usize;
    while next < buf.len() && got < 4 {
        let want = if got < 2 { 0 } else { 0xff };
        if buf[next] as i32 == want {
            got += 1;
        } else if buf[next] != 0 {
            got = 0;
        } else {
            got = 4 - got;
        }
        next += 1;
    }
    *have = got;
    next
}

// ===========================================================================
// Auxiliary inflate functions (ported from inflate.c)
// ===========================================================================

impl InflateState {
    /// Current decode mode — the safe analogue of inspecting `state->mode`.
    ///
    /// The C `inflateStateCheck` null/owner/range validation is unnecessary in
    /// this model: ownership via `Box<InflateState>` guarantees a non-null,
    /// well-typed state, and the `InflateMode` enum can only ever hold a valid
    /// variant. This accessor is provided for the FFI layer's diagnostics.
    #[inline]
    pub fn mode(&self) -> InflateMode {
        self.mode
    }

    /// Port of C `inflateGetDictionary` (inflate.c L1167-L1185).
    ///
    /// Copies the sliding-window contents — the most recent `whave` bytes, in
    /// chronological (oldest-first) order — into `dictionary` when one is
    /// supplied, and returns the number of bytes available (`whave`). Pass
    /// `None` to query the length without copying.
    pub fn get_dictionary(&self, dictionary: Option<&mut [u8]>) -> usize {
        if let Some(dict) = dictionary {
            if self.whave != 0 {
                // Older bytes first: window[wnext .. wnext + (whave - wnext)].
                let first = self.whave - self.wnext;
                dict[..first].copy_from_slice(&self.window[self.wnext..self.wnext + first]);
                // Then the wrapped-around newer bytes: window[0 .. wnext].
                dict[first..first + self.wnext].copy_from_slice(&self.window[..self.wnext]);
            }
        }
        self.whave
    }

    /// Port of C `inflateSetDictionary` (inflate.c L1187-L1217).
    ///
    /// Installs `dictionary` as the sliding-window history. For a zlib stream
    /// this is only legal in [`InflateMode::Dict`] (i.e. after a `Z_NEED_DICT`
    /// return) and the dictionary's Adler-32 must match the stored
    /// identifier; for raw streams it may be set before decoding begins.
    pub fn set_dictionary(&mut self, dictionary: &[u8]) -> Result<ReturnCode> {
        // A dictionary is only meaningful at DICT for wrapped streams.
        if self.wrap != 0 && self.mode != InflateMode::Dict {
            return Err(ZlibError::StreamError);
        }
        // Verify the dictionary identifier against the stored check value.
        if self.mode == InflateMode::Dict {
            // C `inflateSetDictionary` computes the identifier as
            // `adler32(adler32(0L, Z_NULL, 0), dictionary, dictLength)`, and
            // `adler32(0L, Z_NULL, 0)` returns the Adler-32 *initial* value `1`
            // (not 0). In this core `adler32(0, &[])` returns 0, so seeding the
            // running check with the literal `1` is required to match the value
            // the deflate side stored — otherwise valid preset-dictionary
            // streams are wrongly rejected (review finding #3).
            let dictid = adler32(1, dictionary);
            if dictid != self.check {
                return Err(ZlibError::DataError);
            }
        }
        // Load the dictionary into the window (lazily allocating it). The last
        // `dictionary.len()` bytes ending at the dictionary's end are copied,
        // i.e. the entire dictionary. A window allocation failure here is the C
        // `inflateSetDictionary` -> `updatewindow` -> `Z_MEM_ERROR` path: set
        // `mode = MEM` and return `Z_MEM_ERROR`.
        if self.update_window(dictionary, dictionary.len()).is_err() {
            self.mode = InflateMode::Mem;
            return Err(ZlibError::MemError);
        }
        self.havedict = true;
        Ok(ReturnCode::Ok)
    }

    /// Port of C `inflateGetHeader` (inflate.c L1219-L1231), gzip-gated.
    ///
    /// Requests capture of the gzip header into `head` (which may be
    /// pre-configured with `Some(Vec)` buffers for `name`/`comment`/`extra` to
    /// opt into capturing those fields). Only valid when a gzip header is
    /// expected (`wrap & 2`). The populated header is retrieved afterwards via
    /// [`InflateState::header`] or [`InflateState::take_header`].
    #[cfg(feature = "gzip")]
    pub fn get_header(&mut self, head: GzHeader) -> Result<ReturnCode> {
        if (self.wrap & 2) == 0 {
            return Err(ZlibError::StreamError);
        }
        let mut head = head;
        head.done = false;
        self.head = Some(head);
        Ok(ReturnCode::Ok)
    }

    /// Borrow the captured gzip header, if header capture was requested.
    #[cfg(feature = "gzip")]
    #[inline]
    pub fn header(&self) -> Option<&GzHeader> {
        self.head.as_ref()
    }

    /// Take ownership of the captured gzip header, clearing it from the state.
    #[cfg(feature = "gzip")]
    #[inline]
    pub fn take_header(&mut self) -> Option<GzHeader> {
        self.head.take()
    }

    /// Port of C `inflateSync` (inflate.c L1264-L1310).
    ///
    /// Skips invalid compressed data, searching the buffered bits and then
    /// `input` for a full-flush marker (`00 00 FF FF`). On success the stream is
    /// reset to [`InflateMode::Type`] so decoding can resume at the next block,
    /// preserving the running totals and the gzip/zlib `flags`. Returns the
    /// number of input bytes consumed and the status:
    /// `Ok(Ok)` on a successful resync, `Err(DataError)` if no marker was found,
    /// or `Err(BufError)` when there is nothing to search.
    pub fn sync(&mut self, input: &[u8]) -> (usize, Result<ReturnCode>) {
        // Nothing to look at: no input and fewer than 8 buffered bits.
        if input.is_empty() && self.bits < 8 {
            return (0, Err(ZlibError::BufError));
        }

        // First time through: turn the residual bit buffer into bytes and
        // search those, byte-aligning the accumulator first.
        if self.mode != InflateMode::Sync {
            self.mode = InflateMode::Sync;
            let r = self.bits & 7;
            self.hold >>= r;
            self.bits -= r;
            let mut buf = [0u8; 4];
            let mut len = 0usize;
            while self.bits >= 8 && len < buf.len() {
                buf[len] = self.hold as u8;
                len += 1;
                self.hold >>= 8;
                self.bits -= 8;
            }
            self.have = 0;
            syncsearch(&mut self.have, &buf[..len]);
        }

        // Search the available input for the remainder of the marker.
        let len = syncsearch(&mut self.have, input);
        // C unsigned-counter parity: `total_in` wraps on overflow.
        self.total_in = self.total_in.wrapping_add(len as u64);

        // No marker found yet.
        if self.have != 4 {
            return (len, Err(ZlibError::DataError));
        }

        // Marker found: set up to restart inflate() on a fresh block.
        if self.flags == -1 {
            // No header seen yet -> treat the remainder as raw deflate.
            self.wrap = 0;
        } else {
            // A header was seen -> stop validating the (now broken) check.
            self.wrap &= !4;
        }
        let flags = self.flags;
        let in_total = self.total_in;
        let out_total = self.total_out;
        self.reset();
        self.total_in = in_total;
        self.total_out = out_total;
        self.flags = flags;
        self.mode = InflateMode::Type;
        (len, Ok(ReturnCode::Ok))
    }

    /// Port of C `inflateSyncPoint` (inflate.c L1320-L1326).
    ///
    /// Returns `true` when inflate is positioned exactly at the end of a block
    /// emitted by `Z_SYNC_FLUSH`/`Z_FULL_FLUSH` — i.e. awaiting the length bytes
    /// of an empty stored block with an empty bit accumulator.
    #[inline]
    pub fn sync_point(&self) -> bool {
        self.mode == InflateMode::Stored && self.bits == 0
    }

    /// Port of C `inflateCopy` (inflate.c L1328-L1368).
    ///
    /// Produces an independent deep copy of the decompression state, including
    /// the sliding window. Because `lencode`/`distcode`/`next` are stored as
    /// offsets into the owned `codes` array (not raw pointers), the derived
    /// `Clone` keeps them valid in the copy with no fix-up — subsuming the C
    /// pointer-rebasing logic.
    #[inline]
    pub fn copy(&self) -> Box<InflateState> {
        Box::new(self.clone())
    }

    /// Port of C `inflateUndermine` (inflate.c L1370-L1383).
    ///
    /// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` is not enabled in this
    /// build, so — exactly like stock zlib — this forces `sane = true` and
    /// reports `Z_DATA_ERROR` to signal that subversion is unsupported.
    pub fn undermine(&mut self, _subvert: bool) -> Result<ReturnCode> {
        self.sane = true;
        Err(ZlibError::DataError)
    }

    /// Port of C `inflateValidate` (inflate.c L1385-L1395).
    ///
    /// Enables (`check == true`) or disables checksum validation by toggling
    /// `wrap` bit 2, but only when the stream actually has a wrapper.
    pub fn validate(&mut self, check: bool) -> Result<ReturnCode> {
        if check && self.wrap != 0 {
            self.wrap |= 4;
        } else {
            self.wrap &= !4;
        }
        Ok(ReturnCode::Ok)
    }

    /// Port of C `inflateMark` (inflate.c L1397-L1406).
    ///
    /// Returns a packed diagnostic used by random-access tools: the number of
    /// bits back of the current bit position in the high part, and the number
    /// of bytes already copied from the in-progress match/stored run in the low
    /// part. A fresh state (`back == -1`) yields `-(1 << 16)`.
    pub fn mark(&self) -> i64 {
        let extra: i64 = match self.mode {
            InflateMode::Copy => self.length as i64,
            InflateMode::Match => (self.was as i64) - (self.length as i64),
            _ => 0,
        };
        ((self.back as i64) << 16) + extra
    }

    /// Port of C `inflateCodesUsed` (inflate.c L1408-L1413).
    ///
    /// Returns the number of decode-table [`Code`] entries used so far — the
    /// `next` offset into the `codes` arena.
    #[inline]
    pub fn codes_used(&self) -> usize {
        self.next
    }
}

// ===========================================================================
// Smoke tests
//
// These exercise the engine end-to-end against reference streams produced by
// the canonical C zlib (embedded as byte literals so the tests require no dev
// dependency). They cover zlib, raw-deflate and gzip framing, single-shot and
// chunked I/O (proving the mid-symbol save/resume path), the `windowBits`
// overloading of `reset2`, and — by virtue of the trailer checks passing — the
// Adler-32 (big-endian) vs CRC-32 (little-endian) trailer-endianness handling.
// Full conformance lives in `zlib-rs/tests/`.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const MSG: &[u8] = b"hello, world! hello, world! the quick brown fox jumps over the lazy dog.";

    // zlib container (RFC 1950), level 6.
    const ZLIB_STREAM: [u8; 65] = [
        120, 156, 203, 72, 205, 201, 201, 215, 81, 40, 207, 47, 202, 73, 81, 84, 200, 64, 225, 149,
        100, 164, 42, 20, 150, 102, 38, 103, 43, 36, 21, 229, 151, 231, 41, 164, 229, 87, 40, 100,
        149, 230, 22, 20, 43, 228, 151, 165, 22, 129, 165, 115, 18, 171, 42, 21, 82, 242, 211, 245,
        0, 167, 40, 25, 186,
    ];

    // Raw DEFLATE (RFC 1951), windowBits = -15.
    const RAW_STREAM: [u8; 59] = [
        203, 72, 205, 201, 201, 215, 81, 40, 207, 47, 202, 73, 81, 84, 200, 64, 225, 149, 100, 164,
        42, 20, 150, 102, 38, 103, 43, 36, 21, 229, 151, 231, 41, 164, 229, 87, 40, 100, 149, 230,
        22, 20, 43, 228, 151, 165, 22, 129, 165, 115, 18, 171, 42, 21, 82, 242, 211, 245, 0,
    ];

    // gzip container (RFC 1952), level 6.
    #[cfg(feature = "gzip")]
    const GZIP_STREAM: [u8; 77] = [
        31, 139, 8, 0, 147, 206, 60, 106, 0, 255, 203, 72, 205, 201, 201, 215, 81, 40, 207, 47,
        202, 73, 81, 84, 200, 64, 225, 149, 100, 164, 42, 20, 150, 102, 38, 103, 43, 36, 21, 229,
        151, 231, 41, 164, 229, 87, 40, 100, 149, 230, 22, 20, 43, 228, 151, 165, 22, 129, 165,
        115, 18, 171, 42, 21, 82, 242, 211, 245, 0, 76, 208, 122, 218, 72, 0, 0, 0,
    ];

    /// Single-shot inflate of `stream` into an ample buffer.
    fn inflate_once(window_bits: i32, stream: &[u8]) -> (Vec<u8>, Result<ReturnCode>) {
        let mut st = InflateState::new(window_bits).expect("init");
        let mut out = [0u8; 512];
        let r = st.inflate(stream, &mut out, Flush::Finish);
        (out[..r.produced].to_vec(), r.status)
    }

    /// Inflate `stream`, feeding `in_chunk` input bytes and writing into
    /// `out_chunk`-sized output windows per call, to exercise save/resume.
    fn inflate_chunked(
        window_bits: i32,
        stream: &[u8],
        in_chunk: usize,
        out_chunk: usize,
    ) -> (Vec<u8>, Result<ReturnCode>) {
        let mut st = InflateState::new(window_bits).expect("init");
        let mut produced = Vec::new();
        let mut consumed_total = 0usize;
        loop {
            let in_end = core::cmp::min(consumed_total + in_chunk, stream.len());
            let mut out = alloc::vec![0u8; out_chunk];
            let r = st.inflate(&stream[consumed_total..in_end], &mut out, Flush::NoFlush);
            consumed_total += r.consumed;
            produced.extend_from_slice(&out[..r.produced]);
            match r.status {
                Ok(ReturnCode::StreamEnd) => return (produced, Ok(ReturnCode::StreamEnd)),
                Ok(_) => {
                    // Guard the test against a stall (should not happen for a
                    // well-formed stream with input still available).
                    if r.consumed == 0 && r.produced == 0 && consumed_total >= stream.len() {
                        return (produced, r.status);
                    }
                }
                Err(e) => return (produced, Err(e)),
            }
        }
    }

    #[test]
    fn inflate_mode_sentinels() {
        assert_eq!(InflateMode::Head as isize, 16180);
        assert_eq!(
            (InflateMode::Sync as isize) - (InflateMode::Head as isize),
            31
        );
    }

    #[test]
    fn inflate_zlib_single_shot() {
        let (out, status) = inflate_once(15, &ZLIB_STREAM);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[test]
    fn inflate_raw_single_shot() {
        let (out, status) = inflate_once(-15, &RAW_STREAM);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn inflate_gzip_single_shot() {
        // windowBits 31 == 15 | 16 selects gzip-only decoding.
        let (out, status) = inflate_once(31, &GZIP_STREAM);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn inflate_auto_detect_gzip_and_zlib() {
        // windowBits 47 == 15 | 32 enables zlib/gzip auto-detection.
        let (g, gs) = inflate_once(47, &GZIP_STREAM);
        assert_eq!(gs, Ok(ReturnCode::StreamEnd));
        assert_eq!(g.as_slice(), MSG);
        let (z, zs) = inflate_once(47, &ZLIB_STREAM);
        assert_eq!(zs, Ok(ReturnCode::StreamEnd));
        assert_eq!(z.as_slice(), MSG);
    }

    #[test]
    fn inflate_zlib_byte_at_a_time_input() {
        // One input byte per call: exercises the PULLBYTE/NEEDBITS save/resume.
        let (out, status) = inflate_chunked(15, &ZLIB_STREAM, 1, 512);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[test]
    fn inflate_zlib_tiny_output_windows() {
        // Tiny output windows force the sliding-window copy path across calls.
        let (out, status) = inflate_chunked(15, &ZLIB_STREAM, 64, 7);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[test]
    fn inflate_raw_tiny_output_windows() {
        let (out, status) = inflate_chunked(-15, &RAW_STREAM, 64, 5);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.as_slice(), MSG);
    }

    #[test]
    fn reset2_window_bits_overloading() {
        // These cases resolve identically whether or not the `gzip` feature is
        // enabled (none of them depend on the gzip-gated `&= 15` masking), so
        // the assertions hold across every feature configuration.
        //
        // Accepted: explicit zlib window sizes 8..=15, the matching raw range
        // -8..=-15, and 0 — a special value meaning "use the window size from
        // the zlib header". C's guard is `if (windowBits && (out of range))`,
        // so a zeroed windowBits short-circuits past the range check.
        for wb in [0, 8, 9, 15, -8, -9, -15] {
            assert!(InflateState::new(wb).is_ok(), "windowBits {wb} should init");
        }
        // Rejected: a non-zero window outside 8..=15 that never masks back into
        // range — 7/-7 (too small), -16 (below the raw floor), and 48 (>= 48 so
        // C never applies the `&= 15` masking, leaving it out of range).
        for wb in [7, -7, -16, 48] {
            assert!(
                InflateState::new(wb).is_err(),
                "windowBits {wb} should be rejected"
            );
        }
        // The gzip/auto ranges (24..=31, 40..=47) and the masking edge cases
        // (e.g. 16 -> 0) are feature-dependent and are covered by the
        // gzip-gated `reset2_gzip_and_auto_ranges_accepted` test.
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn reset2_gzip_and_auto_ranges_accepted() {
        for wb in [24, 31, 40, 47] {
            assert!(InflateState::new(wb).is_ok(), "windowBits {wb} should init");
        }
    }

    #[test]
    fn bad_zlib_header_is_data_error() {
        // Corrupt the FCHECK so the header fails the %31 validation.
        let mut bad = ZLIB_STREAM;
        bad[1] = 0;
        let (_out, status) = inflate_once(15, &bad);
        assert_eq!(status, Err(ZlibError::DataError));
    }

    #[test]
    fn truncated_stream_does_not_reach_stream_end() {
        // Feeding only the header bytes must not yield StreamEnd.
        let (_out, status) = inflate_once(15, &ZLIB_STREAM[..2]);
        assert_ne!(status, Ok(ReturnCode::StreamEnd));
    }

    #[test]
    fn auxiliary_accessors_behave() {
        let st = InflateState::new(15).expect("init");
        assert_eq!(st.mode(), InflateMode::Head);
        assert!(!st.sync_point());
        // Fresh state: inflateMark packs back = -1 -> -(1 << 16).
        assert_eq!(st.mark(), -(1i64 << 16));
        assert_eq!(st.codes_used(), 0);
        // No window yet: dictionary length is zero.
        assert_eq!(st.get_dictionary(None), 0);
    }

    #[test]
    fn copy_produces_independent_state() {
        let mut st = InflateState::new(15).expect("init");
        let mut out = [0u8; 512];
        let _ = st.inflate(&ZLIB_STREAM, &mut out, Flush::Finish);
        let cloned = st.copy();
        assert_eq!(cloned.total_out, st.total_out);
        assert_eq!(cloned.check, st.check);
    }

    #[test]
    fn preset_dictionary_round_trip_is_accepted() {
        // Regression test for the preset-dictionary Adler seed (finding #3).
        // `deflateSetDictionary` stores the dictionary identifier as
        // `adler32(1, dict)` (C seeds with `adler32(0L, Z_NULL, 0) == 1`), so
        // `inflateSetDictionary` must verify against the same seed. Before the
        // fix the inflate side checked `adler32(0, dict)` and wrongly rejected a
        // valid preset-dictionary stream with `Z_DATA_ERROR`.
        use crate::deflate::{deflate, deflate_init, deflate_set_dictionary};
        use crate::stream::ZStream;

        let dictionary = b"the quick brown fox jumps over the lazy dog";
        let data = MSG;

        // Compress with a preset dictionary (zlib wrapper -> FDICT set).
        let mut strm = ZStream::default();
        deflate_init(&mut strm, 6).expect("deflate_init");
        deflate_set_dictionary(&mut strm, dictionary).expect("deflate_set_dictionary");
        // The deflate side seeds the dictionary id with 1.
        assert_eq!(strm.adler, crate::checksum::adler32(1, dictionary));

        let mut compressed = alloc::vec![0u8; 4096];
        let (res, consumed, produced) = deflate(&mut strm, data, &mut compressed, Flush::Finish);
        assert_eq!(res, Ok(ReturnCode::StreamEnd));
        assert_eq!(consumed, data.len());
        compressed.truncate(produced);

        // Decompress: the header requests a dictionary, which must now be
        // accepted (this was `Err(ZlibError::DataError)` before the fix).
        let mut st = InflateState::new(15).expect("init");
        let mut out = alloc::vec![0u8; 4096];
        let r1 = st.inflate(&compressed, &mut out, Flush::NoFlush);
        assert_eq!(r1.status, Ok(ReturnCode::NeedDict));
        st.set_dictionary(dictionary)
            .expect("valid preset dictionary must be accepted");
        let r2 = st.inflate(
            &compressed[r1.consumed..],
            &mut out[r1.produced..],
            Flush::Finish,
        );
        assert_eq!(r2.status, Ok(ReturnCode::StreamEnd));
        let total = r1.produced + r2.produced;
        assert_eq!(&out[..total], data);
    }
}
