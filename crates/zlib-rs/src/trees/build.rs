//! Huffman tree construction, tree transmission and block-body emission --
//! the Rust counterpart of the algorithmic half of `trees.c`.
//!
//! An unqualified line reference in this file is a line of `trees.c`.
//!
//! # What lives here, and where it came from
//!
//! | C function | L | Here |
//! |---|---|---|
//! | `init_block` | 440-451 | [`init_block`] |
//! | `gen_codes` | 203-232 | [`gen_codes`] |
//! | `smaller` (macro) | 499-501 | [`HeapView::smaller`] |
//! | `pqremove` (macro) | 488-493 | [`pqremove`] |
//! | `pqdownheap` | 509-528 | [`pqdownheap`] |
//! | `gen_bitlen` | 540-613 | [`gen_bitlen`] |
//! | `build_tree` | 627-706 | [`build_tree`] |
//! | `scan_tree` | 712-747 | [`scan_tree`] |
//! | `send_tree` | 753-794 | [`send_tree`] |
//! | `build_bl_tree` | 800-826 | [`build_bl_tree`] |
//! | `send_all_trees` | 833-855 | [`send_all_trees`] |
//! | `compress_block` | 900-951 | [`compress_block`] |
//! | `detect_data_type` | 966-991 | [`detect_data_type`] |
//! | `_tr_flush_block`'s block choice | 1027-1074 | [`select_block_type`] |
//! | `d_code` (macro, `deflate.h`) | 320-321 | [`d_code`] |
//!
//! `_tr_init`, `_tr_tally`, `_tr_flush_block`, `_tr_flush_bits`, `_tr_align`
//! and `_tr_stored_block` are the internal API `deflate.h` L311-L317 declares;
//! they orchestrate the helpers above and belong to `trees/mod.rs`. Bit
//! emission -- `send_bits`, `send_code`, `bi_reverse`, `bi_flush`, `bi_windup`
//! -- belongs to `trees/bit_writer.rs` and is used from here, never reproduced.
//!
//! # Why this file is transliterated rather than rewritten
//!
//! RFC 1951 constrains the *format* of a deflate stream, not the *choices* an
//! encoder makes within it. Which of several equally short codes a symbol gets,
//! how an equal-frequency tie is broken, and which of the three block types is
//! cheapest are all left open by the specification, and this implementation must make the
//! same choices as the reference for the same input, byte for byte. A tidier or
//! measurably better Huffman coder would still be wrong here. Every loop below
//! therefore keeps the reference's shape, including the two `do`-`while` loops,
//! the `continue` at L604 that deliberately skips a decrement, and the nested
//! assignment at L656.
//!
//! Three of the encoder's decisions are made in this file and nowhere else:
//!
//! * **The `smaller` tie-break** ([`HeapView::smaller`], L499-L501) compares depth with
//!   `<=`, not `<`. Reverse that and equal-frequency symbols swap places in the
//!   heap, the code lengths change, and every emitted block changes with them.
//! * **The forced-two-codes repair** ([`build_tree`], L655-L661) invents up to
//!   two symbols so that a degenerate alphabet still has two codes, because "the
//!   pkzip format requires that at least one distance code exists, and that at
//!   least one bit should be sent even if there is only one possible code"
//!   (L650-L653).
//! * **The block-type comparison** ([`select_block_type`], L1027-L1074) is
//!   integer arithmetic with a truncating `>> 3`, and its three predicates are
//!   evaluated in a fixed order.
//!
//! # Wrapping arithmetic on `opt_len` and `static_len`
//!
//! `DeflateState::opt_len` and `DeflateState::static_len` are C `ulg`, an
//! *unsigned* type, and the reference subtracts from both while they are zero.
//! L659 is
//!
//! ```c
//! s->opt_len--; if (stree) s->static_len -= stree[node].Len;
//! ```
//!
//! and it runs before any `gen_bitlen` has added anything, so both subtractions
//! wrap around. That is deliberate: the decrement pre-compensates for the
//! artificial node `gen_bitlen` is about to count, and the wrap cancels exactly
//! when it does. L607 wraps the other way, adding a difference that is negative
//! whenever a code is being shortened.
//!
//! For the literal tree of an empty block the repair leaves `opt_len` at the maximum
//! value of its type and `static_len` at that maximum minus 7; `gen_bitlen` then adds
//! back exactly enough to bring them to 1 and 7, which `select_block_type` turns into
//! an `opt_lenb` of 1 and a `static_lenb` of 2. The intermediate values are whatever
//! the field's width makes them, which is precisely why nothing may depend on them.
//!
//! Every arithmetic operation on these two fields therefore uses
//! `wrapping_add`, `wrapping_sub` or `wrapping_mul`. A plain `-=` would abort a
//! debug build, and both `cargo test` and Miri are debug builds -- so this is
//! not a theoretical concern but the difference between a working implementation and one
//! that cannot be tested at all. Because the wrap cancels, the final value does
//! not depend on whether C's `ulg` is 32 or 64 bits wide, which is why the implementation
//! is correct on both LP64 and LLP64 targets.
//!
//! This path is not exotic. `init_block` always sets
//! `dyn_ltree[END_BLOCK].Freq = 1` (L448), so a block with no symbols in it has
//! exactly one non-zero frequency and takes the repair; finishing an empty
//! stream does precisely that.
//!
//! # Reading and writing tree entries
//!
//! C reaches a tree through `desc->dyn_tree`, a pointer into the very
//! `deflate_state` that also holds the heap, the depths and the two length
//! accumulators. Holding that as a Rust borrow would lock the whole state, so
//! each access here goes through a small helper that borrows
//! `DeflateState::tree_for` or `DeflateState::tree_for_mut` for the duration of
//! one read or one write and copies the four-byte entry in or out. The tree is
//! named by a [`StaticTreeKind`] rather than pointed at, exactly as
//! `DeflateState::tree_for` documents, so `desc` collapses into that one
//! argument.
//!
//! Every such helper is bounds-checked and returns a default rather than
//! panicking, so no index in this file can abort the process. Where the index is
//! provably in range the comment says why.
//!
//! # Compile-time configuration
//!
//! Only the default configuration is implemented: `LIT_MEM` is commented out at
//! `deflate.h` L28, so the symbol buffer is one array of three bytes per symbol
//! (L913-L915) and never the `d_buf`/`l_buf` pair; `FORCE_STATIC` (L1034) and
//! `FORCE_STORED` (L1044) are undefined, so both selection predicates are
//! present; `GEN_TREES_H` (L35) is undefined, so the static tables are the
//! transcribed constants in `trees/static_tables.rs`; and `ZLIB_DEBUG` is
//! undefined, so there is no `compressed_len` or `bits_sent` accounting and the
//! reference's `Assert`s appear only as `debug_assert!`.

use crate::config::{Strategy, Z_BINARY, Z_TEXT};
use crate::deflate::state::{
    Allocator, CtData, DeflateState, StaticTreeKind, BL_CODES, DYN_TREES, D_CODES, HEAP_SIZE,
    LITERALS, L_CODES, MAX_BITS, STATIC_TREES, STORED_BLOCK, SYMBOL_BYTES,
};
use crate::trees::bit_writer::{bi_reverse, send_bits, send_code};
use crate::trees::static_tables::{
    _dist_code, _length_code, base_dist, base_length, bl_order, extra_dbits, extra_lbits,
    static_dtree, static_ltree, END_BLOCK, REPZ_11_138, REPZ_3_10, REP_3_6,
};
use crate::trees::tree_desc::StaticTreeDesc;

/// Index within the heap array of the least frequent node.
///
/// Mirrors `#define SMALLEST 1` (L480-L481). `heap[0]` is never used, so the
/// heap's children relation is the textbook `2k` and `2k + 1` without an offset.
const SMALLEST: i32 = 1;

// C moves freely between `int` node numbers, `ush` tree fields and array
// indices. Each direction is written once here as a checked conversion with an
// unreachable fallback, so that no conversion in the body of an algorithm needs
// a cast, and so that a value that somehow left its documented range degrades to
// a harmless default instead of aborting.

/// A C `int` as an array index -- C's implicit `int` to index conversion.
///
/// Every value converted here is a node number or a heap position in
/// `0..HEAP_SIZE`, so the saturation is unreachable. It saturates to
/// [`usize::MAX`] rather than 0 deliberately: an out-of-range value then fails
/// every `get`, which reads as a default and writes as a no-op, instead of
/// silently aliasing element 0.
#[inline]
fn as_index(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// An array index as a C `int` -- the inverse of [`as_index`].
///
/// Saturates to [`i32::MAX`], which is not a node number, for the same reason.
/// Unreachable: every index converted here is at most [`HEAP_SIZE`].
#[inline]
fn as_int(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX)
}

/// A value narrowed to `ush`, as the reference's `(ush)` casts do.
///
/// Every value narrowed here is a code, a code length, a repeat count, a
/// frequency, a residue or a node number, all of which fit in `u16`; the
/// fallback is unreachable. C would truncate instead, which for in-range values
/// is the same thing. One generic helper rather than one per source width, so
/// that no call site needs a cast that `clippy::pedantic` would have to be told
/// about.
#[inline]
fn as_ush<T: TryInto<u16>>(value: T) -> u16 {
    value.try_into().unwrap_or(0)
}

/// A value widened to C `ulg`, the type of both length accumulators.
///
/// Infallible for every value this file produces.
#[inline]
fn as_ulg<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().unwrap_or(0)
}

/// One entry of the tree `kind` names, or a zero entry when `node` is out of
/// range.
///
/// This is `desc->dyn_tree[node]` (`deflate.h` L91) as a copy. [`CtData`] is
/// four bytes and [`Copy`], so copying costs nothing and leaves no borrow of the
/// state outstanding -- which is what lets the caller write to the heap, the
/// depths or the accumulators in the same expression.
#[inline]
fn entry_at<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
) -> CtData {
    state
        .tree_for(kind)
        .get(as_index(node))
        .copied()
        .unwrap_or_default()
}

/// Replaces one entry of the tree `kind` names; a no-op when `node` is out of
/// range.
#[inline]
fn set_entry_at<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
    entry: CtData,
) {
    if let Some(slot) = state.tree_for_mut(kind).get_mut(as_index(node)) {
        *slot = entry;
    }
}

/// `tree[node].Freq` (`deflate.h` L83).
#[inline]
fn freq_at<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
) -> u16 {
    entry_at(state, kind, node).freq()
}

/// `tree[node].Freq = freq`.
#[inline]
fn set_freq_at<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
    freq: u16,
) {
    let mut entry = entry_at(state, kind, node);
    entry.set_freq(freq);
    set_entry_at(state, kind, node, entry);
}

/// `tree[node].Len` (`deflate.h` L86).
#[inline]
fn len_at<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
) -> u16 {
    entry_at(state, kind, node).len()
}

/// `tree[node].Len = len`.
///
/// Shares storage with `Dad`, which is why `gen_bitlen` can note that it
/// overwrites `tree[n].Dad`, "which is no longer needed" (L566).
#[inline]
fn set_len_at<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
    len: u16,
) {
    let mut entry = entry_at(state, kind, node);
    entry.set_len(len);
    set_entry_at(state, kind, node, entry);
}

/// `tree[node].Dad` (`deflate.h` L85).
#[inline]
fn dad_at<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
) -> u16 {
    entry_at(state, kind, node).dad()
}

/// `tree[node].Dad = dad`.
#[inline]
fn set_dad_at<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
    dad: u16,
) {
    let mut entry = entry_at(state, kind, node);
    entry.set_dad(dad);
    set_entry_at(state, kind, node, entry);
}

/// `tree[node].Code = code`, the one write `gen_codes` makes (L227).
///
/// Shares storage with `Freq`, so the frequency of `node` is gone after this.
/// That is intended and harmless: the frequencies of a block are dead once its
/// codes exist, and `init_block` reinstates them for the next block.
#[inline]
fn set_code_at<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    node: i32,
    code: u16,
) {
    let mut entry = entry_at(state, kind, node);
    entry.set_code(code);
    set_entry_at(state, kind, node, entry);
}

/// `s->heap[position]`, or 0 when `position` is out of range.
#[inline]
fn heap_at<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>, position: i32) -> i32 {
    state
        .heap
        .get(as_index(position))
        .copied()
        .unwrap_or_default()
}

/// `s->heap[position] = node`; a no-op when `position` is out of range.
#[inline]
fn set_heap_at<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, position: i32, node: i32) {
    if let Some(slot) = state.heap.get_mut(as_index(position)) {
        *slot = node;
    }
}

/// `s->depth[node]`, or 0 when `node` is out of range.
#[inline]
fn depth_at<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>, node: i32) -> u8 {
    state.depth.get(as_index(node)).copied().unwrap_or_default()
}

/// `s->depth[node] = depth`; a no-op when `node` is out of range.
#[inline]
fn set_depth_at<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, node: i32, depth: u8) {
    if let Some(slot) = state.depth.get_mut(as_index(node)) {
        *slot = depth;
    }
}

/// `s->bl_count[bits]`, or 0 when `bits` is out of range.
#[inline]
fn bl_count_at<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>, bits: usize) -> u16 {
    state.bl_count.get(bits).copied().unwrap_or_default()
}

/// `s->bl_count[bits] = count`; a no-op when `bits` is out of range.
#[inline]
fn set_bl_count_at<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, bits: usize, count: u16) {
    if let Some(slot) = state.bl_count.get_mut(bits) {
        *slot = count;
    }
}

/// Initialises a new block: clears every frequency, then reinstates the single
/// frequency a block always has.
///
/// Mirrors `init_block` (L440-L451).
///
/// Called from `_tr_init` for the first block of a stream (L477) and from
/// `_tr_flush_block` for each block after one is emitted (L1079); both callers
/// live in `trees/mod.rs`, which is why this is defined once here.
///
/// # Two details that carry weight
///
/// The three loops stop at the *alphabet* size, not at the array size, because
/// the entries above the alphabet are scratch space for the internal nodes
/// `build_tree` creates (L672) and are written before they are read.
///
/// `dyn_ltree[END_BLOCK].Freq = 1` (L448) accounts for the end-of-block symbol
/// every block ends with (L950). It is also the reason a block with no symbols
/// of its own still has one non-zero frequency and so takes `build_tree`'s
/// forced-two-codes repair -- see the module documentation.
///
/// Nothing here relies on the state's buffers having been zeroed: every field
/// this function reads later, it writes now.
pub(crate) fn init_block<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // The three frequency resets. `iter_mut().take(..)` rather than an index
    // loop: the bound is the alphabet size in both spellings, and this one
    // cannot index out of range.
    for entry in state.dyn_ltree.iter_mut().take(L_CODES) {
        entry.set_freq(0);
    }
    for entry in state.dyn_dtree.iter_mut().take(D_CODES) {
        entry.set_freq(0);
    }
    for entry in state.bl_tree.iter_mut().take(BL_CODES) {
        entry.set_freq(0);
    }

    // `s->dyn_ltree[END_BLOCK].Freq = 1;` -- END_BLOCK is 256 and `dyn_ltree`
    // has HEAP_SIZE (573) entries, so the slot always exists.
    if let Some(entry) = state.dyn_ltree.get_mut(END_BLOCK) {
        entry.set_freq(1);
    }

    // `s->opt_len = s->static_len = 0L;`
    state.opt_len = 0;
    state.static_len = 0;

    // `s->sym_next = s->matches = 0;`
    state.pending.reset_symbols();
    state.matches = 0;
}

/// Generates the bit string of every symbol from the bit-length counts.
///
/// Mirrors `gen_codes` (L203-L232). C takes the tree, `max_code` and
/// `s->bl_count`; here the tree is named by `kind` and the counts are read from
/// the state, so the three arguments collapse to two.
///
/// IN assertion: `bl_count` holds the bit-length statistics of this tree and
/// `Len` is set for every element (L198-L199). OUT assertion: `Code` is set for
/// every element of non-zero code length (L200-L201).
///
/// # The two passes
///
/// The first pass turns the length histogram into the first code of each length,
/// which is the canonical construction RFC 1951 3.2.2 describes:
/// `code = (code + bl_count[bits - 1]) << 1`. The second walks the alphabet in
/// order and hands out consecutive codes of the right length, reversing each one
/// because deflate transmits Huffman codes most-significant bit first inside a
/// least-significant-bit-first bit stream (L226 and `doc/rfc1951.txt` 3.1.1).
///
/// `next_code[len]` is *post*-incremented (L227), so each symbol takes the
/// current value and the next symbol of the same length takes the one after.
///
/// Writing `Code` destroys `Freq`; see [`set_code_at`].
pub(crate) fn gen_codes<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    max_code: i32,
) {
    // `ush next_code[MAX_BITS+1];` -- the next code value for each bit length.
    let mut next_code = [0u16; MAX_BITS + 1];
    // `unsigned code = 0;` -- the running code value. Kept 32 bits wide, as C
    // does, and narrowed only on the store below.
    let mut code: u32 = 0;

    // `for (bits = 1; bits <= MAX_BITS; bits++)`
    for bits in 1..=MAX_BITS {
        // `code = (code + bl_count[bits - 1]) << 1;` -- `bits >= 1`, so the
        // predecessor index cannot underflow.
        code = (code + u32::from(bl_count_at(state, bits - 1))) << 1;
        // `next_code[bits] = (ush)code;`
        if let Some(slot) = next_code.get_mut(bits) {
            *slot = as_ush(code);
        }
    }

    // `Assert (code + bl_count[MAX_BITS] - 1 == (1 << MAX_BITS) - 1, "inconsistent bit counts");`
    // (L219-L220), which is compiled only under ZLIB_DEBUG. Expressed with the
    // same wrapping the C expression has, so that an inconsistent histogram
    // reports rather than aborting a release build.
    debug_assert_eq!(
        code.wrapping_add(u32::from(bl_count_at(state, MAX_BITS)))
            .wrapping_sub(1),
        (1u32 << MAX_BITS) - 1,
        "inconsistent bit counts (trees.c L219)"
    );

    // `for (n = 0; n <= max_code; n++)`. A negative `max_code` means an empty
    // loop, which is what C's comparison does too.
    for node in 0..=max_code {
        // `int len = tree[n].Len; if (len == 0) continue;`
        let len = len_at(state, kind, node);
        if len == 0 {
            continue;
        }
        // `tree[n].Code = (ush)bi_reverse(next_code[len]++, len);`
        let index = usize::from(len);
        let next = next_code.get(index).copied().unwrap_or_default();
        if let Some(slot) = next_code.get_mut(index) {
            *slot = slot.wrapping_add(1);
        }
        set_code_at(state, kind, node, bi_reverse(next, len));
    }
}

/// The three arrays the heap algorithms touch, borrowed once.
///
/// C's `pqdownheap(s, tree, k)` receives `tree` as a `ct_data *` and then indexes
/// `tree[...]`, `s->heap[...]` and `s->depth[...]` directly. The accessor stack
/// above reproduces each of those as `state.tree_for(kind)` -- a three-way match
/// on the kind -- followed by a bounds-checked `get`, which is correct but pays
/// the kind selection again for every one of the four array reads that a single
/// `smaller` comparison performs, and `pqdownheap` performs up to two comparisons
/// per level.
///
/// This view resolves the kind **once**, when it is built, and then hands the
/// descent three plain slices. It is the "specialised tree view with one-time
/// validation" the review asks for, and it changes nothing about the algorithm:
/// [`HeapView::smaller`] is the `smaller` macro transcribed unchanged, including
/// the `<=` depth tie-breaker that decides which pair `build_tree` combines
/// first, and [`HeapView::pqdownheap`] visits the same nodes in the same order as
/// L509-L528.
///
/// The three borrows are disjoint fields of one [`DeflateState`], which is why
/// [`HeapView::of`] destructures the state rather than calling its accessors: a
/// method returning `&mut self.heap` and another returning `&self.dyn_ltree`
/// could not both be live, whereas one destructuring gives independent borrows of
/// each field.
#[derive(Debug)]
struct HeapView<'v> {
    /// `s->heap` (`deflate.h` L262-L263). Written by the descent, hence `&mut`.
    heap: &'v mut [i32],
    /// `s->depth` (`deflate.h` L265-L268). Read only.
    depth: &'v [u8],
    /// `desc->dyn_tree` (`deflate.h` L91), already selected. Read only: the heap
    /// algorithms never write a tree entry.
    tree: &'v [CtData],
    /// `s->heap_len` (`deflate.h` L264), copied because the descent only reads it.
    heap_len: i32,
}

impl<'v> HeapView<'v> {
    /// Borrows `state`'s heap, depths and the tree that `kind` names.
    fn of<'a, A: Allocator<'a>>(state: &'v mut DeflateState<'a, A>, kind: StaticTreeKind) -> Self {
        // Destructuring is what makes the three borrows below independent; `..`
        // covers every field the heap algorithms do not touch.
        let DeflateState {
            heap,
            heap_len,
            depth,
            dyn_ltree,
            dyn_dtree,
            bl_tree,
            ..
        } = state;

        // The one and only resolution of the tree kind. Same pairing as
        // `DeflateState::tree_for`, which `_tr_init` establishes at
        // `trees.c` L459-L466.
        let tree: &[CtData] = match kind {
            StaticTreeKind::Literal => dyn_ltree,
            StaticTreeKind::Distance => dyn_dtree,
            StaticTreeKind::BitLength => bl_tree,
        };

        Self {
            heap,
            depth,
            tree,
            heap_len: *heap_len,
        }
    }

    /// `s->heap[position]`, or 0 when `position` is out of range.
    #[inline]
    fn heap_at(&self, position: i32) -> i32 {
        self.heap
            .get(as_index(position))
            .copied()
            .unwrap_or_default()
    }

    /// `s->heap[position] = node`; a no-op when `position` is out of range.
    #[inline]
    fn set_heap_at(&mut self, position: i32, node: i32) {
        if let Some(slot) = self.heap.get_mut(as_index(position)) {
            *slot = node;
        }
    }

    /// `tree[node].Freq` (`deflate.h` L83), or 0 when `node` is out of range.
    #[inline]
    fn freq_at(&self, node: i32) -> u16 {
        self.tree
            .get(as_index(node))
            .copied()
            .unwrap_or_default()
            .freq()
    }

    /// `s->depth[node]`, or 0 when `node` is out of range.
    #[inline]
    fn depth_at(&self, node: i32) -> u8 {
        self.depth.get(as_index(node)).copied().unwrap_or_default()
    }

    /// Compares two subtrees, using tree depth as the tie-breaker when the two
    /// have equal frequency.
    ///
    /// Port of the `smaller` macro (L499-L501):
    ///
    /// ```c
    /// #define smaller(tree, n, m, depth) \
    ///    (tree[n].Freq < tree[m].Freq || \
    ///    (tree[n].Freq == tree[m].Freq && depth[n] <= depth[m]))
    /// ```
    ///
    /// `n` and `m` are node numbers, not heap positions -- every call site passes
    /// `heap[...]` or a value taken from it.
    ///
    /// # Why the `<=` matters
    ///
    /// Preferring the *shallower* subtree on a tie is what "minimizes the worst
    /// case length" (L496-L497), and the comparison is not symmetric: with `<`
    /// instead of `<=` two equally frequent, equally deep symbols would compare
    /// as neither smaller than the other, [`HeapView::pqdownheap`] would stop one
    /// level earlier, and the heap would pop them in the opposite order. That
    /// changes which pair `build_tree` combines first, which changes their code
    /// lengths, which changes every byte the encoder emits for that block. This
    /// is one of the encoder decisions RFC 1951 leaves open, so matching the
    /// reference here is the whole requirement -- see the module documentation.
    ///
    /// The two-term short-circuit shape is kept: the depth comparison is reached
    /// only for equal frequencies.
    ///
    /// # Why it is a method on the view
    ///
    /// This is the only comparison the heap algorithms make, and each call reads
    /// four array elements -- two frequencies and two depths. Reaching them
    /// through `DeflateState::tree_for` would re-resolve the tree kind for every
    /// one of those reads, four times per comparison and up to eight times per
    /// level of the descent. On the view they are three plain slices, resolved
    /// once. See [`HeapView`].
    #[inline]
    fn smaller(&self, n: i32, m: i32) -> bool {
        let freq_n = self.freq_at(n);
        let freq_m = self.freq_at(m);

        freq_n < freq_m || (freq_n == freq_m && self.depth_at(n) <= self.depth_at(m))
    }

    /// The body of [`pqdownheap`] (L509-L528).
    fn pqdownheap(&mut self, k: i32) {
        // `int v = s->heap[k];`
        let v = self.heap_at(k);
        let mut k = k;
        // `int j = k << 1;` -- the left son of k.
        let mut j = k << 1;

        // `while (j <= s->heap_len)`
        while j <= self.heap_len {
            // `if (j < s->heap_len && smaller(tree, s->heap[j + 1], s->heap[j], s->depth)) j++;`
            // -- set j to the smaller of the two sons.
            if j < self.heap_len && self.smaller(self.heap_at(j + 1), self.heap_at(j)) {
                j += 1;
            }

            // `if (smaller(tree, v, s->heap[j], s->depth)) break;` -- v is smaller
            // than both sons, so the heap property holds from here down.
            if self.smaller(v, self.heap_at(j)) {
                break;
            }

            // `s->heap[k] = s->heap[j];  k = j;` -- exchange v with the smaller son.
            let son = self.heap_at(j);
            self.set_heap_at(k, son);
            k = j;

            // `j <<= 1;` -- continue down the tree.
            j <<= 1;
        }

        // `s->heap[k] = v;`
        self.set_heap_at(k, v);
    }
}

/// Restores the heap property by moving node `k` down.
///
/// Mirrors `pqdownheap` (L509-L528): exchange a node with the smaller of its two
/// sons until each father is smaller than both of its sons.
///
/// # Why no index can go out of range
///
/// `j` is only read after the loop condition has established `j <= heap_len`,
/// and `heap[j + 1]` is read only under the extra guard `j < heap_len`, so the
/// highest index touched is `heap_len`. `heap_len` never exceeds `HEAP_SIZE - 1`:
/// `build_tree` grows it by one per non-zero frequency over an alphabet of at
/// most `L_CODES` symbols (L641-L648) and by at most two more in the repair
/// (L655-L661), and `heap` has `HEAP_SIZE` entries. The accessors are still
/// bounds-checked -- a wrong index would read a default rather than abort -- but
/// the argument above is why that fallback is unreachable.
///
/// # The view
///
/// The descent itself lives in [`HeapView::pqdownheap`]. This wrapper exists to
/// resolve `kind` into a tree slice **once** per call, rather than once per array
/// read inside every comparison; see [`HeapView`]. The signature is unchanged, so
/// `pqremove` and `build_tree` call it exactly as before.
pub(crate) fn pqdownheap<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    k: i32,
) {
    HeapView::of(state, kind).pqdownheap(k);
}

/// Removes the smallest element from the heap and restores the heap property.
///
/// Mirrors the `pqremove` macro (L488-L493):
///
/// ```c
/// top = s->heap[SMALLEST];
/// s->heap[SMALLEST] = s->heap[s->heap_len--];
/// pqdownheap(s, tree, SMALLEST);
/// ```
///
/// The macro assigns its result through an out-parameter; here it is returned.
/// C's `s->heap_len--` is a *post*-decrement, so the value moved into
/// `heap[SMALLEST]` is the one at the old `heap_len`; the two statements below
/// keep that order.
///
/// `heap_len` is at least 2 at every call site -- `build_tree`'s combine loop
/// runs while `heap_len >= 2` (L695) and the repair at L655-L661 guarantees the
/// first iteration -- so the decrement cannot take it negative.
fn pqremove<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, kind: StaticTreeKind) -> i32 {
    // One [`HeapView`] for the whole macro, rather than three trips through the
    // free-standing accessors plus a fourth inside `pqdownheap`: this is the
    // hottest of the heap routines, because `build_tree`'s combine loop calls it
    // twice per internal node.
    let mut view = HeapView::of(state, kind);

    // `top = s->heap[SMALLEST];`
    let top = view.heap_at(SMALLEST);

    // `s->heap[SMALLEST] = s->heap[s->heap_len--];` -- a *post*-decrement, so the
    // value moved down is the one at the old `heap_len`.
    let last = view.heap_at(view.heap_len);
    view.set_heap_at(SMALLEST, last);
    view.heap_len -= 1;

    // `pqdownheap(s, tree, SMALLEST);`
    view.pqdownheap(SMALLEST);

    // The view holds `heap_len` by value, so that the descent reads it from a
    // register rather than through a borrow; the decrement above therefore has to
    // be published back to the state before the borrow ends.
    let heap_len = view.heap_len;
    state.heap_len = heap_len;

    top
}

/// Computes the optimal bit length of every code in a tree and adds the block's
/// bit cost to both length accumulators.
///
/// Mirrors `gen_bitlen` (L540-L613).
///
/// IN assertion: `Freq` and `Dad` are set for every element, and
/// `heap[heap_max]` upwards are the tree nodes sorted by increasing frequency
/// (L533-L534). OUT assertions: `Len` is set to the optimal bit length,
/// `bl_count` holds the frequency of each bit length, `opt_len` is updated, and
/// `static_len` is updated too when this tree has a static counterpart
/// (L535-L538).
///
/// # Three passes
///
/// The first pass (L561-L576) walks the sorted nodes from the root outwards and
/// gives each node its father's length plus one, clamping at the alphabet's
/// `max_length` and counting each clamp as an overflow. It writes `Len` over
/// `Dad` -- the reference says so explicitly at L566 -- which is sound because
/// the two share storage and a node's father is not needed once its depth is
/// known. Only leaves, meaning nodes at or below `max_code`, contribute to the
/// histogram and to the accumulators.
///
/// If nothing overflowed, that is the whole function (L577). Otherwise the second
/// pass (L583-L593) moves leaves down one level at a time until every code fits,
/// and the third (L600-L612) rebuilds every length from the repaired histogram
/// rather than patching the wrong ones, "an idea ... taken from 'ar' written by
/// Haruhiko Okumura" (L597-L598). Overflow "happens for example on obj2 and pic
/// of the Calgary corpus" (L580) and always for the bit-length tree, whose
/// `max_length` is only 7.
///
/// # Deliberate non-idioms
///
/// The second pass is a `do`-`while`: its body runs once before `overflow` is
/// tested, which is correct because it is entered only when `overflow > 0`.
///
/// In the third pass, `if (m > max_code) continue;` (L604) skips the `n--` at
/// L610 as well as the body. That is not a bug to be repaired: an internal node
/// consumes a heap slot without consuming one of the `bl_count[bits]` leaves the
/// pass is placing. The loop still terminates because `m = s->heap[--h]`
/// decrements `h` on every iteration, taken branch or not.
///
/// Both accumulator updates use wrapping arithmetic -- see the module
/// documentation. The L607 update in particular adds `bits - tree[m].Len`, which
/// is negative whenever a code is being shortened.
pub(crate) fn gen_bitlen<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
) {
    // The five members C reads out of `desc` and `desc->stat_desc` up front.
    let desc = StaticTreeDesc::for_kind(kind);
    let max_code = state.desc_for(kind).max_code;
    let stree = desc.static_tree;
    let extra = desc.extra_bits;
    let base = desc.extra_base;
    let max_length = desc.max_length;
    // `int overflow = 0;` -- number of elements with a bit length that is too
    // large.
    let mut overflow: i32 = 0;

    // `for (bits = 0; bits <= MAX_BITS; bits++) s->bl_count[bits] = 0;` -- the
    // array is exactly `MAX_BITS + 1` long, so clearing all of it is the same
    // range the reference writes.
    for count in &mut state.bl_count {
        *count = 0;
    }

    // `tree[s->heap[s->heap_max]].Len = 0;` -- the root of the heap, whose
    // length seeds the first pass.
    let root = heap_at(state, state.heap_max);
    set_len_at(state, kind, root, 0);

    // `for (h = s->heap_max + 1; h < HEAP_SIZE; h++)`. `heap_max` is in
    // `0..=HEAP_SIZE` after `build_tree`, so the conversion cannot fail; a value
    // of `HEAP_SIZE` yields an empty range, exactly as C's comparison would.
    let heap_max = as_index(state.heap_max);
    for position in heap_max.saturating_add(1)..HEAP_SIZE {
        // `n = s->heap[h];`
        let node = heap_at(state, as_int(position));

        // `bits = tree[tree[n].Dad].Len + 1;`
        let father = i32::from(dad_at(state, kind, node));
        let mut bits = usize::from(len_at(state, kind, father)).saturating_add(1);

        // `if (bits > max_length) bits = max_length, overflow++;`
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }

        // `tree[n].Len = (ush)bits;` -- overwrites `Dad`, which is dead now.
        set_len_at(state, kind, node, as_ush(bits));

        // `if (n > max_code) continue;` -- an internal node, not a leaf: it has
        // a length so that its sons can be measured, but it is neither counted
        // nor transmitted.
        if node > max_code {
            continue;
        }

        // `s->bl_count[bits]++;`
        set_bl_count_at(state, bits, bl_count_at(state, bits).wrapping_add(1));

        // `xbits = 0; if (n >= base) xbits = extra[n - base];`
        let index = as_index(node);
        let mut xbits: usize = 0;
        if index >= base {
            xbits = as_index(extra.get(index - base).copied().unwrap_or(0));
        }

        // `f = tree[n].Freq;`
        let freq = u64::from(freq_at(state, kind, node));

        // `s->opt_len += (ulg)f * (unsigned)(bits + xbits);`
        state.opt_len = state
            .opt_len
            .wrapping_add(freq.wrapping_mul(as_ulg(bits.saturating_add(xbits))));

        // `if (stree) s->static_len += (ulg)f * (unsigned)(stree[n].Len + xbits);`
        if let Some(stree) = stree {
            let static_bits = usize::from(stree.get(index).copied().map_or(0, CtData::len));
            state.static_len = state
                .static_len
                .wrapping_add(freq.wrapping_mul(as_ulg(static_bits.saturating_add(xbits))));
        }
    }

    // `if (overflow == 0) return;`
    if overflow == 0 {
        return;
    }

    // `do { ... } while (overflow > 0);` -- find the first bit length that could
    // increase, move one leaf down and one overflow item up as its brother.
    loop {
        // `bits = max_length - 1; while (s->bl_count[bits] == 0) bits--;`
        // `max_length` is at least 7, so the initial value is positive. The
        // `bits > 0` guard has no reachable effect -- `overflow > 0` implies some
        // shorter length is populated -- and exists only so that the search
        // cannot run off the front of the histogram the way the C expression can.
        let mut bits = max_length.saturating_sub(1);
        while bits > 0 && bl_count_at(state, bits) == 0 {
            bits -= 1;
        }

        // `s->bl_count[bits]--;` -- move one leaf down the tree.
        set_bl_count_at(state, bits, bl_count_at(state, bits).wrapping_sub(1));
        // `s->bl_count[bits + 1] += 2;` -- move one overflow item as its brother.
        let next = bits.saturating_add(1);
        set_bl_count_at(state, next, bl_count_at(state, next).wrapping_add(2));
        // `s->bl_count[max_length]--;` -- "The brother of the overflow item also
        // moves one step up, but this does not affect bl_count[max_length]"
        // (L589-L591).
        set_bl_count_at(
            state,
            max_length,
            bl_count_at(state, max_length).wrapping_sub(1),
        );

        // `overflow -= 2;` and the loop test.
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }

    // `for (bits = max_length; bits != 0; bits--)`. C reuses `h`, which "is still
    // equal to HEAP_SIZE" (L596) because the first pass ran to completion.
    let mut position = HEAP_SIZE;
    for bits in (1..=max_length).rev() {
        // `n = s->bl_count[bits];`
        let mut remaining = i32::from(bl_count_at(state, bits));

        // `while (n != 0)`
        while remaining != 0 {
            // Unreachable: the heap region below `HEAP_SIZE` holds one entry per
            // node of this tree, and `bl_count` sums to the number of leaves, so
            // the walk stops well above zero. Present so that the decrement
            // cannot run off the front of the heap.
            if position == 0 {
                break;
            }

            // `m = s->heap[--h];`
            position -= 1;
            let node = heap_at(state, as_int(position));

            // `if (m > max_code) continue;` -- and note that this skips the
            // `n--` below, deliberately.
            if node > max_code {
                continue;
            }

            // `if ((unsigned) tree[m].Len != (unsigned) bits) { ... }`
            let current = len_at(state, kind, node);
            if usize::from(current) != bits {
                // `s->opt_len += ((ulg)bits - tree[m].Len) * tree[m].Freq;` --
                // the subtraction is negative whenever the code is shortening,
                // and C's unsigned arithmetic wraps through it.
                let freq = u64::from(freq_at(state, kind, node));
                state.opt_len = state.opt_len.wrapping_add(
                    as_ulg(bits)
                        .wrapping_sub(u64::from(current))
                        .wrapping_mul(freq),
                );
                // `tree[m].Len = (ush)bits;`
                set_len_at(state, kind, node, as_ush(bits));
            }

            // `n--;`
            remaining -= 1;
        }
    }
}

/// Constructs one Huffman tree, assigns every code string and length, and adds
/// the block's bit cost to both accumulators.
///
/// Mirrors `build_tree` (L627-L706).
///
/// IN assertion: `Freq` is set for every element (L622). OUT assertions: `Len`
/// and `Code` are set to the optimal bit length and the corresponding code,
/// `opt_len` is updated, `static_len` is updated when this tree has a static
/// counterpart, and `max_code` is set (L623-L625).
///
/// # Structure
///
/// The initial heap holds one entry per symbol of non-zero frequency, with the
/// least frequent at `heap[SMALLEST]`; the sons of `heap[n]` are `heap[2n]` and
/// `heap[2n + 1]` and `heap[0]` is unused (L635-L637). The combine loop then
/// repeatedly removes the two least frequent nodes, joins them under a new
/// internal node numbered from `elems` upwards, and pushes that node back --
/// which is Huffman's algorithm, with `depth` breaking frequency ties as
/// [`HeapView::smaller`] describes. The nodes are recorded from the top of `heap`
/// downwards as they are removed, so that `heap[heap_max..]` ends up sorted by
/// increasing frequency, which is the order [`gen_bitlen`] needs.
///
/// # The forced-two-codes repair (L650-L661)
///
/// The symbol each forced code is given is the load-bearing part, because it decides which
/// code the decoder will see (`trees.c` L651):
///
/// ```c
/// node = s->heap[++(s->heap_len)] = (max_code < 2 ? ++max_code : 0);
/// ```
///
/// The reference's reason is quoted directly: "The pkzip format requires that at
/// least one distance code exists, and that at least one bit should be sent even
/// if there is only one possible code. So to avoid special checks later on we
/// force at least two codes of non zero frequency" (L650-L653).
///
/// Three details are reproduced exactly rather than tidied.
///
/// * `++max_code` is a pre-increment with a side effect: it advances `max_code`
///   *and* yields the advanced value as the new node, and that advanced value is
///   what gets stored into `desc->max_code` at L662.
/// * The invented node is 0 or 1, so "it does not have extra bits" (L660) and the
///   accumulators need no residue term.
/// * `opt_len--` and `static_len -=` run while both are still zero, so both wrap.
///   The wrap is intentional pre-compensation: the artificial node is about to be
///   counted by [`gen_bitlen`], and the two cancel exactly. This is why every
///   operation here is a `wrapping_*` one.
///
/// A clean-room implementation would plausibly handle a degenerate alphabet some
/// other way; doing so would change the emitted bytes, so it is out of the
/// question.
///
/// # Why `dyn_ltree` is longer than its alphabet
///
/// `node` starts at `elems` and grows by one per internal node, so a tree array
/// must hold `2 * elems - 1` entries. That is why `dyn_ltree` has `HEAP_SIZE`
/// (573) entries for an alphabet of `L_CODES` (286) and must not be shortened.
pub(crate) fn build_tree<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
) {
    let desc = StaticTreeDesc::for_kind(kind);
    let stree = desc.static_tree;
    let elems = desc.elems;
    // `int max_code = -1;` -- the largest code with a non-zero frequency.
    let mut max_code: i32 = -1;

    // `s->heap_len = 0, s->heap_max = HEAP_SIZE;`
    state.heap_len = 0;
    state.heap_max = as_int(HEAP_SIZE);

    // `for (n = 0; n < elems; n++)`
    for index in 0..elems {
        let node = as_int(index);
        if freq_at(state, kind, node) == 0 {
            // `tree[n].Len = 0;`
            set_len_at(state, kind, node, 0);
        } else {
            // `s->heap[++(s->heap_len)] = max_code = n; s->depth[n] = 0;`
            state.heap_len += 1;
            set_heap_at(state, state.heap_len, node);
            max_code = node;
            set_depth_at(state, node, 0);
        }
    }

    // `while (s->heap_len < 2)` -- the forced-two-codes repair; see above.
    while state.heap_len < 2 {
        // `node = s->heap[++(s->heap_len)] = (max_code < 2 ? ++max_code : 0);`
        let node = if max_code < 2 {
            max_code += 1;
            max_code
        } else {
            0
        };
        state.heap_len += 1;
        set_heap_at(state, state.heap_len, node);

        // `tree[node].Freq = 1; s->depth[node] = 0;`
        set_freq_at(state, kind, node, 1);
        set_depth_at(state, node, 0);

        // `s->opt_len--;` -- WRAPS, and the wrap is the point: it pre-compensates
        // for the artificial node `gen_bitlen` will count. See the module
        // documentation for the measured values.
        state.opt_len = state.opt_len.wrapping_sub(1);

        // `if (stree) s->static_len -= stree[node].Len;` -- also wraps, for the
        // same reason. `STATIC_BL_DESC` has no static tree, so the bit-length
        // tree's `static_len` is left alone entirely.
        if let Some(stree) = stree {
            let static_bits = u64::from(stree.get(as_index(node)).copied().map_or(0, CtData::len));
            state.static_len = state.static_len.wrapping_sub(static_bits);
        }
    }

    // `desc->max_code = max_code;` -- the value the repair may just have
    // advanced.
    state.desc_for_mut(kind).max_code = max_code;

    // `for (n = s->heap_len/2; n >= 1; n--) pqdownheap(s, tree, n);` -- the
    // elements `heap[heap_len/2 + 1 ..= heap_len]` are leaves, so sub-heaps of
    // increasing length are established from the middle outwards (L664-L666).
    let mut seed = state.heap_len / 2;
    while seed >= 1 {
        pqdownheap(state, kind, seed);
        seed -= 1;
    }

    // `node = elems;` -- the next internal node of the tree. Then
    // `do { ... } while (s->heap_len >= 2);`, which is entered unconditionally
    // because the repair above guarantees at least two nodes.
    let mut node = as_int(elems);
    loop {
        // `pqremove(s, tree, n);` -- n is the node of least frequency.
        let least = pqremove(state, kind);
        // `m = s->heap[SMALLEST];` -- m is the node of next least frequency.
        let next_least = heap_at(state, SMALLEST);

        // `s->heap[--(s->heap_max)] = n; s->heap[--(s->heap_max)] = m;` -- keep
        // the nodes sorted by frequency, growing downwards from the top.
        state.heap_max -= 1;
        set_heap_at(state, state.heap_max, least);
        state.heap_max -= 1;
        set_heap_at(state, state.heap_max, next_least);

        // `tree[node].Freq = tree[n].Freq + tree[m].Freq;` -- C adds two `ush`
        // values as `int` and truncates on the store; the sum is the number of
        // symbols under the new node and cannot reach 65536, so the wrap is
        // unreachable and only reproduces the cast.
        let combined = freq_at(state, kind, least).wrapping_add(freq_at(state, kind, next_least));
        set_freq_at(state, kind, node, combined);

        // `s->depth[node] = (uch)((s->depth[n] >= s->depth[m] ? s->depth[n] : s->depth[m]) + 1);`
        // -- written as the conditional C uses rather than as `max`, and with the
        // same `uch` truncation. A depth of 255 would need Fibonacci-like
        // frequencies far beyond a 16-bit counter, so it cannot be reached.
        let depth_least = depth_at(state, least);
        let depth_next = depth_at(state, next_least);
        let deeper = if depth_least >= depth_next {
            depth_least
        } else {
            depth_next
        };
        set_depth_at(state, node, deeper.wrapping_add(1));

        // `tree[n].Dad = tree[m].Dad = (ush)node;`
        set_dad_at(state, kind, least, as_ush(node));
        set_dad_at(state, kind, next_least, as_ush(node));

        // `s->heap[SMALLEST] = node++; pqdownheap(s, tree, SMALLEST);` -- insert
        // the new node into the heap.
        set_heap_at(state, SMALLEST, node);
        node += 1;
        pqdownheap(state, kind, SMALLEST);

        // `} while (s->heap_len >= 2);`
        if state.heap_len < 2 {
            break;
        }
    }

    // `s->heap[--(s->heap_max)] = s->heap[SMALLEST];` -- the root, which is the
    // last node left in the heap.
    let root = heap_at(state, SMALLEST);
    state.heap_max -= 1;
    set_heap_at(state, state.heap_max, root);

    // "At this point, the fields freq and dad are set. We can now generate the
    // bit lengths." (L699-L701)
    gen_bitlen(state, kind);

    // "The field len is now set, we can generate the bit codes" (L704). The order
    // is load-bearing: `gen_codes` reads the lengths `gen_bitlen` writes.
    gen_codes(state, kind, max_code);
}

/// `s->bl_tree[code].Freq++`.
///
/// Saturating rather than wrapping, matching `CtData::increment_freq`. The
/// counter cannot get anywhere near saturation: it counts runs over an alphabet
/// of at most `L_CODES + 1` symbols.
#[inline]
fn bump_bl_freq<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, code: usize) {
    if let Some(entry) = state.bl_tree.get_mut(code) {
        entry.increment_freq();
    }
}

/// `s->bl_tree[code].Freq += count` (L729).
///
/// Saturating for the same reason as [`bump_bl_freq`]: the run counts over one
/// tree sum to at most the size of its alphabet.
#[inline]
fn add_bl_freq<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, code: usize, count: u16) {
    if let Some(entry) = state.bl_tree.get_mut(code) {
        let updated = entry.freq().saturating_add(count);
        entry.set_freq(updated);
    }
}

/// `send_code(s, code, s->bl_tree)`.
///
/// The bit-length tree lives inside the state, so the entry is copied out before
/// the state is written through; `send_code` itself can only be handed a table
/// that is independent of the state, such as one of the `'static` constants.
/// This is the arrangement `trees/bit_writer.rs` documents for exactly this case.
#[inline]
fn send_bl_code<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>, code: usize) {
    let entry = state.bl_tree.get(code).copied().unwrap_or_default();
    send_bits(state, entry.code(), i32::from(entry.len()));
}

/// Scans a literal or distance tree to accumulate the frequencies of the codes
/// that will transmit its bit lengths.
///
/// Mirrors `scan_tree` (L712-L747). It counts into `bl_tree` exactly what
/// [`send_tree`] will later emit, so the two must agree run for run -- they share
/// one control structure and are kept as two functions, as in the reference,
/// because merging them would obscure the emission order that agreement depends
/// on.
///
/// # The run encoding
///
/// Lengths are transmitted as a run-length code (`doc/rfc1951.txt` 3.2.7): a run
/// shorter than `min_count` is written out literally; a longer run of a non-zero
/// length becomes [`REP_3_6`]; a run of zeros becomes [`REPZ_3_10`] or
/// [`REPZ_11_138`]. `max_count` and `min_count` are re-chosen after every run
/// from whether the next length is zero and whether it repeats the current one,
/// which is why the same triple appears at the end of the loop in both functions.
///
/// `prevlen` starts at -1, a value no real length can take, so the first run
/// always transmits its length explicitly. It is signed for that reason.
///
/// # The guard element
///
/// L722 writes `tree[max_code + 1].Len = 0xffff`, one element *past* `max_code`,
/// so that the final iteration's lookahead `tree[n + 1].Len` compares unequal to
/// everything and terminates the run. [`send_tree`] relies on that write having
/// happened -- its own comment at L762 reads "guard already set" -- which is why
/// `build_bl_tree` must scan both trees before `send_all_trees` sends either.
///
/// Every array is long enough for the write: `dyn_ltree` holds 573 entries and
/// its `max_code` is at most `L_CODES - 1` = 285; `dyn_dtree` holds 61 and its
/// `max_code` is at most `D_CODES - 1` = 29; `bl_tree` holds 39 and its
/// `max_code` is at most `BL_CODES - 1` = 18.
// `needless_continue` wants the `continue` arm split off from the ladder that
// follows it. That ladder is a verbatim mirror of L726-L737, where the `continue`
// is the first arm of one `if`/`else if` chain; splitting it would stop this
// function and `send_tree` reading as the same code, which is the property their
// run-for-run agreement rests on.
#[allow(clippy::needless_continue)]
pub(crate) fn scan_tree<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    max_code: i32,
) {
    // `int prevlen = -1;` -- the last emitted length.
    let mut prevlen: i32 = -1;
    // `int nextlen = tree[0].Len;` -- the length of the next code.
    let mut nextlen = i32::from(len_at(state, kind, 0));
    // `int count = 0;` -- the repeat count of the current code.
    let mut count: i32 = 0;
    // `int max_count = 7; int min_count = 4;`
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    // `if (nextlen == 0) max_count = 138, min_count = 3;`
    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    // `tree[max_code + 1].Len = (ush)0xffff;` -- the guard; see above.
    set_len_at(state, kind, max_code + 1, 0xffff);

    // `for (n = 0; n <= max_code; n++)`
    for node in 0..=max_code {
        // `curlen = nextlen; nextlen = tree[n + 1].Len;`
        let curlen = nextlen;
        nextlen = i32::from(len_at(state, kind, node + 1));

        // `if (++count < max_count && curlen == nextlen) continue;` -- the
        // increment happens before the comparison.
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            // `s->bl_tree[curlen].Freq += (ush)count;`
            add_bl_freq(state, as_index(curlen), as_ush(count));
        } else if curlen != 0 {
            // `if (curlen != prevlen) s->bl_tree[curlen].Freq++;`
            if curlen != prevlen {
                bump_bl_freq(state, as_index(curlen));
            }
            // `s->bl_tree[REP_3_6].Freq++;`
            bump_bl_freq(state, REP_3_6);
        } else if count <= 10 {
            // `s->bl_tree[REPZ_3_10].Freq++;`
            bump_bl_freq(state, REPZ_3_10);
        } else {
            // `s->bl_tree[REPZ_11_138].Freq++;`
            bump_bl_freq(state, REPZ_11_138);
        }

        // `count = 0; prevlen = curlen;`
        count = 0;
        prevlen = curlen;

        // The reset triple (L739-L745).
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Sends a literal or distance tree in compressed form, using the codes in
/// `bl_tree`.
///
/// Mirrors `send_tree` (L753-L794). The control structure is identical to
/// [`scan_tree`]'s; every place that function increments a frequency, this one
/// emits the corresponding code, so the bit-length tree it emits with is exactly
/// the tree those frequencies produced.
///
/// The guard element at `tree[max_code + 1]` is *not* written here -- the
/// reference's commented-out line at L762 says "guard already set" -- so
/// [`scan_tree`] must have run over this tree first.
// Allowed for the same reason as on `scan_tree`: L767-L784 is one `if`/`else if`
// ladder whose first arm is a `continue`, and the two functions must keep the
// same shape.
#[allow(clippy::needless_continue)]
pub(crate) fn send_tree<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    kind: StaticTreeKind,
    max_code: i32,
) {
    // The same six locals, initialised the same way (L754-L760).
    let mut prevlen: i32 = -1;
    let mut nextlen = i32::from(len_at(state, kind, 0));
    let mut count: i32 = 0;
    let mut max_count: i32 = 7;
    let mut min_count: i32 = 4;

    // `/* tree[max_code + 1].Len = -1; */  /* guard already set */` (L762), then
    // `if (nextlen == 0) max_count = 138, min_count = 3;`
    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }

    // `for (n = 0; n <= max_code; n++)`
    for node in 0..=max_code {
        // `curlen = nextlen; nextlen = tree[n + 1].Len;` -- at `n == max_code`
        // this reads the guard.
        let curlen = nextlen;
        nextlen = i32::from(len_at(state, kind, node + 1));

        // `if (++count < max_count && curlen == nextlen) continue;`
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            // `do { send_code(s, curlen, s->bl_tree); } while (--count != 0);`
            // `count` is at least 1 here because it was just incremented, so the
            // reference's `do`-`while` runs exactly `count` times; iterating over
            // that count emits the same sequence and cannot spin if the invariant
            // ever changed. C's `count = 0` below makes the residual irrelevant.
            for _ in 0..count {
                send_bl_code(state, as_index(curlen));
            }
        } else if curlen != 0 {
            // `if (curlen != prevlen) { send_code(s, curlen, s->bl_tree); count--; }`
            if curlen != prevlen {
                send_bl_code(state, as_index(curlen));
                count -= 1;
            }
            // `Assert(count >= 3 && count <= 6, " 3_6?");`
            debug_assert!(
                (3..=6).contains(&count),
                "REP_3_6 repeat count out of range (trees.c L776)"
            );
            // `send_code(s, REP_3_6, s->bl_tree); send_bits(s, count - 3, 2);`
            send_bl_code(state, REP_3_6);
            send_bits(state, as_ush(count - 3), 2);
        } else if count <= 10 {
            // `send_code(s, REPZ_3_10, s->bl_tree); send_bits(s, count - 3, 3);`
            send_bl_code(state, REPZ_3_10);
            send_bits(state, as_ush(count - 3), 3);
        } else {
            // `send_code(s, REPZ_11_138, s->bl_tree); send_bits(s, count - 11, 7);`
            send_bl_code(state, REPZ_11_138);
            send_bits(state, as_ush(count - 11), 7);
        }

        // `count = 0; prevlen = curlen;`
        count = 0;
        prevlen = curlen;

        // The same reset triple as `scan_tree`'s (L786-L792).
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Builds the Huffman tree for the bit lengths and returns the index in
/// `bl_order` of the last bit-length code that has to be sent.
///
/// Mirrors `build_bl_tree` (L800-L826). On return, `opt_len` "includes the length
/// of the tree representations, except the lengths of the bit lengths codes and
/// the 5 + 5 + 4 bits for the counts" (L809-L811) -- and then L821 adds those too,
/// so on return from *this* function `opt_len` is the complete cost of a dynamic
/// block, header and body.
///
/// # The trailing search
///
/// ```c
/// for (max_blindex = BL_CODES-1; max_blindex >= 3; max_blindex--) {
///     if (s->bl_tree[bl_order[max_blindex]].Len != 0) break;
/// }
/// ```
///
/// `bl_order` lists the bit-length codes "in order of decreasing probability, to
/// avoid transmitting the lengths for unused bit length codes" (L73-L75), so
/// walking it backwards finds the last one that is actually used. The floor of 3
/// is there because "the pkzip format requires that at least 4 bit length codes
/// be sent. (appnote.txt says 3 but the actual value used is 4.)" (L813-L815).
///
/// Note that C's `for` decrements *before* re-testing, so if no code at index 3
/// or above is used the loop leaves `max_blindex` at 2, not 3. That is
/// transliterated as-is rather than clamped, because clamping would change
/// `opt_len` and therefore the block type. It is unreachable in practice: a
/// literal tree always has at least one symbol of non-zero length, so
/// [`scan_tree`] always raises the frequency of at least one length code in
/// `0..=15`, and every one of those sits at index 3 or above in `bl_order`.
pub(crate) fn build_bl_tree<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) -> i32 {
    // `scan_tree(s, (ct_data *)s->dyn_ltree, s->l_desc.max_code);`
    let literal_max = state.desc_for(StaticTreeKind::Literal).max_code;
    scan_tree(state, StaticTreeKind::Literal, literal_max);
    // `scan_tree(s, (ct_data *)s->dyn_dtree, s->d_desc.max_code);`
    let distance_max = state.desc_for(StaticTreeKind::Distance).max_code;
    scan_tree(state, StaticTreeKind::Distance, distance_max);

    // `build_tree(s, (tree_desc *)(&(s->bl_desc)));`
    build_tree(state, StaticTreeKind::BitLength);

    // The trailing search; see above.
    let mut max_blindex = as_int(BL_CODES) - 1;
    while max_blindex >= 3 {
        let code = usize::from(bl_order.get(as_index(max_blindex)).copied().unwrap_or(0));
        if state.bl_tree.get(code).copied().map_or(0, CtData::len) != 0 {
            break;
        }
        max_blindex -= 1;
    }

    // `s->opt_len += 3*((ulg)max_blindex + 1) + 5 + 5 + 4;` -- three bits per
    // transmitted bit-length code plus the HLIT, HDIST and HCLEN fields of
    // `doc/rfc1951.txt` 3.2.7. Wrapping because `opt_len` may still be carrying
    // the forced-two-codes pre-compensation.
    state.opt_len = state
        .opt_len
        .wrapping_add(3u64.wrapping_mul(as_ulg(max_blindex).wrapping_add(1)))
        .wrapping_add(5 + 5 + 4);

    max_blindex
}

/// Sends the header of a dynamic-Huffman block: the three counts, the lengths of
/// the bit-length codes, then the literal tree and the distance tree.
///
/// Mirrors `send_all_trees` (L833-L855).
///
/// IN assertion: `lcodes >= 257`, `dcodes >= 1`, `blcodes >= 4` (L831), and each
/// is bounded above by its alphabet size (L838-L839). Both are `Assert`s in the
/// reference, so both are `debug_assert!` here and neither can abort a release
/// build.
///
/// The three counts are biased exactly as the reference writes them, with its two
/// corrections to the format note kept in the comments: the literal count is sent
/// as `lcodes - 257`, "not +255 as stated in appnote.txt" (L841), and the
/// bit-length count as `blcodes - 4`, "not -3 as stated in appnote.txt" (L843).
/// These three fields are HLIT, HDIST and HCLEN of `doc/rfc1951.txt` 3.2.7, and
/// the literal tree is sent before the distance tree because that is the order
/// the format defines.
pub(crate) fn send_all_trees<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    lcodes: i32,
    dcodes: i32,
    blcodes: i32,
) {
    debug_assert!(
        lcodes >= 257 && dcodes >= 1 && blcodes >= 4,
        "not enough codes (trees.c L837)"
    );
    debug_assert!(
        lcodes <= as_int(L_CODES) && dcodes <= as_int(D_CODES) && blcodes <= as_int(BL_CODES),
        "too many codes (trees.c L838)"
    );

    // `send_bits(s, lcodes - 257, 5);` -- not +255 as stated in appnote.txt.
    send_bits(state, as_ush(lcodes - 257), 5);
    // `send_bits(s, dcodes - 1, 5);`
    send_bits(state, as_ush(dcodes - 1), 5);
    // `send_bits(s, blcodes - 4, 4);` -- not -3 as stated in appnote.txt.
    send_bits(state, as_ush(blcodes - 4), 4);

    // `for (rank = 0; rank < blcodes; rank++) send_bits(s, s->bl_tree[bl_order[rank]].Len, 3);`
    for rank in 0..blcodes {
        let code = usize::from(bl_order.get(as_index(rank)).copied().unwrap_or(0));
        let len = state.bl_tree.get(code).copied().map_or(0, CtData::len);
        send_bits(state, len, 3);
    }

    // `send_tree(s, (ct_data *)s->dyn_ltree, lcodes - 1);` -- the literal tree.
    send_tree(state, StaticTreeKind::Literal, lcodes - 1);
    // `send_tree(s, (ct_data *)s->dyn_dtree, dcodes - 1);` -- the distance tree.
    send_tree(state, StaticTreeKind::Distance, dcodes - 1);
}

/// Maps a match distance to its distance code.
///
/// Mirrors the `d_code` macro (`deflate.h` L320-L321):
///
/// ```c
/// #define d_code(dist) \
///    ((dist) < 256 ? _dist_code[dist] : _dist_code[256+((dist)>>7)])
/// ```
///
/// `dist` is the match distance *minus one*, which is why the callers decrement
/// before calling. The table's "first 256 values correspond to the distances
/// 3 .. 258, the last 256 values correspond to the top 8 bits of the 15 bit
/// distances" (L99-L101), so the two halves are indexed differently: directly for
/// short distances, and by the high bits for long ones.
///
/// The index cannot leave the table. A distance is at most `MAX_DIST`, so
/// `dist <= 32767` and `256 + (32767 >> 7)` is 511, the last of the
/// `DIST_CODE_LEN` = 512 entries. `_dist_code[256]` and `_dist_code[257]` "are
/// never used" (L102-L103) because a `dist` of 256 or 257 lands in the second
/// branch, whose index starts at 258.
///
/// A `pub(crate)` function rather than a private one because `_tr_tally`
/// (L1095-L1119, in `trees/mod.rs`) needs the same mapping when it tallies a
/// match, and both sides must use the identical table lookup.
/// The index is computed in `u32`, the width C's `unsigned dist` already has,
/// rather than widened to `ulg` first. That keeps the conversion to an index
/// infallible on every target with a 32-bit-or-wider `usize`, so nothing is
/// branched on -- this runs twice per match, once in `_tr_tally` and once in
/// `compress_block`.
#[must_use]
#[inline]
pub(crate) fn d_code(dist: u32) -> usize {
    let index = if dist < 256 {
        dist
    } else {
        (dist >> 7).wrapping_add(256)
    };

    usize::from(
        _dist_code
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(0),
    )
}

/// Which pair of Huffman trees a block body is emitted with.
///
/// The reference passes the two trees as pointers --
/// `compress_block(s, static_ltree, static_dtree)` at L1060-L1061 and
/// `compress_block(s, s->dyn_ltree, s->dyn_dtree)` at L1069-L1070 -- and those
/// are the only two calls there are. Naming the pair instead of pointing at it is
/// what lets the dynamic case work at all in safe Rust: `dyn_ltree` lives inside
/// the very state that `send_bits` writes through, so a borrow of it could not be
/// held across the emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BlockTrees {
    /// The fixed codes of `doc/rfc1951.txt` 3.2.6 -- `static_ltree` and
    /// `static_dtree` (L1060-L1061).
    Static,
    /// The block's own codes -- `s->dyn_ltree` and `s->dyn_dtree` (L1069-L1070).
    Dynamic,
}

/// `send_code(s, code, ltree)` for whichever literal/length tree is in use.
///
/// The static table is a `const`, so it is independent of the state and
/// `send_code` can take it directly. The dynamic tree is a field of the state, so
/// its entry is copied out first.
///
/// # Why the tree is a const generic and not a parameter
///
/// C chooses the two trees **once per block** -- `compress_block` takes them as
/// two `const ct_data *` arguments (L900) and the only two calls there are pass
/// either the static pair (L1060-L1061) or the dynamic pair (L1069-L1070). A
/// runtime [`BlockTrees`] parameter turned that single choice into a branch per
/// emitted symbol, which release codegen kept, along with a call it declined to
/// inline. `USE_STATIC` restores C's shape: the two monomorphisations are
/// generated separately, the `if` below is folded at compile time, and
/// `#[inline(always)]` puts the body back at the call site the way C's
/// `send_code` macro was expanded there.
#[allow(clippy::inline_always)]
#[inline(always)]
fn send_literal_code<'a, A: Allocator<'a>, const USE_STATIC: bool>(
    state: &mut DeflateState<'a, A>,
    code: usize,
) {
    if USE_STATIC {
        send_code(state, code, &static_ltree);
    } else {
        let entry = state.dyn_ltree.get(code).copied().unwrap_or_default();
        send_bits(state, entry.code(), i32::from(entry.len()));
    }
}

/// `send_code(s, code, dtree)` for whichever distance tree is in use.
///
/// `USE_STATIC` and `#[inline(always)]` are there for the reason given on
/// [`send_literal_code`].
#[allow(clippy::inline_always)]
#[inline(always)]
fn send_distance_code<'a, A: Allocator<'a>, const USE_STATIC: bool>(
    state: &mut DeflateState<'a, A>,
    code: usize,
) {
    if USE_STATIC {
        send_code(state, code, &static_dtree);
    } else {
        let entry = state.dyn_dtree.get(code).copied().unwrap_or_default();
        send_bits(state, entry.code(), i32::from(entry.len()));
    }
}

/// Sends the body of a block: every tallied symbol, then the end-of-block code.
///
/// Mirrors `compress_block` (L900-L951).
///
/// # The symbol buffer
///
/// `LIT_MEM` is commented out at `deflate.h` L28, so this is the default layout:
/// one `sym_buf` holding three bytes per symbol, decoded exactly as L913-L915
/// does it --
///
/// ```c
/// dist  = s->sym_buf[sx++] & 0xff;
/// dist += (unsigned)(s->sym_buf[sx++] & 0xff) << 8;
/// lc    = s->sym_buf[sx++];
/// ```
///
/// -- which is a little-endian distance followed by a length or literal.
/// `PendingBuf::decode_symbols` performs precisely that decode, so the three reads
/// and the three increments of `sx` become one array slot and one `SYMBOL_BYTES`
/// step. The `d_buf`/`l_buf` pair of the `LIT_MEM` build (L909-L911) is not
/// implemented.
///
/// Symbols are decoded [`SYMBOL_BATCH`] at a time rather than one at a time. C
/// reads its three bytes straight out of `sym_buf` with no bound to establish;
/// safe Rust has to establish one, and doing it once per batch instead of once per
/// symbol is what keeps that cost off the per-symbol path. See
/// [`SYMBOL_BATCH`] for why reading a batch before emitting any of it cannot lose
/// a symbol.
///
/// A distance of zero marks a literal (L916-L917). Otherwise `lc` is the match
/// length minus `MIN_MATCH`, and the pair is emitted as a length code with its
/// residue followed by a distance code with its residue, per
/// `doc/rfc1951.txt` 3.2.5. Note the order: the whole length -- code *and* extra
/// bits -- precedes the distance, and `dist` is decremented between them because
/// the distance alphabet is indexed by distance minus one.
///
/// # The zero test
///
/// The reference writes `if (s->sym_next != 0) do { ... } while (sx < s->sym_next);`
/// -- a `do`-`while` guarded by a zero test, because the body must not run for an
/// empty block. A `while sx < sym_next` loop is equivalent *only* because of that
/// guard, which is why the guard's reasoning is recorded here rather than dropped:
/// with `sym_next == 0` both forms emit nothing but the end-of-block code.
///
/// The end-of-block code at L950 is unconditional and outside the loop.
pub(crate) fn compress_block<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    trees: BlockTrees,
) {
    // C's single choice of tree pair, made once per block (L1060-L1061 and
    // L1069-L1070), expressed as the one place this port branches on it.
    match trees {
        BlockTrees::Static => compress_block_with::<A, true>(state),
        BlockTrees::Dynamic => compress_block_with::<A, false>(state),
    }
}

/// How many symbols one [`crate::weak_slice::PendingBuf::decode_symbols`] call
/// establishes bounds for.
///
/// C decodes one symbol at a time straight out of `sym_buf` (L913-L915) with no
/// bound to establish; safe Rust has to establish one, so it establishes it for a
/// batch. Thirty-two symbols is 96 bytes of buffer described by one range check
/// and 128 bytes of stack for the decoded pairs -- small enough to stay in
/// registers-and-cache territory, large enough that the range work per symbol
/// disappears.
///
/// # Why reading ahead cannot lose a symbol
///
/// The compressed output and the unread symbols share one allocation, and
/// `Assert(s->pending < s->lit_bufsize + sx, "pendingBuf overflow")` (L945) is
/// what keeps the write cursor behind the read cursor. Emitting the batch may
/// therefore overwrite the very bytes the batch came from -- which the assertion
/// permits, and which costs nothing here because those symbols have already been
/// copied out. What the assertion also guarantees is that after the batch's last
/// symbol the cursor is still below `lit_bufsize + sx`, and `sx` by then names the
/// *end* of the batch, so the next batch's bytes have not been touched.
const SYMBOL_BATCH: usize = 32;

/// The body of [`compress_block`], monomorphised for one of the two tree pairs.
///
/// `USE_STATIC` selects `static_ltree`/`static_dtree` when true and
/// `dyn_ltree`/`dyn_dtree` when false; see [`send_literal_code`] for why it is a
/// const generic rather than a parameter.
fn compress_block_with<'a, A: Allocator<'a>, const USE_STATIC: bool>(
    state: &mut DeflateState<'a, A>,
) {
    let sym_next = state.sym_next();
    // `unsigned sx = 0;` -- the running index in the symbol buffer.
    let mut sx: usize = 0;
    let mut batch = [(0_u16, 0_u8); SYMBOL_BATCH];

    while sx < sym_next {
        // The three-byte decode of L913-L915, for up to `SYMBOL_BATCH` symbols
        // under one range check. Zero is unreachable while `sx < sym_next`:
        // `sym_next` is a whole number of symbols within the buffer, which
        // `push_symbol` maintains.
        let filled = state.pending.decode_symbols(sx, &mut batch);
        if filled == 0 {
            break;
        }

        for &(symbol_dist, len_or_lit) in batch.iter().take(filled) {
            sx = sx.saturating_add(SYMBOL_BYTES);

            // C's `unsigned dist` and `int lc`. `len_or_lit` is a `u8`, so both
            // conversions are infallible -- which is the point: `lc` indexes
            // `_length_code`, whose 256 entries cover every value a byte can hold,
            // so no fallible narrowing and no range fallback is needed on the
            // per-symbol path.
            let mut dist = u32::from(symbol_dist);
            let lc = usize::from(len_or_lit);

            if dist == 0 {
                // `send_code(s, lc, ltree);` -- a literal byte.
                send_literal_code::<A, USE_STATIC>(state, lc);
            } else {
                // Here `lc` is the match length minus `MIN_MATCH`.
                // `code = _length_code[lc];`
                let code = usize::from(_length_code.get(lc).copied().unwrap_or(0));
                // `send_code(s, code + LITERALS + 1, ltree);` -- the length code.
                send_literal_code::<A, USE_STATIC>(
                    state,
                    code.saturating_add(LITERALS).saturating_add(1),
                );
                // `extra = extra_lbits[code]; if (extra != 0) { lc -= base_length[code]; send_bits(s, lc, extra); }`
                let extra = extra_lbits.get(code).copied().unwrap_or(0);
                if extra != 0 {
                    let residue = as_int(lc) - base_length.get(code).copied().unwrap_or(0);
                    send_bits(state, as_ush(residue), extra);
                }

                // `dist--;` -- dist is now the match distance minus one.
                dist -= 1;
                // `code = d_code(dist);`
                let code = d_code(dist);
                // `Assert (code < D_CODES, "bad d_code");`
                debug_assert!(code < D_CODES, "bad d_code (trees.c L931)");

                // `send_code(s, code, dtree);` -- the distance code.
                send_distance_code::<A, USE_STATIC>(state, code);
                // `extra = extra_dbits[code]; if (extra != 0) { dist -= (unsigned)base_dist[code]; send_bits(s, (int)dist, extra); }`
                let extra = extra_dbits.get(code).copied().unwrap_or(0);
                if extra != 0 {
                    dist -= u32::try_from(base_dist.get(code).copied().unwrap_or(0)).unwrap_or(0);
                    send_bits(state, as_ush(dist), extra);
                }
            }

            // `Assert(s->pending < s->lit_bufsize + sx, "pendingBuf overflow");`
            // (L945) -- the compressed output must not overtake the symbols it is
            // still reading. Debug-only, as in the reference, and checked per
            // symbol exactly as C checks it, because it is what licenses the
            // batching above.
            debug_assert!(
                state.pending_bytes() < state.lit_bufsize().saturating_add(sx),
                "pendingBuf overflow (trees.c L945)"
            );
        }
    }

    // `send_code(s, END_BLOCK, ltree);`
    send_literal_code::<A, USE_STATIC>(state, END_BLOCK);
}

/// Classifies the current block as text or binary from its literal frequencies.
///
/// Mirrors `detect_data_type` (L966-L991), contributed by Cosmin Truta and
/// described in `doc/txtvsbin.txt`, which sorts the 256 byte values into three
/// categories. The result becomes the caller-visible `z_stream.data_type`
/// (`zlib.h` L107), so it is externally observable and must match the reference
/// exactly.
///
/// A block is text when both conditions hold (L954-L959):
///
/// * no byte from the *block list* -- the non-portable control characters 0..6,
///   14..25 and 28..31 -- occurs, and
/// * at least one byte from the *allow list* -- 9 TAB, 10 LF, 13 CR, or 32..255
///   -- occurs.
///
/// Otherwise it is binary. The remaining six values form a *gray list* --
/// "7 BEL, 8 BS, 11 VT, 12 FF, 26 SUB, 27 ESC" (L961-L963) -- which this
/// algorithm ignores entirely: a block containing nothing but gray-listed bytes
/// has no allow-listed byte and so falls through to binary, which is also the
/// answer for an empty block (L987-L990).
///
/// IN assertion: the `Freq` fields of `dyn_ltree` are set (L964).
///
/// # The mask
///
/// `0xf3ffc07f` is the block list as a bit set, "binary
/// 11110011111111111100000001111111" (L967-L969). C shifts it in the `for`
/// statement's increment clause -- `for (n = 0; n <= 31; n++, block_mask >>= 1)`
/// -- so bit 0 always describes the byte under test. The shift below sits at the
/// end of the body, which is the same place: an early `return` skips it in both
/// languages.
#[must_use]
pub(crate) fn detect_data_type<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> i32 {
    // `unsigned long block_mask = 0xf3ffc07fUL;`
    let mut block_mask: u32 = 0xf3ff_c07f;

    // Check for non-textual ("block-listed") bytes.
    // `for (n = 0; n <= 31; n++, block_mask >>= 1) if ((block_mask & 1) && (s->dyn_ltree[n].Freq != 0)) return Z_BINARY;`
    for byte in 0..=31usize {
        if (block_mask & 1) != 0 && literal_freq(state, byte) != 0 {
            return Z_BINARY;
        }
        block_mask >>= 1;
    }

    // Check for textual ("allow-listed") bytes: TAB, LF and CR first.
    // `if (s->dyn_ltree[9].Freq != 0 || s->dyn_ltree[10].Freq != 0 || s->dyn_ltree[13].Freq != 0) return Z_TEXT;`
    if literal_freq(state, 9) != 0 || literal_freq(state, 10) != 0 || literal_freq(state, 13) != 0 {
        return Z_TEXT;
    }
    // `for (n = 32; n < LITERALS; n++) if (s->dyn_ltree[n].Freq != 0) return Z_TEXT;`
    for byte in 32..LITERALS {
        if literal_freq(state, byte) != 0 {
            return Z_TEXT;
        }
    }

    // "There are no 'block-listed' or 'allow-listed' bytes: this stream either is
    // empty or has tolerated ('gray-listed') bytes only." (L987-L989)
    Z_BINARY
}

/// `s->dyn_ltree[byte].Freq`, for [`detect_data_type`]'s three scans.
///
/// A separate helper rather than [`freq_at`] because the reference names the
/// array directly here instead of going through a descriptor, and because the
/// index is a byte value rather than a node number.
#[inline]
fn literal_freq<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>, byte: usize) -> u16 {
    state.dyn_ltree.get(byte).copied().map_or(0, CtData::freq)
}

/// Which of the three block encodings `_tr_flush_block` will emit.
///
/// The three cases and their order are those of L1044-L1074. The `u8` codes are
/// the block-type field of `doc/rfc1951.txt` 3.2.3, and
/// [`BlockChoice::header_bits`] assembles the three-bit header from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BlockChoice {
    /// A stored block -- `_tr_stored_block(s, buf, stored_len, last)` (L1056).
    /// Chosen when the raw bytes plus their four-byte length pair are no larger
    /// than the cheaper of the two coded forms, and the block's bytes are still
    /// available in the window.
    Stored,
    /// A block coded with the fixed trees -- `send_bits(s, (STATIC_TREES<<1) + last, 3)`
    /// followed by `compress_block(s, static_ltree, static_dtree)` (L1058-L1061).
    Static,
    /// A block coded with its own trees -- `send_bits(s, (DYN_TREES<<1) + last, 3)`,
    /// `send_all_trees(...)`, `compress_block(s, s->dyn_ltree, s->dyn_dtree)`
    /// (L1065-L1070).
    Dynamic,
}

impl BlockChoice {
    /// The block-type code this choice sends: [`STORED_BLOCK`], [`STATIC_TREES`]
    /// or [`DYN_TREES`] (`zutil.h` L87-L96).
    #[must_use]
    pub(crate) const fn block_type(self) -> u8 {
        match self {
            Self::Stored => STORED_BLOCK,
            Self::Static => STATIC_TREES,
            Self::Dynamic => DYN_TREES,
        }
    }

    /// The three-bit block header: `(block_type << 1) + last`.
    ///
    /// This is the expression the reference writes at L862, L1059 and L1066. The
    /// `last` flag is the BFINAL bit and the type is BTYPE, in that bit order
    /// (`doc/rfc1951.txt` 3.2.3); `send_bits` then emits them least significant
    /// bit first, which puts BFINAL on the wire first as the format requires.
    #[must_use]
    pub(crate) const fn header_bits(self, last: bool) -> u16 {
        (self.block_type() as u16) << 1 | (last as u16)
    }

    /// Which pair of trees the body of this block is coded with, or [`None`] for
    /// a stored block, whose body is not coded at all.
    // Exercised by this module's test suite but not called from a library path,
    // deliberately: `_tr_flush_block` in `trees/mod.rs` matches on the choice
    // itself so that its three arms stay line-for-line with `trees.c`
    // L1056-L1070, and folding the dispatch through this accessor would obscure
    // exactly the correspondence the byte-identity requirement is verified
    // against. Narrowly scoped rather than restructuring the emission code, and
    // `#[allow]` rather than `#[expect]` because the latter needs rustc 1.81 and
    // the MSRV is 1.80.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) const fn trees(self) -> Option<BlockTrees> {
        match self {
            Self::Stored => None,
            Self::Static => Some(BlockTrees::Static),
            Self::Dynamic => Some(BlockTrees::Dynamic),
        }
    }
}

/// The outcome of the block-type decision: the choice and the two byte counts it
/// was made from.
///
/// The counts are returned as well as the choice because `_tr_flush_block` uses
/// them for nothing else but this decision, and returning them is what makes the
/// decision testable at its boundaries -- the interesting cases are exactly
/// `static_lenb == opt_lenb` and `stored_len + 4 == opt_lenb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BlockSelection {
    /// The encoding to emit.
    pub(crate) choice: BlockChoice,
    /// `opt_lenb` -- the cost in bytes of the encoding finally preferred, after
    /// the L1035-L1037 substitution.
    pub(crate) opt_lenb: u64,
    /// `static_lenb` -- the cost in bytes of the fixed-tree encoding.
    pub(crate) static_lenb: u64,
}

/// Determines the best encoding for the current block.
///
/// Mirrors the decision half of `_tr_flush_block` (L1026-L1054): the arithmetic
/// at L1027-L1042 and the three predicates at L1044-L1074, without any of the
/// emission. `trees/mod.rs` owns the orchestration -- deciding whether to build
/// the trees at all, calling this, and then dispatching on the answer.
///
/// The two costs are rounded to whole bytes by truncating integer arithmetic, and the
/// comparison between them is what picks the block type (`trees.c` L1027-L1074):
///
/// ```c
/// opt_lenb    = (s->opt_len    + 3 + 7) >> 3;
/// static_lenb = (s->static_len + 3 + 7) >> 3;
/// if (static_lenb <= opt_lenb || s->strategy == Z_FIXED) opt_lenb = static_lenb;
/// ```
///
/// # What must not be changed
///
/// The `+ 3 + 7` then `>> 3` is a bit-to-byte conversion that rounds up while
/// paying for the three-bit block header, and it *truncates*. Rewriting it as a
/// division, a `div_ceil`, or anything involving a floating-point value would
/// change the answer at the boundaries, and the answer is the block type -- three
/// bits that every decoder reads. So it stays as a shift, with no reassociation.
///
/// The three predicates keep their order and their exact comparisons: stored
/// first, with `<=` and the `buf` test; then equality against `static_lenb`; then
/// dynamic as the fallback. Note that the stored test is reached even at
/// `level > 0`, which is what lets an incompressible block still be stored, and
/// that `static_lenb <= opt_lenb` uses `<=` so that a tie prefers the fixed trees
/// -- they cost nothing to transmit.
///
/// # Arguments
///
/// * `opt_len`, `static_len` -- `s->opt_len` and `s->static_len` in bits, as
///   [`build_tree`] and [`build_bl_tree`] leave them. Both may be carrying a
///   wrapped intermediate value, so every operation on them here wraps too.
/// * `stored_len` -- `stored_len`, the block's byte count, which
///   `FLUSH_BLOCK_ONLY` computes as `strstart - block_start` (`deflate.c` L1634).
/// * `level` -- `s->level`. At level 0 no trees were built, so the two byte
///   counts are replaced by the stored size and a stored block is forced.
/// * `strategy` -- `s->strategy`. [`Strategy::Fixed`] forbids dynamic trees, so it
///   forces `opt_lenb` down to `static_lenb` regardless of which is cheaper.
/// * `buf_available` -- the reference's `buf != (char *)0`. `FLUSH_BLOCK_ONLY`
///   passes `s->block_start >= 0L ? &s->window[s->block_start] : Z_NULL`
///   (`deflate.c` L1631-L1633), so this is exactly `block_start >= 0`, which is
///   what `Window::block_region` reports as [`Some`]. A window offset, never a
///   pointer.
#[must_use]
pub(crate) fn select_block_type(
    opt_len: u64,
    static_len: u64,
    stored_len: u64,
    level: i32,
    strategy: Strategy,
    buf_available: bool,
) -> BlockSelection {
    let mut opt_lenb: u64;
    let static_lenb: u64;

    if level > 0 {
        // `opt_lenb    = (s->opt_len    + 3 + 7) >> 3;`
        // `static_lenb = (s->static_len + 3 + 7) >> 3;`
        opt_lenb = opt_len.wrapping_add(3).wrapping_add(7) >> 3;
        static_lenb = static_len.wrapping_add(3).wrapping_add(7) >> 3;

        // `if (static_lenb <= opt_lenb || s->strategy == Z_FIXED) opt_lenb = static_lenb;`
        // -- the `#ifndef FORCE_STATIC` guard at L1034 wraps only the condition,
        // so the default build has the `if` and this implementation implements that.
        if static_lenb <= opt_lenb || strategy == Strategy::Fixed {
            opt_lenb = static_lenb;
        }
    } else {
        // `Assert(buf != (char*)0, "lost buf");` (L1040)
        debug_assert!(buf_available, "lost buf (trees.c L1040)");
        // `opt_lenb = static_lenb = stored_len + 5;` -- force a stored block.
        opt_lenb = stored_len.wrapping_add(5);
        static_lenb = opt_lenb;
    }

    // `if (stored_len + 4 <= opt_lenb && buf != (char*)0)` -- 4: two words for
    // the lengths. The `buf` test "is only necessary if LIT_BUFSIZE > WSIZE"
    // (L1050-L1055). This is the `#else` half of the `FORCE_STORED` conditional
    // at L1044-L1049, which is the default build.
    let choice = if stored_len.wrapping_add(4) <= opt_lenb && buf_available {
        BlockChoice::Stored
    } else if static_lenb == opt_lenb {
        BlockChoice::Static
    } else {
        BlockChoice::Dynamic
    };

    BlockSelection {
        choice,
        opt_lenb,
        static_lenb,
    }
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// harness: a test asserts, and a failing assertion panics. Indexing is allowed because every index
// below is bounded by a literal the line above it establishes. The same relaxation, for the same
// reason, appears in `trees/bit_writer.rs`, `deflate/pending.rs` and `deflate/state.rs`.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    // Imported rather than named as `alloc::vec::Vec` at each use site. The qualification is
    // *required* in the default `no_std` build, where `Vec` is not in the prelude, but is
    // redundant once the `std` feature is on -- and `unused_qualifications`, which the workspace
    // promotes to `warn`, fires on exactly that configuration. One import satisfies both.
    use alloc::vec::Vec;

    use super::{
        build_bl_tree, build_tree, compress_block, d_code, detect_data_type, gen_codes, init_block,
        pqdownheap, scan_tree, select_block_type, send_all_trees, send_tree, BlockChoice,
        BlockTrees, HeapView,
    };
    use crate::config::{Z_BINARY, Z_TEXT};
    use crate::deflate::state::{
        CtData, DeflateConfig, DeflateState, GlobalAllocator, Method, StaticTreeKind, Strategy,
        BL_CODES, DEF_MEM_LEVEL, DYN_TREES, D_CODES, HEAP_SIZE, LITERALS, L_CODES, MAX_BITS,
        STATIC_TREES, STORED_BLOCK,
    };
    use crate::trees::bit_writer::{bi_reverse, bi_windup};
    use crate::trees::static_tables::{bl_order, static_dtree, static_ltree, END_BLOCK};
    use crate::trees::tree_desc::StaticTreeDesc;

    /// The stream `deflateInit2_` produces for the default configuration -- level 6, `windowBits`
    /// 15, `memLevel` 8, [`Strategy::Default`] (`deflate.c` L387-L533), then wired up as `_tr_init`
    /// wires it (`trees.c` L456-L478) and given a fresh block by [`init_block`].
    ///
    /// The C oracle these tests were checked against fills its state with `0xa5` before doing the
    /// same wiring, exactly as `test/infcover.c` L87 fills the blocks it hands out, so every
    /// expectation below holds without any reliance on zeroed memory.
    fn fresh_state() -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: 15,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };

        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        init_block(&mut state);
        state.pending.reset_pending();
        state.bi_buf = 0;
        state.bi_valid = 0;
        state
    }

    /// Sets `freqs[n]` as the frequency of symbol `n` of the tree `kind` names, and clears the two
    /// accumulators so that a `build_tree` run is measured from zero.
    fn seed_freqs(
        state: &mut DeflateState<'static, GlobalAllocator>,
        kind: StaticTreeKind,
        freqs: &[u16],
    ) {
        for entry in &mut *state.tree_for_mut(kind) {
            *entry = CtData::default();
        }
        for (node, &freq) in freqs.iter().enumerate() {
            state.tree_for_mut(kind)[node].set_freq(freq);
        }
        state.opt_len = 0;
        state.static_len = 0;
    }

    /// The `Len` of symbols `0..=max_code` of the tree `kind` names.
    fn lens_of(
        state: &DeflateState<'static, GlobalAllocator>,
        kind: StaticTreeKind,
        max_code: i32,
    ) -> Vec<u16> {
        (0..=max_code)
            .map(|node| state.tree_for(kind)[usize::try_from(node).unwrap()].len())
            .collect()
    }

    /// The `Code` of symbols `0..=max_code` of the tree `kind` names.
    fn codes_of(
        state: &DeflateState<'static, GlobalAllocator>,
        kind: StaticTreeKind,
        max_code: i32,
    ) -> Vec<u16> {
        (0..=max_code)
            .map(|node| state.tree_for(kind)[usize::try_from(node).unwrap()].code())
            .collect()
    }

    /// `unsigned short a = 1, b = 1; for (n = 0; n < count; n++) { f[n] = a; t = a + b; a = b; b = t; }`
    ///
    /// Written out rather than tabulated so that the wrap of the 25th term -- 75025 does not fit in
    /// a `ush` -- happens here for the same reason it happens in the C oracle.
    fn fibonacci_freqs(count: usize) -> Vec<u16> {
        let mut out = Vec::with_capacity(count);
        let (mut a, mut b) = (1u16, 1u16);
        for _ in 0..count {
            out.push(a);
            let next = a.wrapping_add(b);
            a = b;
            b = next;
        }
        out
    }

    /// Gives every entry of `bl_tree` a five-bit code, so that [`send_tree`] and
    /// [`send_all_trees`] emit something a byte-for-byte comparison can pin down. This is what the
    /// C oracle does for the same reason.
    fn trivial_bl_codes(state: &mut DeflateState<'static, GlobalAllocator>) {
        for node in 0..BL_CODES {
            let code = bi_reverse(u16::try_from(node).unwrap(), 5);
            state.bl_tree[node].set_len(5);
            state.bl_tree[node].set_code(code);
        }
    }

    #[test]
    fn the_consumed_static_tables_have_the_reference_shape() {
        use crate::trees::static_tables::{
            _dist_code, _length_code, base_dist, base_length, extra_dbits, extra_lbits,
        };

        // Declared lengths, from `trees.h` and `trees.c` L62-L72.
        assert_eq!(static_ltree.len(), L_CODES + 2);
        assert_eq!(static_dtree.len(), D_CODES);
        assert_eq!(_dist_code.len(), 512);
        assert_eq!(_length_code.len(), 256);
        assert_eq!(extra_lbits.len(), 29);
        assert_eq!(extra_dbits.len(), D_CODES);
        assert_eq!(bl_order.len(), BL_CODES);

        // The three values this module's arithmetic leans on hardest.
        // `static_ltree[0].Len` is 8: it is the number `build_tree`'s repair subtracts from
        // `static_len` for an empty literal tree, and so the source of the measured
        // `u64::MAX - 7`.
        assert_eq!(static_ltree[0].len(), 8);
        // `static_ltree[END_BLOCK].Len` is 7, which with the above makes an empty block's
        // `static_len` come out at 7.
        assert_eq!(static_ltree[END_BLOCK].len(), 7);
        // Every static distance code is five bits (`trees.c` L94-L96).
        assert!(static_dtree.iter().all(|entry| entry.len() == 5));

        // `_length_code[255]` is overwritten to 28 rather than 27, because match length 258 "can
        // be represented in two different ways: code 284 + 5 bits or code 285" and the reference
        // picks the shorter (`trees.c` L326-L330).
        assert_eq!(_length_code[255], 28);
        assert_eq!(_length_code[254], 27);
        // The last length code carries no residue and has base 0 for the same reason.
        assert_eq!(extra_lbits[28], 0);
        assert_eq!(base_length[28], 0);
        assert_eq!(base_dist[29], 24576);
        assert_eq!(extra_dbits[29], 13);
        // `bl_order` begins with the three repeat codes (`trees.c` L71-L72).
        assert_eq!(bl_order[0], 16);
        assert_eq!(bl_order[1], 17);
        assert_eq!(bl_order[2], 18);
    }

    #[test]
    fn the_static_descriptors_are_the_ones_this_module_expects() {
        let literal = StaticTreeDesc::for_kind(StaticTreeKind::Literal);
        assert_eq!(literal.elems, L_CODES);
        assert_eq!(literal.extra_base, LITERALS + 1);
        assert_eq!(literal.max_length, MAX_BITS);
        assert!(literal.static_tree.is_some());

        let distance = StaticTreeDesc::for_kind(StaticTreeKind::Distance);
        assert_eq!(distance.elems, D_CODES);
        assert_eq!(distance.extra_base, 0);
        assert_eq!(distance.max_length, MAX_BITS);
        assert!(distance.static_tree.is_some());

        // `static_bl_desc` has a null static tree, which is why an empty bit-length tree does not
        // move `static_len` at all (`trees.c` L137-L138).
        let bit_length = StaticTreeDesc::for_kind(StaticTreeKind::BitLength);
        assert_eq!(bit_length.elems, BL_CODES);
        assert_eq!(bit_length.extra_base, 0);
        assert_eq!(bit_length.max_length, 7);
        assert!(bit_length.static_tree.is_none());
    }

    #[test]
    fn init_block_clears_the_alphabets_and_sets_end_of_block() {
        let mut state = fresh_state();

        // Dirty every entry of all three trees first, so that nothing below can be passing merely
        // because the allocation happened to be zeroed.
        for node in 0..L_CODES {
            state.dyn_ltree[node].set_freq(0xa5a5);
        }
        for node in 0..D_CODES {
            state.dyn_dtree[node].set_freq(0xa5a5);
        }
        for node in 0..BL_CODES {
            state.bl_tree[node].set_freq(0xa5a5);
        }
        state.opt_len = 0xa5a5_a5a5;
        state.static_len = 0xa5a5_a5a5;
        state.matches = 0xa5a5;

        init_block(&mut state);

        for node in 0..L_CODES {
            let expected = u16::from(node == END_BLOCK);
            assert_eq!(
                state.dyn_ltree[node].freq(),
                expected,
                "dyn_ltree[{node}] after init_block"
            );
        }
        assert!(state.dyn_dtree[..D_CODES]
            .iter()
            .all(|entry| entry.freq() == 0));
        assert!(state.bl_tree[..BL_CODES]
            .iter()
            .all(|entry| entry.freq() == 0));
        assert_eq!(state.opt_len, 0);
        assert_eq!(state.static_len, 0);
        assert_eq!(state.sym_next(), 0);
        assert_eq!(state.matches, 0);
    }

    #[test]
    fn init_block_leaves_the_scratch_half_of_a_tree_alone() {
        // The loops stop at the alphabet size, not the array size: the entries above are scratch
        // for the internal nodes `build_tree` creates and are written before they are read.
        let mut state = fresh_state();
        state.dyn_ltree[L_CODES].set_freq(0x1234);
        state.dyn_dtree[D_CODES].set_freq(0x1234);
        state.bl_tree[BL_CODES].set_freq(0x1234);

        init_block(&mut state);

        assert_eq!(state.dyn_ltree[L_CODES].freq(), 0x1234);
        assert_eq!(state.dyn_dtree[D_CODES].freq(), 0x1234);
        assert_eq!(state.bl_tree[BL_CODES].freq(), 0x1234);
    }

    #[test]
    fn smaller_compares_frequencies_first() {
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[3, 7]);

        let view = HeapView::of(&mut state, StaticTreeKind::Distance);
        assert!(view.smaller(0, 1));
        assert!(!view.smaller(1, 0));
    }

    #[test]
    fn smaller_breaks_an_equal_frequency_tie_by_depth_inclusively() {
        // The `<=` of L501, isolated. Equal frequencies and equal depths must compare as smaller in
        // *both* directions; a `<` here would make neither smaller than the other, which is
        // precisely the divergence that changes every emitted block.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[5, 5, 5]);
        state.depth[0] = 0;
        state.depth[1] = 0;
        state.depth[2] = 3;

        let view = HeapView::of(&mut state, StaticTreeKind::Distance);
        assert!(view.smaller(0, 1));
        assert!(view.smaller(1, 0));
        // A shallower subtree wins the tie; a deeper one loses it.
        assert!(view.smaller(0, 2));
        assert!(!view.smaller(2, 0));
    }

    #[test]
    fn the_heap_view_reads_the_tree_that_its_kind_names() {
        // The kind is resolved once, when the view is built, so the resolution has to be the same
        // pairing `DeflateState::tree_for` performs (`trees.c` L459-L466). Distinct frequencies in
        // the three trees make a mis-pairing visible.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Literal, &[11]);
        seed_freqs(&mut state, StaticTreeKind::Distance, &[22]);
        seed_freqs(&mut state, StaticTreeKind::BitLength, &[33]);
        state.heap[1] = 0;
        state.heap_len = 1;

        for (kind, freq) in [
            (StaticTreeKind::Literal, 11),
            (StaticTreeKind::Distance, 22),
            (StaticTreeKind::BitLength, 33),
        ] {
            let view = HeapView::of(&mut state, kind);
            assert_eq!(view.freq_at(0), freq, "{}", kind.c_name());
            assert_eq!(view.heap_at(1), 0, "the heap is shared by every kind");
            assert_eq!(view.heap_len, 1);
            // Out-of-range indices read a default rather than panicking, as the free-standing
            // accessors do.
            assert_eq!(view.freq_at(-1), 0);
            assert_eq!(view.heap_at(-1), 0);
            assert_eq!(view.depth_at(-1), 0);
        }
    }

    #[test]
    fn pqdownheap_heapifies_exactly_as_the_reference_does() {
        // Oracle: frequencies {9,4,7,1,5,3} heapified by
        // `for (n = heap_len/2; n >= 1; n--) pqdownheap(...)` leave heap[1..=6] = 3,1,5,0,4,2.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[9, 4, 7, 1, 5, 3]);
        for node in 0..6 {
            state.depth[node] = 0;
        }
        state.heap_len = 6;
        for node in 0..6 {
            state.heap[node + 1] = i32::try_from(node).unwrap();
        }

        let mut seed = state.heap_len / 2;
        while seed >= 1 {
            pqdownheap(&mut state, StaticTreeKind::Distance, seed);
            seed -= 1;
        }

        assert_eq!(&state.heap[1..=6], &[3, 1, 5, 0, 4, 2]);
    }

    #[test]
    fn gen_codes_produces_a_canonical_prefix_free_code() {
        // Lengths chosen so the Kraft sum is exactly 1: 1, 2, 3, 3.
        let mut state = fresh_state();
        for entry in &mut state.dyn_dtree {
            *entry = CtData::default();
        }
        for (node, len) in [1u16, 2, 3, 3].into_iter().enumerate() {
            state.dyn_dtree[node].set_len(len);
            state.bl_count[usize::from(len)] += 1;
        }

        gen_codes(&mut state, StaticTreeKind::Distance, 3);

        // Canonical construction: first code of length 1 is 0, of length 2 is 10, of length 3 is
        // 110 then 111 -- each stored bit-reversed, because deflate transmits Huffman codes
        // most-significant bit first (`trees.c` L226).
        assert_eq!(codes_of(&state, StaticTreeKind::Distance, 3), [0, 1, 3, 7]);

        // Prefix-freeness, checked independently of the construction: no code is a prefix of
        // another once each is read most-significant bit first.
        let entries: Vec<(u16, u16)> = (0..4)
            .map(|node| {
                let entry = state.dyn_dtree[node];
                (bi_reverse(entry.code(), entry.len()), entry.len())
            })
            .collect();
        for (index, &(code, len)) in entries.iter().enumerate() {
            for (other_index, &(other, other_len)) in entries.iter().enumerate() {
                if index == other_index || len > other_len {
                    continue;
                }
                let shifted = other >> (other_len - len);
                assert_ne!(
                    (code, len),
                    (shifted, len),
                    "code {index} is a prefix of code {other_index}"
                );
            }
        }
    }

    #[test]
    fn gen_codes_leaves_zero_length_symbols_untouched() {
        let mut state = fresh_state();
        for entry in &mut state.dyn_dtree {
            *entry = CtData::default();
        }
        state.dyn_dtree[0].set_len(1);
        state.dyn_dtree[1].set_len(1);
        state.bl_count[1] = 2;
        // A frequency on a zero-length symbol must survive, because `gen_codes` skips it.
        state.dyn_dtree[2] = CtData::new(0xbeef, 0);

        gen_codes(&mut state, StaticTreeKind::Distance, 2);

        assert_eq!(state.dyn_dtree[2].code(), 0xbeef);
    }

    #[test]
    fn build_tree_matches_the_reference_on_a_hand_distribution() {
        // Oracle: distance frequencies {1,1,2,3,5} give lens 4,4,3,2,1 and codes 7,15,3,1,0, with
        // opt_len 30 and static_len 65. Those lengths are also derivable by hand: combining the two
        // 1s, then that node with the 2, then that with the 3, then with the 5 -- and the Kraft sum
        // 2^-4 + 2^-4 + 2^-3 + 2^-2 + 2^-1 is exactly 1.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[1, 1, 2, 3, 5]);

        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 4);
        assert_eq!(
            lens_of(&state, StaticTreeKind::Distance, 4),
            [4, 4, 3, 2, 1]
        );
        assert_eq!(
            codes_of(&state, StaticTreeKind::Distance, 4),
            [7, 15, 3, 1, 0]
        );
        // `opt_len` counts the residues too: symbol 4 carries one extra distance bit, so the cost
        // is 1*4 + 1*4 + 2*3 + 3*2 + 5*(1+1) = 30.
        assert_eq!(state.opt_len, 30);
        // Every static distance code is five bits: 1*5 + 1*5 + 2*5 + 3*5 + 5*(5+1) = 65.
        assert_eq!(state.static_len, 65);
        assert_eq!(&state.bl_count[..5], &[0, 1, 1, 1, 2]);
    }

    #[test]
    fn build_tree_matches_the_reference_when_every_frequency_is_equal() {
        // This is where the `smaller` tie-break is decided (byte-identity point 6): with all
        // frequencies equal, *every* comparison is a tie, so a `<` instead of `<=` shows up
        // immediately as different code lengths.
        //
        // Oracle, six symbols: lens 3,3,2,3,2,3 and codes 1,5,0,3,2,7, opt_len 18, static_len 32.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[1; 6]);

        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 5);
        assert_eq!(
            lens_of(&state, StaticTreeKind::Distance, 5),
            [3, 3, 2, 3, 2, 3]
        );
        assert_eq!(
            codes_of(&state, StaticTreeKind::Distance, 5),
            [1, 5, 0, 3, 2, 7]
        );
        assert_eq!(state.opt_len, 18);
        assert_eq!(state.static_len, 32);
    }

    #[test]
    fn build_tree_matches_the_reference_on_a_full_flat_distance_alphabet() {
        // Oracle, thirty symbols all of frequency 1: two of them come out at four bits and the
        // rest at five, and *which* two -- indices 9 and 20 -- is decided entirely by the tie-break
        // and the heap traversal order.
        let mut state = fresh_state();
        seed_freqs(&mut state, StaticTreeKind::Distance, &[1; 30]);

        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 29);
        assert_eq!(
            lens_of(&state, StaticTreeKind::Distance, 29),
            [
                5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5,
                5, 5
            ]
        );
        assert_eq!(
            codes_of(&state, StaticTreeKind::Distance, 29),
            [
                4, 20, 12, 28, 2, 18, 10, 26, 6, 0, 22, 14, 30, 1, 17, 9, 25, 5, 21, 13, 8, 29, 3,
                19, 11, 27, 7, 23, 15, 31
            ]
        );
        assert_eq!(state.opt_len, 330);
        assert_eq!(state.static_len, 332);
    }

    #[test]
    fn build_tree_forced_two_codes_wraps_both_accumulators_and_cancels() {
        // Byte-identity point 7, and the wrapping-arithmetic requirement, together.
        //
        // After `init_block` an empty block has exactly one non-zero frequency --
        // `dyn_ltree[END_BLOCK]` -- so `heap_len` is 1 and the repair at L655-L661 runs once. The
        // oracle measures `opt_len` at `u64::MAX` and `static_len` at `u64::MAX - 7` immediately
        // after it, then 1 and 7 once `gen_bitlen` has counted the artificial node. If this file
        // ever stops using wrapping arithmetic, the debug build aborts here.
        let mut state = fresh_state();
        assert_eq!(state.dyn_ltree[END_BLOCK].freq(), 1);

        build_tree(&mut state, StaticTreeKind::Literal);

        assert_eq!(state.desc_for(StaticTreeKind::Literal).max_code, 256);
        assert_eq!(state.opt_len, 1);
        assert_eq!(state.static_len, 7);

        // The distance tree of an empty block has *no* non-zero frequency, so the repair runs
        // twice, inventing symbols 0 and 1 and advancing `max_code` from -1 to 1.
        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 1);
        assert_eq!(state.opt_len, 1);
        assert_eq!(state.static_len, 7);
    }

    #[test]
    fn the_forced_two_codes_intermediate_really_does_wrap() {
        // The prologue of `build_tree` reproduced far enough to observe the intermediate the test
        // above can only see the cancellation of. Oracle: `opt_len` is `u64::MAX` and `static_len`
        // is `u64::MAX - 7` at this point, the 7 being `static_ltree[0].Len` of 8 subtracted from
        // zero.
        let mut state = fresh_state();
        let stree = StaticTreeDesc::for_kind(StaticTreeKind::Literal)
            .static_tree
            .unwrap();

        state.heap_len = 0;
        state.heap_max = i32::try_from(HEAP_SIZE).unwrap();
        let mut max_code: i32 = -1;
        for node in 0..L_CODES {
            if state.dyn_ltree[node].freq() == 0 {
                state.dyn_ltree[node].set_len(0);
            } else {
                state.heap_len += 1;
                state.heap[usize::try_from(state.heap_len).unwrap()] = i32::try_from(node).unwrap();
                max_code = i32::try_from(node).unwrap();
                state.depth[node] = 0;
            }
        }
        assert_eq!(state.heap_len, 1, "one non-zero frequency: END_BLOCK");

        while state.heap_len < 2 {
            let node = if max_code < 2 {
                max_code += 1;
                max_code
            } else {
                0
            };
            state.heap_len += 1;
            state.heap[usize::try_from(state.heap_len).unwrap()] = node;
            let index = usize::try_from(node).unwrap();
            state.dyn_ltree[index].set_freq(1);
            state.depth[index] = 0;
            state.opt_len = state.opt_len.wrapping_sub(1);
            state.static_len = state.static_len.wrapping_sub(u64::from(stree[index].len()));
        }

        assert_eq!(
            max_code, 256,
            "max_code is not advanced: it is already >= 2"
        );
        assert_eq!(state.opt_len, u64::MAX);
        assert_eq!(state.static_len, u64::MAX - 7);
    }

    #[test]
    fn gen_bitlen_redistributes_an_overflowing_bit_length_tree() {
        // The bit-length alphabet caps codes at seven bits, so Fibonacci frequencies -- the
        // worst case for Huffman depth -- force the repair loop at L583-L593 and then the full
        // recompute at L600-L612.
        //
        // Oracle: sixteen symbols end at seven bits and three at 3, 2 and 1, with
        // bl_count[1..=7] = 1,1,1,0,0,0,16 and opt_len 72434. `static_len` stays at zero because
        // `static_bl_desc` has no static tree.
        let mut state = fresh_state();
        let freqs = fibonacci_freqs(BL_CODES);
        seed_freqs(&mut state, StaticTreeKind::BitLength, &freqs);

        build_tree(&mut state, StaticTreeKind::BitLength);

        assert_eq!(state.desc_for(StaticTreeKind::BitLength).max_code, 18);
        assert_eq!(
            lens_of(&state, StaticTreeKind::BitLength, 18),
            [7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 3, 2, 1]
        );
        assert_eq!(
            codes_of(&state, StaticTreeKind::BitLength, 18),
            [7, 71, 39, 103, 23, 87, 55, 119, 15, 79, 47, 111, 31, 95, 63, 127, 3, 1, 0]
        );
        assert_eq!(&state.bl_count[..8], &[0, 1, 1, 1, 0, 0, 0, 16]);
        assert_eq!(state.opt_len, 72_434);
        assert_eq!(state.static_len, 0);
    }

    #[test]
    fn gen_bitlen_redistributes_an_overflowing_distance_tree() {
        // The same worst case against a fifteen-bit cap: the overflow repair fires, and the
        // recompute pass walks the heap while skipping internal nodes without consuming a leaf --
        // the `continue` at L604 that deliberately steps over the `n--`.
        //
        // Oracle: opt_len 1605861, static_len 1897694.
        let mut state = fresh_state();
        let freqs = fibonacci_freqs(25);
        seed_freqs(&mut state, StaticTreeKind::Distance, &freqs);

        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 24);
        assert_eq!(
            lens_of(&state, StaticTreeKind::Distance, 24),
            [
                15, 15, 15, 15, 15, 15, 15, 15, 14, 14, 13, 11, 10, 9, 8, 7, 6, 5, 4, 4, 3, 3, 2,
                2, 4
            ]
        );
        assert_eq!(
            codes_of(&state, StaticTreeKind::Distance, 24),
            [
                4095, 20479, 12287, 28671, 8191, 24575, 16383, 32767, 6143, 14335, 2047, 1023, 511,
                255, 127, 63, 31, 15, 3, 11, 1, 5, 0, 2, 7
            ]
        );
        assert_eq!(state.opt_len, 1_605_861);
        assert_eq!(state.static_len, 1_897_694);
        // Every code fits the cap the descriptor declares.
        assert!(lens_of(&state, StaticTreeKind::Distance, 24)
            .iter()
            .all(|&len| usize::from(len) <= MAX_BITS));
    }

    /// The mixed run pattern both tree-transmission tests use: long zero runs, a medium run of
    /// sevens, a short run of nines, and two singletons. It exercises every arm of the ladder --
    /// literal repeats, `REP_3_6`, `REPZ_3_10` and `REPZ_11_138`.
    fn seed_mixed_literal_lengths(state: &mut DeflateState<'static, GlobalAllocator>) -> i32 {
        for node in 0..L_CODES + 2 {
            state.dyn_ltree[node].set_len(0);
        }
        for node in 40..60 {
            state.dyn_ltree[node].set_len(7);
        }
        for node in 60..63 {
            state.dyn_ltree[node].set_len(9);
        }
        state.dyn_ltree[200].set_len(5);
        state.dyn_ltree[201].set_len(6);
        for node in 0..BL_CODES {
            state.bl_tree[node].set_freq(0);
        }
        201
    }

    #[test]
    fn scan_tree_accumulates_the_reference_frequencies_and_writes_the_guard() {
        // Oracle: bl_tree frequencies 0,0,0,0,0,1,1,2,0,3,0,...,0,3,0,2 and a guard of 0xffff at
        // `dyn_ltree[max_code + 1]`.
        let mut state = fresh_state();
        let max_code = seed_mixed_literal_lengths(&mut state);

        scan_tree(&mut state, StaticTreeKind::Literal, max_code);

        let freqs: Vec<u16> = (0..BL_CODES)
            .map(|node| state.bl_tree[node].freq())
            .collect();
        assert_eq!(
            freqs,
            [0, 0, 0, 0, 0, 1, 1, 2, 0, 3, 0, 0, 0, 0, 0, 0, 3, 0, 2]
        );
        assert_eq!(state.dyn_ltree[202].len(), 0xffff, "the L722 guard");
    }

    #[test]
    fn the_guard_write_stays_inside_every_tree_array() {
        // The guard goes one element past `max_code`, so each array must be long enough at its own
        // largest `max_code`. Written as a positive check on the widest case of each tree rather
        // than as a comment, because an out-of-range write here would silently be a no-op.
        let mut state = fresh_state();

        for node in 0..L_CODES + 2 {
            state.dyn_ltree[node].set_len(1);
        }
        scan_tree(
            &mut state,
            StaticTreeKind::Literal,
            i32::try_from(L_CODES).unwrap() - 1,
        );
        assert_eq!(state.dyn_ltree[L_CODES].len(), 0xffff);

        for node in 0..=D_CODES {
            state.dyn_dtree[node].set_len(1);
        }
        scan_tree(
            &mut state,
            StaticTreeKind::Distance,
            i32::try_from(D_CODES).unwrap() - 1,
        );
        assert_eq!(state.dyn_dtree[D_CODES].len(), 0xffff);

        for node in 0..=BL_CODES {
            state.bl_tree[node].set_len(1);
        }
        scan_tree(
            &mut state,
            StaticTreeKind::BitLength,
            i32::try_from(BL_CODES).unwrap() - 1,
        );
        assert_eq!(state.bl_tree[BL_CODES].len(), 0xffff);
    }

    #[test]
    fn send_tree_emits_the_reference_bytes_for_the_runs_scan_tree_counted() {
        // The agreement check: the same tree, scanned and then sent, must emit exactly what the C
        // oracle emits -- 10 bytes -- using bit-length codes derived from nothing but the run
        // structure. Oracle: a9,c3,c3,e1,30,97,52,26,3f,65.
        let mut state = fresh_state();
        let max_code = seed_mixed_literal_lengths(&mut state);

        scan_tree(&mut state, StaticTreeKind::Literal, max_code);
        trivial_bl_codes(&mut state);
        state.pending.reset_pending();
        state.bi_buf = 0;
        state.bi_valid = 0;

        send_tree(&mut state, StaticTreeKind::Literal, max_code);
        bi_windup(&mut state);

        assert_eq!(
            state.pending.written(),
            [0xa9, 0xc3, 0xc3, 0xe1, 0x30, 0x97, 0x52, 0x26, 0x3f, 0x65]
        );
    }

    #[test]
    fn send_all_trees_emits_the_reference_bytes() {
        // Oracle: 53 bytes for a 257-symbol literal tree of uniform nine-bit codes, a full
        // distance tree of five-bit codes, and all nineteen bit-length codes sent.
        let mut state = fresh_state();
        for node in 0..L_CODES + 2 {
            state.dyn_ltree[node].set_len(0);
        }
        for node in 0..257 {
            state.dyn_ltree[node].set_len(9);
        }
        for node in 0..D_CODES {
            state.dyn_dtree[node].set_len(5);
        }
        for node in 0..BL_CODES {
            state.bl_tree[node].set_freq(0);
        }
        scan_tree(&mut state, StaticTreeKind::Literal, 256);
        scan_tree(&mut state, StaticTreeKind::Distance, 29);
        trivial_bl_codes(&mut state);
        state.pending.reset_pending();
        state.bi_buf = 0;
        state.bi_valid = 0;

        send_all_trees(&mut state, 257, 30, 19);
        bi_windup(&mut state);

        assert_eq!(
            state.pending.written(),
            [
                0xa0, 0x7f, 0xdb, 0xb6, 0x6d, 0xdb, 0xb6, 0x6d, 0x5b, 0x19, 0x0e, 0x87, 0xc3, 0xe1,
                0x70, 0x38, 0x1c, 0x0e, 0x87, 0xc3, 0xe1, 0x70, 0x38, 0x1c, 0x0e, 0x87, 0xc3, 0xe1,
                0x70, 0x38, 0x1c, 0x0e, 0x87, 0xc3, 0xe1, 0x70, 0x38, 0x1c, 0x0e, 0x87, 0xc3, 0xe1,
                0x70, 0x38, 0x1c, 0x0e, 0x87, 0x68, 0x38, 0x1c, 0x0e, 0x07, 0x01
            ]
        );
    }

    #[test]
    fn build_bl_tree_matches_the_reference_for_an_empty_block() {
        // Oracle: an empty block leaves opt_len at 89, static_len at 7 and max_blindex at 17.
        let mut state = fresh_state();
        build_tree(&mut state, StaticTreeKind::Literal);
        build_tree(&mut state, StaticTreeKind::Distance);

        let max_blindex = build_bl_tree(&mut state);

        assert_eq!(max_blindex, 17);
        assert_eq!(state.opt_len, 89);
        assert_eq!(state.static_len, 7);
        // The floor of 3 is never reached here, and the code the search stopped on is really used.
        assert!(max_blindex >= 3);
        let code = usize::from(bl_order[usize::try_from(max_blindex).unwrap()]);
        assert_ne!(state.bl_tree[code].len(), 0);
    }

    #[test]
    fn d_code_matches_the_reference_across_both_halves_of_the_table() {
        // Oracle values, chosen either side of the 256 boundary and at the extremes.
        for (dist, expected) in [
            (0u32, 0usize),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 4),
            (255, 15),
            (256, 16),
            (257, 16),
            (258, 16),
            (512, 18),
            (1024, 20),
            (4095, 23),
            (16383, 27),
            (32767, 29),
        ] {
            assert_eq!(d_code(dist), expected, "d_code({dist})");
        }
    }

    #[test]
    fn d_code_never_leaves_the_distance_alphabet() {
        // Over the whole admissible range -- a distance is at most `MAX_DIST`, so `dist` here is at
        // most 32767 -- the code must stay below `D_CODES`, which is the `Assert` at L931.
        for dist in 0..32768u32 {
            assert!(d_code(dist) < D_CODES, "d_code({dist}) out of range");
        }
    }

    #[test]
    fn compress_block_of_an_empty_symbol_buffer_emits_only_end_of_block() {
        // The zero test at L908 is what makes this correct: with no symbols, the body must not run
        // at all, leaving just the seven-bit static end-of-block code. Oracle: one byte, 0x00.
        let mut state = fresh_state();

        compress_block(&mut state, BlockTrees::Static);
        bi_windup(&mut state);

        assert_eq!(state.pending.written(), [0x00]);
    }

    #[test]
    fn compress_block_with_the_static_trees_emits_the_reference_bytes() {
        // A mix that reaches every arm: two literals, a short match, the longest possible match at
        // the greatest possible distance, the shortest match at distance 1, and a mid-range match
        // with residues on both the length and the distance.
        //
        // Oracle: 89,60,54,35,7a,ff,3f,10,d0,29,04,00.
        let mut state = fresh_state();
        for (dist, len_or_lit) in [
            (0u16, b'a'),
            (3, 2),
            (0, b'z'),
            (32768, 255),
            (1, 0),
            (258, 100),
        ] {
            assert!(state.pending.push_symbol(dist, len_or_lit).is_some());
        }

        compress_block(&mut state, BlockTrees::Static);
        bi_windup(&mut state);

        assert_eq!(
            state.pending.written(),
            [0x89, 0x60, 0x54, 0x35, 0x7a, 0xff, 0x3f, 0x10, 0xd0, 0x29, 0x04, 0x00]
        );
    }

    #[test]
    fn compress_block_with_the_block_trees_emits_the_reference_bytes() {
        // The dynamic path, driven end to end: tally the symbols as `_tr_tally` would, build all
        // three trees, then emit. Oracle: data_type 1, opt_len 147, static_len 65, l_max 266,
        // d_max 4, max_blindex 17, and the body 26,2e,05.
        let mut state = fresh_state();
        for (dist, len_or_lit) in [
            (0u16, b'a'),
            (3, 2),
            (0, b'z'),
            (0, b'a'),
            (5, 10),
            (0, b'a'),
        ] {
            assert!(state.pending.push_symbol(dist, len_or_lit).is_some());
            if dist == 0 {
                state.dyn_ltree[usize::from(len_or_lit)].increment_freq();
            } else {
                state.matches += 1;
                let code =
                    usize::from(crate::trees::static_tables::_length_code[usize::from(len_or_lit)]);
                state.dyn_ltree[code + LITERALS + 1].increment_freq();
                state.dyn_dtree[d_code(u32::from(dist) - 1)].increment_freq();
            }
        }

        assert_eq!(detect_data_type(&state), Z_TEXT);

        build_tree(&mut state, StaticTreeKind::Literal);
        build_tree(&mut state, StaticTreeKind::Distance);
        let max_blindex = build_bl_tree(&mut state);

        assert_eq!(state.desc_for(StaticTreeKind::Literal).max_code, 266);
        assert_eq!(state.desc_for(StaticTreeKind::Distance).max_code, 4);
        assert_eq!(max_blindex, 17);
        assert_eq!(state.opt_len, 147);
        assert_eq!(state.static_len, 65);

        let selection = select_block_type(
            state.opt_len,
            state.static_len,
            32,
            state.level(),
            state.strategy(),
            true,
        );
        assert_eq!(selection.opt_lenb, 9);
        assert_eq!(selection.static_lenb, 9);

        state.pending.reset_pending();
        state.bi_buf = 0;
        state.bi_valid = 0;
        compress_block(&mut state, BlockTrees::Dynamic);
        bi_windup(&mut state);

        assert_eq!(state.pending.written(), [0x26, 0x2e, 0x05]);
    }

    /// Makes `byte` the block's only literal, on top of the end-of-block frequency `init_block`
    /// always sets. `END_BLOCK` is 256 and the scans stop at `LITERALS` = 256, so it cannot affect
    /// the answer.
    ///
    /// Mutates a state rather than returning a new one so that a sweep over many bytes needs one
    /// allocation instead of one per byte. That matters: a `DeflateState` owns a 32 KiB window and
    /// a 64 KiB pending buffer, and this module's tests are run under Miri, where allocating and
    /// filling those hundreds of times dominates everything else. The assertions are unchanged --
    /// each byte is still tested entirely on its own, because the reset below is exactly
    /// `init_block`'s.
    fn set_only_literal(state: &mut DeflateState<'static, GlobalAllocator>, byte: usize) {
        for entry in state.dyn_ltree.iter_mut().take(L_CODES) {
            entry.set_freq(0);
        }
        state.dyn_ltree[END_BLOCK].set_freq(1);
        state.dyn_ltree[byte].set_freq(1);
    }

    /// Asserts the classification of each byte in `bytes`, each one tested alone.
    fn assert_each_byte_alone(bytes: impl IntoIterator<Item = usize>, expected: i32) {
        let mut state = fresh_state();
        for byte in bytes {
            set_only_literal(&mut state, byte);
            assert_eq!(
                detect_data_type(&state),
                expected,
                "byte {byte} alone in the block"
            );
        }
    }

    #[test]
    fn detect_data_type_reports_binary_for_every_block_listed_byte() {
        // The block list is 0..6, 14..25 and 28..31 -- the bits of the 0xf3ffc07f mask.
        assert_each_byte_alone((0..=6).chain(14..=25).chain(28..=31), Z_BINARY);
    }

    #[test]
    fn detect_data_type_reports_binary_for_each_gray_listed_byte_alone() {
        // 7 BEL, 8 BS, 11 VT, 12 FF, 26 SUB, 27 ESC are ignored by the algorithm, so a block made
        // only of them has no allow-listed byte and falls through to binary.
        assert_each_byte_alone([7, 8, 11, 12, 26, 27], Z_BINARY);
    }

    #[test]
    fn detect_data_type_reports_text_for_tab_line_feed_and_carriage_return() {
        assert_each_byte_alone([9, 10, 13], Z_TEXT);
    }

    #[test]
    fn detect_data_type_reports_text_for_every_byte_from_thirty_two_upwards() {
        assert_each_byte_alone(32..LITERALS, Z_TEXT);
    }

    #[test]
    fn detect_data_type_reports_binary_for_an_empty_block() {
        assert_eq!(detect_data_type(&fresh_state()), Z_BINARY);
    }

    #[test]
    fn detect_data_type_lets_one_block_listed_byte_outweigh_any_amount_of_text() {
        // The block-list scan runs first and returns immediately, so a single control character
        // makes the whole block binary however much text accompanies it.
        let mut state = fresh_state();
        for byte in 32..LITERALS {
            state.dyn_ltree[byte].set_freq(9);
        }
        assert_eq!(detect_data_type(&state), Z_TEXT);
        state.dyn_ltree[0].set_freq(1);
        assert_eq!(detect_data_type(&state), Z_BINARY);
    }

    #[test]
    fn detect_data_type_shifts_the_mask_on_every_iteration() {
        // If the mask were not shifted in step with the byte index, the gray-listed bytes would
        // start reporting binary and the block-listed ones would stop. Checking all 32 low bytes
        // one at a time pins the alignment exactly.
        let expected: [i32; 32] = [
            Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY,
            Z_BINARY, Z_TEXT, Z_TEXT, Z_BINARY, Z_BINARY, Z_TEXT, Z_BINARY, Z_BINARY, Z_BINARY,
            Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY,
            Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY, Z_BINARY,
        ];
        let mut state = fresh_state();
        for (byte, &want) in expected.iter().enumerate() {
            set_only_literal(&mut state, byte);
            assert_eq!(detect_data_type(&state), want, "byte {byte}");
        }
    }

    #[test]
    fn select_block_type_matches_the_reference_at_every_boundary() {
        // Every row is an oracle measurement of the C expressions at L1027-L1074, chosen to sit
        // exactly on a boundary or one step either side of it.
        let cases = [
            // static_lenb == opt_lenb exactly -> static.
            (
                70u64,
                70u64,
                100u64,
                6i32,
                Strategy::Default,
                true,
                BlockChoice::Static,
                10u64,
                10u64,
            ),
            // static_lenb one byte above opt_lenb -> the dynamic trees are worth transmitting.
            (
                70,
                78,
                100,
                6,
                Strategy::Default,
                true,
                BlockChoice::Dynamic,
                10,
                11,
            ),
            // static_lenb one byte below -> the substitution at L1037 fires.
            (
                70,
                62,
                100,
                6,
                Strategy::Default,
                true,
                BlockChoice::Static,
                9,
                9,
            ),
            // Z_FIXED forbids dynamic trees, so the substitution fires even though static is dearer.
            (
                70,
                78,
                100,
                6,
                Strategy::Fixed,
                true,
                BlockChoice::Static,
                11,
                11,
            ),
            // stored_len + 4 == opt_lenb exactly -> stored, because the test is `<=`.
            (
                70,
                70,
                6,
                6,
                Strategy::Default,
                true,
                BlockChoice::Stored,
                10,
                10,
            ),
            // one byte more of payload and storing no longer pays.
            (
                70,
                70,
                7,
                6,
                Strategy::Default,
                true,
                BlockChoice::Static,
                10,
                10,
            ),
            // one byte less and it still does.
            (
                70,
                70,
                5,
                6,
                Strategy::Default,
                true,
                BlockChoice::Stored,
                10,
                10,
            ),
            // the same block with no window bytes available cannot be stored at all.
            (
                70,
                70,
                6,
                6,
                Strategy::Default,
                false,
                BlockChoice::Static,
                10,
                10,
            ),
            // level 0 forces a stored block, whatever the accumulators say.
            (
                0,
                0,
                0,
                0,
                Strategy::Default,
                true,
                BlockChoice::Stored,
                5,
                5,
            ),
            (
                0,
                0,
                40,
                0,
                Strategy::Default,
                true,
                BlockChoice::Stored,
                45,
                45,
            ),
            // the empty block, measured before and after `build_bl_tree` adds the header cost.
            (
                1,
                7,
                0,
                6,
                Strategy::Default,
                true,
                BlockChoice::Dynamic,
                1,
                2,
            ),
            (
                89,
                7,
                0,
                6,
                Strategy::Default,
                true,
                BlockChoice::Static,
                2,
                2,
            ),
        ];

        for (
            opt_len,
            static_len,
            stored_len,
            level,
            strategy,
            buf,
            choice,
            opt_lenb,
            static_lenb,
        ) in cases
        {
            let selection =
                select_block_type(opt_len, static_len, stored_len, level, strategy, buf);
            assert_eq!(
                (selection.choice, selection.opt_lenb, selection.static_lenb),
                (choice, opt_lenb, static_lenb),
                "opt_len={opt_len} static_len={static_len} stored_len={stored_len} \
                 level={level} strategy={strategy:?} buf={buf}"
            );
        }
    }

    #[test]
    fn select_block_type_rounds_bits_up_to_bytes_by_truncating_shift() {
        // `(len + 3 + 7) >> 3` pays for the three-bit header and rounds up, and it truncates. Any
        // rewrite as a division would move one of these answers, and the answer is the block type.
        for (bits, bytes) in [
            (0u64, 1u64),
            (1, 1),
            (5, 1),
            (6, 2),
            (13, 2),
            (14, 3),
            (53, 7),
        ] {
            // `stored_len` is large enough that storing can never win, so only the shift is under
            // test here.
            let selection = select_block_type(bits, bits, 1_000_000, 6, Strategy::Default, true);
            assert_eq!(selection.opt_lenb, bytes, "{bits} bits");
            assert_eq!(selection.static_lenb, bytes, "{bits} bits");
            assert_eq!(selection.choice, BlockChoice::Static, "{bits} bits");
        }
    }

    #[test]
    fn empty_block_reaches_the_measured_opt_lenb_of_one_and_static_lenb_of_two() {
        // The regression test the wrapping-arithmetic requirement asks for, driven end to end: an
        // empty block, both trees built, then the byte counts the reference computes from them.
        // Non-wrapping arithmetic aborts before reaching the assertions; arithmetic that wraps but
        // does not cancel reaches them with the wrong values.
        let mut state = fresh_state();
        build_tree(&mut state, StaticTreeKind::Literal);
        build_tree(&mut state, StaticTreeKind::Distance);

        assert_eq!(state.opt_len, 1);
        assert_eq!(state.static_len, 7);

        let selection = select_block_type(
            state.opt_len,
            state.static_len,
            0,
            state.level(),
            state.strategy(),
            true,
        );
        assert_eq!(selection.opt_lenb, 1);
        assert_eq!(selection.static_lenb, 2);
    }

    #[test]
    fn an_empty_final_block_is_sent_with_the_static_trees() {
        // Carried through `build_bl_tree`, which is what `_tr_flush_block` actually does, the same
        // empty block selects a static block whose three header bits are 0b011 -- and `03 00` is
        // exactly what the reference library emits when a stream is finished with no input.
        let mut state = fresh_state();
        build_tree(&mut state, StaticTreeKind::Literal);
        build_tree(&mut state, StaticTreeKind::Distance);
        build_bl_tree(&mut state);

        let selection = select_block_type(
            state.opt_len,
            state.static_len,
            0,
            state.level(),
            state.strategy(),
            true,
        );
        assert_eq!(selection.choice, BlockChoice::Static);
        // `opt_len` of 89 gives a raw `opt_lenb` of 12, but `static_lenb` of 2 is cheaper, so the
        // substitution at L1037 replaces it. `BlockSelection::opt_lenb` reports the value C's
        // variable holds *after* that assignment, which is what the two predicates below it read.
        assert_eq!(state.opt_len, 89);
        assert_eq!((state.opt_len + 3 + 7) >> 3, 12);
        assert_eq!(selection.opt_lenb, 2);
        assert_eq!(selection.static_lenb, 2);
        assert_eq!(selection.choice.header_bits(true), 0b011);
    }

    #[test]
    fn block_choice_reports_the_reference_type_codes_and_header_bits() {
        assert_eq!(BlockChoice::Stored.block_type(), STORED_BLOCK);
        assert_eq!(BlockChoice::Static.block_type(), STATIC_TREES);
        assert_eq!(BlockChoice::Dynamic.block_type(), DYN_TREES);

        // `(BLOCK_TYPE << 1) + last`, the expression at L862, L1059 and L1066.
        for choice in [
            BlockChoice::Stored,
            BlockChoice::Static,
            BlockChoice::Dynamic,
        ] {
            let base = u16::from(choice.block_type()) << 1;
            assert_eq!(choice.header_bits(false), base);
            assert_eq!(choice.header_bits(true), base + 1);
        }

        assert_eq!(BlockChoice::Stored.trees(), None);
        assert_eq!(BlockChoice::Static.trees(), Some(BlockTrees::Static));
        assert_eq!(BlockChoice::Dynamic.trees(), Some(BlockTrees::Dynamic));
    }
}
