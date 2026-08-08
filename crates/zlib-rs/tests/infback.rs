//! Integration suite for `inflateBack`, the callback-driven raw-DEFLATE
//! decompressor.
//!
//! The unit under test is `crates/zlib-rs/src/infback.rs`, the port of
//! `infback.c` -- `inflate_back_init` (`infback.c` L25-L64), `inflate_back`
//! (L191-L570) and `inflate_back_end` (L572-L579). It shares its state, its
//! decode tables and its fast path with the `inflate()` subtree, but its
//! *contract* is entirely different: the caller supplies the window, the caller
//! supplies both buffers through callbacks, and the two callbacks report failure
//! in **opposite** directions.
//!
//! # ★ THE POLARITY RULE
//!
//! `infback.c` L182-L185 states it, and it is the single most error-prone thing
//! about this interface:
//!
//! > `in()` should return zero on failure. `out()` should return non-zero on
//! > failure.
//!
//! Read that twice. The two conventions are **inverted**, and both are part of
//! the frozen C contract established by the `in_func` and `out_func` typedefs at
//! `zlib.h` L1134-L1136:
//!
//! ```c
//! typedef unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *);
//! typedef int (*out_func)(void FAR *, unsigned char FAR *, unsigned);
//! ```
//!
//! `in_func` returns a **count**, so zero means "no bytes, and there will be no
//! more" -- failure. `out_func` returns a **status**, so zero means "accepted"
//! and anything else means failure. Either failure makes `inflateBack` return
//! `Z_BUF_ERROR`, and the caller tells them apart through `strm->next_in`, which
//! `zlib.h` L1199-L1202 says "will be `Z_NULL` only if `in()` returned an error".
//!
//! In the safe core those three facts become, respectively:
//!
//! | C | Here |
//! |---|---|
//! | `in()` returns 0 | [`InflateBackInput::next_chunk`] yields `None` **or** `Some(&[])` |
//! | `out()` returns non-zero | [`InflateBackOutput::write_out`] yields `Err(OutputFailure)` |
//! | `strm->next_in == Z_NULL` | `InflateBackResult::next_in` is `None` |
//!
//! Nothing else in this file matters if that is wrong, so the polarity is
//! asserted from both directions and then asserted *again*, in
//! [`the_two_callback_polarities_are_opposite`], as a single test whose only job
//! is to fail loudly if the two conventions are ever made to agree.
//!
//! # ★ Why this module gets a suite of its own
//!
//! `inflateBack` and the `gz*` family were the last pieces needed for full
//! compatibility with the zlib public API, so this is deliberately a first-class
//! suite rather than a corner of `tests/inflate.rs`.
//!
//! And there is a second, sharper reason. The baseline commit of this entire port
//! is `09a1572`, whose subject line is *"Fix `inflateBack()` bug that would fail
//! to detect a too far back."* -- a bug that "would pass off an invalid deflate
//! stream as good, and copy uninitialized memory contents to the output". The
//! `invalid distance too far back` vector exercised by
//! [`the_invalid_distance_too_far_back_vector_is_rejected`] is therefore not one
//! assertion among forty; it is the reason this file exists at all.
//!
//! # The oracle
//!
//! Every expectation below is taken from `test/infcover.c`, the C harness, and
//! from nowhere else:
//!
//! * `cover_back()` (L471-L505) with its `pull()` and `push()` callbacks
//!   (L447-L468) is reproduced step for step by
//!   [`the_cover_back_sequence_from_the_c_harness_reproduces_step_for_step`],
//!   with the three steps that are structurally unreachable from safe Rust
//!   asserted as their reachable subset and each delegation recorded in a comment.
//! * `try()` (L508-L579) runs every raw vector through **both** `inflate()` and
//!   `inflateBack`, asserting `ret != Z_STREAM_ERROR` always and, for the error
//!   vectors, `Z_DATA_ERROR` with a byte-for-byte equal `strm->msg`. That vector
//!   table -- `cover_inflate()` L584-L614 -- is transcribed verbatim into
//!   [`RAW_VECTORS`] and driven by
//!   [`every_raw_vector_from_the_c_harness_behaves_as_the_reference_does`].
//!
//! The expected messages are written out as string literals rather than imported,
//! because the crate's `MSG_*` constants are `pub(crate)`. That is the better
//! arrangement here anyway: it makes these assertions independent of the
//! implementation's own spelling, exactly as C's `strcmp(id, strm->msg)` is.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no FFI, no `extern "C"`.** The crate under test carries
//!   `#![forbid(unsafe_code)]`; its tests hold the same line. The callbacks below
//!   are ordinary safe Rust types implementing two ordinary traits. Adapting C
//!   function pointers to them is `crates/libz-rs-sys/src/infback.rs`'s job and is
//!   tested there.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` declares an empty
//!   `[dependencies]` and no `[dev-dependencies]`, and `deny.toml`'s `[bans]`
//!   section names that table as the enforcement point. Only the built-in `#[test]`
//!   harness over `core`/`alloc`/`std` and `zlib_rs` itself are available.
//! * **`std` is available and `no_std` is not.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test is its
//!   own crate and links `std` unconditionally, so no `#![no_std]` appears here.
//! * **Reach the crate by module path.** `zlib-rs` declares `default = []` and
//!   every crate-root re-export is gated on `rust-api`, so `zlib_rs::ReturnCode`
//!   does not exist in a default build while `zlib_rs::error::ReturnCode` always
//!   does. Nothing below is feature-gated, and this file compiles and passes
//!   identically under `--no-default-features`, `--features simd` and
//!   `--all-features`.
//! * **Hermetic.** No network, no filesystem, no clock, no environment. Every
//!   fixture is either a byte literal taken from the C reference, a
//!   `common::corpus` payload, or a stream emitted by [`BlockWriter`], the small
//!   RFC 1951 writer below.
//! * **Independent of the encoder.** Nothing here reaches into `crate::deflate`.
//!   A decoder test whose fixtures come from the encoder in the same port would
//!   cancel out a shared misreading of the format, so the fixtures come from the
//!   reference and from a writer that consults nothing but RFC 1951 -- and the two
//!   are pinned to each other by
//!   [`the_fixture_writer_agrees_with_the_c_reference`].
//!
//! [`InflateBackInput::next_chunk`]: zlib_rs::infback::InflateBackInput::next_chunk
//! [`InflateBackOutput::write_out`]: zlib_rs::infback::InflateBackOutput::write_out

// The panic family is denied workspace-wide (Cargo.toml's
// `[workspace.lints.clippy]`), which is right for a library that decodes
// untrusted input and wrong for the harness that has to assert on it.
// clippy.toml's `allow-unwrap-in-tests`, `allow-expect-in-tests` and
// `allow-panic-in-tests` keys cover code *inside* a `#[test]` function, but this
// file also carries file-scope helpers -- the fixture builders and the vector
// runner -- which those keys do not reach, and `indexing_slicing` has no in-tests
// key at all. Hence the crate-level relaxation, matching what
// `tests/common/mod.rs` does for its own self-tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use zlib_rs::allocate::{Allocator, GlobalAllocator};
use zlib_rs::config::InflateConfig;
use zlib_rs::error::ReturnCode;
use zlib_rs::infback::{
    inflate_back, inflate_back_end, inflate_back_init, InflateBackInput, InflateBackOutput,
    OutputFailure,
};
use zlib_rs::inflate::state::{InflateState, DMAX_DEFAULT};
use zlib_rs::inflate::Mode;

// ---------------------------------------------------------------------------
// Fixtures transcribed from the C harness
// ---------------------------------------------------------------------------

/// The window size `test/infcover.c` uses for every `inflateBack` call.
///
/// `cover_back()` declares `unsigned char win[32768]` (L475) and `try()`
/// `malloc(32768)` (L524), then passes `windowBits = 15` to `inflateBackInit`
/// (L486, L502, L559). `1 << 15` is 32768, so the buffer is exactly the size the
/// contract at `infback.c` L22-L23 demands: "windowBits is in the range 8..15,
/// and window is a user-supplied window and output buffer that is 2**windowBits
/// bytes."
const HARNESS_WINDOW_LEN: usize = 32768;

/// The `windowBits` the harness uses throughout, and the largest the interface
/// accepts.
const HARNESS_WINDOW_BITS: i32 = 15;

/// The smallest `windowBits` `inflate_back_init` accepts (`infback.c` L34).
const MIN_BACK_WINDOW_BITS: i32 = 8;

/// The largest `windowBits` `inflate_back_init` accepts (`infback.c` L34).
const MAX_BACK_WINDOW_BITS: i32 = 15;

/// The byte every caller-supplied window in this file is pre-filled with.
///
/// `test/infcover.c`'s `mem_alloc` fills each block with `0xa5` and says why
/// (L87): "fill memory with a non-zero value to make sure that the code isn't
/// depending on zeros". The window handed to `inflateBack` is the *one* buffer
/// that discipline cannot reach, because the caller allocates it -- so this file
/// applies the same discipline by hand. Any code path that assumes a zeroed
/// window produces `0xa5` bytes in its output and fails loudly.
const WINDOW_SENTINEL: u8 = 0xa5;

/// One row of the raw-DEFLATE table `cover_inflate()` drives (`test/infcover.c`
/// L584-L614).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Vector {
    /// The stream, in the liberal hexadecimal `h2b` accepts.
    hex: &'static str,

    /// C's `id` argument: for an error vector this is the exact `strm->msg` the
    /// reference sets, and `try()` asserts `strcmp(id, strm.msg) == 0` on it
    /// (L549, L567). For a success vector it is a label only.
    id: &'static str,

    /// C's `err` argument. `1` means "expect `Z_DATA_ERROR` and this message",
    /// `0` means "expect success, assert only that it is not `Z_STREAM_ERROR`".
    ///
    /// C's third value, `-1`, marks a gzip-wrapped vector; `try()` skips the
    /// `inflateBack` leg for those (`if (err >= 0)`, L555) because `inflateBack`
    /// decodes raw streams only. They are listed separately in
    /// [`WRAPPED_VECTORS`] rather than being smuggled in here with a negative
    /// flag that every consumer would have to remember to filter.
    fails: bool,
}

/// Every raw-DEFLATE vector `try()` puts through `inflateBack`, in the order
/// `cover_inflate()` lists them (`test/infcover.c` L584-L611).
///
/// Nineteen rows: twelve malformed streams whose `strm->msg` is pinned
/// character for character, and seven well-formed ones. Between them they reach
/// the invalid-block-type check, both stored-block failure modes, every
/// code-length repeat form, both `inflate_table` failures, both invalid-code
/// checks, the too-far-back check, second-level table lookups, the longest
/// possible code, both extra-bit paths and the end-of-window wrap.
const RAW_VECTORS: &[Vector] = &[
    Vector {
        hex: "0 0 0 0 0",
        id: "invalid stored block lengths",
        fails: true,
    },
    Vector {
        hex: "3 0",
        id: "fixed",
        fails: false,
    },
    Vector {
        hex: "6",
        id: "invalid block type",
        fails: true,
    },
    Vector {
        hex: "1 1 0 fe ff 0",
        id: "stored",
        fails: false,
    },
    Vector {
        hex: "fc 0 0",
        id: "too many length or distance symbols",
        fails: true,
    },
    Vector {
        hex: "4 0 fe ff",
        id: "invalid code lengths set",
        fails: true,
    },
    Vector {
        hex: "4 0 24 49 0",
        id: "invalid bit length repeat",
        fails: true,
    },
    Vector {
        hex: "4 0 24 e9 ff ff",
        id: "invalid bit length repeat",
        fails: true,
    },
    Vector {
        hex: "4 0 24 e9 ff 6d",
        id: "invalid code -- missing end-of-block",
        fails: true,
    },
    Vector {
        hex: "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        id: "invalid literal/lengths set",
        fails: true,
    },
    Vector {
        hex: "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        id: "invalid distances set",
        fails: true,
    },
    Vector {
        hex: "4 c0 81 8 0 0 0 0 20 7f eb b 0 0",
        id: "invalid literal/length code",
        fails: true,
    },
    Vector {
        hex: "2 7e ff ff",
        id: "invalid distance code",
        fails: true,
    },
    // ★ The `09a1572` regression vector. See TOO_FAR_BACK below, which names the
    // same stream so that the dedicated test can reach it without an index.
    Vector {
        hex: TOO_FAR_BACK_HEX,
        id: MSG_TOO_FAR_BACK,
        fails: true,
    },
    Vector {
        hex: "5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c",
        id: "pull 17",
        fails: false,
    },
    Vector {
        hex: "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
        id: "long code",
        fails: false,
    },
    Vector {
        hex: "ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f",
        id: "length extra",
        fails: false,
    },
    Vector {
        hex: "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        id: "long distance and extra",
        fails: false,
    },
    Vector {
        hex: "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
              0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        id: "window end",
        fails: false,
    },
];

/// The two vectors `cover_inflate()` passes with `err = -1` (`test/infcover.c`
/// L601-L603), which `try()` deliberately withholds from `inflateBack`.
///
/// Both begin `1f 8b 8`, the gzip magic and method bytes, and both test a
/// *trailer* mismatch -- `"incorrect data check"` and `"incorrect length
/// check"`. `inflateBack` has no trailer to check and no header to parse, so it
/// cannot reproduce either verdict. Keeping them here, named and explained, is
/// what stops a future reader from "fixing" the vector table by merging them in;
/// [`the_gzip_wrapped_vectors_the_harness_skips_are_not_raw_streams`] asserts
/// what actually happens to them instead.
const WRAPPED_VECTORS: &[&str] = &[
    "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1",
    "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
];

/// The stream `cover_inflate()` labels `"invalid distance too far back"`
/// (`test/infcover.c` L598).
///
/// A dynamic block whose first symbol is a length/distance pair reaching back
/// past anything that has been written. This is the stream that commit
/// `09a1572` -- the baseline of this port -- restored the rejection of.
const TOO_FAR_BACK_HEX: &str = "c c0 81 0 0 0 0 0 90 ff 6b 4 0";

/// The message the reference sets for [`TOO_FAR_BACK_HEX`] (`infback.c` L519).
const MSG_TOO_FAR_BACK: &str = "invalid distance too far back";

/// The stream `cover_inflate()` labels `"fixed"` (`test/infcover.c` L585): a
/// final fixed-Huffman block holding nothing but the end-of-block code.
///
/// This is also the input `cover_back()` stages for its first successful call
/// (L487-L488, `avail_in = 2` over the C literal `"\x03"`, whose bytes are
/// `0x03` and its terminating NUL). It must return `Z_STREAM_END` having never
/// called `out()` at all, because it produces no output.
const EMPTY_FIXED: &[u8] = &[0x03, 0x00];

/// The stream `cover_back()` stages for its output-error call (`test/infcover.c`
/// L492-L493): `avail_in = 3` over the C literal `"\x63\x00"`, whose bytes are
/// `0x63`, `0x00` and the terminating NUL.
///
/// `0x63` is `0110_0011`: `BFINAL = 1`, `BTYPE = 01` (fixed). It decodes to a
/// single zero byte, which is exactly enough output to make `out()` run -- and
/// therefore exactly enough to observe an output failure.
const ONE_ZERO_BYTE_FIXED: &[u8] = &[0x63, 0x00, 0x00];

/// The single byte [`ONE_ZERO_BYTE_FIXED`] decodes to.
const ONE_ZERO_BYTE: &[u8] = &[0x00];

/// The bytes `pull()` hands out one at a time (`test/infcover.c` L450):
/// `static unsigned char dat[] = {0x63, 0, 2, 0}`.
///
/// Driving this array through an input callback that yields **one byte per
/// call** is the resumability contract of the callback form, and it is the shape
/// the C harness chose deliberately: a source that never yields six bytes at
/// once keeps `have` below the fast path's threshold, so `infback.c` L424's
/// `have >= 6 && left >= 258` can never fire and every symbol goes through the
/// slow path.
const PULL_DAT: &[u8] = &[0x63, 0x00, 0x02, 0x00];

/// `"hello, hello!"` raw-deflated at level 6: one fixed-Huffman block with a
/// match, which is the payload `test/example.c` L35 uses throughout because
/// "the repeated `hello` stresses the compression code better".
///
/// Verified against the C implementation through `zlib.compressobj(6, DEFLATED,
/// -15)`, which is that library.
const HELLO_FIXED: &[u8] = &[
    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00,
];

/// What [`HELLO_FIXED`] decodes to. Thirteen bytes, with no terminating NUL:
/// this fixture is the deflate of `strlen(hello)` bytes, not of
/// `common::corpus::HELLO`, which carries the NUL.
const HELLO: &[u8] = b"hello, hello!";

/// [`HELLO_FIXED`] inside an RFC 1950 zlib wrapper: `78 9c`, the raw block, then
/// the big-endian Adler-32 of the payload.
///
/// A complete, well-formed zlib stream, which is the point -- the assertion in
/// [`a_zlib_wrapped_stream_fails_as_data_not_as_a_header_parse`] is that
/// `inflateBack` rejects it *as data*, and that only means something if the stream
/// itself is valid. `0x78` is the CMF for method 8 with `windowBits = 15`, `0x9c`
/// the FLG for the default compression level, and `21 70 04 96` is
/// `adler32("hello, hello!")` most-significant byte first, per RFC 1950 §2.2.
///
/// Verified against the C implementation through `zlib.compress(b"hello, hello!",
/// 6)`, which is that library.
const ZLIB_WRAPPED_HELLO: &[u8] = &[
    0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21, 0x70,
    0x04, 0x96,
];

/// One row of the dynamic-Huffman fixtures taken from `cover_inflate()`.
///
/// These are the harness's own well-formed dynamic streams, reused here because a
/// dynamic block cannot be hand-written without a dynamic encoder and because
/// taking them from the C oracle is stronger provenance than generating them
/// would be. Each decodes to a run of zero bytes, which is what makes the expected
/// output expressible as a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DynamicFixture {
    /// The stream, in the liberal hexadecimal `h2b` accepts.
    hex: &'static str,

    /// The label `cover_inflate()` gives it.
    id: &'static str,

    /// How many zero bytes it decodes to.
    decoded_len: usize,
}

/// The three well-formed dynamic-Huffman streams from `cover_inflate()`
/// (`test/infcover.c` L607-L611), with their decoded lengths.
///
/// Their names say what each one reaches: `length extra` exercises a length code
/// with extra bits, `long distance and extra` a distance code with the widest
/// extra field, and `window end` a decode that runs to the very end of a 32 KiB
/// window. Every one is a `BFINAL = 1`, `BTYPE = 10` block, so each also
/// exercises the whole code-length alphabet and both `inflate_table` calls.
///
/// The decoded lengths were read off the C implementation through
/// `zlib.decompressobj(-15)`.
const DYNAMIC_FIXTURES: &[DynamicFixture] = &[
    DynamicFixture {
        hex: "ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f",
        id: "length extra",
        decoded_len: 516,
    },
    DynamicFixture {
        hex: "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        id: "long distance and extra",
        decoded_len: 518,
    },
    DynamicFixture {
        hex: "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
              0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        id: "window end",
        decoded_len: 33025,
    },
];

/// The smallest of [`DYNAMIC_FIXTURES`], for tests that need one dynamic stream
/// rather than all of them.
///
/// `length extra`: fifteen input bytes, 516 output bytes, every one of them zero.
const DYNAMIC_LENGTH_EXTRA: &DynamicFixture = &DYNAMIC_FIXTURES[0];

/// The one of [`DYNAMIC_FIXTURES`] whose matches reach furthest back.
///
/// `long distance and extra`: 24 input bytes, 518 output bytes. Its distances are
/// what make it useful for the window tests -- a decode that satisfies them has
/// genuinely consulted the window rather than the last thing it wrote.
const DYNAMIC_LONG_DISTANCE: &DynamicFixture = &DYNAMIC_FIXTURES[1];

// ---------------------------------------------------------------------------
// Input sources -- the `in()` side of the contract
// ---------------------------------------------------------------------------

/// An input source that never yields anything.
///
/// C's `pull(Z_NULL, ..)`, which resets its cursor and `return 0` with the
/// comment "no input (already provided at `next_in`)" (`test/infcover.c`
/// L453-L456). Both `cover_back()`'s successful call and every `try()` call pass
/// `Z_NULL` as its `in_desc`, so this is the source the whole C vector suite runs
/// with: all of its input arrives staged in `next_in`, and the callback exists
/// only to say "that was all of it".
///
/// ★ Yielding nothing is **failure** on this side. See the polarity rule in the
/// module documentation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Declining {
    /// How many times the decoder asked. C cannot observe this; a Rust test can,
    /// and it is how [`staged_input_is_consumed_before_the_callback_is_asked`]
    /// proves the staged buffer is drained first.
    calls: usize,
}

impl<'i> InflateBackInput<'i> for Declining {
    /// Always [`None`], which is C's `in()` returning zero: failure.
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        self.calls += 1;
        None
    }
}

/// An input source that hands out fixed-size pieces of one buffer and then
/// declines.
///
/// The Rust form of `pull()` with a non-null descriptor: C walks a static array
/// one byte per call (`test/infcover.c` L460) and returns zero once it runs out.
/// The piece size is a parameter here because it selects the decode path --
/// pieces smaller than six bytes keep `have` below the `have >= 6 && left >= 258`
/// threshold at `infback.c` L424, so the fast path never fires and every symbol
/// is decoded the slow way.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pieces<'i> {
    /// The pieces still to hand out, in order.
    queue: Vec<&'i [u8]>,

    /// How far through [`Self::queue`] the source has got.
    next: usize,

    /// How many times the decoder asked.
    calls: usize,
}

impl<'i> Pieces<'i> {
    /// Splits `stream` into runs of `size` bytes, the last possibly shorter.
    ///
    /// # Panics
    ///
    /// If `size` is zero, which would describe a source that makes no progress
    /// and would therefore turn a decode into an infinite loop rather than a
    /// test.
    fn new(stream: &'i [u8], size: usize) -> Self {
        assert!(size > 0, "a zero-byte piece would never make progress");
        Self {
            queue: stream.chunks(size).collect(),
            next: 0,
            calls: 0,
        }
    }

    /// A source whose **first** answer is an empty slice.
    ///
    /// C's `have == 0` test inside `PULL()` (`infback.c` L101-L108) compares only
    /// the returned count, so a callback that returns zero having also set `*buf`
    /// is indistinguishable from one that returned zero and left `buf` alone.
    /// `Some(&[])` is that case, and it must be treated exactly like `None`.
    fn yielding_nothing() -> Self {
        Self {
            queue: vec![&[][..]],
            next: 0,
            calls: 0,
        }
    }
}

impl<'i> InflateBackInput<'i> for Pieces<'i> {
    /// The next queued piece, or [`None`] once the queue is empty -- which, on this
    /// side of the contract, is failure.
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        self.calls += 1;
        let piece = self.queue.get(self.next).copied();
        if piece.is_some() {
            self.next += 1;
        }
        piece
    }
}

// ---------------------------------------------------------------------------
// Output sinks -- the `out()` side of the contract
// ---------------------------------------------------------------------------

/// An output sink that accepts everything and keeps it.
///
/// C's `push(Z_NULL, ..)`, which `return desc != Z_NULL` evaluates to zero for
/// (`test/infcover.c` L467) -- and zero, on this side, means **success**.
///
/// The C harness throws the bytes away because it only ever checks status codes.
/// Keeping them is what lets this suite assert that the decode is *correct* and
/// not merely uncomplaining.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Collector {
    /// Everything handed over, concatenated in the order it arrived.
    written: Vec<u8>,

    /// How many times `out()` ran. `infback.c` calls it once per window fill
    /// (`ROOM()`, L157) plus at most once more in the epilogue (L562-L566), and
    /// **not at all** when the stream produced no output or ended exactly on a
    /// window boundary.
    calls: usize,

    /// The length of the longest run handed over, which `zlib.h` L1177-L1178
    /// caps at the window size.
    longest: usize,
}

impl InflateBackOutput for Collector {
    /// Always [`Ok`], which is C's `out()` returning zero: success. Note the
    /// inversion against [`Declining::next_chunk`] one screen up.
    ///
    /// # Errors
    ///
    /// Never. The signature is the trait's.
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
        self.calls += 1;
        self.longest = self.longest.max(data.len());
        self.written.extend_from_slice(data);
        Ok(())
    }
}

/// An output sink that accepts a fixed number of runs and then refuses.
///
/// C's `push(&strm, ..)`, which `return desc != Z_NULL` evaluates to **one**
/// for, forcing the output-error path (`test/infcover.c` L467, driven at L494).
/// A budget of zero refuses the very first call, which is what `cover_back()`
/// does; a positive budget lets the decode get going before the failure, which is
/// how [`a_mid_stream_output_failure_stops_the_decode`] reaches the `ROOM()`
/// failure at `infback.c` L157-L160 rather than the epilogue failure at L565.
///
/// ★ Refusing is **non-zero** on this side, which is the opposite of the input
/// convention. See the polarity rule in the module documentation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Refusing {
    /// How many runs to accept before refusing.
    budget: usize,

    /// How many times `out()` ran, refusals included.
    calls: usize,

    /// The runs that were accepted, concatenated.
    written: Vec<u8>,
}

/// The two shapes `cover_back()` and its neighbours need.
impl Refusing {
    /// A sink that refuses its first call, like `cover_back()`'s.
    const fn immediately() -> Self {
        Self {
            budget: 0,
            calls: 0,
            written: Vec::new(),
        }
    }

    /// A sink that accepts `budget` runs and refuses the next.
    const fn after(budget: usize) -> Self {
        Self {
            budget,
            calls: 0,
            written: Vec::new(),
        }
    }
}

impl InflateBackOutput for Refusing {
    /// [`Ok`] while the budget lasts, then [`Err`] -- C's `out()` returning
    /// non-zero, which is failure on this side.
    ///
    /// # Errors
    ///
    /// [`OutputFailure`] on the call after the budget is exhausted.
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
        self.calls += 1;
        if self.calls > self.budget {
            return Err(OutputFailure);
        }
        self.written.extend_from_slice(data);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Everything one `inflate_back` call can be asked about afterwards.
///
/// The three fields C writes back through `z_stream` (`infback.c` L565-L568) plus
/// the two things only a Rust test can see: what the sink actually received, and
/// which [`Mode`] the state came to rest in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    /// The status. Never [`ReturnCode::OK`]: `zlib.h` L1204 says outright that
    /// "`inflateBack()` cannot return `Z_OK`".
    code: ReturnCode,

    /// `strm->msg`, set only for a data error.
    msg: Option<&'static str>,

    /// How many input bytes went unused, or [`None`] for C's null `next_in` --
    /// which, per `zlib.h` L1199-L1202, happens **only** when the input callback
    /// failed.
    unused: Option<usize>,

    /// Everything the sink accepted.
    written: Vec<u8>,

    /// How many times the sink was called.
    out_calls: usize,

    /// The longest single run the sink was handed.
    longest_run: usize,

    /// The state's mode when the call returned, which
    /// [`only_the_reduced_mode_subset_is_ever_reached`] checks against the six
    /// arms `infback.c`'s `switch` actually implements.
    mode: Option<Mode>,

    /// What `inflate_back_end` reported for the state afterwards.
    end: ReturnCode,
}

/// The modes `inflate_back` can leave a state in, which is also every mode its
/// `switch` implements.
///
/// `infback.c` L226-L558 has exactly the arms `TYPE`, `STORED`, `TABLE`, `LEN`,
/// `DONE`, `BAD` and `default`. Six modes, not the eight a reader of `inflate.c`
/// might expect: `inflateBack` has **no** `LENLENS` or `CODELENS` arm, because it
/// reads the whole code-length alphabet inline inside `TABLE` (L313-L383) instead
/// of splitting it into resumable states -- it never has to suspend
/// mid-alphabet. It equally has no header modes (`HEAD`, `FLAGS`, `TIME`, `OS`,
/// `EXLEN`, `EXTRA`, `NAME`, `COMMENT`, `HCRC`), no trailer modes (`CHECK`,
/// `LENGTH`), no dictionary modes (`DICTID`, `DICT`), no `SYNC` and no `MEM`,
/// because a raw stream has no container and this decoder never suspends.
///
/// `Mode::ALL` has 32 entries, so this names six of them and the other 26 are
/// asserted unreachable.
const REACHABLE_MODES: [Mode; 6] = [
    Mode::Type,
    Mode::Stored,
    Mode::Table,
    Mode::Len,
    Mode::Done,
    Mode::Bad,
];

/// Returns a caller-supplied window of `len` bytes, pre-filled with
/// [`WINDOW_SENTINEL`].
///
/// Never zeroed, on purpose: see [`WINDOW_SENTINEL`].
fn sentinel_window(len: usize) -> Vec<u8> {
    vec![WINDOW_SENTINEL; len]
}

/// Runs one complete `inflate_back` call over `window` with all of `staged`
/// pre-supplied, a [`Declining`] input callback and a [`Collector`] output
/// callback, then ends the state.
///
/// This is `try()`'s `inflateBack` leg (`test/infcover.c` L556-L570) turned into
/// a function: `inflateBackInit(&strm, 15, win)`, stage the whole stream in
/// `next_in`, `inflateBack(&strm, pull, Z_NULL, push, Z_NULL)`,
/// `inflateBackEnd(&strm)`.
///
/// # Panics
///
/// If `inflate_back_init` rejects `window_bits` or `window`, which for every call
/// site below would mean the fixture is wrong rather than the decoder.
fn decode(window: &mut [u8], window_bits: i32, staged: &[u8]) -> Outcome {
    let mut state = inflate_back_init(window_bits, window, GlobalAllocator)
        .expect("inflate_back_init rejected a window this test built for it");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(staged), &mut source, &mut sink);
    let (code, msg, unused) = (result.code, result.msg, result.next_in.map(<[u8]>::len));
    let mode = Mode::from_raw(state.mode_tag());
    let end = inflate_back_end(state);
    Outcome {
        code,
        msg,
        unused,
        written: sink.written,
        out_calls: sink.calls,
        longest_run: sink.longest,
        mode,
        end,
    }
}

/// [`decode`] with the harness's own window size and `windowBits`.
fn decode_as_harness(staged: &[u8]) -> Outcome {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    decode(&mut window, HARNESS_WINDOW_BITS, staged)
}

/// Reads one scalar out of `InflateState`'s [`Debug`] rendering.
///
/// The state's fields are `pub(crate)`, so an integration test cannot read
/// `dmax`, `wsize` or `wbits` directly -- but `impl Debug for InflateState`
/// deliberately publishes exactly the "resumability scalars and buffer shapes",
/// which is the documented route to observing them from outside the crate. This
/// is how [`dmax_is_fixed_at_thirty_two_kibibytes_for_every_window_size`] can
/// assert `infback.c` L56's `state->dmax = 32768U` at all.
///
/// # Panics
///
/// If `field` does not appear in the rendering, which would mean the `Debug` impl
/// stopped publishing it and the assertion resting on it has silently stopped
/// checking anything.
fn debug_scalar<'a, A: Allocator<'a>>(state: &InflateState<'a, A>, field: &str) -> String {
    let rendered = format!("{state:?}");
    let needle = format!("{field}: ");
    let tail = rendered
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("InflateState's Debug rendering no longer publishes `{field}`"));
    tail.split(',').next().unwrap_or(tail).trim().to_string()
}

/// A minimal RFC 1951 encoder, used to build fixtures rather than to compress
/// anything.
///
/// ★ **Why this exists instead of a call into `crate::deflate`.** This file tests the
/// decoder, and a decoder test whose fixtures come from the encoder in the same port
/// proves less than one whose fixtures are independent of it: a shared
/// misunderstanding of the format would cancel out and the suite would pass. So the
/// literal fixtures here come from the **C reference** and everything else is emitted
/// by this writer, which implements only what RFC 1951 says and consults no part of
/// the port. The two are cross-checked against each other by
/// [`the_fixture_writer_agrees_with_the_c_reference`], which reproduces a
/// C-generated stream bit for bit.
///
/// The second reason is control. A generic compressor decides for itself which
/// matches to emit, so a test that needs "a match at distance 300, decoded with a
/// 256-byte window" cannot ask for one. Here the symbol sequence is written out
/// explicitly, which is what makes the window tests in section 5 possible.
///
/// # Bit order
///
/// RFC 1951 §3.1.1 gives three rules, and getting any of them wrong produces a
/// stream that fails to decode rather than one that decodes wrongly:
///
/// 1. Data elements are packed into bytes starting with the byte's **least**
///    significant bit.
/// 2. Non-Huffman values are packed starting with their least significant bit --
///    [`Self::bits`].
/// 3. Huffman codes are packed starting with their **most** significant bit --
///    [`Self::code`], which is why it reverses before it emits, exactly as
///    `bi_reverse` (`trees.c` L154) does for the same reason.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BlockWriter {
    /// The stream so far, whole bytes only.
    out: Vec<u8>,

    /// Bits emitted but not yet a whole byte, held least-significant-first. C's
    /// `bi_buf`.
    accumulator: u32,

    /// How many bits of [`Self::accumulator`] are live. C's `bi_valid`.
    live: u32,
}

/// The base length of each length code 257..=285, RFC 1951 §3.2.5.
const LENGTH_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];

/// How many extra bits each length code carries, RFC 1951 §3.2.5.
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// The base distance of each distance code 0..=29, RFC 1951 §3.2.5.
const DISTANCE_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];

/// How many extra bits each distance code carries, RFC 1951 §3.2.5.
const DISTANCE_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The largest `LEN` a single stored block may declare, RFC 1951 §3.2.4.
const MAX_STORED_BLOCK: usize = 65535;

/// The emitters, in the order a block uses them: bit primitives, then the
/// fixed-Huffman alphabet, then whole blocks.
impl BlockWriter {
    /// A writer positioned at the start of a stream.
    fn new() -> Self {
        Self::default()
    }

    /// Emits `count` low bits of `value`, least significant first.
    ///
    /// C's `send_bits` (`trees.c` L253), reduced to the case where the accumulator
    /// cannot overflow because nothing here emits more than sixteen bits at a time.
    ///
    /// # Panics
    ///
    /// If `count` exceeds sixteen, which no RFC 1951 element does.
    fn bits(&mut self, value: u32, count: u32) {
        assert!(count <= 16, "no DEFLATE element is wider than 16 bits");
        let masked = value & ((1_u32 << count) - 1);
        assert_eq!(masked, value, "{value} does not fit in {count} bits");
        self.accumulator |= value << self.live;
        self.live += count;
        while self.live >= 8 {
            // Narrowing to the low eight bits is exactly what C's `put_byte` does.
            let byte = u8::try_from(self.accumulator & 0xff).expect("masked to one byte");
            self.out.push(byte);
            self.accumulator >>= 8;
            self.live -= 8;
        }
    }

    /// Emits a Huffman code: `count` low bits of `value`, **most** significant first.
    ///
    /// The reversal is `bi_reverse` (`trees.c` L154), and it is required by RFC 1951
    /// §3.1.1's rule that "Huffman codes are packed starting with the most
    /// significant bit of the code".
    fn code(&mut self, value: u32, count: u32) {
        let mut reversed = 0_u32;
        let mut remaining = value;
        for _ in 0..count {
            reversed = (reversed << 1) | (remaining & 1);
            remaining >>= 1;
        }
        self.bits(reversed, count);
    }

    /// Pads to the next byte boundary with zero bits.
    ///
    /// C's `bi_windup` (`trees.c` L181), and the `BYTEBITS()` the stored-block arm
    /// performs at `infback.c` L268 before reading `LEN`.
    fn align(&mut self) {
        if self.live % 8 != 0 {
            self.bits(0, 8 - (self.live % 8));
        }
    }

    /// Opens a fixed-Huffman block: one `BFINAL` bit then `BTYPE = 01`
    /// (RFC 1951 §3.2.3).
    fn begin_fixed_block(&mut self, last: bool) {
        self.bits(u32::from(last), 1);
        self.bits(0b01, 2);
    }

    /// Emits one literal/length symbol using the fixed code table of RFC 1951 §3.2.6.
    ///
    /// The four ranges of that table, verbatim: 0..=143 are eight bits starting at
    /// `00110000`, 144..=255 are nine bits starting at `110010000`, 256..=279 are
    /// seven bits starting at `0000000`, and 280..=287 are eight bits starting at
    /// `11000000`.
    ///
    /// # Panics
    ///
    /// If `symbol` exceeds 287, which is not a literal/length symbol.
    fn fixed_symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0b0011_0000 + symbol, 8),
            144..=255 => self.code(0b1_1001_0000 + (symbol - 144), 9),
            256..=279 => self.code(symbol - 256, 7),
            280..=287 => self.code(0b1100_0000 + (symbol - 280), 8),
            _ => panic!("{symbol} is not a literal/length symbol"),
        }
    }

    /// Emits one literal byte.
    fn literal(&mut self, byte: u8) {
        self.fixed_symbol(u32::from(byte));
    }

    /// Emits the end-of-block symbol, 256.
    fn end_of_block(&mut self) {
        self.fixed_symbol(256);
    }

    /// Emits a length symbol and its extra bits, RFC 1951 §3.2.5.
    ///
    /// The code chosen is the **last** whose base does not exceed `length`, which is
    /// what makes a length of 258 come out as symbol 285 with no extra bits rather
    /// than as symbol 284 with an extra value of 31. Both are legal; the reference
    /// emits 285, and matching it is the point of this writer.
    ///
    /// # Panics
    ///
    /// If `length` is outside `3 ..= 258`, the match lengths RFC 1951 §3.2.5 defines.
    fn length(&mut self, length: u32) {
        assert!(
            (3..=258).contains(&length),
            "{length} is not a DEFLATE match length"
        );
        let index = LENGTH_BASE
            .iter()
            .rposition(|&base| base <= length)
            .expect("3 is the smallest base");
        self.fixed_symbol(257 + u32::try_from(index).expect("index below 29"));
        let extra = LENGTH_EXTRA[index];
        if extra > 0 {
            self.bits(length - LENGTH_BASE[index], extra);
        }
    }

    /// Emits a distance code and its extra bits, RFC 1951 §3.2.5.
    ///
    /// In a fixed-Huffman block the distance alphabet is a flat five-bit code
    /// (§3.2.6: "distance codes 0-31 are represented by (fixed-length) 5-bit codes"),
    /// so the code is the index itself.
    ///
    /// # Panics
    ///
    /// If `distance` is outside `1 ..= 32768`, the range RFC 1951 §3.2.5 defines.
    fn distance(&mut self, distance: u32) {
        assert!(
            (1..=32768).contains(&distance),
            "{distance} is not a DEFLATE match distance"
        );
        let index = DISTANCE_BASE
            .iter()
            .rposition(|&base| base <= distance)
            .expect("1 is the smallest base");
        self.code(u32::try_from(index).expect("index below 30"), 5);
        let extra = DISTANCE_EXTRA[index];
        if extra > 0 {
            self.bits(distance - DISTANCE_BASE[index], extra);
        }
    }

    /// Emits a length/distance pair, that is one back-reference.
    fn back_reference(&mut self, length: u32, distance: u32) {
        self.length(length);
        self.distance(distance);
    }

    /// Emits `data` as one or more stored blocks, RFC 1951 §3.2.4.
    ///
    /// Three bits of header, then padding to the next byte boundary, then `LEN` and
    /// its one's complement `NLEN` as little-endian sixteen-bit values, then the bytes
    /// verbatim. `LEN` caps at [`MAX_STORED_BLOCK`], so a longer payload becomes
    /// several blocks and only the last carries `BFINAL`.
    ///
    /// An empty payload still produces one block, declaring `LEN = 0`, because that is
    /// how a zero-byte stored stream is spelled.
    fn stored(&mut self, data: &[u8], last: bool) {
        let mut chunks = data.chunks(MAX_STORED_BLOCK).peekable();
        if chunks.peek().is_none() {
            self.stored_chunk(&[], last);
            return;
        }
        while let Some(chunk) = chunks.next() {
            self.stored_chunk(chunk, last && chunks.peek().is_none());
        }
    }

    /// One stored block, whose length must already be within `LEN`'s range.
    fn stored_chunk(&mut self, chunk: &[u8], last: bool) {
        let len = u16::try_from(chunk.len()).expect("chunked to LEN's range");
        self.bits(u32::from(last), 1);
        self.bits(0b00, 2);
        self.align();
        self.out.extend_from_slice(&len.to_le_bytes());
        self.out.extend_from_slice(&(!len).to_le_bytes());
        self.out.extend_from_slice(chunk);
    }

    /// Appends a complete, already-encoded block sequence at the current bit position.
    ///
    /// A DEFLATE stream is a bit sequence, not a byte sequence, so a self-contained
    /// block sequence can be spliced in at any bit offset simply by re-emitting its
    /// bytes eight bits at a time. Whatever padding its own final byte carried follows
    /// its last end-of-block symbol, which a decoder that has already seen `BFINAL`
    /// never reads.
    ///
    /// This is what lets a fixture combine hand-written stored and fixed blocks with a
    /// **dynamic** block taken from the C harness, without this file having to
    /// implement a dynamic encoder.
    fn splice(&mut self, encoded: &[u8]) {
        for &byte in encoded {
            self.bits(u32::from(byte), 8);
        }
    }

    /// Finishes the stream, padding the last partial byte with zeros.
    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

/// Encodes `payload` as a single final stored block.
fn stored_stream(payload: &[u8]) -> Vec<u8> {
    let mut writer = BlockWriter::new();
    writer.stored(payload, true);
    writer.finish()
}

/// Encodes `payload` as a single final fixed-Huffman block of literals only.
///
/// No back-reference is emitted, which is deliberate: it makes the encoding valid at
/// **every** window size, since a stream with no distances cannot reach outside a
/// window however small. That is what lets the corpus round-trip run at
/// `windowBits = 8` and `windowBits = 15` over the same payload.
fn fixed_literal_stream(payload: &[u8]) -> Vec<u8> {
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in payload {
        writer.literal(byte);
    }
    writer.end_of_block();
    writer.finish()
}

/// The `BFINAL` bit and the two `BTYPE` bits of a stream's first block, RFC 1951
/// §3.2.3.
///
/// The first block header always begins at bit 0 of byte 0, so it is readable
/// without a bit reader: bit 0 is `BFINAL` and bits 1-2 are `BTYPE`, where 0 is
/// stored, 1 fixed and 2 dynamic (`doc/rfc1951.txt` §3.2.3).
///
/// # Panics
///
/// If `stream` is empty, which is not a DEFLATE stream at all.
fn first_block_header(stream: &[u8]) -> (bool, u8) {
    assert!(!stream.is_empty(), "an empty stream has no block header");
    let first = stream[0];
    (first & 1 == 1, (first >> 1) & 0b11)
}

// ===========================================================================
// 0. The fixture writer, checked against the C reference before it is trusted
// ===========================================================================

/// ★ [`BlockWriter`] reproduces C-generated streams bit for bit.
///
/// A fixture generator that is wrong in the same way the decoder is wrong proves
/// nothing, so this runs first and pins the writer to streams the reference
/// produced. Three cross-checks, each exact rather than merely round-tripping:
///
/// 1. **A stored block.** `zlib.compressobj(0, DEFLATED, -15)` on
///    `"hello, hello!"` emits `01 0d 00 f2 ff` followed by the thirteen bytes
///    verbatim: three header bits, five zero bits of padding to the byte boundary,
///    then `LEN` and `NLEN` little-endian (RFC 1951 §3.2.4).
/// 2. **A fixed-Huffman block with a match.** [`HELLO_FIXED`] is the level-6
///    encoding of the same payload. Its symbol sequence was recovered by decoding
///    those bytes rather than guessed at, and it is worth knowing that the obvious
///    guess is wrong: the encoder emits **eight** literals and then a length-**4**
///    match at distance 7, not seven literals and a length-5 match. Both describe
///    the same output, and which one `deflate_slow`'s lazy-match logic settles on is
///    exactly the kind of non-normative choice this port has to reproduce. Writing
///    the sequence the reference chose has to give the twelve bytes it produced.
/// 3. **A back-reference reaching too far.** One literal then a length-3 match at
///    distance 5 is `4b 04 12 00`, the invalid stream whose rejection commit
///    `09a1572` restored. Reproducing it byte for byte is what lets
///    [`a_hand_written_reference_reaching_past_the_written_window_is_rejected`]
///    claim to be testing that specific check.
///
/// Getting any of RFC 1951 §3.1.1's three bit-order rules wrong changes at least one
/// of these three, so this single test covers all of them.
#[test]
fn the_fixture_writer_agrees_with_the_c_reference() {
    // 1. The stored form, from `zlib.compressobj(0, zlib.DEFLATED, -15)`.
    const HELLO_STORED: &[u8] = &[
        0x01, 0x0d, 0x00, 0xf2, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68, 0x65, 0x6c,
        0x6c, 0x6f, 0x21,
    ];
    assert_eq!(
        stored_stream(HELLO),
        HELLO_STORED,
        "the stored-block header, padding and LEN/NLEN must match the reference"
    );

    // 2. The fixed form, symbol for symbol: "hello, " then "hello" as a match at
    //    distance 7, then '!' and the end-of-block code.
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in b"hello, h" {
        writer.literal(byte);
    }
    writer.back_reference(4, 7);
    writer.literal(b'!');
    writer.end_of_block();
    assert_eq!(
        writer.finish(),
        HELLO_FIXED,
        "the fixed literal/length codes, the distance code and the bit order must \
         all match the reference"
    );

    // 3. The too-far-back form, which is also fixture 3's provenance.
    assert_eq!(
        hand_written_too_far_back(),
        TOO_FAR_BACK_BY_HAND,
        "one literal then a length-3 match at distance 5"
    );

    // And the property the writer exists for: whatever it emits, the decoder reads
    // back unchanged. Checked over a payload that spans the whole byte range so that
    // both halves of the fixed literal table -- the eight-bit codes below 144 and the
    // nine-bit codes above -- are exercised.
    let every_byte: Vec<u8> = (0..=u8::MAX).collect();
    for (label, stream) in [
        ("stored", stored_stream(&every_byte)),
        ("fixed literals", fixed_literal_stream(&every_byte)),
    ] {
        let outcome = decode_as_harness(&stream);
        assert_eq!(outcome.code, ReturnCode::STREAM_END, "{label}");
        assert_eq!(outcome.written, every_byte, "{label}");
    }
}

/// One literal then a length-3 back-reference at distance 5, in a single final
/// fixed-Huffman block.
///
/// Only one byte has been written when the reference is read, so it reaches five
/// bytes before anything valid -- which `infback.c` L516-L521 must reject. Written
/// out here rather than inline so that the writer's self-test and the rejection test
/// are provably looking at the same stream.
fn hand_written_too_far_back() -> Vec<u8> {
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    writer.literal(b'a');
    writer.back_reference(3, 5);
    writer.end_of_block();
    writer.finish()
}

/// What [`hand_written_too_far_back`] must produce, verified against the C
/// implementation through `zlib.decompressobj(-15)`, which reports
/// `"invalid distance too far back"` for it.
const TOO_FAR_BACK_BY_HAND: &[u8] = &[0x4b, 0x04, 0x12, 0x00];

// ===========================================================================
// 1. Initialisation and teardown -- `inflateBackInit_` and `inflateBackEnd`
// ===========================================================================

/// `windowBits` in `8 ..= 15` is accepted, each with a window of the matching
/// size.
///
/// `infback.c` L22-L23: "windowBits is in the range 8..15, and window is a
/// user-supplied window and output buffer that is 2**windowBits bytes." All eight
/// values are exercised, and each state is then actually *used*, because an
/// initialisation that succeeds and produces an unusable state would pass a
/// weaker test.
#[test]
fn every_window_bits_from_eight_through_fifteen_is_accepted() {
    for bits in MIN_BACK_WINDOW_BITS..=MAX_BACK_WINDOW_BITS {
        let size = 1_usize << bits;
        let mut window = sentinel_window(size);
        let outcome = decode(&mut window, bits, HELLO_FIXED);
        assert_eq!(
            outcome.code,
            ReturnCode::STREAM_END,
            "windowBits {bits} should decode a complete stream"
        );
        assert_eq!(outcome.written, HELLO, "windowBits {bits} output");
        assert_eq!(outcome.end, ReturnCode::OK, "windowBits {bits} teardown");
    }
}

/// `windowBits` outside `8 ..= 15` is rejected with `Z_STREAM_ERROR`.
///
/// `infback.c` L33-L35 rejects `windowBits < 8 || windowBits > 15` with
/// `Z_STREAM_ERROR`, in the same condition that rejects a null `strm` and a null
/// `window`. The window handed over here is always the full 32768 bytes, so a
/// rejection can only be about the exponent.
///
/// The two extremes are included because they are the values that would overflow
/// a naive `1 << windowBits`: `i32::MAX` shifts past any word size, and
/// `i32::MIN` negated does not fit in an `i32` at all. The negative and
/// `+16`/`+32` forms are listed here as well, even though
/// [`inflate_back_init_accepts_no_container_request_unlike_inflate_init2`] is
/// where their significance is argued, so that this test alone covers the whole
/// rejected set.
#[test]
fn window_bits_outside_eight_through_fifteen_is_a_stream_error() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for bits in [
        7_i32,
        16,
        0,
        1,
        -1,
        -8,
        -15,
        16 + 15,
        32 + 15,
        31,
        32,
        i32::MIN,
        i32::MAX,
    ] {
        let rejected = inflate_back_init(bits, &mut window, GlobalAllocator);
        assert_eq!(
            rejected.err(),
            Some(ReturnCode::STREAM_ERROR),
            "windowBits {bits} must be rejected"
        );
    }
}

/// ★ `inflate_back_init` is raw-only, so the negative and `+16`/`+32` forms that
/// `inflate_init2` accepts are errors here.
///
/// This asymmetry is real and easy to miss, because the two functions take an
/// argument of the same name and the same type. `inflate_init2` decodes a
/// *container request* out of `windowBits` -- `8 ..= 15` for zlib, `-15 ..= -8`
/// for raw, `+16` for gzip, `+32` for automatic detection -- whereas
/// `inflateBack` decodes raw DEFLATE and nothing else (`zlib.h` L1155-L1162), so
/// `infback.c` L34 tests the bare range and rejects everything else.
///
/// Asserting it as a *contrast* rather than as another rejection is the point:
/// each value below is checked to be accepted by `inflate_init2` and rejected by
/// `inflate_back_init` in the same iteration, so the test fails if either half
/// ever drifts towards the other.
#[test]
fn inflate_back_init_accepts_no_container_request_unlike_inflate_init2() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for bits in [-15_i32, -12, -8, 16 + 15, 32 + 15, 0] {
        let forward: Result<InflateState<'_, GlobalAllocator>, ReturnCode> =
            InflateState::new(InflateConfig::new(bits), GlobalAllocator);
        assert!(
            forward.is_ok(),
            "inflate_init2 should accept windowBits {bits}"
        );
        drop(forward);

        let backward = inflate_back_init(bits, &mut window, GlobalAllocator);
        assert_eq!(
            backward.err(),
            Some(ReturnCode::STREAM_ERROR),
            "inflate_back_init must reject the container request in windowBits {bits}"
        );
    }
}

/// A window shorter than `2**windowBits` is rejected.
///
/// The window's length is part of this interface's contract in a way it is not
/// for `inflate()`, which allocates its own. C cannot check it -- it receives a
/// bare `unsigned char *` and simply trusts that `2**windowBits` bytes are there,
/// which is why `zlib.h` L1163-L1166 makes it the caller's stated obligation. A
/// `&mut [u8]` carries its length, so the port checks what C can only document,
/// and reports the same `Z_STREAM_ERROR` the rest of L33-L35 reports.
#[test]
fn a_window_shorter_than_the_requested_size_is_rejected() {
    for bits in MIN_BACK_WINDOW_BITS..=MAX_BACK_WINDOW_BITS {
        let short = (1_usize << bits) - 1;
        let mut window = sentinel_window(short);
        let rejected = inflate_back_init(bits, &mut window, GlobalAllocator);
        assert_eq!(
            rejected.err(),
            Some(ReturnCode::STREAM_ERROR),
            "windowBits {bits} with a {short}-byte window must be rejected"
        );
    }
    // An empty window is the degenerate case of the same rule.
    let mut nothing: Vec<u8> = Vec::new();
    assert_eq!(
        inflate_back_init(MIN_BACK_WINDOW_BITS, &mut nothing, GlobalAllocator).err(),
        Some(ReturnCode::STREAM_ERROR),
        "an empty window must be rejected"
    );
}

/// A window longer than `2**windowBits` is accepted, and only that prefix is
/// used.
///
/// C reads `state->wsize` bytes and never learns how large the caller's buffer
/// really was, so a generous caller is simply not C's problem. The port keeps that
/// behaviour by truncating the borrow to exactly `1 << window_bits`, which is
/// observable twice over: `wsize` reports the requested size rather than the
/// buffer's, and the bytes past the prefix keep their sentinel value.
#[test]
fn a_window_longer_than_the_requested_size_is_truncated_to_it() {
    const BITS: i32 = 8;
    const REQUESTED: usize = 1 << BITS;
    let mut window = sentinel_window(REQUESTED + 500);

    let state = inflate_back_init(BITS, &mut window, GlobalAllocator).expect("init");
    assert_eq!(debug_scalar(&state, "wsize"), REQUESTED.to_string());
    assert_eq!(debug_scalar(&state, "wbits"), BITS.to_string());
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    let outcome = decode(&mut window, BITS, HELLO_FIXED);
    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(outcome.written, HELLO);
    assert!(
        window[REQUESTED..]
            .iter()
            .all(|&byte| byte == WINDOW_SENTINEL),
        "bytes past 2**windowBits must be untouched"
    );
}

/// ★ `dmax` is fixed at 32768 whatever the window size is.
///
/// `infback.c` L56 assigns `state->dmax = 32768U` unconditionally, immediately
/// before `state->wsize = 1U << windowBits` at L58. It is *not* derived from
/// `windowBits`, which is worth pinning because deriving it would look like an
/// obvious simplification and would change which streams are accepted under
/// `INFLATE_STRICT`.
///
/// Observed through the state's `Debug` rendering, which is the documented route
/// for a scalar the crate keeps `pub(crate)`; see [`debug_scalar`].
#[test]
fn dmax_is_fixed_at_thirty_two_kibibytes_for_every_window_size() {
    assert_eq!(DMAX_DEFAULT, 32768, "infback.c L56");
    for bits in MIN_BACK_WINDOW_BITS..=MAX_BACK_WINDOW_BITS {
        let mut window = sentinel_window(1_usize << bits);
        let state = inflate_back_init(bits, &mut window, GlobalAllocator).expect("init");
        assert_eq!(
            debug_scalar(&state, "dmax"),
            DMAX_DEFAULT.to_string(),
            "dmax must not follow windowBits {bits}"
        );
        assert_eq!(
            debug_scalar(&state, "wsize"),
            (1_usize << bits).to_string(),
            "wsize must follow windowBits {bits}"
        );
        assert_eq!(inflate_back_end(state), ReturnCode::OK);
    }
}

/// A freshly initialised state starts with no valid history and a sane decoder.
///
/// `infback.c` L60-L62 sets `wnext = 0`, `whave = 0` and `sane = 1`. `whave` is
/// the load-bearing one: it is what makes an unwritten window unreadable, and
/// therefore what the too-far-back check at L516-L521 tests against. A state that
/// started life claiming history it does not have would accept invalid streams and
/// emit whatever the caller's buffer happened to contain -- which is precisely the
/// defect commit `09a1572` fixed.
#[test]
fn a_fresh_state_claims_no_window_history() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    assert_eq!(debug_scalar(&state, "whave"), "0", "infback.c L61");
    assert_eq!(debug_scalar(&state, "wnext"), "0", "infback.c L60");
    assert_eq!(debug_scalar(&state, "sane"), "true", "infback.c L62");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// Ending a freshly initialised state reports `Z_OK` and hands the window back
/// untouched.
///
/// `inflateBackEnd` (`infback.c` L572-L579) frees the state and nulls
/// `strm->state`; it does **not** free the window, because the caller owns it.
/// The Rust form takes the state by value, so the borrow simply ends -- and the
/// proof that nothing took ownership is that the caller can still read *and*
/// write every byte afterwards, which the code below does.
#[test]
fn ending_a_freshly_initialised_state_reports_ok_and_returns_the_window() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    assert!(
        window.iter().all(|&byte| byte == WINDOW_SENTINEL),
        "initialisation must not write to the caller's window"
    );
    window[0] = 0x5a;
    assert_eq!(window[0], 0x5a, "the caller's window is writable again");
}

/// A state that was not built for `inflateBack` is a `Z_STREAM_ERROR`.
///
/// This is the reachable half of `cover_back()`'s three null-pointer steps
/// (`test/infcover.c` L479-L482): `inflateBackInit(Z_NULL, ..)`,
/// `inflateBack(Z_NULL, ..)` and `inflateBackEnd(Z_NULL)` all report
/// `Z_STREAM_ERROR`. A null `z_streamp` is unrepresentable in the safe core --
/// `&mut InflateState` proves non-nullness by existing -- so those exact three
/// calls are the facade's to make, and `crates/libz-rs-sys/tests` is where they
/// belong.
///
/// What *is* reachable, and is the same failure in substance, is handing
/// `inflate_back` a state that carries no `inflateBack` window: one built by
/// `InflateState::new` for `inflate()`, whose window is allocated lazily and is
/// absent at first. C in that position would proceed with `put == NULL` and
/// `left == 0` and spin inside `ROOM()` forever; refusing the call is the same
/// "the state was not initialized" verdict `zlib.h` L1203 describes, and it
/// terminates.
///
/// Note that the mode is untouched: the refusal happens before the entry reset at
/// `infback.c` L215, so the state is left exactly as the caller handed it over.
#[test]
fn a_state_not_built_for_inflate_back_is_a_stream_error() {
    let mut state: InflateState<'_, GlobalAllocator> =
        InflateState::new(InflateConfig::new(-MAX_BACK_WINDOW_BITS), GlobalAllocator)
            .expect("inflate_init2");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(EMPTY_FIXED), &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::STREAM_ERROR);
    assert_eq!(result.msg, None, "a stream error sets no message");
    assert_eq!(
        result.next_in.map(<[u8]>::len),
        Some(EMPTY_FIXED.len()),
        "the staged input is handed straight back"
    );
    assert_eq!(sink.calls, 0, "the output callback is never reached");
    assert_eq!(source.calls, 0, "the input callback is never reached");
    assert_eq!(
        Mode::from_raw(state.mode_tag()),
        Some(Mode::Head),
        "the refusal precedes the entry reset at infback.c L215"
    );
}

// ===========================================================================
// 2. Callback polarity -- the centrepiece
// ===========================================================================
//
// Restating the rule once more, at the top of the tests that check it, because
// every assertion in this section is meaningless if it is read backwards
// (`infback.c` L182-L185, `zlib.h` L1134-L1136):
//
//     in()  fails by returning ZERO      -> here: `None` or `Some(&[])`
//     out() fails by returning NON-ZERO  -> here: `Err(OutputFailure)`
//
// and both failures surface as `Z_BUF_ERROR`, told apart by whether
// `InflateBackResult::next_in` is `None`.

/// The staged buffer is drained before the input callback is asked for anything.
///
/// `zlib.h` L1182-L1190 documents the staged path "for convenience":
/// `inflateBack` uses `strm->next_in` and `strm->avail_in` first and only then
/// calls `in()`. `infback.c` L218-L219 seeds `next`/`have` from the stream, and
/// `PULL()` at L101 only consults the callback `if (have == 0)`.
///
/// The observable consequence is that a complete staged stream never reaches the
/// callback at all -- which is why the whole C vector suite can pass `Z_NULL` as
/// `in_desc` and still decode.
#[test]
fn staged_input_is_consumed_before_the_callback_is_asked() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_eq!(sink.written, HELLO);
    assert_eq!(
        source.calls, 0,
        "a complete staged stream must never reach in()"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// An input callback that declines *after* a complete stream reports
/// `Z_STREAM_END`.
///
/// Returning nothing is failure on this side, but a failure that arrives once the
/// stream has already ended never gets consulted: `infback.c` reaches `DONE` at
/// L546 and leaves. So "the source ran dry" and "the decode succeeded" are not in
/// tension, and this is the ordinary shape of a successful callback-driven decode.
#[test]
fn an_input_callback_that_declines_after_a_complete_stream_reports_stream_end() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(HELLO_FIXED, 4);
    let result = inflate_back(&mut state, None, &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_eq!(sink.written, HELLO);
    assert!(
        source.calls >= 1,
        "with nothing staged the callback must be asked at least once"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// An input callback that declines *before* the stream ends reports
/// `Z_BUF_ERROR`, with `next_in` nulled.
///
/// `infback.c` L102-L107: when `in()` yields nothing, `PULL()` sets
/// `next = Z_NULL`, `ret = Z_BUF_ERROR` and jumps to `inf_leave`. `zlib.h`
/// L1199-L1202 makes the nulled pointer the documented discriminator: it "will be
/// `Z_NULL` only if `in()` returned an error".
///
/// The stream fed here is [`HELLO_FIXED`] with its last byte withheld, so the
/// decode is genuinely mid-symbol when the source runs out.
#[test]
fn an_input_callback_that_declines_after_a_truncated_stream_is_a_buffer_error() {
    let truncated = &HELLO_FIXED[..HELLO_FIXED.len() - 1];
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(truncated, 4);
    let result = inflate_back(&mut state, None, &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::BUF_ERROR);
    assert_eq!(
        result.next_in, None,
        "an input failure nulls next_in (zlib.h L1199-L1202)"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// An input callback that declines immediately, with nothing staged, is a
/// `Z_BUF_ERROR`.
///
/// The shortest possible route through `PULL()`: the very first `TYPE` state needs
/// three bits, `have` is zero because nothing was staged, and the callback yields
/// nothing. `zlib.h` L1170-L1172 spells out this exact case -- when there is no
/// input available "`in()` must return zero -- `buf` is ignored in that case --
/// and `inflateBack()` will return a buffer error".
///
/// The state comes to rest in `TYPE`, having decoded nothing, which is the mode
/// the entry reset at `infback.c` L215 put it in.
#[test]
fn an_input_callback_that_declines_immediately_is_a_buffer_error() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, None, &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::BUF_ERROR);
    assert_eq!(result.next_in, None);
    assert_eq!(source.calls, 1, "asked once, told no, gave up");
    assert_eq!(sink.calls, 0, "nothing was decoded, so nothing was written");
    assert_eq!(
        Mode::from_raw(state.mode_tag()),
        Some(Mode::Type),
        "infback.c L215 left the state in TYPE"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// An input callback that yields an **empty** chunk has failed, exactly as one
/// that yields nothing has.
///
/// C's `PULL()` compares only the count `in()` returned (`infback.c` L103), so a
/// callback that returns zero having also set `*buf` is indistinguishable from one
/// that returned zero and left `buf` alone. `Some(&[])` is that case, and treating
/// it as "ask again" instead would be an infinite loop -- which is why this is a
/// test and not a comment.
///
/// `Some(&[])` staged in `next_in` is the same story from the other direction:
/// `zlib.h` L1186-L1188 permits a valid pointer with `avail_in == 0`, and
/// `infback.c` L218-L219 gives `have = 0` for it, so the callback is consulted
/// immediately.
#[test]
fn an_input_callback_yielding_an_empty_chunk_is_the_same_as_declining() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::yielding_nothing();
    let from_callback = inflate_back(&mut state, None, &mut source, &mut sink);
    assert_eq!(from_callback.code, ReturnCode::BUF_ERROR);
    assert_eq!(from_callback.next_in, None);
    assert_eq!(source.calls, 1, "an empty answer ends the conversation");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let from_staged = inflate_back(&mut state, Some(&[]), &mut source, &mut sink);
    assert_eq!(from_staged.code, ReturnCode::BUF_ERROR);
    assert_eq!(from_staged.next_in, None);
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// ★ An output callback that **fails** is a `Z_BUF_ERROR`, and `next_in` survives.
///
/// This is `cover_back()` step six (`test/infcover.c` L491-L495): the same
/// `push()` callback that returned zero for a null descriptor returns one for a
/// non-null one, and `inflateBack` reports `Z_BUF_ERROR`.
///
/// Two things are asserted beyond the status. First, `out()` really did run --
/// [`ONE_ZERO_BYTE_FIXED`] decodes to one byte, so the epilogue at `infback.c`
/// L562-L566 has something to flush, and a failure there is what L563-L565 turns
/// into `Z_BUF_ERROR`. Second, `next_in` is `Some`, not `None`: the input side did
/// not fail, and `zlib.h` L1199-L1202 requires the two to stay
/// distinguishable.
#[test]
fn an_output_callback_that_fails_is_a_buffer_error() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Refusing::immediately();
    let mut source = Declining::default();
    let result = inflate_back(
        &mut state,
        Some(ONE_ZERO_BYTE_FIXED),
        &mut source,
        &mut sink,
    );

    assert_eq!(result.code, ReturnCode::BUF_ERROR);
    assert_eq!(
        result.next_in.map(<[u8]>::len),
        Some(0),
        "an output failure leaves next_in non-null (zlib.h L1199-L1202)"
    );
    assert_eq!(sink.calls, 1, "out() ran and refused");
    assert!(sink.written.is_empty(), "a refusal accepts nothing");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// ★ A failing epilogue flush downgrades success and nothing else.
///
/// The exact shape of `inf_leave` (`infback.c` L560-L566) is easy to get subtly
/// wrong, and the `&&` is where:
///
/// ```c
/// if (left < state->wsize) {
///     if (out(out_desc, state->window, state->wsize - left) &&
///         ret == Z_STREAM_END)
///         ret = Z_BUF_ERROR;
/// }
/// ```
///
/// Three separate facts live in those five lines:
///
/// 1. The flush happens whenever **anything** was put into the window, error paths
///    included -- so a stream that decoded one byte and then failed still hands that
///    byte over.
/// 2. `out()` is still *called* on an error path, and its answer is still read.
/// 3. But a refusal only overwrites `ret` when `ret` was `Z_STREAM_END`. A
///    `Z_DATA_ERROR` **survives** it, because a malformed stream is the more
///    specific and more useful diagnosis. Dropping the `ret == Z_STREAM_END` guard
///    would turn every failed flush into a buffer error and lose the message with it.
///
/// The fixture is the too-far-back stream from
/// [`hand_written_too_far_back`], which is the smallest thing that is both a data
/// error and a producer of output -- exactly the combination needed to tell the two
/// codes apart.
#[test]
fn a_failing_epilogue_flush_downgrades_success_and_nothing_else() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    // A data error with output pending, and a refusing sink: the data error wins.
    let invalid = hand_written_too_far_back();
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Refusing::immediately();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(&invalid), &mut source, &mut sink);
    assert_eq!(
        result.code,
        ReturnCode::DATA_ERROR,
        "a data error must survive a refused epilogue flush"
    );
    assert_eq!(
        result.msg,
        Some(MSG_TOO_FAR_BACK),
        "and must keep its message"
    );
    assert_eq!(sink.calls, 1, "out() is still called on the error path");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    // The same refusal against a valid stream: success is downgraded.
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Refusing::immediately();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
    assert_eq!(
        result.code,
        ReturnCode::BUF_ERROR,
        "Z_STREAM_END is the one status a refused flush overwrites"
    );
    assert_eq!(result.msg, None);
    assert_eq!(sink.calls, 1);
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// ★ An output callback that **succeeds** lets the same stream decode.
///
/// The control for the test above, and the other half of the polarity: `push()`
/// with a null descriptor returns zero, zero means accepted, and the decode
/// completes. The stream, the window and the input source are identical -- only the
/// sink's answer differs -- so the status difference can only be the polarity.
#[test]
fn an_output_callback_that_succeeds_lets_the_decode_complete() {
    let outcome = decode_as_harness(ONE_ZERO_BYTE_FIXED);
    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(outcome.written, ONE_ZERO_BYTE);
    assert_eq!(outcome.out_calls, 1);
    assert_eq!(outcome.end, ReturnCode::OK);
}

/// ★ The two polarities are genuinely opposite -- the one test whose whole job is
/// to fail if they are ever made to agree.
///
/// The construction is the point. One boolean, `refuse`, expresses a single intent
/// -- "the callback reports trouble" -- and it is handed to each side in that
/// side's own convention: to the input side as "yield nothing", to the output side
/// as "return `Err`". If the implementation ever reads either convention the wrong
/// way round, one of the four cells below changes, and this test says which.
///
/// | | `in()` yields bytes | `in()` yields nothing |
/// |---|---|---|
/// | **`out()` returns `Ok`** | `Z_STREAM_END` | `Z_BUF_ERROR`, `next_in == None` |
/// | **`out()` returns `Err`** | `Z_BUF_ERROR`, `next_in == Some` | `Z_BUF_ERROR`, `next_in == None` |
///
/// The bottom-right cell is where the priority shows: when both sides fail, the
/// *input* failure wins, because `PULL()` leaves through `inf_leave` before the
/// epilogue's flush ever runs (`infback.c` L105 versus L562-L566). The `next_in`
/// discriminator therefore reports the input failure, which is exactly what
/// `zlib.h` L1199-L1202 promises.
///
/// The stream is [`HELLO_FIXED`], long enough to produce output so that `out()` is
/// genuinely reached, and it is fed through the callback rather than staged so that
/// `in()` is genuinely reached too.
#[test]
fn the_two_callback_polarities_are_opposite() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    for input_fails in [false, true] {
        for output_fails in [false, true] {
            let mut state =
                inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");

            // The input side spells "trouble" as an empty queue: nothing to
            // yield, which is C's `in()` returning zero.
            let mut source = if input_fails {
                Pieces::new(&HELLO_FIXED[..1], HELLO_FIXED.len())
            } else {
                Pieces::new(HELLO_FIXED, HELLO_FIXED.len())
            };
            // The output side spells the same "trouble" as `Err`, which is C's
            // `out()` returning non-zero. Note that this is the *opposite*
            // direction: an empty answer above, a non-empty error here.
            let mut sink = if output_fails {
                Refusing::immediately()
            } else {
                Refusing::after(usize::MAX)
            };

            let result = inflate_back(&mut state, None, &mut source, &mut sink);
            let expected = if input_fails || output_fails {
                ReturnCode::BUF_ERROR
            } else {
                ReturnCode::STREAM_END
            };
            assert_eq!(
                result.code, expected,
                "in() fails: {input_fails}, out() fails: {output_fails}"
            );
            assert_eq!(
                result.next_in.is_none(),
                input_fails,
                "next_in is null exactly when in() failed \
                 (in() fails: {input_fails}, out() fails: {output_fails})"
            );
            assert_eq!(inflate_back_end(state), ReturnCode::OK);
        }
    }
}

/// An input callback handing over one byte per call still decodes correctly.
///
/// This is the resumability contract of the callback form, and it is the shape C's
/// own `pull()` has: `return next < sizeof(dat) ? (*buf = dat + next++, 1) : 0`
/// (`test/infcover.c` L460) hands over exactly one byte at a time.
///
/// It also pins a path the staged form cannot reach. `infback.c` L424 dispatches to
/// `inflate_fast` only when `have >= 6 && left >= 258`; a source that never has more
/// than one byte in hand keeps `have` at one, so every literal, length and distance
/// goes through the slow path at L431-L544. The two must agree, so this asserts the
/// same output the whole-stream decode produces.
///
/// Both `pull()`'s own `dat[] = {0x63, 0, 2, 0}` and a real payload are driven, and
/// the call count is checked to be one per byte plus the final refusal -- proof that
/// the source really was consulted byte by byte rather than being read past.
#[test]
fn an_input_callback_handing_over_one_byte_at_a_time_still_decodes() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(HELLO_FIXED, 1);
    let result = inflate_back(&mut state, None, &mut source, &mut sink);
    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_eq!(
        sink.written, HELLO,
        "a byte-at-a-time source decodes what a whole-stream source does"
    );
    assert_eq!(
        source.calls,
        HELLO_FIXED.len(),
        "one call per byte, and the stream ended before a refusal was needed"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    // C's own `dat[]`, driven the way C drives it.
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Pieces::new(PULL_DAT, 1);
    let result = inflate_back(&mut state, None, &mut source, &mut sink);
    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_eq!(
        sink.written,
        [0x00, 0x00, 0x00, 0x00],
        "pull()'s dat[] is a fixed block of four zero bytes"
    );
    assert_eq!(source.calls, PULL_DAT.len());
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// ★ Both sides of the fast-path threshold decode identically.
///
/// `infback.c` L423-L427 is the only dispatch decision in the whole decoder:
///
/// ```c
/// /* use inflate_fast() if we have enough input and output */
/// if (have >= 6 && left >= 258) {
///     RESTORE();
///     inflate_fast(strm, state->wsize);
///     LOAD();
///     break;
/// }
/// ```
///
/// Two independent conditions, and this test crosses each of them in turn while
/// holding the stream constant, so the output has to be identical on both sides of
/// both. That is the strongest available statement about a fast path: not that it
/// works, but that it is *indistinguishable* from the slow path it replaces.
///
/// * **`have >= 6`** is crossed by the piece size of the input source. A source that
///   never has more than five bytes in hand can never satisfy it, so every symbol
///   goes through the slow decode at L431-L544; a source that hands over the whole
///   stream satisfies it immediately.
/// * **`left >= 258`** is crossed by the window size, because `left` starts at
///   `wsize` (`infback.c` L216-L222 with `put` at the base). At
///   `windowBits = 8` the window is 256 bytes and the condition can *never* hold, so
///   the fast path is structurally unreachable however much input is available; at
///   `windowBits = 9` it is 512 and it can.
///
/// Note that the two 258-byte figures are unrelated: this one is `MAX_MATCH`, the
/// most a single match can write, and it is why the fast path may skip its own output
/// bounds check.
#[test]
fn both_sides_of_the_fast_path_threshold_decode_identically() {
    // A payload with a long self-overlapping run, so that the two paths' match-copy
    // loops are both genuinely exercised rather than only their literal handling.
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(b"abcdefgh");
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in &payload {
        writer.literal(byte);
    }
    // The maximal match, at the shortest distance: 258 bytes copied one at a time out
    // of the eight already written.
    writer.back_reference(258, 8);
    writer.end_of_block();
    let stream = writer.finish();

    // The expected output is built the way the decoder must build it: one byte at a
    // time, each read eight positions back from the *current* end. A block copy would
    // give a different -- and wrong -- answer for any distance shorter than the
    // length, which is exactly why `infback.c` L529-L541 copies byte by byte.
    let mut expected = payload.clone();
    for _ in 0..258 {
        let byte = expected[expected.len() - 8];
        expected.push(byte);
    }
    assert_eq!(expected.len(), 8 + 258);

    // The `have >= 6` axis: piece sizes below, at and above six, plus the whole
    // stream staged at once.
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for piece in [1_usize, 5, 6, 7, stream.len()] {
        let mut state =
            inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
        let mut sink = Collector::default();
        let mut source = Pieces::new(&stream, piece);
        let result = inflate_back(&mut state, None, &mut source, &mut sink);
        assert_eq!(
            result.code,
            ReturnCode::STREAM_END,
            "pieces of {piece} byte(s)"
        );
        assert_eq!(
            sink.written, expected,
            "pieces of {piece} byte(s) must decode to the same bytes"
        );
        assert_eq!(inflate_back_end(state), ReturnCode::OK);
    }

    // The `left >= 258` axis: 256 bytes of window, where the condition can never
    // hold, against 512, where it can. The whole stream is staged in both cases so
    // that the input half of the condition is always satisfied and only the output
    // half varies.
    let mut narrow = sentinel_window(1 << 8);
    let below = decode(&mut narrow, 8, &stream);
    let mut wide = sentinel_window(1 << 9);
    let above = decode(&mut wide, 9, &stream);
    assert_eq!(below.code, ReturnCode::STREAM_END, "windowBits 8");
    assert_eq!(above.code, ReturnCode::STREAM_END, "windowBits 9");
    assert_eq!(below.written, expected, "windowBits 8");
    assert_eq!(
        above.written, expected,
        "the window size must not change the decoded bytes"
    );
}

/// An output failure part-way through a long decode stops it there.
///
/// The test above reaches the epilogue's flush (`infback.c` L562-L566); this one
/// reaches `ROOM()`'s (L157-L160), which is the other of the two places `out()` is
/// called from and the one that matters for a stream longer than the window. The
/// sink accepts the first window and refuses the second, so the decode dies
/// mid-stream rather than at the end.
///
/// A 256-byte window with a payload several windows long is what makes the two
/// flushes distinguishable without a large fixture.
#[test]
fn a_mid_stream_output_failure_stops_the_decode() {
    const BITS: i32 = 8;
    const WINDOW: usize = 1 << BITS;
    let payload = common::corpus::repetitive(WINDOW * 4);
    let stream = fixed_literal_stream(&payload);
    let mut window = sentinel_window(WINDOW);

    let mut state = inflate_back_init(BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Refusing::after(1);
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(&stream), &mut source, &mut sink);

    assert_eq!(result.code, ReturnCode::BUF_ERROR);
    assert!(
        result.next_in.is_some(),
        "the input side did not fail, so next_in stays non-null"
    );
    assert_eq!(sink.calls, 2, "one window accepted, the next refused");
    assert_eq!(
        sink.written.len(),
        WINDOW,
        "exactly the accepted window was kept"
    );
    assert!(
        result.next_in.map_or(0, <[u8]>::len) > 0,
        "the decode stopped with input still unread"
    );
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

// ===========================================================================
// 3. The hex vector suite, driven through the `inflateBack` path
// ===========================================================================

/// Every raw vector the C harness drives through `inflateBack` behaves as the
/// reference does.
///
/// This is `try()`'s second leg (`test/infcover.c` L554-L571), assertion for
/// assertion:
///
/// ```c
/// ret = inflateBack(&strm, pull, Z_NULL, push, Z_NULL);
/// assert(ret != Z_STREAM_ERROR);
/// if (err) {
///     assert(ret == Z_DATA_ERROR);
///     assert(strcmp(id, strm.msg) == 0);
/// }
/// ```
///
/// The `ret != Z_STREAM_ERROR` check applies to **every** vector, well-formed and
/// malformed alike, and it is the sharpest single assertion in the file: a
/// `Z_STREAM_ERROR` out of a decode means the state machine reached a mode
/// `inflateBack` does not implement, which is a defect in the decoder rather than a
/// verdict about the data. `Z_MEM_ERROR` is checked out too, for the same reason
/// `try()`'s first leg checks it out at L543.
///
/// Beyond C's assertions, the mode is checked against [`REACHABLE_MODES`] and the
/// success cases are required to reach `DONE` while the failures reach `BAD` --
/// which C cannot see, and which is what turns "the right code came back" into "for
/// the right reason".
#[test]
fn every_raw_vector_from_the_c_harness_behaves_as_the_reference_does() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for vector in RAW_VECTORS {
        let stream = common::h2b(vector.hex);
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        let id = vector.id;

        assert_ne!(
            outcome.code,
            ReturnCode::STREAM_ERROR,
            "`{id}`: a stream error means the decoder reached an unimplemented mode"
        );
        assert_ne!(
            outcome.code,
            ReturnCode::MEM_ERROR,
            "`{id}`: inflateBack allocates nothing, so it cannot run out of memory"
        );
        assert_ne!(
            outcome.code,
            ReturnCode::OK,
            "`{id}`: zlib.h L1204 -- inflateBack() cannot return Z_OK"
        );
        assert!(
            outcome
                .mode
                .is_some_and(|mode| REACHABLE_MODES.contains(&mode)),
            "`{id}`: came to rest in {:?}, outside the modes infback.c implements",
            outcome.mode
        );

        if vector.fails {
            assert_eq!(
                outcome.code,
                ReturnCode::DATA_ERROR,
                "`{id}`: the reference reports a data error"
            );
            assert_eq!(
                outcome.msg,
                Some(id),
                "`{id}`: strm->msg must match character for character"
            );
            assert_eq!(outcome.mode, Some(Mode::Bad), "`{id}`: a data error is BAD");
        } else {
            assert_eq!(
                outcome.code,
                ReturnCode::STREAM_END,
                "`{id}`: a well-formed stream ends"
            );
            assert_eq!(outcome.msg, None, "`{id}`: success sets no message");
            assert_eq!(
                outcome.mode,
                Some(Mode::Done),
                "`{id}`: a completed stream is DONE"
            );
        }
        assert_eq!(outcome.end, ReturnCode::OK, "`{id}`: teardown");
    }
}

/// ★ The `invalid distance too far back` vector is rejected -- the direct
/// regression guard for this port's own baseline commit.
///
/// HEAD `09a1572` is titled *"Fix `inflateBack()` bug that would fail to detect a
/// too far back"*, and its message continues: "The bug would pass off an invalid
/// deflate stream as good, and copy uninitialized memory contents to the output."
/// The fix **deletes** two lines from the fast-path dispatch inside `case LEN:`,
/// between `RESTORE()` and the `inflate_fast()` call:
///
/// ```text
/// -    if (state->whave < state->wsize)
/// -        state->whave = state->wsize - left;
/// ```
///
/// Widening `whave` there declares window bytes nobody wrote to be valid history,
/// which defeats the check at `infback.c` L516-L521:
///
/// ```c
/// if (state->offset > state->wsize - (state->whave < state->wsize ? left : 0)) {
///     strm->msg = (z_const char *)"invalid distance too far back";
///     state->mode = BAD;
///     break;
/// }
/// ```
///
/// So this test asserts three things, and the third is the one that would have
/// caught the original bug: the status is `Z_DATA_ERROR`, the message is exact, and
/// **nothing at all reached the output** -- because the failure mode being guarded
/// against is not a wrong status but uninitialised window bytes being emitted as if
/// they were decoded data. The window is pre-filled with [`WINDOW_SENTINEL`], so a
/// regression would show up as `0xa5` bytes in a sink that should be empty.
#[test]
fn the_invalid_distance_too_far_back_vector_is_rejected() {
    let stream = common::h2b(TOO_FAR_BACK_HEX);
    let outcome = decode_as_harness(&stream);

    assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
    assert_eq!(outcome.msg, Some(MSG_TOO_FAR_BACK));
    assert_eq!(outcome.mode, Some(Mode::Bad));
    assert!(
        outcome.written.is_empty(),
        "no uninitialised window byte may reach the output: got {:?}",
        outcome.written
    );
    assert_eq!(
        outcome.out_calls, 0,
        "the epilogue must not flush a window the decoder never legitimately filled"
    );
    assert_eq!(outcome.end, ReturnCode::OK);
}

/// ★ A hand-written back-reference reaching past the written window is rejected,
/// with nothing emitted.
///
/// The companion to the test above, from the other direction. That one takes the
/// harness's vector on trust; this one constructs the minimal case symbol by symbol
/// -- one literal, then a length-3 match at distance 5 -- so that exactly *which*
/// invalid thing is being rejected is beyond doubt. Only one byte has been written
/// when the reference is read, `whave` is still zero, and `infback.c` L516-L521
/// therefore has to refuse it:
///
/// ```c
/// if (state->offset > state->wsize - (state->whave < state->wsize ? left : 0)) {
/// ```
///
/// With `whave < wsize`, `wsize - left` is exactly how many bytes have been put into
/// the window, which is one. A distance of five is greater, so the stream is invalid.
/// The two lines commit `09a1572` deleted would have raised `whave` to
/// `wsize - left` before the fast path ran, making that comparison always pass.
///
/// ★ The assertion that catches the original bug is the one about the output. The
/// epilogue at `infback.c` L560-L568 runs on the error path too, so the one byte that
/// *was* legitimately decoded is flushed -- and exactly that one byte, never four. A
/// decoder that accepted the reference would have emitted three further bytes copied
/// out of a window nobody wrote, which the pre-filled [`WINDOW_SENTINEL`] makes
/// unmistakable: they would read `a5 a5 a5`. "Copy uninitialized memory contents to
/// the output" is the commit message's own description of the harm.
#[test]
fn a_hand_written_reference_reaching_past_the_written_window_is_rejected() {
    let stream = hand_written_too_far_back();
    let outcome = decode_as_harness(&stream);

    assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
    assert_eq!(outcome.msg, Some(MSG_TOO_FAR_BACK));
    assert_eq!(outcome.mode, Some(Mode::Bad));
    assert_eq!(
        outcome.written, b"a",
        "exactly the one byte that was legitimately decoded, and nothing the \
         back-reference would have copied out of an unwritten window"
    );
    assert_eq!(
        outcome.out_calls, 1,
        "infback.c L560-L568 flushes what the window holds even on the error path"
    );
    assert!(
        !outcome.written.contains(&WINDOW_SENTINEL),
        "no uninitialised window byte may reach the output"
    );

    // The same stream with the distance brought inside what has been written decodes,
    // which is what makes the rejection above about the distance and not about the
    // shape of the stream. Three literals then a length-3 match at distance 3.
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in b"abc" {
        writer.literal(byte);
    }
    writer.back_reference(3, 3);
    writer.end_of_block();
    let legal = decode_as_harness(&writer.finish());
    assert_eq!(legal.code, ReturnCode::STREAM_END);
    assert_eq!(legal.written, b"abcabc");
}

/// The `fixed` and `stored` success vectors decode, and the first produces no
/// output at all.
///
/// `try("3 0", "fixed", 0)` and `try("1 1 0 fe ff 0", "stored", 0)`
/// (`test/infcover.c` L585, L587) are the two smallest well-formed streams in the
/// harness, one per non-dynamic block type.
///
/// `"3 0"` is a final fixed block holding only the end-of-block code, so it decodes
/// to nothing -- and that is worth asserting explicitly, because `infback.c`'s
/// epilogue only calls `out()` `if (state->wsize != left && ...)` (L562), so a zero
/// byte decode must leave the sink entirely untouched rather than handing it an
/// empty slice.
///
/// `"1 1 0 fe ff 0"` is a final stored block declaring one byte, whose complement
/// `fe ff` is correct -- the well-formed counterpart of the
/// `invalid stored block lengths` vector.
#[test]
fn the_fixed_and_stored_success_vectors_decode() {
    let fixed = decode_as_harness(&common::h2b("3 0"));
    assert_eq!(fixed.code, ReturnCode::STREAM_END);
    assert!(fixed.written.is_empty());
    assert_eq!(
        fixed.out_calls, 0,
        "infback.c L562 skips the flush when nothing was decoded"
    );
    assert_eq!(
        &common::h2b("3 0"),
        EMPTY_FIXED,
        "the harness's `3 0` is the stream cover_back() stages first"
    );

    let stored = decode_as_harness(&common::h2b("1 1 0 fe ff 0"));
    assert_eq!(stored.code, ReturnCode::STREAM_END);
    assert_eq!(stored.written, [0x00], "one stored byte");
    assert_eq!(stored.out_calls, 1);
}

/// The `inflate_fast TYPE return` vector decodes.
///
/// `cover_inflate()` drives this one through `inf()` rather than `try()`
/// (`test/infcover.c` L612-L613), so C never puts it through `inflateBack` at all --
/// which is exactly why it is worth adding here. Its name says what it covers:
/// `inflate_fast` returning with the mode back at `TYPE`, that is, the fast path
/// completing a block and handing control back to the block dispatcher rather than
/// to an error or to the end of the stream. `infback.c` L424-L428 is the dispatch
/// being exercised, and L427-L428's `LOAD(); break;` is the return being tested.
///
/// `window wrap` (L614, also an `inf()` vector) is included alongside it because it
/// is the one small fixture that makes the fast path copy from a *wrapped* window,
/// and it produces 262 bytes -- more than a 256-byte window holds, so it also
/// crosses a `ROOM()` boundary when driven at `windowBits = 8`.
#[test]
fn the_inf_only_fast_path_vectors_decode_through_inflate_back_too() {
    let type_return = decode_as_harness(&common::h2b("2 8 20 80 0 3 0"));
    assert_eq!(type_return.code, ReturnCode::STREAM_END);
    assert_eq!(type_return.mode, Some(Mode::Done));
    assert!(type_return.written.is_empty());

    let wrap = decode_as_harness(&common::h2b("63 18 5 40 c 0"));
    assert_eq!(wrap.code, ReturnCode::STREAM_END);
    assert_eq!(wrap.written.len(), 262, "the reference decodes 262 bytes");
    assert!(
        wrap.written.iter().all(|&byte| byte == 0),
        "every decoded byte is zero"
    );

    // The same stream at the smallest window, where 262 bytes no longer fit in one
    // window and `ROOM()` has to flush mid-block.
    let mut small = sentinel_window(1 << MIN_BACK_WINDOW_BITS);
    let wrapped = decode(
        &mut small,
        MIN_BACK_WINDOW_BITS,
        &common::h2b("63 18 5 40 c 0"),
    );
    assert_eq!(wrapped.code, ReturnCode::STREAM_END);
    assert_eq!(
        wrapped.written, wrap.written,
        "the window size cannot change the bytes"
    );
    assert_eq!(wrapped.out_calls, 2, "262 bytes need two 256-byte flushes");
}

/// A stored block whose length disagrees with its complement is rejected.
///
/// `try("0 0 0 0 0", "invalid stored block lengths", 1)` (`test/infcover.c` L584).
/// RFC 1951 §3.2.4 requires `NLEN` to be the one's complement of `LEN`, and
/// `infback.c` L272-L276 enforces it. `0 0 0 0 0` declares `LEN = 0` with
/// `NLEN = 0`, and zero is not the complement of zero.
///
/// Called out on its own rather than left to the table because it is the one vector
/// whose *absence* would be silent: a decoder that skipped the complement check
/// would decode this stream to nothing and report success, which looks exactly like
/// the `"3 0"` success case.
#[test]
fn a_stored_block_whose_complement_disagrees_is_rejected() {
    let outcome = decode_as_harness(&common::h2b("0 0 0 0 0"));
    assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
    assert_eq!(outcome.msg, Some("invalid stored block lengths"));
    assert_eq!(outcome.mode, Some(Mode::Bad));
    assert!(outcome.written.is_empty());
}

// ===========================================================================
// 4. The reduced state machine
// ===========================================================================

/// Only the six modes `infback.c`'s `switch` implements are ever reached.
///
/// See [`REACHABLE_MODES`] for which six and why. The interesting half of this test
/// is the complement: `Mode::ALL` has 32 entries, so 26 modes must be unreachable,
/// and every one of them would produce `Z_STREAM_ERROR` from the `default` arm at
/// `infback.c` L554-L557 if it were ever entered.
///
/// The evidence is gathered across the whole vector table plus the callback failure
/// paths, so the sample covers success, data error and buffer error alike. All three
/// of the modes that actually get *observed* are then named, so that a change which
/// stopped reaching one of them -- say, one that never left a state in `TYPE` -- would
/// fail here rather than pass quietly.
#[test]
fn only_the_reduced_mode_subset_is_ever_reached() {
    assert_eq!(Mode::ALL.len(), 32, "the enum still has 32 states");
    assert_eq!(REACHABLE_MODES.len(), 6, "infback.c implements six arms");

    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut observed: Vec<Mode> = Vec::new();

    for vector in RAW_VECTORS {
        let stream = common::h2b(vector.hex);
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        let mode = outcome.mode.expect("the mode tag must name a state");
        assert!(
            REACHABLE_MODES.contains(&mode),
            "`{}` reached {mode:?}, which infback.c has no arm for",
            vector.id
        );
        if !observed.contains(&mode) {
            observed.push(mode);
        }
    }

    // A buffer error before any block header has been read, which is the only way
    // to observe `TYPE` at rest.
    let starved = decode(&mut window, HARNESS_WINDOW_BITS, &[]);
    assert_eq!(starved.code, ReturnCode::BUF_ERROR);
    let mode = starved.mode.expect("the mode tag must name a state");
    if !observed.contains(&mode) {
        observed.push(mode);
    }

    observed.sort_unstable();
    assert_eq!(
        observed,
        [Mode::Type, Mode::Done, Mode::Bad],
        "the three modes an `inflateBack` call can come to rest in"
    );

    // And the complement: no header, trailer, dictionary, sync or memory state.
    for mode in Mode::ALL {
        if REACHABLE_MODES.contains(&mode) {
            continue;
        }
        assert!(
            !observed.contains(&mode),
            "{mode:?} ({}) must be unreachable from inflateBack",
            mode.c_name()
        );
    }
}

/// A zlib-wrapped stream fails as *data*, not as a header parse.
///
/// `inflateBack` decodes raw DEFLATE only (`zlib.h` L1155-L1162), and this is the
/// cleanest observable proof of it: hand it a well-formed RFC 1950 stream and the
/// `78 9c` header bytes are interpreted as DEFLATE block structure rather than as a
/// header. `0x78` is `0111_1000`, so `BFINAL = 0` and `BTYPE = 00` -- a non-final
/// stored block -- and the two bytes that follow are read as `LEN`/`NLEN`, which do
/// not complement each other.
///
/// The message therefore has to be `invalid stored block lengths`, and *not* any of
/// `incorrect header check`, `unknown compression method` or `invalid window size`,
/// which are what the header modes `inflateBack` does not implement would have
/// produced.
#[test]
fn a_zlib_wrapped_stream_fails_as_data_not_as_a_header_parse() {
    assert_eq!(
        &ZLIB_WRAPPED_HELLO[..2],
        &[0x78, 0x9c],
        "a level-6 zlib stream starts 78 9c"
    );
    assert_eq!(
        &ZLIB_WRAPPED_HELLO[2..ZLIB_WRAPPED_HELLO.len() - 4],
        HELLO_FIXED,
        "and carries the raw block this file already decodes on its own"
    );

    let outcome = decode_as_harness(ZLIB_WRAPPED_HELLO);
    assert_eq!(outcome.code, ReturnCode::DATA_ERROR);
    assert_eq!(
        outcome.msg,
        Some("invalid stored block lengths"),
        "78 9c was read as a stored block header, not as a zlib header"
    );
    assert_eq!(outcome.mode, Some(Mode::Bad));
}

/// A gzip-wrapped stream likewise fails as data.
///
/// The same argument as above with a different first byte: `0x1f` is `0001_1111`,
/// so `BFINAL = 1` and `BTYPE = 11`, which RFC 1951 §3.2.3 reserves and
/// `infback.c` L253-L257 rejects outright. The verdict is
/// `invalid block type`, not `incorrect gzip header check`.
///
/// The two gzip vectors the harness withholds from `inflateBack` (L601-L603) are
/// driven here too, which is the assertion that keeps [`WRAPPED_VECTORS`]
/// honest: they are skipped by `try()` because they test a *trailer*, and what
/// `inflateBack` makes of them is a block-type error long before any trailer
/// could matter.
#[test]
fn the_gzip_wrapped_vectors_the_harness_skips_are_not_raw_streams() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for hex in WRAPPED_VECTORS {
        let stream = common::h2b(hex);
        assert_eq!(
            &stream[..3],
            &[0x1f, 0x8b, 0x08],
            "`{hex}` is a gzip stream"
        );
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        assert_eq!(
            outcome.code,
            ReturnCode::DATA_ERROR,
            "`{hex}` is not a raw stream"
        );
        assert_eq!(
            outcome.msg,
            Some("invalid block type"),
            "`{hex}`: 1f is BFINAL=1 with the reserved BTYPE=11"
        );
        assert!(outcome.written.is_empty());
    }
}

/// A stream that ends mid-block is a `Z_BUF_ERROR`, never a `Z_STREAM_END`.
///
/// Truncation is an input failure, not a data error: the bytes seen so far were
/// valid, there simply are not enough of them. `PULL()` (`infback.c` L99-L109) is
/// where that verdict is made, and it is made the same way at every depth --
/// mid-header, mid-code-length-alphabet, mid-symbol and mid-stored-block.
///
/// Each truncation point below is chosen to land in a different state, and the
/// dynamic stream is long enough that the middle two really are mid-`TABLE` and
/// mid-`LEN` rather than both being mid-header.
#[test]
fn a_stream_that_ends_mid_block_is_a_buffer_error() {
    let dynamic = common::h2b(DYNAMIC_LONG_DISTANCE.hex);
    assert!(
        dynamic.len() > 16,
        "the fixture must be long enough to truncate meaningfully"
    );
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    for cut in [1, 2, 8, dynamic.len() / 2, dynamic.len() - 1] {
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &dynamic[..cut]);
        assert_eq!(
            outcome.code,
            ReturnCode::BUF_ERROR,
            "a stream cut at {cut} of {} bytes is incomplete, not invalid",
            dynamic.len()
        );
        assert!(
            outcome.unused.is_none(),
            "truncation is an input failure, so next_in is nulled"
        );
        assert_ne!(
            outcome.mode,
            Some(Mode::Done),
            "an incomplete stream never reaches DONE"
        );
    }
}

/// A dynamic-Huffman block decodes, which is how the `order[19]` permutation is
/// checked.
///
/// `infback.c` L205-L206 declares the code-length code-length permutation:
///
/// ```c
/// static const unsigned short order[19] =
///     {16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15};
/// ```
///
/// It is applied at L317 (`state->lens[order[state->have++]] = ..`) and at L321
/// (the zero-fill for the codes the stream did not send). RFC 1951 §3.2.7 defines
/// it, and the ordering is not decorative: get a single entry wrong and the
/// code-length alphabet is built from the wrong lengths, so `inflate_table` either
/// fails or produces a table that decodes garbage. Either way **every** dynamic
/// block breaks, so one dynamic block decoding correctly is a strong signal, and
/// that is the whole argument for testing it this way rather than reaching into
/// the private table.
///
/// The fixture is `common::corpus::text()` at level 9, verified below to really be
/// a dynamic block rather than a fixed one, and long enough (716 bytes from 391)
/// that its alphabet exercises all three repeat forms.
#[test]
fn a_dynamic_huffman_block_decodes_so_the_code_length_permutation_is_honoured() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    for fixture in DYNAMIC_FIXTURES {
        let stream = common::h2b(fixture.hex);
        assert_eq!(
            first_block_header(&stream),
            (true, 2),
            "`{}` must be a single final dynamic block",
            fixture.id
        );

        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        assert_eq!(outcome.code, ReturnCode::STREAM_END, "`{}`", fixture.id);
        assert_eq!(outcome.mode, Some(Mode::Done), "`{}`", fixture.id);
        assert_eq!(
            outcome.written.len(),
            fixture.decoded_len,
            "`{}` decodes to {} bytes in the reference",
            fixture.id,
            fixture.decoded_len
        );
        assert!(
            outcome.written.iter().all(|&byte| byte == 0),
            "`{}` decodes to zero bytes throughout",
            fixture.id
        );
    }
}

/// Stored, fixed-Huffman and dynamic-Huffman blocks each decode.
///
/// The three `BTYPE` values RFC 1951 §3.2.3 defines, and the three arms
/// `infback.c` L242-L258 dispatches to: `STORED` at L243, the fixed tables at
/// L247-L250 and `TABLE` at L252-L255. One fixture per arm, each pinned to the
/// block type it is meant to exercise by reading `BFINAL` and `BTYPE` straight out
/// of byte zero -- because a fixture that silently became a fixed block would make
/// the dynamic arm untested while still passing.
#[test]
fn stored_fixed_and_dynamic_blocks_each_decode() {
    let payload = common::corpus::text();
    let dynamic = common::h2b(DYNAMIC_LENGTH_EXTRA.hex);
    let dynamic_expected = vec![0_u8; DYNAMIC_LENGTH_EXTRA.decoded_len];
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    for (name, stream, expected, expected_btype) in [
        ("stored", stored_stream(&payload), payload.clone(), 0_u8),
        ("fixed", fixed_literal_stream(&payload), payload.clone(), 1),
        ("dynamic", dynamic, dynamic_expected, 2),
    ] {
        assert_eq!(
            first_block_header(&stream),
            (true, expected_btype),
            "the {name} fixture must be a single final block of type {expected_btype}"
        );
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        assert_eq!(outcome.code, ReturnCode::STREAM_END, "{name}");
        assert_eq!(outcome.written, expected, "{name}");
    }
}

/// A stream mixing all three block types decodes.
///
/// Each arm decoding on its own does not prove they hand over to each other
/// correctly: the block dispatcher has to leave the bit accumulator aligned and the
/// window history intact across a block boundary, and `deflate_stored` in
/// particular flushes differently from the Huffman coders.
///
/// The fixture is built in three passes over one growing input, with
/// `deflate_params` switching the encoder between them -- which is what forces a
/// block boundary and a change of block type at a chosen point. The result is
/// verified to open with a *non-final* stored block, so it really is more than one
/// block, and to decode to every byte of the payload.
#[test]
fn a_stream_mixing_all_three_block_types_decodes() {
    // Three regions, one per block type. The stored and fixed halves are written out
    // by `BlockWriter`; the dynamic half is spliced in from the C harness, because
    // this file deliberately implements no dynamic encoder.
    let stored_region = common::corpus::repetitive(32);
    let literal_region = &common::corpus::text()[..200];
    let dynamic = common::h2b(DYNAMIC_LENGTH_EXTRA.hex);

    let mut writer = BlockWriter::new();
    // Block one: a non-final stored block. It ends byte-aligned by construction
    // (RFC 1951 SS3.2.4), which is why nothing has to be done between blocks here.
    writer.stored(&stored_region, false);
    // Block two: a non-final fixed-Huffman block of literals.
    writer.begin_fixed_block(false);
    for &byte in literal_region {
        writer.literal(byte);
    }
    writer.end_of_block();
    // Block three: the harness's dynamic block, spliced in bit-wise at whatever
    // offset block two ended on. It carries `BFINAL = 1`, so it terminates the
    // stream. See `BlockWriter::splice` for why this is a valid continuation.
    writer.splice(&dynamic);
    let stream = writer.finish();

    let mut expected: Vec<u8> = Vec::new();
    expected.extend_from_slice(&stored_region);
    expected.extend_from_slice(literal_region);
    expected.extend(core::iter::repeat(0_u8).take(DYNAMIC_LENGTH_EXTRA.decoded_len));

    assert_eq!(
        first_block_header(&stream),
        (false, 0),
        "the mixed fixture must open with a non-final stored block"
    );

    let outcome = decode_as_harness(&stream);
    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(outcome.written, expected);
    assert_eq!(outcome.mode, Some(Mode::Done));
}

/// A mode forced before the call is discarded by the entry reset.
///
/// This is the reachable half of `cover_back()` step seven
/// (`test/infcover.c` L496-L498). C reaches the `default` arm by having its `pull()`
/// callback reach through the descriptor it was handed and assign
/// `state->mode = SYNC`, with the comment "force an otherwise impossible
/// situation"; the `default` arm then returns `Z_STREAM_ERROR` from L554-L557,
/// whose own comment reads "can't happen, but makes compilers happy".
///
/// That is not reachable here, and the reason is a safety property rather than a
/// gap: a callback cannot hold `&mut InflateState` while `inflate_back` does, so
/// nothing outside the decoder can move the mode while it runs. The `default` arm
/// still exists in `src/infback.rs`, still returns `Z_STREAM_ERROR`, and is covered
/// by that file's own `#[cfg(test)]` module, which can construct the situation
/// because it is inside the crate. **That is where step seven's assertion lives.**
///
/// What is reachable, and is asserted here, is the neighbouring guarantee: forcing
/// a mode *between* calls has no effect either, because `infback.c` L215 assigns
/// `state->mode = TYPE` at entry. So a state left in any mode -- including the
/// impossible `SYNC` -- decodes the next stream exactly as a fresh one would. The
/// `Z_STREAM_ERROR` half of step seven is instead reached through
/// [`a_state_not_built_for_inflate_back_is_a_stream_error`].
#[test]
fn a_mode_forced_before_the_call_is_discarded_by_the_entry_reset() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");

    for forced in Mode::ALL {
        assert!(
            state.set_mode_tag(forced.as_raw()),
            "{} is a live tag",
            forced.c_name()
        );
        let mut sink = Collector::default();
        let mut source = Declining::default();
        let result = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
        assert_eq!(
            result.code,
            ReturnCode::STREAM_END,
            "a mode forced to {} before the call must be discarded",
            forced.c_name()
        );
        assert_eq!(sink.written, HELLO, "forced {}", forced.c_name());
    }

    // A tag that names no state is refused outright rather than installed, so the
    // state cannot be corrupted through this door at all.
    assert!(!state.set_mode_tag(-1), "-1 names no state");
    assert!(!state.set_mode_tag(i32::MAX), "i32::MAX names no state");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

// ===========================================================================
// 5. The borrowed-window contract
// ===========================================================================
//
// This is the structural difference between `inflateBack` and `inflate()`.
// `inflate()` allocates and owns its window lazily; `inflateBackInit_` stores the
// caller's pointer (`infback.c` L59) and `inflateBack` uses the window *as* the
// output buffer, which `zlib.h` L1141-L1147 explains is the whole point -- it
// "avoids copying between the output and the sliding window by simply making the
// window itself the output buffer".
//
// Three consequences follow, and each gets a test: the window's length is part of
// the contract (section 1 covers that), its initial contents are the caller's
// business and must never be assumed, and the borrow ends when the state does.

/// One window serves two successive decodes.
///
/// `zlib.h` L1150-L1152: `inflateBackInit` "must be called first to allocate the
/// internal state, and to initialize the state with the user-provided window
/// buffer. `inflateBack()` may then be used multiple times to inflate a complete,
/// raw deflate stream with each call." Reuse across calls is not an accident of the
/// implementation, it is the advertised behaviour, and it works because every field
/// the decoder depends on is reset at entry (`infback.c` L214-L223).
///
/// Two different streams are decoded in the two rounds, and the second is the
/// dynamic one, so the reuse has to survive a change of both block type and table
/// contents rather than just repeating itself. The window is deliberately *not*
/// re-filled with the sentinel between rounds, so the second decode starts with the
/// first decode's output still sitting in the buffer -- which is exactly the state a
/// real caller's window is in.
#[test]
fn one_window_serves_two_successive_decodes() {
    let dynamic = common::h2b(DYNAMIC_LONG_DISTANCE.hex);
    let dynamic_expected = vec![0_u8; DYNAMIC_LONG_DISTANCE.decoded_len];
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");

    for round in 0..2 {
        let mut sink = Collector::default();
        let mut source = Declining::default();
        let first = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
        assert_eq!(
            first.code,
            ReturnCode::STREAM_END,
            "round {round}, stream 1"
        );
        assert_eq!(sink.written, HELLO, "round {round}, stream 1");

        let mut sink = Collector::default();
        let mut source = Declining::default();
        let second = inflate_back(&mut state, Some(&dynamic), &mut source, &mut sink);
        assert_eq!(
            second.code,
            ReturnCode::STREAM_END,
            "round {round}, stream 2"
        );
        assert_eq!(sink.written, dynamic_expected, "round {round}, stream 2");
    }

    assert_eq!(inflate_back_end(state), ReturnCode::OK);
}

/// ★ A window pre-filled with `0xa5` still decodes correctly.
///
/// The caller's window is **not** zeroed by the library -- `inflateBackInit_` stores
/// the pointer and nothing else (`infback.c` L59), and `inflateBack` resets `whave`
/// to zero (L217) rather than the bytes. `whave` is what makes the difference: it
/// records how much of the window has actually been written, a distance may reach
/// back only that far, and the checks at L516-L521 and inside `inflate_fast` are
/// what enforce it.
///
/// So any code path that assumes a fresh window is zeroed is a defect, and filling
/// with `0xa5` is the cheapest way to make it fail loudly rather than pass by luck.
/// This is `test/infcover.c`'s own discipline -- `mem_alloc` fills every block with
/// `0xa5` "to make sure that the code isn't depending on zeros" (L87) -- applied to
/// the one buffer that allocator cannot reach.
///
/// Two fill patterns are used, and the second is the interesting one: `0x00` is what
/// a lucky implementation would get away with, so decoding identically under both
/// proves the output does not depend on the fill at all.
#[test]
fn a_window_prefilled_with_the_sentinel_still_decodes_correctly() {
    // A stream whose matches reach back a long way, so that the decode genuinely
    // reads from the window rather than only writing to it.
    let dynamic = common::h2b(DYNAMIC_LONG_DISTANCE.hex);
    let expected = vec![0_u8; DYNAMIC_LONG_DISTANCE.decoded_len];

    let mut reference: Option<Vec<u8>> = None;
    for fill in [WINDOW_SENTINEL, 0x00, 0xff] {
        let mut window = vec![fill; HARNESS_WINDOW_LEN];
        let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &dynamic);
        assert_eq!(
            outcome.code,
            ReturnCode::STREAM_END,
            "a window pre-filled with {fill:#04x}"
        );
        assert_eq!(
            outcome.written, expected,
            "the fill byte {fill:#04x} must not reach the output"
        );
        match &reference {
            None => reference = Some(outcome.written),
            Some(first) => assert_eq!(
                first, &outcome.written,
                "the fill byte {fill:#04x} changed the output"
            ),
        }
    }
}

/// Output longer than the window is flushed a window at a time, and the runs are
/// window-sized.
///
/// `ROOM()` (`infback.c` L151-L162) hands the **whole** window to `out()` when it
/// fills, resets `put` to the base and sets `whave = wsize`; the epilogue at
/// L562-L566 hands over whatever is left. So a payload of `n` windows plus a
/// remainder produces `n` full-window calls and one short one, and `zlib.h`
/// L1177-L1178's cap -- "The length written by `out()` will be at most the window
/// size" -- is an equality for every call but the last.
///
/// A 256-byte window keeps the fixture small while still crossing the boundary
/// several times, which matters because every byte here is a byte Miri has to
/// interpret.
#[test]
fn output_longer_than_the_window_is_flushed_a_window_at_a_time() {
    const BITS: i32 = 8;
    const WINDOW: usize = 1 << BITS;
    const WINDOWS: usize = 3;
    const REMAINDER: usize = 17;

    let payload = common::corpus::repetitive(WINDOW * WINDOWS + REMAINDER);
    let stream = fixed_literal_stream(&payload);
    let mut window = sentinel_window(WINDOW);
    let outcome = decode(&mut window, BITS, &stream);

    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(outcome.written, payload);
    assert_eq!(
        outcome.out_calls,
        WINDOWS + 1,
        "three full windows plus the epilogue's remainder"
    );
    assert_eq!(
        outcome.longest_run, WINDOW,
        "zlib.h L1177-L1178 caps a run at the window size, and ROOM() hits the cap"
    );

    // Exactly one window of output is the boundary case: `ROOM()` flushes it, `put`
    // returns to the base, and the epilogue then has nothing to hand over.
    let exact = common::corpus::repetitive(WINDOW);
    let exact_stream = fixed_literal_stream(&exact);
    let flushed = decode(&mut window, BITS, &exact_stream);
    assert_eq!(flushed.code, ReturnCode::STREAM_END);
    assert_eq!(flushed.written, exact);
    assert_eq!(
        flushed.out_calls, 1,
        "a stream ending on a window boundary must not produce an empty final call"
    );
}

/// A match reaching further back than the last flush is served from the window.
///
/// This is the test that proves the window is genuinely consulted rather than the
/// output being reconstructed from whatever the sink was last handed. After
/// `ROOM()` has flushed, the decoder no longer has the bytes it emitted -- the sink
/// took them -- so a distance reaching into the previous window can only be
/// satisfied out of the window buffer itself, whose valid extent is `whave`
/// (`infback.c` L156, L516-L521).
///
/// The fixture is a marker, then filler, then the marker again, with the repeat
/// placed far enough back that the match crosses at least one flush at
/// `windowBits = 8`. `common::corpus::window_crossing` is the same construction at
/// the 32 KiB scale; this one is deliberately small so that it can run under Miri.
#[test]
fn a_match_reaching_back_further_than_one_flush_uses_the_window() {
    const BITS: i32 = 8;
    const WINDOW: usize = 1 << BITS;
    const MARKER: u32 = 64;

    // Pseudo-random regions, so that the only back-reference in either stream is the
    // one written out explicitly below.
    let mut marker = vec![0_u8; MARKER as usize];
    common::lcg_fill(0x5eed, &mut marker);
    let mut filler = vec![0_u8; WINDOW + 40];
    common::lcg_fill(0xf111, &mut filler);

    // Case A: a match at a distance far larger than any single output run, decoded at
    // the largest window. The distance is chosen, not searched for, which is the
    // advantage of `BlockWriter` over a compressor here.
    let long_distance = MARKER + u32::try_from(filler.len()).expect("filler fits in u32");
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in marker.iter().chain(filler.iter()) {
        writer.literal(byte);
    }
    writer.back_reference(MARKER, long_distance);
    writer.end_of_block();
    let stream = writer.finish();

    let mut expected: Vec<u8> = Vec::new();
    expected.extend_from_slice(&marker);
    expected.extend_from_slice(&filler);
    expected.extend_from_slice(&marker);

    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(
        outcome.written, expected,
        "a match {long_distance} bytes back must be served from the window"
    );

    // Case B: the same shape at the smallest window, where the match is decoded
    // *after* `ROOM()` has already handed the bytes it refers to to the sink. Two
    // inequalities have to hold at once, and both are asserted rather than assumed:
    // the output outgrows one window, so a flush really does intervene, yet the
    // distance stays inside one window, so the reference is legal rather than a
    // too-far-back error. Once the sink has taken those bytes the decoder cannot get
    // them back from anywhere except the window itself, which is the whole point.
    let near_filler = &filler[..WINDOW / 2 + 32];
    let near_distance = MARKER + u32::try_from(near_filler.len()).expect("fits in u32");
    let mut writer = BlockWriter::new();
    writer.begin_fixed_block(true);
    for &byte in marker.iter().chain(near_filler.iter()) {
        writer.literal(byte);
    }
    writer.back_reference(MARKER, near_distance);
    writer.end_of_block();
    let near_stream = writer.finish();

    let mut near_expected: Vec<u8> = Vec::new();
    near_expected.extend_from_slice(&marker);
    near_expected.extend_from_slice(near_filler);
    near_expected.extend_from_slice(&marker);
    assert!(
        near_expected.len() > WINDOW,
        "the output must outgrow one {WINDOW}-byte window ({} bytes)",
        near_expected.len()
    );
    assert!(
        (near_distance as usize) < WINDOW,
        "yet the reference must stay within one window's reach ({near_distance} bytes back)"
    );

    let mut small = sentinel_window(WINDOW);
    let wrapped = decode(&mut small, BITS, &near_stream);
    assert_eq!(wrapped.code, ReturnCode::STREAM_END);
    assert_eq!(wrapped.written, near_expected);
    assert!(
        wrapped.out_calls >= 2,
        "the decode must have crossed at least one flush, got {} call(s)",
        wrapped.out_calls
    );
}

/// The caller may read and write its window once the state has ended.
///
/// The borrow really does end -- `inflate_back_end` takes the state by value, so
/// the compiler releases the window at that point -- and the bytes left behind are
/// the caller's to inspect. That the decoded data is still *there* is worth
/// asserting on its own: it is the observable consequence of the window and the
/// output buffer being one and the same, which is what `zlib.h` L1141-L1144
/// promises.
///
/// This is also the reachable half of `cover_back()`'s claim that
/// `inflateBackEnd` releases the state and not the window (`test/infcover.c` L499
/// followed by `mem_done`, which would report a leak if anything of the caller's had
/// been taken).
#[test]
fn the_caller_may_read_and_write_its_window_after_the_state_ends() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut state =
        inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    assert_eq!(
        &window[..HELLO.len()],
        HELLO,
        "the window *is* the output buffer, so the decoded bytes are still in it"
    );
    assert!(
        window[HELLO.len()..]
            .iter()
            .all(|&byte| byte == WINDOW_SENTINEL),
        "nothing past the decoded prefix was touched"
    );

    window.fill(0x5a);
    assert!(
        window.iter().all(|&byte| byte == 0x5a),
        "the caller owns its window again"
    );
}

// ===========================================================================
// 6. Allocation behaviour
// ===========================================================================

/// ★ Initialisation and teardown are free of allocator traffic.
///
/// C's `inflateBackInit_` performs exactly one allocation -- `ZALLOC(strm, 1,
/// sizeof(struct inflate_state))` at L51-L52 -- and `inflateBackEnd` performs
/// exactly one release, `ZFREE(strm, strm->state)` at L577. It never allocates the
/// window, because the caller supplies it.
///
/// The safe core does **neither**, and that is a deliberate division of labour
/// rather than a gap. `inflate_back_init` returns an `InflateState` *by value*; the
/// state's `lens`, `work` and `codes` arenas are inline arrays rather than separate
/// allocations, and its window is a borrow. So there is nothing left for the
/// allocator to do, and the tracker below observes a high-water mark of zero. C's
/// single allocation is the *container* for the state, which
/// `crates/libz-rs-sys/src/infback.rs` boxes through the caller's `zalloc` and
/// releases through the caller's `zfree` -- and that is where `cover_back()`'s
/// `mem_done` assertions about it belong.
///
/// What this test therefore asserts is the reachable and stronger property: the
/// core touches the caller's allocator not at all, so it cannot leak, cannot free
/// out of order and cannot free a block it was never given. `mem_done`'s three
/// diagnostics (`test/infcover.c` L220-L227) are all vacuously satisfied, and
/// `assert_clean` says so.
#[test]
fn initialisation_and_teardown_are_free_of_allocator_traffic() {
    let tracker = common::TrackingAllocator::new();
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, &tracker).expect("init");
    assert_eq!(
        tracker.total(),
        0,
        "the core allocates no state container -- see the facade"
    );
    assert_eq!(tracker.high_water(), 0, "and never has");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    assert_eq!(tracker.total(), 0, "nothing outstanding");
    assert_eq!(tracker.not_lifo(), 0, "mem_done L223-L224");
    assert_eq!(tracker.rogue(), 0, "mem_done L225-L227");
    let report = tracker.finish();
    assert!(report.is_clean(), "mem_done reported {report:?}");
    assert_eq!(report.high_water, 0);
}

/// An allocation ceiling does not prevent initialisation, because there is nothing
/// to allocate.
///
/// `test/infcover.c` drives `Z_MEM_ERROR` deliberately with `mem_limit(&strm, 1)`
/// (L326-L329), and for `inflateBackInit_` that would fail the state allocation at
/// L51-L53 and return `Z_MEM_ERROR` from L53. **That case is the facade's**, for the
/// reason set out on
/// [`initialisation_and_teardown_are_free_of_allocator_traffic`]: the core allocates
/// nothing here, so no ceiling it could be given would change the outcome.
///
/// Asserting that explicitly is worth more than leaving it unstated. A ceiling of
/// one byte is armed -- which `common::TrackingAllocator`'s own self-test confirms
/// fails every request the library could make -- and initialisation still succeeds,
/// the decode still runs, and the tracker still reports a clean, empty ledger. Any
/// future change that started allocating in the core would fail this test rather
/// than silently acquiring a new failure mode.
#[test]
fn an_allocation_ceiling_does_not_prevent_initialisation() {
    let tracker = common::TrackingAllocator::new();
    tracker.set_limit(1);
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, &tracker)
        .expect("the core allocates nothing, so a ceiling cannot stop it");
    let mut sink = Collector::default();
    let mut source = Declining::default();
    let result = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
    assert_eq!(result.code, ReturnCode::STREAM_END);
    assert_ne!(
        result.code,
        ReturnCode::MEM_ERROR,
        "no allocation means no memory error"
    );
    assert_eq!(sink.written, HELLO);
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    tracker.assert_clean();
}

/// A whole decode through the tracking allocator is clean, and its output is
/// unaffected by the sentinel fill.
///
/// The tracker fills every block it hands out with `0xa5` (`test/infcover.c` L87),
/// so running a real decode through it is what catches a dependence on zeroed
/// memory anywhere the allocator *does* reach. Here it reaches nothing, which the
/// test above establishes -- so this one's job is to confirm that the combination
/// of an instrumented allocator and a sentinel-filled window still produces exactly
/// the bytes the default allocator produces.
///
/// Both a fixed and a dynamic stream are run, so the decode tables are built as
/// well as used.
#[test]
fn a_whole_decode_through_the_tracking_allocator_is_clean() {
    let dynamic = common::h2b(DYNAMIC_LENGTH_EXTRA.hex);
    let dynamic_expected = vec![0_u8; DYNAMIC_LENGTH_EXTRA.decoded_len];
    let prose = common::corpus::text();
    let stored = stored_stream(&prose);
    let tracker = common::TrackingAllocator::new();
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, &tracker).expect("init");
    for (label, stream, expected) in [
        ("fixed", HELLO_FIXED, HELLO),
        ("stored", stored.as_slice(), prose.as_slice()),
        ("dynamic", dynamic.as_slice(), dynamic_expected.as_slice()),
    ] {
        let mut sink = Collector::default();
        let mut source = Declining::default();
        let result = inflate_back(&mut state, Some(stream), &mut source, &mut sink);
        assert_eq!(result.code, ReturnCode::STREAM_END, "{label}");
        assert_eq!(sink.written, expected, "{label}");
    }
    assert_eq!(inflate_back_end(state), ReturnCode::OK);
    tracker.assert_clean();
}

/// The built-in allocator round-trips, which is `cover_back()`'s last step.
///
/// `test/infcover.c` L502-L504 closes the function by initialising and ending once
/// more with no `mem_setup` in force, so that `inflateBackInit_`'s
/// `zalloc`/`zfree` defaulting at L37-L50 -- to `zcalloc` and `zcfree` -- is
/// exercised too:
///
/// ```c
/// ret = inflateBackInit(&strm, 15, win);      assert(ret == Z_OK);
/// ret = inflateBackEnd(&strm);                assert(ret == Z_OK);
/// fputs("inflateBack built-in memory routines\n", stderr);
/// ```
///
/// `GlobalAllocator` is the core's counterpart of that default, and it is what every
/// other test in this file already uses; this one states the round trip on its own so
/// that the `cover_back()` sequence is complete, and adds a decode so that the
/// default path is shown to work rather than merely to be constructible.
#[test]
fn the_built_in_allocator_round_trips() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    let state = inflate_back_init(HARNESS_WINDOW_BITS, &mut window, GlobalAllocator).expect("init");
    assert_eq!(inflate_back_end(state), ReturnCode::OK);

    let outcome = decode(&mut window, HARNESS_WINDOW_BITS, HELLO_FIXED);
    assert_eq!(outcome.code, ReturnCode::STREAM_END);
    assert_eq!(outcome.written, HELLO);
    assert_eq!(outcome.end, ReturnCode::OK);
}

// ===========================================================================
// 7. Hostile input -- no panics, no hangs, no undocumented codes
// ===========================================================================

/// Every status `inflate_back` is permitted to return.
///
/// `zlib.h` L1191-L1204 lists them, and L1204 excludes `Z_OK` outright: "Note that
/// `inflateBack()` cannot return `Z_OK`." `Z_MEM_ERROR` is absent as well, and for a
/// reason established by
/// [`initialisation_and_teardown_are_free_of_allocator_traffic`]: this decoder does
/// not allocate, so it cannot run out of memory. `Z_STREAM_ERROR` *is* permitted, but
/// only for an unusable state -- never as a verdict about data -- so the sweeps below
/// exclude it explicitly rather than tolerating it.
const DOCUMENTED_CODES: [ReturnCode; 3] = [
    ReturnCode::STREAM_END,
    ReturnCode::DATA_ERROR,
    ReturnCode::BUF_ERROR,
];

/// Asserts that an outcome is one of the three statuses a data-driven decode may
/// produce, with a message exactly when it is a data error.
///
/// # Panics
///
/// If the status is undocumented, if a data error carries no message, or if a
/// non-data-error carries one.
fn assert_documented(outcome: &Outcome, context: &str) {
    assert!(
        DOCUMENTED_CODES.contains(&outcome.code),
        "{context}: undocumented status {}",
        outcome.code.as_i32()
    );
    assert_eq!(
        outcome.msg.is_some(),
        outcome.code == ReturnCode::DATA_ERROR,
        "{context}: a message accompanies a data error and nothing else"
    );
    assert!(
        outcome
            .mode
            .is_some_and(|mode| REACHABLE_MODES.contains(&mode)),
        "{context}: came to rest in {:?}",
        outcome.mode
    );
    assert_eq!(outcome.end, ReturnCode::OK, "{context}: teardown");
}

/// Pseudo-random noise never panics, never hangs and always returns a documented
/// status.
///
/// The point of this sweep is not that noise fails -- of course it does -- but *how*
/// it fails. `src/infback.rs` is written so that every loop either makes progress or
/// leaves and so that no path can panic: no `unwrap`, no `expect`, no `panic!` and no
/// panicking index outside its own test module. This is the cheap, broad check of
/// both properties at once. A hang shows up as the harness never finishing, a panic
/// as a failure, and a wrong verdict as an undocumented status.
///
/// `common::lcg_fill` supplies the bytes because there is no `rand` dependency to
/// draw on, and it is deterministic, so a failure here is reproducible from the seed
/// and length alone rather than being a once-a-fortnight mystery.
///
/// Deliberately gated out of Miri: this is 96 whole decodes over a 32 KiB window,
/// which the interpreter would spend a long time on for a class of bug -- an
/// arithmetic or bounds fault on hostile input -- that
/// [`every_raw_vector_from_the_c_harness_behaves_as_the_reference_does`] already
/// exercises under Miri with pinned expectations. It still runs in full natively, on
/// every `cargo test`.
#[test]
#[cfg_attr(
    miri,
    ignore = "broad sweep: slow under the interpreter, runs natively"
)]
fn pseudo_random_noise_never_panics_and_always_terminates() {
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);
    let mut noise = vec![0_u8; 1024];

    for length in [1_usize, 2, 3, 5, 7, 16, 64, 257, 512, 1024] {
        for seed in 0..12_u64 {
            let buffer = &mut noise[..length];
            common::lcg_fill(seed, buffer);
            let outcome = decode(&mut window, HARNESS_WINDOW_BITS, buffer);
            assert_documented(&outcome, &format!("noise seed {seed}, {length} bytes"));
        }
    }
}

/// Every truncation prefix of a valid stream is a `Z_BUF_ERROR`.
///
/// A prefix of a well-formed stream is never *invalid*, only incomplete, so the
/// verdict has to be the same at every length: `Z_BUF_ERROR` with `next_in` nulled,
/// which is `PULL()`'s (`infback.c` L102-L107). Sweeping every length rather than a
/// handful is what makes this a real check of resumability at arbitrary depth --
/// mid-header, mid-alphabet, mid-code, mid-extra-bits and mid-match all get hit,
/// without anyone having to work out where those boundaries fall.
///
/// Two streams are swept, and between them they cover the whole state machine: a
/// couple of hundred bytes of fixed-Huffman literals, which cuts through `TYPE` and
/// `LEN`, and the harness's `long distance and extra` dynamic block, which cuts
/// through `TABLE`'s code-length alphabet and both `inflate_table` calls. Both stay
/// small, because the fixture's size is the whole cost here. Only the full length of
/// each is expected to succeed.
///
/// Gated out of Miri for the same reason as the noise sweep, and for the same
/// reason still valuable natively.
#[test]
#[cfg_attr(
    miri,
    ignore = "broad sweep: slow under the interpreter, runs natively"
)]
fn every_truncation_prefix_of_a_valid_stream_is_a_buffer_error() {
    let prose = common::corpus::text();
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    for (label, stream, expected) in [
        (
            "fixed literals",
            fixed_literal_stream(&prose[..200]),
            prose[..200].to_vec(),
        ),
        (
            DYNAMIC_LONG_DISTANCE.id,
            common::h2b(DYNAMIC_LONG_DISTANCE.hex),
            vec![0_u8; DYNAMIC_LONG_DISTANCE.decoded_len],
        ),
    ] {
        assert!(
            stream.len() < 1024,
            "keep the sweep cheap: `{label}` is {} bytes",
            stream.len()
        );

        for cut in 0..stream.len() {
            let outcome = decode(&mut window, HARNESS_WINDOW_BITS, &stream[..cut]);
            assert_documented(&outcome, &format!("`{label}` prefix of {cut} bytes"));
            assert_eq!(
                outcome.code,
                ReturnCode::BUF_ERROR,
                "a {cut}-byte prefix of the {}-byte `{label}` stream is incomplete",
                stream.len()
            );
            assert!(
                outcome.unused.is_none(),
                "a {cut}-byte prefix of `{label}` ends in an input failure"
            );
            assert_ne!(
                outcome.mode,
                Some(Mode::Done),
                "a prefix of `{label}` never reaches DONE"
            );
        }

        let whole = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
        assert_eq!(
            whole.code,
            ReturnCode::STREAM_END,
            "the full `{label}` stream decodes"
        );
        assert_eq!(whole.written, expected, "`{label}`");
    }
}

/// Every corpus class round-trips through the callback interface.
///
/// `common::corpus` carries the eight payload classes the port's plan specifies for
/// its committed minimal corpus -- empty, single byte, the `test/example.c` `hello`
/// fixture, highly repetitive, incompressible, text, binary and window-crossing --
/// and this drives all of them through `inflate_back` at both the largest and the
/// smallest window, which is what makes the flush behaviour part of the test rather
/// than an accident of fixture size.
///
/// Each class is encoded twice, by [`stored_stream`] and by [`fixed_literal_stream`],
/// so both the `STORED` arm and the fixed-Huffman arm see every payload shape. Neither
/// encoding emits a back-reference, which is what makes the *same* stream valid at both
/// window sizes -- a stream with no distances cannot reach outside a window however
/// small it is. Back-references over the window are covered by
/// [`a_match_reaching_back_further_than_one_flush_uses_the_window`] and by the dynamic
/// vectors, where the distances can be chosen or are already known.
///
/// Gated out of Miri because `window_crossing` alone is 33048 bytes, decoded four
/// times.
/// The classes that matter for the decoder's *logic* rather than its volume are
/// covered under Miri by the vector suite and by
/// [`stored_fixed_and_dynamic_blocks_each_decode`].
#[test]
#[cfg_attr(
    miri,
    ignore = "33 KiB corpus: slow under the interpreter, runs natively"
)]
fn every_corpus_class_round_trips_through_the_callback_interface() {
    // One window at a time, reused across every class and both sizes, so the test
    // never holds more than a single 32 KiB buffer.
    let mut window = sentinel_window(HARNESS_WINDOW_LEN);

    for (name, payload) in common::corpus::all() {
        for (encoding, stream) in [
            ("stored", stored_stream(&payload)),
            ("fixed literals", fixed_literal_stream(&payload)),
        ] {
            let large = decode(&mut window, HARNESS_WINDOW_BITS, &stream);
            assert_eq!(
                large.code,
                ReturnCode::STREAM_END,
                "{name} as {encoding} at windowBits 15"
            );
            assert_eq!(
                large.written, payload,
                "{name} as {encoding} at windowBits 15"
            );
            assert!(
                large.longest_run <= HARNESS_WINDOW_LEN,
                "{name} as {encoding}: zlib.h L1177-L1178 caps a run at the window size"
            );

            // The smallest window, which forces `ROOM()` to flush for every class
            // larger than 256 bytes. The bytes must not change because of it.
            let small = decode(
                &mut window[..1 << MIN_BACK_WINDOW_BITS],
                MIN_BACK_WINDOW_BITS,
                &stream,
            );
            assert_eq!(
                small.code,
                ReturnCode::STREAM_END,
                "{name} as {encoding} at windowBits 8"
            );
            assert_eq!(
                small.written, payload,
                "{name} as {encoding}: the window size must not change the decoded bytes"
            );
            assert!(
                small.longest_run <= 1 << MIN_BACK_WINDOW_BITS,
                "{name} as {encoding}: a run never exceeds the smaller window either"
            );
        }
    }
}

// ===========================================================================
// 8. The `cover_back()` sequence, reproduced in order
// ===========================================================================

/// `cover_back()` from `test/infcover.c` L471-L505, step for step.
///
/// The nine steps and how each is reached here. Three of them are structurally
/// unreachable from a test binary that holds no raw pointers, and each of those
/// three is recorded below with the assertion that replaces it and the crate that
/// owns the original.
///
/// | # | C | Expected | Here |
/// |---|---|---|---|
/// | 1 | `inflateBackInit_(Z_NULL, 0, win, 0, 0)` | `Z_VERSION_ERROR` | **delegated** -- the core takes no version argument |
/// | 2 | `inflateBackInit(Z_NULL, 0, win)` | `Z_STREAM_ERROR` | `window_bits = 0` is rejected; the null `strm` is delegated |
/// | 3 | `inflateBack(Z_NULL, ..)` | `Z_STREAM_ERROR` | a state with no `inflateBack` window is rejected; the null `strm` is delegated |
/// | 4 | `inflateBackEnd(Z_NULL)` | `Z_STREAM_ERROR` | **delegated** -- a `&mut InflateState` cannot be null |
/// | 5 | `"\x03"`, `avail_in = 2` | `Z_STREAM_END` | asserted |
/// | 6 | `"\x63\x00"`, `avail_in = 3`, failing `out()` | `Z_BUF_ERROR` | asserted |
/// | 7 | `pull()` forces `state->mode = SYNC` | `Z_STREAM_ERROR` | **delegated** -- see below |
/// | 8 | `inflateBackEnd`, then `mem_done` | `Z_OK`, clean ledger | asserted |
/// | 9 | init and end with the built-in allocator | `Z_OK`, `Z_OK` | asserted |
///
/// **Step 1 is delegated** because `inflateBackInit_`'s version check (`infback.c`
/// L30-L32) compares the caller's compile-time `ZLIB_VERSION` and `sizeof(z_stream)`
/// against the library's, and neither is this crate's business -- the core has no
/// version parameter at all. `crates/libz-rs-sys/tests` owns it.
///
/// **Steps 2, 3 and 4's null-pointer halves are delegated** for the same class of
/// reason: `Z_NULL` is not representable where a `&mut` is required. What *is*
/// reachable is the other half of each of those conditions, and it is asserted --
/// `windowBits` out of range for step 2, an unusable state for step 3.
///
/// **Step 7 is delegated** to the `#[cfg(test)]` module in `src/infback.rs`. C reaches
/// the `default` arm by having `pull()` reach through its descriptor and assign
/// `state->mode = SYNC` *while the decode is running*; safe Rust makes that
/// impossible, because a callback cannot hold `&mut InflateState` while
/// `inflate_back` does. The arm still exists and still returns `Z_STREAM_ERROR`, and
/// [`a_mode_forced_before_the_call_is_discarded_by_the_entry_reset`] covers the
/// neighbouring guarantee that a mode forced *between* calls is discarded.
///
/// Step 8's `mem_done` is reproduced with `common::TrackingAllocator`, whose verdict
/// is the same three diagnostics C prints: nothing leaked, nothing released out of
/// order, nothing released that was never handed out.
#[test]
fn the_cover_back_sequence_from_the_c_harness_reproduces_step_for_step() {
    // C: `unsigned char win[32768];` (L475). One buffer, reused by every step, as
    // the reference does.
    let mut win = sentinel_window(HARNESS_WINDOW_LEN);

    // ---- Steps 1-4: bad parameters (L477-L483) ------------------------------
    //
    // Step 1 -- `inflateBackInit_(Z_NULL, 0, win, 0, 0) == Z_VERSION_ERROR` --
    // is delegated to the facade; see the table above. `ReturnCode::VERSION_ERROR`
    // exists here and is the code that will be returned there, so the constant is
    // pinned to `zlib.h` L192's `Z_VERSION_ERROR` and nothing else is claimed.
    assert_eq!(ReturnCode::VERSION_ERROR.as_i32(), -6, "zlib.h L192");

    // Step 2 -- `inflateBackInit(Z_NULL, 0, win) == Z_STREAM_ERROR`. C fails this on
    // the null `strm`, but `windowBits = 0` in the same call would fail it just as
    // surely (`infback.c` L33-L35), and that half is reachable.
    assert_eq!(
        inflate_back_init(0, &mut win, GlobalAllocator).err(),
        Some(ReturnCode::STREAM_ERROR),
        "step 2: windowBits 0 is out of the 8..15 range"
    );

    // Step 3 -- `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL) ==
    // Z_STREAM_ERROR`. Reached here through a state that carries no `inflateBack`
    // window, which is C's "the state was not initialized" case (`zlib.h` L1203).
    {
        let mut foreign: InflateState<'_, GlobalAllocator> =
            InflateState::new(InflateConfig::new(-MAX_BACK_WINDOW_BITS), GlobalAllocator)
                .expect("inflate_init2");
        let mut sink = Collector::default();
        let mut source = Declining::default();
        let refused = inflate_back(&mut foreign, None, &mut source, &mut sink);
        assert_eq!(
            refused.code,
            ReturnCode::STREAM_ERROR,
            "step 3: a state without an inflateBack window is unusable"
        );
    }

    // Step 4 -- `inflateBackEnd(Z_NULL) == Z_STREAM_ERROR` -- is delegated: the Rust
    // signature takes the state by value, so there is no null to pass.

    // ---- Steps 5-8: one tracked state, three calls (L485-L500) --------------
    //
    // C: `mem_setup(&strm); inflateBackInit(&strm, 15, win);`
    let tracker = common::TrackingAllocator::new();
    let mut state = inflate_back_init(HARNESS_WINDOW_BITS, &mut win, &tracker)
        .expect("step 5: inflateBackInit(&strm, 15, win) == Z_OK");

    // Step 5 -- `strm.avail_in = 2; strm.next_in = "\x03";
    //            inflateBack(&strm, pull, Z_NULL, push, Z_NULL) == Z_STREAM_END`.
    //
    // `pull` with a null descriptor declines, `push` with a null descriptor accepts,
    // and the two staged bytes are a complete empty fixed block.
    {
        let mut sink = Refusing::after(usize::MAX);
        let mut source = Declining::default();
        let ended = inflate_back(&mut state, Some(EMPTY_FIXED), &mut source, &mut sink);
        assert_eq!(ended.code, ReturnCode::STREAM_END, "step 5");
        assert_eq!(source.calls, 0, "step 5: the staged bytes were enough");
        assert_eq!(
            sink.calls, 0,
            "step 5: an empty stream produces no output, so out() never runs"
        );
    }

    // Step 6 -- `strm.avail_in = 3; strm.next_in = "\x63\x00";
    //            inflateBack(&strm, pull, Z_NULL, push, &strm) == Z_BUF_ERROR`.
    //
    // The only change from step 5 is that `out_desc` is now non-null, which makes
    // `push` return one. Non-zero is failure on the output side.
    {
        let mut sink = Refusing::immediately();
        let mut source = Declining::default();
        let refused = inflate_back(
            &mut state,
            Some(ONE_ZERO_BYTE_FIXED),
            &mut source,
            &mut sink,
        );
        assert_eq!(refused.code, ReturnCode::BUF_ERROR, "step 6");
        assert_eq!(sink.calls, 1, "step 6: out() ran and refused");
        assert!(
            refused.next_in.is_some(),
            "step 6: an output failure leaves next_in non-null"
        );
    }

    // Step 7 -- `inflateBack(&strm, pull, &strm, push, Z_NULL) == Z_STREAM_ERROR` --
    // is delegated to `src/infback.rs`'s own test module; see the table above. The
    // reachable neighbour is asserted instead: the state is still perfectly usable
    // after step 6's failure, because every field the decoder depends on is reset at
    // entry (`infback.c` L214-L223).
    {
        let mut sink = Collector::default();
        let mut source = Declining::default();
        let recovered = inflate_back(&mut state, Some(HELLO_FIXED), &mut source, &mut sink);
        assert_eq!(
            recovered.code,
            ReturnCode::STREAM_END,
            "step 7's neighbour: a failed call leaves the state reusable"
        );
        assert_eq!(sink.written, HELLO);
    }

    // Step 8 -- `inflateBackEnd(&strm) == Z_OK; mem_done(&strm, "inflateBack bad
    // state");`
    assert_eq!(inflate_back_end(state), ReturnCode::OK, "step 8");
    tracker.assert_clean();

    // ---- Step 9: the built-in memory routines (L502-L504) -------------------
    //
    // C: `inflateBackInit(&strm, 15, win) == Z_OK; inflateBackEnd(&strm) == Z_OK;`
    // with no `mem_setup` in force, so `zcalloc`/`zcfree` are defaulted in.
    let builtin = inflate_back_init(HARNESS_WINDOW_BITS, &mut win, GlobalAllocator)
        .expect("step 9: init with the built-in allocator");
    assert_eq!(
        inflate_back_end(builtin),
        ReturnCode::OK,
        "step 9: end with the built-in allocator"
    );
}
