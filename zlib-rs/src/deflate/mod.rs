//! DEFLATE compressor — module root, public API, and shared engine
//! (`deflate.c` / `deflate.h` / `zlib.h`).
//!
//! This is the safe-Rust translation of the C `deflate.c` translation unit. It
//! wires together the compressor's eight submodules, defines the engine I/O
//! context shared by every match-search strategy, the main [`deflate`] state
//! machine, all of the public `deflate*` API functions, and the shared engine
//! primitives that the C code keeps as file-local helpers (`longest_match`,
//! `fill_window`, `read_buf`, `slide_hash`, `flush_pending`, `putShortMSB`, the
//! `FLUSH_BLOCK` / `FLUSH_BLOCK_ONLY` helpers, and the zlib/gzip header and
//! trailer emission).
//!
//! # Layering
//!
//! * [`state`] — the owned compressor state ([`state::DeflateState`]) and its
//!   status / block-state enums, the safe-Rust counterpart of the C
//!   `deflate_state` struct.
//! * [`trees`] — the Huffman tree data structures and the `_tr_*` block
//!   emission methods consumed by the engine.
//! * [`strategy`] — the per-level `configuration_table` ([`strategy::CONFIG_TABLE`])
//!   and the [`strategy::BlockProducer`] dispatcher selecting a per-block
//!   producer.
//! * [`stored`], [`fast`], [`slow`], [`huff`], [`rle`] — the five block
//!   producers (`deflate_stored` / `deflate_fast` / `deflate_slow` /
//!   `deflate_huff` / `deflate_rle`), one per compression strategy/level class.
//!
//! # Engine I/O context (design decision D1)
//!
//! The C compressor threads all per-call stream scalars through the `z_stream`
//! the strategy functions reach via `s->strm`: the `next_in` / `next_out`
//! cursors with their `avail_*` counts, the `total_in` / `total_out` byte
//! counters, the running `adler` check value, and the `data_type` field. To keep
//! [`state::DeflateState`] a pure, owned data model (it deliberately stores
//! *none* of those scalars), this module bundles them into a single
//! [`DeflateContext`] that the per-block producers borrow. The context also owns
//! the helper operations the C code calls `read_buf` and `flush_pending`, both
//! of which update the check value — which is why they live here, in the module
//! that may depend on [`crate::checksum`], rather than in the individual
//! strategy modules.
//!
//! # Safety
//!
//! The whole module honors the crate-wide `#![forbid(unsafe_code)]`: every
//! window read, hash write, and buffer transfer goes through bounds-checked
//! slice indexing, and no raw pointers are used. `Drop` on the owned
//! [`state::DeflateState`] buffers replaces the C `deflateEnd` `ZFREE` calls.

// ---------------------------------------------------------------------------
// `FLUSH_BLOCK` macro (textual scope for `huff.rs`)
// ---------------------------------------------------------------------------
//
// The C `FLUSH_BLOCK(s, last)` macro both emits the current block and, when the
// output buffer is exhausted, performs an early `return` of the appropriate
// `block_state`. `deflate_huff` (in `huff.rs`) relies on that early-return form.
// `macro_rules!` macros are textually scoped: defining `flush_block!` *before*
// the `pub mod huff;` declaration makes it visible inside `huff.rs` without an
// import, exactly as the C macro is visible to `deflate_huff`. It delegates to
// the free [`flush_block`] function (value namespace) — the macro
// (`flush_block!`) and the function (`flush_block`) occupy distinct namespaces,
// so they coexist without conflict.
macro_rules! flush_block {
    ($cx:expr, $last:expr) => {
        if let Some(__bstate) = $crate::deflate::flush_block($cx, $last) {
            return __bstate;
        }
    };
}

// ---------------------------------------------------------------------------
// Submodule tree
// ---------------------------------------------------------------------------

pub mod state;
pub mod trees;

pub mod strategy;

pub mod fast;
pub mod huff;
pub mod rle;
pub mod slow;
pub mod stored;

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

use alloc::boxed::Box;

// `crc32` doubles for the gzip header CRC (the C code's `crc32` / `crc32_z`
// calls are interchangeable for a contiguous byte range), so a single import
// covers both the `read_buf` checksum and the header CRC without a feature gate.
use crate::checksum::{adler32, crc32};
use crate::constants::{
    DEF_MEM_LEVEL, DataType, Flush, MAX_MATCH, MAX_MEM_LEVEL, MAX_WBITS, MIN_MATCH, PRESET_DICT,
    Strategy as CompressionStrategy, Z_DEFAULT_COMPRESSION, Z_DEFLATED,
};
use crate::error::{Result, ReturnCode, ZlibError};
use crate::gz_header::GzHeader;
use crate::stream::ZStream;

// `DeflateState` and `DeflateStatus` are brought into this module's scope by the
// `pub use` re-export just below (a `pub use` is also a `use`), so they are not
// re-imported here. `BlockState` is imported privately under its own name for
// internal use (the public re-export aliases it to `DeflateBlockState`).
use state::{BlockState, MIN_LOOKAHEAD, Pos, WIN_INIT};
use strategy::CONFIG_TABLE;

// ---------------------------------------------------------------------------
// Re-exports — the idiomatic public Rust API surface
// ---------------------------------------------------------------------------
//
// The `extern "C"` ABI wrappers (the C drop-in symbols) live in the separate
// `libz-rs-sys` shim, NOT here. This crate exposes the idiomatic Rust entry
// points: the owned state/status enums and the `deflate*` functions defined
// below.
pub use state::{BlockState as DeflateBlockState, DeflateState, DeflateStatus};

// ---------------------------------------------------------------------------
// Engine constants
// ---------------------------------------------------------------------------

/// The empty / "no position" sentinel for the hash tables (C `NIL`).
///
/// `head[h] == NIL` means hash slot `h` has never been seen; a hash chain
/// terminates at `NIL`. Window position `0` is kept out of the chains so the
/// value can double as the terminator.
pub(crate) const NIL: Pos = 0;

/// Operating-system code written into the gzip header (`zutil.h` `OS_CODE`).
///
/// The C library selects this from the build platform; the Rust target supports
/// only modern Unix-like and Windows hosts and uses the C "assume Unix" default
/// (`3`), matching a native build of zlib on Linux — the reference oracle. Only
/// the gzip header consumes it, so the constant is gated behind the `gzip`
/// feature to keep the zlib/raw core warning-clean under `-D warnings`.
#[cfg(feature = "gzip")]
pub(crate) const OS_CODE: u8 = 3;

// ---------------------------------------------------------------------------
// Rolling hash (C macro `UPDATE_HASH`)
// ---------------------------------------------------------------------------

/// Advance the rolling hash by one byte (C macro `UPDATE_HASH`).
///
/// Computes `h = ((h << hash_shift) ^ c) & hash_mask`. After `MIN_MATCH`
/// successive calls the contribution of the oldest byte has been shifted out, so
/// `ins_h` always reflects exactly the `MIN_MATCH`-byte string about to be
/// inserted. Keeping this byte-identical to C is essential: a different hash
/// sends strings to different chains and diverges the match decisions — and the
/// output — immediately. Shared by [`fill_window`] here and by `insert_string` /
/// `deflate_fast` in [`fast`].
#[inline]
pub(crate) fn update_hash(hash_shift: u32, hash_mask: usize, h: usize, c: u8) -> usize {
    ((h << hash_shift) ^ (c as usize)) & hash_mask
}

// ---------------------------------------------------------------------------
// Engine I/O context (design decision D1)
// ---------------------------------------------------------------------------

/// The engine I/O context shared by the DEFLATE per-block producers.
///
/// This is the safe-Rust stand-in for the parts of the C `z_stream` that the
/// strategy functions manipulate through `s->strm` (design decision D1). It
/// bundles a mutable borrow of the owned [`DeflateState`] with the borrowed
/// input/output slices, their cursors, and the stream-level scalars that
/// [`DeflateState`] does not itself store. The lifetime `'a` ties the context to
/// the borrowed state and buffers for one `deflate` call; nothing here is owned.
///
/// All buffer transfers performed through this context are bounds-checked slice
/// copies, so the whole compressor honors the crate-wide
/// `#![forbid(unsafe_code)]`.
pub(crate) struct DeflateContext<'a> {
    /// The owned compressor state (window, pending buffer, tree data, …).
    pub(crate) state: &'a mut DeflateState,
    /// Input buffer; `next_in` is the read cursor into it (C `next_in`).
    pub(crate) input: &'a [u8],
    /// Number of input bytes already consumed (the offset of C `next_in`).
    pub(crate) next_in: usize,
    /// Output buffer; `next_out` is the write cursor into it (C `next_out`).
    pub(crate) output: &'a mut [u8],
    /// Number of output bytes already produced (the offset of C `next_out`).
    pub(crate) next_out: usize,
    /// Running total of input bytes consumed (C `uLong total_in`).
    pub(crate) total_in: &'a mut u64,
    /// Running total of output bytes produced (C `uLong total_out`).
    pub(crate) total_out: &'a mut u64,
    /// Running check value (C `uLong adler`): Adler-32 when `wrap == 1`, CRC-32
    /// when `wrap == 2`, untouched otherwise.
    pub(crate) adler: &'a mut u32,
    /// Detected data type of the stream (C `int data_type`): set by
    /// [`flush_block_only`] from `detect_data_type()` on the first block, for
    /// FFI parity. Never influences the emitted bitstream.
    pub(crate) data_type: &'a mut i32,
}

impl DeflateContext<'_> {
    /// Bytes of input still available to read (C `strm->avail_in`).
    #[inline]
    pub(crate) fn avail_in(&self) -> usize {
        self.input.len() - self.next_in
    }

    /// Bytes of room still available in the output (C `strm->avail_out`).
    #[inline]
    pub(crate) fn avail_out(&self) -> usize {
        self.output.len() - self.next_out
    }

    /// C `read_buf(strm, next_out, size)` — copy up to `size` bytes from the
    /// input directly into the output buffer at `next_out`, update the running
    /// check value and `total_in`, and advance `next_in`.
    ///
    /// The output cursor is **not** advanced here (the C `read_buf` manages only
    /// the input side); the caller advances `next_out` itself. Returns the number
    /// of bytes copied.
    pub(crate) fn read_buf_to_output(&mut self, size: usize) -> usize {
        let len = core::cmp::min(self.avail_in(), size);
        if len == 0 {
            return 0;
        }
        let src = self.next_in;
        let dst = self.next_out;
        self.output[dst..dst + len].copy_from_slice(&self.input[src..src + len]);
        match self.state.wrap {
            1 => *self.adler = adler32(*self.adler, &self.input[src..src + len]),
            2 => *self.adler = crc32(*self.adler, &self.input[src..src + len]),
            _ => {}
        }
        self.next_in += len;
        // C unsigned-counter parity: `strm->total_in` is an unsigned `uLong`
        // that wraps on overflow. `wrapping_add` reproduces that exactly and
        // avoids a debug-build overflow panic on a hypothetical lifetime total
        // exceeding `u64::MAX` (review finding #13).
        *self.total_in = (*self.total_in).wrapping_add(len as u64);
        len
    }

    /// C `read_buf(strm, window + at, size)` — copy up to `size` bytes from the
    /// input into the window at offset `at`, update the running check value and
    /// `total_in`, and advance `next_in`. Returns the number of bytes copied.
    pub(crate) fn read_buf_to_window(&mut self, at: usize, size: usize) -> usize {
        let len = core::cmp::min(self.avail_in(), size);
        if len == 0 {
            return 0;
        }
        let src = self.next_in;
        self.state.window[at..at + len].copy_from_slice(&self.input[src..src + len]);
        match self.state.wrap {
            1 => *self.adler = adler32(*self.adler, &self.input[src..src + len]),
            2 => *self.adler = crc32(*self.adler, &self.input[src..src + len]),
            _ => {}
        }
        self.next_in += len;
        // C unsigned-counter parity (see `read_buf`): wrap rather than panic.
        *self.total_in = (*self.total_in).wrapping_add(len as u64);
        len
    }

    /// C `flush_pending(strm)` — flush the bit accumulator into the pending
    /// buffer, then copy as many pending bytes as fit into `next_out`, advancing
    /// the output cursor, `pending_out`, and `total_out`, and resetting
    /// `pending_out` to the start of the buffer once fully drained.
    pub(crate) fn flush_pending(&mut self) {
        // C `_tr_flush_bits(s)`: move any whole bytes from the bit accumulator
        // into `pending` before measuring it.
        self.state.tr_flush_bits();

        let mut len = self.state.pending;
        let avail_out = self.avail_out();
        if len > avail_out {
            len = avail_out;
        }
        if len == 0 {
            return;
        }

        let po = self.state.pending_out;
        let no = self.next_out;
        self.output[no..no + len].copy_from_slice(&self.state.pending_buf[po..po + len]);
        self.next_out += len;
        self.state.pending_out += len;
        // C unsigned-counter parity: `strm->total_out` wraps on overflow.
        *self.total_out = (*self.total_out).wrapping_add(len as u64);
        self.state.pending -= len;
        if self.state.pending == 0 {
            self.state.pending_out = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Hash-table and match-search primitives (`impl DeflateState`)
// ---------------------------------------------------------------------------
//
// `slide_hash` and `longest_match` are methods on the owned state (mirroring the
// `impl DeflateState` convention used across `state.rs` / `trees.rs`) so that
// `deflate_fast` can call them as `s.slide_hash()` / `s.longest_match(head)`.
// The free [`longest_match`] wrapper below adapts the `Pos`-typed call site in
// `deflate_slow`.

impl DeflateState {
    /// Slide the hash tables by one window when the window itself slides
    /// (C `slide_hash`, `deflate.c` lines ~187-210).
    ///
    /// Every entry of `head` (all `hash_size` slots) and of `prev` (all `w_size`
    /// slots) is decremented by `w_size`, clamping to [`NIL`] for entries that
    /// would go negative. This keeps stored positions consistent after the
    /// window's upper half is copied down to the lower half. We slide even at
    /// level 0 (where matching is unused) to stay consistent with C, and set
    /// `slid = 1` afterwards (C `deflate.c` line 209) so `deflateCopy` can tell
    /// the hash tables have been slid.
    pub(crate) fn slide_hash(&mut self) {
        // `w_size <= 32768` so it always fits in a `Pos` (`u16`).
        let wsize = self.w_size as Pos;
        // `saturating_sub` is exactly C's `m >= wsize ? m - wsize : NIL` clamp.
        for m in self.head.iter_mut() {
            *m = m.saturating_sub(wsize);
        }
        // `prev` has exactly `w_size` entries, so this slides all of it.
        for m in self.prev.iter_mut() {
            *m = m.saturating_sub(wsize);
        }
        self.slid = 1;
    }

    /// Find the longest match for the string at `strstart`, starting the search
    /// at chain head `cur_match`, and return its length (C `longest_match`,
    /// the default-build standard byte-by-byte version, `deflate.c`
    /// lines 1389-1531).
    ///
    /// Sets [`match_start`](DeflateState::match_start) to the position of the
    /// best match found. Matches no longer than
    /// [`prev_length`](DeflateState::prev_length) are discarded (the result then
    /// equals `prev_length` and `match_start` is left as-is, matching C's
    /// "garbage" contract). The returned length never exceeds
    /// [`lookahead`](DeflateState::lookahead).
    ///
    /// C's inner loop uses a `scan_end` / `scan_end1` shortcut to skip
    /// candidates that cannot beat the current best before doing the full byte
    /// compare. That shortcut is a pure performance optimization: any candidate
    /// it skips has a match length `<= best_len` and so could never have updated
    /// `best_len` or `match_start`. This translation therefore performs a
    /// straightforward longest-common-prefix scan, which is far cleaner under
    /// bounds-checked indexing and yields **bit-identical** `best_len` /
    /// `match_start` results. The `good_match` chain-shortening, the
    /// `nice_match`/`lookahead` clamps, the `MAX_DIST` limit, and the
    /// "most-recent match wins on ties" rule are all preserved exactly.
    pub(crate) fn longest_match(&mut self, cur_match: usize) -> usize {
        let mut cur_match = cur_match;
        let mut chain_length = self.max_chain_length;
        let strstart = self.strstart;
        let mut best_len = self.prev_length;
        let lookahead = self.lookahead;
        let wmask = self.w_mask;

        // Clamp `nice_match` to the available lookahead so the search stops at
        // the end of the input (C: `if (nice_match > lookahead) nice_match =
        // lookahead;`). `nice_match` is always >= 0 here.
        let mut nice_match = self.nice_match as usize;
        if nice_match > lookahead {
            nice_match = lookahead;
        }

        // Stop when the chain reaches positions older than `limit` (the furthest
        // back a match may start), and never match the string at window index 0
        // (the `NIL` sentinel).
        let max_dist = self.max_dist();
        let limit = if strstart > max_dist {
            strstart - max_dist
        } else {
            NIL as usize
        };

        // Don't waste time on long chains once we already hold a good match.
        if best_len >= self.good_match as usize {
            chain_length >>= 2;
        }

        let window = &self.window;
        let mut match_start = self.match_start;

        loop {
            // Longest common prefix of the current string and the candidate,
            // capped at MAX_MATCH and at the window bounds (the bounds cap is a
            // safety net; in correct operation `cur_match < strstart` and
            // `strstart + MAX_MATCH <= window.len()`, so it equals MAX_MATCH and
            // matches C, which scans up to `window + strstart + MAX_MATCH`).
            let span = core::cmp::min(
                MAX_MATCH,
                core::cmp::min(window.len() - strstart, window.len() - cur_match),
            );
            let mut len = 0usize;
            while len < span && window[strstart + len] == window[cur_match + len] {
                len += 1;
            }

            if len > best_len {
                match_start = cur_match;
                best_len = len;
                if len >= nice_match {
                    break;
                }
            }

            // Advance to the next older position on this hash chain.
            cur_match = self.prev[cur_match & wmask] as usize;
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }

        self.match_start = match_start;

        // OUT assertion: the match length is never greater than the lookahead.
        if best_len <= lookahead {
            best_len
        } else {
            lookahead
        }
    }
}

// ---------------------------------------------------------------------------
// Shared engine free functions
// ---------------------------------------------------------------------------

/// Put a 16-bit value into the pending buffer in MSB (big-endian) order
/// (C `putShortMSB`, `deflate.c` lines 937-942).
///
/// Used for the zlib header and the big-endian zlib Adler-32 trailer.
fn put_short_msb(cx: &mut DeflateContext<'_>, value: u16) {
    cx.state.put_byte((value >> 8) as u8);
    cx.state.put_byte((value & 0xff) as u8);
}

/// Initialize / clear the hash table (C macro `CLEAR_HASH`, `deflate.c`
/// lines 170-175).
///
/// Sets every `head` slot to [`NIL`] and resets the `slid` flag. (`prev` is
/// initialized lazily on the fly, exactly as in C.)
fn clear_hash(s: &mut DeflateState) {
    for h in s.head.iter_mut() {
        *h = NIL;
    }
    s.slid = 0;
}

/// Refill the sliding window when the lookahead becomes insufficient
/// (C `fill_window`, `deflate.c` lines 252-376).
///
/// Reads input into the window, sliding the window's upper half down to the
/// lower half when it fills up (and sliding the hash tables to match via
/// [`DeflateState::slide_hash`]), then re-seeds `ins_h` and inserts any `insert`
/// strings carried over a block/dictionary boundary. The trailing high-water
/// bookkeeping keeps the bytes the match scanner may speculatively read
/// initialized to zero, which under `#![forbid(unsafe_code)]` is also what keeps
/// those reads in-bounds. The C 16-bit (`sizeof(int) <= 2`) special case is
/// dropped — the Rust target supports modern 32-/64-bit platforms only.
pub(crate) fn fill_window(cx: &mut DeflateContext<'_>) {
    let wsize = cx.state.w_size;

    loop {
        // Free space at the end of the window before this iteration.
        let mut more = cx.state.window_size - cx.state.lookahead - cx.state.strstart;

        // Slide the window down once the current position has advanced a full
        // window past the start, freeing the upper half for new input.
        if cx.state.strstart >= wsize + cx.state.max_dist() {
            let copy_len = wsize - more;
            cx.state.window.copy_within(wsize..wsize + copy_len, 0);
            // `match_start` is stale here (only meaningful right after a
            // successful match); C lets the unsigned subtraction wrap, so
            // `wrapping_sub` reproduces it without a debug-build panic.
            cx.state.match_start = cx.state.match_start.wrapping_sub(wsize);
            cx.state.strstart -= wsize;
            cx.state.block_start -= wsize as isize;
            if cx.state.insert > cx.state.strstart {
                cx.state.insert = cx.state.strstart;
            }
            cx.state.slide_hash();
            more += wsize;
        }

        // No more input to pull in this round.
        if cx.avail_in() == 0 {
            break;
        }

        let buf_start = cx.state.strstart + cx.state.lookahead;
        let n = cx.read_buf_to_window(buf_start, more);
        cx.state.lookahead += n;

        // Initialize the hash value now that we have at least MIN_MATCH bytes
        // (counting any `insert` carried over). Mirrors C `deflate.c`
        // lines 314-336 exactly, including inserting the carried strings.
        if cx.state.lookahead + cx.state.insert >= MIN_MATCH {
            let mut str_pos = cx.state.strstart - cx.state.insert;
            cx.state.ins_h = cx.state.window[str_pos] as usize;
            let next = cx.state.window[str_pos + 1];
            cx.state.ins_h = update_hash(
                cx.state.hash_shift,
                cx.state.hash_mask,
                cx.state.ins_h,
                next,
            );
            while cx.state.insert != 0 {
                let c = cx.state.window[str_pos + MIN_MATCH - 1];
                cx.state.ins_h =
                    update_hash(cx.state.hash_shift, cx.state.hash_mask, cx.state.ins_h, c);
                let h = cx.state.ins_h;
                cx.state.prev[str_pos & cx.state.w_mask] = cx.state.head[h];
                cx.state.head[h] = str_pos as Pos;
                str_pos += 1;
                cx.state.insert -= 1;
                if cx.state.lookahead + cx.state.insert < MIN_MATCH {
                    break;
                }
            }
        }

        // C loop condition: `lookahead < MIN_LOOKAHEAD && avail_in != 0`.
        if cx.state.lookahead >= MIN_LOOKAHEAD || cx.avail_in() == 0 {
            break;
        }
    }

    // High-water handling (C `deflate.c` lines 342-372): zero the window region
    // the match scanner may speculatively read past the data end.
    if cx.state.high_water < cx.state.window_size {
        let curr = cx.state.strstart + cx.state.lookahead;
        if cx.state.high_water < curr {
            let mut init = cx.state.window_size - curr;
            if init > WIN_INIT {
                init = WIN_INIT;
            }
            for b in &mut cx.state.window[curr..curr + init] {
                *b = 0;
            }
            cx.state.high_water = curr + init;
        } else if cx.state.high_water < curr + WIN_INIT {
            let mut init = curr + WIN_INIT - cx.state.high_water;
            if init > cx.state.window_size - cx.state.high_water {
                init = cx.state.window_size - cx.state.high_water;
            }
            let hw = cx.state.high_water;
            for b in &mut cx.state.window[hw..hw + init] {
                *b = 0;
            }
            cx.state.high_water += init;
        }
    }
}

/// `deflate_slow`'s entry point into the match search (C `longest_match`).
///
/// `deflate_slow` holds its chain head as a [`Pos`] (`u16`); Rust does not apply
/// deref/numeric coercion to function arguments, so this free wrapper takes
/// `Pos` explicitly and delegates to [`DeflateState::longest_match`]. (Note that
/// `deflate_fast` calls the method directly with a `usize`.)
pub(crate) fn longest_match(cx: &mut DeflateContext<'_>, cur_match: Pos) -> usize {
    cx.state.longest_match(cur_match as usize)
}

/// Emit the current block and flush the pending buffer (C macro
/// `FLUSH_BLOCK_ONLY`, `deflate.c` lines 1630-1639).
///
/// Calls [`DeflateState::tr_flush_block`] on the window slice
/// `window[block_start..strstart]` (the bytes of this block), advances
/// `block_start`, then drains the pending buffer. Unlike [`flush_block`], it does
/// **not** check `avail_out` or return early — `deflate_slow` performs that check
/// itself.
///
/// The data-type detection the C code performs inside `_tr_flush_block` is done
/// here instead (the Rust `tr_flush_block` omits it to keep the bitstream
/// byte-identical): `strm->data_type` is set from `detect_data_type()` on the
/// first block at `level > 0`. It never influences the emitted bytes.
pub(crate) fn flush_block_only(cx: &mut DeflateContext<'_>, last: bool) {
    // C `_tr_flush_block`: `if (level > 0 && data_type == Z_UNKNOWN) data_type =
    // detect_data_type(s);`. Done before the trees are rebuilt, while the
    // symbol-frequency tables still reflect this block.
    if cx.state.level > 0 && *cx.data_type == DataType::Unknown.as_i32() {
        let dt = cx.state.detect_data_type();
        *cx.data_type = dt;
    }

    let block_start = cx.state.block_start;
    // `(long)strstart - block_start`; `block_start` may be negative after a
    // window slide, in which case the block length still comes out positive.
    let stored_len = (cx.state.strstart as isize - block_start) as usize;

    // `tr_flush_block` needs `buf` as `Option<&[u8]>` into the window, but it
    // also takes `&mut self`. It never reads `self.window` internally (stored
    // blocks copy the passed `buf`; compressed blocks read `sym_buf`), so move
    // the window out, borrow the block slice, flush, then put it back — a
    // zero-copy way to satisfy the borrow checker without `unsafe`.
    let window = core::mem::take(&mut cx.state.window);
    if block_start >= 0 {
        let bs = block_start as usize;
        cx.state
            .tr_flush_block(Some(&window[bs..bs + stored_len]), stored_len, last);
    } else {
        cx.state.tr_flush_block(None, stored_len, last);
    }
    cx.state.window = window;

    cx.state.block_start = cx.state.strstart as isize;
    cx.flush_pending();
}

/// Emit the current block, flush it, and signal output exhaustion (C macro
/// `FLUSH_BLOCK`, `deflate.c` lines 1642-1645).
///
/// Runs [`flush_block_only`], then returns `Some(state)` — to be propagated out
/// of the block producer — when the output buffer is exhausted (the C macro's
/// `if (avail_out == 0) return (last) ? finish_started : need_more;`), and `None`
/// otherwise. `deflate_fast` and `deflate_rle` consume this directly; the
/// `flush_block!` macro wraps it for `deflate_huff`.
pub(crate) fn flush_block(cx: &mut DeflateContext<'_>, last: bool) -> Option<BlockState> {
    flush_block_only(cx, last);
    if cx.avail_out() == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Public engine entry — `deflate`
// ---------------------------------------------------------------------------

/// C `RANK(f)` macro (`deflate.c` line 977).
///
/// Maps a flush mode to a monotonic priority used to reject duplicate or
/// lower-priority flushes when there is no new input to process. The exact
/// formula must be preserved so the `BUF_ERROR`-vs-`OK` distinction on repeated
/// `deflate` calls matches C byte-for-byte: `RANK(f) = 2*f - (f > 4 ? 9 : 0)`.
#[inline]
fn rank(flush: i32) -> i32 {
    flush * 2 - if flush > 4 { 9 } else { 0 }
}

/// Compress as much input as possible, producing as much output as fits
/// (C `deflate`, `deflate.c` lines 981-1291).
///
/// This is the idiomatic-Rust engine entry the FFI shim (`libz-rs-sys`) and the
/// one-shot helpers in [`crate::util`] call. Its slice-based I/O model mirrors
/// the C `z_stream` discipline without raw pointers: `input` is the read-only
/// source (the C `next_in` / `avail_in` pair), `output` is the writable sink
/// (the C `next_out` / `avail_out` pair), and the running totals and check value
/// live on `strm`. The return tuple is `(status, consumed, produced)`:
///
/// * `status` — the [`ReturnCode`] (`Z_OK` / `Z_STREAM_END`) on success, or a
///   [`ZlibError`] (`Z_STREAM_ERROR` / `Z_BUF_ERROR`) on the documented error
///   conditions.
/// * `consumed` — bytes read from `input` (added to `strm.total_in`).
/// * `produced` — bytes written to `output` (added to `strm.total_out`).
///
/// The stream-level scalars the C engine reaches through `s->strm`
/// (`total_in` / `total_out` / `adler` / `data_type`) are snapshotted into locals
/// first so the [`DeflateContext`] can hold `&mut` borrows of them while the
/// single `&mut DeflateState` borrow is live — the safe-Rust resolution of the
/// C code's simultaneous `strm->...` and `s->...` access. They are written back
/// onto `strm` before returning.
pub fn deflate(
    strm: &mut ZStream,
    input: &[u8],
    output: &mut [u8],
    flush: Flush,
) -> (Result<ReturnCode>, usize, usize) {
    let mut adler = strm.adler;
    let mut total_in = strm.total_in;
    let mut total_out = strm.total_out;
    let mut data_type = strm.data_type;

    let (result, consumed, produced) = match strm.deflate_state_mut() {
        Some(state) => {
            let mut cx = DeflateContext {
                state,
                input,
                next_in: 0,
                output,
                next_out: 0,
                total_in: &mut total_in,
                total_out: &mut total_out,
                adler: &mut adler,
                data_type: &mut data_type,
            };
            let rc = deflate_run(&mut cx, flush);
            (rc, cx.next_in, cx.next_out)
        }
        // C `deflateStateCheck` failure → Z_STREAM_ERROR.
        None => (Err(ZlibError::StreamError), 0, 0),
    };

    strm.adler = adler;
    strm.total_in = total_in;
    strm.total_out = total_out;
    strm.data_type = data_type;

    (result, consumed, produced)
}

/// The body of [`deflate`], operating on an already-built [`DeflateContext`].
///
/// Reproduces the C `deflate` control flow exactly (`deflate.c` lines 985-1291):
/// entry validation, the zlib/gzip header state machine, per-block dispatch
/// through [`strategy::select`], the post-block flush handling, and the trailer.
fn deflate_run(cx: &mut DeflateContext<'_>, flush: Flush) -> Result<ReturnCode> {
    // ---- Entry validation (deflate.c lines 985-1026) ----
    //
    // C: `if (deflateStateCheck(strm) || flush > Z_BLOCK || flush < 0)`. The
    // state check already ran in the public `deflate` wrapper; `flush < 0` is
    // unrepresentable in the `Flush` enum; `flush > Z_BLOCK` rejects `Z_TREES`.
    // `Flush` has no `PartialOrd`, so the ordering is done on the `as_i32` codes.
    if flush.as_i32() > Flush::Block.as_i32() {
        return Err(ZlibError::StreamError);
    }
    // C also rejects `next_out == NULL` and a non-empty input whose `next_in`
    // is NULL; both are unrepresentable with Rust slices. The remaining clause
    // — finishing must stay finishing — is preserved.
    if cx.state.status == DeflateStatus::Finish && flush != Flush::Finish {
        return Err(ZlibError::StreamError);
    }
    if cx.avail_out() == 0 {
        return Err(ZlibError::BufError);
    }

    let old_flush = cx.state.last_flush;
    cx.state.last_flush = flush.as_i32();

    // Flush as much pending output as possible.
    if cx.state.pending != 0 {
        cx.flush_pending();
        if cx.avail_out() == 0 {
            // The output is full. `deflate` will be called again with more
            // room, possibly with both `pending` and `avail_in` zero — which is
            // not an error — so arrange to return `Z_OK` rather than
            // `Z_BUF_ERROR` on that next call.
            cx.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    } else if cx.avail_in() == 0
        && rank(flush.as_i32()) <= rank(old_flush)
        && flush != Flush::Finish
    {
        // Nothing to do and a duplicate/lower-priority flush: report
        // `Z_BUF_ERROR` (but repeated `Z_FINISH` keeps returning `Z_STREAM_END`,
        // handled by the `flush != Finish` guard).
        return Err(ZlibError::BufError);
    }

    // The caller must not supply more input after the first `Z_FINISH`.
    if cx.state.status == DeflateStatus::Finish && cx.avail_in() != 0 {
        return Err(ZlibError::BufError);
    }

    // ---- Write the zlib header (deflate.c lines 1031-1064) ----
    if cx.state.status == DeflateStatus::Init && cx.state.wrap == 0 {
        // Raw DEFLATE: no header, go straight to compressing.
        cx.state.status = DeflateStatus::Busy;
    }
    if cx.state.status == DeflateStatus::Init {
        let w_bits = cx.state.w_bits;
        let level = cx.state.level;
        let strategy = cx.state.strategy;

        // CMF/FLG: method+window in the high byte, flags in the low byte.
        let mut header: u32 = (Z_DEFLATED as u32 + ((w_bits - 8) << 4)) << 8;
        let level_flags: u32 =
            if strategy.as_i32() >= CompressionStrategy::HuffmanOnly.as_i32() || level < 2 {
                0
            } else if level < 6 {
                1
            } else if level == 6 {
                2
            } else {
                3
            };
        header |= level_flags << 6;
        if cx.state.strstart != 0 {
            header |= PRESET_DICT as u32;
        }
        // Make the 16-bit header a multiple of 31 (the zlib FCHECK rule).
        header += 31 - (header % 31);

        put_short_msb(cx, header as u16);

        // Emit the Adler-32 of the preset dictionary, big-endian, if one was set.
        if cx.state.strstart != 0 {
            put_short_msb(cx, (*cx.adler >> 16) as u16);
            put_short_msb(cx, (*cx.adler & 0xffff) as u16);
        }
        // D6: C `adler32(0L, Z_NULL, 0)` evaluates to the literal `1` — *not*
        // the `0` that Rust's `adler32(0, &[])` returns. Initialize explicitly.
        *cx.adler = 1;
        cx.state.status = DeflateStatus::Busy;

        // Compression must start with an empty pending buffer.
        cx.flush_pending();
        if cx.state.pending != 0 {
            cx.state.last_flush = -1;
            return Ok(ReturnCode::Ok);
        }
    }

    // ---- Write the gzip header (deflate.c lines 1065-1208) ----
    //
    // The entire gzip header machine is feature-gated; the zlib/raw core builds
    // and runs without it. `emit_gzip_header` returns `Some(status)` when the
    // output buffer filled mid-header and the call must return early.
    #[cfg(feature = "gzip")]
    {
        if let Some(early) = emit_gzip_header(cx) {
            return early;
        }
    }

    // ---- Start a new block or continue the current one (lines 1212-1262) ----
    if cx.avail_in() != 0
        || cx.state.lookahead != 0
        || (flush != Flush::NoFlush && cx.state.status != DeflateStatus::Finish)
    {
        let level = cx.state.level;
        let strategy = cx.state.strategy;
        // C: `s->level == 0 ? deflate_stored : strategy == Z_HUFFMAN_ONLY ?
        // deflate_huff : strategy == Z_RLE ? deflate_rle :
        // configuration_table[level].func`. `strategy::select` encapsulates that
        // exact dispatch.
        let bstate = strategy::select(level, strategy).run(cx, flush);

        if bstate == BlockState::FinishStarted || bstate == BlockState::FinishDone {
            cx.state.status = DeflateStatus::Finish;
        }
        if bstate == BlockState::NeedMore || bstate == BlockState::FinishStarted {
            if cx.avail_out() == 0 {
                // Avoid `Z_BUF_ERROR` on the next call (see the pending-flush
                // comment above). The flush is completed on the next call with
                // the same flush mode, so no empty block is emitted here.
                cx.state.last_flush = -1;
            }
            return Ok(ReturnCode::Ok);
        }
        if bstate == BlockState::BlockDone {
            if flush == Flush::PartialFlush {
                cx.state.tr_align();
            } else if flush != Flush::Block {
                // `Z_FULL_FLUSH` or `Z_SYNC_FLUSH`: emit an empty stored block,
                // which `inflate_sync` recognizes as a flush marker.
                cx.state.tr_stored_block(&[], false);
                if flush == Flush::FullFlush {
                    // Forget the match history so the stream can be resynced.
                    clear_hash(cx.state);
                    if cx.state.lookahead == 0 {
                        cx.state.strstart = 0;
                        cx.state.block_start = 0;
                        cx.state.insert = 0;
                    }
                }
            }
            cx.flush_pending();
            if cx.avail_out() == 0 {
                cx.state.last_flush = -1;
                return Ok(ReturnCode::Ok);
            }
        }
    }

    // ---- Write the trailer (deflate.c lines 1263-1289) ----
    if flush != Flush::Finish {
        return Ok(ReturnCode::Ok);
    }
    if cx.state.wrap <= 0 {
        // Raw stream (or the trailer already written): nothing more to emit.
        return Ok(ReturnCode::StreamEnd);
    }

    // gzip trailer — CRC-32 then ISIZE, both little-endian. Only reachable with
    // the gzip feature on (raw/zlib streams never have `wrap == 2`).
    #[cfg(feature = "gzip")]
    let wrote_gzip_trailer = if cx.state.wrap == 2 {
        let crc = *cx.adler;
        cx.state.put_byte((crc & 0xff) as u8);
        cx.state.put_byte(((crc >> 8) & 0xff) as u8);
        cx.state.put_byte(((crc >> 16) & 0xff) as u8);
        cx.state.put_byte(((crc >> 24) & 0xff) as u8);
        // ISIZE is the input size modulo 2^32 (the low four bytes of total_in).
        let total = *cx.total_in;
        cx.state.put_byte((total & 0xff) as u8);
        cx.state.put_byte(((total >> 8) & 0xff) as u8);
        cx.state.put_byte(((total >> 16) & 0xff) as u8);
        cx.state.put_byte(((total >> 24) & 0xff) as u8);
        true
    } else {
        false
    };
    #[cfg(not(feature = "gzip"))]
    let wrote_gzip_trailer = false;

    if !wrote_gzip_trailer {
        // zlib trailer — big-endian Adler-32.
        put_short_msb(cx, (*cx.adler >> 16) as u16);
        put_short_msb(cx, (*cx.adler & 0xffff) as u16);
    }

    cx.flush_pending();
    // Negate `wrap` so the trailer is written exactly once.
    if cx.state.wrap > 0 {
        cx.state.wrap = -cx.state.wrap;
    }
    // `Z_STREAM_END` only once the trailer has fully drained to the output.
    if cx.state.pending != 0 {
        Ok(ReturnCode::Ok)
    } else {
        Ok(ReturnCode::StreamEnd)
    }
}

// ---------------------------------------------------------------------------
// gzip header emission (feature-gated)
// ---------------------------------------------------------------------------

/// Fold the freshly-written header bytes `[beg..pending)` into the running gzip
/// header CRC when the caller requested one (C macro `HCRC_UPDATE`, `deflate.c`).
#[cfg(feature = "gzip")]
fn hcrc_update(cx: &mut DeflateContext<'_>, beg: usize) {
    let hcrc = cx.state.gzhead.as_ref().map(|h| h.hcrc).unwrap_or(false);
    if hcrc && cx.state.pending > beg {
        let p = cx.state.pending;
        *cx.adler = crc32(*cx.adler, &cx.state.pending_buf[beg..p]);
    }
}

/// Emit the gzip header (C `deflate.c` lines 1065-1208), advancing through the
/// `Gzip` → `Extra` → `Name` → `Comment` → `Hcrc` → `Busy` status sequence.
///
/// Returns `Some(status)` when the output buffer filled while emitting a field
/// and the enclosing `deflate` call must return early (re-entering the same
/// state on the next call); returns `None` once the whole header has been
/// emitted (status now `Busy`) and block compression may begin.
///
/// The Rust [`GzHeader`] stores the `name` / `comment` / `extra` payloads
/// *without* a trailing NUL, so the `Name` / `Comment` states emit the stored
/// bytes followed by an explicit `0`, reproducing the C loop over a
/// NUL-terminated C string byte-for-byte.
#[cfg(feature = "gzip")]
fn emit_gzip_header(cx: &mut DeflateContext<'_>) -> Option<Result<ReturnCode>> {
    use DeflateStatus::{Comment, Extra, Gzip, Hcrc, Name};

    // ---- GZIP_STATE: magic, flag byte, MTIME, XFL, OS ----
    if cx.state.status == Gzip {
        // D6: C `crc32(0L, Z_NULL, 0)` is the literal `0`.
        *cx.adler = 0;
        cx.state.put_byte(31);
        cx.state.put_byte(139);
        cx.state.put_byte(8);

        let level = cx.state.level;
        let strategy = cx.state.strategy;
        // XFL: 2 = best compression, 4 = fastest, 0 = neither.
        let xfl: u8 = if level == 9 {
            2
        } else if strategy.as_i32() >= CompressionStrategy::HuffmanOnly.as_i32() || level < 2 {
            4
        } else {
            0
        };

        if cx.state.gzhead.is_none() {
            // No caller-supplied header: the fixed 10-byte gzip header.
            cx.state.put_byte(0); // FLG
            cx.state.put_byte(0); // MTIME[0]
            cx.state.put_byte(0); // MTIME[1]
            cx.state.put_byte(0); // MTIME[2]
            cx.state.put_byte(0); // MTIME[3]
            cx.state.put_byte(xfl);
            cx.state.put_byte(OS_CODE);
            cx.state.status = DeflateStatus::Busy;

            cx.flush_pending();
            if cx.state.pending != 0 {
                cx.state.last_flush = -1;
                return Some(Ok(ReturnCode::Ok));
            }
            // status == Busy: the field-state blocks below are all skipped, and
            // control falls through to the final `None`.
        } else {
            // Caller-supplied header: flag byte, fixed fields, then advance into
            // the variable-length field states.
            let (text, hcrc, has_extra, has_name, has_comment, time, os, extra_len) = {
                let h = cx.state.gzhead.as_ref().unwrap();
                (
                    h.text,
                    h.hcrc,
                    h.extra.is_some(),
                    h.name.is_some(),
                    h.comment.is_some(),
                    h.time,
                    h.os,
                    h.extra_len(),
                )
            };
            let flags = (text as u8)
                | ((hcrc as u8) << 1)
                | ((has_extra as u8) << 2)
                | ((has_name as u8) << 3)
                | ((has_comment as u8) << 4);
            cx.state.put_byte(flags);
            cx.state.put_byte((time & 0xff) as u8);
            cx.state.put_byte(((time >> 8) & 0xff) as u8);
            cx.state.put_byte(((time >> 16) & 0xff) as u8);
            cx.state.put_byte(((time >> 24) & 0xff) as u8);
            cx.state.put_byte(xfl);
            cx.state.put_byte((os & 0xff) as u8);
            if has_extra {
                cx.state.put_byte((extra_len & 0xff) as u8);
                cx.state.put_byte(((extra_len >> 8) & 0xff) as u8);
            }
            if hcrc {
                // Fold the header bytes emitted so far into the header CRC.
                let p = cx.state.pending;
                *cx.adler = crc32(*cx.adler, &cx.state.pending_buf[..p]);
            }
            cx.state.gzindex = 0;
            cx.state.status = Extra;
        }
    }

    // ---- EXTRA_STATE: the "extra" field, chunked against `pending_buf_size` ----
    if cx.state.status == Extra {
        if let Some(extra) = cx.state.gzhead.as_ref().and_then(|h| h.extra.clone()) {
            let extra_len = extra.len();
            let mut beg = cx.state.pending; // first byte not yet folded into CRC
            let mut left = (extra_len & 0xffff) - cx.state.gzindex;
            while cx.state.pending + left > cx.state.pending_buf_size {
                let copy = cx.state.pending_buf_size - cx.state.pending;
                let p = cx.state.pending;
                let gi = cx.state.gzindex;
                cx.state.pending_buf[p..p + copy].copy_from_slice(&extra[gi..gi + copy]);
                cx.state.pending = cx.state.pending_buf_size;
                hcrc_update(cx, beg);
                cx.state.gzindex += copy;
                cx.flush_pending();
                if cx.state.pending != 0 {
                    cx.state.last_flush = -1;
                    return Some(Ok(ReturnCode::Ok));
                }
                beg = 0;
                left -= copy;
            }
            let p = cx.state.pending;
            let gi = cx.state.gzindex;
            cx.state.pending_buf[p..p + left].copy_from_slice(&extra[gi..gi + left]);
            cx.state.pending += left;
            hcrc_update(cx, beg);
            cx.state.gzindex = 0;
        }
        cx.state.status = Name;
    }

    // ---- NAME_STATE: NUL-terminated original file name ----
    if cx.state.status == Name {
        if let Some(name) = cx.state.gzhead.as_ref().and_then(|h| h.name.clone()) {
            let mut beg = cx.state.pending;
            loop {
                if cx.state.pending == cx.state.pending_buf_size {
                    hcrc_update(cx, beg);
                    cx.flush_pending();
                    if cx.state.pending != 0 {
                        cx.state.last_flush = -1;
                        return Some(Ok(ReturnCode::Ok));
                    }
                    beg = 0;
                }
                let gi = cx.state.gzindex;
                let val = if gi < name.len() { name[gi] } else { 0 };
                cx.state.gzindex += 1;
                cx.state.put_byte(val);
                if val == 0 {
                    break;
                }
            }
            hcrc_update(cx, beg);
            cx.state.gzindex = 0;
        }
        cx.state.status = Comment;
    }

    // ---- COMMENT_STATE: NUL-terminated comment ----
    if cx.state.status == Comment {
        if let Some(comment) = cx.state.gzhead.as_ref().and_then(|h| h.comment.clone()) {
            let mut beg = cx.state.pending;
            loop {
                if cx.state.pending == cx.state.pending_buf_size {
                    hcrc_update(cx, beg);
                    cx.flush_pending();
                    if cx.state.pending != 0 {
                        cx.state.last_flush = -1;
                        return Some(Ok(ReturnCode::Ok));
                    }
                    beg = 0;
                }
                let gi = cx.state.gzindex;
                let val = if gi < comment.len() { comment[gi] } else { 0 };
                cx.state.gzindex += 1;
                cx.state.put_byte(val);
                if val == 0 {
                    break;
                }
            }
            hcrc_update(cx, beg);
        }
        cx.state.status = Hcrc;
    }

    // ---- HCRC_STATE: optional 2-byte little-endian header CRC ----
    if cx.state.status == Hcrc {
        let hcrc = cx.state.gzhead.as_ref().map(|h| h.hcrc).unwrap_or(false);
        if hcrc {
            if cx.state.pending + 2 > cx.state.pending_buf_size {
                cx.flush_pending();
                if cx.state.pending != 0 {
                    cx.state.last_flush = -1;
                    return Some(Ok(ReturnCode::Ok));
                }
            }
            let crc = *cx.adler;
            cx.state.put_byte((crc & 0xff) as u8);
            cx.state.put_byte(((crc >> 8) & 0xff) as u8);
            // D6: reset the running CRC to the literal `0` for the body.
            *cx.adler = 0;
        }
        cx.state.status = DeflateStatus::Busy;

        cx.flush_pending();
        if cx.state.pending != 0 {
            cx.state.last_flush = -1;
            return Some(Ok(ReturnCode::Ok));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Initialization & reset API
// ---------------------------------------------------------------------------

/// Initialize a stream for compression with full control over the format and
/// memory/strategy tuning (C `deflateInit2_`, `deflate.c` lines 387-533).
///
/// The C `version` / `stream_size` ABI-guard arguments are intentionally absent
/// from this idiomatic entry: that compile-time compatibility check belongs to
/// the `libz-rs-sys` FFI shim, which performs it before delegating here. The
/// `strategy` argument is the strongly-typed [`CompressionStrategy`], so the C
/// `strategy < 0 || strategy > Z_FIXED` range check is enforced by the type
/// system; the FFI shim maps a raw `int` to the enum (rejecting out-of-range
/// values with `Z_STREAM_ERROR`) before calling in.
///
/// `window_bits` carries the same overloading as C: `8..=15` selects a zlib
/// wrapper, `-15..=-8` selects raw DEFLATE (no wrapper), and (with the `gzip`
/// feature) `24..=31` selects a gzip wrapper.
pub fn deflate_init2(
    strm: &mut ZStream,
    level: i32,
    method: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: CompressionStrategy,
) -> Result<ReturnCode> {
    strm.msg = None;

    let mut level = level;
    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }

    let mut window_bits = window_bits;
    let mut wrap = 1;
    if window_bits < 0 {
        // Negative `windowBits` suppresses the zlib wrapper (raw DEFLATE).
        if window_bits < -15 {
            return Err(ZlibError::StreamError);
        }
        wrap = 0;
        window_bits = -window_bits;
    } else if window_bits > 15 {
        // `windowBits > 15` requests a gzip wrapper (feature-gated).
        #[cfg(feature = "gzip")]
        {
            wrap = 2;
            window_bits -= 16;
        }
        #[cfg(not(feature = "gzip"))]
        {
            return Err(ZlibError::StreamError);
        }
    }

    // Validate the (post-overloading) parameters, mirroring the C guard exactly.
    // `method` must be `Z_DEFLATED`; `memLevel` ∈ 1..=MAX_MEM_LEVEL; the resolved
    // `windowBits` ∈ 8..=15; `level` ∈ 0..=9; and an 8-bit window is permitted
    // only for the zlib wrapper.
    if !(1..=MAX_MEM_LEVEL).contains(&mem_level)
        || method != Z_DEFLATED
        || !(8..=15).contains(&window_bits)
        || !(0..=9).contains(&level)
        || (window_bits == 8 && wrap != 1)
    {
        return Err(ZlibError::StreamError);
    }

    if window_bits == 8 {
        // C: "until 256-byte window bug fixed" — promote an 8-bit window to 9.
        window_bits = 9;
    }

    // Allocate the owned compressor state (fallibly — the C `Z_MEM_ERROR` path).
    let state = DeflateState::new(
        level,
        method as u8,
        window_bits as u32,
        mem_level,
        strategy,
        wrap,
    )?;
    strm.set_deflate_state(Box::new(state));

    // C `deflateReset` finishes initialization: reset counters, status, the
    // check value, the trees, and the match state.
    deflate_reset(strm)
}

/// Initialize a stream for compression at `level` with the default method,
/// window size, memory level, and strategy (C `deflateInit_`, `deflate.c`
/// line 379).
///
/// Equivalent to [`deflate_init2`] with `method = Z_DEFLATED`,
/// `window_bits = MAX_WBITS`, `mem_level = DEF_MEM_LEVEL`, and
/// `strategy = CompressionStrategy::Default`.
pub fn deflate_init(strm: &mut ZStream, level: i32) -> Result<ReturnCode> {
    deflate_init2(
        strm,
        level,
        Z_DEFLATED,
        MAX_WBITS,
        DEF_MEM_LEVEL,
        CompressionStrategy::Default,
    )
}

/// Reset all stream bookkeeping *except* the allocated buffers, returning the
/// compressor to its just-initialized state (C `deflateResetKeep`, `deflate.c`
/// lines 644-677).
///
/// Zeroes the totals, clears the message, resets the data-type hint, empties the
/// pending buffer, restores a `Z_FINISH`-negated `wrap`, reselects the initial
/// status (`Gzip` for a gzip wrapper, otherwise `Init`), re-seeds the check
/// value (D6: the literal `0` for gzip/CRC-32, `1` for zlib/Adler-32 — *not*
/// `crc32(0,&[])` / `adler32(0,&[])`), and re-initializes the Huffman trees.
pub fn deflate_reset_keep(strm: &mut ZStream) -> Result<ReturnCode> {
    // Touch the state first (also the C `deflateStateCheck`), reading back the
    // `wrap` the stream-level check value depends on.
    let wrap = {
        let state = strm.deflate_state_or_err()?;
        state.pending = 0;
        state.pending_out = 0;
        if state.wrap < 0 {
            // Was made negative by a completed `deflate(..., Z_FINISH)`.
            state.wrap = -state.wrap;
        }
        let wrap = state.wrap;
        state.status = if wrap == 2 {
            DeflateStatus::Gzip
        } else {
            DeflateStatus::Init
        };
        state.last_flush = -2;
        state.tr_init();
        wrap
    };

    // Stream-level scalars (the borrow of the state has ended).
    strm.reset_counters();
    // D6: literal check-value seeds, never `adler32/crc32(_, &[])`.
    strm.adler = if wrap == 2 { 0 } else { 1 };

    Ok(ReturnCode::Ok)
}

/// Reset a stream for a fresh compression run, reinitializing both the stream
/// bookkeeping and the LZ77 match state (C `deflateReset`, `deflate.c`
/// line 704).
///
/// Runs [`deflate_reset_keep`] and then [`lm_init`].
pub fn deflate_reset(strm: &mut ZStream) -> Result<ReturnCode> {
    deflate_reset_keep(strm)?;
    lm_init(strm.deflate_state_or_err()?);
    Ok(ReturnCode::Ok)
}

/// Initialize the LZ77 match state from the per-level configuration table
/// (C `lm_init`, `deflate.c` lines 682-701).
///
/// Sets the window size, clears the hash table, loads the four search-tuning
/// parameters (`max_lazy_match` / `good_match` / `nice_match` /
/// `max_chain_length`) from [`CONFIG_TABLE`] for the current level, and resets
/// the match cursors. These four values are correctness-critical: they steer the
/// match search, so loading them verbatim from the table is what keeps the
/// emitted bitstream bit-identical to C at every level.
fn lm_init(s: &mut DeflateState) {
    s.window_size = 2 * s.w_size;

    clear_hash(s);

    let cfg = &CONFIG_TABLE[s.level as usize];
    s.max_lazy_match = cfg.max_lazy as usize;
    s.good_match = cfg.good_length as u32;
    s.nice_match = cfg.nice_length as i32;
    s.max_chain_length = cfg.max_chain as u32;

    s.strstart = 0;
    s.block_start = 0;
    s.lookahead = 0;
    s.insert = 0;
    s.match_length = MIN_MATCH - 1;
    s.prev_length = MIN_MATCH - 1;
    s.match_available = false;
    s.ins_h = 0;
}

// ---------------------------------------------------------------------------
// Dictionary, header, and tuning API
// ---------------------------------------------------------------------------

/// Initialize the compressor with a preset dictionary (C `deflateSetDictionary`,
/// `deflate.c` lines 559-622).
///
/// Only valid for a raw or zlib stream that has not yet produced output
/// (`status == Init`, `lookahead == 0`, `wrap != 2`); a gzip stream or a
/// mid-stream call is a `Z_STREAM_ERROR`. For the zlib wrapper, the *full*
/// dictionary is first folded into the running Adler-32 (before any tail
/// truncation). The dictionary is then loaded into the window and hash chains
/// exactly as the compressor would have processed it as input — but with the
/// check value temporarily frozen (`wrap = 0`) so `read_buf` does not fold those
/// bytes a second time.
pub fn deflate_set_dictionary(strm: &mut ZStream, dictionary: &[u8]) -> Result<ReturnCode> {
    // Validate and read `wrap` / `w_size` (also the C `deflateStateCheck`).
    let (wrap, w_size) = {
        let s = strm.deflate_state_or_err()?;
        let wrap = s.wrap;
        if wrap == 2 || (wrap == 1 && s.status != DeflateStatus::Init) || s.lookahead != 0 {
            return Err(ZlibError::StreamError);
        }
        (wrap, s.w_size)
    };

    // For the zlib wrapper, fold the entire dictionary into the Adler-32 first.
    if wrap == 1 {
        strm.adler = adler32(strm.adler, dictionary);
    }

    // Snapshot the stream scalars threaded through the transient context.
    let mut total_in = strm.total_in;
    let mut total_out = strm.total_out;
    let mut adler = strm.adler;
    let mut data_type = strm.data_type;

    {
        let s = strm.deflate_state_or_err()?;
        // Freeze the check value so `read_buf` does not recompute it.
        s.wrap = 0;

        // If the dictionary would fill the window, keep only its tail.
        let dict: &[u8] = if dictionary.len() >= w_size {
            if wrap == 0 {
                // The window was not necessarily empty: forget the history.
                clear_hash(s);
                s.strstart = 0;
                s.block_start = 0;
                s.insert = 0;
            }
            &dictionary[dictionary.len() - w_size..]
        } else {
            dictionary
        };

        let mut cx = DeflateContext {
            state: s,
            input: dict,
            next_in: 0,
            output: &mut [],
            next_out: 0,
            total_in: &mut total_in,
            total_out: &mut total_out,
            adler: &mut adler,
            data_type: &mut data_type,
        };

        fill_window(&mut cx);
        while cx.state.lookahead >= MIN_MATCH {
            let mut str_ = cx.state.strstart;
            // `n = lookahead - (MIN_MATCH - 1)` is ≥ 1 here, so the C do/while
            // runs at least once.
            let mut n = cx.state.lookahead - (MIN_MATCH - 1);
            loop {
                // INSERT_STRING, inlined (the C `UPDATE_HASH` + chain link).
                let c = cx.state.window[str_ + MIN_MATCH - 1];
                cx.state.ins_h =
                    update_hash(cx.state.hash_shift, cx.state.hash_mask, cx.state.ins_h, c);
                let h = cx.state.ins_h;
                let head_h = cx.state.head[h];
                let wmask = cx.state.w_mask;
                cx.state.prev[str_ & wmask] = head_h;
                cx.state.head[h] = str_ as Pos;
                str_ += 1;
                n -= 1;
                if n == 0 {
                    break;
                }
            }
            cx.state.strstart = str_;
            cx.state.lookahead = MIN_MATCH - 1;
            fill_window(&mut cx);
        }
        cx.state.strstart += cx.state.lookahead;
        cx.state.block_start = cx.state.strstart as isize;
        cx.state.insert = cx.state.lookahead;
        cx.state.lookahead = 0;
        cx.state.match_length = MIN_MATCH - 1;
        cx.state.prev_length = MIN_MATCH - 1;
        cx.state.match_available = false;
        // Restore the original wrapper mode.
        cx.state.wrap = wrap;
    }

    // Write back the scalars `read_buf` advanced (notably `total_in`, which — as
    // in C — accumulates the dictionary bytes; this never affects the output
    // because preset dictionaries are disallowed for gzip's ISIZE trailer).
    strm.total_in = total_in;
    strm.total_out = total_out;
    strm.adler = adler;
    strm.data_type = data_type;

    Ok(ReturnCode::Ok)
}

/// Retrieve the sliding-window history that would be used as a dictionary
/// (C `deflateGetDictionary`, `deflate.c` lines 625-641).
///
/// Copies the most recent `min(strstart + lookahead, w_size)` window bytes into
/// `dictionary` (when provided) and returns that length. Passing `None` queries
/// the length without copying.
pub fn deflate_get_dictionary(strm: &mut ZStream, dictionary: Option<&mut [u8]>) -> Result<usize> {
    let s = strm.deflate_state_or_err()?;
    let mut len = s.strstart + s.lookahead;
    if len > s.w_size {
        len = s.w_size;
    }
    if let Some(dict) = dictionary {
        if len != 0 {
            let start = s.strstart + s.lookahead - len;
            dict[..len].copy_from_slice(&s.window[start..start + len]);
        }
    }
    Ok(len)
}

/// Provide a gzip header for the stream (C `deflateSetHeader`, `deflate.c`
/// lines 714-718).
///
/// Only valid for a gzip stream (`wrap == 2`); any other wrapper is a
/// `Z_STREAM_ERROR`. The header takes effect as the stream's gzip header is
/// emitted (see [`emit_gzip_header`]).
pub fn deflate_set_header(strm: &mut ZStream, head: GzHeader) -> Result<ReturnCode> {
    let s = strm.deflate_state_or_err()?;
    if s.wrap != 2 {
        return Err(ZlibError::StreamError);
    }
    s.gzhead = Some(head);
    Ok(ReturnCode::Ok)
}

/// Report the number of bytes and bits of output generated but not yet provided
/// (C `deflatePending`, `deflate.c` lines 722-734).
///
/// Returns `(pending, bits)` where `pending` is the count of bytes buffered in
/// the pending output and `bits` is the count of bits buffered in the bit
/// accumulator (`bi_valid`).
pub fn deflate_pending(strm: &mut ZStream) -> Result<(u32, i32)> {
    let s = strm.deflate_state_or_err()?;
    Ok((s.pending as u32, s.bi_valid))
}

/// Insert `bits` (0..=16) low bits of `value` into the output ahead of the next
/// block (C `deflatePrime`, `deflate.c` lines 745-771).
///
/// Used to prepend bits to the stream (for example, to byte-align a raw stream
/// into a larger bit container). The C overlay-collision `Z_BUF_ERROR` guard is
/// irrelevant here — design decision D5 keeps the symbol and pending buffers
/// separate — and is therefore omitted.
pub fn deflate_prime(strm: &mut ZStream, bits: i32, value: i32) -> Result<ReturnCode> {
    /// C `Buf_size`: the width of the bit accumulator interface (16 bits).
    const BUF_SIZE: i32 = 16;

    let s = strm.deflate_state_or_err()?;
    if !(0..=16).contains(&bits) {
        return Err(ZlibError::BufError);
    }

    let mut bits = bits;
    let mut value = value;
    loop {
        let mut put = BUF_SIZE - s.bi_valid;
        if put > bits {
            put = bits;
        }
        // `put + bi_valid <= 16`, so the shifted, masked value always fits in
        // the 16-bit accumulator and the `as u16` truncation is exact.
        s.bi_buf |= ((value & ((1 << put) - 1)) << s.bi_valid) as u16;
        s.bi_valid += put;
        s.tr_flush_bits();
        value >>= put;
        bits -= put;
        if bits == 0 {
            break;
        }
    }
    Ok(ReturnCode::Ok)
}

/// Dynamically change the compression level and/or strategy mid-stream
/// (C `deflateParams`, `deflate.c` lines 774-816).
///
/// When the change would select a different block algorithm and compression has
/// already begun (`last_flush != -2`), the current block is first flushed with
/// `Z_BLOCK`; this is why the call threads the per-call `input` / `output`
/// slices and returns `(status, consumed, produced)` like [`deflate`] itself.
/// If that flush cannot complete in the supplied buffers, `Z_BUF_ERROR` is
/// returned and the parameters are left unchanged.
pub fn deflate_params(
    strm: &mut ZStream,
    level: i32,
    strategy: CompressionStrategy,
    input: &[u8],
    output: &mut [u8],
) -> (Result<ReturnCode>, usize, usize) {
    let mut level = level;
    if level == Z_DEFAULT_COMPRESSION {
        level = 6;
    }
    if !(0..=9).contains(&level) {
        return (Err(ZlibError::StreamError), 0, 0);
    }

    // Read the current level/strategy/last_flush (also the C state check).
    let (cur_level, cur_strategy, last_flush) = match strm.deflate_state_mut() {
        Some(s) => (s.level, s.strategy, s.last_flush),
        None => return (Err(ZlibError::StreamError), 0, 0),
    };

    let func = CONFIG_TABLE[cur_level as usize].func;
    let mut consumed = 0;
    let mut produced = 0;

    if (strategy != cur_strategy || func != CONFIG_TABLE[level as usize].func) && last_flush != -2 {
        // The algorithm is changing mid-stream: flush the current block first.
        let (err, c, p) = deflate(strm, input, output, Flush::Block);
        consumed = c;
        produced = p;
        if matches!(err, Err(ZlibError::StreamError)) {
            return (err, consumed, produced);
        }
        // The block must have fully drained for the change to be safe.
        let s = match strm.deflate_state_mut() {
            Some(s) => s,
            None => return (Err(ZlibError::StreamError), consumed, produced),
        };
        let unflushed = (s.strstart as isize - s.block_start) as usize + s.lookahead;
        if (input.len() - consumed) != 0 || unflushed != 0 {
            return (Err(ZlibError::BufError), consumed, produced);
        }
    }

    // Apply the changes to the live state.
    let s = match strm.deflate_state_mut() {
        Some(s) => s,
        None => return (Err(ZlibError::StreamError), consumed, produced),
    };
    if s.level != level {
        if s.level == 0 && s.matches != 0 {
            // Leaving level 0 (stored): the hash table holds stale data. One
            // intervening block can be salvaged by sliding; otherwise clear it.
            if s.matches == 1 {
                s.slide_hash();
            } else {
                clear_hash(s);
            }
            s.matches = 0;
        }
        s.level = level;
        let cfg = &CONFIG_TABLE[level as usize];
        s.max_lazy_match = cfg.max_lazy as usize;
        s.good_match = cfg.good_length as u32;
        s.nice_match = cfg.nice_length as i32;
        s.max_chain_length = cfg.max_chain as u32;
    }
    s.strategy = strategy;
    (Ok(ReturnCode::Ok), consumed, produced)
}

/// Fine-tune the internal match-search parameters (C `deflateTune`, `deflate.c`
/// lines 819-830).
///
/// An advanced hook (used by the compressor's own profiling) that overrides the
/// four per-level search-tuning fields directly.
pub fn deflate_tune(
    strm: &mut ZStream,
    good_length: i32,
    max_lazy: i32,
    nice_length: i32,
    max_chain: i32,
) -> Result<ReturnCode> {
    let s = strm.deflate_state_or_err()?;
    s.good_match = good_length as u32;
    s.max_lazy_match = max_lazy as usize;
    s.nice_match = nice_length;
    s.max_chain_length = max_chain as u32;
    Ok(ReturnCode::Ok)
}

// ---------------------------------------------------------------------------
// Bound, copy, and teardown API
// ---------------------------------------------------------------------------

impl DeflateState {
    /// An upper bound on the compressed size of `source_len` input bytes given
    /// the current configuration (C `deflateBound_z`, `deflate.c` lines 856-932).
    ///
    /// Reproduces the full C computation — *not* only the default-parameters
    /// branch — so a caller can size an output buffer that is guaranteed to hold
    /// the result of a single-shot `deflate`. The three regimes are: the tight
    /// `~0.03%` bound for the default `windowBits = 15` / `memLevel = 8`; the
    /// conservative `~4%` (stored) or `~13%` (fixed) bound otherwise; plus the
    /// wrapper overhead. All sums saturate to `u64::MAX` on overflow, matching
    /// the C `(z_size_t)-1` clamps.
    ///
    /// The wrapper length for a gzip header counts the `name` / `comment`
    /// trailing NUL bytes that the C code includes (`do { wraplen++ } while
    /// (*str++)`), even though the Rust [`GzHeader`] stores those payloads
    /// without the terminator — hence the explicit `+ 1` per field.
    pub fn deflate_bound(&self, source_len: u64) -> u64 {
        // Fixed-block bound (~13%): 9-bit literals, length-255 runs (memLevel 2).
        let mut fixedlen = source_len
            .wrapping_add(source_len >> 3)
            .wrapping_add(source_len >> 8)
            .wrapping_add(source_len >> 9)
            .wrapping_add(4);
        if fixedlen < source_len {
            fixedlen = u64::MAX;
        }
        // Stored-block bound (~4%): length-127 stored blocks (memLevel 1).
        let mut storelen = source_len
            .wrapping_add(source_len >> 5)
            .wrapping_add(source_len >> 7)
            .wrapping_add(source_len >> 11)
            .wrapping_add(7);
        if storelen < source_len {
            storelen = u64::MAX;
        }

        // Wrapper length, keyed on the absolute value of `wrap`.
        let wrap = if self.wrap < 0 { -self.wrap } else { self.wrap };
        let wraplen: u64 = match wrap {
            0 => 0,
            1 => 6 + if self.strstart != 0 { 4 } else { 0 },
            // gzip wrapper (and the C "compiler happiness" default).
            _ => {
                let extra: u64 = {
                    #[cfg(feature = "gzip")]
                    {
                        self.gzhead.as_ref().map_or(0, |h| {
                            let mut e = 0u64;
                            if h.extra.is_some() {
                                e += 2 + h.extra_len() as u64;
                            }
                            if h.name.is_some() {
                                e += h.name_len() as u64 + 1;
                            }
                            if h.comment.is_some() {
                                e += h.comment_len() as u64 + 1;
                            }
                            if h.hcrc {
                                e += 2;
                            }
                            e
                        })
                    }
                    #[cfg(not(feature = "gzip"))]
                    {
                        0
                    }
                };
                18 + extra
            }
        };

        // Non-default window or hash table: one of the conservative bounds.
        if self.w_bits != 15 || self.hash_bits != 15 {
            let bound = if self.w_bits <= self.hash_bits && self.level != 0 {
                fixedlen
            } else {
                storelen
            };
            return bound.saturating_add(wraplen);
        }

        // Default settings: the tight bound (~0.03% overhead).
        let bound = source_len
            .wrapping_add(source_len >> 12)
            .wrapping_add(source_len >> 14)
            .wrapping_add(source_len >> 25)
            .wrapping_add(7) // C `+ 13 - 6`
            .wrapping_add(wraplen);
        if bound < source_len { u64::MAX } else { bound }
    }
}

/// Copy the complete compression state from `source` into `dest`
/// (C `deflateCopy`, `deflate.c` lines 1317-1377).
///
/// Used to fork a stream — to compress with the same history in two ways, or to
/// roll back to a checkpoint. Because the Rust [`DeflateState`] owns its buffers
/// and derives `Clone`, the copy is a single deep clone plus a copy of the
/// stream-level scalars; none of the C pointer fixups (the `strm` back-pointer,
/// the `dyn_tree` pointers, the `sym_buf` overlay) are needed.
pub fn deflate_copy(dest: &mut ZStream, source: &mut ZStream) -> Result<ReturnCode> {
    let cloned = match source.deflate_state() {
        Some(ss) => ss.clone(),
        None => return Err(ZlibError::StreamError),
    };

    // C `zmemcpy(dest, source, sizeof(z_stream))` for the stream-level scalars.
    dest.total_in = source.total_in;
    dest.total_out = source.total_out;
    dest.adler = source.adler;
    dest.data_type = source.data_type;
    dest.msg = source.msg;

    dest.set_deflate_state(Box::new(cloned));
    Ok(ReturnCode::Ok)
}

/// Free the compression state (C `deflateEnd`, `deflate.c` lines 1293-1310).
///
/// Returns `Z_STREAM_ERROR` if the stream is not an initialized DEFLATE stream,
/// and `Z_DATA_ERROR` if the compressor was caught mid-stream
/// (`status == Busy`) — but in either of the latter cases the owned buffers are
/// still released, because dropping the `Box<DeflateState>` (RAII) performs the
/// work of the C `ZFREE` calls unconditionally.
pub fn deflate_end(strm: &mut ZStream) -> Result<ReturnCode> {
    if !strm.is_deflate() {
        return Err(ZlibError::StreamError);
    }
    strm.end()
}
