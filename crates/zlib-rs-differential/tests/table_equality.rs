//! Element-for-element equality of the port's `const` tables against the C reference's arrays.
//!
//! This is the cheapest high-signal check in the whole port, and the one to run **first**. Three
//! of the reference's tables are generated files -- `crc32.h` carries the banner "Generated
//! automatically by crc32.c", `trees.h` "header created automatically with `-DGEN_TREES_H`", and
//! `inffixed.h` "Generated automatically by `makefixed()`" -- and the port transcribes all three
//! into `const` arrays, some nine thousand literals in total. A single mistyped digit anywhere in
//! them does not make the encoder worse in a way a decompressor could detect; it makes it emit
//! *different bytes*, for essentially every input, at whichever level or block type reads the
//! affected entry. So a byte-identity failure whose real cause is a transcription slip would show
//! up as thousands of failing configurations in the differential matrix and point at none of them.
//! Running these comparisons first turns that into one named index in one named table.
//!
//! Every comparison here reads the C object rather than a second transcription. `zutil.h` defines
//! `local` as `static`, so nine of these arrays have no linkable symbol at all; the harness's
//! `build.rs` generates a small C shim that hands out a pointer and an element count per table, and
//! `crates/zlib-rs-differential/src/oracle.rs` wraps each pair in a safe `&'static [T]`. This file
//! consumes those wrappers, which is why it contains **no `unsafe` of its own** -- see the
//! "Contract" section below.
//!
//! # What is compared
//!
//! Twelve table families, one `#[test]` each. The tests are independent, immutable, read-only
//! comparisons over process-lifetime data with no shared mutable state, so Cargo running them in
//! parallel is safe and deliberate. (Contrast `crates/libz-rs-sys/tests/c_api_parity.rs`, which
//! puts everything in one `#[test]` because *its* helpers are order-dependent.)
//!
//! | Port | C reference | Entries |
//! |---|---|---|
//! | `CRC_TABLE` | `crc_table`, `crc32.h:5` | 256 |
//! | `X2N_TABLE` | `x2n_table`, `crc32.h:9439` | 32 |
//! | `CRC_BRAID_TABLE` | `crc_braid_table`, `crc32.h:6365` or `crc32.h:7475` | `W` x 256 |
//! | `static_ltree` | `static_ltree`, `trees.h:3` | 288 |
//! | `static_dtree` | `static_dtree`, `trees.h:64` | 30 |
//! | `base_length` | `base_length`, `trees.h:118` | 29 |
//! | `base_dist` | `base_dist`, `trees.h:123` | 30 |
//! | `_dist_code` | `_dist_code`, `trees.h:73` | 512 |
//! | `_length_code` | `_length_code`, `trees.h:102` | 256 |
//! | `lenfix` | `lenfix`, `inffixed.h:10` | 512 |
//! | `distfix` | `distfix`, `inffixed.h:87` | 32 |
//! | `CONFIGURATION_TABLE` | `configuration_table`, `deflate.c:112` | 10 |
//!
//! No length is hardcoded where the C side owns one: every comparison reads the count from the
//! table's own `sizeof(array) / sizeof(array[0])` accessor and asserts the two lengths agree
//! *before* looking at any element, so a dimension error is reported as a dimension error rather
//! than as an out-of-range panic.
//!
//! # Why the literal anchors are here as well
//!
//! A comparison of the port against the shim can only prove the two agree; it cannot prove they
//! agree with the committed header. `crc32.h` alone holds twelve alternative `crc_braid_table`
//! definitions -- one per `#if N == 1..6` crossed with `#if W == 8` / `#else W == 4` -- and only one
//! arm is ever compiled. A shim that reached the wrong arm, or the big-endian variant, would hand
//! back a table that is perfectly valid and still the wrong answer, and a self-consistent
//! comparison would pass. So each test additionally asserts a short run of values transcribed
//! straight out of the header as *independent* literals. The opening run of `crc_table` is
//! `0x00000000, 0x77073096, 0xee0e612c, 0x990951ba`; a C driver linked against this very oracle was
//! measured printing `crc0=77073096`, which is `crc_table[1]`, so that anchor has been confirmed
//! from outside this test as well.
//!
//! Two tests go further and check *structure* rather than data, using relations that
//! `tr_static_init` (`trees.c:295`) establishes and that hold independently of any transcription:
//! `_length_code` and `_dist_code` must be non-decreasing, and each entry must name a code whose
//! `base_length` / `base_dist` does not exceed the length or distance being encoded.
//!
//! # What is deliberately *not* compared, and why
//!
//! * `CRC_BIG_TABLE` and `CRC_BRAID_BIG_TABLE`. Their C accessors return `*const c_void`, because
//!   `z_word_t` is 64 or 32 bits depending on the build, and `oracle.rs` therefore offers no typed
//!   safe wrapper for either. Reaching them from here would mean writing `unsafe`, which this file
//!   must not do. What would make the comparison direct is a typed safe wrapper in `oracle.rs`; the
//!   byte-swapped relationship between the little- and big-endian tables is meanwhile covered
//!   inside the core by `crates/zlib-rs/tests/crc32.rs`.
//! * `N`, the braid count. `c_oracle_crc_braid_n` likewise has no safe wrapper. The per-width
//!   literal anchors are what catch a wrong-`N` arm instead.
//! * `zlibCompileFlags`. Worth knowing why, because it is adjacent: the port's tables are
//!   `const`-evaluated rather than generated at run time, so it reports bit 13
//!   (`DYNAMIC_CRC_TABLE`) **clear**, which is the honest answer -- and `crc32.c:13` records that
//!   the dynamic path has no mutex guarding table construction, which is exactly the hazard `const`
//!   evaluation removes. Asserting the flag word belongs to
//!   `crates/libz-rs-sys/tests/c_api_parity.rs`, not here.
//!
//! # Contract
//!
//! * **No `unsafe`.** All of this crate's `unsafe` lives in `src/oracle.rs`; this file consumes the
//!   safe `&'static [T]` wrappers built over the `c_oracle_*` accessors. If a comparison seems to
//!   need `unsafe`, a wrapper is being bypassed -- or, as with the big-endian tables above, the
//!   wrapper does not exist and the honest answer is to say so rather than to reach past it.
//! * **No `#[no_mangle]` and no `extern "C"` declarations.** This crate exports no C symbol and
//!   declares its C surface in exactly one place.
//! * **Nothing is loosened.** No prefix-only comparison, no `#[ignore]`, no tolerance. A mismatch
//!   here is a defect in the port, never in the test: the C array is authoritative.
//! * **No I/O.** These tables are compiled into the test binary and into the linked archive. This
//!   file opens no file, reads no environment variable and touches no network.
//! * **Not under Miri, by design.** Miri interprets Rust MIR and cannot execute the compiled C
//!   oracle, so the Miri gate is scoped to `-p zlib-rs`; nightly AddressSanitizer covers this crate
//!   instead. That is a CI job scope, not something to work around here.

// The workspace lint table denies the panic-prone lints, which is right for library code and wrong
// for a test: a test asserts, a failed assertion panics, and reading a table by index is clearer
// than defensively matching on it -- especially here, where every index is either a loop position
// bounded by the table's own length or a fixed offset the surrounding assertion has just checked.
// `clippy.toml` grants `unwrap`/`expect`/`panic` inside `#[test]` context, but there is no
// `allow-indexing-slicing-in-tests` option and the helpers below are file-scope rather than
// `#[test]` functions, so the relaxation is stated once here. Same quartet, same reason, as
// `crates/zlib-rs/tests/crc32.rs` and `crates/zlib-rs/tests/trees.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use core::ffi::{c_int, c_uint};
use core::mem::size_of;

// The ten table re-exports come in through the crate root on purpose. `crates/zlib-rs/src/lib.rs`
// states that it exports them for this file specifically, and this crate's manifest enables
// `zlib-rs`'s `rust-api` feature, so the root path always resolves here even though it does not in
// `cargo test -p zlib-rs`. Everything with no root re-export -- the flush and strategy constants,
// the deflate entry points, `CtData` and `Code` -- comes in by module path.
use zlib_rs::config::{Z_DEFAULT_STRATEGY, Z_NO_FLUSH};
use zlib_rs::deflate::state::CtData;
use zlib_rs::deflate::{deflate, deflate_end, deflate_init2, deflate_params};
use zlib_rs::inflate::inftrees::Code;
use zlib_rs::{
    _dist_code, _length_code, base_dist, base_length, distfix, lenfix, static_dtree, static_ltree,
    DeflateConfig, DeflateStream, GlobalAllocator, Method, ReturnCode, Strategy,
    CONFIGURATION_TABLE, CRC_BRAID_TABLE, CRC_TABLE, X2N_TABLE,
};

// `libz_rs_sys` is deliberately absent. The facade would only add a way to reach the same tables
// through raw `z_stream` pointers, which would require `unsafe`; the safe core exposes the same
// data and the same `deflate_params` behaviour, so the facade is not needed to compare anything
// here. Should a future test in this file need it, the manifest's dependency-rename form fixes the
// spelling as `use libz_rs_sys::...`, never `use z::...`.
use zlib_rs_differential::oracle::{
    code, ct_data, oracle_base_dist, oracle_base_length, oracle_config, oracle_config_field_size,
    oracle_config_table_len, oracle_crc_braid_dimensions, oracle_crc_braid_table, oracle_crc_table,
    oracle_dist_code, oracle_distfix, oracle_element_sizes, oracle_lenfix, oracle_length_code,
    oracle_static_dtree, oracle_static_ltree, oracle_x2n_table, uch, OracleCompressFunc, D_CODES,
    LENGTH_CODES, L_CODES,
};

// =================================================================================================
//  Where each table lives in the C sources
//
//  Every failure message names one of these, so a failing run points straight at the header to
//  re-transcribe instead of at the Rust file that noticed.
// =================================================================================================

/// `local const z_crc_t FAR crc_table[]`.
const CRC_TABLE_SOURCE: &str = "crc32.h:5";

/// `local const z_crc_t FAR x2n_table[]`, which spans `crc32.h:9439-9446`.
const X2N_TABLE_SOURCE: &str = "crc32.h:9439";

/// `local const ct_data static_ltree[L_CODES+2]`.
const STATIC_LTREE_SOURCE: &str = "trees.h:3";

/// `local const ct_data static_dtree[D_CODES]`.
const STATIC_DTREE_SOURCE: &str = "trees.h:64";

/// `const uch ZLIB_INTERNAL _dist_code[DIST_CODE_LEN]`.
const DIST_CODE_SOURCE: &str = "trees.h:73";

/// `const uch ZLIB_INTERNAL _length_code[MAX_MATCH-MIN_MATCH+1]`.
const LENGTH_CODE_SOURCE: &str = "trees.h:102";

/// `local const int base_length[LENGTH_CODES]`.
const BASE_LENGTH_SOURCE: &str = "trees.h:118";

/// `local const int base_dist[D_CODES]`.
const BASE_DIST_SOURCE: &str = "trees.h:123";

/// `static const code lenfix[512]`.
const LENFIX_SOURCE: &str = "inffixed.h:10";

/// `static const code distfix[32]`.
const DISTFIX_SOURCE: &str = "inffixed.h:87";

/// `local const config configuration_table[10]`, the `#else` arm of the pair at `deflate.c:106`.
const CONFIG_TABLE_SOURCE: &str = "deflate.c:112";

/// `trees.c:295`, where `tr_static_init` derives the mappings the structural checks assert.
const TR_STATIC_INIT_SOURCE: &str = "trees.c:295";

// =================================================================================================
//  Literal anchors, transcribed by hand from the committed headers
//
//  These are independent of the shim on purpose: they fail even when the port and the C table agree
//  with each other, which is the only way to catch a shim that reached a valid-but-wrong table.
// =================================================================================================

/// The first five entries of `crc_table` (`crc32.h:6-7`).
const CRC_TABLE_HEAD: [u32; 5] = [
    0x0000_0000,
    0x7707_3096,
    0xee0e_612c,
    0x9909_51ba,
    0x076d_c419,
];

/// The first five entries of `x2n_table` (`crc32.h:9440`): `x^2^n mod p(x)` for the first five `n`.
const X2N_TABLE_HEAD: [u32; 5] = [
    0x4000_0000,
    0x2000_0000,
    0x0800_0000,
    0x0080_0000,
    0x0000_8000,
];

/// The last entry of `x2n_table` (`crc32.h:9446`).
const X2N_TABLE_LAST: u32 = 0xc4e2_2c3c;

/// The first five entries of `crc_braid_table[0]` in the `W == 8` arm (`crc32.h:6366`).
const BRAID_W8_HEAD: [u32; 5] = [
    0x0000_0000,
    0xaf44_9247,
    0x85f8_22cf,
    0x2abc_b088,
    0xd081_43df,
];

/// The final entry of the `W == 8` arm, `crc_braid_table[7][255]` (`crc32.h:6781`).
const BRAID_W8_LAST: u32 = 0xf437_7108;

/// The first five entries of `crc_braid_table[0]` in the `W == 4` arm (`crc32.h:7476`).
const BRAID_W4_HEAD: [u32; 5] = [
    0x0000_0000,
    0x6567_3b46,
    0xcace_768c,
    0xafa9_4dca,
    0x4eed_eb59,
];

/// The final entry of the `W == 4` arm, `crc_braid_table[3][255]` (`crc32.h:7683`).
const BRAID_W4_LAST: u32 = 0x18ba_364e;

/// The first five entries of `static_ltree` (`trees.h:4`), spelled `{{ 12},{  8}}` and so on:
/// `fc` first, which for a built tree is the bit string, then `dl`, the code length.
const STATIC_LTREE_HEAD: [ct_data; 5] = [
    ct_data { fc: 12, dl: 8 },
    ct_data { fc: 140, dl: 8 },
    ct_data { fc: 76, dl: 8 },
    ct_data { fc: 204, dl: 8 },
    ct_data { fc: 44, dl: 8 },
];

/// The first five entries of `static_dtree` (`trees.h:65`); every static distance code is 5 bits.
const STATIC_DTREE_HEAD: [ct_data; 5] = [
    ct_data { fc: 0, dl: 5 },
    ct_data { fc: 16, dl: 5 },
    ct_data { fc: 8, dl: 5 },
    ct_data { fc: 24, dl: 5 },
    ct_data { fc: 4, dl: 5 },
];

/// The first six entries of `base_length` (`trees.h:119`); the first six length codes carry no
/// extra bits, so each maps to exactly one length.
const BASE_LENGTH_HEAD: [c_int; 6] = [0, 1, 2, 3, 4, 5];

/// The first ten entries of `base_dist` (`trees.h:124`).
const BASE_DIST_HEAD: [c_int; 10] = [0, 1, 2, 3, 4, 6, 8, 12, 16, 24];

/// The first twenty entries of `_dist_code` (`trees.h:74`), one full line of the header.
const DIST_CODE_HEAD: [uch; 20] = [0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8];

/// The first twenty entries of `_length_code` (`trees.h:103`), one full line of the header.
const LENGTH_CODE_HEAD: [uch; 20] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12,
];

/// The first four entries of `lenfix` (`inffixed.h:11`).
const LENFIX_HEAD: [code; 4] = [
    code {
        op: 96,
        bits: 7,
        val: 0,
    },
    code {
        op: 0,
        bits: 8,
        val: 80,
    },
    code {
        op: 0,
        bits: 8,
        val: 16,
    },
    code {
        op: 20,
        bits: 8,
        val: 115,
    },
];

/// The last entry of `lenfix` (`inffixed.h:85`).
const LENFIX_LAST: code = code {
    op: 0,
    bits: 9,
    val: 255,
};

/// The first four entries of `distfix` (`inffixed.h:88`).
const DISTFIX_HEAD: [code; 4] = [
    code {
        op: 16,
        bits: 5,
        val: 1,
    },
    code {
        op: 23,
        bits: 5,
        val: 257,
    },
    code {
        op: 19,
        bits: 5,
        val: 17,
    },
    code {
        op: 27,
        bits: 5,
        val: 4097,
    },
];

/// The invalid-code sentinel `{64,5,0}`, which `distfix` carries at both index 15 and index 31.
///
/// `op` bit 6 is `inftrees.h`'s "invalid code" flag, and `inffast.c` tests `op & 64` to reject the
/// two distance codes RFC 1951 leaves unused.
const INVALID_DISTANCE_CODE: code = code {
    op: 64,
    bits: 5,
    val: 0,
};

// =================================================================================================
//  Dimensions the C sources fix in files that declare no accessor
//
//  Preferred order is always: read the length from the C side. These two exist because
//  `DIST_CODE_LEN` is a `trees.c`-local macro and `MAX_MATCH`/`MIN_MATCH` are `deflate.h` macros --
//  neither has a symbol -- so the pointer accessors' companion lengths are checked against them
//  rather than the other way round.
// =================================================================================================

/// `DIST_CODE_LEN` (`trees.c:81`): the element count of `_dist_code`.
const DIST_CODE_LEN: usize = 512;

/// `MAX_MATCH - MIN_MATCH + 1` (`deflate.h`): the element count of `_length_code`.
const LENGTH_CODE_LEN: usize = 256;

/// The column count of every `crc_braid_table` arm: one entry per byte value.
const BRAID_COLUMNS: usize = 256;

/// The row count of `configuration_table` in the default build (`deflate.c:112`).
///
/// Two under `FASTEST` (`deflate.c:107`), which the oracle deliberately does not define -- so any
/// other value means the oracle is a *different encoder* and comparing bytes against it would be
/// meaningless.
const CONFIG_TABLE_ROWS: usize = 10;

// =================================================================================================
//  Diagnostics
//
//  A bare `assert_eq!` on two nine-thousand-element slices prints both of them and tells the reader
//  nothing, so every comparison goes through the two helpers below. They report the first differing
//  index, both values at it, the total number of differing entries -- which is what distinguishes a
//  single typo from an entirely wrong table -- and the C source location to re-transcribe from.
// =================================================================================================

/// Renders a CRC-32 word the way `crc32.h` writes it.
///
/// The base matters. These tables are eight-digit hexadecimal in the header, and a value reported in
/// decimal cannot be compared against them by eye.
fn as_hex_word(value: u32) -> String {
    format!("0x{value:08x}")
}

/// Renders a C `ct_data` entry as a built tree node: the bit string, then its length.
///
/// Both members of both unions are `ush`, and for a *static* tree the live ones are C's `Code` and
/// `Len` macros -- never `Freq` or `Dad`, which belong to a tree still being built.
fn as_tree_node(value: ct_data) -> String {
    format!("code={} len={}", value.fc, value.dl)
}

/// Renders the port's tree node, whose halves sit behind accessors rather than public fields.
fn as_port_tree_node(value: CtData) -> String {
    format!("code={} len={}", value.code(), value.len())
}

/// Renders a C `code` entry in `inffixed.h`'s field order.
fn as_decode_entry(value: code) -> String {
    format!("op={} bits={} val={}", value.op, value.bits, value.val)
}

/// Renders the port's decoding-table entry, in the same order.
fn as_port_decode_entry(value: Code) -> String {
    format!("op={} bits={} val={}", value.op, value.bits, value.val)
}

/// Renders an entry of one of the two `int` tables, which `trees.h` writes in decimal.
fn as_int(value: c_int) -> String {
    format!("{value}")
}

/// Renders an entry of one of the two code mappings, which `trees.h` writes in decimal.
fn as_code_index(value: uch) -> String {
    format!("{value}")
}

/// Compares the port's transcription of a generated table against the C array, element for element.
///
/// `same` decides equality across the two element types, which are never the same Rust type: the
/// port's `CtData` hides its halves behind accessors, its `Code` is not `#[repr(C)]`, and the
/// integer tables are `i32` against `c_int`. `show` renders one differing pair and is called only on
/// failure, so it is free to allocate and to choose a readable base.
///
/// Lengths are checked first and separately. That ordering is the point: a dimension error then
/// reports as a dimension error, rather than as an index-out-of-range panic from whichever loop
/// happened to run off the end first.
fn assert_tables_match<P: Copy, C: Copy>(
    table: &str,
    source: &str,
    port: &[P],
    reference: &[C],
    same: impl Fn(P, C) -> bool,
    show: impl Fn(P, C) -> String,
) {
    assert_eq!(
        port.len(),
        reference.len(),
        "{table}: LENGTH mismatch. The port declares {} entries; the C array at {source} has {}. \
         Correct the port's dimension first -- an element-for-element comparison of tables of \
         different length is meaningless.",
        port.len(),
        reference.len(),
    );

    let mut first_difference = None;
    let mut differing = 0_usize;
    for (index, (mine, theirs)) in port
        .iter()
        .copied()
        .zip(reference.iter().copied())
        .enumerate()
    {
        if !same(mine, theirs) {
            differing += 1;
            if first_difference.is_none() {
                first_difference = Some(index);
            }
        }
    }

    if let Some(index) = first_difference {
        panic!(
            "{table}: {differing} of {total} entries differ from the C array at {source}.\n  \
             first difference at index {index}: {rendered}\n  \
             The C array is authoritative; re-transcribe the affected entry from {source}. A single \
             differing entry is usually a typo; a large fraction of them usually means the wrong \
             table, or the wrong conditional arm of it, was transcribed.",
            total = port.len(),
            rendered = show(port[index], reference[index]),
        );
    }
}

/// Asserts that `actual` opens with exactly `expected`, as an independent check against the header.
///
/// This is not a weakened form of [`assert_tables_match`] and never replaces it. It compares against
/// literals typed out of the committed header, so it still fails when the port and the shim agree
/// with each other but both disagree with the file on disk -- which is precisely the failure a
/// self-consistent comparison cannot see.
///
/// `render` chooses the base the two values are reported in, for the same reason
/// [`assert_tables_match`] takes a `show`: a CRC word read out in decimal cannot be compared against
/// a header full of hexadecimal by eye, and a Huffman code read out in hexadecimal cannot be compared
/// against `trees.h` either. It is called only on failure.
fn assert_opening_run<T: Copy + PartialEq>(
    table: &str,
    source: &str,
    actual: &[T],
    expected: &[T],
    render: impl Fn(T) -> String,
) {
    assert!(
        actual.len() >= expected.len(),
        "{table}: has only {} entries, so the {}-entry opening run transcribed from {source} cannot \
         be checked at all.",
        actual.len(),
        expected.len(),
    );

    for (index, &want) in expected.iter().enumerate() {
        let got = actual[index];
        assert!(
            got == want,
            "{table}: entry {index} is {found}, but the literal transcribed from {source} is \
             {wanted}. This anchor is independent of the C shim, so it fails even when the port and \
             the shim agree with each other.",
            found = render(got),
            wanted = render(want),
        );
    }
}

/// Asserts that a code mapping never steps backwards, over the half of it starting at `offset`.
///
/// `tr_static_init` fills `_length_code` and both halves of `_dist_code` by walking the codes in
/// increasing order and writing each one across its own run of values, so the result is
/// non-decreasing by construction. Checking it needs neither the shim nor the header, which is what
/// makes it worth checking.
fn assert_non_decreasing(table: &str, source: &str, values: &[uch], offset: usize) {
    for (step, pair) in values.windows(2).enumerate() {
        assert!(
            pair[0] <= pair[1],
            "{table}: entry {} is code {} but entry {} is code {}, which steps backwards. \
             {source} fills each code across one contiguous run, in increasing order, so the \
             mapping is non-decreasing.",
            offset + step,
            pair[0],
            offset + step + 1,
            pair[1],
        );
    }
}

// =================================================================================================
//  The braided-CRC arms
// =================================================================================================

/// The literal anchors and the header location for whichever `crc_braid_table` arm has `width` rows.
///
/// `crc32.c:87-106` admits exactly two widths -- 8 where a 64-bit `z_word_t` is available and 4
/// otherwise -- and `crc32.h` holds a separate table for each, inside the `#if N == 5` block at
/// `crc32.h:6361`. Neither side may hardcode the width: it has to be read from whichever
/// implementation is being anchored, or the anchor would pin a table that was never compiled.
fn braid_arm(width: usize) -> (&'static [u32], u32, &'static str) {
    match width {
        8 => (
            BRAID_W8_HEAD.as_slice(),
            BRAID_W8_LAST,
            "crc32.h:6365 (#if N == 5, #if W == 8)",
        ),
        4 => (
            BRAID_W4_HEAD.as_slice(),
            BRAID_W4_LAST,
            "crc32.h:7475 (#if N == 5, #else W == 4)",
        ),
        other => panic!(
            "crc_braid_table: a width of {other} rows is neither arm crc32.c can compile -- \
             crc32.c:87-106 gives W == 8 with a 64-bit z_word_t and W == 4 without one -- so \
             crc32.h holds no committed table to anchor against."
        ),
    }
}

// =================================================================================================
//  Observing the port's compressor grouping
//
//  `configuration_table`'s fifth member is a function pointer, and the port's counterpart --
//  `Config::func`, a `CompressFunc` discriminant -- is `pub(crate)`, deliberately: promoting it
//  would export a type `zlib.map` does not mention. So the four numeric fields are compared
//  directly and the fifth is compared as the only thing about it that has cross-implementation
//  meaning, namely which levels it groups together.
//
//  That grouping is observable, exactly once, through `deflate_params`. `deflate.c:791` decides
//  whether a level change must first close the open block by comparing the two levels' `func`
//  *addresses*; the port compares discriminants at the same point. Both then flush with
//  `deflate(strm, Z_BLOCK)` and, if anything is left unwritten, answer `Z_BUF_ERROR` and change
//  nothing (`deflate.c:797`). Offer no output room at all and that flush cannot write a byte, so the
//  answer is `BUF_ERROR` when the two levels name different compressors and `OK` when they name the
//  same one -- a clean binary read-out of the equivalence relation, with no bytes to interpret.
// =================================================================================================

/// The payload the probe primes a stream with before changing its level.
///
/// Four bytes is enough and the shortest thing that is. `deflate_params` only consults the compressor
/// when `last_flush != -2`, i.e. when `deflate` has already been called since the reset
/// (`deflate.c:791`), and it only reports `BUF_ERROR` when the flush leaves something behind
/// (`deflate.c:797`). One `Z_NO_FLUSH` call over four bytes satisfies both for every level: levels 1
/// to 9 return `need_more` with the bytes still in the window, and level 0's `deflate_stored` breaks
/// out of its copy loop precisely because the flush mode is `Z_NO_FLUSH`, then fills the window too.
const PRIMING_INPUT: &[u8] = b"abcd";

/// Output room for the priming call: enough that it is never what limits the call.
///
/// The call emits only the two-byte zlib header, so this is far more than required; being generous
/// is what keeps the probe measuring the compressor comparison and nothing else.
const PRIMING_ROOM: usize = 4096;

/// Returns whether the port's table puts levels `from` and `to` in different compressor groups.
///
/// See the section comment above for why `deflate_params` answers this. Everything here runs on the
/// safe core's typed API, so the observation costs no `unsafe`.
fn port_changes_compressor(from: i32, to: i32) -> bool {
    let config = DeflateConfig {
        level: from,
        method: Method::Deflated,
        window_bits: 15,
        mem_level: 8,
        strategy: Strategy::Default,
    };
    let mut state = deflate_init2(config, GlobalAllocator)
        .expect("deflate_init2 with the reference defaults and a level in 0..=9");

    // 1. Prime the stream, so that `last_flush != -2` and the window holds unwritten bytes.
    let mut room = [0_u8; PRIMING_ROOM];
    let mut stream = DeflateStream::new(PRIMING_INPUT, &mut room);
    let primed = deflate(&mut state, &mut stream, Z_NO_FLUSH);
    assert_eq!(
        primed,
        ReturnCode::OK,
        "the probe's priming deflate at level {from} must succeed before the level is changed",
    );

    // 2. Change the level with no output room, holding the strategy fixed so that only the
    //    compressor comparison at `deflate.c:791` can take the flush branch.
    let mut no_room: [u8; 0] = [];
    let mut stream = DeflateStream::new(&[], &mut no_room);
    let changed = deflate_params(&mut state, &mut stream, to, Z_DEFAULT_STRATEGY);

    // 3. Release the window and the hash chains. `DATA_ERROR` is the expected answer, not a
    //    failure: `zlib.h` documents it for a stream freed mid-block, and the probe always frees one.
    let ended = deflate_end(&mut state);
    assert!(
        ended == ReturnCode::OK || ended == ReturnCode::DATA_ERROR,
        "deflate_end after the {from} -> {to} probe returned {ended:?}; zlib.h admits only Z_OK \
         and Z_DATA_ERROR here, so anything else means the probe corrupted the state",
    );

    if changed == ReturnCode::BUF_ERROR {
        true
    } else if changed == ReturnCode::OK {
        false
    } else {
        panic!(
            "deflate_params({from} -> {to}) returned {changed:?}. With a valid state, a level in \
             0..=9 and an unchanged strategy, deflate.c:791-799 admits only Z_OK (no flush needed) \
             or Z_BUF_ERROR (flush needed but no output room), so the probe can no longer tell the \
             two compressor groups apart."
        );
    }
}

/// The `configuration_table` row indices, as both integer types the comparisons need.
///
/// `oracle_config` takes a `c_uint` and `deflate_params` takes an `i32`, and converting between them
/// with `as` would trip the workspace's cast lints for no benefit, so the conversion is done once,
/// fallibly, here.
fn levels(rows: c_uint) -> impl Iterator<Item = (c_uint, i32)> {
    (0..rows).map(|level| {
        let as_level = i32::try_from(level)
            .expect("a configuration_table row index is at most 9 and so fits in an i32");
        (level, as_level)
    })
}

// =================================================================================================
//  crc32.h
// =================================================================================================

/// `CRC_TABLE` is `crc_table` (`crc32.h:5`), all 256 entries.
///
/// The byte-at-a-time CRC-32 table, read by `crc32_z` for every input shorter than the braided
/// threshold and for the head and tail of every longer one, so it participates in the CRC of
/// essentially every stream this library produces or checks.
#[test]
fn crc_table_matches_the_c_reference() {
    let reference = oracle_crc_table();

    assert_eq!(
        reference.len(),
        256,
        "crc_table: {CRC_TABLE_SOURCE} declares one entry per byte value",
    );
    assert_opening_run(
        "crc_table",
        CRC_TABLE_SOURCE,
        reference,
        &CRC_TABLE_HEAD,
        as_hex_word,
    );

    assert_tables_match(
        "crc_table",
        CRC_TABLE_SOURCE,
        &CRC_TABLE,
        reference,
        |mine, theirs| mine == theirs,
        |mine, theirs| format!("port {}, C {}", as_hex_word(mine), as_hex_word(theirs)),
    );
}

/// `X2N_TABLE` is `x2n_table` (`crc32.h:9439`), all 32 entries.
///
/// `x2n_table[n]` is `x^2^n mod p(x)`. `crc32_combine_gen` and `crc32_combine_op` are built entirely
/// out of it, so the whole combine family's correctness rests on these 32 words.
#[test]
fn x2n_table_matches_the_c_reference() {
    let reference = oracle_x2n_table();

    assert_eq!(
        reference.len(),
        32,
        "x2n_table: {X2N_TABLE_SOURCE} runs to crc32.h:9446 and holds 32 entries",
    );
    assert_opening_run(
        "x2n_table",
        X2N_TABLE_SOURCE,
        reference,
        &X2N_TABLE_HEAD,
        as_hex_word,
    );
    assert_eq!(
        reference[31], X2N_TABLE_LAST,
        "x2n_table: the final entry is the last value on crc32.h:9446",
    );

    assert_tables_match(
        "x2n_table",
        X2N_TABLE_SOURCE,
        &X2N_TABLE,
        reference,
        |mine, theirs| mine == theirs,
        |mine, theirs| format!("port {}, C {}", as_hex_word(mine), as_hex_word(theirs)),
    );
}

/// `CRC_BRAID_TABLE` is `crc_braid_table` (`crc32.h:6365` or `crc32.h:7475`), all `W` x 256 entries.
///
/// The word-at-a-time tables the braided CRC body reads, `W` rows of 256. Both dimensions are read
/// from the C side rather than assumed, because `crc32.h` holds twelve alternative definitions --
/// `#if N == 1..6` crossed with `#if W == 8` / `#else W == 4` -- and only one is compiled.
///
/// # The one case where the two implementations legitimately differ
///
/// The port selects its arm on `target_pointer_width = "64"`; `crc32.c:87-95` selects `W` on
/// `defined(__x86_64__) || defined(__aarch64__)`. The two rules agree on `x86_64`, on `aarch64` and
/// on every 32-bit target, and `crates/zlib-rs/src/crc32/tables.rs:125` records that they disagree
/// on a 64-bit target outside C's list -- `riscv64`, `powerpc64`, `s390x` -- where C takes `W == 4`
/// and the port takes `W == 8`. That divergence is output-neutral, because the braid width is an
/// evaluation strategy and not part of the CRC's definition, but it means the two builds hold
/// genuinely different tables and there is no C-side data for the port's arm to be compared against.
/// So the widths are compared, each side is anchored against the header arm its own width selects,
/// and only a disagreement in width suppresses the content comparison -- loudly, and never silently.
#[test]
fn crc_braid_table_matches_the_c_reference() {
    let (rows, columns) = oracle_crc_braid_dimensions();
    let reference = oracle_crc_braid_table();
    let c_rows = rows as usize;
    let c_columns = columns as usize;

    if c_rows == 0 {
        assert!(
            reference.is_empty(),
            "crc_braid_table: the C side reports no rows, so its table view must be empty, yet it \
             handed back {} entries",
            reference.len(),
        );
        println!(
            "crc_braid_table: COMPARISON SKIPPED. The C oracle reports a width of 0, which means \
             crc32.h compiled the braided path out entirely on this target (crc32.c:96-106 leaves \
             W undefined when neither Z_U8 nor Z_U4 is available), so there is no C array to \
             compare the port's {} x {} table against.",
            CRC_BRAID_TABLE.len(),
            BRAID_COLUMNS,
        );
        return;
    }

    // The C side's geometry, and the header arm its own width selects.
    assert_eq!(
        c_columns, BRAID_COLUMNS,
        "crc_braid_table: crc32.c:205 declares crc_braid_table[W][256], so the C side must report \
         256 columns",
    );
    assert_eq!(
        reference.len(),
        c_rows * c_columns,
        "crc_braid_table: the flattened C view must hold exactly rows x columns entries",
    );
    let (c_head, c_last, c_source) = braid_arm(c_rows);
    assert_opening_run(
        "crc_braid_table (C)",
        c_source,
        reference,
        c_head,
        as_hex_word,
    );
    assert_eq!(
        reference[reference.len() - 1],
        c_last,
        "crc_braid_table (C): the final entry disagrees with {c_source}, so the shim reached a \
         table crc32.c did not compile -- a different N arm, or the big-endian variant",
    );

    // The port's geometry, anchored the same way against the arm its own width selects.
    let port_rows = CRC_BRAID_TABLE.len();
    assert_eq!(
        CRC_BRAID_TABLE[0].len(),
        BRAID_COLUMNS,
        "crc_braid_table (port): each row must hold one entry per byte value",
    );
    let (port_head, port_last, port_source) = braid_arm(port_rows);
    assert_opening_run(
        "crc_braid_table (port)",
        port_source,
        &CRC_BRAID_TABLE[0],
        port_head,
        as_hex_word,
    );
    assert_eq!(
        CRC_BRAID_TABLE[port_rows - 1][BRAID_COLUMNS - 1],
        port_last,
        "crc_braid_table (port): the final entry disagrees with {port_source}",
    );

    if port_rows != c_rows {
        println!(
            "crc_braid_table: CONTENT COMPARISON SKIPPED. The port compiled the {port_rows}-row arm \
             and the C reference the {c_rows}-row one, which is the documented divergence recorded \
             at crates/zlib-rs/src/crc32/tables.rs:125: the port keys W off \
             target_pointer_width == 64 and crc32.c:87-95 keys it off __x86_64__/__aarch64__. Both \
             arms were still checked against their own crc32.h literals above. The divergence is \
             output-neutral -- braid width changes only how many bytes one step consumes -- so there \
             is nothing to compare rather than something being skipped."
        );
        return;
    }

    // A C array is contiguous and row-major, so the port's rows flatten to the same order the C
    // view already has: `crc_braid_table[k][b]` is `flat[k * columns + b]` on both sides.
    let port: Vec<u32> = CRC_BRAID_TABLE.iter().flatten().copied().collect();
    assert_tables_match(
        "crc_braid_table",
        c_source,
        &port,
        reference,
        |mine, theirs| mine == theirs,
        |mine, theirs| format!("port {}, C {}", as_hex_word(mine), as_hex_word(theirs)),
    );
}

// =================================================================================================
//  trees.h
// =================================================================================================

/// `static_ltree` is `static_ltree` (`trees.h:3`), all `L_CODES + 2` = 288 entries.
///
/// The static literal/length tree, emitted for every block `_tr_flush_block` decides is no larger
/// static than dynamic. Every static block in the output stream is encoded with it, so a wrong entry
/// changes emitted bytes directly rather than merely making the encoding worse.
///
/// This test also carries the `ct_data` layout cross-check, because this is the first table whose
/// element type is a struct: `oracle.rs` mirrors C's pair of single-typed unions as a pair of `u16`
/// fields, and `c_oracle_ct_data_size` reports what the C compiler actually produced so the mirror
/// is checked rather than assumed.
#[test]
fn static_ltree_matches_the_c_reference() {
    let (c_ct_data, _) = oracle_element_sizes();
    assert_eq!(
        c_ct_data as usize,
        size_of::<ct_data>(),
        "ct_data: the harness's mirror must be the size the C compiler gave deflate.h:71-86",
    );
    assert_eq!(
        size_of::<CtData>(),
        size_of::<ct_data>(),
        "CtData: the port's tree node must be the same four bytes as C's ct_data",
    );

    let reference = oracle_static_ltree();
    assert_eq!(
        reference.len(),
        (L_CODES + 2) as usize,
        "static_ltree: {STATIC_LTREE_SOURCE} dimensions it as L_CODES + 2",
    );
    assert_opening_run(
        "static_ltree",
        STATIC_LTREE_SOURCE,
        reference,
        &STATIC_LTREE_HEAD,
        as_tree_node,
    );

    assert_tables_match(
        "static_ltree",
        STATIC_LTREE_SOURCE,
        &static_ltree,
        reference,
        |mine, theirs| mine.code() == theirs.fc && mine.len() == theirs.dl,
        |mine, theirs| {
            format!(
                "port {}, C {}",
                as_port_tree_node(mine),
                as_tree_node(theirs)
            )
        },
    );
}

/// `static_dtree` is `static_dtree` (`trees.h:64`), all `D_CODES` = 30 entries.
///
/// The static distance tree. Every code in it is 5 bits, which is what makes a static block's
/// distance encoding fixed-width, so an error here shifts every subsequent bit of such a block.
#[test]
fn static_dtree_matches_the_c_reference() {
    let reference = oracle_static_dtree();

    assert_eq!(
        reference.len(),
        D_CODES as usize,
        "static_dtree: {STATIC_DTREE_SOURCE} dimensions it as D_CODES",
    );
    assert_opening_run(
        "static_dtree",
        STATIC_DTREE_SOURCE,
        reference,
        &STATIC_DTREE_HEAD,
        as_tree_node,
    );

    assert_tables_match(
        "static_dtree",
        STATIC_DTREE_SOURCE,
        &static_dtree,
        reference,
        |mine, theirs| mine.code() == theirs.fc && mine.len() == theirs.dl,
        |mine, theirs| {
            format!(
                "port {}, C {}",
                as_port_tree_node(mine),
                as_tree_node(theirs)
            )
        },
    );
}

/// `base_length` is `base_length` (`trees.h:118`), all `LENGTH_CODES` = 29 entries.
///
/// The first match length each length code covers. `compress_block` subtracts it from the actual
/// length to obtain the extra bits it then emits, so these values are part of the bitstream and not
/// merely part of the encoder's bookkeeping.
#[test]
fn base_length_matches_the_c_reference() {
    let reference = oracle_base_length();

    assert_eq!(
        reference.len(),
        LENGTH_CODES as usize,
        "base_length: {BASE_LENGTH_SOURCE} dimensions it as LENGTH_CODES",
    );
    assert_opening_run(
        "base_length",
        BASE_LENGTH_SOURCE,
        reference,
        &BASE_LENGTH_HEAD,
        as_int,
    );
    assert_eq!(
        reference[LENGTH_CODES as usize - 1],
        0,
        "base_length: the final entry is 0, not 256. Match length 258 has two encodings -- code 284 \
         plus 5 extra bits, or code 285 -- and {TR_STATIC_INIT_SOURCE} leaves the last slot at 0 \
         because code 285 carries no extra bits at all",
    );

    assert_tables_match(
        "base_length",
        BASE_LENGTH_SOURCE,
        &base_length,
        reference,
        |mine, theirs| i64::from(mine) == i64::from(theirs),
        |mine, theirs| format!("port {}, C {}", as_int(mine), as_int(theirs)),
    );
}

/// `base_dist` is `base_dist` (`trees.h:123`), all `D_CODES` = 30 entries.
///
/// The first match distance each distance code covers, the distance-side counterpart of
/// `base_length` and read at the same point in `compress_block`.
#[test]
fn base_dist_matches_the_c_reference() {
    let reference = oracle_base_dist();

    assert_eq!(
        reference.len(),
        D_CODES as usize,
        "base_dist: {BASE_DIST_SOURCE} dimensions it as D_CODES",
    );
    assert_opening_run(
        "base_dist",
        BASE_DIST_SOURCE,
        reference,
        &BASE_DIST_HEAD,
        as_int,
    );

    assert_tables_match(
        "base_dist",
        BASE_DIST_SOURCE,
        &base_dist,
        reference,
        |mine, theirs| i64::from(mine) == i64::from(theirs),
        |mine, theirs| format!("port {}, C {}", as_int(mine), as_int(theirs)),
    );
}

/// `_dist_code` is `_dist_code` (`trees.h:73`), all `DIST_CODE_LEN` = 512 entries.
///
/// Maps a match distance to its distance code, in two halves: index `d` for the distances `0..256`,
/// and index `256 + j` for the distance `j << 7` above that -- "from now on, all distances are
/// divided by 128", as `tr_static_init` puts it.
///
/// Unlike the nine `local` tables, this one and `_length_code` are `ZLIB_INTERNAL` and so do have
/// linkable symbols; the shim still routes them through an accessor, which is what lets its own view
/// of the array be renamed away from the definition in `trees.o`. Either way the comparison below is
/// direct and element for element.
#[test]
fn dist_code_matches_the_c_reference() {
    let reference = oracle_dist_code();

    assert_eq!(
        reference.len(),
        DIST_CODE_LEN,
        "_dist_code: {DIST_CODE_SOURCE} dimensions it as DIST_CODE_LEN, which trees.c:81 fixes at \
         512 -- 256 distances plus 256 more in units of 128",
    );
    assert_opening_run(
        "_dist_code",
        DIST_CODE_SOURCE,
        reference,
        &DIST_CODE_HEAD,
        as_code_index,
    );
    assert_eq!(
        reference[DIST_CODE_LEN - 1],
        29,
        "_dist_code: the final entry is distance code 29, the last of D_CODES",
    );

    assert_tables_match(
        "_dist_code",
        DIST_CODE_SOURCE,
        &_dist_code,
        reference,
        |mine, theirs| mine == theirs,
        |mine, theirs| {
            format!(
                "port code {}, C code {}",
                as_code_index(mine),
                as_code_index(theirs)
            )
        },
    );

    // Independent structural cross-check, needing neither the shim nor the header: every entry must
    // name a code whose first distance does not exceed the distance being encoded, and the codes
    // must not step backwards. `tr_static_init` builds both halves that way by construction.
    let bases = oracle_base_dist();
    assert_non_decreasing(
        "_dist_code (low half)",
        TR_STATIC_INIT_SOURCE,
        &_dist_code[..256],
        0,
    );
    assert_non_decreasing(
        "_dist_code (high half)",
        TR_STATIC_INIT_SOURCE,
        &_dist_code[256..],
        256,
    );

    for (distance, &dist_code) in _dist_code[..256].iter().enumerate() {
        let first = usize::try_from(bases[usize::from(dist_code)])
            .expect("every base_dist entry is non-negative");
        assert!(
            first <= distance,
            "_dist_code[{distance}] names code {dist_code}, whose base_dist is {first} -- but a \
             code cannot start above the distance it encodes ({TR_STATIC_INIT_SOURCE})",
        );
    }

    for (step, &dist_code) in _dist_code[256..].iter().enumerate() {
        let distance = step << 7;
        let first = usize::try_from(bases[usize::from(dist_code)])
            .expect("every base_dist entry is non-negative");
        assert!(
            first <= distance,
            "_dist_code[{}] covers distance {distance} and names code {dist_code}, whose base_dist \
             is {first} -- but a code cannot start above the distance it encodes. \
             {TR_STATIC_INIT_SOURCE} fills this half in units of 128",
            256 + step,
        );
    }
}

/// `_length_code` is `_length_code` (`trees.h:102`), all `MAX_MATCH - MIN_MATCH + 1` = 256 entries.
///
/// Maps a match length, as an offset from `MIN_MATCH`, to its length code. The companion of
/// `_dist_code` and read alongside it by `_tr_tally_dist`.
#[test]
fn length_code_matches_the_c_reference() {
    let reference = oracle_length_code();

    assert_eq!(
        reference.len(),
        LENGTH_CODE_LEN,
        "_length_code: {LENGTH_CODE_SOURCE} dimensions it as MAX_MATCH - MIN_MATCH + 1 = 256",
    );
    assert_opening_run(
        "_length_code",
        LENGTH_CODE_SOURCE,
        reference,
        &LENGTH_CODE_HEAD,
        as_code_index,
    );
    assert_eq!(
        reference[LENGTH_CODE_LEN - 1],
        28,
        "_length_code: the final entry is 28. {TR_STATIC_INIT_SOURCE} fills the array with codes 0 \
         to 27 and then overwrites the last slot, because match length 258 is cheaper as code 285 \
         than as code 284 plus 5 extra bits",
    );

    assert_tables_match(
        "_length_code",
        LENGTH_CODE_SOURCE,
        &_length_code,
        reference,
        |mine, theirs| mine == theirs,
        |mine, theirs| {
            format!(
                "port code {}, C code {}",
                as_code_index(mine),
                as_code_index(theirs)
            )
        },
    );

    // The same independent structural cross-check as `_dist_code`, over the single range this table
    // has. It holds at the overwritten last slot too, because base_length's last entry is 0.
    let bases = oracle_base_length();
    assert_non_decreasing("_length_code", TR_STATIC_INIT_SOURCE, &_length_code, 0);

    for (length, &length_code) in _length_code.iter().enumerate() {
        let first = usize::try_from(bases[usize::from(length_code)])
            .expect("every base_length entry is non-negative");
        assert!(
            first <= length,
            "_length_code[{length}] names code {length_code}, whose base_length is {first} -- but a \
             code cannot start above the length it encodes ({TR_STATIC_INIT_SOURCE})",
        );
    }
}

// =================================================================================================
//  inffixed.h
// =================================================================================================

/// `lenfix` is `lenfix` (`inffixed.h:10`), all 512 entries.
///
/// The fixed literal/length decoding table for blocks encoded with RFC 1951 section 3.2.6's fixed
/// Huffman codes. `inflate` installs it directly rather than building it, so an error here decodes
/// every fixed block wrongly -- and fixed blocks are what a small or incompressible input produces.
///
/// This test also carries the `code` layout cross-check, for the same reason `static_ltree` carries
/// `ct_data`'s: `struct inflate_state` budgets `4 * ENOUGH` bytes for its decoding arena, and
/// `test/infcover.c` accounts for that budget when it limits allocation, so the four-byte element is
/// load-bearing beyond this table.
#[test]
fn lenfix_matches_the_c_reference() {
    let (_, c_code) = oracle_element_sizes();
    assert_eq!(
        c_code as usize,
        size_of::<code>(),
        "code: the harness's mirror must be the size the C compiler gave inftrees.h:24-28",
    );
    assert_eq!(
        size_of::<Code>(),
        size_of::<code>(),
        "Code: the port's decoding-table entry must be the same four bytes as C's code",
    );

    let reference = oracle_lenfix();
    assert_eq!(
        reference.len(),
        512,
        "lenfix: {LENFIX_SOURCE} declares 512 entries",
    );
    assert_opening_run(
        "lenfix",
        LENFIX_SOURCE,
        reference,
        &LENFIX_HEAD,
        as_decode_entry,
    );
    assert_eq!(
        reference[511], LENFIX_LAST,
        "lenfix: the final entry is the last one printed at inffixed.h:85",
    );

    assert_tables_match(
        "lenfix",
        LENFIX_SOURCE,
        &lenfix,
        reference,
        |mine, theirs| mine.op == theirs.op && mine.bits == theirs.bits && mine.val == theirs.val,
        |mine, theirs| {
            format!(
                "port {}, C {}",
                as_port_decode_entry(mine),
                as_decode_entry(theirs)
            )
        },
    );
}

/// `distfix` is `distfix` (`inffixed.h:87`), all 32 entries.
///
/// The fixed distance decoding table. Two of its 32 slots are the invalid-code sentinel `{64,5,0}`,
/// because RFC 1951 leaves distance codes 30 and 31 unused: the fixed code space has 32 five-bit
/// slots and only 30 legal codes. Those two entries are what makes `inflate` reject a stream that
/// uses them, so they are asserted by position as well as by comparison.
#[test]
fn distfix_matches_the_c_reference() {
    let reference = oracle_distfix();

    assert_eq!(
        reference.len(),
        32,
        "distfix: {DISTFIX_SOURCE} declares 32 entries -- 30 legal distance codes and 2 invalid \
         slots",
    );
    assert_opening_run(
        "distfix",
        DISTFIX_SOURCE,
        reference,
        &DISTFIX_HEAD,
        as_decode_entry,
    );
    assert_eq!(
        reference[15], INVALID_DISTANCE_CODE,
        "distfix: entry 15 must be the invalid-code sentinel {{64,5,0}} (inffixed.h:91)",
    );
    assert_eq!(
        reference[31], INVALID_DISTANCE_CODE,
        "distfix: entry 31 must be the invalid-code sentinel {{64,5,0}} (inffixed.h:93)",
    );

    assert_tables_match(
        "distfix",
        DISTFIX_SOURCE,
        &distfix,
        reference,
        |mine, theirs| mine.op == theirs.op && mine.bits == theirs.bits && mine.val == theirs.val,
        |mine, theirs| {
            format!(
                "port {}, C {}",
                as_port_decode_entry(mine),
                as_decode_entry(theirs)
            )
        },
    );
}

// =================================================================================================
//  deflate.c
// =================================================================================================

/// `CONFIGURATION_TABLE` is `configuration_table` (`deflate.c:112`), all ten rows.
///
/// The first of the byte-identity decision points. Its four numbers per level decide which candidate
/// matches `longest_match` examines and accepts, and therefore which literal, length and distance
/// symbols reach the Huffman coder; a single altered digit does not make the encoder better or worse
/// in any way a decompressor could detect, it makes it emit different bytes at that level for
/// essentially every input.
///
/// The four numeric fields are compared directly against the C object -- the shim reads them from
/// `deflate.c` itself rather than restating them, so nothing here is one transcription checked
/// against another. The fifth member is a function pointer, whose *value* means nothing across two
/// implementations; what both sides must reproduce is the grouping it induces, and that is compared
/// two ways: as an independent literal expectation on the C column, and behaviourally on the port
/// through `deflate_params`, which is the one place the grouping is observable. See the section
/// comment on `port_changes_compressor` for why that read-out is exact.
#[test]
fn configuration_table_matches_the_c_reference() {
    let rows = oracle_config_table_len();

    assert_eq!(
        rows as usize,
        CONFIG_TABLE_ROWS,
        "configuration_table: the C oracle reports {rows} rows. {CONFIG_TABLE_SOURCE} declares ten; \
         two would mean the oracle was built with FASTEST (deflate.c:107), which is a different \
         encoder, and comparing bytes against it would be meaningless",
    );
    assert_eq!(
        CONFIGURATION_TABLE.len(),
        rows as usize,
        "configuration_table: the port's table must have exactly the C table's row count",
    );
    assert_eq!(
        oracle_config_field_size() as usize,
        size_of::<u16>(),
        "configuration_table: C declares all four numeric fields as ush (deflate.c:99-102), so the \
         port's u16 fields must be the same width",
    );

    // The four numeric fields, level by level, read out of the C object.
    for (level, index) in levels(rows) {
        let reference = oracle_config(level)
            .expect("the level is below the row count the C side itself just reported");
        let port = CONFIGURATION_TABLE[level as usize];

        assert_eq!(
            c_uint::from(port.good_length),
            reference.good_length,
            "configuration_table[{index}].good_length disagrees with {CONFIG_TABLE_SOURCE}",
        );
        assert_eq!(
            c_uint::from(port.max_lazy),
            reference.max_lazy,
            "configuration_table[{index}].max_lazy disagrees with {CONFIG_TABLE_SOURCE}",
        );
        assert_eq!(
            c_uint::from(port.nice_length),
            reference.nice_length,
            "configuration_table[{index}].nice_length disagrees with {CONFIG_TABLE_SOURCE}",
        );
        assert_eq!(
            c_uint::from(port.max_chain),
            reference.max_chain,
            "configuration_table[{index}].max_chain disagrees with {CONFIG_TABLE_SOURCE}",
        );
    }

    // The C column, against an independent literal expectation: `deflate_stored` at level 0,
    // `deflate_fast` at 1 to 3, `deflate_slow` at 4 to 9 (deflate.c:114-124).
    for (level, index) in levels(rows) {
        let expected = match index {
            0 => OracleCompressFunc::Stored,
            1..=3 => OracleCompressFunc::Fast,
            _ => OracleCompressFunc::Slow,
        };
        let reference = oracle_config(level).expect("the level is below the reported row count");
        assert_eq!(
            reference.func,
            Some(expected),
            "configuration_table[{index}].func is not the compressor {CONFIG_TABLE_SOURCE} names. \
             A None here would mean the stored address matched none of deflate.c's five \
             compressors, i.e. that the reference gained a sixth",
        );
    }

    // The port's column, against the C column, for every ordered pair of levels. The port's
    // `Config::func` is pub(crate), so the grouping is read behaviourally instead; a pair that
    // agrees on the compressor must leave `deflate_params` with nothing to flush, and a pair that
    // does not must force it to try.
    for (from_level, from) in levels(rows) {
        let from_func = oracle_config(from_level)
            .expect("the level is below the reported row count")
            .func;

        for (to_level, to) in levels(rows) {
            let to_func = oracle_config(to_level)
                .expect("the level is below the reported row count")
                .func;

            assert_eq!(
                port_changes_compressor(from, to),
                from_func != to_func,
                "configuration_table: the port and {CONFIG_TABLE_SOURCE} disagree about whether \
                 levels {from} and {to} name the same compressor. The C table says {from_func:?} \
                 and {to_func:?}. deflate.c:791 compares the two rows' func by identity to decide \
                 whether a level change must first close the open block, so a wrong grouping makes \
                 deflateParams flush where C does not, or fail to where C does -- and either \
                 changes the emitted bytes",
            );
        }
    }
}
