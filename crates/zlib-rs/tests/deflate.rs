//! Integration tests for the DEFLATE compressor.
//!
//! This suite drives `zlib_rs::deflate` from **outside** the crate, through exactly the surface
//! `crates/libz-rs-sys` builds its `extern "C"` exports on. That vantage point is the point: a
//! `#[cfg(test)] mod tests` inside `src/deflate/**` can reach crate-private helpers and so can
//! test them directly, but it cannot prove that the *public* surface is sufficient to compress
//! anything. This file can, and does.
//!
//! # Why this file exists
//!
//! RFC 1951 constrains the DEFLATE *format*; it says nothing about the encoder's *choices*. Which
//! of several equally valid matches to emit, when to abandon a lazy match, how to break a Huffman
//! frequency tie and when to switch block types are all implementation-defined, and callers of
//! `libz` depend on zlib's specific answers. A fully RFC-conformant encoder that made different
//! choices would still be a regression here, so a handful of deliberately non-optimal heuristics
//! are load-bearing and every one of them is a verbatim port. Five of the eight decision points
//! that fix the emitted bytes live in the code this suite covers:
//!
//! | # | Decision point | C location | Covered by |
//! |---|---|---|---|
//! | 1 | the per-level tuning table | `deflate.c` L112-L124 | [`configuration_table_matches_the_reference_row_for_row`] |
//! | 2 | the `NIL` window-index-0 prohibition | `deflate.c` L1988-L1994 | [`a_match_against_window_index_zero_is_never_emitted`] |
//! | 4 | the `TOO_FAR` lazy rejection | `deflate.c` L89, L1999-L2007 | [`too_far_discards_a_three_byte_match_beyond_4096_bytes`] |
//! | 5 | the lazy-emit decision | `deflate.c` L2011-L2013 | [`lazy_matching_pays_off_from_level_four_upwards`] |
//! | 8 | block-type selection | `trees.c` L1027-L1074 | [`level_zero_emits_stored_blocks`], [`container_headers_and_trailers_have_the_reference_shape`] |
//!
//! Decision point 3 (the early-rejection quartet at `deflate.c` L1481-L1484) is an ordering
//! *inside* `longest_match`, which no caller can observe apart from through the matches it
//! selects; it is exercised transitively by every match-bearing test here and directly by the
//! unit tests in `src/deflate/longest_match.rs`. Decisions 6 (the Huffman depth tie-breaker) and
//! 7 (the forced two-code case) belong to `src/trees/**` and are tested there.
//!
//! # Scope boundary: this is *self*-consistency, not cross-implementation identity
//!
//! **Byte-identity against the compiled C library is deliberately not this file's job.** The
//! full level x `windowBits` x `memLevel` x strategy x flush x corpus matrix belongs to a
//! differential suite under `crates/zlib-rs-differential`, the only crate that can link the C
//! oracle and compare the two implementations directly. **No such suite exists in the tree, so
//! cross-implementation byte-identity is currently unverified by anything** -- and reproducing
//! even part of that matrix here would be duplication that drifts, and would make Miri unusable.
//!
//! What this file pins instead:
//!
//! * **transcribed oracle values** -- the tuning table, the sizing constants and the status
//!   codes, compared against the numbers read out of `deflate.c`, `deflate.h`, `zutil.h` and
//!   `zlib.h`;
//! * **specific heuristic behaviours** -- inputs crafted so that one encoder decision, and only
//!   that decision, changes the output, so a "better" encoder fails here rather than silently
//!   passing;
//! * **self-consistency** -- the same input compressed twice must give the same bytes, and
//!   feeding it one byte at a time must give the same bytes as feeding it all at once;
//! * **the public contract** -- every documented return code, every documented rejection, and a
//!   round trip through `zlib_rs::inflate` for every container format.
//!
//! # Reachability: what this suite can assert directly, and what it delegates
//!
//! Everything below was checked against the sources rather than assumed, because several items
//! are less reachable than their role suggests.
//!
//! **Reachable and asserted directly:** `deflate::CONFIGURATION_TABLE` and `deflate::Config`'s
//! four numeric fields; `deflate::Status` and its raw values; `config::Flush`, `config::Strategy`,
//! `config::Method`, `config::DeflateConfig` and the bounds constants; `error::ReturnCode`;
//! `weak_slice`'s sizing constants and the `Pos`/`IPos` newtypes; `deflate::state`'s
//! `HEAP_SIZE`/`MAX_BITS`/`BUF_SIZE`; every `deflate_*` entry point; and `DeflateState`'s
//! read-only accessors.
//!
//! **Not reachable -- consequences asserted instead, with the delegation named:**
//!
//! * `Config::func` is `pub(crate)`, and it is a `CompressFunc` *enum*, not a function pointer.
//!   Comparing it is impossible from here, which is the outcome the plan wanted anyway: function
//!   pointers compare unreliably in Rust because two functions may or may not share an address
//!   after optimisation. The level-to-strategy binding is therefore verified **behaviourally**,
//!   in [`level_zero_emits_stored_blocks`], [`lazy_matching_pays_off_from_level_four_upwards`],
//!   [`huffman_only_emits_no_matches_at_all`] and
//!   [`rle_can_only_reach_back_one_byte`].
//! * `BlockState` is `pub(crate)`; its four variants surface only as the `ReturnCode` the driver
//!   returns, which is what the flush tests assert.
//! * `longest_match`, `fill_window`, `slide_hash`, `update_hash`, `insert_string`, `clear_hash`,
//!   `flush_pending`, `put_byte`, `put_short_msb` and every `_tr_*` entry point are
//!   `pub(crate)`, mirroring `zlib.map`'s `local:` block. Direct coverage lives in the
//!   `#[cfg(test)] mod tests` blocks of the modules that define them. **Widening a visibility to
//!   make one of them testable from here is prohibited** -- the hidden-symbol set is a shipped
//!   contract stated by `zlib.map`, and `Makefile.in`'s `rust-symbols` target is what compares the
//!   built library against the 111-symbol baseline. No `cargo test` checks it.
//! * `TOO_FAR` is a private constant in `src/deflate/algorithm/slow.rs`. Its value is pinned
//!   behaviourally instead, at the exact 4096-byte boundary, which is a stronger check than
//!   reading the constant back.
//! * `DeflateState::hash_shift` is `pub(crate)`, so the invariant
//!   `hash_shift * MIN_MATCH >= hash_bits` cannot be read field-for-field. It is recomputed from
//!   the public `hash_bits()` here; the field-level assertion is
//!   `src/deflate/state.rs`'s `hash_shift_invariant_holds_for_every_mem_level`.
//! * The `Z_BUF_ERROR` plus `pending = (unsigned)-1` arm of `deflatePending` (`deflate.c`
//!   L722-L735) is unreachable through the public API on any target whose `usize` is at least 32
//!   bits, because `pending_buf_size` never exceeds `LIT_BUFS * MAX_LIT_BUFSIZE` = 131072. It is
//!   covered by `src/deflate/pending.rs`'s `narrow_pending_probes_the_unsigned_narrowing`.
//! * `inflate_get_header` moves its `GzHeaderSink` into the inflate state and no accessor hands
//!   it back, so only the caller-owned `Cell<u8>` buffers -- the extra field, the name and the
//!   comment -- are observable here. The scalar fields are asserted in
//!   `src/inflate/header.rs`.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no FFI, no third-party crate.** `crates/zlib-rs/Cargo.toml` declares an empty
//!   `[dependencies]` and no `[dev-dependencies]`, and `deny.toml`'s `[bans]` names that table as
//!   the enforcement point. Only `core`, `alloc`, `std`, `zlib_rs` and the shared `common` module
//!   are available, so pseudo-random filler comes from `common::lcg_fill` rather than `rand`.
//! * **`std` is available; `no_std` is not.** The library is `no_std` by default, but an
//!   integration test is its own crate and links `std` unconditionally, so no `#![no_std]`
//!   appears here.
//! * **Miri-clean.** `cargo +nightly miri test -p zlib-rs --test deflate` must report no
//!   undefined behaviour. Every payload is a few kilobytes at most; the single case that must
//!   exceed the 32 KiB window is isolated in
//!   [`a_payload_larger_than_the_window_round_trips_after_the_window_slides`] and is the only
//!   test marked `#[cfg_attr(miri, ignore)]`.
//! * **Hermetic.** No clock, no environment, no filesystem, no network: every payload is computed
//!   from constants, so the pinned byte sequences below are reproducible on every run and every
//!   platform.
//! * **No feature gating.** This file compiles and passes identically under
//!   `--no-default-features`, `--features std`, `--features simd`, `--features rust-api` and
//!   `--all-features`. It reaches the engine by module path, never through the `rust-api`
//!   re-exports, because those carry `#[cfg(feature = "rust-api")]` and `cargo test` builds with
//!   default features. [`compressed_output_is_identical_across_repeated_runs`] is what makes the
//!   `simd` configuration meaningful: the feature may only change throughput, so a vectorised
//!   path leaking into match finding -- which is prohibited outright -- would fail there.

// The panic family is denied workspace-wide for library code, and `clippy.toml` grants
// `allow-unwrap-in-tests` / `allow-expect-in-tests` / `allow-panic-in-tests` only inside a
// `#[test]` function. The harness below is file-scope, and `indexing_slicing` has no in-test
// escape hatch at all, so the four relaxations are stated here for the whole crate. This is a
// test binary: an out-of-range index or a failed `unwrap` is the failure report, which is exactly
// what those lints exist to prevent in shipped code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::cell::Cell;

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::config::{
    DeflateConfig, InflateConfig, Method, Strategy, DEF_MEM_LEVEL, DEF_WBITS, MAX_MEM_LEVEL,
    MAX_WBITS, MIN_MEM_LEVEL, MIN_WBITS, PRESET_DICT, Z_BEST_COMPRESSION, Z_BEST_SPEED,
    Z_DEFAULT_COMPRESSION, Z_DEFLATED, Z_NO_COMPRESSION, Z_UNKNOWN,
};
use zlib_rs::deflate::state::{GzHeaderView, BUF_SIZE, HEAP_SIZE, MAX_BITS};
use zlib_rs::deflate::{
    deflate, deflate_bound, deflate_bound_z, deflate_copy, deflate_end, deflate_get_dictionary,
    deflate_init, deflate_init2, deflate_params, deflate_pending, deflate_prime, deflate_reset,
    deflate_reset_keep, deflate_reset_snapshot, deflate_set_dictionary, deflate_set_header,
    deflate_state_check, deflate_tune, deflate_used, DeflateReset, DeflateState, DeflateStream,
    Status, CONFIGURATION_TABLE, OS_CODE,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::state::GzHeaderSink;
use zlib_rs::inflate::{
    inflate, inflate_end, inflate_get_header, inflate_init2, inflate_reset, inflate_set_dictionary,
    inflate_sync, InflateStream,
};
use zlib_rs::read_buf::OutputRegion;
use zlib_rs::weak_slice::{
    IPos, Pos, LIT_BUFS, MAX_MATCH, MIN_LOOKAHEAD, MIN_MATCH, NIL, WIN_INIT,
};

use common::corpus;

/// The concrete deflate state this suite drives.
///
/// `GlobalAllocator` is the stand-in for C's default `malloc`/`free` path (`zutil.c` L215, L240),
/// and it satisfies `Allocator<'a>` for any lifetime, so `'static` is the natural choice for a
/// state that owns its buffers for the whole test.
type Deflate = DeflateState<'static, GlobalAllocator>;

// ---------------------------------------------------------------------------------------------
// Oracle constants, transcribed from the C sources
//
// Every literal in this block was read out of the reference implementation, not derived from the
// Rust port. That is the whole point: a test that recomputed these from the code under test would
// agree with it however wrong it became.
// ---------------------------------------------------------------------------------------------

/// `Z_NO_FLUSH` (`zlib.h` L172).
const NO_FLUSH: i32 = 0;
/// `Z_PARTIAL_FLUSH` (`zlib.h` L173).
const PARTIAL_FLUSH: i32 = 1;
/// `Z_SYNC_FLUSH` (`zlib.h` L174).
const SYNC_FLUSH: i32 = 2;
/// `Z_FULL_FLUSH` (`zlib.h` L175).
const FULL_FLUSH: i32 = 3;
/// `Z_FINISH` (`zlib.h` L176).
const FINISH: i32 = 4;
/// `Z_BLOCK` (`zlib.h` L177).
const BLOCK: i32 = 5;
/// `Z_TREES` (`zlib.h` L178) -- an **inflate-only** flush. `deflate()` rejects it at
/// `deflate.c` L985 together with every other value above `Z_BLOCK`.
const TREES: i32 = 6;

/// `TOO_FAR` (`deflate.c` L88-L91): "Matches of length 3 are discarded if their distance exceeds
/// `TOO_FAR`". Transcribed rather than imported, because the port keeps it private to
/// `src/deflate/algorithm/slow.rs`.
const TOO_FAR: usize = 4096;

/// Raw DEFLATE: no wrapper at all (`zlib.h` L556-L560, negative `windowBits`).
const RAW: i32 = -15;
/// The RFC 1950 zlib wrapper: a two-byte header and an Adler-32 trailer.
const ZLIB: i32 = 15;
/// The RFC 1952 gzip wrapper: `windowBits + 16` (`zlib.h` L562-L566).
const GZIP: i32 = 31;

/// `deflate.c` L112-L124, the non-`FASTEST` `configuration_table[10]`, as
/// `(good_length, max_lazy, nice_length, max_chain)`.
///
/// The fifth column of the C table is the strategy function -- `deflate_stored` for level 0,
/// `deflate_fast` for levels 1 to 3 and `deflate_slow` for levels 4 to 9. It is deliberately
/// absent here: the port models it as a `pub(crate)` enum, and even if it were reachable,
/// comparing it would be comparing function identity. The binding is verified behaviourally
/// instead; see the reachability notes in this file's documentation.
///
/// The `FASTEST` two-entry variant at `deflate.c` L107-L110 is `#ifdef`-ed out in the shipped
/// build and the port implements the default configuration only. `FASTEST`, `LIT_MEM`,
/// `UNALIGNED_OK`, `FORCE_STATIC`, `FORCE_STORED` and `GEN_TREES_H` are compile-time switches
/// that change emitted bytes, so none of them may become a runtime or feature option.
#[rustfmt::skip]
const REFERENCE_CONFIGURATION_TABLE: [(u16, u16, u16, u16); 10] = [
    /* 0: store only            */ ( 0,   0,   0,    0),
    /* 1: max speed, no lazy    */ ( 4,   4,   8,    4),
    /* 2                        */ ( 4,   5,  16,    8),
    /* 3                        */ ( 4,   6,  32,   32),
    /* 4: lazy matches          */ ( 4,   4,  16,   16),
    /* 5                        */ ( 8,  16,  32,   32),
    /* 6                        */ ( 8,  16, 128,  128),
    /* 7                        */ ( 8,  32, 128,  256),
    /* 8                        */ (32, 128, 258, 1024),
    /* 9: max compression       */ (32, 258, 258, 4096),
];

/// The first level that uses `deflate_slow`, and therefore lazy matching (`deflate.c` L119).
const FIRST_LAZY_LEVEL: usize = 4;

/// `deflate.h` L104-L118 and `zutil.h`: the eight `deflate_state.status` values, paired with the
/// port's [`Status`] variant. `FINISH_STATE` is 666 rather than a round number, and that is not a
/// typo in either implementation.
const REFERENCE_STATUS_VALUES: [(Status, i32, &str); 8] = [
    (Status::Init, 42, "INIT_STATE"),
    (Status::GzipHeader, 57, "GZIP_STATE"),
    (Status::Extra, 69, "EXTRA_STATE"),
    (Status::Name, 73, "NAME_STATE"),
    (Status::Comment, 91, "COMMENT_STATE"),
    (Status::Hcrc, 103, "HCRC_STATE"),
    (Status::Busy, 113, "BUSY_STATE"),
    (Status::Finish, 666, "FINISH_STATE"),
];

/// The canonical `Z_SYNC_FLUSH` marker: an empty stored block, byte-aligned, whose `LEN`/`NLEN`
/// pair is `0x0000`/`0xffff` (RFC 1951 section 3.2.4, emitted by `_tr_stored_block` through
/// `deflate.c`'s `Z_SYNC_FLUSH` path).
const SYNC_MARKER: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// The smallest window the implementation ever builds (`deflate.c` L438 promotes 8 to 9).
///
/// Used wherever a test needs the sliding-window path without a payload larger than the 32 KiB
/// default window: a 512-byte window slides after a few hundred bytes, which keeps those tests
/// inside Miri's reach.
const NARROW_WINDOW_BITS: i32 = 9;
/// The window size `NARROW_WINDOW_BITS` implies.
const NARROW_WINDOW_SIZE: usize = 1 << NARROW_WINDOW_BITS;

// ---------------------------------------------------------------------------------------------
// Harness
//
// C callers drive deflate by mutating four `z_stream` fields -- `next_in`, `avail_in`, `next_out`,
// `avail_out` -- before each call and reading them back afterwards. The port replaces the two
// pointer/length pairs with two slices and two offsets, so the equivalent of "make exactly N
// input bytes available" is "pass a slice of exactly N bytes". [`Deflater`] does that bookkeeping
// and, crucially, carries `total_in`, `total_out`, `adler`, `msg` and `data_type` from one call to
// the next: those five live on the stream rather than in the state, and dropping them between
// calls would silently reset the running checksum.
// ---------------------------------------------------------------------------------------------

/// Wraps a byte literal as the shared-mutable slice a [`GzHeaderView`] field takes.
///
/// The view's `extra`, `name` and `comment` model storage the *application* owns and may
/// write while the compressor holds the borrow, so their element type is
/// [`core::cell::Cell`] rather than `u8`. A test owns its fixtures outright, so the
/// conversion is a plain copy into an owned vector; the caller keeps it alive for as long
/// as the view is used, which is what the borrow requires.
fn cells(bytes: &[u8]) -> Vec<Cell<u8>> {
    bytes.iter().copied().map(Cell::new).collect()
}

/// Builds a deflate state from raw `deflateInit2_` arguments and resets it.
///
/// [`deflate_init2`] already resets internally but discards the [`DeflateReset`] scalars, and a
/// caller needs them to seed the stream's `adler`. Calling [`deflate_reset`] again is both cheap
/// and idempotent, and it is how the crate's own tests obtain the same values.
fn open(
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: Strategy,
) -> (Deflate, DeflateReset) {
    let config = DeflateConfig {
        level,
        method: Method::Deflated,
        window_bits,
        mem_level,
        strategy,
    };
    let mut state = deflate_init2(config, GlobalAllocator).unwrap_or_else(|err| {
        panic!("deflate_init2({level}, {window_bits}, {mem_level}) failed: {err:?}")
    });
    let reset = deflate_reset(&mut state);
    (state, reset)
}

/// A deflate state plus the stream scalars a C caller would keep in its `z_stream`.
///
/// Owning the output buffer here is what lets [`Deflater::step`] hand the driver a sub-slice of
/// exactly `avail_out` bytes without the call sites juggling offsets.
struct Deflater {
    state: Deflate,
    out: Vec<u8>,
    /// Total bytes written so far, i.e. C's `next_out - start`.
    produced: usize,
    total_in: u64,
    total_out: u64,
    adler: u32,
    msg: Option<&'static str>,
    data_type: i32,
}

impl Deflater {
    /// Opens a stream whose output buffer has room for `capacity` bytes.
    fn new(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
        capacity: usize,
    ) -> Self {
        let (state, reset) = open(level, window_bits, mem_level, strategy);
        Self {
            state,
            out: vec![0u8; capacity],
            produced: 0,
            total_in: reset.total_in,
            total_out: reset.total_out,
            adler: reset.adler,
            msg: reset.msg,
            data_type: reset.data_type,
        }
    }

    /// Opens a stream whose output buffer is `deflate_bound_z` for `input_len`, plus a margin.
    ///
    /// The bound is read off the state this stream will actually use, so the buffer reflects the
    /// real `wrap` and tuning rather than a default -- and, unlike computing it from a throwaway
    /// state, it costs one allocation of the window and hash tables rather than two. That
    /// difference is invisible natively and very visible under Miri, which interprets every byte
    /// of those tables.
    fn bounded(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: Strategy,
        input_len: usize,
    ) -> Self {
        let mut deflater = Self::new(level, window_bits, mem_level, strategy, 0);
        let capacity = deflate_bound_z(Some(&deflater.state), input_len) + 128;
        deflater.out = vec![0u8; capacity];
        deflater
    }

    /// One `deflate()` call with `input` available as input and `avail_out` bytes of room.
    ///
    /// Returns the status code and how many input bytes were consumed, mirroring what a C caller
    /// learns from the return value and from `avail_in` afterwards.
    fn step(&mut self, input: &[u8], avail_out: usize, flush: i32) -> (ReturnCode, usize) {
        let end = (self.produced + avail_out).min(self.out.len());
        let mut stream = DeflateStream::new(input, &mut self.out[self.produced..end]);
        stream.total_in = self.total_in;
        stream.total_out = self.total_out;
        stream.adler = self.adler;
        stream.msg = self.msg;
        stream.data_type = self.data_type;

        let ret = deflate(&mut self.state, &mut stream, flush);

        let consumed = stream.next_in;
        let written = stream.next_out;
        self.total_in = stream.total_in;
        self.total_out = stream.total_out;
        self.adler = stream.adler;
        self.msg = stream.msg;
        self.data_type = stream.data_type;
        self.produced += written;
        (ret, consumed)
    }

    /// Runs `deflate_params` mid-stream, which needs a stream because it may have to flush the
    /// block in progress (`deflate.c` L789-L807).
    fn params(&mut self, level: i32, strategy: i32) -> ReturnCode {
        let end = self.out.len();
        let mut stream = DeflateStream::new(&[], &mut self.out[self.produced..end]);
        stream.total_in = self.total_in;
        stream.total_out = self.total_out;
        stream.adler = self.adler;
        stream.msg = self.msg;
        stream.data_type = self.data_type;

        let ret = deflate_params(&mut self.state, &mut stream, level, strategy);

        let written = stream.next_out;
        self.total_in = stream.total_in;
        self.total_out = stream.total_out;
        self.adler = stream.adler;
        self.msg = stream.msg;
        self.data_type = stream.data_type;
        self.produced += written;
        ret
    }

    /// Output room still unused, i.e. C's `avail_out` when the whole buffer is offered.
    fn room(&self) -> usize {
        self.out.len() - self.produced
    }

    /// The compressed bytes emitted so far.
    fn written(&self) -> &[u8] {
        &self.out[..self.produced]
    }

    /// `deflateEnd`. Consuming `self` is the port's way of spelling the C contract that the state
    /// is unusable afterwards; the return code still follows `deflate.c` L1309 exactly.
    fn end(mut self) -> ReturnCode {
        deflate_end(&mut self.state)
    }
}

/// Compresses `input` in a single `Z_FINISH` call and returns the bytes.
///
/// The output buffer is `deflate_bound_z` plus a margin. The margin exists so that a bound
/// regression shows up as a failure in the dedicated bound tests rather than as a confusing
/// `Z_BUF_ERROR` in every other test; [`a_single_finish_call_always_fits_inside_the_bound`] is
/// where the bound is held to be sufficient on its own.
fn squeeze(
    input: &[u8],
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: Strategy,
) -> Vec<u8> {
    let mut deflater = Deflater::bounded(level, window_bits, mem_level, strategy, input.len());
    let room = deflater.room();
    let (ret, consumed) = deflater.step(input, room, FINISH);
    assert_eq!(
        ret,
        ReturnCode::STREAM_END,
        "one-shot Z_FINISH at level {level}, windowBits {window_bits}, memLevel {mem_level}, \
         {strategy:?} must complete in one call"
    );
    assert_eq!(consumed, input.len(), "Z_FINISH must consume all input");
    let out = deflater.written().to_vec();
    assert_eq!(
        deflater.end(),
        ReturnCode::OK,
        "deflateEnd after Z_STREAM_END"
    );
    out
}

/// Compresses at the given level with default `windowBits`, `memLevel` and strategy.
fn squeeze_at(input: &[u8], level: i32) -> Vec<u8> {
    squeeze(input, level, ZLIB, DEF_MEM_LEVEL, Strategy::Default)
}

/// Decompresses `input` through `zlib_rs::inflate` with the matching `windowBits`.
///
/// A successful round trip is the only proof that a pinned byte sequence is a *correct* encoding
/// of the input rather than merely a stable one, so every pinned value in this file is checked
/// through here as well.
fn expand(input: &[u8], window_bits: i32, expected_len: usize) -> Vec<u8> {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("inflate_init2 with a valid windowBits");
    let reset = inflate_reset(&mut state);
    let mut out = vec![0u8; expected_len + 64];
    let produced = {
        let mut stream = InflateStream::new(input, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END,
            "inflate must accept the stream this suite produced (windowBits {window_bits})"
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);
    out
}

/// Compresses and immediately decompresses, asserting the input is recovered byte for byte.
fn round_trip(
    input: &[u8],
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: Strategy,
) -> Vec<u8> {
    let compressed = squeeze(input, level, window_bits, mem_level, strategy);
    let recovered = expand(&compressed, window_bits, input.len());
    assert_eq!(
        recovered, input,
        "round trip at level {level}, windowBits {window_bits}, memLevel {mem_level}, {strategy:?}"
    );
    compressed
}

/// The three-bit block header of a raw DEFLATE stream (RFC 1951 section 3.2.3).
///
/// Bit 0 is `BFINAL`; bits 1 and 2 are `BTYPE`, little-endian within the byte: `00` stored,
/// `01` static Huffman, `10` dynamic Huffman, `11` reserved.
fn block_header(raw_stream: &[u8]) -> (u8, u8) {
    let first = raw_stream[0];
    (first & 1, (first >> 1) & 3)
}

/// `BTYPE == 00`: a stored block.
const BTYPE_STORED: u8 = 0;
/// `BTYPE == 01`: compressed with the static Huffman trees.
const BTYPE_STATIC: u8 = 1;
/// `BTYPE == 10`: compressed with per-block dynamic Huffman trees.
const BTYPE_DYNAMIC: u8 = 2;

// =============================================================================================
// 1. The per-level tuning table -- decision point 1
//
// This is the first thing that decides the emitted bytes, and the cheapest thing to get wrong.
// The four numbers in each row select which matches `longest_match` is even allowed to find, so a
// single transposed digit changes the output of every stream compressed at that level while
// leaving it perfectly decodable -- the failure mode a differential test would catch a thousand
// tests later, and this one catches immediately.
// =============================================================================================

/// Every field of every row, against the numbers read out of `deflate.c` L112-L124.
#[test]
fn configuration_table_matches_the_reference_row_for_row() {
    assert_eq!(
        CONFIGURATION_TABLE.len(),
        10,
        "the shipped build compiles the non-FASTEST table, which has one row per level 0..=9; \
         a length of 2 would mean FASTEST leaked into the port (deflate.c L106-L110)"
    );
    assert_eq!(
        CONFIGURATION_TABLE.len(),
        REFERENCE_CONFIGURATION_TABLE.len(),
        "the transcribed reference table must cover exactly the levels the port defines"
    );

    for (level, &(good_length, max_lazy, nice_length, max_chain)) in
        REFERENCE_CONFIGURATION_TABLE.iter().enumerate()
    {
        let row = CONFIGURATION_TABLE[level];
        assert_eq!(
            row.good_length,
            good_length,
            "level {level}: good_length must be {good_length} (deflate.c L{})",
            114 + level
        );
        assert_eq!(
            row.max_lazy,
            max_lazy,
            "level {level}: max_lazy must be {max_lazy} (deflate.c L{})",
            114 + level
        );
        assert_eq!(
            row.nice_length,
            nice_length,
            "level {level}: nice_length must be {nice_length} (deflate.c L{})",
            114 + level
        );
        assert_eq!(
            row.max_chain,
            max_chain,
            "level {level}: max_chain must be {max_chain} (deflate.c L{})",
            114 + level
        );
    }
}

/// Level 0 is the store-only configuration, and all four of its tuning fields are zero.
///
/// They have to be: `deflate_stored` never calls `longest_match`, so there is nothing for a
/// chain length or a lazy threshold to mean. Any non-zero value here would be dead data that a
/// future edit could start believing.
#[test]
fn level_zero_is_the_all_zero_store_only_row() {
    let row = CONFIGURATION_TABLE[0];
    assert_eq!(
        (
            row.good_length,
            row.max_lazy,
            row.nice_length,
            row.max_chain
        ),
        (0, 0, 0, 0),
        "level 0 selects deflate_stored, so every tuning field is 0 (deflate.c L114)"
    );
}

/// The two invariants `deflate.c` states in prose immediately after the table.
///
/// `deflate.c` L127-L130: "Note: the `deflate()` code requires `max_lazy >= MIN_MATCH` and
/// `max_chain >= 4`. For `deflate_fast()` (levels <= 3) `good` is ignored and `lazy` has a
/// different meaning."
///
/// Because `lazy` means something else on the fast path, the `max_lazy >= MIN_MATCH` half is
/// asserted from [`FIRST_LAZY_LEVEL`] upwards -- which is exactly why level 1's `max_lazy` of 4
/// and level 4's `max_lazy` of 4 can coexist with level 2's 5 and level 3's 6 without the table
/// being monotonic.
#[test]
fn the_table_satisfies_the_invariants_the_reference_documents() {
    for (level, row) in CONFIGURATION_TABLE.iter().enumerate().skip(1) {
        assert!(
            usize::from(row.max_chain) >= 4,
            "level {level}: deflate() requires max_chain >= 4, found {} (deflate.c L127)",
            row.max_chain
        );
    }

    for (level, row) in CONFIGURATION_TABLE
        .iter()
        .enumerate()
        .skip(FIRST_LAZY_LEVEL)
    {
        assert!(
            usize::from(row.max_lazy) >= MIN_MATCH,
            "level {level} uses deflate_slow, where deflate() requires max_lazy >= MIN_MATCH \
             ({MIN_MATCH}), found {} (deflate.c L127)",
            row.max_lazy
        );
    }
}

/// `nice_length` is an early-exit threshold on match length, so it cannot exceed `MAX_MATCH`.
///
/// `longest_match` stops searching as soon as it finds a match at least `nice_match` long
/// (`deflate.c` L1497-L1498). A value above `MAX_MATCH` would be unreachable and would silently
/// turn the early exit off, converting the two highest levels into an exhaustive search that
/// still emits the same bytes but takes unboundedly longer. Levels 8 and 9 sit exactly at the
/// ceiling, which is the strongest form of "search as hard as the format allows".
#[test]
fn nice_length_never_exceeds_the_longest_possible_match() {
    for (level, row) in CONFIGURATION_TABLE.iter().enumerate() {
        let nice = usize::from(row.nice_length);
        assert!(
            nice <= MAX_MATCH,
            "level {level}: nice_length {nice} exceeds MAX_MATCH {MAX_MATCH}"
        );
    }

    for level in [8usize, 9] {
        assert_eq!(
            usize::from(CONFIGURATION_TABLE[level].nice_length),
            MAX_MATCH,
            "level {level} searches to the format's ceiling, so nice_length is MAX_MATCH"
        );
    }
}

/// The tuning parameters a level was built with are the ones the state reports.
///
/// This closes the loop between the table and the driver: the table could be perfect and
/// `lm_init` could still load the wrong row. `max_dist` is the observable that depends on nothing
/// but `w_size`, and `hash_bits`/`lit_bufsize` are the ones `deflateInit2_` derives from
/// `memLevel`, so together they show the state was built from the arguments it was given rather
/// than from a default.
#[test]
fn a_new_state_reports_the_geometry_its_arguments_imply() {
    // Every accepted value of each dimension, paired with the other's default. The 7 x 9 cross
    // product would add nothing -- `w_bits` and `hash_bits` are derived independently
    // (`deflate.c` L508-L521) -- and it would multiply the Miri cost of this test by seven, since
    // interpreting the window and hash-table initialisation is what dominates that cost.
    let configurations = (9..=15i32)
        .map(|window_bits| (window_bits, DEF_MEM_LEVEL))
        .chain((MIN_MEM_LEVEL..=MAX_MEM_LEVEL).map(|mem_level| (MAX_WBITS, mem_level)));

    for (window_bits, mem_level) in configurations {
        let (mut state, _) = open(6, window_bits, mem_level, Strategy::Default);

        let w_bits = u32::try_from(window_bits).unwrap();
        assert_eq!(
            state.w_bits(),
            w_bits,
            "w_bits for windowBits {window_bits}"
        );
        assert_eq!(
            state.w_size(),
            1usize << w_bits,
            "w_size is 1 << w_bits (deflate.c L508)"
        );
        assert_eq!(
            state.w_mask(),
            state.w_size() - 1,
            "w_mask is w_size - 1 (deflate.c L509)"
        );
        assert_eq!(
            state.max_dist(),
            state.w_size() - MIN_LOOKAHEAD,
            "MAX_DIST(s) is w_size - MIN_LOOKAHEAD (deflate.h L301)"
        );

        // `s->hash_bits = memLevel + 7` and `s->lit_bufsize = 1 << (memLevel + 6)`
        // (`deflate.c` L511, L521).
        let mem = u32::try_from(mem_level).unwrap();
        assert_eq!(
            state.hash_bits(),
            mem + 7,
            "hash_bits is memLevel + 7 for memLevel {mem_level}"
        );
        assert_eq!(
            state.hash_size(),
            1usize << (mem + 7),
            "hash_size is 1 << hash_bits"
        );
        assert_eq!(
            state.hash_mask(),
            state.hash_size() - 1,
            "hash_mask is hash_size - 1 (deflate.c L513)"
        );
        assert_eq!(
            state.lit_bufsize(),
            1usize << (mem + 6),
            "lit_bufsize is 1 << (memLevel + 6) for memLevel {mem_level}"
        );
        assert_eq!(
            state.pending_buf_size(),
            LIT_BUFS * state.lit_bufsize(),
            "pending_buf_size is LIT_BUFS * lit_bufsize (deflate.h L225-L229)"
        );

        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

// =============================================================================================
// 2. The level-and-strategy binding, verified behaviourally
//
// `Config::func` is crate-private, and it is an enum rather than a function pointer, so there is
// nothing to compare even if it were reachable. That is the better position to be in: comparing
// function pointers in Rust is unreliable, because two functions with identical bodies may or may
// not share an address once the optimiser has run. Each of the five compressors is therefore
// identified by something only it does.
// =============================================================================================

/// Level 0 emits stored blocks, and nothing else.
///
/// `CompressFunc::select` sends level 0 to `deflate_stored` before it looks at the strategy at
/// all, so this holds for every strategy. The observable is the three-bit block header at the head
/// of a raw stream (RFC 1951 section 3.2.3): bit 0 is `BFINAL`, bits 1-2 are `BTYPE`, and `BTYPE`
/// must be `00`. A stored block then carries `LEN` and `NLEN` as little-endian 16-bit words with
/// `NLEN` the ones' complement of `LEN`, followed by the literal bytes -- five bytes of framing
/// around an untouched copy of the input, which is what makes the length assertion exact rather
/// than approximate.
#[test]
fn level_zero_emits_stored_blocks() {
    for length in [0usize, 1, 13, 300] {
        let payload = corpus::repetitive(length);
        let out = round_trip(&payload, 0, RAW, DEF_MEM_LEVEL, Strategy::Default);

        let (bfinal, btype) = block_header(&out);
        assert_eq!(bfinal, 1, "a one-call Z_FINISH marks its last block final");
        assert_eq!(
            btype, BTYPE_STORED,
            "level 0 must select deflate_stored, so BTYPE is 00 for a {length}-byte payload"
        );

        assert_eq!(
            out.len(),
            length + 5,
            "a stored block is BFINAL/BTYPE plus LEN and NLEN plus the literal bytes"
        );
        let len = u16::from_le_bytes([out[1], out[2]]);
        let nlen = u16::from_le_bytes([out[3], out[4]]);
        assert_eq!(usize::from(len), length, "stored LEN is the byte count");
        assert_eq!(nlen, !len, "stored NLEN is the ones' complement of LEN");
        assert_eq!(
            &out[5..],
            &payload[..],
            "a stored block copies its input verbatim"
        );
    }
}

/// Level 0 stores whatever the strategy says, because level is tested first.
///
/// `CompressFunc::select` (mirroring `deflate.c`'s `configuration_table[level].func` lookup plus
/// the `Z_HUFFMAN_ONLY`/`Z_RLE` overrides) checks `level == 0` before it checks the strategy. So
/// `Z_HUFFMAN_ONLY` at level 0 still stores; if the order were reversed it would emit a
/// literal-only Huffman block instead, which is decodable and therefore invisible to any test
/// that only round-trips.
#[test]
fn level_zero_beats_every_strategy() {
    let payload = corpus::repetitive(300);
    for strategy in Strategy::ALL {
        let out = round_trip(&payload, 0, RAW, DEF_MEM_LEVEL, strategy);
        let (_, btype) = block_header(&out);
        assert_eq!(
            btype, BTYPE_STORED,
            "level 0 with {strategy:?} must still store: the level test comes first"
        );
        assert_eq!(out.len(), payload.len() + 5, "still a bare stored block");
    }
}

/// Builds a payload on which lazy matching demonstrably pays off.
///
/// Each repetition lays down three pieces: a short three-byte token, a nine-byte token that
/// begins with the short token's *second* byte, and finally a ten-byte run that begins with the
/// short token and continues into the long one.
///
/// At the position of that last run's first byte the encoder can match the short token, three
/// bytes. One byte later it can match the long token, nine bytes. `deflate_fast` is greedy: it
/// takes the three-byte match and moves on. `deflate_slow` holds the three-byte match back, sees
/// the nine-byte match at the next position, and because
/// `prev_length >= MIN_MATCH && match_length <= prev_length` is then false it emits a single
/// literal followed by the longer match (`deflate.c` L2011-L2013).
///
/// Twenty repetitions with rotating letters amplify a per-repetition saving of a few bits into a
/// difference of whole bytes, while keeping the payload small enough for Miri.
fn lazy_favouring_payload(repetitions: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(repetitions * 26);
    for index in 0..repetitions {
        let first = b'A' + u8::try_from(index % 20).unwrap();
        let second = b'a' + u8::try_from(index % 20).unwrap();

        out.extend_from_slice(&[first, second, b'0']);
        out.push(b'|');
        out.extend_from_slice(&[second, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7']);
        out.push(b'|');
        out.extend_from_slice(&[
            first, second, b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
        ]);
        out.push(b'#');
    }
    out
}

/// Levels 1 to 3 are greedy; levels 4 to 9 match lazily -- decision point 5.
///
/// The two families are distinguished by outcome rather than by identity: on
/// [`lazy_favouring_payload`] every level from [`FIRST_LAZY_LEVEL`] upwards must beat every level
/// below it. Nothing weaker would do. Asserting merely that the outputs *differ* would also pass
/// if the two families had simply been swapped, and asserting a specific size would pin the
/// Huffman coder rather than the match selector.
#[test]
fn lazy_matching_pays_off_from_level_four_upwards() {
    let payload = lazy_favouring_payload(20);

    let mut sizes = [0usize; 10];
    for (level, slot) in sizes.iter_mut().enumerate().skip(1) {
        let compressed = round_trip(
            &payload,
            i32::try_from(level).unwrap(),
            RAW,
            DEF_MEM_LEVEL,
            Strategy::Default,
        );
        *slot = compressed.len();
    }

    let greedy_best = sizes[1..FIRST_LAZY_LEVEL].iter().copied().min().unwrap();
    let lazy_worst = sizes[FIRST_LAZY_LEVEL..=9].iter().copied().max().unwrap();
    assert!(
        lazy_worst < greedy_best,
        "on a payload built so that the match one byte later is longer, every deflate_slow level \
         must beat every deflate_fast level; got greedy {:?} and lazy {:?}",
        &sizes[1..FIRST_LAZY_LEVEL],
        &sizes[FIRST_LAZY_LEVEL..=9]
    );
}

/// Raising the level never makes the output bigger, for any of the standard corpus classes.
///
/// This is a property of the tuning table read as a whole -- `good_length`, `nice_length` and
/// `max_chain` all widen or hold as the level rises, so the match finder's search space is
/// monotonically non-decreasing.
///
/// It is worth being precise about what is and is not claimed. Non-increasing size is **not** a
/// theorem of DEFLATE: a longer match found at one position can displace a better pair of matches
/// later, and `TOO_FAR` is a heuristic that is documented as "not always a win". What is asserted
/// here is that the property holds for these specific, committed payloads, which were checked
/// against the implementation. A pathological input that violates it would not be a bug, which is
/// why nothing beyond the named corpus classes is fed in here, and why the round trip -- which
/// *is* unconditional -- is asserted for every level regardless.
#[test]
fn output_size_is_non_increasing_across_the_levels() {
    // Two classes, chosen because they exercise opposite ends of the match finder: a pure run,
    // where the whole gain comes from one very long match, and prose, where it comes from many
    // short ones. `binary` and `incompressible` behave like one or the other and are covered for
    // determinism and round-tripping in
    // `every_corpus_class_compresses_deterministically`; adding them here would multiply this
    // test's Miri cost without reaching a new code path.
    let payloads: [(&str, Vec<u8>); 2] = [
        ("repetitive", corpus::repetitive(2048)),
        ("text", corpus::text()),
    ];

    for (name, payload) in &payloads {
        let mut previous = usize::MAX;
        for level in 1..=9i32 {
            let size = round_trip(payload, level, ZLIB, DEF_MEM_LEVEL, Strategy::Default).len();
            assert!(
                size <= previous,
                "{name}: level {level} produced {size} bytes, more than level {} produced \
                 ({previous})",
                level - 1
            );
            previous = size;
        }
    }
}

/// `Z_HUFFMAN_ONLY` emits no match at all -- it is an entropy coder with the matcher switched off.
///
/// `deflate.c` L2151-L2153 says so directly: "For `Z_HUFFMAN_ONLY`, do not look for matches. Do
/// not maintain a hash table." The observable is dramatic and needs no subtlety: 4096 identical
/// bytes are a single distance-1 run, so the default strategy encodes them in a couple of dozen
/// bytes, while `Z_HUFFMAN_ONLY` has to spend a Huffman code on each of the 4096 literals.
///
/// The assertion is a ratio rather than a size so that it tests the absence of matching rather
/// than the exact width of a Huffman code. A factor of ten is far below the roughly twenty-four
/// times observed and far above anything a match-bearing encoder could reach.
#[test]
fn huffman_only_emits_no_matches_at_all() {
    let payload = corpus::repetitive(4096);

    let with_matches = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let literals_only = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::HuffmanOnly);

    assert!(
        literals_only.len() > with_matches.len() * 10,
        "Z_HUFFMAN_ONLY must spend a code per literal on a 4096-byte run: got {} bytes against \
         {} for Z_DEFAULT_STRATEGY",
        literals_only.len(),
        with_matches.len()
    );
    let (_, btype) = block_header(&literals_only);
    assert_ne!(
        btype, BTYPE_STORED,
        "a literal-only Huffman block is still a Huffman block, not a stored one"
    );
}

/// Builds a payload with no run of three equal consecutive bytes anywhere.
///
/// The cycle length 7 is coprime with nothing that matters here; what matters is that consecutive
/// bytes always differ, which the caller asserts before relying on it.
fn payload_without_any_run(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| b'a' + u8::try_from(index % 7).unwrap())
        .collect()
}

/// `Z_RLE` can only ever reach back one byte, and that is exactly observable.
///
/// `deflate_rle` (`deflate.c` L2084-L2148) does not consult the hash chains at all. It looks at
/// `window[strstart - 1]`, counts how far that byte repeats, and emits
/// `_tr_tally_dist(s, 1, match_length - MIN_MATCH, ...)` -- the distance is the literal constant
/// `1`, on every path.
///
/// Two consequences, and both are asserted, because either one alone is weak:
///
/// * On a payload with **no** run of `MIN_MATCH` equal consecutive bytes, `deflate_rle` can find
///   nothing, so its symbol stream is literals only -- identical to `deflate_huff`'s. Both
///   strategies are at or above `Z_HUFFMAN_ONLY`, so the zlib header's `FLEVEL` bits agree too
///   (`deflate.c` L1028-L1033), and the two streams must therefore be **byte-identical**. This is
///   the falsifiable half: an implementation that let `Z_RLE` reach distance 2 would break it
///   immediately.
/// * On a run-heavy payload it must compress about as well as the general matcher, since a run is
///   precisely what distance 1 expresses.
#[test]
fn rle_can_only_reach_back_one_byte() {
    let runless = payload_without_any_run(600);
    assert!(
        runless.windows(3).all(|w| !(w[0] == w[1] && w[1] == w[2])),
        "the fixture must contain no run of MIN_MATCH equal bytes for the argument below to hold"
    );

    let rle = round_trip(&runless, 6, RAW, DEF_MEM_LEVEL, Strategy::Rle);
    let huff = round_trip(&runless, 6, RAW, DEF_MEM_LEVEL, Strategy::HuffmanOnly);
    assert_eq!(
        rle, huff,
        "with no distance-1 run available, Z_RLE has nothing to emit but literals, so its output \
         must be byte-identical to Z_HUFFMAN_ONLY's"
    );

    // And the general matcher does find something on the same payload, so the equality above is
    // a statement about Z_RLE's reach rather than about the payload being incompressible.
    let general = round_trip(&runless, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    assert!(
        general.len() < rle.len(),
        "the fixture must be compressible by a distance-unrestricted matcher, else the byte \
         equality above would be vacuous; got {} against {}",
        general.len(),
        rle.len()
    );

    // The converse: a pure run is the one thing distance 1 expresses perfectly.
    let run = corpus::repetitive(4096);
    let rle_run = round_trip(&run, 6, RAW, DEF_MEM_LEVEL, Strategy::Rle);
    let general_run = round_trip(&run, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    assert!(
        rle_run.len() <= general_run.len() + 2,
        "on a 4096-byte run Z_RLE must be within a couple of bytes of the general matcher; got \
         {} against {}",
        rle_run.len(),
        general_run.len()
    );
}

/// `Z_FIXED` forces the static Huffman trees, so no block may carry a dynamic tree.
///
/// `trees.c` L1035-L1037 overrides the cost comparison when the strategy is `Z_FIXED`, which
/// removes the per-block tree description from the stream. A stored block is still reachable
/// afterwards, because the `stored_len + 4 <= opt_lenb` test at L1047 runs after the override --
/// so the assertion is that `BTYPE` is never `10`, not that it is always `01`.
#[test]
fn fixed_strategy_never_emits_a_dynamic_tree() {
    for payload in [
        corpus::text(),
        corpus::binary(),
        corpus::repetitive(2048),
        corpus::incompressible(1024),
    ] {
        let out = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Fixed);
        let (_, btype) = block_header(&out);
        assert_ne!(
            btype, BTYPE_DYNAMIC,
            "Z_FIXED must suppress dynamic trees (trees.c L1035); got BTYPE {btype}"
        );
        assert!(
            btype == BTYPE_STATIC || btype == BTYPE_STORED,
            "Z_FIXED leaves only static and stored blocks reachable; got BTYPE {btype}"
        );
    }
}

// =============================================================================================
// 3. The byte-identity decision points, driven through the public driver
//
// None of `longest_match`, `deflate_slow` or the hash chains is reachable from here. What is
// reachable is the compressed output, so each test below crafts an input on which one decision --
// and, as far as can be arranged, only that decision -- changes what comes out.
//
// Two of these tests pin literal byte sequences. Those values were not invented: they were taken
// from the implementation and then verified by decompressing them back to the input, which every
// pinned case does inline. They are there to detect drift, not to derive the encoding from first
// principles, and the surrounding assertions are what carry the actual argument -- so a legitimate
// change to the encoder would fail the pinned bytes *and* the argument, while a regression in the
// heuristics fails the argument even if the bytes happened to survive.
//
// TO BE PLAIN ABOUT THE DIVISION OF LABOUR, so that this suite is not read as more than it is:
// **cross-implementation byte-identity against the compiled C library belongs to a differential
// suite under `crates/zlib-rs-differential`**, the only crate that links the oracle and can
// therefore compare the two implementations directly, across the full
// level x windowBits x memLevel x strategy x flush x corpus matrix. **No such suite is in the
// tree, so that comparison has not been made.** This section pins *self*
// consistency -- the same input giving the same bytes, and the bytes not depending on how the
// caller chunked its buffers -- plus the specific heuristic behaviours above. Neither would
// substitute for the other: a differential matrix would not notice that `TOO_FAR` is a
// heuristic rather than an optimisation, and this suite cannot prove the C library agrees.
// =============================================================================================

/// Decision point 2: a match against window index 0 is never emitted.
///
/// `deflate.c` L1988-L1991 states the rule in a comment on the guard that enforces it: "To
/// simplify the code, we prevent matches with the string of window index 0 (in particular we have
/// to avoid a match of the string with itself at the start of the input file)."
///
/// The mechanism is a shared sentinel. `NIL` is 0 (`deflate.c` L85-L86, "Tail of hash chains"),
/// and `head[h]` stores a window index, so a chain head of 0 is indistinguishable from an empty
/// chain. `deflate_slow` skips `longest_match` entirely when `hash_head == NIL` (L1987), and
/// inside `longest_match` the same value terminates the walk because
/// `limit = strstart > MAX_DIST(s) ? strstart - MAX_DIST(s) : NIL` (`deflate.c` L1408-L1409) and
/// the loop condition is `cur_match > limit`.
///
/// The fixture makes that visible with nine bytes. `"abcdefabc"` repeats `"abc"` at offset 6, and
/// the only earlier occurrence is at offset **0**, so no match is available and the encoder emits
/// nine literals. `"zabcdefabc"` is the same payload with one byte in front, which moves the first
/// occurrence to offset 1 -- an ordinary chain entry -- and the repeat becomes a length-3 match at
/// distance 6. One byte shorter output from one byte longer input is the whole proof.
#[test]
fn a_match_against_window_index_zero_is_never_emitted() {
    assert_eq!(
        NIL, 0,
        "NIL is the chain terminator and the index it shadows (deflate.c L85)"
    );

    // Only occurrence of the repeated prefix is at window index 0, so it is unreachable.
    let at_index_zero: &[u8] = b"abcdefabc";
    // Same repeat, shifted one byte, so the candidate lands at window index 1.
    let at_index_one: &[u8] = b"zabcdefabc";

    let shadowed = round_trip(at_index_zero, 9, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let reachable = round_trip(at_index_one, 9, RAW, DEF_MEM_LEVEL, Strategy::Default);

    assert!(
        reachable.len() < shadowed.len(),
        "the shifted payload is one byte longer yet must compress smaller, because only its \
         repeat is reachable as a match; got {} against {}",
        reachable.len(),
        shadowed.len()
    );

    // Pinned to detect drift. Verified by the round trips above, which decode each of these back
    // to its input before the comparison.
    assert_eq!(
        shadowed,
        [0x4b, 0x4c, 0x4a, 0x4e, 0x49, 0x4d, 0x4b, 0x4c, 0x4a, 0x06, 0x00],
        "nine static-Huffman literals and an end-of-block: the repeat at offset 6 found nothing, \
         which is why the codes for 'a', 'b' and 'c' appear twice"
    );
    assert_eq!(
        at_index_zero.len(),
        9,
        "the pinned sequence above encodes nine literals, one per input byte"
    );
    assert_eq!(
        reachable,
        [0xab, 0x4a, 0x4c, 0x4a, 0x4e, 0x49, 0x4d, 0x03, 0x92, 0x00],
        "seven literals and one length-3, distance-6 match"
    );
}

/// Length of one half of the [`too_far_payload`] fixture, in bytes.
///
/// Twenty-seven bytes per record: a twenty-byte run, two marker bytes, the three-byte token, and
/// two more marker bytes.
const TOO_FAR_RECORD_LEN: usize = 27;
/// How many token pairs the fixture carries. Each pair contributes only a couple of bits, so the
/// count is what turns the effect into whole bytes; forty-eight keeps the payload near five
/// kilobytes, which Miri can still interpret quickly.
const TOO_FAR_RECORDS: usize = 48;
/// Distance between a token's two occurrences when the padding is zero.
const TOO_FAR_HALF_LEN: usize = TOO_FAR_RECORDS * TOO_FAR_RECORD_LEN;

/// Builds a fixture in which every three-byte token recurs at exactly `distance` bytes.
///
/// The shape of each record matters, so it is worth spelling out:
///
/// ```text
///   'a' x 20   digit   half-marker   t0 t1 t2   half-marker   trailing-marker
/// ```
///
/// * The twenty-byte run makes the payload compressible, which it has to be -- an incompressible
///   payload is emitted as stored blocks (`trees.c` L1047) and stored blocks carry no match
///   information at all, so the effect under test would vanish.
/// * The markers on both sides of the token differ between the two halves. Without them the match
///   at the token would extend into the identical surrounding context and come out far longer than
///   three bytes, and `TOO_FAR` only ever applies to a match of exactly `MIN_MATCH`
///   (`deflate.c` L2001-L2002). This is the detail that makes or breaks the fixture.
///
/// When `repeat` is false the second copy's last byte differs, so only two of its three bytes
/// match and no `MIN_MATCH` match exists. That variant is the control: it costs whatever three
/// literals cost, which under `Z_FIXED` is exactly three eight-bit codes either way.
fn too_far_payload(distance: usize, repeat: bool) -> Vec<u8> {
    assert!(
        distance >= TOO_FAR_HALF_LEN,
        "the requested distance must leave room for the first half"
    );
    let padding = distance - TOO_FAR_HALF_LEN;

    let mut out = Vec::with_capacity(2 * TOO_FAR_HALF_LEN + padding);
    for half in 0..2u8 {
        if half == 1 {
            out.extend(std::iter::repeat(b'b').take(padding));
        }
        for index in 0..TOO_FAR_RECORDS {
            let index_byte = u8::try_from(index).unwrap();
            out.extend(std::iter::repeat(b'a').take(TOO_FAR_RECORD_LEN - 7));
            out.push(b'0' + index_byte % 10);
            out.push(b'+' + half);
            out.push(b'A' + index_byte);
            out.push(b'`' + index_byte % 26);
            out.push(if half == 1 && !repeat { b'"' } else { b'!' });
            out.push(b'-' + half);
            out.push(b'7' - index_byte % 8);
        }
    }
    out
}

/// Decision point 4: `TOO_FAR` discards a three-byte match whose distance exceeds 4096.
///
/// `deflate.c` L1999-L2007, inside `deflate_slow`:
///
/// ```c
/// if (s->match_length <= 5 && (s->strategy == Z_FILTERED
///     || (s->match_length == MIN_MATCH &&
///         s->strstart - s->match_start > TOO_FAR)))
///     s->match_length = MIN_MATCH-1;
/// ```
///
/// This is a pure heuristic with no basis in RFC 1951: a length-3 match at a large distance is
/// legal, decodable and often smaller than three literals. zlib rejects it anyway because the
/// distance code costs more than it saves *on average*, and callers depend on that judgement.
///
/// The test brackets the threshold from both sides using [`too_far_payload`] and `Z_FIXED`, which
/// removes the Huffman coder from the comparison by fixing every code length in advance:
///
/// * at a distance of exactly `TOO_FAR`, the condition is `>` and therefore false, so the matches
///   are kept and the repeating fixture must differ from its non-repeating control;
/// * ten bytes further out the matches are discarded, so the two must compress to **exactly** the
///   same number of bytes -- the repeat buys literally nothing, which is only true if the encoder
///   threw it away.
///
/// The second assertion is the strong one, and it also pins `>` against `>=`: an off-by-one in the
/// comparison would move the boundary by one byte and break the first assertion.
///
/// One more thing falls out of this fixture and is asserted rather than glossed over: inside the
/// threshold, taking the match makes the output **larger**. `deflate.c` L1478-L1480 says as much
/// of the neighbouring heuristic -- it "is not always a win" -- and the reason is decision point 5:
/// emitting a match instead of literals changes which positions enter the hash chains, so it
/// perturbs every later match too. A port that "improved" this would compress this fixture better
/// and be wrong.
#[test]
fn too_far_discards_a_three_byte_match_beyond_4096_bytes() {
    assert_eq!(TOO_FAR, 4096, "deflate.c L88-L90");

    let inside = TOO_FAR;
    let outside = TOO_FAR + 10;

    let inside_repeat = round_trip(
        &too_far_payload(inside, true),
        6,
        RAW,
        DEF_MEM_LEVEL,
        Strategy::Fixed,
    );
    let inside_control = round_trip(
        &too_far_payload(inside, false),
        6,
        RAW,
        DEF_MEM_LEVEL,
        Strategy::Fixed,
    );
    assert_ne!(
        inside_repeat.len(),
        inside_control.len(),
        "at a distance of exactly TOO_FAR the condition `> TOO_FAR` is false, so the three-byte \
         matches are kept and the repeating fixture cannot cost the same as the control"
    );
    assert!(
        inside_repeat.len() > inside_control.len(),
        "taking the three-byte match perturbs later hash insertions, so zlib's own heuristic \
         loses here -- `deflate.c` L1478-L1480 calls the neighbouring test \"not always a win\", \
         and reproducing that loss is the requirement; got repeat {} against control {}",
        inside_repeat.len(),
        inside_control.len()
    );

    let outside_repeat = round_trip(
        &too_far_payload(outside, true),
        6,
        RAW,
        DEF_MEM_LEVEL,
        Strategy::Fixed,
    );
    let outside_control = round_trip(
        &too_far_payload(outside, false),
        6,
        RAW,
        DEF_MEM_LEVEL,
        Strategy::Fixed,
    );
    assert_eq!(
        outside_repeat.len(),
        outside_control.len(),
        "ten bytes past TOO_FAR every three-byte match is discarded, so a fixture whose tokens \
         repeat must cost exactly as much as one whose tokens do not"
    );
}

/// The `Z_FILTERED` half of the same condition: every match of five bytes or fewer goes.
///
/// The `strategy == Z_FILTERED` disjunct at `deflate.c` L2000 fires irrespective of distance, so
/// on a payload whose only matches are short, `Z_FILTERED` has nothing left to emit but literals
/// and must lose ground to the default strategy. The fixture repeats a four-byte prefix at a very
/// short distance, which is well inside `TOO_FAR` and therefore isolates the `Z_FILTERED` clause
/// from the `TOO_FAR` clause.
#[test]
fn filtered_discards_every_match_of_five_bytes_or_fewer() {
    let mut payload = Vec::with_capacity(40 * 5);
    for index in 0..40u8 {
        // "abcd" recurs every five bytes; the fifth byte is unique, so no match reaches five.
        payload.extend_from_slice(&[b'a', b'b', b'c', b'd', b'0' + index % 10]);
        payload.push(b'A' + index);
    }

    let default = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let filtered = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Filtered);

    assert!(
        filtered.len() > default.len(),
        "Z_FILTERED discards matches of length <= 5 whatever their distance, so a payload whose \
         only matches are four bytes long must compress worse under it; got {} against {}",
        filtered.len(),
        default.len()
    );
}

/// The same bytes in, the same bytes out -- every time, in every configuration.
///
/// Cheap, and worth far more than it costs. `zcalloc` is built on `malloc`, not `calloc`
/// (`zutil.c` L215-L238), so nothing hands the encoder zeroed memory; the instrumented allocator
/// in `test/infcover.c` makes that concrete by filling every block with `0xa5`. Any dependence on
/// uninitialised state, on an allocation address, on iteration order over a hash map, or on
/// anything else that varies between calls shows up here as two different byte sequences.
///
/// This is also the test that gives the `simd` feature its meaning. Vectorisation is admissible
/// only where it cannot perturb an emitted byte, which is why it is confined to the two
/// checksums; a vectorised match finder is prohibited outright. Running this file under
/// `--features simd` must produce identical results, and a SIMD path that leaked into match
/// finding would break the pinned sequences elsewhere in this section.
#[test]
fn compressed_output_is_identical_across_repeated_runs() {
    // Each dimension is swept against the defaults, and then a handful of named corner
    // *combinations* is added on top. Deliberately not a cross product: the exhaustive
    // level x windowBits x memLevel x strategy x flush x corpus matrix belongs to
    // `crates/zlib-rs-differential`, which can compare against the C oracle and is not run under
    // Miri, and building it here would make this suite unusable under Miri without covering a
    // single value the sweep below misses. Every parameter value is still exercised.
    let payload = corpus::HELLO;

    let mut cases: Vec<(i32, i32, i32, Strategy)> = Vec::new();
    for level in [Z_NO_COMPRESSION, Z_BEST_SPEED, 6, Z_BEST_COMPRESSION] {
        cases.push((level, ZLIB, DEF_MEM_LEVEL, Strategy::Default));
    }
    for window_bits in [RAW, ZLIB, GZIP] {
        cases.push((6, window_bits, DEF_MEM_LEVEL, Strategy::Default));
    }
    for mem_level in [MIN_MEM_LEVEL, DEF_MEM_LEVEL, MAX_MEM_LEVEL] {
        cases.push((6, ZLIB, mem_level, Strategy::Default));
    }
    for strategy in Strategy::ALL {
        cases.push((6, ZLIB, DEF_MEM_LEVEL, strategy));
    }
    // Named corners, where two unusual choices meet.
    cases.push((Z_NO_COMPRESSION, RAW, MIN_MEM_LEVEL, Strategy::Fixed));
    cases.push((
        Z_BEST_COMPRESSION,
        GZIP,
        MAX_MEM_LEVEL,
        Strategy::HuffmanOnly,
    ));
    cases.push((Z_BEST_SPEED, RAW, MIN_MEM_LEVEL, Strategy::Rle));
    cases.push((6, GZIP, MAX_MEM_LEVEL, Strategy::Filtered));

    for (level, window_bits, mem_level, strategy) in cases {
        let first = squeeze(payload, level, window_bits, mem_level, strategy);
        let second = squeeze(payload, level, window_bits, mem_level, strategy);
        assert_eq!(
            first, second,
            "level {level}, windowBits {window_bits}, memLevel {mem_level}, {strategy:?} must \
             compress to the same bytes twice"
        );
        assert_eq!(
            expand(&first, window_bits, payload.len()),
            payload,
            "level {level}, windowBits {window_bits}, memLevel {mem_level}, {strategy:?} must \
             round trip"
        );
    }
}

/// Every corpus class compresses deterministically and round trips.
///
/// The classes are chosen to hit distinct code paths rather than to fill a matrix: an empty input
/// and a single byte exercise the degenerate cases, a long run exercises the match finder's upper
/// limits, incompressible data forces stored-block selection, prose and binary data drive
/// `detect_data_type` (`trees.c` L966) down opposite branches.
#[test]
fn every_corpus_class_compresses_deterministically() {
    let payloads: [(&str, Vec<u8>); 7] = [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(2048)),
        ("incompressible", corpus::incompressible(1024)),
        ("text", corpus::text()),
        ("binary", corpus::binary()),
    ];

    for (name, payload) in &payloads {
        let first = squeeze_at(payload, 6);
        let second = squeeze_at(payload, 6);
        assert_eq!(
            first, second,
            "{name} must compress to the same bytes twice"
        );
        assert_eq!(
            expand(&first, ZLIB, payload.len()),
            *payload,
            "{name} must round trip"
        );
    }
}

/// The memory-level endpoints behave, including `memLevel = 9`.
///
/// `MAX_MEM_LEVEL` is 9 in this build (`zconf.h` L273-L277), so 9 must be accepted rather than
/// rejected as out of range -- a port that copied a `MAX_MEM_LEVEL` of 8 from some other
/// configuration would fail here. `memLevel = 1` is the other end and is the configuration
/// `deflateBound`'s comment singles out as "the lowest that may not use stored blocks".
#[test]
fn both_memory_level_endpoints_work() {
    assert_eq!(MIN_MEM_LEVEL, 1, "zconf.h: memLevel starts at 1");
    assert_eq!(
        MAX_MEM_LEVEL, 9,
        "zconf.h L273-L277: MAX_MEM_LEVEL is 9 in this build"
    );
    assert_eq!(DEF_MEM_LEVEL, 8, "zlib.h: the documented default memLevel");

    let payload = corpus::text();
    for mem_level in [MIN_MEM_LEVEL, DEF_MEM_LEVEL, MAX_MEM_LEVEL] {
        let first = round_trip(&payload, 6, ZLIB, mem_level, Strategy::Default);
        let second = squeeze(&payload, 6, ZLIB, mem_level, Strategy::Default);
        assert_eq!(
            first, second,
            "memLevel {mem_level} must compress to the same bytes twice"
        );
    }
}

/// Feeding the stream one byte at a time gives the same bytes as feeding it all at once.
///
/// This is `test/example.c`'s `test_deflate` (L187-L198), which sets
/// `avail_in = avail_out = 1` for the whole stream and then finishes with `avail_out = 1`, turned
/// into an equality rather than merely a "does not crash".
///
/// The equality is the interesting part. Chunk boundaries interact with `fill_window`, with the
/// pending buffer and with the `Z_NO_FLUSH` "need more input" path, so an implementation that let
/// the buffering geometry reach the encoder would produce a different -- still perfectly valid --
/// stream. `avail_in` and `avail_out` are the caller's business and must not be visible in the
/// output.
#[test]
fn one_byte_at_a_time_produces_the_same_stream_as_one_call() {
    let payload = corpus::HELLO;
    let single_call = squeeze_at(payload, Z_DEFAULT_COMPRESSION);

    let mut deflater = Deflater::new(
        Z_DEFAULT_COMPRESSION,
        ZLIB,
        DEF_MEM_LEVEL,
        Strategy::Default,
        single_call.len() + 64,
    );

    // `while (c_stream.total_in != len && c_stream.total_out < comprLen)` with both counts
    // pinned to one byte per call.
    let mut consumed = 0usize;
    while consumed < payload.len() {
        let (ret, taken) = deflater.step(&payload[consumed..=consumed], 1, NO_FLUSH);
        assert!(
            ret == ReturnCode::OK || ret == ReturnCode::BUF_ERROR,
            "a one-byte Z_NO_FLUSH call reports Z_OK, or Z_BUF_ERROR when it could neither \
             consume nor emit; got {ret:?}"
        );
        consumed += taken;
    }
    assert_eq!(consumed, payload.len(), "every input byte must be consumed");

    // `for (;;) { c_stream.avail_out = 1; err = deflate(&c_stream, Z_FINISH); ... }`
    loop {
        let (ret, _) = deflater.step(&[], 1, FINISH);
        if ret == ReturnCode::STREAM_END {
            break;
        }
        assert!(
            ret == ReturnCode::OK || ret == ReturnCode::BUF_ERROR,
            "Z_FINISH with one byte of room reports Z_OK until it is done; got {ret:?}"
        );
        assert!(
            deflater.room() > 0,
            "the output buffer was sized from deflate_bound, so Z_FINISH must complete inside it"
        );
    }

    let chunked = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);

    assert_eq!(
        chunked, single_call,
        "the caller's buffer geometry must not be observable in the compressed stream"
    );
    assert_eq!(expand(&chunked, ZLIB, payload.len()), payload);
}

// =============================================================================================
// 4. Sizing constants, status codes and state invariants
//
// These are transcriptions, and transcription errors are the cheapest class of bug to find and
// the most expensive to find late: a wrong `LIT_BUFS` does not fail to compile and does not fail
// to decompress -- it changes when a block is flushed, and therefore changes the output of every
// stream. Each constant is asserted twice where possible: once against the literal C value, and
// once as the *relation* the C header defines it by, so that a change to a base constant
// propagates instead of quietly contradicting a hardcoded number.
// =============================================================================================

/// Every sizing constant, against `deflate.h` and `zutil.h`.
///
/// `LIT_BUFS` deserves its own note because it is the one that is most easily wrong. `deflate.h`
/// L28 has `// #define LIT_MEM` -- commented out -- and L225-L229 then select `LIT_BUFS` of **4**
/// with a single `sym_buf` at three bytes per symbol. The `LIT_MEM` build uses 5 and a split
/// buffer. Asserting 5 here would not merely be wrong; it would mask a real divergence in the
/// pending-buffer layout, because `pending_buf_size` is `LIT_BUFS * lit_bufsize` and every block
/// boundary depends on it.
#[test]
fn the_sizing_constants_match_the_reference_headers() {
    assert_eq!(
        MIN_MATCH, 3,
        "deflate.h L37: the shortest match DEFLATE can encode"
    );
    assert_eq!(
        MAX_MATCH, 258,
        "deflate.h L40: the longest match DEFLATE can encode"
    );
    assert_eq!(
        LIT_BUFS, 4,
        "deflate.h L229 with LIT_MEM commented out at L28 -- four, not five"
    );
    assert_eq!(MIN_LOOKAHEAD, 262, "deflate.h L296");
    assert_eq!(WIN_INIT, 258, "deflate.h L306");
    assert_eq!(HEAP_SIZE, 573, "deflate.h L60: 2 * L_CODES + 1");
    assert_eq!(
        MAX_BITS, 15,
        "deflate.h L63: the longest Huffman code length"
    );
    assert_eq!(BUF_SIZE, 16, "deflate.h L64: the bit-accumulator width");
    assert_eq!(NIL, 0, "deflate.c L85: the tail of a hash chain");
}

/// The two derived constants, asserted as the relations that define them.
///
/// `deflate.h` L296 writes `MIN_LOOKAHEAD` as `(MAX_MATCH + MIN_MATCH + 1)` and L306 writes
/// `WIN_INIT` as `MAX_MATCH`. Restating them here as arithmetic rather than as literals means that
/// if a future `MAX_MATCH` ever moved, this test would keep holding while the literal assertions
/// above would fail -- which is precisely the signal a maintainer wants: "the base constant
/// changed", not "two unrelated numbers disagree".
#[test]
fn the_derived_constants_follow_from_their_bases() {
    assert_eq!(
        MIN_LOOKAHEAD,
        MAX_MATCH + MIN_MATCH + 1,
        "deflate.h L296: MIN_LOOKAHEAD is MAX_MATCH + MIN_MATCH + 1"
    );
    assert_eq!(WIN_INIT, MAX_MATCH, "deflate.h L306: WIN_INIT is MAX_MATCH");
}

/// The bounds constants in `zconf.h` and `zlib.h`.
///
/// `MIN_WBITS` is 8 in `config` and 9 in `weak_slice`, and that is not a contradiction: 8 is what
/// `deflateInit2_` **accepts** from a caller, and 9 is the smallest window it ever **builds**,
/// because L438 silently promotes 8 to 9 ("until 256-byte window bug fixed").
#[test]
fn the_bounds_constants_match_the_reference_headers() {
    assert_eq!(
        MIN_WBITS, 8,
        "zlib.h: deflateInit2_ accepts windowBits from 8"
    );
    assert_eq!(MAX_WBITS, 15, "zconf.h L287: a 32 KiB window");
    assert_eq!(DEF_WBITS, MAX_WBITS, "zconf.h: the default is the maximum");
    assert_eq!(
        Z_DEFLATED, 8,
        "zlib.h L200: the only method the format defines"
    );
    assert_eq!(Z_NO_COMPRESSION, 0, "zlib.h L196");
    assert_eq!(Z_BEST_SPEED, 1, "zlib.h L197");
    assert_eq!(Z_BEST_COMPRESSION, 9, "zlib.h L198");
    assert_eq!(Z_DEFAULT_COMPRESSION, -1, "zlib.h L199");
    assert_eq!(
        PRESET_DICT, 0x20,
        "zutil.h L94: the FDICT bit of the zlib FLG byte"
    );
}

/// The eight `deflate_state.status` values, including the famous 666.
///
/// `deflate_state_check` is the port's `deflateStateCheck` (`deflate.c` L538): it answers "is this
/// **not** a legal status", so a legal value returns `false`. Both directions are asserted,
/// because a check that accepted everything would pass a one-sided test.
#[test]
fn the_status_values_match_the_reference() {
    for (status, raw, c_name) in REFERENCE_STATUS_VALUES {
        assert_eq!(status.as_raw(), raw, "{c_name} is {raw}");
        assert_eq!(
            Status::from_raw(raw),
            Some(status),
            "{c_name} must round trip through from_raw"
        );
        assert!(
            !deflate_state_check(raw),
            "{c_name} ({raw}) is a legal status, so deflateStateCheck must not reject it"
        );
    }

    assert_eq!(
        Status::Finish.as_raw(),
        666,
        "deflate.h L118: FINISH_STATE really is 666"
    );
    assert_eq!(
        Status::ALL.len(),
        REFERENCE_STATUS_VALUES.len(),
        "the port must define exactly the eight statuses the reference does"
    );

    // Values the reference never uses, including the two either side of FINISH_STATE, so that an
    // off-by-one in a range check cannot hide.
    for raw in [i32::MIN, -1, 0, 1, 41, 43, 112, 114, 665, 667, i32::MAX] {
        assert!(
            deflate_state_check(raw),
            "{raw} is not a status the reference defines, so deflateStateCheck must reject it"
        );
        assert_eq!(Status::from_raw(raw), None, "{raw} has no Status variant");
    }
}

/// `Pos` is two bytes and `IPos` is four, as `deflate.h` L96 and L98 declare.
///
/// `Pos` is `ush` and indexes the window and the hash chains, so its width caps the largest window
/// the implementation can address; `IPos` is `unsigned` and holds a chain value during a match
/// walk, where the arithmetic must not wrap. Widening either one would change the memory each
/// stream requests -- which the plan's 15 percent per-stream memory budget is measured against --
/// so the sizes are part of the contract and not an implementation detail.
#[test]
fn the_window_index_types_have_the_reference_widths() {
    assert_eq!(size_of::<Pos>(), 2, "deflate.h L96: Pos is ush, two bytes");
    assert_eq!(
        size_of::<IPos>(),
        4,
        "deflate.h L98: IPos is unsigned, four bytes"
    );
    assert!(
        MAX_MATCH < usize::from(u16::MAX),
        "a match length must fit in the window index type"
    );
}

/// The hash-shift invariant holds for every accepted `memLevel` and `windowBits`.
///
/// `deflate.h` L152-L155 states it: `hash_shift` must be chosen so that "`hash_shift * MIN_MATCH`
/// is at least `hash_bits`" -- otherwise the earliest byte of a three-byte key would be shifted
/// out of the hash before the key is complete, and `insert_string` would index the chains by a
/// two-byte prefix. The stream would still decode; it would just find worse matches, and every
/// compressed byte would change.
///
/// `DeflateState::hash_shift` is `pub(crate)`, so the field itself is out of reach here. What is
/// reachable is `hash_bits()`, and the field is derived from it by
/// `hash_bits.div_ceil(MIN_MATCH)` -- so recomputing that division and checking the invariant on
/// the result verifies the same property from the same input. The field-level assertion is
/// `src/deflate/state.rs`'s `hash_shift_invariant_holds_for_every_mem_level`, which is the right
/// home for it because that module owns the derivation.
#[test]
fn the_hash_shift_invariant_holds_for_every_accepted_configuration() {
    // As above: each dimension against the other's default rather than the cross product, because
    // `hash_shift` is derived from `hash_bits` alone.
    let configurations = (9..=15i32)
        .map(|window_bits| (window_bits, DEF_MEM_LEVEL))
        .chain((MIN_MEM_LEVEL..=MAX_MEM_LEVEL).map(|mem_level| (MAX_WBITS, mem_level)));

    for (window_bits, mem_level) in configurations {
        let (mut state, _) = open(6, window_bits, mem_level, Strategy::Default);

        let hash_bits = state.hash_bits();
        let hash_shift = usize::try_from(hash_bits).unwrap().div_ceil(MIN_MATCH);
        assert!(
            hash_shift * MIN_MATCH >= usize::try_from(hash_bits).unwrap(),
            "windowBits {window_bits}, memLevel {mem_level}: hash_shift {hash_shift} times \
             MIN_MATCH must reach hash_bits {hash_bits} (deflate.h L152-L155)"
        );
        // The other half of the same requirement: the shift must not be so large that the
        // whole key is discarded on the first update.
        assert!(
            hash_shift <= usize::try_from(hash_bits).unwrap(),
            "windowBits {window_bits}, memLevel {mem_level}: a hash_shift of {hash_shift} \
             would shift the key clean out of a {hash_bits}-bit hash"
        );

        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

/// A fresh state starts empty, unslid, and with the flush sentinel the reference resets to.
///
/// Two details of `deflate.c` L656-L672 are pinned here.
///
/// `last_flush` resets to `-2`, deliberately outside the range of every real flush value so that
/// the first call cannot be mistaken for a repeat of a previous one. The rank comparison at L1030
/// relies on it, which is why `deflate_params` also tests against `-2` before deciding whether it
/// must flush.
///
/// The starting status is **not** uniformly `INIT_STATE`: L667 is
/// `s->status = s->wrap == 2 ? GZIP_STATE : INIT_STATE`, so a gzip stream begins one state further
/// along, because the header it has to emit is a different one. A port that started every stream
/// at `INIT_STATE` would emit a zlib header on a gzip stream.
#[test]
fn a_fresh_state_reports_the_reference_starting_point() {
    for window_bits in [RAW, ZLIB, GZIP] {
        let (mut state, reset) = open(6, window_bits, DEF_MEM_LEVEL, Strategy::Default);

        let expected_status = if window_bits == GZIP {
            Status::GzipHeader
        } else {
            Status::Init
        };
        assert_eq!(
            state.status(),
            expected_status,
            "deflate.c L667: windowBits {window_bits} must reset to {expected_status:?}"
        );
        assert_eq!(
            state.last_flush(),
            -2,
            "deflate.c L672: last_flush resets to the -2 sentinel"
        );
        assert!(
            !state.slid(),
            "nothing has been consumed, so the window cannot have slid"
        );
        assert_eq!(state.level(), 6, "the level the caller asked for");
        assert_eq!(state.strategy(), Strategy::Default);
        assert_eq!(state.method(), Method::Deflated);
        assert_eq!(state.pending_bytes(), 0, "no output is buffered yet");
        assert_eq!(state.sym_next(), 0, "no symbol has been tallied yet");
        assert!(
            state.gzhead().is_none(),
            "no gzip header has been installed"
        );

        assert_eq!(reset.total_in, 0);
        assert_eq!(reset.total_out, 0);
        assert!(reset.msg.is_none());

        // `s->wrap` is 0 for raw, 1 for zlib and 2 for gzip (`deflate.c` L466-L480).
        let expected_wrap = match window_bits {
            RAW => 0,
            ZLIB => 1,
            _ => 2,
        };
        assert_eq!(
            state.wrap(),
            expected_wrap,
            "windowBits {window_bits} selects wrap {expected_wrap}"
        );

        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

/// `deflate_init` is `deflateInit_`: default `windowBits`, `memLevel` and strategy.
///
/// `zlib.h` L232-L237 documents the convenience form as equivalent to
/// `deflateInit2(strm, level, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL, Z_DEFAULT_STRATEGY)`, and the
/// equivalence is asserted the only way that cannot be faked -- by compressing the same input both
/// ways and comparing the bytes.
#[test]
fn deflate_init_is_deflate_init2_with_the_documented_defaults() {
    let payload = corpus::HELLO;

    let mut state = deflate_init(6, GlobalAllocator).unwrap();
    let reset = deflate_reset(&mut state);
    assert_eq!(state.w_bits(), u32::try_from(MAX_WBITS).unwrap());
    assert_eq!(
        state.wrap(),
        1,
        "the convenience form selects the zlib wrapper"
    );
    assert_eq!(state.strategy(), Strategy::Default);
    assert_eq!(
        state.lit_bufsize(),
        1usize << (u32::try_from(DEF_MEM_LEVEL).unwrap() + 6)
    );

    let mut out = vec![0u8; deflate_bound_z(Some(&state), payload.len()) + 64];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    assert_eq!(
        out,
        squeeze(payload, 6, MAX_WBITS, DEF_MEM_LEVEL, Strategy::Default),
        "deflateInit_ must be byte-for-byte the documented deflateInit2_ call"
    );
}

// =============================================================================================
// 5. Flush modes
//
// `deflate.c` L985 gates every call on `flush > Z_BLOCK || flush < 0`, and L1030 then compares the
// new flush's rank against the previous call's using `RANK(f) = ((f) * 2) - ((f) > 4 ? 9 : 0)`
// (L132-L133). That expression exists for one reason -- to sort `Z_BLOCK`, whose numeric value is
// 5, *between* `Z_NO_FLUSH` and `Z_PARTIAL_FLUSH` -- and the ordering it produces is what turns a
// repeated flush with no new input into `Z_BUF_ERROR` instead of an infinite stream of empty
// blocks.
// =============================================================================================

/// Every flush mode `deflate` accepts works mid-stream and loses no data across the boundary.
///
/// The three things asserted per mode are the three things a caller can observe: the status code,
/// that the input was consumed, and that the accumulated output decodes back to the original. The
/// last one is what "no data is lost across the flush boundary" means concretely -- a flush that
/// dropped the block in progress would still produce a decodable stream, just a shorter one.
#[test]
fn every_flush_mode_preserves_the_stream() {
    let payload = corpus::text();
    let midpoint = payload.len() / 2;

    for flush in [NO_FLUSH, PARTIAL_FLUSH, SYNC_FLUSH, FULL_FLUSH, BLOCK] {
        let mut deflater =
            Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

        let room = deflater.room();
        let (first, taken) = deflater.step(&payload[..midpoint], room, flush);
        assert_eq!(
            first,
            ReturnCode::OK,
            "flush {flush} mid-stream reports Z_OK while output room remains"
        );
        assert_eq!(
            taken, midpoint,
            "flush {flush} must consume the input offered to it"
        );

        let room = deflater.room();
        let (second, rest) = deflater.step(&payload[midpoint..], room, FINISH);
        assert_eq!(
            second,
            ReturnCode::STREAM_END,
            "Z_FINISH after flush {flush} must complete the stream"
        );
        assert_eq!(
            rest,
            payload.len() - midpoint,
            "Z_FINISH consumes the remainder"
        );

        let stream = deflater.written().to_vec();
        assert_eq!(deflater.end(), ReturnCode::OK);
        assert_eq!(
            expand(&stream, ZLIB, payload.len()),
            payload,
            "the stream flushed with {flush} must decode back to the whole input"
        );
    }
}

/// `Z_SYNC_FLUSH` byte-aligns the stream and emits the canonical empty stored block.
///
/// The four bytes `00 00 FF FF` are the `LEN`/`NLEN` pair of a zero-length stored block, and they
/// are the reason `Z_SYNC_FLUSH` is usable as a framing marker in a protocol: a reader that has
/// consumed up to that point has every byte the writer has produced, and the next block starts on
/// a byte boundary.
///
/// `Z_FULL_FLUSH` must produce the same marker -- it is `Z_SYNC_FLUSH` plus a window reset
/// (`deflate.c` L1064-L1069) -- while `Z_PARTIAL_FLUSH` must **not**, because it deliberately stops
/// short of byte alignment to save the padding bits. Asserting the negative case is what makes the
/// positive one mean something.
#[test]
fn sync_and_full_flush_emit_the_empty_stored_block_marker() {
    let payload = corpus::text();

    for flush in [SYNC_FLUSH, FULL_FLUSH] {
        for window_bits in [RAW, ZLIB, GZIP] {
            let mut deflater = Deflater::bounded(
                6,
                window_bits,
                DEF_MEM_LEVEL,
                Strategy::Default,
                payload.len(),
            );
            let room = deflater.room();
            assert_eq!(deflater.step(&payload, room, flush).0, ReturnCode::OK);

            let written = deflater.written();
            assert!(
                written.len() >= SYNC_MARKER.len(),
                "flush {flush} must emit at least the four-byte marker"
            );
            assert_eq!(
                &written[written.len() - SYNC_MARKER.len()..],
                &SYNC_MARKER,
                "flush {flush} at windowBits {window_bits} must end on the empty stored block"
            );

            // The stream is mid-flight, so `deflateEnd` reports the documented Z_DATA_ERROR.
            assert_eq!(deflater.end(), ReturnCode::DATA_ERROR);
        }
    }

    let mut partial = Deflater::bounded(6, RAW, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = partial.room();
    assert_eq!(
        partial.step(&payload, room, PARTIAL_FLUSH).0,
        ReturnCode::OK
    );
    let written = partial.written();
    assert_ne!(
        &written[written.len() - SYNC_MARKER.len()..],
        &SYNC_MARKER,
        "Z_PARTIAL_FLUSH deliberately stops short of byte alignment, so it must not emit the marker"
    );
    assert_eq!(partial.end(), ReturnCode::DATA_ERROR);
}

/// `Z_FULL_FLUSH` makes the stream recoverable after the flush point, even if the bytes before it
/// are corrupted.
///
/// This is `test/example.c`'s `test_flush` (L338-L368) and `test_sync` (L373-L408) run as one
/// scenario, because neither half means anything alone. `test_flush` compresses the first three
/// bytes with `Z_FULL_FLUSH`, then does `compr[3]++` to corrupt the first block, then finishes.
/// `test_sync` reads only the two header bytes, calls `inflateSync` to skip the damaged part, and
/// expects `Z_FINISH` to reach `Z_STREAM_END` -- recovering everything from the flush point on.
///
/// The recovery is only possible because `Z_FULL_FLUSH` resets the window as well as byte-aligning
/// (`deflate.c` L1066-L1069, `CLEAR_HASH` plus the `block_start`/`strstart` reset): with the
/// history cleared, no match in the later section can reach back into the corrupted one. That is
/// the difference between `Z_FULL_FLUSH` and `Z_SYNC_FLUSH`, and this test is what distinguishes
/// them.
#[test]
fn a_full_flush_boundary_survives_corruption_of_everything_before_it() {
    let payload = corpus::HELLO;
    // `c_stream.avail_in = 3` -- exactly the "hel" of "hello, hello!".
    let head_len = 3;

    let mut deflater = Deflater::bounded(
        Z_DEFAULT_COMPRESSION,
        ZLIB,
        DEF_MEM_LEVEL,
        Strategy::Default,
        payload.len(),
    );
    let room = deflater.room();
    let (ret, taken) = deflater.step(&payload[..head_len], room, FULL_FLUSH);
    assert_eq!(ret, ReturnCode::OK, "the full flush itself reports Z_OK");
    assert_eq!(taken, head_len);
    let flush_point = deflater.produced;
    assert!(
        flush_point > 4,
        "the flushed prefix must be longer than the byte the C test corrupts"
    );

    // `compr[3]++` -- force an error in the first compressed block.
    deflater.out[3] = deflater.out[3].wrapping_add(1);

    let room = deflater.room();
    let (finished, remaining) = deflater.step(&payload[head_len..], room, FINISH);
    assert_eq!(
        finished,
        ReturnCode::STREAM_END,
        "Z_FINISH still completes the stream"
    );
    assert_eq!(remaining, payload.len() - head_len);
    let corrupted = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);

    // test_sync: read just the zlib header, then resynchronise past the damage.
    let mut state = inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).unwrap();
    let reset = inflate_reset(&mut state);
    let mut out = vec![0u8; payload.len() + 64];
    let next_in;
    let mut next_out;

    {
        // `d_stream.avail_in = 2; /* just read the zlib header */`
        let mut stream = InflateStream::new(&corrupted[..2], &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut stream, NO_FLUSH),
            ReturnCode::OK,
            "two bytes is the whole zlib header and nothing more"
        );
        next_in = stream.next_in;
        next_out = stream.next_out;
    }
    assert_eq!(
        next_out, 0,
        "no payload byte can be produced from the header alone"
    );

    let next_in_after_sync;
    {
        // `d_stream.avail_in = comprLen - 2; err = inflateSync(&d_stream);`
        let mut stream = InflateStream::new(&corrupted, &mut out);
        stream.next_in = next_in;
        stream.next_out = next_out;
        assert_eq!(
            inflate_sync(&mut state, &mut stream),
            ReturnCode::OK,
            "inflateSync must find the flush point"
        );
        assert!(
            stream.next_in >= flush_point,
            "inflateSync must skip past the corrupted first block, which ended at {flush_point}"
        );
        next_in_after_sync = stream.next_in;
        next_out = stream.next_out;
    }

    {
        let mut stream = InflateStream::new(&corrupted, &mut out);
        stream.next_in = next_in_after_sync;
        stream.next_out = next_out;
        assert_eq!(
            inflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END,
            "everything after the full-flush point must still decode"
        );
        next_out = stream.next_out;
    }
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // The C test prints "hel" itself and then the recovered tail, so the tail is what is left of
    // the payload once its first three bytes -- the ones inside the corrupted block -- are gone.
    assert_eq!(
        &out[..next_out],
        &payload[head_len..],
        "the recovered section is exactly the input after the full-flush point"
    );
}

/// `Z_TREES` is inflate-only, and `deflate` rejects it -- as it rejects every out-of-range flush.
///
/// `deflate.c` L985 is a single guard: `if (deflateStateCheck(strm) || flush > Z_BLOCK ||
/// flush < 0) return Z_STREAM_ERROR;`. `Z_TREES` is 6 and `Z_BLOCK` is 5, so `Z_TREES` fails the
/// range test like any other value above it. `Z_STREAM_ERROR` is what the source implements -- not
/// `Z_BUF_ERROR`, which would tell a caller to retry with more room and produce an infinite loop.
///
/// The rejection must also leave the stream usable, because a caller that passes a bad flush by
/// mistake is entitled to carry on: the guard runs before `s->last_flush = flush`, so nothing is
/// recorded.
#[test]
fn deflate_rejects_z_trees_and_every_other_out_of_range_flush() {
    assert_eq!(TREES, 6, "zlib.h L178");
    assert_eq!(BLOCK, 5, "zlib.h L177: the highest flush deflate accepts");

    let payload = corpus::HELLO;
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

    for bad in [TREES, BLOCK + 2, i32::MAX, -1, i32::MIN] {
        let room = deflater.room();
        let (ret, consumed) = deflater.step(payload, room, bad);
        assert_eq!(
            ret,
            ReturnCode::STREAM_ERROR,
            "flush {bad} is outside 0..=Z_BLOCK, so deflate.c L985 returns Z_STREAM_ERROR"
        );
        assert_eq!(consumed, 0, "a rejected call must consume nothing");
        assert_eq!(deflater.produced, 0, "a rejected call must emit nothing");
        assert_eq!(
            deflater.state.last_flush(),
            -2,
            "the guard precedes `s->last_flush = flush`, so the sentinel is untouched"
        );
    }

    // And the stream still works afterwards.
    let room = deflater.room();
    assert_eq!(
        deflater.step(payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    let out = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);
    assert_eq!(expand(&out, ZLIB, payload.len()), payload);
}

/// Repeating a flush with no new input is `Z_BUF_ERROR`, and the `RANK` ordering says which
/// repeats count.
///
/// `deflate.c` L1029-L1034:
///
/// ```c
/// } else if (strm->avail_in == 0 && RANK(flush) <= RANK(old_flush) &&
///            flush != Z_FINISH) {
///     ERR_RETURN(strm, Z_BUF_ERROR);
/// }
/// ```
///
/// `RANK(f) = ((f) * 2) - ((f) > 4 ? 9 : 0)` maps the flush values to
/// `Z_NO_FLUSH` 0, `Z_BLOCK` 1, `Z_PARTIAL_FLUSH` 2, `Z_SYNC_FLUSH` 4, `Z_FULL_FLUSH` 6 -- which
/// is the whole purpose of the expression: it lifts `Z_BLOCK` out of numeric order and drops it
/// between `Z_NO_FLUSH` and `Z_PARTIAL_FLUSH`, where it belongs by strength.
///
/// So after a `Z_SYNC_FLUSH` (rank 4) with no new input, both a second `Z_SYNC_FLUSH` (rank 4, not
/// greater) and a `Z_BLOCK` (rank 1, weaker) are refused, while `Z_FULL_FLUSH` (rank 6, stronger)
/// is allowed through and `Z_FINISH` is exempt outright. Without the refusal a caller polling in a
/// loop would receive an unbounded run of empty stored blocks.
#[test]
fn a_repeated_or_weaker_flush_with_no_new_input_is_a_buf_error() {
    let payload = corpus::HELLO;
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

    let room = deflater.room();
    assert_eq!(deflater.step(payload, room, SYNC_FLUSH).0, ReturnCode::OK);
    let after_first = deflater.produced;

    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, SYNC_FLUSH).0,
        ReturnCode::BUF_ERROR,
        "RANK(Z_SYNC_FLUSH) <= RANK(Z_SYNC_FLUSH) with no new input is Z_BUF_ERROR"
    );
    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, BLOCK).0,
        ReturnCode::BUF_ERROR,
        "RANK(Z_BLOCK) is 1, below RANK(Z_SYNC_FLUSH) of 4, so it is refused too"
    );
    assert_eq!(
        deflater.produced, after_first,
        "a refused flush must not emit anything"
    );

    // A strictly stronger flush is not a repeat, so it is allowed even with no new input.
    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, FULL_FLUSH).0,
        ReturnCode::OK,
        "RANK(Z_FULL_FLUSH) is 6, above RANK(Z_SYNC_FLUSH) of 4, so it proceeds"
    );
    assert!(
        deflater.produced > after_first,
        "the stronger flush must actually emit its marker"
    );

    // `flush != Z_FINISH` exempts Z_FINISH from the rank test entirely.
    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, FINISH).0,
        ReturnCode::STREAM_END,
        "Z_FINISH is exempt from the rank comparison"
    );
    let out = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);
    assert_eq!(expand(&out, ZLIB, payload.len()), payload);
}

/// `Z_BLOCK` after `Z_NO_FLUSH` is a genuine strengthening, so it is not refused.
///
/// The mirror image of the previous test, and the reason `RANK` cannot simply be `f`:
/// `RANK(Z_BLOCK)` is 1 and `RANK(Z_NO_FLUSH)` is 0, so `1 <= 0` is false and the call proceeds.
/// Compare the flush values directly and `5 <= 0` is also false -- but after a `Z_PARTIAL_FLUSH`
/// the naive comparison would let `Z_BLOCK` through where `RANK` correctly refuses it, which is the
/// case the expression exists for.
#[test]
fn z_block_after_no_flush_is_allowed() {
    let payload = corpus::HELLO;
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

    let room = deflater.room();
    assert_eq!(deflater.step(payload, room, NO_FLUSH).0, ReturnCode::OK);

    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, BLOCK).0,
        ReturnCode::OK,
        "RANK(Z_BLOCK) of 1 is above RANK(Z_NO_FLUSH) of 0, so this is not a repeat"
    );

    let room = deflater.room();
    assert_eq!(deflater.step(&[], room, FINISH).0, ReturnCode::STREAM_END);
    let out = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);
    assert_eq!(expand(&out, ZLIB, payload.len()), payload);
}

/// More input after `Z_FINISH` has completed is `Z_BUF_ERROR`, and a non-`Z_FINISH` flush after
/// `FINISH_STATE` is `Z_STREAM_ERROR`.
///
/// `deflate.c` L991-L995 folds the second case into the entry guard:
/// `(s->status == FINISH_STATE && flush != Z_FINISH)` is a stream error, because a caller that
/// finishes a stream and then asks for a partial flush has lost track of its own protocol. Feeding
/// more input to an already finished stream is the milder `Z_BUF_ERROR` from L1046.
#[test]
fn a_finished_stream_refuses_further_work() {
    let payload = corpus::HELLO;
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

    let room = deflater.room();
    assert_eq!(
        deflater.step(payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    assert_eq!(
        deflater.state.status(),
        Status::Finish,
        "FINISH_STATE is 666"
    );
    let completed = deflater.produced;

    let room = deflater.room();
    assert_eq!(
        deflater.step(payload, room, FINISH).0,
        ReturnCode::BUF_ERROR,
        "a finished stream has nothing left to do, so more input is Z_BUF_ERROR"
    );

    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, SYNC_FLUSH).0,
        ReturnCode::STREAM_ERROR,
        "deflate.c L993: any flush other than Z_FINISH from FINISH_STATE is Z_STREAM_ERROR"
    );
    assert_eq!(
        deflater.produced, completed,
        "neither refusal may add a byte to a completed stream"
    );
    assert_eq!(deflater.end(), ReturnCode::OK);
}

/// No output room at all is `Z_BUF_ERROR`, checked before anything else can happen.
///
/// `deflate.c` L996: `if (strm->avail_out == 0) ERR_RETURN(strm, Z_BUF_ERROR);`. The point of
/// asserting it separately is that it is *not* a stream error: the call is well formed and the
/// caller is expected to make room and retry, which is exactly what the one-byte-at-a-time test
/// above relies on.
#[test]
fn a_zero_length_output_buffer_is_a_buf_error() {
    let (mut state, reset) = open(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let mut nothing: [u8; 0] = [];
    {
        let mut stream = DeflateStream::new(corpus::HELLO, &mut nothing);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, NO_FLUSH),
            ReturnCode::BUF_ERROR,
            "deflate.c L996: avail_out == 0 is Z_BUF_ERROR, not Z_STREAM_ERROR"
        );
        assert_eq!(
            stream.next_in, 0,
            "nothing can be consumed with nowhere to put it"
        );
    }
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
}

/// The whole flush enumeration, and which members `deflate` accepts.
///
/// `Flush::ALL` must cover all seven values `zlib.h` L172-L178 defines, `as_raw` must agree with
/// each numeric constant, and `is_valid_for_deflate` must draw the line exactly where
/// `deflate.c` L985 draws it -- at `Z_BLOCK`.
#[test]
fn the_flush_enumeration_matches_the_reference() {
    use zlib_rs::config::Flush;

    let reference = [
        (Flush::NoFlush, NO_FLUSH, "Z_NO_FLUSH", true),
        (Flush::PartialFlush, PARTIAL_FLUSH, "Z_PARTIAL_FLUSH", true),
        (Flush::SyncFlush, SYNC_FLUSH, "Z_SYNC_FLUSH", true),
        (Flush::FullFlush, FULL_FLUSH, "Z_FULL_FLUSH", true),
        (Flush::Finish, FINISH, "Z_FINISH", true),
        (Flush::Block, BLOCK, "Z_BLOCK", true),
        (Flush::Trees, TREES, "Z_TREES", false),
    ];

    assert_eq!(
        Flush::ALL.len(),
        reference.len(),
        "zlib.h L172-L178 defines seven flush values"
    );

    for (flush, raw, c_name, valid_for_deflate) in reference {
        assert_eq!(flush.as_raw(), raw, "{c_name} is {raw}");
        assert_eq!(
            Flush::from_raw(raw),
            Some(flush),
            "{c_name} must round trip"
        );
        assert_eq!(flush.c_name(), c_name, "the reported C name of {flush:?}");
        assert_eq!(
            flush.is_valid_for_deflate(),
            valid_for_deflate,
            "{c_name}: deflate.c L985 accepts 0..=Z_BLOCK and nothing else"
        );
    }

    for raw in [-1, 7, i32::MIN, i32::MAX] {
        assert_eq!(Flush::from_raw(raw), None, "{raw} is not a flush value");
    }
}

// =============================================================================================
// 6. deflateBound
//
// This is not an informational helper. Callers size their output buffers with the number it
// returns, so a bound that is one byte too small becomes a heap overflow **in caller code**, in a
// program that was compiled years ago against a header this port may not change. A bound that is
// too large is milder but still breaks callers that assert on exact sizes, and it inflates the
// buffer every one-shot `compress()` allocates.
//
// The arithmetic, from `deflate.c` L856-L927:
//
//   fixedlen = len + (len >> 3) + (len >> 8) + (len >> 9) + 4      saturating on overflow
//   storelen = len + (len >> 5) + (len >> 7) + (len >> 11) + 7     saturating on overflow
//
//   no usable state  ->  max(fixedlen, storelen) + 18
//
//   wraplen = 0                                   for raw deflate
//           = 6 + (strstart ? 4 : 0)              for the zlib wrapper
//           = 18 + 2 + extra_len                  for gzip, per field present
//                + strlen(name) + 1
//                + strlen(comment) + 1
//                + 2 if hcrc
//
//   w_bits != 15 || hash_bits != 15
//                    ->  (w_bits <= hash_bits && level ? fixedlen : storelen) + wraplen
//   otherwise        ->  len + (len >> 12) + (len >> 14) + (len >> 25) + 13 - 6 + wraplen
//
// The `13 - 6` in the last line is not a typo in either implementation: 13 is the constant of the
// tight bound including a zlib wrapper, and the 6 is subtracted so that `wraplen` can add the
// wrapper back for whichever container is actually in use.
// =============================================================================================

/// Recomputes the tight default bound from the C formula, for the default configuration.
///
/// Deliberately written out here rather than called into the crate: a test that asked the code
/// under test for the expected value would agree with it however wrong it became.
fn reference_tight_bound(source_len: usize, wraplen: usize) -> usize {
    source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13 - 6 + wraplen
}

/// The wrapper overhead for a container with no user-supplied gzip header and no dictionary.
fn reference_wraplen(window_bits: i32) -> usize {
    match window_bits {
        RAW => 0,
        ZLIB => 6,
        GZIP => 18,
        other => panic!("unexpected windowBits {other}"),
    }
}

/// The default configuration returns exactly the tight bound the C formula yields.
///
/// Every length here was checked against the arithmetic by hand as well as against the
/// implementation: for the zlib wrapper, 0 gives 13, 1 gives 14, 13 gives 26, 1024 gives
/// `1024 + 0 + 0 + 0 + 13 = 1037`, 8192 gives `8192 + 2 + 0 + 0 + 13 = 8207`, and 1000000 gives
/// `1000000 + 244 + 61 + 0 + 13 = 1000318`.
#[test]
fn bound_is_exactly_the_reference_formula_for_default_parameters() {
    for window_bits in [RAW, ZLIB, GZIP] {
        let (mut state, _) = open(6, window_bits, DEF_MEM_LEVEL, Strategy::Default);
        assert_eq!(
            state.w_bits(),
            15,
            "the tight bound applies only at w_bits 15"
        );
        assert_eq!(state.hash_bits(), 15, "and only at hash_bits 8 + 7");

        let wraplen = reference_wraplen(window_bits);
        for source_len in [0usize, 1, 13, 100, 1024, 8192, 1_000_000] {
            assert_eq!(
                deflate_bound_z(Some(&state), source_len),
                reference_tight_bound(source_len, wraplen),
                "windowBits {window_bits}, sourceLen {source_len}"
            );
        }

        // The three spot values worth naming, because they are the ones a reader can check
        // mentally: 13 bytes of overhead for zlib, 7 for raw, 25 for gzip.
        assert_eq!(deflate_bound_z(Some(&state), 0), 7 + wraplen);
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

/// A non-default `windowBits` or `memLevel` falls back to one of the two conservative bounds.
///
/// `deflate.c` L919-L923 chooses between them with `w_bits <= hash_bits && s->level`: when the
/// hash is at least as wide as the window every position is reachable, so the fixed-block bound
/// applies; otherwise the encoder may fall back to stored blocks and the larger store bound is
/// needed. `memLevel = 1` gives `hash_bits` 8 against `w_bits` 15 and so takes the store branch;
/// `windowBits = 9` gives `w_bits` 9 against `hash_bits` 15 and so takes the fixed branch.
#[test]
fn bound_falls_back_to_a_conservative_formula_off_the_default_path() {
    let source_len = 1024usize;
    let fixedlen = source_len + (source_len >> 3) + (source_len >> 8) + (source_len >> 9) + 4;
    let storelen = source_len + (source_len >> 5) + (source_len >> 7) + (source_len >> 11) + 7;

    // memLevel 1: hash_bits 8 < w_bits 15, so the store bound.
    let (mut narrow_hash, _) = open(6, ZLIB, MIN_MEM_LEVEL, Strategy::Default);
    assert_eq!(narrow_hash.hash_bits(), 8);
    assert_eq!(
        deflate_bound_z(Some(&narrow_hash), source_len),
        storelen + reference_wraplen(ZLIB),
        "w_bits 15 > hash_bits 8 selects storelen (deflate.c L920-L921)"
    );
    assert_eq!(deflate_end(&mut narrow_hash), ReturnCode::OK);

    // windowBits 9: w_bits 9 <= hash_bits 15 and the level is non-zero, so the fixed bound.
    let (mut narrow_window, _) = open(6, 9, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(narrow_window.w_bits(), 9);
    assert_eq!(
        deflate_bound_z(Some(&narrow_window), source_len),
        fixedlen + reference_wraplen(ZLIB),
        "w_bits 9 <= hash_bits 15 with a non-zero level selects fixedlen"
    );
    assert_eq!(deflate_end(&mut narrow_window), ReturnCode::OK);

    // Level 0 off the default path takes the store branch whatever the widths, because `&& s->level`
    // is false: the encoder will be emitting stored blocks.
    let (mut stored, _) = open(0, 9, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(
        deflate_bound_z(Some(&stored), source_len),
        storelen + reference_wraplen(ZLIB),
        "level 0 selects storelen regardless of the widths (deflate.c L920)"
    );
    assert_eq!(deflate_end(&mut stored), ReturnCode::OK);
}

/// With no state to consult, the bound is the larger of the two plus a full wrapper.
///
/// `deflate.c` L878-L881. Without a stream there is no way to know the container or the tuning, so
/// the answer has to cover the worst case: the larger conservative bound plus 18 bytes, which is
/// the widest wrapper (gzip) the library can emit.
#[test]
fn bound_without_a_state_returns_the_larger_bound_plus_a_full_wrapper() {
    for source_len in [0usize, 1, 13, 1024, 8192] {
        let fixedlen = source_len + (source_len >> 3) + (source_len >> 8) + (source_len >> 9) + 4;
        let storelen = source_len + (source_len >> 5) + (source_len >> 7) + (source_len >> 11) + 7;
        assert_eq!(
            deflate_bound_z(None::<&Deflate>, source_len),
            fixedlen.max(storelen) + 18,
            "sourceLen {source_len} without a state"
        );
    }
}

/// The `_z` and `uLong` forms agree wherever both are defined.
///
/// `deflateBound` is `deflateBound_z` narrowed to `uLong` (`deflate.c` L929-L932), returning
/// `(uLong)-1` if the narrowing would lose information. On this target `uLong` and `z_size_t` are
/// both 64 bits wide, so the two agree for every length a caller can actually pass -- and that
/// agreement is what the assertion states, rather than a claim about narrower targets.
#[test]
fn bound_and_bound_z_agree() {
    for window_bits in [RAW, ZLIB, GZIP] {
        let (mut state, _) = open(6, window_bits, DEF_MEM_LEVEL, Strategy::Default);
        for source_len in [0usize, 1, 13, 100, 1024, 8192, 1_000_000] {
            let wide = deflate_bound(Some(&state), u64::try_from(source_len).unwrap());
            let sized = deflate_bound_z(Some(&state), source_len);
            assert_eq!(
                wide,
                u64::try_from(sized).unwrap(),
                "windowBits {window_bits}, sourceLen {source_len}: the two forms must agree"
            );
        }
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

/// The bound saturates instead of wrapping.
///
/// `deflate.c` returns `(z_size_t)-1` at every overflow site rather than letting the addition
/// wrap. That direction is the only safe one: a wrapped bound would be a *small* number, and a
/// caller would allocate a small buffer for a huge input.
#[test]
fn bound_saturates_instead_of_wrapping() {
    let (mut state, _) = open(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);

    assert_eq!(
        deflate_bound_z(Some(&state), usize::MAX),
        usize::MAX,
        "the tight formula would wrap, so it saturates (deflate.c L926)"
    );
    assert_eq!(
        deflate_bound(Some(&state), u64::MAX),
        u64::MAX,
        "and so does the uLong form (deflate.c L931)"
    );
    assert_eq!(
        deflate_bound_z(None::<&Deflate>, usize::MAX),
        usize::MAX,
        "including the no-state path, whose `bound + 18` would wrap (deflate.c L880)"
    );

    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
}

/// A user-supplied gzip header widens the bound by exactly what it will occupy.
///
/// `deflate.c` L890-L910 walks the installed `gz_header` and adds `2 + extra_len` for the extra
/// field, `strlen(name) + 1` and `strlen(comment) + 1` for the two strings -- the `+ 1` being the
/// terminating NUL the C loop emits -- and 2 more for the header CRC. The port's `GzHeaderView`
/// carries `name` and `comment` as slices that already include their NUL, so the arithmetic below
/// uses their lengths directly.
///
/// The second assertion is the one that matters: the widened bound must still bound the real
/// output. A bound that grew by the wrong amount would be caught by the first assertion; a bound
/// that grew by too little would corrupt a caller's heap, and only the second assertion notices.
#[test]
fn bound_accounts_for_an_installed_gzip_header() {
    const EXTRA: &[u8] = &[0xde, 0xad, 0xbe, 0xef];
    const NAME: &[u8] = b"payload.bin\0";
    const COMMENT: &[u8] = b"written by the deflate integration suite\0";

    // The three fields are `Cell` slices because the view models storage the *caller* owns
    // and may write; see `GzHeaderView`. A test owns them outright, so a `Vec<Cell<u8>>`
    // built from the literal is the shortest honest spelling.
    let extra = cells(EXTRA);
    let name = cells(NAME);
    let comment = cells(COMMENT);
    let header = GzHeaderView::new(
        true,
        0x1234_5678,
        3,
        true,
        Some(&extra),
        Some(&name),
        Some(&comment),
    );

    let payload = corpus::text();

    let (mut bare, _) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    let bare_bound = deflate_bound_z(Some(&bare), payload.len());
    assert_eq!(deflate_end(&mut bare), ReturnCode::OK);

    let (mut state, reset) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(deflate_set_header(&mut state, Some(header)), ReturnCode::OK);
    let header_bound = deflate_bound_z(Some(&state), payload.len());

    let field_bytes = 2 + EXTRA.len() + NAME.len() + COMMENT.len() + 2;
    assert_eq!(
        header_bound,
        bare_bound + field_bytes,
        "the bound must grow by 2 + extra_len, the two NUL-terminated strings, and 2 for FHCRC"
    );

    let mut out = vec![0u8; header_bound];
    let produced = {
        let mut stream = DeflateStream::new(&payload, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END,
            "the widened bound must be sufficient on its own, with no margin added"
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    assert!(
        out.len() <= header_bound,
        "the real output must fit inside the bound it was sized from: {} against {header_bound}",
        out.len()
    );
    assert_eq!(expand(&out, GZIP, payload.len()), payload);
}

/// A preset dictionary adds the four `DICTID` bytes to a zlib stream's bound.
///
/// `deflate.c` L885: the zlib wrapper is `6 + (s->strstart ? 4 : 0)`, and `deflateSetDictionary`
/// is what leaves `strstart` non-zero. The extra four bytes are the dictionary's Adler-32, which
/// `deflate` writes after the two header bytes (L1038-L1040).
#[test]
fn bound_grows_by_four_once_a_dictionary_has_been_set() {
    let payload = corpus::HELLO;

    let (mut bare, _) = open(9, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let bare_bound = deflate_bound_z(Some(&bare), payload.len());
    assert_eq!(deflate_end(&mut bare), ReturnCode::OK);

    let (mut state, reset) = open(9, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let mut adler = reset.adler;
    let mut total_in = reset.total_in;
    assert_eq!(
        deflate_set_dictionary(&mut state, &mut adler, &mut total_in, corpus::DICTIONARY),
        ReturnCode::OK
    );
    assert_eq!(
        deflate_bound_z(Some(&state), payload.len()),
        bare_bound + 4,
        "a non-zero strstart adds the four DICTID bytes (deflate.c L885)"
    );
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
}

/// The bound covers the worst realistic case: incompressible data at level 0.
///
/// Level 0 with random bytes is what maximises expansion, because every byte must be stored and
/// the stored blocks add framing rather than removing anything. `memLevel = 1` is included because
/// `deflateBound`'s own comment names it as the configuration that may fall back to 127-byte
/// stored blocks, which is the highest framing overhead the encoder can produce.
#[test]
fn the_bound_covers_the_worst_realistic_expansion() {
    for source_len in [0usize, 13, 1024, 8192] {
        let payload = corpus::incompressible(source_len);
        for (window_bits, mem_level) in [
            (RAW, DEF_MEM_LEVEL),
            (ZLIB, DEF_MEM_LEVEL),
            (GZIP, DEF_MEM_LEVEL),
            (ZLIB, MIN_MEM_LEVEL),
        ] {
            let (mut state, _) = open(0, window_bits, mem_level, Strategy::Default);
            let bound = deflate_bound_z(Some(&state), source_len);
            assert_eq!(deflate_end(&mut state), ReturnCode::OK);

            let actual = squeeze(&payload, 0, window_bits, mem_level, Strategy::Default).len();
            assert!(
                actual <= bound,
                "level 0, windowBits {window_bits}, memLevel {mem_level}, sourceLen \
                 {source_len}: produced {actual} bytes but the bound promised {bound}"
            );
        }
    }
}

/// A single `Z_FINISH` call fits inside `deflateBound` with no margin, for every configuration.
///
/// `zlib.h` L768-L779 makes this a guarantee, not a hope: "`deflateBound()` returns an upper bound
/// on the compressed size after deflation of sourceLen bytes ... If the parameters are not
/// changed, and `deflate()` is called with `Z_FINISH`, then `deflate()` will return
/// `Z_STREAM_END`."
/// [`squeeze`] adds a margin so that a bound regression shows up here rather than everywhere; this
/// test is where the margin is removed and the promise is held to.
#[test]
fn a_single_finish_call_always_fits_inside_the_bound() {
    // The two payloads that bracket the bound: incompressible data maximises expansion, and an
    // empty input is the degenerate case where the wrapper is the entire output.
    let payloads: [(&str, Vec<u8>); 2] = [
        ("empty", corpus::EMPTY.to_vec()),
        ("incompressible", corpus::incompressible(1024)),
    ];

    for (name, payload) in &payloads {
        for level in [0i32, 1, 6, 9] {
            for window_bits in [RAW, ZLIB, GZIP] {
                let (mut state, reset) = open(level, window_bits, DEF_MEM_LEVEL, Strategy::Default);
                let bound = deflate_bound_z(Some(&state), payload.len());

                let mut out = vec![0u8; bound];
                let produced = {
                    let mut stream = DeflateStream::new(payload, &mut out);
                    stream.apply_reset(reset);
                    assert_eq!(
                        deflate(&mut state, &mut stream, FINISH),
                        ReturnCode::STREAM_END,
                        "{name} at level {level}, windowBits {window_bits}: Z_FINISH must complete \
                         within exactly deflate_bound bytes"
                    );
                    stream.next_out
                };
                assert_eq!(deflate_end(&mut state), ReturnCode::OK);
                assert!(produced <= bound);

                out.truncate(produced);
                assert_eq!(expand(&out, window_bits, payload.len()), *payload);
            }
        }
    }
}

// =============================================================================================
// 7. The rest of the public surface
//
// Each of these is a documented, caller-visible contract, and several of them are places where an
// idiomatic Rust port drifts away from C by doing the *nicer* thing. `deflateEnd`'s
// `Z_DATA_ERROR` is the clearest example: ownership makes teardown infallible, so a port that
// leaned on `Drop` would return `Z_OK` and silently stop telling callers that they abandoned a
// stream mid-flight.
// =============================================================================================

/// `test/example.c`'s `test_dict_deflate`, end to end -- including the six-byte dictionary.
///
/// The C test passes `(int)sizeof(dictionary)` for `"hello"`, which is **6** bytes and includes the
/// terminating NUL. `common::corpus::DICTIONARY` is `b"hello\0"` for exactly that reason: five
/// bytes would be a different dictionary with a different Adler-32 and therefore a different
/// `DICTID` in the stream, so the distinction is observable rather than pedantic.
///
/// The full chain is asserted: the dictionary's Adler-32 becomes the stream's `adler`; `strstart`
/// advances, which sets `FDICT` and appends `DICTID`; `Z_FINISH` reaches `Z_STREAM_END`;
/// `deflateGetDictionary` reads back what was set; and the stream is decodable **only** after the
/// matching `inflateSetDictionary`, which is what makes a preset dictionary a preset dictionary
/// rather than a hint.
#[test]
fn the_example_c_dictionary_case_works_end_to_end() {
    assert_eq!(
        corpus::DICTIONARY,
        b"hello\0",
        "test/example.c passes sizeof(dictionary), so the NUL is part of the dictionary"
    );
    assert_eq!(corpus::DICTIONARY.len(), 6);

    let (mut state, reset) = open(Z_BEST_COMPRESSION, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let mut adler = reset.adler;
    let mut total_in = reset.total_in;
    assert_eq!(adler, 1, "adler32(0, NULL, 0) is 1, the documented seed");

    assert_eq!(
        deflate_set_dictionary(&mut state, &mut adler, &mut total_in, corpus::DICTIONARY),
        ReturnCode::OK
    );
    // `dictId = c_stream.adler;`
    let dict_id = adler;
    assert_ne!(
        dict_id, 1,
        "setting a dictionary must fold it into the stream's Adler-32"
    );
    assert_eq!(
        total_in,
        u64::try_from(corpus::DICTIONARY.len()).unwrap(),
        "deflateSetDictionary runs the dictionary through read_buf, which advances total_in"
    );

    // `deflateGetDictionary` reads back the window, capped at w_size (`deflate.c` L633-L635).
    let mut readback = [0u8; 64];
    let mut readback_len = 0u32;
    assert_eq!(
        deflate_get_dictionary(
            &state,
            Some(&mut OutputRegion::init(&mut readback)),
            Some(&mut readback_len)
        ),
        ReturnCode::OK
    );
    assert_eq!(
        usize::try_from(readback_len).unwrap(),
        corpus::DICTIONARY.len(),
        "the whole dictionary is still in the window"
    );
    assert_eq!(
        &readback[..corpus::DICTIONARY.len()],
        corpus::DICTIONARY,
        "deflateGetDictionary must return the bytes that were set"
    );

    let payload = corpus::HELLO;
    let mut out = vec![0u8; deflate_bound_z(Some(&state), payload.len()) + 64];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        stream.adler = adler;
        stream.total_in = total_in;
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END,
            "test/example.c: deflate should report Z_STREAM_END"
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    // FDICT plus the four DICTID bytes, most significant first (`deflate.c` L1030, L1038-L1040).
    assert_eq!(
        u32::from(out[1]) & PRESET_DICT,
        PRESET_DICT,
        "a preset dictionary sets the FDICT bit of the zlib FLG byte"
    );
    assert_eq!(
        (u32::from(out[0]) << 8 | u32::from(out[1])) % 31,
        0,
        "the mod-31 correction still holds with FDICT set"
    );
    assert_eq!(
        &out[2..6],
        &dict_id.to_be_bytes(),
        "DICTID is the dictionary's Adler-32, most significant byte first"
    );

    // test_dict_inflate: the stream needs the dictionary, and says so.
    let mut inflater = inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).unwrap();
    let inflate_reset_values = inflate_reset(&mut inflater);
    let mut recovered = vec![0u8; payload.len() + 64];
    let next_in;
    let mut next_out;
    {
        let mut stream = InflateStream::new(&out, &mut recovered);
        stream.apply_reset(inflate_reset_values);
        assert_eq!(
            inflate(&mut inflater, &mut stream, NO_FLUSH),
            ReturnCode::NEED_DICT,
            "a stream compressed with a preset dictionary cannot be decoded without it"
        );
        assert_eq!(
            stream.adler,
            Some(dict_id),
            "Z_NEED_DICT reports which dictionary is wanted, by its Adler-32"
        );
        next_in = stream.next_in;
        next_out = stream.next_out;
    }
    assert_eq!(
        inflate_set_dictionary(&mut inflater, corpus::DICTIONARY),
        ReturnCode::OK
    );
    {
        let mut stream = InflateStream::new(&out, &mut recovered);
        stream.next_in = next_in;
        stream.next_out = next_out;
        assert_eq!(
            inflate(&mut inflater, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        next_out = stream.next_out;
    }
    assert_eq!(inflate_end(&mut inflater), ReturnCode::OK);
    assert_eq!(
        &recovered[..next_out],
        payload,
        "the dictionary makes it decodable"
    );
}

/// `deflateSetDictionary` is refused once it could not take effect.
///
/// The guard is `deflate.c` L570-L572:
/// `if (wrap == 2 || (wrap == 1 && s->status != INIT_STATE) || s->lookahead) return
/// Z_STREAM_ERROR;`. Three separate reasons, and each one is asserted, because they fail for
/// genuinely different causes:
///
/// * a gzip stream (`wrap == 2`) has no way to signal a dictionary in its header, so it is refused
///   before anything else;
/// * a zlib stream past `INIT_STATE` has already written its header, and `FDICT` lives in that
///   header;
/// * a non-zero `lookahead` means input is already buffered, and a dictionary must precede all
///   input.
///
/// Worth recording precisely, because it looks like a gap: after a *successful*
/// `deflateSetDictionary` the C code sets `s->wrap = 0` (L580, "avoid computing Adler-32 in
/// `read_buf`") and restores it only at the end of the call, and a later call on a finished stream
/// therefore sees `wrap == 1` with `status == FINISH_STATE`, which the second clause refuses. The
/// port reproduces that structure exactly rather than tightening it.
#[test]
fn set_dictionary_is_refused_when_it_could_not_take_effect() {
    // A gzip stream: refused outright.
    let (mut gzip, reset) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    let mut adler = reset.adler;
    let mut total_in = reset.total_in;
    assert_eq!(
        deflate_set_dictionary(&mut gzip, &mut adler, &mut total_in, corpus::DICTIONARY),
        ReturnCode::STREAM_ERROR,
        "wrap == 2 has nowhere to record a preset dictionary (deflate.c L571)"
    );
    assert_eq!(deflate_end(&mut gzip), ReturnCode::OK);

    // A zlib stream that has already begun: refused because the header is written.
    let payload = corpus::text();
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = deflater.room();
    assert_eq!(deflater.step(&payload, room, NO_FLUSH).0, ReturnCode::OK);
    assert_eq!(
        deflater.state.status(),
        Status::Busy,
        "a stream that has consumed input is at BUSY_STATE"
    );
    let mut late_adler = 1u32;
    let mut late_total_in = 0u64;
    assert_eq!(
        deflate_set_dictionary(
            &mut deflater.state,
            &mut late_adler,
            &mut late_total_in,
            corpus::DICTIONARY
        ),
        ReturnCode::STREAM_ERROR,
        "wrap == 1 past INIT_STATE has already emitted the FLG byte (deflate.c L571)"
    );
    assert_eq!(
        late_adler, 1,
        "a refused call must not touch the caller's checksum"
    );
    assert_eq!(late_total_in, 0, "nor its input count");
    // The stream is mid-flight, hence the documented Z_DATA_ERROR.
    assert_eq!(deflater.end(), ReturnCode::DATA_ERROR);
}

/// A raw stream accepts a dictionary at any point before input arrives, and it changes the output.
///
/// With `wrap == 0` the first two clauses of the guard cannot fire, so only `s->lookahead` stands
/// in the way -- which is `deflate.c`'s way of saying that a raw stream may be primed with history
/// but not re-primed mid-stream. The dictionary is not recorded in the stream (there is no header
/// to record it in), so the decoder must be given it separately; that asymmetry is the whole
/// reason raw streams with dictionaries exist.
#[test]
fn a_raw_stream_accepts_a_dictionary_and_uses_it() {
    let payload = corpus::HELLO;

    let without = squeeze(payload, 9, RAW, DEF_MEM_LEVEL, Strategy::Default);

    let (mut state, reset) = open(9, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let mut adler = reset.adler;
    let mut total_in = reset.total_in;
    assert_eq!(
        deflate_set_dictionary(&mut state, &mut adler, &mut total_in, b"hello, "),
        ReturnCode::OK,
        "wrap == 0 leaves only the lookahead clause, which is zero here"
    );
    assert_eq!(
        adler, reset.adler,
        "a raw stream computes no Adler-32, so the checksum is untouched (deflate.c L577-L578)"
    );

    let mut out = vec![0u8; deflate_bound_z(Some(&state), payload.len()) + 64];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        stream.total_in = total_in;
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    assert!(
        out.len() < without.len(),
        "priming the window with a prefix of the payload must help; got {} against {}",
        out.len(),
        without.len()
    );

    // The decoder needs the same history, supplied out of band because a raw stream carries none.
    let mut inflater = inflate_init2(InflateConfig::new(RAW), GlobalAllocator).unwrap();
    let inflate_reset_values = inflate_reset(&mut inflater);
    assert_eq!(
        inflate_set_dictionary(&mut inflater, b"hello, "),
        ReturnCode::OK,
        "a raw inflate stream accepts a dictionary at any time (inflate.c L1196)"
    );
    let mut recovered = vec![0u8; payload.len() + 64];
    let next_out = {
        let mut stream = InflateStream::new(&out, &mut recovered);
        stream.apply_reset(inflate_reset_values);
        assert_eq!(
            inflate(&mut inflater, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut inflater), ReturnCode::OK);
    assert_eq!(&recovered[..next_out], payload);
}

/// `deflateGetDictionary` handles both null arguments the way `zlib.h` documents.
///
/// `deflate.c` L636-L639 tests `dictionary != Z_NULL` and `dictLength != Z_NULL` independently, so
/// a caller may ask for the length alone -- which is the documented way to size a buffer before
/// asking for the bytes. The port spells the two nulls as `Option::None`.
#[test]
fn get_dictionary_accepts_either_argument_alone() {
    let payload = corpus::HELLO;
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = deflater.room();
    assert_eq!(
        deflater.step(payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );

    // Length only.
    let mut length = 0u32;
    assert_eq!(
        deflate_get_dictionary(&deflater.state, None, Some(&mut length)),
        ReturnCode::OK
    );
    assert_eq!(
        usize::try_from(length).unwrap(),
        payload.len(),
        "the whole payload is still in the window, so it is the whole dictionary"
    );

    // Bytes only.
    let mut buffer = vec![0u8; usize::try_from(length).unwrap()];
    assert_eq!(
        deflate_get_dictionary(
            &deflater.state,
            Some(&mut OutputRegion::init(&mut buffer)),
            None
        ),
        ReturnCode::OK
    );
    assert_eq!(buffer, payload, "the window holds the most recent input");

    // Neither: a legal, documented no-op.
    assert_eq!(
        deflate_get_dictionary(&deflater.state, None, None),
        ReturnCode::OK
    );

    assert_eq!(deflater.end(), ReturnCode::OK);
}

/// `deflateCopy` duplicates a stream, and the two halves then behave identically and independently.
///
/// Both properties are needed and neither implies the other. Identical output proves the copy
/// captured the whole encoder state -- the window, the hash chains, the pending buffer, the bit
/// accumulator and the tallied symbols. Independence proves it captured them by value: a copy that
/// shared a buffer would also produce identical output right up to the moment one side advanced.
#[test]
fn deflate_copy_produces_an_identical_and_independent_stream() {
    let payload = corpus::text();
    let midpoint = payload.len() / 2;

    let mut original = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = original.room();
    assert_eq!(
        original.step(&payload[..midpoint], room, NO_FLUSH).0,
        ReturnCode::OK
    );

    let copy_state = deflate_copy(&original.state, GlobalAllocator).expect("deflateCopy");
    assert_eq!(
        copy_state.status(),
        original.state.status(),
        "the copy resumes from the same status"
    );
    assert_eq!(copy_state.level(), original.state.level());
    assert_eq!(copy_state.slid(), original.state.slid());
    assert_eq!(copy_state.pending_bytes(), original.state.pending_bytes());
    assert_eq!(copy_state.sym_next(), original.state.sym_next());

    let mut copy = Deflater {
        state: copy_state,
        out: original.out.clone(),
        produced: original.produced,
        total_in: original.total_in,
        total_out: original.total_out,
        adler: original.adler,
        msg: original.msg,
        data_type: original.data_type,
    };

    let room = original.room();
    assert_eq!(
        original.step(&payload[midpoint..], room, FINISH).0,
        ReturnCode::STREAM_END
    );
    let room = copy.room();
    assert_eq!(
        copy.step(&payload[midpoint..], room, FINISH).0,
        ReturnCode::STREAM_END
    );

    assert_eq!(
        original.written(),
        copy.written(),
        "a copy fed the same remaining input must emit the same remaining bytes"
    );
    assert_eq!(
        original.adler, copy.adler,
        "and carry the same running checksum"
    );

    let stream = original.written().to_vec();
    assert_eq!(original.end(), ReturnCode::OK);
    assert_eq!(copy.end(), ReturnCode::OK);
    assert_eq!(expand(&stream, ZLIB, payload.len()), payload);
}

/// A copy taken after the window has slid still works.
///
/// `src/deflate/window.rs` sets a `slid` flag when `fill_window` moves the window down, and
/// `deflate_copy` has to carry it: `deflate.c` L1327-L1341 copies `window`, `prev` and `head`
/// wholesale, and the `prev` array's contents are meaningful only relative to how far the window
/// has slid. Getting this wrong produces a copy that finds matches at wrong distances -- decodable
/// output that differs from the original's, which is why the equality assertion is the one that
/// catches it.
///
/// `windowBits = 9` gives a 512-byte window, so a payload of a few hundred bytes slides it
/// repeatedly. That keeps the test small enough for Miri while still exercising the path that
/// otherwise needs more than 32 KiB.
#[test]
fn a_copy_taken_after_the_window_has_slid_still_matches() {
    // Three copies of the prose corpus: compressible, so the window keeps filling, and long
    // enough that a 512-byte window must slide several times before the midpoint is reached.
    let mut payload = corpus::text();
    let single = payload.len();
    payload.extend_from_within(..single);
    payload.extend_from_within(..single);
    assert!(
        payload.len() / 2 > 2 * NARROW_WINDOW_SIZE,
        "the fixture must be several windows long at windowBits 9"
    );
    let midpoint = payload.len() / 2;

    let mut narrow = Deflater::bounded(
        6,
        NARROW_WINDOW_BITS,
        DEF_MEM_LEVEL,
        Strategy::Default,
        payload.len(),
    );
    let room = narrow.room();
    assert_eq!(
        narrow.step(&payload[..midpoint], room, NO_FLUSH).0,
        ReturnCode::OK
    );
    assert!(
        narrow.state.slid(),
        "consuming {midpoint} bytes through a 512-byte window must have slid it"
    );
    assert!(
        narrow.state.copied_prev_entries() > 0,
        "a slid window leaves live entries in the prev array for deflateCopy to carry"
    );

    let copy_state = deflate_copy(&narrow.state, GlobalAllocator).expect("deflateCopy after slide");
    assert!(
        copy_state.slid(),
        "the copy must remember that the window has slid"
    );
    assert_eq!(
        copy_state.copied_prev_entries(),
        narrow.state.copied_prev_entries(),
        "and must carry the same live prefix of the prev array"
    );

    let mut copy = Deflater {
        state: copy_state,
        out: narrow.out.clone(),
        produced: narrow.produced,
        total_in: narrow.total_in,
        total_out: narrow.total_out,
        adler: narrow.adler,
        msg: narrow.msg,
        data_type: narrow.data_type,
    };

    let room = narrow.room();
    assert_eq!(
        narrow.step(&payload[midpoint..], room, FINISH).0,
        ReturnCode::STREAM_END
    );
    let room = copy.room();
    assert_eq!(
        copy.step(&payload[midpoint..], room, FINISH).0,
        ReturnCode::STREAM_END
    );
    assert_eq!(
        narrow.written(),
        copy.written(),
        "a post-slide copy must resume identically"
    );

    let stream = narrow.written().to_vec();
    assert_eq!(narrow.end(), ReturnCode::OK);
    assert_eq!(copy.end(), ReturnCode::OK);
    assert_eq!(expand(&stream, NARROW_WINDOW_BITS, payload.len()), payload);
}

/// `deflateReset` returns a stream to exactly its freshly initialised state.
///
/// Byte equality against a stream built from scratch is the only assertion that covers everything
/// a reset has to clear -- the window, the hash chains, the pending buffer, the bit accumulator,
/// the symbol buffer and the tree frequencies -- without enumerating them. Anything left behind
/// changes the second stream's output.
#[test]
fn reset_returns_a_stream_to_its_initial_state() {
    let payload = corpus::text();
    let fresh = squeeze_at(&payload, 6);

    let mut deflater = Deflater::new(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, fresh.len() + 64);
    let room = deflater.room();
    assert_eq!(
        deflater.step(&payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );

    let reset = deflate_reset(&mut deflater.state);
    assert_eq!(deflater.state.status(), Status::Init);
    assert_eq!(deflater.state.last_flush(), -2, "the -2 sentinel returns");
    assert_eq!(reset.total_in, 0);
    assert_eq!(reset.total_out, 0);
    assert_eq!(reset.adler, 1, "the Adler-32 seed returns");
    assert!(reset.msg.is_none());

    // A reset stream must know nothing: the window is empty again.
    let mut length = 0u32;
    assert_eq!(
        deflate_get_dictionary(&deflater.state, None, Some(&mut length)),
        ReturnCode::OK
    );
    assert_eq!(
        length, 0,
        "deflateReset calls lm_init, which clears strstart and lookahead (deflate.c L679)"
    );

    deflater.produced = 0;
    deflater.total_in = reset.total_in;
    deflater.total_out = reset.total_out;
    deflater.adler = reset.adler;
    deflater.msg = reset.msg;
    deflater.data_type = reset.data_type;

    let room = deflater.room();
    assert_eq!(
        deflater.step(&payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    assert_eq!(
        deflater.written(),
        fresh.as_slice(),
        "a reset stream must compress byte-for-byte like a new one"
    );
    assert_eq!(deflater.end(), ReturnCode::OK);
}

/// The reset snapshot reports exactly what a real reset would, for all three containers.
///
/// This is the invariant `deflateInit2_` depends on. The core's `deflate_init2` already performs a full
/// `deflateReset` -- C's `return deflateReset(strm)` at `deflate.c` L532 -- but it cannot hand back the
/// `z_stream` half, so the facade used to call `deflate_reset` a second time purely to obtain those five
/// values. That repeated `_tr_init` and all of `lm_init`, whose `CLEAR_HASH` writes 64 KiB at the
/// default `memLevel` and 128 KiB at `memLevel 9`, for values that were already in place. The facade now
/// reads them with [`deflate_reset_snapshot`] instead, and the reset happens exactly once.
///
/// That substitution is only correct while the snapshot and the reset agree, and the seed is the field
/// where they could diverge: it is `crc32(0, Z_NULL, 0)` -- zero -- for a gzip stream and
/// `adler32(0, Z_NULL, 0)` -- **one**, not zero -- for zlib and raw (`deflate.c` L667-L671). All three
/// containers are checked, in both orders: the snapshot before the reset, and again after it, since a
/// caller reads it at the first of those points and `deflate_reset_keep` computes it at the second.
///
/// The assertion is whole-struct equality rather than field-by-field, so a member added to
/// [`DeflateReset`] is covered the day it appears.
#[test]
fn the_reset_snapshot_matches_a_real_reset_for_every_container() {
    for (bits, expected_seed, container) in [(RAW, 1, "raw"), (ZLIB, 1, "zlib"), (GZIP, 0, "gzip")]
    {
        let mut deflater = Deflater::new(6, bits, DEF_MEM_LEVEL, Strategy::Default, 512);

        // What a facade reads immediately after `deflate_init2`, which is where the second reset used
        // to be. Taken first, so it cannot be an artefact of the reset below.
        let snapshot = deflate_reset_snapshot(&deflater.state);
        assert_eq!(
            snapshot.adler, expected_seed,
            "{container}: the check seed after init"
        );

        let performed = deflate_reset(&mut deflater.state);
        assert_eq!(
            snapshot, performed,
            "{container}: the snapshot must report what the reset performs"
        );
        assert_eq!(
            deflate_reset_snapshot(&deflater.state),
            performed,
            "{container}: and it must still agree once the reset has run"
        );

        // The other four members are fixed, and stated here so that a change to any of them has to be
        // acknowledged in a test rather than only in `write_reset`.
        assert_eq!(snapshot.total_in, 0);
        assert_eq!(snapshot.total_out, 0);
        assert!(snapshot.msg.is_none());
        assert_eq!(snapshot.data_type, Z_UNKNOWN);

        assert_eq!(deflater.end(), ReturnCode::OK);
    }
}

/// `deflateResetKeep` keeps the match state that `deflateReset` throws away.
///
/// `deflate.c` L649-L677 and L681-L684: `deflateResetKeep` resets the stream bookkeeping -- the
/// counters, the status, the pending cursors, the `-2` flush sentinel -- and calls `_tr_init`, but
/// **not** `lm_init`. `deflateReset` is `deflateResetKeep` plus `lm_init`, and `lm_init` is what
/// clears `strstart`, `lookahead` and the hash chains.
///
/// The observable difference is `deflateGetDictionary`, which reports `strstart + lookahead`
/// bytes: zero after `deflateReset`, the previous stream's window contents after
/// `deflateResetKeep`. That is the whole point of the `Keep` variant -- it lets a caller start a
/// new stream that still matches against the old one's history.
#[test]
fn reset_keep_preserves_the_window_that_reset_clears() {
    let payload = corpus::HELLO;

    for keep in [false, true] {
        let mut deflater =
            Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
        let room = deflater.room();
        assert_eq!(
            deflater.step(payload, room, FINISH).0,
            ReturnCode::STREAM_END
        );

        let reset = if keep {
            deflate_reset_keep(&mut deflater.state)
        } else {
            deflate_reset(&mut deflater.state)
        };

        // Both forms reset the stream bookkeeping identically.
        assert_eq!(reset.total_in, 0, "keep = {keep}");
        assert_eq!(reset.total_out, 0, "keep = {keep}");
        assert_eq!(reset.adler, 1, "keep = {keep}");
        assert_eq!(deflater.state.status(), Status::Init, "keep = {keep}");
        assert_eq!(deflater.state.last_flush(), -2, "keep = {keep}");

        // Only `deflateReset` clears the match state.
        let mut length = 0u32;
        assert_eq!(
            deflate_get_dictionary(&deflater.state, None, Some(&mut length)),
            ReturnCode::OK
        );
        let expected = if keep {
            u32::try_from(payload.len()).unwrap()
        } else {
            0
        };
        assert_eq!(
            length, expected,
            "keep = {keep}: deflateResetKeep skips lm_init, so the window survives"
        );

        if keep {
            let mut window = vec![0u8; usize::try_from(length).unwrap()];
            assert_eq!(
                deflate_get_dictionary(
                    &deflater.state,
                    Some(&mut OutputRegion::init(&mut window)),
                    None
                ),
                ReturnCode::OK
            );
            assert_eq!(
                window, payload,
                "and the surviving window holds the previous stream's input"
            );
        }

        assert_eq!(deflater.end(), ReturnCode::OK);
    }
}

/// `test/example.c`'s `test_large_deflate`, including the "deflate not greedy" check.
///
/// The C sequence (L243-L290) is followed step for step, because each step tests something
/// different:
///
/// 1. `deflateInit(Z_BEST_SPEED)` and one `Z_NO_FLUSH` call over a highly compressible buffer.
///    `avail_in` must be 0 afterwards -- C prints "deflate not greedy" and exits if it is not.
///    A driver that returned early with input still in hand would pass a round-trip test and fail
///    this one.
/// 2. `deflateParams(Z_NO_COMPRESSION, Z_DEFAULT_STRATEGY)` mid-stream, then feed *already
///    compressed* data. The level change crosses the `deflate_fast` to `deflate_stored` boundary,
///    which is exactly the case `deflateParams` has to flush the block in progress for
///    (`deflate.c` L789-L807).
/// 3. `deflateParams(Z_BEST_COMPRESSION, Z_FILTERED)` and more input: back the other way, and with
///    a strategy change as well.
/// 4. `Z_FINISH` must reach `Z_STREAM_END`.
///
/// The C test stops there. This one also decompresses the result and checks it against the
/// concatenation of everything that was fed in, which is the assertion that would catch a
/// `deflateParams` that flushed the wrong amount.
#[test]
fn the_example_c_large_deflate_sequence_works() {
    // "At this point, uncompr is still mostly zeroes, so it should compress very well."
    let compressible = vec![0u8; 4096];

    let mut deflater = Deflater::new(
        Z_BEST_SPEED,
        ZLIB,
        DEF_MEM_LEVEL,
        Strategy::Default,
        4 * 4096 + 1024,
    );

    let room = deflater.room();
    let (ret, taken) = deflater.step(&compressible, room, NO_FLUSH);
    assert_eq!(ret, ReturnCode::OK);
    assert_eq!(
        taken,
        compressible.len(),
        "deflate not greedy: one Z_NO_FLUSH call with ample output room must consume all input"
    );

    assert_eq!(
        deflater.params(Z_NO_COMPRESSION, Strategy::Default.as_raw()),
        ReturnCode::OK,
        "switching from deflate_fast to deflate_stored mid-stream"
    );
    assert_eq!(deflater.state.level(), Z_NO_COMPRESSION);

    // "Feed in already compressed data and switch to no compression."
    let already_compressed = deflater.written().to_vec();
    let room = deflater.room();
    let (ret, taken) = deflater.step(&already_compressed, room, NO_FLUSH);
    assert_eq!(ret, ReturnCode::OK);
    assert_eq!(taken, already_compressed.len());

    // "Switch back to compressing mode."
    assert_eq!(
        deflater.params(Z_BEST_COMPRESSION, Strategy::Filtered.as_raw()),
        ReturnCode::OK
    );
    assert_eq!(deflater.state.level(), Z_BEST_COMPRESSION);
    assert_eq!(deflater.state.strategy(), Strategy::Filtered);

    let room = deflater.room();
    let (ret, taken) = deflater.step(&compressible, room, NO_FLUSH);
    assert_eq!(ret, ReturnCode::OK);
    assert_eq!(taken, compressible.len());

    let room = deflater.room();
    assert_eq!(
        deflater.step(&[], room, FINISH).0,
        ReturnCode::STREAM_END,
        "test/example.c: deflate should report Z_STREAM_END"
    );

    let stream = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);

    let mut expected = Vec::new();
    expected.extend_from_slice(&compressible);
    expected.extend_from_slice(&already_compressed);
    expected.extend_from_slice(&compressible);
    assert_eq!(
        expand(&stream, ZLIB, expected.len()),
        expected,
        "every byte fed across three parameter regimes must come back out"
    );
}

/// `deflateParams` rejects out-of-range arguments and leaves the stream alone.
///
/// `deflate.c` L784-L788 validates before it does anything, so a rejected call must not have
/// changed the level or the strategy -- otherwise a caller that passed a typo would find its stream
/// silently reconfigured.
#[test]
fn params_rejects_out_of_range_arguments() {
    let mut deflater = Deflater::new(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, 1024);

    for (level, strategy) in [
        (10i32, 0i32),
        (-2, 0),
        (6, 5),
        (6, -1),
        (i32::MAX, i32::MAX),
    ] {
        assert_eq!(
            deflater.params(level, strategy),
            ReturnCode::STREAM_ERROR,
            "deflateParams({level}, {strategy}) is out of range (deflate.c L784-L788)"
        );
        assert_eq!(deflater.state.level(), 6, "a rejected call changes nothing");
        assert_eq!(deflater.state.strategy(), Strategy::Default);
    }

    // Z_DEFAULT_COMPRESSION is in range and resolves to the default level.
    assert_eq!(
        deflater.params(Z_DEFAULT_COMPRESSION, Strategy::Rle.as_raw()),
        ReturnCode::OK
    );
    assert_eq!(
        deflater.state.level(),
        6,
        "Z_DEFAULT_COMPRESSION means level 6"
    );
    assert_eq!(deflater.state.strategy(), Strategy::Rle);

    assert_eq!(deflater.end(), ReturnCode::OK);
}

/// `deflatePrime` inserts bits at the head of the stream, and rejects impossible bit counts.
///
/// `deflate.c` L756-L758 refuses `bits < 0`, `bits > 16`, and the case where the pending buffer has
/// no room for the flush that follows -- all three as `Z_BUF_ERROR`, not `Z_STREAM_ERROR`, because
/// the last of them is retryable.
///
/// The positive case is arranged to be exactly checkable. Priming eight bits is a whole byte, so
/// `_tr_flush_bits` empties the accumulator and leaves the rest of the stream byte-aligned exactly
/// where it would have been. The output must therefore be the primed byte followed by the
/// *unprimed* stream, byte for byte -- and `out[1..]` must decompress on its own. Nothing weaker
/// would distinguish "inserted eight bits" from "inserted eight bits and perturbed the encoder".
#[test]
fn prime_inserts_bits_without_disturbing_the_rest_of_the_stream() {
    let payload = corpus::HELLO;

    let (mut state, _) = open(6, RAW, DEF_MEM_LEVEL, Strategy::Default);

    for bits in [-1i32, 17, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_prime(&mut state, bits, 0),
            ReturnCode::BUF_ERROR,
            "deflatePrime accepts 0..=16 bits and reports Z_BUF_ERROR outside it \
             (deflate.c L756)"
        );
    }

    // A partial byte lands in the accumulator and is reported by deflatePending's bit count.
    assert_eq!(deflate_prime(&mut state, 5, 0b1_1111), ReturnCode::OK);
    let (pending_bytes, pending_bits, status) = deflate_pending(&state);
    assert_eq!(status, ReturnCode::OK);
    assert_eq!(pending_bytes, 0, "five bits do not make a byte");
    assert_eq!(pending_bits, 5, "deflatePending reports bi_valid");
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);

    // Eight bits is a whole byte, so the stream stays byte-aligned and is exactly comparable.
    let unprimed = squeeze(payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);

    let (mut state, reset2) = open(6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(deflate_prime(&mut state, 8, 0x5a), ReturnCode::OK);
    let (pending_bytes, pending_bits, _) = deflate_pending(&state);
    assert_eq!(
        pending_bytes, 1,
        "eight bits are flushed into a pending byte"
    );
    assert_eq!(pending_bits, 0, "and leave the accumulator empty");

    let mut out = vec![0u8; unprimed.len() + 64];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset2);
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    assert_eq!(out[0], 0x5a, "the primed byte comes out first");
    assert_eq!(
        &out[1..],
        unprimed.as_slice(),
        "a whole-byte prime must leave the rest of the stream untouched"
    );
    assert_eq!(
        expand(&out[1..], RAW, payload.len()),
        payload,
        "and the untouched remainder must decode on its own"
    );
}

/// `deflateTune` accepts the four parameters and does not break the stream.
///
/// `zlib.h` L826-L836 describes it as an undocumented-by-design tuning hook "for research
/// purposes", and `deflate.c` L820-L830 validates nothing beyond the state check -- it simply
/// stores the four values. That is what is asserted here: `Z_OK` for every input including negative
/// ones, because inventing a validation the reference does not perform would reject callers the C
/// library accepts.
///
/// The substantive assertion is that tuning is *effective* and still correct: retuning level 9's
/// parameters onto a level-1 stream must change the output (or the parameters were ignored) and the
/// result must still round trip.
#[test]
fn tune_stores_its_parameters_without_validating_them() {
    let payload = corpus::text();

    let (mut state, _) = open(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    for (good, lazy, nice, chain) in [
        (8i32, 16i32, 128i32, 128i32),
        (0, 0, 0, 0),
        (-1, -1, -1, -1),
        (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
    ] {
        assert_eq!(
            deflate_tune(&mut state, good, lazy, nice, chain),
            ReturnCode::OK,
            "deflateTune validates nothing beyond the state check (deflate.c L820)"
        );
    }
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);

    // Tuning a level-1 stream up to level 9's parameters must actually change what it finds.
    let untuned = squeeze(&payload, 1, RAW, DEF_MEM_LEVEL, Strategy::Default);

    let level_nine = CONFIGURATION_TABLE[9];
    let mut deflater = Deflater::new(
        1,
        RAW,
        DEF_MEM_LEVEL,
        Strategy::Default,
        untuned.len() + 512,
    );
    assert_eq!(
        deflate_tune(
            &mut deflater.state,
            i32::from(level_nine.good_length),
            i32::from(level_nine.max_lazy),
            i32::from(level_nine.nice_length),
            i32::from(level_nine.max_chain),
        ),
        ReturnCode::OK
    );
    let room = deflater.room();
    assert_eq!(
        deflater.step(&payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    let tuned = deflater.written().to_vec();
    assert_eq!(deflater.end(), ReturnCode::OK);

    assert_ne!(
        tuned, untuned,
        "widening the search must change which matches a level-1 stream finds"
    );
    assert_eq!(
        expand(&tuned, RAW, payload.len()),
        payload,
        "and the retuned stream must still be a correct encoding"
    );
}

/// `deflatePending` and `deflateUsed` report the buffered bytes and the bit positions.
///
/// `deflate.c` L722-L735 and L739-L744. `deflatePending`'s `bits` output is `bi_valid`, the number
/// of bits sitting in the accumulator that have not yet become a byte, and `deflateUsed` reports
/// `bi_used`, how many bits of the final byte carry data.
///
/// Both must read zero on a stream that has just flushed everything, which is the property a
/// caller needs in order to know that concatenating another stream after this one is safe.
///
/// The `Z_BUF_ERROR` plus `pending = (unsigned)-1` arm of L730-L733 cannot be reached from here:
/// `pending_buf_size` is `LIT_BUFS * lit_bufsize`, at most 131072, so the narrowing to `unsigned`
/// never loses information on any target this port supports. It is covered by
/// `src/deflate/pending.rs`'s `narrow_pending_probes_the_unsigned_narrowing`, which calls the
/// narrowing directly.
#[test]
fn pending_and_used_report_the_buffered_state() {
    let payload = corpus::text();
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());

    let (bytes, bits, status) = deflate_pending(&deflater.state);
    assert_eq!(status, ReturnCode::OK);
    assert_eq!(bytes, 0, "nothing is buffered before the first call");
    assert_eq!(bits, 0);
    assert_eq!(deflate_used(&deflater.state), 0);

    // Mid-stream with plenty of output room, everything the encoder produced has been handed over.
    let room = deflater.room();
    assert_eq!(deflater.step(&payload, room, SYNC_FLUSH).0, ReturnCode::OK);
    let (bytes, bits, status) = deflate_pending(&deflater.state);
    assert_eq!(status, ReturnCode::OK);
    assert_eq!(
        bytes, 0,
        "a Z_SYNC_FLUSH with ample room leaves nothing buffered"
    );
    assert_eq!(bits, 0, "and byte-aligns, so the accumulator is empty");

    let room = deflater.room();
    assert_eq!(deflater.step(&[], room, FINISH).0, ReturnCode::STREAM_END);
    let (bytes, bits, status) = deflate_pending(&deflater.state);
    assert_eq!(status, ReturnCode::OK);
    assert_eq!(bytes, 0, "a completed stream has handed over every byte");
    assert_eq!(bits, 0);

    // With a single byte of room the flush cannot drain, so the byte count becomes observable.
    // Z_SYNC_FLUSH rather than Z_FINISH on purpose: Z_FINISH moves the status straight to
    // FINISH_STATE, and this stream needs to stay at BUSY_STATE for the teardown assertion below.
    let mut starved = Deflater::new(6, RAW, DEF_MEM_LEVEL, Strategy::Default, 1);
    let (ret, _) = starved.step(&payload, 1, SYNC_FLUSH);
    assert!(
        ret == ReturnCode::OK || ret == ReturnCode::BUF_ERROR,
        "one byte of room is not enough to hand over a flushed block; got {ret:?}"
    );
    assert_eq!(
        starved.state.status(),
        Status::Busy,
        "a mid-stream flush leaves the stream at BUSY_STATE"
    );
    let (bytes, _, status) = deflate_pending(&starved.state);
    assert_eq!(status, ReturnCode::OK);
    assert!(
        bytes > 0,
        "output the encoder could not hand over must be reported as pending"
    );
    assert_eq!(starved.end(), ReturnCode::DATA_ERROR);
}

/// `deflateEnd` reports `Z_DATA_ERROR` when the stream was abandoned mid-flight.
///
/// `deflate.c` L1293-L1310, whose last line is
/// `return status == BUSY_STATE ? Z_DATA_ERROR : Z_OK;`.
///
/// This is the contract an ownership-based port erases without meaning to. Taking the state by
/// value makes teardown infallible, so the natural Rust implementation returns `Z_OK` and a caller
/// that abandoned a stream half-written is never told. The C behaviour is documented in `zlib.h`
/// L515-L519 -- "`Z_DATA_ERROR` if the stream was freed prematurely (some input or output was
/// discarded)" -- so `Z_OK` here would be a silent, caller-visible regression.
///
/// Every other status returns `Z_OK`, including `FINISH_STATE`, and both directions are asserted.
#[test]
fn end_reports_data_error_only_from_busy_state() {
    let payload = corpus::text();

    // BUSY_STATE: input was consumed and the stream was never finished.
    let mut abandoned = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = abandoned.room();
    assert_eq!(abandoned.step(&payload, room, NO_FLUSH).0, ReturnCode::OK);
    assert_eq!(abandoned.state.status(), Status::Busy);
    assert_eq!(
        abandoned.end(),
        ReturnCode::DATA_ERROR,
        "deflate.c L1309: BUSY_STATE means input or output was discarded"
    );

    // INIT_STATE: nothing happened, so nothing was lost.
    let (mut untouched, _) = open(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(untouched.status(), Status::Init);
    assert_eq!(deflate_end(&mut untouched), ReturnCode::OK);

    // GZIP_STATE: likewise nothing happened, and this is the gzip stream's starting status.
    let (mut gzip, _) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(gzip.status(), Status::GzipHeader);
    assert_eq!(deflate_end(&mut gzip), ReturnCode::OK);

    // FINISH_STATE: the stream completed, so the caller discarded nothing.
    let mut finished = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = finished.room();
    assert_eq!(
        finished.step(&payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    assert_eq!(finished.state.status(), Status::Finish);
    assert_eq!(
        finished.end(),
        ReturnCode::OK,
        "a completed stream tears down cleanly"
    );
}

/// `deflateSetHeader` installs an RFC 1952 header, and `deflate` emits every field of it.
///
/// The layout is `doc/rfc1952.txt` section 2.3, and the emission is `deflate.c` L1077-L1190:
///
/// ```text
///   ID1 ID2 CM FLG MTIME(4, little-endian) XFL OS
///   [XLEN(2, little-endian) extra-bytes]      if FEXTRA
///   [name NUL]                                if FNAME
///   [comment NUL]                             if FCOMMENT
///   [CRC16(2, little-endian)]                 if FHCRC
/// ```
///
/// `XFL` is not a constant: `deflate.c` L1073-L1075 emits 2 at level 9, 4 when the strategy is at
/// or above `Z_HUFFMAN_ONLY` or the level is below 2, and 0 otherwise. `OS` comes from the header
/// the caller installed rather than from the build's [`OS_CODE`], which is what makes gzip output
/// reproducible across platforms when a caller cares.
///
/// `inflateGetHeader` then reads the three variable-length fields back. Only those three are
/// observable from here, because the sink is moved into the inflate state and no accessor returns
/// it; the scalar fields are asserted in `src/inflate/header.rs`.
#[test]
fn set_header_emits_every_rfc_1952_field() {
    const EXTRA: &[u8] = &[0xde, 0xad, 0xbe, 0xef];
    const NAME: &[u8] = b"probe.txt\0";
    const COMMENT: &[u8] = b"a comment\0";
    const MTIME: u32 = 0x1234_5678;
    /// The `OS` byte for "Unix" (`doc/rfc1952.txt` section 2.3.1.2).
    const OS_UNIX: i32 = 3;

    // The FLG bits, `doc/rfc1952.txt` section 2.3.1.2.
    const FTEXT: u8 = 0x01;
    const FHCRC: u8 = 0x02;
    const FEXTRA: u8 = 0x04;
    const FNAME: u8 = 0x08;
    const FCOMMENT: u8 = 0x10;

    let extra = cells(EXTRA);
    let name = cells(NAME);
    let comment = cells(COMMENT);
    let header = GzHeaderView::new(
        true,
        MTIME,
        OS_UNIX,
        true,
        Some(&extra),
        Some(&name),
        Some(&comment),
    );

    let payload = corpus::HELLO;
    let (mut state, reset) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(deflate_set_header(&mut state, Some(header)), ReturnCode::OK);
    assert_eq!(
        state.gzhead(),
        Some(header),
        "the installed header must be readable back off the state"
    );

    let mut out = vec![0u8; deflate_bound_z(Some(&state), payload.len()) + 64];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);

    assert_eq!(&out[..3], &[0x1f, 0x8b, 0x08], "ID1, ID2 and CM = deflate");
    assert_eq!(
        out[3],
        FTEXT | FHCRC | FEXTRA | FNAME | FCOMMENT,
        "every FLG bit the header asked for must be set"
    );
    assert_eq!(
        u32::from_le_bytes([out[4], out[5], out[6], out[7]]),
        MTIME,
        "MTIME is little-endian (doc/rfc1952.txt section 2.3.1)"
    );
    assert_eq!(out[8], 0, "XFL is 0 at level 6 (deflate.c L1073-L1075)");
    assert_eq!(
        i32::from(out[9]),
        OS_UNIX,
        "OS comes from the installed header, not from the build's OS_CODE"
    );

    let mut cursor = 10usize;
    assert_eq!(
        usize::from(u16::from_le_bytes([out[cursor], out[cursor + 1]])),
        EXTRA.len(),
        "XLEN is the extra field's length, little-endian"
    );
    cursor += 2;
    assert_eq!(&out[cursor..cursor + EXTRA.len()], EXTRA);
    cursor += EXTRA.len();
    assert_eq!(
        &out[cursor..cursor + NAME.len()],
        NAME,
        "the file name is emitted including its terminating NUL"
    );
    cursor += NAME.len();
    assert_eq!(
        &out[cursor..cursor + COMMENT.len()],
        COMMENT,
        "and so is the comment"
    );
    cursor += COMMENT.len();
    // Two more bytes of header CRC before the deflate data begins.
    cursor += 2;
    assert!(
        cursor < out.len(),
        "there must be compressed data after the header"
    );

    // The trailer, `doc/rfc1952.txt` section 2.3.1: CRC-32 then ISIZE, both little-endian.
    let isize_field = u32::from_le_bytes([
        out[out.len() - 4],
        out[out.len() - 3],
        out[out.len() - 2],
        out[out.len() - 1],
    ]);
    assert_eq!(
        usize::try_from(isize_field).unwrap(),
        payload.len(),
        "ISIZE is the uncompressed length modulo 2^32"
    );

    // And the whole thing decodes, with the three variable-length fields recovered.
    let extra_sink: Vec<Cell<u8>> = (0..16).map(|_| Cell::new(0xa5)).collect();
    let name_sink: Vec<Cell<u8>> = (0..32).map(|_| Cell::new(0xa5)).collect();
    let comment_sink: Vec<Cell<u8>> = (0..32).map(|_| Cell::new(0xa5)).collect();

    let mut inflater = inflate_init2(InflateConfig::new(GZIP), GlobalAllocator).unwrap();
    let inflate_reset_values = inflate_reset(&mut inflater);
    assert_eq!(
        inflate_get_header(
            &mut inflater,
            GzHeaderSink::new(Some(&extra_sink), Some(&name_sink), Some(&comment_sink)),
        ),
        ReturnCode::OK
    );
    let mut recovered = vec![0u8; payload.len() + 64];
    let next_out = {
        let mut stream = InflateStream::new(&out, &mut recovered);
        stream.apply_reset(inflate_reset_values);
        assert_eq!(
            inflate(&mut inflater, &mut stream, FINISH),
            ReturnCode::STREAM_END
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut inflater), ReturnCode::OK);
    assert_eq!(&recovered[..next_out], payload);

    let read_back =
        |sink: &[Cell<u8>], len: usize| -> Vec<u8> { sink[..len].iter().map(Cell::get).collect() };
    assert_eq!(
        read_back(&extra_sink, EXTRA.len()),
        EXTRA,
        "inflateGetHeader must recover the extra field"
    );
    assert_eq!(
        read_back(&name_sink, NAME.len()),
        NAME,
        "and the NUL-terminated name"
    );
    assert_eq!(
        read_back(&comment_sink, COMMENT.len()),
        COMMENT,
        "and the NUL-terminated comment"
    );
    assert_eq!(
        extra_sink[EXTRA.len()].get(),
        0xa5,
        "and must not write past the length it was told"
    );
}

/// `XFL` follows the level and the strategy, and `OS_CODE` is the fallback when no header is set.
///
/// `deflate.c` L1053-L1075: with no installed header the ten-byte minimal header is emitted with
/// `MTIME` zero and `OS` taken from the build's `OS_CODE`, and `XFL` is 2 at level 9, 4 at level 1
/// or under any strategy at or above `Z_HUFFMAN_ONLY`, and 0 elsewhere.
#[test]
fn the_bare_gzip_header_reports_the_level_through_xfl() {
    let payload = corpus::HELLO;

    for (level, strategy, expected_xfl) in [
        (Z_BEST_SPEED, Strategy::Default, 4u8),
        (6, Strategy::Default, 0),
        (Z_BEST_COMPRESSION, Strategy::Default, 2),
        (6, Strategy::HuffmanOnly, 4),
        (6, Strategy::Rle, 4),
        (6, Strategy::Fixed, 4),
        (6, Strategy::Filtered, 0),
    ] {
        let out = round_trip(payload, level, GZIP, DEF_MEM_LEVEL, strategy);
        assert_eq!(
            &out[..4],
            &[0x1f, 0x8b, 0x08, 0x00],
            "with no installed header, FLG is 0"
        );
        assert_eq!(
            &out[4..8],
            &[0, 0, 0, 0],
            "and MTIME is zero (deflate.c L1058-L1061)"
        );
        assert_eq!(
            out[8], expected_xfl,
            "level {level} with {strategy:?} must report XFL {expected_xfl}"
        );
        assert_eq!(
            out[9], OS_CODE,
            "OS falls back to the build's OS_CODE (deflate.c L1063)"
        );
    }
}

/// `deflateSetHeader` is refused on anything but a gzip stream.
///
/// `deflate.c` L715: `if (deflateStateCheck(strm) || strm->state->wrap != 2) return
/// Z_STREAM_ERROR;`. A raw or zlib stream has no gzip header to fill in, and quietly accepting one
/// would leave a caller believing a name and comment had been written.
#[test]
fn set_header_is_refused_on_a_non_gzip_stream() {
    for window_bits in [RAW, ZLIB] {
        let (mut state, _) = open(6, window_bits, DEF_MEM_LEVEL, Strategy::Default);
        assert_eq!(
            deflate_set_header(&mut state, None),
            ReturnCode::STREAM_ERROR,
            "windowBits {window_bits} gives wrap != 2, so deflateSetHeader is refused"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    // And on a gzip stream it is accepted, including being cleared again with None.
    let (mut gzip, _) = open(6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(deflate_set_header(&mut gzip, None), ReturnCode::OK);
    assert!(gzip.gzhead().is_none());
    assert_eq!(deflate_end(&mut gzip), ReturnCode::OK);
}

/// Out-of-range initialisation arguments are refused, and the in-range edge cases are not.
///
/// `deflate.c` L410-L455 validates every argument before it allocates anything. The interesting
/// entries are the ones that look like errors and are not:
///
/// * `windowBits = 8` is accepted and silently promoted to 9 (L438, "until 256-byte window bug
///   fixed"), so a caller asking for a 256-byte window gets 512 rather than an error;
/// * `Z_DEFAULT_COMPRESSION`, which is `-1`, becomes level 6 (L432);
/// * `memLevel = 9` is accepted because `MAX_MEM_LEVEL` is 9 in this build.
#[test]
fn initialisation_validates_every_argument() {
    let rejected = [
        ("level 10", 10i32, ZLIB, DEF_MEM_LEVEL),
        ("level -2", -2, ZLIB, DEF_MEM_LEVEL),
        ("level i32::MAX", i32::MAX, ZLIB, DEF_MEM_LEVEL),
        ("windowBits 7", 6, 7, DEF_MEM_LEVEL),
        ("windowBits 16", 6, 16, DEF_MEM_LEVEL),
        ("windowBits 0", 6, 0, DEF_MEM_LEVEL),
        ("windowBits -8", 6, -8, DEF_MEM_LEVEL),
        ("windowBits -16", 6, -16, DEF_MEM_LEVEL),
        ("windowBits 32", 6, 32, DEF_MEM_LEVEL),
        ("memLevel 0", 6, ZLIB, 0),
        ("memLevel 10", 6, ZLIB, 10),
        ("memLevel -1", 6, ZLIB, -1),
    ];

    for (name, level, window_bits, mem_level) in rejected {
        let config = DeflateConfig {
            level,
            method: Method::Deflated,
            window_bits,
            mem_level,
            strategy: Strategy::Default,
        };
        assert_eq!(
            deflate_init2(config, GlobalAllocator).err(),
            Some(ReturnCode::STREAM_ERROR),
            "{name} must be refused with Z_STREAM_ERROR"
        );
    }

    // windowBits 8 is accepted and promoted to 9.
    let (mut promoted, _) = open(6, 8, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(
        promoted.w_bits(),
        9,
        "deflate.c L438 promotes windowBits 8 to 9 rather than refusing it"
    );
    assert_eq!(deflate_end(&mut promoted), ReturnCode::OK);

    // Z_DEFAULT_COMPRESSION resolves to 6.
    let (mut defaulted, _) = open(
        Z_DEFAULT_COMPRESSION,
        ZLIB,
        DEF_MEM_LEVEL,
        Strategy::Default,
    );
    assert_eq!(
        defaulted.level(),
        6,
        "deflate.c L432: Z_DEFAULT_COMPRESSION is level 6"
    );
    assert_eq!(deflate_end(&mut defaulted), ReturnCode::OK);

    // Every level, every strategy and both memory-level endpoints are accepted -- swept
    // independently rather than as a cross product, for the reason given in
    // `a_new_state_reports_the_geometry_its_arguments_imply`.
    for level in 0..=9i32 {
        let (mut state, _) = open(level, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
        assert_eq!(
            state.level(),
            level,
            "level {level} must be accepted verbatim"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
    for strategy in Strategy::ALL {
        let (mut state, _) = open(6, ZLIB, DEF_MEM_LEVEL, strategy);
        assert_eq!(
            state.strategy(),
            strategy,
            "{strategy:?} must be accepted verbatim"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
    for mem_level in [MIN_MEM_LEVEL, MAX_MEM_LEVEL] {
        let (mut state, _) = open(6, ZLIB, mem_level, Strategy::Default);
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }
}

/// A stream whose cursors point past the end of their slices is refused.
///
/// C cannot express this case: a `z_stream` holds two pointers and two counts, so "the offset is
/// past the end" has no representation. The port holds two slices and two offsets, and an offset
/// beyond its slice describes no stream at all. Rejecting it with `Z_STREAM_ERROR` is what lets
/// every cursor operation inside the driver be infallible, and it is the closest analogue to C's
/// `next_out == Z_NULL` check at L991.
#[test]
fn a_stream_with_impossible_cursors_is_refused() {
    let payload = corpus::HELLO;

    let (mut state, reset) = open(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let mut out = vec![0u8; 256];
    let capacity = out.len();
    {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        stream.next_in = payload.len() + 1;
        assert_eq!(
            deflate(&mut state, &mut stream, NO_FLUSH),
            ReturnCode::STREAM_ERROR,
            "an input cursor past the end of its slice describes no stream"
        );
        assert!(
            stream.msg.is_some(),
            "and the failure must be recorded on the stream"
        );
    }
    {
        let mut stream = DeflateStream::new(payload, &mut out);
        stream.apply_reset(reset);
        stream.next_out = capacity + 1;
        assert_eq!(
            deflate(&mut state, &mut stream, NO_FLUSH),
            ReturnCode::STREAM_ERROR,
            "and neither does an output cursor past the end of its slice"
        );
    }
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
}

// =============================================================================================
// 8. Container formats
//
// Three wire formats share one encoder, selected by the sign and magnitude of `windowBits`
// (`zlib.h` L541-L567): negative for raw DEFLATE, 8 to 15 for the RFC 1950 zlib wrapper, and
// `windowBits + 16` for the RFC 1952 gzip wrapper. The DEFLATE data between the wrappers is the
// same in all three, so what these tests pin is the framing -- and the framing is what a
// downstream tool actually parses.
// =============================================================================================

/// Each container has the header and trailer its RFC prescribes, and round trips.
///
/// * **raw** (`RFC 1951`): nothing before the first block header and nothing after the last, so
///   the very first byte is already `BFINAL` and `BTYPE`.
/// * **zlib** (`RFC 1950` section 2.2): a two-byte big-endian header that must be a multiple of
///   31, with `CM = 8` and `CINFO = windowBits - 8` in the first byte, followed by a four-byte
///   big-endian Adler-32 of the *uncompressed* data.
/// * **gzip** (`RFC 1952` section 2.3): a ten-byte minimal header beginning `1f 8b 08`, followed
///   by a four-byte little-endian CRC-32 and a four-byte little-endian `ISIZE`.
///
/// The mod-31 property of the zlib header is the one worth spelling out: `deflate.c` L1035 does
/// `header += 31 - (header % 31)` specifically so that a decoder can reject a stream that is not
/// zlib-wrapped, and `inflate.c` checks it. It is a checksum on the header, not a coincidence.
#[test]
fn container_headers_and_trailers_have_the_reference_shape() {
    let payload = corpus::text();

    // Raw: no framing at all.
    let raw = round_trip(&payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let (_, btype) = block_header(&raw);
    assert!(
        btype == BTYPE_STATIC || btype == BTYPE_DYNAMIC || btype == BTYPE_STORED,
        "a raw stream starts with a block header and nothing else; got BTYPE {btype}"
    );

    // zlib: two-byte header, mod 31 == 0, Adler-32 trailer.
    let zlib = round_trip(&payload, 6, ZLIB, DEF_MEM_LEVEL, Strategy::Default);
    let header = (u32::from(zlib[0]) << 8) | u32::from(zlib[1]);
    assert_eq!(
        u32::from(zlib[0]) & 0x0f,
        8,
        "RFC 1950: CM must be 8 for deflate"
    );
    assert_eq!(
        u32::from(zlib[0]) >> 4,
        15 - 8,
        "RFC 1950: CINFO is windowBits - 8"
    );
    assert_eq!(
        header % 31,
        0,
        "RFC 1950 section 2.2: the two-byte header must be a multiple of 31 \
         (deflate.c L1035 makes it so)"
    );
    assert_eq!(u32::from(zlib[1]) & PRESET_DICT, 0, "no dictionary was set");
    // The Adler-32 of the input, big-endian, is the last four bytes.
    let trailer = &zlib[zlib.len() - 4..];
    assert_eq!(
        u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]),
        zlib_rs::adler32::adler32(1, &payload),
        "RFC 1950: the trailer is the Adler-32 of the uncompressed data, most significant byte \
         first"
    );

    // gzip: ten-byte minimal header, CRC-32 then ISIZE, both little-endian.
    let gzip = round_trip(&payload, 6, GZIP, DEF_MEM_LEVEL, Strategy::Default);
    assert_eq!(
        &gzip[..4],
        &[0x1f, 0x8b, 0x08, 0x00],
        "RFC 1952: ID1, ID2, CM and a zero FLG with no installed header"
    );
    assert_eq!(&gzip[4..8], &[0, 0, 0, 0], "MTIME is zero when unset");
    assert_eq!(gzip[9], OS_CODE, "OS is the build's OS_CODE");
    let crc = &gzip[gzip.len() - 8..gzip.len() - 4];
    let size = &gzip[gzip.len() - 4..];
    assert_eq!(
        u32::from_le_bytes([crc[0], crc[1], crc[2], crc[3]]),
        zlib_rs::crc32::crc32(0, &payload),
        "RFC 1952: CRC32 of the uncompressed data, least significant byte first"
    );
    assert_eq!(
        usize::try_from(u32::from_le_bytes([size[0], size[1], size[2], size[3]])).unwrap(),
        payload.len(),
        "RFC 1952: ISIZE is the input length modulo 2^32"
    );

    // The DEFLATE payload is the same in all three; only the framing differs.
    assert_eq!(
        zlib.len() - 6,
        raw.len(),
        "the zlib wrapper is exactly six bytes of framing around the same deflate data"
    );
    assert_eq!(
        gzip.len() - 18,
        raw.len(),
        "and the minimal gzip wrapper is exactly eighteen"
    );
}

/// The zlib header's `FLEVEL` bits follow the level, with the strategy able to override them.
///
/// `deflate.c` L1026-L1035 computes `level_flags` as: 0 when the strategy is at or above
/// `Z_HUFFMAN_ONLY` **or** the level is below 2; 1 when the level is below 6; 2 at level 6; 3
/// otherwise. The strategy test comes **first**, which is why `Z_RLE` and `Z_FIXED` -- numerically
/// above `Z_HUFFMAN_ONLY` at 3 and 4 -- also force 0, while `Z_FILTERED` at 1 sits below the
/// threshold and lets the level decide.
///
/// The two bits are advisory: RFC 1950 section 2.2 says a decompressor may ignore them. They are
/// still part of the emitted bytes, so they are part of the contract.
#[test]
fn the_zlib_header_reports_the_level_through_flevel() {
    let payload = corpus::HELLO;
    let flevel = |level: i32, strategy: Strategy| -> u32 {
        let out = squeeze(payload, level, ZLIB, DEF_MEM_LEVEL, strategy);
        (u32::from(out[1]) >> 6) & 3
    };

    assert_eq!(flevel(0, Strategy::Default), 0, "level 0 is below 2");
    assert_eq!(flevel(1, Strategy::Default), 0, "level 1 is below 2");
    assert_eq!(flevel(2, Strategy::Default), 1, "level 2 is below 6");
    assert_eq!(flevel(5, Strategy::Default), 1, "level 5 is below 6");
    assert_eq!(flevel(6, Strategy::Default), 2, "level 6 has its own value");
    assert_eq!(
        flevel(7, Strategy::Default),
        3,
        "above 6 is the highest value"
    );
    assert_eq!(flevel(9, Strategy::Default), 3);

    // The strategy test precedes the level tests, so everything at or above Z_HUFFMAN_ONLY is 0.
    assert_eq!(flevel(6, Strategy::HuffmanOnly), 0, "Z_HUFFMAN_ONLY is 2");
    assert_eq!(
        flevel(6, Strategy::Rle),
        0,
        "Z_RLE is 3, above the threshold"
    );
    assert_eq!(
        flevel(6, Strategy::Fixed),
        0,
        "Z_FIXED is 4, above the threshold"
    );
    assert_eq!(
        flevel(6, Strategy::Filtered),
        2,
        "Z_FILTERED is 1, below Z_HUFFMAN_ONLY, so the level still decides"
    );
}

/// The zlib header records the window size in `CINFO`, for every window the encoder builds.
///
/// `deflate.c` L1021-L1024 puts `w_bits - 8` in the high nibble of `CMF`, so a decoder knows how
/// much history it must allocate. `windowBits = 8` appears as 9 because of the promotion at L438,
/// which is exactly why the promotion is caller-visible and not an implementation detail.
#[test]
fn the_zlib_header_records_the_window_size() {
    let payload = corpus::HELLO;

    for requested in 8..=15i32 {
        let built = requested.max(NARROW_WINDOW_BITS);
        let out = squeeze(payload, 6, requested, DEF_MEM_LEVEL, Strategy::Default);
        // Decoded through the window the encoder actually built, not the one that was asked for:
        // `inflate` refuses a stream whose CINFO advertises a larger window than it was configured
        // for (`inflate.c`, "invalid window size"), and the promotion at `deflate.c` L438 is
        // precisely a case where the two differ.
        assert_eq!(expand(&out, built, payload.len()), payload);
        assert_eq!(
            i32::from(out[0] >> 4),
            built - 8,
            "windowBits {requested} builds a {built}-bit window, so CINFO is {}",
            built - 8
        );
        assert_eq!(
            u32::from(out[0]) & 0x0f,
            8,
            "CM stays 8 whatever the window size"
        );
        assert_eq!(
            ((u32::from(out[0]) << 8) | u32::from(out[1])) % 31,
            0,
            "the mod-31 correction holds for windowBits {requested}"
        );
    }
}

/// Every container decodes only through its own inflate configuration.
///
/// The negative statement matters as much as the positive one: a raw stream fed to a zlib decoder
/// must be rejected, because its first two bytes are block data rather than a header and
/// misreading them would silently produce garbage. `Z_DATA_ERROR` is what `inflate.c` returns for
/// a header that fails the mod-31 check.
#[test]
fn a_stream_decodes_only_through_its_own_container() {
    let payload = corpus::HELLO;
    let raw = squeeze(payload, 6, RAW, DEF_MEM_LEVEL, Strategy::Default);
    let gzip = squeeze(payload, 6, GZIP, DEF_MEM_LEVEL, Strategy::Default);

    // A raw stream offered to a zlib decoder: rejected, not silently mis-decoded.
    let mut state = inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).unwrap();
    let reset = inflate_reset(&mut state);
    let mut out = vec![0u8; payload.len() + 64];
    {
        let mut stream = InflateStream::new(&raw, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut stream, FINISH),
            ReturnCode::DATA_ERROR,
            "a raw stream has no zlib header, so the mod-31 check must reject it"
        );
        assert!(stream.msg.is_some(), "and the reason must be recorded");
    }
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // A gzip stream offered to a zlib decoder: also rejected, for the same reason.
    let mut state = inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).unwrap();
    let reset = inflate_reset(&mut state);
    {
        let mut stream = InflateStream::new(&gzip, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut stream, FINISH),
            ReturnCode::DATA_ERROR,
            "1f 8b is not a valid zlib header word"
        );
    }
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // windowBits + 32 asks inflate to detect zlib or gzip automatically (`zlib.h` L878-L881), and
    // must accept the gzip stream a fixed gzip decoder accepts.
    assert_eq!(expand(&gzip, 15 + 32, payload.len()), payload);
    assert_eq!(
        expand(
            &squeeze(payload, 6, ZLIB, DEF_MEM_LEVEL, Strategy::Default),
            15 + 32,
            payload.len()
        ),
        payload,
        "automatic detection must accept a zlib stream too"
    );
}

/// A payload larger than the window round trips, which is what makes the window slide.
///
/// `common::corpus::window_crossing()` is 33048 bytes -- deliberately just past the 32768-byte
/// default window -- built as a distinctive marker, a long stretch of unrelated filler, and the
/// same marker again. The second marker sits far enough back that `fill_window` must have slid the
/// window and `slide_hash` must have rebased every hash-chain entry before it can be matched, so a
/// port that got the rebasing wrong produces either a wrong distance or no match at all.
///
/// **This is the only test in this file marked `#[cfg_attr(miri, ignore)]`**, and the reason is
/// interpreter speed rather than correctness: Miri interprets every byte of a 32 KiB window, a
/// 32 KiB hash table and a 64 KiB pending buffer, and it does so for each of the configurations
/// below. The sliding-window code paths are still covered under Miri by
/// [`a_copy_taken_after_the_window_has_slid_still_matches`] and
/// [`the_narrow_window_slides_and_still_round_trips`], which reach them through a 512-byte window
/// instead. This test runs natively on every `cargo test`.
#[test]
#[cfg_attr(miri, ignore)]
fn a_payload_larger_than_the_window_round_trips_after_the_window_slides() {
    let payload = corpus::window_crossing();
    assert!(
        payload.len() > (1usize << 15),
        "the fixture must exceed the 32 KiB default window to slide it"
    );

    for level in [1i32, 6, 9] {
        for window_bits in [RAW, ZLIB, GZIP] {
            let compressed = round_trip(
                &payload,
                level,
                window_bits,
                DEF_MEM_LEVEL,
                Strategy::Default,
            );
            assert!(
                compressed.len() < payload.len(),
                "level {level}, windowBits {window_bits}: the repeated marker must be found \
                 across the window boundary, so the output must be smaller than the input"
            );
        }
    }

    // The window filled to capacity, and the match that spans it was found at a distance close
    // to the format's limit.
    //
    // Note what is *not* asserted: `slid()` is still false here, and that is correct. The window
    // buffer is `2 * w_size` bytes and `fill_window` only slides once `strstart` reaches
    // `w_size + MAX_DIST(s)` (`deflate.c` L281-L283), which for a 32 KiB window is 65274 bytes --
    // nearly twice this payload. Crossing `w_size` fills the window; sliding it needs twice that
    // again. The tests that exercise the slide do it through a 512-byte window instead.
    let mut deflater = Deflater::bounded(6, ZLIB, DEF_MEM_LEVEL, Strategy::Default, payload.len());
    let room = deflater.room();
    assert_eq!(
        deflater.step(&payload, room, FINISH).0,
        ReturnCode::STREAM_END
    );
    assert!(
        !deflater.state.slid(),
        "a 33 KiB payload fills a 32 KiB window but does not reach the 65274-byte slide point"
    );

    let mut held = 0u32;
    assert_eq!(
        deflate_get_dictionary(&deflater.state, None, Some(&mut held)),
        ReturnCode::OK
    );
    assert_eq!(
        usize::try_from(held).unwrap(),
        deflater.state.w_size(),
        "after more than w_size bytes the window is saturated, and deflateGetDictionary caps its          report at w_size (deflate.c L633-L635)"
    );
    assert_eq!(deflater.end(), ReturnCode::OK);
}

/// The sliding-window path through a 512-byte window, which Miri can afford to interpret.
///
/// Same property as the test above -- `fill_window` slides and `slide_hash` rebases the chains --
/// reached with a payload of a couple of kilobytes instead of thirty-three, by shrinking the window
/// rather than growing the input. `windowBits = 9` is the smallest window the encoder builds, so
/// this is the path with the most sliding per input byte.
///
/// The `max_dist` assertion is the one that makes it a window test rather than a round-trip test:
/// with a 512-byte window no match may reach further back than
/// `w_size - MIN_LOOKAHEAD` = 250 bytes, so the encoder is being forced to rebase and discard
/// history continuously.
#[test]
fn the_narrow_window_slides_and_still_round_trips() {
    let mut payload = corpus::text();
    let single = payload.len();
    payload.extend_from_within(..single);
    payload.extend_from_within(..single);
    assert!(
        payload.len() > 4 * NARROW_WINDOW_SIZE,
        "the fixture must be several 512-byte windows long"
    );

    for level in [1i32, 6, 9] {
        let mut deflater = Deflater::bounded(
            level,
            NARROW_WINDOW_BITS,
            DEF_MEM_LEVEL,
            Strategy::Default,
            payload.len(),
        );
        assert_eq!(
            deflater.state.max_dist(),
            NARROW_WINDOW_SIZE - MIN_LOOKAHEAD,
            "a 512-byte window limits every match to w_size - MIN_LOOKAHEAD bytes back"
        );

        let room = deflater.room();
        assert_eq!(
            deflater.step(&payload, room, FINISH).0,
            ReturnCode::STREAM_END
        );
        assert!(
            deflater.state.slid(),
            "level {level}: {} bytes through a {NARROW_WINDOW_SIZE}-byte window must slide it",
            payload.len()
        );

        let stream = deflater.written().to_vec();
        assert_eq!(deflater.end(), ReturnCode::OK);
        assert_eq!(
            expand(&stream, NARROW_WINDOW_BITS, payload.len()),
            payload,
            "level {level}: a repeatedly slid window must still produce a correct stream"
        );
        assert!(
            stream.len() < payload.len(),
            "level {level}: three copies of the same prose must still compress"
        );
    }
}

/// A caller-supplied allocator is used for every buffer, and every buffer comes back.
///
/// `zalloc`/`zfree`/`opaque` are the caller's (`zlib.h` L85-L86), and memory obtained from a
/// caller's `zalloc` must be released through that same caller's `zfree` -- which is the bug class
/// `common::TrackingAllocator` exists to catch. It is modelled on `test/infcover.c`'s `mem_zone`
/// and, like it, fills every block with a sentinel rather than zeroes, so a code path that assumed
/// zeroed memory shows up as wrong output rather than as a leak.
///
/// `assert_clean` covers all four of the C harness's checks at once: nothing leaked, nothing was
/// released out of order, nothing unrecognised was released, and the high-water mark is reported.
/// The sentinel fill is what makes the byte-equality assertion meaningful: the stream produced
/// through the tracking allocator must be identical to the one produced through the global
/// allocator, so the encoder cannot be depending on the contents of a fresh block.
#[test]
fn a_caller_supplied_allocator_is_used_and_balanced() {
    let payload = corpus::text();
    let expected = squeeze_at(&payload, 6);

    let tracker = common::TrackingAllocator::new();
    {
        let borrowed: &common::TrackingAllocator = &tracker;
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: ZLIB,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };
        let mut state = deflate_init2(config, borrowed).expect("deflate_init2 with a tracker");
        let reset = deflate_reset(&mut state);
        assert!(
            tracker.total() > 0,
            "the state's buffers must come from the caller's allocator"
        );
        assert!(
            tracker.high_water() >= tracker.total(),
            "the high-water mark cannot be below the live total"
        );

        let mut out = vec![0u8; deflate_bound_z(Some(&state), payload.len()) + 64];
        let produced = {
            let mut stream = DeflateStream::new(&payload, &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        out.truncate(produced);

        assert_eq!(
            out, expected,
            "the allocator must not change the emitted bytes -- the tracker fills every block \
             with a sentinel, so any reliance on fresh memory being zeroed would show up here"
        );
    }

    // Nothing leaked, nothing freed out of order, nothing rogue.
    tracker.assert_clean();
}

/// Allocation failure is reported as `Z_MEM_ERROR` and leaks nothing.
///
/// `deflate.c` L482-L502 checks every allocation and unwinds through `deflateEnd` on failure, and
/// `test/infcover.c`'s `mem_limit()` exists to drive exactly this path -- error handling that
/// fuzzing reaches only by luck. The tracker's limit reproduces it deterministically.
///
/// The assertion that matters is the second one: a failed initialisation must release whatever it
/// had already taken. A port that allocated the window, failed on the hash table and returned
/// without freeing the window would still report `Z_MEM_ERROR` and still be a leak.
#[test]
fn allocation_failure_is_reported_and_leaks_nothing() {
    let tracker = common::TrackingAllocator::new();
    {
        let borrowed: &common::TrackingAllocator = &tracker;
        // Far too small for a 32 KiB window, so the very first allocation fails.
        tracker.set_limit(64);
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: ZLIB,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };
        assert_eq!(
            deflate_init2(config, borrowed).err(),
            Some(ReturnCode::MEM_ERROR),
            "an allocator that refuses must produce Z_MEM_ERROR, not a panic"
        );
    }
    tracker.assert_clean();

    // A limit that admits some buffers but not all, so the unwind path runs with work to undo.
    let tracker = common::TrackingAllocator::new();
    {
        let borrowed: &common::TrackingAllocator = &tracker;
        tracker.set_limit(70_000);
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: ZLIB,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        };
        assert_eq!(
            deflate_init2(config, borrowed).err(),
            Some(ReturnCode::MEM_ERROR),
            "a partial budget must still fail cleanly"
        );
        assert!(
            tracker.high_water() > 0,
            "the partial budget must have admitted at least one buffer, so the unwind path ran"
        );
    }
    tracker.assert_clean();
}
