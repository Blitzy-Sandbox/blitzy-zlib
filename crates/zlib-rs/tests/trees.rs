//! Integration tests for the Huffman coder -- `crates/zlib-rs/src/trees/**`, ported
//! from `trees.c` and `trees.h`.
//!
//! This subsystem owns three of the eight decision points that determine whether the
//! port's compressed output is byte-identical to the reference implementation's
//! (AAP §0.6.2), so it is one of the highest-value suites in this folder:
//!
//! | # | Decision point | Oracle |
//! |---|---|---|
//! | 6 | the `smaller` tie-break, `<=` on `depth` | `trees.c` L499-L501 |
//! | 7 | the forced two codes of a degenerate tree | `trees.c` L655-L661 |
//! | 8 | stored versus static versus dynamic block selection | `trees.c` L1027-L1074 |
//!
//! It additionally pins `detect_data_type` (`trees.c` L966-L991 and
//! `doc/txtvsbin.txt`), the six generated tables of `trees.h`, and the observable
//! consequences of the bit writer (`trees.c` L143-L191 and L253-L292).
//!
//! # ★ THE REACHABILITY CONSTRAINT -- read this before adding a test
//!
//! `crates/zlib-rs/src/trees/mod.rs` exposes **only the six generated data tables** as
//! `pub`:
//!
//! ```text
//! pub use crate::trees::static_tables::{
//!     _dist_code, _length_code, base_dist, base_length, static_dtree, static_ltree,
//! };
//! ```
//!
//! Everything else in that subtree is `pub(crate)` or private -- `_tr_init`,
//! `_tr_tally`, `_tr_flush_bits`, `_tr_align`, `_tr_stored_block`, `_tr_flush_block`,
//! `smaller`, `pqdownheap`, `pqremove`, `gen_bitlen`, `gen_codes`, `init_block`,
//! `build_tree`, `scan_tree`, `send_tree`, `build_bl_tree`, `send_all_trees`,
//! `compress_block`, `detect_data_type`, `bi_reverse`, `bi_flush`, `bi_windup`,
//! `send_bits`, `send_code`, `put_short`, `StaticTreeDesc`, `STATIC_L_DESC`,
//! `STATIC_D_DESC`, `STATIC_BL_DESC`, `extra_lbits`, `extra_dbits`, `extra_blbits`,
//! `bl_order`, `DIST_CODE_LEN`, `MAX_BL_BITS`, `END_BLOCK`, `REP_3_6`, `REPZ_3_10` and
//! `REPZ_11_138` -- because an integration test is an **external crate** and
//! `pub(crate)` items are invisible to it.
//!
//! That visibility is not an oversight to be worked around. It mirrors the `local`
//! linkage of the C sources and the `local:` block of `zlib.map`, and
//! `crates/libz-rs-sys/tests/symbol_parity.rs` asserts the resulting export surface
//! against the reference library's 111-symbol baseline. **Widening any of those items to
//! `pub` in order to test it from here would break that contract.** Do not do it.
//!
//! The coverage is therefore split, deliberately, in two:
//!
//! * **Direct unit coverage of the `pub(crate)` helpers** lives in the
//!   `#[cfg(test)] mod tests` blocks inside `src/trees/static_tables.rs`,
//!   `src/trees/tree_desc.rs`, `src/trees/bit_writer.rs` and `src/trees/build.rs`,
//!   which are *inside* the crate and can reach them.
//! * **This suite** asserts the six public tables directly, and reaches everything else
//!   through the public deflate driver -- [`deflate_init2`], [`deflate`],
//!   [`deflate_end`] -- observing only what a caller can genuinely observe: the emitted
//!   bytes, the three block-header bits of RFC 1951 §3.2.3, and the `data_type` the
//!   driver reports.
//!
//! Where a property of an internal helper cannot be observed from outside, the closest
//! observable consequence is asserted instead and the substitution is named in a comment
//! on the test.
//!
//! # Two porting traps this suite exists to catch
//!
//! 1. **The degenerate-tree cases are a panic detector, not a formality** -- see
//!    [`a_single_byte_payload_survives_the_forced_two_codes_path`] and its neighbours.
//!    `trees.c` L655-L661 decrements `s->opt_len` -- an
//!    unsigned counter -- below zero and relies on the wraparound cancelling later in
//!    `gen_bitlen`. In Rust that must be `wrapping_sub`
//!    (`src/trees/build.rs` L1057 and L1064); a plain `-=` compiles and then **panics on
//!    overflow in a debug build**, which is exactly the build `cargo test` and Miri
//!    produce. Empty, one-symbol, two-symbol and `Z_HUFFMAN_ONLY` inputs are the cheapest
//!    possible way to trip it.
//! 2. **`put_short` is least-significant byte first** (`trees.c` L143-L147) while
//!    `putShortMSB` is most-significant byte first (`deflate.c` L939-L942). They live in
//!    different files, do nearly the same thing, and confusing them produces a stream
//!    that is corrupt with no other symptom. The `LEN`/`NLEN` complement assertions pin
//!    the former.
//!
//! # What this suite deliberately does not do
//!
//! Element-for-element comparison of the ported Rust tables against the C arrays
//! themselves belongs to `crates/zlib-rs-differential/tests/table_equality.rs`, which can
//! link the C oracle. It is not duplicated here. What is asserted instead is stronger in
//! one respect and weaker in another: every claim below is derived from RFC 1951 §3.2.3
//! to §3.2.6 or from `doc/txtvsbin.txt`, i.e. from a normative text rather than from a
//! transcription, so a table that was transcribed wrongly *and* compared against the same
//! wrong transcription still fails here.
//!
//! # Constraints
//!
//! * No `unsafe` and no FFI. The crate under test carries `#![forbid(unsafe_code)]`
//!   (AAP §0.7.1(a)); its tests hold to the same standard.
//! * No third-party crates. `crates/zlib-rs/Cargo.toml` has an empty `[dependencies]`
//!   and declares no `[dev-dependencies]`, and the `[bans]` section of `deny.toml` names
//!   that table as the enforcement point (AAP §0.7.1(i)). Only `core`, `alloc`, `std`,
//!   `zlib_rs` and the shared [`common`] module are available; pseudo-random filler comes
//!   from `common::lcg_fill` rather than from `rand`.
//! * No `#![no_std]`. The library is `#![cfg_attr(not(feature = "std"), no_std)]`, but an
//!   integration test is its own crate and links `std` unconditionally.
//! * Every sweep is bounded, because this suite must also run under
//!   `cargo +nightly miri test -p zlib-rs --test trees` (AAP §0.7.1(d)). The one
//!   unbounded-in-spirit matrix is split into a small unconditional core and a
//!   `#[cfg_attr(miri, ignore)]` full sweep; see the round-trip section.
//! * Imports use **module paths**, never crate-root re-exports: `zlib-rs` declares
//!   `default = []` and every root re-export carries `#[cfg(feature = "rust-api")]`, so
//!   `zlib_rs::static_ltree` does not exist in the configuration `cargo test -p zlib-rs`
//!   builds, while `zlib_rs::trees::static_ltree` always does.

// The workspace lint table denies the panic-prone lints, which is right for library code
// and wrong for a test: a test asserts, an assertion that fails panics, and reading a
// fixture or a table by index is clearer than defensively matching on it. `clippy.toml`
// grants `unwrap`/`expect`/`panic` inside `#[test]` context, but there is no
// `allow-indexing-slicing-in-tests` option and the helpers below are file-scope rather
// than `#[test]` functions, so the relaxation is stated once here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use common::corpus;

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::config::{
    DeflateConfig, InflateConfig, Method, Strategy, DYN_TREES, MAX_MATCH, MIN_MATCH, STATIC_TREES,
    STORED_BLOCK, Z_BINARY, Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_SYNC_FLUSH,
    Z_TEXT, Z_UNKNOWN,
};
use zlib_rs::deflate::state::{CtData, D_CODES, LENGTH_CODES, LITERALS, L_CODES};
use zlib_rs::deflate::{
    deflate, deflate_bound_z, deflate_end, deflate_init2, deflate_reset, DeflateStream,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::{
    inflate, inflate_end, inflate_init2, inflate_reset, inflate_sync, InflateStream,
};
use zlib_rs::trees::{
    _dist_code, _length_code, base_dist, base_length, static_dtree, static_ltree,
};

// ---------------------------------------------------------------------------------------
// Oracle constants
//
// Every value here is transcribed from the C sources or from a normative text, with the
// citation beside it. None of them is reachable from an integration test -- `DIST_CODE_LEN`
// is `pub(crate)` in `src/trees/static_tables.rs`, and the RFC's boundaries are not
// constants in the port at all -- so they are restated rather than imported.
// ---------------------------------------------------------------------------------------

/// `DIST_CODE_LEN`, the length of `_dist_code` (`trees.c` L81).
///
/// 512 rather than 256 because `d_code` folds distances of 256 and above into the upper
/// half: `_dist_code[256 + (dist >> 7)]` (`deflate.h` L320-L321, over the array `trees.c`
/// L104-L105 describes).
const DIST_CODE_LEN: usize = 512;

/// The largest match distance RFC 1951 §3.2.5 admits, as `d_code` sees it -- that is, the
/// match distance minus one, so `32768 - 1`.
const MAX_REDUCED_DIST: usize = 32_767;

/// Highest literal whose fixed code is 8 bits wide (RFC 1951 §3.2.6).
const FIXED_8_BIT_LITERAL_MAX: usize = 143;

/// Highest literal whose fixed code is 9 bits wide (RFC 1951 §3.2.6).
const FIXED_9_BIT_LITERAL_MAX: usize = 255;

/// Highest symbol whose fixed code is 7 bits wide (RFC 1951 §3.2.6).
const FIXED_7_BIT_SYMBOL_MAX: usize = 279;

/// First canonical fixed code for the 8-bit literal run, `00110000` (RFC 1951 §3.2.6).
const FIXED_8_BIT_FIRST_CODE: u16 = 0b0011_0000;

/// First canonical fixed code for the 9-bit literal run, `110010000` (RFC 1951 §3.2.6).
const FIXED_9_BIT_FIRST_CODE: u16 = 0b1_1001_0000;

/// First canonical fixed code for the 7-bit symbol run, `0000000` (RFC 1951 §3.2.6).
const FIXED_7_BIT_FIRST_CODE: u16 = 0b000_0000;

/// First canonical fixed code for the trailing 8-bit run, `11000000` (RFC 1951 §3.2.6).
const FIXED_TAIL_FIRST_CODE: u16 = 0b1100_0000;

/// Width in bits of every fixed distance code (RFC 1951 §3.2.6).
const FIXED_DISTANCE_CODE_BITS: u16 = 5;

/// `windowBits` for a **raw** DEFLATE stream with the largest window.
///
/// Raw is what the block-header assertions need: a zlib or gzip wrapper would put its own
/// bytes in front of the first block and move the three header bits off byte 0
/// (`deflate.c` L1048 puts the zlib `CMF`/`FLG` word there, L1069 the gzip magic).
const RAW_WINDOW_BITS: i32 = -15;

/// A small raw window: 512 bytes, the smallest `deflateInit2_` accepts after its promotion
/// of 8 to 9 (`deflate.c` L439 and `zconf.h` L287).
///
/// Used only where the payload is a handful of bytes and the sweep is long, because a
/// state at this size allocates roughly two orders of magnitude less than the default one
/// and every allocated byte is a byte Miri has to interpret.
const SMALL_RAW_WINDOW_BITS: i32 = -9;

/// `DEF_MEM_LEVEL` (`zutil.h` L81), the `memLevel` `deflateInit_` supplies.
const DEF_MEM_LEVEL: i32 = 8;

/// The smallest `memLevel` `deflateInit2_` accepts, the companion of
/// [`SMALL_RAW_WINDOW_BITS`]. `MAX_MEM_LEVEL` is 9 (`zconf.h` L273-L278) and `deflateInit2_`
/// rejects `memLevel < 1` (`deflate.c` L434-L438), so this is the floor.
const MIN_MEM_LEVEL: i32 = 1;

/// The four levels every matrix in this file walks: `Z_NO_COMPRESSION`, `Z_BEST_SPEED`,
/// the default, and `Z_BEST_COMPRESSION` (`zlib.h` L194-L196).
///
/// Level 0 is not decoration. It is the only level that takes the `else` branch of the
/// block decision, where `opt_lenb = static_lenb = stored_len + 5` forces a stored block
/// (`trees.c` L1039-L1042), and the only one for which `detect_data_type` never runs
/// (L1002).
const LEVELS: [i32; 4] = [0, 1, 6, 9];

/// The gray list of `doc/txtvsbin.txt`: 7 (BEL), 8 (BS), 11 (VT), 12 (FF), 26 (SUB),
/// 27 (ESC). Tolerated, which means *ignored* -- never a reason on its own to call a
/// buffer text.
const GRAY_LIST: [u8; 6] = [7, 8, 11, 12, 26, 27];

/// Payload length for the sweeps that run under Miri.
///
/// Long enough to carry a mixture of literals and matches through `_tr_tally` and to make
/// `build_tree` construct a tree with real depth, short enough that a sweep of a hundred-odd
/// configurations finishes in an interpreted run. AAP §0.7.1(d) requires every sweep here to
/// be bounded, and together with [`small_raw_config`] this is what bounds them.
///
/// The larger payload classes are not lost -- they are covered by the tests marked
/// `#[cfg_attr(miri, ignore)]`, each of which says so.
const MIRI_SWEEP_LEN: usize = 128;

/// The first [`MIRI_SWEEP_LEN`] bytes of `corpus::text()`.
///
/// Allow-list-only prose, so it classifies as text, and a mixture of repeated words and
/// unique ones, so the compressor finds some matches and not others.
fn short_text() -> Vec<u8> {
    corpus::text()[..MIRI_SWEEP_LEN].to_vec()
}

/// The canonical byte-aligned empty stored block: `LEN` = 0 then `NLEN` = `0xffff`, both
/// least-significant byte first (RFC 1951 §3.2.4, emitted by `_tr_stored_block` through
/// `put_short`, `trees.c` L143-L147 and L233-L250).
///
/// This is the four-byte marker `Z_SYNC_FLUSH` and `Z_FULL_FLUSH` leave at the end of the
/// output, and it is the single most legible observable consequence of `bi_windup`'s
/// alignment (`trees.c` L181-L191).
const SYNC_MARKER: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// The three header bits every DEFLATE block begins with (RFC 1951 §3.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockHeader {
    /// `BFINAL`: set if and only if this is the last block of the data set.
    last: bool,
    /// `BTYPE`: `00` stored, `01` fixed (static) trees, `10` dynamic trees, `11` reserved.
    ///
    /// Compared against `STORED_BLOCK`, `STATIC_TREES` and `DYN_TREES`
    /// (`zutil.h` L87-L89), which are the same three values `_tr_flush_block` shifts into
    /// place at `trees.c` L1058 and L1065.
    block_type: u8,
}

/// Reads the header of the **first** block of a raw DEFLATE stream.
///
/// RFC 1951 §3.1.1 packs Huffman codes and header fields starting with the
/// least-significant bit of each byte, so for the first block -- and only for the first,
/// since later blocks need not begin on a byte boundary -- the three bits sit in byte 0:
///
/// ```text
///   bit 0      BFINAL
///   bits 1..2  BTYPE
/// ```
///
/// This is why every block-type assertion in this file uses [`RAW_WINDOW_BITS`]: a zlib
/// header would occupy bytes 0 and 1, and a gzip header at least ten.
fn first_block_header(stream: &[u8]) -> BlockHeader {
    assert!(
        !stream.is_empty(),
        "a DEFLATE stream carrying a block cannot be empty"
    );
    let first = stream[0];
    let block_type = (first >> 1) & 0b11;
    assert_ne!(
        block_type, 0b11,
        "BTYPE 11 is reserved and must never be emitted (RFC 1951 3.2.3)"
    );
    BlockHeader {
        last: (first & 1) == 1,
        block_type,
    }
}

/// `LEN` and `NLEN` of a stored block that begins at byte 0 of a raw stream.
///
/// After the three header bits of a stored block "any bits of input up to the next byte
/// boundary are ignored" (RFC 1951 §3.2.4), so for the first block the pair begins at
/// offset one. Both halves are 16-bit little-endian, because `_tr_stored_block` writes them
/// with `put_short`, which is least-significant byte first (`trees.c` L143-L147).
fn stored_block_lengths(stream: &[u8]) -> (u16, u16) {
    assert_eq!(
        first_block_header(stream).block_type,
        STORED_BLOCK,
        "stored_block_lengths requires a stored first block"
    );
    assert!(
        stream.len() >= 5,
        "a stored block header is one byte plus the four-byte LEN/NLEN pair"
    );
    let len = u16::from_le_bytes([stream[1], stream[2]]);
    let nlen = u16::from_le_bytes([stream[3], stream[4]]);
    (len, nlen)
}

/// The distance code of a reduced distance, i.e. `d_code` (`deflate.h` L320-L321).
///
/// ```c
/// #define d_code(dist) \
///    ((dist) < 256 ? _dist_code[dist] : _dist_code[256+((dist)>>7)])
/// ```
///
/// The argument is a *reduced* distance -- a match distance minus one -- because that is
/// what `_tr_tally` stores and what `compress_block` looks up (`trees.c` L930-L936). The
/// fold at 256 is why `_dist_code` is 512 entries long (`trees.c` L81): distances of 256 and above share a
/// code in blocks of 128, so the top 256 slots index `dist >> 7` rather than `dist`.
///
/// Restated here rather than imported because the C macro has no `pub` counterpart -- it is
/// `build::d_code`, which is `pub(crate)`.
fn d_code(reduced_dist: usize) -> usize {
    usize::from(_dist_code[dist_code_index(reduced_dist)])
}

/// The `_dist_code` index [`d_code`] reads for `reduced_dist`.
///
/// Split out so that a test can record which slots the table is actually read through, which
/// is how the two deliberately unreachable ones are proved unreachable rather than merely
/// asserted to be zero.
fn dist_code_index(reduced_dist: usize) -> usize {
    if reduced_dist < 256 {
        reduced_dist
    } else {
        256 + (reduced_dist >> 7)
    }
}

/// Reverses the low `bits` bits of `code`, i.e. `bi_reverse` (`trees.c` L154-L161).
///
/// `send_bits` writes bits least-significant first (`trees.c` L274-L286) while RFC 1951
/// §3.1.1 requires Huffman codes to be packed most-significant bit first, so `gen_codes`
/// stores every code pre-reversed (`trees.c` L227). Undoing that reversal is what turns a
/// table entry back into the canonical code the RFC tabulates, which is how the static
/// trees are checked against the normative text below.
fn bit_reverse(code: u16, bits: u16) -> u16 {
    let mut source = code;
    let mut reversed = 0_u16;
    for _ in 0..bits {
        reversed = (reversed << 1) | (source & 1);
        source >>= 1;
    }
    reversed
}

/// A raw-DEFLATE configuration at the default window and `memLevel`.
fn raw_config(level: i32, strategy: Strategy) -> DeflateConfig {
    DeflateConfig {
        level,
        method: Method::Deflated,
        window_bits: RAW_WINDOW_BITS,
        mem_level: DEF_MEM_LEVEL,
        strategy,
    }
}

/// A raw-DEFLATE configuration with the smallest window and `memLevel`.
///
/// # When this may be substituted for [`raw_config`], and why
///
/// `windowBits` and `memLevel` govern *match finding* and the size of the symbol buffer:
/// the former bounds how far back `longest_match` may look, the latter sets `lit_bufsize`
/// and therefore how many symbols accumulate before a block is flushed
/// (`deflate.c` L455-L470 and L464). Neither reaches the Huffman coder's own decisions --
/// `build_tree` works from the frequency array and the heap, `detect_data_type` reads
/// nothing but frequencies, and `bi_windup` and `put_short` see only the pending buffer.
///
/// So every property in this file that concerns *tree construction*, *classification* or
/// *bit emission* holds at either size, while the properties that concern the block-type
/// *decision* do not: that decision compares a coded cost against a stored cost, and a
/// smaller `lit_bufsize` flushes smaller blocks whose economics differ. Tests of the first
/// kind therefore use this configuration and tests of the second kind use [`raw_config`],
/// and the substitution is pinned rather than assumed --
/// [`the_classification_is_independent_of_window_size_and_mem_level`] and
/// [`the_forced_two_codes_path_survives_the_default_window_too`] are those pins.
///
/// # Why it matters
///
/// A state at `windowBits` 15 and `memLevel` 8 allocates roughly 256 `KiB` -- 64 `KiB` of
/// window, 64 `KiB` of `prev`, 64 `KiB` of `head` and 64 `KiB` of pending buffer -- and
/// `GlobalAllocator` *writes* every byte of it, because a safe-Rust allocation cannot hand
/// back uninitialised memory. At this size the same state is under 4 `KiB`. Natively the
/// difference is invisible; under Miri, where each of those bytes is interpreted and carries
/// provenance, it is the difference between a suite that runs in seconds and one that does
/// not finish. AAP §0.7.1(d) requires every sweep here to be bounded, and this is how the
/// high-cardinality ones are bounded.
fn small_raw_config(level: i32, strategy: Strategy) -> DeflateConfig {
    DeflateConfig {
        level,
        method: Method::Deflated,
        window_bits: SMALL_RAW_WINDOW_BITS,
        mem_level: MIN_MEM_LEVEL,
        strategy,
    }
}

/// What one whole compression produced: the bytes, and the `data_type` the driver
/// reported.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Compressed {
    /// The complete DEFLATE stream.
    bytes: Vec<u8>,
    /// `z_stream.data_type` after the call: `Z_BINARY`, `Z_TEXT` or `Z_UNKNOWN`
    /// (`zlib.h` L207-L210).
    data_type: i32,
}

/// Compresses `input` in a single `Z_FINISH` call.
///
/// The call sequence is C's, in C's order: `deflateInit2_`, then `deflateReset`, then
/// `deflate`, then `deflateEnd`. The reset is **not optional** -- `deflateInit2_` finishes
/// by calling it (`deflate.c` L532), and it is what puts `Z_UNKNOWN` in the caller's
/// `data_type`. Skipping it leaves `data_type` at the zero a fresh `z_stream` carries,
/// which is `Z_BINARY`, and `_tr_flush_block` only ever *overwrites* `Z_UNKNOWN`
/// (`trees.c` L1005-L1007) -- so the classification would silently never run.
///
/// One call with `Z_FINISH` and a buffer of `deflateBound` bytes must always complete: with
/// that much room "deflate is guaranteed to return `Z_STREAM_END`" (`zlib.h` L338-L342, and the
/// declaration at L768). The assertion below is that contract.
fn compress_whole(input: &[u8], config: DeflateConfig) -> Compressed {
    let mut state = deflate_init2(config, GlobalAllocator).expect("deflate state allocation");
    let reset = deflate_reset(&mut state);

    // `deflateBound` is the caller's obligation, not a suggestion: a smaller buffer would
    // make `deflate` return `Z_OK` with work outstanding rather than `Z_STREAM_END`. The
    // slack is for the tiny wrapped-container cases, where the bound is exact.
    let mut out = vec![0_u8; deflate_bound_z(Some(&state), input.len()) + 64];

    let (produced, data_type) = {
        let mut stream = DeflateStream::new(input, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "a one-call Z_FINISH within deflateBound must finish the stream"
        );
        (stream.next_out, stream.data_type)
    };

    // `Z_OK` rather than `Z_DATA_ERROR`, because `Z_FINISH` left the state in
    // `FINISH_STATE` and not `BUSY_STATE` (`deflate.c` L1309).
    assert_eq!(
        deflate_end(state),
        ReturnCode::OK,
        "deflateEnd after Z_STREAM_END reports success"
    );

    out.truncate(produced);
    Compressed {
        bytes: out,
        data_type,
    }
}

/// [`compress_whole`] for a raw stream at the default window and `memLevel`.
fn compress_raw(input: &[u8], level: i32, strategy: Strategy) -> Compressed {
    compress_whole(input, raw_config(level, strategy))
}

/// Decompresses a complete raw DEFLATE stream at the default window size.
fn inflate_raw(stream: &[u8], expected_len: usize) -> Vec<u8> {
    inflate_raw_with(stream, expected_len, RAW_WINDOW_BITS)
}

/// Decompresses a complete raw DEFLATE stream and asserts it ends where it should.
///
/// `expected_len` sizes the output buffer; the surplus is what makes an over-long result
/// visible as a length mismatch instead of a `Z_BUF_ERROR`.
///
/// `window_bits` must be at least the encoder's: `zlib.h` L870-L873 requires it to be
/// "greater than or equal to the `windowBits` value provided to `deflateInit2()` while
/// compressing", on pain of `Z_DATA_ERROR`. Matching it exactly is
/// what keeps a small-configuration round trip small on *both* sides: an inflate state at
/// `windowBits` 15 allocates a 32 `KiB` window whatever the stream needs.
fn inflate_raw_with(stream: &[u8], expected_len: usize, window_bits: i32) -> Vec<u8> {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("inflate state allocation");
    let reset = inflate_reset(&mut state);
    let mut out = vec![0_u8; expected_len + 64];

    let produced = {
        let mut view = InflateStream::new(stream, &mut out);
        view.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut view, Z_FINISH),
            ReturnCode::STREAM_END,
            "a complete raw stream must inflate to Z_STREAM_END"
        );
        view.next_out
    };
    assert_eq!(inflate_end(state), ReturnCode::OK);

    out.truncate(produced);
    out
}

/// Decompresses as much of `fragment` as decodes, without requiring a final block.
///
/// This is what a `Z_SYNC_FLUSH`-terminated prefix needs: it is a well-formed sequence of
/// non-final blocks, so `inflate` returns `Z_OK` with everything flushed rather than
/// `Z_STREAM_END`. `zlib.h` L290-L294 is the contract being relied on -- after a sync flush
/// "the decompressor can get all input data available so far".
fn inflate_raw_fragment(fragment: &[u8], room: usize, window_bits: i32) -> Vec<u8> {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("inflate state allocation");
    let reset = inflate_reset(&mut state);
    let mut out = vec![0_u8; room + 64];

    let produced = {
        let mut view = InflateStream::new(fragment, &mut out);
        view.apply_reset(reset);
        let code = inflate(&mut state, &mut view, Z_NO_FLUSH);
        assert!(
            code == ReturnCode::OK || code == ReturnCode::STREAM_END,
            "a flush-terminated fragment must decode cleanly, got {code:?}"
        );
        view.next_out
    };
    assert_eq!(inflate_end(state), ReturnCode::OK);

    out.truncate(produced);
    out
}

/// Compresses then decompresses `input` under `config`, asserting exact recovery, and
/// returns the compressed bytes so the caller can also assert on them.
///
/// Every block-type assertion in this file is paired with one of these: a header claim about
/// a stream that does not decode would prove nothing.
///
/// The decoder is given the encoder's own `windowBits`, which is both correct -- no distance
/// can exceed it -- and, for [`small_raw_config`], what keeps the decode side as cheap as the
/// encode side.
fn round_trip(input: &[u8], config: DeflateConfig, context: &str) -> Compressed {
    let compressed = compress_whole(input, config);
    let recovered = inflate_raw_with(&compressed.bytes, input.len(), config.window_bits);
    assert_eq!(
        recovered, input,
        "{context}: raw round trip must recover the input exactly"
    );
    compressed
}

/// [`round_trip`] at the default window and `memLevel`.
fn round_trip_raw(input: &[u8], level: i32, strategy: Strategy, context: &str) -> Compressed {
    round_trip(input, raw_config(level, strategy), context)
}

/// [`round_trip`] at the smallest window and `memLevel`; see [`small_raw_config`] for when
/// that substitution is sound.
fn round_trip_small(input: &[u8], level: i32, strategy: Strategy, context: &str) -> Compressed {
    round_trip(input, small_raw_config(level, strategy), context)
}

/// The `data_type` reported for `payload`, computed with the cheap small-window state.
///
/// `detect_data_type` reads nothing but the `dyn_ltree` frequencies of the block
/// (`trees.c` L964-L991), so neither `windowBits` nor `memLevel` can affect its verdict --
/// which is what makes [`small_raw_config`] a valid substitution in the 256-byte sweep and
/// not merely a faster one. A test below checks that substitution directly rather than
/// asserting it here.
fn data_type_of(payload: &[u8]) -> i32 {
    compress_whole(payload, small_raw_config(6, Strategy::Default)).data_type
}

// =======================================================================================
// The six public tables
//
// These are the only items in `src/trees/**` an integration test can name, and they are
// `pub` precisely so that they can be checked as data. Every length below is written as
// the *expression* `trees.h` declares the array with rather than as a bare literal,
// because the expression is the claim: `static_ltree[L_CODES + 2]` is a statement about
// why there are 288 entries, whereas `288` is a statement about nothing.
// =======================================================================================

/// Every table has the length its C declaration gives it.
///
/// The declarations, all from `trees.h`: `static_ltree[L_CODES+2]` (L3),
/// `static_dtree[D_CODES]` (L64), `_dist_code[DIST_CODE_LEN]` (L73),
/// `_length_code[MAX_MATCH-MIN_MATCH+1]` (L102), `base_length[LENGTH_CODES]` (L118) and
/// `base_dist[D_CODES]` (L123).
#[test]
fn every_table_has_the_length_its_c_declaration_gives_it() {
    // `L_CODES` is `LITERALS + 1 + LENGTH_CODES` (`deflate.h` L40), so the arithmetic is
    // checked as well as the length: 256 + 1 + 29 = 286, plus the two surplus entries.
    assert_eq!(L_CODES, LITERALS + 1 + LENGTH_CODES);
    assert_eq!(static_ltree.len(), L_CODES + 2);

    assert_eq!(static_dtree.len(), D_CODES);
    assert_eq!(_dist_code.len(), DIST_CODE_LEN);
    assert_eq!(_length_code.len(), MAX_MATCH - MIN_MATCH + 1);
    assert_eq!(base_length.len(), LENGTH_CODES);
    assert_eq!(base_dist.len(), D_CODES);
}

/// The two surplus `static_ltree` entries are real codes, and they are the reason the
/// table is 288 long rather than 286.
///
/// `L_CODES` is 286, so symbols 286 and 287 can never occur in a stream. `tr_static_init`
/// includes them in the construction anyway (`trees.c` L353-L360), because a canonical
/// Huffman tree needs its longest code to be all ones. Dropping them would change every
/// 9-bit code in the table, so they are transcribed and they carry real lengths.
#[test]
fn the_two_surplus_static_ltree_entries_are_present_and_coded() {
    assert_eq!(static_ltree.len() - L_CODES, 2);
    for symbol in [L_CODES, L_CODES + 1] {
        assert_ne!(
            static_ltree[symbol].len(),
            0,
            "surplus symbol {symbol} must carry a code length"
        );
    }
}

/// The first eight and the last `static_ltree` entries, transcribed from `trees.h`.
///
/// `trees.h` L4-L5 opens with `{{ 12},{  8}}, {{140},{  8}}, {{ 76},{  8}}, ...`, which is
/// `fc` then `dl` -- for a static tree, the bit string then its width. The last entry is
/// `{{227},{  8}}` (L61). `CtData` flattens C's two anonymous unions into two `u16` fields
/// and exposes them through the four C macro names, so the accessors here are
/// [`CtData::code`] (C's `Code`) and [`CtData::len`] (C's `Len`); [`CtData::freq`] and
/// [`CtData::dad`] read the same two words under their other names.
///
/// These spot values are the cheap tripwire. The structural test below is what actually
/// validates all 288 entries.
#[test]
fn the_transcribed_static_ltree_entries_match_trees_h() {
    /// `trees.h` L4-L5, as `(code, len)` pairs.
    const FIRST_EIGHT: [(u16, u16); 8] = [
        (12, 8),
        (140, 8),
        (76, 8),
        (204, 8),
        (44, 8),
        (172, 8),
        (108, 8),
        (236, 8),
    ];

    for (symbol, &(code, len)) in FIRST_EIGHT.iter().enumerate() {
        let entry = static_ltree[symbol];
        assert_eq!(entry.code(), code, "static_ltree[{symbol}] code");
        assert_eq!(entry.len(), len, "static_ltree[{symbol}] len");
        // The union spelling: `Code` and `Freq` are one word, `Len` and `Dad` the other.
        assert_eq!(entry.freq(), code);
        assert_eq!(entry.dad(), len);
        // And the entry is exactly what `CtData::new` builds from the header's two halves.
        assert_eq!(entry, CtData::new(code, len));
    }

    let last = static_ltree[static_ltree.len() - 1];
    assert_eq!(
        (last.code(), last.len()),
        (227, 8),
        "static_ltree final entry"
    );
}

/// Every one of the 288 `static_ltree` entries is the canonical fixed code RFC 1951
/// §3.2.6 tabulates, reversed.
///
/// This is the strongest assertion in the file, and it is derived from the normative text
/// rather than from a transcription. The RFC gives the fixed literal/length alphabet as
///
/// ```text
///   Lit Value    Bits        Codes
///     0 - 143     8          00110000 through 10111111
///   144 - 255     9          110010000 through 111111111
///   256 - 279     7          0000000 through 0010111
///   280 - 287     8          11000000 through 11000111
/// ```
///
/// so the canonical code of a symbol is its run's first code plus its offset within the
/// run -- four contiguous ascending ladders, with no gaps and no exceptions. The table
/// stores each code pre-reversed, because `send_bits` emits least-significant bit first
/// (`trees.c` L274-L286) while §3.1.1 packs Huffman codes most-significant bit first; the
/// reversal is `bi_reverse` as applied by `gen_codes` (`trees.c` L154-L161 and L227).
/// Undoing it with [`bit_reverse`] therefore checks the width, the code and the bit order
/// of all 288 entries at once, and pins `bi_reverse` -- which is `pub(crate)` and
/// unreachable -- through the data it produced.
#[test]
fn the_whole_static_ltree_is_the_rfc_1951_fixed_alphabet_reversed() {
    for (symbol, entry) in static_ltree.iter().enumerate() {
        let (bits, canonical) = if symbol <= FIXED_8_BIT_LITERAL_MAX {
            (8, FIXED_8_BIT_FIRST_CODE + u16::try_from(symbol).unwrap())
        } else if symbol <= FIXED_9_BIT_LITERAL_MAX {
            let offset = u16::try_from(symbol - FIXED_8_BIT_LITERAL_MAX - 1).unwrap();
            (9, FIXED_9_BIT_FIRST_CODE + offset)
        } else if symbol <= FIXED_7_BIT_SYMBOL_MAX {
            let offset = u16::try_from(symbol - FIXED_9_BIT_LITERAL_MAX - 1).unwrap();
            (7, FIXED_7_BIT_FIRST_CODE + offset)
        } else {
            let offset = u16::try_from(symbol - FIXED_7_BIT_SYMBOL_MAX - 1).unwrap();
            (8, FIXED_TAIL_FIRST_CODE + offset)
        };

        assert_eq!(
            entry.len(),
            bits,
            "static_ltree[{symbol}] width (RFC 1951 3.2.6)"
        );
        assert_eq!(
            bit_reverse(entry.code(), bits),
            canonical,
            "static_ltree[{symbol}] canonical code (RFC 1951 3.2.6)"
        );
    }

    // The run boundaries, restated as counts so that a table shifted by one whole run --
    // which the per-symbol loop above would report 288 times -- reports once, legibly.
    let widths: Vec<u16> = static_ltree.iter().copied().map(CtData::len).collect();
    assert_eq!(widths.iter().filter(|&&w| w == 7).count(), 24);
    assert_eq!(widths.iter().filter(|&&w| w == 8).count(), 144 + 8);
    assert_eq!(widths.iter().filter(|&&w| w == 9).count(), 112);
}

/// Every `static_dtree` entry is a 5-bit code, and it is its own index reversed.
///
/// RFC 1951 §3.2.6: "Distance codes 0-31 are represented by (fixed-length) 5-bit codes",
/// so the canonical code for distance code `d` is simply `d` in five bits -- a flat
/// alphabet, not a Huffman one. `trees.h` L65-L71 stores those codes reversed for the same
/// reason `static_ltree` does, which is why the table opens `0, 16, 8, 24, 4, 20` rather
/// than `0, 1, 2, 3, 4, 5`: those are 0, 1, 2, 3, 4, 5 with their five bits turned around.
///
/// Only 30 of the 32 codes are present, because `D_CODES` is 30; §3.2.6 notes that codes
/// 30 and 31 "will never actually occur in the compressed data".
#[test]
fn the_whole_static_dtree_is_the_rfc_1951_flat_5_bit_alphabet_reversed() {
    for (code, entry) in static_dtree.iter().enumerate() {
        assert_eq!(
            entry.len(),
            FIXED_DISTANCE_CODE_BITS,
            "static_dtree[{code}] width (RFC 1951 3.2.6)"
        );
        assert_eq!(
            bit_reverse(entry.code(), FIXED_DISTANCE_CODE_BITS),
            u16::try_from(code).unwrap(),
            "static_dtree[{code}] canonical code (RFC 1951 3.2.6)"
        );
    }

    // The documented opening, as a transcription tripwire beside the derivation above.
    let opening: Vec<u16> = static_dtree
        .iter()
        .copied()
        .take(6)
        .map(CtData::code)
        .collect();
    assert_eq!(opening, vec![0, 16, 8, 24, 4, 20]);
}

/// `_dist_code` holds only real distance codes, and its two unused slots are zero.
///
/// `d_code` reads the table two ways -- `_dist_code[dist]` for a reduced distance below
/// 256 and `_dist_code[256 + (dist >> 7)]` above (`deflate.h` L320-L321) -- so indices 256
/// and 257 correspond to `dist >> 7` of 0 and 1, i.e. to distances the *first* branch
/// already handles. They are unreachable by construction, and `trees.h` L73 leaves them
/// zero.
#[test]
fn the_two_unreachable_dist_code_slots_are_zero_and_the_rest_are_real_codes() {
    assert_eq!(_dist_code[256], 0, "_dist_code[256] is unused");
    assert_eq!(_dist_code[257], 0, "_dist_code[257] is unused");

    for (index, &code) in _dist_code.iter().enumerate() {
        assert!(
            usize::from(code) < D_CODES,
            "_dist_code[{index}] = {code} is not a distance code (D_CODES = {D_CODES})"
        );
    }

    // Both halves are exercised by the tiling test below; assert here only that the two
    // unused slots are the *only* ones a code of zero can come from other than index 0,
    // which is the shortest description of the table's shape.
    let zeroes: Vec<usize> = _dist_code
        .iter()
        .enumerate()
        .filter(|&(_, &code)| code == 0)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(zeroes, vec![0, 256, 257]);
}

/// `_dist_code` and `base_dist` tile the whole reduced-distance range exactly once.
///
/// For every reduced distance -- a match distance minus one, so `0 ..= 32767` per RFC 1951
/// §3.2.5 -- the code `d_code` selects must be the one whose `base_dist` interval contains
/// it. `base_dist[n]` is the smallest reduced distance mapping to code `n` (`trees.h`
/// L123), and `compress_block` subtracts it to obtain the residue the extra bits carry
/// (`trees.c` L936), so a table off by one anywhere makes every distance in the affected
/// interval decode to the wrong place.
///
/// Walking all 32768 distances also touches every reachable `_dist_code` entry: 0 to 255
/// through the direct branch and 258 to 511 through the folded one, leaving only the two
/// unused slots above. Integer work only, so it is cheap enough for Miri.
#[test]
fn dist_code_and_base_dist_tile_the_reduced_distance_range() {
    // `base_dist` must be strictly increasing from zero for the intervals to be
    // well-formed at all -- that is the geometric ladder of RFC 1951 3.2.5, doubling every
    // second code so that 30 codes plus at most 13 extra bits address a 32 KiB window.
    assert_eq!(base_dist[0], 0, "base_dist starts at zero");
    for window in base_dist.windows(2) {
        assert!(
            window[0] < window[1],
            "base_dist must be strictly increasing: {window:?}"
        );
    }
    assert_eq!(
        base_dist[D_CODES - 1],
        24_576,
        "the last distance interval opens at 24576 (trees.h L123-L127)"
    );

    let mut touched = [false; DIST_CODE_LEN];
    for reduced in 0..=MAX_REDUCED_DIST {
        let code = d_code(reduced);
        touched[dist_code_index(reduced)] = true;

        let low = usize::try_from(base_dist[code]).unwrap();
        let high = if code + 1 < D_CODES {
            usize::try_from(base_dist[code + 1]).unwrap() - 1
        } else {
            MAX_REDUCED_DIST
        };
        assert!(
            low <= reduced && reduced <= high,
            "reduced distance {reduced} mapped to code {code}, whose interval is {low}..={high}"
        );
    }

    // Exactly the two documented slots are never read.
    let untouched: Vec<usize> = touched
        .iter()
        .enumerate()
        .filter(|&(_, &seen)| !seen)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        untouched,
        vec![256, 257],
        "only the two documented slots may be unreachable"
    );
}

/// `_length_code` holds only real length codes, and it is anchored at `MIN_MATCH`.
///
/// The array is declared `_length_code[MAX_MATCH-MIN_MATCH+1]` (`trees.h` L102) and is
/// indexed by the *normalised* length `length - MIN_MATCH`, which is exactly what
/// `_tr_tally` stores (`deflate.h` L366 and L372). So index 0 describes a match of
/// `MIN_MATCH` = 3 bytes and must be code 0, whose interval opens at `base_length[0]` = 0;
/// and the last index, `MAX_MATCH - MIN_MATCH` = 255, describes a match of 258 bytes and
/// must be code 28 -- RFC 1951's length code 285, the one that stands for 258 alone.
#[test]
fn length_code_is_anchored_at_min_match_and_holds_only_real_codes() {
    assert_eq!(MIN_MATCH, 3, "zutil.h L92");
    assert_eq!(MAX_MATCH, 258, "zutil.h L93");

    assert_eq!(
        _length_code[MIN_MATCH - MIN_MATCH],
        0,
        "a MIN_MATCH-byte match is length code 0"
    );
    assert_eq!(base_length[0], 0, "and code 0's interval opens at zero");
    assert_eq!(
        _length_code[MAX_MATCH - MIN_MATCH],
        u8::try_from(LENGTH_CODES - 1).unwrap(),
        "a MAX_MATCH-byte match is the last length code"
    );

    for (index, &code) in _length_code.iter().enumerate() {
        assert!(
            usize::from(code) < LENGTH_CODES,
            "_length_code[{index}] = {code} is not a length code (LENGTH_CODES = {LENGTH_CODES})"
        );
    }
}

/// `base_length` opens each interval, ends in the anomalous zero, and is otherwise
/// ascending.
///
/// `trees.h` L118-L121 ends the array with `224, 0`. The trailing zero is not a
/// transcription slip: length code 28 -- RFC 1951's 285 -- carries no extra bits and
/// stands for the single length 258, so it has no interval to open and `base_length` holds
/// nothing meaningful for it. Every other element is the smallest normalised length that
/// maps to its code, and those ascend strictly.
#[test]
fn base_length_ascends_and_ends_in_the_anomalous_zero() {
    let last = LENGTH_CODES - 1;
    assert_eq!(
        base_length[last], 0,
        "base_length's final element is zero (trees.h L118-L121)"
    );
    assert_eq!(base_length[last - 1], 224, "and the one before it is 224");

    for pair in 0..last - 1 {
        assert!(
            base_length[pair] < base_length[pair + 1],
            "base_length must ascend strictly up to the final element: \
             [{pair}] = {} is not below [{}] = {}",
            base_length[pair],
            pair + 1,
            base_length[pair + 1]
        );
    }
}

/// `_length_code` and `base_length` tile the whole normalised-length range exactly once.
///
/// Cross-table consistency is what catches a table transcribed with an off-by-one shift,
/// which spot checks miss entirely: for each of the 29 length codes, *every* normalised
/// length in its interval must map back to that code through `_length_code`.
///
/// The intervals come from `base_length` itself, with the two documented irregularities at
/// the top handled explicitly, because `base_length[28]` is zero and so cannot serve as
/// code 27's upper bound:
///
/// | Code | Interval of normalised lengths |
/// |---|---|
/// | 0 ..= 26 | `base_length[c] ..= base_length[c+1] - 1` |
/// | 27 | `base_length[27] ..= 254` -- 31 lengths, not 32, because 255 is taken |
/// | 28 | `255` alone -- RFC 1951's length code 285, meaning 258 bytes |
#[test]
fn length_code_and_base_length_tile_the_normalised_length_range() {
    /// Code 28, RFC 1951's 285: the one that stands for `MAX_MATCH` alone.
    const LAST_CODE: usize = LENGTH_CODES - 1;
    /// Code 27, whose interval is one short because 255 belongs to [`LAST_CODE`].
    const PENULTIMATE_CODE: usize = LENGTH_CODES - 2;
    /// The largest normalised length, `MAX_MATCH - MIN_MATCH` = 255.
    const LAST_NORMALISED: usize = MAX_MATCH - MIN_MATCH;

    /// The inclusive interval of normalised lengths belonging to `code`.
    fn interval(code: usize) -> (usize, usize) {
        let low = usize::try_from(base_length[code]).unwrap();
        match code {
            LAST_CODE => (LAST_NORMALISED, LAST_NORMALISED),
            PENULTIMATE_CODE => (low, LAST_NORMALISED - 1),
            _ => (low, usize::try_from(base_length[code + 1]).unwrap() - 1),
        }
    }

    let mut covered = 0_usize;
    for code in 0..LENGTH_CODES {
        let (low, high) = interval(code);
        assert!(low <= high, "code {code} has an empty interval");
        for (offset, &mapped) in _length_code[low..=high].iter().enumerate() {
            let normalised = low + offset;
            assert_eq!(
                usize::from(mapped),
                code,
                "normalised length {normalised} must map to length code {code}"
            );
        }
        covered += high - low + 1;
    }

    // The intervals are contiguous and non-overlapping precisely when their sizes sum to
    // the table's length, given that each one was verified to be non-empty and that every
    // index it claims was verified to hold its code.
    assert_eq!(
        covered,
        _length_code.len(),
        "the 29 intervals must tile 0..={} exactly once",
        MAX_MATCH - MIN_MATCH
    );
}

// =======================================================================================
// Decision point 8 -- block-type selection
//
// `_tr_flush_block` chooses between a stored block, a fixed-tree block and a dynamic-tree
// block, and writes the choice into the two `BTYPE` bits every decoder reads
// (`trees.c` L1026-L1074):
//
//     opt_lenb = (s->opt_len + 3 + 7) >> 3;
//     static_lenb = (s->static_len + 3 + 7) >> 3;
//     if (static_lenb <= opt_lenb || s->strategy == Z_FIXED)  opt_lenb = static_lenb;
//     ... else (level == 0):  opt_lenb = static_lenb = stored_len + 5;
//
//     if (stored_len + 4 <= opt_lenb && buf != NULL)  -> _tr_stored_block   (STORED_BLOCK)
//     else if (static_lenb == opt_lenb)               -> STATIC_TREES
//     else                                            -> DYN_TREES
//
// Those are integer-truncating comparisons on unsigned bit counts. AAP §0.6.2 #8 requires
// identical integer arithmetic -- no floating point, no algebraic reordering -- so the
// tests below assert the *outcome* on inputs chosen to land on each branch, which is the
// only part of the computation an external caller can see. AAP §0.8.4 additionally
// prohibits a "better" selection rule: if a future change makes any of these blocks
// smaller by choosing differently, these assertions are what must fail.
// =======================================================================================

/// Incompressible input takes the stored branch, at every level.
///
/// With no matches to find and a near-flat symbol distribution, the coded forms cost more
/// than the bytes themselves, so `stored_len + 4 <= opt_lenb` holds and the block is
/// stored. The `+ 4` is the `LEN`/`NLEN` pair, as the source comment says: "4: two words
/// for the lengths" (`trees.c` L1047-L1048).
///
/// The size relation is asserted too, because it is the whole reason the branch is taken:
/// a stored block of `n` bytes costs `n + 5` -- one header byte after alignment plus the
/// four length bytes -- and nothing the coder could do would beat that here.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn incompressible_input_selects_a_stored_block() {
    let payload = corpus::incompressible(1024);

    for level in LEVELS {
        let compressed = round_trip_raw(&payload, level, Strategy::Default, "incompressible");
        let header = first_block_header(&compressed.bytes);

        assert_eq!(
            header.block_type, STORED_BLOCK,
            "incompressible input at level {level} must be stored"
        );
        assert!(header.last, "a one-call Z_FINISH emits a final block");
        assert_eq!(
            compressed.bytes.len(),
            payload.len() + 5,
            "a stored block costs its bytes plus a header byte and the LEN/NLEN pair"
        );

        let (len, nlen) = stored_block_lengths(&compressed.bytes);
        assert_eq!(
            usize::from(len),
            payload.len(),
            "LEN counts the block's bytes"
        );
        assert_eq!(nlen, !len, "NLEN is the one's complement of LEN");
    }
}

/// A very short payload takes the fixed-tree branch.
///
/// `static_lenb <= opt_lenb` because transmitting a dynamic tree costs far more than the
/// handful of symbols it would save on: `build_bl_tree` adds the tree's own transmission
/// cost to `opt_len` (`trees.c` L821), and for a payload this size that cost dominates.
/// `corpus::HELLO` is the fixture the C suite compresses -- `"hello, hello!"` plus its
/// terminating NUL (`test/example.c` L35) -- and it lands here at every level above zero.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn a_short_payload_selects_a_static_tree_block() {
    for level in [1, 6, 9] {
        for input in [corpus::SINGLE_BYTE, corpus::HELLO] {
            let compressed = round_trip_raw(input, level, Strategy::Default, "short payload");
            assert_eq!(
                first_block_header(&compressed.bytes).block_type,
                STATIC_TREES,
                "a {}-byte payload at level {level} must use the fixed trees",
                input.len()
            );
        }
    }
}

/// Larger structured input takes the dynamic-tree branch.
///
/// Here the dynamic trees genuinely win: the symbol distribution is skewed enough that the
/// bits saved on the body outweigh the cost of transmitting the trees, so `static_lenb`
/// stays above `opt_lenb`, the `if` at `trees.c` L1035 does not fire, and the final `else`
/// at L1064 selects `DYN_TREES`.
///
/// Level 0 is excluded by construction rather than by omission: it cannot reach this
/// branch, because its `else` arm forces a stored block (`trees.c` L1039-L1042). That case
/// has its own test below.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn larger_structured_input_selects_a_dynamic_tree_block() {
    let repetitive = corpus::repetitive(4096);
    let text = corpus::text();

    for level in [1, 6, 9] {
        for (name, input) in [("repetitive", &repetitive), ("text", &text)] {
            let compressed = round_trip_raw(input, level, Strategy::Default, name);
            assert_eq!(
                first_block_header(&compressed.bytes).block_type,
                DYN_TREES,
                "{name} at level {level} must build its own trees"
            );
            assert!(
                compressed.bytes.len() < input.len(),
                "{name} at level {level} must actually compress"
            );
        }
    }
}

/// `Z_FIXED` forces the fixed trees even when the dynamic ones would be smaller.
///
/// This is the `|| s->strategy == Z_FIXED` disjunct of `trees.c` L1035. The inputs are the
/// two that [`larger_structured_input_selects_a_dynamic_tree_block`] shows choose
/// `DYN_TREES` under `Z_DEFAULT_STRATEGY`, so the strategy is demonstrably what changes the
/// outcome -- and the resulting stream is demonstrably larger, which is the point:
/// `Z_FIXED` exists to keep a decoder from having to read a dynamic tree, and it pays for
/// that in bytes (`zlib.h` L203).
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn z_fixed_forces_the_static_trees_where_dynamic_would_win() {
    let repetitive = corpus::repetitive(4096);
    let text = corpus::text();

    for level in [1, 6, 9] {
        for (name, input) in [("repetitive", &repetitive), ("text", &text)] {
            let dynamic = round_trip_raw(input, level, Strategy::Default, name);
            let fixed = round_trip_raw(input, level, Strategy::Fixed, name);

            assert_eq!(
                first_block_header(&dynamic.bytes).block_type,
                DYN_TREES,
                "{name} at level {level} under Z_DEFAULT_STRATEGY"
            );
            assert_eq!(
                first_block_header(&fixed.bytes).block_type,
                STATIC_TREES,
                "{name} at level {level} under Z_FIXED must use the fixed trees"
            );
            assert!(
                fixed.bytes.len() > dynamic.bytes.len(),
                "{name} at level {level}: Z_FIXED gives up bytes for a fixed tree, so the \
                 fixed stream must be the larger one ({} vs {})",
                fixed.bytes.len(),
                dynamic.bytes.len()
            );
        }
    }
}

/// Level 0 always stores, at every strategy, and reports the stored form exactly.
///
/// This is the `else` arm of the block decision: with `s->level == 0` no trees are built at
/// all, `opt_lenb` and `static_lenb` are both assigned `stored_len + 5`, and the first
/// predicate -- `stored_len + 4 <= opt_lenb` -- is then true by construction
/// (`trees.c` L1039-L1048). The strategy is irrelevant here, which is itself worth
/// asserting: `Z_FIXED` cannot override a forced stored block, because the `if` that reads
/// the strategy lives in the branch level 0 does not take.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn level_zero_always_stores_whatever_the_strategy() {
    let payloads = [
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(600)),
        ("text", corpus::text()),
    ];

    for (name, payload) in &payloads {
        for strategy in Strategy::ALL {
            let compressed = round_trip_raw(payload, 0, strategy, name);
            assert_eq!(
                first_block_header(&compressed.bytes).block_type,
                STORED_BLOCK,
                "{name} at level 0 under {} must be stored",
                strategy.c_name()
            );
            assert_eq!(
                compressed.bytes.len(),
                payload.len() + 5,
                "{name} at level 0 under {}: stored cost is n + 5",
                strategy.c_name()
            );

            let (len, nlen) = stored_block_lengths(&compressed.bytes);
            assert_eq!(usize::from(len), payload.len());
            assert_eq!(nlen, !len);
        }
    }
}

/// Empty input produces a well-formed final block at every level, and decodes to nothing.
///
/// Two different branches meet here, and both matter:
///
/// * **At level 0** the `else` arm gives `opt_lenb = static_lenb = stored_len + 5` with
///   `stored_len` zero, so an empty *stored* block is emitted: the five bytes
///   `01 00 00 ff ff` -- header, then `LEN` = 0 and `NLEN` = `0xffff` (RFC 1951 §3.2.4).
/// * **Above level 0** trees are built over a block with no symbols but for the
///   end-of-block code, `static_lenb <= opt_lenb` holds, and the result is a two-byte
///   fixed-tree block. Reaching that at all requires the forced-two-codes path of
///   `trees.c` L655-L661 to survive a tree with nothing in it; see the degenerate-tree
///   section.
///
/// `zlib.h` L338-L342 is the contract this checks: `Z_FINISH` "can be used in the first
/// deflate call after deflateInit if all the compression is to be done in a single step", and
/// with `deflateBound` bytes of room "deflate is guaranteed to return `Z_STREAM_END`" -- an
/// empty input included.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The block-type
/// decision compares a coded cost against a stored cost, and those economics depend on
/// `lit_bufsize` -- so unlike the tree-construction properties this test cannot be moved to
/// [`small_raw_config`], and a default-size state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `all_three_block_types_are_reachable_at_one_level_and_strategy`
/// keeps all three branches covered under Miri in three compressions.
#[test]
#[cfg_attr(miri, ignore)]
fn empty_input_produces_a_well_formed_final_block_at_every_level() {
    for level in LEVELS {
        for strategy in Strategy::ALL {
            let compressed = round_trip_raw(corpus::EMPTY, level, strategy, "empty");
            let header = first_block_header(&compressed.bytes);

            assert!(
                header.last,
                "empty input at level {level} must emit a final block"
            );

            if level == 0 {
                assert_eq!(header.block_type, STORED_BLOCK);
                assert_eq!(
                    compressed.bytes,
                    vec![0x01, 0x00, 0x00, 0xff, 0xff],
                    "an empty stored block is the header byte plus LEN = 0 and NLEN = 0xffff"
                );
                let (len, nlen) = stored_block_lengths(&compressed.bytes);
                assert_eq!((len, nlen), (0, 0xffff));
            } else {
                assert_eq!(
                    header.block_type, STATIC_TREES,
                    "empty input at level {level} costs least under the fixed trees"
                );
                assert_eq!(
                    compressed.bytes,
                    vec![0x03, 0x00],
                    "the final fixed-tree block over no symbols is two bytes"
                );
            }
        }
    }
}

/// The three branches are genuinely distinct: one corpus reaches each of them.
///
/// A single test that observes all three outcomes side by side is what makes it impossible
/// for a regression to satisfy the individual tests above by collapsing the decision --
/// always storing, say, would pass nothing here.
#[test]
fn all_three_block_types_are_reachable_at_one_level_and_strategy() {
    let observed: Vec<u8> = [
        corpus::incompressible(1024),
        corpus::HELLO.to_vec(),
        corpus::text(),
    ]
    .iter()
    .map(|payload| {
        let compressed = round_trip_raw(payload, 6, Strategy::Default, "branch survey");
        first_block_header(&compressed.bytes).block_type
    })
    .collect();

    assert_eq!(observed, vec![STORED_BLOCK, STATIC_TREES, DYN_TREES]);
}

// =======================================================================================
// Decision point 7 -- forced two codes, and the wrapping-arithmetic trap
//
// ★ DO NOT DELETE ANY TEST IN THIS SECTION. They are a panic detector, not a formality.
//
// `build_tree` refuses to produce a tree with fewer than two codes (`trees.c` L650-L662):
//
//     /* The pkzip format requires that at least one distance code exists,
//      * and that at least one bit should be sent even if there is only one
//      * possible code. So to avoid special checks later on we force at least
//      * two codes of non zero frequency.
//      */
//     while (s->heap_len < 2) {
//         node = s->heap[++(s->heap_len)] = (max_code < 2 ? ++max_code : 0);
//         tree[node].Freq = 1;
//         s->depth[node] = 0;
//         s->opt_len--; if (stree) s->static_len -= stree[node].Len;
//     }
//
// `s->opt_len` and `s->static_len` are **unsigned** (`ulg`), and this loop drives them
// below zero on purpose: the reference relies on the wraparound cancelling against the
// additions `gen_bitlen` makes afterwards (`trees.c` L540-L625). The Rust port must
// therefore use `wrapping_sub` -- `src/trees/build.rs` L1057 and L1064 do. A plain `-=`
// compiles, passes review, and then **panics with "attempt to subtract with overflow" in a
// debug build**, which is precisely the build `cargo test` and
// `cargo +nightly miri test` produce.
//
// The loop runs exactly when a block has fewer than two symbols of non-zero frequency in
// one of its alphabets, and the cheapest ways to arrange that are: no input at all, one
// distinct byte, two distinct bytes, and `Z_HUFFMAN_ONLY`/`Z_RLE`, where the distance
// alphabet is empty or nearly so. Every case below is deliberately tiny, so all of them
// run under Miri -- which is the entire reason Miri is valuable for this subsystem.
// =======================================================================================

/// A single-byte payload compresses, does not panic, and round-trips -- at every level and
/// strategy.
///
/// One distinct symbol means exactly one literal has a non-zero frequency, so the literal
/// alphabet reaches `build_tree` with `heap_len == 1` and the forced-two-codes loop runs
/// once. The distance alphabet reaches it with `heap_len == 0` and the loop runs twice.
#[test]
fn a_single_byte_payload_survives_the_forced_two_codes_path() {
    for level in LEVELS {
        for strategy in Strategy::ALL {
            let compressed = round_trip_small(corpus::SINGLE_BYTE, level, strategy, "single byte");
            assert!(
                !compressed.bytes.is_empty(),
                "level {level} under {} produced no output",
                strategy.c_name()
            );
            assert!(first_block_header(&compressed.bytes).last);
        }
    }
}

/// Empty input compresses, does not panic, and round-trips -- at every level and strategy.
///
/// The extreme of the same case: *both* alphabets arrive with `heap_len == 0`, so the loop
/// runs twice for each, and `opt_len` is decremented four times from a value that counts
/// only the end-of-block symbol. This is the shortest path to the wraparound.
#[test]
fn empty_input_survives_the_forced_two_codes_path() {
    for level in LEVELS {
        for strategy in Strategy::ALL {
            let compressed = round_trip_small(corpus::EMPTY, level, strategy, "empty");
            assert!(
                !compressed.bytes.is_empty(),
                "an empty stream still has a final block"
            );
        }
    }
}

/// Two distinct symbols, and one symbol repeated, both survive.
///
/// `heap_len == 2` for the literal alphabet is the first case that does *not* need the
/// forced loop, so `two_symbols` is the boundary on the safe side; `alternating` and
/// `one_symbol_repeated` keep the distance alphabet degenerate while the literal alphabet
/// is not, which exercises the loop for one descriptor and not the other -- the asymmetric
/// case, where a port that hoisted the arithmetic out of the loop would go wrong.
#[test]
fn two_symbol_and_one_symbol_payloads_survive_the_forced_two_codes_path() {
    let alternating: Vec<u8> = (0..MIRI_SWEEP_LEN)
        .map(|index| if index % 2 == 0 { b'x' } else { b'y' })
        .collect();
    let payloads = [
        ("two_symbols", b"ab".to_vec()),
        ("alternating", alternating),
        ("one_symbol_repeated", corpus::repetitive(MIRI_SWEEP_LEN)),
    ];

    for (name, payload) in &payloads {
        for level in LEVELS {
            for strategy in Strategy::ALL {
                let compressed = round_trip_small(payload, level, strategy, name);
                assert!(
                    !compressed.bytes.is_empty(),
                    "{name} at level {level} under {} produced no output",
                    strategy.c_name()
                );
            }
        }
    }
}

/// `Z_HUFFMAN_ONLY` and `Z_RLE` survive, including on the degenerate payloads.
///
/// `Z_HUFFMAN_ONLY` disables string matching entirely (`zlib.h` L201), so the distance
/// alphabet genuinely has **zero** codes for every payload -- the forced loop runs twice for
/// `d_desc` on every single block, not just on contrived input. `Z_RLE` limits distances to
/// one, which leaves the distance alphabet with a single code and runs the loop once.
///
/// Between them these two strategies make the forced path unavoidable, which is why they
/// are called out separately from the strategy sweep above.
#[test]
fn huffman_only_and_rle_survive_an_empty_distance_alphabet() {
    let payloads = [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(MIRI_SWEEP_LEN)),
        ("text", short_text()),
    ];

    for (name, payload) in &payloads {
        for strategy in [Strategy::HuffmanOnly, Strategy::Rle] {
            for level in [1, 6, 9] {
                let compressed = round_trip_small(payload, level, strategy, name);
                assert!(
                    !compressed.bytes.is_empty(),
                    "{name} at level {level} under {} produced no output",
                    strategy.c_name()
                );
            }
        }
    }
}

/// The whole degenerate corpus survives at the **default** window and `memLevel` too.
///
/// The sweeps above run at [`small_raw_config`] so that they fit inside a Miri run, and this
/// is the pin for that substitution: the same payloads, the same strategies, the same levels,
/// at `windowBits` 15 and `memLevel` 8. If the forced-two-codes path were somehow sensitive
/// to either parameter -- it is not, because `build_tree` works from the frequency array and
/// the heap and never consults the window -- this is where that would show.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** A state at the
/// default size is roughly 256 `KiB`, every byte of which `GlobalAllocator` writes, so the
/// 140-odd states this creates are minutes of native time and hours of interpreted time. The
/// property itself is fully covered under Miri by the four tests above.
#[test]
#[cfg_attr(miri, ignore)]
fn the_forced_two_codes_path_survives_the_default_window_too() {
    let alternating: Vec<u8> = (0..512_u32)
        .map(|index| if index % 2 == 0 { b'x' } else { b'y' })
        .collect();
    let payloads: [(&str, Vec<u8>); 5] = [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("two_symbols", b"ab".to_vec()),
        ("alternating", alternating),
        ("one_symbol_repeated", corpus::repetitive(512)),
    ];

    for (name, payload) in &payloads {
        for level in LEVELS {
            for strategy in Strategy::ALL {
                let compressed = round_trip_raw(payload, level, strategy, name);
                assert!(
                    !compressed.bytes.is_empty(),
                    "{name} at level {level} under {} produced no output",
                    strategy.c_name()
                );
            }
        }
    }
}

/// The degenerate blocks decode under a decoder that has never seen the encoder.
///
/// The round trips above all run through this port's own `inflate`, so a *symmetric* defect
/// -- an encoder that emits a malformed degenerate tree and a decoder that happens to
/// accept it -- would go unnoticed. RFC 1951 §3.2.7 constrains what a dynamic-tree block
/// may contain, and the tightest externally checkable consequence available here is that
/// the same bytes also decode when the stream is fed one byte at a time, which drives the
/// decoder through every one of its resume points rather than letting it consume the block
/// in a single pass.
///
/// This is the substitution the reachability note at the top of the file describes: the
/// property one would like to assert -- that `build_tree` produced exactly two codes -- is
/// unreachable, so its closest observable consequence is asserted instead.
#[test]
fn degenerate_blocks_decode_when_fed_one_byte_at_a_time() {
    let payloads = [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("two_symbols", b"ab".to_vec()),
        ("one_symbol_repeated", corpus::repetitive(64)),
    ];

    for (name, payload) in &payloads {
        for strategy in [Strategy::Default, Strategy::HuffmanOnly, Strategy::Rle] {
            let config = small_raw_config(6, strategy);
            let compressed = compress_whole(payload, config);
            let recovered =
                inflate_byte_at_a_time(&compressed.bytes, payload.len(), config.window_bits);
            assert_eq!(
                &recovered,
                payload,
                "{name} under {} must decode byte by byte",
                strategy.c_name()
            );
        }
    }
}

/// Decompresses a complete raw stream while exposing exactly one new input byte per call.
///
/// The documented way to hand `inflate` more input is to widen the input slice and leave
/// `next_in` where the previous call left it; re-slicing from `next_in` would discard the
/// already-consumed prefix the window still refers to. Each call therefore sees one more
/// byte than the last, and `Z_NO_FLUSH` lets the decoder stop wherever it runs dry.
fn inflate_byte_at_a_time(stream: &[u8], expected_len: usize, window_bits: i32) -> Vec<u8> {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("inflate state allocation");
    let reset = inflate_reset(&mut state);
    let mut out = vec![0_u8; expected_len + 64];

    let produced = {
        // The view is built over the empty prefix and widened one byte at a time.
        let mut view = InflateStream::new(&stream[..0], &mut out);
        view.apply_reset(reset);

        let mut finished = false;
        for exposed in 1..=stream.len() {
            view.input = &stream[..exposed];
            let flush = if exposed == stream.len() {
                Z_FINISH
            } else {
                Z_NO_FLUSH
            };
            match inflate(&mut state, &mut view, flush) {
                ReturnCode::OK | ReturnCode::BUF_ERROR => {}
                ReturnCode::STREAM_END => {
                    finished = true;
                    break;
                }
                other => panic!(
                    "byte-at-a-time inflate failed with {other:?} after {exposed} of {} bytes",
                    stream.len()
                ),
            }
        }
        assert!(
            finished,
            "the stream must reach Z_STREAM_END once every byte has been offered"
        );
        view.next_out
    };
    assert_eq!(inflate_end(state), ReturnCode::OK);

    out.truncate(produced);
    out
}

// =======================================================================================
// `detect_data_type`, observed through `z_stream.data_type`
//
// `detect_data_type` classifies a block as text or binary from its literal frequencies
// (`trees.c` L966-L991):
//
//     unsigned long block_mask = 0xf3ffc07fUL;
//     for (n = 0; n <= 31; n++, block_mask >>= 1)
//         if ((block_mask & 1) && (s->dyn_ltree[n].Freq != 0)) return Z_BINARY;
//     if (s->dyn_ltree[9].Freq || s->dyn_ltree[10].Freq || s->dyn_ltree[13].Freq)
//         return Z_TEXT;
//     for (n = 32; n < LITERALS; n++) if (s->dyn_ltree[n].Freq != 0) return Z_TEXT;
//     return Z_BINARY;
//
// The mask is the whole specification, and it is exactly the three lists of
// `doc/txtvsbin.txt` L40-L45. Reading `0xf3ffc07f` as the source comment does --
// "set bits 0..6, 14..25, and 28..31", i.e. binary 11110011111111111100000001111111 with
// bit 0 rightmost -- and remembering that the loop tests `block_mask & 1` while shifting
// right, so bit `n` of the mask governs byte value `n`:
//
//   | Mask bit | Byte values | `doc/txtvsbin.txt` list | Verdict contribution |
//   |---|---|---|---|
//   | set   | 0..=6, 14..=25, 28..=31 | block list | any occurrence forces BINARY |
//   | clear | 7, 8, 11, 12, 26, 27    | gray list  | tolerated: neither forces anything |
//   | clear | 9, 10, 13               | allow list | tested by the second stanza -> TEXT |
//   | n/a   | 32..=255                | allow list | tested by the third stanza -> TEXT |
//
// Note that the block-list row is 14..=25 and 28..=31, not the "14 to 31" the prose gives:
// the gray list carves 26 and 27 out of that range, and the mask -- which is what the code
// reads -- has those two bits clear. See
// `every_byte_value_is_classified_as_the_documentation_specifies`, which pins the
// resolution.
//
// Two consequences are counter-intuitive and are asserted explicitly below, because a
// clean-room implementation would plausibly get either one wrong:
//
//   * a buffer of **gray-list bytes only** is BINARY -- being tolerated is not being
//     allowed, so nothing ever selects TEXT and the final `return Z_BINARY` decides it;
//   * the **empty** buffer is BINARY, for the same reason. `doc/txtvsbin.txt` L50-L51 says
//     so in as many words: "the boundary case, when the file is empty, automatically falls
//     into the latter category".
//
// `_tr_flush_block` threads the caller's `data_type` through its signature rather than
// reaching it through an `s->strm` back-pointer, which safe Rust cannot express, and writes
// it **only** when it arrives as `Z_UNKNOWN` (`trees.c` L1005-L1007). The public driver
// then writes it back into `DeflateStream::data_type`, which is where every test below
// reads it.
// =======================================================================================

/// Prose, tabs, line feeds and carriage returns are text.
///
/// `corpus::text()` is printable ASCII and line feeds only -- no byte from the block list
/// anywhere -- so the second and third stanzas fire and the first cannot. The three
/// single-byte cases are the allow-list control characters the second stanza tests by hand,
/// which is the only place in the function where a specific literal index appears.
#[test]
fn allow_list_input_is_classified_as_text() {
    assert_eq!(data_type_of(&corpus::text()), Z_TEXT, "prose");
    assert_eq!(data_type_of(corpus::SINGLE_BYTE), Z_TEXT, "one letter");
    assert_eq!(data_type_of(b"plain ASCII words"), Z_TEXT, "words");

    for allowed in [9_u8, 10, 13] {
        assert_eq!(
            data_type_of(&[allowed; 4]),
            Z_TEXT,
            "byte {allowed} is on the allow list (doc/txtvsbin.txt L41-L42)"
        );
    }

    // The high half of the allow list, 128..=255, is easy to overlook because it is not
    // ASCII at all; `doc/txtvsbin.txt` L42 puts "32 (SPACE) to 255" on the allow list, so
    // a buffer of high bytes is text.
    assert_eq!(data_type_of(&[0x80_u8, 0xfe, 0xff]), Z_TEXT, "high bytes");
}

/// Allow-list bytes mixed with gray-list bytes are still text.
///
/// The gray list is *tolerated*, not rejected: those six mask bits are clear, so the first
/// stanza skips them entirely and the allow-list bytes alone decide the verdict. A port that
/// treated the gray list as blocked would call this binary.
///
/// Each gray byte is checked on its own as well as all six together, because two of them --
/// 26 (SUB) and 27 (ESC) -- fall inside the "14 to 31" the block-list prose gives, and are
/// gray only because the mask says so. They are the two a careless reading gets wrong, and a
/// per-byte assertion is what names the culprit when it happens.
#[test]
fn gray_list_bytes_do_not_spoil_otherwise_textual_input() {
    /// Allow-list-only prose, so the verdict turns entirely on what is appended to it.
    const PROSE: &[u8] = b"a form feed, a bell and an escape walk into a buffer";

    let mut mixed = PROSE.to_vec();
    mixed.extend_from_slice(&GRAY_LIST);
    mixed.extend_from_slice(b"\nand nothing happens.\n");
    assert_eq!(
        data_type_of(&mixed),
        Z_TEXT,
        "gray-list bytes are tolerated, so the allow-list bytes decide"
    );

    for gray in GRAY_LIST {
        let mut payload = PROSE.to_vec();
        payload.push(gray);
        assert_eq!(
            data_type_of(&payload),
            Z_TEXT,
            "byte {gray} is tolerated and must not turn text into binary"
        );
    }
}

/// Gray-list bytes on their own are binary.
///
/// Nothing selects TEXT -- the gray list is not on the allow list -- so control falls
/// through to the final `return Z_BINARY` at `trees.c` L990. Each gray byte is also checked
/// alone, so a mask with one bit wrongly set is localised rather than merely detected.
#[test]
fn gray_list_only_input_is_classified_as_binary() {
    assert_eq!(
        data_type_of(&GRAY_LIST),
        Z_BINARY,
        "a buffer of tolerated bytes and nothing else is binary"
    );

    for gray in GRAY_LIST {
        assert_eq!(
            data_type_of(&[gray; 4]),
            Z_BINARY,
            "byte {gray} is on the gray list, which never selects text"
        );
    }
}

/// A single block-list byte makes otherwise textual input binary.
///
/// The first stanza returns as soon as it finds one, before either allow-list stanza runs,
/// so position and quantity are irrelevant -- which is what the three placements below
/// check. `corpus::HELLO` is a real example of the same thing: it is printable ASCII plus a
/// terminating NUL (`test/example.c` L35), and the NUL alone makes it binary.
#[test]
fn one_block_list_byte_is_enough_to_force_binary() {
    for blocked in [0_u8, 1, 3, 6, 14, 20, 25, 28, 31] {
        let text = b"perfectly ordinary text";

        let mut leading = vec![blocked];
        leading.extend_from_slice(text);
        let mut trailing = text.to_vec();
        trailing.push(blocked);
        let mut embedded = text.to_vec();
        embedded.insert(text.len() / 2, blocked);

        for (placement, payload) in [
            ("leading", leading),
            ("trailing", trailing),
            ("embedded", embedded),
        ] {
            assert_eq!(
                data_type_of(&payload),
                Z_BINARY,
                "a {placement} block-listed byte {blocked} forces binary"
            );
        }
    }

    assert_eq!(
        data_type_of(corpus::HELLO),
        Z_BINARY,
        "corpus::HELLO ends in a NUL, which is block-listed"
    );
    assert_eq!(data_type_of(&corpus::binary()), Z_BINARY, "all 256 values");
}

/// The empty buffer is binary.
///
/// `doc/txtvsbin.txt` L50-L51 states the boundary case, and `trees.c`'s final
/// `return Z_BINARY` implements it: with no frequencies set, no stanza can fire.
#[test]
fn empty_input_is_classified_as_binary() {
    assert_eq!(data_type_of(corpus::EMPTY), Z_BINARY);
}

/// Every one of the 256 byte values is classified exactly as `doc/txtvsbin.txt` says.
///
/// This validates the whole `0xf3ffc07f` mask rather than a handful of samples, and it is
/// what would catch a mask transcribed with a single bit wrong -- a defect that changes the
/// verdict for exactly one byte value and that no sample-based test would find.
///
/// 256 compressions of a four-byte payload, at the smallest window and `memLevel`, which is
/// what keeps it comfortable under Miri: `detect_data_type` reads only the `dyn_ltree`
/// frequencies, so neither parameter can influence the answer. The next test verifies that
/// substitution instead of assuming it.
#[test]
fn every_byte_value_is_classified_as_the_documentation_specifies() {
    // The three lists of `doc/txtvsbin.txt` L40-L45, spelled out independently of the
    // `0xf3ffc07f` mask so that two statements of the same rule are being compared rather
    // than one being restated.
    //
    // ★ One subtlety, and it is the whole reason to write the lists out rather than trust
    // the prose: `doc/txtvsbin.txt` L44-L45 gives the block list as "0 (NUL) to 6, 14 to
    // 31", which textually *overlaps* the gray list it has just given as "7, 8, 11, 12, 26,
    // 27" -- 26 and 27 appear in both. The mask settles it, and the doc's own wording says
    // which way: the gray list is "ignored in this detection algorithm", and bits 26 and 27
    // of `0xf3ffc07f` are indeed clear, so the effective block list is
    // 0..=6, 14..=25 and 28..=31. The observable difference is real rather than academic:
    // reading 26 as blocked would make prose containing a SUB byte binary, whereas the
    // reference calls it text. `gray_list_bytes_do_not_spoil_otherwise_textual_input`
    // asserts that case directly.
    let is_allow_listed = |value: u8| matches!(value, 9 | 10 | 13 | 32..=u8::MAX);
    let is_block_listed = |value: u8| matches!(value, 0..=6 | 14..=25 | 28..=31);

    for value in 0..=u8::MAX {
        // The gray list is precisely what belongs to neither of the other two. Asserting
        // that -- rather than assuming it -- is what makes the two closures above a
        // *partition* of the byte range, so a value accidentally omitted from both would be
        // caught here instead of silently defaulting to binary.
        assert_eq!(
            GRAY_LIST.contains(&value),
            !is_allow_listed(value) && !is_block_listed(value),
            "byte {value}: the gray list is exactly the complement of allow and block"
        );

        // `doc/txtvsbin.txt` L48-L51, verbatim: "If a file contains at least one byte that
        // belongs to the allow list and no byte that belongs to the block list, then the
        // file is categorized as plain text; otherwise, it is categorized as binary."
        let expected = if is_allow_listed(value) && !is_block_listed(value) {
            Z_TEXT
        } else {
            Z_BINARY
        };

        assert_eq!(
            data_type_of(&[value; 4]),
            expected,
            "byte {value} classified against doc/txtvsbin.txt"
        );
    }
}

/// The window size and `memLevel` cannot change the classification.
///
/// This is the assumption [`data_type_of`] rests on, made into an assertion: the sweep above
/// uses a 512-byte window and `memLevel` 1 for speed, and that is only legitimate because
/// `detect_data_type` reads nothing but literal frequencies. One representative byte from
/// each of the three lists is compared across both configurations, plus the two boundary
/// payloads.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** This is the one
/// test in the section that must use a default-size state on both sides of the comparison,
/// and a default-size state is roughly 256 `KiB` that `GlobalAllocator` writes in full. Every
/// other classification test runs at [`small_raw_config`] and therefore runs under Miri.
#[test]
#[cfg_attr(miri, ignore)]
fn the_classification_is_independent_of_window_size_and_mem_level() {
    let payloads: [(&str, Vec<u8>); 6] = [
        ("allow", vec![b'a'; 4]),
        ("gray", vec![7_u8; 4]),
        ("block", vec![0_u8; 4]),
        ("empty", Vec::new()),
        ("hello", corpus::HELLO.to_vec()),
        ("text", corpus::text()),
    ];

    for (name, payload) in &payloads {
        let small = compress_whole(payload, small_raw_config(6, Strategy::Default)).data_type;
        let default = compress_whole(payload, raw_config(6, Strategy::Default)).data_type;
        assert_eq!(
            small, default,
            "{name}: a 512-byte window must classify as a 32 KiB one does"
        );
    }
}

/// The strategy cannot change the classification either, but level 0 suppresses it
/// entirely.
///
/// `detect_data_type` is called from inside `if (s->level > 0)` (`trees.c` L1002-L1007), so
/// at level 0 no tree is built, the probe never runs, and the caller's `data_type` keeps the
/// `Z_UNKNOWN` that `deflateReset` put there (`deflate.c` L653). That is not an omission to
/// be tidied up: `zlib.h` L350-L353 describes `data_type` as informational, and a stored
/// block carries no Huffman frequencies to classify.
#[test]
fn the_classification_ignores_the_strategy_and_level_zero_leaves_it_unknown() {
    // Only the *first* block's frequencies decide the verdict, because `_tr_flush_block`
    // writes `data_type` once and only while it still reads `Z_UNKNOWN` (`trees.c`
    // L1005-L1007). At [`MIN_MEM_LEVEL`] a block holds at most 127 symbols
    // (`lit_bufsize` = 128, `deflate.c` L464), so feeding more than [`MIRI_SWEEP_LEN`] bytes
    // to this particular sweep would cost interpreter time without testing anything further.
    let payloads: [(&str, Vec<u8>, i32); 3] = [
        ("text", short_text(), Z_TEXT),
        ("hello", corpus::HELLO.to_vec(), Z_BINARY),
        (
            "binary",
            corpus::binary()[..MIRI_SWEEP_LEN].to_vec(),
            Z_BINARY,
        ),
    ];

    for (name, payload, expected) in &payloads {
        for strategy in Strategy::ALL {
            for level in [1, 6, 9] {
                assert_eq!(
                    compress_whole(payload, small_raw_config(level, strategy)).data_type,
                    *expected,
                    "{name} at level {level} under {}",
                    strategy.c_name()
                );
            }

            assert_eq!(
                compress_whole(payload, small_raw_config(0, strategy)).data_type,
                Z_UNKNOWN,
                "{name} at level 0 under {} must leave data_type untouched",
                strategy.c_name()
            );
        }
    }
}

// =======================================================================================
// The bit writer, observed through its consequences
//
// `send_bits`, `bi_reverse`, `bi_flush` and `bi_windup` (`trees.c` L154-L191 and
// L253-L292) are all `pub(crate)`, so none of them can be called from here. What *is*
// observable is what they leave in the output, and three properties pin them tightly:
//
//   1. `bi_windup` pads to a byte boundary, and `_tr_stored_block` then writes an empty
//      stored block. Together they are the four bytes `00 00 FF FF` that `zlib.h` L296-L298
//      names outright, reached through `_tr_stored_block` (`trees.c` L860) and, for
//      `Z_PARTIAL_FLUSH`, `_tr_align` (`trees.c` L888) from the call site at
//      `deflate.c` L1240. The block shape itself is RFC 1951 §3.2.4.
//   2. `put_short` writes least-significant byte first (`trees.c` L143-L147), whereas
//      `putShortMSB` in `deflate.c` L939-L942 writes most-significant byte first. The two
//      live in different files, do almost the same thing, and swapping them corrupts the
//      stream with no other symptom. `LEN` followed by its one's complement `NLEN`, read as
//      two little-endian 16-bit words, is what pins the former; the zlib header bytes pin
//      the latter and belong to `tests/deflate.rs`.
//   3. `DeflateState::bi_buf` is a `u16` and must stay one (`src/trees/bit_writer.rs`, and
//      `deflate.h` L266 declares C's as `ush`), because the truncation at 16 bits is
//      observable: `send_bits` spills the accumulator to the pending buffer at a width that
//      a wider type would reach later. The buffer cannot be read from here, but flushing at
//      many different bit offsets can be, and a widened accumulator changes the spill point
//      and corrupts at least one of them.
// =======================================================================================

/// `Z_SYNC_FLUSH` ends the output with the canonical empty stored block.
///
/// `zlib.h` L290-L298 defines the flush and then names the bytes: the output "is aligned on a
/// byte boundary", and the block "is three bits plus filler bits to the next byte, followed by
/// four bytes (00 00 ff ff)". The alignment is `bi_windup`; the four bytes are
/// `_tr_stored_block` writing a zero-length block, whose `LEN`/`NLEN` pair is `00 00` then
/// `FF FF`.
///
/// Asserted across the strategies and over inputs from nothing to a few hundred bytes,
/// because the marker must appear whatever state the accumulator was left in.
#[test]
fn sync_flush_ends_with_the_canonical_empty_stored_block() {
    let text = short_text();
    let payloads: [(&str, &[u8]); 4] = [
        ("empty", corpus::EMPTY),
        ("single_byte", corpus::SINGLE_BYTE),
        ("hello", corpus::HELLO),
        ("text", &text),
    ];

    for (name, payload) in payloads {
        for strategy in Strategy::ALL {
            let config = small_raw_config(6, strategy);
            let flushed = compress_until_flush(payload, config, Z_SYNC_FLUSH);
            let tail = &flushed[flushed.len() - SYNC_MARKER.len()..];
            assert_eq!(
                tail,
                &SYNC_MARKER,
                "{name} under {}: Z_SYNC_FLUSH must end with 00 00 FF FF",
                strategy.c_name()
            );

            // And the flushed prefix is a complete, decodable sequence of non-final blocks:
            // `zlib.h` L290-L294 promises that after a sync flush "the decompressor can get
            // all input data available so far".
            assert_eq!(
                inflate_raw_fragment(&flushed, payload.len(), config.window_bits),
                payload,
                "{name} under {}: the flushed prefix must decode",
                strategy.c_name()
            );
        }
    }
}

/// `LEN` and `NLEN` are one's complements, written least-significant byte first.
///
/// Three independent stored blocks are read: the empty one a `Z_SYNC_FLUSH` leaves, and two
/// data-carrying ones from level 0. The byte order is the load-bearing part -- `0x000e`
/// stored as `0e 00` rather than `00 0e` is what makes `put_short` least-significant byte
/// first -- so the raw bytes are asserted, not just the decoded numbers.
#[test]
fn stored_block_lengths_are_ones_complements_written_lsb_first() {
    // The empty block a sync flush leaves: LEN = 0x0000, NLEN = 0xffff.
    let flushed = compress_until_flush(
        corpus::HELLO,
        raw_config(6, Strategy::Default),
        Z_SYNC_FLUSH,
    );
    let marker = &flushed[flushed.len() - SYNC_MARKER.len()..];
    let len = u16::from_le_bytes([marker[0], marker[1]]);
    let nlen = u16::from_le_bytes([marker[2], marker[3]]);
    assert_eq!((len, nlen), (0x0000, 0xffff));
    assert_eq!(nlen, !len);

    // Level 0 stored blocks, where LEN is non-zero and the byte order is visible.
    // `corpus::HELLO` is 14 bytes, so LEN is 0x000e and its bytes must be `0e 00`; NLEN is
    // 0xfff1 and its bytes must be `f1 ff`.
    let hello = compress_raw(corpus::HELLO, 0, Strategy::Default).bytes;
    assert_eq!(
        &hello[..5],
        &[0x01, 0x0e, 0x00, 0xf1, 0xff],
        "LEN and NLEN are little-endian 16-bit words, not big-endian ones"
    );

    let payload = corpus::incompressible(300);
    let stored = compress_raw(&payload, 0, Strategy::Default).bytes;
    let (len, nlen) = stored_block_lengths(&stored);
    assert_eq!(usize::from(len), payload.len());
    assert_eq!(nlen, !len);
    // 300 is 0x012c, whose two halves differ, so this pair would be caught by a byte swap
    // where a value below 256 would not.
    assert_eq!(&stored[1..5], &[0x2c, 0x01, 0xd3, 0xfe]);
}

/// `Z_PARTIAL_FLUSH` flushes without aligning, and still round-trips.
///
/// The contrast with `Z_SYNC_FLUSH` is the point: `zlib.h` L300-L306 says a partial flush
/// makes all pending output available but "the output is not aligned to a byte boundary", so
/// `bi_windup` does **not** run and the four-byte marker must be absent. What `_tr_align`
/// emits instead is "an empty fixed codes block that is 10 bits long" (`trees.c` L888-L905),
/// which occupies a non-integral number of bytes.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The payload is
/// `corpus::text()` at the default window, so the state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `sync_flush_ends_with_the_canonical_empty_stored_block`
/// and `flushing_at_every_bit_offset_produces_a_decodable_stream` cover the aligning case
/// under Miri at [`small_raw_config`].
#[test]
#[cfg_attr(miri, ignore)]
fn partial_flush_does_not_align_the_output() {
    let payload = corpus::text();
    let flushed = compress_until_flush(&payload, raw_config(6, Strategy::Default), Z_PARTIAL_FLUSH);

    assert!(
        !flushed.ends_with(&SYNC_MARKER),
        "Z_PARTIAL_FLUSH must not byte-align, so it must not leave the sync marker"
    );
    assert_eq!(
        inflate_raw_fragment(&flushed, payload.len(), RAW_WINDOW_BITS),
        payload,
        "the partially flushed prefix must still decode in full"
    );

    // The whole stream, flush and all, must finish and round-trip.
    let (whole, _mark) = compress_with_flush_then_finish(&payload, Z_PARTIAL_FLUSH);
    assert_eq!(inflate_raw(&whole, payload.len()), payload);
}

/// `Z_FULL_FLUSH` aligns, resets the window, and leaves an independently decodable tail.
///
/// `zlib.h` L317-L321: a full flush is a sync flush that additionally resets the compression
/// state "so that decompression can restart from this point if previous compressed data has
/// been damaged or if random access is desired". Three consequences are asserted, and the third is the one that actually
/// proves the reset happened:
///
/// 1. the marker is present, because a full flush aligns exactly as a sync flush does;
/// 2. the whole stream round-trips;
/// 3. the bytes **after** the marker decode on their own, from a decoder with an empty
///    window -- which they could not if the second section still referred back into the
///    first -- and `inflateSync` finds the marker and recovers that section even when the
///    first section has been corrupted, which is the recovery `zlib.h` describes.
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** The payload is
/// `corpus::text()` at the default window, so the state is roughly 256 `KiB` that
/// `GlobalAllocator` writes in full. `sync_flush_ends_with_the_canonical_empty_stored_block`
/// and `flushing_at_every_bit_offset_produces_a_decodable_stream` cover the aligning case
/// under Miri at [`small_raw_config`].
#[test]
#[cfg_attr(miri, ignore)]
fn full_flush_resets_the_window_and_leaves_a_recoverable_boundary() {
    // A first section with a long run in it, so that a compressor which had *not* reset
    // would find matches back into it from the second section.
    let first = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA first section, quite repetitive".to_vec();
    let second = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA second section, quite repetitive".to_vec();
    let mut whole = first.clone();
    whole.extend_from_slice(&second);

    let (stream, mark) = compress_two_sections(&whole, first.len(), Z_FULL_FLUSH);

    // 1. The boundary is the aligned marker.
    assert_eq!(
        &stream[mark - SYNC_MARKER.len()..mark],
        &SYNC_MARKER,
        "Z_FULL_FLUSH aligns and leaves the empty stored block"
    );

    // 2. The whole stream round-trips.
    assert_eq!(inflate_raw(&stream, whole.len()), whole);

    // 3. The tail stands alone, and is recoverable after damage to the head.
    assert_eq!(
        inflate_raw(&stream[mark..], second.len()),
        second,
        "after a full flush the following section must not refer back into the window"
    );
    assert_eq!(
        recover_after_sync(&stream, second.len()),
        second,
        "inflateSync must find the marker and resume at the second section"
    );
}

/// Flushing at every bit offset from 0 to 24 bytes of input always yields a decodable
/// stream.
///
/// This is the substitute for reading `bi_buf`, which is `pub(crate)`. Each iteration leaves
/// the accumulator holding a different number of valid bits when `bi_windup` runs, so the
/// sweep walks the whole `0..16` range of `bi_valid` several times over. An accumulator
/// wider than the `ush` C declares (`deflate.h` L266) would spill to the pending buffer at
/// different moments and corrupt at least one of these cases, and a `bi_windup` that padded
/// with the wrong number of bits would corrupt most of them.
///
/// 25 compressions of at most 24 bytes: bounded, and cheap enough for Miri.
#[test]
fn flushing_at_every_bit_offset_produces_a_decodable_stream() {
    let source = corpus::text();

    for exposed in 0..=24_usize {
        let payload = &source[..exposed];
        let config = small_raw_config(6, Strategy::Default);
        let flushed = compress_until_flush(payload, config, Z_SYNC_FLUSH);

        assert!(
            flushed.ends_with(&SYNC_MARKER),
            "{exposed} bytes: the flush must byte-align"
        );
        assert_eq!(
            inflate_raw_fragment(&flushed, exposed, config.window_bits),
            payload,
            "{exposed} bytes: the flushed prefix must decode exactly"
        );
    }
}

/// Compresses `input` and returns everything a single `flush` call produced.
///
/// The state is deliberately dropped mid-stream, so `deflateEnd` reports `Z_DATA_ERROR` --
/// the status `zlib.h` documents for a stream freed prematurely, with some input or output
/// discarded. `deflateEnd` derives it from the state still being `BUSY_STATE`
/// (`deflate.c` L1298 and L1309). Asserting that value here rather than tolerating it keeps the helper
/// honest about what it is doing, and incidentally pins a documented public contract.
fn compress_until_flush(input: &[u8], config: DeflateConfig, flush: i32) -> Vec<u8> {
    let mut state = deflate_init2(config, GlobalAllocator).expect("deflate state allocation");
    let reset = deflate_reset(&mut state);
    let mut out = vec![0_u8; deflate_bound_z(Some(&state), input.len()) + 64];

    let produced = {
        let mut stream = DeflateStream::new(input, &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, flush),
            ReturnCode::OK,
            "a mid-stream flush with room to spare returns Z_OK"
        );
        assert_eq!(
            stream.avail_in(),
            0,
            "a flush must consume all the input offered"
        );
        stream.next_out
    };
    assert_eq!(
        deflate_end(state),
        ReturnCode::DATA_ERROR,
        "ending a BUSY stream reports Z_DATA_ERROR (deflate.c L1309)"
    );

    out.truncate(produced);
    out
}

/// Compresses `input` with one `flush` call and then finishes, returning the whole stream
/// and how many bytes the flush had produced by the time it returned.
fn compress_with_flush_then_finish(input: &[u8], flush: i32) -> (Vec<u8>, usize) {
    let config = raw_config(6, Strategy::Default);
    let mut state = deflate_init2(config, GlobalAllocator).expect("deflate state allocation");
    let reset = deflate_reset(&mut state);
    let mut out = vec![0_u8; deflate_bound_z(Some(&state), input.len()) + 4096];

    let (mark, total) = {
        let mut stream = DeflateStream::new(input, &mut out);
        stream.apply_reset(reset);
        assert_eq!(deflate(&mut state, &mut stream, flush), ReturnCode::OK);
        let mark = stream.next_out;
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END
        );
        (mark, stream.next_out)
    };
    assert_eq!(deflate_end(state), ReturnCode::OK);

    out.truncate(total);
    (out, mark)
}

/// Compresses `whole` as two sections split at `split`, with `flush` between them.
///
/// Returns the complete stream and the offset just past the flush, i.e. where the second
/// section begins.
///
/// More input is offered by **widening** the input slice and leaving `next_in` where the
/// previous call left it, which is the pattern `crates/zlib-rs/src/deflate/mod.rs` documents.
/// Re-slicing from `next_in` instead would discard the consumed prefix that `deflate_stored`
/// reads backwards through and would desynchronise the cursor from the slice it indexes.
fn compress_two_sections(whole: &[u8], split: usize, flush: i32) -> (Vec<u8>, usize) {
    let config = raw_config(6, Strategy::Default);
    let mut state = deflate_init2(config, GlobalAllocator).expect("deflate state allocation");
    let reset = deflate_reset(&mut state);
    let mut out = vec![0_u8; deflate_bound_z(Some(&state), whole.len()) + 4096];

    let (mark, total) = {
        let mut stream = DeflateStream::new(&whole[..split], &mut out);
        stream.apply_reset(reset);
        assert_eq!(deflate(&mut state, &mut stream, flush), ReturnCode::OK);
        let mark = stream.next_out;

        stream.input = whole;
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END
        );
        (mark, stream.next_out)
    };
    assert_eq!(deflate_end(state), ReturnCode::OK);

    out.truncate(total);
    (out, mark)
}

/// Damages the first section of `stream`, then recovers the rest with `inflateSync`.
///
/// `zlib.h` L951-L968 describes exactly this: `inflateSync` "skips invalid compressed data
/// until a possible full flush point ... can be found", and is intended for "restarting
/// inflate after `Z_DATA_ERROR`". The byte flipped is inside the first compressed block, far
/// enough in to corrupt it and far enough from the marker to leave the boundary intact.
fn recover_after_sync(stream: &[u8], tail_len: usize) -> Vec<u8> {
    let mut damaged = stream.to_vec();
    damaged[2] ^= 0xff;

    let mut state = inflate_init2(InflateConfig::new(RAW_WINDOW_BITS), GlobalAllocator)
        .expect("inflate state allocation");
    let reset = inflate_reset(&mut state);
    let mut out = vec![0_u8; stream.len() + tail_len + 64];

    let recovered = {
        let mut view = InflateStream::new(&damaged, &mut out);
        view.apply_reset(reset);
        assert_eq!(
            inflate(&mut state, &mut view, Z_FINISH),
            ReturnCode::DATA_ERROR,
            "the damaged first section must be rejected"
        );
        assert_eq!(
            inflate_sync(&mut state, &mut view),
            ReturnCode::OK,
            "inflateSync must find the full-flush point"
        );

        let resumed_at = view.next_out;
        assert_eq!(
            inflate(&mut state, &mut view, Z_FINISH),
            ReturnCode::STREAM_END,
            "the surviving section must decode to the end of the stream"
        );
        view.output[resumed_at..view.next_out].to_vec()
    };
    assert_eq!(inflate_end(state), ReturnCode::OK);

    recovered
}

// =======================================================================================
// Round-trip integrity across the strategy space
//
// The broad safety net beneath every targeted assertion above. The targeted tests say what
// the coder must decide; these say that whatever it decided is decodable. Any
// tree-construction defect the targeted tests miss -- a mis-transmitted bit-length tree, a
// `send_tree` run length off by one, a `gen_bitlen` overflow adjustment applied to the wrong
// bucket -- will almost certainly break a round trip somewhere in this matrix, because the
// decoder rebuilds the trees from exactly the bits the encoder sent.
//
// Two tiers, for one reason only: interpreter speed.
// =======================================================================================

/// Every strategy at every level of [`LEVELS`] round-trips the small payload classes.
///
/// Deliberately small -- no payload exceeds [`MIRI_SWEEP_LEN`] and the state is
/// [`small_raw_config`] -- so that this tier runs unconditionally, including under Miri,
/// where every byte of payload and every byte of allocated state is a byte the interpreter
/// has to account for. `Vec::len` is checked separately from the contents so that a
/// truncation reports as a length rather than as an opaque slice mismatch.
#[test]
fn the_small_payload_classes_round_trip_across_every_level_and_strategy() {
    let classes: [(&str, Vec<u8>); 6] = [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(MIRI_SWEEP_LEN)),
        ("incompressible", corpus::incompressible(MIRI_SWEEP_LEN)),
        ("text", short_text()),
    ];

    for (name, payload) in &classes {
        for level in LEVELS {
            for strategy in Strategy::ALL {
                let config = small_raw_config(level, strategy);
                let compressed = compress_whole(payload, config);
                let recovered =
                    inflate_raw_with(&compressed.bytes, payload.len(), config.window_bits);

                assert_eq!(
                    recovered.len(),
                    payload.len(),
                    "{name} at level {level} under {}: recovered length",
                    strategy.c_name()
                );
                assert_eq!(
                    &recovered,
                    payload,
                    "{name} at level {level} under {}: recovered bytes",
                    strategy.c_name()
                );
            }
        }
    }
}

/// Every strategy at every one of the ten levels round-trips every corpus class.
///
/// The full sweep: 8 classes x 10 levels x 5 strategies = 400 round trips, including
/// `corpus::window_crossing()` at 33048 bytes, which is the only class that forces
/// `fill_window` to slide and `slide_hash` to re-base the chains -- and therefore the only
/// one that exercises tree construction over a block whose bytes are no longer all in the
/// window, which is exactly the `buf == NULL` case the stored-block predicate guards against
/// (`trees.c` L1047-L1055).
///
/// **Ignored under Miri for interpreter speed only, never for correctness.** Roughly 1.3 MB
/// of payload passes through the compressor and the decompressor here; under Miri that is
/// hours rather than the fraction of a second it takes natively. The tier above keeps the
/// same code paths covered under Miri with small payloads, and the degenerate-tree section
/// -- the part Miri is genuinely valuable for, because it is where the port's `wrapping_sub`
/// arithmetic lives -- runs there unconditionally.
#[test]
#[cfg_attr(miri, ignore)]
fn every_corpus_class_round_trips_across_every_level_and_strategy() {
    for (name, payload) in corpus::all() {
        for level in 0..=9 {
            for strategy in Strategy::ALL {
                let compressed = compress_raw(&payload, level, strategy);
                let recovered = inflate_raw(&compressed.bytes, payload.len());

                assert_eq!(
                    recovered.len(),
                    payload.len(),
                    "{name} at level {level} under {}: recovered length",
                    strategy.c_name()
                );
                assert_eq!(
                    recovered,
                    payload,
                    "{name} at level {level} under {}: recovered bytes",
                    strategy.c_name()
                );
            }
        }
    }
}

/// Compression is deterministic: the same input and parameters give the same bytes, every
/// time.
///
/// Byte-identical output against the C reference is `crates/zlib-rs-differential`'s job,
/// because only that crate can link the oracle. What *is* checkable here is the precondition
/// for it: if the coder were not deterministic -- if it read uninitialised state, or hashed
/// an address, or depended on allocation addresses -- byte-identity could not hold and no
/// differential run would be reproducible. Repeating each configuration in a fresh state
/// checks exactly that, and it is the cheapest place in the suite to check it.
///
/// The allocator fills every block it hands out with `0xa5` rather than zero
/// (`crates/zlib-rs/src/allocate.rs`, mirroring `test/infcover.c` L87), so a coder that read
/// a field before writing it would produce that pattern's consequences consistently rather
/// than randomly -- which is why determinism is asserted alongside, and not instead of, the
/// value assertions elsewhere in this file.
#[test]
fn compression_is_deterministic_for_a_fixed_configuration() {
    let classes: [(&str, Vec<u8>); 4] = [
        ("empty", corpus::EMPTY.to_vec()),
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(MIRI_SWEEP_LEN)),
        ("text", short_text()),
    ];

    for (name, payload) in &classes {
        for level in LEVELS {
            for strategy in Strategy::ALL {
                let first = compress_whole(payload, small_raw_config(level, strategy));
                let second = compress_whole(payload, small_raw_config(level, strategy));
                assert_eq!(
                    first,
                    second,
                    "{name} at level {level} under {}: two runs must agree byte for byte",
                    strategy.c_name()
                );
            }
        }
    }
}
