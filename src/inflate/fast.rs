//! The `inflate_fast` hot loop — the performance-critical inner decode path.
//!
//! This module is a faithful, byte-for-byte safe-Rust port of the C file
//! `inffast.c` from zlib 1.3.2.1. It decodes literal, length, and distance
//! codes and writes the resulting literal and match bytes for as long as there
//! is *ample* input and output available. When `inflate()` is supplied with
//! large buffers (for example a 16 KiB input and a 64 KiB output), **more than
//! 95 % of the decompression time is spent in this routine**, so it is written
//! to avoid per-symbol input/output bounds checks (the surrounding driver
//! guarantees enough headroom up front — see *Entry preconditions* below).
//!
//! # The one core `unsafe` site (AAP §0.6.2, §0.7.2)
//!
//! Per the migration plan, `unsafe` in the entire compression/decompression
//! core is confined to **exactly two** zones: `crate::inflate::fast` (this
//! module) and `crate::ffi`. Everything else — the deflate engine, the rest of
//! the inflate engine, the checksum engine — is 100 % safe Rust.
//!
//! Even here, the guiding rule is *prefer safe slicing; reach for `unsafe` only
//! for a proven-bounded hot copy*. Concretely:
//!
//! * window → output copies use the safe, vectorizable
//!   [`slice::copy_from_slice`] (the window and output buffers are disjoint
//!   allocations, so the copy can never overlap);
//! * non-overlapping output → output copies (match distance `>=` remaining
//!   length) use the safe [`slice::copy_within`] (a `memmove`); and
//! * **only** the genuinely overlapping LZ77 run-copy (match distance `<`
//!   remaining length, e.g. the `dist == 1` byte-run) uses a single `unsafe`
//!   raw-pointer forward byte loop — a bulk copy is *impossible* there because
//!   each written byte feeds a later read. That one `unsafe` block is preceded
//!   by a `debug_assert!` encoding its bound and carries a `// SAFETY:` comment
//!   proving the access is in range (AAP §0.7.2).
//!
//! Literal writes, input refills, and the Huffman table look-ups all remain
//! safe (bounds-checked) slice operations: they are provably in range from the
//! entry preconditions and the loop bounds, and they are not the part of the
//! loop that the decompression throughput gate (AAP §0.7.3) is sensitive to.
//!
//! # Byte-identical decoding
//!
//! The decoded output, the **bit-refill cadence** (two bytes pulled when fewer
//! than 15 bits remain; one byte for length-extra; up to two bytes for
//! distance-extra), and every window-copy edge case mirror `inffast.c` exactly.
//! Any deviation would corrupt the output or consume a different number of
//! input bytes, breaking byte-for-byte interop with canonical C zlib.
//!
//! # `no_std`
//!
//! This module references only [`core`] (slices, raw pointers, `debug_assert!`)
//! and the crate's own types; it performs no allocation and uses no `std`
//! facilities, so it compiles unchanged under
//! `--no-default-features --features no-std`.
//!
//! # Relationship to the C signature
//!
//! C `inflate_fast(z_streamp strm, unsigned start)` operates directly on the
//! `z_stream`'s raw `next_in`/`next_out` pointers. This port uses the
//! decoupled *slice + cursor* model described on [`inflate_fast`]: the input
//! and output buffers are passed as slices with `usize` cursors, and the engine
//! state is the safe [`InflateState`]. The one externally observable difference
//! is error reporting: C writes `strm->msg` directly, whereas [`InflateState`]
//! carries no message field, so this function **returns** the message (see
//! [`inflate_fast`]).

// Only the decode-state machine and the resumable state struct are needed in
// the production path: the table-entry type `Code` is inferred for the `here`
// cursor and the `lcode`/`dcode` slices, so it is not named here (the test
// module imports it explicitly). Imports are restricted to the
// `depends_on_files` whitelist (`crate::inflate::state`).
use crate::inflate::state::{InflateMode, InflateState};

// ===========================================================================
// Private copy primitives
// ===========================================================================

/// A read-only view of the inflate sliding window plus the bookkeeping the
/// match-copy logic needs.
///
/// Bundling these five values keeps [`copy_match_window_or_output`] to a small,
/// readable argument list and makes the window-copy logic independently
/// testable. All fields are plain copies of the corresponding
/// [`InflateState`] fields, captured once before the hot loop (they do not
/// change during a single `inflate_fast` call).
#[derive(Clone, Copy)]
struct WindowRef<'a> {
    /// The sliding window bytes (`state.window`). Length is `size` when a
    /// window is in use, or `0` when no window has been allocated yet.
    data: &'a [u8],
    /// Window size in bytes (`state.wsize`), or `0` if unused.
    size: u32,
    /// Number of valid bytes currently in the window (`state.whave`).
    have: u32,
    /// Window write index — where the next output byte will be stored
    /// (`state.wnext`).
    next: u32,
    /// If `true`, reject an "invalid distance too far back" (`state.sane`).
    sane: bool,
}

/// Copy `n` bytes from `window[from..]` to `output[to..]`.
///
/// The window and output buffers are **distinct allocations**, so this copy can
/// never overlap and the safe, vectorizable [`slice::copy_from_slice`] is both
/// correct and fast. No `unsafe` is required or used.
#[inline]
fn copy_from_window(window: &[u8], from: usize, output: &mut [u8], to: usize, n: usize) {
    debug_assert!(
        from + n <= window.len(),
        "inflate_fast: window read out of bounds"
    );
    debug_assert!(
        to + n <= output.len(),
        "inflate_fast: window->output write out of bounds"
    );
    output[to..to + n].copy_from_slice(&window[from..from + n]);
}

/// Copy a `len`-byte match within the output buffer, from `output[from..]` to
/// `output[to..]`, preserving LZ77 run-length semantics.
///
/// The caller guarantees `from < to` (the source always precedes the
/// destination, since `from = to - dist` with `dist >= 1`) and
/// `to + len <= output.len()`.
///
/// Two regimes:
///
/// * **`dist >= len` (non-overlapping):** the source range `[from, from+len)`
///   and the destination range `[to, to+len)` are disjoint, so a `memmove`
///   reproduces the bytes exactly. Implemented with the safe
///   [`slice::copy_within`], which the compiler can lower to a vectorized copy.
/// * **`dist < len` (overlapping run, e.g. `dist == 1`):** each written byte is
///   read again `dist` positions later, so the copy **must** advance one byte
///   at a time — a bulk `memmove` would shift the data instead of repeating it.
///   This is the inflate hot path and the **sole `unsafe` site in the core**
///   (AAP §0.6.2); a raw-pointer forward loop elides the per-byte bounds check.
#[inline]
fn copy_match(output: &mut [u8], from: usize, to: usize, len: usize) {
    debug_assert!(
        from < to,
        "inflate_fast: match source must precede destination"
    );
    debug_assert!(
        to + len <= output.len(),
        "inflate_fast: match write out of bounds"
    );

    // `dist = to - from` is the LZ77 match distance (>= 1 by the `from < to`
    // precondition). It decides whether the copy can overlap.
    let dist = to - from;
    if dist >= len {
        // Disjoint ranges (`from + len <= to`): a memmove equals a memcpy here
        // and stays entirely in safe Rust.
        output.copy_within(from..from + len, to);
    } else {
        // Overlapping run-length copy — the only place the core uses `unsafe`.
        debug_assert!(
            to + len <= output.len(),
            "inflate_fast: overlapping run-copy out of bounds"
        );
        let p = output.as_mut_ptr();
        // SAFETY: `to + len <= output.len()` is asserted above and guaranteed by
        // the caller — on entry `avail_out >= 258` and a single length/distance
        // pair emits at most 258 bytes, so `out_pos + len <= output.len()`.
        // Because `from < to`, every read index in `[from, from+len)` and every
        // write index in `[to, to+len)` is strictly less than `output.len()`,
        // so all accesses stay inside the one `output` allocation. Bytes are
        // moved one at a time through a raw pointer (no overlapping `&`/`&mut`
        // references are ever formed), which preserves the run-length semantics
        // that a bulk copy would destroy.
        unsafe {
            let mut s = from;
            let mut d = to;
            let limit = to + len;
            while d < limit {
                *p.add(d) = *p.add(s);
                s += 1;
                d += 1;
            }
        }
    }
}

/// Reconstruct one length/distance match of length `len` at the current output
/// position, copying from either the sliding window, the already-produced
/// output, or both. Mirrors `inffast.c` lines 167-262.
///
/// `*out_pos` is advanced by exactly `len` bytes on success. Returns `false`
/// (without writing anything) when the match distance reaches further back than
/// the window actually holds and the stream is `sane` — the caller then sets
/// `mode = Bad` and the "invalid distance too far back" message. (The
/// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` branch of C is compiled out by
/// default, so it is intentionally not ported.)
///
/// The three window sub-cases (`next == 0`, `next < window-bytes`, contiguous)
/// reproduce the C control flow exactly; in each, the portion that lies in the
/// window is copied with the safe [`copy_from_window`], and any remainder that
/// "wraps" forward into the freshly written output is finished with
/// [`copy_match`] (which handles the overlapping `dist < len` case).
#[inline]
fn copy_match_window_or_output(
    output: &mut [u8],
    out_pos: &mut usize,
    win: WindowRef<'_>,
    dist: u32,
    mut len: u32,
) -> bool {
    // C `op = (unsigned)(out - beg)` — bytes available directly from the output
    // produced so far this `inflate()` call (the output slice begins at `beg`).
    let op2 = *out_pos as u32;

    if dist > op2 {
        // The match reaches back past the start of this call's output, so the
        // earliest bytes come from the sliding window. `wop` is how far back
        // into the window we must reach (C reuses `op` for this).
        let wop = dist - op2;
        if wop > win.have && win.sane {
            // Distance points before any valid window data: data error.
            return false;
        }

        let wsize = win.size;
        let wnext = win.next;
        let dist_us = dist as usize;

        if wnext == 0 {
            // Very common case: window write index at 0, so the most recent
            // `wop` bytes sit at the tail of the window.
            let from_w = (wsize - wop) as usize;
            if wop < len {
                // Only part of the match is in the window; the rest wraps into
                // the output we are about to write.
                len -= wop;
                copy_from_window(win.data, from_w, output, *out_pos, wop as usize);
                *out_pos += wop as usize;
                let from0 = *out_pos - dist_us;
                copy_match(output, from0, *out_pos, len as usize);
                *out_pos += len as usize;
            } else {
                // The entire remaining match lies within the window tail.
                copy_from_window(win.data, from_w, output, *out_pos, len as usize);
                *out_pos += len as usize;
            }
        } else if wnext < wop {
            // The needed bytes straddle the window wrap: an older segment at the
            // end of the window followed by the newest bytes at the start.
            let from_w = (wsize + wnext - wop) as usize;
            let seg1 = wop - wnext; // bytes available from the end of the window
            if seg1 < len {
                len -= seg1;
                copy_from_window(win.data, from_w, output, *out_pos, seg1 as usize);
                *out_pos += seg1 as usize;
                if wnext < len {
                    // Some bytes also come from the start of the window.
                    let seg2 = wnext;
                    len -= seg2;
                    copy_from_window(win.data, 0, output, *out_pos, seg2 as usize);
                    *out_pos += seg2 as usize;
                    let from0 = *out_pos - dist_us;
                    copy_match(output, from0, *out_pos, len as usize);
                    *out_pos += len as usize;
                } else {
                    // Remainder fits entirely in the start-of-window segment.
                    copy_from_window(win.data, 0, output, *out_pos, len as usize);
                    *out_pos += len as usize;
                }
            } else {
                // The whole remaining match fits in the end-of-window segment.
                copy_from_window(win.data, from_w, output, *out_pos, len as usize);
                *out_pos += len as usize;
            }
        } else {
            // Contiguous in the window (no wrap): the bytes sit just before the
            // write index.
            let from_w = (wnext - wop) as usize;
            if wop < len {
                len -= wop;
                copy_from_window(win.data, from_w, output, *out_pos, wop as usize);
                *out_pos += wop as usize;
                let from0 = *out_pos - dist_us;
                copy_match(output, from0, *out_pos, len as usize);
                *out_pos += len as usize;
            } else {
                copy_from_window(win.data, from_w, output, *out_pos, len as usize);
                *out_pos += len as usize;
            }
        }
    } else {
        // Whole match is within the output already written this call — copy it
        // directly (overlapping when `dist < len`, e.g. an LZ77 byte-run).
        let from0 = *out_pos - dist as usize;
        copy_match(output, from0, *out_pos, len as usize);
        *out_pos += len as usize;
    }

    true
}

// ===========================================================================
// The hot loop
// ===========================================================================

/// Decode literals and length/distance matches until end-of-block, or until
/// fewer than 6 input bytes / 258 output bytes remain. A faithful port of C
/// `inflate_fast` (`inffast.c`).
///
/// # Slice + cursor model
///
/// C operates on the `z_stream`'s raw `next_in`/`next_out` pointers plus a
/// `start` count. This port decouples from `ZStream` internals:
///
/// * `input`  — the input bytes available this call (the slice starting at
///   `next_in`); `*in_pos` is the consumed cursor (C's `in - next_in`).
/// * `output` — the output buffer **beginning at this `inflate()` call's
///   initial `next_out`** (C's `beg`); `*out_pos` is the current write offset,
///   so C's `out - beg` is exactly `*out_pos`, and C's `end` is
///   `output.len() - 257`.
/// * `state`  — the resumable inflate state (bit accumulator, root tables,
///   sliding window, mode).
///
/// Because `output` is referenced to `beg`, the C `start`/`beg` bookkeeping
/// collapses to plain offsets and no separate `start` parameter is needed; the
/// offset semantics are identical to C.
///
/// # Entry preconditions (assert-checked)
///
/// The caller (the `inflate` driver / `inflate_back`) must guarantee, exactly
/// as C documents (`inffast.c` lines 23-29):
///
/// * `state.mode == Len`,
/// * `input.len() >= 6` (`avail_in >= 6` — a length/distance pair needs at most
///   48 bits = 6 bytes, so the loop never has to check input mid-symbol),
/// * `output.len() - *out_pos >= 258` (`avail_out >= 258` — a pair emits at most
///   258 bytes, so the loop never has to check output mid-symbol),
/// * `state.bits < 8` (so returning the unused whole bytes can never push the
///   input cursor back before where this call started).
///
/// # Return value
///
/// C records errors by writing `strm->msg` and setting `state->mode = BAD`.
/// [`InflateState`] carries no message field, so this function instead **returns
/// the message**:
///
/// * `None` — normal exit; `state.mode` is [`Len`](InflateMode::Len) (ran out of
///   input or output) or [`Type`](InflateMode::Type) (hit end-of-block).
/// * `Some(msg)` — a data error; `state.mode` is [`Bad`](InflateMode::Bad) and
///   `msg` is the exact C string. The caller stores it via `ZStream::set_msg`.
///
/// This function never returns a `Z_*` code; it only advances the cursors and
/// sets `state.mode`. The caller recomputes `avail_in`/`avail_out` from the
/// updated cursors and interprets the resulting mode.
pub(crate) fn inflate_fast(
    input: &[u8],
    in_pos: &mut usize,
    output: &mut [u8],
    out_pos: &mut usize,
    state: &mut InflateState,
) -> Option<&'static str> {
    // --- Entry preconditions (C inffast.c:23-29) --------------------------
    debug_assert!(
        state.mode == InflateMode::Len,
        "inflate_fast: entry requires mode == Len"
    );
    debug_assert!(
        input.len() >= 6,
        "inflate_fast: entry requires avail_in >= 6"
    );
    debug_assert!(
        output.len() - *out_pos >= 258,
        "inflate_fast: entry requires avail_out >= 258"
    );
    debug_assert!(state.bits < 8, "inflate_fast: entry requires bits < 8");

    // =====================================================================
    // Phase A — load locals, masks, table/window views
    // =====================================================================
    // The bit accumulator and counter are pulled into locals so the hot loop
    // touches registers, not memory; they are written back in Phase F.
    let mut hold: u32 = state.hold;
    let mut bits: u32 = state.bits;

    // First-level index masks (C: `lmask`/`dmask`). `lenbits`/`distbits` are the
    // root-table widths chosen by `inflate_table`; they are small (<= 9), so the
    // `1 << n` never overflows `u32`.
    let lmask: u32 = (1u32 << state.lenbits) - 1;
    let dmask: u32 = (1u32 << state.distbits) - 1;

    // Sliding-window bookkeeping (read-only for the duration of the call).
    let wsize = state.wsize;
    let whave = state.whave;
    let wnext = state.wnext;

    // Loop bounds (C: `last`/`end`). `last = next_in + (avail_in - 5)` becomes an
    // offset; `end = next_out + (avail_out - 257)` becomes `output.len() - 257`.
    // Both subtractions are safe: the preconditions give `input.len() >= 6` and
    // `output.len() >= *out_pos + 258 >= 258`.
    let last = input.len() - 5;
    let end = output.len() - 257;

    // Root code tables as slices (C: `lcode`/`dcode`). The `lencode`/`distcode`
    // fields are offsets into the single shared `codes` arena. Indexing these
    // with masked values stays in-bounds, so it is left as safe slice indexing
    // (it is not the throughput-critical part of the loop).
    let lencode = state.lencode;
    let distcode = state.distcode;
    let lcode = &state.codes[lencode..];
    let dcode = &state.codes[distcode..];

    // A read-only view of the window for the match-copy helper. Borrows only
    // `state.window`; the write-backs in Phase F touch disjoint fields.
    let win = WindowRef {
        data: &state.window,
        size: wsize,
        have: whave,
        next: wnext,
        sane: state.sane,
    };

    // Where the loop leaves the machine. Defaults to `Len` — the "ran out of
    // input or output" exit taken when the do-while guard fails.
    let mut next_mode = InflateMode::Len;
    // Error message to hand back (mirrors the C `strm->msg` writes). `Some`
    // exactly when `next_mode` becomes `Bad`.
    let mut err_msg: Option<&'static str> = None;

    // =====================================================================
    // Phase B — main decode loop: `do { … } while (in < last && out < end)`
    // =====================================================================
    // Modeled as `loop { body; if !guard { break } }` to preserve the C
    // do-while semantics (the body executes once before the guard is tested).
    'outer: loop {
        // --- Refill: pull two bytes (LSB-first) when fewer than 15 bits ---
        // remain, matching DEFLATE bit order and the C refill cadence exactly.
        if bits < 15 {
            hold |= (input[*in_pos] as u32) << bits;
            *in_pos += 1;
            bits += 8;
            hold |= (input[*in_pos] as u32) << bits;
            *in_pos += 1;
            bits += 8;
        }

        // First-level length/literal lookup.
        let mut here = lcode[(hold & lmask) as usize];

        // --- `dolen`: process the (possibly multi-level) length/literal code -
        // The labeled loop models C's `goto dolen` for second-level codes via
        // `continue 'dolen`.
        'dolen: loop {
            // Consume the bits this table entry occupies.
            let mut op = here.bits as u32;
            hold >>= op;
            bits -= op;
            op = here.op as u32;

            if op == 0 {
                // ---- Literal byte ----
                output[*out_pos] = here.val as u8;
                *out_pos += 1;
                break 'dolen; // fall through to the do-while tail
            } else if op & 16 != 0 {
                // ---- Length base ----
                let mut len = here.val as u32;
                op &= 15; // number of length-extra bits
                if op != 0 {
                    if bits < op {
                        hold |= (input[*in_pos] as u32) << bits;
                        *in_pos += 1;
                        bits += 8;
                    }
                    len += hold & ((1u32 << op) - 1);
                    hold >>= op;
                    bits -= op;
                }

                // Refill before the distance code (same two-byte cadence).
                if bits < 15 {
                    hold |= (input[*in_pos] as u32) << bits;
                    *in_pos += 1;
                    bits += 8;
                    hold |= (input[*in_pos] as u32) << bits;
                    *in_pos += 1;
                    bits += 8;
                }

                // First-level distance lookup.
                here = dcode[(hold & dmask) as usize];

                // --- `dodist`: process the distance code (also multi-level) --
                'dodist: loop {
                    let mut op = here.bits as u32;
                    hold >>= op;
                    bits -= op;
                    op = here.op as u32;

                    if op & 16 != 0 {
                        // ---- Distance base ----
                        let mut dist = here.val as u32;
                        op &= 15; // number of distance-extra bits
                        if bits < op {
                            hold |= (input[*in_pos] as u32) << bits;
                            *in_pos += 1;
                            bits += 8;
                            if bits < op {
                                hold |= (input[*in_pos] as u32) << bits;
                                *in_pos += 1;
                                bits += 8;
                            }
                        }
                        dist += hold & ((1u32 << op) - 1);
                        // NOTE: the `INFLATE_STRICT` `dist > dmax` check is not
                        // compiled in the default zlib build, so it is omitted.
                        hold >>= op;
                        bits -= op;

                        // Reconstruct the match. On a "too far back" error the
                        // helper returns false without writing anything.
                        if !copy_match_window_or_output(output, out_pos, win, dist, len) {
                            err_msg = Some("invalid distance too far back");
                            next_mode = InflateMode::Bad;
                            break 'outer;
                        }
                        break 'dolen; // match done; fall to the do-while tail
                    } else if op & 64 == 0 {
                        // ---- Second-level distance code (C: goto dodist) ----
                        here = dcode[(here.val as u32 + (hold & ((1u32 << op) - 1))) as usize];
                        continue 'dodist;
                    } else {
                        // ---- Invalid distance code ----
                        err_msg = Some("invalid distance code");
                        next_mode = InflateMode::Bad;
                        break 'outer;
                    }
                }
            } else if op & 64 == 0 {
                // ---- Second-level length code (C: goto dolen) ----
                here = lcode[(here.val as u32 + (hold & ((1u32 << op) - 1))) as usize];
                continue 'dolen;
            } else if op & 32 != 0 {
                // ---- End of block ----
                next_mode = InflateMode::Type;
                break 'outer;
            } else {
                // ---- Invalid literal/length code ----
                err_msg = Some("invalid literal/length code");
                next_mode = InflateMode::Bad;
                break 'outer;
            }
        }

        // --- do-while guard: keep going only while there is ample headroom ---
        if !(*in_pos < last && *out_pos < end) {
            break 'outer;
        }
    }

    // =====================================================================
    // Phase F — return unused whole bytes, then write back the bit state
    // =====================================================================
    // Hand back the whole bytes still sitting in the accumulator (C lines
    // 290-294). On entry `bits < 8`, so at most one extra byte was ever loaded
    // beyond what was consumed, and `*in_pos` never moves before this call's
    // starting position.
    let unused = bits >> 3; // whole bytes still buffered
    *in_pos -= unused as usize;
    bits -= unused << 3;
    hold &= (1u32 << bits) - 1; // keep only the still-buffered fractional bits

    // Persist the resumable bit state. The cursors `*in_pos`/`*out_pos` are
    // already up to date; the caller recomputes `avail_in = input.len() -
    // *in_pos` and `avail_out = output.len() - *out_pos`. These equal C's
    // `in < last ? 5 + (last - in) : 5 - (in - last)` and the analogous
    // `out`/`end` formula: with `last = input.len() - 5`, the `in < last`
    // branch gives `5 + (input.len() - 5 - *in_pos) = input.len() - *in_pos`,
    // and the `in >= last` branch gives `5 - (*in_pos - (input.len() - 5)) =
    // input.len() - *in_pos` as well — so the cursor difference is exactly
    // `avail_in` in both branches (and likewise for output).
    state.hold = hold;
    state.bits = bits;
    state.mode = next_mode;

    err_msg
}

// ===========================================================================
// Tests
// ===========================================================================
//
// The test module is gated on `feature = "std"` (like the alloc-dependent
// tests in `crate::error`) so that `cargo test --no-default-features` still
// compiles: every test here builds heap buffers and uses the `std` prelude.
// The production code above is verified `no_std`-clean separately by
// `cargo build --no-default-features`.
//
// Strategy. `inflate_fast` is only the *inner* decode loop — it assumes the
// block header has already been parsed and the Huffman tables installed. The
// tests therefore drive it directly:
//
//  * A small **fixed-Huffman encoder** (RFC 1951 §3.2.6) turns a token list
//    (literals + length/distance matches) into a raw-DEFLATE bit stream. Its
//    correctness is cross-checked independently by `flate2`
//    (`DeflateDecoder`), so the encoder is a trustworthy fixture.
//  * A **linear-history reference decoder** reconstructs the expected output
//    byte-by-byte from the (optionally pre-populated) window plus the output
//    produced so far — the plain semantic definition of an LZ77 match, with no
//    case analysis — and is used to check the window-copy paths that `flate2`
//    cannot reproduce (they need a pre-seeded window).
//  * `inflate_table` (the real table builder) plus a canonical-code assigner
//    build genuine two-level tables to exercise the `continue 'dolen` /
//    `continue 'dodist` sub-table traversal and the invalid-code paths.
#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::inflate::state::{InflateMode, InflateState};
    use crate::inflate::tables::{Code, CodeType, inflate_table};

    // -- RFC 1951 length/distance base + extra-bit tables (encoder side) ------
    // These mirror `tables.rs` `LBASE`/`LEXT`/`DBASE`/`DEXT` but expose the
    // *extra-bit counts* directly (the decoder tables fold the count into an
    // `op` nibble). Indexed by code-symbol offset.
    const T_LBASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const T_LEXT: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const T_DBASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const T_DEXT: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    /// Packs bits into bytes **LSB-first**, the DEFLATE bit order (RFC 1951
    /// §3.1.1). Huffman codes are written MSB-first via [`Self::write_code`];
    /// everything else (extra bits, the block header) is written LSB-first via
    /// [`Self::write_lsb`].
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

        /// Append the low `count` bits of `value`, least-significant bit first.
        fn write_lsb(&mut self, value: u32, count: u32) {
            debug_assert!(count <= 24);
            self.acc |= (value & ((1u32 << count) - 1)) << self.nbits;
            self.nbits += count;
            while self.nbits >= 8 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.nbits -= 8;
            }
        }

        /// Append a Huffman code of `len` bits, **most-significant bit first**
        /// (the code is transmitted MSB→LSB but still packed into the stream
        /// LSB-first overall, per RFC 1951).
        fn write_code(&mut self, code: u32, len: u32) {
            let mut i = len;
            while i > 0 {
                i -= 1;
                self.write_lsb((code >> i) & 1, 1);
            }
        }

        /// Flush the partial final byte (zero-padded) and return the stream.
        fn finish(mut self) -> Vec<u8> {
            if self.nbits > 0 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc = 0;
                self.nbits = 0;
            }
            self.bytes
        }
    }

    /// A DEFLATE token: a literal byte or a length/distance back-reference.
    #[derive(Clone, Copy, Debug)]
    enum Token {
        Lit(u8),
        Match { len: u16, dist: u16 },
    }

    /// Fixed-Huffman literal/length code (RFC 1951 §3.2.6): returns
    /// `(code, bit_length)` with the code in MSB-first form.
    fn fixed_litlen(sym: u16) -> (u32, u32) {
        match sym {
            0..=143 => (0x30 + sym as u32, 8),
            144..=255 => (0x190 + (sym as u32 - 144), 9),
            256..=279 => (sym as u32 - 256, 7),
            280..=287 => (0xC0 + (sym as u32 - 280), 8),
            _ => unreachable!("invalid literal/length symbol {sym}"),
        }
    }

    /// Length value (3..=258) → fixed length symbol index (0..=28, i.e. code
    /// 257..=285). Mirrors zlib's choice of code 285 for the exact length 258.
    fn length_index(len: u16) -> usize {
        let mut i = 28;
        while i > 0 && len < T_LBASE[i] {
            i -= 1;
        }
        i
    }

    /// Distance value (1..=32768) → distance symbol (0..=29).
    fn distance_index(dist: u16) -> usize {
        let mut i = 29;
        while i > 0 && dist < T_DBASE[i] {
            i -= 1;
        }
        i
    }

    /// Encode `tokens` plus the end-of-block symbol as a single fixed-Huffman
    /// DEFLATE block. When `with_header` is set, the 3-bit block header
    /// (`BFINAL=1`, `BTYPE=01`) is prepended — needed for the `flate2` oracle,
    /// omitted for the header-less stream fed straight to `inflate_fast`.
    fn encode_fixed(tokens: &[Token], with_header: bool) -> Vec<u8> {
        let mut bw = BitWriter::new();
        if with_header {
            bw.write_lsb(1, 1); // BFINAL = 1 (last block)
            bw.write_lsb(0b01, 2); // BTYPE  = 01 (fixed Huffman)
        }
        for tok in tokens {
            match *tok {
                Token::Lit(b) => {
                    let (code, n) = fixed_litlen(b as u16);
                    bw.write_code(code, n);
                }
                Token::Match { len, dist } => {
                    let li = length_index(len);
                    let (code, n) = fixed_litlen(257 + li as u16);
                    bw.write_code(code, n);
                    if T_LEXT[li] > 0 {
                        bw.write_lsb((len - T_LBASE[li]) as u32, T_LEXT[li] as u32);
                    }
                    let di = distance_index(dist);
                    // Fixed distance codes are 5 bits, value == symbol.
                    bw.write_code(di as u32, 5);
                    if T_DEXT[di] > 0 {
                        bw.write_lsb((dist - T_DBASE[di]) as u32, T_DEXT[di] as u32);
                    }
                }
            }
        }
        let (code, n) = fixed_litlen(256); // end-of-block
        bw.write_code(code, n);
        bw.finish()
    }

    /// Assign canonical Huffman codes (RFC 1951 §3.2.2) for the given per-symbol
    /// code `lens`, returning `(code, len)` per symbol (MSB-first). This is the
    /// *encoder* counterpart to `inflate_table`'s decoder: feeding both the same
    /// `lens` lets a round trip validate the two-level traversal in
    /// `inflate_fast`.
    fn canonical_codes(lens: &[u16]) -> Vec<(u32, u32)> {
        let maxbits = *lens.iter().max().unwrap_or(&0) as usize;
        let mut bl_count = vec![0u32; maxbits + 1];
        for &l in lens {
            if l > 0 {
                bl_count[l as usize] += 1;
            }
        }
        let mut next_code = vec![0u32; maxbits + 2];
        let mut code = 0u32;
        for bits in 1..=maxbits {
            code = (code + bl_count[bits - 1]) << 1;
            next_code[bits] = code;
        }
        let mut out = vec![(0u32, 0u32); lens.len()];
        for (sym, &l) in lens.iter().enumerate() {
            if l > 0 {
                out[sym] = (next_code[l as usize], l as u32);
                next_code[l as usize] += 1;
            }
        }
        out
    }

    /// Build a real decode table from per-symbol code `lens` using the
    /// production [`inflate_table`]. `root_bits` is the requested root width
    /// (kept small to force two-level sub-tables when `max_len > root_bits`).
    /// Returns the populated `codes` arena and the actual root width chosen.
    fn build_table(type_: CodeType, lens: &[u16], root_bits: u32) -> (Vec<Code>, u32) {
        let mut codes = vec![Code::default(); crate::inflate::tables::ENOUGH];
        let mut next = 0usize;
        let mut bits = root_bits;
        let mut work = vec![0u16; lens.len().max(1)];
        let ret = inflate_table(
            type_,
            lens,
            lens.len(),
            &mut codes,
            &mut next,
            &mut bits,
            &mut work,
        );
        assert_eq!(ret, 0, "inflate_table returned {ret} (expected 0)");
        (codes, bits)
    }

    /// Construct an [`InflateState`] poised for an `inflate_fast` call (mode
    /// `Len`, empty bit accumulator), with the given root tables and window.
    /// The `codes` arena holds `lcode` at `lencode` and `dcode` at `distcode`.
    #[allow(clippy::too_many_arguments)]
    fn make_state(
        codes: Vec<Code>,
        lencode: usize,
        lenbits: u32,
        distcode: usize,
        distbits: u32,
        window: Vec<u8>,
        wsize: u32,
        whave: u32,
        wnext: u32,
    ) -> InflateState {
        let mut st = InflateState::new();
        st.codes = codes.into_boxed_slice();
        st.lencode = lencode;
        st.lenbits = lenbits;
        st.distcode = distcode;
        st.distbits = distbits;
        st.window = window;
        st.wsize = wsize;
        st.whave = whave;
        st.wnext = wnext;
        st.sane = true;
        st.mode = InflateMode::Len;
        st.hold = 0;
        st.bits = 0;
        st
    }

    /// RFC 1951 §3.2.6 code lengths for the **fixed literal/length** alphabet
    /// (288 symbols): 0–143 → 8 bits, 144–255 → 9, 256–279 → 7, 280–287 → 8.
    /// This is the exact length vector zlib's `fixedtables()` feeds to
    /// `inflate_table` to materialise the fixed `LENFIX` table.
    fn fixed_lit_len_lengths() -> Vec<u16> {
        let mut lens = vec![0u16; 288];
        lens[0..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..288].fill(8);
        lens
    }

    /// RFC 1951 §3.2.6 code lengths for the **fixed distance** alphabet:
    /// 32 symbols, each 5 bits — the length vector zlib feeds to
    /// `inflate_table` to materialise the fixed `DISTFIX` table.
    fn fixed_dist_lengths() -> Vec<u16> {
        vec![5u16; 32]
    }

    /// Build the fixed literal/length decode table through the production
    /// [`inflate_table`] (root width 9), reproducing zlib's `fixedtables()`
    /// without importing the pre-baked `inffixed.h` arrays. The fixed code's
    /// maximum length is 9, so the result is exactly `2^9 = 512` root entries
    /// with no sub-tables; the first 512 codes are returned. This keeps the
    /// fixtures dependent only on the whitelisted `tables.rs`.
    fn build_fixed_litlen() -> Vec<Code> {
        let lens = fixed_lit_len_lengths();
        let (codes, bits) = build_table(CodeType::Lens, &lens, 9);
        assert_eq!(bits, 9, "fixed litlen root width must be 9");
        codes[..512].to_vec()
    }

    /// Build the fixed distance decode table through [`inflate_table`] (root
    /// width 5), reproducing zlib's `fixedtables()`. The fixed distance code is
    /// 32 symbols of length 5, so the result is exactly `2^5 = 32` root entries
    /// with no sub-tables.
    fn build_fixed_dist() -> Vec<Code> {
        let lens = fixed_dist_lengths();
        let (codes, bits) = build_table(CodeType::Dists, &lens, 5);
        assert_eq!(bits, 5, "fixed distance root width must be 5");
        codes[..32].to_vec()
    }

    /// Build a state carrying the RFC 1951 **fixed** Huffman tables, exactly as
    /// the `inflate` driver installs them: the fixed literal/length table
    /// (512 entries, 9-bit root) at offset 0 and the fixed distance table
    /// (32 entries, 5-bit root) at offset 512. Both tables are materialised via
    /// the production `inflate_table` (see `build_fixed_litlen` /
    /// `build_fixed_dist`), so this fixture depends only on `tables.rs`.
    fn make_fixed_state(window: Vec<u8>, wsize: u32, whave: u32, wnext: u32) -> InflateState {
        let mut codes = vec![Code::default(); 512 + 32];
        codes[..512].copy_from_slice(&build_fixed_litlen());
        codes[512..512 + 32].copy_from_slice(&build_fixed_dist());
        make_state(codes, 0, 9, 512, 5, window, wsize, whave, wnext)
    }

    /// Drive `inflate_fast` over a **header-less** symbol stream. The input is
    /// padded so the entry preconditions hold (`avail_in >= 6`) and so the loop
    /// keeps enough look-ahead to process the end-of-block before its do-while
    /// guard trips; the output buffer is over-sized for the same reason
    /// (`end = len - 257` must stay beyond the decoded length). Returns the
    /// decoded bytes, the final mode, and any error message.
    fn run_fast(
        mut state: InflateState,
        stream: &[u8],
        expected_len: usize,
    ) -> (Vec<u8>, InflateMode, Option<&'static str>) {
        let mut input = stream.to_vec();
        // 16 bytes of trailing zero padding: guarantees avail_in >= 6 and keeps
        // `in_pos` comfortably below `last = input.len() - 5` until EOB.
        input.resize(stream.len() + 16, 0);
        // Output sized so `end = output.len() - 257` exceeds the decoded length,
        // and always >= 258 for the entry precondition.
        let mut output = vec![0u8; expected_len + 512];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        let msg = inflate_fast(&input, &mut in_pos, &mut output, &mut out_pos, &mut state);
        output.truncate(out_pos);
        (output, state.mode, msg)
    }

    /// Linearize a ring-buffer window into chronological (oldest→newest) order:
    /// the `whave` valid bytes end just before `wnext`. This is the history that
    /// `inflate_fast`'s window-copy arithmetic indexes into.
    fn linear_window(window: &[u8], wsize: u32, whave: u32, wnext: u32) -> Vec<u8> {
        let wsize = wsize as usize;
        let whave = whave as usize;
        let wnext = wnext as usize;
        let mut out = Vec::with_capacity(whave);
        for j in 0..whave {
            out.push(window[(wnext + wsize - whave + j) % wsize]);
        }
        out
    }

    /// Independent, obviously-correct LZ77 reference decoder. Reconstructs the
    /// output byte-by-byte from `initial_history` (the linearized window) plus
    /// the token list — overlap falls out naturally because the history vector
    /// grows as bytes are produced. Returns only the bytes produced "this call"
    /// (i.e. excluding the pre-existing history), matching `inflate_fast`'s
    /// output. Used as the oracle for the window-copy paths.
    fn reference_decode(initial_history: &[u8], tokens: &[Token]) -> Vec<u8> {
        let mut hist = initial_history.to_vec();
        let base = hist.len();
        for tok in tokens {
            match *tok {
                Token::Lit(b) => hist.push(b),
                Token::Match { len, dist } => {
                    let start = hist.len() - dist as usize;
                    for k in 0..len as usize {
                        let byte = hist[start + k];
                        hist.push(byte);
                    }
                }
            }
        }
        hist[base..].to_vec()
    }

    /// Decode a *with-header* raw-DEFLATE stream with `flate2` (independent C/Rust
    /// reference). Confirms the fixed-Huffman encoder emits RFC-1951-valid bytes.
    fn flate2_decode(stream_with_header: &[u8]) -> Vec<u8> {
        use flate2::read::DeflateDecoder;
        use std::io::Read;
        let mut decoder = DeflateDecoder::new(stream_with_header);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("flate2 failed to decode encoder output");
        out
    }

    // -- Fixed-Huffman tests (oracle: flate2 + linear-history reference) ------

    #[test]
    fn fixed_literals_round_trip() {
        // A spread of bytes covering both fixed-Huffman literal ranges:
        // 0..=143 (8-bit codes) and 144..=255 (9-bit codes).
        let data: Vec<u8> = vec![0, 1, 65, 66, 67, 127, 143, 144, 200, 255, 0, 255, 42];
        let tokens: Vec<Token> = data.iter().map(|&b| Token::Lit(b)).collect();

        let expected = reference_decode(&[], &tokens);
        assert_eq!(expected, data);
        // Encoder sanity: an independent decoder must agree.
        assert_eq!(flate2_decode(&encode_fixed(&tokens, true)), data);

        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            data.len(),
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type, "EOB must leave mode == Type");
        assert_eq!(out, data);
    }

    #[test]
    fn fixed_match_direct_output_non_overlapping() {
        // "ABCD" then a length-4 / distance-4 match → "ABCDABCD". dist == len,
        // so the output copy is non-overlapping (the safe `copy_within` path).
        let tokens = [
            Token::Lit(b'A'),
            Token::Lit(b'B'),
            Token::Lit(b'C'),
            Token::Lit(b'D'),
            Token::Match { len: 4, dist: 4 },
        ];
        let expected = reference_decode(&[], &tokens);
        assert_eq!(expected, b"ABCDABCD");
        assert_eq!(flate2_decode(&encode_fixed(&tokens, true)), expected);

        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            expected.len(),
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn fixed_match_overlap_dist_one_run() {
        // The classic LZ77 byte-run: a single 'X' then a distance-1 match. This
        // is the genuinely OVERLAPPING copy that exercises the sole `unsafe`
        // block (a non-overlapping bulk copy would be wrong here).
        let tokens = [Token::Lit(b'X'), Token::Match { len: 200, dist: 1 }];
        let expected = reference_decode(&[], &tokens);
        assert_eq!(expected.len(), 201);
        assert!(expected.iter().all(|&b| b == b'X'));
        assert_eq!(flate2_decode(&encode_fixed(&tokens, true)), expected);

        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            expected.len(),
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn fixed_match_overlap_dist_three() {
        // Distance 3, length 10: another overlapping run (dist < len) producing
        // the repeating pattern "123123...".
        let tokens = [
            Token::Lit(1),
            Token::Lit(2),
            Token::Lit(3),
            Token::Match { len: 10, dist: 3 },
        ];
        let expected = reference_decode(&[], &tokens);
        assert_eq!(expected, vec![1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3, 1]);
        assert_eq!(flate2_decode(&encode_fixed(&tokens, true)), expected);

        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            expected.len(),
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn fixed_max_length_match() {
        // The maximum match length (258, fixed code 285, 0 extra bits) with a
        // moderate distance, validating the length-258 special case end-to-end.
        let mut tokens = vec![Token::Lit(b'Q'), Token::Lit(b'R')];
        tokens.push(Token::Match { len: 258, dist: 2 });
        let expected = reference_decode(&[], &tokens);
        assert_eq!(expected.len(), 2 + 258);
        assert_eq!(flate2_decode(&encode_fixed(&tokens, true)), expected);

        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            expected.len(),
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    // -- Window-copy tests (oracle: linear-history reference) -----------------
    //
    // These pre-seed the sliding window (simulating prior `inflate()` output)
    // and start the call with a match whose distance reaches back into it
    // (`dist > out_pos == 0`). The three sub-cases are selected by `wnext`.

    /// Fill a window of `wsize` bytes with a recognizable, non-repeating pattern.
    fn patterned_window(wsize: usize) -> Vec<u8> {
        (0..wsize).map(|i| (i as u32 * 37 + 11) as u8).collect()
    }

    #[test]
    fn window_copy_wnext_zero() {
        // Very common case: write index at 0, so the most-recent bytes are at
        // the tail of the window.
        let wsize = 64u32;
        let window = patterned_window(wsize as usize);
        let (whave, wnext) = (wsize, 0u32);
        // dist 5 reaches the last 5 window bytes; len 12 spills the rest into
        // freshly written output (exercises "some from window, rest from output").
        let tokens = [Token::Match { len: 12, dist: 5 }];

        let hist = linear_window(&window, wsize, whave, wnext);
        let expected = reference_decode(&hist, &tokens);

        let state = make_fixed_state(window, wsize, whave, wnext);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn window_copy_wnext_zero_entirely_in_window() {
        // Whole match satisfied from the window tail (len <= dist), so nothing
        // spills into output.
        let wsize = 64u32;
        let window = patterned_window(wsize as usize);
        let tokens = [Token::Match { len: 6, dist: 10 }];
        let hist = linear_window(&window, wsize, wsize, 0);
        let expected = reference_decode(&hist, &tokens);

        let state = make_fixed_state(window, wsize, wsize, 0);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn window_copy_wrap_around() {
        // wnext < wop: the needed bytes straddle the ring-buffer wrap (some at
        // the end of the window, some at the start), then spill into output.
        let wsize = 64u32;
        let window = patterned_window(wsize as usize);
        let (whave, wnext) = (wsize, 3u32);
        // dist 10 > wnext 3, and len 20 forces end-of-window + start-of-window +
        // output segments all to run.
        let tokens = [Token::Match { len: 20, dist: 10 }];
        let hist = linear_window(&window, wsize, whave, wnext);
        let expected = reference_decode(&hist, &tokens);

        let state = make_fixed_state(window, wsize, whave, wnext);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn window_copy_contiguous() {
        // wnext >= wop: the bytes sit contiguously just before the write index.
        let wsize = 64u32;
        let window = patterned_window(wsize as usize);
        let (whave, wnext) = (wsize, 40u32);
        let tokens = [Token::Match { len: 9, dist: 5 }];
        let hist = linear_window(&window, wsize, whave, wnext);
        let expected = reference_decode(&hist, &tokens);

        let state = make_fixed_state(window, wsize, whave, wnext);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn window_then_literals_and_output_match() {
        // Mixed program: a window-referencing match, some literals, then a
        // second match that copies from this call's own output (Phase E).
        let wsize = 128u32;
        let window = patterned_window(wsize as usize);
        let (whave, wnext) = (wsize, 0u32);
        let tokens = [
            Token::Match { len: 8, dist: 20 }, // from window
            Token::Lit(0xAA),
            Token::Lit(0xBB),
            Token::Match { len: 5, dist: 3 }, // from recent output (overlap)
        ];
        let hist = linear_window(&window, wsize, whave, wnext);
        let expected = reference_decode(&hist, &tokens);

        let state = make_fixed_state(window, wsize, whave, wnext);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn invalid_distance_too_far_back() {
        // Window holds only `whave` valid bytes; a distance reaching beyond that
        // (with `sane`) is a data error, leaving mode == Bad and the canonical
        // message — and NOT writing past it.
        let wsize = 64u32;
        let window = patterned_window(wsize as usize);
        let whave = 4u32; // only 4 valid bytes
        let tokens = [Token::Match { len: 3, dist: 10 }]; // 10 > whave (out_pos 0)

        let state = make_fixed_state(window, wsize, whave, 4);
        let (out, mode, msg) = run_fast(state, &encode_fixed(&tokens, false), 8);
        assert_eq!(mode, InflateMode::Bad);
        assert_eq!(msg, Some("invalid distance too far back"));
        assert!(
            out.is_empty(),
            "no output should be produced before the error"
        );
    }

    // -- Two-level (sub-table) code traversal ---------------------------------

    #[test]
    fn second_level_distance_code() {
        // Custom distance code whose longest codewords (7 bits) exceed the
        // 6-bit root, forcing a sub-table — exercises `continue 'dodist`.
        // Lengths [1,2,3,4,5,6,7,7] over distance symbols 0..7 form a complete
        // code (Kraft sum == 1).
        let dist_lens: Vec<u16> = vec![1, 2, 3, 4, 5, 6, 7, 7];
        let (dist_codes_buf, distbits) = build_table(CodeType::Dists, &dist_lens, 6);
        assert_eq!(
            distbits, 6,
            "root must stay at 6 so 7-bit codes need a sub-table"
        );
        let enc_codes = canonical_codes(&dist_lens);

        // Distance symbol 6 → 7-bit code (sub-table) → base DBASE[6]=9, 2 extra.
        let dist_sym = 6usize;
        assert_eq!(
            enc_codes[dist_sym].1, 7,
            "symbol 6 must use a 7-bit (2nd-level) code"
        );
        let extra_val = 0u32; // distance == 9
        let distance = T_DBASE[dist_sym] + extra_val as u16;

        // 12 distinct literals, then a length-4 match at distance 9.
        let mut bw = BitWriter::new();
        let lits: Vec<u8> = (0..12).map(|i| 100 + i as u8).collect();
        for &b in &lits {
            let (c, n) = fixed_litlen(b as u16);
            bw.write_code(c, n);
        }
        // length 4 (fixed code 258, 0 extra)
        let li = length_index(4);
        let (lc, ln) = fixed_litlen(257 + li as u16);
        bw.write_code(lc, ln);
        // distance via the custom 2-level code + extra bits
        let (dc, dn) = enc_codes[dist_sym];
        bw.write_code(dc, dn);
        if T_DEXT[dist_sym] > 0 {
            bw.write_lsb(extra_val, T_DEXT[dist_sym] as u32);
        }
        let (ec, en) = fixed_litlen(256);
        bw.write_code(ec, en);
        let stream = bw.finish();

        // Tokens for the reference oracle.
        let mut tokens: Vec<Token> = lits.iter().map(|&b| Token::Lit(b)).collect();
        tokens.push(Token::Match {
            len: 4,
            dist: distance,
        });
        let expected = reference_decode(&[], &tokens);

        // codes arena: fixed litlen table at 0, custom distance table at 512.
        let mut codes = build_fixed_litlen();
        codes.extend_from_slice(&dist_codes_buf);
        let state = make_state(codes, 0, 9, 512, distbits, Vec::new(), 0, 0, 0);

        let (out, mode, msg) = run_fast(state, &stream, expected.len());
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, expected);
    }

    #[test]
    fn second_level_length_code() {
        // Custom literal/length code whose longest codewords (8 bits) exceed the
        // 7-bit root, forcing a sub-table — exercises `continue 'dolen`. The
        // 8-bit codes fall on literal symbol 7 and the end-of-block symbol 256,
        // so BOTH a literal and the EOB take the second-level path.
        let mut lens = vec![0u16; 257];
        lens[0] = 1;
        lens[1] = 2;
        lens[2] = 3;
        lens[3] = 4;
        lens[4] = 5;
        lens[5] = 6;
        lens[6] = 7;
        lens[7] = 8;
        lens[256] = 8;
        let (litlen_buf, lenbits) = build_table(CodeType::Lens, &lens, 7);
        assert_eq!(
            lenbits, 7,
            "root must stay at 7 so 8-bit codes need a sub-table"
        );
        let enc_codes = canonical_codes(&lens);
        assert_eq!(
            enc_codes[7].1, 8,
            "literal 7 must use an 8-bit (2nd-level) code"
        );
        assert_eq!(
            enc_codes[256].1, 8,
            "EOB must use an 8-bit (2nd-level) code"
        );

        let mut bw = BitWriter::new();
        let (c7, n7) = enc_codes[7]; // literal byte 0x07 via sub-table
        bw.write_code(c7, n7);
        let (ce, ne) = enc_codes[256]; // EOB via sub-table
        bw.write_code(ce, ne);
        let stream = bw.finish();

        // codes arena: custom litlen table at 0, fixed distance table appended (unused).
        let mut codes = litlen_buf;
        let dist_at = codes.len();
        codes.extend_from_slice(&build_fixed_dist());
        let state = make_state(codes, 0, lenbits, dist_at, 5, Vec::new(), 0, 0, 0);

        let (out, mode, msg) = run_fast(state, &stream, 4);
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![7u8]);
    }

    #[test]
    fn invalid_literal_length_code() {
        // A litlen table with a single 1-bit code (EOB) is incomplete; the table
        // builder fills the unused slot with an invalid marker. Feeding the "1"
        // bit hits it → mode Bad + canonical message.
        let mut lens = vec![0u16; 257];
        lens[256] = 1; // only EOB has a code; code "1" is invalid
        let (litlen_buf, lenbits) = build_table(CodeType::Lens, &lens, 9);
        assert_eq!(lenbits, 1, "single 1-bit code → 1-bit root");

        let mut bw = BitWriter::new();
        bw.write_lsb(1, 1); // the invalid code
        let stream = bw.finish();

        let mut codes = litlen_buf;
        let dist_at = codes.len();
        codes.extend_from_slice(&build_fixed_dist());
        let state = make_state(codes, 0, lenbits, dist_at, 5, Vec::new(), 0, 0, 0);

        let (out, mode, msg) = run_fast(state, &stream, 4);
        assert_eq!(mode, InflateMode::Bad);
        assert_eq!(msg, Some("invalid literal/length code"));
        assert!(out.is_empty());
    }

    #[test]
    fn invalid_distance_code() {
        // Valid (fixed) length code reaches `dodist`, then an invalid 1-bit
        // distance code → mode Bad + canonical message.
        let mut dist_lens = vec![0u16; 30];
        dist_lens[0] = 1; // only symbol 0; code "1" is invalid
        let (dist_buf, distbits) = build_table(CodeType::Dists, &dist_lens, 6);
        assert_eq!(distbits, 1);

        let mut bw = BitWriter::new();
        // fixed length code for length 3 (symbol 257, 0 extra)
        let (lc, ln) = fixed_litlen(257);
        bw.write_code(lc, ln);
        bw.write_lsb(1, 1); // invalid distance code
        let stream = bw.finish();

        let mut codes = build_fixed_litlen();
        codes.extend_from_slice(&dist_buf);
        let state = make_state(codes, 0, 9, 512, distbits, Vec::new(), 0, 0, 0);

        let (_out, mode, msg) = run_fast(state, &stream, 8);
        assert_eq!(mode, InflateMode::Bad);
        assert_eq!(msg, Some("invalid distance code"));
    }

    #[test]
    fn empty_block_immediate_end_of_block() {
        // A block consisting of only the end-of-block symbol: no output, mode
        // transitions straight to Type.
        let tokens: [Token; 0] = [];
        let (out, mode, msg) = run_fast(
            make_fixed_state(Vec::new(), 0, 0, 0),
            &encode_fixed(&tokens, false),
            0,
        );
        assert_eq!(msg, None);
        assert_eq!(mode, InflateMode::Type);
        assert!(out.is_empty());
    }
}
