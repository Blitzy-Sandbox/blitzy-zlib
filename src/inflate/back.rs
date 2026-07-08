//! Raw-callback back-inflate (`inflateBack` / `inflateBackInit` /
//! `inflateBackEnd`) — safe Rust port of `infback.c`. Uses caller callbacks for
//! I/O and a caller/owned window; raw DEFLATE only.
//!
//! Unlike the main `inflate()` driver (which pushes decoded bytes into a
//! caller-supplied `next_out` buffer), [`inflate_back`] pulls its input and
//! pushes its output through **caller-supplied callbacks** ([`InFunc`] /
//! [`OutFunc`]), decoding a single *raw* DEFLATE stream (RFC 1951 — no zlib or
//! gzip wrapper) using a single sliding window that doubles as the output
//! buffer. This is the high-throughput entry point used by consumers such as
//! `gzip -d`'s core loop.
//!
//! The decoded output is **byte-identical** to reference zlib for the same
//! input (AAP §0.6.4, §0.7.1): the DEFLATE bit reader, the block dispatch, the
//! dynamic Huffman table construction (shared with the main driver via
//! [`crate::inflate::tables::inflate_table`]), the fixed tables
//! ([`crate::inflate::fixed`]), and — crucially — the overlapping LZ77 window
//! copy are reproduced exactly.
//!
//! # Unsafe policy
//!
//! This module contains **zero** `unsafe` (AAP §0.6.2). All window and input
//! access uses bounds-checked slice indexing. The raw C function-pointer
//! callbacks (`in_func` / `out_func` in `zlib.h`) are *not* modelled here;
//! wrapping those into the safe [`InFunc`] / [`OutFunc`] traits is the job of
//! the FFI boundary (`src/ffi/inflate.rs`).
//!
//! # Relationship to `inflate_fast`
//!
//! Reference `infback.c` calls the shared `inflate_fast` hot loop when at least
//! six input and 258 output bytes are available. That routine
//! ([`crate::inflate::fast::inflate_fast`]) is written for the main driver,
//! where the output buffer and the history window are **distinct** allocations.
//! In back-inflate the window *is* the output buffer, so feeding it as both the
//! `&mut` output and the `&` history would require aliasing a single `Vec<u8>`
//! mutably and immutably at once — impossible in safe Rust, and a per-call
//! window clone would cost more than it saves. This port therefore always uses
//! the equivalent single-symbol decode path (a faithful port of `infback.c`
//! L446-L543). The emitted bytes are identical either way; the fast path is a
//! pure throughput optimization, not a correctness requirement.
//!
//! # `no_std`
//!
//! The implementation is `no_std` + `alloc`: it uses [`alloc::boxed::Box`] and
//! [`alloc::vec::Vec`] and never references `std`. The I/O callbacks are
//! expressed as traits so `no_std` callers can supply their own.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::constants::MAX_WBITS;
use crate::error::ReturnCode;
use crate::inflate::fixed::{DISTFIX, LENFIX};
use crate::inflate::state::{InflateMode, InflateState, TableSource};
use crate::inflate::tables::{Code, CodeType, inflate_table};
use crate::stream::{AllocBuffer, AllocHook};

/// Permutation of the 19 code-length code lengths, as read from a dynamic
/// block header. Transcribed verbatim from `infback.c` L205-L206 (identical to
/// the `order[]` used by the main `inflate()` driver).
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Minimum permitted `windowBits` for `inflateBack` (raw DEFLATE only).
///
/// Reference `infback.c` L34 rejects anything below `8`.
const MIN_WBITS: i32 = 8;

/// Returns a mask selecting the low `n` bits (`(1 << n) - 1`), i.e. the C
/// `BITS(n)` mask.
///
/// `n` must be strictly less than 32; every caller in this module passes a
/// value derived from a DEFLATE code length (`<= 15`) or a small literal
/// header width, so the shift never overflows.
#[inline]
const fn low_mask(n: u32) -> u32 {
    (1u32 << n) - 1
}

/// Provides input bytes to [`inflate_back`], replacing the C `in_func`
/// callback.
///
/// The C prototype is
/// `typedef unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *)`:
/// it hands back a pointer to a run of input bytes and the count available,
/// returning `0` to signal end-of-input (or an input error).
///
/// The safe Rust equivalent returns the next chunk of input as a slice. An
/// **empty** slice signals end-of-input (or an error) exactly as a `0` count
/// does in C, in which case [`inflate_back`] stops and returns
/// [`ReturnCode::BufError`]. A non-empty slice is fully consumed before
/// [`next_input`](InFunc::next_input) is called again, mirroring the C contract
/// that "the application must not change the provided input until `in()` is
/// called again".
pub trait InFunc {
    /// Returns the next chunk of input bytes, or an empty slice at end-of-input.
    fn next_input(&mut self) -> &[u8];

    /// Reports, once at the end of [`inflate_back`], how many bytes of the most
    /// recently yielded chunk were left unconsumed by the engine.
    ///
    /// This mirrors C `infback.c`'s `inf_leave` writing `strm->avail_in = have`
    /// (and `next` at the unconsumed offset): an FFI adapter overriding this can
    /// restore the C `z_stream`'s `next_in`/`avail_in` cursors to point at the
    /// unconsumed tail of the last provider buffer. The default is a no-op, so
    /// pure-Rust callers that do not track raw cursors are unaffected.
    fn set_unconsumed(&mut self, _unconsumed: usize) {}
}

/// Consumes output bytes produced by [`inflate_back`], replacing the C
/// `out_func` callback.
///
/// The C prototype is
/// `typedef int (*out_func)(void FAR *, unsigned char FAR *, unsigned)`: it is
/// handed a pointer and a length and returns `0` on success or non-zero to
/// abort.
///
/// The safe Rust equivalent receives the produced bytes as a slice and returns
/// `Ok(())` on success or `Err(())` to abort (the non-zero C return). On
/// `Err(())`, [`inflate_back`] stops and returns [`ReturnCode::BufError`],
/// matching the C behavior when `out()` fails.
///
/// The unit error type (`Result<(), ()>`) is intentional: it mirrors the C
/// `out_func` contract exactly (zero for success, non-zero to abort) without
/// inventing a richer error the callback cannot supply. Hence
/// `clippy::result_unit_err` is allowed here.
#[allow(clippy::result_unit_err)]
pub trait OutFunc {
    /// Consumes `buf`; returns `Err(())` to abort decoding.
    fn write_output(&mut self, buf: &[u8]) -> Result<(), ()>;
}

/// The mutable working registers of a single [`inflate_back`] call.
///
/// This bundles the parts of the C `inflateBack` local state that live in
/// registers between the C `LOAD`/`RESTORE` macros — the bit accumulator
/// (`hold`/`bits`), the current input chunk and cursor (`next`/`have`), and the
/// output-window cursor (`put`/`left`) — together with the two I/O callbacks.
///
/// The current input chunk is held as an owned [`Vec<u8>`] copy of whatever the
/// [`InFunc`] most recently yielded. Copying (rather than borrowing the
/// provider's slice for the lifetime of the decode) keeps the whole routine in
/// safe Rust without fighting the borrow checker over a long-lived borrow of
/// the provider: the provider's slice is copied in and the borrow released
/// immediately, so [`InFunc::next_input`] can be called again freely.
struct BackCtx<'a, I: InFunc, O: OutFunc> {
    /// Owned copy of the current input chunk (C `next`/`have` source buffer).
    inb: Vec<u8>,
    /// Read cursor into [`inb`](BackCtx::inb); `inb.len() - next` is the C
    /// `have`.
    next: usize,
    /// Input bit accumulator (C `hold`; only the low 32 bits are ever used).
    hold: u32,
    /// Number of valid bits currently in [`hold`](BackCtx::hold) (C `bits`).
    bits: u32,
    /// Write cursor into the window/output buffer (offset of C `put` from the
    /// window base).
    put: usize,
    /// Remaining free space in the window from [`put`](BackCtx::put) to the end
    /// (C `left`). The invariant `put + left == wsize` always holds.
    left: usize,
    /// Window size in bytes (`1 << windowBits`); a cached copy of
    /// [`InflateState::wsize`].
    wsize: usize,
    /// Input provider (C `in` / `in_desc`).
    src: &'a mut I,
    /// Output sink (C `out` / `out_desc`).
    sink: &'a mut O,
}

impl<I: InFunc, O: OutFunc> BackCtx<'_, I, O> {
    /// C `PULL()`: assure at least one input byte is available, refilling from
    /// the [`InFunc`] when the current chunk is exhausted.
    ///
    /// Returns `Err(ReturnCode::BufError)` when the provider yields no more
    /// input, matching the C macro's `goto inf_leave` with `ret = Z_BUF_ERROR`.
    #[inline]
    fn pull(&mut self) -> Result<(), ReturnCode> {
        if self.next == self.inb.len() {
            let chunk = self.src.next_input();
            if chunk.is_empty() {
                return Err(ReturnCode::BufError);
            }
            // Copy the chunk into the owned buffer, releasing the borrow of the
            // provider immediately (reusing the existing allocation).
            self.inb.clear();
            self.inb.extend_from_slice(chunk);
            self.next = 0;
        }
        Ok(())
    }

    /// C `PULLBYTE()`: pull one input byte into the bit accumulator.
    #[inline]
    fn pull_byte(&mut self) -> Result<(), ReturnCode> {
        self.pull()?;
        // The low `bits` positions are occupied; the new byte's 8 bits sit
        // above them, so `|` is exactly the C `+=` here (no carry, no overflow:
        // `bits < 32` at every call site, so the shift is well-defined).
        self.hold |= u32::from(self.inb[self.next]) << self.bits;
        self.next += 1;
        self.bits += 8;
        Ok(())
    }

    /// C `NEEDBITS(n)`: assure at least `n` bits are in the accumulator.
    #[inline]
    fn need_bits(&mut self, n: u32) -> Result<(), ReturnCode> {
        while self.bits < n {
            self.pull_byte()?;
        }
        Ok(())
    }

    /// C `BITS(n)`: return the low `n` bits of the accumulator.
    #[inline]
    fn bits_val(&self, n: u32) -> u32 {
        self.hold & low_mask(n)
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

    /// C `ROOM()`: assure some output space by flushing the window when it is
    /// full.
    ///
    /// When the window is full ([`left`](BackCtx::left) is `0`) the whole window
    /// is handed to the [`OutFunc`], `whave` is set to the full window size, and
    /// the cursor is reset to the window base. Returns
    /// `Err(ReturnCode::BufError)` if the sink aborts, matching the C macro.
    #[inline]
    fn room(&mut self, window: &mut [u8], whave: &mut u32) -> Result<(), ReturnCode> {
        if self.left == 0 {
            self.put = 0;
            self.left = self.wsize;
            *whave = self.wsize as u32;
            if self.sink.write_output(&window[..self.wsize]).is_err() {
                return Err(ReturnCode::BufError);
            }
        }
        Ok(())
    }
}

impl<I: InFunc, O: OutFunc> BackCtx<'_, I, O> {
    /// Decode one Huffman symbol using a two-level decode table, returning the
    /// resolved leaf [`Code`].
    ///
    /// This is the shared decode used for both literal/length codes and
    /// distance codes in the per-symbol path (a faithful port of the two nested
    /// loops in `infback.c` L432-L447 and L486-L501). `table` is the code table
    /// (either a module-static fixed table or a sub-slice of
    /// [`InflateState::codes`]) and `root_bits` is its root index width
    /// (`state.lenbits` or `state.distbits`).
    ///
    /// The first-level loop assures just enough bits for the root code without
    /// over-reading; a second level is walked only for a table-link entry
    /// (`op != 0 && op & 0xf0 == 0`). Bits for the resolved code (and any parent
    /// table-link bits) are consumed before returning, exactly as the C
    /// `DROPBITS` sequence does.
    #[inline]
    fn decode_symbol(&mut self, table: &[Code], root_bits: u32) -> Result<Code, ReturnCode> {
        // First-level lookup: pull bytes until the indexed code fits in `bits`.
        let mut here: Code;
        loop {
            here = table[self.bits_val(root_bits) as usize];
            if u32::from(here.bits) <= self.bits {
                break;
            }
            self.pull_byte()?;
        }

        // Second-level lookup for a table-link entry (op is a nonzero index
        // width with no high-nibble flags set). A literal (op == 0) or a
        // length/distance/EOB/invalid leaf (op & 0xf0 != 0) is already resolved.
        if here.op != 0 && (here.op & 0xf0) == 0 {
            let last = here;
            loop {
                let idx = last.val as usize
                    + (self.bits_val(u32::from(last.bits) + u32::from(last.op)) >> last.bits)
                        as usize;
                here = table[idx];
                if u32::from(last.bits) + u32::from(here.bits) <= self.bits {
                    break;
                }
                self.pull_byte()?;
            }
            self.drop_bits(u32::from(last.bits));
        }
        self.drop_bits(u32::from(here.bits));
        Ok(here)
    }
}

/// Initialize an [`InflateState`] for raw-callback back-inflate, allocating the
/// owned sliding window/output buffer.
///
/// Port of `inflateBackInit_` (`infback.c` L25-L64). `inflateBack` supports
/// **raw** DEFLATE only, so `window_bits` must be in `8..=15`
/// ([`MIN_WBITS`]..=[`MAX_WBITS`](crate::constants::MAX_WBITS)); any other value
/// yields [`ReturnCode::StreamError`], matching the C `Z_STREAM_ERROR` return.
///
/// On success the returned boxed state owns a window of `1 << window_bits`
/// bytes (the AAP §0.6.3 owned-buffer model — the C API's caller-supplied
/// `window` argument is replaced by this owned allocation), with `dmax` set to
/// `32768`, `sane` enabled, and the window-geometry fields initialized. The
/// state is created with `wrap = 0` (raw framing). The block-decode `mode` is
/// intentionally *not* set here; [`inflate_back`] sets it to
/// [`InflateMode::Type`] on entry, exactly as the C code does.
///
/// # Errors
///
/// Returns [`ReturnCode::StreamError`] if `window_bits` is outside `8..=15`.
///
/// # Examples
///
/// ```
/// # use zlib_rs::inflate::back::inflate_back_init;
/// let state = inflate_back_init(15).expect("15 is a valid windowBits");
/// assert_eq!(state.wsize, 1 << 15);
/// assert!(inflate_back_init(7).is_err());
/// assert!(inflate_back_init(16).is_err());
/// ```
#[inline]
pub fn inflate_back_init(window_bits: i32) -> Result<Box<InflateState>, ReturnCode> {
    inflate_back_init_in(AllocHook::none(), window_bits)
}

/// Same as [`inflate_back_init`] but threads an explicit [`AllocHook`] so the
/// owned back-inflate window is allocated through the caller's `zalloc`/`zfree`
/// when one was installed via the FFI `z_stream` (AAP §0.6.3 has-hook clause;
/// QA FINDING-3). In back-inflate the window *is* the output buffer, so unlike
/// the streaming `inflate` path this allocation is eager (performed here rather
/// than lazily in `updatewindow`); routing it through the hook means the
/// output buffer honors a caller-supplied allocator exactly as reference zlib's
/// `ZALLOC(strm, ...)` does.
///
/// # Errors
///
/// Returns [`ReturnCode::StreamError`] if `window_bits` is outside `8..=15`.
pub fn inflate_back_init_in(
    hook: AllocHook,
    window_bits: i32,
) -> Result<Box<InflateState>, ReturnCode> {
    // C L33-L35: reject windowBits outside the raw range 8..=15.
    if !(MIN_WBITS..=MAX_WBITS).contains(&window_bits) {
        return Err(ReturnCode::StreamError);
    }

    // Raw stream: wrap = 0. `new_in` already sets dmax = 32768, sane = true, and
    // records `hook` so any window (re)allocation routes through it.
    let mut state = InflateState::new_in(hook, 0, window_bits as u32);

    // C L56-L62: set the window geometry and allocate the owned window through
    // the caller's allocator hook (or the global allocator when no hook is
    // installed). An active-hook OOM surfaces as `Z_MEM_ERROR` (M7); C's
    // `inflateBackInit_` likewise returns `Z_MEM_ERROR` when the window `ZALLOC`
    // fails.
    state.dmax = 32768;
    state.wbits = window_bits as u32;
    state.wsize = 1u32 << window_bits;
    state.whave = 0;
    state.wnext = 0;
    state.sane = true;
    state.window =
        AllocBuffer::try_zeroed(state.wsize as usize, hook).ok_or(ReturnCode::MemError)?;

    Ok(state)
}

/// Handle the `TYPE` state: read a block header and dispatch on the block type.
///
/// Port of `infback.c` L228-L260. Reads the final-block flag and the 2-bit
/// block type, then transitions to [`InflateMode::Stored`], the fixed-code
/// [`InflateMode::Len`] path (configuring the module-static fixed tables, as
/// C's `inflate_fixed` does: `lenbits = 9`, `distbits = 5`),
/// [`InflateMode::Table`] (dynamic codes), or [`InflateMode::Bad`] for the
/// reserved block type `3`. When the previous block was final, byte-aligns and
/// transitions to [`InflateMode::Done`].
///
/// Returns `Err(ReturnCode::BufError)` only if the input callback is exhausted
/// while reading the header.
fn do_type<I: InFunc, O: OutFunc>(
    ctx: &mut BackCtx<'_, I, O>,
    state: &mut InflateState,
) -> Result<(), ReturnCode> {
    if state.last {
        // C L230-L233: at the last block, discard padding to a byte boundary
        // and finish.
        ctx.byte_align();
        state.mode = InflateMode::Done;
        return Ok(());
    }

    ctx.need_bits(3)?;
    state.last = ctx.bits_val(1) != 0;
    ctx.drop_bits(1);
    match ctx.bits_val(2) {
        0 => state.mode = InflateMode::Stored,
        1 => {
            // Fixed Huffman block: point at the module-static fixed tables with
            // their canonical root widths (C `inflate_fixed`).
            state.lentable = TableSource::Fixed;
            state.lenbits = 9;
            state.disttable = TableSource::Fixed;
            state.distbits = 5;
            state.mode = InflateMode::Len;
        }
        2 => state.mode = InflateMode::Table,
        // C L255-L257: reserved block type 3 -> "invalid block type".
        _ => state.mode = InflateMode::Bad,
    }
    // C L259: consume the type bits in all cases (including BAD).
    ctx.drop_bits(2);
    Ok(())
}

/// Handle the `STORED` state: copy an uncompressed (stored) block straight from
/// input to the window/output.
///
/// Port of `infback.c` L262-L292. Byte-aligns, reads the 16-bit length and its
/// one's-complement check (transitioning to [`InflateMode::Bad`] on mismatch),
/// then streams `length` bytes through the input ([`BackCtx::pull`]) and output
/// ([`BackCtx::room`]) callbacks. Returns to [`InflateMode::Type`] on
/// completion.
///
/// # Errors
///
/// Returns `Err(ReturnCode::BufError)` if the input or output callback fails
/// mid-copy.
fn do_stored<I: InFunc, O: OutFunc>(
    ctx: &mut BackCtx<'_, I, O>,
    state: &mut InflateState,
) -> Result<(), ReturnCode> {
    // C L264-L266: align to a byte boundary and read LEN/NLEN (32 bits).
    ctx.byte_align();
    ctx.need_bits(32)?;
    if (ctx.hold & 0xffff) != ((ctx.hold >> 16) ^ 0xffff) {
        // C L266-L269: LEN and ~NLEN disagree.
        state.mode = InflateMode::Bad;
        return Ok(());
    }
    let mut length = ctx.hold & 0xffff;
    ctx.init_bits();

    // C L277-L289: copy `length` stored bytes input -> window/output.
    while length != 0 {
        let mut copy = length as usize;
        ctx.pull()?;
        ctx.room(&mut state.window, &mut state.whave)?;
        let have = ctx.inb.len() - ctx.next;
        if copy > have {
            copy = have;
        }
        if copy > ctx.left {
            copy = ctx.left;
        }
        state.window[ctx.put..ctx.put + copy].copy_from_slice(&ctx.inb[ctx.next..ctx.next + copy]);
        ctx.next += copy;
        ctx.left -= copy;
        ctx.put += copy;
        length -= copy as u32;
    }

    state.mode = InflateMode::Type;
    Ok(())
}

/// Handle the `TABLE` state: read a dynamic block's Huffman code descriptor and
/// build the literal/length and distance decode tables.
///
/// Port of `infback.c` L294-L419. Reads the `HLIT`/`HDIST`/`HCLEN` counts,
/// builds the code-length code table, decodes the literal/length and distance
/// code lengths (including the `16`/`17`/`18` repeat codes), and finally builds
/// the two decode tables via [`inflate_table`] into
/// [`InflateState::codes`]. On success transitions to [`InflateMode::Len`];
/// every malformed-input branch transitions to [`InflateMode::Bad`].
///
/// The code-length codes are decoded with an inline **single-level** lookup
/// that defers `DROPBITS` until after the repeat's extra bits have been
/// requested — this differs from the two-level [`BackCtx::decode_symbol`] used
/// for literal/length and distance symbols, and mirrors the C control flow
/// exactly.
///
/// # Errors
///
/// Returns `Err(ReturnCode::BufError)` if the input callback is exhausted while
/// reading the descriptor or code lengths.
fn do_table<I: InFunc, O: OutFunc>(
    ctx: &mut BackCtx<'_, I, O>,
    state: &mut InflateState,
) -> Result<(), ReturnCode> {
    // C L296-L302: read HLIT (nlen), HDIST (ndist) and HCLEN (ncode).
    ctx.need_bits(14)?;
    let nlen = (ctx.bits_val(5) + 257) as usize;
    ctx.drop_bits(5);
    let ndist = (ctx.bits_val(5) + 1) as usize;
    ctx.drop_bits(5);
    let ncode = (ctx.bits_val(4) + 4) as usize;
    ctx.drop_bits(4);

    // C L303-L309: bound the symbol counts.
    if nlen > 286 || ndist > 30 {
        state.mode = InflateMode::Bad;
        return Ok(());
    }

    // C L313-L321: read the `ncode` code-length code lengths in `ORDER`
    // permutation, zeroing the remainder of the 19-entry alphabet.
    for &ord in ORDER.iter().take(ncode) {
        ctx.need_bits(3)?;
        state.lens[ord as usize] = ctx.bits_val(3) as u16;
        ctx.drop_bits(3);
    }
    for &ord in ORDER.iter().skip(ncode) {
        state.lens[ord as usize] = 0;
    }

    // C L322-L331: build the code-length code table at codes[0..].
    let mut code_index = 0usize;
    let mut code_bits: usize = 7;
    if inflate_table(
        CodeType::Codes,
        &state.lens,
        19,
        &mut state.codes,
        &mut code_index,
        &mut code_bits,
        &mut state.work,
    )
    .is_err()
    {
        state.mode = InflateMode::Bad;
        return Ok(());
    }

    // C L334-L383: decode the nlen + ndist literal/length and distance code
    // lengths, expanding the 16/17/18 repeat codes.
    let total = nlen + ndist;
    let mut n = 0usize;
    while n < total {
        // Inline single-level decode of one code-length code (defers DROPBITS).
        let mut here: Code;
        loop {
            let idx = ctx.bits_val(code_bits as u32) as usize;
            here = state.codes[idx];
            if u32::from(here.bits) <= ctx.bits {
                break;
            }
            ctx.pull_byte()?;
        }

        if here.val < 16 {
            // C L342-L345: a literal code length.
            ctx.drop_bits(u32::from(here.bits));
            state.lens[n] = here.val;
            n += 1;
        } else {
            // C L346-L382: a repeat code (16/17/18).
            let repeat_val: u16;
            let mut copy: usize;
            if here.val == 16 {
                // Repeat the previous length 3..6 times.
                ctx.need_bits(u32::from(here.bits) + 2)?;
                ctx.drop_bits(u32::from(here.bits));
                if n == 0 {
                    // C L350-L354: nothing to repeat.
                    state.mode = InflateMode::Bad;
                    return Ok(());
                }
                repeat_val = state.lens[n - 1];
                copy = 3 + ctx.bits_val(2) as usize;
                ctx.drop_bits(2);
            } else if here.val == 17 {
                // Repeat a zero length 3..10 times.
                ctx.need_bits(u32::from(here.bits) + 3)?;
                ctx.drop_bits(u32::from(here.bits));
                repeat_val = 0;
                copy = 3 + ctx.bits_val(3) as usize;
                ctx.drop_bits(3);
            } else {
                // Repeat a zero length 11..138 times.
                ctx.need_bits(u32::from(here.bits) + 7)?;
                ctx.drop_bits(u32::from(here.bits));
                repeat_val = 0;
                copy = 11 + ctx.bits_val(7) as usize;
                ctx.drop_bits(7);
            }
            // C L374-L379: a repeat must not overrun the declared code counts.
            if n + copy > total {
                state.mode = InflateMode::Bad;
                return Ok(());
            }
            while copy != 0 {
                state.lens[n] = repeat_val;
                n += 1;
                copy -= 1;
            }
        }
    }

    // C L388-L394: a valid block must define an end-of-block code.
    if state.lens[256] == 0 {
        state.mode = InflateMode::Bad;
        return Ok(());
    }

    // C L399-L408: build the literal/length table at codes[0..]. Do not change
    // the root width (9) without revisiting the ENOUGH sizing in tables.rs.
    state.lencode = 0;
    state.lentable = TableSource::Dynamic;
    let mut table_index = 0usize;
    let mut lenbits: usize = 9;
    if inflate_table(
        CodeType::Lens,
        &state.lens,
        nlen,
        &mut state.codes,
        &mut table_index,
        &mut lenbits,
        &mut state.work,
    )
    .is_err()
    {
        state.mode = InflateMode::Bad;
        return Ok(());
    }
    state.lenbits = lenbits as u32;

    // C L409-L417: build the distance table immediately after the length table
    // (root width 6). `table_index` is now the arena cursor past the length
    // table, so it becomes the distance table's base offset.
    state.distcode = table_index;
    state.disttable = TableSource::Dynamic;
    let mut distbits: usize = 6;
    if inflate_table(
        CodeType::Dists,
        &state.lens[nlen..],
        ndist,
        &mut state.codes,
        &mut table_index,
        &mut distbits,
        &mut state.work,
    )
    .is_err()
    {
        state.mode = InflateMode::Bad;
        return Ok(());
    }
    state.distbits = distbits as u32;

    // C L418-L420: tables built; fall through to symbol decoding.
    state.mode = InflateMode::Len;
    Ok(())
}

/// Handle the `LEN` state: decode one literal/length symbol and, for a length
/// code, its distance and the resulting match copy.
///
/// Port of `infback.c` L422-L543, using the single-symbol decode path only (see
/// the module-level note on `inflate_fast`). A literal is emitted to the
/// window/output; an end-of-block returns to [`InflateMode::Type`]; a
/// length/distance pair copies the match from the window with correct ring-wrap
/// and overlapping-copy (RLE) semantics. Malformed codes transition to
/// [`InflateMode::Bad`].
///
/// # Errors
///
/// Returns `Err(ReturnCode::BufError)` if the input or output callback fails
/// while decoding or emitting.
fn do_len<I: InFunc, O: OutFunc>(
    ctx: &mut BackCtx<'_, I, O>,
    state: &mut InflateState,
) -> Result<(), ReturnCode> {
    // C L432-L448: decode a literal/length code (two-level lookup).
    let lenbits = state.lenbits;
    let here = {
        let len_table: &[Code] = match state.lentable {
            TableSource::Fixed => &LENFIX[..],
            TableSource::Dynamic => &state.codes[state.lencode..],
        };
        ctx.decode_symbol(len_table, lenbits)?
    };

    // C L450-L459: a literal byte.
    if here.op == 0 {
        ctx.room(&mut state.window, &mut state.whave)?;
        state.window[ctx.put] = here.val as u8;
        ctx.put += 1;
        ctx.left -= 1;
        state.mode = InflateMode::Len;
        return Ok(());
    }

    // C L462-L466: end of block.
    if here.op & 32 != 0 {
        state.mode = InflateMode::Type;
        return Ok(());
    }

    // C L469-L473: invalid literal/length code.
    if here.op & 64 != 0 {
        state.mode = InflateMode::Bad;
        return Ok(());
    }

    // C L476-L482: length base plus any extra bits.
    let mut length = here.val as usize;
    let extra = u32::from(here.op) & 15;
    if extra != 0 {
        ctx.need_bits(extra)?;
        length += ctx.bits_val(extra) as usize;
        ctx.drop_bits(extra);
    }

    // C L485-L506: decode a distance code (two-level lookup).
    let distbits = state.distbits;
    let dhere = {
        let dist_table: &[Code] = match state.disttable {
            TableSource::Fixed => &DISTFIX[..],
            TableSource::Dynamic => &state.codes[state.distcode..],
        };
        ctx.decode_symbol(dist_table, distbits)?
    };
    if dhere.op & 64 != 0 {
        // C L502-L505: invalid distance code.
        state.mode = InflateMode::Bad;
        return Ok(());
    }
    let mut offset = dhere.val as usize;

    // C L509-L515: distance base plus any extra bits.
    let dextra = u32::from(dhere.op) & 15;
    if dextra != 0 {
        ctx.need_bits(dextra)?;
        offset += ctx.bits_val(dextra) as usize;
        ctx.drop_bits(dextra);
    }

    // C L516-L521: reject a distance that reaches past the available history.
    // NOTE: `infback` does *not* consult `state.sane` here (unlike the main
    // `inflate` driver); this follows the C source verbatim.
    let reach = if state.whave < ctx.wsize as u32 {
        ctx.left
    } else {
        0
    };
    if offset > ctx.wsize - reach {
        state.mode = InflateMode::Bad;
        return Ok(());
    }

    // C L524-L542: copy the match from the window to the output. The window is
    // a ring, so a copy may wrap; overlapping copies (offset < length) are
    // performed byte-by-byte to reproduce the LZ77/RLE semantics exactly.
    loop {
        ctx.room(&mut state.window, &mut state.whave)?;
        let mut copy = ctx.wsize - offset;
        let from;
        if copy < ctx.left {
            // The source wraps around to the tail of the window.
            from = ctx.put + copy;
            copy = ctx.left - copy;
        } else {
            // The source is a direct back-reference within the current fill.
            from = ctx.put - offset;
            copy = ctx.left;
        }
        if copy > length {
            copy = length;
        }
        length -= copy;
        ctx.left -= copy;
        // Byte-by-byte in increasing order so an overlapping copy (offset <
        // run length) propagates like C's `*put++ = *from++` (RLE); `memmove`
        // / `copy_within` semantics would be wrong here.
        for k in 0..copy {
            let byte = state.window[from + k];
            state.window[ctx.put + k] = byte;
        }
        ctx.put += copy;
        if length == 0 {
            break;
        }
    }

    state.mode = InflateMode::Len;
    Ok(())
}

/// Flush any output still buffered in the window at loop exit (C `inf_leave`,
/// `infback.c` L561-L569).
///
/// When the window is partially filled (`left < wsize`) the pending bytes
/// `window[0..wsize - left]` are handed to the [`OutFunc`]. If that final write
/// fails and decoding had otherwise succeeded ([`ReturnCode::StreamEnd`]), the
/// result is downgraded to [`ReturnCode::BufError`], matching the C behavior.
fn inf_leave<I: InFunc, O: OutFunc>(
    ctx: &mut BackCtx<'_, I, O>,
    state: &InflateState,
    ret: ReturnCode,
) -> ReturnCode {
    if ctx.left < ctx.wsize {
        let n = ctx.wsize - ctx.left;
        if ctx.sink.write_output(&state.window[..n]).is_err() && ret == ReturnCode::StreamEnd {
            return ReturnCode::BufError;
        }
    }
    ret
}

/// Decode a single **raw** DEFLATE stream, pulling input from `in_func` and
/// pushing output to `out_func` through the caller-supplied window.
///
/// Port of `inflateBack` (`infback.c` L191-L570). The `state` must have been
/// produced by [`inflate_back_init`]. Decoding runs to completion in this one
/// call: the [`InFunc`] is invoked whenever more input is needed and the
/// [`OutFunc`] whenever the window fills or at end-of-stream. Only raw DEFLATE
/// (RFC 1951) is handled — there is no zlib or gzip wrapper.
///
/// # Returns
///
/// - [`ReturnCode::StreamEnd`] on a successfully decoded final block.
/// - [`ReturnCode::DataError`] on a DEFLATE format error (an invalid block
///   type, bad stored-block lengths, a malformed dynamic header, an invalid
///   code, or a distance that reaches too far back).
/// - [`ReturnCode::BufError`] if the input callback runs dry or the output
///   callback aborts.
/// - [`ReturnCode::StreamError`] if `state` is not a valid, window-sized
///   back-inflate state.
///
/// # Examples
///
/// ```
/// # use zlib_rs::inflate::back::{inflate_back, inflate_back_init, InFunc, OutFunc};
/// # use zlib_rs::error::ReturnCode;
/// // Raw DEFLATE for the empty stream: a final fixed block containing only the
/// // end-of-block code.
/// struct OneShot<'a>(Option<&'a [u8]>);
/// impl InFunc for OneShot<'_> {
///     fn next_input(&mut self) -> &[u8] {
///         self.0.take().unwrap_or(&[])
///     }
/// }
/// struct Collect(Vec<u8>);
/// impl OutFunc for Collect {
///     fn write_output(&mut self, buf: &[u8]) -> Result<(), ()> {
///         self.0.extend_from_slice(buf);
///         Ok(())
///     }
/// }
/// let mut state = inflate_back_init(15).unwrap();
/// let data = [0x03u8, 0x00];
/// let mut src = OneShot(Some(&data));
/// let mut sink = Collect(Vec::new());
/// assert_eq!(inflate_back(&mut state, &mut src, &mut sink), ReturnCode::StreamEnd);
/// assert!(sink.0.is_empty());
/// ```
pub fn inflate_back<I: InFunc, O: OutFunc>(
    state: &mut InflateState,
    in_func: &mut I,
    out_func: &mut O,
) -> ReturnCode {
    // C L208-L211: the state must be initialized (via inflate_back_init), which
    // means a valid mode and an allocated, correctly sized window.
    let wsize = state.wsize as usize;
    if !state.is_valid() || wsize == 0 || state.window.len() < wsize {
        return ReturnCode::StreamError;
    }

    // C L213-L223: reset per-call state and load the registers.
    state.mode = InflateMode::Type;
    state.last = false;
    state.whave = 0;

    let mut ctx = BackCtx {
        inb: Vec::new(),
        next: 0,
        hold: 0,
        bits: 0,
        put: 0,
        left: wsize,
        wsize,
        src: in_func,
        sink: out_func,
    };

    // C L225-L560: run the block/symbol state machine. Each handler returns
    // `Err(code)` to leave immediately (a failed callback), or `Ok(())` after
    // updating `state.mode` (possibly to `Done`/`Bad`).
    let ret: ReturnCode = loop {
        let step = match state.mode {
            InflateMode::Type => do_type(&mut ctx, state),
            InflateMode::Stored => do_stored(&mut ctx, state),
            InflateMode::Table => do_table(&mut ctx, state),
            InflateMode::Len => do_len(&mut ctx, state),
            // C L545-L548: DONE.
            InflateMode::Done => break ReturnCode::StreamEnd,
            // C L550-L551: BAD (a format error; `strm->msg` was set in C).
            InflateMode::Bad => break ReturnCode::DataError,
            // No other mode is reachable in back-inflate.
            _ => break ReturnCode::StreamError,
        };
        if let Err(code) = step {
            break code;
        }
    };

    // Report the unconsumed tail of the last provider chunk (C `have`) so an FFI
    // adapter can restore `next_in`/`avail_in` exactly like C `inf_leave`
    // (infback.c L561-L569 sets `strm->avail_in = have`). `next <= inb.len()`, so
    // this is the number of bytes pulled but not yet consumed from that chunk.
    let unconsumed = ctx.inb.len() - ctx.next;
    ctx.src.set_unconsumed(unconsumed);

    // C L561-L569: flush the tail of the window and return.
    inf_leave(&mut ctx, state, ret)
}

/// Finish a back-inflate session (C `inflateBackEnd`, `infback.c` L562-L579).
///
/// In C this frees the internal state; in Rust the owned window and state are
/// released when the [`Box`] is dropped at the end of this function, so this
/// wrapper exists for API/FFI parity. It validates the state and returns
/// [`ReturnCode::Ok`], or [`ReturnCode::StreamError`] if the state is not a
/// valid inflate state.
///
/// Callers using the idiomatic API may simply drop the [`Box<InflateState>`]
/// instead of calling this function; the effect is identical.
pub fn inflate_back_end(state: Box<InflateState>) -> ReturnCode {
    if !state.is_valid() {
        return ReturnCode::StreamError;
    }
    // `state` and its owned window are freed here, subsuming the C `ZFREE`.
    ReturnCode::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference raw-DEFLATE (RFC 1951) test vectors, produced by Python's
    // `zlib` (the reference C implementation) with a negative `wbits` (raw
    // framing). Decoding these and comparing against the known plaintext is a
    // byte-exact interoperability check against reference zlib.

    /// Stored (uncompressed) block: `"Hello, stored DEFLATE world!"`.
    const STORED_VEC: [u8; 33] = [
        0x01, 0x1c, 0x00, 0xe3, 0xff, 0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x73, 0x74, 0x6f,
        0x72, 0x65, 0x64, 0x20, 0x44, 0x45, 0x46, 0x4c, 0x41, 0x54, 0x45, 0x20, 0x77, 0x6f, 0x72,
        0x6c, 0x64, 0x21,
    ];
    /// Fixed-Huffman block: `"abababababcdcdcd"`.
    const FIXED_VEC: [u8; 10] = [0x4b, 0x4c, 0x4a, 0x84, 0xc2, 0xe4, 0x14, 0x10, 0x04, 0x00];
    /// Dynamic-Huffman block: a 175-byte pangram passage.
    const DYNAMIC_VEC: [u8; 79] = [
        0xb5, 0xcb, 0xc7, 0x01, 0x80, 0x20, 0x10, 0x05, 0xd1, 0x56, 0x7e, 0x05, 0xd4, 0xe2, 0xc1,
        0x06, 0x40, 0x49, 0x06, 0x56, 0xb2, 0x50, 0xbd, 0xdb, 0x84, 0xe7, 0x79, 0xb3, 0x3a, 0x8d,
        0x58, 0xfd, 0x76, 0x42, 0x25, 0xea, 0x01, 0x86, 0x5e, 0x1c, 0xf5, 0x7e, 0x32, 0xa8, 0xe9,
        0x84, 0xc2, 0xf9, 0x92, 0x73, 0x60, 0x27, 0x2b, 0xb0, 0xfe, 0x86, 0x17, 0xc9, 0xee, 0x1e,
        0x50, 0x8c, 0xba, 0x2f, 0x0e, 0xc6, 0x37, 0xcd, 0x69, 0xea, 0x80, 0xcb, 0xc7, 0x4a, 0x89,
        0x5f, 0x9b, 0xc5, 0x07,
    ];
    /// 2048 bytes of `"0123456789abcdef"` repeated, decoded with a 512-byte
    /// (`windowBits = 9`) window so it forces multiple `ROOM` flushes and
    /// cross-wrap back-references.
    const WRAP_VEC: [u8; 34] = [
        0x33, 0x30, 0x34, 0x32, 0x36, 0x31, 0x35, 0x33, 0xb7, 0xb0, 0x4c, 0x4c, 0x4a, 0x4e, 0x49,
        0x4d, 0x33, 0x18, 0xe5, 0x8f, 0xf2, 0x47, 0xf9, 0xa3, 0xfc, 0x51, 0xfe, 0x28, 0x7f, 0x94,
        0x3f, 0xec, 0xf9, 0x00,
    ];
    /// The empty stream: a final fixed block with only an end-of-block code.
    const EMPTY_VEC: [u8; 2] = [0x03, 0x00];

    /// Input provider that yields the entire buffer in a single chunk, then
    /// signals end-of-input.
    struct SliceIn<'a> {
        data: &'a [u8],
        done: bool,
    }
    impl InFunc for SliceIn<'_> {
        fn next_input(&mut self) -> &[u8] {
            if self.done {
                &[]
            } else {
                self.done = true;
                self.data
            }
        }
    }

    /// Input provider that yields one byte at a time, exercising the `PULL`
    /// refill path across many callback invocations.
    struct ChunkyIn<'a> {
        data: &'a [u8],
        pos: usize,
    }
    impl InFunc for ChunkyIn<'_> {
        fn next_input(&mut self) -> &[u8] {
            if self.pos >= self.data.len() {
                return &[];
            }
            let one = &self.data[self.pos..self.pos + 1];
            self.pos += 1;
            one
        }
    }

    /// Output sink that accumulates every emitted byte.
    struct VecOut {
        data: Vec<u8>,
    }
    impl OutFunc for VecOut {
        fn write_output(&mut self, buf: &[u8]) -> Result<(), ()> {
            self.data.extend_from_slice(buf);
            Ok(())
        }
    }

    /// Output sink that aborts on the first write.
    struct AlwaysErrOut;
    impl OutFunc for AlwaysErrOut {
        fn write_output(&mut self, _buf: &[u8]) -> Result<(), ()> {
            Err(())
        }
    }

    /// Decode `data` in one shot with the given window size, returning the
    /// return code and the collected output.
    fn run(window_bits: i32, data: &[u8]) -> (ReturnCode, Vec<u8>) {
        let mut state = inflate_back_init(window_bits).expect("valid windowBits");
        let mut src = SliceIn { data, done: false };
        let mut sink = VecOut { data: Vec::new() };
        let rc = inflate_back(&mut state, &mut src, &mut sink);
        (rc, sink.data)
    }

    /// The 175-byte plaintext behind [`DYNAMIC_VEC`].
    fn dynamic_plaintext() -> Vec<u8> {
        let base = b"The quick brown fox jumps over the lazy dog. ";
        let mut expected: Vec<u8> = Vec::new();
        for _ in 0..3 {
            expected.extend_from_slice(base);
        }
        expected.extend_from_slice(b"Pack my box with five dozen liquor jugs.");
        expected
    }

    #[test]
    fn order_matches_c() {
        // Verbatim `order[]` permutation from infback.c L205-L206.
        assert_eq!(
            ORDER,
            [
                16u16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15
            ]
        );
    }

    #[test]
    fn init_sets_raw_geometry() {
        let state = inflate_back_init(15).expect("15 is valid");
        assert_eq!(state.dmax, 32768);
        assert!(state.sane);
        assert_eq!(state.wbits, 15);
        assert_eq!(state.wsize, 1 << 15);
        assert_eq!(state.whave, 0);
        assert_eq!(state.wnext, 0);
        assert_eq!(state.window.len(), 1 << 15);
    }

    #[test]
    fn window_bits_out_of_range_is_stream_error() {
        // Raw inflateBack accepts only 8..=15. (`InflateState` does not derive
        // `Debug`, so match on the result rather than calling `unwrap_err`.)
        assert!(matches!(inflate_back_init(7), Err(ReturnCode::StreamError)));
        assert!(matches!(
            inflate_back_init(16),
            Err(ReturnCode::StreamError)
        ));
        assert!(matches!(inflate_back_init(0), Err(ReturnCode::StreamError)));
        assert!(matches!(
            inflate_back_init(-15),
            Err(ReturnCode::StreamError)
        ));
        assert!(inflate_back_init(8).is_ok());
        assert!(inflate_back_init(15).is_ok());
    }

    #[test]
    fn stored_block_round_trips() {
        let (rc, out) = run(15, &STORED_VEC);
        assert_eq!(rc, ReturnCode::StreamEnd);
        assert_eq!(out, b"Hello, stored DEFLATE world!");
    }

    #[test]
    fn fixed_block_round_trips() {
        let (rc, out) = run(15, &FIXED_VEC);
        assert_eq!(rc, ReturnCode::StreamEnd);
        assert_eq!(out, b"abababababcdcdcd");
    }

    #[test]
    fn dynamic_block_round_trips() {
        let (rc, out) = run(15, &DYNAMIC_VEC);
        assert_eq!(rc, ReturnCode::StreamEnd);
        assert_eq!(out, dynamic_plaintext());
    }

    #[test]
    fn empty_stream_round_trips() {
        let (rc, out) = run(15, &EMPTY_VEC);
        assert_eq!(rc, ReturnCode::StreamEnd);
        assert!(out.is_empty());
    }

    #[test]
    fn window_wrap_and_room_flush() {
        // 2048 bytes decoded through a 512-byte window: forces ROOM() to flush
        // the full window several times and resolves back-references that wrap
        // around the ring.
        let (rc, out) = run(9, &WRAP_VEC);
        assert_eq!(rc, ReturnCode::StreamEnd);
        let expected: Vec<u8> = b"0123456789abcdef"
            .iter()
            .copied()
            .cycle()
            .take(2048)
            .collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn chunky_input_refills_across_callbacks() {
        // Feed the dynamic stream one byte per callback to exercise PULL refill.
        let mut state = inflate_back_init(15).unwrap();
        let mut src = ChunkyIn {
            data: &DYNAMIC_VEC,
            pos: 0,
        };
        let mut sink = VecOut { data: Vec::new() };
        let rc = inflate_back(&mut state, &mut src, &mut sink);
        assert_eq!(rc, ReturnCode::StreamEnd);
        assert_eq!(sink.data, dynamic_plaintext());
    }

    #[test]
    fn invalid_block_type_is_data_error() {
        // A single byte: BFINAL = 1, BTYPE = 11 (reserved) -> data error.
        let (rc, out) = run(15, &[0x07]);
        assert_eq!(rc, ReturnCode::DataError);
        assert!(out.is_empty());
    }

    #[test]
    fn bad_stored_length_check_is_data_error() {
        // Stored block whose NLEN is not the one's-complement of LEN.
        // byte0 = 0x01 (BFINAL=1, stored); LEN=0x0004; NLEN=0x0000 (wrong).
        let bad = [0x01u8, 0x04, 0x00, 0x00, 0x00];
        let (rc, _out) = run(15, &bad);
        assert_eq!(rc, ReturnCode::DataError);
    }

    #[test]
    fn truncated_input_is_buf_error() {
        // Only the stored header's first byte: the decoder needs more input
        // than the callback can supply.
        let (rc, _out) = run(15, &STORED_VEC[..1]);
        assert_eq!(rc, ReturnCode::BufError);
    }

    #[test]
    fn output_abort_is_buf_error() {
        // Decoding succeeds internally, but the final flush is refused by the
        // sink; a successful decode is downgraded to BufError (C inf_leave).
        let mut state = inflate_back_init(15).unwrap();
        let mut src = SliceIn {
            data: &FIXED_VEC,
            done: false,
        };
        let mut sink = AlwaysErrOut;
        let rc = inflate_back(&mut state, &mut src, &mut sink);
        assert_eq!(rc, ReturnCode::BufError);
    }

    #[test]
    fn end_subsumes_drop() {
        // inflate_back_end validates and returns Ok; the box (and its window)
        // is freed on return.
        let state = inflate_back_init(15).unwrap();
        assert_eq!(inflate_back_end(state), ReturnCode::Ok);
    }
}
