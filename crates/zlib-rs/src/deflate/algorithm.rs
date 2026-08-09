//! Module root for the five DEFLATE compression strategies, and the vocabulary they share.
//!
//! `deflate.c` reaches its compressors through one function pointer -- `compress_func`
//! (L70-L71) -- and five file-static implementations selected by level and strategy
//! (L73-L79). This module is the Rust root of that subtree: the five implementations live
//! in `deflate/algorithm/` beside this file, one per leaf, and everything the driver and
//! the leaves must agree on lives here.
//!
//! # Why this file exists, and why `algorithm/mod.rs` must not
//!
//! `crates/zlib-rs/src/deflate/mod.rs` declares `mod algorithm;`. Rust resolves that to
//! **either** `deflate/algorithm.rs` **or** `deflate/algorithm/mod.rs`, never both --
//! finding both is the hard error `E0761`, "file for module `algorithm` found at both". This
//! file is the authoritative one, in the standard Rust 2018 layout where a module file sits
//! beside the directory holding its children. If an `algorithm/mod.rs` ever appears, that is
//! the file to delete.
//!
//! # What belongs here, and what does not
//!
//! Only the shared dispatch vocabulary and the shared block-flush helper. No compression
//! logic: each strategy is a whole algorithm with its own byte-identity hazards and lives
//! in its own leaf file. What is here is precisely the set of definitions that more than one
//! leaf -- or a leaf and the driver -- has to agree on, because a second, subtly different
//! copy of any of them would change *when* blocks are emitted, and therefore the emitted
//! bytes.
//!
//! | C construct | `deflate.c` | Here |
//! |---|---|---|
//! | `block_state` enum | L63-L68 | [`BlockState`] |
//! | `compress_func` typedef | L70-L71 | [`CompressFunc`] |
//! | the five `local` compressors | L73-L79 | [`fast`], [`huff`], [`rle`], [`slow`], [`stored`] |
//! | `RANK(f)` | L132-L133 | [`Flush::rank`], [`rank_of_i32`] |
//! | the dispatch chain in `deflate()` | L1217-L1220 | [`CompressFunc::select`] |
//! | `FLUSH_BLOCK_ONLY(s, last)` | L1630-L1640 | [`flush_block_only`] |
//! | `FLUSH_BLOCK(s, last)` | L1642-L1646 | [`flush_block_exit`] and [`flush_block!`] |
//! | `MAX_STORED` | L1647-L1648 | [`MAX_STORED`] |
//! | `MIN(a, b)` | L1650-L1651 | not implemented -- [`core::cmp::min`] |
//!
//! # `s->strm` is a parameter here, not a field
//!
//! Every C compressor reaches the caller's buffers through `s->strm`, a back-pointer from
//! the compression state to the `z_stream` that owns it. This implementation cannot hold one: a
//! `&mut z_stream` inside [`DeflateState`] would alias the `&mut DeflateState` that every
//! function takes, which safe Rust rejects outright, and `deflate/state.rs` accordingly
//! declares no `strm` field. Everything the C code reaches through it therefore travels as
//! [`StreamCursors`], which is threaded to the leaves alongside the state.
//!
//! That is one parameter rather than six because `clippy::too_many_arguments` -- denied
//! workspace-wide, with the threshold in `clippy.toml` at 8 -- would reject the flattened
//! form once a leaf added its own arguments, and because a single bundle is what keeps the
//! five leaves, `deflate/config_table.rs` and `deflate/mod.rs` on one signature.
//!
//! # Two deliberate departures from the C shape
//!
//! Both are recorded on the items themselves; in summary:
//!
//! * **[`Flush`] is re-exported from [`crate::config`], not redefined here.** The flush
//!   selector is shared with `inflate`, which accepts `Z_TREES` where `deflate` does not, so
//!   it is declared once beside the other `zlib.h` parameter types. `Z_TREES` is excluded
//!   from compression where C excludes it -- at the driver's front door (L985) -- rather
//!   than by omitting a variant.
//! * **[`CompressFunc`] is an enum, not a function pointer.** [`DeflateState`] is generic
//!   over its allocator, so no single monomorphic `fn` pointer can name a compressor, and
//!   `deflate/config_table.rs` needs a value it can put in a plain `const` array. See
//!   [`CompressFunc`] for why the grouping and the equality test are preserved exactly.
//!
//! # What is deliberately not implemented
//!
//! Only the default compile-time configuration, and none of these is offered as a runtime
//! option or a Cargo feature. `FASTEST` (`deflate.c` L106 and L76) replaces the ten-entry
//! configuration table with two entries and drops `deflate_slow` altogether; this implementation
//! always has all five compressors and all ten levels. `ZLIB_DEBUG` accounting -- the
//! `Tracev((stderr,"[FLUSH]"))` at L1639 and the `compressed_len`/`bits_sent` counters --
//! has no counterpart, so the reference's traces appear neither as output nor as state.

// The five compressors of `deflate.c` L73-L79, one module each, in the `algorithm/` directory
// beside this file. Alphabetical so that `cargo fmt`'s module reordering is a fixed point; the
// order carries no meaning, since the level and the strategy pick the compressor
// ([`CompressFunc::select`]). Each is `pub(crate)`: all five are `local` in C and none appears
// in `zlib.map`, so none may become a crate export. Their own `//!` documentation, in their own
// files, is what describes each algorithm -- there is deliberately no `///` here, which would
// merely duplicate it.
pub(crate) mod fast;
pub(crate) mod huff;
pub(crate) mod rle;
pub(crate) mod slow;
pub(crate) mod stored;

use core::mem::size_of;

// Where each of these comes from, since none of it is a choice:
//
// * `Allocator`, `DeflateState` and `Strategy` are `deflate/state.rs`'s own re-exports;
//   `DeflateState<'a, A: Allocator<'a>>` cannot be named without the bound, and `Strategy` is
//   what `CompressFunc::select` dispatches on.
// * `flush_pending` is step 3 of `FLUSH_BLOCK_ONLY`, and `_tr_flush_block` is step 1.
//   `flush_bits` is `crate::trees::_tr_flush_bits`, which `flush_pending` takes as an argument
//   rather than importing -- see `deflate/pending.rs` on why.
// * `InputCursor` and `OutputCursor` arrive through those same signatures, not as a separate
//   dependency: `flush_pending` takes `&mut OutputCursor` and `deflate/window.rs`'s
//   `fill_window` takes `&mut InputCursor`, so the two cursor types are already part of the
//   contract this module has to satisfy. Declaring look-alikes here instead would give the
//   crate two incompatible cursor types over one buffer.
use crate::deflate::pending::flush_pending;
use crate::deflate::state::{Allocator, DeflateState, Strategy};
use crate::read_buf::{InputCursor, OutputCursor};
use crate::trees::{_tr_flush_block, flush_bits};

/// The flush selector `deflate()` and every compressor take as their second argument.
///
/// Re-exported rather than redeclared. `crate::config::Flush` is the single definition of
/// `zlib.h`'s seven flush values for the whole crate, because `inflate` takes the same
/// argument and treats `Z_TREES` as meaningful while `deflate` rejects it; declaring a
/// second, `Trees`-less enum here would give the crate two incompatible types called
/// `Flush`, and `crate::config::validate_deflate_flush`, whose return type is the shared
/// one, could not feed it.
///
/// `Z_TREES` never reaches a compressor, and it is kept out exactly where C keeps it out:
/// `deflate()` rejects `flush > Z_BLOCK || flush < 0` before dispatching (`deflate.c` L985),
/// which is [`Flush::is_valid_for_deflate`] and `crate::config::validate_deflate_flush`. A
/// leaf therefore never has to consider it, and `CompressFunc::call` asserts as much in debug
/// builds. (Named without a doc link on purpose: this re-export is `pub` and
/// `CompressFunc` is crate-private, which `rustdoc::private_intra_doc_links` rejects.)
///
/// `deflate/mod.rs` re-exports this again as `crate::deflate::Flush`, which is the path
/// `crates/zlib-rs/src/compress.rs`, `crates/zlib-rs/src/gz/write.rs` and
/// `crates/libz-rs-sys/src/deflate.rs` use. That second re-export is why this one is `pub`
/// and not `pub(crate)`: a `pub use` of a crate-public item is `E0365`, "`Flush` is only
/// public within the crate, and cannot be re-exported outside". Nothing escapes as a result,
/// because `deflate/mod.rs` declares this module itself as `pub(crate) mod algorithm;`.
pub use crate::config::Flush;

// The five compressors, under the names `deflate.c` gives them (L73-L79), so that a reader
// can grep from either language to the other. They are `pub(crate)` and not `pub`: all five
// are `local` in C and none appears in `zlib.map`, so none may become a crate export.
pub(crate) use crate::deflate::algorithm::fast::deflate_fast;
pub(crate) use crate::deflate::algorithm::huff::deflate_huff;
pub(crate) use crate::deflate::algorithm::rle::deflate_rle;
pub(crate) use crate::deflate::algorithm::slow::deflate_slow;
pub(crate) use crate::deflate::algorithm::stored::deflate_stored;

/// [`flush_block_only`] converts a byte count to the `u64` that
/// [`crate::trees::_tr_flush_block`] takes, and this implementation's standard forbids a cast that
/// could truncate.
///
/// `crate::deflate::pending` and `crate::read_buf` assert the same relationship for the same
/// reason, at the same cost of nothing: on a hypothetical target with a `usize` wider than
/// `u64` this crate fails to compile rather than silently mis-sizing a block.
const _: () = assert!(
    size_of::<usize>() <= size_of::<u64>(),
    "flush_block_only converts a usize byte count to u64; usize must not be the wider type"
);

/// Maximum stored block length in deflate format, not including the header.
///
/// `#define MAX_STORED 65535` (`deflate.c` L1647-L1648). It is the largest value the
/// `LEN`/`NLEN` pair of a stored block can express, since both are 16-bit little-endian
/// fields (`doc/rfc1951.txt` section 3.2.4), and `deflate_stored` clamps every direct copy
/// to it (`deflate.c` L1687 and L1830).
///
/// `usize` because C assigns it to `unsigned len` and then compares that against
/// `avail_in`, `avail_out` and `strstart - block_start`, all of which are `usize` in this
/// implementation; a narrower type would put a cast on every one of those comparisons.
pub(crate) const MAX_STORED: usize = 65535;

/// How a compressor finished: the state of the block it was working on.
///
/// Mirrors `block_state` (`deflate.c` L63-L68), variant for variant and in the same
/// order. The four values are not interchangeable status codes -- each one is a specific
/// instruction to `deflate()` about what to do next, and returning the wrong one at the
/// wrong moment changes where block boundaries fall. See the section below for what the
/// driver does with each.
///
/// # How `deflate()` consumes this (`deflate.c` L1222-L1260)
///
/// The driver applies three tests, in this order, to the value a compressor returned:
///
/// 1. `if (bstate == finish_started || bstate == finish_done) s->status = FINISH_STATE;`
///    (L1222-L1224). Both "finish" variants move the stream into `FINISH_STATE`, after which
///    `deflate()` rejects any flush other than `Z_FINISH` and any further input.
/// 2. `if (bstate == need_more || bstate == finish_started) { ... return Z_OK; }`
///    (L1225-L1237). Both "more output needed" variants return `Z_OK` immediately, and set
///    `last_flush = -1` first if `avail_out` has reached zero, so that the next call is not
///    met with `Z_BUF_ERROR` by the duplicate-flush test. Note that
///    [`FinishStarted`](Self::FinishStarted) satisfies *both* this test and the one above:
///    it enters `FINISH_STATE` **and** returns `Z_OK`.
/// 3. `if (bstate == block_done) { ... }` (L1238-L1259). Only then does the driver emit the
///    per-flush-mode block terminator: `_tr_align(s)` for `Z_PARTIAL_FLUSH`, an empty
///    `_tr_stored_block(s, NULL, 0, 0)` for `Z_SYNC_FLUSH` and `Z_FULL_FLUSH`, and nothing
///    at all for `Z_BLOCK`. `Z_FULL_FLUSH` additionally clears the hash chains and, when
///    `lookahead` is zero, rewinds `strstart`, `block_start` and `insert` to zero.
///
/// [`FinishDone`](Self::FinishDone) matches only the first test, so it neither returns early
/// nor emits a terminator: the driver falls through to the trailer.
///
/// # Why an enum rather than an integer
///
/// The four values are exhaustive and disjoint, so a `match` over them is checked: adding a
/// fifth outcome would turn every consumer into a compile error rather than a silent
/// fall-through. That is the same reason `inflate`'s 32-state mode is an enum here -- 32 is the
/// variant count of `inflate_mode` in `inflate.h`, reproduced exactly by
/// [`crate::inflate::mode::Mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockState {
    /// `need_more` -- "block not completed, need more input or more output"
    /// (`deflate.c` L64).
    NeedMore,

    /// `block_done` -- "block flush performed" (`deflate.c` L65).
    BlockDone,

    /// `finish_started` -- "finish started, need only more output at next deflate"
    /// (`deflate.c` L66).
    FinishStarted,

    /// `finish_done` -- "finish done, accept no more input or output" (`deflate.c` L67).
    FinishDone,
}

/// `RANK(f)` over a raw `int` flush value (`deflate.c` L132-L133).
///
/// ```text
/// /* rank Z_BLOCK between Z_NO_FLUSH and Z_PARTIAL_FLUSH */
/// #define RANK(f) (((f) * 2) - ((f) > 4 ? 9 : 0))
/// ```
///
/// The macro exists to reorder one value. Doubling spaces the flush modes out, and
/// subtracting 9 from anything above `Z_FINISH` pulls `Z_BLOCK` (5) down to 1, which places
/// it *between* `Z_NO_FLUSH` (0) and `Z_PARTIAL_FLUSH` (2) instead of above `Z_FINISH`:
///
/// | flush | value | rank |
/// |---|---|---|
/// | `Z_NO_FLUSH` | 0 | 0 |
/// | `Z_BLOCK` | 5 | 1 |
/// | `Z_PARTIAL_FLUSH` | 1 | 2 |
/// | `Z_TREES` | 6 | 3 |
/// | `Z_SYNC_FLUSH` | 2 | 4 |
/// | `Z_FULL_FLUSH` | 3 | 6 |
/// | `Z_FINISH` | 4 | 8 |
///
/// # Why a raw `i32` and not a [`Flush`]
///
/// Because C applies the macro to `s->last_flush`, which is **not** a flush value in general.
/// `deflate.h` L115 declares it `int`, and `deflate.c` stores two out-of-band sentinels in
/// it: `-1` means "the previous call ran out of output space", written at ten places in
/// `deflate()` (L1010, L1061, L1087, L1130, L1153, L1175, L1192, L1205, L1227, L1257), and
/// `-2` means "freshly reset", written by `deflateResetKeep` (L672) and tested by
/// `deflateParams` (L792). `deflate/state.rs` holds the field as `i32` for exactly that
/// reason.
///
/// The duplicate-flush guard therefore compares a real rank against a sentinel's rank:
///
/// ```text
/// } else if (strm->avail_in == 0 && RANK(flush) <= RANK(old_flush) &&
///            flush != Z_FINISH) {
///     ERR_RETURN(strm, Z_BUF_ERROR);
/// ```
///
/// -- `deflate.c` L1018-L1020, which in this implementation is
/// `flush.rank() <= rank_of_i32(old_flush)`. The sentinels work because the arithmetic puts
/// them below every real rank: `RANK(-1)` is `-2` and `RANK(-2)` is `-4`, so a call that
/// follows an out-of-output return or a reset can never be mistaken for a repeat of the same
/// flush. That is reproduced by computing the same expression, not by special-casing the two
/// values.
///
/// # Total, and panic-free
///
/// The two operations wrap rather than being checked. On the reachable domain -- `-2..=6` --
/// wrapping and exact arithmetic agree exactly; beyond it, C's `int` multiplication overflows
/// into undefined behaviour, and wrapping is both the closest well-defined analogue and what
/// the reference compiler actually emits. Written with `*` and `-` this function would panic
/// in a debug build on inputs that C merely mis-handles, and library code in this crate does
/// not panic.
#[inline]
#[must_use]
pub(crate) const fn rank_of_i32(raw: i32) -> i32 {
    // `((f) * 2) - ((f) > 4 ? 9 : 0)`
    raw.wrapping_mul(2)
        .wrapping_sub(if raw > 4 { 9 } else { 0 })
}

impl Flush {
    /// `RANK(f)` for a flush value that is known to name one of the seven modes.
    ///
    /// The typed half of [`rank_of_i32`], which is where the arithmetic and the reasoning
    /// live. Defined in this module rather than beside [`Flush`] itself because `RANK` is a
    /// `deflate.c` macro (L132-L133) and has no meaning for decompression: `inflate` never
    /// compares flush values.
    ///
    /// Use this for the incoming `flush` argument and [`rank_of_i32`] for
    /// `DeflateState::last_flush`, which may hold a sentinel that no [`Flush`] can represent.
    #[inline]
    #[must_use]
    pub(crate) const fn rank(self) -> i32 {
        rank_of_i32(self.as_raw())
    }
}

/// The parts of the caller's `z_stream` that a compressor reads and writes.
///
/// C's compressors reach all of this through `s->strm` (`deflate.h` L105). This implementation has no
/// such back-pointer -- see the module documentation for why it cannot -- so the same six
/// things travel as one value alongside the `&mut DeflateState`.
///
/// | Field | `z_stream` | `zlib.h` |
/// |---|---|---|
/// | [`input`](Self::input) | `next_in` and `avail_in`, as one cursor | L91-L92 |
/// | [`output`](Self::output) | `next_out` and `avail_out`, as one cursor | L94-L95 |
/// | [`check`](Self::check) | `adler` -- the running Adler-32 or CRC-32 | L106 |
/// | [`total_in`](Self::total_in) | `total_in` | L93 |
/// | [`total_out`](Self::total_out) | `total_out` | L96 |
/// | [`data_type`](Self::data_type) | `data_type` | L105 |
///
/// `msg` is absent: no compressor writes it. Only `deflate()` does, through `ERR_RETURN`, so
/// it belongs to the driver.
///
/// # Every field is written by the caller, and there is no constructor
///
/// Deliberately: the driver builds this with a struct literal, which the compiler will not
/// let it write without naming all six fields. A `new` taking only the two buffers would have
/// to guess the other four, and one of them cannot be guessed --
/// [`check`](Self::check) is `adler32(0, Z_NULL, 0)` (1) for a zlib stream and
/// `crc32(0, Z_NULL, 0)` (0) for a gzip one, per `deflateResetKeep` (`deflate.c` L663-L668),
/// so a default would silently corrupt one of the two containers. Requiring the literal turns
/// that into an omission the compiler catches.
///
/// # Borrowing
///
/// The fields are separate so that the existing helpers can be called unchanged: Rust borrows
/// struct fields disjointly, so
/// `fill_window(state, &mut cursors.input, &mut cursors.check, &mut cursors.total_in)` and
/// `read_buf_into_output(&mut cursors.input, &mut cursors.output, ..)` both type-check without
/// splitting the bundle apart.
#[derive(Debug)]
pub(crate) struct StreamCursors<'i, 'o> {
    /// The caller's whole input buffer with a position in it: `next_in` plus `avail_in`.
    ///
    /// The whole buffer, not the unread tail, because `deflate_stored` rebuilds the window
    /// history by reading *backwards* from `next_in` (`deflate.c` L1766 and L1780) --
    /// `InputCursor::consumed_tail` is that read.
    pub(crate) input: InputCursor<'i>,

    /// The caller's whole output buffer with a position in it: `next_out` plus `avail_out`.
    pub(crate) output: OutputCursor<'o>,

    /// `z_stream.adler`: the running check value (`zlib.h` L106).
    ///
    /// Which checksum this is follows from `DeflateState::wrap`, which
    /// [`crate::read_buf::read_buf`] consults on every call; a raw stream leaves it alone.
    pub(crate) check: u32,

    /// `z_stream.total_in`: total input bytes consumed so far (`zlib.h` L93).
    pub(crate) total_in: u64,

    /// `z_stream.total_out`: total output bytes produced so far (`zlib.h` L96).
    pub(crate) total_out: u64,

    /// `z_stream.data_type`: `Z_BINARY`, `Z_TEXT` or `Z_UNKNOWN` (`zlib.h` L105).
    ///
    /// `_tr_flush_block` fills it in on the first block of a stream, when it still holds
    /// `Z_UNKNOWN` (`deflate.c` L1005-L1007), and it is observable afterwards: both
    /// `test/example.c` and the differential suite read it back.
    pub(crate) data_type: i32,
}

impl StreamCursors<'_, '_> {
    /// `strm->avail_in`: input bytes not yet consumed (`zlib.h` L92).
    ///
    /// Every compressor tests it -- `deflate_stored` at `deflate.c` L1682, `deflate_fast` at
    /// L1869, `deflate_slow` at L1969, `deflate_rle` at L2096 and `deflate_huff` at L2162 --
    /// so it is named here once rather than spelled out at each site.
    #[inline]
    #[must_use]
    pub(crate) fn avail_in(&self) -> usize {
        self.input.remaining()
    }

    /// `strm->avail_out`: output space not yet written (`zlib.h` L95).
    ///
    /// This is the value `FLUSH_BLOCK` tests to decide on a premature exit
    /// (`deflate.c` L1644), which is [`flush_block_exit`].
    #[inline]
    #[must_use]
    pub(crate) fn avail_out(&self) -> usize {
        self.output.remaining()
    }
}

/// Which of the five compressors to run.
///
/// Mirrors `compress_func` (`deflate.c` L70-L71):
///
/// ```text
/// typedef block_state (*compress_func)(deflate_state *s, int flush);
/// /* Compression function. Returns the block state after the call. */
/// ```
///
/// # Why an enum where C has a function pointer
///
/// [`DeflateState`] is generic over its allocator, `DeflateState<'a, A: Allocator<'a>>`, so a
/// compressor is a *generic* function and there is no single monomorphic `fn` pointer that
/// names one. `deflate/config_table.rs` needs a value it can store in a plain
/// `const [Config; 10]`, and a function-pointer field would force that table -- and the
/// `Config` struct with it -- to carry the lifetime and allocator parameters and a
/// `PhantomData`, which is a large amount of machinery to reproduce a C detail that nothing
/// observes. A five-variant enum is `Copy`, `Eq`, `const`-constructible and exhaustively
/// matched.
///
/// # The two properties that must be preserved, and are
///
/// * **The grouping.** `configuration_table` (`deflate.c` L112-L124) holds exactly three
///   distinct function addresses: `deflate_stored` at level 0, `deflate_fast` at levels 1 to
///   3, and `deflate_slow` at levels 4 to 9. `deflate/config_table.rs` must map levels to
///   [`Stored`](Self::Stored), [`Fast`](Self::Fast) and [`Slow`](Self::Slow) on exactly those
///   boundaries. Note that `deflate_rle` and `deflate_huff` appear nowhere in the table: they
///   are reachable only through the strategy, in [`select`](Self::select).
/// * **The equality test.** `deflateParams` decides whether a level change has to flush the
///   current block by comparing the two table entries *by identity*
///   (`deflate.c` L789-L792).
///
///   Comparing addresses and comparing discriminants agree *because* the grouping above is
///   the same: two levels compare equal here precisely when they name the same C function.
///   Getting the grouping wrong would make `deflateParams` flush where C does not -- or fail
///   to flush where C does -- and either changes the emitted bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressFunc {
    /// `deflate_stored` (`deflate.c` L1668) -- store only; the level-0 compressor.
    Stored,

    /// `deflate_fast` (`deflate.c` L1857) -- greedy matching, no lazy evaluation.
    Fast,

    /// `deflate_slow` (`deflate.c` L1956) -- lazy match evaluation.
    Slow,

    /// `deflate_rle` (`deflate.c` L2084) -- match distances limited to one.
    Rle,

    /// `deflate_huff` (`deflate.c` L2155) -- Huffman coding only, no string matching.
    Huff,
}

impl CompressFunc {
    /// The compressor `deflate()` runs for a given level, strategy and table entry.
    ///
    /// Mirrors the conditional chain at `deflate.c` L1217-L1220.
    ///
    /// **The order is the contract.** Level 0 is tested *first*, so a level-0 stream stores
    /// its input even when the strategy is `Z_HUFFMAN_ONLY` or `Z_RLE`; only then does the
    /// strategy get a say, and only if it is neither of those two does the level's table entry
    /// decide. Reordering the tests would silently change the encoder for two parameter
    /// combinations that `deflateInit2_` accepts and the differential matrix exercises.
    ///
    /// `from_table` is `CONFIGURATION_TABLE[level].func`, which `deflate/config_table.rs`
    /// owns. It is a parameter rather than a lookup here so that this module stays independent
    /// of the table, and because the C code likewise evaluates the table entry in the caller.
    ///
    /// `level` is an `i32` because that is how `deflate/state.rs` holds it, matching C's
    /// `int level` (`deflate.h` L117); the range `0..=9` was established by
    /// `crate::config::normalize_deflate_level` when the stream was configured.
    #[inline]
    #[must_use]
    pub(crate) const fn select(level: i32, strategy: Strategy, from_table: Self) -> Self {
        // `matches!` rather than `==` only because `PartialEq::eq` is not a `const fn`; the
        // comparison it performs is the same one.
        if level == 0 {
            Self::Stored
        } else if matches!(strategy, Strategy::HuffmanOnly) {
            Self::Huff
        } else if matches!(strategy, Strategy::Rle) {
            Self::Rle
        } else {
            from_table
        }
    }

    /// Runs this compressor: the C call through the `compress_func` pointer.
    ///
    /// The five arms are the five call sites C reaches through one indirect call
    /// (`deflate.c` L1217-L1220). Dispatch is a `match` on an exhaustive enum, so a sixth
    /// compressor could not be added without updating this function.
    #[inline]
    pub(crate) fn call<'a, A: Allocator<'a>>(
        self,
        state: &mut DeflateState<'a, A>,
        cursors: &mut StreamCursors<'_, '_>,
        flush: Flush,
    ) -> BlockState {
        // `deflate()` rejects `flush > Z_BLOCK` before it dispatches (`deflate.c` L985), so
        // `Z_TREES` cannot reach a compressor. Stated here as well because the leaves rely on
        // it: none of them has a `Trees` arm, and none should grow one.
        debug_assert!(
            flush.is_valid_for_deflate(),
            "Z_TREES reached a compressor; deflate() rejects it at deflate.c L985"
        );

        match self {
            Self::Stored => deflate_stored(state, cursors, flush),
            Self::Fast => deflate_fast(state, cursors, flush),
            Self::Slow => deflate_slow(state, cursors, flush),
            Self::Rle => deflate_rle(state, cursors, flush),
            Self::Huff => deflate_huff(state, cursors, flush),
        }
    }
}

/// Flush the current block, with the given end-of-file flag.
///
/// Mirrors `FLUSH_BLOCK_ONLY` (`deflate.c` L1630-L1640).
///
/// `IN assertion: strstart is set to the end of the current match` (L1628).
///
/// All five compressors use this, three of them only through [`flush_block!`]. It is defined
/// once rather than five times because it decides where block boundaries fall: five copies
/// would be five chances for one of them to drift, and a block boundary in the wrong place is
/// a different compressed stream.
///
/// # The three steps, in order
///
/// 1. **`_tr_flush_block`** with the block's bytes, its length and the `BFINAL` bit. The
///    buffer selector is C's conditional expression: `usize::try_from` on the signed
///    `block_start` fails for exactly the negative values, so it *is* the `block_start >= 0L`
///    test, and [`None`] is C's `Z_NULL`. A negative `block_start` is a legitimate state --
///    it "gets negative when the window is moved backwards" (`deflate.h` L159-L161) -- and
///    `crate::trees::_tr_flush_block` treats the absent buffer as making a stored block
///    ineligible, which is what C's null pointer does at `trees.c` L1047.
/// 2. **`s->block_start = s->strstart`**, opening the next block where this one ended. It has
///    to follow the call, because `_tr_flush_block` is still describing the block that is
///    being closed.
/// 3. **`flush_pending`**, moving as much of the newly written block as fits into the
///    caller's output buffer. This is what makes the `avail_out == 0` test in
///    [`flush_block_exit`] meaningful, and it is the reason `flush_pending` is called here
///    rather than left to the driver.
///
/// The `Tracev` at L1639 is `ZLIB_DEBUG`-only and has no counterpart.
// `_tr_flush_block` keeps the underscore-prefixed C spelling of `deflate.h` L313-L314 for
// oracle traceability, which `clippy::pedantic` flags at the call site. The same relaxation,
// for the same reason, appears in `trees/mod.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items)]
#[inline]
pub(crate) fn flush_block_only<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    last: bool,
) {
    // `s->block_start >= 0L ? (charf *)&s->window[(unsigned)s->block_start] : (charf *)Z_NULL`
    // (L1631-L1633), carried as a window offset rather than a pointer.
    let buf = usize::try_from(state.window.block_start).ok();

    // `(ulg)((long)s->strstart - s->block_start)`  (L1634)
    //
    // C relies on the subtraction being non-negative; where it is not, the cast to `ulg`
    // would produce an astronomically large length and read far past the window. This implementation
    // declines the block instead: `bytes_since_block_start` reports [`None`] for exactly that
    // case, and a zero length emits an empty block, which is safe and observable rather than
    // undefined. The `debug_assert!` records that no caller is expected to get there --
    // `block_start` only ever moves to `strstart` or down by a whole window.
    let stored_len = state.window.bytes_since_block_start();
    debug_assert!(
        stored_len.is_some(),
        "flush_block_only with block_start ahead of strstart (deflate.c L1634)"
    );
    // Widening guarded by the compile-time assertion at the top of this module, so the cast
    // cannot truncate.
    let stored_len = stored_len.unwrap_or(0) as u64;

    let strstart = state.window.strstart;

    // `_tr_flush_block(s, ..., (last));`  (L1631-L1636)
    //
    // `data_type` is C's `s->strm->data_type`, which `_tr_flush_block` fills in on the first
    // block of a stream; see the module documentation on why it is threaded rather than
    // reached through a back-pointer.
    _tr_flush_block(state, &mut cursors.data_type, buf, stored_len, last);

    // `s->block_start = s->strstart;`  (L1637)
    //
    // `strstart` never exceeds `window_size`, at most 65536, so the conversion is exact on
    // every target this crate builds for. Saturating rather than unwrapping keeps the
    // function total, as the same conversion does in `deflate/window.rs`; the saturated value
    // would leave `bytes_since_block_start` reporting nothing further to flush, so the next
    // block would be empty rather than wrong.
    state.window.block_start = isize::try_from(strstart).unwrap_or(isize::MAX);

    // `flush_pending(s->strm);`  (L1638)
    //
    // `crate::trees::flush_bits` is `_tr_flush_bits`, the step `flush_pending` performs
    // first; it is a parameter there rather than an import, and there is exactly one correct
    // value for it in the library.
    let _flushed = flush_pending(
        state,
        &mut cursors.output,
        &mut cursors.total_out,
        flush_bits,
    );
}

/// [`flush_block_only`], then the premature-exit decision `FLUSH_BLOCK` adds.
///
/// Mirrors `FLUSH_BLOCK` (`deflate.c` L1642-L1646):
///
/// ```text
/// /* Same but force premature exit if necessary. */
/// #define FLUSH_BLOCK(s, last) { \
///    FLUSH_BLOCK_ONLY(s, last); \
///    if (s->strm->avail_out == 0) return (last) ? finish_started : need_more; \
/// }
/// ```
///
/// [`Some`] means the caller must return that [`BlockState`] immediately; [`None`] means it
/// carries on. The `return` itself cannot live in a function -- C's macro returns from the
/// function that expanded it -- so it is supplied by [`flush_block!`], which is what every
/// compressor should use. This function exists so that the macro contains nothing but the
/// `return`, which keeps the whole of `FLUSH_BLOCK`'s behaviour type-checked in one place.
///
/// # `FLUSH_BLOCK_ONLY` and `FLUSH_BLOCK` are not interchangeable
///
/// `deflate_slow`'s literal branch (`deflate.c` L2040-L2051) deliberately uses the *plain*
/// `FLUSH_BLOCK_ONLY`.
///
/// It flushes, *then* advances `strstart` and `lookahead`, and only then gives up on a full
/// output buffer. Substituting `FLUSH_BLOCK` there would return before the two cursors moved,
/// so the next call would re-emit the literal at `strstart - 1`. Collapsing the two spellings
/// changes the encoder's state at a block boundary; that branch must call
/// [`flush_block_only`] directly.
#[inline]
#[must_use]
pub(crate) fn flush_block_exit<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    last: bool,
) -> Option<BlockState> {
    // `FLUSH_BLOCK_ONLY(s, last);`  (L1643)
    flush_block_only(state, cursors, last);

    // `if (s->strm->avail_out == 0) return (last) ? finish_started : need_more;`  (L1644)
    if cursors.avail_out() == 0 {
        Some(if last {
            BlockState::FinishStarted
        } else {
            BlockState::NeedMore
        })
    } else {
        None
    }
}

/// `FLUSH_BLOCK(s, last)` (`deflate.c` L1642-L1646), including its early `return`.
///
/// Expands to a call to [`flush_block_exit`] followed by the `return` that C's macro performs
/// on behalf of its caller. That `return` is the whole reason this is a macro: a function
/// cannot return from the function that called it.
///
/// # Only valid inside a function returning [`BlockState`]
///
/// The expansion returns a [`BlockState`], so it may appear only in the body of a compressor.
/// It is not valid in `deflate/mod.rs`, whose entry points return
/// [`ReturnCode`](crate::error::ReturnCode), and it is not valid in `deflate_slow`'s literal
/// branch, which must call [`flush_block_only`] directly -- see [`flush_block_exit`] for why.
///
/// # Arguments
///
/// `$state` is the `&mut DeflateState`, `$cursors` the `&mut StreamCursors` and `$last` the
/// `BFINAL` bit as a `bool` -- C's `0` and `1` become `false` and `true`. Each is expanded
/// exactly once, so passing an expression with a side effect evaluates it once, as C's macro
/// does not guarantee.
///
/// The shape every call site takes, standing in for `if (bflush) FLUSH_BLOCK(s, 0);`
/// (`deflate.c` L1930). It is shown rather than compiled because the macro is crate-private
/// and is expanded only inside the five compressors, each of which already holds `state`,
/// `cursors` and a `bflush` of its own:
///
/// ```text
/// if bflush {
///     flush_block!(state, cursors, false);
/// }
/// ```
macro_rules! flush_block {
    ($state:expr, $cursors:expr, $last:expr) => {
        if let Some(block_state) =
            $crate::deflate::algorithm::flush_block_exit($state, $cursors, $last)
        {
            return block_state;
        }
    };
}

// A `macro_rules!` macro is otherwise reachable only in the textual scope that follows its
// definition, which would leave it invisible to the five modules declared above this file's
// body. This re-export gives it a path, so a leaf writes
// `use crate::deflate::algorithm::flush_block;` like any other item. It is `pub(crate)` and not
// `#[macro_export]` deliberately: `#[macro_export]` would put it at the crate root as a public
// macro, and `FLUSH_BLOCK` is an implementation detail of `deflate.c`.
pub(crate) use flush_block;

#[cfg(test)]
// `unwrap`, `expect` and `panic` are already relaxed inside tests by `clippy.toml`;
// `used_underscore_items` is not, and these tests call `_tr_init` and `_tr_tally`, whose names
// are the C spellings of `deflate.h` L311-L317. `unused_qualifications` is relaxed because
// `flush_block!` expands to a fully qualified `$crate::` path -- correct at every real call
// site, where nothing is imported -- and this module imports the same item to test it
// directly.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items, unused_qualifications)]
mod tests {
    use super::{
        flush_block_exit, flush_block_only, rank_of_i32, BlockState, CompressFunc, Flush,
        StreamCursors, MAX_STORED,
    };
    use crate::config::{
        validate_deflate_flush, Z_BLOCK, Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH, Z_PARTIAL_FLUSH,
        Z_SYNC_FLUSH, Z_TREES, Z_UNKNOWN,
    };
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL,
    };
    use crate::error::ReturnCode;
    use crate::read_buf::{InputCursor, OutputCursor};
    use crate::trees::{_tr_init, _tr_tally};

    /// A stream configured as `deflateInit2(&strm, 6, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY)`
    /// and wired up by `_tr_init`, which is the state a compressor is first entered with.
    ///
    /// Raw DEFLATE (negative `windowBits`) so that nothing but the block itself reaches the
    /// output, and the default memory level, so `lit_bufsize` is 16384. In a debug build every
    /// block the allocator hands back is filled with `0xa5`, the byte `test/infcover.c` L87
    /// uses, so nothing below can pass by accident on zeroed memory.
    fn new_state() -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: -15,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        state
    }

    /// Cursors over no input and `output`, with the scalars a freshly reset raw stream carries:
    /// `total_in`/`total_out` at zero, no check value, and `data_type` at `Z_UNKNOWN`
    /// (`deflate.c` L663-L668).
    fn new_cursors(output: &mut [u8]) -> StreamCursors<'static, '_> {
        StreamCursors {
            input: InputCursor::new(&[]),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// Tallies `bytes` as literals and points the window and `strstart` at them, so that the
    /// state describes a real block of that length starting at `block_start` 0.
    fn tally_literals(state: &mut DeflateState<'static, GlobalAllocator>, bytes: &[u8]) {
        assert!(state.window.write_at(0, bytes));
        for &byte in bytes {
            // `_tr_tally` reports whether the symbol buffer filled; irrelevant here, since the
            // fixtures are far shorter than `sym_end`.
            let _full = _tr_tally(state, 0, byte);
        }
        state.window.strstart = bytes.len();
        state.window.block_start = 0;
    }

    /// Every flush value against `RANK(f) = ((f) * 2) - ((f) > 4 ? 9 : 0)`, computed by hand.
    #[test]
    fn rank_matches_the_c_macro_for_every_flush_value() {
        const EXPECTED: [(Flush, i32, i32); 7] = [
            (Flush::NoFlush, Z_NO_FLUSH, 0),
            (Flush::PartialFlush, Z_PARTIAL_FLUSH, 2),
            (Flush::SyncFlush, Z_SYNC_FLUSH, 4),
            (Flush::FullFlush, Z_FULL_FLUSH, 6),
            (Flush::Finish, Z_FINISH, 8),
            (Flush::Block, Z_BLOCK, 1),
            (Flush::Trees, Z_TREES, 3),
        ];

        for (flush, raw, rank) in EXPECTED {
            assert_eq!(flush.as_raw(), raw, "{flush:?} has the wrong C value");
            assert_eq!(flush.rank(), rank, "{flush:?} has the wrong rank");
            assert_eq!(rank_of_i32(raw), rank, "rank_of_i32({raw}) disagrees");
        }
    }

    /// The single reason the macro exists: "rank `Z_BLOCK` between `Z_NO_FLUSH` and
    /// `Z_PARTIAL_FLUSH`" (`deflate.c` L132).
    ///
    /// Without the `- 9`, `Z_BLOCK` would rank 10, above `Z_FINISH`, and the duplicate-flush
    /// guard at L1018 would reject a `Z_BLOCK` that follows any other flush.
    #[test]
    fn rank_places_z_block_between_no_flush_and_partial_flush() {
        assert!(Flush::NoFlush.rank() < Flush::Block.rank());
        assert!(Flush::Block.rank() < Flush::PartialFlush.rank());

        // And the five non-`Z_BLOCK` modes keep their declared order.
        assert!(Flush::PartialFlush.rank() < Flush::SyncFlush.rank());
        assert!(Flush::SyncFlush.rank() < Flush::FullFlush.rank());
        assert!(Flush::FullFlush.rank() < Flush::Finish.rank());
    }

    /// The two sentinels `DeflateState::last_flush` carries, and why they work.
    ///
    /// `-1` is "the previous call ran out of output space" (`deflate.c` L1010 and nine more
    /// sites) and `-2` is "freshly reset" (L672). Both must rank below every real flush so
    /// that the guard at L1018 never mistakes the next call for a repeat.
    #[test]
    fn rank_of_i32_reproduces_the_out_of_band_sentinels() {
        assert_eq!(rank_of_i32(-1), -2);
        assert_eq!(rank_of_i32(-2), -4);

        for flush in Flush::ALL {
            assert!(rank_of_i32(-1) < flush.rank(), "{flush:?} outranked by -1");
            assert!(rank_of_i32(-2) < flush.rank(), "{flush:?} outranked by -2");
        }
    }

    /// `rank_of_i32` is total: no input panics, including in a debug build.
    ///
    /// C's `(f) * 2` on an `int` is undefined at the extremes; wrapping is the defined
    /// analogue, and it is what keeps this function out of the panic family the workspace
    /// lints deny.
    #[test]
    fn rank_of_i32_is_total_over_i32() {
        assert_eq!(rank_of_i32(0), 0);
        assert_eq!(
            rank_of_i32(i32::MAX),
            i32::MAX.wrapping_mul(2).wrapping_sub(9)
        );
        assert_eq!(rank_of_i32(i32::MIN), i32::MIN.wrapping_mul(2));
    }

    /// The guard at `deflate.c` L1018-L1020, `RANK(flush) <= RANK(old_flush)`, over the pairs
    /// that decide whether a caller gets `Z_OK` or `Z_BUF_ERROR`.
    #[test]
    fn duplicate_flush_guard_agrees_with_the_reference() {
        // A repeated flush of the same mode is the case the guard exists to reject.
        assert!(Flush::SyncFlush.rank() <= rank_of_i32(Flush::SyncFlush.as_raw()));

        // A stronger flush after a weaker one is not a duplicate.
        assert!(Flush::FullFlush.rank() > rank_of_i32(Flush::SyncFlush.as_raw()));

        // `Z_BLOCK` after `Z_NO_FLUSH` is legal precisely because of the reordering; this is
        // the path `deflateParams` takes when it flushes with `Z_BLOCK` (L793).
        assert!(Flush::Block.rank() > rank_of_i32(Flush::NoFlush.as_raw()));

        // But `Z_BLOCK` after `Z_PARTIAL_FLUSH` is a duplicate, since 1 <= 2.
        assert!(Flush::Block.rank() <= rank_of_i32(Flush::PartialFlush.as_raw()));

        // Either sentinel lets any flush through.
        for flush in Flush::ALL {
            assert!(flush.rank() > rank_of_i32(-1));
            assert!(flush.rank() > rank_of_i32(-2));
        }
    }

    /// `deflate()` accepts exactly `0 ..= 5`; `Z_TREES` and everything else is
    /// `Z_STREAM_ERROR` (`deflate.c` L985).
    ///
    /// This is what keeps a `Trees` variant out of the compressors without omitting it from
    /// the shared [`Flush`], which `inflate` needs.
    #[test]
    fn deflate_accepts_z_no_flush_through_z_block_and_nothing_else() {
        for raw in 0..=5 {
            let flush = Flush::from_raw(raw).unwrap();
            assert!(flush.is_valid_for_deflate(), "{flush:?} should be accepted");
            assert_eq!(validate_deflate_flush(raw), Ok(flush));
        }

        // `Z_TREES` names a real mode, but not one `deflate` will take.
        assert_eq!(Flush::from_raw(Z_TREES), Some(Flush::Trees));
        assert!(!Flush::Trees.is_valid_for_deflate());
        assert_eq!(
            validate_deflate_flush(Z_TREES),
            Err(ReturnCode::STREAM_ERROR)
        );

        // Neither sentinel, nor anything above `Z_TREES`, names a mode at all.
        for raw in [-2, -1, 7, i32::MIN, i32::MAX] {
            assert_eq!(Flush::from_raw(raw), None, "{raw} should name no mode");
            assert_eq!(
                validate_deflate_flush(raw),
                Err(ReturnCode::STREAM_ERROR),
                "{raw} should be rejected"
            );
        }
    }

    /// The four variants are distinct, which is what makes the driver's three tests
    /// (`deflate.c` L1222, L1225, L1238) select disjoint work.
    #[test]
    fn block_state_variants_are_distinct() {
        const ALL: [BlockState; 4] = [
            BlockState::NeedMore,
            BlockState::BlockDone,
            BlockState::FinishStarted,
            BlockState::FinishDone,
        ];

        for (i, left) in ALL.iter().enumerate() {
            for (j, right) in ALL.iter().enumerate() {
                assert_eq!(left == right, i == j, "{left:?} vs {right:?}");
            }
        }
    }

    /// The two groupings `deflate()` tests for, and the overlap between them.
    ///
    /// `finish_started || finish_done` enters `FINISH_STATE` (L1222); `need_more ||
    /// finish_started` returns `Z_OK` (L1225). `FinishStarted` is in both, `BlockDone` in
    /// neither -- it is the only variant that reaches the block-terminator code at L1238.
    #[test]
    fn block_state_groupings_match_the_driver() {
        let enters_finish_state =
            |state: BlockState| matches!(state, BlockState::FinishStarted | BlockState::FinishDone);
        let returns_ok =
            |state: BlockState| matches!(state, BlockState::NeedMore | BlockState::FinishStarted);

        assert!(!enters_finish_state(BlockState::NeedMore));
        assert!(!enters_finish_state(BlockState::BlockDone));
        assert!(enters_finish_state(BlockState::FinishStarted));
        assert!(enters_finish_state(BlockState::FinishDone));

        assert!(returns_ok(BlockState::NeedMore));
        assert!(!returns_ok(BlockState::BlockDone));
        assert!(returns_ok(BlockState::FinishStarted));
        assert!(!returns_ok(BlockState::FinishDone));

        // `BlockDone` alone runs the per-flush-mode terminator.
        for state in [
            BlockState::NeedMore,
            BlockState::FinishStarted,
            BlockState::FinishDone,
        ] {
            assert_ne!(state, BlockState::BlockDone);
        }
    }

    /// Level 0 is tested before the strategy, so a level-0 stream stores whatever the strategy
    /// says (`deflate.c` L1217).
    ///
    /// This is the ordering bug the conditional chain is easiest to get wrong, and it is
    /// reachable: `deflateInit2_` accepts level 0 with `Z_HUFFMAN_ONLY` and with `Z_RLE`.
    #[test]
    fn select_lets_level_zero_beat_every_strategy() {
        for strategy in Strategy::ALL {
            assert_eq!(
                CompressFunc::select(0, strategy, CompressFunc::Slow),
                CompressFunc::Stored,
                "level 0 with {strategy:?}"
            );
        }
    }

    /// `Z_HUFFMAN_ONLY` and `Z_RLE` override the level's table entry, in that order
    /// (`deflate.c` L1218-L1219).
    #[test]
    fn select_lets_the_two_special_strategies_override_the_table() {
        for level in 1..=9 {
            assert_eq!(
                CompressFunc::select(level, Strategy::HuffmanOnly, CompressFunc::Slow),
                CompressFunc::Huff
            );
            assert_eq!(
                CompressFunc::select(level, Strategy::Rle, CompressFunc::Fast),
                CompressFunc::Rle
            );
        }
    }

    /// Every other strategy defers to `configuration_table[level].func` (`deflate.c` L1220).
    #[test]
    fn select_defers_to_the_table_for_the_other_strategies() {
        for strategy in [Strategy::Default, Strategy::Filtered, Strategy::Fixed] {
            for from_table in [CompressFunc::Fast, CompressFunc::Slow] {
                for level in 1..=9 {
                    assert_eq!(
                        CompressFunc::select(level, strategy, from_table),
                        from_table,
                        "level {level} with {strategy:?}"
                    );
                }
            }
        }
    }

    /// The three compressors `configuration_table` names are distinguishable, which is what
    /// makes `deflateParams`' identity test (`deflate.c` L791) reproduce C's pointer
    /// comparison.
    #[test]
    fn the_three_table_compressors_compare_unequal() {
        assert_ne!(CompressFunc::Stored, CompressFunc::Fast);
        assert_ne!(CompressFunc::Fast, CompressFunc::Slow);
        assert_ne!(CompressFunc::Stored, CompressFunc::Slow);
    }

    /// The property `deflate/config_table.rs` depends on: a [`CompressFunc`] is usable in a
    /// plain `const` array, with the level-to-compressor grouping of
    /// `configuration_table[10]` (`deflate.c` L112-L124).
    ///
    /// A function-pointer field could not do this, because a compressor is generic over the
    /// allocator; the array would have to carry that parameter. The grouping asserted here is
    /// the one `deflateParams`' identity test depends on, so `config_table.rs` must reproduce
    /// exactly these boundaries.
    #[test]
    fn compress_func_is_usable_in_a_const_array_with_the_c_grouping() {
        const TABLE: [CompressFunc; 10] = [
            CompressFunc::Stored, // level 0 -- store only
            CompressFunc::Fast,   // level 1 -- max speed, no lazy matches
            CompressFunc::Fast,   // level 2
            CompressFunc::Fast,   // level 3
            CompressFunc::Slow,   // level 4 -- lazy matches
            CompressFunc::Slow,   // level 5
            CompressFunc::Slow,   // level 6
            CompressFunc::Slow,   // level 7
            CompressFunc::Slow,   // level 8
            CompressFunc::Slow,   // level 9 -- max compression
        ];

        for (level, &func) in TABLE.iter().enumerate() {
            let expected = match level {
                0 => CompressFunc::Stored,
                1..=3 => CompressFunc::Fast,
                _ => CompressFunc::Slow,
            };
            assert_eq!(func, expected, "level {level}");
        }

        // Neither `deflate_rle` nor `deflate_huff` appears in the table: they are reachable
        // only through the strategy, in `select`.
        assert!(!TABLE.contains(&CompressFunc::Rle));
        assert!(!TABLE.contains(&CompressFunc::Huff));
    }

    /// `MAX_STORED` is the largest length a stored block's 16-bit `LEN` field can hold.
    #[test]
    fn max_stored_is_the_sixteen_bit_limit() {
        assert_eq!(MAX_STORED, 65535);
        assert_eq!(MAX_STORED, usize::from(u16::MAX));
    }

    /// The block moves on: `s->block_start = s->strstart` (L1637), and what was written lands
    /// in the caller's output buffer via `flush_pending` (L1638).
    #[test]
    fn flush_block_only_opens_the_next_block_at_strstart() {
        let mut state = new_state();
        tally_literals(&mut state, b"hello, hello!");

        let mut buffer = [0_u8; 256];
        let mut cursors = new_cursors(&mut buffer);

        flush_block_only(&mut state, &mut cursors, false);

        assert_eq!(state.window.block_start, 13);
        assert_eq!(state.window.bytes_since_block_start(), Some(0));

        // The whole block fitted, so nothing is left behind and `total_out` accounts for
        // exactly what the cursor received.
        let written = cursors.output.written();
        assert!(written > 0, "no block reached the output buffer");
        assert_eq!(state.pending_bytes(), 0);
        assert_eq!(cursors.total_out, written as u64);
    }

    /// `_tr_flush_block` is told the data type on the first block of a stream
    /// (`deflate.c` L1005-L1007), and it is visible to the caller afterwards.
    #[test]
    fn flush_block_only_publishes_the_data_type() {
        let mut state = new_state();
        tally_literals(&mut state, b"hello, hello!");

        let mut buffer = [0_u8; 256];
        let mut cursors = new_cursors(&mut buffer);
        assert_eq!(cursors.data_type, Z_UNKNOWN);

        flush_block_only(&mut state, &mut cursors, false);

        assert_ne!(cursors.data_type, Z_UNKNOWN);
    }

    /// A negative `block_start` selects the no-buffer form, C's `Z_NULL` (L1631-L1633).
    ///
    /// The state is reachable: `fill_window` slides the window down and takes `block_start`
    /// with it, so it "gets negative when the window is moved backwards"
    /// (`deflate.h` L159-L161). The offset must then not be handed to `_tr_flush_block`, and
    /// the block still has to close normally.
    #[test]
    fn flush_block_only_passes_no_buffer_when_block_start_is_negative() {
        let mut state = new_state();
        state.window.strstart = 0;
        state.window.block_start = -5;

        let mut buffer = [0_u8; 256];
        let mut cursors = new_cursors(&mut buffer);

        flush_block_only(&mut state, &mut cursors, false);

        assert_eq!(state.window.block_start, 0);
        assert!(cursors.output.written() > 0);
    }

    /// `if (s->strm->avail_out == 0) return (last) ? finish_started : need_more;` (L1644).
    #[test]
    fn flush_block_exit_reports_the_premature_exit() {
        // No output space at all, so `flush_pending` moves nothing and `avail_out` stays zero.
        let mut state = new_state();
        let mut empty: [u8; 0] = [];
        let mut cursors = new_cursors(&mut empty);
        assert_eq!(
            flush_block_exit(&mut state, &mut cursors, false),
            Some(BlockState::NeedMore)
        );

        let mut state = new_state();
        let mut empty: [u8; 0] = [];
        let mut cursors = new_cursors(&mut empty);
        assert_eq!(
            flush_block_exit(&mut state, &mut cursors, true),
            Some(BlockState::FinishStarted)
        );
    }

    /// With room to spare there is no premature exit, and the caller carries on.
    #[test]
    fn flush_block_exit_reports_nothing_when_output_remains() {
        let mut state = new_state();
        tally_literals(&mut state, b"hello");

        let mut buffer = [0_u8; 256];
        let mut cursors = new_cursors(&mut buffer);

        assert_eq!(flush_block_exit(&mut state, &mut cursors, false), None);
        assert!(cursors.avail_out() > 0);
        assert_eq!(cursors.avail_in(), 0);
    }

    /// The macro's whole job: returning from the function that expanded it.
    ///
    /// `BlockDone` is a sentinel here, not a meaningful outcome -- reaching it proves the
    /// macro did *not* return.
    fn run_flush_block(
        state: &mut DeflateState<'static, GlobalAllocator>,
        cursors: &mut StreamCursors<'_, '_>,
        last: bool,
    ) -> BlockState {
        flush_block!(state, cursors, last);
        BlockState::BlockDone
    }

    #[test]
    fn flush_block_macro_returns_from_its_caller() {
        let mut state = new_state();
        let mut empty: [u8; 0] = [];
        let mut cursors = new_cursors(&mut empty);
        assert_eq!(
            run_flush_block(&mut state, &mut cursors, false),
            BlockState::NeedMore
        );

        let mut state = new_state();
        let mut empty: [u8; 0] = [];
        let mut cursors = new_cursors(&mut empty);
        assert_eq!(
            run_flush_block(&mut state, &mut cursors, true),
            BlockState::FinishStarted
        );
    }

    #[test]
    fn flush_block_macro_falls_through_when_output_remains() {
        let mut state = new_state();
        tally_literals(&mut state, b"hello");

        let mut buffer = [0_u8; 256];
        let mut cursors = new_cursors(&mut buffer);

        assert_eq!(
            run_flush_block(&mut state, &mut cursors, false),
            BlockState::BlockDone
        );
        assert_eq!(state.window.block_start, 5);
    }
}
