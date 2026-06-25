//! Huffman tree data structures for the DEFLATE compressor.
//!
//! Safe-Rust port of the *data model* in `trees.c` / `trees.h`: the `ct_data`
//! entry type and the three static tree descriptors. The tree-*construction*
//! and bit-emission algorithms (`build_tree`, `gen_codes`, `compress_block`,
//! `_tr_*`) are not part of this foundation layer and land in a later
//! milestone; this file deliberately contains **no** algorithmic logic.
//!
//! These types are consumed by [`super::state::DeflateState`], whose
//! `dyn_ltree` / `dyn_dtree` / `bl_tree` arrays hold [`CtData`] entries and
//! whose tree descriptors borrow the `'static` [`StaticTreeDesc`] constants
//! defined here.

// DEFLATE alphabet sizes (RFC 1951 §3.2.5-3.2.6), matching the C `#define`s in
// `zutil.h` / `deflate.h`. These are kept local to this module so it remains a
// self-contained leaf; `deflate::state` defines its own copies for its arrays.

/// Number of literal bytes 0..=255.
const LITERALS: usize = 256;
/// Number of length codes (RFC 1951: 29 codes, 257..=285).
const LENGTH_CODES: usize = 29;
/// Number of literal/length codes: 256 literals + the end-of-block code + the
/// 29 length codes = 286.
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
/// Number of distance codes (0..=29).
const D_CODES: usize = 30;
/// Number of bit-length codes used to transmit the dynamic code lengths.
const BL_CODES: usize = 19;
/// Maximum bit length of any literal/length or distance code.
const MAX_BITS: usize = 15;
/// Maximum bit length of the bit-length (code-length) codes.
const MAX_BL_BITS: usize = 7;

/// A single Huffman tree entry — the safe-Rust analogue of the C `ct_data`
/// union (`trees.h`).
///
/// In C, `ct_data` overlays two `ush` (`u16`) unions:
///
/// ```text
/// union { ush freq; ush code; } fc;   // frequency  ↔ assigned code
/// union { ush dad;  ush len;  } dl;   // parent node ↔ code bit length
/// ```
///
/// Rust has no safe field-overlapping unions, so the two 16-bit cells are
/// stored as plain fields and the union *views* are exposed through accessors.
/// Which view is meaningful depends on the tree-construction phase, exactly as
/// in the C code: `freq`/`dad` are used while building the tree, then
/// `code`/`len_bits` after code lengths are assigned.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct CtData {
    /// The `fc` union cell: symbol *frequency* (tree-building phase) or the
    /// assigned Huffman *code* (post-assignment phase).
    pub fc: u16,
    /// The `dl` union cell: parent node index `dad` (tree-building phase) or the
    /// code bit *length* (post-assignment phase).
    pub dl: u16,
}

impl CtData {
    /// Read the `fc` cell as a symbol **frequency**.
    #[inline]
    #[must_use]
    pub const fn freq(&self) -> u16 {
        self.fc
    }

    /// Set the `fc` cell as a symbol **frequency**.
    #[inline]
    pub fn set_freq(&mut self, value: u16) {
        self.fc = value;
    }

    /// Read the `fc` cell as an assigned Huffman **code**.
    #[inline]
    #[must_use]
    pub const fn code(&self) -> u16 {
        self.fc
    }

    /// Set the `fc` cell as an assigned Huffman **code**.
    #[inline]
    pub fn set_code(&mut self, value: u16) {
        self.fc = value;
    }

    /// Read the `dl` cell as the parent-node index (`dad`).
    #[inline]
    #[must_use]
    pub const fn dad(&self) -> u16 {
        self.dl
    }

    /// Set the `dl` cell as the parent-node index (`dad`).
    #[inline]
    pub fn set_dad(&mut self, value: u16) {
        self.dl = value;
    }

    /// Read the `dl` cell as the code **bit length** (the C `len` union view).
    ///
    /// Named `len_bits` rather than `len` so the type does not advertise a
    /// collection-like `len()`/`is_empty()` contract.
    #[inline]
    #[must_use]
    pub const fn len_bits(&self) -> u16 {
        self.dl
    }

    /// Set the `dl` cell as the code **bit length**.
    #[inline]
    pub fn set_len_bits(&mut self, value: u16) {
        self.dl = value;
    }
}

/// Static description of one Huffman code family — the safe-Rust analogue of the
/// C `static_tree_desc` (`trees.h`).
///
/// In C this is `{ const ct_data *static_tree; const int *extra_bits; int
/// extra_base; int elems; int max_length; }`. The `static_tree` pointer is
/// `NULL` for the bit-length code family (and is populated only in a
/// `BUILDFIXED`-style configuration for the literal/length and distance
/// families), so it is modeled as an [`Option`].
pub struct StaticTreeDesc {
    /// The precomputed static Huffman tree, or [`None`] when the family has no
    /// static tree (always `None` at this foundation milestone; the full static
    /// literal/length and distance trees are materialized in a later
    /// milestone, exactly as C only fills them under `BUILDFIXED`).
    pub static_tree: Option<&'static [CtData]>,
    /// Extra bits transmitted after each code in this family.
    pub extra_bits: &'static [u16],
    /// Index of the first code that carries extra bits (the C `extra_base`).
    pub extra_base: usize,
    /// Maximum number of elements (codes) in the family.
    pub elems: usize,
    /// Maximum code bit length for the family.
    pub max_length: usize,
}

/// Extra bits for each of the 29 length codes (`trees.c` `extra_lbits`).
static EXTRA_LBITS: [u16; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Extra bits for each of the 30 distance codes (`trees.c` `extra_dbits`).
static EXTRA_DBITS: [u16; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Extra bits for each of the 19 bit-length codes (`trees.c` `extra_blbits`).
static EXTRA_BLBITS: [u16; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Static descriptor for the literal/length code family (`trees.c`
/// `static_l_desc`).
pub static STATIC_L_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};

/// Static descriptor for the distance code family (`trees.c` `static_d_desc`).
pub static STATIC_D_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_DBITS,
    extra_base: 0,
    elems: D_CODES,
    max_length: MAX_BITS,
};

/// Static descriptor for the bit-length code family (`trees.c`
/// `static_bl_desc`). Its `static_tree` is `NULL` in C and `None` here.
pub static STATIC_BL_DESC: StaticTreeDesc = StaticTreeDesc {
    static_tree: None,
    extra_bits: &EXTRA_BLBITS,
    extra_base: 0,
    elems: BL_CODES,
    max_length: MAX_BL_BITS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_data_union_views_alias_the_two_cells() {
        let mut e = CtData::default();
        assert_eq!(e, CtData { fc: 0, dl: 0 });

        // `freq` and `code` are two views of the same `fc` cell.
        e.set_freq(123);
        assert_eq!(e.freq(), 123);
        assert_eq!(e.code(), 123);
        e.set_code(456);
        assert_eq!(e.fc, 456);

        // `dad` and `len_bits` are two views of the same `dl` cell.
        e.set_dad(7);
        assert_eq!(e.dad(), 7);
        assert_eq!(e.len_bits(), 7);
        e.set_len_bits(9);
        assert_eq!(e.dl, 9);
    }

    #[test]
    fn static_descriptors_match_c_metadata() {
        // Element counts and base indices mirror `trees.c` exactly.
        assert_eq!(STATIC_L_DESC.elems, 286);
        assert_eq!(STATIC_L_DESC.extra_base, 257);
        assert_eq!(STATIC_L_DESC.max_length, 15);
        assert_eq!(STATIC_L_DESC.extra_bits.len(), 29);

        assert_eq!(STATIC_D_DESC.elems, 30);
        assert_eq!(STATIC_D_DESC.extra_base, 0);
        assert_eq!(STATIC_D_DESC.max_length, 15);
        assert_eq!(STATIC_D_DESC.extra_bits.len(), 30);

        assert_eq!(STATIC_BL_DESC.elems, 19);
        assert_eq!(STATIC_BL_DESC.extra_base, 0);
        assert_eq!(STATIC_BL_DESC.max_length, 7);
        assert_eq!(STATIC_BL_DESC.extra_bits.len(), 19);

        // No static tree is materialized at this milestone.
        assert!(STATIC_L_DESC.static_tree.is_none());
        assert!(STATIC_D_DESC.static_tree.is_none());
        assert!(STATIC_BL_DESC.static_tree.is_none());
    }
}
