//! DEFLATE compressor (`deflate.c` / `deflate.h` / `trees.c` / `trees.h`).
//!
//! This module root wires together the compressor's submodules and owns the
//! engine I/O context shared by every match-search strategy:
//!
//! * [`state`] — the owned compressor state ([`state::DeflateState`]) and its
//!   status enum, the safe-Rust counterpart of the C `deflate_state` struct.
//! * [`trees`] — the Huffman tree *data structures* (`ct_data`, the static tree
//!   descriptors, and the per-code extra-bit tables) consumed by `state`.
//! * [`stored`] — the level-0 (no compression) strategy ([`stored::deflate_stored`]),
//!   the first of the per-block producers.
//!
//! # Engine I/O context (design decision D1)
//!
//! The C compressor threads all per-call stream scalars through the
//! `z_stream` the strategy functions reach via `s->strm`: the `next_in` /
//! `next_out` pointers with their `avail_*` counts, the `total_in` / `total_out`
//! byte counters, and the running `adler` check value. To keep
//! [`state::DeflateState`] a pure, owned data model (it deliberately stores
//! *none* of those scalars), this module bundles them into a single
//! [`DeflateContext`] that the per-block producers borrow. The context also owns
//! the two helper operations the C code calls `read_buf` and `flush_pending`,
//! both of which update the check value — which is why they live here, in the
//! module that may depend on [`crate::checksum`], rather than in the individual
//! strategy modules.
//!
//! The remaining match-search strategies (`deflate_fast` / `deflate_slow` /
//! `deflate_rle` / `deflate_huff`) and the top-level `deflate` driver layer on
//! top of these types in subsequent milestones.

pub mod fast;
pub mod state;
pub mod stored;
pub mod trees;

use crate::checksum::{adler32, crc32};
use state::DeflateState;

/// The engine I/O context shared by the DEFLATE per-block producers.
///
/// This is the safe-Rust stand-in for the parts of the C `z_stream` that the
/// strategy functions manipulate through `s->strm` (design decision D1). It
/// bundles a mutable borrow of the owned [`DeflateState`] with the borrowed
/// input/output slices, their cursors, and the stream-level scalars that
/// [`DeflateState`] does not itself store.
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
        // `output` and `input` are distinct fields, so the simultaneous
        // mutable/shared borrows below are disjoint and safe.
        self.output[dst..dst + len].copy_from_slice(&self.input[src..src + len]);
        match self.state.wrap {
            1 => *self.adler = adler32(*self.adler, &self.input[src..src + len]),
            2 => *self.adler = crc32(*self.adler, &self.input[src..src + len]),
            _ => {}
        }
        self.next_in += len;
        *self.total_in += len as u64;
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
        *self.total_in += len as u64;
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
        *self.total_out += len as u64;
        self.state.pending -= len;
        if self.state.pending == 0 {
            self.state.pending_out = 0;
        }
    }
}
