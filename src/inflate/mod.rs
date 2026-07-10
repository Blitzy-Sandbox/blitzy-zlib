//! DEFLATE decompression engine — a safe Rust port of zlib's `inflate.c`.
//!
//! This module is the root of the `src/inflate/` tree. It declares the engine
//! submodules, re-exports the public inflate surface, and — most importantly —
//! contains the central [`inflate`] mode-loop state machine together with the
//! full inflate public API (init / reset / dictionary / sync / copy / prime /
//! mark / header / validate and friends).
//!
//! # Byte-exact fidelity
//!
//! The decoded output and the error/recovery behaviour of this module are
//! **byte-identical** to reference zlib for the same compressed input — that is
//! the defining acceptance criterion (AAP §0.6.1, §0.6.4, §0.7.1). Every check,
//! every diagnostic message string, and the exact order of bit reads are
//! preserved verbatim from `inflate.c`. In particular:
//!
//! * The `data_type` formula reported on return (`inflate.c` L1147-L1149) is
//!   reproduced exactly.
//! * The zlib Adler-32 trailer and the gzip CRC-32 + ISIZE trailers are checked
//!   with the same byte order and the same "incorrect data/length check"
//!   diagnostics.
//! * `inflateSync` scans for the `00 00 FF FF` marker and restarts decoding
//!   with the identical state fix-up C performs.
//!
//! # Idiomatic wins over C
//!
//! * The C `switch (state->mode)` with `goto` fall-through becomes an
//!   exhaustive `match` over [`InflateMode`] inside a `'inf_leave` loop; the
//!   compiler statically guarantees no mode is left unhandled (AAP §0.6.1). C
//!   `break` (re-enter the `for(;;)`) becomes `continue 'inf_leave`, and C
//!   `goto inf_leave` becomes `break 'inf_leave`.
//! * [`inflate_copy`] is a straightforward deep clone: because the state uses
//!   `usize` table offsets plus a fixed/dynamic discriminator instead of
//!   self-referential raw pointers, the C pointer-fix-up dance in `inflateCopy`
//!   is unnecessary — a genuine memory-safety win.
//! * Dropping the boxed [`InflateState`] frees the sliding window automatically,
//!   so [`inflate_end`] merely detaches the state (RAII subsumes the
//!   `ZFREE(window)` that C `inflateEnd` performs).
//!
//! # Safety and portability
//!
//! There is **zero `unsafe`** anywhere in this file — and, per AAP §0.6.1, in
//! the entire inflate layer including the `fast.rs` fast path; all `unsafe` in
//! the crate is confined to the `src/ffi/` C-ABI boundary. The module is
//! `no_std` + `alloc`
//! and targets the Rust 2024 edition (MSRV 1.85.0). All gzip-framing code is
//! gated behind the `gzip` cargo feature; with gzip disabled, [`inflate`]
//! still fully handles zlib and raw DEFLATE streams.

// ---------------------------------------------------------------------------
// Submodule declarations.
//
// Declared `pub mod` because this file *is* `inflate/mod.rs`: the sibling
// modules reach each other by absolute path (e.g.
// `use crate::inflate::state::InflateState;`), and `src/stream.rs`,
// `src/util/uncompress.rs`, and `src/ffi/inflate.rs` reach into this tree.
// ---------------------------------------------------------------------------
pub mod back;
pub mod fast;
pub mod fixed;
pub mod state;
pub mod tables;

// Public re-exports: the state model and the table-builder surface the rest of
// the crate consumes, plus the raw-callback back-inflate API.
pub use back::{InFunc, OutFunc, inflate_back, inflate_back_end, inflate_back_init};
pub use state::{InflateMode, InflateState};
pub use tables::{Code, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS, MAXBITS, inflate_table};

// ---------------------------------------------------------------------------
// Imports.
// ---------------------------------------------------------------------------
use alloc::boxed::Box;

use crate::checksum::adler32;
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{DEF_WBITS, MAX_WBITS, Z_BLOCK, Z_DEFLATED, Z_FINISH, Z_TREES};
use crate::error::{ReturnCode, ZlibError};
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;
use crate::stream::{AllocBuffer, Allocator, StreamState, ZStream};

use crate::inflate::fast::inflate_fast;
use crate::inflate::fixed::{DISTFIX, LENFIX};
use crate::inflate::state::TableSource;
use crate::inflate::tables::CodeType;

// ---------------------------------------------------------------------------
// Result / outcome types.
// ---------------------------------------------------------------------------

/// The internal `Result` alias used across the inflate init/reset/dictionary
/// API: `Ok` carries a success [`ReturnCode`] (typically [`ReturnCode::Ok`],
/// occasionally [`ReturnCode::NeedDict`]), while `Err` carries a [`ZlibError`].
/// The FFI boundary flattens both arms back to the integer C return codes.
pub type InflateResult = Result<ReturnCode, ZlibError>;

/// The result of a streaming [`inflate`] call.
///
/// Because [`ZStream`] carries no `next_in`/`next_out`/`avail_*` cursor fields
/// (input and output are passed as slices per call), progress is reported
/// explicitly: `consumed` bytes were read from `input` and `produced` bytes were
/// written to `output`. The FFI boundary uses these to advance the caller's
/// `z_stream` cursors. This mirrors the deflate engine's `DeflateOutcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InflateOutcome {
    /// The zlib return code produced by the call. Typically
    /// [`ReturnCode::Ok`], [`ReturnCode::StreamEnd`], [`ReturnCode::NeedDict`],
    /// [`ReturnCode::BufError`], [`ReturnCode::DataError`], or
    /// [`ReturnCode::StreamError`].
    pub code: ReturnCode,
    /// Number of input bytes consumed from the supplied `input` slice.
    pub consumed: usize,
    /// Number of output bytes written to the supplied `output` slice.
    pub produced: usize,
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// Reproduces the C `ZSWAP32` macro (`zutil.h` L258): a 32-bit byte swap used to
/// read the big-endian Adler-32 in a zlib trailer / dictionary id. Rust's
/// [`u32::swap_bytes`] is exactly this operation.
#[inline]
fn zswap32(q: u32) -> u32 {
    q.swap_bytes()
}

/// The Adler-32 checksum of the empty input (`adler32(0, Z_NULL, 0) == 1`).
///
/// Reference zlib obtains a fresh Adler-32 accumulator by calling
/// `adler32(0L, Z_NULL, 0)`; the idiomatic Rust [`adler32`] returns its input
/// unchanged for an empty slice, so the initializer is simply this literal `1`
/// (see the checksum-init idiom in the module port notes / AAP §0.6.4). Using
/// the literal avoids the incorrect `adler32(0, &[])`, which would yield `0`.
const ADLER32_INIT: u32 = 1;

/// The CRC-32 checksum of the empty input (`crc32(0L, Z_NULL, 0) == 0`).
///
/// The gzip counterpart of [`ADLER32_INIT`]: a fresh CRC-32 accumulator is the
/// literal `0`.
#[cfg(feature = "gzip")]
const CRC32_INIT: u32 = 0;

/// The running-check dispatch (C `UPDATE_CHECK` macro, `inflate.c` L301-L306):
/// folds `buf` into the running check value, choosing CRC-32 for a gzip stream
/// (`state.flags != 0`) or Adler-32 otherwise.
///
/// This is only ever invoked when the validate bit (`wrap & 4`) is set, which
/// never happens for a raw stream — so the `flags != 0` test cleanly selects
/// CRC-32 for gzip and Adler-32 for zlib.
#[cfg(feature = "gzip")]
#[inline]
fn update_check(flags: i32, check: u32, buf: &[u8]) -> u32 {
    if flags != 0 {
        crc32(check, buf)
    } else {
        adler32(check, buf)
    }
}

/// The running-check dispatch when gzip support is compiled out: always
/// Adler-32 (C `#else` branch of `UPDATE_CHECK`).
#[cfg(not(feature = "gzip"))]
#[inline]
fn update_check(_flags: i32, check: u32, buf: &[u8]) -> u32 {
    adler32(check, buf)
}

/// Folds the two low little-endian bytes of `word` into the gzip header CRC —
/// the Rust port of the C `CRC2` macro (`inflate.c` L143-L149).
///
/// Used while parsing a gzip header (`FLAGS`/`OS`/`EXLEN`/`HCRC`) to accumulate
/// the header's own CRC-16 (the low 16 bits of a running CRC-32), which is later
/// verified against the `FHCRC` trailer.
#[cfg(feature = "gzip")]
#[inline]
fn crc2(check: &mut u32, word: u32) {
    let hbuf = [word as u8, (word >> 8) as u8];
    *check = crc32(*check, &hbuf);
}

/// Folds the four little-endian bytes of `word` into the gzip header CRC — the
/// Rust port of the C `CRC4` macro (`inflate.c` L151-L160), used for the 32-bit
/// modification-time field in `TIME`.
#[cfg(feature = "gzip")]
#[inline]
fn crc4(check: &mut u32, word: u32) {
    let hbuf = [
        word as u8,
        (word >> 8) as u8,
        (word >> 16) as u8,
        (word >> 24) as u8,
    ];
    *check = crc32(*check, &hbuf);
}

/// The permutation of code-length code indices used while reading a dynamic
/// block's code-length code lengths (C `order[19]`, `inflate.c` L491-L492).
const ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ===========================================================================
// InflateIo — the input/output cursor + bit-accumulator bundle.
//
// This is the slice-based analogue of the C `next`/`have`/`put`/`left`/`hold`/
// `bits` locals threaded through `inflate()`. Keeping them in one struct with
// small `#[inline]` methods reproduces the C `LOAD`/`RESTORE`/`NEEDBITS`/…
// macro group without any `unsafe`, and mirrors the `BackCtx` design used by
// the sibling `back.rs`.
// ===========================================================================

/// Bit-accumulator + I/O cursor bundle threaded through [`inflate`].
struct InflateIo<'a> {
    /// The available input bytes (C `next` points into this; `avail_in` is its
    /// length).
    input: &'a [u8],
    /// Read cursor into [`input`](InflateIo::input); equal to the number of
    /// input bytes consumed so far this call (C `next - base`; `have` is
    /// `input.len() - next`).
    next: usize,
    /// The output buffer (C `put` points into this; `avail_out` is its length).
    output: &'a mut [u8],
    /// Write cursor into [`output`](InflateIo::output); equal to the number of
    /// output bytes produced so far this call (C `put - base`; `left` is
    /// `output.len() - put`).
    put: usize,
    /// Input bit accumulator (C `hold`; only the low 32 bits are ever used).
    hold: u32,
    /// Number of valid bits currently held in [`hold`](InflateIo::hold) (C
    /// `bits`).
    bits: u32,
}

impl InflateIo<'_> {
    /// Number of input bytes still available (C `have`).
    #[inline]
    fn have(&self) -> usize {
        self.input.len() - self.next
    }

    /// Number of output bytes of free space remaining (C `left`).
    #[inline]
    fn left(&self) -> usize {
        self.output.len() - self.put
    }

    /// C `PULLBYTE()`: pull one input byte into the bit accumulator.
    ///
    /// Returns [`None`] when no input is available — the caller translates that
    /// into `break 'inf_leave` (the C macro's `goto inf_leave`). The new byte's
    /// eight bits sit above the `bits` already present, so `|=` is exactly the C
    /// `+=` (no carry: every call site has `bits < 32`, and the paths that reach
    /// `NEEDBITS(32)` are byte-aligned, so the shift is at most 24).
    #[inline]
    fn pull_byte(&mut self) -> Option<()> {
        if self.have() == 0 {
            return None;
        }
        self.hold |= u32::from(self.input[self.next]) << self.bits;
        self.next += 1;
        self.bits += 8;
        Some(())
    }

    /// C `NEEDBITS(n)`: assure at least `n` bits are in the accumulator, pulling
    /// input bytes as needed. Returns [`None`] if input is exhausted first.
    #[inline]
    fn need_bits(&mut self, n: u32) -> Option<()> {
        while self.bits < n {
            self.pull_byte()?;
        }
        Some(())
    }

    /// C `BITS(n)`: return the low `n` bits of the accumulator (`n <= 16`).
    #[inline]
    fn bits_val(&self, n: u32) -> u32 {
        self.hold & ((1u32 << n) - 1)
    }

    /// C `DROPBITS(n)`: remove `n` bits from the accumulator.
    #[inline]
    fn drop_bits(&mut self, n: u32) {
        self.hold >>= n;
        self.bits -= n;
    }

    /// C `BYTEBITS()`: drop 0-7 bits to reach a byte boundary.
    #[inline]
    fn byte_align(&mut self) {
        let rem = self.bits & 7;
        self.hold >>= rem;
        self.bits -= rem;
    }

    /// C `INITBITS()`: clear the bit accumulator.
    #[inline]
    fn init_bits(&mut self) {
        self.hold = 0;
        self.bits = 0;
    }
}

// ===========================================================================
// Private helpers.
// ===========================================================================

/// Installs the fixed (static) Huffman decode tables, mirroring the
/// non-`BUILDFIXED` branch of C `fixedtables` (`inflate.c` L216-L260) which
/// simply points the decoder at the pre-generated `lenfix`/`distfix` tables.
///
/// Rather than copying the tables, the state records that the fixed tables are
/// in use ([`TableSource::Fixed`]) with the canonical root bit-lengths (`9` for
/// literals/lengths, `5` for distances); [`InflateState::lencode_slice`] and
/// [`InflateState::distcode_slice`] then resolve to [`LENFIX`]/[`DISTFIX`].
#[inline]
fn fixedtables(state: &mut InflateState) {
    state.lentable = TableSource::Fixed;
    state.lenbits = 9;
    state.disttable = TableSource::Fixed;
    state.distbits = 5;
}

/// Copies the most-recent `copy` output bytes into the circular sliding window,
/// lazily allocating the window on first use — a faithful port of C
/// `updatewindow` (`inflate.c` L252-L296).
///
/// `output[..end]` is the output produced during the current [`inflate`] call;
/// the bytes `output[end - copy .. end]` are the ones to fold into the window.
/// The wrap-around copy math is reproduced exactly, because `fast::inflate_fast`
/// relies on identical `wsize`/`whave`/`wnext` bookkeeping when it copies match
/// bytes out of the window.
///
/// The window is an owned [`AllocBuffer<u8>`]: when the owning stream carries a
/// caller-supplied `zalloc`/`zfree` (installed through the FFI `z_stream`), the
/// window is allocated through those hooks via the state's stored
/// [`alloc_hook`](InflateState::alloc_hook) (AAP §0.6.3; QA FINDING-3);
/// otherwise it uses the Rust global allocator.
///
/// # Errors
///
/// Returns [`ZlibError::MemError`] when the lazy window allocation is routed
/// through an active caller hook whose `zalloc` reports out-of-memory. This is
/// the faithful port of C `updatewindow` returning `1` on `ZALLOC` failure,
/// which the callers translate into the `MEM` mode / `Z_MEM_ERROR` (M7). The
/// global-allocator path is infallible (it aborts on OOM per Rust convention),
/// so this only fails for a caller-installed bounded allocator.
fn updatewindow(
    state: &mut InflateState,
    output: &[u8],
    end: usize,
    mut copy: usize,
) -> Result<(), ZlibError> {
    // If it hasn't been done already, allocate space for the window — routed
    // through the caller's allocator hook when one was installed. An active-hook
    // OOM propagates as `Z_MEM_ERROR` (M7) rather than falling back to global.
    if state.window.is_empty() {
        state.window = AllocBuffer::try_zeroed(1usize << state.wbits, state.alloc_hook)
            .ok_or(ZlibError::MemError)?;
    }

    // If the window is not in use yet, initialise its geometry.
    let wsize = 1usize << state.wbits;
    if state.wsize == 0 {
        state.wsize = wsize as u32;
        state.wnext = 0;
        state.whave = 0;
    }
    let wsize = state.wsize as usize;

    // Copy `wsize` or fewer output bytes into the circular window.
    if copy >= wsize {
        // The last `wsize` bytes fill the whole window.
        state.window[..wsize].copy_from_slice(&output[end - wsize..end]);
        state.wnext = 0;
        state.whave = wsize as u32;
    } else {
        let wnext = state.wnext as usize;
        let mut dist = wsize - wnext;
        if dist > copy {
            dist = copy;
        }
        // First span: from `end - copy` into the window at `wnext`.
        state.window[wnext..wnext + dist].copy_from_slice(&output[end - copy..end - copy + dist]);
        copy -= dist;
        if copy > 0 {
            // Wrap-around span: remaining bytes go to the front of the window.
            state.window[..copy].copy_from_slice(&output[end - copy..end]);
            state.wnext = copy as u32;
            state.whave = wsize as u32;
        } else {
            let mut next = wnext + dist;
            if next == wsize {
                next = 0;
            }
            state.wnext = next as u32;
            if (state.whave as usize) < wsize {
                state.whave += dist as u32;
            }
        }
    }

    Ok(())
}

/// Scans `buf` for the 4-byte flush marker `00 00 FF FF`, a faithful port of C
/// `syncsearch` (`inflate.c` L1244-L1262).
///
/// `have` carries the number of pattern bytes matched so far (`0..=4`) across
/// calls; it is updated in place. The return value is the number of bytes of
/// `buf` consumed. When `*have` reaches `4` the full marker has been found.
fn syncsearch(have: &mut u32, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next = 0usize;
    let len = buf.len();
    while next < len && got < 4 {
        let want: u8 = if got < 2 { 0 } else { 0xff };
        if buf[next] == want {
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
// Reset / initialisation API.
// ===========================================================================

/// Resets the stream for a fresh decode **without** discarding the sliding
/// window contents — the Rust port of C `inflateResetKeep` (`inflate.c`
/// L100-L123).
///
/// Clears the total counters, `msg`, and `data_type`; restores the running
/// `adler` mirror to the wrapper's initial value (`wrap & 1`); and delegates the
/// per-stream decode-state reset to [`InflateState::reset_keep`]. The window
/// geometry ([`wsize`](InflateState::wsize)/[`whave`](InflateState::whave)/
/// [`wnext`](InflateState::wnext)) is intentionally left intact so history can
/// be reused (e.g. after [`inflate_sync`]).
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state installed.
pub fn inflate_reset_keep<A: Allocator>(strm: &mut ZStream<A>) -> InflateResult {
    let wrap = {
        let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
        state.reset_keep();
        state.wrap
    };
    strm.total_in = 0;
    strm.total_out = 0;
    strm.msg = None;
    strm.data_type = 0;
    // "to support ill-conceived Java test suite": expose the wrapper's initial
    // Adler value (1 for zlib/auto, 0 for gzip/raw) even before any data.
    if wrap != 0 {
        strm.adler = (wrap & 1) as u32;
    }
    Ok(ReturnCode::Ok)
}

/// Resets the stream for a fresh decode, additionally invalidating the sliding
/// window contents — the Rust port of C `inflateReset` (`inflate.c` L125-L134).
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state installed.
pub fn inflate_reset<A: Allocator>(strm: &mut ZStream<A>) -> InflateResult {
    {
        let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
        state.wsize = 0;
        state.whave = 0;
        state.wnext = 0;
    }
    inflate_reset_keep(strm)
}

/// Reconfigures the wrapper and window size, then resets — the Rust port of C
/// `inflateReset2` (`inflate.c` L136-L171).
///
/// `window_bits` is overloaded exactly as in zlib:
///
/// * `8..=15` — zlib (RFC 1950) wrapper, `wrap = 5` (check + validate).
/// * `-15..=-8` — raw DEFLATE, `wrap = 0` (no wrapper, no check).
/// * `24..=31` — gzip (RFC 1952) wrapper (`= 16 + windowBits`), `wrap = 6`.
/// * `40..=47` — automatic zlib/gzip detection (`= 32 + windowBits`),
///   `wrap = 7`.
/// * `0` — permitted; the effective window size is taken from the zlib header.
///
/// The `wrap` encoding is `(windowBits >> 4) + 5` for the non-negative cases:
/// bit 0 selects the zlib Adler check, bit 1 selects gzip, and bit 2 enables
/// check validation. When gzip support is compiled out, the `& 15` masking of
/// the gzip/auto ranges does not occur, so those requests fail the `8..=15`
/// bounds test — matching C built without `GUNZIP`.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] for an invalid `window_bits` or if `strm`
/// has no inflate state.
pub fn inflate_reset2<A: Allocator>(strm: &mut ZStream<A>, window_bits: i32) -> InflateResult {
    {
        let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;

        // Extract the wrap request from the windowBits parameter.
        let wrap;
        let mut wb = window_bits;
        if wb < 0 {
            if wb < -15 {
                return Err(ZlibError::StreamError);
            }
            wrap = 0;
            wb = -wb;
        } else {
            wrap = (wb >> 4) + 5;
            #[cfg(feature = "gzip")]
            if wb < 48 {
                wb &= 15;
            }
        }

        // Validate the window size (0 is allowed: "use the zlib header size").
        if wb != 0 && !(8..=MAX_WBITS).contains(&wb) {
            return Err(ZlibError::StreamError);
        }

        // Free the window if the size changed, so it is re-sized on next use.
        // Assigning an empty `AllocBuffer` drops the previous one, which routes
        // through the caller's `zfree` when the window was hook-backed
        // (AAP §0.6.3; QA FINDING-3). The stored `alloc_hook` is left intact so
        // the re-allocation on next use goes through the same allocator.
        if !state.window.is_empty() && state.wbits != wb as u32 {
            state.window = AllocBuffer::default();
        }

        state.wrap = wrap;
        state.wbits = wb as u32;
    }
    inflate_reset(strm)
}

/// Allocates and initialises the inflate state for `strm` with the given
/// `window_bits` — the Rust port of C `inflateInit2_` (`inflate.c` L173-L212).
///
/// The version/`stream_size` compatibility check that C performs is a
/// concern of the FFI boundary (where a real `z_stream` layout exists); this
/// idiomatic entry point installs a fresh [`InflateState`] and delegates to
/// [`inflate_reset2`].
///
/// # Allocation and the caller's hook
/// C `inflateInit2_` allocates the state struct through the caller's
/// allocator (`ZALLOC(strm, 1, sizeof(struct inflate_state))`) before any
/// window is needed, so a null/failing `zalloc` fails the init with
/// `Z_MEM_ERROR`. The idiomatic state here lives in a Rust [`Box`], but to
/// preserve that observable contract this path *also* reserves the equivalent
/// footprint through the caller's [`AllocHook`](crate::stream::AllocHook) (parked in
/// [`InflateState::state_alloc`]) whenever a hook is active. A hook whose
/// `zalloc` reports out-of-memory therefore surfaces [`ZlibError::MemError`]
/// here — matching C's allocation count (one at init for a single-shot inflate)
/// and failure timing. Under the global allocator no extra allocation is made,
/// keeping the crate's ~7 KB inflate memory-bounds parity (AAP §0.7.1). The
/// `inflateBack` init path does not go through here, so its single
/// (window-only) allocation is unaffected.
///
/// # Errors
/// Returns [`ZlibError::MemError`] when an active caller hook's `zalloc` reports
/// out-of-memory for the state reservation, and propagates
/// [`ZlibError::StreamError`] from [`inflate_reset2`] for an invalid
/// `window_bits`; the partially-installed state is torn down on error.
pub fn inflate_init2<A: Allocator>(strm: &mut ZStream<A>, window_bits: i32) -> InflateResult {
    strm.msg = None;
    // `new_in` sets mode = Head so the state passes reset2's inflate-state
    // guard; wrap/wbits are (re)assigned by inflate_reset2 below. The caller's
    // allocator hook (the `zalloc`/`zfree` installed via the FFI `z_stream`, or
    // a no-op under the global allocator) is threaded in so the lazily-allocated
    // window is later routed through it (AAP §0.6.3; QA FINDING-3).
    let hook = strm.allocator().hook();
    let mut state = InflateState::new_in(hook, 0, 0);
    // Route the inflate *state* allocation through the caller's hook, mirroring
    // C `inflateInit2_`'s `ZALLOC(strm, 1, sizeof(struct inflate_state))`. The
    // idiomatic state lives in a global `Box`; this reserves the equivalent
    // footprint through an active caller hook so a limited/failing `zalloc`
    // surfaces `Z_MEM_ERROR` at init — before the window is needed — matching
    // C's allocation count and failure timing (AAP §0.6.3/§0.6.5). Under the
    // global allocator (`!hook.is_active()`) no extra allocation is made, so the
    // ~7 KB inflate memory-bounds parity is preserved (AAP §0.7.1). This closes
    // the QA finding that the inflate state allocation bypassed the hook.
    if hook.is_active() {
        match AllocBuffer::try_zeroed(core::mem::size_of::<InflateState>(), hook) {
            Some(cell) => state.state_alloc = cell,
            // The state was not installed on the stream yet, so nothing to tear
            // down; report OOM exactly as C's failed state `ZALLOC` does.
            None => return Err(ZlibError::MemError),
        }
    }
    strm.set_inflate_state(state);
    match inflate_reset2(strm, window_bits) {
        Ok(rc) => Ok(rc),
        Err(e) => {
            // Mirror C freeing the state and nulling strm->state on failure. The
            // `state_alloc` reservation drops with the state, releasing it
            // through the caller's `zfree`.
            strm.clear_state();
            Err(e)
        }
    }
}

/// Allocates and initialises the inflate state using the default window size
/// ([`DEF_WBITS`] = 15) — the Rust port of C `inflateInit_` (`inflate.c`
/// L214-L217).
///
/// # Errors
/// See [`inflate_init2`].
pub fn inflate_init<A: Allocator>(strm: &mut ZStream<A>) -> InflateResult {
    inflate_init2(strm, DEF_WBITS)
}

/// Injects `bits` bits of `value` into the input bit accumulator — the Rust
/// port of C `inflatePrime` (`inflate.c` L219-L236).
///
/// This is used to resume decoding mid-byte (e.g. after [`inflate_copy`] or when
/// bits were pre-read by a caller). A negative `bits` flushes the accumulator; a
/// `bits` of `0` is a no-op. `bits` must be `<= 16` and must not overflow the
/// 32-bit accumulator.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state, if
/// `bits > 16`, or if adding `bits` would exceed the 32-bit accumulator.
pub fn inflate_prime<A: Allocator>(strm: &mut ZStream<A>, bits: i32, value: i32) -> InflateResult {
    let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
    if bits == 0 {
        return Ok(ReturnCode::Ok);
    }
    if bits < 0 {
        state.hold = 0;
        state.bits = 0;
        return Ok(ReturnCode::Ok);
    }
    if bits > 16 || state.bits + bits as u32 > 32 {
        return Err(ZlibError::StreamError);
    }
    let masked = (value as u32) & ((1u32 << bits) - 1);
    state.hold += masked << state.bits;
    state.bits += bits as u32;
    Ok(ReturnCode::Ok)
}

// ===========================================================================
// inflate() — the central mode-loop state machine.
// ===========================================================================

/// Decompresses as much data as possible — the Rust port of C `inflate`
/// (`inflate.c` L474-L1152), the heart of the decoder.
///
/// This consumes bytes from `input` and produces bytes into `output`, advancing
/// the internal state machine ([`InflateMode`]) until it runs out of input, runs
/// out of output space, reaches a requested block boundary, hits the end of the
/// stream, or encounters an error. The number of bytes actually read and written
/// is reported in the returned [`InflateOutcome`] (reference zlib mutates the
/// `z_stream` cursors instead; the FFI boundary maps between the two).
///
/// # Flush modes
/// `flush` is one of the zlib flush constants; only [`Z_BLOCK`]
/// (stop at block boundaries), [`Z_TREES`] (also stop after the block header),
/// and [`Z_FINISH`] (expect the whole stream) alter behaviour here — every other
/// value is treated like `Z_NO_FLUSH`.
///
/// # Return codes
/// * [`ReturnCode::Ok`] — progress was made; call again with more room/input.
/// * [`ReturnCode::StreamEnd`] — the end of the compressed stream was reached.
/// * [`ReturnCode::NeedDict`] — a preset dictionary is required (its Adler-32 id
///   is placed in [`ZStream::adler`]); supply it via [`inflate_set_dictionary`].
/// * [`ReturnCode::BufError`] — no progress was possible (needs more input or
///   output space).
/// * [`ReturnCode::DataError`] — the input is corrupt; [`ZStream::msg`] describes
///   the fault with the same wording reference zlib uses.
/// * [`ReturnCode::StreamError`] — the stream state is inconsistent (e.g. no
///   inflate state installed).
///
/// # Byte-exact fidelity
/// Every state transition, bounds test, checksum fold, and diagnostic string is
/// reproduced verbatim from `inflate.c`; the reported [`ZStream::data_type`]
/// uses the exact C formula (L1147-L1149). The slow path here and the fast path
/// in [`fast::inflate_fast`] are both entirely `unsafe`-free and together decode
/// byte-identically to reference zlib.
#[allow(clippy::too_many_lines)]
pub fn inflate<A: Allocator>(
    strm: &mut ZStream<A>,
    input: &[u8],
    output: &mut [u8],
    flush: i32,
) -> InflateOutcome {
    // ---- guard: an inflate state must be installed (C `inflateStateCheck`) ---
    //
    // Take the boxed state *out* of `strm` for the duration of the call. This
    // decouples the borrows: `strm` stays fully available for `strm.msg`,
    // `strm.total_*`, and `strm.adler`, while the owned `state` is mutated
    // freely. Every exit path reinstalls the state via `set_inflate_state`.
    let mut state: Box<InflateState> = match strm.take_state() {
        StreamState::Inflate(s) => s,
        // Not an inflate stream — put back whatever we removed and error out.
        StreamState::Deflate(d) => {
            strm.set_deflate_state(d);
            return InflateOutcome {
                code: ReturnCode::StreamError,
                consumed: 0,
                produced: 0,
            };
        }
        StreamState::None => {
            return InflateOutcome {
                code: ReturnCode::StreamError,
                consumed: 0,
                produced: 0,
            };
        }
    };

    // C: "if (state->mode == TYPE) state->mode = TYPEDO;  /* skip check */".
    // On re-entry at a block boundary, advance past the Z_BLOCK/Z_TREES early
    // exit so a fresh call makes progress rather than immediately returning.
    if state.mode == InflateMode::Type {
        state.mode = InflateMode::TypeDo;
    }

    // LOAD(): pull the bit accumulator into the local I/O context.
    let mut io = InflateIo {
        input,
        next: 0,
        output,
        put: 0,
        hold: state.hold,
        bits: state.bits,
    };

    // C snapshots `in = have; out = left;` at entry. `outck` is the running
    // "output checkpoint" (C's reused `out`): the free space at the last point
    // the running check was folded. It starts at the full output length and is
    // reset after the trailer check folds the final data bytes.
    let mut outck = io.left();
    let mut ret = ReturnCode::Ok;

    'inf_leave: loop {
        match state.mode {
            // ---------------------------------------------------------------
            // Stream header.
            // ---------------------------------------------------------------
            InflateMode::Head => {
                if state.wrap == 0 {
                    state.mode = InflateMode::TypeDo;
                    continue 'inf_leave;
                }
                if io.need_bits(16).is_none() {
                    break 'inf_leave;
                }
                #[cfg(feature = "gzip")]
                {
                    // gzip magic (0x1f, 0x8b) with gzip decoding enabled.
                    if (state.wrap & 2) != 0 && io.hold == 0x8b1f {
                        if state.wbits == 0 {
                            state.wbits = 15;
                        }
                        state.check = CRC32_INIT;
                        crc2(&mut state.check, io.hold);
                        io.init_bits();
                        state.mode = InflateMode::Flags;
                        continue 'inf_leave;
                    }
                    // Not gzip: mark any requested header as "not a gzip header".
                    if let Some(head) = state.head.as_mut() {
                        head.done = false;
                    }
                }
                // zlib header validation. The `wrap & 1` guard is present only
                // when gzip support exists (C wraps it in `#ifdef GUNZIP`).
                let header_bad = {
                    #[cfg(feature = "gzip")]
                    {
                        (state.wrap & 1) == 0 || ((io.bits_val(8) << 8) + (io.hold >> 8)) % 31 != 0
                    }
                    #[cfg(not(feature = "gzip"))]
                    {
                        ((io.bits_val(8) << 8) + (io.hold >> 8)) % 31 != 0
                    }
                };
                if header_bad {
                    strm.msg = Some("incorrect header check");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                if io.bits_val(4) != Z_DEFLATED as u32 {
                    strm.msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                io.drop_bits(4);
                let len = io.bits_val(4) + 8;
                if state.wbits == 0 {
                    state.wbits = len;
                }
                if len > 15 || len > state.wbits {
                    strm.msg = Some("invalid window size");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.dmax = 1u32 << len;
                state.flags = 0; // indicate zlib header
                state.check = ADLER32_INIT;
                state.mode = if (io.hold & 0x200) != 0 {
                    InflateMode::DictId
                } else {
                    InflateMode::Type
                };
                io.init_bits();
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // gzip header fields (RFC 1952). All gated on the `gzip` feature.
            // ---------------------------------------------------------------
            #[cfg(feature = "gzip")]
            InflateMode::Flags => {
                if io.need_bits(16).is_none() {
                    break 'inf_leave;
                }
                state.flags = io.hold as i32;
                if (state.flags & 0xff) != Z_DEFLATED {
                    strm.msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                if (state.flags & 0xe000) != 0 {
                    strm.msg = Some("unknown header flags set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                if let Some(head) = state.head.as_mut() {
                    head.text = ((io.hold >> 8) & 1) != 0;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2(&mut state.check, io.hold);
                }
                io.init_bits();
                state.mode = InflateMode::Time;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Time => {
                if io.need_bits(32).is_none() {
                    break 'inf_leave;
                }
                if let Some(head) = state.head.as_mut() {
                    head.time = io.hold;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc4(&mut state.check, io.hold);
                }
                io.init_bits();
                state.mode = InflateMode::Os;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Os => {
                if io.need_bits(16).is_none() {
                    break 'inf_leave;
                }
                if let Some(head) = state.head.as_mut() {
                    head.xflags = (io.hold & 0xff) as i32;
                    head.os = (io.hold >> 8) as i32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2(&mut state.check, io.hold);
                }
                io.init_bits();
                state.mode = InflateMode::ExLen;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::ExLen => {
                if (state.flags & 0x0400) != 0 {
                    if io.need_bits(16).is_none() {
                        break 'inf_leave;
                    }
                    state.length = io.hold;
                    // (Rust `GzHeader` has no `extra_len` field; the `extra`
                    // Vec's own length tracks how many bytes have been stored.)
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        crc2(&mut state.check, io.hold);
                    }
                    io.init_bits();
                } else if let Some(head) = state.head.as_mut() {
                    head.extra = None;
                }
                state.mode = InflateMode::Extra;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Extra => {
                if (state.flags & 0x0400) != 0 {
                    let mut copy = state.length as usize;
                    if copy > io.have() {
                        copy = io.have();
                    }
                    if copy > 0 {
                        // Store into the caller's `extra` buffer up to its cap.
                        // While fewer than `extra_max` bytes have been stored,
                        // the Vec's length equals the C `len = extra_len -
                        // length` write offset, so appending matches C exactly.
                        if let Some(head) = state.head.as_mut() {
                            let extra_max = head.extra_max as usize;
                            if let Some(extra) = head.extra.as_mut() {
                                if extra.len() < extra_max {
                                    let room = extra_max - extra.len();
                                    let n = core::cmp::min(copy, room);
                                    extra.extend_from_slice(&io.input[io.next..io.next + n]);
                                }
                            }
                        }
                        if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                            state.check = crc32(state.check, &io.input[io.next..io.next + copy]);
                        }
                        io.next += copy;
                        state.length -= copy as u32;
                    }
                    if state.length != 0 {
                        break 'inf_leave;
                    }
                }
                state.length = 0;
                state.mode = InflateMode::Name;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Name => {
                if (state.flags & 0x0800) != 0 {
                    if io.have() == 0 {
                        break 'inf_leave;
                    }
                    let mut copy = 0usize;
                    let mut last_byte: u8;
                    loop {
                        last_byte = io.input[io.next + copy];
                        copy += 1;
                        // Store the name without its terminating NUL, bounded by
                        // `name_max`; the Vec length is the write index.
                        if last_byte != 0 {
                            if let Some(head) = state.head.as_mut() {
                                let name_max = head.name_max as usize;
                                if let Some(name) = head.name.as_mut() {
                                    if name.len() < name_max {
                                        name.push(last_byte);
                                    }
                                }
                            }
                        }
                        if last_byte == 0 || copy >= io.have() {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &io.input[io.next..io.next + copy]);
                    }
                    io.next += copy;
                    if last_byte != 0 {
                        break 'inf_leave;
                    }
                } else if let Some(head) = state.head.as_mut() {
                    head.name = None;
                }
                state.length = 0;
                state.mode = InflateMode::Comment;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Comment => {
                if (state.flags & 0x1000) != 0 {
                    if io.have() == 0 {
                        break 'inf_leave;
                    }
                    let mut copy = 0usize;
                    let mut last_byte: u8;
                    loop {
                        last_byte = io.input[io.next + copy];
                        copy += 1;
                        if last_byte != 0 {
                            if let Some(head) = state.head.as_mut() {
                                let comm_max = head.comm_max as usize;
                                if let Some(comment) = head.comment.as_mut() {
                                    if comment.len() < comm_max {
                                        comment.push(last_byte);
                                    }
                                }
                            }
                        }
                        if last_byte == 0 || copy >= io.have() {
                            break;
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &io.input[io.next..io.next + copy]);
                    }
                    io.next += copy;
                    if last_byte != 0 {
                        break 'inf_leave;
                    }
                } else if let Some(head) = state.head.as_mut() {
                    head.comment = None;
                }
                state.mode = InflateMode::Hcrc;
                continue 'inf_leave;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Hcrc => {
                if (state.flags & 0x0200) != 0 {
                    if io.need_bits(16).is_none() {
                        break 'inf_leave;
                    }
                    if (state.wrap & 4) != 0 && io.hold != (state.check & 0xffff) {
                        strm.msg = Some("header crc mismatch");
                        state.mode = InflateMode::Bad;
                        continue 'inf_leave;
                    }
                    io.init_bits();
                }
                let flags = state.flags;
                if let Some(head) = state.head.as_mut() {
                    head.hcrc = ((flags >> 9) & 1) != 0;
                    head.done = true;
                }
                state.check = CRC32_INIT;
                state.mode = InflateMode::Type;
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // When gzip support is compiled out, the header-field modes above
            // are never entered (HEAD cannot transition into them). This
            // combined arm preserves `match` exhaustiveness without a wildcard.
            // ---------------------------------------------------------------
            #[cfg(not(feature = "gzip"))]
            InflateMode::Flags
            | InflateMode::Time
            | InflateMode::Os
            | InflateMode::ExLen
            | InflateMode::Extra
            | InflateMode::Name
            | InflateMode::Comment
            | InflateMode::Hcrc => {
                strm.msg = Some("unknown compression method");
                state.mode = InflateMode::Bad;
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Preset dictionary (zlib FDICT).
            // ---------------------------------------------------------------
            InflateMode::DictId => {
                if io.need_bits(32).is_none() {
                    break 'inf_leave;
                }
                state.check = zswap32(io.hold);
                io.init_bits();
                state.mode = InflateMode::Dict;
                continue 'inf_leave;
            }
            InflateMode::Dict => {
                if !state.havedict {
                    // C: `RESTORE(); return Z_NEED_DICT;` — no epilogue. Expose
                    // the dictionary id in `adler` so the caller can select the
                    // right dictionary, then hand the state back.
                    state.hold = io.hold;
                    state.bits = io.bits;
                    strm.adler = state.check;
                    let consumed = io.next;
                    let produced = io.put;
                    strm.set_inflate_state(state);
                    return InflateOutcome {
                        code: ReturnCode::NeedDict,
                        consumed,
                        produced,
                    };
                }
                state.check = ADLER32_INIT;
                state.mode = InflateMode::Type;
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Block header dispatch.
            // ---------------------------------------------------------------
            InflateMode::Type => {
                if flush == Z_BLOCK || flush == Z_TREES {
                    break 'inf_leave;
                }
                state.mode = InflateMode::TypeDo;
                continue 'inf_leave;
            }
            InflateMode::TypeDo => {
                if state.last {
                    io.byte_align();
                    state.mode = InflateMode::Check;
                    continue 'inf_leave;
                }
                if io.need_bits(3).is_none() {
                    break 'inf_leave;
                }
                state.last = io.bits_val(1) != 0;
                io.drop_bits(1);
                match io.bits_val(2) {
                    0 => {
                        // stored (uncompressed) block
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        // fixed Huffman block
                        fixedtables(&mut state);
                        state.mode = InflateMode::LenUnderscore;
                        if flush == Z_TREES {
                            io.drop_bits(2);
                            break 'inf_leave;
                        }
                    }
                    2 => {
                        // dynamic Huffman block
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        strm.msg = Some("invalid block type");
                        state.mode = InflateMode::Bad;
                    }
                }
                io.drop_bits(2);
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Stored (uncompressed) block.
            // ---------------------------------------------------------------
            InflateMode::Stored => {
                io.byte_align();
                if io.need_bits(32).is_none() {
                    break 'inf_leave;
                }
                if (io.hold & 0xffff) != ((io.hold >> 16) ^ 0xffff) {
                    strm.msg = Some("invalid stored block lengths");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.length = io.hold & 0xffff;
                io.init_bits();
                state.mode = InflateMode::CopyUnderscore;
                if flush == Z_TREES {
                    break 'inf_leave;
                }
                continue 'inf_leave;
            }
            InflateMode::CopyUnderscore => {
                state.mode = InflateMode::Copy;
                continue 'inf_leave;
            }
            InflateMode::Copy => {
                let mut copy = state.length as usize;
                if copy > 0 {
                    if copy > io.have() {
                        copy = io.have();
                    }
                    if copy > io.left() {
                        copy = io.left();
                    }
                    if copy == 0 {
                        break 'inf_leave;
                    }
                    let dst = io.put;
                    let src = io.next;
                    io.output[dst..dst + copy].copy_from_slice(&io.input[src..src + copy]);
                    io.next += copy;
                    io.put += copy;
                    state.length -= copy as u32;
                    continue 'inf_leave;
                }
                state.mode = InflateMode::Type;
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Dynamic block: read the code-length code lengths, then the
            // literal/length and distance code lengths, then build the tables.
            // ---------------------------------------------------------------
            InflateMode::Table => {
                if io.need_bits(14).is_none() {
                    break 'inf_leave;
                }
                state.nlen = io.bits_val(5) + 257;
                io.drop_bits(5);
                state.ndist = io.bits_val(5) + 1;
                io.drop_bits(5);
                state.ncode = io.bits_val(4) + 4;
                io.drop_bits(4);
                if state.nlen > 286 || state.ndist > 30 {
                    strm.msg = Some("too many length or distance symbols");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.have = 0;
                state.mode = InflateMode::LenLens;
                continue 'inf_leave;
            }
            InflateMode::LenLens => {
                while state.have < state.ncode {
                    if io.need_bits(3).is_none() {
                        break 'inf_leave;
                    }
                    let idx = ORDER[state.have as usize];
                    state.lens[idx] = io.bits_val(3) as u16;
                    state.have += 1;
                    io.drop_bits(3);
                }
                while state.have < 19 {
                    let idx = ORDER[state.have as usize];
                    state.lens[idx] = 0;
                    state.have += 1;
                }
                state.next = 0;
                state.lencode = 0;
                state.distcode = 0;
                state.lentable = TableSource::Dynamic;
                state.disttable = TableSource::Dynamic;
                let mut lb: usize = 7;
                let res = inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    &mut state.next,
                    &mut lb,
                    &mut state.work,
                );
                state.lenbits = lb as u32;
                if res.is_err() {
                    strm.msg = Some("invalid code lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.have = 0;
                state.mode = InflateMode::CodeLens;
                continue 'inf_leave;
            }
            InflateMode::CodeLens => {
                let total = (state.nlen + state.ndist) as usize;
                while (state.have as usize) < total {
                    // Decode one code-length code. The immutable `lencode`
                    // borrow lives only inside this block; `io` mutations are
                    // fine because `io` is disjoint from `state`.
                    let here: Code = {
                        let lc = state.lencode_slice(&LENFIX);
                        let mut h = lc[io.bits_val(state.lenbits) as usize];
                        while (h.bits as u32) > io.bits {
                            if io.pull_byte().is_none() {
                                break 'inf_leave;
                            }
                            h = lc[io.bits_val(state.lenbits) as usize];
                        }
                        h
                    };
                    if here.val < 16 {
                        io.drop_bits(here.bits as u32);
                        state.lens[state.have as usize] = here.val;
                        state.have += 1;
                    } else {
                        let len: u16;
                        let mut copy: u32;
                        if here.val == 16 {
                            if io.need_bits(here.bits as u32 + 2).is_none() {
                                break 'inf_leave;
                            }
                            io.drop_bits(here.bits as u32);
                            if state.have == 0 {
                                strm.msg = Some("invalid bit length repeat");
                                state.mode = InflateMode::Bad;
                                continue 'inf_leave;
                            }
                            len = state.lens[state.have as usize - 1];
                            copy = 3 + io.bits_val(2);
                            io.drop_bits(2);
                        } else if here.val == 17 {
                            if io.need_bits(here.bits as u32 + 3).is_none() {
                                break 'inf_leave;
                            }
                            io.drop_bits(here.bits as u32);
                            len = 0;
                            copy = 3 + io.bits_val(3);
                            io.drop_bits(3);
                        } else {
                            if io.need_bits(here.bits as u32 + 7).is_none() {
                                break 'inf_leave;
                            }
                            io.drop_bits(here.bits as u32);
                            len = 0;
                            copy = 11 + io.bits_val(7);
                            io.drop_bits(7);
                        }
                        if state.have as usize + copy as usize > total {
                            strm.msg = Some("invalid bit length repeat");
                            state.mode = InflateMode::Bad;
                            continue 'inf_leave;
                        }
                        while copy > 0 {
                            state.lens[state.have as usize] = len;
                            state.have += 1;
                            copy -= 1;
                        }
                    }
                }

                // Handle error breaks above (unreachable once BAD is set, since
                // those `continue 'inf_leave` re-dispatch to the BAD arm).
                if state.mode == InflateMode::Bad {
                    continue 'inf_leave;
                }

                // Check for an end-of-block code (length symbol 256).
                if state.lens[256] == 0 {
                    strm.msg = Some("invalid code -- missing end-of-block");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }

                // Build the literal/length decode table. `lenbits` is the
                // requested root size in/out; a temporary `usize` bridges the
                // `u32` state field and the `&mut usize` table API.
                state.next = 0;
                state.lencode = 0;
                state.lentable = TableSource::Dynamic;
                let nlen = state.nlen as usize;
                let mut lb: usize = 9;
                let res = inflate_table(
                    CodeType::Lens,
                    &state.lens,
                    nlen,
                    &mut state.codes,
                    &mut state.next,
                    &mut lb,
                    &mut state.work,
                );
                state.lenbits = lb as u32;
                if res.is_err() {
                    strm.msg = Some("invalid literal/lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }

                // Build the distance decode table, continuing from where the
                // literal/length table left off in the shared `codes` arena.
                state.distcode = state.next;
                state.disttable = TableSource::Dynamic;
                let ndist = state.ndist as usize;
                let mut db: usize = 6;
                let res = inflate_table(
                    CodeType::Dists,
                    &state.lens[nlen..],
                    ndist,
                    &mut state.codes,
                    &mut state.next,
                    &mut db,
                    &mut state.work,
                );
                state.distbits = db as u32;
                if res.is_err() {
                    strm.msg = Some("invalid distances set");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.mode = InflateMode::LenUnderscore;
                if flush == Z_TREES {
                    break 'inf_leave;
                }
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Symbol decode + copy loop.
            // ---------------------------------------------------------------
            InflateMode::LenUnderscore => {
                state.mode = InflateMode::Len;
                continue 'inf_leave;
            }
            InflateMode::Len => {
                // Fast path: with at least 6 input bytes and 258 output bytes
                // available, decode in bulk via the safe, `unsafe`-free
                // `fast::inflate_fast` routine.
                if io.have() >= 6 && io.left() >= 258 {
                    // RESTORE the bit accumulator so the fast path can read it.
                    state.hold = io.hold;
                    state.bits = io.bits;
                    let msg = inflate_fast(
                        &mut state,
                        io.input,
                        &mut io.next,
                        io.output,
                        &mut io.put,
                        outck,
                        &LENFIX,
                        &DISTFIX,
                    );
                    // LOAD the accumulator back out of the state.
                    io.hold = state.hold;
                    io.bits = state.bits;
                    if let Some(m) = msg {
                        strm.msg = Some(m);
                    }
                    if state.mode == InflateMode::Type {
                        state.back = -1;
                    }
                    continue 'inf_leave;
                }

                // Slow path: decode one length code, following one level of
                // sub-table indirection if the root entry is a link. The
                // immutable `lencode` borrow is confined to the block; `state`
                // mutations (e.g. `state.back`) happen only after it ends.
                state.back = 0;
                let (here, back_add): (Code, i32) = {
                    let lc = state.lencode_slice(&LENFIX);
                    let mut h = lc[io.bits_val(state.lenbits) as usize];
                    while (h.bits as u32) > io.bits {
                        if io.pull_byte().is_none() {
                            break 'inf_leave;
                        }
                        h = lc[io.bits_val(state.lenbits) as usize];
                    }
                    let mut badd = 0i32;
                    if h.op != 0 && (h.op & 0xf0) == 0 {
                        let last = h;
                        let mut idx = last.val as usize
                            + ((io.bits_val(last.bits as u32 + last.op as u32) >> last.bits)
                                as usize);
                        h = lc[idx];
                        while (last.bits as u32 + h.bits as u32) > io.bits {
                            if io.pull_byte().is_none() {
                                break 'inf_leave;
                            }
                            idx = last.val as usize
                                + ((io.bits_val(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize);
                            h = lc[idx];
                        }
                        io.drop_bits(last.bits as u32);
                        badd = last.bits as i32;
                    }
                    (h, badd)
                };
                state.back += back_add;
                io.drop_bits(here.bits as u32);
                state.back += here.bits as i32;
                state.length = here.val as u32;
                if here.op == 0 {
                    // Literal byte.
                    state.mode = InflateMode::Lit;
                    continue 'inf_leave;
                }
                if (here.op & 32) != 0 {
                    // End of block.
                    state.back = -1;
                    state.mode = InflateMode::Type;
                    continue 'inf_leave;
                }
                if (here.op & 64) != 0 {
                    strm.msg = Some("invalid literal/length code");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::LenExt;
                continue 'inf_leave;
            }
            InflateMode::LenExt => {
                if state.extra != 0 {
                    let extra = state.extra;
                    if io.need_bits(extra).is_none() {
                        break 'inf_leave;
                    }
                    state.length += io.bits_val(extra);
                    io.drop_bits(extra);
                    state.back += extra as i32;
                }
                state.was = state.length;
                state.mode = InflateMode::Dist;
                continue 'inf_leave;
            }
            InflateMode::Dist => {
                let (here, back_add): (Code, i32) = {
                    let dc = state.distcode_slice(&DISTFIX);
                    let mut h = dc[io.bits_val(state.distbits) as usize];
                    while (h.bits as u32) > io.bits {
                        if io.pull_byte().is_none() {
                            break 'inf_leave;
                        }
                        h = dc[io.bits_val(state.distbits) as usize];
                    }
                    let mut badd = 0i32;
                    if (h.op & 0xf0) == 0 {
                        let last = h;
                        let mut idx = last.val as usize
                            + ((io.bits_val(last.bits as u32 + last.op as u32) >> last.bits)
                                as usize);
                        h = dc[idx];
                        while (last.bits as u32 + h.bits as u32) > io.bits {
                            if io.pull_byte().is_none() {
                                break 'inf_leave;
                            }
                            idx = last.val as usize
                                + ((io.bits_val(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize);
                            h = dc[idx];
                        }
                        io.drop_bits(last.bits as u32);
                        badd = last.bits as i32;
                    }
                    (h, badd)
                };
                state.back += back_add;
                io.drop_bits(here.bits as u32);
                state.back += here.bits as i32;
                if (here.op & 64) != 0 {
                    strm.msg = Some("invalid distance code");
                    state.mode = InflateMode::Bad;
                    continue 'inf_leave;
                }
                state.offset = here.val as u32;
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::DistExt;
                continue 'inf_leave;
            }
            InflateMode::DistExt => {
                if state.extra != 0 {
                    let extra = state.extra;
                    if io.need_bits(extra).is_none() {
                        break 'inf_leave;
                    }
                    state.offset += io.bits_val(extra);
                    io.drop_bits(extra);
                    state.back += extra as i32;
                }
                // The `INFLATE_STRICT` `offset > dmax` guard is not enabled in
                // the default build, so it is intentionally omitted for parity.
                state.mode = InflateMode::Match;
                continue 'inf_leave;
            }
            InflateMode::Match => {
                if io.left() == 0 {
                    break 'inf_leave;
                }
                // Bytes already produced this call (C `copy = out - left`).
                let mut copy = outck - io.left();
                let from_output: bool;
                let from_base: usize;
                if state.offset as usize > copy {
                    // Distance reaches into the sliding window (earlier calls).
                    copy = state.offset as usize - copy;
                    if copy > state.whave as usize && state.sane {
                        strm.msg = Some("invalid distance too far back");
                        state.mode = InflateMode::Bad;
                        continue 'inf_leave;
                    }
                    if copy > state.wnext as usize {
                        copy -= state.wnext as usize;
                        from_base = state.wsize as usize - copy;
                    } else {
                        from_base = state.wnext as usize - copy;
                    }
                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                    from_output = false;
                } else {
                    // Distance reaches into output already produced this call.
                    from_base = io.put - state.offset as usize;
                    copy = state.length as usize;
                    from_output = true;
                }
                if copy > io.left() {
                    copy = io.left();
                }
                state.length -= copy as u32;
                // Byte-by-byte copy: the read cursor is always strictly behind
                // the write cursor, so overlapping LZ77 copies work correctly.
                for k in 0..copy {
                    let b = if from_output {
                        io.output[from_base + k]
                    } else {
                        state.window[from_base + k]
                    };
                    let p = io.put;
                    io.output[p] = b;
                    io.put += 1;
                }
                if state.length == 0 {
                    state.mode = InflateMode::Len;
                }
                continue 'inf_leave;
            }
            InflateMode::Lit => {
                if io.left() == 0 {
                    break 'inf_leave;
                }
                let p = io.put;
                io.output[p] = state.length as u8;
                io.put += 1;
                state.mode = InflateMode::Len;
                continue 'inf_leave;
            }

            // ---------------------------------------------------------------
            // Trailer verification.
            // ---------------------------------------------------------------
            InflateMode::Check => {
                if state.wrap != 0 {
                    if io.need_bits(32).is_none() {
                        break 'inf_leave;
                    }
                    let produced = outck - io.left();
                    state.total += produced as u64;
                    if (state.wrap & 4) != 0 && produced != 0 {
                        let start = io.put - produced;
                        state.check =
                            update_check(state.flags, state.check, &io.output[start..io.put]);
                    }
                    outck = io.left();
                    // Compare the stored trailer with the computed check. For a
                    // gzip stream the CRC-32 is little-endian; for zlib the
                    // Adler-32 is big-endian, hence the `zswap32`.
                    let mismatch = {
                        #[cfg(feature = "gzip")]
                        {
                            (state.wrap & 4) != 0
                                && (if state.flags != 0 {
                                    io.hold
                                } else {
                                    zswap32(io.hold)
                                }) != state.check
                        }
                        #[cfg(not(feature = "gzip"))]
                        {
                            (state.wrap & 4) != 0 && zswap32(io.hold) != state.check
                        }
                    };
                    if mismatch {
                        strm.msg = Some("incorrect data check");
                        state.mode = InflateMode::Bad;
                        continue 'inf_leave;
                    }
                    io.init_bits();
                }
                state.mode = InflateMode::Length;
                continue 'inf_leave;
            }
            InflateMode::Length => {
                // The gzip ISIZE (uncompressed length mod 2^32) check. For zlib
                // and raw streams `state.flags` is `0`, so this is a no-op that
                // simply advances to `Done` — hence the arm is not gzip-gated.
                if state.wrap != 0 && state.flags != 0 {
                    if io.need_bits(32).is_none() {
                        break 'inf_leave;
                    }
                    if (state.wrap & 4) != 0 && io.hold as u64 != (state.total & 0xffff_ffff) {
                        strm.msg = Some("incorrect length check");
                        state.mode = InflateMode::Bad;
                        continue 'inf_leave;
                    }
                    io.init_bits();
                }
                state.mode = InflateMode::Done;
                continue 'inf_leave;
            }
            InflateMode::Done => {
                ret = ReturnCode::StreamEnd;
                break 'inf_leave;
            }
            InflateMode::Bad => {
                ret = ReturnCode::DataError;
                break 'inf_leave;
            }
            InflateMode::Mem => {
                // C `case MEM: return Z_MEM_ERROR;` — an immediate return with
                // no epilogue. Unreachable in this port because window
                // allocation never fails, but retained for exhaustiveness and
                // FFI parity. The stream cursors are left unadvanced, matching
                // C skipping `RESTORE()`.
                strm.set_inflate_state(state);
                return InflateOutcome {
                    code: ReturnCode::MemError,
                    consumed: 0,
                    produced: 0,
                };
            }
            InflateMode::Sync => {
                // C `case SYNC: default: return Z_STREAM_ERROR;`.
                strm.set_inflate_state(state);
                return InflateOutcome {
                    code: ReturnCode::StreamError,
                    consumed: 0,
                    produced: 0,
                };
            }
        }
    }

    // =======================================================================
    // inf_leave: restore the accumulator, update the window, and account for
    // progress before returning (C `inflate.c` L1131-L1152).
    // =======================================================================

    // RESTORE(): write the bit accumulator back to the state.
    state.hold = io.hold;
    state.bits = io.bits;

    let produced_since_ck = outck - io.left();

    let mode_u = state.mode as u16;
    // Update the sliding window with freshly produced output when the window is
    // already in use, or while more output is still expected (and this is not a
    // short read at the very end of a `Z_FINISH` request). Mirrors the C
    // condition at L1133-L1136 (using the enum discriminants for `< BAD` /
    // `< CHECK`, since `InflateMode` is ordered like the C `mode` values).
    let needs_window_update = state.wsize != 0
        || (produced_since_ck != 0
            && mode_u < InflateMode::Bad as u16
            && (mode_u < InflateMode::Check as u16 || flush != Z_FINISH));
    // `&&` short-circuits, so `updatewindow` runs (with its window side effect)
    // exactly when the condition holds; the body runs only on an OOM failure.
    if needs_window_update
        && updatewindow(&mut state, &io.output[..], io.put, produced_since_ck).is_err()
    {
        // C `inf_leave`: `state->mode = MEM; return Z_MEM_ERROR;` — the lazy
        // window allocation (routed through the caller's `zalloc`) reported OOM.
        // Enter the permanent `MEM` error state and return `Z_MEM_ERROR` with no
        // committed progress, matching both the C control flow and the
        // `InflateMode::Mem` arm above (M7).
        state.mode = InflateMode::Mem;
        strm.set_inflate_state(state);
        return InflateOutcome {
            code: ReturnCode::MemError,
            consumed: 0,
            produced: 0,
        };
    }

    let consumed = io.next;
    let produced = io.put;

    state.total += produced_since_ck as u64;

    // Fold the output produced since the last checkpoint into the running
    // check (the pre-checkpoint bytes were folded in the CHECK arm).
    if (state.wrap & 4) != 0 && produced_since_ck != 0 {
        let start = io.put - produced_since_ck;
        state.check = update_check(state.flags, state.check, &io.output[start..io.put]);
    }

    // data_type: the exact C formula (inflate.c L1147-L1149) — low 7 bits are
    // the bit-buffer occupancy, plus flags for last-block / at-block-boundary /
    // at-start-of-block.
    strm.data_type = state.bits as i32
        + if state.last { 64 } else { 0 }
        + if state.mode == InflateMode::Type {
            128
        } else {
            0
        }
        + if state.mode == InflateMode::LenUnderscore || state.mode == InflateMode::CopyUnderscore {
            256
        } else {
            0
        };

    // Downgrade Z_OK to Z_BUF_ERROR when the call was unable to make progress
    // (no bytes in or out) or a Z_FINISH could not complete.
    if ((consumed == 0 && produced == 0) || flush == Z_FINISH) && ret == ReturnCode::Ok {
        ret = ReturnCode::BufError;
    }

    // Commit progress counters and the running check mirror, then reinstall the
    // state so subsequent calls resume where this one left off.
    strm.total_in += consumed as u64;
    strm.total_out += produced as u64;
    strm.adler = state.check;
    strm.set_inflate_state(state);

    InflateOutcome {
        code: ret,
        consumed,
        produced,
    }
}

// ===========================================================================
// Dictionary / header / sync / copy / diagnostics API.
// ===========================================================================

/// Returns the sliding-window history as a preset dictionary — the Rust port of
/// C `inflateGetDictionary` (`inflate.c` L1167-L1185).
///
/// Up to [`InflateState::whave`] bytes of decoded history are copied into
/// `dictionary` in chronological order (oldest first), and the true length is
/// written to `dict_length`. Pass an empty (or too-small) `dictionary` slice to
/// query only the length — the copy is skipped when `dictionary` cannot hold the
/// full history, mirroring how C callers pass `Z_NULL` to size their buffer.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state.
pub fn inflate_get_dictionary<A: Allocator>(
    strm: &ZStream<A>,
    dictionary: &mut [u8],
    dict_length: &mut usize,
) -> InflateResult {
    let state = strm.inflate_state().ok_or(ZlibError::StreamError)?;
    let whave = state.whave as usize;
    let wnext = state.wnext as usize;
    if whave != 0 && dictionary.len() >= whave {
        // Older bytes first (`window[wnext..whave]`), then the wrapped-around
        // newer bytes (`window[..wnext]`).
        let head_len = whave - wnext;
        dictionary[..head_len].copy_from_slice(&state.window[wnext..whave]);
        dictionary[head_len..whave].copy_from_slice(&state.window[..wnext]);
    }
    *dict_length = whave;
    Ok(ReturnCode::Ok)
}

/// Installs a preset dictionary into the sliding window — the Rust port of C
/// `inflateSetDictionary` (`inflate.c` L1187-L1217).
///
/// Valid only for a raw stream (before any data) or immediately after
/// [`inflate`] returned [`ReturnCode::NeedDict`] (state [`InflateMode::Dict`]).
/// In the latter case the dictionary's Adler-32 must match the id the stream
/// requested, or [`ZlibError::DataError`] is returned.
///
/// # Errors
/// * [`ZlibError::StreamError`] — no inflate state, or a wrapped stream not
///   awaiting a dictionary.
/// * [`ZlibError::DataError`] — the dictionary's Adler-32 id does not match.
pub fn inflate_set_dictionary<A: Allocator>(
    strm: &mut ZStream<A>,
    dictionary: &[u8],
) -> InflateResult {
    let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
    // A dictionary is only accepted for a raw stream, or when explicitly needed.
    if state.wrap != 0 && state.mode != InflateMode::Dict {
        return Err(ZlibError::StreamError);
    }
    // Verify the dictionary identifier when one was requested.
    if state.mode == InflateMode::Dict {
        let dictid = adler32(ADLER32_INIT, dictionary);
        if dictid != state.check {
            return Err(ZlibError::DataError);
        }
    }
    // Load the dictionary into the window (amending existing history). C treats
    // `updatewindow` failure as `Z_MEM_ERROR` (setting `mode = MEM`); reproduce
    // that when the window allocation is routed through a caller hook that
    // reports OOM (M7).
    let dict_len = dictionary.len();
    if updatewindow(state, dictionary, dict_len, dict_len).is_err() {
        state.mode = InflateMode::Mem;
        return Err(ZlibError::MemError);
    }
    state.havedict = true;
    Ok(ReturnCode::Ok)
}

/// Registers a [`GzHeader`] to receive the gzip header fields parsed by
/// [`inflate`] — the Rust port of C `inflateGetHeader` (`inflate.c`
/// L1219-L1230). Only meaningful for a gzip-capable stream (`wrap & 2`).
///
/// Ownership of `head` is transferred to the decoder: its `extra`/`name`/
/// `comment` capacities and buffers (if `Some`) bound what is captured while the
/// gzip header modes run, and `done` is cleared. The higher-level FFI shim in
/// `src/ffi/inflate.rs` bridges this owned model back to C's borrowed
/// `gz_headerp`.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state or the
/// stream is not gzip-capable.
#[cfg(feature = "gzip")]
pub fn inflate_get_header<A: Allocator>(strm: &mut ZStream<A>, head: GzHeader) -> InflateResult {
    let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
    if (state.wrap & 2) == 0 {
        return Err(ZlibError::StreamError);
    }
    let mut head = head;
    head.done = false;
    state.head = Some(head);
    Ok(ReturnCode::Ok)
}

/// Scans `input` for the DEFLATE flush marker (`00 00 FF FF`) and, when found,
/// restarts decoding at the following block — the Rust port of C `inflateSync`
/// (`inflate.c` L1264-L1310), used for error recovery.
///
/// Returns the [`ReturnCode`] together with the number of `input` bytes
/// consumed (the FFI shim advances `next_in`/`avail_in` by that amount). The
/// error-recovery behaviour is byte-for-byte identical to reference zlib: it
/// first drains any bits still held in the accumulator, then searches the
/// supplied input, and on a full match resets to [`InflateMode::Type`] while
/// preserving the running byte totals.
///
/// Possible codes: [`ReturnCode::Ok`] (marker found, decoding can resume),
/// [`ReturnCode::BufError`] (no input and fewer than 8 held bits — call again
/// with more input), [`ReturnCode::DataError`] (input exhausted without a full
/// marker), or [`ReturnCode::StreamError`] (no inflate state).
pub fn inflate_sync<A: Allocator>(strm: &mut ZStream<A>, input: &[u8]) -> (ReturnCode, usize) {
    // check parameters
    let state_bits = match strm.inflate_state() {
        Some(s) => s.bits,
        None => return (ReturnCode::StreamError, 0),
    };
    if input.is_empty() && state_bits < 8 {
        return (ReturnCode::BufError, 0);
    }

    // If this is the first sync call, prime the search from the bit buffer.
    {
        let state = strm
            .inflate_state_mut()
            .expect("inflate state present (checked above)");
        if state.mode != InflateMode::Sync {
            state.mode = InflateMode::Sync;
            let rem = state.bits & 7;
            state.hold >>= rem;
            state.bits -= rem;
            let mut buf = [0u8; 4];
            let mut len = 0usize;
            while state.bits >= 8 {
                buf[len] = state.hold as u8;
                len += 1;
                state.hold >>= 8;
                state.bits -= 8;
            }
            state.have = 0;
            let mut have = state.have;
            syncsearch(&mut have, &buf[..len]);
            state.have = have;
        }
    }

    // Search the supplied input.
    let consumed = {
        let state = strm
            .inflate_state_mut()
            .expect("inflate state present (checked above)");
        let mut have = state.have;
        let n = syncsearch(&mut have, input);
        state.have = have;
        n
    };
    strm.total_in += consumed as u64;

    // No full marker yet.
    if strm
        .inflate_state()
        .expect("inflate state present (checked above)")
        .have
        != 4
    {
        return (ReturnCode::DataError, consumed);
    }

    // Marker found: reset to resume decoding on the next block, preserving the
    // running totals and header status.
    let flags = strm
        .inflate_state()
        .expect("inflate state present (checked above)")
        .flags;
    {
        let state = strm
            .inflate_state_mut()
            .expect("inflate state present (checked above)");
        if state.flags == -1 {
            state.wrap = 0; // no header seen yet: treat as raw
        } else {
            state.wrap &= !4; // computing a check value now is pointless
        }
    }
    let in_total = strm.total_in;
    let out_total = strm.total_out;
    // Cannot fail: the inflate state is present.
    let _ = inflate_reset(strm);
    strm.total_in = in_total;
    strm.total_out = out_total;
    {
        let state = strm
            .inflate_state_mut()
            .expect("inflate state present (checked above)");
        state.flags = flags;
        state.mode = InflateMode::Type;
    }
    (ReturnCode::Ok, consumed)
}

/// Reports whether the decoder is paused exactly at a stored-block boundary —
/// the Rust port of C `inflateSyncPoint` (`inflate.c` L1320-L1327).
///
/// Used by protocols such as PPP that emit `Z_SYNC_FLUSH`/`Z_FULL_FLUSH` empty
/// stored blocks and then verify the decoder is waiting for the length bytes.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state; otherwise
/// `Ok(true)` at a sync point and `Ok(false)` elsewhere.
pub fn inflate_sync_point<A: Allocator>(strm: &ZStream<A>) -> Result<bool, ZlibError> {
    let state = strm.inflate_state().ok_or(ZlibError::StreamError)?;
    Ok(state.mode == InflateMode::Stored && state.bits == 0)
}

/// Deep-clones an [`InflateState`] into a fresh [`Box`].
///
/// This is the safe-Rust replacement for the pointer fix-up dance in C
/// `inflateCopy`: because the state records the active decode tables as `usize`
/// offsets into [`InflateState::codes`] plus a fixed/dynamic discriminator
/// ([`TableSource`]) — rather than self-referential raw pointers — copying the
/// fields verbatim already yields a correct, independent clone. No offsets need
/// to be re-based. (`InflateState` deliberately does not derive [`Clone`], so
/// the copy is written out explicitly.)
fn clone_inflate_state(s: &InflateState) -> Box<InflateState> {
    Box::new(InflateState {
        mode: s.mode,
        last: s.last,
        wrap: s.wrap,
        havedict: s.havedict,
        flags: s.flags,
        dmax: s.dmax,
        check: s.check,
        total: s.total,
        head: s.head.clone(),
        wbits: s.wbits,
        wsize: s.wsize,
        whave: s.whave,
        wnext: s.wnext,
        window: s.window.clone(),
        hold: s.hold,
        bits: s.bits,
        length: s.length,
        offset: s.offset,
        extra: s.extra,
        lencode: s.lencode,
        distcode: s.distcode,
        lentable: s.lentable,
        disttable: s.disttable,
        lenbits: s.lenbits,
        distbits: s.distbits,
        ncode: s.ncode,
        nlen: s.nlen,
        ndist: s.ndist,
        have: s.have,
        next: s.next,
        lens: s.lens,
        work: s.work,
        codes: s.codes,
        sane: s.sane,
        back: s.back,
        was: s.was,
        // Carry the caller's allocator hook into the clone so the copied
        // window (already re-allocated through the same hook by
        // `AllocBuffer::clone`) and any future re-allocation stay routed
        // through the caller's `zalloc`/`zfree` (AAP §0.6.3; QA FINDING-3).
        alloc_hook: s.alloc_hook,
        // Give the copy its own state reservation, re-allocated through the same
        // hook — mirroring C `inflateCopy`, which `ZALLOC`s a fresh state for the
        // destination. It is empty when the source has none (global-allocator
        // streams), so no-hook copies stay allocation-free. `AllocBuffer::clone`
        // is infallible: it degrades to a sound global copy only if the hook
        // reports OOM, which keeps this `deflateCopy`-style clone total.
        state_alloc: s.state_alloc.clone(),
    })
}

/// Copies a complete inflate stream — the Rust port of C `inflateCopy`
/// (`inflate.c` L1328-L1367).
///
/// The `source` stream's bookkeeping fields and a deep clone of its decode state
/// (including the sliding window and any gzip header) are installed into `dest`,
/// so the two streams thereafter decode independently. This is typically used to
/// snapshot a decoder mid-stream (e.g. for random access).
///
/// Unlike C — which must re-base the `lencode`/`distcode`/`next` pointers into
/// the copied `codes` array — no fix-up is required here: those are `usize`
/// offsets, so `clone_inflate_state` produces a correct clone directly.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `source` has no inflate state.
pub fn inflate_copy<A: Allocator>(dest: &mut ZStream<A>, source: &ZStream<A>) -> InflateResult {
    let state = source.inflate_state().ok_or(ZlibError::StreamError)?;
    let copy = clone_inflate_state(state);
    // Mirror C `zmemcpy(dest, source, sizeof(z_stream))` for the observable
    // stream bookkeeping (the allocator and I/O cursors are the caller's).
    dest.total_in = source.total_in;
    dest.total_out = source.total_out;
    dest.adler = source.adler;
    dest.data_type = source.data_type;
    dest.msg = source.msg;
    dest.set_inflate_state(copy);
    Ok(ReturnCode::Ok)
}

/// Toggles acceptance of invalid distances-too-far-back — the Rust port of C
/// `inflateUndermine` (`inflate.c` L1370-L1383).
///
/// The `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` build option is not enabled
/// (matching reference zlib defaults), so this always forces `sane` back on and
/// reports [`ZlibError::DataError`] to signal the feature is unavailable.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state, otherwise
/// always [`ZlibError::DataError`].
pub fn inflate_undermine<A: Allocator>(strm: &mut ZStream<A>, _subvert: i32) -> InflateResult {
    let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
    state.sane = true;
    Err(ZlibError::DataError)
}

/// Enables or disables trailer-checksum validation — the Rust port of C
/// `inflateValidate` (`inflate.c` L1385-L1395).
///
/// When `check` is `true` and the stream has a wrapper, the validate bit
/// (`wrap & 4`) is set so [`inflate`] verifies the Adler-32/CRC-32 trailer;
/// otherwise validation is turned off.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state.
pub fn inflate_validate<A: Allocator>(strm: &mut ZStream<A>, check: bool) -> InflateResult {
    let state = strm.inflate_state_mut().ok_or(ZlibError::StreamError)?;
    if check && state.wrap != 0 {
        state.wrap |= 4;
    } else {
        state.wrap &= !4;
    }
    Ok(ReturnCode::Ok)
}

/// Returns a diagnostic "mark" of decode progress — the Rust port of C
/// `inflateMark` (`inflate.c` L1397-L1406).
///
/// The result packs the number of bits back into the input at the current
/// literal/length code (high bits) and the number of bytes still to copy for an
/// in-progress match (low 16 bits). A value of `-(1 << 16)` indicates an invalid
/// stream state. Applications use this to locate the last full flush point.
#[must_use]
pub fn inflate_mark<A: Allocator>(strm: &ZStream<A>) -> i64 {
    let state = match strm.inflate_state() {
        Some(s) => s,
        None => return -(1i64 << 16),
    };
    let offset = match state.mode {
        InflateMode::Copy => state.length as i64,
        InflateMode::Match => state.was as i64 - state.length as i64,
        _ => 0,
    };
    ((state.back as i64) << 16) + offset
}

/// Returns the number of decode-table entries used so far — the Rust port of C
/// `inflateCodesUsed` (`inflate.c` L1408-L1414).
///
/// This equals [`InflateState::next`] (the C `state->next - state->codes`, since
/// `next` is already an index into [`InflateState::codes`]). Returns [`None`]
/// for an invalid stream state (the C `(unsigned long)-1` sentinel).
#[must_use]
pub fn inflate_codes_used<A: Allocator>(strm: &ZStream<A>) -> Option<usize> {
    strm.inflate_state().map(|s| s.next)
}

/// Releases the inflate state — the Rust port of C `inflateEnd` (`inflate.c`
/// L1155-L1165).
///
/// Dropping the boxed [`InflateState`] automatically frees the sliding window
/// (RAII subsumes the C `ZFREE(window)`), so this simply detaches the state from
/// `strm`.
///
/// # Errors
/// Returns [`ZlibError::StreamError`] if `strm` has no inflate state.
pub fn inflate_end<A: Allocator>(strm: &mut ZStream<A>) -> InflateResult {
    if !strm.is_inflate() {
        return Err(ZlibError::StreamError);
    }
    strm.clear_state();
    Ok(ReturnCode::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::Z_NO_FLUSH;
    use alloc::vec::Vec;

    /// The uncompressed reference message. It repeats "hello, " so the decoder
    /// exercises the length/distance/match copy path (back-references into the
    /// window), not just literal output.
    const MSG: &[u8] = b"hello, hello, hello, world!";

    /// `MSG` compressed as a zlib stream (RFC 1950), level 6.
    const ZLIB_STREAM: &[u8] = &[
        120, 156, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139, 114, 82, 20, 1,
        133, 250, 9, 106,
    ];

    /// `MSG` compressed as raw DEFLATE (RFC 1951), level 6, `windowBits = -15`.
    const RAW_STREAM: &[u8] = &[
        203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243, 139, 114, 82, 20, 1,
    ];

    /// `MSG` compressed as a gzip stream (RFC 1952), level 6,
    /// `windowBits = 16 + 15`.
    #[cfg(feature = "gzip")]
    const GZIP_STREAM: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 203, 72, 205, 201, 201, 215, 81, 200, 64, 161, 202, 243,
        139, 114, 82, 20, 1, 131, 137, 31, 110, 27, 0, 0, 0,
    ];

    /// Fully decodes `input` (given `window_bits`) in a single call with a
    /// generous output buffer, asserting the message round-trips exactly and the
    /// stream ends cleanly.
    fn decode_ok(input: &[u8], window_bits: i32) {
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, window_bits), Ok(ReturnCode::Ok));
        let mut out = alloc::vec![0u8; 256];
        let outcome = inflate(&mut strm, input, &mut out, Z_NO_FLUSH);
        assert_eq!(
            outcome.code,
            ReturnCode::StreamEnd,
            "expected Z_STREAM_END, got {:?} (msg {:?})",
            outcome.code,
            strm.msg
        );
        assert_eq!(outcome.consumed, input.len(), "all input consumed");
        assert_eq!(&out[..outcome.produced], MSG, "decoded bytes match input");
        assert_eq!(inflate_end(&mut strm), Ok(ReturnCode::Ok));
    }

    #[test]
    fn round_trip_zlib() {
        decode_ok(ZLIB_STREAM, 15);
    }

    #[test]
    fn round_trip_raw() {
        decode_ok(RAW_STREAM, -15);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn round_trip_gzip() {
        decode_ok(GZIP_STREAM, 16 + 15);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn round_trip_auto_detect() {
        // windowBits 32 + 15 auto-detects a zlib *or* gzip wrapper.
        decode_ok(ZLIB_STREAM, 32 + 15);
        decode_ok(GZIP_STREAM, 32 + 15);
    }

    #[test]
    fn decode_with_tight_output_buffer() {
        // A 4-byte output buffer forces many `Z_OK` returns, exercising the
        // resumable window-copy / continuation paths across calls.
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, 15), Ok(ReturnCode::Ok));
        let mut decoded = Vec::new();
        let mut in_pos = 0usize;
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 10_000, "decode did not terminate");
            let mut out = [0u8; 4];
            let outcome = inflate(&mut strm, &ZLIB_STREAM[in_pos..], &mut out, Z_NO_FLUSH);
            in_pos += outcome.consumed;
            decoded.extend_from_slice(&out[..outcome.produced]);
            match outcome.code {
                ReturnCode::StreamEnd => break,
                ReturnCode::Ok | ReturnCode::BufError => {
                    assert!(
                        outcome.consumed != 0 || outcome.produced != 0,
                        "stalled with code {:?}",
                        outcome.code
                    );
                }
                other => panic!("unexpected code {other:?}"),
            }
        }
        assert_eq!(decoded.as_slice(), MSG);
    }

    #[test]
    fn inflate_without_state_is_stream_error() {
        let mut strm = ZStream::new();
        let mut out = [0u8; 16];
        let outcome = inflate(&mut strm, &[], &mut out, Z_NO_FLUSH);
        assert_eq!(outcome.code, ReturnCode::StreamError);
    }

    #[test]
    fn corrupt_body_is_data_error() {
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, 15), Ok(ReturnCode::Ok));
        // Valid zlib header (0x78 0x9c) followed by an invalid block type run.
        let bad = [0x78u8, 0x9c, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let mut out = [0u8; 64];
        let outcome = inflate(&mut strm, &bad, &mut out, Z_NO_FLUSH);
        assert_eq!(outcome.code, ReturnCode::DataError);
        assert!(strm.msg.is_some(), "a diagnostic message must be set");
    }

    #[test]
    fn bad_header_check_is_data_error() {
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, 15), Ok(ReturnCode::Ok));
        // CMF = 0x78 is a valid deflate method (CM = 8) with a 32 KiB window,
        // but FLG = 0x00 makes the 16-bit header value 0x7800, and
        // 0x7800 % 31 == 30 (non-zero), so the zlib header checksum fails.
        // Reference zlib reports "incorrect header check" for exactly this
        // input (verified against the C library). Contrast with 0x0000, which
        // *passes* the %31 check and instead trips the compression-method test.
        let bad = [0x78u8, 0x00];
        let mut out = [0u8; 16];
        let outcome = inflate(&mut strm, &bad, &mut out, Z_NO_FLUSH);
        assert_eq!(outcome.code, ReturnCode::DataError);
        assert_eq!(strm.msg, Some("incorrect header check"));
    }

    #[test]
    fn invalid_window_bits_rejected() {
        let mut strm = ZStream::new();
        // 7 is below the minimum window size of 8.
        assert_eq!(inflate_init2(&mut strm, 7), Err(ZlibError::StreamError));
    }

    #[test]
    fn reset_reinitialises_for_a_fresh_member() {
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, 15), Ok(ReturnCode::Ok));
        let mut out = alloc::vec![0u8; 256];
        let first = inflate(&mut strm, ZLIB_STREAM, &mut out, Z_NO_FLUSH);
        assert_eq!(first.code, ReturnCode::StreamEnd);
        // After a reset the same decoder decodes the member again from scratch.
        assert_eq!(inflate_reset(&mut strm), Ok(ReturnCode::Ok));
        let second = inflate(&mut strm, ZLIB_STREAM, &mut out, Z_NO_FLUSH);
        assert_eq!(second.code, ReturnCode::StreamEnd);
        assert_eq!(&out[..second.produced], MSG);
    }

    #[test]
    fn mark_is_sentinel_without_state() {
        let strm = ZStream::new();
        assert_eq!(inflate_mark(&strm), -(1i64 << 16));
    }

    #[test]
    fn codes_used_is_none_without_state() {
        let strm = ZStream::new();
        assert_eq!(inflate_codes_used(&strm), None);
    }

    #[test]
    fn prime_and_flush_bits() {
        let mut strm = ZStream::new();
        assert_eq!(inflate_init2(&mut strm, 15), Ok(ReturnCode::Ok));
        // Inject 5 bits, then confirm a negative count flushes them back out.
        assert_eq!(inflate_prime(&mut strm, 5, 0b1_0101), Ok(ReturnCode::Ok));
        assert_eq!(inflate_prime(&mut strm, -1, 0), Ok(ReturnCode::Ok));
        // Over-wide bit counts are rejected.
        assert_eq!(inflate_prime(&mut strm, 17, 0), Err(ZlibError::StreamError));
    }

    #[test]
    fn copy_produces_independent_streams() {
        let mut src = ZStream::new();
        assert_eq!(inflate_init2(&mut src, 15), Ok(ReturnCode::Ok));
        // Partially decode into a tight buffer so the source holds live state.
        let mut out = [0u8; 4];
        let _ = inflate(&mut src, ZLIB_STREAM, &mut out, Z_NO_FLUSH);

        let mut dest = ZStream::new();
        assert_eq!(inflate_copy(&mut dest, &src), Ok(ReturnCode::Ok));
        assert!(dest.is_inflate(), "destination received a cloned state");
        assert_eq!(dest.total_out, src.total_out);
    }

    #[test]
    fn set_dictionary_rejects_bad_state() {
        // No inflate state installed → Z_STREAM_ERROR.
        let mut strm = ZStream::new();
        assert_eq!(
            inflate_set_dictionary(&mut strm, b"dict"),
            Err(ZlibError::StreamError)
        );
    }
}
