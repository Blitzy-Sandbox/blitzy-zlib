//! DEFLATE engine state — `DeflateState`, `DeflateStream`, `BlockState`, and the
//! low-level LZ77 plumbing.
//!
//! This module is the **foundation** of the DEFLATE compression engine. It is a
//! faithful, 100% safe-Rust port of:
//!
//! * **`deflate.h`** — the `deflate_state` struct (the persistent ~80-field
//!   compression context) plus the engine-internal constants and macros.
//! * **`deflate.c`** — the LZ77 plumbing functions: `read_buf`, `fill_window`,
//!   `slide_hash`, `longest_match`, `lm_init`, `deflateStateCheck`,
//!   `flush_pending`, `put_byte`, and `putShortMSB`.
//!
//! # Design
//!
//! The C implementation keeps the persistent engine state, the transient
//! per-call input/output cursors, and the dynamic Huffman/LZ77 bookkeeping all
//! reachable through a single `deflate_state *` plus the caller's `z_stream`.
//! The Rust port splits this into two cooperating types:
//!
//! * [`DeflateState`] — the **persistent** engine state that lives across
//!   `deflate()` calls. It owns every working buffer (`window`, `pending_buf`,
//!   `prev`, `head`) as global-allocator [`Vec`]/[`Box`], so teardown is the
//!   automatic, deterministic, leak-free RAII drop that replaces `deflateEnd`
//!   (AAP §0.6.3).
//! * [`DeflateStream`] — the **transient** per-call context that pairs a
//!   `&mut DeflateState` with the current input/output slices. It replaces the
//!   `next_in`/`avail_in`/`next_out`/`avail_out` quartet for the duration of a
//!   single `deflate()` call and is where `read_buf`, `fill_window`,
//!   `flush_pending`, and `longest_match` operate.
//!
//! This split is precisely what lets the five strategy functions
//! (`deflate_stored`/`fast`/`slow`/`rle`/`huff`, in sibling modules) remain
//! 100% safe Rust while the only `unsafe` in the whole crate stays confined to
//! `src/ffi.rs` and `src/inflate/fast.rs` (AAP §0.6.2).
//!
//! # Byte-identical output
//!
//! Every constant, sizing formula, hash computation, window slide, and match
//! decision in this module is ported **mechanically** from C zlib so that the
//! produced bitstream is byte-for-byte identical for the same input, level,
//! strategy, and window configuration (AAP §0.6.6, §0.6.7, §0.7.1). Nothing
//! here is "improved" or "modernised" in a way that would change the chosen
//! LZ77 matches or Huffman block boundaries.

// `no_std`-clean: under `std` the prelude already provides `Box`/`Vec`/`vec!`;
// under `no-std` we pull them from `alloc`. Only `core::` and `alloc::` are
// ever used in this module — never `std::`.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use crate::checksum::{adler32, crc32};
use crate::constants::Z_UNKNOWN;
// `HuffmanNode` (trees.rs) and `DeflateStatus` (mod.rs) live in sibling deflate
// modules. The crate compiles as a single unit, so these intra-crate reference
// cycles (state ↔ trees, state ↔ mod) are perfectly fine.
use crate::deflate::DeflateStatus;
use crate::deflate::trees::HuffmanNode;
use crate::stream::{StreamKind, StreamState};

// ===========================================================================
// Phase A — Deflate-internal constants
//
// Ported verbatim from `deflate.h` (and the handful that originate in
// `zlib.h` / `zutil.h`). DO NOT change any numeric value: each one feeds the
// buffer-sizing arithmetic and the Huffman/LZ77 logic that must match C zlib
// bit-for-bit. These are `pub(crate)` because the sibling deflate modules
// (`trees`, `strategy`, `stored`, `fast`, `slow`, `rle`, `huff`, `mod`) build
// directly on them.
// ===========================================================================

/// Number of length codes, not counting the special `END_BLOCK` code.
pub(crate) const LENGTH_CODES: usize = 29;

/// Number of literal bytes (0..=255).
pub(crate) const LITERALS: usize = 256;

/// Number of literal/length codes, including the `END_BLOCK` code.
/// `LITERALS + 1 + LENGTH_CODES` = 286.
pub(crate) const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes.
pub(crate) const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths.
pub(crate) const BL_CODES: usize = 19;

/// Maximum heap size: `2 * L_CODES + 1` = 573.
pub(crate) const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// All codes must not exceed `MAX_BITS` bits.
pub(crate) const MAX_BITS: usize = 15;

/// Bit length codes must not exceed `MAX_BL_BITS` bits.
pub(crate) const MAX_BL_BITS: usize = 7;

/// Size of the bit buffer in bits (C `Buf_size`).
pub(crate) const BUF_SIZE: i32 = 16;

/// The minimum match length.
pub(crate) const MIN_MATCH: usize = 3;

/// The maximum match length.
pub(crate) const MAX_MATCH: usize = 258;

/// The minimum lookahead, i.e. the amount of data the deflate engine keeps
/// available ahead of `strstart`: `MAX_MATCH + MIN_MATCH + 1` = 262.
pub(crate) const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes after the end of the data in the window that must be
/// initialized before any read (mirrors C `WIN_INIT == MAX_MATCH`).
pub(crate) const WIN_INIT: usize = MAX_MATCH;

/// Hash-chain terminator (an invalid window position).
pub(crate) const NIL: u16 = 0;

/// Matches of length 3 are discarded if their distance exceeds `TOO_FAR`.
pub(crate) const TOO_FAR: usize = 4096;

/// End-of-block literal/length code symbol.
pub(crate) const END_BLOCK: usize = 256;

/// Number of buffers a single byte of `pending_buf` is multiplexed into.
///
/// In upstream zlib this is `LIT_BUFS`, which is 4 when `LIT_MEM` is **not**
/// defined (`deflate.h` keeps the `LIT_MEM` define commented out). With
/// `LIT_BUFS == 4`, each symbol occupies 3 bytes in the overlaid symbol buffer.
pub(crate) const LIT_BUFS: usize = 4;

/// Maximum stored-block payload size (a 16-bit length field).
pub(crate) const MAX_STORED: usize = 65535;

/// `PRESET_DICT` flag bit, set in the zlib header when a preset dictionary was
/// supplied (`zlib.h`).
pub(crate) const PRESET_DICT: i32 = 0x20;

/// The DEFLATE compression method identifier (`zlib.h`).
pub(crate) const Z_DEFLATED: i32 = 8;

/// A window position. Mirrors C `Pos`/`Posf` (`ush` == `u16`). Window indices
/// never exceed `2 * w_size - 1` and `w_size <= 32768`, so a `u16` always
/// suffices.
pub(crate) type Pos = u16;

/// Maximum distance an LZ77 match may reference, given the window size.
///
/// Mirrors the C macro `MAX_DIST(s) == s->w_size - MIN_LOOKAHEAD`.
///
/// `deflateInit2_` promotes `windowBits == 8` to `9` (the historical
/// 256-byte-window bug fix, `deflate.c` L439), guaranteeing
/// `w_size >= 512 > MIN_LOOKAHEAD (262)`, so this subtraction never underflows.
#[inline]
pub(crate) const fn max_dist(w_size: usize) -> usize {
    w_size - MIN_LOOKAHEAD
}

// ===========================================================================
// Phase B — BlockState
//
// Ports the C `block_state` enum (`deflate.c` L63-68). Returned by all five
// strategy functions (`deflate_stored`/`fast`/`slow`/`rle`/`huff`) to tell the
// `deflate()` driver how far the current block got.
// ===========================================================================

/// Result of a single call to one of the block-compression (strategy)
/// functions, mirroring C `block_state`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlockState {
    /// `need_more`: ran out of input or output before reaching the end of the
    /// current block.
    NeedMore,
    /// `block_done`: a block flush was performed and the block is complete.
    BlockDone,
    /// `finish_started`: finishing has begun; only more output room is needed
    /// at the next `deflate()` call.
    FinishStarted,
    /// `finish_done`: finishing is complete; no more input or output will be
    /// accepted.
    FinishDone,
}

// ===========================================================================
// Phase C — DeflateState
//
// Ports the C `deflate_state` struct (`deflate.h` L48-280). This is the
// PERSISTENT engine state, owned across `deflate()` calls. Field groups and
// ordering mirror the C source for ease of cross-referencing.
//
// `#[derive(Clone)]` is the safe-Rust replacement for `deflateCopy`
// (`deflate.c` L1317-1377): because the raw `tree_desc` pointers were replaced
// by plain `*_desc_max_code: i32` fields (the dynamic trees live inline and the
// static descriptors are `const`s selected by a `TreeKind` enum in trees.rs),
// there is nothing to "rewire" after a clone — a structural deep copy of the
// owned buffers is exactly correct. The derive copies the *entire* buffers
// rather than only the used prefixes, which is functionally identical to C
// because the unused tail bytes are zero.
// ===========================================================================

/// The persistent DEFLATE compression engine state.
///
/// Owns all working buffers via the global allocator; custom C
/// `zalloc`/`zfree` hooks are bridged at the `ffi.rs` boundary, never here.
#[derive(Clone)]
pub(crate) struct DeflateState {
    // -- Accounting -------------------------------------------------------
    // C keeps these on `z_stream`; the Rust engine keeps working copies here
    // and the wrapper / FFI layer mirrors them to/from `ZStream` after each
    // call (see the "Accounting sync" note at the bottom of this module).
    /// Header-emission state machine
    /// (INIT/GZIP/EXTRA/NAME/COMMENT/HCRC/BUSY/FINISH).
    pub(crate) status: DeflateStatus,
    /// Running checksum: Adler-32 for the zlib wrapper, CRC-32 for gzip.
    pub(crate) adler: u32,
    /// Total number of input bytes read so far.
    pub(crate) total_in: u64,
    /// Total number of output bytes produced so far.
    pub(crate) total_out: u64,
    /// Best guess about the data type: `Z_BINARY`(0) / `Z_TEXT`(1) /
    /// `Z_UNKNOWN`(2).
    pub(crate) data_type: i32,
    /// Last error message, or `None`.
    pub(crate) msg: Option<&'static str>,

    // -- Pending output buffer -------------------------------------------
    /// Output still pending (owned). Size is `lit_bufsize * LIT_BUFS`.
    pub(crate) pending_buf: Vec<u8>,
    /// Size of `pending_buf` in bytes (`lit_bufsize * LIT_BUFS`).
    pub(crate) pending_buf_size: usize,
    /// Index into `pending_buf` of the next pending output byte (replaces the
    /// C `pending_out` pointer).
    pub(crate) pending_out: usize,
    /// Number of bytes in `pending_buf` not yet flushed to the output.
    pub(crate) pending: usize,

    // -- Wrapper configuration -------------------------------------------
    /// Stream framing: 0 = raw DEFLATE, 1 = zlib, 2 = gzip (made negative by
    /// `deflate()` once the trailer has been written).
    pub(crate) wrap: i32,
    /// Optional gzip header supplied by the caller (gzip builds only).
    #[cfg(feature = "gzip")]
    pub(crate) gzhead: Option<crate::gz_header::GzHeader>,
    /// Cursor into the gzip header's extra/name/comment field during emission.
    #[cfg(feature = "gzip")]
    pub(crate) gzindex: usize,
    /// Compression method; always `Z_DEFLATED` (8).
    pub(crate) method: u8,
    /// `flush` argument value of the previous `deflate()` call (init `-2`).
    pub(crate) last_flush: i32,

    // -- Sliding window / hashing ----------------------------------------
    /// LZ77 window size (`1 << w_bits`).
    pub(crate) w_size: usize,
    /// `log2(w_size)`.
    pub(crate) w_bits: u32,
    /// `w_size - 1` (mask for window indices).
    pub(crate) w_mask: usize,
    /// Sliding window, `2 * w_size` bytes (owned). The first half holds the
    /// already-output data used as the dictionary; matches are sought here.
    pub(crate) window: Vec<u8>,
    /// Actual usable window size; set to `2 * w_size` by [`DeflateState::lm_init`].
    pub(crate) window_size: usize,
    /// Hash-chain links, `w_size` entries (owned). `prev[i & w_mask]` is the
    /// previous string with the same hash as the string at position `i`.
    pub(crate) prev: Box<[Pos]>,
    /// Hash heads, `hash_size` entries (owned). `head[h]` is the most recent
    /// string with hash `h`.
    pub(crate) head: Box<[Pos]>,
    /// Current hash value of the string being inserted.
    pub(crate) ins_h: usize,
    /// Number of hash-head slots (`1 << hash_bits`).
    pub(crate) hash_size: usize,
    /// `log2(hash_size)`.
    pub(crate) hash_bits: u32,
    /// `hash_size - 1` (mask for hash values).
    pub(crate) hash_mask: usize,
    /// Number of bits by which `ins_h` must be shifted at each input step so
    /// that, after `MIN_MATCH` steps, the oldest byte no longer influences the
    /// hash: `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`.
    pub(crate) hash_shift: u32,

    // -- LZ77 match state ------------------------------------------------
    /// Window position at the beginning of the current block. May go negative
    /// after a window slide, hence `isize` (C uses `long`).
    pub(crate) block_start: isize,
    /// Length of the best match for the current string.
    pub(crate) match_length: usize,
    /// Previous match (the hash head that started the previous longest-match
    /// search).
    pub(crate) prev_match: Pos,
    /// True if there is a deferred (lazy) match awaiting emission.
    pub(crate) match_available: bool,
    /// Start of the string to be matched in the window.
    pub(crate) strstart: usize,
    /// Window position of the start of the best match found.
    pub(crate) match_start: usize,
    /// Number of valid bytes ahead of `strstart` in the window.
    pub(crate) lookahead: usize,
    /// Length of the best match at the previous step (lazy matching).
    pub(crate) prev_length: usize,
    /// To speed up deflation, hash chains are never searched beyond this many
    /// links. A higher limit improves compression at the cost of speed.
    pub(crate) max_chain_length: usize,
    /// Attempt to find a better match only when the current match is strictly
    /// shorter than this. Also used (under the `max_insert_length` alias in C)
    /// as the upper bound for inserting new strings into the hash table.
    ///
    /// NOTE: C defines `#define max_insert_length max_lazy_match`
    /// (`deflate.h` L186) — there is deliberately **no** separate
    /// `max_insert_length` field; this one serves both roles.
    pub(crate) max_lazy_match: usize,
    /// Compression level (0..=9).
    pub(crate) level: i32,
    /// Favor / force the Huffman coding strategy
    /// (`Z_DEFAULT_STRATEGY`/`FILTERED`/`HUFFMAN_ONLY`/`RLE`/`FIXED`).
    pub(crate) strategy: i32,
    /// Use a faster search when the previous match is at least this long.
    pub(crate) good_match: usize,
    /// Stop searching when the current match is at least this long.
    pub(crate) nice_match: usize,

    // -- Huffman trees ----------------------------------------------------
    /// Literal and length tree.
    pub(crate) dyn_ltree: [HuffmanNode; HEAP_SIZE],
    /// Distance tree.
    pub(crate) dyn_dtree: [HuffmanNode; 2 * D_CODES + 1],
    /// Huffman tree for bit lengths.
    pub(crate) bl_tree: [HuffmanNode; 2 * BL_CODES + 1],
    /// Number of codes at each bit length for the current block.
    pub(crate) bl_count: [u16; MAX_BITS + 1],
    /// Heap used to build the Huffman trees. The sons of `heap[n]` are
    /// `heap[2*n]` and `heap[2*n+1]`; `heap[0]` is unused.
    pub(crate) heap: [i32; 2 * L_CODES + 1],
    /// Number of elements currently in the heap.
    pub(crate) heap_len: usize,
    /// Element of largest frequency (top of the heap).
    pub(crate) heap_max: usize,
    /// Depth of each subtree, used to break ties between subtrees of equal
    /// frequency so that the shallower tree is preferred.
    pub(crate) depth: [u8; 2 * L_CODES + 1],
    /// `max_code` for the literal/length tree (replaces the C `l_desc`
    /// `tree_desc`'s mutable field; the static descriptor lives in trees.rs).
    pub(crate) l_desc_max_code: i32,
    /// `max_code` for the distance tree.
    pub(crate) d_desc_max_code: i32,
    /// `max_code` for the bit-length tree.
    pub(crate) bl_desc_max_code: i32,

    // -- Symbol buffer (overlaid inside pending_buf) ---------------------
    // With `LIT_MEM` undefined, the per-symbol buffer is the region of
    // `pending_buf` that starts at byte offset `lit_bufsize`, with 3 bytes per
    // symbol (low distance byte, high distance byte, literal/length). There is
    // therefore intentionally NO separate `sym_buf` field; the Phase-E
    // `sym_write_raw` / `sym_read` helpers address that region directly.
    /// `1 << (memLevel + 6)`: governs the symbol-buffer capacity and the size
    /// of `pending_buf`.
    pub(crate) lit_bufsize: usize,
    /// Byte offset of the next free symbol slot within the symbol region
    /// (advances by 3 per symbol).
    pub(crate) sym_next: usize,
    /// `(lit_bufsize - 1) * 3`: when `sym_next == sym_end`, the current block
    /// must be flushed.
    pub(crate) sym_end: usize,

    // -- Bit-length accounting + bit accumulator -------------------------
    /// Bit length of the current block with the optimal Huffman trees.
    pub(crate) opt_len: usize,
    /// Bit length of the current block with the static Huffman trees.
    pub(crate) static_len: usize,
    /// Number of string matches in the current block. (Also reused by
    /// `deflate_stored` to track pending hash-slides.)
    pub(crate) matches: usize,
    /// Bytes at the end of the window left to be inserted into the hash table.
    pub(crate) insert: usize,
    /// Output bit accumulator (C `ush`).
    pub(crate) bi_buf: u16,
    /// Number of valid bits in `bi_buf` (all bits below `bi_valid` are valid).
    pub(crate) bi_valid: i32,
    /// Bits used in the last emitted byte; set by `bi_windup` (trees.rs) and
    /// consumed at the FFI boundary (`deflatePending`'s `bits` out-parameter).
    pub(crate) bi_used: i32,
    /// High-water mark: bytes of `window` at or beyond this index have not yet
    /// been written and are lazily zero-initialized before being read.
    pub(crate) high_water: usize,
    /// Set to 1 by [`DeflateState::slide_hash`] and cleared by
    /// [`DeflateState::clear_hash`]; mirrors C `slid`.
    pub(crate) slid: u32,
}

// ===========================================================================
// Phase E — DeflateState methods (pure state mutation; no I/O)
// ===========================================================================

impl DeflateState {
    /// Slide the hash table when sliding the window. Ports `slide_hash`
    /// (`deflate.c` L187-210).
    ///
    /// Every entry in `head` (`hash_size` entries) and then `prev` (`w_size`
    /// entries) is decremented by `w_size`; entries that would underflow are
    /// reset to `NIL`. This keeps the hash chains valid after the window
    /// content has been moved down by `w_size` bytes.
    pub(crate) fn slide_hash(&mut self) {
        // `w_size <= 32768`, so it fits in a `u16` exactly.
        let w = self.w_size as Pos;
        for x in self.head.iter_mut() {
            *x = if *x >= w { *x - w } else { NIL };
        }
        for x in self.prev.iter_mut() {
            *x = if *x >= w { *x - w } else { NIL };
        }
        self.slid = 1;
    }

    /// Clear the hash table. Ports the C `CLEAR_HASH` macro
    /// (`deflate.c` L170-175).
    ///
    /// Setting every head to `NIL` (0) is equivalent to the C sequence of
    /// "set last head to `NIL`, then `zmemzero` the rest".
    pub(crate) fn clear_hash(&mut self) {
        self.head.fill(NIL);
        self.slid = 0;
    }

    /// Update a rolling hash with the next input byte. Ports the C
    /// `UPDATE_HASH` macro (`deflate.c` L141):
    /// `h = ((h << hash_shift) ^ c) & hash_mask`.
    #[inline]
    pub(crate) fn update_hash(&self, h: usize, c: u8) -> usize {
        ((h << self.hash_shift) ^ (c as usize)) & self.hash_mask
    }

    /// Insert the string at window position `str` into the hash table and
    /// return the previous head of its hash chain. Ports the non-`FASTEST`
    /// `INSERT_STRING` macro (`deflate.c` L160-163).
    ///
    /// The ordering is significant and matches C exactly:
    /// `prev[str & w_mask]` first receives the *old* head, and only then does
    /// `head[ins_h]` become `str`.
    pub(crate) fn insert_string(&mut self, str: usize) -> Pos {
        self.ins_h = self.update_hash(self.ins_h, self.window[str + MIN_MATCH - 1]);
        let hash_head = self.head[self.ins_h];
        self.prev[str & self.w_mask] = hash_head;
        self.head[self.ins_h] = str as Pos;
        hash_head
    }

    /// Append one byte to `pending_buf`. Ports the C `put_byte` macro
    /// (`deflate.h`).
    #[inline]
    pub(crate) fn put_byte(&mut self, b: u8) {
        self.pending_buf[self.pending] = b;
        self.pending += 1;
    }

    /// Append a 16-bit value to `pending_buf`, least-significant byte first
    /// (DEFLATE byte order). Ports the C `put_short` macro.
    #[inline]
    pub(crate) fn put_short(&mut self, w: u16) {
        self.put_byte((w & 0xff) as u8);
        self.put_byte((w >> 8) as u8);
    }

    /// Append a 16-bit value to `pending_buf`, most-significant byte first.
    /// Ports `putShortMSB` (`deflate.c` L939-942); used for the zlib header
    /// and the Adler-32 trailer.
    #[inline]
    pub(crate) fn put_short_msb(&mut self, w: u16) {
        self.put_byte((w >> 8) as u8);
        self.put_byte((w & 0xff) as u8);
    }

    /// Write one LZ77 symbol (distance + literal/length) into the symbol
    /// region overlaid in `pending_buf` (the region starting at byte offset
    /// `lit_bufsize`), advancing `sym_next` by 3.
    ///
    /// This is the low-level byte accessor only; the frequency accounting
    /// (`_tr_tally` and friends) lives in trees.rs.
    pub(crate) fn sym_write_raw(&mut self, dist: u16, lc: u8) {
        let base = self.lit_bufsize + self.sym_next;
        self.pending_buf[base] = (dist & 0xff) as u8;
        self.pending_buf[base + 1] = (dist >> 8) as u8;
        self.pending_buf[base + 2] = lc;
        self.sym_next += 3;
    }

    /// Read the LZ77 symbol stored at byte offset `sx` within the symbol
    /// region, returning `(distance, literal_or_length)`. Inverse of
    /// [`DeflateState::sym_write_raw`].
    pub(crate) fn sym_read(&self, sx: usize) -> (u16, u8) {
        let base = self.lit_bufsize + sx;
        let d = (self.pending_buf[base] as u16) | ((self.pending_buf[base + 1] as u16) << 8);
        let lc = self.pending_buf[base + 2];
        (d, lc)
    }
}

// ===========================================================================
// Phase F — lm_init + constructor
// ===========================================================================

impl DeflateState {
    /// Initialize the "longest match" routines for a new compression run.
    /// Ports `lm_init` (`deflate.c` L682-701).
    ///
    /// Pulls the four search-tuning parameters for the current level out of the
    /// `CONFIG_TABLE` (the Rust port of the C `configuration_table`) and resets
    /// all LZ77 match bookkeeping. The per-level compression *function* is
    /// selected separately by the `deflate()` driver, so it is deliberately not
    /// copied here.
    pub(crate) fn lm_init(&mut self) {
        self.window_size = 2 * self.w_size;

        self.clear_hash();

        // Set the default configuration parameters for the current level.
        let cfg = crate::deflate::strategy::CONFIG_TABLE[self.level as usize];
        self.max_lazy_match = cfg.max_lazy as usize;
        self.good_match = cfg.good_length as usize;
        self.nice_match = cfg.nice_length as usize;
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

    /// Allocate the working buffers and initialize every field for a fresh
    /// deflate stream. Mirrors the allocation/sizing block of `deflateInit2_`
    /// (`deflate.c` L440-532).
    ///
    /// This performs allocation and field initialization **only**. Full
    /// parameter validation (legal `windowBits`/`memLevel`/`level`/`strategy`,
    /// the `windowBits == 8 -> 9` promotion, etc.) is the caller's job in
    /// `mod.rs`'s `deflate_init2`. The caller must subsequently run a deflate
    /// reset (which invokes [`DeflateState::lm_init`] and the trees `tr_init`)
    /// before the state is used for compression; this constructor deliberately
    /// does **not** call reset itself.
    ///
    /// Parameters are the post-validation values: `w_bits` and `hash_bits` are
    /// the window/hash log2 sizes, `mem_level` controls `lit_bufsize`, and
    /// `level`/`strategy`/`method`/`wrap` configure the stream.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_allocated(
        w_bits: u32,
        hash_bits: u32,
        mem_level: i32,
        level: i32,
        strategy: i32,
        method: u8,
        wrap: i32,
    ) -> DeflateState {
        let w_size = 1usize << w_bits;
        let w_mask = w_size - 1;

        let hash_size = 1usize << hash_bits;
        let hash_mask = hash_size - 1;
        // C `(hash_bits + MIN_MATCH - 1) / MIN_MATCH` is exactly ceiling
        // division; `div_ceil` produces the byte-identical result.
        let hash_shift = (hash_bits as usize).div_ceil(MIN_MATCH) as u32;

        let lit_bufsize = 1usize << (mem_level + 6);
        let pending_buf_size = lit_bufsize * LIT_BUFS;
        let sym_end = (lit_bufsize - 1) * 3;

        DeflateState {
            // Accounting. `adler` is seeded to the zlib/raw value (1) here; the
            // deflate reset in mod.rs re-seeds it (CRC-32's 0 for gzip) — see
            // the "Adler-32 init subtlety" note at the bottom of this module.
            status: DeflateStatus::Init,
            adler: 1,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
            msg: None,

            // Pending output buffer.
            pending_buf: vec![0u8; pending_buf_size],
            pending_buf_size,
            pending_out: 0,
            pending: 0,

            // Wrapper configuration.
            wrap,
            #[cfg(feature = "gzip")]
            gzhead: None,
            #[cfg(feature = "gzip")]
            gzindex: 0,
            method,
            last_flush: -2,

            // Sliding window / hashing. `window_size` is set by `lm_init`.
            w_size,
            w_bits,
            w_mask,
            window: vec![0u8; 2 * w_size],
            window_size: 0,
            // Element type `Pos` (= u16) is inferred from the `prev`/`head`
            // field types; no cast needed.
            prev: vec![0; w_size].into_boxed_slice(),
            head: vec![0; hash_size].into_boxed_slice(),
            ins_h: 0,
            hash_size,
            hash_bits,
            hash_mask,
            hash_shift,

            // LZ77 match state (configured by `lm_init` during reset).
            block_start: 0,
            match_length: 0,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: 0,
            max_chain_length: 0,
            max_lazy_match: 0,
            level,
            strategy,
            good_match: 0,
            nice_match: 0,

            // Huffman trees (zeroed; populated by trees.rs `tr_init`/`tr_tally`).
            dyn_ltree: core::array::from_fn(|_| HuffmanNode::default()),
            dyn_dtree: core::array::from_fn(|_| HuffmanNode::default()),
            bl_tree: core::array::from_fn(|_| HuffmanNode::default()),
            bl_count: [0u16; MAX_BITS + 1],
            heap: [0i32; 2 * L_CODES + 1],
            heap_len: 0,
            heap_max: 0,
            depth: [0u8; 2 * L_CODES + 1],
            l_desc_max_code: 0,
            d_desc_max_code: 0,
            bl_desc_max_code: 0,

            // Symbol buffer (overlaid in pending_buf).
            lit_bufsize,
            sym_next: 0,
            sym_end,

            // Bit-length accounting + bit accumulator.
            opt_len: 0,
            static_len: 0,
            matches: 0,
            insert: 0,
            bi_buf: 0,
            bi_valid: 0,
            bi_used: 0,
            high_water: 0,
            slid: 0,
        }
    }
}

// ===========================================================================
// Phase H — deflateStateCheck equivalent
// ===========================================================================

impl DeflateState {
    /// Validate that this state is in one of the eight legal header-emission
    /// states. Ports the status portion of `deflateStateCheck`
    /// (`deflate.c` L538-556).
    ///
    /// In C, `deflateStateCheck` also guards against `Z_NULL` streams/states
    /// and mismatched `zalloc`/`zfree` hooks — failure modes that detect
    /// corrupted or uninitialized memory. Ownership and the type system make
    /// every one of those impossible here: holding a live `&DeflateState`
    /// proves the state is present and initialized, and a [`DeflateStatus`] can
    /// only ever hold one of the eight valid sentinels (an out-of-range status
    /// is unrepresentable in safe Rust). The remaining status check is
    /// therefore always satisfied.
    #[inline]
    pub(crate) fn is_valid_status(&self) -> bool {
        // `self.status` is, by construction, one of
        // INIT/GZIP/EXTRA/NAME/COMMENT/HCRC/BUSY/FINISH.
        true
    }
}

// ===========================================================================
// Phase D — DeflateStream<'a> per-call I/O context
//
// The TRANSIENT context for a single `deflate()` call. It pairs the persistent
// `DeflateState` with the current input/output slices, replacing the C
// `next_in`/`avail_in`/`next_out`/`avail_out` quartet. The five strategy
// functions plus `read_buf`, `fill_window`, `flush_pending`, and
// `longest_match` all operate on this type.
// ===========================================================================

/// Per-call deflate I/O context: a borrowed engine state plus the input and
/// output slices and their cursors.
pub(crate) struct DeflateStream<'a> {
    /// The persistent engine state for this stream.
    pub(crate) state: &'a mut DeflateState,
    /// The caller's input buffer.
    pub(crate) input: &'a [u8],
    /// Number of input bytes consumed so far (cursor into `input`).
    pub(crate) in_next: usize,
    /// The caller's output buffer.
    pub(crate) output: &'a mut [u8],
    /// Number of output bytes produced so far (cursor into `output`).
    pub(crate) out_next: usize,
}

impl<'a> DeflateStream<'a> {
    /// Number of input bytes still available to read.
    #[inline]
    pub(crate) fn avail_in(&self) -> usize {
        self.input.len() - self.in_next
    }

    /// Number of output bytes of free space still available to write.
    #[inline]
    pub(crate) fn avail_out(&self) -> usize {
        self.output.len() - self.out_next
    }

    /// Read up to `size` input bytes into the window at `into_window_at`,
    /// updating the running checksum and `total_in`. Ports `read_buf`
    /// (`deflate.c` L219-240) for the window-fill path.
    ///
    /// Returns the number of bytes actually copied (0 when input is exhausted).
    ///
    /// NOTE: the engine seeds `adler` to 1 (zlib/raw) or 0 (gzip CRC) in the
    /// deflate reset — never via `adler32(0, b"")`, which would return 0 rather
    /// than the C init value of 1. This routine only *updates* the running
    /// checksum over the consumed bytes.
    pub(crate) fn read_buf_window(&mut self, into_window_at: usize, size: usize) -> usize {
        let len = self.avail_in().min(size);
        if len == 0 {
            return 0;
        }
        let start = self.in_next;

        // Copy the input into the window.
        self.state.window[into_window_at..into_window_at + len]
            .copy_from_slice(&self.input[start..start + len]);

        // Update the running checksum over exactly the bytes consumed.
        if self.state.wrap == 1 {
            self.state.adler = adler32(self.state.adler, &self.input[start..start + len]);
        } else if self.state.wrap == 2 {
            self.state.adler = crc32(self.state.adler, &self.input[start..start + len]);
        }

        self.in_next += len;
        self.state.total_in += len as u64;
        len
    }

    /// Read up to `size` input bytes directly into the output buffer (the
    /// `deflate_stored` stored-block fast path), updating the running checksum
    /// and `total_in`. Mirrors the C `read_buf(strm, strm->next_out, len)`
    /// call made by `deflate_stored`.
    ///
    /// Returns the number of bytes actually copied. This advances `out_next`
    /// but, matching C exactly, does **not** touch `total_out`: the
    /// `deflate_stored` caller is responsible for `total_out += len`.
    pub(crate) fn read_buf_output(&mut self, size: usize) -> usize {
        let len = self.avail_in().min(size);
        if len == 0 {
            return 0;
        }
        let start = self.in_next;
        let on = self.out_next;

        // Copy the input straight into the output.
        self.output[on..on + len].copy_from_slice(&self.input[start..start + len]);

        // Update the running checksum over exactly the bytes consumed.
        if self.state.wrap == 1 {
            self.state.adler = adler32(self.state.adler, &self.input[start..start + len]);
        } else if self.state.wrap == 2 {
            self.state.adler = crc32(self.state.adler, &self.input[start..start + len]);
        }

        self.in_next += len;
        self.out_next += len;
        self.state.total_in += len as u64;
        // total_out is advanced by the deflate_stored caller (matches C).
        len
    }

    /// Flush as much of `pending_buf` to the output as will fit. Ports
    /// `flush_pending` (`deflate.c` L950-968).
    ///
    /// First flushes any bits still held in the bit accumulator (the trees.rs
    /// `_tr_flush_bits`), then copies `min(pending, avail_out)` bytes from the
    /// pending buffer to the output, advancing all the relevant cursors and
    /// counters. When the pending buffer drains completely, `pending_out` is
    /// rewound to the start.
    pub(crate) fn flush_pending(&mut self) {
        // Flush bits held in the bit accumulator into `pending_buf` first.
        self.state.tr_flush_bits();

        let len = self.state.pending.min(self.avail_out());
        if len == 0 {
            return;
        }

        let po = self.state.pending_out;
        let on = self.out_next;
        self.output[on..on + len].copy_from_slice(&self.state.pending_buf[po..po + len]);

        self.out_next += len;
        self.state.pending_out += len;
        self.state.total_out += len as u64;
        self.state.pending -= len;
        if self.state.pending == 0 {
            self.state.pending_out = 0;
        }
    }

    /// Fill the window when the lookahead drops below `MIN_LOOKAHEAD`. Ports
    /// `fill_window` (`deflate.c` L252-376).
    ///
    /// When `strstart` has advanced far enough that no match can reference the
    /// oldest data, the upper half of the window is slid down to the lower half
    /// (via [`slice::copy_within`]) and the hash tables are slid to match
    /// (`slide_hash`). Fresh input is then read in, newly-available strings are
    /// inserted into the hash table, and finally the freshly-exposed window
    /// tail is lazily zero-initialized up to `WIN_INIT` bytes to keep
    /// `longest_match`'s reads deterministic.
    pub(crate) fn fill_window(&mut self) {
        loop {
            let wsize = self.state.w_size;
            // Amount of free space at the end of the window.
            let more = self.state.window_size - self.state.lookahead - self.state.strstart;

            // If the window is almost full and there is insufficient lookahead,
            // move the upper half to the lower half to make room in the upper.
            let more = if self.state.strstart >= wsize + max_dist(wsize) {
                self.state
                    .window
                    .copy_within(wsize..wsize + (wsize - more), 0);
                self.state.match_start -= wsize;
                self.state.strstart -= wsize;
                self.state.block_start -= wsize as isize;
                if self.state.insert > self.state.strstart {
                    self.state.insert = self.state.strstart;
                }
                self.state.slide_hash();
                more + wsize
            } else {
                more
            };

            if self.avail_in() == 0 {
                break;
            }

            // Read into the window past the current lookahead.
            let at = self.state.strstart + self.state.lookahead;
            let n = self.read_buf_window(at, more);
            self.state.lookahead += n;

            // Initialize the hash value now that we have at least `MIN_MATCH`
            // bytes available, and insert any strings that became complete.
            if self.state.lookahead + self.state.insert >= MIN_MATCH {
                let mut str = self.state.strstart - self.state.insert;
                self.state.ins_h = self.state.window[str] as usize;
                let c = self.state.window[str + 1];
                self.state.ins_h = self.state.update_hash(self.state.ins_h, c);
                while self.state.insert != 0 {
                    let c = self.state.window[str + MIN_MATCH - 1];
                    self.state.ins_h = self.state.update_hash(self.state.ins_h, c);
                    let h = self.state.ins_h;
                    let head = self.state.head[h];
                    let wm = str & self.state.w_mask;
                    self.state.prev[wm] = head;
                    self.state.head[h] = str as Pos;
                    str += 1;
                    self.state.insert -= 1;
                    if self.state.lookahead + self.state.insert < MIN_MATCH {
                        break;
                    }
                }
            }

            // `do { ... } while (lookahead < MIN_LOOKAHEAD && avail_in != 0)`.
            if !(self.state.lookahead < MIN_LOOKAHEAD && self.avail_in() != 0) {
                break;
            }
        }

        // Lazily zero-initialize the bytes of the window past the current data,
        // up to `WIN_INIT`, so that `longest_match` reads only defined bytes.
        // Ports `deflate.c` L347-372.
        if self.state.high_water < self.state.window_size {
            let curr = self.state.strstart + self.state.lookahead;
            if self.state.high_water < curr {
                // Previous high water mark below current data: zero `WIN_INIT`
                // bytes (or up to the end of the window).
                let mut init = self.state.window_size - curr;
                if init > WIN_INIT {
                    init = WIN_INIT;
                }
                self.state.window[curr..curr + init].fill(0);
                self.state.high_water = curr + init;
            } else if self.state.high_water < curr + WIN_INIT {
                // High water mark at or above current data, but below the
                // `WIN_INIT` boundary: zero the gap up to that boundary (or up
                // to the end of the window).
                let hw = self.state.high_water;
                let mut init = curr + WIN_INIT - hw;
                if init > self.state.window_size - hw {
                    init = self.state.window_size - hw;
                }
                self.state.window[hw..hw + init].fill(0);
                self.state.high_water = hw + init;
            }
        }
    }

    /// Find the longest match for the string at `strstart`, searching the hash
    /// chain that begins at window position `cur_match`. Ports the standard
    /// portable (non-`FASTEST`, non-`UNALIGNED_OK`) branch of `longest_match`
    /// (`deflate.c` L1480-1530), updating `self.state.match_start` and
    /// returning the length of the longest match (never beyond `lookahead`).
    ///
    /// # Byte-identical correctness
    ///
    /// The C source unrolls the inner comparison eight bytes at a time purely
    /// as a speed optimization; a plain capped common-prefix loop computes the
    /// *identical* `len`, because the scan terminates exactly at
    /// `strstart + MAX_MATCH`. The match-acceptance order, the `chain_length`
    /// quartering when the previous match already exceeds `good_match`, and the
    /// clamp of `nice_match`/the result to `lookahead` are all preserved
    /// exactly — each of these can change *which* match is chosen and therefore
    /// the emitted bytes.
    pub(crate) fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let s = &mut *self.state;

        let mut chain_length = s.max_chain_length; // max hash chain length
        let strstart = s.strstart;
        let mut best_len = s.prev_length; // best match length so far
        let mut nice_match = s.nice_match; // stop if match long enough
        let w_mask = s.w_mask;

        // Stop when `cur_match` becomes <= `limit`. To simplify the code we
        // prevent matches with the string of window index 0 (in particular we
        // avoid a match of the string with itself at the start of the input).
        let limit = if strstart > max_dist(s.w_size) {
            strstart - max_dist(s.w_size)
        } else {
            NIL as usize
        };

        // The code is optimized for HASH_BITS >= 8 and MAX_MATCH-2 multiple of
        // 16. It is easy to get rid of this optimization if necessary.
        let mut scan_end1 = s.window[strstart + best_len - 1];
        let mut scan_end = s.window[strstart + best_len];

        // Do not waste too much time if we already have a good match.
        if s.prev_length >= s.good_match {
            chain_length >>= 2;
        }

        // Do not look for matches beyond the end of the input. This is
        // necessary to make deflate deterministic.
        if nice_match > s.lookahead {
            nice_match = s.lookahead;
        }

        loop {
            let m = cur_match;

            // Skip to the next chain entry unless this candidate could improve
            // on the best match so far. The checks are ordered (cheapest /
            // most-discriminating first) exactly as in C.
            if s.window[m + best_len] == scan_end
                && s.window[m + best_len - 1] == scan_end1
                && s.window[m] == s.window[strstart]
                && s.window[m + 1] == s.window[strstart + 1]
            {
                // Compute the length of the common prefix, capped at MAX_MATCH.
                // This is byte-for-byte equivalent to the C unrolled scan, which
                // stops at `strend = scan + MAX_MATCH`.
                let mut len = 0usize;
                while len < MAX_MATCH && s.window[strstart + len] == s.window[m + len] {
                    len += 1;
                }

                if len > best_len {
                    s.match_start = m;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = s.window[strstart + best_len - 1];
                    scan_end = s.window[strstart + best_len];
                }
            }

            // Advance to the previous string in the same hash chain.
            cur_match = s.prev[cur_match & w_mask] as usize;
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }

        if best_len <= s.lookahead {
            best_len
        } else {
            s.lookahead
        }
    }
}

// ===========================================================================
// Phase G — Trait impls
// ===========================================================================

impl StreamState for DeflateState {
    #[inline]
    fn kind(&self) -> StreamKind {
        StreamKind::Deflate
    }

    /// Reset the engine to its post-initialization condition, retaining the
    /// already-allocated buffers. This is the engine-internal portion of
    /// `deflateReset` (`deflate.c`): it performs `deflateResetKeep`'s field
    /// resets and then re-runs `tr_init` (trees.rs) and [`DeflateState::lm_init`].
    ///
    /// The public stream-level accounting on `ZStream` (its own `total_*` /
    /// message) is reset separately by `ZStream::reset`; here we reset the
    /// engine's working copies so the two stay consistent.
    fn reset(&mut self) {
        // deflateResetKeep: per-stream accounting and header-emission state.
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = Z_UNKNOWN;
        self.pending = 0;
        self.pending_out = 0;

        // `deflate()` negates `wrap` once the trailer has been written; undo
        // that so the next run re-emits the wrapper.
        if self.wrap < 0 {
            self.wrap = -self.wrap;
        }

        // Header-emission start state: GZIP_STATE for gzip, otherwise INIT_STATE.
        #[cfg(feature = "gzip")]
        {
            self.status = if self.wrap == 2 {
                DeflateStatus::Gzip
            } else {
                DeflateStatus::Init
            };
        }
        #[cfg(not(feature = "gzip"))]
        {
            self.status = DeflateStatus::Init;
        }

        // Adler-32 init subtlety (byte-exact): seed the running checksum to the
        // C init value directly — 1 for the zlib/raw Adler-32, 0 for the gzip
        // CRC-32. We must NOT compute this via `adler32(0, b"")`, because the
        // checksum port returns 0 (not 1) for an empty input slice.
        self.adler = if self.wrap == 2 { 0 } else { 1 };

        self.last_flush = -2;

        // _tr_init(s): reset the Huffman trees and the bit accumulator.
        self.tr_init();
        // lm_init(s): reset the LZ77 match state and window bookkeeping.
        self.lm_init();
    }
}

impl Drop for DeflateState {
    /// RAII teardown — the safe-Rust replacement for `deflateEnd`'s buffer free
    /// (AAP §0.6.3).
    ///
    /// Intentionally empty: every buffer (`window`, `pending_buf`, `prev`,
    /// `head`) is an owned [`Vec`]/[`Box`] that frees itself when the state is
    /// dropped. Ownership makes the teardown deterministic and leak-free, and
    /// double-free / use-after-free are unrepresentable.
    fn drop(&mut self) {}
}

// ===========================================================================
// Accounting sync
//
// `DeflateState` persistently holds `adler` / `total_in` / `total_out` /
// `data_type` / `msg`. The idiomatic `Deflate` wrapper (mod.rs) and the FFI
// shim (ffi.rs) mirror these to/from `ZStream` / `z_stream` after each call.
// The transient `DeflateStream` context holds only the per-call I/O cursors.
// Concretely: `read_buf_window` / `read_buf_output` bump `state.total_in` (and
// the running checksum), `flush_pending` bumps `state.total_out`, and the
// strategy / trees code reads and writes `state.data_type`.
// ===========================================================================
