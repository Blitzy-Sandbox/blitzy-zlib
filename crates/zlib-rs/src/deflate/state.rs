//! The compressor's working state: the port of C `deflate_state`.
//!
//! This is the Rust form of `struct internal_state` / `deflate_state`
//! (`deflate.h` L104-L288) together with the constants that surround it
//! (L34-L67), the two small types it embeds (`ct_data` at L71-L86 and
//! `tree_desc` at L88-L94), and the construction, teardown and duplication
//! arithmetic that `deflate.c` performs on it (`deflateInit2_` L387-L533,
//! `deflateEnd` L1293-L1310, `deflateCopy` L1317-L1377).
//!
//! Everything else in the `deflate` subtree, and the whole of the `trees`
//! subtree, operates on `&mut DeflateState`: `trees.c`'s only `#include` is
//! `"deflate.h"`, so the Huffman coder is not an independent subsystem with
//! state of its own but a sibling that mutates this struct. That is why
//! [`CtData`] and [`TreeDesc`] are declared here rather than in `trees`.
//!
//! `deflate.h` opens with "WARNING: this file should *not* be used by
//! applications" (L6-L9), so unlike `z_stream` this struct is **not** part of
//! the ABI. Its Rust layout is therefore free: there is no `#[repr(C)]` here,
//! no field order that a caller can observe, and no size that anything
//! asserts. What is *not* free is the field set and the width and signedness
//! of each field, because those decide the arithmetic, and the arithmetic
//! decides the emitted bytes.
//!
//! # Where each C member lives
//!
//! Three groups of members are held by the bounds-checked views in
//! [`crate::weak_slice`] rather than as fields here, because in C they are raw
//! pointers plus the cursors that walk them. The view owns the buffer and the
//! cursors together, which is what removes the pointer arithmetic.
//!
//! | `deflate.h` | C member | Here |
//! |---|---|---|
//! | L105 | `strm` | **omitted** — see below |
//! | L106 | `status` | `DeflateState::status` |
//! | L107 | `pending_buf` | `DeflateState::pending` (the wrapped buffer) |
//! | L108 | `pending_buf_size` | `pending.pending_buf_size()` |
//! | L109 | `pending_out` | `pending.pending_out_offset()` |
//! | L110 | `pending` | `pending.pending()` |
//! | L111 | `wrap` | `DeflateState::wrap` |
//! | L112 | `gzhead` | `DeflateState::gzhead` |
//! | L113 | `gzindex` | `DeflateState::gzindex` |
//! | L114 | `method` | `DeflateState::method` |
//! | L115 | `last_flush` | `DeflateState::last_flush` |
//! | L119-L121 | `w_size`, `w_bits`, `w_mask` | `window.w_size()`, `w_bits()`, `w_mask()` |
//! | L123 | `window` | `DeflateState::window` (the wrapped buffer) |
//! | L133 | `window_size` | `window.window_size()` |
//! | L138 | `prev` | `DeflateState::hash` |
//! | L144 | `head` | `DeflateState::hash` |
//! | L146 | `ins_h` | `DeflateState::ins_h` |
//! | L147-L149 | `hash_size`, `hash_bits`, `hash_mask` | `hash.hash_size()`, `hash_bits()`, `hash_mask()` |
//! | L151 | `hash_shift` | `DeflateState::hash_shift` |
//! | L158 | `block_start` | `window.block_start` |
//! | L163 | `match_length` | `DeflateState::match_length` |
//! | L164 | `prev_match` | `DeflateState::prev_match` |
//! | L165 | `match_available` | `DeflateState::match_available` |
//! | L166-L168 | `strstart`, `match_start`, `lookahead` | `window.strstart`, `match_start`, `lookahead` |
//! | L170 | `prev_length` | `DeflateState::prev_length` |
//! | L175 | `max_chain_length` | `DeflateState::max_chain_length` |
//! | L181, L186 | `max_lazy_match`, `max_insert_length` | `DeflateState::max_lazy_match` |
//! | L192-L193 | `level`, `strategy` | `DeflateState::level`, `DeflateState::strategy` |
//! | L195, L198 | `good_match`, `nice_match` | `DeflateState::good_match`, `DeflateState::nice_match` |
//! | L202-L204 | `dyn_ltree`, `dyn_dtree`, `bl_tree` | fields of the same names |
//! | L206-L208 | `l_desc`, `d_desc`, `bl_desc` | fields of the same names |
//! | L210 | `bl_count` | `DeflateState::bl_count` |
//! | L213-L215 | `heap`, `heap_len`, `heap_max` | fields of the same names |
//! | L220 | `depth` | `DeflateState::depth` |
//! | L230 | `sym_buf` | `pending` — the overlay, see below |
//! | L233 | `lit_bufsize` | `pending.lit_bufsize()` |
//! | L253-L254 | `sym_next`, `sym_end` | `pending.sym_next()`, `sym_end()` |
//! | L256-L258 | `opt_len`, `static_len`, `matches` | fields of the same names |
//! | L259 | `insert` | `window.insert` |
//! | L262-L263 | `compressed_len`, `bits_sent` | **omitted** — `ZLIB_DEBUG` only |
//! | L266, L270, L274 | `bi_buf`, `bi_valid`, `bi_used` | fields of the same names |
//! | L278 | `high_water` | `window.high_water()` |
//! | L285 | `slid` | `hash.slid()` |
//!
//! # Deliberate departures, and why each is unobservable
//!
//! * **No `strm` back-pointer** (L105). In C the state points back at its
//!   `z_stream` so that `read_buf`, `flush_pending` and the header writer can
//!   reach `next_in`, `avail_in`, `next_out`, `avail_out`, `adler`, `total_in`,
//!   `total_out` and `msg`. Here the stream side is passed in as parameters at
//!   each call, so the core never holds a borrow it cannot justify. One
//!   consequence must be handled by `deflate/mod.rs`: `deflateStateCheck`
//!   includes `s->strm != strm` in its validity test (`deflate.c` L544), which
//!   detects a state that belongs to a different stream. That check has no
//!   analogue here and must be replaced by the facade's own tag validation of
//!   the opaque `state` pointer. What *does* survive is the status test
//!   (L544-L553): because [`Status`] is an enum, "the status is one of exactly
//!   these eight values" is true by construction rather than by comparison.
//! * **The two anonymous unions are flattened.** See [`CtData`].
//! * **`tree_desc` keeps no pointer to its dynamic tree.** See [`TreeDesc`].
//! * **`LIT_MEM` is not implemented.** It is commented out at `deflate.h` L28,
//!   so the shipped layout is the `LIT_BUFS` 4 single-`sym_buf` branch
//!   (L229-L230) and not the `LIT_BUFS` 5 branch with separate `d_buf` and
//!   `l_buf` (L224-L227). Switching layouts changes the symbol encoding and
//!   the `deflatePrime` guard, and therefore the emitted bytes; it must not be
//!   "restored".
//! * **`GZIP` is implemented.** `deflate.h` L22-L24 defines it unless
//!   `NO_GZIP` is set, and the shipped build does not set it, so
//!   [`Status::GzipHeader`] and the four gzip header states are in scope.
//! * **`compressed_len` and `bits_sent` are absent** (L261-L264). They exist
//!   only under `ZLIB_DEBUG`, which the shipped build does not define, and
//!   they never influence emitted bytes.
//! * **`Pos`, `IPos`, `NIL`, `MIN_MATCH`, `MAX_MATCH`, `MIN_LOOKAHEAD`,
//!   `WIN_INIT`, `LIT_BUFS` and `SYMBOL_BYTES` are re-exported, not
//!   redeclared.** They belong to [`crate::weak_slice`], and a
//!   second structurally identical declaration here would produce type
//!   mismatches across the `deflate`/`trees` boundary. They are re-exported so
//!   that the subtree can also reach them through this module, which is where
//!   `deflate.h` puts them.
//!
//! # The `pending_buf` and `sym_buf` overlay
//!
//! One allocation of `LIT_BUFS * lit_bufsize` bytes serves as both the pending
//! compressed output and the symbol buffer, with `sym_buf = pending_buf +
//! lit_bufsize` (`deflate.c` L520) and `pending_buf_size = lit_bufsize * 4`
//! (L506). It is **not** two allocations, and it must not become two:
//! `deflate_stored` patches the four length bytes at `pending_buf[pending - 4
//! ..= pending - 1]` (`deflate.c` L1716-L1719) and `deflatePrime` refuses when
//! `sym_buf < pending_out + ((Buf_size + 7) >> 3)` (L757), both of which are
//! statements about one address space.
//!
//! The reference implementation's own justification, quoted from `deflate.c`
//! L466-L503 because it is the argument a future reader will need:
//!
//! > We overlay `pending_buf` and `sym_buf`. This works since the average size
//! > for length/distance pairs over any compressed block is assured to be 31
//! > bits or less. The longest fixed codes are a length code of 8 bits plus 5
//! > extra bits, for lengths 131 to 257. The longest fixed distance codes are 5
//! > bits plus 13 extra bits, for distances 16385 to 32768. The longest
//! > possible fixed-codes length/distance pair is then 31 bits total.
//! > `sym_buf` starts one-fourth of the way into `pending_buf`, so there are
//! > three bytes in `sym_buf` for every four bytes in `pending_buf`. Each
//! > symbol in `sym_buf` is three bytes -- two for the distance and one for the
//! > literal/length. As each symbol is consumed, the pointer to the next
//! > `sym_buf` value to read moves forward three bytes. From that symbol, up to
//! > 31 bits are written to `pending_buf`. The closest the written
//! > `pending_buf` bits gets to the next `sym_buf` symbol to read is just
//! > before the last code is written: at that time `31 * (n - 2)` bits have
//! > been written, just after `24 * (n - 2)` bits have been consumed from
//! > `sym_buf`, and `sym_buf` starts at `8 * n` bits into `pending_buf`. The
//! > closest the writing gets to what is unread is then `n + 14` bits, where
//! > `n` is `lit_bufsize` -- 16384 by default, and anywhere from 128 to 32768.
//! > Therefore there are at least 142 bits of space between what is written and
//! > what is read, so the symbols cannot be overwritten by the compressed data;
//! > that space is actually 139 bits, due to the three-bit fixed-code block
//! > header. That covers the case where fixed codes are forced or chosen, and a
//! > dynamic-code block is only emitted when it is smaller than the fixed-code
//! > block would be, so its average symbol length is also under 31 bits and the
//! > same bound applies.
//!
//! [`crate::weak_slice::PendingBuf`] is the safe expression of that layout, and
//! its `split_at_symbol_cursor` splits at exactly the bound `compress_block`
//! asserts (`trees.c` L945).
//!
//! # Memory
//!
//! Every buffer is obtained from the injected [`Allocator`] -- the caller's
//! `zalloc`/`zfree`/`opaque` triple when the facade supplies one -- and
//! returned to that same allocator, never to Rust's global allocator. The four
//! blocks are released in the order `deflateEnd` uses, "in reverse order of
//! allocations" (`deflate.c` L1300-L1304): `pending_buf`, `head`, `prev`,
//! `window`. See [`DeflateState::release`] and [`ByteBlock`].
//!
//! There is deliberately **no `Drop` implementation on [`DeflateState`]**, and
//! its absence is what makes the ordering work rather than a gap in it. A
//! destructor receives `&mut self`, and the views in [`crate::weak_slice`]
//! surrender their storage only by value, so such a destructor could not reach
//! the blocks at all. The release therefore lives on the blocks themselves
//! ([`ByteBlock`] and [`PosBlock`]), and the order comes from the declaration
//! order of the three view fields. That also leaves [`DeflateState`] free of a
//! destructor, which is precisely what lets [`DeflateState::release`] move the
//! views out of `self` and spell the four releases in C's order explicitly.
//!
//! Fresh blocks are **not** zeroed. `zcalloc` selects `malloc` over `calloc`
//! whenever `sizeof(uInt) > 2` (`zutil.c` L299-L303), which is every target of
//! interest, and `deflateInit2_` zeroes only the state struct itself
//! (`deflate.c` L442) -- not `window`, `prev`, `head` or `pending_buf`. Nothing
//! here assumes otherwise and nothing here zeroes them gratuitously: clearing
//! `head` is `lm_init`'s job through `CLEAR_HASH` (`deflate.c` L170-L175, L685)
//! and bounding the garbage past the data is `high_water`'s job through
//! `WIN_INIT` (L347-L372). `test/infcover.c` fills every block it hands out
//! with `0xa5` (L87) precisely to catch an implementation that assumes zeros.

// `DeflateState` "repeats" its module name because the module is the port of
// `deflate_state` (`deflate.h` L288) and the type must keep that name; the
// same applies to the constants named after their C `#define`s.
#![allow(clippy::module_name_repetitions)]

// Re-export the allocator bound with the state it parameterizes. Deflate leaf modules are allowed
// to depend on this foundational module without reaching through it to the allocation layer.
pub(crate) use crate::allocate::Allocator;
use crate::allocate::Buffer;
pub(crate) use crate::config::{DeflateConfig, Method, Strategy};
use crate::config::{ValidatedDeflateConfig, Wrap};
use crate::error::ReturnCode;
use crate::weak_slice::{HashChains, PendingBuf, Window};
use core::fmt;

// Test-only fixture inputs are re-exported for deflate leaf-module tests so those modules can keep
// the same dependency boundary as their production code.
#[cfg(test)]
pub(crate) use crate::allocate::GlobalAllocator;
#[cfg(test)]
pub(crate) use crate::config::{DEF_MEM_LEVEL, MAX_MEM_LEVEL, MAX_WBITS, MIN_MEM_LEVEL};

// Re-exported rather than redeclared, and usable both here and, through this
// module, from `deflate/**` and `trees/**` -- which is where `deflate.h` and
// `zutil.h` put them. `unused_imports` is allowed for the block as a whole
// because a re-export that this file does not itself name is still part of the
// module's surface: `NIL`, `Pos`, `MAX_MATCH`, `MIN_LOOKAHEAD`, `WIN_INIT` and
// `SYMBOL_BYTES` are consumed by `hash_chain.rs`, `longest_match.rs`,
// `window.rs`, `pending.rs` and `trees/**`, none of which the lint can see from
// here.
#[allow(unused_imports)]
pub(crate) use crate::weak_slice::{
    IPos, Pos, LIT_BUFS, MAX_MATCH, MIN_LOOKAHEAD, MIN_MATCH, NIL, SYMBOL_BYTES, WIN_INIT,
};

// The three block-type codes and the preset-dictionary flag, from `zutil.h`
// L87-L96. They already exist in `crate::config`; they are re-exported here so
// that `trees/**`, which is where they are actually emitted, can reach them
// alongside the rest of the deflate constants. Allowed for the same reason as
// the block above.
#[allow(unused_imports)]
pub(crate) use crate::config::{DYN_TREES, PRESET_DICT, STATIC_TREES, STORED_BLOCK};

// -----------------------------------------------------------------------------
//  Sizing constants -- `deflate.h` L34-L56
// -----------------------------------------------------------------------------

/// Number of length codes, not counting the special `END_BLOCK` code.
///
/// Port of `#define LENGTH_CODES 29` (`deflate.h` L34-L35).
pub const LENGTH_CODES: usize = 29;

/// Number of literal bytes, `0` through `255`.
///
/// Port of `#define LITERALS 256` (`deflate.h` L37-L38).
pub const LITERALS: usize = 256;

/// Number of literal-or-length codes, including the `END_BLOCK` code -- 286.
///
/// Port of `#define L_CODES (LITERALS+1+LENGTH_CODES)` (`deflate.h` L40-L41).
pub const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes.
///
/// Port of `#define D_CODES 30` (`deflate.h` L43-L44).
pub const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths.
///
/// Port of `#define BL_CODES 19` (`deflate.h` L46-L47).
pub const BL_CODES: usize = 19;

/// Maximum heap size -- 573.
///
/// Port of `#define HEAP_SIZE (2*L_CODES+1)` (`deflate.h` L49-L50). It is also
/// the length of the `dyn_ltree` array (L202), which is deliberately larger
/// than `L_CODES` because `build_tree` uses the upper half for the internal
/// nodes it creates.
pub const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// No Huffman code may exceed this many bits.
///
/// Port of `#define MAX_BITS 15` (`deflate.h` L52-L53). Declared `usize`
/// because it is both a loop bound and the index bound of
/// `DeflateState::bl_count`. Note that the bit-length tree has its own,
/// smaller bound, `MAX_BL_BITS` of 7, which belongs to `trees.c` (L45-L46) and
/// stays there.
pub const MAX_BITS: usize = 15;

/// Width in bits of the bit-accumulation buffer `DeflateState::bi_buf`.
///
/// Port of `#define Buf_size 16` (`deflate.h` L55-L56). Declared `i32` rather
/// than `usize` because every use is arithmetic against
/// `DeflateState::bi_valid`, which the reference declares `int`
/// (`deflate.h` L270-L273): `put = Buf_size - s->bi_valid` in `deflatePrime`
/// (`deflate.c` L761) and `s->bi_valid > (int)Buf_size - length` in `send_bits`
/// (`trees.c` L255). The one place it is used as a byte count,
/// `(Buf_size + 7) >> 3` in `deflatePrime`'s overlay guard
/// (`deflate.c` L757), is the compile-time constant 2.
pub const BUF_SIZE: i32 = 16;

/// Length of `DeflateState::dyn_ltree`, from `deflate.h` L202.
pub const DYN_LTREE_LEN: usize = HEAP_SIZE;

/// Length of `DeflateState::dyn_dtree` -- 61, from `deflate.h` L203.
pub const DYN_DTREE_LEN: usize = 2 * D_CODES + 1;

/// Length of `DeflateState::bl_tree` -- 39, from `deflate.h` L204.
pub const BL_TREE_LEN: usize = 2 * BL_CODES + 1;

/// Length of `DeflateState::bl_count` -- 16, from `deflate.h` L210.
pub const BL_COUNT_LEN: usize = MAX_BITS + 1;

/// Length of `DeflateState::heap` -- 573, from `deflate.h` L213.
pub const HEAP_ARRAY_LEN: usize = 2 * L_CODES + 1;

/// Length of `DeflateState::depth` -- 573, from `deflate.h` L220.
pub const DEPTH_ARRAY_LEN: usize = 2 * L_CODES + 1;

// -----------------------------------------------------------------------------
//  Stream status -- `deflate.h` L58-L68
// -----------------------------------------------------------------------------

/// Where a compression stream is in its lifecycle.
///
/// Port of the eight `#define`s at `deflate.h` L58-L68, with the discriminants
/// preserved exactly. The values are not consecutive and are not arbitrary
/// either: they are sparse on purpose, so that a `deflate_state` that has been
/// freed, zeroed or never initialised is overwhelmingly unlikely to hold one of
/// them, which is what makes `deflateStateCheck`'s status test
/// (`deflate.c` L544-L553) a useful integrity check rather than a formality.
/// [`Status::from_raw`] is that test.
///
/// The gzip states exist because `deflate.h` L22-L24 defines `GZIP` unless
/// `NO_GZIP` is set, and the shipped build does not set it.
///
/// The transitions, from the comments beside each `#define`: [`Init`] writes the
/// zlib header and goes to [`Busy`]; [`GzipHeader`] writes the gzip header and
/// goes to [`Busy`] (no header struct) or [`Extra`] (header struct supplied);
/// [`Extra`] to [`Name`] to [`Comment`] to [`Hcrc`] to [`Busy`]; [`Busy`] to
/// [`Finish`] once the stream is complete.
///
/// [`Init`]: Status::Init
/// [`GzipHeader`]: Status::GzipHeader
/// [`Extra`]: Status::Extra
/// [`Name`]: Status::Name
/// [`Comment`]: Status::Comment
/// [`Hcrc`]: Status::Hcrc
/// [`Busy`]: Status::Busy
/// [`Finish`]: Status::Finish
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// `INIT_STATE` (42): the zlib header has still to be written.
    Init,
    /// `GZIP_STATE` (57): the gzip header has still to be written.
    GzipHeader,
    /// `EXTRA_STATE` (69): writing the gzip extra field.
    Extra,
    /// `NAME_STATE` (73): writing the gzip file name.
    Name,
    /// `COMMENT_STATE` (91): writing the gzip comment.
    Comment,
    /// `HCRC_STATE` (103): writing the gzip header CRC.
    Hcrc,
    /// `BUSY_STATE` (113): compressing. `deflateEnd` reports
    /// [`ReturnCode::DATA_ERROR`] for a stream torn down in this state
    /// (`deflate.c` L1309).
    Busy,
    /// `FINISH_STATE` (666): the stream is complete, or initialisation failed
    /// (`deflate.c` L510).
    Finish,
}

impl Status {
    /// Every status, in the order the `#define`s run.
    ///
    /// Ported from `deflate.h` L58-L67.
    pub const ALL: [Self; 8] = [
        Self::Init,
        Self::GzipHeader,
        Self::Extra,
        Self::Name,
        Self::Comment,
        Self::Hcrc,
        Self::Busy,
        Self::Finish,
    ];

    /// Returns the C `int` value of this status.
    ///
    /// Written out as a `match` rather than an `as` cast so that each value is
    /// visibly the one from `deflate.h` L58-L67 rather than an artefact of the
    /// declaration order.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Init => 42,
            Self::GzipHeader => 57,
            Self::Extra => 69,
            Self::Name => 73,
            Self::Comment => 91,
            Self::Hcrc => 103,
            Self::Busy => 113,
            Self::Finish => 666,
        }
    }

    /// Converts a raw status value back into a [`Status`], returning [`None`]
    /// for anything that is not one of the eight.
    ///
    /// This is the status half of `deflateStateCheck` (`deflate.c` L544-L553):
    /// there, eight `!=` comparisons reject a state whose status field holds
    /// anything else. Here the enum makes an invalid status unrepresentable
    /// inside the library, so the check is needed only at the boundary where a
    /// raw value arrives.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            42 => Some(Self::Init),
            57 => Some(Self::GzipHeader),
            69 => Some(Self::Extra),
            73 => Some(Self::Name),
            91 => Some(Self::Comment),
            103 => Some(Self::Hcrc),
            113 => Some(Self::Busy),
            666 => Some(Self::Finish),
            _ => None,
        }
    }

    /// Whether this is one of the four states in which a gzip header field is
    /// being written, i.e. one of the states that dereferences
    /// `DeflateState::gzhead`.
    ///
    /// True for [`Status::Extra`], [`Status::Name`], [`Status::Comment`] and
    /// [`Status::Hcrc`] -- the four blocks at `deflate.c` L1117-L1200.
    #[must_use]
    pub const fn writes_gzip_header_field(self) -> bool {
        matches!(self, Self::Extra | Self::Name | Self::Comment | Self::Hcrc)
    }

    /// The reference spelling of this status, for diagnostics.
    ///
    /// Ported from `deflate.h` L58-L67.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Init => "INIT_STATE",
            Self::GzipHeader => "GZIP_STATE",
            Self::Extra => "EXTRA_STATE",
            Self::Name => "NAME_STATE",
            Self::Comment => "COMMENT_STATE",
            Self::Hcrc => "HCRC_STATE",
            Self::Busy => "BUSY_STATE",
            Self::Finish => "FINISH_STATE",
        }
    }
}

// -----------------------------------------------------------------------------
//  Huffman tree entries -- `deflate.h` L71-L94
// -----------------------------------------------------------------------------

/// One entry of a Huffman tree: a value and its code string.
///
/// Port of `ct_data` (`deflate.h` L71-L86), which is a pair of anonymous
/// unions:
///
/// ```c
/// typedef struct ct_data_s {
///     union { ush freq; ush code; } fc;
///     union { ush dad;  ush len;  } dl;
/// } FAR ct_data;
/// #define Freq fc.freq
/// #define Code fc.code
/// #define Dad  dl.dad
/// #define Len  dl.len
/// ```
///
/// Each union is flattened into a single `u16` here, and the four C macro names
/// become accessor pairs. That is exact rather than approximate because the two
/// members of a union are never live at the same time: `build_tree` reads
/// frequencies and writes parents, and `gen_codes` then overwrites the
/// frequency half with the bit string (`trees.c` L203-L232) once the
/// frequencies are no longer needed. Reproducing the unions would require
/// either `unsafe` or a redundant discriminant, and would buy nothing -- the
/// two spellings of each field name are documentation of *when* the field is
/// read, not two different storage locations.
///
/// [`Default`] yields an all-zero entry, which is what `zmemzero` on the state
/// (`deflate.c` L442) and `init_block`'s frequency reset (`trees.c` L444-L446)
/// both produce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CtData {
    /// C's `fc` union: the frequency count, later the bit string.
    fc: u16,
    /// C's `dl` union: the parent node, later the code length.
    dl: u16,
}

// `len` is the name of C's `Len` macro (`deflate.h` L86) and reports a Huffman
// code length, not a container length, so `len_without_is_empty` does not apply
// and an `is_empty` would be meaningless. Fidelity of the four accessor names to
// the four macro names is the point of this type.
#[allow(clippy::len_without_is_empty)]
impl CtData {
    /// Builds an entry from its two raw halves.
    ///
    /// The two halves are the `fc` and `dl` unions of `ct_data`, in declaration
    /// order (`deflate.h` L71-L81). `const` because the static trees are `const`
    /// data: `trees.h` spells its entries `{{12},{8}}`, i.e. `fc` then `dl`,
    /// which for a static tree means the code and its length. This is the
    /// constructor `trees/static_tables.rs` transcribes those tables with.
    #[must_use]
    pub const fn new(fc: u16, dl: u16) -> Self {
        Self { fc, dl }
    }

    /// The frequency count -- C's `Freq`, i.e. `fc.freq` (`deflate.h` L74, L83).
    #[must_use]
    #[inline]
    pub const fn freq(self) -> u16 {
        self.fc
    }

    /// Sets the frequency count -- C's `Freq` (`deflate.h` L74, L83).
    #[inline]
    pub fn set_freq(&mut self, freq: u16) {
        self.fc = freq;
    }

    /// Adds one to the frequency count, saturating rather than wrapping.
    ///
    /// This is the `s->dyn_ltree[cc].Freq++` of `_tr_tally_lit` and
    /// `_tr_tally_dist` (`deflate.h` L362, L372-L373) and the
    /// `s->dyn_ltree[END_BLOCK].Freq = 1` neighbourhood of `init_block`
    /// (`trees.c` L448). Saturation cannot be reached, and so cannot change
    /// behaviour: a frequency counts symbols in one block, a block holds at
    /// most `lit_bufsize - 1` symbols, and `lit_bufsize` is at most 32768
    /// (`deflate.c` L464 with `memLevel` 9) -- which is also exactly why the
    /// reference can "keep frequencies in 16 bit counters" (`deflate.h` L236).
    #[inline]
    pub fn increment_freq(&mut self) {
        self.fc = self.fc.saturating_add(1);
    }

    /// The bit string -- C's `Code`, i.e. `fc.code` (`deflate.h` L75, L84).
    ///
    /// Shares storage with [`CtData::freq`]: `gen_codes` overwrites the
    /// frequency with the code once the tree is built (`trees.c` L224).
    #[must_use]
    #[inline]
    pub const fn code(self) -> u16 {
        self.fc
    }

    /// Sets the bit string -- C's `Code` (`deflate.h` L75, L84).
    #[inline]
    pub fn set_code(&mut self, code: u16) {
        self.fc = code;
    }

    /// The parent node in the Huffman tree -- C's `Dad`, i.e. `dl.dad`
    /// (`deflate.h` L78, L85).
    #[must_use]
    #[inline]
    pub const fn dad(self) -> u16 {
        self.dl
    }

    /// Sets the parent node -- C's `Dad` (`deflate.h` L78, L85).
    #[inline]
    pub fn set_dad(&mut self, dad: u16) {
        self.dl = dad;
    }

    /// The length of the bit string -- C's `Len`, i.e. `dl.len`
    /// (`deflate.h` L79, L86).
    ///
    /// Shares storage with [`CtData::dad`]: `gen_bitlen` replaces each parent
    /// link with the code length it implies (`trees.c` L540-L625).
    #[must_use]
    #[inline]
    pub const fn len(self) -> u16 {
        self.dl
    }

    /// Sets the length of the bit string -- C's `Len` (`deflate.h` L79, L86).
    #[inline]
    pub fn set_len(&mut self, len: u16) {
        self.dl = len;
    }
}

/// Which of the three static tree descriptors a [`TreeDesc`] refers to.
///
/// This is the Rust stand-in for `const static_tree_desc *stat_desc`
/// (`deflate.h` L93). The descriptors themselves --
///
/// ```c
/// local const static_tree_desc static_l_desc  =
///     {static_ltree, extra_lbits, LITERALS+1, L_CODES, MAX_BITS};
/// local const static_tree_desc static_d_desc  =
///     {static_dtree, extra_dbits, 0,          D_CODES, MAX_BITS};
/// local const static_tree_desc static_bl_desc =
///     {(const ct_data *)0, extra_blbits, 0,   BL_CODES, MAX_BL_BITS};
/// ```
///
/// (`trees.c` L116-L138) -- are built from the generated static tables and from
/// `MAX_BL_BITS`, all of which belong to the `trees` module. So this enum names
/// them and `trees/tree_desc.rs` supplies them: for each variant it must
/// provide the static tree (absent for [`BitLength`]), the `extra_bits` table,
/// `extra_base`, `elems` and `max_length`.
///
/// Naming a descriptor instead of pointing at one is what removes an entire
/// class of aliasing from the port; see [`TreeDesc`].
///
/// [`BitLength`]: StaticTreeKind::BitLength
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StaticTreeKind {
    /// `static_l_desc` -- the literal and length tree, over
    /// `DeflateState::dyn_ltree` (`trees.c` L131-L132).
    Literal,
    /// `static_d_desc` -- the distance tree, over
    /// `DeflateState::dyn_dtree` (`trees.c` L134-L135).
    Distance,
    /// `static_bl_desc` -- the bit-length tree, over
    /// `DeflateState::bl_tree` (`trees.c` L137-L138). It has no static tree:
    /// the C descriptor's first member is a null pointer.
    BitLength,
}

impl StaticTreeKind {
    /// All three descriptors, in the order `trees.c` declares them
    /// (L131-L138).
    pub const ALL: [Self; 3] = [Self::Literal, Self::Distance, Self::BitLength];

    /// The reference spelling of this descriptor, for diagnostics.
    ///
    /// Ported from `trees.c` L131-L138.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Literal => "static_l_desc",
            Self::Distance => "static_d_desc",
            Self::BitLength => "static_bl_desc",
        }
    }
}

/// A dynamic Huffman tree under construction.
///
/// Port of `tree_desc` (`deflate.h` L88-L94):
///
/// ```c
/// typedef struct tree_desc_s {
///     ct_data *dyn_tree;                  /* the dynamic tree */
///     int     max_code;                   /* largest code with non zero frequency */
///     const static_tree_desc *stat_desc;  /* the corresponding static tree */
/// } FAR tree_desc;
/// ```
///
/// with `dyn_tree` **removed**. In C that member points *into the very same
/// `deflate_state`* -- `_tr_init` sets `s->l_desc.dyn_tree = s->dyn_ltree`
/// (`trees.c` L459-L466) -- which is a self-referential pointer, and a
/// self-referential borrow cannot be expressed in safe Rust at all.
///
/// The replacement is to select the array by *which descriptor is being
/// operated on* rather than by following a stored pointer:
/// [`DeflateState::tree_for`] and [`DeflateState::tree_for_mut`] map a
/// [`StaticTreeKind`] to `dyn_ltree`, `dyn_dtree` or `bl_tree`. Nothing is lost,
/// because in C the pairing is fixed for the lifetime of the state anyway.
///
/// One concrete benefit: `deflateCopy` has to repair those three pointers after
/// duplicating the state, because a bytewise copy leaves them pointing at the
/// *source* --
///
/// ```c
/// ds->l_desc.dyn_tree = ds->dyn_ltree;
/// ds->d_desc.dyn_tree = ds->dyn_dtree;
/// ds->bl_desc.dyn_tree = ds->bl_tree;
/// ```
///
/// (`deflate.c` L1371-L1373). [`DeflateState::try_clone_in`] needs no such
/// fix-up, and cannot forget it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeDesc {
    /// Largest code with a non-zero frequency, from `int max_code`
    /// (`deflate.h` L92). Set by `build_tree` (`trees.c` L627-L710) and read by
    /// `scan_tree`, `send_tree` and `send_all_trees`.
    pub max_code: i32,
    /// The corresponding static tree, from `const static_tree_desc *stat_desc`
    /// (`deflate.h` L93).
    pub stat_desc: StaticTreeKind,
}

impl TreeDesc {
    /// A descriptor for `stat_desc` with `max_code` at zero.
    ///
    /// `max_code` starts at zero because `deflateInit2_` zeroes the whole state
    /// (`deflate.c` L442) and because `build_tree` assigns it before anything
    /// reads it (`trees.c` L668). The pairing with the dynamic array is fixed at
    /// construction here, which is the work `_tr_init` does at `trees.c`
    /// L459-L466.
    #[must_use]
    pub const fn new(stat_desc: StaticTreeKind) -> Self {
        Self {
            max_code: 0,
            stat_desc,
        }
    }
}

// -----------------------------------------------------------------------------
//  The caller's gzip header -- `deflate.h` L112 and `zlib.h` L118-L133
// -----------------------------------------------------------------------------

/// A read-only view of the caller's `gz_header`, as the compressor needs it.
///
/// Port of `gz_headerp gzhead` (`deflate.h` L112). The `gz_header` struct
/// (`zlib.h` L118-L133) belongs to the *caller*: `deflateSetHeader` stores the
/// pointer and nothing else (`deflate.c` L714-L719), and the compressor only
/// ever reads through it, while the caller is required to keep it alive and
/// unmodified until the header has been written. The core cannot hold a raw
/// pointer, so the facade -- the one crate permitted to dereference one --
/// converts it into this borrow once, at the `deflateSetHeader` boundary.
///
/// Only the seven members the compressor reads are represented. `xflags` is
/// computed from the level and strategy rather than taken from the caller
/// (`deflate.c` L1102-L1104), and `extra_max`, `name_max`, `comm_max` and `done`
/// are documented in `zlib.h` as being for *reading* a header, not writing one,
/// so all five are deliberately absent.
///
/// # `name` and `comment` include their terminating zero
///
/// This is the one non-obvious part of the contract. The reference writes those
/// two fields with a loop that stops *after* emitting the terminator:
///
/// ```c
/// val = s->gzhead->name[s->gzindex++];
/// put_byte(s, val);
/// } while (val != 0);
/// ```
///
/// (`deflate.c` L1158-L1160, and identically for `comment` at L1180-L1182), so
/// the zero byte is part of the gzip stream, as RFC 1952 requires. The facade
/// must therefore build these slices to include the terminator -- for a C
/// string of `n` visible bytes the slice is `n + 1` bytes long. A slice that
/// omits it would silently produce a gzip header with no field separator.
///
/// `extra` is different: it is a counted field, so its slice is exactly
/// `extra_len` bytes and holds no terminator (`deflate.c` L1106-L1108 writes the
/// length itself into the header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GzHeaderView<'h> {
    /// `int text` (`zlib.h` L119): true if the data is believed to be text.
    /// Contributes bit 0 of the gzip `FLG` byte (`deflate.c` L1092).
    pub text: bool,
    /// `uLong time` (`zlib.h` L120): the modification time, written as four
    /// little-endian bytes (`deflate.c` L1098-L1101).
    ///
    /// Held as `u32` rather than `c_ulong` because those four `put_byte` calls
    /// consume exactly the low 32 bits and the compressor never writes the
    /// field back, so no wider value is observable. Zero means "no time
    /// available", per RFC 1952.
    pub time: u32,
    /// `int os` (`zlib.h` L122): the operating-system code, written masked to
    /// eight bits (`deflate.c` L1105).
    pub os: i32,
    /// `Bytef *extra` with `uInt extra_len` (`zlib.h` L123-L124), or [`None`]
    /// for `Z_NULL`, which clears bit 2 of `FLG` (`deflate.c` L1094).
    ///
    /// The slice length *is* `extra_len`; see [`GzHeaderView::extra_len`] for
    /// the 16-bit clamp the reference applies when writing it.
    pub extra: Option<&'h [u8]>,
    /// `Bytef *name` (`zlib.h` L126), or [`None`] for `Z_NULL`, which clears
    /// bit 3 of `FLG` (`deflate.c` L1095). **Includes the terminating zero
    /// byte** -- see the type-level documentation.
    pub name: Option<&'h [u8]>,
    /// `Bytef *comment` (`zlib.h` L128), or [`None`] for `Z_NULL`, which clears
    /// bit 4 of `FLG` (`deflate.c` L1096). **Includes the terminating zero
    /// byte** -- see the type-level documentation.
    pub comment: Option<&'h [u8]>,
    /// `int hcrc` (`zlib.h` L130): true if a header CRC is to be written.
    /// Contributes bit 1 of `FLG` (`deflate.c` L1093) and causes the two-byte
    /// CRC at `deflate.c` L1196-L1197.
    pub hcrc: bool,
}

impl GzHeaderView<'_> {
    /// The number of extra-field bytes the header will actually carry.
    ///
    /// Reproduces `(s->gzhead->extra_len & 0xffff)` (`deflate.c` L1120): the
    /// length is written as two bytes (L1107-L1108), so only the low 16 bits are
    /// transmitted and only that many bytes are copied. A caller that sets a
    /// longer `extra_len` therefore has its field truncated rather than
    /// mis-framed, and this port must truncate identically.
    #[must_use]
    pub fn extra_len(self) -> usize {
        self.extra.map_or(0, |extra| extra.len() & 0xffff)
    }

    /// The byte at `index` of the extra field, or [`None`] past its end.
    ///
    /// The extra field is copied in bulk rather than byte by byte
    /// (`deflate.c` L1123-L1137); this accessor exists for the resumable case,
    /// where `DeflateState::gzindex` records how far the copy got before the
    /// pending buffer filled.
    #[must_use]
    pub fn extra_at(self, index: usize) -> Option<u8> {
        self.extra?.get(..self.extra_len())?.get(index).copied()
    }

    /// The byte at `index` of the file name, including its terminating zero, or
    /// [`None`] past the end of the slice.
    ///
    /// The port of `s->gzhead->name[s->gzindex++]` (`deflate.c` L1158). A
    /// well-formed view always yields the terminator before running out, so
    /// [`None`] means the facade built a slice with no zero byte in it.
    #[must_use]
    pub fn name_at(self, index: usize) -> Option<u8> {
        self.name?.get(index).copied()
    }

    /// The byte at `index` of the comment, including its terminating zero, or
    /// [`None`] past the end of the slice.
    ///
    /// The port of `s->gzhead->comment[s->gzindex++]` (`deflate.c` L1180).
    #[must_use]
    pub fn comment_at(self, index: usize) -> Option<u8> {
        self.comment?.get(index).copied()
    }
}

// -----------------------------------------------------------------------------
//  Allocated blocks that release themselves
// -----------------------------------------------------------------------------

/// A byte block owned by a [`DeflateState`], released to the allocator that
/// produced it when it is dropped.
///
/// This is the storage type behind `DeflateState::window` and
/// `DeflateState::pending`, and it exists to make an *ordered* teardown
/// possible in safe Rust. `deflateEnd` frees "in reverse order of allocations"
/// -- `pending_buf`, `head`, `prev`, `window` (`deflate.c` L1300-L1304) -- and
/// `test/infcover.c` counts a departure from that last-in-first-out order as a
/// `notlifo` error (its `mem_free`, L112-L154). The views in
/// [`crate::weak_slice`] own their storage privately and surrender it only by
/// value, through `into_inner(self)`, so a `Drop` implementation on
/// [`DeflateState`] -- which gets `&mut self` -- could never reach the buffers
/// to release them. Attaching the release to the *block* solves that: Rust drops
/// struct fields in declaration order, the three views are declared in exactly
/// C's release order, and the order therefore falls out of the language rather
/// than out of a hand-written destructor.
///
/// [`DeflateState::release`] performs the same four releases explicitly, in the
/// same order, for a `deflateEnd` that wants to read like the C function.
///
/// The allocator is stored by value, which is what
/// [`crate::allocate::Allocator`] is designed for: a facade allocator is used
/// through a shared reference, and a shared reference is [`Copy`].
pub struct ByteBlock<'a, A: Allocator<'a>> {
    /// The block. [`None`] only while it is being released.
    buffer: Option<Buffer<'a, u8>>,
    /// The allocator that produced the block, and the only one that may take it
    /// back -- a mismatch is refused by `Buffer::release_to` rather than
    /// corrupting a heap.
    allocator: A,
}

impl<'a, A: Allocator<'a>> ByteBlock<'a, A> {
    /// Takes ownership of `buffer`, which must have come from `allocator`.
    ///
    /// The two byte blocks are `pending_buf` (`deflate.h` L107) and `window`
    /// (L123).
    #[must_use]
    pub const fn new(allocator: A, buffer: Buffer<'a, u8>) -> Self {
        Self {
            buffer: Some(buffer),
            allocator,
        }
    }

    /// The number of bytes in the block.
    ///
    /// A Rust affordance with no C counterpart: there the length is implicit in
    /// `w_size` or `lit_bufsize`, and the pointer carries nothing.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, Buffer::len)
    }

    /// Whether the block has no bytes. A Rust affordance, as for
    /// [`ByteBlock::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a, A: Allocator<'a>> AsRef<[u8]> for ByteBlock<'a, A> {
    fn as_ref(&self) -> &[u8] {
        self.buffer.as_ref().map_or(&[], Buffer::as_slice)
    }
}

impl<'a, A: Allocator<'a>> AsMut<[u8]> for ByteBlock<'a, A> {
    fn as_mut(&mut self) -> &mut [u8] {
        match &mut self.buffer {
            Some(buffer) => buffer.as_mut_slice(),
            // Unreachable outside the destructor, which does not hand out
            // borrows. An empty slice keeps the accessor total.
            None => &mut [],
        }
    }
}

impl<'a, A: Allocator<'a>> Drop for ByteBlock<'a, A> {
    /// Returns the block to the allocator that produced it.
    ///
    /// `try_deallocate_bytes` is the `TRY_FREE(s, p)` analogue -- `{if (p)
    /// ZFREE(s, p);}` (`zutil.h` L255) -- which is the form `deflateEnd` uses
    /// (`deflate.c` L1301-L1304) precisely because an initialisation that failed
    /// part-way leaves some buffers absent.
    fn drop(&mut self) {
        self.allocator.try_deallocate_bytes(self.buffer.take());
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for ByteBlock<'a, A> {
    /// Reports the block's shape, never its contents and never the allocator.
    ///
    /// Written out rather than derived so that it does not require `A: Debug`,
    /// which would make [`DeflateState`] undebuggable for a facade allocator
    /// that has no `Debug` implementation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ByteBlock")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// A block of `Pos` entries owned by a [`DeflateState`], released to the
/// allocator that produced it when it is dropped.
///
/// The [`ByteBlock`] counterpart for the two hash-chain arrays, `head` and
/// `prev`, which hold `Pos` -- that is, `ush`, an unsigned 16-bit window index
/// (`deflate.h` L96, `zutil.h` L45). They are allocated as `u16` rather than as
/// bytes so that the request reaching the caller's `zalloc` is the same
/// `ZALLOC(strm, items, sizeof(Pos))` the reference makes
/// (`deflate.c` L459-L460), and so that the release goes back through the
/// matching `deallocate_u16s`.
pub struct PosBlock<'a, A: Allocator<'a>> {
    /// The block. [`None`] only while it is being released.
    buffer: Option<Buffer<'a, u16>>,
    /// The allocator that produced the block; see [`ByteBlock`].
    allocator: A,
}

impl<'a, A: Allocator<'a>> PosBlock<'a, A> {
    /// Takes ownership of `buffer`, which must have come from `allocator`.
    ///
    /// The two `Pos` blocks are `head` (`deflate.h` L144) and `prev` (L138).
    #[must_use]
    pub const fn new(allocator: A, buffer: Buffer<'a, u16>) -> Self {
        Self {
            buffer: Some(buffer),
            allocator,
        }
    }

    /// The number of entries in the block.
    ///
    /// A Rust affordance with no C counterpart; see [`ByteBlock::len`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, Buffer::len)
    }

    /// Whether the block has no entries. A Rust affordance, as for
    /// [`PosBlock::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a, A: Allocator<'a>> AsRef<[u16]> for PosBlock<'a, A> {
    fn as_ref(&self) -> &[u16] {
        self.buffer.as_ref().map_or(&[], Buffer::as_slice)
    }
}

impl<'a, A: Allocator<'a>> AsMut<[u16]> for PosBlock<'a, A> {
    fn as_mut(&mut self) -> &mut [u16] {
        match &mut self.buffer {
            Some(buffer) => buffer.as_mut_slice(),
            // Unreachable outside the destructor; see `ByteBlock`.
            None => &mut [],
        }
    }
}

impl<'a, A: Allocator<'a>> Drop for PosBlock<'a, A> {
    /// Returns the block to the allocator that produced it, through the
    /// `TRY_FREE` analogue for `u16` blocks (`zutil.h` L255,
    /// `deflate.c` L1302-L1303).
    fn drop(&mut self) {
        self.allocator.try_deallocate_u16s(self.buffer.take());
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for PosBlock<'a, A> {
    /// Reports the block's shape only; see [`ByteBlock`]'s implementation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PosBlock")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// The sliding window as [`DeflateState`] holds it: the port of `Bytef *window`
/// plus `window_size` (`deflate.h` L123-L138).
pub type DeflateWindow<'a, A> = Window<ByteBlock<'a, A>>;

/// The hash chains as [`DeflateState`] holds them: the port of `Posf *prev` and
/// `Posf *head` plus their sizes and the `slid` flag
/// (`deflate.h` L138-L149, L285).
pub type DeflateHashChains<'a, A> = HashChains<PosBlock<'a, A>>;

/// The overlaid pending-output and symbol buffer as [`DeflateState`] holds it:
/// the port of `pending_buf`, `pending_out`, `pending`, `pending_buf_size`,
/// `sym_buf`, `lit_bufsize`, `sym_next` and `sym_end`
/// (`deflate.h` L107-L110, L230-L254).
pub type DeflatePending<'a, A> = PendingBuf<ByteBlock<'a, A>>;

// -----------------------------------------------------------------------------
//  The state -- `deflate.h` L104-L288
// -----------------------------------------------------------------------------

/// The internal compression state: the port of `deflate_state`
/// (`deflate.h` L104-L288).
///
/// Build one with [`DeflateState::new`], which reproduces `deflateInit2_`'s
/// allocation and sizing arithmetic (`deflate.c` L440-L530); complete the
/// initialisation with the reset half, which belongs to `deflate/mod.rs`; and
/// tear it down with [`DeflateState::release`], or simply drop it. See the
/// module documentation for the full member-by-member mapping, for the three
/// deliberate departures from the C layout, and for the `pending_buf`/`sym_buf`
/// overlay.
///
/// Fields are `pub(crate)` rather than hidden behind setters, for the same
/// reason [`crate::weak_slice::Window`]'s cursors are public: the reference
/// implementation adjusts them freely from `deflate.c`, `trees.c` and every
/// algorithm variant, no invariant of this type depends on their values, and a
/// wall of one-line setters would obscure rather than protect the correspondence
/// with the C source that byte-identical output depends on. The buffers, by
/// contrast, are reachable only through the checked views, so no cursor value
/// can cause an out-of-bounds access.
///
/// # Size
///
/// Around six kilobytes, nearly all of it the three inline tree arrays and the
/// `heap` and `depth` arrays, exactly as in C. Hold it behind a pointer -- the
/// facade places it in the block a caller's `zalloc` returned, which is what
/// `ZALLOC(strm, 1, sizeof(deflate_state))` (`deflate.c` L440) does -- and avoid
/// moving it by value once built.
///
/// # Lifetimes and type parameters
///
/// `'a` is the lifetime of the allocations, and of the caller's `gz_header` if
/// one has been installed; `A` is the injected allocator, which every buffer is
/// obtained from and returned to.
pub struct DeflateState<'a, A: Allocator<'a>> {
    // -------------------------------------------------------------------------
    //  The three buffer-owning views.
    //
    //  DECLARATION ORDER IS LOAD-BEARING. Rust drops fields in declaration
    //  order and `HashChains` declares `head` before `prev`, so dropping the
    //  state releases `pending_buf`, `head`, `prev`, `window` -- exactly the
    //  order `deflateEnd` uses, "in reverse order of allocations"
    //  (`deflate.c` L1300-L1304), and exactly the last-in-first-out order
    //  `test/infcover.c` requires. Reordering these three fields changes the
    //  release order silently; the `release_order_matches_deflate_end` test
    //  exists to catch that.
    // -------------------------------------------------------------------------
    /// `Bytef *pending_buf`, `ulg pending_buf_size`, `Bytef *pending_out`,
    /// `ulg pending` (`deflate.h` L107-L110) and `uchf *sym_buf`,
    /// `uInt lit_bufsize`, `uInt sym_next`, `uInt sym_end`
    /// (`deflate.h` L230-L254) -- one allocation, overlaid as the module
    /// documentation describes. Released first (`deflate.c` L1301).
    pub(crate) pending: DeflatePending<'a, A>,

    /// `Posf *head` and `Posf *prev` (`deflate.h` L138-L144) with
    /// `uInt hash_size`, `uInt hash_bits`, `uInt hash_mask`
    /// (L147-L149) and `int slid` (L285-L286).
    ///
    /// `head[h]` is the most recent position whose three-byte prefix hashed to
    /// `h`; `prev[p & w_mask]` is the next older position on that chain, with
    /// `NIL` as the tail. Released second and third -- `head` then `prev`,
    /// matching `deflate.c` L1302-L1303.
    pub(crate) hash: DeflateHashChains<'a, A>,

    /// `Bytef *window` (`deflate.h` L123-L131) and `ulg window_size` (L133),
    /// with the five cursors `uInt strstart`, `uInt match_start`,
    /// `uInt lookahead`, `long block_start` and `uInt insert`
    /// (L158-L168, L259) and `ulg high_water` (L278-L283).
    ///
    /// `block_start` is `isize` there rather than `i64`, which is what C's
    /// `long` actually is on the LP64 targets this port builds for, and is
    /// signed for the reason `deflate.h` L159-L161 gives: it "gets negative when
    /// the window is moved backwards". Released last (`deflate.c` L1304).
    pub(crate) window: DeflateWindow<'a, A>,

    // -------------------------------------------------------------------------
    //  Scalars, in `deflate.h` order. `z_streamp strm` (L105) is omitted; see
    //  the module documentation.
    // -------------------------------------------------------------------------
    /// `int status` -- "as the name implies" (`deflate.h` L106).
    pub(crate) status: Status,

    /// `int wrap` -- "bit 0 true for zlib, bit 1 true for gzip"
    /// (`deflate.h` L111), so 0 raw, 1 zlib, 2 gzip
    /// (`deflate.c` L391, L423, L430).
    ///
    /// **Signed, and that is load-bearing.** `deflate()` negates it once the
    /// trailer has been written -- `if (s->wrap > 0) s->wrap = -s->wrap;`, "write
    /// the trailer only once!" (`deflate.c` L1288) -- and `deflateResetKeep`
    /// restores the sign (L659-L661). Other code branches on `s->wrap <= 0`
    /// (L1264) and `s->wrap < 0`. An unsigned field would wrap instead of going
    /// negative and the trailer would be written twice. Use
    /// [`DeflateState::container`] to recover the [`Wrap`] regardless of sign.
    pub(crate) wrap: i32,

    /// `gz_headerp gzhead` -- "gzip header information to write"
    /// (`deflate.h` L112), or [`None`] for `Z_NULL`, which selects the
    /// no-header-struct branch at `deflate.c` L1072-L1090. Installed by
    /// `deflateSetHeader` (L714-L719), which the reference permits only when
    /// `wrap == 2`.
    pub(crate) gzhead: Option<GzHeaderView<'a>>,

    /// `ulg gzindex` -- "where in extra, name, or comment" (`deflate.h` L113).
    ///
    /// The resumable cursor for the four gzip header states: the extra-field
    /// copy advances it and resets it to zero (`deflate.c` L1127, L1140), and the
    /// name and comment loops do the same (L1158-L1162, L1180). Held as `usize`
    /// because every use is an index into one of the three
    /// [`GzHeaderView`] slices.
    pub(crate) gzindex: usize,

    /// `Byte method` -- "can only be DEFLATED" (`deflate.h` L114). A
    /// [`Method`] rather than a byte, which makes that comment a type.
    pub(crate) method: Method,

    /// `int last_flush` -- "value of flush param for previous deflate call"
    /// (`deflate.h` L115).
    ///
    /// Deliberately **not** a `Flush`, because it carries two out-of-band
    /// sentinels as well as the seven real flush values: `-1` means "the last
    /// call ran out of output space", set at ten places in `deflate()` to keep
    /// the next call from reporting `Z_BUF_ERROR` (`deflate.c` L1010, L1061,
    /// L1087, L1130, L1153, L1175, L1192, L1205, L1227, L1257), and `-2` means
    /// "freshly reset", set by `deflateResetKeep` (L672) and tested by
    /// `deflateParams` (L792).
    pub(crate) last_flush: i32,

    /// `uInt ins_h` -- "hash index of string to be inserted"
    /// (`deflate.h` L146).
    ///
    /// Held as `usize` rather than `u32`: it is the running hash from
    /// `UPDATE_HASH` (`deflate.c` L141), which masks it with `hash_mask` and so
    /// keeps it below `hash_size` -- at most 65536 -- and its only use is as the
    /// index handed to the hash-chain accessors, which take `usize`. Nothing
    /// observable depends on the wider type, and the narrower one would require
    /// a cast at every use.
    pub(crate) ins_h: usize,

    /// `uInt hash_shift` -- the number of bits `ins_h` is shifted by at each
    /// input step (`deflate.h` L151).
    ///
    /// The invariant, quoted from `deflate.h` L152-L155: "It must be such that
    /// after `MIN_MATCH` steps, the oldest byte no longer takes part in the hash
    /// key, that is: `hash_shift * MIN_MATCH >= hash_bits`". `deflateInit2_`
    /// establishes it as `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`
    /// (`deflate.c` L456), so it ranges from 3 at `memLevel` 1 to 6 at
    /// `memLevel` 9. Held as `usize` for the same reason as `ins_h`.
    pub(crate) hash_shift: usize,

    /// `uInt match_length` -- "length of best match" (`deflate.h` L163).
    ///
    /// A byte count in the window, so `usize`, which is the type of the
    /// `lookahead` it is repeatedly clamped against
    /// (`deflate.c` L1528-L1529). `lm_init` starts it at `MIN_MATCH - 1`
    /// (L698).
    pub(crate) match_length: usize,

    /// `IPos prev_match` -- "previous match" (`deflate.h` L164), the candidate
    /// `deflate_slow` remembers across one position so that it can emit the
    /// earlier of two overlapping matches.
    pub(crate) prev_match: IPos,

    /// `int match_available` -- "set if previous match exists"
    /// (`deflate.h` L165).
    ///
    /// A `bool`: the reference only ever assigns 0 or 1 and only ever tests for
    /// truth, so nothing is lost.
    pub(crate) match_available: bool,

    /// `uInt prev_length` -- "Length of the best match at previous step.
    /// Matches not greater than this are discarded. This is used in the lazy
    /// match evaluation." (`deflate.h` L170-L173). `usize`, like
    /// `match_length`; `lm_init` starts it at `MIN_MATCH - 1`
    /// (`deflate.c` L698).
    pub(crate) prev_length: usize,

    /// `uInt max_chain_length` -- "To speed up deflation, hash chains are never
    /// searched beyond this length. A higher limit improves compression ratio
    /// but degrades the speed." (`deflate.h` L175-L179).
    ///
    /// One of the four tuning parameters loaded from `configuration_table`
    /// (`deflate.c` L692); `longest_match` copies it into its chain counter
    /// (L1390). Byte-identity critical: it decides how many candidates are
    /// examined and therefore which match is chosen.
    pub(crate) max_chain_length: usize,

    /// `uInt max_lazy_match` -- "Attempt to find a better match only when the
    /// current match is strictly smaller than this value. This mechanism is used
    /// only for compression levels >= 4." (`deflate.h` L181-L185).
    ///
    /// C also spells this field `max_insert_length`, via
    /// `#define max_insert_length max_lazy_match` (`deflate.h` L186): "Insert
    /// new strings in the hash table only if the match length is not greater
    /// than this length. This saves time but degrades compression.
    /// `max_insert_length` is used only for compression levels <= 3."
    /// (L187-L190). They are **one field with two names**, read under the second
    /// name by `deflate_fast` (`deflate.c` L1906) and under the first by
    /// `deflate_slow` (L1988); [`DeflateState::max_insert_length`] is that
    /// second spelling.
    pub(crate) max_lazy_match: usize,

    /// `int level` -- "compression level (1..9)" (`deflate.h` L192), though 0 is
    /// also valid and means "store only".
    ///
    /// `i32`, as in C, because it is compared against the literals the C code
    /// compares it against -- `s->level == 0` (`deflate.c` L801), `s->level == 9`
    /// (L1078), `s->level < 2` (L1079) -- and because `deflateParams` accepts an
    /// `int` from the caller.
    pub(crate) level: i32,

    /// `int strategy` -- "favor or force Huffman coding" (`deflate.h` L193).
    ///
    /// A [`Strategy`] rather than an `int`. The C code performs *ordered*
    /// comparisons on it -- `s->strategy >= Z_HUFFMAN_ONLY`
    /// (`deflate.c` L1036, L1079, L1103) and `s->strategy == Z_FILTERED`
    /// (L1997) -- so the numeric order `Z_DEFAULT_STRATEGY` 0 <
    /// `Z_FILTERED` 1 < `Z_HUFFMAN_ONLY` 2 < `Z_RLE` 3 < `Z_FIXED` 4 must be
    /// preserved; `Strategy::as_raw` is the accessor that preserves it, and
    /// [`DeflateState::favours_huffman_only`] is the ordered test itself.
    pub(crate) strategy: Strategy,

    /// `uInt good_match` -- "Use a faster search when the previous match is
    /// longer than this" (`deflate.h` L194-L195). Loaded from
    /// `configuration_table` (`deflate.c` L690) and tested at L1423.
    pub(crate) good_match: usize,

    /// `int nice_match` -- "Stop searching when current match exceeds this"
    /// (`deflate.h` L197).
    ///
    /// **Signed, and that is load-bearing.** `longest_match` copies it into an
    /// `int`, then clamps it with a round trip through unsigned and back:
    /// `if ((uInt)nice_match > s->lookahead) nice_match = (int)s->lookahead;`
    /// (`deflate.c` L1429), and compares `len >= nice_match` on `int`s (L1517).
    /// `deflateTune` also assigns it straight from a caller-supplied `int`
    /// (L827). Keeping it `int` is what lets that sequence be reproduced
    /// exactly.
    pub(crate) nice_match: i32,

    /// `struct ct_data_s dyn_ltree[HEAP_SIZE]` -- "literal and length tree"
    /// (`deflate.h` L202). Longer than `L_CODES` because `build_tree` builds its
    /// internal nodes in the upper half.
    pub(crate) dyn_ltree: [CtData; DYN_LTREE_LEN],

    /// `struct ct_data_s dyn_dtree[2*D_CODES+1]` -- "distance tree"
    /// (`deflate.h` L203).
    pub(crate) dyn_dtree: [CtData; DYN_DTREE_LEN],

    /// `struct ct_data_s bl_tree[2*BL_CODES+1]` -- "Huffman tree for bit
    /// lengths" (`deflate.h` L204), the tree that encodes the other two trees.
    pub(crate) bl_tree: [CtData; BL_TREE_LEN],

    /// `struct tree_desc_s l_desc` -- "desc. for literal tree"
    /// (`deflate.h` L206), paired with `dyn_ltree`.
    pub(crate) l_desc: TreeDesc,

    /// `struct tree_desc_s d_desc` -- "desc. for distance tree"
    /// (`deflate.h` L207), paired with `dyn_dtree`.
    pub(crate) d_desc: TreeDesc,

    /// `struct tree_desc_s bl_desc` -- "desc. for bit length tree"
    /// (`deflate.h` L208), paired with `bl_tree`.
    pub(crate) bl_desc: TreeDesc,

    /// `ush bl_count[MAX_BITS+1]` -- "number of codes at each bit length for an
    /// optimal tree" (`deflate.h` L210-L211). Filled by `gen_bitlen` and
    /// consumed by `gen_codes` (`trees.c` L540-L625, L203-L232).
    pub(crate) bl_count: [u16; BL_COUNT_LEN],

    /// `int heap[2*L_CODES+1]` -- "heap used to build the Huffman trees"
    /// (`deflate.h` L213). "The sons of `heap[n]` are `heap[2*n]` and
    /// `heap[2*n+1]`. `heap[0]` is not used. The same heap array is used to
    /// build all trees." (L216-L218).
    pub(crate) heap: [i32; HEAP_ARRAY_LEN],

    /// `int heap_len` -- "number of elements in the heap"
    /// (`deflate.h` L214).
    pub(crate) heap_len: i32,

    /// `int heap_max` -- "element of largest frequency" (`deflate.h` L215).
    /// Counts *down* from `HEAP_SIZE` as `build_tree` fills the array from the
    /// top (`trees.c` L646).
    pub(crate) heap_max: i32,

    /// `uch depth[2*L_CODES+1]` -- "Depth of each subtree used as tie breaker
    /// for trees of equal frequency" (`deflate.h` L220-L222).
    ///
    /// Byte-identity critical: the `smaller` macro breaks a frequency tie with
    /// `depth[n] <= depth[m]` (`trees.c` L499-L501), so these values decide the
    /// code lengths whenever two symbols occur equally often.
    pub(crate) depth: [u8; DEPTH_ARRAY_LEN],

    /// `ulg opt_len` -- "bit length of current block with optimal trees"
    /// (`deflate.h` L256).
    ///
    /// `u64` because C's `ulg` is `unsigned long`, which is 64-bit on LP64 and
    /// 32-bit elsewhere; taking the wider of the two means the accumulation in
    /// `gen_bitlen` and the `(opt_len + 3 + 7) >> 3` comparison that picks the
    /// block type (`trees.c` L1027-L1074) cannot truncate on any target.
    pub(crate) opt_len: u64,

    /// `ulg static_len` -- "bit length of current block with static trees"
    /// (`deflate.h` L257). The counterpart of `opt_len`; the smaller of the two,
    /// after both are rounded up to whole bytes, selects the block type.
    pub(crate) static_len: u64,

    /// `uInt matches` -- "number of string matches in current block"
    /// (`deflate.h` L258). Reset by `init_block` (`trees.c` L449) and read by
    /// `deflateParams` to decide whether a level change needs a flush first
    /// (`deflate.c` L801-L806).
    pub(crate) matches: u32,

    /// `ush bi_buf` -- "Output buffer. bits are inserted starting at the bottom
    /// (least significant bits)." (`deflate.h` L266-L269). Its width is
    /// [`BUF_SIZE`] bits.
    pub(crate) bi_buf: u16,

    /// `int bi_valid` -- "Number of valid bits in `bi_buf`. All bits above the
    /// last valid bit are always zero." (`deflate.h` L270-L273). Reported to
    /// callers by `deflatePending` (`deflate.c` L725).
    pub(crate) bi_valid: i32,

    /// `int bi_used` -- "Last number of used bits when going to a byte
    /// boundary." (`deflate.h` L274-L276).
    ///
    /// Maintained by `bi_windup` as `((s->bi_valid - 1) & 7) + 1`
    /// (`trees.c` L187) and forced to 8 where a stored block ends on a byte
    /// boundary (`deflate.c` L1791, L1846). This is what `deflateUsed` reports
    /// (L740).
    pub(crate) bi_used: i32,

    /// The allocator every buffer came from, kept so that the state is
    /// self-sufficient in teardown and so that `deflateCopy` can be told
    /// explicitly which allocator the *copy* must use
    /// (`deflate.c` L1335-L1345 allocates from `dest`).
    ///
    /// Declared last so that it is dropped after the blocks that hold their own
    /// copies of it.
    pub(crate) allocator: A,
}

/// Copies `source` into `target` starting at `at`, without indexing that could
/// panic.
///
/// The bulk-copy primitive behind [`DeflateState::try_clone_in`], standing in
/// for the six `zmemcpy` calls at `deflate.c` L1353-L1368. Both bounds are
/// checked before the copy, and the two slices are equal in length by
/// construction, so `copy_from_slice` cannot fail.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if the destination range does not fit, which is
/// unreachable for the fresh, correctly sized blocks the caller passes and is
/// reported rather than asserted only so that no path here can panic.
fn copy_into<T: Copy>(target: &mut [T], at: usize, source: &[T]) -> Result<(), ReturnCode> {
    let end = at
        .checked_add(source.len())
        .ok_or(ReturnCode::STREAM_ERROR)?;
    let slot = target.get_mut(at..end).ok_or(ReturnCode::STREAM_ERROR)?;
    slot.copy_from_slice(source);
    Ok(())
}

impl<'a, A: Allocator<'a> + Copy> DeflateState<'a, A> {
    /// Allocates and initialises a compression state, as `deflateInit2_` does.
    ///
    /// `config` is validated first, exactly as `deflateInit2_` validates its
    /// five parameters before touching memory (`deflate.c` L419-L439), so a
    /// rejected configuration costs no allocation. That validation --
    /// including the `Z_DEFAULT_COMPRESSION` default, the `windowBits` sign and
    /// offset decoding, and the promotion of `windowBits` 8 to 9 "until 256-byte
    /// window bug fixed" (L439) -- lives in [`crate::config`] and is not
    /// duplicated here.
    ///
    /// # Errors
    ///
    /// * [`ReturnCode::STREAM_ERROR`] if `config` names an invalid combination
    ///   of level, method, `windowBits`, `memLevel` and strategy
    ///   (`deflate.c` L434-L438).
    /// * [`ReturnCode::MEM_ERROR`] if any of the four buffers cannot be
    ///   allocated (`deflate.c` L508-L514). Blocks obtained before the failure
    ///   are released to the same allocator, in `deflateEnd`'s order, before
    ///   this returns -- which is what `deflateInit2_` achieves by calling
    ///   `deflateEnd` on the failure path (L512).
    pub fn new(config: DeflateConfig, allocator: A) -> Result<Self, ReturnCode> {
        Self::with_validated_config(config.validate()?, allocator)
    }

    /// Allocates and initialises a compression state from an already-validated
    /// configuration.
    ///
    /// This is the second half of `deflateInit2_` (`deflate.c` L440-L530): the
    /// derived sizes, the four allocations, and the field assignments. It stops
    /// where the C function does, at `return deflateReset(strm)` (L532) --
    /// **the state is not yet ready to compress**. The reset half,
    /// `deflateResetKeep` plus `lm_init` (L644-L711), belongs to
    /// `deflate/mod.rs`; [`DeflateState::reset_keep`] and
    /// [`DeflateState::set_tuning`] are the pieces of it that live here.
    ///
    /// Nothing is zeroed beyond what C zeroes. `zmemzero(s, sizeof(deflate_state))`
    /// (L442) clears the *struct*, which is reproduced by the field values below;
    /// `window`, `prev`, `head` and `pending_buf` are left exactly as the
    /// allocator handed them over, because clearing `head` is `CLEAR_HASH`'s job
    /// (L170-L175) and bounding the window's garbage is `high_water`'s
    /// (L347-L372).
    ///
    /// # Allocation failure
    ///
    /// The C function issues all four requests and then tests all four results
    /// together (`deflate.c` L458-L460, L505, L508-L513); this returns as soon as
    /// one request is declined. The difference is not observable: the status is
    /// [`ReturnCode::MEM_ERROR`] either way, nothing obtained is leaked either
    /// way, and this way the transient peak is lower. Two of C's three failure
    /// actions belong elsewhere for the same reason: `strm->msg =
    /// ERR_MSG(Z_MEM_ERROR)` (L511) is the stream's, through
    /// `ReturnCode::record_msg`, and `s->status = FINISH_STATE` (L510) has no
    /// analogue because on this path no state comes into existence to hold it.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if any allocation fails; see
    /// [`DeflateState::new`]. [`ReturnCode::STREAM_ERROR`] is returned if a
    /// buffer cannot be wrapped in its view, which is unreachable for a
    /// [`ValidatedDeflateConfig`]: validation guarantees `window_bits` in
    /// `9..=15` and `mem_level` in `1..=9`, and the blocks are allocated at
    /// exactly the sizes the views require. The branch exists because those
    /// constructors are total functions rather than assertions.
    pub fn with_validated_config(
        config: ValidatedDeflateConfig,
        allocator: A,
    ) -> Result<Self, ReturnCode> {
        // Derived sizes, in the order `deflateInit2_` computes them
        // (`deflate.c` L449-L456 and L464). `hash_bits` is needed both as the
        // exponent the hash-chain view records and as a plain number for the
        // shift arithmetic, so it is converted from `mem_level` twice rather
        // than cast once.
        let w_bits = u32::from(config.window_bits);
        let hash_bits = u32::from(config.mem_level) + 7;
        let mem_level = usize::from(config.mem_level);
        let hash_exponent = mem_level + 7;
        let w_size = 1_usize << w_bits;
        let hash_size = 1_usize << hash_exponent;
        let lit_bufsize = 1_usize << (mem_level + 6);
        // `(s->hash_bits + MIN_MATCH-1) / MIN_MATCH` (`deflate.c` L456), which is
        // that division rounded up -- 3 at `memLevel` 1 through 6 at `memLevel` 9.
        let hash_shift = hash_exponent.div_ceil(MIN_MATCH);

        // The four allocations, in `deflateInit2_`'s order
        // (`deflate.c` L458-L460 and L505). Each block is handed to its owner
        // immediately, so if a later allocation fails the earlier blocks are
        // released by their own destructors -- and because Rust drops locals in
        // reverse declaration order, they are released as `pending_buf`, `head`,
        // `prev`, `window`, which is precisely the order `deflateEnd` uses
        // (L1300-L1304) and the last-in-first-out order `test/infcover.c`
        // requires.
        let window = ByteBlock::new(allocator, allocator.allocate_bytes_or_mem_error(w_size, 2)?);
        let prev = PosBlock::new(allocator, allocator.allocate_u16s_or_mem_error(w_size)?);
        let head = PosBlock::new(allocator, allocator.allocate_u16s_or_mem_error(hash_size)?);
        let pending = ByteBlock::new(
            allocator,
            allocator.allocate_bytes_or_mem_error(lit_bufsize, LIT_BUFS)?,
        );

        // Wrapping the blocks establishes `pending_buf_size = lit_bufsize * 4`
        // (`deflate.c` L506), `sym_buf = pending_buf + lit_bufsize` (L520) and
        // `sym_end = (lit_bufsize - 1) * 3` (L521), and starts every cursor and
        // `high_water` at zero (L462). It also establishes
        // `window_size = 2 * w_size`, which C leaves zero until `lm_init`
        // (L683); deriving it from `w_bits` instead makes it right from the
        // start, and nothing can read it in between, because everything that
        // uses it runs after the reset.
        let (Some(window), Some(hash), Some(pending)) = (
            Window::new(window, w_bits),
            HashChains::new(head, prev, w_bits, hash_bits),
            PendingBuf::new(pending, lit_bufsize),
        ) else {
            return Err(ReturnCode::STREAM_ERROR);
        };

        Ok(Self {
            pending,
            hash,
            window,
            // `s->status = INIT_STATE` -- "to pass state test in deflateReset()"
            // (`deflate.c` L445). `deflateResetKeep` replaces it with
            // `GZIP_STATE` when `wrap == 2` (L662-L666).
            status: Status::Init,
            wrap: config.wrap.as_deflate_wrap(),
            // `s->gzhead = Z_NULL` (`deflate.c` L448).
            gzhead: None,
            gzindex: 0,
            method: config.method,
            // Zero from `zmemzero`; `deflateResetKeep` sets the `-2` sentinel
            // (`deflate.c` L672).
            last_flush: 0,
            ins_h: 0,
            hash_shift,
            // Zero from `zmemzero`; `lm_init` sets both to `MIN_MATCH - 1`
            // (`deflate.c` L698).
            match_length: 0,
            prev_match: IPos::NIL,
            match_available: false,
            prev_length: 0,
            // Zero from `zmemzero`; `lm_init` loads all four from
            // `configuration_table[level]` (`deflate.c` L689-L692).
            max_chain_length: 0,
            max_lazy_match: 0,
            level: i32::from(config.level),
            strategy: config.strategy,
            good_match: 0,
            nice_match: 0,
            // The zeroed trees `zmemzero` leaves (`deflate.c` L442); `init_block`
            // then sets `dyn_ltree[END_BLOCK].Freq = 1` (`trees.c` L448).
            dyn_ltree: [CtData::new(0, 0); DYN_LTREE_LEN],
            dyn_dtree: [CtData::new(0, 0); DYN_DTREE_LEN],
            bl_tree: [CtData::new(0, 0); BL_TREE_LEN],
            // The pairing `_tr_init` establishes at `trees.c` L459-L466, fixed
            // here at construction because it can never change.
            l_desc: TreeDesc::new(StaticTreeKind::Literal),
            d_desc: TreeDesc::new(StaticTreeKind::Distance),
            bl_desc: TreeDesc::new(StaticTreeKind::BitLength),
            bl_count: [0; BL_COUNT_LEN],
            heap: [0; HEAP_ARRAY_LEN],
            heap_len: 0,
            heap_max: 0,
            depth: [0; DEPTH_ARRAY_LEN],
            opt_len: 0,
            static_len: 0,
            matches: 0,
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,
            allocator,
        })
    }

    /// Duplicates the state into fresh buffers obtained from `allocator`.
    ///
    /// The port of `deflateCopy` (`deflate.c` L1317-L1377), whose contract is to
    /// leave the source untouched and to produce a copy that can continue the
    /// same stream. The C function copies the whole struct bytewise (L1339) and
    /// then repairs it; this reproduces the observable result field by field,
    /// which removes two hazards the bytewise copy creates -- the three
    /// self-referential `dyn_tree` pointers it has to re-point afterwards
    /// (L1371-L1373, see [`TreeDesc`]) and the four buffer pointers that
    /// momentarily alias the source's.
    ///
    /// Only the live parts of the buffers are copied, exactly as in C:
    ///
    /// * `window[..high_water]` (L1353) -- above the high water mark the bytes
    ///   are the allocator's, in both implementations.
    /// * `prev[..n]` where `n` is [`DeflateState::copied_prev_entries`]
    ///   (L1354-L1356).
    /// * the whole of `head` (L1357).
    /// * `pending_buf[pending_out .. pending_out + pending]`, at the same offset
    ///   (L1359-L1360).
    /// * `sym_buf[..sym_next]` (L1367-L1368).
    ///
    /// The `allocator` argument is the destination's: `deflateCopy` allocates
    /// the copy's buffers through `dest`'s hooks (L1342-L1345), having first
    /// copied the whole `z_stream` -- hooks and `opaque` included -- from the
    /// source (L1333). A caller that wants C's exact behaviour therefore passes
    /// an allocator built from the source's hooks; passing a different one is
    /// also supported, and is why this takes the allocator explicitly rather
    /// than reusing `DeflateState::allocator`.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if any of the four allocations fails, matching
    /// `deflateCopy`'s `Z_MEM_ERROR` (L1347-L1351); blocks already obtained are
    /// released before this returns. [`ReturnCode::STREAM_ERROR`] if a view or
    /// cursor cannot be reconstructed, which is unreachable for a state built by
    /// [`DeflateState::new`].
    pub fn try_clone_in<'b, B>(&self, allocator: B) -> Result<DeflateState<'b, B>, ReturnCode>
    where
        'a: 'b,
        B: Allocator<'b> + Copy,
    {
        // Every size comes from the *source* state rather than from a
        // configuration, because a bytewise struct copy would have carried them
        // over and because `deflateParams` can have changed the level since
        // initialisation without changing any of these.
        let w_bits = self.window.w_bits();
        let w_size = self.window.w_size();
        let hash_bits = self.hash.hash_bits();
        let hash_size = self.hash.hash_size();
        let lit_bufsize = self.pending.lit_bufsize();
        let high_water = self.window.high_water();
        let pending_out = self.pending.pending_out_offset();

        // Allocated in `deflateCopy`'s order (`deflate.c` L1342-L1345), which is
        // also `deflateInit2_`'s, so a failure part-way releases in
        // `deflateEnd`'s order for the reason given in
        // `with_validated_config`.
        let mut window =
            ByteBlock::new(allocator, allocator.allocate_bytes_or_mem_error(w_size, 2)?);
        let mut prev = PosBlock::new(allocator, allocator.allocate_u16s_or_mem_error(w_size)?);
        let mut head = PosBlock::new(allocator, allocator.allocate_u16s_or_mem_error(hash_size)?);
        let mut pending = ByteBlock::new(
            allocator,
            allocator.allocate_bytes_or_mem_error(lit_bufsize, LIT_BUFS)?,
        );

        let source_window = self
            .window
            .region(0, high_water)
            .ok_or(ReturnCode::STREAM_ERROR)?;
        copy_into(window.as_mut(), 0, source_window)?;

        let copied = self.copied_prev_entries();
        let source_prev = self
            .hash
            .prev_entries()
            .get(..copied)
            .ok_or(ReturnCode::STREAM_ERROR)?;
        copy_into(prev.as_mut(), 0, source_prev)?;
        copy_into(head.as_mut(), 0, self.hash.head_entries())?;

        copy_into(pending.as_mut(), pending_out, self.pending.flushable())?;
        copy_into(pending.as_mut(), lit_bufsize, self.pending.symbols())?;

        let (Some(mut window), Some(mut hash), Some(mut pending)) = (
            Window::new(window, w_bits),
            HashChains::new(head, prev, w_bits, hash_bits),
            PendingBuf::new(pending, lit_bufsize),
        ) else {
            return Err(ReturnCode::STREAM_ERROR);
        };

        // The cursors the bytewise struct copy would have brought across.
        window.strstart = self.window.strstart;
        window.lookahead = self.window.lookahead;
        window.block_start = self.window.block_start;
        window.match_start = self.window.match_start;
        window.insert = self.window.insert;
        hash.set_slid(self.hash.slid());
        // The read offset is restored before the count, so that each setter's
        // "does the result still fit" check sees a consistent pair.
        let restored = window.set_high_water(high_water)
            && pending.set_pending_out_offset(pending_out)
            && pending.set_pending(self.pending.pending())
            && pending.set_sym_next(self.pending.sym_next());
        if !restored {
            return Err(ReturnCode::STREAM_ERROR);
        }

        Ok(DeflateState {
            pending,
            hash,
            window,
            status: self.status,
            wrap: self.wrap,
            gzhead: self.gzhead,
            gzindex: self.gzindex,
            method: self.method,
            last_flush: self.last_flush,
            ins_h: self.ins_h,
            hash_shift: self.hash_shift,
            match_length: self.match_length,
            prev_match: self.prev_match,
            match_available: self.match_available,
            prev_length: self.prev_length,
            max_chain_length: self.max_chain_length,
            max_lazy_match: self.max_lazy_match,
            level: self.level,
            strategy: self.strategy,
            good_match: self.good_match,
            nice_match: self.nice_match,
            dyn_ltree: self.dyn_ltree,
            dyn_dtree: self.dyn_dtree,
            bl_tree: self.bl_tree,
            l_desc: self.l_desc,
            d_desc: self.d_desc,
            bl_desc: self.bl_desc,
            bl_count: self.bl_count,
            heap: self.heap,
            heap_len: self.heap_len,
            heap_max: self.heap_max,
            depth: self.depth,
            opt_len: self.opt_len,
            static_len: self.static_len,
            matches: self.matches,
            bi_buf: self.bi_buf,
            bi_valid: self.bi_valid,
            bi_used: self.bi_used,
            allocator,
        })
    }
}

impl<'a, A: Allocator<'a>> DeflateState<'a, A> {
    /// Releases the four buffers explicitly, in `deflateEnd`'s order.
    ///
    /// The memory half of `deflateEnd` (`deflate.c` L1300-L1304), spelled out so
    /// that it reads like the C function:
    ///
    /// ```c
    /// TRY_FREE(strm, strm->state->pending_buf);
    /// TRY_FREE(strm, strm->state->head);
    /// TRY_FREE(strm, strm->state->prev);
    /// TRY_FREE(strm, strm->state->window);
    /// ```
    ///
    /// Simply dropping the state does exactly the same thing in exactly the same
    /// order, because the three views are declared in that order and Rust drops
    /// fields in declaration order; this method exists so that the order is
    /// stated where a reader of `deflateEnd` will look for it.
    ///
    /// The rest of `deflateEnd` is **not** here. Freeing the state object itself
    /// (`ZFREE(strm, strm->state)`, L1306) belongs to the facade, which is what
    /// allocated it; clearing `strm->state` (L1307) is the stream's; and the
    /// return value -- [`ReturnCode::DATA_ERROR`] when the status was
    /// [`Status::Busy`], [`ReturnCode::OK`] otherwise (L1309) -- belongs to
    /// `deflate/mod.rs`, because a destructor cannot report anything.
    pub fn release(self) {
        drop(self.pending.into_inner());
        // `HashChains` surrenders `head` first, matching `deflate.c`
        // L1302-L1303.
        let (head, prev) = self.hash.into_inner();
        drop(head);
        drop(prev);
        drop(self.window.into_inner());
    }

    /// Performs the part of `deflateResetKeep` that belongs to this state.
    ///
    /// From `deflate.c` L644-L677, in order:
    ///
    /// * `s->pending = 0; s->pending_out = s->pending_buf;` (L656-L657).
    /// * `if (s->wrap < 0) s->wrap = -s->wrap;` -- undo the negation
    ///   `deflate()` applies once the trailer is written (L659-L661).
    /// * `s->status = s->wrap == 2 ? GZIP_STATE : INIT_STATE;` (L662-L666).
    /// * `s->last_flush = -2;` (L672).
    ///
    /// The rest of that function is the caller's, because it is not state:
    /// `strm->total_in`, `strm->total_out`, `strm->msg`, `strm->data_type` and
    /// the initial `strm->adler` (L651-L653, L667-L671) belong to the stream, and
    /// `_tr_init(s)` (L674) belongs to `trees`. `deflateReset` additionally runs
    /// `lm_init` (L709), whose four table lookups are
    /// [`DeflateState::set_tuning`].
    pub fn reset_keep(&mut self) {
        self.pending.reset_pending();
        self.wrap = self.wrap.wrapping_abs();
        self.status = if self.wrap == Wrap::Gzip.as_deflate_wrap() {
            Status::GzipHeader
        } else {
            Status::Init
        };
        self.last_flush = -2;
    }

    /// Loads the four match-finding tuning parameters.
    ///
    /// These are the four assignments `lm_init` makes from
    /// `configuration_table[s->level]` (`deflate.c` L689-L692), which
    /// `deflateParams` repeats on a level change (L809-L812) and `deflateTune`
    /// performs from caller-supplied values (L825-L828). The table itself lives
    /// in `deflate/config_table.rs`, so this takes the four numbers rather than
    /// a level -- which is also what makes `deflateTune` expressible.
    ///
    /// They are byte-identity critical: together they decide which candidate
    /// matches `longest_match` examines and which it accepts, and therefore the
    /// emitted bytes.
    pub fn set_tuning(
        &mut self,
        good_match: usize,
        max_lazy_match: usize,
        nice_match: i32,
        max_chain_length: usize,
    ) {
        self.good_match = good_match;
        self.max_lazy_match = max_lazy_match;
        self.nice_match = nice_match;
        self.max_chain_length = max_chain_length;
    }

    /// Installs, or clears, the caller's gzip header.
    ///
    /// The whole of `deflateSetHeader`'s effect: `strm->state->gzhead = head;`
    /// (`deflate.c` L717). The guards it applies first -- a valid stream and
    /// `wrap == 2`, otherwise `Z_STREAM_ERROR` (L715-L716) -- belong to
    /// `deflate/mod.rs`, which owns the entry point.
    pub fn set_gzhead(&mut self, gzhead: Option<GzHeaderView<'a>>) {
        self.gzhead = gzhead;
    }

    /// How many `prev` entries [`DeflateState::try_clone_in`] copies.
    ///
    /// The port of `deflateCopy`'s expression (`deflate.c` L1354-L1356):
    ///
    /// ```c
    /// (ss->slid || ss->strstart - ss->insert > ds->w_size ? ds->w_size
    ///                                                    : ss->strstart - ss->insert)
    /// ```
    ///
    /// The hash tables have been slid, so any entry may be live and the whole
    /// array must come across; otherwise only the positions between `insert` and
    /// `strstart` have been linked, and copying those is enough.
    ///
    /// C evaluates `strstart - insert` in unsigned arithmetic, so if `insert`
    /// were ever to exceed `strstart` the difference would wrap to an enormous
    /// value, the `> w_size` test would hold, and the whole array would be
    /// copied. `checked_sub` returning [`None`] is that same case, and is
    /// treated the same way, which makes the two implementations agree without
    /// relying on wraparound. (`fill_window` and `deflate_stored` clamp `insert`
    /// to `strstart` after every slide, so the case does not arise --
    /// `deflate.c` L291-L292, L1777-L1778, L1810-L1811.)
    #[must_use]
    pub fn copied_prev_entries(&self) -> usize {
        let w_size = self.window.w_size();
        if self.hash.slid() {
            return w_size;
        }
        match self.window.strstart.checked_sub(self.window.insert) {
            Some(span) if span <= w_size => span,
            _ => w_size,
        }
    }

    /// The dynamic tree a descriptor operates on.
    ///
    /// The replacement for following `tree_desc.dyn_tree` (`deflate.h` L91); see
    /// [`TreeDesc`] for why the pointer is gone. The pairing is the one
    /// `_tr_init` establishes at `trees.c` L459-L466.
    #[must_use]
    pub fn tree_for(&self, kind: StaticTreeKind) -> &[CtData] {
        match kind {
            StaticTreeKind::Literal => &self.dyn_ltree,
            StaticTreeKind::Distance => &self.dyn_dtree,
            StaticTreeKind::BitLength => &self.bl_tree,
        }
    }

    /// The dynamic tree a descriptor operates on, mutably.
    ///
    /// The counterpart of [`DeflateState::tree_for`], and the form
    /// `build_tree` (`trees.c` L627-L710), `gen_bitlen` (L540-L625) and
    /// `gen_codes` (L203-L232) need.
    pub fn tree_for_mut(&mut self, kind: StaticTreeKind) -> &mut [CtData] {
        match kind {
            StaticTreeKind::Literal => &mut self.dyn_ltree,
            StaticTreeKind::Distance => &mut self.dyn_dtree,
            StaticTreeKind::BitLength => &mut self.bl_tree,
        }
    }

    /// The descriptor for a static tree kind: `l_desc`, `d_desc` or `bl_desc`
    /// (`deflate.h` L206-L208).
    #[must_use]
    pub fn desc_for(&self, kind: StaticTreeKind) -> &TreeDesc {
        match kind {
            StaticTreeKind::Literal => &self.l_desc,
            StaticTreeKind::Distance => &self.d_desc,
            StaticTreeKind::BitLength => &self.bl_desc,
        }
    }

    /// The descriptor for a static tree kind, mutably -- the form `build_tree`
    /// needs in order to record `max_code` (`trees.c` L668).
    pub fn desc_for_mut(&mut self, kind: StaticTreeKind) -> &mut TreeDesc {
        match kind {
            StaticTreeKind::Literal => &mut self.l_desc,
            StaticTreeKind::Distance => &mut self.d_desc,
            StaticTreeKind::BitLength => &mut self.bl_desc,
        }
    }

    /// The largest match distance the compressor will emit.
    ///
    /// Port of `#define MAX_DIST(s) ((s)->w_size-MIN_LOOKAHEAD)`
    /// (`deflate.h` L301), which is a macro over the state rather than a
    /// constant because it depends on the window size. "In order to simplify the
    /// code, particularly on 16 bit machines, match distances are limited to
    /// `MAX_DIST` instead of `WSIZE`" (L302-L304); `longest_match` turns it into
    /// the chain-walk limit at `deflate.c` L1396-L1397.
    #[must_use]
    #[inline]
    pub fn max_dist(&self) -> usize {
        self.window.max_dist()
    }

    /// The second name of `DeflateState::max_lazy_match`, from
    /// `#define max_insert_length max_lazy_match` (`deflate.h` L186).
    ///
    /// One field, two names: `deflate_fast` reads it as the longest match after
    /// which it will *stop inserting* into the hash chains
    /// (`deflate.c` L1906), while `deflate_slow` reads the same storage as the
    /// match length below which it will *try a lazy match* (L1988). This
    /// accessor exists so that the `deflate_fast` code can be written the way
    /// the C is.
    #[must_use]
    #[inline]
    pub const fn max_insert_length(&self) -> usize {
        self.max_lazy_match
    }

    /// The container this stream writes, irrespective of the sign
    /// `DeflateState::wrap` currently carries.
    ///
    /// Reproduces `s->wrap < 0 ? -s->wrap : s->wrap`, which `deflateBound` uses
    /// to pick a wrapper length (`deflate.c` L883). [`None`] would mean `wrap`
    /// holds something other than 0, 1 or 2, which no path here can produce.
    #[must_use]
    pub fn container(&self) -> Option<Wrap> {
        Wrap::from_deflate_wrap(self.wrap.wrapping_abs())
    }

    /// Whether the strategy suppresses string matching, i.e. the C test
    /// `s->strategy >= Z_HUFFMAN_ONLY`.
    ///
    /// That ordered comparison appears three times -- choosing the zlib header's
    /// level bits (`deflate.c` L1036), choosing the gzip header's `XFL` byte
    /// (L1079 and L1103) -- and depends on the numeric order of the five
    /// strategies, which is why it goes through `Strategy::as_raw` rather than
    /// through a derived ordering on the enum.
    #[must_use]
    pub fn favours_huffman_only(&self) -> bool {
        self.strategy.as_raw() >= Strategy::HuffmanOnly.as_raw()
    }

    /// `int status` (`deflate.h` L106).
    #[must_use]
    #[inline]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// `int wrap` (`deflate.h` L111), sign included. See
    /// [`DeflateState::container`].
    #[must_use]
    #[inline]
    pub const fn wrap(&self) -> i32 {
        self.wrap
    }

    /// `int level` (`deflate.h` L192).
    #[must_use]
    #[inline]
    pub const fn level(&self) -> i32 {
        self.level
    }

    /// `int strategy` (`deflate.h` L193).
    #[must_use]
    #[inline]
    pub const fn strategy(&self) -> Strategy {
        self.strategy
    }

    /// `Byte method` (`deflate.h` L114).
    #[must_use]
    #[inline]
    pub const fn method(&self) -> Method {
        self.method
    }

    /// `int last_flush` (`deflate.h` L115), sentinels included.
    #[must_use]
    #[inline]
    pub const fn last_flush(&self) -> i32 {
        self.last_flush
    }

    /// `gz_headerp gzhead` (`deflate.h` L112).
    #[must_use]
    #[inline]
    pub const fn gzhead(&self) -> Option<GzHeaderView<'a>> {
        self.gzhead
    }

    /// `uInt w_bits` (`deflate.h` L120).
    #[must_use]
    #[inline]
    pub const fn w_bits(&self) -> u32 {
        self.window.w_bits()
    }

    /// `uInt w_size` (`deflate.h` L119), the LZ77 window size.
    #[must_use]
    #[inline]
    pub const fn w_size(&self) -> usize {
        self.window.w_size()
    }

    /// `uInt w_mask` (`deflate.h` L121), which is `w_size - 1`.
    #[must_use]
    #[inline]
    pub const fn w_mask(&self) -> usize {
        self.window.w_mask()
    }

    /// `ulg window_size` (`deflate.h` L133), which is `2 * w_size`.
    #[must_use]
    #[inline]
    pub const fn window_size(&self) -> usize {
        self.window.window_size()
    }

    /// `ulg high_water` (`deflate.h` L278).
    #[must_use]
    #[inline]
    pub const fn high_water(&self) -> usize {
        self.window.high_water()
    }

    /// `uInt hash_size` (`deflate.h` L147), the number of hash chains.
    #[must_use]
    #[inline]
    pub const fn hash_size(&self) -> usize {
        self.hash.hash_size()
    }

    /// `uInt hash_bits` (`deflate.h` L148), which is `memLevel + 7`.
    #[must_use]
    #[inline]
    pub const fn hash_bits(&self) -> u32 {
        self.hash.hash_bits()
    }

    /// `uInt hash_mask` (`deflate.h` L149), which is `hash_size - 1`.
    #[must_use]
    #[inline]
    pub const fn hash_mask(&self) -> usize {
        self.hash.hash_mask()
    }

    /// `int slid` (`deflate.h` L285): "True if the hash table has been slid
    /// since it was cleared."
    #[must_use]
    #[inline]
    pub const fn slid(&self) -> bool {
        self.hash.slid()
    }

    /// `uInt lit_bufsize` (`deflate.h` L233).
    #[must_use]
    #[inline]
    pub const fn lit_bufsize(&self) -> usize {
        self.pending.lit_bufsize()
    }

    /// `ulg pending_buf_size` (`deflate.h` L108), which is `lit_bufsize * 4`.
    #[must_use]
    #[inline]
    pub const fn pending_buf_size(&self) -> usize {
        self.pending.pending_buf_size()
    }

    /// `ulg pending` (`deflate.h` L110), the bytes of compressed output waiting
    /// to be flushed. This is what `deflatePending` reports
    /// (`deflate.c` L727).
    #[must_use]
    #[inline]
    pub const fn pending_bytes(&self) -> usize {
        self.pending.pending()
    }

    /// `uInt sym_next` (`deflate.h` L253), the running byte offset within the
    /// symbol buffer.
    #[must_use]
    #[inline]
    pub const fn sym_next(&self) -> usize {
        self.pending.sym_next()
    }

    /// `uInt sym_end` (`deflate.h` L254): the symbol buffer is full when
    /// `sym_next` reaches this.
    #[must_use]
    #[inline]
    pub const fn sym_end(&self) -> usize {
        self.pending.sym_end()
    }

    /// The allocator every buffer came from: the Rust form of the
    /// `(zalloc, zfree, opaque)` triple the caller supplies in `z_stream`
    /// (`zlib.h` L102-L104), which C reaches through the omitted `strm`
    /// back-pointer (`deflate.h` L105).
    #[must_use]
    #[inline]
    pub fn allocator(&self) -> &A {
        &self.allocator
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for DeflateState<'a, A> {
    /// Reports the configuration and the cursors, never the buffers.
    ///
    /// Written out rather than derived for three reasons: a derived
    /// implementation would require `A: Debug`, which no allocator is obliged to
    /// be; it would print three arrays of 573 entries and two more of 573 and 16,
    /// which is unreadable; and it would print buffer contents, which for a
    /// compressor is caller data.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeflateState")
            .field("status", &self.status)
            .field("wrap", &self.wrap)
            .field("level", &self.level)
            .field("strategy", &self.strategy)
            .field("w_bits", &self.w_bits())
            .field("hash_bits", &self.hash_bits())
            .field("lit_bufsize", &self.lit_bufsize())
            .field("strstart", &self.window.strstart)
            .field("lookahead", &self.window.lookahead)
            .field("block_start", &self.window.block_start)
            .field("high_water", &self.high_water())
            .field("pending", &self.pending_bytes())
            .field("sym_next", &self.sym_next())
            .field("last_flush", &self.last_flush)
            .finish_non_exhaustive()
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
// The workspace denies the panic-prone lints in library code, which is the right
// policy there and the wrong one in a harness: a test asserts, and an assertion
// that fails panics. Indexing is allowed too, because every expectation below is
// written against a literal, known-good index.
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::{
        CtData, DeflateConfig, DeflateState, GzHeaderView, StaticTreeKind, Status, BL_CODES,
        BL_COUNT_LEN, BL_TREE_LEN, BUF_SIZE, DEPTH_ARRAY_LEN, DYN_DTREE_LEN, DYN_LTREE_LEN,
        DYN_TREES, D_CODES, HEAP_ARRAY_LEN, HEAP_SIZE, LENGTH_CODES, LITERALS, LIT_BUFS, L_CODES,
        MAX_BITS, MAX_MATCH, MIN_LOOKAHEAD, MIN_MATCH, NIL, PRESET_DICT, STATIC_TREES,
        STORED_BLOCK, SYMBOL_BYTES, WIN_INIT,
    };
    use crate::allocate::{Allocator, AllocatorId, Buffer, GlobalAllocator, Opaque};
    use crate::config::{Method, Strategy, Wrap, DEF_MEM_LEVEL, MAX_WBITS};
    use crate::error::ReturnCode;
    use crate::weak_slice::Pos;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    /// The shape of one release, recorded by [`Logger`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Released {
        /// A byte block of this many bytes: `pending_buf` or `window`.
        Bytes(usize),
        /// A `Pos` block of this many entries: `head` or `prev`.
        Entries(usize),
    }

    /// An allocator that delegates to [`GlobalAllocator`] and records the order
    /// in which blocks come back.
    ///
    /// It claims [`AllocatorId::GLOBAL`], which is truthful rather than a cheat:
    /// every block it hands out *is* a Rust global allocation, so the reserved
    /// identity is the correct one and `Buffer::release_to` must accept it. The
    /// type observes the calls and forwards them unchanged.
    #[derive(Debug, Default)]
    struct Logger {
        /// Releases, in the order they happened.
        order: RefCell<Vec<Released>>,
        /// Successful allocations, of either shape.
        allocated: RefCell<usize>,
    }

    impl<'a> Allocator<'a> for Logger {
        fn id(&self) -> AllocatorId {
            AllocatorId::GLOBAL
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
            let buffer = GlobalAllocator.allocate_bytes(items, size)?;
            *self.allocated.borrow_mut() += 1;
            Some(buffer)
        }

        fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
            let buffer = GlobalAllocator.allocate_u16s(items)?;
            *self.allocated.borrow_mut() += 1;
            Some(buffer)
        }

        fn deallocate_bytes(&self, buffer: Buffer<'a, u8>) {
            self.order.borrow_mut().push(Released::Bytes(buffer.len()));
            GlobalAllocator.deallocate_bytes(buffer);
        }

        fn deallocate_u16s(&self, buffer: Buffer<'a, u16>) {
            self.order
                .borrow_mut()
                .push(Released::Entries(buffer.len()));
            GlobalAllocator.deallocate_u16s(buffer);
        }
    }

    /// An allocator that grants a fixed number of requests and then declines,
    /// the way `test/infcover.c`'s `mem_limit` (L176-L181) does.
    ///
    /// Like [`Logger`] it delegates to [`GlobalAllocator`] and so claims the
    /// same, truthful identity; see that type for why.
    #[derive(Debug)]
    struct Budget {
        /// Requests still to be granted.
        remaining: RefCell<usize>,
        /// Requests granted so far.
        granted: RefCell<usize>,
        /// Blocks released so far.
        released: RefCell<usize>,
    }

    impl Budget {
        fn new(limit: usize) -> Self {
            Self {
                remaining: RefCell::new(limit),
                granted: RefCell::new(0),
                released: RefCell::new(0),
            }
        }

        fn take(&self) -> bool {
            let mut remaining = self.remaining.borrow_mut();
            if *remaining == 0 {
                return false;
            }
            *remaining -= 1;
            *self.granted.borrow_mut() += 1;
            true
        }
    }

    impl<'a> Allocator<'a> for Budget {
        fn id(&self) -> AllocatorId {
            AllocatorId::GLOBAL
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
            if !self.take() {
                return None;
            }
            GlobalAllocator.allocate_bytes(items, size)
        }

        fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
            if !self.take() {
                return None;
            }
            GlobalAllocator.allocate_u16s(items)
        }

        fn deallocate_bytes(&self, buffer: Buffer<'a, u8>) {
            *self.released.borrow_mut() += 1;
            GlobalAllocator.deallocate_bytes(buffer);
        }

        fn deallocate_u16s(&self, buffer: Buffer<'a, u16>) {
            *self.released.borrow_mut() += 1;
            GlobalAllocator.deallocate_u16s(buffer);
        }
    }

    /// A configuration with every parameter chosen explicitly.
    fn config(level: i32, window_bits: i32, mem_level: i32) -> DeflateConfig {
        DeflateConfig {
            level,
            method: Method::Deflated,
            window_bits,
            mem_level,
            strategy: Strategy::Default,
        }
    }

    #[test]
    fn derived_sizes_match_deflate_init2_at_the_defaults() {
        // `deflateInit_`'s defaults: MAX_WBITS and DEF_MEM_LEVEL
        // (`deflate.c` L381-L382).
        let state = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();

        assert_eq!(MAX_WBITS, 15);
        assert_eq!(DEF_MEM_LEVEL, 8);

        // `deflate.c` L449-L451.
        assert_eq!(state.w_bits(), 15);
        assert_eq!(state.w_size(), 32_768);
        assert_eq!(state.w_mask(), 32_767);
        // `window_size = 2 * w_size` (`deflate.c` L683).
        assert_eq!(state.window_size(), 65_536);
        // `deflate.c` L453-L456.
        assert_eq!(state.hash_bits(), 15);
        assert_eq!(state.hash_size(), 32_768);
        assert_eq!(state.hash_mask(), 32_767);
        assert_eq!(state.hash_shift, 5);
        // `deflate.c` L464, L506 and L521.
        assert_eq!(state.lit_bufsize(), 16_384);
        assert_eq!(state.pending_buf_size(), 65_536);
        assert_eq!(state.sym_end(), 49_149);
        // `deflate.c` L445-L448 and L462, L528-L530.
        assert_eq!(state.status(), Status::Init);
        assert_eq!(state.wrap(), 1);
        assert_eq!(state.container(), Some(Wrap::Zlib));
        assert_eq!(state.gzhead(), None);
        assert_eq!(state.high_water(), 0);
        assert_eq!(state.level(), 6);
        assert_eq!(state.strategy(), Strategy::Default);
        assert_eq!(state.method(), Method::Deflated);
        // The reset half has not run yet, so these are still `zmemzero`'s zeros.
        assert_eq!(state.last_flush(), 0);
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(state.sym_next(), 0);
    }

    #[test]
    fn derived_sizes_match_deflate_init2_at_the_extremes() {
        // `memLevel` 1: the smallest hash table and symbol buffer
        // (`deflate.c` L453-L456, L464).
        let low = DeflateState::new(config(1, 15, 1), GlobalAllocator).unwrap();
        assert_eq!(low.hash_bits(), 8);
        assert_eq!(low.hash_size(), 256);
        assert_eq!(low.hash_mask(), 255);
        assert_eq!(low.hash_shift, 3);
        assert_eq!(low.lit_bufsize(), 128);
        assert_eq!(low.pending_buf_size(), 512);
        assert_eq!(low.sym_end(), 381);

        // `memLevel` 9, the maximum `MAX_MEM_LEVEL` allows.
        let high = DeflateState::new(config(9, 15, 9), GlobalAllocator).unwrap();
        assert_eq!(high.hash_bits(), 16);
        assert_eq!(high.hash_size(), 65_536);
        assert_eq!(high.hash_mask(), 65_535);
        assert_eq!(high.hash_shift, 6);
        assert_eq!(high.lit_bufsize(), 32_768);
        assert_eq!(high.pending_buf_size(), 131_072);
        assert_eq!(high.sym_end(), 98_301);

        // The smallest window a live state can have, once 8 has been promoted.
        let narrow = DeflateState::new(config(6, 9, 8), GlobalAllocator).unwrap();
        assert_eq!(narrow.w_bits(), 9);
        assert_eq!(narrow.w_size(), 512);
        assert_eq!(narrow.w_mask(), 511);
        assert_eq!(narrow.window_size(), 1_024);
        assert_eq!(narrow.max_dist(), 512 - MIN_LOOKAHEAD);

        // Both container variants, and the sign convention (`deflate.c` L423,
        // L430).
        let raw = DeflateState::new(config(6, -15, 8), GlobalAllocator).unwrap();
        assert_eq!(raw.wrap(), 0);
        assert_eq!(raw.container(), Some(Wrap::None));
        assert_eq!(raw.w_bits(), 15);
        let gzip = DeflateState::new(config(6, 31, 8), GlobalAllocator).unwrap();
        assert_eq!(gzip.wrap(), 2);
        assert_eq!(gzip.container(), Some(Wrap::Gzip));
        assert_eq!(gzip.w_bits(), 15);
    }

    #[test]
    fn window_bits_of_eight_is_promoted_to_nine() {
        // "until 256-byte window bug fixed" (`deflate.c` L439). The promotion
        // happens during validation, but the state is where it becomes visible.
        let state = DeflateState::new(config(6, 8, 8), GlobalAllocator).unwrap();
        assert_eq!(state.w_bits(), 9);
        assert_eq!(state.w_size(), 512);

        // `windowBits == 8` is legal only for the zlib container
        // (`deflate.c` L436), so the raw and gzip spellings are rejected.
        assert_eq!(
            DeflateState::new(config(6, -8, 8), GlobalAllocator).unwrap_err(),
            ReturnCode::STREAM_ERROR
        );
        assert_eq!(
            DeflateState::new(config(6, 24, 8), GlobalAllocator).unwrap_err(),
            ReturnCode::STREAM_ERROR
        );
    }

    #[test]
    fn constants_match_their_c_defines() {
        // `deflate.h` L34-L56.
        assert_eq!(LENGTH_CODES, 29);
        assert_eq!(LITERALS, 256);
        assert_eq!(L_CODES, 286);
        assert_eq!(D_CODES, 30);
        assert_eq!(BL_CODES, 19);
        assert_eq!(HEAP_SIZE, 573);
        assert_eq!(MAX_BITS, 15);
        assert_eq!(BUF_SIZE, 16);
        // `zutil.h` L87-L96 and `deflate.h` L296, L306, L229.
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(MAX_MATCH, 258);
        assert_eq!(MIN_LOOKAHEAD, 262);
        assert_eq!(WIN_INIT, 258);
        assert_eq!(LIT_BUFS, 4);
        assert_eq!(SYMBOL_BYTES, 3);
        assert_eq!(NIL, 0);
        assert_eq!(Pos::NIL.get(), NIL);
        assert_eq!(STORED_BLOCK, 0);
        assert_eq!(STATIC_TREES, 1);
        assert_eq!(DYN_TREES, 2);
        assert_eq!(PRESET_DICT, 0x20);
    }

    #[test]
    fn array_lengths_match_the_c_declarations() {
        // `deflate.h` L202-L220.
        assert_eq!(DYN_LTREE_LEN, 573);
        assert_eq!(DYN_DTREE_LEN, 61);
        assert_eq!(BL_TREE_LEN, 39);
        assert_eq!(BL_COUNT_LEN, 16);
        assert_eq!(HEAP_ARRAY_LEN, 573);
        assert_eq!(DEPTH_ARRAY_LEN, 573);

        let state = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();
        assert_eq!(state.dyn_ltree.len(), DYN_LTREE_LEN);
        assert_eq!(state.dyn_dtree.len(), DYN_DTREE_LEN);
        assert_eq!(state.bl_tree.len(), BL_TREE_LEN);
        assert_eq!(state.bl_count.len(), BL_COUNT_LEN);
        assert_eq!(state.heap.len(), HEAP_ARRAY_LEN);
        assert_eq!(state.depth.len(), DEPTH_ARRAY_LEN);
        // Every entry starts zeroed, as `zmemzero` leaves them
        // (`deflate.c` L442).
        assert!(state
            .dyn_ltree
            .iter()
            .all(|entry| *entry == CtData::new(0, 0)));
        assert!(state.heap.iter().all(|slot| *slot == 0));
        assert!(state.depth.iter().all(|slot| *slot == 0));
    }

    #[test]
    fn hash_shift_invariant_holds_for_every_mem_level() {
        // "It must be such that after MIN_MATCH steps, the oldest byte no longer
        // takes part in the hash key, that is: hash_shift * MIN_MATCH >=
        // hash_bits" (`deflate.h` L152-L155).
        for mem_level in 1..=9 {
            let state = DeflateState::new(config(6, 15, mem_level), GlobalAllocator).unwrap();
            let hash_bits = usize::try_from(state.hash_bits()).unwrap();
            assert_eq!(hash_bits, usize::try_from(mem_level).unwrap() + 7);
            assert!(
                state.hash_shift * MIN_MATCH >= hash_bits,
                "memLevel {mem_level}: hash_shift {} is too small for hash_bits {hash_bits}",
                state.hash_shift
            );
            // And it is the smallest such value, i.e. the division really is
            // rounded up rather than over-approximated.
            assert!((state.hash_shift - 1) * MIN_MATCH < hash_bits);
            // Every window size agrees with `MAX_DIST`.
            assert_eq!(state.max_dist(), state.w_size() - MIN_LOOKAHEAD);
        }
    }

    #[test]
    fn status_round_trips_through_its_c_values() {
        // `deflate.h` L58-L67.
        let expected = [42, 57, 69, 73, 91, 103, 113, 666];
        for (status, raw) in Status::ALL.iter().zip(expected) {
            assert_eq!(status.as_raw(), raw);
            assert_eq!(Status::from_raw(raw), Some(*status));
        }
        // The values `deflateStateCheck` rejects (`deflate.c` L544-L553).
        for raw in [-1, 0, 41, 43, 56, 112, 114, 665, 667] {
            assert_eq!(Status::from_raw(raw), None);
        }
        // The four states that dereference `gzhead` (`deflate.c` L1117-L1200).
        assert!(Status::Extra.writes_gzip_header_field());
        assert!(Status::Name.writes_gzip_header_field());
        assert!(Status::Comment.writes_gzip_header_field());
        assert!(Status::Hcrc.writes_gzip_header_field());
        assert!(!Status::Init.writes_gzip_header_field());
        assert!(!Status::GzipHeader.writes_gzip_header_field());
        assert!(!Status::Busy.writes_gzip_header_field());
        assert!(!Status::Finish.writes_gzip_header_field());
        assert_eq!(Status::Busy.c_name(), "BUSY_STATE");
    }

    #[test]
    fn ct_data_unions_alias_as_the_c_macros_do() {
        // `Freq` and `Code` are one union member (`deflate.h` L73-L76, L83-L84),
        // and so are `Dad` and `Len` (L77-L80, L85-L86).
        let mut entry = CtData::default();
        assert_eq!(entry, CtData::new(0, 0));
        assert_eq!(entry.freq(), 0);
        assert_eq!(entry.len(), 0);

        entry.set_freq(7);
        assert_eq!(entry.freq(), 7);
        assert_eq!(entry.code(), 7);
        entry.set_code(0x1234);
        assert_eq!(entry.freq(), 0x1234);

        entry.set_dad(9);
        assert_eq!(entry.dad(), 9);
        assert_eq!(entry.len(), 9);
        entry.set_len(11);
        assert_eq!(entry.dad(), 11);
        // The two halves are independent of one another.
        assert_eq!(entry.code(), 0x1234);

        // `s->dyn_ltree[cc].Freq++` (`deflate.h` L362).
        let mut counter = CtData::new(0, 0);
        counter.increment_freq();
        counter.increment_freq();
        assert_eq!(counter.freq(), 2);
        // Saturating rather than wrapping, which is unreachable in practice
        // because a block holds at most `lit_bufsize - 1` symbols.
        let mut full = CtData::new(u16::MAX, 0);
        full.increment_freq();
        assert_eq!(full.freq(), u16::MAX);

        // A static tree entry is a code and its length: `trees.h` spells the
        // first one `{{12},{8}}`.
        let static_entry = CtData::new(12, 8);
        assert_eq!(static_entry.code(), 12);
        assert_eq!(static_entry.len(), 8);
    }

    #[test]
    fn tree_descriptors_are_paired_at_construction() {
        // The work `_tr_init` does at `trees.c` L459-L466, without the
        // self-referential pointers.
        let mut state = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();
        assert_eq!(state.l_desc.stat_desc, StaticTreeKind::Literal);
        assert_eq!(state.d_desc.stat_desc, StaticTreeKind::Distance);
        assert_eq!(state.bl_desc.stat_desc, StaticTreeKind::BitLength);
        assert_eq!(state.l_desc.max_code, 0);

        assert_eq!(state.tree_for(StaticTreeKind::Literal).len(), DYN_LTREE_LEN);
        assert_eq!(
            state.tree_for(StaticTreeKind::Distance).len(),
            DYN_DTREE_LEN
        );
        assert_eq!(state.tree_for(StaticTreeKind::BitLength).len(), BL_TREE_LEN);

        // Writing through the selector reaches the array the descriptor is
        // paired with, which is what following `dyn_tree` did.
        for kind in StaticTreeKind::ALL {
            state.tree_for_mut(kind)[1].set_freq(42);
            state.desc_for_mut(kind).max_code = 5;
            assert_eq!(state.tree_for(kind)[1].freq(), 42);
            assert_eq!(state.desc_for(kind).max_code, 5);
        }
        assert_eq!(state.dyn_ltree[1].freq(), 42);
        assert_eq!(state.dyn_dtree[1].freq(), 42);
        assert_eq!(state.bl_tree[1].freq(), 42);
        assert_eq!(StaticTreeKind::Distance.c_name(), "static_d_desc");
    }

    #[test]
    fn release_order_matches_deflate_end() {
        // "Deallocate in reverse order of allocations": pending_buf, head, prev,
        // window (`deflate.c` L1300-L1304). A departure from that order is what
        // `test/infcover.c` counts as `notlifo`.
        let logger = Logger::default();
        // `windowBits` 9 and `memLevel` 1 make all four shapes distinguishable:
        // pending_buf 4*128 bytes, head 256 entries, prev 512 entries, window
        // 2*512 bytes.
        let state = DeflateState::new(config(6, 9, 1), &logger).unwrap();
        assert_eq!(*logger.allocated.borrow(), 4);
        assert!(logger.order.borrow().is_empty());
        drop(state);
        assert_eq!(
            *logger.order.borrow(),
            [
                Released::Bytes(512),
                Released::Entries(256),
                Released::Entries(512),
                Released::Bytes(1_024),
            ]
        );
    }

    #[test]
    fn explicit_release_uses_the_same_order() {
        let logger = Logger::default();
        let state = DeflateState::new(config(6, 9, 1), &logger).unwrap();
        state.release();
        assert_eq!(
            *logger.order.borrow(),
            [
                Released::Bytes(512),
                Released::Entries(256),
                Released::Entries(512),
                Released::Bytes(1_024),
            ]
        );
    }

    #[test]
    fn a_failed_allocation_releases_what_it_already_took() {
        // `deflateInit2_` reports `Z_MEM_ERROR` and calls `deflateEnd` on the
        // failure path (`deflate.c` L508-L514), so nothing it obtained is leaked.
        for limit in 0..4 {
            let budget = Budget::new(limit);
            let outcome = DeflateState::new(DeflateConfig::new(6), &budget);
            assert_eq!(outcome.unwrap_err(), ReturnCode::MEM_ERROR);
            assert_eq!(*budget.granted.borrow(), limit);
            assert_eq!(*budget.released.borrow(), limit);
        }
        // With the fourth request granted the state is built, and tearing it
        // down returns all four.
        let budget = Budget::new(4);
        let state = DeflateState::new(DeflateConfig::new(6), &budget).unwrap();
        assert_eq!(*budget.granted.borrow(), 4);
        assert_eq!(*budget.released.borrow(), 0);
        drop(state);
        assert_eq!(*budget.released.borrow(), 4);
    }

    #[test]
    fn invalid_configurations_are_rejected_without_allocating() {
        // The `||` chain at `deflate.c` L434-L437, every term of which returns
        // `Z_STREAM_ERROR`.
        let rejected = [
            config(10, 15, 8),
            config(-2, 15, 8),
            config(6, 16, 8),
            config(6, -16, 8),
            config(6, 7, 8),
            config(6, 15, 0),
            config(6, 15, 10),
        ];
        for candidate in rejected {
            let budget = Budget::new(4);
            assert_eq!(
                DeflateState::new(candidate, &budget).unwrap_err(),
                ReturnCode::STREAM_ERROR,
                "{candidate:?} should have been rejected"
            );
            assert_eq!(*budget.granted.borrow(), 0);
        }
    }

    #[test]
    fn reset_keep_reproduces_its_c_assignments() {
        // `deflate.c` L644-L677.
        let mut zlib = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();
        assert!(zlib.pending.put_byte(0xab));
        assert_eq!(zlib.pending_bytes(), 1);
        // The negation `deflate()` applies once the trailer is written (L1288).
        zlib.wrap = -zlib.wrap;
        zlib.status = Status::Finish;
        zlib.reset_keep();
        assert_eq!(zlib.wrap(), 1);
        assert_eq!(zlib.status(), Status::Init);
        assert_eq!(zlib.last_flush(), -2);
        assert_eq!(zlib.pending_bytes(), 0);

        // `wrap == 2` starts in the gzip state instead (L662-L666).
        let mut gzip = DeflateState::new(config(6, 31, 8), GlobalAllocator).unwrap();
        gzip.reset_keep();
        assert_eq!(gzip.status(), Status::GzipHeader);
        assert_eq!(gzip.wrap(), 2);

        // A raw stream has no trailer to negate and starts in `INIT_STATE`.
        let mut raw = DeflateState::new(config(6, -15, 8), GlobalAllocator).unwrap();
        raw.reset_keep();
        assert_eq!(raw.status(), Status::Init);
        assert_eq!(raw.wrap(), 0);
    }

    #[test]
    fn tuning_parameters_are_loaded_as_lm_init_loads_them() {
        // The four assignments at `deflate.c` L689-L692, here with level 9's row
        // of `configuration_table` -- {32, 258, 258, 4096} (L124).
        let mut state = DeflateState::new(config(9, 15, 8), GlobalAllocator).unwrap();
        state.set_tuning(32, 258, 258, 4_096);
        assert_eq!(state.good_match, 32);
        assert_eq!(state.max_lazy_match, 258);
        assert_eq!(state.max_insert_length(), 258);
        assert_eq!(state.nice_match, 258);
        assert_eq!(state.max_chain_length, 4_096);

        // `nice_match` is signed, which `deflateTune` relies on (L827).
        state.set_tuning(0, 0, -1, 0);
        assert_eq!(state.nice_match, -1);
    }

    #[test]
    fn strategy_ordering_survives_the_port() {
        // `s->strategy >= Z_HUFFMAN_ONLY` (`deflate.c` L1036, L1079, L1103).
        for (strategy, favours) in [
            (Strategy::Default, false),
            (Strategy::Filtered, false),
            (Strategy::HuffmanOnly, true),
            (Strategy::Rle, true),
            (Strategy::Fixed, true),
        ] {
            let mut requested = DeflateConfig::new(6);
            requested.strategy = strategy;
            let state = DeflateState::new(requested, GlobalAllocator).unwrap();
            assert_eq!(state.favours_huffman_only(), favours);
            assert_eq!(state.strategy(), strategy);
        }
    }

    #[test]
    fn copied_prev_entries_follows_the_slid_rule() {
        // `(ss->slid || ss->strstart - ss->insert > ds->w_size ? ds->w_size :
        //   ss->strstart - ss->insert)` (`deflate.c` L1354-L1356).
        let mut state = DeflateState::new(config(6, 9, 1), GlobalAllocator).unwrap();
        let w_size = state.w_size();

        // Nothing linked yet.
        assert_eq!(state.copied_prev_entries(), 0);

        // A partly filled window that has not slid: only the linked span.
        state.window.strstart = 300;
        state.window.insert = 100;
        assert!(!state.slid());
        assert_eq!(state.copied_prev_entries(), 200);

        // Once the tables have been slid, every entry may be live.
        state.hash.set_slid(true);
        assert_eq!(state.copied_prev_entries(), w_size);

        // A span wider than the array is clamped, which is the `> w_size` term.
        state.hash.set_slid(false);
        state.window.strstart = w_size + 400;
        state.window.insert = 0;
        assert_eq!(state.copied_prev_entries(), w_size);

        // C computes the difference unsigned, so `insert > strstart` would wrap
        // to an enormous value and take the `w_size` branch; so does this.
        state.window.strstart = 10;
        state.window.insert = 20;
        assert_eq!(state.copied_prev_entries(), w_size);
    }

    #[test]
    fn clone_copies_the_live_regions_and_the_scalars() {
        let mut source = DeflateState::new(config(6, 9, 1), GlobalAllocator).unwrap();

        // Something recognisable in each of the four buffers, plus cursors and
        // scalars that a bytewise struct copy would have carried over.
        assert!(source.window.write_at(0, b"the quick brown fox"));
        source.window.strstart = 19;
        source.window.lookahead = 3;
        source.window.block_start = 4;
        source.window.match_start = 2;
        source.window.insert = 5;
        source.window.raise_high_water_to_strstart();
        source.hash.set_head_at(7, Pos::new(11));
        source.hash.set_prev_at(11, Pos::new(3));
        source.hash.set_slid(true);
        assert_eq!(source.pending.append(b"pending"), 7);
        assert!(source.pending.consume_flushed(2));
        assert_eq!(source.pending.push_symbol(0x0102, 0x03), Some(false));
        source.status = Status::Busy;
        source.last_flush = -1;
        source.ins_h = 12_345;
        source.match_length = 6;
        source.prev_length = 4;
        source.match_available = true;
        source.opt_len = 1_234;
        source.static_len = 5_678;
        source.matches = 9;
        source.bi_buf = 0x0f0f;
        source.bi_valid = 5;
        source.bi_used = 8;
        source.heap_len = 3;
        source.heap_max = HEAP_SIZE.try_into().unwrap();
        source.dyn_ltree[256].set_freq(1);
        source.depth[2] = 4;
        source.bl_count[3] = 6;
        source.l_desc.max_code = 285;

        let copy = source.try_clone_in(GlobalAllocator).unwrap();

        // `zmemcpy(ds->window, ss->window, ss->high_water)` (L1353): everything
        // below the high water mark, and nothing is claimed about the rest.
        assert_eq!(copy.high_water(), 19);
        assert_eq!(copy.window.region(0, 19).unwrap(), b"the quick brown fox");
        assert_eq!(copy.window.strstart, 19);
        assert_eq!(copy.window.lookahead, 3);
        assert_eq!(copy.window.block_start, 4);
        assert_eq!(copy.window.match_start, 2);
        assert_eq!(copy.window.insert, 5);

        // The hash tables (L1354-L1357). `slid` is set, so all of `prev` comes
        // across.
        assert!(copy.slid());
        assert_eq!(copy.copied_prev_entries(), copy.w_size());
        assert_eq!(copy.hash.head_at(7), Pos::new(11));
        assert_eq!(copy.hash.prev_at(11), Pos::new(3));
        assert_eq!(copy.hash.head_entries(), source.hash.head_entries());
        assert_eq!(copy.hash.prev_entries(), source.hash.prev_entries());

        // The pending output keeps both its offset and its count
        // (L1359-L1360), and the symbols keep their cursor (L1367-L1368).
        assert_eq!(copy.pending.pending_out_offset(), 2);
        assert_eq!(copy.pending_bytes(), 5);
        assert_eq!(copy.pending.flushable(), b"nding");
        assert_eq!(copy.sym_next(), SYMBOL_BYTES);
        assert_eq!(copy.pending.symbol_at(0), Some((0x0102, 0x03)));

        // Every scalar and every array.
        assert_eq!(copy.status(), Status::Busy);
        assert_eq!(copy.last_flush(), -1);
        assert_eq!(copy.ins_h, 12_345);
        assert_eq!(copy.hash_shift, source.hash_shift);
        assert_eq!(copy.match_length, 6);
        assert_eq!(copy.prev_length, 4);
        assert!(copy.match_available);
        assert_eq!(copy.opt_len, 1_234);
        assert_eq!(copy.static_len, 5_678);
        assert_eq!(copy.matches, 9);
        assert_eq!(copy.bi_buf, 0x0f0f);
        assert_eq!(copy.bi_valid, 5);
        assert_eq!(copy.bi_used, 8);
        assert_eq!(copy.heap_len, 3);
        assert_eq!(copy.heap_max, source.heap_max);
        assert_eq!(copy.dyn_ltree[256].freq(), 1);
        assert_eq!(copy.depth[2], 4);
        assert_eq!(copy.bl_count[3], 6);
        assert_eq!(copy.level(), source.level());
        assert_eq!(copy.strategy(), source.strategy());
        assert_eq!(copy.wrap(), source.wrap());
        // The three descriptors are still paired with this state's own arrays,
        // which is the fix-up `deflateCopy` has to perform by hand
        // (L1371-L1373).
        assert_eq!(copy.l_desc.max_code, 285);
        assert_eq!(copy.l_desc.stat_desc, StaticTreeKind::Literal);
        assert_eq!(copy.d_desc.stat_desc, StaticTreeKind::Distance);
        assert_eq!(copy.bl_desc.stat_desc, StaticTreeKind::BitLength);

        // The source is untouched, which is `deflateCopy`'s contract.
        assert_eq!(source.pending.flushable(), b"nding");
        assert_eq!(source.window.region(0, 19).unwrap(), b"the quick brown fox");
    }

    #[test]
    fn a_failed_clone_releases_what_it_already_took() {
        let source = DeflateState::new(DeflateConfig::new(6), GlobalAllocator).unwrap();
        for limit in 0..4 {
            let budget = Budget::new(limit);
            assert_eq!(
                source.try_clone_in(&budget).unwrap_err(),
                ReturnCode::MEM_ERROR
            );
            assert_eq!(*budget.granted.borrow(), limit);
            assert_eq!(*budget.released.borrow(), limit);
        }
    }

    #[test]
    fn gz_header_view_indexes_include_the_terminator() {
        // The facade builds `name` and `comment` with their terminating zero,
        // because the C loop emits bytes until it has emitted one
        // (`deflate.c` L1158-L1160).
        let header = GzHeaderView {
            text: true,
            time: 0x1234_5678,
            os: 3,
            extra: Some(&[1, 2, 3]),
            name: Some(b"a.txt\0"),
            comment: Some(b"c\0"),
            hcrc: true,
        };

        assert_eq!(header.extra_len(), 3);
        assert_eq!(header.extra_at(0), Some(1));
        assert_eq!(header.extra_at(2), Some(3));
        assert_eq!(header.extra_at(3), None);

        assert_eq!(header.name_at(0), Some(b'a'));
        assert_eq!(header.name_at(5), Some(0));
        assert_eq!(header.name_at(6), None);
        assert_eq!(header.comment_at(1), Some(0));

        // `Z_NULL` for all three optional fields.
        let bare = GzHeaderView {
            text: false,
            time: 0,
            os: 255,
            extra: None,
            name: None,
            comment: None,
            hcrc: false,
        };
        assert_eq!(bare.extra_len(), 0);
        assert_eq!(bare.extra_at(0), None);
        assert_eq!(bare.name_at(0), None);
        assert_eq!(bare.comment_at(0), None);

        // Installed and cleared exactly as `deflateSetHeader` does
        // (`deflate.c` L717).
        let mut state = DeflateState::new(config(6, 31, 8), GlobalAllocator).unwrap();
        state.set_gzhead(Some(header));
        assert_eq!(state.gzhead(), Some(header));
        assert_eq!(state.gzhead().unwrap().name_at(5), Some(0));
        state.set_gzhead(None);
        assert_eq!(state.gzhead(), None);
    }
}
