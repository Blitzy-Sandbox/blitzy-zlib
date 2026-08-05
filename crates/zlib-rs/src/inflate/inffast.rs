//! The hot decode loop: the port of `inffast.c` L50-L305 (`inflate_fast`),
//! folded into the `inflate` module tree.
//!
//! The reference file states this module's importance in its own header comment
//! (`inffast.c` L15-L21):
//!
//! > Decode literal, length, and distance codes and write out the resulting
//! > literal and match bytes until either not enough input or output is
//! > available, an end-of-block is encountered, or a data error is encountered.
//! > When large enough input and output buffers are supplied to `inflate()`, for
//! > example a 16K input buffer and a 64K output buffer, more than 95% of the
//! > `inflate` execution time is spent in this routine.
//!
//! It is therefore both the performance centre of decompression and the most
//! attacker-exposed function in the library: every byte of every untrusted
//! DEFLATE stream that is large enough to be worth attacking passes through it.
//! Those two facts pull in opposite directions and shape every decision below.
//!
//! # Why this file exists as a separate module
//!
//! `inffast.c` is a distinct translation unit in C only so that hand-written
//! assembler could replace it (`inffast.c` L11-L13, and the note at
//! `infback.c` L7-L11 that "the interface with `inffast.c` is retained so that
//! optimized assembler-coded versions of `inflate_fast()` can be used with
//! either `inflate.c` or `infback.c`"). It shares its four private headers --
//! `zutil.h`, `inftrees.h`, `inflate.h`, `inffast.h` -- with `inflate.c`,
//! `inftrees.c` and `infback.c`, which is why it is folded into `inflate/`
//! here rather than kept at crate level.
//!
//! # Entry and exit contract (`inffast.c` L23-L36)
//!
//! The reference lists five entry *assumptions*. It does not check them, and
//! neither does anything in C's inner loop; that is precisely what makes the
//! loop fast.
//!
//! | Assumption | Reference |
//! |---|---|
//! | `state->mode == LEN` | `inffast.c` L25 |
//! | `strm->avail_in >= 6` | `inffast.c` L26 |
//! | `strm->avail_out >= 258` | `inffast.c` L27 |
//! | `start >= strm->avail_out` | `inffast.c` L28 |
//! | `state->bits < 8` | `inffast.c` L29 |
//!
//! On return the mode is exactly one of three states (`inffast.c` L31-L35):
//!
//! | Mode | Meaning |
//! |---|---|
//! | [`Mode::Len`] | ran out of enough output space or enough available input |
//! | [`Mode::Type`] | reached end-of-block code, `inflate()` to interpret next block |
//! | [`Mode::Bad`] | error in block data |
//!
//! [`Mode::Bad`] is what the driver turns into
//! [`crate::error::ReturnCode::DATA_ERROR`], together with the message this
//! function reports in [`FastExit::msg`].
//!
//! ## Why six and two hundred fifty-eight
//!
//! Reproduced from `inffast.c` L39-L48. A length/distance pair consumes at most
//! 15 bits for the length code, 5 for the length extra, 15 for the distance code
//! and 13 for the distance extra: 48 bits, or **six bytes**. With
//! `avail_in >= 6` the inner loop needs no input checks at all. Symmetrically a
//! single pair emits at most **258** bytes -- the largest codeable match length
//! -- so `avail_out >= 258` removes every output check.
//!
//! Those two numbers are the reason the loop condition at `inffast.c` L288 tests
//! sentinels five and 257 bytes short of the true ends rather than the ends
//! themselves: leaving that much slack is what preserves the guarantee for the
//! *next* iteration.
//!
//! ## Because they are assumptions, this port does not rely on them
//!
//! Every buffer access here is bounds-checked whether or not the contract holds,
//! and the contract is additionally `debug_assert`ed at entry. A violated
//! assumption produces a clean early return, never a panic and never an
//! out-of-bounds access. See [the no-panic section](#no-panics-ever) for the
//! full argument.
//!
//! # Six raw pointers become integer cursors
//!
//! C runs this entire function on raw pointers. The crate root carries
//! `#![forbid(unsafe_code)]`, so all six become `usize` indices into safe slices
//! (AAP §0.3.3.7):
//!
//! | `inffast.c` pointer | Meaning | Set up at | Here |
//! |---|---|---|---|
//! | `in` | input cursor | L79 | `in_index`, an index into `input` |
//! | `last` | "enough input while `in < last`" | L80 | `last`, an index into `input` |
//! | `out` | output cursor | L81 | `out_index`, an index into `output` |
//! | `beg` | `inflate()`'s initial `next_out` | L82 | `beg`, an index into `output` |
//! | `end` | "while `out < end`, enough space available" | L83 | `end`, an index into `output` |
//! | `from` | where to copy the match from | L75 | [`MatchSource`] |
//!
//! `from` is the awkward one, because it addresses **either** the sliding window
//! **or** output bytes this call has already written, and switches between them
//! mid-match. It becomes an explicit two-variant enum rather than one fused
//! cursor, so that which buffer is being read is never in doubt.
//!
//! ## `avail_in` and `avail_out` are derived, not stored
//!
//! In this port `input` and `output` are the caller's whole buffers and the two
//! cursors are indices into them, so
//! `avail_in == input.len() - in_index` and
//! `avail_out == output.len() - out_index` by construction. That is not an
//! approximation of what C does at `inffast.c` L299-L301; it is algebraically
//! the same value. With `last == input.len() - 5`:
//!
//! ```text
//! in <  last:  5 + (last - in)  = 5 + input.len() - 5 - in = input.len() - in
//! in >= last:  5 - (in - last)  = 5 - in + input.len() - 5 = input.len() - in
//! ```
//!
//! Both arms of C's conditional collapse to the same expression, and the same
//! holds for `out`/`end` with 257. [`inflate_fast`] still evaluates C's two
//! conditionals literally and returns the results in [`FastExit`], and a debug
//! assertion pins the identity so that a future change to the cursor model
//! cannot break it silently.
//!
//! # One variable named `op`, four different meanings
//!
//! `inffast.c` L71-L72 documents the reuse in the declaration itself: "code
//! bits, operation, extra bits, or window position, window bytes to copy". This
//! port keeps the single variable, because splitting it would make the code stop
//! reading like the source it must stay faithful to. Each reuse is commented at
//! the point it happens:
//!
//! | Role | Assigned at | Range |
//! |---|---|---|
//! | bits in this part of the code | L109, L140 | `0 ..= 15` |
//! | the entry's operation byte | L112, L143 | `0 ..= 255` |
//! | number of extra bits | L121, L146 | `0 ..= 15` |
//! | max distance in output, then window position and byte count | L167, L169, L210, L218 | up to `wsize` |
//!
//! # Compile-time knobs that are not ported
//!
//! This port implements the **default** configuration, which is the one the
//! shipped library is built with (AAP §0.6.2.1). Three reference constructs are
//! therefore deliberately absent:
//!
//! * `ASMINF` (`inffast.c` L11-L13) replaces this entire function with
//!   assembler. Out of scope by AAP §0.2.2.2.
//! * `INFLATE_STRICT` (`inffast.c` L57-L59, L84-L86, L156-L163) adds a
//!   `dist > dmax` rejection using the window size declared in the zlib header.
//!   It is not defined in the shipped build. `InflateState` keeps its `dmax`
//!   field for fidelity, and nothing here reads it.
//! * `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` (`inffast.c` L177-L195) turns
//!   a too-far-back distance into a run of zero bytes instead of an error. It is
//!   `#ifdef`-gated out, and in the shipped build `inflateUndermine` cannot even
//!   reach it: it forces `sane` back to true and returns `Z_DATA_ERROR`
//!   (`inflate.c` L1376-L1382), which `test/infcover.c` L440 asserts. The
//!   `sane` test itself is kept, so the decision point stays where the reference
//!   put it; its false arm is unreachable.
//!
//! # `whave` is the load-bearing security guard
//!
//! The check at `inffast.c` L170-L176 -- reject when the distance reaches
//! further back than `whave` says is valid -- is the only thing standing between
//! a malformed stream and the contents of memory the decoder never wrote.
//!
//! This is not theoretical. The commit at the head of this branch, `09a1572`
//! ("Fix `inflateBack()` bug that would fail to detect a too far back"), deletes
//! exactly two lines from `infback.c`:
//!
//! ```text
//! -                if (state->whave < state->wsize)
//! -                    state->whave = state->wsize - left;
//! ```
//!
//! Its message records the consequence: the bug "would pass off an invalid
//! deflate stream as good, and copy uninitialized memory contents to the
//! output." So:
//!
//! * `whave` is read **once**, at entry, exactly as `inffast.c` L88 reads it,
//!   and is never widened, recomputed or relaxed anywhere in this file;
//! * the readable history is additionally *clamped* to `window[..whave]`, which
//!   makes emitting an unwritten window byte structurally impossible rather than
//!   merely ruled out by argument.
//!
//! The clamp changes no accept/reject decision on any reachable input. Given the
//! window invariant that `updatewindow` (`inflate.c` L246-L280),
//! `inflateSetDictionary` and `inflateBack`'s `ROOM()` (`infback.c` L149-L161)
//! all maintain -- `whave == wnext` before the window first wraps, and
//! `whave == wsize` after -- every index the three window cases below can
//! produce already lies in `0 .. whave`:
//!
//! | Case | Indices read | Requires | Holds because |
//! |---|---|---|---|
//! | `wnext == 0` | `wsize - op ..= wsize - 1` | `whave == wsize` | `wnext == 0` with `whave > 0` implies the window wrapped |
//! | `wnext < op` | `wsize + wnext - op ..= wsize - 1`, then `0 ..= wnext - 1` | `whave == wsize` | `op <= whave` and `op > wnext` rule out the pre-wrap `whave == wnext` |
//! | `wnext >= op` | `wnext - op ..= wnext - 1` | `whave >= wnext` | true in both phases |
//!
//! # Matches copy forward, one byte at a time
//!
//! A match distance may be **smaller** than its length: `dist == 1` with
//! `len == 258` is a legal, common and cheap way to encode a run of 258 equal
//! bytes. The copy is therefore *intentionally self-overlapping* and must read
//! bytes it has only just written. `copy_from_slice`, `slice::copy_within`,
//! `memcpy` or any vectorised bulk move would read stale bytes and produce
//! different output, so [`copy_match`] reads and writes strictly one byte at a
//! time in forward order. The three-at-a-time unrolling of `inffast.c`
//! L237-L242 and L251-L256 is byte-for-byte equivalent and is kept.
//!
//! # No panics, ever
//!
//! An abort inside a C caller's process is not an acceptable response to a
//! malformed byte, so this file has no panicking path at all:
//!
//! * every buffer read is `slice::get`, every write `slice::get_mut`;
//! * every subtraction that could go negative uses a saturating form;
//! * every shift goes through [`low_mask`] or [`drop_bits`], which cannot shift
//!   by more than the width of the accumulator;
//! * a read or write that fails despite the entry contract is reported as
//!   [`Mode::Bad`] rather than ignored, so it can never be mistaken for success.
//!
//! Those fallbacks are unreachable when the contract holds, which is why each is
//! commented as such at its site. They exist so that the absence of a panic is a
//! structural property of the code rather than a conclusion drawn from the
//! surrounding argument.
//!
//! # The loop always terminates
//!
//! Three nested loops, three separate arguments:
//!
//! * The **outer** loop (`inffast.c` L100-L288) makes progress on every
//!   iteration: each one either writes at least one output byte or leaves via
//!   `break`, and it re-tests `in_index < last && out_index < end`, both of
//!   which are monotone.
//! * The **`dolen` / `dodist`** back-edges (`inffast.c` L276 and L266) follow a
//!   table link, and a link always drops the current entry's `bits` from the
//!   accumulator before the next lookup. Since `inflate_table` builds at most a
//!   root table plus one sub-table level, and `MAXBITS` is 15, at most two hops
//!   occur; a corrupt table cannot loop forever either, because `bits` is
//!   saturating and a zero-`bits` entry that keeps linking would immediately
//!   exhaust the accumulator and land on a bounds-checked lookup failure.
//! * The **copy drains** (`inffast.c` L237-L242, L251-L256) decrement `len` by
//!   three per iteration with a saturating subtraction, so `len` reaches its
//!   exit condition even if a copy fails.
//!
//! # Speedups that turned out slower
//!
//! Transcribed from `inffast.c` L307-L318, which records what Mark Adler
//! measured on a PowerPC G3 750CXe. None of these is attempted here, and none
//! should be attempted without measurement:
//!
//! * using bit fields for the code structure;
//! * a different `op` definition to avoid `&` for extra bits, doing the `&` for
//!   table bits instead;
//! * three separate decoding `do`-loops for direct, window and `wnext == 0`;
//! * a special case for distance greater than one, to do an overlapped load and
//!   store copy;
//! * explicit branch predictions based on measured branch probabilities;
//! * deferring the match copy and interspersing it with decoding subsequent
//!   codes;
//! * swapping the literal/length `else`;
//! * swapping the window/direct `else`;
//! * larger unrolled copy loops (three is about right);
//! * moving the `len -= 3` statement into the middle of the loop.
//!
//! SIMD is prohibited in this file for a different reason: AAP §0.8.2 confines
//! the `simd` feature to CRC-32 and Adler-32, because those produce a single
//! scalar result and so cannot perturb the bitstream. Vectorising match copying
//! or code decoding could change emitted bytes, which the byte-identity
//! requirement forbids outright.
//!
//! # Visibility
//!
//! `zlib.map`'s `ZLIB_1.2.0` `local:` block lists `inflate_fast`, so it is
//! hidden from the shared library's dynamic symbol table. [`inflate_fast`] is
//! therefore `pub(crate)`, never `pub`, and carries no `#[no_mangle]`,
//! `#[repr(C)]` or `extern "C"`. `pub(crate)` is exactly the reachability its
//! two callers need: `crate::inflate` for `inflate()` and `crate::infback` for
//! `inflateBack()`.

// Two conventions in this module are inherited from the reference rather than
// chosen, and neither may be "tidied" without breaking the verbatim
// correspondence with `inffast.c` L100-L288 that behavioural fidelity depends
// on. The branch cascades are in the reference's order, which is ordered for
// speed and not for readability, so several arms end in a diverging statement
// and are still followed by an `else`. And `lcode`/`dcode` and `lmask`/`dmask`
// are the reference's own names for the two code tables and their root masks
// (`inffast.c` L66-L69), so they stay one character apart. No `#[allow]` is
// needed for either -- both are lint-clean at `clippy::all` and
// `clippy::pedantic` -- but a future edit that provokes one of those lints
// should be reconsidered rather than silenced.

// The `Allocator` bound is not a dependency of this module's own logic: it is
// required to *name* `InflateState`, whose allocator parameter stands in for C's
// `z_stream` back-pointer (`inflate.h` L83). Nothing here allocates.
use crate::allocate::Allocator;
use crate::inflate::inftrees::Code;
use crate::inflate::mode::Mode;
use crate::inflate::state::InflateState;

// -----------------------------------------------------------------------------
//  Contract constants
// -----------------------------------------------------------------------------

/// Smallest `avail_in` this function may be entered with (`inffast.c` L26).
///
/// Six bytes hold the 48 bits of the longest possible length/distance pair, so
/// the inner loop needs no input availability checks (`inffast.c` L39-L43).
pub(crate) const MIN_AVAIL_IN: usize = 6;

/// Smallest `avail_out` this function may be entered with (`inffast.c` L27).
///
/// 258 is the largest match length RFC 1951 can code, so one iteration can never
/// overrun the output buffer (`inffast.c` L45-L48).
pub(crate) const MIN_AVAIL_OUT: usize = 258;

/// Input slack the loop condition keeps in reserve: `MIN_AVAIL_IN - 1`.
///
/// `last = in + (avail_in - 5)` at `inffast.c` L80, and the same five reappears
/// in the `avail_in` recomputation at L299.
const IN_SLACK: usize = MIN_AVAIL_IN - 1;

/// Output slack the loop condition keeps in reserve: `MIN_AVAIL_OUT - 1`.
///
/// `end = out + (avail_out - 257)` at `inffast.c` L83, and the same 257
/// reappears in the `avail_out` recomputation at L300-L301.
const OUT_SLACK: usize = MIN_AVAIL_OUT - 1;

/// Bit-count threshold for the two-byte refill (`inffast.c` L101 and L132).
///
/// Fifteen is the longest Huffman code RFC 1951 permits, so once the accumulator
/// holds fifteen bits a whole code is guaranteed to be present.
const REFILL_BITS: u32 = 15;

/// Bytes copied per iteration of the unrolled drains (`inffast.c` L237-L242).
const UNROLL: u32 = 3;

// -----------------------------------------------------------------------------
//  Error messages
// -----------------------------------------------------------------------------
//
// These are the exact strings the reference stores in `z_stream.msg`, and
// `test/infcover.c` prints them, so they are reproduced character for character
// and at the same decision points. `inflate.c`'s slow path sets the same three
// texts at L957, L995 and L1027-L1028; they are `pub(crate)` here so that the
// driver can share these definitions instead of repeating the literals.

/// `strm->msg` for a distance entry the table marks invalid
/// (`inffast.c` L269, and `inflate.c` L995 in the slow path).
pub(crate) const MSG_INVALID_DISTANCE_CODE: &str = "invalid distance code";

/// `strm->msg` for a literal/length entry the table marks invalid
/// (`inffast.c` L284, and `inflate.c` L957 in the slow path).
pub(crate) const MSG_INVALID_LITERAL_LENGTH_CODE: &str = "invalid literal/length code";

/// `strm->msg` for a distance that reaches back beyond the valid window
/// (`inffast.c` L172-L174, and `inflate.c` L1027-L1028 in the slow path).
pub(crate) const MSG_INVALID_DISTANCE_TOO_FAR_BACK: &str = "invalid distance too far back";

// -----------------------------------------------------------------------------
//  Small total helpers
// -----------------------------------------------------------------------------

/// Narrows a 64-bit accumulator value to a slice index.
///
/// The saturating fallback is unreachable on every target this crate supports,
/// where `usize` is at least 32 bits and every value passed here is a masked
/// field of at most fifteen bits or a count of at most `wsize`. Saturating to
/// `usize::MAX` rather than truncating means that if it ever were reached, the
/// result would fail a bounds check instead of silently addressing the wrong
/// byte.
#[inline]
fn to_index(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The empty history slice, used when no sliding window is addressable.
///
/// This is the safe reading of `state->window == Z_NULL`. Every index into it
/// fails its bounds check, which is the correct outcome: with no window there is
/// no valid history, and the `op > whave` guard rejects such a distance before a
/// read is even attempted.
const NO_HISTORY: &[u8] = &[];

/// Narrows a masked accumulator field to the `unsigned` width C uses for `len`
/// and `dist`.
///
/// Every call site masks with [`low_mask`] first, so the value is at most
/// fifteen bits wide and the saturating fallback is unreachable.
#[inline]
fn to_extra(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Narrows a slice index or index difference to the `unsigned` width C uses for
/// `op`, `wsize`, `whave` and `wnext`.
///
/// `avail_out` is a `uInt` in the public API, so `out - beg` cannot exceed
/// `u32::MAX` for any stream reachable through the C entry points. Saturating
/// rather than truncating keeps a hypothetical larger Rust-side buffer correct:
/// a saturated `op` is still greater than any codeable `dist`, which selects the
/// copy-direct-from-output path -- the right answer when the output already holds
/// more than four gibibytes.
#[inline]
fn to_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// `(1 << n) - 1`: the mask C writes as `(1U << op) - 1`
/// (`inffast.c` L127, L155, L265, L275, L294) and as
/// `(1U << state->lenbits) - 1` (L95-L96).
///
/// A shift width of 64 or more would be undefined in C and would panic in a
/// debug build here; every call site passes a value bounded by `MAXBITS` (15) or
/// by `bits & 7`, so the `None` arm is unreachable. It yields an all-ones mask,
/// the limit of the sequence, which keeps the fallback from inventing a
/// narrower mask than the caller asked for.
#[inline]
const fn low_mask(n: u32) -> u64 {
    match 1_u64.checked_shl(n) {
        Some(bit) => bit - 1,
        None => u64::MAX,
    }
}

/// `hold >>= n`, yielding zero instead of panicking on an out-of-range shift.
///
/// Zero is the correct limit: shifting a 64-bit accumulator right by 64 or more
/// discards every bit. Reachable widths come from a table entry's `bits` field
/// or from `op & 15`, both at most fifteen.
#[inline]
const fn drop_bits(hold: u64, n: u32) -> u64 {
    match hold.checked_shr(n) {
        Some(shifted) => shifted,
        None => 0,
    }
}

// -----------------------------------------------------------------------------
//  The match source
// -----------------------------------------------------------------------------

/// Where the next byte of a match copy comes from: the two things C's `from`
/// pointer can address (`inffast.c` L75, "where to copy match from").
///
/// C fuses both into one `unsigned char *` and switches it between the window
/// and the output buffer four times inside a single match (`inffast.c` L197,
/// L205, L216, L223, L234, L250). Safe Rust cannot fuse two distinct
/// allocations into one cursor, and naming the two cases is clearer anyway:
/// which buffer is being read is decided once, at the site the reference assigns
/// the pointer, and is then carried in the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchSource {
    /// An index into the sliding window.
    Window(usize),
    /// An index into output bytes this call, or an earlier one, already wrote.
    Output(usize),
}

// -----------------------------------------------------------------------------
//  The return value
// -----------------------------------------------------------------------------

/// Everything C writes back through `z_stream` at `inffast.c` L296-L303 that is
/// not already reachable through the state or the cursors.
///
/// C is a `void` function that reaches its caller's stream through the
/// `z_streamp` it was handed. This port has no stream: the cursors are `&mut`
/// parameters, `hold`, `bits` and `mode` are written into the
/// [`InflateState`], and the remaining three values -- the resulting mode, the
/// message and the two recomputed availabilities -- come back here.
///
/// A caller that only needs the mode can read [`InflateState::mode`] instead and
/// discard this value; nothing is marked `#[must_use]`, so ignoring it is not an
/// error. Discarding it does lose [`FastExit::msg`], which is the only channel
/// by which the three data-error texts reach `z_stream.msg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FastExit {
    /// The mode written into the state: [`Mode::Len`], [`Mode::Type`] or
    /// [`Mode::Bad`] (`inffast.c` L31-L35).
    pub(crate) mode: Mode,

    /// The text C assigns to `strm->msg`, or [`None`] when no error occurred.
    ///
    /// Always one of [`MSG_INVALID_DISTANCE_CODE`],
    /// [`MSG_INVALID_LITERAL_LENGTH_CODE`] or
    /// [`MSG_INVALID_DISTANCE_TOO_FAR_BACK`], and always [`Some`] exactly when
    /// `mode` is [`Mode::Bad`].
    pub(crate) msg: Option<&'static str>,

    /// `strm->avail_in` as recomputed at `inffast.c` L299.
    pub(crate) avail_in: usize,

    /// `strm->avail_out` as recomputed at `inffast.c` L300-L301.
    pub(crate) avail_out: usize,
}

// -----------------------------------------------------------------------------
//  The match copy
// -----------------------------------------------------------------------------

/// Copies `len` match bytes forward, one byte at a time, and advances both
/// cursors.
///
/// This is the body shared by all six copy loops of `inffast.c`
/// (L202-L204, L213-L215, L220-L222, L231-L233, L237-L242, L251-L256). Returns
/// `false` without completing if either range leaves its buffer, which the entry
/// contract makes unreachable; see the module's no-panic section.
///
/// # Byte-at-a-time is not a simplification
///
/// When the source is [`MatchSource::Output`] the ranges may overlap, and are
/// *meant* to: a match with `dist < len` encodes a repeating run, and each byte
/// after the first `dist` of them is read back from what this loop just wrote.
/// The read and the write of one byte are therefore strictly ordered, and no
/// bulk copy may be substituted.
///
/// Ranges are validated once per segment rather than once per byte. That is the
/// segment-level form of the per-byte gate `InflateState::valid_window_byte`
/// applies, and it is what keeps a bounds check off the hot path without letting
/// an out-of-range index through.
#[inline]
fn copy_match(
    output: &mut [u8],
    out: &mut usize,
    history: &[u8],
    from: &mut MatchSource,
    len: u32,
) -> bool {
    let count = to_index(u64::from(len));
    let dst = *out;

    // The destination range is shared by both source cases.
    let Some(dst_end) = dst.checked_add(count) else {
        return false;
    };
    if dst_end > output.len() {
        return false;
    }

    match *from {
        MatchSource::Window(src) => {
            let Some(src_end) = src.checked_add(count) else {
                return false;
            };
            if src_end > history.len() {
                return false;
            }
            // `offset` walks both ranges together; both `src + offset` and
            // `dst + offset` are bounded by the ends checked above, so neither
            // addition can overflow and neither `get` can fail.
            let mut offset = 0;
            while offset < count {
                let Some(&byte) = history.get(src + offset) else {
                    return false;
                };
                let Some(slot) = output.get_mut(dst + offset) else {
                    return false;
                };
                *slot = byte;
                offset += 1;
            }
            *from = MatchSource::Window(src_end);
        }
        MatchSource::Output(src) => {
            let Some(src_end) = src.checked_add(count) else {
                return false;
            };
            if src_end > output.len() {
                return false;
            }
            // Read then write, one byte per iteration, in ascending order: this
            // is what makes a self-overlapping run reproduce the reference's
            // bytes rather than stale ones.
            let mut offset = 0;
            while offset < count {
                let Some(&byte) = output.get(src + offset) else {
                    return false;
                };
                let Some(slot) = output.get_mut(dst + offset) else {
                    return false;
                };
                *slot = byte;
                offset += 1;
            }
            *from = MatchSource::Output(src_end);
        }
    }

    *out = dst_end;
    true
}

/// The match source for window index `index`.
///
/// This is C's `from = window + <offset>` (`inffast.c` L197 with L199, L209 or
/// L228), with one wrinkle that has no C counterpart. `inflateBack()`'s window
/// **is** its output buffer: `infback.c` L143-L152 sets `put = state->window`
/// and `left = state->wsize`, so both of C's pointers address the same bytes.
/// Safe Rust cannot hold two live references to one buffer, so a caller in that
/// position hands the window over as `output` and leaves the state's own window
/// slot empty. `window_is_output` records that, and the same index then names the
/// same byte -- `beg` is the window's base there, so window offsets and output
/// offsets coincide.
///
/// For `inflate()` the flag is always `false` when it matters: an absent window
/// means `whave == 0`, and `op > whave` rejects the distance before any read.
#[inline]
fn window_source(index: u32, window_is_output: bool) -> MatchSource {
    let index = to_index(u64::from(index));
    if window_is_output {
        MatchSource::Output(index)
    } else {
        MatchSource::Window(index)
    }
}

/// C's `from = out - dist`, "copy direct from output" (`inffast.c` L205, L223,
/// L234 and L250).
///
/// The subtraction cannot go negative on any reachable path: in the direct case
/// `dist <= out - beg`, and in the window cases the preceding segment copies
/// advance `out` to exactly `beg + dist`. It saturates anyway, so that a
/// violated invariant produces a bounds-check failure rather than a wrapped
/// index.
#[inline]
fn output_source(out: usize, dist: u32) -> MatchSource {
    MatchSource::Output(out.saturating_sub(to_index(u64::from(dist))))
}

// -----------------------------------------------------------------------------
//  inflate_fast
// -----------------------------------------------------------------------------

/// Decodes literal, length and distance codes and writes out the resulting
/// literal and match bytes until input runs short, output runs short, an
/// end-of-block code is reached, or the block data is found to be invalid.
///
/// The port of `inflate_fast` (`inffast.c` L50-L305), whose prototype is the
/// single line of `inffast.h` (its L11):
///
/// ```c
/// void ZLIB_INTERNAL inflate_fast(z_streamp strm, unsigned start);
/// ```
///
/// C reaches the input, the output, the cursors and the state through one
/// `z_streamp`. This port takes them apart, because the safe core has no
/// `z_stream`: raw pointers stop at the facade (AAP §0.6.1), and the two
/// pointer/length pairs arrive here as slices with separate cursors.
///
/// # Parameters
///
/// * `state` -- the decoder state. Read at entry for the register set
///   (`inffast.c` L87-L96); `hold`, `bits` and `mode` are written back at exit
///   (L302-L303 and the three `state->mode` assignments).
/// * `input` -- the whole input buffer, i.e. what `z_stream.next_in` addressed
///   when the enclosing call began. `avail_in` is `input.len() - *next_in`.
/// * `next_in` -- cursor into `input`; C's `strm->next_in` (L79, L297).
/// * `output` -- the whole output buffer. `avail_out` is
///   `output.len() - *next_out`.
/// * `next_out` -- cursor into `output`; C's `strm->next_out` (L81, L298).
/// * `start` -- the enclosing call's *starting* `avail_out`, used only to
///   reconstruct `beg` (L82). The two callers pass different things and both are
///   correct:
///
/// | Caller | `start` | Reference |
/// |---|---|---|
/// | `inflate()` | `out`, the `avail_out` the call began with | `inflate.c` L915 |
/// | `inflateBack()` | `state->wsize`, because its output buffer is the window | `infback.c` L425 |
///
/// # Entry assumptions
///
/// `state.mode == Mode::Len`, `avail_in >= 6`, `avail_out >= 258`,
/// `start >= avail_out` and `state.bits < 8` (`inffast.c` L23-L29). They are
/// `debug_assert`ed and, for the two that bound buffer arithmetic, checked: a
/// violated contract returns immediately with the state untouched, which is the
/// [`Mode::Len`] "not enough input or output" outcome. Nothing here relies on the
/// assumptions for memory safety.
///
/// # Returns
///
/// A [`FastExit`] carrying the resulting mode, the data-error message if any, and
/// `avail_in`/`avail_out` as C recomputes them at L299-L301. `state.mode` is also
/// written, so a caller that only needs the mode may discard the return value.
///
/// [`Mode::Type`] means an end-of-block code was consumed. Note that
/// `state.back = -1` is then set by the *caller*, not here (`inflate.c` L917).
pub(crate) fn inflate_fast<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
    output: &mut [u8],
    next_out: &mut usize,
    start: usize,
) -> FastExit {
    // -------------------------------------------------------------------------
    //  Entry contract (`inffast.c` L23-L29)
    // -------------------------------------------------------------------------
    debug_assert_eq!(
        state.mode,
        Mode::Len,
        "inffast.c L25 requires state->mode == LEN on entry"
    );
    debug_assert!(
        state.bits < 8,
        "inffast.c L29 requires state->bits < 8 on entry"
    );

    let avail_in = input.len().saturating_sub(*next_in);
    let avail_out = output.len().saturating_sub(*next_out);
    debug_assert!(
        avail_in >= MIN_AVAIL_IN,
        "inffast.c L26 requires avail_in >= 6 on entry"
    );
    debug_assert!(
        avail_out >= MIN_AVAIL_OUT,
        "inffast.c L27 requires avail_out >= 258 on entry"
    );
    debug_assert!(
        start >= avail_out,
        "inffast.c L28 requires start >= avail_out on entry"
    );

    if avail_in < MIN_AVAIL_IN || avail_out < MIN_AVAIL_OUT {
        // The contract does not hold, so the sentinels below cannot be formed.
        // Nothing has been decoded and nothing is written back; reporting the
        // unchanged mode is the reference's own "ran out of enough output space
        // or enough available input" outcome (`inffast.c` L33).
        return FastExit {
            mode: state.mode,
            msg: None,
            avail_in,
            avail_out,
        };
    }

    // -------------------------------------------------------------------------
    //  Copy state to local variables (`inffast.c` L77-L96)
    // -------------------------------------------------------------------------
    let mut in_index = *next_in; // `in`   -- L79
    let last = in_index + (avail_in - IN_SLACK); // `last` -- L80
    let mut out_index = *next_out; // `out`  -- L81
    let beg = out_index.saturating_sub(start.saturating_sub(avail_out)); // `beg` -- L82
    let end = out_index + (avail_out - OUT_SLACK); // `end`  -- L83

    // L84-L86 read `dmax` under INFLATE_STRICT; not ported.

    let wsize = state.wsize; // L87
    let whave = state.whave; // L88
    let wnext = state.wnext; // L89
    let sane = state.sane; // used at L171
    let mut hold = state.hold; // L91
    let mut bits = state.bits; // L92
    let lmask = low_mask(to_count(state.tables.lenbits)); // L95
    let dmask = low_mask(to_count(state.tables.distbits)); // L96
    let lcode: &[Code] = state.lencode(); // L93
    let dcode: &[Code] = state.distcode(); // L94

    // `window` -- L90. The readable history is clamped to `window[..whave]`; see
    // the module's `whave` section for why that is exactly C's behaviour on every
    // reachable input and structurally safer on the rest. `window_is_output`
    // handles `inflateBack()`, whose window and output buffer are one and the
    // same; see `window_source`.
    let (history, window_is_output) = match state.window.as_slice() {
        Some(window) => {
            let valid = to_index(u64::from(whave.min(wsize))).min(window.len());
            (window.get(..valid).unwrap_or(NO_HISTORY), false)
        }
        None => (NO_HISTORY, true),
    };

    // The outcome, accumulated in locals and written back once at the end. C
    // assigns `state->mode` and `strm->msg` in place at each break; nothing reads
    // either between the assignment and the return, so deferring both is exact.
    let mut mode = Mode::Len;
    let mut msg: Option<&'static str> = None;

    // -------------------------------------------------------------------------
    //  Decode literals and length/distances until end-of-block or not enough
    //  input data or output space (`inffast.c` L98-L288)
    // -------------------------------------------------------------------------
    'outer: loop {
        // L101-L106. Two unconditional byte pushes rather than a loop: with
        // `avail_in >= 6` guaranteed, a `while` would test a condition that
        // cannot fail and would hide the invariant. `bits < 15` bounds both
        // shift widths to at most 22, so neither can overflow the accumulator.
        if bits < REFILL_BITS {
            let Some(&byte) = input.get(in_index) else {
                // Unreachable: `in_index < last` leaves at least six bytes.
                break 'outer;
            };
            hold = hold.wrapping_add(u64::from(byte) << bits);
            in_index += 1;
            bits += 8;
            let Some(&byte) = input.get(in_index) else {
                break 'outer;
            };
            hold = hold.wrapping_add(u64::from(byte) << bits);
            in_index += 1;
            bits += 8;
        }

        // L107
        let Some(&entry) = lcode.get(to_index(hold & lmask)) else {
            // Unreachable: `lmask` is `(1 << lenbits) - 1` and `inflate_table`
            // always fills a complete `1 << lenbits` root table.
            msg = Some(MSG_INVALID_LITERAL_LENGTH_CODE);
            mode = Mode::Bad;
            break 'outer;
        };
        let mut here: Code = entry;

        // C's `dolen:` label (L108). Each `continue` here is one of its `goto`s
        // (L276), taken only after a table link has dropped that entry's bits.
        'dolen: loop {
            // L109-L111: `op` = bits in this part of the code.
            let mut op = u32::from(here.bits);
            hold = drop_bits(hold, op);
            bits = bits.saturating_sub(op);
            // L112: `op` = the entry's operation byte.
            op = u32::from(here.op);
            if op == 0 {
                // literal -- L113-L118. `(unsigned char)(here->val)` keeps the
                // low byte only; destructuring `to_le_bytes` performs exactly
                // that truncation without a lossy cast.
                let [literal, _] = here.val.to_le_bytes();
                let Some(slot) = output.get_mut(out_index) else {
                    // Unreachable: `out_index < end` leaves at least 258 bytes.
                    break 'outer;
                };
                *slot = literal;
                out_index += 1;
            } else if op & 16 != 0 {
                // length base -- L119-L121
                let mut len = u32::from(here.val);
                op &= 15; // `op` = number of extra bits
                if op != 0 {
                    // L122-L130: one conditional single-byte push.
                    if bits < op {
                        let Some(&byte) = input.get(in_index) else {
                            break 'outer;
                        };
                        hold = hold.wrapping_add(u64::from(byte) << bits);
                        in_index += 1;
                        bits += 8;
                    }
                    len = len.wrapping_add(to_extra(hold & low_mask(op)));
                    hold = drop_bits(hold, op);
                    bits = bits.saturating_sub(op);
                }
                // L132-L137
                if bits < REFILL_BITS {
                    let Some(&byte) = input.get(in_index) else {
                        break 'outer;
                    };
                    hold = hold.wrapping_add(u64::from(byte) << bits);
                    in_index += 1;
                    bits += 8;
                    let Some(&byte) = input.get(in_index) else {
                        break 'outer;
                    };
                    hold = hold.wrapping_add(u64::from(byte) << bits);
                    in_index += 1;
                    bits += 8;
                }
                // L138
                let Some(&entry) = dcode.get(to_index(hold & dmask)) else {
                    // Unreachable, as for the length root table above.
                    msg = Some(MSG_INVALID_DISTANCE_CODE);
                    mode = Mode::Bad;
                    break 'outer;
                };
                here = entry;

                // C's `dodist:` label (L139).
                'dodist: loop {
                    // L140-L142: `op` = bits in this part of the code.
                    op = u32::from(here.bits);
                    hold = drop_bits(hold, op);
                    bits = bits.saturating_sub(op);
                    // L143: `op` = the entry's operation byte.
                    op = u32::from(here.op);
                    if op & 16 != 0 {
                        // distance base -- L144-L146
                        let mut dist = u32::from(here.val);
                        op &= 15; // `op` = number of extra bits

                        // L147-L154: two conditional single-byte pushes, nested
                        // rather than looped, because thirteen extra bits can
                        // need two whole bytes.
                        if bits < op {
                            let Some(&byte) = input.get(in_index) else {
                                break 'outer;
                            };
                            hold = hold.wrapping_add(u64::from(byte) << bits);
                            in_index += 1;
                            bits += 8;
                            if bits < op {
                                let Some(&byte) = input.get(in_index) else {
                                    break 'outer;
                                };
                                hold = hold.wrapping_add(u64::from(byte) << bits);
                                in_index += 1;
                                bits += 8;
                            }
                        }
                        dist = dist.wrapping_add(to_extra(hold & low_mask(op))); // L155

                        // L156-L163 rejects `dist > dmax` under INFLATE_STRICT;
                        // not defined in the shipped build, so not ported.
                        hold = drop_bits(hold, op); // L164
                        bits = bits.saturating_sub(op); // L165

                        // L167: `op` = max distance available in the output.
                        op = to_count(out_index.saturating_sub(beg));

                        // Whether every segment copy below succeeded. They can
                        // only fail if the entry contract was violated, and a
                        // failure must not be mistaken for success, so it is
                        // accumulated and reported as a data error once. Every
                        // loop below still terminates, because `len` decreases
                        // monotonically regardless.
                        let mut copied = true;

                        if dist > op {
                            // see if copy from window -- L168
                            op = dist - op; // L169: distance back in window

                            // L170-L176. The two levels are one test here
                            // because the `!sane` arm's body -- the
                            // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR block
                            // at L177-L195 -- is `#ifdef`-gated out of the
                            // shipped build and is not ported, so both spellings
                            // fall through identically. `sane` is nonetheless
                            // kept: it is the reference's decision point, and
                            // `inflateUndermine` cannot clear it here
                            // (`inflate.c` L1376-L1382).
                            //
                            // This is the guard commit 09a1572 exists to
                            // protect. `whave` was read once at L88 and is never
                            // widened; see the module's `whave` section.
                            if op > whave && sane {
                                msg = Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK);
                                mode = Mode::Bad;
                                break 'outer;
                            }

                            // `from = window` -- L197. All three cases are kept
                            // separate and in the reference's order: the index
                            // arithmetic differs in each, and the wrap case has a
                            // second segment the others do not.
                            let mut from: MatchSource;
                            if wnext == 0 {
                                // very common case -- L198-L207
                                from = window_source(wsize.saturating_sub(op), window_is_output);
                                if op < len {
                                    // some from window
                                    len -= op;
                                    copied &=
                                        copy_match(output, &mut out_index, history, &mut from, op);
                                    // rest from output
                                    from = output_source(out_index, dist);
                                }
                            } else if wnext < op {
                                // wrap around window -- L208-L226
                                from = window_source(
                                    wsize.saturating_add(wnext).saturating_sub(op),
                                    window_is_output,
                                );
                                op -= wnext;
                                if op < len {
                                    // some from end of window
                                    len -= op;
                                    copied &=
                                        copy_match(output, &mut out_index, history, &mut from, op);
                                    from = window_source(0, window_is_output);
                                    if wnext < len {
                                        // some from start of window
                                        op = wnext;
                                        len -= op;
                                        copied &= copy_match(
                                            output,
                                            &mut out_index,
                                            history,
                                            &mut from,
                                            op,
                                        );
                                        // rest from output
                                        from = output_source(out_index, dist);
                                    }
                                }
                            } else {
                                // contiguous in window -- L227-L236
                                from = window_source(wnext.saturating_sub(op), window_is_output);
                                if op < len {
                                    // some from window
                                    len -= op;
                                    copied &=
                                        copy_match(output, &mut out_index, history, &mut from, op);
                                    // rest from output
                                    from = output_source(out_index, dist);
                                }
                            }
                            // L237-L242. A pre-test loop: after a partial window
                            // copy `len` may already be zero.
                            while len > 2 {
                                copied &=
                                    copy_match(output, &mut out_index, history, &mut from, UNROLL);
                                len -= UNROLL;
                            }
                            // L243-L247
                            if len != 0 {
                                copied &= copy_match(output, &mut out_index, history, &mut from, 1);
                                if len > 1 {
                                    copied &=
                                        copy_match(output, &mut out_index, history, &mut from, 1);
                                }
                            }
                        } else {
                            // copy direct from output -- L249-L250
                            let mut from = output_source(out_index, dist);
                            // L251-L256. A post-test loop, because the minimum
                            // codeable length is three and one pass is therefore
                            // always warranted. The subtraction saturates so that
                            // a corrupt table claiming a shorter length still
                            // terminates instead of wrapping the way C would.
                            loop {
                                copied &=
                                    copy_match(output, &mut out_index, history, &mut from, UNROLL);
                                len = len.saturating_sub(UNROLL);
                                if len <= 2 {
                                    break;
                                }
                            }
                            // L257-L261
                            if len != 0 {
                                copied &= copy_match(output, &mut out_index, history, &mut from, 1);
                                if len > 1 {
                                    copied &=
                                        copy_match(output, &mut out_index, history, &mut from, 1);
                                }
                            }
                        }

                        if !copied {
                            // Unreachable under the entry contract: a match copy
                            // can only leave its buffer if `avail_out >= 258` was
                            // violated, or if the window is shorter than `whave`
                            // claims. Either way the distance cannot be honoured,
                            // which is what this message reports.
                            msg = Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK);
                            mode = Mode::Bad;
                            break 'outer;
                        }
                    } else if op & 64 == 0 {
                        // 2nd level distance code -- L264-L267. `here.val` is an
                        // offset from the base of the level-one table, which is
                        // exactly what `inflate_table` writes (`inftrees.c`
                        // L293), and `dcode` starts at that base.
                        let Some(&entry) =
                            dcode.get(to_index(u64::from(here.val) + (hold & low_mask(op))))
                        else {
                            msg = Some(MSG_INVALID_DISTANCE_CODE);
                            mode = Mode::Bad;
                            break 'outer;
                        };
                        here = entry;
                        continue 'dodist;
                    } else {
                        // L268-L272
                        msg = Some(MSG_INVALID_DISTANCE_CODE);
                        mode = Mode::Bad;
                        break 'outer;
                    }
                    // The length/distance pair is complete; leave `dodist` for
                    // the loop condition at L288, exactly as C falls out of its
                    // `if (op & 16)` block.
                    break 'dodist;
                }
            } else if op & 64 == 0 {
                // 2nd level length code -- L274-L277, relative to `lcode` for
                // the same reason as the distance link above.
                let Some(&entry) = lcode.get(to_index(u64::from(here.val) + (hold & low_mask(op))))
                else {
                    msg = Some(MSG_INVALID_LITERAL_LENGTH_CODE);
                    mode = Mode::Bad;
                    break 'outer;
                };
                here = entry;
                continue 'dolen;
            } else if op & 32 != 0 {
                // end-of-block -- L278-L282
                mode = Mode::Type;
                break 'outer;
            } else {
                // L283-L287
                msg = Some(MSG_INVALID_LITERAL_LENGTH_CODE);
                mode = Mode::Bad;
                break 'outer;
            }
            break 'dolen;
        }

        // L288: `} while (in < last && out < end);`
        if in_index >= last || out_index >= end {
            break 'outer;
        }
    }

    // -------------------------------------------------------------------------
    //  Return unused bytes (`inffast.c` L290-L294)
    //
    //  On entry `bits < 8`, so `in` cannot go back past where it started: every
    //  whole byte still in the accumulator was pulled by this call.
    // -------------------------------------------------------------------------
    let whole = bits >> 3; // L291: `len = bits >> 3`
    in_index = in_index.saturating_sub(to_index(u64::from(whole))); // L292
    bits = bits.saturating_sub(whole << 3); // L293
    hold &= low_mask(bits); // L294

    // -------------------------------------------------------------------------
    //  Update state and return (`inffast.c` L296-L303)
    // -------------------------------------------------------------------------
    *next_in = in_index; // L297
    *next_out = out_index; // L298

    // L299 and L300-L301, evaluated exactly as written. Both conditionals
    // collapse algebraically to `buffer.len() - cursor`; see the module's
    // availability section, and the assertions below which pin the identity.
    let avail_in = if in_index < last {
        IN_SLACK + (last - in_index)
    } else {
        IN_SLACK.saturating_sub(in_index - last)
    };
    let avail_out = if out_index < end {
        OUT_SLACK + (end - out_index)
    } else {
        OUT_SLACK.saturating_sub(out_index - end)
    };
    debug_assert_eq!(
        avail_in,
        input.len().saturating_sub(in_index),
        "inffast.c L299 must agree with the cursor model"
    );
    debug_assert_eq!(
        avail_out,
        output.len().saturating_sub(out_index),
        "inffast.c L300-L301 must agree with the cursor model"
    );
    state.hold = hold; // L302
    state.bits = bits; // L303
    state.mode = mode;

    FastExit {
        mode,
        msg,
        avail_in,
        avail_out,
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
// The workspace denies the panic family and slice indexing in library code,
// which is exactly what this module is built to guarantee; a test that cannot
// assert is useless, so the harness opts back in here only. Indices below are
// literal and known good.
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        copy_match, drop_bits, inflate_fast, low_mask, output_source, to_count, to_extra, to_index,
        window_source, FastExit, MatchSource, MIN_AVAIL_IN, MIN_AVAIL_OUT,
        MSG_INVALID_DISTANCE_CODE, MSG_INVALID_DISTANCE_TOO_FAR_BACK,
        MSG_INVALID_LITERAL_LENGTH_CODE, NO_HISTORY,
    };

    use alloc::vec;
    use alloc::vec::Vec;

    use crate::allocate::GlobalAllocator;
    use crate::config::InflateConfig;
    use crate::inflate::inftrees::Code;
    use crate::inflate::mode::Mode;
    use crate::inflate::state::{InflateState, InflateWindow};

    /// The state type every test drives.
    type TestState<'a> = InflateState<'a, GlobalAllocator>;

    /// Output buffer large enough that `avail_out >= 258` still holds after a
    /// maximum-length match, so a test can decode several codes per call.
    const OUT_LEN: usize = 1024;

    /// A distinguishable, position-dependent window byte.
    ///
    /// Deliberately never zero and never repeating within a 32 KiB window, so a
    /// wrong window index shows up as a wrong byte rather than as a coincidence.
    fn pattern(index: usize) -> u8 {
        u8::try_from((index * 31 + 7) % 251).unwrap() | 1
    }

    /// A raw-mode state with `mode == Len`, ready for `inflate_fast`.
    fn raw_state<'a>() -> TestState<'a> {
        let mut state = InflateState::new(InflateConfig::new(-15), GlobalAllocator).unwrap();
        state.mode = Mode::Len;
        state
    }

    /// Writes `(arena index, op, bits, val)` entries into the code arena and
    /// selects them as the dynamic tables.
    fn install_tables(
        state: &mut TestState<'_>,
        entries: &[(usize, u8, u8, u16)],
        lencode_offset: usize,
        lenbits: usize,
        distcode_offset: usize,
        distbits: usize,
    ) {
        for &(index, op, bits, val) in entries {
            state.codes[index] = Code::new(op, bits, val);
        }
        assert!(state.use_dynamic_tables(lencode_offset, lenbits, distcode_offset, distbits));
    }

    /// One-bit root tables: index 0 of the length table is a length base of
    /// `len` with no extra bits, index 1 is end-of-block, and index 0 of the
    /// distance table is a distance base of `dist` with no extra bits.
    ///
    /// This is the smallest table shape that can drive a length/distance pair,
    /// which makes the input bit stream trivially readable: bit 0 selects the
    /// length code, bit 1 the distance code.
    fn install_pair_tables(state: &mut TestState<'_>, len: u16, dist: u16) {
        install_tables(
            state,
            &[
                (0, 16, 1, len),  // length base, zero extra bits
                (1, 96, 1, 0),    // end-of-block (32 | 64)
                (2, 16, 1, dist), // distance base, zero extra bits
                (3, 64, 1, 0),    // invalid distance code
            ],
            0,
            1,
            2,
            1,
        );
    }

    /// Packs a DEFLATE bit stream: Huffman codes most-significant bit first,
    /// extra bits least-significant bit first (RFC 1951 §3.1.1).
    #[derive(Debug, Default)]
    struct BitStream {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl BitStream {
        fn push_bit(&mut self, bit: u8) {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            let last = self.bytes.len() - 1;
            self.bytes[last] |= bit << u32::try_from(self.bits % 8).unwrap();
            self.bits += 1;
        }

        /// A Huffman code, most-significant bit of the code first.
        fn code(&mut self, code: u32, width: u32) {
            for shift in (0..width).rev() {
                self.push_bit(u8::try_from((code >> shift) & 1).unwrap());
            }
        }

        /// Trailing padding, so that `avail_in >= 6` holds and the loop
        /// condition has room to run.
        fn finish(mut self, padding: usize) -> Vec<u8> {
            for _ in 0..padding {
                self.bytes.push(0);
            }
            self.bytes
        }
    }

    /// The fixed literal/length code for a symbol, as `(code, width)`
    /// (RFC 1951 §3.2.6, and the table `inflate_table` reproduces in
    /// `inffixed.h`).
    fn fixed_litlen(symbol: u32) -> (u32, u32) {
        match symbol {
            0..=143 => (0x30 + symbol, 8),
            144..=255 => (0x190 + symbol - 144, 9),
            256..=279 => (symbol - 256, 7),
            _ => (0xc0 + symbol - 280, 8),
        }
    }

    /// Runs `inflate_fast` over freshly sized buffers with `beg == 0`.
    ///
    /// `start` is `output.len()`, which is what `inflate()` passes: the
    /// `avail_out` the enclosing call began with (`inflate.c` L915). With
    /// `next_out` pre-positioned at `out_at`, that reconstructs
    /// `beg == 0` exactly as C's `out - (start - avail_out)` does.
    fn drive(
        state: &mut TestState<'_>,
        input: &[u8],
        output: &mut [u8],
        out_at: usize,
    ) -> (FastExit, usize, usize) {
        let mut next_in = 0;
        let mut next_out = out_at;
        let start = output.len();
        let exit = inflate_fast(state, input, &mut next_in, output, &mut next_out, start);
        (exit, next_in, next_out)
    }

    // -------------------------------------------------------------------------
    //  Total helpers
    // -------------------------------------------------------------------------

    #[test]
    fn low_mask_matches_the_c_expression_and_is_total() {
        for n in 0..64_u32 {
            assert_eq!(low_mask(n), (1_u64 << n) - 1, "low_mask({n})");
        }
        // C's `(1U << n) - 1` is undefined for n >= the type width; the port
        // saturates to every bit rather than panicking.
        assert_eq!(low_mask(64), u64::MAX);
        assert_eq!(low_mask(u32::MAX), u64::MAX);
        assert_eq!(low_mask(0), 0);
    }

    #[test]
    fn drop_bits_matches_the_c_shift_and_is_total() {
        let hold = 0xdead_beef_1234_5678_u64;
        for n in 0..64_u32 {
            assert_eq!(drop_bits(hold, n), hold >> n, "drop_bits({n})");
        }
        assert_eq!(drop_bits(hold, 64), 0);
        assert_eq!(drop_bits(hold, u32::MAX), 0);
    }

    #[test]
    fn the_conversion_helpers_saturate_rather_than_wrap() {
        assert_eq!(to_index(0), 0);
        assert_eq!(to_index(258), 258);
        assert_eq!(to_extra(0x7fff), 0x7fff);
        assert_eq!(to_extra(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(to_extra(u64::MAX), u32::MAX);
        assert_eq!(to_count(0), 0);
        assert_eq!(to_count(32768), 32768);
        assert_eq!(to_count(usize::MAX), u32::MAX);
    }

    #[test]
    fn match_sources_name_the_two_buffers_c_fuses() {
        assert_eq!(window_source(7, false), MatchSource::Window(7));
        // `inflateBack()`: the window *is* the output buffer, so the same index
        // names the same byte.
        assert_eq!(window_source(7, true), MatchSource::Output(7));
        assert_eq!(output_source(10, 3), MatchSource::Output(7));
        // `out - dist` saturates rather than wrapping.
        assert_eq!(output_source(2, 9), MatchSource::Output(0));
    }

    // -------------------------------------------------------------------------
    //  copy_match
    // -------------------------------------------------------------------------

    #[test]
    fn copy_match_from_the_window_advances_both_cursors() {
        let history = [10_u8, 11, 12, 13, 14];
        let mut output = [0_u8; 8];
        let mut out = 2;
        let mut from = MatchSource::Window(1);
        assert!(copy_match(&mut output, &mut out, &history, &mut from, 3));
        assert_eq!(&output[..], &[0, 0, 11, 12, 13, 0, 0, 0]);
        assert_eq!(out, 5);
        assert_eq!(from, MatchSource::Window(4));
    }

    #[test]
    fn copy_match_within_the_output_reproduces_a_naive_byte_loop() {
        // Distance one is the RLE case: every byte after the first is read back
        // from what this copy just wrote. Any bulk move fails this.
        for dist in 1..=4_usize {
            for len in 1..=32_u32 {
                let mut output = vec![0_u8; 128];
                for (index, slot) in output.iter_mut().take(dist).enumerate() {
                    *slot = pattern(index);
                }
                let mut out = dist;
                let mut from = MatchSource::Output(0);
                assert!(copy_match(
                    &mut output,
                    &mut out,
                    NO_HISTORY,
                    &mut from,
                    len
                ));

                let mut expected: Vec<u8> = (0..dist).map(pattern).collect();
                for _ in 0..len {
                    expected.push(expected[expected.len() - dist]);
                }
                assert_eq!(
                    &output[..expected.len()],
                    &expected[..],
                    "dist {dist}, len {len}"
                );
                assert_eq!(out, dist + usize::try_from(len).unwrap());
            }
        }
    }

    #[test]
    fn copy_match_refuses_to_leave_either_buffer() {
        let history = [1_u8, 2, 3];
        let mut output = [0_u8; 4];

        // Destination overruns.
        let mut out = 2;
        let mut from = MatchSource::Window(0);
        assert!(!copy_match(&mut output, &mut out, &history, &mut from, 3));
        assert_eq!(out, 2, "a refused copy advances nothing");
        assert_eq!(output, [0; 4], "a refused copy writes nothing");

        // Source overruns the history.
        let mut out = 0;
        let mut from = MatchSource::Window(2);
        assert!(!copy_match(&mut output, &mut out, &history, &mut from, 2));
        assert_eq!(out, 0);

        // Source overruns the output.
        let mut out = 0;
        let mut from = MatchSource::Output(3);
        assert!(!copy_match(&mut output, &mut out, &history, &mut from, 2));
        assert_eq!(out, 0);

        // An index that saturated to `usize::MAX` fails its bounds check
        // instead of wrapping.
        let mut out = 0;
        let mut from = MatchSource::Window(usize::MAX);
        assert!(!copy_match(&mut output, &mut out, &history, &mut from, 1));
    }

    // -------------------------------------------------------------------------
    //  The decode loop
    // -------------------------------------------------------------------------

    #[test]
    fn a_fixed_block_of_literals_decodes_byte_identically() {
        // The exact payload `test/example.c` compresses (its L34).
        let payload: &[u8] = b"hello, hello!";
        let mut stream = BitStream::default();
        for &byte in payload {
            let (code, width) = fixed_litlen(u32::from(byte));
            stream.code(code, width);
        }
        let (eob, eob_width) = fixed_litlen(256);
        stream.code(eob, eob_width);
        let input = stream.finish(8);

        let mut state = raw_state();
        state.use_fixed_tables();
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, next_in, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Type, "end-of-block must report TYPE");
        assert_eq!(exit.msg, None);
        assert_eq!(state.mode, Mode::Type, "the state is written back too");
        assert_eq!(next_out, payload.len());
        assert_eq!(&output[..payload.len()], payload);
        assert_eq!(exit.avail_in, input.len() - next_in);
        assert_eq!(exit.avail_out, output.len() - next_out);
        assert!(state.bits < 8, "whole bytes are returned to the input");
        assert_eq!(state.hold >> state.bits, 0, "no stray high bits in hold");
    }

    #[test]
    fn a_fixed_block_match_of_maximum_length_decodes_byte_identically() {
        // Literal 'Q', then length 258 (symbol 285) at distance 1 (symbol 0),
        // then end-of-block: 259 copies of one byte through the direct path.
        let mut stream = BitStream::default();
        let (literal, literal_width) = fixed_litlen(u32::from(b'Q'));
        stream.code(literal, literal_width);
        let (length, length_width) = fixed_litlen(285);
        stream.code(length, length_width);
        stream.code(0, 5); // distance symbol 0 == distance 1
        let (eob, eob_width) = fixed_litlen(256);
        stream.code(eob, eob_width);
        let input = stream.finish(8);

        let mut state = raw_state();
        state.use_fixed_tables();
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Type);
        assert_eq!(next_out, 259);
        assert!(
            output[..259].iter().all(|&byte| byte == b'Q'),
            "a distance-one match of length 258 is a run of one byte"
        );
        assert_eq!(output[259], 0, "nothing is written past the match");
    }

    #[test]
    fn second_level_length_and_distance_tables_are_both_followed() {
        // Root width one on both sides, with entry zero a table link. The
        // sub-tables sit immediately after each root, which is what
        // `inflate_table` produces and what `here.val` is an offset from
        // (`inftrees.c` L293).
        let mut state = raw_state();
        install_tables(
            &mut state,
            &[
                // literal/length root at 0, one index bit
                (0, 2, 1, 2),  // link: two more index bits, sub-table at +2
                (1, 96, 1, 0), // end-of-block
                // literal/length sub-table at 2, two index bits
                (2, 64, 2, 0), // invalid
                (3, 16, 2, 5), // length base 5, zero extra bits
                (4, 0, 2, 67), // literal 'C'
                (5, 0, 2, 68), // literal 'D'
                // distance root at 6, one index bit
                (6, 1, 1, 2),  // link: one more index bit, sub-table at +2
                (7, 64, 1, 0), // invalid
                // distance sub-table at 8
                (8, 64, 1, 0), // invalid
                (9, 16, 1, 2), // distance base 2, zero extra bits
            ],
            0,
            1,
            6,
            1,
        );

        // bit 0 = 0 -> length root link; bits 1..2 = 01 -> sub-index 1, length 5;
        // bit 3 = 0 -> distance root link; bit 4 = 1 -> sub-index 1, distance 2.
        let input = [0b0001_0010_u8, 0, 0, 0, 0, 0];

        let mut output = vec![0_u8; OUT_LEN];
        output[..4].copy_from_slice(b"WXYZ");
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 4);

        assert_eq!(exit.mode, Mode::Len, "input ran out after one pair");
        assert_eq!(exit.msg, None);
        assert_eq!(next_out, 9);
        assert_eq!(&output[..9], b"WXYZYZYZY");
    }

    #[test]
    fn end_of_block_reports_type_and_keeps_the_bit_buffer_exact() {
        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 1);
        // bit 0 = 1 selects length-table index 1, the end-of-block entry.
        let input = [0b0000_0001_u8, 0, 0, 0, 0, 0];
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, next_in, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Type);
        assert_eq!(exit.msg, None);
        assert_eq!(next_out, 0, "end-of-block emits nothing");
        // Two bytes were pulled and one bit consumed, so fifteen bits remain:
        // one whole byte goes back to the input and seven bits stay in `hold`.
        assert_eq!(next_in, 1);
        assert_eq!(state.bits, 7);
        assert_eq!(state.hold, 0);
        assert_eq!(exit.avail_in, input.len() - next_in);
        assert_eq!(exit.avail_out, output.len() - next_out);
    }

    #[test]
    fn an_invalid_literal_length_code_reports_the_reference_message() {
        let mut state = raw_state();
        install_tables(&mut state, &[(0, 64, 1, 0), (2, 16, 1, 1)], 0, 1, 2, 1);
        let input = [0_u8; 6];
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Bad);
        assert_eq!(exit.msg, Some(MSG_INVALID_LITERAL_LENGTH_CODE));
        assert_eq!(exit.msg, Some("invalid literal/length code"));
        assert_eq!(state.mode, Mode::Bad);
        assert_eq!(next_out, 0);
    }

    #[test]
    fn an_invalid_distance_code_reports_the_reference_message() {
        let mut state = raw_state();
        // Length code is valid; the distance entry is marked invalid.
        install_tables(&mut state, &[(0, 16, 1, 3), (2, 64, 1, 0)], 0, 1, 2, 1);
        let input = [0_u8; 6];
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Bad);
        assert_eq!(exit.msg, Some(MSG_INVALID_DISTANCE_CODE));
        assert_eq!(exit.msg, Some("invalid distance code"));
        assert_eq!(next_out, 0);
    }

    #[test]
    fn the_three_error_messages_are_the_reference_strings() {
        // `test/infcover.c` prints these, so they are compared literally.
        assert_eq!(MSG_INVALID_DISTANCE_CODE, "invalid distance code");
        assert_eq!(
            MSG_INVALID_LITERAL_LENGTH_CODE,
            "invalid literal/length code"
        );
        assert_eq!(
            MSG_INVALID_DISTANCE_TOO_FAR_BACK,
            "invalid distance too far back"
        );
    }

    // -------------------------------------------------------------------------
    //  The window copy
    // -------------------------------------------------------------------------

    /// Window size used by the window-case tests.
    ///
    /// Thirty-two bytes is far smaller than any real window, which is the point:
    /// every index in the three cases can be written out by hand and checked.
    /// Nothing in `inflate_fast` depends on `wsize` being a particular size.
    const SMALL_WSIZE: usize = 32;

    /// Drives one length/distance pair against a hand-built window and returns
    /// the bytes written.
    ///
    /// `whave` and `wnext` are installed verbatim so that each of the three
    /// window cases -- and the `op > whave` rejection -- can be selected
    /// directly.
    fn drive_window_case(
        len: u16,
        dist: u16,
        whave: u32,
        wnext: u32,
        out_at: usize,
    ) -> (FastExit, Vec<u8>, usize) {
        let mut window = [0_u8; SMALL_WSIZE];
        for (index, slot) in window.iter_mut().enumerate() {
            *slot = pattern(index);
        }

        let mut state = raw_state();
        install_pair_tables(&mut state, len, dist);
        state.wsize = u32::try_from(SMALL_WSIZE).unwrap();
        state.whave = whave;
        state.wnext = wnext;
        state.window = InflateWindow::borrowed(&mut window);

        // bit 0 = 0 -> length index 0; bit 1 = 0 -> distance index 0.
        let input = [0_u8; MIN_AVAIL_IN];
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, out_at);
        (exit, output, next_out)
    }

    #[test]
    fn all_three_window_cases_copy_the_reference_bytes() {
        // Every row is one branch of `inffast.c` L197-L236, driven with
        // `out_index == 0` so that `op == out - beg == 0` and the whole match
        // comes from the window until `from` switches to the output.
        //
        // (len, dist, whave, wnext, expected window/output indices)
        struct Case {
            name: &'static str,
            len: u16,
            dist: u16,
            whave: u32,
            wnext: u32,
            /// Expected bytes, as window indices; `None` means "the output byte
            /// already written at this position", i.e. a self-referential copy.
            expected: &'static [usize],
        }

        let cases = [
            Case {
                // wnext == 0, op < len: some from window, rest from output.
                name: "very common case, op < len",
                len: 10,
                dist: 4,
                whave: 32,
                wnext: 0,
                expected: &[28, 29, 30, 31, 28, 29, 30, 31, 28, 29],
            },
            Case {
                // wnext == 0, op >= len: the whole match is in the window.
                name: "very common case, op >= len",
                len: 4,
                dist: 10,
                whave: 32,
                wnext: 0,
                expected: &[22, 23, 24, 25],
            },
            Case {
                // wnext < op: wrap around the window, one segment only.
                name: "wrap around window, wnext >= len after the first segment",
                len: 10,
                dist: 10,
                whave: 32,
                wnext: 4,
                expected: &[26, 27, 28, 29, 30, 31, 0, 1, 2, 3],
            },
            Case {
                // wnext < op and wnext < len: both wrap segments run, then the
                // rest comes from the output.
                name: "wrap around window, both segments then output",
                len: 12,
                dist: 10,
                whave: 32,
                wnext: 4,
                expected: &[26, 27, 28, 29, 30, 31, 0, 1, 2, 3, 26, 27],
            },
            Case {
                // wnext < op, op >= len: only the tail segment is needed.
                name: "wrap around window, op >= len",
                len: 4,
                dist: 10,
                whave: 32,
                wnext: 4,
                expected: &[26, 27, 28, 29],
            },
            Case {
                // wnext >= op: contiguous in the window, pre-wrap `whave ==
                // wnext`, so the clamp to `window[..whave]` is exercised too.
                name: "contiguous in window, op < len",
                len: 15,
                dist: 10,
                whave: 20,
                wnext: 20,
                expected: &[10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 10, 11, 12, 13, 14],
            },
            Case {
                name: "contiguous in window, op >= len",
                len: 5,
                dist: 10,
                whave: 20,
                wnext: 20,
                expected: &[10, 11, 12, 13, 14],
            },
        ];

        for case in cases {
            let (exit, output, next_out) =
                drive_window_case(case.len, case.dist, case.whave, case.wnext, 0);
            assert_eq!(exit.mode, Mode::Len, "{}", case.name);
            assert_eq!(exit.msg, None, "{}", case.name);
            assert_eq!(next_out, case.expected.len(), "{}", case.name);
            let expected: Vec<u8> = case.expected.iter().copied().map(pattern).collect();
            assert_eq!(&output[..expected.len()], &expected[..], "{}", case.name);
            assert_eq!(
                output[expected.len()],
                0,
                "{}: nothing past the match",
                case.name
            );
        }
    }

    #[test]
    fn a_distance_reaching_past_whave_is_rejected() {
        // `whave == wnext == 8` is the pre-wrap state: only `window[..8]` has
        // ever been written. A distance of twelve reaches four bytes further
        // back than that, so `op > whave` and the stream is invalid.
        let (exit, output, next_out) = drive_window_case(6, 12, 8, 8, 0);
        assert_eq!(exit.mode, Mode::Bad);
        assert_eq!(exit.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
        assert_eq!(next_out, 0, "nothing is emitted from an invalid distance");
        assert_eq!(output[0], 0);
    }

    #[test]
    fn widening_whave_would_have_accepted_the_invalid_distance() {
        // The negative half of the test above, and the reason commit 09a1572
        // exists. Re-running the identical stream with `whave` widened to
        // `wsize` -- exactly what the two deleted lines of `infback.c` did --
        // accepts it and emits window bytes 28..32, which no legitimate stream
        // ever wrote. Nothing in `inflate_fast` may ever widen `whave`.
        let (exit, output, next_out) = drive_window_case(6, 12, 32, 8, 0);
        assert_eq!(
            exit.mode,
            Mode::Len,
            "a widened whave silently accepts the bad stream"
        );
        assert_eq!(exit.msg, None);
        assert_eq!(next_out, 6);
        let expected: Vec<u8> = [28, 29, 30, 31, 0, 1]
            .iter()
            .copied()
            .map(pattern)
            .collect();
        assert_eq!(&output[..6], &expected[..]);
    }

    #[test]
    fn an_absent_window_rejects_every_backward_distance() {
        // `inflate()` before `updatewindow` has run: no window, `whave == 0`.
        // The guard rejects before any read is attempted.
        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 1);
        assert!(state.window.is_absent());
        assert_eq!(state.whave, 0);
        let input = [0_u8; MIN_AVAIL_IN];
        let mut output = vec![0_u8; OUT_LEN];
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 0);

        assert_eq!(exit.mode, Mode::Bad);
        assert_eq!(exit.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
        assert_eq!(next_out, 0);
    }

    #[test]
    fn inflate_back_reads_history_from_its_output_buffer() {
        // `inflateBack()`'s window *is* its output buffer (`infback.c` L143-L152)
        // and its `wnext` is always zero. The caller hands the window over as
        // `output`, leaving the state's window slot empty, and passes
        // `start == wsize` (`infback.c` L425). History indices then name output
        // indices directly.
        const BACK_WSIZE: usize = 512;

        let mut state = raw_state();
        install_pair_tables(&mut state, 10, 20);
        state.wsize = u32::try_from(BACK_WSIZE).unwrap();
        state.whave = state.wsize; // set by `ROOM()` when the window is flushed
        state.wnext = 0;
        assert!(state.window.is_absent(), "the window travels as `output`");

        let mut output = vec![0_u8; BACK_WSIZE];
        for (index, slot) in output.iter_mut().enumerate() {
            *slot = pattern(index);
        }
        let previous = output.clone();

        let input = [0_u8; MIN_AVAIL_IN];
        let mut next_in = 0;
        let mut next_out = 0;
        let start = BACK_WSIZE; // `state->wsize`, not `avail_out`
        let exit = inflate_fast(
            &mut state,
            &input,
            &mut next_in,
            &mut output,
            &mut next_out,
            start,
        );

        assert_eq!(exit.mode, Mode::Len);
        assert_eq!(exit.msg, None);
        assert_eq!(next_out, 10);
        // `wnext == 0` selects `from = window + (wsize - op)` with `op == dist`,
        // so the match reads the ten bytes at `wsize - 20`.
        assert_eq!(
            &output[..10],
            &previous[BACK_WSIZE - 20..BACK_WSIZE - 10],
            "history came from the output buffer at the same indices"
        );
    }

    // -------------------------------------------------------------------------
    //  Overlapping copies
    // -------------------------------------------------------------------------

    #[test]
    fn overlapping_matches_reproduce_a_naive_byte_at_a_time_reference() {
        // Distances below the match length are legal and common. The reference
        // for each is a byte-at-a-time loop written here, independently of the
        // implementation.
        for dist in 1..=3_usize {
            let mut state = raw_state();
            install_pair_tables(&mut state, 258, u16::try_from(dist).unwrap());

            let mut output = vec![0_u8; OUT_LEN];
            for (index, slot) in output.iter_mut().take(dist).enumerate() {
                *slot = pattern(index);
            }

            let input = [0_u8; MIN_AVAIL_IN];
            let (exit, _, next_out) = drive(&mut state, &input, &mut output, dist);

            let mut expected: Vec<u8> = (0..dist).map(pattern).collect();
            for _ in 0..258 {
                expected.push(expected[expected.len() - dist]);
            }

            assert_eq!(exit.mode, Mode::Len, "dist {dist}");
            assert_eq!(next_out, dist + 258, "dist {dist}");
            assert_eq!(&output[..dist + 258], &expected[..], "dist {dist}");
        }
    }

    // -------------------------------------------------------------------------
    //  The exit fixup
    // -------------------------------------------------------------------------

    #[test]
    fn the_exit_fixup_reproduces_the_reference_availabilities() {
        // `inffast.c` L299-L301 branches on whether the cursor has passed its
        // sentinel. Both sides of both conditionals are exercised, since `last`
        // and `end` move with the buffer lengths.
        for extra_in in 0..6_usize {
            for out_len in [MIN_AVAIL_OUT, MIN_AVAIL_OUT + 1, OUT_LEN] {
                let mut state = raw_state();
                install_pair_tables(&mut state, 3, 1);
                // Length-table index 1 is end-of-block: one bit consumed, so
                // the fixup has whole bytes to hand back.
                let mut input = vec![0_u8; MIN_AVAIL_IN + extra_in];
                input[0] = 1;
                let mut output = vec![0_u8; out_len];
                let (exit, next_in, next_out) = drive(&mut state, &input, &mut output, 0);

                assert_eq!(
                    exit.avail_in,
                    input.len() - next_in,
                    "avail_in for extra_in {extra_in}"
                );
                assert_eq!(
                    exit.avail_out,
                    output.len() - next_out,
                    "avail_out for out_len {out_len}"
                );
                assert!(state.bits < 8, "entry bits < 8 is restored on exit");
                assert_eq!(state.hold >> state.bits, 0, "hold carries no stray bits");
                assert_eq!(exit.mode, Mode::Type);
            }
        }
    }

    #[test]
    fn the_minimum_permitted_buffers_decode_exactly_one_iteration() {
        // `avail_in == 6` and `avail_out == 258` are the smallest values the
        // contract allows. `last` is then `1` and the two-byte refill takes the
        // cursor straight past it, so exactly one code is decoded.
        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 1);
        let input = [0_u8; MIN_AVAIL_IN];
        // One seed byte plus the 258 the contract demands, so `avail_out` is
        // exactly 258 once `next_out` sits at one.
        let mut output = vec![0_u8; MIN_AVAIL_OUT + 1];
        // A distance of one reaching back into the seed byte keeps the whole
        // match inside the output, so no window is needed.
        output[0] = b'k';
        let (exit, next_in, next_out) = drive(&mut state, &input, &mut output, 1);

        assert_eq!(exit.mode, Mode::Len, "one iteration, then out of input");
        assert_eq!(next_out, 4, "three bytes copied at distance one");
        assert_eq!(&output[..4], b"kkkk");
        assert_eq!(exit.avail_in, input.len() - next_in);
        assert_eq!(exit.avail_out, output.len() - next_out);
    }

    #[test]
    fn start_reconstructs_beg_for_both_caller_conventions() {
        // `beg` is `out - (start - avail_out)`. `inflate()` passes the starting
        // `avail_out`, which puts `beg` at the beginning of the output buffer;
        // `inflateBack()` passes `wsize`, which puts it at the beginning of the
        // window. A `start` that names a later origin moves `beg` forward, and
        // then a distance reaching before it must come from the window rather
        // than from the output.
        let mut window = [0_u8; SMALL_WSIZE];
        for (index, slot) in window.iter_mut().enumerate() {
            *slot = pattern(index);
        }

        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 2);
        state.wsize = u32::try_from(SMALL_WSIZE).unwrap();
        state.whave = state.wsize;
        state.wnext = 0;
        state.window = InflateWindow::borrowed(&mut window);

        let input = [0_u8; MIN_AVAIL_IN];
        let mut output = vec![0_u8; OUT_LEN];
        output[..4].copy_from_slice(b"abcd");
        let mut next_in = 0;
        let mut next_out = 4;
        // `start` chosen so that `start - avail_out == 0`, i.e. `beg == out`:
        // nothing in this call's output is addressable, so a distance of two
        // must be served from the window.
        let start = output.len() - next_out;
        let exit = inflate_fast(
            &mut state,
            &input,
            &mut next_in,
            &mut output,
            &mut next_out,
            start,
        );

        assert_eq!(exit.mode, Mode::Len);
        assert_eq!(next_out, 7);
        let expected: Vec<u8> = [30, 31, 30].iter().copied().map(pattern).collect();
        assert_eq!(
            &output[4..7],
            &expected[..],
            "beg == out sends the whole match to the window"
        );
        assert_eq!(&output[..4], b"abcd", "earlier output is untouched");
    }

    // -------------------------------------------------------------------------
    //  The entry contract
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(not(debug_assertions))]
    fn a_violated_entry_contract_returns_without_decoding() {
        for (input_len, output_len) in [
            (MIN_AVAIL_IN - 1, OUT_LEN),
            (MIN_AVAIL_IN, MIN_AVAIL_OUT - 1),
            (0, 0),
        ] {
            let mut state = raw_state();
            install_pair_tables(&mut state, 3, 1);
            let input = vec![0_u8; input_len];
            let mut output = vec![0_u8; output_len];
            let (exit, next_in, next_out) = drive(&mut state, &input, &mut output, 0);

            assert_eq!(exit.mode, Mode::Len);
            assert_eq!(exit.msg, None);
            assert_eq!(next_in, 0, "nothing consumed");
            assert_eq!(next_out, 0, "nothing emitted");
            assert_eq!(state.bits, 0, "state untouched");
            assert!(output.iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "avail_in >= 6")]
    fn a_short_input_trips_the_debug_assertion() {
        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 1);
        let input = [0_u8; MIN_AVAIL_IN - 1];
        let mut output = vec![0_u8; OUT_LEN];
        let _ = drive(&mut state, &input, &mut output, 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "avail_out >= 258")]
    fn a_short_output_trips_the_debug_assertion() {
        let mut state = raw_state();
        install_pair_tables(&mut state, 3, 1);
        let input = [0_u8; MIN_AVAIL_IN];
        let mut output = vec![0_u8; MIN_AVAIL_OUT - 1];
        let _ = drive(&mut state, &input, &mut output, 0);
    }

    // -------------------------------------------------------------------------
    //  Termination and robustness
    // -------------------------------------------------------------------------

    /// A deterministic xorshift64\* generator, so the smoke test below needs no
    /// external crate and reproduces exactly on every run.
    #[derive(Debug)]
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn fill(&mut self, bytes: &mut [u8]) {
            for slot in bytes.iter_mut() {
                *slot = u8::try_from(self.next_u64() & 0xff).unwrap();
            }
        }
    }

    #[test]
    fn pseudo_random_input_never_panics_and_always_terminates() {
        // The real gate is `cargo fuzz run fuzz_inflate -- -max_total_time=300`.
        // This is the in-crate smoke test for the same property: arbitrary bytes
        // decoded against the RFC 1951 fixed tables and a fully valid window must
        // always come back, in bounds, with one of the three permitted modes.
        const ROUNDS: usize = 4096;
        const IN_LEN: usize = 64;

        let mut rng = Rng(0x1234_5678_9abc_def0);
        let mut window = [0_u8; SMALL_WSIZE];
        for (index, slot) in window.iter_mut().enumerate() {
            *slot = pattern(index);
        }

        let mut state = raw_state();
        state.use_fixed_tables();
        state.wsize = u32::try_from(SMALL_WSIZE).unwrap();
        state.window = InflateWindow::borrowed(&mut window);

        let mut input = [0_u8; IN_LEN];
        let mut output = vec![0_u8; OUT_LEN];

        for round in 0..ROUNDS {
            rng.fill(&mut input);
            // Vary the window bookkeeping across the legal states: pre-wrap
            // (`whave == wnext`) and post-wrap (`whave == wsize`).
            let wnext = u32::try_from(round % (SMALL_WSIZE + 1)).unwrap();
            state.wnext = wnext % u32::try_from(SMALL_WSIZE).unwrap();
            state.whave = if round % 2 == 0 {
                state.wnext
            } else {
                state.wsize
            };
            state.hold = 0;
            state.bits = 0;
            state.mode = Mode::Len;

            let mut next_in = 0;
            let mut next_out = 0;
            let start = output.len();
            let exit = inflate_fast(
                &mut state,
                &input,
                &mut next_in,
                &mut output,
                &mut next_out,
                start,
            );

            assert!(
                matches!(exit.mode, Mode::Len | Mode::Type | Mode::Bad),
                "round {round}: mode {:?} is not one of inffast.c L31-L35",
                exit.mode
            );
            assert_eq!(exit.mode == Mode::Bad, exit.msg.is_some(), "round {round}");
            assert!(next_in <= input.len(), "round {round}");
            assert!(next_out <= output.len(), "round {round}");
            assert_eq!(exit.avail_in, input.len() - next_in, "round {round}");
            assert_eq!(exit.avail_out, output.len() - next_out, "round {round}");
            assert!(state.bits < 8, "round {round}");
            assert_eq!(state.hold >> state.bits, 0, "round {round}");
        }
    }

    #[test]
    fn a_corrupt_zero_length_entry_terminates_instead_of_running_away() {
        // `LBASE` never yields a length below three, so C's post-test drain is
        // sound for every table `inflate_table` builds. A hand-corrupted entry
        // claiming length zero would underflow C's `len -= 3`; here the
        // subtraction saturates, so the drain stops after one pass.
        let mut state = raw_state();
        install_pair_tables(&mut state, 0, 1);
        let input = [0_u8; MIN_AVAIL_IN];
        let mut output = vec![0_u8; OUT_LEN];
        output[0] = b'z';
        let (exit, _, next_out) = drive(&mut state, &input, &mut output, 1);

        assert_eq!(exit.mode, Mode::Len);
        assert_eq!(next_out, 4, "one unrolled pass, then the loop ends");
        assert_eq!(&output[..4], b"zzzz");
    }
}
