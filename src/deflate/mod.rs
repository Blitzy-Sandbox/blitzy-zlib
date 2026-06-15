//! DEFLATE engine module root — shared types and submodule wiring.
//!
//! This is the module root for `crate::deflate`, the safe-Rust port of zlib's
//! compression engine (`deflate.c` + `deflate.h`). It declares the engine's
//! foundation submodules and defines the header-emission state machine
//! ([`DeflateStatus`]) that every part of the engine shares.
//!
//! # Layout
//!
//! | Submodule           | C source     | Responsibility                                            |
//! |---------------------|--------------|-----------------------------------------------------------|
//! | [`mod@state`]       | `deflate.h`  | [`DeflateState`](state::DeflateState) + LZ77 plumbing      |
//! | [`mod@strategy`]    | `deflate.c`  | Per-level [`CONFIG_TABLE`](strategy::CONFIG_TABLE) sizing  |
//! | [`mod@trees`]       | `trees.c`    | Huffman tree build/emit, static tables, bit-level output  |
//!
//! The per-level strategy inner loops (`deflate_stored`/`deflate_fast`/
//! `deflate_slow`/`deflate_rle`/`deflate_huff`), the strategy dispatch over
//! [`CONFIG_TABLE`](strategy::CONFIG_TABLE),
//! the `flush_block` helpers, and the public `deflate*` orchestration
//! (`deflate()`/`deflateInit2_`/`deflateEnd`, AAP §0.4.1) build on the
//! foundation declared here. They are layered on top of this root as the
//! deflate engine is completed; this file owns only the cross-cutting state
//! enum and the submodule declarations they all depend on.
//!
//! # `DeflateStatus` — the header-emission state machine
//!
//! The C implementation tracks header emission with an integer `status` field
//! on `deflate_state`, compared against eight sentinel `#define`s in
//! `deflate.h` (`INIT_STATE`=42 … `FINISH_STATE`=666). Per AAP §0.6.1 this
//! becomes the exhaustive [`DeflateStatus`] enum: an integer `status` can hold
//! an out-of-range value (which C's `deflateStateCheck` must guard against),
//! whereas a `DeflateStatus` is *always* one of the legal states by
//! construction, so the status-validity check
//! ([`DeflateState::is_valid_status`](state::DeflateState::is_valid_status))
//! becomes trivially true.
//!
//! # Constraints
//!
//! * **No `unsafe`.** The entire `crate::deflate` tree is safe Rust; `unsafe`
//!   lives only in `crate::ffi` and `crate::inflate::fast` (AAP §0.6.2).
//! * **`no_std`-clean.** Only `core`/`alloc` are referenced, never `std`.

pub mod state;
pub mod strategy;
pub mod trees;

// Per-level strategy inner loops — the safe-Rust ports of the C
// `deflate_stored`/`deflate_fast`/`deflate_slow`/`deflate_rle`/`deflate_huff`
// functions. Each implements the shared `strategy::CompressFn` signature and is
// selected per `deflate()` call by `strategy::deflate_dispatch` over the
// per-level `strategy::CONFIG_TABLE`.
pub mod fast;
pub mod huff;
pub mod rle;
pub mod slow;
pub mod stored;

use crate::deflate::state::{BlockState, DeflateStream};

/// Header-emission state machine for the DEFLATE engine — the safe-Rust
/// replacement for the integer `status` field and its `*_STATE` sentinel
/// `#define`s in `deflate.h` (AAP §0.6.1).
///
/// The discriminants are pinned to the canonical zlib sentinel values so the
/// state semantics line up exactly with the C engine (and so the values are
/// recognizable when debugging against a C reference). The enum is evaluated
/// with exhaustive `match`, which both removes the need for C's
/// `deflateStateCheck` status guard (an enum can never hold an out-of-range
/// value) and lets the compiler prove every state transition is handled.
///
/// State flow (from `deflate.h`):
///
/// * [`Init`](DeflateStatus::Init) → [`Busy`](DeflateStatus::Busy) — emit the
///   zlib (RFC 1950) two-byte header, then compress.
/// * [`Gzip`](DeflateStatus::Gzip) →
///   [`Extra`](DeflateStatus::Extra)/[`Busy`](DeflateStatus::Busy) — emit the
///   gzip (RFC 1952) header, optionally followed by the extra/name/comment/HCRC
///   fields.
/// * [`Extra`](DeflateStatus::Extra) → [`Name`](DeflateStatus::Name) →
///   [`Comment`](DeflateStatus::Comment) → [`Hcrc`](DeflateStatus::Hcrc) →
///   [`Busy`](DeflateStatus::Busy) — the optional gzip header fields.
/// * [`Busy`](DeflateStatus::Busy) → [`Finish`](DeflateStatus::Finish) —
///   compression in progress, then the stream trailer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub(crate) enum DeflateStatus {
    /// `INIT_STATE` (42): about to emit the zlib wrapper header.
    Init = 42,
    /// `GZIP_STATE` (57): about to emit the gzip wrapper header.
    Gzip = 57,
    /// `EXTRA_STATE` (69): emitting the gzip header's optional extra field.
    Extra = 69,
    /// `NAME_STATE` (73): emitting the gzip header's optional file name.
    Name = 73,
    /// `COMMENT_STATE` (91): emitting the gzip header's optional comment.
    Comment = 91,
    /// `HCRC_STATE` (103): emitting the gzip header's optional CRC-16.
    Hcrc = 103,
    /// `BUSY_STATE` (113): header complete; compressing the payload.
    Busy = 113,
    /// `FINISH_STATE` (666): payload complete; emitting the stream trailer.
    Finish = 666,
}

/// `FLUSH_BLOCK_ONLY` — emit the current block and push the resulting bytes to
/// the output, **without** the output-exhaustion early return that
/// [`flush_block`] adds.
///
/// The safe-Rust port of C's `FLUSH_BLOCK_ONLY` macro (`deflate.c`):
///
/// ```c
/// #define FLUSH_BLOCK_ONLY(s, last) { \
///    _tr_flush_block(s, (s->block_start >= 0L ? \
///                    (charf *)&s->window[(unsigned)s->block_start] : \
///                    (charf *)Z_NULL), \
///                 (ulg)((long)s->strstart - s->block_start), \
///                 (last)); \
///    s->block_start = s->strstart; \
///    flush_pending(s->strm); \
/// }
/// ```
///
/// It (1) hands the just-accumulated block to
/// [`tr_flush_block`](state::DeflateState::tr_flush_block), which selects the
/// smallest of the {stored, static, dynamic} encodings and writes it into
/// `pending_buf`; (2) advances `block_start` to `strstart`, marking the start of
/// the next block; and (3) drains `pending_buf` into the caller's output buffer
/// via [`flush_pending`](state::DeflateStream::flush_pending).
///
/// `stored_len = strstart - block_start` is the length of the block's raw window
/// bytes. When `block_start >= 0` those bytes are the candidate payload for a
/// stored block; `tr_flush_block` copies them **only** if it actually selects a
/// stored block (when a stored block is no larger than the compressed
/// encodings). C passes a pointer straight into `window`; because
/// `tr_flush_block` borrows the engine state mutably, the candidate payload is
/// copied into a short-lived buffer first so the shared window slice does not
/// alias the mutably-borrowed state — the emitted bytes are byte-identical
/// either way. A negative `block_start` is the C `-1` sentinel (no pending
/// window payload) and passes `None`, exactly as C passes `Z_NULL`.
pub(crate) fn flush_block_only(s: &mut DeflateStream, last: bool) {
    let block_start = s.state.block_start;
    let stored_len = (s.state.strstart as isize - block_start) as usize;

    if block_start >= 0 {
        let start = block_start as usize;
        // `tr_flush_block` borrows `*s.state` mutably and would also need a
        // shared borrow of `s.state.window` for the stored-block payload — an
        // alias C avoids only because it uses raw pointers. Copy the candidate
        // payload out first; `tr_flush_block` consumes it solely when it selects
        // a stored block, so this keeps the emitted bytes identical to C while
        // staying in safe Rust.
        let payload = s.state.window[start..start + stored_len].to_vec();
        s.state.tr_flush_block(Some(&payload), stored_len, last);
    } else {
        // C `-1` sentinel: no window payload is available for a stored block.
        s.state.tr_flush_block(None, stored_len, last);
    }

    // Mark the start of the next block, then push the encoded bytes out.
    s.state.block_start = s.state.strstart as isize;
    s.flush_pending();
}

/// `FLUSH_BLOCK` — [`flush_block_only`] followed by the output-exhaustion early
/// return; the helper the per-level inner loops
/// ([`fast`]/[`slow`]/[`rle`]/[`huff`]) invoke at every block boundary.
///
/// The safe-Rust port of C's `FLUSH_BLOCK` macro (`deflate.c`):
///
/// ```c
/// #define FLUSH_BLOCK(s, last) { \
///    FLUSH_BLOCK_ONLY(s, last); \
///    if (s->strm->avail_out == 0) return (last) ? finish_started : need_more; \
/// }
/// ```
///
/// Rust has no macro-level `return`, so the early exit is encoded in the return
/// value: `Some(bstate)` means "C's macro would have returned `bstate`" and the
/// caller must propagate it
/// (`if let Some(bs) = flush_block(s, last) { return bs; }`), while `None` means
/// "continue". When the output buffer is full after the flush, the result is
/// [`BlockState::FinishStarted`] for the final block and
/// [`BlockState::NeedMore`] otherwise.
#[must_use]
pub(crate) fn flush_block(s: &mut DeflateStream, last: bool) -> Option<BlockState> {
    flush_block_only(s, last);
    if s.avail_out() == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}
