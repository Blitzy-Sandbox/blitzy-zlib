//! The pending-output buffer: bytes the compressor has produced but not yet handed back.
//!
//! Six things the reference implementation keeps together around `pending_buf` are implemented here:
//!
//! | C item | C source | Here |
//! |---|---|---|
//! | `put_byte` | `deflate.h` L290-L293 | [`put_byte`] |
//! | `putShortMSB` | `deflate.c` L934-L942 | [`put_short_msb`] |
//! | `flush_pending` | `deflate.c` L944-L968 | [`flush_pending`] |
//! | `deflatePending`'s accounting | `deflate.c` L722-L734 | [`deflate_pending`] |
//! | `deflateUsed`'s accounting | `deflate.c` L737-L742 | [`deflate_used`] |
//! | `deflatePrime`'s overlay guard | `deflate.c` L756-L757 | [`has_prime_room`] |
//!
//! The buffer itself is not here. It is one allocation owned by
//! [`DeflateState::pending`](crate::deflate::state::DeflateState) and viewed through
//! [`crate::weak_slice::PendingBuf`], which holds the three C members `pending_buf`,
//! `pending_out` and `pending` (`deflate.h` L107-L110) together with the `sym_buf` cursors that
//! share the same bytes (L230-L254). This module is the *policy* over that view: which byte goes
//! in, in what order, and when the accumulated bytes are copied out to the caller.
//!
//! # The buffer is one allocation, overlaid, and must never be resized
//!
//! `deflateInit2_` makes a single request of `LIT_BUFS * lit_bufsize` bytes
//! (`deflate.c` L505), sets `pending_buf_size = lit_bufsize * 4` (L506) and then places the
//! symbol buffer *inside* it at `sym_buf = pending_buf + lit_bufsize` (L520). Pending output and
//! the symbols the block writer has yet to consume therefore live in the same bytes, and the
//! 38-line argument at `deflate.c` L466-L503 is what proves they cannot collide: the longest
//! fixed-code length/distance pair is 31 bits, each consumed symbol frees 24 bits of symbol
//! buffer, `sym_buf` starts `8 * lit_bufsize` bits in, and at least 139 bits of slack therefore
//! remain between the write cursor and the next unread symbol.
//!
//! Nothing in this module allocates, reallocates, resizes, re-boxes or replaces that buffer, and
//! nothing may be added that does. Two pieces of code elsewhere in the implementation are statements about
//! one address space and would be silently invalidated by a second allocation: `deflate_stored`
//! patches the four length bytes of a dummy stored block in place, at the four indices ending at
//! `pending` (`deflate.c` L1716-L1719), and `deflatePrime` refuses a stream whose pending output
//! has reached the symbols by comparing the two cursors' *addresses* (L756-L757), which is
//! [`has_prime_room`]. Sizing and allocation belong to `deflate/state.rs`; releasing belongs to
//! the blocks themselves.
//!
//! # `flush_pending` is the choke point for output
//!
//! The comment above the C function says how much traffic passes through it
//! (`deflate.c` L944-L949):
//!
//! ```text
//! Flush as much pending output as possible. All deflate() output, except for
//! some deflate_stored() output, goes through this function so some
//! applications may wish to modify it to avoid allocating a large
//! strm->next_out buffer and copying into it. (See also read_buf()).
//! ```
//!
//! Thirteen call sites in `deflate.c` reach it: the top-of-call drain (L1002), the zlib and gzip
//! header writers (L1059, L1085, L1128, L1151, L1173, L1190), the block boundary (L1203), the
//! post-flush drain (L1255), the trailer (L1284), the `FLUSH_BLOCK_ONLY` macro (L1637) and
//! `deflate_stored` twice (L1722, L1841). The single exception the comment names is
//! `deflate_stored`'s direct `next_in`-to-`next_out` copy, which is
//! [`crate::read_buf::read_buf_into_output`].
//!
//! Its input-side twin is `read_buf` (`deflate.c` L219-L240), which the same comment
//! cross-references and which lives in [`crate::read_buf`]. The division is deliberate: that
//! function funnels every byte *in* and owns the check value; this one funnels almost every byte
//! *out* and owns the pending cursors.
//!
//! # Two behavioural requirements, not implementation details
//!
//! Both of the following are load-bearing. They are stated here, and again on
//! [`flush_pending`], so that a later refactor cannot mistake either for tidiable incidental
//! structure.
//!
//! **1. The bit buffer is flushed before the copy, never after.** `_tr_flush_bits`
//! (`deflate.c` L954, `trees.c` L880-L882) drains whole bytes out of `bi_buf` into `pending_buf`;
//! only then is `len` computed and the copy made. Deferring it until after the copy, or omitting
//! it because "the caller will flush eventually", withholds up to one complete byte from a caller
//! that supplied space for it -- and the bytes a caller does not receive are exactly the bytes
//! `test/example.c` waits for when it drives `deflate()` with a one-byte `avail_out`.
//!
//! **2. The read cursor rewinds when, and only when, the buffer empties.**
//! `if (s->pending == 0) s->pending_out = s->pending_buf;` (`deflate.c` L965-L967) is the only
//! place `pending_out` moves backwards. Because `put_byte` writes at `pending_buf[pending]` --
//! an index from the *base* of the allocation, not from `pending_out` -- a `pending_out` that
//! never rewound would march forward across the buffer until it reached `sym_buf` and the write
//! and read regions overlapped. The reset is what makes the buffer reusable across calls, and
//! the `len == 0` early return above it is what keeps the reset from firing on a call that moved
//! nothing.
//!
//! # Why the bit-flush step is injected rather than imported
//!
//! `_tr_flush_bits` is `trees.c`'s (L880-L882), and `trees` is a *sibling* of `deflate` in this
//! implementation rather than something beneath it: `trees.c`'s only `#include` is `"deflate.h"`, so the
//! Huffman coder mutates the same [`DeflateState`](crate::deflate::state::DeflateState) this
//! module does. [`flush_pending`] therefore takes the step as a parameter and calls it first,
//! inside itself. Two things follow, and both are the point:
//!
//! * requirement 1 above is enforced by this function for every caller, instead of being
//!   re-remembered at each of the thirteen call sites; and
//! * this module needs no dependency on `trees`, so the `deflate`/`trees` pair keeps an acyclic
//!   module graph while still sharing one state struct.
//!
//! There is deliberately **no** variant of [`flush_pending`] that skips the step. A caller that
//! genuinely has no bits to flush passes a step that does nothing, which is a decision visible at
//! its call site rather than a second entry point that invites the wrong choice.
//!
//! # What is deliberately elsewhere
//!
//! * **`put_short`** -- the *least*-significant-byte-first short (`trees.c` L144-L147) -- is not
//!   here. It is declared in `trees.c`, not in `deflate.h`, and its callers are `send_bits`,
//!   `bi_flush`, `bi_windup` and `_tr_stored_block` (`trees.c` L264, L279, L168, L183 and
//!   L864-L865), so it belongs to `trees/bit_writer.rs` with them. Only
//!   the MSB-first form, which is `deflate.c`'s own (L939-L942), is implemented here, and no
//!   `put_short_lsb` or `put_u32_le` convenience is offered: the little-endian fields of the gzip
//!   header and trailer are written as explicit `put_byte` sequences in C
//!   (`deflate.c` L1069-L1081, L1269-L1276) and stay explicit [`put_byte`] sequences in
//!   `deflate/mod.rs`.
//! * **`deflateStateCheck`** (`deflate.c` L538-L556) and the two null out-parameter tests of
//!   `deflatePending` (L724, L726) are the exported wrapper's, not this module's.
//!   [`deflate_pending`] reports both values unconditionally and cannot know whether the caller
//!   asked for them; deciding which to write through a raw pointer is the facade's job, and it is
//!   the only layer that has a pointer to test.
//! * **The bit accumulator** `bi_buf`/`bi_valid`/`bi_used` (`deflate.h` L266-L276) is written by
//!   `trees`. This module only *reports* `bi_valid` and `bi_used`, because that is all
//!   `deflatePending` and `deflateUsed` do with them.
//! * **`deflatePrime`'s bit insertion** (`deflate.c` L760-L769) is `deflate/mod.rs`'s. Only the
//!   half of its guard that is a statement about the pending buffer is here.
//!
//! # Integer widths
//!
//! C works in four types across these functions: `pending` and `pending_buf_size` are `ulg`,
//! `len` and `avail_out` are `unsigned`, `bi_valid` and `bi_used` are `int`, and `total_out` is
//! `uLong`. This module uses the width each value needs rather than modelling `uLong`, exactly as
//! [`crate::read_buf`] does:
//!
//! | Value | Type here | Why |
//! |---|---|---|
//! | `pending`, `pending_out`, `len` | `usize` | Offsets into, and lengths within, one slice |
//! | `bi_valid`, `bi_used` | `i32` | C declares both `int`, and `deflatePending`/`deflateUsed` write through an `int *` |
//! | `total_out` | `u64` | Wide enough for every target's `uLong`, so nothing is lost before the facade narrows it |
//! | `deflatePending`'s `*pending` | `u32` | C writes through an `unsigned *`; see [`deflate_pending`] for the narrowing probe |
//!
//! Reconciling any of these with the caller's `c_ulong` is the facade's job and its alone.
//!
//! # Safety and failure posture
//!
//! No `unsafe`, no raw pointer and no pointer arithmetic: the crate root's
//! `#![forbid(unsafe_code)]` makes that compiler-enforced rather than claimed, and every access
//! goes through a bounds-checked view. Nothing here can panic in a release build either. There is
//! no `[]` indexing, no `unwrap`, no `expect` and no arithmetic that can overflow: the two
//! additions use [`usize::saturating_add`] and [`u64::wrapping_add`], the latter because C's
//! `total_out += len` on an unsigned type wraps and this implementation reproduces that rather than
//! diverging from it.
//!
//! Two invariants that C states as IN assertions are re-checked with [`debug_assert!`] instead,
//! so a violation is loud in a test build and impossible to turn into a release-mode abort inside
//! a library that a C caller has linked: the room test [`put_byte`] carries
//! (`deflate.h` L290-L292) and the agreement between the length [`flush_pending`] computes and
//! the length actually copied.

// The Rust counterpart of `flush_pending` and of `deflatePending`'s accounting must keep the names their C
// originals have, and both end with this module's own name. That is a naming collision with the
// C source's vocabulary, not a naming problem to solve: renaming either would break the
// one-to-one correspondence with `deflate.c` that the byte-identical-output requirement is
// verified against. `deflate/state.rs` allows the same lint for the same reason.
#![allow(clippy::module_name_repetitions)]

use core::mem::size_of;

use crate::deflate::state::{Allocator, DeflateState, BUF_SIZE};
use crate::error::ReturnCode;
use crate::read_buf::OutputCursor;

/// [`flush_pending`] adds a byte count to a `u64` total, and this implementation's standard forbids a cast
/// that could truncate.
///
/// [`crate::read_buf`] asserts the same relationship for the same reason, at the same cost of
/// nothing: on a hypothetical target with a `usize` wider than `u64` this crate fails to compile
/// rather than silently losing output accounting.
const _: () = assert!(
    size_of::<usize>() <= size_of::<u64>(),
    "flush_pending converts a usize byte count to u64; usize must not be the wider type"
);

/// Bytes of pending output that `deflatePrime` insists lie ahead of the read cursor, which is
/// `(Buf_size + 7) >> 3` in `deflate.c` L757 -- the bit buffer rounded up to whole bytes.
///
/// Written as the literal 2 and tied to `Buf_size` by the assertion below, rather than computed,
/// because `Buf_size` is an `i32` in this implementation (it is `int` arithmetic against `bi_valid`
/// everywhere else, per [`BUF_SIZE`]) and converting it here would introduce a cast into a
/// constant whose value is settled at compile time anyway. See [`has_prime_room`].
const PRIME_HEADROOM_BYTES: usize = 2;

/// Keeps [`PRIME_HEADROOM_BYTES`] honest: the whole expression, in C's own integer type.
///
/// A build failure here means `Buf_size` has changed and the literal above must be updated to
/// match; it does not mean the assertion is wrong.
const _: () = assert!(
    (BUF_SIZE + 7) >> 3 == 2,
    "PRIME_HEADROOM_BYTES is (Buf_size + 7) >> 3 from deflate.c L757"
);

/// Appends one byte of compressed output to the pending buffer.
///
/// Mirrors `put_byte` (`deflate.h` L290-L293), which is a macro and not a function:
///
/// ```text
/// /* Output a byte on the stream.
///  * IN assertion: there is enough room in pending_buf.
///  */
/// #define put_byte(s, c) {s->pending_buf[s->pending++] = (Bytef)(c);}
/// ```
///
/// The byte lands at `pending_buf[pending]` -- an index from the **base** of the allocation, not
/// from `pending_out` -- and `pending` then advances. That is C's indexing exactly, and it is why
/// the read cursor has to rewind when the buffer empties; see requirement 2 in the module
/// documentation.
///
/// # The IN assertion
///
/// C's contract is that the caller has already established there is room, and C does nothing to
/// check it: the macro would write one past the end of the allocation. Every call site in
/// `deflate.c` discharges the obligation, either because the writes are bounded by
/// `pending_buf_size` in a loop that flushes when it fills (the gzip name and comment
/// writers, `deflate.c` L1144-L1186) or because the buffer is known to be empty at that point
/// ("Compression must start with an empty pending buffer", L1058).
///
/// This implementation keeps the obligation and removes the hazard. The [`debug_assert!`] states the
/// assertion where a reader of `deflate.h` L291 will look for it, and the underlying
/// [`crate::weak_slice::PendingBuf::put_byte`] is bounds-checked, so a violation in a release
/// build declines the write and reports `false` instead of corrupting the symbol buffer or
/// whatever follows the allocation.
///
/// # Return value
///
/// `true` when the byte was stored, `false` when the buffer was already full. C's macro is a
/// statement and yields nothing, and a caller that has discharged the IN assertion may ignore
/// this in exactly the same spirit -- it is deliberately not `#[must_use]`, because the header
/// writer performs runs of these and a forest of `let _ =` would obscure the correspondence with
/// the C source. A caller that has *not* established room, such as one writing a variable-length
/// field, should test it and flush.
#[inline]
pub(crate) fn put_byte<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, byte: u8) -> bool {
    // `IN assertion: there is enough room in pending_buf.` (`deflate.h` L290-L292). The write
    // index is `pending`, so "enough room" is exactly one free slot at that index.
    debug_assert!(
        state.pending_bytes() < state.pending_buf_size(),
        "put_byte with no room in pending_buf (deflate.h L290-L292)"
    );

    state.pending.put_byte(byte)
}

/// Appends a 16-bit value to the pending buffer, most significant byte first.
///
/// Mirrors `putShortMSB` (`deflate.c` L934-L942), whose own precondition is that the stream
/// state is correct and `pending_buf` has room for both bytes.
///
/// Three kinds of call site use it, and every one of them is big-endian by specification rather
/// than by convention, so the order is not negotiable:
///
/// * the two-byte zlib header, `putShortMSB(s, header)` (`deflate.c` L1048) -- RFC 1950 defines
///   `CMF` then `FLG` in that order and requires that they, "when viewed as a 16-bit unsigned
///   integer stored in MSB order (CMF*256 + FLG), is a multiple of 31"
///   (`doc/rfc1950.txt` L216, L275-L277);
/// * the Adler-32 of a preset dictionary, written as two shorts (`deflate.c` L1052-L1053); and
/// * the zlib trailer's Adler-32, likewise (`deflate.c` L1281-L1282), which RFC 1950 specifies is
///   stored "in most-significant-byte first (network) order" (`doc/rfc1950.txt` L327-L329).
///
/// # Why the parameter is 32 bits wide
///
/// C declares it `uInt`, which is `typedef unsigned int uInt; /* 16 bits or more */`
/// (`zconf.h` L405), and then masks. That width is load-bearing at two of the three kinds of
/// call site, which pass
/// `(uInt)(strm->adler >> 16)` and `(uInt)(strm->adler & 0xffff)` from a `uLong` check value
/// (`deflate.c` L1052-L1053, L1281-L1282): the value handed over is a *derived* 16-bit quantity,
/// not something the type system guarantees is 16 bits. Taking `u32` and masking here mirrors
/// that, and means a caller cannot narrow the check value differently from C by accident. Callers
/// that already hold a `u16` may pass `u32::from(value)`.
///
/// Both masks are C's own: `(Byte)(b >> 8)` keeps bits 8 to 15 and `(Byte)(b & 0xff)` keeps bits
/// 0 to 7, so any bits above 15 are discarded exactly as the two `(Byte)` casts discard them.
///
/// # Return value
///
/// `true` when both bytes were stored. `false` means the buffer filled, in which case the high
/// byte may have been written and the low byte not -- the same partial write C's two-statement
/// macro expansion would perform. As with [`put_byte`], the IN assertion makes this unreachable
/// from a correct caller: every C call site writes into a buffer it has just emptied.
#[inline]
pub(crate) fn put_short_msb<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    value: u32,
) -> bool {
    // `put_byte(s, (Byte)(b >> 8));` -- the mask is what the `(Byte)` cast performs, and it also
    // bounds the value so that the narrowing cannot lose anything the C code keeps.
    let high = ((value >> 8) & 0xff) as u8;
    // `put_byte(s, (Byte)(b & 0xff));`
    let low = (value & 0xff) as u8;

    // Written in C's order, high byte first. `&&` short-circuits, so a full buffer stops after
    // the first byte rather than counting a second one it did not store; C would write both and
    // run off the end, which is the hazard the checked view removes.
    put_byte(state, high) && put_byte(state, low)
}

/// Appends a 16-bit value to the pending buffer, least significant byte first.
///
/// Port of the `put_short` macro (`trees.c` L144-L147), whose comment is "Output a short LSB
/// first on the stream", and the little-endian counterpart of [`put_short_msb`]:
///
/// ```text
/// #define put_short(s, w) { \
///     put_byte(s, (uch)((w) & 0xff)); \
///     put_byte(s, (uch)((ush)(w) >> 8)); \
/// }
/// ```
///
/// This is the *bitstream* order -- RFC 1951 §3.1.1 packs the bit accumulator from the least
/// significant bit upwards, so the low byte of a filled accumulator is the one that goes out
/// first -- and it is the opposite of [`put_short_msb`]'s, which serves the RFC 1950 header and
/// trailer. Confusing the two would corrupt every stored block and every check value.
///
/// # Why this is a function rather than two `put_byte` calls
///
/// It is the innermost write of the compressor: [`crate::trees::bit_writer::send_bits`] spills
/// the accumulator through here whenever it fills, which for an incompressible block is roughly
/// once per input byte. Two `put_byte` calls establish the same buffer bound twice; this
/// establishes it once and writes both bytes together, which also makes the write
/// all-or-nothing rather than leaving a lone low byte behind on a full buffer -- the same
/// discipline [`crate::weak_slice::PendingBuf::put_short_msb`] already applies.
///
/// # Return value
///
/// `true` when both bytes were stored, `false` when fewer than two bytes remained, in which case
/// nothing was written. As with [`put_byte`], the C macro's IN assertion makes `false`
/// unreachable from a correct caller.
#[inline]
pub(crate) fn put_short_lsb<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    value: u16,
) -> bool {
    // The same "IN assertion: there is enough room in pending_buf" the `put_byte` macro carries
    // (`deflate.h` L290-L292), stated for both bytes at once.
    debug_assert!(
        state.pending_bytes() + 2 <= state.pending_buf_size(),
        "put_short with fewer than two free bytes in pending_buf (deflate.h L290-L292)"
    );

    state.pending.put_short_le(value)
}
/// Copies as much pending output as the caller's buffer will take, and accounts for it.
///
/// Mirrors `flush_pending` (`deflate.c` L944-L968).
///
/// # Parameters, and how they map onto the C signature
///
/// C takes only `z_streamp` and reaches everything else through it, including `strm->state`. This
/// implementation has no `strm` back-pointer in the state (see `deflate/state.rs`), so everything the C
/// function reaches through the stream arrives as its own parameter:
///
/// | Here | There |
/// |---|---|
/// | `state` | `strm->state`: `pending`, `pending_out`, `pending_buf`, and `bi_buf`/`bi_valid` via the bit flush |
/// | `output` | `strm->next_out` and `strm->avail_out`, as one cursor |
/// | `total_out` | `strm->total_out` |
/// | `flush_bits` | the call to `_tr_flush_bits` (`deflate.c` L954) |
///
/// `deflate/mod.rs` supplies `trees`' `_tr_flush_bits` implementation as the last argument:
///
/// ```text
/// flush_pending(state, output, total_out, crate::trees::flush_bits);
/// ```
///
/// See the module documentation for why the step is a parameter rather than an import. A caller
/// that passes a step which does not drain the bit buffer changes the emitted byte stream, so the
/// argument is not a policy knob: there is exactly one correct value for it in the library, and
/// only tests have any business passing anything else.
///
/// # Statement order is part of the contract
///
/// 1. **Flush the bit buffer** -- `_tr_flush_bits(s)` (L954). First, unconditionally, before
///    `len` exists. `trees.c` L880-L882 defines it as `bi_flush(s)`, which moves whole bytes out
///    of `bi_buf` and leaves at most seven bits behind, so running it later would hold back a
///    byte the caller has room for and running it not at all would hold back up to a byte at
///    every block boundary.
/// 2. **Clamp** -- `len = s->pending > strm->avail_out ? strm->avail_out : s->pending`
///    (L955-L956), which is [`Ord::min`] over the two. Computed *after* the flush, because the
///    flush is what decides how many bytes there are to copy.
/// 3. **Early return** -- `if (len == 0) return;` (L957). Nothing below runs, and in particular
///    the rewind in step 6 does not: a call that moves no byte must leave both cursors exactly
///    where it found them. This is the path taken whenever `avail_out` is zero, and the state in
///    which `deflate()` sets `last_flush = -1` and returns `Z_OK` so that the caller comes back
///    for the rest (L1255-L1259).
/// 4. **Copy** -- `zmemcpy(strm->next_out, s->pending_out, len)` (L959), which `zutil.h` L216
///    defines as plain `memcpy`. The source starts at `pending_out`, not at the base of the
///    allocation; a second flush after a partial one therefore delivers the *tail* of the pending
///    output rather than repeating its head.
/// 5. **Advance the caller's cursor** -- `strm->next_out += len; strm->avail_out -= len;`
///    (L960, L963), which this implementation holds as one number, so the intermediate state in which one
///    half has moved and the other has not is not expressible. Nothing between them reads either,
///    exactly as in [`crate::read_buf::read_buf`].
/// 6. **Account for the flushed bytes** -- `s->pending_out += len; s->pending -= len;` and the
///    rewind `if (s->pending == 0) s->pending_out = s->pending_buf;` (L961, L964, L965-L967),
///    which are [`crate::weak_slice::PendingBuf::consume_flushed`].
/// 7. **Add to the total** -- `strm->total_out += len;` (L962).
///
/// Steps 5 to 7 are C's five assignments in a different textual order. That is unobservable and
/// nothing more: the copy has already happened, the five values are independent of one another,
/// and no code runs between them. What is *not* reordered is anything that a later step reads --
/// the flush before the clamp, the clamp before the early return, and the early return before the
/// rewind.
///
/// # Return value
///
/// The number of bytes copied, which is C's `len`. C returns `void` and its callers re-derive the
/// outcome by testing `strm->avail_out == 0` afterwards (`deflate.c` L1003, L1256, L1644);
/// returning the count adds no effect they could not already observe, and it lets a caller that
/// wants the count avoid comparing cursor positions to obtain it. Zero means either that there was
/// nothing pending or that the caller supplied no space, which `deflate()` tells apart by looking
/// at `pending` itself rather than at the count (`deflate.c` L1058-L1062, L1284-L1289).
#[inline]
pub(crate) fn flush_pending<'a, A, F>(
    state: &mut DeflateState<'a, A>,
    output: &mut OutputCursor<'_>,
    total_out: &mut u64,
    flush_bits: F,
) -> usize
where
    A: Allocator<'a>,
    F: FnOnce(&mut DeflateState<'a, A>),
{
    // 1. `_tr_flush_bits(s);`  (`deflate.c` L954)
    //
    // BEHAVIOURAL REQUIREMENT, not incidental ordering: this drains whole bytes from `bi_buf`
    // into the pending buffer, so it must precede the length computation below. See the module
    // documentation.
    flush_bits(state);

    // 2. `len = s->pending > strm->avail_out ? strm->avail_out : (unsigned)s->pending;`
    //    (`deflate.c` L955-L956)
    let len = state.pending_bytes().min(output.remaining());

    // 3. `if (len == 0) return;`  (`deflate.c` L957)
    //
    // Load-bearing: it is what keeps step 6's rewind from running on a call that moved nothing.
    if len == 0 {
        return 0;
    }

    // 4 and 5. `zmemcpy(strm->next_out, s->pending_out, len);` (L959) together with
    // `strm->next_out += len;` (L960) and `strm->avail_out -= len;` (L963).
    //
    // The source is `flushable()`, which is `pending_out` for `pending` bytes; `push_slice`
    // copies the smaller of that length and the space left, which is the same `len` computed
    // above, and advances the cursor by exactly what it copied.
    let copied = output.push_slice(state.pending.flushable());
    debug_assert_eq!(
        copied, len,
        "flush_pending copied a different length than it clamped to (deflate.c L955-L959)"
    );

    // 6. `s->pending_out += len;` (L961), `s->pending -= len;` (L964) and the rewind
    //    `if (s->pending == 0) s->pending_out = s->pending_buf;` (L965-L967).
    //
    // BEHAVIOURAL REQUIREMENT: the rewind is the only place the read cursor moves backwards, and
    // without it `pending_out` would advance until it met `sym_buf`. It lives in
    // `PendingBuf::consume_flushed` because it is a statement about that buffer's two cursors.
    let accounted = state.pending.consume_flushed(copied);
    debug_assert!(
        accounted,
        "flush_pending flushed more than was pending (deflate.c L964)"
    );

    // 7. `strm->total_out += len;`  (`deflate.c` L962)
    //
    // Widening guarded by the compile-time assertion at the top of this module, so the cast
    // cannot truncate. Wrapping because `total_out` is an unsigned C type and overflows by
    // wrapping; reproducing that is more faithful than diverging on a stream no caller reaches.
    *total_out = total_out.wrapping_add(copied as u64);

    copied
}

/// Narrows a pending-byte count to the `unsigned` that `deflatePending` writes through, reporting
/// whether anything was lost.
///
/// This is C's self-comparison, isolated so that it can be exercised directly
/// (`deflate.c` L727-L731).
///
/// # It is a portability probe, not dead code
///
/// `s->pending` is a `ulg` (`deflate.h` L110) and `*pending` is an `unsigned` (`zlib.h` L787), so
/// the assignment narrows. Where `unsigned` is the narrower type the comparison catches the
/// truncation, and `zlib.h` L798-L801 documents precisely when that happens: "If an int is 16 bits
/// and memLevel is 9, then it is possible for the number of pending bytes to not fit in an
/// unsigned. In that case `Z_BUF_ERROR` is returned and `*pending` is set to the maximum value of
/// an unsigned." A 16-bit target with `memLevel` 9 has `pending_buf_size` of 131072, which a
/// 16-bit `unsigned` cannot hold.
///
/// The same narrowing exists in this implementation, because `pending` is a `usize` and the value reported
/// is a `u32`, so the branch is reachable by type rather than merely retained for symmetry.
/// **No live stream can take it**: `pending <= pending_buf_size = lit_bufsize * 4 <= 131072`
/// (`deflate.c` L506 with `lit_bufsize <= 32768`), which fits a `u32` on every target this implementation
/// builds for. It is kept anyway, exactly as C keeps it, so that a future target with a narrower
/// count reports what `zlib.h` says it reports rather than silently handing back a wrong number.
///
/// `u32::MAX` is C's `(unsigned)-1`: the maximum value of the type being written, which is what
/// the documented contract calls for.
fn narrow_pending(pending: usize) -> (u32, ReturnCode) {
    match u32::try_from(pending) {
        Ok(bytes) => (bytes, ReturnCode::OK),
        // `*pending = (unsigned)-1; return Z_BUF_ERROR;`  (`deflate.c` L729-L730)
        Err(_) => (u32::MAX, ReturnCode::BUF_ERROR),
    }
}

/// Reports the output that has been generated but not yet handed to the caller.
///
/// Mirrors the body of `deflatePending` (`deflate.c` L722-L734).
///
/// `zlib.h` L790-L795 states what the two numbers mean: the bytes are output "generated, but not
/// yet provided in the available output ... due to the available output space having being
/// consumed", and the bits are "between 0 and 7, where they await more bits to join them in order
/// to fill out a full byte".
///
/// # Returns
///
/// `(bytes, bits, status)`:
///
/// * `bytes` -- `pending` narrowed to the `unsigned` the caller's out-parameter has, or `u32::MAX`
///   if that narrowing lost information. See [`narrow_pending`].
/// * `bits` -- `bi_valid` (`deflate.h` L270-L273) verbatim, which is C's `int`.
/// * `status` -- [`ReturnCode::OK`], or [`ReturnCode::BUF_ERROR`] when `bytes` was clamped.
///
/// # What the caller still owes
///
/// All three of the C function's tests are deliberately absent here, because every one of them is
/// about a *pointer* and this crate has none:
///
/// * `deflateStateCheck(strm)` (L723) is the facade's tag validation of the opaque `state`
///   pointer; reaching this function at all means a valid state was already found.
/// * `bits != Z_NULL` and `pending != Z_NULL` (L724, L726) are the caller's own tests. Both values
///   are computed unconditionally and returned together, so a facade with only one out-parameter
///   simply discards the other. That is not a divergence: neither computation has any effect, and
///   C evaluates the bits assignment before the pending one regardless of which pointers are null.
///
/// One ordering subtlety survives from C and is preserved by returning all three values at once:
/// when the narrowing fails, C has *already* written `*bits` before it returns `Z_BUF_ERROR`, so a
/// caller that passed both pointers gets a valid bit count alongside the error. The facade
/// reproduces that by writing both out-parameters and then returning the status.
#[inline]
pub(crate) fn deflate_pending<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
) -> (u32, i32, ReturnCode) {
    // `*pending = (unsigned)strm->state->pending;` with its overflow probe (L727-L731).
    let (bytes, status) = narrow_pending(state.pending_bytes());

    // `*bits = strm->state->bi_valid;`  (`deflate.c` L725)
    (bytes, state.bi_valid, status)
}

/// Reports the number of bits used in the last byte at the most recent flush to a byte boundary.
///
/// Mirrors the body of `deflateUsed` (`deflate.c` L737-L742).
///
/// The value is `bi_used`, "last number of used bits when going to a byte boundary"
/// (`deflate.h` L274-L276), which `bi_windup` maintains as `((s->bi_valid - 1) & 7) + 1`
/// (`trees.c` L187). `zlib.h` L807-L810 documents the range: "The result is in 1..8, or 0 if there
/// has not yet been a flush", and it "helps determine the location of the last bit of a deflate
/// stream" -- which is what makes it useful to a caller that will later resume with
/// `deflatePrime`.
///
/// There is no error path. The state check and the null-pointer test are the facade's, exactly as
/// for [`deflate_pending`], and once a valid state is in hand C's remaining behaviour is a single
/// read and `Z_OK`; the exported wrapper supplies that constant rather than this function
/// returning a status that could only ever be one value.
#[inline]
pub(crate) fn deflate_used<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> i32 {
    state.bi_used
}

/// Whether the pending output still leaves room ahead of it for `deflatePrime` to insert bits.
///
/// Mirrors the pending-buffer half of `deflatePrime`'s guard, the shipped non-`LIT_MEM` branch
/// (`deflate.c` L756-L758).
///
/// This function is the second disjunct, negated: it answers `true` when the stream may be primed
/// and `false` when `deflatePrime` must return [`ReturnCode::BUF_ERROR`]. The `bits` range test is
/// argument validation and stays with `deflatePrime` in `deflate/mod.rs`.
///
/// # What the comparison means
///
/// It is a comparison of two *addresses* inside one allocation, which is only meaningful because
/// `pending_buf` and `sym_buf` are one block (`deflate.c` L505, L520). Priming pushes up to 16
/// bits into `bi_buf`, and flushing those bits writes up to `(Buf_size + 7) >> 3` -- two -- bytes
/// at the write cursor. The guard refuses when the read cursor has advanced so far that those two
/// bytes could land at or beyond `sym_buf`, which would overwrite symbols the block writer has
/// not consumed yet.
///
/// Expressed as offsets from the base of the allocation, which is how
/// [`crate::weak_slice::PendingBuf`] holds both cursors, the C test becomes
/// `sym_buf_offset < pending_out_offset + 2` and this function is its negation. Callers get the
/// whole predicate rather than the two offsets precisely so that the comparison cannot be written
/// the wrong way round at the call site; the offsets themselves remain available as
/// [`crate::weak_slice::PendingBuf::sym_buf_offset`] and
/// [`crate::weak_slice::PendingBuf::pending_out_offset`] for anything else that needs them.
///
/// # In practice
///
/// A freshly reset stream has `pending_out` at zero and `sym_buf` at `lit_bufsize`, which is at
/// least 128 (`deflate.c` L464 with `memLevel >= 1`), so the answer is `true`. It becomes `false`
/// only after enough partial flushes have advanced the read cursor to within two bytes of the
/// symbol buffer -- which is why `zlib.h` L823-L824 restricts `deflatePrime` to "before the first
/// `deflate()` call after a `deflateInit2()` or `deflateReset()`".
#[inline]
pub(crate) fn has_prime_room<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> bool {
    // `s->pending_out + ((Buf_size + 7) >> 3)`, as an offset. Saturating so that the addition has
    // no panicking path at all; `pending_out_offset() <= pending_buf_size() <= 131072`, so it
    // cannot come near saturating in any live stream.
    let barrier = state
        .pending
        .pending_out_offset()
        .saturating_add(PRIME_HEADROOM_BYTES);

    // The negation of `s->sym_buf < barrier`.
    state.pending.sym_buf_offset() >= barrier
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// harness: a test asserts, and a failing assertion panics. Indexing is allowed because every
// index below is a literal bound established by the fixture on the line above it. The same
// relaxation, for the same reason, appears in `deflate/state.rs` and `deflate/hash_chain.rs`.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{
        deflate_pending, deflate_used, flush_pending, has_prime_room, narrow_pending, put_byte,
        put_short_msb, PRIME_HEADROOM_BYTES,
    };
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL,
        MIN_MEM_LEVEL,
    };
    use crate::error::ReturnCode;
    use crate::read_buf::OutputCursor;

    /// `#define Z_DEFLATED 8` (`zlib.h` L213), the only compression method the format defines.
    ///
    /// Written out rather than read from [`Method::as_raw`] in the header formula below, so that
    /// the expectation does not come from the same place as the value under test.
    const Z_DEFLATED: u32 = 8;

    /// The stream `deflateInit2_` produces for the default configuration: level 6, `windowBits`
    /// 15, `memLevel` 8, [`Strategy::Default`], as `deflateInit2_` sizes it (`deflate.c` L387-L533).
    ///
    /// The buffers arrive filled with `0xa5`, exactly as `test/infcover.c` L87 fills the blocks it
    /// hands out, so nothing below may assume a zeroed pending buffer.
    fn default_state() -> DeflateState<'static, GlobalAllocator> {
        state_with_mem_level(DEF_MEM_LEVEL)
    }

    /// [`default_state`] with `memLevel` chosen, which is what sizes the overlaid buffer:
    /// `lit_bufsize = 1 << (memLevel + 6)` and `pending_buf_size = lit_bufsize * 4`
    /// (`deflate.c` L464, L506).
    fn state_with_mem_level(mem_level: i32) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: 15,
            mem_level,
            strategy: Strategy::Default,
        };

        DeflateState::new(config, GlobalAllocator).unwrap()
    }

    /// A bit-flush step that does nothing.
    ///
    /// The stand-in for `_tr_flush_bits` (`trees.c` L880-L882) in a fixture whose `bi_buf` holds
    /// no bits, which is the state `deflateResetKeep` leaves and the state every test here that
    /// writes with [`put_byte`] is in. `bi_flush` would move nothing in that case
    /// (`trees.c` L166-L177 acts only when `bi_valid >= 8`), so the no-op is the faithful stand-in
    /// and not a shortcut around requirement 1.
    fn no_bits(_state: &mut DeflateState<'static, GlobalAllocator>) {}

    /// Appends `bytes` one at a time through [`put_byte`], asserting each write was accepted.
    fn put_bytes(state: &mut DeflateState<'static, GlobalAllocator>, bytes: &[u8]) {
        for &byte in bytes {
            assert!(
                put_byte(state, byte),
                "put_byte refused a byte with room left"
            );
        }
    }

    #[test]
    fn put_byte_appends_at_the_write_cursor() {
        let mut state = default_state();
        assert_eq!(state.pending_bytes(), 0);

        put_bytes(&mut state, &[0x1f, 0x8b, 0x08]);

        // `s->pending_buf[s->pending++] = c` three times.
        assert_eq!(state.pending_bytes(), 3);
        assert_eq!(state.pending.written(), &[0x1f, 0x8b, 0x08]);
        // The read cursor is untouched by a write; only `flush_pending` moves it.
        assert_eq!(state.pending.pending_out_offset(), 0);
    }

    #[test]
    fn put_byte_writes_at_a_base_relative_index() {
        // C's macro indexes from the base of the allocation, `pending_buf[s->pending++]`
        // (`deflate.h` L293), not from `pending_out`. The two are the same number until a partial
        // flush separates them, so this fixture separates them and pins which one is used.
        let mut state = default_state();
        let payload: [u8; 8] = [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17];
        put_bytes(&mut state, &payload);

        let mut narrow = [0u8; 3];
        let mut output = OutputCursor::new(&mut narrow);
        let mut total_out = 0u64;
        assert_eq!(
            flush_pending(&mut state, &mut output, &mut total_out, no_bits),
            3
        );
        assert_eq!(state.pending.pending_out_offset(), 3);
        assert_eq!(state.pending_bytes(), 5);

        assert!(put_byte(&mut state, 0xff));

        // The byte landed at index 5 -- `pending` -- and not at index 8, which is where
        // `pending_out + pending` points. `written()` is `pending_buf[..pending]`.
        assert_eq!(state.pending_bytes(), 6);
        assert_eq!(state.pending.written()[5], 0xff);
        assert_eq!(&state.pending.written()[..5], &payload[..5]);
        // `deflate()` never reaches this state -- it drains the buffer completely before writing
        // more (`deflate.c` L1058-L1063) -- but the indexing rule is C's either way, and the
        // rewind in `flush_pending` is what keeps it harmless.
    }

    #[test]
    fn put_short_msb_writes_the_high_byte_first() {
        let mut state = default_state();

        assert!(put_short_msb(&mut state, 0x1234));

        // `put_byte(s, (Byte)(b >> 8)); put_byte(s, (Byte)(b & 0xff));`
        assert_eq!(state.pending.written(), &[0x12, 0x34]);
        assert_eq!(state.pending_bytes(), 2);
    }

    #[test]
    fn put_short_msb_masks_exactly_as_the_byte_casts_do() {
        let mut state = default_state();

        // C takes a `uInt`, so bits above 15 are simply discarded by the two `(Byte)` casts:
        // `(Byte)(0xfedcba >> 8)` is 0xdc and `(Byte)(0xfedcba & 0xff)` is 0xba.
        assert!(put_short_msb(&mut state, 0x00fe_dcba));

        assert_eq!(state.pending.written(), &[0xdc, 0xba]);
    }

    #[test]
    fn zlib_header_word_is_emitted_msb_first() {
        let mut state = default_state();
        // The `PRESET_DICT` term at `deflate.c` L1045 is conditional on `s->strstart != 0`, and a
        // freshly initialised stream has not loaded a dictionary.
        assert_eq!(state.window.strstart, 0);

        // `deflate.c` L1033-L1046, transcribed:
        //   uInt header = (Z_DEFLATED + ((s->w_bits - 8) << 4)) << 8;
        let mut header = (Z_DEFLATED + ((state.w_bits() - 8) << 4)) << 8;
        //   level_flags = 0 / 1 / 2 / 3 by strategy and level
        let level_flags = if state.favours_huffman_only() || state.level() < 2 {
            0
        } else if state.level() < 6 {
            1
        } else if state.level() == 6 {
            2
        } else {
            3
        };
        //   header |= (level_flags << 6);   (L1044)
        header |= level_flags << 6;
        //   header += 31 - (header % 31);   (L1046)
        header += 31 - (header % 31);

        // RFC 1950 L275-L277 requires that CMF*256 + FLG be a multiple of 31, which is what the
        // adjustment above is for.
        assert_eq!(header % 31, 0);
        // The familiar two bytes of a default zlib stream, which every conforming decoder and the
        // whole differential corpus expect to see first.
        assert_eq!(header, 0x789c);

        assert!(put_short_msb(&mut state, header));
        assert_eq!(state.pending.written(), &[0x78, 0x9c]);
    }

    #[test]
    fn the_header_drains_one_byte_at_a_time_exactly_as_the_c_library_does() {
        // Measured against the reference implementation, driven through the public API with
        // `avail_out` pinned to 1 and the cursors read back with `deflatePending`/`deflateUsed`
        // after every call:
        //
        //   step=0  out[0]=78  total_out=1  pending=1  bits=0  used=0
        //   step=1  out[1]=9c  total_out=2  pending=0  bits=0  used=0
        //   step=2  out[2]=cb  total_out=3  pending=11 bits=0  used=7
        //
        // Three separate properties of this module are pinned by that trace, and this test
        // reproduces it at the primitive level:
        //
        //   * step 0 proves the clamp -- one byte of a two-byte short, not both;
        //   * step 1 proves the resumption -- 0x9c, the *tail*, and not 0x78 again;
        //   * step 2 proves the rewind -- the compressed data that follows is written at
        //     `pending_buf[0]` by `put_byte`, so a `pending_out` still sitting at 2 would have
        //     handed the caller stale bytes instead of the new ones.
        let mut state = default_state();
        let mut total_out = 0u64;

        assert!(put_short_msb(&mut state, 0x789c));
        assert_eq!(state.pending_bytes(), 2);

        // step 0
        let mut first = [0u8; 1];
        {
            let mut output = OutputCursor::new(&mut first);
            assert_eq!(
                flush_pending(&mut state, &mut output, &mut total_out, no_bits),
                1
            );
        }
        assert_eq!(first, [0x78]);
        assert_eq!(total_out, 1);
        assert_eq!(deflate_pending(&state), (1, 0, ReturnCode::OK));
        assert_eq!(deflate_used(&state), 0);
        assert_eq!(state.pending.pending_out_offset(), 1);

        // step 1
        let mut second = [0u8; 1];
        {
            let mut output = OutputCursor::new(&mut second);
            assert_eq!(
                flush_pending(&mut state, &mut output, &mut total_out, no_bits),
                1
            );
        }
        assert_eq!(second, [0x9c]);
        assert_eq!(total_out, 2);
        assert_eq!(deflate_pending(&state), (0, 0, ReturnCode::OK));
        // The rewind, which is what makes step 2 below deliver new data.
        assert_eq!(state.pending.pending_out_offset(), 0);

        // step 2: the first byte of the compressed data the reference emits for
        // "hello, hello!" at level 6.
        assert!(put_byte(&mut state, 0xcb));
        let mut third = [0u8; 1];
        {
            let mut output = OutputCursor::new(&mut third);
            assert_eq!(
                flush_pending(&mut state, &mut output, &mut total_out, no_bits),
                1
            );
        }
        assert_eq!(third, [0xcb]);
        assert_eq!(total_out, 3);
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.pending.pending_out_offset(), 0);
    }

    #[test]
    fn flush_pending_runs_the_bit_flush_before_the_copy() {
        // Requirement 1, made observable: the pending buffer starts empty, so the only byte that
        // can reach the output is one the bit-flush step contributes. A step called after the copy
        // -- or not called at all -- would leave the output empty.
        let mut state = default_state();
        let mut buffer = [0u8; 4];
        let mut output = OutputCursor::new(&mut buffer);
        let mut total_out = 0u64;

        let copied = flush_pending(&mut state, &mut output, &mut total_out, |state| {
            assert!(put_byte(state, 0xc3));
        });

        assert_eq!(copied, 1);
        assert_eq!(output.written_slice(), &[0xc3]);
        assert_eq!(total_out, 1);
        // Fully drained, so the read cursor has rewound.
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.pending.pending_out_offset(), 0);
    }

    #[test]
    fn flush_pending_runs_the_bit_flush_even_with_no_output_space() {
        // `_tr_flush_bits(s)` precedes the `len` computation in C, so it runs whether or not
        // anything can be copied: the drained byte stays in the pending buffer for the next call.
        let mut state = default_state();
        let mut empty: [u8; 0] = [];
        let mut output = OutputCursor::new(&mut empty);
        let mut total_out = 7u64;

        let copied = flush_pending(&mut state, &mut output, &mut total_out, |state| {
            assert!(put_byte(state, 0xc3));
        });

        assert_eq!(copied, 0);
        assert_eq!(state.pending_bytes(), 1);
        assert_eq!(state.pending.pending_out_offset(), 0);
        assert_eq!(
            total_out, 7,
            "total_out must not move when nothing is copied"
        );
    }

    #[test]
    fn flush_pending_with_no_output_space_moves_nothing() {
        let mut state = default_state();
        put_bytes(&mut state, &[0xaa, 0xbb]);

        let mut empty: [u8; 0] = [];
        let mut output = OutputCursor::new(&mut empty);
        let mut total_out = 0u64;

        assert_eq!(
            flush_pending(&mut state, &mut output, &mut total_out, no_bits),
            0
        );

        // `if (len == 0) return;` -- every cursor is exactly where it was.
        assert_eq!(state.pending_bytes(), 2);
        assert_eq!(state.pending.pending_out_offset(), 0);
        assert_eq!(state.pending.written(), &[0xaa, 0xbb]);
        assert_eq!(output.written(), 0);
        assert_eq!(total_out, 0);
    }

    #[test]
    fn flush_pending_resumes_from_the_read_cursor_and_rewinds_when_drained() {
        let mut state = default_state();
        let payload: [u8; 8] = [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];
        put_bytes(&mut state, &payload);

        let lit_bufsize = state.lit_bufsize();
        let pending_buf_size = state.pending_buf_size();
        let sym_buf_offset = state.pending.sym_buf_offset();
        let sym_end = state.pending.sym_end();
        let mut total_out = 0u64;

        // Stage 1: less output space than pending output. `len` clamps to `avail_out`.
        let mut narrow = [0u8; 3];
        {
            let mut output = OutputCursor::new(&mut narrow);
            assert_eq!(
                flush_pending(&mut state, &mut output, &mut total_out, no_bits),
                3
            );
            assert_eq!(output.written_slice(), &payload[..3]);
            assert_eq!(output.remaining(), 0);
        }
        assert_eq!(state.pending_bytes(), 5);
        // The read cursor advanced and was NOT rewound: `pending` is still non-zero.
        assert_eq!(state.pending.pending_out_offset(), 3);
        assert_eq!(total_out, 3);

        // Stage 2: room to spare. The copy resumes from `pending_out`, delivering the tail rather
        // than repeating the head, and the rewind fires because `pending` reaches zero.
        let mut wide = [0u8; 16];
        {
            let mut output = OutputCursor::new(&mut wide);
            assert_eq!(
                flush_pending(&mut state, &mut output, &mut total_out, no_bits),
                5
            );
            assert_eq!(output.written_slice(), &payload[3..]);
        }
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.pending.pending_out_offset(), 0);
        assert_eq!(total_out, 8);

        // Nothing resized the overlaid allocation: `pending_buf` and `sym_buf` are still one
        // block of the size `deflateInit2_` chose, with the symbol cursors untouched.
        assert_eq!(state.lit_bufsize(), lit_bufsize);
        assert_eq!(state.pending_buf_size(), pending_buf_size);
        assert_eq!(state.pending.sym_buf_offset(), sym_buf_offset);
        assert_eq!(state.pending.sym_end(), sym_end);
        assert_eq!(state.pending.sym_next(), 0);
    }

    #[test]
    fn flush_pending_with_exactly_enough_space_drains_and_rewinds() {
        let mut state = default_state();
        let payload: [u8; 5] = [0x30, 0x31, 0x32, 0x33, 0x34];
        put_bytes(&mut state, &payload);

        let mut buffer = [0u8; 5];
        let mut output = OutputCursor::new(&mut buffer);
        let mut total_out = u64::from(u32::MAX);

        assert_eq!(
            flush_pending(&mut state, &mut output, &mut total_out, no_bits),
            5
        );

        assert_eq!(output.written_slice(), &payload);
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.pending.pending_out_offset(), 0);
        // `total_out` is a running count, not a per-call one.
        assert_eq!(total_out, u64::from(u32::MAX) + 5);
    }

    #[test]
    fn flush_pending_on_an_empty_buffer_is_a_no_op() {
        let mut state = default_state();
        let mut buffer = [0u8; 8];
        let mut output = OutputCursor::new(&mut buffer);
        let mut total_out = 0u64;

        assert_eq!(
            flush_pending(&mut state, &mut output, &mut total_out, no_bits),
            0
        );

        assert_eq!(output.written(), 0);
        assert_eq!(total_out, 0);
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.pending.pending_out_offset(), 0);
    }

    #[test]
    fn deflate_pending_reports_the_byte_and_bit_counts() {
        let mut state = default_state();

        // A fresh stream has produced nothing and holds no bits.
        assert_eq!(deflate_pending(&state), (0, 0, ReturnCode::OK));

        put_bytes(&mut state, &[0x01, 0x02, 0x03]);
        // `*bits = strm->state->bi_valid;` -- the 0..7 residue `zlib.h` L793-L794 describes.
        state.bi_valid = 5;

        assert_eq!(deflate_pending(&state), (3, 5, ReturnCode::OK));
    }

    #[test]
    fn deflate_pending_reports_a_full_buffer_of_the_smallest_memory_level() {
        // `memLevel` 1 gives `lit_bufsize` 128 and `pending_buf_size` 512 (`deflate.c` L464,
        // L506), which is small enough to fill byte by byte and confirms that the reported count
        // is the whole buffer rather than something clamped.
        let mut state = state_with_mem_level(MIN_MEM_LEVEL);
        assert_eq!(state.lit_bufsize(), 128);
        assert_eq!(state.pending_buf_size(), 512);

        for index in 0..state.pending_buf_size() {
            assert!(put_byte(&mut state, u8::try_from(index & 0xff).unwrap()));
        }

        assert_eq!(deflate_pending(&state), (512, 0, ReturnCode::OK));
        // The buffer is full, so a further write is refused rather than overrunning. The
        // `debug_assert!` in `put_byte` guards this case, so the refusal is exercised through the
        // view it delegates to.
        assert!(!state.pending.put_byte(0xff));
        assert_eq!(state.pending_bytes(), 512);
    }

    #[test]
    fn deflate_used_reports_the_last_byte_boundary_width() {
        let mut state = default_state();

        // "0 if there has not yet been a flush" (`zlib.h` L808-L809).
        assert_eq!(deflate_used(&state), 0);

        // `bi_windup` sets `bi_used = ((bi_valid - 1) & 7) + 1` (`trees.c` L187), so the reported
        // value is in 1..=8; 8 is what a whole final byte looks like.
        state.bi_used = 8;
        assert_eq!(deflate_used(&state), 8);

        state.bi_used = 3;
        assert_eq!(deflate_used(&state), 3);
    }

    #[test]
    fn narrow_pending_probes_the_unsigned_narrowing() {
        assert_eq!(narrow_pending(0), (0, ReturnCode::OK));

        // The largest count any live stream can hold: `pending_buf_size` at `memLevel` 9 is
        // 32768 * 4 (`deflate.c` L464, L506). Well inside a `u32`, which is why the error arm
        // below is unreachable from a real stream on this target.
        assert_eq!(narrow_pending(131_072), (131_072, ReturnCode::OK));

        // Exactly `u32::MAX` fits, so C's `*pending != pending` comparison is false and the call
        // succeeds. The sentinel is therefore ambiguous in C too -- which is why the status, not
        // the value, is what a caller must test.
        let max_u32 = usize::try_from(u32::MAX).unwrap();
        assert_eq!(narrow_pending(max_u32), (u32::MAX, ReturnCode::OK));

        // One past it truncates. Skipped, rather than asserted, on a target whose `usize` is not
        // wider than `u32`: there the branch is unreachable by type, exactly as C's is on a target
        // whose `unsigned` is as wide as its `ulg`.
        if let Ok(oversized) = usize::try_from(u64::from(u32::MAX) + 1) {
            assert_eq!(narrow_pending(oversized), (u32::MAX, ReturnCode::BUF_ERROR));
            assert_eq!(
                narrow_pending(usize::MAX),
                (u32::MAX, ReturnCode::BUF_ERROR)
            );
        }
    }

    #[test]
    fn has_prime_room_matches_the_c_guard() {
        let mut state = default_state();
        let sym_buf_offset = state.pending.sym_buf_offset();
        // `s->sym_buf = s->pending_buf + s->lit_bufsize;`  (`deflate.c` L520)
        assert_eq!(sym_buf_offset, state.lit_bufsize());
        assert_eq!(PRIME_HEADROOM_BYTES, 2);

        // A freshly initialised stream can always be primed, which is the state `zlib.h`
        // L823-L824 tells callers to use `deflatePrime` in.
        assert_eq!(state.pending.pending_out_offset(), 0);
        assert!(has_prime_room(&state));

        // The last read-cursor position that still leaves the two bytes of headroom:
        // `sym_buf == pending_out + 2` is not less than the barrier, so the guard permits it.
        assert!(state
            .pending
            .set_pending_out_offset(sym_buf_offset - PRIME_HEADROOM_BYTES));
        assert!(has_prime_room(&state));

        // One byte further and `sym_buf < pending_out + 2` holds: `deflatePrime` must refuse.
        assert!(state
            .pending
            .set_pending_out_offset(sym_buf_offset - PRIME_HEADROOM_BYTES + 1));
        assert!(!has_prime_room(&state));

        // And at the symbol buffer itself, plainly refused.
        assert!(state.pending.set_pending_out_offset(sym_buf_offset));
        assert!(!has_prime_room(&state));
    }
}
