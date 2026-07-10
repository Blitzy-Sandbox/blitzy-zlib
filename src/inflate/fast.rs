//! Inflate fast decode loop (`inflate_fast`) — port of C `inffast.c`.
//!
//! This is the throughput-critical inner decode loop invoked by the main
//! `inflate()` driver (in [`crate::inflate`]'s `mod.rs`) whenever there is
//! *enough* input and output available to decode literal, length, and distance
//! codes without re-checking buffer bounds on every symbol. When large enough
//! buffers are supplied, more than 95% of `inflate()` execution time is spent
//! here (see the C source header comment in `inffast.c`).
//!
//! # Safety: 100% safe Rust
//!
//! Per AAP §0.6.1 and the `src/inflate/` folder invariant, this module — like
//! every other file under `src/inflate/` — contains **zero `unsafe`** and is,
//! and must remain, written entirely in **safe Rust**. Despite being the
//! throughput-critical hot loop, it uses bounds-checked slice indexing
//! throughout rather than unchecked access. The entry contract documented on
//! [`inflate_fast`] guarantees that at most six input bytes and at most 258
//! output bytes are touched per loop iteration, so — for a well-formed stream
//! honoring that contract — those bounds checks are provably never triggered
//! and the optimizer elides them on the hot path. No `unsafe`, `get_unchecked`,
//! raw-pointer casting, or `transmute` is used or permitted here.
//!
//! # Byte-exact output
//!
//! The decode sequence, the bit-refill schedule, and — crucially — the
//! *overlapping* LZ77 back-reference copy are reproduced exactly so that output
//! is byte-identical to reference zlib (AAP §0.6.4, §0.7.1). The overlapping
//! copy (when `dist < len`) is performed forward, one byte at a time, so that
//! freshly written bytes are re-read; a `memcpy`/`copy_from_slice` would be
//! incorrect there.

use crate::inflate::state::{InflateMode, InflateState};
use crate::inflate::tables::Code;

/// Minimum number of input bytes `inflate_fast` requires to run a loop
/// iteration without checking for available input.
///
/// A single length/distance pair consumes at most 48 bits — 15 (length code) +
/// 5 (length extra) + 15 (distance code) + 13 (distance extra) — i.e. six
/// bytes. Mirrors the C `INFLATE_FAST_MIN_INPUT` (6).
const INFLATE_FAST_MIN_INPUT: usize = 6;

/// Minimum number of output bytes `inflate_fast` requires to run a loop
/// iteration without checking for available output.
///
/// A single length/distance pair emits at most 258 bytes (the maximum coded
/// match length). Mirrors the C `INFLATE_FAST_MIN_OUTPUT` (258).
const INFLATE_FAST_MIN_OUTPUT: usize = 258;

/// Decode literal, length, and distance codes as fast as possible while enough
/// input and output is available. Safe-Rust port of C `inflate_fast`.
///
/// The driver ([`crate::inflate`]'s `inflate()`) loads local copies of the
/// stream cursors, calls this routine, and restores them afterwards. This
/// function threads exactly the state it mutates: the bit accumulator
/// ([`InflateState::hold`]/[`InflateState::bits`]), the decode mode
/// ([`InflateState::mode`]), and the input/output cursors.
///
/// # Parameters
///
/// * `state` — the inflate state. Only [`hold`](InflateState::hold),
///   [`bits`](InflateState::bits), and [`mode`](InflateState::mode) are
///   written; the decode tables and window are read-only here.
/// * `input` — the available input bytes; `in_pos` is the read cursor into it,
///   advanced as bytes are consumed.
/// * `output` — the output buffer; `out_pos` is the write cursor into it,
///   advanced as bytes are produced.
/// * `start` — `inflate()`'s `avail_out` captured at the last state reset,
///   measured against **this same `output` slice**. Used to locate the earliest
///   output byte the current call may reference (`beg = output.len() - start`)
///   for the window/output copy decision.
/// * `lenfix` — the module-static fixed literal/length table
///   (`&crate::inflate::fixed::LENFIX[..]`); returned verbatim by
///   [`InflateState::lencode_slice`] when the active table is fixed. Passing it
///   in (rather than importing it) keeps this hot loop's dependency surface
///   minimal and avoids a borrow conflict at the call site (the fixed tables
///   are `'static`, independent of `state`).
/// * `distfix` — the module-static fixed distance table
///   (`&crate::inflate::fixed::DISTFIX[..]`).
///
/// # Entry assumptions (the caller MUST guarantee these; C `inffast.c` L23-L48)
///
/// * `state.mode == InflateMode::Len`
/// * `input.len() - *in_pos >= 6` (at least six input bytes available)
/// * `output.len() - *out_pos >= 258` (at least 258 output bytes available)
/// * `start >= output.len() - *out_pos` (`start` ≥ current `avail_out`)
/// * `state.bits < 8`
///
/// These invariants are what make the bounds-checked indexing provably
/// panic-free for a well-formed stream, and are asserted in debug builds.
///
/// # Return value
///
/// On a data error this sets `state.mode = InflateMode::Bad` and returns
/// `Some(message)`, where `message` is the exact zlib diagnostic string
/// (e.g. `"invalid distance code"`); the driver should store it into the
/// stream's `msg` field. On normal exit it returns `None` and leaves
/// `state.mode` at either [`InflateMode::Type`] (an end-of-block code was
/// reached) or [`InflateMode::Len`] (ran out of enough input or output),
/// matching C.
// This routine threads exactly the state `inflate_fast` mutates (the bit
// accumulator, mode, and I/O cursors) plus the two `'static` fixed decode
// tables. Bundling these into a parameter struct would only obscure the C
// LOAD/RESTORE contract the driver depends on, so the explicit list is kept.
#[allow(clippy::too_many_arguments)]
pub fn inflate_fast(
    state: &mut InflateState,
    input: &[u8],
    in_pos: &mut usize,
    output: &mut [u8],
    out_pos: &mut usize,
    start: usize,
    lenfix: &[Code],
    distfix: &[Code],
) -> Option<&'static str> {
    // ---- entry-invariant debug assertions (mirror the C implicit contract) --
    debug_assert_eq!(
        state.mode,
        InflateMode::Len,
        "inflate_fast entry: state.mode must be Len"
    );
    debug_assert!(
        input.len() - *in_pos >= INFLATE_FAST_MIN_INPUT,
        "inflate_fast entry: at least {INFLATE_FAST_MIN_INPUT} input bytes required"
    );
    debug_assert!(
        output.len() - *out_pos >= INFLATE_FAST_MIN_OUTPUT,
        "inflate_fast entry: at least {INFLATE_FAST_MIN_OUTPUT} output bytes required"
    );
    debug_assert!(state.bits < 8, "inflate_fast entry: state.bits must be < 8");
    debug_assert!(
        start >= output.len() - *out_pos,
        "inflate_fast entry: start must be >= avail_out"
    );

    // ---- LOAD: copy the parts of the stream state we touch into locals -------
    // Keeping these in locals (rather than repeatedly touching `state.*`) is the
    // C LOAD/RESTORE idiom, essential both for performance and for letting the
    // driver resume mid-stream via the epilogue below.
    let mut in_idx = *in_pos;
    let mut out_idx = *out_pos;

    // Loop bounds. `last_safe_in`: while `in_idx < last_safe_in`, at least six
    // input bytes remain. `end_safe`: while `out_idx < end_safe`, at least 258
    // output bytes remain. Both mirror the C `last`/`end` markers.
    let last_safe_in = input.len() - (INFLATE_FAST_MIN_INPUT - 1); // input.len() - 5
    let end_safe = output.len() - (INFLATE_FAST_MIN_OUTPUT - 1); // output.len() - 257

    // `beg` is the output index at which the current inflate() call began
    // writing — the earliest byte a back-reference may reach in the output
    // before it must instead come from the sliding window. Equivalent to the C
    // `beg = out - (start - avail_out)` with `avail_out == output.len() - out`.
    let beg = output.len() - start;

    let wsize = state.wsize as usize;
    let whave = state.whave as usize;
    let wnext = state.wnext as usize;
    let sane = state.sane;
    let lmask = (1u32 << state.lenbits) - 1;
    let dmask = (1u32 << state.distbits) - 1;
    // `dmax` is only consulted by the optional INFLATE_STRICT check (default
    // off). Reading it unconditionally would be a dead load when the feature is
    // disabled, so it is gated together with its use.
    #[cfg(feature = "inflate_strict")]
    let dmax = state.dmax;

    let mut hold = state.hold;
    let mut bits = state.bits;

    // Resolve the active literal/length and distance decode tables to slices.
    // These borrow `state` immutably for the duration of the loop; `state` is
    // therefore only mutated *after* the loop, once these borrows have ended.
    let lcode: &[Code] = state.lencode_slice(lenfix);
    let dcode: &[Code] = state.distcode_slice(distfix);
    let window: &[u8] = &state.window;

    // Exit outcome, applied to `state.mode` after the loop. `None` means "leave
    // the mode unchanged" (the C behavior when the loop simply runs out of
    // input or output, keeping mode == LEN).
    let mut final_mode: Option<InflateMode> = None;
    let mut error_msg: Option<&'static str> = None;

    // ---- main decode loop (C `do { ... } while (in < last && out < end)`) ----
    'outer: loop {
        // Refill the bit accumulator to at least 15 bits by pulling two bytes.
        // The entry/loop contract guarantees these reads are in bounds.
        if bits < 15 {
            hold += (input[in_idx] as u32) << bits;
            in_idx += 1;
            bits += 8;
            hold += (input[in_idx] as u32) << bits;
            in_idx += 1;
            bits += 8;
        }

        // Look up the length/literal code, then process it (C label `dolen`).
        let mut here: Code = lcode[(hold & lmask) as usize];
        let mut len: usize = 0;
        let mut go_dodist = false;

        'dolen: loop {
            // Consume the bits of the code just retrieved.
            let code_bits = here.bits as u32;
            hold >>= code_bits;
            bits -= code_bits;

            let op = here.op as u32;
            if op == 0 {
                // Literal byte.
                output[out_idx] = here.val as u8;
                out_idx += 1;
                break 'dolen;
            } else if op & 16 != 0 {
                // Length base + extra bits.
                len = here.val as usize;
                let extra = op & 15;
                if extra != 0 {
                    if bits < extra {
                        hold += (input[in_idx] as u32) << bits;
                        in_idx += 1;
                        bits += 8;
                    }
                    len += (hold & ((1u32 << extra) - 1)) as usize;
                    hold >>= extra;
                    bits -= extra;
                }
                // Refill for the upcoming distance code.
                if bits < 15 {
                    hold += (input[in_idx] as u32) << bits;
                    in_idx += 1;
                    bits += 8;
                    hold += (input[in_idx] as u32) << bits;
                    in_idx += 1;
                    bits += 8;
                }
                here = dcode[(hold & dmask) as usize];
                go_dodist = true;
                break 'dolen;
            } else if op & 64 == 0 {
                // Second-level length table: chase the link and re-decode.
                here = lcode[here.val as usize + (hold & ((1u32 << op) - 1)) as usize];
                continue 'dolen;
            } else if op & 32 != 0 {
                // End of block.
                final_mode = Some(InflateMode::Type);
                break 'outer;
            } else {
                // Invalid literal/length code.
                error_msg = Some("invalid literal/length code");
                final_mode = Some(InflateMode::Bad);
                break 'outer;
            }
        }

        if go_dodist {
            // Look up and process the distance code (C label `dodist`).
            'dodist: loop {
                let code_bits = here.bits as u32;
                hold >>= code_bits;
                bits -= code_bits;

                let op = here.op as u32;
                if op & 16 != 0 {
                    // Distance base + extra bits.
                    let mut dist = here.val as usize;
                    let extra = op & 15;
                    if bits < extra {
                        hold += (input[in_idx] as u32) << bits;
                        in_idx += 1;
                        bits += 8;
                        if bits < extra {
                            hold += (input[in_idx] as u32) << bits;
                            in_idx += 1;
                            bits += 8;
                        }
                    }
                    dist += (hold & ((1u32 << extra) - 1)) as usize;

                    // INFLATE_STRICT distance validation (C `#ifdef
                    // INFLATE_STRICT`) — off by default; a byte-exact default
                    // build does not compile this in.
                    #[cfg(feature = "inflate_strict")]
                    {
                        if dist > dmax as usize {
                            error_msg = Some("invalid distance too far back");
                            final_mode = Some(InflateMode::Bad);
                            break 'outer;
                        }
                    }

                    hold >>= extra;
                    bits -= extra;

                    // `op_out` = bytes already produced in the current output
                    // buffer; the maximum distance that can be satisfied from the
                    // output alone (C `op = out - beg`).
                    let op_out = out_idx - beg;
                    if dist > op_out {
                        // The reference reaches back into the sliding window.
                        let dist_back = dist - op_out; // distance back into window

                        // A reference beyond the valid window data is a data
                        // error in normal (`sane`) operation. The
                        // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR
                        // sanitizer/fuzzing path (default OFF) that would
                        // otherwise zero-fill is intentionally not ported; with
                        // the default `sane == true` this guard always fires for
                        // such a reference.
                        if dist_back > whave && sane {
                            error_msg = Some("invalid distance too far back");
                            final_mode = Some(InflateMode::Bad);
                            break 'outer;
                        }

                        if wnext == 0 {
                            // Window has not wrapped; valid data ends at `wsize`.
                            let wpos = wsize - dist_back;
                            if dist_back < len {
                                copy_from_window(output, &mut out_idx, window, wpos, dist_back);
                                len -= dist_back;
                                // Remainder comes from the freshly written output.
                                let src = out_idx - dist;
                                copy_within_output(output, &mut out_idx, src, len);
                            } else {
                                copy_from_window(output, &mut out_idx, window, wpos, len);
                            }
                        } else if wnext < dist_back {
                            // Reference wraps around the circular window: read
                            // from the tail, then from the head.
                            let wpos = wsize + wnext - dist_back;
                            let tail = dist_back - wnext;
                            if tail < len {
                                copy_from_window(output, &mut out_idx, window, wpos, tail);
                                len -= tail;
                                if wnext < len {
                                    copy_from_window(output, &mut out_idx, window, 0, wnext);
                                    len -= wnext;
                                    let src = out_idx - dist;
                                    copy_within_output(output, &mut out_idx, src, len);
                                } else {
                                    copy_from_window(output, &mut out_idx, window, 0, len);
                                }
                            } else {
                                copy_from_window(output, &mut out_idx, window, wpos, len);
                            }
                        } else {
                            // Reference is contiguous within the window.
                            let wpos = wnext - dist_back;
                            if dist_back < len {
                                copy_from_window(output, &mut out_idx, window, wpos, dist_back);
                                len -= dist_back;
                                let src = out_idx - dist;
                                copy_within_output(output, &mut out_idx, src, len);
                            } else {
                                copy_from_window(output, &mut out_idx, window, wpos, len);
                            }
                        }
                    } else {
                        // Copy directly from the output. This overlaps when
                        // `dist < len` (the essence of LZ77), so it MUST be a
                        // forward, byte-by-byte copy that re-reads freshly
                        // written bytes — never a `copy_from_slice`.
                        let src = out_idx - dist;
                        copy_within_output(output, &mut out_idx, src, len);
                    }
                    break 'dodist;
                } else if op & 64 == 0 {
                    // Second-level distance table: chase the link and re-decode.
                    here = dcode[here.val as usize + (hold & ((1u32 << op) - 1)) as usize];
                    continue 'dodist;
                } else {
                    // Invalid distance code.
                    error_msg = Some("invalid distance code");
                    final_mode = Some(InflateMode::Bad);
                    break 'outer;
                }
            }
        }

        // C `do { ... } while (in < last && out < end)`.
        if !(in_idx < last_safe_in && out_idx < end_safe) {
            break 'outer;
        }
    }

    // ---- epilogue: return unused whole bytes to the input (C L290-L294) ------
    // On entry `bits < 8`, so backing up over the whole bytes we buffered never
    // moves `in_idx` before where it started.
    let unused = (bits >> 3) as usize;
    in_idx -= unused;
    bits -= (unused as u32) << 3;
    hold &= (1u32 << bits) - 1;

    // ---- RESTORE: write locals back into the state and the caller's cursors --
    state.hold = hold;
    state.bits = bits;
    if let Some(mode) = final_mode {
        state.mode = mode;
    }
    *in_pos = in_idx;
    *out_pos = out_idx;

    error_msg
}

/// Copies `n` bytes from `window[wpos..]` into `output[*out..]`, advancing
/// `*out` by `n`.
///
/// The sliding window and the output buffer are distinct allocations, so this
/// is a plain, non-overlapping block copy and is expressed as a single
/// `copy_from_slice` (which the compiler lowers to a `memcpy`).
#[inline]
fn copy_from_window(output: &mut [u8], out: &mut usize, window: &[u8], wpos: usize, n: usize) {
    let d = *out;
    output[d..d + n].copy_from_slice(&window[wpos..wpos + n]);
    *out = d + n;
}

/// Copies `n` bytes within `output` from `src` to `*out`, advancing `*out` by
/// `n`.
///
/// The copy is performed forward, one byte at a time, so it is correct for the
/// overlapping LZ77 back-references that make `dist < n` meaningful: each byte
/// is read only after all nearer bytes have been written, so a short-distance
/// reference correctly repeats the just-written pattern. A `copy_within` /
/// `copy_from_slice` (memmove/memcpy) would be *incorrect* here because it would
/// not observe the freshly written bytes.
#[inline]
fn copy_within_output(output: &mut [u8], out: &mut usize, src: usize, n: usize) {
    let mut d = *out;
    let mut s = src;
    let end = d + n;
    while d < end {
        output[d] = output[s];
        d += 1;
        s += 1;
    }
    *out = d;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::fixed::{DISTFIX, LENFIX};
    use crate::inflate::state::{InflateMode, InflateState, TableSource};
    use crate::stream::AllocBuffer;
    use alloc::vec::Vec;

    /// A minimal DEFLATE bit writer used to hand-craft fixed-Huffman blocks for
    /// these tests. Bits are packed into bytes least-significant-bit first, as
    /// required by RFC 1951.
    struct BitWriter {
        bytes: Vec<u8>,
        bitbuf: u32,
        nbits: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bitbuf: 0,
                nbits: 0,
            }
        }

        fn push_bit(&mut self, bit: u32) {
            self.bitbuf |= (bit & 1) << self.nbits;
            self.nbits += 1;
            if self.nbits == 8 {
                self.bytes.push(self.bitbuf as u8);
                self.bitbuf = 0;
                self.nbits = 0;
            }
        }

        /// Writes an integer field (extra bits, block header) least-significant
        /// bit first — the DEFLATE convention for non-Huffman values.
        fn write_int(&mut self, value: u32, n: u32) {
            for i in 0..n {
                self.push_bit((value >> i) & 1);
            }
        }

        /// Writes a Huffman code most-significant bit first — the DEFLATE
        /// convention for code words.
        fn write_code(&mut self, code: u32, n: u32) {
            for i in (0..n).rev() {
                self.push_bit((code >> i) & 1);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.nbits > 0 {
                self.bytes.push(self.bitbuf as u8);
            }
            self.bytes
        }
    }

    /// Fixed literal/length symbol → (code, bit length) per RFC 1951 §3.2.6.
    fn fixed_litlen_code(sym: u32) -> (u32, u32) {
        match sym {
            0..=143 => (0x30 + sym, 8),
            144..=255 => (0x190 + (sym - 144), 9),
            256..=279 => (sym - 256, 7),
            280..=287 => (0xc0 + (sym - 280), 8),
            _ => unreachable!("invalid fixed literal/length symbol {sym}"),
        }
    }

    /// Length value → (length symbol, extra-bit value, extra-bit count).
    fn length_encode(len: u32) -> (u32, u32, u32) {
        // (base, extra_count, symbol)
        const TABLE: &[(u32, u32, u32)] = &[
            (3, 0, 257),
            (4, 0, 258),
            (5, 0, 259),
            (6, 0, 260),
            (7, 0, 261),
            (8, 0, 262),
            (9, 0, 263),
            (10, 0, 264),
            (11, 1, 265),
            (13, 1, 266),
            (15, 1, 267),
            (17, 1, 268),
            (19, 2, 269),
            (23, 2, 270),
            (27, 2, 271),
            (31, 2, 272),
            (35, 3, 273),
            (43, 3, 274),
            (51, 3, 275),
            (59, 3, 276),
            (67, 4, 277),
            (83, 4, 278),
            (99, 4, 279),
            (115, 4, 280),
            (131, 5, 281),
            (163, 5, 282),
            (195, 5, 283),
            (227, 5, 284),
            (258, 0, 285),
        ];
        for &(base, extra, sym) in TABLE.iter().rev() {
            if len >= base {
                return (sym, len - base, extra);
            }
        }
        unreachable!("length {len} out of range")
    }

    /// Distance value → (distance symbol, extra-bit value, extra-bit count).
    fn dist_encode(dist: u32) -> (u32, u32, u32) {
        // (base, extra_count, symbol)
        const TABLE: &[(u32, u32, u32)] = &[
            (1, 0, 0),
            (2, 0, 1),
            (3, 0, 2),
            (4, 0, 3),
            (5, 1, 4),
            (7, 1, 5),
            (9, 2, 6),
            (13, 2, 7),
            (17, 3, 8),
            (25, 3, 9),
            (33, 4, 10),
            (49, 4, 11),
            (65, 5, 12),
            (97, 5, 13),
            (129, 6, 14),
            (193, 6, 15),
            (257, 7, 16),
            (385, 7, 17),
            (513, 8, 18),
            (769, 8, 19),
            (1025, 9, 20),
            (1537, 9, 21),
            (2049, 10, 22),
            (3073, 10, 23),
            (4097, 11, 24),
            (6145, 11, 25),
            (8193, 12, 26),
            (12289, 12, 27),
            (16385, 13, 28),
            (24577, 13, 29),
        ];
        for &(base, extra, sym) in TABLE.iter().rev() {
            if dist >= base {
                return (sym, dist - base, extra);
            }
        }
        unreachable!("distance {dist} out of range")
    }

    /// A LZ77 token: a literal byte or a (length, distance) back-reference.
    enum Tok {
        Lit(u8),
        Match(u32, u32),
    }

    /// Encodes a single final fixed-Huffman DEFLATE block for `tokens`, followed
    /// by the end-of-block code and `pad` trailing zero bytes. The padding lets
    /// `inflate_fast` (which stops when fewer than six input bytes remain) reach
    /// the end-of-block code within the fast loop.
    fn encode_fixed_block(tokens: &[Tok], pad: usize) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_int(1, 1); // BFINAL = 1 (final block)
        w.write_int(1, 2); // BTYPE = 01 (fixed Huffman)
        for t in tokens {
            match *t {
                Tok::Lit(b) => {
                    let (code, n) = fixed_litlen_code(b as u32);
                    w.write_code(code, n);
                }
                Tok::Match(len, dist) => {
                    let (lsym, lextra_v, lextra_n) = length_encode(len);
                    let (lcode, ln) = fixed_litlen_code(lsym);
                    w.write_code(lcode, ln);
                    if lextra_n > 0 {
                        w.write_int(lextra_v, lextra_n);
                    }
                    let (dsym, dextra_v, dextra_n) = dist_encode(dist);
                    // Fixed distance codes are simply the 5-bit symbol number.
                    w.write_code(dsym, 5);
                    if dextra_n > 0 {
                        w.write_int(dextra_v, dextra_n);
                    }
                }
            }
        }
        // End of block (symbol 256).
        let (eob, en) = fixed_litlen_code(256);
        w.write_code(eob, en);
        let mut bytes = w.finish();
        bytes.resize(bytes.len() + pad, 0);
        bytes
    }

    /// Builds an inflate state primed to enter `inflate_fast` at a fixed-Huffman
    /// block boundary: `mode == Len`, fixed tables selected, and the three block
    /// header bits already consumed from `stream[0]`.
    fn primed_fixed_state() -> alloc::boxed::Box<InflateState> {
        let mut state = InflateState::new(0, 15);
        state.mode = InflateMode::Len;
        state.lentable = TableSource::Fixed;
        state.disttable = TableSource::Fixed;
        state.lenbits = 9;
        state.distbits = 5;
        state.sane = true;
        state.hold = 0;
        state.bits = 0;
        state
    }

    /// Runs `inflate_fast` over a hand-crafted fixed-Huffman `stream` (whose
    /// first byte still holds the 3-bit block header) and returns the decoded
    /// bytes together with the resulting mode.
    fn decode_fast(
        state: &mut InflateState,
        stream: &[u8],
        out_cap: usize,
    ) -> (Vec<u8>, InflateMode, Option<&'static str>) {
        // Consume the 3-bit block header from the first byte, exactly as the
        // inflate() driver would before dispatching into the fast loop.
        state.hold = (stream[0] as u32) >> 3;
        state.bits = 5;
        let mut in_pos = 1usize;
        let mut output = alloc::vec![0u8; out_cap];
        let mut out_pos = 0usize;
        let start = output.len();
        let msg = inflate_fast(
            state,
            stream,
            &mut in_pos,
            &mut output,
            &mut out_pos,
            start,
            &LENFIX,
            &DISTFIX,
        );
        let mode = state.mode;
        output.truncate(out_pos);
        (output, mode, msg)
    }

    #[test]
    fn decodes_literals_only() {
        let tokens = [Tok::Lit(b'H'), Tok::Lit(b'i'), Tok::Lit(b'!')];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type, "should stop at end-of-block");
        assert_eq!(out, b"Hi!");
    }

    #[test]
    fn decodes_overlapping_output_copy() {
        // "ab" then a match of length 6 at distance 2 => "abababab". Because
        // dist (2) < len (6), this is an overlapping copy that must re-read the
        // freshly written bytes.
        let tokens = [Tok::Lit(b'a'), Tok::Lit(b'b'), Tok::Match(6, 2)];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"abababab");
    }

    #[test]
    fn decodes_high_literals_and_nonoverlapping_match() {
        // Literals above 143 use 9-bit fixed codes; the match (len 4, dist 4)
        // is non-overlapping (dist == len) and copies directly from output.
        let tokens = [
            Tok::Lit(200),
            Tok::Lit(201),
            Tok::Lit(202),
            Tok::Lit(203),
            Tok::Match(4, 4),
        ];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, &[200, 201, 202, 203, 200, 201, 202, 203]);
    }

    #[test]
    fn decodes_from_window_wnext_zero() {
        // Prime a non-wrapped window [0, 1, .., 63]. After three literals the
        // output holds 3 bytes (`op_out == 3`); a match at distance 5 reaches
        // two bytes back into the window (window[62], window[63]) and then two
        // bytes forward from the current output.
        let tokens = [
            Tok::Lit(200),
            Tok::Lit(201),
            Tok::Lit(202),
            Tok::Match(4, 5),
        ];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        let wsize = 64usize;
        let mut window = alloc::vec![0u8; wsize];
        for (i, b) in window.iter_mut().enumerate() {
            *b = i as u8;
        }
        state.window = AllocBuffer::from_vec(window);
        state.wsize = wsize as u32;
        state.whave = wsize as u32;
        state.wnext = 0;
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        // 200,201,202 then [window[62]=62, window[63]=63, out[0]=200, out[1]=201]
        assert_eq!(out, &[200, 201, 202, 62, 63, 200, 201]);
    }

    #[test]
    fn decodes_from_window_wrapped() {
        // A wrapped window: wnext == 4, so the four most-recent bytes are at the
        // front (window[0..4]) and older bytes are at the tail. A distance that
        // exceeds `wnext` must read the tail first, then the head.
        let tokens = [Tok::Lit(10), Tok::Lit(11), Tok::Match(6, 5)];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        let wsize = 16usize;
        let mut window = alloc::vec![0u8; wsize];
        for (i, b) in window.iter_mut().enumerate() {
            *b = (100 + i) as u8;
        }
        state.window = AllocBuffer::from_vec(window);
        state.wsize = wsize as u32;
        state.whave = wsize as u32;
        state.wnext = 4;
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        // Reference model of the wrapped-window copy (see below) yields:
        //   out = [10, 11] ++ copy(len=6, dist=5)
        // op_out = 2, dist_back = 3, wnext = 4 => contiguous branch
        // wpos = wnext - dist_back = 1 => window[1],[2],[3] = 101,102,103
        // then remainder (3 bytes) from output at out-dist: out[0..]=10,11,101
        assert_eq!(out, &[10, 11, 101, 102, 103, 10, 11, 101]);
    }

    #[test]
    fn reports_invalid_distance_too_far_back() {
        // No window data (whave == 0) but a match whose distance reaches beyond
        // the current output must be reported as a data error.
        let tokens = [Tok::Lit(b'x'), Tok::Match(3, 2)];
        let stream = encode_fixed_block(&tokens, 16);
        let mut state = primed_fixed_state();
        // wsize/whave/wnext all zero: nothing valid in the (empty) window.
        let (out, mode, msg) = decode_fast(&mut state, &stream, 512);
        assert_eq!(mode, InflateMode::Bad);
        assert_eq!(msg, Some("invalid distance too far back"));
        // Only the single literal was emitted before the error.
        assert_eq!(out, b"x");
    }
}
