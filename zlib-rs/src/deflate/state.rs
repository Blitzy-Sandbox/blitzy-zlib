//! Central data model for the DEFLATE compressor.
//!
//! This module is the safe-Rust translation of the C `deflate_state` structure
//! (`deflate.h` lines 104-288) together with the compressor *status* constants
//! (`deflate.h` lines 58-67) and the per-block producer return type (the C
//! `block_state` enum, `deflate.c` lines 64-68). It is the foundational module
//! of `crate::deflate`: every other deflate submodule (`trees`, `strategy`,
//! `fast`, `slow`, `stored`, `huff`, `rle`, and `mod`) operates on a
//! [`DeflateState`].
//!
//! # Ownership and safety model
//!
//! The C compressor manages its working memory by hand: `deflateInit2_` calls
//! `ZALLOC` for the window, hash, and pending buffers, and `deflateEnd` frees
//! them in reverse order. Here every buffer is an owned [`Vec`], sized once at
//! construction:
//!
//! * `deflateEnd`'s deallocation is realized by Rust's automatic [`Drop`] of the
//!   owned `Vec` buffers (and of the `Box<DeflateState>` that wraps this struct
//!   inside `crate::stream`). **No manual [`Drop`] implementation is needed or
//!   provided.** The C `Z_DATA_ERROR` result returned when a stream is ended
//!   mid-compression (`status == BUSY_STATE`) is a *control-flow* concern handled
//!   in `mod.rs::deflate_end`, not a memory concern, so it does not belong here.
//! * `deflateCopy`'s deep duplication is realized by `#[derive(Clone)]`. The C
//!   implementation re-allocates the buffers and then fixes up the self-pointers
//!   (`l_desc.dyn_tree = dyn_ltree`, the `pending_out`/`sym_buf` offsets); none of
//!   that is necessary here because [`TreeDesc`] stores no pointer and every
//!   buffer is owned. The C partial-copy optimizations (copying only
//!   `high_water` window bytes, or a `slid`-dependent slice of `prev`) are
//!   bit-exact-equivalent to a full clone because the regions C leaves uncopied
//!   are never read back.
//!
//! This module contains **no `unsafe` and no raw pointers**, satisfying the
//! crate-wide `#![forbid(unsafe_code)]`, and uses only `core`/`alloc` so the
//! crate's `no_std` build path is preserved.
//!
//! # Division of labor with `mod.rs`
//!
//! This file is intentionally *pure data plus sizing*. Parameter validation and
//! the `windowBits` overloading (negative → raw, `> 15` → gzip, `== 8 → 9`) live
//! in `mod.rs`'s `deflateInit2_` equivalent; the four configuration-table–derived
//! match parameters and the Huffman-tree initialization performed by the C
//! `_tr_init` are owned by `mod.rs`/`trees.rs`. See [`DeflateState::new`] for the
//! exact split.

use alloc::vec::Vec;

use crate::constants::{MAX_MATCH, MIN_MATCH, Strategy as CompressionStrategy};
use crate::error::{Result, ZlibError};
use crate::gz_header::GzHeader;

// `CtData` and `StaticTreeDesc` are defined in the sibling `trees.rs` module.
// The `self` import also brings the `trees` module path into scope so the
// `'static` static-descriptor constants can be referenced as
// `trees::STATIC_L_DESC` etc. when initializing the tree descriptors.
use super::trees::{self, CtData, StaticTreeDesc};

// ===========================================================================
// Tree-structure sizing constants
// ===========================================================================
//
// DESIGN NOTE (agent prompt, Phase 1): these deflate-internal tree-sizing
// constants could be sourced canonically from `trees.rs` and imported here.
// They are instead defined LOCALLY as `pub(crate) const` for two reasons: (1)
// `state.rs` is created in parallel with `trees.rs`, so depending on `trees.rs`
// for the *values* (as opposed to the `CtData`/`StaticTreeDesc` *types*, which
// are inherently its domain) would couple the data model to another file's
// availability for no benefit; and (2) every value is fixed by the DEFLATE
// format (RFC 1951), so a local definition can never drift. `trees.rs` is free
// to keep its own copies or to `use super::state::*` — both remain consistent
// because the numbers are format-mandated.

/// Number of literal bytes `0..=255` (C `LITERALS`).
pub(crate) const LITERALS: usize = 256;

/// Number of length codes, excluding the special END_BLOCK code
/// (C `LENGTH_CODES`).
pub(crate) const LENGTH_CODES: usize = 29;

/// Number of literal-or-length codes, including END_BLOCK (C `L_CODES`).
pub(crate) const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes (C `D_CODES`).
pub(crate) const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths (C `BL_CODES`).
pub(crate) const BL_CODES: usize = 19;

/// Maximum Huffman heap size (C `HEAP_SIZE`).
pub(crate) const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// Maximum number of bits any single code may occupy (C `MAX_BITS`).
pub(crate) const MAX_BITS: usize = 15;

/// Size in bits of the bit accumulator [`DeflateState::bi_buf`] (C `Buf_size`).
///
/// This equals the bit width of `u16`, which is precisely why `bi_buf` must be a
/// `u16`: a wider accumulator would change `send_bits` overflow timing and
/// corrupt the bitstream.
pub(crate) const BUF_SIZE: usize = 16;

// Compile-time guarantees for the binding array dimensions (agent prompt,
// Phase 10). A mismatch fails the build instead of silently mis-sizing a
// buffer.
const _: () = assert!(L_CODES == 286);
const _: () = assert!(HEAP_SIZE == 573);
const _: () = assert!(2 * D_CODES + 1 == 61);
const _: () = assert!(2 * BL_CODES + 1 == 39);
const _: () = assert!(MAX_BITS + 1 == 16);
const _: () = assert!(BUF_SIZE == 16);

// ===========================================================================
// Position type
// ===========================================================================

/// A position within the LZ77 sliding window.
///
/// Mirrors the C `typedef ush Pos`: a 16-bit index that keeps the `prev` and
/// `head` hash-chain tables compact. An index into `prev` is a window position
/// taken modulo the window size, so 16 bits always suffice.
pub(crate) type Pos = u16;

// ===========================================================================
// Compressor status
// ===========================================================================

/// Stream status of the DEFLATE compressor.
///
/// This is the exhaustive translation of the C status macros (`deflate.h` lines
/// 58-67). The discriminants are **binding**: the C code compares the raw values
/// directly and they must round-trip losslessly through the `libz-rs-sys` FFI
/// layer, so `#[repr(i32)]` pins each variant to its exact C integer.
///
/// The normal progression is `Init`/`Gzip` → (for gzip headers: `Extra` →
/// `Name` → `Comment` → `Hcrc`) → `Busy` → `Finish`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum DeflateStatus {
    /// `INIT_STATE` — a zlib (or raw) header is pending; advances to `Busy`.
    Init = 42,
    /// `GZIP_STATE` — a gzip header is pending; advances to `Busy` or `Extra`.
    Gzip = 57,
    /// `EXTRA_STATE` — emitting the gzip "extra" field; advances to `Name`.
    Extra = 69,
    /// `NAME_STATE` — emitting the gzip original file name; advances to
    /// `Comment`.
    Name = 73,
    /// `COMMENT_STATE` — emitting the gzip comment; advances to `Hcrc`.
    Comment = 91,
    /// `HCRC_STATE` — emitting the gzip header CRC; advances to `Busy`.
    Hcrc = 103,
    /// `BUSY_STATE` — actively compressing; advances to `Finish`.
    Busy = 113,
    /// `FINISH_STATE` — the stream is complete.
    Finish = 666,
}

// Pin every discriminant to its exact C value (agent prompt, Phase 10).
const _: () = assert!(DeflateStatus::Init as i32 == 42);
const _: () = assert!(DeflateStatus::Gzip as i32 == 57);
const _: () = assert!(DeflateStatus::Extra as i32 == 69);
const _: () = assert!(DeflateStatus::Name as i32 == 73);
const _: () = assert!(DeflateStatus::Comment as i32 == 91);
const _: () = assert!(DeflateStatus::Hcrc as i32 == 103);
const _: () = assert!(DeflateStatus::Busy as i32 == 113);
const _: () = assert!(DeflateStatus::Finish as i32 == 666);

// ===========================================================================
// Block-producer result
// ===========================================================================

/// Result of one invocation of a per-strategy block producer.
///
/// Safe-Rust translation of the C `block_state` enum (`deflate.c` lines 64-68),
/// returned by `deflate_stored`, `deflate_fast`, `deflate_slow`, `deflate_rle`,
/// and `deflate_huff` and matched by the driver in `mod.rs`. It is defined here
/// in `state.rs` rather than in `strategy.rs` on purpose: every strategy module
/// already depends on `state`, so this is the natural shared home and avoids an
/// extra cross-module edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockState {
    /// `need_more` — the block is not complete; more input or more output room
    /// is required before progress can continue.
    NeedMore = 0,
    /// `block_done` — a block flush was performed.
    BlockDone = 1,
    /// `finish_started` — finishing has begun; only more output room is needed
    /// on the next `deflate` call.
    FinishStarted = 2,
    /// `finish_done` — finishing is complete; no more input or output will be
    /// accepted.
    FinishDone = 3,
}

// ===========================================================================
// Huffman tree descriptor
// ===========================================================================

/// Descriptor for a single Huffman tree — the safe translation of the C
/// `tree_desc` struct (`deflate.h` lines 90-94).
///
/// The C structure embeds `ct_data *dyn_tree`, a raw pointer into the owning
/// `deflate_state`'s own arrays. A self-referential pointer cannot exist under
/// `#![forbid(unsafe_code)]`, so (design decision D2) the dynamic tree arrays
/// [`DeflateState::dyn_ltree`], [`DeflateState::dyn_dtree`], and
/// [`DeflateState::bl_tree`] live as separate fields, and this descriptor keeps
/// only the largest used code plus a `'static` reference to the matching static
/// (fixed-Huffman) descriptor. The tree-building routines in `trees.rs` select
/// the active dynamic array by destructuring [`DeflateState`] (for example via a
/// small `TreeId` discriminator), rather than by following a stored pointer.
#[derive(Clone, Copy)]
pub struct TreeDesc {
    /// Largest code with a non-zero frequency (C `int max_code`).
    pub max_code: i32,
    /// The corresponding static (fixed-Huffman) tree descriptor, owned for the
    /// program's lifetime by `trees.rs`.
    pub stat_desc: &'static StaticTreeDesc,
}

// ===========================================================================
// Deflate state
// ===========================================================================

/// The complete DEFLATE compressor state.
///
/// A safe, owning translation of the C `deflate_state` struct (`deflate.h` lines
/// 104-288). Field names follow the C originals (in Rust `snake_case`) so the
/// algorithmic ports in the sibling modules read as direct transliterations of
/// the reference implementation.
///
/// Two C fields are intentionally **absent**:
///
/// * the `strm` back-pointer to the owning `z_stream` — in this design the
///   stream-level scalars (`adler`, `total_in`, `total_out`, `msg`, `data_type`)
///   are threaded through the engine context rather than stored on the state
///   (design decision D1); and
/// * the `compressed_len` / `bits_sent` counters, which exist in C only under
///   `ZLIB_DEBUG`.
///
/// All other fields are present, including the `bi_used`, `slid`, `high_water`,
/// `insert`, `matches`, `sym_end`, and `pending_buf_size` fields that later
/// modules (`deflate_stored`, `slide_hash`, `bi_windup`, `deflateCopy`) rely on.
///
/// See the [module documentation](self) for the `Drop`/`Clone` rationale.
#[derive(Clone)]
pub struct DeflateState {
    // ---- Output / pending buffer ----------------------------------------
    /// Current compressor status (C `int status`).
    pub status: DeflateStatus,
    /// Output staging buffer awaiting flush to `next_out` (C `pending_buf`).
    ///
    /// Sized to [`pending_buf_size`](Self::pending_buf_size) bytes. Design
    /// decision D5: this is a standalone buffer and is **not** overlaid with
    /// [`sym_buf`](Self::sym_buf) the way the C allocation is.
    pub pending_buf: Vec<u8>,
    /// Capacity of [`pending_buf`](Self::pending_buf) in bytes, equal to
    /// `lit_bufsize * 4` (C `pending_buf_size`).
    ///
    /// Preserved verbatim from C because `deflate_stored` derives its minimum
    /// block size from it (`min(pending_buf_size - 5, w_size)`).
    pub pending_buf_size: usize,
    /// Index of the next byte in [`pending_buf`](Self::pending_buf) to copy to
    /// the output (replaces the C `pending_out` pointer; the C reset to
    /// `pending_buf` corresponds to `0`).
    pub pending_out: usize,
    /// Number of bytes currently held in [`pending_buf`](Self::pending_buf)
    /// awaiting flush (C `pending`).
    pub pending: usize,
    /// Wrapper selector: `1` = zlib, `0` = raw, `2` = gzip (C `wrap`). It is made
    /// negative during the `Z_FINISH` trailer and restored by the reset path.
    pub wrap: i32,

    // ---- gzip header ----------------------------------------------------
    /// Caller-supplied gzip header to emit, set via `deflateSetHeader`
    /// (C `gz_headerp gzhead`).
    ///
    /// The field is always present regardless of the `gzip` feature; only the
    /// header *emission* logic in `mod.rs` is feature-gated, which keeps the data
    /// model uniform.
    pub gzhead: Option<GzHeader>,
    /// Cursor within the gzip extra / name / comment field during header
    /// emission (C `gzindex`).
    pub gzindex: usize,

    // ---- Configuration / method ----------------------------------------
    /// Compression method; always `Z_DEFLATED` (`8`) (C `Byte method`).
    pub method: u8,
    /// `flush` argument from the previous `deflate` call; initialized to `-2`
    /// (C `last_flush`).
    pub last_flush: i32,

    // ---- Sliding window -------------------------------------------------
    /// LZ77 window size in bytes, `1 << w_bits` (C `w_size`).
    pub w_size: usize,
    /// `log2(w_size)` (C `w_bits`).
    pub w_bits: u32,
    /// `w_size - 1`, used to wrap window indices (C `w_mask`).
    pub w_mask: usize,
    /// The sliding window. Length `2 * w_size`: input is read into the upper half
    /// and migrated to the lower half to retain a dictionary (C `window`).
    pub window: Vec<u8>,
    /// Actual allocated window length, `2 * w_size` (C `window_size`).
    pub window_size: usize,
    /// Hash-chain links to older strings with the same hash (C `prev`). Length
    /// `w_size`; an entry is a window position modulo `w_size`.
    pub prev: Vec<Pos>,
    /// Heads of the hash chains, or `NIL` (C `head`). Length `hash_size`.
    pub head: Vec<Pos>,
    /// Rolling hash of the string about to be inserted (C `ins_h`).
    pub ins_h: usize,
    /// Number of slots in the hash table, `1 << hash_bits` (C `hash_size`).
    pub hash_size: usize,
    /// `log2(hash_size)` (C `hash_bits`).
    pub hash_bits: u32,
    /// `hash_size - 1` (C `hash_mask`).
    pub hash_mask: usize,
    /// Per-byte shift applied to `ins_h` so that after `MIN_MATCH` steps the
    /// oldest byte no longer affects the hash (C `hash_shift`).
    pub hash_shift: u32,
    /// Window position at the start of the current output block. Goes negative
    /// when the window slides backward, hence a signed type (C `long
    /// block_start`).
    pub block_start: isize,

    // ---- Match state ----------------------------------------------------
    /// Length of the best match found at the current position (C `match_length`).
    pub match_length: usize,
    /// Previous match position used by the lazy evaluator (C `IPos prev_match`).
    /// Stored in 16 bits, which always suffices for a valid window position.
    pub prev_match: u16,
    /// Set when a deferred (lazy) match from the previous step exists
    /// (C `int match_available`).
    pub match_available: bool,
    /// Start of the string currently being inserted (C `strstart`).
    pub strstart: usize,
    /// Start of the matching string in the window (C `match_start`).
    pub match_start: usize,
    /// Number of valid bytes ahead of `strstart` in the window (C `lookahead`).
    pub lookahead: usize,
    /// Length of the best match at the previous step; shorter matches are
    /// discarded by the lazy heuristic (C `prev_length`).
    pub prev_length: usize,
    /// Upper bound on hash-chain traversal length (C `max_chain_length`).
    ///
    /// Filled by `mod.rs`'s `lm_init` from `configuration_table[level]`.
    pub max_chain_length: u32,
    /// Lazy-match threshold; also serves as the C `max_insert_length` (the two
    /// are the same field via `#define max_insert_length max_lazy_match`).
    ///
    /// Filled by `mod.rs`'s `lm_init` from `configuration_table[level]`.
    pub max_lazy_match: usize,
    /// Compression level `0..=9` (C `int level`).
    pub level: i32,
    /// Compression strategy (C `int strategy`).
    ///
    /// This is the **public API** strategy ([`crate::constants::Strategy`],
    /// imported here as `CompressionStrategy`), distinct from the internal
    /// stored/fast/slow dispatch enum in `crate::deflate::strategy`.
    pub strategy: CompressionStrategy,
    /// Use a faster search once the previous match exceeds this length
    /// (C `good_match`).
    ///
    /// Filled by `mod.rs`'s `lm_init` from `configuration_table[level]`.
    pub good_match: u32,
    /// Stop searching once the current match reaches this length (C `int
    /// nice_match`).
    ///
    /// Filled by `mod.rs`'s `lm_init` from `configuration_table[level]`.
    pub nice_match: i32,

    // ---- Huffman trees --------------------------------------------------
    /// Literal/length tree (C `dyn_ltree[HEAP_SIZE]`).
    pub dyn_ltree: [CtData; HEAP_SIZE],
    /// Distance tree (C `dyn_dtree[2*D_CODES+1]`).
    pub dyn_dtree: [CtData; 2 * D_CODES + 1],
    /// Bit-length tree used to transmit the other trees (C
    /// `bl_tree[2*BL_CODES+1]`).
    pub bl_tree: [CtData; 2 * BL_CODES + 1],
    /// Descriptor for the literal/length tree (C `l_desc`).
    pub l_desc: TreeDesc,
    /// Descriptor for the distance tree (C `d_desc`).
    pub d_desc: TreeDesc,
    /// Descriptor for the bit-length tree (C `bl_desc`).
    pub bl_desc: TreeDesc,
    /// Count of codes at each bit length while building an optimal tree
    /// (C `bl_count[MAX_BITS+1]`).
    pub bl_count: [u16; MAX_BITS + 1],
    /// Heap used to build the Huffman trees; `heap[2*n]`/`heap[2*n+1]` are the
    /// children of `heap[n]` and `heap[0]` is unused (C `heap[2*L_CODES+1]`).
    pub heap: [i32; 2 * L_CODES + 1],
    /// Number of elements currently in [`heap`](Self::heap) (C `heap_len`).
    pub heap_len: usize,
    /// Index in [`heap`](Self::heap) of the element of largest frequency
    /// (C `heap_max`).
    pub heap_max: usize,
    /// Depth of each tree node, used as a tie-breaker between equal frequencies
    /// (C `depth[2*L_CODES+1]`).
    pub depth: [u8; 2 * L_CODES + 1],

    // ---- Symbol buffer --------------------------------------------------
    /// Buffer of `(distance, literal/length)` symbols for the current block
    /// (C `sym_buf`).
    ///
    /// Design decision D5: a **separate** owned buffer of `lit_bufsize * 3` bytes
    /// (three bytes per symbol: distance low, distance high, length/literal),
    /// rather than the C overlay into `pending_buf`.
    pub sym_buf: Vec<u8>,
    /// Number of symbol slots, `1 << (memLevel + 6)` (C `lit_bufsize`).
    pub lit_bufsize: usize,
    /// Running write index into [`sym_buf`](Self::sym_buf) (C `sym_next`).
    pub sym_next: usize,
    /// Index at which [`sym_buf`](Self::sym_buf) is considered full, equal to
    /// `(lit_bufsize - 1) * 3` (C `sym_end`).
    pub sym_end: usize,

    // ---- Counters / bit buffer ------------------------------------------
    /// Bit length of the current block under the optimal (dynamic) trees
    /// (C `opt_len`).
    pub opt_len: usize,
    /// Bit length of the current block under the static trees (C `static_len`).
    pub static_len: usize,
    /// Number of string matches in the current block (C `matches`).
    pub matches: usize,
    /// Bytes at the start of the block still to be inserted into the hash table
    /// (C `insert`).
    pub insert: usize,
    /// Bit accumulator; bits are inserted from the least-significant end
    /// (C `ush bi_buf`).
    ///
    /// **Must** be `u16` to reproduce the C `send_bits` overflow timing exactly.
    pub bi_buf: u16,
    /// Number of valid low bits currently in [`bi_buf`](Self::bi_buf)
    /// (C `int bi_valid`).
    pub bi_valid: i32,
    /// Number of bits used when last aligning to a byte boundary, set by
    /// `bi_windup` as `((bi_valid - 1) & 7) + 1` and read by `deflate_stored`
    /// (C `int bi_used`).
    pub bi_used: i32,
    /// High-water mark of initialized bytes in [`window`](Self::window); bytes
    /// above it are zeroed lazily so match routines never read uninitialized
    /// memory (C `high_water`).
    pub high_water: usize,
    /// `1` once the hash table has been slid since it was last cleared, `0`
    /// otherwise; read by `deflateCopy` (C `int slid`).
    pub slid: i32,
}

// ===========================================================================
// Window / lookahead constants (deflate.h macros)
// ===========================================================================

/// Minimum amount of lookahead, except at the end of the input (C macro
/// `MIN_LOOKAHEAD`).
///
/// Equals `MAX_MATCH + MIN_MATCH + 1`, i.e. `258 + 3 + 1 = 262` — the longest
/// encodable match, plus the minimum match, plus one guard byte. (The C macro is
/// `(MAX_MATCH+MIN_MATCH+1)`; this preserves that formula exactly.)
pub(crate) const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes past the end of the data that must be initialized in the
/// window so the longest-match routines never read uninitialized memory
/// (C macro `WIN_INIT`); equals `MAX_MATCH` = `258`.
pub(crate) const WIN_INIT: usize = MAX_MATCH;

const _: () = assert!(MIN_LOOKAHEAD == 262);
const _: () = assert!(WIN_INIT == 258);

impl DeflateState {
    /// Allocate and initialize a fresh compressor state, mirroring the buffer
    /// sizing of the C `deflateInit2_` (`deflate.c` lines 387-533) followed by
    /// the data-model resets of `deflateReset` (`deflate.c` lines 644-712).
    ///
    /// # Division of labor (binding)
    ///
    /// This constructor is responsible **only** for allocation and data-model
    /// initialization. It deliberately does *not* perform parameter validation
    /// or `windowBits` overloading: `mod.rs`'s `deflateInit2_` equivalent does
    /// all of that first — rejecting bad arguments, resolving
    /// `Z_DEFAULT_COMPRESSION` to level `6`, mapping a negative `windowBits` to a
    /// raw stream (`wrap = 0`), a `windowBits > 15` to a gzip stream
    /// (`wrap = 2`), and `windowBits == 8` to `9` — and then calls this function
    /// with an already-normalized `window_bits` in `8..=15` and the computed
    /// `wrap`.
    ///
    /// Two groups of fields are left for `mod.rs`/`trees.rs` to populate, exactly
    /// as the C code defers them out of `deflateInit2_`:
    ///
    /// * the four `configuration_table[level]`-derived match parameters
    ///   ([`max_lazy_match`](Self::max_lazy_match), [`good_match`](Self::good_match),
    ///   [`nice_match`](Self::nice_match), [`max_chain_length`](Self::max_chain_length))
    ///   start at `0`; `mod.rs`'s `lm_init` fills them from the configuration
    ///   table, which lives with the strategy logic rather than in this
    ///   data-model module; and
    /// * the Huffman trees start fully zeroed; the work the C `_tr_init`/
    ///   `init_block` does (setting `dyn_ltree[END_BLOCK].Freq = 1`, etc.) is
    ///   `trees.rs`'s responsibility. The tree *descriptors* are still wired up
    ///   here so the struct is internally consistent the moment it is returned.
    ///
    /// # Parameters
    ///
    /// * `level` — resolved compression level in `0..=9`.
    /// * `method` — compression method; always `Z_DEFLATED` (`8`).
    /// * `window_bits` — normalized window exponent in `8..=15`.
    /// * `mem_level` — memory level in `1..=9` (validated by the caller).
    /// * `strategy` — the public-API compression strategy.
    /// * `wrap` — wrapper selector computed by the caller: `0` raw, `1` zlib,
    ///   `2` gzip.
    ///
    /// # Errors
    ///
    /// Returns [`ZlibError::MemError`] if any buffer allocation cannot be
    /// satisfied, mirroring the C `Z_MEM_ERROR` path. Allocation is fallible, so
    /// this constructor never panics on an out-of-memory condition.
    pub fn new(
        level: i32,
        method: u8,
        window_bits: u32,
        mem_level: i32,
        strategy: CompressionStrategy,
        wrap: i32,
    ) -> Result<Self> {
        // --- Window geometry (deflate.c lines 446-450) ---
        let w_bits = window_bits;
        let w_size = 1usize << w_bits;
        let w_mask = w_size - 1;

        // --- Hash-table geometry (deflate.c lines 452-455) ---
        let hash_bits = (mem_level as u32) + 7;
        let hash_size = 1usize << hash_bits;
        let hash_mask = hash_size - 1;
        // C: hash_shift = (hash_bits + MIN_MATCH - 1) / MIN_MATCH, a ceil
        // division that is exactly `div_ceil` for these (small, non-negative)
        // operands.
        let hash_shift = hash_bits.div_ceil(MIN_MATCH as u32);

        // --- Literal / symbol buffer sizing (deflate.c lines 463, 510-521) ---
        let lit_bufsize = 1usize << ((mem_level as u32) + 6);
        // C overlays `pending_buf` and `sym_buf` within a single allocation of
        // `lit_bufsize * LIT_BUFS` (`LIT_BUFS == 4`) bytes. Design decision D5
        // keeps them as two separate owned buffers, but the sizes that drive
        // block boundaries are preserved verbatim so output stays bit-identical:
        // `pending_buf_size == lit_bufsize * 4` and `sym_end == (lit_bufsize - 1)
        // * 3`.
        let pending_buf_size = lit_bufsize * 4;
        let sym_end = (lit_bufsize - 1) * 3;

        // --- Owned buffers, allocated fallibly (mirrors the C Z_MEM_ERROR
        //     path; `vec!`-style infallible allocation would abort instead). ---
        let window = try_alloc(2 * w_size, 0u8)?;
        let prev = try_alloc(w_size, 0u16)?;
        let head = try_alloc(hash_size, 0u16)?;
        let pending_buf = try_alloc(pending_buf_size, 0u8)?;
        let sym_buf = try_alloc(lit_bufsize * 3, 0u8)?;

        // Initial status mirrors `deflateResetKeep` (deflate.c lines 661-666):
        // a gzip wrapper starts in `Gzip`, everything else in `Init`.
        let status = if wrap == 2 {
            DeflateStatus::Gzip
        } else {
            DeflateStatus::Init
        };

        Ok(Self {
            // Output / pending buffer
            status,
            pending_buf,
            pending_buf_size,
            pending_out: 0,
            pending: 0,
            wrap,

            // gzip header
            gzhead: None,
            gzindex: 0,

            // Configuration / method
            method,
            last_flush: -2,

            // Sliding window
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

            // Match state (lm_init values that do not depend on the config
            // table; the four that do are left at 0 for mod.rs::lm_init).
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

            // Huffman trees (zeroed; _tr_init/init_block run by trees.rs)
            dyn_ltree: core::array::from_fn(|_| CtData::default()),
            dyn_dtree: core::array::from_fn(|_| CtData::default()),
            bl_tree: core::array::from_fn(|_| CtData::default()),
            l_desc: TreeDesc {
                max_code: 0,
                stat_desc: &trees::STATIC_L_DESC,
            },
            d_desc: TreeDesc {
                max_code: 0,
                stat_desc: &trees::STATIC_D_DESC,
            },
            bl_desc: TreeDesc {
                max_code: 0,
                stat_desc: &trees::STATIC_BL_DESC,
            },
            bl_count: [0; MAX_BITS + 1],
            heap: [0; 2 * L_CODES + 1],
            heap_len: 0,
            heap_max: 0,
            depth: [0; 2 * L_CODES + 1],

            // Symbol buffer
            sym_buf,
            lit_bufsize,
            sym_next: 0,
            sym_end,

            // Counters / bit buffer
            opt_len: 0,
            static_len: 0,
            matches: 0,
            insert: 0,
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,
            high_water: 0,
            slid: 0,
        })
    }

    /// Append one byte to the pending output buffer (C macro `put_byte`).
    ///
    /// Mirrors the C macro's documented `IN assertion: there is enough room in
    /// pending_buf`. The caller must ensure `pending < pending_buf_size`; a
    /// violation triggers the standard slice bounds check (a panic) rather than
    /// the silent out-of-bounds write the C macro would perform.
    #[inline]
    pub(crate) fn put_byte(&mut self, b: u8) {
        self.pending_buf[self.pending] = b;
        self.pending += 1;
    }

    /// Maximum match distance (C macro `MAX_DIST(s)`).
    ///
    /// Match distances are limited to `w_size - MIN_LOOKAHEAD` to keep the window
    /// bookkeeping simple, as in the C implementation.
    #[inline]
    #[must_use]
    pub(crate) fn max_dist(&self) -> usize {
        self.w_size - MIN_LOOKAHEAD
    }
}

/// Fallibly allocate a `Vec<T>` of `len` elements, each initialized to `fill`.
///
/// This is the safe-Rust counterpart of the C `ZALLOC` calls in `deflateInit2_`:
/// it returns [`ZlibError::MemError`] when the allocation cannot be satisfied
/// instead of aborting the process the way the infallible `vec!`/`Vec::resize`
/// path would. `try_reserve_exact` requests exactly `len` capacity up front, so
/// the subsequent `resize` cannot reallocate and therefore cannot panic.
fn try_alloc<T: Clone>(len: usize, fill: T) -> Result<Vec<T>> {
    let mut v: Vec<T> = Vec::new();
    v.try_reserve_exact(len).map_err(|_| ZlibError::MemError)?;
    v.resize(len, fill);
    Ok(v)
}
