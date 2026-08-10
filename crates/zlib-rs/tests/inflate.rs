// UNSAFE CONTAINMENT, and it is mechanical rather than a convention.  `crates/zlib-rs` is the
// safe core: `src/lib.rs` carries `#![forbid(unsafe_code)]`, and its test suites carry it too, so
// the property "the core and everything that exercises it contains no `unsafe`" is enforced by the
// compiler in both halves.  The workspace's designated FFI boundary -- the only place a raw pointer
// crosses into a foreign implementation -- is `crates/libz-rs-sys/src/**` for the shipped library
// and `crates/zlib-rs-differential/src/{oracle,port}.rs` for the dev-only harness; an assertion
// that needs one of those belongs in a suite of that package, not here.
#![forbid(unsafe_code)]
//! Integration tests for the decompressor: `crates/zlib-rs/src/inflate/**`.
//!
//! `inflate()` is the port's primary untrusted-input attack surface. Every byte it
//! sees may be hostile, so the governing requirement of this suite is blunt: **no
//! input, however malformed, may panic, hang or over-read.** A bad stream must come
//! back as `Z_DATA_ERROR` -- or another documented status -- and stop.
//!
//! # Where the expectations come from
//!
//! Almost nothing here is invented. `test/infcover.c` carries roughly forty
//! **byte-exact** hexadecimal vectors, each paired with the precise
//! `z_stream.msg` string the reference produces, and this file drives every one of
//! them through the safe core:
//!
//! | Group | C origin | Driver here |
//! |---|---|---|
//! | raw errors, raw successes, gzip trailer mismatches | `try()`, `test/infcover.c` L487-L578 | [`try_raw`] |
//! | header and trailer cases | `inf()`, `test/infcover.c` L283-L345 | [`inf`] |
//! | fast-path cases | `cover_fast()`, `test/infcover.c` L642-L659 | [`inf`] |
//! | dictionary recovery, sync recovery | `test_dict_inflate`, `test_sync`, `test/example.c` | dedicated tests |
//!
//! The eighteen message strings asserted below were each confirmed to appear
//! verbatim in the in-tree `inflate.c` and `inffast.c`, and independently
//! cross-checked against a reference build's own `z_stream.msg`. They are a
//! caller-visible contract: `test/infcover.c` L549 compares them with `strcmp`, so a
//! rephrased message is a breaking change, not a cosmetic one.
//!
//! # Reachability -- what this suite can and cannot touch
//!
//! An integration test is an ordinary downstream consumer, so it sees exactly the
//! public surface and nothing else. That is the point: it proves the API is usable
//! from outside, and it cannot accidentally assert on an internal detail.
//!
//! Reachable and exercised here: `Mode` (and its `ALL`, `COUNT`,
//! `MIN_DISCRIMINANT`, `MAX_DISCRIMINANT`, `is_valid_tag`, `is_error_free`,
//! `is_before_trailer` surface), `InflateState`, `InflateStream`, `lenfix`,
//! `distfix`, `Code`, the `ENOUGH` family, and the seventeen entry points
//! `inflate`, `inflate_init`, `inflate_init2`, `inflate_end`, `inflate_reset`,
//! `inflate_reset2`, `inflate_reset_keep`, `inflate_prime`, `inflate_mark`,
//! `inflate_sync`, `inflate_sync_point`, `inflate_codes_used`, `inflate_validate`,
//! `inflate_undermine`, `inflate_copy`, `inflate_get_dictionary`,
//! `inflate_set_dictionary`, plus `inflate_get_header` and `inflate_state_check`.
//!
//! Not reachable, because `zlib.map` puts them in a `local:` block and the port
//! honours that: `inflate_fixed`, `inflate_fast`, `update_window` and `syncsearch`
//! are `pub(crate)`, and the `inffast` and `window` submodules are themselves
//! crate-private. This suite therefore asserts their *consequences* -- reaching the
//! fast path through the output-length trick described below, and reaching a window
//! update through the vectors named for it -- never the functions.
//!
//! ## Delegation note 1 -- the `ENOUGH`-exceeded cases
//!
//! `cover_trees()` (`test/infcover.c` L619-L640) calls `inflate_table(DISTS, ...)`
//! **directly**, for the reason its own comment gives: "zlib insures that enough is
//! always enough", so the not-enough return is unreachable through the public API.
//! That remains true here even though this port's `inflate_table` happens to be
//! `#[doc(hidden)] pub` (it has to be callable by the C ABI facade, which wraps it
//! for the unmodified `test/infcover.c` to link against). A test binary that reached
//! in that way would be asserting on a hidden item rather than on the API, so both
//! of the C harness's calls -- the `bits = 15` variant and the `bits = 1` variant,
//! each expecting the return value `1` -- are **delegated** to the
//! `#[cfg(test)] mod tests` block inside `crates/zlib-rs/src/inflate/inftrees.rs`,
//! where `enough_is_reported_when_the_table_would_not_fit` already makes them.
//! What this suite asserts instead is the complementary public-path property: the
//! `ENOUGH` capacity is never exceeded and no legitimate stream is ever refused for
//! want of table space. See `code_tables_never_exceed_the_enough_capacity`.
//!
//! ## Delegation note 2 -- the `gz_header` scalars
//!
//! `inflate_get_header` moves a `GzHeaderSink` **into** the state, and the state
//! exposes no getter for it, so the scalars the parse writes back -- `done`,
//! `extra_len`, `text`, `time`, `xflags`, `os`, `hcrc` -- cannot be read from
//! outside the crate. Their transitions are **delegated** to the
//! `#[cfg(test)] mod tests` block inside `crates/zlib-rs/src/inflate/header.rs`
//! (`done_reports_all_three_states`,
//! `done_is_cleared_before_the_zlib_header_is_validated`,
//! `inflate_get_header_resets_done`). What is observable from here, and what this
//! suite therefore asserts, is the part that actually matters for security: the
//! caller's `extra`, `name` and `comment` buffers are shared `Cell` slices, so every
//! byte the parse writes is visible, and a guard region past each advertised maximum
//! proves nothing was written beyond it.
//!
//! Neither delegation is a licence to widen visibility in `src/**`: the hidden-symbol
//! set is what `zlib.map`'s `local:` block declares, and `Makefile.in`'s `rust-symbols`
//! target is what checks the built library against it.
//!
//! # Two mechanical facts worth stating once
//!
//! * **Import by module path, never by crate-root re-export.** `zlib-rs` declares
//!   `default = []` and every root re-export carries `#[cfg(feature = "rust-api")]`,
//!   so `zlib_rs::error::ReturnCode` always exists while `zlib_rs::ReturnCode` does
//!   not. Nothing below is feature-gated, and this file compiles identically under
//!   `--no-default-features`, `--features simd`, `--features std` and
//!   `--all-features`.
//! * **This is its own crate and it links `std`.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test binary is
//!   not, so no `#![no_std]` appears here. There is no `unsafe`, no `extern "C"`, no
//!   raw pointer and no third-party crate anywhere in this file -- `zlib-rs` has an
//!   empty `[dependencies]` table and no `[dev-dependencies]`, which the `[bans]`
//!   section of `deny.toml` enforces, so the harness is the built-in `#[test]` one
//!   and the pseudo-random filler is `common::lcg_fill` rather than `rand`.

// The workspace denies the panic family and slice indexing, because that is the
// property the decoder itself must have. A test that cannot assert is useless, so the
// harness opts back in. `clippy.toml`'s `allow-unwrap-in-tests`,
// `allow-expect-in-tests` and `allow-panic-in-tests` keys cover code inside a
// `#[test]` function; the file-scope drivers below are not `#[test]` functions, so the
// relaxation has to be stated at crate level to reach them too.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod common;

use core::cell::Cell;

use common::corpus::{DICTIONARY, HELLO};
use common::{check_err, h2b, lcg_fill, TrackingAllocator};

use zlib_rs::adler32::adler32;
use zlib_rs::allocate::{Allocator, GlobalAllocator};
use zlib_rs::config::{
    DeflateConfig, InflateConfig, Strategy, Z_BLOCK, Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH,
    Z_SYNC_FLUSH, Z_TREES,
};
use zlib_rs::crc32::crc32;
use zlib_rs::deflate::state::GzHeaderView;
use zlib_rs::deflate::{
    deflate, deflate_bound, deflate_end, deflate_init2, deflate_reset, deflate_set_dictionary,
    deflate_set_header, DeflateStream,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::inftrees::{Code, ENOUGH, ENOUGH_DISTS, ENOUGH_LENS, MAXBITS};
use zlib_rs::inflate::state::{GzHeaderSink, WrapFlags};
use zlib_rs::inflate::{
    distfix, inflate, inflate_codes_used, inflate_copy, inflate_end, inflate_get_dictionary,
    inflate_get_header, inflate_init, inflate_init2, inflate_mark, inflate_prime, inflate_reset,
    inflate_reset2, inflate_reset_keep, inflate_set_dictionary, inflate_state_check, inflate_sync,
    inflate_sync_point, inflate_undermine, inflate_validate, lenfix, InflateState, InflateStream,
    Mode, INFLATE_CODES_USED_BAD_STATE, INFLATE_MARK_BAD_STATE,
};
use zlib_rs::read_buf::OutputRegion;

// ---------------------------------------------------------------------------
// Window-bits spellings, in the reference's own numbering (`zlib.h` L859-L888).
// ---------------------------------------------------------------------------

/// Raw DEFLATE with the largest window: no wrapper, no check value.
const RAW: i32 = -15;

/// Raw DEFLATE with a 256-byte window, which is what forces the window-wrap and
/// split-update paths the `-8` vectors are named for.
const RAW_SMALL: i32 = -8;

/// A zlib stream (RFC 1950) with the largest window.
const ZLIB: i32 = 15;

/// A gzip member (RFC 1952) with the largest window: `15 + 16`.
const GZIP: i32 = 31;

/// Automatic zlib-or-gzip detection: `15 + 32`. `inf()` treats this value specially
/// and installs a `gz_header`, exactly as `test/infcover.c` L300-L310 does.
const AUTODETECT: i32 = 47;

/// Take the window size from the zlib header rather than from the caller.
const FROM_HEADER: i32 = 0;

// ---------------------------------------------------------------------------
// The reference's message strings. Each is verbatim from `inflate.c` or
// `inffast.c`; each lands in `z_stream.msg`; each is compared with `strcmp` by
// `test/infcover.c` L549.
// ---------------------------------------------------------------------------

const MSG_STORED_LENGTHS: &str = "invalid stored block lengths";
const MSG_BLOCK_TYPE: &str = "invalid block type";
const MSG_TOO_MANY_SYMBOLS: &str = "too many length or distance symbols";
const MSG_CODE_LENGTHS_SET: &str = "invalid code lengths set";
const MSG_BIT_LENGTH_REPEAT: &str = "invalid bit length repeat";
const MSG_MISSING_END_OF_BLOCK: &str = "invalid code -- missing end-of-block";
const MSG_LITERAL_LENGTHS_SET: &str = "invalid literal/lengths set";
const MSG_DISTANCES_SET: &str = "invalid distances set";
const MSG_LITERAL_LENGTH_CODE: &str = "invalid literal/length code";
const MSG_DISTANCE_CODE: &str = "invalid distance code";
const MSG_DISTANCE_TOO_FAR: &str = "invalid distance too far back";
const MSG_DATA_CHECK: &str = "incorrect data check";
const MSG_LENGTH_CHECK: &str = "incorrect length check";
const MSG_HEADER_CHECK: &str = "incorrect header check";
const MSG_COMPRESSION_METHOD: &str = "unknown compression method";
const MSG_WINDOW_SIZE: &str = "invalid window size";
const MSG_HEADER_FLAGS: &str = "unknown header flags set";
const MSG_HEADER_CRC: &str = "header crc mismatch";

/// Every message the decoder is permitted to publish.
///
/// Used where the C harness checks only the status and not the text: the message
/// still has to be one the reference could have produced, which catches a
/// hand-written or reworded string even when the exact expectation is not pinned.
const EVERY_MESSAGE: [&str; 18] = [
    MSG_STORED_LENGTHS,
    MSG_BLOCK_TYPE,
    MSG_TOO_MANY_SYMBOLS,
    MSG_CODE_LENGTHS_SET,
    MSG_BIT_LENGTH_REPEAT,
    MSG_MISSING_END_OF_BLOCK,
    MSG_LITERAL_LENGTHS_SET,
    MSG_DISTANCES_SET,
    MSG_LITERAL_LENGTH_CODE,
    MSG_DISTANCE_CODE,
    MSG_DISTANCE_TOO_FAR,
    MSG_DATA_CHECK,
    MSG_LENGTH_CHECK,
    MSG_HEADER_CHECK,
    MSG_COMPRESSION_METHOD,
    MSG_WINDOW_SIZE,
    MSG_HEADER_FLAGS,
    MSG_HEADER_CRC,
];

// ---------------------------------------------------------------------------
// The vectors. Transcribed from `test/infcover.c`; the hexadecimal spelling is the
// reference's own, including its one-digit tokens, which is why the decoder used is
// `common::h2b` (the port of that file's liberal `h2b`, L245-L272) rather than a
// strict pair-wise reader.
// ---------------------------------------------------------------------------

/// `try(hex, id, 1)` from `cover_inflate()` (`test/infcover.c` L582-L595): raw
/// DEFLATE streams that must fail with `Z_DATA_ERROR` and exactly the named message.
const RAW_ERROR_VECTORS: [(&str, &str); 12] = [
    ("0 0 0 0 0", MSG_STORED_LENGTHS),
    ("6", MSG_BLOCK_TYPE),
    ("fc 0 0", MSG_TOO_MANY_SYMBOLS),
    ("4 0 fe ff", MSG_CODE_LENGTHS_SET),
    ("4 0 24 49 0", MSG_BIT_LENGTH_REPEAT),
    ("4 0 24 e9 ff ff", MSG_BIT_LENGTH_REPEAT),
    ("4 0 24 e9 ff 6d", MSG_MISSING_END_OF_BLOCK),
    (
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        MSG_LITERAL_LENGTHS_SET,
    ),
    ("4 80 49 92 24 49 92 24 f b4 ff ff c3 84", MSG_DISTANCES_SET),
    ("4 c0 81 8 0 0 0 0 20 7f eb b 0 0", MSG_LITERAL_LENGTH_CODE),
    ("2 7e ff ff", MSG_DISTANCE_CODE),
    ("c c0 81 0 0 0 0 0 90 ff 6b 4 0", MSG_DISTANCE_TOO_FAR),
];

/// `try(hex, id, 0)` from `cover_inflate()`: raw DEFLATE streams that must decode.
///
/// The third element is the exact number of bytes the reference produces. Every one
/// of these payloads is a run of zero bytes, which is what makes an exact
/// whole-output comparison possible without carrying a fixture file: the expectations
/// were taken from a reference decode of each vector.
const RAW_SUCCESS_VECTORS: [(&str, &str, usize); 7] = [
    ("3 0", "fixed", 0),
    ("1 1 0 fe ff 0", "stored", 1),
    ("5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c", "pull 17", 0),
    (
        "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
        "long code",
        0,
    ),
    (
        "ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f",
        "length extra",
        516,
    ),
    (
        "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        "long distance and extra",
        518,
    ),
    (
        "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
         0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        "window end",
        33_025,
    ),
];

/// `try(hex, id, -1)` from `cover_inflate()` (`test/infcover.c` L598-L601): gzip
/// members whose trailer does not match, checked through automatic detection because
/// `try()` passes `err < 0 ? 47 : -15`.
const GZIP_TRAILER_VECTORS: [(&str, &str); 2] = [
    ("1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1", MSG_DATA_CHECK),
    (
        "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
        MSG_LENGTH_CHECK,
    ),
];

/// One row of the `inf()` table: `(hex, what, step, window_bits, out_len, expected)`.
///
/// `expected` is the status the **first** `inflate` call must return, which is the
/// only one `test/infcover.c` L319 asserts; `step` is the chunk size, zero meaning
/// "all of it".
type InfVector = (&'static str, &'static str, usize, i32, usize, ReturnCode);

/// The header, trailer and window cases from `cover_support()` (`test/infcover.c`
/// L366-L370), `cover_wrap()` (L399-L410) and the two `inf()` calls at the end of
/// `cover_inflate()` (L611-L613).
///
/// The empty-input `"bad window size"` row is deliberately included: `window_bits`
/// of 1 is rejected by initialisation, so it exercises the path where no state is
/// ever built.
const INF_VECTORS: [InfVector; 18] = [
    ("63 0", "force window allocation", 0, RAW, 1, ReturnCode::OK),
    (
        "63 18 5",
        "force window replacement",
        0,
        RAW_SMALL,
        259,
        ReturnCode::OK,
    ),
    (
        "63 18 68 30 d0 0 0",
        "force split window update",
        4,
        RAW_SMALL,
        259,
        ReturnCode::OK,
    ),
    ("3 0", "use fixed blocks", 0, RAW, 1, ReturnCode::STREAM_END),
    ("", "bad window size", 0, 1, 0, ReturnCode::STREAM_ERROR),
    (
        "1f 8b 0 0",
        "bad gzip method",
        0,
        GZIP,
        0,
        ReturnCode::DATA_ERROR,
    ),
    (
        "1f 8b 8 80",
        "bad gzip flags",
        0,
        GZIP,
        0,
        ReturnCode::DATA_ERROR,
    ),
    (
        "77 85",
        "bad zlib method",
        0,
        ZLIB,
        0,
        ReturnCode::DATA_ERROR,
    ),
    (
        "8 99",
        "set window size from header",
        0,
        FROM_HEADER,
        0,
        ReturnCode::OK,
    ),
    (
        "78 9c",
        "bad zlib window size",
        0,
        8,
        0,
        ReturnCode::DATA_ERROR,
    ),
    (
        "78 9c 63 0 0 0 1 0 1",
        "check adler32",
        0,
        ZLIB,
        1,
        ReturnCode::STREAM_END,
    ),
    (
        "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
        "bad header crc",
        0,
        AUTODETECT,
        1,
        ReturnCode::DATA_ERROR,
    ),
    (
        "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
        "check gzip length",
        0,
        AUTODETECT,
        0,
        ReturnCode::STREAM_END,
    ),
    (
        "78 90",
        "bad zlib header check",
        0,
        AUTODETECT,
        0,
        ReturnCode::DATA_ERROR,
    ),
    (
        "8 b8 0 0 0 1",
        "need dictionary",
        0,
        8,
        0,
        ReturnCode::NEED_DICT,
    ),
    ("78 9c 63 0", "compute adler32", 0, ZLIB, 1, ReturnCode::OK),
    (
        "2 8 20 80 0 3 0",
        "inflate_fast TYPE return",
        0,
        RAW,
        258,
        ReturnCode::STREAM_END,
    ),
    (
        "63 18 5 40 c 0",
        "window wrap",
        3,
        RAW_SMALL,
        300,
        ReturnCode::OK,
    ),
];

/// `cover_fast()` (`test/infcover.c` L642-L659), every row with `window_bits` `-8`.
///
/// The `258` and `259` output lengths are load-bearing, not incidental:
/// `inflate()` dispatches to the unrolled decode loop only when
/// `avail_in >= 6 && avail_out >= 258` (`inflate.c` L915-L922), so these lengths are
/// the only way a caller can reach `inflate_fast` -- which is `pub(crate)` and
/// therefore not callable from here. Shrinking any of them silently moves the test
/// onto the slow path and stops covering what it is named for.
const FAST_VECTORS: [InfVector; 8] = [
    (
        "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 \
         ff 7f 0f 0 0 0",
        "fast length extra bits",
        0,
        RAW_SMALL,
        258,
        ReturnCode::DATA_ERROR,
    ),
    (
        "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 \
         50 fe ff ff 3f 0 0",
        "fast distance extra bits",
        0,
        RAW_SMALL,
        258,
        ReturnCode::DATA_ERROR,
    ),
    (
        "3 7e 0 0 0 0 0",
        "fast invalid distance code",
        0,
        RAW_SMALL,
        258,
        ReturnCode::DATA_ERROR,
    ),
    (
        "1b 7 0 0 0 0 0",
        "fast invalid literal/length code",
        0,
        RAW_SMALL,
        258,
        ReturnCode::DATA_ERROR,
    ),
    (
        "d c7 1 ae eb 38 c 4 41 a0 87 72 de df fb 1f b8 36 b1 38 5d ff ff 0",
        "fast 2nd level codes and too far back",
        0,
        RAW_SMALL,
        258,
        ReturnCode::DATA_ERROR,
    ),
    (
        "63 18 5 8c 10 8 0 0 0 0",
        "very common case",
        0,
        RAW_SMALL,
        259,
        ReturnCode::OK,
    ),
    (
        "63 60 60 18 c9 0 8 18 18 18 26 c0 28 0 29 0 0 0",
        "contiguous and wrap around window",
        6,
        RAW_SMALL,
        259,
        ReturnCode::OK,
    ),
    (
        "63 0 3 0 0 0 0 0",
        "copy direct from output",
        0,
        RAW_SMALL,
        259,
        ReturnCode::STREAM_END,
    ),
];

/// The messages the five `cover_fast()` failures produce.
///
/// `inf()` checks only the status, so these were taken from a reference build's own
/// `z_stream.msg` and are asserted here as well: the fast decode loop has its own
/// copies of three of the eighteen strings (`inffast.c`), and a divergence between
/// the two copies is exactly the kind of slip that a status-only assertion misses.
const FAST_MESSAGES: [(&str, &str); 5] = [
    ("fast length extra bits", MSG_DISTANCE_TOO_FAR),
    ("fast distance extra bits", MSG_DISTANCE_TOO_FAR),
    ("fast invalid distance code", MSG_DISTANCE_CODE),
    ("fast invalid literal/length code", MSG_LITERAL_LENGTH_CODE),
    (
        "fast 2nd level codes and too far back",
        MSG_DISTANCE_TOO_FAR,
    ),
];

/// The messages the `INF_VECTORS` failures produce, for the same reason.
const INF_MESSAGES: [(&str, &str); 6] = [
    ("bad gzip method", MSG_COMPRESSION_METHOD),
    ("bad gzip flags", MSG_HEADER_FLAGS),
    ("bad zlib method", MSG_COMPRESSION_METHOD),
    ("bad zlib window size", MSG_WINDOW_SIZE),
    ("bad header crc", MSG_HEADER_CRC),
    ("bad zlib header check", MSG_HEADER_CHECK),
];

// ---------------------------------------------------------------------------
// Drivers.
// ---------------------------------------------------------------------------

/// Everything one decode run produced.
///
/// `test/infcover.c` throws all of this away; keeping it is what lets a Rust test
/// assert on the numbers rather than eyeball a log line.
#[derive(Debug, Clone)]
struct Outcome {
    /// The status of the **first** `inflate` call -- the only one `inf()` asserts
    /// (`test/infcover.c` L319, after which it sets `err = 9`, "don't care").
    first: ReturnCode,
    /// The status of the last `inflate` call.
    last: ReturnCode,
    /// Every byte the run wrote, concatenated across calls.
    output: Vec<u8>,
    /// `z_stream.msg` as it stood when the run ended.
    msg: Option<&'static str>,
    /// `z_stream.total_in`.
    total_in: u64,
    /// `z_stream.total_out`.
    total_out: u64,
    /// `z_stream.adler`: the running or final check value.
    adler: u32,
    /// `z_stream.data_type`.
    data_type: i32,
    /// How many `inflate` calls the run took.
    calls: u32,
    /// How many mid-stream `inflate_copy` round trips succeeded. `inf()` performs one
    /// per loop iteration (`test/infcover.c` L335-L336); every other driver leaves
    /// this at zero.
    copies: u32,
}

impl Outcome {
    /// The outcome of a run that never started, because initialisation was refused.
    fn refused(err: ReturnCode) -> Self {
        Self {
            first: err,
            last: err,
            output: Vec::new(),
            msg: None,
            total_in: 0,
            total_out: 0,
            adler: 0,
            data_type: 0,
            calls: 0,
            copies: 0,
        }
    }

    /// Asserts the run ended in one of the statuses a caller must be prepared for.
    ///
    /// This is the "no panic, no hang, no over-read" property stated as an assertion:
    /// arbitrary bytes may be refused, may ask for a dictionary, may run out of room
    /// or may be accepted, and nothing else is allowed to happen.
    fn assert_documented_status(&self, what: &str) {
        assert!(
            matches!(
                self.last,
                ReturnCode::OK
                    | ReturnCode::STREAM_END
                    | ReturnCode::NEED_DICT
                    | ReturnCode::BUF_ERROR
                    | ReturnCode::DATA_ERROR
            ),
            "{what}: undocumented status {:?}",
            self.last
        );
        if self.last == ReturnCode::DATA_ERROR {
            let msg = self.msg.expect("a data error must carry a message");
            assert!(
                EVERY_MESSAGE.contains(&msg),
                "{what}: {msg:?} is not one of the reference's messages"
            );
        }
    }
}

/// The value `inflateReset` leaves in `z_stream.adler` for `window_bits`.
///
/// `inflate.c` L107-L108 writes `strm->adler = state->wrap & 1`, so a zlib or gzip
/// stream reports the Adler-32 seed 1 before any byte is decoded and a raw stream
/// reports 0. Reproducing that is what makes these drivers agree with the reference
/// on `adler` for a stream that fails inside the header and never produces output.
/// The state's own wrap flags are crate-private, so the value is recomputed from the
/// configuration through the same public helpers the facade uses.
fn adler_seed(window_bits: i32) -> u32 {
    InflateConfig::new(window_bits)
        .validate()
        .map_or(0, |validated| {
            WrapFlags::from_request(validated.wrap).adler_seed()
        })
}

/// How much output room to offer per call for an input of `input_len` bytes.
///
/// `test/infcover.c` L512 uses `len << 3`; the floor keeps an empty or one-byte input
/// from being handed a zero-length buffer, which would make no progress possible.
fn output_room(input_len: usize) -> usize {
    input_len.saturating_mul(8).saturating_add(512)
}

/// Drives a decode to completion over a state the caller owns.
///
/// The loop is `try()`'s (`test/infcover.c` L516-L523): feed `step` bytes at a time
/// (zero meaning all of them), re-offer `out_len` bytes of room on every call, and
/// keep going while there is input left to feed **or** the last call filled the
/// output buffer exactly -- which is C's `while (strm.avail_in || strm.avail_out == 0)`
/// and is what lets an input that expands by more than `out_len` finish.
///
/// Three safeguards make "never hangs" an assertion rather than a hope: a run of
/// three calls that consume no input and produce no output ends the loop, a hard
/// iteration cap derived from DEFLATE's maximum expansion ratio panics with a clear
/// message, and every terminal status ends the loop immediately.
fn drive<'a, A>(
    state: &mut InflateState<'a, A>,
    bytes: &[u8],
    step: usize,
    out_len: usize,
    flush: i32,
    seed: u32,
) -> Outcome
where
    A: Allocator<'a> + Copy,
{
    let total = bytes.len();
    // L513-L514: `if (step == 0 || step > have) step = have;`
    let step = if step == 0 || step > total {
        total
    } else {
        step
    };

    // A DEFLATE stream expands by at most 1032 bytes per input byte, so this bounds
    // the number of calls any input can legitimately need.
    let cap = u32::try_from(
        total
            .saturating_mul(1100)
            .saturating_div(out_len.max(1))
            .saturating_add(total)
            .saturating_add(64),
    )
    .unwrap_or(u32::MAX);

    let mut read_at = 0_usize;
    let mut chunk_end = step.min(total);
    let mut output = Vec::new();
    let mut first = None;
    // Declared without an initial value on purpose: every `break` below sits after the
    // assignment, so the compiler can prove it is set, and a placeholder would be a
    // value that is written and never read.
    let mut last;
    // `msg` *does* start at `None`, and is carried into every call rather than being
    // read back only at the end: C's `z_stream` persists across calls, so a message the
    // decoder set on one call is still there on the next -- the `BAD` arm reports
    // `Z_DATA_ERROR` again without rewriting it (`inflate.c` L1115).
    let mut msg = None;
    let mut total_in = 0_u64;
    let mut total_out = 0_u64;
    let mut adler = seed;
    let mut data_type = 0_i32;
    let mut calls = 0_u32;
    let mut idle = 0_u32;

    loop {
        assert!(calls < cap, "inflate did not terminate after {calls} calls");

        let mut buffer = vec![0_u8; out_len];
        let mut stream = InflateStream::new(&bytes[read_at..chunk_end], &mut buffer);
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        // `adler` is deliberately not seeded: the facade hands the core `None` and
        // adopts a value only where C assigns `strm->adler`, so the carried value
        // below is the caller's own member rather than a round trip through the view.
        stream.data_type = data_type;

        let ret = inflate(state, &mut stream, flush);
        calls = calls.saturating_add(1);

        let progressed = stream.next_in != 0 || stream.next_out != 0;
        let filled = out_len != 0 && stream.next_out == out_len;
        output.extend_from_slice(stream.written());
        read_at = read_at.saturating_add(stream.next_in);
        total_in = stream.total_in;
        total_out = stream.total_out;
        if let Some(value) = stream.adler {
            adler = value;
        }
        data_type = stream.data_type;
        msg = stream.msg;
        if first.is_none() {
            first = Some(ret);
        }
        last = ret;

        if matches!(
            ret,
            ReturnCode::STREAM_END
                | ReturnCode::DATA_ERROR
                | ReturnCode::MEM_ERROR
                | ReturnCode::STREAM_ERROR
                | ReturnCode::NEED_DICT
        ) {
            break;
        }

        idle = if progressed {
            0
        } else {
            idle.saturating_add(1)
        };
        if idle >= 3 {
            break;
        }

        // L338-L340: return the unconsumed remainder to the pool and take a new chunk.
        chunk_end = read_at.saturating_add(step.min(total.saturating_sub(read_at)));
        if chunk_end == read_at && !filled {
            break;
        }
    }

    Outcome {
        first: first.unwrap_or(last),
        last,
        output,
        msg,
        total_in,
        total_out,
        adler,
        data_type,
        calls,
        copies: 0,
    }
}

/// Decodes `bytes` with `window_bits`, feeding `step` bytes and offering `out_len`
/// bytes of room per call.
fn decode_chunked(bytes: &[u8], window_bits: i32, step: usize, out_len: usize) -> Outcome {
    let mut state = match inflate_init2(InflateConfig::new(window_bits), GlobalAllocator) {
        Ok(state) => state,
        Err(err) => return Outcome::refused(err),
    };
    let outcome = drive(
        &mut state,
        bytes,
        step,
        out_len,
        Z_NO_FLUSH,
        adler_seed(window_bits),
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK, "inflateEnd");
    outcome
}

/// Decodes `bytes` with `window_bits` in as few calls as the buffers allow.
fn decode(bytes: &[u8], window_bits: i32) -> Outcome {
    decode_chunked(bytes, window_bits, 0, output_room(bytes.len()))
}

/// The byte the guard region past every advertised `gz_header` maximum is filled with.
///
/// Deliberately not `0xa5`: that is the allocator's own sentinel, so a distinct value
/// keeps "the header parser wrote here" and "a fresh allocation was handed out here"
/// from being confusable.
const HEADER_GUARD: u8 = 0x5a;

/// How many guard bytes sit past each advertised maximum.
const HEADER_GUARD_LEN: usize = 32;

/// `try(hex, id, err)` -- the `inflate()` half (`test/infcover.c` L487-L546).
///
/// A raw decode of a hexadecimal vector, or a gzip decode through automatic detection
/// when `err` is negative, exactly as C's `err < 0 ? 47 : -15` selects. `err` follows
/// the C convention: non-zero means the stream must be refused with `Z_DATA_ERROR`
/// **and** `id` must be the message, zero means it must decode.
///
/// The `inflateBack()` half of `try()` is not reproduced here; `inflateBack` is
/// `crate::infback`'s public surface and belongs to that module's own suite.
fn try_raw(hex: &str, id: &'static str, err: i32) -> Outcome {
    let bytes = h2b(hex);
    let window_bits = if err < 0 { AUTODETECT } else { RAW };
    // L512: `size = len << 3`.
    let out_len = bytes.len().saturating_mul(8);

    let tracker = TrackingAllocator::new();
    let outcome = {
        let mut state = inflate_init2(InflateConfig::new(window_bits), &tracker)
            .expect("inflateInit2 accepts a raw or auto-detect window");
        let outcome = drive(
            &mut state,
            &bytes,
            0,
            out_len,
            Z_TREES,
            adler_seed(window_bits),
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK, "{id}: inflateEnd");
        outcome
    };
    // `mem_done` (L546): no leak, no out-of-order release, no unrecognised release.
    tracker.assert_clean();

    // L520: neither status may appear, on any call. Both are terminal in `drive`, so
    // asserting the last status is exactly asserting it of every call.
    assert!(
        !matches!(
            outcome.last,
            ReturnCode::STREAM_ERROR | ReturnCode::MEM_ERROR
        ),
        "{id}: unexpected {:?}",
        outcome.last
    );

    if err == 0 {
        assert!(
            matches!(outcome.last, ReturnCode::STREAM_END | ReturnCode::OK),
            "{id}: expected the stream to decode, got {:?}",
            outcome.last
        );
        assert_eq!(outcome.msg, None, "{id}: a clean decode leaves msg unset");
    } else {
        // L525-L528: `assert(ret == Z_DATA_ERROR); assert(strcmp(id, strm.msg) == 0);`
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{id}: status");
        assert_eq!(outcome.msg, Some(id), "{id}: z_stream.msg");
    }
    outcome
}

/// `inf(hex, what, step, win, len, err)` -- the whole of `test/infcover.c` L283-L346.
///
/// Everything the C function does, in its order: instrument the allocator, initialise,
/// install a `gz_header` when `win` is 47, feed the input in `step`-sized chunks while
/// re-offering `len` bytes of room, assert the **first** status, service a
/// `Z_NEED_DICT` with the same five-step dance C performs, copy and immediately end
/// the state on every iteration, then `inflateReset2(-8)`, `inflateEnd`, and check the
/// allocator's books.
///
/// Two deliberate departures, both forced and both narrow:
///
/// * C points `head.extra`, `head.name` and `head.comment` at the very buffer it also
///   uses for output. Safe Rust cannot hold `&mut [u8]` and `&[Cell<u8>]` over the
///   same bytes, so the header fields share a **separate** buffer of the same length.
///   The substantive property of the fixture -- all three fields aliasing one buffer,
///   each advertising the full length -- is preserved; only the incidental overlap with
///   the output buffer is not.
/// * That buffer carries [`HEADER_GUARD_LEN`] extra bytes past the advertised maximum,
///   which C has no way to check. Nothing may write into them.
fn inf(vector: InfVector) -> Outcome {
    let (hex, what, step, win, out_len, expected) = vector;
    let bytes = h2b(hex);
    let tracker = TrackingAllocator::new();

    // Declared before the state, so the sink's borrow outlives the state that holds it.
    let header_cells: Vec<Cell<u8>> =
        vec![Cell::new(HEADER_GUARD); out_len.saturating_add(HEADER_GUARD_LEN)];

    let mut state = match inflate_init2(InflateConfig::new(win), &tracker) {
        Ok(state) => state,
        Err(err) => {
            // L294-L297: initialisation refused, so `inf()` reports and returns.
            assert_eq!(err, expected, "{what}: inflateInit2({win})");
            tracker.assert_clean();
            return Outcome::refused(err);
        }
    };

    // L300-L310.
    if win == AUTODETECT {
        let field = &header_cells[..out_len];
        let sink = GzHeaderSink::new(Some(field), Some(field), Some(field));
        assert_eq!(
            inflate_get_header(&mut state, sink),
            ReturnCode::OK,
            "{what}: inflateGetHeader"
        );
    }

    let total = bytes.len();
    // L311-L315.
    let step = if step == 0 || step > total {
        total
    } else {
        step
    };
    let cap = u32::try_from(
        total
            .saturating_mul(1100)
            .saturating_div(out_len.max(1))
            .saturating_add(total)
            .saturating_add(64),
    )
    .unwrap_or(u32::MAX);

    let mut read_at = 0_usize;
    let mut chunk_end = step.min(total);
    let mut output = Vec::new();
    let mut first = None;
    // See the notes in `drive`.
    let mut last;
    let mut msg = None;
    let mut total_in = 0_u64;
    let mut total_out = 0_u64;
    let mut adler = adler_seed(win);
    let mut data_type = 0_i32;
    let mut calls = 0_u32;
    let mut copies = 0_u32;

    loop {
        assert!(calls < cap, "{what}: inflate did not terminate");

        // L317-L318: the output buffer is re-offered whole on every call.
        let mut buffer = vec![0_u8; out_len];
        let mut stream = InflateStream::new(&bytes[read_at..chunk_end], &mut buffer);
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        // `adler` is deliberately not seeded: the facade hands the core `None` and
        // adopts a value only where C assigns `strm->adler`, so the carried value
        // below is the caller's own member rather than a round trip through the view.
        stream.data_type = data_type;

        let mut ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        calls = calls.saturating_add(1);
        // L319: `assert(err == 9 || ret == err)` -- only the first call is pinned.
        if first.is_none() {
            assert_eq!(ret, expected, "{what}: first inflate status");
            first = Some(ret);
        }

        // L320. Named `keep_going` rather than `stop` so it cannot be confused with the
        // `step` binding destructured from the vector tuple above.
        let keep_going = matches!(
            ret,
            ReturnCode::OK | ReturnCode::BUF_ERROR | ReturnCode::NEED_DICT
        );

        // L321-L334.
        if keep_going && ret == ReturnCode::NEED_DICT {
            assert!(!bytes.is_empty(), "{what}: a dictionary stream has input");
            // L322-L323: one byte of the compressed stream is not this dictionary.
            assert_eq!(
                inflate_set_dictionary(&mut state, &bytes[..1]),
                ReturnCode::DATA_ERROR,
                "{what}: wrong dictionary"
            );
            // L324-L327: the window this dictionary needs cannot be allocated.
            tracker.set_limit(1);
            assert_eq!(
                inflate_set_dictionary(&mut state, &[]),
                ReturnCode::MEM_ERROR,
                "{what}: dictionary under an exhausted allocator"
            );
            tracker.set_limit(0);
            // L331. C casts through `strm.state` to restore a mode the memory error
            // replaced; `set_mode_tag` is the safe public counterpart of that cast, so
            // this case needs no delegation.
            assert!(
                state.set_mode_tag(Mode::Dict.as_raw()),
                "{what}: DICT is a live tag"
            );
            // L332-L333: this stream's dictionary id is the empty Adler-32, so an
            // empty dictionary is the matching one.
            assert_eq!(
                inflate_set_dictionary(&mut state, &[]),
                ReturnCode::OK,
                "{what}: empty dictionary matches this stream's id"
            );
            // L334.
            ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
            calls = calls.saturating_add(1);
            assert_eq!(
                ret,
                ReturnCode::BUF_ERROR,
                "{what}: no input left after the dictionary"
            );
        }

        output.extend_from_slice(stream.written());
        read_at = read_at.saturating_add(stream.next_in);
        total_in = stream.total_in;
        total_out = stream.total_out;
        if let Some(value) = stream.adler {
            adler = value;
        }
        data_type = stream.data_type;
        msg = stream.msg;
        last = ret;

        if !keep_going {
            break;
        }

        // L335-L336: copy the live state and tear the copy down again, every iteration.
        let mut copy = inflate_copy(&state, &tracker).expect("inflateCopy");
        assert_eq!(
            inflate_end(&mut copy),
            ReturnCode::OK,
            "{what}: copy inflateEnd"
        );
        copies = copies.saturating_add(1);

        // L338-L341 and the `while (strm.avail_in)` condition at L342.
        chunk_end = read_at.saturating_add(step.min(total.saturating_sub(read_at)));
        if chunk_end == read_at {
            break;
        }
    }

    // L343-L344.
    assert!(
        inflate_reset2(&mut state, InflateConfig::new(RAW_SMALL)).is_ok(),
        "{what}: inflateReset2(-8)"
    );
    assert_eq!(
        inflate_end(&mut state),
        ReturnCode::OK,
        "{what}: inflateEnd"
    );

    // L345: `mem_done`.
    tracker.assert_clean();
    assert!(
        header_cells[out_len..]
            .iter()
            .all(|cell| cell.get() == HEADER_GUARD),
        "{what}: the header parser wrote past an advertised maximum"
    );

    Outcome {
        first: first.unwrap_or(last),
        last,
        output,
        msg,
        total_in,
        total_out,
        adler,
        data_type,
        calls,
        copies,
    }
}

// ---------------------------------------------------------------------------
// Producing streams to decode. Every fixture this suite decodes that is not one of
// the reference's hexadecimal vectors is compressed here, by this workspace's own
// compressor, so no opaque byte blobs need to be carried and every payload is
// reproducible from the corpus constants.
// ---------------------------------------------------------------------------

/// Compresses `plain` in one `Z_FINISH` call and returns the finished stream.
///
/// `dictionary` primes the compressor with a preset dictionary before it starts, and
/// `head` installs a gzip header; both are `None` for an ordinary stream. The
/// `adler` the compressor publishes after `deflateSetDictionary` is the dictionary id
/// a decoder will demand, which is why the reset is applied before the call rather
/// than after: it is what puts the Adler-32 seed in place.
fn compress_with(
    plain: &[u8],
    config: DeflateConfig,
    dictionary: Option<&[u8]>,
    head: Option<GzHeaderView<'_>>,
) -> Vec<u8> {
    let mut state = deflate_init2(config, GlobalAllocator).expect("deflate configuration");
    let reset = deflate_reset(&mut state);

    let source_len = u64::try_from(plain.len()).unwrap_or(u64::MAX);
    let mut room = usize::try_from(deflate_bound(Some(&state), source_len)).unwrap_or(usize::MAX);
    if let Some(view) = head {
        // `deflateBound` describes the compressed payload and the fixed part of the
        // wrapper; the optional gzip fields are the caller's own bytes and are added
        // to whatever it reported.
        room = room
            .saturating_add(view.advertised_extra_len())
            .saturating_add(view.name_bound_len())
            .saturating_add(view.comment_bound_len());
    }
    room = room.saturating_add(4096);

    let mut out = vec![0_u8; room];
    let mut stream = DeflateStream::new(plain, &mut out);
    stream.apply_reset(reset);

    if let Some(view) = head {
        assert_eq!(
            deflate_set_header(&mut state, Some(view)),
            ReturnCode::OK,
            "deflateSetHeader"
        );
    }
    if let Some(dict) = dictionary {
        assert_eq!(
            deflate_set_dictionary(&mut state, &mut stream.adler, &mut stream.total_in, dict),
            ReturnCode::OK,
            "deflateSetDictionary"
        );
    }

    assert_eq!(
        deflate(&mut state, &mut stream, Z_FINISH),
        ReturnCode::STREAM_END,
        "deflate(Z_FINISH) should finish in one call"
    );
    let compressed = stream.written().to_vec();
    assert_eq!(deflate_end(&mut state), ReturnCode::OK, "deflateEnd");
    compressed
}

/// Compresses `plain` at the default level into the container `window_bits` names.
/// The largest payload this suite feeds through a full round trip when it runs under
/// Miri.
///
/// Miri interprets MIR rather than executing machine code, at roughly four orders of
/// magnitude the cost. Two tests here -- [`every_container_round_trips`] and
/// [`code_tables_never_exceed_the_enough_capacity`] -- move about 450 KiB of payload
/// between them, including `common::corpus::window_crossing`'s 33 048 bytes at
/// compression level 9, where `max_chain` is 4096. Natively that is cheap; interpreted it
/// is expensive enough that the suite would become something nobody runs.
///
/// Capping the volume is sound because every assertion in this file is *structural*:
/// which code path runs, which status comes back, which message is published, and
/// whether the recovered bytes equal the input. None of them is a claim about size. And
/// 512 bytes still exceeds the 256-byte window that `windowBits` -8 gives, so window
/// sliding, multi-block emission and non-trivial distance codes all still happen; the
/// fixed-size vectors, the fast-path vectors and every hostile-input assertion are left
/// at their exact reference sizes because those *are* size claims. Nothing is skipped
/// and no assertion is relaxed -- only the corpus byte count changes, and only under the
/// interpreter. The capped configuration is exercised natively before every Miri run,
/// so a payload short enough to change an outcome would fail the native suite too.
const MIRI_PAYLOAD_CAP: usize = 512;

/// Caps `plain` at [`MIRI_PAYLOAD_CAP`] under Miri; returns it untouched natively.
///
/// Truncating a payload leaves a payload: any prefix of a byte string is itself a valid
/// input to `deflate`, so the round trip being asserted is the same round trip.
fn capped(plain: Vec<u8>) -> Vec<u8> {
    if cfg!(miri) {
        let mut capped = plain;
        capped.truncate(MIRI_PAYLOAD_CAP);
        capped
    } else {
        plain
    }
}

fn compress(plain: &[u8], window_bits: i32) -> Vec<u8> {
    let mut config = DeflateConfig::new(6);
    config.window_bits = window_bits;
    compress_with(plain, config, None, None)
}

/// Compresses `plain` at `level` with `strategy` into the container `window_bits` names.
fn compress_at(plain: &[u8], window_bits: i32, level: i32, strategy: Strategy) -> Vec<u8> {
    let mut config = DeflateConfig::new(level);
    config.window_bits = window_bits;
    config.strategy = strategy;
    compress_with(plain, config, None, None)
}

// ---------------------------------------------------------------------------
// 1. `Mode` -- the state tag (`inflate.h` L20-L53).
//
// The legal transitions, transcribed from the diagram at `inflate.h` L56-L78:
//
//     HEAD -> (gzip) or (zlib) or (raw)
//     (gzip) -> FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT
//                 -> HCRC -> TYPE
//     (zlib) -> DICTID or TYPE
//     DICTID -> DICT -> TYPE
//     (raw) -> TYPEDO
//     TYPE -> TYPEDO -> STORED or TABLE or LEN_ or CHECK
//     STORED -> COPY_ -> COPY -> TYPE
//     TABLE -> LENLENS -> CODELENS -> LEN_
//     LEN_ -> LEN
//     LEN -> LENEXT or LIT or TYPE
//     LENEXT -> DIST -> DISTEXT -> MATCH -> LEN
//     LIT -> LEN
//     CHECK -> LENGTH -> DONE
//     (most modes can also go to BAD or MEM on error)
// ---------------------------------------------------------------------------

/// Every state, once, in the declaration order of `inflate.h` L21-L52.
///
/// Written out rather than taken from `Mode::ALL`, so that the two have to agree: a
/// state added to the enum but not to `Mode::ALL` fails the length check below, and a
/// state renamed in either place fails to compile.
const EVERY_MODE: [Mode; 32] = [
    Mode::Head,
    Mode::Flags,
    Mode::Time,
    Mode::Os,
    Mode::ExLen,
    Mode::Extra,
    Mode::Name,
    Mode::Comment,
    Mode::HCrc,
    Mode::DictId,
    Mode::Dict,
    Mode::Type,
    Mode::TypeDo,
    Mode::Stored,
    Mode::CopyBlock,
    Mode::Copy,
    Mode::Table,
    Mode::LenLens,
    Mode::CodeLens,
    Mode::LenFirst,
    Mode::Len,
    Mode::LenExt,
    Mode::Dist,
    Mode::DistExt,
    Mode::Match,
    Mode::Lit,
    Mode::Check,
    Mode::Length,
    Mode::Done,
    Mode::Bad,
    Mode::Mem,
    Mode::Sync,
];

/// The lowest live tag: `HEAD = 16180` (`inflate.h` L21).
const HEAD_TAG: i32 = 16_180;

/// The highest live tag: `SYNC` (`inflate.h` L52), reached by implicit increment.
const SYNC_TAG: i32 = 16_211;

#[test]
fn there_are_exactly_thirty_two_states() {
    // The reference enum has 32 entries, counted directly from `inflate.h` L21-L52.
    // Some prose describes it as 31; the source is what this port follows.
    assert_eq!(
        EVERY_MODE.len(),
        32,
        "the enumeration above must be complete"
    );
    assert_eq!(Mode::COUNT, 32, "Mode::COUNT");
    assert_eq!(Mode::ALL, EVERY_MODE, "Mode::ALL in declaration order");

    // Every state is constructible and distinct from every other, so no two variants
    // silently share a tag.
    for (index, mode) in EVERY_MODE.iter().enumerate() {
        for (other_index, other) in EVERY_MODE.iter().enumerate() {
            assert_eq!(
                index == other_index,
                mode == other,
                "{mode:?} and {other:?} must be equal exactly when they are the same state"
            );
        }
    }
}

#[test]
fn discriminants_are_the_dense_block_the_reference_uses() {
    assert_eq!(Mode::Head.as_raw(), HEAD_TAG, "HEAD = 16180");
    assert_eq!(Mode::Sync.as_raw(), SYNC_TAG, "SYNC = 16211");
    assert_eq!(Mode::MIN_DISCRIMINANT, HEAD_TAG);
    assert_eq!(Mode::MAX_DISCRIMINANT, SYNC_TAG);

    // The C enum assigns only `HEAD = 16180` and lets the rest increment implicitly,
    // so the block has to be contiguous and strictly ascending in source order.
    for (index, mode) in EVERY_MODE.iter().enumerate() {
        let offset = i32::try_from(index).expect("32 fits in an i32");
        assert_eq!(
            mode.as_raw(),
            HEAD_TAG + offset,
            "{mode:?} is state number {index}"
        );
    }
    for pair in EVERY_MODE.windows(2) {
        let (earlier, later) = (pair[0], pair[1]);
        assert!(earlier < later, "{earlier:?} must order before {later:?}");
        assert_eq!(
            later.as_raw() - earlier.as_raw(),
            1,
            "{earlier:?} and {later:?} must be adjacent tags"
        );
    }
    assert_eq!(
        SYNC_TAG - HEAD_TAG + 1,
        32,
        "the block must be exactly as wide as the state set"
    );
}

#[test]
fn the_range_predicate_is_the_one_inflate_state_check_uses() {
    // `inflate.c` L95 rejects a stream when `state->mode < HEAD || state->mode > SYNC`.
    // Note the polarity of the port's helper: `inflate_state_check` reports *true* for
    // a tag that does not name a live state, which is C's "this is not one of mine".
    for mode in EVERY_MODE {
        let tag = mode.as_raw();
        assert!(Mode::is_valid_tag(tag), "{mode:?} is a live tag");
        assert!(!inflate_state_check(tag), "{mode:?} must not be rejected");
        assert_eq!(Mode::from_raw(tag), Some(mode), "{mode:?} round trip");
        assert!(
            (HEAD_TAG..=SYNC_TAG).contains(&tag),
            "{mode:?} must sit inside HEAD..=SYNC"
        );
    }

    // The two tags immediately outside the block are the ones an off-by-one would
    // wrongly accept; the extremes stand in for a foreign or uninitialised pointer.
    for tag in [i32::MIN, -1, 0, HEAD_TAG - 1, SYNC_TAG + 1, i32::MAX] {
        assert!(!Mode::is_valid_tag(tag), "{tag} must not be a live tag");
        assert!(inflate_state_check(tag), "{tag} must be rejected");
        assert_eq!(Mode::from_raw(tag), None, "{tag} names no state");
    }
}

#[test]
fn the_ordinal_comparisons_partition_at_bad_and_check() {
    for mode in EVERY_MODE {
        // `inflate.c` L1133: `state->mode < BAD` means "no error has been latched".
        assert_eq!(
            mode.is_error_free(),
            mode < Mode::Bad,
            "{mode:?} against BAD"
        );
        // `inflate.c` L1134: `state->mode < CHECK` means "not into the trailer yet".
        assert_eq!(
            mode.is_before_trailer(),
            mode < Mode::Check,
            "{mode:?} against CHECK"
        );
    }

    assert!(Mode::Len < Mode::Bad, "a decoding state is error-free");
    assert!(
        Mode::Match < Mode::Check,
        "a match copy is before the trailer"
    );
    assert!(
        Mode::Length > Mode::Check,
        "the gzip length is in the trailer"
    );
    assert!(Mode::Bad < Mode::Mem, "BAD orders before MEM");
    assert!(Mode::Mem < Mode::Sync, "MEM orders before SYNC");
    assert!(!Mode::Mem.is_error_free(), "MEM is an error state");
    assert!(!Mode::Sync.is_error_free(), "SYNC orders after BAD");
}

/// Feeds `bytes` one byte at a time and returns the states the decoder passed through,
/// with consecutive duplicates collapsed.
///
/// `InflateState::mode_tag` is the only window a downstream consumer has onto the state
/// machine, and it exists because the C ABI facade needs it to publish the
/// compatibility prefix `test/infcover.c` L331 reaches for. One byte per call is what
/// makes the intermediate header states observable at all: a whole header in one call
/// walks all of them before returning.
fn modes_while_feeding(bytes: &[u8], window_bits: i32) -> Vec<Mode> {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("window bits should be accepted");
    let mut seen = Vec::new();
    let mut read_at = 0_usize;
    let mut total_in = 0_u64;
    let mut total_out = 0_u64;

    // The first call deliberately offers **no** input. That is the only way to observe
    // a state the decoder passes straight through given a single byte: `TYPEDO` needs
    // three bits, so one byte already carries it into `LEN`.
    for call in 0..bytes.len().saturating_add(3) {
        let end = if call == 0 {
            read_at
        } else {
            read_at.saturating_add(1).min(bytes.len())
        };
        let mut buffer = [0_u8; 64];
        let mut stream = InflateStream::new(&bytes[read_at..end], &mut buffer);
        stream.total_in = total_in;
        stream.total_out = total_out;

        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        read_at = read_at.saturating_add(stream.next_in);
        total_in = stream.total_in;
        total_out = stream.total_out;

        let mode = Mode::from_raw(state.mode_tag()).expect("a live state tag");
        if seen.last() != Some(&mode) {
            seen.push(mode);
        }
        if matches!(
            ret,
            ReturnCode::STREAM_END
                | ReturnCode::DATA_ERROR
                | ReturnCode::MEM_ERROR
                | ReturnCode::STREAM_ERROR
                | ReturnCode::NEED_DICT
        ) {
            break;
        }
    }

    assert_eq!(inflate_end(&mut state), ReturnCode::OK, "inflateEnd");
    seen
}

#[test]
fn each_container_enters_the_state_machine_by_its_documented_branch() {
    // `(raw) -> TYPEDO`: there is no header to parse, so the state machine leaves
    // `HEAD` without consuming anything and the very first call is already in the block
    // dispatcher.
    let raw = modes_while_feeding(&compress(HELLO, RAW), RAW);
    assert_eq!(
        raw.first(),
        Some(&Mode::TypeDo),
        "a raw stream must go straight to TYPEDO, saw {raw:?}"
    );
    assert!(
        !raw.contains(&Mode::Head),
        "a raw stream must never wait in HEAD, saw {raw:?}"
    );

    // `(zlib) -> DICTID or TYPE`, taking the TYPE branch because this stream carries
    // no preset dictionary. `HEAD` is visible first because the header needs 16 bits.
    let zlib = modes_while_feeding(&compress(HELLO, ZLIB), ZLIB);
    assert_eq!(
        zlib.first(),
        Some(&Mode::Head),
        "a zlib stream must wait in HEAD for its two header bytes, saw {zlib:?}"
    );
    let after_head = zlib
        .iter()
        .copied()
        .find(|mode| *mode != Mode::Head)
        .expect("the header is parsed");
    assert_eq!(
        after_head,
        Mode::Type,
        "a dictionary-free zlib stream must leave HEAD for TYPE, saw {zlib:?}"
    );
    assert!(
        !zlib.contains(&Mode::DictId),
        "and must not take the DICTID branch, saw {zlib:?}"
    );

    // `(zlib) -> DICTID -> DICT`, the other branch of the same choice.
    let dict_stream = compress_with(HELLO, DeflateConfig::new(9), Some(DICTIONARY), None);
    let dict = modes_while_feeding(&dict_stream, ZLIB);
    assert!(
        dict.contains(&Mode::DictId),
        "a preset-dictionary stream must pass through DICTID, saw {dict:?}"
    );
    assert_eq!(
        dict.last(),
        Some(&Mode::Dict),
        "and must come to rest in DICT, saw {dict:?}"
    );

    // `(gzip) -> FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT -> HCRC
    // -> TYPE`, in that order. A minimal header carries none of the optional fields, so
    // the four states that guard them are passed straight through and never observed;
    // what has to hold is that whatever *is* observed appears in the documented order.
    let gzip = modes_while_feeding(&compress(HELLO, GZIP), GZIP);
    let documented = [
        Mode::Head,
        Mode::Flags,
        Mode::Time,
        Mode::Os,
        Mode::ExLen,
        Mode::Extra,
        Mode::Name,
        Mode::Comment,
        Mode::HCrc,
        Mode::Type,
    ];
    let reaches_type = gzip
        .iter()
        .position(|mode| *mode == Mode::Type)
        .expect("the gzip header must complete");
    let mut walk = documented.iter();
    for mode in &gzip[..=reaches_type] {
        assert!(
            walk.any(|expected| expected == mode),
            "{mode:?} is out of order on the gzip header path, saw {gzip:?}"
        );
    }
    for required in [Mode::Flags, Mode::Time, Mode::Os, Mode::Type] {
        assert!(
            gzip.contains(&required),
            "the gzip path must visit {required:?}, saw {gzip:?}"
        );
    }

    // `CHECK -> LENGTH -> DONE`, the other end of the graph. A gzip member verifies a
    // CRC-32 *and* the uncompressed length, so it visits LENGTH; a zlib stream has only
    // its Adler-32 and goes straight from CHECK to DONE.
    let trailer_of = |walked: &[Mode]| -> Vec<Mode> {
        walked
            .iter()
            .copied()
            .filter(|mode| matches!(mode, Mode::Check | Mode::Length | Mode::Done))
            .collect()
    };
    assert_eq!(
        trailer_of(&gzip),
        vec![Mode::Check, Mode::Length, Mode::Done],
        "the gzip trailer path, saw {gzip:?}"
    );
    assert_eq!(
        trailer_of(&zlib),
        vec![Mode::Check, Mode::Done],
        "the zlib trailer path, saw {zlib:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. The fixed decode tables (`inffixed.h`, RFC 1951 section 3.2.6).
// ---------------------------------------------------------------------------

#[test]
fn the_fixed_tables_have_the_shapes_inffixed_h_declares() {
    assert_eq!(lenfix.len(), 512, "lenfix[512]");
    assert_eq!(distfix.len(), 32, "distfix[32]");
    // `struct inflate_state` budgets `4 * ENOUGH` bytes for the code arena
    // (`inftrees.h` L24-L28), and `test/infcover.c` accounts for that budget when it
    // limits allocation, so the entry width is part of the memory contract.
    assert_eq!(size_of::<Code>(), 4, "sizeof(code)");
    assert_eq!(MAXBITS, 15, "MAXBITS");
}

#[test]
fn the_fixed_table_endpoints_are_the_generated_values() {
    // The first and last rows `makefixed()` printed into `inffixed.h`.
    assert_eq!(lenfix[0], Code::new(96, 7, 0), "lenfix[0]");
    assert_eq!(lenfix[511], Code::new(0, 9, 255), "lenfix[511]");
    assert_eq!(distfix[0], Code::new(16, 5, 1), "distfix[0]");
    assert_eq!(distfix[31], Code::new(64, 5, 0), "distfix[31]");

    // Field order is the C struct's: `{op, bits, val}` (`inftrees.h` L24-L28), which
    // is also the order the committed tables read in.
    assert_eq!(lenfix[0].op, 96);
    assert_eq!(lenfix[0].bits, 7);
    assert_eq!(lenfix[0].val, 0);
    assert!(
        lenfix[0].is_end_of_block(),
        "op 96 is 32 | 64: end of block"
    );
    assert!(distfix[31].is_invalid(), "op 64 is the invalid-code marker");
    assert!(lenfix[511].is_literal(), "op 0 is a literal");
    assert!(
        distfix[0].is_length_or_distance(),
        "op 16 carries a distance base"
    );
}

#[test]
fn every_fixed_table_entry_is_structurally_valid() {
    // RFC 1951 section 3.2.6 fixes the literal/length code lengths at 7, 8 or 9 bits
    // and every distance code at 5 bits. Both tables are complete root tables -- their
    // lengths are `1 << root` -- so no entry may link to a second level.
    for (index, entry) in lenfix.iter().enumerate() {
        assert!(
            (7..=9).contains(&entry.bits),
            "lenfix[{index}] has {} bits",
            entry.bits
        );
        assert!(
            !entry.is_table_link(),
            "lenfix[{index}] must not link to a sub-table"
        );
        assert!(
            entry.is_literal()
                || entry.is_length_or_distance()
                || entry.is_end_of_block()
                || entry.is_invalid(),
            "lenfix[{index}] has an undocumented op {}",
            entry.op
        );
        if entry.is_length_or_distance() {
            assert!(
                entry.extra_bits() <= 5,
                "lenfix[{index}] claims {} extra bits",
                entry.extra_bits()
            );
            assert!(
                (3..=258).contains(&entry.val),
                "lenfix[{index}] has length base {}",
                entry.val
            );
        }
        if entry.is_literal() {
            assert!(
                entry.val <= 255,
                "lenfix[{index}] has literal {}",
                entry.val
            );
        }
    }

    for (index, entry) in distfix.iter().enumerate() {
        assert_eq!(entry.bits, 5, "distfix[{index}] must be a 5-bit code");
        assert!(
            !entry.is_table_link(),
            "distfix[{index}] must not link to a sub-table"
        );
        assert!(
            entry.is_length_or_distance() || entry.is_invalid(),
            "distfix[{index}] has an undocumented op {}",
            entry.op
        );
        if entry.is_length_or_distance() {
            assert!(
                entry.extra_bits() <= 13,
                "distfix[{index}] claims {} extra bits",
                entry.extra_bits()
            );
            assert!(
                (1..=24_577).contains(&entry.val),
                "distfix[{index}] has distance base {}",
                entry.val
            );
        }
    }

    // The two reserved distance symbols 30 and 31 -- undefined by RFC 1951 section
    // 3.2.5 -- are the only invalid entries, and there are exactly two of them.
    assert_eq!(
        distfix.iter().filter(|entry| entry.is_invalid()).count(),
        2,
        "distfix must mark exactly the two reserved symbols invalid"
    );
}

#[test]
fn the_enough_capacities_are_the_reference_values() {
    // `inftrees.h` L49-L51.
    assert_eq!(ENOUGH_LENS, 852, "ENOUGH_LENS");
    assert_eq!(ENOUGH_DISTS, 592, "ENOUGH_DISTS");
    assert_eq!(ENOUGH, 1444, "ENOUGH");
    assert_eq!(ENOUGH, ENOUGH_LENS + ENOUGH_DISTS, "ENOUGH is the sum");
}

// ---------------------------------------------------------------------------
// 3. The hexadecimal vector suite -- the core of this file.
// ---------------------------------------------------------------------------

#[test]
fn every_raw_error_vector_reports_the_reference_message() {
    // `try()` itself asserts the status and the message; the additions here are the
    // properties C cannot check because it discards the run: that the error is
    // reported again on a further call, and that the accounting is consistent.
    for (hex, message) in RAW_ERROR_VECTORS {
        let outcome = try_raw(hex, message, 1);
        outcome.assert_documented_status(message);
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{message}: status");
        assert_eq!(outcome.msg, Some(message), "{message}: z_stream.msg");
        assert_eq!(
            outcome.total_out,
            u64::try_from(outcome.output.len()).expect("output length fits"),
            "{message}: total_out must count every byte written"
        );
        assert!(outcome.calls >= 1, "{message}: the run must have happened");

        // A latched data error is not recoverable: the next call reports it again
        // rather than resuming (`inflate.c` L1115).
        let bytes = h2b(hex);
        let mut state = inflate_init2(InflateConfig::new(RAW), GlobalAllocator)
            .expect("raw window bits are accepted");
        let mut room = vec![0_u8; output_room(bytes.len())];
        let mut stream = InflateStream::new(&bytes, &mut room);
        assert_eq!(
            inflate(&mut state, &mut stream, Z_NO_FLUSH),
            ReturnCode::DATA_ERROR,
            "{message}: first call"
        );
        let consumed = stream.next_in;
        let mut again = InflateStream::new(&bytes[consumed..], &mut room);
        assert_eq!(
            inflate(&mut state, &mut again, Z_NO_FLUSH),
            ReturnCode::DATA_ERROR,
            "{message}: a data error must be reported again"
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }
}

#[test]
fn every_raw_success_vector_decodes_to_the_reference_bytes() {
    for (hex, what, expected_len) in RAW_SUCCESS_VECTORS {
        // The faithful `try()` run first: `Z_TREES`, an output buffer of `len << 3`,
        // and C's loop condition. It asserts no error and no message. It deliberately
        // does *not* assert `Z_STREAM_END`, because `Z_TREES` stops at every block
        // boundary and C's loop ends as soon as the input is spent -- so `Z_OK` with a
        // block boundary pending is the reference's own outcome for several of these.
        let trees = try_raw(hex, what, 0);
        assert!(trees.calls >= 1, "{what}: the run must have happened");

        // Then an ordinary `Z_NO_FLUSH` decode, which is where the payload itself can
        // be compared against the reference decode of the same vector.
        let bytes = h2b(hex);
        let outcome = decode(&bytes, RAW);
        assert_eq!(outcome.last, ReturnCode::STREAM_END, "{what}: status");
        assert_eq!(outcome.output.len(), expected_len, "{what}: output length");
        assert_eq!(
            outcome.total_out,
            u64::try_from(expected_len).expect("length fits"),
            "{what}: total_out"
        );
        assert_eq!(
            outcome.total_in,
            u64::try_from(bytes.len()).expect("length fits"),
            "{what}: total_in must account for the whole vector"
        );
        // Every one of these vectors expands to a run of zero bytes; the expectations
        // came from a reference decode of each.
        assert!(
            outcome.output.iter().all(|byte| *byte == 0),
            "{what}: the payload is a run of zero bytes"
        );
        // A raw stream carries no check value, so `adler` stays at its seed of zero.
        assert_eq!(outcome.adler, 0, "{what}: a raw stream has no check value");

        // And the recovered payload round trips: recompressing it and decoding again
        // reproduces it exactly.
        let again = decode(&compress(&outcome.output, RAW), RAW);
        assert_eq!(
            again.last,
            ReturnCode::STREAM_END,
            "{what}: round trip status"
        );
        assert_eq!(again.output, outcome.output, "{what}: round trip bytes");
    }
}

#[test]
fn every_gzip_trailer_vector_reports_the_reference_message() {
    // `try()` passes `err < 0 ? 47 : -15`, so these two decode through automatic
    // container detection. Both are well-formed gzip members whose trailer disagrees
    // with the data, which is the one class of failure that only the wrapped path can
    // produce.
    for (hex, message) in GZIP_TRAILER_VECTORS {
        let outcome = try_raw(hex, message, -1);
        outcome.assert_documented_status(message);
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{message}: status");
        assert_eq!(outcome.msg, Some(message), "{message}: z_stream.msg");
    }
}

/// Locates a row of a vector table by the name `test/infcover.c` gave it.
fn vector_named(table: &[InfVector], what: &str) -> InfVector {
    *table
        .iter()
        .find(|row| row.1 == what)
        .expect("the table must carry the named vector")
}

#[test]
fn every_header_and_trailer_vector_behaves_as_the_reference_does() {
    for vector in INF_VECTORS {
        let what = vector.1;
        let expected = vector.5;
        let outcome = inf(vector);
        assert_eq!(outcome.first, expected, "{what}: first inflate status");

        if outcome.calls == 0 {
            // The only row that reaches this branch is the one whose `window_bits` is
            // refused outright, so no state is ever built and nothing else can be said.
            assert_eq!(outcome.last, ReturnCode::STREAM_ERROR, "{what}");
            assert_eq!(outcome.copies, 0, "{what}: nothing to copy");
            continue;
        }

        outcome.assert_documented_status(what);
        // `inf()` copies the live state on every non-terminal iteration and immediately
        // ends the copy, so a run that got past its first call must have made one.
        let resumable = matches!(
            expected,
            ReturnCode::OK | ReturnCode::BUF_ERROR | ReturnCode::NEED_DICT
        );
        assert_eq!(
            outcome.copies >= 1,
            resumable,
            "{what}: mid-stream inflateCopy"
        );
        assert_eq!(
            outcome.total_out,
            u64::try_from(outcome.output.len()).expect("output length fits"),
            "{what}: total_out must count every byte written"
        );
    }
}

#[test]
fn every_header_failure_reports_the_reference_message() {
    for (what, message) in INF_MESSAGES {
        let outcome = inf(vector_named(&INF_VECTORS, what));
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{what}: status");
        assert_eq!(outcome.msg, Some(message), "{what}: z_stream.msg");
    }
}

#[test]
fn every_fast_path_vector_behaves_as_the_reference_does() {
    for vector in FAST_VECTORS {
        let what = vector.1;
        let outcome = inf(vector);
        assert_eq!(outcome.first, vector.5, "{what}: first inflate status");
        outcome.assert_documented_status(what);
        // The output length is what forces the unrolled loop, so a vector that
        // produced nothing at all would mean the fixture stopped covering it.
        assert!(outcome.calls >= 1, "{what}: the run must have happened");
    }

    for (what, message) in FAST_MESSAGES {
        let outcome = inf(vector_named(&FAST_VECTORS, what));
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{what}: status");
        assert_eq!(outcome.msg, Some(message), "{what}: z_stream.msg");
    }
}

#[test]
fn a_reset_stream_immediately_decodes_a_fresh_raw_stream() {
    // `inf()` ends every run with `inflateReset2(&strm, -8)` (`test/infcover.c` L343),
    // which is only meaningful if the stream is genuinely usable afterwards. Every
    // vector above proves the call succeeds; this proves the state it leaves behind
    // works, for a run that got as far as allocating and filling a window.
    // `windowBits` of -8 is a decoder-only spelling: `inflateInit2` accepts it, while
    // `deflateInit2` refuses a 256-byte window for anything but a zlib wrapper
    // (`deflate.c` rejects `windowBits == 8 && wrap != 1`). So the small-window phase
    // uses one of the reference's own vectors and the fresh stream is a -15 one, which
    // also makes `inflateReset2` change the window size and therefore replace the
    // allocation.
    let plain = capped(common::corpus::repetitive(600));
    let raw = compress(&plain, RAW);

    let mut state = inflate_init2(InflateConfig::new(RAW_SMALL), GlobalAllocator)
        .expect("-8 is a valid decoder window");
    let first = drive(
        &mut state,
        &h2b("63 18 5"),
        0,
        259,
        Z_NO_FLUSH,
        adler_seed(RAW_SMALL),
    );
    assert_eq!(
        first.last,
        ReturnCode::BUF_ERROR,
        "the vector runs out of input"
    );
    assert!(!first.output.is_empty(), "and produces window history");

    assert!(
        inflate_reset2(&mut state, InflateConfig::new(RAW)).is_ok(),
        "inflateReset2(-15)"
    );
    let second = drive(
        &mut state,
        &raw,
        0,
        output_room(raw.len()),
        Z_NO_FLUSH,
        adler_seed(RAW),
    );
    assert_eq!(
        second.last,
        ReturnCode::STREAM_END,
        "the fresh stream decodes"
    );
    assert_eq!(second.output, plain, "and decodes to the right bytes");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn every_raw_vector_survives_a_mid_stream_copy_and_reset() {
    // `inf()` copies the live state and immediately ends the copy on every iteration
    // (`test/infcover.c` L335-L336), and finishes every run with `inflateReset2(-8)`
    // (L343). `try()` does neither, so the vectors it owns get the same treatment here:
    // every one of them must survive being forked mid-stream and being reset afterwards,
    // and the allocator's books must balance in every case.
    let vectors = RAW_ERROR_VECTORS
        .iter()
        .map(|row| row.0)
        .chain(RAW_SUCCESS_VECTORS.iter().map(|row| row.0));

    for hex in vectors {
        let bytes = h2b(hex);
        let tracker = TrackingAllocator::new();
        {
            let mut state =
                inflate_init2(InflateConfig::new(RAW), &tracker).expect("raw window bits");
            let room_len = output_room(bytes.len());
            let (consumed, adler, first_msg) = {
                let mut room = vec![0_u8; room_len];
                let mut stream = InflateStream::new(&bytes, &mut room);
                let ret = inflate(&mut state, &mut stream, Z_TREES);
                assert!(
                    !matches!(ret, ReturnCode::STREAM_ERROR | ReturnCode::MEM_ERROR),
                    "{hex}: unexpected {ret:?}"
                );
                (stream.next_in, stream.adler.unwrap_or(0), stream.msg)
            };

            let mut copy = inflate_copy(&state, &tracker).expect("inflateCopy");
            assert_eq!(
                inflate_end(&mut copy),
                ReturnCode::OK,
                "{hex}: the copy must end cleanly on its own"
            );

            // The source is untouched by the copy's life and death. The status check is
            // spelled out rather than delegated to `Outcome::assert_documented_status`
            // because the message may have been published by the call above, on a stream
            // this continuation does not share -- which is precisely why C keeps `msg` in
            // the caller's `z_stream` rather than recomputing it.
            let after = drive(&mut state, &bytes[consumed..], 0, room_len, Z_TREES, adler);
            assert!(
                matches!(
                    after.last,
                    ReturnCode::OK
                        | ReturnCode::STREAM_END
                        | ReturnCode::NEED_DICT
                        | ReturnCode::BUF_ERROR
                        | ReturnCode::DATA_ERROR
                ),
                "{hex}: undocumented status {:?}",
                after.last
            );
            if after.last == ReturnCode::DATA_ERROR {
                let msg = after
                    .msg
                    .or(first_msg)
                    .expect("a data error must carry a message");
                assert!(
                    EVERY_MESSAGE.contains(&msg),
                    "{hex}: {msg:?} is not one of the reference's messages"
                );
            }

            assert!(
                inflate_reset2(&mut state, InflateConfig::new(RAW_SMALL)).is_ok(),
                "{hex}: inflateReset2(-8)"
            );
            assert_eq!(inflate_end(&mut state), ReturnCode::OK, "{hex}: inflateEnd");
        }
        tracker.assert_clean();
    }
}

// ---------------------------------------------------------------------------
// 4. Container detection.
// ---------------------------------------------------------------------------

#[test]
fn every_container_round_trips() {
    for (name, plain) in common::corpus::all() {
        let plain = capped(plain);
        for window_bits in [RAW, ZLIB, GZIP] {
            let compressed = compress(&plain, window_bits);
            let outcome = decode(&compressed, window_bits);
            assert_eq!(
                outcome.last,
                ReturnCode::STREAM_END,
                "{name} at windowBits {window_bits}"
            );
            assert_eq!(
                outcome.output, plain,
                "{name} at windowBits {window_bits}: recovered bytes"
            );
            assert_eq!(
                outcome.total_in,
                u64::try_from(compressed.len()).expect("length fits"),
                "{name} at windowBits {window_bits}: total_in"
            );
        }
    }
}

#[test]
fn autodetect_accepts_both_zlib_and_gzip() {
    let plain = capped(common::corpus::text());
    for window_bits in [ZLIB, GZIP] {
        let compressed = compress(&plain, window_bits);
        let outcome = decode(&compressed, AUTODETECT);
        assert_eq!(
            outcome.last,
            ReturnCode::STREAM_END,
            "auto-detect on a windowBits {window_bits} stream"
        );
        assert_eq!(outcome.output, plain, "auto-detect recovered bytes");
    }
}

#[test]
fn window_bits_zero_takes_the_window_size_from_the_header() {
    // The `8 99` vector advertises a 256-byte window through its `CINFO` nibble, and
    // `windowBits` of 0 says "believe the header".
    let outcome = inf(vector_named(&INF_VECTORS, "set window size from header"));
    assert_eq!(outcome.first, ReturnCode::OK, "the header is accepted");

    // A real stream compressed with the smallest window decodes the same way.
    let plain = capped(common::corpus::repetitive(4096));
    let compressed = compress(&plain, 8);
    let outcome = decode(&compressed, FROM_HEADER);
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "status");
    assert_eq!(outcome.output, plain, "recovered bytes");

    // And so does one compressed with the largest, because the header names that too.
    let compressed = compress(&plain, ZLIB);
    let outcome = decode(&compressed, FROM_HEADER);
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "status");
    assert_eq!(outcome.output, plain, "recovered bytes");
}

#[test]
fn container_discrimination_rejects_the_wrong_wrapper() {
    let plain = capped(common::corpus::text());

    // A raw decoder handed a zlib stream reads the two header bytes as block data.
    // `78 9c` has `BFINAL = 0`, `BTYPE = 00` -- a stored block -- whose length field
    // then fails its own complement check, so this is a data error and not a silent
    // mis-decode.
    let zlib_stream = compress(&plain, ZLIB);
    let outcome = decode(&zlib_stream, RAW);
    assert_eq!(
        outcome.last,
        ReturnCode::DATA_ERROR,
        "a raw decoder must refuse a zlib stream"
    );
    outcome.assert_documented_status("zlib stream into a raw decoder");
    assert_ne!(outcome.output, plain, "and must not recover the payload");

    // A zlib decoder handed a raw stream refuses it too. Which message it reports
    // depends on the first two payload bytes -- the modulo-31 check, then the method
    // nibble, then the window nibble -- so only the class is pinned here.
    let raw_stream = compress(&plain, RAW);
    let outcome = decode(&raw_stream, ZLIB);
    assert_eq!(
        outcome.last,
        ReturnCode::DATA_ERROR,
        "a zlib decoder must refuse a raw stream"
    );
    outcome.assert_documented_status("raw stream into a zlib decoder");
    assert_ne!(outcome.output, plain, "and must not recover the payload");

    // Automatic detection is opt-in, not implicit: a gzip-only decoder refuses a zlib
    // stream and a zlib-only decoder refuses a gzip member. Both messages *are* pinned,
    // because both are decided by header bytes that never vary -- a gzip member always
    // begins `1f 8b`, whose 0x1f8b fails the modulo-31 test, and for a gzip-only
    // decoder `!(state->wrap & 1)` short-circuits the same test for any non-gzip magic.
    let outcome = decode(&zlib_stream, GZIP);
    assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "zlib into gzip-only");
    assert_eq!(outcome.msg, Some(MSG_HEADER_CHECK), "z_stream.msg");

    let gzip_stream = compress(&plain, GZIP);
    assert_eq!(
        gzip_stream.get(..2),
        Some(&[0x1f_u8, 0x8b][..]),
        "a gzip member begins with its magic"
    );
    let outcome = decode(&gzip_stream, ZLIB);
    assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "gzip into zlib-only");
    assert_eq!(outcome.msg, Some(MSG_HEADER_CHECK), "z_stream.msg");

    // And a raw decoder handed a gzip member reads the magic as a reserved block type.
    let outcome = decode(&gzip_stream, RAW);
    assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "gzip into raw");
    assert_eq!(outcome.msg, Some(MSG_BLOCK_TYPE), "z_stream.msg");
}

// ---------------------------------------------------------------------------
// 5. Malformed, truncated and hostile input.
//
// This is the section the whole file exists for. Every assertion below is the same
// one: whatever the bytes are, the call returns, and it returns one of the statuses
// `zlib.h` documents. The failure modes being excluded are a panic, a read past the
// end of a buffer, and a call that never comes back -- each a vulnerability rather
// than a wrong answer.
// ---------------------------------------------------------------------------

/// Decodes `bytes` and asserts only that the outcome is one a caller must handle.
///
/// A deliberately small output buffer is used so that the resumable paths -- the ones
/// that suspend for want of room and have to restore their own state -- are the ones
/// being exercised.
fn assert_survives(bytes: &[u8], window_bits: i32, what: &str) {
    let outcome = decode_chunked(bytes, window_bits, 0, 64);
    outcome.assert_documented_status(what);
}

#[test]
fn every_truncation_of_one_stream_is_handled() {
    // Kept unconditional, and kept to one short stream, so that it is cheap enough to
    // run under Miri -- which is where this class of assertion is worth the most.
    let compressed = compress(HELLO, ZLIB);
    for end in 0..=compressed.len() {
        assert_survives(
            &compressed[..end],
            ZLIB,
            &format!("zlib truncated to {end}"),
        );
    }
}

#[test]
// Ignored under Miri for interpreter speed only. It is not conditional on anything
// else and it still runs, in full, on every native `cargo test`.
#[cfg_attr(miri, ignore)]
fn every_truncation_of_every_container_is_handled() {
    for (name, plain) in [
        ("hello", HELLO.to_vec()),
        ("text", capped(common::corpus::text())),
        (
            "incompressible",
            capped(common::corpus::incompressible(300)),
        ),
        ("repetitive", capped(common::corpus::repetitive(400))),
    ] {
        for window_bits in [RAW, ZLIB, GZIP] {
            let compressed = compress(&plain, window_bits);
            for end in 0..=compressed.len() {
                assert_survives(
                    &compressed[..end],
                    window_bits,
                    &format!("{name} at windowBits {window_bits} truncated to {end}"),
                );
            }
        }
    }
}

#[test]
// Ignored under Miri for interpreter speed only; it still runs natively in full.
#[cfg_attr(miri, ignore)]
fn single_byte_mutations_never_panic() {
    for window_bits in [RAW, ZLIB, GZIP, AUTODETECT] {
        let compressed = compress(
            HELLO,
            if window_bits == AUTODETECT {
                GZIP
            } else {
                window_bits
            },
        );
        for index in 0..compressed.len() {
            for replacement in [0x00_u8, 0x01, 0x0f, 0x55, 0x7f, 0x80, 0xaa, 0xfe, 0xff] {
                if compressed[index] == replacement {
                    continue;
                }
                let mut mutated = compressed.clone();
                mutated[index] = replacement;
                assert_survives(
                    &mutated,
                    window_bits,
                    &format!("windowBits {window_bits} byte {index} set to {replacement:#04x}"),
                );
            }
        }
    }
}

#[test]
fn pseudo_random_noise_never_panics() {
    // `common::lcg_fill` rather than `rand`: the crate forbids third-party
    // dependencies, and a corpus that changed between runs could not be reproduced
    // from a failure message anyway.
    for len in [0_usize, 1, 2, 7, 64] {
        for seed in 0..3_u64 {
            let mut noise = vec![0_u8; len];
            lcg_fill(seed, &mut noise);
            for window_bits in [RAW, RAW_SMALL, ZLIB, GZIP, AUTODETECT] {
                assert_survives(
                    &noise,
                    window_bits,
                    &format!("noise len {len} seed {seed} at windowBits {window_bits}"),
                );
            }
        }
    }
}

#[test]
fn a_distance_beyond_the_available_history_is_rejected() {
    // The reference's own vector for the slow path, and its own vector for the fast
    // path, both of which pin the message.
    let slow = try_raw("c c0 81 0 0 0 0 0 90 ff 6b 4 0", MSG_DISTANCE_TOO_FAR, 1);
    assert_eq!(slow.msg, Some(MSG_DISTANCE_TOO_FAR), "slow path");
    let fast = inf(vector_named(
        &FAST_VECTORS,
        "fast 2nd level codes and too far back",
    ));
    assert_eq!(fast.msg, Some(MSG_DISTANCE_TOO_FAR), "fast path");

    // The same rejection reached from the other direction, and the reason this whole
    // class of assertion exists. `whave` -- how much of the window actually holds
    // decoded history -- is security-load-bearing: the reference implementation this port
    // is taken from carries a fix for a defect in which an inflated `whave` let the decoder
    // copy *uninitialised* window bytes into the output and accept an invalid stream.
    // A raw stream compressed against a preset dictionary is the cleanest way to
    // produce a legitimate distance that reaches behind the start of the data: decoded
    // without that dictionary, `whave` is zero, and the copy must be refused rather
    // than served from whatever the window buffer happens to contain.
    let dictionary: Vec<u8> = (0..300_u32)
        .map(|index| u8::try_from((index * 7 + 3) % 251).expect("modulo 251 fits a byte"))
        .collect();
    let mut payload = dictionary.clone();
    payload.extend_from_slice(b"tail-marker-bytes");

    let mut config = DeflateConfig::new(9);
    config.window_bits = RAW;
    let raw = compress_with(&payload, config, Some(&dictionary), None);

    let without = decode(&raw, RAW);
    assert_eq!(
        without.last,
        ReturnCode::DATA_ERROR,
        "a distance reaching behind the data must be refused"
    );
    assert_eq!(
        without.msg,
        Some(MSG_DISTANCE_TOO_FAR),
        "and must say exactly why"
    );
    assert!(
        without.output.len() < payload.len(),
        "and must not have served the whole payload from an empty window"
    );

    // With the dictionary supplied, the very same stream decodes: the refusal above is
    // about missing history, not about the stream being malformed.
    let mut state =
        inflate_init2(InflateConfig::new(RAW), GlobalAllocator).expect("raw window bits");
    assert_eq!(
        inflate_set_dictionary(&mut state, &dictionary),
        ReturnCode::OK,
        "a raw stream accepts a dictionary at any time"
    );
    let with = drive(
        &mut state,
        &raw,
        0,
        output_room(raw.len()),
        Z_NO_FLUSH,
        adler_seed(RAW),
    );
    assert_eq!(
        with.last,
        ReturnCode::STREAM_END,
        "status with the dictionary"
    );
    assert_eq!(with.output, payload, "recovered bytes with the dictionary");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn no_progress_is_reported_rather_than_hung() {
    let compressed = compress(HELLO, ZLIB);
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");

    // `avail_in == 0` with room to write: nothing can be done, and `Z_BUF_ERROR` says
    // so rather than the call spinning (`zlib.h` L440-L447).
    let mut room = vec![0_u8; 64];
    let mut stream = InflateStream::new(&[], &mut room);
    assert_eq!(
        inflate(&mut state, &mut stream, Z_NO_FLUSH),
        ReturnCode::BUF_ERROR,
        "no input and no progress"
    );
    assert_eq!(stream.next_out, 0, "and nothing was written");

    // A zero-length output buffer: the header can still be absorbed, so the first calls
    // may make progress, but the decoder has to arrive at `Z_BUF_ERROR` in a bounded
    // number of calls instead of looping.
    let mut consumed = 0_usize;
    let mut calls = 0_u32;
    loop {
        assert!(
            calls < 64,
            "a zero-length output buffer must stall, not spin"
        );
        let mut empty: [u8; 0] = [];
        let mut stream = InflateStream::new(&compressed[consumed..], &mut empty);
        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        calls = calls.saturating_add(1);
        consumed = consumed.saturating_add(stream.next_in);
        if ret == ReturnCode::BUF_ERROR {
            break;
        }
        assert_eq!(
            ret,
            ReturnCode::OK,
            "with no room, only Z_OK can precede it"
        );
    }

    // And the stream is still usable the moment a real buffer arrives.
    let mut room = vec![0_u8; 64];
    let mut stream = InflateStream::new(&compressed[consumed..], &mut room);
    assert_eq!(
        inflate(&mut state, &mut stream, Z_FINISH),
        ReturnCode::STREAM_END,
        "the stalled stream resumes"
    );
    assert_eq!(stream.written(), HELLO, "and produces the payload");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

// ---------------------------------------------------------------------------
// 6. Gzip header handling, and the clamping guarantee.
// ---------------------------------------------------------------------------

/// `FTEXT`, RFC 1952 section 2.3.1.
const FTEXT: u8 = 0x01;
/// `FHCRC`.
const FHCRC: u8 = 0x02;
/// `FEXTRA`.
const FEXTRA: u8 = 0x04;
/// `FNAME`.
const FNAME: u8 = 0x08;
/// `FCOMMENT`.
const FCOMMENT: u8 = 0x10;
/// The three bits RFC 1952 reserves and requires a decoder to reject.
const FRESERVED: u8 = 0xe0;

/// Assembles a gzip member by hand, so that the `FLG` byte can be set to anything.
///
/// The layout is RFC 1952 section 2.2: the two magic bytes, the compression method, the
/// flag byte, a four-byte modification time, the extra-flags and operating-system
/// bytes, then whichever optional fields `flg` advertises, then the raw DEFLATE
/// payload, then the CRC-32 and the uncompressed length, both little-endian.
/// `FHCRC` appends the low sixteen bits of the CRC-32 of everything before it, which is
/// what makes a deliberately wrong value testable.
fn build_gzip_member(
    flg: u8,
    extra: &[u8],
    name: &[u8],
    comment: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    // RFC 1952's ISIZE modulus, declared here at the top of the scope rather than beside its
    // use, because clippy::items_after_statements is denied for this workspace.
    const ISIZE_MODULUS: u64 = 1 << 32;

    let mut member = vec![0x1f, 0x8b, 0x08, flg, 0, 0, 0, 0, 0, 0xff];
    if flg & FEXTRA != 0 {
        let len = u16::try_from(extra.len()).expect("the fixture extra field is short");
        member.extend_from_slice(&len.to_le_bytes());
        member.extend_from_slice(extra);
    }
    if flg & FNAME != 0 {
        member.extend_from_slice(name);
        member.push(0);
    }
    if flg & FCOMMENT != 0 {
        member.extend_from_slice(comment);
        member.push(0);
    }
    if flg & FHCRC != 0 {
        let crc = crc32(0, &member);
        member.extend_from_slice(&u16::try_from(crc & 0xffff).expect("masked").to_le_bytes());
    }
    member.extend_from_slice(&compress(payload, RAW));
    member.extend_from_slice(&crc32(0, payload).to_le_bytes());
    // RFC 1952's ISIZE is the uncompressed size modulo 2^32.  The modulus is taken over
    // `u64` rather than `usize` because `1_usize << 32` is an overflowing shift wherever
    // `usize` is 32 bits wide -- and it is an `arithmetic_overflow` error rather than a
    // warning, so this line refused to compile for a 32-bit target at all.  On such a
    // target the reduction is the identity, which is exactly what the widened form
    // computes, so the value is unchanged on every platform.
    let payload_len = u64::try_from(payload.len()).expect("a fixture length fits in u64");
    let isize_field = u32::try_from(payload_len % ISIZE_MODULUS).expect("modulo 2^32 fits");
    member.extend_from_slice(&isize_field.to_le_bytes());
    member
}

#[test]
fn gzip_header_fields_are_clamped_to_the_callers_maxima() {
    // The single most security-relevant assertion in this file. `inflateGetHeader` is
    // handed three caller buffers and three advertised capacities, and the fields in the
    // stream are all longer than the capacities. Historically this is the gzip-header
    // overflow class; here every write goes through a `Cell` slice whose length *is* the
    // capacity, and a guard region past each one proves nothing spilled.
    // The three advertised capacities, deliberately far smaller than the fields the
    // stream carries.  Declared before the first statement so the items sit at the top
    // of their scope, where they are visible from anyway.
    const EXTRA_MAX: usize = 7;
    const NAME_MAX: usize = 5;
    const COMMENT_MAX: usize = 3;

    // `Cell` because a `GzHeaderView` field models storage the *application* owns and may
    // write while the compressor holds the borrow; this test owns the fixtures outright.
    let extra: Vec<Cell<u8>> = (0..200_u32)
        .map(|index| Cell::new(u8::try_from(index % 251).expect("modulo 251 fits a byte")))
        .collect();
    let mut name = b"a-rather-long-member-name-that-will-not-fit".to_vec();
    name.push(0);
    let name: Vec<Cell<u8>> = name.into_iter().map(Cell::new).collect();
    let mut comment = b"and a comment that is longer still, by some margin".to_vec();
    comment.push(0);
    let comment: Vec<Cell<u8>> = comment.into_iter().map(Cell::new).collect();

    let mut config = DeflateConfig::new(9);
    config.window_bits = GZIP;
    let compressed = compress_with(
        HELLO,
        config,
        None,
        Some(GzHeaderView::new(
            true,
            0x5a5a_5a5a,
            3,
            true,
            Some(&extra),
            Some(&name),
            Some(&comment),
        )),
    );

    let extra_cells = vec![Cell::new(HEADER_GUARD); EXTRA_MAX + HEADER_GUARD_LEN];
    let name_cells = vec![Cell::new(HEADER_GUARD); NAME_MAX + HEADER_GUARD_LEN];
    let comment_cells = vec![Cell::new(HEADER_GUARD); COMMENT_MAX + HEADER_GUARD_LEN];

    let mut state = inflate_init2(InflateConfig::new(AUTODETECT), GlobalAllocator)
        .expect("auto-detect window bits");
    let sink = GzHeaderSink::new(
        Some(&extra_cells[..EXTRA_MAX]),
        Some(&name_cells[..NAME_MAX]),
        Some(&comment_cells[..COMMENT_MAX]),
    );
    assert_eq!(sink.extra_max(), 7, "extra_max is the slice length");
    assert_eq!(sink.name_max(), 5, "name_max is the slice length");
    assert_eq!(sink.comm_max(), 3, "comm_max is the slice length");
    assert_eq!(
        inflate_get_header(&mut state, sink),
        ReturnCode::OK,
        "inflateGetHeader"
    );

    // One byte of input and eight bytes of room per call, so every header state has to
    // suspend and resume -- which is where a clamp that is applied once rather than on
    // every resumption would go wrong.
    let outcome = drive(
        &mut state,
        &compressed,
        1,
        8,
        Z_NO_FLUSH,
        adler_seed(AUTODETECT),
    );
    assert_eq!(
        outcome.last,
        ReturnCode::STREAM_END,
        "decompression must still complete"
    );
    assert_eq!(outcome.output, HELLO, "and must recover the payload");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    let written = |cells: &[Cell<u8>], upto: usize| -> Vec<u8> {
        cells[..upto].iter().map(Cell::get).collect()
    };
    // (b) Each field is truncated to exactly its advertised maximum, and the bytes that
    // did fit are the leading bytes of what the stream carried.
    assert_eq!(
        written(&extra_cells, EXTRA_MAX),
        written(&extra, EXTRA_MAX),
        "extra field"
    );
    assert_eq!(
        written(&name_cells, NAME_MAX),
        written(&name, NAME_MAX),
        "name"
    );
    assert_eq!(
        written(&comment_cells, COMMENT_MAX),
        written(&comment, COMMENT_MAX),
        "comment"
    );
    // (a) Nothing at all was written past any advertised maximum.
    for (label, cells, max) in [
        ("extra", &extra_cells, EXTRA_MAX),
        ("name", &name_cells, NAME_MAX),
        ("comment", &comment_cells, COMMENT_MAX),
    ] {
        assert!(
            cells[max..].iter().all(|cell| cell.get() == HEADER_GUARD),
            "{label}: the parser wrote past its advertised maximum"
        );
    }

    // The scalars the parse writes back -- `done`, `extra_len`, `text`, `time`,
    // `xflags`, `os`, `hcrc` -- live on the sink the state now owns, and the state
    // exposes no getter for it, so they are not observable from a downstream crate.
    // Their transitions are asserted by the `#[cfg(test)] mod tests` block in
    // `crates/zlib-rs/src/inflate/header.rs`; see delegation note 2 in this file's
    // header. What is observable is the part above, which is the part that could
    // corrupt a caller's memory.
}

#[test]
fn the_flg_bit_vocabulary_is_honoured() {
    // Every combination of the five defined flag bits must be accepted and must not
    // disturb the payload.
    for flg in 0..=(FTEXT | FHCRC | FEXTRA | FNAME | FCOMMENT) {
        let member = build_gzip_member(flg, b"XY", b"member", b"remark", HELLO);
        let outcome = decode(&member, GZIP);
        assert_eq!(
            outcome.last,
            ReturnCode::STREAM_END,
            "FLG {flg:#04x} must be accepted"
        );
        assert_eq!(outcome.output, HELLO, "FLG {flg:#04x} payload");
    }

    // Each reserved bit, alone and together, must be refused with the reference's own
    // message: `inflate.c` tests `state->flags & 0xe000`, which is the high three bits
    // of the flag byte.
    for reserved in [0x20_u8, 0x40, 0x80, FRESERVED] {
        let member = build_gzip_member(reserved, b"XY", b"member", b"remark", HELLO);
        let outcome = decode(&member, GZIP);
        assert_eq!(
            outcome.last,
            ReturnCode::DATA_ERROR,
            "FLG {reserved:#04x} must be refused"
        );
        assert_eq!(
            outcome.msg,
            Some(MSG_HEADER_FLAGS),
            "FLG {reserved:#04x} message"
        );
    }

    // And the reference's own vector for it.
    let outcome = inf(vector_named(&INF_VECTORS, "bad gzip flags"));
    assert_eq!(outcome.msg, Some(MSG_HEADER_FLAGS), "the reference vector");
}

#[test]
fn a_header_crc_mismatch_is_reported() {
    // A correct `FHCRC` is accepted.
    let good = build_gzip_member(FHCRC | FNAME, b"", b"member", b"", HELLO);
    let outcome = decode(&good, GZIP);
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "a correct header CRC");
    assert_eq!(outcome.output, HELLO, "payload");

    // Flipping one bit of it is not. The two `FHCRC` bytes sit immediately before the
    // DEFLATE payload, so the last header byte is at a known offset from the name.
    let hcrc_at = 10 + b"member".len() + 1;
    let mut bad = good.clone();
    bad[hcrc_at] ^= 0xff;
    let outcome = decode(&bad, GZIP);
    assert_eq!(
        outcome.last,
        ReturnCode::DATA_ERROR,
        "a corrupted header CRC must be refused"
    );
    assert_eq!(outcome.msg, Some(MSG_HEADER_CRC), "z_stream.msg");

    // And the reference's own vector, which pins the same string.
    let outcome = inf(vector_named(&INF_VECTORS, "bad header crc"));
    assert_eq!(outcome.msg, Some(MSG_HEADER_CRC), "the reference vector");
}

// ---------------------------------------------------------------------------
// 7. The remaining public API.
// ---------------------------------------------------------------------------

/// Reproduces `test_flush` (`test/example.c` L340-L371): three bytes of `plain`
/// followed by a `Z_FULL_FLUSH`, then the damage C inflicts at its L358
/// (`compr[3]++`, "force an error in first compressed block"), then the remainder
/// under `Z_FINISH`.
///
/// The damaged first block plus the flush point behind it is exactly what
/// `inflateSync` exists to recover from, and it is why the two tests are a pair in the
/// C harness too.
fn full_flush_stream_with_damage(plain: &[u8]) -> Vec<u8> {
    assert!(plain.len() > 3, "the fixture needs a damaged prefix");
    let mut state = deflate_init2(DeflateConfig::new(6), GlobalAllocator).expect("level 6");
    let reset = deflate_reset(&mut state);
    let mut out = vec![0_u8; 4096];

    let (head_len, total_in, total_out, adler) = {
        let mut stream = DeflateStream::new(&plain[..3], &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FULL_FLUSH),
            ReturnCode::OK,
            "deflate(Z_FULL_FLUSH)"
        );
        (
            stream.next_out,
            stream.total_in,
            stream.total_out,
            stream.adler,
        )
    };

    // `compr[3]++`: byte 3 is inside the first compressed block, so the block decodes
    // to something other than what was compressed -- or not at all.
    out[3] = out[3].wrapping_add(1);

    let tail_len = {
        let mut stream = DeflateStream::new(&plain[3..], &mut out[head_len..]);
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.adler = adler;
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "deflate(Z_FINISH)"
        );
        stream.next_out
    };

    assert_eq!(deflate_end(&mut state), ReturnCode::OK, "deflateEnd");
    out.truncate(head_len.saturating_add(tail_len));
    out
}

#[test]
fn sync_skips_a_damaged_block_like_the_reference_example() {
    // `test_sync` (`test/example.c` L373-L408), step for step.
    let damaged = full_flush_stream_with_damage(HELLO);
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    let mut room = vec![0_u8; 128];

    // "just read the zlib header".
    let (total_in, total_out, adler, consumed) = {
        let mut stream = InflateStream::new(&damaged[..2], &mut room);
        check_err(inflate(&mut state, &mut stream, Z_NO_FLUSH), "inflate");
        assert_eq!(stream.next_out, 0, "the header produces no output");
        (
            stream.total_in,
            stream.total_out,
            stream.adler,
            stream.next_in,
        )
    };
    assert_eq!(consumed, 2, "both header bytes are absorbed");

    // "read all compressed data ... but skip the damaged part".
    let skipped = {
        let mut stream = InflateStream::new(&damaged[2..], &mut room);
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.adler = adler;
        check_err(inflate_sync(&mut state, &mut stream), "inflateSync");
        stream.next_in
    };
    assert!(skipped > 0, "inflateSync must consume the damaged prefix");

    let resume_at = 2_usize.saturating_add(skipped);
    let mut stream = InflateStream::new(&damaged[resume_at..], &mut room);
    stream.adler = adler;
    assert_eq!(
        inflate(&mut state, &mut stream, Z_FINISH),
        ReturnCode::STREAM_END,
        "inflate should report Z_STREAM_END"
    );
    // C prints `"after inflateSync(): hel%s"`, i.e. the three bytes of the damaged
    // block are gone and everything after the flush point is recovered.
    assert_eq!(
        stream.written(),
        &HELLO[3..],
        "everything after the flush point must be recovered"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK, "inflateEnd");
}

#[test]
fn sync_reports_the_reference_statuses() {
    let mut state =
        inflate_init2(InflateConfig::new(RAW_SMALL), GlobalAllocator).expect("-8 window");
    let mut room = [0_u8; 1];

    // `test/infcover.c` L430-L432: one byte that is not part of the pattern.
    let stray = [0x80_u8];
    let mut stream = InflateStream::new(&stray, &mut room);
    assert_eq!(
        inflate_sync(&mut state, &mut stream),
        ReturnCode::DATA_ERROR,
        "no synchronisation point in a single stray byte"
    );
    // L433: `inflateSync` left the state in SYNC, which `inflate()` refuses outright.
    assert_eq!(
        inflate(&mut state, &mut stream, Z_NO_FLUSH),
        ReturnCode::STREAM_ERROR,
        "inflate() must refuse a state left in SYNC"
    );

    // L434-L436: the four-byte pattern is the tail of the empty stored block a
    // `Z_SYNC_FLUSH` emits, and finding it is what lets decoding restart.
    let pattern = [0x00_u8, 0x00, 0xff, 0xff];
    let mut stream = InflateStream::new(&pattern, &mut room);
    assert_eq!(
        inflate_sync(&mut state, &mut stream),
        ReturnCode::OK,
        "the 00 00 ff ff pattern is a synchronisation point"
    );
    assert_eq!(stream.next_in, 4, "and all four bytes are consumed");

    // With nothing buffered and nothing offered there is nothing to search.
    let mut fresh = inflate_init(GlobalAllocator).expect("the default configuration");
    let mut stream = InflateStream::new(&[], &mut room);
    assert_eq!(
        inflate_sync(&mut fresh, &mut stream),
        ReturnCode::BUF_ERROR,
        "no input and fewer than eight buffered bits"
    );

    assert_eq!(inflate_end(&mut fresh), ReturnCode::OK);
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn sync_point_marks_an_empty_stored_block_boundary() {
    // A `Z_SYNC_FLUSH` in the middle of a stream emits an empty stored block, and
    // `inflateSyncPoint` reports the moment the decoder is sitting on its length field
    // with an empty bit accumulator. Feeding one byte at a time is what makes that
    // moment observable from outside.
    let mut state = deflate_init2(DeflateConfig::new(6), GlobalAllocator).expect("level 6");
    let reset = deflate_reset(&mut state);
    let mut out = vec![0_u8; 4096];
    let (head_len, total_in, total_out, adler) = {
        let mut stream = DeflateStream::new(b"first", &mut out);
        stream.apply_reset(reset);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_SYNC_FLUSH),
            ReturnCode::OK,
            "deflate(Z_SYNC_FLUSH)"
        );
        (
            stream.next_out,
            stream.total_in,
            stream.total_out,
            stream.adler,
        )
    };
    let tail_len = {
        let mut stream = DeflateStream::new(b"second", &mut out[head_len..]);
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.adler = adler;
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "deflate(Z_FINISH)"
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(head_len.saturating_add(tail_len));

    let saw_sync_point = |bytes: &[u8]| -> bool {
        let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
        let mut seen = false;
        let mut read_at = 0_usize;
        for call in 0..bytes.len().saturating_add(3) {
            let end = if call == 0 {
                read_at
            } else {
                read_at.saturating_add(1).min(bytes.len())
            };
            let mut room = [0_u8; 32];
            let mut stream = InflateStream::new(&bytes[read_at..end], &mut room);
            let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
            read_at = read_at.saturating_add(stream.next_in);
            seen |= inflate_sync_point(&state);
            if matches!(ret, ReturnCode::STREAM_END | ReturnCode::DATA_ERROR) {
                break;
            }
        }
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        seen
    };

    assert!(
        saw_sync_point(&out),
        "a Z_SYNC_FLUSH stream must present a synchronisation point"
    );
    assert!(
        !saw_sync_point(&compress(HELLO, ZLIB)),
        "a stream with no flush point must never report one"
    );
}

#[test]
fn a_copy_decodes_the_remainder_independently() {
    let plain = capped(common::corpus::repetitive(2000));
    let compressed = compress(&plain, ZLIB);
    let mut state =
        inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");

    // Decode part of the stream, then fork.
    let mut room = vec![0_u8; 512];
    let (prefix, consumed, adler) = {
        let mut stream = InflateStream::new(&compressed[..compressed.len() / 2], &mut room);
        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        assert!(
            matches!(ret, ReturnCode::OK | ReturnCode::BUF_ERROR),
            "a partial stream must suspend, got {ret:?}"
        );
        (
            stream.written().to_vec(),
            stream.next_in,
            stream.adler.unwrap_or(0),
        )
    };
    assert!(!prefix.is_empty(), "the first half must produce output");

    let mut copy = inflate_copy(&state, GlobalAllocator).expect("inflateCopy");
    let tail = &compressed[consumed..];
    let from_source = drive(
        &mut state,
        tail,
        0,
        output_room(tail.len()),
        Z_NO_FLUSH,
        adler,
    );
    let from_copy = drive(
        &mut copy,
        tail,
        0,
        output_room(tail.len()),
        Z_NO_FLUSH,
        adler,
    );

    assert_eq!(from_source.last, ReturnCode::STREAM_END, "source status");
    assert_eq!(from_copy.last, ReturnCode::STREAM_END, "copy status");
    assert_eq!(
        from_source.output, from_copy.output,
        "the copy must decode identically"
    );
    assert_eq!(
        from_source.adler, from_copy.adler,
        "and must carry the same check value"
    );

    let mut whole = prefix;
    whole.extend_from_slice(&from_copy.output);
    assert_eq!(whole, plain, "prefix plus remainder is the payload");

    // Ending the copy must leave the source alone: it is still resettable and still
    // decodes.
    assert_eq!(inflate_end(&mut copy), ReturnCode::OK, "copy inflateEnd");
    assert!(
        inflate_reset2(&mut state, InflateConfig::new(ZLIB)).is_ok(),
        "the source survives the copy's teardown"
    );
    let again = drive(
        &mut state,
        &compressed,
        0,
        output_room(compressed.len()),
        Z_NO_FLUSH,
        adler_seed(ZLIB),
    );
    assert_eq!(again.last, ReturnCode::STREAM_END);
    assert_eq!(again.output, plain);
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn reset_keep_preserves_the_window_and_reset_clears_it() {
    // Deliberately NOT passed through `capped`: this payload's length is load-bearing.
    // The epilogue only refreshes the window when the call actually advanced the output
    // (`inflate.c` L1076-L1084), and the `CHECK` arm resets that checkpoint once the
    // trailer is credited, so a stream short enough to finish in a single call leaves
    // `whave` at zero and there is no kept history for `inflateResetKeep` to keep. A
    // 1000-byte run of one byte is cheap even under the interpreter.
    let plain = common::corpus::repetitive(1000);
    let compressed = compress(&plain, ZLIB);
    let mut state =
        inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");
    let outcome = drive(
        &mut state,
        &compressed,
        0,
        output_room(compressed.len()),
        Z_NO_FLUSH,
        adler_seed(ZLIB),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END);
    assert_eq!(outcome.output, plain);

    let history_len = |state: &InflateState<'_, GlobalAllocator>| -> u32 {
        let mut length = u32::MAX;
        assert_eq!(
            inflate_get_dictionary(state, None, Some(&mut length)),
            ReturnCode::OK,
            "inflateGetDictionary"
        );
        length
    };

    let kept = history_len(&state);
    assert!(kept > 0, "a completed decode leaves history in the window");

    // `inflateResetKeep` (`inflate.c` L99-L123) leaves the window cursors alone.
    let reset = inflate_reset_keep(&mut state);
    assert_eq!(reset.total_in, 0, "total_in");
    assert_eq!(reset.total_out, 0, "total_out");
    assert_eq!(reset.msg, None, "msg");
    assert_eq!(reset.data_type, 0, "data_type");
    assert_eq!(reset.adler, Some(1), "a zlib stream reseeds adler to 1");
    assert_eq!(
        history_len(&state),
        kept,
        "inflateResetKeep must keep the window contents"
    );

    // `inflateReset` (L124-L133) clears them first.
    let reset = inflate_reset(&mut state);
    assert_eq!(reset.adler, Some(1), "a zlib stream reseeds adler to 1");
    assert_eq!(
        history_len(&state),
        0,
        "inflateReset must clear the window cursors"
    );

    // And a reset stream decodes exactly as a brand-new one does.
    let again = drive(
        &mut state,
        &compressed,
        0,
        output_room(compressed.len()),
        Z_NO_FLUSH,
        adler_seed(ZLIB),
    );
    let fresh = decode(&compressed, ZLIB);
    assert_eq!(again.last, fresh.last, "status");
    assert_eq!(again.output, fresh.output, "output");
    assert_eq!(again.adler, fresh.adler, "check value");
    assert_eq!(again.total_in, fresh.total_in, "total_in");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn reset2_changes_the_container_mid_life() {
    let plain = capped(common::corpus::text());
    let zlib_stream = compress(&plain, ZLIB);
    let gzip_stream = compress(&plain, GZIP);
    let raw_stream = compress(&plain, RAW);

    let mut state =
        inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");
    for (window_bits, bytes) in [
        (ZLIB, &zlib_stream),
        (GZIP, &gzip_stream),
        (RAW, &raw_stream),
        (AUTODETECT, &gzip_stream),
        (AUTODETECT, &zlib_stream),
    ] {
        let reset = inflate_reset2(&mut state, InflateConfig::new(window_bits));
        assert!(reset.is_ok(), "inflateReset2({window_bits})");
        let outcome = drive(
            &mut state,
            bytes,
            0,
            output_room(bytes.len()),
            Z_NO_FLUSH,
            adler_seed(window_bits),
        );
        assert_eq!(
            outcome.last,
            ReturnCode::STREAM_END,
            "windowBits {window_bits}: status"
        );
        assert_eq!(outcome.output, plain, "windowBits {window_bits}: output");
    }

    // The window *size* really changes too, and the way to see it is a stream whose
    // matches reach further back than the new window -- with an output buffer small
    // enough that the history has to come out of that window rather than out of the
    // output buffer the decoder is still writing into. With a large enough buffer the
    // reference accepts such a stream, because the match source is still addressable;
    // this behaviour was confirmed against it directly.
    let mut long_distance = b"MARKER".to_vec();
    long_distance.resize(1006, 0);
    long_distance.extend_from_slice(b"MARKER");
    let far = compress(&long_distance, RAW);

    assert!(inflate_reset2(&mut state, InflateConfig::new(RAW_SMALL)).is_ok());
    let outcome = drive(&mut state, &far, 0, 64, Z_NO_FLUSH, adler_seed(RAW_SMALL));
    assert_eq!(
        outcome.last,
        ReturnCode::DATA_ERROR,
        "a 256-byte window must refuse a match from 1006 bytes back"
    );
    assert_eq!(outcome.msg, Some(MSG_DISTANCE_TOO_FAR), "z_stream.msg");

    // The same stream through a 32K window decodes, so the refusal is about the window
    // size `inflateReset2` installed and nothing else.
    assert!(inflate_reset2(&mut state, InflateConfig::new(RAW)).is_ok());
    let outcome = drive(&mut state, &far, 0, 64, Z_NO_FLUSH, adler_seed(RAW));
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "32K window status");
    assert_eq!(outcome.output, long_distance, "32K window output");

    // An invalid request is refused, and refusing it leaves the state usable.
    assert_eq!(
        inflate_reset2(&mut state, InflateConfig::new(1)).err(),
        Some(ReturnCode::STREAM_ERROR),
        "windowBits of 1 is not a window size"
    );
    assert!(inflate_reset2(&mut state, InflateConfig::new(RAW)).is_ok());
    let outcome = drive(
        &mut state,
        &raw_stream,
        0,
        output_room(raw_stream.len()),
        Z_NO_FLUSH,
        adler_seed(RAW),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "still usable");
    assert_eq!(outcome.output, plain);
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn prime_accepts_and_refuses_what_the_reference_does() {
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");

    // `cover_support()` (`test/infcover.c` L360-L361).
    assert_eq!(inflate_prime(&mut state, 5, 31), ReturnCode::OK, "(5, 31)");
    assert_eq!(
        inflate_prime(&mut state, -1, 0),
        ReturnCode::OK,
        "(-1, 0) discards the accumulator"
    );
    assert_eq!(inflate_prime(&mut state, 0, 0), ReturnCode::OK, "(0, 0)");

    // More than sixteen bits in one call is refused (`inflate.c` L215-L232).
    assert_eq!(
        inflate_prime(&mut state, 17, 0),
        ReturnCode::STREAM_ERROR,
        "(17, 0)"
    );
    // And so is a request that would take the accumulator past 32 bits.
    assert_eq!(inflate_prime(&mut state, -1, 0), ReturnCode::OK);
    assert_eq!(inflate_prime(&mut state, 16, 0), ReturnCode::OK, "16 bits");
    assert_eq!(inflate_prime(&mut state, 16, 0), ReturnCode::OK, "32 bits");
    assert_eq!(
        inflate_prime(&mut state, 16, 0),
        ReturnCode::STREAM_ERROR,
        "48 bits does not fit"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // The documented use: hand the decoder bits it will never see as input. Priming
    // with the stream's own first byte and then feeding the rest must decode it.
    let raw = compress(HELLO, RAW);
    let mut state =
        inflate_init2(InflateConfig::new(RAW), GlobalAllocator).expect("raw window bits");
    assert_eq!(
        inflate_prime(&mut state, 8, i32::from(raw[0])),
        ReturnCode::OK,
        "priming with the first byte"
    );
    let outcome = drive(
        &mut state,
        &raw[1..],
        0,
        output_room(raw.len()),
        Z_NO_FLUSH,
        adler_seed(RAW),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "primed status");
    assert_eq!(outcome.output, HELLO, "primed output");
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn mark_reports_the_documented_accounting() {
    // `zlib.h` L1042-L1060: the value packs the number of bits of the last code back
    // from the current position into the high half, and the progress through a copy into
    // the low half. An initial or inconsistent state reports `-1 << 16`.
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    assert_eq!(
        inflate_mark(&state),
        INFLATE_MARK_BAD_STATE,
        "an initial state has no mark"
    );

    let plain = capped(common::corpus::repetitive(600));
    let compressed = compress(&plain, ZLIB);
    let mut marks = Vec::new();
    let mut read_at = 0_usize;
    for call in 0..compressed.len().saturating_add(3) {
        let end = if call == 0 {
            read_at
        } else {
            read_at.saturating_add(1).min(compressed.len())
        };
        let mut room = [0_u8; 4];
        let mut stream = InflateStream::new(&compressed[read_at..end], &mut room);
        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        read_at = read_at.saturating_add(stream.next_in);
        marks.push(inflate_mark(&state));
        if matches!(ret, ReturnCode::STREAM_END | ReturnCode::DATA_ERROR) {
            break;
        }
    }
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    assert!(
        marks.iter().any(|mark| *mark != INFLATE_MARK_BAD_STATE),
        "the mark must become meaningful once decoding starts"
    );
    assert!(
        marks.iter().all(|mark| *mark >= INFLATE_MARK_BAD_STATE),
        "no mark may be more negative than the initial one, saw {marks:?}"
    );
    for mark in &marks {
        // The high half is the bit count of the code in progress, which no code can
        // exceed; the low half is progress through a copy, bounded by `MAX_MATCH`.
        let back = mark >> 16;
        let progress = mark & 0xffff;
        assert!(
            (-1..=48).contains(&back),
            "bits back out of range in {mark}"
        );
        assert!(progress <= 258, "copy progress out of range in {mark}");
    }
}

#[test]
fn a_dictionary_stream_is_recovered_like_the_reference_example() {
    // `test_dict_deflate` and `test_dict_inflate` (`test/example.c` L414-L490).
    // `DICTIONARY` is `b"hello\0"`, six bytes: C passes `sizeof(dictionary)`, which
    // includes the terminator, so the id is computed over it too.
    let compressed = compress_with(HELLO, DeflateConfig::new(9), Some(DICTIONARY), None);
    let dict_id = adler32(1, DICTIONARY);

    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    let mut room = vec![0_u8; 64];
    let (consumed, adler) = {
        let mut stream = InflateStream::new(&compressed, &mut room);
        assert_eq!(
            inflate(&mut state, &mut stream, Z_NO_FLUSH),
            ReturnCode::NEED_DICT,
            "the stream must ask for its dictionary"
        );
        assert_eq!(
            stream.adler,
            Some(dict_id),
            "and must publish the dictionary's Adler-32 as its id"
        );
        (stream.next_in, stream.adler)
    };

    // A dictionary whose id does not match is refused, and refusing it does not spoil
    // the stream.
    assert_eq!(
        inflate_set_dictionary(&mut state, b"goodbye"),
        ReturnCode::DATA_ERROR,
        "the wrong dictionary"
    );
    assert_eq!(
        inflate_set_dictionary(&mut state, DICTIONARY),
        ReturnCode::OK,
        "the right dictionary"
    );

    let mut stream = InflateStream::new(&compressed[consumed..], &mut room);
    stream.adler = adler;
    assert_eq!(
        inflate(&mut state, &mut stream, Z_FINISH),
        ReturnCode::STREAM_END,
        "inflate with dict"
    );
    assert_eq!(stream.written(), HELLO, "bad inflate with dict");

    // `inflateGetDictionary` reports the window -- and note what it does *not* hold:
    // the fourteen bytes just decoded. Once the trailer has been credited, the `CHECK`
    // arm resets the epilogue's output checkpoint (`out = left`, `inflate.c` L1081), so
    // a stream that finishes inside a single call credits no window update on that call.
    // There is no next call to keep history for, so nothing is lost. This was verified
    // against the reference implementation, which reports the same six bytes here.
    let mut history = vec![0_u8; 1 << 15];
    let mut length = 0_u32;
    assert_eq!(
        inflate_get_dictionary(
            &state,
            Some(&mut OutputRegion::init(&mut history)),
            Some(&mut length)
        ),
        ReturnCode::OK,
        "inflateGetDictionary"
    );
    let length = usize::try_from(length).expect("a window length fits in a usize");
    assert_eq!(
        &history[..length],
        DICTIONARY,
        "a single-call decode leaves only the preset dictionary in the window"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // Decoded a few bytes at a time, the intermediate calls *do* update the window, so
    // the history then holds the dictionary followed by what has been decoded so far.
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    let mut room = vec![0_u8; 4];
    let (consumed, adler) = {
        let mut stream = InflateStream::new(&compressed, &mut room);
        assert_eq!(
            inflate(&mut state, &mut stream, Z_NO_FLUSH),
            ReturnCode::NEED_DICT,
            "the stream must ask for its dictionary"
        );
        (stream.next_in, stream.adler.unwrap_or(0))
    };
    assert_eq!(
        inflate_set_dictionary(&mut state, DICTIONARY),
        ReturnCode::OK
    );
    let outcome = drive(&mut state, &compressed[consumed..], 0, 4, Z_NO_FLUSH, adler);
    assert_eq!(outcome.last, ReturnCode::STREAM_END, "piecewise status");
    assert_eq!(outcome.output, HELLO, "piecewise output");

    let mut history = vec![0_u8; 1 << 15];
    let mut length = 0_u32;
    assert_eq!(
        inflate_get_dictionary(
            &state,
            Some(&mut OutputRegion::init(&mut history)),
            Some(&mut length)
        ),
        ReturnCode::OK,
        "inflateGetDictionary"
    );
    let length = usize::try_from(length).expect("a window length fits in a usize");
    assert!(
        length > DICTIONARY.len(),
        "piecewise decoding must have extended the window past the dictionary"
    );
    assert!(
        length <= DICTIONARY.len() + HELLO.len(),
        "and cannot hold more than was ever put in it"
    );
    assert!(
        history[..length].starts_with(DICTIONARY),
        "the history must begin with the preset dictionary"
    );
    assert!(
        HELLO.starts_with(&history[DICTIONARY.len()..length]),
        "and continue with the decoded payload in stream order"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn set_dictionary_refuses_a_stream_that_wants_none() {
    // `cover_support()` L362-L363 passes `Z_NULL` with a length of zero; the guard it
    // trips is on the stream, not on the buffer, so an empty slice reproduces it
    // exactly: a wrapped stream that is not waiting for a dictionary is refused.
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    assert_eq!(
        inflate_set_dictionary(&mut state, &[]),
        ReturnCode::STREAM_ERROR,
        "an absent dictionary on a fresh zlib stream"
    );
    assert_eq!(
        inflate_set_dictionary(&mut state, DICTIONARY),
        ReturnCode::STREAM_ERROR,
        "and so is a real one"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // `cover_wrap()` L425-L427: a *raw* stream accepts one at any time, and 257 zero
    // bytes is the length the C harness uses.
    let mut state =
        inflate_init2(InflateConfig::new(RAW_SMALL), GlobalAllocator).expect("-8 window");
    let dict = [0_u8; 257];
    assert_eq!(
        inflate_set_dictionary(&mut state, &dict),
        ReturnCode::OK,
        "a raw stream accepts a dictionary"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

#[test]
fn undermine_is_refused_in_the_shipped_configuration() {
    // `test/infcover.c` L440 asserts `Z_DATA_ERROR`, and that is the *correct* answer,
    // not a failure: the library is not built with the option that lets invalid
    // distances through, so there is nothing to undermine.
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    assert_eq!(
        inflate_undermine(&mut state, true),
        ReturnCode::DATA_ERROR,
        "inflateUndermine(1)"
    );
    assert_eq!(
        inflate_undermine(&mut state, false),
        ReturnCode::DATA_ERROR,
        "inflateUndermine(0)"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // And the refusal changes nothing: a good stream still decodes and a bad distance
    // is still an error.
    let compressed = compress(HELLO, ZLIB);
    let mut state = inflate_init(GlobalAllocator).expect("the default configuration");
    assert_eq!(inflate_undermine(&mut state, true), ReturnCode::DATA_ERROR);
    let outcome = drive(
        &mut state,
        &compressed,
        0,
        output_room(compressed.len()),
        Z_NO_FLUSH,
        adler_seed(ZLIB),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END);
    assert_eq!(outcome.output, HELLO);
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    let still_bad = try_raw("c c0 81 0 0 0 0 0 90 ff 6b 4 0", MSG_DISTANCE_TOO_FAR, 1);
    assert_eq!(still_bad.msg, Some(MSG_DISTANCE_TOO_FAR));
}

#[test]
fn validation_can_be_turned_off_and_on() {
    for (window_bits, message) in [(ZLIB, MSG_DATA_CHECK), (GZIP, MSG_DATA_CHECK)] {
        let mut compressed = compress(HELLO, window_bits);
        // Corrupt the check value. For zlib it is the last four bytes; for gzip the
        // CRC-32 is the four bytes before the length, so the fifth byte from the end.
        let target = if window_bits == GZIP {
            compressed.len() - 5
        } else {
            compressed.len() - 1
        };
        compressed[target] ^= 0xff;

        // Validation is on by default, so the mismatch is a data error.
        let outcome = decode(&compressed, window_bits);
        assert_eq!(
            outcome.last,
            ReturnCode::DATA_ERROR,
            "windowBits {window_bits}: a wrong check value must be refused"
        );
        assert_eq!(outcome.msg, Some(message), "windowBits {window_bits}");

        // Turning it off makes the same stream decode (`inflate.c` L1384-L1394).
        let mut state =
            inflate_init2(InflateConfig::new(window_bits), GlobalAllocator).expect("window bits");
        assert_eq!(
            inflate_validate(&mut state, false),
            ReturnCode::OK,
            "inflateValidate(0)"
        );
        let outcome = drive(
            &mut state,
            &compressed,
            0,
            output_room(compressed.len()),
            Z_NO_FLUSH,
            adler_seed(window_bits),
        );
        assert_eq!(
            outcome.last,
            ReturnCode::STREAM_END,
            "windowBits {window_bits}: validation off"
        );
        assert_eq!(outcome.output, HELLO, "windowBits {window_bits}");

        // Turning it back on restores the refusal.
        assert!(inflate_reset2(&mut state, InflateConfig::new(window_bits)).is_ok());
        assert_eq!(
            inflate_validate(&mut state, true),
            ReturnCode::OK,
            "inflateValidate(1)"
        );
        let outcome = drive(
            &mut state,
            &compressed,
            0,
            output_room(compressed.len()),
            Z_NO_FLUSH,
            adler_seed(window_bits),
        );
        assert_eq!(
            outcome.last,
            ReturnCode::DATA_ERROR,
            "windowBits {window_bits}: validation on again"
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }
}

#[test]
fn codes_used_reports_the_arena_occupancy() {
    let enough = u64::try_from(ENOUGH).expect("1444 fits in a u64");

    // A `Z_FIXED` stream uses the shared static tables, so it takes no arena entries.
    let fixed = compress_at(HELLO, RAW, 9, Strategy::Fixed);
    let mut state =
        inflate_init2(InflateConfig::new(RAW), GlobalAllocator).expect("raw window bits");
    assert_eq!(
        inflate_codes_used(&state),
        0,
        "a fresh state has built no tables"
    );
    let outcome = drive(
        &mut state,
        &fixed,
        0,
        output_room(fixed.len()),
        Z_NO_FLUSH,
        adler_seed(RAW),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END);
    assert_eq!(outcome.output, HELLO);
    assert_eq!(
        inflate_codes_used(&state),
        0,
        "a fixed-code block builds no dynamic tables"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);

    // A dynamic block does, and never more than the arena holds.
    let plain = capped(common::corpus::text());
    let dynamic = compress_at(&plain, ZLIB, 9, Strategy::Default);
    let mut state =
        inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");
    let outcome = drive(
        &mut state,
        &dynamic,
        0,
        output_room(dynamic.len()),
        Z_NO_FLUSH,
        adler_seed(ZLIB),
    );
    assert_eq!(outcome.last, ReturnCode::STREAM_END);
    assert_eq!(outcome.output, plain);
    let used = inflate_codes_used(&state);
    assert!(used > 0, "a dynamic block must occupy arena entries");
    assert!(used <= enough, "{used} entries exceeds ENOUGH ({enough})");
    assert_ne!(
        used, INFLATE_CODES_USED_BAD_STATE,
        "a live state must report a real count"
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

// ---------------------------------------------------------------------------
// 8. The `ENOUGH` capacity, reached through the public path.
//
// DELEGATION: the deliberately *exceeded* cases -- `cover_trees()`'s two
// `inflate_table(DISTS, ...)` calls at `test/infcover.c` L632 and L636, both expecting
// the return value 1 -- cannot be produced through the public API, because, as that
// function's own comment puts it, "zlib insures that enough is always enough". They
// live in the `#[cfg(test)] mod tests` block of
// `crates/zlib-rs/src/inflate/inftrees.rs`, in
// `enough_is_reported_when_the_table_would_not_fit`. What follows is the reachable
// half of the same property: through the public API the bound always holds.
// ---------------------------------------------------------------------------

#[test]
fn code_tables_never_exceed_the_enough_capacity() {
    let enough = u64::try_from(ENOUGH).expect("1444 fits in a u64");

    // Payloads chosen to drive the code sets as wide as a legitimate stream can: high
    // entropy needs nearly every literal, repetitive data needs the long length and
    // distance codes, and data that crosses the window boundary needs both.
    let payloads: [(&str, Vec<u8>); 5] = [
        (
            "incompressible",
            capped(common::corpus::incompressible(20_000)),
        ),
        ("binary", capped(common::corpus::binary())),
        ("text", capped(common::corpus::text())),
        ("window crossing", capped(common::corpus::window_crossing())),
        ("repetitive", capped(common::corpus::repetitive(20_000))),
    ];
    for (name, plain) in payloads {
        for strategy in [
            Strategy::Default,
            Strategy::Filtered,
            Strategy::HuffmanOnly,
            Strategy::Rle,
        ] {
            let compressed = compress_at(&plain, ZLIB, 9, strategy);
            let mut state =
                inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");
            let outcome = drive(
                &mut state,
                &compressed,
                0,
                output_room(compressed.len()),
                Z_NO_FLUSH,
                adler_seed(ZLIB),
            );
            assert_eq!(
                outcome.last,
                ReturnCode::STREAM_END,
                "{name} with {strategy:?}: no stream may be refused for want of table space"
            );
            assert_eq!(outcome.output, plain, "{name} with {strategy:?}: output");
            let used = inflate_codes_used(&state);
            assert!(
                used <= enough,
                "{name} with {strategy:?}: {used} entries exceeds ENOUGH ({enough})"
            );
            assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        }
    }

    // The reference's own maximal-code-set vectors are refused for what they actually
    // are -- an over-subscribed or incomplete code -- and never for table space.
    for (hex, message) in [
        (
            "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
            MSG_LITERAL_LENGTHS_SET,
        ),
        ("4 80 49 92 24 49 92 24 f b4 ff ff c3 84", MSG_DISTANCES_SET),
    ] {
        let bytes = h2b(hex);
        let mut state =
            inflate_init2(InflateConfig::new(RAW), GlobalAllocator).expect("raw window bits");
        let outcome = drive(
            &mut state,
            &bytes,
            0,
            output_room(bytes.len()),
            Z_NO_FLUSH,
            adler_seed(RAW),
        );
        assert_eq!(outcome.last, ReturnCode::DATA_ERROR, "{message}");
        assert_eq!(outcome.msg, Some(message), "{message}");
        assert!(
            inflate_codes_used(&state) <= enough,
            "{message}: the arena cursor must stay inside ENOUGH"
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }
}

#[test]
fn data_type_reports_the_accumulator_and_the_block_flags() {
    // `inflate.c` L1147-L1149: the low bits are the accumulator occupancy, 64 marks the
    // last block, 128 marks a suspension in `TYPE` and 256 one in `LEN_` or `COPY_`.
    let compressed = compress(HELLO, ZLIB);

    let finished = decode(&compressed, ZLIB);
    assert_eq!(finished.last, ReturnCode::STREAM_END);
    assert_eq!(
        finished.data_type & 64,
        64,
        "a completed stream has seen its last block, got {}",
        finished.data_type
    );
    assert_eq!(
        finished.data_type & 0x3f,
        0,
        "and its accumulator is empty, got {}",
        finished.data_type
    );

    // `Z_BLOCK` stops at a block boundary, which is a suspension in `TYPE`.
    let mut state =
        inflate_init2(InflateConfig::new(ZLIB), GlobalAllocator).expect("zlib window bits");
    let mut room = vec![0_u8; 64];
    let mut stream = InflateStream::new(&compressed, &mut room);
    let ret = inflate(&mut state, &mut stream, Z_BLOCK);
    assert!(
        matches!(ret, ReturnCode::OK | ReturnCode::STREAM_END),
        "Z_BLOCK on a whole stream, got {ret:?}"
    );
    assert!(
        stream.data_type & 0x3f <= 32,
        "the accumulator can hold at most 32 bits, got {}",
        stream.data_type
    );
    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
}

// ---------------------------------------------------------------------------
// 9. Allocation failure.
//
// `test/infcover.c`'s instrumented allocator is the single most valuable piece of the
// C suite for a reimplementation, and `common::TrackingAllocator` is its port: it
// fills every block with `0xa5` rather than zeroing it, it detects a leak, a release
// out of order and a release of a block it never handed out, and its ceiling makes
// `Z_MEM_ERROR` reachable on demand.
// ---------------------------------------------------------------------------

#[test]
fn the_cover_wrap_memory_sequence_reproduces_the_reference_statuses() {
    // `cover_wrap()` (`test/infcover.c` L412-L443), step for step.
    let tracker = TrackingAllocator::new();
    {
        let mut state = inflate_init2(InflateConfig::new(RAW_SMALL), &tracker)
            .expect("-8 is a valid decoder window");
        let mut room = [0_u8; 1];

        // L416-L423. One byte of room, and allocation capped at one byte, so the window
        // the epilogue needs cannot be obtained. A memory error out of `inflate()` is
        // non-recoverable -- the reference says so at `inflate.c` L1129 -- which is why
        // the second call reports it again instead of resuming.
        tracker.set_limit(1);
        {
            let input = [0x63_u8, 0x00];
            let mut stream = InflateStream::new(&input, &mut room);
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::MEM_ERROR,
                "the first call cannot allocate a window"
            );
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::MEM_ERROR,
                "and the error is latched, not retried"
            );
        }
        tracker.set_limit(0);

        // L425-L427: 257 zero bytes into a raw stream, which is what allocates the
        // window in the first place.
        let dict = [0_u8; 257];
        assert_eq!(
            inflate_set_dictionary(&mut state, &dict),
            ReturnCode::OK,
            "a raw stream accepts a dictionary even after a memory error"
        );
        let outstanding = tracker.total();
        assert_eq!(outstanding, 1 << 8, "the -8 window is 256 bytes");

        // L428. C caps allocation at `(sizeof(struct inflate_state) << 1) + 256`, a
        // figure tuned so that the copy's *state* fits and its *window* does not. Here
        // the state is a by-value Rust struct, so the allocator only ever sees the
        // window: the equivalent ceiling is exactly what is already outstanding, which
        // leaves room for nothing more. The C figure is computed rather than written
        // down, so that the adaptation is visible instead of silent.
        let c_budget = size_of_val(&state).saturating_mul(2).saturating_add(256);
        assert!(
            c_budget > outstanding,
            "the C ceiling ({c_budget}) is the looser of the two"
        );
        tracker.set_limit(outstanding);

        // L429.
        assert_eq!(
            inflate_prime(&mut state, 16, 0),
            ReturnCode::OK,
            "inflatePrime(16, 0)"
        );

        // L430-L433.
        {
            let stray = [0x80_u8];
            let mut stream = InflateStream::new(&stray, &mut room);
            assert_eq!(
                inflate_sync(&mut state, &mut stream),
                ReturnCode::DATA_ERROR,
                "inflateSync finds no pattern"
            );
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::STREAM_ERROR,
                "inflate() refuses a state left in SYNC"
            );
        }

        // L434-L436.
        {
            let pattern = [0_u8, 0, 0xff, 0xff];
            let mut stream = InflateStream::new(&pattern, &mut room);
            assert_eq!(
                inflate_sync(&mut state, &mut stream),
                ReturnCode::OK,
                "inflateSync finds the pattern"
            );
        }
        assert!(
            !inflate_sync_point(&state),
            "a synchronised stream is placed at TYPE, not at a stored-block length"
        );

        // L437: the copy needs a window of its own, and the ceiling forbids it.
        assert_eq!(
            inflate_copy(&state, &tracker).err(),
            Some(ReturnCode::MEM_ERROR),
            "inflateCopy under the ceiling"
        );
        tracker.set_limit(0);

        // L439-L441.
        assert_eq!(
            inflate_undermine(&mut state, true),
            ReturnCode::DATA_ERROR,
            "inflateUndermine is refused in this configuration"
        );
        let _mark = inflate_mark(&state);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK, "inflateEnd");
    }
    // L442: `mem_done`.
    tracker.assert_clean();
}

#[test]
fn an_exhausted_allocator_reports_a_sticky_memory_error() {
    let tracker = TrackingAllocator::new();
    let compressed = compress(HELLO, ZLIB);
    {
        let mut state =
            inflate_init2(InflateConfig::new(ZLIB), &tracker).expect("zlib window bits");
        let mut room = [0_u8; 1];

        tracker.set_limit(1);
        for attempt in 0..2 {
            let mut stream = InflateStream::new(&compressed, &mut room);
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::MEM_ERROR,
                "attempt {attempt}"
            );
        }

        // Lifting the ceiling does not un-stick it: the reference records at
        // `inflate.c` L1129 that a memory error from `inflate()` cannot be resumed from.
        tracker.set_limit(0);
        {
            let mut stream = InflateStream::new(&compressed, &mut room);
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::MEM_ERROR,
                "the latch survives the ceiling being lifted"
            );
        }

        // A reset does clear it, and the stream then decodes normally -- so the state is
        // still sound, it was only refusing to pretend the failure had not happened.
        let reset = inflate_reset(&mut state);
        assert_eq!(reset.msg, None, "the reset clears the message");
        let outcome = drive(
            &mut state,
            &compressed,
            0,
            output_room(compressed.len()),
            Z_NO_FLUSH,
            adler_seed(ZLIB),
        );
        assert_eq!(outcome.last, ReturnCode::STREAM_END, "usable after a reset");
        assert_eq!(outcome.output, HELLO, "and correct");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }
    // No leak, no out-of-order release and no unrecognised release, even across the
    // failures: a refused allocation must record nothing at all.
    tracker.assert_clean();
}

#[test]
fn the_lazily_allocated_window_reports_a_memory_error_not_a_panic() {
    // The window is `1 << wbits` bytes and is allocated on first use, so a ceiling one
    // byte below it is refused at exactly the moment the decoder needs history.
    for window_bits in [RAW, ZLIB, GZIP] {
        let tracker = TrackingAllocator::new();
        let compressed = compress(&capped(common::corpus::repetitive(2000)), window_bits);
        {
            let mut state = inflate_init2(InflateConfig::new(window_bits), &tracker)
                .expect("window bits should be accepted");
            tracker.set_limit((1_usize << 15) - 1);
            let outcome = drive(
                &mut state,
                &compressed,
                0,
                32,
                Z_NO_FLUSH,
                adler_seed(window_bits),
            );
            assert_eq!(
                outcome.last,
                ReturnCode::MEM_ERROR,
                "windowBits {window_bits}: a window one byte too large must be refused"
            );
            assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        }
        tracker.assert_clean();
        assert_eq!(
            tracker.high_water(),
            0,
            "windowBits {window_bits}: a refused allocation must record nothing"
        );
    }
}

#[test]
fn a_sentinel_filling_allocator_still_decodes_correctly() {
    // Every block comes back filled with `0xa5`, never zeroed -- `mem_alloc`
    // (`test/infcover.c` L87) and the reason its own comment gives: "fill memory with a
    // non-zero value to make sure that the code isn't depending on zeros". The generic
    // `zcalloc` reaches `malloc` rather than `calloc` on every target this port
    // supports, so anything that assumes a fresh buffer is zeroed is simply wrong, and
    // this is what makes it fail loudly instead of passing by luck.
    let payloads: [(&str, Vec<u8>); 4] = [
        ("hello", HELLO.to_vec()),
        ("text", capped(common::corpus::text())),
        ("repetitive", capped(common::corpus::repetitive(2000))),
        (
            "incompressible",
            capped(common::corpus::incompressible(2000)),
        ),
    ];
    for (name, plain) in payloads {
        for window_bits in [RAW, ZLIB, GZIP] {
            let tracker = TrackingAllocator::new();
            let compressed = compress(&plain, window_bits);
            {
                let mut state = inflate_init2(InflateConfig::new(window_bits), &tracker)
                    .expect("window bits should be accepted");
                // A small output buffer, so the window really is allocated and really is
                // read back out of.
                let outcome = drive(
                    &mut state,
                    &compressed,
                    0,
                    64,
                    Z_NO_FLUSH,
                    adler_seed(window_bits),
                );
                assert_eq!(
                    outcome.last,
                    ReturnCode::STREAM_END,
                    "{name} at windowBits {window_bits}: status"
                );
                assert_eq!(
                    outcome.output, plain,
                    "{name} at windowBits {window_bits}: output"
                );
                assert_eq!(inflate_end(&mut state), ReturnCode::OK);
            }
            tracker.assert_clean();
            // A payload that fits inside one output buffer never needs history, so the
            // window is only expected for the ones that do not.
            if plain.len() > 64 {
                assert!(
                    tracker.high_water() >= 1 << 15,
                    "{name} at windowBits {window_bits}: the window must have been allocated"
                );
            }
        }
    }
}
