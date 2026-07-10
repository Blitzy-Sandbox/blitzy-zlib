//! Deflate internal state — the foundational module of the `deflate` engine.
//!
//! This is a faithful, **100% safe Rust** port of the C baseline `deflate.h`
//! (the `deflate_state` structure and its companion constants) together with
//! the shared match-finding, window-management, and bit-output helpers that
//! live in `deflate.c` and `trees.c`. Every other file in `src/deflate/`
//! (`trees.rs`, `strategy.rs`, `fast.rs`, `slow.rs`, `stored.rs`, `huff.rs`,
//! `rle.rs`, and the `mod.rs` driver) builds on the types and methods defined
//! here.
//!
//! # Safety
//!
//! There is **zero `unsafe`** in this module (and, by design, in the whole
//! `deflate/` tree). This satisfies the project constraint "zero unsafe blocks
//! in core compression logic". All buffers are owned [`Vec`]/arrays and every
//! access is a checked slice index. The C code relies on `zcalloc`/`zcfree`
//! and raw pointer arithmetic; here that is replaced by Rust ownership, owned
//! `Vec` allocations, and `Drop`-based cleanup (the latter is automatic).
//!
//! # Byte-exact fidelity
//!
//! Every constant, table, and algorithm reproduces the reference C behavior
//! bit-for-bit so that the emitted DEFLATE token stream is byte-identical to
//! reference zlib for the same input, level, and strategy. The match-finder
//! (`longest_match`, `fill_window`) and the bit packer (`send_bits`) are ported
//! with particular care because they directly determine the produced bytes.
//!
//! # `no_std`
//!
//! The module is `no_std` + `alloc`: it uses `alloc::vec::Vec` (never `std`)
//! and `core` facilities only. The crate root is expected to declare
//! `#![cfg_attr(not(feature = "std"), no_std)]` and `extern crate alloc;`.
//!
//! # C cross-reference
//!
//! Field and function names are kept close to the C originals (converted to
//! `snake_case`) so that the port can be audited against `deflate.h` /
//! `deflate.c` / `trees.c` line-by-line.

#![deny(unsafe_code)]

use alloc::boxed::Box;

use crate::checksum::adler32;
// `crc32` is only used for gzip (`wrap == 2`) framing, so gate its import to
// avoid an unused-import warning in no-gzip builds.
#[cfg(feature = "gzip")]
use crate::checksum::crc32;
use crate::constants::{DataType, MAX_MEM_LEVEL, Strategy, Z_DEFAULT_COMPRESSION, Z_DEFLATED};
use crate::error::ZlibError;
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;
use crate::stream::{AllocBuffer, AllocHook};

// ===========================================================================
// Compile-time constants (ported from deflate.h / trees.h / trees.c / zutil.h)
// ===========================================================================

/// Number of length codes, not counting the special `END_BLOCK` code
/// (`deflate.h`: `LENGTH_CODES`).
pub const LENGTH_CODES: usize = 29;

/// Number of literal bytes `0..=255` (`deflate.h`: `LITERALS`).
pub const LITERALS: usize = 256;

/// Number of literal or length codes, including the `END_BLOCK` code
/// (`deflate.h`: `L_CODES = LITERALS + 1 + LENGTH_CODES` = 286).
pub const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes (`deflate.h`: `D_CODES`).
pub const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths (`deflate.h`: `BL_CODES`).
pub const BL_CODES: usize = 19;

/// Maximum heap size used when building the Huffman trees
/// (`deflate.h`: `HEAP_SIZE = 2 * L_CODES + 1` = 573).
pub const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// All codes must not exceed this many bits (`deflate.h`: `MAX_BITS`).
pub const MAX_BITS: usize = 15;

/// Maximum number of bits used to encode the bit lengths themselves
/// (`trees.c`: `MAX_BL_BITS`).
pub const MAX_BL_BITS: usize = 7;

/// Size of the bit buffer `bi_buf`, in bits (`deflate.h`: `Buf_size`).
///
/// Kept as an `i32` because it participates in signed arithmetic in
/// [`DeflateState::send_bits`] (e.g. `Buf_size - length`). The C name is
/// `Buf_size`; the Rust constant is upper-cased to satisfy naming lints.
pub const BUF_SIZE: i32 = 16;

/// Minimum match length recognized by the LZ77 stage (`zutil.h`: `MIN_MATCH`).
pub const MIN_MATCH: usize = 3;

/// Maximum match length recognized by the LZ77 stage (`zutil.h`: `MAX_MATCH`).
pub const MAX_MATCH: usize = 258;

/// Minimum amount of lookahead required, except at the end of the input
/// (`deflate.h`: `MIN_LOOKAHEAD = MAX_MATCH + MIN_MATCH + 1` = 262).
pub const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes after the end of the current data in the window that are
/// initialized (zeroed) so that the longest-match routines may scan up to
/// `strstart + MAX_MATCH` without reading uninitialized memory
/// (`deflate.h`: `WIN_INIT = MAX_MATCH` = 258).
pub const WIN_INIT: usize = MAX_MATCH;

/// The special end-of-block code (`trees.c`: `END_BLOCK`).
pub const END_BLOCK: usize = 256;

/// Tail-of-hash-chain sentinel (`deflate.c`: `NIL`). A `Pos` (window index)
/// equal to `NIL` denotes "no further entry".
pub const NIL: u16 = 0;

/// Matches of length [`MIN_MATCH`] are discarded if their distance exceeds this
/// value (`deflate.c`: `TOO_FAR`).
pub const TOO_FAR: usize = 4096;

// ===========================================================================
// DeflateStatus — the deflate stream state machine (deflate.h L58-L67)
// ===========================================================================

/// The deflate stream status, replacing the integer-tagged `status` field of
/// the C `deflate_state`.
///
/// The discriminants are the **exact** C sentinel values (`deflate.h`
/// L58-L67); they must never change because some are observable through the
/// FFI state check and through save/restore semantics. Using a Rust `enum`
/// means the C `deflateStateCheck` status-membership test (does `status`
/// belong to the valid set?) becomes automatic: any `DeflateStatus` value is,
/// by construction, one of the valid variants, so that portion of the check is
/// trivially satisfied.
///
/// `#[repr(u16)]` is required because `Finish = 666` does not fit in a `u8`.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeflateStatus {
    /// `INIT_STATE = 42`: zlib header pending → transitions to `Busy`.
    Init = 42,
    /// `GZIP_STATE = 57`: gzip header pending → `Busy` / `Extra`.
    Gzip = 57,
    /// `EXTRA_STATE = 69`: writing the gzip "extra" field → `Name`.
    Extra = 69,
    /// `NAME_STATE = 73`: writing the gzip file name → `Comment`.
    Name = 73,
    /// `COMMENT_STATE = 91`: writing the gzip comment → `Hcrc`.
    Comment = 91,
    /// `HCRC_STATE = 103`: writing the gzip header CRC → `Busy`.
    Hcrc = 103,
    /// `BUSY_STATE = 113`: actively deflating → `Finish`.
    Busy = 113,
    /// `FINISH_STATE = 666`: the stream is complete.
    Finish = 666,
}

// ===========================================================================
// CtData — the C `ct_data` union rendered as a safe struct (deflate.h)
// ===========================================================================

/// A single entry of a Huffman tree — the safe-Rust equivalent of the C
/// `ct_data` union.
///
/// In C, `ct_data` is a pair of unions: `fc` overlays `freq` (frequency count,
/// used while building the tree) with `code` (the emitted bit string, used
/// after `gen_codes`), and `dl` overlays `dad` (parent node during tree
/// construction) with `len` (code length afterwards). Because both members of
/// each union are `ush` (`u16`) and the two lifetimes never overlap in the
/// algorithm, the union is faithfully represented by two plain `u16` fields
/// with accessor methods that name the intended interpretation.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CtData {
    /// The `fc` union member: frequency (`freq`) or code (`code`).
    pub fc: u16,
    /// The `dl` union member: parent (`dad`) or code length (`len`).
    pub dl: u16,
}

impl CtData {
    /// Reads the frequency count (`fc.freq`), used during tree building.
    #[inline]
    #[must_use]
    pub fn freq(&self) -> u16 {
        self.fc
    }

    /// Writes the frequency count (`fc.freq`).
    #[inline]
    pub fn set_freq(&mut self, v: u16) {
        self.fc = v;
    }

    /// Reads the emitted bit string (`fc.code`), valid after `gen_codes`.
    #[inline]
    #[must_use]
    pub fn code(&self) -> u16 {
        self.fc
    }

    /// Writes the emitted bit string (`fc.code`).
    #[inline]
    pub fn set_code(&mut self, v: u16) {
        self.fc = v;
    }

    /// Reads the parent node index (`dl.dad`), used during tree building.
    #[inline]
    #[must_use]
    pub fn dad(&self) -> u16 {
        self.dl
    }

    /// Writes the parent node index (`dl.dad`).
    #[inline]
    pub fn set_dad(&mut self, v: u16) {
        self.dl = v;
    }

    /// Reads the code length in bits (`dl.len`), valid after tree building.
    ///
    /// This mirrors the C `Len(tree, n)` accessor (`ct_data.dl.len`); it is a
    /// domain field name, not a collection length, so the companion
    /// `is_empty` that `clippy::len_without_is_empty` expects would be
    /// meaningless here.
    #[inline]
    #[must_use]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        self.dl
    }

    /// Writes the code length in bits (`dl.len`).
    #[inline]
    pub fn set_len(&mut self, v: u16) {
        self.dl = v;
    }
}

// ===========================================================================
// TreeKind — replaces the C static_tree_desc / tree_desc pointer plumbing
// ===========================================================================

/// Identifies which of the three Huffman trees a routine is operating on,
/// replacing the C `tree_desc` / `static_tree_desc` pointer pairs.
///
/// In C, each of `l_desc`, `d_desc`, and `bl_desc` bundles a pointer to the
/// dynamic tree (`dyn_tree`) with a pointer to the corresponding static
/// descriptor (`stat_desc`). In this port the dynamic trees live directly in
/// [`DeflateState`] (`dyn_ltree`, `dyn_dtree`, `bl_tree`) and their per-tree
/// `max_code` is stored in [`DeflateState::l_max_code`],
/// [`DeflateState::d_max_code`], and [`DeflateState::bl_max_code`]. A
/// `TreeKind` value then selects the matching static descriptor data
/// (extra-bits table, extra base, element count, maximum length) which
/// `trees.rs` provides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeKind {
    /// The combined literal/length tree (`l_desc` in C).
    Literal,
    /// The distance tree (`d_desc` in C).
    Distance,
    /// The bit-length tree (`bl_desc` in C).
    BitLength,
}

// ===========================================================================
// Match-finder configuration table (deflate.c configuration_table)
// ===========================================================================

/// The per-level match-finder tuning parameters, mirroring the numeric portion
/// of the C `configuration_table` (`deflate.c`).
///
/// The C table additionally stores a `compress_func` function pointer per
/// level; that dispatch concern is handled separately by `strategy.rs`
/// (as a Rust `enum`), so only the four tuning fields are kept here. These
/// values must match zlib exactly because they change the lazy-match decisions
/// and therefore the emitted token stream.
#[derive(Clone, Copy)]
struct Config {
    /// Reduce lazy search above this match length (`good_length`).
    good_length: u16,
    /// Do not perform lazy search above this match length (`max_lazy`).
    max_lazy: u16,
    /// Quit search above this match length (`nice_length`).
    nice_length: u16,
    /// Never search a hash chain beyond this many links (`max_chain`).
    max_chain: u16,
}

/// The ten-level match-finder configuration table, ported verbatim from the
/// non-`FASTEST` `configuration_table` in `deflate.c`. Indexed by the
/// (already resolved) compression level `0..=9`.
const CONFIGURATION_TABLE: [Config; 10] = [
    // good  lazy  nice  chain
    Config {
        good_length: 0,
        max_lazy: 0,
        nice_length: 0,
        max_chain: 0,
    }, // 0 store only
    Config {
        good_length: 4,
        max_lazy: 4,
        nice_length: 8,
        max_chain: 4,
    }, // 1 max speed
    Config {
        good_length: 4,
        max_lazy: 5,
        nice_length: 16,
        max_chain: 8,
    }, // 2
    Config {
        good_length: 4,
        max_lazy: 6,
        nice_length: 32,
        max_chain: 32,
    }, // 3
    Config {
        good_length: 4,
        max_lazy: 4,
        nice_length: 16,
        max_chain: 16,
    }, // 4 lazy matches
    Config {
        good_length: 8,
        max_lazy: 16,
        nice_length: 32,
        max_chain: 32,
    }, // 5
    Config {
        good_length: 8,
        max_lazy: 16,
        nice_length: 128,
        max_chain: 128,
    }, // 6
    Config {
        good_length: 8,
        max_lazy: 32,
        nice_length: 128,
        max_chain: 256,
    }, // 7
    Config {
        good_length: 32,
        max_lazy: 128,
        nice_length: 258,
        max_chain: 1024,
    }, // 8
    Config {
        good_length: 32,
        max_lazy: 258,
        nice_length: 258,
        max_chain: 4096,
    }, // 9 max compression
];

// ===========================================================================
// IoContext — the transient z_stream I/O view threaded through the engine
// ===========================================================================

/// A borrowed, mutable view of the `z_stream` input/output cursors, checksum,
/// and byte counters that the deflate engine reads and advances during a
/// single call.
///
/// The C engine reaches back into `strm` (the `z_stream`) through
/// `s->strm->next_in`, `avail_in`, `next_out`, `avail_out`, `total_in`,
/// `total_out`, and `adler`. Storing those inside [`DeflateState`] would create
/// a dependency cycle with the public `ZStream` type in `src/stream.rs`, so
/// they are gathered here instead. The `mod.rs` driver constructs an
/// `IoContext` from the `ZStream`'s public buffers, threads it through the
/// block producers, and writes the advanced cursors / totals / checksum back
/// into the `ZStream` when the call returns.
///
/// # Field semantics (mirroring `z_stream`)
///
/// * [`input`](Self::input) / [`next_in`](Self::next_in) /
///   [`avail_in`](Self::avail_in): the input buffer, the read cursor into it,
///   and the number of bytes still available (`input.len() == next_in +
///   avail_in` is the intended invariant).
/// * [`output`](Self::output) / [`next_out`](Self::next_out) /
///   [`avail_out`](Self::avail_out): the output buffer, the write cursor, and
///   the remaining free space.
/// * [`total_in`](Self::total_in) / [`total_out`](Self::total_out): running
///   byte counters. zlib types these as `uLong`; `u64` is used internally and
///   the FFI boundary narrows them as required. The gzip trailer uses
///   `total_in & 0xffff_ffff`.
/// * [`adler`](Self::adler): the running checksum — Adler-32 for a zlib
///   wrapper, CRC-32 for a gzip wrapper.
pub struct IoContext<'a> {
    /// The input byte buffer (`z_stream::next_in` points into this).
    pub input: &'a [u8],
    /// Read cursor: index of the next unconsumed input byte.
    pub next_in: usize,
    /// Number of input bytes still available from [`next_in`](Self::next_in).
    pub avail_in: usize,
    /// The output byte buffer (`z_stream::next_out` points into this).
    pub output: &'a mut [u8],
    /// Write cursor: index of the next free output byte.
    pub next_out: usize,
    /// Number of free output bytes still available from
    /// [`next_out`](Self::next_out).
    pub avail_out: usize,
    /// Running total of input bytes consumed (`z_stream::total_in`).
    pub total_in: u64,
    /// Running total of output bytes produced (`z_stream::total_out`).
    pub total_out: u64,
    /// Running checksum accumulator (`z_stream::adler`): Adler-32 for zlib
    /// framing, CRC-32 for gzip framing.
    pub adler: u32,
}

impl<'a> IoContext<'a> {
    /// Creates an `IoContext` over the given input and output buffers with the
    /// `avail_*` counters initialized to the full buffer lengths, the cursors
    /// at the start, and the totals/checksum seeded to zero.
    ///
    /// The `adler` seed is deliberately `0`: the caller (the `mod.rs` driver)
    /// overwrites it with the correct initial checksum for the chosen wrapper
    /// via [`DeflateState::initial_adler`] before the first byte is processed.
    #[must_use]
    pub fn new(input: &'a [u8], output: &'a mut [u8]) -> Self {
        let avail_in = input.len();
        let avail_out = output.len();
        IoContext {
            input,
            next_in: 0,
            avail_in,
            output,
            next_out: 0,
            avail_out,
            total_in: 0,
            total_out: 0,
            adler: 0,
        }
    }

    /// Returns the number of input bytes still available.
    ///
    /// Mirrors reading `strm->avail_in` in the C engine. This simply returns
    /// the [`avail_in`](Self::avail_in) field and is provided for call-site
    /// readability alongside [`avail_out_remaining`](Self::avail_out_remaining).
    #[inline]
    #[must_use]
    pub fn avail_in_remaining(&self) -> usize {
        self.avail_in
    }

    /// Returns the number of free output bytes still available.
    ///
    /// Mirrors reading `strm->avail_out` in the C engine.
    #[inline]
    #[must_use]
    pub fn avail_out_remaining(&self) -> usize {
        self.avail_out
    }
}

// ===========================================================================
// DeflateState — the port of C `deflate_state` (deflate.h L104-L288)
// ===========================================================================

/// The complete internal deflate state, a safe-Rust port of the C
/// `deflate_state` structure.
///
/// This owns every working buffer (the sliding [`window`](Self::window), the
/// hash-chain tables [`prev`](Self::prev)/[`head`](Self::head), the
/// [`pending_buf`](Self::pending_buf) output staging area, and the
/// [`sym_buf`](Self::sym_buf) symbol buffer) as `Vec` allocations, replacing
/// the C `zcalloc`/`zcfree` pointers. It is created with [`DeflateState::new`]
/// and typically held as `Option<Box<DeflateState>>` by the owning `ZStream`,
/// so its memory is released automatically via `Drop`.
///
/// `#[derive(Clone)]` supports the `deflateCopy` operation: cloning deep-copies
/// every `Vec`, and — because this port uses indices and owned arrays rather
/// than raw pointers — there are **no pointer fix-ups** to perform afterwards,
/// a substantial simplification over the C implementation.
///
/// # Memory footprint
///
/// At the defaults (`mem_level = 8`, `w_bits = 15`) the allocations are:
/// `window` = `2 * 32 KiB` = 64 KiB, `prev` = `32 Ki * 2 B` = 64 KiB,
/// `head` = `32 Ki * 2 B` = 64 KiB, `pending_buf` = `4 * lit_bufsize` =
/// `4 * 16 KiB` = 64 KiB, and `sym_buf` = `3 * lit_bufsize` = 48 KiB, for a
/// total of about 304 KiB. Reference zlib overlays `sym_buf` inside
/// `pending_buf` (saving 48 KiB, for ~256 KiB); this port keeps a **separate**
/// `sym_buf` (see the field docs) as the price of a fully safe, pointer-free
/// layout. The `pending_buf` sizing (`4 * lit_bufsize`) is preserved exactly so
/// the block-emission overflow guarantees still hold.
#[derive(Clone)]
pub struct DeflateState {
    /// Current stream status (C `status`). See [`DeflateStatus`].
    pub status: DeflateStatus,

    /// Output staging buffer (C `pending_buf`). Bytes produced by the bit
    /// packer accumulate here before being copied to the caller's output by
    /// [`flush_pending`](Self::flush_pending). Sized `lit_bufsize * 4`.
    pub pending_buf: AllocBuffer<u8>,

    /// Size of [`pending_buf`](Self::pending_buf) in bytes (C
    /// `pending_buf_size`), equal to `lit_bufsize * 4`.
    pub pending_buf_size: usize,

    /// Index into [`pending_buf`](Self::pending_buf) of the next byte to output
    /// (C `pending_out`, which is a pointer there; stored here as an offset).
    pub pending_out: usize,

    /// Number of bytes currently queued in [`pending_buf`](Self::pending_buf)
    /// awaiting output (C `pending`).
    pub pending: usize,

    /// Wrapper selector (C `wrap`): `0` = raw DEFLATE, `1` = zlib, `2` = gzip.
    /// May be temporarily negated by `deflate(..., Z_FINISH)` after the trailer
    /// is written; [`reset_keep`](Self::reset_keep) restores it to positive.
    pub wrap: i32,

    /// The gzip header to write, if any (C `gzhead`). Only present when the
    /// `gzip` feature is enabled.
    #[cfg(feature = "gzip")]
    pub gzhead: Option<GzHeader>,

    /// Current offset within the gzip extra/name/comment field being written
    /// (C `gzindex`). Only present when the `gzip` feature is enabled.
    #[cfg(feature = "gzip")]
    pub gzindex: usize,

    /// The compression method (C `method`). Always [`Z_DEFLATED`] (`8`).
    pub method: u8,

    /// The `flush` argument supplied to the previous `deflate` call (C
    /// `last_flush`), initialized to `-2`.
    pub last_flush: i32,

    /// LZ77 window size in bytes, `1 << w_bits` (C `w_size`).
    pub w_size: usize,
    /// `log2(w_size)`, in `8..=15` (C `w_bits`).
    pub w_bits: u32,
    /// `w_size - 1`, used to wrap window indices (C `w_mask`).
    pub w_mask: usize,

    /// The sliding window (C `window`), of length `2 * w_size`. Input bytes are
    /// read into the upper half and shifted down to retain a dictionary.
    pub window: AllocBuffer<u8>,

    /// Actual usable window size, `2 * w_size` (C `window_size`).
    pub window_size: usize,

    /// Hash-chain link table (C `prev`), length `w_size`. `prev[i]` links a
    /// window position to the previous position with the same hash. Entries are
    /// `Pos` (`u16`) window indices modulo `w_size`.
    pub prev: AllocBuffer<u16>,

    /// Hash-chain heads (C `head`), length `hash_size`. `head[h]` is the most
    /// recent window position hashing to `h`, or [`NIL`].
    pub head: AllocBuffer<u16>,

    /// Running hash index of the string about to be inserted (C `ins_h`).
    pub ins_h: usize,
    /// Number of slots in the hash table (C `hash_size`), `1 << hash_bits`.
    pub hash_size: usize,
    /// `log2(hash_size)` (C `hash_bits`).
    pub hash_bits: u32,
    /// `hash_size - 1` (C `hash_mask`).
    pub hash_mask: usize,

    /// Number of bits `ins_h` is shifted by per input byte (C `hash_shift`),
    /// equal to `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`.
    pub hash_shift: u32,

    /// Window position at the start of the current output block (C
    /// `block_start`). **Signed** because it goes negative when the window
    /// slides backwards; used by the block flush to decide whether a stored
    /// block can be emitted from the window.
    pub block_start: isize,

    /// Length of the best match found (C `match_length`).
    pub match_length: usize,
    /// Previous match position (C `prev_match`, an `IPos`).
    pub prev_match: u16,
    /// Set when a deferred (lazy) match from the previous step exists (C
    /// `match_available`, an `int` used as a boolean).
    pub match_available: bool,
    /// Start of the string currently being inserted / matched (C `strstart`).
    pub strstart: usize,
    /// Start of the matching string in the window (C `match_start`), set as a
    /// side effect of [`longest_match`](Self::longest_match).
    pub match_start: usize,
    /// Number of valid bytes ahead of `strstart` in the window (C `lookahead`).
    pub lookahead: usize,

    /// Length of the best match at the previous step (C `prev_length`). Matches
    /// not longer than this are discarded during lazy evaluation.
    pub prev_length: usize,

    /// Hash chains are never searched beyond this many links (C
    /// `max_chain_length`).
    pub max_chain_length: usize,

    /// Only attempt a better (lazy) match when the current match is strictly
    /// shorter than this (C `max_lazy_match`). For levels `<= 3` the same field
    /// is read as `max_insert_length` via
    /// [`max_insert_length`](Self::max_insert_length).
    pub max_lazy_match: usize,

    /// Compression level `0..=9` (C `level`), with [`Z_DEFAULT_COMPRESSION`]
    /// already resolved to `6` by [`new`](Self::new).
    pub level: i32,
    /// Compression strategy (C `strategy`). Stored as the crate
    /// [`Strategy`] enum so comparisons such as `strategy == HuffmanOnly` are
    /// exhaustive and type-checked.
    pub strategy: Strategy,

    /// Switch to a faster search once the previous match is longer than this
    /// (C `good_match`).
    pub good_match: usize,

    /// Stop searching once a match reaches at least this length (C
    /// `nice_match`). Signed to match the C `int`.
    pub nice_match: i32,

    /// The dynamic literal/length tree (C `dyn_ltree`), `HEAP_SIZE` entries.
    pub dyn_ltree: [CtData; HEAP_SIZE],
    /// The dynamic distance tree (C `dyn_dtree`), `2 * D_CODES + 1` entries.
    pub dyn_dtree: [CtData; 2 * D_CODES + 1],
    /// The bit-length tree (C `bl_tree`), `2 * BL_CODES + 1` entries.
    pub bl_tree: [CtData; 2 * BL_CODES + 1],

    /// Largest code with non-zero frequency in the literal/length tree
    /// (replaces `l_desc.max_code`).
    pub l_max_code: usize,
    /// Largest code with non-zero frequency in the distance tree (replaces
    /// `d_desc.max_code`).
    pub d_max_code: usize,
    /// Largest code with non-zero frequency in the bit-length tree (replaces
    /// `bl_desc.max_code`).
    pub bl_max_code: usize,

    /// Count of codes at each bit length for an optimal tree (C `bl_count`),
    /// `MAX_BITS + 1` entries.
    pub bl_count: [u16; MAX_BITS + 1],

    /// Heap used to build the Huffman trees (C `heap`), `2 * L_CODES + 1`
    /// entries. `heap[0]` is unused; the sons of `heap[n]` are `heap[2n]` and
    /// `heap[2n + 1]`.
    pub heap: [i32; 2 * L_CODES + 1],
    /// Number of elements currently in the heap (C `heap_len`).
    pub heap_len: usize,
    /// Index of the element of largest frequency in the heap (C `heap_max`).
    pub heap_max: usize,

    /// Depth of each subtree, used as a tie-breaker for equal-frequency trees
    /// (C `depth`), `2 * L_CODES + 1` entries.
    pub depth: [u8; 2 * L_CODES + 1],

    /// Symbol buffer holding three bytes per token (two for the distance, one
    /// for the literal/length) (C `sym_buf`).
    ///
    /// **Design note:** reference zlib overlays this on `pending_buf` at offset
    /// `lit_bufsize` (`sym_buf = pending_buf + lit_bufsize`). This port instead
    /// uses a dedicated owned `Vec<u8>` of length `lit_bufsize * 3`, which is
    /// behaviorally identical to the non-`LIT_MEM` C layout and avoids all
    /// pointer aliasing. The overflow interaction with `pending_buf` is
    /// preserved because the symbol buffer is only consumed (by `compress_block`
    /// in `trees.rs`) at a flush, when `pending` has been drained.
    pub sym_buf: AllocBuffer<u8>,

    /// Size of the literal/length symbol buffer in symbols-worth of bytes
    /// (C `lit_bufsize`), equal to `1 << (mem_level + 6)`.
    pub lit_bufsize: usize,

    /// Running write index into [`sym_buf`](Self::sym_buf) (C `sym_next`).
    pub sym_next: usize,
    /// Index at which [`sym_buf`](Self::sym_buf) is considered full and a flush
    /// is forced (C `sym_end`), equal to `(lit_bufsize - 1) * 3`.
    pub sym_end: usize,

    /// Bit length of the current block encoded with the optimal (dynamic) trees
    /// (C `opt_len`, a `ulg`).
    pub opt_len: usize,
    /// Bit length of the current block encoded with the static trees
    /// (C `static_len`, a `ulg`).
    pub static_len: usize,
    /// Number of string matches in the current block; also reused as the count
    /// of pending hash-table slides for `deflateParams` when `level == 0`
    /// (C `matches`).
    pub matches: usize,
    /// Bytes at the end of the window still to be inserted into the hash on the
    /// next `deflate` call (C `insert`).
    pub insert: usize,

    /// Bit accumulator; bits are inserted starting at the least-significant end
    /// (C `bi_buf`, a `ush`).
    pub bi_buf: u16,
    /// Number of valid bits currently in [`bi_buf`](Self::bi_buf) (C
    /// `bi_valid`). Signed to match the C `int` and its signed arithmetic.
    pub bi_valid: i32,
    /// Number of bits used in the final byte when aligning to a byte boundary
    /// (C `bi_used`), reported for the `deflateUsed`/`deflatePrime` machinery.
    pub bi_used: i32,

    /// High-water mark: the highest window offset that has been initialized
    /// (C `high_water`). Bytes above this are zeroed by
    /// [`fill_window`](Self::fill_window) so the match routines never read
    /// uninitialized memory.
    pub high_water: usize,

    /// `true` once the hash table has been slid at least once since it was
    /// cleared (C `slid`).
    pub slid: bool,

    /// Best-guess classification of the input set by `deflate` (C
    /// `data_type`), initialized to [`DataType::Unknown`].
    pub data_type: DataType,

    /// The `mem_level` the state was created with (`1..=8`). Retained for
    /// `deflateParams` / `deflateBound` bookkeeping even though most derived
    /// quantities (`hash_bits`, `lit_bufsize`) are precomputed.
    pub mem_level: i32,
}

impl DeflateState {
    /// Allocates and initializes a new deflate state, reproducing the
    /// allocation and field-setup portion of the C `deflateInit2_`
    /// (`deflate.c` L387-L533).
    ///
    /// The overloaded `windowBits` decoding (negative → raw, `> 15` → gzip) is
    /// performed by the caller (`mod.rs`, via
    /// [`crate::constants::parse_window_bits`]); this constructor therefore
    /// receives an already-resolved positive `window_bits` (`8..=15`) and the
    /// resolved `wrap` selector (`0` raw, `1` zlib, `2` gzip).
    ///
    /// Parameter validation mirrors zlib exactly and yields
    /// [`ZlibError::StreamError`] on any invalid combination:
    ///
    /// * `mem_level` must be in `1..=`[`MAX_MEM_LEVEL`];
    /// * `method` must equal [`Z_DEFLATED`];
    /// * `window_bits` must be in `8..=15` (and `8` requires `wrap == 1`);
    /// * `level` must be in `0..=9` after [`Z_DEFAULT_COMPRESSION`] is resolved
    ///   to `6`.
    ///
    /// The `strategy` is a [`Strategy`] enum and is therefore always valid, so
    /// the C `strategy` range check is unnecessary.
    ///
    /// # I/O-side reset
    ///
    /// C `deflateInit2_` finishes by calling `deflateReset`, which also zeroes
    /// `total_in`/`total_out` and seeds `adler`. Those live in the
    /// [`IoContext`] / owning `ZStream` rather than in `DeflateState`, so this
    /// constructor performs only the state-side reset. The caller must set the
    /// stream's `total_in`/`total_out` to `0` and its `adler` to
    /// [`DeflateState::initial_adler`]`(wrap)`.
    ///
    /// # Errors
    ///
    /// Returns [`ZlibError::StreamError`] if any parameter is invalid.
    ///
    /// # Allocator
    ///
    /// This convenience constructor uses the Rust global allocator for all
    /// working buffers (equivalent to a C caller with null `zalloc`/`zfree`).
    /// The FFI init path calls [`new_in`](Self::new_in) instead, passing the
    /// caller's [`AllocHook`] so the buffers are routed through the caller's
    /// `zalloc`/`zfree` (AAP §0.6.3).
    #[inline]
    pub fn new(
        level: i32,
        method: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
        wrap: i32,
    ) -> Result<Box<DeflateState>, ZlibError> {
        Self::new_in(
            AllocHook::none(),
            level,
            method,
            window_bits,
            mem_level,
            strategy,
            wrap,
        )
    }

    /// Builds a boxed [`DeflateState`], allocating every working buffer through
    /// the supplied [`AllocHook`] (the caller's `zalloc`/`zfree` when active, or
    /// the global allocator otherwise). See [`new`](Self::new) for the parameter
    /// contract, I/O-side-reset note, and errors — this is the same constructor
    /// with an explicit allocator hook (AAP §0.6.3 has-hook clause).
    pub fn new_in(
        hook: AllocHook,
        level: i32,
        method: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
        wrap: i32,
    ) -> Result<Box<DeflateState>, ZlibError> {
        // Resolve the default level exactly as deflateInit2_ does.
        let level = if level == Z_DEFAULT_COMPRESSION {
            6
        } else {
            level
        };

        // Validate parameters (deflate.c L433-L437).
        if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
            || method != Z_DEFLATED
            || !(8..=15).contains(&window_bits)
            || !(0..=9).contains(&level)
            || (window_bits == 8 && wrap != 1)
        {
            return Err(ZlibError::StreamError);
        }

        // "until 256-byte window bug fixed": an 8-bit window is bumped to 9.
        let window_bits = if window_bits == 8 { 9 } else { window_bits };

        let w_bits = window_bits as u32;
        let w_size = 1usize << w_bits;
        let w_mask = w_size - 1;

        let hash_bits = mem_level as u32 + 7;
        let hash_size = 1usize << hash_bits;
        let hash_mask = hash_size - 1;
        // C: (hash_bits + MIN_MATCH - 1) / MIN_MATCH — i.e. a ceiling divide,
        // expressed with `div_ceil` for clarity (identical value).
        let hash_shift = hash_bits.div_ceil(MIN_MATCH as u32);

        let lit_bufsize = 1usize << (mem_level as u32 + 6);
        // We size pending_buf as 4 * lit_bufsize (LIT_BUFS == 4), exactly as C
        // does, preserving the block-emission overflow guarantee. sym_buf is a
        // separate lit_bufsize * 3 buffer (see the field documentation).
        let pending_buf_size = lit_bufsize * 4;
        // We avoid equality with lit_bufsize*3 to match C (wraparound / stored
        // block considerations): sym_end = (lit_bufsize - 1) * 3.
        let sym_end = (lit_bufsize - 1) * 3;

        // Allocate every working buffer up front, routed through the caller's
        // allocator hook. When an active hook's `zalloc` reports out-of-memory,
        // `try_zeroed` yields `None`, which we surface as `Z_MEM_ERROR` (M7);
        // any buffers already allocated are released as the early return drops
        // them. The null-hook (global) path never fails.
        let pending_buf =
            AllocBuffer::try_zeroed(pending_buf_size, hook).ok_or(ZlibError::MemError)?;
        let window = AllocBuffer::try_zeroed(2 * w_size, hook).ok_or(ZlibError::MemError)?;
        let prev = AllocBuffer::try_zeroed(w_size, hook).ok_or(ZlibError::MemError)?;
        let head = AllocBuffer::try_zeroed(hash_size, hook).ok_or(ZlibError::MemError)?;
        let sym_buf = AllocBuffer::try_zeroed(lit_bufsize * 3, hook).ok_or(ZlibError::MemError)?;

        let mut state = Box::new(DeflateState {
            status: DeflateStatus::Init,
            pending_buf,
            pending_buf_size,
            pending_out: 0,
            pending: 0,
            wrap,
            #[cfg(feature = "gzip")]
            gzhead: None,
            #[cfg(feature = "gzip")]
            gzindex: 0,
            method: Z_DEFLATED as u8,
            last_flush: -2,
            w_size,
            w_bits,
            w_mask,
            window,
            window_size: 2 * w_size,
            prev,
            head,
            ins_h: 0,
            hash_size,
            hash_bits,
            hash_mask,
            hash_shift,
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: MIN_MATCH - 1,
            max_chain_length: 0,
            max_lazy_match: 0,
            level,
            strategy,
            good_match: 0,
            nice_match: 0,
            dyn_ltree: [CtData::default(); HEAP_SIZE],
            dyn_dtree: [CtData::default(); 2 * D_CODES + 1],
            bl_tree: [CtData::default(); 2 * BL_CODES + 1],
            l_max_code: 0,
            d_max_code: 0,
            bl_max_code: 0,
            bl_count: [0u16; MAX_BITS + 1],
            heap: [0i32; 2 * L_CODES + 1],
            heap_len: 0,
            heap_max: 0,
            depth: [0u8; 2 * L_CODES + 1],
            sym_buf,
            lit_bufsize,
            sym_next: 0,
            sym_end,
            opt_len: 0,
            static_len: 0,
            matches: 0,
            insert: 0,
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,
            high_water: 0,
            slid: false,
            data_type: DataType::Unknown,
            mem_level,
        });

        // Reproduce the state-resetting portion of deflateReset. The I/O-side
        // reset (total_in/total_out = 0, adler = initial_adler(wrap)) is the
        // caller's responsibility (this constructor has no IoContext).
        state.reset_state();
        state.lm_init();

        Ok(state)
    }

    /// Returns the initial checksum seed for the given wrapper, matching the C
    /// `deflateResetKeep` assignment `adler = wrap == 2 ? crc32(0, Z_NULL, 0) :
    /// adler32(0, Z_NULL, 0)`.
    ///
    /// In C the `Z_NULL` sentinel makes `adler32(0, Z_NULL, 0)` return `1`.
    /// This crate's slice-based [`adler32`](crate::checksum::adler32()) has no
    /// null case, so the equivalent seed is obtained with `adler32(1, &[])`
    /// (which returns `1`); the gzip seed is `crc32(0, &[])` (which returns
    /// `0`). Both reproduce the C initial values exactly.
    #[must_use]
    pub fn initial_adler(wrap: i32) -> u32 {
        #[cfg(feature = "gzip")]
        if wrap == 2 {
            return crc32(0, &[]);
        }
        #[cfg(not(feature = "gzip"))]
        let _ = wrap;
        adler32(1, &[])
    }

    /// Returns the initial [`DeflateStatus`] for the given (already
    /// sign-normalized) wrapper: [`DeflateStatus::Gzip`] for a gzip wrapper
    /// (`wrap == 2`, `gzip` feature only), otherwise [`DeflateStatus::Init`].
    fn initial_status(wrap: i32) -> DeflateStatus {
        #[cfg(feature = "gzip")]
        if wrap == 2 {
            return DeflateStatus::Gzip;
        }
        #[cfg(not(feature = "gzip"))]
        let _ = wrap;
        DeflateStatus::Init
    }

    /// Residual equivalent of the C `deflateStateCheck` (`deflate.c`
    /// L538-L556).
    ///
    /// In C this validates that `strm`, `zalloc`/`zfree`, and `state` are
    /// non-null and that `status` is one of the recognized sentinels. With Rust
    /// ownership, holding a `&DeflateState` already guarantees a live, valid
    /// state, and the `status`-membership test is automatic because
    /// [`DeflateStatus`] can only hold a valid variant. The only residual
    /// invariant worth asserting is that the method is DEFLATE, which
    /// [`new`](Self::new) enforces — so this returns `true` for any well-formed
    /// state.
    #[inline]
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.method == Z_DEFLATED as u8
    }

    /// State-side portion of the C `deflateResetKeep` (`deflate.c` L644-L677):
    /// everything that lives in `DeflateState` rather than in the owning
    /// stream. The `total_in`/`total_out`/`adler` reset is performed by
    /// [`reset_keep`](Self::reset_keep) because those fields belong to the
    /// [`IoContext`].
    ///
    /// This also zeroes the bit-buffer trio (`bi_buf`/`bi_valid`/`bi_used`),
    /// which in C is done inside `_tr_init`. The higher-level tree-frequency
    /// initialization (`init_block`) is intentionally *not* performed here — it
    /// lives in `trees.rs` and is invoked by the `mod.rs` driver after a reset,
    /// because this module does not depend on `trees.rs`.
    fn reset_state(&mut self) {
        self.data_type = DataType::Unknown;
        self.pending = 0;
        self.pending_out = 0;
        // A negative wrap (used transiently while a trailer is written) is
        // normalized back to positive on reset, exactly as C does.
        if self.wrap < 0 {
            self.wrap = -self.wrap;
        }
        self.status = Self::initial_status(self.wrap);
        self.last_flush = -2;
        // Part of what C `_tr_init` resets; safe to do here since these are
        // plain state fields touched only by the bit-output primitives.
        self.bi_buf = 0;
        self.bi_valid = 0;
        self.bi_used = 0;
    }

    /// Reproduces C `deflateResetKeep` (`deflate.c` L644-L677): resets the
    /// stream I/O counters and checksum seed (via the supplied [`IoContext`])
    /// together with the state-side fields, but keeps the allocated buffers and
    /// configuration.
    ///
    /// The caller-visible `total_in`/`total_out` are zeroed and `adler` is
    /// seeded with [`initial_adler`](Self::initial_adler). The tree-frequency
    /// initialization performed by C `_tr_init` is deferred to the `trees.rs`
    /// driver (see `reset_state`).
    pub fn reset_keep(&mut self, io: &mut IoContext) {
        io.total_in = 0;
        io.total_out = 0;
        self.reset_state();
        io.adler = Self::initial_adler(self.wrap);
    }

    /// Reproduces C `deflateReset` (`deflate.c` L704): a
    /// [`reset_keep`](Self::reset_keep) followed by
    /// [`lm_init`](Self::lm_init), re-establishing the match-finder state.
    pub fn reset(&mut self, io: &mut IoContext) {
        self.reset_keep(io);
        self.lm_init();
    }

    /// Reproduces C `lm_init` (`deflate.c` L682-L701): initializes the
    /// match-finder window bookkeeping, clears the hash tables, and loads the
    /// per-level tuning parameters (`good_match`, `max_lazy_match`,
    /// `nice_match`, `max_chain_length`) from the configuration table.
    pub fn lm_init(&mut self) {
        self.window_size = 2 * self.w_size;
        self.clear_hash();

        // Load the per-level configuration (byte-exact port of C
        // configuration_table[level]). `level` is validated to 0..=9 in `new`.
        let cfg = &CONFIGURATION_TABLE[self.level as usize];
        self.max_lazy_match = cfg.max_lazy as usize;
        self.good_match = cfg.good_length as usize;
        self.nice_match = cfg.nice_length as i32;
        self.max_chain_length = cfg.max_chain as usize;

        self.strstart = 0;
        self.block_start = 0;
        self.lookahead = 0;
        self.insert = 0;
        self.match_length = MIN_MATCH - 1;
        self.prev_length = MIN_MATCH - 1;
        self.match_available = false;
        self.ins_h = 0;
    }

    /// Reproduces C `CLEAR_HASH` (`deflate.h`): resets every hash-head entry to
    /// [`NIL`]. C writes `head[hash_size - 1] = NIL` and zeroes the remainder;
    /// because `NIL == 0` the net effect is that all entries become `NIL`, so a
    /// single fill is equivalent. Also clears the [`slid`](DeflateState::slid)
    /// flag.
    fn clear_hash(&mut self) {
        self.head.iter_mut().for_each(|h| *h = NIL);
        self.slid = false;
    }

    /// Reproduces C `UPDATE_HASH` (`deflate.c` L154): rolls the byte `c` into
    /// the running hash index `ins_h`, masked to the hash-table size.
    ///
    /// `ins_h` stays strictly below `hash_size` (`<= 2^15`) so the intermediate
    /// shift cannot overflow a `usize`.
    #[inline]
    pub fn update_hash(&mut self, c: u8) {
        self.ins_h = ((self.ins_h << self.hash_shift) ^ (c as usize)) & self.hash_mask;
    }

    /// Reproduces the non-`FASTEST` C `INSERT_STRING` macro (`deflate.c`
    /// L165-L179): rolls the byte at `str_idx + MIN_MATCH - 1` into the hash,
    /// links the new string into the head of its hash chain, and returns the
    /// previous chain head (the candidate `match_head`).
    ///
    /// The caller must guarantee `str_idx + MIN_MATCH - 1` is a valid window
    /// index; [`fill_window`](Self::fill_window) upholds this by zero-filling
    /// bytes above `high_water`.
    #[inline]
    pub fn insert_string(&mut self, str_idx: usize) -> u16 {
        self.update_hash(self.window[str_idx + MIN_MATCH - 1]);
        let match_head = self.head[self.ins_h];
        self.prev[str_idx & self.w_mask] = match_head;
        self.head[self.ins_h] = str_idx as u16;
        match_head
    }

    /// Reproduces C `slide_hash` (`deflate.c` L217-L241): when the window
    /// slides by `w_size`, every hash-table and chain entry is decremented by
    /// `w_size`, with entries that would go negative reset to [`NIL`].
    ///
    /// C computes `m >= wsize ? m - wsize : NIL`; because `NIL == 0`, this is
    /// exactly [`u16::saturating_sub`], which is used here to stay panic-free
    /// without any `unsafe` or explicit branch.
    pub fn slide_hash(&mut self) {
        let wsize = self.w_size as u16;
        for m in self.head.iter_mut() {
            *m = m.saturating_sub(wsize);
        }
        for m in self.prev.iter_mut() {
            *m = m.saturating_sub(wsize);
        }
        self.slid = true;
    }

    /// Fully-decomposed core of C `read_buf` (`deflate.c` L295-L322): copies up
    /// to `dst.len()` bytes from `input[*next_in..]` into `dst`, updates the
    /// running checksum over the copied bytes according to `wrap`, and advances
    /// the input cursor/counter trio.
    ///
    /// This is an associated function taking each I/O field by mutable
    /// reference (rather than a `&mut self` method) so that it can be driven
    /// with **two different destinations** without tripping the borrow checker:
    ///
    /// * window fills pass `dst = &mut self.window[..]` (see
    ///   `read_buf` and [`fill_window`](Self::fill_window));
    /// * the stored-block fast path (in `stored.rs`) passes
    ///   `dst = &mut io.output[..]` to copy input straight to output.
    ///
    /// In both cases the `input`/cursor/`adler` arguments come from the same
    /// [`IoContext`], while `dst` borrows a disjoint buffer, so the two mutable
    /// borrows never overlap.
    ///
    /// Returns the number of bytes actually copied (`0` when input is
    /// exhausted).
    #[allow(clippy::too_many_arguments)]
    pub fn read_buf_into(
        input: &[u8],
        next_in: &mut usize,
        avail_in: &mut usize,
        total_in: &mut u64,
        adler: &mut u32,
        wrap: i32,
        dst: &mut [u8],
    ) -> usize {
        let len = core::cmp::min(*avail_in, dst.len());
        if len == 0 {
            return 0;
        }

        let start = *next_in;
        dst[..len].copy_from_slice(&input[start..start + len]);

        // Checksum the bytes just read (identical to updating over `dst`, since
        // it now holds the same bytes). zlib framing (wrap == 1) uses Adler-32;
        // gzip framing (wrap == 2) uses CRC-32.
        if wrap == 1 {
            *adler = adler32(*adler, &dst[..len]);
        }
        #[cfg(feature = "gzip")]
        if wrap == 2 {
            *adler = crc32(*adler, &dst[..len]);
        }

        *next_in += len;
        *avail_in -= len;
        *total_in += len as u64;
        len
    }

    /// Reproduces C `read_buf` (`deflate.c` L295-L322): reads up to `size`
    /// bytes of input into the window starting at `buf_start`, updating the
    /// checksum. Thin wrapper over [`read_buf_into`](Self::read_buf_into) that
    /// targets `self.window[buf_start..buf_start + size]`.
    ///
    /// Returns the number of bytes copied.
    fn read_buf(&mut self, io: &mut IoContext, buf_start: usize, size: usize) -> usize {
        let end = buf_start + size;
        Self::read_buf_into(
            io.input,
            &mut io.next_in,
            &mut io.avail_in,
            &mut io.total_in,
            &mut io.adler,
            self.wrap,
            &mut self.window[buf_start..end],
        )
    }

    /// Reproduces C `fill_window` (`deflate.c` L252-L376): refills the sliding
    /// window from the input so the match finder has at least
    /// [`MIN_LOOKAHEAD`] bytes available (when input permits), sliding the
    /// window down by `w_size` when it fills.
    ///
    /// The 16-bit-machine special-casing (`sizeof(int) <= 2`) in the C source
    /// is intentionally omitted — `usize` is 64-bit on all supported targets.
    ///
    /// After refilling, any window bytes beyond the current data that have
    /// never been written are zero-filled up to `high_water` (both C branches
    /// are reproduced). This is what makes the portable
    /// [`longest_match`](Self::longest_match) safe with pure bounds-checked
    /// indexing: scans up to `strstart + MAX_MATCH` always read initialized,
    /// in-bounds bytes, so **no `unsafe` is required**.
    pub fn fill_window(&mut self, io: &mut IoContext) {
        let wsize = self.w_size;

        loop {
            // Free space at the end of the window.
            let mut more = self.window_size - self.lookahead - self.strstart;

            // If the window is almost full and lookahead is insufficient, move
            // the upper half down to make room in the upper half.
            if self.strstart >= wsize + self.max_dist() {
                // memcpy(window, window + wsize, wsize - more)
                self.window.copy_within(wsize..wsize + (wsize - more), 0);
                // match_start may be stale here; C subtracts unconditionally.
                // Use wrapping_sub to stay panic-free without `unsafe`.
                self.match_start = self.match_start.wrapping_sub(wsize);
                // The slide condition guarantees strstart >= wsize.
                self.strstart -= wsize;
                self.block_start -= wsize as isize;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += wsize;
            }

            if io.avail_in == 0 {
                break;
            }

            // Read into window[strstart + lookahead ..], at most `more` bytes.
            let n = self.read_buf(io, self.strstart + self.lookahead, more);
            self.lookahead += n;

            // Initialize the hash value now that we have some input.
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut str_idx = self.strstart - self.insert;
                self.ins_h = self.window[str_idx] as usize;
                self.update_hash(self.window[str_idx + 1]);
                // (MIN_MATCH == 3, so no extra UPDATE_HASH calls are needed.)
                while self.insert != 0 {
                    self.update_hash(self.window[str_idx + MIN_MATCH - 1]);
                    self.prev[str_idx & self.w_mask] = self.head[self.ins_h];
                    self.head[self.ins_h] = str_idx as u16;
                    str_idx += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }

            if !(self.lookahead < MIN_LOOKAHEAD && io.avail_in != 0) {
                break;
            }
        }

        // Zero any never-written bytes in the WIN_INIT region beyond the
        // current data, updating the high-water mark (deflate.c L355-L372).
        if self.high_water < self.window_size {
            let curr = self.strstart + self.lookahead;

            if self.high_water < curr {
                // Previous high water below current data: zero WIN_INIT bytes
                // (or up to end of window, whichever is less).
                let mut init = self.window_size - curr;
                if init > WIN_INIT {
                    init = WIN_INIT;
                }
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                // High water at/above current data but below data + WIN_INIT:
                // zero out to data + WIN_INIT (or end of window).
                let mut init = curr + WIN_INIT - self.high_water;
                if init > self.window_size - self.high_water {
                    init = self.window_size - self.high_water;
                }
                let hw = self.high_water;
                self.window[hw..hw + init].fill(0);
                self.high_water += init;
            }
        }
    }

    /// Reproduces the **portable** (`#else /* UNALIGNED_OK */`) C
    /// `longest_match` (`deflate.c` L1480-L1512).
    ///
    /// Finds the longest match for the string at `strstart`, following the hash
    /// chain that begins at `cur_match`. Returns the best match length (capped
    /// at the current `lookahead`) and, as a side effect, sets
    /// [`match_start`](DeflateState::match_start) to the position of that match
    /// — mirroring the C out-parameter.
    ///
    /// This is the byte-by-byte comparison variant (both `UNALIGNED_OK` and
    /// `FASTEST` are disabled in the reference build), implemented with pure
    /// bounds-checked indexing and **no `unsafe`**. All reads are guaranteed
    /// in-bounds: `strstart <= window_size - MIN_LOOKAHEAD`, `cur_match <
    /// strstart`, `best_len <= MAX_MATCH`, and [`fill_window`](Self::fill_window)
    /// zero-fills the `WIN_INIT` bytes beyond the data, so scans up to
    /// `strstart + MAX_MATCH` stay within the `2 * w_size` window and read
    /// deterministic bytes.
    ///
    /// # Byte-exact fidelity
    ///
    /// The quick-reject comparison order and the `best_len`/`best_len - 1`
    /// probe offsets are preserved exactly, because they determine which of two
    /// equally long matches wins and therefore the produced byte stream. Byte
    /// offset `2` is intentionally **not** re-compared: when the surrounding
    /// bytes match and the hash keys are equal (with `HASH_BITS >= 8`), byte `2`
    /// is always equal, so the C code — and this port — begin extending from
    /// offset `3`.
    ///
    /// The C 8×-unrolled compare loop with an every-8th `scan < strend` bound
    /// check is replaced by a simple per-iteration loop. Because `MAX_MATCH - 2
    /// == 256` is a multiple of 8, both formulations terminate at exactly the
    /// same `scan` position and compute the identical `len`, so the result is
    /// bit-for-bit the same.
    pub fn longest_match(&mut self, cur_match: usize) -> usize {
        let mut chain_length = self.max_chain_length;
        let mut best_len = self.prev_length;
        let mut nice_match = self.nice_match as usize;
        // Stop when cur_match drops to/below `limit`; index 0 is never matched.
        let limit = if self.strstart > self.max_dist() {
            self.strstart - self.max_dist()
        } else {
            NIL as usize
        };
        let wmask = self.w_mask;
        let strend = self.strstart + MAX_MATCH;

        let mut scan_end1 = self.window[self.strstart + best_len - 1];
        let mut scan_end = self.window[self.strstart + best_len];

        // Do not waste time if we already have a good match.
        if self.prev_length >= self.good_match {
            chain_length >>= 2;
        }
        // Do not look beyond the end of the input; keeps deflate deterministic.
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }

        let mut cur_match = cur_match;

        loop {
            let match_base = cur_match;

            // Quick reject (positive form of C's `if (A||B||C||D) continue;`).
            // These are pure reads, so the De Morgan inversion preserves
            // behavior exactly. The offsets and the pair of probes at
            // `best_len` / `best_len - 1` match C.
            if self.window[match_base + best_len] == scan_end
                && self.window[match_base + best_len - 1] == scan_end1
                && self.window[match_base] == self.window[self.strstart]
                && self.window[match_base + 1] == self.window[self.strstart + 1]
            {
                // Offsets 0 and 1 matched above; offset 2 is guaranteed equal
                // by the hash and is skipped. Extend the match from offset 3.
                let mut s_idx = self.strstart + 2;
                let mut m_idx = match_base + 2;
                loop {
                    s_idx += 1;
                    m_idx += 1;
                    if self.window[s_idx] != self.window[m_idx] {
                        break;
                    }
                    if s_idx >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - s_idx);

                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = self.window[self.strstart + best_len - 1];
                    scan_end = self.window[self.strstart + best_len];
                }
            }

            // Advance along the hash chain: C's
            // `while ((cur_match = prev[cur_match & wmask]) > limit
            //         && --chain_length != 0)`.
            cur_match = self.prev[cur_match & wmask] as usize;
            if cur_match <= limit {
                break;
            }
            // Equivalent to `--chain_length != 0` for the chain_length >= 1 that
            // valid deflate levels always supply, and panic-safe otherwise.
            if chain_length <= 1 {
                break;
            }
            chain_length -= 1;
        }

        if best_len <= self.lookahead {
            best_len
        } else {
            self.lookahead
        }
    }

    // -----------------------------------------------------------------------
    // Low-level bit/byte output primitives.
    //
    // These live here (rather than in `trees.rs`) because they only touch the
    // bit-accumulator (`bi_buf`/`bi_valid`/`bi_used`) and the pending-output
    // buffer (`pending_buf`/`pending`), which are all `DeflateState` fields.
    // Keeping them here lets `flush_pending` call `tr_flush_bits`/`bi_flush`
    // without a `state.rs -> trees.rs` dependency cycle; the higher-level tree
    // routines in `trees.rs` call these primitives in turn.
    // -----------------------------------------------------------------------

    /// Appends one byte to the pending-output buffer (`deflate.h`: `put_byte`).
    #[inline]
    pub fn put_byte(&mut self, b: u8) {
        self.pending_buf[self.pending] = b;
        self.pending += 1;
    }

    /// Appends a 16-bit value to the pending buffer, **least-significant byte
    /// first** (`trees.c`: `put_short`).
    #[inline]
    pub fn put_short(&mut self, w: u16) {
        self.put_byte((w & 0xff) as u8);
        self.put_byte((w >> 8) as u8);
    }

    /// Reproduces the compiled (non-debug) C `send_bits` macro (`trees.c`):
    /// sends `length` low bits of `value` into the bit accumulator, flushing a
    /// full 16-bit word to the pending buffer when the accumulator overflows.
    ///
    /// # Rust shift safety
    ///
    /// C promotes the operands to `int` and relies on defined behavior for the
    /// shifts. In Rust a shift such as `(value as u16) << 16` (which can occur
    /// when `bi_valid == 16`) would **panic** in debug builds. To reproduce the
    /// C truncation semantics without any `unsafe`, the shift is computed in
    /// `u32` and then truncated with `as u16`; the shift amount is always in
    /// `0..=16`, so the `u32` shift is always well-defined.
    pub fn send_bits(&mut self, value: i32, length: i32) {
        let val = value as u32;
        if self.bi_valid > BUF_SIZE - length {
            self.bi_buf |= (val << self.bi_valid as u32) as u16;
            let bi_buf = self.bi_buf;
            self.put_short(bi_buf);
            self.bi_buf = (val >> (BUF_SIZE - self.bi_valid) as u32) as u16;
            self.bi_valid += length - BUF_SIZE;
        } else {
            self.bi_buf |= (val << self.bi_valid as u32) as u16;
            self.bi_valid += length;
        }
    }

    /// Sends the Huffman code for symbol `c` from `tree` (`trees.c`:
    /// `send_code`): a [`send_bits`](Self::send_bits) of the code value and its
    /// bit length.
    #[inline]
    pub fn send_code(&mut self, c: usize, tree: &[CtData]) {
        self.send_bits(tree[c].code() as i32, tree[c].len() as i32);
    }

    /// Reproduces C `bi_flush` (`trees.c`): flushes whole bytes currently held
    /// in the bit accumulator to the pending buffer, leaving fewer than 8 bits
    /// buffered.
    pub fn bi_flush(&mut self) {
        if self.bi_valid == 16 {
            let bi_buf = self.bi_buf;
            self.put_short(bi_buf);
            self.bi_buf = 0;
            self.bi_valid = 0;
        } else if self.bi_valid >= 8 {
            self.put_byte((self.bi_buf & 0xff) as u8);
            self.bi_buf >>= 8;
            self.bi_valid -= 8;
        }
    }

    /// Reproduces C `bi_windup` (`trees.c`): flushes the remaining bits,
    /// padding to a byte boundary, and resets the accumulator.
    ///
    /// Also records [`bi_used`](DeflateState::bi_used) — the number of bits
    /// occupied in the final output byte — using the C expression
    /// `((bi_valid - 1) & 7) + 1`, evaluated on the pre-windup `bi_valid`.
    pub fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let bi_buf = self.bi_buf;
            self.put_short(bi_buf);
        } else if self.bi_valid > 0 {
            self.put_byte((self.bi_buf & 0xff) as u8);
        }
        self.bi_used = ((self.bi_valid - 1) & 7) + 1;
        self.bi_buf = 0;
        self.bi_valid = 0;
    }

    /// Reproduces C `_tr_flush_bits` (`trees.c`): flushes the bit buffer to the
    /// pending output (a thin wrapper over [`bi_flush`](Self::bi_flush)). Named
    /// without the leading underscore to keep it a conventional public method.
    #[inline]
    pub fn tr_flush_bits(&mut self) {
        self.bi_flush();
    }

    /// Reproduces C `flush_pending` (`deflate.c` L950-L968): first flushes any
    /// buffered bits (`tr_flush_bits`), then copies as many pending bytes as
    /// fit into the output buffer described by `io`, advancing all cursors and
    /// counters. When the pending buffer drains completely, the read offset is
    /// reset to the start.
    pub fn flush_pending(&mut self, io: &mut IoContext) {
        self.tr_flush_bits();

        let len = core::cmp::min(self.pending, io.avail_out);
        if len == 0 {
            return;
        }

        let src = self.pending_out;
        let dst = io.next_out;
        io.output[dst..dst + len].copy_from_slice(&self.pending_buf[src..src + len]);

        io.next_out += len;
        self.pending_out += len;
        io.total_out += len as u64;
        io.avail_out -= len;
        self.pending -= len;

        if self.pending == 0 {
            self.pending_out = 0;
        }
    }

    /// Reproduces the C `MAX_DIST(s)` macro (`deflate.h`): the largest distance
    /// a match may reach back, `w_size - MIN_LOOKAHEAD`. Kept as a method so the
    /// window size is always read from the live state.
    #[inline]
    #[must_use]
    pub fn max_dist(&self) -> usize {
        self.w_size - MIN_LOOKAHEAD
    }

    /// Reproduces the C `#define max_insert_length max_lazy_match`
    /// (`deflate.h`): the two names are aliases for the same value, exposed
    /// here as an accessor for the match-finder call sites.
    #[inline]
    #[must_use]
    pub fn max_insert_length(&self) -> usize {
        self.max_lazy_match
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{Strategy, Z_DEFAULT_COMPRESSION, Z_DEFLATED};

    /// Compile-time constants must equal the C baseline exactly.
    #[test]
    fn constants_match_c() {
        assert_eq!(LENGTH_CODES, 29);
        assert_eq!(LITERALS, 256);
        assert_eq!(L_CODES, 286);
        assert_eq!(D_CODES, 30);
        assert_eq!(BL_CODES, 19);
        assert_eq!(HEAP_SIZE, 573);
        assert_eq!(MAX_BITS, 15);
        assert_eq!(MAX_BL_BITS, 7);
        assert_eq!(BUF_SIZE, 16);
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(MAX_MATCH, 258);
        assert_eq!(MIN_LOOKAHEAD, 262);
        assert_eq!(WIN_INIT, 258);
        assert_eq!(END_BLOCK, 256);
        assert_eq!(NIL, 0);
        assert_eq!(TOO_FAR, 4096);
    }

    /// `DeflateStatus` discriminants must be the exact C sentinels.
    #[test]
    fn status_discriminants() {
        assert_eq!(DeflateStatus::Init as u16, 42);
        assert_eq!(DeflateStatus::Gzip as u16, 57);
        assert_eq!(DeflateStatus::Extra as u16, 69);
        assert_eq!(DeflateStatus::Name as u16, 73);
        assert_eq!(DeflateStatus::Comment as u16, 91);
        assert_eq!(DeflateStatus::Hcrc as u16, 103);
        assert_eq!(DeflateStatus::Busy as u16, 113);
        assert_eq!(DeflateStatus::Finish as u16, 666);
    }

    /// `CtData` faithfully overlays the C `ct_data` union: `freq` aliases
    /// `code` (both `fc`), and `dad` aliases `len` (both `dl`).
    #[test]
    fn ctdata_union_aliasing() {
        let mut c = CtData::default();
        assert_eq!(c.freq(), 0);
        assert_eq!(c.dad(), 0);

        c.set_freq(1234);
        assert_eq!(c.code(), 1234); // code shares storage with freq
        c.set_code(4321);
        assert_eq!(c.freq(), 4321);

        c.set_dad(77);
        assert_eq!(c.len(), 77); // len shares storage with dad
        c.set_len(88);
        assert_eq!(c.dad(), 88);
    }

    /// `send_bits` must never panic on shift overflow, even when `bi_valid`
    /// reaches 16 and 16-bit values are pushed, and must produce the C result.
    #[test]
    fn send_bits_no_shift_panic() {
        let mut s = DeflateState::new(6, Z_DEFLATED, 15, 8, Strategy::Default, 1).unwrap();

        // Simple accumulation (else branch).
        s.send_bits(0b101, 3);
        assert_eq!(s.bi_buf, 0b101);
        assert_eq!(s.bi_valid, 3);

        // Force the worst case: a full accumulator, then push 15 more bits.
        // With bi_valid == 16 this shifts a u16 value left by 16, which would
        // panic if computed in u16; the port computes in u32 and truncates.
        s.bi_buf = 0;
        s.bi_valid = 16;
        s.pending = 0;
        s.send_bits(0xFFFF, 15);
        assert_eq!(s.pending, 2); // one 16-bit word flushed via put_short
        assert_eq!(s.bi_valid, 15); // 16 + 15 - 16
        assert_eq!(s.bi_buf, 0xFFFF); // value >> (16 - 16)
    }

    /// Checksum seeds must match the C initial values despite the crate's
    /// slice-based (no null-sentinel) checksum API.
    #[test]
    fn initial_adler_seed() {
        // zlib framing: adler32(1, &[]) == 1 (C adler32(0, Z_NULL, 0) == 1).
        assert_eq!(DeflateState::initial_adler(1), 1);
        assert_eq!(DeflateState::initial_adler(0), 1);
        #[cfg(feature = "gzip")]
        {
            // gzip framing: crc32(0, &[]) == 0.
            assert_eq!(DeflateState::initial_adler(2), 0);
        }
    }

    /// `new` reproduces the deflateInit2_ allocation and derived sizing, and
    /// resolves `Z_DEFAULT_COMPRESSION` to level 6.
    #[test]
    fn new_allocations_and_config() {
        let s = DeflateState::new(
            Z_DEFAULT_COMPRESSION,
            Z_DEFLATED,
            15,
            8,
            Strategy::Default,
            1,
        )
        .unwrap();

        assert_eq!(s.level, 6); // default resolved
        assert_eq!(s.w_bits, 15);
        assert_eq!(s.w_size, 1 << 15);
        assert_eq!(s.w_mask, (1 << 15) - 1);
        assert_eq!(s.window.len(), 2 << 15);
        assert_eq!(s.window_size, 2 << 15);
        assert_eq!(s.prev.len(), 1 << 15);
        assert_eq!(s.hash_bits, 15);
        assert_eq!(s.hash_size, 1 << 15);
        assert_eq!(s.head.len(), 1 << 15);
        assert_eq!(s.hash_shift, 5); // (15 + 2) / 3
        assert_eq!(s.lit_bufsize, 1 << 14);
        assert_eq!(s.pending_buf.len(), 4 * (1 << 14));
        assert_eq!(s.pending_buf_size, 4 * (1 << 14));
        assert_eq!(s.sym_buf.len(), 3 * (1 << 14));
        assert_eq!(s.sym_end, ((1 << 14) - 1) * 3);
        assert_eq!(s.method, Z_DEFLATED as u8);
        assert!(s.is_valid());
        assert_eq!(s.status, DeflateStatus::Init); // wrap == 1
        assert_eq!(s.last_flush, -2);
        // lm_init defaults for level 6.
        assert_eq!(s.good_match, 8);
        assert_eq!(s.max_lazy_match, 16);
        assert_eq!(s.nice_match, 128);
        assert_eq!(s.max_chain_length, 128);
        assert_eq!(s.match_length, MIN_MATCH - 1);
        assert_eq!(s.prev_length, MIN_MATCH - 1);
    }

    /// An 8-bit window is legal only with the zlib wrapper and is bumped to 9.
    #[test]
    fn new_window_bits_eight_bumped() {
        let s = DeflateState::new(6, Z_DEFLATED, 8, 8, Strategy::Default, 1).unwrap();
        assert_eq!(s.w_bits, 9);
        assert_eq!(s.w_size, 1 << 9);
    }

    /// Invalid parameter combinations must be rejected exactly as zlib does.
    #[test]
    fn new_rejects_invalid_params() {
        // windowBits == 8 requires wrap == 1.
        assert!(DeflateState::new(6, Z_DEFLATED, 8, 8, Strategy::Default, 0).is_err());
        // method must be Z_DEFLATED.
        assert!(DeflateState::new(6, 7, 15, 8, Strategy::Default, 1).is_err());
        // level out of range.
        assert!(DeflateState::new(10, Z_DEFLATED, 15, 8, Strategy::Default, 1).is_err());
        // windowBits out of range.
        assert!(DeflateState::new(6, Z_DEFLATED, 16, 8, Strategy::Default, 1).is_err());
        assert!(DeflateState::new(6, Z_DEFLATED, 7, 8, Strategy::Default, 1).is_err());
        // memLevel out of range: `0` (below the `1` minimum) and `10` (above
        // MAX_MEM_LEVEL == 9) are rejected; the full `1..=9` range is accepted
        // (see `new_accepts_max_mem_level`).
        assert!(DeflateState::new(6, Z_DEFLATED, 15, 0, Strategy::Default, 1).is_err());
        assert!(DeflateState::new(6, Z_DEFLATED, 15, 10, Strategy::Default, 1).is_err());
    }

    /// `memLevel == MAX_MEM_LEVEL == 9` must be accepted, matching reference
    /// zlib on modern (non-`MAXSEG_64K`) platforms (`deflate.c` L434). This is
    /// the boundary that a stricter `MAX_MEM_LEVEL == 8` would wrongly reject.
    #[test]
    fn new_accepts_max_mem_level() {
        // Every in-range memLevel (1..=9) constructs successfully.
        for mem_level in 1..=MAX_MEM_LEVEL {
            assert!(
                DeflateState::new(6, Z_DEFLATED, 15, mem_level, Strategy::Default, 1).is_ok(),
                "memLevel {mem_level} should be accepted"
            );
        }
        // memLevel == 9 sizes the hash table with hash_bits == mem_level + 7 == 16.
        let s = DeflateState::new(6, Z_DEFLATED, 15, 9, Strategy::Default, 1).unwrap();
        assert_eq!(s.mem_level, 9);
        assert_eq!(s.hash_bits, 16);
        assert_eq!(s.hash_size, 1 << 16);
        assert_eq!(s.head.len(), 1 << 16);
        assert_eq!(s.lit_bufsize, 1 << 15);
    }

    /// `slide_hash` decrements entries by `w_size`, flooring at `NIL`.
    #[test]
    fn slide_hash_saturates() {
        // Small window (w_size == 512) keeps the arithmetic easy to check.
        let mut s = DeflateState::new(6, Z_DEFLATED, 9, 8, Strategy::Default, 1).unwrap();
        assert_eq!(s.w_size, 512);
        s.head[0] = 600;
        s.head[1] = 100; // below w_size -> becomes NIL
        s.prev[0] = 512; // exactly w_size -> becomes 0
        s.prev[1] = 700;
        s.slide_hash();
        assert_eq!(s.head[0], 600 - 512);
        assert_eq!(s.head[1], NIL);
        assert_eq!(s.prev[0], 0);
        assert_eq!(s.prev[1], 700 - 512);
        assert!(s.slid);
    }

    /// `longest_match` finds a planted match and reports its length (capped by
    /// the lookahead), setting `match_start`.
    #[test]
    fn longest_match_finds_planted_match() {
        let mut s = DeflateState::new(9, Z_DEFLATED, 15, 8, Strategy::Default, 0).unwrap();

        // Deterministic window.
        s.window.iter_mut().for_each(|b| *b = 0);
        let pat: [u8; 12] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mstart = 40usize;
        let strstart = 100usize;
        s.window[mstart..mstart + 12].copy_from_slice(&pat);
        s.window[strstart..strstart + 12].copy_from_slice(&pat);
        // Make the byte just past the pattern differ so the match ends at 12.
        s.window[mstart + 12] = 201;
        s.window[strstart + 12] = 202;

        s.strstart = strstart;
        s.lookahead = 12; // cap the match at exactly the planted length
        s.prev_length = MIN_MATCH - 1; // best_len starts at 2
        s.good_match = 9999;
        s.nice_match = 9999;
        s.max_chain_length = 64;

        let len = s.longest_match(mstart);
        assert_eq!(len, 12);
        assert_eq!(s.match_start, mstart);
    }

    /// With empty input, `fill_window` performs no reads but still zero-fills
    /// the `WIN_INIT` region beyond the (empty) data and advances high_water.
    #[test]
    fn fill_window_zeroes_win_init() {
        let mut s = DeflateState::new(6, Z_DEFLATED, 15, 8, Strategy::Default, 1).unwrap();
        assert_eq!(s.high_water, 0);

        let input: [u8; 0] = [];
        let mut output = [0u8; 16];
        let mut io = IoContext::new(&input, &mut output);

        s.fill_window(&mut io);

        // curr == 0, so the second branch zeroes WIN_INIT bytes from 0.
        assert_eq!(s.high_water, WIN_INIT);
        assert_eq!(s.lookahead, 0);
    }

    /// `DeflateState` is deep-cloneable (supports `deflateCopy`); owned buffers
    /// are duplicated and index-based state needs no pointer fix-ups.
    #[test]
    fn deflate_state_is_cloneable() {
        let mut s = DeflateState::new(6, Z_DEFLATED, 15, 8, Strategy::Default, 1).unwrap();
        s.window[7] = 0x5A;
        s.strstart = 12;

        let c = s.clone();
        assert_eq!(c.w_size, s.w_size);
        assert_eq!(c.window.len(), s.window.len());
        assert_eq!(c.window[7], 0x5A);
        assert_eq!(c.strstart, 12);
        assert_eq!(c.status, s.status);
        assert_eq!(c.pending_buf.len(), s.pending_buf.len());
    }

    /// `read_buf_into` copies input, updates the Adler-32 checksum for zlib
    /// framing, and advances the input cursors.
    #[test]
    fn read_buf_into_copies_and_checksums() {
        let input = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut next_in = 0usize;
        let mut avail_in = input.len();
        let mut total_in = 0u64;
        let mut adler = adler32(1, &[]);
        let mut dst = [0u8; 4];

        let n = DeflateState::read_buf_into(
            &input,
            &mut next_in,
            &mut avail_in,
            &mut total_in,
            &mut adler,
            1, // zlib framing -> adler32
            &mut dst,
        );

        assert_eq!(n, 4);
        assert_eq!(dst, [1, 2, 3, 4]);
        assert_eq!(next_in, 4);
        assert_eq!(avail_in, 4);
        assert_eq!(total_in, 4);
        assert_eq!(adler, adler32(1, &[1, 2, 3, 4]));
    }
}
