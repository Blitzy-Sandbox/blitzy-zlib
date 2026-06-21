//! `inflateBack` — callback-driven raw-DEFLATE decompression.
//!
//! This module is the safe-Rust port of zlib's `infback.c`. It implements the
//! `inflateBack` family (`inflate_back_init` / `inflate_back` /
//! `inflate_back_end`), a self-contained decompressor that:
//!
//! * decodes a **raw DEFLATE stream only** (RFC 1951) — there is *no* zlib
//!   (RFC 1950) or gzip (RFC 1952) header or trailer, and therefore **no
//!   checksum** is computed or verified here;
//! * uses the caller's window buffer as **both** the 32 KiB-class sliding
//!   window *and* the output buffer (a single buffer plays both roles); and
//! * pulls input and pushes output through **callbacks** rather than through
//!   the `next_in`/`next_out` slices of a [`ZStream`](crate::stream::ZStream):
//!   when it needs more input it calls [`BackInput::fill`], and when the window
//!   fills (or the stream ends) it calls [`BackOutput::write`].
//!
//! As the C header comment notes, "this code is largely copied from
//! `inflate.c`": it reuses the same Huffman table builder
//! (`inflate_table`), the same fixed
//! tables (`inflate_fixed`), and the
//! same [`Code`](crate::inflate::tables::Code) decode-entry layout. Normally an
//! application links *either*
//! `inflate` *or* `inflateBack`, not both.
//!
//! # Idiomatic callbacks replace C function pointers
//!
//! The C API takes two C function pointers (`in_func` / `out_func`) plus opaque
//! descriptor pointers (`in_desc` / `out_desc`). Per AAP §0.3.2 / §0.6.2 those
//! become the safe traits [`BackInput`] and [`BackOutput`]: the descriptor is
//! simply the trait object's own state. The C drop-in shim in `src/ffi.rs`
//! constructs adapters that invoke the raw C `in_func`/`out_func` through the
//! (unsafe) FFI boundary — **that `unsafe` lives in `ffi.rs`, never here.**
//!
//! [`BackInput::fill`] takes `&mut self` and returns `&[u8]`, which cleanly
//! scopes the returned slice's lifetime to "valid until `fill` is called again"
//! — exactly the C `in()` contract ("the application must not change the
//! provided input until `in()` is called again or `inflateBack()` returns").
//!
//! # Zero `unsafe`, `no_std`-clean
//!
//! This module is **100 % safe Rust** (zero `unsafe`, per AAP §0.6.2 / §0.7.2):
//! the window/output match copy — which in C is raw pointer arithmetic over a
//! single aliased buffer — is performed here with **safe slice indexing** (a
//! byte-at-a-time loop, because the run may overlap). It is `no_std`-clean,
//! using only `core`; it compiles under `--no-default-features --features
//! no-std`. (See [the aliasing note](self#why-inflate_fast-is-not-reused) for
//! why the `inflate_fast` optimisation is
//! intentionally *not* used here.)
//!
//! # Control flow: `goto inf_leave` → labeled loop
//!
//! C drives the decode with `for (;;) switch (state->mode)` and jumps to a
//! shared `inf_leave:` tail with `goto` to flush leftover output and return.
//! Here the state machine is a `'inf: loop { match state.mode { … } }`; the
//! C `goto inf_leave` becomes `break 'inf <return-code>`, and the leave handler
//! (the leftover-output flush) runs immediately after the loop. The input
//! refill macros (`PULL`/`PULLBYTE`/`NEEDBITS`/`ROOM`) are expressed as local
//! `macro_rules!` that may `break 'inf Z_BUF_ERROR` on callback failure —
//! a faithful reproduction of the C macros, which `goto inf_leave` on failure.
//!
//! # Why `inflate_fast` is not reused
//!
//! In C, `state->window` *is* the output buffer, so the hot loop
//! `inflate_fast` copies LZ77 matches within that single aliased buffer. In
//! this crate's safe model, `inflate_fast`
//! reads `state.window` as a buffer **separate** from its `output` slice;
//! reusing it for `inflateBack` would require aliasing the window and the
//! output, which the borrow checker forbids without `unsafe`. Per AAP §0.6.2
//! the inflate core confines `unsafe` to `fast.rs` alone, and AAP §0.7.3 gates
//! the *main* inflate path (not this niche callback API) on throughput.
//! Therefore `inflate_back` deliberately omits the `inflate_fast` call and uses
//! only the per-symbol decode loop, which is byte-identical to C and keeps this
//! module 100 % safe.

use crate::constants::{Z_BUF_ERROR, Z_DATA_ERROR, Z_OK, Z_STREAM_END, Z_STREAM_ERROR};
use crate::inflate::state::{InflateMode, InflateState};
use crate::inflate::tables::{CodeType, inflate_fixed, inflate_table};

/// Input source for [`inflate_back`].
///
/// This is the safe-trait replacement for zlib's `in_func` C function pointer.
/// [`fill`](BackInput::fill) returns the next chunk of compressed input; an
/// **empty** slice signals "no more input is available", which causes
/// [`inflate_back`] to stop and return [`Z_BUF_ERROR`]
/// (mirroring C, where an `in()` return of `0` does the same).
///
/// # Lifetime contract
///
/// The borrow returned by [`fill`](BackInput::fill) is tied to `&mut self`, so
/// it is guaranteed valid until the next call to `fill` (or until
/// [`inflate_back`] returns). This is precisely zlib's documented `in()`
/// contract — "the application must not change the provided input until `in()`
/// is called again" — enforced here by the borrow checker rather than by
/// convention.
pub trait BackInput {
    /// Return the next chunk of compressed input, or an empty slice to signal
    /// that no further input is available.
    fn fill(&mut self) -> &[u8];
}

/// Output sink for [`inflate_back`].
///
/// This is the safe-trait replacement for zlib's `out_func` C function pointer.
/// [`write`](BackOutput::write) is handed a slice of freshly produced output
/// bytes and must consume all of them; it returns `true` on **failure**, which
/// causes [`inflate_back`] to stop and return
/// [`Z_BUF_ERROR`] (mirroring C, where a
/// non-zero `out()` return means failure).
pub trait BackOutput {
    /// Consume `buf` of decompressed output. Return `true` on failure.
    fn write(&mut self, buf: &[u8]) -> bool;
}

/// Any `FnMut(&[u8]) -> bool` closure is usable as a [`BackOutput`].
///
/// This blanket impl makes the common case ergonomic — a caller can pass a
/// closure directly as the output sink (`|buf| { dst.extend_from_slice(buf); false }`)
/// without defining a wrapper type. The closure returns `true` on failure,
/// matching [`BackOutput::write`].
impl<F> BackOutput for F
where
    F: FnMut(&[u8]) -> bool,
{
    #[inline]
    fn write(&mut self, buf: &[u8]) -> bool {
        self(buf)
    }
}

/// A [`BackInput`] adapter that serves a single in-memory buffer, optionally in
/// fixed-size chunks.
///
/// This is a convenience source for the common "decompress this whole buffer"
/// case and for exercising [`inflate_back`] with realistic, chunked input
/// refills. Construct it with [`SliceInput::new`] (serves the entire buffer in
/// one chunk) or [`SliceInput::with_chunk`] (serves at most `chunk` bytes per
/// [`fill`](BackInput::fill) call, simulating a streaming source). Once the
/// buffer is exhausted, `fill` returns an empty slice.
pub struct SliceInput<'a> {
    data: &'a [u8],
    pos: usize,
    chunk: usize,
}

impl<'a> SliceInput<'a> {
    /// Serve the entire `data` buffer in a single [`fill`](BackInput::fill).
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        // A zero-length buffer still yields an empty first chunk (signalling
        // end-of-input immediately); `chunk` is irrelevant in that case.
        SliceInput {
            data,
            pos: 0,
            chunk: data.len().max(1),
        }
    }

    /// Serve `data` in pieces of at most `chunk` bytes per
    /// [`fill`](BackInput::fill) call. `chunk` is clamped up to `1` so progress
    /// is always made.
    #[must_use]
    pub fn with_chunk(data: &'a [u8], chunk: usize) -> Self {
        SliceInput {
            data,
            pos: 0,
            chunk: chunk.max(1),
        }
    }
}

impl BackInput for SliceInput<'_> {
    #[inline]
    fn fill(&mut self) -> &[u8] {
        let start = self.pos;
        let end = (start + self.chunk).min(self.data.len());
        self.pos = end;
        &self.data[start..end]
    }
}

/// Initialise an [`InflateState`] for use with [`inflate_back`].
///
/// This is the safe-Rust port of C `inflateBackInit_` (`infback.c`, lines
/// 25-64). The version and `stream_size` checks performed by the C macro live
/// in the FFI shim (`src/ffi.rs`); the genuine initialisation logic — the
/// `windowBits` validation and the state field setup — lives here.
///
/// # Parameters
///
/// * `window_bits` — the base-2 logarithm of the window size. It **must** be in
///   `8..=15`; the caller-supplied window/output buffer passed later to
///   [`inflate_back`] must be exactly `1 << window_bits` bytes.
///
/// # Returns
///
/// * `Ok(state)` — a fresh decode state configured for raw-DEFLATE callback
///   decompression, ready to pass to [`inflate_back`].
/// * `Err(`[`Z_STREAM_ERROR`]`)` — `window_bits`
///   is outside `8..=15`.
///
/// # Window ownership
///
/// Unlike the main inflate engine, this routine **does not allocate or own the
/// sliding window**. In C, `inflateBackInit_` stores the caller's `window`
/// pointer in `state->window`; here the window is instead supplied as a
/// `&mut [u8]` argument to [`inflate_back`] on every call. The
/// [`window`](InflateState::window) `Vec` of the returned state is therefore
/// left empty and unused — the wrapper/checksum fields
/// ([`wrap`](InflateState::wrap) / [`flags`](InflateState::flags)) are likewise
/// irrelevant because this is a *raw* stream. Only the decode tables
/// ([`codes`](InflateState::codes), allocated to length
/// [`ENOUGH`](crate::inflate::tables::ENOUGH) by
/// [`InflateState::new`]) are owned.
///
/// # Example
///
/// ```ignore
/// let mut state = inflate_back_init(15).expect("valid windowBits");
/// let mut window = vec![0u8; 1 << 15];
/// // … feed `state` and `window` to `inflate_back` …
/// ```
pub fn inflate_back_init(window_bits: i32) -> Result<InflateState, i32> {
    // C: `if (... || windowBits < 8 || windowBits > 15) return Z_STREAM_ERROR;`
    // (The `strm`/`window` NULL checks are the FFI shim's responsibility.)
    if !(8..=15).contains(&window_bits) {
        return Err(Z_STREAM_ERROR);
    }

    // Allocate the decode-table buffer (length ENOUGH) and zero the scratch
    // arrays via the standard constructor, then apply the inflateBackInit_
    // field overrides (C lines 56-62).
    let mut state = InflateState::new();
    state.dmax = 32768; // C: state->dmax = 32768U;
    state.wbits = window_bits as u32; // C: state->wbits = (uInt)windowBits;
    state.wsize = 1u32 << window_bits; // C: state->wsize = 1U << windowBits;
    state.wnext = 0; // C: state->wnext = 0;
    state.whave = 0; // C: state->whave = 0;
    state.sane = true; // C: state->sane = 1;
    // The window/output buffer is supplied by the caller to `inflate_back`
    // (C stores `state->window = window` here; we do not own it).
    state.mode = InflateMode::Type; // ready to decode the first block

    Ok(state)
}

/// Permutation of the code-length code-length order (C `infback.c` `order`).
///
/// The 19 code-length codes are transmitted in this order, so that the most
/// commonly used lengths come first and trailing zeros can be elided.
const ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Decompress a raw-DEFLATE stream using caller-supplied input/output callbacks.
///
/// This is the safe-Rust port of C `inflateBack` (`infback.c`, lines 191-570).
/// It decodes a **raw DEFLATE** (RFC 1951) stream — no zlib/gzip wrapper, no
/// checksum — pulling compressed input via [`BackInput::fill`] and pushing
/// decompressed output via [`BackOutput::write`]. The `window` buffer serves
/// simultaneously as the LZ77 sliding window *and* as the output staging
/// buffer, exactly as in C.
///
/// # Parameters
///
/// * `state` — a state previously initialised by [`inflate_back_init`]. It is
///   reset (mode → `Type`, `last` → `false`, `whave` → `0`) at entry, mirroring
///   C lines 213-216, so a single state may drive successive streams.
/// * `window` — the combined window/output buffer. Its length **must** equal
///   `state.wsize` (`1 << windowBits`); a shorter buffer yields
///   [`Z_STREAM_ERROR`].
/// * `input` — the compressed-input source (see [`BackInput`]).
/// * `output` — the decompressed-output sink (see [`BackOutput`]).
///
/// # Returns (identical to C `inflateBack`)
///
/// * [`Z_STREAM_END`] — the stream terminated
///   correctly (final block decoded and all output flushed).
/// * [`Z_BUF_ERROR`] — [`BackInput::fill`]
///   signalled end-of-input mid-stream, or [`BackOutput::write`] reported a
///   write failure.
/// * [`Z_DATA_ERROR`] — the input is not a valid
///   DEFLATE stream (bad block type, bad stored-length complement, invalid
///   Huffman code, or a back-reference distance that points before the start of
///   the window).
/// * [`Z_STREAM_ERROR`] — the `window` buffer
///   is the wrong size, or the state machine reached an impossible mode.
///
/// # No `inflate_fast`, by design
///
/// See [the module-level aliasing note](self#why-inflate_fast-is-not-reused):
/// the optimised `inflate_fast` is *not*
/// invoked here. The per-symbol decode loop below handles every case and is
/// byte-identical to C; it is the always-correct path.
pub fn inflate_back<In, Out>(
    state: &mut InflateState,
    window: &mut [u8],
    input: &mut In,
    output: &mut Out,
) -> i32
where
    In: BackInput,
    Out: BackOutput,
{
    let wsize: usize = state.wsize as usize;

    // The caller must supply a window of exactly `state.wsize` bytes. C trusts
    // the caller; we guard so misuse surfaces as Z_STREAM_ERROR rather than a
    // panic, and so the safe slice indexing below is always in bounds.
    if wsize == 0 || window.len() < wsize {
        return Z_STREAM_ERROR;
    }

    // ---- Reset the state (C lines 213-216). ----
    state.mode = InflateMode::Type;
    state.last = false;
    state.whave = 0;

    // ---- Locals (C lines 192-205, 217-223). ----
    // `inbuf`/`inpos` model C's `next` pointer: the current input chunk and the
    // read cursor into it. `have` (C's available-input count) is the derived
    // quantity `inbuf.len() - inpos`. For the pure-Rust API we start with an
    // empty chunk so the first `PULL` calls `input.fill()`.
    let mut inbuf: &[u8] = &[];
    let mut inpos: usize = 0;
    let mut hold: u32 = 0; // bit accumulator (C `hold`)
    let mut bits: u32 = 0; // valid bits in `hold` (C `bits`)
    let mut put: usize = 0; // write cursor into `window` (C `put`)
    let mut left: usize = wsize; // remaining output space in `window` (C `left`)
    // Invariant maintained throughout: `put + left == wsize`.

    // The whole state machine runs inside this labeled loop. `break 'inf <code>`
    // is the faithful reproduction of C's `goto inf_leave` — it carries the
    // return code out to the leftover-flush handler that follows the loop.
    let mut ret: i32 = 'inf: loop {
        // -------- C macros, reproduced as local `macro_rules!`. --------
        // Each may reference the enclosing locals, and the input/output ones
        // may `break 'inf Z_BUF_ERROR` on callback failure (C's `goto
        // inf_leave`).

        /// C `have` — number of input bytes currently available.
        macro_rules! have {
            () => {
                (inbuf.len() - inpos)
            };
        }

        /// C `PULL()` — ensure some input is available, refilling via the
        /// callback when the current chunk is exhausted. An empty refill means
        /// "no more input" → `Z_BUF_ERROR`.
        macro_rules! pull {
            () => {
                if inbuf.len() == inpos {
                    inbuf = input.fill();
                    inpos = 0;
                    if inbuf.is_empty() {
                        break 'inf Z_BUF_ERROR;
                    }
                }
            };
        }

        /// C `PULLBYTE()` — pull one input byte into the bit accumulator.
        macro_rules! pullbyte {
            () => {{
                pull!();
                let byte = inbuf[inpos];
                inpos += 1;
                hold |= (byte as u32) << bits;
                bits += 8;
            }};
        }

        /// C `NEEDBITS(n)` — ensure at least `n` bits are in the accumulator.
        macro_rules! needbits {
            ($n:expr) => {
                while bits < ($n) {
                    pullbyte!();
                }
            };
        }

        /// C `BITS(n)` — the low `n` bits of the accumulator (`n <= 15`).
        macro_rules! getbits {
            ($n:expr) => {
                (hold & ((1u32 << ($n)) - 1))
            };
        }

        /// C `DROPBITS(n)` — remove `n` bits from the accumulator. Callers pass
        /// a `u32` (casting `u8` code fields explicitly).
        macro_rules! dropbits {
            ($n:expr) => {{
                hold >>= ($n);
                bits -= ($n);
            }};
        }

        /// C `BYTEBITS()` — drop 0..7 bits to reach a byte boundary.
        macro_rules! bytebits {
            () => {{
                hold >>= bits & 7;
                bits -= bits & 7;
            }};
        }

        /// C `INITBITS()` — clear the bit accumulator.
        macro_rules! initbits {
            () => {{
                hold = 0;
                bits = 0;
            }};
        }

        /// C `ROOM()` — ensure output space, flushing the full window via the
        /// callback when it is full. A flush sets `whave = wsize` so matches may
        /// reach the entire window; a write failure → `Z_BUF_ERROR`.
        macro_rules! room {
            () => {
                if left == 0 {
                    put = 0;
                    left = wsize;
                    state.whave = wsize as u32;
                    if output.write(&window[..wsize]) {
                        break 'inf Z_BUF_ERROR;
                    }
                }
            };
        }

        // -------- The `for (;;) switch (state->mode)` state machine. --------
        match state.mode {
            // ---- TYPE: determine and dispatch the block type (C 228-260). ----
            InflateMode::Type => {
                if state.last {
                    // Drop to a byte boundary; the stream is finished.
                    bytebits!();
                    state.mode = InflateMode::Done;
                } else {
                    needbits!(3);
                    state.last = getbits!(1) != 0;
                    dropbits!(1);
                    match getbits!(2) {
                        0 => state.mode = InflateMode::Stored, // stored block
                        1 => {
                            // Fixed Huffman block: install the static tables.
                            // (Per the `inflate_fixed` contract, do NOT advance
                            // `state.next`.)
                            let ft = inflate_fixed(&mut state.codes);
                            state.lencode = ft.lencode;
                            state.lenbits = ft.lenbits;
                            state.distcode = ft.distcode;
                            state.distbits = ft.distbits;
                            state.mode = InflateMode::Len;
                        }
                        2 => state.mode = InflateMode::Table, // dynamic block
                        _ => state.mode = InflateMode::Bad,   // invalid block type
                    }
                    dropbits!(2);
                }
            }

            // ---- STORED: copy an uncompressed block (C 262-292). ----
            InflateMode::Stored => {
                bytebits!(); // go to a byte boundary
                needbits!(32);
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    // LEN and its ones-complement NLEN disagree.
                    state.mode = InflateMode::Bad;
                } else {
                    state.length = hold & 0xffff;
                    initbits!();
                    // Copy `length` bytes straight from input to the window.
                    let mut length = state.length as usize;
                    while length != 0 {
                        let mut copy = length;
                        pull!();
                        room!();
                        copy = copy.min(have!()).min(left);
                        window[put..put + copy].copy_from_slice(&inbuf[inpos..inpos + copy]);
                        inpos += copy;
                        left -= copy;
                        put += copy;
                        length -= copy;
                    }
                    state.mode = InflateMode::Type;
                }
            }

            // ---- TABLE: read and build the dynamic Huffman tables (C 294-419). ----
            InflateMode::Table => {
                'table: {
                    needbits!(14);
                    state.nlen = getbits!(5) + 257;
                    dropbits!(5);
                    state.ndist = getbits!(5) + 1;
                    dropbits!(5);
                    state.ncode = getbits!(4) + 4;
                    dropbits!(4);
                    if state.nlen > 286 || state.ndist > 30 {
                        // "too many length or distance symbols"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Read the code-length code lengths in their permuted order.
                    state.have = 0;
                    while state.have < state.ncode {
                        needbits!(3);
                        state.lens[ORDER[state.have as usize]] = getbits!(3) as u16;
                        state.have += 1;
                        dropbits!(3);
                    }
                    while state.have < 19 {
                        state.lens[ORDER[state.have as usize]] = 0;
                        state.have += 1;
                    }

                    // Build the code-length code table (root = 7 bits).
                    state.next = 0;
                    state.lencode = 0;
                    state.lenbits = 7;
                    if inflate_table(
                        CodeType::Codes,
                        &state.lens,
                        19,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.lenbits,
                        &mut state.work,
                    ) != 0
                    {
                        // "invalid code lengths set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Decode the literal/length and distance code lengths.
                    state.have = 0;
                    let total = state.nlen + state.ndist;
                    while state.have < total {
                        // Decode one code-length code (with sub-table descent).
                        let mut here =
                            state.codes[state.lencode + getbits!(state.lenbits) as usize];
                        while (here.bits as u32) > bits {
                            pullbyte!();
                            here = state.codes[state.lencode + getbits!(state.lenbits) as usize];
                        }
                        if here.val < 16 {
                            // A literal code length 0..15.
                            dropbits!(here.bits as u32);
                            state.lens[state.have as usize] = here.val;
                            state.have += 1;
                        } else {
                            // A repeat code: 16 (copy prev 3-6), 17 (zero 3-10),
                            // 18 (zero 11-138).
                            let len: u16;
                            let mut copy: u32;
                            if here.val == 16 {
                                needbits!(here.bits as u32 + 2);
                                dropbits!(here.bits as u32);
                                if state.have == 0 {
                                    // "invalid bit length repeat"
                                    state.mode = InflateMode::Bad;
                                    break 'table;
                                }
                                len = state.lens[state.have as usize - 1];
                                copy = 3 + getbits!(2);
                                dropbits!(2);
                            } else if here.val == 17 {
                                needbits!(here.bits as u32 + 3);
                                dropbits!(here.bits as u32);
                                len = 0;
                                copy = 3 + getbits!(3);
                                dropbits!(3);
                            } else {
                                needbits!(here.bits as u32 + 7);
                                dropbits!(here.bits as u32);
                                len = 0;
                                copy = 11 + getbits!(7);
                                dropbits!(7);
                            }
                            if state.have + copy > total {
                                // "invalid bit length repeat"
                                state.mode = InflateMode::Bad;
                                break 'table;
                            }
                            while copy != 0 {
                                state.lens[state.have as usize] = len;
                                state.have += 1;
                                copy -= 1;
                            }
                        }
                    }

                    // There must be an end-of-block code.
                    if state.lens[256] == 0 {
                        // "invalid code -- missing end-of-block"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Build the literal/length table (root = 9) and the distance
                    // table (root = 6). Do NOT change these root sizes — the
                    // ENOUGH constants depend on 9 and 6 (inftrees.h).
                    let nlen = state.nlen as usize;
                    let ndist = state.ndist as usize;
                    state.next = 0;
                    state.lencode = 0;
                    state.lenbits = 9;
                    if inflate_table(
                        CodeType::Lens,
                        &state.lens,
                        nlen,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.lenbits,
                        &mut state.work,
                    ) != 0
                    {
                        // "invalid literal/lengths set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }
                    // The distance table starts at the next free slot after the
                    // lit/len table (C `state->distcode = state->next`).
                    state.distcode = state.next;
                    state.distbits = 6;
                    if inflate_table(
                        CodeType::Dists,
                        &state.lens[nlen..],
                        ndist,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.distbits,
                        &mut state.work,
                    ) != 0
                    {
                        // "invalid distances set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }
                    state.mode = InflateMode::Len; // fall through to LEN
                }
            }

            // ---- LEN: decode literal/length/distance codes (C 422-543). ----
            InflateMode::Len => {
                'len: {
                    // NOTE: the C fast path `if (have >= 6 && left >= 258) {
                    // inflate_fast(); }` is intentionally omitted — see the
                    // module docs. The per-symbol path below is byte-identical
                    // and always correct.

                    // Decode a literal/length/end-of-block code (with sub-table
                    // descent, C 432-446).
                    let mut here = state.codes[state.lencode + getbits!(state.lenbits) as usize];
                    while (here.bits as u32) > bits {
                        pullbyte!();
                        here = state.codes[state.lencode + getbits!(state.lenbits) as usize];
                    }
                    if here.op != 0 && (here.op & 0xf0) == 0 {
                        let last = here;
                        let idx = last.val as usize
                            + (getbits!(last.bits as u32 + last.op as u32) >> last.bits) as usize;
                        here = state.codes[state.lencode + idx];
                        while (last.bits as u32 + here.bits as u32) > bits {
                            pullbyte!();
                            let idx = last.val as usize
                                + (getbits!(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize;
                            here = state.codes[state.lencode + idx];
                        }
                        dropbits!(last.bits as u32);
                    }
                    dropbits!(here.bits as u32);
                    state.length = here.val as u32;

                    if here.op == 0 {
                        // Literal byte → emit it.
                        room!();
                        window[put] = state.length as u8;
                        put += 1;
                        left -= 1;
                        break 'len; // mode stays LEN
                    }
                    if here.op & 32 != 0 {
                        // End of block → back to TYPE.
                        state.mode = InflateMode::Type;
                        break 'len;
                    }
                    if here.op & 64 != 0 {
                        // "invalid literal/length code"
                        state.mode = InflateMode::Bad;
                        break 'len;
                    }

                    // Length code → add extra bits.
                    state.extra = (here.op & 15) as u32;
                    if state.extra != 0 {
                        needbits!(state.extra);
                        state.length += getbits!(state.extra);
                        dropbits!(state.extra);
                    }

                    // Decode a distance code (with sub-table descent, C 486-500).
                    let mut here = state.codes[state.distcode + getbits!(state.distbits) as usize];
                    while (here.bits as u32) > bits {
                        pullbyte!();
                        here = state.codes[state.distcode + getbits!(state.distbits) as usize];
                    }
                    if (here.op & 0xf0) == 0 {
                        let last = here;
                        let idx = last.val as usize
                            + (getbits!(last.bits as u32 + last.op as u32) >> last.bits) as usize;
                        here = state.codes[state.distcode + idx];
                        while (last.bits as u32 + here.bits as u32) > bits {
                            pullbyte!();
                            let idx = last.val as usize
                                + (getbits!(last.bits as u32 + last.op as u32) >> last.bits)
                                    as usize;
                            here = state.codes[state.distcode + idx];
                        }
                        dropbits!(last.bits as u32);
                    }
                    dropbits!(here.bits as u32);
                    if here.op & 64 != 0 {
                        // "invalid distance code"
                        state.mode = InflateMode::Bad;
                        break 'len;
                    }
                    state.offset = here.val as u32;

                    // Distance extra bits.
                    state.extra = (here.op & 15) as u32;
                    if state.extra != 0 {
                        needbits!(state.extra);
                        state.offset += getbits!(state.extra);
                        dropbits!(state.extra);
                    }

                    // Reject a distance that points before the available window.
                    // (The inflateBack form of this check, C 516-517.)
                    let offset = state.offset as usize;
                    let whave = state.whave as usize;
                    if offset > wsize - (if whave < wsize { left } else { 0 }) {
                        // "invalid distance too far back"
                        state.mode = InflateMode::Bad;
                        break 'len;
                    }

                    // Copy the match from the window to the output — which are
                    // the same buffer. The run may overlap (e.g. offset == 1),
                    // so it MUST be a byte-at-a-time copy with safe indexing
                    // (C 524-542).
                    let mut length = state.length as usize;
                    loop {
                        room!();
                        let mut copy = wsize - offset;
                        let mut from;
                        if copy < left {
                            // The source wraps around the window end.
                            from = put + copy;
                            copy = left - copy;
                        } else {
                            from = put - offset;
                            copy = left;
                        }
                        if copy > length {
                            copy = length;
                        }
                        length -= copy;
                        left -= copy;
                        for _ in 0..copy {
                            window[put] = window[from];
                            put += 1;
                            from += 1;
                        }
                        if length == 0 {
                            break;
                        }
                    }
                    // mode stays LEN — decode the next code.
                }
            }

            // ---- DONE: the stream terminated properly (C 545-548). ----
            InflateMode::Done => break 'inf Z_STREAM_END,

            // ---- BAD: a data-format error (C 550-552). ----
            InflateMode::Bad => break 'inf Z_DATA_ERROR,

            // ---- Any other mode "can't happen" (C 554-557). ----
            _ => break 'inf Z_STREAM_ERROR,
        }
    };

    // ---- inf_leave: flush any leftover output (C 561-569). ----
    // `put + left == wsize`, so `wsize - left` bytes have been produced into the
    // window since the last flush.
    if left < wsize && output.write(&window[..wsize - left]) && ret == Z_STREAM_END {
        // The final flush failed: downgrade a successful return to Z_BUF_ERROR.
        ret = Z_BUF_ERROR;
    }
    // (C also writes back strm->next_in/avail_in here; in the pure-Rust trait
    // API that bookkeeping is the FFI shim's responsibility — see module docs.)
    ret
}

/// Finish an `inflateBack` stream and release its state.
///
/// This is the safe-Rust port of C `inflateBackEnd` (`infback.c`, lines
/// 572-579). In C this frees `strm->state` via `ZFREE`; here teardown is
/// **RAII** — taking the [`InflateState`] by value drops it at the end of this
/// function, which deterministically frees the owned
/// [`codes`](InflateState::codes) table (and any window backing) with no manual
/// free and no `unsafe` (AAP §0.6.3).
///
/// Always returns [`Z_OK`]. The C function's
/// `Z_STREAM_ERROR` path guards against null `strm`/`state`/`zfree`; those are
/// FFI-level concerns handled in `src/ffi.rs` (where a `ZStream`-based entry
/// would instead clear its `Option<Box<dyn StreamState>>` to `None`, triggering
/// the same [`Drop`]).
#[inline]
pub fn inflate_back_end(state: InflateState) -> i32 {
    // Explicit drop documents the intent (the RAII replacement for ZFREE);
    // it would also run automatically at end of scope.
    drop(state);
    Z_OK
}

#[cfg(all(test, feature = "std"))]
mod tests {
    //! Tests are gated behind `std` because they use the `flate2` reference
    //! oracle (which links canonical C zlib) to generate raw-DEFLATE vectors,
    //! and `Vec`/closures for the input/output adapters. The `back.rs` module
    //! itself remains `no_std`-clean (verified separately by
    //! `cargo build --no-default-features --features no-std`).

    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;
    use std::vec;
    use std::vec::Vec;

    // ---------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------

    /// Compress `data` into a **raw** DEFLATE stream (RFC 1951, no zlib/gzip
    /// wrapper) using the `flate2`/C-zlib oracle at the given level.
    fn raw_deflate(data: &[u8], level: u32) -> Vec<u8> {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level));
        enc.write_all(data).expect("deflate write");
        enc.finish().expect("deflate finish")
    }

    /// Drive [`inflate_back`] over `raw`, serving input in `chunk`-sized pieces
    /// and collecting the decompressed output. Returns `(return_code, output)`.
    fn decode_back(raw: &[u8], window_bits: i32, chunk: usize) -> (i32, Vec<u8>) {
        let mut state = inflate_back_init(window_bits).expect("valid windowBits");
        let mut window = vec![0u8; 1usize << window_bits];
        let mut input = SliceInput::with_chunk(raw, chunk);
        let mut out: Vec<u8> = Vec::new();
        let ret = {
            let mut sink = |buf: &[u8]| -> bool {
                out.extend_from_slice(buf);
                false
            };
            inflate_back(&mut state, &mut window, &mut input, &mut sink)
        };
        (ret, out)
    }

    /// Deterministic pseudo-random bytes (an LCG) so tests are reproducible
    /// without depending on a particular RNG.
    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push((s >> 33) as u8);
        }
        v
    }

    /// The block type of the first DEFLATE block: bits 1-2 of the first byte
    /// (`0` = stored, `1` = fixed, `2` = dynamic).
    fn first_block_btype(raw: &[u8]) -> u8 {
        (raw[0] >> 1) & 0x3
    }

    /// A minimal LSB-first DEFLATE bit writer used to hand-craft exact streams
    /// for tests that need a guaranteed block type or a deliberately malformed
    /// stream. Non-Huffman fields are written LSB-first via [`Self::bits`];
    /// Huffman codes are written MSB-first via [`Self::huff`] (the order the
    /// DEFLATE spec mandates for code bits).
    struct BitWriter {
        bytes: Vec<u8>,
        acc: u32,
        nbits: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            BitWriter {
                bytes: Vec::new(),
                acc: 0,
                nbits: 0,
            }
        }

        fn bits(&mut self, val: u32, n: u32) {
            self.acc |= (val & ((1u32 << n) - 1)) << self.nbits;
            self.nbits += n;
            while self.nbits >= 8 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.nbits -= 8;
            }
        }

        fn huff(&mut self, code: u32, n: u32) {
            // Emit the code most-significant-bit first.
            for i in (0..n).rev() {
                self.bits((code >> i) & 1, 1);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.nbits > 0 {
                self.bytes.push((self.acc & 0xff) as u8);
            }
            self.bytes
        }
    }

    // ---------------------------------------------------------------------
    // inflate_back_init / inflate_back_end
    // ---------------------------------------------------------------------

    #[test]
    fn init_validates_window_bits() {
        // Out of range -> Z_STREAM_ERROR. Use `.err()` (not `.unwrap_err()`)
        // so we never need `InflateState: Debug` for the discarded Ok value.
        assert_eq!(inflate_back_init(7).err(), Some(Z_STREAM_ERROR));
        assert_eq!(inflate_back_init(16).err(), Some(Z_STREAM_ERROR));
        assert_eq!(inflate_back_init(0).err(), Some(Z_STREAM_ERROR));
        assert_eq!(inflate_back_init(-1).err(), Some(Z_STREAM_ERROR));

        // In range -> Ok with the expected configuration.
        for wb in 8..=15 {
            let st = inflate_back_init(wb).expect("valid");
            assert_eq!(st.wbits, wb as u32);
            assert_eq!(st.wsize, 1u32 << wb);
            assert_eq!(st.dmax, 32768);
            assert!(st.sane);
            assert_eq!(st.whave, 0);
            assert_eq!(st.wnext, 0);
            assert_eq!(st.mode, InflateMode::Type);
            // The window is NOT owned here (caller supplies it to inflate_back).
            assert!(st.window.is_empty());
            // The decode-table buffer IS owned and fully allocated.
            assert_eq!(st.codes.len(), crate::inflate::tables::ENOUGH);
        }
    }

    #[test]
    fn back_end_returns_ok_and_drops() {
        let state = inflate_back_init(15).unwrap();
        assert_eq!(inflate_back_end(state), Z_OK);
    }

    // ---------------------------------------------------------------------
    // SliceInput adapter
    // ---------------------------------------------------------------------

    #[test]
    fn slice_input_serves_whole_then_empty() {
        let data = [1u8, 2, 3, 4, 5];
        let mut si = SliceInput::new(&data);
        assert_eq!(si.fill(), &data[..]);
        assert_eq!(si.fill(), &[][..]); // exhausted
        assert_eq!(si.fill(), &[][..]); // stays empty
    }

    #[test]
    fn slice_input_serves_in_chunks() {
        let data = [1u8, 2, 3, 4, 5];
        let mut si = SliceInput::with_chunk(&data, 2);
        assert_eq!(si.fill(), &[1, 2][..]);
        assert_eq!(si.fill(), &[3, 4][..]);
        assert_eq!(si.fill(), &[5][..]);
        assert_eq!(si.fill(), &[][..]);
    }

    #[test]
    fn slice_input_empty_buffer_signals_end_immediately() {
        let mut si = SliceInput::new(&[]);
        assert_eq!(si.fill(), &[][..]);
    }

    // ---------------------------------------------------------------------
    // Round-trip across levels, chunk sizes, and data shapes
    // ---------------------------------------------------------------------

    #[test]
    fn roundtrip_levels_chunks_and_shapes() {
        let corpus: Vec<Vec<u8>> = vec![
            Vec::new(),                               // empty
            b"A".to_vec(),                            // single byte
            b"hello, world".to_vec(),                 // short text
            b"abababababababababababababab".to_vec(), // periodic (matches)
            vec![b'z'; 1000],                         // long single-byte run
            b"The quick brown fox jumps over the lazy dog. ".repeat(40),
            pseudo_random(2048, 0x1234_5678_9abc_def0), // incompressible-ish
        ];

        for data in &corpus {
            for level in [0u32, 1, 6, 9] {
                let raw = raw_deflate(data, level);
                for chunk in [1usize, 7, raw.len().max(1)] {
                    let (ret, out) = decode_back(&raw, 15, chunk);
                    assert_eq!(
                        ret,
                        Z_STREAM_END,
                        "level={level} chunk={chunk} len={}",
                        data.len()
                    );
                    assert_eq!(&out, data, "level={level} chunk={chunk}");
                }
            }
        }
    }

    #[test]
    fn block_type_coverage_stored_fixed_dynamic() {
        // Assert that, across the chosen inputs/levels, we actually exercise
        // all three DEFLATE block types in the decoder.
        let mut seen_stored = false;
        let mut seen_fixed = false;
        let mut seen_dynamic = false;

        let candidates: [(Vec<u8>, u32); 4] = [
            (b"x".to_vec(), 0),    // tiny @ none -> stored
            (b"x".to_vec(), 9),    // tiny @ best -> fixed
            (vec![b'q'; 4096], 9), // long run @ best -> dynamic
            (b"The quick brown fox. ".repeat(60), 9),
        ];
        for (data, level) in &candidates {
            let raw = raw_deflate(data, *level);
            match first_block_btype(&raw) {
                0 => seen_stored = true,
                1 => seen_fixed = true,
                2 => seen_dynamic = true,
                _ => {}
            }
            // Whatever the block type, it must round-trip.
            let (ret, out) = decode_back(&raw, 15, raw.len().max(1));
            assert_eq!(ret, Z_STREAM_END);
            assert_eq!(&out, data);
        }
        assert!(seen_stored, "no stored block exercised");
        assert!(seen_fixed, "no fixed block exercised");
        assert!(seen_dynamic, "no dynamic block exercised");
    }

    // ---------------------------------------------------------------------
    // Multi-block, window wrapping, and ROOM flushing
    // ---------------------------------------------------------------------

    #[test]
    fn large_run_exercises_room_flush_and_overlap() {
        // 100_000 identical bytes -> distance-1 matches (overlapping run-copy),
        // output far larger than the 32 KiB window so ROOM flushes several
        // times and the window wraps. Distance 1 is safe for any window.
        let data = vec![b'a'; 100_000];
        let raw = raw_deflate(&data, 9);
        let (ret, out) = decode_back(&raw, 15, 4096);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, data);
    }

    #[test]
    fn periodic_pattern_wraps_window_with_nontrivial_offset() {
        // A 100-byte period repeated to >32 KiB: matches at distance 100, which
        // (combined with the window write cursor) forces the wrap branch of the
        // window->output copy. Distance 100 <= 32 KiB window, so it is safe.
        let mut pattern = Vec::new();
        for i in 0..100u32 {
            pattern.push((i as u8).wrapping_mul(31).wrapping_add(7));
        }
        let mut data = Vec::new();
        while data.len() < 60_000 {
            data.extend_from_slice(&pattern);
        }
        let raw = raw_deflate(&data, 6);
        let (ret, out) = decode_back(&raw, 15, 333);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, data);
    }

    #[test]
    fn small_window_decode_single_byte_run() {
        // windowBits = 8 (256-byte window). A single-byte run (distance 1) is
        // safe for a tiny window and stresses the small-wsize arithmetic and
        // frequent ROOM flushes.
        let data = vec![b'k'; 5_000];
        let raw = raw_deflate(&data, 9);
        let (ret, out) = decode_back(&raw, 8, 64);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, data);
    }

    #[test]
    fn multi_block_stream_roundtrips() {
        // ~200 KiB of varied text reliably spans multiple DEFLATE blocks.
        let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(3600);
        let raw = raw_deflate(&data, 6);
        let (ret, out) = decode_back(&raw, 15, 8192);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, data);
    }

    // ---------------------------------------------------------------------
    // Hand-crafted streams (guaranteed block types)
    // ---------------------------------------------------------------------

    #[test]
    fn handcrafted_fixed_block_literals() {
        // A fixed-Huffman block emitting the literals "Hi" then end-of-block.
        // Fixed lit/len codes: symbol 0..=143 is 8 bits, code = symbol + 0x30;
        // end-of-block (256) is 7 bits, code 0.
        let mut bw = BitWriter::new();
        bw.bits(1, 1); // BFINAL = 1
        bw.bits(1, 2); // BTYPE  = 01 (fixed)
        bw.huff((b'H' as u32) + 0x30, 8);
        bw.huff((b'i' as u32) + 0x30, 8);
        bw.huff(0, 7); // end of block
        let raw = bw.finish();

        let (ret, out) = decode_back(&raw, 15, raw.len());
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, b"Hi");
        // Sanity: the crafted stream is a genuine fixed block.
        assert_eq!(first_block_btype(&raw), 1);
    }

    #[test]
    fn handcrafted_stored_block() {
        // A stored block carrying the 4 bytes "data".
        // After BFINAL(1)+BTYPE(00) we are at bit 3; BYTEBITS advances to the
        // next byte boundary, then LEN (LE u16), NLEN (~LEN), then the bytes.
        let payload = b"data";
        let mut bw = BitWriter::new();
        bw.bits(1, 1); // BFINAL = 1
        bw.bits(0, 2); // BTYPE  = 00 (stored)
        // Pad to a byte boundary (BYTEBITS on the decode side).
        bw.bits(0, 5);
        let raw_header = bw.finish();
        let mut raw = raw_header;
        let len = payload.len() as u16;
        raw.extend_from_slice(&len.to_le_bytes());
        raw.extend_from_slice(&(!len).to_le_bytes());
        raw.extend_from_slice(payload);

        let (ret, out) = decode_back(&raw, 15, raw.len());
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, payload);
        assert_eq!(first_block_btype(&raw), 0);
    }

    // ---------------------------------------------------------------------
    // Error paths
    // ---------------------------------------------------------------------

    #[test]
    fn buf_error_on_truncated_input() {
        // Truncate a valid multi-byte stream to a single byte: the decoder will
        // exhaust input mid-stream and the empty refill yields Z_BUF_ERROR.
        let data = b"the quick brown fox".to_vec();
        let raw = raw_deflate(&data, 9);
        assert!(raw.len() >= 2);
        let truncated = &raw[..1];
        let (ret, _out) = decode_back(truncated, 15, 1);
        assert_eq!(ret, Z_BUF_ERROR);
    }

    #[test]
    fn buf_error_on_output_write_failure() {
        // A sink that always reports failure -> the leftover-flush at inf_leave
        // fails and downgrades the otherwise-successful return to Z_BUF_ERROR.
        let data = b"some output bytes".to_vec();
        let raw = raw_deflate(&data, 9);

        let mut state = inflate_back_init(15).unwrap();
        let mut window = vec![0u8; 1usize << 15];
        let mut input = SliceInput::new(&raw);
        let mut sink = |_buf: &[u8]| -> bool { true }; // always fail
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
        assert_eq!(ret, Z_BUF_ERROR);
    }

    #[test]
    fn data_error_invalid_block_type() {
        // First byte 0x06 => BFINAL=0, BTYPE=11 (the reserved/invalid type).
        let raw = [0x06u8];
        let (ret, _out) = decode_back(&raw, 15, 1);
        assert_eq!(ret, Z_DATA_ERROR);
    }

    #[test]
    fn data_error_bad_stored_length_complement() {
        // Stored block whose NLEN is not the ones-complement of LEN.
        let mut bw = BitWriter::new();
        bw.bits(1, 1); // BFINAL = 1
        bw.bits(0, 2); // BTYPE  = 00 (stored)
        bw.bits(0, 5); // pad to byte boundary
        let mut raw = bw.finish();
        raw.extend_from_slice(&5u16.to_le_bytes()); // LEN  = 5
        raw.extend_from_slice(&0u16.to_le_bytes()); // NLEN = 0 (should be !5)
        raw.extend_from_slice(&[0u8; 5]); // (never reached)

        let (ret, _out) = decode_back(&raw, 15, raw.len());
        assert_eq!(ret, Z_DATA_ERROR);
    }

    #[test]
    fn data_error_distance_too_far_back() {
        // A fixed block that begins with a match (length 3, distance 1) before
        // any history exists. At the stream start whave==0 and left==wsize, so
        // ANY positive distance is "too far back".
        // Fixed length symbol 257 (length 3) is the 7-bit code value 1;
        // fixed distance symbol 0 (distance 1) is the 5-bit code value 0.
        let mut bw = BitWriter::new();
        bw.bits(1, 1); // BFINAL = 1
        bw.bits(1, 2); // BTYPE  = 01 (fixed)
        bw.huff(1, 7); // length symbol 257 -> length 3, no extra bits
        bw.huff(0, 5); // distance symbol 0 -> distance 1, no extra bits
        let raw = bw.finish();

        let (ret, _out) = decode_back(&raw, 15, raw.len());
        assert_eq!(ret, Z_DATA_ERROR);
    }

    #[test]
    fn stream_error_on_undersized_window() {
        // The window must be exactly `1 << windowBits` bytes; a short buffer is
        // rejected as Z_STREAM_ERROR rather than panicking.
        let raw = raw_deflate(b"hello", 9);
        let mut state = inflate_back_init(15).unwrap();
        let mut window = vec![0u8; (1usize << 15) - 1]; // one byte short
        let mut input = SliceInput::new(&raw);
        let mut sink = |_buf: &[u8]| -> bool { false };
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
        assert_eq!(ret, Z_STREAM_ERROR);
    }

    // ---------------------------------------------------------------------
    // Re-use: one state can drive successive streams
    // ---------------------------------------------------------------------

    #[test]
    fn state_can_be_reused_for_successive_streams() {
        let mut state = inflate_back_init(15).unwrap();
        let mut window = vec![0u8; 1usize << 15];

        for msg in [&b"first stream"[..], &b"second, longer stream of bytes"[..]] {
            let raw = raw_deflate(msg, 6);
            let mut input = SliceInput::with_chunk(&raw, 5);
            let mut out: Vec<u8> = Vec::new();
            let ret = {
                let mut sink = |buf: &[u8]| -> bool {
                    out.extend_from_slice(buf);
                    false
                };
                inflate_back(&mut state, &mut window, &mut input, &mut sink)
            };
            assert_eq!(ret, Z_STREAM_END);
            assert_eq!(&out, msg);
        }
    }
}
