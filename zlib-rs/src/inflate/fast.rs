//! The `inflate` fast inner decode loop (`inffast.c`).
//!
//! This module is the safe-Rust translation of zlib's `inflate_fast()`
//! (`inffast.c`, zlib 1.3.2.1) — the *hot path* of decompression. The C source
//! notes that, with large enough input and output buffers, **more than 95% of
//! `inflate` execution time is spent in this routine**, because it decodes a
//! whole run of literal / length / distance symbols without re-checking for
//! available input or output on every byte.
//!
//! # Why this is safe Rust with **zero `unsafe`**
//!
//! The C routine achieves its speed by hoisting the input/output bounds checks
//! out of the inner loop: the caller guarantees `avail_in >= 6` and
//! `avail_out >= 258` on entry, which is exactly enough headroom for one full
//! length/distance pair (≤ 48 bits ≈ 6 input bytes, ≤ 258 output bytes), so the
//! raw-pointer writes inside the loop need no per-byte checks. Per the Agent
//! Action Plan (§0.2.2 / §0.6.2) the entire `zlib-rs` core — **including this
//! file** — is compiled under `#![forbid(unsafe_code)]`. This is *stricter* than
//! upstream's blueprint, which permitted `unsafe` here. We therefore keep the
//! same loop structure but index through safe, bounds-checked slices
//! ([`output`], [`InflateState::window`], `input`) and accept the residual
//! bounds-check cost. The entry guarantees still hold, so for any well-formed
//! stream the indices are always in range and the checks never fail; a *corrupt*
//! stream simply lands in [`InflateMode::Bad`] (it can never provoke
//! out-of-bounds undefined behavior — at worst a checked index would panic, but
//! the `op_w > whave` distance guard and the entry headroom prevent that for the
//! caller-upheld contract).
//!
//! # Bit-exactness
//!
//! [`inflate_fast`] and the per-code "slow" path in
//! [`crate::inflate::state::InflateState::inflate`] decode the *same* DEFLATE
//! codes and **must produce byte-identical output**; the fast path is purely a
//! throughput optimization. To preserve that, this port reproduces the C bit
//! accumulator fill/consume order exactly (`hold += (byte as u64) << bits;
//! bits += 8;`), and — crucially — performs the overlapping LZ77 match copy
//! (the run-length case where `dist < len`) as a **byte-by-byte forward loop**,
//! never a bulk `copy_within`/`copy_from_slice` (which would behave like a
//! non-overlapping `memmove` and corrupt run-length matches). See
//! [`inflate_fast`] for the per-line correspondence to `inffast.c`.
//!
//! # `no_std`
//!
//! The routine uses only `core` (indexing and integer arithmetic); it allocates
//! nothing, operating entirely on the caller's `input`/`output` slices and the
//! state's already-allocated sliding [`window`](InflateState::window).

use crate::inflate::state::{InflateMode, InflateState};

/// Minimum match length at which a *long overlapping* direct-output
/// back-reference (`dist < len`, i.e. a run-length / RLE match) is diverted from
/// the per-byte forward loop in [`inflate_fast`] to the bulk `copy_within`
/// [`copy_long_match`].
///
/// Below this length the byte loop wins (the compiler keeps it tight and there
/// is no copy-routine call/setup cost); at and above it the bulk doubling wins
/// decisively — e.g. the `repetitive` profile's distance-8, length-258 matches,
/// which a byte loop must propagate one bounds-checked byte at a time. Short
/// matches and all *non-overlapping* matches (`dist >= len`, the per-symbol-bound
/// `binary` profile's common case) keep the original byte loop, executing exactly
/// the same copy code as before.
const INFLATE_RLE_BULK_THRESHOLD: usize = 32;

/// Copy a long *overlapping* LZ77 back-reference (`dist < len`) that lies
/// entirely within already-produced `output` (the C `else` branch of
/// `inffast.c` L249-L262) using bulk `copy_within`, returning the advanced
/// output cursor (`out_idx + len`).
///
/// Produces `output[out_idx..out_idx + len]` from the `dist`-byte pattern based
/// at the back-reference source `src = out_idx - dist`, replicated forward in
/// non-overlapping chunks that double each step — the run-length / RLE case a
/// byte loop would propagate one bounds-checked byte at a time. (The doubling is
/// written generally and stays correct for `dist >= len` too — where it collapses
/// to a single `copy_within` — but the call site only routes overlapping long
/// matches here, leaving non-overlapping matches on the byte loop.)
///
/// # Out-of-line and `#[cold]`
///
/// This is deliberately `#[cold]` + `#[inline(never)]`. [`inflate_fast`]'s inner
/// loop is acutely codegen-sensitive: the per-symbol-bound `binary` profile is
/// measurably slowed if this copy code is *inlined* into the loop body, even on
/// a branch binary never takes, because the extra live values raise register
/// pressure across the literal-decode / bit-accumulator hot path. Keeping the
/// body out-of-line leaves that loop byte-for-byte as it was with a bare byte
/// loop, while long overlapping matches still get the fast path. Such matches
/// are comparatively few, so the call overhead is negligible.
///
/// # Bit-exactness (AAP §0.7.1 / §0.6.4)
///
/// The first `dist` bytes (`output[src..src + dist]`, already produced) are the
/// pattern. Each [`slice::copy_within`] sets `output[src + filled + j] =
/// output[src + j]`; `filled` starts at `dist` and stays a multiple of `dist`
/// except on the final partial chunk, so `(filled + j) % dist == j % dist` — the
/// value already present. The written range `output[src + dist..src + dist +
/// len] == output[out_idx..out_idx + len]` is therefore byte-identical to the C
/// `*out++ = *from++` forward loop (true for the non-overlapping single-chunk
/// case as well, where `filled == dist` and `chunk == len`).
///
/// [`slice::copy_within`] is a *safe* std method (its `unsafe` lives in `core`,
/// not this crate), preserving the crate-wide `#![forbid(unsafe_code)]`.
#[cold]
#[inline(never)]
fn copy_long_match(output: &mut [u8], out_idx: usize, dist: usize, len: usize) -> usize {
    let src = out_idx - dist;
    let target = dist + len;
    let mut filled = dist;
    while filled < target {
        let chunk = core::cmp::min(filled, target - filled);
        output.copy_within(src..src + chunk, src + filled);
        filled += chunk;
    }
    out_idx + len
}

/// Decode literal, length, and distance codes and write the resulting literal
/// and match bytes until either not enough input or output remains, an
/// end-of-block code is reached, or a data error is encountered.
///
/// This is the safe-Rust port of C `inflate_fast(strm, start)` (`inffast.c`
/// L50-L305). Instead of a `z_stream` with raw `next_in`/`next_out` pointers,
/// the caller passes the **full** `input` and `output` slices plus mutable
/// cursors (`in_pos`/`out_pos`) — the full buffers are required because the
/// window-copy logic reads already-produced output behind `out_pos`.
///
/// # Parameters
///
/// * `state` — the decompressor state. On entry `state.mode` must be
///   [`InflateMode::Len`] and the decode tables (`lencode`/`distcode` offsets
///   into `state.codes`, `lenbits`/`distbits`) must be built. The bit
///   accumulator (`state.hold`/`state.bits`) is loaded into locals on entry and
///   written back on return.
/// * `input` — the full input buffer; `*in_pos` is the index of the next unread
///   byte (C `next_in`). Advanced as bytes are pulled into the accumulator.
/// * `in_pos` — in/out cursor into `input`.
/// * `output` — the full output buffer; `*out_pos` is the index of the next
///   byte to write (C `next_out`). Advanced as literals/matches are emitted.
/// * `out_pos` — in/out cursor into `output`.
/// * `start` — the value of `avail_out` (`output.len() - *out_pos`) at the
///   moment `inflate()` began tracking output for the current call. The C
///   landmark `beg = out - (start - avail_out)` becomes `beg = output.len() -
///   start`: the index where *this* call's output began. `*out_pos - beg` is
///   the number of bytes produced so far this call, i.e. the maximum distance
///   that can be satisfied **directly from `output`**; anything farther back is
///   copied from `state.window`.
///
/// # Returns
///
/// Nothing. On return `state.mode` is one of:
/// * [`InflateMode::Len`] — ran out of enough input or output space (the
///   loop's tail condition became false); the caller continues with the slow
///   per-code path or re-enters once more input/output is available.
/// * [`InflateMode::Type`] — reached an end-of-block code.
/// * [`InflateMode::Bad`] — a data error was found; `state.msg` describes it.
///
/// The cursors `*in_pos`/`*out_pos` and `state.hold`/`state.bits` are updated to
/// reflect exactly the consumed input and produced output.
///
/// # Entry assumptions
///
/// Mirrors the C contract (`inffast.c` L23-L29). The caller must guarantee
/// `state.mode == Len`, `avail_in >= 6`, `avail_out >= 258`, and
/// `avail_out <= start <= output.len()`. These are asserted with `debug_assert!`
/// (compiled out in release, so they never change behavior).
///
/// The C source also lists `state.bits < 8` as an entry assumption, but it is
/// **not** asserted here. That bound is only the *worst case* for input
/// consumption (it makes a length/distance pair need up to the full 6 input
/// bytes; a larger `bits` only reduces the bytes pulled). The exit logic below
/// normalizes any `bits` value by returning whole unused bytes to the input, so
/// correctness does not depend on it — and asserting it would wrongly reject the
/// legitimate mid-stream entry points where `inflate()` resumes the fast loop
/// with a partially filled accumulator.
//
// The four byte-copy loops in the body faithfully mirror the C
// `*out++ = *from++` pointer-increment copies (inffast.c L197-L262), advancing a
// source index and the output cursor in lockstep. Clippy's
// `explicit_counter_loop` would have these counters replaced by iterator
// adapters, but the two output->output copies are intentionally *overlapping*
// run-length copies (`dist < len`): each byte must be read only after the
// previous byte was written, which a bulk slice copy cannot express and an
// iterator over a precomputed range paired with a separately mutated cursor only
// obscures. Keeping a single, uniform explicit-counter form across all four copy
// sites maximizes correctness and the line-by-line correspondence to the C
// source. The lint is therefore allowed for this function (never via `unsafe`);
// the only explicit-counter loops in the function are exactly these copy loops.
#[allow(clippy::explicit_counter_loop)]
pub fn inflate_fast(
    state: &mut InflateState,
    input: &[u8],
    in_pos: &mut usize,
    output: &mut [u8],
    out_pos: &mut usize,
    start: usize,
) {
    // --- Entry assumptions (inffast.c L23-L29). ---------------------------
    debug_assert_eq!(
        state.mode,
        InflateMode::Len,
        "inflate_fast entry: mode == LEN"
    );
    debug_assert!(
        input.len() - *in_pos >= 6,
        "inflate_fast entry: avail_in >= 6"
    );
    debug_assert!(
        output.len() - *out_pos >= 258,
        "inflate_fast entry: avail_out >= 258"
    );
    debug_assert!(
        start <= output.len(),
        "inflate_fast entry: start <= output.len()"
    );
    debug_assert!(
        start >= output.len() - *out_pos,
        "inflate_fast entry: start >= avail_out"
    );

    // --- Copy state to local variables (inffast.c L77-L96). ---------------
    //
    // Working with locals both mirrors the C "copy state to local variables"
    // optimization and side-steps every borrow-checker conflict without
    // `unsafe`: `Code` is `Copy`, so each `state.codes[..]` read yields an owned
    // value with no lingering borrow, and the scalar window/accumulator fields
    // are plain copies. The cursors are pulled into `in_idx`/`out_idx` and
    // written back through the `&mut usize` params at the end (C writes back to
    // `strm->next_in`/`strm->next_out`).
    let mut in_idx = *in_pos;
    let mut out_idx = *out_pos;

    // Index landmarks translating the C pointer math:
    //   last = in + (avail_in - 5)   -> loop while `in_idx < last`
    //   end  = out + (avail_out - 257) -> loop while `out_idx < end`
    //   beg  = out - (start - avail_out) -> output index where this call began
    let last = input.len() - 5;
    let end = output.len() - 257;
    let beg = output.len() - start;

    let wsize = state.wsize;
    let whave = state.whave;
    let wnext = state.wnext;
    let mut hold = state.hold;
    let mut bits = state.bits;
    let lencode = state.lencode;
    let distcode = state.distcode;
    // First-level index masks. Kept as `u64` so they combine directly with the
    // `u64` accumulator (C uses `unsigned`, promoted to `unsigned long` by `&`).
    let lmask = (1u64 << state.lenbits) - 1;
    let dmask = (1u64 << state.distbits) - 1;
    let sane = state.sane;

    // --- Decode literals and length/distance pairs until end-of-block or not
    //     enough input/output (inffast.c L98-L288). The C `do { … } while (…)`
    //     becomes a `loop { … }` with the tail condition checked at the bottom.
    'main: loop {
        // Refill: ensure ≥ 15 bits buffered for the length code (L101-L106).
        if bits < 15 {
            hold += (input[in_idx] as u64) << bits;
            in_idx += 1;
            bits += 8;
            hold += (input[in_idx] as u64) << bits;
            in_idx += 1;
            bits += 8;
        }

        // Length-code lookup. The C `dolen:` label with its `goto dolen` for a
        // second-level table link becomes this inner `loop` (continue == goto).
        let mut here = state.codes[lencode + (hold & lmask) as usize];
        'dolen: loop {
            // Consume this entry's bits, then read its operation (L109-L112).
            let mut op = here.bits as u32;
            hold >>= op;
            bits -= op;
            op = here.op as u32;

            if op == 0 {
                // Literal (L113-L118).
                output[out_idx] = here.val as u8;
                out_idx += 1;
                break 'dolen;
            } else if op & 16 != 0 {
                // Length base (L119-L131): read the base length, then any extra
                // bits the length code requires.
                let mut len = here.val as usize;
                op &= 15; // number of extra bits
                if op != 0 {
                    if bits < op {
                        hold += (input[in_idx] as u64) << bits;
                        in_idx += 1;
                        bits += 8;
                    }
                    len += (hold & ((1u64 << op) - 1)) as usize;
                    hold >>= op;
                    bits -= op;
                }

                // Refill for the distance code (L132-L137).
                if bits < 15 {
                    hold += (input[in_idx] as u64) << bits;
                    in_idx += 1;
                    bits += 8;
                    hold += (input[in_idx] as u64) << bits;
                    in_idx += 1;
                    bits += 8;
                }

                // Distance-code lookup. C `dodist:` + `goto dodist` (L138-L267).
                let mut dhere = state.codes[distcode + (hold & dmask) as usize];
                'dodist: loop {
                    let mut dop = dhere.bits as u32;
                    hold >>= dop;
                    bits -= dop;
                    dop = dhere.op as u32;

                    if dop & 16 != 0 {
                        // Distance base (L144-L262): base distance + extra bits.
                        // The distance extra can need up to 13 bits, so up to two
                        // byte pulls (L147-L154).
                        let mut dist = dhere.val as usize;
                        dop &= 15; // number of extra bits
                        if bits < dop {
                            hold += (input[in_idx] as u64) << bits;
                            in_idx += 1;
                            bits += 8;
                            if bits < dop {
                                hold += (input[in_idx] as u64) << bits;
                                in_idx += 1;
                                bits += 8;
                            }
                        }
                        dist += (hold & ((1u64 << dop) - 1)) as usize;
                        // NOTE: the `INFLATE_STRICT` `dist > dmax` guard
                        // (L156-L163) is compiled out of stock zlib by default
                        // and is intentionally omitted, matching the slow path.
                        hold >>= dop;
                        bits -= dop;

                        // Perform the copy. `from_output_avail` is the number of
                        // bytes produced so far this call (C `op = out - beg`,
                        // L167); a distance reaching farther back than that comes
                        // (partly) from the sliding window.
                        let from_output_avail = out_idx - beg;
                        if dist > from_output_avail {
                            // Copy (partly) from the window (L168-L248).
                            let op_w = dist - from_output_avail; // distance back in window
                            if op_w > whave && sane {
                                // L170-L176: distance reaches before the start of
                                // available history. (The
                                // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR
                                // branch, L177-L195, is off by default and not
                                // implemented.)
                                state.msg = Some("invalid distance too far back");
                                state.mode = InflateMode::Bad;
                                break 'main;
                            }

                            // After copying the window portion, `from_idx` points
                            // either at more window bytes (`from_window == true`)
                            // or at the "rest from output" position
                            // (`from_window == false`, equal to `out_idx - dist`).
                            let from_idx;
                            let from_window;
                            if wnext == 0 {
                                // Very common case (L198-L207): window has not
                                // wrapped; history is the tail of the buffer.
                                let mut wf = wsize - op_w;
                                if op_w < len {
                                    len -= op_w;
                                    for _ in 0..op_w {
                                        let b = state.window[wf];
                                        output[out_idx] = b;
                                        out_idx += 1;
                                        wf += 1;
                                    }
                                    from_idx = out_idx - dist; // rest from output
                                    from_window = false;
                                } else {
                                    from_idx = wf;
                                    from_window = true;
                                }
                            } else if wnext < op_w {
                                // Wrap-around (L208-L226): copy from the end of
                                // the window, then (if needed) from its start.
                                let mut wf = wsize + wnext - op_w;
                                let op2 = op_w - wnext; // bytes at end of window
                                if op2 < len {
                                    len -= op2;
                                    for _ in 0..op2 {
                                        let b = state.window[wf];
                                        output[out_idx] = b;
                                        out_idx += 1;
                                        wf += 1;
                                    }
                                    if wnext < len {
                                        // Some from the start of the window.
                                        let mut wf2 = 0usize;
                                        len -= wnext;
                                        for _ in 0..wnext {
                                            let b = state.window[wf2];
                                            output[out_idx] = b;
                                            out_idx += 1;
                                            wf2 += 1;
                                        }
                                        from_idx = out_idx - dist; // rest from output
                                        from_window = false;
                                    } else {
                                        from_idx = 0; // window start
                                        from_window = true;
                                    }
                                } else {
                                    from_idx = wf;
                                    from_window = true;
                                }
                            } else {
                                // Contiguous in window (L227-L236).
                                let mut wf = wnext - op_w;
                                if op_w < len {
                                    len -= op_w;
                                    for _ in 0..op_w {
                                        let b = state.window[wf];
                                        output[out_idx] = b;
                                        out_idx += 1;
                                        wf += 1;
                                    }
                                    from_idx = out_idx - dist; // rest from output
                                    from_window = false;
                                } else {
                                    from_idx = wf;
                                    from_window = true;
                                }
                            }

                            // Copy the remaining `len` bytes (the C unrolled
                            // `while (len > 2) { 3× } if (len) { 1-2 }` blocks,
                            // L237-L247). A plain forward byte loop is
                            // bit-identical; the source is the window or the
                            // output depending on the case above.
                            if from_window {
                                let mut f = from_idx;
                                for _ in 0..len {
                                    let b = state.window[f];
                                    output[out_idx] = b;
                                    out_idx += 1;
                                    f += 1;
                                }
                            } else {
                                let mut f = from_idx;
                                for _ in 0..len {
                                    let b = output[f];
                                    output[out_idx] = b;
                                    out_idx += 1;
                                    f += 1;
                                }
                            }
                        } else if dist < len && len > INFLATE_RLE_BULK_THRESHOLD {
                            // Long *overlapping* run-length match (`dist < len`) — the
                            // documented Issue 2 hot spot. Because the destination
                            // overlaps the source it cannot use a single `memcpy`, and
                            // a byte-by-byte forward loop must propagate every byte
                            // with its own bounds check (the `repetitive` worst case:
                            // dist 8, len 258, ~1.45× slower than C). Delegate to the
                            // out-of-line, `#[cold]` [`copy_long_match`], whose bulk
                            // `copy_within` does one bounds check per (growing) chunk
                            // instead of per byte.
                            //
                            // This branch is the *only* change to the direct-output
                            // copy: every **non-overlapping** match (`dist >= len`,
                            // the entire `binary`/`text`/`random` common case) and
                            // every short match still runs the byte loop below,
                            // byte-for-byte the original code. Keeping the bulk body
                            // out-of-line leaves that loop's codegen undisturbed.
                            out_idx = copy_long_match(output, out_idx, dist, len);
                        } else {
                            // Copy directly from output (L249-L262). This is the
                            // LZ77 match and may still **overlap** when `dist < len`
                            // for *short* matches (`len <= INFLATE_RLE_BULK_THRESHOLD`,
                            // where the byte loop beats any copy-routine setup). A
                            // forward byte-by-byte loop reproduces the C `*out++ =
                            // *from++` overlap semantics exactly; a bulk `copy_within`
                            // must NOT be used here (it is a non-overlapping memmove).
                            let mut f = out_idx - dist;
                            for _ in 0..len {
                                let b = output[f];
                                output[out_idx] = b;
                                out_idx += 1;
                                f += 1;
                            }
                        }
                        break 'dodist;
                    } else if dop & 64 == 0 {
                        // Second-level distance code (L264-L267): re-index the
                        // sub-table with the next `dop` bits and re-dispatch.
                        dhere = state.codes
                            [distcode + dhere.val as usize + (hold & ((1u64 << dop) - 1)) as usize];
                        continue 'dodist;
                    } else {
                        // Invalid distance code (L268-L272).
                        state.msg = Some("invalid distance code");
                        state.mode = InflateMode::Bad;
                        break 'main;
                    }
                }

                break 'dolen;
            } else if op & 64 == 0 {
                // Second-level length code (L274-L277): re-index the sub-table
                // with the next `op` bits and re-dispatch (goto dolen).
                here =
                    state.codes[lencode + here.val as usize + (hold & ((1u64 << op) - 1)) as usize];
                continue 'dolen;
            } else if op & 32 != 0 {
                // End of block (L278-L282): hand control back to inflate() to
                // interpret the next block.
                state.mode = InflateMode::Type;
                break 'main;
            } else {
                // Invalid literal/length code (L283-L287).
                state.msg = Some("invalid literal/length code");
                state.mode = InflateMode::Bad;
                break 'main;
            }
        }

        // C `do { … } while (in < last && out < end);` (L288). Exiting here
        // leaves `state.mode == Len` (unchanged), signalling "ran out of input
        // or output space".
        if !(in_idx < last && out_idx < end) {
            break 'main;
        }
    }

    // --- Return unused bytes (inffast.c L290-L304). -----------------------
    //
    // Whole bytes still buffered in the accumulator are handed back to the
    // input; only the fractional remainder (< 8 bits) is kept. The C comment
    // notes this cannot rewind `in` past the buffer start under the documented
    // entry conditions.
    let bytes = bits >> 3;
    in_idx -= bytes as usize;
    bits -= bytes << 3;
    hold &= (1u64 << bits) - 1;

    // Write the locals back. The `in_idx`/`out_idx` cursors are returned through
    // the `&mut usize` params (the caller derives `avail_in`/`avail_out` from
    // them — no need to recompute `strm->avail_*` as the C does at L299-L301).
    // `state.mode` was set inside the loop (Type/Bad) or is unchanged (Len).
    *in_pos = in_idx;
    *out_pos = out_idx;
    state.hold = hold;
    state.bits = bits;
}

/// The `inflateBack` fast inner decode loop — the window-as-output sibling of
/// [`inflate_fast`].
///
/// C zlib uses the *same* `inflate_fast()` for both drivers: `infback.c` calls
/// `inflate_fast(strm, state->wsize)` with `strm->next_out` pointing **into the
/// window** (infback.c L424-L428). [`inflate_fast`] cannot serve that role under
/// `#![forbid(unsafe_code)]` because it takes the output and the history window
/// as two *disjoint* `&mut` borrows, whereas in `inflateBack` they are the same
/// buffer ([`InflateState::window`]) — an impossible double mutable borrow. This
/// routine resolves that by writing into, and reading match history from, the
/// single `state.window` buffer, so it needs only one mutable borrow of `state`.
///
/// It is otherwise a faithful, bit-identical translation of `inflate_fast`,
/// specialized to the `inflateBack` window model:
///
/// * **`beg == 0`.** C passes `start = wsize`, so `beg = out - (start - avail_out)`
///   resolves to the window base; "bytes produced this call" is therefore the
///   absolute window index `out_idx` (= the C `put` offset).
/// * **`wnext == 0` always.** `inflateBack`'s window never advances circularly:
///   the `ROOM()` flush resets `put` to `0` and leaves `wnext == 0` (infback.c
///   L151-L162), so only the very-common `wnext == 0` window-history branch of
///   [`inflate_fast`] is reachable here. This is asserted in debug builds.
/// * **`end = wsize - 257`.** The loop runs while `out_idx < end`, i.e. while at
///   least 258 bytes of window space remain — the caller upholds `left >= 258`
///   on entry, exactly as for [`inflate_fast`]. The fast loop therefore never
///   fills the window (never needs `ROOM()`); when it stops with the window
///   nearly full, the per-code slow path takes over and flushes via `ROOM()`.
///
/// # Entry conditions (upheld by the caller, the `inflateBack` `LEN` arm)
///
/// * `state.mode == Len`, `state.wnext == 0`.
/// * At least 6 input bytes remain (`input.len() - *in_pos >= 6`).
/// * At least 258 bytes of window space remain (`state.wsize - *out_pos >= 258`).
///
/// `state.hold`/`state.bits` carry the bit accumulator in and out (the caller
/// syncs its loop locals through `state` around the call, mirroring the C
/// `RESTORE()`/`LOAD()`).
///
/// # Exit `state.mode`
///
/// * [`InflateMode::Len`] — ran out of enough input or window space; the caller
///   resumes with the per-code path.
/// * [`InflateMode::Type`] — reached an end-of-block code.
/// * [`InflateMode::Bad`] — a data error was found (`state.msg` is set).
///
/// `*out_pos` is the updated window write index (`put`); the caller derives
/// `left = wsize - *out_pos`.
#[allow(clippy::explicit_counter_loop)]
pub fn inflate_fast_back(
    state: &mut InflateState,
    input: &[u8],
    in_pos: &mut usize,
    out_pos: &mut usize,
) {
    // --- Entry assumptions (infback.c L424; inffast.c L23-L29). -----------
    debug_assert_eq!(
        state.mode,
        InflateMode::Len,
        "inflate_fast_back entry: mode == LEN"
    );
    debug_assert_eq!(
        state.wnext, 0,
        "inflate_fast_back entry: inflateBack window never wraps (wnext == 0)"
    );
    debug_assert!(
        input.len() - *in_pos >= 6,
        "inflate_fast_back entry: avail_in >= 6"
    );
    debug_assert!(
        state.wsize - *out_pos >= 258,
        "inflate_fast_back entry: window space >= 258"
    );

    // --- Copy state to local variables (inffast.c L77-L96). ---------------
    // The window itself stays as `state.window` (matches read freshly written
    // bytes for RLE, so it must be the live buffer). Every copy reads one byte
    // into a temporary first — `let b = state.window[f]; state.window[o] = b;` —
    // so the immutable read borrow ends before the mutable write borrow begins,
    // which is exactly why a single shared buffer is expressible in safe Rust.
    let mut in_idx = *in_pos;
    let mut out_idx = *out_pos;

    let wsize = state.wsize;
    // `last`/`end` translate the C pointer math; `beg == 0` for the back model,
    // so "bytes produced this call" is simply `out_idx`.
    let last = input.len() - 5;
    let end = wsize - 257;
    let whave = state.whave;
    let mut hold = state.hold;
    let mut bits = state.bits;
    let lencode = state.lencode;
    let distcode = state.distcode;
    let lmask = (1u64 << state.lenbits) - 1;
    let dmask = (1u64 << state.distbits) - 1;
    let sane = state.sane;

    'main: loop {
        // Refill: ensure >= 15 bits buffered for the length code.
        if bits < 15 {
            hold += (input[in_idx] as u64) << bits;
            in_idx += 1;
            bits += 8;
            hold += (input[in_idx] as u64) << bits;
            in_idx += 1;
            bits += 8;
        }

        let mut here = state.codes[lencode + (hold & lmask) as usize];
        'dolen: loop {
            let mut op = here.bits as u32;
            hold >>= op;
            bits -= op;
            op = here.op as u32;

            if op == 0 {
                // Literal -> straight into the window.
                state.window[out_idx] = here.val as u8;
                out_idx += 1;
                break 'dolen;
            } else if op & 16 != 0 {
                // Length base + extra bits.
                let mut len = here.val as usize;
                op &= 15;
                if op != 0 {
                    if bits < op {
                        hold += (input[in_idx] as u64) << bits;
                        in_idx += 1;
                        bits += 8;
                    }
                    len += (hold & ((1u64 << op) - 1)) as usize;
                    hold >>= op;
                    bits -= op;
                }

                // Refill for the distance code.
                if bits < 15 {
                    hold += (input[in_idx] as u64) << bits;
                    in_idx += 1;
                    bits += 8;
                    hold += (input[in_idx] as u64) << bits;
                    in_idx += 1;
                    bits += 8;
                }

                let mut dhere = state.codes[distcode + (hold & dmask) as usize];
                'dodist: loop {
                    let mut dop = dhere.bits as u32;
                    hold >>= dop;
                    bits -= dop;
                    dop = dhere.op as u32;

                    if dop & 16 != 0 {
                        // Distance base + extra bits (up to 13 -> two pulls).
                        let mut dist = dhere.val as usize;
                        dop &= 15;
                        if bits < dop {
                            hold += (input[in_idx] as u64) << bits;
                            in_idx += 1;
                            bits += 8;
                            if bits < dop {
                                hold += (input[in_idx] as u64) << bits;
                                in_idx += 1;
                                bits += 8;
                            }
                        }
                        dist += (hold & ((1u64 << dop) - 1)) as usize;
                        hold >>= dop;
                        bits -= dop;

                        // `beg == 0`, so bytes produced this call == `out_idx`.
                        if dist > out_idx {
                            // Copy (partly) from the window history tail. With
                            // `wnext == 0` this is the only reachable window
                            // branch (inffast.c L198-L207).
                            let op_w = dist - out_idx; // distance back in window
                            if op_w > whave && sane {
                                // Reaches before available history (before the
                                // window has been filled, `whave == 0`, so any
                                // such reference is rejected — matching the
                                // slow path's `offset > wsize - left` guard).
                                state.msg = Some("invalid distance too far back");
                                state.mode = InflateMode::Bad;
                                break 'main;
                            }
                            let wf = wsize - op_w;
                            if op_w < len {
                                // Some bytes from the window tail, the remainder
                                // from `out - dist` (which, the window being the
                                // output, lands back at the window start).
                                len -= op_w;
                                let mut f = wf;
                                for _ in 0..op_w {
                                    let b = state.window[f];
                                    state.window[out_idx] = b;
                                    out_idx += 1;
                                    f += 1;
                                }
                                let mut g = out_idx - dist; // == 0 here
                                for _ in 0..len {
                                    let b = state.window[g];
                                    state.window[out_idx] = b;
                                    out_idx += 1;
                                    g += 1;
                                }
                            } else {
                                // Whole match lies in the window tail.
                                let mut f = wf;
                                for _ in 0..len {
                                    let b = state.window[f];
                                    state.window[out_idx] = b;
                                    out_idx += 1;
                                    f += 1;
                                }
                            }
                        } else {
                            // Copy directly from the output (= window); may
                            // overlap when `dist < len` (RLE). A forward
                            // byte-by-byte loop reproduces the C `*put++ =
                            // *from++` overlap semantics exactly.
                            let mut f = out_idx - dist;
                            for _ in 0..len {
                                let b = state.window[f];
                                state.window[out_idx] = b;
                                out_idx += 1;
                                f += 1;
                            }
                        }
                        break 'dodist;
                    } else if dop & 64 == 0 {
                        // Second-level distance code: re-index the sub-table.
                        dhere = state.codes
                            [distcode + dhere.val as usize + (hold & ((1u64 << dop) - 1)) as usize];
                        continue 'dodist;
                    } else {
                        state.msg = Some("invalid distance code");
                        state.mode = InflateMode::Bad;
                        break 'main;
                    }
                }

                break 'dolen;
            } else if op & 64 == 0 {
                // Second-level length code: re-index the sub-table.
                here =
                    state.codes[lencode + here.val as usize + (hold & ((1u64 << op) - 1)) as usize];
                continue 'dolen;
            } else if op & 32 != 0 {
                // End of block -> hand back to the block dispatcher.
                state.mode = InflateMode::Type;
                break 'main;
            } else {
                state.msg = Some("invalid literal/length code");
                state.mode = InflateMode::Bad;
                break 'main;
            }
        }

        // C `do { … } while (in < last && out < end);`. Exiting here leaves
        // `state.mode == Len` (ran out of input or window space).
        if !(in_idx < last && out_idx < end) {
            break 'main;
        }
    }

    // Return whole buffered bytes to the input (inffast.c L290-L304).
    let bytes = bits >> 3;
    in_idx -= bytes as usize;
    bits -= bytes << 3;
    hold &= (1u64 << bits) - 1;

    *in_pos = in_idx;
    *out_pos = out_idx;
    state.hold = hold;
    state.bits = bits;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::Flush;
    use crate::error::ReturnCode;
    use crate::inflate::tables::{CodeType, inflate_table};
    // The crate is `#![no_std]` whenever the default `std` feature is disabled,
    // so `Vec`, `Box`, and the `vec!` macro are not in the `core` prelude and
    // must be pulled in explicitly from `alloc` for the unit tests to compile
    // (and run) under `--no-default-features`. This mirrors the sibling test
    // modules (`deflate/fast.rs`, `inflate/back.rs`) and keeps the whole
    // `zlib-rs` unit-test binary buildable in the `Z_SOLO`/`no_std` config.
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    // ---------------------------------------------------------------------
    // DEFLATE bit-stream construction
    //
    // Per RFC 1951 §3.1.1, data elements (the block header and length/distance
    // *extra* bits) are packed least-significant-bit-first, while Huffman codes
    // are packed most-significant-bit-first. These two writers reproduce that
    // exactly so the hand-built streams decode through both the fast path and
    // the production slow path identically.
    // ---------------------------------------------------------------------

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

        /// Append the low `n` bits of `value`, least-significant bit first.
        fn write_bits(&mut self, value: u32, n: u32) {
            debug_assert!(n <= 16);
            let masked = if n == 0 { 0 } else { value & ((1u32 << n) - 1) };
            self.acc |= masked << self.nbits;
            self.nbits += n;
            while self.nbits >= 8 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.nbits -= 8;
            }
        }

        /// Append an `n`-bit Huffman `code`, most-significant bit first.
        fn write_huff(&mut self, code: u32, n: u32) {
            for i in (0..n).rev() {
                self.write_bits((code >> i) & 1, 1);
            }
        }

        /// Flush a partial trailing byte (zero-padded) and yield the bytes.
        fn finish(mut self) -> Vec<u8> {
            if self.nbits > 0 {
                self.bytes.push((self.acc & 0xff) as u8);
            }
            self.bytes
        }
    }

    // ---- fixed-Huffman symbol emission (RFC 1951 §3.2.6) -----------------

    /// Emit a fixed literal/length symbol's Huffman code.
    fn write_litlen_sym(bw: &mut BitWriter, sym: u32) {
        if sym <= 143 {
            bw.write_huff(0x30 + sym, 8);
        } else if sym <= 255 {
            bw.write_huff(0x190 + (sym - 144), 9);
        } else if sym <= 279 {
            bw.write_huff(sym - 256, 7);
        } else {
            bw.write_huff(0xc0 + (sym - 280), 8);
        }
    }

    /// Emit a fixed distance symbol's 5-bit Huffman code.
    fn write_dist_sym(bw: &mut BitWriter, sym: u32) {
        bw.write_huff(sym, 5);
    }

    /// (length symbol, extra bits, base length) for lengths 3..=258.
    const LEN_TABLE: [(u32, u32, u32); 29] = [
        (257, 0, 3),
        (258, 0, 4),
        (259, 0, 5),
        (260, 0, 6),
        (261, 0, 7),
        (262, 0, 8),
        (263, 0, 9),
        (264, 0, 10),
        (265, 1, 11),
        (266, 1, 13),
        (267, 1, 15),
        (268, 1, 17),
        (269, 2, 19),
        (270, 2, 23),
        (271, 2, 27),
        (272, 2, 31),
        (273, 3, 35),
        (274, 3, 43),
        (275, 3, 51),
        (276, 3, 59),
        (277, 4, 67),
        (278, 4, 83),
        (279, 4, 99),
        (280, 4, 115),
        (281, 5, 131),
        (282, 5, 163),
        (283, 5, 195),
        (284, 5, 227),
        (285, 0, 258),
    ];

    /// (distance symbol, extra bits, base distance) for distances 1..=32768.
    const DIST_TABLE: [(u32, u32, u32); 30] = [
        (0, 0, 1),
        (1, 0, 2),
        (2, 0, 3),
        (3, 0, 4),
        (4, 1, 5),
        (5, 1, 7),
        (6, 2, 9),
        (7, 2, 13),
        (8, 3, 17),
        (9, 3, 25),
        (10, 4, 33),
        (11, 4, 49),
        (12, 5, 65),
        (13, 5, 97),
        (14, 6, 129),
        (15, 6, 193),
        (16, 7, 257),
        (17, 7, 385),
        (18, 8, 513),
        (19, 8, 769),
        (20, 9, 1025),
        (21, 9, 1537),
        (22, 10, 2049),
        (23, 10, 3073),
        (24, 11, 4097),
        (25, 11, 6145),
        (26, 12, 8193),
        (27, 12, 12289),
        (28, 13, 16385),
        (29, 13, 24577),
    ];

    /// Emit a complete fixed-Huffman length+distance match.
    fn write_match(bw: &mut BitWriter, len: u32, dist: u32) {
        let (lsym, lebits, lbase) = *LEN_TABLE
            .iter()
            .rev()
            .find(|&&(_, _, base)| len >= base)
            .expect("valid match length 3..=258");
        write_litlen_sym(bw, lsym);
        if lebits > 0 {
            bw.write_bits(len - lbase, lebits);
        }
        let (dsym, debits, dbase) = *DIST_TABLE
            .iter()
            .rev()
            .find(|&&(_, _, base)| dist >= base)
            .expect("valid match distance 1..=32768");
        write_dist_sym(bw, dsym);
        if debits > 0 {
            bw.write_bits(dist - dbase, debits);
        }
    }

    /// Emit the end-of-block symbol (256, fixed 7-bit code `0000000`).
    fn write_eob(bw: &mut BitWriter) {
        write_litlen_sym(bw, 256);
    }

    /// Collect just the block-body symbols (no header) for the fast path.
    fn symbols_of(emit: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
        let mut bw = BitWriter::new();
        emit(&mut bw);
        bw.finish()
    }

    // ---------------------------------------------------------------------
    // Decoder-state setup & drivers
    // ---------------------------------------------------------------------

    /// Build an [`InflateState`] primed exactly as the C `fixed_tables()` leaves
    /// it: fixed literal/length and distance decode tables installed,
    /// `mode == Len`, empty accumulator, no window.
    ///
    /// The two fixed tables are constructed here via [`inflate_table`] from the
    /// canonical fixed code lengths (RFC 1951 §3.2.6), exactly as the C
    /// `makefixed()`/`fixed_tables()` routines do. This keeps the test's
    /// dependencies confined to `tables.rs` (a declared dependency of this file)
    /// rather than pulling in the precomputed `inffixed.h` tables, and it
    /// additionally exercises the real table-building path.
    fn fixed_state() -> Box<InflateState> {
        let mut state = InflateState::new(-15).expect("raw inflate state");

        let mut lens = [0u16; 320];
        let mut work = [0u16; 288];

        // Literal/length code lengths: 0..=143 -> 8, 144..=255 -> 9,
        // 256..=279 -> 7, 280..=287 -> 8. Root index = 9 bits.
        for (sym, slot) in lens.iter_mut().enumerate().take(288) {
            *slot = if sym < 144 {
                8
            } else if sym < 256 {
                9
            } else if sym < 280 {
                7
            } else {
                8
            };
        }
        let mut lenbits = 9usize;
        {
            let (_head, tail) = state.codes.split_at_mut(0);
            inflate_table(CodeType::Lens, &lens, 288, tail, &mut lenbits, &mut work)
                .expect("fixed literal/length table builds");
        }

        // Distance code lengths: all 32 codes are 5 bits. Root index = 5 bits.
        for slot in lens.iter_mut().take(32) {
            *slot = 5;
        }
        let mut distbits = 5usize;
        let distcode = 512usize;
        {
            let (_head, tail) = state.codes.split_at_mut(distcode);
            inflate_table(CodeType::Dists, &lens, 32, tail, &mut distbits, &mut work)
                .expect("fixed distance table builds");
        }

        state.lencode = 0;
        state.lenbits = lenbits;
        state.distcode = distcode;
        state.distbits = distbits;
        state.mode = InflateMode::Len;
        state.hold = 0;
        state.bits = 0;
        state.wsize = 0;
        state.whave = 0;
        state.wnext = 0;
        state.sane = true;
        state
    }

    /// Run [`inflate_fast`] over a symbols-only stream (the fast loop resumes at
    /// the first length code, so no block header is present). The input is
    /// padded so the `avail_in >= 6` entry contract holds, and the output
    /// buffer is generous so the loop terminates on the stream's end-of-block
    /// rather than on output space. Returns the produced output and the final
    /// input cursor.
    fn run_fast(state: &mut InflateState, symbols: &[u8]) -> (Vec<u8>, usize) {
        let mut input = symbols.to_vec();
        input.extend_from_slice(&[0u8; 32]);
        let mut output = vec![0u8; 1024];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        let start = output.len();
        inflate_fast(state, &input, &mut in_pos, &mut output, &mut out_pos, start);
        output.truncate(out_pos);
        (output, in_pos)
    }

    /// Decode a *complete* raw-DEFLATE stream (block header + symbols) through
    /// the independent per-code slow path ([`InflateState::inflate`]). It shares
    /// the fixed decode tables with the fast path but re-implements the decode
    /// loop, so byte-for-byte agreement is a strong correctness signal.
    fn run_slow(stream: &[u8]) -> Vec<u8> {
        let mut state = InflateState::new(-15).expect("raw inflate state");
        let mut output = vec![0u8; 4096];
        let res = state.inflate(stream, &mut output, Flush::Finish);
        assert!(
            matches!(res.status, Ok(ReturnCode::StreamEnd)),
            "slow-path oracle did not reach stream end: {:?}",
            res.status
        );
        output.truncate(res.produced);
        output
    }

    /// Decode the same logical fixed-Huffman block through both the fast path
    /// (symbols only) and the slow-path oracle (full final block), asserting
    /// both agree with each other and with the known-answer `expected`.
    fn assert_fast_and_slow(expected: &[u8], emit: impl Fn(&mut BitWriter)) {
        // Fast path: symbols only, resuming at the first length code.
        let fast_in = symbols_of(&emit);
        let mut state = fixed_state();
        let (fast_out, _) = run_fast(&mut state, &fast_in);
        assert_eq!(
            state.mode,
            InflateMode::Type,
            "fast path should stop at end-of-block"
        );
        assert_eq!(state.msg, None, "no error expected on the fast path");
        assert_eq!(fast_out, expected, "fast-path output mismatch");

        // Slow oracle: a complete final fixed block (BFINAL=1, BTYPE=01).
        let mut bws = BitWriter::new();
        bws.write_bits(0b011, 3);
        emit(&mut bws);
        let slow_out = run_slow(&bws.finish());
        assert_eq!(slow_out, expected, "slow-path oracle output mismatch");
    }

    /// Drive a single match (then end-of-block) against a fast state with a
    /// pre-populated window. The match is the first symbol, so `beg == out_pos`
    /// and the entire distance reaches into the window.
    fn run_window_match(
        wsize: usize,
        whave: usize,
        wnext: usize,
        window: Vec<u8>,
        len: u32,
        dist: u32,
    ) -> (Vec<u8>, InflateMode, Option<&'static str>) {
        let mut state = fixed_state();
        state.wsize = wsize;
        state.whave = whave;
        state.wnext = wnext;
        state.window = window;
        let stream = symbols_of(|bw| {
            write_match(bw, len, dist);
            write_eob(bw);
        });
        let (out, _) = run_fast(&mut state, &stream);
        (out, state.mode, state.msg)
    }

    /// A window filled with the ramp `0, 1, 2, …` (wrapping at 256).
    fn ramp(n: usize) -> Vec<u8> {
        (0..n).map(|i| i as u8).collect()
    }

    // ---------------------------------------------------------------------
    // Happy-path decode tests (fast vs slow vs known answer)
    // ---------------------------------------------------------------------

    #[test]
    fn literals_only() {
        let data = b"Hello, zlib-rs!";
        assert_fast_and_slow(data, |bw| {
            for &b in data {
                write_litlen_sym(bw, b as u32);
            }
            write_eob(bw);
        });
    }

    #[test]
    fn nine_bit_literals() {
        // Bytes >= 144 use the 9-bit fixed codes; verify that path too.
        let data = &[0u8, 200, 255, 144, 7, 250];
        assert_fast_and_slow(data, |bw| {
            for &b in data {
                write_litlen_sym(bw, b as u32);
            }
            write_eob(bw);
        });
    }

    #[test]
    fn overlapping_match_run_length() {
        // 'a' then a len=8 dist=1 match -> 9 'a's. This is the critical
        // overlapping (RLE) copy where `dist < len`. A forward byte-by-byte
        // copy is mandatory for bit-exact output; a bulk memmove would corrupt
        // it.
        let expected = vec![b'a'; 9];
        assert_fast_and_slow(&expected, |bw| {
            write_litlen_sym(bw, b'a' as u32);
            write_match(bw, 8, 1);
            write_eob(bw);
        });
    }

    #[test]
    fn overlapping_match_period_two() {
        // "ab" then len=6 dist=2 -> "abababab" (period-2 overlap).
        let expected = b"abababab";
        assert_fast_and_slow(expected, |bw| {
            write_litlen_sym(bw, b'a' as u32);
            write_litlen_sym(bw, b'b' as u32);
            write_match(bw, 6, 2);
            write_eob(bw);
        });
    }

    #[test]
    fn non_overlapping_match() {
        // "abc" then len=3 dist=3 -> "abcabc".
        let expected = b"abcabc";
        assert_fast_and_slow(expected, |bw| {
            for &b in b"abc" {
                write_litlen_sym(bw, b as u32);
            }
            write_match(bw, 3, 3);
            write_eob(bw);
        });
    }

    #[test]
    fn length_with_extra_bits() {
        // 'x' then len=11 dist=1 -> 12 'x'. Length 11 is symbol 265 + 1 extra.
        let expected = vec![b'x'; 12];
        assert_fast_and_slow(&expected, |bw| {
            write_litlen_sym(bw, b'x' as u32);
            write_match(bw, 11, 1);
            write_eob(bw);
        });
    }

    #[test]
    fn larger_length_extra_bits() {
        // 'z' then len=131 dist=1 -> 132 'z'. Length 131 is symbol 281 + 5
        // extra bits, exercising a wider extra-bit read.
        let expected = vec![b'z'; 132];
        assert_fast_and_slow(&expected, |bw| {
            write_litlen_sym(bw, b'z' as u32);
            write_match(bw, 131, 1);
            write_eob(bw);
        });
    }

    #[test]
    fn max_length_match() {
        // 'q' then len=258 dist=1 -> 259 'q'. Length 258 is the special symbol
        // 285 (0 extra) and the maximum match length.
        let expected = vec![b'q'; 259];
        assert_fast_and_slow(&expected, |bw| {
            write_litlen_sym(bw, b'q' as u32);
            write_match(bw, 258, 1);
            write_eob(bw);
        });
    }

    #[test]
    fn distance_with_extra_bits() {
        // 20 distinct literals, then len=4 dist=10 copies bytes [10..14].
        // Distance 10 is symbol 6 + 2 extra bits.
        let lits: Vec<u8> = (0u8..20).map(|i| b'A' + i).collect();
        let mut expected = lits.clone();
        let from = lits.len() - 10;
        for k in 0..4 {
            expected.push(expected[from + k]);
        }
        assert_fast_and_slow(&expected, |bw| {
            for &b in &lits {
                write_litlen_sym(bw, b as u32);
            }
            write_match(bw, 4, 10);
            write_eob(bw);
        });
    }

    #[test]
    fn mixed_literals_and_matches() {
        // A longer mixed block stresses the loop tail condition and multiple
        // match copies in sequence.
        let mut expected = Vec::new();
        expected.extend_from_slice(b"abcd");
        expected.extend_from_slice(b"abcd"); // match len=4 dist=4
        expected.extend_from_slice(&[b'e'; 8]); // 'e' literal + RLE run len=7 dist=1
        assert_fast_and_slow(&expected, |bw| {
            for &b in b"abcd" {
                write_litlen_sym(bw, b as u32);
            }
            write_match(bw, 4, 4);
            write_litlen_sym(bw, b'e' as u32);
            write_match(bw, 7, 1);
            write_eob(bw);
        });
    }

    // ---------------------------------------------------------------------
    // Window-copy tests
    //
    // These use hand-computed known answers: the slow path cannot easily replay
    // a pre-populated window through its public entry point. The expected
    // values follow directly from LZ77 semantics and the three C window cases
    // (inffast.c L197-L236). All matches are the first symbol, so `beg` equals
    // the start position and the whole distance reaches into the window.
    // ---------------------------------------------------------------------

    #[test]
    fn window_wnext_zero_all_from_window() {
        // wnext == 0, op_w(8) >= len(5): copy 5 bytes from window[wsize-8 ..].
        let (out, mode, msg) = run_window_match(32, 32, 0, ramp(32), 5, 8);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(msg, None);
        assert_eq!(out, vec![24, 25, 26, 27, 28]);
    }

    #[test]
    fn window_wnext_zero_rollover_to_output() {
        // wnext == 0, op_w(4) < len(6): 4 bytes from window[28..32], then 2 from
        // freshly produced output (the distance rolls over into new output).
        let (out, mode, _) = run_window_match(32, 32, 0, ramp(32), 6, 4);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![28, 29, 30, 31, 28, 29]);
    }

    #[test]
    fn window_contiguous_all_from_window() {
        // wnext(20) >= op_w(5) and op_w >= len(3): 3 bytes from window[15..18].
        let (out, mode, _) = run_window_match(32, 20, 20, ramp(32), 3, 5);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![15, 16, 17]);
    }

    #[test]
    fn window_contiguous_rollover_to_output() {
        // wnext(20) >= op_w(3), op_w < len(5): 3 from window[17..20], 2 from
        // output.
        let (out, mode, _) = run_window_match(32, 20, 20, ramp(32), 5, 3);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![17, 18, 19, 17, 18]);
    }

    #[test]
    fn window_wraparound_end_only() {
        // wnext(10) < op_w(15), op2(5) >= len(4): 4 bytes from the end of the
        // window, window[27..31].
        let (out, mode, _) = run_window_match(32, 32, 10, ramp(32), 4, 15);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![27, 28, 29, 30]);
    }

    #[test]
    fn window_wraparound_into_start() {
        // wnext(10) < op_w(15), op2(5) < len(12), wnext(10) >= remaining(7):
        // 5 from the window end [27..32], then 7 from the window start [0..7].
        let (out, mode, _) = run_window_match(32, 32, 10, ramp(32), 12, 15);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, vec![27, 28, 29, 30, 31, 0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn window_wraparound_into_output() {
        // wnext(3) < op_w(10), op2(7) < len(15), wnext(3) < remaining(8):
        // 7 from the window end [25..32], 3 from the window start [0..3], then
        // 5 from freshly produced output.
        let (out, mode, _) = run_window_match(32, 32, 3, ramp(32), 15, 10);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(
            out,
            vec![25, 26, 27, 28, 29, 30, 31, 0, 1, 2, 25, 26, 27, 28, 29]
        );
    }

    // ---------------------------------------------------------------------
    // Error paths
    // ---------------------------------------------------------------------

    #[test]
    fn invalid_distance_too_far_back() {
        // whave(4) < op_w(10): the match reaches before the start of available
        // history. With `sane` set (the default), this is a hard error.
        let (out, mode, msg) = run_window_match(32, 4, 0, ramp(32), 5, 10);
        assert_eq!(mode, InflateMode::Bad);
        assert_eq!(msg, Some("invalid distance too far back"));
        assert!(out.is_empty());
    }

    /// Find a first-level table index (within `count` slots starting at
    /// `base` in the built `codes` arena) whose entry is marked invalid: bit
    /// `0x40` set, with neither `0x10` (a length/distance base) nor `0x20`
    /// (end-of-block) set.
    fn first_invalid_index(state: &InflateState, base: usize, count: usize) -> usize {
        (0..count)
            .find(|&i| {
                let op = state.codes[base + i].op;
                op & 16 == 0 && op & 32 == 0 && op & 64 != 0
            })
            .expect("table has an invalid entry")
    }

    #[test]
    fn invalid_distance_code() {
        // A first-level distance index whose entry is marked invalid. A valid
        // length code first takes the decoder into the distance lookup.
        let mut state = fixed_state();
        let bad = first_invalid_index(&state, state.distcode, 32);
        let stream = symbols_of(|bw| {
            write_litlen_sym(bw, 257); // length 3, 0 extra bits
            bw.write_bits(bad as u32, 5); // 5-bit distance index -> invalid entry
        });
        let (out, _) = run_fast(&mut state, &stream);
        assert_eq!(state.mode, InflateMode::Bad);
        assert_eq!(state.msg, Some("invalid distance code"));
        assert!(out.is_empty());
    }

    #[test]
    fn invalid_literal_length_code() {
        // A first-level litlen index whose entry is marked invalid.
        let mut state = fixed_state();
        let bad = first_invalid_index(&state, state.lencode, 512);
        let stream = symbols_of(|bw| {
            bw.write_bits(bad as u32, 9); // 9-bit litlen index -> invalid entry
        });
        let (out, _) = run_fast(&mut state, &stream);
        assert_eq!(state.mode, InflateMode::Bad);
        assert_eq!(state.msg, Some("invalid literal/length code"));
        assert!(out.is_empty());
    }

    // ---------------------------------------------------------------------
    // Second-level (sub-table) decode
    // ---------------------------------------------------------------------

    /// RFC 1951 §3.2.2 canonical Huffman code assignment from code lengths.
    fn canonical_codes(lens: &[u16]) -> Vec<u32> {
        let max = *lens.iter().max().unwrap() as usize;
        let mut bl_count = vec![0u32; max + 1];
        for &l in lens {
            if l != 0 {
                bl_count[l as usize] += 1;
            }
        }
        let mut next = vec![0u32; max + 2];
        let mut code = 0u32;
        for bits in 1..=max {
            code = (code + bl_count[bits - 1]) << 1;
            next[bits] = code;
        }
        let mut codes = vec![0u32; lens.len()];
        for (sym, &l) in lens.iter().enumerate() {
            if l != 0 {
                codes[sym] = next[l as usize];
                next[l as usize] += 1;
            }
        }
        codes
    }

    #[test]
    fn second_level_distance_table() {
        // Build a custom distance table whose longest code (7 bits) exceeds the
        // 6-bit root, forcing a second-level sub-table. Decoding a 7-bit
        // distance code then exercises the `dodist` sub-table re-lookup
        // (inffast.c L264-L267).
        let lens: [u16; 8] = [1, 2, 3, 4, 5, 6, 7, 7];
        let codes = canonical_codes(&lens);

        let mut state = fixed_state();
        // Literals stay on the fixed table (LENFIX at offset 0); build the
        // custom distance table at offset 600 in the shared `codes` arena.
        let dist_off = 600usize;
        let mut distbits = 6usize;
        let mut work = [0u16; 288];
        let used = {
            // inflate_table fills relative to the slice start (== dist_off), and
            // sub-table links store `val` as an offset from that same base.
            let (_head, tail) = state.codes.split_at_mut(dist_off);
            inflate_table(
                CodeType::Dists,
                &lens,
                lens.len(),
                tail,
                &mut distbits,
                &mut work,
            )
            .expect("custom distance table builds")
        };
        assert!(used > 0, "table consumed entries");
        assert!(distbits < 7, "root must be smaller than the longest code");
        state.distcode = dist_off;
        state.distbits = distbits;

        // Distance symbol 6 -> base distance 9 with 2 extra bits; its 7-bit code
        // lands in the second-level table. 20 literals precede the match so the
        // copy comes from already-produced output.
        let lits: Vec<u8> = (0u8..20).map(|i| b'A' + i).collect();
        let dist = 9usize;
        let len = 4usize;
        let mut expected = lits.clone();
        let from = lits.len() - dist;
        for k in 0..len {
            expected.push(expected[from + k]);
        }

        let stream = symbols_of(|bw| {
            for &b in &lits {
                write_litlen_sym(bw, b as u32);
            }
            write_litlen_sym(bw, 258); // length 4 (0 extra)
            bw.write_huff(codes[6], lens[6] as u32); // custom 7-bit distance code
            bw.write_bits(0, 2); // 2 distance extra bits -> base distance 9
            write_eob(bw);
        });

        let (out, _) = run_fast(&mut state, &stream);
        assert_eq!(state.mode, InflateMode::Type);
        assert_eq!(state.msg, None);
        assert_eq!(out, expected);
    }

    // ---------------------------------------------------------------------
    // Exit bookkeeping (Phase D)
    // ---------------------------------------------------------------------

    #[test]
    fn returns_whole_unused_input_bytes() {
        // After decoding, the fast path hands whole unused bytes back to the
        // input cursor (only a sub-byte remainder stays in the accumulator).
        // A short literals+EOB block leaves the 32 padding bytes (and any whole
        // bytes still buffered) unconsumed.
        let data = b"abcdef";
        let stream = symbols_of(|bw| {
            for &b in data {
                write_litlen_sym(bw, b as u32);
            }
            write_eob(bw);
        });
        let mut state = fixed_state();
        let total_in = stream.len() + 32; // run_fast pads with 32 bytes
        let (out, in_pos) = run_fast(&mut state, &stream);
        assert_eq!(out, data);
        assert_eq!(state.mode, InflateMode::Type);
        // The consumed prefix must be a whole number of bytes and cannot exceed
        // the bytes actually provided; the accumulator keeps only sub-byte bits.
        assert!(in_pos <= total_in);
        assert!(state.bits < 8, "only a sub-byte remainder stays buffered");
    }
}
