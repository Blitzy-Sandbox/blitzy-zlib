//! Raw-DEFLATE decompression driven by caller-supplied input and output
//! callbacks: the Rust counterpart of `infback.c` (all 579 lines of it).
//!
//! This module implements the three entry points `inflateBackInit_`
//! (`infback.c` L25-L64), `inflateBack` (L191-L570) and `inflateBackEnd`
//! (L572-L579), declared at `zlib.h` L1113, L1138-L1140 and L1208. It decodes a
//! complete raw DEFLATE stream -- RFC 1951, with no zlib (RFC 1950) or gzip
//! (RFC 1952) wrapper and no check value -- per call, obtaining input from an
//! `in()` callback and handing finished output to an `out()` callback.
//!
//! The reference file opens by explaining its own existence (`infback.c`
//! L6-L11): the code "is largely copied from inflate.c", normally only one of
//! the two is linked into an application, and "the interface with inffast.c is
//! retained so that optimized assembler-coded versions of `inflate_fast()` can
//! be used with either". That is why this module shares
//! [`InflateState`],
//! [`Mode`],
//! `inflate_table` and
//! `inflate_fast` with the [`crate::inflate`] subtree instead of
//! defining its own, and why it lives beside that subtree rather than inside it:
//! only its *public entry points* are distinct.
//!
//! # Why this exists at all, and why it is cheap
//!
//! `zlib.h` L1141-L1147 states the trade: `inflateBack` "avoids copying between
//! the output and the sliding window by simply making the window itself the
//! output buffer". The window is supplied by the caller, is `2**windowBits`
//! bytes, and doubles as the output buffer. `put` starts at the base of the
//! window and, when the window fills, C's `ROOM()` macro (L151-L162) hands the
//! *whole* window to `out()`, resets `put` to the base and marks the entire
//! window valid history. The memory profile is therefore flat: one caller-owned
//! buffer, no growth, no second copy.
//!
//! # ★ The caller-supplied window is the structural difference from `inflate()`
//!
//! `inflate()` allocates and owns its window lazily. `inflateBack` does not
//! allocate one at all -- `inflateBackInit_` stores the caller's pointer
//! (`infback.c` L59) and `inflateBackEnd` frees only the state (L572-L578).
//!
//! ★ **The window is supplied to [`inflate_back`] once per call, and is not stored in
//! the state at all.** C can keep the pointer for the state's whole life; a Rust
//! `&mut [u8]` cannot, because holding one asserts *exclusive* access for as long as
//! it is held, and `zlib.h` L1174-L1175 asks the application only not to change the
//! window "until `inflateBack()` returns". Between calls, and after `inflateBackEnd`,
//! the buffer is the caller's again -- to read, to reuse, or to free. A borrow retained
//! across that boundary would be a claim the caller is entitled to violate and, worse,
//! one that can outlive the memory it describes. [`inflate_back_init`] therefore
//! records only the required extent, in `wsize`, and [`inflate_back`] refuses a window
//! whose length disagrees with it.
//!
//! That also disposes of the one problem safe Rust has here that C does not. C's `put`
//! and `state->window` are two pointers to the same bytes; two live `&mut` references
//! to one buffer are not expressible in Rust and never will be. Because the window
//! arrives as the *output buffer* and the state's own window slot stays
//! [`InflateState`]-absent throughout, only one `&mut` ever exists. `inflate_fast` is
//! built for exactly this: an absent window slot is how it recognises the
//! `inflateBack` caller, after which window offsets and output offsets name the same
//! byte.
//!
//! # ★ The window is *not* zero-initialised, and `whave` is the guard
//!
//! The caller may hand over a freshly allocated buffer; `test/infcover.c` fills
//! every allocation with `0xa5` precisely to catch code that assumes otherwise.
//! `state.whave` is what records how much of the window has actually been
//! written: zero at entry (`infback.c` L217), and the full window size once
//! `ROOM()` has flushed it (L156). A distance may reach back only that far, and
//! that rule is enforced in two places -- the slow path's check at L516-L521 and
//! `inflate_fast`'s own `op > whave` test.
//!
//! ## Commit `09a1572` -- do not "restore" the deleted lines
//!
//! The baseline of this implementation is commit `09a1572`, "Fix `inflateBack()` bug that
//! would fail to detect a too far back", whose message continues: "The bug would
//! pass off an invalid deflate stream as good, and copy uninitialized memory
//! contents to the output." It **deletes** two lines from the fast-path
//! dispatch inside `case LEN:`, between the `RESTORE()` and the
//! `inflate_fast()` call:
//!
//! ```text
//! -    if (state->whave < state->wsize)
//! -        state->whave = state->wsize - left;
//! ```
//!
//! Widening `whave` there tells the decoder that window bytes nobody wrote are
//! valid history, which both accepts invalid streams and leaks uninitialised
//! memory into the output. `Backer::fast` therefore does nothing whatsoever
//! between restoring the cursors and calling `inflate_fast`, and this module
//! never assigns `whave` anywhere except in `Backer::room`, which is C's
//! `ROOM()`.
//!
//! # Callback polarity, which is inverted between the two directions
//!
//! `infback.c` L182-L185 spells out the contract, and it is genuinely
//! counter-intuitive: "`in()` should return zero on failure. `out()` should return
//! non-zero on failure." Either failure makes `inflateBack` return
//! `Z_BUF_ERROR`, and `strm->next_in` is how the caller tells the two apart --
//! it is null only when `in()` failed (`zlib.h` L1196-L1202).
//!
//! This implementation expresses both directions so that the polarity cannot be misread,
//! and the facade translates:
//!
//! | C | Here | Facade adapter must |
//! |---|---|---|
//! | `in()` returns 0 | [`InflateBackInput::next_chunk`] returns [`None`] or an empty slice | map a zero return to `None` |
//! | `in()` returns `n > 0`, sets `*buf` | `next_chunk` returns `Some(chunk)` with `chunk.len() == n` | rebuild the slice from `*buf` and `n` |
//! | `out()` returns 0 | [`InflateBackOutput::write_out`] returns [`Ok`] | map zero to `Ok(())` |
//! | `out()` returns non-zero | `write_out` returns [`Err`]`(`[`OutputFailure`]`)` | map non-zero to `Err` |
//! | `strm->next_in = Z_NULL` | [`InflateBackResult::next_in`] is [`None`] | publish a null `next_in` and `avail_in = 0` |
//! | `strm->next_in = next; strm->avail_in = have` | `next_in` is `Some(rest)` | publish `rest.as_ptr()` and `rest.len()` |
//!
//! C's `in_desc` and `out_desc` are opaque descriptors the library never
//! interprets (`infback.c` L177-L180); here they are simply whatever state the
//! trait implementor carries in `&mut self`.
//!
//! # Not resumable, unlike `inflate()`
//!
//! `inflate()` suspends and resumes across calls, which is why its bit
//! accumulator and mode persist. `inflateBack` does not: it resets
//! `mode = TYPE`, `last`, `whave`, `hold` and `bits` at entry (`infback.c`
//! L214-L223) and runs a whole raw stream, returning only when the stream ends
//! or something fails. `zlib.h` L1150-L1152 puts it plainly -- it "may then be
//! used multiple times to inflate a complete, raw deflate stream with each
//! call". Nothing here therefore has to be restartable mid-symbol, and the
//! caller-visible bit state at return is deliberately not meaningful.
//!
//! # No panics, and no hangs, on any input
//!
//! This code decodes untrusted data, so it is written to fail rather than to
//! trap. There is no `unwrap`, no `expect`, no `panic!` and no panicking index
//! outside `#[cfg(test)]`; every buffer access goes through `get`/`get_mut` and
//! every arithmetic narrowing through the checked helpers below. Every loop
//! either makes progress or leaves:
//!
//! * the bit-reader loops advance `bits` by eight per byte and stop the moment
//!   `in()` declines, which is C's `PULL()` turning into `Z_BUF_ERROR`;
//! * the stored-block loop copies `min(length, have, left)` bytes, and both
//!   `have >= 1` and `left >= 1` hold after `PULL()` and `ROOM()` succeed, so
//!   `length` strictly decreases;
//! * the match-copy loop likewise copies at least one byte per turn, because
//!   `ROOM()` guarantees `left >= 1` and the window is never empty;
//! * the two Huffman lookup loops pull at most two bytes before the entry's
//!   width is satisfied, and stop when input runs out;
//! * the code-length loop advances `have` towards `nlen + ndist` and rejects any
//!   repeat that would overshoot.
//!
//! # Correspondence with the reference
//!
//! | C construct | Here |
//! |---|---|
//! | `LOAD` / `RESTORE` (L69-L88) | no counterpart: the cursors *are* `Backer`'s fields, so there is nothing to spill |
//! | `INITBITS` (L91-L95) | `InflateState::init_bits` |
//! | `PULL` (L99-L109) | `Backer::pull` |
//! | `PULLBYTE` (L113-L119) | `Backer::pull_byte` |
//! | `NEEDBITS` (L124-L128) | `Backer::need_bits` |
//! | `BITS` (L131-L132) | `low_bits` -- deliberately *not* `InflateState::low_bits`; see its note |
//! | `DROPBITS` (L135-L139) | `drop_bits` |
//! | `BYTEBITS` (L142-L146) | `InflateState::byte_bits` |
//! | `ROOM` (L151-L162) | `Backer::room` |
//! | `goto inf_leave` | `Step::Leave`, funnelled into the single `Backer::finish` call site |
//! | `inf_leave:` (L560-L568) | `Backer::finish` |
//!
//! # Visibility
//!
//! The three entry points and the two callback traits are `pub`, because
//! `crates/libz-rs-sys/src/infback.rs` builds `inflateBackInit_`, `inflateBack`
//! and `inflateBackEnd` on top of them. Everything else is private. Nothing here
//! is `#[no_mangle]`, `extern "C"` or `#[repr(C)]`: `zlib.map` lists
//! `inflate_fast`, `inflate_table` and `inflate_fixed` in its `local:` block, and
//! the safe core exports no symbol at all.
//!
//! [`InflateBackResult::next_in`]: crate::infback::InflateBackResult::next_in
//! [`InflateWindow`]: crate::inflate::state::InflateWindow
//! [`InflateWindow::absent`]: crate::inflate::state::InflateWindow::absent
//! [`OutputFailure`]: crate::infback::OutputFailure
//! [`inflate_back`]: crate::infback::inflate_back
//! [`inflate_back_init`]: crate::infback::inflate_back_init

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

use crate::allocate::Allocator;
use crate::error::ReturnCode;
use crate::inflate::inffast::{
    inflate_fast, MIN_AVAIL_IN, MIN_AVAIL_OUT, MSG_INVALID_DISTANCE_CODE,
    MSG_INVALID_DISTANCE_TOO_FAR_BACK, MSG_INVALID_LITERAL_LENGTH_CODE,
};
use crate::inflate::inftrees::{inflate_table, Code, CodeTableSource, CodeType, TABLE_OK};
use crate::inflate::mode::Mode;
use crate::inflate::state::InflateState;
use crate::inflate::{
    MSG_INVALID_BIT_LENGTH_REPEAT, MSG_INVALID_BLOCK_TYPE, MSG_INVALID_CODE_LENGTHS_SET,
    MSG_INVALID_DISTANCES_SET, MSG_INVALID_LITERAL_LENGTHS_SET, MSG_INVALID_STORED_BLOCK_LENGTHS,
    MSG_MISSING_END_OF_BLOCK, MSG_TOO_MANY_SYMBOLS,
};
use crate::read_buf::{OutputCursor, OutputRegion};

/// Permutation of the code-length code lengths, RFC 1951 §3.2.7.
///
/// A private copy rather than a shared one on purpose: `infback.c` L205-L206
/// declares its own `static const unsigned short order[19]`, exactly as
/// `inflate.c` L800-L801 does, and the two are independent definitions of the
/// same table. Keeping that shape means a reviewer diffing this file against the
/// reference finds the array where the reference puts it.
///
/// Mirrors `infback.c` L205-L206.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Root index width requested for the code-length alphabet (`infback.c` L324).
const CODES_ROOT_BITS: usize = 7;

/// Root index width requested for the literal/length alphabet
/// (`infback.c` L401).
///
/// ★ `infback.c` L396-L398 warns: "do not change the lenbits or distbits values
/// here (9 and 6) without reading the comments in inftrees.h concerning the
/// ENOUGH constants, which depend on those values". `ENOUGH_LENS` (852) and
/// `ENOUGH_DISTS` (592) are sized for exactly these two widths, so changing
/// either would overflow the arena that
/// [`InflateState`] reserves.
const LENS_ROOT_BITS: usize = 9;

/// Root index width requested for the distance alphabet (`infback.c` L410).
///
/// See the warning on [`LENS_ROOT_BITS`].
const DISTS_ROOT_BITS: usize = 6;

/// Number of code-length codes, i.e. the extent of [`ORDER`]
/// (`infback.c` L320).
const CODE_LENGTH_CODES: usize = 19;

/// Largest `nlen` a well-formed dynamic block may declare (`infback.c` L304).
const MAX_LITERAL_LENGTH_CODES: u32 = 286;

/// Largest `ndist` a well-formed dynamic block may declare (`infback.c` L304).
const MAX_DISTANCE_CODES: u32 = 30;

/// The end-of-block symbol, whose code length must be non-zero
/// (`infback.c` L389).
const END_OF_BLOCK_SYMBOL: usize = 256;

/// `op` bit marking a decode entry as end-of-block (`infback.c` L463).
const OP_END_OF_BLOCK: u8 = 32;

/// `op` bit marking a decode entry as invalid (`infback.c` L470 and L502).
const OP_INVALID_CODE: u8 = 64;

/// `op` low nibble: an extra-bit count, or a second-level table's index width
/// (`infback.c` L441, L477, L495 and L510).
const OP_EXTRA_BITS: u8 = 15;

/// `op` high nibble mask. A clear high nibble marks a second-level table link
/// (`infback.c` L437 and L491).
const OP_KIND_MASK: u8 = 0xf0;

/// Smallest code-length symbol that encodes a repeat rather than a literal
/// length, RFC 1951 §3.2.7 (`infback.c` L342).
const FIRST_REPEAT_SYMBOL: u16 = 16;

/// Code-length symbol 16: repeat the previous length 3..=6 times
/// (`infback.c` L347-L358).
const REPEAT_PREVIOUS_SYMBOL: u16 = 16;

/// Code-length symbol 17: repeat a zero length 3..=10 times
/// (`infback.c` L360-L365).
const REPEAT_SHORT_ZERO_SYMBOL: u16 = 17;

// C narrows freely between `unsigned`, `unsigned long` and pointers. Every such
// narrowing here goes through one of the helpers below, so that no conversion
// can panic in a debug build and each saturating fallback has one place to be
// justified. Every fallback is unreachable on any target where `usize` is at
// least 32 bits wide: no value in this decoder exceeds `2**32`.

/// A slice index or byte count, from the 64-bit accumulator domain.
#[inline]
fn to_index(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A bit or byte count, from the `usize` domain.
#[inline]
fn to_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// C's `(unsigned)` narrowing of an accumulator value.
#[inline]
fn low_u32(value: u64) -> u32 {
    u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX)
}

/// C's `(unsigned short)` narrowing, used where a code length reaches `lens`.
#[inline]
fn low_u16(value: u64) -> u16 {
    u16::try_from(value & u64::from(u16::MAX)).unwrap_or(u16::MAX)
}

/// C's `(unsigned char)` narrowing, used where a literal reaches the window.
#[inline]
fn low_u8(value: u64) -> u8 {
    u8::try_from(value & u64::from(u8::MAX)).unwrap_or(u8::MAX)
}

/// `BITS(n)`: the low `count` bits of the accumulator (`infback.c` L131-L132).
///
/// ★ Unlike [`InflateState::low_bits`], this does **not** require the bits to be
/// present, and that difference is load-bearing. A decode table is indexed with
/// `hold & ((1 << root) - 1)` *before* anyone knows whether `root` bits are
/// available, because the entry found is what states how many bits the code
/// costs (`infback.c` L338, L433 and L487). The accumulator's unfilled high bits
/// are zero, so a short read merely selects a low entry, the loop pulls another
/// byte, and the index is recomputed -- which is precisely why those loops
/// terminate.
///
/// A `count` at or above 64 yields the whole accumulator, the limit of C's
/// `(1U << n) - 1` mask as `n` grows. No call site here passes more than 15.
#[inline]
fn low_bits(hold: u64, count: u32) -> u64 {
    match 1_u64.checked_shl(count) {
        Some(one) => hold & one.wrapping_sub(1),
        None => hold,
    }
}

/// `DROPBITS(n)`: remove `count` bits from the accumulator
/// (`infback.c` L135-L139).
///
/// Every call site is preceded either by a `NEEDBITS` or by a decode entry that
/// named the width, so at least `count` bits are always live and the checked
/// form in [`InflateState::drop_bits`] cannot fail. Discarding its result loses
/// nothing: were it ever to fail, the accumulator would be left exactly as it
/// was and the stream would go on to report invalid data rather than misdecode.
/// C's unsigned `bits -= n` would silently wrap in the same situation.
#[inline]
fn drop_bits<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>, count: u32) {
    let _dropped = state.drop_bits(count);
}

/// Copies `count` bytes forward inside one buffer, one byte at a time.
///
/// The Rust counterpart of the inner `do { *put++ = *from++; } while (--copy);` of
/// `infback.c` L539-L541, and of the `zmemcpy`-free half of the match copy.
///
/// # Byte-at-a-time is required, not a simplification
///
/// When `to > from` the two ranges overlap, and they are *meant* to: a match
/// with `distance < length` encodes a repeating run, so every byte after the
/// first `distance` of them is read back from what this loop has just written. A
/// bulk copy -- `copy_within`, `copy_from_slice` or `memmove` -- would read the
/// pre-existing bytes instead and produce different output. The read and the
/// write of one byte are strictly ordered here for that reason.
///
/// Returns `false` without completing if either range leaves the buffer. Both
/// ranges are provably inside it at every call site (see [`Backer::copy_match`]),
/// so this is what makes the bound structural rather than merely argued.
fn copy_forward(window: &mut [u8], from: usize, to: usize, count: usize) -> bool {
    let Some(from_end) = from.checked_add(count) else {
        return false;
    };
    let Some(to_end) = to.checked_add(count) else {
        return false;
    };
    if from_end > window.len() || to_end > window.len() {
        return false;
    }

    // `step` is bounded by `count`, and both ends were just checked against the
    // buffer length, so neither addition below can overflow and neither
    // accessor can fail.
    let mut step = 0;
    while step < count {
        let Some(&byte) = window.get(from + step) else {
            return false;
        };
        let Some(slot) = window.get_mut(to + step) else {
            return false;
        };
        *slot = byte;
        step += 1;
    }
    true
}

/// The output callback declined to accept the bytes it was handed.
///
/// The safe-Rust spelling of a non-zero return from C's `out_func`
/// (`zlib.h` L1136). It carries no detail because C's own contract carries none:
/// `inflateBack` looks only at whether the call failed, and reports every
/// failure as [`ReturnCode::BUF_ERROR`] (`infback.c` L157-L160 and L563-L565).
/// An implementor that needs to know *why* its write failed should record that
/// in its own state, which is what C's `out_desc` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct OutputFailure;

/// Supplies compressed input to [`inflate_back`], replacing C's `in_func`.
///
/// The Rust form of `typedef unsigned (*in_func)(void FAR *, z_const unsigned
/// char FAR * FAR *)` (`zlib.h` L1134-L1135). C's opaque `in_desc` descriptor
/// becomes `&mut self`: the library attaches no meaning to whatever an
/// implementor keeps there -- file handles, running totals, check values -- and
/// never inspects it.
///
/// # ★ Returning nothing means failure
///
/// `infback.c` L182 is explicit: "`in()` should return zero on failure", and
/// `zlib.h` L1170-L1172 adds that when there is no input available "`in()`
/// must return zero -- `buf` is ignored in that case -- and `inflateBack()` will
/// return a
/// buffer error". Both [`None`] and `Some(&[])` are that zero, and both are
/// treated identically, because C compares only the returned count. There is no
/// "temporarily out of input, ask again later": a stream that ends early is a
/// [`ReturnCode::BUF_ERROR`], and [`InflateBackResult::next_in`] is then [`None`]
/// so that the caller can tell an input failure from an output one.
///
/// # Why the chunk's lifetime is not tied to `&mut self`
///
/// The returned slice is valid until the next call or until [`inflate_back`]
/// returns -- `infback.c` L172-L174 requires exactly that of the application:
/// "The application must not change the provided input until `in()` is called
/// again or `inflateBack()` returns." A `&'i [u8]` independent of the `&mut self`
/// borrow is the direct expression of that promise, and it is what lets the
/// decoder keep decoding out of one chunk while remaining free to ask for the
/// next. An implementor typically holds a longer `&'i [u8]` and hands out
/// subslices of it.
pub trait InflateBackInput<'i> {
    /// Returns the next run of input bytes, or [`None`] to signal failure.
    ///
    /// Called only when the previous chunk has been fully consumed, which is
    /// C's `if (have == 0)` guard inside `PULL()` (`infback.c` L101).
    fn next_chunk(&mut self) -> Option<&'i [u8]>;
}

/// Accepts decompressed output from [`inflate_back`], replacing C's `out_func`.
///
/// The Rust form of `typedef int (*out_func)(void FAR *, unsigned char FAR *,
/// unsigned)` (`zlib.h` L1136). As with [`InflateBackInput`], C's opaque
/// `out_desc` becomes `&mut self` and the library never interprets it.
///
/// # ★ The polarity is the opposite of the input side
///
/// `infback.c` L182-L183: "`out()` should return non-zero on failure". A facade
/// adapter must therefore map a **zero** C return to [`Ok`] and a **non-zero**
/// one to [`Err`]. Getting this backwards turns every successful write into a
/// buffer error, and every failed one into silent data loss.
///
/// The slice is never longer than the window -- `zlib.h` L1177-L1178: "The
/// length written by `out()` will be at most the window size" -- and is a prefix
/// of the caller's own window buffer, which `zlib.h` L1175-L1177 forbids the
/// callback from modifying.
pub trait InflateBackOutput {
    /// Consumes `data`, the next run of decompressed bytes.
    ///
    /// Called when the window has filled (C's `ROOM()`, `infback.c` L157) and
    /// once more at the end of the call for whatever the window still holds
    /// (`infback.c` L562-L566). It is *not* called at all when the stream ends
    /// exactly on a window boundary, because there is then nothing left to
    /// write.
    ///
    /// # Errors
    ///
    /// [`OutputFailure`] when the bytes could not be accepted. [`inflate_back`]
    /// then stops and reports [`ReturnCode::BUF_ERROR`], leaving
    /// [`InflateBackResult::next_in`] as [`Some`] so that the caller can
    /// distinguish this from an input failure.
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure>;
}

/// Forwards through a mutable reference, so a caller may lend its source to
/// [`inflate_back`] instead of surrendering it.
impl<'i, T> InflateBackInput<'i> for &mut T
where
    T: InflateBackInput<'i> + ?Sized,
{
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        (**self).next_chunk()
    }
}

/// Forwards through a mutable reference, so a caller may lend its sink to
/// [`inflate_back`] and still read what was written afterwards.
impl<T> InflateBackOutput for &mut T
where
    T: InflateBackOutput + ?Sized,
{
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
        (**self).write_out(data)
    }
}

/// Everything C writes back through `z_stream` on the way out of `inflateBack`
/// (`infback.c` L565-L569), plus the message it may have set at L214.
///
/// C reaches the caller's stream through the `z_streamp` it was handed and
/// assigns three fields. This implementation has no `z_stream` and dereferences no
/// pointer, so the same three values come back here for the facade to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InflateBackResult<'i> {
    /// The status C returns: [`ReturnCode::STREAM_END`],
    /// [`ReturnCode::BUF_ERROR`], [`ReturnCode::DATA_ERROR`] or
    /// [`ReturnCode::STREAM_ERROR`].
    ///
    /// ★ [`ReturnCode::OK`] is impossible, as `zlib.h` L1204 states: "Note that
    /// `inflateBack()` cannot return `Z_OK`."
    pub code: ReturnCode,

    /// Unused input, i.e. `strm->next_in` and `strm->avail_in` together
    /// (`infback.c` L567-L568).
    ///
    /// [`Some`]`(rest)` publishes `rest.as_ptr()` as `next_in` and `rest.len()`
    /// as `avail_in`; an empty `rest` is the legal "nothing left, but the
    /// pointer is still valid" state.
    ///
    /// ★ [`None`] is C's `next_in == Z_NULL`, and it means specifically that
    /// [`InflateBackInput::next_chunk`] declined (`infback.c` L104). `zlib.h`
    /// L1199-L1202 makes that the documented way to tell an input failure from
    /// an output failure when [`Self::code`] is [`ReturnCode::BUF_ERROR`]: "an
    /// input or output error can be distinguished using strm->next_in which will
    /// be `Z_NULL` only if `in()` returned an error."
    pub next_in: Option<&'i [u8]>,

    /// `strm->msg`: the reason for a [`ReturnCode::DATA_ERROR`], or [`None`] for
    /// C's `Z_NULL`.
    ///
    /// Always cleared at entry (`infback.c` L214), so the facade may install it
    /// unconditionally. Every possible string is a `&'static str` literal taken
    /// from the reference character for character, which is why they are
    /// publishable as `const char *` with no allocation.
    pub msg: Option<&'static str>,
}

/// The two ways an arm of C's `switch (state->mode)` can end.
///
/// Naming them is what replaces `goto inf_leave`: an arm says how it finished,
/// and `Backer::run` acts on it, so the epilogue has exactly one call site
/// instead of eight jump targets.
///
/// There are only two variants, where the `inflate()` driver needs four, because
/// `inflateBack` never suspends: every exit is terminal, so there is no
/// "leave and resume here next time".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// C's `break` out of the `switch`, and equally C's `/* fallthrough */` at
    /// `infback.c` L420.
    ///
    /// The arm has already put the next state in `InflateState::mode`, so the
    /// dispatcher simply looks again. The fall-through is safe to model this way
    /// precisely because L419 assigns `state->mode = LEN` before it.
    Continue,

    /// C's `goto inf_leave`, with `ret` already decided.
    Leave(ReturnCode),
}

/// Which of the two live decode tables a lookup walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeTable {
    /// `state->lencode`, indexed with `state->lenbits`.
    Length,
    /// `state->distcode`, indexed with `state->distbits`.
    Distance,
}

/// The outcome of locating one Huffman code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// The entry the code selects. Any `DROPBITS` owed for a *first-level* entry
    /// that linked to a second-level table has already been applied.
    Found(Code),
    /// C's `PULLBYTE` found no input, so the enclosing state must leave with
    /// [`ReturnCode::BUF_ERROR`].
    NeedInput,
    /// The computed index fell outside the table.
    ///
    /// Unreachable from any stream: [`inflate_table`] always fills all
    /// `1 << root` root entries, a root width never exceeds `MAXBITS`, and a
    /// second-level index is bounded by `val + (1 << op)`, which is the extent
    /// that function reserved. It exists so that the lookup is total, and the
    /// caller turns it into the invalid-code data error the state would have
    /// produced anyway.
    OutOfRange,
}

/// The root index width of the table `which` names, i.e. `state->lenbits` or
/// `state->distbits` (`infback.c` L338, L433 and L487).
#[inline]
fn root_bits<'a, A: Allocator<'a>>(state: &InflateState<'a, A>, which: DecodeTable) -> usize {
    match which {
        DecodeTable::Length => state.tables.lenbits,
        DecodeTable::Distance => state.tables.distbits,
    }
}

/// The table `which` names, i.e. `state->lencode` or `state->distcode`.
#[inline]
fn decode_table<'s, 'a, A: Allocator<'a>>(
    state: &'s InflateState<'a, A>,
    which: DecodeTable,
) -> &'s [Code] {
    match which {
        DecodeTable::Length => state.lencode(),
        DecodeTable::Distance => state.distcode(),
    }
}

/// `state->lens[order[state->have++]] = length` (`infback.c` L317 and L321).
///
/// Returns `false` if the permutation or the length array cannot be indexed.
/// Neither can happen: `have` is below nineteen at every call, [`ORDER`] has
/// nineteen entries, and every value in it is far below the extent of `lens`.
fn store_code_length<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>, length: u16) -> bool {
    let position = to_index(u64::from(state.have));
    let Some(&slot) = ORDER.get(position) else {
        return false;
    };
    let Some(entry) = state.lens.get_mut(to_index(u64::from(slot))) else {
        return false;
    };
    *entry = length;
    state.have = state.have.saturating_add(1);
    true
}

/// `state->lens[state->have++] = length`, bounded by the two alphabets' combined
/// size (`infback.c` L344 and L381).
///
/// Returns `false` once `have` has reached `total` or the end of `lens`. Every
/// caller has already established that it has not -- the `while` condition at
/// L336 and the overrun check at L374 -- so the guard is what makes the write
/// provably in range rather than argued to be.
fn store_length_at_have<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    length: u16,
    total: usize,
) -> bool {
    let position = to_index(u64::from(state.have));
    if position >= total {
        return false;
    }
    let Some(entry) = state.lens.get_mut(position) else {
        return false;
    };
    *entry = length;
    state.have = state.have.saturating_add(1);
    true
}

/// Creates the decoder state [`inflate_back`] needs, borrowing the caller's
/// window.
///
/// The core half of `inflateBackInit_` (`infback.c` L25-L64), declared at
/// `zlib.h` L1113. C's steps map as follows:
///
/// | C | Here |
/// |---|---|
/// | L30-L32 `version`/`stream_size` check giving `Z_VERSION_ERROR` | the facade; `ZLIB_VERSION` is not this crate's business |
/// | L33-L35 `strm` and `window` null checks | the facade for `strm`; a `&mut [u8]` cannot be null |
/// | L33-L35 `windowBits < 8 \|\| windowBits > 15` | [`InflateState::for_inflate_back`], through `crate::config::validate_inflate_back_window_bits` |
/// | L36 `strm->msg = Z_NULL` | the facade, which owns `z_stream` |
/// | L37-L50 defaulting `zalloc`/`zfree` to `zcalloc`/`zcfree` | the facade, which owns the hook pointers |
/// | L51-L53 `ZALLOC` of the state, `Z_MEM_ERROR` on failure | the facade, which boxes the returned value through the caller's `zalloc` |
/// | L55 installing it into `strm->state` | the facade |
/// | L56-L62 `dmax`, `wbits`, `wsize`, `window`, `wnext`, `whave`, `sane` | the returned value, fully initialised |
///
/// ★ **The window is not an argument here, and that is the point.** C's L59 keeps the
/// caller's pointer for the state's whole life; a `&mut [u8]` cannot, because it
/// asserts exclusive access for as long as it is held while `zlib.h` L1174-L1175 asks
/// the application only to leave the buffer alone "until `inflateBack()` returns". The
/// window is therefore supplied to [`inflate_back`] once per call, and this
/// constructor records only its required extent in `wsize`. Its contents are
/// irrelevant -- see the module documentation on why `whave`, rather than
/// initialisation, is what makes the window safe to read from.
///
/// ★ C does **not** zero the freshly allocated state: there is no `zmemzero` at
/// L51-L53, unlike `inflateInit2_`. It does not need to, because `inflateBack`
/// assigns every field it reads before reading it. The value returned here is
/// fully initialised regardless, which is strictly safer and observationally
/// identical.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] when `window_bits` lies outside `8 ..= 15`, when
/// `window` is shorter than `2**window_bits`, or when that size is not
/// representable -- the code C returns at L35.
pub fn inflate_back_init<'a, A: Allocator<'a>>(
    window_bits: i32,
    allocator: A,
) -> Result<InflateState<'a, A>, ReturnCode> {
    InflateState::for_inflate_back(window_bits, allocator)
}

/// Releases everything the decoder state owns.
///
/// The port of `inflateBackEnd` (`infback.c` L572-L579), declared at `zlib.h` L1208.
///
/// # What is released here, and what is not
///
/// C does two things: it calls `ZFREE(strm, strm->state)` -- the *caller's* `zfree`, applied to
/// the enclosing state allocation -- and it sets `strm->state = Z_NULL`. This function does
/// neither of those two things, and saying that dropping the value performs C's `zfree` would be
/// wrong. Precisely:
///
/// * **This function** takes the state by value and calls
///   [`InflateState::release`](crate::inflate::state::InflateState::release), which returns every
///   buffer the state *owns* to **the allocator the state was constructed with**. The mechanism is
///   `WindowBlock`'s destructor, which calls `try_deallocate_bytes` on the `Allocator` value it
///   stored at allocation time -- so a block obtained from a caller's `zalloc` goes back through
///   that same caller's `zfree`, and never through Rust's global allocator. `Buffer` records its
///   owner's `AllocatorId` for exactly that reason, and `Buffer::release_to` refuses a block
///   offered to the wrong allocator rather than corrupting the caller's heap.
/// * **The facade** still has to free the allocation that *contains* the state and then clear
///   `z_stream.state`, both through the caller's hooks. That half of `inflateBackEnd` cannot
///   happen here, because this crate never allocated the container and holds no pointer to it.
///
/// The window is *not* freed, in C or here, because the caller owns it -- and this state never
/// held it in the first place: it arrives as an argument to [`inflate_back`] and the borrow ends
/// when that call returns.
///
/// Always [`ReturnCode::OK`]. C's three failure conditions at L573-L574 are all
/// unrepresentable for an owned [`InflateState`]: a null `strm`, a null
/// `strm->state` and a null `zfree` are the facade's to check, and
/// `test/infcover.c` exercises exactly that with
/// `inflateBackEnd(Z_NULL) == Z_STREAM_ERROR`.
///
/// ★ The state arrives by mutable reference for the reason
/// [`crate::inflate::inflate_end`] gives: a state moved into a call carries its
/// window's borrow in argument position, where freeing the window would be undefined
/// behaviour.
#[must_use]
pub fn inflate_back_end<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>) -> ReturnCode {
    state.release();
    ReturnCode::OK
}

/// Decompresses one complete raw DEFLATE stream, pulling input from `input` and
/// pushing finished output to `output`.
///
/// The Rust counterpart of `inflateBack` (`infback.c` L191-L570), declared at `zlib.h`
/// L1138-L1140. A raw stream is one with no zlib or gzip header and no trailer
/// (`zlib.h` L1155-L1162): this function decodes RFC 1951 block structure only,
/// and verifies no check value, because there is none to verify.
///
/// # Parameters
///
/// * `state` -- built by [`inflate_back_init`] and reusable across calls. Every
///   field this function depends on is reset at entry (`infback.c` L214-L223), so
///   a state that has already decoded a stream needs nothing done to it.
/// * `next_in` -- input the caller has already staged, i.e. `strm->next_in` and
///   `strm->avail_in`. `zlib.h` L1182-L1190 documents this "for convenience"
///   path: [`None`] is C's null `next_in`, in which case `input` is consulted
///   immediately; `Some(bytes)` is consumed before `input` is asked for more, and
///   `Some(&[])` is the legal "a valid pointer, but nothing behind it" case.
/// * `input` -- the `in()` callback. See [`InflateBackInput`], and note that
///   yielding nothing means failure.
/// * `output` -- the `out()` callback. See [`InflateBackOutput`], whose failure
///   polarity is the opposite of `input`'s.
///
/// Both callbacks are taken by value so that `&mut` references may be passed;
/// the blanket implementations on `&mut T` are what make that work, and are why a
/// caller can still inspect its sink afterwards.
///
/// # Returns
///
/// An [`InflateBackResult`] carrying the status, the unused input and the message
/// -- exactly the three things C writes back through `z_stream`. The status is
/// one of:
///
/// * [`ReturnCode::STREAM_END`] -- a complete stream was decoded (`infback.c`
///   L547).
/// * [`ReturnCode::DATA_ERROR`] -- the stream is malformed;
///   [`InflateBackResult::msg`] says how (L551).
/// * [`ReturnCode::BUF_ERROR`] -- a callback failed (L105, L158) or the final
///   flush failed (L565). [`InflateBackResult::next_in`] discriminates the two
///   directions.
/// * [`ReturnCode::STREAM_ERROR`] -- the state is not usable, or the decoder
///   somehow reached a mode `inflateBack` does not implement (L556).
///
/// Never [`ReturnCode::OK`]: `zlib.h` L1204 states outright that
/// "`inflateBack()` cannot return `Z_OK`".
///
/// # ★ The window is an argument, borrowed for exactly this call
///
/// C's `put` and `state->window` address the same bytes. Safe Rust cannot hold two `&mut`
/// views of one buffer, so there is only ever one: the window arrives here as
/// *the output buffer*, and the state's own window slot stays absent throughout --
/// permanently, for a state built by [`inflate_back_init`]. That is also exactly how
/// `inflate_fast` recognises this caller and reads history from the output buffer
/// instead of from a separate window.
///
/// Taking the window per call rather than storing it is what keeps the exclusive
/// borrow honest: `zlib.h` L1174-L1175 asks the application not to change the window
/// "until `inflateBack()` returns", and says nothing about the intervals in between --
/// during which the buffer is the caller's to read, write or free. See
/// [`InflateState::for_inflate_back`].
///
/// C's guards at L209-L210 -- a null `strm` or a null `strm->state` -- are the
/// facade's, because a `&mut InflateState` proves both. What is checked here instead
/// is that `window` really can serve as this state's output buffer: at least `wsize`
/// bytes, and a state that is not already carrying a window of its own. Anything else
/// is C's "the state was not initialized" case, [`ReturnCode::STREAM_ERROR`].
#[must_use]
pub fn inflate_back<'a, 'i, A, I, O>(
    state: &mut InflateState<'a, A>,
    window: &mut [u8],
    next_in: Option<&'i [u8]>,
    input: I,
    output: O,
) -> InflateBackResult<'i>
where
    A: Allocator<'a>,
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    let Some(window) = window_prefix(state, window) else {
        // C would proceed with `put == NULL` and `left == 0` here and loop forever
        // inside `ROOM()`; refusing the call is the same "state was not properly
        // initialized" outcome `zlib.h` L1203 describes, and it terminates.
        return InflateBackResult {
            code: ReturnCode::STREAM_ERROR,
            next_in,
            msg: None,
        };
    };
    run(state, window, next_in, input, output)
}

/// The exactly-`wsize`-byte prefix of `window` this state may use as its
/// `inflateBack` output buffer.
///
/// [`None`] -- which becomes [`ReturnCode::STREAM_ERROR`] -- when `wsize` is zero or
/// unrepresentable, when the window is shorter than `wsize`, or when the state already
/// carries a window of its own. [`inflate_back_init`] establishes the first, so this
/// only ever rejects a state that was not built for `inflateBack` or a caller that
/// supplied too small a buffer.
///
/// ★ **A longer window is accepted and truncated, not refused.** C receives a bare
/// `unsigned char *` and reads `state->wsize` bytes; it never learns how large the
/// caller's buffer really was, so a generous caller is simply not C's problem
/// (`zlib.h` L1163-L1166 states the minimum, not the exact size). Truncating here
/// reproduces that, and it is also what lets the rest of the module use
/// `window.len()` as C uses `state->wsize` and know the two are the same number: every
/// bound is then established against the slice that actually exists.
///
/// The last test is the one with no C counterpart. An `inflateBack` state owns no
/// window (`infback.c` L51 allocates only the state), so a state whose slot is
/// occupied was not built by [`inflate_back_init`] -- and proceeding would mean two
/// live views of two different buffers both claiming to be "the window".
fn window_prefix<'w, 'a, A: Allocator<'a>>(
    state: &InflateState<'a, A>,
    window: &'w mut [u8],
) -> Option<&'w mut [u8]> {
    let expected = usize::try_from(state.wsize).ok()?;
    if expected == 0 || !state.window.is_absent() {
        return None;
    }
    window.get_mut(..expected)
}

/// Resets the state, runs the state machine, and performs the `inf_leave`
/// epilogue exactly once.
///
/// This is `inflateBack`'s body from L214 onwards, with the window already
/// borrowed. Splitting it out is what guarantees the property the reference gets
/// from a label: the final flush at L562-L566 has **one** call site, and no
/// early return can skip it, because `Backer::run` returns a value rather than
/// jumping.
fn run<'a, 'i, A, I, O>(
    state: &mut InflateState<'a, A>,
    window: &mut [u8],
    next_in: Option<&'i [u8]>,
    input: I,
    output: O,
) -> InflateBackResult<'i>
where
    A: Allocator<'a>,
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    // L215-L217: reset the state.
    state.mode = Mode::Type;
    state.last = false;
    state.whave = 0;
    // L220-L221: `hold = 0; bits = 0;`, which is `INITBITS()`.
    state.init_bits();

    // L214 (`strm->msg = Z_NULL`) and L218-L223. `have` is implied by the extent
    // of `chunk`, so C's `have = next != Z_NULL ? strm->avail_in : 0` needs no
    // counterpart: `None` yields zero and `Some` yields the slice's length.
    // `left = state->wsize` is likewise implied by `put == 0` together with
    // `window.len() == wsize`, which `window_prefix` has just established.
    let mut backer = Backer {
        chunk: next_in,
        consumed: 0,
        window,
        put: 0,
        input,
        output,
        msg: None,
    };

    // L226-L558: `for (;;) switch (state->mode) { ... }`.
    let code = backer.drive(state);

    // L560-L568: the epilogue, reached from every `goto inf_leave` and from
    // nowhere else.
    let code = backer.finish(code);
    InflateBackResult {
        code,
        next_in: backer.rest(),
        msg: backer.msg,
    }
}

/// The locals of C's `inflateBack` that outlive a single state arm
/// (`infback.c` L193-L204).
///
/// Holding them in one value keeps every state method to two parameters, well
/// inside `clippy.toml`'s argument threshold, and puts the write-back to
/// [`InflateBackResult`] in exactly one place.
///
/// C's `LOAD()` and `RESTORE()` macros (L69-L88) have no counterpart, and that is
/// not an omission: they exist only to spill these locals into `z_stream` so that
/// `inflate_fast` can find them, and to reload them afterwards. Here the fields
/// *are* the canonical copy, and `inflate_fast` is handed `&mut` references to
/// them, so there is nothing to spill and nothing to reload.
struct Backer<'i, 'w, I, O> {
    /// The input run currently being decoded: C's `next`, before any of it has
    /// been consumed.
    ///
    /// [`None`] is C's `next == Z_NULL`, which is both the initial state when the
    /// caller staged no input and the state C forces at L104 when `in()` fails.
    chunk: Option<&'i [u8]>,

    /// How far into [`Self::chunk`] the decoder has read. C's `next` is
    /// `chunk[consumed..]` and C's `have` is its length.
    consumed: usize,

    /// The caller's window, which is also the output buffer
    /// (`zlib.h` L1141-L1144). Its length is `state.wsize`.
    window: &'w mut [u8],

    /// C's `put`, as an index into [`Self::window`]. C's `left` is
    /// `window.len() - put`.
    put: usize,

    /// The `in()` callback together with C's opaque `in_desc`.
    input: I,

    /// The `out()` callback together with C's opaque `out_desc`.
    output: O,

    /// C's `strm->msg`, accumulated here and published on the way out.
    msg: Option<&'static str>,
}

impl<'i, I, O> Backer<'i, '_, I, O>
where
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    /// C's `have`: input bytes still unread in the current chunk.
    #[inline]
    fn have(&self) -> usize {
        match self.chunk {
            Some(chunk) => chunk.len().saturating_sub(self.consumed),
            None => 0,
        }
    }

    /// C's `left`: window bytes still unwritten before the next flush.
    #[inline]
    fn left(&self) -> usize {
        self.window.len().saturating_sub(self.put)
    }

    /// The unused input, as [`InflateBackResult::next_in`] reports it
    /// (`infback.c` L567-L568).
    ///
    /// An exhausted chunk yields `Some(&[])` rather than [`None`], which is the
    /// distinction the caller depends on: C's `next` still points one past the
    /// last byte it read, and only an `in()` failure nulls it.
    #[inline]
    fn rest(&self) -> Option<&'i [u8]> {
        match self.chunk {
            Some(chunk) => Some(chunk.get(self.consumed..).unwrap_or(&[])),
            None => None,
        }
    }

    /// `PULL()`: assure that some input is available (`infback.c` L99-L109).
    ///
    /// Returns `false` when `in()` declined, having first set [`Self::chunk`] to
    /// [`None`] -- C's `next = Z_NULL` at L104 -- so that the caller can tell an
    /// input failure from an output one. The caller then leaves with
    /// [`ReturnCode::BUF_ERROR`].
    ///
    /// A callback that returns an empty slice is treated identically to one that
    /// returns [`None`], because C compares only the returned count against zero
    /// (L103).
    fn pull(&mut self) -> bool {
        if self.have() != 0 {
            return true;
        }

        // ★ **Drop the exhausted chunk's borrow *before* the callback runs.** C's `in()`
        // is documented to keep its bytes stable only "until in() is called again or
        // until inflateBack() returns" (`infback.c` L172-L174), so refilling the same
        // buffer on the next call is not merely permitted, it is the obvious
        // implementation -- `test/infcover.c`'s `pull` hands back the *same* static
        // array each time. A `&[u8]` still borrowing those bytes while the callback
        // writes them would be undefined behaviour whether or not it is ever read
        // again. Clearing first costs nothing: this line is reached only when the chunk
        // is exhausted, and both outcomes below assign the field anyway.
        self.chunk = None;
        self.consumed = 0;

        match self.input.next_chunk() {
            Some(chunk) if !chunk.is_empty() => {
                self.chunk = Some(chunk);
                true
            }
            // L104-L106: `next = Z_NULL`, which the cleared field above already is.
            _ => false,
        }
    }

    /// `PULLBYTE()`: move one input byte into the bit accumulator
    /// (`infback.c` L113-L119).
    ///
    /// The byte is consumed only once it has been accepted, so a refusal leaves
    /// both the input cursor and the accumulator exactly as they were. C's
    /// `have--; hold += ...; bits += 8;` is unconditional, and the two agree on
    /// every reachable input, because [`InflateState::pull_byte`] declines only
    /// above 56 live bits and this decoder never exceeds 39: the widest request
    /// is `NEEDBITS(32)` from a byte-aligned accumulator (`infback.c` L264-L265),
    /// and the widest Huffman code is 15 bits.
    fn pull_byte<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> bool {
        if !self.pull() {
            return false;
        }
        let Some(chunk) = self.chunk else {
            return false;
        };
        let Some(&byte) = chunk.get(self.consumed) else {
            return false;
        };
        if !state.pull_byte(byte) {
            return false;
        }
        self.consumed = self.consumed.saturating_add(1);
        true
    }

    /// `NEEDBITS(n)`: assure `needed` bits are live (`infback.c` L124-L128).
    ///
    /// Terminates for every input: each accepted byte raises `bits` by eight, and
    /// a refusal returns immediately.
    fn need_bits<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        needed: u32,
    ) -> bool {
        while !state.has_bits(needed) {
            if !self.pull_byte(state) {
                return false;
            }
        }
        true
    }

    /// `ROOM()`: flush the window through `out()` if it is full
    /// (`infback.c` L151-L162).
    ///
    /// ★ Three things happen together here, and their order is C's: `put` returns
    /// to the base of the window, `left` becomes the whole window again, and
    /// `state.whave` is set to that same size *before* `out()` is called (L154-L156).
    /// That last assignment is the only place in this module that touches `whave`,
    /// and it is what promotes the window from "partly written" to "entirely
    /// valid history" -- which is exactly why widening `whave` anywhere else
    /// (see the module documentation on commit `09a1572`) is a security defect.
    ///
    /// Returns `false` when `out()` failed, which the caller turns into
    /// [`ReturnCode::BUF_ERROR`] (L158). On success the window always has at
    /// least one free byte, because [`window_prefix`] rejected an empty window;
    /// that is what makes the copy loops below make progress.
    fn room<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> bool {
        if self.left() != 0 {
            return true;
        }
        self.put = 0;
        state.whave = state.wsize;
        self.output.write_out(&*self.window).is_ok()
    }

    /// Writes one literal byte into the window (`infback.c` L456-L457).
    ///
    /// The caller has just run [`Self::room`], so `put` addresses a free byte and
    /// the accessor cannot fail.
    fn put_byte(&mut self, byte: u8) -> bool {
        let Some(slot) = self.window.get_mut(self.put) else {
            return false;
        };
        *slot = byte;
        self.put = self.put.saturating_add(1);
        true
    }

    /// The `inf_leave` epilogue: write leftover output and settle the status
    /// (`infback.c` L560-L566).
    ///
    /// C's guard is `if (left < state->wsize)`, which is `put > 0` here: the
    /// window holds `put` bytes that no `ROOM()` has flushed yet. When the stream
    /// ends exactly on a window boundary, `put` is zero and `out()` is **not**
    /// called a second time.
    ///
    /// ★ A failure here only *downgrades* success: `ret` becomes
    /// [`ReturnCode::BUF_ERROR`] if and only if it was [`ReturnCode::STREAM_END`]
    /// (L563-L565). An already-failing status -- a data error, or a buffer error
    /// from a callback -- is reported unchanged, so the first failure is the one
    /// the caller sees.
    fn finish(&mut self, ret: ReturnCode) -> ReturnCode {
        if self.put == 0 {
            return ret;
        }
        let Some(written) = self.window.get(..self.put) else {
            return ret;
        };
        if self.output.write_out(written).is_err() && ret == ReturnCode::STREAM_END {
            return ReturnCode::BUF_ERROR;
        }
        ret
    }

    /// Records a data error and parks the decoder in [`Mode::Bad`].
    ///
    /// The three lines every rejection in the reference writes -- `strm->msg =
    /// "..."; state->mode = BAD; break;` -- with the `break` becoming
    /// [`Step::Continue`], because C's `break` leaves the `switch` and the `for
    /// (;;)` immediately re-enters it, sees `BAD`, and only *then* leaves with
    /// [`ReturnCode::DATA_ERROR`] (L550-L552). Returning the error directly here
    /// would skip that round trip and, with it, C's own ordering.
    fn reject<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        msg: &'static str,
    ) -> Step {
        self.msg = Some(msg);
        state.mode = Mode::Bad;
        Step::Continue
    }
}

impl<'i, I, O> Backer<'i, '_, I, O>
where
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    /// `for (;;) switch (state->mode) { ... }` (`infback.c` L226-L558).
    ///
    /// ★ Six of the thirty-two [`Mode`] variants are implemented, and that is not
    /// an oversight: `inflateBack` decodes raw DEFLATE, so there is no container
    /// header to parse and no check value to verify. The reference's `switch`
    /// likewise has exactly the arms `TYPE`, `STORED`, `TABLE`, `LEN`, `DONE`,
    /// `BAD` and `default`. Note in particular that `infback.c` has **no**
    /// `LENLENS` or `CODELENS` arm -- unlike `inflate.c`, which splits them out as
    /// resumable states, it reads the whole code-length alphabet inline inside
    /// `TABLE` (L313-L383), because it never has to suspend mid-alphabet.
    ///
    /// The catch-all is written out variant by variant rather than as `_`, so that
    /// adding a state to [`Mode`] is a compile error here rather than a silent
    /// fall-through into `Z_STREAM_ERROR`.
    ///
    /// ## Is the catch-all reachable?
    ///
    /// In C, yes -- and `test/infcover.c` deliberately reaches it. Its `pull()`
    /// callback writes `state->mode = SYNC` through the descriptor it was handed,
    /// with the comment "force an otherwise impossible situation", and the
    /// `default` arm then returns `Z_STREAM_ERROR` (L554-L557, whose own comment
    /// reads "can't happen, but makes compilers happy").
    ///
    /// In safe Rust it is not: `mode` is reset to [`Mode::Type`] at entry, the only
    /// assignments after that are the ones below, and a callback cannot reach the
    /// state at all -- there is no way to hold `&mut InflateState` while this
    /// function does. The arm is kept anyway, and returns exactly what C returns,
    /// because being unable to construct the situation today is not a reason to
    /// answer it wrongly tomorrow. It is deliberately *not* an `unreachable!()`:
    /// this decoder must never panic, and the workspace denies that macro.
    fn drive<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> ReturnCode {
        loop {
            let step = match state.mode {
                Mode::Type => self.block_type(state),
                Mode::Stored => self.stored(state),
                Mode::Table => self.table(state),
                Mode::Len => self.len(state),
                // L545-L548: the stream terminated properly.
                Mode::Done => Step::Leave(ReturnCode::STREAM_END),
                // L550-L552.
                Mode::Bad => Step::Leave(ReturnCode::DATA_ERROR),
                // L554-L557.
                Mode::Head
                | Mode::Flags
                | Mode::Time
                | Mode::Os
                | Mode::ExLen
                | Mode::Extra
                | Mode::Name
                | Mode::Comment
                | Mode::HCrc
                | Mode::DictId
                | Mode::Dict
                | Mode::TypeDo
                | Mode::CopyBlock
                | Mode::Copy
                | Mode::LenLens
                | Mode::CodeLens
                | Mode::LenFirst
                | Mode::LenExt
                | Mode::Dist
                | Mode::DistExt
                | Mode::Match
                | Mode::Lit
                | Mode::Check
                | Mode::Length
                | Mode::Mem
                | Mode::Sync => Step::Leave(ReturnCode::STREAM_ERROR),
            };
            match step {
                Step::Continue => {}
                Step::Leave(code) => return code,
            }
        }
    }

    /// Determines and dispatches the next block type, RFC 1951 §3.2.3.
    ///
    /// Reads the three-bit block header: one `BFINAL` bit and two `BTYPE` bits.
    ///
    /// ★ `DROPBITS(2)` at L259 sits *after* the inner `switch` and therefore runs
    /// on **every** path, the invalid-type path included -- C's inner `default`
    /// sets `BAD` and falls straight into it. Dropping the two bits only on the
    /// valid paths would leave the accumulator misaligned, which is unobservable
    /// for a stream that is about to fail but is a divergence all the same.
    fn block_type<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L230-L234: the previous block was the last one, so align to a byte and
        // finish. Nothing follows a raw stream, so there is no trailer to read.
        if state.last {
            state.byte_bits();
            state.mode = Mode::Done;
            return Step::Continue;
        }

        // L235-L237.
        if !self.need_bits(state, 3) {
            return Step::Leave(ReturnCode::BUF_ERROR);
        }
        state.last = low_bits(state.hold, 1) != 0;
        drop_bits(state, 1);

        // L238-L258.
        match low_bits(state.hold, 2) {
            // L239-L243: stored block.
            0 => state.mode = Mode::Stored,
            // L244-L249: fixed block. `InflateState::use_fixed_tables` is the
            // method form of `inflate_fixed(state)` (L245) -- the same four
            // assignments of `inftrees.c` L364-L372, applied to the state that
            // owns them.
            1 => {
                state.use_fixed_tables();
                state.mode = Mode::Len;
            }
            // L250-L254: dynamic block.
            2 => state.mode = Mode::Table,
            // L255-L257.
            _ => {
                self.msg = Some(MSG_INVALID_BLOCK_TYPE);
                state.mode = Mode::Bad;
            }
        }

        // L259.
        drop_bits(state, 2);
        Step::Continue
    }

    /// Copies a stored (uncompressed) block from input to output,
    /// RFC 1951 §3.2.4.
    ///
    /// The block header is a byte-aligned `LEN` followed by its one's complement
    /// `NLEN`, and L266 checks them against each other.
    ///
    /// ★ `NEEDBITS(32)` is why the accumulator must be wider than 32 bits. C's
    /// `hold` is an `unsigned long`, 64 bits wide on every LP64 target -- which is
    /// what the reference build this implementation is measured against uses -- and
    /// [`InflateState::hold`] is a `u64` for exactly that reason. A 32-bit
    /// accumulator would shift the length out of the top as the complement came
    /// in. `BYTEBITS()` at L264 leaves at most 32 bits live, so after the pull
    /// `hold` holds precisely the four header bytes and `hold >> 16` is the
    /// complement alone.
    fn stored<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L264-L265.
        state.byte_bits();
        if !self.need_bits(state, 32) {
            return Step::Leave(ReturnCode::BUF_ERROR);
        }

        // L266-L270, evaluated exactly as written:
        //     if ((hold & 0xffff) != ((hold >> 16) ^ 0xffff))
        if (state.hold & 0xffff) != ((state.hold >> 16) ^ 0xffff) {
            return self.reject(state, MSG_INVALID_STORED_BLOCK_LENGTHS);
        }

        // L271-L274.
        state.length = low_u32(low_bits(state.hold, 16));
        state.init_bits();

        // L277-L289: copy the block, a chunkful at a time.
        //
        // Terminates on every input: `PULL()` leaves `have >= 1` and `ROOM()`
        // leaves `left >= 1`, and `length` is non-zero by the loop condition, so
        // `copy` is at least one and `length` strictly decreases.
        while state.length != 0 {
            // L278-L280.
            let mut copy = to_index(u64::from(state.length));
            if !self.pull() {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            if !self.room(state) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }

            // L281-L282.
            copy = copy.min(self.have()).min(self.left());

            // L283-L288.
            if !self.copy_stored(copy) {
                // Unreachable: `copy` was just clamped to both extents.
                // Reporting "no progress was possible" rather than looping is the
                // safe answer if an invariant were ever violated.
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            state.length = state.length.saturating_sub(to_count(copy));
        }

        // L290-L292.
        state.mode = Mode::Type;
        Step::Continue
    }

    /// `zmemcpy(put, next, copy)` plus the four cursor updates
    /// (`infback.c` L283-L288).
    ///
    /// The source and destination are distinct buffers -- input chunk and window
    /// -- so this is a plain bulk copy, unlike the match copy, which may overlap
    /// itself. Both ranges are established before either is touched, which is what
    /// makes `copy_from_slice` provably total here.
    fn copy_stored(&mut self, copy: usize) -> bool {
        let Some(chunk) = self.chunk else {
            return false;
        };
        let Some(source_end) = self.consumed.checked_add(copy) else {
            return false;
        };
        let Some(source) = chunk.get(self.consumed..source_end) else {
            return false;
        };
        let Some(dest_end) = self.put.checked_add(copy) else {
            return false;
        };
        let Some(dest) = self.window.get_mut(self.put..dest_end) else {
            return false;
        };
        dest.copy_from_slice(source);
        self.consumed = source_end;
        self.put = dest_end;
        true
    }
}

impl<'i, I, O> Backer<'i, '_, I, O>
where
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    /// Locates one code in the root table, pulling input until the entry it
    /// selects says it has enough bits.
    ///
    /// The Rust counterpart of the bare `for (;;)` loops at `infback.c` L337-L341, L432-L436
    /// and L486-L490. Nothing is dropped here: the caller decides what the code
    /// actually cost, because a first-level entry may turn out to be a link into a
    /// second-level table.
    ///
    /// The index is `BITS(root)` even when fewer than `root` bits are live -- see
    /// [`low_bits`] -- and that is exactly why the loop terminates: a short read
    /// lands on an entry whose `bits` exceeds what is available, one more byte is
    /// pulled, and the index is recomputed. At most two pulls are ever needed,
    /// because no RFC 1951 code is longer than fifteen bits.
    fn root_lookup<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        which: DecodeTable,
    ) -> Lookup {
        loop {
            let width = to_count(root_bits(state, which));
            let index = to_index(low_bits(state.hold, width));
            let Some(&here) = decode_table(state, which).get(index) else {
                return Lookup::OutOfRange;
            };
            if u32::from(here.bits) <= state.bits {
                return Lookup::Found(here);
            }
            if !self.pull_byte(state) {
                return Lookup::NeedInput;
            }
        }
    }

    /// Locates one code, descending into a second-level table when the root entry
    /// is a link.
    ///
    /// The Rust counterpart of `infback.c` L432-L447 (literal/length) and L486-L501
    /// (distance), which differ in exactly one place, L437 versus L491:
    ///
    /// | Table | C test | Why |
    /// |---|---|---|
    /// | literal/length | `here.op && (here.op & 0xf0) == 0` | `op == 0` is a literal byte, not a link |
    /// | distance | `(here.op & 0xf0) == 0` | the distance alphabet has no literals, so a clear high nibble can only be a link |
    ///
    /// When it does descend, the `DROPBITS(last.bits)` that follows C's inner loop
    /// (L445 and L499) is applied here, leaving the caller exactly C's remaining
    /// work: drop `here.bits`. Unlike `inflate()`, there is no `state->back`
    /// accounting to do -- `infback.c` never reads that field, because
    /// `inflateMark` is meaningless for a function that cannot suspend.
    fn lookup_code<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        which: DecodeTable,
    ) -> Lookup {
        let last = match self.root_lookup(state, which) {
            Lookup::Found(here) => here,
            other => return other,
        };

        // L437 and L491.
        let is_link = match which {
            DecodeTable::Length => last.op != 0 && (last.op & OP_KIND_MASK) == 0,
            DecodeTable::Distance => (last.op & OP_KIND_MASK) == 0,
        };
        if !is_link {
            return Lookup::Found(last);
        }

        // L439-L444 and L493-L498: index the second-level table with the bits
        // above the ones the first level consumed.
        let base = u32::from(last.bits);
        let width = base.saturating_add(u32::from(last.op & OP_EXTRA_BITS));
        let here = loop {
            let offset = low_bits(state.hold, width).checked_shr(base).unwrap_or(0);
            let index = to_index(u64::from(last.val)).saturating_add(to_index(offset));
            let Some(&here) = decode_table(state, which).get(index) else {
                return Lookup::OutOfRange;
            };
            if base.saturating_add(u32::from(here.bits)) <= state.bits {
                break here;
            }
            if !self.pull_byte(state) {
                return Lookup::NeedInput;
            }
        };

        // L445 and L499.
        drop_bits(state, base);
        Lookup::Found(here)
    }
}

impl<'i, I, O> Backer<'i, '_, I, O>
where
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    /// Reads a dynamic block's header and builds its two decode tables,
    /// RFC 1951 §3.2.7.
    ///
    /// Three stages, in the reference's order:
    ///
    /// 1. L296-L311 the three counts `HLIT`, `HDIST` and `HCLEN`, and the bound on
    ///    the first two.
    /// 2. L313-L331 the code-length alphabet: `ncode` three-bit lengths in
    ///    [`ORDER`] permutation, the rest zero, then one [`inflate_table`] build
    ///    with a seven-bit root.
    /// 3. L334-L417 the literal/length and distance code lengths, read *with* that
    ///    alphabet, then the two real tables.
    ///
    /// ★ L419 assigns `state->mode = LEN` and then falls through into `case LEN:`.
    /// Because the assignment happens first, returning [`Step::Continue`] and
    /// letting the dispatcher look again is exactly equivalent -- the same arm runs
    /// next, with the same state.
    fn table<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L296-L302.
        if !self.need_bits(state, 14) {
            return Step::Leave(ReturnCode::BUF_ERROR);
        }
        state.nlen = low_u32(low_bits(state.hold, 5)).saturating_add(257);
        drop_bits(state, 5);
        state.ndist = low_u32(low_bits(state.hold, 5)).saturating_add(1);
        drop_bits(state, 5);
        state.ncode = low_u32(low_bits(state.hold, 4)).saturating_add(4);
        drop_bits(state, 4);

        // L303-L310. The reference wraps this in `#ifndef PKZIP_BUG_WORKAROUND`;
        // the shipped configuration does not define that macro, so the check is
        // unconditional here.
        if state.nlen > MAX_LITERAL_LENGTH_CODES || state.ndist > MAX_DISTANCE_CODES {
            return self.reject(state, MSG_TOO_MANY_SYMBOLS);
        }

        // L313-L331.
        match self.code_length_alphabet(state) {
            Step::Continue => {}
            leave @ Step::Leave(_) => return leave,
        }

        // L334-L383.
        match self.code_lengths(state) {
            Step::Continue => {}
            leave @ Step::Leave(_) => return leave,
        }

        // L385-L386: `if (state->mode == BAD) break;` -- the two rejections inside
        // the loop above break out of the `while`, not out of the `switch`, so the
        // arm has to notice on their behalf.
        if state.mode == Mode::Bad {
            return Step::Continue;
        }

        // L388-L394. C indexes `state->lens[256]` unconditionally, and may do so
        // safely because `nlen` is at least 257, so the code-length loop has
        // always written that entry.
        if state.lens.get(END_OF_BLOCK_SYMBOL).copied() == Some(0) {
            return self.reject(state, MSG_MISSING_END_OF_BLOCK);
        }

        // L396-L417.
        self.build_tables(state)
    }

    /// Reads the code-length alphabet and builds its decode table
    /// (`infback.c` L313-L331).
    ///
    /// The name is not a typo in the reference either -- L313 says "get code
    /// length code lengths (not a typo)". `ncode` of the nineteen lengths are sent
    /// explicitly, in [`ORDER`] permutation, and the remainder are zero.
    fn code_length_alphabet<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
    ) -> Step {
        // L314-L319. `ncode` is `BITS(4) + 4`, hence at most nineteen; clamping to
        // the extent of `ORDER` makes that bound structural rather than argued.
        state.have = 0;
        let ncode = to_index(u64::from(state.ncode)).min(CODE_LENGTH_CODES);
        while to_index(u64::from(state.have)) < ncode {
            if !self.need_bits(state, 3) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            if !store_code_length(state, low_u16(low_bits(state.hold, 3))) {
                // Unreachable: `have < ncode <= 19` and every `ORDER` entry is a
                // valid `lens` index.
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            drop_bits(state, 3);
        }

        // L320-L321: every code-length code not sent has length zero.
        while to_index(u64::from(state.have)) < CODE_LENGTH_CODES {
            if !store_code_length(state, 0) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
        }

        // L322-L326. All three assignments precede the build, and `lenbits` is
        // passed by reference, so the width the builder settles on overwrites the
        // seven whether the build succeeds or fails. `distcode` and `distbits` are
        // deliberately left alone: C does not touch them here, because the
        // distance table has no meaning until L409-L412 builds one.
        state.next = 0;
        state.tables.lencode = CodeTableSource::Dynamic { offset: 0 };
        state.tables.lenbits = CODES_ROOT_BITS;
        let built = inflate_table(
            CodeType::Codes,
            &state.lens,
            CODE_LENGTH_CODES,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.lenbits,
            &mut state.work,
        );

        // L327-L331.
        if built == TABLE_OK {
            Step::Continue
        } else {
            self.reject(state, MSG_INVALID_CODE_LENGTHS_SET)
        }
    }

    /// Reads the `nlen + ndist` literal/length and distance code lengths
    /// (`infback.c` L334-L383).
    ///
    /// Symbols 0..=15 are literal lengths; 16, 17 and 18 are the three repeat
    /// forms of RFC 1951 §3.2.7:
    ///
    /// | Symbol | Extra bits | Meaning |
    /// |---|---|---|
    /// | 16 | 2 | repeat the previous length 3..=6 times |
    /// | 17 | 3 | repeat a zero length 3..=10 times |
    /// | 18 | 7 | repeat a zero length 11..=138 times |
    ///
    /// ★ Two **distinct** sites both emit [`MSG_INVALID_BIT_LENGTH_REPEAT`], and
    /// both are reproduced: symbol 16 arriving with no previous length to copy
    /// (L350-L355), and any repeat that would run past the end of the two
    /// alphabets (L374-L379). C reaches both with a `break` that leaves the
    /// enclosing `while` rather than the `switch`, which is why they return
    /// [`Step::Continue`] with the mode already [`Mode::Bad`] and why the caller
    /// re-checks the mode at L385-L386.
    ///
    /// Note that only a *root* lookup is needed: the code-length alphabet has at
    /// most nineteen symbols and a seven-bit root, so no second-level table can
    /// exist and C performs no descent here either.
    fn code_lengths<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // ★ L335: `state->have = 0;` -- a *second* reset, because `have` counted
        // the nineteen code-length codes a moment ago and now has to count the
        // literal/length and distance lengths instead. Omitting it leaves `have`
        // at nineteen, which silently defeats the "nothing to repeat" check at
        // L350 and turns a data error into a buffer error.
        state.have = 0;

        // L336. Both counts are already bounded by L304, so `total` is at most
        // 316 and comfortably inside `lens`.
        let total =
            to_index(u64::from(state.nlen)).saturating_add(to_index(u64::from(state.ndist)));

        while to_index(u64::from(state.have)) < total {
            // L337-L341.
            let here = match self.root_lookup(state, DecodeTable::Length) {
                Lookup::Found(here) => here,
                Lookup::NeedInput => return Step::Leave(ReturnCode::BUF_ERROR),
                Lookup::OutOfRange => return self.reject(state, MSG_INVALID_CODE_LENGTHS_SET),
            };

            if here.val < FIRST_REPEAT_SYMBOL {
                // L342-L345: a literal code length.
                drop_bits(state, u32::from(here.bits));
                if !store_length_at_have(state, here.val, total) {
                    // Unreachable: the `while` condition just established
                    // `have < total`.
                    return Step::Leave(ReturnCode::BUF_ERROR);
                }
                continue;
            }

            // L346-L382: one of the three repeat forms.
            let repeat = match self.repeat_form(state, here) {
                Ok(repeat) => repeat,
                Err(step) => return step,
            };

            // L374-L379.
            let Some(reach) = to_index(u64::from(state.have)).checked_add(repeat.count) else {
                return self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
            };
            if reach > total {
                return self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
            }

            // L380-L381.
            let mut remaining = repeat.count;
            while remaining != 0 {
                if !store_length_at_have(state, repeat.length, total) {
                    // Unreachable: `have + count <= total` was just checked.
                    return Step::Leave(ReturnCode::BUF_ERROR);
                }
                remaining -= 1;
            }
        }

        Step::Continue
    }

    /// Decodes one repeat symbol into the length to write and how many times
    /// (`infback.c` L346-L373).
    ///
    /// The `Err` arm carries the [`Step`] the caller must return: either the
    /// out-of-input [`ReturnCode::BUF_ERROR`], or the "nothing to repeat"
    /// rejection at L350-L355, whose `break` leaves the code-length loop before
    /// the overrun check ever runs.
    fn repeat_form<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        here: Code,
    ) -> Result<Repeat, Step> {
        let width = u32::from(here.bits);
        if here.val == REPEAT_PREVIOUS_SYMBOL {
            // L347-L358.
            if !self.need_bits(state, width.saturating_add(2)) {
                return Err(Step::Leave(ReturnCode::BUF_ERROR));
            }
            drop_bits(state, width);
            if state.have == 0 {
                return Err(self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT));
            }
            // L356: `len = state->lens[state->have - 1]`. `have` is non-zero here,
            // and the entry was written by an earlier turn of the loop.
            let previous = to_index(u64::from(state.have)).saturating_sub(1);
            let Some(&length) = state.lens.get(previous) else {
                return Err(Step::Leave(ReturnCode::BUF_ERROR));
            };
            // L357-L358.
            let count = 3_usize.saturating_add(to_index(low_bits(state.hold, 2)));
            drop_bits(state, 2);
            Ok(Repeat { length, count })
        } else if here.val == REPEAT_SHORT_ZERO_SYMBOL {
            // L360-L366.
            if !self.need_bits(state, width.saturating_add(3)) {
                return Err(Step::Leave(ReturnCode::BUF_ERROR));
            }
            drop_bits(state, width);
            let count = 3_usize.saturating_add(to_index(low_bits(state.hold, 3)));
            drop_bits(state, 3);
            Ok(Repeat { length: 0, count })
        } else {
            // L367-L373: symbol 18, and equally any symbol above it. C's `else`
            // is unconditional, and `inflate_table` never produces a code-length
            // symbol above 18 for a nineteen-symbol alphabet.
            if !self.need_bits(state, width.saturating_add(7)) {
                return Err(Step::Leave(ReturnCode::BUF_ERROR));
            }
            drop_bits(state, width);
            let count = 11_usize.saturating_add(to_index(low_bits(state.hold, 7)));
            drop_bits(state, 7);
            Ok(Repeat { length: 0, count })
        }
    }

    /// Builds the literal/length and distance decode tables
    /// (`infback.c` L396-L417).
    ///
    /// The distance table starts where the literal/length table ended, which is
    /// why `state.next` is read into `distcode` *between* the two builds (L409)
    /// and why both share one cursor. See [`LENS_ROOT_BITS`] for why the two root
    /// widths may not be changed.
    fn build_tables<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L399-L403.
        state.next = 0;
        state.tables.lencode = CodeTableSource::Dynamic { offset: 0 };
        state.tables.lenbits = LENS_ROOT_BITS;
        let nlen = to_index(u64::from(state.nlen));
        let built = inflate_table(
            CodeType::Lens,
            &state.lens,
            nlen,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.lenbits,
            &mut state.work,
        );

        // L404-L408.
        if built != TABLE_OK {
            return self.reject(state, MSG_INVALID_LITERAL_LENGTHS_SET);
        }

        // L409-L412. `nlen` is at most 286 and `lens` holds 320 entries, so the
        // tail always exists; the fallible form is used because it is the only way
        // to take it without indexing.
        state.tables.distcode = CodeTableSource::Dynamic { offset: state.next };
        state.tables.distbits = DISTS_ROOT_BITS;
        let ndist = to_index(u64::from(state.ndist));
        let Some(distance_lens) = state.lens.get(nlen..) else {
            return self.reject(state, MSG_INVALID_DISTANCES_SET);
        };
        let built = inflate_table(
            CodeType::Dists,
            distance_lens,
            ndist,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.distbits,
            &mut state.work,
        );

        // L413-L417.
        if built != TABLE_OK {
            return self.reject(state, MSG_INVALID_DISTANCES_SET);
        }

        // L418-L419, then the fall-through into `case LEN:`.
        state.mode = Mode::Len;
        Step::Continue
    }
}

/// One decoded repeat instruction from the code-length alphabet.
///
/// C keeps these in the locals `len` and `copy` (`infback.c` L199 and L203);
/// naming them together is what lets [`Backer::repeat_form`] hand both back at
/// once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Repeat {
    /// C's `len`: the code length to write, which is zero for symbols 17 and 18.
    length: u16,
    /// C's `copy`: how many entries to write it into.
    count: usize,
}

impl<'i, I, O> Backer<'i, '_, I, O>
where
    I: InflateBackInput<'i>,
    O: InflateBackOutput,
{
    /// Decodes one literal, one length/distance pair, or an end-of-block code,
    /// RFC 1951 §3.2.5.
    ///
    /// Begins by handing the whole job to `inflate_fast` when there is enough
    /// input and output room for it to run without per-symbol bounds checks;
    /// otherwise decodes exactly one symbol and returns to the dispatcher.
    fn len<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L423-L429.
        if self.have() >= MIN_AVAIL_IN && self.left() >= MIN_AVAIL_OUT {
            return self.fast(state);
        }

        // L431-L448.
        let here = match self.lookup_code(state, DecodeTable::Length) {
            Lookup::Found(here) => here,
            Lookup::NeedInput => return Step::Leave(ReturnCode::BUF_ERROR),
            Lookup::OutOfRange => return self.reject(state, MSG_INVALID_LITERAL_LENGTH_CODE),
        };
        drop_bits(state, u32::from(here.bits));
        state.length = u32::from(here.val);

        // L450-L460: a literal byte.
        if here.op == 0 {
            if !self.room(state) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            if !self.put_byte(low_u8(u64::from(state.length))) {
                // Unreachable: `ROOM()` just guaranteed a free byte.
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            state.mode = Mode::Len;
            return Step::Continue;
        }

        // L462-L467: end of block. A raw stream has no trailer, so the next thing
        // to read is another block header.
        if here.op & OP_END_OF_BLOCK != 0 {
            state.mode = Mode::Type;
            return Step::Continue;
        }

        // L469-L474.
        if here.op & OP_INVALID_CODE != 0 {
            return self.reject(state, MSG_INVALID_LITERAL_LENGTH_CODE);
        }

        // L476-L482: the length's extra bits, if any.
        state.extra = u32::from(here.op & OP_EXTRA_BITS);
        if state.extra != 0 {
            if !self.need_bits(state, state.extra) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            state.length = state
                .length
                .saturating_add(low_u32(low_bits(state.hold, state.extra)));
            drop_bits(state, state.extra);
        }

        // L485-L521.
        match self.distance(state) {
            Step::Continue => {}
            leave @ Step::Leave(_) => return leave,
        }
        if state.mode == Mode::Bad {
            return Step::Continue;
        }

        // L524-L543.
        self.copy_match(state)
    }

    /// Hands the decode loop to `inflate_fast` (`infback.c` L423-L429).
    ///
    /// # ★★ Nothing may be inserted between the restore and the call
    ///
    /// Commit `09a1572` -- "Fix `inflateBack()` bug that would fail to detect a
    /// too far back" -- **deleted** two lines from precisely this spot:
    ///
    /// ```text
    /// -    if (state->whave < state->wsize)
    /// -        state->whave = state->wsize - left;
    /// ```
    ///
    /// They widened `whave` to cover window bytes that no `ROOM()` had ever
    /// flushed, which disabled `inflate_fast`'s `op > whave` guard and let an
    /// invalid stream copy uninitialised memory into the output. Do not restore
    /// them, in any form: the only assignment to `whave` in this module is the one
    /// in [`Backer::room`], which is C's own. See also the module documentation and
    /// `inflate_fast`'s `whave` section, which records the same warning from the
    /// other side of the call.
    ///
    /// # What replaces `RESTORE()` and `LOAD()`
    ///
    /// C spills six locals into `z_stream` (L80-L88), calls `inflate_fast`, then
    /// reloads them (L69-L77). Here the input slice, the input cursor and the output
    /// cursor are passed directly -- the cursors as `&mut`, so the callee updates
    /// them in place -- and `hold`, `bits` and `mode` already live in the state. The
    /// spill and the reload therefore have nothing to do.
    ///
    /// The output cursor is built here, around the call, rather than held in this
    /// structure: `inflateBack`'s output buffer is the window, which is a `&mut [u8]`
    /// this crate has already initialised, so it is an [`OutputRegion::init`] and
    /// reborrowing it for the duration of the call costs nothing. `put` is the
    /// cursor's position on the way in and is taken back from it on the way out, so
    /// there is never a second copy of the index to keep in step.
    ///
    /// # `start` is `wsize`, not `avail_out`
    ///
    /// The `start` argument is the `avail_out` the enclosing call *began* with, and
    /// `inflateBack` passes `state->wsize` (L426) because its output buffer is the
    /// window. That makes `inflate_fast`'s `beg` land on the base of the window, so
    /// window offsets and output offsets coincide -- which, together with the
    /// state's window slot being absent for the duration, is how `inflate_fast`
    /// serves history out of the output buffer.
    fn fast<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        let Some(chunk) = self.chunk else {
            // Unreachable: `have() >= 6` implies a chunk exists.
            return Step::Leave(ReturnCode::BUF_ERROR);
        };
        // L426. Equal to `self.window.len()`, which `window_prefix` established.
        let start = to_index(u64::from(state.wsize));
        let mut output = OutputCursor::from_region(OutputRegion::init(&mut *self.window), self.put);
        let exit = inflate_fast(state, chunk, &mut self.consumed, &mut output, start);
        self.put = output.written();
        // C's `inflate_fast` writes `strm->msg` in place on its three error paths
        // and leaves it alone otherwise, so this assignment is conditional too.
        if let Some(msg) = exit.msg {
            self.msg = Some(msg);
        }
        // L427-L428: `LOAD(); break;`. The resulting mode -- `LEN`, `TYPE` or
        // `BAD` -- is already in the state, so the dispatcher sees it next turn.
        Step::Continue
    }

    /// Decodes the distance code and its extra bits, then validates the reach
    /// (`infback.c` L485-L521).
    ///
    /// Returns [`Step::Continue`] with the mode left at [`Mode::Len`] when the
    /// distance is good, and with the mode at [`Mode::Bad`] when it is not; the
    /// caller re-checks, exactly as C's `break` out of the `switch` makes it.
    fn distance<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L486-L501.
        let here = match self.lookup_code(state, DecodeTable::Distance) {
            Lookup::Found(here) => here,
            Lookup::NeedInput => return Step::Leave(ReturnCode::BUF_ERROR),
            Lookup::OutOfRange => return self.reject(state, MSG_INVALID_DISTANCE_CODE),
        };
        drop_bits(state, u32::from(here.bits));

        // L502-L506.
        if here.op & OP_INVALID_CODE != 0 {
            return self.reject(state, MSG_INVALID_DISTANCE_CODE);
        }
        // L507.
        state.offset = u32::from(here.val);

        // L509-L515: the distance's extra bits, if any.
        state.extra = u32::from(here.op & OP_EXTRA_BITS);
        if state.extra != 0 {
            if !self.need_bits(state, state.extra) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            state.offset = state
                .offset
                .saturating_add(low_u32(low_bits(state.hold, state.extra)));
            drop_bits(state, state.extra);
        }

        // L516-L521, evaluated exactly as written:
        //
        //     if (state->offset > state->wsize - (state->whave < state->wsize ?
        //                                         left : 0))
        //
        // ★ This is the check commit `09a1572` restored the effectiveness of. Read
        // it in its two phases:
        //
        //   * before the window has ever been flushed, `whave` is zero, so the
        //     bound collapses to `wsize - left`, which is `put` -- a distance may
        //     reach only back to the base of the window, i.e. only over bytes this
        //     call has actually written;
        //   * once `ROOM()` has flushed the window, `whave == wsize`, the
        //     subtrahend is zero, and the whole window is legitimate history.
        //
        // Widening `whave` early therefore does not "optimise" anything; it hands
        // the decoder bytes nobody wrote.
        let reach = if state.whave < state.wsize {
            to_count(self.left())
        } else {
            0
        };
        if state.offset > state.wsize.saturating_sub(reach) {
            return self.reject(state, MSG_INVALID_DISTANCE_TOO_FAR_BACK);
        }

        Step::Continue
    }

    /// Copies a match out of the window and into the window (`infback.c`
    /// L524-L542).
    ///
    /// The window is both source and destination, and the source may lie either
    /// behind `put` or -- when the match reaches back past the base of the window
    /// -- wrapped around at its end. C picks between the two with one comparison
    /// (L528) and this reproduces it, in index form. Writing `p` for `put`, `w`
    /// for the window size and `d` for the distance, so that `left == w - p`:
    ///
    /// | C | Condition | Source index | Bytes available |
    /// |---|---|---|---|
    /// | L529-L530 | `w - d < left`, i.e. `p < d` | `p + w - d` (wrapped) | `d - p`, up to the end of the window |
    /// | L533-L534 | otherwise, i.e. `p >= d` | `p - d` (behind `put`) | `left` |
    ///
    /// Both ranges provably stay inside the window: in the first case
    /// `(p + w - d) + (d - p) == w`, and in the second `(p - d) + (w - p) == w - d`.
    /// The destination is bounded by `left`. That is what makes [`copy_forward`]'s
    /// guards inert rather than load-bearing -- they exist so the bound is checked
    /// rather than merely reasoned about.
    ///
    /// ★ The copy is byte-at-a-time because `d < length` is legal and means a
    /// repeating run; see [`copy_forward`].
    ///
    /// Terminates on every input: `ROOM()` leaves `left >= 1`, so each turn copies
    /// at least one byte and `length` strictly decreases. (C's inner
    /// `do { ... } while (--copy);` would run 2**32 times were `copy` ever zero;
    /// it never is, because a length code is at least three, and the loop here
    /// simply does nothing in that case rather than wrapping.)
    fn copy_match<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        loop {
            // L526.
            if !self.room(state) {
                return Step::Leave(ReturnCode::BUF_ERROR);
            }

            // L527-L535.
            let offset = to_index(u64::from(state.offset));
            let left = self.left();
            let span = self.window.len().saturating_sub(offset);
            let (from, available) = if span < left {
                (self.put.saturating_add(span), left.saturating_sub(span))
            } else {
                (self.put.saturating_sub(offset), left)
            };

            // L536-L538. `left -= copy` needs no counterpart: `left` is derived
            // from `put`, which advances by the same amount.
            let copy = available.min(to_index(u64::from(state.length)));
            state.length = state.length.saturating_sub(to_count(copy));

            // L539-L541.
            if !copy_forward(self.window, from, self.put, copy) {
                // Unreachable: both ranges were shown above to end at or before
                // the window's last byte.
                return Step::Leave(ReturnCode::BUF_ERROR);
            }
            self.put = self.put.saturating_add(copy);

            // L542. The mode stays `LEN`, so the dispatcher decodes the next
            // symbol.
            if state.length == 0 {
                return Step::Continue;
            }
        }
    }
}

#[cfg(test)]
// The workspace denies the panic family in library code, which is the whole point
// of a module that decodes untrusted input; a test that cannot assert is useless,
// so the harness opts back in here only, which is exactly what clippy.toml's
// `allow-unwrap-in-tests` and `allow-panic-in-tests` keys are for.
// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::{
        inflate_back, inflate_back_end, inflate_back_init, low_bits, InflateBackInput,
        InflateBackOutput, InflateBackResult, OutputFailure,
    };
    use crate::allocate::{GlobalAllocator, SENTINEL_FILL};
    use crate::config::{MAX_WBITS, MIN_WBITS};
    use crate::error::ReturnCode;
    use crate::inflate::inffast::{
        MSG_INVALID_DISTANCE_CODE, MSG_INVALID_DISTANCE_TOO_FAR_BACK,
        MSG_INVALID_LITERAL_LENGTH_CODE,
    };
    use crate::inflate::{
        MSG_INVALID_BIT_LENGTH_REPEAT, MSG_INVALID_BLOCK_TYPE, MSG_INVALID_CODE_LENGTHS_SET,
        MSG_INVALID_DISTANCES_SET, MSG_INVALID_LITERAL_LENGTHS_SET,
        MSG_INVALID_STORED_BLOCK_LENGTHS, MSG_MISSING_END_OF_BLOCK, MSG_TOO_MANY_SYMBOLS,
    };
    use alloc::vec;
    use alloc::vec::Vec;

    // Every byte string below was emitted by, or verified against, the C zlib in
    // this repository (through `zlib.compressobj(level, DEFLATED, -15)` and
    // `zlib.decompressobj(-15)`, which are that library). Each malformed fixture
    // is annotated with the exact `strm->msg` the reference reports for it, so a
    // divergence in either the status or the text shows up as a failing assertion
    // rather than as a silent behaviour change.

    /// `"hello, hello!"` raw-deflated at level 6 -- one fixed-Huffman block with a
    /// match, which is the payload `test/example.c` uses throughout.
    const HELLO_FIXED: &[u8] = &[
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00,
    ];

    /// The same payload at level 0: a single stored block.
    const HELLO_STORED: &[u8] = &[
        0x01, 0x0d, 0x00, 0xf2, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68, 0x65, 0x6c,
        0x6c, 0x6f, 0x21,
    ];

    /// The decoded form of both fixtures above.
    const HELLO: &[u8] = b"hello, hello!";

    /// A dynamic-Huffman block: `TEXT` at level 9. Exercises the whole `TABLE`
    /// arm -- the code-length alphabet, all three repeat forms, and both real
    /// tables.
    const TEXT_DYNAMIC: &[u8] = &[
        0x0b, 0xc9, 0x48, 0x55, 0x28, 0x2c, 0xcd, 0x4c, 0xce, 0x56, 0x48, 0x2a, 0xca, 0x2f, 0xcf,
        0x53, 0x48, 0xcb, 0xaf, 0x50, 0xc8, 0x2a, 0xcd, 0x2d, 0x28, 0x56, 0xc8, 0x2f, 0x4b, 0x2d,
        0x52, 0x28, 0x01, 0x4a, 0xe7, 0x24, 0x56, 0x55, 0x2a, 0xa4, 0xe4, 0xa7, 0xeb, 0x29, 0x84,
        0x8c, 0x2a, 0x1e, 0x49, 0x8a, 0x01,
    ];

    /// The sentence `TEXT_DYNAMIC` encodes, twelve times over: 540 bytes, whose
    /// longest match reaches back 45.
    const SENTENCE: &[u8] = b"The quick brown fox jumps over the lazy dog. ";

    /// Two blocks in one stream: a non-final stored block, then a final fixed one.
    const STORED_THEN_FIXED: &[u8] = &[
        0x00, 0x05, 0x00, 0xfa, 0xff, 0x66, 0x69, 0x72, 0x73, 0x74, 0xab, 0x29, 0x4e, 0x4d, 0xce,
        0xcf, 0x4b, 0x01, 0x00,
    ];

    /// A final fixed block containing nothing but an end-of-block code. This is
    /// the `"\x03"` stream `test/infcover.c`'s `cover_back()` feeds first, and it
    /// must return `Z_STREAM_END` having never called `out()`.
    const EMPTY_FIXED: &[u8] = &[0x03, 0x00];

    /// `b"abcabcabc"` seventy times over, at level 6: 630 bytes of output, which
    /// overflows a 256-byte window twice.
    const REPEATED: &[u8] = &[
        0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x45, 0xa3, 0x88, 0xbe, 0x08, 0x00,
    ];

    /// 256 `b'z'` at level 6: exactly one 256-byte window of output.
    const ONE_WINDOW: &[u8] = &[0xab, 0xaa, 0x1a, 0xd9, 0x00, 0x00];

    /// ★ The `09a1572` regression fixture: a fixed block holding one literal
    /// `b'a'` and then a length-3 match at distance 5. Only one byte has been
    /// written, so the distance reaches five bytes before anything valid -- and
    /// before the commit that check was defeated for the fast path, which "would
    /// pass off an invalid deflate stream as good, and copy uninitialized memory
    /// contents to the output".
    ///
    /// The reference reports `"invalid distance too far back"`.
    const TOO_FAR_BACK: &[u8] = &[0x4b, 0x04, 0x12, 0x00];

    /// A literal `b'a'` then a length-258 match at distance 1: the maximal
    /// self-overlapping run, which only a byte-at-a-time copy reproduces.
    const RLE_MAXIMAL: &[u8] = &[0x4b, 0x1c, 0x05, 0x00];

    /// An input source that hands out fixed-size pieces of one buffer, then
    /// declines.
    ///
    /// The piece size is what selects the decode path: a source that never yields
    /// six bytes at once keeps `have` below `MIN_AVAIL_IN`, so the fast-path
    /// dispatch at `infback.c` L424 can never fire.
    struct Pieces<'i> {
        queue: Vec<&'i [u8]>,
        next: usize,
        calls: usize,
    }

    impl<'i> Pieces<'i> {
        fn new(stream: &'i [u8], size: usize) -> Self {
            assert!(size > 0, "a zero-byte piece would never make progress");
            Self {
                queue: stream.chunks(size).collect(),
                next: 0,
                calls: 0,
            }
        }

        /// A source that declines immediately, like `test/infcover.c`'s
        /// `pull(Z_NULL, ..)`.
        fn exhausted() -> Self {
            Self {
                queue: Vec::new(),
                next: 0,
                calls: 0,
            }
        }

        /// A source whose first answer is an empty slice, which C's `have == 0`
        /// test treats identically to no answer at all.
        fn yielding_nothing() -> Self {
            Self {
                queue: vec![&[][..]],
                next: 0,
                calls: 0,
            }
        }
    }

    impl<'i> InflateBackInput<'i> for Pieces<'i> {
        fn next_chunk(&mut self) -> Option<&'i [u8]> {
            self.calls += 1;
            let piece = self.queue.get(self.next).copied();
            self.next += 1;
            piece
        }
    }

    /// A sink that records everything it is handed, or refuses everything.
    #[derive(Default)]
    struct Sink {
        written: Vec<u8>,
        calls: usize,
        refuse: bool,
    }

    impl InflateBackOutput for Sink {
        fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
            self.calls += 1;
            if self.refuse {
                return Err(OutputFailure);
            }
            self.written.extend_from_slice(data);
            Ok(())
        }
    }

    /// Everything one `inflate_back` call is worth asserting about.
    #[derive(Debug)]
    struct Outcome {
        code: ReturnCode,
        msg: Option<&'static str>,
        output: Vec<u8>,
        out_calls: usize,
        in_calls: usize,
        /// `Some(n)` is a non-null `next_in` with `avail_in == n`; `None` is C's
        /// null `next_in`, which happens only when the input callback declined.
        avail_in: Option<usize>,
    }

    /// Runs one stream through a fresh state.
    ///
    /// `piece` chooses how the input arrives: `Some(n)` feeds it through the
    /// callback `n` bytes at a time with nothing staged, and `None` stages the
    /// whole stream as `next_in` and gives the callback nothing to offer -- which
    /// is the `zlib.h` L1182-L1190 "for convenience" path, and how
    /// `test/infcover.c` drives it.
    fn drive(stream: &[u8], window_bits: i32, piece: Option<usize>, refuse: bool) -> Outcome {
        let size = 1_usize << window_bits;
        // Filled the way `test/infcover.c`'s tracking allocator fills every block,
        // so that any byte the decoder emits without having written it first shows
        // up in the output as 0xa5.
        let mut window = vec![SENTINEL_FILL; size];
        let mut state =
            inflate_back_init(window_bits, GlobalAllocator).expect("the exponent is valid");

        let mut source = match piece {
            Some(size) => Pieces::new(stream, size),
            None => Pieces::exhausted(),
        };
        let mut sink = Sink {
            refuse,
            ..Sink::default()
        };
        let staged = if piece.is_some() { None } else { Some(stream) };

        let result = inflate_back(&mut state, &mut window, staged, &mut source, &mut sink);
        let outcome = summarise(&result, &source, sink);
        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
        outcome
    }

    fn summarise(result: &InflateBackResult<'_>, source: &Pieces<'_>, sink: Sink) -> Outcome {
        Outcome {
            code: result.code,
            msg: result.msg,
            output: sink.written,
            out_calls: sink.calls,
            in_calls: source.calls,
            avail_in: result.next_in.map(<[u8]>::len),
        }
    }

    /// Asserts that `stream` decodes to `expected` however the input is delivered.
    ///
    /// The three deliveries are chosen to select different code paths from the
    /// same bytes, so an inconsistency between them fails here rather than in
    /// production:
    ///
    /// * one byte at a time -- `have` never reaches `MIN_AVAIL_IN`, so every
    ///   symbol goes through the slow path, and every `NEEDBITS` is interrupted;
    /// * five bytes at a time -- still one short of the fast-path threshold;
    /// * the whole stream at once -- the fast path runs whenever the window has
    ///   258 bytes free.
    fn assert_decodes(stream: &[u8], window_bits: i32, expected: &[u8]) {
        for piece in [Some(1), Some(5), Some(stream.len()), None] {
            let outcome = drive(stream, window_bits, piece, false);
            assert_eq!(
                outcome.code,
                ReturnCode::STREAM_END,
                "piece = {piece:?}, msg = {:?}",
                outcome.msg
            );
            assert_eq!(outcome.msg, None, "piece = {piece:?}");
            assert_eq!(outcome.output, expected, "piece = {piece:?}");
            assert_eq!(outcome.avail_in, Some(0), "piece = {piece:?}");
        }
    }

    /// Narrows a value that is already known to be one byte wide.
    ///
    /// Test code, so the panicking form is permitted -- and wanted: a mask that
    /// stopped being wide enough should fail the test rather than silently
    /// truncate, which is exactly what a plain `as` cast would do.
    fn as_byte(value: u32) -> u8 {
        u8::try_from(value & 0xff).expect("masked to a single byte")
    }

    /// Length codes 257..=285: `(symbol, extra bits, smallest length)`,
    /// RFC 1951 §3.2.5.
    const LENGTH_CODES: [(u32, u32, u32); 29] = [
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

    /// Distance codes 0..=29: `(code, extra bits, smallest distance)`,
    /// RFC 1951 §3.2.5.
    const DISTANCE_CODES: [(u32, u32, u32); 30] = [
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

    /// Writes one final fixed-Huffman block, RFC 1951 §3.2.6.
    ///
    /// Only what these tests need, but exact: header fields and extra bits go in
    /// least-significant-bit first, Huffman codes most-significant-bit first, as
    /// RFC 1951 §3.1.1 requires. `the_test_writer_agrees_with_the_reference`
    /// pins the result against bytes the C implementation produced.
    struct FixedBlock {
        bytes: Vec<u8>,
        accumulator: u32,
        live: u32,
    }

    impl FixedBlock {
        /// Opens a block, writing the `BFINAL` bit and `BTYPE = 01`.
        fn new() -> Self {
            let mut block = Self {
                bytes: Vec::new(),
                accumulator: 0,
                live: 0,
            };
            block.bits(1, 1);
            block.bits(1, 2);
            block
        }

        /// Least-significant-bit first: header fields and extra bits.
        fn bits(&mut self, value: u32, count: u32) {
            for offset in 0..count {
                self.accumulator |= ((value >> offset) & 1) << self.live;
                self.live += 1;
                if self.live == 8 {
                    self.bytes.push(as_byte(self.accumulator));
                    self.accumulator = 0;
                    self.live = 0;
                }
            }
        }

        /// Most-significant-bit first: Huffman codes.
        fn code(&mut self, value: u32, count: u32) {
            for offset in (0..count).rev() {
                self.bits((value >> offset) & 1, 1);
            }
        }

        /// One literal/length symbol in the fixed code, RFC 1951 §3.2.6.
        fn symbol(&mut self, symbol: u32) {
            match symbol {
                0..=143 => self.code(0x30 + symbol, 8),
                144..=255 => self.code(0x190 + (symbol - 144), 9),
                256..=279 => self.code(symbol - 256, 7),
                _ => self.code(0xc0 + (symbol - 280), 8),
            }
        }

        fn literal(&mut self, byte: u8) {
            self.symbol(u32::from(byte));
        }

        fn length(&mut self, length: u32) {
            let &(symbol, extra, base) = LENGTH_CODES
                .iter()
                .rev()
                .find(|&&(_, _, base)| length >= base)
                .expect("a length of at least three");
            self.symbol(symbol);
            if extra != 0 {
                self.bits(length - base, extra);
            }
        }

        fn distance(&mut self, distance: u32) {
            let &(code, extra, base) = DISTANCE_CODES
                .iter()
                .rev()
                .find(|&&(_, _, base)| distance >= base)
                .expect("a distance of at least one");
            self.code(code, 5);
            if extra != 0 {
                self.bits(distance - base, extra);
            }
        }

        /// Writes the end-of-block code and pads to a byte boundary.
        fn finish(mut self) -> Vec<u8> {
            self.symbol(256);
            if self.live != 0 {
                self.bytes.push(as_byte(self.accumulator));
                self.accumulator = 0;
                self.live = 0;
            }
            self.bytes
        }
    }

    #[test]
    fn the_test_writer_agrees_with_the_reference() {
        // Both expectations are bytes the C implementation itself produced, so a
        // bug in the writer above cannot masquerade as correct decoding below.
        let mut block = FixedBlock::new();
        block.literal(b'a');
        block.length(258);
        block.distance(1);
        assert_eq!(block.finish(), RLE_MAXIMAL);

        let mut block = FixedBlock::new();
        block.literal(b'a');
        block.length(3);
        block.distance(5);
        assert_eq!(block.finish(), TOO_FAR_BACK);
    }

    #[test]
    fn a_fixed_block_round_trips_however_the_input_arrives() {
        assert_decodes(HELLO_FIXED, MAX_WBITS, HELLO);
    }

    #[test]
    fn a_stored_block_round_trips_however_the_input_arrives() {
        assert_decodes(HELLO_STORED, MAX_WBITS, HELLO);
    }

    #[test]
    fn a_dynamic_block_round_trips_however_the_input_arrives() {
        let expected: Vec<u8> = SENTENCE.repeat(12);
        assert_decodes(TEXT_DYNAMIC, MAX_WBITS, &expected);
    }

    #[test]
    fn two_blocks_in_one_stream_round_trip() {
        // Only the second block carries BFINAL, so `TYPE` is re-entered after the
        // stored block's end and the fixed block's end-of-block code.
        assert_decodes(STORED_THEN_FIXED, MAX_WBITS, b"first|second");
    }

    #[test]
    fn the_smallest_window_decodes_the_same_bytes_as_the_largest() {
        // ★ With `windowBits == 8` the window is 256 bytes, so `left` can never
        // reach `MIN_AVAIL_OUT` (258) and the fast-path dispatch at `infback.c`
        // L424 is structurally unreachable. Every symbol therefore goes through
        // the slow path. Comparing that against a 32 KiB window, where the fast
        // path does run, is the strongest black-box assertion available that the
        // two paths agree.
        let expected: Vec<u8> = SENTENCE.repeat(12);
        assert_decodes(TEXT_DYNAMIC, MIN_WBITS, &expected);
        assert_decodes(TEXT_DYNAMIC, MAX_WBITS, &expected);
    }

    #[test]
    fn staged_input_is_consumed_before_the_callback_is_asked() {
        // `zlib.h` L1182-L1190: input may be provided up front through `next_in`
        // and `avail_in`. This stream needs nothing more, so the callback -- which
        // would fail -- is never reached.
        let outcome = drive(HELLO_FIXED, MAX_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(outcome.in_calls, 0);
        assert_eq!(outcome.avail_in, Some(0));
    }

    #[test]
    fn unused_trailing_input_is_handed_back() {
        // `infback.c` L567-L568 returns whatever the last `in()` call provided and
        // the decoder did not need. Four trailing bytes are not part of the
        // stream, so they must come back as `avail_in == 4` with a non-null
        // `next_in`.
        let mut stream = HELLO_FIXED.to_vec();
        stream.extend_from_slice(b"tail");
        let outcome = drive(&stream, MAX_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(outcome.avail_in, Some(4));
    }

    #[test]
    fn a_state_decodes_a_second_stream_without_being_reset() {
        // `infback.c` L214-L223 resets everything the decoder reads, so `zlib.h`
        // L1150-L1152's "may then be used multiple times" needs no help from the
        // caller. Notably `whave` returns to zero, so the second stream cannot
        // reach back into the first one's history.
        let mut window = vec![SENTINEL_FILL; 1_usize << MAX_WBITS];
        let mut state = inflate_back_init(MAX_WBITS, GlobalAllocator).expect("valid exponent");

        for _ in 0..2 {
            let mut source = Pieces::exhausted();
            let mut sink = Sink::default();
            let result = inflate_back(
                &mut state,
                &mut window,
                Some(HELLO_FIXED),
                &mut source,
                &mut sink,
            );
            assert_eq!(result.code, ReturnCode::STREAM_END);
            assert_eq!(sink.written, HELLO);
        }

        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn output_longer_than_the_window_is_flushed_a_window_at_a_time() {
        // 630 bytes through a 256-byte window: `ROOM()` flushes at 256 and at 512,
        // and the epilogue flushes the remaining 118.
        let expected: Vec<u8> = b"abcabcabc".repeat(70);
        assert_eq!(expected.len(), 630);
        let outcome = drive(REPEATED, MIN_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, expected);
        assert_eq!(outcome.out_calls, 3);
    }

    #[test]
    fn exactly_one_window_of_output_is_flushed_once() {
        // 256 bytes into a 256-byte window. `ROOM()` runs only *before* a write,
        // so the window is still holding all 256 bytes when the epilogue runs;
        // `put` is non-zero there and the single flush happens at L563.
        let outcome = drive(ONE_WINDOW, MIN_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, vec![b'z'; 256]);
        assert_eq!(outcome.out_calls, 1);
    }

    #[test]
    fn an_empty_stream_never_calls_the_output_callback() {
        // The `"\x03"` case from `test/infcover.c`'s `cover_back()`: a final fixed
        // block whose first code is end-of-block. Nothing is written, so `left`
        // still equals `wsize` at `infback.c` L562 and the guard skips `out()`
        // altogether. This is the only way `put` can be zero at the epilogue.
        let outcome = drive(EMPTY_FIXED, MAX_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert!(outcome.output.is_empty());
        assert_eq!(outcome.out_calls, 0);
    }

    #[test]
    fn a_match_reaching_before_the_window_base_reads_the_wrapped_history() {
        // Fill the window with 256 distinct bytes, which flushes it and sets
        // `whave = wsize`, then ask for eight bytes at distance 250. `put` is back
        // at zero, so `wsize - offset` (6) is below `left` (256) and the source is
        // the *wrapped* index `put + 6`; the bytes there are the ones the first
        // pass wrote.
        let mut block = FixedBlock::new();
        for byte in 0..=255_u8 {
            block.literal(byte);
        }
        block.length(8);
        block.distance(250);
        let stream = block.finish();

        let mut expected: Vec<u8> = (0..=255_u8).collect();
        expected.extend(6..=13_u8);

        assert_decodes(&stream, MIN_WBITS, &expected);
    }

    #[test]
    fn a_stored_block_longer_than_the_window_crosses_the_boundary() {
        // ★ A 300-byte stored block, whose `LEN` is 0x012c and `NLEN` 0xfed3. This
        // is the `NEEDBITS(32)` case: the fourth header byte has to shift in above
        // 24 already-live bits, which a 32-bit accumulator could not hold together
        // with the length. C's `hold` is an `unsigned long`, and
        // `InflateState::hold` is a `u64` for the same reason.
        let payload: Vec<u8> = (0..300_u32).map(|index| as_byte(index * 13 + 7)).collect();
        let mut stream = vec![0x01, 0x2c, 0x01, 0xd3, 0xfe];
        stream.extend_from_slice(&payload);

        assert_decodes(&stream, MIN_WBITS, &payload);
    }

    #[test]
    fn a_thirty_two_bit_header_splits_into_length_and_complement() {
        // The arithmetic of `infback.c` L266, in isolation. `BYTEBITS()` leaves at
        // most 32 bits live, so after `NEEDBITS(32)` the accumulator holds exactly
        // the four header bytes: the low sixteen are `LEN` and the next sixteen are
        // its one's complement.
        let hold: u64 = 0xedcb_1234;
        assert_eq!(low_bits(hold, 16), 0x1234);
        assert_eq!(hold & 0xffff, (hold >> 16) ^ 0xffff);

        // A mismatched complement is what the check exists to catch.
        let corrupt: u64 = 0xedcb_1235;
        assert_ne!(corrupt & 0xffff, (corrupt >> 16) ^ 0xffff);
    }

    #[test]
    fn a_distance_reaching_past_valid_history_is_rejected_on_the_slow_path() {
        // Four bytes of input, so `have` is below `MIN_AVAIL_IN` at the `LEN`
        // state and the check at `infback.c` L516-L521 is the one that fires.
        let outcome = drive(TOO_FAR_BACK, MAX_WBITS, Some(TOO_FAR_BACK.len()), false);
        assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
        // Exactly the one literal the stream legitimately produced, and nothing
        // else. A 0xa5 here would be the uninitialised window leaking out.
        assert_eq!(outcome.output, b"a");
    }

    #[test]
    fn a_distance_reaching_past_valid_history_is_rejected_on_the_fast_path() {
        // ★ The same stream, padded so that `have` is comfortably above
        // `MIN_AVAIL_IN` when `LEN` is reached; with a 32 KiB window `left` is far
        // above `MIN_AVAIL_OUT`, so the dispatch at L424 hands the whole job to
        // `inflate_fast` and *its* `op > whave` guard is what must reject the
        // distance. This is the path the two lines commit `09a1572` deleted used
        // to disable.
        let mut stream = TOO_FAR_BACK.to_vec();
        stream.extend_from_slice(&[0; 8]);
        let outcome = drive(&stream, MAX_WBITS, Some(stream.len()), false);
        assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
        assert_eq!(outcome.output, b"a");
    }

    #[test]
    fn no_uninitialised_window_byte_ever_reaches_the_output() {
        // The window arrives full of 0xa5, exactly as `test/infcover.c` leaves its
        // allocations. Whatever the outcome, and on either decode path, not one of
        // those bytes may be emitted.
        for piece in [Some(TOO_FAR_BACK.len()), None, Some(1)] {
            let outcome = drive(TOO_FAR_BACK, MAX_WBITS, piece, false);
            assert!(
                !outcome.output.contains(&SENTINEL_FILL),
                "piece = {piece:?} leaked an unwritten window byte"
            );
        }
    }

    #[test]
    fn a_distance_within_written_history_is_accepted_before_any_flush() {
        // The counterpart of the two rejections above: with `whave` still zero, a
        // distance that reaches back only over bytes this call has written is
        // perfectly legal, and `HELLO_FIXED` is exactly such a stream -- its match
        // repeats "hello" from five bytes earlier.
        let outcome = drive(HELLO_FIXED, MAX_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
    }

    #[test]
    fn a_distance_of_one_repeats_the_previous_byte() {
        // Length 258 at distance 1: the maximal run RFC 1951 can code. Every byte
        // after the first is read back from what the copy has just written, which
        // only a strictly ordered byte-at-a-time copy reproduces.
        assert_decodes(RLE_MAXIMAL, MAX_WBITS, &vec![b'a'; 259]);
        // And through a window small enough that the run straddles a flush.
        assert_decodes(RLE_MAXIMAL, MIN_WBITS, &vec![b'a'; 259]);
    }

    #[test]
    fn a_partly_overlapping_match_repeats_its_own_prefix() {
        // Distance 3, length 10: the first three bytes come from history and the
        // remaining seven from bytes this match itself produced.
        let mut block = FixedBlock::new();
        for byte in b"xyz" {
            block.literal(*byte);
        }
        block.length(10);
        block.distance(3);
        let stream = block.finish();

        assert_decodes(&stream, MAX_WBITS, b"xyzxyzxyzxyzx");
    }

    #[test]
    fn an_input_failure_nulls_the_reported_input() {
        // A truncated stream: the decoder asks for more, the source declines, and
        // `infback.c` L104 sets `next = Z_NULL` before leaving with
        // `Z_BUF_ERROR`. `zlib.h` L1199-L1202 makes that null the documented way
        // to tell an input failure from an output one.
        let truncated = &HELLO_FIXED[..4];
        let outcome = drive(truncated, MAX_WBITS, None, false);
        assert_eq!(outcome.code, ReturnCode::BUF_ERROR);
        assert_eq!(outcome.avail_in, None, "in() failure must null next_in");
        assert_eq!(outcome.msg, None, "a buffer error is not a data error");
    }

    #[test]
    fn an_output_failure_leaves_the_reported_input_intact() {
        // The mirror image: the whole stream is present, so the failure is the
        // sink's, and `next_in` must stay non-null.
        let outcome = drive(HELLO_FIXED, MAX_WBITS, None, true);
        assert_eq!(outcome.code, ReturnCode::BUF_ERROR);
        assert_eq!(
            outcome.avail_in,
            Some(0),
            "out() failure must leave next_in non-null"
        );
        assert_eq!(outcome.out_calls, 1, "the epilogue flush is the failure");
        assert!(outcome.output.is_empty());
    }

    #[test]
    fn an_empty_chunk_from_the_input_callback_is_a_failure() {
        // C compares only the count `in()` returned against zero, so a callback
        // that hands back a valid pointer and no bytes has failed.
        let mut window = vec![SENTINEL_FILL; 1_usize << MAX_WBITS];
        let mut state = inflate_back_init(MAX_WBITS, GlobalAllocator).expect("valid exponent");
        let mut source = Pieces::yielding_nothing();
        let mut sink = Sink::default();

        let result = inflate_back(&mut state, &mut window, None, &mut source, &mut sink);
        assert_eq!(result.code, ReturnCode::BUF_ERROR);
        assert_eq!(result.next_in, None);
        assert_eq!(source.calls, 1);
        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_failing_epilogue_only_downgrades_success() {
        // `infback.c` L563-L565 replaces the status with `Z_BUF_ERROR` only when it
        // was `Z_STREAM_END`. A stream that already failed keeps its own status and
        // message, so the first failure is the one the caller sees.
        let outcome = drive(TOO_FAR_BACK, MAX_WBITS, None, true);
        assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
        assert_eq!(outcome.out_calls, 1, "the epilogue still tried to flush");
    }

    #[test]
    fn a_mid_stream_output_failure_stops_the_decode() {
        // Here the refusal happens inside `ROOM()` rather than in the epilogue,
        // because 630 bytes overflow a 256-byte window before the stream ends.
        let outcome = drive(REPEATED, MIN_WBITS, None, true);
        assert_eq!(outcome.code, ReturnCode::BUF_ERROR);
        assert_eq!(outcome.out_calls, 1, "the first flush already failed");
        assert!(outcome.avail_in.is_some(), "the input side did not fail");
    }

    #[test]
    fn inflate_back_never_reports_ok() {
        // `zlib.h` L1204: "Note that inflateBack() cannot return Z_OK." Every
        // fixture in this file, well formed or not, must respect that.
        let fixtures: [&[u8]; 6] = [
            HELLO_FIXED,
            HELLO_STORED,
            EMPTY_FIXED,
            TOO_FAR_BACK,
            &HELLO_FIXED[..2],
            &[],
        ];
        for fixture in fixtures {
            for refuse in [false, true] {
                let outcome = drive(fixture, MAX_WBITS, None, refuse);
                assert_ne!(outcome.code, ReturnCode::OK, "fixture {fixture:?}");
                assert_ne!(outcome.code, ReturnCode::NEED_DICT, "fixture {fixture:?}");
            }
        }
    }

    #[test]
    fn only_window_bits_eight_through_fifteen_are_accepted() {
        // `infback.c` L33-L35: a plain `windowBits < 8 || windowBits > 15`. None of
        // `inflateInit2_`'s conveniences apply -- no negative raw request, no +16
        // gzip request, no +32 automatic detection, no zero.
        for exponent in [MIN_WBITS, 9, 12, MAX_WBITS] {
            assert!(
                inflate_back_init(exponent, GlobalAllocator).is_ok(),
                "windowBits = {exponent} must be accepted"
            );
        }
        for exponent in [0, 1, 7, 16, 31, 32, 47, -1, -8, -15, i32::MIN, i32::MAX] {
            assert_eq!(
                inflate_back_init(exponent, GlobalAllocator).err(),
                Some(ReturnCode::STREAM_ERROR),
                "windowBits = {exponent} must be rejected"
            );
        }
    }

    #[test]
    fn a_window_of_the_wrong_size_is_rejected_by_the_call_that_supplies_it() {
        // The window is a per-call argument, so its extent is checked on the call
        // rather than at initialisation -- which is where a caller could get it wrong
        // more than once. `wsize` is the number it must match.
        let mut state = inflate_back_init(MIN_WBITS, GlobalAllocator).expect("valid exponent");

        for len in [0_usize, 1, 255] {
            let mut window = vec![0_u8; len];
            let mut source = Pieces::exhausted();
            let mut sink = Sink::default();
            let refused = inflate_back(
                &mut state,
                &mut window,
                Some(HELLO_FIXED),
                &mut source,
                &mut sink,
            );
            assert_eq!(
                refused.code,
                ReturnCode::STREAM_ERROR,
                "a {len}-byte window cannot serve windowBits = {MIN_WBITS}"
            );
            assert_eq!(sink.calls, 0, "the output callback is never reached");
        }

        // Exactly the requested size is accepted, and so is more than enough: C reads
        // `wsize` bytes and never learns the buffer's real length, so a generous caller
        // is not its problem either.
        for len in [256_usize, 257, 1024] {
            let mut window = vec![0_u8; len];
            let mut source = Pieces::exhausted();
            let mut sink = Sink::default();
            let accepted = inflate_back(
                &mut state,
                &mut window,
                Some(HELLO_FIXED),
                &mut source,
                &mut sink,
            );
            assert_eq!(
                accepted.code,
                ReturnCode::STREAM_END,
                "a {len}-byte window must serve windowBits = {MIN_WBITS}"
            );
            assert_eq!(sink.written, HELLO);
            assert!(
                window[256..].iter().all(|&byte| byte == 0),
                "bytes past 2**windowBits must be untouched"
            );
        }
        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_state_without_a_window_is_a_stream_error() {
        // C would proceed here with `put == NULL` and `left == 0` and spin inside
        // `ROOM()` forever. Refusing the call is `zlib.h` L1203's "the stream was
        // not properly initialized", and it terminates.
        let mut window = vec![0_u8; 1_usize << MIN_WBITS];
        let mut state = inflate_back_init(MIN_WBITS, GlobalAllocator).expect("valid exponent");
        // An `inflateBack` state carries no window of its own, so occupying the slot is
        // what makes it unusable: `inflate_back` refuses a state that already holds one,
        // because proceeding would mean two buffers both claiming to be "the window".
        state.window = crate::inflate::state::InflateWindow::borrowed(&mut window);

        let mut source = Pieces::exhausted();
        let mut sink = Sink::default();
        let mut output = vec![0_u8; 1_usize << MIN_WBITS];
        let result = inflate_back(
            &mut state,
            &mut output,
            Some(HELLO_FIXED),
            &mut source,
            &mut sink,
        );
        assert_eq!(result.code, ReturnCode::STREAM_ERROR);
        assert_eq!(result.msg, None);
        assert_eq!(sink.calls, 0);
        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn ending_a_state_reports_ok_and_releases_nothing_of_the_caller_s() {
        // `inflateBackEnd` frees the state, never the window: the caller owns that
        // buffer, and here the borrow simply ends. Reading the window afterwards
        // is proof that it survived.
        let window = vec![0x11_u8; 1_usize << MIN_WBITS];
        let mut state = inflate_back_init(MIN_WBITS, GlobalAllocator).expect("valid exponent");
        // ★ The state arrives by mutable reference, so its buffers are released in
        // place rather than in argument position; see `inflate_back_end`.
        assert_eq!(inflate_back_end(&mut state), ReturnCode::OK);

        assert!(window.iter().all(|&byte| byte == 0x11));
    }

    #[test]
    fn every_malformed_stream_reports_the_reference_message() {
        // Each stream below was fed to the C implementation, which reported exactly
        // the message beside it. The texts are part of the observable contract --
        // they land in `strm->msg`, and `test/infcover.c` prints them -- so they
        // are compared character for character.
        let cases: [(&str, &[u8], &str); 12] = [
            // `infback.c` L255-L257: BTYPE == 3.
            ("invalid block type", &[0x07], MSG_INVALID_BLOCK_TYPE),
            // L266-L270: LEN and NLEN are not complements.
            (
                "stored block length mismatch",
                &[0x01, 0x05, 0x00, 0x00, 0x00],
                MSG_INVALID_STORED_BLOCK_LENGTHS,
            ),
            // L303-L310: HLIT == 31, so nlen == 288 > 286.
            (
                "too many symbols",
                &[0xfd, 0x00, 0x00],
                MSG_TOO_MANY_SYMBOLS,
            ),
            // L327-L331: eight code-length codes of one bit is over-subscribed.
            (
                "over-subscribed code-length alphabet",
                &[0x05, 0x80, 0x92, 0x24, 0x49, 0x00],
                MSG_INVALID_CODE_LENGTHS_SET,
            ),
            // L350-L355: symbol 16 arrives with no previous length to repeat.
            (
                "repeat with nothing to copy",
                &[0x05, 0x00, 0x12, 0x00],
                MSG_INVALID_BIT_LENGTH_REPEAT,
            ),
            // L374-L379: two symbol-18 runs of 138 and 138 overshoot 258 entries.
            (
                "repeat overruns the alphabets",
                &[0x05, 0x00, 0x80, 0xe4, 0xff, 0x1f],
                MSG_INVALID_BIT_LENGTH_REPEAT,
            ),
            // L388-L394: every code length is zero, so lens[256] is too.
            (
                "no end-of-block code",
                &[0x05, 0x00, 0x80, 0xe4, 0x7f, 0x1b],
                MSG_MISSING_END_OF_BLOCK,
            ),
            // L404-L408: four literal/length codes of one bit is over-subscribed.
            (
                "over-subscribed literal/length alphabet",
                &[
                    0x05, 0xc0, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0xfc, 0x47, 0x03,
                ],
                MSG_INVALID_LITERAL_LENGTHS_SET,
            ),
            // L413-L417: three distance codes of one bit is over-subscribed.
            (
                "over-subscribed distance alphabet",
                &[
                    0x05, 0xc2, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0xff, 0xd5, 0x00,
                ],
                MSG_INVALID_DISTANCES_SET,
            ),
            // L469-L474: the fixed code's symbol 286 has no meaning.
            (
                "unmeaning literal/length code",
                &[0x1b, 0x03],
                MSG_INVALID_LITERAL_LENGTH_CODE,
            ),
            // L502-L506: the fixed distance code 30 has no meaning.
            (
                "unmeaning distance code",
                &[0x4b, 0x04, 0x3e],
                MSG_INVALID_DISTANCE_CODE,
            ),
            // L516-L521.
            (
                "distance past valid history",
                TOO_FAR_BACK,
                MSG_INVALID_DISTANCE_TOO_FAR_BACK,
            ),
        ];

        for (name, stream, expected) in cases {
            // Both window sizes, so that the slow path and the fast path are both
            // asked, and both input deliveries, so that a rejection cannot depend
            // on where a chunk boundary happened to fall.
            for exponent in [MIN_WBITS, MAX_WBITS] {
                for piece in [None, Some(1)] {
                    let outcome = drive(stream, exponent, piece, false);
                    assert_eq!(
                        outcome.code,
                        ReturnCode::DATA_ERROR,
                        "{name}: windowBits = {exponent}, piece = {piece:?}"
                    );
                    assert_eq!(
                        outcome.msg,
                        Some(expected),
                        "{name}: windowBits = {exponent}, piece = {piece:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_truncated_stream_is_a_buffer_error_at_every_length() {
        // Every proper prefix of a valid stream is incomplete, so every one of them
        // must ask for more input, be refused, and report `Z_BUF_ERROR` with a null
        // `next_in` -- never a data error, never a panic, never a hang.
        for stream in [HELLO_FIXED, HELLO_STORED, TEXT_DYNAMIC, STORED_THEN_FIXED] {
            for length in 0..stream.len() {
                let outcome = drive(&stream[..length], MAX_WBITS, None, false);
                assert_eq!(
                    outcome.code,
                    ReturnCode::BUF_ERROR,
                    "prefix of {length} bytes"
                );
                assert_eq!(outcome.avail_in, None, "prefix of {length} bytes");
            }
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic_and_always_terminate() {
        // A deterministic stand-in for the fuzz target: a linear congruential walk
        // over inputs of every length up to 64, decoded through the smallest window
        // so that the slow path carries all of it. Any outcome is acceptable except
        // a panic, a hang, or `Z_OK`.
        let mut seed: u32 = 0x1234_5678;
        for length in 0..64_usize {
            let mut stream = Vec::with_capacity(length);
            for _ in 0..length {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                stream.push(as_byte(seed >> 16));
            }
            for exponent in [MIN_WBITS, MAX_WBITS] {
                for piece in [None, Some(1), Some(7)] {
                    let outcome = drive(&stream, exponent, piece, false);
                    assert_ne!(outcome.code, ReturnCode::OK);
                    if outcome.code == ReturnCode::DATA_ERROR {
                        assert!(outcome.msg.is_some(), "a data error must say why");
                    }
                }
            }
        }
    }

    #[test]
    fn the_output_room_threshold_decodes_identically_on_either_side() {
        // `infback.c` L424 hands the decode to `inflate_fast` only while
        // `left >= 258`. With a 512-byte window, a stream of 254 literals leaves
        // `left == 258` when its match is reached and one of 255 leaves
        // `left == 257`, so the pair brackets the threshold exactly. Both must
        // produce the reference bytes.
        for literals in [253_u32, 254, 255, 256] {
            let mut block = FixedBlock::new();
            let mut expected = Vec::new();
            for index in 0..literals {
                let byte = as_byte(index * 11 + 5);
                block.literal(byte);
                expected.push(byte);
            }
            block.length(4);
            block.distance(4);
            let stream = block.finish();

            let tail = expected.len() - 4;
            let repeat: Vec<u8> = expected[tail..].to_vec();
            expected.extend_from_slice(&repeat);

            // Nine is the smallest window in which the fast path can run at all,
            // because 2**8 is below `MIN_AVAIL_OUT`.
            for exponent in [MIN_WBITS, 9, MAX_WBITS] {
                let outcome = drive(&stream, exponent, None, false);
                assert_eq!(
                    outcome.code,
                    ReturnCode::STREAM_END,
                    "{literals} literals, windowBits = {exponent}, msg = {:?}",
                    outcome.msg
                );
                assert_eq!(
                    outcome.output, expected,
                    "{literals} literals, windowBits = {exponent}"
                );
            }
        }
    }

    #[test]
    fn the_input_threshold_decodes_identically_on_either_side() {
        // A source that never yields six bytes at once keeps `have` below
        // `MIN_AVAIL_IN`, so the fast path cannot run however much window room
        // there is. Five and six bracket that threshold; the results must agree.
        let expected: Vec<u8> = SENTENCE.repeat(12);
        for piece in [5_usize, 6, 7] {
            let outcome = drive(TEXT_DYNAMIC, MAX_WBITS, Some(piece), false);
            assert_eq!(outcome.code, ReturnCode::STREAM_END, "piece = {piece}");
            assert_eq!(outcome.output, expected, "piece = {piece}");
        }
    }

    /// Takes one chunk through the trait, by value.
    ///
    /// Calling it with a `&mut Pieces` is what exercises the forwarding
    /// implementation rather than the inherent one.
    fn take_one<'i, I: InflateBackInput<'i>>(mut source: I) -> Option<&'i [u8]> {
        source.next_chunk()
    }

    /// Hands one run of bytes through the trait, by value, for the same reason.
    fn put_one<O: InflateBackOutput>(mut sink: O, data: &[u8]) -> Result<(), OutputFailure> {
        sink.write_out(data)
    }

    #[test]
    fn the_forwarding_implementations_reach_through_a_mutable_reference() {
        // The blanket implementations on `&mut T` are what let `drive` keep its
        // source and sink and inspect them afterwards; without them every caller
        // would have to surrender ownership.
        let data = b"payload";
        let mut source = Pieces::new(data, 3);
        let mut sink = Sink::default();

        assert_eq!(take_one(&mut source), Some(&data[..3]));
        assert_eq!(take_one(&mut source), Some(&data[3..6]));
        assert_eq!(source.calls, 2);

        assert_eq!(put_one(&mut sink, b"ab"), Ok(()));
        assert_eq!(sink.written, b"ab");

        sink.refuse = true;
        assert_eq!(put_one(&mut sink, b"cd"), Err(OutputFailure));
        assert_eq!(sink.written, b"ab", "a refused write records nothing");
    }

    #[test]
    fn a_result_carries_the_three_values_the_facade_installs() {
        // `InflateBackResult` exists so a facade has exactly what `infback.c`
        // L565-L569 writes into `z_stream`, and nothing more.
        let rest: &[u8] = b"xy";
        let result = InflateBackResult {
            code: ReturnCode::STREAM_END,
            next_in: Some(rest),
            msg: None,
        };
        assert_eq!(result.code.as_i32(), 1);
        assert_eq!(result.next_in.map(<[u8]>::len), Some(2));
        assert_eq!(result.msg, None);

        // `OutputFailure` is a plain marker: comparable, copyable, and carrying no
        // detail, because C's contract carries none either.
        let failure = OutputFailure;
        assert_eq!(Err::<(), OutputFailure>(failure), Err(OutputFailure));
    }
}
