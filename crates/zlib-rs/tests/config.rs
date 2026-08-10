// UNSAFE CONTAINMENT, and it is mechanical rather than a convention.  `crates/zlib-rs` is the
// safe core: `src/lib.rs` carries `#![forbid(unsafe_code)]`, and its test suites carry it too, so
// the property "the core and everything that exercises it contains no `unsafe`" is enforced by the
// compiler in both halves.  The workspace's designated FFI boundary -- the only place a raw pointer
// crosses into a foreign implementation -- is `crates/libz-rs-sys/src/**` for the shipped library
// and `crates/zlib-rs-differential/src/{oracle,port}.rs` for the dev-only harness; an assertion
// that needs one of those belongs in a suite of that package, not here.
#![forbid(unsafe_code)]
//! Integration tests for `zlib_rs::config` -- the parameter-validation boundary.
//!
//! Every caller of this library crosses this boundary exactly once per stream, in
//! `deflateInit2_` or `inflateInit2_`, and the boundary is made entirely of bounds
//! on five small integers. An off-by-one in any of them is invisible in normal use
//! and catastrophic at the edges: tighten one and the library refuses a stream the
//! reference compresses, loosen one and it builds a state the reference refuses.
//! Neither is a hardening improvement; both are behaviour changes. This suite is
//! where each bound is pinned to the value the C sources actually carry.
//!
//! # What this adds over the unit tests inside `src/config.rs`
//!
//! Three things a `#[cfg(test)] mod tests` inside the crate structurally cannot do:
//!
//! 1. **It proves the surface is reachable from outside.** An integration test is a
//!    separate crate, so anything it names has to be genuinely `pub` *and* exported
//!    on a path a real consumer can spell. That matters more here than usual --
//!    `zlib-rs` declares `default = []` and every crate-root re-export carries
//!    `#[cfg(feature = "rust-api")]`, so `zlib_rs::DeflateConfig` does not exist in
//!    a default build while `zlib_rs::config::DeflateConfig` always does. Every
//!    import below is by module path for that reason, which is also exactly how the
//!    `libz-rs-sys` facade consumes the crate.
//! 2. **It drives the real entry points.** Asserting that a predicate says "no" is
//!    weaker than asserting that `deflate_init2` says "no": a bound that is checked
//!    in `config` but not consulted by the initialisation path would pass the former
//!    and fail the latter. Each section therefore asserts the verdict twice -- once
//!    through the predicate, once through `deflate_init2` / `inflate_init2` /
//!    `inflate_reset2` -- and, where the state exposes them, cross-checks the
//!    resolved parameters the state ends up holding.
//! 3. **It sweeps against an independently formulated oracle.** The two
//!    `windowBits` decoders are ports of imperative C (sign test, shift, mask,
//!    range check). The oracles in this file are *declarative* range tables
//!    transcribed from the prose of `zlib.h` and from the accept/reject outcome of
//!    `deflate.c` L422-L439 and `inflate.c` L145-L161. Formulating the same rule two
//!    different ways is what makes a shared transcription error unlikely: a slip in
//!    the imperative version does not produce the same slip in the table.
//!
//! # Sources of truth
//!
//! | Value | C source |
//! |---|---|
//! | levels, strategies, data types, method | `zlib.h` L194-L214 |
//! | flush modes | `zlib.h` L172-L178 |
//! | `MAX_MEM_LEVEL`, `MAX_WBITS` | `zconf.h` L272-L289 |
//! | `DEF_WBITS`, `DEF_MEM_LEVEL`, block types, match lengths, `PRESET_DICT` | `zutil.h` L72-L96 |
//! | deflate `windowBits` and the four other parameter bounds | `deflate.c` L419-L439 |
//! | inflate `windowBits`, including `0` and the `+32` band | `inflate.c` L145-L161 |
//! | `inflateBack`'s narrower rule | `infback.c` L33-L35 |
//! | `deflateParams`' bounds | `deflate.c` L784-L788 |
//! | `deflate`'s flush guard | `deflate.c` L985 |
//! | the three window-size vectors | `test/infcover.c` L371, L402, L403 |
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no FFI.** The crate under test carries
//!   `#![forbid(unsafe_code)]`; nothing here needs an escape hatch either.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` has an empty
//!   `[dependencies]` table and declares no `[dev-dependencies]`, and the `[bans]`
//!   section of `deny.toml` names that table as the enforcement point. The sweeps
//!   below are hand-written nested loops over named corner values rather than a
//!   property-testing dependency, which is both sufficient and far cheaper to
//!   interpret.
//! * **`std` is available and `no_std` is not.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test is its
//!   own crate and links `std` unconditionally, so no `#![no_std]` appears here.
//! * **No `#[cfg(feature = ...)]`.** This file compiles and passes identically
//!   under `--no-default-features`, `--features simd`, `--features std` and
//!   `--all-features`; nothing it touches is feature-gated.
//! * **Every sweep is deliberately bounded.** See the note on
//!   `LEVEL_CORNERS` for why, and for the rule to apply if one ever has to shrink.

mod common;

use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::config::{
    decode_deflate_window_bits, decode_inflate_window_bits, normalize_deflate_level,
    validate_deflate_flush, validate_deflate_params, validate_deflate_params_change,
    validate_inflate_back_window_bits, validate_mem_level, DeflateConfig, Flush, InflateConfig,
    InflateWrap, Method, Strategy, Wrap, DEF_LEVEL, DEF_MEM_LEVEL, DEF_WBITS, DYN_TREES, MAX_MATCH,
    MAX_MEM_LEVEL, MAX_WBITS, MIN_MATCH, MIN_MEM_LEVEL, MIN_WBITS, PRESET_DICT, STATIC_TREES,
    STORED_BLOCK, Z_ASCII, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_BINARY, Z_BLOCK,
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED, Z_FINISH, Z_FIXED,
    Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_RLE,
    Z_SYNC_FLUSH, Z_TEXT, Z_TREES, Z_UNKNOWN,
};
use zlib_rs::deflate::{deflate_end, deflate_init2, DeflateState};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::{inflate_end, inflate_init2, inflate_reset2};

// ---------------------------------------------------------------------------
// The sweep bounds
// ---------------------------------------------------------------------------

/// The lowest `windowBits` the sweeps visit.
///
/// One below `-31`, so the sweep steps off the bottom of every negative rule
/// rather than starting inside one. `-16` (the first refusal below raw's `-15`)
/// and `-32` are both covered.
const SWEEP_LOW: i32 = -32;

/// The highest `windowBits` the sweeps visit.
///
/// `48` is the first value `inflate.c` L154-L155 declines to mask, so it is the
/// first refusal above the `+32` automatic-detection band and has to be inside the
/// sweep. Everything from `33` up is covered on the way there.
const SWEEP_HIGH: i32 = 48;

/// The corner levels the combination sweep visits.
///
/// `-1` is the magic default, `0` selects stored output, `1` and `9` are the two
/// documented extremes and `6` is what `-1` resolves to. Every level in `0..=9` is
/// checked individually elsewhere; this is the subset the *combination* sweep
/// crosses with the other three fields.
///
/// # Why the sweeps are bounded rather than exhaustive
///
/// The whole point of this suite is that it runs everywhere it is pointed --
/// including under `cargo +nightly miri test`, whose interpreter is several orders
/// of magnitude slower than native code. A full cross product of the four
/// parameters' real ranges is 11 x 81 x 12 x 6 configurations, and stepping every
/// one of them through an allocating initialisation would put this file firmly in
/// the "disabled under Miri" category, which is the one outcome that would make it
/// worthless. Named corner values give the same defect-detection power for a tiny
/// fraction of the cost, because validation is a conjunction of independent range
/// checks: a wrong bound shows up at the boundary of its own field, and the
/// boundaries are all here.
///
/// If this ever does need to be cheaper, **shrink these corner sets** -- never
/// reach for `#[cfg_attr(miri, ignore)]`.
const LEVEL_CORNERS: [i32; 5] = [Z_DEFAULT_COMPRESSION, 0, 1, 6, 9];

/// The corner `windowBits` the combination sweep visits: two raw, two zlib, two
/// gzip, each pair being the smallest and largest the container accepts.
const WINDOW_BITS_CORNERS: [i32; 6] = [-15, -9, 9, 15, 25, 31];

/// The corner `memLevel` values the combination sweep visits: both bounds plus the
/// default.
const MEM_LEVEL_CORNERS: [i32; 3] = [MIN_MEM_LEVEL, DEF_MEM_LEVEL, MAX_MEM_LEVEL];

/// Integer extremes that a range check written with arithmetic rather than
/// comparison would mis-handle, and that a debug build would panic on.
///
/// Every validator in `config` is documented as total over `i32`, so these belong
/// in each field's rejection list rather than in a test of their own.
const INTEGER_EXTREMES: [i32; 2] = [i32::MIN, i32::MAX];

/// The cheapest `windowBits` a deflate state can be built with: a 512-byte window.
///
/// Used by every test that has to *build* a state for a reason other than the window
/// size itself -- the level, strategy, method and combination checks. Paired with
/// [`MIN_MEM_LEVEL`] it makes a state cost about 3 KiB of allocation instead of the
/// 197 KiB the library's own defaults ask for.
///
/// # Why the allocating tests are deliberately frugal
///
/// A `deflate_state` is not cheap: `deflate.c` L440-L530 allocates a window of
/// `2 * 2^windowBits` bytes, a `prev` array of `2^windowBits` positions, a hash table
/// of `2^(memLevel + 7)` positions and a pending buffer of `4 * 2^(memLevel + 6)`
/// bytes, and this implementation fills each one with the `0xa5` sentinel in a debug build so
/// that a read of uninitialised memory shows up rather than reading a convenient
/// zero. Native code does that in microseconds. Miri, which interprets every write
/// and tracks provenance for each one, gets through roughly 200 KiB of it in half a
/// minute -- measured, not estimated -- so a suite that initialises a hundred
/// default-sized states is a suite nobody will ever run under Miri.
///
/// The response is to separate the two claims each allocating assertion makes:
///
/// * **"the rule is right"** is checked by the allocation-free validators
///   (`decode_deflate_window_bits`, `validate_mem_level`, `DeflateConfig::validate`)
///   over the *whole* input span, because those cost nothing; and
/// * **"the initialisation path consults the rule and can build what it describes"**
///   is checked through `deflate_init2` over named corner values, because that claim
///   is per-path rather than per-value and does not need repeating 81 times.
///
/// A refusal allocates nothing, so every *rejection* is still driven through
/// `deflate_init2` at full breadth; only acceptances are rationed.
const SMALL_WINDOW_BITS: i32 = MIN_WBITS + 1;

/// The `windowBits` values whose *acceptance* is confirmed through the real,
/// allocating `deflate_init2`.
///
/// Five values covering everything that path can get wrong: one request per container
/// (`-9` raw, `9` zlib, `25` gzip) so a dropped container shows up, the `8` that must
/// arrive as `9`, and `15` so the largest window the library will ever allocate is
/// actually built once. The exponent alone determines the allocation size, so
/// covering the largest exponent once rather than three times costs 132 KiB instead
/// of 396 KiB and proves the same thing.
///
/// The accept/reject *rule* is verified over the full `-32 ..= 48` span by
/// `the_deflate_window_bits_sweep_matches_the_reference_table`; see
/// [`SMALL_WINDOW_BITS`] for the reasoning behind the split.
const ALLOCATING_WINDOW_BITS: [i32; 5] = [-9, 8, 9, 15, 25];

// ---------------------------------------------------------------------------
// Declaratively transcribed oracles
// ---------------------------------------------------------------------------

/// What `deflateInit2_` does with a `windowBits` argument, as a table of ranges.
///
/// `Some((container, exponent))` for a request the reference accepts, [`None`] for
/// one it refuses. The exponent is returned as an `i32` purely so that comparing it
/// against the `u8` the implementation returns needs a widening `i32::from` rather
/// than a narrowing cast; there is no other reason for the type.
///
/// The three rows, and the two values conspicuously missing from them:
///
/// * `-15 ..= -9` is raw DEFLATE (`zlib.h` L570-L572). **`-8` is absent**: only a
///   zlib header can transmit a window size, so `deflate.c` L436 refuses an
///   exponent of 8 for any container but zlib.
/// * `8 ..= 15` is the zlib container, and `8` maps to `9`, not to `8` --
///   `deflate.c` L439, "until 256-byte window bug fixed".
/// * `25 ..= 31` is the gzip container, `+16` (`zlib.h` L574-L580). **`24` is
///   absent** for the same reason `-8` is.
///
/// Mirrors the outcome of `deflate.c` L422-L439.
fn oracle_deflate_window_bits(window_bits: i32) -> Option<(Wrap, i32)> {
    match window_bits {
        -15..=-9 => Some((Wrap::None, -window_bits)),
        8 => Some((Wrap::Zlib, MIN_WBITS + 1)),
        9..=15 => Some((Wrap::Zlib, window_bits)),
        25..=31 => Some((Wrap::Gzip, window_bits - 16)),
        _ => None,
    }
}

/// What `inflateInit2_` and `inflateReset2` do with a `windowBits` argument, as a
/// table of ranges.
///
/// Deliberately a *different function* from [`oracle_deflate_window_bits`], because
/// the reference applies different rules and collapsing the two is the single most
/// likely defect in this area. Four rows have no deflate analogue at all:
///
/// * **`0` is accepted**, meaning "take the window size from the stream header"
///   (`zlib.h` L875-L876, `inflate.c` L160). `test/infcover.c` L402 pins it:
///   `inf("8 99", "set window size from header", 0, 0, 0, Z_OK)`.
/// * **`-8` is accepted**, where deflate refuses it: a decompressor is told the
///   window size, so a 256-byte raw window is unambiguous.
/// * `16` and `32` are accepted with a deferred exponent, being the `+16` and `+32`
///   offsets applied to a `windowBits` of `0`.
/// * `40 ..= 47` is the automatic zlib-or-gzip detection band (`zlib.h`
///   L890-L892). `47` in particular is what nearly every gzip-header test in
///   `test/infcover.c` initialises with.
///
/// And one row is a refusal worth naming: `1 ..= 7` is rejected, which
/// `test/infcover.c` L371 pins as `inf("", "bad window size", 0, 1, 0,
/// Z_STREAM_ERROR)`.
///
/// Mirrors the outcome of `inflate.c` L145-L161.
fn oracle_inflate_window_bits(window_bits: i32) -> Option<(InflateWrap, i32)> {
    match window_bits {
        -15..=-8 => Some((InflateWrap::None, -window_bits)),
        0 => Some((InflateWrap::Zlib, 0)),
        8..=15 => Some((InflateWrap::Zlib, window_bits)),
        16 => Some((InflateWrap::Gzip, 0)),
        24..=31 => Some((InflateWrap::Gzip, window_bits - 16)),
        32 => Some((InflateWrap::ZlibOrGzip, 0)),
        40..=47 => Some((InflateWrap::ZlibOrGzip, window_bits - 32)),
        _ => None,
    }
}

/// The decoded deflate `windowBits`, with the exponent widened for comparison.
///
/// Widening rather than narrowing keeps the comparison total: there is no input for
/// which building the expected value could itself fail.
fn decoded_deflate_window_bits(window_bits: i32) -> Option<(Wrap, i32)> {
    decode_deflate_window_bits(window_bits)
        .ok()
        .map(|(wrap, exponent)| (wrap, i32::from(exponent)))
}

/// The decoded inflate `windowBits`, with the exponent widened for comparison.
fn decoded_inflate_window_bits(window_bits: i32) -> Option<(InflateWrap, i32)> {
    decode_inflate_window_bits(window_bits)
        .ok()
        .map(|(wrap, exponent)| (wrap, i32::from(exponent)))
}

// ---------------------------------------------------------------------------
// Driving the real entry points
// ---------------------------------------------------------------------------

/// A [`DeflateConfig`] holding the four numeric parameters, with `method` fixed at
/// the only value that exists.
///
/// `strategy` is taken as a [`Strategy`] rather than an `i32` because the struct
/// field is typed; the raw-integer form of the same call is
/// [`validate_deflate_params`], which the strategy and method sections use instead.
fn deflate_config(
    level: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: Strategy,
) -> DeflateConfig {
    DeflateConfig {
        level,
        method: Method::Deflated,
        window_bits,
        mem_level,
        strategy,
    }
}

/// Runs a configuration through the real `deflate_init2` and returns the status a C
/// caller of `deflateInit2_` would observe.
///
/// `deflate_init2` finishes by calling `deflate_reset` (`deflate.c` L532), so a
/// state that comes back from it is in `INIT_STATE` or `GZIP_STATE` and never
/// `BUSY_STATE`. `deflate_end` therefore reports [`ReturnCode::OK`] on the success
/// path (`deflate.c` L1309), which is what makes a single status enough to describe
/// the whole round trip. Every allocated state is ended before returning, so the
/// sweeps below are leak-free under Miri.
fn deflate_init_status(config: DeflateConfig) -> ReturnCode {
    match deflate_init2(config, GlobalAllocator) {
        Ok(mut state) => deflate_end(&mut state),
        Err(code) => code,
    }
}

/// The parameters a live [`DeflateState`] actually holds, or the status that refused
/// the configuration.
///
/// Returned as `(level, container, window exponent, memLevel, strategy)`. `memLevel`
/// is recovered from `hash_bits`, which `deflate.c` L453 defines as `memLevel + 7`;
/// there is no direct accessor for it and there is no need for one, since the
/// derivation is exactly the relationship worth asserting.
fn deflate_state_parameters(
    config: DeflateConfig,
) -> Result<(i32, Option<Wrap>, u32, u32, Strategy), ReturnCode> {
    let mut state: DeflateState<'_, GlobalAllocator> = deflate_init2(config, GlobalAllocator)?;
    let parameters = (
        state.level(),
        state.container(),
        state.w_bits(),
        state.hash_bits() - 7,
        state.strategy(),
    );
    common::check_err(
        deflate_end(&mut state),
        "deflateEnd after parameter inspection",
    );
    Ok(parameters)
}

/// Runs a `windowBits` request through the real `inflate_init2` and returns the
/// status a C caller of `inflateInit2_` would observe.
///
/// `inflate_end` is unconditionally [`ReturnCode::OK`] (`inflate.c` L153-L160 of the
/// teardown), so as on the deflate side one status describes the round trip.
fn inflate_init_status(window_bits: i32) -> ReturnCode {
    match inflate_init2(InflateConfig::new(window_bits), GlobalAllocator) {
        Ok(mut state) => inflate_end(&mut state),
        Err(code) => code,
    }
}

/// Runs a `windowBits` request through `inflate_reset2` on an already-initialised
/// stream, and returns the status a C caller of `inflateReset2` would observe.
///
/// This is the function the decoder is literally a port of: `inflateInit2_` reaches
/// the same code only by delegating to it (`inflate.c` L206). Re-tuning a live
/// stream is also the path where a caller can legitimately move between containers,
/// so it is worth exercising separately rather than assuming the two agree.
///
/// A failure of the *initial* `inflate_init2` is returned as-is rather than
/// asserted here; `inflate_init2_accepts_the_default_configuration` pins that call
/// on its own, so an unexpected status is attributed to the right cause.
fn inflate_reset2_status(window_bits: i32) -> ReturnCode {
    let mut state = match inflate_init2(InflateConfig::default(), GlobalAllocator) {
        Ok(state) => state,
        Err(code) => return code,
    };

    let status = match inflate_reset2(&mut state, InflateConfig::new(window_bits)) {
        Ok(_reset) => ReturnCode::OK,
        Err(code) => code,
    };

    common::check_err(inflate_end(&mut state), "inflateEnd after inflateReset2");
    status
}

/// The status `validate_deflate_params` reports for five raw `int` arguments.
///
/// The exact shape of `deflateInit2_`'s parameter list, which is what lets the
/// method and strategy sections drive values no typed field can hold.
fn deflate_params_status(
    level: i32,
    method: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> ReturnCode {
    match validate_deflate_params(level, method, window_bits, mem_level, strategy) {
        Ok(_validated) => ReturnCode::OK,
        Err(code) => code,
    }
}

/// The status a validator that yields a value reports, discarding the value.
///
/// Collapsing `Result<T, ReturnCode>` to a [`ReturnCode`] keeps the accept and
/// reject assertions in the same shape, so a table of `(input, expected status)`
/// pairs can drive both.
fn status_of<T>(result: Result<T, ReturnCode>) -> ReturnCode {
    match result {
        Ok(_value) => ReturnCode::OK,
        Err(code) => code,
    }
}

// ---------------------------------------------------------------------------
// 3.1  Constants
// ---------------------------------------------------------------------------

/// Every compression level constant, checked one at a time.
///
/// Asserted individually rather than as a tuple so a failure names the single
/// constant that moved. A wrong value here does not fail loudly -- it silently
/// changes which levels the library accepts.
///
/// Mirrors `zlib.h` L194-L197.
#[test]
fn level_constants_are_the_zlib_h_values() {
    assert_eq!(Z_NO_COMPRESSION, 0, "Z_NO_COMPRESSION must be 0");
    assert_eq!(Z_BEST_SPEED, 1, "Z_BEST_SPEED must be 1");
    assert_eq!(Z_BEST_COMPRESSION, 9, "Z_BEST_COMPRESSION must be 9");
    assert_eq!(
        Z_DEFAULT_COMPRESSION, -1,
        "Z_DEFAULT_COMPRESSION must be -1, the one negative level that is legal"
    );

    // Not a `zlib.h` constant: `deflate.c` L419 spells the resolved default as a
    // bare `6`. The port names it, so the name has to carry that literal.
    assert_eq!(
        DEF_LEVEL, 6,
        "DEF_LEVEL must be 6, the level Z_DEFAULT_COMPRESSION resolves to"
    );

    // The relation that makes the resolution legal: the default must itself be a
    // level, or `normalize_deflate_level` would reject its own output.
    assert!(
        (Z_NO_COMPRESSION..=Z_BEST_COMPRESSION).contains(&DEF_LEVEL),
        "DEF_LEVEL must lie within Z_NO_COMPRESSION ..= Z_BEST_COMPRESSION"
    );
}

/// Every strategy constant, checked one at a time.
///
/// The numeric *order* matters as much as the values: `deflate.c` L1036, L1079 and
/// L1103 all test `strategy >= Z_HUFFMAN_ONLY`, so the three matching-suppressing
/// strategies have to sort above the two that do not suppress matching.
///
/// Mirrors `zlib.h` L200-L204.
#[test]
fn strategy_constants_are_the_zlib_h_values() {
    assert_eq!(Z_DEFAULT_STRATEGY, 0, "Z_DEFAULT_STRATEGY must be 0");
    assert_eq!(Z_FILTERED, 1, "Z_FILTERED must be 1");
    assert_eq!(Z_HUFFMAN_ONLY, 2, "Z_HUFFMAN_ONLY must be 2");
    assert_eq!(Z_RLE, 3, "Z_RLE must be 3");
    assert_eq!(Z_FIXED, 4, "Z_FIXED must be 4");

    // `deflate.c` L436 tests `strategy < 0 || strategy > Z_FIXED`, so Z_FIXED is
    // simultaneously a strategy and the upper bound of the accepted range. The five
    // values must therefore be contiguous from zero, or the bound would admit a
    // value that names nothing.
    assert_eq!(
        Z_FIXED, 4,
        "Z_FIXED doubles as the strategy range's upper bound in deflate.c L436"
    );

    // Asserted in a `const` block rather than at run time: the relation holds between
    // two constants, so a violation is a fact about the source and deserves to fail
    // the build rather than a test run. The same reasoning applies to the two window
    // and match relations below.
    const {
        assert!(
            Z_HUFFMAN_ONLY < Z_RLE && Z_RLE < Z_FIXED,
            "the strategy values must be contiguous and ascending"
        );
    }
}

/// The compression method constant.
///
/// `zlib.h` L214 states DEFLATE is "the only one supported in this version", which
/// is why this is a single-value check rather than a range.
///
/// Mirrors `zlib.h` L214.
#[test]
fn method_constant_is_the_zlib_h_value() {
    assert_eq!(Z_DEFLATED, 8, "Z_DEFLATED must be 8");
}

/// The `data_type` constants.
///
/// `Z_ASCII` is an alias retained for compatibility with zlib 1.2.2 and earlier, so
/// the assertion is that it *equals* `Z_TEXT`, not merely that it equals `1`.
///
/// Mirrors `zlib.h` L206-L209.
#[test]
fn data_type_constants_are_the_zlib_h_values() {
    assert_eq!(Z_BINARY, 0, "Z_BINARY must be 0");
    assert_eq!(Z_TEXT, 1, "Z_TEXT must be 1");
    assert_eq!(
        Z_ASCII, Z_TEXT,
        "Z_ASCII is an alias of Z_TEXT, not an independent value"
    );
    assert_eq!(Z_UNKNOWN, 2, "Z_UNKNOWN must be 2");
}

/// Every flush constant, checked one at a time.
///
/// `Z_BLOCK` doubles as the upper bound of `deflate`'s flush guard (`flush > Z_BLOCK
/// || flush < 0`, `deflate.c` L985), so its value is what makes `Z_TREES`
/// decompression-only. The seven values must be contiguous from zero for that
/// comparison to mean what it says.
///
/// Mirrors `zlib.h` L172-L178.
#[test]
fn flush_constants_are_the_zlib_h_values() {
    assert_eq!(Z_NO_FLUSH, 0, "Z_NO_FLUSH must be 0");
    assert_eq!(Z_PARTIAL_FLUSH, 1, "Z_PARTIAL_FLUSH must be 1");
    assert_eq!(Z_SYNC_FLUSH, 2, "Z_SYNC_FLUSH must be 2");
    assert_eq!(Z_FULL_FLUSH, 3, "Z_FULL_FLUSH must be 3");
    assert_eq!(Z_FINISH, 4, "Z_FINISH must be 4");
    assert_eq!(Z_BLOCK, 5, "Z_BLOCK must be 5");
    assert_eq!(Z_TREES, 6, "Z_TREES must be 6");

    assert_eq!(
        Z_TREES,
        Z_BLOCK + 1,
        "Z_TREES must sit immediately above deflate's flush ceiling Z_BLOCK"
    );
}

/// The window bounds, and the relation between `DEF_WBITS` and `MAX_WBITS`.
///
/// `DEF_WBITS` is asserted against `MAX_WBITS` as well as against `15`, because
/// `zutil.h` L75-L77 defines it *as* `MAX_WBITS` rather than as a literal. Checking
/// only the number would let the two drift apart while both tests still passed.
///
/// Mirrors `zconf.h` L286-L289 and `zutil.h` L72-L78.
#[test]
fn window_bounds_are_the_shipped_values() {
    assert_eq!(MAX_WBITS, 15, "MAX_WBITS must be 15, a 32 KiB LZ77 window");
    assert_eq!(
        MIN_WBITS, 8,
        "MIN_WBITS must be 8, the smallest exponent deflate.c L435 admits"
    );
    assert_eq!(DEF_WBITS, 15, "DEF_WBITS must be 15");
    assert_eq!(
        DEF_WBITS, MAX_WBITS,
        "zutil.h L75-L77 defines DEF_WBITS AS MAX_WBITS; the two must not drift"
    );

    // `zutil.h` L72-L74 is a `#error` guard, not a comment: a build whose
    // MAX_WBITS leaves 9 ..= 15 does not compile at all.
    assert!(
        (9..=15).contains(&MAX_WBITS),
        "zutil.h L72-L74 refuses to build unless MAX_WBITS is in 9 ..= 15"
    );
    const {
        assert!(
            MIN_WBITS < MAX_WBITS,
            "the window range must be non-empty and correctly ordered"
        );
    }
}

/// The memory-level bounds, and the relation between `DEF_MEM_LEVEL` and
/// `MAX_MEM_LEVEL`.
///
/// **`MAX_MEM_LEVEL` is 9 on this build, not 8.** `zconf.h` L272-L279 picks 8 only
/// under `MAXSEG_64K`, which no supported target defines, so 9 is the shipped value
/// and asserting 8 would be wrong. `DEF_MEM_LEVEL` is the one that is 8
/// (`zutil.h` L80-L84), and the two being different numbers is exactly why they are
/// easy to confuse.
///
/// Mirrors `zconf.h` L272-L279 and `zutil.h` L80-L84.
#[test]
fn memory_level_bounds_are_the_shipped_values() {
    assert_eq!(
        MAX_MEM_LEVEL, 9,
        "MAX_MEM_LEVEL must be 9: zconf.h L272-L279 picks 8 only under MAXSEG_64K"
    );
    assert_eq!(MIN_MEM_LEVEL, 1, "MIN_MEM_LEVEL must be 1");
    assert_eq!(
        DEF_MEM_LEVEL, 8,
        "DEF_MEM_LEVEL must be 8, which is NOT MAX_MEM_LEVEL"
    );

    // `zutil.h` L80-L84 caps the default at the maximum, so this is a relation the
    // reference guarantees rather than a coincidence of two literals.
    assert!(
        (MIN_MEM_LEVEL..=MAX_MEM_LEVEL).contains(&DEF_MEM_LEVEL),
        "DEF_MEM_LEVEL must lie within MIN_MEM_LEVEL ..= MAX_MEM_LEVEL"
    );
    assert_ne!(
        DEF_MEM_LEVEL, MAX_MEM_LEVEL,
        "the default memory level is deliberately one below the maximum"
    );
}

/// The match-length bounds and the remaining `zutil.h` constants.
///
/// `MIN_MATCH` and `MAX_MATCH` are the LZ77 length limits RFC 1951 encodes; they are
/// `usize` here because every use is an index or a length. `PRESET_DICT` is the
/// `FLG` bit of the RFC 1950 header, and the three block-type tags are the two bits
/// RFC 1951 puts at the head of every block.
///
/// Mirrors `zutil.h` L87-L96.
#[test]
fn match_and_block_constants_are_the_zutil_h_values() {
    assert_eq!(MIN_MATCH, 3, "MIN_MATCH must be 3");
    assert_eq!(MAX_MATCH, 258, "MAX_MATCH must be 258");
    const {
        assert!(
            MIN_MATCH < MAX_MATCH,
            "the match-length range must be non-empty and correctly ordered"
        );
    }

    assert_eq!(
        PRESET_DICT, 0x20,
        "PRESET_DICT must be 0x20, the FLG dictionary bit of the zlib header"
    );

    assert_eq!(STORED_BLOCK, 0, "STORED_BLOCK must be 0");
    assert_eq!(STATIC_TREES, 1, "STATIC_TREES must be 1");
    assert_eq!(DYN_TREES, 2, "DYN_TREES must be 2");
}

// ---------------------------------------------------------------------------
// 3.2  The strategy enumeration
// ---------------------------------------------------------------------------

/// All five strategies are constructible and each carries its exact C integer.
///
/// `Strategy::ALL` is the list the differential matrix enumerates, so it has to hold
/// every variant exactly once and in the order the C values run. Its length is
/// asserted as well as its contents: a variant added without being listed would
/// otherwise silently drop out of every sweep that iterates it.
///
/// Mirrors `zlib.h` L200-L204.
#[test]
fn every_strategy_maps_to_its_c_integer() {
    assert_eq!(
        Strategy::ALL.len(),
        5,
        "Strategy::ALL must hold all five strategies"
    );

    assert_eq!(Strategy::Default.as_raw(), Z_DEFAULT_STRATEGY);
    assert_eq!(Strategy::Filtered.as_raw(), Z_FILTERED);
    assert_eq!(Strategy::HuffmanOnly.as_raw(), Z_HUFFMAN_ONLY);
    assert_eq!(Strategy::Rle.as_raw(), Z_RLE);
    assert_eq!(Strategy::Fixed.as_raw(), Z_FIXED);

    // `ALL` is in ascending C-value order, which several sweeps rely on for legible
    // failure output. Zipping against the range rather than counting also asserts that
    // the five values are contiguous, since a gap would exhaust one side early.
    for (expected, strategy) in (Z_DEFAULT_STRATEGY..=Z_FIXED).zip(Strategy::ALL) {
        assert_eq!(
            strategy.as_raw(),
            expected,
            "Strategy::ALL must run in ascending C-value order at {strategy:?}"
        );
    }
}

/// The strategy mapping is injective, and the C spelling of each is distinct.
///
/// Two strategies sharing an integer would make `from_raw` lose information without
/// any single assertion above being able to notice. The comparison is written as an
/// equivalence -- "the raw values are equal exactly when the positions are" -- so it
/// checks well-definedness in the same pass.
#[test]
fn the_strategy_mapping_is_injective() {
    for (left_index, left) in Strategy::ALL.into_iter().enumerate() {
        for (right_index, right) in Strategy::ALL.into_iter().enumerate() {
            assert_eq!(
                left_index == right_index,
                left.as_raw() == right.as_raw(),
                "{left:?} and {right:?} must share an integer only if they are the same strategy"
            );
            assert_eq!(
                left_index == right_index,
                left.c_name() == right.c_name(),
                "{left:?} and {right:?} must share a C name only if they are the same strategy"
            );
        }
    }
}

/// Every strategy survives a round trip through `i32`, in both directions.
///
/// Both directions are needed. `Strategy -> i32 -> Strategy` catches a variant whose
/// `as_raw` and `from_raw` disagree; `i32 -> Strategy -> i32` catches an accepted
/// integer that decodes to the wrong variant. Only the pair rules out a transposed
/// arm.
#[test]
fn every_strategy_round_trips_through_i32() {
    for strategy in Strategy::ALL {
        assert_eq!(
            Strategy::from_raw(strategy.as_raw()),
            Some(strategy),
            "{strategy:?} must survive Strategy -> i32 -> Strategy"
        );
        assert_eq!(
            Strategy::try_from(strategy.as_raw()),
            Ok(strategy),
            "{strategy:?} must survive the TryFrom form of the same trip"
        );
    }

    for raw in Z_DEFAULT_STRATEGY..=Z_FIXED {
        let decoded = Strategy::from_raw(raw);
        assert!(decoded.is_some(), "strategy {raw} must be accepted");
        assert_eq!(
            decoded.map(Strategy::as_raw),
            Some(raw),
            "strategy {raw} must survive i32 -> Strategy -> i32"
        );
    }
}

/// Every integer outside `0 ..= 4` is refused, by both conversion forms and by the
/// real parameter validator.
///
/// `from_raw` reports the refusal as [`None`] and `TryFrom` as
/// [`ReturnCode::STREAM_ERROR`]; both are asserted, because a caller reaching the
/// library through the C ABI only ever sees the latter. `5` and `-1` are the two
/// nearest failures either side of the range, and the integer extremes are included
/// because `deflate.c` L436's `strategy < 0 || strategy > Z_FIXED` is a pair of
/// comparisons that a rewrite could easily turn into arithmetic.
///
/// Mirrors `deflate.c` L436-L437.
#[test]
fn strategy_rejects_every_integer_outside_zero_to_four() {
    let rejected = [-1, 5, 6, 7, 8, 100, -100, i32::MIN, i32::MAX];

    for raw in rejected {
        assert_eq!(
            Strategy::from_raw(raw),
            None,
            "strategy {raw} must not name a strategy"
        );
        assert_eq!(
            Strategy::try_from(raw),
            Err(ReturnCode::STREAM_ERROR),
            "strategy {raw} must be refused with Z_STREAM_ERROR"
        );
        assert_eq!(
            deflate_params_status(6, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL, raw),
            ReturnCode::STREAM_ERROR,
            "deflateInit2_ must refuse strategy {raw}"
        );
        assert_eq!(
            status_of(DeflateConfig::from_raw(
                6,
                Z_DEFLATED,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                raw
            )),
            ReturnCode::STREAM_ERROR,
            "DeflateConfig::from_raw must refuse strategy {raw}"
        );
    }
}

/// Every strategy is accepted by the real initialisation path, and the state keeps
/// the one it was given.
///
/// `zlib.h` L604-L606 records that strategy "affects only the compression ratio",
/// never correctness, which is what makes all five unconditionally admissible. The
/// state's own accessor is checked because a strategy that were silently normalised
/// on the way in would change the emitted bytes without changing any return code.
#[test]
fn every_strategy_is_accepted_and_retained_by_deflate_init2() {
    for strategy in Strategy::ALL {
        // One initialisation per strategy, not two: a successful
        // `deflate_state_parameters` is itself the proof that the configuration was
        // accepted, so calling `deflate_init_status` as well would double the
        // allocation for no additional claim. Strategy does not influence any buffer
        // size, so the cheapest window and memory level are used.
        match deflate_state_parameters(deflate_config(
            6,
            SMALL_WINDOW_BITS,
            MIN_MEM_LEVEL,
            strategy,
        )) {
            Ok((_level, _wrap, _window_bits, _mem_level, held)) => assert_eq!(
                held, strategy,
                "the state must retain {strategy:?} exactly as requested"
            ),
            Err(code) => assert_eq!(
                code,
                ReturnCode::OK,
                "deflateInit2_ unexpectedly refused {strategy:?}"
            ),
        }
    }
}

/// The default strategy is `Z_DEFAULT_STRATEGY`.
///
/// `deflateInit_` supplies it on the caller's behalf (`deflate.c` L381), so the
/// `Default` impl has to agree with that call rather than with the first declared
/// variant by coincidence.
#[test]
fn the_default_strategy_is_z_default_strategy() {
    assert_eq!(Strategy::default(), Strategy::Default);
    assert_eq!(Strategy::default().as_raw(), Z_DEFAULT_STRATEGY);
}

/// The method enumeration admits exactly one value.
///
/// The same three checks the strategy section applies, collapsed to a single-variant
/// enum: `ALL` has one entry, the round trip holds, and the `Default` is that entry.
///
/// Mirrors `zlib.h` L214.
#[test]
fn the_method_enumeration_admits_only_deflated() {
    assert_eq!(Method::ALL.len(), 1, "DEFLATE is the only method");
    assert_eq!(Method::Deflated.as_raw(), Z_DEFLATED);
    assert_eq!(Method::from_raw(Z_DEFLATED), Some(Method::Deflated));
    assert_eq!(Method::try_from(Z_DEFLATED), Ok(Method::Deflated));
    assert_eq!(Method::default(), Method::Deflated);
    assert_eq!(Method::Deflated.c_name(), "Z_DEFLATED");
}

// ---------------------------------------------------------------------------
// 3.3  Level validation
// ---------------------------------------------------------------------------

/// `Z_DEFAULT_COMPRESSION` and every level in `0 ..= 9` are accepted -- all eleven,
/// individually -- and each resolves to the level the encoder will actually use.
///
/// The resolution order is what makes `-1` legal and `-2` not: `deflate.c` L419
/// substitutes the default *before* L435 range-checks the result. Swapping the two
/// steps would reject `Z_DEFAULT_COMPRESSION`, which every caller of `deflateInit`
/// and both one-shot `compress` wrappers depend on.
///
/// Mirrors `deflate.c` L419 and L435.
#[test]
fn every_level_from_default_through_nine_is_accepted() {
    assert_eq!(
        normalize_deflate_level(Z_DEFAULT_COMPRESSION).map(i32::from),
        Ok(DEF_LEVEL),
        "Z_DEFAULT_COMPRESSION must resolve to DEF_LEVEL, which is 6"
    );

    for level in Z_NO_COMPRESSION..=Z_BEST_COMPRESSION {
        let resolved = normalize_deflate_level(level);
        assert_eq!(
            resolved.map(i32::from),
            Ok(level),
            "level {level} must be accepted and must resolve to itself"
        );

        // The level does not influence any buffer size, so the cheapest window and
        // memory level are used; see `SMALL_WINDOW_BITS`.
        assert_eq!(
            deflate_init_status(deflate_config(
                level,
                SMALL_WINDOW_BITS,
                MIN_MEM_LEVEL,
                Strategy::Default
            )),
            ReturnCode::OK,
            "deflateInit2_ must accept level {level}"
        );
    }

    // The default reaches the state as the level it resolves to, not as -1.
    match deflate_state_parameters(deflate_config(
        Z_DEFAULT_COMPRESSION,
        SMALL_WINDOW_BITS,
        MIN_MEM_LEVEL,
        Strategy::Default,
    )) {
        Ok((level, _wrap, _window_bits, _mem_level, _strategy)) => assert_eq!(
            level, DEF_LEVEL,
            "a state built from Z_DEFAULT_COMPRESSION must hold DEF_LEVEL"
        ),
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "deflateInit2_ unexpectedly refused Z_DEFAULT_COMPRESSION"
        ),
    }
}

/// Every level outside `0 ..= 9` -- once the default has been resolved -- is refused
/// with `Z_STREAM_ERROR`.
///
/// `10` and `-2` are the two nearest failures, and they are the pair that localises
/// an off-by-one immediately. The integer extremes matter for a different reason: a
/// range check written with arithmetic rather than comparison can overflow, and both
/// `cargo test` and Miri run debug builds where an overflow panics rather than
/// wrapping quietly.
///
/// Mirrors `deflate.c` L435.
#[test]
fn every_level_outside_zero_to_nine_is_refused() {
    let mut rejected = vec![10, 11, 12, -2, -3, -100, 100];
    rejected.extend_from_slice(&INTEGER_EXTREMES);

    for level in rejected {
        assert_eq!(
            normalize_deflate_level(level),
            Err(ReturnCode::STREAM_ERROR),
            "level {level} must be refused with Z_STREAM_ERROR"
        );
        // A refusal allocates nothing -- validation precedes every allocation
        // (`deflate.c` L434-L440) -- so the library's own defaults are used here
        // rather than the frugal pair, which makes this the exact `deflateInit` call
        // a caller would have made.
        assert_eq!(
            deflate_init_status(deflate_config(
                level,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                Strategy::Default
            )),
            ReturnCode::STREAM_ERROR,
            "deflateInit2_ must refuse level {level}"
        );
    }
}

/// The level boundary pairs, asserted next to each other.
///
/// For every range the two values that matter most are the last accepted and the
/// first rejected; putting them adjacent means a failure message localises the
/// off-by-one without any further investigation.
#[test]
fn the_level_boundaries_are_adjacent_and_correct() {
    assert!(normalize_deflate_level(-2).is_err(), "-2 is not a level");
    assert!(
        normalize_deflate_level(-1).is_ok(),
        "-1 is Z_DEFAULT_COMPRESSION and is a level"
    );
    assert!(normalize_deflate_level(0).is_ok(), "0 is a level");

    assert!(normalize_deflate_level(9).is_ok(), "9 is a level");
    assert!(normalize_deflate_level(10).is_err(), "10 is not a level");
}

// ---------------------------------------------------------------------------
// 3.5  Memory-level validation
// ---------------------------------------------------------------------------

/// Every `memLevel` in `1 ..= 9` is accepted -- all nine, individually -- and the
/// state sizes its hash table from the value it was given.
///
/// `memLevel` never changes the format of what the encoder emits, only how much
/// memory the stream needs and how well it compresses: `hash_bits = memLevel + 7`
/// and `lit_bufsize = 1 << (memLevel + 6)` (`deflate.c` L453 and L464). Recovering
/// it from `hash_bits` therefore asserts the relationship and the retention at once.
///
/// Mirrors the `memLevel < 1 || memLevel > MAX_MEM_LEVEL` term of `deflate.c` L434.
#[test]
fn every_memory_level_from_one_to_nine_is_accepted() {
    // All nine through the allocation-free validator, which is where the rule lives.
    for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
        assert_eq!(
            validate_mem_level(mem_level).map(i32::from),
            Ok(mem_level),
            "memLevel {mem_level} must be accepted and must validate to itself"
        );
    }

    // The two bounds through the real, allocating path. `memLevel` is the one
    // parameter whose value cannot be made cheap -- it *is* the size of the hash
    // table and the symbol buffer, so `MAX_MEM_LEVEL` costs 256 KiB however it is
    // requested. The two bounds are what matter (`DEF_MEM_LEVEL` in between is
    // covered by `the_default_deflate_configuration_is_the_deflate_init_call`), and
    // the window is held at its cheapest so only the memory level contributes; see
    // `SMALL_WINDOW_BITS`.
    for mem_level in [MIN_MEM_LEVEL, MAX_MEM_LEVEL] {
        match deflate_state_parameters(deflate_config(
            6,
            SMALL_WINDOW_BITS,
            mem_level,
            Strategy::Default,
        )) {
            Ok((_level, _wrap, _window_bits, held, _strategy)) => assert_eq!(
                i64::from(held),
                i64::from(mem_level),
                "hash_bits - 7 must recover the requested memLevel {mem_level}"
            ),
            Err(code) => assert_eq!(
                code,
                ReturnCode::OK,
                "deflateInit2_ unexpectedly refused memLevel {mem_level}"
            ),
        }
    }
}

/// Every `memLevel` outside `1 ..= 9` is refused with `Z_STREAM_ERROR`.
///
/// `0` and `10` are the two nearest failures. `0` is the one worth stating out loud:
/// it is the value a zeroed structure carries, so a range check that admitted it
/// would accept a caller who simply forgot to fill the field in.
///
/// Mirrors `deflate.c` L434.
#[test]
fn every_memory_level_outside_one_to_nine_is_refused() {
    let mut rejected = vec![0, 10, 11, 16, -1, -9];
    rejected.extend_from_slice(&INTEGER_EXTREMES);

    for mem_level in rejected {
        assert_eq!(
            validate_mem_level(mem_level),
            Err(ReturnCode::STREAM_ERROR),
            "memLevel {mem_level} must be refused with Z_STREAM_ERROR"
        );
        assert_eq!(
            deflate_init_status(deflate_config(6, MAX_WBITS, mem_level, Strategy::Default)),
            ReturnCode::STREAM_ERROR,
            "deflateInit2_ must refuse memLevel {mem_level}"
        );
    }
}

/// The `memLevel` boundary pairs, asserted next to each other.
#[test]
fn the_memory_level_boundaries_are_adjacent_and_correct() {
    assert!(validate_mem_level(0).is_err(), "0 is not a memLevel");
    assert!(
        validate_mem_level(MIN_MEM_LEVEL).is_ok(),
        "MIN_MEM_LEVEL is a memLevel"
    );

    assert!(
        validate_mem_level(MAX_MEM_LEVEL).is_ok(),
        "MAX_MEM_LEVEL is a memLevel"
    );
    assert!(
        validate_mem_level(MAX_MEM_LEVEL + 1).is_err(),
        "one above MAX_MEM_LEVEL is not a memLevel"
    );
}

// ---------------------------------------------------------------------------
// 3.6  Method validation
// ---------------------------------------------------------------------------

/// `Z_DEFLATED` is accepted and the state records it.
///
/// Mirrors `zlib.h` L214 and `deflate.c` L434.
#[test]
fn the_only_accepted_method_is_deflated() {
    assert_eq!(
        deflate_params_status(6, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL, Z_DEFAULT_STRATEGY),
        ReturnCode::OK,
        "deflateInit2_ must accept Z_DEFLATED"
    );

    // The method influences no buffer size, so the cheapest state is built; see
    // `SMALL_WINDOW_BITS`.
    match deflate_init2(
        deflate_config(6, SMALL_WINDOW_BITS, MIN_MEM_LEVEL, Strategy::Default),
        GlobalAllocator,
    ) {
        Ok(mut state) => {
            assert_eq!(
                state.method(),
                Method::Deflated,
                "the state must record the DEFLATE method"
            );
            common::check_err(
                deflate_end(&mut state),
                "deflateEnd after method inspection",
            );
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "deflateInit2_ unexpectedly refused Z_DEFLATED"
        ),
    }
}

/// Every method other than `Z_DEFLATED` is refused with `Z_STREAM_ERROR`.
///
/// `7` and `9` bracket the accepted value, `0` is what a zeroed field carries, and
/// `16` is the value a caller might reach for by confusing the method parameter with
/// the `+16` gzip offset that belongs to `windowBits`. Because the check is
/// equality rather than a range, the extremes are refused for the same reason as
/// everything else.
///
/// Mirrors `deflate.c` L434 and `zlib.h` L214.
#[test]
fn every_method_other_than_deflated_is_refused() {
    let mut rejected = vec![0, 1, 2, 7, 9, 16, -8, -1];
    rejected.extend_from_slice(&INTEGER_EXTREMES);

    for method in rejected {
        assert_eq!(
            Method::from_raw(method),
            None,
            "method {method} must not name a method"
        );
        assert_eq!(
            Method::try_from(method),
            Err(ReturnCode::STREAM_ERROR),
            "method {method} must be refused with Z_STREAM_ERROR"
        );
        assert_eq!(
            deflate_params_status(6, method, MAX_WBITS, DEF_MEM_LEVEL, Z_DEFAULT_STRATEGY),
            ReturnCode::STREAM_ERROR,
            "deflateInit2_ must refuse method {method}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3.4  windowBits validation -- deflate
// ---------------------------------------------------------------------------

/// Every `windowBits` in `-32 ..= 48` decodes exactly as the deflate table says, and
/// `deflateInit2_` agrees with the decode on every one of them.
///
/// A sweep rather than a list of cases, because `windowBits` is the one parameter
/// whose accepted set is not a single interval: it is three disjoint bands with two
/// deliberate holes in them (`-8` and `24`). Walking the whole span is the only way
/// to be sure nothing has leaked into a gap, and 81 values is nothing to walk.
///
/// The sweep asserts three things, which is what makes a failure diagnosable rather
/// than merely visible:
///
/// 1. the decoded `(container, exponent)` pair equals the table's, for every value;
/// 2. the decoder's status equals the table's verdict, for every value; and
/// 3. every value the table *refuses* is also refused by `deflateInit2_` -- a bound
///    checked in `config` but not consulted by the initialisation path would pass (1)
///    and (2) and fail (3).
///
/// Acceptances are confirmed through `deflateInit2_` by
/// `deflate_init2_applies_the_same_window_bits_rule`, over
/// [`ALLOCATING_WINDOW_BITS`], because an accepted `windowBits` allocates a window of
/// `2^windowBits` bytes while a refusal allocates nothing. See [`SMALL_WINDOW_BITS`]
/// for why that split exists.
///
/// Mirrors `deflate.c` L422-L439.
#[test]
fn the_deflate_window_bits_sweep_matches_the_reference_table() {
    for window_bits in SWEEP_LOW..=SWEEP_HIGH {
        let expected = oracle_deflate_window_bits(window_bits);

        assert_eq!(
            decoded_deflate_window_bits(window_bits),
            expected,
            "deflate windowBits {window_bits} must decode as the reference table says"
        );

        let Some(_accepted) = expected else {
            assert_eq!(
                status_of(decode_deflate_window_bits(window_bits)),
                ReturnCode::STREAM_ERROR,
                "the deflate decoder must refuse windowBits {window_bits}"
            );
            assert_eq!(
                deflate_init_status(deflate_config(
                    6,
                    window_bits,
                    MIN_MEM_LEVEL,
                    Strategy::Default
                )),
                ReturnCode::STREAM_ERROR,
                "deflateInit2_ must refuse windowBits {window_bits} too"
            );
            continue;
        };

        assert_eq!(
            status_of(decode_deflate_window_bits(window_bits)),
            ReturnCode::OK,
            "the deflate decoder must accept windowBits {window_bits}"
        );
    }
}

/// The deflate `windowBits` boundary pairs, each accepted value stated next to the
/// adjacent refusal.
///
/// Six pairs, one per edge of the three bands. The two that are *not* simple
/// off-by-one edges are the ones this test exists for:
///
/// * `-8` is refused while `-9` is accepted, and `24` is refused while `25` is
///   accepted, because `deflate.c` L436 rejects an exponent of 8 for any container
///   but zlib -- only a zlib header can transmit a window size to the decompressor.
///   Both holes sit *inside* their band, so a sweep alone would not make the reason
///   obvious.
/// * `8` is accepted but does **not** stay 8: `deflate.c` L439 promotes it to 9,
///   "until 256-byte window bug fixed".
#[test]
fn the_deflate_window_bits_boundaries_are_adjacent_and_correct() {
    // The zlib band, 8 ..= 15.
    assert!(
        decode_deflate_window_bits(7).is_err(),
        "7 is below the zlib band"
    );
    assert!(
        decode_deflate_window_bits(8).is_ok(),
        "8 opens the zlib band"
    );
    assert!(
        decode_deflate_window_bits(15).is_ok(),
        "15 closes the zlib band"
    );
    assert!(
        decode_deflate_window_bits(16).is_err(),
        "16 is a gzip request with too small an exponent, not a zlib one"
    );

    // The raw band, -15 ..= -9, with the -8 hole.
    assert!(
        decode_deflate_window_bits(-8).is_err(),
        "-8 asks for a 256-byte raw window, which deflate.c L436 refuses"
    );
    assert!(
        decode_deflate_window_bits(-9).is_ok(),
        "-9 opens the raw band"
    );
    assert!(
        decode_deflate_window_bits(-15).is_ok(),
        "-15 closes the raw band"
    );
    assert!(
        decode_deflate_window_bits(-16).is_err(),
        "-16 is below the raw band"
    );

    // The gzip band, 25 ..= 31, with the 24 hole.
    assert!(
        decode_deflate_window_bits(24).is_err(),
        "24 asks for a 256-byte gzip window, refused by the same rule as -8"
    );
    assert!(
        decode_deflate_window_bits(25).is_ok(),
        "25 opens the gzip band"
    );
    assert!(
        decode_deflate_window_bits(31).is_ok(),
        "31 closes the gzip band"
    );
    assert!(
        decode_deflate_window_bits(32).is_err(),
        "32 is above the gzip band; deflate has no +32 mode"
    );
}

/// Deflate promotes a request for `8` to `9`, and only for the zlib container.
///
/// The promotion is silent in the reference and silent in the port, deliberately:
/// reporting it would be an observable difference in `deflateInit2_`'s return value.
/// Its caller-visible consequence is documented at `zlib.h` L562-L568 -- an encoder
/// asked for `8` writes a header advertising a 512-byte window, so a decoder
/// initialised with `8` refuses the stream. That refusal is asserted in
/// `a_zlib_header_window_size_is_adopted_or_refused` below, which closes the loop.
///
/// Mirrors `deflate.c` L436 and L439.
#[test]
fn deflate_promotes_a_request_for_eight_to_nine() {
    assert_eq!(
        decode_deflate_window_bits(8),
        Ok((Wrap::Zlib, 9)),
        "a zlib request for 8 must come back as 9"
    );
    assert_eq!(
        decode_deflate_window_bits(9),
        Ok((Wrap::Zlib, 9)),
        "a zlib request for 9 must also be 9, so the two requests coincide"
    );

    // The promotion does not rescue the other two containers: their exponent-8
    // requests are refused outright rather than promoted.
    assert_eq!(
        decode_deflate_window_bits(-8),
        Err(ReturnCode::STREAM_ERROR)
    );
    assert_eq!(
        decode_deflate_window_bits(24),
        Err(ReturnCode::STREAM_ERROR)
    );

    // And it reaches the live state, not just the decoder.
    match deflate_state_parameters(deflate_config(6, 8, MIN_MEM_LEVEL, Strategy::Default)) {
        Ok((_level, wrap, window_bits, _mem_level, _strategy)) => {
            assert_eq!(window_bits, 9, "the state's window exponent must be 9");
            assert_eq!(wrap, Some(Wrap::Zlib), "the container must still be zlib");
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "deflateInit2_ unexpectedly refused windowBits 8"
        ),
    }
}

/// `deflateInit2_` accepts each corner `windowBits` and stores the container and
/// exponent the decoder reported.
///
/// Two claims the sweep above cannot make. The decoder could be right while the
/// initialisation path dropped the container on the floor, and both would still return
/// `Z_OK`; and a decoder that agreed about the exponent could still fail to allocate
/// the window it describes. Driving the real path and then reading the state's own
/// accessors back rules out both.
///
/// Restricted to [`ALLOCATING_WINDOW_BITS`] -- one request per container, the `8`
/// promotion, and the largest window once -- because the exponent alone determines what
/// a state costs to build, and covering the largest exponent three times proves nothing
/// the first does not. See [`SMALL_WINDOW_BITS`].
///
/// Mirrors `deflate.c` L422-L439 followed by L440-L530.
#[test]
fn deflate_init2_applies_the_same_window_bits_rule() {
    for window_bits in ALLOCATING_WINDOW_BITS {
        let Some((expected_wrap, expected_exponent)) = oracle_deflate_window_bits(window_bits)
        else {
            // Every member of the corner set is an accepted value by construction, so
            // reaching this arm means the set and the table have drifted apart.
            assert_eq!(
                decoded_deflate_window_bits(window_bits),
                oracle_deflate_window_bits(window_bits),
                "ALLOCATING_WINDOW_BITS must hold only accepted values, and \
                 {window_bits} is not one"
            );
            continue;
        };

        match deflate_state_parameters(deflate_config(
            6,
            window_bits,
            MIN_MEM_LEVEL,
            Strategy::Default,
        )) {
            Ok((_level, wrap, exponent, _mem_level, _strategy)) => {
                assert_eq!(
                    wrap,
                    Some(expected_wrap),
                    "windowBits {window_bits} must select {expected_wrap:?}"
                );
                assert_eq!(
                    i64::from(exponent),
                    i64::from(expected_exponent),
                    "windowBits {window_bits} must yield exponent {expected_exponent}"
                );
            }
            Err(code) => assert_eq!(
                code,
                ReturnCode::OK,
                "deflateInit2_ unexpectedly refused windowBits {window_bits}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// 3.4  windowBits validation -- inflate
// ---------------------------------------------------------------------------

/// Every `windowBits` in `-32 ..= 48` decodes exactly as the inflate table says, and
/// both `inflateInit2_` and `inflateReset2` agree with the decode.
///
/// The same sweep as the deflate side and deliberately a *separate* one, because the
/// two directions do not agree: inflate accepts `-8`, accepts `0`, and has a `+32`
/// band that deflate has no analogue for. Reproducing one set of rules for both is
/// the single most likely defect in this area, and a shared sweep would hide it.
///
/// `inflateReset2` is included because it is the function the decoder is literally a
/// port of; `inflateInit2_` reaches the same code only by delegating to it
/// (`inflate.c` L206). Asserting both proves the delegation is intact.
///
/// Mirrors `inflate.c` L145-L161.
#[test]
fn the_inflate_window_bits_sweep_matches_the_reference_table() {
    for window_bits in SWEEP_LOW..=SWEEP_HIGH {
        let expected = oracle_inflate_window_bits(window_bits);

        assert_eq!(
            decoded_inflate_window_bits(window_bits),
            expected,
            "inflate windowBits {window_bits} must decode as the reference table says"
        );

        let expected_status = if expected.is_some() {
            ReturnCode::OK
        } else {
            ReturnCode::STREAM_ERROR
        };

        assert_eq!(
            status_of(decode_inflate_window_bits(window_bits)),
            expected_status,
            "the inflate decoder's status for windowBits {window_bits} must match the table"
        );
        assert_eq!(
            inflate_init_status(window_bits),
            expected_status,
            "inflateInit2_'s status for windowBits {window_bits} must match the decoder's"
        );
        assert_eq!(
            inflate_reset2_status(window_bits),
            expected_status,
            "inflateReset2's status for windowBits {window_bits} must match the decoder's"
        );
    }
}

/// The inflate `windowBits` boundary pairs, each accepted value stated next to the
/// adjacent refusal.
///
/// The pairs that differ from the deflate side are the point: `-8` is **accepted**
/// here, and `0` is accepted where deflate refuses it. Everything from `48` up is
/// refused because `inflate.c` L154-L155 masks the exponent out of the low nibble
/// only for requests below 48, so a larger request keeps its full value and fails the
/// range check -- which is also what keeps the wrap request from exceeding 7.
#[test]
fn the_inflate_window_bits_boundaries_are_adjacent_and_correct() {
    // The zlib band, with the 0 exemption immediately below it.
    assert!(
        decode_inflate_window_bits(0).is_ok(),
        "0 means take the window size from the header"
    );
    assert!(
        decode_inflate_window_bits(1).is_err(),
        "1 is a bad window size; test/infcover.c L371 pins this"
    );
    assert!(
        decode_inflate_window_bits(7).is_err(),
        "7 is still too small"
    );
    assert!(
        decode_inflate_window_bits(8).is_ok(),
        "8 opens the zlib band"
    );
    assert!(
        decode_inflate_window_bits(15).is_ok(),
        "15 closes the zlib band"
    );

    // The raw band -- note -8 is accepted, unlike on the deflate side.
    assert!(
        decode_inflate_window_bits(-8).is_ok(),
        "-8 is a legal raw request for inflate, where deflate refuses it"
    );
    assert!(
        decode_inflate_window_bits(-15).is_ok(),
        "-15 closes the raw band"
    );
    assert!(
        decode_inflate_window_bits(-16).is_err(),
        "-16 is below the raw band"
    );

    // The gzip band, 16 then 24 ..= 31.
    assert!(
        decode_inflate_window_bits(16).is_ok(),
        "16 is gzip with a deferred exponent"
    );
    assert!(
        decode_inflate_window_bits(23).is_err(),
        "23 is a gzip request with too small an exponent"
    );
    assert!(
        decode_inflate_window_bits(24).is_ok(),
        "24 opens the explicit gzip band"
    );
    assert!(
        decode_inflate_window_bits(31).is_ok(),
        "31 closes the explicit gzip band"
    );

    // The automatic-detection band, 32 then 40 ..= 47.
    assert!(
        decode_inflate_window_bits(32).is_ok(),
        "32 is automatic detection with a deferred exponent"
    );
    assert!(
        decode_inflate_window_bits(39).is_err(),
        "39 asks for detection with too small an exponent"
    );
    assert!(
        decode_inflate_window_bits(40).is_ok(),
        "40 opens the automatic-detection band"
    );
    assert!(
        decode_inflate_window_bits(47).is_ok(),
        "47 closes the automatic-detection band"
    );
    assert!(
        decode_inflate_window_bits(48).is_err(),
        "48 is the first value inflate.c L154 declines to mask"
    );
}

/// `windowBits == 0` is accepted and defers the window size to the stream header.
///
/// `test/infcover.c` L402 drives exactly this: `inf("8 99", "set window size from
/// header", 0, 0, 0, Z_OK)`. The exemption is what `inflate.c` L160's
/// `if (windowBits && ...)` is for, and it is the one place where a zero parameter is
/// meaningful rather than a caller's oversight -- which is precisely why a rewrite
/// might reject it.
#[test]
fn inflate_accepts_window_bits_zero_and_defers_to_the_header() {
    assert_eq!(
        decode_inflate_window_bits(0),
        Ok((InflateWrap::Zlib, 0)),
        "0 must decode to a zlib request with a deferred exponent"
    );
    assert_eq!(inflate_init_status(0), ReturnCode::OK);
    assert_eq!(inflate_reset2_status(0), ReturnCode::OK);

    match InflateConfig::new(0).validate() {
        Ok(validated) => {
            assert!(
                validated.window_size_from_header(),
                "a deferred exponent must report window_size_from_header()"
            );
            assert_eq!(validated.wrap, InflateWrap::Zlib);
        }
        Err(code) => assert_eq!(code, ReturnCode::OK, "windowBits 0 must validate"),
    }

    // And the same deferral applies through the +16 and +32 offsets, which are those
    // offsets applied to a windowBits of zero.
    for window_bits in [16, 32] {
        match InflateConfig::new(window_bits).validate() {
            Ok(validated) => assert!(
                validated.window_size_from_header(),
                "windowBits {window_bits} must also defer the exponent"
            ),
            Err(code) => assert_eq!(
                code,
                ReturnCode::OK,
                "windowBits {window_bits} must validate"
            ),
        }
    }
}

/// `windowBits == 1` is refused, and `windowBits == 47` is accepted.
///
/// Two vectors lifted straight out of the existing C harness, and the pair is worth a
/// test of its own because a failure of either would be catastrophic in an obscure
/// way. `test/infcover.c` L371 asserts the refusal --
/// `inf("", "bad window size", 0, 1, 0, Z_STREAM_ERROR)` -- and nearly every
/// gzip-header test in that same file initialises with `inflateInit2(&strm, 47)`. If
/// the automatic-detection band were refused, those tests would fail wholesale with
/// a status that pointed nowhere near the cause; this is where that root cause is
/// obvious.
#[test]
fn the_infcover_window_size_vectors_hold() {
    assert_eq!(
        decode_inflate_window_bits(1),
        Err(ReturnCode::STREAM_ERROR),
        "windowBits 1 must be refused: test/infcover.c L371"
    );
    assert_eq!(inflate_init_status(1), ReturnCode::STREAM_ERROR);
    assert_eq!(inflate_reset2_status(1), ReturnCode::STREAM_ERROR);

    assert_eq!(
        decode_inflate_window_bits(47),
        Ok((InflateWrap::ZlibOrGzip, 15)),
        "windowBits 47 must select automatic detection with a 32 KiB window"
    );
    assert_eq!(inflate_init_status(47), ReturnCode::OK);
    assert_eq!(inflate_reset2_status(47), ReturnCode::OK);
}

/// `inflateInit2_` accepts the configuration `inflateInit` builds.
///
/// Pinned on its own so that a failure of `inflate_reset2_status` -- which starts by
/// building exactly this state -- is attributed to the value under test rather than
/// to the harness.
#[test]
fn inflate_init2_accepts_the_default_configuration() {
    assert_eq!(
        InflateConfig::default().window_bits,
        DEF_WBITS,
        "the default inflate configuration must request DEF_WBITS"
    );
    assert_eq!(
        inflate_init_status(DEF_WBITS),
        ReturnCode::OK,
        "inflateInit2_ must accept DEF_WBITS"
    );
}

// ---------------------------------------------------------------------------
// 3.4  Container classification
// ---------------------------------------------------------------------------

/// The container each representative `windowBits` selects, and the encoding each
/// container carries in a live state.
///
/// The two encodings are deliberately different and both are ABI-visible through the
/// state a caller can inspect: deflate stores `0`, `1` or `2` (`deflate.c` L391, L423
/// and L430) while inflate stores a bitmask -- `0`, `5`, `6` or `7`, being the
/// zlib-header, gzip-header and verify-check bits combined (`inflate.c` L148 and
/// L152). Asserting the numbers, not just the variants, is what keeps the port's
/// state layout compatible with what `test/infcover.c` observes.
#[test]
fn each_container_is_selected_and_encoded_as_the_reference_does() {
    // Deflate: raw, zlib, gzip.
    assert_eq!(
        decode_deflate_window_bits(-15).map(|(wrap, _)| wrap),
        Ok(Wrap::None)
    );
    assert_eq!(
        decode_deflate_window_bits(15).map(|(wrap, _)| wrap),
        Ok(Wrap::Zlib)
    );
    assert_eq!(
        decode_deflate_window_bits(31).map(|(wrap, _)| wrap),
        Ok(Wrap::Gzip)
    );

    assert_eq!(Wrap::None.as_deflate_wrap(), 0);
    assert_eq!(Wrap::Zlib.as_deflate_wrap(), 1);
    assert_eq!(Wrap::Gzip.as_deflate_wrap(), 2);

    // Only the zlib container accumulates an Adler-32 and only gzip a CRC-32, which
    // is the `wrap == 1` / `wrap == 2` test at `deflate.c` L228.
    assert!(!Wrap::None.is_wrapped());
    assert!(Wrap::Zlib.computes_adler32() && !Wrap::Zlib.computes_crc32());
    assert!(Wrap::Gzip.computes_crc32() && !Wrap::Gzip.computes_adler32());

    // Inflate: raw, zlib, gzip-only, automatic detection.
    assert_eq!(
        decode_inflate_window_bits(-15).map(|(wrap, _)| wrap),
        Ok(InflateWrap::None)
    );
    assert_eq!(
        decode_inflate_window_bits(15).map(|(wrap, _)| wrap),
        Ok(InflateWrap::Zlib)
    );
    assert_eq!(
        decode_inflate_window_bits(31).map(|(wrap, _)| wrap),
        Ok(InflateWrap::Gzip)
    );
    assert_eq!(
        decode_inflate_window_bits(47).map(|(wrap, _)| wrap),
        Ok(InflateWrap::ZlibOrGzip)
    );

    assert_eq!(InflateWrap::None.as_inflate_wrap(), 0);
    assert_eq!(InflateWrap::Zlib.as_inflate_wrap(), 5);
    assert_eq!(InflateWrap::Gzip.as_inflate_wrap(), 6);
    assert_eq!(InflateWrap::ZlibOrGzip.as_inflate_wrap(), 7);

    // Which headers each request will read, and whether the trailer is verified.
    assert!(InflateWrap::Zlib.allows_zlib_header() && !InflateWrap::Zlib.allows_gzip_header());
    assert!(InflateWrap::Gzip.allows_gzip_header() && !InflateWrap::Gzip.allows_zlib_header());
    assert!(
        InflateWrap::ZlibOrGzip.allows_zlib_header()
            && InflateWrap::ZlibOrGzip.allows_gzip_header()
    );
    assert!(!InflateWrap::None.verifies_check_value());
    assert!(InflateWrap::Zlib.verifies_check_value());

    // Automatic detection has no single container yet; the other three do.
    assert_eq!(InflateWrap::ZlibOrGzip.determinate_wrap(), None);
    assert_eq!(InflateWrap::None.determinate_wrap(), Some(Wrap::None));
    assert_eq!(InflateWrap::Zlib.determinate_wrap(), Some(Wrap::Zlib));
    assert_eq!(InflateWrap::Gzip.determinate_wrap(), Some(Wrap::Gzip));
}

/// Classifying a `windowBits` is a pure function of the argument.
///
/// Both decoders take an `i32` and return a value; neither reads nor writes anything
/// else. That is worth asserting rather than assuming, because a decoder that cached
/// state -- a memoised table, a lazily initialised bound -- would make the very first
/// call in a process behave differently from every later one, and every sweep above
/// would still pass while a real caller saw something else. Calling twice and
/// comparing, over the whole sweep, rules that out.
#[test]
fn window_bits_classification_is_a_pure_function() {
    for window_bits in SWEEP_LOW..=SWEEP_HIGH {
        assert_eq!(
            decode_deflate_window_bits(window_bits),
            decode_deflate_window_bits(window_bits),
            "the deflate decode of {window_bits} must not depend on call order"
        );
        assert_eq!(
            decode_inflate_window_bits(window_bits),
            decode_inflate_window_bits(window_bits),
            "the inflate decode of {window_bits} must not depend on call order"
        );
        assert_eq!(
            validate_inflate_back_window_bits(window_bits),
            validate_inflate_back_window_bits(window_bits),
            "the inflateBack check of {window_bits} must not depend on call order"
        );

        // The three rules are also independent of each other: reordering the calls
        // must not change any of the answers.
        let inflate_first = decode_inflate_window_bits(window_bits);
        let deflate_second = decode_deflate_window_bits(window_bits);
        assert_eq!(inflate_first, decode_inflate_window_bits(window_bits));
        assert_eq!(deflate_second, decode_deflate_window_bits(window_bits));
    }
}

/// `inflateBackInit_` applies the narrowest of the three `windowBits` rules: `8 ..=
/// 15` and nothing else.
///
/// None of `inflateInit2_`'s conveniences survive here, and the reason is structural
/// rather than an oversight: the caller supplies the window buffer itself, so the
/// library can neither pick a size nor grow one later. `infback.c` L22-L23 states the
/// contract and L33-L35 enforces it with a plain pair of comparisons -- so a negative
/// request, a `+16` gzip request, a `+32` detection request and a `0` deferral are
/// all refused, including the three values the *other* two rules accept.
///
/// Mirrors `infback.c` L33-L35.
#[test]
fn inflate_back_window_bits_is_the_narrowest_rule() {
    for window_bits in SWEEP_LOW..=SWEEP_HIGH {
        let expected = if (MIN_WBITS..=MAX_WBITS).contains(&window_bits) {
            Ok(window_bits)
        } else {
            Err(ReturnCode::STREAM_ERROR)
        };

        assert_eq!(
            validate_inflate_back_window_bits(window_bits).map(i32::from),
            expected,
            "inflateBackInit_ must accept exactly 8 ..= 15, and {window_bits} is not special"
        );
    }

    // The three values the other rules accept and this one does not, stated
    // explicitly because they are where a shared implementation would show up.
    assert!(
        validate_inflate_back_window_bits(0).is_err(),
        "inflateBack cannot defer the window size: there is no header to read it from"
    );
    assert!(
        validate_inflate_back_window_bits(-15).is_err(),
        "inflateBack decodes raw deflate unconditionally, so there is no raw request"
    );
    assert!(
        validate_inflate_back_window_bits(47).is_err(),
        "inflateBack has no automatic-detection mode"
    );

    assert!(validate_inflate_back_window_bits(7).is_err());
    assert!(validate_inflate_back_window_bits(8).is_ok());
    assert!(validate_inflate_back_window_bits(15).is_ok());
    assert!(validate_inflate_back_window_bits(16).is_err());
}

/// A zlib header's advertised window size is adopted when none was requested and
/// refused when it exceeds the request.
///
/// This is the other half of the deflate `8 -> 9` promotion, and the pairing is the
/// point. An encoder asked for `windowBits = 8` writes a header advertising `CINFO =
/// 1`, a 512-byte window; a decoder initialised with `windowBits = 8` reads that back
/// as `len = CINFO + 8 = 9`, finds `len > wbits`, and refuses to grow its window --
/// so **pairing `8` with `8` across a round trip cannot work.** `zlib.h` L869-L873
/// documents the refusal and `test/infcover.c` L403 drives it:
/// `inf("78 9c", "bad zlib window size", 0, 8, 0, Z_DATA_ERROR)`.
///
/// Note the status: [`ReturnCode::DATA_ERROR`], not [`ReturnCode::STREAM_ERROR`]. The
/// parameters were fine; the *stream* is the problem.
///
/// Mirrors `inflate.c` L539-L546.
#[test]
fn a_zlib_header_window_size_is_adopted_or_refused() {
    // Deferred request: the header's exponent is adopted, whatever it is within range.
    match InflateConfig::new(0).validate() {
        Ok(deferred) => {
            for header_exponent in MIN_WBITS..=MAX_WBITS {
                let Ok(narrowed) = u8::try_from(header_exponent) else {
                    continue;
                };
                assert_eq!(
                    deferred
                        .resolve_zlib_header_window_bits(narrowed)
                        .map(i32::from),
                    Ok(header_exponent),
                    "a deferred request must adopt the header's exponent {header_exponent}"
                );
            }

            // A header advertising more than MAX_WBITS is refused even when nothing
            // was requested: `inflate.c` L542's first comparison is `len > 15`.
            assert_eq!(
                deferred.resolve_zlib_header_window_bits(16),
                Err(ReturnCode::DATA_ERROR),
                "a header above MAX_WBITS must be refused with Z_DATA_ERROR"
            );
        }
        Err(code) => assert_eq!(code, ReturnCode::OK, "windowBits 0 must validate"),
    }

    // Explicit request of 8 against a header advertising 9: the infcover vector.
    match InflateConfig::new(8).validate() {
        Ok(requested) => {
            assert!(
                !requested.window_size_from_header(),
                "an explicit request must not report a deferred exponent"
            );
            assert_eq!(
                requested.resolve_zlib_header_window_bits(8).map(i32::from),
                Ok(8),
                "a header that fits the request is accepted"
            );
            assert_eq!(
                requested.resolve_zlib_header_window_bits(9),
                Err(ReturnCode::DATA_ERROR),
                "test/infcover.c L403: a 512-byte stream into an 8-bit window is Z_DATA_ERROR"
            );
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "windowBits 8 must validate for inflate"
        ),
    }

    // The largest request accepts every legal header, which is why DEF_WBITS is the
    // default: it is the only value that never refuses a conforming stream.
    match InflateConfig::default().validate() {
        Ok(widest) => {
            for header_exponent in MIN_WBITS..=MAX_WBITS {
                let Ok(narrowed) = u8::try_from(header_exponent) else {
                    continue;
                };
                assert_eq!(
                    widest
                        .resolve_zlib_header_window_bits(narrowed)
                        .map(i32::from),
                    Ok(MAX_WBITS),
                    "the widest window keeps its own exponent and accepts header {header_exponent}"
                );
            }
        }
        Err(code) => assert_eq!(code, ReturnCode::OK, "DEF_WBITS must validate"),
    }

    // A gzip header carries no window size, so there is nothing to negotiate: a
    // deferred request simply takes the maximum (`inflate.c` L514-L515).
    match InflateConfig::new(32).validate() {
        Ok(deferred_gzip) => assert_eq!(
            i32::from(deferred_gzip.resolve_gzip_window_bits()),
            MAX_WBITS,
            "a deferred gzip request must resolve to MAX_WBITS"
        ),
        Err(code) => assert_eq!(code, ReturnCode::OK, "windowBits 32 must validate"),
    }
    match InflateConfig::new(25).validate() {
        Ok(explicit_gzip) => assert_eq!(
            i32::from(explicit_gzip.resolve_gzip_window_bits()),
            9,
            "an explicit gzip request must keep its own exponent"
        ),
        Err(code) => assert_eq!(code, ReturnCode::OK, "windowBits 25 must validate"),
    }
}

// ---------------------------------------------------------------------------
// 3.7  Combination sweeps
// ---------------------------------------------------------------------------

/// Every combination of corner parameters is accepted: 5 levels x 6 `windowBits` x 3
/// `memLevel` x 5 strategies = 450 configurations.
///
/// The four fields are validated by independent range checks, so a cross product over
/// their corners is what proves the checks are genuinely independent -- that no
/// combination is refused because two individually-legal values were tested against
/// each other. Every configuration here is one the reference accepts, so a single
/// refusal is a defect.
///
/// The sweep runs through the allocation-free path (`DeflateConfig::validate`) rather
/// than through `deflate_init2`, and that is a deliberate division of labour: 450
/// initialisations would each allocate a window, a hash table and a symbol buffer,
/// which is wasted work when what is under test is a conjunction of integer
/// comparisons. `deflate_init2` is exercised over a smaller set by
/// `the_corner_combinations_are_accepted_by_deflate_init2` below, and over the whole
/// `windowBits` span by the sweeps in section 3.4. See `LEVEL_CORNERS` for why the
/// corner sets are bounded rather than exhaustive.
#[test]
fn every_corner_combination_is_accepted() {
    let mut accepted = 0_u32;

    for level in LEVEL_CORNERS {
        for window_bits in WINDOW_BITS_CORNERS {
            for mem_level in MEM_LEVEL_CORNERS {
                for strategy in Strategy::ALL {
                    let config = deflate_config(level, window_bits, mem_level, strategy);

                    match config.validate() {
                        Ok(validated) => {
                            // The resolved configuration must agree with the request
                            // on every field the request pinned down.
                            assert_eq!(validated.method, Method::Deflated);
                            assert_eq!(validated.strategy, strategy);
                            assert_eq!(i32::from(validated.mem_level), mem_level);
                            assert_eq!(
                                Some((validated.wrap, i32::from(validated.window_bits))),
                                oracle_deflate_window_bits(window_bits),
                                "level {level}, windowBits {window_bits}, memLevel \
                                 {mem_level}, {strategy:?}: the container or exponent is wrong"
                            );
                            let expected_level = if level == Z_DEFAULT_COMPRESSION {
                                DEF_LEVEL
                            } else {
                                level
                            };
                            assert_eq!(i32::from(validated.level), expected_level);
                        }
                        Err(code) => assert_eq!(
                            code,
                            ReturnCode::OK,
                            "level {level}, windowBits {window_bits}, memLevel {mem_level}, \
                             {strategy:?} must be accepted"
                        ),
                    }

                    // The raw-integer form of the same call must agree.
                    assert_eq!(
                        deflate_params_status(
                            level,
                            Z_DEFLATED,
                            window_bits,
                            mem_level,
                            strategy.as_raw()
                        ),
                        ReturnCode::OK,
                        "the raw-integer form must accept the same combination"
                    );

                    accepted += 1;
                }
            }
        }
    }

    assert_eq!(
        accepted, 450,
        "the sweep must cover 5 x 6 x 3 x 5 = 450 configurations"
    );
}

/// The corner combinations survive the real, allocating initialisation path.
///
/// A reduced cross product on purpose. What the 450-case sweep above proves is that the
/// four range checks are independent; what this proves is that the initialisation path
/// consults them and can actually build the state each one describes -- a much smaller
/// claim per configuration, so it needs far fewer of them.
///
/// The two dimensions that drive allocation size are pinned to their cheapest useful
/// values: one `windowBits` per container at the smallest exponent, and `memLevel` at
/// its minimum. Both are swept at full range where they are the thing under test --
/// `deflate_init2_applies_the_same_window_bits_rule` and
/// `every_memory_level_from_one_to_nine_is_accepted` -- so nothing is lost here beyond
/// combinations of two independently verified bounds. The two dimensions that cost
/// nothing to vary, level and strategy, are covered in full.
///
/// That leaves 5 x 3 x 5 = 75 states of roughly 3 KiB each. See [`SMALL_WINDOW_BITS`]
/// for the cost model and `LEVEL_CORNERS` for the bounding rule.
#[test]
fn the_corner_combinations_are_accepted_by_deflate_init2() {
    for level in LEVEL_CORNERS {
        for window_bits in [
            -SMALL_WINDOW_BITS,
            SMALL_WINDOW_BITS,
            SMALL_WINDOW_BITS + 16,
        ] {
            for strategy in Strategy::ALL {
                assert_eq!(
                    deflate_init_status(deflate_config(
                        level,
                        window_bits,
                        MIN_MEM_LEVEL,
                        strategy
                    )),
                    ReturnCode::OK,
                    "deflateInit2_ must accept level {level}, windowBits {window_bits}, \
                     memLevel {MIN_MEM_LEVEL}, {strategy:?}"
                );
            }
        }
    }
}

/// One field out of range at a time, with the other three held at valid defaults.
///
/// The complement of the sweep above, and the reason it is a separate test: a
/// combined sweep that fails tells you *a* configuration was mishandled, not *which
/// field's* bound was wrong. Stepping one field just past each of its boundaries
/// while the rest stay valid localises the defect to a single validator, and the
/// assertion message names it.
///
/// Every value below is the first refusal on its side of a boundary, plus the two
/// integer extremes, plus -- for `windowBits` -- the two interior holes that no other
/// field has.
///
/// Mirrors `deflate.c` L434-L438, whose five terms are exactly the five groups here.
#[test]
fn one_bad_field_at_a_time_is_refused() {
    // The configuration every group starts from, which must itself be accepted or
    // the isolation is meaningless.
    assert_eq!(
        deflate_params_status(
            DEF_LEVEL,
            Z_DEFLATED,
            MAX_WBITS,
            DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY
        ),
        ReturnCode::OK,
        "the baseline configuration must be valid before any field is perturbed"
    );

    // Group 1: level, `level < 0 || level > 9` after the default is resolved.
    for level in [-2, -3, 10, 11, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_params_status(
                level,
                Z_DEFLATED,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY
            ),
            ReturnCode::STREAM_ERROR,
            "level {level} alone must refuse the configuration"
        );
    }

    // Group 2: method, `method != Z_DEFLATED`.
    for method in [0, 7, 9, 16, -8, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_params_status(
                DEF_LEVEL,
                method,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY
            ),
            ReturnCode::STREAM_ERROR,
            "method {method} alone must refuse the configuration"
        );
    }

    // Group 3: windowBits, including the two interior holes at -8 and 24.
    for window_bits in [0, 7, 16, 32, -8, 24, -16, 48, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_params_status(
                DEF_LEVEL,
                Z_DEFLATED,
                window_bits,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY
            ),
            ReturnCode::STREAM_ERROR,
            "windowBits {window_bits} alone must refuse the configuration"
        );
    }

    // Group 4: memLevel, `memLevel < 1 || memLevel > MAX_MEM_LEVEL`.
    for mem_level in [0, -1, MAX_MEM_LEVEL + 1, 16, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_params_status(
                DEF_LEVEL,
                Z_DEFLATED,
                MAX_WBITS,
                mem_level,
                Z_DEFAULT_STRATEGY
            ),
            ReturnCode::STREAM_ERROR,
            "memLevel {mem_level} alone must refuse the configuration"
        );
    }

    // Group 5: strategy, `strategy < 0 || strategy > Z_FIXED`.
    for strategy in [-1, Z_FIXED + 1, 100, i32::MIN, i32::MAX] {
        assert_eq!(
            deflate_params_status(DEF_LEVEL, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL, strategy),
            ReturnCode::STREAM_ERROR,
            "strategy {strategy} alone must refuse the configuration"
        );
    }
}

// ---------------------------------------------------------------------------
// 3.8  Defaults
// ---------------------------------------------------------------------------

/// The default deflate configuration is the one `deflateInit` builds.
///
/// `deflateInit_` is `deflateInit2_` with four of its five parameters supplied on the
/// caller's behalf -- `Z_DEFLATED`, `MAX_WBITS`, `DEF_MEM_LEVEL` and
/// `Z_DEFAULT_STRATEGY` -- and the level passed straight through (`deflate.c`
/// L379-L383). Each of the four is asserted against its *named constant* rather than
/// against a literal, so a constant that moved would fail here as well as in section
/// 3.1 instead of quietly taking the default with it.
///
/// This is also the configuration both one-shot `compress` wrappers use, since both
/// go through `deflateInit` (`compress.c` L42, reached from L80 and L84).
///
/// Mirrors `deflate.c` L379-L383.
#[test]
fn the_default_deflate_configuration_is_the_deflate_init_call() {
    let config = DeflateConfig::default();

    assert_eq!(
        config.level, Z_DEFAULT_COMPRESSION,
        "the default level must be Z_DEFAULT_COMPRESSION"
    );
    assert_eq!(
        config.method,
        Method::Deflated,
        "the default method must be Z_DEFLATED"
    );
    assert_eq!(
        config.method.as_raw(),
        Z_DEFLATED,
        "the default method must carry the integer 8"
    );
    assert_eq!(
        config.window_bits, MAX_WBITS,
        "the default windowBits must be MAX_WBITS"
    );
    assert_eq!(
        config.window_bits, DEF_WBITS,
        "MAX_WBITS is also DEF_WBITS, so the two spellings must agree"
    );
    assert_eq!(
        config.mem_level, DEF_MEM_LEVEL,
        "the default memLevel must be DEF_MEM_LEVEL"
    );
    assert_eq!(
        config.strategy,
        Strategy::Default,
        "the default strategy must be Z_DEFAULT_STRATEGY"
    );
    assert_eq!(config.strategy.as_raw(), Z_DEFAULT_STRATEGY);

    // `new` is the same call parameterised by level, so the two must coincide at the
    // default level and differ only in `level` elsewhere.
    assert_eq!(config, DeflateConfig::new(Z_DEFAULT_COMPRESSION));
    for level in Z_NO_COMPRESSION..=Z_BEST_COMPRESSION {
        let explicit = DeflateConfig::new(level);
        assert_eq!(explicit.level, level);
        assert_eq!(explicit.method, config.method);
        assert_eq!(explicit.window_bits, config.window_bits);
        assert_eq!(explicit.mem_level, config.mem_level);
        assert_eq!(explicit.strategy, config.strategy);
    }

    // And the default validates, resolving the level to DEF_LEVEL and the container
    // to zlib.
    match config.validate() {
        Ok(validated) => {
            assert_eq!(i32::from(validated.level), DEF_LEVEL);
            assert_eq!(validated.wrap, Wrap::Zlib);
            assert_eq!(i32::from(validated.window_bits), MAX_WBITS);
            assert_eq!(i32::from(validated.mem_level), DEF_MEM_LEVEL);
            assert_eq!(validated.strategy, Strategy::Default);
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "the default deflate configuration must validate"
        ),
    }

    assert_eq!(
        deflate_init_status(config),
        ReturnCode::OK,
        "deflateInit must accept its own defaults"
    );
}

/// The default inflate configuration is the one `inflateInit` builds.
///
/// `inflateInit_` is `inflateInit2_` with `windowBits = DEF_WBITS` (`inflate.c`
/// L214-L217), which asks for a zlib stream with the largest window. The largest
/// window is the right default for the reason spelt out in
/// `a_zlib_header_window_size_is_adopted_or_refused`: it is the only explicit request
/// that never refuses a conforming stream.
///
/// This is also the configuration both `uncompress` wrappers use, since both go
/// through `inflateInit` (`uncompr.c` L51).
///
/// Mirrors `inflate.c` L214-L217.
#[test]
fn the_default_inflate_configuration_is_the_inflate_init_call() {
    let config = InflateConfig::default();

    assert_eq!(
        config.window_bits, DEF_WBITS,
        "the default windowBits must be DEF_WBITS"
    );
    assert_eq!(config, InflateConfig::new(DEF_WBITS));

    match config.validate() {
        Ok(validated) => {
            assert_eq!(
                validated.wrap,
                InflateWrap::Zlib,
                "the default must ask for a zlib header only, not automatic detection"
            );
            assert_eq!(i32::from(validated.window_bits), MAX_WBITS);
            assert!(
                !validated.window_size_from_header(),
                "the default requests an explicit window size"
            );
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "the default inflate configuration must validate"
        ),
    }

    assert_eq!(
        inflate_init_status(DEF_WBITS),
        ReturnCode::OK,
        "inflateInit must accept its own default"
    );
}

// ---------------------------------------------------------------------------
// The rest of the module's validation surface
// ---------------------------------------------------------------------------

/// `deflate` accepts six of the seven flush modes and refuses `Z_TREES`.
///
/// The flush argument is the one parameter a caller supplies on *every* call rather
/// than once per stream, so its bound is crossed far more often than any of the five
/// above. `deflate.c` L985 is a single test, `flush > Z_BLOCK || flush < 0`, and both
/// halves matter: the upper half is what makes `Z_TREES` decompression-only, and the
/// lower half is what rejects a negative argument that a bit-twiddling caller might
/// produce.
///
/// There is deliberately no counterpart for `inflate`, which accepts any `int`.
///
/// Mirrors `deflate.c` L985.
#[test]
fn deflate_accepts_six_of_the_seven_flush_modes() {
    assert_eq!(Flush::ALL.len(), 7, "Flush::ALL must hold all seven modes");

    for flush in Flush::ALL {
        // Every mode round-trips, `Z_TREES` included: it is a valid flush value that
        // `deflate` in particular refuses, not an unrecognised integer.
        assert_eq!(Flush::from_raw(flush.as_raw()), Some(flush));
        assert_eq!(Flush::try_from(flush.as_raw()), Ok(flush));

        let expected = flush != Flush::Trees;
        assert_eq!(
            flush.is_valid_for_deflate(),
            expected,
            "{flush:?} must be {} for deflate",
            if expected { "valid" } else { "invalid" }
        );
        assert_eq!(
            status_of(validate_deflate_flush(flush.as_raw())),
            if expected {
                ReturnCode::OK
            } else {
                ReturnCode::STREAM_ERROR
            },
            "validate_deflate_flush must agree with is_valid_for_deflate for {flush:?}"
        );
    }

    // The named constants map to the named modes.
    assert_eq!(Flush::from_raw(Z_NO_FLUSH), Some(Flush::NoFlush));
    assert_eq!(Flush::from_raw(Z_PARTIAL_FLUSH), Some(Flush::PartialFlush));
    assert_eq!(Flush::from_raw(Z_SYNC_FLUSH), Some(Flush::SyncFlush));
    assert_eq!(Flush::from_raw(Z_FULL_FLUSH), Some(Flush::FullFlush));
    assert_eq!(Flush::from_raw(Z_FINISH), Some(Flush::Finish));
    assert_eq!(Flush::from_raw(Z_BLOCK), Some(Flush::Block));
    assert_eq!(Flush::from_raw(Z_TREES), Some(Flush::Trees));
    assert_eq!(Flush::default(), Flush::NoFlush);

    // The boundary pair on each side, plus the extremes.
    assert!(
        validate_deflate_flush(Z_BLOCK).is_ok(),
        "Z_BLOCK is the ceiling"
    );
    assert!(
        validate_deflate_flush(Z_TREES).is_err(),
        "Z_TREES is one above deflate's ceiling"
    );
    for flush in [-1, 7, 8, 100, i32::MIN, i32::MAX] {
        assert_eq!(
            Flush::from_raw(flush),
            None,
            "flush {flush} must not name a mode"
        );
        assert_eq!(
            validate_deflate_flush(flush),
            Err(ReturnCode::STREAM_ERROR),
            "deflate must refuse flush {flush}"
        );
    }
}

/// `deflateParams` applies exactly the bounds `deflateInit2_` applies to the two
/// parameters it re-tunes.
///
/// Re-tuning a live stream is a second entry point into the same two checks, and the
/// reference reaches them with the same code and in the same order -- the default-level
/// resolution included (`deflate.c` L784-L788). Asserting the two agree, value for
/// value, is what stops a stream being reconfigurable into a state it could not have
/// been initialised in.
///
/// Mirrors `deflate.c` L784-L788.
#[test]
fn deflate_params_shares_the_init_bounds() {
    for level in Z_NO_COMPRESSION..=Z_BEST_COMPRESSION {
        for strategy in Strategy::ALL {
            assert_eq!(
                validate_deflate_params_change(level, strategy.as_raw())
                    .map(|(resolved, held)| (i32::from(resolved), held)),
                Ok((level, strategy)),
                "deflateParams must accept level {level} with {strategy:?}"
            );
        }
    }

    // The default level resolves here too, which is what lets a caller re-tune a
    // stream back to the library default.
    assert_eq!(
        validate_deflate_params_change(Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY)
            .map(|(resolved, _)| i32::from(resolved)),
        Ok(DEF_LEVEL),
        "deflateParams must resolve Z_DEFAULT_COMPRESSION to DEF_LEVEL"
    );

    // And the same values `deflateInit2_` refuses are refused here, with the same
    // status. Asserting the agreement rather than each side separately is what makes
    // this a shared-bounds test.
    for level in [-2, 10, i32::MIN, i32::MAX] {
        assert_eq!(
            status_of(validate_deflate_params_change(level, Z_DEFAULT_STRATEGY)),
            deflate_params_status(
                level,
                Z_DEFLATED,
                MAX_WBITS,
                DEF_MEM_LEVEL,
                Z_DEFAULT_STRATEGY
            ),
            "deflateParams and deflateInit2_ must agree about level {level}"
        );
    }
    for strategy in [-1, Z_FIXED + 1, i32::MIN, i32::MAX] {
        assert_eq!(
            status_of(validate_deflate_params_change(DEF_LEVEL, strategy)),
            deflate_params_status(DEF_LEVEL, Z_DEFLATED, MAX_WBITS, DEF_MEM_LEVEL, strategy),
            "deflateParams and deflateInit2_ must agree about strategy {strategy}"
        );
    }
}

/// `DeflateConfig::from_raw` types the two enumerated parameters and leaves the three
/// integers alone.
///
/// The asymmetry is deliberate and worth pinning: a method or a strategy is a choice
/// from a fixed set, so an invalid one need never be representable and `from_raw`
/// refuses it immediately; a level and a `windowBits` are a number on a scale and a
/// small encoded record, so they are carried through exactly as the caller wrote them
/// and checked by `validate`. A `from_raw` that checked all five would make it
/// impossible to hold what a C caller actually passed.
#[test]
fn deflate_config_from_raw_types_only_the_enumerated_parameters() {
    // The three integers pass through untouched, out of range included.
    match DeflateConfig::from_raw(1000, Z_DEFLATED, 9999, -7, Z_RLE) {
        Ok(config) => {
            assert_eq!(config.level, 1000, "the level is carried, not clamped");
            assert_eq!(
                config.window_bits, 9999,
                "windowBits is carried, not clamped"
            );
            assert_eq!(config.mem_level, -7, "memLevel is carried, not clamped");
            assert_eq!(config.method, Method::Deflated);
            assert_eq!(config.strategy, Strategy::Rle);

            // ... and are refused by `validate`, which is where the bounds live.
            assert_eq!(
                status_of(config.validate()),
                ReturnCode::STREAM_ERROR,
                "validate must refuse what from_raw carried through"
            );
        }
        Err(code) => assert_eq!(
            code,
            ReturnCode::OK,
            "from_raw must not check the three integer parameters"
        ),
    }

    // A valid five-tuple builds the configuration it names.
    match DeflateConfig::from_raw(9, Z_DEFLATED, -15, MIN_MEM_LEVEL, Z_FIXED) {
        Ok(config) => {
            assert_eq!(
                config,
                deflate_config(9, -15, MIN_MEM_LEVEL, Strategy::Fixed),
                "from_raw must agree with direct construction"
            );
            common::check_err(
                status_of(config.validate()),
                "validating a raw-built configuration",
            );
        }
        Err(code) => assert_eq!(code, ReturnCode::OK, "a valid five-tuple must be accepted"),
    }
}

/// The two directions disagree exactly where the reference does, and nowhere else.
///
/// The single most valuable assertion in this file. `decode_deflate_window_bits` and
/// `decode_inflate_window_bits` are two ports of superficially similar C, and the
/// cheapest way to get them wrong is to write one and reuse it for the other. This
/// walks the whole sweep and requires the two verdicts to differ on precisely the set
/// of values the reference differs on -- so an implementation that unified them would
/// fail here with the offending value named, rather than surfacing later as a stream
/// that will not round-trip.
///
/// The disagreements, and why each exists:
///
/// | `windowBits` | deflate | inflate | Reason |
/// |---|---|---|---|
/// | `0` | refused | accepted | only a decompressor can read a size from a header |
/// | `-8` | refused | accepted | only a zlib header can *transmit* a 256-byte size |
/// | `16`, `24` | refused | accepted | inflate defers, deflate needs the exponent now |
/// | `32`, `40 ..= 47` | refused | accepted | automatic detection is decompression-only |
#[test]
fn the_two_directions_disagree_exactly_where_the_reference_does() {
    let mut disagreements = Vec::new();

    for window_bits in SWEEP_LOW..=SWEEP_HIGH {
        let deflate_accepts = decode_deflate_window_bits(window_bits).is_ok();
        let inflate_accepts = decode_inflate_window_bits(window_bits).is_ok();

        if deflate_accepts != inflate_accepts {
            // Every disagreement runs the same way round: inflate is the permissive
            // direction. A value deflate accepted and inflate refused would mean a
            // stream this library can write and cannot read.
            assert!(
                inflate_accepts,
                "windowBits {window_bits} is accepted for compression but refused for \
                 decompression, which would make an unreadable stream"
            );
            disagreements.push(window_bits);
        }
    }

    let expected: Vec<i32> = core::iter::once(-8)
        .chain(core::iter::once(0))
        .chain(core::iter::once(16))
        .chain(core::iter::once(24))
        .chain(core::iter::once(32))
        .chain(40..=47)
        .collect();

    assert_eq!(
        disagreements, expected,
        "the two windowBits rules must differ on exactly the reference's set of values"
    );
}
