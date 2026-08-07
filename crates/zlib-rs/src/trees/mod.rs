//! The internal Huffman-coding API of `trees.c` -- the six `_tr_*` entry points
//! and the root of the `trees` module tree.
//!
//! An unqualified line reference in this file is a line of `trees.c`.
//!
//! # The six entry points, and where they came from
//!
//! These are exactly the six functions `deflate.h` L310-L318 declares under the
//! comment `/* in trees.c */`, and they are the whole crate-facing surface of
//! this folder:
//!
//! | C function | L | Here |
//! |---|---|---|
//! | `_tr_init` | 456-478 | `_tr_init` |
//! | `_tr_stored_block` | 860-875 | `_tr_stored_block` |
//! | `_tr_flush_bits` | 880-882 | `_tr_flush_bits` |
//! | `_tr_align` | 888-895 | `_tr_align` |
//! | `_tr_flush_block` | 997-1089 | `_tr_flush_block` |
//! | `_tr_tally` | 1095-1119 | `_tr_tally` |
//!
//! The C names are kept verbatim so that the mapping to `deflate.h` L311-L318 is
//! one to one and every reader can find the oracle. They are `pub(crate)`, never
//! `#[no_mangle]`, which matches the `local:` block of `zlib.map` -- it hides
//! `_*`, so none of these six may ever become an exported symbol.
//!
//! # The module map
//!
//! The folder is declared here in dependency order, and each module owns one
//! layer of the reference's Huffman coder:
//!
//! | Module | From | Owns |
//! |---|---|---|
//! | `static_tables` | `trees.h`, L47-L115 | the transcribed `const` tables and the six file-local constants |
//! | `tree_desc` | L117-L138 | `static_tree_desc` and the three descriptors |
//! | `bit_writer` | L140-L286 | `put_short`, `send_bits`, `send_code`, `bi_reverse`, `bi_flush`, `bi_windup` |
//! | `build` | L203-L991, L1027-L1074 | tree construction, tree transmission, the block body, and the block-type decision |
//!
//! # Why this is part of the deflate subsystem rather than a peer of it
//!
//! `trees.c`'s only `#include` is `"deflate.h"` (L37). The Huffman coder has no
//! state of its own: every function below is an operation on
//! [`DeflateState`], the same struct `deflate/**` mutates, and there is
//! deliberately no parallel state type here. What C achieves by convention --
//! two translation units agreeing to share one struct -- this implementation achieves with
//! `&mut DeflateState`, so the compiler enforces the aliasing discipline instead
//! of trusting it.
//!
//! # Three decisions worth reading before the code
//!
//! **There is no static-table initialisation.** L457 calls `tr_static_init()`,
//! and it is tempting to look for its implementation. There is none, because in the
//! configuration this library ships the function is *empty*: its entire body sits
//! inside `#if defined(GEN_TREES_H) || !defined(STDC)` (L296-L373), L35 has
//! `/* #define GEN_TREES_H */` commented out, and `STDC` is defined, so L113-L114
//! takes the `#include "trees.h"` branch and the tables are compile-time data.
//! In Rust they are `const` arrays in `static_tables`, so there is nothing to
//! initialise and the call is omitted rather than ported as a stub.
//!
//! **`data_type` is threaded through the signature, not reached through a
//! back-pointer.** L1006-L1007 reads and writes `s->strm->data_type`, which is a
//! field of the *caller's* `z_stream` (`zlib.h` L106) and is observable by the
//! caller after the call. C reaches it through `s->strm`, a back-pointer from the
//! state to the stream that owns it. This implementation cannot hold one: a `&mut z_stream`
//! stored inside [`DeflateState`] would alias the `&mut DeflateState` every
//! function here takes, which safe Rust rejects outright -- and
//! `deflate/state.rs` accordingly declares neither a `strm` field nor a
//! `data_type` field. `_tr_flush_block` therefore takes `data_type: &mut i32`
//! as its second argument, and the facade is what connects that borrow to the
//! caller's struct. The value is not internal bookkeeping: `test/example.c` and
//! the differential suite both read `z_stream.data_type` back.
//!
//! **`buf` is an `Option<usize>` whose `None` means `block_start < 0`.** C passes
//! a `charf *` that `FLUSH_BLOCK_ONLY` computes as
//! `s->block_start >= 0L ? (charf *)&s->window[(unsigned)s->block_start] : (charf *)Z_NULL`
//! (`deflate.c` L1630-L1636), so the pointer is always a *window offset* and the
//! null case is exactly a negative `block_start` -- which is a legitimate state,
//! because `block_start` "gets negative when the window is moved backwards"
//! (`deflate.h` L159-L161). The implementation carries the offset instead of the pointer,
//! and the one expression that reproduces the conditional is shown rather than compiled,
//! because the field it reads is crate-private:
//!
//! ```text
//! let buf: Option<usize> = usize::try_from(state.window.block_start).ok();
//! ```
//!
//! -- `usize::try_from` on an [`isize`] fails for exactly the negative values, so
//! that *is* the `block_start >= 0L` test, and there is no second spelling of it.
//! `deflate/algorithm.rs`, which owns `flush_block_only`, is the caller that has
//! to produce it; both functions below restate the requirement, and
//! `_tr_stored_block` additionally asserts it, because whether `buf` is null
//! decides whether a stored block is eligible at all (L1047) and therefore
//! decides the emitted block type. Handing in a `&[u8]` instead is not an option:
//! it would borrow `state.window` for the duration of the call, and every
//! function here needs `&mut DeflateState`.
//!
//! # What is deliberately not implemented
//!
//! Only the default compile-time configuration, and none of these is offered as
//! a runtime option or a Cargo feature: `GEN_TREES_H` and its `tr_static_init`
//! and `gen_trees_header` (L35, L234, L295, L379-L435); `LIT_MEM`
//! (`deflate.h` L28, commented out), so the symbol buffer is the single
//! three-bytes-per-symbol `sym_buf` and never the `d_buf`/`l_buf` pair;
//! `ZLIB_DEBUG`, so there is no `compressed_len` or `bits_sent` accounting and
//! the reference's `Assert`s appear only as [`debug_assert!`]; `FORCE_STATIC`
//! (L1034); `FORCE_STORED` (L1044); and `DUMP_BL_TREE`.
//!
//! # Provenance
//!
//! Huffman tree construction and bit emission.
//!
//! Ported from `trees.c` and `trees.h`. A sibling of [`crate::deflate`] rather than a layer of its
//! own, because `trees.c` includes only `deflate.h`: the coder operates on `&mut DeflateState`, so
//! the `_tr_*` entry points are crate-private and only the generated tables are public.

pub(crate) mod static_tables;
pub(crate) mod tree_desc;

pub(crate) mod bit_writer;
pub(crate) mod build;

// The six generated tables, re-exported as data so that `crates/zlib-rs/src/lib.rs`
// can expose them and the planned `crates/zlib-rs-differential/tests/table_equality.rs` will be
// able to compare them element for element against `trees.h`. That comparison is the
// cheapest high-signal check in the whole port, which is why these six -- and
// only these six -- are `pub`. Everything else in this folder is `pub(crate)` or
// private, mirroring the `local` linkage of the C sources and the `local:` block
// of `zlib.map`.
pub use crate::trees::static_tables::{
    _dist_code, _length_code, base_dist, base_length, static_dtree, static_ltree,
};

use crate::config::Z_UNKNOWN;
use crate::deflate::state::{
    Allocator, DeflateState, StaticTreeKind, TreeDesc, D_CODES, LITERALS, STATIC_TREES,
};
use crate::trees::bit_writer::{bi_flush, bi_windup, put_short, send_bits, send_code};
use crate::trees::build::{
    build_bl_tree, build_tree, compress_block, d_code, detect_data_type, init_block,
    select_block_type, send_all_trees, BlockChoice, BlockTrees,
};
use crate::trees::static_tables::END_BLOCK;

// The name `deflate/pending.rs` documents for the bit-flush step it injects into
// `flush_pending` -- `flush_pending(state, output, total_out, crate::trees::flush_bits)`
// -- kept as an alias so that both spellings resolve: the C-identical
// `_tr_flush_bits` for anyone tracing `deflate.h` L315, and `flush_bits` for the
// call `deflate/mod.rs` is documented to make. `unused_imports` is allowed for
// the same reason `deflate/state.rs` allows it over its own re-export block: an
// alias this file does not itself name is still part of the module's surface, and
// the lint cannot see the consumer from here.
#[allow(unused_imports)]
pub(crate) use crate::trees::_tr_flush_bits as flush_bits;

/// Initialises the tree data structures for a new zlib stream.
///
/// Mirrors `_tr_init` (L456-L478).
///
/// # The three descriptor bindings
///
/// L459-L466 pair each descriptor with its dynamic array and its static tree.
/// The `dyn_tree` half is a self-referential pointer -- into the very state that
/// holds the descriptor -- which safe Rust cannot express, so
/// [`TreeDesc`] drops it and names the array by
/// [`StaticTreeKind`] instead; [`DeflateState::tree_for_mut`] performs the
/// selection C performs by dereference. Assigning a whole fresh
/// [`TreeDesc`] also resets `max_code`, which C does not do here. That is
/// deliberate and cannot change behaviour: `build_tree` assigns `max_code`
/// before anything reads it (L668, and the assignment in
/// [`build::build_tree`]), and a freshly initialised C state has it zero anyway
/// because `deflateInit2_` zeroes the struct (`deflate.c` L442). What it buys is
/// that no field this folder later reads is left holding whatever the allocator
/// happened to return -- the discipline `test/infcover.c` enforces by filling
/// every block it hands out with `0xa5` (L87).
///
/// # There is no `tr_static_init` call
///
/// L457 calls it; this does not implementation it, because in this configuration the
/// function has no body. See the module documentation.
///
/// # `bi_used` is not optional
///
/// L470 clears it alongside `bi_buf` and `bi_valid`. It records how many bits of
/// the last emitted byte were used and is what `deflateUsed` reports to callers
/// (`deflate.c` L737-L742, and [`crate::deflate::pending::deflate_used`]), so it
/// is caller-visible state rather than bookkeeping that may be dropped.
///
/// The `#ifdef ZLIB_DEBUG` initialisation of `compressed_len` and `bits_sent`
/// (L471-L474) is not implemented.
///
/// # Callers
///
/// `deflate/mod.rs`, at `deflateInit2_`'s tail through `lm_init` and again from
/// `deflateReset` (`deflate.c` L674). Both paths reach it once per stream reset,
/// which is why it must be idempotent -- and it is: every statement is an
/// assignment.
pub(crate) fn _tr_init<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // `s->l_desc.dyn_tree = s->dyn_ltree; s->l_desc.stat_desc = &static_l_desc;`
    // (L459-L460)
    state.l_desc = TreeDesc::new(StaticTreeKind::Literal);
    // `s->d_desc.dyn_tree = s->dyn_dtree; s->d_desc.stat_desc = &static_d_desc;`
    // (L462-L463)
    state.d_desc = TreeDesc::new(StaticTreeKind::Distance);
    // `s->bl_desc.dyn_tree = s->bl_tree; s->bl_desc.stat_desc = &static_bl_desc;`
    // (L465-L466)
    state.bl_desc = TreeDesc::new(StaticTreeKind::BitLength);

    // `s->bi_buf = 0; s->bi_valid = 0; s->bi_used = 0;` (L468-L470)
    state.bi_buf = 0;
    state.bi_valid = 0;
    state.bi_used = 0;

    // `init_block(s);` -- "Initialize the first block of the first file" (L476-L477).
    // Defined once, in `build.rs`, because `_tr_flush_block` calls it too (L1079).
    init_block(state);
}

/// Sends a stored block: the three-bit header, then `LEN`, `NLEN` and the raw
/// bytes.
///
/// Mirrors `_tr_stored_block` (L860-L875).
///
/// This is the `BTYPE = 00` block of `doc/rfc1951.txt` 3.2.4: the header is
/// followed by "any bits of input up to the next byte boundary are ignored", then
/// `LEN` and its one's complement `NLEN`, each a little-endian 16-bit count, then
/// `LEN` literal bytes. [`bit_writer::bi_windup`] performs the alignment and
/// [`bit_writer::put_short`] writes least-significant byte first, which is why
/// neither step may be reordered or replaced.
///
/// # Arguments
///
/// * `buf` -- where the block's bytes are, as a window offset; see
///   `usize::try_from(state.window.block_start).ok()`, per the module
///   documentation. Every C call site passes either `&s->window[s->block_start]`
///   (`deflate.c` L1839 and, through `FLUSH_BLOCK_ONLY`, L1631-L1633) or `Z_NULL`
///   together with a `stored_len` of zero (`deflate.c` L1242, L1713), and the two
///   [`debug_assert!`]s below state both halves of that pairing.
/// * `stored_len` -- C's `ulg stored_len`, the number of raw bytes to copy.
/// * `last` -- C's `int last`, the `BFINAL` bit, folded into the three header
///   bits as `(STORED_BLOCK << 1) + last`. [`BlockChoice::header_bits`] is that
///   expression, defined once in `build.rs` and shared with the two coded
///   branches of [`_tr_flush_block`].
///
/// # The copy
///
/// C copies with `zmemcpy` from a raw pointer and then advances `pending`
/// unconditionally. Here the source is a bounds-checked window region and the
/// destination is [`crate::weak_slice::PendingBuf::append`], which writes at
/// `pending_buf[pending]` and advances `pending` by what it wrote -- so one call
/// is both statements. A zero-length copy advances nothing, which is C's
/// `s->pending += 0`.
///
/// Writing past `lit_bufsize` overwrites the symbol buffer, and that is correct
/// rather than tolerated: the symbols overlay the same allocation
/// (`deflate.c` L520), and a stored block does not code them, so they are dead
/// the moment this function is chosen. [`_tr_flush_block`] calls
/// [`build::init_block`] immediately afterwards in any case.
///
/// The `#ifdef ZLIB_DEBUG` accounting at L869-L874 is not implemented.
///
/// # Callers
///
/// [`_tr_flush_block`] for the stored branch (L1056), and `deflate/mod.rs` and
/// `deflate/algorithm/stored.rs` for the three direct calls in `deflate.c`: the
/// empty marker block a `Z_SYNC_FLUSH` or `Z_FULL_FLUSH` emits (L1242), and the
/// two blocks `deflate_stored` writes (L1713, L1839).
pub(crate) fn _tr_stored_block<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    buf: Option<usize>,
    stored_len: u64,
    last: bool,
) {
    // Every C call site pairs a null `buf` with a zero `stored_len`; a null
    // pointer with a non-zero length would be an out-of-bounds read there. The
    // implementation declines the copy instead, which is the assertion stated as code.
    debug_assert!(
        buf.is_some() || stored_len == 0,
        "_tr_stored_block with no buf and a non-empty block (trees.c L866-L867)"
    );
    // And every non-null `buf` is `&s->window[s->block_start]`: `FLUSH_BLOCK_ONLY`
    // passes exactly that (`deflate.c` L1631-L1633) and so does `deflate_stored`
    // (`deflate.c` L1839). The offset is therefore never arbitrary, so a caller
    // that derived it any other way is caught here rather than silently storing
    // the wrong bytes.
    debug_assert!(
        buf.is_none() || buf == usize::try_from(state.window.block_start).ok(),
        "_tr_stored_block buf is not the block start (deflate.c L1631-L1633, L1839)"
    );

    // `send_bits(s, (STORED_BLOCK<<1) + last, 3);` -- send block type (L862).
    send_bits(state, BlockChoice::Stored.header_bits(last), 3);

    // `bi_windup(s);` -- align on byte boundary (L863). `LEN` and `NLEN` are
    // whole bytes, so the partial byte in `bi_buf` has to be flushed and padded
    // first; this is the "skip any remaining bits" step of RFC 1951 3.2.4.
    bi_windup(state);

    // `put_short(s, (ush)stored_len); put_short(s, (ush)~stored_len);`
    // (L864-L865). `~` is applied to the *narrowed* value, which is what C's
    // `(ush)~stored_len` computes for every `stored_len` that fits in 16 bits --
    // and every one does: `_tr_flush_block` receives `strstart - block_start`, at
    // most one 32768-byte window, and `deflate_stored` clamps to `MAX_STORED`,
    // 65535 (`deflate.c` L1650, L1829-L1830). The mask is C's `(ush)` truncation,
    // and it makes the conversion that follows infallible.
    let len = u16::try_from(stored_len & 0xffff).unwrap_or(0);
    put_short(state, len);
    put_short(state, !len);

    // `if (stored_len) zmemcpy(s->pending_buf + s->pending, (Bytef *)buf, stored_len);`
    // `s->pending += stored_len;` (L866-L868)
    //
    // C's implicit `ulg` to `size_t` widening. Saturating to `usize::MAX` rather
    // than to 0 so that a value which somehow left the range above fails every
    // bounds check instead of silently copying nothing; unreachable on any target
    // whose `usize` is at least 32 bits wide.
    let count = usize::try_from(stored_len).unwrap_or(usize::MAX);
    let payload = match buf {
        Some(offset) => state.window.region(offset, count),
        None => None,
    };
    match payload {
        Some(bytes) => {
            let copied = state.pending.append(bytes);
            // C has no bound at all here; the IN assertion is the caller's, and
            // every caller has discharged it (`deflate.c` L1828-L1831 sizes the
            // block against `pending_buf_size` before calling). A short append
            // would mean that reasoning had failed.
            debug_assert_eq!(
                copied, count,
                "_tr_stored_block ran out of pending_buf (trees.c L867)"
            );
        }
        // Reached only for an empty block, where C's `zmemcpy` is skipped and
        // `s->pending += 0` is a no-op -- or, unreachably, for an offset the
        // window cannot satisfy, where declining the copy is the safe answer.
        None => debug_assert_eq!(
            count, 0,
            "_tr_stored_block payload outside the window (trees.c L867)"
        ),
    }
}

/// Flushes the bit buffer to pending output, leaving at most seven bits behind.
///
/// Mirrors `_tr_flush_bits` (L880-L882), whose entire body is `bi_flush(s)`. It
/// exists as a separate function because `bi_flush` is `local` to `trees.c`
/// while `deflate.c` needs the operation, so this is the exported wrapper --
/// which is exactly the shape kept here: the implementation is
/// [`bit_writer::bi_flush`] and this is the name `deflate.h` L315 declares.
///
/// # Callers, and why the ordering matters
///
/// `deflate/mod.rs` calls it directly where `deflate.c` does (L766), and
/// [`crate::deflate::pending::flush_pending`] calls it **first**, before it
/// computes how many bytes to copy out (`deflate.c` L954). That ordering is
/// behavioural, not incidental: whole bytes still sitting in `bi_buf` when the
/// length is computed are bytes a caller with room for them does not receive. It
/// therefore runs on the hot path of every output flush. `flush_pending` takes it
/// as a parameter rather than importing it, which is what keeps the `deflate` and
/// `trees` module graph acyclic while both mutate one state; the alias
/// [`flush_bits`] is the name that call site is documented with.
pub(crate) fn _tr_flush_bits<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // `bi_flush(s);` (L881)
    bi_flush(state);
}

/// Sends one empty static block, to give inflate enough lookahead.
///
/// Mirrors `_tr_align` (L888-L895):
///
/// ```c
/// send_bits(s, STATIC_TREES<<1, 3);
/// send_code(s, END_BLOCK, static_ltree);
/// bi_flush(s);
/// ```
///
/// The reference's own description is the contract: "Send one empty static block
/// to give enough lookahead for inflate. This takes 10 bits, of which 7 may
/// remain in the bit buffer." (L885-L886). Three bits of block header plus the
/// seven-bit `static_ltree` code for `END_BLOCK` is ten, and the trailing step is
/// [`bit_writer::bi_flush`] and **not**
/// [`bit_writer::bi_windup`] -- a `Z_PARTIAL_FLUSH` must not pad to a byte
/// boundary, because the stream continues. Substituting the windup would insert
/// padding bits the reference does not emit.
///
/// Note that the header has no `+ last` term: an alignment block is never the
/// last block, so `BFINAL` is zero. The value is therefore
/// `STATIC_TREES << 1`, which is what [`BlockChoice::header_bits`] would return
/// for `last = false`; it is written as the reference writes it, at L889.
///
/// The `#ifdef ZLIB_DEBUG` `compressed_len += 10` at L892 is not implemented.
///
/// # Callers
///
/// `deflate/mod.rs`, on the `Z_PARTIAL_FLUSH` path only (`deflate.c` L1240).
pub(crate) fn _tr_align<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // `send_bits(s, STATIC_TREES<<1, 3);` (L889)
    send_bits(state, u16::from(STATIC_TREES) << 1, 3);

    // `send_code(s, END_BLOCK, static_ltree);` (L890). The static tree is
    // `const` data rather than a field of the state, so it can be passed
    // directly; a dynamic tree could not be, and `compress_block` handles that
    // case in `build.rs`.
    send_code(state, END_BLOCK, &static_ltree);

    // `bi_flush(s);` (L894)
    bi_flush(state);
}

/// Determines the best encoding for the current block and writes it out.
///
/// Mirrors `_tr_flush_block` (L997-L1089). This function is *orchestration only*:
/// `build.rs` owns the decision arithmetic and every helper, and this owns the
/// order in which they run and the dispatch on the answer. The split is on
/// purpose, and the seam is exact -- [`build::select_block_type`] is the whole of
/// L1026-L1054 and appears nowhere else, while the sequence below is the whole of
/// L1002-L1086.
///
/// # Why the order is the contract
///
/// Every step below feeds the next, and the last three bits it emits are read by
/// every decoder:
///
/// 1. **The data-type probe, first.** L1006-L1007 runs `detect_data_type` while
///    `dyn_ltree` still holds this block's *frequencies*. `build_tree` overwrites
///    `Freq` with `Code` (L224, and [`crate::deflate::state::CtData`]'s union
///    semantics), so probing after the trees are built would classify code words
///    instead of bytes.
/// 2. **The literal tree, then the distance tree.** L1010 and L1014, in that
///    order, because `build_tree` *accumulates* into `opt_len` and `static_len`
///    rather than assigning them. Swapping the calls leaves the same totals but
///    different `max_code` bookkeeping; omitting either makes the totals wrong
///    and therefore the block type wrong.
/// 3. **The bit-length tree.** L1024, which must follow both, because it scans
///    the two trees it summarises and adds the cost of transmitting them to
///    `opt_len` (L821).
/// 4. **The decision.** L1026-L1042, delegated whole.
/// 5. **The dispatch**, in C's predicate order (L1044-L1074): stored, then
///    static, then dynamic.
/// 6. **`init_block`**, L1079, which resets the frequencies for the next block --
///    after the body has been emitted from them, never before.
/// 7. **`bi_windup` when `last`**, L1081-L1082, which pads the final byte of the
///    stream. Not before step 6, and not at all for a non-final block.
///
/// # Arguments
///
/// * `data_type` -- the caller's `z_stream.data_type`, threaded rather than
///   reached through a `s->strm` back-pointer; see the module documentation. It is
///   written **only** when it arrives as [`Z_UNKNOWN`], which is what makes
///   `deflateSetHeader`-style callers able to force a classification and have it
///   respected.
/// * `buf` -- the block's bytes as a window offset, or [`None`] for `Z_NULL`; see
///   `usize::try_from(state.window.block_start).ok()`, per the module
///   documentation. A [`None`] here makes a stored block ineligible whatever the
///   byte counts say (L1047).
/// * `stored_len` -- C's `ulg stored_len`, which `FLUSH_BLOCK_ONLY` computes as
///   `(ulg)((long)s->strstart - s->block_start)` (`deflate.c` L1634).
/// * `last` -- C's `int last`, the `BFINAL` bit.
///
/// # What is not implemented
///
/// `Assert(s->compressed_len == s->bits_sent, "bad compressed size")` (L1075) and
/// every `Tracev` are `ZLIB_DEBUG`-only, and the two counters they compare do not
/// exist in this implementation. The `#ifdef ZLIB_DEBUG` additions to `compressed_len` at
/// L1063, L1072 and L1084 are omitted for the same reason.
///
/// # Callers
///
/// `deflate/algorithm.rs`, from `flush_block_only` -- the Rust counterpart of
/// `FLUSH_BLOCK_ONLY` (`deflate.c` L1630-L1640) -- which is what every one of the
/// five compression strategies reaches it through.
// The call to `_tr_stored_block` below names an underscore-prefixed item, which
// `clippy::pedantic` flags. The six names in this module are the C spellings of
// `deflate.h` L311-L318 and are kept for oracle traceability, so the call is
// allowed here rather than the name changed.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items)]
pub(crate) fn _tr_flush_block<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    data_type: &mut i32,
    buf: Option<usize>,
    stored_len: u64,
    last: bool,
) {
    // `int max_blindex = 0;` -- "index of last bit length code of non zero freq"
    // (L1000). It stays zero when no trees are built, and is only read by the
    // dynamic branch, which only the tree-building branch can select.
    let mut max_blindex: i32 = 0;

    // `if (s->level > 0) {` -- "Build the Huffman trees unless a stored block is
    // forced" (L1002-L1003).
    if state.level > 0 {
        // `if (s->strm->data_type == Z_UNKNOWN) s->strm->data_type = detect_data_type(s);`
        // -- "Check if the file is binary or text" (L1005-L1007).
        if *data_type == Z_UNKNOWN {
            *data_type = detect_data_type(state);
        }

        // `build_tree(s, (tree_desc *)(&(s->l_desc)));` (L1010)
        build_tree(state, StaticTreeKind::Literal);
        // `build_tree(s, (tree_desc *)(&(s->d_desc)));` (L1014)
        //
        // "At this point, opt_len and static_len are the total bit lengths of the
        // compressed block data, excluding the tree representations."
        // (L1017-L1019)
        build_tree(state, StaticTreeKind::Distance);

        // `max_blindex = build_bl_tree(s);` -- "Build the bit length tree for the
        // above two trees, and get the index in bl_order of the last bit length
        // code to send." (L1021-L1024)
        max_blindex = build_bl_tree(state);
    }

    // "Determine the best encoding. Compute the block lengths in bytes."
    // (L1026-L1042) together with the three predicates at L1044-L1074, delegated
    // whole so that the truncating `(x + 3 + 7) >> 3` arithmetic and the
    // predicate order exist in exactly one place. The `else` half -- `level == 0`,
    // where `opt_lenb = static_lenb = stored_len + 5` forces a stored block
    // (L1039-L1042) -- is inside it as well, which is why this call is not itself
    // conditional.
    let selection = select_block_type(
        state.opt_len,
        state.static_len,
        stored_len,
        state.level,
        state.strategy,
        buf.is_some(),
    );

    // The invariant L1035-L1037 and L1041 jointly establish: the preferred cost
    // is never dearer than the fixed-tree cost, because either the fixed trees
    // were cheaper and `opt_lenb` was lowered to them, or they were not and
    // `opt_lenb` is the strictly smaller dynamic cost. It is also what makes the
    // `static_lenb == opt_lenb` test at L1058 a test for "the fixed trees won".
    debug_assert!(
        selection.static_lenb >= selection.opt_lenb,
        "block cost inversion: static_lenb below opt_lenb (trees.c L1035-L1041)"
    );

    match selection.choice {
        // `_tr_stored_block(s, buf, stored_len, last);` (L1056). "The test
        // buf != NULL is only necessary if LIT_BUFSIZE > WSIZE." (L1050-L1055)
        BlockChoice::Stored => _tr_stored_block(state, buf, stored_len, last),

        // `send_bits(s, (STATIC_TREES<<1) + last, 3);`
        // `compress_block(s, static_ltree, static_dtree);` (L1058-L1061)
        BlockChoice::Static => {
            send_bits(state, BlockChoice::Static.header_bits(last), 3);
            compress_block(state, BlockTrees::Static);
        }

        // `send_bits(s, (DYN_TREES<<1) + last, 3);`
        // `send_all_trees(s, s->l_desc.max_code + 1, s->d_desc.max_code + 1, max_blindex + 1);`
        // `compress_block(s, s->dyn_ltree, s->dyn_dtree);` (L1065-L1070)
        BlockChoice::Dynamic => {
            send_bits(state, BlockChoice::Dynamic.header_bits(last), 3);
            // Read after the header is sent, exactly as C evaluates the
            // arguments after `send_bits` has returned. Neither is disturbed by
            // it, so the two orders are equivalent; this one matches the source.
            let lcodes = state.desc_for(StaticTreeKind::Literal).max_code + 1;
            let dcodes = state.desc_for(StaticTreeKind::Distance).max_code + 1;
            send_all_trees(state, lcodes, dcodes, max_blindex + 1);
            compress_block(state, BlockTrees::Dynamic);
        }
    }

    // `init_block(s);` (L1079)
    init_block(state);

    // `if (last) { bi_windup(s); }` -- align the stream on a byte boundary
    // (L1081-L1082).
    if last {
        bi_windup(state);
    }
}

/// Saves one literal or one match and tallies its frequency counts.
///
/// Mirrors `_tr_tally` (L1095-L1119), whose contract is its own comment: "Save
/// the match info and tally the frequency counts. Return true if the current
/// block must be flushed." (L1092-L1093).
///
/// # Arguments and their widths
///
/// * `dist` -- the match distance, or `0` for a literal. `u16` because the
///   inline forms of this function narrow it themselves -- `ush dist = (ush)(distance);`
///   (`deflate.h` L367) -- and because a distance is at most `MAX_DIST`, one
///   window less the lookahead (`deflate.h` L301).
/// * `lc` -- the literal byte when `dist` is zero, otherwise the match length
///   minus `MIN_MATCH`. `u8` for the same reason: `uch len = (uch)(length);`
///   (`deflate.h` L366), and the largest normalised length is
///   `MAX_MATCH - MIN_MATCH`, which is 255.
///
/// A distance of zero is how a literal is spelled, so the three-byte record has
/// one layout for both kinds of symbol and `compress_block` tells them apart by
/// testing `dist` (L916-L917).
///
/// # Return value
///
/// `true` when the symbol buffer has just become full, which C returns as
/// `s->sym_next == s->sym_end` (L1118). Callers use it for nothing but a flush
/// trigger -- `if (bflush) FLUSH_BLOCK(s, 0);` (`deflate.c` L1938, L2038, L2139,
/// L2175) -- so [`bool`] is the honest type for C's `int` here.
///
/// # The symbol buffer
///
/// `LIT_MEM` is commented out at `deflate.h` L28, so `LIT_BUFS` is 4 and there is
/// a single `sym_buf` of three bytes per symbol: the distance least significant
/// byte first, then `lc`. That buffer is *not a separate allocation* -- it
/// overlays the pending output at `pending_buf + lit_bufsize`
/// (`deflate.c` L520) -- so it is reached through
/// [`crate::weak_slice::PendingBuf::push_symbol`], which owns the split, performs
/// exactly the three writes of L1100-L1102 and advances `sym_next` by three.
/// Nothing here reallocates or resizes it.
///
/// `push_symbol` reports [`None`] when the three bytes would not fit, which
/// cannot happen: `sym_end` is a multiple of three (`deflate.c` L521), `sym_next`
/// advances in threes, and a caller stops as soon as this function returns
/// `true`, so `sym_next < sym_end` always implies room. The fallback still
/// requests a flush rather than silently continuing, which is the outcome that
/// recovers the buffer.
///
/// The `Assert` at L1111-L1113 -- that the distance, the normalised length and
/// the resulting distance code are all in range -- is `ZLIB_DEBUG`-only, so it is
/// a [`debug_assert!`] and can never fail a release build. The bound on `dist` is
/// deliberately not restated: it needs `MAX_DIST(s)`, and every index below is
/// bounds-checked in its own right.
///
/// # Callers
///
/// All five compression strategies, through the `_tr_tally_lit` and
/// `_tr_tally_dist` macros of `deflate.h` L357-L375: `deflate/algorithm/fast.rs`,
/// `slow.rs`, `rle.rs`, `huff.rs` and `stored.rs`. Those macros are the inline
/// expansion of this function, not a different computation -- `deflate.h`
/// L378-L380 defines them *as* a call to it under `ZLIB_DEBUG` -- so this one
/// implementation serves both spellings.
#[must_use]
pub(crate) fn _tr_tally<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    dist: u16,
    lc: u8,
) -> bool {
    // `s->sym_buf[s->sym_next++] = (uch)dist;`
    // `s->sym_buf[s->sym_next++] = (uch)(dist >> 8);`
    // `s->sym_buf[s->sym_next++] = (uch)lc;`  (L1100-L1102)
    //
    // `unwrap_or(true)` covers the unreachable full-buffer case by asking for the
    // flush that empties it; see the documentation above.
    let flush = state.pending.push_symbol(dist, lc).unwrap_or(true);

    if dist == 0 {
        // `s->dyn_ltree[lc].Freq++;` -- lc is the unmatched char (L1104-L1106).
        // The index is a byte value and `dyn_ltree` has 573 entries, so the slot
        // always exists.
        if let Some(entry) = state.dyn_ltree.get_mut(usize::from(lc)) {
            entry.increment_freq();
        }
    } else {
        // `s->matches++;` (L1108). Saturating, and the saturation is
        // unreachable: this counts matches within one block, and a block holds at
        // most `lit_bufsize - 1` symbols, which is at most 32767.
        state.matches = state.matches.saturating_add(1);

        // `dist--;` -- "dist = match distance - 1" (L1109-L1110). Cannot
        // underflow: this is the `dist != 0` branch.
        let dist = u32::from(dist) - 1;

        // `Assert((ush)lc <= (ush)(MAX_MATCH-MIN_MATCH) && (ush)d_code(dist) < (ush)D_CODES, ...)`
        // (L1111-L1113). The `lc` half holds for every `u8` by construction, so
        // what is left to check is the distance code.
        debug_assert!(
            d_code(dist) < D_CODES,
            "_tr_tally: bad match (trees.c L1111-L1113)"
        );

        // `s->dyn_ltree[_length_code[lc] + LITERALS + 1].Freq++;` -- lc is the
        // match length minus MIN_MATCH, so the literal/length alphabet is indexed
        // past the 256 literals and the end-of-block code (L1115). `_length_code`
        // has one entry per normalised length, so the lookup is always in range,
        // and its values are at most 28, so the sum is at most 285 -- within
        // `dyn_ltree`.
        let length_code = usize::from(_length_code.get(usize::from(lc)).copied().unwrap_or(0));
        if let Some(entry) = state.dyn_ltree.get_mut(length_code + LITERALS + 1) {
            entry.increment_freq();
        }

        // `s->dyn_dtree[d_code(dist)].Freq++;` (L1116). One `d_code`
        // implementation, `build.rs`'s, is shared with `compress_block` (L930):
        // the two must agree, because one tallies the frequency that builds the
        // tree and the other emits the code the tree produced.
        if let Some(entry) = state.dyn_dtree.get_mut(d_code(dist)) {
            entry.increment_freq();
        }
    }

    // `return (s->sym_next == s->sym_end);` (L1118)
    flush
}

#[cfg(test)]
// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// harness: a test asserts, and a failing assertion panics. Indexing is allowed because every index
// below is bounded by a constant the line above it establishes. `used_underscore_items` is allowed
// because these tests call the six functions under test, whose names are the C spellings of
// `deflate.h` L311-L318. The same relaxation, for the same reasons, appears in `trees/build.rs`,
// `trees/bit_writer.rs`, `deflate/pending.rs` and `deflate/state.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(
    unknown_lints,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::used_underscore_items
)]
mod tests {
    use super::{
        _tr_align, _tr_flush_bits, _tr_flush_block, _tr_init, _tr_stored_block, _tr_tally,
    };
    use crate::config::{Z_BINARY, Z_TEXT, Z_UNKNOWN};
    use crate::deflate::state::{
        CtData, DeflateConfig, DeflateState, GlobalAllocator, Method, StaticTreeKind, Strategy,
        BL_CODES, DEF_MEM_LEVEL, D_CODES, LITERALS, L_CODES,
    };
    use crate::trees::static_tables::{_length_code, static_ltree, END_BLOCK};

    /// A stream configured as `deflateInit2(&strm, level, Z_DEFLATED, -15, 8, strategy)` and then
    /// wired up by [`_tr_init`], which is exactly the state the C oracle for these tests starts
    /// from.
    ///
    /// The window bits are the maximum and the memory level is the default, so `lit_bufsize` is
    /// 16384 and `sym_end` is 49149 -- both measured from the reference. In a debug build every
    /// block the allocator hands back is filled with `0xa5`, the same value
    /// `test/infcover.c` L87 uses, so nothing below can be passing by accident on zeroed memory.
    fn oracle_state(level: i32, strategy: Strategy) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level,
            method: Method::Deflated,
            window_bits: 15,
            mem_level: DEF_MEM_LEVEL,
            strategy,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        state
    }

    /// The default stream: level 6, [`Strategy::Default`].
    fn state() -> DeflateState<'static, GlobalAllocator> {
        oracle_state(6, Strategy::Default)
    }

    /// Overwrites every field [`_tr_init`] is responsible for with a recognisable non-zero
    /// pattern, so that a missing assignment in it shows up as that pattern rather than as a
    /// zero that happened to be correct.
    fn poison(state: &mut DeflateState<'static, GlobalAllocator>) {
        const GARBAGE: CtData = CtData::new(0xa5a5, 0xa5a5);

        state.bi_buf = 0xa5a5;
        state.bi_valid = 0x5a;
        state.bi_used = 0x5a;
        state.opt_len = u64::MAX;
        state.static_len = u64::MAX;
        state.matches = 0xa5a5_a5a5;
        assert!(state.pending.set_sym_next(30));

        for entry in &mut state.dyn_ltree[..] {
            *entry = GARBAGE;
        }
        for entry in &mut state.dyn_dtree[..] {
            *entry = GARBAGE;
        }
        for entry in &mut state.bl_tree[..] {
            *entry = GARBAGE;
        }

        // Mispair all three descriptors, so that a binding this function does not restore would
        // select the wrong dynamic array.
        state.l_desc.stat_desc = StaticTreeKind::BitLength;
        state.l_desc.max_code = 0x5a5a;
        state.d_desc.stat_desc = StaticTreeKind::BitLength;
        state.d_desc.max_code = 0x5a5a;
        state.bl_desc.stat_desc = StaticTreeKind::Literal;
        state.bl_desc.max_code = 0x5a5a;
    }

    /// Tallies one literal of every byte value and lays the same bytes down in the window, which
    /// is the most incompressible block a literal alphabet can produce and therefore the case in
    /// which a stored block wins.
    fn tally_all_bytes(state: &mut DeflateState<'static, GlobalAllocator>) {
        for byte in 0..=u8::MAX {
            assert!(state.window.write_at(usize::from(byte), &[byte]));
            assert!(!_tr_tally(state, 0, byte));
        }
    }

    /// Tallies `count` copies of `literal`, and lays them down in the window.
    fn tally_repeated(
        state: &mut DeflateState<'static, GlobalAllocator>,
        literal: u8,
        count: usize,
    ) {
        for index in 0..count {
            assert!(state.window.write_at(index, &[literal]));
            assert!(!_tr_tally(state, 0, literal));
        }
    }

    /// The three block-type bits of the first emitted byte: `BFINAL` in bit 0 and `BTYPE` in
    /// bits 1 and 2 (`doc/rfc1951.txt` 3.2.3).
    fn header_bits(state: &DeflateState<'static, GlobalAllocator>) -> u8 {
        state.pending.written()[0] & 7
    }

    /// Every field the folder later reads is established, even from poisoned memory.
    ///
    /// The expected values are the reference's, read out of a C `deflate_state` immediately after
    /// `deflateInit2(&strm, 6, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY)`: `bi_buf` 0,
    /// `bi_valid` 0, `bi_used` 0, `opt_len` 0, `static_len` 0, `sym_next` 0, `sym_end` 49149,
    /// `matches` 0, `dyn_ltree[256].Freq` 1 and all three descriptors paired.
    #[test]
    fn tr_init_establishes_every_field_the_folder_reads() {
        let mut state = state();
        poison(&mut state);
        _tr_init(&mut state);

        // The three descriptor bindings of L459-L466.
        assert_eq!(state.l_desc.stat_desc, StaticTreeKind::Literal);
        assert_eq!(state.d_desc.stat_desc, StaticTreeKind::Distance);
        assert_eq!(state.bl_desc.stat_desc, StaticTreeKind::BitLength);
        assert_eq!(state.l_desc.max_code, 0);
        assert_eq!(state.d_desc.max_code, 0);
        assert_eq!(state.bl_desc.max_code, 0);

        // L468-L470.
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_used, 0);

        // What `init_block` contributes through L477.
        for node in 0..L_CODES {
            let expected = u16::from(node == END_BLOCK);
            assert_eq!(
                state.dyn_ltree[node].freq(),
                expected,
                "dyn_ltree[{node}].Freq"
            );
        }
        for node in 0..D_CODES {
            assert_eq!(state.dyn_dtree[node].freq(), 0, "dyn_dtree[{node}].Freq");
        }
        for node in 0..BL_CODES {
            assert_eq!(state.bl_tree[node].freq(), 0, "bl_tree[{node}].Freq");
        }
        assert_eq!(state.opt_len, 0);
        assert_eq!(state.static_len, 0);
        assert_eq!(state.sym_next(), 0);
        assert_eq!(state.sym_end(), 49149);
        assert_eq!(state.matches, 0);
    }

    /// Running it twice changes nothing: `deflateReset` reaches it once per reset
    /// (`deflate.c` L674).
    #[test]
    fn tr_init_is_idempotent() {
        let once = state();
        let mut twice = state();
        _tr_init(&mut twice);

        assert_eq!(once.bi_buf, twice.bi_buf);
        assert_eq!(once.bi_valid, twice.bi_valid);
        assert_eq!(once.bi_used, twice.bi_used);
        assert_eq!(once.opt_len, twice.opt_len);
        assert_eq!(once.static_len, twice.static_len);
        assert_eq!(once.sym_next(), twice.sym_next());
        assert_eq!(once.matches, twice.matches);
        assert_eq!(
            once.dyn_ltree[END_BLOCK].freq(),
            twice.dyn_ltree[END_BLOCK].freq()
        );
        assert_eq!(once.l_desc, twice.l_desc);
        assert_eq!(once.d_desc, twice.d_desc);
        assert_eq!(once.bl_desc, twice.bl_desc);
    }

    /// A literal is stored as a zero distance and raises its own frequency.
    ///
    /// Measured from the reference for `_tr_tally(s, 0, 'q')`: `sym_buf` becomes
    /// `[0x00, 0x00, 0x71]`, `sym_next` 3, `matches` 0, `dyn_ltree[113].Freq` 1, return 0.
    #[test]
    fn tr_tally_records_a_literal() {
        let mut state = state();

        assert!(!_tr_tally(&mut state, 0, b'q'));

        assert_eq!(state.sym_next(), 3);
        assert_eq!(state.pending.symbols(), &[0x00, 0x00, 0x71]);
        assert_eq!(state.matches, 0);
        assert_eq!(state.dyn_ltree[usize::from(b'q')].freq(), 1);
        // The end-of-block frequency `init_block` set is untouched.
        assert_eq!(state.dyn_ltree[END_BLOCK].freq(), 1);
    }

    /// A match is stored little-endian and raises a length code and a distance code.
    ///
    /// Measured from the reference for `_tr_tally(s, 3, 1)`: `sym_buf` becomes
    /// `[0x03, 0x00, 0x01]`, `matches` 1, `dyn_ltree[258].Freq` 1 -- that is
    /// `_length_code[1] + LITERALS + 1` -- and `dyn_dtree[2].Freq` 1, from `d_code(3 - 1)`.
    #[test]
    fn tr_tally_records_a_match() {
        let mut state = state();

        assert!(!_tr_tally(&mut state, 3, 1));

        assert_eq!(state.sym_next(), 3);
        assert_eq!(state.pending.symbols(), &[0x03, 0x00, 0x01]);
        assert_eq!(state.matches, 1);

        let length_code = usize::from(_length_code[1]) + LITERALS + 1;
        assert_eq!(length_code, 258);
        assert_eq!(state.dyn_ltree[length_code].freq(), 1);
        assert_eq!(state.dyn_dtree[2].freq(), 1);
    }

    /// The largest match the format allows reaches the second half of `_dist_code`.
    ///
    /// Measured from the reference for `_tr_tally(s, 32506, 255)` -- `MAX_DIST` for a 32 KiB
    /// window and `MAX_MATCH - MIN_MATCH`: `sym_buf` becomes `[0xfa, 0x7e, 0xff]`, the length
    /// code is 285 and the distance code is 29.
    #[test]
    fn tr_tally_records_the_largest_match() {
        let mut state = state();

        assert!(!_tr_tally(&mut state, 32506, 255));

        assert_eq!(state.pending.symbols(), &[0xfa, 0x7e, 0xff]);
        let length_code = usize::from(_length_code[255]) + LITERALS + 1;
        assert_eq!(length_code, 285);
        assert_eq!(state.dyn_ltree[length_code].freq(), 1);
        assert_eq!(state.dyn_dtree[29].freq(), 1);
        assert_eq!(state.matches, 1);
    }

    /// The return value flips to `true` exactly once `sym_next` reaches `sym_end`.
    ///
    /// Measured from the reference: with `lit_bufsize` 16384 and `sym_end` 49149 the reference
    /// returns 0 for 16382 calls and 1 on the 16383rd.
    #[test]
    fn tr_tally_reports_the_flush_trigger_at_sym_end() {
        let mut state = state();
        let mut falses = 0usize;

        loop {
            if _tr_tally(&mut state, 0, b'x') {
                break;
            }
            falses += 1;
            assert!(falses < state.sym_end(), "flush trigger never fired");
        }

        assert_eq!(falses, 16382);
        assert_eq!(state.sym_next(), state.sym_end());
        assert_eq!(state.sym_next(), 49149);
    }

    /// One empty static block is ten bits: three of header, then the seven-bit
    /// `static_ltree` code for `END_BLOCK`.
    ///
    /// Measured from the reference: `pending` becomes `[0x02]` with `bi_valid` 2 and `bi_used`
    /// untouched at 0 -- so `bi_flush`, not `bi_windup`, ended the sequence.
    #[test]
    fn tr_align_emits_ten_bits_of_an_empty_static_block() {
        let mut state = state();

        _tr_align(&mut state);

        // Derived rather than hardcoded, so the assertion also pins the table entry: the header
        // is `STATIC_TREES << 1` in the low three bits, then the end-of-block code above it.
        let eob = static_ltree[END_BLOCK];
        assert_eq!(eob.len(), 7);
        assert_eq!(eob.code(), 0);
        let bits = 3 + u32::from(eob.len());
        assert_eq!(bits, 10);

        let whole = u32::from(2u16) | (u32::from(eob.code()) << 3);
        assert_eq!(
            state.pending.written(),
            &[u8::try_from(whole & 0xff).unwrap()]
        );
        assert_eq!(state.pending.written(), &[0x02]);
        assert_eq!(state.bi_valid, i32::try_from(bits).unwrap() - 8);
        assert_eq!(state.bi_valid, 2);
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_used, 0);
    }

    /// Two alignment blocks in a row carry the bit buffer across the boundary.
    ///
    /// Measured from the reference: `pending` becomes `[0x02, 0x08]` with `bi_valid` 4.
    #[test]
    fn tr_align_twice_carries_the_bit_buffer() {
        let mut state = state();

        _tr_align(&mut state);
        _tr_align(&mut state);

        assert_eq!(state.pending.written(), &[0x02, 0x08]);
        assert_eq!(state.bi_valid, 4);
    }

    /// `_tr_flush_bits` emits nothing when fewer than eight bits are buffered.
    ///
    /// Measured from the reference: after `_tr_align` the state is `[0x02]` with `bi_valid` 2,
    /// and `_tr_flush_bits` leaves it exactly there -- "leaves at most 7 bits" (L878).
    #[test]
    fn tr_flush_bits_leaves_at_most_seven_bits() {
        let mut state = state();
        _tr_align(&mut state);

        _tr_flush_bits(&mut state);

        assert_eq!(state.pending.written(), &[0x02]);
        assert_eq!(state.bi_valid, 2);
        assert!(state.bi_valid < 8);
    }

    /// A whole buffered byte does leave, which is the property `flush_pending` depends on.
    ///
    /// Measured from the reference: a non-final empty block leaves ten bits in `bi_buf` and
    /// nothing in `pending`, because [`_tr_flush_block`] winds up only for the last block. The
    /// following `_tr_flush_bits` moves one byte out -- `[0x02]` -- and leaves two bits. That
    /// byte is exactly what a caller with room for it would otherwise not receive, which is why
    /// `flush_pending` runs this step before it measures how much to copy (`deflate.c` L954).
    #[test]
    fn tr_flush_bits_drains_whole_bytes() {
        let mut state = state();
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut state, &mut data_type, None, 0, false);
        assert_eq!(state.bi_valid, 10);
        assert!(state.pending.written().is_empty());

        _tr_flush_bits(&mut state);

        assert_eq!(state.pending.written(), &[0x02]);
        assert_eq!(state.bi_valid, 2);
        assert_eq!(state.bi_buf, 0);
    }

    /// Three alignment blocks, then a flush that has nothing whole to move.
    ///
    /// Measured from the reference: `pending` becomes `[0x02, 0x08, 0x20]` with `bi_valid` 6,
    /// and `_tr_flush_bits` leaves both exactly there -- each `_tr_align` ends with its own
    /// `bi_flush`, so there is never a whole byte still waiting.
    #[test]
    fn tr_flush_bits_is_a_no_op_when_nothing_whole_is_buffered() {
        let mut state = state();
        _tr_align(&mut state);
        _tr_align(&mut state);
        _tr_align(&mut state);
        assert_eq!(state.pending.written(), &[0x02, 0x08, 0x20]);
        assert_eq!(state.bi_valid, 6);

        _tr_flush_bits(&mut state);

        assert_eq!(state.pending.written(), &[0x02, 0x08, 0x20]);
        assert_eq!(state.bi_valid, 6);
    }

    /// The header, the `LEN`/`NLEN` pair and the payload, in that order.
    ///
    /// Measured from the reference for `_tr_stored_block(s, s->window, 5, 0)` over the bytes
    /// `"hello"`: `pending` becomes `[0x00, 0x05, 0x00, 0xfa, 0xff, 'h', 'e', 'l', 'l', 'o']`
    /// with `bi_valid` 0 and `bi_used` 3.
    #[test]
    fn tr_stored_block_writes_len_nlen_and_the_payload() {
        let mut state = state();
        assert!(state.window.write_at(0, b"hello"));

        _tr_stored_block(&mut state, Some(0), 5, false);

        assert_eq!(
            state.pending.written(),
            &[0x00, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o']
        );
        // `NLEN` is the one's complement of `LEN`, both least significant byte first.
        let written = state.pending.written();
        let len = u16::from_le_bytes([written[1], written[2]]);
        let nlen = u16::from_le_bytes([written[3], written[4]]);
        assert_eq!(len, 5);
        assert_eq!(nlen, !len);

        // `bi_windup` aligned the stream, so nothing is left in the bit buffer and `bi_used`
        // records the three header bits as the last byte's occupancy.
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_used, 3);
    }

    /// The empty marker block a `Z_SYNC_FLUSH` emits, with `BFINAL` set.
    ///
    /// Measured from the reference for `_tr_stored_block(s, Z_NULL, 0, 1)`: `pending` becomes
    /// `[0x01, 0x00, 0x00, 0xff, 0xff]`. Those are the five bytes `inflateSync` recognises.
    #[test]
    fn tr_stored_block_empty_marker() {
        let mut state = state();

        _tr_stored_block(&mut state, None, 0, true);

        assert_eq!(state.pending.written(), &[0x01, 0x00, 0x00, 0xff, 0xff]);
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_used, 3);
    }

    /// A zero-length block with a buf behaves exactly like one without: no payload, no advance.
    #[test]
    fn tr_stored_block_with_an_empty_payload() {
        let mut state = state();

        _tr_stored_block(&mut state, Some(0), 0, false);

        assert_eq!(state.pending.written(), &[0x00, 0x00, 0x00, 0xff, 0xff]);
    }

    /// An empty final block: the `Z_FINISH`-on-empty-input path.
    ///
    /// Measured from the reference for `_tr_flush_block(s, Z_NULL, 0, 1)` on a fresh level-6
    /// stream: `pending` becomes `[0x03, 0x00]`, `bi_valid` 0, `bi_used` 2, `data_type`
    /// `Z_BINARY`, `l_desc.max_code` 256 and `d_desc.max_code` 1 -- the last two being the
    /// forced-two-codes repair at work. These are also the two bytes a raw deflate stream of
    /// empty input consists of.
    ///
    /// This is the case in which `build_tree` decrements `opt_len` and `static_len` while both
    /// are zero, so it is the regression test for that wrap: a plain subtraction would abort
    /// this test, and Miri, and every debug build.
    #[test]
    fn tr_flush_block_emits_the_empty_final_block() {
        let mut state = state();
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, None, 0, true);

        assert_eq!(state.pending.written(), &[0x03, 0x00]);
        assert_eq!(header_bits(&state), 3, "BFINAL set, BTYPE static");
        assert_eq!(data_type, Z_BINARY);
        assert_eq!(state.l_desc.max_code, 256);
        assert_eq!(state.d_desc.max_code, 1);

        // `last` was set, so the stream was wound up and the next block starts on a byte.
        assert_eq!(state.bi_valid, 0);
        assert_eq!(state.bi_buf, 0);
        assert_eq!(state.bi_used, 2);

        // `init_block` ran afterwards, so the block is ready to be filled again.
        assert_eq!(state.sym_next(), 0);
        assert_eq!(state.opt_len, 0);
        assert_eq!(state.static_len, 0);
        assert_eq!(state.matches, 0);
        assert_eq!(state.dyn_ltree[END_BLOCK].freq(), 1);
    }

    /// Level 0 forces a stored block and leaves `data_type` alone.
    ///
    /// Measured from the reference for `_tr_flush_block(s, s->window, 5, 0)` on a level-0
    /// stream over `"hello"`: the same ten bytes `_tr_stored_block` produces, and `data_type`
    /// still `Z_UNKNOWN` -- because the probe at L1006 is inside the `level > 0` branch.
    #[test]
    fn tr_flush_block_forces_a_stored_block_at_level_zero() {
        let mut state = oracle_state(0, Strategy::Default);
        assert!(state.window.write_at(0, b"hello"));
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, Some(0), 5, false);

        assert_eq!(
            state.pending.written(),
            &[0x00, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o']
        );
        assert_eq!(header_bits(&state), 0, "BFINAL clear, BTYPE stored");
        assert_eq!(
            data_type, Z_UNKNOWN,
            "the probe is inside the level > 0 branch"
        );
    }

    /// A stored block is chosen at exactly `stored_len + 4 == opt_lenb`, and not one byte later.
    ///
    /// Measured from the reference: with one literal of every byte value tallied, `opt_lenb` is
    /// 272, so a stored block is chosen for every `stored_len` up to and including 268 and the
    /// fixed trees take over at 269. Both sides of that boundary are asserted, because a
    /// `<` where the reference writes `<=` would move it by one.
    #[test]
    fn tr_flush_block_chooses_stored_at_the_exact_boundary() {
        let mut at_boundary = state();
        tally_all_bytes(&mut at_boundary);
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut at_boundary, &mut data_type, Some(0), 268, false);
        assert_eq!(header_bits(&at_boundary), 0, "stored at stored_len 268");
        assert_eq!(at_boundary.pending.written().len(), 273, "1 + 4 + 268");
        assert_eq!(data_type, Z_BINARY);

        let mut past_boundary = state();
        tally_all_bytes(&mut past_boundary);
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut past_boundary, &mut data_type, Some(0), 269, false);
        assert_eq!(header_bits(&past_boundary), 2, "static at stored_len 269");
    }

    /// A null buf makes a stored block ineligible however cheap it would be.
    ///
    /// Measured from the reference: the boundary case above, with `buf` replaced by `Z_NULL`,
    /// emits the fixed-tree block instead -- byte for byte the one `stored_len` 269 produces.
    #[test]
    fn tr_flush_block_cannot_store_without_a_buf() {
        let mut with_buf = state();
        tally_all_bytes(&mut with_buf);
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut with_buf, &mut data_type, Some(0), 269, false);

        let mut without_buf = state();
        tally_all_bytes(&mut without_buf);
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut without_buf, &mut data_type, None, 268, false);

        assert_eq!(header_bits(&without_buf), 2, "static, not stored");
        assert_eq!(
            without_buf.pending.written(),
            with_buf.pending.written(),
            "the same fixed-tree block either way"
        );
    }

    /// The fixed trees are chosen for a short block, where transmitting a tree cannot pay.
    ///
    /// Measured from the reference for three literals `'a'`, `'b'`, `'c'` with `stored_len` 3:
    /// `pending` becomes `[0x4a, 0x4c, 0x4a, 0x06]` with `bi_valid` 2, and the block type is
    /// static.
    #[test]
    fn tr_flush_block_chooses_static_for_a_short_block() {
        let mut state = state();
        for (index, literal) in b"abc".iter().enumerate() {
            assert!(state.window.write_at(index, &[*literal]));
            assert!(!_tr_tally(&mut state, 0, *literal));
        }
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, Some(0), 3, false);

        assert_eq!(state.pending.written(), &[0x4a, 0x4c, 0x4a, 0x06]);
        assert_eq!(header_bits(&state), 2, "BFINAL clear, BTYPE static");
        assert_eq!(state.bi_valid, 2);
        assert_eq!(data_type, Z_TEXT);
    }

    /// Dynamic trees are chosen once they pay for themselves.
    ///
    /// Measured from the reference for 120 copies of `'a'` with `stored_len` 120: `pending`
    /// becomes 26 bytes beginning `[0x04, 0xc1, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x56,
    /// 0xff, 0x13]`, with `bi_buf` `0x0800` and `bi_valid` 12.
    #[test]
    fn tr_flush_block_chooses_dynamic_when_its_trees_pay() {
        let mut state = state();
        tally_repeated(&mut state, b'a', 120);
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, Some(0), 120, false);

        assert_eq!(header_bits(&state), 4, "BFINAL clear, BTYPE dynamic");
        assert_eq!(state.pending.written().len(), 26);
        assert_eq!(
            &state.pending.written()[..12],
            &[0x04, 0xc1, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x56, 0xff, 0x13]
        );
        assert_eq!(state.bi_buf, 0x0800);
        assert_eq!(state.bi_valid, 12);
        assert_eq!(data_type, Z_TEXT);
    }

    /// `Z_FIXED` forbids dynamic trees, so the same input takes the fixed ones.
    ///
    /// Measured from the reference for the same 120 copies of `'a'` under `Z_FIXED`: the block
    /// type is static and `pending` is 120 bytes beginning `[0x4a, 0x4c, 0x4c, 0x4c]` -- far
    /// larger than the 26 bytes the dynamic trees achieve, which is the point of the strategy.
    #[test]
    fn tr_flush_block_honours_z_fixed() {
        let mut state = oracle_state(6, Strategy::Fixed);
        tally_repeated(&mut state, b'a', 120);
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, Some(0), 120, false);

        assert_eq!(header_bits(&state), 2, "BFINAL clear, BTYPE static");
        assert_eq!(state.pending.written().len(), 120);
        assert_eq!(
            &state.pending.written()[..4],
            &[0x4a, 0x4c, 0x4c, 0x4c],
            "the fixed-tree body"
        );
        assert_eq!(state.bi_buf, 0x0004);
        assert_eq!(state.bi_valid, 10);
    }

    /// `data_type` is written only when it arrives as `Z_UNKNOWN`.
    ///
    /// Measured from the reference: a block whose only symbol is the letter `'A'` classifies as
    /// `Z_TEXT`, and the same block with `strm->data_type` preset to `Z_TEXT` leaves it there
    /// -- as does one preset to `Z_BINARY`, which is what makes the probe a one-shot
    /// classification rather than a per-block one.
    #[test]
    fn tr_flush_block_writes_data_type_only_when_unknown() {
        let mut unknown = state();
        assert!(!_tr_tally(&mut unknown, 0, b'A'));
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut unknown, &mut data_type, None, 1, false);
        assert_eq!(data_type, Z_TEXT);

        let mut preset_text = state();
        assert!(!_tr_tally(&mut preset_text, 0, b'A'));
        let mut data_type = Z_TEXT;
        _tr_flush_block(&mut preset_text, &mut data_type, None, 1, false);
        assert_eq!(data_type, Z_TEXT);

        // A text block preset to binary stays binary: the probe never runs a second time.
        let mut preset_binary = state();
        assert!(!_tr_tally(&mut preset_binary, 0, b'A'));
        let mut data_type = Z_BINARY;
        _tr_flush_block(&mut preset_binary, &mut data_type, None, 1, false);
        assert_eq!(data_type, Z_BINARY);
    }

    /// A non-final block is not wound up, so the next block starts mid-byte.
    ///
    /// Measured from the reference: the same empty block with `last` clear leaves all ten bits
    /// in `bi_buf` and `pending` empty, whereas with `last` set the windup pushes both bytes out
    /// and clears the buffer. Winding up a non-final block would insert padding bits the
    /// reference does not emit.
    #[test]
    fn tr_flush_block_winds_up_only_for_the_last_block() {
        let mut not_last = state();
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut not_last, &mut data_type, None, 0, false);
        assert!(not_last.pending.written().is_empty());
        assert_eq!(not_last.bi_buf, 0x0002);
        assert_eq!(not_last.bi_valid, 10, "all ten bits remain buffered");
        assert_eq!(not_last.bi_used, 0, "bi_windup did not run");

        let mut last = state();
        let mut data_type = Z_UNKNOWN;
        _tr_flush_block(&mut last, &mut data_type, None, 0, true);
        assert_eq!(last.pending.written(), &[0x03, 0x00]);
        assert_eq!(last.bi_valid, 0, "wound up");
        assert_eq!(last.bi_used, 2);
    }

    /// Consecutive blocks accumulate in the pending buffer, and each starts from a clean
    /// frequency table.
    ///
    /// Measured from the reference for eight copies of `'a'` and then eight of `'b'`, the second
    /// block final: the first block is the eight bytes `[0x4a, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c,
    /// 0x4c, 0x4c]`, the pair together is nineteen bytes ending `[0x09, 0x00]`, and `'a'`'s
    /// frequency is back to zero before the second block is tallied.
    #[test]
    fn tr_flush_block_resets_the_block_between_calls() {
        let mut state = state();
        tally_repeated(&mut state, b'a', 8);
        let mut data_type = Z_UNKNOWN;

        _tr_flush_block(&mut state, &mut data_type, Some(0), 8, false);
        assert_eq!(
            state.pending.written(),
            &[0x4a, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c]
        );
        assert_eq!(state.sym_next(), 0);
        assert_eq!(state.dyn_ltree[usize::from(b'a')].freq(), 0);
        assert_eq!(state.dyn_ltree[END_BLOCK].freq(), 1);

        tally_repeated(&mut state, b'b', 8);
        _tr_flush_block(&mut state, &mut data_type, Some(0), 8, true);

        assert_eq!(state.pending.written().len(), 19);
        assert_eq!(
            &state.pending.written()[8..],
            &[0x04, 0x2c, 0x29, 0x29, 0x29, 0x29, 0x29, 0x29, 0x29, 0x09, 0x00]
        );
        assert_eq!(state.bi_valid, 0, "the final block is wound up");
    }
}
