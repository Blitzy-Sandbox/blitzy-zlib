//! The DEFLATE compression engine, ported from `deflate.c` / `deflate.h`,
//! `trees.c` / `trees.h`.
//!
//! ## Checkpoint scope
//!
//! This module is delivered as a **foundational preview**. It currently wires
//! together three submodules:
//!
//! * [`state`] — the persistent [`state::DeflateState`] engine state plus the
//!   transient per-call I/O context and the LZ77 window/hash/match machinery
//!   (ported from `deflate.h` and the plumbing functions of `deflate.c`).
//! * [`trees`] — the Huffman tree builder and the static literal/distance
//!   trees (ported from `trees.c`).
//! * [`strategy`] — the per-level [`strategy::CompressionConfig`] table that
//!   reproduces `deflate.c`'s `configuration_table` verbatim.
//!
//! The `deflate()` driver, the public `deflateInit2_` / `deflateEnd` API, and
//! the five per-level compress functions (`deflate_stored` / `deflate_fast` /
//! `deflate_slow` / `deflate_rle` / `deflate_huff`) that orchestrate these
//! pieces land in the next checkpoint. The header-emission state machine
//! ([`DeflateStatus`]) is defined here now because the preview state struct in
//! [`state`] is typed by it.

pub mod state;
pub mod strategy;
pub mod trees;

/// The DEFLATE header-emission state machine.
///
/// This enum replaces the integer `status` field of the C `deflate_state`
/// struct and the `*_STATE` sentinel macros defined in `deflate.h`
/// (`deflate.h` L58-67). It tracks where a deflate stream is within the
/// wrapper-header emission sequence before the main DEFLATE body
/// ([`Busy`](DeflateStatus::Busy)) and after the final block
/// ([`Finish`](DeflateStatus::Finish)).
///
/// The transitions mirror the C source:
///
/// * [`Init`](DeflateStatus::Init) — a zlib header is pending →
///   [`Busy`](DeflateStatus::Busy).
/// * [`Gzip`](DeflateStatus::Gzip) — a gzip header is pending →
///   [`Busy`](DeflateStatus::Busy) (or [`Extra`](DeflateStatus::Extra) when an
///   extra field is present).
/// * [`Extra`](DeflateStatus::Extra) → [`Name`](DeflateStatus::Name) →
///   [`Comment`](DeflateStatus::Comment) → [`Hcrc`](DeflateStatus::Hcrc) —
///   the optional gzip extra/name/comment/header-CRC fields.
/// * [`Busy`](DeflateStatus::Busy) — emitting compressed blocks →
///   [`Finish`](DeflateStatus::Finish).
/// * [`Finish`](DeflateStatus::Finish) — the stream is complete.
///
/// # Discriminant values
///
/// The explicit discriminants reproduce the exact C sentinel constants. They
/// are deliberately *not* sequential: the values are part of zlib's documented
/// internal contract (for example [`Finish`](DeflateStatus::Finish) is `666`),
/// and preserving them keeps the Rust engine bit-for-bit faithful to any C
/// logic that compares against the raw sentinels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(i32)]
pub enum DeflateStatus {
    /// `INIT_STATE` (42): a zlib wrapper header is pending.
    Init = 42,
    /// `GZIP_STATE` (57): a gzip wrapper header is pending.
    Gzip = 57,
    /// `EXTRA_STATE` (69): emitting the gzip `FEXTRA` field.
    Extra = 69,
    /// `NAME_STATE` (73): emitting the gzip `FNAME` field.
    Name = 73,
    /// `COMMENT_STATE` (91): emitting the gzip `FCOMMENT` field.
    Comment = 91,
    /// `HCRC_STATE` (103): emitting the gzip header CRC-16.
    Hcrc = 103,
    /// `BUSY_STATE` (113): emitting compressed DEFLATE blocks.
    Busy = 113,
    /// `FINISH_STATE` (666): the stream is complete.
    Finish = 666,
}

impl DeflateStatus {
    /// Returns the raw C sentinel value backing this status.
    ///
    /// Useful at the FFI boundary and for diagnostics that need to match the
    /// integer `*_STATE` constants from `deflate.h`.
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}
