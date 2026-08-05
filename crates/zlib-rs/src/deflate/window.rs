//! The sliding window and the hash tables that index it.
//!
//! Port of two functions from the reference implementation, and nothing else:
//!
//! | Here | There | Role |
//! |---|---|---|
//! | [`slide_hash`] | `slide_hash` (`deflate.c` L177-L210) | Remap both tables down by `w_size` |
//! | [`fill_window`] | `fill_window` (`deflate.c` L242-L376) | Slide, refill, prime, zero tail |
//!
//! Every one of the five compression strategies calls [`fill_window`] the moment its lookahead
//! runs short, so this is the single point at which input enters the window, at which history
//! is discarded, and at which the region past the data is made safe for the match routines to
//! over-scan.
//!
//! # How the window is organized
//!
//! The buffer is twice the LZ77 window size, and the two halves have different jobs
//! (`deflate.h` L124-L131):
//!
//! ```text
//! Sliding window. Input bytes are read into the second half of the window,
//! and move to the first half later to keep a dictionary of at least wSize
//! bytes. With this organization, matches are limited to a distance of
//! wSize-MAX_MATCH bytes, but this ensures that IO is always
//! performed with a length multiple of the block size.
//! ```
//!
//! ```text
//!  0                     w_size                    2 * w_size == window_size
//!  |------- history -------|------- fresh input -------|
//!  ^                       ^          ^                ^
//!  0                  block_start  strstart      strstart + lookahead
//!                                                      |<-- free space = `more` -->|
//! ```
//!
//! `window_size == 2 * w_size` is established once, by `lm_init`
//! (`s->window_size = (ulg)2L*s->w_size;`, `deflate.c` L683), and
//! [`crate::weak_slice::Window::new`] refuses any buffer that is not exactly that length, so
//! the invariant every calculation below depends on is checked where the buffer is wrapped
//! rather than re-asserted here.
//!
//! That layout is *why* match distances are capped below `w_size`. `MAX_DIST(s)` is
//! `w_size - MIN_LOOKAHEAD` (`deflate.h` L301), which leaves 262 bytes of slack so that a
//! `MAX_MATCH`-long comparison starting anywhere the compressor may stand stays inside the
//! buffer.
//!
//! # `high_water`, and why zeroing is not optional
//!
//! `longest_match` compares up to `strstart + MAX_MATCH` bytes *ignoring `lookahead`*
//! (`deflate.c` L344-L345), so it deliberately reads past the end of the data. The window
//! arrives from the caller's allocator, which is `malloc` and not `calloc` in the default
//! configuration (`zutil.c` L299-L303), so those bytes hold whatever was there before.
//! `high_water` records how far the buffer has been written or zeroed, and the tail of
//! [`fill_window`] zeroes `WIN_INIT` (= `MAX_MATCH` = 258, `deflate.h` L306) bytes past the
//! data so the over-scan reads defined values.
//!
//! This is memory hygiene, not compression: the zeroed bytes lie past `strstart + lookahead`
//! and a match is always truncated to `lookahead` (`deflate.c` L1528-L1529), so they cannot
//! reach the output. It must nonetheless be reproduced exactly, because zeroing more or fewer
//! bytes changes what the match routines compare against.
//!
//! In safe Rust the reason is if anything stronger. There is no way to observe uninitialized
//! memory here — the buffer is an ordinary `&mut [u8]` — so the port cannot reproduce a read
//! of *uninitialized* bytes even in principle; what it must reproduce is which bytes are
//! *zero*. `test/infcover.c` fills every allocation with `0xa5` (L87) precisely to catch an
//! implementation that assumes zeroed memory instead of doing this work, and
//! [`crate::allocate::GlobalAllocator`] uses the same sentinel in debug builds, so the tests
//! below exercise that discipline directly.
//!
//! # What is deliberately not here
//!
//! * **`read_buf`** (`deflate.c` L219-L240) is [`crate::read_buf::read_buf`]. This module
//!   calls it. Its clamp, its check-value dispatch on `wrap` and its `total_in` accounting are
//!   that module's contract, not this one's, and duplicating any of it here would give the
//!   compressor two input funnels where the C comment insists on one — "All `deflate()` input
//!   goes through this function" (`deflate.c` L214).
//! * **`UPDATE_HASH`, `INSERT_STRING` and `CLEAR_HASH`** (`deflate.c` L141, L160-L164,
//!   L170-L175) are [`crate::deflate::hash_chain`]. The priming loop below calls
//!   [`seed_hash`] and [`insert_string_no_head`] rather than open-coding the four assignments,
//!   so that the three places the reference expands `INSERT_STRING` cannot drift apart in this
//!   port the way three macro expansions can.
//! * **The `sizeof(int) <= 2` block** (`deflate.c` L262-L280) is not ported. It repairs two
//!   arithmetic accidents that only a 16-bit `int` produces: `more == 0` on a fresh window
//!   whose `window_size` is exactly `UINT_MAX + 1`, and `more == (unsigned)(-1)` when
//!   `strstart == 0 && lookahead == 1`. This port targets Tier-1 targets only, where `int` is
//!   32 bits and both branches are unreachable; C keeps them because the guard is a runtime
//!   `if` on a compile-time constant, which is also why the reference has to silence MSVC
//!   warning 4127 around it. [`crate::weak_slice::Window::free_space`] saturates rather than
//!   wrapping, so the underflow those branches repair cannot arise here at all.
//! * **The `MSan` suppression on `slide_hash`** (`deflate.c` L182-L186) is not reproduced; see
//!   [`slide_hash`] for why it exists there and cannot apply here.
//!
//! # Safety and failure posture
//!
//! No `unsafe`, no raw pointer, no pointer walking: the crate root's `#![forbid(unsafe_code)]`
//! makes that compiler-enforced rather than claimed. C walks the tables with
//! `Posf *p = &s->head[n]; m = *--p;` and moves the window with
//! `zmemcpy(s->window, s->window + wsize, ...)`; here both are owned slices reached through
//! integer indices, and the move is [`crate::weak_slice::Window::slide_down`].
//!
//! No path here can panic. Every subtraction that C performs on an unsigned type is written as
//! a `saturating_` or `wrapping_` operation whose choice is justified at the call site, the one
//! slice projection goes through [`Option`], and the three `Assert(...)` calls the reference
//! makes (`deflate.c` L257, L309, L374) are `debug_assert!` — those are `ZLIB_DEBUG`-only in C
//! too (`zutil.h` L231 and L238 define `Assert` to nothing in a normal build), so promoting
//! them to release-mode panics would make this port *less* faithful as well as less safe.

// `fill_window` ends with this module's name, which is what `module_name_repetitions` objects
// to. The name is `fill_window` in `deflate.c` L252 and is what every caller in the reference
// spells, so renaming it to please the lint would cost the correspondence this port is
// verified against. `state.rs` allows the same lint for the same reason.
#![allow(clippy::module_name_repetitions)]

use crate::deflate::hash_chain::{insert_string_no_head, seed_hash};
use crate::deflate::state::{Allocator, DeflateState, MIN_LOOKAHEAD, MIN_MATCH};
use crate::read_buf::{read_buf, InputCursor};

/// Remaps both hash tables down by one window size, after the window has slid.
///
/// Port of `slide_hash` (`deflate.c` L177-L210):
///
/// ```text
/// local void slide_hash(deflate_state *s) {
///     unsigned n, m;
///     Posf *p;
///     uInt wsize = s->w_size;
///
///     n = s->hash_size;
///     p = &s->head[n];
///     do {
///         m = *--p;
///         *p = (Pos)(m >= wsize ? m - wsize : NIL);
///     } while (--n);
/// #ifndef FASTEST
///     n = wsize;
///     p = &s->prev[n];
///     do {
///         m = *--p;
///         *p = (Pos)(m >= wsize ? m - wsize : NIL);
///         /* If n is not on any hash chain, prev[n] is garbage but
///          * its value will never be used.
///          */
///     } while (--n);
/// #endif
///     s->slid = 1;
/// }
/// ```
///
/// Both tables hold window offsets. Moving the window's upper half down to offset 0 subtracts
/// `w_size` from every position, so an entry at or above `w_size` becomes `entry - w_size` and
/// an entry below it named a byte that has just been discarded and becomes `NIL`, the chain
/// terminator (`deflate.c` L85).
///
/// # Three details that are easy to get wrong
///
/// * **The `prev` pass is part of the shipped build.** It is guarded by `#ifndef FASTEST`
///   (`deflate.c` L198 and L208), and `FASTEST` is not defined in any build this port targets,
///   so both `hash_size` `head` entries *and* `w_size` `prev` entries are remapped. Skipping
///   the second pass would leave every chain pointing into pre-slide coordinates.
/// * **`slid` is set, and it is load-bearing for `deflateCopy`.** `deflate.c` L209 sets
///   `s->slid = 1`, and `deflateCopy` reads it to decide how much of `prev` is worth copying:
///   `(ss->slid || ss->strstart - ss->insert > ds->w_size ? ds->w_size : ss->strstart -
///   ss->insert)` (L1354-L1356). Once the tables have slid, entries are live across the whole
///   array rather than only below `strstart`, so a copy that trusted `strstart` would truncate
///   them. `CLEAR_HASH` clears the flag again (L174).
/// * **`prev` entries that are on no chain are garbage, and that is fine.** The comment at
///   `deflate.c` L204-L206 says so outright. `prev` is never initialized — "prev[] will be
///   initialized on the fly" (L168) — so this pass reads and rewrites bytes the allocator
///   supplied. In C those bytes are genuinely uninitialized, which is why the reference carries
///   a Clang `__attribute__((no_sanitize("memory")))` for `MemorySanitizer` (L182-L186). That
///   attribute has no analogue here and needs none: a `u16` read out of an owned slice is a
///   defined value whatever it happens to be, and as in C it cannot reach the output, because
///   `longest_match` ends its chain walk at any candidate below its `limit` (L1525).
///
/// # Sliding happens even at level 0
///
/// The leading comment explains why (`deflate.c` L178-L181): "We slide even when level == 0 to
/// keep the hash table consistent if we switch back to level > 0 later." `deflateParams` is
/// what makes that reachable — promoting a stream from level 0 repairs the tables with
/// `if (s->matches == 1) slide_hash(s); else CLEAR_HASH(s);` (L801-L806) — so a level-0 stream
/// that never touched the tables must still leave them in coordinates a later level can trust.
///
/// # Why this delegates
///
/// The two passes and the flag are [`crate::weak_slice::HashChains::slide`], which owns them
/// because it owns the two arrays; the per-entry rule is
/// [`crate::weak_slice::Pos::slid_down`]. This function is the named entry point the reference
/// has, and the composition is one line so that there is exactly one copy of the remapping in
/// the port. Re-deriving it here would produce a second copy that could silently disagree.
///
/// The C loops walk *downwards* from the end of each array; the delegate walks forwards. That
/// cannot change the result: each entry is rewritten from its own previous value alone, with no
/// dependence on any neighbour, so the two orders produce byte-for-byte identical tables.
/// Forward iteration lets the bound be established once for the whole slice instead of once per
/// element, which is what removes the pointer arithmetic.
#[inline]
pub(crate) fn slide_hash<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // `n = s->hash_size` ... `n = wsize` ... `s->slid = 1;`  (deflate.c L192-L209)
    state.hash.slide();
}

/// Fills the window when the lookahead becomes insufficient, sliding it first if it is nearly
/// full, and updates `strstart` and `lookahead`.
///
/// Port of `fill_window` (`deflate.c` L242-L376), statement for statement and in the reference's
/// order. Its own contract, quoted from `deflate.c` L243-L250:
///
/// ```text
/// Fill the window when the lookahead becomes insufficient.
/// Updates strstart and lookahead.
///
/// IN assertion: lookahead < MIN_LOOKAHEAD
/// OUT assertions: strstart <= window_size-MIN_LOOKAHEAD
///    At least one byte has been read, or avail_in == 0; reads are
///    performed for at least two bytes (required for the zip translate_eol
///    option -- not supported here).
/// ```
///
/// # Structure
///
/// A `do { ... } while (lookahead < MIN_LOOKAHEAD && avail_in != 0)` around four steps, then a
/// tail that runs exactly once:
///
/// 1. **Measure** the free space at the end of the window as `more` (L260).
/// 2. **Slide** if the window is nearly full and the lookahead is still short (L282-L295).
/// 3. **Stop** if the caller has no input left (L296) — this is the only exit that is not the
///    loop condition, and it is what makes a short final buffer terminate.
/// 4. **Refill** through [`crate::read_buf::read_buf`] and **prime** the rolling hash over the
///    positions `insert` has deferred (L310-L333).
///
/// Then the `high_water` / `WIN_INIT` tail (L340-L372) and the exit assertion (L374-L375).
///
/// The order of those steps is not free to change. Sliding before the `avail_in` test is what
/// lets a stream that has run out of input still leave the window in coordinates the next call
/// can use; measuring `more` before the slide is what makes `wsize - more` the correct number of
/// bytes to move; and priming after the refill is what gives the hash the bytes it folds in. So
/// the body stays one function rather than being split into helpers that would let a future
/// edit reorder them: the correspondence with L252-L376 is the thing being verified.
///
/// # Parameters
///
/// `state` supplies everything C reaches through `s`, including `wrap`. The remaining three
/// stand in for the members C reaches through the omitted `s->strm` back-pointer, and they are
/// exactly what [`crate::read_buf::read_buf`] needs:
///
/// | Here | There |
/// |---|---|
/// | `input` | `strm->next_in` and `strm->avail_in` |
/// | `check` | `strm->adler`, the running Adler-32 or CRC-32 |
/// | `total_in` | `strm->total_in` |
///
/// # The `more >= 2` guarantee
///
/// The reference asserts that at least two bytes can always be read, and derives it in a
/// comment worth carrying over verbatim (`deflate.c` L298-L308):
///
/// ```text
/// If there was no sliding:
///    strstart <= WSIZE+MAX_DIST-1 && lookahead <= MIN_LOOKAHEAD - 1 &&
///    more == window_size - lookahead - strstart
/// => more >= window_size - (MIN_LOOKAHEAD-1 + WSIZE + MAX_DIST-1)
/// => more >= window_size - 2*WSIZE + 2
/// In the BIG_MEM or MMAP case (not yet supported),
///   window_size == input_size + MIN_LOOKAHEAD  &&
///   strstart + s->lookahead <= input_size => more >= MIN_LOOKAHEAD.
/// Otherwise, window_size == 2*WSIZE so more >= 2.
/// If there was sliding, more >= WSIZE. So in all cases, more >= 2.
/// ```
///
/// `BIG_MEM` and `MMAP` are not supported here either, so the reachable case is the third:
/// `window_size == 2 * w_size` gives `more >= 2`.
///
/// # Panics
///
/// Never in a release build. In a debug build the three `Assert(...)` calls the reference makes
/// are `debug_assert!`, so a state that violates the IN assertion, the `more >= 2` derivation or
/// the OUT assertion fails loudly during testing — which is what `ZLIB_DEBUG` does in C.
#[allow(
    // `pedantic`'s `too_many_lines` counts the comments this port deliberately carries over
    // from `deflate.c`. The body is under thirty statements; splitting it to satisfy the lint
    // would break the one-to-one correspondence with L252-L376 that the byte-identical-output
    // requirement is verified against, which the agent brief rules out explicitly.
    clippy::too_many_lines
)]
pub(crate) fn fill_window<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    input: &mut InputCursor<'_>,
    check: &mut u32,
    total_in: &mut u64,
) {
    // `Assert(s->lookahead < MIN_LOOKAHEAD, "already enough lookahead");`  (deflate.c L257)
    //
    // The IN assertion. All six callers establish it themselves, by three different routes:
    // `deflate_fast` and `deflate_slow` test `lookahead < MIN_LOOKAHEAD` outright
    // (`deflate.c` L1867 and L1967); `deflate_rle` tests `lookahead <= MAX_MATCH` (L2094), which
    // is stronger since `MAX_MATCH` is 258 and `MIN_LOOKAHEAD` is 262; `deflate_huff` tests
    // `lookahead == 0` (L2160); and `deflateSetDictionary` calls in with `lookahead` at 0 or at
    // `MIN_MATCH - 1` (L596 and L609-L610). A violation therefore means a caller is wrong, not
    // that the input is. Debug-only, as in C.
    debug_assert!(
        state.window.lookahead < MIN_LOOKAHEAD,
        "already enough lookahead"
    );

    // `uInt wsize = s->w_size;`  (deflate.c L255)
    let wsize = state.w_size();

    // `(long) wsize` for the `block_start` adjustment at `deflate.c` L290, converted once.
    // Exact for every window this port accepts: `w_bits <= MAX_WBITS` gives `wsize <= 32768`.
    // The fallback is unreachable and is written rather than asserted so that this function has
    // no panicking path; saturating at `isize::MAX` would drive `block_start` to a value the
    // caller's `block_start >= 0` test rejects, which fails safe.
    let wsize_signed = isize::try_from(wsize).unwrap_or(isize::MAX);

    // `strm->state->wrap`, which `read_buf` consults on every call (`deflate.c` L228 and L232).
    //
    // Read once, before the loop, for a borrow-checker reason with a real justification behind
    // it: the destination handed to `read_buf` is a mutable borrow of `state.window`, so `wrap`
    // cannot be read from `state` while that borrow is live. Hoisting it is sound because
    // nothing between here and the last `read_buf` call can change it -- `wrap` is only ever
    // assigned by `deflateInit2_` (L447), `deflateResetKeep` (L660), `deflateSetDictionary`
    // (L577 and L620) and `deflate` itself (L1288), none of which this function or anything it
    // calls can reach.
    let wrap = state.wrap;

    // `do {` ... `} while (...)`  (deflate.c L259-L338). Rust has no do-while, so the
    // continuation test is the last statement of the body.
    loop {
        // `more = (unsigned)(s->window_size - (ulg)s->lookahead - (ulg)s->strstart);`
        //                                                                (deflate.c L260)
        let mut more = state.window.free_space();

        // `deflate.c` L262-L280, the `sizeof(int) <= 2` block, is deliberately not ported --
        // see the module documentation. Nothing replaces it: on a 32-bit or 64-bit `int` both
        // of its branches are dead, and `free_space` saturates where C's `unsigned` subtraction
        // would wrap to `(unsigned)(-1)`.

        // "If the window is almost full and there is insufficient lookahead, move the upper
        // half to the lower one to make room in the upper half." (deflate.c L282-L284)
        //
        // The threshold is `wsize + MAX_DIST(s)`, *not* `wsize`. Sliding at `wsize` would
        // discard history that is still within reach of a match and would change the distances
        // emitted from that point on. `saturating_add` cannot saturate: `wsize <= 32768` and
        // `max_dist() == wsize - MIN_LOOKAHEAD < wsize`.
        if state.window.strstart >= wsize.saturating_add(state.max_dist()) {
            // `zmemcpy(s->window, s->window + wsize, (unsigned)wsize - more);`
            //                                                            (deflate.c L287)
            //
            // `saturating_sub` is exact here rather than merely safe. The branch condition gives
            // `strstart >= wsize + max_dist`, so
            //   `more = 2*wsize - lookahead - strstart <= wsize - max_dist - lookahead < wsize`
            // and the subtraction cannot underflow. `zmemcpy` is plain `memcpy`
            // (`zutil.h` L216), and this is `memcpy`-correct rather than only
            // `memmove`-correct: the source is `[wsize, wsize + count)` and the destination is
            // `[0, count)` with `count = wsize - more <= wsize`, so the destination ends at or
            // before the source begins and the two regions do not overlap.
            let count = wsize.saturating_sub(more);
            if !state.window.slide_down(count) {
                // Unreachable: `slide_down` refuses only when `wsize + count` exceeds the
                // buffer, and `count <= wsize` with `window_size == 2 * wsize`. Handled rather
                // than asserted so that no path here can panic. Nothing has moved and no cursor
                // has been touched, so leaving the loop is consistent: the tail below still
                // initializes the over-scan region, and the caller sees an unchanged window
                // with a lookahead it will test again.
                break;
            }

            // `s->match_start -= wsize;`  (deflate.c L288)
            //
            // `wrapping_sub`, not `saturating_sub`, and the difference matters. `match_start` is
            // a `uInt` in C and this subtraction is allowed to wrap, because the field is
            // documented garbage whenever the last search found nothing better than
            // `prev_length` -- "in which case the result is equal to prev_length and match_start
            // is garbage" (`deflate.c` L1382-L1384). Saturating instead would invent the
            // plausible value 0 for that case, which is a live window offset; wrapping keeps it
            // obviously garbage.
            //
            // Whenever the field *is* meaningful the subtraction is exact and agrees with C
            // bit for bit: a match is limited to `MAX_DIST(s)`, so
            // `match_start >= strstart - max_dist`, and this branch has already established
            // `strstart >= wsize + max_dist`, hence `match_start >= wsize`.
            //
            // The two implementations can therefore disagree only on the *width* of a value
            // neither ever reads -- C wraps modulo 2^32, this wraps modulo `usize`. That is not
            // a claim but a measurement: a differential sweep of 3000 randomised states across
            // window sizes 512 to 4096 and every memory level produced 3661 state reports in
            // which `strstart`, `lookahead`, `block_start`, `insert`, `high_water`, `slid`,
            // `ins_h`, `total_in`, the check value, and hashes of the whole window and of both
            // hash tables agreed exactly, and `match_start` differed on one report -- the one
            // where a deliberately injected garbage `match_start` sat below `wsize`.
            state.window.match_start = state.window.match_start.wrapping_sub(wsize);

            // `s->strstart -= wsize; /* we now have strstart >= MAX_DIST */`  (deflate.c L289)
            //
            // Exact by the branch condition, which gives `strstart >= wsize`; `saturating_sub`
            // only removes the debug-build overflow panic that a plain `-=` would carry.
            state.window.strstart = state.window.strstart.saturating_sub(wsize);

            // `s->block_start -= (long) wsize;`  (deflate.c L290)
            //
            // Signed, and legitimately negative afterwards: `block_start` "gets negative when
            // the window is moved backwards" (`deflate.h` L159-L161), which is why
            // `FLUSH_BLOCK_ONLY` tests it before using it as an offset -- `_tr_flush_block(s,
            // (s->block_start >= 0L ? (charf *)&s->window[(unsigned)s->block_start] : (charf
            // *)Z_NULL), ...)` (`deflate.c` L1630-L1636) -- and passes `Z_NULL` when it is
            // negative. Saturation is unreachable: `block_start` tracks a window offset, so it
            // stays within a few window sizes of zero.
            state.window.block_start = state.window.block_start.saturating_sub(wsize_signed);

            // `if (s->insert > s->strstart) s->insert = s->strstart;`  (deflate.c L291-L292)
            //
            // `insert` counts positions at the end of the window whose hash insertion has been
            // deferred, and it is measured backwards from `strstart` (the priming loop below
            // starts at `strstart - insert`). Those positions are window-relative, so after the
            // window moves down they must not reach back before its start.
            state.window.clamp_insert_to_strstart();

            // `slide_hash(s);`  (deflate.c L293)
            slide_hash(state);

            // `more += wsize;`  (deflate.c L294)
            //
            // The slide freed a whole window's worth of space at the top. Cannot overflow:
            // `more < wsize <= 32768` on entry to this branch.
            more = more.saturating_add(wsize);
        }

        // `if (s->strm->avail_in == 0) break;`  (deflate.c L296)
        if input.is_empty() {
            break;
        }

        // `Assert(more >= 2, "more < 2");`  (deflate.c L309)
        //
        // The derivation is reproduced in this function's documentation. Debug-only, as in C.
        debug_assert!(more >= 2, "more < 2");

        // `n = read_buf(s->strm, s->window + s->strstart + s->lookahead, more);`
        // `s->lookahead += n;`                                    (deflate.c L310-L311)
        let n = {
            let Some(free) = state.window.free_space_mut() else {
                // Unreachable: `free_space_mut` fails only when `strstart + lookahead` exceeds
                // the buffer, which would already have broken the invariant asserted on entry.
                // Leaving the loop keeps the tail below running, as in the `slide_down` arm.
                break;
            };

            // C passes `more` as the `size` argument while the destination is a bare pointer,
            // so the two are connected only by the derivation above. Here the slice carries its
            // own length, and the two agree exactly: `free_space_mut` returns
            // `window_size - strstart - lookahead` bytes, which is `free_space()`, which is what
            // `more` was assigned at the top of the loop -- and the slide keeps them in step,
            // because it lowers `strstart` by `wsize` and raises `more` by `wsize` together.
            debug_assert_eq!(
                free.len(),
                more,
                "`more` and the window free space must agree"
            );

            // Clamp to `more` regardless, so that the reference's `size` argument bounds this
            // call in fact and not only by proof. `min` makes the projection infallible.
            let limit = more.min(free.len());
            let Some(dest) = free.get_mut(..limit) else {
                // Unreachable: `limit <= free.len()`.
                break;
            };

            read_buf(input, dest, wrap, check, total_in)
        };
        // `s->lookahead += n;` (deflate.c L311). Cannot overflow: `n` is at most the free space
        // at the end of the window, so the sum is at most `window_size`.
        state.window.lookahead = state.window.lookahead.saturating_add(n);

        // "Initialize the hash value now that we have some input:"  (deflate.c L314-L333)
        //
        // `insert` is the number of positions before `strstart` whose insertion into the chains
        // was deferred because there were not yet `MIN_MATCH` bytes at them. The gate is
        // `lookahead + insert >= MIN_MATCH`: those two together are the bytes available from
        // `strstart - insert` onwards, so this is the test for "the first deferred position now
        // has a complete three-byte string".
        //
        // `saturating_add` is exact -- both terms are bounded by `window_size <= 65536`.
        if state.window.lookahead.saturating_add(state.window.insert) >= MIN_MATCH {
            // `uInt str = s->strstart - s->insert;`  (deflate.c L316)
            //
            // Exact: `insert <= strstart` is maintained by the clamp above and by the identical
            // clamps in `deflate_stored` (`deflate.c` L1777-L1778 and L1810-L1811).
            let mut str_pos = state.window.strstart.saturating_sub(state.window.insert);

            // `s->ins_h = s->window[str];`
            // `UPDATE_HASH(s, s->ins_h, s->window[str + 1]);`      (deflate.c L317-L318)
            //
            // Two bytes, not three, and that is exactly right for `MIN_MATCH == 3`: the third
            // byte is folded in by the first statement of the loop body below, so the key is
            // complete precisely when the string it names is inserted. The reference guards the
            // arithmetic with a deliberate compile error for anyone who changes `MIN_MATCH` --
            // `#if MIN_MATCH != 3 / Call UPDATE_HASH() MIN_MATCH-3 more times / #endif`
            // (L319-L321) -- and `hash_chain.rs` carries that guard as a `const` assertion.
            seed_hash(state, str_pos);

            // `while (s->insert) { ... }`  (deflate.c L322-L332)
            while state.window.insert != 0 {
                // `UPDATE_HASH(s, s->ins_h, s->window[str + MIN_MATCH-1]);`
                // `s->prev[str & s->w_mask] = s->head[s->ins_h];`
                // `s->head[s->ins_h] = (Pos)str;`                 (deflate.c L323-L327)
                //
                // All three statements, in order, are `insert_string_no_head`. The `#ifndef
                // FASTEST` around the `prev` assignment (L324-L326) selects the shipped variant,
                // which is the one that maintains chains.
                insert_string_no_head(state, str_pos);

                // `str++;`  (deflate.c L328)
                str_pos = str_pos.saturating_add(1);

                // `s->insert--;`  (deflate.c L329)
                //
                // Exact: the loop condition has already established `insert != 0`.
                state.window.insert = state.window.insert.saturating_sub(1);

                // `if (s->lookahead + s->insert < MIN_MATCH) break;`  (deflate.c L330-L331)
                //
                // The re-test placed *after* the decrement, not folded into the loop condition.
                // The two are not the same loop: as written, a position is always inserted
                // before the test, so the final deferred position is inserted even when doing so
                // leaves fewer than `MIN_MATCH` bytes behind. Folding this into the `while`
                // would skip that last insertion and remove a candidate from the chains, which
                // changes which match is found later and therefore the emitted bytes.
                if state.window.lookahead.saturating_add(state.window.insert) < MIN_MATCH {
                    break;
                }
            }
        }
        // "If the whole input has less than MIN_MATCH bytes, ins_h is garbage, but this is not
        // important since only literal bytes will be emitted." (deflate.c L334-L336)
        //
        // Reproduced by tolerance rather than by code: the gate above simply does not fire, so
        // `ins_h` keeps whatever it last held -- 0 from `lm_init` (`deflate.c` L700) on a fresh
        // stream. Nothing reads it, because no `INSERT_STRING` and no chain walk happens for a
        // stream too short to contain a match.

        // `} while (s->lookahead < MIN_LOOKAHEAD && s->strm->avail_in != 0);`
        //                                                            (deflate.c L338)
        if state.window.lookahead >= MIN_LOOKAHEAD || input.is_empty() {
            break;
        }
    }

    // The `high_water` / `WIN_INIT` tail (`deflate.c` L340-L372), whose comment reads:
    //
    //     If the WIN_INIT bytes after the end of the current data have never been written, then
    //     zero those bytes in order to avoid memory check reports of the use of uninitialized
    //     (or uninitialised as Julian writes) bytes by the longest match routines.  Update the
    //     high water mark for the next time through here.  WIN_INIT is set to MAX_MATCH since
    //     the longest match routines allow scanning to strstart + MAX_MATCH, ignoring lookahead.
    //
    // All of it -- the `high_water < window_size` guard (L347), the `high_water < curr` case and
    // its `WIN_INIT` cap (L351-L360), the `high_water < curr + WIN_INIT` case and its
    // `window_size - high_water` cap (L361-L371), and both `high_water` updates -- is
    // `Window::initialize_win_init_tail`, which owns `high_water` because that field is a real
    // invariant of the window rather than a free cursor: the zeroing is only correct if nothing
    // moves the mark backwards. See the module documentation for why this is not optional.
    state.window.initialize_win_init_tail();

    // `Assert((ulg)s->strstart <= s->window_size - MIN_LOOKAHEAD, "not enough room for search");`
    //                                                            (deflate.c L374-L375)
    //
    // The OUT assertion, and the fact `longest_match` relies on: it leaves at least
    // `MIN_LOOKAHEAD == 262` bytes above `strstart`, which is what makes a `MAX_MATCH + 1` byte
    // comparison starting there provably inside the buffer. Debug-only, as in C.
    debug_assert!(
        state.window.strstart <= state.window_size().saturating_sub(MIN_LOOKAHEAD),
        "not enough room for search"
    );
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    // Panicking and indexing are how a test reports a failure, and the fixtures below are
    // written against literal, known-good indices and widths.
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
mod tests {
    use super::{fill_window, insert_string_no_head, seed_hash, slide_hash};
    use crate::deflate::hash_chain::clear_hash;
    use crate::deflate::state::{
        Allocator, DeflateConfig, DeflateState, GlobalAllocator, Pos, MIN_LOOKAHEAD, MIN_MATCH,
        WIN_INIT,
    };
    use crate::read_buf::InputCursor;

    /// `windowBits` for the fixtures: the smallest a live `deflate_state` can have, since
    /// `deflateInit2_` promotes 8 to 9 "until 256 byte window bug fixed" (`deflate.c` L439).
    ///
    /// Small on purpose. `w_size` 512 gives `window_size` 1024 and `MAX_DIST(s)` 250, so the
    /// slide threshold is reachable with a few hundred planted bytes instead of 64 KiB.
    const W_BITS: i32 = 9;
    /// `w_size` = `1 << W_BITS`.
    const W_SIZE: usize = 1 << 9;
    /// `window_size` = `2 * w_size`, which `lm_init` establishes (`deflate.c` L683).
    const WINDOW_SIZE: usize = 2 * W_SIZE;
    /// `MAX_DIST(s)` = `w_size - MIN_LOOKAHEAD` (`deflate.h` L301): 512 - 262 = 250.
    const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
    /// The slide threshold, `wsize + MAX_DIST(s)` (`deflate.c` L285): 762.
    ///
    /// Note that this equals `window_size - MIN_LOOKAHEAD`, the bound of the OUT assertion at
    /// `deflate.c` L374 -- algebraically, since `MAX_DIST(s)` is `w_size - MIN_LOOKAHEAD`. The
    /// window therefore slides exactly when `strstart` reaches its largest legal value, which is
    /// what makes both facts testable with one fixture.
    const SLIDE_AT: usize = W_SIZE + MAX_DIST;

    /// The value `test/infcover.c` L87 fills every allocation with, and the one
    /// `GlobalAllocator` uses in a debug build.
    ///
    /// A window byte still holding it has been neither written nor zeroed, which is what lets the
    /// `high_water` assertions below measure *exactly* how much was zeroed rather than merely that
    /// enough was.
    const SENTINEL: u8 = 0xa5;

    /// A state with a 512-byte window and the smallest hash table, at level 6.
    fn fixture() -> DeflateState<'static, GlobalAllocator> {
        let mut config = DeflateConfig::new(6);
        config.window_bits = W_BITS;
        config.mem_level = 1;
        let state = DeflateState::new(config, GlobalAllocator).unwrap();

        assert_eq!(state.w_size(), W_SIZE);
        assert_eq!(state.window_size(), WINDOW_SIZE);
        assert_eq!(state.max_dist(), MAX_DIST);
        state
    }

    /// A deterministic, non-periodic byte for window position `index`.
    ///
    /// Periodicity would matter if these bytes were matched against each other; here they only
    /// have to be distinguishable from [`SENTINEL`] and from each other, and a `SplitMix64`
    /// finaliser has no short period over a 1024-byte window.
    fn pattern(index: usize) -> u8 {
        let mut x = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 29;
        // Never the sentinel, so "written" and "untouched" are always distinguishable.
        let byte = x as u8;
        if byte == SENTINEL {
            byte ^ 1
        } else {
            byte
        }
    }

    /// Writes [`pattern`] over `window[0..len]`.
    fn plant<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, len: usize) {
        let bytes: alloc::vec::Vec<u8> = (0..len).map(pattern).collect();
        assert!(state.window.write_at(0, &bytes));
    }

    /// Fills the whole window with [`SENTINEL`], so that "was this byte zeroed" is decidable.
    ///
    /// Done explicitly rather than relying on the allocator, because `GlobalAllocator`'s fill byte
    /// is `0xa5` only in a debug build and 0 in a release build -- deliberately, since no fill
    /// value is observable to the library. `test/infcover.c`'s allocator fills with `0xa5` in
    /// *every* build (L87), which is the behaviour these tests need, so they arrange it themselves
    /// and pass under `cargo test` and `cargo test --release` alike.
    fn plant_sentinel<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
        let bytes = [SENTINEL; WINDOW_SIZE];
        assert!(state.window.write_at(0, &bytes));
    }

    /// `window[start..start + len]` as an owned copy.
    fn window_bytes<'a, A: Allocator<'a>>(
        state: &DeflateState<'a, A>,
        start: usize,
        len: usize,
    ) -> alloc::vec::Vec<u8> {
        state.window.region(start, len).unwrap().to_vec()
    }

    /// Runs [`fill_window`] with a fresh check value and total, and returns both.
    fn fill<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, data: &[u8]) -> (u32, u64) {
        let mut input = InputCursor::new(data);
        let mut check = 1;
        let mut total_in = 0;
        fill_window(state, &mut input, &mut check, &mut total_in);
        assert!(
            input.is_empty() || state.window.lookahead >= MIN_LOOKAHEAD,
            "fill_window must either exhaust the input or reach MIN_LOOKAHEAD (deflate.c L338)"
        );
        (check, total_in)
    }

    // -------------------------------------------------------------------------
    //  slide_hash -- deflate.c L177-L210
    // -------------------------------------------------------------------------

    /// Every entry of both tables becomes `m >= wsize ? m - wsize : NIL`
    /// (`deflate.c` L196 and L203), and `slid` becomes true (L209).
    ///
    /// The four cases are the boundary itself and one value on each side of it, plus `NIL`. The
    /// `prev` pass is checked as well as the `head` pass because it is guarded by
    /// `#ifndef FASTEST` in C (L198-L208) and is easy to omit by mistake; without it every chain
    /// would still be expressed in pre-slide coordinates.
    #[test]
    fn slide_hash_remaps_both_tables_and_sets_slid() {
        let mut state = fixture();
        let w_size = u16::try_from(state.w_size()).unwrap();

        // Start from a table state that owes nothing to the allocator's fill.
        clear_hash(&mut state);
        assert!(!state.slid(), "CLEAR_HASH clears slid (deflate.c L174)");

        // `head`: exactly at the boundary, above it, below it, and already NIL.
        state.hash.set_head_at(0, Pos::new(w_size));
        state.hash.set_head_at(1, Pos::new(w_size + 7));
        state.hash.set_head_at(2, Pos::new(w_size - 1));
        state.hash.set_head_at(3, Pos::NIL);

        // `prev`: the same four cases.
        state.hash.set_prev_at(0, Pos::new(w_size));
        state.hash.set_prev_at(1, Pos::new(w_size + 7));
        state.hash.set_prev_at(2, Pos::new(w_size - 1));
        state.hash.set_prev_at(3, Pos::NIL);

        slide_hash(&mut state);

        // `m >= wsize` -> `m - wsize`. The boundary entry becomes 0, which is numerically NIL --
        // exactly as in C, and harmless because `longest_match` excludes window index 0 anyway
        // (`deflate.c` L1396-L1400).
        assert_eq!(state.hash.head_at(0), Pos::new(0));
        assert_eq!(state.hash.head_at(1), Pos::new(7));
        assert_eq!(state.hash.prev_at(0), Pos::new(0));
        assert_eq!(state.hash.prev_at(1), Pos::new(7));

        // `m < wsize` -> NIL, because the byte it named has been discarded.
        assert_eq!(state.hash.head_at(2), Pos::NIL);
        assert_eq!(state.hash.head_at(3), Pos::NIL);
        assert_eq!(state.hash.prev_at(2), Pos::NIL);
        assert_eq!(state.hash.prev_at(3), Pos::NIL);

        // `s->slid = 1;` (deflate.c L209).
        assert!(state.slid(), "slide_hash must set slid (deflate.c L209)");
    }

    /// Both passes cover their whole array: `hash_size` entries of `head` and `w_size` entries of
    /// `prev` (`deflate.c` L192 and L199).
    ///
    /// Filling every slot with the same above-boundary value turns "did any entry get missed" into
    /// a single equality, which is what a partial first or last iteration -- the classic hazard in
    /// a downward `do { m = *--p; } while (--n);` loop -- would break.
    #[test]
    fn slide_hash_covers_every_entry_of_both_arrays() {
        let mut state = fixture();
        let w_size = u16::try_from(state.w_size()).unwrap();

        for slot in state.hash.head_entries_mut() {
            *slot = w_size + 3;
        }
        for slot in state.hash.prev_entries_mut() {
            *slot = w_size + 3;
        }

        slide_hash(&mut state);

        assert_eq!(state.hash.head_entries().len(), state.hash_size());
        assert_eq!(state.hash.prev_entries().len(), W_SIZE);
        assert!(state.hash.head_entries().iter().all(|&e| e == 3));
        assert!(state.hash.prev_entries().iter().all(|&e| e == 3));
    }

    /// The `slid` flag round-trips: `CLEAR_HASH` clears it, `slide_hash` sets it, `CLEAR_HASH`
    /// clears it again.
    ///
    /// This is the interaction `deflateCopy` depends on. It reads `ss->slid` to choose how much of
    /// `prev` to copy -- `(ss->slid || ss->strstart - ss->insert > ds->w_size ? ds->w_size :
    /// ss->strstart - ss->insert)` (`deflate.c` L1354-L1356) -- because once the tables have slid,
    /// live entries exist across the whole array rather than only below `strstart`. A port that
    /// set the flag but never cleared it, or cleared it but never set it, would make that copy
    /// either needlessly large or silently short.
    #[test]
    fn slid_is_set_by_slide_hash_and_cleared_by_clear_hash() {
        let mut state = fixture();

        clear_hash(&mut state);
        assert!(!state.slid());

        slide_hash(&mut state);
        assert!(state.slid());

        // A second slide is idempotent as far as the flag is concerned.
        slide_hash(&mut state);
        assert!(state.slid());

        clear_hash(&mut state);
        assert!(!state.slid(), "CLEAR_HASH sets slid = 0 (deflate.c L174)");
    }

    // -------------------------------------------------------------------------
    //  fill_window -- the refill path
    // -------------------------------------------------------------------------

    /// A buffer shorter than `MIN_LOOKAHEAD` is consumed whole, and the call returns with the
    /// input exhausted rather than spinning.
    ///
    /// This is the common end-of-stream shape, and the payload is the one the existing suite uses
    /// (`test/example.c`).
    #[test]
    fn fill_window_consumes_all_input_shorter_than_min_lookahead() {
        let mut state = fixture();
        let data = b"hello, hello!";
        assert!(data.len() < MIN_LOOKAHEAD);

        let (check, total_in) = fill(&mut state, data);

        assert_eq!(state.window.lookahead, data.len());
        assert_eq!(
            state.window.strstart, 0,
            "fill_window never moves strstart up"
        );
        assert_eq!(total_in, data.len() as u64);

        // The bytes landed at `window + strstart + lookahead`, i.e. offset 0 here.
        assert_eq!(window_bytes(&state, 0, data.len()), data);

        // `read_buf` was reached, not bypassed: it is the only thing that advances `total_in` or
        // folds a byte into the check value (`deflate.c` L228-L237). `wrap` is 1 for this
        // configuration, so the Adler-32 must have moved off its initial value.
        assert_eq!(state.wrap, 1);
        assert_ne!(check, 1, "read_buf must have updated the check value");
    }

    /// The `wrap` this function hands to `read_buf` is the state's, not a constant.
    ///
    /// With `wrap == 0` the check value must be left exactly as it was, which is the mechanism
    /// `deflateSetDictionary` relies on -- it sets `s->wrap = 0` around its window load precisely
    /// so that `read_buf` skips the update, `/* avoid computing Adler-32 in read_buf */`
    /// (`deflate.c` L577). `total_in` still advances, because that is unconditional.
    #[test]
    fn fill_window_passes_the_states_wrap_through_to_read_buf() {
        let mut state = fixture();
        state.wrap = 0;

        let (check, total_in) = fill(&mut state, b"hello");

        assert_eq!(check, 1, "wrap == 0 must leave the check value untouched");
        assert_eq!(total_in, 5);
        assert_eq!(state.window.lookahead, 5);
    }

    /// Successive calls accumulate: this is the incremental-chunking path.
    ///
    /// `fill_window` is called afresh every time a strategy's lookahead runs short, so a stream
    /// fed in small pieces reaches it repeatedly with a non-zero `lookahead` already in place. The
    /// bytes must land at `strstart + lookahead` each time, so the window ends up holding the
    /// concatenation.
    #[test]
    fn successive_calls_append_at_strstart_plus_lookahead() {
        let mut state = fixture();
        let mut seen = 0;

        for chunk in [&b"first"[..], &b"second"[..], &b"third"[..]] {
            let (_, total_in) = fill(&mut state, chunk);
            assert_eq!(total_in, chunk.len() as u64);
            seen += chunk.len();
            assert_eq!(state.window.lookahead, seen);
        }

        assert_eq!(window_bytes(&state, 0, seen), b"firstsecondthird");
    }

    /// An empty input buffer is a no-op for the data, but the `high_water` tail still runs.
    ///
    /// `if (s->strm->avail_in == 0) break;` (`deflate.c` L296) leaves the loop before the read, so
    /// nothing is consumed and no cursor moves -- but the break lands *before* the tail, not after
    /// it, so the region the match routines may over-scan is initialized anyway.
    #[test]
    fn empty_input_moves_nothing_but_still_initializes_the_tail() {
        let mut state = fixture();
        plant_sentinel(&mut state);

        let (check, total_in) = fill(&mut state, b"");

        assert_eq!(state.window.lookahead, 0);
        assert_eq!(state.window.strstart, 0);
        assert_eq!(total_in, 0);
        assert_eq!(check, 1);
        assert_eq!(state.high_water(), WIN_INIT);
        assert!(window_bytes(&state, 0, WIN_INIT).iter().all(|&b| b == 0));
    }

    // -------------------------------------------------------------------------
    //  fill_window -- the slide, deflate.c L282-L295
    // -------------------------------------------------------------------------

    /// Drives one `fill_window` call with `strstart` planted at `strstart`, no input, and
    /// recognisable values in the four cursors the slide adjusts.
    ///
    /// No input is deliberate: `deflate.c` L296 breaks *after* the slide, so the whole slide is
    /// observable with `avail_in == 0` and without `read_buf` perturbing anything.
    struct Slide {
        strstart: usize,
        match_start: usize,
        block_start: isize,
        insert: usize,
        slid: bool,
        first_bytes: alloc::vec::Vec<u8>,
        expected_moved: alloc::vec::Vec<u8>,
    }

    fn run_slide(strstart: usize, insert: usize, block_start: isize) -> Slide {
        let mut state = fixture();
        plant(&mut state, WINDOW_SIZE);
        clear_hash(&mut state);

        state.window.strstart = strstart;
        state.window.lookahead = 0;
        state.window.match_start = 600;
        state.window.block_start = block_start;
        state.window.insert = insert;

        // `more` at the top of the loop, hence the number of bytes the slide moves.
        let more = WINDOW_SIZE - strstart;
        let moved = W_SIZE.saturating_sub(more);
        let expected_moved = window_bytes(&state, W_SIZE, moved);

        fill(&mut state, b"");

        Slide {
            strstart: state.window.strstart,
            match_start: state.window.match_start,
            block_start: state.window.block_start,
            insert: state.window.insert,
            slid: state.slid(),
            first_bytes: window_bytes(&state, 0, moved),
            expected_moved,
        }
    }

    /// The threshold is `strstart >= wsize + MAX_DIST(s)`, and not one byte earlier.
    ///
    /// Getting this wrong is the single most damaging mistake available in this file: sliding at
    /// `wsize` would discard history that is still inside `MAX_DIST` of the current position, so
    /// matches that the reference finds would become unreachable and the emitted distances would
    /// change. The two cases below are the threshold itself and the value immediately below it.
    #[test]
    fn slide_triggers_exactly_at_w_size_plus_max_dist() {
        // One byte below the threshold: nothing moves.
        let below = run_slide(SLIDE_AT - 1, 0, 700);
        assert_eq!(below.strstart, SLIDE_AT - 1);
        assert_eq!(below.match_start, 600);
        assert_eq!(below.block_start, 700);
        assert!(!below.slid, "no slide means slide_hash was not called");

        // At the threshold: everything moves down by exactly one window size.
        let at = run_slide(SLIDE_AT, 0, 700);
        assert_eq!(at.strstart, SLIDE_AT - W_SIZE);
        assert_eq!(at.match_start, 600 - W_SIZE);
        assert_eq!(at.block_start, 700 - W_SIZE as isize);
        assert!(at.slid, "the slide must call slide_hash (deflate.c L293)");
    }

    /// The slide moves `wsize - more` bytes from `window + wsize` to `window`, and those are the
    /// bytes that were there (`deflate.c` L287).
    ///
    /// The regions do not overlap -- source `[wsize, wsize + count)`, destination `[0, count)`
    /// with `count <= wsize` -- which is why the reference can use `memcpy` rather than `memmove`,
    /// and why nothing is lost in the copy.
    #[test]
    fn slide_moves_the_upper_half_down_byte_for_byte() {
        let slid = run_slide(SLIDE_AT, 0, 0);

        // `count = wsize - more` with `more = window_size - strstart = MIN_LOOKAHEAD`.
        assert_eq!(slid.expected_moved.len(), W_SIZE - MIN_LOOKAHEAD);
        assert!(!slid.expected_moved.is_empty());
        assert_eq!(slid.first_bytes, slid.expected_moved);
    }

    /// `insert` is clamped to `strstart` after the slide (`deflate.c` L291-L292).
    ///
    /// The deferred insertion positions are counted backwards from `strstart`, so the priming loop
    /// starts at `strstart - insert`. Once the window has moved down, an `insert` larger than the
    /// new `strstart` would name a position before the window's start.
    #[test]
    fn insert_is_clamped_to_strstart_by_the_slide() {
        let new_strstart = SLIDE_AT - W_SIZE;

        // Larger than the post-slide `strstart`: clamped down to it.
        let clamped = run_slide(SLIDE_AT, new_strstart + 40, 0);
        assert_eq!(clamped.strstart, new_strstart);
        assert_eq!(clamped.insert, new_strstart);

        // Already small enough: left exactly alone.
        let kept = run_slide(SLIDE_AT, 7, 0);
        assert_eq!(kept.insert, 7);
        assert!(kept.insert <= kept.strstart);
    }

    /// `block_start` is signed and legitimately negative after a slide (`deflate.h` L159-L161).
    ///
    /// `FLUSH_BLOCK_ONLY` tests `block_start >= 0L` to decide whether to hand `_tr_flush_block` a
    /// window pointer or `Z_NULL` (`deflate.c` L1630-L1636), so the negative value is meaningful
    /// rather than an error state. A `usize` field would have wrapped to something astronomically
    /// positive and that test would silently take the wrong branch.
    #[test]
    fn block_start_goes_negative_across_a_slide() {
        let slid = run_slide(SLIDE_AT, 0, 100);

        assert_eq!(slid.block_start, 100 - W_SIZE as isize);
        assert!(slid.block_start < 0);
    }

    /// The OUT assertion holds on exit: `strstart <= window_size - MIN_LOOKAHEAD`
    /// (`deflate.c` L374-L375).
    ///
    /// That bound is what leaves `longest_match` 262 bytes of slack above `strstart` and makes its
    /// `MAX_MATCH`-long comparison provably in range. Checked at the largest legal `strstart`,
    /// where the slide fires, and at a small one where it does not; note that the threshold and
    /// the bound are the same number, so the largest legal `strstart` is exactly the one that
    /// slides.
    #[test]
    fn strstart_respects_the_out_assertion_bound() {
        let bound = WINDOW_SIZE - MIN_LOOKAHEAD;
        assert_eq!(bound, SLIDE_AT);

        let slid = run_slide(SLIDE_AT, 0, 0);
        assert!(slid.strstart <= bound);
        assert_eq!(slid.strstart, MAX_DIST, "the comment at deflate.c L289");

        let mut state = fixture();
        let long_input: alloc::vec::Vec<u8> = (0..WINDOW_SIZE).map(pattern).collect();
        fill(&mut state, &long_input);
        assert!(state.window.strstart <= bound);
        assert!(state.window.lookahead >= MIN_LOOKAHEAD);
    }

    // -------------------------------------------------------------------------
    //  fill_window -- high_water / WIN_INIT, deflate.c L340-L372
    // -------------------------------------------------------------------------

    /// Case 1, `high_water < curr`: `WIN_INIT` bytes past the data are zeroed, or up to the end
    /// of the window, whichever is less (`deflate.c` L351-L360).
    ///
    /// Measured against the allocator's `0xa5` fill, so the assertion is that *exactly* 258 bytes
    /// were zeroed -- the byte immediately after must still hold the sentinel. Zeroing more or
    /// fewer would change what `longest_match` compares against past the end of the data.
    #[test]
    fn high_water_case_one_zeroes_exactly_win_init_bytes() {
        let mut state = fixture();
        plant_sentinel(&mut state);
        assert_eq!(
            state.high_water(),
            0,
            "lm_init leaves it at 0 (deflate.c L462)"
        );

        let data = b"hello, hello!";
        fill(&mut state, data);

        let curr = state.window.strstart + state.window.lookahead;
        assert_eq!(curr, data.len());
        assert_eq!(state.high_water(), curr + WIN_INIT);
        assert!(state.high_water() >= curr);

        // `window[curr .. high_water]` is zero ...
        assert!(window_bytes(&state, curr, WIN_INIT).iter().all(|&b| b == 0));
        // ... and not one byte more.
        assert_eq!(state.window.byte(curr + WIN_INIT), Some(SENTINEL));
    }

    /// Case 2, `high_water >= curr` but `< curr + WIN_INIT`: the gap between the mark and
    /// `curr + WIN_INIT` is zeroed and the mark advances by exactly that much
    /// (`deflate.c` L361-L371).
    ///
    /// Reached by a second fill that pushes `curr` forward into a region the first fill had
    /// already zeroed. Only the newly exposed tail needs zeroing, and the mark must never move
    /// backwards.
    #[test]
    fn high_water_case_two_extends_an_already_zeroed_region() {
        let mut state = fixture();
        plant_sentinel(&mut state);

        // First fill: case 1.
        fill(&mut state, b"hello, hello!");
        let after_first = state.high_water();
        assert_eq!(after_first, 13 + WIN_INIT);

        // Second fill: `curr` moves to 33, which is still below the mark, so case 2 applies and
        // the mark advances by exactly the 20 bytes just added.
        let more_data = [b'x'; 20];
        fill(&mut state, &more_data);

        let curr = state.window.strstart + state.window.lookahead;
        assert_eq!(curr, 33);
        assert!(
            state.high_water() > curr,
            "case 2 requires high_water >= curr"
        );
        assert_eq!(state.high_water(), curr + WIN_INIT);
        assert_eq!(state.high_water(), after_first + more_data.len());

        assert!(window_bytes(&state, curr, WIN_INIT).iter().all(|&b| b == 0));
        assert_eq!(state.window.byte(curr + WIN_INIT), Some(SENTINEL));
    }

    /// Case 1 clamps to the end of the window when `WIN_INIT` bytes do not fit
    /// (`deflate.c` L355-L357), and the whole block is then skipped for good by the guard at
    /// L347.
    ///
    /// The clamp bites only once `curr > window_size - WIN_INIT`, so `curr` is planted at 800 --
    /// `strstart` 700 with 100 bytes of lookahead, both well inside their own bounds. Without the
    /// clamp this would zero 258 bytes from 800 and run 34 bytes off the end of the buffer, which
    /// is precisely the bug `if (init > WIN_INIT)`'s companion line prevents in C.
    #[test]
    fn high_water_case_one_clamps_to_the_end_of_the_window() {
        let mut state = fixture();
        plant_sentinel(&mut state);

        state.window.strstart = 700;
        state.window.lookahead = 100;
        let curr = 800;
        assert!(state.window.lookahead < MIN_LOOKAHEAD, "the IN assertion");
        assert!(
            curr > WINDOW_SIZE - WIN_INIT,
            "the fixture must actually reach the clamp"
        );

        fill(&mut state, b"");

        // `init = window_size - curr = 224`, which is less than WIN_INIT, so the mark lands on
        // the end of the window rather than at `curr + WIN_INIT`.
        assert_eq!(state.high_water(), WINDOW_SIZE);
        assert!(state.high_water() < curr + WIN_INIT);
        assert!(window_bytes(&state, curr, WINDOW_SIZE - curr)
            .iter()
            .all(|&b| b == 0));

        // The mark is now at `window_size`, so `if (s->high_water < s->window_size)` is false and
        // a further call must neither move it nor write anything.
        assert!(state.window.write_at(WINDOW_SIZE - 1, &[SENTINEL]));
        fill(&mut state, b"");
        assert_eq!(state.high_water(), WINDOW_SIZE);
        assert_eq!(state.window.byte(WINDOW_SIZE - 1), Some(SENTINEL));
    }

    /// Case 2 clamps to the end of the window as well, through its own `init > window_size -
    /// high_water` test (`deflate.c` L367-L368).
    ///
    /// Reached by a first fill that leaves the mark just short of the top, then a second that
    /// pushes `curr + WIN_INIT` past it. The gap to `curr + WIN_INIT` is 20 bytes but only 6
    /// remain in the buffer, so 6 is what may be zeroed.
    #[test]
    fn high_water_case_two_clamps_to_the_end_of_the_window() {
        let mut state = fixture();
        plant_sentinel(&mut state);

        state.window.strstart = 700;
        state.window.lookahead = 60;
        fill(&mut state, b"");

        // Case 1, capped by WIN_INIT: 760 + 258.
        let after_first = state.high_water();
        assert_eq!(after_first, 760 + WIN_INIT);
        assert!(after_first < WINDOW_SIZE);

        // Twenty more bytes take `curr` to 780, so `curr + WIN_INIT` is 1038 -- 14 bytes past the
        // end of the buffer. Only the 6 bytes that remain may be zeroed.
        fill(&mut state, &[b'y'; 20]);

        let curr = state.window.strstart + state.window.lookahead;
        assert_eq!(curr, 780);
        assert!(
            state.high_water() >= curr,
            "case 2 requires high_water >= curr"
        );
        assert_eq!(state.high_water(), WINDOW_SIZE);
        assert!(
            state.high_water() < curr + WIN_INIT,
            "the window-end clamp must have bitten, not the gap"
        );
        assert!(window_bytes(&state, after_first, WINDOW_SIZE - after_first)
            .iter()
            .all(|&b| b == 0));
    }

    // -------------------------------------------------------------------------
    //  fill_window -- hash priming, deflate.c L313-L333
    // -------------------------------------------------------------------------

    /// Priming `insert` positions is exactly `seed_hash` followed by `insert` calls to
    /// `insert_string_no_head`, and it drives `insert` to zero (`deflate.c` L316-L332).
    ///
    /// Verified differentially against the primitives themselves, over both tables and the running
    /// key, so a wrong starting position, a missing or extra iteration, or a stale `ins_h` all
    /// show up as a table mismatch rather than as a subtle change in a later match.
    #[test]
    fn hash_priming_equals_successive_insert_string_no_head_calls() {
        const STRSTART: usize = 100;
        const DEFERRED: usize = 5;
        let data = *b"abcd";

        // The subject: `fill_window` primes over the deferred positions.
        let mut subject = fixture();
        plant(&mut subject, WINDOW_SIZE);
        clear_hash(&mut subject);
        subject.window.strstart = STRSTART;
        subject.window.lookahead = 0;
        subject.window.insert = DEFERRED;
        fill(&mut subject, &data);

        assert_eq!(subject.window.lookahead, data.len());
        assert_eq!(
            subject.window.insert, 0,
            "`insert` must be driven to zero when lookahead >= MIN_MATCH"
        );

        // The reference: the same primitives, called by hand.
        let mut expected = fixture();
        plant(&mut expected, WINDOW_SIZE);
        // Reproduce what `read_buf` wrote, so both windows hold identical bytes.
        assert!(expected.window.write_at(STRSTART, &data));
        clear_hash(&mut expected);
        expected.window.strstart = STRSTART;
        expected.window.lookahead = data.len();
        let first = STRSTART - DEFERRED;
        seed_hash(&mut expected, first);
        for offset in 0..DEFERRED {
            insert_string_no_head(&mut expected, first + offset);
        }

        assert_eq!(subject.ins_h, expected.ins_h, "the running key must agree");
        assert_eq!(subject.hash.head_entries(), expected.hash.head_entries());
        assert_eq!(subject.hash.prev_entries(), expected.hash.prev_entries());

        // And the chains really were populated, so the comparison is not two empty tables.
        assert!(subject.hash.head_entries().iter().any(|&e| e != 0));
    }

    /// The break is tested *after* the decrement, so the last deferred position is still inserted
    /// even when doing so leaves fewer than `MIN_MATCH` bytes behind (`deflate.c` L328-L331).
    ///
    /// With one byte of lookahead and five deferred positions the loop performs four insertions and
    /// stops with `insert == 1`. Folding the test into the `while` condition would perform three,
    /// removing a candidate from the chains and changing which match is found later -- which is why
    /// this loop is transcribed rather than tidied.
    #[test]
    fn priming_stops_on_the_post_decrement_break_leaving_insert_non_zero() {
        const STRSTART: usize = 100;
        const DEFERRED: usize = 5;
        let data = *b"z";

        let mut subject = fixture();
        plant(&mut subject, WINDOW_SIZE);
        clear_hash(&mut subject);
        subject.window.strstart = STRSTART;
        subject.window.lookahead = 0;
        subject.window.insert = DEFERRED;
        fill(&mut subject, &data);

        // lookahead 1: the test fires once `1 + insert < 3`, i.e. once `insert` reaches 1.
        assert_eq!(subject.window.lookahead, 1);
        assert_eq!(subject.window.insert, 1);

        // Four insertions, at `strstart - 5 ..= strstart - 2`.
        let inserted = DEFERRED - subject.window.insert;
        assert_eq!(inserted, 4);

        let mut expected = fixture();
        plant(&mut expected, WINDOW_SIZE);
        assert!(expected.window.write_at(STRSTART, &data));
        clear_hash(&mut expected);
        expected.window.strstart = STRSTART;
        expected.window.lookahead = 1;
        let first = STRSTART - DEFERRED;
        seed_hash(&mut expected, first);
        for offset in 0..inserted {
            insert_string_no_head(&mut expected, first + offset);
        }

        assert_eq!(subject.ins_h, expected.ins_h);
        assert_eq!(subject.hash.head_entries(), expected.hash.head_entries());
        assert_eq!(subject.hash.prev_entries(), expected.hash.prev_entries());
    }

    /// Below the `lookahead + insert >= MIN_MATCH` gate nothing is primed, and `ins_h` is left as
    /// it was (`deflate.c` L315, L334-L336).
    ///
    /// The reference tolerates a garbage key here rather than computing one: "If the whole input
    /// has less than `MIN_MATCH` bytes, `ins_h` is garbage, but this is not important since
    /// only literal bytes will be emitted."
    #[test]
    fn priming_is_skipped_below_min_match_and_leaves_the_tables_alone() {
        let mut state = fixture();
        clear_hash(&mut state);
        let before_head = state.hash.head_entries().to_vec();
        let before_prev = state.hash.prev_entries().to_vec();
        let before_key = state.ins_h;

        // Two bytes, no deferred positions: `lookahead + insert == 2 < MIN_MATCH`.
        fill(&mut state, b"hi");

        assert_eq!(state.window.lookahead, 2);
        assert!(state.window.lookahead + state.window.insert < MIN_MATCH);
        assert_eq!(state.ins_h, before_key, "ins_h must be left untouched");
        assert_eq!(state.hash.head_entries(), before_head.as_slice());
        assert_eq!(state.hash.prev_entries(), before_prev.as_slice());
    }

    /// A fresh stream with `insert == 0` still seeds the key once `MIN_MATCH` bytes are available,
    /// without touching the chains (`deflate.c` L315-L318 with an empty loop body).
    ///
    /// The gate is `lookahead + insert`, not `insert`, so the seed runs even though there is
    /// nothing deferred to insert. That is what leaves the key correct for the first
    /// `INSERT_STRING` a strategy performs at `strstart`.
    #[test]
    fn seeding_happens_with_nothing_deferred() {
        let mut state = fixture();
        plant(&mut state, WINDOW_SIZE);
        clear_hash(&mut state);
        state.ins_h = 0xdead;
        let before_head = state.hash.head_entries().to_vec();

        fill(&mut state, b"abcdef");

        assert_eq!(state.window.insert, 0);
        assert_ne!(state.ins_h, 0xdead, "the key must be reseeded");

        let mut expected = fixture();
        plant(&mut expected, WINDOW_SIZE);
        assert!(expected.window.write_at(0, b"abcdef"));
        seed_hash(&mut expected, 0);
        assert_eq!(state.ins_h, expected.ins_h);

        // No chain was touched, because the loop body never ran.
        assert_eq!(state.hash.head_entries(), before_head.as_slice());
    }

    // -------------------------------------------------------------------------
    //  The two together
    // -------------------------------------------------------------------------

    /// A slide immediately followed by a refill leaves a coherent window: the history that
    /// survived is where the hash tables now say it is.
    ///
    /// This is the path the "crosses the 32 KiB window boundary" corpus item exercises in the
    /// differential harness. Checking it here as well means a regression is localised to this file
    /// rather than surfacing as a byte mismatch several layers up.
    #[test]
    fn slide_then_refill_keeps_the_window_coherent() {
        let mut state = fixture();
        plant(&mut state, WINDOW_SIZE);
        clear_hash(&mut state);

        state.window.strstart = SLIDE_AT;
        state.window.lookahead = 0;
        state.window.insert = 0;
        state.window.block_start = SLIDE_AT as isize;
        state.window.match_start = W_SIZE + 11;

        // The bytes that must survive the slide: `wsize - more` of them, from `window + wsize`.
        let more = WINDOW_SIZE - SLIDE_AT;
        let surviving = window_bytes(&state, W_SIZE, W_SIZE - more);

        let fresh: alloc::vec::Vec<u8> = (0..400).map(|i| pattern(i + 7777)).collect();
        fill(&mut state, &fresh);

        // The slide fired ...
        assert!(state.slid());
        assert_eq!(state.window.strstart, MAX_DIST);
        assert_eq!(state.window.match_start, 11);
        assert_eq!(
            state.window.block_start,
            SLIDE_AT as isize - W_SIZE as isize
        );

        // ... the history moved down intact ...
        assert_eq!(window_bytes(&state, 0, surviving.len()), surviving);

        // ... and the new input landed directly above `strstart`.
        assert_eq!(state.window.lookahead, fresh.len());
        assert_eq!(window_bytes(&state, MAX_DIST, fresh.len()), fresh);

        // The OUT assertion and the over-scan region both hold.
        assert!(state.window.strstart <= WINDOW_SIZE - MIN_LOOKAHEAD);
        let curr = state.window.strstart + state.window.lookahead;
        assert!(state.high_water() >= curr);
    }
}
