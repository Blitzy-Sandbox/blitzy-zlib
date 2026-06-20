//! inflate.rs -- zlib decompression engine (root module).
//!
//! Safe-Rust port of zlib's `inflate.c` (Copyright 1995-2026 Mark Adler),
//! the largest and most intricate translation unit of the INFLATE side. This
//! module owns the full `inflate()` decode state machine, the bit-accumulator
//! helpers, the deferred sliding-window machinery, the gzip / zlib / raw
//! header parsers, the preset-dictionary (`Z_NEED_DICT`) handshake,
//! `inflateSync` error recovery, `inflateCopy`, and the complete public
//! `inflate*` API surface. It also declares and re-exports the inflate
//! submodules.
//!
//! # Format coverage
//!
//! The decoder accepts every RFC 1951 raw-DEFLATE stream that canonical C
//! zlib can produce, plus the two checked framings layered on top of it:
//! RFC 1950 (zlib, Adler-32 trailer) and RFC 1952 (gzip, CRC-32 + ISIZE
//! trailer). The `windowBits` parameter selects among them exactly as in C
//! (see [`inflate_reset2`]): 8..=15 zlib, -8..=-15 raw, 24..=31 gzip, and
//! 40..=47 automatic zlib/gzip detection.
//!
//! # Faithfulness invariants (locked design decisions)
//!
//! * **Pointers become indices.** C's `state->lencode` / `state->distcode` /
//!   `state->next` are `code*` pointers into `state->codes[]`; here they are
//!   `usize` indices into [`InflateState::codes`]. Every table read is
//!   `state.codes[base + offset]` — there are **no raw pointers** in this file.
//! * **`hold` is `u32`.** The bit accumulator matches the portable C
//!   `unsigned long` refill cadence (two bytes when `bits < 15`) that
//!   `inffast.rs` assumes; widening to `u64` would change block-boundary
//!   behaviour and break byte-exact parity.
//! * **Checksum init via literals.** Where C writes
//!   `adler32(0, Z_NULL, 0)` (== 1) or `crc32(0, Z_NULL, 0)` (== 0) to
//!   *initialise* a running check, this module writes the literal `1` / `0`.
//!   The sibling [`crate::checksum`] functions recombine for an *empty* slice
//!   and therefore do **not** reproduce those C init constants; they are only
//!   ever called here with **non-empty** buffers.
//! * **100% safe Rust.** This module contains no raw-pointer or
//!   bounds-elided code at all; the sole core bounds-check-elision site of the
//!   whole crate is [`crate::inflate::fast`]. Every copy here (stored-block
//!   copy, the match back-reference copy from the window or from
//!   already-written output, the sync-buffer scan) uses safe slice operations
//!   and index loops.
//!
//! The line citations in the implementation refer to the C `inflate.c` in the
//! repository root that this module reproduces.

use alloc::{boxed::Box, vec, vec::Vec};

use crate::checksum::adler32;
// `crc32` is only used along gzip-gated paths (the gzip header CRC, the gzip
// trailer, and the gzip branch of `update_check`), so it is imported only when
// the `gzip` feature is enabled — keeping the `no-std`/no-gzip build warning-free.
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{
    DEF_WBITS, Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR, Z_DEFLATED, Z_FINISH, Z_MEM_ERROR, Z_NEED_DICT,
    Z_OK, Z_STREAM_END, Z_STREAM_ERROR, Z_TREES,
};
use crate::error::ZlibError;

// ---------------------------------------------------------------------------
// Submodule declarations (dependency order) and public re-exports.
// ---------------------------------------------------------------------------

/// Huffman decode-table builder (`inftrees.c`): the canonical [`Code`] entry,
/// the [`inflate_table`] constructor, the [`inflate_fixed`] installer, and the
/// `ENOUGH*` sizing constants.
pub mod tables;

/// Internal decode state (`inflate.h`): [`InflateState`] and the
/// [`InflateMode`] state-machine enum.
pub mod state;

/// Fixed (static) Huffman tables (`inffixed.h`): `LENFIX` / `DISTFIX`.
pub mod fixed;

/// The `inflate_fast` hot loop (`inffast.c`) — the sole core bounds-check
/// elision site of the crate (all other inflate code is 100% safe Rust).
pub mod fast;

/// Callback decompression (`infback.c`): the `inflateBack` API.
pub mod back;

pub use back::{
    BackInput, BackOutput, SliceInput, inflate_back, inflate_back_end, inflate_back_init,
};
pub use state::{InflateMode, InflateState};
pub use tables::{Code, CodeType, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS, MAXBITS};

// The table builder and fixed-table installer are `pub(crate)` in `tables`
// (they correspond to C's `ZLIB_INTERNAL` functions, not the public zlib API),
// so they are imported for crate-internal use by the state machine below and
// are NOT re-exported beyond the crate. Crate siblings (e.g. `ffi.rs`) that need
// the builder reach it via `crate::inflate::tables::inflate_table`.
use tables::{inflate_fixed, inflate_table};

#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// ---------------------------------------------------------------------------
// Pure bit-accumulator helpers (faithful ports of the `inflate.c` macros that
// carry no control flow). The flow-control macros (`PULLBYTE`/`NEEDBITS`) are
// defined as *local* `macro_rules!` inside `inflate()` so they can `break`
// the labelled decode loop and reference the function's registers.
// ---------------------------------------------------------------------------

/// Return the low `n` bits of the bit accumulator (`BITS(n)` in C, `n < 16`).
///
/// `hold & ((1 << n) - 1)` — extracts the next `n` accumulated bits without
/// consuming them.
#[inline]
const fn bits_low(hold: u32, n: u32) -> u32 {
    hold & ((1u32 << n) - 1)
}

/// Fold `buf` into the running check value, selecting CRC-32 for gzip streams
/// and Adler-32 for zlib streams — the `UPDATE_CHECK` macro from `inflate.c`
/// (the `#ifdef GUNZIP` form).
///
/// `state.flags` is non-zero once a gzip header has been parsed; otherwise the
/// stream is zlib (or raw) and Adler-32 applies. **Must** be called with a
/// non-empty `buf` (see the module-level checksum-init note).
#[inline]
fn update_check(state: &InflateState, buf: &[u8]) -> u32 {
    #[cfg(feature = "gzip")]
    {
        if state.flags != 0 {
            crc32(state.check, buf)
        } else {
            adler32(state.check, buf)
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        adler32(state.check, buf)
    }
}

// ---------------------------------------------------------------------------
// inflate() result contract.
// ---------------------------------------------------------------------------

/// Everything the FFI shim / idiomatic wrapper needs to reconcile a
/// [`ZStream`](crate::stream::ZStream) after a single [`inflate`] call.
///
/// The core [`inflate`] engine operates on borrowed `input` / `output` slices
/// and an owned [`InflateState`]; it cannot touch the caller's `z_stream`
/// directly, so it reports its effects through this struct.
///
/// * [`in_consumed`](Self::in_consumed) / [`out_produced`](Self::out_produced)
///   advance the caller's `next_in` / `avail_in` and `next_out` / `avail_out`.
/// * [`total_in_delta`](Self::total_in_delta) /
///   [`total_out_delta`](Self::total_out_delta) advance the cumulative
///   `total_in` / `total_out` counters. They normally equal the consumed /
///   produced counts, but are **zero** for the `Z_NEED_DICT` / fatal early
///   returns, faithfully reproducing zlib's accounting (those C paths
///   `RESTORE` the cursors but skip the `total_*` update).
/// * [`ret`](Self::ret) is the C integer return code (`Z_OK` ..
///   `Z_STREAM_END` / `Z_NEED_DICT` / negative errors).
/// * [`adler`](Self::adler) mirrors C's `strm->adler` (the running / final
///   check value, or the preset-dictionary id on `Z_NEED_DICT`).
/// * [`msg`](Self::msg) is `Some` only when this call *set* a new error
///   message; callers persist their previous message otherwise (C never
///   clears `strm->msg` inside `inflate`).
#[derive(Clone, Copy, Debug)]
pub struct InflateOutcome {
    /// Input bytes consumed this call (advances `next_in` / `avail_in`).
    pub in_consumed: usize,
    /// Output bytes produced this call (advances `next_out` / `avail_out`).
    pub out_produced: usize,
    /// C integer return code.
    pub ret: i32,
    /// `strm->data_type` value computed at loop exit.
    pub data_type: i32,
    /// A freshly-set error message, if any (else carry the prior message).
    pub msg: Option<&'static str>,
    /// `strm->adler` — running/final check value (or preset-dict id).
    pub adler: u32,
    /// Amount to add to the cumulative `total_in` (see struct docs).
    pub total_in_delta: u64,
    /// Amount to add to the cumulative `total_out` (see struct docs).
    pub total_out_delta: u64,
}

// ---------------------------------------------------------------------------
// State validation + reset / init family (`inflate.c` L88-236).
//
// These operate on `&mut InflateState`. The public C `strm->adler` is held by
// the caller (the FFI shim's `z_stream` or the idiomatic `Inflate` wrapper),
// so the reset functions take `adler: &mut u32` to reproduce
// `inflateResetKeep`'s `strm->adler = state->wrap & 1` assignment. The
// caller is responsible for resetting `total_in`/`total_out`/`msg`/`data_type`
// (the `z_stream`-level fields not stored in `InflateState`).
// ---------------------------------------------------------------------------

/// Equivalent of C `inflateStateCheck` (`inflate.c` L88-98).
///
/// In C this guards against a null stream, missing allocator hooks, a null or
/// foreign `state` pointer, and a `mode` outside `HEAD..=SYNC`. With a typed,
/// owned [`InflateState`] those structural conditions are guaranteed by
/// construction (the allocator / null / back-pointer checks live in
/// `src/ffi.rs`), and `mode` is always a valid [`InflateMode`] variant — so
/// the only residual check is vacuously satisfied. Retained as documentation
/// of the mapping; always returns `false` ("state OK").
///
/// Consumed by `src/ffi.rs` (the C-ABI shim, created separately); it appears
/// unused when the crate is built without that sibling, hence `allow(dead_code)`.
#[inline]
#[allow(dead_code)]
pub(crate) fn state_check(state: &InflateState) -> bool {
    let _ = state;
    false
}

/// Port of C `inflateResetKeep` (`inflate.c` L100-123).
///
/// Resets the per-stream decode state **without** clearing the window
/// bookkeeping (`wsize`/`whave`/`wnext`) or reallocating buffers. Returns
/// `Z_OK`.
pub(crate) fn inflate_reset_keep(state: &mut InflateState, adler: &mut u32) -> i32 {
    state.total = 0;
    // `strm->total_in = strm->total_out = 0` and `strm->msg = Z_NULL`,
    // `strm->data_type = 0` are the caller's responsibility (z_stream-level).
    if state.wrap != 0 {
        // "to support ill-conceived Java test suite"
        *adler = (state.wrap & 1) as u32;
    }
    state.mode = InflateMode::Head;
    state.last = false;
    state.havedict = false;
    state.flags = -1;
    state.dmax = 32768;
    #[cfg(feature = "gzip")]
    {
        state.head = None;
    }
    state.hold = 0;
    state.bits = 0;
    // `lencode = distcode = next = codes` -> all index 0.
    state.lencode = 0;
    state.distcode = 0;
    state.next = 0;
    state.sane = true;
    state.back = -1;
    Z_OK
}

/// Port of C `inflateReset` (`inflate.c` L125-134): clears the window
/// bookkeeping, then defers to [`inflate_reset_keep`].
pub(crate) fn inflate_reset(state: &mut InflateState, adler: &mut u32) -> i32 {
    state.wsize = 0;
    state.whave = 0;
    state.wnext = 0;
    inflate_reset_keep(state, adler)
}

/// Port of C `inflateReset2` (`inflate.c` L136-171) — the `windowBits`
/// overloading that selects the stream framing.
///
/// `window_bits` is interpreted exactly as in C:
///
/// | range      | meaning                | resulting `wrap`            |
/// |------------|------------------------|-----------------------------|
/// | `-15..=-8` | raw DEFLATE            | `0`                         |
/// | `8..=15`   | zlib (Adler trailer)   | `5` (bit0 = zlib check)     |
/// | `24..=31`  | gzip (CRC + ISIZE)     | `6` (bit1 = gzip)           |
/// | `40..=47`  | auto-detect zlib/gzip  | `7` (bits0+1)               |
///
/// `wrap` bit meanings: bit0 = expect/produce a zlib Adler check, bit1 = gzip,
/// bit2 (== 4) = *validate* the check (set later by [`inflate_validate`]).
/// `flags == -1` means "header not yet seen". When the `gzip` feature is
/// disabled the `& 15` masking is compiled out, so the gzip/auto ranges fail
/// the window-size validation below (exactly mirroring a `GUNZIP`-undefined
/// C build).
pub(crate) fn inflate_reset2(
    state: &mut InflateState,
    mut window_bits: i32,
    adler: &mut u32,
) -> i32 {
    let wrap: i32;
    if window_bits < 0 {
        if window_bits < -15 {
            return Z_STREAM_ERROR;
        }
        wrap = 0;
        window_bits = -window_bits;
    } else {
        wrap = (window_bits >> 4) + 5;
        #[cfg(feature = "gzip")]
        {
            if window_bits < 48 {
                window_bits &= 15;
            }
        }
    }

    // C: `if (windowBits && (windowBits < 8 || windowBits > 15))`.
    if window_bits != 0 && !(8..=15).contains(&window_bits) {
        return Z_STREAM_ERROR;
    }
    // Free the window if a different size was previously configured (an empty
    // `Vec` is the "not yet allocated" sentinel, analogous to C `Z_NULL`).
    if !state.window.is_empty() && state.wbits != window_bits as u32 {
        state.window = Vec::new();
    }

    state.wrap = wrap;
    state.wbits = window_bits as u32;
    inflate_reset(state, adler)
}

/// Port of the engine portion of C `inflateInit2_` (`inflate.c` L173-212).
///
/// Allocates a fresh [`InflateState`] (its `codes` table is already sized to
/// [`ENOUGH`]), configures it via [`inflate_reset2`], and returns it boxed.
/// The C `version` / `stream_size` ABI check belongs to `src/ffi.rs`; this
/// idiomatic constructor takes only `window_bits`. On an invalid `window_bits`
/// the C integer error code is returned via `Err`.
pub(crate) fn inflate_init2(window_bits: i32) -> Result<Box<InflateState>, i32> {
    let mut state = Box::new(InflateState::new());
    state.mode = InflateMode::Head; // to pass the state test in inflate_reset2
    // `state.window` is already an empty `Vec` (== C `Z_NULL`).
    let mut adler = 0u32;
    let ret = inflate_reset2(&mut state, window_bits, &mut adler);
    if ret != Z_OK {
        return Err(ret);
    }
    Ok(state)
}

/// Port of C `inflateInit_` (`inflate.c` L214-217): initialise with the
/// default window size [`DEF_WBITS`].
pub(crate) fn inflate_init() -> Result<Box<InflateState>, i32> {
    inflate_init2(DEF_WBITS)
}

/// Port of C `inflatePrime` (`inflate.c` L219-236): inject `bits` bits of
/// `value` into the accumulator (or flush it when `bits < 0`).
pub(crate) fn inflate_prime(state: &mut InflateState, bits: i32, value: i32) -> i32 {
    if bits == 0 {
        return Z_OK;
    }
    if bits < 0 {
        state.hold = 0;
        state.bits = 0;
        return Z_OK;
    }
    if bits > 16 || state.bits + bits as u32 > 32 {
        return Z_STREAM_ERROR;
    }
    let value = (value & ((1i32 << bits) - 1)) as u32;
    state.hold += value << state.bits;
    state.bits += bits as u32;
    Z_OK
}

// ---------------------------------------------------------------------------
// updatewindow (`inflate.c` L252-296) — the deferred sliding window.
// ---------------------------------------------------------------------------

/// Fold the last `copy` bytes of just-written output into the circular
/// sliding window, allocating the window lazily on first use. Faithful port of
/// C `updatewindow` (`inflate.c` L252-296).
///
/// `output[..end]` is the region written so far this call (C passes
/// `strm->next_out`, i.e. the cursor one past the last byte, as `end`), and
/// `copy` is the number of trailing bytes to fold in (C `out - avail_out`).
/// The window is also reused by [`inflate_set_dictionary`] to seed dictionary
/// data, by passing the dictionary slice as `output` with `end == copy ==
/// dict.len()`.
///
/// Returns `0` always: in C a non-zero return signals `ZALLOC` failure
/// (mapped by callers to `Z_MEM_ERROR` / the `Mem` mode), but Rust's `vec!`
/// aborts on allocation failure rather than returning, so the error path is
/// unreachable here. The `-> i32` shape and the callers' checks are retained
/// for faithfulness.
fn update_window(state: &mut InflateState, output: &[u8], end: usize, mut copy: usize) -> i32 {
    // Allocate the window on first use (C: `ZALLOC(1U << wbits)`).
    if state.window.is_empty() {
        state.window = vec![0u8; 1usize << state.wbits];
    }

    // Initialise the window bookkeeping if not yet in use.
    if state.wsize == 0 {
        state.wsize = 1u32 << state.wbits;
        state.wnext = 0;
        state.whave = 0;
    }

    let wsize = state.wsize as usize;
    if copy >= wsize {
        // The new output alone fills (or overfills) the window: keep its last
        // `wsize` bytes.
        state.window[..wsize].copy_from_slice(&output[end - wsize..end]);
        state.wnext = 0;
        state.whave = wsize as u32;
    } else {
        let wnext = state.wnext as usize;
        let mut dist = wsize - wnext;
        if dist > copy {
            dist = copy;
        }
        // First span: from the current write cursor to the end of the window.
        state.window[wnext..wnext + dist].copy_from_slice(&output[end - copy..end - copy + dist]);
        copy -= dist;
        if copy > 0 {
            // Wrap-around: remaining bytes go to the front of the window.
            state.window[..copy].copy_from_slice(&output[end - copy..end]);
            state.wnext = copy as u32;
            state.whave = wsize as u32;
        } else {
            let mut new_wnext = wnext + dist;
            if new_wnext == wsize {
                new_wnext = 0;
            }
            state.wnext = new_wnext as u32;
            if (state.whave as usize) < wsize {
                state.whave += dist as u32;
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// inflate() — the decode state machine (`inflate.c` L474-1153).
//
// The C `for (;;) switch (state->mode)` becomes `'inf: loop { match state.mode
// { .. } }`. C `goto inf_leave` becomes `break 'inf`; C `switch` fall-through
// becomes `state.mode = Next; continue 'inf;` (one extra loop trip, identical
// semantics); C immediate `return` becomes `return` / `leave_now!`. The bit
// registers `hold`/`bits`/`have`/`left`/`next`/`put` are kept in locals (the
// C LOAD()) and written back at `inf_leave` (the C RESTORE()).
// ---------------------------------------------------------------------------

/// Decompress from `input` into `output`, advancing the decode `state`.
///
/// This is the safe-Rust port of C `inflate()`. It returns an
/// [`InflateOutcome`] describing how much input/output was consumed/produced,
/// the C return code, the running check value, and any error message — the
/// caller (FFI shim or [`Inflate`] wrapper) applies those to its `z_stream`.
///
/// The C null-pointer entry guards (`next_out == Z_NULL`, `next_in == Z_NULL`
/// with `avail_in != 0` → `Z_STREAM_ERROR`) live in `src/ffi.rs`; with
/// borrowed slices they are structurally satisfied here, and empty slices are
/// handled naturally as `have == 0` / `left == 0`.
pub(crate) fn inflate(
    state: &mut InflateState,
    input: &[u8],
    output: &mut [u8],
    flush: i32,
) -> InflateOutcome {
    use InflateMode::*;

    // C: `if (state->mode == TYPE) state->mode = TYPEDO;` — on re-entry, skip
    // the Z_BLOCK/Z_TREES early return that TYPE would otherwise take.
    if state.mode == Type {
        state.mode = Typedo;
    }

    // LOAD(): bring the stream registers into locals for the decode loop.
    let mut next: usize = 0; // input cursor  (C next_in offset)
    let mut put: usize = 0; // output cursor (C next_out offset)
    let mut have: usize = input.len(); // C avail_in
    let mut left: usize = output.len(); // C avail_out
    let mut hold: u32 = state.hold;
    let mut bits: u32 = state.bits;

    let in_start: usize = have; // C `in`  (avail_in saved at entry)
    let out_start: usize = left; // C `out` baseline (avail_out at entry)
    // C mutable `out`: the avail_out baseline from which the *next* check fold
    // measures production. Reset to `left` inside CHECK so `inf_leave` never
    // double-counts already-folded bytes.
    let mut out_used: usize = out_start;

    // Reconstruct C `strm->adler` at entry: during header parsing on a wrapped
    // stream (no header seen yet) it equals `wrap & 1`; afterwards it tracks
    // the running check value `state.check`.
    let mut adler: u32 = if state.wrap != 0 && state.flags == -1 {
        (state.wrap & 1) as u32
    } else {
        state.check
    };

    let mut ret: i32 = Z_OK;
    let mut msg: Option<&'static str> = None;

    // ----- direct (non-inf_leave) return, mirroring C's bare `return code;`
    //       in the MEM / SYNC / default arms (and, without gzip, the
    //       unreachable header arms): saves the accumulator, advances cursors
    //       but not the cumulative totals. Defined before the loop because it
    //       uses `return` (not `break 'inf`), so it needs no in-scope label.
    macro_rules! leave_now {
        ($code:expr) => {{
            state.hold = hold;
            state.bits = bits;
            return InflateOutcome {
                in_consumed: in_start - have,
                out_produced: out_start - left,
                ret: $code,
                data_type: 0,
                msg,
                adler,
                total_in_delta: 0,
                total_out_delta: 0,
            };
        }};
    }

    'inf: loop {
        // ----- bit-accumulator flow-control macros (inflate.c L353-390) -----
        // These local `macro_rules!` MUST be defined INSIDE the `'inf` loop:
        // label hygiene resolves `break 'inf` at the macro's *definition* site,
        // so `'inf` must be in scope here (proven idiom). They also read/write
        // the function registers `hold`/`bits`/`have`/`next`/`input`.
        macro_rules! pullbyte {
            () => {{
                if have == 0 {
                    break 'inf;
                }
                have -= 1;
                hold += (input[next] as u32) << bits;
                next += 1;
                bits += 8;
            }};
        }
        macro_rules! needbits {
            ($n:expr) => {{
                while bits < ($n) as u32 {
                    pullbyte!();
                }
            }};
        }
        macro_rules! dropbits {
            ($n:expr) => {{
                hold >>= ($n) as u32;
                bits -= ($n) as u32;
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

        // ----- gzip header CRC helpers (inflate.c L310-324) -----
        #[cfg(feature = "gzip")]
        macro_rules! crc2 {
            ($word:expr) => {{
                let w: u32 = $word;
                let hbuf = [w as u8, (w >> 8) as u8];
                state.check = crc32(state.check, &hbuf);
            }};
        }
        #[cfg(feature = "gzip")]
        macro_rules! crc4 {
            ($word:expr) => {{
                let w: u32 = $word;
                let hbuf = [w as u8, (w >> 8) as u8, (w >> 16) as u8, (w >> 24) as u8];
                state.check = crc32(state.check, &hbuf);
            }};
        }

        // ----- Huffman decode helpers: pull until the indexed entry has enough
        //       bits, then return it. `decode_here!` is the first-level lookup;
        //       `decode_sub!` is the second-level (sub-table) lookup. Both can
        //       `break 'inf` (via `pullbyte!`) to fetch more input. -----
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
        macro_rules! decode_sub {
            ($base:expr, $last:expr) => {{
                let base: usize = $base;
                let last: Code = $last;
                loop {
                    let idx = last.val as usize
                        + (bits_low(hold, last.bits as u32 + last.op as u32) >> last.bits as u32)
                            as usize;
                    let h = state.codes[base + idx];
                    if (last.bits as u32 + h.bits as u32) <= bits {
                        break h;
                    }
                    pullbyte!();
                }
            }};
        }

        match state.mode {
            // ===== Header: zlib / gzip / raw detection (L506-553) =====
            Head => {
                if state.wrap == 0 {
                    state.mode = Typedo;
                    continue 'inf;
                }
                needbits!(16);
                #[cfg(feature = "gzip")]
                {
                    if (state.wrap & 2) != 0 && hold == 0x8b1f {
                        // gzip magic 0x1f 0x8b.
                        if state.wbits == 0 {
                            state.wbits = 15;
                        }
                        state.check = 0; // crc32(0, Z_NULL, 0)
                        crc2!(hold);
                        initbits!();
                        state.mode = Flags;
                        continue 'inf;
                    }
                    // Not gzip: C would set `head->done = -1` here; the
                    // simplified GzHeader has no tri-state `done`, so this is
                    // left implicit (done stays false until HCRC).
                }
                // zlib header (or raw not reaching here because wrap==0 above).
                if (state.wrap & 1) == 0 || (((hold & 0xff) << 8) + (hold >> 8)) % 31 != 0 {
                    msg = Some("incorrect header check");
                    state.mode = Bad;
                    continue 'inf;
                }
                if (hold & 0x0f) != Z_DEFLATED as u32 {
                    msg = Some("unknown compression method");
                    state.mode = Bad;
                    continue 'inf;
                }
                dropbits!(4);
                let len = (hold & 0x0f) + 8;
                if state.wbits == 0 {
                    state.wbits = len;
                }
                if len > 15 || len > state.wbits {
                    msg = Some("invalid window size");
                    state.mode = Bad;
                    continue 'inf;
                }
                state.dmax = 1u32 << len;
                state.flags = 0; // indicate zlib header
                state.check = 1; // adler32(0, Z_NULL, 0)
                adler = 1;
                state.mode = if (hold & 0x200) != 0 { Dictid } else { Type };
                initbits!();
                continue 'inf;
            }

            // ===== gzip header field chain (L555-692) =====
            #[cfg(feature = "gzip")]
            Flags => {
                needbits!(16);
                state.flags = hold as i32;
                if (state.flags & 0xff) != Z_DEFLATED {
                    msg = Some("unknown compression method");
                    state.mode = Bad;
                    continue 'inf;
                }
                if (state.flags & 0xe000) != 0 {
                    msg = Some("unknown header flags set");
                    state.mode = Bad;
                    continue 'inf;
                }
                if let Some(head) = state.head.as_mut() {
                    head.text = ((hold >> 8) & 1) != 0;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2!(hold);
                }
                initbits!();
                state.mode = Time;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Time => {
                needbits!(32);
                if let Some(head) = state.head.as_mut() {
                    head.time = hold;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc4!(hold);
                }
                initbits!();
                state.mode = Os;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Os => {
                needbits!(16);
                if let Some(head) = state.head.as_mut() {
                    head.xflags = (hold & 0xff) as i32;
                    head.os = (hold >> 8) as i32;
                }
                if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                    crc2!(hold);
                }
                initbits!();
                state.mode = Exlen;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Exlen => {
                if (state.flags & 0x0400) != 0 {
                    needbits!(16);
                    state.length = hold;
                    if let Some(head) = state.head.as_mut() {
                        // C records `head->extra_len`; the growing capture Vec
                        // serves that role here.
                        head.extra = Some(Vec::new());
                    }
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        crc2!(hold);
                    }
                    initbits!();
                } else if let Some(head) = state.head.as_mut() {
                    head.extra = None;
                }
                state.mode = Extra;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Extra => {
                if (state.flags & 0x0400) != 0 {
                    let mut copy = state.length as usize;
                    if copy > have {
                        copy = have;
                    }
                    if copy > 0 {
                        if let Some(head) = state.head.as_mut() {
                            if let Some(extra) = head.extra.as_mut() {
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
                state.mode = Name;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Name => {
                if (state.flags & 0x0800) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    // C do-while: read each byte, store non-NUL bytes into the
                    // (unbounded) name Vec, continue while `len && copy < have`.
                    // The terminating NUL is consumed (and CRC'd) but not stored
                    // — the idiomatic GzHeader holds the name without a NUL.
                    let mut copy = 0usize;
                    let last_byte = loop {
                        let b = input[next + copy];
                        copy += 1;
                        if b != 0 {
                            if let Some(head) = state.head.as_mut() {
                                head.name.get_or_insert_with(Vec::new).push(b);
                            }
                        }
                        if b == 0 || copy >= have {
                            break b;
                        }
                    };
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &input[next..next + copy]);
                    }
                    have -= copy;
                    next += copy;
                    if last_byte != 0 {
                        break 'inf;
                    }
                } else if let Some(head) = state.head.as_mut() {
                    head.name = None;
                }
                state.length = 0;
                state.mode = Comment;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Comment => {
                if (state.flags & 0x1000) != 0 {
                    if have == 0 {
                        break 'inf;
                    }
                    // Same do-while structure as NAME, into the comment Vec.
                    let mut copy = 0usize;
                    let last_byte = loop {
                        let b = input[next + copy];
                        copy += 1;
                        if b != 0 {
                            if let Some(head) = state.head.as_mut() {
                                head.comment.get_or_insert_with(Vec::new).push(b);
                            }
                        }
                        if b == 0 || copy >= have {
                            break b;
                        }
                    };
                    if (state.flags & 0x0200) != 0 && (state.wrap & 4) != 0 {
                        state.check = crc32(state.check, &input[next..next + copy]);
                    }
                    have -= copy;
                    next += copy;
                    if last_byte != 0 {
                        break 'inf;
                    }
                } else if let Some(head) = state.head.as_mut() {
                    head.comment = None;
                }
                state.mode = Hcrc;
                continue 'inf;
            }
            #[cfg(feature = "gzip")]
            Hcrc => {
                if (state.flags & 0x0200) != 0 {
                    needbits!(16);
                    if (state.wrap & 4) != 0 && hold != (state.check & 0xffff) {
                        msg = Some("header crc mismatch");
                        state.mode = Bad;
                        continue 'inf;
                    }
                    initbits!();
                }
                if let Some(head) = state.head.as_mut() {
                    head.hcrc = ((state.flags >> 9) & 1) != 0;
                    head.done = true;
                }
                state.check = 0; // crc32(0, Z_NULL, 0) for the data that follows
                adler = 0;
                state.mode = Type;
                continue 'inf;
            }

            // Without the gzip feature, the gzip-only modes are unreachable
            // (wrap never gains bit 1, and CHECK routes straight to DONE). Keep
            // the match exhaustive over all 32 InflateMode variants.
            #[cfg(not(feature = "gzip"))]
            Flags | Time | Os | Exlen | Extra | Name | Comment | Hcrc | Length => {
                leave_now!(Z_STREAM_ERROR);
            }

            // ===== zlib preset dictionary (L694-707) =====
            Dictid => {
                needbits!(32);
                state.check = hold.swap_bytes(); // ZSWAP32(hold)
                adler = state.check;
                initbits!();
                state.mode = Dict;
                continue 'inf;
            }
            Dict => {
                if !state.havedict {
                    // The Z_NEED_DICT handshake: report it (the inf_leave path
                    // preserves `adler` == the dictionary id because no output
                    // was produced) and await `inflate_set_dictionary`.
                    ret = Z_NEED_DICT;
                    break 'inf;
                }
                state.check = 1; // adler32(0, Z_NULL, 0)
                adler = 1;
                state.mode = Type;
                continue 'inf;
            }

            // ===== block type dispatch (L708-746) =====
            Type => {
                if flush == Z_BLOCK || flush == Z_TREES {
                    break 'inf;
                }
                state.mode = Typedo;
                continue 'inf;
            }
            Typedo => {
                if state.last {
                    bytebits!();
                    state.mode = Check;
                    continue 'inf;
                }
                needbits!(3);
                state.last = bits_low(hold, 1) != 0;
                dropbits!(1);
                match bits_low(hold, 2) {
                    0 => {
                        // stored block
                        state.mode = Stored;
                    }
                    1 => {
                        // fixed Huffman block: install the static tables.
                        // NOTE: inflate_fixed must NOT advance state.next, so
                        // inflateCodesUsed stays unchanged (matches C).
                        let ft = inflate_fixed(&mut state.codes);
                        state.lencode = ft.lencode;
                        state.lenbits = ft.lenbits;
                        state.distcode = ft.distcode;
                        state.distbits = ft.distbits;
                        state.mode = LenBegin;
                        if flush == Z_TREES {
                            dropbits!(2);
                            break 'inf;
                        }
                    }
                    2 => {
                        // dynamic Huffman block
                        state.mode = Table;
                    }
                    _ => {
                        // bits_low(hold,2) == 3
                        msg = Some("invalid block type");
                        state.mode = Bad;
                    }
                }
                dropbits!(2);
                continue 'inf;
            }

            // ===== stored block (L747-781) =====
            Stored => {
                bytebits!(); // go to byte boundary
                needbits!(32);
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    msg = Some("invalid stored block lengths");
                    state.mode = Bad;
                    continue 'inf;
                }
                state.length = hold & 0xffff;
                initbits!();
                state.mode = CopyBegin;
                if flush == Z_TREES {
                    break 'inf;
                }
                continue 'inf;
            }
            CopyBegin => {
                state.mode = Copy;
                continue 'inf;
            }
            Copy => {
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
                    // Disjoint copy (input vs output): always safe.
                    output[put..put + copy].copy_from_slice(&input[next..next + copy]);
                    have -= copy;
                    next += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                    continue 'inf;
                }
                state.mode = Type;
                continue 'inf;
            }

            // ===== dynamic block table sizes (L782-801) =====
            Table => {
                needbits!(14);
                state.nlen = bits_low(hold, 5) + 257;
                dropbits!(5);
                state.ndist = bits_low(hold, 5) + 1;
                dropbits!(5);
                state.ncode = bits_low(hold, 4) + 4;
                dropbits!(4);
                if state.nlen > 286 || state.ndist > 30 {
                    msg = Some("too many length or distance symbols");
                    state.mode = Bad;
                    continue 'inf;
                }
                state.have = 0;
                state.mode = Lenlens;
                continue 'inf;
            }

            // ===== code-length code lengths (L802-823) =====
            Lenlens => {
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
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
                state.lenbits = 7;
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
                    state.mode = Bad;
                    continue 'inf;
                }
                state.have = 0;
                state.mode = Codelens;
                continue 'inf;
            }

            // ===== literal/length and distance code lengths (L824-910) =====
            Codelens => {
                while state.have < state.nlen + state.ndist {
                    let here = decode_here!(state.lencode, state.lenbits);
                    if here.val < 16 {
                        dropbits!(here.bits as u32);
                        state.lens[state.have as usize] = here.val;
                        state.have += 1;
                    } else {
                        let len_val: u16;
                        let mut copy: u32;
                        if here.val == 16 {
                            needbits!(here.bits as u32 + 2);
                            dropbits!(here.bits as u32);
                            if state.have == 0 {
                                msg = Some("invalid bit length repeat");
                                state.mode = Bad;
                                break;
                            }
                            len_val = state.lens[state.have as usize - 1];
                            copy = 3 + bits_low(hold, 2);
                            dropbits!(2);
                        } else if here.val == 17 {
                            needbits!(here.bits as u32 + 3);
                            dropbits!(here.bits as u32);
                            len_val = 0;
                            copy = 3 + bits_low(hold, 3);
                            dropbits!(3);
                        } else {
                            needbits!(here.bits as u32 + 7);
                            dropbits!(here.bits as u32);
                            len_val = 0;
                            copy = 11 + bits_low(hold, 7);
                            dropbits!(7);
                        }
                        if state.have + copy > state.nlen + state.ndist {
                            msg = Some("invalid bit length repeat");
                            state.mode = Bad;
                            break;
                        }
                        while copy > 0 {
                            state.lens[state.have as usize] = len_val;
                            state.have += 1;
                            copy -= 1;
                        }
                    }
                }

                // Handle error breaks out of the while loop above.
                if state.mode == Bad {
                    continue 'inf;
                }

                // Check for the end-of-block code (there must be one).
                if state.lens[256] == 0 {
                    msg = Some("invalid code -- missing end-of-block");
                    state.mode = Bad;
                    continue 'inf;
                }

                // Build the literal/length and distance tables. DO NOT change
                // lenbits (9) / distbits (6) — the ENOUGH sizing in inftrees.rs
                // depends on exactly those values (inflate.c L885-887).
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
                    state.mode = Bad;
                    continue 'inf;
                }
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
                    state.mode = Bad;
                    continue 'inf;
                }
                state.mode = LenBegin;
                if flush == Z_TREES {
                    break 'inf;
                }
                continue 'inf;
            }

            // ===== literal/length decode (L911-963) =====
            LenBegin => {
                state.mode = Len;
                continue 'inf;
            }
            Len => {
                // Fast path: enough guaranteed input/output for the unrolled
                // inner loop (the sole core bounds-elision site, in fast.rs).
                if have >= 6 && left >= 258 {
                    // RESTORE the accumulator for the fast loop.
                    state.hold = hold;
                    state.bits = bits;
                    fast::inflate_fast(input, &mut next, output, &mut put, state);
                    // LOAD back.
                    hold = state.hold;
                    bits = state.bits;
                    have = input.len() - next;
                    left = output.len() - put;
                    if state.mode == Type {
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
                    state.mode = Lit;
                    continue 'inf;
                }
                if (here.op & 32) != 0 {
                    // end of block
                    state.back = -1;
                    state.mode = Type;
                    continue 'inf;
                }
                if (here.op & 64) != 0 {
                    msg = Some("invalid literal/length code");
                    state.mode = Bad;
                    continue 'inf;
                }
                state.extra = (here.op & 15) as u32;
                state.mode = Lenext;
                continue 'inf;
            }
            Lenext => {
                if state.extra != 0 {
                    needbits!(state.extra);
                    state.length += bits_low(hold, state.extra);
                    dropbits!(state.extra);
                    state.back += state.extra as i32;
                }
                state.was = state.length;
                state.mode = Dist;
                continue 'inf;
            }

            // ===== distance decode (L975-1019) =====
            Dist => {
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
                    state.mode = Bad;
                    continue 'inf;
                }
                state.offset = here.val as u32;
                state.extra = (here.op & 15) as u32;
                state.mode = Distext;
                continue 'inf;
            }
            Distext => {
                if state.extra != 0 {
                    needbits!(state.extra);
                    state.offset += bits_low(hold, state.extra);
                    dropbits!(state.extra);
                    state.back += state.extra as i32;
                }
                // INFLATE_STRICT (offset > dmax check) is OFF by default in
                // this repo, matching fast.rs — omit it.
                state.mode = Match;
                continue 'inf;
            }

            // ===== copy match from window or output (L1020-1065) =====
            Match => {
                if left == 0 {
                    break 'inf;
                }
                // C `copy = out - left`; with the put/left invariant this is
                // exactly `put` (bytes produced so far this call).
                let mut copy: usize = put;
                let from_window = state.offset as usize > copy;
                let from_idx: usize;
                if from_window {
                    copy = state.offset as usize - copy; // back-distance into window
                    // A back-distance reaching past what the window holds is
                    // invalid. C guards this with `if (sane)` and an
                    // `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` zero-fill
                    // `else`; that compile option is OFF here, so `sane` is
                    // always true and the (collapsed) check always fails fast.
                    if copy > state.whave as usize && state.sane {
                        msg = Some("invalid distance too far back");
                        state.mode = Bad;
                        continue 'inf;
                    }
                    let wnext = state.wnext as usize;
                    let wsize = state.wsize as usize;
                    if copy > wnext {
                        copy -= wnext;
                        from_idx = wsize - copy;
                    } else {
                        from_idx = wnext - copy;
                    }
                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                } else {
                    from_idx = copy - state.offset as usize; // put - offset
                    copy = state.length as usize;
                }
                if copy > left {
                    copy = left;
                }
                left -= copy;
                state.length -= copy as u32;
                if from_window {
                    // Window and output are DISJOINT arrays, so a slice copy is
                    // both safe and faithful (C's window-case byte loop is
                    // equivalent to a `memcpy` over non-overlapping memory).
                    output[put..put + copy]
                        .copy_from_slice(&state.window[from_idx..from_idx + copy]);
                    put += copy;
                } else {
                    // Copy from already-written output: overlapping ranges, so
                    // this MUST be a byte-by-byte index loop (LZ77 self-copy).
                    let off = state.offset as usize;
                    for _ in 0..copy {
                        let b = output[put - off];
                        output[put] = b;
                        put += 1;
                    }
                }
                if state.length == 0 {
                    state.mode = Len;
                }
                continue 'inf;
            }

            // ===== emit a literal (L1066-1071) =====
            Lit => {
                if left == 0 {
                    break 'inf;
                }
                output[put] = state.length as u8;
                put += 1;
                left -= 1;
                state.mode = Len;
                continue 'inf;
            }

            // ===== trailer: data check (L1072-1096) =====
            Check => {
                if state.wrap != 0 {
                    needbits!(32);
                    let produced = out_used - left; // C: out -= left
                    state.total += produced as u64;
                    if (state.wrap & 4) != 0 && produced != 0 {
                        let start = put - produced;
                        state.check = update_check(state, &output[start..put]);
                        adler = state.check;
                    }
                    out_used = left; // C: out = left (reset the fold baseline)
                    let want = {
                        #[cfg(feature = "gzip")]
                        {
                            if state.flags != 0 {
                                hold
                            } else {
                                hold.swap_bytes()
                            }
                        }
                        #[cfg(not(feature = "gzip"))]
                        {
                            hold.swap_bytes()
                        }
                    };
                    if (state.wrap & 4) != 0 && want != state.check {
                        msg = Some("incorrect data check");
                        state.mode = Bad;
                        continue 'inf;
                    }
                    initbits!();
                }
                #[cfg(feature = "gzip")]
                {
                    state.mode = Length;
                }
                #[cfg(not(feature = "gzip"))]
                {
                    state.mode = Done;
                }
                continue 'inf;
            }

            // ===== trailer: gzip ISIZE length check (L1097-1108) =====
            #[cfg(feature = "gzip")]
            Length => {
                if state.wrap != 0 && state.flags != 0 {
                    needbits!(32);
                    if (state.wrap & 4) != 0 && hold != (state.total & 0xffffffff) as u32 {
                        msg = Some("incorrect length check");
                        state.mode = Bad;
                        continue 'inf;
                    }
                    initbits!();
                }
                state.mode = Done;
                continue 'inf;
            }

            // ===== terminal states (L1111-1122) =====
            Done => {
                ret = Z_STREAM_END;
                break 'inf;
            }
            Bad => {
                ret = Z_DATA_ERROR;
                break 'inf;
            }
            Mem => {
                // C `return Z_MEM_ERROR;` — non-recoverable, no inf_leave.
                leave_now!(Z_MEM_ERROR);
            }
            Sync => {
                // C `default: return Z_STREAM_ERROR;`
                leave_now!(Z_STREAM_ERROR);
            }
        }
    }

    // ===== inf_leave (L1131-1152) =====
    // RESTORE the accumulator.
    state.hold = hold;
    state.bits = bits;

    // Deferred window update: fold the bytes produced since the last check
    // reset into the sliding window (creating it if needed).
    let out_seg = out_used - left; // C inf_leave `out` (segment since last reset)
    if state.wsize != 0
        || (out_used != left
            && (state.mode as u32) < (InflateMode::Bad as u32)
            && ((state.mode as u32) < (InflateMode::Check as u32) || flush != Z_FINISH))
    {
        // OOM aborts inside `vec!` rather than returning, so this never yields
        // the C `Z_MEM_ERROR`/`Mem` path; see `update_window`.
        update_window(state, output, put, out_seg);
    }

    let in_consumed = in_start - have;
    let out_produced = out_start - left;

    // Accumulate the internal total (C `state->total`) and fold the trailing
    // segment into the running check exactly once.
    state.total += out_seg as u64;
    if (state.wrap & 4) != 0 && out_seg != 0 {
        let start = put - out_seg;
        state.check = update_check(state, &output[start..put]);
        adler = state.check;
    }

    // strm->data_type (L1147-1149).
    let data_type = bits as i32
        + if state.last { 64 } else { 0 }
        + if state.mode == InflateMode::Type {
            128
        } else {
            0
        }
        + if state.mode == InflateMode::LenBegin || state.mode == InflateMode::CopyBegin {
            256
        } else {
            0
        };

    // No-progress buffer error (L1150-1151).
    if ((in_consumed == 0 && out_seg == 0) || flush == Z_FINISH) && ret == Z_OK {
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
// Post-inflate API functions (`inflate.c` L1155-1413).
//
// These are the `pub(crate)` free functions that `src/ffi.rs` wraps to mint the
// exact C symbol names. They operate on `&mut InflateState` (the FFI shim owns
// the `Box<InflateState>` behind the raw `z_stream.state` pointer; the
// idiomatic `Inflate` wrapper below owns it directly). The C
// `inflateStateCheck` null/allocator guards live in `ffi.rs`; here the typed
// `&InflateState` makes them structurally unnecessary (see [`state_check`]).
// ===========================================================================

/// Port of C `inflateEnd` (`inflate.c` L1155-1165).
///
/// In C this frees `state->window` and `strm->state`. In Rust both are owned
/// (`window: Vec<u8>`, the state is a `Box<InflateState>`), so dropping the box
/// reclaims everything deterministically — there is no manual free and no leak.
/// Callers (the FFI shim / the [`Inflate`] wrapper) simply drop the box; this
/// helper makes the intent explicit and always reports success.
///
/// The idiomatic [`Inflate`] wrapper relies on `Drop` instead, so this is only
/// invoked from `src/ffi.rs` (created separately) — hence `allow(dead_code)`.
#[allow(dead_code)]
pub(crate) fn inflate_end(state: Box<InflateState>) -> i32 {
    drop(state);
    Z_OK
}

/// Port of C `inflateGetDictionary` (`inflate.c` L1167-1185).
///
/// Copies the sliding-window contents into `dictionary` in chronological order
/// — the older bytes (`window[wnext..whave]`) first, then the newer bytes
/// (`window[..wnext]`) — and returns the number of valid window bytes
/// (`whave`). When `dictionary` is shorter than `whave` the copy is skipped
/// (the caller is expected to first query the length with an empty slice, then
/// allocate `whave` bytes), but the required length is still returned.
pub(crate) fn inflate_get_dictionary(state: &InflateState, dictionary: &mut [u8]) -> usize {
    let whave = state.whave as usize;
    let wnext = state.wnext as usize;
    if whave != 0 && dictionary.len() >= whave {
        let older = whave - wnext; // window[wnext..whave]
        dictionary[..older].copy_from_slice(&state.window[wnext..whave]);
        dictionary[older..older + wnext].copy_from_slice(&state.window[..wnext]);
    }
    whave
}

/// Port of C `inflateSetDictionary` (`inflate.c` L1187-1217).
///
/// Installs `dictionary` as the preset dictionary. When the stream is wrapped
/// the call is only valid in the [`Dict`](InflateMode::Dict) state (the
/// `Z_NEED_DICT` handshake); for raw streams it may be called at any point. In
/// the `Dict` state the dictionary's Adler-32 id is validated against the value
/// read from the zlib header. The dictionary is folded into the window with the
/// same [`update_window`] machinery used during decoding.
///
/// Returns `Z_OK`, `Z_STREAM_ERROR` (wrong state), or `Z_DATA_ERROR` (id
/// mismatch). The C `Z_MEM_ERROR` path is unreachable here because window
/// allocation aborts on OOM rather than returning a code.
pub(crate) fn inflate_set_dictionary(state: &mut InflateState, dictionary: &[u8]) -> i32 {
    if state.wrap != 0 && state.mode != InflateMode::Dict {
        return Z_STREAM_ERROR;
    }
    // Validate the dictionary identifier when one is expected. C computes
    // `adler32(adler32(0, NULL, 0), dict, len)`; since `adler32(0, NULL, 0) == 1`
    // and the sibling checksum does not reproduce that constant for an empty
    // slice, start from the literal `1` (see the module-level design notes).
    if state.mode == InflateMode::Dict {
        let dictid = adler32(1, dictionary);
        if dictid != state.check {
            return Z_DATA_ERROR;
        }
    }
    // Fold the whole dictionary into the window (C passes `dict + dictLength`
    // as the end pointer with `copy == dictLength`).
    let len = dictionary.len();
    update_window(state, dictionary, len, len);
    state.havedict = true;
    Z_OK
}

/// Port of C `inflateGetHeader` (`inflate.c` L1219-1231).
///
/// Attaches a fresh [`GzHeader`] sink so the gzip header-field states populate
/// it during decoding. Only valid when the stream may carry a gzip wrapper
/// (`wrap & 2`). After parsing, retrieve the header via [`Inflate::header`].
#[cfg(feature = "gzip")]
pub(crate) fn inflate_get_header(state: &mut InflateState) -> i32 {
    if (state.wrap & 2) == 0 {
        return Z_STREAM_ERROR;
    }
    // `GzHeader::default()` has `done == false` (matching C `head->done = 0`).
    state.head = Some(GzHeader::default());
    Z_OK
}

/// Port of C `syncsearch` (`inflate.c` L1244-1262).
///
/// Scans `buf` for the `00 00 FF FF` flush marker, carrying the partial-match
/// count in `*have` (0..4) across calls. Returns the number of bytes consumed;
/// when `*have` reaches 4 the marker was found.
fn syncsearch(have: &mut u32, buf: &[u8]) -> usize {
    let mut got = *have;
    let mut next = 0usize;
    while next < buf.len() && got < 4 {
        let b = buf[next];
        let expect: u8 = if got < 2 { 0 } else { 0xff };
        if b == expect {
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

/// Port of C `inflateSync` (`inflate.c` L1264-1310): error recovery.
///
/// Skips to just past the next `00 00 FF FF` flush marker so decoding can
/// restart at a block boundary, scanning first the bit accumulator and then
/// `input`. Returns `(code, consumed)` where `consumed` is the number of
/// `input` bytes scanned (the caller advances `next_in`/`avail_in`/`total_in`
/// by that amount, even on `Z_DATA_ERROR`). On success the mode is reset to
/// [`Type`](InflateMode::Type) and the running totals are preserved (the core
/// reset only zeroes the internal `state.total`, not the wrapper's totals).
pub(crate) fn inflate_sync(
    state: &mut InflateState,
    input: &[u8],
    adler: &mut u32,
) -> (i32, usize) {
    if input.is_empty() && state.bits < 8 {
        return (Z_BUF_ERROR, 0);
    }
    // First time: drain the bit accumulator into a byte buffer and seed the
    // search with it.
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
        let mut h = 0u32;
        syncsearch(&mut h, &buf[..len]);
        state.have = h;
    }
    // Search the available input.
    let mut h = state.have;
    let len = syncsearch(&mut h, input);
    state.have = h;
    if state.have != 4 {
        return (Z_DATA_ERROR, len);
    }
    // Found the marker: set up to restart on a fresh block.
    let flags = state.flags;
    if state.flags == -1 {
        state.wrap = 0; // no header seen yet — treat as raw
    } else {
        state.wrap &= !4; // no point validating a check value now
    }
    // C saves/restores total_in/total_out around inflateReset; in this crate
    // those totals live in the wrapper and are untouched by the core reset,
    // so no save/restore is needed here. inflate_reset zeroes state.total.
    inflate_reset(state, adler);
    state.flags = flags;
    state.mode = InflateMode::Type;
    (Z_OK, len)
}

/// Port of C `inflateSyncPoint` (`inflate.c` L1320-1326).
///
/// Returns non-zero when inflate is at the end of a `Z_SYNC_FLUSH` /
/// `Z_FULL_FLUSH` empty stored block (waiting for its length bytes).
pub(crate) fn inflate_sync_point(state: &InflateState) -> i32 {
    (state.mode == InflateMode::Stored && state.bits == 0) as i32
}

/// Port of C `inflateCopy` (`inflate.c` L1328-1368).
///
/// Produces an independent copy of the decode state. Because `lencode` /
/// `distcode` / `next` are **indices** into the owned `codes` table (not raw
/// pointers) and `window` / `codes` are owned containers, a plain `clone()`
/// reproduces the C pointer fix-up automatically and correctly — the indices
/// are already relative to the (cloned) table.
pub(crate) fn inflate_copy(source: &InflateState) -> InflateState {
    source.clone()
}

/// Port of C `inflateUndermine` (`inflate.c` L1370-1383).
///
/// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` is disabled in this port, so
/// (matching the C `#else` branch) the `subvert` request is ignored: `sane`
/// is forced true and `Z_DATA_ERROR` is returned.
pub(crate) fn inflate_undermine(state: &mut InflateState, _subvert: i32) -> i32 {
    state.sane = true;
    Z_DATA_ERROR
}

/// Port of C `inflateValidate` (`inflate.c` L1385-1395).
///
/// Enables (`check != 0`) or disables the running check-value validation by
/// toggling bit 2 (`4`) of `wrap`. Only meaningful when the stream is wrapped.
pub(crate) fn inflate_validate(state: &mut InflateState, check: i32) -> i32 {
    if check != 0 && state.wrap != 0 {
        state.wrap |= 4;
    } else {
        state.wrap &= !4;
    }
    Z_OK
}

/// Port of C `inflateMark` (`inflate.c` L1397-1406).
///
/// Returns a value encoding the decode progress: the high bits carry
/// `state.back` (the number of bits back to the start of the current code) and
/// the low bits carry the bytes remaining to copy for the current match. The
/// FFI shim maps the unreachable bad-state case to `-(1 << 16)`.
pub(crate) fn inflate_mark(state: &InflateState) -> i64 {
    let low: i64 = match state.mode {
        InflateMode::Copy => state.length as i64,
        InflateMode::Match => (state.was - state.length) as i64,
        _ => 0,
    };
    ((state.back as i64) << 16) + low
}

/// Port of C `inflateCodesUsed` (`inflate.c` L1408-1413).
///
/// Returns the number of `Code` table entries consumed so far. C computes
/// `state->next - state->codes`; here `state.next` is already that index.
pub(crate) fn inflate_codes_used(state: &InflateState) -> u64 {
    state.next as u64
}

/// Maps an integer zlib return code to a [`ZlibError`], defaulting to
/// [`ZlibError::StreamError`] for any non-error/unknown code (the init and
/// reset paths only ever yield `Z_STREAM_ERROR`).
#[inline]
fn to_err(code: i32) -> ZlibError {
    ZlibError::from_c_int(code).unwrap_or(ZlibError::StreamError)
}

// ===========================================================================
// Idiomatic safe-Rust wrapper (`Inflate`).
//
// Owns the `Box<InflateState>` directly and exposes a `Result`/`Option`-based
// API. The C-ABI surface is minted separately in `src/ffi.rs` from the
// `pub(crate)` free functions above; this type is what pure-Rust callers use.
// Teardown is automatic (the `Box<InflateState>` drops, freeing the window).
// ===========================================================================

/// A safe, owning DEFLATE/zlib/gzip decompressor.
///
/// Wraps the [`InflateState`] decode machine with idiomatic accounting
/// (`total_in`/`total_out`/`adler`) and a `Result`-based API. Construct with
/// [`Inflate::new`] (zlib, default window) or [`Inflate::with_window_bits`]
/// (selecting raw/zlib/gzip/auto per the `windowBits` convention), then drive
/// decoding with [`Inflate::inflate`].
pub struct Inflate {
    state: Box<InflateState>,
    /// Total bytes consumed from input across all calls.
    pub total_in: u64,
    /// Total bytes produced to output across all calls.
    pub total_out: u64,
    /// Running check value (Adler-32 for zlib, CRC-32 for gzip), mirroring the
    /// C `z_stream.adler`.
    pub adler: u32,
    /// The most recent error message, if any (mirrors C `z_stream.msg`).
    pub msg: Option<&'static str>,
    /// Decode data-type bits (mirrors C `z_stream.data_type`).
    pub data_type: i32,
}

impl Inflate {
    /// Creates a decompressor for the zlib format with the default window size
    /// ([`DEF_WBITS`]). Equivalent to C `inflateInit`.
    pub fn new() -> core::result::Result<Self, ZlibError> {
        let state = inflate_init().map_err(to_err)?;
        // Initial public check value == `wrap & 1` (1 for zlib), matching
        // inflate_reset_keep.
        let adler = (state.wrap & 1) as u32;
        Ok(Inflate {
            state,
            total_in: 0,
            total_out: 0,
            adler,
            msg: None,
            data_type: 0,
        })
    }

    /// Creates a decompressor with an explicit `window_bits`, honouring the
    /// `windowBits` overloading convention (8..=15 zlib, -8..=-15 raw, 24..=31
    /// gzip, 40..=47 auto-detect). Equivalent to C `inflateInit2`.
    pub fn with_window_bits(window_bits: i32) -> core::result::Result<Self, ZlibError> {
        let state = inflate_init2(window_bits).map_err(to_err)?;
        // Initial public check value == `wrap & 1` (1 for zlib, 0 otherwise),
        // matching inflate_reset_keep.
        let adler = (state.wrap & 1) as u32;
        Ok(Inflate {
            state,
            total_in: 0,
            total_out: 0,
            adler,
            msg: None,
            data_type: 0,
        })
    }

    /// Resets the decompressor to decode a fresh stream, keeping the configured
    /// window size and wrap mode. Equivalent to C `inflateReset`.
    pub fn reset(&mut self) {
        let mut adler = self.adler;
        inflate_reset(&mut self.state, &mut adler);
        self.adler = adler;
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = 0;
    }

    /// Resets the decompressor and reconfigures the window/wrap mode from a new
    /// `window_bits`. Equivalent to C `inflateReset2`.
    pub fn reset2(&mut self, window_bits: i32) -> core::result::Result<(), ZlibError> {
        let mut adler = self.adler;
        let ret = inflate_reset2(&mut self.state, window_bits, &mut adler);
        if ret != Z_OK {
            return Err(to_err(ret));
        }
        self.adler = adler;
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = 0;
        Ok(())
    }

    /// Installs a preset dictionary. Equivalent to C `inflateSetDictionary`.
    pub fn set_dictionary(&mut self, dict: &[u8]) -> core::result::Result<(), ZlibError> {
        match inflate_set_dictionary(&mut self.state, dict) {
            Z_OK => Ok(()),
            code => Err(to_err(code)),
        }
    }

    /// Retrieves the current sliding-window contents (the effective dictionary)
    /// into `out`, returning the number of valid bytes. Equivalent to C
    /// `inflateGetDictionary`.
    pub fn get_dictionary(&self, out: &mut [u8]) -> usize {
        inflate_get_dictionary(&self.state, out)
    }

    /// Injects `bits` bits of `value` into the bit accumulator (or flushes it
    /// when `bits < 0`). Equivalent to C `inflatePrime`.
    pub fn prime(&mut self, bits: i32, value: i32) -> core::result::Result<(), ZlibError> {
        match inflate_prime(&mut self.state, bits, value) {
            Z_OK => Ok(()),
            code => Err(to_err(code)),
        }
    }

    /// Attaches a gzip header sink to capture the header fields during
    /// decoding. Equivalent to C `inflateGetHeader`.
    #[cfg(feature = "gzip")]
    pub fn get_header(&mut self) -> core::result::Result<(), ZlibError> {
        match inflate_get_header(&mut self.state) {
            Z_OK => Ok(()),
            code => Err(to_err(code)),
        }
    }

    /// Returns the parsed gzip header, if one was requested via
    /// [`Inflate::get_header`] and has begun/finished parsing.
    #[cfg(feature = "gzip")]
    pub fn header(&self) -> Option<&GzHeader> {
        self.state.head.as_ref()
    }

    /// Scans `input` for the next flush marker to recover from corrupt data,
    /// returning `(code, bytes_consumed)`. Equivalent to C `inflateSync`.
    pub fn sync(&mut self, input: &[u8]) -> (i32, usize) {
        let mut adler = self.adler;
        let (ret, consumed) = inflate_sync(&mut self.state, input, &mut adler);
        self.adler = adler;
        self.total_in += consumed as u64;
        (ret, consumed)
    }

    /// Returns `true` when inflate is at a sync-flush block boundary. Equivalent
    /// to C `inflateSyncPoint`.
    pub fn sync_point(&self) -> bool {
        inflate_sync_point(&self.state) != 0
    }

    /// Returns the decode-progress marker. Equivalent to C `inflateMark`.
    pub fn mark(&self) -> i64 {
        inflate_mark(&self.state)
    }

    /// Returns the number of `Code` table entries used. Equivalent to C
    /// `inflateCodesUsed`.
    pub fn codes_used(&self) -> u64 {
        inflate_codes_used(&self.state)
    }

    /// Enables or disables running check-value validation. Equivalent to C
    /// `inflateValidate`.
    pub fn validate(&mut self, check: bool) {
        inflate_validate(&mut self.state, check as i32);
    }

    /// Requests that invalid "distance too far" matches be tolerated. As in C
    /// (with the corresponding compile option disabled) this is a no-op that
    /// forces strict checking back on and reports `Z_DATA_ERROR`. Equivalent to
    /// C `inflateUndermine`.
    pub fn undermine(&mut self, subvert: bool) -> i32 {
        inflate_undermine(&mut self.state, subvert as i32)
    }

    /// Produces an independent copy of this decompressor. Equivalent to C
    /// `inflateCopy`.
    pub fn copy(&self) -> Self {
        Inflate {
            state: Box::new(inflate_copy(&self.state)),
            total_in: self.total_in,
            total_out: self.total_out,
            adler: self.adler,
            msg: self.msg,
            data_type: self.data_type,
        }
    }

    /// Decompresses from `input` into `output` with the given `flush` mode.
    ///
    /// Returns `(code, consumed, produced)` where `code` is the C return code
    /// (`Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT`, or a negative error), `consumed`
    /// is the number of input bytes read, and `produced` is the number of
    /// output bytes written. The running totals, `adler`, `msg`, and
    /// `data_type` fields are updated from the call.
    pub fn inflate(&mut self, input: &[u8], output: &mut [u8], flush: i32) -> (i32, usize, usize) {
        let outcome = inflate(&mut self.state, input, output, flush);
        self.total_in += outcome.total_in_delta;
        self.total_out += outcome.total_out_delta;
        self.adler = outcome.adler;
        self.msg = outcome.msg;
        self.data_type = outcome.data_type;
        (outcome.ret, outcome.in_consumed, outcome.out_produced)
    }
}
