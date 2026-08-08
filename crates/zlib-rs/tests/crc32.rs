//! Integration tests for the CRC-32 engine: `crates/zlib-rs/src/crc32/**`.
//!
//! This suite is the black-box counterpart to the `#[cfg(test)] mod tests` blocks inside the
//! engine itself. It sees only what a consumer sees -- the items `zlib_rs::crc32` makes public --
//! and it measures them against two independent authorities:
//!
//! 1. **`POLY` alone.** Every one of the roughly 4,600 `const` table entries the engine ships is
//!    recomputed here from the single constant `0xedb8_8320`, following the generator in
//!    `crc32.c` L247-L262 and L461-L472. That is the check the plan calls the cheapest
//!    high-signal verification in the whole port: a transcription slip anywhere in the
//!    9,446 lines of `crc32.h` fails an assertion that names the exact index.
//! 2. **The reference C implementation.** The literal check values and the
//!    [`ORACLE_CHECKS`] matrix below were produced by compiling the in-tree `crc32.c` -- which
//!    this port leaves untouched precisely so that it can serve as the oracle -- and reading its
//!    answers off. Nothing here was recalled from memory; see [`ORACLE_CHECKS`] for the exact
//!    procedure.
//!
//! Between them the two authorities close the loop: (1) proves the tables are the *reference's*
//! tables, and (2) proves the code that walks them produces the *reference's* bytes.
//!
//! # What each section covers
//!
//! | Section | Subject | Anchored by |
//! |---|---|---|
//! | Tables | [`CRC_TABLE`], [`CRC_BIG_TABLE`], [`X2N_TABLE`], both braid tables | `POLY`, plus a 20-entry prefix transcribed from `crc32.h` L6-L9 |
//! | `get_crc_table` | the accessor at `crc32.c` L482-L487 | [`CRC_TABLE`] and pointer identity |
//! | Known answers | `crc32`, `crc32_z` | the C oracle and [`reference_crc32`] |
//! | Length sweeps | the `N * W + W - 1` braid threshold of `crc32.c` L640 | [`reference_crc32`] |
//! | Resumability | chunked versus single-shot accumulation | a single call over the whole buffer |
//! | Backends | [`Generic`], [`Braid`] and, when built, `Simd` | each other, bit for bit |
//! | Combine | the five `crc32_combine*` entry points | the concatenation identity and the C oracle |
//!
//! # The structural boundary everything turns on
//!
//! `crc32_z` switches from a byte-at-a-time loop to the braided word-at-a-time body at
//! `len >= N * W + W - 1` (`crc32.c` L640). With `N` fixed at 5 (`crc32.c` L64) and `W` either 8
//! or 4 (`crc32.c` L87-L101) that threshold is **47 bytes on a 64-bit target and 19 on a 32-bit
//! one**. Every boundary length in this file is therefore computed from the `N` and `W` constants
//! rather than written as a literal: a hard-coded 47 would silently pass on a 32-bit target while
//! testing nothing at all, which is the single easiest way for a suite like this to look thorough
//! and be worthless.
//!
//! The literal *check values* are a different matter and are hard-coded freely, because the
//! CRC-32 of a byte string is a property of the string and not of the target: `W` decides which
//! instructions compute it, never what it is.
//!
//! # What this suite deliberately does not reach
//!
//! `multmodp`, `x2nmodp`, `byte_swap`, `crc_word` and `crc_word_big` are `local` in the C sources
//! and crate-private here, so they are unreachable from an integration test by design and are
//! exercised only through the public combine functions below. Their direct coverage lives in the
//! `#[cfg(test)] mod tests` blocks inside `crates/zlib-rs/src/crc32/combine.rs` and
//! `crates/zlib-rs/src/crc32/braid.rs`, which is the right place for it: a test that had to see
//! them would be arguing for widening a visibility the C sources deliberately close.
//!
//! One consequence is worth stating, because it looks like an omission and is not. This file
//! contains its own `multmodp` ([`reference_multmodp`]) purely to derive the tables; it is a
//! second implementation written to be *obviously* right, never a way of reaching the engine's.
//!
//! # No lazy initialization anywhere in the subsystem
//!
//! Confirmed by inspection of `crates/zlib-rs/src/crc32/tables.rs` and its five siblings: there
//! is no `OnceCell`, no `OnceLock`, no `Once`, no `static mut` and no interior mutability of any
//! kind. Every table is `const` data materialized by the compiler. That is what removes the
//! hazard `crc32.c` L12-L17 warns about -- "there is no mutex or semaphore protection on the
//! static variables used to control the first-use generation of the crc tables" -- rather than
//! merely mitigating it, and it is why bit 13 of `zlibCompileFlags()`, `DYNAMIC_CRC_TABLE`, must
//! be reported CLEAR by the facade. There is consequently no first-call path for this suite to
//! test, and [`get_crc_table`] is asserted to be a pure accessor rather than an initializer.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no FFI.** The crate under test carries `#![forbid(unsafe_code)]`; its tests
//!   hold to the same standard. Nothing here declares or calls an `extern` function.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` has an empty `[dependencies]` table
//!   and no `[dev-dependencies]`, and `deny.toml`'s `[bans]` section names that table as the
//!   enforcement point. So the harness is the built-in `#[test]` one, the assertions are the
//!   built-in macros, and the pseudo-random filler is `common::lcg_fill` rather than `rand`.
//! * **No narrowing casts.** The workspace denies `clippy::pedantic`, which includes the cast
//!   family. Every byte is taken out of a `to_le_bytes` or `to_be_bytes` image and every widening
//!   goes through `From`, so no `as` conversion appears below.
//! * **`std` is available and `no_std` is not.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test is its own crate and
//!   links `std` unconditionally, so no `#![no_std]` appears here.
//! * **Deterministic and hermetic.** Every payload is computed from constants, so a failure
//!   reproduces exactly. No network, no filesystem, no clock, no environment.
//! * **Miri-clean.** The suite runs under `cargo +nightly miri test -p zlib-rs`. Three
//!   concessions are made to the interpreter's speed and to nothing else: the long length sweep
//!   and the whole-corpus backend comparison are `#[cfg_attr(miri, ignore)]`, and the corpus
//!   payloads are truncated by [`capped`]. Each is stated at its own definition together with what
//!   still covers the same ground -- and between them the boundary tests, the exhaustive
//!   `0..=`[`SHORT_SWEEP_MAX`] sweep and the full backend comparison at every structural length all
//!   run under Miri unchanged, so no structural case goes unchecked there.

// The workspace denies the panic-prone lints, which is right for library code and wrong for a
// test: a test asserts, a failed assertion panics, and reading a table by index is clearer than
// defensively matching on it -- especially here, where every index is either a `u8` widened into
// a 256-entry table or a loop bound taken from the table's own length. Scoped to this file, which
// ships nothing. `crates/zlib-rs/tests/common/mod.rs` relaxes the same lints in its own
// `self_tests` module for the same reason.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use core::ptr;

use common::{corpus, lcg_fill};
use zlib_rs::crc32::tables::{Word, N, POLY, W};
use zlib_rs::crc32::{
    crc32, crc32_combine, crc32_combine64, crc32_combine_gen, crc32_combine_gen64,
    crc32_combine_op, crc32_z, get_crc_table, Braid, Crc32Backend, Generic, CRC_BIG_TABLE,
    CRC_BRAID_BIG_TABLE, CRC_BRAID_TABLE, CRC_TABLE, X2N_TABLE,
};

#[cfg(feature = "simd")]
use zlib_rs::crc32::Simd;

/// `z_word_t` is 64 or 32 bits and nothing else (`crc32.c` L71-L74, L96-L103).
///
/// Several helpers below build a `W`-byte array around a four-byte value, which needs `W >= 4`.
/// Pinning the two admissible widths here makes that a build failure rather than a subtraction
/// overflow if the alias ever grows a third case.
const _: () = assert!(
    W == 8 || W == 4,
    "crc32.c L96-L103: z_word_t is either Z_U8 (W == 8) or Z_U4 (W == 4)"
);

/// The number of interleaved CRC streams the braided body runs, `crc32.c` L64.
const _: () = assert!(
    N == 5,
    "crc32.c L62-L67: N is 5 unless Z_TESTN overrides it"
);

/// The length at or above which `crc32_z` runs the braided word-at-a-time body.
///
/// `crc32.c` L640: `if (len >= N * W + W - 1)`. 47 bytes where `W` is 8, 19 where it is 4. The
/// `W - 1` term is the worst-case cost of the alignment prologue at `crc32.c` L646-L650, which
/// consumes bytes one at a time until `buf` sits on a `z_word_t` boundary.
const MIN_BRAID_LEN: usize = N * W + W - 1;

/// The number of bytes one braided iteration consumes: `N * W`, `crc32.c` L653-L654.
///
/// `blks = len / (N * W)` decides how many full blocks run, so `len % (N * W)` is the remainder
/// class the byte tail at `crc32.c` L922-L937 has to finish. Sweeping every length below covers
/// every one of those classes.
const BRAID_BLOCK: usize = N * W;

/// Seed for the pseudo-random fixture every length-parameterised test draws from.
///
/// Frozen at zero: [`ORACLE_CHECKS`] records the C implementation's answers for prefixes of
/// exactly this stream, so changing it would invalidate all 88 of them.
const FIXTURE_SEED: u64 = 0;

/// Starting check values used wherever a test varies the seed.
///
/// `0` is the fresh-value case every caller starts from, `1` is the smallest resumption, and
/// `0xffff_ffff` is the all-ones case that pre-conditions to zero and so would hide a missing
/// leading complement. `0x1234_abcd` is an arbitrary mid-range value with bits set in all four
/// bytes. The same four seeds index [`ORACLE_CHECKS`].
const ORACLE_SEEDS: [u32; 4] = [0x0000_0000, 0x0000_0001, 0x1234_abcd, 0xffff_ffff];

/// Prefix lengths of the [`FIXTURE_SEED`] stream that [`ORACLE_CHECKS`] records.
///
/// Chosen to straddle both possible braid thresholds and both possible alignment prologues: `0`
/// and `1` are the degenerate cases, `7`/`8`/`9` bracket `W == 8`, `18`/`19`/`20` bracket the
/// `W == 4` threshold, `39`/`40` bracket `N * W` for `W == 8`, `46`/`47`/`48` bracket the
/// `W == 8` threshold, `55`/`56` bracket the first braided block plus its prologue,
/// `63`/`64`/`255`/`256` are power-of-two neighbourhoods, and `600`, `1_024` and `4_096` exercise
/// many consecutive blocks.
///
/// These are absolute byte counts, so they are meaningful on both `W` values even though only one
/// of them is the live threshold on any given target.
const ORACLE_LENGTHS: [usize; 22] = [
    0, 1, 7, 8, 9, 18, 19, 20, 39, 40, 46, 47, 48, 55, 56, 63, 64, 255, 256, 600, 1_024, 4_096,
];

/// The reference C implementation's answers, indexed by [`ORACLE_SEEDS`] then [`ORACLE_LENGTHS`].
///
/// `ORACLE_CHECKS[s][l]` is `crc32(ORACLE_SEEDS[s], fixture[..ORACLE_LENGTHS[l]])` as computed by
/// the C library built from the in-tree `crc32.c`, where `fixture` is
/// `lcg_fill(FIXTURE_SEED, ..)`. Each value was obtained by compiling a short driver against
/// `crc32.c` with the reference flags -- `gcc -O2 -I. -D_LARGEFILE64_SOURCE=1 driver.c crc32.c`
/// -- reimplementing `lcg_fill`'s two `MMIX` constants in C so both sides fold identical bytes,
/// and printing the result. None of these numbers was recalled or copied from elsewhere.
///
/// A CRC-32 is a property of the byte string, so this matrix is target-independent even though
/// the code path taken to compute it is not: on a 32-bit target the braided body starts at length
/// 19 instead of 47 and must still produce these same values.
///
/// `crc32(seed, &[])` is `seed` in every row, which is the empty-slice identity of
/// `crc32.c` L635 and L940 cancelling, and is deliberately not the C entry point's
/// `buf == Z_NULL` early return at `crc32.c` L627-L628 -- that case cannot arise behind a slice
/// and belongs to the facade.
const ORACLE_CHECKS: [[u32; 22]; 4] = [
    // start = 0x0000_0000
    [
        0x0000_0000,
        0xc8d8_3bf0,
        0xc4d9_9e87,
        0x5da3_a8ec,
        0xbc34_4ac2,
        0xf27b_929b,
        0x7a97_4b4a,
        0x701f_09b4,
        0x885f_4447,
        0x015c_4432,
        0x8a9e_5fce,
        0xd5e5_b5cb,
        0x1edb_7dad,
        0x8030_f3aa,
        0x9f30_be26,
        0x9ee3_fbd5,
        0x4d23_e8d3,
        0x88ff_03c6,
        0xa156_060d,
        0x3150_3d3a,
        0x0694_5f2d,
        0x01e2_9d57,
    ],
    // start = 0x0000_0001
    [
        0x0000_0001,
        0xbfdf_0b66,
        0x62ae_9533,
        0x9109_a872,
        0xab4f_5e81,
        0x1d29_247a,
        0xad75_cb12,
        0x1578_32f2,
        0xc6d6_4fee,
        0xae18_d675,
        0xf557_c45d,
        0xbc9c_beae,
        0x236a_9171,
        0xa233_40ba,
        0x82a5_adf1,
        0xce50_9109,
        0xc216_c546,
        0xda6a_57b1,
        0x6f65_77c6,
        0x043d_1da5,
        0xfe05_ae42,
        0x5b3a_347b,
    ],
    // start = 0x1234_abcd
    [
        0x1234_abcd,
        0x2d1f_b156,
        0xb479_00b3,
        0x7c67_fcc7,
        0x10a9_77d6,
        0xfd5e_0123,
        0xbf22_5567,
        0x3505_e0df,
        0x89dd_1ea3,
        0xa63a_e009,
        0xa3fe_72cc,
        0x3bc2_b4ca,
        0x6932_6a3a,
        0x67a0_b4b0,
        0x62b5_d71b,
        0x75a6_904d,
        0xb31c_b6ce,
        0x6991_1b26,
        0x01bd_8a6d,
        0xdd08_a9f0,
        0x725e_c1c4,
        0x7278_6261,
    ],
    // start = 0xffff_ffff
    [
        0xffff_ffff,
        0xe525_2b82,
        0xa64a_be06,
        0xc77e_887a,
        0xa5c2_a193,
        0x6a9f_a229,
        0x5f60_7dda,
        0x8035_6dc6,
        0xf752_4f98,
        0x174f_867c,
        0x761e_a8da,
        0xcfc3_e141,
        0x13ac_31c7,
        0x6ef4_ce14,
        0xb307_e490,
        0x89b6_dece,
        0xc751_741a,
        0x8385_ea95,
        0x533f_7caa,
        0xb942_60e6,
        0x16de_0ffc,
        0x3901_62b9,
    ],
];

/// The first sixteen bytes [`lcg_fill`] produces from [`FIXTURE_SEED`].
///
/// A guard, not a test of the generator. [`ORACLE_CHECKS`] is only meaningful while
/// `common::lcg_fill` keeps producing the bytes it produced when those values were measured, so
/// this pins a short prefix of that stream. If the shared helper ever changes, the failure reads
/// "the fixture generator changed" instead of eighty-eight unexplained checksum mismatches.
const FIXTURE_PREFIX: [u8; 16] = [
    0x14, 0x1a, 0x9a, 0x66, 0x62, 0x8f, 0x14, 0x5b, 0x7b, 0x72, 0xa2, 0x5d, 0x0c, 0x18, 0xe9, 0x32,
];

/// Longest fixture any test draws from, and the largest entry of [`ORACLE_LENGTHS`].
const FIXTURE_LEN: usize = 4_096;

/// Upper bound of the exhaustive short length sweep, run in every configuration.
///
/// 256 comfortably exceeds both possible values of [`MIN_BRAID_LEN`] and covers at least six full
/// braided blocks on a 64-bit target and twelve on a 32-bit one, so every remainder class of
/// `len % (N * W)` appears whichever `W` is live. It is not scaled down under Miri: the whole
/// point of the sweep is that no remainder class is skipped, and at roughly 33,000 folded bytes
/// it is affordable even in the interpreter.
const SHORT_SWEEP_MAX: usize = 256;

/// Upper bound of the long length sweep, which continues where [`SHORT_SWEEP_MAX`] stops.
const LONG_SWEEP_MAX: usize = 4_096;

/// Length of the buffer split at *every* offset by the resumability test.
///
/// Roughly 600 bytes natively. Reduced under Miri because the test is quadratic in this value --
/// it folds `len` bytes once per split point -- and 160 still spans several braided blocks and
/// both possible thresholds, so no structural case is lost.
#[cfg(not(miri))]
const SPLIT_LEN: usize = 600;
/// See the native definition above; reduced only for interpreter speed.
#[cfg(miri)]
const SPLIT_LEN: usize = 160;

/// Length of the buffer split at selected structural offsets.
///
/// 4 `KiB` natively, which is a hundred braided blocks on a 64-bit target. Reduced under Miri,
/// where the interesting offsets are all far below 1 `KiB` anyway.
#[cfg(not(miri))]
const LARGE_SPLIT_LEN: usize = 4_096;
/// See the native definition above; reduced only for interpreter speed.
#[cfg(miri)]
const LARGE_SPLIT_LEN: usize = 1_024;

/// Returns `len` pseudo-random bytes from [`FIXTURE_SEED`].
///
/// The prefix property matters and is relied on throughout: `fixture(m)` is a prefix of
/// `fixture(n)` for `m <= n`, because [`lcg_fill`] advances one state per byte from a fixed seed.
/// That is what lets [`ORACLE_CHECKS`] be a table of prefix lengths rather than a table of
/// independent buffers.
fn fixture(len: usize) -> Vec<u8> {
    let mut out = vec![0_u8; len];
    lcg_fill(FIXTURE_SEED, &mut out);
    out
}

/// The prefix of a corpus payload the content-varying tests fold.
///
/// The identity natively: every class is folded in full, which for `corpus::window_crossing()`
/// means 33,048 bytes. It exists so that the Miri truncation below has exactly one definition and
/// every test that folds a whole payload goes through it, rather than each growing its own
/// `#[cfg(miri)]` arm.
#[cfg(not(miri))]
fn capped(payload: &[u8]) -> &[u8] {
    payload
}

/// The prefix of a corpus payload the content-varying tests fold, truncated under Miri.
///
/// Folding `corpus::window_crossing()` in full dominates the whole interpreted run, and it is the
/// only class large enough to do so. Truncating costs very little: every class is still exercised,
/// with its own leading bytes and -- for the classes whose character is uniform, such as the long
/// single-byte run and the ascending byte-value cycle -- its content character wholly intact. Only
/// the tail of the window-crossing class goes unseen, and it is folded in full natively, in
/// `cargo test` and in CI. The truncation is therefore for interpreter speed alone.
#[cfg(miri)]
fn capped(payload: &[u8]) -> &[u8] {
    /// Two `KiB` is more than fifty braided blocks past either possible value of
    /// [`MIN_BRAID_LEN`], so no structural case is lost by truncating here.
    const CORPUS_CAP: usize = 2_048;

    &payload[..payload.len().min(CORPUS_CAP)]
}

/// The CRC-32 of `buf`, folded into `start` one byte at a time.
///
/// A second implementation of the whole algorithm, written to be *obviously* correct rather than
/// fast, and deliberately not a dependency of anything: it exists so that the engine's braided
/// and vectorised paths have something to disagree with. It is three lines of the C source and
/// nothing else:
///
/// ```c
/// crc = (~crc) & 0xffffffff;                              /* crc32.c L635 */
/// while (len--) crc = (crc >> 8) ^ crc_table[(crc ^ *buf++) & 0xff];  /* crc32.c L934-L937 */
/// return crc ^ 0xffffffff;                                /* crc32.c L940 */
/// ```
///
/// It reads [`CRC_TABLE`] from the crate rather than rebuilding it, so it is an independent
/// implementation of the *loop*, not of the table; the table has its own tests, which derive
/// every entry from [`POLY`].
fn reference_crc32(start: u32, buf: &[u8]) -> u32 {
    // `crc32.c` L635. `(~crc) & 0xffffffff` on a wider C `uLong` is exactly `!crc` on a `u32`:
    // the mask keeps only the low half, which is all a `u32` holds.
    let mut crc = !start;

    for &byte in buf {
        // C writes `crc_table[(crc ^ *buf++) & 0xff]`. Masking to eight bits after the
        // exclusive-or is the same as taking the low byte of `crc` first, and the low byte is the
        // first element of the little-endian image -- obtained without a narrowing cast.
        let [low, ..] = crc.to_le_bytes();
        // `usize::from` of a `u8` is at most 255 and `CRC_TABLE` has 256 entries, so this index
        // is in range for every possible pair of operands.
        crc = (crc >> 8) ^ CRC_TABLE[usize::from(low ^ byte)];
    }

    // `crc32.c` L940.
    !crc
}

/// `a(x)` multiplied by `b(x)` modulo the CRC polynomial, both reflected.
///
/// A second implementation of `multmodp` (`crc32.c` L163-L178), used only to derive the tables
/// below from [`POLY`]. The engine's own `multmodp` is crate-private, as its C original is
/// `local`, so this is a re-derivation and not a call into it.
///
/// The C loop is `for (;;)`, and its comment records the precondition that makes that safe: "For
/// speed, this requires that a not be zero." When `a` is zero the guard `if (a & m)` never fires,
/// `m` shifts down to zero, and the loop cannot terminate. The bound of 32 iterations here is the
/// same bound the engine applies, and it is exact rather than defensive: `m` starts at bit 31 and
/// loses one bit per iteration, so a non-zero `a` always meets its lowest set bit within 32
/// iterations and breaks, while a zero `a` falls out having accumulated nothing and returns `0`.
/// That return value is precisely what the braid generator writes into index 0 of every table by
/// hand (`crc32.c` L466-L467) in order to avoid the call, so the bounded form agrees with the C
/// code on the whole of its domain and is defined on the point where the C code is not.
fn reference_multmodp(a: u32, mut b: u32) -> u32 {
    // `crc32.c` L166-L167.
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;

    for _ in 0..u32::BITS {
        // `crc32.c` L169-L173.
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                break;
            }
        }
        // `crc32.c` L174-L175. `m` is at least 1 at the top of every iteration -- it is
        // `1 << (31 - i)` on iteration `i` -- so `m - 1` above never underflows.
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }

    p
}

/// The table of `x^(2^n) mod p(x)` for `n` in `0..32`, rebuilt from [`POLY`].
///
/// `crc32.c` L259-L262:
///
/// ```c
/// p = (z_crc_t)1 << 30;         /* x^1 */
/// x2n_table[0] = p;
/// for (n = 1; n < 32; n++)
///     x2n_table[n] = p = (z_crc_t)multmodp(p, p);
/// ```
///
/// Each entry is the square of the one before, so the table walks `x^1, x^2, x^4, x^8, ...`. The
/// reflected encoding puts `x^1` at bit 30, which is why the seed is `1 << 30` and not `1 << 31`
/// -- bit 31 is `x^0`.
fn reference_x2n_table() -> [u32; 32] {
    let mut table = [0_u32; 32];

    let mut p: u32 = 1 << 30;
    table[0] = p;
    for slot in table.iter_mut().skip(1) {
        p = reference_multmodp(p, p);
        *slot = p;
    }

    table
}

/// `x^(n * 2^k) mod p(x)`, from a table produced by [`reference_x2n_table`].
///
/// A second implementation of `x2nmodp` (`crc32.c` L184-L195). The `k & 31` index mask is the C
/// code's and is reproduced exactly rather than corrected: `k` grows by one per bit of `n`, so it
/// exceeds 31 for any `n` of 2^29 or more, and from there the table wraps. That wrap is
/// observable through `crc32_combine_gen64` for lengths of 512 `MiB` and up, which is well inside
/// the range of files this library is used on, so it is behaviour to preserve and not a bug to
/// fix. The tests below pin it against the C implementation's own answers.
fn reference_x2nmodp(x2n: &[u32; 32], mut n: u64, mut k: u32) -> u32 {
    // `crc32.c` L187: x^0 == 1, which is bit 31 in the reflected encoding.
    let mut p: u32 = 1 << 31;

    while n != 0 {
        if n & 1 != 0 {
            // `k & 31` is at most 31, so the conversion cannot fail on any target: Rust requires
            // `usize` to be at least 16 bits wide.
            let index = usize::try_from(k & 31).unwrap();
            p = reference_multmodp(x2n[index], p);
        }
        n >>= 1;
        // C's `k++` on an `unsigned`; wrapping is unreachable for any `n` that fits in 64 bits
        // but is spelled out so the arithmetic cannot panic in a debug build.
        k = k.wrapping_add(1);
    }

    p
}

/// Widens a 32-bit value into a `z_word_t` and reverses its bytes.
///
/// `byte_swap` (`crc32.c` L121-L139) reverses all `W` bytes of a word, and every value the table
/// generator hands it is a `z_crc_t` -- 32 bits -- promoted to `z_word_t`. So on a 64-bit target
/// the four significant bytes end up at the *top* of the result and four zero bytes follow, which
/// is exactly what `crc32.h`'s big-endian tables show.
///
/// The widening goes through the byte representation rather than `Word::from` so that it is both
/// cast-free and free of an identity conversion, and therefore compiles and lints cleanly whether
/// `Word` is `u64` or `u32`.
fn byte_swapped(value: u32) -> Word {
    let mut bytes = [0_u8; W];
    // Little-endian: the four bytes of `value` occupy the low end of the word and the remaining
    // `W - 4` bytes -- none at all when `W` is 4 -- stay zero. The compile-time assertion at the
    // top of this file guarantees `W >= 4`.
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    Word::from_le_bytes(bytes).swap_bytes()
}

/// Every length at which the byte-at-a-time path, the alignment prologue, the braided body and
/// the byte tail hand off to one another.
///
/// All of them are computed from `N` and `W`, never written as literals, so the set adapts to a
/// 32-bit target where [`MIN_BRAID_LEN`] is 19 rather than 47. Sorted and de-duplicated because
/// several expressions coincide on one target and not the other -- `N * W + W - 2` *is*
/// `MIN_BRAID_LEN - 1`, and on a 32-bit target `N * W - 1` *is* `MIN_BRAID_LEN`.
fn boundary_lengths() -> Vec<usize> {
    let mut lengths = vec![
        // The degenerate cases, below any word.
        0,
        1,
        2,
        // One `z_word_t`: the granularity of the alignment prologue at `crc32.c` L646-L650.
        W - 1,
        W,
        W + 1,
        // One braided block: `blks = len / (N * W)` at `crc32.c` L653.
        BRAID_BLOCK - 1,
        BRAID_BLOCK,
        BRAID_BLOCK + 1,
        // The threshold itself, `crc32.c` L640, and its immediate neighbours. One below takes the
        // byte-at-a-time path for the whole buffer; the threshold and one above take the braided
        // one.
        MIN_BRAID_LEN - 1,
        MIN_BRAID_LEN,
        MIN_BRAID_LEN + 1,
        // A block plus a whole word of tail.
        BRAID_BLOCK + W,
        // Several blocks, with and without a remainder.
        2 * BRAID_BLOCK,
        2 * BRAID_BLOCK + W - 1,
        3 * BRAID_BLOCK,
        4 * BRAID_BLOCK + 1,
        5 * BRAID_BLOCK,
    ];

    lengths.sort_unstable();
    lengths.dedup();
    lengths
}

// ---------------------------------------------------------------------------------------------
// Tables -- the cheapest high-signal verification in the port
//
// `crc32.h` is 9,446 generated lines. Transcribing it into Rust `const` arrays is the one step of
// this port where a single mistyped nibble produces a library that computes plausible-looking
// check values that match nothing, and where no amount of algorithmic testing localises the
// fault. So every entry of every table is recomputed here from `POLY` -- roughly 4,600 values,
// each derived by the same construction the C generator uses -- and a hand-transcribed prefix is
// checked as well, because the prefix is what catches a table that is wrong *in kind* rather than
// wrong in one place.
// ---------------------------------------------------------------------------------------------

/// `POLY` is the reflected CRC-32 generator, with the `x^32` term implied.
///
/// `crc32.c` L157: `#define POLY 0xedb88320`. The polynomial is
/// `x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 + x^4 + x^2 + x + 1`
/// (`crc32.c` L219-L220), which `doc/rfc1952.txt` L419-L425 names as the CRC-32 of ISO 3309 and
/// of ITU-T V.42 section 8.1.1.6.2.
///
/// Reflected means the coefficient of `x^k` sits at bit `31 - k`, so the constant is rebuilt here
/// from the exponent list rather than compared against a remembered hexadecimal literal. The
/// `x^32` term is not represented: it is the implied leading term the remainder is taken against.
#[test]
fn poly_is_the_reflected_crc32_generator() {
    // The fourteen non-leading terms of the generator, highest power first.
    const EXPONENTS: [u32; 14] = [26, 23, 22, 16, 12, 11, 10, 8, 7, 5, 4, 2, 1, 0];

    let mut reflected: u32 = 0;
    for exponent in EXPONENTS {
        reflected |= 1 << (31 - exponent);
    }

    assert_eq!(
        POLY, reflected,
        "crc32.c L157 and L219-L220: POLY must be the reflected generator polynomial"
    );
    assert_eq!(
        POLY, 0xedb8_8320,
        "crc32.c L157: the literal spelling of the same constant"
    );
}

/// Every table has the length its C original has.
///
/// `crc_table` and `crc_big_table` have one entry per byte value; `x2n_table` has one entry per
/// bit of a `z_crc_t`; the braid tables are `W` blocks of 256 (`crc32.c` L458-L459: "Each array
/// must have room for w blocks of 256 elements"). `size_of::<Word>() == W` ties the alias to the
/// constant, which is the invariant the braided loads depend on.
#[test]
fn every_table_has_the_length_of_its_c_original() {
    assert_eq!(
        CRC_TABLE.len(),
        256,
        "crc32.h L5: crc_table[] is byte-indexed"
    );
    assert_eq!(
        CRC_BIG_TABLE.len(),
        256,
        "crc32.c L254: crc_big_table[] mirrors crc_table[] entry for entry"
    );
    assert_eq!(
        X2N_TABLE.len(),
        32,
        "crc32.c L259-L262: x2n_table[] holds x^(2^n) for n in 0..32"
    );

    assert_eq!(
        size_of::<Word>(),
        W,
        "crc32.c L96-L103: W is the width of z_word_t in bytes"
    );
    assert_eq!(CRC_BRAID_TABLE.len(), W, "crc32.c L459: w blocks of 256");
    assert_eq!(
        CRC_BRAID_BIG_TABLE.len(),
        W,
        "crc32.c L459: w blocks of 256"
    );
    for (k, block) in CRC_BRAID_TABLE.iter().enumerate() {
        assert_eq!(block.len(), 256, "crc_braid_table[{k}] is byte-indexed");
    }
    for (k, block) in CRC_BRAID_BIG_TABLE.iter().enumerate() {
        assert_eq!(block.len(), 256, "crc_braid_big_table[{k}] is byte-indexed");
    }
}

/// The first twenty entries of `crc_table` are the first four lines of `crc32.h`.
///
/// Transcribed by hand from `crc32.h` L6-L9, under the banner at L1-L3 that reads "tables for
/// rapid CRC calculation / Generated automatically by crc32.c":
///
/// ```text
/// local const z_crc_t FAR crc_table[] = {
///     0x00000000, 0x77073096, 0xee0e612c, 0x990951ba, 0x076dc419,
///     0x706af48f, 0xe963a535, 0x9e6495a3, 0x0edb8832, 0x79dcb8a4,
///     0xe0d5e91e, 0x97d2d988, 0x09b64c2b, 0x7eb17cbd, 0xe7b82d07,
///     0x90bf1d91, 0x1db71064, 0x6ab020f2, 0xf3b97148, 0x84be41de,
/// ```
///
/// Twenty rather than four, because the derivation test that follows proves the table is
/// *self-consistent* while this one proves it is the *right* table: a wholly different but
/// internally consistent construction -- the non-reflected polynomial, say, or a different
/// bit order -- would satisfy the derivation and fail here, at index 1, unmistakably.
#[test]
fn crc_table_prefix_matches_the_generated_header() {
    const HEADER_PREFIX: [u32; 20] = [
        0x0000_0000,
        0x7707_3096,
        0xee0e_612c,
        0x9909_51ba,
        0x076d_c419,
        0x706a_f48f,
        0xe963_a535,
        0x9e64_95a3,
        0x0edb_8832,
        0x79dc_b8a4,
        0xe0d5_e91e,
        0x97d2_d988,
        0x09b6_4c2b,
        0x7eb1_7cbd,
        0xe7b8_2d07,
        0x90bf_1d91,
        0x1db7_1064,
        0x6ab0_20f2,
        0xf3b9_7148,
        0x84be_41de,
    ];

    for (index, expected) in HEADER_PREFIX.into_iter().enumerate() {
        assert_eq!(
            CRC_TABLE[index], expected,
            "crc32.h L6-L9: crc_table[{index}] was transcribed incorrectly"
        );
    }
}

/// Every entry of `crc_table` is the CRC-32 of its own index, recomputed from [`POLY`].
///
/// `crc32.c` L248-L252:
///
/// ```c
/// for (i = 0; i < 256; i++) {
///     p = i;
///     for (j = 0; j < 8; j++)
///         p = p & 1 ? (p >> 1) ^ POLY : p >> 1;
///     crc_table[i] = p;
/// ```
///
/// Eight rounds of the shift-register step per entry, 2,048 operations for the whole table. As
/// `crc32.c` L238-L240 puts it, "The table is simply the CRC of all possible eight bit values."
/// Deriving all 256 entries costs less than transcribing four of them by hand and validates the
/// other 252 as well.
#[test]
fn crc_table_recomputes_from_the_polynomial() {
    for (index, &entry) in CRC_TABLE.iter().enumerate() {
        // `index` is a table position in `0..256`, so it fits a `u32` on every target.
        let mut p = u32::try_from(index).unwrap();
        for _ in 0..8 {
            p = if p & 1 != 0 { (p >> 1) ^ POLY } else { p >> 1 };
        }

        assert_eq!(
            entry, p,
            "crc32.c L248-L252: crc_table[{index}] is not the CRC of {index}"
        );
    }
}

/// `crc_big_table` is `crc_table` with every word's bytes reversed.
///
/// `crc32.c` L252-L255 builds the two together, one entry per iteration:
///
/// ```c
/// crc_table[i] = p;
/// #ifdef W
/// crc_big_table[i] = byte_swap(p);
/// #endif
/// ```
///
/// The braided body loads a `z_word_t` at a time and picks a table according to the endianness it
/// observes at run time (`crc32.c` L657-L660, and the two branches at L662 and L791), so a fault
/// in this table is invisible on one byte order and total on the other. Deriving it from
/// `crc_table` catches that without needing a big-endian machine to run on.
#[test]
fn crc_big_table_is_the_byte_swapped_crc_table() {
    for (index, (&small, &big)) in CRC_TABLE.iter().zip(CRC_BIG_TABLE.iter()).enumerate() {
        assert_eq!(
            big,
            byte_swapped(small),
            "crc32.c L254: crc_big_table[{index}] is not byte_swap(crc_table[{index}])"
        );
    }
}

/// `x2n_table` is the repeated-squaring ladder of `x`, recomputed from [`POLY`].
///
/// Entry `n` is `x^(2^n) mod p(x)`, each the square of the one before (`crc32.c` L259-L262). The
/// ladder is what makes `crc32_combine` cheap: advancing a check value by `len2` bytes needs only
/// the set bits of `len2`, so combining two multi-gigabyte streams costs a few dozen
/// multiplications rather than a re-read.
///
/// The first six entries are worth naming, because they are recognisable and they anchor the
/// reflected encoding: `x^1` is `0x4000_0000` (bit 30), `x^2` is `0x2000_0000`, `x^4` is
/// `0x0800_0000`, `x^8` is `0x0080_0000`, `x^16` is `0x0000_8000`, and `x^32` is [`POLY`] itself
/// -- which is the definition of the generator with the leading term folded back in.
#[test]
fn x2n_table_recomputes_from_the_polynomial() {
    let derived = reference_x2n_table();

    for (n, (&entry, &expected)) in X2N_TABLE.iter().zip(derived.iter()).enumerate() {
        assert_eq!(
            entry, expected,
            "crc32.c L259-L262: x2n_table[{n}] is not x^(2^{n}) mod p(x)"
        );
    }

    assert_eq!(
        X2N_TABLE[0],
        1 << 30,
        "crc32.c L259-L260: x2n_table[0] is x^1, which is bit 30 reflected"
    );
    assert_eq!(
        X2N_TABLE[5], POLY,
        "x^32 mod p(x) is the generator with its implied leading term folded back in"
    );
}

/// Both braid tables agree with the generator in `crc32.c` L461-L472.
///
/// ```c
/// for (k = 0; k < w; k++) {
///     p = (z_crc_t)x2nmodp((n * w + 3 - k) << 3, 0);
///     ltl[k][0] = 0;
///     big[w - 1 - k][0] = 0;
///     for (i = 1; i < 256; i++) {
///         ltl[k][i] = q = (z_crc_t)multmodp(i << 24, p);
///         big[w - 1 - k][i] = byte_swap(q);
///     }
/// }
/// ```
///
/// Three details of that loop are load-bearing and each is asserted below.
///
/// * The exponent `(N * W + 3 - k) << 3` is how far ahead of the current position byte lane `k`
///   sits, in bits. It depends on both `N` and `W`, so this derivation is the only way to check
///   the tables without a separate hand-transcribed copy per target.
/// * **The big-endian table is indexed in the opposite direction**, `w - 1 - k` against `k`. An
///   implementation that reproduced the values but not the reversal would pass a naive
///   element-count check and decode garbage on a big-endian machine.
/// * Index 0 of every block is written as a literal zero rather than computed, because
///   `multmodp` is documented to require a non-zero first argument (`crc32.c` L160-L162) and
///   `i << 24` is zero at `i == 0`. [`reference_multmodp`] is bounded and so returns `0` there
///   anyway, which is the same value -- but the assertion is written against the literal, since
///   that is what the C code commits to.
///
/// This is `W * 255` multiplications, about 2,000 on a 64-bit target, and it validates all
/// `2 * W * 256` braid entries -- 4,096 of them -- from [`POLY`] alone.
#[test]
fn braid_tables_recompute_from_the_polynomial() {
    let x2n = reference_x2n_table();

    // Iterating the little-endian table rather than `0..W` keeps the block reference in hand; the
    // outer loop still runs exactly `W` times, since that is the table's length.
    for (k, block) in CRC_BRAID_TABLE.iter().enumerate() {
        // `crc32.c` L465. `N * W + 3 - k` is at most `N * W + 3`, so the shift cannot overflow a
        // `u64` on any target, and `k < W <= N * W + 3` keeps the subtraction positive.
        let exponent = u64::try_from((BRAID_BLOCK + 3 - k) << 3).unwrap();
        let p = reference_x2nmodp(&x2n, exponent, 0);

        // The big-endian table is written in the opposite direction, `w - 1 - k` against `k`.
        let mirrored = W - 1 - k;
        let big_block = &CRC_BRAID_BIG_TABLE[mirrored];

        // `crc32.c` L466-L467.
        assert_eq!(
            block[0], 0,
            "crc32.c L466: crc_braid_table[{k}][0] is written as zero, not computed"
        );
        assert_eq!(
            big_block[0], 0,
            "crc32.c L467: crc_braid_big_table[{mirrored}][0] is written as zero, not computed"
        );

        // `skip(1)` leaves index 0 to the two assertions above, matching the C loop's `i = 1`
        // start; both tables are 256 long, so `zip` drops nothing.
        for (index, (&entry, &big_entry)) in block.iter().zip(big_block.iter()).enumerate().skip(1)
        {
            // A table position below 256, so it fits a `u32` on every target.
            let i = u32::try_from(index).unwrap();

            // `crc32.c` L469: multmodp(i << 24, p). The shift lifts the byte value into the top
            // eight bits, which in the reflected encoding is the byte's lowest-power position.
            let expected = reference_multmodp(i << 24, p);

            assert_eq!(
                entry, expected,
                "crc32.c L469: crc_braid_table[{k}][{index}] disagrees with the generator"
            );
            assert_eq!(
                big_entry,
                byte_swapped(expected),
                "crc32.c L470: crc_braid_big_table[{mirrored}][{index}] disagrees with the generator"
            );
        }
    }
}

/// The big-endian braid table is the little-endian one, byte-reversed and block-reversed.
///
/// The same relation the previous test derives, asserted directly against the shipped tables
/// rather than against a recomputation. It costs 2,048 comparisons and no multiplications, and it
/// isolates the failure: if this passes and the derivation does not, both tables are wrong
/// together, which points at [`X2N_TABLE`]; if this fails and the derivation passes for the
/// little-endian half, only the big-endian transcription is at fault.
#[test]
fn braid_big_table_mirrors_the_braid_table() {
    for (j, block) in CRC_BRAID_BIG_TABLE.iter().enumerate() {
        // `crc32.c` L467 and L470 write index `w - 1 - k` of the big table while writing index
        // `k` of the little one, so reading the big table at `j` means reading the little one at
        // `W - 1 - j`.
        let source = &CRC_BRAID_TABLE[W - 1 - j];

        for (i, (&big, &small)) in block.iter().zip(source.iter()).enumerate() {
            assert_eq!(
                big,
                byte_swapped(small),
                "crc_braid_big_table[{j}][{i}] is not byte_swap(crc_braid_table[{}][{i}])",
                W - 1 - j
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// get_crc_table
// ---------------------------------------------------------------------------------------------

/// `get_crc_table` hands back the byte-wise table itself.
///
/// `crc32.c` L482-L487 returns `(const z_crc_t FAR *)crc_table`, declared at `zlib.h` L2035
/// among the undocumented functions. The C comment at L478-L481 gives its two purposes: to let an
/// assembler `crc32()` share the table, and "to force the generation of the CRC tables in a
/// threaded application". Only the first applies here; see the module documentation on why the
/// second cannot.
#[test]
fn get_crc_table_returns_the_byte_wise_table() {
    let table = get_crc_table();

    assert_eq!(
        table.len(),
        256,
        "zlib.h L2035 promises a byte-indexed table"
    );
    assert_eq!(
        table, &CRC_TABLE,
        "crc32.c L486 returns crc_table itself, not a copy"
    );
}

/// Successive calls return one and the same `'static` table, at one and the same address.
///
/// Contents alone would not settle it: a function that rebuilt the table into a leaked allocation
/// on every call would satisfy an equality check while breaking the C contract, which hands out a
/// pointer callers may hold indefinitely and compare. Pointer identity is the property that says
/// the table is compiler-materialized `const` data and that this accessor initializes nothing.
///
/// The assertion is deterministic rather than incidental: `get_crc_table` contains exactly one
/// `&CRC_TABLE` expression, so both calls return the same promoted anonymous static.
#[test]
fn get_crc_table_returns_the_same_static_table_every_call() {
    let first = get_crc_table();
    let second = get_crc_table();

    assert!(
        ptr::eq(first, second),
        "get_crc_table must be a pure accessor over const data, not an initializer"
    );
    assert_eq!(first, second, "and the contents must agree, trivially");
}

// ---------------------------------------------------------------------------------------------
// Known answers, and the independent reference
// ---------------------------------------------------------------------------------------------

/// The fixture generator still produces the bytes [`ORACLE_CHECKS`] was measured against.
///
/// Guard rather than test: `common::lcg_fill` belongs to the shared test-support module, and its
/// output is the input every hard-coded check value in this file was computed from. Asserting a
/// short prefix of the stream means a change there fails once, legibly, instead of failing
/// eighty-eight checksums with no hint as to why.
#[test]
fn the_fixture_generator_still_produces_the_measured_stream() {
    let buf = fixture(FIXTURE_PREFIX.len());

    assert_eq!(
        buf.as_slice(),
        FIXTURE_PREFIX.as_slice(),
        "common::lcg_fill changed; the hard-coded oracle values in this file were measured \
         against its previous output and must be re-measured"
    );
}

/// An empty slice is a zero-length update, and therefore the identity.
///
/// The two complements cancel: `!(!start)` is `start`, so nothing is folded and nothing changes.
/// That is `crc32.c` L635 and L940 with no loop in between.
///
/// This is emphatically **not** the C entry point's `Z_NULL` behaviour. `crc32.c` L627-L628
/// returns the initial value `0` for a null buffer *regardless of the `crc` argument*, so
/// `crc32(5, Z_NULL, 0)` is `0` while `crc32(5, &[])` is `5`. A `&[u8]` cannot be null, so that
/// asymmetry belongs to the facade and is honoured there; the distinction is asserted here so
/// that nobody later "fixes" this function into returning zero.
#[test]
fn an_empty_slice_is_the_identity_on_the_check_value() {
    assert_eq!(
        crc32(0, b""),
        0,
        "a fresh check value over no input is zero -- the value zlib.h L1858's \
         `crc32(0L, Z_NULL, 0)` idiom obtains"
    );

    for start in ORACLE_SEEDS {
        assert_eq!(
            crc32(start, &[]),
            start,
            "crc32({start:#010x}, &[]) must return the incoming value unchanged"
        );
        assert_eq!(crc32_z(start, &[]), start);
        assert_eq!(reference_crc32(start, &[]), start);
    }
}

/// The canonical short vectors match the reference C implementation, byte for byte.
///
/// Every value here was read off the C library built from the in-tree `crc32.c`; the procedure is
/// the one described on [`ORACLE_CHECKS`]. The first one is additionally derived by hand, since a
/// single fully worked example is what makes the rest inspectable:
///
/// `crc32(0, b"a")`: pre-conditioning gives `!0 == 0xffff_ffff`; the low byte is `0xff` and the
/// input byte is `0x61`, so the table index is `0xff ^ 0x61 == 0x9e`; `CRC_TABLE[0x9e]` is
/// `0x17b7_be43`; the step at `crc32.c` L936 -- textually identical to L649 -- yields `(0xffff_ffff >> 8) ^ 0x17b7_be43`, which is
/// `0x00ff_ffff ^ 0x17b7_be43 == 0x1748_41bc`; post-conditioning gives `!0x1748_41bc`, which is
/// `0xe8b7_be43`.
///
/// `b"123456789"` is the check value every CRC catalogue lists for CRC-32/ISO-HDLC, so it is the
/// one vector a reader can verify against an outside source without leaving their chair.
///
/// The two `test/example.c` fixtures are included with their **terminating NUL**, because that
/// suite passes `strlen(hello) + 1` at L69, L95, L175, L341 and L434 and `sizeof(dictionary)` at
/// L426 and L477. `corpus::HELLO` is therefore 14 bytes and not 13, and the 13-byte value is
/// listed beside it so that an off-by-one in the fixture is distinguishable from an error in the
/// engine.
#[test]
fn the_canonical_vectors_match_the_c_implementation() {
    const VECTORS: [(&[u8], u32); 7] = [
        (b"", 0x0000_0000),
        (b"a", 0xe8b7_be43),
        (b"abc", 0x3524_41c2),
        (b"123456789", 0xcbf4_3926),
        (b"hello, hello!", 0xb39a_dc9b),
        // `corpus::HELLO`, 14 bytes including the NUL.
        (b"hello, hello!\0", 0xb56c_3f9d),
        // `corpus::DICTIONARY`, 6 bytes including the NUL.
        (b"hello\0", 0xd6ef_d93e),
    ];

    // Confirm the hand derivation quoted above rather than merely asserting its conclusion.
    assert_eq!(
        CRC_TABLE[0x9e], 0x17b7_be43,
        "the worked example on this test reads CRC_TABLE[0xff ^ 0x61]"
    );

    for (input, expected) in VECTORS {
        assert_eq!(
            crc32(0, input),
            expected,
            "crc32(0, {input:?}) disagrees with the C implementation"
        );
        assert_eq!(crc32_z(0, input), expected, "crc32_z must agree with crc32");
        assert_eq!(
            reference_crc32(0, input),
            expected,
            "the in-file byte-at-a-time reference must agree too"
        );
    }

    // The two `test/example.c` fixtures, reached through the shared corpus rather than as
    // literals, so that a change to either constant fails here.
    assert_eq!(crc32(0, corpus::HELLO), 0xb56c_3f9d);
    assert_eq!(crc32(0, corpus::DICTIONARY), 0xd6ef_d93e);
    assert_eq!(
        crc32(0, corpus::SINGLE_BYTE),
        0xe8b7_be43,
        "a single 0x61 byte"
    );
    assert_eq!(crc32(0, corpus::EMPTY), 0x0000_0000);
}

/// The engine agrees with the in-file reference on every payload class of the minimal corpus.
///
/// The eight classes are empty, single byte, the `test/example.c` string, a long run of one byte,
/// incompressible pseudo-random data, natural-language text, all 256 byte values in order, and a
/// payload that exceeds the 32 `KiB` window. They are the plan's committed minimal corpus, and
/// between them they cover every byte value, every degenerate length and a payload long enough to
/// run hundreds of braided blocks.
///
/// Each is also checked with every seed in [`ORACLE_SEEDS`], because a fault in the pre- or
/// post-conditioning is invisible at `start == 0` for exactly one input length and visible
/// everywhere else.
#[test]
fn the_engine_agrees_with_the_reference_on_every_corpus_class() {
    for (name, whole) in corpus::all() {
        let payload = capped(&whole);

        for start in ORACLE_SEEDS {
            let expected = reference_crc32(start, payload);

            assert_eq!(
                crc32(start, payload),
                expected,
                "crc32({start:#010x}, corpus::{name}) disagrees with the byte-at-a-time reference \
                 ({} bytes)",
                payload.len()
            );
            assert_eq!(
                crc32_z(start, payload),
                expected,
                "crc32_z({start:#010x}, corpus::{name}) disagrees with crc32"
            );
        }
    }
}

/// `crc32` and `crc32_z` are the same function.
///
/// The C declarations differ only in the width of the length argument -- `uInt` at
/// `zlib.h` L1848, `z_size_t` at `zlib.h` L1866-L1867, whose documentation reads "Same as
/// `crc32()`, but with a `size_t` length" -- and `crc32` is a one-line forwarder to `crc32_z`
/// (`crc32.c` L946-L951). A Rust slice carries its own length, so the distinction disappears and
/// the two must be indistinguishable. Both names are nevertheless kept, because the facade has to
/// export both C symbols and the reference sources call both from inside the library
/// (`deflate.c` L233 and L976).
#[test]
fn crc32_and_crc32_z_are_indistinguishable() {
    let buf = fixture(FIXTURE_LEN);

    for start in ORACLE_SEEDS {
        for len in boundary_lengths() {
            let slice = &buf[..len];
            assert_eq!(
                crc32(start, slice),
                crc32_z(start, slice),
                "crc32 and crc32_z disagree at start {start:#010x}, length {len}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Length sweeps -- crossing the braid threshold and hitting every remainder class
// ---------------------------------------------------------------------------------------------

/// The engine reproduces the reference C implementation's answers exactly.
///
/// 88 measured values: four starting check values against 22 prefix lengths of the fixture
/// stream. This is the one test in the file whose expected values come from *outside* the Rust
/// implementation entirely, so it is what distinguishes "self-consistent" from "correct". See
/// [`ORACLE_CHECKS`] for how the values were obtained and why they are target-independent.
#[test]
fn the_oracle_matrix_matches_the_c_implementation() {
    let buf = fixture(FIXTURE_LEN);

    for (row, start) in ORACLE_SEEDS.into_iter().enumerate() {
        for (column, len) in ORACLE_LENGTHS.into_iter().enumerate() {
            let expected = ORACLE_CHECKS[row][column];
            let slice = &buf[..len];

            assert_eq!(
                crc32(start, slice),
                expected,
                "crc32({start:#010x}, fixture[..{len}]) disagrees with the C implementation"
            );
            assert_eq!(
                reference_crc32(start, slice),
                expected,
                "the in-file reference disagrees with the C implementation at \
                 start {start:#010x}, length {len}"
            );
        }
    }
}

/// Every length from 0 to [`SHORT_SWEEP_MAX`] folds to the same value as the reference.
///
/// Exhaustive rather than sampled, and that is the point. The braided body consumes `N * W` bytes
/// per iteration and hands `len % (N * W)` bytes to the byte tail at `crc32.c` L922-L937, with a
/// further `W - 1` bytes possible in the alignment prologue at L644-L647. Sweeping every length
/// visits every combination of prologue length, block count and tail length that can occur, which
/// no finite set of interesting lengths can promise.
///
/// The bound covers both possible thresholds -- 47 on a 64-bit target, 19 on a 32-bit one -- and
/// at least six full blocks beyond either. It is not reduced under Miri; see [`SHORT_SWEEP_MAX`].
#[test]
fn every_length_up_to_the_short_bound_matches_the_reference() {
    let buf = fixture(SHORT_SWEEP_MAX);

    for len in 0..=SHORT_SWEEP_MAX {
        let slice = &buf[..len];
        assert_eq!(
            crc32(0, slice),
            reference_crc32(0, slice),
            "length {len} disagrees with the byte-at-a-time reference"
        );
    }
}

/// The sweep continued from [`SHORT_SWEEP_MAX`] to [`LONG_SWEEP_MAX`].
///
/// Skipped under Miri **only** for interpreter speed: it folds about eight million bytes, which is
/// seconds natively and minutes in the interpreter. Everything structural it covers -- every
/// prologue length, every remainder class, both thresholds -- is already covered by the
/// unconditional short sweep above and by the boundary test below, both of which run under Miri.
/// It still runs natively, in `cargo test` and in CI, where it adds coverage of long block runs
/// and of lengths far beyond any threshold.
#[test]
#[cfg_attr(miri, ignore)]
fn the_long_length_sweep_matches_the_reference() {
    let buf = fixture(LONG_SWEEP_MAX);

    for len in (SHORT_SWEEP_MAX + 1)..=LONG_SWEEP_MAX {
        let slice = &buf[..len];
        assert_eq!(
            crc32(0, slice),
            reference_crc32(0, slice),
            "length {len} disagrees with the byte-at-a-time reference"
        );
    }
}

/// Each structural boundary of the braided body is checked on its own, and named.
///
/// The sweeps above would catch any failure here, but they would report it as one of 4,096
/// lengths. This test reports it as "the braid threshold", which is the difference between a
/// diagnosis and a data point. Every length is computed from `N` and `W`; see
/// [`boundary_lengths`].
///
/// Never skipped, under Miri or anything else: these are the lengths at which the implementation
/// changes what it does.
#[test]
fn the_structural_boundaries_match_the_reference() {
    let buf = fixture(FIXTURE_LEN);

    for len in boundary_lengths() {
        let slice = &buf[..len];
        let braided = if len >= MIN_BRAID_LEN {
            "braided"
        } else {
            "byte-at-a-time"
        };

        for start in ORACLE_SEEDS {
            assert_eq!(
                crc32(start, slice),
                reference_crc32(start, slice),
                "boundary length {len} ({braided}, threshold {MIN_BRAID_LEN}, W = {W}, N = {N}) \
                 disagrees at start {start:#010x}"
            );
        }
    }
}

/// Uniform buffers match the reference at every structural boundary.
///
/// All-zero and all-`0xff` inputs are worth their own test because they are the two cases where a
/// table-indexing fault can cancel itself out. Pseudo-random data touches a different table entry
/// on essentially every step, so a wrong index is very likely to show; a run of one byte drives
/// the index from the running state alone, and a lane mix-up in the braided body can leave the
/// answer unchanged. `0x00` and `0xff` are also the two bytes for which `crc ^ byte` degenerates
/// -- to `crc` and to `!crc` -- which is exactly when an omitted exclusive-or is invisible.
#[test]
fn uniform_buffers_match_the_reference_at_every_boundary() {
    for filler in [0x00_u8, 0xff_u8] {
        for len in boundary_lengths() {
            let buf = vec![filler; len];

            for start in ORACLE_SEEDS {
                assert_eq!(
                    crc32(start, &buf),
                    reference_crc32(start, &buf),
                    "{len} bytes of {filler:#04x} disagree at start {start:#010x}"
                );
            }
        }
    }

    // Two absolute anchors from the C implementation, so that this test is not purely relative.
    // 256 bytes is well past both thresholds and runs several full braided blocks.
    assert_eq!(
        crc32(0, &[0x00_u8; 256]),
        0x0d96_8558,
        "256 zero bytes, measured against the C implementation"
    );
    assert_eq!(
        crc32(0, &[0xff_u8; 256]),
        0xfea8_a821,
        "256 0xff bytes, measured against the C implementation"
    );
}

// ---------------------------------------------------------------------------------------------
// Resumability -- chunked accumulation must equal a single call
//
// This is not a nicety. `deflate` folds a gzip member's check value over whatever `avail_in` each
// call happens to carry (`deflate.c` L233, reached from `read_buf`), and `inflate` folds it over
// whatever it manages to produce (`inflate.c` L300-L305). Neither controls the chunk boundaries;
// the caller does. So `crc32(crc32(0, a), b) == crc32(0, a ++ b)` for every split is the property
// that makes the streaming API usable at all, and it holds because the post-conditioning of one
// call is undone by the pre-conditioning of the next.
// ---------------------------------------------------------------------------------------------

/// Splitting the input at *every* offset gives the same value as one call.
///
/// [`SPLIT_LEN`] offsets over a [`SPLIT_LEN`]-byte buffer, so both halves take every possible
/// combination of paths: short-and-short, short-and-braided, braided-and-short and
/// braided-and-braided, with every possible alignment prologue on the second half. A resumption
/// fault that only showed up when the tail began mid-word would survive a handful of chosen
/// offsets and cannot survive this.
#[test]
fn splitting_at_every_offset_matches_a_single_call() {
    let buf = fixture(SPLIT_LEN);

    for start in ORACLE_SEEDS {
        let whole = crc32(start, &buf);

        for split in 0..=SPLIT_LEN {
            let (head, tail) = buf.split_at(split);
            assert_eq!(
                crc32(crc32(start, head), tail),
                whole,
                "splitting {SPLIT_LEN} bytes at offset {split} changed the check value \
                 (start {start:#010x})"
            );
        }
    }
}

/// Splitting a larger buffer at the structurally interesting offsets gives the same value.
///
/// The offsets are the ones at which the second half changes which path it takes or which
/// alignment it starts from: one byte, one word either side, the braid threshold either side, one
/// full block, and the midpoint. All are derived from `N` and `W`, and offsets beyond the buffer
/// are skipped rather than clamped so that a shrunken buffer cannot silently turn several
/// distinct cases into the same one.
#[test]
fn splitting_a_large_buffer_at_the_structural_offsets_matches_a_single_call() {
    let buf = fixture(LARGE_SPLIT_LEN);

    let offsets = [
        1,
        W - 1,
        W,
        W + 1,
        MIN_BRAID_LEN - 1,
        MIN_BRAID_LEN,
        MIN_BRAID_LEN + 1,
        BRAID_BLOCK,
        LARGE_SPLIT_LEN / 2,
        LARGE_SPLIT_LEN - 1,
    ];

    for start in ORACLE_SEEDS {
        let whole = crc32(start, &buf);

        for split in offsets {
            assert!(
                split <= buf.len(),
                "offset {split} exceeds the {LARGE_SPLIT_LEN}-byte buffer; \
                 LARGE_SPLIT_LEN must stay above every structural offset"
            );

            let (head, tail) = buf.split_at(split);
            assert_eq!(
                crc32(crc32(start, head), tail),
                whole,
                "splitting {LARGE_SPLIT_LEN} bytes at offset {split} changed the check value \
                 (start {start:#010x})"
            );
        }
    }
}

/// Feeding the input one byte at a time gives the same value as one call.
///
/// The limiting case of the property above, and the one the `gz*` layer reaches through `gzputc`.
/// It also forces the byte-at-a-time path on every single call, so it is the configuration in
/// which the braided body never runs at all -- which makes it a check that resumption does not
/// depend on the fast path having been entered.
#[test]
fn byte_at_a_time_accumulation_matches_a_single_call() {
    let buf = fixture(SPLIT_LEN);

    for start in ORACLE_SEEDS {
        let mut accumulated = start;
        for &byte in &buf {
            accumulated = crc32(accumulated, &[byte]);
        }

        assert_eq!(
            accumulated,
            crc32(start, &buf),
            "one byte per call disagrees with a single call over {SPLIT_LEN} bytes \
             (start {start:#010x})"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Backend output neutrality
//
// The plan admits vectorization for the two checksum modules and nowhere else, and it admits it on
// exactly one ground: a check value is a single scalar in GF(2) however its linear recurrence is
// evaluated, so independent partial remainders recombine exactly and the emitted bytes cannot
// change. Match finding gets no such licence, because the order in which candidates are examined
// decides which match is emitted. The two cases look alike and must not be conflated.
//
// "Cannot change" is a claim, so it is tested: the backends are compared against each other bit
// for bit, and against the byte-at-a-time reference. `Generic` and `Braid` are always reachable;
// `Simd` is compiled only under the `simd` feature, so the sections that name it are gated and CI
// runs this file in both configurations.
// ---------------------------------------------------------------------------------------------

/// Folds `buf` into `start` through one named backend, applying the conditioning [`crc32`] applies.
///
/// [`Crc32Backend::update`] takes and returns an already pre-conditioned state and complements
/// nothing -- that is what lets the braided backend hand its prologue and remainder bytes to the
/// byte-at-a-time one. So calling a backend directly means bracketing it with the two complements
/// of `crc32.c` L635 and L940, which is exactly what [`crc32`] does around its own selection.
fn through<B: Crc32Backend>(start: u32, buf: &[u8]) -> u32 {
    !B::update(!start, buf)
}

/// Every backend agrees at every structural boundary, for every starting value.
///
/// The boundaries are where the backends differ most: [`Braid`] delegates the whole buffer to
/// [`Generic`] below [`MIN_BRAID_LEN`] and `Simd` delegates to [`Braid`] below its own, slightly
/// larger, threshold. So these lengths are precisely the ones at which one backend starts doing
/// something the others are not, and they are the lengths at which a mismatch is possible at all.
#[test]
fn every_backend_agrees_at_the_structural_boundaries() {
    let buf = fixture(FIXTURE_LEN);

    for len in boundary_lengths() {
        let slice = &buf[..len];

        for start in ORACLE_SEEDS {
            let expected = reference_crc32(start, slice);

            assert_eq!(
                through::<Generic>(start, slice),
                expected,
                "{} disagrees with the reference at length {len}, start {start:#010x}",
                Generic::NAME
            );
            assert_eq!(
                through::<Braid>(start, slice),
                expected,
                "{} disagrees with the reference at length {len}, start {start:#010x}",
                Braid::NAME
            );

            #[cfg(feature = "simd")]
            assert_eq!(
                through::<Simd>(start, slice),
                expected,
                "{} disagrees with the reference at length {len}, start {start:#010x}",
                Simd::NAME
            );

            // And the public entry point, whichever backend the build selected, must agree with
            // all of them. This is the assertion that makes the CI matrix meaningful: running the
            // identical file with and without `--features simd` pins both selections to one
            // answer.
            assert_eq!(
                crc32(start, slice),
                expected,
                "the selected backend disagrees with the reference at length {len}, \
                 start {start:#010x}"
            );
        }
    }
}

/// Every backend agrees on every payload class of the minimal corpus.
///
/// Where the boundary test varies the length over one pseudo-random stream, this varies the
/// *content*: a long run of one byte, incompressible data, natural-language text, all 256 byte
/// values in order, and a payload past the 32 `KiB` window. Uniform and highly structured content
/// is where a lane mix-up in a wide backend can cancel out, so content variation is not
/// redundant with length variation.
///
/// Skipped under Miri for interpreter speed alone: the classes total roughly 45 `KiB`, folded once
/// per backend per seed. The boundary test above covers every backend on every structural length
/// and does run under Miri, so nothing about backend equivalence goes unchecked there.
#[test]
#[cfg_attr(miri, ignore)]
fn every_backend_agrees_on_every_corpus_class() {
    for (name, whole) in corpus::all() {
        let payload = capped(&whole);

        for start in ORACLE_SEEDS {
            let expected = reference_crc32(start, payload);

            assert_eq!(
                through::<Generic>(start, payload),
                expected,
                "{} disagrees on corpus::{name} at start {start:#010x}",
                Generic::NAME
            );
            assert_eq!(
                through::<Braid>(start, payload),
                expected,
                "{} disagrees on corpus::{name} at start {start:#010x}",
                Braid::NAME
            );

            #[cfg(feature = "simd")]
            assert_eq!(
                through::<Simd>(start, payload),
                expected,
                "{} disagrees on corpus::{name} at start {start:#010x}",
                Simd::NAME
            );

            assert_eq!(
                crc32(start, payload),
                expected,
                "the selected backend disagrees on corpus::{name} at start {start:#010x}"
            );
        }
    }
}

/// The backends are distinguishable by name, so a failure can say which one produced a value.
///
/// [`Crc32Backend::NAME`] exists for diagnostics and benchmark labels, and it is only useful if
/// the names are distinct and stable. Asserting the literals pins them, since a benchmark row or a
/// differential report that renamed itself between runs would be worse than one with no label.
#[test]
fn backend_names_are_distinct_and_stable() {
    assert_eq!(Generic::NAME, "generic");
    assert_eq!(Braid::NAME, "braid");
    assert_ne!(Generic::NAME, Braid::NAME);

    #[cfg(feature = "simd")]
    {
        assert_eq!(Simd::NAME, "simd");
        assert_ne!(Simd::NAME, Generic::NAME);
        assert_ne!(Simd::NAME, Braid::NAME);
    }
}

/// Every backend reproduces the reference C implementation's measured answers.
///
/// The backend counterpart of [`the_oracle_matrix_matches_the_c_implementation`]. Agreeing with
/// each other only proves the backends are consistent; agreeing with [`ORACLE_CHECKS`] proves each
/// one independently reproduces the reference implementation, which is the requirement the plan
/// actually states.
#[test]
fn every_backend_matches_the_oracle_matrix() {
    let buf = fixture(FIXTURE_LEN);

    for (row, start) in ORACLE_SEEDS.into_iter().enumerate() {
        for (column, len) in ORACLE_LENGTHS.into_iter().enumerate() {
            let expected = ORACLE_CHECKS[row][column];
            let slice = &buf[..len];

            assert_eq!(
                through::<Generic>(start, slice),
                expected,
                "{} disagrees with the C implementation at start {start:#010x}, length {len}",
                Generic::NAME
            );
            assert_eq!(
                through::<Braid>(start, slice),
                expected,
                "{} disagrees with the C implementation at start {start:#010x}, length {len}",
                Braid::NAME
            );

            #[cfg(feature = "simd")]
            assert_eq!(
                through::<Simd>(start, slice),
                expected,
                "{} disagrees with the C implementation at start {start:#010x}, length {len}",
                Simd::NAME
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The combine family
//
// Five exported symbols over one operation: given the check values of two byte sequences and the
// length of the second, produce the check value of their concatenation without re-reading either.
// `crc32.c` implements it as `combine_op(crc1, crc2, combine_gen(len2))` (L976-L980), and splits
// the two halves apart so that a caller combining many equal-length blocks can compute the
// operator once (`zlib.h` L1884-L1892).
//
// `multmodp` and `x2nmodp` are `local` in C and crate-private here, so everything below reaches
// them only through these five functions.
// ---------------------------------------------------------------------------------------------

/// Combining two check values reproduces the check value of the concatenation.
///
/// The property the whole family exists for, and the only one a caller cares about:
///
/// ```text
/// combine64(crc32(0, a), crc32(0, b), b.len()) == crc32(0, a ++ b)
/// ```
///
/// The length matrix includes both empty cases, both singletons, and pairs whose parts straddle
/// [`MIN_BRAID_LEN`] in each direction, so the two check values being combined were themselves
/// produced by different code paths. Lengths are derived from `N` and `W` where they are
/// structural and written plainly where they are not.
#[test]
fn combining_two_check_values_reproduces_the_concatenation() {
    let buf = fixture(FIXTURE_LEN);

    let pairs = [
        (0, 0),
        (0, 1),
        (1, 0),
        (1, 1),
        (2, 3),
        (W - 1, W),
        (W, W - 1),
        (0, MIN_BRAID_LEN),
        (MIN_BRAID_LEN, 0),
        (MIN_BRAID_LEN - 1, 1),
        (1, MIN_BRAID_LEN - 1),
        (MIN_BRAID_LEN, MIN_BRAID_LEN),
        (BRAID_BLOCK, BRAID_BLOCK),
        (BRAID_BLOCK - 1, BRAID_BLOCK + 1),
        (255, 1),
        (1, 255),
        (600, 424),
    ];

    for (len1, len2) in pairs {
        let first = &buf[..len1];
        let second = &buf[len1..len1 + len2];

        let crc1 = crc32(0, first);
        let crc2 = crc32(0, second);

        // `crc32(crc32(0, a), b)` is the concatenation computed by re-reading `b`; the combine
        // family must reach the same answer from the two check values and a length alone.
        let expected = crc32(crc1, second);

        // `len2` is a test-chosen buffer length well under `i64::MAX`.
        let signed = i64::try_from(len2).unwrap();

        assert_eq!(
            crc32_combine64(crc1, crc2, signed),
            expected,
            "combine64 failed for a {len1}-byte prefix and a {len2}-byte suffix"
        );
        assert_eq!(
            crc32_combine(crc1, crc2, signed),
            expected,
            "combine failed for a {len1}-byte prefix and a {len2}-byte suffix"
        );
    }
}

/// The two-step form composes into the one-step form exactly.
///
/// `crc32.c` L976-L978 defines `combine64` as `combine_op(crc1, crc2, combine_gen64(len2))`, and
/// `zlib.h` L1884-L1892 documents the split as an optimisation for callers combining many blocks
/// of the same length: compute the operator once with `combine_gen`, then apply it repeatedly with
/// `combine_op`. Those callers get the same answers as the one-step form or the optimisation is a
/// trap, so the identity is asserted rather than assumed -- over both the `z_off64_t` pair and the
/// `z_off_t` pair, since all four are separately exported symbols.
#[test]
fn the_two_step_form_composes_into_the_one_step_form() {
    let lengths = [0_i64, 1, 2, 3, 4, 5, 7, 8, 16, 47, 48, 1_024, 65_536];
    let values = [
        (0x0000_0000_u32, 0x0000_0000_u32),
        (0x0000_0000, 0xdead_beef),
        (0x1122_3344, 0x5566_7788),
        (0xffff_ffff, 0x0000_0001),
        (0xcbf4_3926, 0xe8b7_be43),
    ];

    for len2 in lengths {
        let op = crc32_combine_gen64(len2);
        assert_eq!(
            crc32_combine_gen(len2),
            op,
            "crc32_combine_gen and crc32_combine_gen64 must agree at len2 {len2}"
        );

        for (crc1, crc2) in values {
            let one_step = crc32_combine64(crc1, crc2, len2);

            assert_eq!(
                crc32_combine_op(crc1, crc2, op),
                one_step,
                "combine_op(.., combine_gen64({len2})) must equal combine64(.., {len2}) \
                 for ({crc1:#010x}, {crc2:#010x})"
            );
            assert_eq!(
                crc32_combine(crc1, crc2, len2),
                one_step,
                "combine and combine64 must agree at len2 {len2}"
            );
        }
    }
}

/// A zero-length second block leaves the first check value alone.
///
/// `zlib.h` L1874-L1878 says `crc32_combine` returns the check value of the concatenation, so
/// concatenating nothing must be the identity. It is not a special case in the code, which is what
/// makes it worth asserting: `combine_gen64(0)` is `x^0`, which is `0x8000_0000` in the reflected
/// encoding, and `multmodp(x^0, crc1)` is `crc1`, so `combine64(crc1, 0, 0)` reduces to
/// `crc1 ^ 0`. The identity therefore falls out of the arithmetic rather than being guarded.
///
/// Note the second argument has to be the check value of the *empty* sequence, which is `0`. That
/// is the one place the empty-slice identity and the `Z_NULL` initial value coincide, and it is
/// why the C usage idiom `crc32(0L, Z_NULL, 0)` works as a way of getting a starting value.
#[test]
fn a_zero_length_second_block_is_the_identity() {
    let empty = crc32(0, b"");
    assert_eq!(empty, 0, "the check value of nothing is zero");

    assert_eq!(
        crc32_combine_gen64(0),
        0x8000_0000,
        "x^0 is bit 31 in the reflected encoding; measured against the C implementation"
    );

    for crc1 in ORACLE_SEEDS {
        assert_eq!(
            crc32_combine64(crc1, empty, 0),
            crc1,
            "combining {crc1:#010x} with the empty sequence must leave it unchanged"
        );
        assert_eq!(crc32_combine(crc1, empty, 0), crc1);
    }

    // Non-zero `crc2` at `len2 == 0` is the plain exclusive-or, which is the same statement
    // written without the identity element hidden inside it.
    assert_eq!(
        crc32_combine64(0x1122_3344, 0x5566_7788, 0),
        0x4444_44cc,
        "0x11223344 ^ 0x55667788; measured against the C implementation"
    );
}

/// A negative second length yields zero, for every negative value.
///
/// `crc32.c` L954-L956 opens `crc32_combine_gen64` with `if (len2 < 0) return 0;`, and
/// `zlib.h` L1887-L1888 documents the contract: "len2 must be non-negative", with zero returned
/// otherwise. The guard exists because `x2nmodp`'s own comment requires `n` not be negative
/// (`crc32.c` L181-L182); without it a negative length would be reinterpreted as an enormous
/// positive one.
///
/// `i64::MIN` is included deliberately: it is the one value whose negation overflows, so an
/// implementation that reached for `abs()` or `-len2` on the way to a conversion would fail here
/// and nowhere else.
///
/// The consequence propagates: a zero operator makes `combine_op` return zero, so
/// `combine64(_, _, negative)` is zero as well rather than one of its arguments.
#[test]
fn a_negative_second_length_yields_zero() {
    let negatives = [
        -1_i64,
        -2,
        -47,
        -1_024,
        -4_294_967_296,
        i64::MIN + 1,
        i64::MIN,
    ];

    for len2 in negatives {
        assert_eq!(
            crc32_combine_gen64(len2),
            0,
            "crc32.c L955-L956: combine_gen64({len2}) must be zero"
        );
        assert_eq!(
            crc32_combine_gen(len2),
            0,
            "crc32.c L965: combine_gen({len2}) must be zero"
        );

        assert_eq!(
            crc32_combine64(0x1122_3344, 0x5566_7788, len2),
            0,
            "a zero operator propagates: combine64(.., {len2}) must be zero"
        );
        assert_eq!(crc32_combine(0x1122_3344, 0x5566_7788, len2), 0);
    }
}

/// A zero operator makes `combine_op` return zero -- not `crc2`.
///
/// This one is easy to get wrong in the plausible direction. `combine_op` is
/// `multmodp(op, crc1) ^ crc2`, and `multmodp(0, crc1)` would be zero, so the "obvious" answer for
/// a zero operator is `crc2`. That is **not** what the C implementation does.
/// `crc32.c` L969-L973 reads, in full:
///
/// ```c
/// uLong ZEXPORT crc32_combine_op(uLong crc1, uLong crc2, uLong op) {
///     if (op == 0)
///         return 0;
///     return multmodp(op, crc1 & 0xffffffff) ^ (crc2 & 0xffffffff);
/// }
/// ```
///
/// The early return is unconditional and discards both check values. Its purpose is not
/// arithmetic but self-protection: `multmodp` cannot terminate when its first argument is zero
/// (`crc32.c` L160-L162), so this guard is the only thing keeping a caller who passed a negative
/// length -- and so got a zero operator out of `combine_gen` -- from hanging the process.
///
/// Verified against the C library rather than reasoned about: `crc32_combine_op(0x12345678,
/// 0xdeadbeef, 0)` returns `0x00000000` there. This test asserts what the source does, and the
/// quotation above is the line it was checked against.
#[test]
fn a_zero_operator_makes_combine_op_return_zero() {
    let values = [
        (0x0000_0000_u32, 0x0000_0000_u32),
        (0x1234_5678, 0xdead_beef),
        (0xffff_ffff, 0xffff_ffff),
        (0x0000_0000, 0xffff_ffff),
        (0xffff_ffff, 0x0000_0000),
    ];

    for (crc1, crc2) in values {
        assert_eq!(
            crc32_combine_op(crc1, crc2, 0),
            0,
            "crc32.c L970-L971: combine_op({crc1:#010x}, {crc2:#010x}, 0) must be zero, \
             not crc2"
        );
    }
}

/// `combine_op` terminates promptly for the degenerate operands, and returns a defined value.
///
/// This test looks pointless and is not. `multmodp` in C is an unbounded `for (;;)` whose only
/// exit is finding the lowest set bit of its first argument (`crc32.c` L168-L177), guarded by
/// nothing but the comment at L160-L162: "For speed, this requires that a not be zero." Hand it a
/// zero and `m` shifts down past bit 0, `a & m` stays false forever, and the loop never ends.
///
/// This port bounds that loop at 32 iterations, which is exact rather than defensive -- a non-zero
/// argument always terminates within 32 -- and which turns the undefined point into a defined one.
/// So there are two things to check, and the value matters less than the fact that control
/// returns:
///
/// * `op == 0` must return, via the early guard, and must return `0`.
/// * `crc1 == 0` must return. This is the *second* argument being zero, which terminates in C too,
///   but it is the case where the accumulator never receives anything and so is worth pinning:
///   the result is `crc2` alone.
///
/// A regression here does not produce a wrong answer. It produces a test run that never finishes,
/// which is why the case is called out by name instead of being left to the general tests.
#[test]
fn combine_op_terminates_for_the_degenerate_operands() {
    // The `op == 0` path, which is the one the unbounded C loop could not survive.
    assert_eq!(crc32_combine_op(0x0000_0000, 0x0000_0000, 0), 0);
    assert_eq!(crc32_combine_op(0xffff_ffff, 0xffff_ffff, 0), 0);

    // A zero first check value: `multmodp(op, 0)` accumulates nothing, so the answer is `crc2`.
    // Measured against the C implementation, which returns 0xdeadbeef for the first of these.
    let op = crc32_combine_gen64(10);
    assert_ne!(
        op, 0,
        "a non-negative length must produce a non-zero operator"
    );
    assert_eq!(
        crc32_combine_op(0, 0xdead_beef, op),
        0xdead_beef,
        "combine_op(0, crc2, op) is crc2, because multmodp(op, 0) is zero"
    );
    assert_eq!(crc32_combine_op(0, 0, op), 0);

    // Both degenerate at once, and every operator the exponent ladder can produce. Thirty-two
    // operators times two check values, each of which must return rather than spin.
    for bit in 0..u32::BITS {
        let op = crc32_combine_gen64(1_i64 << bit);
        assert_ne!(op, 0, "combine_gen64(1 << {bit}) must be non-zero");
        assert_eq!(crc32_combine_op(0, 0, op), 0);
        assert_eq!(crc32_combine_op(0, 0x1234_abcd, op), 0x1234_abcd);
    }
}

/// Combining is associative: `(a ++ b) ++ c` and `a ++ (b ++ c)` give the same check value.
///
/// Follows from the operation being polynomial multiplication and addition in `GF(2)`, but it is
/// the property callers actually rely on when they fold a list of blocks in whatever order suits
/// their data structure -- left, right, or a tree. The three lengths straddle
/// [`MIN_BRAID_LEN`] so that the parts were not all produced by the same code path.
#[test]
fn combining_is_associative() {
    let buf = fixture(FIXTURE_LEN);

    let splits = [
        (1_usize, 1_usize, 1_usize),
        (W, W, W),
        (MIN_BRAID_LEN - 1, 1, MIN_BRAID_LEN),
        (MIN_BRAID_LEN, MIN_BRAID_LEN, MIN_BRAID_LEN),
        (BRAID_BLOCK, 3, 255),
        (100, 155, 200),
    ];

    for (len_a, len_b, len_c) in splits {
        let a = &buf[..len_a];
        let b = &buf[len_a..len_a + len_b];
        let c = &buf[len_a + len_b..len_a + len_b + len_c];

        let crc_a = crc32(0, a);
        let crc_b = crc32(0, b);
        let crc_c = crc32(0, c);

        // Buffer lengths chosen by this test, all far below `i64::MAX`.
        let signed_b = i64::try_from(len_b).unwrap();
        let signed_c = i64::try_from(len_c).unwrap();

        // (a ++ b) ++ c
        let left = crc32_combine64(crc32_combine64(crc_a, crc_b, signed_b), crc_c, signed_c);
        // a ++ (b ++ c)
        let right = crc32_combine64(
            crc_a,
            crc32_combine64(crc_b, crc_c, signed_c),
            signed_b + signed_c,
        );

        // And the answer both must reach.
        let expected = crc32(0, &buf[..len_a + len_b + len_c]);

        assert_eq!(
            left, expected,
            "left-associated combine of ({len_a}, {len_b}, {len_c}) is wrong"
        );
        assert_eq!(
            right, expected,
            "right-associated combine of ({len_a}, {len_b}, {len_c}) is wrong"
        );
    }
}

/// `combine_gen64` matches the C implementation for lengths past `u32::MAX`.
///
/// Two things only become observable at these magnitudes, and neither can be reached with a buffer
/// a test could allocate.
///
/// **The argument really is 64 bits wide.** `crc32_combine_gen64` takes a `z_off64_t`
/// (`zlib.h` L1983-L1984), and truncating it to 32 bits would leave the entries from `2^32`
/// onwards agreeing with their low halves. They do not.
///
/// **The `k & 31` index wrap is live.** `x2nmodp` advances `k` once per bit of `n` and indexes
/// `x2n_table[k & 31]` (`crc32.c` L184-L195). Starting from `k == 3`, `k` reaches 32 once `n` has
/// 30 bits, so the wrap first bites at `len2 == 2^29` -- 512 `MiB`, which is an ordinary file size,
/// not a corner case. The entry at `2^29` is `0x4000_0000`, which is `X2N_TABLE[0]`: the ladder
/// has come all the way round. An implementation that masked differently, or that grew the table,
/// would differ from the reference from that point on and agree with it everywhere a small test
/// buffer could reach.
///
/// Every value below was read off the C library; see [`ORACLE_CHECKS`] for the procedure. The
/// entries below `2^29` are included so that the same table pins the ordinary range too.
#[test]
fn combine_gen64_matches_the_c_implementation_for_large_lengths() {
    const GEN_VECTORS: [(i64, u32); 23] = [
        (0, 0x8000_0000),
        (1, 0x0080_0000),
        (2, 0x0000_8000),
        (3, 0x0000_0080),
        (4, 0xedb8_8320),
        (5, 0x3b83_984b),
        (7, 0xed59_b63b),
        (8, 0xb1e6_b092),
        (16, 0xa06a_2517),
        (47, 0xce31_785d),
        (48, 0x1514_1c31),
        (1_024, 0x6427_800e),
        (65_536, 0x31fe_c169),
        // 2^29: the first length at which `k & 31` wraps, and the ladder returns to x^1.
        (536_870_912, 0x4000_0000),
        (1_073_741_824, 0x2000_0000),
        (2_147_483_648, 0x0800_0000),
        // Either side of 2^32, where a 32-bit argument would fold onto its low half.
        (4_294_967_295, 0x8000_0000),
        (4_294_967_296, 0x0080_0000),
        (4_294_967_297, 0x0000_8000),
        (8_589_934_592, 0x0000_8000),
        (1_099_511_627_776, 0xec44_7f11),
        (1_152_921_504_606_846_976, 0xc4e2_2c3c),
        (i64::MAX, 0x6d3d_2d4d),
    ];

    for (len2, expected) in GEN_VECTORS {
        assert_eq!(
            crc32_combine_gen64(len2),
            expected,
            "combine_gen64({len2}) disagrees with the C implementation"
        );
    }

    // The wrap is a property of the ladder, so state it as one rather than only as a literal.
    assert_eq!(
        crc32_combine_gen64(536_870_912),
        X2N_TABLE[0],
        "crc32.c L190: x2n_table[k & 31] brings k == 32 back to x2n_table[0]"
    );

    // A large operator must still combine correctly on data a test can hold: the operator only
    // has to be self-consistent with the arithmetic, not with the buffer's real length.
    let big = crc32_combine_gen64(4_294_967_296);
    assert_eq!(
        crc32_combine_op(0, 0x1234_abcd, big),
        0x1234_abcd,
        "a large operator must behave like any other when the first check value is zero"
    );
    assert_eq!(
        crc32_combine_op(0x1234_abcd, 0, big),
        crc32_combine_op(0x1234_abcd, 0, crc32_combine_gen64(1)),
        "combine_gen64(2^32) equals combine_gen64(1), so the two operators must act alike"
    );
}

/// The `z_off_t` delegates agree with their `z_off64_t` forms everywhere.
///
/// `crc32_combine` and `crc32_combine_gen` are one-line forwarders in C
/// (`crc32.c` L964-L966 and L981-L983), and both names exist because `zconf.h` redirects them
/// according to the caller's `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` settings
/// (`zlib.h` L2002-L2003). All four are separately exported symbols, so a caller compiled either
/// way must get the same answers, and the equivalence is asserted rather than inferred from the
/// forwarding.
#[test]
fn the_off_t_delegates_agree_with_their_sixty_four_bit_forms() {
    let lengths = [
        i64::MIN,
        -1,
        0,
        1,
        2,
        47,
        48,
        1_024,
        4_294_967_296,
        i64::MAX,
    ];

    for len2 in lengths {
        assert_eq!(
            crc32_combine_gen(len2),
            crc32_combine_gen64(len2),
            "crc32_combine_gen({len2}) must forward unchanged"
        );

        for crc1 in ORACLE_SEEDS {
            for crc2 in ORACLE_SEEDS {
                assert_eq!(
                    crc32_combine(crc1, crc2, len2),
                    crc32_combine64(crc1, crc2, len2),
                    "crc32_combine({crc1:#010x}, {crc2:#010x}, {len2}) must forward unchanged"
                );
            }
        }
    }
}

/// Combining reproduces the concatenation for every payload class of the minimal corpus.
///
/// Where the length matrix varies the split of one pseudo-random stream, this varies the content:
/// each class is halved and its two halves recombined. Content matters because `combine` folds one
/// check value into another without seeing any bytes at all, so a fault that depended on the data
/// -- rather than on the length -- would be the kind this catches.
#[test]
fn combining_reproduces_the_concatenation_for_every_corpus_class() {
    for (name, whole) in corpus::all() {
        let payload = capped(&whole);
        let (head, tail) = payload.split_at(payload.len() / 2);

        let crc_head = crc32(0, head);
        let crc_tail = crc32(0, tail);
        // Corpus payloads are at most a few tens of kilobytes.
        let signed = i64::try_from(tail.len()).unwrap();

        assert_eq!(
            crc32_combine64(crc_head, crc_tail, signed),
            crc32(0, payload),
            "combining the halves of corpus::{name} ({} + {} bytes) does not reproduce the whole",
            head.len(),
            tail.len()
        );
    }
}
