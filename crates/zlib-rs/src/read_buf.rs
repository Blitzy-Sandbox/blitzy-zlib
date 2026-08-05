//! The stream-facing cursors, and the input drain every byte the compressor sees passes
//! through.
//!
//! # What this module replaces
//!
//! A `z_stream` describes the caller's two buffers as a pointer and a count apiece --
//! `next_in`/`avail_in` and `next_out`/`avail_out` (`zlib.h` L91-L95) -- and the reference
//! implementation walks both with raw pointer arithmetic, incrementing one member while
//! decrementing the other and relying on convention to keep the pair consistent. This
//! module models each pair as a **borrowed slice plus an integer cursor** instead, so the
//! two halves cannot drift apart: there is only ever one number, and the slice's own length
//! bounds it.
//!
//! Three things live here, and nothing else:
//!
//! | Item | Ported from | Role |
//! |---|---|---|
//! | [`InputCursor`] | `next_in` / `avail_in` (`zlib.h` L91-L92) | The unconsumed input |
//! | [`OutputCursor`] | `next_out` / `avail_out` (`zlib.h` L94-L95) | The unwritten output |
//! | `read_buf` | `read_buf` (`deflate.c` L219-L240) | Drain input, update the check value |
//!
//! # `read_buf` is the compressor's single input funnel
//!
//! The comment above the C function states the reason this small operation matters so much
//! (`deflate.c` L212-L217):
//!
//! ```text
//! Read a new buffer from the current input stream, update the adler32
//! and total number of bytes read.  All deflate() input goes through
//! this function so some applications may wish to modify it to avoid
//! allocating a large strm->next_in buffer and copying from it.
//! (See also flush_pending()).
//! ```
//!
//! "All `deflate()` input goes through this function" makes it the **only** place the
//! compression side advances `total_in` or folds a byte into `strm->adler`. Both are
//! caller-visible members of `z_stream`, both are printed and asserted on by
//! `test/example.c`, and the finished check value is written verbatim into the zlib or gzip
//! trailer. An arithmetic slip here is therefore not a local defect: it corrupts the trailer
//! of every stream this port produces and shows up as a wrong total in the existing test
//! suite. That is why the operation below is a statement-for-statement transcription rather
//! than a re-derivation.
//!
//! # The three call sites the API has to serve
//!
//! The signature is shaped by how the reference implementation actually calls `read_buf`,
//! which is with a destination that is *not* always part of the deflate state:
//!
//! | `deflate.c` | Caller | Destination |
//! |---|---|---|
//! | L311 | `fill_window` | `s->window + s->strstart + s->lookahead` -- window free space |
//! | L1746 | `deflate_stored` | `s->strm->next_out` -- **the caller's output buffer** |
//! | L1816 | `deflate_stored` | `s->window + s->strstart` -- window free space |
//!
//! Because L1746 copies input straight through to the output buffer, the destination cannot
//! be typed as a window view; it is a plain `&mut [u8]`, which every one of the three sites
//! can supply. [`OutputCursor::unwritten_up_to`] exists precisely so that the L1746 shape is
//! expressible without indexing, and `read_buf_into_output` packages the five lines around
//! it (`deflate.c` L1745-L1750) so that no caller can copy through the output buffer and
//! then forget to advance it.
//!
//! `deflate_stored` also reads *backwards* past the cursor -- `zmemcpy(s->window,
//! s->strm->next_in - s->w_size, s->w_size)` at `deflate.c` L1766 rebuilds the window
//! history from input that has already been consumed. [`InputCursor::consumed_tail`] serves
//! that access without reviving a pointer.
//!
//! # What is deliberately *not* here
//!
//! * **`flush_pending`** (`deflate.c` L950-L968), the symmetric output-side operation the C
//!   comment cross-references, belongs to `deflate/pending.rs`: it flushes the *pending
//!   buffer*, which is deflate state, and it calls `_tr_flush_bits` first. Only its copy
//!   step is a cursor operation, and that step is [`OutputCursor::push_slice`].
//! * **`deflate_stored`'s algorithm** (`deflate.c` L1668) stays in
//!   `deflate/algorithm/stored.rs`. This module supplies the primitives it composes, not any
//!   of its block-sizing or flush decisions.
//! * **Views over deflate's own buffers** -- the window, the pending buffer and the hash
//!   arrays -- belong to `weak_slice.rs`. The division is by ownership: that module views
//!   buffers the *library* allocated, this one views buffers the *caller* supplied.
//!
//! # Integer widths, and where the C types are reconciled
//!
//! The C function works in three different integer types: `len` and `size` are `unsigned`,
//! while `strm->total_in` and `strm->adler` are both `uLong`. `uLong` is `unsigned long`
//! (`zconf.h` L406), which is 64 bits on an LP64 target and 32 bits on LLP64 Windows, so
//! substituting any fixed-width integer for it would silently corrupt one platform or the
//! other.
//!
//! This module therefore does not model `uLong` at all. It uses the width each value
//! actually needs:
//!
//! | Value | Type here | Why |
//! |---|---|---|
//! | Byte counts and cursor positions | `usize` | The natural index type for a slice |
//! | The running check value | `u32` | Adler-32 and CRC-32 are both 32-bit quantities whatever `uLong` is |
//! | `total_in` / `total_out` | `u64` | Wide enough for every target's `uLong`, so no accounting is lost before the facade sees it |
//!
//! **Reconciling these with the caller's `c_ulong` is `crates/libz-rs-sys/src/types.rs`'s
//! job, and its alone.** On a 32-bit `uLong` target the facade's narrowing of a `u64` total
//! to `c_ulong` reproduces exactly the wraparound the C code gets from `total_in += len` on a
//! 32-bit `unsigned long`, which is why the totals here use wrapping addition rather than a
//! checked one: the arithmetic is faithful to C's unsigned overflow, not merely tolerant of
//! it.
//!
//! No cast in this module can truncate. The one conversion that is not a widening between
//! equal-width types, `usize` to `u64`, is guarded by a compile-time assertion below.
//!
//! # The empty-slice contract with the facade
//!
//! A C caller may legitimately present `avail_in == 0` together with a **null** `next_in`,
//! and `zlib.h` L1813-L1814 documents the same overload for the checksum entry points.
//! Turning that pair into a zero-length slice is the facade's responsibility -- it is the one
//! place raw pointers exist, and reconstructing the caller's buffers is the second of the six
//! unsafe-site categories the port's plan enumerates.
//!
//! This module's half of the contract is to **tolerate a zero-length slice everywhere**. A
//! cursor over an empty slice is a valid cursor: it reports zero bytes remaining, hands out
//! empty slices, refuses to advance, and drives `read_buf` straight into the `len == 0`
//! early return. Nothing in this module indexes, so an empty buffer cannot be a special
//! case that was forgotten.
//!
//! # Panic freedom
//!
//! The crate root asserts `#![forbid(unsafe_code)]`, so the absence of raw pointers here is
//! compiler-enforced rather than claimed. Panic freedom is enforced structurally instead:
//! every accessor either clamps its argument or returns [`Option`], no `[]` indexing appears
//! anywhere in the module, and `read_buf` resolves both of its slice views *before* it
//! mutates anything, so the impossible failure of a bound check it has already established
//! leaves the stream untouched and reports zero bytes transferred -- which is exactly what
//! the C function reports when it moves nothing.

use core::mem::size_of;

use crate::adler32::adler32;
use crate::crc32::crc32;

/// `usize` is never wider than `u64` on any target this crate supports.
///
/// [`read_buf`] converts a byte count into the `u64` it adds to a total, and the port's
/// standard forbids a cast that could truncate. Asserting the relationship at compile time
/// turns "provably lossless" into a property the build checks rather than a comment a
/// reviewer has to trust: on a hypothetical target with a wider `usize` this crate would
/// fail to compile instead of silently losing input accounting.
const _: () = assert!(
    size_of::<usize>() <= size_of::<u64>(),
    "read_buf converts a usize byte count to u64; usize must not be the wider type"
);

/// `deflate_state.wrap` for a zlib stream: two-byte header, Adler-32 trailer.
///
/// `deflate.c` L228 tests `strm->state->wrap == 1` and folds the bytes just copied into
/// `strm->adler` with `adler32`. RFC 1950 defines that check value over the uncompressed data
/// (`doc/rfc1950.txt` L325-L329).
const WRAP_ZLIB: i32 = 1;

/// `deflate_state.wrap` for a gzip stream: RFC 1952 header and trailer, CRC-32 check value.
///
/// `deflate.c` L232 tests `strm->state->wrap == 2` and folds the bytes just copied into
/// `strm->adler` with `crc32` instead. **This arm is not optional.** It sits behind `#ifdef
/// GZIP`, and `deflate.h` L21-L24 defines `GZIP` unless the build opts out with `NO_GZIP`:
///
/// ```text
/// #ifndef NO_GZIP
/// #  define GZIP
/// #endif
/// ```
///
/// The shipped configuration therefore compiles it in, and this port implements it
/// unconditionally. A port that handled only [`WRAP_ZLIB`] would emit gzip members whose
/// trailing CRC-32 was an Adler-32 -- a stream every conforming decoder rejects, and one no
/// round-trip test that used this port on both sides would ever catch.
const WRAP_GZIP: i32 = 2;

/// The two named wrapper values are exactly `deflate.h`'s encoding.
///
/// `deflate.h` L111 describes the field bitwise -- "bit 0 true for zlib, bit 1 true for gzip"
/// -- so the numbers are not arbitrary labels this port is free to renumber: `deflateInit2_`
/// derives them from the caller's `windowBits` (`deflate.c` L422-L432), `deflateBound`
/// switches on them (`deflate.c` L883), and the trailer writer compares against `2`
/// (`deflate.c` L1268). Pinning them here means a mistyped constant is a build failure rather
/// than a stream whose check value silently uses the wrong algorithm -- a defect the port's
/// own round-trip tests could not detect, because both sides would agree with each other and
/// disagree only with the rest of the world.
///
/// There is deliberately no constant for the raw case. Zero is one of *four* values that leave
/// the check value alone -- `0`, and the negated `-1` and `-2`, and anything else a C caller
/// might put in the field -- so naming only zero would suggest the other three were handled
/// somewhere else. [`read_buf`] reaches all of them through one wildcard arm, and the wrap
/// table in its documentation is where each is accounted for.
const _: () = assert!(
    WRAP_ZLIB == 1 && WRAP_GZIP == 2,
    "deflate_state.wrap is 1 for zlib and 2 for gzip; see deflate.h L111"
);

/// A cursor over the caller's input buffer: `next_in` and `avail_in` as one number.
///
/// # What it models
///
/// `z_stream` splits the unconsumed input across two members (`zlib.h` L91-L92):
///
/// ```text
/// z_const Bytef *next_in;  /* next input byte */
/// uInt     avail_in;       /* number of bytes available at next_in */
/// ```
///
/// The reference implementation keeps them consistent by hand, adding to one wherever it
/// subtracts from the other. Here they are a borrowed slice and one index into it, with the
/// invariant `position <= capacity` maintained by every method, so `remaining` is derived
/// rather than stored and the pair cannot disagree.
///
/// # Consumed input remains addressable
///
/// Advancing does not discard anything: the cursor keeps the whole buffer and only moves its
/// position. That is required, not merely convenient -- `deflate_stored` rebuilds the window
/// history by reading backwards from `next_in` (`deflate.c` L1766), which
/// [`consumed_tail`](Self::consumed_tail) expresses directly.
///
/// # Copying a cursor
///
/// [`Copy`] is derived because the type is a shared borrow and an integer. A copy is an
/// **independent position** over the same bytes, which is the idiomatic way to save one and
/// come back to it; it is not a second handle on the original, so advancing a copy leaves
/// the original where it was.
///
/// # Empty buffers
///
/// A cursor over `&[]` is valid and total: `remaining` is zero, [`is_empty`](Self::is_empty)
/// is true, the slice accessors hand back empty slices or [`None`], and
/// [`advance`](Self::advance) refuses any non-zero count. This is the module's half of the
/// contract described in the module documentation, under which the facade renders a null
/// `next_in` with `avail_in == 0` as an empty slice.
#[derive(Debug, Clone, Copy)]
pub struct InputCursor<'a> {
    /// The caller's whole input buffer. Never re-sliced, so consumed bytes stay reachable.
    buf: &'a [u8],
    /// Bytes consumed so far. Invariant: `position <= buf.len()`.
    position: usize,
}

impl<'a> InputCursor<'a> {
    /// Creates a cursor positioned at the start of `buf`.
    ///
    /// `buf` corresponds to the `avail_in` bytes at `next_in` on entry to a stream function.
    /// An empty slice is accepted and is the correct rendering of `avail_in == 0`, including
    /// when the caller paired it with a null `next_in`.
    #[inline]
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, position: 0 }
    }

    /// Number of bytes not yet consumed: the value of `avail_in`.
    #[inline]
    #[must_use]
    pub const fn remaining(&self) -> usize {
        // `saturating_sub` rather than `-`: the invariant `position <= len` already rules
        // out a wrap, and this way the guarantee does not depend on the invariant holding.
        self.buf.len().saturating_sub(self.position)
    }

    /// Whether all input has been consumed, i.e. `avail_in == 0`.
    ///
    /// True both for an exhausted cursor and for one built over an empty slice; the two are
    /// indistinguishable to a caller, exactly as they are in C.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Number of bytes consumed so far: how far `next_in` has advanced from the buffer's
    /// start.
    ///
    /// This is the value the facade needs on the way out. Rebuilding the caller's pair from
    /// it -- `next_in += consumed()` and `avail_in -= consumed()` -- restores exactly the
    /// state the C implementation would have left behind.
    #[inline]
    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.position
    }

    /// Total length of the buffer, consumed and unconsumed together.
    ///
    /// Named `capacity` rather than `len` on purpose. A `len` would invite the reading
    /// `is_empty() == (len() == 0)`, which is false here: [`is_empty`](Self::is_empty)
    /// reports whether input is *exhausted*, not whether the buffer was empty to begin with.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// The unconsumed input: the [`remaining`](Self::remaining) bytes at `next_in`.
    ///
    /// Empty when the input is exhausted. The borrow is tied to the buffer rather than to
    /// this cursor, so the slice stays usable while the cursor is advanced -- which is how
    /// `read_buf` can copy out of the input and then move the position without a second
    /// bounds check.
    #[inline]
    #[must_use]
    pub fn unconsumed(&self) -> &'a [u8] {
        self.buf.get(self.position..).unwrap_or(&[])
    }

    /// The whole input buffer, ignoring the cursor position.
    ///
    /// Useful when a caller has to reason about the buffer as the C code does, in terms of
    /// an origin and an offset, rather than in terms of what is left.
    #[inline]
    #[must_use]
    pub const fn as_slice(&self) -> &'a [u8] {
        self.buf
    }

    /// The next `count` unconsumed bytes, or [`None`] if fewer than `count` remain.
    ///
    /// Rejecting a short request rather than truncating it is what makes this usable as a
    /// bound check: a [`Some`] result is a proof that `count` bytes were available, so the
    /// caller does not have to re-establish that fact.
    #[inline]
    #[must_use]
    pub fn peek_slice(&self, count: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(count)?;
        self.buf.get(self.position..end)
    }

    /// The last `count` bytes *already consumed*, ending at the cursor, or [`None`] if fewer
    /// than `count` have been consumed.
    ///
    /// This is the safe form of `s->strm->next_in - s->w_size` at `deflate.c` L1766, where
    /// `deflate_stored` supplants the window's history with the final `w_size` bytes it just
    /// copied through. Reading behind the cursor is legitimate -- those bytes are still in
    /// the caller's buffer and the caller must not touch it until the call returns -- but in
    /// C it is unchecked pointer arithmetic that walks off the front of the buffer whenever
    /// fewer than `w_size` bytes have been read. Here that case is [`None`].
    #[inline]
    #[must_use]
    pub fn consumed_tail(&self, count: usize) -> Option<&'a [u8]> {
        let start = self.position.checked_sub(count)?;
        self.buf.get(start..self.position)
    }

    /// Consumes `count` bytes, or nothing at all.
    ///
    /// Returns `true` when the cursor moved. Returns `false`, leaving the cursor exactly
    /// where it was, when `count` exceeds [`remaining`](Self::remaining) -- the all-or-nothing
    /// rule that keeps a rejected advance from leaving a half-updated stream. Advancing by
    /// zero always succeeds and is a no-op.
    #[inline]
    pub fn advance(&mut self, count: usize) -> bool {
        if count > self.remaining() {
            return false;
        }
        self.position = self.position.saturating_add(count);
        true
    }

    /// Consumes at most `count` bytes and reports how many were consumed.
    ///
    /// Private, and infallible by construction: the count is clamped to what remains, so the
    /// invariant `position <= capacity` is preserved without a fallible path that a caller
    /// would then have to unwind. [`read_buf`] uses this for its single state change, having
    /// already derived the count as a minimum that includes [`remaining`](Self::remaining).
    #[inline]
    fn advance_clamped(&mut self, count: usize) -> usize {
        let taken = count.min(self.remaining());
        self.position = self.position.saturating_add(taken);
        taken
    }
}

/// A cursor over the caller's output buffer: `next_out` and `avail_out` as one number.
///
/// # What it models
///
/// `z_stream` splits the free output space across two members (`zlib.h` L94-L95):
///
/// ```text
/// Bytef    *next_out; /* next output byte will go here */
/// uInt     avail_out; /* remaining free space at next_out */
/// ```
///
/// As with [`InputCursor`], the two become a borrowed slice and one index, with the invariant
/// `position <= capacity`. The borrow is exclusive because the buffer is written to, which is
/// also why this type is neither [`Copy`] nor [`Clone`]: two cursors over one output buffer
/// would be two writers, and the type system rules that out rather than documenting against
/// it.
///
/// # Why the compressor needs it
///
/// Two operations in `deflate.c` write through `next_out`, and both are expressible here
/// without pointer arithmetic:
///
/// * `flush_pending` copies from the pending buffer (`deflate.c` L959-L963) --
///   [`push_slice`](Self::push_slice) is that copy, clamped to the space available. The rest
///   of `flush_pending` is deflate state and stays in `deflate/pending.rs`.
/// * `deflate_stored` copies input *straight through* to the output buffer
///   (`deflate.c` L1746) -- [`unwritten_up_to`](Self::unwritten_up_to) hands
///   `read_buf` a destination of exactly the right length, and
///   `read_buf_into_output` performs the whole five-line sequence.
///
/// # Empty buffers
///
/// A cursor over `&mut []` is valid: nothing remains, [`push_slice`](Self::push_slice) copies
/// nothing and reports zero, and the mutable accessors return empty slices. A caller that
/// supplied `avail_out == 0` therefore needs no special case, matching the C code, where a
/// zero `avail_out` simply makes every `len` computation clamp to zero.
#[derive(Debug)]
pub struct OutputCursor<'a> {
    /// The caller's whole output buffer. Never re-sliced, so written bytes stay reachable.
    buf: &'a mut [u8],
    /// Bytes written so far. Invariant: `position <= buf.len()`.
    position: usize,
}

impl<'a> OutputCursor<'a> {
    /// Creates a cursor positioned at the start of `buf`.
    ///
    /// `buf` corresponds to the `avail_out` bytes at `next_out` on entry to a stream
    /// function. An empty slice is accepted and is the correct rendering of
    /// `avail_out == 0`.
    #[inline]
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, position: 0 }
    }

    /// Free space left in the buffer: the value of `avail_out`.
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.position)
    }

    /// Whether the buffer is full, i.e. `avail_out == 0`.
    ///
    /// True both for a filled cursor and for one built over an empty slice.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Number of bytes written so far: how far `next_out` has advanced.
    ///
    /// The facade rebuilds the caller's pair from this on the way out -- `next_out +=
    /// written()` and `avail_out -= written()` -- and adds the same count to `total_out`.
    #[inline]
    #[must_use]
    pub fn written(&self) -> usize {
        self.position
    }

    /// Total length of the buffer, written and unwritten together.
    ///
    /// Named `capacity` for the same reason as [`InputCursor::capacity`]: `is_empty` here
    /// means "no space left", not "the buffer was created empty".
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// The bytes written so far, as a shared slice.
    ///
    /// Lets a caller inspect what it has produced -- which is what the tests in this module
    /// do -- without having to hold on to the original buffer separately.
    #[inline]
    #[must_use]
    pub fn written_slice(&self) -> &[u8] {
        self.buf.get(..self.position).unwrap_or(&[])
    }

    /// The unwritten tail of the buffer, as an exclusive slice: the `avail_out` bytes at
    /// `next_out`.
    ///
    /// Writing into it does **not** advance the cursor; pair it with
    /// [`advance`](Self::advance) once the number of bytes actually written is known. Prefer
    /// [`push_slice`](Self::push_slice) when the bytes are already in a slice, since it
    /// cannot be used without advancing.
    #[inline]
    pub fn unwritten(&mut self) -> &mut [u8] {
        self.buf.get_mut(self.position..).unwrap_or(&mut [])
    }

    /// The next `count` unwritten bytes, or as many as remain if fewer than `count` are
    /// left.
    ///
    /// Clamping rather than refusing is what makes this the right shape for `deflate.c`
    /// L1746, where the surrounding code has already reduced `len` to fit `avail_out` and the
    /// clamp is a second line of defence rather than the primary bound. It also means the
    /// returned slice's own length *is* the effective size argument, so `read_buf` needs no
    /// separate count.
    #[inline]
    pub fn unwritten_up_to(&mut self, count: usize) -> &mut [u8] {
        let take = count.min(self.remaining());
        let end = self.position.saturating_add(take);
        self.buf.get_mut(self.position..end).unwrap_or(&mut [])
    }

    /// Marks `count` bytes as written, or nothing at all.
    ///
    /// Returns `true` when the cursor moved. Returns `false`, leaving it exactly where it
    /// was, when `count` exceeds [`remaining`](Self::remaining). Advancing by zero always
    /// succeeds and is a no-op.
    #[inline]
    pub fn advance(&mut self, count: usize) -> bool {
        if count > self.remaining() {
            return false;
        }
        self.position = self.position.saturating_add(count);
        true
    }

    /// Copies as much of `src` as fits and reports how many bytes were copied.
    ///
    /// This is the safe form of the copy at the heart of `flush_pending`
    /// (`deflate.c` L959-L963), where the C code clamps the length to `avail_out` and then
    /// advances `next_out`, `pending_out`, `total_out`, `avail_out` and `pending` around it:
    ///
    /// ```text
    /// len = s->pending > strm->avail_out ? strm->avail_out : (unsigned)s->pending;
    /// if (len == 0) return;
    /// zmemcpy(strm->next_out, s->pending_out, len);
    /// ```
    ///
    /// A short copy is normal and is the caller's signal that the output buffer filled: the
    /// return value is the `len` the C code computed, and the un-copied remainder of `src`
    /// stays the caller's to re-present on the next call. The cursor is advanced by exactly
    /// the number of bytes copied, so the two can never disagree.
    #[inline]
    pub fn push_slice(&mut self, src: &[u8]) -> usize {
        let len = src.len().min(self.remaining());
        // Mirrors `if (len == 0) return;` -- and, more importantly, keeps the two `get`
        // calls below off the zero-length path entirely.
        if len == 0 {
            return 0;
        }
        let end = self.position.saturating_add(len);
        match (self.buf.get_mut(self.position..end), src.get(..len)) {
            (Some(dest), Some(head)) => {
                dest.copy_from_slice(head);
                self.position = end;
                len
            }
            // Unreachable: `len` is the smaller of the two lengths, so both views exist.
            // Reported as "nothing copied" rather than asserted, so that this method cannot
            // panic for any input, and without mutating anything on the way out.
            _ => 0,
        }
    }

    /// Marks at most `count` bytes as written and reports how many.
    ///
    /// The output-side twin of [`InputCursor::advance_clamped`], and private for the same
    /// reason: [`read_buf_into_output`] has already established the count and needs a state
    /// change that cannot fail.
    #[inline]
    fn advance_clamped(&mut self, count: usize) -> usize {
        let taken = count.min(self.remaining());
        self.position = self.position.saturating_add(taken);
        taken
    }
}

/// Copies up to `dest.len()` bytes of input into `dest`, folds them into the running check
/// value and advances the input accounting. Returns the number of bytes transferred.
///
/// Port of `read_buf` (`deflate.c` L219-L240), the function every byte the compressor
/// consumes passes through.
///
/// # The reference implementation
///
/// ```text
/// local unsigned read_buf(z_streamp strm, Bytef *buf, unsigned size) {
///     unsigned len = strm->avail_in;
///
///     if (len > size) len = size;
///     if (len == 0) return 0;
///
///     strm->avail_in  -= len;
///
///     zmemcpy(buf, strm->next_in, len);
///     if (strm->state->wrap == 1) {
///         strm->adler = adler32(strm->adler, buf, len);
///     }
/// #ifdef GZIP
///     else if (strm->state->wrap == 2) {
///         strm->adler = crc32(strm->adler, buf, len);
///     }
/// #endif
///     strm->next_in  += len;
///     strm->total_in += len;
///
///     return len;
/// }
/// ```
///
/// # Parameters, and how they map onto the C signature
///
/// | Here | There | Notes |
/// |---|---|---|
/// | `input` | `strm->next_in` + `strm->avail_in` | Advanced by the return value |
/// | `dest` | `buf` **and** `size` | A slice carries its length, so `size` is `dest.len()` |
/// | `wrap` | `strm->state->wrap` | Signed, and read on every call -- see below |
/// | `check` | `strm->adler` | The running Adler-32 or CRC-32 |
/// | `total_in` | `strm->total_in` | Incremented by the return value |
///
/// Collapsing `buf` and `size` into one slice removes a whole defect class rather than merely
/// tidying the signature: in C nothing connects the pointer to the count, so a caller that
/// passed a `size` larger than the buffer would overflow it silently. Here the length *is*
/// the buffer's length.
///
/// # Statement order is part of the contract
///
/// The steps below run in the reference implementation's order, because a rearrangement that
/// looks equivalent need not be under partial input:
///
/// 1. **Clamp** -- `len = min(avail_in, size)` (`deflate.c` L220-L222).
/// 2. **Early return** -- `if (len == 0) return 0;` (`deflate.c` L223). Nothing is touched
///    on this path: the check value and both totals are left exactly as they were. That
///    matters because `fill_window` calls this speculatively whenever the lookahead runs
///    short, so the no-input case is the common case near the end of a stream, and it must
///    not perturb the trailer.
/// 3. **Copy** -- `zmemcpy(buf, strm->next_in, len)` (`deflate.c` L227), which `zutil.h`
///    L216 defines as plain `memcpy`, hence `copy_from_slice` over two views of equal length.
/// 4. **Check value** -- computed over the **destination** bytes, after the copy
///    (`deflate.c` L228-L235), and only when `wrap` is exactly [`WRAP_ZLIB`] or exactly
///    [`WRAP_GZIP`].
/// 5. **Advance and account** -- `next_in += len; total_in += len;`
///    (`deflate.c` L236-L237).
/// 6. **Return `len`** (`deflate.c` L239).
///
/// ## Why steps 3 and 4 read the destination, not the source
///
/// The two buffers hold identical bytes once the copy has run, so checksumming the source
/// beforehand would produce the same number and save a dependency on the copy completing.
/// The reference operand is kept anyway. It costs nothing, it keeps this function a
/// transcription that can be diffed against `deflate.c` line by line, and it means the value
/// is computed over the bytes the compressor will actually go on to read -- so if the copy
/// were ever wrong, the check value would disagree with the source rather than silently
/// agreeing with it.
///
/// ## Why one cursor advance stands in for two C statements
///
/// C decrements `avail_in` at L225, *before* the copy, and increments `next_in` at L236,
/// *after* the check value. Those are the two halves of a single invariant --
/// `next_in + avail_in` is the end of the caller's buffer -- and this cursor holds that
/// invariant in one number, so it cannot express the intermediate state in which one half has
/// been updated and the other has not.
///
/// That is sound, and the reason is specific rather than general: **nothing between L225 and
/// L236 reads either member.** The copy reads the local `buf` and `len`; the check value
/// reads `buf`, `len` and `strm->adler`. Neither touches `strm->avail_in` or `strm->next_in`,
/// and neither can observe the split. The single advance is therefore placed at the later of
/// the two points, and the intermediate state it skips is unobservable rather than merely
/// unlikely to be observed.
///
/// # `wrap` is signed, and is consulted on every call
///
/// `deflate.h` L111 declares `int wrap; /* bit 0 true for zlib, bit 1 true for gzip */`, and
/// the field takes five values over a stream's life:
///
/// | `wrap` | Meaning | Effect here |
/// |---|---|---|
/// | `0` | Raw DEFLATE, or a dictionary load in progress (`deflate.c` L577) | No check value |
/// | `1` | zlib container | Adler-32 |
/// | `2` | gzip container | CRC-32 |
/// | `-1`, `-2` | Trailer already written (`deflate.c` L1288) | No check value |
///
/// So the parameter is an `i32` and the test is exact equality, never a bitmask and never a
/// truncation to an unsigned type. Both extremes carry real behaviour:
///
/// * `deflateSetDictionary` sets `wrap = 0` around its window load precisely so that this
///   function skips the update -- the C comment says so outright, "avoid computing Adler-32
///   in `read_buf`" -- because RFC 1950 excludes dictionary bytes from the stream's check
///   value (`doc/rfc1950.txt` L319-L320). The exclusion is implemented *here*.
/// * `deflate` negates `wrap` once it has emitted the trailer, `if (s->wrap > 0) s->wrap =
///   -s->wrap; /* write the trailer only once! */`. A negative value must therefore leave the
///   check value alone; folding the sign away -- by taking a `u32`, or by matching on
///   `wrap.abs()` -- would corrupt an already-finished stream.
///
/// The caller supplies this value; `config::Wrap::as_deflate_wrap` is what produces it for a
/// freshly initialised stream, and `deflate/state.rs` stores it as a signed field for exactly
/// the reason above.
///
/// # Widths
///
/// `check` is a `u32` because both checksums are 32-bit quantities whatever `uLong` happens
/// to be, and `total_in` is a `u64` so that no accounting is lost on any target. The
/// addition wraps, faithfully to C's `total_in += len` on an unsigned type; reconciling
/// either with the caller's `c_ulong` is the facade's job. See the module documentation.
///
/// # Panic freedom
///
/// Both slice views are resolved before anything is mutated, so the by-construction-impossible
/// failure of a bound this function has already computed returns zero with the stream
/// untouched -- indistinguishable, to a caller, from the ordinary "no input available" case
/// at step 2.
pub(crate) fn read_buf(
    input: &mut InputCursor<'_>,
    dest: &mut [u8],
    wrap: i32,
    check: &mut u32,
    total_in: &mut u64,
) -> usize {
    // 1. `len = strm->avail_in; if (len > size) len = size;`  (deflate.c L220-L222)
    let len = input.remaining().min(dest.len());

    // 2. `if (len == 0) return 0;`  (deflate.c L223)
    //
    // Before the check value, before the totals, before the cursor: an empty input buffer or
    // a full destination leaves the stream bit-for-bit as it was found.
    if len == 0 {
        return 0;
    }

    // Resolve both views up front. `len` is the smaller of the two lengths, so each `Some`
    // is guaranteed; taking them now means the fallback arm below has nothing to undo.
    let (Some(src), Some(dest_head)) = (input.peek_slice(len), dest.get_mut(..len)) else {
        // Unreachable, and handled rather than asserted so that this function cannot panic.
        // Zero is the honest answer as well as the safe one: nothing has been copied,
        // nothing counted, and the C function returns zero for every case in which it moves
        // no bytes.
        return 0;
    };

    // 3. `zmemcpy(buf, strm->next_in, len);`  (deflate.c L227)
    //
    // `zmemcpy` is `memcpy` (`zutil.h` L216). The two slices are `len` bytes each by
    // construction, which is the precondition `copy_from_slice` requires.
    dest_head.copy_from_slice(src);

    // 4. The check value, over the freshly written destination bytes.  (deflate.c L228-L235)
    //
    // Exact equality on a signed value: 0 and the negated post-trailer values fall through
    // and update nothing, exactly as the C code's two `if`s do.
    match wrap {
        // `strm->adler = adler32(strm->adler, buf, len);`  (deflate.c L229)
        WRAP_ZLIB => *check = adler32(*check, dest_head),
        // `strm->adler = crc32(strm->adler, buf, len);`  (deflate.c L233, inside `#ifdef
        // GZIP`, which `deflate.h` L21-L24 defines by default)
        WRAP_GZIP => *check = crc32(*check, dest_head),
        // Everything else, which is to say: a raw stream (`wrap == 0`); the window load inside
        // `deflateSetDictionary`, which sets `wrap = 0` for precisely this effect and says so
        // -- `s->wrap = 0; /* avoid computing Adler-32 in read_buf */` (`deflate.c` L577),
        // restored at L620 -- because RFC 1950 excludes dictionary bytes from the stream's
        // check value (`doc/rfc1950.txt` L319-L320); a finished stream, whose `wrap` has been
        // negated (`deflate.c` L1288); and any other value a C caller could leave in the
        // field. One wildcard covers them all, exactly as the C code reaches this case by
        // falling out of both of its `if`s.
        _ => {}
    }

    // 5. `strm->avail_in -= len;` (deflate.c L225) and `strm->next_in += len;`
    //    (deflate.c L236), which this cursor holds as one number -- see the section above on
    //    why merging them is unobservable. Infallible: `len <= input.remaining()`.
    let consumed = input.advance_clamped(len);

    // `strm->total_in += len;`  (deflate.c L237)
    //
    // `consumed` rather than `len` so the count added is provably the count taken. The two
    // are equal, because `len` was derived as a minimum that included `remaining()`.
    //
    // Widening: guarded by the compile-time assertion at the top of this module, so it cannot
    // truncate. Wrapping: `total_in` is an unsigned C type and overflows by wrapping; this
    // reproduces that rather than diverging on an input volume no real stream reaches.
    *total_in = total_in.wrapping_add(consumed as u64);

    // 6. `return len;`  (deflate.c L239)
    consumed
}

/// Copies up to `size` bytes of input straight through to the output buffer, folding them
/// into the check value and advancing both totals. Returns the number of bytes transferred.
///
/// Port of the stored-block passthrough at `deflate.c` L1742-L1750, whose comment reads
/// "Copy uncompressed bytes directly from `next_in` to `next_out`, updating the check
/// value":
///
/// ```text
/// if (len) {
///     read_buf(s->strm, s->strm->next_out, len);
///     s->strm->next_out += len;
///     s->strm->avail_out -= len;
///     s->strm->total_out += len;
/// }
/// ```
///
/// # Why this composition lives here
///
/// It is the one call site at which [`read_buf`]'s destination is the caller's *output*
/// buffer rather than the window, and the four statements around it are pure cursor
/// plumbing -- no block sizing, no flush handling, no decision of any kind. Bundling them
/// makes the one mistake they invite structurally impossible: a caller cannot copy through
/// `next_out` and then forget to advance it, because advancing is not a separate step.
///
/// Everything that *is* a decision -- how `len` was arrived at, the `left` window copy that
/// precedes it (`deflate.c` L1731-L1740), and the `do { } while (last == 0)` loop around the
/// whole thing -- stays in `deflate/algorithm/stored.rs`, where `deflate_stored`
/// (`deflate.c` L1668) is ported.
///
/// # Behaviour
///
/// `size` is clamped to the space actually left in `output`, so the effective count is
/// `min(input.remaining(), size, output.remaining())`. Everything else -- the `len == 0`
/// early return, the statement order, the exact-equality `wrap` test, the wrapping totals --
/// is [`read_buf`]'s, unchanged, because this function delegates to it. When nothing is
/// transferred, neither total moves and the check value is untouched.
pub(crate) fn read_buf_into_output(
    input: &mut InputCursor<'_>,
    output: &mut OutputCursor<'_>,
    size: usize,
    wrap: i32,
    check: &mut u32,
    total_in: &mut u64,
    total_out: &mut u64,
) -> usize {
    // `read_buf(s->strm, s->strm->next_out, len);`  (deflate.c L1746)
    //
    // The destination is the unwritten tail, trimmed to `size`; because a slice carries its
    // own length, that trimming *is* the `size` argument the C call passes separately.
    let copied = read_buf(input, output.unwritten_up_to(size), wrap, check, total_in);

    // `s->strm->next_out += len; s->strm->avail_out -= len;`  (deflate.c L1747-L1748), once
    // more the two halves of one invariant. Infallible: `copied` came back from a destination
    // that was itself clamped to `output.remaining()`.
    let advanced = output.advance_clamped(copied);

    // `s->strm->total_out += len;`  (deflate.c L1749). Widening guarded by the compile-time
    // assertion at the top of this module; wrapping for the same reason as `total_in`.
    *total_out = total_out.wrapping_add(advanced as u64);

    advanced
}

// Assertion macros panic, and every expectation below is a compile-time constant or a value
// this module just produced, so an assertion that fails is a defect in the module and not a
// runtime condition to be handled. `unwrap` on an `Option` accessor is used the same way: the
// bound is established by the fixture, so `None` would itself be the bug being reported.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{read_buf, read_buf_into_output, InputCursor, OutputCursor, WRAP_GZIP, WRAP_ZLIB};

    /// The raw-DEFLATE value of `deflate_state.wrap`: no header, no trailer, no check value.
    ///
    /// Named here rather than beside [`WRAP_ZLIB`](super::WRAP_ZLIB) and
    /// [`WRAP_GZIP`](super::WRAP_GZIP) because the implementation reaches it through a wildcard
    /// arm and so has no use for the name; these tests do, because they assert on it
    /// specifically.
    const WRAP_RAW: i32 = 0;

    /// The payload `test/example.c` compresses, and the fixture every checksum expectation
    /// below is taken over.
    ///
    /// Chosen because the existing C suite already uses it, so the numbers here are the same
    /// numbers that suite exercises, and because its repeated `hello` makes it a payload the
    /// compressor can actually find a match in.
    const HELLO: &[u8] = b"hello, hello!";

    /// Length of [`HELLO`], stated once so the split fixtures cannot drift from it.
    const HELLO_LEN: usize = 13;

    /// The preset dictionary `test/example.c` pairs with [`HELLO`]: also its first five bytes.
    ///
    /// That coincidence is what makes the split-accumulation expectations below double as a
    /// check that a partial read leaves the same intermediate check value the C
    /// implementation leaves.
    const DICT_LEN: usize = 5;

    /// Adler-32 initial value: `adler32(0L, Z_NULL, 0)` (`zlib.h` L1821).
    const ADLER_INIT: u32 = 1;

    /// CRC-32 initial value: `crc32(0L, Z_NULL, 0)`.
    const CRC_INIT: u32 = 0;

    // Every constant below was produced by linking the prebuilt reference C library and
    // printing the value, never by hand computation. Each is the oracle's answer for the
    // named input, so a disagreement here is a disagreement with the C implementation itself.

    /// `adler32(1, "hello, hello!", 13)`.
    const ADLER_HELLO: u32 = 0x2170_0496;

    /// `crc32(0, "hello, hello!", 13)`.
    const CRC_HELLO: u32 = 0xb39a_dc9b;

    /// `adler32(1, "hello", 5)` -- the first five bytes of [`HELLO`].
    const ADLER_HELLO_5: u32 = 0x062c_0215;

    /// `crc32(0, "hello", 5)` -- the first five bytes of [`HELLO`].
    const CRC_HELLO_5: u32 = 0x3610_a686;

    /// `adler32(1, "hell", 4)` -- what a destination of four bytes must clamp to.
    const ADLER_HELLO_4: u32 = 0x0417_01a6;

    /// `crc32(0, "hell", 4)` -- what a destination of four bytes must clamp to.
    const CRC_HELLO_4: u32 = 0x1c86_00e3;

    /// An arbitrary non-initial check value, used to prove the untouched paths are untouched.
    ///
    /// Deliberately not 0 or 1, so that "left alone" cannot be confused with "reset to an
    /// initial value", and verified against the oracle as a fixed point of both checksums over
    /// an empty buffer: `adler32(0xdeadbeef, buf, 0)` and `crc32(0xdeadbeef, buf, 0)` both
    /// return `0xdeadbeef`.
    const SENTINEL_CHECK: u32 = 0xdead_beef;

    /// An arbitrary non-zero starting total, used the same way as [`SENTINEL_CHECK`].
    const SENTINEL_TOTAL: u64 = 0x0102_0304_0506_0708;

    /// The fixture length really is [`HELLO_LEN`], and the dictionary prefix really is shorter.
    ///
    /// Checked at compile time so the relationship survives an edit of the fixture, rather
    /// than in a test body where it would be an assertion over constants the compiler folds
    /// away.
    const _: () = assert!(
        HELLO.len() == HELLO_LEN && DICT_LEN < HELLO_LEN,
        "the fixtures must agree: HELLO is 13 bytes and the dictionary prefix is shorter"
    );

    /// Drives [`read_buf`] once over the whole of [`HELLO`] with the given wrapper, into a
    /// destination of `dest_len` bytes, and reports everything an observer could see.
    ///
    /// Returns `(transferred, check, total_in, destination bytes actually written)`. Bundling
    /// the observations keeps each test below a statement of one expectation rather than six
    /// lines of setup.
    fn drive(
        wrap: i32,
        dest_len: usize,
        check_in: u32,
        total_in_in: u64,
    ) -> (usize, u32, u64, [u8; 32]) {
        let mut input = InputCursor::new(HELLO);
        let mut storage = [0_u8; 32];
        let mut check = check_in;
        let mut total_in = total_in_in;
        let transferred = read_buf(
            &mut input,
            &mut storage[..dest_len],
            wrap,
            &mut check,
            &mut total_in,
        );
        assert_eq!(
            input.consumed(),
            transferred,
            "the cursor must advance by exactly the number of bytes reported"
        );
        (transferred, check, total_in, storage)
    }

    // ---------------------------------------------------------------------------------------
    // Step 2 of the port: `if (len == 0) return 0;` (deflate.c L223), before anything else.
    // ---------------------------------------------------------------------------------------

    /// Empty input transfers nothing and leaves the check value and the total untouched.
    ///
    /// The C function returns at L223, before `avail_in`, `adler`, `next_in` or `total_in` is
    /// written. Anything else here would corrupt the trailer of every stream, because
    /// `fill_window` calls this speculatively whenever the lookahead runs short and so hits
    /// the no-input case repeatedly near the end of a stream.
    #[test]
    fn empty_input_is_a_total_no_op() {
        for wrap in [WRAP_RAW, WRAP_ZLIB, WRAP_GZIP, -1, -2] {
            let mut input = InputCursor::new(&[]);
            let mut dest = [0_u8; 8];
            let mut check = SENTINEL_CHECK;
            let mut total_in = SENTINEL_TOTAL;

            let n = read_buf(&mut input, &mut dest, wrap, &mut check, &mut total_in);

            assert_eq!(
                n, 0,
                "wrap {wrap}: nothing can be transferred from empty input"
            );
            assert_eq!(
                check, SENTINEL_CHECK,
                "wrap {wrap}: the check value must be untouched"
            );
            assert_eq!(
                total_in, SENTINEL_TOTAL,
                "wrap {wrap}: the total must be untouched"
            );
            assert_eq!(
                dest, [0_u8; 8],
                "wrap {wrap}: the destination must be untouched"
            );
            assert_eq!(input.consumed(), 0, "wrap {wrap}: the cursor must not move");
        }
    }

    /// A zero-length destination transfers nothing, even with input available.
    ///
    /// This is the `size == 0` half of the same clamp: `len = min(avail_in, size)` is zero
    /// whichever operand is zero, so the early return is reached and nothing is observed to
    /// change.
    #[test]
    fn empty_destination_is_a_total_no_op() {
        for wrap in [WRAP_RAW, WRAP_ZLIB, WRAP_GZIP] {
            let mut input = InputCursor::new(HELLO);
            let mut check = SENTINEL_CHECK;
            let mut total_in = SENTINEL_TOTAL;

            let n = read_buf(&mut input, &mut [], wrap, &mut check, &mut total_in);

            assert_eq!(n, 0, "wrap {wrap}: a zero-length destination takes nothing");
            assert_eq!(
                check, SENTINEL_CHECK,
                "wrap {wrap}: the check value must be untouched"
            );
            assert_eq!(
                total_in, SENTINEL_TOTAL,
                "wrap {wrap}: the total must be untouched"
            );
            assert_eq!(
                input.remaining(),
                HELLO_LEN,
                "wrap {wrap}: the input must be left entirely unconsumed"
            );
        }
    }

    /// An exhausted cursor behaves exactly like one built over an empty slice.
    ///
    /// Covers the "cursor advanced to exactly its end" case: `remaining()` is zero by
    /// subtraction rather than by the buffer being empty, and the two must be
    /// indistinguishable.
    #[test]
    fn exhausted_cursor_is_a_total_no_op() {
        let mut input = InputCursor::new(HELLO);
        assert!(
            input.advance(HELLO_LEN),
            "the whole fixture must be consumable"
        );
        assert_eq!(input.remaining(), 0);
        assert!(input.is_empty());
        assert_eq!(input.consumed(), HELLO_LEN);

        let mut dest = [0_u8; 8];
        let mut check = SENTINEL_CHECK;
        let mut total_in = SENTINEL_TOTAL;
        let n = read_buf(&mut input, &mut dest, WRAP_ZLIB, &mut check, &mut total_in);

        assert_eq!(n, 0);
        assert_eq!(check, SENTINEL_CHECK);
        assert_eq!(total_in, SENTINEL_TOTAL);
    }

    // ---------------------------------------------------------------------------------------
    // Step 1 of the port: `len = min(avail_in, size)` (deflate.c L220-L222).
    // ---------------------------------------------------------------------------------------

    /// A destination larger than the input transfers exactly the input length.
    #[test]
    fn destination_larger_than_input_transfers_all_of_it() {
        let (n, check, total, dest) = drive(WRAP_ZLIB, 32, ADLER_INIT, 0);

        assert_eq!(n, HELLO_LEN, "all available input must be taken");
        assert_eq!(&dest[..HELLO_LEN], HELLO, "the bytes must arrive unaltered");
        assert_eq!(
            &dest[HELLO_LEN..],
            &[0_u8; 32 - HELLO_LEN],
            "nothing may be written past the transferred bytes"
        );
        assert_eq!(check, ADLER_HELLO);
        assert_eq!(total, HELLO_LEN as u64);
    }

    /// A destination smaller than the input transfers exactly the destination length.
    ///
    /// The clamp is what stops `read_buf` overrunning the window's free space, so the check
    /// value must cover the clamped prefix and nothing more -- verified against the oracle's
    /// four-byte answers rather than against a value this module computed for itself.
    #[test]
    fn destination_smaller_than_input_clamps_to_it() {
        let (n, check, total, dest) = drive(WRAP_ZLIB, 4, ADLER_INIT, 0);
        assert_eq!(n, 4, "the destination length is the bound");
        assert_eq!(&dest[..4], b"hell");
        assert_eq!(
            check, ADLER_HELLO_4,
            "the check value must cover only the four bytes copied"
        );
        assert_eq!(total, 4);

        let (n, check, total, dest) = drive(WRAP_GZIP, 4, CRC_INIT, 0);
        assert_eq!(n, 4);
        assert_eq!(&dest[..4], b"hell");
        assert_eq!(check, CRC_HELLO_4);
        assert_eq!(total, 4);
    }

    /// A destination of exactly the input length is neither clamp: both operands agree.
    #[test]
    fn destination_equal_to_input_transfers_all_of_it() {
        let (n, check, total, dest) = drive(WRAP_GZIP, HELLO_LEN, CRC_INIT, 0);
        assert_eq!(n, HELLO_LEN);
        assert_eq!(&dest[..HELLO_LEN], HELLO);
        assert_eq!(check, CRC_HELLO);
        assert_eq!(total, HELLO_LEN as u64);
    }

    // ---------------------------------------------------------------------------------------
    // Step 4 of the port: the check value (deflate.c L228-L235).
    // ---------------------------------------------------------------------------------------

    /// `wrap == 1` folds the copied bytes into an Adler-32 (`deflate.c` L229).
    #[test]
    fn wrap_zlib_accumulates_adler32() {
        let (n, check, _, _) = drive(WRAP_ZLIB, 32, ADLER_INIT, 0);
        assert_eq!(n, HELLO_LEN);
        assert_eq!(
            check, ADLER_HELLO,
            "wrap == 1 must produce the oracle's Adler-32"
        );
        assert_ne!(
            check, CRC_HELLO,
            "wrap == 1 must not have produced a CRC-32"
        );
    }

    /// `wrap == 2` folds the copied bytes into a CRC-32 (`deflate.c` L233).
    ///
    /// This arm sits behind `#ifdef GZIP`, which `deflate.h` L21-L24 defines by default, so it
    /// is part of the shipped behaviour. A port that omitted it would emit gzip members whose
    /// trailer held an Adler-32 -- rejected by every conforming decoder, and invisible to any
    /// round-trip test that used this port on both sides.
    #[test]
    fn wrap_gzip_accumulates_crc32() {
        let (n, check, _, _) = drive(WRAP_GZIP, 32, CRC_INIT, 0);
        assert_eq!(n, HELLO_LEN);
        assert_eq!(
            check, CRC_HELLO,
            "wrap == 2 must produce the oracle's CRC-32"
        );
        assert_ne!(
            check, ADLER_HELLO,
            "wrap == 2 must not have produced an Adler-32"
        );
    }

    /// `wrap == 0` copies the bytes and counts them but leaves the check value alone.
    ///
    /// Two distinct situations reach this: a raw DEFLATE stream, and the window load inside
    /// `deflateSetDictionary`, which sets `wrap = 0` for exactly this purpose -- the C comment
    /// reads "avoid computing Adler-32 in `read_buf`" (`deflate.c` L577) -- because RFC 1950
    /// excludes dictionary bytes from the stream's check value.
    #[test]
    fn wrap_raw_leaves_the_check_value_alone() {
        let (n, check, total, dest) = drive(WRAP_RAW, 32, SENTINEL_CHECK, 0);
        assert_eq!(n, HELLO_LEN, "the bytes are still copied");
        assert_eq!(&dest[..HELLO_LEN], HELLO, "and they still arrive unaltered");
        assert_eq!(total, HELLO_LEN as u64, "and they are still counted");
        assert_eq!(check, SENTINEL_CHECK, "but the check value is not touched");
    }

    /// A negative `wrap` leaves the check value alone.
    ///
    /// `deflate` negates the field once the trailer has been emitted --
    /// `if (s->wrap > 0) s->wrap = -s->wrap; /* write the trailer only once! */`
    /// (`deflate.c` L1288) -- so the sign is load-bearing. Folding it away, by taking an
    /// unsigned parameter or by matching on an absolute value, would keep mutating the check
    /// value of a stream that has already been finished.
    #[test]
    fn negative_wrap_leaves_the_check_value_alone() {
        for wrap in [-WRAP_ZLIB, -WRAP_GZIP] {
            let (n, check, total, _) = drive(wrap, 32, SENTINEL_CHECK, 0);
            assert_eq!(
                n, HELLO_LEN,
                "wrap {wrap}: the bytes are still copied and counted"
            );
            assert_eq!(
                total, HELLO_LEN as u64,
                "wrap {wrap}: the total still advances"
            );
            assert_eq!(
                check, SENTINEL_CHECK,
                "wrap {wrap}: a finished stream's check value must not move"
            );
        }
    }

    /// An unrecognised `wrap` is treated as raw rather than mishandled.
    ///
    /// Not a state the compressor reaches, but the parameter is a plain `int` at the C ABI
    /// edge, so the wildcard arm has to be total.
    #[test]
    fn unknown_wrap_is_treated_as_raw() {
        for wrap in [3, 7, i32::MAX, i32::MIN] {
            let (n, check, total, _) = drive(wrap, 32, SENTINEL_CHECK, 0);
            assert_eq!(n, HELLO_LEN, "wrap {wrap}: the copy still happens");
            assert_eq!(
                total, HELLO_LEN as u64,
                "wrap {wrap}: the total still advances"
            );
            assert_eq!(
                check, SENTINEL_CHECK,
                "wrap {wrap}: no check value is computed"
            );
        }
    }

    // ---------------------------------------------------------------------------------------
    // Accumulation across calls: the property `deflate` depends on, since a caller may
    // present its input in however many pieces it likes.
    // ---------------------------------------------------------------------------------------

    /// Two partial reads leave the same check value and total as one combined read.
    ///
    /// This is the property that makes a check value accumulated over an arbitrary chunking of
    /// the input equal to the one accumulated over the whole of it -- without which the trailer
    /// would depend on the caller's buffer sizes. The intermediate value after five bytes is
    /// pinned as well, against the oracle, so a failure localises to the first call or the
    /// second.
    #[test]
    fn two_partial_reads_equal_one_combined_read() {
        for (wrap, init, mid, end) in [
            (WRAP_ZLIB, ADLER_INIT, ADLER_HELLO_5, ADLER_HELLO),
            (WRAP_GZIP, CRC_INIT, CRC_HELLO_5, CRC_HELLO),
        ] {
            let mut input = InputCursor::new(HELLO);
            let mut dest = [0_u8; 32];
            let mut check = init;
            let mut total_in = 0_u64;

            let first = read_buf(
                &mut input,
                &mut dest[..DICT_LEN],
                wrap,
                &mut check,
                &mut total_in,
            );
            assert_eq!(
                first, DICT_LEN,
                "wrap {wrap}: the first read is clamped to five bytes"
            );
            assert_eq!(
                check, mid,
                "wrap {wrap}: the intermediate check value must match the oracle"
            );
            assert_eq!(total_in, DICT_LEN as u64);
            assert_eq!(&dest[..DICT_LEN], b"hello");

            let second = read_buf(
                &mut input,
                &mut dest[DICT_LEN..],
                wrap,
                &mut check,
                &mut total_in,
            );
            assert_eq!(
                second,
                HELLO_LEN - DICT_LEN,
                "wrap {wrap}: the rest follows"
            );
            assert_eq!(
                check, end,
                "wrap {wrap}: the accumulated value must equal the one-shot value"
            );
            assert_eq!(total_in, HELLO_LEN as u64);
            assert_eq!(
                &dest[..HELLO_LEN],
                HELLO,
                "wrap {wrap}: the pieces must reassemble"
            );
            assert!(input.is_empty(), "wrap {wrap}: the input must be exhausted");
        }
    }

    /// Feeding the fixture one byte at a time reaches the same place as one whole read.
    ///
    /// The extreme chunking case, which also exercises the `len == 1` paths in both checksum
    /// backends and, on the final iteration, the transition to the exhausted cursor.
    #[test]
    fn byte_at_a_time_reaches_the_same_result() {
        for (wrap, init, expected) in [
            (WRAP_ZLIB, ADLER_INIT, ADLER_HELLO),
            (WRAP_GZIP, CRC_INIT, CRC_HELLO),
        ] {
            let mut input = InputCursor::new(HELLO);
            let mut check = init;
            let mut total_in = 0_u64;
            let mut seen = [0_u8; HELLO_LEN];

            for (index, slot) in seen.iter_mut().enumerate() {
                let mut one = [0_u8; 1];
                let n = read_buf(&mut input, &mut one, wrap, &mut check, &mut total_in);
                assert_eq!(n, 1, "wrap {wrap}: byte {index} must transfer");
                *slot = one[0];
            }

            assert_eq!(
                &seen[..],
                HELLO,
                "wrap {wrap}: the bytes must arrive in order"
            );
            assert_eq!(
                check, expected,
                "wrap {wrap}: thirteen one-byte reads must agree with one"
            );
            assert_eq!(total_in, HELLO_LEN as u64);

            // The fourteenth read finds nothing and must change nothing.
            let mut one = [0_u8; 1];
            let n = read_buf(&mut input, &mut one, wrap, &mut check, &mut total_in);
            assert_eq!(n, 0, "wrap {wrap}: the input is exhausted");
            assert_eq!(
                check, expected,
                "wrap {wrap}: and the check value stays put"
            );
            assert_eq!(total_in, HELLO_LEN as u64);
        }
    }

    /// The total accumulates onto whatever it already held, rather than being assigned.
    ///
    /// `total_in += len` (`deflate.c` L237) is an increment, and a stream's total spans every
    /// call made against it.
    #[test]
    fn the_total_accumulates_rather_than_resetting() {
        let (n, _, total, _) = drive(WRAP_ZLIB, 32, ADLER_INIT, SENTINEL_TOTAL);
        assert_eq!(n, HELLO_LEN);
        assert_eq!(total, SENTINEL_TOTAL + HELLO_LEN as u64);
    }

    /// A total at the very top of the range wraps, as the C unsigned type does.
    ///
    /// `strm->total_in` is a `uLong`, and unsigned overflow in C wraps rather than trapping.
    /// Reproducing that keeps the port from panicking, or from diverging, on a stream no real
    /// caller reaches.
    #[test]
    fn the_total_wraps_like_the_c_unsigned_type() {
        let start = u64::MAX - 2;
        let (n, _, total, _) = drive(WRAP_RAW, 32, SENTINEL_CHECK, start);
        assert_eq!(n, HELLO_LEN);
        assert_eq!(total, start.wrapping_add(HELLO_LEN as u64));
    }

    // ---------------------------------------------------------------------------------------
    // InputCursor: the `next_in` / `avail_in` pair.
    // ---------------------------------------------------------------------------------------

    /// A fresh cursor reports the whole buffer as unconsumed.
    #[test]
    fn a_fresh_input_cursor_has_consumed_nothing() {
        let cursor = InputCursor::new(HELLO);
        assert_eq!(cursor.remaining(), HELLO_LEN);
        assert_eq!(cursor.capacity(), HELLO_LEN);
        assert_eq!(cursor.consumed(), 0);
        assert!(!cursor.is_empty());
        assert_eq!(cursor.unconsumed(), HELLO);
        assert_eq!(cursor.as_slice(), HELLO);
    }

    /// A cursor over an empty slice is valid, total and never indexes.
    ///
    /// This is the module's half of the contract with the facade: a null `next_in` paired with
    /// `avail_in == 0` arrives here as `&[]`, and every accessor has to answer it.
    #[test]
    fn a_cursor_over_an_empty_slice_answers_every_accessor() {
        let mut cursor = InputCursor::new(&[]);
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(cursor.capacity(), 0);
        assert_eq!(cursor.consumed(), 0);
        assert!(cursor.is_empty());
        assert_eq!(cursor.unconsumed(), &[] as &[u8]);
        assert_eq!(cursor.as_slice(), &[] as &[u8]);
        assert_eq!(cursor.peek_slice(0), Some(&[] as &[u8]));
        assert_eq!(cursor.peek_slice(1), None);
        assert_eq!(cursor.consumed_tail(0), Some(&[] as &[u8]));
        assert_eq!(cursor.consumed_tail(1), None);
        assert!(cursor.advance(0), "advancing by zero always succeeds");
        assert!(!cursor.advance(1), "there is nothing to advance over");
        assert_eq!(cursor.consumed(), 0, "the rejected advance moved nothing");
    }

    /// Advancing moves the boundary between consumed and unconsumed, and keeps both reachable.
    #[test]
    fn advancing_partitions_the_buffer() {
        let mut cursor = InputCursor::new(HELLO);
        assert!(cursor.advance(DICT_LEN));

        assert_eq!(cursor.consumed(), DICT_LEN);
        assert_eq!(cursor.remaining(), HELLO_LEN - DICT_LEN);
        assert_eq!(cursor.capacity(), HELLO_LEN, "the capacity never changes");
        assert_eq!(cursor.unconsumed(), b", hello!");
        assert_eq!(
            cursor.as_slice(),
            HELLO,
            "the whole buffer stays addressable"
        );
        assert_eq!(cursor.consumed_tail(DICT_LEN), Some(&b"hello"[..]));
    }

    /// An advance beyond the end is refused outright and leaves the cursor untouched.
    ///
    /// All-or-nothing matters: a partial advance would leave the caller believing more had been
    /// consumed than the check value covered.
    #[test]
    fn an_over_long_advance_is_refused_atomically() {
        let mut cursor = InputCursor::new(HELLO);
        assert!(cursor.advance(DICT_LEN));

        assert!(
            !cursor.advance(HELLO_LEN),
            "more than remains must be refused"
        );
        assert_eq!(cursor.consumed(), DICT_LEN, "and must change nothing");

        assert!(
            !cursor.advance(usize::MAX),
            "usize::MAX must be refused, not wrapped"
        );
        assert_eq!(cursor.consumed(), DICT_LEN);

        assert!(
            cursor.advance(HELLO_LEN - DICT_LEN),
            "advancing to exactly the end must succeed"
        );
        assert_eq!(cursor.consumed(), HELLO_LEN);
        assert!(cursor.is_empty());
    }

    /// `peek_slice` refuses a short request rather than truncating it.
    ///
    /// That is what makes a `Some` result usable as a proof of availability, which is how
    /// [`read_buf`] avoids re-checking a bound it has already computed.
    #[test]
    fn peek_slice_is_all_or_nothing() {
        let cursor = InputCursor::new(HELLO);
        assert_eq!(cursor.peek_slice(0), Some(&[] as &[u8]));
        assert_eq!(cursor.peek_slice(DICT_LEN), Some(&b"hello"[..]));
        assert_eq!(cursor.peek_slice(HELLO_LEN), Some(HELLO));
        assert_eq!(cursor.peek_slice(HELLO_LEN + 1), None);
        assert_eq!(
            cursor.peek_slice(usize::MAX),
            None,
            "must not overflow the end index"
        );
    }

    /// `consumed_tail` reads behind the cursor, and refuses to read past the buffer's start.
    ///
    /// The safe form of `s->strm->next_in - s->w_size` (`deflate.c` L1766). In C that
    /// subtraction walks off the front of the buffer whenever fewer bytes have been read than
    /// are being asked for; here it is `None`.
    #[test]
    fn consumed_tail_reads_behind_the_cursor_without_underflowing() {
        let mut cursor = InputCursor::new(HELLO);
        assert_eq!(
            cursor.consumed_tail(1),
            None,
            "nothing has been consumed yet"
        );

        assert!(cursor.advance(DICT_LEN));
        assert_eq!(cursor.consumed_tail(0), Some(&[] as &[u8]));
        assert_eq!(cursor.consumed_tail(2), Some(&b"lo"[..]));
        assert_eq!(cursor.consumed_tail(DICT_LEN), Some(&b"hello"[..]));
        assert_eq!(
            cursor.consumed_tail(DICT_LEN + 1),
            None,
            "only five bytes are behind us"
        );
        assert_eq!(
            cursor.consumed_tail(usize::MAX),
            None,
            "must not underflow the start index"
        );

        assert!(cursor.advance(HELLO_LEN - DICT_LEN));
        assert_eq!(cursor.consumed_tail(HELLO_LEN), Some(HELLO));
    }

    /// A copied cursor is an independent position over the same bytes.
    #[test]
    fn a_copied_cursor_advances_independently() {
        let original = InputCursor::new(HELLO);
        let mut copy = original;
        assert!(copy.advance(DICT_LEN));

        assert_eq!(copy.consumed(), DICT_LEN);
        assert_eq!(original.consumed(), 0, "the original must not have moved");
        assert_eq!(original.unconsumed(), HELLO);
    }

    // ---------------------------------------------------------------------------------------
    // OutputCursor: the `next_out` / `avail_out` pair.
    // ---------------------------------------------------------------------------------------

    /// A fresh cursor reports the whole buffer as free.
    #[test]
    fn a_fresh_output_cursor_has_written_nothing() {
        let mut storage = [0_u8; 8];
        let mut cursor = OutputCursor::new(&mut storage);
        assert_eq!(cursor.remaining(), 8);
        assert_eq!(cursor.capacity(), 8);
        assert_eq!(cursor.written(), 0);
        assert!(!cursor.is_empty());
        assert_eq!(cursor.written_slice(), &[] as &[u8]);
        assert_eq!(cursor.unwritten().len(), 8);
    }

    /// A cursor over an empty slice is valid and takes nothing.
    #[test]
    fn an_output_cursor_over_an_empty_slice_takes_nothing() {
        let mut cursor = OutputCursor::new(&mut []);
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(cursor.capacity(), 0);
        assert!(cursor.is_empty());
        assert_eq!(cursor.written(), 0);
        assert_eq!(cursor.written_slice(), &[] as &[u8]);
        assert_eq!(cursor.unwritten(), &mut [] as &mut [u8]);
        assert_eq!(cursor.unwritten_up_to(8), &mut [] as &mut [u8]);
        assert_eq!(cursor.push_slice(HELLO), 0, "there is nowhere to put it");
        assert!(cursor.advance(0));
        assert!(!cursor.advance(1));
    }

    /// `push_slice` copies what fits, advances by exactly that much, and reports it.
    ///
    /// The safe form of the copy at the heart of `flush_pending` (`deflate.c` L959-L963).
    #[test]
    fn push_slice_copies_what_fits_and_advances_by_it() {
        let mut storage = [0_u8; 8];
        let mut cursor = OutputCursor::new(&mut storage);

        assert_eq!(cursor.push_slice(b"hello"), DICT_LEN);
        assert_eq!(cursor.written(), DICT_LEN);
        assert_eq!(cursor.remaining(), 3);
        assert_eq!(cursor.written_slice(), b"hello");

        // A short copy is normal: it is how the caller learns the output buffer filled.
        assert_eq!(
            cursor.push_slice(b", hello!"),
            3,
            "only three bytes of room are left"
        );
        assert_eq!(cursor.written(), 8);
        assert_eq!(cursor.remaining(), 0);
        assert!(cursor.is_empty());
        assert_eq!(cursor.written_slice(), b"hello, h");

        assert_eq!(
            cursor.push_slice(b"more"),
            0,
            "a full buffer takes nothing further"
        );
        assert_eq!(cursor.written(), 8);
        assert_eq!(&storage, b"hello, h");
    }

    /// Pushing an empty slice is a no-op rather than a special case.
    #[test]
    fn pushing_an_empty_slice_changes_nothing() {
        let mut storage = [0_u8; 4];
        let mut cursor = OutputCursor::new(&mut storage);
        assert_eq!(cursor.push_slice(&[]), 0);
        assert_eq!(cursor.written(), 0);
        assert_eq!(storage, [0_u8; 4]);
    }

    /// `unwritten_up_to` clamps rather than refusing, and never overruns the tail.
    ///
    /// Clamping is the right shape for `deflate.c` L1746, where the caller has already reduced
    /// its length to fit `avail_out` and this is a second line of defence.
    #[test]
    fn unwritten_up_to_clamps_to_the_space_left() {
        let mut storage = [0_u8; 8];
        let mut cursor = OutputCursor::new(&mut storage);

        assert_eq!(cursor.unwritten_up_to(0).len(), 0);
        assert_eq!(cursor.unwritten_up_to(3).len(), 3);
        assert_eq!(cursor.unwritten_up_to(8).len(), 8);
        assert_eq!(cursor.unwritten_up_to(99).len(), 8, "clamped, not refused");
        assert_eq!(
            cursor.unwritten_up_to(usize::MAX).len(),
            8,
            "and never overflows"
        );

        assert!(cursor.advance(6));
        assert_eq!(cursor.unwritten_up_to(usize::MAX).len(), 2);
        assert_eq!(cursor.unwritten().len(), 2);
    }

    /// Writing through `unwritten` and then advancing is equivalent to `push_slice`.
    #[test]
    fn writing_through_unwritten_then_advancing_matches_push_slice() {
        let mut storage = [0_u8; 8];
        let mut cursor = OutputCursor::new(&mut storage);

        let tail = cursor.unwritten_up_to(DICT_LEN);
        assert_eq!(tail.len(), DICT_LEN);
        tail.copy_from_slice(b"hello");
        assert_eq!(cursor.written(), 0, "writing alone does not advance");
        assert!(cursor.advance(DICT_LEN));
        assert_eq!(cursor.written(), DICT_LEN);
        assert_eq!(cursor.written_slice(), b"hello");
    }

    /// An over-long advance is refused outright, as on the input side.
    #[test]
    fn an_over_long_output_advance_is_refused_atomically() {
        let mut storage = [0_u8; 8];
        let mut cursor = OutputCursor::new(&mut storage);
        assert!(cursor.advance(6));
        assert!(!cursor.advance(3), "more than remains must be refused");
        assert_eq!(cursor.written(), 6);
        assert!(
            !cursor.advance(usize::MAX),
            "usize::MAX must be refused, not wrapped"
        );
        assert_eq!(cursor.written(), 6);
        assert!(
            cursor.advance(2),
            "advancing to exactly the end must succeed"
        );
        assert!(cursor.is_empty());
    }

    // ---------------------------------------------------------------------------------------
    // read_buf_into_output: the stored-block passthrough (deflate.c L1742-L1750).
    // ---------------------------------------------------------------------------------------

    /// Input flows straight through to the output buffer, updating the check value and both
    /// totals.
    #[test]
    fn passthrough_copies_input_to_output_and_updates_both_totals() {
        let mut input = InputCursor::new(HELLO);
        let mut storage = [0_u8; 32];
        let mut output = OutputCursor::new(&mut storage);
        let mut check = ADLER_INIT;
        let mut total_in = 0_u64;
        let mut total_out = 0_u64;

        let n = read_buf_into_output(
            &mut input,
            &mut output,
            HELLO_LEN,
            WRAP_ZLIB,
            &mut check,
            &mut total_in,
            &mut total_out,
        );

        assert_eq!(n, HELLO_LEN);
        assert_eq!(
            output.written(),
            HELLO_LEN,
            "the output cursor advances itself"
        );
        assert_eq!(output.written_slice(), HELLO);
        assert_eq!(input.consumed(), HELLO_LEN);
        assert_eq!(
            check, ADLER_HELLO,
            "the check value matches the direct read"
        );
        assert_eq!(total_in, HELLO_LEN as u64);
        assert_eq!(total_out, HELLO_LEN as u64, "output bytes are counted too");
    }

    /// The effective count is the smallest of the three bounds: input, `size` and output room.
    #[test]
    fn passthrough_clamps_to_the_tightest_of_the_three_bounds() {
        // `size` is the binding constraint.
        let mut input = InputCursor::new(HELLO);
        let mut storage = [0_u8; 32];
        let mut output = OutputCursor::new(&mut storage);
        let mut check = CRC_INIT;
        let (mut total_in, mut total_out) = (0_u64, 0_u64);
        let n = read_buf_into_output(
            &mut input,
            &mut output,
            4,
            WRAP_GZIP,
            &mut check,
            &mut total_in,
            &mut total_out,
        );
        assert_eq!(n, 4);
        assert_eq!(output.written_slice(), b"hell");
        assert_eq!(
            check, CRC_HELLO_4,
            "only the four transferred bytes are covered"
        );
        assert_eq!((total_in, total_out), (4, 4));

        // The output buffer is the binding constraint.
        let mut input = InputCursor::new(HELLO);
        let mut storage = [0_u8; 4];
        let mut output = OutputCursor::new(&mut storage);
        let mut check = CRC_INIT;
        let (mut total_in, mut total_out) = (0_u64, 0_u64);
        let n = read_buf_into_output(
            &mut input,
            &mut output,
            HELLO_LEN,
            WRAP_GZIP,
            &mut check,
            &mut total_in,
            &mut total_out,
        );
        assert_eq!(n, 4, "avail_out bounds the transfer");
        assert_eq!(check, CRC_HELLO_4);
        assert_eq!(
            input.remaining(),
            HELLO_LEN - 4,
            "the rest of the input is left for later"
        );
        assert_eq!((total_in, total_out), (4, 4));
    }

    /// With no room, no input or a zero size, the passthrough changes nothing at all.
    #[test]
    fn passthrough_is_a_total_no_op_when_any_bound_is_zero() {
        for (input_bytes, room, size) in [
            (HELLO, 0_usize, HELLO_LEN),
            (&[] as &[u8], 32, HELLO_LEN),
            (HELLO, 32, 0),
        ] {
            let mut input = InputCursor::new(input_bytes);
            let mut storage = [0_u8; 32];
            let mut output = OutputCursor::new(&mut storage[..room]);
            let mut check = SENTINEL_CHECK;
            let mut total_in = SENTINEL_TOTAL;
            let mut total_out = SENTINEL_TOTAL;

            let n = read_buf_into_output(
                &mut input,
                &mut output,
                size,
                WRAP_ZLIB,
                &mut check,
                &mut total_in,
                &mut total_out,
            );

            assert_eq!(n, 0, "room {room}, size {size}: nothing can move");
            assert_eq!(
                output.written(),
                0,
                "room {room}, size {size}: no output written"
            );
            assert_eq!(
                input.consumed(),
                0,
                "room {room}, size {size}: no input consumed"
            );
            assert_eq!(
                check, SENTINEL_CHECK,
                "room {room}, size {size}: check untouched"
            );
            assert_eq!(
                total_in, SENTINEL_TOTAL,
                "room {room}, size {size}: total_in untouched"
            );
            assert_eq!(
                total_out, SENTINEL_TOTAL,
                "room {room}, size {size}: total_out untouched"
            );
        }
    }

    /// Repeated passthroughs accumulate to the same result as one whole transfer.
    ///
    /// `deflate_stored` loops (`deflate.c` L1751, `do { } while (last == 0)`), so this is the
    /// shape the real caller uses.
    #[test]
    fn repeated_passthroughs_accumulate() {
        let mut input = InputCursor::new(HELLO);
        let mut storage = [0_u8; HELLO_LEN];
        let mut output = OutputCursor::new(&mut storage);
        let mut check = ADLER_INIT;
        let (mut total_in, mut total_out) = (0_u64, 0_u64);

        let mut moved = 0_usize;
        while !input.is_empty() && !output.is_empty() {
            let n = read_buf_into_output(
                &mut input,
                &mut output,
                3,
                WRAP_ZLIB,
                &mut check,
                &mut total_in,
                &mut total_out,
            );
            assert_ne!(n, 0, "progress must be made while both sides have capacity");
            moved += n;
        }

        assert_eq!(moved, HELLO_LEN);
        assert_eq!(
            check, ADLER_HELLO,
            "chunked passthrough must agree with one whole read"
        );
        assert_eq!((total_in, total_out), (HELLO_LEN as u64, HELLO_LEN as u64));
        assert_eq!(&storage[..], HELLO);
    }

    /// The passthrough honours `wrap` exactly as the direct read does.
    #[test]
    fn passthrough_honours_every_wrap_value() {
        for (wrap, init, expected) in [
            (WRAP_RAW, SENTINEL_CHECK, SENTINEL_CHECK),
            (WRAP_ZLIB, ADLER_INIT, ADLER_HELLO),
            (WRAP_GZIP, CRC_INIT, CRC_HELLO),
            (-WRAP_ZLIB, SENTINEL_CHECK, SENTINEL_CHECK),
            (-WRAP_GZIP, SENTINEL_CHECK, SENTINEL_CHECK),
        ] {
            let mut input = InputCursor::new(HELLO);
            let mut storage = [0_u8; 32];
            let mut output = OutputCursor::new(&mut storage);
            let mut check = init;
            let (mut total_in, mut total_out) = (0_u64, 0_u64);

            let n = read_buf_into_output(
                &mut input,
                &mut output,
                HELLO_LEN,
                wrap,
                &mut check,
                &mut total_in,
                &mut total_out,
            );

            assert_eq!(n, HELLO_LEN, "wrap {wrap}: the bytes move regardless");
            assert_eq!(
                check, expected,
                "wrap {wrap}: check value dispatch must match read_buf"
            );
            assert_eq!((total_in, total_out), (HELLO_LEN as u64, HELLO_LEN as u64));
        }
    }

    // ---------------------------------------------------------------------------------------
    // Overflow and saturation safety.
    // ---------------------------------------------------------------------------------------

    /// A destination shaped like the whole address space is clamped, not wrapped.
    ///
    /// `read_buf` derives its length from the two slice lengths, so a caller cannot ask for
    /// more than exists. The nearest equivalent of a `usize::MAX` size argument is a
    /// `usize::MAX` count handed to the accessors, which every one of them rejects or clamps.
    #[test]
    fn extreme_counts_never_wrap() {
        let mut input = InputCursor::new(HELLO);
        assert_eq!(input.peek_slice(usize::MAX), None);
        assert_eq!(input.consumed_tail(usize::MAX), None);
        assert!(!input.advance(usize::MAX));
        assert_eq!(input.consumed(), 0, "every rejection left the cursor alone");

        let mut storage = [0_u8; 8];
        let mut output = OutputCursor::new(&mut storage);
        assert_eq!(output.unwritten_up_to(usize::MAX).len(), 8);
        assert!(!output.advance(usize::MAX));
        assert_eq!(output.written(), 0);

        let mut check = SENTINEL_CHECK;
        let mut total_in = SENTINEL_TOTAL;
        let mut total_out = SENTINEL_TOTAL;
        let n = read_buf_into_output(
            &mut input,
            &mut output,
            usize::MAX,
            WRAP_ZLIB,
            &mut check,
            &mut total_in,
            &mut total_out,
        );
        assert_eq!(n, 8, "clamped to the eight bytes of output room");
        assert_eq!(total_in, SENTINEL_TOTAL + 8);
        assert_eq!(total_out, SENTINEL_TOTAL + 8);
    }

    /// Every byte value round-trips unaltered, including `0x00` and `0xff`.
    ///
    /// `read_buf` is a byte pipe: it must not translate line endings, stop at a NUL or treat
    /// any value specially. The C code's `zmemcpy` does not, and neither may this.
    #[test]
    fn every_byte_value_survives_the_copy() {
        let mut source = [0_u8; 256];
        for (index, slot) in source.iter_mut().enumerate() {
            *slot = u8::try_from(index).unwrap_or(0);
        }

        let mut input = InputCursor::new(&source);
        let mut dest = [0_u8; 256];
        let mut check = ADLER_INIT;
        let mut total_in = 0_u64;
        let n = read_buf(&mut input, &mut dest, WRAP_ZLIB, &mut check, &mut total_in);

        assert_eq!(n, 256);
        assert_eq!(dest, source, "all 256 byte values must arrive unaltered");
        assert_eq!(total_in, 256);
        assert_ne!(
            check, ADLER_INIT,
            "and must have been folded into the check value"
        );
    }
}
