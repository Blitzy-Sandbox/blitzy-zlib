//! The three static tree descriptors, transcribed from `trees.c` L117-L139.
//!
//! A descriptor is the *constant* half of one of the Huffman coder's three
//! trees. It pairs the tree's alphabet size and its width limit with the
//! extra-bit table whose entries say how wide a residue each symbol carries,
//! and -- for the two alphabets RFC 1951 3.2.6 supplies fixed codes for -- with
//! those codes. None of it changes while a stream is compressed.
//!
//! An unqualified line reference in this file is a line of `trees.c`.
//!
//! # Who reads a descriptor, and what breaks if a field is wrong
//!
//! * `_tr_init` (L456-L478) attaches one descriptor to each of the three trees
//!   of a fresh stream, with `s->l_desc.stat_desc = &static_l_desc` and its two
//!   siblings (L459-L466). [`StaticTreeDesc::for_kind`] is that binding here.
//! * `build_tree` (L627-L706) reads `elems` to decide how much of the alphabet
//!   to scan for non-zero frequencies (L641), and reads `static_tree` in the
//!   forced-two-codes repair at L655-L661.
//! * `gen_bitlen` (L540-L613) reads all five: `extra_bits` and `extra_base` give
//!   it the residue width of a symbol (L572), `max_length` caps a code and
//!   counts an overflow (L564), and `static_tree` decides whether the block's
//!   `static_len` is accumulated at all (L575).
//!
//! Those two accumulators are what makes this file load-bearing rather than
//! merely descriptive. `_tr_flush_block` turns `opt_len` and `static_len` into
//! byte counts (L1027-L1028) and then picks the cheaper of a dynamic and a
//! static block from the comparison `static_lenb <= opt_lenb` (L1035). A
//! descriptor field that is off by one therefore does not produce a slightly
//! worse block: it changes the code widths, or the block type, or both, for
//! every block the encoder emits.
//!
//! # Relationship to the two C types with similar names
//!
//! `deflate.h` L88 is `typedef struct static_tree_desc_s static_tree_desc;` --
//! an alias for the struct declared at L117-L123 of `trees.c`, which is
//! [`StaticTreeDesc`] here.
//!
//! `deflate.h` L90-L94 declares a **different** type, `tree_desc_s`, holding
//! `dyn_tree`, `max_code` and `stat_desc`. That is the *mutable* half of a tree
//! and is owned by [`crate::deflate::state::TreeDesc`] in `deflate/state.rs`, which the compressor
//! embeds three times as its `l_desc`, `d_desc` and `bl_desc` fields. It is not
//! redefined here, and neither is [`CtData`]: this module contributes only the
//! `'static` data.
//!
//! # No initialisation step
//!
//! C guards its descriptors with `TCONST` (L125-L129), which drops the `const`
//! qualifier when `NO_INIT_GLOBAL_POINTERS` is defined, because a few
//! link-time environments cannot place an initialised pointer in read-only
//! memory. That distinction has no Rust analogue and no observable effect, so
//! the descriptors below are plain `const` items. Nothing in this module is
//! `static mut`, lazily built or otherwise observable as state.

use crate::deflate::state::{
    CtData, StaticTreeKind, BL_CODES, D_CODES, LITERALS, L_CODES, MAX_BITS,
};
use crate::trees::static_tables::{
    extra_blbits, extra_dbits, extra_lbits, static_dtree, static_ltree, MAX_BL_BITS,
};

/// The constant description of one Huffman tree.
///
/// Mirrors `struct static_tree_desc_s` (L117-L123).
///
/// The three instances are [`STATIC_L_DESC`], [`STATIC_D_DESC`] and
/// [`STATIC_BL_DESC`]; there are never any others, and none is ever mutated.
///
/// # Integer widths
///
/// C declares the last three members `int`. They are `usize` here because every
/// use of them is an index or an index bound: `elems` bounds the loop
/// `for (n = 0; n < elems; n++)` over a tree array (L641), `extra_base` is
/// subtracted from a tree index to index [`Self::extra_bits`] (L572), and
/// `max_length` indexes `DeflateState::bl_count` (L588) and is compared against
/// a code width (L564). That also matches the widths the implementation already uses for
/// [`MAX_BITS`] and [`MAX_BL_BITS`], so no descriptor field needs a cast the
/// reference does not also perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StaticTreeDesc {
    /// The fixed codes for this alphabet, or [`None`] when it has none (L118).
    ///
    /// The null case is a live branch, not a defensive one, and it is the whole
    /// reason this is an [`Option`] rather than a slice. Two sites test it:
    ///
    /// * `gen_bitlen` L575:
    ///   `if (stree) s->static_len += (ulg)f * (unsigned)(stree[n].Len + xbits);`
    /// * `build_tree` L659:
    ///   `s->opt_len--; if (stree) s->static_len -= stree[node].Len;`
    ///
    /// Both read the *length* of the static code for a symbol, through
    /// [`CtData::len`]. A descriptor with no static tree contributes nothing to
    /// `static_len`, which is exactly why the bit-length tree can never be sent
    /// with static codes -- see [`STATIC_BL_DESC`].
    pub(crate) static_tree: Option<&'static [CtData]>,

    /// How many extra bits each symbol of this alphabet carries (L119). The element type
    /// is `i32` because C's `intf` is `int` (`zconf.h` L415).
    ///
    /// Deliberately **not** an [`Option`], even though the C comment reads
    /// "extra bits for each code or NULL". All three descriptors supply a
    /// table -- [`extra_lbits`], [`extra_dbits`] and [`extra_blbits`] -- and
    /// `gen_bitlen` dereferences the member unconditionally once its index
    /// guard passes (L572), so the null case the comment allows for never
    /// occurs and no consumer tests for it. Modelling it would add an
    /// unwrapping step at the one hot site that reads the table.
    ///
    /// One slice type serves all three because C reaches all three tables
    /// through this single `const intf *` member; that is why
    /// `trees/static_tables.rs` gives them a common `i32` element type.
    pub(crate) extra_bits: &'static [i32],

    /// The first symbol of this alphabet that can carry extra bits (L120): the base index
    /// `extra_bits` is offset by.
    ///
    /// `gen_bitlen` reads `extra_bits[n - extra_base]` for a symbol `n`, and
    /// only when `n >= extra_base` (L571-L572); a symbol below the base carries
    /// no residue and contributes `xbits = 0`. The literal/length alphabet is
    /// the only one with a non-zero base, because its first 257 symbols are the
    /// 256 literals plus the end-of-block code and none of those is a length.
    pub(crate) extra_base: usize,

    /// The number of symbols in this alphabet (L121), which is the most a tree over it can
    /// hold.
    ///
    /// `build_tree` scans exactly this many entries of the dynamic tree for
    /// non-zero frequencies (L641) and starts numbering the internal nodes it
    /// creates at this value (L672), so it also fixes where the leaf half of
    /// the tree array ends. It is smaller than the array it describes:
    /// `dyn_ltree` holds `HEAP_SIZE` entries because the internal nodes are
    /// stored above the leaves.
    pub(crate) elems: usize,

    /// The widest code this alphabet permits, in bits (L122).
    ///
    /// `gen_bitlen` clamps any deeper code to this and counts the clamp as an
    /// overflow (L564); a non-zero overflow count then runs the repair loop at
    /// L583-L593, which redistributes leaves until every code fits. The
    /// literal/length and distance alphabets allow [`MAX_BITS`] of 15, the
    /// ceiling RFC 1951 3.2.7 sets for them; the bit-length alphabet allows
    /// only [`MAX_BL_BITS`] of 7, because its widths are transmitted in
    /// three-bit fields.
    pub(crate) max_length: usize,
}

impl StaticTreeDesc {
    /// The descriptor `kind` names.
    ///
    /// This is the Rust form of the three assignments `_tr_init` makes at
    /// L459-L466, where C stores `&static_l_desc`, `&static_d_desc` or
    /// `&static_bl_desc` into a tree's `stat_desc` member. [`crate::deflate::state::TreeDesc`] records
    /// a [`StaticTreeKind`] instead of a pointer -- naming the descriptor rather
    /// than pointing at it is what removes that aliasing from the implementation -- so
    /// this is the lookup that turns the recorded name back into the data.
    ///
    /// Returned by value: a descriptor is [`Copy`] and holds nothing but two
    /// slice references and three indices, and C's indirection exists to share
    /// one instance between streams rather than to allow mutation through it.
    #[must_use]
    pub(crate) const fn for_kind(kind: StaticTreeKind) -> Self {
        match kind {
            StaticTreeKind::Literal => STATIC_L_DESC,
            StaticTreeKind::Distance => STATIC_D_DESC,
            StaticTreeKind::BitLength => STATIC_BL_DESC,
        }
    }
}

// Every field is written as the expression the C initialiser uses -- `LITERALS +
// 1` rather than 257, `L_CODES` rather than 286 -- so that a change to a shared
// sizing constant propagates here exactly as the preprocessor propagates it
// there, and so that each line reads as a transliteration of the one it came
// from. The literal values those expressions evaluate to are pinned separately,
// by the assertions further down.

/// The literal/length tree. Mirrors `static_l_desc` (L131-L132), whose five fields are
/// `static_ltree`, `extra_lbits`, an `extra_base` of `LITERALS + 1`, `L_CODES` elements and a
/// `max_length` of `MAX_BITS`.
///
/// Describes the alphabet RFC 1951 3.2.5 tabulates: the 256 literal bytes, the
/// end-of-block code, and the 29 length codes, for [`L_CODES`] = 286 symbols.
/// The `extra_base` of `LITERALS + 1` = 257 skips the literals and the
/// end-of-block code, which carry no residue, so that [`extra_lbits`] -- 29
/// entries, one per length code -- is indexed from its own zero.
///
/// Its static tree is [`static_ltree`], the fixed literal/length codes of
/// RFC 1951 3.2.6.
pub(crate) const STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&static_ltree),
    extra_bits: &extra_lbits,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// The distance tree. Mirrors `static_d_desc` (L134-L135): `static_dtree`, `extra_dbits`, an
/// `extra_base` of `0`, `D_CODES` elements and a `max_length` of `MAX_BITS`.
///
/// Describes the [`D_CODES`] = 30 distance codes. Its `extra_base` is zero
/// because every symbol of this alphabet is a distance code and so may carry a
/// residue; [`extra_dbits`] therefore has one entry per symbol and is indexed
/// directly.
///
/// Its static tree is [`static_dtree`], which RFC 1951 3.2.6 makes trivial --
/// all thirty codes are five bits wide.
pub(crate) const STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: Some(&static_dtree),
    extra_bits: &extra_dbits,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// The bit-length tree. Mirrors `static_bl_desc` (L137-L138): a null static tree,
/// `extra_blbits`, an `extra_base` of `0`, `BL_CODES` elements and a `max_length` of
/// `MAX_BL_BITS`.
///
/// Describes the [`BL_CODES`] = 19 codes used to *transmit* the widths of the
/// other two trees: the sixteen literal widths 0 through 15 plus the three
/// repeat codes of RFC 1951 3.2.7, whose residues are the non-zero tail of
/// [`extra_blbits`]. Its `extra_base` is zero for the same reason as the
/// distance tree's.
///
/// # Two ways this descriptor differs from its siblings
///
/// Its `static_tree` is **[`None`]**, from C's explicit `(const ct_data *)0`.
/// RFC 1951 defines fixed codes for the literal/length and distance alphabets
/// only, so this alphabet has no static counterpart, and there is nothing for
/// `gen_bitlen` L575 or `build_tree` L659 to charge to `static_len`. That is
/// consistent rather than merely permitted: the bit-length tree exists only to
/// describe a *dynamic* block's two trees, and a static block transmits no
/// trees at all, so a static cost for it would be a cost that no encoding can
/// ever pay.
///
/// Its `max_length` is [`MAX_BL_BITS`] = 7 rather than [`MAX_BITS`] = 15,
/// because RFC 1951 3.2.7 sends each of these widths in a three-bit field.
/// Seven is the tighter bound that makes `gen_bitlen`'s overflow-repair loop
/// (L583-L593) reachable in practice.
pub(crate) const STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &extra_blbits,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

// Each field above tracks a shared sizing constant; these assertions pin the
// resulting values to the numbers `trees.c` L131-L138 and `deflate.h` L34-L52
// actually produce, so that a drifting constant fails the build rather than the
// stream.
const _: () = assert!(STATIC_L_DESC.extra_base == LITERALS + 1);
const _: () = assert!(STATIC_L_DESC.extra_base == 257);
const _: () = assert!(STATIC_L_DESC.elems == L_CODES);
const _: () = assert!(STATIC_L_DESC.elems == 286);
const _: () = assert!(STATIC_L_DESC.max_length == MAX_BITS);
const _: () = assert!(STATIC_L_DESC.max_length == 15);

const _: () = assert!(STATIC_D_DESC.extra_base == 0);
const _: () = assert!(STATIC_D_DESC.elems == D_CODES);
const _: () = assert!(STATIC_D_DESC.elems == 30);
const _: () = assert!(STATIC_D_DESC.max_length == MAX_BITS);
const _: () = assert!(STATIC_D_DESC.max_length == 15);

const _: () = assert!(STATIC_BL_DESC.extra_base == 0);
const _: () = assert!(STATIC_BL_DESC.elems == BL_CODES);
const _: () = assert!(STATIC_BL_DESC.elems == 19);
const _: () = assert!(STATIC_BL_DESC.max_length == MAX_BL_BITS);
const _: () = assert!(STATIC_BL_DESC.max_length == 7);

// The presence or absence of a static tree is the one field that changes control
// flow, so it is pinned in both directions.
const _: () = assert!(STATIC_L_DESC.static_tree.is_some());
const _: () = assert!(STATIC_D_DESC.static_tree.is_some());
const _: () = assert!(STATIC_BL_DESC.static_tree.is_none());

// Every extra-bit table is exactly as long as the range of symbols that can
// index it, which is what makes `gen_bitlen`'s `extra[n - base]` (L572) provably
// in range and so lets the loop read the table without a fallible
// accessor or a bounds check the reference does not have. The argument in full:
// that read is guarded by `if (n >= base)` (L572), so the index is never
// negative; and it is reached only after `if (n > max_code) continue;` (L568)
// with `max_code` set by `build_tree` from a scan bounded by `elems` (L641-L643),
// so `n <= max_code <= elems - 1`. The index therefore lies in
// `0 ..= elems - 1 - extra_base`, and each assertion below states that this
// equals the last valid index of that descriptor's table.
const _: () =
    assert!(STATIC_L_DESC.elems - STATIC_L_DESC.extra_base == STATIC_L_DESC.extra_bits.len());
const _: () =
    assert!(STATIC_D_DESC.elems - STATIC_D_DESC.extra_base == STATIC_D_DESC.extra_bits.len());
const _: () =
    assert!(STATIC_BL_DESC.elems - STATIC_BL_DESC.extra_base == STATIC_BL_DESC.extra_bits.len());

// The absolute table lengths, from the C declarations at L62, L65 and L68.
const _: () = assert!(STATIC_L_DESC.extra_bits.len() == 29);
const _: () = assert!(STATIC_D_DESC.extra_bits.len() == 30);
const _: () = assert!(STATIC_BL_DESC.extra_bits.len() == 19);

// A static tree is indexed by a symbol of its own alphabet -- `stree[n]` for a
// leaf `n <= max_code` (L575) and `stree[node]` for the node 0 or 1 that the
// forced-two-codes repair creates (L659) -- so it has to be at least `elems`
// long. Both are, and `static_ltree` is longer: it carries the two surplus
// entries `trees.h` L3 declares as `L_CODES+2`, which exist so that
// `tr_static_init` could build a canonical tree whose longest code is all ones.
const _: () = assert!(match STATIC_L_DESC.static_tree {
    Some(tree) => tree.len() == L_CODES + 2 && tree.len() >= STATIC_L_DESC.elems,
    None => false,
});
const _: () = assert!(match STATIC_D_DESC.static_tree {
    Some(tree) => tree.len() == D_CODES && tree.len() >= STATIC_D_DESC.elems,
    None => false,
});

// The bit-length tree is bounded more tightly than the other two, which is the
// asymmetry `trees.c` L47-L48 introduces against `deflate.h` L52-L53, and the
// other two share one bound. Stated here rather than in a test because both
// sides of each comparison are constants.
const _: () = assert!(STATIC_BL_DESC.max_length < STATIC_L_DESC.max_length);
const _: () = assert!(STATIC_L_DESC.max_length == STATIC_D_DESC.max_length);

#[cfg(test)]
mod tests {
    use super::{
        extra_blbits, extra_dbits, extra_lbits, static_dtree, static_ltree, StaticTreeDesc,
        StaticTreeKind, STATIC_BL_DESC, STATIC_D_DESC, STATIC_L_DESC,
    };

    /// `static_l_desc` (`trees.c` L131-L132) reduced to five comparable
    /// properties: whether it has a static tree, the length of its extra-bit
    /// table, and then `extra_base`, `elems` and `max_length` with the
    /// preprocessor expanded -- `LITERALS+1`, `L_CODES`, `MAX_BITS`.
    const L_DESC_FIELDS: (bool, usize, usize, usize, usize) = (true, 29, 257, 286, 15);

    /// The same five properties of `static_d_desc` (`trees.c` L134-L135), where
    /// `extra_base` is 0 and `elems` is `D_CODES`.
    const D_DESC_FIELDS: (bool, usize, usize, usize, usize) = (true, 30, 0, 30, 15);

    /// The same five properties of `static_bl_desc` (`trees.c` L137-L138). The
    /// leading `false` is its explicit `(const ct_data *)0`, and the trailing 7
    /// is `MAX_BL_BITS` rather than `MAX_BITS`.
    const BL_DESC_FIELDS: (bool, usize, usize, usize, usize) = (false, 19, 0, 19, 7);

    /// Reduces a descriptor to the tuple shape the three fixtures above use.
    fn fields_of(desc: &StaticTreeDesc) -> (bool, usize, usize, usize, usize) {
        (
            desc.static_tree.is_some(),
            desc.extra_bits.len(),
            desc.extra_base,
            desc.elems,
            desc.max_length,
        )
    }

    /// Every field of every descriptor, against the literal values `trees.c`
    /// L131-L138 produces.
    #[test]
    fn descriptor_fields_match_the_reference_initialisers() {
        assert_eq!(fields_of(&STATIC_L_DESC), L_DESC_FIELDS);
        assert_eq!(fields_of(&STATIC_D_DESC), D_DESC_FIELDS);
        assert_eq!(fields_of(&STATIC_BL_DESC), BL_DESC_FIELDS);
    }

    /// Only the bit-length descriptor lacks a static tree (`trees.c` L137).
    #[test]
    fn only_the_bit_length_descriptor_has_no_static_tree() {
        assert!(STATIC_L_DESC.static_tree.is_some());
        assert!(STATIC_D_DESC.static_tree.is_some());
        assert!(STATIC_BL_DESC.static_tree.is_none());
    }

    /// Each descriptor names the static tree its C initialiser names, at the
    /// length `trees.h` declares -- `L_CODES+2` = 288 and `D_CODES` = 30.
    #[test]
    fn static_trees_are_the_generated_tables() {
        assert_eq!(STATIC_L_DESC.static_tree, Some(static_ltree.as_slice()));
        assert_eq!(STATIC_D_DESC.static_tree, Some(static_dtree.as_slice()));

        assert_eq!(STATIC_L_DESC.static_tree.map(<[_]>::len), Some(288));
        assert_eq!(STATIC_D_DESC.static_tree.map(<[_]>::len), Some(30));
        assert_eq!(STATIC_BL_DESC.static_tree.map(<[_]>::len), None);
    }

    /// Each descriptor names its own extra-bit table and not a sibling's.
    ///
    /// This compares contents, not lengths. The three tables happen to have
    /// three different lengths today, so the compile-time length assertions
    /// above would catch a transposition -- but that is a coincidence of the
    /// alphabet sizes, not a property anything guarantees, and it says nothing
    /// about a table replaced by a same-length one.
    #[test]
    fn extra_bit_tables_are_not_transposed() {
        assert_eq!(STATIC_L_DESC.extra_bits, extra_lbits.as_slice());
        assert_eq!(STATIC_D_DESC.extra_bits, extra_dbits.as_slice());
        assert_eq!(STATIC_BL_DESC.extra_bits, extra_blbits.as_slice());
    }

    /// `for_kind` reproduces the three bindings `_tr_init` makes at `trees.c`
    /// L459-L466, and does so for every variant.
    #[test]
    fn for_kind_matches_the_tr_init_bindings() {
        assert_eq!(
            StaticTreeDesc::for_kind(StaticTreeKind::Literal),
            STATIC_L_DESC
        );
        assert_eq!(
            StaticTreeDesc::for_kind(StaticTreeKind::Distance),
            STATIC_D_DESC
        );
        assert_eq!(
            StaticTreeDesc::for_kind(StaticTreeKind::BitLength),
            STATIC_BL_DESC
        );

        let all: [StaticTreeDesc; 3] = [STATIC_L_DESC, STATIC_D_DESC, STATIC_BL_DESC];
        for (kind, expected) in StaticTreeKind::ALL.into_iter().zip(all) {
            assert_eq!(
                StaticTreeDesc::for_kind(kind),
                expected,
                "{}",
                kind.c_name()
            );
        }
    }

    /// `gen_bitlen`'s `extra[n - base]` (`trees.c` L572) is in range for every
    /// symbol that read can be reached with, and the table has no unreachable
    /// slack either.
    #[test]
    fn extra_bit_index_is_in_range_for_every_reachable_symbol() {
        for kind in StaticTreeKind::ALL {
            let desc = StaticTreeDesc::for_kind(kind);

            // `n <= max_code <= elems - 1`, from `build_tree` L641-L643 and
            // `gen_bitlen` L568.
            for n in 0..desc.elems {
                // The read happens only under `if (n >= base)`.
                if n < desc.extra_base {
                    continue;
                }
                assert!(
                    desc.extra_bits.get(n - desc.extra_base).is_some(),
                    "{}: symbol {n} indexes past its extra-bit table",
                    kind.c_name(),
                );
            }

            assert_eq!(
                desc.elems - desc.extra_base,
                desc.extra_bits.len(),
                "{}: extra-bit table length does not match its symbol range",
                kind.c_name(),
            );
        }
    }

    /// Every `max_length` bound is a width a code of that alphabet can actually
    /// be sent with. The ordering between the three bounds is pinned at compile
    /// time instead, since those comparisons are between constants.
    #[test]
    fn max_length_bounds_are_usable() {
        for kind in StaticTreeKind::ALL {
            let desc = StaticTreeDesc::for_kind(kind);
            assert!(desc.max_length > 0, "{}", kind.c_name());
            // A tree of `elems` leaves always fits in `elems - 1` bits, so a
            // bound wider than that could never be reached.
            assert!(desc.max_length < desc.elems, "{}", kind.c_name());
        }
    }
}
