//! Callback-driven raw-DEFLATE decoder (`infback.c`).
//!
//! This module is the safe-Rust translation of zlib's `infback.c` — the
//! `inflateBack` family (`inflateBackInit_`, `inflateBack`, `inflateBackEnd`).
//! It decompresses a **raw DEFLATE** stream (RFC 1951) using the sliding
//! [`window`](InflateState::window) **as its output buffer** and driving all
//! I/O through caller-supplied input/output callbacks. The C header notes that
//! this code is "largely copied from inflate.c"; normally an application links
//! either `inflate.o` *or* `infback.o`, not both.
//!
//! # How `inflateBack` differs from `inflate`
//!
//! * **Raw DEFLATE only.** There is no zlib/gzip header or trailer handling and
//!   no running checksum. The mode machine starts directly at
//!   [`InflateMode::Type`] (block dispatch) and only ever visits the
//!   deflate-block states `Type` / `Stored` / `Table` / `Len` / `Done` / `Bad`
//!   — a strict subset of [`crate::inflate::state`]'s full machine.
//! * **The window *is* the output.** Literals and match copies are written
//!   straight into [`InflateState::window`]; there is no separate output slice.
//!   When the window fills, it is flushed in one piece through the `write`
//!   callback and reused (the C `ROOM()` macro). This is fundamentally unlike
//!   [`InflateState::inflate`], which decodes into a caller output buffer and
//!   keeps a *separate* sliding window for history.
//! * **Pull/push I/O.** Input is pulled on demand through the `read` callback
//!   (the C `in()` / `PULL()`); whenever the window is full or decoding
//!   finishes, output is pushed through the `write` callback (the C `out()`).
//!
//! # Why this port does **not** call [`crate::inflate::fast::inflate_fast`]
//!
//! The C source keeps the `inffast.c` interface so an optimized `inflate_fast`
//! can serve both drivers. This safe-Rust port deliberately uses the per-code
//! "slow" decode instead, for three independent reasons:
//!
//! 1. **Borrow safety.** Under the crate-wide `#![forbid(unsafe_code)]`,
//!    `inflate_fast(state, .., output, .., ..)` requires `state` and `output`
//!    as *disjoint* mutable borrows. In `inflateBack` the output *is*
//!    `state.window`, so passing it as both is an impossible double mutable
//!    borrow — it cannot be expressed without `unsafe`.
//! 2. **Window model.** [`crate::inflate::fast::inflate_fast`] resolves match
//!    history through the *circular* `wnext`/`whave` window of `inflate()`,
//!    whereas `inflateBack` uses the *linear* window-as-output model. Reusing
//!    it would decode against the wrong addressing scheme.
//! 3. **Parity with the sibling driver.** [`InflateState::inflate`] itself runs
//!    the per-code path exclusively (its own docs note the fast path "produces
//!    byte-identical output" and is "not part of this file's verified
//!    dependency set"). The per-code path here is byte-identical to C zlib for
//!    every well-formed stream, satisfying the bit-exactness contract
//!    (AAP §0.6.4).
//!
//! # `no_std`
//!
//! The decoder uses only `core` (indexing, integer arithmetic) and `alloc`
//! (the owned [`Vec`] window). It works with `--no-default-features` because
//! raw DEFLATE needs neither `std` nor the `gzip` feature.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Result, ReturnCode, ZlibError};
use crate::inflate::fixed::{DISTFIX, LENFIX};
use crate::inflate::state::{InflateMode, InflateState};
use crate::inflate::tables::{CodeType, inflate_table};

/// Permutation of the 19 code-length-code lengths (`infback.c` L205-L206 /
/// `inflate.c`). The dynamic-block header transmits the code-length code
/// lengths in this order; decoding scatters them back into natural order.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Outcome of an [`inflate_back`] call.
///
/// `inflateBack` reports two things to its caller (C `inflateBack` writes the
/// second back through `strm->next_in` / `strm->avail_in`):
///
/// * [`status`](Self::status) — the typed return code. `Ok(ReturnCode::StreamEnd)`
///   on a properly terminated stream, or `Err(..)` for the failure codes the C
///   function returns: [`ZlibError::DataError`] for a malformed deflate stream
///   (mode `Bad`), [`ZlibError::BufError`] when the `read` callback could
///   supply no more input or the `write` callback failed, and
///   [`ZlibError::StreamError`] for the "can't happen" default state.
/// * [`unused_input`](Self::unused_input) — the still-unconsumed tail of the
///   most recent input chunk returned by the `read` callback (the C
///   `strm->next_in` / `avail_in` left on return). The FFI shim copies this
///   back onto the public `z_stream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InflateBackResult<'a> {
    /// The typed return code (see the type-level docs).
    pub status: Result<ReturnCode>,
    /// The unconsumed tail of the last input chunk handed back by `read`.
    pub unused_input: &'a [u8],
}

/// Port of C `inflateBackInit_` (`infback.c` L25-L64).
///
/// Initializes an [`InflateState`] for back-inflation. The C function takes a
/// caller-supplied window buffer of `2**windowBits` bytes; in this safe-Rust
/// core the window is modeled as an owned [`Vec<u8>`] of exactly
/// `1 << window_bits` bytes living inside the returned state (the FFI shim in
/// `libz-rs-sys` bridges the C raw window pointer onto this model). Allocation
/// via `Box::new` replaces the C `ZALLOC`, and the eventual free performed by
/// C `inflateBackEnd` becomes the automatic [`Drop`] of the owned window and
/// box (see [`inflate_back_end`]).
///
/// `window_bits` must be in `8..=15` (the C `windowBits < 8 || windowBits > 15`
/// check); any other value yields [`ZlibError::StreamError`]. Unlike the public
/// `inflate` overloading, `inflateBack` never negates or offsets
/// `window_bits` — it is always a plain raw-window size.
///
/// The returned state has `mode` left at its default; [`inflate_back`] resets
/// it to [`InflateMode::Type`] on entry, exactly as the C driver does.
pub fn inflate_back_init(window_bits: i32) -> Result<Box<InflateState>> {
    // C: `if (windowBits < 8 || windowBits > 15) return Z_STREAM_ERROR;`
    // (The version / stream-size checks live in the FFI shim, not here.)
    if !(8..=15).contains(&window_bits) {
        return Err(ZlibError::StreamError);
    }

    let mut state = Box::new(InflateState::default());

    // C `inflateBackInit_` field initialization (infback.c L56-L62). `wsize`
    // is the full `2**windowBits`; the window is eagerly allocated here (the C
    // caller supplies it), in contrast to `inflate`'s lazy allocation.
    let wsize = 1usize << window_bits;
    state.dmax = 32768;
    state.wbits = window_bits as u32;
    state.wsize = wsize;
    // Eagerly allocate the full `2**windowBits` window, zero-filled (the C
    // caller supplies this buffer; here it is an owned `Vec<u8>`).
    let window: Vec<u8> = vec![0u8; wsize];
    state.window = window;
    state.wnext = 0;
    state.whave = 0;
    state.sane = true;
    // Back-inflate is header-less; `flags = -1` marks "raw / no header", matching
    // the state a freshly reset raw-mode `inflate` would carry.
    state.flags = -1;

    Ok(state)
}

/// Port of C `inflateBack` (`infback.c` L191-L570).
///
/// Decompresses a raw DEFLATE stream, using the state's [`window`] as the
/// output buffer and driving I/O through the `read` and `write` callbacks.
///
/// # Callbacks
///
/// * `read: FnMut() -> &'a [u8]` — the C `in_func`. Returns the next chunk of
///   input. An **empty** slice signals "no more input"; if decoding still needs
///   a byte at that point the call fails with [`ZlibError::BufError`] (the C
///   `in()`-returns-0 path). Each returned chunk is fully consumed before
///   `read` is called again, mirroring the C contract that the caller must not
///   change the buffer until `in()` is next called.
/// * `write: FnMut(&[u8]) -> bool` — the C `out_func`. Receives a slice of the
///   window to emit and returns `true` on **failure** (matching the C
///   convention that `out()` returns non-zero on failure), which fails the call
///   with [`ZlibError::BufError`].
///
/// The lifetime `'a` ties every chunk `read` yields to the duration of this
/// call: the decoder holds at most one chunk at a time and only re-fetches once
/// the current chunk is exhausted, so this is exactly the C "buffer valid until
/// the next `in()`" discipline expressed in safe Rust.
///
/// # Returns
///
/// An [`InflateBackResult`] carrying the typed [`status`](InflateBackResult::status)
/// and the [`unused_input`](InflateBackResult::unused_input) tail (see that
/// type's docs). `state` must have been produced by [`inflate_back_init`]; the
/// function resets the block-decode bookkeeping on entry, so the same state can
/// be reused for another raw stream.
///
/// [`window`]: InflateState::window
pub fn inflate_back<'a, R, W>(
    state: &mut InflateState,
    mut read: R,
    mut write: W,
) -> InflateBackResult<'a>
where
    R: FnMut() -> &'a [u8],
    W: FnMut(&[u8]) -> bool,
{
    // ---- Reset the state (infback.c L213-L223). -------------------------
    // Back-inflate starts at block dispatch; there is no header to parse.
    state.msg = None;
    state.mode = InflateMode::Type;
    state.last = false;
    state.whave = 0;

    // ---- Streaming registers held in locals (the C LOAD/RESTORE idiom). --
    // `cur`/`next`/`have` track the *current input chunk*: `cur` is the slice
    // last handed back by `read`, `next` the index of the next unread byte, and
    // `have` the count still available (`cur.len() - next`). Input begins empty
    // so the first `pull!` fetches via `read` (the C `next_in == Z_NULL` start).
    let mut cur: &'a [u8] = &[];
    let mut next: usize = 0;
    let mut have: usize = 0;
    // `put`/`left` track the window-as-output cursor: `put` is the next write
    // index into `state.window`, `left` the remaining window space. They move
    // in lockstep (put up, left down) and reset together on a `room!` flush.
    let mut put: usize = 0;
    let mut left: usize = state.wsize;
    // Bit accumulator (`hold`) and its valid-bit count (`bits`).
    let mut hold: u64 = 0;
    let mut bits: u32 = 0;
    // Pending return code. Left deliberately uninitialized (as in the C `int
    // ret;`): the decode loop is exited *only* via `break 'inf`, and every such
    // break assigns `ret` first, so it is definitely initialized before the
    // epilogue reads it.
    let mut ret: Result<ReturnCode>;

    // ---- Bit-accumulator + I/O macros (ports of infback.c L91-L162). -----
    //
    // Defined as function-local `macro_rules!` so they can mutate the loop
    // locals *and* `break` straight to the shared epilogue when input/output is
    // exhausted. Loop labels are hygienic inside `macro_rules!`, so the
    // enclosing `'inf` label is threaded in as a `:lifetime` metavariable;
    // every call site passes `'inf`. Free identifiers (`hold`, `bits`, `cur`,
    // `next`, `have`, `put`, `left`, `ret`, `state`, `read`, `write`) resolve to
    // the bindings above, which are all in scope at the definition site.

    // PULL(): ensure some input is available, refetching via `read` when the
    // current chunk is spent; an empty refetch is a Z_BUF_ERROR (infback.c
    // L99-L109).
    macro_rules! pull {
        ($lab:lifetime) => {{
            if have == 0 {
                cur = read();
                next = 0;
                have = cur.len();
                if have == 0 {
                    ret = Err(ZlibError::BufError);
                    break $lab;
                }
            }
        }};
    }
    // PULLBYTE(): pull one byte into the accumulator (infback.c L113-L119).
    macro_rules! pull_byte {
        ($lab:lifetime) => {{
            pull!($lab);
            let b = cur[next];
            next += 1;
            have -= 1;
            hold += (b as u64) << bits;
            bits += 8;
        }};
    }
    // NEEDBITS(n): ensure at least `n` bits are buffered (infback.c L124-L128).
    macro_rules! need_bits {
        ($lab:lifetime, $n:expr) => {{
            while bits < ($n as u32) {
                pull_byte!($lab);
            }
        }};
    }
    // BITS(n): the low `n` bits of the accumulator (infback.c L131-L132).
    macro_rules! bits_val {
        ($n:expr) => {
            ((hold & ((1u64 << ($n as u32)) - 1)) as u32)
        };
    }
    // DROPBITS(n): remove `n` bits from the bottom (infback.c L135-L139).
    macro_rules! drop_bits {
        ($n:expr) => {{
            let n = $n as u32;
            hold >>= n;
            bits -= n;
        }};
    }
    // INITBITS(): clear the accumulator (infback.c L91-L95).
    macro_rules! init_bits {
        () => {{
            hold = 0;
            bits = 0;
        }};
    }
    // BYTEBITS(): discard bits up to the next byte boundary (infback.c L142-L146).
    macro_rules! byte_bits {
        () => {{
            let r = bits & 7;
            hold >>= r;
            bits -= r;
        }};
    }
    // ROOM(): flush the full window through `write` when it is full, then reuse
    // it from the start (infback.c L151-L162). `write` returning `true` (the C
    // non-zero failure) is a Z_BUF_ERROR.
    macro_rules! room {
        ($lab:lifetime) => {{
            if left == 0 {
                state.whave = state.wsize;
                let failed = write(&state.window[..state.wsize]);
                put = 0;
                left = state.wsize;
                if failed {
                    ret = Err(ZlibError::BufError);
                    break $lab;
                }
            }
        }};
    }

    // ---- The decode driver (infback.c L226-L558). -----------------------
    // The C `for (;;) switch (state->mode)` becomes a labeled `loop` with an
    // exhaustive `match`. Each C `break;` (re-dispatch) is a natural fall-off
    // or `continue 'inf`; each C `goto inf_leave;` becomes `break 'inf` to the
    // shared epilogue after the loop.
    'inf: loop {
        match state.mode {
            // ---------------------------------------------------------------
            // Block type dispatch (infback.c L228-L260).
            // ---------------------------------------------------------------
            InflateMode::Type => {
                if state.last {
                    // Final block already seen: align and finish.
                    byte_bits!();
                    state.mode = InflateMode::Done;
                    continue 'inf;
                }
                need_bits!('inf, 3);
                state.last = bits_val!(1) != 0;
                drop_bits!(1);
                match bits_val!(2) {
                    0 => {
                        // stored (uncompressed) block
                        state.mode = InflateMode::Stored;
                    }
                    1 => {
                        // fixed Huffman tables (port of C `inflate_fixed`):
                        // LENFIX occupies codes[0..512], DISTFIX codes[512..544].
                        state.codes[0..512].copy_from_slice(&LENFIX);
                        state.codes[512..544].copy_from_slice(&DISTFIX);
                        state.lencode = 0;
                        state.lenbits = 9;
                        state.distcode = 512;
                        state.distbits = 5;
                        state.mode = InflateMode::Len;
                    }
                    2 => {
                        // dynamic Huffman tables
                        state.mode = InflateMode::Table;
                    }
                    _ => {
                        // BITS(2) == 3 — reserved/invalid block type.
                        state.msg = Some("invalid block type");
                        state.mode = InflateMode::Bad;
                    }
                }
                drop_bits!(2);
            }

            // ---------------------------------------------------------------
            // Stored (uncompressed) block (infback.c L262-L292).
            // ---------------------------------------------------------------
            InflateMode::Stored => {
                byte_bits!(); // go to a byte boundary
                need_bits!('inf, 32);
                // LEN and its one's-complement NLEN must agree.
                if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
                    state.msg = Some("invalid stored block lengths");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.length = (hold & 0xffff) as u32;
                init_bits!();

                // Copy `length` literal bytes straight from input to the window.
                while state.length != 0 {
                    let mut copy = state.length as usize;
                    pull!('inf); // assure input
                    room!('inf); // assure output (window) space
                    if copy > have {
                        copy = have;
                    }
                    if copy > left {
                        copy = left;
                    }
                    state.window[put..put + copy].copy_from_slice(&cur[next..next + copy]);
                    have -= copy;
                    next += copy;
                    left -= copy;
                    put += copy;
                    state.length -= copy as u32;
                }
                state.mode = InflateMode::Type;
            }

            // ---------------------------------------------------------------
            // Dynamic Huffman table construction (infback.c L294-L419).
            // ---------------------------------------------------------------
            InflateMode::Table => {
                need_bits!('inf, 14);
                state.nlen = (bits_val!(5) + 257) as usize;
                drop_bits!(5);
                state.ndist = (bits_val!(5) + 1) as usize;
                drop_bits!(5);
                state.ncode = (bits_val!(4) + 4) as usize;
                drop_bits!(4);
                if state.nlen > 286 || state.ndist > 30 {
                    state.msg = Some("too many length or distance symbols");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // Read the code-length-code lengths in their permuted order,
                // zero-filling the remaining slots up to 19.
                state.have = 0;
                while state.have < state.ncode {
                    need_bits!('inf, 3);
                    state.lens[ORDER[state.have] as usize] = bits_val!(3) as u16;
                    state.have += 1;
                    drop_bits!(3);
                }
                while state.have < 19 {
                    state.lens[ORDER[state.have] as usize] = 0;
                    state.have += 1;
                }

                // Build the code-length decode table (lenbits = 7). Disjoint
                // field borrows of `state` are accepted by the borrow checker.
                state.next = 0;
                state.lencode = 0;
                state.lenbits = 7;
                match inflate_table(
                    CodeType::Codes,
                    &state.lens,
                    19,
                    &mut state.codes,
                    &mut state.lenbits,
                    &mut state.work,
                ) {
                    Ok(used) => state.next += used,
                    Err(_) => {
                        state.msg = Some("invalid code lengths set");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                }

                // Decode the `nlen + ndist` literal/length and distance code
                // lengths, expanding the 16/17/18 repeat codes.
                state.have = 0;
                while state.have < state.nlen + state.ndist {
                    let here = loop {
                        let h = state.codes[state.lencode + bits_val!(state.lenbits) as usize];
                        if (h.bits as u32) <= bits {
                            break h;
                        }
                        pull_byte!('inf);
                    };
                    if here.val < 16 {
                        drop_bits!(here.bits as u32);
                        state.lens[state.have] = here.val;
                        state.have += 1;
                    } else {
                        let len_fill: u16;
                        let copy_count: usize;
                        if here.val == 16 {
                            need_bits!('inf, here.bits as u32 + 2);
                            drop_bits!(here.bits as u32);
                            if state.have == 0 {
                                state.msg = Some("invalid bit length repeat");
                                state.mode = InflateMode::Bad;
                                break;
                            }
                            len_fill = state.lens[state.have - 1];
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
                        if state.have + copy_count > state.nlen + state.ndist {
                            state.msg = Some("invalid bit length repeat");
                            state.mode = InflateMode::Bad;
                            break;
                        }
                        let mut c = copy_count;
                        while c != 0 {
                            state.lens[state.have] = len_fill;
                            state.have += 1;
                            c -= 1;
                        }
                    }
                }

                // Propagate a BAD set inside the decode loop (C: `if (mode ==
                // BAD) break;`).
                if state.mode == InflateMode::Bad {
                    continue 'inf;
                }

                // An end-of-block code (symbol 256) must be present.
                if state.lens[256] == 0 {
                    state.msg = Some("invalid code -- missing end-of-block");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // Build the literal/length table (lenbits = 9). The fixed
                // lenbits/distbits values (9 and 6) must not change without
                // revisiting the ENOUGH constants — see inftrees.h.
                state.next = 0;
                state.lencode = 0;
                state.lenbits = 9;
                let nlen = state.nlen;
                match inflate_table(
                    CodeType::Lens,
                    &state.lens,
                    nlen,
                    &mut state.codes,
                    &mut state.lenbits,
                    &mut state.work,
                ) {
                    Ok(used) => state.next += used,
                    Err(_) => {
                        state.msg = Some("invalid literal/lengths set");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                }

                // Build the distance table (distbits = 6) after the length
                // table in the shared `codes` arena.
                state.distcode = state.next;
                state.distbits = 6;
                let dc = state.distcode;
                let ndist = state.ndist;
                match inflate_table(
                    CodeType::Dists,
                    &state.lens[nlen..],
                    ndist,
                    &mut state.codes[dc..],
                    &mut state.distbits,
                    &mut state.work,
                ) {
                    Ok(used) => state.next += used,
                    Err(_) => {
                        state.msg = Some("invalid distances set");
                        state.mode = InflateMode::Bad;
                        continue 'inf;
                    }
                }

                state.mode = InflateMode::Len;
            }

            // ---------------------------------------------------------------
            // Length / literal / distance / match decode (infback.c L422-L543).
            //
            // The whole length+distance+match sequence runs in this one arm,
            // matching the C `case LEN`. (The C fast path is intentionally not
            // used — see the module docs.) `pull_byte!`/`need_bits!` refetch
            // input on demand, so unlike `inflate()` there is no need to split
            // this into resumable sub-states.
            // ---------------------------------------------------------------
            InflateMode::Len => {
                // Decode a length / literal / end-of-block code.
                let mut here = loop {
                    let h = state.codes[state.lencode + bits_val!(state.lenbits) as usize];
                    if (h.bits as u32) <= bits {
                        break h;
                    }
                    pull_byte!('inf);
                };
                // Follow a second-level (sub-)table link, if this is one.
                if here.op != 0 && (here.op & 0xf0) == 0 {
                    let last = here;
                    here = loop {
                        let idx = state.lencode
                            + last.val as usize
                            + (bits_val!(last.bits as u32 + last.op as u32) >> last.bits) as usize;
                        let h = state.codes[idx];
                        if (last.bits as u32 + h.bits as u32) <= bits {
                            break h;
                        }
                        pull_byte!('inf);
                    };
                    drop_bits!(last.bits as u32);
                }
                drop_bits!(here.bits as u32);
                state.length = here.val as u32;

                // Literal byte -> write directly into the window.
                if here.op == 0 {
                    room!('inf);
                    state.window[put] = state.length as u8;
                    put += 1;
                    left -= 1;
                    // mode stays Len
                    continue 'inf;
                }
                // End-of-block code -> back to block dispatch.
                if (here.op & 32) != 0 {
                    state.mode = InflateMode::Type;
                    continue 'inf;
                }
                // Invalid literal/length code.
                if (here.op & 64) != 0 {
                    state.msg = Some("invalid literal/length code");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // Length code: pull the base length's extra bits, if any.
                state.extra = (here.op & 15) as u32;
                if state.extra != 0 {
                    need_bits!('inf, state.extra);
                    state.length += bits_val!(state.extra);
                    drop_bits!(state.extra);
                }

                // Decode the distance code.
                let mut here = loop {
                    let h = state.codes[state.distcode + bits_val!(state.distbits) as usize];
                    if (h.bits as u32) <= bits {
                        break h;
                    }
                    pull_byte!('inf);
                };
                if (here.op & 0xf0) == 0 {
                    let last = here;
                    here = loop {
                        let idx = state.distcode
                            + last.val as usize
                            + (bits_val!(last.bits as u32 + last.op as u32) >> last.bits) as usize;
                        let h = state.codes[idx];
                        if (last.bits as u32 + h.bits as u32) <= bits {
                            break h;
                        }
                        pull_byte!('inf);
                    };
                    drop_bits!(last.bits as u32);
                }
                drop_bits!(here.bits as u32);
                if (here.op & 64) != 0 {
                    state.msg = Some("invalid distance code");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }
                state.offset = here.val as u32;

                // Distance extra bits, if any.
                state.extra = (here.op & 15) as u32;
                if state.extra != 0 {
                    need_bits!('inf, state.extra);
                    state.offset += bits_val!(state.extra);
                    drop_bits!(state.extra);
                }

                // Guard: the distance must not reach before available history.
                // Before the window has been filled once (`whave < wsize`) only
                // the `wsize - left` bytes written so far are valid history.
                let offset = state.offset as usize;
                let limit = state.wsize - if state.whave < state.wsize { left } else { 0 };
                if offset > limit {
                    state.msg = Some("invalid distance too far back");
                    state.mode = InflateMode::Bad;
                    continue 'inf;
                }

                // Copy the match from the window into the window (window IS the
                // output). `from_start` is derived from the pre-update `copy` so
                // the wrap-around source index is correct; the inner copy is
                // byte-by-byte to honor overlapping (run-length) matches where
                // the source trails the destination by `offset`.
                loop {
                    room!('inf);
                    let mut copy = state.wsize - offset;
                    let from_start = if copy < left {
                        // Source wraps past the window end: it lives at
                        // `put + copy`, and only `left - copy` bytes are
                        // available before the boundary this round.
                        let f = put + copy;
                        copy = left - copy;
                        f
                    } else {
                        // Source is directly behind `put` within the window.
                        copy = left;
                        put - offset
                    };
                    if copy > state.length as usize {
                        copy = state.length as usize;
                    }
                    state.length -= copy as u32;
                    left -= copy;
                    // Byte-by-byte copy (source and destination may overlap when
                    // `offset < copy`, i.e. RLE-style runs): reading
                    // `window[from_start + i]` for `i >= offset` returns a byte
                    // written earlier in this same loop, exactly as C's
                    // `*put++ = *from++;` lockstep advance does.
                    for i in 0..copy {
                        state.window[put + i] = state.window[from_start + i];
                    }
                    put += copy;
                    if state.length == 0 {
                        break;
                    }
                }
                // mode stays Len: re-dispatch to decode the next code.
            }

            // ---------------------------------------------------------------
            // Terminal states (infback.c L545-L557).
            // ---------------------------------------------------------------
            InflateMode::Done => {
                // The stream terminated properly.
                ret = Ok(ReturnCode::StreamEnd);
                break 'inf;
            }
            InflateMode::Bad => {
                ret = Err(ZlibError::DataError);
                break 'inf;
            }
            _ => {
                // Unreachable for raw back-inflation (no header/trailer states
                // are ever entered). Mirrors the C `default` "can't happen"
                // branch returning Z_STREAM_ERROR.
                ret = Err(ZlibError::StreamError);
                break 'inf;
            }
        }
    }

    // ---- inf_leave epilogue (infback.c L560-L569). ----------------------
    // Write whatever output is still buffered in the window. If this final
    // flush fails on an otherwise successfully terminated stream, downgrade the
    // result to Z_BUF_ERROR, exactly as the C code does.
    if left < state.wsize {
        let n = state.wsize - left;
        let failed = write(&state.window[..n]);
        if failed && ret == Ok(ReturnCode::StreamEnd) {
            ret = Err(ZlibError::BufError);
        }
    }

    // Hand back the unconsumed tail of the current input chunk (the C function
    // writes this through strm->next_in / strm->avail_in on return).
    InflateBackResult {
        status: ret,
        unused_input: &cur[next..],
    }
}

/// Port of C `inflateBackEnd` (`infback.c` L572-L579).
///
/// The C function frees the heap-allocated decompressor state via `ZFREE`. In
/// this safe-Rust core that deallocation is handled by ownership: taking the
/// boxed [`InflateState`] by value drops it (and its owned [`window`] `Vec`) at
/// the end of this function, with no explicit free required (RAII replaces
/// `inflateEnd`/`inflateBackEnd`). Returns `Ok(ReturnCode::Ok)` for API parity
/// with the C `Z_OK`.
///
/// [`window`]: InflateState::window
pub fn inflate_back_end(state: Box<InflateState>) -> Result<ReturnCode> {
    // Dropping `state` here frees the window `Vec` and the boxed state.
    drop(state);
    Ok(ReturnCode::Ok)
}

#[cfg(test)]
mod tests {
    use super::{inflate_back, inflate_back_end, inflate_back_init};
    use crate::error::{Result, ReturnCode, ZlibError};
    use alloc::vec::Vec;

    // ---- Pre-computed raw-DEFLATE (RFC 1951) test vectors. --------------
    // Produced offline with the reference C zlib (Python `zlib`) using a raw
    // deflate stream (negative windowBits). Each is paired with the exact
    // plaintext it must decode back to, giving a self-contained interop oracle
    // that needs no dev-dependency.

    /// `b"Hello, World! This is zlib inflateBack in safe Rust.\n"`, level 6.
    const RAW_HELLO: &[u8] = &[
        0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x08, 0xcf, 0x2f, 0xca, 0x49, 0x51, 0x54, 0x08,
        0xc9, 0xc8, 0x2c, 0x56, 0x00, 0xa2, 0xaa, 0x9c, 0xcc, 0x24, 0x85, 0xcc, 0xbc, 0xb4, 0x9c,
        0xc4, 0x92, 0x54, 0xa7, 0xc4, 0xe4, 0x6c, 0x20, 0x5b, 0xa1, 0x38, 0x31, 0x2d, 0x55, 0x21,
        0xa8, 0xb4, 0xb8, 0x44, 0x8f, 0x0b, 0x00,
    ];
    const RAW_HELLO_PLAIN: &[u8] = b"Hello, World! This is zlib inflateBack in safe Rust.\n";

    /// `b"abracadabra " * 8`, level 9 (length/distance matches, fixed Huffman).
    const RAW_REP: &[u8] = &[
        0x4b, 0x4c, 0x2a, 0x4a, 0x4c, 0x4e, 0x4c, 0x49, 0x04, 0x52, 0x0a, 0x89, 0x34, 0x60, 0x03,
        0x00,
    ];

    /// `b"STOREDblock-no-compression-0123456789"`, level 0 (a stored block).
    const RAW_STORED: &[u8] = &[
        0x01, 0x25, 0x00, 0xda, 0xff, 0x53, 0x54, 0x4f, 0x52, 0x45, 0x44, 0x62, 0x6c, 0x6f, 0x63,
        0x6b, 0x2d, 0x6e, 0x6f, 0x2d, 0x63, 0x6f, 0x6d, 0x70, 0x72, 0x65, 0x73, 0x73, 0x69, 0x6f,
        0x6e, 0x2d, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    ];
    const RAW_STORED_PLAIN: &[u8] = b"STOREDblock-no-compression-0123456789";

    /// A pangram unit repeated 3x, level 6 (forces a dynamic Huffman block).
    const RAW_DYN: &[u8] = &[
        0xed, 0x8e, 0x49, 0x12, 0xc2, 0x20, 0x14, 0x44, 0xaf, 0xd2, 0xee, 0xad, 0x94, 0xf3, 0x70,
        0x03, 0x97, 0x56, 0x99, 0x0b, 0x80, 0x01, 0x82, 0x22, 0x3f, 0x21, 0x0c, 0x09, 0xa7, 0x97,
        0x58, 0x5e, 0xc1, 0x9d, 0xeb, 0xd7, 0xfd, 0xba, 0xeb, 0x56, 0xa0, 0x0f, 0xfa, 0xfe, 0x04,
        0x77, 0x94, 0x2c, 0x24, 0x8d, 0x78, 0x84, 0x57, 0x37, 0x80, 0xa2, 0x70, 0xf0, 0x05, 0x1b,
        0x96, 0x27, 0x34, 0xa4, 0x2a, 0x5c, 0x59, 0xc9, 0xbd, 0x26, 0xf0, 0x12, 0x4a, 0xda, 0xb7,
        0x90, 0x3a, 0x8a, 0x82, 0xb2, 0xb0, 0x30, 0xba, 0x0f, 0xe4, 0x4a, 0x57, 0x0d, 0x15, 0x2e,
        0x94, 0x10, 0xc5, 0xa8, 0xad, 0x32, 0xd3, 0x57, 0xdf, 0x30, 0xe9, 0x91, 0x05, 0x77, 0x6c,
        0xf8, 0x0c, 0x2c, 0x70, 0xeb, 0x5a, 0x6d, 0x47, 0x90, 0x04, 0x37, 0xb3, 0xb8, 0x0f, 0xcc,
        0xf9, 0xbc, 0x2c, 0xb4, 0x51, 0x62, 0x9e, 0x89, 0x94, 0x2a, 0xac, 0xd6, 0x9b, 0xed, 0x6e,
        0x7f, 0x38, 0x9e, 0xce, 0xf5, 0xff, 0xea, 0x0f, 0xae, 0xbe, 0x01,
    ];

    /// 13800 bytes of plaintext (`BIG_UNIT` repeated `BIG_TIMES` times),
    /// compressed as raw DEFLATE level 9 with a 512-byte window (`wbits = -9`)
    /// by the reference C zlib. Decoded with `window_bits = 9` it overflows the
    /// 512-byte window ~27 times, exercising the `ROOM()`/`out()` flush path.
    /// Verified to round-trip against reference zlib at the same window size.
    const RAW_BIG_W9: &[u8] = &[
        0xed, 0xc8, 0xc1, 0x0d, 0x80, 0x20, 0x00, 0x04, 0xb0, 0x89, 0x48, 0x10, 0x14, 0x74, 0x9c,
        0x53, 0x27, 0x70, 0xff, 0x87, 0x4e, 0xe1, 0xab, 0xaf, 0x26, 0xcd, 0xf9, 0xe4, 0xca, 0x9d,
        0x8f, 0x52, 0x97, 0xd6, 0xd7, 0x6d, 0xcc, 0xfd, 0x28, 0xd1, 0x5a, 0x6b, 0xad, 0xb5, 0xd6,
        0x5a, 0x6b, 0xad, 0xb5, 0xd6, 0x5a, 0x6b, 0xad, 0xb5, 0xd6, 0x5a, 0x6b, 0xad, 0xb5, 0xd6,
        0x5a, 0x6b, 0xad, 0xb5, 0xd6, 0x5a, 0x6b, 0xad, 0xb5, 0xd6, 0x5a, 0x6b, 0xfd, 0x4f, 0xbf,
    ];
    const BIG_UNIT: &[u8] = b"abracadabra-0123456789-";
    const BIG_TIMES: usize = 600;

    /// Reconstruct the `RAW_BIG_W9` plaintext: `BIG_UNIT` repeated `BIG_TIMES`
    /// times (13800 bytes total).
    fn big_expected() -> Vec<u8> {
        repeat(BIG_UNIT, BIG_TIMES)
    }

    fn repeat(unit: &[u8], times: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..times {
            v.extend_from_slice(unit);
        }
        v
    }

    /// Drive `inflate_back` over `comp`, feeding input in `chunk`-byte pieces
    /// (the `read` callback) and accumulating all window flushes (the `write`
    /// callback). `write_fail` forces every `write` to report failure. Returns
    /// `(status, decoded_output, unused_input_len)`.
    fn run(
        comp: &[u8],
        window_bits: i32,
        chunk: usize,
        write_fail: bool,
    ) -> (Result<ReturnCode>, Vec<u8>, usize) {
        let mut state = inflate_back_init(window_bits).expect("init must succeed");
        let mut out: Vec<u8> = Vec::new();
        let mut pos: usize = 0;
        let comp_len = comp.len();
        let step = chunk.max(1);

        let read = || -> &[u8] {
            if pos >= comp_len {
                return &[];
            }
            let end = core::cmp::min(pos + step, comp_len);
            let s = &comp[pos..end];
            pos = end;
            s
        };
        let write = |buf: &[u8]| -> bool {
            if write_fail {
                return true;
            }
            out.extend_from_slice(buf);
            false
        };

        let res = inflate_back(&mut state, read, write);
        let status = res.status;
        let unused_len = res.unused_input.len();
        (status, out, unused_len)
    }

    #[test]
    fn init_validates_window_bits() {
        // Out of range -> StreamError (the C `windowBits < 8 || > 15` check).
        assert!(matches!(inflate_back_init(7), Err(ZlibError::StreamError)));
        assert!(matches!(inflate_back_init(16), Err(ZlibError::StreamError)));
        assert!(matches!(
            inflate_back_init(-15),
            Err(ZlibError::StreamError)
        ));
        assert!(matches!(inflate_back_init(0), Err(ZlibError::StreamError)));
        // In range -> Ok, with the window eagerly sized to 1 << window_bits.
        for wb in 8..=15 {
            let st = inflate_back_init(wb).expect("valid window_bits");
            assert_eq!(st.wsize, 1usize << wb);
            assert_eq!(st.window.len(), 1usize << wb);
            assert_eq!(st.dmax, 32768);
            assert!(st.sane);
        }
    }

    #[test]
    fn init_then_end_is_ok() {
        let state = inflate_back_init(15).expect("init");
        // RAII end (no explicit free) returns Z_OK for parity.
        assert_eq!(inflate_back_end(state), Ok(ReturnCode::Ok));
    }

    #[test]
    fn roundtrip_fixed_block_single_chunk() {
        let (status, out, unused) = run(RAW_HELLO, 15, RAW_HELLO.len(), false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, RAW_HELLO_PLAIN);
        assert_eq!(unused, 0);
    }

    #[test]
    fn roundtrip_stored_block() {
        let (status, out, unused) = run(RAW_STORED, 15, RAW_STORED.len(), false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, RAW_STORED_PLAIN);
        assert_eq!(unused, 0);
    }

    #[test]
    fn roundtrip_repetitive_matches() {
        let (status, out, _) = run(RAW_REP, 15, RAW_REP.len(), false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, repeat(b"abracadabra ", 8));
    }

    #[test]
    fn roundtrip_dynamic_block() {
        let unit: &[u8] = b"The quick brown fox jumps over the lazy dog. \
Pack my box with five dozen liquor jugs. How vexingly quick daft zebras jump! \
Sphinx of black quartz, judge my vow. 0123456789";
        let (status, out, _) = run(RAW_DYN, 15, RAW_DYN.len(), false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, repeat(unit, 3));
    }

    #[test]
    fn roundtrip_input_one_byte_at_a_time() {
        // chunk == 1 forces a fresh `read` (PULL refetch) for every input byte,
        // exercising the input callback / cursor logic heavily.
        let unit: &[u8] = b"The quick brown fox jumps over the lazy dog. \
Pack my box with five dozen liquor jugs. How vexingly quick daft zebras jump! \
Sphinx of black quartz, judge my vow. 0123456789";
        let (status, out, _) = run(RAW_DYN, 15, 1, false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, repeat(unit, 3));
    }

    #[test]
    fn window_flush_room_path() {
        // 13 KiB of output through a 512-byte window forces many ROOM flushes;
        // the accumulated output must still equal the original plaintext.
        let expected = big_expected();
        let (status, out, _) = run(RAW_BIG_W9, 9, 64, false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out.len(), expected.len());
        assert_eq!(out, expected);
    }

    #[test]
    fn window_flush_room_path_chunked_input() {
        // Same large stream, but also feeding input in tiny pieces.
        let expected = big_expected();
        let (status, out, _) = run(RAW_BIG_W9, 9, 1, false);
        assert_eq!(status, Ok(ReturnCode::StreamEnd));
        assert_eq!(out, expected);
    }

    #[test]
    fn read_returning_empty_yields_buf_error() {
        // No input at all: the first PULL gets an empty slice -> Z_BUF_ERROR.
        let (status, out, _) = run(&[], 15, 16, false);
        assert_eq!(status, Err(ZlibError::BufError));
        assert!(out.is_empty());
    }

    #[test]
    fn truncated_stream_yields_buf_error() {
        // A valid prefix that needs more input: read runs dry mid-stream.
        let truncated = &RAW_HELLO[..6];
        let (status, _out, _) = run(truncated, 15, truncated.len(), false);
        assert_eq!(status, Err(ZlibError::BufError));
    }

    #[test]
    fn write_failure_yields_buf_error() {
        // The stream decodes fully, but the (epilogue) window flush fails:
        // a successful StreamEnd is downgraded to Z_BUF_ERROR.
        let (status, _out, _) = run(RAW_HELLO, 15, RAW_HELLO.len(), true);
        assert_eq!(status, Err(ZlibError::BufError));
    }

    #[test]
    fn write_failure_midstream_yields_buf_error() {
        // The big stream fills the window before completing, so the failing
        // write is hit on the first mid-stream ROOM flush, not just the epilogue.
        let (status, _out, _) = run(RAW_BIG_W9, 9, 64, true);
        assert_eq!(status, Err(ZlibError::BufError));
    }

    #[test]
    fn malformed_invalid_block_type_is_data_error() {
        // A single byte: last-block flag set, block type == 3 (reserved).
        // Must report a data error and never panic.
        let (status, _out, _) = run(&[0x07], 15, 1, false);
        assert_eq!(status, Err(ZlibError::DataError));
    }

    #[test]
    fn malformed_stored_lengths_is_data_error() {
        // Corrupt the stored block's NLEN so it no longer complements LEN.
        let mut bad: Vec<u8> = Vec::new();
        bad.extend_from_slice(RAW_STORED);
        bad[3] ^= 0xff; // break the LEN/NLEN one's-complement invariant
        let (status, _out, _) = run(&bad, 15, bad.len(), false);
        assert_eq!(status, Err(ZlibError::DataError));
    }

    #[test]
    fn malformed_random_bytes_never_panics() {
        // A spread of arbitrary inputs must always yield a typed result, never
        // a panic from out-of-bounds indexing (bounds-checked, zero `unsafe`).
        for seed in 0u16..512 {
            let mut data: Vec<u8> = Vec::new();
            let mut x = seed.wrapping_mul(2654).wrapping_add(17);
            for _ in 0..24 {
                x = x.wrapping_mul(40503).wrapping_add(1);
                data.push((x >> 8) as u8);
            }
            let (status, _out, _) = run(&data, 15, data.len(), false);
            // Either it happened to be a valid stream (StreamEnd) or it failed
            // with one of the defined error codes — but it returned a value.
            match status {
                Ok(ReturnCode::StreamEnd)
                | Err(ZlibError::DataError)
                | Err(ZlibError::BufError)
                | Err(ZlibError::StreamError) => {}
                other => panic!("unexpected status for fuzz input: {other:?}"),
            }
        }
    }

    #[test]
    fn unused_input_tail_is_returned() {
        // Trailing bytes after a complete stream are handed back unconsumed,
        // exactly as C writes them to strm->next_in / avail_in.
        let mut combined: Vec<u8> = Vec::new();
        combined.extend_from_slice(RAW_STORED);
        combined.extend_from_slice(b"EXTRA");

        let mut state = inflate_back_init(15).expect("init");
        let mut out: Vec<u8> = Vec::new();
        let mut pos = 0usize;
        let clen = combined.len();
        let read = || -> &[u8] {
            if pos >= clen {
                return &[];
            }
            let s = &combined[pos..clen];
            pos = clen;
            s
        };
        let write = |buf: &[u8]| -> bool {
            out.extend_from_slice(buf);
            false
        };
        let res = inflate_back(&mut state, read, write);
        assert_eq!(res.status, Ok(ReturnCode::StreamEnd));
        assert_eq!(res.unused_input, b"EXTRA");
        assert_eq!(out, RAW_STORED_PLAIN);
    }
}
