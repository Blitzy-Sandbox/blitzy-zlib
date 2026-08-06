//! The hash-chain search that decides which match the compressor emits.
//!
//! This is the port of `longest_match` (`deflate.c` L1379-L1530), specifically
//! the default variant: the one the shipped build compiles, with neither
//! `FASTEST` nor `UNALIGNED_OK` defined.
//!
//! # ⚠ THIS FILE IS PART OF THE OUTPUT CONTRACT — DO NOT "OPTIMIZE" IT ⚠
//!
//! RFC 1951 constrains the *format* of a DEFLATE stream, not the *choices* an
//! encoder makes within it. Which of several equally long matches is emitted,
//! how far a hash chain is walked, and which candidates are rejected before
//! they are ever compared are all implementation-defined — and this port is
//! required to reproduce the reference implementation's answers byte for byte.
//! Two of the decision points that determine that, out of the eight in the
//! whole encoder, are in this one function. Every one of the following is part
//! of the compressed output, not an implementation detail:
//!
//! * The `limit` fallback to `NIL` rather than a saturating subtraction, which
//!   makes window index 0 unmatchable (`deflate.c` L1396-L1400).
//! * The four-test early rejection in its exact written order, *including* the
//!   deliberately redundant `best_len - 1` test the reference itself describes
//!   as "not always a win" (`deflate.c` L1482-L1491).
//! * The eight-way unrolled comparison, whose group boundary — not merely the
//!   comparison itself — decides how far the scan runs past the data and
//!   therefore what `len` comes out (`deflate.c` L1499-L1509).
//! * The strict `len > best_len`, which keeps the *first* candidate of a given
//!   length. Chains run most recent first, so that candidate is the nearest
//!   one, and `doc/algorithm.txt` §1 records why that matters: the chains "are
//!   searched starting with the most recent strings, to favor small distances
//!   and thus take advantage of the Huffman encoding".
//! * The chain-walk termination order — the `prev` load and the `> limit` test
//!   happen before `chain_length` is decremented, because C's `&&`
//!   short-circuits (`deflate.c` L1525-L1526).
//!
//! A *better* match finder fails the acceptance criterion. So does a faster one
//! that changes any of the above. There is deliberately **no SIMD here**: the
//! vectorised backends in this crate are confined to CRC-32 and Adler-32,
//! because a checksum is one scalar however it is computed, whereas vectorised
//! match finding would change the emitted bytes.
//!
//! # Variants that are deliberately not ported
//!
//! * `UNALIGNED_OK` (`deflate.c` L1404-L1410 and L1446-L1478) compares two
//!   bytes at a time through `ush` loads, uses `strend = window + strstart +
//!   MAX_MATCH - 1`, and finishes with `len = (MAX_MATCH - 1) - (strend -
//!   scan)` plus a trailing single-byte fixup. It is a different code path with
//!   different boundary behaviour, so porting it would be a silent behaviour
//!   change.
//! * `FASTEST` (`deflate.c` L1532-L1588) examines only the head of the chain
//!   and belongs to a different encoder — a two-entry `configuration_table`
//!   and a forced level 1 (`deflate.c` L106-L111).
//!
//! Neither `FASTEST` nor `LIT_MEM` may ever become a Cargo feature of this
//! crate: both change the compressed output, and a build knob that changes the
//! output is indistinguishable from a bug in a library whose contract is
//! byte-identical compression.
//!
//! # Reading past the end of the data is intentional
//!
//! The reference implementation checks for insufficient lookahead only every
//! eighth comparison, so it knowingly reads bytes beyond the input. Its own
//! comment (`deflate.c` L1438-L1445) explains why that is harmless:
//! "uninitialized memory will be accessed, and conditional jumps will be made
//! that depend on those values. However the length of the match is limited to
//! the lookahead, so the output of deflate is not affected by the uninitialized
//! values."
//!
//! In safe Rust those bytes must be *initialized* — arbitrary, but not
//! undefined — and they are: `fill_window` zeroes `WIN_INIT` bytes above
//! `high_water` for exactly this reason (`deflate.c` L347-L372, and
//! [`crate::weak_slice::Window::initialize_win_init_tail`]). This module
//! depends on that guarantee and deliberately adds no bounds narrowing of its
//! own, because narrowing the scan would change which bytes are compared and
//! therefore which match wins.
//!
//! # How the C pointers become indices
//!
//! `scan` and `match` are two *read* pointers into one buffer
//! (`deflate.c` L1391 and L1436), which in Rust is simply two shared borrows of
//! the window — they may overlap, and for a hash-chain candidate they always
//! do. [`crate::weak_slice::Window::scan_pair`] hands both out at once with the
//! bound established once, and every subsequent read is a `usize` offset into
//! them. No raw pointer, no `unsafe`, and no arithmetic that can leave the
//! buffer.
//!
//! # Assertions
//!
//! C's `Assert` compiles to nothing unless `ZLIB_DEBUG` is defined, which the
//! shipped build does not do. Each one therefore becomes a `debug_assert!`,
//! which likewise vanishes from a release build — with one deliberate
//! exception, `Assert(*scan == *match, "match[2]?")` (`deflate.c` L1494). That
//! one is *not* an invariant: `UPDATE_HASH` collides, badly so at `memLevel` 1
//! where `hash_bits` is 8, and two positions on one chain can therefore agree
//! at offsets 0 and 1 while differing at offset 2. C never notices because the
//! assertion is compiled out; asserting it here would turn a benign no-op into
//! a debug-build panic on perfectly legal input. It is recorded as a comment at
//! the point where it appears in the original instead.

use crate::allocate::Allocator;
use crate::deflate::state::{DeflateState, IPos, MAX_MATCH, MIN_LOOKAHEAD};
use crate::weak_slice::{HashChains, Window, MIN_HASH_BITS};

/// Number of byte comparisons between two lookahead checks.
///
/// The reference implementation spells this out as eight literal
/// `*++scan == *++match` terms joined by `&&` (`deflate.c` L1499-L1504), with
/// the comment "We check for insufficient lookahead only every 8th comparison;
/// the 256th check will be made at strstart + 258" (L1496-L1497).
///
/// This is not a tuning parameter. The group boundary is what decides how far
/// the scan may run before the `scan < strend` guard is consulted, and that
/// decides the resulting match length in the boundary cases.
const UNROLL: usize = 8;

/// Length of the two window views the comparison runs over,
/// `MAX_MATCH + 1` = 259 bytes.
///
/// One more than `MAX_MATCH` because the largest offset either view is read at
/// is `MAX_MATCH` itself: the unrolled loop stops with its cursor at exactly
/// `strend - window - strstart` = `MAX_MATCH` (`deflate.c` L1504-L1509), and
/// the early-rejection test reads `scan[best_len]` where `best_len` can reach
/// `MAX_MATCH` (`deflate.c` L1522).
const VIEW_LEN: usize = MAX_MATCH + 1;

// `Assert(s->hash_bits >= 8 && MAX_MATCH == 258, "Code too clever")`
// (`deflate.c` L1420). The `MAX_MATCH` half is a statement about a constant, so
// it is checked at compile time rather than at run time; the `hash_bits` half
// is checked below, where the value is available.
const _: () = assert!(
    MAX_MATCH == 258,
    "MAX_MATCH must be 258: the unrolled comparison and the 259-byte window \
     views both depend on it (deflate.c L1417-L1420)"
);

// The reference comment at `deflate.c` L1417 records that "The code is
// optimized for HASH_BITS >= 8 and MAX_MATCH-2 multiple of 16". This branch of
// the comparison needs only a multiple of `UNROLL`, which is what makes the
// final group land exactly on `strend`: 258 - 2 = 256 = 8 * 32, so the 256th
// comparison is the one made at `strstart + 258`.
const _: () = assert!(
    (MAX_MATCH - 2) % UNROLL == 0,
    "MAX_MATCH - 2 must be a whole number of unrolled groups (deflate.c L1417)"
);

/// The four state values the search reads but never writes.
///
/// In C these are `s->max_chain_length`, `s->prev_length`, `s->good_match` and
/// `s->nice_match`, read straight off the state at `deflate.c` L1390, L1394,
/// L1395 and L1423. They are copied into a value here so that the search itself
/// borrows nothing but the window and the hash chains, which is what lets
/// `match_start` be written back afterwards without aliasing.
#[derive(Debug, Clone, Copy)]
struct MatchParams {
    /// `s->max_chain_length` (`deflate.h` L175): the chain-walk budget.
    max_chain_length: usize,
    /// `s->prev_length` (`deflate.h` L170): matches not longer than this are
    /// discarded, and it seeds `best_len`.
    prev_length: usize,
    /// `s->good_match` (`deflate.h` L195): above this the budget is quartered.
    good_match: usize,
    /// `s->nice_match` (`deflate.h` L197), signed exactly as in C.
    nice_match: i32,
}

/// What one search produced.
///
/// `match_start` is [`None`] when no candidate beat `prev_length`, which is the
/// case the contract comment describes as "the result is equal to `prev_length`
/// and `match_start` is garbage" (`deflate.c` L1382-L1384). C leaves the field
/// holding whatever it held before; not writing it is the same thing, and it is
/// why the search can take a shared borrow of the window.
#[derive(Debug, Clone, Copy)]
struct Outcome {
    /// The value `longest_match` returns (`deflate.c` L1528-L1529).
    len: usize,
    /// The window index to store in `s->match_start`, if any
    /// (`deflate.c` L1515).
    match_start: Option<usize>,
}

/// Widens a window offset to the `int` C uses for `len`, `best_len` and
/// `nice_match`.
///
/// Every value this is called with is at most `MAX_MATCH`, or is a `lookahead`
/// bounded by `2 * w_size <= 65536`, so the conversion is always exact. The
/// saturating fallback exists only so that nothing in this module can panic
/// even in principle.
#[inline]
fn as_int(offset: usize) -> i32 {
    i32::try_from(offset).unwrap_or(i32::MAX)
}

/// Narrows one of C's `int` match lengths back to a window offset, or [`None`]
/// when it is negative.
///
/// Used wherever C indexes with plain `best_len` (`deflate.c` L1414, L1482,
/// L1522). A negative value cannot arise, because `best_len` is seeded from the
/// unsigned `prev_length` and only ever grows; [`None`] would nonetheless make
/// the read fail rather than wrap.
///
/// The `best_len - 1` reads deliberately do **not** go through here — that index
/// legitimately reaches `-1` and is served by [`byte_below`] instead.
#[inline]
fn as_offset(len: i32) -> Option<usize> {
    usize::try_from(len).ok()
}

/// Reinterprets one of C's `int` lengths as the `uInt` it is compared against.
///
/// This is the `(uInt)nice_match` cast at `deflate.c` L1429 and the
/// `(uInt)best_len` cast at L1528. A C conversion from `int` to `unsigned` is
/// modular, so a negative value becomes a very large one and the `>` or `<=`
/// test it feeds then fails; round-tripping the two's-complement bytes
/// reproduces that exactly, with no `as` cast and no target-dependent
/// behaviour.
#[inline]
fn as_uint(value: i32) -> u32 {
    u32::from_ne_bytes(value.to_ne_bytes())
}

/// One byte of a window view, or [`None`] when the offset is past its end.
///
/// The views are [`VIEW_LEN`] bytes long and no offset used here exceeds
/// `MAX_MATCH`, so [`None`] is unreachable; it is returned rather than
/// panicking because a compression library must not abort its caller's process.
#[inline]
fn byte_at(view: &[u8], offset: usize) -> Option<u8> {
    view.get(offset).copied()
}

/// The byte C addresses as `p[best_len - 1]`, where `p` is a window pointer
/// sitting at absolute window index `base`.
///
/// ★ This cannot be a view-relative read, because `best_len` reaches **zero**
/// and the index is then `-1`. The contract comment at `deflate.c` L1386 claims
/// the IN assertion `prev_length >= 1`, but the shipped build violates it:
/// `deflate_slow` leaves `prev_length` at exactly `0` after emitting a match —
/// `s->prev_length -= 2;` followed by `while (--s->prev_length != 0);`
/// (`deflate.c` L2028-L2033) — and `deflate_fast` never re-seeds `prev_length`
/// at all. So a `deflateParams()` switch from a lazy level (4-9) to a greedy one
/// (1-3) enters `longest_match` with `best_len == 0`, and C then reads and
/// compares the byte one *below* the pointer, i.e. `window[base - 1]`.
///
/// That byte is a live participant in decision point #3, so it must be read the
/// way C reads it. Treating the `-1` index as a failed read instead rejects
/// **every** candidate on the chain, which costs real compression: the C oracle
/// emits 96 bytes for a corpus where declining the read emits 1019.
///
/// [`None`] is returned only when `base == 0 && best_len == 0`, where C would
/// read outside its own window allocation. That combination is unreachable —
/// `cur_match` and `strstart` are both non-zero whenever the chain head is not
/// `NIL` — so no reachable input reaches the fallback, and returning [`None`]
/// rather than panicking honours the no-panic requirement.
#[inline]
fn byte_below<W>(window: &Window<W>, base: usize, best_len: i32) -> Option<u8>
where
    W: AsRef<[u8]> + AsMut<[u8]>,
{
    // `base + best_len - 1`, evaluated where the intermediate may legitimately
    // be negative. `i64` holds every value exactly: `base` is a window offset
    // below `2 * 32768` and `best_len` is at most `MAX_MATCH`.
    let base = i64::try_from(base).ok()?;
    let index = base.checked_add(i64::from(best_len))?.checked_sub(1)?;
    window.byte(usize::try_from(index).ok()?)
}

/// Whether two window views agree at the given offsets, i.e. C's
/// `*++scan == *++match`.
///
/// A read that falls outside either view is reported as a *mismatch*, which
/// terminates the scan. That direction is deliberate: it is unreachable (see
/// [`byte_at`]), and reporting equality instead could let the unrolled loop run
/// on without advancing towards its guard.
#[inline]
fn bytes_match(scan: &[u8], scan_offset: usize, mat: &[u8], match_offset: usize) -> bool {
    match (scan.get(scan_offset), mat.get(match_offset)) {
        (Some(scan_byte), Some(match_byte)) => scan_byte == match_byte,
        _ => false,
    }
}

/// Sets `match_start` to the longest match starting at the current string and
/// returns its length.
///
/// Port of `local uInt longest_match(deflate_state *s, IPos cur_match)`
/// (`deflate.c` L1379-L1530), default configuration — see the module
/// documentation for the two variants that are deliberately not ported.
///
/// The contract, quoted from `deflate.c` L1380-L1387 because callers depend on
/// every clause of it:
///
/// > Set `match_start` to the longest match starting at the given string and
/// > return its length. Matches shorter or equal to `prev_length` are
/// > discarded, in which case the result is equal to `prev_length` and
/// > `match_start` is garbage.
/// >
/// > IN assertions: `cur_match` is the head of the hash chain for the current
/// > string (`strstart`) and its distance is `<= MAX_DIST`, and
/// > `prev_length >= 1`.
/// >
/// > OUT assertion: the match length is not greater than `s->lookahead`.
///
/// ★ The `prev_length >= 1` half of that IN assertion is **not** honoured by the
/// shipped build: `deflate_slow` drives `prev_length` to exactly `0`
/// (`deflate.c` L2028-L2033) and `deflate_fast` never re-seeds it, so a
/// `deflateParams()` switch from a lazy level to a greedy one calls this
/// function with `prev_length == 0`. C copes by indexing one byte below its
/// `scan` and `match` pointers; [`byte_below`] reproduces that, and the
/// behaviour is required for byte-identical output.
///
/// So `match_start` is a *side effect*, and reading it is only meaningful when
/// the returned length exceeds `prev_length`. Both callers honour that:
/// `deflate_fast` uses it only once `match_length >= MIN_MATCH`
/// (`deflate.c` L1894-L1898) and `deflate_slow` notes that "If `prev_match` is
/// also `MIN_MATCH`, `match_start` is garbage but we will ignore the current
/// match anyway" (L2004-L2006). This port leaves the field untouched in that
/// case, which is precisely what "garbage" means.
///
/// `cur_match` must not be [`IPos::NIL`]. Both callers guarantee it — they test
/// `hash_head != NIL` before calling (`deflate.c` L1886 and L1988) — and the
/// chain walk itself can never reach 0, because it stops at `limit`, which is
/// never below `NIL`. A `NIL` argument nonetheless yields the `prev_length`
/// result rather than a match against window index 0.
#[must_use]
pub(crate) fn longest_match<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cur_match: IPos,
) -> usize {
    // The four values C reads directly off `s`. Copying them first is what lets
    // the search take a shared borrow of `state.window`, run to completion, and
    // only then write `match_start` back.
    let params = MatchParams {
        max_chain_length: state.max_chain_length,
        prev_length: state.prev_length,
        good_match: state.good_match,
        nice_match: state.nice_match,
    };

    let outcome = search(&state.window, &state.hash, params, cur_match);

    // `s->match_start = cur_match;` (`deflate.c` L1515), deferred to here.
    // `longest_match` never reads `match_start`, so the moment of the write is
    // unobservable; what *is* observable is that no write happens at all when
    // nothing beat `prev_length`.
    if let Some(match_start) = outcome.match_start {
        state.window.match_start = match_start;
    }

    outcome.len
}

/// The chain walk itself, over the two views it needs and nothing else.
///
/// Split out from [`longest_match`] for two reasons: it takes only shared
/// borrows, which is what makes the deferred `match_start` write possible; and
/// being generic over the buffer types it can be exercised directly against a
/// hand-built [`Window`] and [`HashChains`], with no allocator in sight. The
/// algorithm is unchanged — every comparison, every ordering and every cast is
/// the one in `deflate.c`.
fn search<W, P>(
    window: &Window<W>,
    hash: &HashChains<P>,
    params: MatchParams,
    cur_match: IPos,
) -> Outcome
where
    W: AsRef<[u8]> + AsMut<[u8]>,
    P: AsRef<[u16]> + AsMut<[u16]>,
{
    // =====================================================================
    //  DECISION POINT #2 — chain-walk initialization (`deflate.c` L1390-L1431)
    //
    //  Every value set up here changes which candidates are examined, in what
    //  order, and how many of them: the budget, the seed for `best_len`, the
    //  early-exit threshold, and the floor the walk stops at. Change any one of
    //  them and a different match is emitted.
    // =====================================================================

    // `Bytef *scan = s->window + s->strstart;` (L1391) becomes the offset
    // `strstart`, from which the scan view below is taken.
    let strstart = window.strstart;
    let lookahead = window.lookahead;

    // `unsigned chain_length = s->max_chain_length;` (L1390). Held as a `u32`
    // rather than a `usize` precisely because C's is an `unsigned`: the
    // `>>= 2` below and the wrapping `--chain_length` at the bottom of the walk
    // are both modulo 2^32 there, and this is what makes them modulo 2^32 here.
    // `max_chain_length` is a `uInt` in C too (`deflate.h` L175), so nothing is
    // ever actually clamped by the conversion.
    let mut chain_length = u32::try_from(params.max_chain_length).unwrap_or(u32::MAX);

    // `int best_len = (int)s->prev_length;` (L1394). SIGNED, and that is
    // load-bearing: `scan[best_len - 1]` is indexed below, and `len > best_len`
    // is a signed comparison.
    let mut best_len = as_int(params.prev_length);

    // `int nice_match = s->nice_match;` (L1395). Signed for the same reason:
    // `len >= nice_match` is compared on `int`s (L1517).
    let mut nice_match = params.nice_match;

    // `IPos limit = s->strstart > (IPos)MAX_DIST(s) ?`
    // `             s->strstart - (IPos)MAX_DIST(s) : NIL;` (L1396-L1397).
    //
    // ★ The `NIL` fallback is deliberate and must NOT become a saturating
    // subtraction. The reference explains itself at L1398-L1400: "Stop when
    // cur_match becomes <= limit. To simplify the code, we prevent matches with
    // the string of window index 0." A saturating subtraction would also yield
    // 0 here, but `limit` is compared with `>`, so 0 as a *limit* excludes
    // index 0 — which is exactly the point. `deflate_slow` restates the
    // consequence at L1990-L1992: "we prevent matches with the string of window
    // index 0 (in particular we have to avoid a match of the string with itself
    // at the start of the input file)".
    let max_dist = window.max_dist();
    let limit = if strstart > max_dist {
        // Exact: `strstart - max_dist < strstart < 2 * w_size <= 65536`.
        IPos::from_index(strstart - max_dist).unwrap_or(IPos::NIL)
    } else {
        IPos::NIL
    };

    // `s->lookahead` as the `uInt` the two casts at L1429 and L1528 compare
    // against. A live stream keeps it below `2 * w_size <= 65536`, so the
    // conversion is exact and the saturating fallback is unreachable.
    let lookahead_uint = u32::try_from(lookahead).unwrap_or(u32::MAX);

    // `Assert(s->hash_bits >= 8 && ...)` (L1420); the `MAX_MATCH` half is the
    // compile-time assertion above. `HashChains::new` already refuses anything
    // smaller, so this records the requirement rather than discovering it.
    debug_assert!(
        hash.hash_bits() >= MIN_HASH_BITS,
        "code too clever: the comparison assumes hash_bits >= 8 (deflate.c L1417-L1420)"
    );

    // `Assert((ulg)s->strstart <= s->window_size - MIN_LOOKAHEAD,`
    // `       "need lookahead");` (L1431-L1432). This is what guarantees the
    // 259-byte scan view fits: it leaves 262 bytes of slack above `strstart`.
    debug_assert!(
        strstart <= window.window_size().saturating_sub(MIN_LOOKAHEAD),
        "need lookahead: strstart must leave MIN_LOOKAHEAD bytes (deflate.c L1431)"
    );

    // ★ The IN assertion at `deflate.c` L1386 claims `prev_length >= 1`, and the
    // shipped build DOES NOT HONOUR IT. `lm_init` seeds `prev_length` with
    // `MIN_MATCH - 1` = 2 (L698) and `deflate_slow` assigns a previous
    // `match_length` to it (L1985), but L2028-L2033 then drives it to exactly
    // `0` — `s->prev_length -= 2;` followed by
    // `while (--s->prev_length != 0);` — and `deflate_fast` never re-seeds it.
    // A `deflateParams()` switch from a lazy level to a greedy one therefore
    // arrives here with `best_len == 0`, which was confirmed against an
    // instrumented build of the in-tree C oracle.
    //
    // So this must NOT be asserted: C reads `scan[-1]` and `match[-1]` in that
    // case and compares them, and [`byte_below`] reproduces exactly that. What
    // does hold unconditionally is that `best_len` is non-negative, since
    // `prev_length` is unsigned in C and `best_len` only grows.
    debug_assert!(
        best_len >= 0,
        "best_len is seeded from the unsigned prev_length and only grows"
    );

    let mut best_start: Option<usize> = None;
    let mut cur = cur_match;

    'search: {
        // The current string, as a view rather than a pointer. `strend` in C is
        // `s->window + s->strstart + MAX_MATCH` (L1412), i.e. offset
        // `MAX_MATCH` into this view; note the `UNALIGNED_OK` variant uses
        // `MAX_MATCH - 1` there and is not what is ported.
        let Some(scan_init) = window.region(strstart, VIEW_LEN) else {
            break 'search;
        };

        // `Byte scan_end1 = scan[best_len - 1];` (L1413)
        // `Byte scan_end  = scan[best_len];`     (L1414)
        let Some(best_offset) = as_offset(best_len) else {
            break 'search;
        };
        // `scan[best_len - 1]` goes through `byte_below`, because `best_len` can
        // be `0` and C then reads `window[strstart - 1]` — see [`byte_below`].
        let mut scan_end1 = byte_below(window, strstart, best_len).unwrap_or(0);
        let mut scan_end = byte_at(scan_init, best_offset).unwrap_or(0);

        // `if (s->prev_length >= s->good_match) { chain_length >>= 2; }`
        // (L1422-L1425) — "Do not waste too much time if we already have a good
        // match". Both operands are `uInt` in C and `usize` here, so the
        // comparison is unsigned either way.
        if params.prev_length >= params.good_match {
            chain_length >>= 2;
        }

        // `if ((uInt)nice_match > s->lookahead) nice_match = (int)s->lookahead;`
        // (L1429). The comparison is UNSIGNED and the assignment is SIGNED, and
        // both casts are reproduced rather than tidied away. L1426-L1428 gives
        // the reason: "Do not look for matches beyond the end of the input.
        // This is necessary to make deflate deterministic."
        if as_uint(nice_match) > lookahead_uint {
            nice_match = as_int(lookahead);
        }

        // `do { ... } while (...)` (L1434, L1525-L1526). A `loop` with the
        // condition spelled out at the bottom, because C's `continue` in a
        // `do`-`while` jumps to the *condition*, not to the top — so the
        // early-rejection path must still advance the chain and consume budget.
        // That is what the `'candidate` block below expresses.
        'chain: loop {
            // `Assert(cur_match < s->strstart, "no future");` (L1435)
            debug_assert!(
                cur.to_index().is_some_and(|index| index < strstart),
                "no future: cur_match must lie behind strstart (deflate.c L1435)"
            );

            // `match = s->window + cur_match;` (L1436). `match_index` yields
            // `None` for `NIL`, which the `limit` test already excludes.
            let Some(candidate) = cur.match_index() else {
                break 'chain;
            };

            // C's `scan` and `match` are two read pointers into one buffer;
            // here they are two shared borrows of it, overlapping exactly as
            // the pointers do. Re-taken each iteration because C recomputes
            // `match` at L1436 and resets `scan` at L1510.
            let Some((scan, mat)) = window.scan_pair(strstart, candidate, VIEW_LEN) else {
                break 'chain;
            };

            'candidate: {
                // =========================================================
                //  DECISION POINT #3 — the early-rejection quartet
                //  (`deflate.c` L1482-L1485)
                //
                //      if (match[best_len]     != scan_end  ||
                //          match[best_len - 1] != scan_end1 ||
                //          *match              != *scan     ||
                //          *++match            != scan[1])      continue;
                //
                //  ★ The ORDER is load-bearing. `*++match` mutates `match` as a
                //  side effect, and that mutation persists into the
                //  `scan += 2, match++;` at L1493 — so after the quartet the
                //  cursors sit at `candidate + 2` and `strstart + 2`, i.e. at
                //  offset 2 in both views. Those are the values `scan_offset`
                //  and `match_offset` start from below.
                //
                //  ★ The `best_len - 1` test is DELIBERATELY REDUNDANT and must
                //  be kept. Verbatim from `deflate.c` L1487-L1491:
                //
                //      The check at best_len - 1 can be removed because it will
                //      be made again later. (This heuristic is not always a
                //      win.) It is not necessary to compare scan[2] and
                //      match[2] since they are always equal when the other
                //      bytes match, given that the hash keys are equal and that
                //      HASH_BITS >= 8.
                //
                //  Removing it changes which candidates survive to the full
                //  comparison, and therefore which match wins. It is not an
                //  optimization to be recovered later.
                // =========================================================
                let Some(best_offset) = as_offset(best_len) else {
                    break 'candidate;
                };

                // `match[best_len]`, `match[best_len - 1]`, `*match`, `*++match`
                // and, on the scan side, `*scan` and `scan[1]`. The
                // `best_len - 1` read goes through [`byte_below`] because
                // `best_len` reaches `0` after a lazy-to-greedy
                // `deflateParams()` switch, and C then compares
                // `window[cur_match - 1]`.
                if byte_at(mat, best_offset) != Some(scan_end)
                    || byte_below(window, candidate, best_len) != Some(scan_end1)
                    || byte_at(mat, 0) != byte_at(scan, 0)
                    || byte_at(mat, 1) != byte_at(scan, 1)
                {
                    break 'candidate;
                }

                // `scan += 2, match++;` (L1493) — see the note above on why
                // both cursors land on offset 2.
                //
                // `Assert(*scan == *match, "match[2]?");` (L1494) is NOT
                // reproduced as a `debug_assert!`. It is not an invariant: the
                // rolling hash collides, and at `memLevel` 1 (`hash_bits` 8,
                // `hash_shift` 3) two positions on one chain can agree at
                // offsets 0 and 1 and differ at offset 2. C is unaffected
                // because the assertion is compiled out unless `ZLIB_DEBUG` is
                // defined; asserting it here would panic a debug build on legal
                // input.
                let mut scan_offset = 2;
                let mut match_offset = 2;

                // The eight-way unrolled comparison (L1499-L1504):
                //
                //     do {
                //     } while (*++scan == *++match && ... 8 terms ... &&
                //              scan < strend);
                //
                // The pre-increments and the short-circuiting are both
                // reproduced: a mismatch at the k-th term leaves the remaining
                // increments undone, and the `scan < strend` guard is consulted
                // only after a full group of eight. That cadence is what makes
                // the last group end exactly on `strend`, and it is what fixes
                // `len` in the boundary cases — a plain byte-at-a-time loop
                // with a bound check every iteration computes a different
                // length.
                'unrolled: loop {
                    for _ in 0..UNROLL {
                        scan_offset += 1;
                        match_offset += 1;
                        if !bytes_match(scan, scan_offset, mat, match_offset) {
                            break 'unrolled;
                        }
                    }
                    // `&& scan < strend`, where `strend` is offset `MAX_MATCH`.
                    if scan_offset >= MAX_MATCH {
                        break 'unrolled;
                    }
                }

                // `Assert(scan <= s->window + (unsigned)(s->window_size - 1),`
                // `       "wild scan");` (L1506-L1507)
                debug_assert!(
                    strstart.saturating_add(scan_offset) < window.window_size(),
                    "wild scan: the comparison ran off the window (deflate.c L1506)"
                );

                // `len = MAX_MATCH - (int)(strend - scan);` (L1509), in signed
                // arithmetic. `strend` is offset `MAX_MATCH` and `scan` is
                // `scan_offset`, so `strend - scan` is `MAX_MATCH -
                // scan_offset` and the whole expression is `scan_offset` — the
                // offset of the first mismatch, capped at `MAX_MATCH` by the
                // loop's own guard.
                //
                // `scan = strend - MAX_MATCH;` (L1510) restores the cursor to
                // `strstart`, which is offset 0 of the `scan` view; the reads
                // below are relative to that, as C's are.
                let len = as_int(scan_offset);

                // ---- the improvement branch (L1514-L1524) ----
                if len > best_len {
                    // ★ STRICT `>`. The first candidate to reach a given length
                    // keeps it, and because chains run most recent first that
                    // is the NEAREST candidate — the small-distance preference
                    // `doc/algorithm.txt` §1 describes. `>=` would emit a
                    // farther match of the same length and a different
                    // distance code.
                    best_start = Some(candidate); // `s->match_start = cur_match` (L1515)
                    best_len = len; // (L1516)

                    // `if (len >= nice_match) break;` (L1517) — signed, and
                    // BEFORE the two refreshes below, so a candidate that trips
                    // the early exit leaves `scan_end1`/`scan_end` stale. That
                    // is unobservable only because the walk ends here.
                    if len >= nice_match {
                        break 'chain;
                    }

                    // `scan_end1 = scan[best_len - 1];` (L1521)
                    // `scan_end   = scan[best_len];`    (L1522)
                    // indexed from the restored cursor, i.e. from `strstart`,
                    // with the NEW `best_len`.
                    let Some(new_offset) = as_offset(best_len) else {
                        break 'candidate;
                    };
                    scan_end1 = byte_below(window, strstart, best_len).unwrap_or(scan_end1);
                    scan_end = byte_at(scan, new_offset).unwrap_or(scan_end);
                }
            }

            // `} while ((cur_match = prev[cur_match & wmask]) > limit`
            // `         && --chain_length != 0);` (L1525-L1526)
            //
            // ★ Both effects live in the condition and the order matters. The
            // `prev` load and the `> limit` test happen first; `chain_length` is
            // decremented only if the limit test passed, because C's `&&`
            // short-circuits. Decrementing unconditionally walks one link too
            // few; testing the budget first walks one too many.
            let Some(cur_index) = cur.to_index() else {
                break 'chain;
            };
            // `prev[cur_match & wmask]`: the mask is part of the data
            // structure, not a bounds check — a `prev` index "is thus a window
            // index modulo 32K" (`deflate.h` L139-L142) — and `prev_at` applies
            // it.
            cur = IPos::from_pos(hash.prev_at(cur_index));
            if cur <= limit {
                break 'chain;
            }
            // `--chain_length != 0` on a C `unsigned`, which wraps rather than
            // trapping. `deflateTune` lets a caller set `max_chain` to 0
            // (`deflate.c` L826-L830), and the reference then walks the chain
            // `UINT_MAX` times; `wrapping_sub` reproduces that instead of
            // panicking in a debug build.
            chain_length = chain_length.wrapping_sub(1);
            if chain_length == 0 {
                break 'chain;
            }
        }
    }

    // `if ((uInt)best_len <= s->lookahead) return (uInt)best_len;`
    // `return s->lookahead;` (L1528-L1529) — the OUT assertion, "the match
    // length is not greater than s->lookahead". The cast is unsigned, so a
    // negative `best_len` (impossible, but spelled out anyway) compares as a
    // very large value and the lookahead is returned instead.
    let len = if as_uint(best_len) <= lookahead_uint {
        as_offset(best_len).unwrap_or(lookahead)
    } else {
        lookahead
    };

    Outcome {
        len,
        match_start: best_start,
    }
}

// -----------------------------------------------------------------------------
//  Tests
//
//  These exercise `search` directly against a hand-built window and hand-built
//  hash chains, which is the whole reason the search is generic over its buffer
//  types: no allocator, no `z_stream`, and every candidate's match length under
//  the test's control. `longest_match` itself is covered too, because the
//  deferred `match_start` write is part of the contract rather than an
//  implementation detail.
//
//  Note that these tests can only prove the *local* behaviour of this function.
//  The only proof of byte-identical output is
//  `crates/zlib-rs-differential/tests/byte_identical.rs`, which diffs the whole
//  encoder against the C oracle across the level x windowBits x memLevel x
//  strategy x flush x corpus matrix. Levels 1-3 (`deflate_fast`) and levels 4-9
//  (`deflate_slow`) both route through here, so a defect in this file fails
//  almost all of it.
// -----------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    // Panicking and indexing are how a test reports a failure, and the C source
    // this file transcribes is written in exactly the casts reproduced below.
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
mod tests {
    use super::{longest_match, search, IPos, MatchParams, Outcome, MAX_MATCH};
    use crate::allocate::GlobalAllocator;
    use crate::config::DeflateConfig;
    use crate::deflate::state::{DeflateState, MIN_LOOKAHEAD, MIN_MATCH};
    use crate::weak_slice::{HashChains, Pos, Window};

    /// `w_bits` for the fixture. 10 gives a 1024-byte window inside a 2048-byte
    /// buffer, which is large enough for a full `MAX_MATCH` comparison and small
    /// enough to sit on the stack.
    const W_BITS: u32 = 10;
    /// `hash_bits` for the fixture. Equal to [`W_BITS`] so that `head` and
    /// `prev` can be the same array type, which is what lets both be plain
    /// arrays instead of allocations.
    const HASH_BITS: u32 = 10;
    /// `w_size` = `1 << W_BITS`.
    const W_SIZE: usize = 1 << W_BITS;
    /// `window_size` = `2 << W_BITS`, the length `Window::new` requires.
    const WINDOW_SIZE: usize = 2 << W_BITS;
    /// `MAX_DIST(s)` for the fixture: 1024 - 262 = 762.
    const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
    /// Largest block the fixture plants in one go.
    const MAX_PLANT: usize = 300;

    /// A deterministic, non-periodic byte for window position `index`.
    ///
    /// Periodicity matters: a pattern that repeated every 256 bytes would create
    /// a full-length match at distance 256 at every position and every test
    /// below would measure that instead of what it planted. This is a
    /// `SplitMix64`-style finaliser, which has no short period over a
    /// 2048-byte window.
    fn pattern(seed: u64, index: usize) -> u8 {
        let mut x = (index as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(seed);
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 29;
        x as u8
    }

    /// A deterministic generator for the randomised differential test.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            // xorshift64, then a multiplicative finaliser.
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// A value in `0..bound`.
        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }

        /// A value in `low..=high`.
        fn between(&mut self, low: usize, high: usize) -> usize {
            low + self.below(high - low + 1)
        }
    }

    /// A window plus hash chains plus tuning parameters, all under test control.
    struct Fixture {
        window: Window<[u8; WINDOW_SIZE]>,
        hash: HashChains<[u16; W_SIZE]>,
        params: MatchParams,
    }

    impl Fixture {
        /// A fixture whose window holds nothing but [`pattern`] bytes, with
        /// `strstart` at [`MAX_DIST`] so that `limit` is `NIL` and every
        /// candidate above zero is reachable.
        fn new(seed: u64) -> Self {
            let mut buf = [0u8; WINDOW_SIZE];
            for (index, slot) in buf.iter_mut().enumerate() {
                *slot = pattern(seed, index);
            }
            let mut window = Window::new(buf, W_BITS).unwrap();
            window.strstart = MAX_DIST;
            window.lookahead = MAX_MATCH;
            window.match_start = usize::MAX;
            let hash = HashChains::new([0u16; W_SIZE], [0u16; W_SIZE], W_BITS, HASH_BITS).unwrap();
            Self {
                window,
                hash,
                params: MatchParams {
                    // `configuration_table[6]` = {8, 16, 128, 128}
                    // (`deflate.c` L120), with `nice_match` widened to
                    // `MAX_MATCH` so that a planted match is not cut short by
                    // the early exit unless a test asks for that.
                    max_chain_length: 128,
                    prev_length: 2,
                    good_match: 8,
                    nice_match: MAX_MATCH as i32,
                },
            }
        }

        /// Copies the first `len` bytes of the current string to `candidate`, so
        /// that the candidate matches for exactly as long as the surrounding
        /// pattern allows.
        fn plant(&mut self, candidate: usize, len: usize) {
            assert!(len <= MAX_PLANT);
            let mut buf = [0u8; MAX_PLANT];
            let strstart = self.window.strstart;
            buf[..len].copy_from_slice(self.window.region(strstart, len).unwrap());
            assert!(self.window.write_at(candidate, &buf[..len]));
        }

        /// Plants a match of *exactly* `len` bytes at `candidate` by copying the
        /// current string and then forcing the byte after it to differ.
        ///
        /// Without the forced divergence the length would depend on whether the
        /// two [`pattern`] bytes at offset `len` happened to collide, which
        /// would make the assertions probabilistic.
        fn plant_exact(&mut self, candidate: usize, len: usize) {
            self.plant(candidate, len);
            let strstart = self.window.strstart;
            let next = self.window.byte(strstart + len).unwrap();
            self.poke(candidate + len, next ^ 0xFF);
        }

        /// Makes `candidate` fail the early rejection outright, by forcing its
        /// first byte to differ from the current string's.
        ///
        /// That is the `*match != *scan` test (`deflate.c` L1484), so the
        /// candidate is rejected whatever the other three tests say.
        fn decoy(&mut self, candidate: usize) {
            let strstart = self.window.strstart;
            let first = self.window.byte(strstart).unwrap();
            self.poke(candidate, first ^ 0xFF);
        }

        /// Overwrites one window byte.
        fn poke(&mut self, index: usize, value: u8) {
            assert!(self.window.write_at(index, &[value]));
        }

        /// Links `positions` into one hash chain, most recent first, terminated
        /// by `NIL` — the shape `INSERT_STRING` builds
        /// (`deflate.c` L160-L163).
        fn chain(&mut self, positions: &[usize]) {
            for (index, position) in positions.iter().enumerate() {
                let next = positions
                    .get(index + 1)
                    .map_or(Pos::NIL, |p| Pos::from_index(*p).unwrap());
                self.hash.set_prev_at(*position, next);
            }
        }

        /// Runs the search from `head`, which is what a caller's `hash_head` is.
        fn run(&self, head: usize) -> Outcome {
            search(
                &self.window,
                &self.hash,
                self.params,
                IPos::from_index(head).unwrap(),
            )
        }

        /// The whole window, for the independent transcription below.
        fn bytes(&self) -> &[u8] {
            self.window.region(0, WINDOW_SIZE).unwrap()
        }
    }

    /// An independent transcription of `longest_match` from `deflate.c`, written
    /// with absolute window indices rather than the two bounded views the port
    /// uses.
    ///
    /// The point of writing it a second time, and differently, is that the
    /// port's translation of C's pointer arithmetic into offsets *within* a view
    /// is exactly where an off-by-one would hide. This version keeps C's
    /// pointers as absolute `usize` indices into the whole window, so the two
    /// disagree if that translation is wrong anywhere. The eight-way unrolling
    /// and the `scan < strend` cadence are reproduced here as well, because a
    /// reference that merely computed the longest common prefix would disagree
    /// legitimately at the boundary.
    fn reference(
        window: &[u8],
        prev: &[u16],
        w_mask: usize,
        strstart: usize,
        lookahead: usize,
        max_dist: usize,
        params: MatchParams,
        head: u32,
    ) -> (usize, Option<usize>) {
        let mut chain_length = params.max_chain_length as u32;
        let scan = strstart;
        let mut best_len = params.prev_length as i32;
        let mut nice_match = params.nice_match;
        let limit: u32 = if strstart > max_dist {
            (strstart - max_dist) as u32
        } else {
            0
        };
        let strend = strstart + MAX_MATCH;
        let mut scan_end1 = window[scan + (best_len - 1) as usize];
        let mut scan_end = window[scan + best_len as usize];
        let mut match_start: Option<usize> = None;
        let mut cur_match = head;

        if params.prev_length >= params.good_match {
            chain_length >>= 2;
        }
        if (nice_match as u32) > lookahead as u32 {
            nice_match = lookahead as i32;
        }

        loop {
            let base = cur_match as usize;
            let rejected = window[base + best_len as usize] != scan_end
                || window[base + (best_len - 1) as usize] != scan_end1
                || window[base] != window[scan]
                || window[base + 1] != window[scan + 1];

            if !rejected {
                let mut scan_at = scan + 2;
                let mut match_at = base + 2;
                'unrolled: loop {
                    for _ in 0..8 {
                        scan_at += 1;
                        match_at += 1;
                        if window[scan_at] != window[match_at] {
                            break 'unrolled;
                        }
                    }
                    if scan_at >= strend {
                        break 'unrolled;
                    }
                }
                let len = MAX_MATCH as i32 - (strend as i32 - scan_at as i32);
                if len > best_len {
                    match_start = Some(base);
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = window[scan + (best_len - 1) as usize];
                    scan_end = window[scan + best_len as usize];
                }
            }

            cur_match = u32::from(prev[(cur_match as usize) & w_mask]);
            if cur_match <= limit {
                break;
            }
            chain_length = chain_length.wrapping_sub(1);
            if chain_length == 0 {
                break;
            }
        }

        let len = if (best_len as u32) <= lookahead as u32 {
            best_len as usize
        } else {
            lookahead
        };
        (len, match_start)
    }

    // -------------------------------------------------------------------------
    //  DECISION POINT #2 — the chain-walk limit
    // -------------------------------------------------------------------------

    /// `limit = strstart - MAX_DIST(s)` when `strstart` exceeds it, and the walk
    /// stops at `cur_match <= limit` (`deflate.c` L1396-L1397, L1525).
    #[test]
    fn limit_excludes_candidates_at_or_below_strstart_minus_max_dist() {
        // strstart 1000 > MAX_DIST 762, so limit is 1000 - 762 = 238 and a
        // candidate at exactly 238 must never be examined.
        let mut excluded = Fixture::new(0x1111);
        excluded.window.strstart = 1000;
        excluded.plant_exact(238, 40);
        excluded.decoy(300);
        excluded.chain(&[300, 238]);
        let outcome = excluded.run(300);
        assert_eq!(
            outcome.match_start, None,
            "a candidate at limit was matched"
        );
        assert_eq!(outcome.len, 2, "the result must fall back to prev_length");

        // The same candidate, with strstart at MAX_DIST so that limit is NIL, is
        // reachable. That is the control that proves the exclusion above came
        // from `limit` and not from the fixture.
        let mut included = Fixture::new(0x1111);
        assert_eq!(included.window.strstart, MAX_DIST);
        included.plant_exact(238, 40);
        included.decoy(300);
        included.chain(&[300, 238]);
        let outcome = included.run(300);
        assert_eq!(outcome.match_start, Some(238));
        assert_eq!(outcome.len, 40);
    }

    /// The `NIL` fallback makes window index 0 unmatchable, which is what stops a
    /// string matching itself at the very start of the input
    /// (`deflate.c` L1398-L1400, restated at L1990-L1992).
    #[test]
    fn window_index_zero_is_never_a_candidate() {
        // A perfect 40-byte match sitting at index 0, reachable only through the
        // chain. `limit` is NIL here, and `0 > 0` is false, so the walk ends
        // before the candidate is looked at.
        let mut at_zero = Fixture::new(0x2222);
        at_zero.plant_exact(0, 40);
        at_zero.decoy(300);
        at_zero.chain(&[300, 0]);
        let outcome = at_zero.run(300);
        assert_eq!(
            outcome.match_start, None,
            "window index 0 must never be matched"
        );
        assert_eq!(outcome.len, 2);

        // The identical match one byte further along IS matched, so the
        // exclusion above is about index 0 and nothing else.
        let mut at_one = Fixture::new(0x2222);
        at_one.plant_exact(1, 40);
        at_one.decoy(300);
        at_one.chain(&[300, 1]);
        let outcome = at_one.run(300);
        assert_eq!(outcome.match_start, Some(1));
        assert_eq!(outcome.len, 40);
    }

    /// `if (s->prev_length >= s->good_match) chain_length >>= 2;`
    /// (`deflate.c` L1422-L1425).
    #[test]
    fn good_match_quarters_the_chain_budget() {
        // Five candidates, the fifth carrying the only real match. A budget of
        // 8 reaches it; the quartered budget of 2 does not.
        let build = |prev_length: usize| {
            let mut fixture = Fixture::new(0x3333);
            fixture.params.max_chain_length = 8;
            fixture.params.good_match = 4;
            fixture.params.prev_length = prev_length;
            fixture.plant_exact(300, 40);
            for decoy in [700, 690, 680, 670] {
                fixture.decoy(decoy);
            }
            fixture.chain(&[700, 690, 680, 670, 300]);
            fixture.run(700)
        };

        // 3 < good_match, so the full budget of 8 candidates is walked.
        let full = build(3);
        assert_eq!(full.match_start, Some(300));
        assert_eq!(full.len, 40);

        // 4 >= good_match, so the budget becomes 8 >> 2 = 2 and only the first
        // two candidates are examined.
        let quartered = build(4);
        assert_eq!(
            quartered.match_start, None,
            "the quartered budget must not reach the fifth candidate"
        );
        assert_eq!(quartered.len, 4);
    }

    /// `if ((uInt)nice_match > s->lookahead) nice_match = (int)s->lookahead;`
    /// (`deflate.c` L1429) — "necessary to make deflate deterministic".
    #[test]
    fn nice_match_is_clamped_to_the_lookahead() {
        let build = |lookahead: usize| {
            let mut fixture = Fixture::new(0x4444);
            fixture.window.lookahead = lookahead;
            fixture.params.nice_match = 200;
            fixture.plant_exact(200, 100);
            fixture.plant_exact(500, 60);
            fixture.chain(&[500, 200]);
            fixture.run(500)
        };

        // lookahead 50 < nice_match 200, so nice_match becomes 50 and the
        // 60-byte head candidate ends the walk immediately.
        let clamped = build(50);
        assert_eq!(
            clamped.match_start,
            Some(500),
            "the clamp must stop the walk at the first long-enough candidate"
        );
        assert_eq!(clamped.len, 50, "the return is clamped to the lookahead");

        // With enough lookahead nothing is clamped, the walk continues, and the
        // longer but farther candidate wins.
        let unclamped = build(MAX_MATCH);
        assert_eq!(unclamped.match_start, Some(200));
        assert_eq!(unclamped.len, 100);
    }

    // -------------------------------------------------------------------------
    //  Chain order, the early exit and the budget
    // -------------------------------------------------------------------------

    /// The first candidate to reach a given length keeps it
    /// (`deflate.c` L1514), and because chains run most recent first
    /// (`doc/algorithm.txt` §1) that is the nearest one.
    ///
    /// Note that `>` versus `>=` is not separately observable: the early
    /// rejection already requires `match[best_len] == scan_end`, so a candidate
    /// whose length would merely tie is rejected before its length is ever
    /// computed. What the strict comparison guarantees, and what this test
    /// pins down, is that the *first* candidate of a length is the one recorded.
    #[test]
    fn the_first_candidate_of_a_given_length_wins() {
        let build = |order: [usize; 2]| {
            let mut fixture = Fixture::new(0x5555);
            fixture.plant_exact(300, 40);
            fixture.plant_exact(700, 40);
            fixture.chain(&order);
            fixture.run(order[0])
        };

        // Most recent first, which is how `INSERT_STRING` builds a chain: the
        // nearer candidate is examined first and keeps the length.
        let nearest_first = build([700, 300]);
        assert_eq!(nearest_first.match_start, Some(700));
        assert_eq!(nearest_first.len, 40);

        // Reversed, the farther one is examined first and keeps it, which is
        // what makes the real chain order load-bearing.
        let farthest_first = build([300, 700]);
        assert_eq!(farthest_first.match_start, Some(300));
        assert_eq!(farthest_first.len, 40);
    }

    /// `if (len >= nice_match) break;` (`deflate.c` L1517) ends the walk before
    /// a longer, farther candidate can be considered.
    #[test]
    fn a_candidate_reaching_nice_match_ends_the_walk() {
        let build = |nice_match: i32| {
            let mut fixture = Fixture::new(0x6666);
            fixture.params.nice_match = nice_match;
            fixture.plant_exact(200, 100);
            fixture.plant_exact(500, 40);
            fixture.chain(&[500, 200]);
            fixture.run(500)
        };

        let early = build(30);
        assert_eq!(
            early.match_start,
            Some(500),
            "the walk must stop at 40 >= 30"
        );
        assert_eq!(early.len, 40);

        let full = build(MAX_MATCH as i32);
        assert_eq!(full.match_start, Some(200));
        assert_eq!(full.len, 100);
    }

    /// `--chain_length != 0` (`deflate.c` L1526): a budget of one examines only
    /// the head of the chain.
    #[test]
    fn a_chain_budget_of_one_examines_only_the_head() {
        let build = |max_chain_length: usize| {
            let mut fixture = Fixture::new(0x7777);
            fixture.params.max_chain_length = max_chain_length;
            fixture.plant_exact(300, 40);
            fixture.decoy(700);
            fixture.chain(&[700, 300]);
            fixture.run(700)
        };

        let exhausted = build(1);
        assert_eq!(
            exhausted.match_start, None,
            "a budget of one must not reach the second candidate"
        );
        assert_eq!(exhausted.len, 2);

        let sufficient = build(2);
        assert_eq!(sufficient.match_start, Some(300));
        assert_eq!(sufficient.len, 40);
    }

    // -------------------------------------------------------------------------
    //  The unrolled comparison and the return
    // -------------------------------------------------------------------------

    /// The OUT assertion: "the match length is not greater than `s->lookahead`"
    /// (`deflate.c` L1387, L1528-L1529).
    #[test]
    fn the_returned_length_never_exceeds_the_lookahead() {
        let mut fixture = Fixture::new(0x8888);
        fixture.window.lookahead = 30;
        fixture.plant_exact(300, 100);
        fixture.chain(&[300]);
        let outcome = fixture.run(300);
        assert_eq!(
            outcome.match_start,
            Some(300),
            "the candidate is still the best match"
        );
        assert_eq!(outcome.len, 30, "but the length reported is the lookahead");
    }

    /// A match that runs to exactly `strend` returns `MAX_MATCH`, whether it
    /// stops there because the bytes diverge or because the `scan < strend`
    /// guard cuts it off (`deflate.c` L1504-L1509).
    #[test]
    fn a_match_reaching_strend_returns_max_match() {
        // Diverging exactly at offset MAX_MATCH.
        let mut divergent = Fixture::new(0x9999);
        divergent.plant_exact(300, MAX_MATCH);
        divergent.chain(&[300]);
        let outcome = divergent.run(300);
        assert_eq!(outcome.match_start, Some(300));
        assert_eq!(outcome.len, MAX_MATCH);

        // Still identical well past offset MAX_MATCH: the guard, not the data,
        // ends the comparison, and the answer is the same.
        let mut unbounded = Fixture::new(0x9999);
        unbounded.plant(300, MAX_PLANT);
        unbounded.chain(&[300]);
        let outcome = unbounded.run(300);
        assert_eq!(outcome.match_start, Some(300));
        assert_eq!(outcome.len, MAX_MATCH);
    }

    /// One byte short of `strend` returns 257, which is the boundary the
    /// eight-way group cadence has to land on
    /// (`deflate.c` L1496-L1497, L1509).
    #[test]
    fn a_match_one_byte_short_of_strend_returns_257() {
        let mut fixture = Fixture::new(0xAAAA);
        fixture.plant(300, MAX_PLANT);
        // Diverge at offset 257, so the 256th comparison of the last group
        // fails and the scan stops one byte before `strend`.
        let scan_byte = fixture.window.byte(MAX_DIST + 257).unwrap();
        fixture.poke(300 + 257, scan_byte ^ 0xFF);
        fixture.chain(&[300]);
        let outcome = fixture.run(300);
        assert_eq!(outcome.match_start, Some(300));
        assert_eq!(outcome.len, MAX_MATCH - 1);
    }

    // -------------------------------------------------------------------------
    //  DECISION POINT #3 — the deliberately redundant test
    // -------------------------------------------------------------------------

    /// The `match[best_len - 1] != scan_end1` test rejects a candidate that
    /// differs at offset 2 (`deflate.c` L1482-L1491).
    ///
    /// This is why the "redundant" test cannot be dropped. The unrolled
    /// comparison starts at offset 3 and never compares offset 2 — the
    /// reference relies on equal hash keys for that — so a candidate that
    /// differs there and agrees everywhere else would be reported with a length
    /// of `MAX_MATCH` when its real match length is 2. With `prev_length` 3,
    /// `best_len - 1` *is* offset 2, and the test that the reference calls "not
    /// always a win" is the only thing standing between that candidate and the
    /// output.
    #[test]
    fn the_redundant_test_rejects_a_candidate_that_differs_at_offset_two() {
        let mut fixture = Fixture::new(0xBBBB);
        fixture.params.prev_length = 3;
        fixture.plant(300, MAX_PLANT);
        let scan_byte = fixture.window.byte(MAX_DIST + 2).unwrap();
        fixture.poke(300 + 2, scan_byte ^ 0xFF);
        fixture.chain(&[300]);

        // Everything from offset 3 up is identical, so a comparison that began
        // there would run all the way to `strend`.
        let window = fixture.bytes();
        for offset in 3..=MAX_MATCH {
            assert_eq!(
                window[300 + offset],
                window[MAX_DIST + offset],
                "offset {offset} was expected to be identical"
            );
        }

        let outcome = fixture.run(300);
        assert_eq!(
            outcome.match_start, None,
            "the candidate differs at offset 2 and must be rejected"
        );
        assert_eq!(outcome.len, 3, "the result falls back to prev_length");

        // Restore offset 2 and the very same candidate becomes a full-length
        // match, which shows the rejection was caused by that one byte.
        let mut restored = Fixture::new(0xBBBB);
        restored.params.prev_length = 3;
        restored.plant(300, MAX_PLANT);
        restored.chain(&[300]);
        let outcome = restored.run(300);
        assert_eq!(outcome.match_start, Some(300));
        assert_eq!(outcome.len, MAX_MATCH);
    }

    // -------------------------------------------------------------------------
    //  The state-facing entry point and the "match_start is garbage" contract
    // -------------------------------------------------------------------------

    /// [`longest_match`] writes `match_start` when a candidate improves on
    /// `prev_length` and leaves it alone when none does
    /// (`deflate.c` L1382-L1384, L1515).
    ///
    /// The second half is the contract's "`match_start` is garbage" clause. C
    /// achieves it by simply not assigning; so does this port, and this test is
    /// what pins that down — a port that wrote a sentinel, or wrote the last
    /// candidate examined, would compile and pass every other test here while
    /// corrupting `deflate_slow`'s lazy-match bookkeeping.
    #[test]
    fn match_start_is_written_only_when_a_candidate_improves() {
        /// Recognisable value that no real candidate could produce.
        const SENTINEL: usize = 0x5A5A;

        let build = |plant: bool| {
            let mut state = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();
            // `configuration_table[6]`, as `lm_init` loads it
            // (`deflate.c` L689-L692), but with `nice_match` at MAX_MATCH so the
            // early exit does not interfere.
            state.set_tuning(8, 16, MAX_MATCH as i32, 128);
            state.prev_length = 2;

            // The allocator hands over a filled, not zeroed, block, and a window
            // of identical bytes would match everywhere. Lay down a pattern over
            // the region these positions use.
            let mut bytes = [0u8; 1400];
            for (index, slot) in bytes.iter_mut().enumerate() {
                *slot = pattern(0xCCCC, index);
            }
            assert!(state.window.write_at(0, &bytes));

            state.window.strstart = 1000;
            state.window.lookahead = MAX_MATCH;
            state.window.match_start = SENTINEL;

            if plant {
                let mut copy = [0u8; 40];
                assert!(state.window.copy_out(1000, &mut copy));
                assert!(state.window.write_at(400, &copy));
                let next = state.window.byte(1000 + 40).unwrap();
                assert!(state.window.write_at(400 + 40, &[next ^ 0xFF]));
            } else {
                // Force the early rejection, exactly as `Fixture::decoy` does.
                let first = state.window.byte(1000).unwrap();
                assert!(state.window.write_at(400, &[first ^ 0xFF]));
            }

            state.hash.set_prev_at(400, Pos::NIL);
            let len = longest_match(&mut state, IPos::from_index(400).unwrap());
            (len, state.window.match_start)
        };

        // An improving candidate: the length is reported and `match_start` moves
        // to it.
        let (len, match_start) = build(true);
        assert_eq!(len, 40);
        assert_eq!(match_start, 400);

        // No improvement: the result is `prev_length` and `match_start` still
        // holds whatever it held before the call.
        let (len, match_start) = build(false);
        assert_eq!(len, 2, "the result must equal prev_length");
        assert_eq!(
            match_start, SENTINEL,
            "match_start must be left untouched -- it is documented as garbage"
        );
    }

    // -------------------------------------------------------------------------
    //  Differential test against an independent transcription
    // -------------------------------------------------------------------------

    /// The port agrees with an independent, absolute-index transcription of
    /// `deflate.c` L1389-L1529 over a randomised corpus, and every match it
    /// reports is a real match.
    ///
    /// Chains are built the way `INSERT_STRING` builds them, with every member
    /// genuinely agreeing with the current string over its first `MIN_MATCH`
    /// bytes. That models a collision-free hash, which is what makes the second
    /// assertion — the `check_match` equivalent from `deflate.c` L1598-L1621 —
    /// meaningful: with offset 2 guaranteed equal, the length the unrolled
    /// comparison computes is the true common prefix.
    #[test]
    fn search_agrees_with_an_independent_transcription() {
        let mut improvements = 0_usize;
        let mut rejections = 0_usize;

        for seed in 1..=400_u64 {
            let mut rng = Rng::new(seed.wrapping_mul(0x0100_0000_01B3));
            let mut fixture = Fixture::new(seed);

            // `strstart` spans both sides of MAX_DIST so that `limit` is
            // sometimes NIL and sometimes a real floor.
            let strstart = rng.between(MAX_DIST, 1500);
            fixture.window.strstart = strstart;
            fixture.window.lookahead = rng.between(MIN_MATCH, 300);
            // Reserve every sixteenth case for the "no possible improvement"
            // path. `MAX_MATCH` is a valid previous length, and the strict
            // improvement rule means no candidate can replace it. Keeping this
            // deterministic prevents the rejection-coverage assertion below
            // from depending on the random corpus's incidental match lengths.
            let prev_length = if seed.trailing_zeros() >= 4 {
                MAX_MATCH
            } else {
                rng.between(1, 30)
            };
            fixture.params = MatchParams {
                max_chain_length: rng.between(1, 10),
                prev_length,
                good_match: rng.between(0, 40),
                nice_match: rng.between(1, 300) as i32 - 20,
            };

            // Candidates spaced at least 8 apart so that the MIN_MATCH prefix
            // pass below cannot clobber a neighbour's first three bytes.
            let count = rng.between(1, 5);
            let mut positions = [0_usize; 5];
            let span = (strstart - MAX_PLANT - 1) / count;
            for (slot, position) in positions.iter_mut().take(count).enumerate() {
                let low = 1 + slot * span;
                *position = rng.between(low, low + span.saturating_sub(8).max(1));
            }
            let mut chain = [0_usize; 5];
            // Most recent first, which is descending window order.
            for (destination, source) in chain.iter_mut().zip(positions.iter().take(count).rev()) {
                *destination = *source;
            }

            // Long plants first, then the MIN_MATCH prefix pass, so that an
            // overlapping plant can shorten a neighbour's match but can never
            // break the "agrees over MIN_MATCH bytes" invariant.
            for position in positions.iter().take(count) {
                let len = rng.between(0, MAX_PLANT);
                if len >= MIN_MATCH {
                    fixture.plant(*position, len);
                }
            }
            for position in positions.iter().take(count) {
                fixture.plant(*position, MIN_MATCH);
            }

            fixture.chain(&chain[..count]);
            let head = chain[0];

            let got = fixture.run(head);
            let expected = reference(
                fixture.bytes(),
                fixture.hash.prev_entries(),
                W_SIZE - 1,
                strstart,
                fixture.window.lookahead,
                MAX_DIST,
                fixture.params,
                head as u32,
            );
            assert_eq!(
                (got.len, got.match_start),
                expected,
                "seed {seed}: the port and the transcription disagree"
            );

            match got.match_start {
                Some(start) => {
                    improvements += 1;
                    // `check_match` (`deflate.c` L1598-L1621): the reported
                    // match must actually be a match.
                    let window = fixture.bytes();
                    for offset in 0..got.len {
                        assert_eq!(
                            window[start + offset],
                            window[strstart + offset],
                            "seed {seed}: reported match at {start} is not a match at offset {offset}"
                        );
                    }
                }
                None => rejections += 1,
            }
        }

        // The corpus has to exercise both outcomes, or the agreement above would
        // be vacuous.
        assert!(
            improvements > 100,
            "too few candidates improved on prev_length: {improvements}"
        );
        assert!(
            rejections > 20,
            "too few candidates were rejected: {rejections}"
        );
    }
}
