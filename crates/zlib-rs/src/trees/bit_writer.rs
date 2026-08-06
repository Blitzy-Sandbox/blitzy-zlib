//! The bit-level output primitives -- every bit the compressor emits passes through here.
//!
//! Ports the six items `trees.c` keeps between L140 and L286: the `put_short` macro, the three
//! bit-buffer routines `bi_reverse`, `bi_flush` and `bi_windup`, and the two macros `send_code`
//! and `send_bits` that all Huffman output travels on.
//!
//! An unqualified line reference in this file, here and in the documentation of every item below,
//! is a line of `trees.c`.
//!
//! | C item | C source | Here |
//! |---|---|---|
//! | `put_short(s, w)` | L140-L147 | [`put_short`] |
//! | `bi_reverse(code, len)` | L149-L161 | [`bi_reverse`] |
//! | `bi_flush(s)` | L163-L176 | [`bi_flush`] |
//! | `bi_windup(s)` | L178-L193 | [`bi_windup`] |
//! | `send_code(s, c, tree)` | L238-L240 | [`send_code`] |
//! | `send_bits(s, value, length)` | L248-L251, L274-L286 | [`send_bits`] |
//!
//! # The accumulator, and why its exact width is observable
//!
//! Three fields of [`DeflateState`] make up the accumulator, and all three are the reference's
//! (`deflate.h` L266-L276):
//!
//! ```c
//! ush bi_buf;   /* Output buffer. bits are inserted starting at the bottom
//!                * (least significant bits). */
//! int bi_valid; /* Number of valid bits in bi_buf.  All bits above the last
//!                * valid bit are always zero. */
//! int bi_used;  /* Last number of used bits when going to a byte boundary. */
//! ```
//!
//! `bi_buf` is a `ush`, so it is **16 bits wide and nothing wider** -- [`BUF_SIZE`] is C's
//! `Buf_size` of 16 (`deflate.h` L55-L56). Widening it to 32 or 64 bits would be a behaviour
//! change rather than an optimisation, because `send_bits` relies on the truncation: it ORs the
//! incoming value in at `bi_valid` and lets the high bits fall off the top of the `ush`
//! (L263), then re-extracts exactly those discarded bits by shifting the *original* value right
//! by `Buf_size - bi_valid` (L265). In a wider register the first step would keep bits the
//! second step also produces, and every spilled short would carry duplicated bits. The port
//! therefore keeps `u16` storage and reproduces C's cast semantics explicitly; see
//! [`shifted_in`] and [`shifted_out`].
//!
//! `bi_valid` is an `int`, i.e. signed, and that too is load-bearing. `bi_windup` computes
//! `((s->bi_valid - 1) & 7) + 1` (L187), which for `bi_valid == 0` is `((-1) & 7) + 1 == 8`.
//! Evaluated in an unsigned type the subtraction would either wrap or, in a debug build, abort a
//! C caller's process. Every comparison and every accumulation here is therefore `i32`, matching
//! [`BUF_SIZE`]'s own declared type.
//!
//! # Bit order is fixed by RFC 1951, in two different directions
//!
//! `doc/rfc1951.txt` L293-L300 sets both rules the accumulator implements:
//!
//! * "Data elements are packed into bytes in order of increasing bit number within the byte,
//!   i.e., starting with the least-significant bit of the byte" (L293-L295). That is why bits
//!   enter `bi_buf` at `bi_valid` from the bottom and why [`put_short`] writes its low byte
//!   first.
//! * "Huffman codes are packed starting with the most-significant bit of the code"
//!   (L299-L300). A Huffman code is therefore stored pre-reversed in the tree, so that emitting
//!   it least-significant-bit-first reproduces the required order. [`bi_reverse`] performs that
//!   reversal once, when the codes are generated, rather than on every emission.
//!
//! # Every byte leaves through `put_byte`
//!
//! Nothing here touches the pending buffer directly. [`put_short`] is two
//! [`put_byte`] calls, and `put_byte` in turn writes through
//! [`crate::weak_slice::PendingBuf`], which is bounds-checked and holds the `pending_buf` /
//! `pending` pair (`deflate.h` L107-L110) as an owned slice with an integer cursor. There is no
//! pointer arithmetic in this module and no second write path.
//!
//! The buffer that receives those bytes is shared with the symbol buffer, which
//! `deflateInit2_` overlays inside the same allocation at `pending_buf + lit_bufsize`
//! (`deflate.c` L520). Nothing here allocates, resizes or replaces it.
//!
//! # What is deliberately elsewhere
//!
//! * **`put_byte`** (`deflate.h` L290-L293) belongs to [`crate::deflate::pending`], which owns
//!   the pending cursors; it is imported, never re-implemented.
//! * **The six `_tr_*` entry points** -- `_tr_init`, `_tr_stored_block`, `_tr_flush_bits`,
//!   `_tr_align`, `_tr_tally` and `_tr_flush_block` (`deflate.h` L311-L317) -- are `trees/mod.rs`'s.
//!   They are the callers of everything here: `_tr_flush_bits` is exactly [`bi_flush`]
//!   (L879-L881), `_tr_align` is `send_bits` + `send_code` + `bi_flush` (L887-L894),
//!   `_tr_stored_block` is `send_bits` + `bi_windup` + two `put_short`s (L860-L866) and
//!   `_tr_flush_block` ends with `bi_windup` on the last block (L1081-L1083).
//! * **`gen_codes`** (L203-L232), the one caller of [`bi_reverse`], is `trees/build.rs`'s, as are
//!   `send_tree`, `send_all_trees` and `compress_block`, which are the other callers of
//!   [`send_bits`] and [`send_code`].
//! * **`deflatePrime`** (`deflate.c` L745-L770) writes `bi_buf` and `bi_valid` itself instead of
//!   going through [`send_bits`], because it inserts bits at a byte boundary the caller chooses.
//!   It is `deflate/mod.rs`'s, and it is the reason [`BUF_SIZE`] is public to the crate.
//! * **Everything under `ZLIB_DEBUG`** is not ported: the `send_bits` *function* form (L253-L271),
//!   `bits_sent` (L190-L192, L256), `compressed_len`, `Assert`, `Tracevv` and the debug variant
//!   of `send_code` (L242-L246). The default build uses the macro forms, and those are what the
//!   byte-identical-output requirement is stated against.
//!
//! # Integer widths
//!
//! | Value | Type here | Why |
//! |---|---|---|
//! | `bi_buf` | `u16` | C's `ush`, and the truncation is observable |
//! | `bi_valid`, `bi_used`, `length` | `i32` | C's `int`; `bi_used`'s formula needs the sign |
//! | a code, a residue, a short | `u16` | Bounded by `length <= Buf_size` bits |
//! | a symbol index | `usize` | It indexes a tree slice |
//!
//! # Failure posture
//!
//! No `unsafe` (the crate root forbids it), no `unwrap`, no `expect`, no `[]` indexing and no
//! arithmetic that can overflow for any input the C contract admits. The two IN assertions C
//! states as comments are restated as [`debug_assert!`]s, so a violated contract is loud in a
//! test build and cannot become a release-mode abort inside a library a C caller has linked --
//! the same posture, for the same reason, as [`crate::deflate::pending`].

use crate::deflate::pending::put_byte;
use crate::deflate::state::{Allocator, CtData, DeflateState, BUF_SIZE};

// -----------------------------------------------------------------------------
//  C cast reproduction
// -----------------------------------------------------------------------------

/// C's `(uch)((w) & 0xff)`: the low byte of a short (L145).
///
/// Also C's `(Byte)s->bi_buf`, which `bi_flush` (L172) and `bi_windup` (L184) use to move the
/// bottom eight bits of the accumulator into the pending buffer.
#[inline]
fn low_byte(value: u16) -> u8 {
    // The mask is the cast's own truncation, so nothing is lost that C keeps.
    (value & 0xff) as u8
}

/// C's `(uch)((ush)(w) >> 8)`: the high byte of a short (L146).
#[inline]
fn high_byte(value: u16) -> u8 {
    (value >> 8) as u8
}

/// C's `(ush)value << bits`, truncated to the width `bi_buf` can hold.
///
/// This is the left-hand side of `s->bi_buf |= (ush)value << s->bi_valid` (L263, L268). In C the
/// `ush` operand is promoted to `int`, shifted in 32 bits, and then narrowed again by the
/// assignment to `s->bi_buf`; the low 16 bits of that are what survives. Two cases arise, and
/// both are reproduced exactly:
///
/// * `bits < 16` -- Rust's `<<` on `u16` discards the bits that leave the top, which is the same
///   set the C assignment discards, and it cannot panic because the shift amount is in range.
/// * `bits >= 16` -- every bit of the value lands at position 16 or above, so C's narrowing
///   yields zero. [`u16::checked_shl`] reports [`None`] for exactly that range, so the
///   `unwrap_or(0)` is not a fallback for an error but the C result itself. The case is reachable:
///   `bi_valid` is 16 whenever a previous `send_bits` filled the accumulator without spilling.
///
/// `bits` is an [`i32`] because it is always `bi_valid`, which C declares `int`. It is
/// non-negative by invariant, so [`i32::unsigned_abs`] is the identity on it; it is preferred to
/// `as u32` because no cast can then silently reinterpret a negative value, and to
/// [`u32::try_from`] because that would introduce an error path this function does not have.
#[inline]
fn shifted_in(value: u16, bits: i32) -> u16 {
    value.checked_shl(bits.unsigned_abs()).unwrap_or(0)
}

/// C's `(ush)value >> bits`: the bits of the value that did not fit in the accumulator.
///
/// This is the right-hand side of `s->bi_buf = (ush)value >> (Buf_size - s->bi_valid)` (L265),
/// evaluated with the `bi_valid` that was current *before* the spill, so it recovers precisely
/// the bits [`shifted_in`] pushed off the top of the `ush`.
///
/// `bits` is `Buf_size - bi_valid` and lies in `0..=15` on the only path that reaches here, since
/// the spill branch is taken only when `bi_valid > Buf_size - length` and `length <= Buf_size`
/// force `bi_valid >= 1`. [`u16::checked_shr`] covers the unreachable remainder without a panic
/// and with C's own answer, a fully shifted-out value being zero.
#[inline]
fn shifted_out(value: u16, bits: i32) -> u16 {
    value.checked_shr(bits.unsigned_abs()).unwrap_or(0)
}

// -----------------------------------------------------------------------------
//  Code reversal -- L149-L161
// -----------------------------------------------------------------------------

/// Reverses the low `len` bits of `code`.
///
/// Port of `bi_reverse` (L154-L161), whose comment records both its contract and its rationale --
/// "Reverse the first len bits of a code, using straightforward code (a faster method would use a
/// table)", `IN assertion: 1 <= len <= 15` (L149-L153):
///
/// ```c
/// local unsigned bi_reverse(unsigned code, int len) {
///     unsigned res = 0;
///     do {
///         res |= code & 1;
///         code >>= 1, res <<= 1;
///     } while (--len > 0);
///     return res >> 1;
/// }
/// ```
///
/// `gen_codes` is the only caller (L227). It reverses each canonical Huffman code once, as the
/// code is assigned, because RFC 1951 requires a code to be transmitted most-significant bit
/// first (`doc/rfc1951.txt` L299-L300) while [`send_bits`] emits least-significant bit first.
///
/// # The loop shape is part of the port
///
/// C's body is a `do`/`while`, so it always runs at least once, and it shifts `res` left on
/// *every* iteration including the last -- which the trailing `res >> 1` then undoes. Both
/// details are reproduced rather than tidied: a Rust `while` or `for` loop would run zero times
/// for `len == 0` and return `0`, where C returns the low bit of `code`. The counter is `i32`
/// because C's `len` is `int` and the exit test is the post-decrement `--len > 0`.
///
/// # Width
///
/// `u16` throughout is exact for the contract range. After `k` iterations `res` is at most
/// `2^(k+1) - 2`, so at `len == 15` it reaches `65534` before the final shift and never overflows;
/// C's caller narrows the `unsigned` result with `(ush)` anyway (L227). `u16` also matches
/// [`CtData::code`], which is where every result is stored, and
/// [`MAX_BITS`](crate::deflate::state::MAX_BITS) of 15 is the widest code the format allows.
#[inline]
pub(crate) fn bi_reverse(code: u16, len: u16) -> u16 {
    let mut code = code;
    let mut res: u16 = 0;
    // C's `int len`, which the loop condition consumes with `--len > 0`.
    let mut remaining = i32::from(len);

    loop {
        // `res |= code & 1;`
        res |= code & 1;
        // `code >>= 1, res <<= 1;` -- one comma expression, in that order.
        code >>= 1;
        res <<= 1;

        // `} while (--len > 0);`
        remaining -= 1;
        if remaining <= 0 {
            break;
        }
    }

    // `return res >> 1;` -- undoes the last iteration's shift.
    res >> 1
}

// -----------------------------------------------------------------------------
//  Writing a short -- L140-L147
// -----------------------------------------------------------------------------

/// Appends a 16-bit value to the pending buffer, **least** significant byte first.
///
/// Port of the `put_short` macro (L144-L147), whose comment is "Output a short LSB first on the
/// stream. IN assertion: there is enough room in pendingBuf." (L140-L143):
///
/// ```c
/// #define put_short(s, w) { \
///     put_byte(s, (uch)((w) & 0xff)); \
///     put_byte(s, (uch)((ush)(w) >> 8)); \
/// }
/// ```
///
/// The order is not a convention that could be swapped. Two kinds of caller depend on it:
///
/// * the accumulator spill in [`send_bits`] (L264) and in [`bi_flush`] / [`bi_windup`] (L168,
///   L183), where the low byte holds the bits that were buffered first and therefore must be
///   transmitted first, per `doc/rfc1951.txt` L293-L295; and
/// * the stored-block header written by `_tr_stored_block` (L864-L865), which emits `LEN` and then
///   its one's complement `NLEN` as two little-endian shorts, as RFC 1951 3.2.4 specifies.
///
/// This is `trees.c`'s own macro and has no relation to `putShortMSB` (`deflate.c` L934-L942),
/// which is big-endian and belongs to the zlib header and trailer; that one is
/// [`crate::deflate::pending::put_short_msb`].
///
/// # Return value
///
/// `true` when both bytes were stored. `false` reports that the pending buffer filled, in which
/// case the low byte may have been written and the high byte not -- the same partial write C's
/// two-statement expansion performs, except that C would also write past the end of the
/// allocation. Deliberately not `#[must_use]`, exactly as
/// [`put_byte`] is not: every C caller has already discharged
/// the IN assertion, so the four call sites in this file ignore the result as C's macro does.
#[inline]
pub(crate) fn put_short<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, w: u16) -> bool {
    // `put_byte(s, (uch)((w) & 0xff));`
    let low = low_byte(w);
    // `put_byte(s, (uch)((ush)(w) >> 8));`
    let high = high_byte(w);

    // `&&` short-circuits where C would perform the second write regardless. The outcome is the
    // same either way -- a buffer with no room for the low byte has none for the high byte -- and
    // stopping is what keeps the checked view from being asked for a slot it does not have.
    put_byte(state, low) && put_byte(state, high)
}

// -----------------------------------------------------------------------------
//  Draining the accumulator -- L163-L193
// -----------------------------------------------------------------------------

/// Moves whole bytes out of the bit buffer, keeping at most seven bits in it.
///
/// Port of `bi_flush` (L166-L176), documented as "Flush the bit buffer, keeping at most 7 bits in
/// it." (L163-L165):
///
/// ```c
/// local void bi_flush(deflate_state *s) {
///     if (s->bi_valid == 16) {
///         put_short(s, s->bi_buf);
///         s->bi_buf = 0;
///         s->bi_valid = 0;
///     } else if (s->bi_valid >= 8) {
///         put_byte(s, (Byte)s->bi_buf);
///         s->bi_buf >>= 8;
///         s->bi_valid -= 8;
///     }
/// }
/// ```
///
/// The three-way shape is exact and the thresholds differ from [`bi_windup`]'s: `== 16` first,
/// then `>= 8`, then an implicit no-op. Nothing is emitted below eight valid bits, because a
/// partial byte cannot be written without inventing the bits that would pad it -- which is what
/// makes this the *flush* rather than the *windup*.
///
/// # Post-condition
///
/// `bi_valid` is in `0..=7` on return, and `bi_buf` holds exactly those bits. The one-byte branch
/// takes `bi_valid` from `8..=15` down to `0..=7`; the short branch clears both fields.
///
/// # Callers
///
/// `_tr_flush_bits` is nothing but this function (L879-L881), and `_tr_align` ends with it
/// (L894). `_tr_flush_bits` matters disproportionately because
/// [`flush_pending`](crate::deflate::pending::flush_pending) invokes it *before* copying anything
/// to the caller (`deflate.c` L954): a byte still sitting in `bi_buf` when the copy is made is a
/// byte a caller that supplied room for it does not receive.
pub(crate) fn bi_flush<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    if state.bi_valid == BUF_SIZE {
        // `put_short(s, s->bi_buf);` -- read out first, because `state` is borrowed mutably for
        // the write. The value, not the field, is what is emitted.
        let bits = state.bi_buf;
        put_short(state, bits);

        // `s->bi_buf = 0; s->bi_valid = 0;`
        state.bi_buf = 0;
        state.bi_valid = 0;
    } else if state.bi_valid >= 8 {
        // `put_byte(s, (Byte)s->bi_buf);`
        let byte = low_byte(state.bi_buf);
        put_byte(state, byte);

        // `s->bi_buf >>= 8; s->bi_valid -= 8;` -- the eight bits just written are dropped, and
        // what was above them becomes the new bottom of the buffer.
        state.bi_buf >>= 8;
        state.bi_valid -= 8;
    }
}

/// Flushes the bit buffer and aligns the output on a byte boundary.
///
/// Port of `bi_windup` (L181-L193), documented as "Flush the bit buffer and align the output on a
/// byte boundary" (L178-L180):
///
/// ```c
/// local void bi_windup(deflate_state *s) {
///     if (s->bi_valid > 8) {
///         put_short(s, s->bi_buf);
///     } else if (s->bi_valid > 0) {
///         put_byte(s, (Byte)s->bi_buf);
///     }
///     s->bi_used = ((s->bi_valid - 1) & 7) + 1;
///     s->bi_buf = 0;
///     s->bi_valid = 0;
/// }
/// ```
///
/// Unlike [`bi_flush`] this emits a *partial* byte, padding it with the zeros above the last valid
/// bit -- which is exactly what aligning means, and why the thresholds are `> 8` and `> 0` here
/// where the flush uses `== 16` and `>= 8`. RFC 1951 3.2.4 requires that alignment before a stored
/// block, and 3.2.3 permits the padding after the final block.
///
/// # Post-condition
///
/// `bi_valid` is `0` and `bi_buf` is `0`; `bi_used` reports how many bits of the byte just
/// completed were real.
///
/// # `bi_used` is signed arithmetic, and `bi_valid == 0` must give 8
///
/// `((bi_valid - 1) & 7) + 1` maps `0` to `8`, `1..=8` to themselves and `9..=16` to `1..=8`. The
/// `0` case is only correct in a signed type: `bi_valid` is `int`, so `0 - 1` is `-1`, and
/// `-1 & 7` is `7`. Computed in an unsigned type the subtraction would wrap -- or, in a debug
/// build, panic -- so the expression is evaluated in [`i32`] here exactly as C evaluates it. The
/// answer 8 is the right one: no bits of the next byte have been used, so the last byte to reach a
/// boundary used all eight of its own.
///
/// This is the value `deflateUsed` reports to callers (`deflate.c` L737-L742), by way of
/// [`deflate_used`](crate::deflate::pending::deflate_used), which is why it is maintained here
/// rather than dropped as bookkeeping. `deflate_stored` additionally forces it to 8 on the paths
/// that end on a boundary without passing through this function (`deflate.c` L1791, L1846).
///
/// # Callers
///
/// `_tr_stored_block` calls this immediately after the three block-type bits, to align before the
/// `LEN`/`NLEN` pair (L863), and `_tr_flush_block` calls it once the last block of the stream has
/// been written (L1081-L1083). The `#ifdef ZLIB_DEBUG` rounding of `bits_sent` (L190-L192) is not
/// ported.
pub(crate) fn bi_windup<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    if state.bi_valid > 8 {
        // `put_short(s, s->bi_buf);` -- more than a byte is pending, so both bytes go out; the
        // bits above `bi_valid` are the zeros the accumulator's invariant guarantees.
        let bits = state.bi_buf;
        put_short(state, bits);
    } else if state.bi_valid > 0 {
        // `put_byte(s, (Byte)s->bi_buf);`
        let byte = low_byte(state.bi_buf);
        put_byte(state, byte);
    }

    // `s->bi_used = ((s->bi_valid - 1) & 7) + 1;` -- `i32`, so the `bi_valid == 0` case yields 8.
    // The subtraction cannot overflow: `bi_valid` is in `0..=Buf_size` for every state this
    // module and `deflatePrime` can produce.
    state.bi_used = ((state.bi_valid - 1) & 7) + 1;

    // `s->bi_buf = 0; s->bi_valid = 0;`
    state.bi_buf = 0;
    state.bi_valid = 0;
}

// -----------------------------------------------------------------------------
//  Sending bits -- L238-L286
// -----------------------------------------------------------------------------

/// Sends a value on a given number of bits.
///
/// Port of the default, non-`ZLIB_DEBUG` `send_bits` macro (L274-L286). Its contract is stated
/// once above both forms -- "Send a value on a given number of bits. IN assertion: length <= 16 and
/// value fits in length bits." (L248-L251) -- and the macro is:
///
/// ```c
/// #define send_bits(s, value, length) \
/// { int len = length;\
///   if (s->bi_valid > (int)Buf_size - len) {\
///     int val = (int)value;\
///     s->bi_buf |= (ush)val << s->bi_valid;\
///     put_short(s, s->bi_buf);\
///     s->bi_buf = (ush)val >> (Buf_size - s->bi_valid);\
///     s->bi_valid += len - Buf_size;\
///   } else {\
///     s->bi_buf |= (ush)(value) << s->bi_valid;\
///     s->bi_valid += len;\
///   }\
/// }
/// ```
///
/// Every bit of every compressed block reaches the output through this function: the three
/// block-type bits (L862, L889, L1059, L1066), the three tree-size fields and the bit-length code
/// widths (L841-L846), the repeat counts of the bit-length tree (L777-L783), every literal, length
/// and distance code by way of [`send_code`], and the extra bits that follow a length or distance
/// (L927, L937).
///
/// # The spill branch, in the order C performs it
///
/// When `bi_valid > Buf_size - length` the incoming value does not fit, and four steps happen in a
/// sequence that cannot be rearranged:
///
/// 1. the value is shifted up by the **pre-update** `bi_valid` and OR-ed into `bi_buf`, filling it
///    to 16 bits and discarding whatever lies above (see [`shifted_in`]);
/// 2. the now-full accumulator is written out as a little-endian short;
/// 3. the residue is recovered from the **original** value -- not from `bi_buf` -- by shifting it
///    right by `Buf_size - bi_valid`, again using the pre-update `bi_valid` (see [`shifted_out`]);
/// 4. only then is `bi_valid` advanced, by `length - Buf_size`.
///
/// The port captures `bi_valid` in a local before step 1 so that steps 1 and 3 cannot see a
/// half-updated value. Getting the order wrong produces a stream that frequently still
/// round-trips locally while differing from the reference, which is why the differential suite,
/// not inspection, is the arbiter here.
///
/// # Boundaries
///
/// The comparison is signed `int` arithmetic on both sides, as C's `(int)Buf_size - len` is, so
/// `length == Buf_size` gives `bi_valid > 0` and any pending bit forces a spill. At exactly
/// `bi_valid == Buf_size - length` the value still fits and the cheap branch is taken, leaving
/// `bi_valid == Buf_size`; the next call then spills. That is also the only way `bi_valid` reaches
/// 16, which is what makes the `bits >= 16` case of [`shifted_in`] reachable.
///
/// # Contract
///
/// `value` is `u16` because the IN assertion bounds it to `length <= Buf_size` bits, which is also
/// the width of `Code` in a tree entry and of the accumulator itself; `length` is `i32` to match
/// [`BUF_SIZE`], `bi_valid` and the `extra_lbits` / `extra_dbits` / `extra_blbits` tables that
/// supply it directly at L927 and L937. Both halves of the C comment's assertion are restated as
/// [`debug_assert!`]s. The `ZLIB_DEBUG` function form additionally asserts `length > 0 && length
/// <= 15` (L255); that stricter bound is deliberately not enforced, because the contract the
/// default build is written against is the comment's.
pub(crate) fn send_bits<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    value: u16,
    length: i32,
) {
    // `IN assertion: length <= 16 ...` (L250). A negative width is meaningless and would make the
    // comparison below spill unconditionally, so the lower bound is asserted too.
    debug_assert!(
        (0..=BUF_SIZE).contains(&length),
        "send_bits length outside 0..=Buf_size (trees.c L250)"
    );
    // `... and value fits in length bits.` (L250). Checked only where it is checkable: at
    // `length == Buf_size` every `u16` fits by construction.
    debug_assert!(
        length >= BUF_SIZE || u32::from(value) < (1u32 << length.unsigned_abs()),
        "send_bits value wider than length bits (trees.c L250)"
    );

    // `{ int len = length; ... }` with `s->bi_valid` read once, before anything writes it, so that
    // both uses below see the value C's macro sees.
    let valid = state.bi_valid;

    if valid > BUF_SIZE - length {
        // `s->bi_buf |= (ush)val << s->bi_valid;`
        state.bi_buf |= shifted_in(value, valid);

        // `put_short(s, s->bi_buf);`
        let bits = state.bi_buf;
        put_short(state, bits);

        // `s->bi_buf = (ush)val >> (Buf_size - s->bi_valid);` -- from `value`, with the pre-update
        // `bi_valid`; this is the residue the OR above pushed off the top of the accumulator.
        state.bi_buf = shifted_out(value, BUF_SIZE - valid);

        // `s->bi_valid += len - Buf_size;` -- in `0..=Buf_size` afterwards, and at least 1,
        // because the branch condition is `valid + length > Buf_size`.
        state.bi_valid = valid + length - BUF_SIZE;
    } else {
        // `s->bi_buf |= (ush)(value) << s->bi_valid;`
        state.bi_buf |= shifted_in(value, valid);

        // `s->bi_valid += len;` -- at most `Buf_size`, by the branch condition.
        state.bi_valid = valid + length;
    }
}

/// Sends the code of a symbol from a given Huffman tree.
///
/// Port of the default, non-`ZLIB_DEBUG` `send_code` macro (L238-L240), whose comment is "Send a
/// code of the given tree. c and tree must not have side effects":
///
/// ```c
/// #define send_code(s, c, tree) send_bits(s, tree[c].Code, tree[c].Len)
/// ```
///
/// The two halves of the entry are read through [`CtData::code`] and [`CtData::len`], which are
/// C's `Code` and `Len` macros over the `fc` and `dl` unions of `ct_data` (`deflate.h` L71-L86).
/// `Code` shares storage with `Freq` and `Len` with `Dad`, so the accessors are not
/// interchangeable with their aliases: an entry is only a code once `gen_codes` has overwritten
/// the frequency (L224) and `gen_bitlen` has overwritten the parent link (L540-L613). Emitting
/// before then would transmit frequencies as bit strings.
///
/// A tree entry left at its zeroed default -- code 0, length 0 -- emits nothing, since
/// [`send_bits`] with `length == 0` neither writes a byte nor advances `bi_valid`. That is also
/// what makes a symbol index past the end of `tree` harmless: it is returned from without
/// emitting, which is the same non-event, rather than being handled with an error the C macro does
/// not have or a panic a C caller cannot survive. No index in the reference can reach it -- every
/// caller iterates an alphabet no larger than the tree it pairs with.
///
/// # Borrowing
///
/// `tree` is a slice and `state` is borrowed mutably, so the compiler will reject a call that
/// passes a tree living *inside* the state -- `dyn_ltree`, `dyn_dtree` or `bl_tree`. That is the
/// aliasing C performs freely through `const ct_data *` while `send_bits` mutates the same struct
/// (L900-L949). A caller in that position copies the entry out first, which is two `u16`s, and
/// emits it directly:
///
/// ```ignore
/// let entry = state.tree_for(kind).get(symbol).copied().unwrap_or_default();
/// send_bits(state, entry.code(), i32::from(entry.len()));
/// ```
///
/// Passing one of the `'static` tables from [`crate::trees::static_tables`], as `_tr_align` does
/// with `send_code(s, END_BLOCK, static_ltree)` (L890), needs no such treatment.
#[inline]
pub(crate) fn send_code<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    code: usize,
    tree: &[CtData],
) {
    // `tree[c]`, read once. `CtData` is `Copy`, so the borrow of `tree` ends here and cannot
    // conflict with the mutable borrow of `state` that follows.
    let Some(&entry) = tree.get(code) else {
        return;
    };

    // `send_bits(s, tree[c].Code, tree[c].Len)`
    send_bits(state, entry.code(), i32::from(entry.len()));
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// harness: a test asserts, and a failing assertion panics. Indexing is allowed because every index
// below is bounded by a literal the line above it establishes. The same relaxation, for the same
// reason, appears in `deflate/pending.rs` and `deflate/state.rs`.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{bi_flush, bi_reverse, bi_windup, put_short, send_bits, send_code};
    use crate::deflate::state::{
        CtData, DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, BUF_SIZE,
        DEF_MEM_LEVEL, MAX_BITS, STATIC_TREES,
    };
    use crate::trees::static_tables::{static_dtree, static_ltree, END_BLOCK};

    /// The stream `deflateInit2_` produces for the default configuration -- level 6, `windowBits`
    /// 15, `memLevel` 8, [`Strategy::Default`] (`deflate.c` L387-L533).
    ///
    /// Its buffers arrive filled with a sentinel rather than zeros, exactly as `test/infcover.c`
    /// L87 fills the blocks it hands out, so no assertion below may read a byte this module did
    /// not write. Every one of them compares against `state.pending.written()`, which is
    /// `pending_buf[..pending]` and therefore covers written bytes only.
    fn default_state() -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: 15,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };

        DeflateState::new(config, GlobalAllocator).unwrap()
    }

    /// A fresh state whose accumulator holds `valid` bits of `pattern`.
    ///
    /// The bits above `valid` are cleared, because "All bits above the last valid bit are always
    /// zero" is an invariant of `bi_buf` (`deflate.h` L271-L272) that every caller of `bi_flush`
    /// and `bi_windup` has already established.
    fn state_holding(pattern: u16, valid: i32) -> DeflateState<'static, GlobalAllocator> {
        let mut state = default_state();
        state.bi_buf = pattern & mask(valid);
        state.bi_valid = valid;
        state
    }

    /// The low `bits` bits set, computed independently of the production shift helpers.
    fn mask(bits: i32) -> u16 {
        let mut out: u16 = 0;
        for bit in 0..bits {
            out |= 1u16 << u32::try_from(bit).unwrap();
        }
        out
    }

    /// Reverses the low `len` bits of `code` by placing each bit at its mirrored position.
    ///
    /// Deliberately unlike [`bi_reverse`]: it indexes bit positions instead of accumulating with a
    /// shift, so agreement between the two is evidence rather than a tautology.
    fn reference_reverse(code: u16, len: u16) -> u16 {
        let mut out: u16 = 0;
        for bit in 0..u32::from(len) {
            if code & (1u16 << bit) != 0 {
                out |= 1u16 << (u32::from(len) - 1 - bit);
            }
        }
        out
    }

    /// An independent least-significant-bit-first bit reader over emitted bytes.
    ///
    /// This is the decoder side of `doc/rfc1951.txt` L293-L295, written as byte/bit index
    /// arithmetic so that it shares no code with the accumulator it checks.
    struct BitReader<'a> {
        bytes: &'a [u8],
        bit: usize,
    }

    impl<'a> BitReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, bit: 0 }
        }

        /// Reads `len` bits, least significant first, as [`send_bits`] wrote them.
        fn take(&mut self, len: i32) -> u16 {
            let mut out: u16 = 0;
            for index in 0..len {
                let byte = self.bytes[self.bit / 8];
                let offset = u32::try_from(self.bit % 8).unwrap();
                let bit = u16::from((byte >> offset) & 1);
                out |= bit << u32::try_from(index).unwrap();
                self.bit += 1;
            }
            out
        }
    }

    /// A deterministic pseudo-random generator, so the round-trip test is reproducible and needs no
    /// dependency. The multiplier and increment are Numerical Recipes' 32-bit LCG.
    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0
        }
    }

    // -------------------------------------------------------------------------
    //  bi_reverse -- L154-L161
    // -------------------------------------------------------------------------

    #[test]
    fn bi_reverse_matches_an_independent_reversal_exhaustively() {
        // The IN assertion is `1 <= len <= 15` (L152), and `MAX_BITS` is that upper bound. Every
        // code of every admissible width is covered: 2^16 - 2 cases in total.
        for len in 1..=u16::try_from(MAX_BITS).unwrap() {
            for code in 0..(1u32 << len) {
                let code = u16::try_from(code).unwrap();
                assert_eq!(
                    bi_reverse(code, len),
                    reference_reverse(code, len),
                    "bi_reverse({code}, {len})"
                );
            }
        }
    }

    #[test]
    fn bi_reverse_is_its_own_inverse_on_the_admissible_range() {
        // Reversing twice restores the original, which no single-direction implementation error
        // survives.
        for len in 1..=u16::try_from(MAX_BITS).unwrap() {
            for code in [0u16, 1, 2, 3, 5, 21, 42, 0x7f, 0x155] {
                let code = code & mask(i32::from(len));
                assert_eq!(bi_reverse(bi_reverse(code, len), len), code);
            }
        }
    }

    #[test]
    fn bi_reverse_runs_its_body_at_least_once() {
        // C's `do`/`while` executes before it tests, so `len == 0` leaves `res` at `(code & 1) << 1`
        // and the trailing `>> 1` returns the low bit of `code`. A `while` or `for` loop would
        // return 0 instead. The value is outside the IN assertion; reproducing it is what proves
        // the loop shape was ported rather than approximated.
        assert_eq!(bi_reverse(0, 0), 0);
        assert_eq!(bi_reverse(1, 0), 1);
        assert_eq!(bi_reverse(0xfffe, 0), 0);
        assert_eq!(bi_reverse(0xffff, 0), 1);
    }

    #[test]
    fn bi_reverse_reproduces_the_widest_code() {
        // 15 bits, the widest RFC 1951 3.2.7 permits: 0b100_0000_0000_0001 reverses to itself
        // only if both ends are read, and the asymmetric case pins the direction.
        assert_eq!(bi_reverse(0b000_0000_0000_0001, 15), 0b100_0000_0000_0000);
        assert_eq!(bi_reverse(0b100_0000_0000_0000, 15), 0b000_0000_0000_0001);
        assert_eq!(bi_reverse(0b111_1111_1111_1111, 15), 0b111_1111_1111_1111);
        assert_eq!(bi_reverse(0b101_0101_0101_0101, 15), 0b101_0101_0101_0101);
    }

    #[test]
    fn static_dtree_codes_are_the_five_bit_reversals_of_their_symbols() {
        // `tr_static_init` builds the distance tree with
        // `static_dtree[n].Len = 5; static_dtree[n].Code = bi_reverse((unsigned)n, 5);`
        // (L364-L367). Re-deriving the committed table from this module's `bi_reverse` checks the
        // transcription in `trees/static_tables.rs` and this function against each other.
        for (symbol, entry) in static_dtree.iter().enumerate() {
            let symbol = u16::try_from(symbol).unwrap();
            assert_eq!(entry.len(), 5, "static_dtree[{symbol}].Len");
            assert_eq!(
                entry.code(),
                bi_reverse(symbol, 5),
                "static_dtree[{symbol}].Code"
            );
        }
    }

    #[test]
    fn static_ltree_codes_are_the_canonical_codes_reversed() {
        // The code widths RFC 1951 3.2.6 fixes, as the four `while` loops of `tr_static_init`
        // install them (L353-L356).
        let width = |symbol: usize| -> u16 {
            match symbol {
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            }
        };

        // `gen_codes`' canonical assignment (L214-L217), verbatim.
        let mut bl_count = [0u16; MAX_BITS + 1];
        for symbol in 0..static_ltree.len() {
            bl_count[usize::from(width(symbol))] += 1;
        }
        let mut next_code = [0u16; MAX_BITS + 1];
        let mut code: u16 = 0;
        for bits in 1..=MAX_BITS {
            code = (code + bl_count[bits - 1]) << 1;
            next_code[bits] = code;
        }

        // `tree[n].Code = (ush)bi_reverse(next_code[len]++, len);` (L227).
        for (symbol, entry) in static_ltree.iter().enumerate() {
            let len = width(symbol);
            assert_eq!(entry.len(), len, "static_ltree[{symbol}].Len");

            let slot = usize::from(len);
            let expected = bi_reverse(next_code[slot], len);
            next_code[slot] += 1;
            assert_eq!(entry.code(), expected, "static_ltree[{symbol}].Code");
        }
    }

    // -------------------------------------------------------------------------
    //  put_short -- L144-L147
    // -------------------------------------------------------------------------

    #[test]
    fn put_short_writes_the_low_byte_first() {
        let mut state = default_state();

        assert!(put_short(&mut state, 0x1234));

        // `put_byte(s, (uch)((w) & 0xff)); put_byte(s, (uch)((ush)(w) >> 8));`
        assert_eq!(state.pending.written(), &[0x34, 0x12]);
        assert_eq!(state.pending_bytes(), 2);
        // A write never moves the read cursor; only `flush_pending` does.
        assert_eq!(state.pending.pending_out_offset(), 0);
    }

    #[test]
    fn put_short_covers_both_bytes_over_the_whole_range() {
        let mut state = default_state();
        let values: [u16; 6] = [0x0000, 0x00ff, 0xff00, 0xffff, 0x8001, 0x0180];

        for value in values {
            assert!(put_short(&mut state, value));
        }

        let mut expected = [0u8; 12];
        for (index, value) in values.iter().enumerate() {
            expected[index * 2] = (value & 0xff) as u8;
            expected[index * 2 + 1] = (value >> 8) as u8;
        }
        assert_eq!(state.pending.written(), expected);
    }

    #[test]
    fn put_short_is_the_opposite_order_of_put_short_msb() {
        // The two shorts of `trees.c` and `deflate.c` are deliberately different functions:
        // `put_short` is little-endian for the bitstream, `putShortMSB` is big-endian for the zlib
        // header and trailer (`deflate.c` L939-L942). Confusing them would corrupt every stored
        // block and every check value.
        let mut lsb = default_state();
        assert!(put_short(&mut lsb, 0x789c));

        let mut msb = default_state();
        assert!(crate::deflate::pending::put_short_msb(&mut msb, 0x789c));

        assert_eq!(lsb.pending.written(), &[0x9c, 0x78]);
        assert_eq!(msb.pending.written(), &[0x78, 0x9c]);
    }

    // -------------------------------------------------------------------------
    //  send_bits -- L274-L286
    // -------------------------------------------------------------------------

    #[test]
    fn send_bits_accumulates_without_emitting_below_the_boundary() {
        let mut state = default_state();

        // 3 bits into an empty accumulator: `bi_valid (0) > Buf_size - 3 (13)` is false.
        send_bits(&mut state, 0b101, 3);

        assert_eq!(state.bi_buf, 0b101);
        assert_eq!(state.bi_valid, 3);
        assert!(state.pending.written().is_empty());

        // 4 more bits go in above them, least significant bit of the value first.
        send_bits(&mut state, 0b1001, 4);

        // The three bits already buffered, with the four new ones stacked on top of them.
        assert_eq!(state.bi_buf, 0b0100_1101);
        assert_eq!(state.bi_valid, 7);
        assert!(state.pending.written().is_empty());
    }

    #[test]
    fn send_bits_fills_the_accumulator_at_the_exact_boundary() {
        // `bi_valid == Buf_size - length` is the largest value that does *not* spill, because the
        // comparison is `>` and not `>=`. It is also the only way `bi_valid` reaches 16.
        let mut state = state_holding(0x1fff, BUF_SIZE - 3);

        send_bits(&mut state, 0b101, 3);

        assert_eq!(state.bi_valid, BUF_SIZE);
        assert_eq!(state.bi_buf, 0b101 << 13 | 0x1fff);
        assert!(
            state.pending.written().is_empty(),
            "the boundary case must not emit"
        );
    }

    #[test]
    fn send_bits_spills_a_short_and_keeps_the_residue() {
        // 13 bits pending and 5 more arriving: 3 of the 5 complete the short and 2 remain.
        let mut state = state_holding(0x1555, 13);
        let value: u16 = 0b1_1010;

        send_bits(&mut state, value, 5);

        // `s->bi_buf |= (ush)val << s->bi_valid;` then `put_short(s, s->bi_buf);`
        let short = 0x1555 | (value << 13);
        assert_eq!(
            state.pending.written(),
            &[(short & 0xff) as u8, (short >> 8) as u8]
        );
        // `s->bi_buf = (ush)val >> (Buf_size - s->bi_valid);` -- from the original value, using the
        // pre-update `bi_valid` of 13.
        assert_eq!(state.bi_buf, value >> 3);
        // `s->bi_valid += len - Buf_size;`
        assert_eq!(state.bi_valid, 13 + 5 - BUF_SIZE);
    }

    #[test]
    fn send_bits_spills_from_a_full_accumulator() {
        // `bi_valid == Buf_size` makes the left shift discard the whole value, which is exactly
        // what C's narrowing to `ush` does, and the residue is then the value itself.
        let mut state = state_holding(0xabcd, BUF_SIZE);
        let value: u16 = 0b110;

        send_bits(&mut state, value, 3);

        assert_eq!(state.pending.written(), &[0xcd, 0xab]);
        assert_eq!(state.bi_buf, value, "the value must not be duplicated");
        assert_eq!(state.bi_valid, 3);
    }

    #[test]
    fn send_bits_handles_a_full_width_value() {
        // `length == Buf_size` is the widest the IN assertion admits. From an empty accumulator it
        // does not spill; with a single bit pending it spills and leaves that many bits behind.
        let mut empty = default_state();
        send_bits(&mut empty, 0xbeef, BUF_SIZE);
        assert_eq!(empty.bi_buf, 0xbeef);
        assert_eq!(empty.bi_valid, BUF_SIZE);
        assert!(empty.pending.written().is_empty());

        let mut pending = state_holding(0b1, 1);
        send_bits(&mut pending, 0xbeef, BUF_SIZE);
        let short = 0b1 | (0xbeefu16 << 1);
        assert_eq!(
            pending.pending.written(),
            &[(short & 0xff) as u8, (short >> 8) as u8]
        );
        assert_eq!(pending.bi_buf, 0xbeef >> 15);
        assert_eq!(pending.bi_valid, 1);
    }

    #[test]
    fn send_bits_with_zero_length_changes_nothing() {
        // Reachable through `send_code` for a tree entry whose `Len` is 0, which is every entry of
        // a symbol the block does not use.
        let mut state = state_holding(0b110, 3);

        send_bits(&mut state, 0, 0);

        assert_eq!(state.bi_buf, 0b110);
        assert_eq!(state.bi_valid, 3);
        assert!(state.pending.written().is_empty());
    }

    #[test]
    fn send_bits_round_trips_through_an_independent_bit_reader() {
        // The property that matters: the byte stream is the concatenation of every value's `length`
        // bits, least significant bit first, across both branches and every boundary. 512 pairs of
        // pseudo-random widths in `1..=16` visit the spill branch thousands of times.
        let mut state = default_state();
        let mut rng = Lcg(0x1234_5678);
        let mut written: [(u16, i32); 512] = [(0, 0); 512];

        for slot in &mut written {
            let length = i32::try_from(rng.next() % 16).unwrap() + 1;
            let value = u16::try_from(rng.next() & 0xffff).unwrap() & mask(length);
            *slot = (value, length);
            send_bits(&mut state, value, length);
        }

        // Align, so the trailing partial byte reaches the buffer.
        bi_windup(&mut state);

        let bytes = state.pending.written();
        let mut reader = BitReader::new(bytes);
        for (index, (value, length)) in written.iter().enumerate() {
            assert_eq!(
                reader.take(*length),
                *value,
                "element {index} of the bit stream"
            );
        }

        // Nothing beyond the last element may have been emitted except the padding of the final
        // byte: the total is the bit count rounded up.
        let bits: usize = written
            .iter()
            .map(|(_, length)| usize::try_from(*length).unwrap())
            .sum();
        assert_eq!(bytes.len(), bits.div_ceil(8));
    }

    #[test]
    fn send_bits_keeps_the_accumulator_invariant() {
        // "All bits above the last valid bit are always zero" (`deflate.h` L271-L272) and
        // `bi_valid` stays in `0..=Buf_size`. Both are relied on by `bi_flush`, `bi_windup` and
        // `deflatePrime`, and both are checked after every one of a long run of calls.
        let mut state = default_state();
        let mut rng = Lcg(0x0bad_c0de);

        for _ in 0..2_000 {
            let length = i32::try_from(rng.next() % 16).unwrap() + 1;
            let value = u16::try_from(rng.next() & 0xffff).unwrap() & mask(length);
            send_bits(&mut state, value, length);

            assert!((0..=BUF_SIZE).contains(&state.bi_valid));
            assert_eq!(
                state.bi_buf & !mask(state.bi_valid),
                0,
                "bits above bi_valid must be zero"
            );
        }
    }

    // -------------------------------------------------------------------------
    //  bi_flush -- L166-L176
    // -------------------------------------------------------------------------

    #[test]
    fn bi_flush_covers_every_threshold() {
        // The three-way structure at every interesting width, plus the post-condition that at most
        // 7 bits remain (L163-L164).
        for valid in 0..=BUF_SIZE {
            let pattern = 0xa5a5u16 & mask(valid);
            let mut state = state_holding(pattern, valid);

            bi_flush(&mut state);

            if valid == BUF_SIZE {
                // `put_short(s, s->bi_buf); s->bi_buf = 0; s->bi_valid = 0;`
                assert_eq!(
                    state.pending.written(),
                    &[(pattern & 0xff) as u8, (pattern >> 8) as u8],
                    "bi_valid == 16"
                );
                assert_eq!(state.bi_buf, 0);
                assert_eq!(state.bi_valid, 0);
            } else if valid >= 8 {
                // `put_byte(s, (Byte)s->bi_buf); s->bi_buf >>= 8; s->bi_valid -= 8;`
                assert_eq!(
                    state.pending.written(),
                    &[(pattern & 0xff) as u8],
                    "bi_valid == {valid}"
                );
                assert_eq!(state.bi_buf, pattern >> 8);
                assert_eq!(state.bi_valid, valid - 8);
            } else {
                // Neither branch: a partial byte cannot be written without inventing padding.
                assert!(
                    state.pending.written().is_empty(),
                    "bi_valid == {valid} must not emit"
                );
                assert_eq!(state.bi_buf, pattern);
                assert_eq!(state.bi_valid, valid);
            }

            assert!(
                state.bi_valid <= 7,
                "bi_flush must leave at most 7 bits, left {} for {valid}",
                state.bi_valid
            );
        }
    }

    #[test]
    fn bi_flush_does_not_touch_bi_used() {
        // Only `bi_windup` maintains `bi_used` (L187); a flush that also wrote it would make
        // `deflateUsed` report a boundary that was never reached.
        let mut state = state_holding(0xffff, BUF_SIZE);
        state.bi_used = 3;

        bi_flush(&mut state);

        assert_eq!(state.bi_used, 3);
    }

    #[test]
    fn repeated_bi_flush_drains_a_full_accumulator_one_byte_at_a_time() {
        // 15 valid bits: the first call moves the low byte out and shifts the remaining 7 down.
        let mut state = state_holding(0x7fff, 15);

        bi_flush(&mut state);
        assert_eq!(state.pending.written(), &[0xff]);
        assert_eq!(state.bi_buf, 0x7f);
        assert_eq!(state.bi_valid, 7);

        // Below 8 bits the second call is a no-op, exactly as the C `else if` chain falls through.
        bi_flush(&mut state);
        assert_eq!(state.pending.written(), &[0xff]);
        assert_eq!(state.bi_buf, 0x7f);
        assert_eq!(state.bi_valid, 7);
    }

    // -------------------------------------------------------------------------
    //  bi_windup -- L181-L193
    // -------------------------------------------------------------------------

    #[test]
    fn bi_windup_reports_bi_used_for_every_bit_count() {
        // `s->bi_used = ((s->bi_valid - 1) & 7) + 1;` in C's signed `int`, tabulated for every
        // width the accumulator can hold. The `bi_valid == 0` case is the one an unsigned type gets
        // wrong -- or panics on in a debug build -- and it must be 8.
        let expected: [i32; 17] = [8, 1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8];

        for valid in 0..=BUF_SIZE {
            let pattern = 0x5a5au16 & mask(valid);
            let mut state = state_holding(pattern, valid);
            state.bi_used = -1;

            bi_windup(&mut state);

            let slot = usize::try_from(valid).unwrap();
            assert_eq!(
                state.bi_used, expected[slot],
                "bi_used for bi_valid == {valid}"
            );
            assert_eq!(state.bi_buf, 0);
            assert_eq!(state.bi_valid, 0);

            if valid > 8 {
                // `put_short(s, s->bi_buf);`
                assert_eq!(
                    state.pending.written(),
                    &[(pattern & 0xff) as u8, (pattern >> 8) as u8],
                    "bi_valid == {valid}"
                );
            } else if valid > 0 {
                // `put_byte(s, (Byte)s->bi_buf);` -- the byte is padded with the zeros above
                // `bi_valid`.
                assert_eq!(
                    state.pending.written(),
                    &[(pattern & 0xff) as u8],
                    "bi_valid == {valid}"
                );
            } else {
                assert!(
                    state.pending.written().is_empty(),
                    "an empty accumulator must emit nothing"
                );
            }
        }
    }

    #[test]
    fn bi_windup_on_an_empty_accumulator_is_a_pure_alignment() {
        // The path `_tr_flush_block` takes when the last block already ended on a boundary
        // (L1081-L1083): no byte is emitted, and `bi_used` still reports a full byte.
        let mut state = default_state();

        bi_windup(&mut state);

        assert!(state.pending.written().is_empty());
        assert_eq!(state.bi_used, 8);
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_buf, 0);
    }

    #[test]
    fn bi_windup_pads_a_partial_byte_with_zeros() {
        // 3 valid bits become one byte whose top 5 bits are zero, which is what makes the output
        // byte-aligned without inventing data.
        let mut state = state_holding(0b101, 3);

        bi_windup(&mut state);

        assert_eq!(state.pending.written(), &[0b0000_0101]);
        assert_eq!(state.bi_used, 3);
    }

    // -------------------------------------------------------------------------
    //  send_code -- L238-L240
    // -------------------------------------------------------------------------

    #[test]
    fn send_code_sends_the_code_and_the_length_of_the_entry() {
        let mut state = default_state();
        // Eight bits, so the byte is complete and the emitted value is directly comparable.
        let tree = [CtData::new(0b1010_1100, 8)];

        send_code(&mut state, 0, &tree);
        bi_flush(&mut state);

        assert_eq!(state.pending.written(), &[0b1010_1100]);
        assert_eq!(state.bi_valid, 0);
    }

    #[test]
    fn send_code_reads_code_and_len_and_not_freq_or_dad() {
        // `Code` shares storage with `Freq` and `Len` with `Dad` (`deflate.h` L71-L86), so an entry
        // written through the frequency accessors must read back identically through the code
        // accessors -- and `send_code` must use the latter.
        let mut entry = CtData::default();
        entry.set_freq(0b0110);
        entry.set_dad(4);
        assert_eq!(entry.code(), 0b0110);
        assert_eq!(entry.len(), 4);

        let mut state = default_state();
        send_code(&mut state, 0, &[entry]);

        assert_eq!(state.bi_buf, 0b0110);
        assert_eq!(state.bi_valid, 4);
    }

    #[test]
    fn send_code_of_an_unused_symbol_emits_nothing() {
        let mut state = state_holding(0b11, 2);

        // A zeroed entry is a symbol with no code, which `init_block` leaves behind for every
        // symbol the block does not use (L444-L446).
        send_code(&mut state, 0, &[CtData::default()]);
        // Past the end of the tree: not reachable from a correct caller, and a non-event rather
        // than a panic if it ever were.
        send_code(&mut state, 7, &[CtData::new(0xff, 8)]);

        assert_eq!(state.bi_buf, 0b11);
        assert_eq!(state.bi_valid, 2);
        assert!(state.pending.written().is_empty());
    }

    // -------------------------------------------------------------------------
    //  The primitives in concert
    // -------------------------------------------------------------------------

    #[test]
    fn tr_align_emits_the_ten_bits_it_documents() {
        // `_tr_align` (L887-L894) verbatim: "Send one empty static block to give enough lookahead
        // for inflate. This takes 10 bits, of which 7 may remain in the bit buffer." (L883-L886)
        //
        //     send_bits(s, STATIC_TREES<<1, 3);
        //     send_code(s, END_BLOCK, static_ltree);
        //     bi_flush(s);
        let mut state = default_state();

        send_bits(&mut state, u16::from(STATIC_TREES) << 1, 3);
        send_code(&mut state, END_BLOCK, &static_ltree);
        bi_flush(&mut state);

        // 3 bits of block header, then the 7-bit end-of-block code, which is 0 in the static tree:
        // 0b0000_0010 with the remaining 2 bits still buffered.
        assert_eq!(state.pending.written(), &[0x02]);
        assert_eq!(state.bi_valid, 2);
        assert_eq!(state.bi_buf, 0);
    }

    #[test]
    fn a_final_empty_static_block_is_the_two_bytes_zlib_emits() {
        // The tail `_tr_flush_block` writes for an empty final block coded with the static trees
        // (L1058-L1062, then `if (last) bi_windup(s)` at L1081-L1083):
        //
        //     send_bits(s, (STATIC_TREES<<1) + last, 3);
        //     compress_block(...)   -- for empty input, only the END_BLOCK code
        //     bi_windup(s);
        //
        // Those ten bits are 0x03 0x00, which is exactly what the reference library produces
        // between the two-byte zlib header and the four-byte Adler-32 when it compresses no input
        // at all: 78 9c 03 00 00 00 00 01.
        let mut state = default_state();

        send_bits(&mut state, (u16::from(STATIC_TREES) << 1) + 1, 3);
        send_code(&mut state, END_BLOCK, &static_ltree);
        bi_windup(&mut state);

        assert_eq!(state.pending.written(), &[0x03, 0x00]);
        assert_eq!(state.bi_used, 2);
        assert_eq!(state.bi_valid, 0);
    }

    #[test]
    fn a_stored_block_header_is_aligned_and_little_endian() {
        // `_tr_stored_block` (L860-L866) up to the payload copy:
        //
        //     send_bits(s, (STORED_BLOCK<<1) + last, 3);
        //     bi_windup(s);
        //     put_short(s, (ush)stored_len);
        //     put_short(s, (ush)~stored_len);
        //
        // RFC 1951 3.2.4 requires the LEN/NLEN pair to be byte-aligned and one's complement.
        let mut state = default_state();
        let stored_len: u16 = 0x1234;

        send_bits(&mut state, 0, 3); // (STORED_BLOCK << 1) + 0
        bi_windup(&mut state);
        put_short(&mut state, stored_len);
        put_short(&mut state, !stored_len);

        assert_eq!(state.pending.written(), &[0x00, 0x34, 0x12, 0xcb, 0xed]);
        assert_eq!(state.bi_valid, 0);
    }

    #[test]
    fn a_run_of_static_codes_decodes_back_to_its_symbols() {
        // Literals through the static tree, which is how a `Z_FIXED` block is written
        // (`compress_block`, L900-L949). Reading them back requires reversing each code again,
        // because the tree stores them pre-reversed -- the round trip therefore exercises
        // `bi_reverse`, `send_code` and the accumulator together.
        let symbols: [usize; 8] = [
            usize::from(b'h'),
            usize::from(b'e'),
            usize::from(b'l'),
            usize::from(b'l'),
            usize::from(b'o'),
            0,
            255,
            END_BLOCK,
        ];
        let mut state = default_state();

        for symbol in symbols {
            send_code(&mut state, symbol, &static_ltree);
        }
        bi_windup(&mut state);

        let bytes = state.pending.written();
        let mut reader = BitReader::new(bytes);
        for symbol in symbols {
            let entry = static_ltree[symbol];
            let length = i32::from(entry.len());
            assert_eq!(reader.take(length), entry.code(), "symbol {symbol}");
        }
    }
}
