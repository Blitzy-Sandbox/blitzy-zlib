//! The `Z_RLE` compressor: matches at distance one only, and no hash table.
//!
//! Mirrors `local block_state deflate_rle(deflate_state *s, int flush)`
//! (`deflate.c` L2084-L2149), whose contract is its own comment (L2079-L2083):
//!
//! > For `Z_RLE`, simply look for runs of bytes, generate matches only of distance one. Do
//! > not maintain a hash table. (It will be regenerated if this run of deflate switches away
//! > from `Z_RLE`.)
//!
//! One of the five compressors of `deflate.c` L73-L79, reached from
//! [`CompressFunc::Rle`](crate::deflate::algorithm::CompressFunc::Rle). It is selected by
//! *strategy*, not by level: the dispatch chain at `deflate.c` L1217-L1220 tests
//! `s->strategy == Z_RLE` after level 0 and `Z_HUFFMAN_ONLY`, and `configuration_table`
//! (L112-L124) never names this function -- so `deflate/config_table.rs` does not reference
//! it, and a level-0 stream stores its input even when the strategy is `Z_RLE`.
//!
//! # What makes this compressor different from the other four
//!
//! | compressor | lookahead guard | hash chains | distances | `insert` on exit |
//! |---|---|---|---|---|
//! | `stored` | never fills | slide count only | none | untouched |
//! | `fast` | `< MIN_LOOKAHEAD` | maintained | any | conditional |
//! | `slow` | `< MIN_LOOKAHEAD` | maintained | any | conditional |
//! | **`rle`** | **`<= MAX_MATCH`** | **untouched** | **exactly 1** | **plain `0`** |
//! | `huff` | `== 0` | untouched | none | plain `0` |
//!
//! The bold `rle` row is where this implementation is most exposed, and two of its entries in
//! particular, because both differ from the compressor a reader is most likely to have just
//! finished reading:
//!
//! * ★ **The lookahead guard is `s->lookahead <= MAX_MATCH`** (L2094), and the re-test after
//!   `fill_window` is `<= MAX_MATCH` as well (L2096), followed by a *separate, flat*
//!   `if (s->lookahead == 0) break;` (L2099). It is not `< MIN_LOOKAHEAD`, and it is not
//!   `deflate_huff`'s nested shape. C's own comment (L2090-L2093) says why: "We need
//!   `MAX_MATCH` bytes for the longest run, plus one for the unrolled loop." Since
//!   `MAX_MATCH` is 258 and `MIN_LOOKAHEAD` is 262, this guard is the *stronger* of the two,
//!   which is what satisfies `fill_window`'s IN assertion (`deflate.c` L257).
//! * ★ **`s->insert = 0;`** on the way out (L2141) is a plain zero, as in `deflate_huff`
//!   (L2177). The `s->strstart < MIN_MATCH-1 ? s->strstart : MIN_MATCH-1` form belongs to
//!   `deflate_fast` (L1940) and `deflate_slow` (L2068) alone. There is nothing to defer here
//!   because there are no chains to insert into.
//!
//! # The run scan decides the emitted bytes
//!
//! `Z_RLE` has no match search to speak of -- the distance is always one -- so the *length*
//! of the run is the whole of this compressor's encoder decision, and four details of
//! `deflate.c` L2105-L2117 fix it. All four are reproduced exactly by [`scan_run`], which
//! documents each in place:
//!
//! 1. the three pre-increment comparisons of L2107, which leave the cursor at
//!    `strstart + 2` before the unrolled loop is entered at all;
//! 2. the eight-way unrolled, empty-bodied `do {} while (...)` of L2109-L2114, whose
//!    `scan < strend` guard is the **last** term of the `&&` chain and is therefore consulted
//!    only after a whole group of eight byte comparisons;
//! 3. the length taken by *subtraction* from the bound, `MAX_MATCH - (strend - scan)` (L2115),
//!    rather than counted up as the scan proceeds;
//! 4. the `lookahead` clamp of L2116-L2117, which is what keeps a run from being reported
//!    across the end of the caller's data.
//!
//! A "cleaner" run detector -- `iter().take_while(|b| *b == prev).count()`, say -- is *not*
//! admissible, however obviously equivalent it looks. It checks its bound on every byte
//! instead of every eighth, so it stops the cursor somewhere else, and the length then
//! differs in the boundary cases. Equivalence here has to be exact, not plausible.
//!
//! # Reading past the end of the data is intentional
//!
//! The scan reads up to and including `window[strstart + MAX_MATCH]` no matter how small
//! `lookahead` is, exactly as C does. Those bytes are legitimate to read because
//! `fill_window` zeroes `WIN_INIT` bytes past the data and records how far with `high_water`
//! (`deflate.c` L340-L372, here
//! [`Window::initialize_win_init_tail`](crate::weak_slice::Window::initialize_win_init_tail));
//! the window arrives from `malloc`, not `calloc` (`zutil.c` L299-L303), so nothing may
//! assume it is zeroed. Any run the scan finds in that tail is then cut back by the
//! `lookahead` clamp, which is why no private clamp is added here: adding one would be a
//! second, differently-placed bound and could only disagree with the reference.
//!
//! # What is deliberately not implemented
//!
//! Only the default compile-time configuration, and none of the following is offered as a
//! runtime option or a Cargo feature, because each one changes the emitted bytes:
//! `FASTEST` (`deflate.c` L106), `LIT_MEM` (commented out at `deflate.h` L28, so the symbol
//! buffer is the single `sym_buf` of `LIT_BUFS == 4` and the `d_buf`/`l_buf` tally variants
//! of `deflate.h` L338-L355 have no counterpart), `UNALIGNED_OK` word-at-a-time comparison,
//! and `ZLIB_DEBUG`. With `ZLIB_DEBUG` undefined, `check_match` is `#define`d to nothing
//! (`deflate.c` L1622-L1624) and `Tracevv` (L2134) compiles away, so both are implemented as
//! nothing at all rather than as an approximation.
//!
//! # Safety and failure posture
//!
//! C walks the window with `Bytef *scan` and `*++scan` (L2105-L2114). This implementation has no raw
//! pointers: the walk is one bounds-checked window view plus an integer cursor, which the
//! crate root's `#![forbid(unsafe_code)]` requires and the compiler enforces. There is no
//! `unwrap()`, no `expect()` and no panicking index outside `#[cfg(test)]`; C's `Assert` is a
//! [`debug_assert!`], and every arithmetic operation that C leaves to unsigned wraparound is
//! saturating here, which agrees with C on every reachable input. Where a bounds-checked read
//! could in principle fail -- it cannot, and each site says why -- the fallback declines the
//! match rather than aborting the caller's process, and always still advances `strstart` and
//! `lookahead`, so no input can make the loop spin.

// `_tr_tally` keeps the underscore-prefixed C spelling of `deflate.h` L311-L318 for oracle
// traceability, which `clippy::pedantic` flags at the call site. The same relaxation, for the
// same reason, appears in `deflate/mod.rs`, `deflate/algorithm.rs` and `trees/mod.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#![allow(unknown_lints, clippy::used_underscore_items)]

use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::state::{Allocator, DeflateState, MAX_MATCH, MIN_MATCH};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Byte comparisons per group of the unrolled scan.
///
/// The reference spells the group out as eight literal `prev == *++scan` terms joined by
/// `&&` (`deflate.c` L2110-L2113), with `scan < strend` following them as a ninth term.
///
/// This is not a tuning parameter. The group boundary decides how far the cursor may run
/// before the bound is consulted, and therefore what length the scan reports; see
/// [`scan_run`].
const UNROLL: usize = 8;

/// Byte comparisons made before the unrolled loop is entered.
///
/// `if (prev == *++scan && prev == *++scan && prev == *++scan)` (`deflate.c` L2107) -- three,
/// and exactly three. Each `*++scan` advances the cursor *then* dereferences, so a successful
/// prelude leaves the cursor three bytes above its start, and the loop below picks up from
/// there.
const PRELUDE_COMPARISONS: usize = 3;

/// Cursor value at which the scan's bound is reached, as an offset into the scan view.
///
/// `strend = s->window + s->strstart + MAX_MATCH;` (`deflate.c` L2108). The view begins one
/// byte *below* `strstart` -- that is where `prev` lives (L2105) -- so window index
/// `strstart + MAX_MATCH` is view offset `MAX_MATCH + 1`.
const STREND_OFFSET: usize = MAX_MATCH + 1;

/// Length of the window view the scan runs over, `MAX_MATCH + 2` = 260 bytes.
///
/// The largest offset the scan reads is [`STREND_OFFSET`] itself: the final group of eight
/// comparisons ends *at* `strend` and only then finds `scan < strend` false. That is the
/// "plus one for the unrolled loop" of C's comment at `deflate.c` L2092, and it is why the
/// view is two bytes longer than `MAX_MATCH` rather than one: one byte below `strstart` for
/// `prev`, and one byte above `strstart + MAX_MATCH - 1` for the last comparison.
const SCAN_VIEW_LEN: usize = STREND_OFFSET + 1;

/// The distance every match this compressor emits carries.
///
/// `_tr_tally_dist(s, 1, ...)` (`deflate.c` L2127) -- literally one, which is the definition
/// of the `Z_RLE` strategy. It is the *raw* distance: the tally records this value in the
/// symbol buffer and feeds `d_code` the decremented one, so the distance code is always
/// `_dist_code[0]` (`deflate.h` L368-L373).
const RLE_MATCH_DISTANCE: u16 = 1;

/// The distance value that marks a symbol as a literal rather than a match.
///
/// `_tr_tally_lit` writes two zero distance bytes before the literal (`deflate.h`
/// L359-L361), and `compress_block` tells the two kinds of symbol apart by testing the
/// distance for zero (`trees.c` L916-L917).
const LITERAL_DISTANCE: u16 = 0;

// `MAX_MATCH` is 258 by `zutil.h` L93. Both the group cadence below and the 260-byte view
// depend on the exact value, so it is checked where a change would be introduced rather than
// left to a test.
const _: () = assert!(
    MAX_MATCH == 258,
    "MAX_MATCH must be 258: the eight-way unrolled scan and the 260-byte window view both \
     depend on it (deflate.c L2108-L2115)"
);

// The property that makes the scan's cursor land *exactly* on `strend`: the distance from the
// end of the prelude to the bound is a whole number of groups, 259 - 3 = 256 = 8 * 32. Were
// it not, a group could carry the cursor past `strend` and `strend - scan` would underflow in
// C -- so this assertion is what licenses the subtraction at L2115.
const _: () = assert!(
    (STREND_OFFSET - PRELUDE_COMPARISONS) % UNROLL == 0,
    "the prelude-to-strend distance must be a whole number of unrolled groups, or the scan \
     could overshoot its bound (deflate.c L2109-L2115)"
);

// Three comparisons establish a run of exactly `MIN_MATCH` bytes, which is the shortest run
// worth emitting as a match (`deflate.c` L2124). The prelude is that test, so the two
// constants are one fact and not two.
const _: () = assert!(
    PRELUDE_COMPARISONS == MIN_MATCH,
    "the prelude must prove a MIN_MATCH-long run before the unrolled loop is entered \
     (deflate.c L2107 and L2124)"
);

/// What one run scan found.
///
/// Both fields are needed, and the cursor cannot be inferred from the length or the other way
/// round: [`RunScan::cursor`] reaches [`PRELUDE_COMPARISONS`] on a *successful* prelude and
/// also on one that failed at its third comparison, so success is carried separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunScan {
    /// Where C's `scan` pointer came to rest, as an offset into the scan view.
    ///
    /// Read only by the `Assert(scan <= s->window + (uInt)(s->window_size - 1), "wild scan")`
    /// of `deflate.c` L2119-L2120, which C evaluates whether or not the prelude succeeded.
    cursor: usize,

    /// The value C assigns to `s->match_length` at `deflate.c` L2115, before the `lookahead`
    /// clamp.
    ///
    /// [`None`] when the prelude of L2107 failed, which is the case where C never enters the
    /// block and `s->match_length` keeps the zero it was given at L2103. Distinguishing this
    /// from `Some(0)` matters: a reported length is always at least [`MIN_MATCH`], never
    /// zero.
    match_length: Option<usize>,
}

impl RunScan {
    /// The outcome when the scan view could not be taken at all.
    ///
    /// Unreachable -- see [`deflate_rle`] for the two bounds that guarantee the view exists
    /// -- and equivalent to a prelude that failed immediately: no match, so the caller emits
    /// a literal and makes progress regardless.
    const UNAVAILABLE: Self = Self {
        cursor: 0,
        match_length: None,
    };
}

/// One `prev == *++scan` comparison, with the pre-increment already applied by the caller.
///
/// A read that falls outside the view is reported as a *mismatch*. That direction is
/// deliberate: it is unreachable, since no offset used here exceeds [`STREND_OFFSET`] and the
/// view is [`SCAN_VIEW_LEN`] bytes long, and reporting equality instead would let the
/// unrolled loop run on without ever reaching its guard.
#[inline]
fn byte_matches(view: &[u8], offset: usize, prev: u8) -> bool {
    view.get(offset) == Some(&prev)
}

/// Measures the run of `view[0]` repeating from `view[1]` onwards, exactly as
/// `deflate.c` L2105-L2115 does.
///
/// `view` is the window from `strstart - 1`, so `view[0]` is C's `prev` -- "byte at distance
/// one to match" (L2086) -- and `view[1]` is the byte at `strstart`. It must be
/// [`SCAN_VIEW_LEN`] bytes long; a shorter one makes the reads past its end register as
/// mismatches, which stops the scan early rather than reading out of bounds.
///
/// # The three parts, and why each is shaped the way it is
///
/// * **The prelude** is three comparisons and no more. Each advances the cursor before
///   comparing, and `&&` short-circuits, so a mismatch at the k-th leaves the remaining
///   increments undone -- which is exactly what [`RunScan::cursor`] has to report for the
///   `Assert` at L2119.
/// * **The loop body is empty**, and the condition is nine terms: eight comparisons and then
///   `scan < strend`. `do {} while (c)` and `while c {}` are the same thing when the body is
///   empty, so the implementation is a `loop` with the two exits in the reference's order. Checking the
///   bound *last* is the whole point: the cursor advances a full group between consultations,
///   which is what makes the final group finish precisely on `strend`.
/// * **The length is a subtraction**, `MAX_MATCH - (strend - scan)`, not a running count. On
///   the reachable domain the two agree, but only because the cursor never passes `strend`;
///   the compile-time assertion on [`UNROLL`] above is what establishes that, and it is also
///   why the C code can subtract two unsigned pointers here without risking a wrap.
///
/// # Return value
///
/// [`RunScan::match_length`] is [`None`] exactly when the prelude failed. Otherwise it is in
/// `MIN_MATCH..=MAX_MATCH`: the loop cannot exit with the cursor at or below the prelude's,
/// so the subtraction cannot produce less than [`MIN_MATCH`]. The caller applies the
/// `lookahead` clamp of L2116-L2117; this function does not, because the clamp needs state
/// the scan has no business reading.
fn scan_run(view: &[u8]) -> RunScan {
    // `prev = *scan;` (L2106), where `scan` is `s->window + s->strstart - 1` (L2105).
    let Some(&prev) = view.first() else {
        return RunScan::UNAVAILABLE;
    };

    // C's `scan` as an offset into `view`, starting on `prev` itself so that the first
    // pre-increment lands on `s->window[s->strstart]`.
    let mut cursor: usize = 0;

    // `if (prev == *++scan && prev == *++scan && prev == *++scan)` (L2107).
    //
    // `saturating_add` where C has `++`: the cursor is bounded by `SCAN_VIEW_LEN` on every
    // reachable path, so the two agree, and saturating cannot overflow even in principle. A
    // saturated cursor reads outside the view, which registers as a mismatch and stops the
    // scan -- the same fail-closed direction as `byte_matches`.
    for _ in 0..PRELUDE_COMPARISONS {
        cursor = cursor.saturating_add(1);
        if !byte_matches(view, cursor, prev) {
            // C falls straight past the `if`, leaving `s->match_length` at the zero of
            // L2103, and evaluates the `Assert` at L2119 with the cursor exactly here.
            return RunScan {
                cursor,
                match_length: None,
            };
        }
    }
    debug_assert_eq!(
        cursor, PRELUDE_COMPARISONS,
        "the prelude must advance the cursor once per comparison (deflate.c L2107)"
    );

    // `do { } while (prev == *++scan && ... && scan < strend);` (L2109-L2114). `strend` is
    // `STREND_OFFSET` in view coordinates.
    'unrolled: loop {
        // The eight `prev == *++scan` terms, in order. A mismatch short-circuits the whole
        // condition, so the loop ends with the cursor on the offending byte.
        for _ in 0..UNROLL {
            cursor = cursor.saturating_add(1);
            if !byte_matches(view, cursor, prev) {
                break 'unrolled;
            }
        }

        // `&& scan < strend` -- the ninth and last term, reached only after eight successful
        // comparisons. `>=` rather than `<` because this is the loop's *exit*.
        if cursor >= STREND_OFFSET {
            break 'unrolled;
        }
    }

    // `s->match_length = MAX_MATCH - (uInt)(strend - scan);` (L2115).
    //
    // Both subtractions are exact on every reachable input: the cursor is in
    // `PRELUDE_COMPARISONS + 1 ..= STREND_OFFSET`, so `STREND_OFFSET - cursor` is at most
    // `MAX_MATCH - MIN_MATCH` and the outer difference is at least `MIN_MATCH`. Saturating
    // rather than plain arithmetic keeps the expression total if a caller ever passes a view
    // that ends early, where the honest answer is a shorter run rather than a panic.
    let match_length = MAX_MATCH.saturating_sub(STREND_OFFSET.saturating_sub(cursor));
    debug_assert!(
        (MIN_MATCH..=MAX_MATCH).contains(&match_length),
        "a successful prelude must yield a run of MIN_MATCH..=MAX_MATCH (deflate.c L2115)"
    );

    RunScan {
        cursor,
        match_length: Some(match_length),
    }
}

/// Compresses with the `Z_RLE` strategy: runs of one byte, emitted at distance one.
///
/// Mirrors `local block_state deflate_rle(deflate_state *s, int flush)`
/// (`deflate.c` L2084-L2149), default compile-time configuration -- see the module
/// documentation for the variants that are deliberately not implemented.
///
/// Called only through [`CompressFunc::call`](crate::deflate::algorithm::CompressFunc::call),
/// which reaches it when `deflate()`'s dispatch chain finds a non-zero level and
/// `s->strategy == Z_RLE` (`deflate.c` L1217-L1220). `Z_TREES` cannot arrive here, because
/// `deflate()` rejects `flush > Z_BLOCK` before dispatching (L985); the `flush` argument is
/// therefore only ever tested against `Z_NO_FLUSH` and `Z_FINISH`, exactly as C tests it.
///
/// # Arguments
///
/// * `state` -- the compression state, C's `deflate_state *s`.
/// * `cursors` -- the parts of the caller's `z_stream` a compressor touches, which C reaches
///   through `s->strm`. Needed for `fill_window`, which consumes input and updates the check
///   value, and for `FLUSH_BLOCK`, which drains the pending buffer into the output.
/// * `flush` -- the caller's flush mode, C's `int flush`.
///
/// # Return value
///
/// The [`BlockState`] C returns, and it is an instruction to `deflate()` rather than a status
/// code: [`NeedMore`](BlockState::NeedMore) at L2097 and from a `FLUSH_BLOCK` that ran out of
/// output, [`FinishStarted`](BlockState::FinishStarted) from a final `FLUSH_BLOCK` that ran
/// out of output, [`FinishDone`](BlockState::FinishDone) at L2144 and
/// [`BlockDone`](BlockState::BlockDone) at L2148. Errors do not travel through this function:
/// there are none to report, and nothing here panics.
///
/// # No hash-table maintenance
///
/// Not an omission. C's comment at L2081-L2082 records that the table "will be regenerated if
/// this run of deflate switches away from `Z_RLE`", and `deflateParams` is where that happens.
/// Nothing in this function calls into `crate::deflate::hash_chain`, and neither `state.hash`
/// nor `state.ins_h` is written here -- keeping the chains "warm" would insert positions the
/// reference never inserts, which changes which match a later `deflate_fast` or `deflate_slow`
/// finds and therefore changes the emitted bytes.
///
/// [`fill_window`] does still run its own deferred-insertion loop (`deflate.c` L313-L337) when
/// it is called from here, exactly as it does for every other caller. That is the reference's
/// behaviour, not an addition: the loop is driven by `s->insert`, which this compressor never
/// raises.
pub(crate) fn deflate_rle<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {` (L2089)
    loop {
        // "Make sure that we always have enough lookahead, except at the end of the input
        // file. We need MAX_MATCH bytes for the longest run, plus one for the unrolled loop."
        //                                                                    (L2090-L2093)
        //
        // ★ `<= MAX_MATCH`, not `< MIN_LOOKAHEAD`. See the module documentation.
        if state.window.lookahead <= MAX_MATCH {
            // `fill_window(s);` (L2095). C's `s->strm` is `cursors` here; the three
            // sub-borrows are disjoint fields, so this needs no splitting.
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            // `if (s->lookahead <= MAX_MATCH && flush == Z_NO_FLUSH) return need_more;`
            //                                                              (L2096-L2098)
            if state.window.lookahead <= MAX_MATCH && flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }

            // `if (s->lookahead == 0) break;` -- "flush the current block" (L2099).
            //
            // A separate, flat test, not an `else` of the one above: with a flush mode other
            // than `Z_NO_FLUSH` and some lookahead left, control falls through to the scan
            // even though the guard was not satisfied. That is how the last, short run of a
            // stream gets emitted.
            if state.window.lookahead == 0 {
                break;
            }
        }

        // "See how many times the previous byte repeats" (L2102), `s->match_length = 0;`
        // (L2103). Unconditional, and before the scan: the scan below assigns
        // `match_length` only when it finds a run, so this is what the no-run case reports.
        state.match_length = 0;

        // `if (s->lookahead >= MIN_MATCH && s->strstart > 0) {` (L2104)
        //
        // Both terms matter. `lookahead >= MIN_MATCH` is the shortest run worth emitting, and
        // `strstart > 0` is what makes `s->window[s->strstart - 1]` -- the byte the whole scan
        // compares against -- a byte that exists. On the first call of a stream `strstart` is
        // zero, so the very first byte always takes the literal path.
        if state.window.lookahead >= MIN_MATCH && state.window.strstart > 0 {
            // `scan = s->window + s->strstart - 1;` (L2105), as a window offset. The
            // `strstart > 0` term above is what makes this exact; `saturating_sub` keeps the
            // expression total regardless.
            let scan_base = state.window.strstart.saturating_sub(1);

            // The view the scan runs over: `SCAN_VIEW_LEN` bytes from `scan_base`, which is
            // C's `scan` cursor turned into a bounds-checked slice.
            //
            // It is always available, by either of two routes. Where `fill_window` ran, the
            // reference's own OUT assertion holds -- `strstart <= window_size - MIN_LOOKAHEAD`
            // (`deflate.c` L374-L375) -- and `MIN_LOOKAHEAD` is 262, so the view ends at
            // `strstart + MAX_MATCH + 1`, three bytes inside the buffer. Where the guard above
            // skipped it, `lookahead > MAX_MATCH` means `lookahead >= MAX_MATCH + 1`, and
            // `strstart + lookahead <= window_size` -- an invariant the loop preserves, since
            // every iteration moves `strstart` up by exactly what it takes off `lookahead` --
            // then puts the view's last byte exactly on the buffer's last byte.
            // [`RunScan::UNAVAILABLE`] is therefore unreachable; it declines the match rather
            // than panicking, and the literal path below still advances both cursors, so the
            // loop cannot spin.
            //
            // The borrow of `state.window` ends with this statement, which is what lets the
            // assignment below take `state` mutably again.
            let scan = state
                .window
                .region(scan_base, SCAN_VIEW_LEN)
                .map_or(RunScan::UNAVAILABLE, scan_run);

            if let Some(match_length) = scan.match_length {
                // `s->match_length = MAX_MATCH - (uInt)(strend - scan);` (L2115) followed by
                // `if (s->match_length > s->lookahead) s->match_length = s->lookahead;`
                // (L2116-L2117), as one expression because nothing observes the intermediate.
                //
                // The clamp is what keeps a run that ran into the zeroed `WIN_INIT` tail from
                // being emitted as data the caller never supplied.
                state.match_length = match_length.min(state.window.lookahead);
            }

            // `Assert(scan <= s->window + (uInt)(s->window_size - 1), "wild scan");`
            //                                                              (L2119-L2120)
            //
            // Evaluated whether or not the prelude succeeded, as in C, and debug-only, as in
            // C: with slice reads the invariant cannot be violated, so this stands as
            // documentation of the bound the scan respects.
            debug_assert!(
                scan_base.saturating_add(scan.cursor) < state.window.window_size(),
                "wild scan: the run scan ran off the window (deflate.c L2119)"
            );
        }

        // "Emit match if have run of MIN_MATCH or longer, else emit literal" (L2123).
        //
        // `bflush` is C's `int bflush`, set by whichever tally ran and tested once below, so
        // that the flush happens *after* both branches have advanced their cursors.
        let bflush = if state.match_length >= MIN_MATCH {
            // `check_match(s, s->strstart, s->strstart - 1, (int)s->match_length);` (L2125)
            // is `#define`d to nothing without `ZLIB_DEBUG` (L1622-L1624), so there is
            // nothing to implement. It would verify the match this code just constructed.

            // `_tr_tally_dist(s, 1, s->match_length - MIN_MATCH, bflush);` (L2127)
            //
            // The length the symbol buffer carries is normalised by `MIN_MATCH`, which is
            // what `_length_code` is indexed by (`deflate.h` L372). `match_length` is in
            // `MIN_MATCH..=MAX_MATCH` here -- the scan cannot report less, and the clamp only
            // lowers it -- so the difference is in `0..=255` and the narrowing is exact.
            let normalised = state.match_length.saturating_sub(MIN_MATCH);
            debug_assert!(
                normalised <= MAX_MATCH - MIN_MATCH,
                "a tallied run must fit the normalised length alphabet (deflate.h L366)"
            );
            let full = _tr_tally(
                state,
                RLE_MATCH_DISTANCE,
                u8::try_from(normalised).unwrap_or(u8::MAX),
            );

            // `s->lookahead -= s->match_length;` (L2129)
            // `s->strstart += s->match_length;` (L2130)
            // `s->match_length = 0;` (L2131)
            //
            // In this order, and all three before the flush below. The subtraction is exact
            // because of the clamp above; the addition cannot overflow because
            // `strstart + lookahead` never exceeds `window_size`.
            state.window.lookahead = state.window.lookahead.saturating_sub(state.match_length);
            state.window.strstart = state.window.strstart.saturating_add(state.match_length);
            state.match_length = 0;

            full
        } else {
            // `Tracevv((stderr,"%c", s->window[s->strstart]));` (L2134) is `ZLIB_DEBUG`-only
            // and compiles away.

            // `_tr_tally_lit(s, s->window[s->strstart], bflush);` (L2135)
            //
            // The read cannot fail: control only reaches here with `lookahead >= 1`, and
            // `strstart + lookahead <= window_size` always holds, so the byte exists.
            // Substituting zero for the unreachable failure keeps the function total *and*
            // still advances both cursors below, which is what guarantees the loop terminates
            // however corrupt the state might be.
            let literal = state.window.byte(state.window.strstart);
            debug_assert!(
                literal.is_some(),
                "the literal at strstart must exist while lookahead is non-zero \
                 (deflate.c L2135)"
            );
            let full = _tr_tally(state, LITERAL_DISTANCE, literal.unwrap_or(0));

            // `s->lookahead--;` (L2136)
            // `s->strstart++;` (L2137)
            state.window.lookahead = state.window.lookahead.saturating_sub(1);
            state.window.strstart = state.window.strstart.saturating_add(1);

            full
        };

        // `if (bflush) FLUSH_BLOCK(s, 0);` (L2139)
        //
        // The full `FLUSH_BLOCK`, not `FLUSH_BLOCK_ONLY`: returning here is correct because
        // both branches above have already advanced `strstart` and `lookahead`, so the next
        // call resumes at the following symbol rather than re-emitting this one. (That is
        // precisely the distinction `deflate_slow`'s literal branch turns on.)
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // ★ `s->insert = 0;` (L2141) -- a plain zero, as in `deflate_huff` (L2177). The
    // conditional form of `deflate_fast` (L1940) and `deflate_slow` (L2068) defers positions
    // to the hash chains, and this compressor has none to defer to.
    state.window.insert = 0;

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }` (L2142-L2145)
    if flush == Flush::Finish {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);` (L2146-L2147)
    //
    // Only when the block holds symbols. Flushing an empty block here would emit a block
    // header the reference does not emit.
    if state.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;` (L2148)
    BlockState::BlockDone
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// harness: a test asserts, and a failing assertion panics. Indexing is allowed because every
// index below is bounded by a constant the line above it establishes. `used_underscore_items`
// is allowed because these tests call `_tr_init` and `_tr_tally`, whose names are the C
// spellings of `deflate.h` L311-L318. The same relaxations, for the same reasons, appear in
// `deflate/algorithm.rs`, `deflate/window.rs` and `trees/mod.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(
    unknown_lints,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests {
    use super::{
        deflate_rle, scan_run, RunScan, PRELUDE_COMPARISONS, SCAN_VIEW_LEN, STREND_OFFSET, UNROLL,
    };
    use crate::config::Z_UNKNOWN;
    use crate::deflate::algorithm::{BlockState, Flush, StreamCursors};
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL, MAX_MATCH,
        MIN_MATCH,
    };
    use crate::read_buf::{InputCursor, OutputCursor};
    use crate::trees::_tr_init;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The largest normalised length a symbol can carry, `MAX_MATCH - MIN_MATCH` = 255.
    const FULL_LENGTH: u8 = 255;

    /// A stream configured as `deflateInit2(&strm, 6, Z_DEFLATED, -15, 8, Z_RLE)` and wired up
    /// by `_tr_init`, which is the state `deflate()` first enters a compressor with.
    ///
    /// Raw DEFLATE (negative `windowBits`) so that nothing but the block itself reaches the
    /// output, and the default memory level, so `lit_bufsize` is 16384 and `sym_end` is 49149 --
    /// far beyond anything these fixtures tally, so no fixture trips the `bflush` path by
    /// accident. In a debug build the allocator fills every block with `0xa5`, the byte
    /// `test/infcover.c` L87 uses, so nothing below can pass by accident on zeroed memory.
    fn new_state() -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: -15,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Rle,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        state
    }

    /// Cursors over `input` and `output`, with the scalars a freshly reset raw stream carries
    /// (`deflate.c` L663-L668).
    fn new_cursors<'i, 'o>(input: &'i [u8], output: &'o mut [u8]) -> StreamCursors<'i, 'o> {
        StreamCursors {
            input: InputCursor::new(input),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// A scan view whose first byte is `prev` and whose next `run` bytes repeat it, padded out
    /// to [`SCAN_VIEW_LEN`] with a byte that cannot extend the run.
    ///
    /// `run` counts the repeats *above* `prev`, i.e. the bytes at `strstart` onwards, so it is
    /// the run length C would find were the bound not in the way.
    fn view_with_run(prev: u8, run: usize) -> Vec<u8> {
        let filler = prev.wrapping_add(1);
        let mut view = vec![filler; SCAN_VIEW_LEN];
        view[0] = prev;
        for slot in view.iter_mut().skip(1).take(run) {
            *slot = prev;
        }
        view
    }

    /// `len` bytes in which no two neighbours are equal, so that every one of them takes the
    /// literal path.
    fn no_repeats(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    /// Everything one `deflate_rle` call left behind that the assertions below need.
    ///
    /// Collected into a value because the symbol buffer has to be read *before* anything else
    /// touches the state: any flush mode other than `Z_NO_FLUSH` ends by flushing the block,
    /// and `_tr_flush_block` calls `init_block`, which resets `sym_next` (`trees.c` L450).
    #[derive(Debug)]
    struct Run {
        /// What `deflate_rle` returned.
        result: BlockState,
        /// The symbols still in the buffer, as `(distance, length_or_literal)` pairs.
        symbols: Vec<(u16, u8)>,
        /// `strm->total_in`: bytes drawn out of the caller's input by `fill_window`.
        total_in: usize,
        /// `strm->total_out`: bytes handed to the caller's output buffer.
        total_out: usize,
        /// `s->lookahead` on exit.
        lookahead: usize,
        /// `s->strstart` on exit.
        strstart: usize,
        /// `s->sym_next` on exit.
        sym_next: usize,
        /// `s->insert` on exit.
        insert: usize,
        /// `s->match_length` on exit.
        match_length: usize,
    }

    impl Run {
        /// Bytes that were turned into symbols: everything read in, less what is still waiting.
        fn consumed(&self) -> usize {
            self.total_in - self.lookahead
        }

        /// The number of input bytes the tallied symbols account for.
        fn covered(&self) -> usize {
            self.symbols
                .iter()
                .map(|&(dist, lc)| {
                    if dist == 0 {
                        1
                    } else {
                        usize::from(lc) + MIN_MATCH
                    }
                })
                .sum()
        }
    }

    /// Runs `deflate_rle` once over `input` and snapshots the result.
    ///
    /// The output buffer is generously sized so that no `FLUSH_BLOCK` takes the
    /// premature-exit path of `deflate.c` L1644, which would mask the state under test.
    fn drive(input: &[u8], flush: Flush) -> Run {
        let mut state = new_state();
        let mut output = vec![0_u8; input.len() + 4096];
        let mut cursors = new_cursors(input, &mut output);
        let result = deflate_rle(&mut state, &mut cursors, flush);
        let symbols = (0..state.sym_next())
            .step_by(3)
            .filter_map(|offset| state.pending.symbol_at(offset))
            .collect();
        Run {
            result,
            symbols,
            total_in: usize::try_from(cursors.total_in).unwrap(),
            total_out: usize::try_from(cursors.total_out).unwrap(),
            lookahead: state.window.lookahead,
            strstart: state.window.strstart,
            sym_next: state.sym_next(),
            insert: state.window.insert,
            match_length: state.match_length,
        }
    }

    /// The four scan constants against the C expressions they come from, by hand.
    #[test]
    fn the_scan_constants_match_the_reference_expressions() {
        assert_eq!(UNROLL, 8, "eight `prev == *++scan` terms (deflate.c L2110)");
        assert_eq!(
            PRELUDE_COMPARISONS, 3,
            "three comparisons before the loop (deflate.c L2107)"
        );
        assert_eq!(
            STREND_OFFSET, 259,
            "strstart + MAX_MATCH, one byte above it"
        );
        assert_eq!(
            SCAN_VIEW_LEN, 260,
            "prev plus MAX_MATCH plus the extra byte"
        );
        assert_eq!(MAX_MATCH, 258);
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(FULL_LENGTH, u8::try_from(MAX_MATCH - MIN_MATCH).unwrap());
    }

    /// The cadence property the subtraction at L2115 depends on: the prelude leaves the cursor
    /// a whole number of groups short of `strend`, so no group can carry it past.
    #[test]
    fn the_prelude_leaves_a_whole_number_of_groups_before_strend() {
        assert_eq!((STREND_OFFSET - PRELUDE_COMPARISONS) % UNROLL, 0);
        assert_eq!((STREND_OFFSET - PRELUDE_COMPARISONS) / UNROLL, 32);
    }

    /// A run shorter than `MIN_MATCH` fails the prelude, so no length is reported and the
    /// cursor stops on the byte that broke it.
    #[test]
    fn a_run_shorter_than_min_match_fails_the_prelude() {
        for run in 0..MIN_MATCH {
            let view = view_with_run(b'a', run);
            let scan = scan_run(&view);
            assert_eq!(
                scan.match_length, None,
                "a run of {run} must not reach the unrolled loop"
            );
            assert_eq!(scan.cursor, run + 1, "cursor for a run of {run}");
        }
    }

    /// A run of exactly `MIN_MATCH` passes the prelude, and the first comparison of the
    /// unrolled loop then fails -- the shortest length this scan can report.
    #[test]
    fn a_run_of_exactly_min_match_reports_min_match() {
        let view = view_with_run(b'a', MIN_MATCH);
        let scan = scan_run(&view);
        assert_eq!(scan.match_length, Some(MIN_MATCH));
        assert_eq!(scan.cursor, PRELUDE_COMPARISONS + 1);
    }

    /// Every run length from `MIN_MATCH` to `MAX_MATCH` is reported exactly, and the cursor is
    /// always one above it -- the identity `MAX_MATCH - (strend - scan)` reduces to.
    #[test]
    fn every_reportable_run_length_is_exact() {
        for run in MIN_MATCH..=MAX_MATCH {
            let view = view_with_run(b'z', run);
            let scan = scan_run(&view);
            assert_eq!(scan.match_length, Some(run), "run of {run}");
            assert_eq!(scan.cursor, run + 1, "cursor for a run of {run}");
        }
    }

    /// A run longer than `MAX_MATCH` is capped by the `scan < strend` guard, with the cursor
    /// resting exactly on `strend`.
    #[test]
    fn a_run_longer_than_max_match_is_capped_at_the_bound() {
        let view = vec![b'q'; SCAN_VIEW_LEN];
        let scan = scan_run(&view);
        assert_eq!(scan.match_length, Some(MAX_MATCH));
        assert_eq!(scan.cursor, STREND_OFFSET);
    }

    /// The cursor never passes `strend`, whatever the run: that is what makes C's
    /// `strend - scan` subtraction well defined.
    #[test]
    fn the_cursor_never_passes_strend() {
        for run in 0..=SCAN_VIEW_LEN {
            let view = view_with_run(b'\0', run);
            assert!(
                scan_run(&view).cursor <= STREND_OFFSET,
                "cursor overshot strend for a run of {run}"
            );
        }
    }

    /// An unusable or truncated view declines the match rather than panicking.
    #[test]
    fn an_unusable_view_declines_the_match() {
        assert_eq!(scan_run(&[]), RunScan::UNAVAILABLE);
        assert_eq!(scan_run(b"a").match_length, None);
        assert_eq!(scan_run(b"aa").cursor, 2);
        // A view that runs out mid-loop stops there instead of reading past its end.
        let short = vec![b'a'; PRELUDE_COMPARISONS + 4];
        assert_eq!(scan_run(&short).cursor, PRELUDE_COMPARISONS + 4);
    }

    /// The first byte of a stream is always a literal, because `s->strstart > 0` is false on
    /// the first pass through the loop (`deflate.c` L2104). The three that follow it then form
    /// the shortest emittable run.
    ///
    /// `Z_NO_FLUSH` throughout this group of tests: it is the only mode whose exit does *not*
    /// flush the block, so it is the only one that leaves the symbols observable.
    #[test]
    fn the_first_byte_of_a_stream_is_always_a_literal() {
        let mut input = vec![b'a'; 4];
        input.extend(no_repeats(2048));
        let run = drive(&input, Flush::NoFlush);

        assert_eq!(run.result, BlockState::NeedMore);
        assert_eq!(
            run.symbols[0],
            (0, b'a'),
            "the first byte must be a literal"
        );
        assert_eq!(
            run.symbols[1],
            (1, 0),
            "the next three bytes are a MIN_MATCH run at distance one"
        );
        assert_eq!(run.covered(), run.consumed());
    }

    /// A run of `MIN_MATCH - 1` repeats is too short to emit, so all of it stays literal --
    /// the `s->match_length >= MIN_MATCH` test of `deflate.c` L2124.
    #[test]
    fn a_run_too_short_to_emit_stays_literal() {
        let mut input = b"abbc".to_vec();
        input.extend(no_repeats(2048));
        let run = drive(&input, Flush::NoFlush);

        assert_eq!(run.result, BlockState::NeedMore);
        assert_eq!(
            &run.symbols[..4],
            &[(0, b'a'), (0, b'b'), (0, b'b'), (0, b'c')]
        );
    }

    /// Data with no repeats at all becomes literals only, one per byte and in order.
    #[test]
    fn data_without_repeats_becomes_literals() {
        let input = no_repeats(2048);
        let run = drive(&input, Flush::NoFlush);

        assert_eq!(run.result, BlockState::NeedMore);
        assert_eq!(run.symbols.len(), run.consumed());
        for (index, &(dist, lc)) in run.symbols.iter().enumerate() {
            assert_eq!(dist, 0, "symbol {index} must be a literal");
            assert_eq!(lc, input[index], "literal {index}");
        }
    }

    /// A long run becomes one literal followed by `MAX_MATCH`-long matches at distance one.
    #[test]
    fn a_long_run_becomes_distance_one_matches() {
        const RUN: usize = 1000;
        let run = drive(&vec![b'a'; RUN], Flush::NoFlush);

        assert_eq!(run.result, BlockState::NeedMore);
        assert_eq!(run.total_in, RUN, "fill_window must take all of it");
        assert_eq!(run.symbols[0], (0, b'a'));
        assert!(
            run.symbols[1..].iter().all(|&(dist, _)| dist == 1),
            "every match must be at distance one: {:?}",
            run.symbols
        );
        assert!(
            run.symbols[1..].iter().all(|&(_, lc)| lc == FULL_LENGTH),
            "each match must run the full MAX_MATCH: {:?}",
            run.symbols
        );
        // One literal plus three full matches accounts for 775 bytes, leaving 225 in
        // lookahead -- fewer than MAX_MATCH, which is exactly why the call returns need_more.
        assert_eq!(run.symbols.len(), 4);
        assert_eq!(run.covered(), 1 + 3 * MAX_MATCH);
        assert_eq!(run.consumed(), run.covered());
        assert_eq!(run.strstart, run.covered());
        assert_eq!(run.lookahead, RUN - run.covered());
    }

    /// A run that outlives a window slide keeps producing distance-one matches, which is the
    /// case `fill_window`'s slide and this scan have to agree about.
    #[test]
    fn a_run_across_a_window_slide_stays_at_distance_one() {
        // `windowBits` 15 gives a 32 KiB window, so 80 KiB of input forces two slides.
        let input = vec![b'w'; 80 * 1024];
        let run = drive(&input, Flush::NoFlush);

        assert_eq!(run.result, BlockState::NeedMore);
        assert_eq!(run.symbols[0], (0, b'w'));
        assert!(run.symbols[1..].iter().all(|&(dist, _)| dist == 1));
        assert!(run.symbols[1..].iter().all(|&(_, lc)| lc == FULL_LENGTH));
        assert_eq!(run.covered(), run.consumed());
        assert!(
            run.consumed() > 64 * 1024,
            "the run must have carried past two slides, not {}",
            run.consumed()
        );
        // `strstart` is below the raw byte count precisely because the window slid.
        assert!(run.strstart < run.consumed());
    }

    /// Feeding the same bytes in one call and in two leaves the same symbols behind: the scan
    /// reads only the window, so a chunk boundary cannot move a run.
    #[test]
    fn incremental_feeding_tallies_the_same_symbols() {
        let input = vec![b'k'; 600];
        let whole = drive(&input, Flush::NoFlush);

        let mut state = new_state();
        let mut output = vec![0_u8; 4096];
        let (first, second) = input.split_at(313);

        let mut cursors = new_cursors(first, &mut output);
        assert_eq!(
            deflate_rle(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );
        let (check, total_in, total_out, data_type) = (
            cursors.check,
            cursors.total_in,
            cursors.total_out,
            cursors.data_type,
        );

        let mut cursors = StreamCursors {
            input: InputCursor::new(second),
            output: OutputCursor::new(&mut output),
            check,
            total_in,
            total_out,
            data_type,
        };
        assert_eq!(
            deflate_rle(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );

        let split: Vec<(u16, u8)> = (0..state.sym_next())
            .step_by(3)
            .filter_map(|offset| state.pending.symbol_at(offset))
            .collect();
        assert_eq!(split, whole.symbols);
        assert_eq!(state.window.lookahead, whole.lookahead);
    }

    /// `Z_NO_FLUSH` with less than `MAX_MATCH` of lookahead and no more input returns
    /// `need_more` without emitting anything (`deflate.c` L2096-L2098).
    #[test]
    fn z_no_flush_with_a_short_stream_asks_for_more() {
        let run = drive(b"aaaaa", Flush::NoFlush);
        assert_eq!(run.result, BlockState::NeedMore);
        assert!(run.symbols.is_empty(), "nothing may be emitted yet");
        // The input was drawn into the window all the same, which is what `fill_window` does.
        assert_eq!(run.lookahead, 5);
        assert_eq!(run.strstart, 0);
        // Returning before the tail leaves `insert` alone, as C does.
        assert_eq!(run.insert, 0);
    }

    /// `Z_FINISH` on a short stream falls through the `lookahead <= MAX_MATCH` guard, emits the
    /// whole of it and reports `finish_done` (`deflate.c` L2142-L2145).
    #[test]
    fn z_finish_emits_a_short_stream_and_finishes() {
        let run = drive(b"aaaabbbb", Flush::Finish);
        assert_eq!(run.result, BlockState::FinishDone);
        assert_eq!(run.strstart, 8);
        assert_eq!(run.lookahead, 0);
        assert!(run.total_out > 0, "the final block must reach the output");
        // The tail flushed the block, so `init_block` has emptied the symbol buffer.
        assert_eq!(run.sym_next, 0);
    }

    /// An empty stream flushed with `Z_FINISH` produces no symbols: the loop breaks at L2099
    /// and only the tail runs.
    #[test]
    fn an_empty_stream_finishes_without_symbols() {
        let run = drive(b"", Flush::Finish);
        assert_eq!(run.result, BlockState::FinishDone);
        assert!(run.symbols.is_empty());
        assert_eq!(run.strstart, 0);
        assert_eq!(run.insert, 0);
    }

    /// A flush mode that is neither `Z_NO_FLUSH` nor `Z_FINISH` reports `block_done`, having
    /// flushed the symbols it accumulated (`deflate.c` L2146-L2148).
    #[test]
    fn a_sync_flush_reports_block_done() {
        let run = drive(b"aaaaaaaaaa", Flush::SyncFlush);
        assert_eq!(run.result, BlockState::BlockDone);
        assert_eq!(run.strstart, 10);
        assert_eq!(run.lookahead, 0);
        assert_eq!(run.sym_next, 0, "the tail flushed the block");
        assert!(run.total_out > 0);
    }

    /// ★ `s->insert = 0;` (`deflate.c` L2141) -- a plain zero, not the conditional form of
    /// `deflate_fast` and `deflate_slow`, whatever `strstart` happens to be.
    ///
    /// `strstart` is deliberately smaller than `MIN_MATCH - 1` in the second case, which is
    /// exactly where the conditional form would leave a non-zero value.
    #[test]
    fn the_tail_always_zeroes_insert() {
        for input in [b"a".as_slice(), b"aa", b"aaaaaaaaaaaa"] {
            let mut state = new_state();
            let mut output = vec![0_u8; 4096];
            let mut cursors = new_cursors(input, &mut output);
            // A non-zero starting value, so that a missing assignment would show.
            state.window.insert = 7;
            let result = deflate_rle(&mut state, &mut cursors, Flush::Finish);
            assert_eq!(result, BlockState::FinishDone);
            assert_eq!(
                state.window.insert,
                0,
                "insert must be a plain zero for input of {} bytes",
                input.len()
            );
        }
    }

    /// Every iteration consumes at least one byte, and `match_length` is left at zero -- the
    /// two facts that together mean no input can make the loop spin.
    #[test]
    fn every_iteration_consumes_at_least_one_byte() {
        let input = b"aaabbbbccccccccd".to_vec();
        let run = drive(&input, Flush::Finish);
        assert_eq!(run.result, BlockState::FinishDone);
        assert_eq!(run.strstart, input.len());
        assert_eq!(run.lookahead, 0);
        assert_eq!(run.match_length, 0, "match_length must be left at zero");
    }

    /// A window whose bytes are all identical is the worst case for the scan, and it must
    /// still terminate with every byte accounted for.
    #[test]
    fn a_run_longer_than_the_symbol_capacity_still_terminates() {
        // 200 KiB of one byte: several window slides, hundreds of maximal matches, and enough
        // symbols to make the `bflush` path plausible without reaching `sym_end` (49149).
        let input = vec![b'\xa5'; 200 * 1024];
        let run = drive(&input, Flush::Finish);
        assert_eq!(run.result, BlockState::FinishDone);
        assert_eq!(run.total_in, input.len());
        assert_eq!(run.lookahead, 0);
        assert!(run.total_out > 0);
    }
}
