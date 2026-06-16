//! `inflate.c` — zlib decompression (RFC 1950 / RFC 1951 / RFC 1952).
//!
//! Safe-Rust port of `inflate.c` (Copyright (C) 1995-2026 Mark Adler; for
//! conditions of distribution and use, see the copyright notice in `zlib.h`).
//!
//! This is the **root module of the INFLATE decompression engine**. It owns the
//! giant `for(;;) switch(state->mode)` decode loop — re-expressed here as a
//! single labeled `'inf: loop { match state.mode { … } }` — together with the
//! bit-accumulator helpers, the deferred sliding-window machinery, the
//! gzip/zlib/raw header parsers, the preset-dictionary (`Z_NEED_DICT`)
//! handshake, `inflateSync` recovery, `inflateCopy`, and the full set of public
//! `inflate*` API functions. It also declares the inflate submodules and
//! re-exports the public surface consumed by `lib.rs`, `ffi.rs`, and the tests.
//!
//! # Design (AAP §0.6.1–§0.6.7)
//!
//! * **Pointers → indices.** C's `lencode`/`distcode`/`next` are `code*`
//!   pointers into `state->codes[]`; here they are [`usize`] indices into
//!   [`InflateState::codes`]. Every table read is `state.codes[base + offset]`.
//! * **`hold: u32`.** The bit accumulator matches C's portable `unsigned long`
//!   path (two-byte refills once fewer than 15 bits remain) so that block
//!   boundaries — and therefore the byte stream — are identical to canonical
//!   zlib.
//! * **Checksum init via literals.** `state.check` is seeded with `1`
//!   (Adler-32) or `0` (CRC-32) directly; the [`crate::checksum`] functions are
//!   only ever called with non-empty buffers.
//! * **Zero `unsafe`.** Every copy here uses safe slice operations or
//!   index loops; the sole core `unsafe` site is `fast::inflate_fast`.
//! * **State machine.** C's integer `mode` + `switch` fall-through becomes the
//!   exhaustive [`InflateMode`] enum + `match`; `goto inf_leave` becomes
//!   `break 'inf`; intentional C fall-through becomes `state.mode = Next;
//!   continue 'inf;`.

// --- Submodules (declared in dependency order) -----------------------------
pub mod back;
pub mod fast;
pub mod fixed;
pub mod state;
pub mod tables;

// --- External / sibling imports --------------------------------------------
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use crate::checksum::adler32;
// `crc32` backs the gzip-stream check value (CRC-32); the zlib path uses
// Adler-32. Every `crc32` call site in this module is inside a
// `#[cfg(feature = "gzip")]` block, so gate the import to match and keep the
// `-D warnings` build clean when the `gzip` feature is disabled.
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::*;
use crate::error::ZlibError;
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// --- Public re-exports (the inflate engine's surface) ----------------------
pub use back::{
    BackInput, BackOutput, SliceInput, inflate_back, inflate_back_end, inflate_back_init,
};
pub use state::{InflateMode, InflateState};
pub use tables::{Code, CodeType, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS, MAXBITS};

// Crate-internal helpers from the table builder (these are `pub(crate)` in
// `tables`, so they are `use`d here rather than re-exported).
use tables::{inflate_fixed, inflate_table};

// ===========================================================================
// Per-call result contract
// ===========================================================================

/// Everything a caller (`ffi.rs` shim or the idiomatic [`Inflate`] wrapper)
/// needs to reconcile its `z_stream`/state after one `inflate` call.
///
/// The core engine is lifetime-free and slice-based (it does not own a
/// `ZStream`), so instead of mutating a stream struct it returns this outcome.
/// `in_consumed`/`out_produced` advance `next_in`/`next_out`;
/// `total_in_delta`/`total_out_delta` advance the running totals; `ret` is the
/// C return code (`Z_OK` … `Z_STREAM_END` … `Z_DATA_ERROR`); `data_type`,
/// `msg`, and `adler` mirror `strm->data_type`, `strm->msg`, and `strm->adler`.
#[derive(Debug, Clone, Copy)]
pub struct InflateOutcome {
    /// Bytes consumed from `input` this call (advance `next_in`).
    pub in_consumed: usize,
    /// Bytes written to `output` this call (advance `next_out`).
    pub out_produced: usize,
    /// C return code: `Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT`, `Z_BUF_ERROR`,
    /// `Z_DATA_ERROR`, `Z_MEM_ERROR`, or `Z_STREAM_ERROR`.
    pub ret: i32,
    /// Mirror of `strm->data_type` (bit count + last-block/type flags).
    pub data_type: i32,
    /// Mirror of `strm->msg` (a static error string, or `None`).
    pub msg: Option<&'static str>,
    /// Mirror of `strm->adler` (the running/!final Adler-32 or CRC-32).
    pub adler: u32,
    /// Amount to add to `total_in`.
    pub total_in_delta: u64,
    /// Amount to add to `total_out`.
    pub total_out_delta: u64,
}

// ===========================================================================
// Phase B — bit-accumulator + checksum pure helpers
// ===========================================================================

/// `BITS(n)` — the low `n` bits of the accumulator (C `hold & ((1U << n) - 1)`).
///
/// `n` is always `< 16` at every call site (root/sub-table widths and the small
/// extra-bit counts), so `1u32 << n` never overflows.
#[inline]
const fn bits_low(hold: u32, n: u32) -> u32 {
    hold & ((1u32 << n) - 1)
}

/// `UPDATE_CHECK` — fold `buf` into the running check value.
///
/// Mirrors the C `#ifdef GUNZIP` macro: a gzip stream (`flags != 0`) uses
/// CRC-32, a zlib stream uses Adler-32. With the `gzip` feature disabled only
/// the zlib (Adler) path exists. Must only be called with a **non-empty**
/// `buf` (an empty slice would return `check` unchanged rather than the C init
/// constant — see the module docs).
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

/// Computes `strm->data_type` exactly as C does at `inf_leave`
/// (`inflate.c`): the live bit count plus flags for last-block, the `TYPE`
/// state, and the `LEN_`/`COPY_` block-boundary states.
#[inline]
fn make_data_type(mode: InflateMode, last: bool, bits: u32) -> i32 {
    bits as i32
        + if last { 64 } else { 0 }
        + if mode == InflateMode::Type { 128 } else { 0 }
        + if mode == InflateMode::LenBegin || mode == InflateMode::CopyBegin {
            256
        } else {
            0
        }
}

// ===========================================================================
// Phase C — inflateStateCheck + reset / init / prime family
// ===========================================================================

/// Equivalent of C `inflateStateCheck` (`inflate.c`).
///
/// In C this guards against a null stream, a null/foreign state, missing
/// `zalloc`/`zfree`, or a `mode` outside `HEAD..=SYNC`. The idiomatic core
/// always operates on a valid, typed [`InflateState`] whose `mode` is a
/// well-formed [`InflateMode`], so the structural checks are vacuously
/// satisfied and this returns `false` ("not bad"). The raw-pointer/allocator
/// validation lives in `ffi.rs`. Returning a value keeps every API entry point
/// structurally parallel to its C counterpart.
// The sole caller of `state_check` is the raw-pointer/allocator validation in
// the FFI shim (`src/ffi.rs`), which is materialized in a later checkpoint.
// Permit the forward-declared entry point without tripping the `-D warnings`
// dead-code gate at this checkpoint.
#[allow(dead_code)]
#[inline]
pub(crate) fn state_check(_state: &InflateState) -> bool {
    false
}

/// C `inflateResetKeep` — reset decode position/header bookkeeping while
/// preserving the allocated window and the `wbits`/`wrap`/`wsize` configuration.
///
/// The state-local fields are reset by [`InflateState::reset_keep`]; the
/// stream-level `adler` is reset here (to `wrap & 1`, "to support an
/// ill-conceived Java test suite"), and the caller resets `total_in`,
/// `total_out`, and `msg`.
pub(crate) fn inflate_reset_keep(state: &mut InflateState, adler: &mut u32) {
    state.reset_keep();
    if state.wrap != 0 {
        *adler = (state.wrap & 1) as u32;
    }
}

/// C `inflateReset` — additionally clears the window occupancy
/// (`wsize`/`whave`/`wnext`) before delegating to [`inflate_reset_keep`].
pub(crate) fn inflate_reset(state: &mut InflateState, adler: &mut u32) {
    state.wsize = 0;
    state.whave = 0;
    state.wnext = 0;
    inflate_reset_keep(state, adler);
}

/// C `inflateReset2` — apply the `windowBits` overloading convention
/// (AAP §0.6.7) and (re)configure `wrap`/`wbits`, freeing the window if its
/// size changed.
///
/// `windowBits` selects the framing and check expectations encoded in `wrap`
/// (bit 0 = zlib Adler check, bit 1 = gzip, bit 2 = validate check):
///
/// * `-15..=-8` — raw DEFLATE (`wrap = 0`);
/// * `8..=15` — zlib (`wrap` gains bit 0);
/// * `24..=31` — gzip (`wrap` gains bit 1; the low nibble is the window size);
/// * `40..=47` — automatic zlib/gzip detection (`wrap` gains bits 0 and 1).
///
/// Returns `Z_OK`, or `Z_STREAM_ERROR` for an out-of-range request.
pub(crate) fn inflate_reset2(
    state: &mut InflateState,
    mut window_bits: i32,
    adler: &mut u32,
) -> i32 {
    // Extract the wrap-mode selector from the sign/magnitude of windowBits.
    let wrap;
    if window_bits < 0 {
        if window_bits < -15 {
            return Z_STREAM_ERROR;
        }
        wrap = 0;
        window_bits = -window_bits;
    } else {
        wrap = (window_bits >> 4) + 5;
        // Mask the gzip/auto-detect high bits down to the real window size.
        // Gated on `gzip`: without it, requests of 24..=47 fall through to the
        // range check below and are rejected (matching C's `#ifdef GUNZIP`).
        #[cfg(feature = "gzip")]
        {
            if window_bits < 48 {
                window_bits &= 15;
            }
        }
    }

    // Validate the resolved window size.
    if window_bits != 0 && !(8..=15).contains(&window_bits) {
        return Z_STREAM_ERROR;
    }

    // Free a previously-allocated window if the size is changing.
    if !state.window.is_empty() && state.wbits != window_bits as u32 {
        state.window = Vec::new();
    }

    state.wrap = wrap;
    state.wbits = window_bits as u32;
    inflate_reset(state, adler);
    Z_OK
}

/// C `inflateInit2_` core — allocate and initialize a fresh [`InflateState`]
/// for the requested `window_bits`.
///
/// Returns the boxed state on success, or the `Z_STREAM_ERROR` code if
/// `window_bits` is invalid. (The C `version`/`stream_size` ABI checks live in
/// `ffi.rs`'s `inflateInit2_`.)
pub(crate) fn inflate_init2(window_bits: i32) -> Result<Box<InflateState>, i32> {
    let mut state = Box::new(InflateState::new());
    state.mode = InflateMode::Head; // pass the state test in inflate_reset2
    let mut adler = 0u32;
    let ret = inflate_reset2(&mut state, window_bits, &mut adler);
    if ret != Z_OK {
        return Err(ret);
    }
    Ok(state)
}

/// C `inflateInit_` core — initialize with the default window size
/// ([`DEF_WBITS`]).
///
/// Only the FFI `inflateInit_` shim (`src/ffi.rs`, a later checkpoint) calls
/// this default-window convenience; the idiomatic [`Inflate`] wrapper calls
/// [`inflate_init2`] directly. Permit the forward-declared definition here.
#[allow(dead_code)]
pub(crate) fn inflate_init() -> Result<Box<InflateState>, i32> {
    inflate_init2(DEF_WBITS)
}

/// C `inflatePrime` — insert `bits_n` bits (low bits of `value`) ahead of the
/// stream, or (for `bits_n < 0`) flush the accumulator.
///
/// Returns `Z_OK`, or `Z_STREAM_ERROR` if `bits_n > 16` or the accumulator
/// would exceed 32 bits.
pub(crate) fn inflate_prime(state: &mut InflateState, bits_n: i32, value: i32) -> i32 {
    if bits_n < 0 {
        state.hold = 0;
        state.bits = 0;
        return Z_OK;
    }
    if bits_n > 16 || state.bits + bits_n as u32 > 32 {
        return Z_STREAM_ERROR;
    }
    let masked = (value & ((1i32 << bits_n) - 1)) as u32;
    // `checked_shl` guards the (only) edge case where bits == 32 and bits_n == 0
    // (masked == 0 there); for bits_n > 0 the shift is always < 32.
    state.hold = state
        .hold
        .wrapping_add(masked.checked_shl(state.bits).unwrap_or(0));
    state.bits += bits_n as u32;
    Z_OK
}

// ===========================================================================
// Phase D — updatewindow (deferred sliding window)
// ===========================================================================

/// C `updatewindow` — fold the most recent `copy` output bytes (ending at index
/// `end` in `output`) into the circular sliding window, allocating it lazily on
/// first use.
///
/// `output[..end]` is the just-written output region. All copies are safe
/// `copy_from_slice` calls over **disjoint** regions (the window versus the
/// output buffer). Returns `0` on success. (C returns `1` on `ZALLOC` failure;
/// a Rust `Vec` allocation either succeeds or aborts, so that branch is
/// unreachable — out-of-memory never silently corrupts state.)
fn update_window(state: &mut InflateState, output: &[u8], end: usize, mut copy: usize) -> i32 {
    // Allocate the window on first use (matches the C `ZALLOC` lazy alloc).
    // The fully-qualified `alloc::vec!` keeps the source identical under `std`
    // and `no-std` (the bare `vec!` is not in the `core` prelude, and this
    // module imports only the `Vec` type from `alloc`) while using the fast
    // zero-initialization path.
    if state.window.is_empty() {
        let size = 1usize << state.wbits;
        state.window = alloc::vec![0u8; size];
    }

    // Initialize the window occupancy the first time it is used.
    if state.wsize == 0 {
        state.wsize = 1u32 << state.wbits;
        state.wnext = 0;
        state.whave = 0;
    }

    let wsize = state.wsize as usize;
    if copy >= wsize {
        // The new data alone fills (or overfills) the window: keep its tail.
        state.window[..wsize].copy_from_slice(&output[end - wsize..end]);
        state.wnext = 0;
        state.whave = state.wsize;
    } else {
        let wnext = state.wnext as usize;
        let mut dist = wsize - wnext;
        if dist > copy {
            dist = copy;
        }
        // First span: from the write cursor to the end of the window buffer.
        state.window[wnext..wnext + dist].copy_from_slice(&output[end - copy..end - copy + dist]);
        copy -= dist;
        if copy != 0 {
            // Second span wraps to the front of the window buffer.
            state.window[..copy].copy_from_slice(&output[end - copy..end]);
            state.wnext = copy as u32;
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
    0
}

// ===========================================================================
// Phase E–K — the `inflate()` core decode state machine
// ===========================================================================

/// Decompress as much as possible from `input` into `output`, advancing the
/// [`InflateState`] machine. This is the faithful safe-Rust port of the giant
/// `for (;;) switch (state->mode)` loop in C `inflate.c` (L474–L1153).
///
/// Returns an [`InflateOutcome`] carrying the bytes consumed/produced, the C
/// return code, `data_type`, `msg`, and the running `adler`/check so the FFI
/// shim (`src/ffi.rs`) or the idiomatic [`Inflate`] wrapper can update the
/// caller's `z_stream`/accounting.
///
/// # Slice / cursor contract
///
/// `input`/`output` are the per-call buffers (mirroring `next_in`/`avail_in`
/// and `next_out`/`avail_out`). Internally the loop tracks `have`/`left`
/// (remaining input/output) and `next`/`put` (input/output cursors), keeping
/// the invariants `have == input.len() - next` and `left == output.len() - put`
/// exactly as C keeps `avail_in`/`avail_out` in lockstep, and restoring
/// `hold`/`bits` onto the state at `inf_leave`.
///
/// The raw-pointer null checks of C (`next_out == Z_NULL`, etc., L496-498) live
/// in `src/ffi.rs`; here the slices are assumed valid. Degenerate empty slices
/// are handled without panicking (the bit/byte macros gate on `have`/`left`).
#[allow(clippy::too_many_lines)]
pub(crate) fn inflate(
    state: &mut InflateState,
    input: &[u8],
    output: &mut [u8],
    flush: i32,
) -> InflateOutcome {
    // Skip the Z_BLOCK/Z_TREES early-return bookkeeping on re-entry (C L500-501).
    if state.mode == InflateMode::Type {
        state.mode = InflateMode::Typedo;
    }

    // --- Local registers (C inflate.c L476-505) ---------------------------
    let mut next: usize = 0; // input cursor (C pointer `next`)
    let mut put: usize = 0; // output cursor (C pointer `put`)
    let mut have: usize = input.len(); // bytes available in `input` (C `have`)
    let mut left: usize = output.len(); // space available in `output` (C `left`)
    let mut hold: u32 = state.hold; // bit accumulator (C LOAD)
    let mut bits: u32 = state.bits; // bits in accumulator
    let in_start: usize = have; // saved avail_in  (C `in`)
    let out_start: usize = left; // saved avail_out (C `out`)
    // `out_acc` plays C's dual role of `out`: it starts at `out_start` and is
    // reset to `left` inside CHECK after folding the data check, so the trailer
    // fold at `inf_leave` counts every produced byte into Adler/CRC exactly once.
    let mut out_acc: usize = out_start;
    let mut ret: i32 = Z_OK;
    let mut msg: Option<&'static str> = None;
    // Running check accounting mirrors C's `strm->adler = state->check = X`:
    // every such C statement is reproduced as `state.check = X; adler = X;`.
    let mut adler: u32 = state.check;

    // The code-length code order (C `order[19]`, inflate.c L477-478).
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    'inf: loop {
        // --- Bit-accumulator + decode macros (faithful to inflate.c L298-390) --
        // Defined inside the `'inf:` loop body so they can `break 'inf` and
        // reference the loop registers (`have`/`hold`/`bits`/`next`/`input`/
        // `state`) in place — mirroring the C macros over function-local regs.
        macro_rules! pullbyte {
            () => {{
                if have == 0 {
                    break 'inf;
                }
                have -= 1;
                // `+=` matches C; with the masked accumulator the target high bits
                // are zero, so this is equivalent to `|=`. The shift never exceeds
                // 24 (NEEDBITS(32) is only ever called byte-aligned), so the `u32`
                // accumulator cannot overflow.
                hold += (input[next] as u32) << bits;
                next += 1;
                bits += 8;
            }};
        }
        macro_rules! needbits {
            ($n:expr) => {{
                while bits < ($n) {
                    pullbyte!();
                }
            }};
        }
        macro_rules! dropbits {
            ($n:expr) => {{
                hold >>= $n;
                bits -= $n;
            }};
        }
        macro_rules! initbits {
            () => {{
                hold = 0;
                bits = 0;
            }};
        }
        macro_rules! bytebits {
            () => {{
                hold >>= bits & 7;
                bits -= bits & 7;
            }};
        }
        // First-level table decode (C `for(;;){here=tbl[BITS(b)];
        // if(here.bits<=bits)break; PULLBYTE();}`). Yields the root [`Code`].
        macro_rules! decode_here {
            ($base:expr, $tbits:expr) => {{
                let base: usize = $base;
                let tbits: u32 = $tbits;
                loop {
                    let h = state.codes[base + bits_low(hold, tbits) as usize];
                    if (h.bits as u32) <= bits {
                        break h;
                    }
                    pullbyte!();
                }
            }};
        }
        // Second-level (sub-table) decode (C `here=tbl[last.val+(BITS(last.bits+
        // last.op)>>last.bits)]; if(last.bits+here.bits<=bits)break; PULLBYTE();`).
        macro_rules! decode_sub {
            ($base:expr, $last:expr) => {{
                let base: usize = $base;
                let last: Code = $last;
                loop {
                    let idx = base
                        + last.val as usize
                        + (bits_low(hold, last.bits as u32 + last.op as u32) >> last.bits) as usize;
                    let h = state.codes[idx];
                    if last.bits as u32 + h.bits as u32 <= bits {
                        break h;
                    }
                    pullbyte!();
                }
            }};
        }
        // gzip header CRC of the low 2 / 4 bytes of the accumulator (C CRC2/CRC4,
        // inflate.c L310-324). Only invoked from gzip-gated arms.
        #[cfg(feature = "gzip")]
        macro_rules! crc2 {
            ($word:expr) => {{
                let w: u32 = $word;
                let hbuf = [(w & 0xff) as u8, ((w >> 8) & 0xff) as u8];
                state.check = crc32(state.check, &hbuf);
            }};
        }
        #[cfg(feature = "gzip")]
        macro_rules! crc4 {
            ($word:expr) => {{
                let w: u32 = $word;
                let hbuf = [
                    (w & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 24) & 0xff) as u8,
                ];
                state.check = crc32(state.check, &hbuf);
            }};
        }
        // Build-and-return an early-exit outcome for the C paths that `return`
        // directly without running `inf_leave` (the MEM / STREAM_ERROR cases).
        macro_rules! early_return {
            ($code:expr) => {{
                state.hold = hold;
                state.bits = bits;
                let in_consumed = in_start - have;
                let out_produced = out_start - left;
                return InflateOutcome {
                    in_consumed,
                    out_produced,
                    ret: $code,
                    data_type: make_data_type(state.mode, state.last, bits),
                    msg,
                    adler,
                    total_in_delta: in_consumed as u64,
                    total_out_delta: out_produced as u64,
                };
            }};
        }
        match state.mode {
            // ---- HEAD: gzip-magic sniff / zlib header parse (L506-553) ----
            InflateMode::Head => {
                if state.wrap == 0 {
                    state.mode = InflateMode::Typedo;
                    continue 'inf;
                }
                needbits!(16);
                #[cfg(feature = "gzip")]
                {
                    if (state.wrap & 2) != 0 && hold == 0x8b1f {
                        // gzip magic 0x1f 0x8b (stored LSB-first in `hold`).
                        if state.wbits == 0 {
                            state.wbits = 15;
                        }
                        state.check = 0; // crc32(0, Z_NULL, 0)
                        crc2!(hold);
                        initbits!();
                        state.mode = InflateMode::Flags;
                        continue 'inf;
                    }
                    // Not a gzip stream: an attached header request is invalid.
                    if let Some(ref mut head) = state.head {
                        head.done = false; // C: head->done = -1
                    }
                }
                // The `!(wrap & 1)` guard only exists in gzip builds (where the
                // wrapper may be gzip-only); raw-zlib builds always permit zlib.
                #[cfg(feature = "gzip")]
                let bad_header =
                    (state.wrap & 1) == 0 || ((bits_low(hold, 8) << 8) + (hold >> 8)) % 31 != 0;
                #[cfg(not(feature = "gzip"))]
                let bad_header = ((bits_low(hold, 8) << 8) + (hold >> 8)) % 31 != 0;
                if bad_header {
                    msg = Some("incorrect header check");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                if bits_low(hold, 4) != Z_DEFLATED as u32 {
                    msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                dropbits!(4);
                let len = bits_low(hold, 4) + 8;
                if state.wbits == 0 {
                    state.wbits = len;
                }
                if len > 15 || len > state.wbits {
                    msg = Some("invalid window size");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.dmax = 1u32 << len;
                state.flags = 0; // indicate a zlib (non-gzip) stream
                state.check = 1; // adler32(0, Z_NULL, 0)
                adler = 1;
                state.mode = if (hold & 0x200) != 0 {
                    InflateMode::Dictid
                } else {
                    InflateMode::Type
                };
                initbits!();
                continue 'inf;
            }

            // ---- gzip header chain (L555-692), all gzip-gated --------------
            #[cfg(feature = "gzip")]
            InflateMode::Flags => {
                needbits!(16);
                state.flags = hold as i32;
                if (state.flags & 0xff) != Z_DEFLATED {
                    msg = Some("unknown compression method");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                if (state.flags & 0xe000) != 0 {
                    msg = Some("unknown header flags set");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                if let Some(ref mut head) = state.head {
                    head.text = ((hold >> 8) & 1) != 0;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2!(hold);
                }
                initbits!();
                state.mode = InflateMode::Time;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Time => {
                needbits!(32);
                if let Some(ref mut head) = state.head {
                    head.time = hold;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc4!(hold);
                }
                initbits!();
                state.mode = InflateMode::Os;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Os => {
                needbits!(16);
                if let Some(ref mut head) = state.head {
                    head.xflags = (hold & 0xff) as i32;
                    head.os = (hold >> 8) as i32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2!(hold);
                }
                initbits!();
                state.mode = InflateMode::Exlen;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Exlen => {
                if (state.flags & 0x0400) != 0 {
                    needbits!(16);
                    state.length = hold; // FEXTRA length (XLEN)
                    if let Some(ref mut head) = state.head {
                        // Growable collection buffer (no fixed `extra_max`).
                        if head.extra.is_none() {
                            head.extra = Some(Vec::new());
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        crc2!(hold);
                    }
                    initbits!();
                } else if let Some(ref mut head) = state.head {
                    head.extra = None;
                }
                state.mode = InflateMode::Extra;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Extra => {
                if (state.flags & 0x0400) != 0 {
                    let mut copy = state.length as usize;
                    if copy > have {
                        copy = have;
                    }
                    if copy != 0 {
                        // Collect the full extra field; CRC covers every byte.
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut extra) = head.extra {
                                extra.extend_from_slice(&input[next..next + copy]);
                            }
                        }
                        if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                            state.check = crc32(state.check, &input[next..next + copy]);
                        }
                        have -= copy;
                        next += copy;
                        state.length -= copy as u32;
                    }
                    if state.length != 0 {
                        break 'inf;
                    }
                }
                state.length = 0;
                state.mode = InflateMode::Name;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Name => {
                if (state.flags & 0x0800) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    // Lazily create the name buffer on first entry only.
                    if let Some(ref mut head) = state.head {
                        if head.name.is_none() {
                            head.name = Some(Vec::new());
                        }
                    }
                    let mut copy = 0usize;
                    let mut terminated = false;
                    while copy < have {
                        let b = input[next + copy];
                        copy += 1;
                        if b == 0 {
                            terminated = true; // NUL terminator consumed
                            break;
                        }
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut name) = head.name {
                                name.push(b); // store the name WITHOUT the NUL
                            }
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &input[next..next + copy]);
                    }
                    have -= copy;
                    next += copy;
                    if !terminated {
                        break 'inf; // resume on the next call (C `if (len) ...`)
                    }
                } else if let Some(ref mut head) = state.head {
                    head.name = None;
                }
                state.length = 0;
                state.mode = InflateMode::Comment;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Comment => {
                if (state.flags & 0x1000) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    if let Some(ref mut head) = state.head {
                        if head.comment.is_none() {
                            head.comment = Some(Vec::new());
                        }
                    }
                    let mut copy = 0usize;
                    let mut terminated = false;
                    while copy < have {
                        let b = input[next + copy];
                        copy += 1;
                        if b == 0 {
                            terminated = true;
                            break;
                        }
                        if let Some(ref mut head) = state.head {
                            if let Some(ref mut comment) = head.comment {
                                comment.push(b);
                            }
                        }
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &input[next..next + copy]);
                    }
                    have -= copy;
                    next += copy;
                    if !terminated {
                        break 'inf;
                    }
                } else if let Some(ref mut head) = state.head {
                    head.comment = None;
                }
                state.mode = InflateMode::Hcrc;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            InflateMode::Hcrc => {
                if (state.flags & 0x0200) != 0 {
                    needbits!(16);
                    if (state.wrap & 4) != 0 && hold != (state.check & 0xffff) {
                        msg = Some("header crc mismatch");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    initbits!();
                }
                if let Some(ref mut head) = state.head {
                    head.hcrc = ((state.flags >> 9) & 1) != 0;
                    head.done = true;
                }
                state.check = 0; // crc32(0, Z_NULL, 0) — start the data CRC
                adler = 0;
                state.mode = InflateMode::Type;
                continue 'inf;
            }

            // ---- DICTID / DICT: preset-dictionary handshake (L694-707) -----
            InflateMode::Dictid => {
                needbits!(32);
                state.check = hold.swap_bytes(); // C ZSWAP32(hold)
                adler = state.check;
                initbits!();
                state.mode = InflateMode::Dict;
                continue 'inf;
            }
            InflateMode::Dict => {
                if !state.havedict {
                    // Z_NEED_DICT: return WITHOUT consuming more; the caller
                    // supplies the dictionary via `inflate_set_dictionary` then
                    // re-enters. `break 'inf` runs RESTORE through `inf_leave`
                    // (the window update is skipped — no output produced yet).
                    ret = Z_NEED_DICT;
                    break 'inf;
                }
                state.check = 1; // adler32(0, Z_NULL, 0)
                adler = 1;
                state.mode = InflateMode::Type;
                continue 'inf;
            }

            // ---- TYPE / TYPEDO: block-type dispatch (L708-746) ------------
            InflateMode::Type => {
                if flush == Z_BLOCK || flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::Typedo;
                continue 'inf;
            }
            InflateMode::Typedo => {
                if state.last {
                    bytebits!(); // go to byte boundary
                    state.mode = InflateMode::Check;
                    continue 'inf;
                }
                needbits!(3);
                state.last = bits_low(hold, 1) != 0;
                dropbits!(1);
                match bits_low(hold, 2) {
                    0 => {
                        // stored block
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        // fixed Huffman block: install the static tables.
                        // MUST NOT advance `state.next` (keeps inflateCodesUsed
                        // == prior value, matching C `fixedtables`).
                        let ft = inflate_fixed(&mut state.codes);
                        state.lencode = ft.lencode;
                        state.lenbits = ft.lenbits;
                        state.distcode = ft.distcode;
                        state.distbits = ft.distbits;
                        state.mode = InflateMode::LenBegin;
                        if flush == Z_TREES {
                            dropbits!(2);
                            break 'inf;
                        }
                    }
                    2 => {
                        // dynamic Huffman block
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        msg = Some("invalid block type");
                        state.mode = InflateMode::Bad;
                    }
                }
                dropbits!(2);
                continue 'inf;
            }

            // ---- STORED block (L747-781) ----------------------------------
            InflateMode::Stored => {
                bytebits!(); // go to byte boundary
                needbits!(32);
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    msg = Some("invalid stored block lengths");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.length = hold & 0xffff;
                initbits!();
                state.mode = InflateMode::CopyBegin;
                if flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::Copy;
                continue 'inf;
            }
            InflateMode::CopyBegin => {
                state.mode = InflateMode::Copy;
                continue 'inf;
            }
            InflateMode::Copy => {
                let mut copy = state.length as usize;
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
                    // Disjoint slices (input vs output): safe bulk copy.
                    output[put..put + copy].copy_from_slice(&input[next..next + copy]);
                    have -= copy;
                    next += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                    continue 'inf;
                }
                state.mode = InflateMode::Type;
                continue 'inf;
            }

            // ---- TABLE / LENLENS / CODELENS: dynamic tables (L782-910) ----
            InflateMode::Table => {
                needbits!(14);
                state.nlen = bits_low(hold, 5) + 257;
                dropbits!(5);
                state.ndist = bits_low(hold, 5) + 1;
                dropbits!(5);
                state.ncode = bits_low(hold, 4) + 4;
                dropbits!(4);
                if state.nlen > 286 || state.ndist > 30 {
                    msg = Some("too many length or distance symbols");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.have = 0;
                state.mode = InflateMode::Lenlens;
                continue 'inf;
            }
            InflateMode::Lenlens => {
                while state.have < state.ncode {
                    needbits!(3);
                    state.lens[ORDER[state.have as usize]] = bits_low(hold, 3) as u16;
                    state.have += 1;
                    dropbits!(3);
                }
                while state.have < 19 {
                    state.lens[ORDER[state.have as usize]] = 0;
                    state.have += 1;
                }
                state.next = 0;
                state.lencode = 0;
                state.distcode = 0;
                state.lenbits = 7;
                // Disjoint named fields → field-level borrow split is allowed.
                ret = inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    &mut state.next,
                    &mut state.lenbits,
                    &mut state.work,
                );
                if ret != 0 {
                    msg = Some("invalid code lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.have = 0;
                state.mode = InflateMode::Codelens;
                continue 'inf;
            }
            InflateMode::Codelens => {
                while state.have < state.nlen + state.ndist {
                    let here = decode_here!(state.lencode, state.lenbits);
                    if (here.val as u32) < 16 {
                        dropbits!(here.bits as u32);
                        state.lens[state.have as usize] = here.val;
                        state.have += 1;
                    } else {
                        let len: u16;
                        let mut copy: u32;
                        if here.val == 16 {
                            needbits!(here.bits as u32 + 2);
                            dropbits!(here.bits as u32);
                            if state.have == 0 {
                                msg = Some("invalid bit length repeat");
                                state.mode = InflateMode::Bad;
                                break;
                            }
                            len = state.lens[state.have as usize - 1];
                            copy = 3 + bits_low(hold, 2);
                            dropbits!(2);
                        } else if here.val == 17 {
                            needbits!(here.bits as u32 + 3);
                            dropbits!(here.bits as u32);
                            len = 0;
                            copy = 3 + bits_low(hold, 3);
                            dropbits!(3);
                        } else {
                            needbits!(here.bits as u32 + 7);
                            dropbits!(here.bits as u32);
                            len = 0;
                            copy = 11 + bits_low(hold, 7);
                            dropbits!(7);
                        }
                        if state.have + copy > state.nlen + state.ndist {
                            msg = Some("invalid bit length repeat");
                            state.mode = InflateMode::Bad;
                            break;
                        }
                        while copy != 0 {
                            state.lens[state.have as usize] = len;
                            state.have += 1;
                            copy -= 1;
                        }
                    }
                }

                // Propagate a BAD set inside the loop (C `if (mode==BAD) break;`).
                if state.mode == InflateMode::Bad {
                    continue 'inf;
                }
                // There must be an end-of-block code.
                if state.lens[256] == 0 {
                    msg = Some("invalid code -- missing end-of-block");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // Build the literal/length table (root width 9 — preserve EXACTLY).
                state.next = 0;
                state.lencode = 0;
                state.lenbits = 9;
                ret = inflate_table(
                    CodeType::Lens,
                    &state.lens,
                    state.nlen as usize,
                    &mut state.codes,
                    &mut state.next,
                    &mut state.lenbits,
                    &mut state.work,
                );
                if ret != 0 {
                    msg = Some("invalid literal/lengths set");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                // Build the distance table (root width 6 — preserve EXACTLY).
                state.distcode = state.next;
                state.distbits = 6;
                let nlen = state.nlen as usize;
                ret = inflate_table(
                    CodeType::Dists,
                    &state.lens[nlen..],
                    state.ndist as usize,
                    &mut state.codes,
                    &mut state.next,
                    &mut state.distbits,
                    &mut state.work,
                );
                if ret != 0 {
                    msg = Some("invalid distances set");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.mode = InflateMode::LenBegin;
                if flush == Z_TREES {
                    break 'inf;
                }
                state.mode = InflateMode::Len;
                continue 'inf;
            }

            // ---- LEN_ / LEN: literal/length decode (L911-963) -------------
            InflateMode::LenBegin => {
                state.mode = InflateMode::Len;
                continue 'inf;
            }
            InflateMode::Len => {
                // Fast path: the hot loop in `fast.rs` (the sole core `unsafe`
                // site). Its entry precondition `bits < 8` is maintained by the
                // decode loop (empirically verified against C zlib).
                if have >= 6 && left >= 258 {
                    state.hold = hold; // RESTORE
                    state.bits = bits;
                    if let Some(m) = fast::inflate_fast(input, &mut next, output, &mut put, state) {
                        msg = Some(m);
                    }
                    hold = state.hold; // LOAD
                    bits = state.bits;
                    have = input.len() - next;
                    left = output.len() - put;
                    if state.mode == InflateMode::Type {
                        state.back = -1;
                    }
                    continue 'inf;
                }
                state.back = 0;
                let mut here = decode_here!(state.lencode, state.lenbits);
                if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last = here;
                    here = decode_sub!(state.lencode, last);
                    dropbits!(last.bits as u32);
                    state.back += last.bits as i32;
                }
                dropbits!(here.bits as u32);
                state.back += here.bits as i32;
                state.length = here.val as u32;
                if here.op == 0 {
                    state.mode = InflateMode::Lit;
                    continue 'inf;
                }
                if (here.op & 32) != 0 {
                    // end of block
                    state.back = -1;
                    state.mode = InflateMode::Type;
                    continue 'inf;
                }
                if (here.op & 64) != 0 {
                    msg = Some("invalid literal/length code");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::Lenext;
                continue 'inf;
            }
            InflateMode::Lenext => {
                if state.extra != 0 {
                    needbits!(state.extra);
                    state.length += bits_low(hold, state.extra);
                    dropbits!(state.extra);
                    state.back += state.extra as i32;
                }
                state.was = state.length;
                state.mode = InflateMode::Dist;
                continue 'inf;
            }

            // ---- DIST / DISTEXT: distance decode (L975-1019) --------------
            InflateMode::Dist => {
                let mut here = decode_here!(state.distcode, state.distbits);
                if (here.op & 0xf0) == 0 {
                    let last = here;
                    here = decode_sub!(state.distcode, last);
                    dropbits!(last.bits as u32);
                    state.back += last.bits as i32;
                }
                dropbits!(here.bits as u32);
                state.back += here.bits as i32;
                if (here.op & 64) != 0 {
                    msg = Some("invalid distance code");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.offset = here.val as u32;
                state.extra = (here.op & 15) as u32;
                state.mode = InflateMode::Distext;
                continue 'inf;
            }
            InflateMode::Distext => {
                if state.extra != 0 {
                    needbits!(state.extra);
                    state.offset += bits_low(hold, state.extra);
                    dropbits!(state.extra);
                    state.back += state.extra as i32;
                }
                // INFLATE_STRICT `offset > dmax` check is OFF by default — omit.
                state.mode = InflateMode::Match;
                continue 'inf;
            }

            // ---- MATCH: back-reference copy (L1020-1065) ------------------
            // 100% safe: window copies use disjoint indexing; the overlapping
            // copy-from-output case is a byte-by-byte index loop (NEVER
            // `copy_from_slice`, which would be unsound on overlap).
            InflateMode::Match => {
                if left == 0 {
                    break 'inf;
                }
                // C `copy = out - left`; since `put == out_start - left` always
                // and CHECK has not yet run (`out == out_start`), this is `put`.
                let mut copy = put;
                let mut from: usize;
                let from_window: bool;
                if state.offset as usize > copy {
                    // Distance reaches before this call's output → window copy.
                    copy = state.offset as usize - copy;
                    // C rejects the stream when the distance reaches before the
                    // start of the window (`copy > whave`) and validation is
                    // enabled (`sane`). INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR
                    // is OFF in this build, so the `!sane` path intentionally
                    // does nothing — the over-far distance is tolerated.
                    if copy > state.whave as usize && state.sane {
                        msg = Some("invalid distance too far back");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    if copy > state.wnext as usize {
                        copy -= state.wnext as usize;
                        from = state.wsize as usize - copy;
                    } else {
                        from = state.wnext as usize - copy;
                    }
                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                    from_window = true;
                } else {
                    // Distance is within this call's output → copy from output.
                    from = put - state.offset as usize;
                    copy = state.length as usize;
                    from_window = false;
                }
                if copy > left {
                    copy = left;
                }
                left -= copy;
                state.length -= copy as u32;
                if from_window {
                    for _ in 0..copy {
                        output[put] = state.window[from];
                        put += 1;
                        from += 1;
                    }
                } else {
                    // Byte-by-byte to honour LZ77 overlap.
                    for _ in 0..copy {
                        let b = output[from];
                        output[put] = b;
                        put += 1;
                        from += 1;
                    }
                }
                if state.length == 0 {
                    state.mode = InflateMode::Len;
                }
                continue 'inf;
            }

            // ---- LIT: emit one literal byte (L1066-1071) ------------------
            InflateMode::Lit => {
                if left == 0 {
                    break 'inf;
                }
                output[put] = state.length as u8;
                put += 1;
                left -= 1;
                state.mode = InflateMode::Len;
                continue 'inf;
            }

            // ---- CHECK: data check (Adler-32 / gzip CRC) (L1072-1096) -----
            InflateMode::Check => {
                if state.wrap != 0 {
                    needbits!(32);
                    let produced = out_acc - left; // C `out -= left`
                    state.total += produced as u64;
                    if (state.wrap & 4) != 0 && produced != 0 {
                        let region = &output[put - produced..put];
                        state.check = update_check(state.check, region, state.flags);
                        adler = state.check;
                    }
                    out_acc = left; // C `out = left` (so inf_leave won't refold)
                    if (state.wrap & 4) != 0 {
                        // gzip CRC is stored LSB-first (`hold`); zlib Adler is
                        // big-endian (`ZSWAP32`). `flags == 0` ⇒ zlib in all
                        // builds, so this branch is correct without a cfg.
                        let want = if state.flags != 0 {
                            hold
                        } else {
                            hold.swap_bytes()
                        };
                        if want != state.check {
                            msg = Some("incorrect data check");
                            state.mode = InflateMode::Bad;
                            continue 'inf;
                        }
                    }
                    initbits!();
                }
                #[cfg(feature = "gzip")]
                {
                    state.mode = InflateMode::Length;
                }
                #[cfg(not(feature = "gzip"))]
                {
                    state.mode = InflateMode::Done;
                }
                continue 'inf;
            }

            // ---- LENGTH: gzip ISIZE trailer (L1097-1108), gzip-gated ------
            #[cfg(feature = "gzip")]
            InflateMode::Length => {
                if state.wrap != 0 && state.flags != 0 {
                    needbits!(32);
                    if (state.wrap & 4) != 0 && hold != (state.total & 0xffff_ffff) as u32 {
                        msg = Some("incorrect length check");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                    initbits!();
                }
                state.mode = InflateMode::Done;
                continue 'inf;
            }

            // ---- terminal states (L1111-1122) -----------------------------
            InflateMode::Done => {
                ret = Z_STREAM_END;
                break 'inf;
            }
            InflateMode::Bad => {
                ret = Z_DATA_ERROR;
                break 'inf;
            }
            InflateMode::Mem => {
                early_return!(Z_MEM_ERROR);
            }
            InflateMode::Sync => {
                early_return!(Z_STREAM_ERROR);
            }

            // In non-gzip builds the gzip-only modes are unreachable (the
            // wrapper never gains bit 1); cover them to keep `match` exhaustive,
            // returning Z_STREAM_ERROR exactly as C's `default:` case.
            #[cfg(not(feature = "gzip"))]
            InflateMode::Flags
            | InflateMode::Time
            | InflateMode::Os
            | InflateMode::Exlen
            | InflateMode::Extra
            | InflateMode::Name
            | InflateMode::Comment
            | InflateMode::Hcrc
            | InflateMode::Length => {
                early_return!(Z_STREAM_ERROR);
            }
        }
    }

    // =======================================================================
    // inf_leave (C inflate.c L1131-1152) — RESTORE, deferred window update,
    // accounting, trailing check fold, data_type, and the Z_BUF_ERROR rule.
    // =======================================================================
    state.hold = hold; // RESTORE
    state.bits = bits;

    // Deferred sliding-window update. Condition mirrors C exactly: update when a
    // window already exists, or when output was produced and we are not at a
    // terminal/finished trailer state.
    let mode_u8 = state.mode as u8;
    if state.wsize != 0
        || (out_acc != left
            && mode_u8 < InflateMode::Bad as u8
            && (mode_u8 < InflateMode::Check as u8 || flush != Z_FINISH))
    {
        let produced = out_acc - left;
        if update_window(state, output, put, produced) != 0 {
            // Window allocation failed → C `state->mode = MEM; return Z_MEM_ERROR`.
            state.mode = InflateMode::Mem;
            let in_consumed = in_start - have;
            let out_produced = out_start - left;
            return InflateOutcome {
                in_consumed,
                out_produced,
                ret: Z_MEM_ERROR,
                data_type: make_data_type(state.mode, state.last, bits),
                msg,
                adler,
                total_in_delta: in_consumed as u64,
                total_out_delta: out_produced as u64,
            };
        }
    }

    let in_consumed = in_start - have;
    let out_produced = out_start - left;
    // `state.total` accrues the produced bytes not already folded in CHECK
    // (after CHECK, `out_acc == left`, so this adds 0 — no double counting).
    state.total += (out_acc - left) as u64;
    // Trailing data-check fold for the non-CHECK exit (C L1144-1146). After
    // CHECK ran, `out_acc - left == 0`, so this is skipped (no double fold).
    if (state.wrap & 4) != 0 && (out_acc - left) != 0 {
        let produced = out_acc - left;
        let region = &output[put - produced..put];
        state.check = update_check(state.check, region, state.flags);
        adler = state.check;
    }
    let data_type = make_data_type(state.mode, state.last, bits);
    // Z_BUF_ERROR when no progress was made (C L1150-1151).
    if ((in_consumed == 0 && out_produced == 0) || flush == Z_FINISH) && ret == Z_OK {
        ret = Z_BUF_ERROR;
    }
    InflateOutcome {
        in_consumed,
        out_produced,
        ret,
        data_type,
        msg,
        adler,
        total_in_delta: in_consumed as u64,
        total_out_delta: out_produced as u64,
    }
}

// ===========================================================================
// Phase J — post-inflate API functions (C inflate.c L1155-1413)
//
// These are the faithful safe-Rust ports of the public `inflate*` helpers that
// live *after* the core decode loop in C. They operate on `&mut InflateState`
// (or `&InflateState`) and return the C `int`/`long` codes directly; the public
// C symbol names are minted in `src/ffi.rs`, and the idiomatic [`Inflate`]
// wrapper (Phase L) calls these too. The raw-pointer null / `zalloc` / `zfree`
// guards that C performs via `inflateStateCheck` live at the FFI boundary; here
// the typed `&mut InflateState` already encodes "the state exists and is the
// inflate kind", so [`state_check`] is the vacuous guard documented above.
// ===========================================================================

/// C `inflateEnd` (L1155-1165) — release the inflate state.
///
/// In C this `ZFREE`s `state->window` and then `state->state`. In this crate
/// both the window (`Vec<u8>`) and the code table (`Box<[Code]>`) are owned by
/// [`InflateState`] and freed automatically by its [`Drop`] impl, so "ending"
/// the stream is simply dropping the owning `Box`. There is no manual free and
/// no possibility of a double free or leak — the borrow checker guarantees it.
///
/// Callers that hold the state behind a raw pointer (the FFI shim) call this to
/// reclaim the box; the idiomatic [`Inflate`] wrapper relies on its own
/// `Drop`. Always returns `Z_OK` at the FFI layer.
///
/// The sole caller (the FFI shim in `src/ffi.rs`) arrives in a later
/// checkpoint, so permit the forward-declared definition here.
#[allow(dead_code)]
pub(crate) fn inflate_end(state: Box<InflateState>) {
    // Explicit drop documents the RAII teardown (runs `InflateState::drop`,
    // which frees the window and code table).
    drop(state);
}

/// C `inflateGetDictionary` (L1167-1185) — copy the sliding-window contents
/// (the most recent up-to-`whave` bytes) into `dictionary` in chronological
/// order, returning the number of bytes available.
///
/// The window is a circular buffer with the write cursor at `wnext`; the oldest
/// retained bytes occupy `window[wnext..whave]` and the newest occupy
/// `window[..wnext]`, so emitting them in that order yields the dictionary in
/// stream order (exactly as C does with its two `zmemcpy` calls).
///
/// `dictionary` must be at least `whave` bytes long (the canonical idiom is to
/// call once with an empty slice to learn the length, then again with a
/// correctly sized buffer). All copies are safe `copy_from_slice` over disjoint
/// ranges. Returns `whave`.
pub(crate) fn inflate_get_dictionary(state: &InflateState, dictionary: &mut [u8]) -> u32 {
    if state.whave != 0 && !dictionary.is_empty() {
        let wnext = state.wnext as usize;
        let whave = state.whave as usize;
        // Older span: window[wnext..whave] → dictionary[0..whave-wnext].
        let first = whave - wnext;
        dictionary[..first].copy_from_slice(&state.window[wnext..whave]);
        // Newer span: window[0..wnext] → dictionary[first..first+wnext].
        dictionary[first..first + wnext].copy_from_slice(&state.window[..wnext]);
    }
    state.whave
}

/// C `inflateSetDictionary` (L1187-1217) — install a preset dictionary,
/// completing the `Z_NEED_DICT` handshake.
///
/// Valid only for a raw stream (`wrap == 0`) at any point, or for a zlib stream
/// stopped in the `DICT` state awaiting its dictionary. When in `DICT`, the
/// dictionary's Adler-32 identifier is checked against the value carried in the
/// stream (`state.check`); a mismatch is `Z_DATA_ERROR`. The Adler-32 is seeded
/// with the literal init value `1` (C `adler32(0L, Z_NULL, 0) == 1`; see the
/// crate-wide checksum-init convention) and folded over the whole dictionary.
///
/// The dictionary is then folded into the sliding window by reusing
/// [`update_window`] (C calls `updatewindow(strm, dictionary + dictLength,
/// dictLength)`, i.e. `end == copy == dictLength`). Returns `Z_OK`,
/// `Z_STREAM_ERROR` (wrong state), or `Z_DATA_ERROR` (id mismatch).
pub(crate) fn inflate_set_dictionary(state: &mut InflateState, dictionary: &[u8]) -> i32 {
    // A dictionary may only be set on a raw stream or one awaiting one.
    if state.wrap != 0 && state.mode != InflateMode::Dict {
        return Z_STREAM_ERROR;
    }
    // Verify the dictionary identifier when one is expected (zlib DICT state).
    if state.mode == InflateMode::Dict {
        // adler32(1, dict) — the `1` is the literal Adler-32 init value.
        let dictid = adler32(1, dictionary);
        if dictid != state.check {
            return Z_DATA_ERROR;
        }
    }
    // Fold the dictionary into the window (amending any existing window). The
    // C call passes the END pointer + length, i.e. read `output[end-copy..end]`
    // == the whole dictionary, with `end == copy == dict.len()`.
    if update_window(state, dictionary, dictionary.len(), dictionary.len()) != 0 {
        state.mode = InflateMode::Mem;
        return Z_MEM_ERROR;
    }
    state.havedict = true;
    Z_OK
}

/// C `inflateGetHeader` (L1219-1231) — request capture of the gzip header into
/// a fresh [`GzHeader`] sink owned by the state.
///
/// Valid only when gzip decoding is possible (`wrap & 2`); the HEAD/…/HCRC
/// header-parse arms then populate `state.head`. The freshly attached header
/// has `done == false` (C `head->done = 0`). Returns `Z_OK` or
/// `Z_STREAM_ERROR`. Gated on the `gzip` feature (the `head` field, and the
/// gzip wrap bit, only exist there).
#[cfg(feature = "gzip")]
pub(crate) fn inflate_get_header(state: &mut InflateState) -> i32 {
    if (state.wrap & 2) == 0 {
        return Z_STREAM_ERROR;
    }
    // GzHeader::new() has done == false, mirroring C's `head->done = 0`.
    state.head = Some(GzHeader::new());
    Z_OK
}

/// C `syncsearch` (L1244-1262) — scan `buf` for the four-byte flush marker
/// `00 00 FF FF`, resuming from a partial match count in `*have` (0..=3).
///
/// On return `*have` is the updated match length; if it reaches `4` the pattern
/// was found and the returned index is how many bytes were consumed (including
/// the final marker byte). Otherwise the whole buffer was consumed
/// (`buf.len()`) and the search can continue later with more input. This is a
/// byte-exact port of the C state machine (note the `4 - got` reset that lets a
/// `00` re-anchor a broken run). Pure/safe; no window or state access.
fn syncsearch(have: &mut u32, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next = 0usize;
    while next < buf.len() && got < 4 {
        let byte = buf[next];
        // The expected byte is 0x00 for the first two positions, 0xFF after.
        let want = if got < 2 { 0u8 } else { 0xff };
        if byte == want {
            got += 1;
        } else if byte != 0 {
            got = 0;
        } else {
            got = 4 - got;
        }
        next += 1;
    }
    *have = got;
    next
}

/// Result of an [`inflate_sync`] call: the C return code plus how many input
/// bytes were consumed scanning for the flush marker (advances `next_in` /
/// `total_in`), and the reset stream `adler` accounting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SyncOutcome {
    /// `Z_OK` (resynchronized), `Z_DATA_ERROR` (marker not found), or
    /// `Z_BUF_ERROR` (no input and nothing buffered to search).
    pub ret: i32,
    /// Input bytes consumed this call (advance `next_in` and `total_in`).
    pub in_consumed: usize,
    /// Reset value for `strm->adler` (mirrors C's `inflateReset`).
    pub adler: u32,
}

/// C `inflateSync` (L1264-1310) — skip invalid compressed data and look for a
/// full flush point (`00 00 FF FF`) so decoding can restart on a fresh block
/// (AAP §0.6.7 error recovery).
///
/// First the residual bit-accumulator is byte-aligned and flushed into a small
/// scratch buffer and scanned; then the available `input` is scanned. When the
/// four-byte marker is found the stream is reset (preserving the public totals,
/// which the wrapper/FFI tracks externally) and positioned at `TYPE` to resume.
/// If no header had been seen yet (`flags == -1`) the stream is downgraded to
/// raw (`wrap = 0`); otherwise the check-validation bit is cleared
/// (`wrap &= ~4`) because a check value can no longer be computed.
///
/// Returns a [`SyncOutcome`]. The caller advances `next_in`/`total_in` by
/// `in_consumed`.
pub(crate) fn inflate_sync(state: &mut InflateState, input: &[u8]) -> SyncOutcome {
    // Nothing buffered and no input → cannot make progress.
    if input.is_empty() && state.bits < 8 {
        return SyncOutcome {
            ret: Z_BUF_ERROR,
            in_consumed: 0,
            adler: (state.wrap & 1) as u32,
        };
    }

    // First entry: byte-align and drain the bit accumulator into `buf`, then
    // seed the marker search from those bytes.
    if state.mode != InflateMode::Sync {
        state.mode = InflateMode::Sync;
        state.hold >>= state.bits & 7;
        state.bits -= state.bits & 7;
        let mut buf = [0u8; 4];
        let mut len = 0usize;
        while state.bits >= 8 {
            buf[len] = state.hold as u8;
            len += 1;
            state.hold >>= 8;
            state.bits -= 8;
        }
        state.have = 0;
        let mut got = state.have;
        syncsearch(&mut got, &buf[..len]);
        state.have = got;
    }

    // Scan the supplied input for the remainder of the marker.
    let mut got = state.have;
    let in_consumed = syncsearch(&mut got, input);
    state.have = got;

    // Marker not (yet) found: report data error, keeping `have` for next time.
    if state.have != 4 {
        return SyncOutcome {
            ret: Z_DATA_ERROR,
            in_consumed,
            adler: (state.wrap & 1) as u32,
        };
    }

    // Found a flush point: downgrade or stop check-validation, then reset and
    // resume at a fresh block. C saves/restores the *public* totals around
    // `inflateReset`; here those totals are tracked by the caller, and the
    // internal `state.total` is intentionally left reset to 0 (it is only read
    // by the gzip LENGTH check, which is now disabled via `wrap &= ~4`).
    if state.flags == -1 {
        state.wrap = 0; // no header seen → treat as raw
    } else {
        state.wrap &= !4; // a check value can no longer be computed
    }
    let flags = state.flags;
    let mut adler = state.check;
    inflate_reset(state, &mut adler);
    state.flags = flags;
    state.mode = InflateMode::Type;
    SyncOutcome {
        ret: Z_OK,
        in_consumed,
        adler,
    }
}

/// C `inflateSyncPoint` (L1320-1326) — report whether inflate is stopped at the
/// boundary of a `Z_SYNC_FLUSH`/`Z_FULL_FLUSH` empty stored block (used by some
/// PPP implementations). True iff `mode == STORED && bits == 0`.
pub(crate) fn inflate_sync_point(state: &InflateState) -> i32 {
    i32::from(state.mode == InflateMode::Stored && state.bits == 0)
}

/// C `inflateCopy` (L1328-1368) — duplicate a mid-stream inflate state.
///
/// In C this hand-copies the struct, allocates a fresh window, and **fixes up**
/// the `lencode`/`distcode`/`next` pointers so they index into the *copy's*
/// `codes[]` rather than the source's. In this crate those fields are already
/// `usize` **indices** (not pointers) and `window`/`codes` are owned
/// `Vec`/`Box`, so a plain `Clone` reproduces the pointer fix-up automatically
/// and correctly: the indices stay valid against the cloned tables, and the
/// buffers are deep-copied. No manual fix-up is possible or necessary.
pub(crate) fn inflate_copy(source: &InflateState) -> InflateState {
    source.clone()
}

/// C `inflateUndermine` (L1370-1383) — with
/// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` **off** (the repo default), this
/// is the no-op-but-error path: it forces `sane = true` (ignoring `subvert`)
/// and returns `Z_DATA_ERROR`, exactly as the C `#else` branch does.
///
/// Exposed only through the FFI `inflateUndermine` shim (`src/ffi.rs`, a later
/// checkpoint), so permit the forward-declared definition here.
#[allow(dead_code)]
pub(crate) fn inflate_undermine(state: &mut InflateState, _subvert: i32) -> i32 {
    state.sane = true;
    Z_DATA_ERROR
}

/// C `inflateValidate` (L1385-1395) — enable or disable check-value validation
/// at run time by toggling wrap bit 2 (`4`). Enabling has effect only when a
/// wrapper is in use (`wrap != 0`). Always returns `Z_OK`.
pub(crate) fn inflate_validate(state: &mut InflateState, check: i32) -> i32 {
    if check != 0 && state.wrap != 0 {
        state.wrap |= 4;
    } else {
        state.wrap &= !4;
    }
    Z_OK
}

/// C `inflateMark` (L1397-1406) — encode decode-position diagnostics for random
/// access: the high bits carry `state.back` (bits back into the current code)
/// and the low bits carry the in-progress copy length.
///
/// Returns `(back << 16) + extra`, where `extra` is the remaining `COPY`
/// length, the consumed `MATCH` length (`was - length`), or `0` otherwise. The
/// `back << 16` is computed with the same wrap-around as C
/// (`back == -1` yields `-(1 << 16)` in the high bits). The FFI maps the
/// inconsistent-state case to `-(1 << 16)`.
pub(crate) fn inflate_mark(state: &InflateState) -> i64 {
    let back = (state.back as i64) << 16;
    let extra = match state.mode {
        InflateMode::Copy => i64::from(state.length),
        InflateMode::Match => i64::from(state.was - state.length),
        _ => 0,
    };
    back + extra
}

/// C `inflateCodesUsed` (L1408-1413) — the number of code-table entries
/// consumed so far. C computes `state->next - state->codes`; since `next` is
/// already that index in this crate, it is returned directly.
pub(crate) fn inflate_codes_used(state: &InflateState) -> u64 {
    state.next as u64
}

// ===========================================================================
// Phase L — idiomatic public wrapper (the safe Rust API)
//
// `Inflate` is the ergonomic, ownership-based front end over the slice-based
// core engine above. It owns the `Box<InflateState>` directly (so teardown is
// automatic via `Drop` — no `inflateEnd` call required) and mirrors the public
// accounting fields of a C `z_stream` (`total_in`/`total_out`/`adler`/`msg`/
// `data_type`) so callers get the same observable bookkeeping without touching
// raw pointers. The `pub(crate)` free functions above remain the exact-C-name
// surface that `src/ffi.rs` marshals; this wrapper is what idiomatic Rust
// callers (and the integration tests) use.
//
// NOTE: `std::io::Read`/`Write` adapters intentionally live in the `gz` layer
// (AAP §0.3.2); this wrapper exposes the raw per-call `inflate` instead so it
// stays `no_std`-clean and gives callers full control over flush semantics.
// ===========================================================================

/// Map a raw C status code to the idiomatic [`ZlibError`], collapsing any
/// unexpected value to [`ZlibError::StreamError`] (zlib's inconsistent-state
/// code). Used by the wrapper's `Result`-returning methods.
#[inline]
fn err_from_code(code: i32) -> ZlibError {
    ZlibError::from_c_int(code).unwrap_or(ZlibError::StreamError)
}

/// An owned, idiomatic DEFLATE/zlib/gzip **decompressor**.
///
/// `Inflate` wraps the inflate state machine with Rust ownership: construct it
/// with [`Inflate::new`] (zlib/gzip default window) or
/// [`Inflate::with_window_bits`] (raw/gzip/auto via the `windowBits`
/// overloading), then drive it with [`Inflate::inflate`], feeding input slices
/// and receiving output slices until it returns [`Z_STREAM_END`]. The state and
/// all buffers are released automatically when the value is dropped (the
/// idiomatic replacement for `inflateEnd`).
///
/// The public fields mirror the corresponding `z_stream` accounting so callers
/// can inspect progress exactly as with C zlib.
///
/// # Examples
///
/// ```ignore
/// let mut inf = Inflate::new()?;            // zlib wrapper, 32 KiB window
/// let mut out = vec![0u8; 1 << 16];
/// let (ret, consumed, produced) = inf.inflate(&compressed, &mut out, Z_NO_FLUSH);
/// assert_eq!(ret, Z_STREAM_END);
/// ```
pub struct Inflate {
    /// The owned engine state (freed on `Drop`).
    state: Box<InflateState>,
    /// Total input bytes consumed across all calls (mirror of `total_in`).
    pub total_in: u64,
    /// Total output bytes produced across all calls (mirror of `total_out`).
    pub total_out: u64,
    /// Running Adler-32 (zlib) or CRC-32 (gzip) check (mirror of `adler`).
    pub adler: u32,
    /// Last error/status message, if any (mirror of `msg`).
    pub msg: Option<&'static str>,
    /// Decode position/type flags after the last call (mirror of `data_type`).
    pub data_type: i32,
}

impl Inflate {
    /// Create a decompressor for the default window size ([`DEF_WBITS`] = 15),
    /// expecting a zlib (RFC 1950) wrapper. Equivalent to
    /// `with_window_bits(DEF_WBITS)`.
    ///
    /// # Errors
    /// Returns [`ZlibError::StreamError`] only if the (fixed) window size is
    /// somehow rejected — i.e. never in practice for this constructor.
    pub fn new() -> Result<Self, ZlibError> {
        Self::with_window_bits(DEF_WBITS)
    }

    /// Create a decompressor for the given `window_bits`, applying the full
    /// `windowBits` overloading convention (AAP §0.6.7):
    ///
    /// * `8..=15` — zlib (RFC 1950);
    /// * `-15..=-8` — raw DEFLATE (RFC 1951), no wrapper/trailer;
    /// * `24..=31` — gzip (RFC 1952);
    /// * `40..=47` — automatic zlib/gzip detection.
    ///
    /// # Errors
    /// Returns [`ZlibError::StreamError`] if `window_bits` is out of range.
    pub fn with_window_bits(window_bits: i32) -> Result<Self, ZlibError> {
        match inflate_init2(window_bits) {
            Ok(state) => Ok(Self {
                state,
                total_in: 0,
                total_out: 0,
                adler: 0,
                msg: None,
                data_type: 0,
            }),
            Err(code) => Err(err_from_code(code)),
        }
    }

    /// Reset the decompressor to begin a new stream, keeping the same window
    /// configuration (C `inflateReset`). Clears the accounting fields.
    pub fn reset(&mut self) {
        let mut adler = self.adler;
        inflate_reset(&mut self.state, &mut adler);
        self.adler = adler;
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = 0;
    }

    /// Reset and reconfigure the window/framing via `window_bits` (C
    /// `inflateReset2`); see [`Inflate::with_window_bits`] for the encoding.
    ///
    /// # Errors
    /// Returns [`ZlibError::StreamError`] if `window_bits` is out of range.
    pub fn reset2(&mut self, window_bits: i32) -> Result<(), ZlibError> {
        let mut adler = self.adler;
        let ret = inflate_reset2(&mut self.state, window_bits, &mut adler);
        if ret != Z_OK {
            return Err(err_from_code(ret));
        }
        self.adler = adler;
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = 0;
        Ok(())
    }

    /// Install a preset dictionary (C `inflateSetDictionary`), completing the
    /// `Z_NEED_DICT` handshake for a zlib stream or seeding a raw stream.
    ///
    /// # Errors
    /// [`ZlibError::StreamError`] if called in the wrong state, or
    /// [`ZlibError::DataError`] if the dictionary's Adler-32 id does not match
    /// the value carried in the stream.
    pub fn set_dictionary(&mut self, dictionary: &[u8]) -> Result<(), ZlibError> {
        let ret = inflate_set_dictionary(&mut self.state, dictionary);
        if ret != Z_OK {
            return Err(err_from_code(ret));
        }
        Ok(())
    }

    /// Copy the current sliding-window contents into `out` in chronological
    /// order (C `inflateGetDictionary`), returning the number of bytes
    /// available. Call with an empty slice to query the length without copying.
    ///
    /// `out` must be at least the available length (the queried value) to
    /// receive the full dictionary; a shorter slice would panic on the safe
    /// copy, so size it from the empty-slice query first.
    #[must_use]
    pub fn get_dictionary(&self, out: &mut [u8]) -> usize {
        inflate_get_dictionary(&self.state, out) as usize
    }

    /// Insert `bits` low bits of `value` ahead of the input (C `inflatePrime`);
    /// a negative `bits` flushes the bit accumulator.
    ///
    /// # Errors
    /// [`ZlibError::StreamError`] if `bits > 16` or the accumulator would
    /// overflow 32 bits.
    pub fn prime(&mut self, bits: i32, value: i32) -> Result<(), ZlibError> {
        let ret = inflate_prime(&mut self.state, bits, value);
        if ret != Z_OK {
            return Err(err_from_code(ret));
        }
        Ok(())
    }

    /// Request capture of the gzip header (C `inflateGetHeader`). After a
    /// successful decode of the header, read it back with [`Inflate::header`].
    ///
    /// # Errors
    /// [`ZlibError::StreamError`] if this stream cannot decode gzip
    /// (`window_bits` did not select gzip or auto-detect).
    #[cfg(feature = "gzip")]
    pub fn get_header(&mut self) -> Result<(), ZlibError> {
        let ret = inflate_get_header(&mut self.state);
        if ret != Z_OK {
            return Err(err_from_code(ret));
        }
        Ok(())
    }

    /// Borrow the captured gzip header, if capture was requested via
    /// [`Inflate::get_header`] and at least the fixed fields have been parsed.
    #[cfg(feature = "gzip")]
    #[must_use]
    pub fn header(&self) -> Option<&GzHeader> {
        self.state.head.as_ref()
    }

    /// Skip invalid input and resynchronize at the next full flush point
    /// (C `inflateSync`); see `inflate_sync`. Returns `(ret, consumed)` where
    /// `ret` is `Z_OK` (resynchronized), `Z_DATA_ERROR` (marker not found), or
    /// `Z_BUF_ERROR` (no progress possible), and `consumed` is how many input
    /// bytes were scanned. Advances `total_in` by `consumed`.
    pub fn sync(&mut self, input: &[u8]) -> (i32, usize) {
        let outcome = inflate_sync(&mut self.state, input);
        self.total_in += outcome.in_consumed as u64;
        self.adler = outcome.adler;
        (outcome.ret, outcome.in_consumed)
    }

    /// Report whether inflate is stopped at a `Z_SYNC_FLUSH`/`Z_FULL_FLUSH`
    /// empty-stored-block boundary (C `inflateSyncPoint`).
    #[must_use]
    pub fn sync_point(&self) -> bool {
        inflate_sync_point(&self.state) != 0
    }

    /// Duplicate this decompressor mid-stream (C `inflateCopy`). Because the
    /// state holds only indices and owned buffers, this is a deep [`Clone`] with
    /// no pointer fix-up required; both copies can then decode independently.
    #[must_use]
    pub fn copy(&self) -> Self {
        Self {
            state: Box::new(inflate_copy(&self.state)),
            total_in: self.total_in,
            total_out: self.total_out,
            adler: self.adler,
            msg: self.msg,
            data_type: self.data_type,
        }
    }

    /// Encode current decode-position diagnostics for random access
    /// (C `inflateMark`): `(bits_back << 16) + in_progress_length`.
    #[must_use]
    pub fn mark(&self) -> i64 {
        inflate_mark(&self.state)
    }

    /// Number of code-table entries used so far (C `inflateCodesUsed`).
    #[must_use]
    pub fn codes_used(&self) -> u64 {
        inflate_codes_used(&self.state)
    }

    /// Enable or disable check-value (Adler-32/CRC-32) validation at run time
    /// (C `inflateValidate`). Enabling has effect only when a wrapper is in use.
    pub fn validate(&mut self, check: bool) {
        inflate_validate(&mut self.state, i32::from(check));
    }

    /// Decompress from `input` into `output`, advancing the state machine
    /// (the idiomatic front end over the core `inflate` function).
    ///
    /// Returns `(ret, in_consumed, out_produced)`:
    ///
    /// * `ret` — the C return code: [`Z_OK`] (made progress, more to do),
    ///   [`Z_STREAM_END`] (stream complete), [`Z_NEED_DICT`] (call
    ///   [`Inflate::set_dictionary`]), [`Z_BUF_ERROR`] (no progress — supply
    ///   more input or output room), [`Z_DATA_ERROR`] (corrupt input; see
    ///   [`Inflate::msg`]), or [`Z_STREAM_ERROR`]/[`Z_MEM_ERROR`];
    /// * `in_consumed` — bytes read from `input`;
    /// * `out_produced` — bytes written to `output`.
    ///
    /// The accounting fields (`total_in`/`total_out`/`adler`/`msg`/`data_type`)
    /// are updated from the call's outcome. `flush` is one of the seven zlib
    /// flush modes (`Z_NO_FLUSH`, `Z_SYNC_FLUSH`, `Z_FINISH`, `Z_BLOCK`,
    /// `Z_TREES`, …); inflate always writes as much as possible regardless.
    pub fn inflate(&mut self, input: &[u8], output: &mut [u8], flush: i32) -> (i32, usize, usize) {
        // Bare `inflate(...)` resolves to the free core function in this module
        // (methods are never found by bare-path lookup), so this does not recurse.
        let outcome = inflate(&mut self.state, input, output, flush);
        self.total_in += outcome.total_in_delta;
        self.total_out += outcome.total_out_delta;
        self.adler = outcome.adler;
        self.msg = outcome.msg;
        self.data_type = outcome.data_type;
        (outcome.ret, outcome.in_consumed, outcome.out_produced)
    }
}
