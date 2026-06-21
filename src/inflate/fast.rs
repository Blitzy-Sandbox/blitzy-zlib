//! The `inflate_fast` hot loop — the performance-critical inner decode routine.
//!
//! This module is the safe-Rust port of zlib's `inffast.c`. It decodes literal,
//! length, and distance codes and writes the resulting literal and match bytes
//! until either not enough input or output is available, an end-of-block is
//! encountered, or a data error is encountered. When inflate is supplied with
//! ample buffers (e.g. a 16 KiB input and a 64 KiB output buffer), **more than
//! 95 % of inflate's execution time is spent here**, which is why this is the
//! one routine where eliding bounds checks is worthwhile.
//!
//! # The sole `unsafe` site in the inflate core
//!
//! Per AAP §0.6.2 and §0.7.2, `unsafe` is permitted in the *core* only in this
//! file (and, separately, at the FFI boundary in `src/ffi.rs`). Every other
//! `src/inflate/*.rs` module is 100 % safe Rust. Following the folder-spec rule
//! "prefer safe slicing; `unsafe` only for proven-bounded hot copies", the
//! baseline here is written with **safe** slice operations:
//!
//! * table lookups (`lcode[..]` / `dcode[..]`) use safe indexing — the indices
//!   are masked (`& lmask` / `& dmask`) within the table size, and the lookups
//!   are not the bottleneck;
//! * input refills use safe indexing — the entry precondition `avail_in >= 6`
//!   together with the proven "at most 6 input bytes per iteration" bound means
//!   every refill read is in bounds;
//! * the single literal store uses safe indexing — `out_pos < output.len()`
//!   always holds at that point;
//! * window-to-output copies use the safe, memcpy-backed
//!   [`slice::copy_from_slice`] (the window and the output are disjoint
//!   buffers, so the runs are provably non-overlapping).
//!
//! `unsafe` is confined to exactly **one** primitive — `copy_overlapping` —
//! used for the LZ77 *overlapping* output run-copy (e.g. `dist == 1`, which
//! repeats the previous byte). That copy must be byte-by-byte (a bulk copy
//! would not reproduce the run-length semantics), and it is the hottest inner
//! loop, so it elides bounds checks via [`slice::get_unchecked`] /
//! [`slice::get_unchecked_mut`]. The single `unsafe` block carries a
//! `// SAFETY:` proof and is preceded by `debug_assert!`s encoding the same
//! bounds (AAP §0.7.2).
//!
//! # Byte-identical decoding
//!
//! The port mirrors `inffast.c` statement-for-statement so that the decoded
//! output, the bit-refill cadence (2 bytes when `bits < 15`, 1 byte for the
//! length extra, up to 2 bytes for the distance extra), and the window-copy
//! edge cases are **byte-identical** to C. The unrolled "copy three at a time"
//! loops in C are replaced by plain copies because the unrolling is purely a
//! micro-optimisation and never changes which bytes are written.
//!
//! # `no_std`
//!
//! This module uses only `core` slice operations and primitive integer
//! arithmetic — no `alloc`, no `std`. It compiles unchanged under
//! `--no-default-features --features no-std`.
//!
//! # Control-flow translation (`goto` → labeled loops)
//!
//! C uses `goto dolen` / `goto dodist` to re-enter the decode logic for a
//! second-level (sub-table) code. Those become labeled loops here: a
//! second-level code does `continue 'dolen` / `continue 'dodist` (re-running
//! the block with the linked entry), while a terminal code does `break 'dolen`
//! (a literal or a completed match falls through to the loop tail) or
//! `break 'inflate_fast` (end-of-block / data error exits the outer loop). The
//! C `do { … } while (in < last && out < end)` becomes a `loop { … }` whose
//! continuation is tested at the bottom, preserving the execute-body-once
//! semantics.

use crate::inflate::state::{InflateMode, InflateState};
use crate::inflate::tables::Code;

/// Copy `len` bytes within `output`, from `src` forward to `dst`, one byte at a
/// time, where the source and destination ranges may **overlap**.
///
/// This is the LZ77 run-length primitive: when `dst - src` (the match distance)
/// is smaller than `len`, bytes written earlier in this call are re-read later,
/// so e.g. a distance of `1` replicates the byte at `src` across the whole run.
/// A bulk copy ([`slice::copy_from_slice`] / [`slice::copy_within`]) must **not**
/// be used here because it would copy the original source bytes rather than the
/// progressively-written ones, corrupting run-length matches.
///
/// # Invariants (guaranteed by every call site)
///
/// * `src < dst` — the match distance is at least 1, so the read cursor always
///   trails the write cursor.
/// * `dst + len <= output.len()` — `inflate_fast` is entered with
///   `avail_out >= 258` and a single length/distance pair emits at most 258
///   bytes, so the entire match fits in the output buffer.
///
/// From `src < dst` it follows that `src + len < dst + len <= output.len()`, so
/// both the reads (at `src .. src + len`) and the writes (at `dst .. dst + len`)
/// are in bounds for every iteration.
#[inline]
fn copy_overlapping(output: &mut [u8], dst: usize, src: usize, len: usize) {
    debug_assert!(
        src < dst,
        "copy_overlapping requires src < dst (distance >= 1)"
    );
    debug_assert!(
        dst.checked_add(len).is_some_and(|end| end <= output.len()),
        "copy_overlapping write range out of bounds"
    );
    // src + len <= dst + len <= output.len() because src < dst, so reads are in
    // bounds too; assert it explicitly to document the second half of the proof.
    debug_assert!(
        src.checked_add(len).is_some_and(|end| end <= output.len()),
        "copy_overlapping read range out of bounds"
    );

    for i in 0..len {
        // SAFETY: The debug_assert!s above prove `dst + len <= output.len()` and
        // `src + len <= output.len()`. For every `i` in `0..len`, both `src + i`
        // and `dst + i` are therefore strictly less than `output.len()`, so the
        // read at `src + i` and the write at `dst + i` are always in bounds.
        // Iterating `i` upward with the read performed *before* the write
        // reproduces zlib's forward, overlapping LZ77 run-length copy
        // (`*out++ = *from++`) exactly: when the match distance `dst - src` is
        // smaller than `len`, bytes written earlier in this loop are re-read on
        // later iterations, which is the intended run-length behaviour.
        unsafe {
            let byte = *output.get_unchecked(src + i);
            *output.get_unchecked_mut(dst + i) = byte;
        }
    }
}

/// The specific data-error condition detected on the [`inflate_fast`] hot path.
///
/// `inflate_fast` runs with no access to the stream's `msg` field (its
/// decoupled slice signature deliberately omits it), so when it sets
/// [`InflateMode::Bad`](crate::inflate::state::InflateMode::Bad) it returns one
/// of these reasons instead of `()`. The caller (`inflate` in `mod.rs`) maps
/// the reason to the corresponding fixed zlib message via [`FastBad::message`]
/// before returning `Z_DATA_ERROR`, so a malformed stream decoded on the fast
/// path receives exactly the same `msg` text as one decoded on the slow path
/// (matching C `inffast.c`, which writes `strm->msg` directly at each site).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FastBad {
    /// An invalid literal/length code was decoded (C `inffast.c`: "invalid
    /// literal/length code").
    InvalidLitLen,
    /// An invalid distance code was decoded (C `inffast.c`: "invalid distance
    /// code").
    InvalidDist,
    /// A match referenced a distance further back than the available history
    /// (C `inffast.c`: "invalid distance too far back").
    DistTooFar,
}

impl FastBad {
    /// The fixed zlib `Z_DATA_ERROR` message text for this condition, identical
    /// to the string C `inffast.c` assigns to `strm->msg` at the corresponding
    /// site (and to the matching slow-path messages in `inflate/mod.rs`).
    pub(crate) const fn message(self) -> &'static str {
        match self {
            FastBad::InvalidLitLen => "invalid literal/length code",
            FastBad::InvalidDist => "invalid distance code",
            FastBad::DistTooFar => "invalid distance too far back",
        }
    }
}

/// Decode literals and length/distance matches until end-of-block or until
/// fewer than 6 input bytes / 258 output bytes remain. Faithful port of C
/// `inflate_fast` (`inffast.c`).
///
/// # Slice/cursor model
///
/// This signature decouples the routine from `ZStream` internals (AAP §5):
///
/// * `input` — the input bytes available this call (C `strm->next_in`);
///   `*in_pos` is the consumed cursor (C `in`).
/// * `output` — the output buffer **beginning at this `inflate()` call's
///   initial `next_out`**, so C's `beg` is offset `0`, C's `out - beg` is
///   `*out_pos`, and C's `end` is `output.len() - 257`. `*out_pos` is the
///   current write offset (C `out`).
/// * `state` — the decode state; on entry `state.mode == Len`, and `hold` /
///   `bits` carry the bit accumulator across calls.
///
/// The C `start` parameter is unnecessary: because `output` is referenced to
/// the call's initial `next_out`, the `beg` bookkeeping reduces to the `out_pos`
/// offset (C `out - beg == *out_pos`, and `start - avail_out == *out_pos` as
/// well, since `start` is the initial `avail_out` and the buffer only grows).
///
/// # Entry preconditions (asserted)
///
/// The caller (`inflate` in `mod.rs`, or `back.rs`) must guarantee, exactly as
/// in C (`inffast.c` lines 23-29):
///
/// * `state.mode == Len`,
/// * `avail_in >= 6` (i.e. `input.len() - *in_pos >= 6`),
/// * `avail_out >= 258` (i.e. `output.len() - *out_pos >= 258`),
/// * `state.bits < 8`.
///
/// A length/distance pair consumes at most 48 bits = 6 bytes (15 length-code +
/// 5 length-extra + 15 distance-code + 13 distance-extra), so `avail_in >= 6`
/// lets the loop skip input-availability checks; and a pair emits at most 258
/// bytes, so `avail_out >= 258` lets it skip output-space checks.
///
/// # On return
///
/// `*in_pos` / `*out_pos`, `state.hold`, and `state.bits` are updated, and
/// `state.mode` is set to one of:
///
/// * [`Len`](InflateMode::Len) — ran out of input or output space (the caller
///   re-enters later);
/// * [`Type`](InflateMode::Type) — reached an end-of-block code;
/// * [`Bad`](InflateMode::Bad) — a data error (invalid literal/length code,
///   invalid distance code, or distance too far back).
///
/// This routine never returns a `Z_*` code; it only advances the cursors and
/// sets `state.mode`, which the caller interprets. It returns
/// [`Some`]`(`[`FastBad`]`)` **iff** it set [`Bad`](InflateMode::Bad), naming the
/// specific data-error condition; the caller assigns the corresponding fixed
/// `Z_DATA_ERROR` message ([`FastBad::message`]) before returning the error, so
/// fast-path and slow-path malformed streams produce identical `msg` text. On
/// any non-`Bad` exit it returns [`None`]. (The decoupled slice signature
/// deliberately gives this routine no access to the stream's `msg` field, hence
/// the returned reason rather than a direct `msg` write as in C `inffast.c`.)
///
/// The caller recomputes `avail_in = input.len() - *in_pos` and
/// `avail_out = output.len() - *out_pos`; these are equivalent to C's
/// `in < last ? 5 + (last - in) : 5 - (in - last)` and
/// `out < end ? 257 + (end - out) : 257 - (out - end)` formulas (C measures the
/// counts relative to the `last`/`end` loop bounds, which are exactly
/// `input.len() - 5` and `output.len() - 257`; substituting gives the slice
/// form used here).
pub(crate) fn inflate_fast(
    input: &[u8],
    in_pos: &mut usize,
    output: &mut [u8],
    out_pos: &mut usize,
    state: &mut InflateState,
) -> Option<FastBad> {
    // ---- Entry preconditions (debug-only; the caller guarantees them). -------
    debug_assert_eq!(
        state.mode,
        InflateMode::Len,
        "inflate_fast entry: state.mode must be Len"
    );
    debug_assert!(state.bits < 8, "inflate_fast entry: state.bits must be < 8");
    debug_assert!(
        input.len() - *in_pos >= 6,
        "inflate_fast entry: avail_in must be >= 6"
    );
    debug_assert!(
        output.len() - *out_pos >= 258,
        "inflate_fast entry: avail_out must be >= 258"
    );

    // ---- Phase A: copy state into locals (C lines 78-95). --------------------
    // The bit accumulator and cursors live in locals for the duration of the
    // loop and are written back once at the end. `hold`/`bits` are `u32`,
    // matching `InflateState` and C's portable (32-bit) `unsigned long` path.
    let mut hold: u32 = state.hold;
    let mut bits: u32 = state.bits;
    let mut ip: usize = *in_pos;
    let mut opos: usize = *out_pos;

    // Loop bounds (C `last` / `end`), expressed as offsets:
    //   C `last = in + (avail_in - 5)`  -> input.len() - 5
    //   C `end  = out + (avail_out - 257)` -> output.len() - 257
    // The entry preconditions (avail_in >= 6, avail_out >= 258) guarantee these
    // subtractions do not underflow.
    let last_off: usize = input.len() - 5;
    let end_off: usize = output.len() - 257;

    // Result mode. On a normal exit (ran out of input/output) C leaves
    // `state.mode` unchanged at `Len`; we mirror that by defaulting to `Len`
    // and overwriting only for end-of-block (`Type`) or a data error (`Bad`).
    let result_mode: InflateMode;
    // The specific data-error reason, set iff `result_mode` becomes `Bad`. The
    // caller turns this into the matching fixed zlib `msg` (see `FastBad`).
    let bad_reason: Option<FastBad>;

    // Borrow the immutable decode inputs (root tables + window) for the loop.
    // These borrows end before `state` is written back below.
    {
        let lmask: u32 = (1u32 << state.lenbits) - 1;
        let dmask: u32 = (1u32 << state.distbits) - 1;
        let wsize: usize = state.wsize as usize;
        let whave: usize = state.whave as usize;
        let wnext: usize = state.wnext as usize;
        let sane: bool = state.sane;
        // Root tables, sliced at their start indices (C `lcode` / `dcode`).
        let lcode: &[Code] = &state.codes[state.lencode..];
        let dcode: &[Code] = &state.codes[state.distcode..];
        // The sliding window (C `window`). Read-only here.
        let window: &[u8] = &state.window;

        let mut mode = InflateMode::Len;
        // Tracks the data-error reason alongside `mode`; set only when `mode`
        // is set to `Bad`, so the caller can assign the exact zlib message.
        let mut bad: Option<FastBad> = None;

        // ---- Phase B: main decode loop (C `do { … } while (…)`). -------------
        'inflate_fast: loop {
            // Refill: ensure at least 15 bits are available for the next code.
            // (2 bytes, LSB-first — DEFLATE bit order, AAP §0.6.4.)
            if bits < 15 {
                hold |= (input[ip] as u32) << bits;
                ip += 1;
                bits += 8;
                hold |= (input[ip] as u32) << bits;
                ip += 1;
                bits += 8;
            }

            // First length/literal table lookup (C `here = lcode + (hold & lmask)`).
            let mut here: Code = lcode[(hold & lmask) as usize];

            'dolen: loop {
                // Drop the bits this table level consumed, then inspect `op`.
                let mut op = here.bits as u32;
                hold >>= op;
                bits -= op;
                op = here.op as u32;

                if op == 0 {
                    // Literal: emit the byte. `opos < output.len()` holds (the
                    // iteration began with avail_out >= 258), so safe indexing
                    // never panics here.
                    output[opos] = here.val as u8;
                    opos += 1;
                    break 'dolen;
                } else if op & 16 != 0 {
                    // Length base. `val` is the base length; the low 4 bits of
                    // `op` are the number of extra bits to add.
                    let mut len = here.val as usize;
                    op &= 15;
                    if op != 0 {
                        if bits < op {
                            hold |= (input[ip] as u32) << bits;
                            ip += 1;
                            bits += 8;
                        }
                        len += (hold & ((1u32 << op) - 1)) as usize;
                        hold >>= op;
                        bits -= op;
                    }

                    // Refill before the distance code (2 bytes when bits < 15).
                    if bits < 15 {
                        hold |= (input[ip] as u32) << bits;
                        ip += 1;
                        bits += 8;
                        hold |= (input[ip] as u32) << bits;
                        ip += 1;
                        bits += 8;
                    }

                    // Distance table lookup (C `here = dcode + (hold & dmask)`).
                    here = dcode[(hold & dmask) as usize];

                    'dodist: loop {
                        let mut op = here.bits as u32;
                        hold >>= op;
                        bits -= op;
                        op = here.op as u32;

                        if op & 16 != 0 {
                            // Distance base. `val` is the base distance; the low
                            // 4 bits of `op` are the number of extra bits.
                            let mut dist = here.val as u32;
                            op &= 15;
                            if bits < op {
                                hold |= (input[ip] as u32) << bits;
                                ip += 1;
                                bits += 8;
                                if bits < op {
                                    hold |= (input[ip] as u32) << bits;
                                    ip += 1;
                                    bits += 8;
                                }
                            }
                            dist += hold & ((1u32 << op) - 1);
                            // NOTE: the INFLATE_STRICT `dist > dmax` check is
                            // omitted — it is not compiled in the default zlib
                            // build this crate targets (AAP §0.6.7).
                            hold >>= op;
                            bits -= op;

                            let dist = dist as usize;
                            // C `op = out - beg` — bytes already produced in this
                            // call and therefore copyable directly from `output`.
                            if dist > opos {
                                // Part (or all) of the match lies in the window.
                                let wop = dist - opos; // distance back into window
                                if wop > whave {
                                    // "invalid distance too far back". In the
                                    // default build `sane` is always true, so
                                    // this is always an error; the
                                    // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR
                                    // fall-through is not compiled (AAP §0.6.7).
                                    if sane {
                                        bad = Some(FastBad::DistTooFar);
                                        mode = InflateMode::Bad;
                                        break 'inflate_fast;
                                    }
                                }

                                // Where the remaining bytes come from after the
                                // window portion, and whether that source is the
                                // window (`from_window`) or the output buffer.
                                let from: usize;
                                let from_window: bool;

                                if wnext == 0 {
                                    // Very common case: window has not wrapped;
                                    // the valid bytes end at `wsize`.
                                    let fidx = wsize - wop;
                                    if wop < len {
                                        // Some bytes from the window, the rest
                                        // from the output buffer.
                                        debug_assert!(opos + wop <= output.len());
                                        debug_assert!(fidx + wop <= window.len());
                                        output[opos..opos + wop]
                                            .copy_from_slice(&window[fidx..fidx + wop]);
                                        opos += wop;
                                        len -= wop;
                                        from = opos - dist; // rest from output
                                        from_window = false;
                                    } else {
                                        // The whole match fits in the window.
                                        from = fidx;
                                        from_window = true;
                                    }
                                } else if wnext < wop {
                                    // Window has wrapped and the match reaches
                                    // past the wrap point.
                                    let fidx = wsize + wnext - wop;
                                    let endbytes = wop - wnext; // bytes at window end
                                    if endbytes < len {
                                        debug_assert!(opos + endbytes <= output.len());
                                        debug_assert!(fidx + endbytes <= window.len());
                                        output[opos..opos + endbytes]
                                            .copy_from_slice(&window[fidx..fidx + endbytes]);
                                        opos += endbytes;
                                        len -= endbytes;
                                        if wnext < len {
                                            // Some bytes from the window start,
                                            // the rest from the output buffer.
                                            debug_assert!(opos + wnext <= output.len());
                                            debug_assert!(wnext <= window.len());
                                            output[opos..opos + wnext]
                                                .copy_from_slice(&window[0..wnext]);
                                            opos += wnext;
                                            len -= wnext;
                                            from = opos - dist; // rest from output
                                            from_window = false;
                                        } else {
                                            // Remaining match fits at the window
                                            // start.
                                            from = 0;
                                            from_window = true;
                                        }
                                    } else {
                                        // The whole match fits at the window end.
                                        from = fidx;
                                        from_window = true;
                                    }
                                } else {
                                    // Window has wrapped but the match is
                                    // contiguous within it.
                                    let fidx = wnext - wop;
                                    if wop < len {
                                        debug_assert!(opos + wop <= output.len());
                                        debug_assert!(fidx + wop <= window.len());
                                        output[opos..opos + wop]
                                            .copy_from_slice(&window[fidx..fidx + wop]);
                                        opos += wop;
                                        len -= wop;
                                        from = opos - dist; // rest from output
                                        from_window = false;
                                    } else {
                                        from = fidx;
                                        from_window = true;
                                    }
                                }

                                // Copy the remaining `len` bytes. If they are
                                // still in the window they are disjoint from the
                                // output (safe bulk copy); otherwise they come
                                // from earlier output and may overlap.
                                if from_window {
                                    debug_assert!(opos + len <= output.len());
                                    debug_assert!(from + len <= window.len());
                                    output[opos..opos + len]
                                        .copy_from_slice(&window[from..from + len]);
                                    opos += len;
                                } else {
                                    copy_overlapping(output, opos, from, len);
                                    opos += len;
                                }
                            } else {
                                // Copy directly from the output buffer; the run
                                // may overlap for short distances (e.g. dist==1).
                                let from = opos - dist;
                                copy_overlapping(output, opos, from, len);
                                opos += len;
                            }

                            break 'dolen;
                        } else if op & 64 == 0 {
                            // Second-level distance code: follow the sub-table
                            // link and re-decode (C `goto dodist`).
                            here = dcode[(here.val as u32 + (hold & ((1u32 << op) - 1))) as usize];
                            continue 'dodist;
                        } else {
                            // Invalid distance code.
                            bad = Some(FastBad::InvalidDist);
                            mode = InflateMode::Bad;
                            break 'inflate_fast;
                        }
                    }
                } else if op & 64 == 0 {
                    // Second-level length code: follow the sub-table link and
                    // re-decode (C `goto dolen`).
                    here = lcode[(here.val as u32 + (hold & ((1u32 << op) - 1))) as usize];
                    continue 'dolen;
                } else if op & 32 != 0 {
                    // End of block.
                    mode = InflateMode::Type;
                    break 'inflate_fast;
                } else {
                    // Invalid literal/length code.
                    bad = Some(FastBad::InvalidLitLen);
                    mode = InflateMode::Bad;
                    break 'inflate_fast;
                }
            }

            // Loop continuation (the `while (in < last && out < end)` of the C
            // `do`/`while`). Evaluated after the body, preserving do-while
            // semantics.
            if !(ip < last_off && opos < end_off) {
                break 'inflate_fast;
            }
        }

        result_mode = mode;
        bad_reason = bad;
    }

    // ---- Phase F: return unused whole bytes to the input (C lines 290-303). --
    // On entry `bits < 8`, so `ip` cannot move back before the caller's start.
    let rem = bits >> 3; // number of whole unused bytes held in the accumulator
    ip -= rem as usize;
    bits -= rem << 3; // now 0..=7
    hold &= (1u32 << bits) - 1; // keep only the still-valid leftover bits

    // ---- Write the locals back into the stream/state. ------------------------
    *in_pos = ip;
    *out_pos = opos;
    state.hold = hold;
    state.bits = bits;
    state.mode = result_mode;

    // Return the data-error reason (if any) so the caller can assign the
    // matching fixed zlib message before returning `Z_DATA_ERROR`.
    bad_reason
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::inflate::tables::{CodeType, inflate_table};

    // ---- DEFLATE length/distance base + extra-bit tables (RFC 1951 §3.2.5). --
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u32; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u32; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    /// Map a match length (3..=258) to its (symbol, extra value, extra bits).
    fn length_sym(len: u16) -> (usize, u32, u32) {
        let mut i = 0;
        while i + 1 < LENGTH_BASE.len() && LENGTH_BASE[i + 1] <= len {
            i += 1;
        }
        (257 + i, (len - LENGTH_BASE[i]) as u32, LENGTH_EXTRA[i])
    }

    /// Map a match distance (1..=32768) to its (symbol, extra value, extra bits).
    fn dist_sym(dist: u16) -> (usize, u32, u32) {
        let mut i = 0;
        while i + 1 < DIST_BASE.len() && DIST_BASE[i + 1] <= dist {
            i += 1;
        }
        (i, (dist - DIST_BASE[i]) as u32, DIST_EXTRA[i])
    }

    /// The canonical Huffman code (MSB-first `code`, bit-length `len`) for every
    /// symbol, derived from the per-symbol code lengths exactly per RFC 1951
    /// §3.2.2. This is the *same* canonical assignment that
    /// `inflate_table` decodes, so a stream encoded with these codes is
    /// decoded correctly by tables built from the identical lengths.
    fn canonical_codes(lens: &[u16]) -> Vec<(u32, u32)> {
        const MAXBITS: usize = 15;
        let mut bl_count = [0u32; MAXBITS + 1];
        for &l in lens {
            bl_count[l as usize] += 1;
        }
        bl_count[0] = 0;
        let mut next_code = [0u32; MAXBITS + 1];
        let mut code = 0u32;
        for bits in 1..=MAXBITS {
            code = (code + bl_count[bits - 1]) << 1;
            next_code[bits] = code;
        }
        let mut out = vec![(0u32, 0u32); lens.len()];
        for (sym, &l) in lens.iter().enumerate() {
            let l = l as usize;
            if l != 0 {
                out[sym] = (next_code[l], l as u32);
                next_code[l] += 1;
            }
        }
        out
    }

    /// LSB-first DEFLATE bit packer: bits fill each byte starting from the least
    /// significant bit, matching `inflate_fast`'s `hold |= byte << bits`.
    struct BitWriter {
        bytes: Vec<u8>,
        nbits: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            BitWriter {
                bytes: Vec::new(),
                nbits: 0,
            }
        }

        fn push_bit(&mut self, bit: u32) {
            let byte_idx = (self.nbits / 8) as usize;
            if byte_idx >= self.bytes.len() {
                self.bytes.push(0);
            }
            if bit & 1 != 0 {
                self.bytes[byte_idx] |= 1 << (self.nbits % 8);
            }
            self.nbits += 1;
        }

        /// Write `n` bits of `val`, least-significant bit first (extra bits, and
        /// the BFINAL/BTYPE header fields).
        fn write_lsb(&mut self, val: u32, n: u32) {
            for i in 0..n {
                self.push_bit((val >> i) & 1);
            }
        }

        /// Write an `n`-bit Huffman code most-significant bit first (DEFLATE
        /// packs Huffman codes starting from the high bit of the code).
        fn write_code_msb(&mut self, code: u32, n: u32) {
            for i in (0..n).rev() {
                self.push_bit((code >> i) & 1);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// A symbol in a DEFLATE block body.
    enum Sym {
        Lit(u8),
        Match { len: u16, dist: u16 },
        Eob,
    }

    /// Encode a symbol sequence into raw DEFLATE bytes using the supplied
    /// canonical code tables. When `with_header` is set, a fixed-block header
    /// (BFINAL=1, BTYPE=01) is prepended so the result is a complete raw-DEFLATE
    /// stream a reference decoder (flate2 / C zlib) can consume; otherwise the
    /// bytes are the bare block body that `inflate_fast` expects (the caller
    /// would have already consumed the block header).
    fn encode(
        syms: &[Sym],
        litlen: &[(u32, u32)],
        dist: &[(u32, u32)],
        with_header: bool,
    ) -> Vec<u8> {
        let mut bw = BitWriter::new();
        if with_header {
            bw.write_lsb(1, 1); // BFINAL = 1 (last block)
            bw.write_lsb(1, 2); // BTYPE = 01 (fixed Huffman)
        }
        for s in syms {
            match s {
                Sym::Lit(b) => {
                    let (code, n) = litlen[*b as usize];
                    assert!(n > 0, "literal {b} has no code");
                    bw.write_code_msb(code, n);
                }
                Sym::Match { len, dist: d } => {
                    let (lsym, lextra, lbits) = length_sym(*len);
                    let (code, n) = litlen[lsym];
                    assert!(n > 0, "length symbol {lsym} has no code");
                    bw.write_code_msb(code, n);
                    if lbits > 0 {
                        bw.write_lsb(lextra, lbits);
                    }
                    let (dsym, dextra, dbits) = dist_sym(*d);
                    let (code, n) = dist[dsym];
                    assert!(n > 0, "distance symbol {dsym} has no code");
                    bw.write_code_msb(code, n);
                    if dbits > 0 {
                        bw.write_lsb(dextra, dbits);
                    }
                }
                Sym::Eob => {
                    let (code, n) = litlen[256];
                    assert!(n > 0, "end-of-block has no code");
                    bw.write_code_msb(code, n);
                }
            }
        }
        bw.into_bytes()
    }

    /// The 288 fixed literal/length code lengths (RFC 1951 §3.2.6).
    fn fixed_litlen_lens() -> [u16; 288] {
        let mut l = [0u16; 288];
        for (i, slot) in l.iter_mut().enumerate() {
            *slot = match i {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        l
    }

    /// The 32 fixed distance code lengths (all 5 bits).
    fn fixed_dist_lens() -> [u16; 32] {
        [5u16; 32]
    }

    /// Build the literal/length and distance decode tables into `state.codes`
    /// via the real `inflate_table`, and configure the table indices/bit
    /// widths — the exact set-up the inflate engine performs before entering the
    /// fast loop.
    fn build_tables(state: &mut InflateState, litlen_lens: &[u16], dist_lens: &[u16]) {
        let mut next = 0usize;
        let mut work = [0u16; 320];

        let mut lenbits = 9u32;
        let ll_base = next;
        let r = inflate_table(
            CodeType::Lens,
            litlen_lens,
            litlen_lens.len(),
            &mut state.codes[..],
            &mut next,
            &mut lenbits,
            &mut work,
        );
        assert_eq!(r, 0, "inflate_table(LENS) returned {r}");
        state.lencode = ll_base;
        state.lenbits = lenbits;

        let mut distbits = 6u32;
        let d_base = next;
        let r = inflate_table(
            CodeType::Dists,
            dist_lens,
            dist_lens.len(),
            &mut state.codes[..],
            &mut next,
            &mut distbits,
            &mut work,
        );
        assert_eq!(r, 0, "inflate_table(DISTS) returned {r}");
        state.distcode = d_base;
        state.distbits = distbits;
    }

    /// Construct a `Len`-mode inflate state with the fixed Huffman tables
    /// installed and a fresh (empty) bit accumulator.
    fn fixed_state() -> InflateState {
        let mut state = InflateState::new();
        build_tables(&mut state, &fixed_litlen_lens(), &fixed_dist_lens());
        state.mode = InflateMode::Len;
        state.hold = 0;
        state.bits = 0;
        state
    }

    /// Run `inflate_fast` over `body`, padding the input so the entry
    /// preconditions (`avail_in >= 6`) and the loop margin hold. Returns the
    /// written output prefix and the final mode.
    fn run_fast(
        state: &mut InflateState,
        body: &[u8],
        _produced_len: usize,
    ) -> (Vec<u8>, InflateMode) {
        let mut input = body.to_vec();
        // Pad with trailing zero bytes so there is always >= 6 input available
        // and the do-while continuation (`ip < input.len() - 5`) never trips
        // before the end-of-block code is reached.
        let target = body.len() + 16;
        while input.len() < target.max(6) {
            input.push(0);
        }
        let mut output = vec![0u8; 512];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        inflate_fast(&input, &mut in_pos, &mut output, &mut out_pos, state);
        output.truncate(out_pos);
        (output, state.mode)
    }

    /// Decode a complete raw-DEFLATE stream with the reference decoder (flate2,
    /// built against canonical C zlib via the `zlib` feature).
    fn flate2_inflate(stream: &[u8]) -> Vec<u8> {
        use flate2::read::DeflateDecoder;
        use std::io::Read;
        let mut dec = DeflateDecoder::new(stream);
        let mut out = Vec::new();
        dec.read_to_end(&mut out)
            .expect("flate2 raw inflate failed");
        out
    }

    /// Encode `syms` with the fixed Huffman tables, decode the header-prefixed
    /// form with flate2 (independent oracle) AND the bare body with
    /// `inflate_fast`, and assert both equal `expected`. Returns nothing; panics
    /// on any mismatch.
    fn assert_fixed_roundtrip(syms: &[Sym], expected: &[u8]) {
        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());

        // Oracle: full fixed-block stream through canonical C zlib.
        let with_header = encode(syms, &llc, &dc, true);
        assert_eq!(
            flate2_inflate(&with_header),
            expected,
            "flate2 (C zlib) oracle disagreed with expected output"
        );

        // Subject: bare block body through inflate_fast.
        let body = encode(syms, &llc, &dc, false);
        let mut state = fixed_state();
        let (out, mode) = run_fast(&mut state, &body, expected.len());
        assert_eq!(mode, InflateMode::Type, "expected Type (end-of-block)");
        assert_eq!(out, expected, "inflate_fast output mismatch");
    }

    #[test]
    fn copy_overlapping_run_length_dist_one() {
        // dist == 1 run-length: a single seed byte replicated across the run.
        let mut buf = vec![0u8; 8];
        buf[0] = b'Q';
        copy_overlapping(&mut buf, 1, 0, 5);
        assert_eq!(&buf[..6], b"QQQQQQ");
    }

    #[test]
    fn copy_overlapping_nonoverlapping() {
        let mut buf = vec![0u8; 8];
        buf[0] = b'A';
        buf[1] = b'B';
        // dist == 2, len == 2: regions do not overlap.
        copy_overlapping(&mut buf, 4, 0, 2);
        assert_eq!(&buf[..6], b"AB\0\0AB");
    }

    #[test]
    fn fast_decodes_literals() {
        let syms: Vec<Sym> = b"Hello, world!"
            .iter()
            .map(|&b| Sym::Lit(b))
            .chain([Sym::Eob])
            .collect();
        assert_fixed_roundtrip(&syms, b"Hello, world!");
    }

    #[test]
    fn fast_match_overlap_dist_one() {
        // 'A' then a length-3 / distance-1 match -> run-length "AAAA".
        let syms = [Sym::Lit(b'A'), Sym::Match { len: 3, dist: 1 }, Sym::Eob];
        assert_fixed_roundtrip(&syms, b"AAAA");
    }

    #[test]
    fn fast_match_overlap_longer() {
        // 'X' then a length-5 / distance-1 match -> six X's.
        let syms = [Sym::Lit(b'X'), Sym::Match { len: 5, dist: 1 }, Sym::Eob];
        assert_fixed_roundtrip(&syms, b"XXXXXX");
    }

    #[test]
    fn fast_match_output_nonoverlap() {
        // "AB" then a length-3 / distance-2 match -> "ABABA".
        let syms = [
            Sym::Lit(b'A'),
            Sym::Lit(b'B'),
            Sym::Match { len: 3, dist: 2 },
            Sym::Eob,
        ];
        assert_fixed_roundtrip(&syms, b"ABABA");
    }

    #[test]
    fn fast_match_then_literals_then_match() {
        // Mixed: literals, a back-reference, more literals, another reference.
        let syms = [
            Sym::Lit(b'a'),
            Sym::Lit(b'b'),
            Sym::Lit(b'c'),
            Sym::Match { len: 3, dist: 3 }, // copies "abc"
            Sym::Lit(b'd'),
            Sym::Match { len: 4, dist: 7 }, // copies "abca"
            Sym::Eob,
        ];
        // abc | abc | d | abca  -> "abcabcdabca"
        assert_fixed_roundtrip(&syms, b"abcabcdabca");
    }

    #[test]
    fn fast_window_wnext_zero() {
        // Match references the window only; wnext == 0 (very common case).
        let mut state = fixed_state();
        state.wsize = 8;
        state.window = b"01234567".to_vec().into_boxed_slice();
        state.whave = 8;
        state.wnext = 0;

        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        // distance 8 reaches the oldest window byte; copy 3 -> "012".
        let body = encode(
            &[Sym::Match { len: 3, dist: 8 }, Sym::Eob],
            &llc,
            &dc,
            false,
        );
        let (out, mode) = run_fast(&mut state, &body, 3);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"012");
    }

    #[test]
    fn fast_window_contiguous() {
        // wnext != 0, match contiguous within the window.
        let mut state = fixed_state();
        state.wsize = 16;
        let mut window = vec![0u8; 16];
        window[..8].copy_from_slice(b"ABCDEFGH");
        state.window = window.into_boxed_slice();
        state.whave = 8;
        state.wnext = 8;

        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        // distance 4 (back from position 8 -> window[4]); copy 3 -> "EFG".
        let body = encode(
            &[Sym::Match { len: 3, dist: 4 }, Sym::Eob],
            &llc,
            &dc,
            false,
        );
        let (out, mode) = run_fast(&mut state, &body, 3);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"EFG");
    }

    #[test]
    fn fast_window_wrap() {
        // wnext < wop: the match wraps around the end of the window.
        let mut state = fixed_state();
        state.wsize = 16;
        let mut window = vec![0u8; 16];
        window[8..16].copy_from_slice(b"ABCDEFGH"); // oldest valid bytes
        window[0] = b'I';
        window[1] = b'J'; // newest valid bytes (wnext = 2)
        state.window = window.into_boxed_slice();
        state.whave = 10;
        state.wnext = 2;

        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        // distance 10, length 10 -> the entire 10-byte valid window in order.
        let body = encode(
            &[Sym::Match { len: 10, dist: 10 }, Sym::Eob],
            &llc,
            &dc,
            false,
        );
        let (out, mode) = run_fast(&mut state, &body, 10);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"ABCDEFGHIJ");
    }

    #[test]
    fn fast_window_then_output_tail() {
        // Match begins in the window and continues into this call's output
        // (exercises the `from_window == false` tail with an overlapping run).
        let mut state = fixed_state();
        state.wsize = 8;
        state.window = b"01234567".to_vec().into_boxed_slice();
        state.whave = 8;
        state.wnext = 0;

        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        // 'Z' (output) then distance-2 / length-5 match: 1 byte from window[7]
        // ('7'), the remaining 4 from output starting at offset 0 ("Z7Z7").
        let body = encode(
            &[Sym::Lit(b'Z'), Sym::Match { len: 5, dist: 2 }, Sym::Eob],
            &llc,
            &dc,
            false,
        );
        let (out, mode) = run_fast(&mut state, &body, 6);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"Z7Z7Z7");
    }

    #[test]
    fn fast_second_level_code() {
        // A literal/length code with a maximum length of 10 bits forces a
        // second-level (sub-table) lookup, because the LENS root is capped at 9
        // bits. Lengths [1,2,3,4,5,6,7,8,9,10,10] form a complete code; the two
        // 10-bit symbols ('I', 'J') require the `goto dolen` sub-table path.
        let mut litlen = [0u16; 288];
        litlen[256] = 1; // EOB
        litlen[b'A' as usize] = 2;
        litlen[b'B' as usize] = 3;
        litlen[b'C' as usize] = 4;
        litlen[b'D' as usize] = 5;
        litlen[b'E' as usize] = 6;
        litlen[b'F' as usize] = 7;
        litlen[b'G' as usize] = 8;
        litlen[b'H' as usize] = 9;
        litlen[b'I' as usize] = 10;
        litlen[b'J' as usize] = 10;
        // A trivial complete distance code (unused: no matches in this stream).
        let dist_lens = [1u16, 1u16];

        let mut state = InflateState::new();
        build_tables(&mut state, &litlen, &dist_lens);
        state.mode = InflateMode::Len;
        state.hold = 0;
        state.bits = 0;
        // The root table must be only 9 bits, so the 10-bit codes are 2nd level.
        assert_eq!(
            state.lenbits, 9,
            "expected a 9-bit root (sub-tables present)"
        );

        let llc = canonical_codes(&litlen);
        let dc = canonical_codes(&dist_lens);
        // 'I' (10-bit, second level) and 'J' (10-bit) then end-of-block.
        let body = encode(
            &[Sym::Lit(b'I'), Sym::Lit(b'J'), Sym::Eob],
            &llc,
            &dc,
            false,
        );
        let (out, mode) = run_fast(&mut state, &body, 2);
        assert_eq!(mode, InflateMode::Type);
        assert_eq!(out, b"IJ");
    }

    #[test]
    fn fast_large_compressible_roundtrip() {
        // A larger run that keeps the loop in the fast path for many iterations:
        // a seed followed by a long distance-1 run, then a distance-26 copy.
        let mut syms = vec![Sym::Lit(b'k'), Sym::Lit(b'l'), Sym::Lit(b'm')];
        // Repeatedly copy the last 3 bytes (distance 3) to grow the buffer.
        for _ in 0..40 {
            syms.push(Sym::Match { len: 3, dist: 3 });
        }
        syms.push(Sym::Eob);

        // Expected: "klm" repeated; 3 + 40*3 = 123 bytes total, all "klm…".
        let mut expected = Vec::new();
        while expected.len() < 123 {
            expected.extend_from_slice(b"klm");
        }
        expected.truncate(123);

        assert_fixed_roundtrip(&syms, &expected);
    }

    #[test]
    fn fast_invalid_distance_too_far_back_sets_bad() {
        // A match whose distance exceeds everything available (no window, no
        // prior output) must set mode = Bad.
        let mut state = fixed_state();
        // No window, no output yet: distance 1 with opos 0 -> wop = 1 > whave 0.
        state.wsize = 0;
        state.whave = 0;
        state.wnext = 0;

        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        let body = encode(
            &[Sym::Match { len: 3, dist: 1 }, Sym::Eob],
            &llc,
            &dc,
            false,
        );
        // Call `inflate_fast` directly (rather than via `run_fast`) so we can
        // capture the returned `FastBad` reason. Pad the input to satisfy the
        // entry precondition (avail_in >= 6) and the do-while margin, mirroring
        // `run_fast`.
        let mut input = body.clone();
        while input.len() < body.len() + 16 {
            input.push(0);
        }
        let mut output = vec![0u8; 512];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        let bad = inflate_fast(&input, &mut in_pos, &mut output, &mut out_pos, &mut state);
        assert_eq!(
            state.mode,
            InflateMode::Bad,
            "expected Bad on distance too far back"
        );
        // M6: the fast path must surface the *specific* reason so the caller in
        // `inflate/mod.rs` can assign the matching fixed zlib `Z_DATA_ERROR`
        // message (identical to the slow-path text).
        assert_eq!(bad, Some(FastBad::DistTooFar));
        assert_eq!(bad.unwrap().message(), "invalid distance too far back");
    }

    #[test]
    fn fast_bad_messages_match_c_inffast() {
        // M6: each `FastBad` variant maps to the exact fixed zlib message that C
        // `inffast.c` writes to `strm->msg`, and that the slow path in
        // `inflate/mod.rs` assigns for the same conditions.
        assert_eq!(
            FastBad::InvalidLitLen.message(),
            "invalid literal/length code"
        );
        assert_eq!(FastBad::InvalidDist.message(), "invalid distance code");
        assert_eq!(
            FastBad::DistTooFar.message(),
            "invalid distance too far back"
        );
    }

    #[test]
    fn fast_runs_out_of_input_keeps_len_mode() {
        // When end-of-block is never reached and input/output is exhausted, the
        // routine must leave mode == Len so the caller re-enters later.
        let mut state = fixed_state();
        let llc = canonical_codes(&fixed_litlen_lens());
        let dc = canonical_codes(&fixed_dist_lens());
        // A long stream of literals with NO end-of-block; a small output buffer
        // forces an early exit while still in Len mode.
        let syms: Vec<Sym> = (0..400).map(|_| Sym::Lit(b'z')).collect();
        let body = encode(&syms, &llc, &dc, false);
        let mut input = body.clone();
        while input.len() < body.len() + 8 {
            input.push(0);
        }
        // Output buffer just big enough to satisfy the entry precondition.
        let mut output = vec![0u8; 300];
        let mut in_pos = 0usize;
        let mut out_pos = 0usize;
        inflate_fast(&input, &mut in_pos, &mut output, &mut out_pos, &mut state);
        assert_eq!(
            state.mode,
            InflateMode::Len,
            "should still be Len after partial decode"
        );
        assert!(out_pos > 0, "some literals should have been produced");
        assert!(output[..out_pos].iter().all(|&b| b == b'z'));
    }
}
