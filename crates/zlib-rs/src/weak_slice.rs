//! Safe, bounds-checked views over the buffers that the reference implementation walks with
//! raw pointers.
//!
//! # Why this module exists
//!
//! `deflate_state` (`deflate.h` L104-L288) reaches its three working buffers through seven
//! raw pointers, several of which alias one another. Nothing but debug assertions keeps the
//! pointer arithmetic in bounds:
//!
//! | C member | `deflate.h` | Aliasing |
//! |---|---|---|
//! | `Bytef *pending_buf` | L107 | base of one allocation of `LIT_BUFS * lit_bufsize` bytes |
//! | `Bytef *pending_out` | L109 | second pointer into that same allocation (read cursor) |
//! | `uchf *sym_buf` | L230 | third pointer into it, at `pending_buf + lit_bufsize` |
//! | `Bytef *window` | L123 | `2 * w_size` bytes, walked forward by two cursors at once |
//! | `Posf *prev` | L138 | `w_size` entries, indexed by a window offset modulo `w_size` |
//! | `Posf *head` | L144 | `hash_size` entries, indexed by a masked hash value |
//!
//! This module replaces every one of those pointers with an owned (or borrowed) buffer plus
//! integer cursors, and offers exactly the access shapes the DEFLATE algorithm actually
//! needs, so that the `deflate` and `trees` modules never require `unsafe` and never need
//! two mutable views of one allocation:
//!
//! * Reading two window regions at once — what `longest_match` does with `scan` and `match`
//!   (`deflate.c` L1391 and L1436) — is two *shared* borrows, which Rust permits directly.
//!   [`Window::scan_pair`] hands both out after one bounds check.
//! * Appending to the window is one *exclusive* borrow of exactly the free space
//!   ([`Window::free_space_mut`]), so the `read_buf` destination pointer
//!   (`deflate.c` L311) disappears.
//! * Sliding the window down by `w_size` is [`Window::slide_down`], built on
//!   `slice::copy_within`, which is the checked, overlap-correct replacement for the
//!   `zmemcpy` at `deflate.c` L287, L1774 and L1806.
//! * The genuinely awkward case — compressed output being appended to `pending_buf` while
//!   symbols are still being read out of the overlaid `sym_buf` — is served by
//!   [`PendingBuf::split_at_symbol_cursor`], the module's single `split_at_mut`, whose split
//!   point is the very bound the reference implementation asserts in `compress_block`
//!   (`trees.c` L945, `Assert(s->pending < s->lit_bufsize + sx)`).
//!
//! # About the name
//!
//! Despite the name inherited from this implementation's module plan, there is nothing "weak" or
//! unchecked here. [`WeakSlice`] and [`WeakSliceMut`] are a *borrow plus an integer cursor* —
//! never a pointer plus a length. Every accessor in this module is bounds checked, returns
//! [`Option`] (or a `bool`) instead of panicking, and the module contains no `unsafe` code,
//! no raw pointers and no unchecked indexing. The crate root asserts
//! `#![forbid(unsafe_code)]` and the workspace lint table denies `clippy::indexing_slicing`,
//! so the first and third of those are machine-enforced rather than claimed.
//!
//! # Buffer ownership
//!
//! Nothing here allocates. Every view *wraps* a buffer supplied by the caller, which keeps
//! allocator policy in one place (`deflate/state.rs`, through the crate's allocator
//! abstraction) and makes these types trivially testable. The storage parameter `S` is
//! generic over anything that can be viewed as a slice, so `Box<[u8]>`, `Vec<u8>`,
//! `&mut [u8]` and `[u8; N]` are all accepted.
//!
//! Buffers are assumed to arrive **uninitialized**: the default allocator is
//! `malloc`-backed (`zutil.c` L299-L303 selects `malloc` over `calloc` whenever
//! `sizeof(uInt) > 2`, which is every target of interest), which is precisely why
//! [`Window::initialize_win_init_tail`] exists and why the hash arrays must be cleared
//! before use. In Rust the bytes are always *defined* — they are whatever the caller's
//! allocator left behind — so reading them is well defined even where the reference
//! implementation calls the value garbage (`deflate.c` L204-L206).
//!
//! # Fidelity
//!
//! The reference implementation's compressed output must be reproduced byte for byte, so
//! every construct here mirrors the C semantics exactly and each item names the file and
//! line range it derives from. Two deliberate non-features follow from that:
//!
//! * The `LIT_MEM` layout (`LIT_BUFS 5` with separate `d_buf` and `l_buf`, `deflate.h`
//!   L224-L227) is **not** implemented. It is commented out at `deflate.h` L28, so the
//!   shipped layout is the `LIT_BUFS 4` single-`sym_buf` branch (L229-L230), and switching
//!   layouts changes the emitted bytes.
//! * No two-bytes-at-a-time comparison helper is offered. Word-at-a-time `scan_end`
//!   matching is the `#ifdef UNALIGNED_OK` variant (`deflate.c` L1446-L1478), a different
//!   code path from the default byte-comparison branch at L1482-L1485 that this implementation
//!   reproduces.
//!
//! # Provenance
//!
//! The sliding window, the hash chains and the pending buffer, as borrow-checked views.
//!
//! Ported from the `deflate_state` buffer members (`deflate.h`) and the pointer walks in
//! `deflate.c`. The views hand out the disjoint sub-slices the match finder needs without
//! reintroducing the aliasing that the raw-pointer original depends on.
//!
//! [`PendingBuf::split_at_symbol_cursor`]: crate::weak_slice::PendingBuf::split_at_symbol_cursor
//! [`WeakSlice`]: crate::weak_slice::WeakSlice
//! [`WeakSliceMut`]: crate::weak_slice::WeakSliceMut
//! [`Window::free_space_mut`]: crate::weak_slice::Window::free_space_mut
//! [`Window::initialize_win_init_tail`]: crate::weak_slice::Window::initialize_win_init_tail
//! [`Window::scan_pair`]: crate::weak_slice::Window::scan_pair
//! [`Window::slide_down`]: crate::weak_slice::Window::slide_down

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

/// Tail of the hash chains, and therefore also the "no match here" sentinel.
///
/// From `#define NIL 0` (`deflate.c` L85). Window index `0` is deliberately unreachable as
/// a match position: `longest_match` computes
/// `limit = strstart > MAX_DIST(s) ? strstart - MAX_DIST(s) : NIL` and stops as soon as a
/// chain entry drops to `limit`, "to simplify the code, we prevent matches with the string
/// of window index 0" (`deflate.c` L1396-L1400). [`Pos::match_index`] encodes that rule in
/// the type system.
pub const NIL: u16 = 0;

/// Smallest match length the LZ77 stage will emit, from `#define MIN_MATCH 3`
/// (`zutil.h` L92).
pub const MIN_MATCH: usize = 3;

/// Longest match length the LZ77 stage will emit, from `#define MAX_MATCH 258`
/// (`zutil.h` L93).
pub const MAX_MATCH: usize = 258;

/// Minimum lookahead the match routines require, from
/// `#define MIN_LOOKAHEAD (MAX_MATCH+MIN_MATCH+1)` (`deflate.h` L296) — 262 bytes.
///
/// `fill_window` guarantees `strstart <= window_size - MIN_LOOKAHEAD` on exit
/// (`deflate.c` L374-L375), which is what makes a `MAX_MATCH + 1` byte scan starting at
/// `strstart` provably in range.
pub const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes past the end of the data that must be initialized in the window, from
/// `#define WIN_INIT MAX_MATCH` (`deflate.h` L306) — 258 bytes.
///
/// The comment there explains the purpose: the match routines may scan up to
/// `strstart + MAX_MATCH` regardless of `lookahead`, so those bytes are zeroed "in order to
/// avoid memory checker errors from longest match routines". See
/// [`Window::initialize_win_init_tail`].
pub const WIN_INIT: usize = MAX_MATCH;

/// Number of `lit_bufsize`-sized units in the `pending_buf` allocation, from
/// `#define LIT_BUFS 4` (`deflate.h` L229).
///
/// This is the default, shipped layout: `LIT_MEM` is commented out at `deflate.h` L28, so
/// the `LIT_BUFS 5` variant with separate `d_buf`/`l_buf` arrays is not implemented here.
pub const LIT_BUFS: usize = 4;

/// Bytes per entry in the symbol buffer: two little-endian distance bytes followed by one
/// length-or-literal byte.
///
/// From the `_tr_tally_lit` and `_tr_tally_dist` macros (`deflate.h` L357-L375) and from
/// `_tr_tally` itself (`trees.c` L1100-L1102); `compress_block` reads the three bytes back
/// in the same order (`trees.c` L913-L915).
pub const SYMBOL_BYTES: usize = 3;

/// Smallest window exponent a deflate stream can use.
///
/// `deflateInit2_` accepts `windowBits >= 8` (`deflate.c` L435) but immediately promotes 8
/// to 9 — `if (windowBits == 8) windowBits = 9; /* until 256-byte window bug fixed */`
/// (`deflate.c` L439) — so a live `deflate_state` never has `w_bits < 9`. Enforcing that
/// here is what makes [`Window::max_dist`] exact rather than saturating.
pub const MIN_WBITS: u32 = 9;

/// Largest window exponent, from `#define MAX_WBITS 15` (`zconf.h`) and the
/// `windowBits > 15` rejection in `deflateInit2_` (`deflate.c` L435).
pub const MAX_WBITS: u32 = 15;

/// Smallest hash-table exponent, from `hash_bits = memLevel + 7` (`deflate.c` L453) at the
/// minimum `memLevel` of 1 (`deflate.c` L434).
pub const MIN_HASH_BITS: u32 = 8;

/// Largest hash-table exponent, from `hash_bits = memLevel + 7` (`deflate.c` L453) at
/// `MAX_MEM_LEVEL` of 9 (`zconf.h`).
pub const MAX_HASH_BITS: u32 = 16;

/// Smallest symbol-buffer element count, from `lit_bufsize = 1 << (memLevel + 6)`
/// (`deflate.c` L464) at `memLevel == 1`.
///
/// The overlay analysis at `deflate.c` L466-L503 states the same range: "n is
/// `lit_bufsize`, which is 16384 by default, and can range from 128 to 32768".
pub const MIN_LIT_BUFSIZE: usize = 128;

/// Largest symbol-buffer element count, from `lit_bufsize = 1 << (memLevel + 6)`
/// (`deflate.c` L464) at `memLevel == 9`.
pub const MAX_LIT_BUFSIZE: usize = 32_768;

/// An index into the sliding window, stored narrowly.
///
/// The C spelling is `typedef ush Pos` with `typedef Pos FAR Posf` (`deflate.h` L96-L97):
/// "A Pos is an index in the character window. We use short instead of int to save space in
/// the various tables" (`deflate.h` L100-L102). The hash arrays therefore hold 16-bit
/// entries, and `prev` is indexed by a window offset taken modulo `w_size`
/// (`deflate.h` L139-L142).
///
/// Value `0` is [`NIL`], which serves double duty as the tail of a hash chain and as "no
/// match": the reference implementation deliberately prevents matches against window index
/// 0 (`deflate.c` L1396-L1400). Use [`Pos::match_index`] when a value is about to be used
/// as a match position — it yields [`None`] for [`NIL`] — and [`Pos::to_index`] only when
/// the raw stored index is what is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos(u16);

impl Pos {
    /// The chain terminator and "no match" sentinel, equal to [`NIL`].
    pub const NIL: Self = Self(NIL);

    /// Largest window index representable, `u16::MAX`.
    ///
    /// A live `deflate_state` never approaches it: `w_size` is at most 32768
    /// (`deflate.c` L450 with `w_bits <= 15`) and window offsets stay below `2 * w_size`.
    pub const MAX: Self = Self(u16::MAX);

    /// Wraps a raw 16-bit window index, exactly as a C `(Pos)` cast would.
    #[must_use]
    #[inline]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Narrows a `usize` window offset to a [`Pos`], returning [`None`] when it does not
    /// fit.
    ///
    /// This is the checked form of the `(Pos)(str)` cast in `INSERT_STRING`
    /// (`deflate.c` L163) and of `s->head[s->ins_h] = (Pos)str` (`deflate.c` L327). The C
    /// casts truncate silently; a live stream never reaches 65536 because window offsets are
    /// bounded by `2 * w_size <= 65536`, so refusing the impossible value costs nothing and
    /// removes a whole class of silent corruption.
    #[must_use]
    #[inline]
    pub fn from_index(index: usize) -> Option<Self> {
        u16::try_from(index).ok().map(Self)
    }

    /// The raw stored value, including [`NIL`].
    #[must_use]
    #[inline]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The raw stored value widened to a `usize`, including [`NIL`].
    ///
    /// Use this for storage-level work — walking or rewriting the hash arrays — where 0 is
    /// simply the value zero. Use [`Pos::match_index`] where 0 means "no match".
    #[must_use]
    #[inline]
    pub fn to_index(self) -> usize {
        usize::from(self.0)
    }

    /// The window index to match against, or [`None`] when this entry is [`NIL`].
    ///
    /// Encodes the rule at `deflate.c` L1396-L1400: `limit` is never below [`NIL`] and the
    /// chain walk stops at it, so index 0 is not a legal match position. A caller that
    /// honours this cannot accidentally treat a chain terminator as a real candidate.
    #[must_use]
    #[inline]
    pub fn match_index(self) -> Option<usize> {
        if self.0 == NIL {
            None
        } else {
            Some(usize::from(self.0))
        }
    }

    /// Whether this entry is the [`NIL`] chain terminator.
    #[must_use]
    #[inline]
    pub const fn is_nil(self) -> bool {
        self.0 == NIL
    }

    /// Subtracts `w_size` when this entry is at least `w_size`, otherwise yields [`NIL`].
    ///
    /// This is the per-entry body of `slide_hash` (`deflate.c` L196 and L203):
    /// `*p = (Pos)(m >= wsize ? m - wsize : NIL)`. Exposed so that a caller sliding a copy
    /// of the arrays gets identical semantics; [`HashChains::slide`] applies it to both
    /// arrays.
    #[must_use]
    #[inline]
    pub const fn slid_down(self, w_size: u16) -> Self {
        if self.0 >= w_size {
            Self(self.0 - w_size)
        } else {
            Self::NIL
        }
    }
}

impl From<Pos> for u16 {
    #[inline]
    fn from(pos: Pos) -> Self {
        pos.0
    }
}

impl From<Pos> for usize {
    #[inline]
    fn from(pos: Pos) -> Self {
        pos.to_index()
    }
}

/// A window index widened for parameter passing.
///
/// The C spelling is `typedef unsigned IPos` (`deflate.h` L98): "`IPos` is used only for
/// parameter passing" (`deflate.h` L100-L102). It is the type of `longest_match`'s
/// `cur_match` argument (`deflate.c` L1389), of `hash_head` in `deflate_fast` and
/// `deflate_slow`, and of `prev_match` (`deflate.h` L164). `unsigned` is 32 bits on every
/// target this implementation supports, so the Rust mirror is a `u32`.
///
/// [`NIL`] means the same thing here as it does for [`Pos`], because an `IPos` is produced
/// by reading a hash-chain entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct IPos(u32);

impl IPos {
    /// The chain terminator and "no match" sentinel; numerically equal to [`NIL`], spelled
    /// as a literal because a widening `From` conversion is not available in a `const` item.
    pub const NIL: Self = Self(0);

    /// Wraps a raw 32-bit window index, exactly as a C `(IPos)` cast would.
    #[must_use]
    #[inline]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Widens a [`Pos`] read out of a hash chain into an [`IPos`], as
    /// `cur_match = prev[cur_match & wmask]` does (`deflate.c` L1525).
    #[must_use]
    #[inline]
    pub fn from_pos(pos: Pos) -> Self {
        Self(u32::from(pos.get()))
    }

    /// Converts a `usize` window offset to an [`IPos`], returning [`None`] when it does not
    /// fit.
    #[must_use]
    #[inline]
    pub fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index).ok().map(Self)
    }

    /// The raw stored value, including [`NIL`].
    #[must_use]
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The raw stored value as a `usize`, returning [`None`] on a 16-bit target where the
    /// value would not fit.
    #[must_use]
    #[inline]
    pub fn to_index(self) -> Option<usize> {
        usize::try_from(self.0).ok()
    }

    /// The window index to match against, or [`None`] when this value is [`NIL`] or does
    /// not fit a `usize`.
    ///
    /// The [`Pos::match_index`] rule applies unchanged: window index 0 is not a legal match
    /// position (`deflate.c` L1396-L1400).
    #[must_use]
    #[inline]
    pub fn match_index(self) -> Option<usize> {
        if self.is_nil() {
            return None;
        }
        self.to_index()
    }

    /// Whether this value is the [`NIL`] chain terminator.
    #[must_use]
    #[inline]
    pub const fn is_nil(self) -> bool {
        self.0 == Self::NIL.0
    }

    /// Narrows to a [`Pos`] for storage in a hash array, returning [`None`] when it does not
    /// fit.
    #[must_use]
    #[inline]
    pub fn to_pos(self) -> Option<Pos> {
        u16::try_from(self.0).ok().map(Pos::new)
    }
}

impl From<Pos> for IPos {
    #[inline]
    fn from(pos: Pos) -> Self {
        Self::from_pos(pos)
    }
}

impl From<IPos> for u32 {
    #[inline]
    fn from(pos: IPos) -> Self {
        pos.0
    }
}

/// A shared borrow of a slice plus an integer read cursor.
///
/// This is the safe replacement for a C read pointer that is advanced through a buffer —
/// `pending_out` (`deflate.h` L109), `sym_buf` as consumed by `compress_block`'s `sx`
/// (`trees.c` L913-L915), or the `scan`/`match` pointers of `longest_match`
/// (`deflate.c` L1391 and L1436). It is a borrow plus indices; it is not a pointer plus a
/// length, and every accessor is bounds checked.
///
/// The cursor is deliberately independent of the borrow: two cursors over the same shared
/// slice may advance at different rates, which is exactly what the match loops need and
/// what Rust permits for shared borrows without any further machinery.
#[derive(Debug, Clone, Copy)]
pub struct WeakSlice<'a, T> {
    items: &'a [T],
    offset: usize,
}

impl<'a, T> WeakSlice<'a, T> {
    /// Wraps `items` with the cursor at the start.
    #[must_use]
    #[inline]
    pub const fn new(items: &'a [T]) -> Self {
        Self { items, offset: 0 }
    }

    /// Wraps `items` with the cursor at `offset`, or [`None`] when `offset` is past the end.
    ///
    /// The cursor may legally sit *at* the end, which represents a fully consumed buffer.
    #[must_use]
    #[inline]
    pub fn at(items: &'a [T], offset: usize) -> Option<Self> {
        if offset > items.len() {
            return None;
        }
        Some(Self { items, offset })
    }

    /// Total number of elements behind the cursor's slice, consumed or not.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the borrowed slice is empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Current cursor position, counted from the start of the borrowed slice.
    #[must_use]
    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Number of elements still ahead of the cursor.
    #[must_use]
    #[inline]
    pub fn remaining(&self) -> usize {
        self.items.len().saturating_sub(self.offset)
    }

    /// Moves the cursor to `offset`, returning `false` and leaving it untouched when that
    /// would be past the end.
    #[inline]
    pub fn set_offset(&mut self, offset: usize) -> bool {
        if offset > self.items.len() {
            return false;
        }
        self.offset = offset;
        true
    }

    /// Advances the cursor by `count`, returning `false` and leaving it untouched when that
    /// would run past the end.
    #[inline]
    pub fn advance(&mut self, count: usize) -> bool {
        match self.offset.checked_add(count) {
            Some(offset) => self.set_offset(offset),
            None => false,
        }
    }

    /// Element `delta` positions ahead of the cursor, without moving it.
    #[must_use]
    #[inline]
    pub fn peek(&self, delta: usize) -> Option<&'a T> {
        self.items.get(self.offset.checked_add(delta)?)
    }

    /// Element at the cursor, advancing it by one.
    ///
    /// Named `take_next` rather than `next` because this type is deliberately not an
    /// [`Iterator`]: `longest_match` and `compress_block` also need random access and cursor
    /// rewinding, which an iterator cannot express.
    #[inline]
    pub fn take_next(&mut self) -> Option<&'a T> {
        let item = self.items.get(self.offset)?;
        self.offset = self.offset.saturating_add(1);
        Some(item)
    }

    /// The whole borrowed slice, ignoring the cursor.
    #[must_use]
    #[inline]
    pub const fn as_slice(&self) -> &'a [T] {
        self.items
    }

    /// The elements from the cursor to the end.
    #[must_use]
    #[inline]
    pub fn remainder(&self) -> &'a [T] {
        // The `None` arm is unreachable: every mutator rejects an offset past the end.
        // `unwrap_or_default` yields an empty slice there, which keeps this accessor total
        // rather than introducing a panic path.
        self.items.get(self.offset..).unwrap_or_default()
    }

    /// `count` elements starting at the cursor, or [`None`] when fewer remain.
    #[must_use]
    #[inline]
    pub fn peek_slice(&self, count: usize) -> Option<&'a [T]> {
        let end = self.offset.checked_add(count)?;
        self.items.get(self.offset..end)
    }
}

/// An exclusive borrow of a slice plus an integer write cursor.
///
/// This is the safe replacement for a C write pointer that is advanced through a buffer:
/// `pending_buf` as written by `put_byte` (`deflate.h` L293), or the `read_buf` destination
/// inside the window (`deflate.c` L311). As with [`WeakSlice`], it is a borrow plus indices
/// rather than a pointer plus a length.
///
/// [`PendingBuf::split_at_symbol_cursor`] hands one of these out over the front of the
/// `pending_buf` allocation while the still-unread symbols are handed out as a
/// [`WeakSlice`], which is how the reference implementation's overlaid `pending_buf` and
/// `sym_buf` pointers are modelled without ever aliasing.
#[derive(Debug)]
pub struct WeakSliceMut<'a, T> {
    items: &'a mut [T],
    offset: usize,
}

impl<'a, T> WeakSliceMut<'a, T> {
    /// Wraps `items` with the cursor at the start.
    #[must_use]
    #[inline]
    pub fn new(items: &'a mut [T]) -> Self {
        Self { items, offset: 0 }
    }

    /// Wraps `items` with the cursor at `offset`, or [`None`] when `offset` is past the end.
    ///
    /// The cursor may legally sit *at* the end, which represents a full buffer.
    #[must_use]
    #[inline]
    pub fn at(items: &'a mut [T], offset: usize) -> Option<Self> {
        if offset > items.len() {
            return None;
        }
        Some(Self { items, offset })
    }

    /// Total number of elements behind the cursor's slice, written or not.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the borrowed slice is empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Current cursor position, counted from the start of the borrowed slice.
    #[must_use]
    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Number of elements still ahead of the cursor.
    #[must_use]
    #[inline]
    pub fn remaining(&self) -> usize {
        self.items.len().saturating_sub(self.offset)
    }

    /// Moves the cursor to `offset`, returning `false` and leaving it untouched when that
    /// would be past the end.
    #[inline]
    pub fn set_offset(&mut self, offset: usize) -> bool {
        if offset > self.items.len() {
            return false;
        }
        self.offset = offset;
        true
    }

    /// Advances the cursor by `count`, returning `false` and leaving it untouched when that
    /// would run past the end.
    #[inline]
    pub fn advance(&mut self, count: usize) -> bool {
        match self.offset.checked_add(count) {
            Some(offset) => self.set_offset(offset),
            None => false,
        }
    }

    /// Writes `value` at the cursor and advances it, returning `false` when the buffer is
    /// full.
    ///
    /// This is the bounds-checked form of `put_byte` (`deflate.h` L293), whose C contract is
    /// only an "IN assertion: there is enough room in `pending_buf`".
    #[inline]
    pub fn put(&mut self, value: T) -> bool {
        let Some(slot) = self.items.get_mut(self.offset) else {
            return false;
        };
        *slot = value;
        self.offset = self.offset.saturating_add(1);
        true
    }

    /// The whole borrowed slice, ignoring the cursor.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        self.items
    }

    /// The whole borrowed slice for writing, ignoring the cursor.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.items
    }

    /// The elements from the cursor to the end, for writing.
    #[inline]
    pub fn remainder_mut(&mut self) -> &mut [T] {
        // The `None` arm is unreachable: every mutator rejects an offset past the end.
        // `unwrap_or_default` yields an empty slice there, which keeps this accessor total
        // rather than introducing a panic path.
        self.items.get_mut(self.offset..).unwrap_or_default()
    }

    /// Splits the borrowed slice at `mid`, or [`None`] when `mid` is past the end.
    ///
    /// The bound is checked first, which is what makes the underlying `split_at_mut`
    /// infallible; the two halves are disjoint, so both may be used at once.
    #[inline]
    pub fn split_at(&mut self, mid: usize) -> Option<(&mut [T], &mut [T])> {
        if mid > self.items.len() {
            return None;
        }
        Some(self.items.split_at_mut(mid))
    }
}

/// The LZ77 sliding window and its cursors.
///
/// Mirrors the window half of `deflate_state`: `window` (`deflate.h` L123), `window_size`
/// (L133), `w_size`/`w_bits`/`w_mask` (L119-L121) and the cursors `strstart`, `match_start`,
/// `lookahead` (L166-L168), `block_start` (L158) and `insert` (L259), plus the `high_water`
/// mark (L278-L283).
///
/// The buffer is `2 * w_size` bytes, as allocated by
/// `ZALLOC(strm, s->w_size, 2*sizeof(Byte))` (`deflate.c` L458) and recorded by
/// `window_size = (ulg)2L*s->w_size` (`deflate.c` L683). Per `deflate.h` L124-L131, input is
/// read into the second half and moved down to the first half later, "to keep a dictionary
/// of at least wSize bytes".
///
/// # Cursors are public, the buffer is not
///
/// `strstart`, `lookahead`, `block_start`, `match_start` and `insert` are plain integers
/// that the reference implementation adjusts freely — `s->strstart -= wsize` in
/// `fill_window` (`deflate.c` L289), `s->strstart = s->w_size` in `deflate_stored`
/// (L1767), `s->strstart++` in `deflate_slow`, and so on — and no invariant of this type
/// depends on their values, so they are public fields rather than a wall of setters. Every
/// accessor validates them at the point of use and returns [`None`] rather than trusting
/// them, so a stale cursor can produce a failed lookup but never an out-of-bounds access.
///
/// `high_water` is private because [`Window::initialize_win_init_tail`] maintains it as a
/// real invariant: it records how much of the buffer has been written, and the zeroing logic
/// is only correct if nothing else moves it backwards.
#[derive(Debug)]
pub struct Window<S> {
    buf: S,
    w_bits: u32,
    /// Start of the string being inserted, from `uInt strstart` (`deflate.h` L166).
    pub strstart: usize,
    /// Number of valid bytes ahead of `strstart`, from `uInt lookahead`
    /// (`deflate.h` L168).
    pub lookahead: usize,
    /// Window position where the current output block began, from `long block_start`
    /// (`deflate.h` L158). Signed because it "gets negative when the window is moved
    /// backwards" (`deflate.h` L159-L161).
    pub block_start: isize,
    /// Start of the matching string, from `uInt match_start` (`deflate.h` L167).
    pub match_start: usize,
    /// Bytes at the end of the window still to be inserted into the hash chains, from
    /// `uInt insert` (`deflate.h` L259).
    pub insert: usize,
    high_water: usize,
}

impl<S: AsRef<[u8]> + AsMut<[u8]>> Window<S> {
    /// Wraps a caller-allocated window buffer.
    ///
    /// Returns [`None`] unless `w_bits` is in `MIN_WBITS..=MAX_WBITS` and `buf` is exactly
    /// `2 << w_bits` bytes long, which are the two facts every other method relies on.
    /// Nothing is allocated and nothing is initialized: the buffer's contents are whatever
    /// the caller's allocator produced (`zutil.c` L299-L303 uses `malloc`, not `calloc`), so
    /// the caller must run [`Window::initialize_win_init_tail`] before the match routines
    /// read past the data, exactly as `fill_window` does (`deflate.c` L347-L372).
    ///
    /// All cursors start at zero, matching `lm_init` (`deflate.c` L694-L697) and
    /// `s->high_water = 0` (`deflate.c` L462).
    #[must_use]
    pub fn new(buf: S, w_bits: u32) -> Option<Self> {
        if !(MIN_WBITS..=MAX_WBITS).contains(&w_bits) {
            return None;
        }
        // `w_bits <= 15`, so `2 << w_bits <= 65536` and neither shift can overflow.
        if buf.as_ref().len() != 2usize << w_bits {
            return None;
        }
        Some(Self {
            buf,
            w_bits,
            strstart: 0,
            lookahead: 0,
            block_start: 0,
            match_start: 0,
            insert: 0,
            high_water: 0,
        })
    }

    /// `log2` of the window size, from `uInt w_bits` (`deflate.h` L120).
    #[must_use]
    #[inline]
    pub const fn w_bits(&self) -> u32 {
        self.w_bits
    }

    /// The LZ77 window size, from `s->w_size = 1 << s->w_bits` (`deflate.c` L450).
    #[must_use]
    #[inline]
    pub const fn w_size(&self) -> usize {
        1usize << self.w_bits
    }

    /// Mask that reduces a window offset modulo the window size, from
    /// `s->w_mask = s->w_size - 1` (`deflate.c` L451).
    #[must_use]
    #[inline]
    pub const fn w_mask(&self) -> usize {
        self.w_size() - 1
    }

    /// Size of the whole buffer, `2 * w_size`, from `window_size` (`deflate.h` L133) as set
    /// at `deflate.c` L683.
    #[must_use]
    #[inline]
    pub const fn window_size(&self) -> usize {
        2usize << self.w_bits
    }

    /// Largest match distance, from `#define MAX_DIST(s) ((s)->w_size-MIN_LOOKAHEAD)`
    /// (`deflate.h` L301).
    ///
    /// The subtraction is exact rather than saturating for every window this type accepts,
    /// because [`MIN_WBITS`] is 9 and therefore `w_size >= 512 > MIN_LOOKAHEAD`; the
    /// saturating form is used only so that no arithmetic here can panic even in principle.
    #[must_use]
    #[inline]
    pub const fn max_dist(&self) -> usize {
        self.w_size().saturating_sub(MIN_LOOKAHEAD)
    }

    /// High water mark: the offset up to which the buffer has been written or zeroed, from
    /// `ulg high_water` (`deflate.h` L278-L283).
    #[must_use]
    #[inline]
    pub const fn high_water(&self) -> usize {
        self.high_water
    }

    /// `len` bytes of the window starting at `start`, or [`None`] when that range does not
    /// fit.
    ///
    /// Replaces expressions such as `s->window + s->strstart + s->lookahead - len` in
    /// `deflateGetDictionary` (`deflate.c` L637) and `s->window + s->block_start` in
    /// `deflate_stored` (`deflate.c` L1734).
    #[must_use]
    #[inline]
    pub fn region(&self, start: usize, len: usize) -> Option<&[u8]> {
        let end = start.checked_add(len)?;
        self.buf.as_ref().get(start..end)
    }

    /// `len` writable bytes of the window starting at `start`, or [`None`] when that range
    /// does not fit.
    #[inline]
    pub fn region_mut(&mut self, start: usize, len: usize) -> Option<&mut [u8]> {
        let end = start.checked_add(len)?;
        self.buf.as_mut().get_mut(start..end)
    }

    /// A single window byte, or [`None`] when `index` is out of range.
    ///
    /// Replaces the `s->window[str]`, `s->window[str + 1]` and
    /// `s->window[str + MIN_MATCH-1]` reads that prime the rolling hash in `fill_window`
    /// (`deflate.c` L317-L323) and in `INSERT_STRING` (`deflate.c` L161).
    #[must_use]
    #[inline]
    pub fn byte(&self, index: usize) -> Option<u8> {
        self.buf.as_ref().get(index).copied()
    }

    /// Two shared views of the window, `len` bytes each, for comparing a candidate match
    /// against the current string.
    ///
    /// This is the whole of what `longest_match` needs from the window: `scan = s->window +
    /// s->strstart` (`deflate.c` L1391) and `match = s->window + cur_match`
    /// (`deflate.c` L1436) are two *read* pointers into one buffer, which in Rust is simply
    /// two shared borrows — they may overlap, and for a hash-chain candidate at
    /// `cur_match < strstart` they always do.
    ///
    /// Pass `len = MAX_MATCH + 1`: the comparison reads `scan[best_len]` where
    /// `best_len < MAX_MATCH` (`deflate.c` L1413-L1414) and the unrolled loop can reach
    /// `strend = s->window + s->strstart + MAX_MATCH` (`deflate.c` L1412). That is always in
    /// range once `fill_window`'s exit assertion holds, since
    /// `strstart <= window_size - MIN_LOOKAHEAD` leaves 262 bytes of slack
    /// (`deflate.c` L374-L375).
    ///
    /// Returned as plain slices so the caller can compare them with ordinary indexing
    /// arithmetic on a bound established once, rather than re-checking every byte. Only the
    /// byte-at-a-time branch of the reference comparison (`deflate.c` L1482-L1485) is
    /// supported by design; the `UNALIGNED_OK` word-at-a-time variant (L1446-L1478) is a
    /// different code path and is not part of this implementation.
    #[must_use]
    #[inline]
    pub fn scan_pair(&self, scan: usize, candidate: usize, len: usize) -> Option<(&[u8], &[u8])> {
        let buf = self.buf.as_ref();
        let scan_end = scan.checked_add(len)?;
        let candidate_end = candidate.checked_add(len)?;
        Some((buf.get(scan..scan_end)?, buf.get(candidate..candidate_end)?))
    }

    /// Free space at the end of the window, from
    /// `more = window_size - lookahead - strstart` (`deflate.c` L260).
    ///
    /// Saturates at zero instead of wrapping. The C code's `more == (unsigned)(-1)` special
    /// case (`deflate.c` L274-L279) exists only for 16-bit `int` targets, which this implementation
    /// does not support, so the underflow it works around cannot arise here.
    #[must_use]
    #[inline]
    pub fn free_space(&self) -> usize {
        self.window_size()
            .saturating_sub(self.lookahead)
            .saturating_sub(self.strstart)
    }

    /// Exactly the free space at the end of the window, for writing.
    ///
    /// Replaces the `s->window + s->strstart + s->lookahead` destination that `fill_window`
    /// hands to `read_buf` together with `more` (`deflate.c` L311), and the
    /// `s->window + s->strstart` destination in `deflate_stored` (`deflate.c` L1816).
    ///
    /// The returned slice is exactly [`Window::free_space`] bytes long, so a caller that
    /// copies into a prefix of it cannot overrun. It is produced by one pre-validated slice
    /// projection — the panic-free equivalent of a validated `split_at_mut` — rather than by
    /// two borrows that would have to be proved disjoint.
    #[inline]
    pub fn free_space_mut(&mut self) -> Option<&mut [u8]> {
        let start = self.strstart.checked_add(self.lookahead)?;
        let len = self.window_size().checked_sub(start)?;
        self.region_mut(start, len)
    }

    /// Copies `src` into the window at `start`, returning `false` when it does not fit.
    ///
    /// Replaces `zmemcpy(s->window + s->strstart, s->strm->next_in - used, used)`
    /// (`deflate.c` L1780) and the history-supplanting copy at `deflate.c` L1766.
    #[inline]
    pub fn write_at(&mut self, start: usize, src: &[u8]) -> bool {
        let Some(dst) = self.region_mut(start, src.len()) else {
            return false;
        };
        // Lengths are equal by construction: `region_mut` returned exactly `src.len()`
        // bytes, so this cannot panic.
        dst.copy_from_slice(src);
        true
    }

    /// Copies `dst.len()` bytes out of the window from `start`, returning `false` when that
    /// range does not fit.
    ///
    /// Replaces `zmemcpy(s->strm->next_out, s->window + s->block_start, left)`
    /// (`deflate.c` L1734) and `zmemcpy(dictionary, s->window + ..., len)`
    /// (`deflate.c` L637).
    #[inline]
    pub fn copy_out(&self, start: usize, dst: &mut [u8]) -> bool {
        let Some(src) = self.region(start, dst.len()) else {
            return false;
        };
        // Lengths are equal by construction, as in `write_at`.
        dst.copy_from_slice(src);
        true
    }

    /// Moves `count` bytes from the upper half of the window down to the start, returning
    /// `false` when `w_size + count` exceeds the buffer.
    ///
    /// This is the safe replacement for the three `zmemcpy` calls that slide the window:
    /// `zmemcpy(s->window, s->window + wsize, (unsigned)wsize - more)` in `fill_window`
    /// (`deflate.c` L287) and `zmemcpy(s->window, s->window + s->w_size, s->strstart)` in
    /// `deflate_stored` (`deflate.c` L1774 and L1806).
    ///
    /// `slice::copy_within` has `memmove` semantics, so the result is correct whether or not
    /// the regions overlap. For every `count` the algorithm produces they do not — `count`
    /// never exceeds `w_size`, which is exactly the gap between source and destination — so
    /// this is byte-for-byte what the C `memcpy` produces, and it stays correct if a future
    /// caller ever passes a larger `count`.
    ///
    /// Cursors are left alone: `fill_window` and the two `deflate_stored` sites adjust
    /// different subsets of them (compare `deflate.c` L288-L292 with L1773-L1778 and
    /// L1804-L1811), so adjusting them here would misfit two of the three callers.
    #[inline]
    pub fn slide_down(&mut self, count: usize) -> bool {
        let w_size = self.w_size();
        let Some(end) = w_size.checked_add(count) else {
            return false;
        };
        let buf = self.buf.as_mut();
        if end > buf.len() {
            return false;
        }
        // Both ends are validated above and the destination is offset 0, so neither of
        // `copy_within`'s panic conditions can be met.
        buf.copy_within(w_size..end, 0);
        true
    }

    /// Clamps `insert` to `strstart`.
    ///
    /// Reproduces `if (s->insert > s->strstart) s->insert = s->strstart;`, which appears
    /// after every window slide (`deflate.c` L291-L292, L1777-L1778 and L1810-L1811).
    #[inline]
    pub fn clamp_insert_to_strstart(&mut self) {
        self.insert = self.insert.min(self.strstart);
    }

    /// `len` bytes of the window starting at `block_start`, or [`None`] when `block_start`
    /// is negative or the range does not fit.
    ///
    /// Replaces `(charf *)s->window + s->block_start` as passed to `_tr_stored_block`
    /// (`deflate.c` L1839) and to `_tr_flush_block`.
    #[must_use]
    #[inline]
    pub fn block_region(&self, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(self.block_start).ok()?;
        self.region(start, len)
    }

    /// Number of window bytes accumulated since the current block began, or [`None`] when
    /// `block_start` is ahead of `strstart`.
    ///
    /// Reproduces `left = (unsigned)(s->strstart - s->block_start)` (`deflate.c` L1693 and
    /// L1832) without the unsigned wraparound that expression relies on.
    #[must_use]
    #[inline]
    pub fn bytes_since_block_start(&self) -> Option<usize> {
        let strstart = isize::try_from(self.strstart).ok()?;
        usize::try_from(strstart.checked_sub(self.block_start)?).ok()
    }

    /// Sets the high water mark, returning `false` when `value` exceeds the buffer.
    ///
    /// Provided for state duplication (`deflateCopy` copies `high_water` along with the
    /// window, `deflate.c` L1353) and for reset paths that clear it (`deflate.c` L462).
    #[inline]
    pub fn set_high_water(&mut self, value: usize) -> bool {
        if value > self.window_size() {
            return false;
        }
        self.high_water = value;
        true
    }

    /// Raises the high water mark to `strstart` if it is behind.
    ///
    /// Reproduces `if (s->high_water < s->strstart) s->high_water = s->strstart;`, which
    /// `deflate_stored` performs after copying input straight into the window
    /// (`deflate.c` L1786-L1787 and L1820-L1821).
    #[inline]
    pub fn raise_high_water_to_strstart(&mut self) {
        self.high_water = self.high_water.max(self.strstart);
    }

    /// Zeroes the bytes past the end of the data that the match routines may read.
    ///
    /// A faithful a mirror of `fill_window`'s tail (`deflate.c` L347-L372), including both
    /// branches and the `window_size` clamp. Its purpose, per the comment at
    /// `deflate.c` L340-L346: the match routines "allow scanning to `strstart + MAX_MATCH`,
    /// ignoring lookahead", so [`WIN_INIT`] bytes past the data are zeroed to avoid reading
    /// never-written memory, and `high_water` records how far that has been done.
    ///
    /// This is not optional bookkeeping. The default allocator hands back `malloc` memory
    /// (`zutil.c` L299-L303), so the window arrives with arbitrary contents; the reference
    /// implementation depends on these bytes being zero for the *deterministic* part of its
    /// behaviour, and `test/infcover.c` fills every allocation with `0xa5` precisely to
    /// catch an implementation that assumes zeroed memory instead of doing this.
    ///
    /// The zeroed bytes are past `strstart + lookahead`, so they never affect emitted
    /// output: a match is always truncated to `lookahead` (`deflate.c` L1528-L1529).
    pub fn initialize_win_init_tail(&mut self) {
        let window_size = self.window_size();
        if self.high_water >= window_size {
            return;
        }
        let Some(curr) = self.strstart.checked_add(self.lookahead) else {
            return;
        };
        if curr > window_size {
            // Violates `fill_window`'s own invariant; refuse rather than compute a bogus
            // length.
            return;
        }

        if self.high_water < curr {
            // Previous high water mark below the current data: zero WIN_INIT bytes, or up
            // to the end of the window, whichever is less (`deflate.c` L351-L360).
            let init = (window_size - curr).min(WIN_INIT);
            let Some(tail) = self.region_mut(curr, init) else {
                return;
            };
            tail.fill(0);
            self.high_water = curr + init;
        } else if self.high_water < curr + WIN_INIT {
            // High water mark at or above the current data but below `curr + WIN_INIT`:
            // zero out to `curr + WIN_INIT`, or to the end of the window, whichever is less
            // (`deflate.c` L361-L371).
            let start = self.high_water;
            let init = (curr + WIN_INIT - start).min(window_size - start);
            let Some(tail) = self.region_mut(start, init) else {
                return;
            };
            tail.fill(0);
            self.high_water = start + init;
        }
    }

    /// Returns the wrapped buffer, so the owner can hand it back to the allocator that
    /// produced it.
    #[must_use]
    #[inline]
    pub fn into_inner(self) -> S {
        self.buf
    }
}

/// The two hash-chain arrays that drive match finding.
///
/// Mirrors `head` and `prev` from `deflate_state` (`deflate.h` L138-L144) together with the
/// sizes that index them (`hash_bits`/`hash_size`/`hash_mask`, L147-L149) and the `slid` flag
/// (L285-L286). Both arrays hold [`Pos`] values, i.e. 16-bit window indices, and both are
/// allocated by the caller: `ZALLOC(strm, s->w_size, sizeof(Pos))` for `prev` and
/// `ZALLOC(strm, s->hash_size, sizeof(Pos))` for `head` (`deflate.c` L459-L460).
///
/// `head[h]` is the most recent window position whose three-byte prefix hashed to `h`;
/// `prev[p & w_mask]` is the next older position on the same chain, with [`NIL`] as the tail
/// (`deflate.c` L85). Per `deflate.h` L139-L142 the `prev` index "is thus a window index
/// modulo 32K", which is why [`HashChains::prev_at`] masks rather than fails.
///
/// The buffers arrive uninitialized from `malloc` (`zutil.c` L299-L303).
/// [`HashChains::clear`] must therefore be called before use, exactly as `deflateReset`
/// does through `CLEAR_HASH` (`deflate.c` L170-L175, reached from `lm_init` at L685). Note
/// that only `head` is cleared: "prev[] will be initialized on the fly"
/// (`deflate.c` L168). Reading an entry of `prev` that is not on any chain is therefore
/// reading whatever the allocator left behind — the reference implementation says as much,
/// "If n is not on any hash chain, `prev[n]` is garbage but its value will never be used"
/// (`deflate.c` L204-L206). Here that read is always a defined `u16` and, as in C, its value
/// cannot reach the output: a candidate below `longest_match`'s `limit` ends the chain walk
/// (`deflate.c` L1525).
#[derive(Debug)]
pub struct HashChains<S> {
    head: S,
    prev: S,
    w_bits: u32,
    hash_bits: u32,
    slid: bool,
}

impl<S: AsRef<[u16]> + AsMut<[u16]>> HashChains<S> {
    /// Wraps caller-allocated `head` and `prev` arrays.
    ///
    /// Returns [`None`] unless `w_bits` is in `MIN_WBITS..=MAX_WBITS`, `hash_bits` is in
    /// `MIN_HASH_BITS..=MAX_HASH_BITS`, `head` holds exactly `1 << hash_bits` entries and
    /// `prev` holds exactly `1 << w_bits` entries. Those are the invariants that let every
    /// masked accessor below be infallible.
    ///
    /// The arrays are not cleared; call [`HashChains::clear`] first, as `lm_init` does
    /// (`deflate.c` L685).
    #[must_use]
    pub fn new(head: S, prev: S, w_bits: u32, hash_bits: u32) -> Option<Self> {
        if !(MIN_WBITS..=MAX_WBITS).contains(&w_bits)
            || !(MIN_HASH_BITS..=MAX_HASH_BITS).contains(&hash_bits)
        {
            return None;
        }
        if head.as_ref().len() != 1usize << hash_bits || prev.as_ref().len() != 1usize << w_bits {
            return None;
        }
        Some(Self {
            head,
            prev,
            w_bits,
            hash_bits,
            slid: false,
        })
    }

    /// `log2` of the window size, from `uInt w_bits` (`deflate.h` L120).
    #[must_use]
    #[inline]
    pub const fn w_bits(&self) -> u32 {
        self.w_bits
    }

    /// Number of `prev` entries, equal to the window size (`deflate.c` L459).
    #[must_use]
    #[inline]
    pub const fn w_size(&self) -> usize {
        1usize << self.w_bits
    }

    /// Mask applied to a window offset before indexing `prev`, from `s->w_mask`
    /// (`deflate.c` L451).
    #[must_use]
    #[inline]
    pub const fn w_mask(&self) -> usize {
        self.w_size() - 1
    }

    /// `log2` of the hash-table size, from `s->hash_bits = memLevel + 7`
    /// (`deflate.c` L453).
    #[must_use]
    #[inline]
    pub const fn hash_bits(&self) -> u32 {
        self.hash_bits
    }

    /// Number of `head` entries, from `s->hash_size = 1 << s->hash_bits`
    /// (`deflate.c` L454).
    #[must_use]
    #[inline]
    pub const fn hash_size(&self) -> usize {
        1usize << self.hash_bits
    }

    /// Mask applied to a hash value before indexing `head`, from
    /// `s->hash_mask = s->hash_size - 1` (`deflate.c` L455).
    ///
    /// This is the same mask `UPDATE_HASH` already applies (`deflate.c` L141), so a hash
    /// produced by that macro is always in range.
    #[must_use]
    #[inline]
    pub const fn hash_mask(&self) -> usize {
        self.hash_size() - 1
    }

    /// Whether the tables have been slid since they were last cleared, from `int slid`
    /// (`deflate.h` L285-L286).
    ///
    /// `deflateCopy` reads it to decide how much of `prev` is worth copying
    /// (`deflate.c` L1354-L1356).
    #[must_use]
    #[inline]
    pub const fn slid(&self) -> bool {
        self.slid
    }

    /// Overrides the slid flag, for state duplication (`deflate.c` L1339 copies the whole
    /// `deflate_state`, `slid` included).
    #[inline]
    pub fn set_slid(&mut self, slid: bool) {
        self.slid = slid;
    }

    /// Head of the hash chain for `hash`, i.e. `s->head[hash]`.
    ///
    /// `hash` is reduced by [`HashChains::hash_mask`] first, mirroring `UPDATE_HASH`
    /// (`deflate.c` L141), so this cannot fail. A [`NIL`] result means the chain is empty
    /// (`deflate.c` L144).
    #[must_use]
    #[inline]
    pub fn head_at(&self, hash: usize) -> Pos {
        let index = hash & self.hash_mask();
        self.head
            .as_ref()
            .get(index)
            .copied()
            .map_or(Pos::NIL, Pos::new)
    }

    /// Sets the head of the hash chain for `hash`, i.e. `s->head[hash] = pos`.
    ///
    /// `hash` is reduced by [`HashChains::hash_mask`] first, so this cannot fail.
    #[inline]
    pub fn set_head_at(&mut self, hash: usize, pos: Pos) {
        let index = hash & self.hash_mask();
        if let Some(slot) = self.head.as_mut().get_mut(index) {
            *slot = pos.get();
        }
    }

    /// Next older position on the chain through `str_index`, i.e. `s->prev[str & w_mask]`.
    ///
    /// The mask is part of the data structure, not a bounds check: an index into `prev` "is
    /// thus a window index modulo 32K" (`deflate.h` L139-L142), which is how a 64 KiB window
    /// is tracked with a 32 KiB array. Matches `prev[cur_match & wmask]` in `longest_match`
    /// (`deflate.c` L1525).
    #[must_use]
    #[inline]
    pub fn prev_at(&self, str_index: usize) -> Pos {
        let index = str_index & self.w_mask();
        self.prev
            .as_ref()
            .get(index)
            .copied()
            .map_or(Pos::NIL, Pos::new)
    }

    /// Links `str_index` to `pos`, i.e. `s->prev[str & w_mask] = pos`.
    #[inline]
    pub fn set_prev_at(&mut self, str_index: usize, pos: Pos) {
        let index = str_index & self.w_mask();
        if let Some(slot) = self.prev.as_mut().get_mut(index) {
            *slot = pos.get();
        }
    }

    /// Inserts `str_index` at the head of the chain for `hash` and returns the previous
    /// head.
    ///
    /// An exact mirror of `INSERT_STRING` minus the hash update (`deflate.c` L160-L163):
    ///
    /// ```text
    /// match_head = s->prev[(str) & s->w_mask] = s->head[s->ins_h],
    /// s->head[s->ins_h] = (Pos)(str)
    /// ```
    ///
    /// The order matters — the old head becomes this position's `prev` link *before* the
    /// head is overwritten — and the returned value is C's `match_head`, the candidate
    /// `deflate_fast` and `deflate_slow` hand to `longest_match`. The same three assignments
    /// appear in `fill_window`'s hash priming loop (`deflate.c` L325-L327).
    ///
    /// Returns [`None`] only when `str_index` does not fit a [`Pos`], which the C cast at
    /// L163 would silently truncate; window offsets stay below `2 * w_size <= 65536`, so a
    /// well-formed stream never reaches it.
    #[inline]
    pub fn insert_string(&mut self, hash: usize, str_index: usize) -> Option<Pos> {
        let pos = Pos::from_index(str_index)?;
        let previous = self.head_at(hash);
        self.set_prev_at(str_index, previous);
        self.set_head_at(hash, pos);
        Some(previous)
    }

    /// Slides both tables down by the window size.
    ///
    /// An exact mirror of `slide_hash` (`deflate.c` L187-L210): every entry of `head` and then
    /// every entry of `prev` becomes `m >= wsize ? m - wsize : NIL`, and `slid` is set
    /// (L209). The C loops walk downwards from the end of each array; the direction cannot
    /// matter because each entry is rewritten from its own old value alone, so iterating
    /// forwards is byte-for-byte equivalent while letting the bound be established once for
    /// the whole array instead of once per element.
    ///
    /// Sliding happens even at level 0, "to keep the hash table consistent if we switch back
    /// to level > 0 later" (`deflate.c` L178-L181).
    #[inline]
    pub fn slide(&mut self) {
        // `w_size <= 32768` because `w_bits <= MAX_WBITS`, so this narrowing always
        // succeeds; the `else` arm is unreachable and simply declines to corrupt the tables.
        let Ok(w_size) = u16::try_from(self.w_size()) else {
            return;
        };
        for entry in self.head.as_mut() {
            *entry = Pos::new(*entry).slid_down(w_size).get();
        }
        for entry in self.prev.as_mut() {
            *entry = Pos::new(*entry).slid_down(w_size).get();
        }
        self.slid = true;
    }

    /// Clears the hash heads and the slid flag.
    ///
    /// An exact mirror of `CLEAR_HASH` (`deflate.c` L170-L175): every `head` entry becomes
    /// [`NIL`] and `slid` becomes false. `prev` is deliberately left alone — "prev[] will be
    /// initialized on the fly" (`deflate.c` L168) — so its contents stay whatever the
    /// allocator produced, which is what the C code relies on too.
    #[inline]
    pub fn clear(&mut self) {
        self.head.as_mut().fill(NIL);
        self.slid = false;
    }

    /// All `head` entries, for bulk work such as the table copy in `deflateCopy`
    /// (`deflate.c` L1357).
    #[must_use]
    #[inline]
    pub fn head_entries(&self) -> &[u16] {
        self.head.as_ref()
    }

    /// All `head` entries, writable.
    #[inline]
    pub fn head_entries_mut(&mut self) -> &mut [u16] {
        self.head.as_mut()
    }

    /// All `prev` entries, for bulk work such as the partial copy in `deflateCopy`
    /// (`deflate.c` L1354-L1356).
    #[must_use]
    #[inline]
    pub fn prev_entries(&self) -> &[u16] {
        self.prev.as_ref()
    }

    /// All `prev` entries, writable.
    #[inline]
    pub fn prev_entries_mut(&mut self) -> &mut [u16] {
        self.prev.as_mut()
    }

    /// Returns the wrapped `head` and `prev` buffers, in that order, so the owner can hand
    /// them back to the allocator that produced them.
    #[must_use]
    #[inline]
    pub fn into_inner(self) -> (S, S) {
        (self.head, self.prev)
    }
}

/// The overlaid pending-output and symbol buffer.
///
/// One allocation, three C pointers into it — the only genuinely tricky aliasing in
/// `deflate_state`:
///
/// | C member | `deflate.h` | Modelled here as |
/// |---|---|---|
/// | `Bytef *pending_buf` | L107 | the wrapped buffer itself |
/// | `Bytef *pending_out` | L109 | [`PendingBuf::pending_out_offset`], a read offset |
/// | `uchf *sym_buf` | L230 | a fixed offset of `lit_bufsize`, per `deflate.c` L520 |
///
/// with `pending` (`deflate.h` L110) counting the bytes of compressed output waiting to be
/// flushed and `sym_next`/`sym_end` (L253-L254) tracking the symbol buffer.
///
/// # Layout
///
/// The allocation is `LIT_BUFS * lit_bufsize` bytes, from
/// `ZALLOC(strm, s->lit_bufsize, LIT_BUFS)` (`deflate.c` L505), and
/// `pending_buf_size = lit_bufsize * 4` (L506). With the shipped `LIT_BUFS` of 4 those are
/// the same number, so pending output may legally advance across the whole buffer,
/// *including* the region the symbols occupy. The symbol buffer starts one quarter of the way
/// in, `sym_buf = pending_buf + lit_bufsize` (L520), and fills to
/// `sym_end = (lit_bufsize - 1) * 3` (L521) — three bytes per symbol, deliberately short of
/// `lit_bufsize * 3` "because of wraparound at 64K on 16 bit machines" (L523-L526).
///
/// This is the `LIT_BUFS 4` layout and the only one implemented. `LIT_MEM` — `LIT_BUFS 5`
/// with separate `d_buf` and `l_buf` arrays (`deflate.h` L224-L227) — is commented out at
/// `deflate.h` L28, and it changes both the buffer size and the symbol encoding, so
/// supporting it would change the emitted bytes.
///
/// # Why the overlay is safe
///
/// Writing compressed output into the same bytes that still hold unread symbols sounds
/// impossible, and the reference implementation proves that it is not (`deflate.c`
/// L466-L503): the longest fixed-code length/distance pair is 31 bits, each symbol consumed
/// frees 24 bits of symbol buffer, and `sym_buf` starts `8 * lit_bufsize` bits in, which
/// leaves at least 139 bits of slack between the write cursor and the next unread symbol.
/// `compress_block` asserts exactly that bound: `Assert(s->pending < s->lit_bufsize + sx)`
/// (`trees.c` L945).
///
/// Two APIs follow from this. [`PendingBuf::symbol_at`] copies a symbol out by value, which
/// removes the aliasing question entirely and is the recommended way to write
/// `compress_block`; a symbol is three bytes, so copying costs nothing.
/// [`PendingBuf::split_at_symbol_cursor`] is for a caller that wants to hold the unread
/// symbols and the write region at the same time, and it splits at `lit_bufsize + consumed`
/// — the same boundary `trees.c` L945 asserts — so the two halves are provably disjoint.
#[derive(Debug)]
pub struct PendingBuf<S> {
    buf: S,
    lit_bufsize: usize,
    pending: usize,
    pending_out: usize,
    sym_next: usize,
    sym_end: usize,
}

impl<S: AsRef<[u8]> + AsMut<[u8]>> PendingBuf<S> {
    /// Wraps a caller-allocated pending buffer.
    ///
    /// Returns [`None`] unless `lit_bufsize` is a power of two in
    /// `MIN_LIT_BUFSIZE..=MAX_LIT_BUFSIZE` — the range `lit_bufsize = 1 << (memLevel + 6)`
    /// can produce (`deflate.c` L464, and the analysis at L486-L487) — and `buf` is exactly
    /// `LIT_BUFS * lit_bufsize` bytes long, matching `deflate.c` L505.
    ///
    /// `sym_end` is derived as `(lit_bufsize - 1) * 3` (`deflate.c` L521) and all three
    /// cursors start at zero, matching `deflateResetKeep` (`deflate.c` L656-L657) and
    /// `init_block`. Nothing is allocated and nothing is initialized: as with the window, the
    /// bytes are whatever the caller's allocator produced (`zutil.c` L299-L303).
    #[must_use]
    pub fn new(buf: S, lit_bufsize: usize) -> Option<Self> {
        if !lit_bufsize.is_power_of_two()
            || !(MIN_LIT_BUFSIZE..=MAX_LIT_BUFSIZE).contains(&lit_bufsize)
        {
            return None;
        }
        // `lit_bufsize <= 32768`, so the product is at most 131072 and cannot overflow.
        if buf.as_ref().len() != LIT_BUFS * lit_bufsize {
            return None;
        }
        Some(Self {
            buf,
            lit_bufsize,
            pending: 0,
            pending_out: 0,
            sym_next: 0,
            sym_end: (lit_bufsize - 1) * SYMBOL_BYTES,
        })
    }

    /// Number of symbol-buffer elements the stream was configured for, from
    /// `uInt lit_bufsize` (`deflate.h` L233).
    #[must_use]
    #[inline]
    pub const fn lit_bufsize(&self) -> usize {
        self.lit_bufsize
    }

    /// Usable size of the pending-output area, from
    /// `s->pending_buf_size = (ulg)s->lit_bufsize * 4` (`deflate.c` L506).
    #[must_use]
    #[inline]
    pub const fn pending_buf_size(&self) -> usize {
        self.lit_bufsize * LIT_BUFS
    }

    /// Offset of the symbol buffer within the allocation, from
    /// `s->sym_buf = s->pending_buf + s->lit_bufsize` (`deflate.c` L520).
    ///
    /// Together with [`PendingBuf::pending_out_offset`] this expresses the pointer
    /// comparison `deflatePrime` uses to refuse a stream whose pending output has already
    /// reached the symbols: `s->sym_buf < s->pending_out + ((Buf_size + 7) >> 3)`
    /// (`deflate.c` L756-L757).
    #[must_use]
    #[inline]
    pub const fn sym_buf_offset(&self) -> usize {
        self.lit_bufsize
    }

    /// Byte offset within the symbol buffer at which it is considered full, from
    /// `uInt sym_end` (`deflate.h` L254) as computed at `deflate.c` L521.
    #[must_use]
    #[inline]
    pub const fn sym_end(&self) -> usize {
        self.sym_end
    }

    /// Running byte offset within the symbol buffer, from `uInt sym_next`
    /// (`deflate.h` L253). This is `sx`'s upper bound in `compress_block`
    /// (`trees.c` L948).
    #[must_use]
    #[inline]
    pub const fn sym_next(&self) -> usize {
        self.sym_next
    }

    /// Number of symbols currently buffered.
    #[must_use]
    #[inline]
    pub const fn symbol_count(&self) -> usize {
        self.sym_next / SYMBOL_BYTES
    }

    /// Bytes of compressed output waiting to be flushed, from `ulg pending`
    /// (`deflate.h` L110).
    #[must_use]
    #[inline]
    pub const fn pending(&self) -> usize {
        self.pending
    }

    /// Offset of the next byte to hand to the caller, from `Bytef *pending_out`
    /// (`deflate.h` L109) expressed relative to the start of the allocation.
    #[must_use]
    #[inline]
    pub const fn pending_out_offset(&self) -> usize {
        self.pending_out
    }

    /// Bytes still available for pending output.
    #[must_use]
    #[inline]
    pub fn room(&self) -> usize {
        self.pending_buf_size().saturating_sub(self.pending)
    }

    /// Appends one byte of pending output, returning `false` when the buffer is full.
    ///
    /// The bounds-checked form of `#define put_byte(s, c)` (`deflate.h` L293), whose C
    /// contract is only an "IN assertion: there is enough room in `pending_buf`" (L291).
    ///
    /// The slice bound *is* the room check: [`PendingBuf::new`] accepts only a buffer of
    /// exactly `LIT_BUFS * lit_bufsize` bytes, which is what
    /// [`PendingBuf::pending_buf_size`] returns, so `index >= pending_buf_size()` and
    /// `get_mut(index) == None` are the same condition. Testing it once rather than twice
    /// matters because every byte of compressed output leaves through here, most of them
    /// two at a time from [`PendingBuf::put_short_le`] inside the Huffman emission loop.
    #[inline]
    pub fn put_byte(&mut self, byte: u8) -> bool {
        let index = self.pending;
        let Some(slot) = self.buf.as_mut().get_mut(index) else {
            return false;
        };
        *slot = byte;
        self.pending = index + 1;
        true
    }

    /// Appends a 16-bit value least-significant byte first, returning `false` when fewer
    /// than two bytes remain.
    ///
    /// A port of `#define put_short(s, w)` (`trees.c` L144-L147), whose comment is "Output a
    /// short LSB first on the stream", and the counterpart of
    /// [`PendingBuf::put_short_msb`]. C expands to two `put_byte`s under an "IN assertion:
    /// there is enough room in `pending_buf`"; this checks that room **once** and then
    /// writes both bytes, which is the same all-or-nothing discipline
    /// [`PendingBuf::put_short_msb`] already applies and half the bounds work of two
    /// separate appends.
    ///
    /// That halving is not incidental. `send_bits` spills through here every time the bit
    /// accumulator fills, which for an incompressible block is roughly once per input byte,
    /// so this is the innermost write of the compressor.
    #[inline]
    pub fn put_short_le(&mut self, value: u16) -> bool {
        let start = self.pending;
        let Some(end) = start.checked_add(2) else {
            return false;
        };
        let Some(slot) = self.buf.as_mut().get_mut(start..end) else {
            return false;
        };
        // Exactly two bytes on both sides, so this cannot panic.
        slot.copy_from_slice(&value.to_le_bytes());
        self.pending = end;
        true
    }

    /// Appends a 16-bit value in most-significant-byte-first order, returning `false` when
    /// fewer than two bytes remain.
    ///
    /// A a mirror of `putShortMSB` (`deflate.c` L939-L942), which is two `put_byte` calls with
    /// `b >> 8` first. `u16::to_be_bytes` produces exactly that order. Room is checked once
    /// up front so the write is all-or-nothing, where the C version would have written one
    /// byte past the end.
    #[inline]
    pub fn put_short_msb(&mut self, value: u16) -> bool {
        if self.room() < 2 {
            return false;
        }
        let [high, low] = value.to_be_bytes();
        self.put_byte(high) && self.put_byte(low)
    }

    /// Appends as much of `src` as fits, returning how many bytes were written.
    ///
    /// Replaces the bounded copies the gzip header path performs,
    /// `zmemcpy(s->pending_buf + s->pending, s->gzhead->extra + s->gzindex, copy)` with
    /// `copy = s->pending_buf_size - s->pending` (`deflate.c` L1121-L1137). A short return
    /// means the buffer filled and the caller must flush and come back, which is exactly what
    /// that loop does.
    #[inline]
    pub fn append(&mut self, src: &[u8]) -> usize {
        let take = src.len().min(self.room());
        let Some(chunk) = src.get(..take) else {
            return 0;
        };
        let start = self.pending;
        let Some(end) = start.checked_add(take) else {
            return 0;
        };
        let Some(dst) = self.buf.as_mut().get_mut(start..end) else {
            return 0;
        };
        // Both slices are `take` bytes long by construction, so this cannot panic.
        dst.copy_from_slice(chunk);
        self.pending = end;
        take
    }

    /// The pending output written so far, counted from the start of the allocation.
    ///
    /// This is the view the gzip header CRC needs: `crc32_z(strm->adler, s->pending_buf,
    /// s->pending)` (`deflate.c` L1111-L1112). Note that it starts at the *base* of the
    /// allocation, not at `pending_out`.
    #[must_use]
    #[inline]
    pub fn written(&self) -> &[u8] {
        // The `None` arm is unreachable: `pending <= pending_buf_size == buf.len()` is
        // maintained by every mutator. An empty slice keeps this accessor total rather than
        // panicking.
        self.buf.as_ref().get(..self.pending).unwrap_or_default()
    }

    /// Pending output from `beg` up to `pending`, or [`None`] when `beg` is past `pending`.
    ///
    /// Reproduces `HCRC_UPDATE(beg)`, which updates the header CRC with
    /// `s->pending_buf[beg..s->pending - 1]` (`deflate.c` L971-L978). The C macro guards with
    /// `s->pending > (beg)`; here an empty range is returned as an empty slice and only a
    /// genuinely out-of-range `beg` yields [`None`].
    #[must_use]
    #[inline]
    pub fn written_from(&self, beg: usize) -> Option<&[u8]> {
        if beg > self.pending {
            return None;
        }
        self.buf.as_ref().get(beg..self.pending)
    }

    /// Overwrites `bytes` ending `back` bytes before the write cursor, returning `false`
    /// when that range is not inside the pending output.
    ///
    /// Reproduces the stored-block header patch in `deflate_stored`, which rewrites the four
    /// length bytes of a dummy block in place (`deflate.c` L1716-L1719):
    ///
    /// ```text
    /// s->pending_buf[s->pending - 4] = (Bytef)len;
    /// s->pending_buf[s->pending - 3] = (Bytef)(len >> 8);
    /// s->pending_buf[s->pending - 2] = (Bytef)~len;
    /// s->pending_buf[s->pending - 1] = (Bytef)(~len >> 8);
    /// ```
    ///
    /// which is `patch_tail(4, &[lo, hi, !lo, !hi])`. Requires `bytes.len() <= back <=
    /// pending`, so a patch can never reach past the cursor or before the buffer.
    #[inline]
    pub fn patch_tail(&mut self, back: usize, bytes: &[u8]) -> bool {
        if back > self.pending || bytes.len() > back {
            return false;
        }
        let start = self.pending - back;
        let Some(end) = start.checked_add(bytes.len()) else {
            return false;
        };
        let Some(dst) = self.buf.as_mut().get_mut(start..end) else {
            return false;
        };
        // Lengths are equal by construction, so this cannot panic.
        dst.copy_from_slice(bytes);
        true
    }

    /// The bytes ready to be copied to the caller's output buffer.
    ///
    /// This is `s->pending_out` with length `s->pending`, the source of
    /// `zmemcpy(strm->next_out, s->pending_out, len)` in `flush_pending`
    /// (`deflate.c` L959). The caller copies a prefix of it — `len` is clamped to
    /// `strm->avail_out` (L955-L956) — and then calls [`PendingBuf::consume_flushed`].
    #[must_use]
    #[inline]
    pub fn flushable(&self) -> &[u8] {
        let Some(end) = self.pending_out.checked_add(self.pending) else {
            return &[];
        };
        // The `None` arm is unreachable: `pending_out + pending <= buf.len()` is maintained by
        // every mutator. An empty slice keeps this accessor total rather than panicking.
        self.buf
            .as_ref()
            .get(self.pending_out..end)
            .unwrap_or_default()
    }

    /// The bytes ready to be flushed and the symbols not yet emitted, at the same time.
    ///
    /// Both views are *shared*, so despite living in one allocation they need no split and no
    /// disjointness argument — this is simply two borrows of `&self`. That is worth stating,
    /// because the equivalent in C is `pending_out` and `sym_buf` aliasing the same
    /// `pending_buf` (`deflate.h` L107-L109 and L230), which is the pattern this module
    /// exists to retire. Reach for [`PendingBuf::split_at_symbol_cursor`] only when the front
    /// of the buffer must be *written* while the symbols are read.
    #[must_use]
    #[inline]
    pub fn flushable_and_symbols(&self) -> (&[u8], &[u8]) {
        (self.flushable(), self.symbols())
    }

    /// Accounts for `len` flushed bytes, returning `false` when `len` exceeds `pending`.
    ///
    /// Reproduces the tail of `flush_pending` (`deflate.c` L961-L967): the read offset
    /// advances, `pending` shrinks, and when nothing is left the read offset returns to the
    /// base of the allocation — `if (s->pending == 0) s->pending_out = s->pending_buf;` —
    /// which is what keeps the write cursor from marching into the symbol buffer.
    #[inline]
    pub fn consume_flushed(&mut self, len: usize) -> bool {
        if len > self.pending {
            return false;
        }
        let Some(pending_out) = self.pending_out.checked_add(len) else {
            return false;
        };
        self.pending_out = pending_out;
        self.pending -= len;
        if self.pending == 0 {
            self.pending_out = 0;
        }
        true
    }

    /// Clears the pending output, as `deflateResetKeep` does with
    /// `s->pending = 0; s->pending_out = s->pending_buf;` (`deflate.c` L656-L657).
    #[inline]
    pub fn reset_pending(&mut self) {
        self.pending = 0;
        self.pending_out = 0;
    }

    /// Overrides the pending count, returning `false` when the result would not fit.
    ///
    /// Needed by two callers: [`PendingBuf::split_at_symbol_cursor`], whose writer reports
    /// its final cursor once the borrow has ended, and state duplication, where
    /// `deflateCopy` reproduces both the count and the read offset (`deflate.c` L1359-L1360).
    #[inline]
    pub fn set_pending(&mut self, pending: usize) -> bool {
        match self.pending_out.checked_add(pending) {
            Some(end) if end <= self.pending_buf_size() => {
                self.pending = pending;
                true
            }
            _ => false,
        }
    }

    /// Overrides the read offset, returning `false` when the result would not fit.
    ///
    /// Reproduces `ds->pending_out = ds->pending_buf + (ss->pending_out - ss->pending_buf)`
    /// in `deflateCopy` (`deflate.c` L1359).
    #[inline]
    pub fn set_pending_out_offset(&mut self, offset: usize) -> bool {
        match offset.checked_add(self.pending) {
            Some(end) if end <= self.pending_buf_size() => {
                self.pending_out = offset;
                true
            }
            _ => false,
        }
    }

    /// Appends one symbol and reports whether the symbol buffer is now full.
    ///
    /// A a mirror of the `_tr_tally_dist`/`_tr_tally_lit` macro pair (`deflate.h` L365-L375) and
    /// of `_tr_tally` itself (`trees.c` L1100-L1102): three bytes, the distance
    /// little-endian first and then the length-or-literal byte. A literal is a distance of
    /// zero, which the C code writes as two zero bytes (`deflate.h` L359-L361), so this one
    /// method serves both macros.
    ///
    /// `Some(true)` reproduces `flush = (s->sym_next == s->sym_end)`, the caller's signal to
    /// flush the block. [`None`] means the three bytes would not fit and nothing was written,
    /// which cannot happen while the caller stops at `sym_end`.
    #[inline]
    pub fn push_symbol(&mut self, dist: u16, len_or_lit: u8) -> Option<bool> {
        // `sym_end` is a whole number of symbols and so is `sym_next`, so "not yet full"
        // and "three more bytes fit below `sym_end`" are the same condition.
        if self.sym_next >= self.sym_end {
            return None;
        }
        let start = self.lit_bufsize.checked_add(self.sym_next)?;
        let end = start.checked_add(SYMBOL_BYTES)?;
        let [low, high] = dist.to_le_bytes();
        let target = self.buf.as_mut().get_mut(start..end)?;
        // Exactly `SYMBOL_BYTES` bytes on both sides, so this cannot panic.
        target.copy_from_slice(&[low, high, len_or_lit]);
        self.sym_next = self.sym_next.saturating_add(SYMBOL_BYTES);
        Some(self.sym_next == self.sym_end)
    }

    /// Reads back the symbol at byte offset `offset` within the symbol buffer as
    /// `(distance, length_or_literal)`.
    ///
    /// `offset` is `sx` from `compress_block` (`trees.c` L913-L915), so it advances in steps
    /// of [`SYMBOL_BYTES`]:
    ///
    /// ```text
    /// dist  = s->sym_buf[sx++] & 0xff;
    /// dist += (unsigned)(s->sym_buf[sx++] & 0xff) << 8;
    /// lc    = s->sym_buf[sx++];
    /// ```
    ///
    /// A distance of zero means `lc` is a literal, exactly as in C (`trees.c` L916-L917).
    ///
    /// Copying the symbol out by value leaves no borrow outstanding, so the same
    /// [`PendingBuf`] can be written through while the block is emitted, and it sidesteps the
    /// overlay question entirely for three bytes of copying.
    #[must_use]
    #[inline]
    pub fn symbol_at(&self, offset: usize) -> Option<(u16, u8)> {
        let start = self.lit_bufsize.checked_add(offset)?;
        let end = start.checked_add(SYMBOL_BYTES)?;
        match self.buf.as_ref().get(start..end)? {
            [low, high, len_or_lit] => Some((u16::from_le_bytes([*low, *high]), *len_or_lit)),
            // Unreachable: the range above is exactly `SYMBOL_BYTES` long.
            _ => None,
        }
    }

    /// Decodes up to `out.len()` consecutive symbols starting at byte offset `offset`,
    /// returning how many were written into `out`.
    ///
    /// The batched form of [`PendingBuf::symbol_at`], and the shape `compress_block`
    /// (`trees.c` L900-L951) should use. `symbol_at` establishes the symbol region's bounds
    /// afresh for every three bytes; this establishes them **once for the whole batch** and
    /// then walks it with [`slice::chunks_exact`], so the emission loop pays no range work
    /// per symbol. Each decoded pair is `(distance, length_or_literal)`, with a distance of
    /// zero marking a literal, exactly as in C (L913-L917).
    ///
    /// Reading a batch out before any of it is emitted is not merely allowed, it is safer
    /// than what C does. The compressed output and the unread symbols share one allocation,
    /// and `Assert(s->pending < s->lit_bufsize + sx, "pendingBuf overflow")` (L945) is what
    /// keeps the write cursor behind the read cursor. Copying the batch out first means the
    /// bytes this batch occupies may be overwritten while the batch is emitted -- which the
    /// assertion permits -- without any symbol being lost, and the assertion still keeps the
    /// cursor below `lit_bufsize + offset + 3 * filled`, so the *next* batch is untouched.
    ///
    /// Stops early at `sym_next`: a partial symbol at the end is not decoded, so the return
    /// value is always a whole number of symbols and `offset + SYMBOL_BYTES * returned`
    /// never exceeds `sym_next`.
    #[inline]
    pub fn decode_symbols(&self, offset: usize, out: &mut [(u16, u8)]) -> usize {
        let Some(available) = self.sym_next.checked_sub(offset) else {
            return 0;
        };
        let wanted = out.len().min(available / SYMBOL_BYTES);
        let Some(span) = wanted.checked_mul(SYMBOL_BYTES) else {
            return 0;
        };
        let Some(start) = self.lit_bufsize.checked_add(offset) else {
            return 0;
        };
        let Some(end) = start.checked_add(span) else {
            return 0;
        };
        // The one range check the whole batch pays. `None` is unreachable while `sym_next`
        // is within `sym_end`, which `push_symbol` and `set_sym_next` both maintain.
        let Some(bytes) = self.buf.as_ref().get(start..end) else {
            return 0;
        };

        let mut filled = 0;
        for (slot, symbol) in out.iter_mut().zip(bytes.chunks_exact(SYMBOL_BYTES)) {
            match symbol {
                [low, high, len_or_lit] => {
                    *slot = (u16::from_le_bytes([*low, *high]), *len_or_lit);
                    filled += 1;
                }
                // Unreachable: `chunks_exact` yields only full-length chunks.
                _ => break,
            }
        }
        filled
    }

    /// The symbol bytes written so far, i.e. `sym_buf[0..sym_next]`.
    #[must_use]
    #[inline]
    pub fn symbols(&self) -> &[u8] {
        let Some(end) = self.lit_bufsize.checked_add(self.sym_next) else {
            return &[];
        };
        // The `None` arm is unreachable: `lit_bufsize + sym_next <= lit_bufsize + sym_end <
        // buf.len()`.
        self.buf
            .as_ref()
            .get(self.lit_bufsize..end)
            .unwrap_or_default()
    }

    /// A read cursor over [`PendingBuf::symbols`], for walking the buffer three bytes at a
    /// time as `compress_block` does with `sx` (`trees.c` L913-L948).
    #[must_use]
    #[inline]
    pub fn symbol_cursor(&self) -> WeakSlice<'_, u8> {
        WeakSlice::new(self.symbols())
    }

    /// The symbol bytes written so far, writable, for bulk work such as the symbol copy in
    /// `deflateCopy` (`deflate.c` L1368).
    #[inline]
    pub fn symbols_mut(&mut self) -> &mut [u8] {
        let start = self.lit_bufsize;
        let Some(end) = start.checked_add(self.sym_next) else {
            return &mut [];
        };
        // The `None` arm is unreachable, as in `symbols`.
        self.buf.as_mut().get_mut(start..end).unwrap_or_default()
    }

    /// Clears the symbol buffer, as `init_block` does with `s->sym_next = 0`.
    #[inline]
    pub fn reset_symbols(&mut self) {
        self.sym_next = 0;
    }

    /// Overrides the symbol cursor, returning `false` when `sym_next` is past `sym_end` or
    /// is not a whole number of symbols.
    ///
    /// Needed for state duplication, where `deflateCopy` copies `sym_next` along with the
    /// symbol bytes (`deflate.c` L1339 and L1368).
    #[inline]
    pub fn set_sym_next(&mut self, sym_next: usize) -> bool {
        if sym_next > self.sym_end || sym_next % SYMBOL_BYTES != 0 {
            return false;
        }
        self.sym_next = sym_next;
        true
    }

    /// Splits the allocation at the symbol read cursor, yielding a write cursor over
    /// everything before it and a read cursor over the symbols not yet consumed.
    ///
    /// `consumed` is `sx` from `compress_block` (`trees.c` L913-L948), so the split point is
    /// `lit_bufsize + consumed` — precisely the bound that function asserts,
    /// `Assert(s->pending < s->lit_bufsize + sx, "pendingBuf overflow")` (`trees.c` L945).
    /// Bytes below it are dead: their symbols have already been emitted, so pending output may
    /// legally overwrite them, which is what makes the overlay work (`deflate.c` L466-L503).
    ///
    /// Returns [`None`] when `consumed` exceeds `sym_next`, or when `pending` has already
    /// passed the split point — that is the C assertion failing, and refusing is better than
    /// handing out a writer that would corrupt unread symbols.
    ///
    /// This is the module's only `split_at_mut`, and the index is validated first, so the
    /// call itself cannot panic and the two halves are disjoint by construction. The write
    /// cursor starts at `pending`; once the borrow ends, report its final position with
    /// [`PendingBuf::set_pending`].
    ///
    /// Most callers do not need this. [`PendingBuf::symbol_at`] copies a symbol out by value
    /// and leaves no borrow outstanding, which is simpler and just as fast.
    #[inline]
    pub fn split_at_symbol_cursor(
        &mut self,
        consumed: usize,
    ) -> Option<(WeakSliceMut<'_, u8>, WeakSlice<'_, u8>)> {
        if consumed > self.sym_next {
            return None;
        }
        let split = self.lit_bufsize.checked_add(consumed)?;
        let symbols_end = self.lit_bufsize.checked_add(self.sym_next)?;
        let unread = symbols_end.checked_sub(split)?;
        let pending = self.pending;
        if pending > split {
            return None;
        }

        let buf = self.buf.as_mut();
        if split > buf.len() || symbols_end > buf.len() {
            return None;
        }
        // `split <= buf.len()` is checked above, so `split_at_mut` cannot panic.
        let (front, back) = buf.split_at_mut(split);
        // Downgrade the second half to a shared borrow, keeping the full lifetime. The two
        // halves are disjoint, so holding a write cursor over one and a read cursor over the
        // other is sound without any further reasoning.
        let readable: &[u8] = back;
        let symbols = readable.get(..unread)?;
        Some((WeakSliceMut::at(front, pending)?, WeakSlice::new(symbols)))
    }

    /// Returns the wrapped buffer, so the owner can hand it back to the allocator that
    /// produced it.
    #[must_use]
    #[inline]
    pub fn into_inner(self) -> S {
        self.buf
    }
}

#[cfg(test)]
// The workspace denies the panic family in library code, which is exactly the point of this
// module; a test that cannot assert is useless, so the harness opts back in here only.
// Indexing is allowed too, because the expectations are written against literal, known-good
// indices.
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines
)]
mod tests {
    use super::{
        HashChains, IPos, PendingBuf, Pos, WeakSlice, WeakSliceMut, Window, LIT_BUFS,
        MAX_HASH_BITS, MAX_LIT_BUFSIZE, MAX_MATCH, MAX_WBITS, MIN_HASH_BITS, MIN_LIT_BUFSIZE,
        MIN_LOOKAHEAD, MIN_MATCH, MIN_WBITS, NIL, SYMBOL_BYTES, WIN_INIT,
    };

    use alloc::vec;
    use alloc::vec::Vec;

    /// `w_bits` for the window fixtures: the smallest a deflate stream can have, because
    /// `deflateInit2_` promotes 8 to 9 (`deflate.c` L439). Small enough for stack fixtures,
    /// large enough that `MIN_LOOKAHEAD` and `MAX_MATCH` bounds are real.
    const W_BITS: u32 = MIN_WBITS;
    const W_SIZE: usize = 1 << W_BITS;
    const WINDOW_SIZE: usize = 2 * W_SIZE;

    /// Exponent for the hash-chain fixtures, used for both `w_bits` and `hash_bits`.
    ///
    /// [`HashChains`] takes one storage type for both arrays, so a fixture built from
    /// fixed-size arrays needs `hash_size == w_size`. That is not a contrived shape: it is the
    /// shipped default, where `memLevel` 8 and `windowBits` 15 give `hash_bits` 15
    /// (`deflate.c` L453) and `w_bits` 15 (L449), i.e. 32768 entries each. Tests that need
    /// differing lengths use `&mut [u16]` storage instead, which carries its length at run
    /// time exactly as the real `Box<[u16]>` buffers do.
    const CHAIN_BITS: u32 = MIN_WBITS;
    const CHAIN_SIZE: usize = 1 << CHAIN_BITS;

    /// `lit_bufsize` for the pending-buffer fixtures, i.e. `memLevel == 1`
    /// (`deflate.c` L464).
    const LIT_BUFSIZE: usize = MIN_LIT_BUFSIZE;
    const PENDING_SIZE: usize = LIT_BUFS * LIT_BUFSIZE;

    /// The byte `test/infcover.c` fills every allocation with, used here for the same reason:
    /// a fixture that is not zero-filled catches any code that assumes zeroed memory.
    const GARBAGE: u8 = 0xa5;

    /// Deterministic, position-dependent filler, never zero, so that a misplaced copy or an
    /// unexpected zero-fill is visible.
    fn pattern(index: usize) -> u8 {
        let low = u8::try_from(index % 251).unwrap();
        (low ^ 0x5a) | 1
    }

    fn filled_window() -> Window<[u8; WINDOW_SIZE]> {
        let mut buf = [0u8; WINDOW_SIZE];
        for (index, slot) in buf.iter_mut().enumerate() {
            *slot = pattern(index);
        }
        Window::new(buf, W_BITS).unwrap()
    }

    /// Two hash arrays pre-filled with the allocator garbage pattern, ready to be wrapped.
    fn garbage_arrays() -> ([u16; CHAIN_SIZE], [u16; CHAIN_SIZE]) {
        ([0xa5a5; CHAIN_SIZE], [0xa5a5; CHAIN_SIZE])
    }

    fn garbage_pending() -> PendingBuf<[u8; PENDING_SIZE]> {
        PendingBuf::new([GARBAGE; PENDING_SIZE], LIT_BUFSIZE).unwrap()
    }

    /// Whether `bytes` equals the pattern for `start..start + bytes.len()`.
    fn matches_pattern(bytes: &[u8], start: usize) -> bool {
        bytes
            .iter()
            .enumerate()
            .all(|(offset, byte)| *byte == pattern(start + offset))
    }

    // ------------------------------------------------------------------ constants ---------

    #[test]
    fn constants_match_the_c_definitions() {
        assert_eq!(NIL, 0, "deflate.c L85");
        assert_eq!(MIN_MATCH, 3, "zutil.h L92");
        assert_eq!(MAX_MATCH, 258, "zutil.h L93");
        assert_eq!(MIN_LOOKAHEAD, 262, "deflate.h L296");
        assert_eq!(WIN_INIT, 258, "deflate.h L306");
        assert_eq!(LIT_BUFS, 4, "deflate.h L229");
        assert_eq!(SYMBOL_BYTES, 3, "deflate.h L368-L370");
        assert_eq!(MIN_WBITS, 9, "deflate.c L439");
        assert_eq!(MAX_WBITS, 15, "deflate.c L435");
        assert_eq!(MIN_HASH_BITS, 8, "deflate.c L453 at memLevel 1");
        assert_eq!(MAX_HASH_BITS, 16, "deflate.c L453 at memLevel 9");
        assert_eq!(MIN_LIT_BUFSIZE, 128, "deflate.c L486-L487");
        assert_eq!(MAX_LIT_BUFSIZE, 32_768, "deflate.c L486-L487");
    }

    // -------------------------------------------------------------------- indices ---------

    #[test]
    fn pos_nil_is_never_a_match_position() {
        assert!(Pos::NIL.is_nil());
        assert_eq!(Pos::NIL.get(), NIL);
        assert_eq!(
            Pos::NIL.to_index(),
            0,
            "as storage, NIL is simply index zero"
        );
        assert_eq!(
            Pos::NIL.match_index(),
            None,
            "as a candidate, NIL is no match at all -- deflate.c L1396-L1400"
        );

        let real = Pos::new(1);
        assert!(!real.is_nil());
        assert_eq!(real.match_index(), Some(1));
        assert_eq!(usize::from(real), 1);
        assert_eq!(u16::from(real), 1);
        assert_eq!(Pos::default(), Pos::NIL);
    }

    #[test]
    fn pos_narrowing_is_checked() {
        assert_eq!(Pos::from_index(0), Some(Pos::NIL));
        assert_eq!(Pos::from_index(65_535), Some(Pos::MAX));
        assert_eq!(
            Pos::from_index(65_536),
            None,
            "the C cast at L163 would truncate"
        );
        assert_eq!(Pos::from_index(usize::MAX), None);
    }

    #[test]
    fn pos_slid_down_matches_slide_hash() {
        // *p = (Pos)(m >= wsize ? m - wsize : NIL) -- deflate.c L196 and L203.
        let w_size = u16::try_from(W_SIZE).unwrap();
        assert_eq!(Pos::new(w_size).slid_down(w_size), Pos::NIL);
        assert_eq!(Pos::new(w_size + 7).slid_down(w_size), Pos::new(7));
        assert_eq!(Pos::new(w_size - 1).slid_down(w_size), Pos::NIL);
        assert_eq!(Pos::NIL.slid_down(w_size), Pos::NIL);
    }

    #[test]
    fn ipos_round_trips_through_pos() {
        assert!(IPos::NIL.is_nil());
        assert_eq!(IPos::NIL.match_index(), None);
        assert_eq!(IPos::default(), IPos::NIL);
        assert_eq!(IPos::from_pos(Pos::new(9)).get(), 9);
        assert_eq!(IPos::from(Pos::new(9)), IPos::new(9));
        assert_eq!(IPos::new(9).to_pos(), Some(Pos::new(9)));
        assert_eq!(IPos::new(9).match_index(), Some(9));
        assert_eq!(IPos::new(9).to_index(), Some(9));
        assert_eq!(IPos::new(70_000).to_pos(), None, "too wide for a Pos");
        assert_eq!(IPos::from_index(4), Some(IPos::new(4)));
        assert_eq!(u32::from(IPos::new(4)), 4);
    }

    // -------------------------------------------------------------------- cursors ---------

    #[test]
    fn weak_slice_cursor_never_runs_off_the_end() {
        let data = [1u8, 2, 3, 4];
        let mut cursor = WeakSlice::new(&data);
        assert_eq!(cursor.len(), 4);
        assert!(!cursor.is_empty());
        assert_eq!(cursor.offset(), 0);
        assert_eq!(cursor.remaining(), 4);
        assert_eq!(cursor.peek(3), Some(&4));
        assert_eq!(cursor.peek(4), None);
        assert_eq!(cursor.peek(usize::MAX), None);

        assert_eq!(cursor.take_next(), Some(&1));
        assert_eq!(cursor.offset(), 1);
        assert_eq!(cursor.remainder(), &[2, 3, 4]);
        assert_eq!(cursor.peek_slice(3), Some(&[2u8, 3, 4][..]));
        assert_eq!(cursor.peek_slice(4), None);
        assert_eq!(cursor.peek_slice(usize::MAX), None);

        assert!(cursor.advance(3));
        assert_eq!(cursor.remaining(), 0);
        assert_eq!(cursor.take_next(), None);
        assert!(!cursor.advance(1));
        assert!(!cursor.advance(usize::MAX));
        assert_eq!(
            cursor.offset(),
            4,
            "a refused advance must not move the cursor"
        );
        assert_eq!(cursor.remainder(), &[] as &[u8]);
        assert!(cursor.set_offset(0));
        assert!(!cursor.set_offset(5));
        assert_eq!(
            cursor.offset(),
            0,
            "a refused seek must not move the cursor"
        );

        assert!(
            WeakSlice::at(&data, 4).is_some(),
            "the end is a legal cursor position"
        );
        assert!(WeakSlice::at(&data, 5).is_none());
        assert_eq!(WeakSlice::new(&data).as_slice(), &data);
        assert!(WeakSlice::<u8>::new(&[]).is_empty());
    }

    #[test]
    fn weak_slice_mut_writes_only_inside_its_borrow() {
        let mut data = [0u8; 3];
        {
            let mut cursor = WeakSliceMut::new(&mut data);
            assert_eq!(cursor.len(), 3);
            assert!(!cursor.is_empty());
            assert!(cursor.put(7));
            assert!(cursor.put(8));
            assert_eq!(cursor.offset(), 2);
            assert_eq!(cursor.remaining(), 1);
            assert_eq!(cursor.remainder_mut(), &mut [0u8][..]);
            assert!(cursor.put(9));
            assert!(!cursor.put(10), "a full buffer must refuse the write");
            assert_eq!(cursor.as_slice(), &[7, 8, 9]);
            assert!(!cursor.advance(1));
            assert_eq!(cursor.offset(), 3);
            assert!(cursor.set_offset(1));
            assert_eq!(cursor.as_mut_slice(), &mut [7u8, 8, 9][..]);

            let (front, back) = cursor.split_at(1).unwrap();
            assert_eq!(front, &mut [7u8][..]);
            assert_eq!(back, &mut [8u8, 9][..]);
            assert!(cursor.split_at(4).is_none());
            assert!(cursor.split_at(usize::MAX).is_none());
        }
        assert_eq!(
            data,
            [7, 8, 9],
            "nothing outside the cursor's own bytes changed"
        );

        let mut empty: [u8; 0] = [];
        assert!(WeakSliceMut::new(&mut empty).is_empty());
        assert!(WeakSliceMut::at(&mut empty, 1).is_none());
    }

    // --------------------------------------------------------------------- window ---------

    #[test]
    fn window_construction_validates_its_buffer() {
        assert!(Window::new([0u8; WINDOW_SIZE], W_BITS).is_some());
        assert!(
            Window::new([0u8; WINDOW_SIZE - 1], W_BITS).is_none(),
            "a short buffer must be refused"
        );
        assert!(
            Window::new([0u8; WINDOW_SIZE + 1], W_BITS).is_none(),
            "the length must be exactly 2 * w_size -- deflate.c L458 and L683"
        );
        assert!(
            Window::new([0u8; WINDOW_SIZE], MIN_WBITS - 1).is_none(),
            "w_bits below MIN_WBITS must be refused -- deflate.c L439"
        );
        assert!(
            Window::new([0u8; WINDOW_SIZE], MAX_WBITS + 1).is_none(),
            "w_bits above MAX_WBITS must be refused -- deflate.c L435"
        );

        let window = filled_window();
        assert_eq!(window.w_bits(), W_BITS);
        assert_eq!(window.w_size(), W_SIZE);
        assert_eq!(window.w_mask(), W_SIZE - 1);
        assert_eq!(window.window_size(), WINDOW_SIZE);
        assert_eq!(window.max_dist(), W_SIZE - MIN_LOOKAHEAD);
        assert_eq!(window.high_water(), 0);
        assert_eq!(window.strstart, 0);
        assert_eq!(window.lookahead, 0);
        assert_eq!(window.block_start, 0);
        assert_eq!(window.match_start, 0);
        assert_eq!(window.insert, 0);
    }

    #[test]
    fn window_slide_moves_the_upper_half_down() {
        // Reproduces fill_window's slide -- zmemcpy(window, window + wsize, wsize - more) --
        // together with the cursor adjustments that follow it, deflate.c L285-L295.
        let mut window = filled_window();
        window.strstart = W_SIZE + window.max_dist();
        window.match_start = W_SIZE + 4;
        window.block_start = isize::try_from(W_SIZE + 2).unwrap();
        window.lookahead = 5;
        window.insert = window.strstart;

        let more = window.free_space();
        let count = W_SIZE - more;
        assert!(count > 0 && count <= W_SIZE);

        assert!(window.slide_down(count));

        window.strstart -= W_SIZE;
        window.match_start -= W_SIZE;
        window.block_start -= isize::try_from(W_SIZE).unwrap();
        window.clamp_insert_to_strstart();

        assert!(
            matches_pattern(window.region(0, count).unwrap(), W_SIZE),
            "the second half must appear at the start, byte for byte"
        );
        assert!(
            matches_pattern(window.region(count, WINDOW_SIZE - count).unwrap(), count),
            "bytes above the copied region must be untouched"
        );
        assert_eq!(window.strstart, window.max_dist());
        assert_eq!(window.match_start, 4);
        assert_eq!(window.block_start, 2);
        assert_eq!(
            window.insert, window.strstart,
            "insert is clamped to strstart after a slide -- deflate.c L291-L292"
        );
    }

    #[test]
    fn window_slide_agrees_with_a_naive_forward_copy() {
        // Every slide the algorithm can produce copies `count <= w_size` bytes from offset
        // w_size down to offset 0, so source and destination cannot overlap and a forward
        // copy is the reference. Larger counts are refused outright rather than truncated.
        for count in [0usize, 1, MIN_MATCH, MAX_MATCH, W_SIZE - 1, W_SIZE] {
            let mut window = filled_window();
            let mut naive = [0u8; WINDOW_SIZE];
            for (index, slot) in naive.iter_mut().enumerate() {
                *slot = pattern(index);
            }
            for offset in 0..count {
                naive[offset] = naive[W_SIZE + offset];
            }

            assert!(window.slide_down(count), "count {count} must be accepted");
            assert_eq!(
                window.region(0, WINDOW_SIZE).unwrap(),
                &naive[..],
                "slide_down({count}) must equal a naive forward copy"
            );
        }

        let mut window = filled_window();
        assert!(
            !window.slide_down(W_SIZE + 1),
            "a slide reaching past the buffer must be refused"
        );
        assert!(!window.slide_down(usize::MAX));
        assert!(
            matches_pattern(window.region(0, WINDOW_SIZE).unwrap(), 0),
            "a refused slide must not have moved anything"
        );

        // `copy_within` has memmove semantics, so a forward copy is the right reference
        // whenever the destination is below the source -- which is the only direction
        // `slide_down` can produce. Demonstrated directly on overlapping ranges.
        let mut overlapping = [1u8, 2, 3, 4, 5];
        overlapping.copy_within(1..4, 0);
        let mut forward = [1u8, 2, 3, 4, 5];
        for offset in 0..3 {
            forward[offset] = forward[1 + offset];
        }
        assert_eq!(overlapping, forward);
        assert_eq!(overlapping, [2, 3, 4, 4, 5]);
    }

    #[test]
    fn window_win_init_tail_zeroes_exactly_max_match_bytes() {
        // fill_window's tail, first branch: zero WIN_INIT bytes past the data, or up to the
        // end of the window, whichever is less -- deflate.c L351-L360.
        let mut window = filled_window();
        window.strstart = 100;
        window.lookahead = 20;
        let curr = window.strstart + window.lookahead;

        window.initialize_win_init_tail();

        assert_eq!(window.high_water(), curr + WIN_INIT);
        assert!(
            window
                .region(curr, WIN_INIT)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0),
            "exactly WIN_INIT bytes past the data must be zeroed"
        );
        assert_eq!(
            window.byte(curr - 1),
            Some(pattern(curr - 1)),
            "the last data byte must be untouched"
        );
        assert_eq!(
            window.byte(curr + WIN_INIT),
            Some(pattern(curr + WIN_INIT)),
            "nothing beyond WIN_INIT may be touched"
        );

        // Second branch: the mark is at or above the data but below curr + WIN_INIT, so only
        // the newly exposed bytes are zeroed -- deflate.c L361-L371.
        let previous = window.high_water();
        window.lookahead += 10;
        let grown = window.strstart + window.lookahead;
        window.initialize_win_init_tail();
        assert_eq!(window.high_water(), grown + WIN_INIT);
        assert!(
            window
                .region(previous, 10)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0),
            "the ten newly exposed bytes are zeroed"
        );
        assert_eq!(
            window.byte(grown + WIN_INIT),
            Some(pattern(grown + WIN_INIT)),
            "and nothing past the new mark"
        );

        // Clamped at the end of the window: init = window_size - curr -- deflate.c L355-L357.
        let mut edge = filled_window();
        edge.strstart = WINDOW_SIZE - 4;
        edge.lookahead = 0;
        edge.initialize_win_init_tail();
        assert_eq!(edge.high_water(), WINDOW_SIZE);
        assert_eq!(
            edge.region(WINDOW_SIZE - 4, 4).unwrap(),
            &[0, 0, 0, 0],
            "zeroing stops at the end of the window"
        );

        // Nothing to do once the whole window has been initialized -- deflate.c L347.
        let mut done = filled_window();
        assert!(done.set_high_water(WINDOW_SIZE));
        done.initialize_win_init_tail();
        assert!(matches_pattern(done.region(0, WINDOW_SIZE).unwrap(), 0));

        // An impossible cursor pair is refused rather than used to compute a length.
        let mut broken = filled_window();
        broken.strstart = WINDOW_SIZE;
        broken.lookahead = 1;
        broken.initialize_win_init_tail();
        assert_eq!(broken.high_water(), 0);
        broken.strstart = usize::MAX;
        broken.initialize_win_init_tail();
        assert_eq!(broken.high_water(), 0);
        assert!(matches_pattern(broken.region(0, WINDOW_SIZE).unwrap(), 0));
    }

    #[test]
    fn window_high_water_tracks_written_bytes() {
        let mut window = filled_window();
        window.strstart = 64;
        window.raise_high_water_to_strstart();
        assert_eq!(window.high_water(), 64, "deflate.c L1786-L1787");
        window.strstart = 32;
        window.raise_high_water_to_strstart();
        assert_eq!(window.high_water(), 64, "the mark never moves backwards");
        assert!(window.set_high_water(0));
        assert_eq!(window.high_water(), 0);
        assert!(!window.set_high_water(WINDOW_SIZE + 1));
        assert_eq!(
            window.high_water(),
            0,
            "a refused write leaves the mark alone"
        );
    }

    #[test]
    fn window_scan_pair_yields_two_overlapping_reads() {
        // scan = window + strstart and match = window + cur_match -- deflate.c L1391, L1436.
        let mut window = filled_window();
        window.strstart = WINDOW_SIZE - MIN_LOOKAHEAD;
        let candidate = window.strstart - 8;

        let (scan, matched) = window
            .scan_pair(window.strstart, candidate, MAX_MATCH + 1)
            .unwrap();
        assert_eq!(scan.len(), MAX_MATCH + 1);
        assert_eq!(matched.len(), MAX_MATCH + 1);
        assert!(
            matches_pattern(scan, window.strstart) && matches_pattern(matched, candidate),
            "both views read the window at their own offset"
        );
        assert_eq!(
            scan[MAX_MATCH],
            pattern(window.strstart + MAX_MATCH),
            "the scan reaches strend = window + strstart + MAX_MATCH -- deflate.c L1412"
        );
        assert_eq!(
            matched[8], scan[0],
            "the two shared views overlap, which is what a hash-chain candidate produces"
        );

        assert!(window.scan_pair(WINDOW_SIZE - 1, 0, 2).is_none());
        assert!(window.scan_pair(0, WINDOW_SIZE - 1, 2).is_none());
        assert!(window.scan_pair(usize::MAX, 0, 1).is_none());
        assert!(window.scan_pair(0, 0, usize::MAX).is_none());
    }

    #[test]
    fn window_free_space_is_exactly_the_writable_tail() {
        let mut window = filled_window();
        window.strstart = 40;
        window.lookahead = 60;
        assert_eq!(window.free_space(), WINDOW_SIZE - 100, "deflate.c L260");
        assert_eq!(window.free_space_mut().unwrap().len(), WINDOW_SIZE - 100);

        window.free_space_mut().unwrap()[0] = 0;
        assert_eq!(
            window.byte(100),
            Some(0),
            "the region starts at strstart + lookahead"
        );
        assert_eq!(
            window.byte(99),
            Some(pattern(99)),
            "and not one byte earlier"
        );

        window.strstart = WINDOW_SIZE;
        window.lookahead = 0;
        assert_eq!(window.free_space(), 0);
        assert_eq!(window.free_space_mut().unwrap().len(), 0);

        window.lookahead = 1;
        assert_eq!(
            window.free_space(),
            0,
            "the count saturates instead of wrapping"
        );
        assert!(window.free_space_mut().is_none());
        window.strstart = usize::MAX;
        assert!(window.free_space_mut().is_none());
    }

    #[test]
    fn window_regions_and_copies_are_bounds_checked() {
        let mut window = filled_window();
        assert!(window.write_at(4, &[1, 2, 3]));
        assert_eq!(window.region(4, 3).unwrap(), &[1, 2, 3]);
        assert!(!window.write_at(WINDOW_SIZE - 1, &[1, 2]));
        assert!(!window.write_at(usize::MAX, &[1]));

        let mut out = [0u8; 3];
        assert!(window.copy_out(4, &mut out));
        assert_eq!(out, [1, 2, 3]);
        assert!(!window.copy_out(WINDOW_SIZE - 2, &mut out));
        assert!(!window.copy_out(usize::MAX, &mut out));

        assert!(window.region(0, WINDOW_SIZE).is_some());
        assert!(window.region(0, WINDOW_SIZE + 1).is_none());
        assert!(window.region(usize::MAX, 1).is_none());
        assert!(window.region_mut(1, usize::MAX).is_none());
        assert_eq!(window.byte(WINDOW_SIZE), None);
        assert_eq!(window.byte(usize::MAX), None);
    }

    #[test]
    fn window_block_start_may_be_negative() {
        let mut window = filled_window();
        window.strstart = 300;
        window.block_start = 100;
        assert_eq!(
            window.bytes_since_block_start(),
            Some(200),
            "deflate.c L1693"
        );
        assert!(
            matches_pattern(window.block_region(4).unwrap(), 100),
            "the block region starts at block_start -- deflate.c L1839"
        );

        window.block_start = -1;
        assert_eq!(
            window.block_region(1),
            None,
            "a negative block_start has no window region"
        );
        assert_eq!(window.bytes_since_block_start(), Some(301));

        window.block_start = isize::try_from(WINDOW_SIZE).unwrap();
        assert_eq!(window.block_region(1), None);
        assert_eq!(
            window.bytes_since_block_start(),
            None,
            "block_start ahead of strstart is refused, not wrapped"
        );
    }

    #[test]
    fn window_into_inner_returns_the_buffer() {
        let window = Window::new([7u8; WINDOW_SIZE], W_BITS).unwrap();
        assert_eq!(window.into_inner().len(), WINDOW_SIZE);
    }

    // ---------------------------------------------------------------- hash chains ---------

    #[test]
    fn hash_chains_construction_validates_both_arrays() {
        let (mut head, mut prev) = garbage_arrays();

        assert!(
            HashChains::new(
                &mut head[..CHAIN_SIZE - 1],
                &mut prev[..],
                CHAIN_BITS,
                CHAIN_BITS
            )
            .is_none(),
            "head must hold exactly hash_size entries -- deflate.c L460"
        );
        assert!(
            HashChains::new(
                &mut head[..],
                &mut prev[..CHAIN_SIZE - 1],
                CHAIN_BITS,
                CHAIN_BITS
            )
            .is_none(),
            "prev must hold exactly w_size entries -- deflate.c L459"
        );
        assert!(
            HashChains::new(&mut head[..], &mut prev[..], MIN_WBITS - 1, CHAIN_BITS).is_none(),
            "w_bits below MIN_WBITS must be refused"
        );
        assert!(
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, MAX_HASH_BITS + 1).is_none(),
            "hash_bits above MAX_HASH_BITS must be refused"
        );
        assert!(
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, MIN_HASH_BITS - 1).is_none(),
            "hash_bits below MIN_HASH_BITS must be refused"
        );

        let chains = HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, CHAIN_BITS).unwrap();
        assert_eq!(chains.w_bits(), CHAIN_BITS);
        assert_eq!(chains.w_size(), CHAIN_SIZE);
        assert_eq!(chains.w_mask(), CHAIN_SIZE - 1);
        assert_eq!(chains.hash_bits(), CHAIN_BITS);
        assert_eq!(chains.hash_size(), CHAIN_SIZE);
        assert_eq!(chains.hash_mask(), CHAIN_SIZE - 1);
        assert!(!chains.slid());
        let (returned_head, returned_prev) = chains.into_inner();
        assert_eq!(returned_head.len(), CHAIN_SIZE);
        assert_eq!(returned_prev.len(), CHAIN_SIZE);
    }

    #[test]
    fn hash_chains_slide_subtracts_or_nils_every_entry() {
        // slide_hash: *p = (Pos)(m >= wsize ? m - wsize : NIL) -- deflate.c L187-L210.
        let (mut head, mut prev) = garbage_arrays();
        let mut chains =
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, CHAIN_BITS).unwrap();
        let w_size = u16::try_from(CHAIN_SIZE).unwrap();
        chains.clear();

        let below = w_size - 3;
        let above = w_size + 11;
        chains.set_head_at(0, Pos::new(below));
        chains.set_head_at(1, Pos::new(w_size));
        chains.set_head_at(2, Pos::new(above));
        chains.set_head_at(3, Pos::NIL);
        chains.set_prev_at(0, Pos::new(below));
        chains.set_prev_at(1, Pos::new(w_size));
        chains.set_prev_at(2, Pos::new(above));
        chains.set_prev_at(3, Pos::NIL);

        assert!(!chains.slid());
        chains.slide();
        assert!(
            chains.slid(),
            "slide_hash sets s->slid = 1 -- deflate.c L209"
        );

        assert_eq!(
            chains.head_at(0),
            Pos::NIL,
            "entries below wsize become NIL"
        );
        assert_eq!(
            chains.head_at(1),
            Pos::NIL,
            "wsize itself maps to zero, i.e. NIL"
        );
        assert_eq!(
            chains.head_at(2),
            Pos::new(11),
            "entries at or above wsize lose wsize"
        );
        assert_eq!(chains.head_at(3), Pos::NIL);
        assert_eq!(chains.prev_at(0), Pos::NIL);
        assert_eq!(chains.prev_at(1), Pos::NIL);
        assert_eq!(chains.prev_at(2), Pos::new(11));
        assert_eq!(chains.prev_at(3), Pos::NIL);

        // `clear` zeroed every head, so untouched heads slide to NIL, while `prev` still
        // holds the allocator's 0xa5a5 -- which is above wsize and therefore slides down.
        assert_eq!(chains.head_at(4), Pos::NIL);
        assert_eq!(chains.prev_at(4), Pos::new(0xa5a5 - w_size));

        chains.set_slid(false);
        assert!(!chains.slid());
    }

    #[test]
    fn hash_chains_clear_touches_only_the_heads() {
        // CLEAR_HASH zeroes head and clears slid -- deflate.c L170-L175. prev is left as the
        // allocator produced it: "prev[] will be initialized on the fly" -- deflate.c L168.
        let (mut head, mut prev) = garbage_arrays();
        let mut chains =
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, CHAIN_BITS).unwrap();
        chains.slide();
        assert!(chains.slid());
        chains.clear();
        assert!(!chains.slid());
        assert!(chains.head_entries().iter().all(|entry| *entry == NIL));
        assert!(
            chains.prev_entries().iter().any(|entry| *entry != NIL),
            "prev must not be cleared"
        );

        chains.prev_entries_mut()[0] = 5;
        assert_eq!(chains.prev_at(0), Pos::new(5));
        chains.head_entries_mut()[0] = 6;
        assert_eq!(chains.head_at(0), Pos::new(6));
    }

    #[test]
    fn hash_chains_insert_string_builds_the_chain() {
        // match_head = prev[str & w_mask] = head[ins_h]; head[ins_h] = (Pos)str -- L160-L163.
        let (mut head, mut prev) = garbage_arrays();
        let mut chains =
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, CHAIN_BITS).unwrap();
        chains.clear();
        let hash = 42;

        assert_eq!(chains.insert_string(hash, 7), Some(Pos::NIL));
        assert_eq!(chains.head_at(hash), Pos::new(7));
        assert_eq!(
            chains.prev_at(7),
            Pos::NIL,
            "the first insertion terminates the chain"
        );

        assert_eq!(chains.insert_string(hash, 9), Some(Pos::new(7)));
        assert_eq!(chains.head_at(hash), Pos::new(9));
        assert_eq!(
            chains.prev_at(9),
            Pos::new(7),
            "the old head becomes the prev link"
        );

        assert_eq!(chains.insert_string(hash, 11), Some(Pos::new(9)));

        // Walk the chain newest to oldest, exactly as longest_match does at L1525.
        let first = chains.head_at(hash).match_index();
        assert_eq!(first, Some(11));
        let second = chains.prev_at(11).match_index();
        assert_eq!(second, Some(9));
        let third = chains.prev_at(9).match_index();
        assert_eq!(third, Some(7));
        assert_eq!(
            chains.prev_at(7).match_index(),
            None,
            "and then the chain ends"
        );

        assert_eq!(chains.insert_string(hash, usize::MAX), None);
        assert_eq!(
            chains.head_at(hash),
            Pos::new(11),
            "a refused insert changes nothing"
        );
    }

    #[test]
    fn hash_chains_indices_are_masked_like_the_c_macros() {
        let (mut head, mut prev) = garbage_arrays();
        let mut chains =
            HashChains::new(&mut head[..], &mut prev[..], CHAIN_BITS, CHAIN_BITS).unwrap();
        chains.clear();
        chains.set_head_at(CHAIN_SIZE + 5, Pos::new(3));
        assert_eq!(
            chains.head_at(5),
            Pos::new(3),
            "hash values are reduced by hash_mask, as UPDATE_HASH does -- deflate.c L141"
        );
        chains.set_prev_at(CHAIN_SIZE + 6, Pos::new(4));
        assert_eq!(
            chains.prev_at(6),
            Pos::new(4),
            "prev indices are window offsets modulo w_size -- deflate.h L139-L142"
        );
        assert_eq!(chains.prev_at(usize::MAX), chains.prev_at(CHAIN_SIZE - 1));
        assert_eq!(chains.head_at(usize::MAX), chains.head_at(CHAIN_SIZE - 1));
    }

    // -------------------------------------------------------------- pending buffer --------

    #[test]
    fn pending_buf_construction_reproduces_the_c_layout() {
        let buffer = garbage_pending();
        assert_eq!(buffer.lit_bufsize(), LIT_BUFSIZE);
        assert_eq!(buffer.pending_buf_size(), LIT_BUFSIZE * 4, "deflate.c L506");
        assert_eq!(buffer.sym_buf_offset(), LIT_BUFSIZE, "deflate.c L520");
        assert_eq!(buffer.sym_end(), (LIT_BUFSIZE - 1) * 3, "deflate.c L521");
        assert_eq!(buffer.pending(), 0);
        assert_eq!(buffer.pending_out_offset(), 0);
        assert_eq!(buffer.sym_next(), 0);
        assert_eq!(buffer.symbol_count(), 0);
        assert_eq!(buffer.room(), PENDING_SIZE);

        assert!(PendingBuf::new([0u8; PENDING_SIZE - 1], LIT_BUFSIZE).is_none());
        assert!(PendingBuf::new([0u8; PENDING_SIZE + 1], LIT_BUFSIZE).is_none());
        assert!(
            PendingBuf::new([0u8; PENDING_SIZE], LIT_BUFSIZE + 64).is_none(),
            "lit_bufsize must be a power of two -- deflate.c L464"
        );
        assert!(
            PendingBuf::new([0u8; PENDING_SIZE], LIT_BUFSIZE / 2).is_none(),
            "below MIN_LIT_BUFSIZE"
        );
        assert!(
            PendingBuf::new([0u8; PENDING_SIZE], MAX_LIT_BUFSIZE * 2).is_none(),
            "above MAX_LIT_BUFSIZE"
        );
        assert_eq!(garbage_pending().into_inner().len(), PENDING_SIZE);
    }

    #[test]
    fn pending_buf_front_and_symbols_do_not_corrupt_each_other() {
        let mut buffer = garbage_pending();

        assert!(buffer.put_byte(0x78));
        assert!(
            buffer.put_short_msb(0x9c5a),
            "putShortMSB writes the high byte first"
        );
        assert_eq!(buffer.append(&[1, 2, 3]), 3);
        assert_eq!(buffer.pending(), 6);
        assert_eq!(buffer.written(), &[0x78, 0x9c, 0x5a, 1, 2, 3]);

        assert_eq!(buffer.push_symbol(0, b'h'), Some(false));
        assert_eq!(buffer.push_symbol(300, 7), Some(false));
        assert_eq!(buffer.sym_next(), 2 * SYMBOL_BYTES);
        assert_eq!(buffer.symbol_count(), 2);

        assert!(buffer.put_byte(4));
        assert_eq!(
            buffer.written(),
            &[0x78, 0x9c, 0x5a, 1, 2, 3, 4],
            "writing pending output must not disturb the symbols"
        );
        assert_eq!(
            buffer.symbol_at(0),
            Some((0, b'h')),
            "a literal is a zero distance -- deflate.h L359-L361"
        );
        assert_eq!(buffer.symbol_at(SYMBOL_BYTES), Some((300, 7)));
        assert_eq!(
            buffer.symbols(),
            &[0, 0, b'h', 44, 1, 7],
            "distance bytes are little-endian, then the length -- trees.c L1100-L1102"
        );
        assert_eq!(buffer.flushable(), &[0x78, 0x9c, 0x5a, 1, 2, 3, 4]);

        // The symbol bytes really do live at pending_buf + lit_bufsize -- deflate.c L520.
        let mut probe = garbage_pending();
        let base = probe.sym_buf_offset();
        assert_eq!(probe.push_symbol(0x0201, 9), Some(false));
        let bytes = probe.into_inner();
        assert_eq!(&bytes[base..base + SYMBOL_BYTES], &[0x01, 0x02, 9]);
        assert_eq!(
            bytes[0], GARBAGE,
            "and the front of the buffer is untouched"
        );
    }

    #[test]
    fn pending_buf_flush_accounting_matches_flush_pending() {
        // flush_pending: pending_out += len; pending -= len; rewind when empty -- L961-L967.
        let mut buffer = garbage_pending();
        for byte in 0..10u8 {
            assert!(buffer.put_byte(byte));
        }
        assert_eq!(buffer.flushable().len(), 10);

        assert!(buffer.consume_flushed(4));
        assert_eq!(buffer.pending(), 6);
        assert_eq!(buffer.pending_out_offset(), 4);
        assert_eq!(buffer.flushable(), &[4, 5, 6, 7, 8, 9]);
        assert_eq!(
            buffer.written().len(),
            6,
            "written() is measured from the base"
        );

        assert!(
            !buffer.consume_flushed(7),
            "cannot flush more than is pending"
        );
        assert!(buffer.consume_flushed(6));
        assert_eq!(buffer.pending(), 0);
        assert_eq!(
            buffer.pending_out_offset(),
            0,
            "an empty buffer rewinds the read offset to the base -- deflate.c L965-L967"
        );
        assert_eq!(buffer.flushable(), &[] as &[u8]);

        assert!(buffer.put_byte(1));
        buffer.reset_pending();
        assert_eq!(buffer.pending(), 0);
        assert_eq!(buffer.pending_out_offset(), 0);
    }

    #[test]
    fn pending_buf_respects_its_capacity() {
        let mut buffer = garbage_pending();
        let capacity = buffer.pending_buf_size();
        let filler = [9u8; PENDING_SIZE];
        assert_eq!(buffer.append(&filler), capacity);
        assert_eq!(buffer.pending(), capacity);
        assert_eq!(buffer.room(), 0);
        assert!(!buffer.put_byte(0), "a full buffer refuses put_byte");
        assert!(!buffer.put_short_msb(0));
        assert_eq!(buffer.append(&[1]), 0);

        assert!(buffer.patch_tail(4, &[1, 2, 3, 4]), "deflate.c L1716-L1719");
        assert_eq!(buffer.written_from(capacity - 4).unwrap(), &[1, 2, 3, 4]);
        assert!(
            buffer.patch_tail(4, &[5, 6]),
            "a shorter patch inside the tail is fine"
        );
        assert_eq!(buffer.written_from(capacity - 4).unwrap(), &[5, 6, 3, 4]);
        assert!(
            !buffer.patch_tail(2, &[1, 2, 3]),
            "a patch may not exceed its own window"
        );
        assert!(!buffer.patch_tail(capacity + 1, &[1]));
        assert_eq!(buffer.written_from(capacity + 1), None);
        assert_eq!(buffer.written_from(capacity).unwrap(), &[] as &[u8]);

        // put_short_msb is all-or-nothing, where two put_byte calls would leave one behind.
        let mut edge = garbage_pending();
        assert_eq!(edge.append(&filler[..capacity - 1]), capacity - 1);
        assert!(!edge.put_short_msb(0xbeef));
        assert_eq!(edge.pending(), capacity - 1, "nothing was written");
        assert!(edge.put_byte(0xef));
        assert_eq!(edge.pending(), capacity);
    }

    #[test]
    fn pending_buf_symbol_buffer_fills_at_sym_end() {
        let mut buffer = garbage_pending();
        let symbols = buffer.sym_end() / SYMBOL_BYTES;
        for index in 0..symbols - 1 {
            assert_eq!(
                buffer.push_symbol(1, 2),
                Some(false),
                "symbol {index} must not report the buffer full"
            );
        }
        assert_eq!(
            buffer.push_symbol(1, 2),
            Some(true),
            "the last symbol reports sym_next == sym_end -- deflate.h L374"
        );
        assert_eq!(buffer.sym_next(), buffer.sym_end());
        assert_eq!(buffer.symbol_count(), symbols);
        assert_eq!(
            buffer.push_symbol(1, 2),
            None,
            "a full symbol buffer refuses the write"
        );

        assert!(buffer.set_sym_next(SYMBOL_BYTES));
        assert_eq!(buffer.sym_next(), SYMBOL_BYTES);
        assert!(
            !buffer.set_sym_next(1),
            "sym_next must be a whole number of symbols"
        );
        assert!(!buffer.set_sym_next(buffer.sym_end() + SYMBOL_BYTES));
        buffer.reset_symbols();
        assert_eq!(buffer.sym_next(), 0);
        assert_eq!(buffer.symbols(), &[] as &[u8]);
        assert_eq!(
            buffer.symbol_at(0),
            Some((1, 2)),
            "resetting the cursor does not erase the bytes, exactly as init_block does not"
        );
        assert_eq!(buffer.symbol_at(usize::MAX), None);
        assert_eq!(buffer.symbol_at(buffer.pending_buf_size()), None);
    }

    #[test]
    fn pending_buf_decode_symbols_agrees_with_symbol_at_everywhere() {
        // The batched decoder is what `compress_block` walks the symbol buffer
        // with, so it has to be indistinguishable from calling `symbol_at` for
        // every offset -- including at the partial batch that ends the buffer, and
        // including past `sym_next`, where it must stop rather than read ahead.
        let mut buffer = garbage_pending();
        let symbols: Vec<(u16, u8)> = (0..37_u8)
            .map(|n| (u16::from(n).wrapping_mul(701), n.wrapping_mul(37)))
            .collect();
        for &(dist, len_or_lit) in &symbols {
            assert_eq!(buffer.push_symbol(dist, len_or_lit), Some(false));
        }

        for capacity in [1_usize, 2, 8, 32, 64] {
            let mut out = vec![(0_u16, 0_u8); capacity];
            let mut offset = 0;
            let mut seen: Vec<(u16, u8)> = Vec::new();
            while offset < buffer.sym_next() {
                let filled = buffer.decode_symbols(offset, &mut out);
                assert!(filled > 0, "capacity {capacity} stalled at {offset}");
                assert!(filled <= capacity);
                for (index, symbol) in out.iter().enumerate().take(filled) {
                    assert_eq!(
                        *symbol,
                        buffer.symbol_at(offset + index * SYMBOL_BYTES).unwrap(),
                        "capacity {capacity}, offset {offset}, index {index}"
                    );
                }
                seen.extend_from_slice(&out[..filled]);
                offset += filled * SYMBOL_BYTES;
            }
            assert_eq!(
                offset,
                buffer.sym_next(),
                "every symbol is consumed exactly once"
            );
            assert_eq!(seen, symbols, "capacity {capacity}");
        }

        // A whole batch of room at the very end yields only the symbols there are.
        let mut out = [(0_u16, 0_u8); 8];
        let tail = buffer.sym_next() - SYMBOL_BYTES;
        assert_eq!(buffer.decode_symbols(tail, &mut out), 1);
        // At and past `sym_next` there is nothing to decode.
        assert_eq!(buffer.decode_symbols(buffer.sym_next(), &mut out), 0);
        assert_eq!(buffer.decode_symbols(usize::MAX, &mut out), 0);
        // A zero-length destination asks for nothing and gets nothing.
        assert_eq!(buffer.decode_symbols(0, &mut []), 0);
    }

    #[test]
    fn pending_buf_put_short_le_is_all_or_nothing() {
        // The little-endian short is what the bit accumulator spills through, so
        // its byte order is the bitstream's: low byte first (`trees.c` L144-L147).
        let mut buffer = garbage_pending();
        assert!(buffer.put_short_le(0x1234));
        assert_eq!(buffer.written(), &[0x34, 0x12]);
        assert_eq!(buffer.pending(), 2);

        // Fill to one byte short of the end: the pair then does not fit, and
        // neither byte is written, where two independent appends would have left
        // the low byte behind.
        let room = buffer.room() - 1;
        for _ in 0..room {
            assert!(buffer.put_byte(0xa5));
        }
        assert_eq!(buffer.room(), 1);
        assert!(!buffer.put_short_le(0xbeef));
        assert_eq!(buffer.room(), 1, "a refused short writes nothing");
        assert!(
            buffer.put_byte(0x5a),
            "the single remaining byte still fits"
        );
        assert_eq!(buffer.room(), 0);
        assert!(!buffer.put_byte(0x5a));
        assert!(!buffer.put_short_le(0xbeef));
    }

    #[test]
    fn pending_buf_split_is_disjoint_and_bounded() {
        // The split point is the bound trees.c L945 asserts: pending < lit_bufsize + sx.
        let mut buffer = garbage_pending();
        assert_eq!(buffer.push_symbol(0x0102, 3), Some(false));
        assert_eq!(buffer.push_symbol(0x0405, 6), Some(false));
        assert!(buffer.put_byte(0xff));

        {
            let (mut writer, symbols) = buffer.split_at_symbol_cursor(SYMBOL_BYTES).unwrap();
            assert_eq!(writer.offset(), 1, "the write cursor starts at pending");
            assert_eq!(writer.len(), LIT_BUFSIZE + SYMBOL_BYTES);
            assert_eq!(
                symbols.as_slice(),
                &[0x05, 0x04, 6],
                "only the symbols not yet consumed are readable"
            );
            assert!(writer.put(0xee));
            assert_eq!(writer.offset(), 2);
        }

        assert!(buffer.set_pending(2));
        assert_eq!(buffer.written(), &[0xff, 0xee]);
        assert_eq!(
            buffer.symbol_at(SYMBOL_BYTES),
            Some((0x0405, 6)),
            "the unread symbol survived the write"
        );

        assert!(
            buffer
                .split_at_symbol_cursor(buffer.sym_next() + SYMBOL_BYTES)
                .is_none(),
            "cannot consume more symbols than were written"
        );
        assert!(buffer.split_at_symbol_cursor(0).is_some());

        // Two shared views of one allocation need no split at all.
        let (flushable, symbols) = buffer.flushable_and_symbols();
        assert_eq!(flushable, &[0xff, 0xee]);
        assert_eq!(symbols, &[0x02, 0x01, 3, 0x05, 0x04, 6]);

        // Once pending has passed the split point the C assertion would fire; refuse instead.
        let mut overrun = garbage_pending();
        let filler = [0u8; PENDING_SIZE];
        assert_eq!(overrun.push_symbol(1, 2), Some(false));
        assert_eq!(overrun.append(&filler[..=LIT_BUFSIZE]), LIT_BUFSIZE + 1);
        assert!(overrun.split_at_symbol_cursor(0).is_none());
        assert!(
            overrun.split_at_symbol_cursor(SYMBOL_BYTES).is_some(),
            "consuming the symbol moves the split point past pending"
        );
    }

    #[test]
    fn pending_buf_state_can_be_restored_for_deflate_copy() {
        // deflateCopy reproduces pending, pending_out and sym_next -- deflate.c L1359-L1368.
        let mut buffer = garbage_pending();
        assert_eq!(buffer.append(&[1, 2, 3, 4, 5]), 5);
        assert!(buffer.consume_flushed(2));
        assert_eq!(buffer.push_symbol(7, 8), Some(false));

        let mut copy = garbage_pending();
        assert!(copy.set_pending(3));
        assert!(copy.set_pending_out_offset(2));
        assert!(copy.set_sym_next(SYMBOL_BYTES));
        assert_eq!(copy.pending(), 3);
        assert_eq!(copy.pending_out_offset(), 2);
        assert_eq!(copy.sym_next(), SYMBOL_BYTES);

        copy.symbols_mut().copy_from_slice(buffer.symbols());
        assert_eq!(copy.symbol_at(0), Some((7, 8)));
        assert_eq!(
            copy.flushable().len(),
            buffer.flushable().len(),
            "the restored cursors describe the same flush window"
        );

        assert!(!copy.set_pending(copy.pending_buf_size()));
        assert!(!copy.set_pending_out_offset(copy.pending_buf_size()));

        let mut cursor = copy.symbol_cursor();
        assert_eq!(cursor.remaining(), SYMBOL_BYTES);
        assert_eq!(cursor.take_next(), Some(&7));
        assert_eq!(cursor.take_next(), Some(&0));
        assert_eq!(cursor.take_next(), Some(&8));
        assert_eq!(cursor.take_next(), None);
    }
}
