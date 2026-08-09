//! Integration tests for the Adler-32 engine of `zlib-rs`.
//!
//! This suite drives `zlib_rs::adler32` from outside the crate, the way `libz-rs-sys` and
//! every other consumer does, and it is load-bearing for two independent reasons.
//!
//! The first is the wire format. An Adler-32 value is not an internal convenience: RFC 1950
//! puts it verbatim in the four-byte trailer of every zlib stream (`doc/rfc1950.txt`
//! L318-L329), `deflate` writes the value this code computes into that trailer, and
//! `inflate` compares the value this code computes against the trailer it read. A single
//! wrong bit here is a stream the reference implementation rejects.
//!
//! The second is the `simd` feature. The port's plan admits vectorisation for the two
//! checksum modules and for nothing else, and it admits it on one ground only: a checksum
//! collapses to a single scalar however it is computed, so a vector arrangement cannot
//! perturb the emitted bytes. That is a claim, and a claim about output needs a test. The
//! equivalence section at the foot of this file is that test -- if `Adler32Simd` and
//! `Adler32Generic` ever disagree on one input, the premise is false and the feature must
//! not ship.
//!
//! # Two oracles, and why one is not enough
//!
//! Every expectation below is anchored to something outside this file. There are two such
//! anchors, and the division between them is not cosmetic.
//!
//! * `reference_adler32` is a second implementation written in this file to be *obviously*
//!   correct rather than fast: a plain accumulation loop that reduces both sums modulo
//!   `BASE` at the end of each `NMAX`-sized block. Comparing the library against it covers
//!   arbitrary lengths and arbitrary data, which no table of remembered constants can.
//! * The pinned vectors -- every `ORACLE_*` constant -- were read off the in-tree C
//!   implementation. They cover what the reference loop provably cannot: the cases where
//!   `adler32.c`'s *reduction schedule* is observable.
//!
//! That second category is the subtle one, so it is spelled out with a witness. The C
//! single-byte path reduces `sum2` with one conditional subtraction (`adler32.c` L75-L76)
//! where a modulo would reduce fully, and one subtraction is not always enough:
//!
//! ```text
//! adler32(0xffff_fff0, &[0x00])
//!   s1 = 0xfff0 = 65520, already below BASE, unchanged by the added zero byte
//!   s2 = 0xffff = 65535, then s2 += s1  ->  131055
//!   131055 >= 2 * BASE (131042), so ONE subtraction leaves 131055 - 65521 = 65534
//!   whereas a modulo would leave      131055 % 65521 = 13
//!   C returns 0xfffe_fff0.  A naive reference returns 0x000d_fff0.
//! ```
//!
//! So a starting value is either *canonical* -- both 16-bit halves already below `BASE`,
//! which is what every checksum this library produces looks like -- or it is not. Nothing in
//! the C API rejects a non-canonical one, and `adler32.c` has a definite answer for it, so
//! the answer is part of the contract. Canonical starting values are checked against
//! `reference_adler32`; non-canonical ones are checked against the pinned C vectors, because
//! there the reference loop is the thing that would be wrong. `generic.rs`'s own unit tests
//! draw the same line, under the names `CANONICAL_STARTS` and `PATHOLOGICAL_STARTS`.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no FFI.** The crate under test carries `#![forbid(unsafe_code)]`; its
//!   integration suite holds itself to the same standard and needs nothing weaker.
//! * **No third-party crates.** `crates/zlib-rs/Cargo.toml` declares an empty
//!   `[dependencies]` and no `[dev-dependencies]`, and `deny.toml`'s `[bans]` section names
//!   that table as the enforcement point. Only the built-in `#[test]` harness over `core`,
//!   `alloc` and `std` is available -- in particular there is no `rand`, so pseudo-random
//!   payloads come from `common::lcg_fill`, which makes every run reproducible byte for byte.
//! * **No `#![no_std]`.** The library is `no_std` by default, but an integration test is its
//!   own crate and links `std` unconditionally.
//! * **Buffers stay small.** The largest fixture here is `2 * NMAX + 1` bytes -- 11 105, or
//!   about 11 KiB -- which crosses every reduction boundary the block engine has. A
//!   megabyte buffer would add no coverage and would make the Miri gate unusable. The one
//!   exception is `common::corpus::window_crossing`, at 33 048 bytes, and it is exercised
//!   once.
//!
//! # A note on `is_supported`
//!
//! `crates/zlib-rs/src/adler32/simd.rs` exposes an `is_supported()` throughput hint, but
//! `mod.rs` declares `mod simd;` privately and re-exports only `Adler32Simd`, so the hint is
//! crate-private by construction and cannot be called from here. That costs this suite
//! nothing, because the property worth asserting is not what the probe *returns* -- it is
//! that the answer cannot matter. `adler32()` is the dispatcher that consults the probe, so
//! asserting that `adler32()` agrees with **both** backends on the same input proves the
//! result is identical whichever branch the probe selected, which is strictly stronger than
//! reading the flag would be. It is also exactly the requirement that a `simd`-enabled build
//! must be correct on hardware lacking a vector unit.

// Every relaxation below is a test-only relaxation of a lint the workspace denies for
// library code, and each is denied for a reason that does not apply here.
//
// `unwrap_used`, `expect_used` and `panic` are denied so that no library path can abort a
// caller's process; a test, by contrast, reports failure by panicking, and clippy.toml
// already grants these three inside a `#[test]` function via `allow-unwrap-in-tests` and its
// two siblings. The allowance is stated at crate scope because the file-scope helpers --
// `prefix` and `len2_of` -- are not themselves `#[test]` functions and so fall outside that
// context.
//
// `indexing_slicing` is denied so that no library path can panic on a bad index, and it has
// no in-tests configuration key at all. The slicing below is at constant, provably in-bounds
// offsets over fixtures this file built itself; routing each one through `get(..)` would add
// an unreachable error arm to every assertion and bury the property under inspection.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod common;

use zlib_rs::adler32::{
    adler32, adler32_combine, adler32_z, Adler32Backend, Adler32Generic, ADLER32_INITIAL_VALUE,
    BASE, NMAX,
};

#[cfg(feature = "simd")]
use zlib_rs::adler32::Adler32Simd;

// ---------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------

/// Seed for the pseudo-random fixture every length sweep runs over.
///
/// The three RFC numbers this library implements -- 1950, 1951 and 1952 -- read as hex
/// digits, which makes the constant recognisable rather than arbitrary. Its exact value is
/// frozen: every `ORACLE_*` entry below was taken over the bytes it produces, so changing it
/// invalidates all of them at once.
const CORPUS_SEED: u64 = 0x1950_1951_1952_0001;

/// Length of that fixture: `2 * NMAX + 1` bytes, 11 105 of them.
///
/// One more byte than two whole reduction blocks, which is the shortest length that forces
/// the block engine through two complete reduction cycles *and* a short tail. Every longer
/// input repeats those three situations without adding a fourth, so this is where the
/// coverage stops paying and the Miri bill starts.
const CORPUS_LEN: usize = 2 * NMAX + 1;

/// The fixture really is longer than two blocks, and by exactly one byte.
///
/// Checked at compile time so the property survives an edit of `NMAX`, and stated as a
/// `const` item rather than inside a test body because an assertion over two constants is
/// dead weight a test run would carry for nothing.
const _: () = assert!(
    CORPUS_LEN > 2 * NMAX && CORPUS_LEN == 2 * NMAX + 1,
    "the long fixture must be two whole NMAX blocks plus a one-byte tail"
);

/// Length of the buffer whose split points are swept exhaustively.
///
/// 600 bytes is 601 split points, and every one of them is tried. It comfortably straddles
/// the single-byte path, the short path and the block path, and it is small enough that the
/// quadratic cost of the sweep stays in the low hundreds of thousands of byte steps -- which
/// is what keeps it inside the Miri budget.
const SPLIT_SWEEP_LEN: usize = 600;

/// Starting values whose two halves are both already below `BASE`.
///
/// These are the values `reference_adler32` is a valid oracle for, and the only kind this
/// library ever produces. `0xfff0_fff0` is the extreme case: `0xfff0` is 65 520, which is
/// `BASE - 1`, so both halves are as large as a canonical checksum can be.
const CANONICAL_STARTS: [u32; 5] = [
    ADLER32_INITIAL_VALUE,
    0x0000_0000,
    0x000e_000e,
    0x1234_5678,
    0xfff0_fff0,
];

/// Starting values with at least one half at or above `BASE`.
///
/// Legal inputs -- nothing in the C API rejects them -- but outside the domain in which
/// `reference_adler32` agrees with `adler32.c`, for the reason set out in the module
/// documentation. They appear only where a pinned C value backs them.
const NON_CANONICAL_STARTS: [u32; 4] = [0xffff_ffff, 0xffff_fef1, 0x0000_ffff, 0xffff_fff0];

/// Lengths that bracket every path boundary in the engine.
///
/// 0, 1 and 2 pick out the empty slice and the single-byte fast path; 15, 16 and 17 straddle
/// the short-path cutoff at `adler32.c` L85; 63, 64 and 65 straddle the vectorised backend's
/// lane threshold, which is four 16-byte sub-chunks; and the last five straddle `NMAX` and
/// its double, where the reduction schedule switches blocks.
const BOUNDARY_LENS: [usize; 18] = [
    0,
    1,
    2,
    15,
    16,
    17,
    31,
    32,
    63,
    64,
    65,
    255,
    1_024,
    NMAX - 1,
    NMAX,
    NMAX + 1,
    2 * NMAX,
    CORPUS_LEN,
];

/// Split offsets used on the long fixture, where sweeping all 11 106 of them is not worth it.
///
/// 1, 15, 16 and 17 put a chunk boundary on each side of the short-path cutoff; the three
/// around `NMAX` put one on each side of a reduction, so a reduction lands mid-chunk, exactly
/// on a chunk edge, and one byte past one.
const INTERESTING_SPLITS: [usize; 7] = [1, 15, 16, 17, NMAX - 1, NMAX, NMAX + 1];

// ---------------------------------------------------------------------------------------
// Pinned C-oracle vectors
//
// Every value in this section was read off the in-tree C implementation -- the same
// `adler32.c` the Rust modules were ported from, which remains in the tree unmodified for
// exactly this purpose. Each table states how it was obtained so a future reader can
// reproduce it rather than trust it, and each hex literal is written `0xHHHH_HHHH` so the two
// 16-bit halves of a checksum can be read straight off the page: `s2` on the left, `s1` on
// the right, as RFC 1950 packs them (`doc/rfc1950.txt` L328-L329).
// ---------------------------------------------------------------------------------------

/// Known answers over literal byte strings, derived by hand and confirmed against C.
///
/// `(input, expected)` with the starting value `ADLER32_INITIAL_VALUE` throughout. The
/// derivation of each is `s1 = 1 + sum(bytes)`, `s2 = sum of the running s1 values`, and
/// every one of them is small enough that no reduction occurs, so the arithmetic below is
/// exact rather than modular:
///
/// * `b""` -- no bytes, so `s1 = 1` and `s2 = 0`, giving `0x0000_0001`. This is
///   `ADLER32_INITIAL_VALUE` itself, so the seed is a fixed point of the empty update.
/// * `b"a"` -- `s1 = 1 + 97 = 98 = 0x62`, `s2 = 0 + 98 = 98 = 0x62`.
/// * `b"ab"` -- `s1 = 98 + 98 = 196 = 0xc4`, `s2 = 98 + 196 = 294 = 0x126`.
/// * `b"abc"` -- `s1 = 196 + 99 = 295 = 0x127`, `s2 = 294 + 295 = 589 = 0x24d`.
/// * `b"hello, hello!"` -- 13 bytes, `s1 = 1 + 1173 = 1174 = 0x496`, and accumulating the
///   thirteen running `s1` values 105, 206, 314, 422, 533, 577, 609, 713, 814, 922, 1030,
///   1141, 1174 gives `s2 = 8560 = 0x2170`. This is the payload of `test/example.c` L35, and
///   the value the doc example in `src/adler32/mod.rs` asserts.
/// * `b"hello"` -- the first five of those bytes: `s1 = 533 = 0x215` and
///   `s2 = 105 + 206 + 314 + 422 + 533 = 1580 = 0x62c`.
const ORACLE_KNOWN_ANSWERS: [(&[u8], u32); 6] = [
    (b"", 0x0000_0001),
    (b"a", 0x0062_0062),
    (b"ab", 0x0126_00c4),
    (b"abc", 0x024d_0127),
    (b"hello, hello!", 0x2170_0496),
    (b"hello", 0x062c_0215),
];

/// Known answers over the two fixtures shared with the C test suite.
///
/// Kept separate from `ORACLE_KNOWN_ANSWERS` because these are named constants from
/// `common::corpus` rather than literals, and because the length of each is itself the
/// assertion: both include a terminating NUL that a `strlen`-shaped reading would drop.
///
/// * `HELLO` is `b"hello, hello!\0"`, **14** bytes. `test/example.c` feeds
///   `strlen(hello) + 1` at L69, L95, L175, L341 and L434. The NUL adds nothing to `s1`, so
///   `s1` stays `1174 = 0x496`, and `s2` gains one more copy of it: `8560 + 1174 = 9734`,
///   which is `0x2606`.
/// * `DICTIONARY` is `b"hello\0"`, **6** bytes, passed as `sizeof(dictionary)` at
///   `test/example.c` L426 and L477. Again `s1` is unchanged at `533 = 0x215` and
///   `s2 = 1580 + 533 = 2113 = 0x841`.
const ORACLE_SUITE_FIXTURES: [(&[u8], usize, u32); 2] = [
    (common::corpus::HELLO, 14, 0x2606_0496),
    (common::corpus::DICTIONARY, 6, 0x0841_0215),
];

/// Checksum of `adler32(ADLER32_INITIAL_VALUE, corpus()[..len])` for each pinned `len`.
///
/// The lengths are the ones that matter structurally: the empty slice, the single-byte fast
/// path, the short path and its cutoff at 16, the vectorised lane threshold at 64, and every
/// `NMAX` reduction edge from both sides. Reproduce any row by filling a buffer with
/// `common::lcg_fill(CORPUS_SEED, ..)` and calling the C `adler32` on its first `len` bytes.
const ORACLE_PREFIXES: [(usize, u32); 20] = [
    (0, 0x0000_0001),
    (1, 0x0008_0008),
    (2, 0x00e8_00e0),
    (3, 0x01cc_00e4),
    (15, 0x376c_07aa),
    (16, 0x3f2f_07c3),
    (17, 0x46fe_07cf),
    (31, 0xdf5a_0e31),
    (32, 0xeda0_0e46),
    (63, 0xc98d_1e8b),
    (64, 0xe8d3_1f46),
    (65, 0x088c_1faa),
    (255, 0xedc1_84b8),
    (SPLIT_SWEEP_LEN, 0x4780_32a8),
    (1_024, 0x30be_170b),
    (NMAX - 1, 0x8035_ca31),
    (NMAX, 0x4b4f_cb0b),
    (NMAX + 1, 0x16f0_cb92),
    (2 * NMAX, 0xd8a5_98bf),
    (CORPUS_LEN, 0x7262_99ae),
];

/// `(start, len, expected)` over the same fixture, for seven starting values.
///
/// Three of the seven are non-canonical, and they are the point of the table: they are where
/// an implementation that merged `adler32.c`'s three differently-reducing paths into one
/// diverges, and where `reference_adler32` is not a valid oracle. The canonical four are
/// included alongside so that a failure can be localised -- if the canonical rows pass and
/// the non-canonical ones fail, the defect is in the reduction schedule and nowhere else.
const ORACLE_STARTS: [(u32, usize, u32); 42] = [
    (0x0000_0000, 0, 0x0000_0000),
    (0x0000_0000, 1, 0x0007_0007),
    (0x0000_0000, 16, 0x3f1f_07c2),
    (0x0000_0000, 64, 0xe893_1f45),
    (0x0000_0000, 1_024, 0x2cbe_170a),
    (0x0000_0000, CORPUS_LEN, 0x4701_99ad),
    (0x0000_ffff, 0, 0x0000_000e),
    (0x0000_ffff, 1, 0x0015_0015),
    (0x0000_ffff, 16, 0x3fff_07d0),
    (0x0000_ffff, 64, 0xec13_1f53),
    (0x0000_ffff, 1_024, 0x64be_1718),
    (0x0000_ffff, CORPUS_LEN, 0xa66d_99bb),
    (0x1234_5678, 0, 0x1234_5678),
    (0x1234_5678, 1, 0x68b3_567f),
    (0x1234_5678, 16, 0xb91e_5e3a),
    (0x1234_5678, 64, 0x9a11_75bd),
    (0x1234_5678, 1_024, 0x3338_6d82),
    (0x1234_5678, CORPUS_LEN, 0x2085_f025),
    (0xfff0_fff0, 0, 0xfff0_fff0),
    (0xfff0_fff0, 1, 0x0005_0006),
    (0xfff0_fff0, 16, 0x3f0e_07c1),
    (0xfff0_fff0, 64, 0xe852_1f44),
    (0xfff0_fff0, 1_024, 0x28bd_1709),
    (0xfff0_fff0, CORPUS_LEN, 0x1b9f_99ac),
    (0xffff_fef1, 0, 0x000e_fef1),
    (0xffff_fef1, 1, 0xff06_fef8),
    (0xffff_fef1, 16, 0x2f2d_06c2),
    (0xffff_fef1, 64, 0xa8a1_1e45),
    (0xffff_fef1, 1_024, 0x2c90_160a),
    (0xffff_fef1, CORPUS_LEN, 0xe37b_98ad),
    (0xffff_fff0, 0, 0x000e_fff0),
    (0xffff_fff0, 1, 0x0014_0006),
    (0xffff_fff0, 16, 0x3f1d_07c1),
    (0xffff_fff0, 64, 0xe861_1f44),
    (0xffff_fff0, 1_024, 0x28cc_1709),
    (0xffff_fff0, CORPUS_LEN, 0x1bae_99ac),
    (0xffff_ffff, 0, 0x000e_000e),
    (0xffff_ffff, 1, 0x0023_0015),
    (0xffff_ffff, 16, 0x400d_07d0),
    (0xffff_ffff, 64, 0xec21_1f53),
    (0xffff_ffff, 1_024, 0x64cc_1718),
    (0xffff_ffff, CORPUS_LEN, 0xa67b_99bb),
];

/// `adler32(0xffff_fff0, &[0u8; len])` for every `len` in `0..=17`.
///
/// The single sharpest instrument in this file. Zero bytes leave `s1` fixed at
/// `0xfff0 = 65_520`, so the whole table isolates what happens to `s2` alone, and the
/// starting `s2` of `0xffff = 65_535` is one above `BASE` -- precisely the condition under
/// which the three code paths part company. Read the first three rows:
///
/// | `len` | result | why |
/// |---|---|---|
/// | 0 | `0x000e_fff0` | short path: `s2 %= BASE` gives `65_535 % 65_521 = 14` |
/// | 1 | `0xfffe_fff0` | single-byte path: `65_535 + 65_520 = 131_055`, and **one** conditional subtraction gives `65_534`, not the `13` a modulo would give |
/// | 2 | `0x000c_fff0` | short path again: `131_055 + 65_520 = 196_575`, and `196_575 % 65_521 = 12` |
///
/// The jump from `0x000e` to `0xfffe` and back to `0x000c` is `adler32.c`'s asymmetry made
/// visible. An implementation that reduced every path the same way -- however much tidier
/// that would look -- fails row 1 and passes the rest. Row 15 to row 16 is the second
/// discontinuity, where the short path hands over to the block engine at
/// `adler32.c` L85.
const ORACLE_ZERO_RUNS: [(usize, u32); 18] = [
    (0, 0x000e_fff0),
    (1, 0xfffe_fff0),
    (2, 0x000c_fff0),
    (3, 0x000b_fff0),
    (4, 0x000a_fff0),
    (5, 0x0009_fff0),
    (6, 0x0008_fff0),
    (7, 0x0007_fff0),
    (8, 0x0006_fff0),
    (9, 0x0005_fff0),
    (10, 0x0004_fff0),
    (11, 0x0003_fff0),
    (12, 0x0002_fff0),
    (13, 0x0001_fff0),
    (14, 0x0000_fff0),
    (15, 0xfff0_fff0),
    (16, 0xffef_fff0),
    (17, 0xffee_fff0),
];

/// Starting value used by `ORACLE_ZERO_RUNS`, named so the two cannot drift apart.
const ZERO_RUN_START: u32 = 0xffff_fff0;

/// `(name, len, checksum)` for all eight payload classes of `common::corpus::all`.
///
/// Broader byte distributions than the length sweeps produce -- a 4 `KiB` run of one byte, 4
/// `KiB` of pseudo-random noise, natural-language prose, an exhaustive cycle of all 256 byte
/// values, and a payload that crosses the 32 `KiB` window -- each pinned to C. The length is
/// carried alongside the checksum so that a future edit to a corpus class fails here, loudly,
/// instead of silently invalidating a checksum.
const ORACLE_CLASSES: [(&str, usize, u32); 8] = [
    ("empty", 0, 0x0000_0001),
    ("single_byte", 1, 0x0062_0062),
    ("hello", 14, 0x2606_0496),
    ("repetitive", 4_096, 0xefcb_105b),
    ("incompressible", 4_096, 0x2fdb_f2fb),
    ("text", 716, 0x3d6e_02be),
    ("binary", 1_024, 0xe4c9_fe10),
    ("window_crossing", 33_048, 0xf4ae_3e1d),
];

/// Checksum of `b"hello, "`, the first seven bytes of `common::corpus::HELLO`.
///
/// `s1 = 1 + 104 + 101 + 108 + 108 + 111 + 44 + 32 = 609 = 0x261`, and the seven running
/// values 105, 206, 314, 422, 533, 577, 609 sum to `s2 = 2766 = 0xace`.
const HEAD_CHECKSUM: u32 = 0x0ace_0261;

/// Checksum of `b"hello!\0"`, the last seven bytes of `common::corpus::HELLO`.
///
/// `s1 = 1 + 104 + 101 + 108 + 108 + 111 + 33 + 0 = 566 = 0x236`, and the seven running
/// values 105, 206, 314, 422, 533, 566, 566 sum to `s2 = 2712 = 0xa98`.
const TAIL_CHECKSUM: u32 = 0x0a98_0236;

/// `(adler1, adler2, len2, expected)` for `adler32_combine`, pinned to C.
///
/// The four rows that carry the most weight, in order:
///
/// * Row 1 is the concatenation itself: joining the checksums of `b"hello, "` and
///   `b"hello!\0"` with `len2 = 7` reproduces `0x2606_0496`, the checksum of the whole of
///   `common::corpus::HELLO`.
/// * Row 5 uses `len2 = 7 + BASE` and row 6 uses `len2 = 7 + 1000 * BASE`; both must give the
///   same answer as row 1, because `adler32.c` L143 reduces `len2` modulo `BASE` before
///   anything else uses it.
/// * Row 7 uses `len2 = 2^32 + 7` and must **not** give row 1's answer. `(2^32 + 7) % BASE`
///   is 232, not 7, so an implementation that narrowed `len2` to 32 bits before reducing --
///   `len2 as u32` would give exactly 7 -- returns `0x2606_0496` here and fails. This single
///   row is the reason the parameter is `i64` and not `u32`.
/// * The last row is the internal-overflow bound. `rem * sum1` is the one product in
///   `adler32_combine_` that could exceed `u32`, and it is largest when both factors are as
///   large as the contract allows: `rem = BASE - 1 = 65_520` and a low half of `65_520`,
///   giving `4_292_870_400`, which is `2_096_895` below `u32::MAX`.
const ORACLE_COMBINE: [(u32, u32, i64, u32); 10] = [
    (HEAD_CHECKSUM, TAIL_CHECKSUM, 7, 0x2606_0496),
    (HEAD_CHECKSUM, ADLER32_INITIAL_VALUE, 0, HEAD_CHECKSUM),
    (ADLER32_INITIAL_VALUE, TAIL_CHECKSUM, 7, TAIL_CHECKSUM),
    (HEAD_CHECKSUM, TAIL_CHECKSUM, 0, 0x1566_0496),
    (HEAD_CHECKSUM, TAIL_CHECKSUM, 7 + 65_521, 0x2606_0496),
    (
        HEAD_CHECKSUM,
        TAIL_CHECKSUM,
        7 + 1_000 * 65_521,
        0x2606_0496,
    ),
    (HEAD_CHECKSUM, TAIL_CHECKSUM, (1 << 32) + 7, 0x3c84_0496),
    (HEAD_CHECKSUM, TAIL_CHECKSUM, 1 << 62, 0x86d9_0496),
    (HEAD_CHECKSUM, TAIL_CHECKSUM, i64::MAX, 0xf5ec_0496),
    (0x0000_fff0, ADLER32_INITIAL_VALUE, 65_520, 0x0002_fff0),
];

/// Negative lengths that must all yield the documented sentinel.
///
/// `adler32.c` L138-L140 returns `0xffffffffUL` "for negative len", calling it an "invalid
/// adler32 as a clue for debugging". That makes it documented API behaviour rather than an
/// error path to normalise away, and `i64::MIN` is included because it is the one value with
/// no representable negation -- an implementation that reached for `abs()` or `-len2` before
/// the sign test would overflow on it.
const NEGATIVE_LENGTHS: [i64; 6] = [-1, -2, -65_521, -(1 << 32), -(1 << 62), i64::MIN];

/// The sentinel a negative `len2` produces: an intentionally invalid Adler-32 value.
///
/// Invalid because both halves are `0xffff = 65_535`, which is above `BASE`, so no genuine
/// checksum can ever equal it.
const NEGATIVE_LEN_SENTINEL: u32 = 0xffff_ffff;

// The three properties below are relations between constants, so they are settled at compile
// time rather than at test time. Written as `const _: () = assert!(..)` items, which is the idiom
// the crate's own modules use and the one clippy asks for when it sees an assertion whose value
// is already known -- and which turns a future edit of `BASE`, `NMAX` or the sentinel into a
// build failure instead of a test failure. The tests below check the run-time consequences of
// each, which is the part a compile-time assertion cannot reach.

/// Each component sum must fit in sixteen bits for the packed representation to be lossless.
///
/// RFC 1950 stores the checksum as `s2 * 65536 + s1` (`doc/rfc1950.txt` L328-L329), which is
/// reversible by a mask and a shift only while each residue is below 65 536. `BASE` being the
/// largest prime *below* 65 536 is what guarantees it. Primality itself is deliberately not
/// asserted anywhere: a primality test would restate the value without proving anything about
/// this code.
const _: () = assert!(
    BASE < 0x1_0000,
    "both Adler-32 component sums must fit in 16 bits to pack into one u32"
);

/// A full reduction block must be a whole number of 16-byte unrolled steps.
///
/// The C block loop computes its iteration count once as `n = NMAX / 16` and then runs
/// `do { DO16(buf); } while (--n)` with no provision for a leftover tail; the comment at
/// `adler32.c` L99 states the assumption outright. An edit of `NMAX` that broke it would silently
/// drop up to fifteen bytes per block.
const _: () = assert!(
    NMAX % 16 == 0,
    "adler32.c L99 requires a full NMAX block to be a whole number of 16-byte steps"
);

/// The negative-length sentinel must be unreachable as a genuine checksum.
///
/// Both of its halves are `0xffff = 65_535`, which is above `BASE`, so no sequence of bytes can
/// produce it from a canonical starting value. That is what makes it usable as the "clue for
/// debugging" `adler32.c` L138 calls it: a caller who sees it knows it did not come from data.
const _: () = assert!(
    NEGATIVE_LEN_SENTINEL & 0xffff >= BASE && NEGATIVE_LEN_SENTINEL >> 16 >= BASE,
    "the sentinel must not be a value any genuine checksum can take"
);

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// Builds the pseudo-random fixture the length sweeps run over.
///
/// `CORPUS_LEN` bytes from `common::lcg_fill` at `CORPUS_SEED`. There is no `rand` crate to
/// reach for and there must not be one, but determinism is the greater virtue here anyway:
/// the `ORACLE_*` tables above pin values over *these* bytes, so a corpus that differed
/// between runs could not be compared against C at all.
fn corpus() -> Vec<u8> {
    corpus_of(CORPUS_LEN)
}

/// Builds `len` bytes of the same fixture.
///
/// `common::lcg_fill` advances one state per byte from the seed, so a longer buffer is a strict
/// extension of a shorter one: `corpus_of(n)` and `corpus_of(m)` agree on their first
/// `min(n, m)` bytes. That is what lets the two tests needing more than `CORPUS_LEN` bytes --
/// the concatenation matrix, whose two lengths can sum past it, and the three-way associativity
/// fold -- ask for what they need without invalidating a single pinned value above.
fn corpus_of(len: usize) -> Vec<u8> {
    let mut out = vec![0_u8; len];
    common::lcg_fill(CORPUS_SEED, &mut out);
    out
}

/// An independent Adler-32, written to be obviously right rather than fast.
///
/// This is the second of the two oracles described in the module documentation, and its whole
/// value is that it shares no code with the subject. It is RFC 1950's definition transcribed
/// directly -- `s1` accumulates the bytes, `s2` accumulates `s1` (`doc/rfc1950.txt`
/// L326-L328) -- with one concession to arithmetic: the sums are reduced modulo `BASE` at the
/// end of every `NMAX`-byte block, which is exactly the cadence `NMAX` is defined to permit
/// (`adler32.c` L11-L12) and therefore the cadence at which 32-bit accumulation is provably
/// safe. Six lines, no branches, no unrolling, nothing to get wrong.
///
/// # Valid for canonical starting values only
///
/// The seed's halves are reduced on entry, and reducing with `%` is what makes this loop
/// simple. It is also what makes it disagree with `adler32.c` when a half arrives at or above
/// `BASE`, because the C single-byte path reduces `s2` by a single conditional subtraction
/// that can leave a value still above `BASE`. The module documentation gives the witness. So
/// this function is the oracle for `CANONICAL_STARTS` and never for `NON_CANONICAL_STARTS`;
/// those are covered by the pinned tables, which is the stronger anchor in any case.
fn reference_adler32(adler: u32, buf: &[u8]) -> u32 {
    // Reducing the seed up front makes the NMAX bound hold for the first block exactly as it
    // does for every later one, whatever the caller passes in.
    let mut s1 = (adler & 0xffff) % BASE;
    let mut s2 = ((adler >> 16) & 0xffff) % BASE;

    for block in buf.chunks(NMAX) {
        for &byte in block {
            s1 += u32::from(byte);
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
    }

    s1 | (s2 << 16)
}

/// The first `len` bytes of `buf`, with a diagnostic when the fixture is too short.
///
/// Every call site passes a length bounded by `CORPUS_LEN`, so the failure arm is
/// unreachable; it exists so that a future edit which shrinks a fixture fails with the two
/// numbers involved rather than with a bare slice panic.
fn prefix(buf: &[u8], len: usize) -> &[u8] {
    buf.get(..len).unwrap_or_else(|| {
        let have = buf.len();
        panic!("fixture holds {have} bytes but {len} were requested")
    })
}

/// Widens a fixture length to the signed 64-bit length `adler32_combine` takes.
///
/// The C parameter is `z_off64_t`, a signed 64-bit file offset (`zconf.h`), and the sign is
/// load-bearing: a negative value is the documented sentinel trigger. Every length here is at
/// most a few tens of kilobytes, so the conversion is exact.
fn len2_of(len: usize) -> i64 {
    i64::try_from(len).expect("every fixture length in this suite fits in an i64")
}

/// Computes a checksum through the `Adler32Backend` trait rather than through a free function.
///
/// Exists so the trait boundary is exercised as a boundary. `checksum` is an associated
/// function with no `self`, so a backend is named rather than constructed; a generic caller is
/// therefore the only way to prove that the two backends are substitutable for one another at
/// the type level and not merely observed to agree.
fn via<B: Adler32Backend>(adler: u32, buf: &[u8]) -> u32 {
    B::checksum(adler, buf)
}

/// Constructs a `T` through its `Default` implementation.
///
/// Written generically on purpose: it exercises the derive rather than the unit-struct literal,
/// which is what a direct `Adler32Generic::default()` collapses into -- and which clippy rightly
/// asks to be written as the literal instead. The same helper, for the same reason, appears in
/// the crate's own unit tests.
fn default_of<T: Default>() -> T {
    T::default()
}

/// Duplicates a `T` through its `Clone` implementation.
///
/// Generic for the same reason as `default_of`: on a `Copy` type a direct `clone()` call is just
/// a copy, so only a `Clone`-bounded caller actually reaches the implementation.
fn clone_of<T: Clone>(value: &T) -> T {
    value.clone()
}

// ---------------------------------------------------------------------------------------
// Miri budget
//
// Miri interprets rather than executes, at roughly three to four orders of magnitude the
// cost. There is nothing here for it to find that a native run would not -- no `unsafe`, no
// raw pointers, no uninitialised memory -- so its useful yield is confined to arithmetic
// overflow and out-of-bounds indexing, both of which the cheap cases reach. The two constants
// below narrow the widest sweeps under `cfg(miri)` so that a checksum-agreement check does not
// dominate a Miri run shared with every other suite in the crate. Nothing is narrowed under a
// normal `cargo test`, and no
// structural coverage is lost either way: every path boundary is still crossed from both sides
// under Miri as well, because each narrowed list keeps the entries that straddle a boundary and
// drops only the ones that re-cross a boundary already covered.
//
// A handful of tests carry `#[cfg_attr(miri, ignore)]` instead of a narrowed list. Every one of
// them is a bulk comparison of values against the C implementation over the longest fixtures, and
// each says so at its own definition: verifying a number is not what Miri is for, the code paths
// those tests reach are already interpreted by the sweeps that do run, and all of them run in full
// natively.
// ---------------------------------------------------------------------------------------

/// Starting values the reference cross-checks sweep over.
///
/// Under Miri, the initial value plus the extreme canonical one -- the two that between them
/// exercise a reduced and a maximally-large pair of halves.
#[cfg(miri)]
const SWEEP_STARTS: &[u32] = &[ADLER32_INITIAL_VALUE, 0xfff0_fff0];
/// Starting values the reference cross-checks sweep over: all five canonical ones.
#[cfg(not(miri))]
const SWEEP_STARTS: &[u32] = &CANONICAL_STARTS;

/// Every starting value, canonical and not, for the sweeps that accept both.
///
/// Under Miri, four of the nine: the initial value, the extreme canonical one, and the two
/// non-canonical ones whose halves differ, so a reduced and an unreduced `s1` and `s2` are all
/// represented.
#[cfg(miri)]
const ALL_STARTS: &[u32] = &[ADLER32_INITIAL_VALUE, 0xfff0_fff0, 0xffff_fff0, 0xffff_ffff];
/// Every starting value, canonical and not: the union of the two classes, in order.
///
/// `the_full_start_list_is_the_union_of_the_two_classes` checks that it really is the union, so
/// adding a value to either class without adding it here fails rather than passing quietly.
#[cfg(not(miri))]
const ALL_STARTS: &[u32] = &[
    ADLER32_INITIAL_VALUE,
    0x0000_0000,
    0x000e_000e,
    0x1234_5678,
    0xfff0_fff0,
    0xffff_ffff,
    0xffff_fef1,
    0x0000_ffff,
    0xffff_fff0,
];

/// Lengths the wide sweeps run over.
///
/// Under Miri, `BOUNDARY_LENS` without its four longest entries -- 1 024, `NMAX`, `2 * NMAX` and
/// `CORPUS_LEN` -- which cuts the swept byte count from 40 450 to 11 562. No boundary is lost:
/// `NMAX - 1` and `NMAX + 1` still straddle the first reduction from both sides, and 63/64/65
/// still straddle the vectorised lane threshold. What is lost is the repetition of a boundary
/// already crossed, which is the only thing the long entries add.
#[cfg(miri)]
const SWEEP_LENS: &[usize] = &[
    0,
    1,
    2,
    15,
    16,
    17,
    31,
    32,
    63,
    64,
    65,
    255,
    NMAX - 1,
    NMAX + 1,
];
/// Lengths the wide sweeps run over: every boundary length.
#[cfg(not(miri))]
const SWEEP_LENS: &[usize] = &BOUNDARY_LENS;

/// Split offsets and chunk sizes applied to the long fixture.
///
/// Under Miri, one of the seven: `NMAX`, the reduction edge, which is the entry no shorter fixture
/// can cover. Each pass over the long fixture costs 11 105 interpreted byte steps, so this is the
/// most expensive knob in the file per entry, and the small offsets it drops are covered under Miri
/// by the exhaustive 600-byte sweep and by the byte-at-a-time test.
#[cfg(miri)]
const LONG_SPLITS: &[usize] = &[NMAX];
/// Split offsets and chunk sizes applied to the long fixture: all seven.
#[cfg(not(miri))]
const LONG_SPLITS: &[usize] = &INTERESTING_SPLITS;

/// Lengths forming the concatenation matrix, whose cost is quadratic in this list.
///
/// Under Miri, the seven small entries only. Dropping the four `NMAX`-scale ones takes the matrix
/// from 144 pairs averaging some 3 000 bytes each to 49 pairs averaging about 100, and the
/// concatenation property is still checked across a reduction edge by
/// `combining_is_associative_over_a_three_way_split` and by the pinned combine vectors.
#[cfg(miri)]
const COMBINE_MATRIX_LENS: &[usize] = &[0, 1, 2, 15, 16, 17, 255];
/// Lengths forming the concatenation matrix: small ones plus every `NMAX` edge.
#[cfg(not(miri))]
const COMBINE_MATRIX_LENS: &[usize] = &[
    0,
    1,
    2,
    15,
    16,
    17,
    64,
    255,
    1_024,
    NMAX - 1,
    NMAX,
    NMAX + 1,
];

/// Lengths the sweeps that make several calls per case run over.
///
/// Separate from `SWEEP_LENS` because these sweeps cost two, three or four checksums per case
/// rather than one, and 96 percent of `SWEEP_LENS`'s swept bytes sit in its two `NMAX`-scale
/// entries. Under Miri those two are dropped here, which is a sevenfold cut, and nothing is lost:
/// the `NMAX` crossing is still interpreted under Miri by
/// `every_reduction_boundary_agrees_with_the_independent_reference`, by
/// `the_two_backends_agree_at_every_reduction_boundary` -- which deliberately keeps the longer
/// list, because crossing a reduction is exactly what the lane engine has to get right -- and by
/// the two tests that pass over the whole 11 105-byte fixture.
#[cfg(miri)]
const BACKEND_SWEEP_LENS: &[usize] = &[0, 1, 2, 15, 16, 17, 31, 32, 63, 64, 65, 255, 1_024];
/// Lengths the sweeps that make several calls per case run over: every boundary length.
#[cfg(not(miri))]
const BACKEND_SWEEP_LENS: &[usize] = &BOUNDARY_LENS;

/// The three piece lengths of the associativity fold, in order.
///
/// Natively three pieces of roughly `NMAX` bytes each, so every one of them crosses a reduction.
/// Under Miri only the middle piece does, which is enough for a property that holds at any length.
#[cfg(miri)]
const ASSOCIATIVITY_PARTS: [usize; 3] = [17, NMAX + 13, 31];
/// The three piece lengths of the associativity fold, in order.
#[cfg(not(miri))]
const ASSOCIATIVITY_PARTS: [usize; 3] = [NMAX - 7, NMAX + 13, NMAX + 1];

/// First-piece lengths of the three-way chunking sweep, whose cost is the product of the two.
#[cfg(miri)]
const THREE_WAY_HEADS: &[usize] = &[0, 16, 199];
/// First-piece lengths of the three-way chunking sweep.
#[cfg(not(miri))]
const THREE_WAY_HEADS: &[usize] = &[0, 1, 15, 16, 17, 199];

/// Second-piece lengths of the three-way chunking sweep.
#[cfg(miri)]
const THREE_WAY_MIDDLES: &[usize] = &[0, 17, 201];
/// Second-piece lengths of the three-way chunking sweep.
#[cfg(not(miri))]
const THREE_WAY_MIDDLES: &[usize] = &[0, 1, 15, 16, 17, 201];

/// Stride between the split points of the exhaustive concatenation sweep.
///
/// One natively, so every one of the 601 offsets is tried. A stride of 19 under Miri keeps 32 of
/// them, and 19 is odd -- hence coprime with the 16-byte unrolled step -- so the sample still
/// lands on every residue class of that step rather than repeating one.
#[cfg(miri)]
const SPLIT_STRIDE: usize = 19;
/// Stride between the split points of the exhaustive concatenation sweep.
#[cfg(not(miri))]
const SPLIT_STRIDE: usize = 1;

// ---------------------------------------------------------------------------------------
// 3.1 -- the constants, and the seed
// ---------------------------------------------------------------------------------------

/// The two defining constants must equal the values `adler32.c` fixes for them.
///
/// `BASE` is `adler32.c` L10 and `NMAX` is L11. They are asserted separately so a failure
/// names which one moved, and they are asserted from outside the crate because both are part
/// of the public surface: `libz-rs-sys` and the differential harness read them.
#[test]
fn the_defining_constants_match_the_c_source() {
    // `adler32.c` L10: `#define BASE 65521U`, "largest prime smaller than 65536".
    assert_eq!(BASE, 65_521, "BASE must equal adler32.c L10");

    // `adler32.c` L11: `#define NMAX 5552`, the largest `n` for which
    // `255 * n * (n + 1) / 2 + (n + 1) * (BASE - 1)` still fits in a `u32`.
    assert_eq!(NMAX, 5_552, "NMAX must equal adler32.c L11");
}

/// Every checksum produced from a canonical start must itself be canonical, and must unpack.
///
/// The inequality `BASE < 0x1_0000` is settled at compile time, at the `const _` item above; what
/// a test can add is its consequence, which is the part that would actually break a stream. Both
/// halves of every value the library returns have to stay below `BASE`, so that `s2 * 65536 + s1`
/// is exact and a decompressor comparing a trailer byte for byte agrees.
///
/// The claim is scoped to canonical starting values on purpose. From a non-canonical one the C
/// implementation can legitimately return a half at or above `BASE` -- `adler32(0xffff_fff0,
/// &[0x00])` is `0xfffe_fff0` -- and reproducing that is required rather than optional.
#[test]
fn a_checksum_of_canonical_input_packs_losslessly_into_one_word() {
    let buf = corpus();

    for &start in SWEEP_STARTS {
        for &len in SWEEP_LENS {
            let value = adler32(start, prefix(&buf, len));
            let s1 = value & 0xffff;
            let s2 = value >> 16;

            assert!(
                s1 < BASE,
                "s1 {s1} is not reduced at start {start:#010x}, len {len}"
            );
            assert!(
                s2 < BASE,
                "s2 {s2} is not reduced at start {start:#010x}, len {len}"
            );
            // The packing is `s2 * 65536 + s1`, and it must round-trip through mask and shift.
            assert_eq!(
                s1 | (s2 << 16),
                value,
                "the two halves must reassemble into the checksum"
            );
            assert_eq!(u32::from(u16::try_from(s2).unwrap()) * 0x1_0000 + s1, value);
        }
    }
}

/// The named initial value must be `1`, and must decode to RFC 1950's `s1 = 1`, `s2 = 0`.
///
/// This constant exists because a Rust `&[u8]` cannot be null, so the C idiom
/// `adler32(0L, Z_NULL, 0)` (`zlib.h` L1821) -- which the reference implementation itself uses
/// in five places purely to obtain the value 1 -- has no direct translation. Asserting the two
/// halves as well as the value ties the constant to the specification rather than to a magic
/// number.
#[test]
fn the_initial_value_is_one_and_decodes_to_the_rfc_halves() {
    assert_eq!(ADLER32_INITIAL_VALUE, 1, "the initial checksum value is 1");
    assert_eq!(
        ADLER32_INITIAL_VALUE & 0xffff,
        1,
        "RFC 1950 initializes s1 to 1"
    );
    assert_eq!(
        ADLER32_INITIAL_VALUE >> 16,
        0,
        "RFC 1950 initializes s2 to 0"
    );
}

/// The seed must be a fixed point of the empty update.
///
/// `adler32(ADLER32_INITIAL_VALUE, &[])` has to return the seed unchanged. This is the
/// property that makes the constant usable the way the C idiom is used -- obtain it once, then
/// accumulate -- and it is asserted through both entry points because `libz-rs-sys` exports
/// both.
#[test]
fn the_initial_value_is_a_fixed_point_of_the_empty_update() {
    assert_eq!(adler32(ADLER32_INITIAL_VALUE, &[]), ADLER32_INITIAL_VALUE);
    assert_eq!(adler32_z(ADLER32_INITIAL_VALUE, &[]), ADLER32_INITIAL_VALUE);
}

/// An empty update must leave any canonical running checksum alone.
///
/// Zero-length updates happen constantly in real use -- `deflate` and `inflate` call the
/// checksum for whatever a caller handed them, and a caller may hand them nothing -- so this
/// has to be free of side effects, not merely harmless.
///
/// Note what is *not* claimed: that an empty update is the identity for every `u32`. It is the
/// identity exactly for canonical values, and the next test pins what happens to the others.
#[test]
fn an_empty_update_is_the_identity_for_canonical_running_values() {
    for &start in &CANONICAL_STARTS {
        assert_eq!(
            adler32(start, &[]),
            start,
            "empty update must not disturb the canonical value {start:#010x}"
        );
    }
}

/// An empty update *normalises* a non-canonical running checksum rather than preserving it.
///
/// The values are `src/adler32/mod.rs`'s documented table, and they come from C. The C short
/// path runs even for a zero-length buffer -- `while (len--)` simply does not iterate -- and
/// then reduces `s1` by conditional subtraction and `s2` by modulo (`adler32.c` L90-L92). So a
/// half at or above `BASE` comes back reduced.
///
/// This is the one place where "an empty slice is not `Z_NULL`" becomes concrete arithmetic:
/// `Z_NULL` would return 1 regardless of the incoming value, whereas an empty slice returns a
/// function of it.
#[test]
fn an_empty_update_normalises_a_non_canonical_running_value() {
    // 0xffff = 65_535 in both halves: s1 -= BASE gives 14, and s2 % BASE gives 14.
    assert_eq!(adler32(0xffff_ffff, &[]), 0x000e_000e);
    // Low half 0xfef1 = 65_265 is already below BASE and survives; the high half reduces.
    assert_eq!(adler32(0xffff_fef1, &[]), 0x000e_fef1);
    // High half zero, low half 65_535: only the low half reduces.
    assert_eq!(adler32(0x0000_ffff, &[]), 0x0000_000e);
    // 0xfff0 = 65_520 = BASE - 1 in both halves: canonical, so nothing changes.
    assert_eq!(adler32(0xfff0_fff0, &[]), 0xfff0_fff0);
}

/// The two classes of starting value must be classified correctly.
///
/// The whole oracle strategy of this file rests on the distinction -- `reference_adler32` is valid
/// for one class and not the other -- so the distinction itself is asserted rather than assumed. A
/// value is canonical exactly when both of its 16-bit halves are below `BASE`.
#[test]
fn the_two_classes_of_starting_value_are_classified_correctly() {
    for &start in &CANONICAL_STARTS {
        assert!(
            start & 0xffff < BASE && start >> 16 < BASE,
            "{start:#010x} is listed as canonical but has a half at or above BASE"
        );
    }

    for &start in &NON_CANONICAL_STARTS {
        assert!(
            start & 0xffff >= BASE || start >> 16 >= BASE,
            "{start:#010x} is listed as non-canonical but both halves are below BASE"
        );
    }
}

/// `ALL_STARTS` must be exactly the union of the two classes.
///
/// Checked natively only, because `ALL_STARTS` is deliberately a subset under Miri. Without this,
/// adding a value to either class and forgetting to add it to `ALL_STARTS` would silently shrink
/// the coverage of every sweep that uses it.
#[cfg(not(miri))]
#[test]
fn the_full_start_list_is_the_union_of_the_two_classes() {
    assert_eq!(
        ALL_STARTS.len(),
        CANONICAL_STARTS.len() + NON_CANONICAL_STARTS.len(),
        "ALL_STARTS must hold every value from both classes and nothing else"
    );

    for &start in CANONICAL_STARTS.iter().chain(NON_CANONICAL_STARTS.iter()) {
        assert!(
            ALL_STARTS.contains(&start),
            "{start:#010x} is in a class but missing from ALL_STARTS"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.2 -- known answers
// ---------------------------------------------------------------------------------------

/// Every hand-derived known answer must match, through both entry points.
///
/// The derivations are in the doc comment on `ORACLE_KNOWN_ANSWERS`; each was worked out from
/// RFC 1950's definition and then confirmed against the C implementation. `b"hello, hello!"`
/// is the payload `test/example.c` compresses, and its expected value is the one the doc
/// example in `src/adler32/mod.rs` also asserts, so the two cannot drift apart.
#[test]
fn hand_derived_known_answers() {
    for &(input, expected) in &ORACLE_KNOWN_ANSWERS {
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, input),
            expected,
            "adler32 of {input:?}"
        );
        assert_eq!(
            adler32_z(ADLER32_INITIAL_VALUE, input),
            expected,
            "adler32_z of {input:?}"
        );
    }
}

/// The two fixtures shared with the C test suite must match, NUL byte included.
///
/// The length assertion is the substance of this test as much as the checksum is:
/// `common::corpus::HELLO` is 14 bytes and `DICTIONARY` is 6, because `test/example.c` passes
/// `strlen(hello) + 1` and `sizeof(dictionary)` respectively. A 13-byte `HELLO` would still
/// produce a *valid* Adler-32 -- just not the one the C suite computes -- so the mistake would
/// survive every test that did not check the length.
#[test]
fn the_reference_suite_fixtures_match_including_their_nul() {
    for &(input, expected_len, expected) in &ORACLE_SUITE_FIXTURES {
        assert_eq!(
            input.len(),
            expected_len,
            "fixture length changed: {input:?} must stay {expected_len} bytes"
        );
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, input),
            expected,
            "adler32 of {input:?}"
        );
    }
}

/// `adler32` and `adler32_z` must agree on every input this file uses.
///
/// In Rust the two collapse: a slice carries its own length, so the `uInt`-versus-`z_size_t`
/// distinction that separates `zlib.h` L1809 from L1829 has nothing to express, and
/// `adler32` is a one-line forwarder exactly as `adler32.c` L128-L130 is. The assertion is
/// made anyway, across the whole boundary-length sweep and every starting value, so that a
/// future divergence -- someone giving one of them its own body -- is caught here rather than
/// in whichever exported C symbol happened to be tested less.
#[test]
fn the_two_entry_points_are_interchangeable() {
    let buf = corpus();

    for &start in ALL_STARTS {
        for &len in BACKEND_SWEEP_LENS {
            let chunk = prefix(&buf, len);
            assert_eq!(
                adler32(start, chunk),
                adler32_z(start, chunk),
                "adler32 and adler32_z disagree at start {start:#010x}, len {len}"
            );
        }
    }

    for &(input, _) in &ORACLE_KNOWN_ANSWERS {
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, input),
            adler32_z(ADLER32_INITIAL_VALUE, input)
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.3 -- the three code paths, and the boundaries between them
//
// `adler32.c` has three paths and they do not reduce alike, so which one a length reaches is
// behaviour and not merely performance. This section pins each path, then crosses every
// boundary between them from both sides.
// ---------------------------------------------------------------------------------------

/// Every one of the 256 byte values must take the C single-byte fast path.
///
/// From the seed, `adler32.c` L70-L78 gives `s1 = 1 + byte` and then `s2 = 0 + s1`, so both
/// halves are the same number and the answer is `(1 + byte) | ((1 + byte) << 16)`. No
/// reduction can occur: the largest value reachable is `1 + 255 = 256`, far below `BASE`. All
/// 256 cases are enumerated rather than sampled because the whole set costs less than one
/// checksum over a kilobyte.
#[test]
fn every_single_byte_takes_the_c_fast_path() {
    for byte in 0..=u8::MAX {
        let sum = 1 + u32::from(byte);
        let expected = sum | (sum << 16);
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, &[byte]),
            expected,
            "single byte {byte:#04x}"
        );
    }

    // The two ends, spelled out, so the formula above cannot be quietly rewritten into
    // agreement with a wrong implementation.
    assert_eq!(adler32(ADLER32_INITIAL_VALUE, &[0x00]), 0x0001_0001);
    assert_eq!(adler32(ADLER32_INITIAL_VALUE, &[0xff]), 0x0100_0100);
}

/// The reduction schedule of all three paths must match `adler32.c` exactly.
///
/// This is the test that pins behavioural fidelity to the reference. Every row of
/// `ORACLE_ZERO_RUNS` is a run of
/// zero bytes fed from `0xffff_fff0`, so `s1` is pinned at `65_520` throughout and the table
/// isolates `s2`. The discontinuities are the point: row 0 reduces with a modulo, row 1
/// reduces with a *single* conditional subtraction and lands on `0xfffe` where a modulo would
/// have given `0x000d`, row 2 is back to a modulo, and rows 15 to 16 cross from the short path
/// into the block engine.
///
/// A reformulation that reduced every path the same way would be shorter, clearer and wrong.
/// It would pass every other test in this file and fail row 1 of this one.
#[test]
fn the_reduction_schedule_matches_the_c_source_on_every_path() {
    let zeros = vec![0_u8; ORACLE_ZERO_RUNS.len()];

    for &(len, expected) in &ORACLE_ZERO_RUNS {
        assert_eq!(
            adler32(ZERO_RUN_START, prefix(&zeros, len)),
            expected,
            "reduction schedule differs at len {len} from start {ZERO_RUN_START:#010x}"
        );
    }

    // Row 1 restated on its own, because it is the one row a "tidied" implementation breaks.
    // s1 = 0xfff0 = 65_520 stays put; s2 = 0xffff = 65_535, then s2 += s1 = 131_055.
    // 131_055 is above 2 * BASE (131_042), so one subtraction leaves 65_534 = 0xfffe.
    assert_eq!(adler32(0xffff_fff0, &[0x00]), 0xfffe_fff0);
    // The same starting value one step further along, which repeats the effect.
    assert_eq!(adler32(0xfffe_fff0, &[0x00]), 0xfffd_fff0);
}

/// Short lengths must agree with the independent reference for every canonical seed.
///
/// `0..=32` straddles two boundaries: the short-path cutoff at 16 (`adler32.c` L85) and the
/// point at which the block engine's 16-byte unrolled step first runs twice. Both halves of
/// each comparison are computed here, so this covers arbitrary length rather than the handful
/// of lengths a pinned table can carry.
#[test]
fn short_lengths_agree_with_the_independent_reference() {
    let buf = corpus();

    for &start in SWEEP_STARTS {
        for len in 0..=32 {
            let chunk = prefix(&buf, len);
            assert_eq!(
                adler32(start, chunk),
                reference_adler32(start, chunk),
                "library and reference differ at start {start:#010x}, len {len}"
            );
        }
    }
}

/// Every reduction boundary must agree with the independent reference.
///
/// `BOUNDARY_LENS` includes `NMAX - 1`, `NMAX`, `NMAX + 1`, `2 * NMAX` and `2 * NMAX + 1` --
/// the lengths at which the modulo schedule switches from one block to the next, which is
/// where an off-by-one in the block loop shows up and nowhere else. It also includes 63, 64
/// and 65, which straddle the vectorised backend's lane threshold, so the same sweep covers
/// both backends' path structure.
///
/// The largest of these is 11 105 bytes. That is deliberate: it crosses every boundary the
/// engine has, and a megabyte buffer would cross none of them a second time while making the
/// Miri gate unusable.
#[test]
fn every_reduction_boundary_agrees_with_the_independent_reference() {
    let buf = corpus();

    for &start in SWEEP_STARTS {
        for &len in SWEEP_LENS {
            let chunk = prefix(&buf, len);
            assert_eq!(
                adler32(start, chunk),
                reference_adler32(start, chunk),
                "library and reference differ at start {start:#010x}, len {len}"
            );
        }
    }
}

/// The pseudo-random fixture itself must be the one every pinned value was taken over.
///
/// Cheap, and the precondition for every `ORACLE_*` table in the file: if `common::lcg_fill` or
/// `CORPUS_SEED` ever changed, those tables would be measuring different bytes and would all fail
/// at once with no indication why. This test says why. It is kept separate from the table it
/// guards precisely so that it still runs where the bulk comparisons are skipped.
#[test]
fn the_pseudo_random_fixture_is_pinned() {
    let buf = corpus();

    assert_eq!(buf.len(), CORPUS_LEN);
    assert_eq!(
        prefix(&buf, 8),
        [7, 216, 4, 17, 235, 119, 143, 206],
        "the lcg_fill fixture changed; every pinned value in this file is taken over it"
    );
    // A longer request must extend the same sequence rather than restart it, which is what makes
    // `corpus_of` safe to use for the two tests that need more than `CORPUS_LEN` bytes.
    assert_eq!(prefix(&corpus_of(CORPUS_LEN + 64), CORPUS_LEN), &buf[..]);
}

/// The pinned prefix checksums must match the C implementation.
///
/// Where the previous two tests check the library against a second Rust implementation, this
/// one checks it against the C one. Both are needed: the reference loop covers lengths no
/// table could enumerate, and the table covers the possibility that the reference loop and the
/// library are wrong in the same way.
// Skipped under Miri: a bulk comparison of numbers against the C implementation over the longest
// fixtures, which is not what an undefined-behaviour interpreter adds value to. Every code path it
// reaches is already interpreted by the sweeps that do run under Miri, the fixture it depends on is
// still pinned by the test above, and this test itself runs in full on every native `cargo test`.
#[cfg_attr(miri, ignore)]
#[test]
fn the_pinned_prefix_checksums_match_the_c_implementation() {
    let buf = corpus();

    for &(len, expected) in &ORACLE_PREFIXES {
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, prefix(&buf, len)),
            expected,
            "pinned prefix checksum differs at len {len}"
        );
    }
}

/// The pinned starting-value matrix must match the C implementation.
///
/// Seven starting values across six lengths. Three of the seven are non-canonical, and those
/// eighteen rows are the only coverage anywhere in this suite for what `adler32.c` does with a
/// half at or above `BASE` over a non-trivial buffer -- the independent reference cannot supply
/// it, for the reason given in the module documentation.
// Skipped under Miri: this is a bulk comparison of numbers against the C implementation over the
// longest fixtures in the file, which is not what an undefined-behaviour interpreter adds value
// to. Every code path it reaches is already interpreted by the sweeps that do run under Miri, and
// the test itself runs in full on every native `cargo test`.
#[cfg_attr(miri, ignore)]
#[test]
fn the_pinned_starting_value_matrix_matches_the_c_implementation() {
    let buf = corpus();

    for &(start, len, expected) in &ORACLE_STARTS {
        assert_eq!(
            adler32(start, prefix(&buf, len)),
            expected,
            "pinned checksum differs at start {start:#010x}, len {len}"
        );
    }
}

/// Every payload class of the shared corpus must match the C implementation.
///
/// A different kind of coverage from the length sweeps: these are byte *distributions* -- a
/// long run of one value, pseudo-random noise, natural-language prose, an exhaustive cycle
/// over all 256 values, and a payload that crosses the 32 `KiB` window. The class name is
/// carried into the failure message so a break names the class rather than an offset.
///
/// The independent reference is checked alongside the pinned value, which makes each row a
/// three-way agreement between the library, a second Rust implementation and C.
// Skipped under Miri: this is a bulk comparison of numbers against the C implementation over the
// longest fixtures in the file, which is not what an undefined-behaviour interpreter adds value
// to. Every code path it reaches is already interpreted by the sweeps that do run under Miri, and
// the test itself runs in full on every native `cargo test`.
#[cfg_attr(miri, ignore)]
#[test]
fn every_corpus_class_matches_the_c_implementation() {
    let classes = common::corpus::all();
    assert_eq!(
        classes.len(),
        ORACLE_CLASSES.len(),
        "common::corpus::all() gained or lost a class; the pinned table must follow"
    );

    for (&(name, expected_len, expected), (actual_name, payload)) in
        ORACLE_CLASSES.iter().zip(classes.iter())
    {
        assert_eq!(name, *actual_name, "corpus classes are out of order");
        assert_eq!(
            payload.len(),
            expected_len,
            "corpus class {name} changed length"
        );
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, payload),
            expected,
            "corpus class {name} differs from the C implementation"
        );
        assert_eq!(
            reference_adler32(ADLER32_INITIAL_VALUE, payload),
            expected,
            "corpus class {name} differs from the independent reference"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.4 -- incremental feeding versus a single call
//
// This is how the checksum is actually driven. `deflate` updates it from `read_buf` as input
// arrives (`deflate.c` L229) and `inflate` updates it as output is produced, so the chunk
// sizes are whatever a caller's buffering happens to produce. A chunk boundary that fell on a
// path switch or a reduction edge and changed the answer would corrupt streams for exactly the
// callers whose buffering was unluckiest, which is the worst possible failure mode.
// ---------------------------------------------------------------------------------------

/// Splitting a 600-byte buffer at *every* offset must reproduce the single-call value.
///
/// All 601 offsets, natively. Each one is a different pair of paths -- the first call may take
/// the single-byte, short or block path and so may the second -- so the sweep is really a
/// 601-case check that the paths compose, not merely that one of them works.
///
/// Under Miri the offsets are sampled with `SPLIT_STRIDE`; the endpoints and the four offsets
/// around the short-path cutoff are added back explicitly so the sample cannot miss them.
#[test]
fn every_split_of_a_600_byte_buffer_reproduces_the_single_call_value() {
    let buf = corpus();
    let whole = prefix(&buf, SPLIT_SWEEP_LEN);
    let expected = adler32(ADLER32_INITIAL_VALUE, whole);

    // Anchor the single-call value to C before using it as an expectation. The literal is the
    // `SPLIT_SWEEP_LEN` row of `ORACLE_PREFIXES`, restated here so this test does not silently
    // depend on a table it never reads.
    assert_eq!(expected, 0x4780_32a8, "pinned 600-byte checksum");
    assert_eq!(expected, reference_adler32(ADLER32_INITIAL_VALUE, whole));

    let sampled = (0..=SPLIT_SWEEP_LEN).step_by(SPLIT_STRIDE);
    let forced = [0, 1, 15, 16, 17, SPLIT_SWEEP_LEN - 1, SPLIT_SWEEP_LEN];

    for split in sampled.chain(forced) {
        let (head, tail) = whole.split_at(split);
        let incremental = adler32(adler32(ADLER32_INITIAL_VALUE, head), tail);
        assert_eq!(
            incremental, expected,
            "splitting 600 bytes at {split} changed the checksum"
        );
    }
}

/// The structurally interesting splits of the long fixture must reproduce the single-call value.
///
/// 11 105 bytes is too many offsets to sweep, and almost all of them would repeat a case
/// already covered. `INTERESTING_SPLITS` keeps the ones that are not repeats: a boundary either
/// side of the short-path cutoff, and a boundary either side of a `NMAX` reduction, so a
/// reduction lands mid-chunk, exactly on a chunk edge, and one byte past one.
#[test]
fn the_interesting_splits_of_the_long_fixture_reproduce_the_single_call_value() {
    let buf = corpus();
    let expected = adler32(ADLER32_INITIAL_VALUE, &buf);
    // The `CORPUS_LEN` row of `ORACLE_PREFIXES`, restated for the same reason as above.
    assert_eq!(expected, 0x7262_99ae, "pinned whole-fixture checksum");

    for &split in LONG_SPLITS {
        let (head, tail) = buf.split_at(split);
        assert_eq!(
            adler32(adler32(ADLER32_INITIAL_VALUE, head), tail),
            expected,
            "splitting the long fixture at {split} changed the checksum"
        );
    }

    // And from the far end, so a split that leaves a short tail is covered as well as one that
    // leaves a short head.
    for &offset in LONG_SPLITS {
        let split = CORPUS_LEN - offset;
        let (head, tail) = buf.split_at(split);
        assert_eq!(
            adler32(adler32(ADLER32_INITIAL_VALUE, head), tail),
            expected,
            "splitting the long fixture at {split} changed the checksum"
        );
    }
}

/// A three-way split must reproduce the single-call value.
///
/// Two splits rather than one, which is what catches an implementation that reset some piece of
/// per-call state correctly the first time and not the second.
#[test]
fn three_way_chunking_reproduces_the_single_call_value() {
    let buf = corpus();
    let whole = prefix(&buf, SPLIT_SWEEP_LEN);
    let expected = adler32(ADLER32_INITIAL_VALUE, whole);

    for &first in THREE_WAY_HEADS {
        for &second in THREE_WAY_MIDDLES {
            let third = SPLIT_SWEEP_LEN - first - second;
            let mut running = adler32(ADLER32_INITIAL_VALUE, &whole[..first]);
            running = adler32(running, &whole[first..first + second]);
            running = adler32(running, &whole[first + second..]);
            assert_eq!(
                running, expected,
                "three-way split {first}/{second}/{third} changed the checksum"
            );
        }
    }
}

/// Feeding one byte at a time must reproduce the single-call value.
///
/// The most demanding chunking there is, and not a hypothetical one: it drives the
/// `len == 1` fast path on every call, and that path is the one with the asymmetric reduction.
/// `zlib.h` L1821-L1824 shows callers the accumulate-in-a-loop idiom precisely so that they may
/// feed the checksum whatever they have, and a byte is what they have when they are reading a
/// stream a byte at a time.
#[test]
fn byte_at_a_time_feeding_reproduces_the_single_call_value() {
    let buf = corpus();
    let whole = prefix(&buf, SPLIT_SWEEP_LEN);
    let expected = adler32(ADLER32_INITIAL_VALUE, whole);

    let mut running = ADLER32_INITIAL_VALUE;
    for &byte in whole {
        running = adler32(running, &[byte]);
    }

    assert_eq!(
        running, expected,
        "byte-at-a-time feeding must equal the single-call value"
    );
}

/// Fixed-size chunking at each interesting size must reproduce the single-call value.
///
/// `chunks` is exactly the shape a caller with a fixed input buffer produces. The sizes are the
/// ones that put a reduction inside a chunk, on a chunk edge and one byte past an edge, so the
/// block engine's carry of `s1` and `s2` across calls is exercised in all three alignments.
#[test]
fn fixed_size_chunking_reproduces_the_single_call_value() {
    let buf = corpus();
    let expected = adler32(ADLER32_INITIAL_VALUE, &buf);

    for &size in LONG_SPLITS {
        let mut running = ADLER32_INITIAL_VALUE;
        for chunk in buf.chunks(size) {
            running = adler32(running, chunk);
        }
        assert_eq!(
            running, expected,
            "feeding the long fixture in {size}-byte chunks changed the checksum"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.5 -- adler32_combine
//
// Concatenation without re-reading either sequence. `adler32.c` L133-L155 does it in eight
// steps whose order is load-bearing twice over: the sign test has to come first so the modulo
// may assume a non-negative length, and the modulo has to come before `rem * sum1` so that the
// product cannot overflow and `BASE - rem` cannot underflow.
// ---------------------------------------------------------------------------------------

/// The pinned combine vectors must match the C implementation.
///
/// Ten rows covering the identity, the concatenation, both length-reduction cases and the
/// maximum internal product. Their individual significance is set out on `ORACLE_COMBINE`.
#[test]
fn the_pinned_combine_vectors_match_the_c_implementation() {
    for &(adler1, adler2, len2, expected) in &ORACLE_COMBINE {
        assert_eq!(
            adler32_combine(adler1, adler2, len2),
            expected,
            "combine({adler1:#010x}, {adler2:#010x}, {len2}) differs from C"
        );
    }
}

/// Combining the checksums of two pieces must equal the checksum of the whole.
///
/// The defining property, checked over a matrix of head and tail lengths that includes an empty
/// head, an empty tail, both empty, and lengths on both sides of the short-path cutoff and of
/// the `NMAX` reduction edge. Every expectation is computed rather than remembered, so this
/// covers combinations no pinned table could enumerate.
#[test]
fn combining_two_pieces_reproduces_the_whole() {
    // Two lengths of `NMAX + 1` sum to one byte more than `CORPUS_LEN`, so this matrix needs a
    // slightly longer prefix of the same fixture. See `corpus_of`.
    let buf = corpus_of(2 * (NMAX + 1));

    for &head_len in COMBINE_MATRIX_LENS {
        for &tail_len in COMBINE_MATRIX_LENS {
            let whole = prefix(&buf, head_len + tail_len);
            let (head, tail) = whole.split_at(head_len);

            let combined = adler32_combine(
                adler32(ADLER32_INITIAL_VALUE, head),
                adler32(ADLER32_INITIAL_VALUE, tail),
                len2_of(tail_len),
            );

            assert_eq!(
                combined,
                adler32(ADLER32_INITIAL_VALUE, whole),
                "combining {head_len} + {tail_len} bytes did not reproduce the whole"
            );
        }
    }
}

/// Concatenating nothing must change nothing.
///
/// `adler32_combine(a, 1, 0)` has to return `a`, because the second sequence is empty and the
/// checksum of an empty sequence is the seed. The row exercises the `rem == 0` branch, where
/// `BASE - rem` is at its largest and so where a formulation that let that term overflow would
/// break.
///
/// A second identity is asserted alongside it: `adler32_combine(1, a, n)` must return `a`, for
/// the mirror-image reason -- prefixing an empty sequence changes nothing either.
#[test]
fn combining_with_an_empty_sequence_is_the_identity() {
    let buf = corpus();

    for &len in SWEEP_LENS {
        let chunk = prefix(&buf, len);
        let checksum = adler32(ADLER32_INITIAL_VALUE, chunk);

        assert_eq!(
            adler32_combine(checksum, ADLER32_INITIAL_VALUE, 0),
            checksum,
            "appending nothing to a {len}-byte sequence changed its checksum"
        );
        assert_eq!(
            adler32_combine(ADLER32_INITIAL_VALUE, checksum, len2_of(len)),
            checksum,
            "prefixing nothing to a {len}-byte sequence changed its checksum"
        );
    }

    // Also for a maximally-large canonical checksum, which is not a value the corpus produces.
    assert_eq!(
        adler32_combine(0xfff0_fff0, ADLER32_INITIAL_VALUE, 0),
        0xfff0_fff0
    );
}

/// A negative length must return the documented sentinel, `0xffff_ffff`.
///
/// `adler32.c` L138-L140 returns "an invalid adler32 as a clue for debugging". That is
/// documented behaviour, not an error path to normalise: a caller who passes a negative length
/// gets back a value no genuine checksum can equal -- both halves are `0xffff`, above `BASE` --
/// and so notices.
///
/// `i64::MIN` is in the list because it is the value that catches an implementation which
/// negated or took the absolute value of the length before testing its sign; `-(1 << 32)` is
/// there because it catches one that narrowed to 32 bits first, where it would read as zero.
#[test]
fn a_negative_length_returns_the_documented_sentinel() {
    for &len2 in &NEGATIVE_LENGTHS {
        assert_eq!(
            adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, len2),
            NEGATIVE_LEN_SENTINEL,
            "len2 {len2} must yield the debugging sentinel"
        );
    }

    // That the sentinel is not a reachable checksum -- both halves exceed BASE -- is asserted at
    // compile time beside the constant itself.

    // The starting values must not affect it -- the sign test is the very first statement.
    assert_eq!(
        adler32_combine(0, 0, -1),
        NEGATIVE_LEN_SENTINEL,
        "the sign test must precede every other step"
    );
}

/// Combining must be associative over a three-way split.
///
/// `(a \u{2016} b) \u{2016} c` and `a \u{2016} (b \u{2016} c)` must agree, and both must equal
/// the checksum of the concatenation. Associativity is what makes the operation usable for the
/// tree-shaped reductions it exists for -- combining per-chunk checksums in whatever order the
/// chunks finish -- so it is a property of the contract and not an accident of the formula.
#[test]
fn combining_is_associative_over_a_three_way_split() {
    // Natively three pieces of roughly `NMAX` bytes each, so every one of them crosses a reduction
    // edge and none of them is a degenerate short case. See `corpus_of` for why the total is longer
    // than `CORPUS_LEN`, and `ASSOCIATIVITY_PARTS` for what Miri runs instead.
    let [head_len, middle_len, tail_len] = ASSOCIATIVITY_PARTS;
    let buf = corpus_of(head_len + middle_len + tail_len);
    let (first, rest) = buf.split_at(head_len);
    let (second, third) = rest.split_at(middle_len);
    assert_eq!(
        third.len(),
        tail_len,
        "the three pieces must partition the fixture"
    );

    let a = adler32(ADLER32_INITIAL_VALUE, first);
    let b = adler32(ADLER32_INITIAL_VALUE, second);
    let c = adler32(ADLER32_INITIAL_VALUE, third);

    let left = adler32_combine(
        adler32_combine(a, b, len2_of(second.len())),
        c,
        len2_of(third.len()),
    );
    let right = adler32_combine(
        a,
        adler32_combine(b, c, len2_of(third.len())),
        len2_of(second.len() + third.len()),
    );

    assert_eq!(left, right, "combine is not associative");
    assert_eq!(
        left,
        adler32(ADLER32_INITIAL_VALUE, &buf),
        "the associative folds must equal the whole-fixture checksum"
    );
}

/// A length above `BASE` must be reduced before it is used, and a length above `u32::MAX` must
/// not be truncated.
///
/// Both halves of this test target the same statement, `adler32.c` L143, from opposite sides.
///
/// * Adding any multiple of `BASE` to a length must leave the answer unchanged, because the
///   length enters the formula only through `len2 % BASE`. If the reduction were skipped, `rem`
///   could exceed `BASE`, `rem * sum1` could overflow `u32` and `BASE - rem` could underflow.
/// * Adding `2^32` must **change** the answer, because `(2^32 + 7) % BASE` is 232 and not 7.
///   An implementation that narrowed the length to 32 bits before reducing -- `len2 as u32`
///   turns `2^32 + 7` into `7` -- returns the unchanged answer here and is caught.
#[test]
fn the_length_is_reduced_modulo_base_at_full_width() {
    let base = i64::from(BASE);
    // The checksum of `common::corpus::HELLO`, derived on `ORACLE_SUITE_FIXTURES`: the two halves
    // of that payload joined at `len2 = 7` must reproduce it.
    let baseline = adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, 7);
    assert_eq!(baseline, 0x2606_0496, "the concatenation of the two halves");

    for multiplier in [1_i64, 2, 1_000, 100_000, 140_000_000_000_000] {
        assert_eq!(
            adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, 7 + multiplier * base),
            baseline,
            "adding {multiplier} multiples of BASE changed the answer"
        );
    }

    // A length that is itself a multiple of BASE must behave exactly like zero.
    assert_eq!(
        adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, base),
        adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, 0),
        "a length congruent to zero must behave like zero"
    );

    // And the width test: 2^32 + 7 is congruent to 232, not to 7.
    let wide = (1_i64 << 32) + 7;
    assert_eq!(wide % base, 232, "the arithmetic this row depends on");
    assert_eq!(
        adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, wide),
        0x3c84_0496,
        "a length above u32::MAX must not be truncated"
    );
    assert_ne!(
        adler32_combine(HEAD_CHECKSUM, TAIL_CHECKSUM, wide),
        baseline,
        "truncating len2 to 32 bits would make this row equal the 7-byte answer"
    );
}

// ---------------------------------------------------------------------------------------
// 3.6 -- output neutrality of the `simd` backend
//
// THE RULE THIS SECTION ENFORCES. Vectorisation is permitted for the two checksum modules and
// prohibited everywhere else, and the
// permission rests on one argument: a checksum collapses to a single scalar however it is
// computed, so a vector arrangement can change how long the computation takes and nothing
// else. Vectorised match finding is prohibited under the same argument read the other way --
// the order in which match candidates are examined decides which match is emitted, so there the
// arrangement *is* observable in the output.
//
// An argument is not a guarantee. This section is the guarantee: it compares the two backends
// directly, bit for bit, across every path boundary and every starting value this file knows
// about. If they ever disagree on one input, the premise is false, the `simd` feature is
// invalid and it must not ship.
//
// Note what the section does NOT do. It does not read `is_supported()`, which is crate-private
// by construction (`src/adler32/simd.rs` L276) and is a throughput hint rather than a
// correctness guard. Instead it asserts that the dispatcher -- which does read it -- agrees with
// both backends, so the result is identical whichever branch the probe selects. That is the
// property the requirement actually needs: a `simd`-enabled build must be correct on hardware
// with no vector unit at all.
// ---------------------------------------------------------------------------------------

// The sweeps below run over `ALL_STARTS`, which is every starting value this file knows -- both
// canonical and non-canonical. Both kinds belong here even though the non-canonical ones lie
// outside the independent reference's domain: this section compares one backend against the other
// rather than either against the reference, so the domain restriction does not apply, and the
// non-canonical values are precisely where the asymmetric reductions live, which makes them the
// ones most likely to expose a divergence.

/// The two backends must agree on every length from 0 to 64.
///
/// This range is chosen against the implementation rather than at random. `Adler32Simd`
/// delegates to `Adler32Generic` for a single byte, for anything shorter than 16 bytes and for
/// anything shorter than its lane threshold of 64, then takes the lane arrangement from 64
/// upwards -- so 0 through 64 walks every one of its four entry decisions and crosses the last
/// of them exactly at the top of the range.
#[cfg(feature = "simd")]
#[test]
fn the_two_backends_agree_on_every_length_to_sixty_four() {
    let buf = corpus();

    for &start in ALL_STARTS {
        for len in 0..=64 {
            let chunk = prefix(&buf, len);
            let generic = Adler32Generic::checksum(start, chunk);
            let simd = Adler32Simd::checksum(start, chunk);
            assert_eq!(
                simd, generic,
                "backends disagree at start {start:#010x}, len {len}"
            );
        }
    }
}

/// The two backends must agree at every reduction boundary.
///
/// Where the previous test walks the lane threshold, this one walks the `NMAX` reduction edges,
/// which are the boundaries the lane arrangement has to respect in order to stay inside a `u32`.
/// A vector backend that carried its accumulators one block too far would overflow, and it would
/// overflow first at exactly these lengths.
#[cfg(feature = "simd")]
#[test]
fn the_two_backends_agree_at_every_reduction_boundary() {
    let buf = corpus();

    for &start in ALL_STARTS {
        for &len in SWEEP_LENS {
            let chunk = prefix(&buf, len);
            assert_eq!(
                Adler32Simd::checksum(start, chunk),
                Adler32Generic::checksum(start, chunk),
                "backends disagree at start {start:#010x}, len {len}"
            );
        }
    }
}

/// The dispatcher must agree with *both* backends, so its choice cannot be observed.
///
/// `adler32` and `adler32_z` consult the crate-private throughput hint and then call one backend
/// or the other. Asserting a three-way equality here is what makes that choice unobservable: it
/// holds whichever branch was taken, on this machine and on any other, which is exactly the
/// requirement that a `simd`-enabled build be correct on hardware lacking a vector unit.
#[cfg(feature = "simd")]
#[test]
fn the_dispatcher_agrees_with_both_backends() {
    let buf = corpus();

    for &start in ALL_STARTS {
        for &len in BACKEND_SWEEP_LENS {
            let chunk = prefix(&buf, len);
            let dispatched = adler32(start, chunk);
            assert_eq!(
                dispatched,
                Adler32Generic::checksum(start, chunk),
                "dispatcher differs from the generic backend at start {start:#010x}, len {len}"
            );
            assert_eq!(
                dispatched,
                Adler32Simd::checksum(start, chunk),
                "dispatcher differs from the simd backend at start {start:#010x}, len {len}"
            );
            assert_eq!(dispatched, adler32_z(start, chunk));
        }
    }
}

/// Every pinned expectation in this file must hold for both backends and the dispatcher.
///
/// The pinned tables were taken from C, so this closes the loop: it is not enough that the two
/// backends agree with each other, they must both agree with the reference implementation. Any
/// row failing here means a `simd` build would emit a stream the C implementation rejects.
// Skipped under Miri: this is a bulk comparison of numbers against the C implementation over the
// longest fixtures in the file, which is not what an undefined-behaviour interpreter adds value
// to. Every code path it reaches is already interpreted by the sweeps that do run under Miri, and
// the test itself runs in full on every native `cargo test`.
#[cfg_attr(miri, ignore)]
#[cfg(feature = "simd")]
#[test]
fn every_pinned_expectation_holds_for_both_backends() {
    let buf = corpus();

    for &(input, expected) in &ORACLE_KNOWN_ANSWERS {
        assert_eq!(
            Adler32Generic::checksum(ADLER32_INITIAL_VALUE, input),
            expected
        );
        assert_eq!(
            Adler32Simd::checksum(ADLER32_INITIAL_VALUE, input),
            expected
        );
    }

    for &(input, _, expected) in &ORACLE_SUITE_FIXTURES {
        assert_eq!(
            Adler32Generic::checksum(ADLER32_INITIAL_VALUE, input),
            expected
        );
        assert_eq!(
            Adler32Simd::checksum(ADLER32_INITIAL_VALUE, input),
            expected
        );
    }

    for &(len, expected) in &ORACLE_PREFIXES {
        let chunk = prefix(&buf, len);
        assert_eq!(
            Adler32Generic::checksum(ADLER32_INITIAL_VALUE, chunk),
            expected,
            "generic backend differs from C at len {len}"
        );
        assert_eq!(
            Adler32Simd::checksum(ADLER32_INITIAL_VALUE, chunk),
            expected,
            "simd backend differs from C at len {len}"
        );
    }

    for &(start, len, expected) in &ORACLE_STARTS {
        let chunk = prefix(&buf, len);
        assert_eq!(Adler32Generic::checksum(start, chunk), expected);
        assert_eq!(Adler32Simd::checksum(start, chunk), expected);
    }

    // The reduction-schedule table, which is the one a vector backend is most likely to get
    // wrong: it can only be reproduced by delegating the short paths rather than reimplementing
    // them, which is what `Adler32Simd` does.
    let zeros = vec![0_u8; ORACLE_ZERO_RUNS.len()];
    for &(len, expected) in &ORACLE_ZERO_RUNS {
        let chunk = prefix(&zeros, len);
        assert_eq!(
            Adler32Generic::checksum(ZERO_RUN_START, chunk),
            expected,
            "generic backend differs from C on the zero run at len {len}"
        );
        assert_eq!(
            Adler32Simd::checksum(ZERO_RUN_START, chunk),
            expected,
            "simd backend differs from C on the zero run at len {len}"
        );
    }
}

/// Both backends must agree with C on every payload class of the shared corpus.
///
/// The broadest byte distributions available, including the 33 048-byte window-crossing payload
/// -- the only fixture in this file long enough to take the lane arrangement through six whole
/// reduction blocks.
// Skipped under Miri: this is a bulk comparison of numbers against the C implementation over the
// longest fixtures in the file, which is not what an undefined-behaviour interpreter adds value
// to. Every code path it reaches is already interpreted by the sweeps that do run under Miri, and
// the test itself runs in full on every native `cargo test`.
#[cfg_attr(miri, ignore)]
#[cfg(feature = "simd")]
#[test]
fn both_backends_match_c_on_every_corpus_class() {
    for (&(name, _, expected), (_, payload)) in
        ORACLE_CLASSES.iter().zip(common::corpus::all().iter())
    {
        assert_eq!(
            Adler32Generic::checksum(ADLER32_INITIAL_VALUE, payload),
            expected,
            "generic backend differs from C on corpus class {name}"
        );
        assert_eq!(
            Adler32Simd::checksum(ADLER32_INITIAL_VALUE, payload),
            expected,
            "simd backend differs from C on corpus class {name}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.7 -- the shape of the backend trait
//
// The swappable-backend trait is the mechanism that guarantees a vectorised checksum is
// interchangeable with a scalar one. A trait only guarantees that if it is genuinely
// the interface -- if a caller can be written once, generically, and instantiated with either
// backend. `via` is that caller, and these tests are what make the guarantee real rather than
// asserted.
// ---------------------------------------------------------------------------------------

/// `Adler32Generic` must be reachable and correct through the trait.
///
/// `checksum` is an associated function with no `self`, so the type is named rather than
/// constructed and `via` has to reach it through the bound. Agreement with the free function is
/// asserted alongside, because a trait implementation that computed something different from the
/// dispatcher would be a trait that documented a lie.
#[test]
fn the_generic_backend_is_correct_through_the_trait() {
    let buf = corpus();

    for &start in SWEEP_STARTS {
        for &len in BACKEND_SWEEP_LENS {
            let chunk = prefix(&buf, len);
            let through_trait = via::<Adler32Generic>(start, chunk);
            assert_eq!(
                through_trait,
                Adler32Generic::checksum(start, chunk),
                "the trait call must equal the inherent one at start {start:#010x}, len {len}"
            );
            assert_eq!(
                through_trait,
                reference_adler32(start, chunk),
                "the trait call must equal the independent reference at len {len}"
            );
        }
    }

    for &(input, expected) in &ORACLE_KNOWN_ANSWERS {
        assert_eq!(
            via::<Adler32Generic>(ADLER32_INITIAL_VALUE, input),
            expected
        );
    }
}

/// The marker types must carry the derives their consumers rely on.
///
/// `Default` lets a backend be produced without naming a constructor, `Copy` and `Clone` let one
/// be passed around freely, and `Debug` is what a failing assertion prints. None of them affects
/// a checksum, which is why they are checked by construction here rather than by arithmetic: the
/// value of the test is that removing a derive breaks the build at this line instead of somewhere
/// downstream.
#[test]
fn the_backend_marker_carries_its_derives() {
    let generic: Adler32Generic = default_of();
    let cloned = clone_of(&generic);
    let copied = cloned;

    // Zero-sized, so naming a backend costs nothing at run time.
    assert_eq!(size_of_val(&copied), 0, "the marker must be zero-sized");
    // `cloned` is still usable after the assignment above, which is `Copy` at work.
    assert_eq!(format!("{cloned:?}"), "Adler32Generic");
    assert_eq!(format!("{copied:?}"), "Adler32Generic");
    // And the trait is reachable in a build that constructs a backend this way, not only from the
    // type name. The literal is the `b"abc"` row of `ORACLE_KNOWN_ANSWERS`, whose derivation is
    // `s1 = 1 + 97 + 98 + 99 = 295 = 0x127` and `s2 = 98 + 196 + 295 = 589 = 0x24d`.
    assert_eq!(
        via::<Adler32Generic>(ADLER32_INITIAL_VALUE, b"abc"),
        0x024d_0127
    );

    #[cfg(feature = "simd")]
    {
        let simd: Adler32Simd = default_of();
        let simd_copy = clone_of(&simd);
        assert_eq!(size_of_val(&simd_copy), 0, "the marker must be zero-sized");
        assert_eq!(format!("{simd:?}"), "Adler32Simd");
        assert_eq!(format!("{simd_copy:?}"), "Adler32Simd");
    }
}

/// Both backends must be substitutable through the trait, with identical results.
///
/// The generic function is instantiated twice from one body, which is the substitutability claim
/// stated in the type system rather than in prose, and the two instantiations are then required
/// to agree bit for bit -- the same output-neutrality property as §3.6, reached through the
/// abstraction that is supposed to guarantee it.
#[cfg(feature = "simd")]
#[test]
fn both_backends_are_substitutable_through_the_trait() {
    let buf = corpus();

    for &start in ALL_STARTS {
        for &len in BACKEND_SWEEP_LENS {
            let chunk = prefix(&buf, len);
            assert_eq!(
                via::<Adler32Simd>(start, chunk),
                via::<Adler32Generic>(start, chunk),
                "the two trait instantiations disagree at start {start:#010x}, len {len}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.7 -- the dispatcher's entry decisions, at the exact lengths the review named
//
// These run in EVERY build, with and without the `simd` feature, which is the point: the
// dispatcher tests the input's length before it asks the machine anything, so the value it
// returns must not depend on which of those two questions was asked first, nor on whether the
// second is asked at all. A `#[cfg(feature = "simd")]` gate here would leave the default build
// -- the one almost every consumer gets -- asserting nothing about the reordering.
// ---------------------------------------------------------------------------------------

/// Six lengths that straddle every entry decision, plus the incremental case, through the
/// dispatcher.
///
/// The lengths are the ones a review of this dispatch asked for -- 0, 1, 16, 63, 64, 65 -- and each
/// is a boundary rather than a sample: 0 is the empty update, 1 is C's single-byte fast path
/// (`adler32.c` L70), 16 is the sub-chunk width, 63 and 64 straddle the lane threshold below which
/// the vectorised backend hands the input straight back, and 65 is the first length past it.
///
/// The dispatcher now tests the length before consulting target-feature detection, so 0 through 63
/// reach the scalar backend without a detection call at all. That reordering is a throughput change
/// and must be a value-preserving one, which is what this asserts: every length, against the
/// independent reference, from every canonical starting value.
///
/// The one-byte incremental sweep is the case the reordering exists for. A caller feeding a stream a
/// byte at a time makes one dispatch per byte -- `deflate` updates the checksum once per `read_buf`
/// (`deflate.c` L229), so the frequency is the caller's chunk size, not the library's -- and it must
/// arrive at exactly the value a single call over the whole buffer produces.
#[test]
fn the_dispatcher_is_value_preserving_at_every_entry_decision() {
    let buf = corpus();

    for &start in &CANONICAL_STARTS {
        for len in [0_usize, 1, 16, 63, 64, 65] {
            let chunk = prefix(&buf, len);
            let expected = reference_adler32(start, chunk);
            assert_eq!(
                adler32_z(start, chunk),
                expected,
                "adler32_z differs from the reference at start {start:#010x}, len {len}"
            );
            assert_eq!(
                adler32(start, chunk),
                expected,
                "adler32 differs from the reference at start {start:#010x}, len {len}"
            );
        }

        // One byte at a time, across the lane threshold: 65 dispatches where a single call makes
        // one. Each of those 65 is below the threshold, so none of them consults detection.
        let whole = prefix(&buf, 65);
        let mut running = start;
        for &byte in whole {
            running = adler32_z(running, &[byte]);
        }
        assert_eq!(
            running,
            adler32_z(start, whole),
            "one byte at a time must equal one call at start {start:#010x}"
        );
        assert_eq!(
            running,
            reference_adler32(start, whole),
            "and both must equal the reference at start {start:#010x}"
        );
    }
}
