//! The rolling hash key and the `prev`/`head` chains it indexes.
//!
//! Mirrors `UPDATE_HASH` (`deflate.c` L135-L141), `INSERT_STRING` (L144-L164) and `CLEAR_HASH`
//! (L166-L175), together with the state members they operate on: `prev` (`deflate.h` L138-L142),
//! `head` (L144), `ins_h`, `hash_size`, `hash_bits`, `hash_mask` (L146-L149), `hash_shift`
//! (L151-L156) and `slid` (L285-L286).
//!
//! # What the structure is
//!
//! Every three-byte string in the window is registered in a hash table so that the match
//! routines can find earlier occurrences of it. `doc/algorithm.txt` §1 states the whole design in
//! four sentences, and they are the reason nothing here may be made cleverer:
//!
//! > Duplicated strings are found using a hash table. All input strings of length 3 are inserted
//! > in the hash table. A hash index is computed for the next 3 bytes. If the hash chain for this
//! > index is not empty, all strings in the chain are compared with the current input string, and
//! > the longest match is selected.
//! >
//! > The hash chains are searched starting with the most recent strings, to favor small distances
//! > and thus take advantage of the Huffman encoding. The hash chains are singly linked. There are
//! > no deletions from the hash chains, the algorithm simply discards matches that are too old.
//!
//! Concretely: `head[h]` is the most recent window position whose three-byte prefix hashed to
//! `h`, `prev[p & w_mask]` is the next older position on the same chain, and `NIL` terminates it
//! (`deflate.c` L85-L86). [`insert_string`] pushes a position onto the front of its chain, which
//! is what makes the walk in `longest_match` most-recent-first.
//!
//! **This module decides nothing about the output and everything about the input to that
//! decision.** It emits no bit and chooses no match, yet it fixes *which* candidate positions
//! `longest_match` will ever see and *in what order* it will see them. Two of the eight decision
//! points that determine byte-identical compressed output — the hash-chain walk and the lazy-match
//! insertion pattern — read what is built here, so a "better" hash function, a different
//! insertion order or a chain that dropped stale entries would all be correct DEFLATE and wrong
//! output.
//!
//! # The running key and the `hash_shift` invariant
//!
//! The key is not recomputed from scratch at each position. It is rolled forward one byte at a
//! time by [`update_hash`], which relies on the IN assertion at `deflate.c` L137-L139: "all calls
//! to `UPDATE_HASH` are made with consecutive input characters, so that a running hash key can be
//! computed from the previous key instead of complete recalculation each time". [`seed_hash`]
//! restarts the key wherever that chain of consecutive bytes is broken.
//!
//! What makes the rolled key equal to a key computed from three bytes alone is the invariant on
//! `hash_shift`, quoted from `deflate.h` L152-L155: "It must be such that after `MIN_MATCH` steps,
//! the oldest byte no longer takes part in the hash key, that is: `hash_shift * MIN_MATCH >=
//! hash_bits`". `deflateInit2_` establishes it as `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`
//! (`deflate.c` L456), giving 3 at `memLevel` 1 up to 6 at `memLevel` 9;
//! [`debug_assert_hash_shift_invariant`] re-checks it at every insertion in a debug build.
//!
//! # One copy of the arithmetic, three call sites
//!
//! `INSERT_STRING` is a macro, and the reference implementation open-codes its body twice more
//! where no `match_head` is wanted: in `deflateSetDictionary` (`deflate.c` L600-L607) and in
//! `fill_window`'s priming loop (L322-L332). All three sites are served from here —
//! [`insert_string`] for the macro, [`insert_string_no_head`] for the two inlined copies — so the
//! insertion order cannot drift between them. The seed pair `ins_h = window[str]` followed by one
//! `UPDATE_HASH` likewise appears twice, at L317-L318 and at L1922-L1923, and is
//! [`seed_hash`].
//!
//! # Where the neighbouring halves live
//!
//! * The three chain assignments themselves belong to [`crate::weak_slice::HashChains`], whose
//!   `insert_string` is "an exact mirror of `INSERT_STRING` minus the hash update"; this module owns
//!   the hash update and the composition. `NIL`, [`Pos`](crate::weak_slice::Pos),
//!   [`IPos`](crate::weak_slice::IPos) and [`MIN_MATCH`] are that module's too, and are imported
//!   here rather than redeclared.
//! * `slide_hash` (`deflate.c` L177-L211) is deliberately **not** here. It belongs to the window
//!   module, which slides the window and the tables together; it reaches the per-entry rule
//!   through `HashChains::slide`, and calls [`clear_hash`] for the cases that discard the history
//!   outright (`deflate.c` L805 and L1247).
//! * `ins_h = 0` is not part of `CLEAR_HASH`. `lm_init` sets it separately (`deflate.c` L700), so
//!   [`clear_hash`] must not touch the running key — see its documentation.
//!
//! # Deliberate non-features
//!
//! * The `#ifdef FASTEST` variant of `INSERT_STRING` (`deflate.c` L154-L158) maintains **no**
//!   chains at all: it writes `head` and never `prev`. `FASTEST` also forces the compression level
//!   to 1 and swaps in a two-entry `configuration_table`, so it is a different encoder producing
//!   different bytes. Only the shipped `#else` branch at L160-L163 is implemented.
//! * Nothing here is vectorised or widened. Reading two or four bytes at a time to compute a key
//!   would change which positions collide, and therefore the emitted distances. The output-neutral
//!   `simd` feature is confined to the checksum modules for exactly this reason.
//!
//! # `prev` is uninitialized on purpose, and read anyway
//!
//! `CLEAR_HASH` clears only `head`; "prev[] will be initialized on the fly" (`deflate.c` L168).
//! An entry of `prev` belonging to a position that is not on any chain therefore holds whatever
//! the allocator left behind — the reference implementation says so itself, "If n is not on any
//! hash chain, `prev[n]` is garbage but its value will never be used" (`deflate.c` L204-L206) —
//! and the chain walk reads it regardless, at `while ((cur_match = prev[cur_match & wmask]) >
//! limit && --chain_length != 0)` (L1525). That is safe because `limit` and `chain_length` bound
//! the walk, not because the value is meaningful.
//!
//! This implementation reproduces the tolerance without reproducing the hazard. Nothing here reads
//! uninitialized memory: the buffers arrive from the caller's allocator holding *defined* bytes,
//! [`crate::weak_slice::HashChains`] hands out `u16` values through bounds-checked accessors, and
//! clearing `prev` would be a gratuitous deviation that costs time and changes nothing.
//!
//! # Safety and failure posture
//!
//! No `unsafe`, no raw pointer, no pointer walking: the crate root's `#![forbid(unsafe_code)]`
//! makes that a compiler-enforced property rather than a claim, and `head` and `prev` are reached
//! only as owned slices behind integer indices. No path here can panic and none can index out of
//! range, because every index is masked into range by the data structure itself — `ins_h` by
//! `hash_mask`, a window offset by `w_mask` — and the one unmasked read, of the window byte being
//! folded in, goes through an [`Option`]-returning accessor. The impossible arms are documented
//! where they appear and leave the state untouched rather than corrupting a chain.

use crate::deflate::state::{Allocator, DeflateState};
use crate::weak_slice::{IPos, MIN_MATCH};

/// The seed in [`seed_hash`] folds exactly `MIN_MATCH - 1` bytes, which implements the
/// compile-time guard the reference implementation places beside both of its seed sites:
///
/// ```text
/// #if MIN_MATCH != 3
///     Call UPDATE_HASH() MIN_MATCH-3 more times
/// #endif
/// ```
///
/// (`deflate.c` L319-L321 and L1924-L1926 — the body is deliberately not valid C, so that raising
/// `MIN_MATCH` fails the build rather than silently mis-hashing.) A build error here means the
/// same thing: [`seed_hash`] would need `MIN_MATCH - 3` further folds.
const _: () = assert!(MIN_MATCH == 3);

/// Folds one input byte into the running hash key.
///
/// Mirrors `UPDATE_HASH` (`deflate.c` L141):
///
/// ```text
/// #define UPDATE_HASH(s,h,c) (h = (((h) << s->hash_shift) ^ (c)) & s->hash_mask)
/// ```
///
/// `h` is the previous key — `s->ins_h` at every call site — and `c` is the byte being folded in.
/// The result is the new key, already reduced to `hash_bits` bits by `hash_mask`, so it indexes
/// `head` without any further reduction.
///
/// Callers must advance one position at a time; see the module documentation for the IN assertion
/// that makes a rolled key meaningful, and [`seed_hash`] for restarting one.
///
/// # Why the shift is masked rather than checked
///
/// In C `h` is a `uInt`, so `h << hash_shift` discards whatever passes bit 31. Writing that as a
/// plain `<<` on a `usize` would panic on overflow in a debug build and wrap silently in a release
/// build — one expression with two behaviours, which an implementation that must reproduce compressed output
/// byte for byte cannot have. [`usize::wrapping_shl`] discards the high bits unconditionally
/// instead, and the result is bit-identical to C's:
///
/// * `hash_mask` is `hash_size - 1` with `hash_bits <= 16` (`deflate.c` L454-L455), so at most
///   bits 0 to 15 survive the mask and everything C drops at bit 32 and above is dropped here too.
/// * In fact neither implementation ever drops anything. `h` is either a previous masked key, so
///   `h <= hash_mask <= 0xffff`, or a single byte from [`seed_hash`], so `h <= 0xff <= hash_mask`
///   because `hash_bits >= 8`; with `hash_shift <= 6` that leaves `h << hash_shift <= 0x3f_ffc0`,
///   comfortably inside a `uInt`.
///
/// # Examples
///
/// The default configuration, `memLevel` 8, gives `hash_shift` 5 and `hash_mask` 0x7fff. Rolling
/// the three bytes of `"abc"` through a zero key reproduces the values the C macro produces:
///
/// The values are shown rather than compiled because this function is crate-private; the same
/// three assertions run in this module's test suite.
///
/// ```text
/// let h = update_hash(5, 0x7fff, 0, b'a');
/// assert_eq!(h, 0x0061);
/// let h = update_hash(5, 0x7fff, h, b'b');
/// assert_eq!(h, 0x0c42);
/// let h = update_hash(5, 0x7fff, h, b'c');
/// assert_eq!(h, 0x0823);
/// ```
#[inline]
pub(crate) fn update_hash(hash_shift: usize, hash_mask: usize, h: usize, c: u8) -> usize {
    // `wrapping_shl` takes its exponent as a `u32`, and `hash_shift` is held as a `usize` because
    // every other use of it is an index. The conversion is exact for every live state:
    // `hash_shift` is `(hash_bits + MIN_MATCH - 1) / MIN_MATCH` with `hash_bits` in `8..=16`
    // (`deflate.c` L453 and L456), hence 3..=6. The fallback is unreachable and exists only so
    // that this function has no panicking path at all; `wrapping_shl` masks its exponent, so even
    // taking it would produce a value rather than a panic.
    let shift = u32::try_from(hash_shift).unwrap_or(u32::MAX);

    (h.wrapping_shl(shift) ^ usize::from(c)) & hash_mask
}

/// Re-checks the `hash_shift` invariant, in debug builds only.
///
/// `deflate.h` L151-L156 requires `hash_shift * MIN_MATCH >= hash_bits` so that "after
/// `MIN_MATCH` steps, the oldest byte no longer takes part in the hash key". Every key this module
/// rolls forward depends on it: without it, the key at a position would still carry bytes from
/// before that position and two occurrences of the same three bytes could land on different
/// chains, which changes the matches found and therefore the emitted bytes.
///
/// `deflateInit2_` derives `hash_shift` from `hash_bits` (`deflate.c` L456) so the invariant holds
/// by construction, and this is a `debug_assert!` for that reason: it costs nothing in a release
/// build and cannot abort a shipped library, while still catching a state assembled by hand — in a
/// test, or by a future `deflateParams`-style path — that violates it.
#[inline]
fn debug_assert_hash_shift_invariant(hash_shift: usize, hash_bits: u32) {
    // `usize::try_from` cannot fail for the `hash_bits` of any live state (8..=16) on any target
    // this implementation supports; the `usize::MAX` fallback would fail the assertion loudly rather than
    // pass it silently, which is the right way round for a check.
    let bits = usize::try_from(hash_bits).unwrap_or(usize::MAX);

    debug_assert!(
        hash_shift.saturating_mul(MIN_MATCH) >= bits,
        "hash_shift * MIN_MATCH >= hash_bits violated (deflate.h L151-L156)"
    );
}

/// Restarts the running key from the first two bytes of the string at `str_pos`.
///
/// Mirrors the seed pair that the reference implementation open-codes at both places where the
/// chain of consecutive bytes is broken and a key must be built from nothing —
/// `fill_window` once new input has arrived (`deflate.c` L317-L318):
///
/// ```text
/// s->ins_h = s->window[str];
/// UPDATE_HASH(s, s->ins_h, s->window[str + 1]);
/// ```
///
/// and `deflate_fast` after it has skipped a match too long to insert (L1922-L1923, identical
/// but reading `s->strstart`). The first byte is assigned **unmasked**, exactly as C does: it is a
/// single byte and `hash_mask >= 0xff`, so masking it would be a no-op that only obscured the
/// correspondence.
///
/// The key is left one fold short of a complete three-byte key on purpose. It becomes the key of
/// the string at `str_pos` when [`insert_string`] folds in `window[str_pos + MIN_MATCH - 1]`,
/// which is the division of labour the two C sites use as well: both are immediately followed by a
/// loop whose first statement is `UPDATE_HASH(s, s->ins_h, s->window[str + MIN_MATCH-1])`. See
/// [`MIN_MATCH`] and the `const` assertion above for why exactly one fold is seeded here.
///
/// Both window bytes are read before anything is written, so a position whose second byte lies
/// outside the window — unreachable, since `fill_window` only seeds after
/// `lookahead + insert >= MIN_MATCH` (`deflate.c` L315) — leaves the key untouched instead of
/// half-updated.
#[inline]
pub(crate) fn seed_hash<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, str_pos: usize) {
    debug_assert_hash_shift_invariant(state.hash_shift, state.hash_bits());

    // `s->window[str]` and `s->window[str + 1]`. `checked_add` and the `None` arm cannot be
    // reached from a live stream: a seed is only issued where at least `MIN_MATCH` bytes are
    // available at `str_pos`, and `fill_window` maintains `strstart <= window_size -
    // MIN_LOOKAHEAD` (`deflate.c` L374-L375), which leaves 262 bytes of slack.
    let Some(second_index) = str_pos.checked_add(1) else {
        return;
    };
    let (Some(first), Some(second)) = (state.window.byte(str_pos), state.window.byte(second_index))
    else {
        return;
    };

    state.ins_h = usize::from(first);
    state.ins_h = update_hash(state.hash_shift, state.hash_mask(), state.ins_h, second);
}

/// Inserts the three-byte string at `str_pos` into the hash chains and returns the previous head
/// of its chain.
///
/// Mirrors the shipped `INSERT_STRING` (`deflate.c` L160-L163).
///
/// The order of the four steps is load-bearing and is reproduced exactly:
///
/// 1. fold `window[str_pos + MIN_MATCH - 1]` into `ins_h`, completing the key of this string;
/// 2. read `head[ins_h]`, the most recent position on that chain;
/// 3. store that value into `prev[str_pos & w_mask]`, linking this position to the older one;
/// 4. overwrite `head[ins_h]` with `str_pos`, making this position the new front of the chain.
///
/// The returned head is therefore the value read in step 2, **before** step 4 — that is what C's
/// chained assignment `match_head = s->prev[...] = s->head[s->ins_h]` expresses. Returning the
/// post-update value instead would hand `longest_match` the current position as its own candidate
/// and every string would appear to match itself.
///
/// Steps 2 to 4 are [`crate::weak_slice::HashChains::insert_string`], which owns them because it
/// owns the two arrays; this function owns step 1 and the composition. The `(Pos)(str)` cast in
/// step 4 is a deliberate 16-bit narrowing — `prev` and `head` hold `Pos`, and window offsets stay
/// below `2 * w_size <= 65536` (`deflate.h` L100-L102) — and is performed there by
/// `Pos::from_index`.
///
/// # Return value
///
/// [`IPos`], not `Pos`, because that is the type C's `match_head` has at both call sites
/// (`IPos hash_head` at `deflate.c` L1858 and L1957) and the type `longest_match` accepts (L1389).
/// A result of [`IPos::NIL`] means the chain was empty. Note that window index 0 is
/// indistinguishable from `NIL`, which is intentional: `longest_match` sets its walk limit so that
/// "we prevent matches with the string of window index 0" (`deflate.c` L1396-L1400), so no
/// candidate is lost by the ambiguity.
#[inline]
pub(crate) fn insert_string<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    str_pos: usize,
) -> IPos {
    debug_assert_hash_shift_invariant(state.hash_shift, state.hash_bits());

    // Step 1's operand: `s->window[(str) + (MIN_MATCH-1)]`, the third byte of the string. The
    // first two are already in the running key, which is what the IN assertion at
    // `deflate.c` L150-L152 buys — "all calls to INSERT_STRING are made with consecutive input
    // characters and the first MIN_MATCH bytes of str are valid".
    //
    // Neither the addition nor the read can fail for a live stream: `INSERT_STRING` is issued
    // only where `lookahead >= MIN_MATCH` (`deflate.c` L1879 and L1979) or, in the insert-all
    // loops, where "strstart never exceeds WSIZE-MAX_MATCH, so there are always MIN_MATCH bytes
    // ahead" (L1912-L1914). Declining to touch anything is nevertheless the right answer for the
    // unreachable case: it leaves the key and both chains exactly as they were and reports an
    // empty chain, so the caller emits a literal rather than a wrong distance.
    let Some(last_index) = str_pos.checked_add(MIN_MATCH - 1) else {
        return IPos::NIL;
    };
    let Some(last_byte) = state.window.byte(last_index) else {
        return IPos::NIL;
    };

    // Step 1: UPDATE_HASH(s, s->ins_h, ...).
    state.ins_h = update_hash(state.hash_shift, state.hash_mask(), state.ins_h, last_byte);

    // Steps 2 to 4, in that order. `insert_string` returns `None` only when `str_pos` does not fit
    // a `Pos`, which the successful window read above has already ruled out: that read proves
    // `str_pos + MIN_MATCH - 1 < window_size <= 65536`, hence `str_pos <= 65533`.
    let key = state.ins_h;
    let Some(previous_head) = state.hash.insert_string(key, str_pos) else {
        return IPos::NIL;
    };

    IPos::from_pos(previous_head)
}

/// Inserts the string at `str_pos` into the hash chains, discarding the previous chain head.
///
/// The two places where the reference implementation open-codes `INSERT_STRING`'s body without a
/// `match_head` argument to copy the head into: `fill_window`'s priming loop
/// (`deflate.c` L322-L332) and `deflateSetDictionary`'s insertion loop (L600-L607). Both read
///
/// which is [`insert_string`] with the result dropped — the head is read either way, because
/// `prev[str & w_mask]` is assigned from it. Defining this in terms of [`insert_string`] rather
/// than beside it is the point: there is one copy of the arithmetic in this implementation, so the three call
/// sites cannot drift apart the way three macro expansions can.
#[inline]
pub(crate) fn insert_string_no_head<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    str_pos: usize,
) {
    let _previous_head = insert_string(state, str_pos);
}

/// Clears the hash heads and the slid flag.
///
/// Mirrors `CLEAR_HASH` (`deflate.c` L170-L175), all three of its statements.
///
/// Zeroing the last entry separately and then the other `hash_size - 1` is a 16-bit-target
/// workaround, not semantics: on such a target `hash_size * sizeof(Pos)` can be exactly 64 KiB and
/// the byte count would wrap to zero, which is what the comment at `deflate.c` L167 means by
/// "avoiding 64K overflow for 16 bit systems". Filling the whole `head` slice in one go is
/// therefore equivalent, and it is what [`crate::weak_slice::HashChains::clear`] does, alongside
/// clearing `slid`.
///
/// Two things this must **not** do:
///
/// * **`prev` is not cleared.** "prev[] will be initialized on the fly" (`deflate.c` L168). An
///   entry belonging to a position that is not on any chain keeps whatever the allocator left
///   there, and the chain walk's `limit` and `chain_length` bounds are what make that harmless
///   (L1525, L204-L206). Clearing it would cost time proportional to the window on every reset and
///   change nothing observable.
/// * **`ins_h` is not reset.** `CLEAR_HASH` does not touch the running key; `lm_init` assigns
///   `s->ins_h = 0` as a separate statement (`deflate.c` L700), and the three other callers —
///   `deflateSetDictionary` (L582), `deflateParams` (L805) and `deflate`'s `Z_FULL_FLUSH` handling
///   (L1247) — leave the key alone entirely. Resetting it here would change the keys computed
///   after a mid-stream flush and therefore the emitted bytes.
#[inline]
pub(crate) fn clear_hash<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    state.hash.clear();
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is the right policy for library code and the
// wrong one for a harness: a test asserts, and a failing assertion panics. Indexing is allowed
// because every expectation below is written against a literal, known-good index. The same
// relaxation, for the same reason, appears in `deflate/state.rs`.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{clear_hash, insert_string, insert_string_no_head, seed_hash, update_hash};
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL,
        MAX_MEM_LEVEL, MAX_WBITS, MIN_MEM_LEVEL,
    };
    use crate::weak_slice::{IPos, Pos, MAX_HASH_BITS, MIN_HASH_BITS, MIN_MATCH, NIL};

    /// The smallest window a live state can have: `deflateInit2_` accepts `windowBits >= 8` and
    /// then promotes 8 to 9, "until 256-byte window bug fixed" (`deflate.c` L435-L439). Small
    /// enough to make a whole-array comparison cheap.
    const SMALL_WBITS: i32 = 9;

    /// The smallest hash table: `memLevel` 1 gives `hash_bits` 8, `hash_shift` 3 and `hash_mask`
    /// 0xff (`deflate.c` L453-L456).
    const SMALL_MEM_LEVEL: i32 = 1;

    /// Period-3 input, so every third position holds the same three bytes and — by the
    /// `hash_shift` invariant — the same key. This is what puts several positions on one chain.
    const PERIODIC: [u8; 12] = *b"abcabcabcabc";

    /// `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`, the derivation `deflateInit2_` performs at
    /// `deflate.c` L456, re-derived here so the expectations below do not simply read the value
    /// back out of the state that produced them.
    ///
    /// `clippy::manual_div_ceil` would have this spelled `hash_bits.div_ceil(MIN_MATCH)`. It is
    /// allowed rather than taken: the point of this helper is to be the C expression, character
    /// for character, and a reviewer comparing it against L456 should not have to translate.
    #[allow(clippy::manual_div_ceil)]
    fn c_hash_shift(hash_bits: usize) -> usize {
        (hash_bits + MIN_MATCH - 1) / MIN_MATCH
    }

    /// The state every caller of this module sees: sized as `deflateInit2_` sizes it
    /// (`deflate.c` L449-L456) and cleared as `lm_init` clears it through `CLEAR_HASH` (L685).
    ///
    /// The clear is doing real work: `GlobalAllocator` fills fresh blocks with `0xa5` in debug
    /// builds, the way `test/infcover.c` L87 does, precisely so that code assuming zeroed memory
    /// is caught.
    fn cleared_state(window_bits: i32, mem_level: i32) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits,
            mem_level,
            strategy: Strategy::Default,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        clear_hash(&mut state);
        state
    }

    /// [`cleared_state`] with `bytes` copied to the front of the window.
    fn with_window(
        window_bits: i32,
        mem_level: i32,
        bytes: &[u8],
    ) -> DeflateState<'static, GlobalAllocator> {
        let mut state = cleared_state(window_bits, mem_level);
        assert!(state.window.write_at(0, bytes));
        state
    }

    /// A deterministic 1 KiB fixture with no short period, so keys collide the way real input
    /// makes them collide rather than the way a constant fixture would.
    fn ramp_window() -> [u8; 1024] {
        let mut bytes = [0u8; 1024];
        let mut value = 7u8;
        for slot in &mut bytes {
            *slot = value;
            value = value.wrapping_mul(31).wrapping_add(11);
        }
        bytes
    }

    /// `INSERT_STRING` (`deflate.c` L160-L163) written out again straight from the C text, reaching
    /// the two arrays through their raw entry slices so that it shares no code with the functions
    /// under test. Returns C's `match_head`.
    fn reference_insert_string(
        state: &mut DeflateState<'static, GlobalAllocator>,
        str_pos: usize,
    ) -> u16 {
        // UPDATE_HASH(s, s->ins_h, s->window[(str) + (MIN_MATCH-1)])
        let c = state.window.byte(str_pos + MIN_MATCH - 1).unwrap();
        state.ins_h = ((state.ins_h << state.hash_shift) ^ usize::from(c)) & state.hash_mask();
        // match_head = s->prev[(str) & s->w_mask] = s->head[s->ins_h]
        let key = state.ins_h;
        let head = state.hash.head_entries()[key];
        let slot = str_pos & state.w_mask();
        state.hash.prev_entries_mut()[slot] = head;
        // s->head[s->ins_h] = (Pos)(str)
        state.hash.head_entries_mut()[key] = u16::try_from(str_pos).unwrap();
        head
    }

    /// The seed pair (`deflate.c` L317-L318), likewise written out from the C text.
    fn reference_seed_hash(state: &mut DeflateState<'static, GlobalAllocator>, str_pos: usize) {
        state.ins_h = usize::from(state.window.byte(str_pos).unwrap());
        let second = state.window.byte(str_pos + 1).unwrap();
        state.ins_h = ((state.ins_h << state.hash_shift) ^ usize::from(second)) & state.hash_mask();
    }

    #[test]
    fn update_hash_matches_the_c_macro_at_the_default_memory_level() {
        // Produced by running the real `UPDATE_HASH` macro (`deflate.c` L141) with `uInt` fields.
        // The `(0x4000, 0x01) -> 0x0001` row is the interesting one: the shift pushes the set bit
        // clean out of the mask, which is why the implementation must mask rather than check.
        const CASES: [(usize, u8, usize); 10] = [
            (0x0000, 0x00, 0x0000),
            (0x0000, 0x61, 0x0061),
            (0x0061, 0x62, 0x0c42),
            (0x0c42, 0x63, 0x0823),
            (0x7fff, 0xff, 0x7f1f),
            (0x7fff, 0x00, 0x7fe0),
            (0x0001, 0xff, 0x00df),
            (0x4000, 0x01, 0x0001),
            (0x1234, 0x5a, 0x46da),
            (0x00ff, 0xff, 0x1f1f),
        ];

        // `memLevel` 8 gives `hash_bits` 15, `hash_shift` 5 and `hash_mask` 0x7fff.
        let state = cleared_state(MAX_WBITS, DEF_MEM_LEVEL);
        assert_eq!(state.hash_bits(), 15);
        assert_eq!(state.hash_shift, 5);
        assert_eq!(state.hash_mask(), 0x7fff);

        for (h, c, expected) in CASES {
            assert_eq!(
                update_hash(state.hash_shift, state.hash_mask(), h, c),
                expected
            );
            // Whatever comes out indexes `head` directly, so it must already be reduced.
            assert!(expected <= state.hash_mask());
        }
    }

    #[test]
    fn update_hash_matches_the_c_macro_at_the_smallest_memory_level() {
        // Again from the real macro, with the narrower mask.
        const CASES: [(usize, u8, usize); 8] = [
            (0x00, 0x00, 0x00),
            (0x00, 0x61, 0x61),
            (0x61, 0x62, 0x6a),
            (0x0a, 0x63, 0x33),
            (0xff, 0xff, 0x07),
            (0xff, 0x00, 0xf8),
            (0x01, 0xff, 0xf7),
            (0x20, 0x01, 0x01),
        ];

        // `memLevel` 1 gives `hash_bits` 8, `hash_shift` 3 and `hash_mask` 0xff.
        let state = cleared_state(SMALL_WBITS, SMALL_MEM_LEVEL);
        assert_eq!(state.hash_bits(), 8);
        assert_eq!(state.hash_shift, 3);
        assert_eq!(state.hash_mask(), 0xff);

        for (h, c, expected) in CASES {
            assert_eq!(
                update_hash(state.hash_shift, state.hash_mask(), h, c),
                expected
            );
            assert!(expected <= state.hash_mask());
        }
    }

    #[test]
    fn update_hash_agrees_with_32_bit_uint_arithmetic() {
        // The claim the implementation rests on: computing the running key in a `usize` is bit-identical to
        // computing it in C's `uInt`, because the mask keeps at most `hash_bits <= 16` bits.
        for hash_bits in MIN_HASH_BITS..=MAX_HASH_BITS {
            let bits = usize::try_from(hash_bits).unwrap();
            let hash_shift = c_hash_shift(bits);
            let hash_mask = (1usize << hash_bits) - 1;
            let shift32 = u32::try_from(hash_shift).unwrap();
            let mask32 = u32::try_from(hash_mask).unwrap();

            for h in [
                0,
                1,
                0x0f,
                0xff,
                hash_mask >> 1,
                hash_mask,
                0x1234 & hash_mask,
            ] {
                for c in 0..=u8::MAX {
                    let expected = ((u32::try_from(h).unwrap() << shift32) ^ u32::from(c)) & mask32;
                    assert_eq!(
                        update_hash(hash_shift, hash_mask, h, c),
                        usize::try_from(expected).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn the_hash_shift_invariant_holds_for_every_memory_level() {
        // `(hash_bits + MIN_MATCH - 1) / MIN_MATCH` for `hash_bits` 8..=16 (`deflate.c` L456).
        const EXPECTED_SHIFT: [usize; 9] = [3, 3, 4, 4, 4, 5, 5, 5, 6];

        // `hash_bits = memLevel + 7` (`deflate.c` L453) with `memLevel` in 1..=9, so the reachable
        // range is 8..=16 -- exactly `MIN_HASH_BITS..=MAX_HASH_BITS`.
        assert_eq!(MIN_HASH_BITS, u32::try_from(MIN_MEM_LEVEL + 7).unwrap());
        assert_eq!(MAX_HASH_BITS, u32::try_from(MAX_MEM_LEVEL + 7).unwrap());

        for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
            let state = cleared_state(MAX_WBITS, mem_level);
            let bits = usize::try_from(state.hash_bits()).unwrap();

            assert_eq!(state.hash_bits(), u32::try_from(mem_level + 7).unwrap());
            assert_eq!(state.hash_size(), 1usize << bits);
            assert_eq!(state.hash_mask(), state.hash_size() - 1);
            assert_eq!(state.hash_shift, c_hash_shift(bits));
            assert_eq!(
                state.hash_shift,
                EXPECTED_SHIFT[usize::try_from(mem_level - MIN_MEM_LEVEL).unwrap()]
            );

            // The invariant itself (`deflate.h` L152-L155). `debug_assert_hash_shift_invariant`
            // checks the same thing inside every insertion, which every test below exercises.
            assert!(state.hash_shift * MIN_MATCH >= bits);
        }
    }

    #[test]
    fn the_running_key_depends_only_on_the_last_min_match_bytes() {
        // The invariant's purpose, and the property that makes two occurrences of one three-byte
        // string land on one chain: a key rolled forward from position 0 equals a key built from
        // scratch at the current position.
        let data = ramp_window();
        let mut rolled = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        let shift = rolled.hash_shift;
        let mask = rolled.hash_mask();
        seed_hash(&mut rolled, 0);

        for pos in 0..128 {
            insert_string_no_head(&mut rolled, pos);

            let fresh = update_hash(
                shift,
                mask,
                update_hash(shift, mask, usize::from(data[pos]), data[pos + 1]),
                data[pos + 2],
            );
            assert_eq!(rolled.ins_h, fresh);
        }
    }

    #[test]
    fn seed_hash_builds_the_key_from_exactly_two_bytes() {
        let mut state = with_window(MAX_WBITS, DEF_MEM_LEVEL, b"abcdef");
        // Whatever the key held must be discarded, not folded in.
        state.ins_h = 0x7fff;
        seed_hash(&mut state, 0);
        assert_eq!(state.ins_h, update_hash(5, 0x7fff, usize::from(b'a'), b'b'));
        assert_eq!(state.ins_h, 0x0c42);

        // Folding in the third byte completes the key of the string at 0, which is exactly what
        // `insert_string` does and what the two C seed sites rely on.
        insert_string_no_head(&mut state, 0);
        assert_eq!(state.ins_h, 0x0823);

        // `s->ins_h = s->window[str]` is unmasked in C; with `hash_mask` 0xff a byte of 0xff must
        // survive the assignment intact and only then be shifted out.
        let mut narrow = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &[0xff, 0x00, 0x00, 0x00]);
        narrow.ins_h = 0;
        seed_hash(&mut narrow, 0);
        assert_eq!(narrow.ins_h, update_hash(3, 0xff, 0xff, 0x00));
        assert_eq!(narrow.ins_h, 0xf8);
    }

    #[test]
    fn periodic_input_chains_positions_three_apart() {
        // Position, the key after the insert, and the head the insert reports -- all produced by
        // the real macro over the same window (`deflate.c` L160-L163). Positions 0, 3, 6 and 9
        // share key 0x33 because they hold the same three bytes.
        const EXPECTED: [(usize, usize, u32); 10] = [
            (0, 0x33, 0),
            (1, 0xf9, 0),
            (2, 0xaa, 0),
            (3, 0x33, 0),
            (4, 0xf9, 1),
            (5, 0xaa, 2),
            (6, 0x33, 3),
            (7, 0xf9, 4),
            (8, 0xaa, 5),
            (9, 0x33, 6),
        ];

        let mut state = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &PERIODIC);
        seed_hash(&mut state, 0);
        assert_eq!(state.ins_h, 0x6a);

        for (pos, key, previous) in EXPECTED {
            let reported = insert_string(&mut state, pos);

            assert_eq!(state.ins_h, key);
            // The reported head is the value from *before* the update...
            assert_eq!(reported, IPos::new(previous));
            // ...and the head afterwards is this position.
            assert_eq!(
                state.hash.head_at(key),
                Pos::new(u16::try_from(pos).unwrap())
            );
            // Never the position just inserted, which is what returning the post-update value
            // would produce. Position 0 is exempt because 0 is also `NIL`.
            if pos > 0 {
                assert_ne!(reported, IPos::new(u32::try_from(pos).unwrap()));
            }
            // The link this insert wrote is the head it reported.
            assert_eq!(
                state.hash.prev_at(pos),
                Pos::new(u16::try_from(previous).unwrap())
            );
        }

        // Singly linked and most recent first: 9 -> 6 -> 3 -> 0, then the `NIL` tail.
        let mut chain = [0u16; 4];
        let mut cursor = state.hash.head_at(0x33);
        for slot in &mut chain {
            *slot = cursor.get();
            cursor = state.hash.prev_at(cursor.to_index());
        }
        assert_eq!(chain, [9, 6, 3, 0]);
        assert_eq!(state.hash.prev_at(0), Pos::NIL);
    }

    #[test]
    fn positions_one_window_apart_share_a_prev_slot() {
        // `prev` holds only `w_size` entries, so "an index in this array is thus a window index
        // modulo 32K" (`deflate.h` L139-L142). Two positions exactly `w_size` apart therefore
        // share one slot, and the older link is overwritten rather than kept.
        let mut state = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &[b'x'; 1024]);
        let w_size = state.w_size();
        assert_eq!(w_size, 512);
        assert_eq!(state.hash.prev_entries().len(), w_size);

        state.ins_h = 0;
        assert_eq!(insert_string(&mut state, 10), IPos::NIL);
        let key = state.ins_h;
        assert_eq!(key, 0x78);
        assert_eq!(state.hash.head_at(key), Pos::new(10));
        assert_eq!(state.hash.prev_at(10), Pos::NIL);

        // Land on the same key again, as period-`w_size` input would.
        state.ins_h = 0;
        let reported = insert_string(&mut state, 10 + w_size);
        assert_eq!(state.ins_h, key);
        assert_eq!(reported, IPos::new(10));
        assert_eq!(
            state.hash.head_at(key),
            Pos::new(u16::try_from(10 + w_size).unwrap())
        );
        assert_eq!(state.hash.prev_at(10 + w_size), Pos::new(10));
        assert_eq!(state.hash.prev_at(10), Pos::new(10));
    }

    #[test]
    fn the_three_call_sites_agree_with_the_c_macro_body() {
        // `insert_string`, `insert_string_no_head` and the C text must stay in lockstep: this is
        // the guard on the "one copy of the arithmetic" property, since the reference
        // implementation open-codes this body at `deflate.c` L322-L332 and L600-L607 as well.
        let data = ramp_window();
        let mut ported = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        let mut headless = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        let mut reference = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);

        seed_hash(&mut ported, 0);
        seed_hash(&mut headless, 0);
        reference_seed_hash(&mut reference, 0);
        assert_eq!(ported.ins_h, reference.ins_h);
        assert_eq!(headless.ins_h, reference.ins_h);

        for pos in 0..600 {
            let reported = insert_string(&mut ported, pos);
            insert_string_no_head(&mut headless, pos);
            let expected = reference_insert_string(&mut reference, pos);

            assert_eq!(reported, IPos::new(u32::from(expected)));
            assert_eq!(ported.ins_h, reference.ins_h);
            assert_eq!(headless.ins_h, reference.ins_h);
            assert_eq!(ported.hash.head_entries(), reference.hash.head_entries());
            assert_eq!(ported.hash.prev_entries(), reference.hash.prev_entries());
            assert_eq!(headless.hash.head_entries(), reference.hash.head_entries());
            assert_eq!(headless.hash.prev_entries(), reference.hash.prev_entries());
        }

        // 600 positions past a 512-entry `prev` also proves the wrap-around is shared.
        assert!(600 > usize::try_from(1i32 << SMALL_WBITS).unwrap());
    }

    #[test]
    fn clear_hash_zeroes_every_head_entry_and_clears_slid() {
        let mut state = cleared_state(SMALL_WBITS, SMALL_MEM_LEVEL);

        // Something other than `NIL` in every head, the flag `slide_hash` sets
        // (`deflate.c` L209), a recognisable `prev`, and a live running key.
        for (index, entry) in state.hash.head_entries_mut().iter_mut().enumerate() {
            *entry = u16::try_from(index + 1).unwrap();
        }
        for (index, entry) in state.hash.prev_entries_mut().iter_mut().enumerate() {
            *entry = u16::try_from(0x1000 + index).unwrap();
        }
        state.hash.set_slid(true);
        state.ins_h = 0x5a;
        let prev_before = state.hash.prev_entries().to_vec();
        assert!(state.slid());

        clear_hash(&mut state);

        // All `hash_size` heads, not `hash_size - 1` of them: the C macro's split into a single
        // store plus a `zmemzero` of the rest is a 16-bit-target workaround (`deflate.c` L167).
        assert_eq!(state.hash.head_entries().len(), state.hash_size());
        assert!(state.hash.head_entries().iter().all(|&entry| entry == NIL));
        assert!(!state.slid());
        // "prev[] will be initialized on the fly" (`deflate.c` L168): untouched.
        assert_eq!(state.hash.prev_entries(), &prev_before[..]);
        // `CLEAR_HASH` does not reset the running key; `lm_init` does that separately (L700).
        assert_eq!(state.ins_h, 0x5a);
    }

    #[test]
    fn clear_hash_empties_every_chain() {
        let mut state = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &PERIODIC);
        seed_hash(&mut state, 0);
        for pos in 0..10 {
            insert_string_no_head(&mut state, pos);
        }
        assert_ne!(state.hash.head_at(0x33), Pos::NIL);

        clear_hash(&mut state);

        // Every chain now reports empty, so the next insertion starts a fresh history -- which is
        // exactly what `deflate`'s `Z_FULL_FLUSH` handling wants (`deflate.c` L1247).
        for key in 0..state.hash_size() {
            assert_eq!(state.hash.head_at(key), Pos::NIL);
        }
        let mut fresh = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &PERIODIC);
        seed_hash(&mut fresh, 0);
        assert_eq!(insert_string(&mut fresh, 0), IPos::NIL);
        state.ins_h = fresh.ins_h;
        assert_eq!(insert_string(&mut state, 0), IPos::NIL);
    }

    #[test]
    fn an_insertion_outside_the_window_changes_nothing() {
        // Unreachable from a live stream -- `INSERT_STRING` is issued only where at least
        // `MIN_MATCH` bytes are available at `str_pos` -- but it must degrade to "no candidate"
        // rather than panicking or corrupting a chain.
        let mut state = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &[b'q'; 1024]);
        state.ins_h = 0x2a;
        let heads_before = state.hash.head_entries().to_vec();
        let prev_before = state.hash.prev_entries().to_vec();
        let window_size = state.window_size();
        assert_eq!(window_size, 1024);

        // An insertion folds `window[str_pos + MIN_MATCH - 1]`, so the first position it cannot
        // serve is `window_size - MIN_MATCH + 1`. `usize::MAX` additionally exercises the arm where
        // the offset arithmetic itself would overflow.
        for pos in [window_size - MIN_MATCH + 1, window_size, usize::MAX] {
            assert_eq!(insert_string(&mut state, pos), IPos::NIL);
            insert_string_no_head(&mut state, pos);

            assert_eq!(state.ins_h, 0x2a);
            assert_eq!(state.hash.head_entries(), &heads_before[..]);
            assert_eq!(state.hash.prev_entries(), &prev_before[..]);
        }

        // The last position that does work, for contrast: its third byte is the final window byte.
        assert_eq!(
            insert_string(&mut state, window_size - MIN_MATCH),
            IPos::NIL
        );
        assert_ne!(state.ins_h, 0x2a);
        assert_ne!(state.hash.head_entries(), &heads_before[..]);
    }

    #[test]
    fn a_seed_outside_the_window_changes_nothing() {
        // A seed reads two bytes, not three (`deflate.c` L317-L318), so its boundary is one
        // position later than an insertion's -- and it must leave the key untouched rather than
        // half-updated when the second byte is missing.
        let mut state = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &[b'q'; 1024]);
        state.ins_h = 0x2a;
        let heads_before = state.hash.head_entries().to_vec();
        let prev_before = state.hash.prev_entries().to_vec();
        let window_size = state.window_size();

        for pos in [window_size - 1, window_size, usize::MAX] {
            seed_hash(&mut state, pos);

            assert_eq!(state.ins_h, 0x2a);
            assert_eq!(state.hash.head_entries(), &heads_before[..]);
            assert_eq!(state.hash.prev_entries(), &prev_before[..]);
        }

        // Two bytes are enough, so the position an insertion cannot serve still seeds.
        seed_hash(&mut state, window_size - MIN_MATCH + 1);
        assert_eq!(state.ins_h, update_hash(3, 0xff, usize::from(b'q'), b'q'));
        assert_ne!(state.ins_h, 0x2a);
        // A seed never touches the chains, whatever it does to the key.
        assert_eq!(state.hash.head_entries(), &heads_before[..]);
        assert_eq!(state.hash.prev_entries(), &prev_before[..]);
    }

    #[test]
    fn the_priming_loop_matches_a_sequence_of_headless_inserts() {
        // `fill_window`'s priming loop (`deflate.c` L314-L332) and `deflateSetDictionary`'s
        // insertion loop (L597-L607) differ only in their loop conditions: the body is the same
        // seed-then-insert sequence, so both must land on the same tables.
        let data = ramp_window();

        // `fill_window` shape: seed at `strstart - insert`, then insert while `insert` remains.
        let mut filled = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        let strstart = 40usize;
        let insert = 32usize;
        let start = strstart - insert;
        seed_hash(&mut filled, start);
        for offset in 0..insert {
            insert_string_no_head(&mut filled, start + offset);
        }

        // `deflateSetDictionary` shape: the same positions, reached by its own loop.
        let mut dictionary = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        seed_hash(&mut dictionary, start);
        let mut str_pos = start;
        let mut n = insert;
        while n > 0 {
            insert_string_no_head(&mut dictionary, str_pos);
            str_pos += 1;
            n -= 1;
        }

        assert_eq!(filled.ins_h, dictionary.ins_h);
        assert_eq!(
            filled.hash.head_entries(),
            dictionary.hash.head_entries(),
            "the two inlined C sites must build identical hash heads"
        );
        assert_eq!(filled.hash.prev_entries(), dictionary.hash.prev_entries());

        // And the head-returning macro form agrees with both.
        let mut macro_form = with_window(SMALL_WBITS, SMALL_MEM_LEVEL, &data);
        seed_hash(&mut macro_form, start);
        for offset in 0..insert {
            let reported = insert_string(&mut macro_form, start + offset);
            // Every position here is distinct and none repeats a key often enough to matter; the
            // reported head is only ever `NIL` or an earlier position.
            assert!(reported.to_index().unwrap_or(0) < start + offset);
        }
        assert_eq!(macro_form.ins_h, dictionary.ins_h);
        assert_eq!(
            macro_form.hash.head_entries(),
            dictionary.hash.head_entries()
        );
        assert_eq!(
            macro_form.hash.prev_entries(),
            dictionary.hash.prev_entries()
        );
    }

    #[test]
    fn insertion_works_at_every_memory_level_and_window_size() {
        // The masked indices must stay in range across the whole configuration space, since
        // `hash_size` and `w_size` vary independently.
        let data = ramp_window();

        for window_bits in SMALL_WBITS..=MAX_WBITS {
            for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
                let mut state = with_window(window_bits, mem_level, &data);
                seed_hash(&mut state, 0);

                for pos in 0..64 {
                    let reported = insert_string(&mut state, pos);
                    assert!(state.ins_h <= state.hash_mask());
                    assert_eq!(
                        state.hash.head_at(state.ins_h),
                        Pos::new(u16::try_from(pos).unwrap())
                    );
                    assert_eq!(
                        state.hash.prev_at(pos),
                        Pos::new(u16::try_from(reported.get()).unwrap())
                    );
                }
            }
        }
    }
}
