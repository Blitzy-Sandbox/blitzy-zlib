//! Integration tests for `zlib_rs::error`, the status-code module that every
//! other part of the engine reports through.
//!
//! This is the foundational suite of `crates/zlib-rs/tests`. Every fallible
//! operation in the crate hands back a [`ReturnCode`], and the exported
//! `zError` is nothing but a thin wrapper over [`err_msg`], so a defect in that
//! module does not announce itself as a status-code defect -- it surfaces as an
//! inexplicable failure in whichever suite happens to run next. Pinning the
//! codes and their messages down here is what keeps that from happening. The
//! suite is correspondingly cheap: a few hundred assertions over constant data,
//! no allocation beyond a handful of `Debug` renderings, and no loop wider than
//! 33 iterations, because it also has to stay fast under
//! `cargo +nightly miri test`.
//!
//! # Where the expected values come from
//!
//! Nothing below is invented. Every expectation is transcribed from the C
//! reference, which is the behavioural oracle for this port:
//!
//! * the nine status integers come from `zlib.h` L181-L189;
//! * the ten message strings come from the `z_errmsg[10]` table at `zutil.c`
//!   L13-L24;
//! * the code-to-slot mapping comes from the `ERR_MSG` macro at `zutil.h` L65,
//!   namely `z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]`.
//!
//! Two consequences of that formula look like defects and are not. `Z_OK` (0)
//! selects slot 2, which holds the **empty string** rather than a word such as
//! "ok"; and every code outside `-6 ..= 2` selects slot 9, which is empty as
//! well. So an empty message does not mean "invalid code", and two distinct
//! codes legitimately share one message. Both facts are asserted below
//! precisely so that a future reader cannot mistake either for an oversight and
//! "fix" it into an incompatibility.
//!
//! # Why the assertions go through `err_msg` rather than through the table
//!
//! `zlib.map` lists `z_errmsg` in the `local:` block of its `ZLIB_1.2.0` node,
//! so that symbol must never be exported, and the port honours this by keeping
//! the ported `Z_ERRMSG` array `pub(crate)`. An integration test is a separate
//! crate: it cannot name that array, and must not be made able to. Every
//! message assertion here therefore routes through the public [`err_msg`] and
//! [`ReturnCode::msg`], which is the same path the facade's `zError` takes.
//!
//! Coverage that genuinely needs the array itself -- its ten-slot shape, and
//! the `usize` arithmetic of the private index helper -- is **delegated to the
//! `#[cfg(test)] mod tests` block inside `crates/zlib-rs/src/error.rs`**, which
//! lives in the crate and can see both. That split is deliberate. Widening the
//! array's visibility in order to move those assertions into this file would
//! break the hidden-symbol contract that the symbol-parity test in
//! `crates/libz-rs-sys/tests` enforces against the reference library's export
//! list.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, and no FFI.** The crate under test carries
//!   `#![forbid(unsafe_code)]`. Raw-pointer and C-ABI behaviour is the business
//!   of `crates/libz-rs-sys/tests`; needing an escape hatch here would mean
//!   testing the wrong crate.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` has an empty
//!   `[dependencies]` table and declares no `[dev-dependencies]`, and the
//!   `[bans]` section of `deny.toml` names that table as the enforcement point.
//!   The assertions are the built-in macros over `core`/`alloc`/`std`, exactly
//!   as the C suite uses a hand-rolled `CHECK_ERR` rather than a framework.
//! * **No feature gating.** The import below uses the module path
//!   `zlib_rs::error::...`, not the crate-root re-exports, which each carry
//!   `#[cfg(feature = "rust-api")]` while `default = []`. `pub mod error` is
//!   unconditional, so this file compiles and passes identically under
//!   `--no-default-features`, `--features simd` and `--all-features`.
//! * **`std` is available.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but a test binary is its own
//!   crate and links `std` unconditionally, so no `#![no_std]` appears here.

// Shared helpers for the suites in this folder. Only `check_err` is used, in
// the final test; the module carries its own `#![allow(dead_code)]` so the
// helpers this file has no use for do not warn.
mod common;

use zlib_rs::error::{err_msg, ReturnCode};

/// Every documented status, one row per slot of the C message table.
///
/// The columns are, in order:
///
/// 1. the [`ReturnCode`] constant under test;
/// 2. the exact C `int` that `zlib.h` L181-L189 assigns to it;
/// 3. the exact `z_errmsg` entry that `zutil.c` L13-L24 pairs with it;
/// 4. the C spelling of the constant's name, carried purely so that a failure
///    message names the symbol a reviewer will grep for rather than an
///    anonymous row number.
///
/// The rows are in **table order** rather than numeric order, so a row's
/// position is exactly the `z_errmsg` slot that the `ERR_MSG` formula selects
/// for that code. That property is itself asserted, in
/// `err_msg_reproduces_every_documented_slot_of_the_c_table`, so the ordering
/// cannot rot silently underneath the tests that depend on it.
///
/// Slot 9 of the C table has no row here because no documented code maps to
/// it: it is the out-of-range sentinel, and it is covered by
/// `codes_outside_the_documented_range_map_to_the_empty_sentinel` instead.
///
/// `#[rustfmt::skip]` keeps one row per line so the table can be diffed against
/// `zutil.c` L13-L24 by eye. Three of the rows exceed rustfmt's call width and
/// would otherwise be exploded across five lines each, which destroys exactly
/// the column alignment that makes such a diff possible. This is the mechanism
/// `.rustfmt.toml` names for a bespoke layout, and the same one
/// `crates/zlib-rs/src/deflate/config_table.rs` uses for the per-level tuning
/// table it ports.
#[rustfmt::skip]
const ORACLE: [(ReturnCode, i32, &str, &str); 9] = [
    (ReturnCode::NEED_DICT, 2, "need dictionary", "Z_NEED_DICT"),
    (ReturnCode::STREAM_END, 1, "stream end", "Z_STREAM_END"),
    (ReturnCode::OK, 0, "", "Z_OK"),
    (ReturnCode::ERRNO, -1, "file error", "Z_ERRNO"),
    (ReturnCode::STREAM_ERROR, -2, "stream error", "Z_STREAM_ERROR"),
    (ReturnCode::DATA_ERROR, -3, "data error", "Z_DATA_ERROR"),
    (ReturnCode::MEM_ERROR, -4, "insufficient memory", "Z_MEM_ERROR"),
    (ReturnCode::BUF_ERROR, -5, "buffer error", "Z_BUF_ERROR"),
    (ReturnCode::VERSION_ERROR, -6, "incompatible version", "Z_VERSION_ERROR"),
];

/// The C `int` of the first row of [`ORACLE`], which is also the upper bound of
/// the `ERR_MSG` formula's in-range interval.
///
/// Mirrors the literal `2` in `zutil.h` L65.
const FIRST_ORACLE_VALUE: i32 = 2;

/// Codes that fall outside `-6 ..= 2` and must therefore select slot 9.
///
/// The list is chosen for the distinct ways a port can get this wrong rather
/// than for volume:
///
/// * `3` and `-7` are the two off-by-one neighbours of the interval, the single
///   most likely transcription slip;
/// * `4` and `-100` are ordinary out-of-range values on either side;
/// * `1000` is the value the module documentation of `err_msg` calls out, since
///   `zError(1000)` returns `""` in the reference implementation;
/// * `i32::MAX` and `i32::MIN` are the extremes, and `i32::MIN` is the one that
///   matters -- see
///   `codes_outside_the_documented_range_map_to_the_empty_sentinel`.
const OUT_OF_RANGE_CODES: [i32; 7] = [3, 4, 1000, i32::MAX, -7, -100, i32::MIN];

/// Each status carries the exact C `int` the header assigns to it.
///
/// Written out one code at a time rather than driven from [`ORACLE`], so that a
/// transposition -- `Z_MEM_ERROR` and `Z_BUF_ERROR` swapped is the classic one
/// -- fails on a line that names the constant it is about, and a sign slip
/// (`6` where `-6` belongs) fails on the line for that constant alone.
#[test]
fn every_status_carries_the_exact_c_integer() {
    assert_eq!(ReturnCode::OK.as_i32(), 0, "Z_OK must be 0 (zlib.h L181)");
    assert_eq!(
        ReturnCode::STREAM_END.as_i32(),
        1,
        "Z_STREAM_END must be 1 (zlib.h L182)"
    );
    assert_eq!(
        ReturnCode::NEED_DICT.as_i32(),
        2,
        "Z_NEED_DICT must be 2 (zlib.h L183)"
    );
    assert_eq!(
        ReturnCode::ERRNO.as_i32(),
        -1,
        "Z_ERRNO must be -1 (zlib.h L184)"
    );
    assert_eq!(
        ReturnCode::STREAM_ERROR.as_i32(),
        -2,
        "Z_STREAM_ERROR must be -2 (zlib.h L185)"
    );
    assert_eq!(
        ReturnCode::DATA_ERROR.as_i32(),
        -3,
        "Z_DATA_ERROR must be -3 (zlib.h L186)"
    );
    assert_eq!(
        ReturnCode::MEM_ERROR.as_i32(),
        -4,
        "Z_MEM_ERROR must be -4 (zlib.h L187)"
    );
    assert_eq!(
        ReturnCode::BUF_ERROR.as_i32(),
        -5,
        "Z_BUF_ERROR must be -5 (zlib.h L188)"
    );
    assert_eq!(
        ReturnCode::VERSION_ERROR.as_i32(),
        -6,
        "Z_VERSION_ERROR must be -6 (zlib.h L189), not 6"
    );
}

/// Converting a status to the C `int` and back recovers the same status, and
/// both directions agree with the oracle.
///
/// The integer is what actually crosses the ABI boundary, so a status that did
/// not survive the round trip would mean a C caller and the engine disagreeing
/// about what happened. Both spellings of the outbound conversion are checked
/// -- [`ReturnCode::as_i32`], which is the `const fn` the engine itself uses,
/// and the [`From`] implementation, which is the idiomatic form -- because the
/// facade and the Rust surface do not use the same one.
#[test]
fn statuses_round_trip_losslessly_through_the_c_integer() {
    for &(code, value, _, name) in &ORACLE {
        assert_eq!(
            code.as_i32(),
            value,
            "{name}: as_i32 must yield the C integer {value}"
        );
        assert_eq!(
            i32::from(code),
            value,
            "{name}: i32::from must agree with as_i32"
        );
        assert_eq!(
            ReturnCode::from_i32(value),
            Some(code),
            "{name}: from_i32({value}) must recover the constant"
        );
        assert_eq!(
            ReturnCode::from_i32(code.as_i32()),
            Some(code),
            "{name}: as_i32 followed by from_i32 must be the identity"
        );
    }
}

/// `err_msg` reproduces every message the C table pairs with a documented code,
/// and [`ReturnCode::msg`] agrees with it.
///
/// The first loop re-establishes the invariant the whole file leans on: because
/// the `ERR_MSG` slot of a code is `2 - code`, listing the rows in table order
/// means their C integers must run 2, 1, 0, -1, ... down to -6 with no gap. A
/// reordered or duplicated row would otherwise quietly weaken every assertion
/// that follows.
#[test]
fn err_msg_reproduces_every_documented_slot_of_the_c_table() {
    let mut expected_value = FIRST_ORACLE_VALUE;
    for &(_, value, _, name) in &ORACLE {
        assert_eq!(
            value, expected_value,
            "ORACLE is out of table order at {name}: slot n must hold code 2 - n"
        );
        expected_value -= 1;
    }
    assert_eq!(
        expected_value, -7,
        "ORACLE must cover the nine documented codes from 2 down to -6"
    );

    for &(code, value, message, name) in &ORACLE {
        assert_eq!(
            err_msg(value),
            message,
            "err_msg({value}) must be the {name} entry of z_errmsg (zutil.c L13-L24)"
        );
        assert_eq!(
            code.msg(),
            message,
            "{name}: msg() must agree with err_msg({value})"
        );
    }
}

/// `Z_OK` maps to the empty string, not to a word meaning success.
///
/// Slot 2 of `z_errmsg` is `""` (`zutil.c` L16). This is called out on its own
/// because "success has no message" is counter-intuitive enough that a port is
/// tempted to substitute `"ok"`, and any such substitution would change what
/// `zError(Z_OK)` returns for every C consumer.
#[test]
fn z_ok_maps_to_the_empty_message_not_to_a_success_word() {
    assert_eq!(
        err_msg(0),
        "",
        "err_msg(Z_OK) must be the empty string (zutil.c L16)"
    );
    assert_eq!(
        ReturnCode::OK.msg(),
        "",
        "ReturnCode::OK.msg() must be the empty string (zutil.c L16)"
    );
}

/// Codes outside `-6 ..= 2` select the sentinel slot and return `""`, and none
/// of them can make the lookup panic.
///
/// `i32::MIN` is not a formality here, it is the trap this test exists for. The
/// C macro computes its index as the bare subtraction `2 - (err)`, which for
/// `err == i32::MIN` overflows; C leaves signed overflow undefined, whereas Rust
/// **panics on it in a debug build** -- and a debug build is exactly what
/// `cargo test` and Miri produce. So a faithful-looking transcription that
/// evaluated the subtraction before the range test would abort a C caller's
/// process on an input the reference implementation answers with `""`. The port
/// must therefore range-check first; nothing else in the workspace proves that
/// it does, so do not remove this case on the grounds that it looks trivial.
///
/// `from_i32` is checked alongside `err_msg` because the two are deliberately
/// asymmetric: an undocumented integer has no [`ReturnCode`], so `from_i32`
/// answers [`None`], while `err_msg` still answers a string, exactly as `zError`
/// accepts any `int`. Both behaviours are total and neither may panic.
#[test]
fn codes_outside_the_documented_range_map_to_the_empty_sentinel() {
    for &code in &OUT_OF_RANGE_CODES {
        assert_eq!(
            err_msg(code),
            "",
            "err_msg({code}) must select slot 9 of z_errmsg, which is empty (zutil.h L65)"
        );
        assert_eq!(
            ReturnCode::from_i32(code),
            None,
            "from_i32({code}) must reject an undocumented code rather than invent one"
        );
    }
}

/// The in-range interval of the index formula is exactly `-6 ..= 2`.
///
/// An off-by-one in either direction is the likeliest porting mistake, and it is
/// silent: a range one too narrow turns a real message into `""`, and one too
/// wide turns an out-of-range code into a real message. Both are checked from
/// both sides of both endpoints, so a boundary can only move by failing here.
#[test]
fn the_range_boundaries_of_the_index_formula_are_exact() {
    // Inside, at the endpoints.
    assert_eq!(
        err_msg(2),
        "need dictionary",
        "Z_NEED_DICT (2) is the upper endpoint and must be inside the range"
    );
    assert_eq!(
        err_msg(-6),
        "incompatible version",
        "Z_VERSION_ERROR (-6) is the lower endpoint and must be inside the range"
    );

    // Immediately outside, on each side.
    assert_eq!(
        err_msg(3),
        "",
        "3 is one past Z_NEED_DICT and must be outside the range"
    );
    assert_eq!(
        err_msg(-7),
        "",
        "-7 is one past Z_VERSION_ERROR and must be outside the range"
    );

    // Just inside each endpoint, so that a range shifted by one is caught even
    // if its width happens to be preserved.
    assert_eq!(
        err_msg(1),
        "stream end",
        "Z_STREAM_END (1) must be inside the range"
    );
    assert_eq!(
        err_msg(-5),
        "buffer error",
        "Z_BUF_ERROR (-5) must be inside the range"
    );
}

/// Statuses compare by the integer they carry, and are `Copy` rather than merely
/// `Clone`.
///
/// `Copy` is load-bearing rather than cosmetic: the engine threads a status
/// through nested returns and the facade hands the same value both to a
/// `z_stream`'s message slot and to its own return statement, so a status that
/// moved on use would force the code that has to stay closest to the C control
/// flow into clones the reference does not have.
#[test]
fn statuses_compare_by_value_and_are_copy_not_merely_clone() {
    // Built from the same integer by two separate calls: equality is by value,
    // not by identity.
    assert_eq!(
        ReturnCode::from_i32(-4),
        ReturnCode::from_i32(-4),
        "two statuses built from the same integer must compare equal"
    );
    assert_eq!(
        ReturnCode::from_i32(-4),
        Some(ReturnCode::MEM_ERROR),
        "from_i32(-4) must equal the Z_MEM_ERROR constant"
    );
    assert_ne!(
        ReturnCode::from_i32(-4),
        ReturnCode::from_i32(-5),
        "statuses built from different integers must compare unequal"
    );
    assert_ne!(
        ReturnCode::MEM_ERROR,
        ReturnCode::BUF_ERROR,
        "Z_MEM_ERROR and Z_BUF_ERROR are distinct codes and must not compare equal"
    );

    let original = ReturnCode::DATA_ERROR;
    let copied = original;
    // `original` is still live on the next line, which compiles only because
    // `ReturnCode` is `Copy`: were it merely `Clone`, the binding above would
    // have moved it and this use would be rejected.
    assert_eq!(
        copied, original,
        "a copied status must still compare equal to its source"
    );
    assert_eq!(
        copied.as_i32(),
        original.as_i32(),
        "a copied status must carry the same C integer as its source"
    );

    // Every documented code must be distinct from every other, so a `ReturnCode`
    // can never be ambiguous about which status it reports.
    for &(left, _, _, left_name) in &ORACLE {
        for &(right, _, _, right_name) in &ORACLE {
            if left_name == right_name {
                assert_eq!(left, right, "{left_name} must compare equal to itself");
            } else {
                assert_ne!(
                    left, right,
                    "{left_name} and {right_name} must be distinct statuses"
                );
            }
        }
    }
}

/// Every status renders as something non-empty under `Debug`.
///
/// Deliberately loose. The exact `Debug` text is not a stability contract and no
/// consumer may depend on it, so asserting a literal rendering here would only
/// create a test that breaks on a harmless improvement. What does matter is that
/// a status is never invisible in a diagnostic: `common::check_err` and the
/// assertion messages throughout this folder interpolate `{:?}`, and an empty
/// rendering would quietly strip the offending code out of every failure report.
#[test]
fn debug_renders_every_status_as_something_non_empty() {
    for &(code, _, _, name) in &ORACLE {
        let rendered = format!("{code:?}");
        assert!(
            !rendered.is_empty(),
            "the Debug rendering of {name} must not be empty"
        );
    }
}

/// A bounded scan restates the whole index formula in one predicate.
///
/// A message is non-empty exactly when the code is in `-6 ..= 2` **and** is not
/// `Z_OK`: everything outside the interval lands on the empty sentinel at slot
/// 9, and `Z_OK` lands on the empty entry at slot 2. That single sentence is the
/// entire observable behaviour of `ERR_MSG`, and this loop checks it at every
/// point of a window that extends ten codes past each endpoint.
///
/// The window is kept at 33 iterations rather than swept over `i32` on purpose:
/// the extremes are covered exactly, by
/// `codes_outside_the_documented_range_map_to_the_empty_sentinel`, and this
/// suite has to stay quick under Miri, where every iteration is interpreted.
#[test]
fn a_bounded_scan_restates_the_whole_index_formula() {
    for code in -16..=16 {
        let message = err_msg(code);
        let expected_non_empty = (-6..=2).contains(&code) && code != 0;
        assert_eq!(
            !message.is_empty(),
            expected_non_empty,
            "err_msg({code}) returned {message:?}; a non-empty message is expected exactly \
             for -6 ..= 2 excluding Z_OK (zutil.h L65)"
        );
    }
}

/// `is_error` follows the sign rule the header documents.
///
/// `zlib.h` L190-L192 states that negative values are errors while positive
/// values report "special but normal events". So `Z_STREAM_END` and
/// `Z_NEED_DICT` are **not** errors despite being non-zero, which is the whole
/// reason they were given positive values, and a caller that tested `ret != 0`
/// instead would mishandle both.
#[test]
fn is_error_follows_the_documented_sign_rule() {
    for &(code, value, _, name) in &ORACLE {
        assert_eq!(
            code.is_error(),
            value < 0,
            "{name} carries {value}, so is_error must report {} (zlib.h L190-L192)",
            value < 0
        );
    }

    assert!(!ReturnCode::OK.is_error(), "Z_OK must not be an error");
    assert!(
        !ReturnCode::STREAM_END.is_error(),
        "Z_STREAM_END must not be an error"
    );
    assert!(
        !ReturnCode::NEED_DICT.is_error(),
        "Z_NEED_DICT must not be an error"
    );
    assert!(ReturnCode::ERRNO.is_error(), "Z_ERRNO must be an error");
    assert!(
        ReturnCode::VERSION_ERROR.is_error(),
        "Z_VERSION_ERROR must be an error"
    );
}

/// `record_msg` stores the status's message in the slot and yields the status
/// back unchanged.
///
/// This is the public, cross-crate reachable form of the `ERR_RETURN` macro at
/// `zutil.h` L67-L68, and it is exercised from an integration test rather than
/// only from inside the crate for a specific reason: the crate-private helper it
/// delegates to cannot be named from another crate, and populating the `msg`
/// field of a `z_stream` is precisely what the `libz-rs-sys` facade needs. So
/// this test stands in for the facade's use of it, from the same side of the
/// crate boundary.
///
/// The `Z_OK` case is separate because it is where a plausible "improvement"
/// would diverge: the macro assigns the empty string, so the slot must end up
/// `Some("")`. Leaving it [`None`] would be the natural Rust instinct and would
/// contradict the reference, which writes a valid pointer to an empty string
/// rather than a null one.
#[test]
fn record_msg_stores_the_message_and_yields_the_status_back() {
    for &(code, _, message, name) in &ORACLE {
        let mut slot: Option<&'static str> = None;
        let returned = code.record_msg(&mut slot);
        assert_eq!(
            returned, code,
            "{name}: record_msg must yield the status it was given"
        );
        assert_eq!(
            slot,
            Some(message),
            "{name}: record_msg must store the z_errmsg entry"
        );
    }

    // Z_OK records the empty string, not nothing at all.
    let mut slot: Option<&'static str> = None;
    let returned = ReturnCode::OK.record_msg(&mut slot);
    assert_eq!(
        returned,
        ReturnCode::OK,
        "record_msg must yield Z_OK unchanged"
    );
    assert_eq!(
        slot,
        Some(""),
        "Z_OK must record the empty string rather than None"
    );

    // A second call overwrites rather than accumulates, which is what a plain
    // pointer assignment in C does.
    let overwritten = ReturnCode::BUF_ERROR.record_msg(&mut slot);
    assert_eq!(
        overwritten,
        ReturnCode::BUF_ERROR,
        "record_msg must yield Z_BUF_ERROR unchanged"
    );
    assert_eq!(
        slot,
        Some("buffer error"),
        "record_msg must replace the previous message"
    );
}

/// `Z_OK` is the status the shared harness accepts as success.
///
/// `common::check_err` -- the Rust form of `CHECK_ERR` from `test/example.c`
/// L28-L33 -- is the gate that every sibling suite in this folder routes its
/// success assertions through. If it and this module's oracle ever disagreed
/// about which status means success, every one of those suites would assert
/// against the wrong value while still reporting green. This file is the
/// folder's canary, so this is where the two are pinned to each other.
///
/// The recovered value is checked as well as the constant, because the status a
/// C caller produces arrives as a raw `int`: it is the value coming back out of
/// [`ReturnCode::from_i32`], not the constant, that a facade entry point
/// actually holds.
#[test]
fn z_ok_is_the_status_the_shared_harness_accepts_as_success() {
    common::check_err(ReturnCode::OK, "ReturnCode::OK");

    let recovered = ReturnCode::from_i32(0);
    assert_eq!(
        recovered,
        Some(ReturnCode::OK),
        "from_i32(0) must recover Z_OK"
    );

    let code = recovered.unwrap();
    common::check_err(code, "ReturnCode::from_i32(Z_OK)");
    assert_eq!(
        code.as_i32(),
        0,
        "the recovered success status must still carry 0"
    );
    assert_eq!(
        code.msg(),
        "",
        "the recovered success status must carry the empty message"
    );
    assert!(
        !code.is_error(),
        "the recovered success status must not be an error"
    );
}
