//! Persistent DEFLATE engine state and the LZ77 plumbing that drives it.
//!
//! This module is the foundation of the compression engine. It ports the
//! `deflate_state` structure and the associated constants/macros from
//! `deflate.h`, together with the low-level LZ77 routines of `deflate.c`
//! (`slide_hash`, `read_buf`, `fill_window`, `longest_match`, `lm_init`,
//! `flush_pending`, `put_byte`, `putShortMSB`, and the `deflateStateCheck`
//! status guard). Every other file in `crate::deflate` builds on the types and
//! helpers defined here.
//!
//! # Two-layer state design
//!
//! The C implementation keeps both the long-lived compression state *and* the
//! per-call input/output cursors on a single `z_stream`/`deflate_state` pair
//! reached through raw pointers. This port splits the two concerns so the safe
//! core never needs raw pointers:
//!
//! * [`DeflateState`] — the **persistent** engine state, owned across every
//!   `deflate()` call. It owns its working buffers (`Vec<u8>` / `Box<[Pos]>`),
//!   so teardown is automatic via RAII (replacing `deflateEnd`'s manual frees,
//!   AAP §0.6.3) and double-free / use-after-free are unrepresentable.
//! * [`DeflateStream`] — the **transient** per-call context that pairs a
//!   `&mut DeflateState` with the current input/output slices and cursor
//!   indices, replacing the C `next_in`/`avail_in`/`next_out`/`avail_out`
//!   fields for the duration of one call. The five strategy functions and the
//!   `read_buf`/`fill_window`/`flush_pending`/`longest_match` helpers operate on
//!   this context, which is what lets them remain 100% safe Rust.
//!
//! # Byte-identical output
//!
//! Every constant, sizing formula, and hash/window/match computation here
//! mirrors C zlib exactly so the compressed bitstream is byte-for-byte
//! identical (AAP §0.6.6, §0.6.7, §0.7.1). The algorithms are ported
//! mechanically and faithfully — the arithmetic (shifts, masks, wrap-around
//! widths, comparison order) is preserved precisely.
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; `unsafe`
//!   lives only in `crate::ffi` and `crate::inflate::fast` (AAP §0.6.2). This
//!   file contains zero `unsafe`, raw pointers, `transmute`, or `MaybeUninit`.
//! * **`no_std`-clean.** Only `core` and `alloc` are referenced, never `std`.
//! * **Global allocator only.** [`DeflateState`] is a concrete type (not generic
//!   over an allocator); buffers use the global allocator. Custom C
//!   `zalloc`/`zfree` hooks are bridged at the `crate::ffi` boundary, not here.

// Heap container types backing the owned buffers. Under the default `std` build
// these come from the prelude; under a `no-std` build they come from the
// `alloc` crate, which the crate root brings into scope with
// `extern crate alloc;`. Gating the import (rather than importing
// unconditionally) mirrors the sibling `crate::stream` module and avoids a
// duplicate import with the `std` prelude.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use core::any::Any;

// Checksum entry points. `read_buf_window`/`read_buf_output` update the running
// checksum exactly as C `read_buf` does: Adler-32 for the zlib wrapper
// (`wrap == 1`) and CRC-32 for the gzip wrapper (`wrap == 2`). Only these two
// functions are needed here; the `*_z` length-form variants are unused.
use crate::checksum::{adler32, crc32};
// `DeflateStatus` is the header-emission state machine, defined in the deflate
// module root (`mod.rs`); `HuffmanNode` is the Huffman tree node defined in
// `trees.rs`. Both participate in the intentional intra-crate module cycle
// (state ↔ mod, state ↔ trees), which is fine because the crate compiles as a
// single unit.
use crate::deflate::DeflateStatus;
use crate::deflate::trees::HuffmanNode;
use crate::stream::{StreamKind, StreamState};

// ===========================================================================
// Phase A — Deflate-internal constants (deflate.h + zutil.h/zlib.h)
// ===========================================================================
//
// Reproduced verbatim from the C sources; these values are frozen because they
// determine the on-the-wire format and the internal table sizing. Several are
// consumed by sibling modules (`trees`, `strategy`, `stored`, `mod`) rather
// than within this file, so they are `pub(crate)`.

/// Number of length codes, not counting the special `END_BLOCK` code
/// (`deflate.h` `LENGTH_CODES`).
pub(crate) const LENGTH_CODES: usize = 29;

/// Number of literal bytes `0..=255` (`deflate.h` `LITERALS`).
pub(crate) const LITERALS: usize = 256;

/// Number of literal or length codes, including the `END_BLOCK` code
/// (`deflate.h` `L_CODES` = `LITERALS + 1 + LENGTH_CODES` = 286).
pub(crate) const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;

/// Number of distance codes (`deflate.h` `D_CODES`).
pub(crate) const D_CODES: usize = 30;

/// Number of codes used to transfer the bit lengths (`deflate.h` `BL_CODES`).
pub(crate) const BL_CODES: usize = 19;

/// Maximum heap size (`deflate.h` `HEAP_SIZE` = `2 * L_CODES + 1` = 573).
pub(crate) const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// All codes must not exceed this many bits (`deflate.h` `MAX_BITS`).
pub(crate) const MAX_BITS: usize = 15;

/// Maximum number of bits in a bit-length code (`trees.c` `MAX_BL_BITS`).
pub(crate) const MAX_BL_BITS: usize = 7;

/// Size, in bits, of the `bi_buf` bit buffer (`deflate.h` `Buf_size`).
pub(crate) const BUF_SIZE: i32 = 16;

/// Minimum match length (`zutil.h` `MIN_MATCH`).
pub(crate) const MIN_MATCH: usize = 3;

/// Maximum match length (`zutil.h` `MAX_MATCH`).
pub(crate) const MAX_MATCH: usize = 258;

/// Minimum lookahead except at the end of input (`deflate.h` `MIN_LOOKAHEAD`
/// = `MAX_MATCH + MIN_MATCH + 1` = 262).
pub(crate) const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// Number of bytes past the end of data in the window to initialize so the
/// longest-match routines never read uninitialized memory (`deflate.h`
/// `WIN_INIT` = `MAX_MATCH` = 258).
pub(crate) const WIN_INIT: usize = MAX_MATCH;

/// Tail-of-hash-chain sentinel (`deflate.c` `NIL`).
pub(crate) const NIL: u16 = 0;

/// Matches of length `MIN_MATCH` are discarded if their distance exceeds this
/// value (`deflate.c` `TOO_FAR`).
pub(crate) const TOO_FAR: usize = 4096;

/// End-of-block code value (`trees.c` `END_BLOCK`).
pub(crate) const END_BLOCK: usize = 256;

/// Number of bytes that `pending_buf` reserves per symbol slot: `sym_buf` is
/// overlaid one quarter of the way into `pending_buf`, so `pending_buf` is
/// `lit_bufsize * LIT_BUFS` bytes. `LIT_MEM` is *not* defined (it is commented
/// out in `deflate.h`), so `LIT_BUFS` is 4 and each symbol occupies 3 bytes.
pub(crate) const LIT_BUFS: usize = 4;

/// Largest stored-block payload (`deflate.c` stored-block limit, 64 KiB − 1).
pub(crate) const MAX_STORED: usize = 65535;

/// `FLG.FDICT` bit indicating a preset dictionary in the zlib header
/// (`deflate.c` `PRESET_DICT`).
pub(crate) const PRESET_DICT: i32 = 0x20;

/// The DEFLATE compression method (`zlib.h` `Z_DEFLATED`); the only method the
/// engine emits.
pub(crate) const Z_DEFLATED: i32 = 8;

/// A position in the sliding window. `u16` (rather than a wider integer) keeps
/// the `prev`/`head` tables compact, matching the C `Pos` typedef; window
/// indices never exceed 32 KiB, so they always fit.
pub(crate) type Pos = u16;

/// Maximum match distance (`deflate.h` `MAX_DIST(s)` = `w_size − MIN_LOOKAHEAD`).
///
/// Match distances are limited to this instead of `w_size` to simplify the
/// sliding-window bookkeeping. `w_size` is always at least 512 (`windowBits`
/// is forced to ≥ 9), so the subtraction never underflows.
#[inline]
pub(crate) const fn max_dist(w_size: usize) -> usize {
    w_size - MIN_LOOKAHEAD
}

// ===========================================================================
// Phase B — BlockState (deflate.c `block_state`, L64-68)
// ===========================================================================

/// Result of a single call to one of the five `deflate_*` strategy functions,
/// describing how far the block-level work progressed. Mirrors the C
/// `block_state` enum exactly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BlockState {
    /// Ran out of input or output before reaching the end of the block
    /// (`need_more`).
    NeedMore,
    /// A block flush was performed (`block_done`).
    BlockDone,
    /// Finish started; only more output is needed at the next `deflate` call
    /// (`finish_started`).
    FinishStarted,
    /// Finish complete; no more input or output will be accepted
    /// (`finish_done`).
    FinishDone,
}

// ===========================================================================
// Phase C — DeflateState (deflate.h `deflate_state`, L48-280)
// ===========================================================================

/// The persistent DEFLATE engine state, owned across every `deflate()` call.
///
/// This is the Rust port of the C `deflate_state` structure. It is a concrete
/// type allocated on the heap (held as `Box<DeflateState>` inside the public
/// stream wrapper); all working buffers are owned, so [`Drop`] reclaims them
/// automatically — there is no `deflateEnd` free path and no possibility of a
/// double free or leak (AAP §0.6.3).
///
/// All fields are `pub(crate)` because the sibling deflate modules
/// (`trees`, `strategy`, `fast`, `slow`, `stored`, `huff`, `mod`) drive the
/// engine by borrowing `&mut DeflateState` and mutating these fields directly,
/// exactly as the C code mutates `deflate_state` members.
///
/// `Clone` implements the C `deflateCopy` operation as a deep copy. Because the
/// raw tree-descriptor pointers were replaced by plain `max_code` integers (and
/// the static descriptors live as `const` data in `trees.rs`), there is nothing
/// to rewire after copying, so a derived `Clone` is correct.
#[derive(Clone)]
pub(crate) struct DeflateState {
    // --- Accounting -------------------------------------------------------
    // The C code keeps these on `z_stream`; this port keeps them on the engine
    // and the wrapper / FFI boundary mirrors them to/from `ZStream`/`z_stream`
    // after each call (see the "Accounting sync" note on the I/O helpers).
    /// Header-emission state machine (INIT/GZIP/EXTRA/NAME/COMMENT/HCRC/BUSY/
    /// FINISH).
    pub(crate) status: DeflateStatus,
    /// Running checksum: Adler-32 for the zlib wrapper, CRC-32 for gzip.
    pub(crate) adler: u32,
    /// Total bytes read so far.
    pub(crate) total_in: u64,
    /// Total bytes written so far.
    pub(crate) total_out: u64,
    /// Best guess of the data type: `Z_BINARY`(0) / `Z_TEXT`(1) /
    /// `Z_UNKNOWN`(2). Initialized to `Z_UNKNOWN`.
    pub(crate) data_type: i32,
    /// Last error message, or `None`.
    pub(crate) msg: Option<&'static str>,

    // --- Pending output buffer -------------------------------------------
    /// Output still pending: also holds the overlaid symbol buffer (`sym_buf`)
    /// in its upper three quarters. Size is `lit_bufsize * LIT_BUFS`.
    pub(crate) pending_buf: Vec<u8>,
    /// Size of `pending_buf` in bytes (`lit_bufsize * LIT_BUFS`).
    pub(crate) pending_buf_size: usize,
    /// Index into `pending_buf` of the next byte to output (replaces the C
    /// `pending_out` pointer).
    pub(crate) pending_out: usize,
    /// Number of bytes in `pending_buf` not yet flushed to the output.
    pub(crate) pending: usize,

    // --- Wrapper configuration -------------------------------------------
    /// Wrapper selector: 0 = raw, 1 = zlib, 2 = gzip. May be driven negative by
    /// `deflate()` once the trailer has been written.
    pub(crate) wrap: i32,
    /// Optional gzip header supplied via `deflateSetHeader`.
    #[cfg(feature = "gzip")]
    pub(crate) gzhead: Option<crate::gz_header::GzHeader>,
    /// Cursor into the gzip header's extra/name/comment fields while the header
    /// is being emitted.
    pub(crate) gzindex: usize,
    /// Compression method; always `Z_DEFLATED`.
    pub(crate) method: u8,
    /// `flush` value passed to the previous `deflate()` call (init `-2`).
    pub(crate) last_flush: i32,

    // --- Sliding window and hashing --------------------------------------
    /// LZ77 window size (`1 << w_bits`).
    pub(crate) w_size: usize,
    /// `log2(w_size)` (9..=15).
    pub(crate) w_bits: u32,
    /// `w_size - 1`, used to wrap window indices.
    pub(crate) w_mask: usize,
    /// The sliding window: holds up to `2 * w_size` bytes, with the previous
    /// `w_size` bytes (the match dictionary) followed by the current `w_size`.
    pub(crate) window: Vec<u8>,
    /// Actual usable size of `window` (set to `2 * w_size` by `lm_init`).
    pub(crate) window_size: usize,
    /// Hash-chain link table: `prev[i]` is the previous position in the chain
    /// that hashed to the same value as position `i`. Size `w_size`.
    pub(crate) prev: Box<[Pos]>,
    /// Hash-head table: `head[h]` is the most recent position whose 3-byte
    /// prefix hashed to `h`. Size `hash_size`.
    pub(crate) head: Box<[Pos]>,
    /// Current rolling hash value.
    pub(crate) ins_h: usize,
    /// Number of hash-head buckets (`1 << hash_bits`).
    pub(crate) hash_size: usize,
    /// `log2(hash_size)` (`memLevel + 7`).
    pub(crate) hash_bits: u32,
    /// `hash_size - 1`, used to mask hash values.
    pub(crate) hash_mask: usize,
    /// Number of bits by which `ins_h` is shifted per input byte;
    /// `(hash_bits + MIN_MATCH - 1) / MIN_MATCH`.
    pub(crate) hash_shift: u32,

    // --- LZ77 match state -------------------------------------------------
    /// Window position of the start of the current block. Can briefly go
    /// negative when the window slides, so it is signed (C uses `long`).
    pub(crate) block_start: isize,
    /// Length of the best match for the current string.
    pub(crate) match_length: usize,
    /// Previous match (the deferred match start during lazy matching).
    pub(crate) prev_match: Pos,
    /// True when a previous match is being held back for lazy evaluation.
    pub(crate) match_available: bool,
    /// Start of the string currently being scanned in `window`.
    pub(crate) strstart: usize,
    /// Start of the best match found by `longest_match`.
    pub(crate) match_start: usize,
    /// Number of valid bytes ahead of `strstart` in `window`.
    pub(crate) lookahead: usize,
    /// Length of the best match at the previous step (lazy matching).
    pub(crate) prev_length: usize,
    /// Maximum number of hash-chain links to follow when searching for a match.
    pub(crate) max_chain_length: usize,
    /// Stop searching when the current match exceeds this length. Also serves
    /// as C's `max_insert_length` (which is `#define`d to `max_lazy_match`), so
    /// no separate field is kept.
    pub(crate) max_lazy_match: usize,
    /// Compression level (0..=9).
    pub(crate) level: i32,
    /// Compression strategy (`Z_DEFAULT_STRATEGY`/`Z_FILTERED`/…).
    pub(crate) strategy: i32,
    /// Reduce the chain length to a quarter once a match this long is found.
    pub(crate) good_match: usize,
    /// Stop the search as soon as a match this long is found.
    pub(crate) nice_match: usize,

    // --- Huffman trees ----------------------------------------------------
    // The C `tree_desc` structs hold raw pointers to the dynamic tree and the
    // static descriptor plus a `max_code`. The raw pointers are removed: the
    // dynamic trees are the array fields below, the static descriptors live as
    // `const` data in `trees.rs` selected by a `TreeKind`, and only the mutable
    // `max_code` per build is kept (the three `*_desc_max_code` fields).
    /// Dynamic literal/length tree.
    pub(crate) dyn_ltree: [HuffmanNode; HEAP_SIZE],
    /// Dynamic distance tree.
    pub(crate) dyn_dtree: [HuffmanNode; 2 * D_CODES + 1],
    /// Huffman tree for the bit lengths.
    pub(crate) bl_tree: [HuffmanNode; 2 * BL_CODES + 1],
    /// Number of codes at each bit length for the current tree.
    pub(crate) bl_count: [u16; MAX_BITS + 1],
    /// Heap used to build the Huffman trees.
    pub(crate) heap: [i32; 2 * L_CODES + 1],
    /// Number of elements currently in the heap.
    pub(crate) heap_len: usize,
    /// Element of largest frequency; top of the heap.
    pub(crate) heap_max: usize,
    /// Depth of each subtree, used to break ties for equal-frequency nodes.
    pub(crate) depth: [u8; 2 * L_CODES + 1],
    /// `max_code` for the literal/length tree (was `l_desc.max_code`).
    pub(crate) l_desc_max_code: i32,
    /// `max_code` for the distance tree (was `d_desc.max_code`).
    pub(crate) d_desc_max_code: i32,
    /// `max_code` for the bit-length tree (was `bl_desc.max_code`).
    pub(crate) bl_desc_max_code: i32,

    // --- Symbol buffer (overlaid in pending_buf) -------------------------
    // `sym_buf` is not a separate allocation; it is the region of `pending_buf`
    // starting at offset `lit_bufsize`. The `sym_*` accessor methods read and
    // write 3 bytes per symbol there.
    /// Size of the literal buffer quarter (`1 << (memLevel + 6)`).
    pub(crate) lit_bufsize: usize,
    /// Byte offset, within the overlaid symbol region, of the next symbol slot
    /// to write; advances by 3 per symbol.
    pub(crate) sym_next: usize,
    /// Symbol-region offset at which the block must be flushed
    /// (`(lit_bufsize - 1) * 3`).
    pub(crate) sym_end: usize,

    // --- Bit-length accounting and bit accumulator -----------------------
    /// Bit length of the current block with the optimal (dynamic) trees.
    pub(crate) opt_len: usize,
    /// Bit length of the current block with the static trees.
    pub(crate) static_len: usize,
    /// Number of string matches in the current block. Also reused by
    /// `deflate_stored` to count pending hash slides.
    pub(crate) matches: usize,
    /// Bytes at the end of the window that still need inserting into the hash.
    pub(crate) insert: usize,
    /// Bit-accumulator output buffer (C `ush`).
    pub(crate) bi_buf: u16,
    /// Number of valid bits in `bi_buf`.
    pub(crate) bi_valid: i32,
    /// Bits used in the last emitted byte; set by `bi_windup`, consumed at the
    /// FFI boundary (`deflatePending`).
    pub(crate) bi_used: i32,
    /// High-water mark in `window` for lazy zero-initialization.
    pub(crate) high_water: usize,
    /// Set by `slide_hash` to record that the window has slid; cleared by
    /// `clear_hash` (C `slid`).
    pub(crate) slid: u32,
}

// ===========================================================================
// Phase F — Constructor (deflate.c 440-532 allocation/sizing)
// ===========================================================================

impl DeflateState {
    /// Allocate the working buffers and initialize every field, mirroring the
    /// allocation and sizing block of C `deflateInit2_` (deflate.c 440-532).
    ///
    /// Only buffer allocation and field initialization happen here. Full
    /// parameter validation (legal `windowBits`, `memLevel`, `level`,
    /// `strategy`, `method`, and the `Z_DEFAULT_COMPRESSION` → 6 resolution)
    /// is performed by `mod.rs`'s `deflate_init2` *before* this is called, so
    /// `level` is already in `0..=9`. The caller (`mod.rs`) subsequently runs
    /// the equivalent of `deflateReset` — `lm_init` plus `trees::tr_init` — to
    /// finish bringing the engine to its initial state.
    ///
    /// # Checksum initialization (byte-exact, see the module-level note below)
    ///
    /// The zlib/raw checksum seed is the literal `1`, and the gzip CRC seed is
    /// `0`. The seed is *not* obtained by calling `adler32(0, &[])`: the sibling
    /// `crate::checksum::adler32` returns the recombined seed `0` for an empty
    /// slice, whereas C's `adler32(0, Z_NULL, 0)` yields `1`. Using the literal
    /// keeps the emitted Adler-32 trailer byte-identical to C zlib.
    pub(crate) fn new_allocated(
        w_bits: u32,
        hash_bits: u32,
        mem_level: u32,
        level: i32,
        strategy: i32,
        method: u8,
        wrap: i32,
    ) -> DeflateState {
        // Window geometry: w_size is a power of two, w_mask wraps indices.
        let w_size = 1usize << w_bits;
        let w_mask = w_size - 1;

        // Hash geometry: hash_bits is `memLevel + 7` (computed in mod.rs and
        // passed in); hash_shift spreads each input byte across the hash so a
        // 3-byte string maps to a single bucket after MIN_MATCH updates.
        let hash_size = 1usize << hash_bits;
        let hash_mask = hash_size - 1;
        let hash_shift = (hash_bits as usize).div_ceil(MIN_MATCH) as u32;

        // Owned working buffers. `window` is 2*w_size bytes; `prev` indexes by
        // `pos & w_mask` (w_size entries); `head` has one entry per hash bucket.
        // `prev`'s never-referenced entries are harmless when initialized to
        // NIL, and `head` is cleared again by `lm_init`'s `clear_hash`, so a
        // zero fill here cannot perturb the compressed output.
        let window = vec![0u8; 2 * w_size];
        let prev = vec![NIL; w_size].into_boxed_slice();
        let head = vec![NIL; hash_size].into_boxed_slice();

        // `pending_buf` overlays the literal/symbol buffer: it is
        // `lit_bufsize * LIT_BUFS` bytes, the symbol region starts at
        // `lit_bufsize`, and a full block is `(lit_bufsize - 1)` symbols.
        let lit_bufsize = 1usize << (mem_level + 6);
        let pending_buf = vec![0u8; lit_bufsize * LIT_BUFS];
        let pending_buf_size = lit_bufsize * LIT_BUFS;
        let sym_end = (lit_bufsize - 1) * 3;

        DeflateState {
            // Accounting.
            status: DeflateStatus::Init,
            adler: if wrap == 2 { 0 } else { 1 },
            total_in: 0,
            total_out: 0,
            data_type: crate::constants::Z_UNKNOWN,
            msg: None,

            // Pending output buffer.
            pending_buf,
            pending_buf_size,
            pending_out: 0,
            pending: 0,

            // Wrapper configuration.
            wrap,
            #[cfg(feature = "gzip")]
            gzhead: None,
            gzindex: 0,
            method,
            last_flush: -2,

            // Sliding window and hashing.
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

            // LZ77 match state (the configuration-derived match parameters are
            // filled in by `lm_init`).
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: NIL,
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

            // Huffman trees (zeroed; populated by `trees::tr_init` and the
            // per-block tree build).
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

            // Symbol buffer.
            lit_bufsize,
            sym_next: 0,
            sym_end,

            // Bit-length accounting and bit accumulator.
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

    // =======================================================================
    // Phase F — lm_init (deflate.c 682-701)
    // =======================================================================

    /// Reset the match state and load the per-level configuration, mirroring C
    /// `lm_init`. Run as part of the `deflateReset` path after construction and
    /// on every explicit reset.
    pub(crate) fn lm_init(&mut self) {
        self.window_size = 2 * self.w_size;

        self.clear_hash();

        // Load the four sizing parameters for this level. The function pointer
        // in the C `configuration_table` is not copied; strategy dispatch is
        // performed by `mod.rs` from the level/strategy directly.
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

    // =======================================================================
    // Phase E — Pure state-mutation helpers (no I/O)
    // =======================================================================

    /// Slide the hash tables by `w_size` when the window slides, mirroring C
    /// `slide_hash` (deflate.c 187-210). Every entry `m` becomes `m - w_size`
    /// if it is still in range, or `NIL` otherwise. `prev` entries that are not
    /// on any live chain become garbage, but their values are never read.
    pub(crate) fn slide_hash(&mut self) {
        // `w_size` ≤ 32768, so it fits in a `Pos` (u16); doing the comparison
        // and subtraction in `Pos` matches the C `ush`/`Pos` arithmetic.
        let w = self.w_size as Pos;
        for entry in self.head.iter_mut() {
            let m = *entry;
            *entry = if m >= w { m - w } else { NIL };
        }
        for entry in self.prev.iter_mut() {
            let m = *entry;
            *entry = if m >= w { m - w } else { NIL };
        }
        self.slid = 1;
    }

    /// Clear all hash heads, mirroring C `CLEAR_HASH` (deflate.c 170-174), and
    /// reset the `slid` flag.
    pub(crate) fn clear_hash(&mut self) {
        self.head.fill(NIL);
        self.slid = 0;
    }

    /// Roll the hash with the next byte `c`, mirroring C `UPDATE_HASH`. After
    /// `MIN_MATCH` updates the hash depends on the whole `MIN_MATCH`-byte
    /// string.
    #[inline]
    pub(crate) fn update_hash(&self, h: usize, c: u8) -> usize {
        ((h << self.hash_shift) ^ (c as usize)) & self.hash_mask
    }

    /// Insert the string starting at window position `str` into the hash,
    /// mirroring C `INSERT_STRING` (non-FASTEST). Returns the previous head of
    /// the chain (the most recent earlier occurrence of the same hash), which
    /// becomes the first candidate for `longest_match`.
    ///
    /// The assignment order matches C exactly: `prev[str & w_mask]` receives the
    /// *old* head, and only then does `head[ins_h]` become `str`.
    pub(crate) fn insert_string(&mut self, str: usize) -> Pos {
        self.ins_h = self.update_hash(self.ins_h, self.window[str + MIN_MATCH - 1]);
        let hash_head = self.head[self.ins_h];
        self.prev[str & self.w_mask] = hash_head;
        self.head[self.ins_h] = str as Pos;
        hash_head
    }

    /// Append one byte to `pending_buf`, mirroring C `put_byte`.
    #[inline]
    pub(crate) fn put_byte(&mut self, b: u8) {
        self.pending_buf[self.pending] = b;
        self.pending += 1;
    }

    /// Append a 16-bit value to `pending_buf`, least-significant byte first
    /// (mirrors C `put_short`). Used for stored-block lengths and the gzip
    /// trailer.
    #[inline]
    pub(crate) fn put_short(&mut self, w: u16) {
        self.put_byte((w & 0xff) as u8);
        self.put_byte((w >> 8) as u8);
    }

    /// Append a 16-bit value to `pending_buf`, most-significant byte first,
    /// mirroring C `putShortMSB` (deflate.c 939-942). Used for the zlib header
    /// and the big-endian Adler-32 trailer.
    #[inline]
    pub(crate) fn put_short_msb(&mut self, w: u16) {
        self.put_byte((w >> 8) as u8);
        self.put_byte((w & 0xff) as u8);
    }

    /// Write one LZ77 symbol into the overlaid symbol buffer (three bytes:
    /// distance low, distance high, length code), then advance `sym_next`.
    ///
    /// This is the low-level byte writer only; the frequency accounting that C
    /// `_tr_tally` performs alongside the write lives in `trees.rs`.
    pub(crate) fn sym_write_raw(&mut self, dist: u16, lc: u8) {
        let base = self.lit_bufsize + self.sym_next;
        self.pending_buf[base] = (dist & 0xff) as u8;
        self.pending_buf[base + 1] = (dist >> 8) as u8;
        self.pending_buf[base + 2] = lc;
        self.sym_next += 3;
    }

    /// Read the LZ77 symbol stored at symbol-region byte offset `sx`, returning
    /// `(distance, length_code)`. The inverse of [`sym_write_raw`]; used by
    /// `compress_block` in `trees.rs`.
    #[inline]
    pub(crate) fn sym_read(&self, sx: usize) -> (u16, u8) {
        let base = self.lit_bufsize + sx;
        let dist = (self.pending_buf[base] as u16) | ((self.pending_buf[base + 1] as u16) << 8);
        let lc = self.pending_buf[base + 2];
        (dist, lc)
    }

    // =======================================================================
    // Phase H — deflateStateCheck equivalent (deflate.c 538-556)
    // =======================================================================

    /// Validate the engine's status, mirroring the status portion of C
    /// `deflateStateCheck`.
    ///
    /// In C this guarded against corrupted or foreign `z_stream` pointers by
    /// checking the `state`/`zalloc`/`zfree` pointers and the integer `status`
    /// against the eight legal sentinels. Here ownership is enforced by the
    /// type system — a `&DeflateState` always refers to a live, well-formed
    /// engine, and `status` is a `DeflateStatus` enum whose every value is one
    /// of the legal states by construction — so the status is always valid.
    #[inline]
    pub(crate) fn is_valid_status(&self) -> bool {
        true
    }
}

// ===========================================================================
// Phase G — StreamState (object-safe trait from crate::stream) + RAII
// ===========================================================================
//
// Implementing `StreamState` lets a boxed `DeflateState` be stored as
// `Box<dyn StreamState>` inside the public `ZStream`. `Clone` (derived above)
// covers `deflateCopy`. An explicit `Drop` is intentionally omitted: every
// buffer is an owned `Vec`/`Box`, so the compiler-generated drop glue frees
// them deterministically and leak-free, with no possibility of a double free
// (the RAII replacement for `deflateEnd`, AAP §0.6.3).

impl StreamState for DeflateState {
    /// This engine compresses, so it reports [`StreamKind::Deflate`].
    fn kind(&self) -> StreamKind {
        StreamKind::Deflate
    }

    /// Return the engine to its just-initialized state, porting C
    /// `deflateResetKeep` followed by `lm_init` (together C `deflateReset`).
    ///
    /// The match parameters and window/hash state are reset by `lm_init`; the
    /// Huffman bookkeeping is reset by `trees::tr_init`. The checksum seed is
    /// the literal `1` for zlib/raw and `0` for gzip (never `adler32(0, &[])`;
    /// see [`new_allocated`](DeflateState::new_allocated)).
    fn reset(&mut self) {
        // --- deflateResetKeep (deflate.c 644-677) ---
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = crate::constants::Z_UNKNOWN;

        self.pending = 0;
        self.pending_out = 0;

        // `deflate(..., Z_FINISH)` flips `wrap` negative once the trailer has
        // been written; a reset restores its positive value.
        if self.wrap < 0 {
            self.wrap = -self.wrap;
        }

        // Header-emission start state: gzip streams begin in GZIP, other
        // wrapped (zlib) streams in INIT, and raw streams skip header emission
        // by starting in BUSY.
        self.status = {
            #[cfg(feature = "gzip")]
            {
                if self.wrap == 2 {
                    DeflateStatus::Gzip
                } else if self.wrap != 0 {
                    DeflateStatus::Init
                } else {
                    DeflateStatus::Busy
                }
            }
            #[cfg(not(feature = "gzip"))]
            {
                if self.wrap != 0 {
                    DeflateStatus::Init
                } else {
                    DeflateStatus::Busy
                }
            }
        };

        // Checksum seed (literal, not via the checksum functions — see the
        // byte-exact note on `new_allocated`).
        self.adler = if self.wrap == 2 { 0 } else { 1 };
        self.last_flush = -2;

        // Reset the Huffman/bit-output bookkeeping (provided by `trees.rs`).
        self.tr_init();

        // --- lm_init (deflate.c 682-701) completes deflateReset ---
        self.lm_init();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ===========================================================================
// Phase D — DeflateStream: the transient per-call I/O context
// ===========================================================================

/// The per-call compression context: a mutable borrow of the persistent
/// [`DeflateState`] paired with the current input and output buffers and their
/// cursors.
///
/// This replaces the C `z_stream`'s `next_in`/`avail_in`/`next_out`/`avail_out`
/// fields for the duration of a single `deflate()` call. The five strategy
/// functions (`deflate_stored`/`fast`/`slow`/`rle`/`huff`) and the
/// `read_buf`/`fill_window`/`flush_pending`/`longest_match` helpers all operate
/// on a `DeflateStream`, which keeps them entirely in safe Rust: the input is a
/// shared slice and the output is an exclusive slice, so there is no aliasing
/// and no pointer arithmetic.
pub(crate) struct DeflateStream<'a> {
    /// The persistent engine state.
    pub(crate) state: &'a mut DeflateState,
    /// The input buffer for this call.
    pub(crate) input: &'a [u8],
    /// Number of input bytes already consumed (cursor into `input`).
    pub(crate) in_next: usize,
    /// The output buffer for this call.
    pub(crate) output: &'a mut [u8],
    /// Number of output bytes already produced (cursor into `output`).
    pub(crate) out_next: usize,
}

impl<'a> DeflateStream<'a> {
    /// Create a per-call context with both cursors at the start of their
    /// buffers.
    pub(crate) fn new(
        state: &'a mut DeflateState,
        input: &'a [u8],
        output: &'a mut [u8],
    ) -> DeflateStream<'a> {
        DeflateStream {
            state,
            input,
            in_next: 0,
            output,
            out_next: 0,
        }
    }

    /// Bytes of input still available (C `avail_in`).
    #[inline]
    pub(crate) fn avail_in(&self) -> usize {
        self.input.len() - self.in_next
    }

    /// Bytes of free space still available in the output (C `avail_out`).
    #[inline]
    pub(crate) fn avail_out(&self) -> usize {
        self.output.len() - self.out_next
    }

    /// Copy up to `size` input bytes into `window[into_window_at..]`, returning
    /// the number copied. Ports C `read_buf` (deflate.c 219-240).
    ///
    /// The running checksum is updated exactly as C does: Adler-32 when the
    /// stream uses the zlib wrapper (`wrap == 1`) and CRC-32 when it uses the
    /// gzip wrapper (`wrap == 2`). `total_in` is advanced here; `total_out` is
    /// *not* touched (this path produces no output).
    ///
    /// # Accounting sync
    ///
    /// This is the single place that advances `state.total_in`, mirroring C
    /// `read_buf`. The wrapper / FFI layer mirrors `total_in` back to
    /// `z_stream` after the call.
    pub(crate) fn read_buf_window(&mut self, into_window_at: usize, size: usize) -> usize {
        let len = self.avail_in().min(size);
        if len == 0 {
            return 0;
        }

        let start = self.in_next;
        // Copy input → window. `input` and `state` are disjoint fields, so the
        // shared and exclusive borrows coexist without aliasing.
        self.state.window[into_window_at..into_window_at + len]
            .copy_from_slice(&self.input[start..start + len]);

        // Update the checksum over exactly the bytes just consumed.
        match self.state.wrap {
            1 => self.state.adler = adler32(self.state.adler, &self.input[start..start + len]),
            2 => self.state.adler = crc32(self.state.adler, &self.input[start..start + len]),
            _ => {}
        }

        self.in_next += len;
        self.state.total_in += len as u64;
        len
    }

    /// The `deflate_stored` variant of `read_buf`: copy up to `size` input bytes
    /// straight into `output[out_next..]` (C `read_buf(strm, strm->next_out,
    /// len)`), returning the number copied.
    ///
    /// Like [`read_buf_window`](Self::read_buf_window) this updates the checksum
    /// and advances `total_in` and the input cursor, and it additionally
    /// advances the output cursor. It deliberately does **not** touch
    /// `total_out`: in C, `read_buf` only updates `total_in`, and
    /// `deflate_stored` adds the copied length to `total_out` itself. The
    /// `stored.rs` caller is therefore responsible for `state.total_out += len`.
    pub(crate) fn read_buf_output(&mut self, size: usize) -> usize {
        let len = self.avail_in().min(size);
        if len == 0 {
            return 0;
        }

        let start = self.in_next;
        let out_start = self.out_next;
        self.output[out_start..out_start + len].copy_from_slice(&self.input[start..start + len]);

        match self.state.wrap {
            1 => self.state.adler = adler32(self.state.adler, &self.input[start..start + len]),
            2 => self.state.adler = crc32(self.state.adler, &self.input[start..start + len]),
            _ => {}
        }

        self.in_next += len;
        self.state.total_in += len as u64;
        self.out_next += len;
        len
    }

    /// Flush as much of `pending_buf` to the output as fits, porting C
    /// `flush_pending` (deflate.c 950-968).
    ///
    /// The bit buffer is first flushed to whole bytes via `trees::tr_flush_bits`
    /// (C `_tr_flush_bits`). This is the single place that advances
    /// `state.total_out`.
    pub(crate) fn flush_pending(&mut self) {
        // Push any buffered bits out to whole bytes first (provided by trees.rs).
        self.state.tr_flush_bits();

        let len = self.state.pending.min(self.avail_out());
        if len == 0 {
            return;
        }

        let src_start = self.state.pending_out;
        let dst_start = self.out_next;
        self.output[dst_start..dst_start + len]
            .copy_from_slice(&self.state.pending_buf[src_start..src_start + len]);

        self.out_next += len;
        self.state.pending_out += len;
        self.state.total_out += len as u64;
        self.state.pending -= len;
        if self.state.pending == 0 {
            // All pending output consumed; rewind to the start of pending_buf.
            self.state.pending_out = 0;
        }
    }

    /// Refill the window with input, sliding it down when it is nearly full.
    /// Ports C `fill_window` (deflate.c 252-376) step for step.
    ///
    /// The 16-bit-`int` (`sizeof(int) <= 2`) special case from C is omitted:
    /// this port targets platforms with at least 32-bit `usize`. The final
    /// block zero-initializes up to `WIN_INIT` bytes past the current data so
    /// [`longest_match`](Self::longest_match) never reads uninitialized window
    /// memory and stays deterministic.
    pub(crate) fn fill_window(&mut self) {
        let wsize = self.state.w_size;

        loop {
            // Free space at the end of the window.
            let mut more = self.state.window_size - self.state.lookahead - self.state.strstart;

            // Slide the window down by `wsize` when it is almost full and the
            // lookahead is insufficient.
            if self.state.strstart >= wsize + max_dist(wsize) {
                let copy_len = wsize - more;
                // Move the upper half (the dictionary) down to the lower half.
                self.state.window.copy_within(wsize..wsize + copy_len, 0);
                // `match_start` may be below `wsize` here; in C this is an
                // unsigned wrap whose result is never read before the next
                // `longest_match` overwrites it, so `wrapping_sub` is both
                // faithful and panic-free.
                self.state.match_start = self.state.match_start.wrapping_sub(wsize);
                self.state.strstart -= wsize;
                self.state.block_start -= wsize as isize;
                self.state.insert = self.state.insert.min(self.state.strstart);
                self.state.slide_hash();
                more += wsize;
            }

            if self.avail_in() == 0 {
                break;
            }

            // Read input into the window just past the current lookahead.
            let at = self.state.strstart + self.state.lookahead;
            let n = self.read_buf_window(at, more);
            self.state.lookahead += n;

            // Initialize the hash value once there are at least MIN_MATCH bytes,
            // inserting any bytes that were deferred (`insert`) into the chains.
            if self.state.lookahead + self.state.insert >= MIN_MATCH {
                let mut str = self.state.strstart - self.state.insert;
                self.state.ins_h = self.state.window[str] as usize;
                self.state.ins_h = self
                    .state
                    .update_hash(self.state.ins_h, self.state.window[str + 1]);
                while self.state.insert != 0 {
                    self.state.ins_h = self
                        .state
                        .update_hash(self.state.ins_h, self.state.window[str + MIN_MATCH - 1]);
                    let h = self.state.ins_h;
                    self.state.prev[str & self.state.w_mask] = self.state.head[h];
                    self.state.head[h] = str as Pos;
                    str += 1;
                    self.state.insert -= 1;
                    if self.state.lookahead + self.state.insert < MIN_MATCH {
                        break;
                    }
                }
            }

            // Continue until we have enough lookahead or run out of input.
            if !(self.state.lookahead < MIN_LOOKAHEAD && self.avail_in() != 0) {
                break;
            }
        }

        // Lazily zero the WIN_INIT bytes beyond the current data the first time
        // they become reachable by the match scanner.
        if self.state.high_water < self.state.window_size {
            let curr = self.state.strstart + self.state.lookahead;
            if self.state.high_water < curr {
                // Previous high-water mark is below the current data: zero
                // WIN_INIT bytes, or up to the end of the window.
                let init = (self.state.window_size - curr).min(WIN_INIT);
                self.state.window[curr..curr + init].fill(0);
                self.state.high_water = curr + init;
            } else if self.state.high_water < curr + WIN_INIT {
                // High-water mark is within WIN_INIT of the current data: zero
                // out to `curr + WIN_INIT`, or up to the end of the window.
                let hw = self.state.high_water;
                let init = (curr + WIN_INIT - hw).min(self.state.window_size - hw);
                self.state.window[hw..hw + init].fill(0);
                self.state.high_water = hw + init;
            }
        }
    }

    /// Find the longest match for the string at `strstart`, starting the search
    /// at hash-chain position `cur_match`. Returns the match length (capped at
    /// the available lookahead) and sets `state.match_start` to the position of
    /// the best match.
    ///
    /// This ports the standard, portable, byte-at-a-time branch of C
    /// `longest_match` (the non-`FASTEST`, non-`UNALIGNED_OK` body, deflate.c
    /// 1389-1530). The C unrolled-by-eight comparison loop is purely a speed
    /// optimization; a straight byte comparison yields the identical match
    /// length and therefore identical compressed output. The comparison order,
    /// the `good_match` chain-shortening heuristic, and the `nice`/`lookahead`
    /// clamp are all preserved exactly because each of them changes which match
    /// is selected and thus the emitted bytes.
    pub(crate) fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = self.state.max_chain_length;
        let strstart = self.state.strstart;
        let mut best_len = self.state.prev_length;
        let mut nice = self.state.nice_match;
        let w_mask = self.state.w_mask;
        let wsize = self.state.w_size;
        let lookahead = self.state.lookahead;

        // Stop when `cur_match` falls to or below `limit`; matching the string
        // at window index 0 (NIL) is prevented.
        let limit = if strstart > max_dist(wsize) {
            strstart - max_dist(wsize)
        } else {
            0
        };

        // The two sentinel bytes at the current best length; refreshed whenever
        // `best_len` grows.
        let mut scan_end1 = self.state.window[strstart + best_len - 1];
        let mut scan_end = self.state.window[strstart + best_len];

        // Do not waste time if we already have a good match.
        if self.state.prev_length >= self.state.good_match {
            chain_length >>= 2;
        }

        // Do not look for matches beyond the end of the input; this keeps
        // deflate deterministic.
        nice = nice.min(lookahead);

        loop {
            let m = cur_match;

            // Skip this candidate unless it can possibly extend the current best
            // match: the bytes at `best_len`/`best_len-1` must equal the
            // sentinels and the first two bytes must match. Same order as C.
            if self.state.window[m + best_len] == scan_end
                && self.state.window[m + best_len - 1] == scan_end1
                && self.state.window[m] == self.state.window[strstart]
                && self.state.window[m + 1] == self.state.window[strstart + 1]
            {
                // Positions 0 and 1 just matched; position 2 is guaranteed equal
                // because the hash (HASH_BITS >= 8, MIN_MATCH == 3) maps equal
                // 3-byte prefixes to the same chain. Compare from MIN_MATCH
                // onward, capping the run at MAX_MATCH. `len` therefore lands in
                // `MIN_MATCH..=MAX_MATCH`, identical to the C result.
                let scan = &self.state.window[strstart + MIN_MATCH..strstart + MAX_MATCH];
                let cand = &self.state.window[m + MIN_MATCH..m + MAX_MATCH];
                let mut len = MIN_MATCH;
                for (a, b) in scan.iter().zip(cand.iter()) {
                    if a != b {
                        break;
                    }
                    len += 1;
                }

                if len > best_len {
                    self.state.match_start = m;
                    best_len = len;
                    if len >= nice {
                        break;
                    }
                    scan_end1 = self.state.window[strstart + best_len - 1];
                    scan_end = self.state.window[strstart + best_len];
                }
            }

            // Advance along the hash chain; stop at `limit` or chain exhaustion.
            cur_match = self.state.prev[cur_match & w_mask] as usize;
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }

        // Never report a match longer than the available lookahead.
        best_len.min(lookahead)
    }
}
