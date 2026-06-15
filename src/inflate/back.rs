//! `inflateBack` — callback-driven raw-DEFLATE decompression (port of `infback.c`).
//!
//! This module is the safe-Rust analogue of the C file `infback.c` from zlib
//! 1.3.2.1. It implements the three entry points of zlib's call-back inflate
//! interface:
//!
//! * [`inflate_back_init`] — the [`inflateBackInit_`] equivalent: validates
//!   `windowBits` and produces an [`InflateState`] configured for raw decode;
//! * [`inflate_back`] — the [`inflateBack`] equivalent: the main decode loop;
//!   and
//! * [`inflate_back_end`] — the [`inflateBackEnd`] equivalent: deterministic,
//!   RAII teardown.
//!
//! [`inflateBackInit_`]: https://www.zlib.net/manual.html
//! [`inflateBack`]: https://www.zlib.net/manual.html
//! [`inflateBackEnd`]: https://www.zlib.net/manual.html
//!
//! # What `inflateBack` is (and is not)
//!
//! Unlike [`crate::inflate`], `inflateBack` decodes a **raw DEFLATE stream
//! only** — RFC 1951 with **no** zlib (RFC 1950) or gzip (RFC 1952) header or
//! trailer, and therefore **no Adler-32 / CRC-32 checksum** is computed or
//! verified here. It is the engine used by utilities such as `gunzip`'s inner
//! loop where the wrapper is parsed separately.
//!
//! Its defining characteristic is that the caller supplies a single buffer that
//! serves as **both** the 2<sup>`windowBits`</sup>-byte sliding window **and**
//! the output buffer. Decoding fills that buffer; whenever it becomes full (or
//! the stream ends) the data is handed to the caller's output callback and the
//! buffer is reused. Input is likewise pulled on demand through the caller's
//! input callback. This makes `inflateBack` allocation-light and streaming, at
//! the cost of the more specialized callback contract described below.
//!
//! The C source notes (lines 7-10) that this code "is largely copied from
//! `inflate.c`"; accordingly this port reuses the shared table builder
//! ([`inflate_table`]) and the fixed-table installer ([`inflate_fixed`]) from
//! [`crate::inflate::tables`], and the decode-state types from
//! [`crate::inflate::state`].
//!
//! # Callback design (AAP §0.3.2 / §0.6.2)
//!
//! C uses two raw function pointers:
//!
//! ```c
//! typedef unsigned (*in_func)(void *, const unsigned char **);
//! typedef int      (*out_func)(void *, unsigned char *, unsigned);
//! ```
//!
//! Here those become the idiomatic [`BackInput`] and [`BackOutput`] traits. The
//! raw-pointer marshalling that turns the C `in_func`/`out_func` into these
//! traits lives **entirely in `crate::ffi`** (the crate's designated FFI
//! boundary) — this module is **100 % safe Rust** (see [Safety](#safety)).
//!
//! ## The input-lifetime contract
//!
//! C's `in_func` returns a pointer that "must not be changed until `in()` is
//! called again or `inflateBack()` returns". Modelling that faithfully in safe
//! Rust is subtle: the decoder must hold the current input chunk across many
//! bit-level reads and then ask for the *next* chunk. A naive
//! `fn fill(&mut self) -> &[u8]` (borrow tied to `&mut self`) cannot express
//! this — the borrow checker forbids re-calling `fill` while the previous
//! borrow is live. [`BackInput`] therefore carries an explicit lifetime
//! parameter `'a`: every chunk it returns lives for `'a` (the lifetime of the
//! underlying source), which is *independent* of the `&mut self` receiver, so
//! the decoder can hold one chunk and request the next. For the pure-Rust
//! adapters this is trivially sound; the `crate::ffi` adapter upholds it via the
//! documented C contract (the previous chunk is never read after the next
//! `fill`).
//!
//! # Safety
//!
//! This module is implemented in **100 % safe Rust** (AAP §0.6.2 / §0.7.2):
//! there are no raw-pointer dereferences and no bounds-check elision. Every
//! window/output copy — including the overlapping LZ77 match copy — is performed
//! with bounds-checked slice indexing. The performance-critical
//! [`inflate_fast`](crate::inflate::fast::inflate_fast) routine is intentionally
//! **not** invoked here: in `inflateBack` the window and the output are the same
//! buffer, which `inflate_fast`'s disjoint `window`/`output` slice signature
//! cannot express without aliasing (forbidden by the borrow checker). The slow,
//! always-correct per-symbol path below is byte-identical to C and is the sole
//! path used; `inflateBack` is a niche API and is not covered by the
//! decompression throughput gate (AAP §0.7.3), so omitting the fast path costs
//! nothing observable. See the `LEN` arm for details.
//!
//! This module is also `no_std`-clean: it references only [`core`] and the
//! crate's own modules, so it compiles under
//! `--no-default-features --features no-std`. There is no file I/O — the
//! callbacks abstract it.

// ===========================================================================
// Imports
// ===========================================================================

// Integer return codes — the exact C `Z_*` values. `inflateBack` can return
// Z_STREAM_END (success), Z_BUF_ERROR (a callback signalled "no input" / "write
// failed"), Z_DATA_ERROR (a DEFLATE format error), or Z_STREAM_ERROR (bad
// parameters / state). No allocation happens here, so Z_MEM_ERROR is not
// produced by this module.
use crate::constants::{Z_BUF_ERROR, Z_DATA_ERROR, Z_OK, Z_STREAM_END, Z_STREAM_ERROR};

// The decode-state machine and the resumable state struct (C `inflate_mode` /
// `struct inflate_state`). `inflateBack` exercises the raw-stream subset of the
// modes: TYPE, STORED, TABLE, LEN, DONE, BAD.
use crate::inflate::state::{InflateMode, InflateState};

// The shared two-level Huffman table builder, the fixed-table installer, the
// code-kind selector, and the decode-table entry type — all reused verbatim
// from the dynamic-block path in `crate::inflate`. `inflate_table` /
// `inflate_fixed` are `pub(crate)`; this module is in the same crate.
use crate::inflate::tables::{Code, CodeType, inflate_fixed, inflate_table};

// ===========================================================================
// Constants
// ===========================================================================

/// Permutation of the 19 code-length-code lengths (C `infback.c` `order[19]`).
///
/// The dynamic-block header transmits the code lengths for the 19-symbol
/// code-length alphabet in this scrambled order (most-frequently-nonzero
/// first), so reading them back applies this permutation. The values **must**
/// match C exactly or dynamic blocks decode incorrectly.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// The maximum match distance for a raw `inflateBack` stream, `1 << 15`
/// (C `state->dmax = 32768U` in `inflateBackInit_`). `inflateBack` does not use
/// `dmax` for its distance check (it uses the window-fill test instead), but the
/// field is initialized to match C.
const DMAX: u32 = 1 << 15;

// ===========================================================================
// Callback traits — the idiomatic replacement for C in_func / out_func
// ===========================================================================

/// An input source for [`inflate_back`].
///
/// This is the safe-Rust analogue of C's `in_func`. Each call to [`fill`] hands
/// back the next chunk of compressed input; an **empty** slice signals "no more
/// input is available", which causes [`inflate_back`] to stop with
/// [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR) — exactly as C returns
/// `Z_BUF_ERROR` when `in()` returns `0`.
///
/// [`fill`]: BackInput::fill
///
/// # Lifetime contract
///
/// The returned slice borrows for `'a` — the lifetime of the underlying input
/// source — **not** for the duration of the `&mut self` receiver. This is the
/// crux of porting C's "valid until `in()` is called again" rule to safe Rust:
/// the decoder holds one chunk (consuming it byte by byte) and, once exhausted,
/// calls [`fill`] again for the next. Tying the chunk's lifetime to `'a` rather
/// than to `&mut self` is what lets those two borrows coexist (see the
/// [module docs](self#the-input-lifetime-contract)).
///
/// Implementations that already hold all input in memory (e.g. [`SliceInput`])
/// satisfy this trivially. The `crate::ffi` adapter that wraps a C `in_func`
/// upholds it through the documented C contract — and that is where the FFI
/// raw-pointer code lives, never here.
pub trait BackInput<'a> {
    /// Returns the next chunk of input, or an empty slice to signal end of
    /// input (which yields [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR)).
    fn fill(&mut self) -> &'a [u8];
}

/// An output sink for [`inflate_back`].
///
/// This is the safe-Rust analogue of C's `out_func`. [`write`] is handed each
/// run of decoded bytes (a full window, or the final partial window on
/// completion). It returns `true` to indicate a **write failure**, which causes
/// [`inflate_back`] to stop with [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR)
/// — mirroring C's convention that `out()` returns non-zero on failure.
///
/// [`write`]: BackOutput::write
///
/// A blanket implementation is provided for any `FnMut(&[u8]) -> bool`, so a
/// closure can be passed directly as the output sink.
pub trait BackOutput {
    /// Consumes `buf`; returns `true` on write failure (yields
    /// [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR)), `false` on success.
    fn write(&mut self, buf: &[u8]) -> bool;
}

/// Any `FnMut(&[u8]) -> bool` is a [`BackOutput`]: the closure receives each
/// output run and returns `true` on failure. This lets callers pass an
/// output closure directly without defining a type.
impl<F: FnMut(&[u8]) -> bool> BackOutput for F {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> bool {
        self(buf)
    }
}

/// A [`BackInput`] adapter that yields an in-memory slice exactly once, then
/// reports end-of-input.
///
/// This is the common case for the pure-Rust API: all compressed input is
/// already in memory. The first [`fill`](BackInput::fill) returns the whole
/// slice; every subsequent call returns an empty slice. Because the slice is
/// borrowed for `'a`, it satisfies the [`BackInput`] lifetime contract in fully
/// safe code.
///
/// For chunked or streaming input, implement [`BackInput`] directly.
///
/// # Examples
///
/// ```ignore
/// let mut input = SliceInput::new(&compressed);
/// let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
/// ```
pub struct SliceInput<'a> {
    data: &'a [u8],
    done: bool,
}

impl<'a> SliceInput<'a> {
    /// Wraps `data` as a one-shot [`BackInput`].
    #[inline]
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        SliceInput { data, done: false }
    }
}

impl<'a> BackInput<'a> for SliceInput<'a> {
    #[inline]
    fn fill(&mut self) -> &'a [u8] {
        if self.done {
            &[]
        } else {
            self.done = true;
            self.data
        }
    }
}

// ===========================================================================
// Phase A — inflate_back_init  (C inflateBackInit_)
// ===========================================================================

/// Initializes an [`InflateState`] for raw-DEFLATE call-back decoding — the
/// safe-Rust analogue of C `inflateBackInit_` (`infback.c` lines 25-64).
///
/// `window_bits` selects the window size as `1 << window_bits` and **must** lie
/// in `8..=15`; any other value yields `Err(`[`Z_STREAM_ERROR`]`)`, exactly as
/// C returns `Z_STREAM_ERROR` for an out-of-range `windowBits`. (The C
/// `version`/`stream_size` ABI checks — and the `Z_VERSION_ERROR` they can
/// produce — belong to the `crate::ffi` shim that wraps this function, not to
/// the safe core.)
///
/// Unlike the streaming [`crate::inflate`] engine, the window buffer is **not**
/// allocated here: in `inflateBack` the caller supplies the window/output buffer
/// directly to [`inflate_back`]. This mirrors C, where `inflateBackInit_` merely
/// records `state->window = window`. Consequently the returned state's
/// [`window`](InflateState::window) field stays empty and is never used by this
/// module — all window access goes through the `window` slice argument of
/// [`inflate_back`].
///
/// The returned state is configured as C does it:
///
/// * [`dmax`](InflateState::dmax) = `32768`;
/// * [`wbits`](InflateState::wbits) = `window_bits`;
/// * [`wsize`](InflateState::wsize) = `1 << window_bits`;
/// * [`wnext`](InflateState::wnext) = `0`, [`whave`](InflateState::whave) = `0`;
/// * [`sane`](InflateState::sane) = `true`;
/// * [`mode`](InflateState::mode) = [`InflateMode::Type`] (ready to read the
///   first block header).
///
/// The Huffman decode-table backing store ([`codes`](InflateState::codes), of
/// length `ENOUGH`) is allocated by the [`InflateState::new`] constructor. The
/// `flags`/`wrap` fields are irrelevant for a raw stream and are left at their
/// constructor defaults.
///
/// [`Z_STREAM_ERROR`]: crate::constants::Z_STREAM_ERROR
///
/// # Errors
///
/// Returns `Err(`[`Z_STREAM_ERROR`]`)` if `window_bits` is not in `8..=15`.
pub fn inflate_back_init(window_bits: i32) -> Result<InflateState, i32> {
    // C: `if (windowBits < 8 || windowBits > 15) return Z_STREAM_ERROR;`
    if !(8..=15).contains(&window_bits) {
        return Err(Z_STREAM_ERROR);
    }

    let mut state = InflateState::new();
    state.dmax = DMAX;
    state.wbits = window_bits as u32;
    state.wsize = 1u32 << window_bits;
    state.wnext = 0;
    state.whave = 0;
    state.sane = true;
    // `mode = TYPE`: ready to dispatch the first deflate block. (C leaves
    // `mode` as the post-allocation default and `inflateBack` resets it to TYPE;
    // we set it here too so a freshly initialized state is self-consistent.)
    state.mode = InflateMode::Type;

    Ok(state)
}

// ===========================================================================
// Phase B — inflate_back  (C inflateBack main loop)
// ===========================================================================

/// Decodes a complete raw-DEFLATE stream using caller-supplied callbacks — the
/// safe-Rust analogue of C `inflateBack` (`infback.c` lines 196-570).
///
/// `state` must have been produced by [`inflate_back_init`]. `window` is the
/// caller's combined window/output buffer; it must be at least
/// `1 << windowBits` = [`state.wsize`](InflateState::wsize) bytes (C requires it
/// to be exactly that size). `input` supplies compressed bytes on demand and
/// `output` receives decoded bytes; see [`BackInput`] / [`BackOutput`].
///
/// # Returns
///
/// The C `int` status code:
///
/// * [`Z_STREAM_END`](crate::constants::Z_STREAM_END) — the stream decoded
///   successfully (a final block was seen and all output flushed);
/// * [`Z_BUF_ERROR`](crate::constants::Z_BUF_ERROR) — [`input.fill`] returned an
///   empty slice mid-stream, or [`output.write`] reported failure;
/// * [`Z_DATA_ERROR`](crate::constants::Z_DATA_ERROR) — the input is not a valid
///   DEFLATE stream;
/// * [`Z_STREAM_ERROR`](crate::constants::Z_STREAM_ERROR) — `window` is smaller
///   than `state.wsize` (a parameter error; C assumes the exact size and would
///   otherwise read/write out of bounds — this safe port rejects it instead).
///
/// [`input.fill`]: BackInput::fill
/// [`output.write`]: BackOutput::write
///
/// # Determinism
///
/// The decoded byte stream is **identical** to C `inflateBack` for any input:
/// the same Huffman tables are built (via [`inflate_table`]/[`inflate_fixed`]),
/// the same bit-accumulator cadence is used, and the same window-wrap match-copy
/// arithmetic is performed. No checksum is computed (raw DEFLATE has no
/// trailer).
pub fn inflate_back<'a, In, Out>(
    state: &mut InflateState,
    window: &mut [u8],
    input: &mut In,
    output: &mut Out,
) -> i32
where
    In: BackInput<'a>,
    Out: BackOutput,
{
    // Logical window size (= 1 << windowBits). The caller's `window` buffer must
    // be at least this long. C assumes exactly `wsize`; this safe port checks to
    // keep all indexing panic-free for valid callers and to convert a misuse
    // into Z_STREAM_ERROR rather than out-of-bounds behavior.
    let wsize = state.wsize as usize;
    if window.len() < wsize {
        return Z_STREAM_ERROR;
    }

    // --- Reset the state (C lines 211-223) -------------------------------
    state.mode = InflateMode::Type;
    state.last = false;
    state.whave = 0;

    // --- Locals (the C register-cached working set) ----------------------
    //
    // `cur`/`next`/`have` model C's `next` pointer + `have` count: `cur` is the
    // current input chunk (borrowed for `'a`), `next` is the consume offset into
    // it, and `have` is the number of bytes remaining (`cur.len() - next`). For
    // the pure-Rust API we start empty, so the first `pull!` invokes
    // `input.fill()` (C seeds these from `strm->next_in`/`avail_in`; the FFI
    // shim can do likewise before calling).
    let mut cur: &'a [u8] = &[];
    let mut next: usize = 0; // consume offset into `cur`
    let mut have: usize = 0; // input bytes available (== cur.len() - next)
    let mut hold: u32 = 0; // bit accumulator (C `unsigned long hold`, portable path)
    let mut bits: u32 = 0; // number of valid bits in `hold`
    let mut put: usize = 0; // write offset into `window`
    let mut left: usize = wsize; // output space remaining (put + left == wsize)

    // --- Macros: faithful ports of the C infback.c macros ----------------
    //
    // Each macro that can fail takes the loop label `$lbl` and exits via
    // `break $lbl <code>` — the structured replacement for C's
    // `goto inf_leave`. (macro_rules! labels are hygienic, so the label MUST be
    // threaded through explicitly; free *variable* references resolve to these
    // locals as written.)

    /// C `PULL()`: ensure some input is available, else bail with Z_BUF_ERROR.
    macro_rules! pull {
        ($lbl:lifetime) => {{
            if have == 0 {
                cur = input.fill();
                next = 0;
                have = cur.len();
                if have == 0 {
                    break $lbl Z_BUF_ERROR;
                }
            }
        }};
    }

    /// C `PULLBYTE()`: pull one input byte into the bit accumulator.
    macro_rules! pullbyte {
        ($lbl:lifetime) => {{
            pull!($lbl);
            let byte = cur[next];
            next += 1;
            have -= 1;
            hold |= (byte as u32) << bits;
            bits += 8;
        }};
    }

    /// C `NEEDBITS(n)`: ensure at least `n` bits are in the accumulator.
    macro_rules! needbits {
        ($lbl:lifetime, $n:expr) => {{
            while bits < ($n) {
                pullbyte!($lbl);
            }
        }};
    }

    /// C `BITS(n)`: the low `n` bits of the accumulator. Parenthesized so it
    /// composes correctly when followed by `>>` (a higher-precedence operator).
    macro_rules! get_bits {
        ($n:expr) => {
            (hold & ((1u32 << ($n)) - 1))
        };
    }

    /// C `DROPBITS(n)`: discard the low `n` bits.
    macro_rules! dropbits {
        ($n:expr) => {{
            hold >>= ($n);
            bits -= ($n);
        }};
    }

    /// C `BYTEBITS()`: discard 0-7 bits to reach a byte boundary.
    macro_rules! bytebits {
        () => {{
            hold >>= bits & 7;
            bits -= bits & 7;
        }};
    }

    /// C `INITBITS()`: clear the bit accumulator.
    macro_rules! initbits {
        () => {{
            hold = 0;
            bits = 0;
        }};
    }

    /// C `ROOM()`: if the window is full, flush it and reuse it. On a flush the
    /// whole window becomes valid history (`whave = wsize`), so later matches can
    /// reach back across the entire window.
    macro_rules! room {
        ($lbl:lifetime) => {{
            if left == 0 {
                put = 0;
                left = wsize;
                state.whave = wsize as u32;
                if output.write(&window[..wsize]) {
                    break $lbl Z_BUF_ERROR;
                }
            }
        }};
    }

    // --- Main decode loop (C `for (;;) switch (state->mode)`) -------------
    //
    // `goto inf_leave` is modelled by `break 'inf <code>`; the labeled loop
    // yields the return code, then the shared leave handler below flushes any
    // residual output. C `case` fall-through (TABLE -> LEN) is reproduced by
    // setting `state.mode` and letting the loop re-dispatch — observationally
    // identical, since no work happens between the assignment and the next case.
    let ret: i32 = 'inf: loop {
        match state.mode {
            // -- TYPE: determine and dispatch the block type (C 228-260) --
            InflateMode::Type => {
                if state.last {
                    // Align to a byte boundary and finish.
                    bytebits!();
                    state.mode = InflateMode::Done;
                } else {
                    needbits!('inf, 3);
                    state.last = get_bits!(1) == 1;
                    dropbits!(1);
                    match get_bits!(2) {
                        0 => state.mode = InflateMode::Stored, // stored block
                        1 => {
                            // Fixed Huffman block: install the static tables.
                            // Do NOT advance `state.next` (C keeps `next` at the
                            // base of `codes` for fixed blocks).
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

            // -- STORED: copy a stored (uncompressed) block (C 262-292) ----
            InflateMode::Stored => {
                // Go to a byte boundary, then read LEN and its one's-complement.
                bytebits!();
                needbits!('inf, 32);
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    state.mode = InflateMode::Bad; // "invalid stored block lengths"
                } else {
                    state.length = hold & 0xffff;
                    initbits!();
                    // Copy `length` bytes straight from input to the window.
                    while state.length != 0 {
                        let mut copy = state.length as usize;
                        pull!('inf);
                        room!('inf);
                        if copy > have {
                            copy = have;
                        }
                        if copy > left {
                            copy = left;
                        }
                        // Disjoint buffers (input vs window): a bulk copy is safe.
                        window[put..put + copy].copy_from_slice(&cur[next..next + copy]);
                        have -= copy;
                        next += copy;
                        left -= copy;
                        put += copy;
                        state.length -= copy as u32;
                    }
                    state.mode = InflateMode::Type;
                }
            }

            // -- TABLE: read the dynamic Huffman code descriptors (C 294-419)
            InflateMode::Table => {
                'table: {
                    needbits!('inf, 14);
                    state.nlen = get_bits!(5) + 257;
                    dropbits!(5);
                    state.ndist = get_bits!(5) + 1;
                    dropbits!(5);
                    state.ncode = get_bits!(4) + 4;
                    dropbits!(4);
                    if state.nlen > 286 || state.ndist > 30 {
                        // "too many length or distance symbols"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Read the code-length-code lengths in the scrambled order.
                    state.have = 0;
                    while state.have < state.ncode {
                        needbits!('inf, 3);
                        state.lens[ORDER[state.have as usize] as usize] = get_bits!(3) as u16;
                        state.have += 1;
                        dropbits!(3);
                    }
                    while state.have < 19 {
                        state.lens[ORDER[state.have as usize] as usize] = 0;
                        state.have += 1;
                    }

                    // Build the code-length-code table (root = 7 bits).
                    state.next = 0;
                    state.lencode = 0;
                    state.lenbits = 7;
                    let r = inflate_table(
                        CodeType::Codes,
                        &state.lens,
                        19,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.lenbits,
                        &mut state.work,
                    );
                    if r != 0 {
                        // "invalid code lengths set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Decode the `nlen + ndist` literal/length and distance code
                    // lengths, expanding the 16/17/18 repeat codes.
                    state.have = 0;
                    let total = state.nlen + state.ndist;
                    while state.have < total {
                        let here: Code = loop {
                            let h = state.codes[state.lencode + get_bits!(state.lenbits) as usize];
                            if (h.bits as u32) <= bits {
                                break h;
                            }
                            pullbyte!('inf);
                        };
                        if here.val < 16 {
                            dropbits!(here.bits as u32);
                            state.lens[state.have as usize] = here.val;
                            state.have += 1;
                        } else {
                            let len: u16;
                            let mut copy: u32;
                            if here.val == 16 {
                                needbits!('inf, here.bits as u32 + 2);
                                dropbits!(here.bits as u32);
                                if state.have == 0 {
                                    // "invalid bit length repeat"
                                    state.mode = InflateMode::Bad;
                                    break;
                                }
                                len = state.lens[state.have as usize - 1];
                                copy = 3 + get_bits!(2);
                                dropbits!(2);
                            } else if here.val == 17 {
                                needbits!('inf, here.bits as u32 + 3);
                                dropbits!(here.bits as u32);
                                len = 0;
                                copy = 3 + get_bits!(3);
                                dropbits!(3);
                            } else {
                                needbits!('inf, here.bits as u32 + 7);
                                dropbits!(here.bits as u32);
                                len = 0;
                                copy = 11 + get_bits!(7);
                                dropbits!(7);
                            }
                            if state.have + copy > total {
                                // "invalid bit length repeat"
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

                    // Propagate an error break from the decode loop.
                    if state.mode == InflateMode::Bad {
                        break 'table;
                    }

                    // There must be an end-of-block code.
                    if state.lens[256] == 0 {
                        // "invalid code -- missing end-of-block"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }

                    // Build the literal/length and distance tables. The root bit
                    // counts (9 and 6) MUST NOT change — the ENOUGH sizing in
                    // `inftrees.h` depends on them.
                    let nlen = state.nlen as usize;
                    let ndist = state.ndist as usize;
                    state.next = 0;
                    state.lencode = 0;
                    state.lenbits = 9;
                    let r = inflate_table(
                        CodeType::Lens,
                        &state.lens,
                        nlen,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.lenbits,
                        &mut state.work,
                    );
                    if r != 0 {
                        // "invalid literal/lengths set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }
                    state.distcode = state.next;
                    state.distbits = 6;
                    let r = inflate_table(
                        CodeType::Dists,
                        &state.lens[nlen..],
                        ndist,
                        &mut state.codes,
                        &mut state.next,
                        &mut state.distbits,
                        &mut state.work,
                    );
                    if r != 0 {
                        // "invalid distances set"
                        state.mode = InflateMode::Bad;
                        break 'table;
                    }
                    // Fall through to LEN (C `state->mode = LEN; /* fallthrough */`).
                    state.mode = InflateMode::Len;
                }
            }

            // -- LEN: decode one literal/length code, then maybe a match ---
            //
            // C optionally calls `inflate_fast` here when `have >= 6 && left >=
            // 258`. We deliberately omit it: in `inflateBack` the window and the
            // output are the SAME buffer, but `inflate_fast` takes the window
            // (via `state`) and the output as two disjoint slices, which cannot
            // be the same allocation without aliasing the borrow checker forbids.
            // The slow per-symbol path below is byte-identical and always
            // correct, and `inflateBack` is not perf-gated (AAP §0.7.3).
            InflateMode::Len => {
                // Decode the literal/length code (with 2nd-level sub-table
                // indirection when the root entry is a table link).
                let mut here: Code = loop {
                    let h = state.codes[state.lencode + get_bits!(state.lenbits) as usize];
                    if (h.bits as u32) <= bits {
                        break h;
                    }
                    pullbyte!('inf);
                };
                if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last_code = here;
                    here = loop {
                        let idx = state.lencode
                            + last_code.val as usize
                            + ((get_bits!(last_code.bits as u32 + last_code.op as u32)
                                >> last_code.bits) as usize);
                        let h = state.codes[idx];
                        if (last_code.bits as u32 + h.bits as u32) <= bits {
                            break h;
                        }
                        pullbyte!('inf);
                    };
                    dropbits!(last_code.bits as u32);
                }
                dropbits!(here.bits as u32);
                state.length = here.val as u32;

                if here.op == 0 {
                    // Literal byte.
                    room!('inf);
                    window[put] = state.length as u8;
                    put += 1;
                    left -= 1;
                    // mode stays LEN
                } else if here.op & 32 != 0 {
                    // End of block.
                    state.mode = InflateMode::Type;
                } else if here.op & 64 != 0 {
                    // "invalid literal/length code"
                    state.mode = InflateMode::Bad;
                } else {
                    // Length code: add the length extra bits.
                    state.extra = (here.op as u32) & 15;
                    if state.extra != 0 {
                        needbits!('inf, state.extra);
                        state.length += get_bits!(state.extra);
                        dropbits!(state.extra);
                    }

                    // Decode the distance code (with 2nd-level indirection).
                    let mut dhere: Code = loop {
                        let h = state.codes[state.distcode + get_bits!(state.distbits) as usize];
                        if (h.bits as u32) <= bits {
                            break h;
                        }
                        pullbyte!('inf);
                    };
                    if (dhere.op & 0xf0) == 0 {
                        let last_code = dhere;
                        dhere = loop {
                            let idx = state.distcode
                                + last_code.val as usize
                                + ((get_bits!(last_code.bits as u32 + last_code.op as u32)
                                    >> last_code.bits) as usize);
                            let h = state.codes[idx];
                            if (last_code.bits as u32 + h.bits as u32) <= bits {
                                break h;
                            }
                            pullbyte!('inf);
                        };
                        dropbits!(last_code.bits as u32);
                    }
                    dropbits!(dhere.bits as u32);

                    if dhere.op & 64 != 0 {
                        // "invalid distance code"
                        state.mode = InflateMode::Bad;
                    } else {
                        state.offset = dhere.val as u32;
                        state.extra = (dhere.op as u32) & 15;
                        if state.extra != 0 {
                            needbits!('inf, state.extra);
                            state.offset += get_bits!(state.extra);
                            dropbits!(state.extra);
                        }
                        // Distance-too-far check (the inflateBack form, which
                        // differs from inflate.c: it uses the window-fill state
                        // rather than `dmax`).
                        let limit = state.wsize
                            - if state.whave < state.wsize {
                                left as u32
                            } else {
                                0
                            };
                        if state.offset > limit {
                            // "invalid distance too far back"
                            state.mode = InflateMode::Bad;
                        } else {
                            // Copy the match from the window to the output
                            // (which are the same buffer). The source may wrap
                            // around the end of the window, and the copy may
                            // overlap (LZ77 run), so a byte-by-byte forward copy
                            // is required — NOT `copy_from_slice`.
                            loop {
                                room!('inf);
                                let off = state.offset as usize;
                                let mut copy = wsize - off; // bytes to window end from wrap source
                                let from: usize;
                                if copy < left {
                                    // Source wraps: start at the window tail.
                                    from = put + copy;
                                    copy = left - copy;
                                } else {
                                    // Source is contiguous behind `put`.
                                    from = put - off;
                                    copy = left;
                                }
                                if copy > state.length as usize {
                                    copy = state.length as usize;
                                }
                                state.length -= copy as u32;
                                left -= copy;
                                // Byte-by-byte forward copy — NOT `copy_within`
                                // or `copy_from_slice`. For a run-length match
                                // the source region overlaps the destination,
                                // and each freshly written byte must be visible
                                // to a later read within the same run (this is
                                // how DEFLATE encodes runs). Indexing with a
                                // single range counter `i` (rather than two
                                // advancing cursors) preserves that exact
                                // ordering while satisfying clippy's
                                // `explicit_counter_loop`. A temporary `b`
                                // sidesteps the simultaneous &/&mut borrow of
                                // `window` that `window[a] = window[b]` would
                                // require.
                                for i in 0..copy {
                                    let b = window[from + i];
                                    window[put + i] = b;
                                }
                                put += copy;
                                if state.length == 0 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // -- DONE: stream terminated properly (C 545-548) --------------
            InflateMode::Done => break 'inf Z_STREAM_END,

            // -- BAD: a deflate format error (C 550-552) -------------------
            InflateMode::Bad => break 'inf Z_DATA_ERROR,

            // -- Any other mode is unreachable for inflateBack (C default) -
            _ => break 'inf Z_STREAM_ERROR,
        }
    };

    // --- inf_leave: flush leftover output, return (C 561-569) ------------
    //
    // `put == wsize - left` between flushes, so the unflushed bytes are exactly
    // `window[..wsize - left]`. If that final write fails on an otherwise
    // successful stream, downgrade the result to Z_BUF_ERROR (C lines 562-566).
    let mut ret = ret;
    if left < wsize && output.write(&window[..wsize - left]) && ret == Z_STREAM_END {
        ret = Z_BUF_ERROR;
    }
    // (The C `strm->next_in = next; strm->avail_in = have;` update applies to the
    // ZStream/FFI entry; the pure-Rust slice API has no z_stream to update. The
    // `crate::ffi` shim restores the unconsumed input from `next`/`have`.)
    ret
}

// ===========================================================================
// Phase C — inflate_back_end  (C inflateBackEnd)
// ===========================================================================

/// Releases an [`InflateState`] created by [`inflate_back_init`] — the
/// safe-Rust analogue of C `inflateBackEnd` (`infback.c` lines 572-579).
///
/// In C this calls `ZFREE(strm, strm->state)` and nulls the pointer. Here the
/// state is taken **by value** and dropped: its owned buffers
/// ([`codes`](InflateState::codes), and the unused
/// [`window`](InflateState::window) `Vec`) are freed automatically and
/// leak-free by [`Drop`] (AAP §0.6.3). The user-supplied window/output buffer
/// passed to [`inflate_back`] is **not** owned by the state and is therefore
/// untouched. Always returns [`Z_OK`](crate::constants::Z_OK).
///
/// The C `Z_STREAM_ERROR` validation (null `strm`/`state`/`zfree`) guards
/// against malformed C handles and is performed in the `crate::ffi` shim before
/// it forwards to this function; an owned [`InflateState`] cannot be in those
/// invalid states.
#[inline]
pub fn inflate_back_end(state: InflateState) -> i32 {
    // Explicit drop documents the RAII teardown (the idiomatic `inflateEnd` /
    // `inflateBackEnd` replacement). The buffers free here.
    drop(state);
    Z_OK
}

// ===========================================================================
// Tests
// ===========================================================================
//
// These tests exercise the full `inflateBack` surface against raw-DEFLATE
// streams produced by `flate2` (which links canonical C zlib — the dev-only
// oracle, AAP §0.5.2). They are `std`-only by virtue of `#[cfg(test)]`: the
// `no-std` build never compiles them.

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress, Status};

    // -- Test helpers -----------------------------------------------------

    /// A chunked [`BackInput`]: yields each pre-sliced piece of compressed
    /// input in turn, then an empty slice (end of input).
    struct ChunkedInput<'a> {
        chunks: Vec<&'a [u8]>,
        idx: usize,
    }

    impl<'a> BackInput<'a> for ChunkedInput<'a> {
        fn fill(&mut self) -> &'a [u8] {
            if self.idx < self.chunks.len() {
                let c = self.chunks[self.idx];
                self.idx += 1;
                c
            } else {
                &[]
            }
        }
    }

    /// A [`BackOutput`] that accumulates everything written into an owned `Vec`.
    struct VecSink {
        out: Vec<u8>,
    }

    impl BackOutput for VecSink {
        fn write(&mut self, buf: &[u8]) -> bool {
            self.out.extend_from_slice(buf);
            false
        }
    }

    /// A [`BackOutput`] that always reports failure (to drive the Z_BUF_ERROR
    /// output path).
    struct FailingSink;

    impl BackOutput for FailingSink {
        fn write(&mut self, _buf: &[u8]) -> bool {
            true
        }
    }

    /// Produce a raw-DEFLATE (RFC 1951, no zlib/gzip wrapper) stream for `data`
    /// at the given `level` and `window_bits` using the canonical C zlib via
    /// `flate2`. This is the byte-for-byte oracle our decoder must accept.
    fn deflate_raw(data: &[u8], level: u32, window_bits: u8) -> Vec<u8> {
        let mut c = Compress::new_with_window_bits(Compression::new(level), false, window_bits);
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let consumed = c.total_in() as usize;
            let produced_before = c.total_out() as usize;
            let status = c
                .compress(&data[consumed..], &mut buf, FlushCompress::Finish)
                .expect("flate2 compress");
            let produced = c.total_out() as usize - produced_before;
            out.extend_from_slice(&buf[..produced]);
            if status == Status::StreamEnd {
                break;
            }
            // Guard against a stuck loop (should not happen with Finish).
            if produced == 0 && c.total_in() as usize == consumed {
                break;
            }
        }
        out
    }

    /// Decode `chunks` (a raw-DEFLATE stream split into input pieces) through
    /// [`inflate_back`] with a `1 << window_bits` window, returning the status
    /// code and the reconstructed output.
    fn run_back(chunks: &[&[u8]], window_bits: i32) -> (i32, Vec<u8>) {
        let mut state = inflate_back_init(window_bits).expect("init");
        let mut window = vec![0u8; 1usize << window_bits];
        let mut input = ChunkedInput {
            chunks: chunks.to_vec(),
            idx: 0,
        };
        let mut sink = VecSink { out: Vec::new() };
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
        (ret, sink.out)
    }

    /// Deterministic pseudo-random bytes (a simple LCG) — incompressible enough
    /// that the encoder emits real literals/matches rather than trivial runs.
    fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            v.push((s >> 24) as u8);
        }
        v
    }

    // -- Round-trip tests -------------------------------------------------

    /// A dynamic-Huffman block (default level) round-trips byte-for-byte.
    #[test]
    fn round_trip_dynamic() {
        let original = b"The quick brown fox jumps over the lazy dog. ".repeat(40);
        let comp = deflate_raw(&original, 6, 15);
        let (ret, out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    /// A stored (uncompressed) block round-trips (level 0 forces stored blocks).
    #[test]
    fn round_trip_stored() {
        let original = pseudo_random(2000, 0x1234_5678);
        let comp = deflate_raw(&original, 0, 15);
        let (ret, out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    /// A fixed-Huffman block round-trips (short, highly compressible input tends
    /// to select fixed codes; whichever block type is chosen must round-trip).
    #[test]
    fn round_trip_fixed_short() {
        let original = b"aaaaaaaaaabbbbbbbbbbcccccccccc".to_vec();
        let comp = deflate_raw(&original, 9, 15);
        let (ret, out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    /// The empty input round-trips to empty output with Z_STREAM_END.
    #[test]
    fn round_trip_empty() {
        let original: Vec<u8> = Vec::new();
        let comp = deflate_raw(&original, 6, 15);
        let (ret, out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_STREAM_END);
        assert!(out.is_empty());
    }

    /// Input delivered in many small chunks (exercising the `pull!`/refill path
    /// repeatedly) still decodes correctly.
    #[test]
    fn round_trip_chunked_input() {
        let original = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(30);
        let comp = deflate_raw(&original, 6, 15);
        // Split the compressed stream into 1-byte chunks.
        let chunks: Vec<&[u8]> = comp.chunks(1).collect();
        let (ret, out) = run_back(&chunks, 15);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    /// A stream larger than the window forces repeated `ROOM` flushes and
    /// window-wrapping back-references; it must still reconstruct exactly.
    #[test]
    fn round_trip_small_window_multiblock() {
        // ~6 KiB of repetitive-but-varied data; window is only 512 bytes.
        let mut original = Vec::new();
        for i in 0..200u32 {
            original.extend_from_slice(
                format!("line {i:04} of the streaming test payload; ").as_bytes(),
            );
        }
        let comp = deflate_raw(&original, 6, 9); // encode with the SAME 512-byte window
        let (ret, out) = run_back(&[&comp], 9);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    /// A larger pseudo-random payload (mix of stored/dynamic blocks across the
    /// 32 KiB window) round-trips.
    #[test]
    fn round_trip_large_pseudo_random() {
        let original = pseudo_random(50_000, 0xDEAD_BEEF);
        let comp = deflate_raw(&original, 6, 15);
        let (ret, out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(out, original);
    }

    // -- Error-path tests -------------------------------------------------

    /// Truncated input (the callback runs out mid-stream) yields Z_BUF_ERROR.
    #[test]
    fn truncated_input_yields_buf_error() {
        let original = b"The quick brown fox jumps over the lazy dog. ".repeat(40);
        let comp = deflate_raw(&original, 6, 15);
        let truncated = &comp[..comp.len() / 2];
        let (ret, _out) = run_back(&[truncated], 15);
        assert_eq!(ret, Z_BUF_ERROR);
    }

    /// An empty input chunk before any data yields Z_BUF_ERROR (the very first
    /// `pull!` fails).
    #[test]
    fn no_input_yields_buf_error() {
        let (ret, out) = run_back(&[], 15);
        assert_eq!(ret, Z_BUF_ERROR);
        assert!(out.is_empty());
    }

    /// An invalid DEFLATE block type (BTYPE = 3) yields Z_DATA_ERROR.
    #[test]
    fn invalid_block_type_yields_data_error() {
        // 0b0000_0111: BFINAL = 1, BTYPE = 0b11 = 3 (reserved/invalid).
        let bad = [0x07u8];
        let (ret, _out) = run_back(&[&bad], 15);
        assert_eq!(ret, Z_DATA_ERROR);
    }

    /// A corrupted stored-block length complement yields Z_DATA_ERROR.
    #[test]
    fn bad_stored_complement_yields_data_error() {
        let original = pseudo_random(64, 0xABCD_1234);
        let mut comp = deflate_raw(&original, 0, 15);
        // Layout for a single stored block: [header byte][LEN:2][NLEN:2][data].
        // Corrupt a byte of NLEN so LEN != ~NLEN.
        assert!(comp.len() > 5, "stored stream unexpectedly short");
        comp[3] ^= 0xFF;
        let (ret, _out) = run_back(&[&comp], 15);
        assert_eq!(ret, Z_DATA_ERROR);
    }

    /// A back-reference whose distance exceeds the (smaller) decode window
    /// yields Z_DATA_ERROR ("invalid distance too far back"). The stream is
    /// encoded with a 32 KiB window (allowing a far match) but decoded with a
    /// 256-byte window.
    #[test]
    fn distance_too_far_yields_data_error() {
        // A 400-byte unique block followed by a copy of it: the copy matches at
        // distance 400, which exceeds the 256-byte decode window.
        let block = pseudo_random(400, 0x0BAD_F00D);
        let mut original = block.clone();
        original.extend_from_slice(&block);
        let comp = deflate_raw(&original, 6, 15);
        let (ret, _out) = run_back(&[&comp], 8); // 256-byte window
        assert_eq!(ret, Z_DATA_ERROR);
    }

    /// A failing output callback yields Z_BUF_ERROR even on an otherwise valid
    /// stream.
    #[test]
    fn failing_output_yields_buf_error() {
        let original = b"some content that will be flushed to the sink".to_vec();
        let comp = deflate_raw(&original, 6, 15);
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];
        let mut input = ChunkedInput {
            chunks: vec![&comp[..]],
            idx: 0,
        };
        let mut sink = FailingSink;
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
        assert_eq!(ret, Z_BUF_ERROR);
    }

    // -- Parameter / lifecycle tests --------------------------------------

    /// `inflate_back_init` accepts the full 8..=15 `windowBits` range and
    /// rejects everything else with Z_STREAM_ERROR.
    #[test]
    fn init_validates_window_bits() {
        for wb in 8..=15 {
            let st = inflate_back_init(wb).expect("valid windowBits");
            assert_eq!(st.wsize, 1u32 << wb);
            assert_eq!(st.wbits, wb as u32);
            assert_eq!(st.dmax, 32768);
            assert!(st.sane);
            assert_eq!(st.mode, InflateMode::Type);
            // The window is NOT allocated by init (user supplies it).
            assert!(st.window.is_empty());
        }
        for wb in [-1, 0, 7, 16, 31, 100] {
            // `.err()` (not `.unwrap_err()`) avoids requiring `InflateState:
            // Debug` for the discarded Ok variant.
            assert_eq!(inflate_back_init(wb).err(), Some(Z_STREAM_ERROR));
        }
    }

    /// A `window` shorter than `wsize` is rejected with Z_STREAM_ERROR rather
    /// than panicking.
    #[test]
    fn undersized_window_yields_stream_error() {
        let comp = deflate_raw(b"hello", 6, 15);
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 16]; // far smaller than 1 << 15
        let mut input = ChunkedInput {
            chunks: vec![&comp[..]],
            idx: 0,
        };
        let mut sink = VecSink { out: Vec::new() };
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
        assert_eq!(ret, Z_STREAM_ERROR);
    }

    /// `inflate_back_end` returns Z_OK (and drops the state's buffers).
    #[test]
    fn end_returns_ok() {
        let state = inflate_back_init(15).expect("init");
        assert_eq!(inflate_back_end(state), Z_OK);
    }

    /// The same state can be reused for a second `inflate_back` call (the
    /// per-call reset returns it to TYPE), decoding a fresh stream correctly.
    #[test]
    fn state_is_reusable() {
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];

        for payload in [&b"first payload"[..], &b"second, different payload"[..]] {
            let comp = deflate_raw(payload, 6, 15);
            let mut input = ChunkedInput {
                chunks: vec![&comp[..]],
                idx: 0,
            };
            let mut sink = VecSink { out: Vec::new() };
            let ret = inflate_back(&mut state, &mut window, &mut input, &mut sink);
            assert_eq!(ret, Z_STREAM_END);
            assert_eq!(sink.out, payload);
        }
    }

    /// The `FnMut(&[u8]) -> bool` blanket [`BackOutput`] impl works: a closure
    /// can be used directly as the output sink.
    #[test]
    fn closure_output_sink() {
        let original = b"closure sink payload".to_vec();
        let comp = deflate_raw(&original, 6, 15);
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];
        let mut input = SliceInput::new(&comp);
        let mut collected: Vec<u8> = Vec::new();
        let ret = inflate_back(&mut state, &mut window, &mut input, &mut |buf: &[u8]| {
            collected.extend_from_slice(buf);
            false
        });
        assert_eq!(ret, Z_STREAM_END);
        assert_eq!(collected, original);
    }
}
