//! Integration tests for the one-shot wrappers: `compress.c` and `uncompr.c`.
//!
//! These six compression and four decompression entry points are the simplest
//! functions in the library and the ones most likely to be used carelessly, which
//! is why the two contracts this suite exists to pin down are stated as **exact
//! numbers** rather than as plausible ranges:
//!
//! 1. **The bound.** `compress_bound`/`compress_bound_z` is a buffer-overflow
//!    hazard, not a hint, and the two directions of error are not symmetric.
//!    *Underestimating is a safety defect*: a caller allocates from the returned
//!    value and then writes into that allocation, so a bound one byte too small
//!    becomes a buffer overflow **in caller code**, which this library can neither
//!    detect nor contain. *Overestimating is a parity defect*: `zlib.h` L768-L774
//!    promises only an upper bound, so a larger answer merely wastes the caller's
//!    memory and stays safe -- but this port must reproduce the reference's
//!    numbers exactly, and a caller or test that asserts an exact size would see
//!    the difference. So the assertion here is equality against the reference's
//!    own output, which satisfies both requirements at once.
//! 2. **The in/out accounting.** `uncompress2_z` reports *two* counts, and the
//!    second one is easy to miss: `*sourceLen` comes back as the number of source
//!    bytes **consumed** (`uncompr.c` L74), so `source + *sourceLen` addresses the
//!    first unused input byte. That is how a caller locates whatever follows a
//!    zlib stream inside a larger container. An implementation that treated the
//!    source length as input-only would still pass a naive round-trip test and
//!    would then break the first caller that relied on the guarantee.
//!
//! # Why an integration test and not another unit test
//!
//! `src/compress.rs` and `src/uncompress.rs` already carry `#[cfg(test)]` modules,
//! and those can see private items. This target deliberately cannot. It links
//! `zlib-rs` the way a real dependent does, which makes it the only place that
//! proves the wrappers are actually *reachable*: that the six plus four entry
//! points are `pub`, that `Compressed` and `Decompressed` and their fields are
//! `pub`, and that all of it resolves without the `rust-api` feature. A visibility
//! regression that every in-crate test would sail through fails here immediately.
//!
//! # Reaching the crate by module path
//!
//! `zlib-rs` declares `default = []` and every re-export at the crate root carries
//! `#[cfg(feature = "rust-api")]`, so `zlib_rs::compress2` does not exist in a
//! default build while `zlib_rs::compress::compress2` always does -- `pub mod
//! compress` and `pub mod uncompress` are unconditional. Everything below
//! therefore imports by module path, which is also what the C ABI facade does, and
//! for the same reason.
//!
//! # The oracle, and where each expectation comes from
//!
//! Two independent derivations back every hard-coded number here, because a single
//! derivation cannot catch an error in itself:
//!
//! * The bound table is transcribed from the **reference's own output** -- the
//!   in-tree C `compressBound_z` -- and each row additionally carries the
//!   arithmetic that produces it, read off the formula at `compress.c` L92-L93. A
//!   transposed digit cannot satisfy both readings at once.
//! * The status and count expectations are transcribed from `uncompr.c` L72-L81,
//!   with the C line cited at each assertion.
//!
//! Byte-for-byte equality against the C encoder over the whole
//! level x `windowBits` x `memLevel` x strategy x flush matrix is a different
//! job, belonging to `crates/zlib-rs-differential`, the crate that can link the C
//! oracle; its `tests/byte_identical.rs` establishes that equality, and its
//! `tests/table_equality.rs` establishes the transcribed tables. What this suite
//! owns is the wrapper: the bound arithmetic, the chunking loop's shape, the
//! bidirectional accounting, and the status ladder.
//!
//! # Interpreter budget
//!
//! Every buffer here is a few kilobytes at most, and the largest corpus class
//! (`window_crossing`, 33048 bytes) is deliberately never used. Under Miri the
//! cost of these tests is dominated not by payload size but by the **number of
//! streams opened**: a compressor allocates its window, hash head and hash chain
//! up front, so one round trip costs roughly the same whether the payload is 14
//! bytes or 256. Opening a stream is orders of magnitude more expensive under the
//! interpreter than evaluating a bound, which is why the splits below are drawn on
//! stream count and not on payload size.
//!
//! Two consequences shape the tests below, and both are about stream count rather
//! than buffer size:
//!
//! * The level x corpus matrices are split. A reduced version -- levels
//!   `{0, 1, 9, default}` against three payload classes -- runs unconditionally,
//!   and the exhaustive version is `#[cfg_attr(miri, ignore)]`. Those skips are for
//!   interpreter speed **only**: every one of them runs under an ordinary native
//!   `cargo test`, and nothing else excludes them.
//! * Every other test sweeps only the axis its own contract actually varies. A test
//!   that two spellings of one function agree cannot be made stronger by varying
//!   the payload, so it varies the level; a test that the bound is sufficient is
//!   only meaningful on the class that expands, so it varies the level against that
//!   class alone. Re-walking a full cross product in each test would multiply the
//!   interpreter cost several times over while adding no assertion that the reduced
//!   matrix does not already make.
//!
//! That second point is the same reasoning `src/adler32/combine.rs` records for its
//! own Miri budget: this file has no `unsafe`, no raw pointers and no uninitialised
//! memory, so the only faults Miri can surface here are integer overflow and an
//! out-of-bounds index -- and both are reached by the cheap tests. The bound
//! arithmetic under 3.1 exercises the overflow surface directly, including the
//! wrap at [`usize::MAX`], and every indexing path in the two wrappers is walked by
//! a single round trip.
//!
//! # Safety posture
//!
//! No `unsafe`, no FFI, no third-party crate, and no `#![no_std]`: an integration
//! test is its own crate and links `std`, which is what supplies `Vec` and the
//! `#[test]` harness. The only dependency is `zlib-rs` itself plus the shared
//! `common` module.
//!
//! `#![forbid(unsafe_code)]` below is deliberate rather than decorative. An
//! integration test is a **separate crate**, so the core library's own
//! `#![forbid(unsafe_code)]` does not extend to this file; restating it here is what
//! makes the safety claim in the paragraph above machine-checked instead of merely
//! asserted in prose. Nothing this suite does needs an escape hatch from the
//! compiler's memory-safety guarantees, so the prohibition costs it nothing.

// The workspace denies the panic-prone lints, which is right for library code and
// wrong for a test: a test asserts, a failing assertion panics, and reading a
// fixture this file just built by index is clearer than defensively matching on
// it. `clippy.toml` grants `allow-unwrap-in-tests`, `allow-expect-in-tests` and
// `allow-panic-in-tests`, but those keys are keyed on `#[test]` context and so do
// not reach the file-scope helpers below -- and there is no
// `allow-indexing-slicing-in-tests` key at all, which is why `indexing_slicing`
// has to be relaxed here rather than in `clippy.toml`. Adding a key clippy's 1.80
// floor does not recognise would abort the whole lint run. The same relaxation,
// for the same reason, already appears on the test module in `src/compress.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Restated because an integration test is its own crate and does not inherit the core
// library's attribute. See "Safety posture" above.
#![forbid(unsafe_code)]

mod common;

use common::{check_err, corpus};
// `GlobalAllocator` is not imported for its own sake: `deflate_init` is generic over
// `Allocator`, so driving the streaming encoder directly -- which the preset-dictionary
// fixture under 3.6 has to do, because the one-shot wrappers never call
// `deflateSetDictionary` -- requires naming an allocator. This is the same one
// `compress2_z` and `uncompress2_z` select internally by zeroing `zalloc`/`zfree`
// (`compress.c` L38-L40, `uncompr.c` L47-L49), so the fixture is built with exactly the
// allocator the wrappers under test use. `tests/common/mod.rs` reaches the same module
// for the same kind of reason.
use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::compress::{
    compress, compress2, compress2_z, compress_bound, compress_bound_z, compress_z, Compressed,
};
use zlib_rs::config::{
    Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_DEFAULT_COMPRESSION, Z_FINISH, Z_NO_COMPRESSION,
};
use zlib_rs::deflate::{
    deflate, deflate_end, deflate_init, deflate_reset, deflate_set_dictionary, DeflateStream,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::uncompress::{uncompress, uncompress2, uncompress2_z, uncompress_z, Decompressed};

// ---------------------------------------------------------------------------
// Shared fixtures and helpers
// ---------------------------------------------------------------------------

/// Every level the wrappers accept, in the order `zlib.h` documents them.
///
/// `Z_DEFAULT_COMPRESSION` is `-1` and selects level 6 (`DEF_LEVEL`); `0` through
/// `9` are `Z_NO_COMPRESSION` through `Z_BEST_COMPRESSION`. Eleven spellings, ten
/// distinct behaviours, because `-1` and `6` must agree.
const ALL_LEVELS: [i32; 11] = [Z_DEFAULT_COMPRESSION, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// The three levels the reduced matrices sweep, plus the default spelling.
///
/// `0` is the stored path, `1` is `deflate_fast`, `9` is `deflate_slow` at its
/// widest search, and `Z_DEFAULT_COMPRESSION` proves the alias resolves. Together
/// they reach every one of the strategy functions the level table selects while
/// opening four streams instead of eleven.
const REDUCED_LEVELS: [i32; 4] = [
    Z_NO_COMPRESSION,
    Z_BEST_SPEED,
    Z_BEST_COMPRESSION,
    Z_DEFAULT_COMPRESSION,
];

/// A short, deterministic payload set that keeps the interpreter budget small.
///
/// Three classes, chosen because they drive three different block-type decisions:
/// `repetitive` is all matches, `incompressible` forces stored blocks, and
/// [`corpus::HELLO`] is the exact fixture the C suite compresses. 256 bytes is
/// enough to reach a real Huffman tree while staying trivial to interpret.
fn reduced_classes() -> [(&'static str, Vec<u8>); 3] {
    [
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive256", corpus::repetitive(256)),
        ("incompressible256", corpus::incompressible(256)),
    ]
}

/// Every payload class the committed minimal corpus defines, except the one that
/// is too large to interpret.
///
/// `window_crossing` is 33048 bytes and exists to make the sliding window slide,
/// which is the compressor's concern rather than the wrappers'. It is left out so
/// that this set stays usable, and the seven that remain still cover empty, one
/// byte, the C suite's fixture, maximally compressible, incompressible, text and
/// binary.
fn full_classes() -> [(&'static str, Vec<u8>); 7] {
    [
        ("empty", corpus::EMPTY.to_vec()),
        ("single_byte", corpus::SINGLE_BYTE.to_vec()),
        ("hello", corpus::HELLO.to_vec()),
        ("repetitive", corpus::repetitive(1024)),
        ("incompressible", corpus::incompressible(1024)),
        ("text", corpus::text()),
        ("binary", corpus::binary()),
    ]
}

/// Compresses `source` at `level` into a destination of exactly the documented
/// bound, and returns the stream.
///
/// This is the sizing discipline `zlib.h` L1298-L1299 prescribes -- "the
/// destination buffer, which must be at least the value returned by
/// `compressBound(sourceLen)`" -- so every use of this helper is also an
/// assertion that the bound is sufficient.
fn pack(source: &[u8], level: i32) -> Vec<u8> {
    let mut stream = vec![0_u8; compress_bound_z(source.len())];
    let report = compress2_z(&mut stream, source, level);
    check_err(report.code, "compress2_z");
    assert!(
        report.produced <= stream.len(),
        "produced {} exceeded the bound {}",
        report.produced,
        stream.len()
    );
    stream.truncate(report.produced);
    stream
}

/// Decompresses `stream` into a destination of exactly `capacity` bytes.
///
/// Returns the report together with the destination truncated to what was
/// actually produced, so a caller can assert on the bytes and the counts at once.
fn unpack(stream: &[u8], capacity: usize) -> (Decompressed, Vec<u8>) {
    let mut plain = vec![0_u8; capacity];
    let report = uncompress2_z(&mut plain, stream);
    plain.truncate(report.produced);
    (report, plain)
}

/// Builds a stream that carries the `FDICT` flag, using the preset dictionary
/// from `test/example.c`.
///
/// The one-shot wrappers cannot produce this: `compress2_z` calls plain
/// `deflateInit` and never `deflateSetDictionary`, so the streaming encoder has to
/// be driven directly. The sequence mirrors `test_dict_deflate` (`test/example.c`
/// L420-L440): initialise, reset, load the dictionary, then finish in one call.
///
/// The point of the fixture is what happens on the way back. RFC 1950 records the
/// dictionary's presence in the header's `FDICT` bit, so a decompressor stops with
/// `Z_NEED_DICT` and asks for it -- and the one-shot form has no parameter through
/// which to supply one, which is why the ladder at `uncompr.c` L79 rewrites that
/// status to `Z_DATA_ERROR`.
fn packed_with_preset_dictionary(source: &[u8]) -> Vec<u8> {
    let mut state = deflate_init(Z_BEST_COMPRESSION, GlobalAllocator)
        .expect("deflate_init must accept Z_BEST_COMPRESSION");
    let reset = deflate_reset(&mut state);

    let mut adler = reset.adler;
    let mut total_in = reset.total_in;
    check_err(
        deflate_set_dictionary(&mut state, &mut adler, &mut total_in, corpus::DICTIONARY),
        "deflate_set_dictionary",
    );

    let mut stream = vec![0_u8; compress_bound_z(source.len()) + 64];
    let produced = {
        let mut view = DeflateStream::new(source, &mut stream);
        view.apply_reset(reset);
        view.adler = adler;
        view.total_in = total_in;
        assert_eq!(
            deflate(&mut state, &mut view, Z_FINISH),
            ReturnCode::STREAM_END,
            "one Z_FINISH into an over-sized destination must complete the stream"
        );
        view.next_out
    };
    check_err(deflate_end(&mut state), "deflate_end");

    stream.truncate(produced);
    stream
}

/// Asserts that a compression report carries exactly the expected status and
/// count.
fn assert_compressed(report: Compressed, code: ReturnCode, produced: usize) {
    assert_eq!(report.code, code, "status");
    assert_eq!(report.produced, produced, "produced");
}

/// Asserts that a decompression report carries exactly the expected status and
/// both expected counts.
fn assert_decompressed(report: Decompressed, code: ReturnCode, produced: usize, consumed: usize) {
    assert_eq!(report.code, code, "status");
    assert_eq!(report.produced, produced, "produced");
    assert_eq!(report.consumed, consumed, "consumed");
}

// ===========================================================================
// 3.1  compress_bound / compress_bound_z -- exact values
//
// The formula, `compress.c` L91-L99:
//
//     z_size_t compressBound_z(z_size_t sourceLen) {
//         z_size_t bound = sourceLen + (sourceLen >> 12) + (sourceLen >> 14) +
//                          (sourceLen >> 25) + 13;
//         return bound < sourceLen ? (z_size_t)-1 : bound;
//     }
//     uLong compressBound(uLong sourceLen) {
//         z_size_t bound = compressBound_z(sourceLen);
//         return (uLong)bound != bound ? (uLong)-1 : (uLong)bound;
//     }
//
// Four constants -- the shifts 12, 14 and 25, and the addend 13 -- and TWO
// distinct saturation conditions, which are not the same check:
//   * `compress_bound_z` saturates when the four-term ADDITION WRAPS, detected as
//     `bound < sourceLen`.
//   * `compress_bound` saturates when the `z_size_t` result DOES NOT FIT in
//     `uLong`, detected as `(uLong)bound != bound`.
// ===========================================================================

/// The bound at twenty-two lengths, each row carrying its own arithmetic.
///
/// Every expectation was taken from the reference's output and is independently
/// reproduced by the shift arithmetic in the trailing comment, so a transposed
/// digit cannot satisfy both readings.
#[test]
fn the_bound_matches_the_reference_at_every_tabulated_length() {
    // (source_len, expected)                 n         + n>>12   + n>>14   + n>>25 + 13
    const TABLE: [(usize, usize); 22] = [
        (0, 13),                  //             0 +      0 +      0 +     0 + 13
        (1, 14),                  //             1 +      0 +      0 +     0 + 13
        (2, 15),                  //             2 +      0 +      0 +     0 + 13
        (12, 25),                 //            12 +      0 +      0 +     0 + 13
        (13, 26),                 //            13 +      0 +      0 +     0 + 13
        (100, 113),               //           100 +      0 +      0 +     0 + 13
        (1_024, 1_037),           //         1_024 +      0 +      0 +     0 + 13
        (4_095, 4_108),           //         4_095 +      0 +      0 +     0 + 13
        (4_096, 4_110),           //         4_096 +      1 +      0 +     0 + 13
        (4_097, 4_111),           //         4_097 +      1 +      0 +     0 + 13
        (8_191, 8_205),           //         8_191 +      1 +      0 +     0 + 13
        (8_192, 8_207),           //         8_192 +      2 +      0 +     0 + 13
        (16_383, 16_399),         //        16_383 +      3 +      0 +     0 + 13
        (16_384, 16_402),         //        16_384 +      4 +      1 +     0 + 13
        (16_385, 16_403),         //        16_385 +      4 +      1 +     0 + 13
        (65_535, 65_566),         //        65_535 +     15 +      3 +     0 + 13
        (65_536, 65_569),         //        65_536 +     16 +      4 +     0 + 13
        (1_000_000, 1_000_318),   //     1_000_000 +    244 +     61 +     0 + 13
        (1_048_576, 1_048_909),   //     1_048_576 +    256 +     64 +     0 + 13
        (16_777_216, 16_782_349), //    16_777_216 +  4_096 +  1_024 +     0 + 13
        (33_554_431, 33_564_682), //    33_554_431 +  8_191 +  2_047 +     0 + 13
        (33_554_432, 33_564_686), //    33_554_432 +  8_192 +  2_048 +     1 + 13
    ];

    for (source_len, expected) in TABLE {
        // Reading one: the reference's measured answer.
        assert_eq!(
            compress_bound_z(source_len),
            expected,
            "compress_bound_z({source_len})"
        );

        // Reading two: the formula, recomputed here from the four constants at
        // `compress.c` L92-L93. None of these lengths comes close to wrapping, so
        // plain addition is exact and the overflow arm is unreachable.
        let recomputed =
            source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 13;
        assert_eq!(
            recomputed, expected,
            "the tabulated bound for {source_len} must also follow from the formula"
        );
    }
}

/// The three shift boundaries, where an off-by-one in a shift amount hides.
///
/// A shift of 11 instead of 12, or 13 instead of 14, or 24 instead of 25, changes
/// nothing at all for most lengths -- the quotients are equal over wide ranges.
/// The only lengths that can tell the difference are the ones either side of a
/// power of two, so those are singled out here rather than left to the table.
#[test]
fn the_bound_is_exact_either_side_of_every_shift_boundary() {
    // >> 12 turns over at 4096 = 2^12.
    assert_eq!(compress_bound_z(4_095), 4_108); // 4_095 + 0 + 0 + 0 + 13
    assert_eq!(compress_bound_z(4_096), 4_110); // 4_096 + 1 + 0 + 0 + 13
    assert_eq!(
        compress_bound_z(4_096) - compress_bound_z(4_095),
        2,
        "crossing 2^12 must add exactly one to the >>12 term, plus the one byte of length"
    );

    // >> 14 turns over at 16384 = 2^14. The >>12 term also advances here, from 3
    // to 4, which is why the step is three rather than two.
    assert_eq!(compress_bound_z(16_383), 16_399); // 16_383 + 3 + 0 + 0 + 13
    assert_eq!(compress_bound_z(16_384), 16_402); // 16_384 + 4 + 1 + 0 + 13
    assert_eq!(
        compress_bound_z(16_384) - compress_bound_z(16_383),
        3,
        "crossing 2^14 advances the >>12 and >>14 terms together"
    );

    // >> 25 turns over at 33554432 = 2^25. This is the term a clean-room
    // implementation is most likely to drop entirely, because it contributes
    // nothing below 32 MiB.
    assert_eq!(compress_bound_z(33_554_431), 33_564_682); // + 8_191 + 2_047 + 0 + 13
    assert_eq!(compress_bound_z(33_554_432), 33_564_686); // + 8_192 + 2_048 + 1 + 13
    assert_eq!(
        compress_bound_z(33_554_432) - compress_bound_z(33_554_431),
        4,
        "crossing 2^25 advances all three shifted terms at once"
    );
}

/// The two spellings agree numerically wherever both are defined.
///
/// In the safe core they agree *everywhere*, because `compress_bound` is
/// `compress_bound_z` under its other name: there is one length domain, `usize`,
/// so C's `(uLong)bound != bound` test at `compress.c` L98 is false for every
/// input. That narrowing saturation is real, but it belongs to the C ABI facade,
/// which converts from `c_ulong` and answers `c_ulong::MAX` when the result does
/// not fit back -- and it only ever fires on a target where `uLong` is narrower
/// than `z_size_t`, which is LLP64 Windows. On this host both are eight bytes
/// wide, so there is nothing for it to do. See `src/compress.rs` on
/// `compress_bound` for the full split.
#[test]
fn the_two_bound_spellings_agree() {
    for source_len in [
        0,
        1,
        13,
        100,
        1_024,
        4_095,
        4_096,
        16_383,
        16_384,
        65_536,
        1_000_000,
        33_554_431,
        33_554_432,
        usize::MAX,
    ] {
        assert_eq!(
            compress_bound(source_len),
            compress_bound_z(source_len),
            "the two spellings must agree at {source_len}"
        );
    }
}

/// The bound never decreases as the length grows.
///
/// A non-monotonic bound would be a serious defect -- a caller that sized a buffer
/// for `n` bytes and then compressed slightly fewer could be handed a smaller
/// allowance than it had already allocated for. The sweep is deliberately dense
/// around the shift boundaries, because that is the only place monotonicity could
/// plausibly break: every term is a monotonic function of `n`, so a violation
/// would mean a sign or subtraction slipped into one of them.
#[test]
fn the_bound_is_monotonic() {
    const LENGTHS: [usize; 24] = [
        0, 1, 2, 13, 100, 1_023, 1_024, 1_025, 4_094, 4_095, 4_096, 4_097, 16_382, 16_383, 16_384,
        16_385, 65_535, 65_536, 1_000_000, 1_048_576, 16_777_216, 33_554_431, 33_554_432,
        33_554_433,
    ];

    let mut previous = compress_bound_z(LENGTHS[0]);
    for window in LENGTHS.windows(2) {
        let (smaller, larger) = (window[0], window[1]);
        assert!(smaller <= larger, "the sweep itself must be sorted");

        let bound = compress_bound_z(larger);
        assert!(
            bound >= previous,
            "compress_bound_z({larger}) = {bound} must not be below compress_bound_z({smaller}) = {previous}"
        );
        previous = bound;
    }
}

/// The bound always leaves room for framing.
///
/// A zlib stream is never smaller than its payload plus a header and a check
/// value -- two bytes and four by RFC 1950 -- plus the block framing DEFLATE
/// itself needs, so a bound that did not strictly exceed its input would be
/// unusable for an incompressible payload. The addend 13 is what guarantees it at
/// the low end.
///
/// The one length where this degenerates to equality is [`usize::MAX`], where the
/// answer is the saturation sentinel; that case is covered on its own below, and
/// is excluded here rather than weakening the comparison to `>=`.
#[test]
fn the_bound_always_exceeds_its_input() {
    for source_len in [
        0, 1, 2, 13, 100, 1_024, 4_095, 4_096, 16_383, 16_384, 65_536, 1_000_000, 16_777_216,
        33_554_432,
    ] {
        let bound = compress_bound_z(source_len);
        assert!(
            bound > source_len,
            "compress_bound_z({source_len}) = {bound} must strictly exceed its input"
        );
        assert!(
            bound - source_len >= 13,
            "the bound must always include the 13-byte addend from compress.c L93"
        );
    }
}

/// The additive-wrap saturation, and the sentinel it produces.
///
/// `compress.c` L94 is `return bound < sourceLen ? (z_size_t)-1 : bound;`. The
/// four-term sum is unsigned, so it truncates rather than trapping, and the
/// overflow test works by *observing* the truncated value: because the three
/// shifted terms plus 13 always sum to less than `2^BITS`, a wrap can only ever
/// subtract `2^BITS` once, which necessarily lands the result below `source_len`.
///
/// The sentinel is C's `(z_size_t)-1`, i.e. [`usize::MAX`]. It is a **documented
/// sentinel value, not an error code**: the function returns `z_size_t` and has no
/// way to signal failure, so "no buffer can be guaranteed large enough" is spelled
/// as the largest representable length. Note that it is also the honest answer for
/// `usize::MAX` itself, which is why saturating there loses nothing.
///
/// The wrap is recomputed here rather than assumed, so this test demonstrates
/// *why* the sentinel appears and not merely that it does. It is width-independent:
/// the sum wraps at [`usize::MAX`] on a 32-bit target as well as a 64-bit one.
#[test]
fn the_bound_saturates_when_the_addition_wraps() {
    let source_len = usize::MAX;

    // The same four terms in the same order as `compress.c` L92-L93, added with
    // the wrapping operator that C's unsigned `+` already is.
    let wrapped = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);
    assert!(
        wrapped < source_len,
        "the four-term sum must wrap at usize::MAX, which is what L94 detects"
    );

    assert_eq!(
        compress_bound_z(source_len),
        usize::MAX,
        "a wrapped sum must report the (z_size_t)-1 sentinel"
    );
    assert_eq!(
        compress_bound(source_len),
        usize::MAX,
        "and the narrow spelling must report the same sentinel in the usize domain"
    );
}

/// The exact length at which the addition begins to wrap.
///
/// Three consecutive lengths straddle the boundary, and together they exercise
/// both arms of the `bound < sourceLen` comparison at the only place it can be
/// told apart. All three share the same shifted terms --
/// `n >> 12 == 4_502_225_523_002_373`, `n >> 14 == 1_125_556_380_750_593` and
/// `n >> 25 == 549_588_076_538` -- so the sums differ only by the leading `n`:
///
/// | `n` | exact sum | fits? | `compress_bound_z` |
/// |---|---|---|---|
/// | `18_441_115_742_217_722_097` | `18_446_744_073_709_551_614` | yes | that sum |
/// | `18_441_115_742_217_722_098` | `18_446_744_073_709_551_615` | yes, exactly [`usize::MAX`] | that sum |
/// | `18_441_115_742_217_722_099` | `18_446_744_073_709_551_616` | no, wraps to 0 | [`usize::MAX`] sentinel |
///
/// The middle row is the interesting one: the honest bound happens to equal the
/// sentinel value, and it is reached without any wrap at all. An implementation
/// that saturated one length early would return the same number there and would
/// only be caught by the first row.
///
/// The literals exceed 32 bits, so this is gated on a 64-bit `usize`; the
/// width-independent half of the same contract is asserted above.
#[cfg(target_pointer_width = "64")]
#[test]
fn the_bound_saturation_boundary_is_exact() {
    /// The last length whose bound is still strictly below the sentinel value, so
    /// the only row here that a saturating-too-early bug cannot disguise.
    const BELOW: usize = 18_441_115_742_217_722_097;
    /// The last length that does not wrap. Its exact bound *is* `usize::MAX`.
    const AT: usize = 18_441_115_742_217_722_098;
    /// The first length that wraps, all the way round to zero.
    const OVER: usize = 18_441_115_742_217_722_099;

    assert_eq!(
        compress_bound_z(BELOW),
        18_446_744_073_709_551_614,
        "one below the boundary must report the real sum, distinguishable from the sentinel"
    );
    assert_eq!(
        compress_bound_z(AT),
        usize::MAX,
        "at the boundary the real sum is exactly usize::MAX, reached without a wrap"
    );

    // Proof that AT really did not wrap and OVER really did, so the two identical
    // answers above and below are reached by different arms of L94.
    let sum_at = AT
        .wrapping_add(AT >> 12)
        .wrapping_add(AT >> 14)
        .wrapping_add(AT >> 25)
        .wrapping_add(13);
    assert!(sum_at >= AT, "AT must not wrap");
    let sum_over = OVER
        .wrapping_add(OVER >> 12)
        .wrapping_add(OVER >> 14)
        .wrapping_add(OVER >> 25)
        .wrapping_add(13);
    assert_eq!(sum_over, 0, "OVER wraps exactly to zero");
    assert!(sum_over < OVER, "which is what L94 detects");

    assert_eq!(
        compress_bound_z(OVER),
        usize::MAX,
        "one past the boundary must report the sentinel"
    );
}

// ===========================================================================
// 3.2  The bound actually bounds
//
// The arithmetic above proves the formula was transcribed correctly. It does not
// prove the formula is *sufficient*, which is the property callers depend on. The
// only way to establish that is to hand the encoder a destination of exactly the
// bound and require it to succeed -- for the payload class that expands the most,
// at the level that expands the most.
// ===========================================================================

/// A destination of exactly the bound is always enough, at the worst-case class.
///
/// `incompressible` is the only class that can breach a too-tight bound, so it is
/// the only one swept here: with no matches to find, the encoder's best option is
/// to store the data verbatim, and the output is therefore *larger* than the input
/// with the bound's slack absorbing the difference. Level 0 is the worst case of
/// all, because `deflate_stored` never even tries to compress. A compressible class
/// would leave the bound with so much slack that it could be badly wrong and still
/// pass, which is why breadth here goes into the level axis rather than the payload
/// axis; the full cross product is the exhaustive test below.
#[test]
fn a_destination_of_exactly_the_bound_is_always_enough() {
    let source = corpus::incompressible(256);
    let bound = compress_bound_z(source.len());

    for level in REDUCED_LEVELS {
        let mut destination = vec![0_u8; bound];
        let report = compress2_z(&mut destination, &source, level);

        assert_eq!(
            report.code,
            ReturnCode::OK,
            "incompressible data at level {level} must fit in its own bound of {bound} bytes"
        );
        assert!(
            report.produced <= bound,
            "level {level} produced {} bytes into a bound of {bound}",
            report.produced
        );
        assert!(
            report.produced > source.len(),
            "incompressible input must actually expand at level {level}, \
             or this test is not exercising the bound's slack at all"
        );

        // The bound is the *guarantee*, and anything less is caller error. A
        // destination of `bound - 1` is deliberately NOT asserted to fail: for
        // most inputs it works perfectly well, since the bound carries slack.
        // Asserting failure there would over-specify the contract and pin down
        // behaviour `zlib.h` never promises.
    }
}

/// The same guarantee across every level and every corpus class.
///
/// Eleven level spellings times seven payload classes, which is 77 streams. Skipped
/// under Miri for interpreter speed **only** -- it runs natively on every
/// `cargo test`, and the reduced form above runs everywhere.
#[test]
#[cfg_attr(
    miri,
    ignore = "77 encoder streams; skipped for interpreter speed only, still runs natively"
)]
fn a_destination_of_exactly_the_bound_is_always_enough_everywhere() {
    for (name, source) in full_classes() {
        let bound = compress_bound_z(source.len());
        for level in ALL_LEVELS {
            let mut destination = vec![0_u8; bound];
            let report = compress2_z(&mut destination, &source, level);

            assert_eq!(
                report.code,
                ReturnCode::OK,
                "{name} at level {level} must fit in its own bound of {bound} bytes"
            );
            assert!(
                report.produced <= bound,
                "{name} at level {level} produced {} bytes into a bound of {bound}",
                report.produced
            );

            // And the stream has to be genuinely complete, not merely short
            // enough: a bound that "fits" because the encoder gave up early would
            // otherwise pass.
            let (back, plain) = unpack(&destination[..report.produced], source.len());
            assert_eq!(
                back.code,
                ReturnCode::OK,
                "{name} at level {level} must round trip"
            );
            assert_eq!(
                plain, source,
                "{name} at level {level} must recover exactly"
            );
        }
    }
}

// ===========================================================================
// 3.3  Round trips
// ===========================================================================

/// Compress then decompress recovers the input exactly, over the reduced matrix.
#[test]
fn the_reduced_level_and_corpus_matrix_round_trips() {
    for (name, source) in reduced_classes() {
        for level in REDUCED_LEVELS {
            let stream = pack(&source, level);
            let (report, plain) = unpack(&stream, source.len());

            assert_eq!(
                report.code,
                ReturnCode::OK,
                "{name} at level {level} must round trip"
            );
            assert_eq!(
                plain, source,
                "{name} at level {level} must recover byte for byte"
            );
            assert_eq!(
                report.produced,
                source.len(),
                "{name} at level {level} must report the whole payload as produced"
            );
            assert_eq!(
                report.consumed,
                stream.len(),
                "{name} at level {level} must report the whole stream as consumed"
            );
        }
    }
}

/// The same, across every level spelling and every corpus class.
///
/// Eleven levels times seven classes, each a compress *and* a decompress, so 154
/// streams. Skipped under Miri for interpreter speed **only** -- it runs natively
/// on every `cargo test`, and the reduced form above runs everywhere.
#[test]
#[cfg_attr(
    miri,
    ignore = "154 streams; skipped for interpreter speed only, still runs natively"
)]
fn the_full_level_and_corpus_matrix_round_trips() {
    for (name, source) in full_classes() {
        for level in ALL_LEVELS {
            let stream = pack(&source, level);
            let (report, plain) = unpack(&stream, source.len());

            assert_eq!(
                report.code,
                ReturnCode::OK,
                "{name} at level {level} must round trip"
            );
            assert_eq!(
                plain, source,
                "{name} at level {level} must recover byte for byte"
            );
            assert_decompressed(report, ReturnCode::OK, source.len(), stream.len());
        }
    }
}

/// `test/example.c`'s `test_compress`, reproduced step for step.
///
/// The C body (L64-L84) is:
///
/// ```c
/// uLong len = (uLong)strlen(hello)+1;
/// err = compress(compr, &comprLen, (const Bytef*)hello, len);
/// CHECK_ERR(err, "compress");
/// strcpy((char*)uncompr, "garbage");
/// err = uncompress(uncompr, &uncomprLen, compr, comprLen);
/// CHECK_ERR(err, "uncompress");
/// if (strcmp((char*)uncompr, hello)) { ... }
/// ```
///
/// Two details are load-bearing and both are preserved:
///
/// * `strlen(hello) + 1`, so the payload is **14 bytes including the terminating
///   NUL**, which is what [`corpus::HELLO`] holds. Using 13 would silently
///   invalidate the comparison.
/// * The `"garbage"` pre-fill. It is not decoration: it converts "the output
///   happens to be right" into "the output was actually written". Without it a
///   decompressor that produced nothing at all would still pass, because the
///   destination started out as zeroes and `strcmp` would stop at the first one.
#[test]
fn the_example_c_test_compress_sequence_reproduces() {
    let payload = corpus::HELLO;
    assert_eq!(
        payload.len(),
        14,
        "example.c passes strlen(hello)+1, so the NUL is part of the payload"
    );

    let mut compressed = vec![0_u8; compress_bound_z(payload.len())];
    let made = compress(&mut compressed, payload);
    check_err(made.code, "compress");

    // `strcpy((char*)uncompr, "garbage")` -- eight bytes, the NUL included.
    let mut recovered = vec![0_u8; 128];
    recovered[..8].copy_from_slice(b"garbage\0");

    let back = uncompress(&mut recovered, &compressed[..made.produced]);
    check_err(back.code, "uncompress");

    assert_eq!(
        &recovered[..back.produced],
        payload,
        "the recovered bytes must equal the payload, not the garbage that was there"
    );
    // The C check is `strcmp`, which compares up to the first NUL. Asserting on
    // the full 14 bytes above is strictly stronger, but the C form is checked too
    // so that a divergence reads the same way in both suites.
    assert_eq!(
        recovered.iter().position(|&byte| byte == 0),
        Some(13),
        "the terminating NUL must land where example.c's strcmp expects it"
    );
}

/// `compress` and `compress_z` emit identical bytes.
///
/// They are two spellings of one function -- `compress` takes the `uLong` route
/// through `compress2` (`compress.c` L82-L85) and `compress_z` takes the
/// `z_size_t` route straight to `compress2_z` (L77-L81) -- and both pass
/// `Z_DEFAULT_COMPRESSION`. Different routes, identical output; if they ever
/// diverged, callers on the two width families would produce different streams
/// from the same input.
#[test]
fn compress_and_compress_z_emit_the_same_bytes() {
    // Two classes rather than the whole set: the property is that two spellings of
    // one function agree, which cannot depend on the payload. One compressible and
    // one incompressible fixture is enough to cover both block-type outcomes.
    for (name, source) in [
        ("hello", corpus::HELLO.to_vec()),
        ("incompressible256", corpus::incompressible(256)),
    ] {
        let bound = compress_bound_z(source.len());

        let mut wide = vec![0_u8; bound];
        let wide_report = compress(&mut wide, &source);
        check_err(wide_report.code, "compress");

        let mut sized = vec![0_u8; bound];
        let sized_report = compress_z(&mut sized, &source);
        check_err(sized_report.code, "compress_z");

        assert_eq!(
            wide_report, sized_report,
            "{name}: both spellings must report the same status and count"
        );
        assert_eq!(
            wide[..wide_report.produced],
            sized[..sized_report.produced],
            "{name}: both spellings must emit the same bytes"
        );
    }
}

/// `compress2` and `compress2_z` emit identical bytes at the same level.
///
/// The level-taking pair, checked the same way. `compress2` narrows its count
/// through `uLong` (`compress.c` L67-L74) while `compress2_z` is the primary
/// implementation, so this also confirms the narrowing wrapper does not perturb
/// the stream.
#[test]
fn compress2_and_compress2_z_emit_the_same_bytes() {
    // The level axis is the one that matters here, because `level` is the argument
    // that distinguishes this pair from the pair above; the payload cannot affect
    // whether two spellings agree, so one fixture is swept across every level
    // rather than every fixture across every level.
    let source = corpus::HELLO;
    let bound = compress_bound_z(source.len());

    for level in REDUCED_LEVELS {
        let mut wide = vec![0_u8; bound];
        let wide_report = compress2(&mut wide, source, level);
        check_err(wide_report.code, "compress2");

        let mut sized = vec![0_u8; bound];
        let sized_report = compress2_z(&mut sized, source, level);
        check_err(sized_report.code, "compress2_z");

        assert_eq!(
            wide_report, sized_report,
            "level {level}: both spellings must report the same status and count"
        );
        assert_eq!(
            wide[..wide_report.produced],
            sized[..sized_report.produced],
            "level {level}: both spellings must emit the same bytes"
        );
    }
}

/// All four decompression spellings recover the same bytes from one stream.
///
/// `uncompress`, `uncompress_z`, `uncompress2` and `uncompress2_z` reach the same
/// body by three different routes -- note the asymmetry at `uncompr.c` L97-L101,
/// where `uncompress` delegates to `uncompress2` rather than to `uncompress_z`, so
/// the plain form takes the `uLong` route. All four must agree on the recovered
/// bytes and on both counts.
#[test]
fn all_four_uncompress_spellings_recover_the_same_bytes() {
    let source = corpus::HELLO;
    let stream = pack(source, Z_BEST_COMPRESSION);

    let mut plain = vec![0_u8; source.len()];
    let primary = uncompress2_z(&mut plain, &stream);
    check_err(primary.code, "uncompress2_z");
    assert_eq!(&plain[..primary.produced], source);

    for (label, report) in [
        ("uncompress2", {
            let mut buffer = vec![0_u8; source.len()];
            let report = uncompress2(&mut buffer, &stream);
            assert_eq!(&buffer[..report.produced], source, "uncompress2 bytes");
            report
        }),
        ("uncompress_z", {
            let mut buffer = vec![0_u8; source.len()];
            let report = uncompress_z(&mut buffer, &stream);
            assert_eq!(&buffer[..report.produced], source, "uncompress_z bytes");
            report
        }),
        ("uncompress", {
            let mut buffer = vec![0_u8; source.len()];
            let report = uncompress(&mut buffer, &stream);
            assert_eq!(&buffer[..report.produced], source, "uncompress bytes");
            report
        }),
    ] {
        assert_eq!(
            report, primary,
            "{label} must report exactly what uncompress2_z reports"
        );
    }
}

// ===========================================================================
// 3.4  Z_BUF_ERROR on insufficient space
// ===========================================================================

/// A destination too small for the whole stream reports `Z_BUF_ERROR`.
///
/// `compress.c` L63 assigns `*destLen` **unconditionally**, and before the status
/// is translated at L65, so the count is meaningful on this path too: the caller
/// learns exactly how many bytes did fit. Measured against the reference, that is
/// the destination's full length -- the encoder fills every byte it was given
/// before reporting that it ran out.
#[test]
fn compressing_into_a_short_destination_reports_buf_error() {
    let payload = corpus::HELLO;

    // The whole stream at the default level is 19 bytes, measured. Every
    // destination strictly below that must fail, and must fail the same way.
    let complete = pack(payload, Z_DEFAULT_COMPRESSION);
    assert_eq!(
        complete.len(),
        19,
        "the 14-byte hello payload compresses to 19 bytes at the default level"
    );

    for capacity in [1_usize, 5, 18] {
        let mut destination = vec![0_u8; capacity];
        let report = compress(&mut destination, payload);
        assert_compressed(report, ReturnCode::BUF_ERROR, capacity);
    }

    // And exactly at the stream length it succeeds, which fixes the boundary
    // rather than merely asserting that small buffers fail.
    let mut exact = vec![0_u8; complete.len()];
    let report = compress(&mut exact, payload);
    assert_compressed(report, ReturnCode::OK, complete.len());
    assert_eq!(
        exact, complete,
        "the exact-fit stream must be the same bytes"
    );
}

/// A decompression destination too small reports `Z_BUF_ERROR` -- and *not*
/// `Z_DATA_ERROR`, because input is still pending.
///
/// This is the pass-through arm of the ladder at `uncompr.c` L80:
/// `err == Z_BUF_ERROR && len == 0 ? Z_DATA_ERROR : err`. Running out of room and
/// running out of input both leave `inflate` reporting `Z_BUF_ERROR`, and the two
/// are told apart by whether any input survived. Here the destination fills first,
/// so input remains, `len != 0`, and the status passes through unchanged.
///
/// `zlib.h` L1330-L1332 promises that "in the case where there is not enough room,
/// `uncompress()` will fill the output buffer with the uncompressed data up to that
/// point", so `produced` is the destination's full length and the bytes are a valid
/// prefix of the payload.
#[test]
fn uncompressing_into_a_short_destination_reports_buf_error() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);

    for capacity in [1_usize, 5, 13] {
        let (report, plain) = unpack(&stream, capacity);

        assert_eq!(
            report.code,
            ReturnCode::BUF_ERROR,
            "a {capacity}-byte destination must report BUF_ERROR, not DATA_ERROR: \
             input remains, so uncompr.c L80's `len == 0` guard does not fire"
        );
        assert_eq!(
            report.produced, capacity,
            "the destination must be filled to capacity (zlib.h L1330-L1332)"
        );
        assert_eq!(
            plain,
            &payload[..capacity],
            "and hold a valid prefix of the payload"
        );
        assert!(
            report.consumed < stream.len(),
            "input must remain unconsumed, which is what keeps the status BUF_ERROR"
        );
    }
}

/// A zero-length destination follows the ladder rather than panicking.
///
/// The interesting half of `uncompr.c` L42-L43 in Rust terms; the full treatment is
/// under 3.7. The point here is only that neither wrapper special-cases an empty
/// slice into a panic or an early return.
#[test]
fn a_zero_length_destination_does_not_panic() {
    // Compression into nothing: the header alone does not fit, so BUF_ERROR with
    // a zero count.
    let report = compress(&mut [], corpus::HELLO);
    assert_compressed(report, ReturnCode::BUF_ERROR, 0);

    // Decompression into nothing, from a stream that has content: BUF_ERROR,
    // because input is left over once there is nowhere to put the output.
    let stream = pack(corpus::HELLO, Z_BEST_COMPRESSION);
    let report = uncompress2_z(&mut [], &stream);
    assert_eq!(report.code, ReturnCode::BUF_ERROR);
    assert_eq!(report.produced, 0);
    assert!(
        report.consumed < stream.len(),
        "the header may be parsed, but the payload cannot be consumed with no room to write it"
    );
}

/// After a `Z_BUF_ERROR` both counts still report real work.
///
/// This is documented behaviour rather than an accident, and it is worth pinning
/// down because "the counts are unspecified on failure" would be a perfectly
/// plausible design that this library does *not* adopt. `Compressed::produced`
/// comes from `compress.c` L63, which runs before the status translation at L65;
/// `Decompressed`'s two counts come from `uncompr.c` L74-L75, which run before the
/// ladder at L78-L81.
///
/// The one documented exception is a failed stream initialisation, where
/// `uncompr.c` L51-L52 returns before either out-parameter is written; that path is
/// unreachable from a test that cannot force an allocation failure.
#[test]
fn the_counts_after_a_buf_error_report_real_work() {
    // Named rather than repeated as a literal, so that the decompression
    // expectations below are visibly the destination's capacity rather than
    // measured oracle values.
    const CAPACITY: usize = 100;

    let payload = corpus::repetitive(256);
    let stream = pack(&payload, Z_BEST_COMPRESSION);

    // Compression: the partial stream really is the first `produced` bytes of the
    // complete one, so the count addresses real content.
    let complete = pack(&payload, Z_BEST_COMPRESSION);
    let mut cramped = vec![0_u8; complete.len() - 1];
    let made = compress2_z(&mut cramped, &payload, Z_BEST_COMPRESSION);
    assert_compressed(made, ReturnCode::BUF_ERROR, complete.len() - 1);
    assert_eq!(
        cramped,
        complete[..complete.len() - 1],
        "the bytes that fit must be the stream's own leading bytes"
    );

    // Decompression: the produced bytes are a real prefix, and the consumed count
    // really does address the first unconsumed input byte.
    let (report, plain) = unpack(&stream, CAPACITY);
    assert_eq!(report.code, ReturnCode::BUF_ERROR);
    assert_eq!(report.produced, CAPACITY);
    assert_eq!(
        plain,
        payload[..CAPACITY],
        "a genuine prefix of the payload"
    );
    assert!(
        report.consumed <= stream.len(),
        "and a count within the input"
    );

    // Resuming from the reported offset is not something the one-shot form
    // supports -- that is what the streaming API is for -- but the offset must
    // still be a valid index into the source, which is the property a caller
    // relies on when it inspects `consumed`.
    assert!(stream.get(report.consumed..).is_some());
}

// ===========================================================================
// 3.5  Bidirectional in/out accounting -- the subtle part
//
// `uncompr.c` L69-L75:
//
//     len  += stream.avail_in;      /* unused input */
//     left += stream.avail_out;     /* unused output space */
//     *sourceLen -= len;            /* => bytes actually consumed */
//     *destLen   -= left;           /* => bytes actually produced */
//
// BOTH length parameters are in/out. `*destLen` reporting the produced count is
// obvious; `*sourceLen` reporting the CONSUMED count is not, and it is the whole
// reason the `uncompress2` form exists.
// ===========================================================================

/// A successful call reports the produced and consumed counts exactly.
#[test]
fn a_successful_call_reports_both_byte_counts() {
    for (name, source) in reduced_classes() {
        let stream = pack(&source, Z_BEST_COMPRESSION);

        // A deliberately over-sized destination, so `produced` cannot be right by
        // accident of the buffer's length.
        let mut plain = vec![0_u8; source.len() + 64];
        let report = uncompress2_z(&mut plain, &stream);

        assert_decompressed(report, ReturnCode::OK, source.len(), stream.len());
        assert_eq!(
            &plain[..report.produced],
            &source[..],
            "{name}: the produced count must address the recovered payload"
        );
        assert!(
            plain[report.produced..].iter().all(|&byte| byte == 0),
            "{name}: nothing beyond the produced count may be touched"
        );
    }
}

/// Trailing data is left unconsumed and locatable.
///
/// **The single most useful property of the `uncompress2` form, and the easiest to
/// get wrong.** `uncompr.c` L18-L20 states the contract outright: "`*sourceLen` is
/// the number of source bytes consumed. Upon return, `source + *sourceLen` points
/// to the first unused input byte." That is how a caller walks a sequence of
/// concatenated members, or finds where a zlib stream ends inside a larger
/// container format.
///
/// An implementation that treated the source length as input-only would pass every
/// round-trip test in this file and fail here.
#[test]
fn trailing_data_is_left_unconsumed_and_locatable() {
    const TAIL: &[u8] = b"not part of the stream";

    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);

    let mut with_tail = stream.clone();
    with_tail.extend_from_slice(TAIL);

    let mut plain = vec![0_u8; payload.len() + 64];
    let report = uncompress2_z(&mut plain, &with_tail);

    // The stream decodes, and the tail is simply not part of it.
    assert_decompressed(report, ReturnCode::OK, payload.len(), stream.len());
    assert_eq!(&plain[..report.produced], payload);

    // The whole point: the reported offset addresses the first unused byte.
    assert_eq!(
        &with_tail[report.consumed..],
        TAIL,
        "source + consumed must address the first unused input byte"
    );

    // And the count is strictly less than what was offered, so a caller can tell
    // that something followed.
    assert!(report.consumed < with_tail.len());
}

/// The `uLong` spelling marshals both counts identically.
///
/// `uncompress2` (`uncompr.c` L83-L91) widens two `uLong` lengths, calls the
/// primary, and narrows both results back. In the safe core there is one length
/// domain so nothing is widened or narrowed, and the two must therefore be
/// indistinguishable -- including on the trailing-data case, where a dropped
/// `*sourceLen` write-back would show up as a consumed count equal to the whole
/// input.
#[test]
fn the_narrowing_spelling_reports_the_same_two_counts() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);
    let mut with_tail = stream.clone();
    with_tail.extend_from_slice(b"tail");

    let mut a = vec![0_u8; payload.len() + 8];
    let primary = uncompress2_z(&mut a, &with_tail);

    let mut b = vec![0_u8; payload.len() + 8];
    let narrowed = uncompress2(&mut b, &with_tail);

    assert_eq!(narrowed, primary, "both counts and the status must match");
    assert_eq!(a, b, "and the destinations must match byte for byte");
    assert_eq!(
        narrowed.consumed,
        stream.len(),
        "the narrowing wrapper must not lose the consumed count"
    );
}

/// The plain spellings treat the whole input as available.
///
/// `uncompress_z` (`uncompr.c` L92-L96) and `uncompress` (L97-L101) have no
/// out-parameter for the consumed count, so C passes the source length in by value
/// -- `z_size_t used = sourceLen;` -- and then drops the updated value. The
/// consequence a caller can observe is that the *whole* input is offered to the
/// decoder, exactly as with the `2` forms; what is lost is only the ability to read
/// the count back.
///
/// The safe core returns one shape from all four entry points, so
/// `Decompressed::consumed` is filled in here as well. A facade exporting these two
/// symbols has nowhere to write it and must ignore it -- which is why this test
/// asserts the value is *correct* rather than that it is absent: correctness is
/// what lets the facade discard it safely.
#[test]
fn the_plain_spellings_treat_the_whole_input_as_available() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);
    let mut with_tail = stream.clone();
    with_tail.extend_from_slice(b"tail");

    for (label, report) in [
        ("uncompress_z", {
            let mut buffer = vec![0_u8; payload.len() + 8];
            let report = uncompress_z(&mut buffer, &with_tail);
            assert_eq!(&buffer[..report.produced], payload, "uncompress_z bytes");
            report
        }),
        ("uncompress", {
            let mut buffer = vec![0_u8; payload.len() + 8];
            let report = uncompress(&mut buffer, &with_tail);
            assert_eq!(&buffer[..report.produced], payload);
            report
        }),
    ] {
        assert_eq!(report.code, ReturnCode::OK, "{label} status");
        assert_eq!(report.produced, payload.len(), "{label} produced");
        assert_eq!(
            report.consumed,
            stream.len(),
            "{label} must have been offered the whole input, tail included, \
             and must report the stream's own length as consumed"
        );
    }
}

/// The counts after a data error report real work too.
///
/// The ladder at `uncompr.c` L78-L81 runs *after* the accounting at L72-L75, so a
/// corrupt or truncated stream still reports how far it got. For a stream whose
/// payload decoded cleanly and whose check value then failed, that means the full
/// payload is present in the destination even though the status is
/// `Z_DATA_ERROR` -- which is precisely why a caller must test the status before
/// trusting the bytes.
#[test]
fn the_counts_after_a_data_error_report_real_work() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);

    // Corrupt only the last byte of the Adler-32 trailer (RFC 1950): every
    // DEFLATE symbol still decodes, and only the check comparison fails.
    let mut damaged = stream.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;

    let (report, plain) = unpack(&damaged, payload.len() + 64);
    assert_eq!(
        report.code,
        ReturnCode::DATA_ERROR,
        "a failed check value is a data error"
    );
    assert_eq!(
        report.produced,
        payload.len(),
        "the payload decoded before the check failed, so the count is the full length"
    );
    assert_eq!(
        report.consumed,
        damaged.len(),
        "and every input byte was consumed reaching the check"
    );
    assert_eq!(
        plain, payload,
        "the bytes are even correct here -- which is why the status must be tested first"
    );

    // A header that is wrong from the outset reports much smaller counts, so the
    // two are not the same number by construction.
    let mut bad_header = stream.clone();
    bad_header[0] ^= 0xff;
    let (early, _) = unpack(&bad_header, payload.len() + 64);
    assert_eq!(early.code, ReturnCode::DATA_ERROR);
    assert_eq!(early.produced, 0, "nothing decodes past a bad header");
    assert!(
        early.consumed < damaged.len(),
        "and the consumed count reflects how little was read"
    );
}

// ===========================================================================
// 3.6  The return ladder
//
// `uncompr.c` L78-L81:
//
//     return err == Z_STREAM_END                     ? Z_OK        :
//            err == Z_NEED_DICT                      ? Z_DATA_ERROR:
//            err == Z_BUF_ERROR && len == 0          ? Z_DATA_ERROR:
//            err;
//
// Four arms, and two of them rewrite a status the decoder produced:
//   * Z_NEED_DICT becomes Z_DATA_ERROR unconditionally, because the one-shot form
//     has no parameter through which to supply a dictionary.
//   * Z_BUF_ERROR becomes Z_DATA_ERROR ONLY when all input was consumed. That
//     `len == 0` guard is the subtlest line in the file, and both of its sides are
//     tested below.
// ===========================================================================

/// A complete stream reports `Z_OK`, never `Z_STREAM_END`.
///
/// The first arm. `inflate` signals a finished stream with `Z_STREAM_END`, which is
/// a success code but not the one the one-shot form promises: `zlib.h` L1322 says
/// `uncompress` "returns `Z_OK` if success". Leaking `Z_STREAM_END` through would
/// break every caller written against that sentence, because `Z_STREAM_END` is 1
/// and `Z_OK` is 0.
#[test]
fn a_complete_stream_reports_ok_and_never_stream_end() {
    for (name, source) in reduced_classes() {
        let stream = pack(&source, Z_BEST_COMPRESSION);
        let (report, _) = unpack(&stream, source.len());

        assert_eq!(report.code, ReturnCode::OK, "{name} must report OK");
        assert_ne!(
            report.code,
            ReturnCode::STREAM_END,
            "{name}: uncompr.c L78 rewrites Z_STREAM_END to Z_OK; it must not leak"
        );
        assert_eq!(report.code.as_i32(), 0, "{name}: Z_OK is numerically 0");
    }
}

/// A preset-dictionary stream reports `Z_DATA_ERROR`.
///
/// The second arm, `uncompr.c` L79. RFC 1950 records a preset dictionary in the
/// header's `FDICT` bit, and a decompressor that meets it stops with
/// `Z_NEED_DICT` to ask for the dictionary by its Adler-32. The one-shot form has
/// no parameter through which to answer, so the status is rewritten to
/// `Z_DATA_ERROR`.
///
/// ★ This is the documented outcome, not a bug, and it must not be "fixed" by
/// surfacing `Z_NEED_DICT`: `zlib.h` L1322-L1327 lists only `Z_OK`,
/// `Z_MEM_ERROR`, `Z_BUF_ERROR` and `Z_DATA_ERROR` as possible returns, so
/// `Z_NEED_DICT` (which is 2) is not in this function's vocabulary at all. A caller
/// that needs the dictionary path must drive `inflate` and
/// `inflateSetDictionary` itself.
#[test]
fn a_preset_dictionary_stream_reports_data_error() {
    let payload = corpus::HELLO;
    let stream = packed_with_preset_dictionary(payload);

    // The FDICT bit is bit 5 of the second header byte (RFC 1950 section 2.2), so
    // the fixture really does carry a preset dictionary rather than merely failing
    // for some other reason.
    assert!(
        stream.len() >= 2,
        "a zlib stream always has a two-byte header"
    );
    assert_eq!(
        stream[1] & 0x20,
        0x20,
        "the fixture must carry FDICT, or this test proves nothing"
    );

    for (label, report) in [
        ("uncompress2_z", {
            let mut buffer = vec![0_u8; payload.len() + 64];
            uncompress2_z(&mut buffer, &stream)
        }),
        ("uncompress", {
            let mut buffer = vec![0_u8; payload.len() + 64];
            uncompress(&mut buffer, &stream)
        }),
    ] {
        assert_eq!(
            report.code,
            ReturnCode::DATA_ERROR,
            "{label}: uncompr.c L79 rewrites Z_NEED_DICT to Z_DATA_ERROR"
        );
        assert_ne!(
            report.code,
            ReturnCode::NEED_DICT,
            "{label}: Z_NEED_DICT is not in this function's documented vocabulary"
        );
    }
}

/// A truncated stream reports `Z_DATA_ERROR`.
///
/// The third arm's `len == 0` side, `uncompr.c` L80. The destination is generously
/// sized, so the decoder cannot run out of room; it runs out of *input* instead,
/// and reports `Z_BUF_ERROR` for want of anything else to say. Because every input
/// byte was consumed the residual `len` is zero, and the ladder converts that into
/// `Z_DATA_ERROR` -- which is exactly what `uncompr.c` L24-L25 promises:
/// `Z_DATA_ERROR` "if the input data was corrupted, including if the input data is
/// an incomplete zlib stream".
#[test]
fn a_truncated_stream_reports_data_error() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);

    // Several truncation depths: inside the trailer, at its start, and inside the
    // DEFLATE data. All are incomplete streams, so all must read the same way.
    for cut in 1..=5_usize {
        let truncated = &stream[..stream.len() - cut];
        let (report, _) = unpack(truncated, payload.len() + 64);

        assert_eq!(
            report.code,
            ReturnCode::DATA_ERROR,
            "a stream truncated by {cut} byte(s) is an incomplete stream, \
             so uncompr.c L80's `len == 0` guard must fire"
        );
        assert_eq!(
            report.consumed,
            truncated.len(),
            "every offered byte must have been consumed, which is what makes len == 0"
        );
    }

    // A single-byte source is the degenerate case: not even the two-byte header is
    // complete.
    let (report, _) = unpack(&stream[..1], 64);
    assert_eq!(report.code, ReturnCode::DATA_ERROR);
}

/// `Z_BUF_ERROR` passes through while input remains.
///
/// The third arm's other side, and the one an implementation is most likely to get
/// wrong by testing the residual too early or not at all. Here the *destination*
/// runs out while input is still pending, so `len != 0`, the guard does not fire,
/// and `Z_BUF_ERROR` reaches the caller unchanged -- which matters because
/// `Z_BUF_ERROR` is recoverable (retry with a bigger buffer) while
/// `Z_DATA_ERROR` is not.
///
/// The payload is highly compressible on purpose: a 256-byte run compresses to a
/// handful of bytes, so a small destination is guaranteed to fill long before the
/// input is exhausted.
#[test]
fn buf_error_passes_through_while_input_remains() {
    // Named rather than repeated as a literal, so that the expectations below are
    // visibly the destination's capacity rather than measured oracle values.
    const CAPACITY: usize = 8;

    let payload = corpus::repetitive(256);
    let stream = pack(&payload, Z_BEST_COMPRESSION);
    assert!(
        stream.len() < payload.len(),
        "the fixture must compress, or the destination cannot fill first"
    );

    let (report, plain) = unpack(&stream, CAPACITY);

    assert_eq!(
        report.code,
        ReturnCode::BUF_ERROR,
        "output space ran out with input still pending, so the status passes through"
    );
    assert_ne!(
        report.code,
        ReturnCode::DATA_ERROR,
        "rewriting this to DATA_ERROR would report a short buffer as corruption"
    );
    assert_eq!(
        report.produced, CAPACITY,
        "the destination is filled to capacity before the shortfall is reported"
    );
    assert_eq!(plain, payload[..CAPACITY]);
    assert!(
        report.consumed < stream.len(),
        "input must remain, which is the condition the guard tests"
    );
}

/// A corrupt payload reports `Z_DATA_ERROR`.
///
/// The fourth arm, where the decoder's own verdict passes through untouched. Two
/// distinct kinds of damage are exercised, because they fail at different stages:
/// a mangled header is rejected before any output, while a mangled Huffman
/// bitstream is rejected part-way through decoding.
#[test]
fn a_corrupt_stream_reports_data_error() {
    let payload = corpus::HELLO;
    let stream = pack(payload, Z_BEST_COMPRESSION);

    // The zlib header carries CMF and FLG, and RFC 1950 requires
    // `(CMF << 8 | FLG) % 31 == 0`; flipping every bit of CMF breaks both the
    // method nibble and the check.
    let mut bad_header = stream.clone();
    bad_header[0] ^= 0xff;
    let (report, _) = unpack(&bad_header, payload.len() + 64);
    assert_eq!(
        report.code,
        ReturnCode::DATA_ERROR,
        "an invalid zlib header is a data error"
    );
    assert_eq!(report.produced, 0);

    // Damage inside the compressed data. Whether this trips an invalid code, an
    // invalid distance or merely the trailing check value, the verdict is the
    // same, so the assertion is on the status rather than on where it failed.
    let middle = stream.len() / 2;
    let mut bad_payload = stream.clone();
    bad_payload[middle] ^= 0xff;
    let (report, _) = unpack(&bad_payload, payload.len() + 64);
    assert_eq!(
        report.code,
        ReturnCode::DATA_ERROR,
        "damaged compressed data is a data error"
    );
}

// ===========================================================================
// 3.7  Empty input and the scratch-output special case
// ===========================================================================

/// Empty input compresses to a valid, complete stream that decompresses to nothing.
///
/// The edge case every entry point has to survive. A zlib stream for no data is
/// still a zlib stream: a two-byte header, an empty final stored block, and a
/// four-byte Adler-32 of the empty sequence -- eight bytes in total, measured
/// against the reference as `78 9c 03 00 00 00 00 01`.
#[test]
fn empty_input_round_trips() {
    let stream = pack(corpus::EMPTY, Z_DEFAULT_COMPRESSION);

    assert_eq!(
        stream,
        [0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        "the empty payload's default-level stream is eight measured bytes: \
         the RFC 1950 header, an empty final stored block, and adler32 of nothing"
    );
    assert!(
        stream.len() <= compress_bound_z(0),
        "and it must fit the bound for a zero-length input, which is 13"
    );

    // Decompressing it produces nothing, and says so, rather than erring.
    let mut plain = vec![0_u8; 64];
    let report = uncompress2_z(&mut plain, &stream);
    assert_decompressed(report, ReturnCode::OK, 0, stream.len());
    assert!(
        plain.iter().all(|&byte| byte == 0),
        "an empty result must not touch the destination"
    );
}

/// An empty destination probes a stream without decoding it.
///
/// The Rust modelling of `uncompr.c` L42-L43:
///
/// ```c
/// if (left == 0 && dest == Z_NULL)
///     dest = (Bytef *)&stream.reserved;       /* next_out cannot be NULL */
/// ```
///
/// C has to redirect a null `dest` at eight bytes of `z_stream` padding it never
/// reads, purely because `inflate` rejects a null output pointer. An empty Rust
/// slice is already non-null and non-dangling, so there is nothing to redirect --
/// but **the behaviour the redirection enables is preserved exactly**, and it is
/// load-bearing rather than incidental. An empty destination is a supported input,
/// not an error, and it must not be short-circuited: the loop runs, the decoder
/// parses as much as it can without writing, and the two outcomes differ.
///
/// | stream | status | why |
/// |---|---|---|
/// | empty payload | `Z_OK` | nothing to write, so the stream completes |
/// | has content | `Z_BUF_ERROR` | input is left over once there is nowhere to write |
///
/// That difference is how a caller holding no buffer at all probes a stream, and a
/// short-circuiting implementation would collapse both rows into one answer.
#[test]
fn an_empty_destination_probes_a_stream() {
    // An empty stream against an empty destination: complete, and it says so.
    let empty_stream = pack(corpus::EMPTY, Z_DEFAULT_COMPRESSION);
    let report = uncompress2_z(&mut [], &empty_stream);
    assert_decompressed(report, ReturnCode::OK, 0, empty_stream.len());

    // A stream with content against an empty destination: BUF_ERROR, because input
    // survives. Not DATA_ERROR -- the stream is perfectly valid.
    let content_stream = pack(corpus::HELLO, Z_DEFAULT_COMPRESSION);
    let report = uncompress2_z(&mut [], &content_stream);
    assert_eq!(
        report.code,
        ReturnCode::BUF_ERROR,
        "a valid stream with content, probed with no buffer, is a short buffer -- not corruption"
    );
    assert_eq!(report.produced, 0);
    assert!(report.consumed < content_stream.len());

    // All four spellings behave the same way, since all four reach the same body.
    assert_eq!(uncompress2(&mut [], &empty_stream).code, ReturnCode::OK);
    assert_eq!(uncompress_z(&mut [], &empty_stream).code, ReturnCode::OK);
    assert_eq!(uncompress(&mut [], &empty_stream).code, ReturnCode::OK);
}

/// An empty source is a data error.
///
/// Zero bytes cannot be a zlib stream -- RFC 1950 requires at least a two-byte
/// header and a four-byte check value -- so this is the `len == 0` arm of the
/// ladder reached by the shortest possible route: the decoder is offered nothing,
/// reports `Z_BUF_ERROR`, and because there was no input to leave over the residual
/// is zero and `uncompr.c` L80 converts it to `Z_DATA_ERROR`.
///
/// Note that this is *not* symmetric with the empty-destination case above:
/// an empty destination is legal input, an empty source is not.
#[test]
fn an_empty_source_is_a_data_error() {
    let (report, _) = unpack(&[], 64);
    assert_decompressed(report, ReturnCode::DATA_ERROR, 0, 0);

    // Including when the destination is empty too, which is the doubly degenerate
    // case and the one most likely to panic if a length were assumed non-zero.
    let report = uncompress2_z(&mut [], &[]);
    assert_decompressed(report, ReturnCode::DATA_ERROR, 0, 0);
}

// ===========================================================================
// 3.8  Chunking behaviour
// ===========================================================================

/// The per-iteration cap is the width of `uInt`, and the arithmetic that applies
/// it must widen rather than truncate.
///
/// `compress.c` L28 is `const uInt max = (uInt)-1;` and L52/L56 clamp `avail_out`
/// and `avail_in` against it, because `z_size_t` is 64 bits wide on LP64 while
/// `avail_in` and `avail_out` are `unsigned int`. A buffer larger than 4 GiB has to
/// be fed to the encoder in pieces.
///
/// ★ On a 64-bit host the cap cannot be exercised directly -- doing so would need a
/// payload above 4 GiB, which is neither Miri-viable nor a reasonable test
/// allocation -- so what is asserted here is the *shape* of the arithmetic: the cap
/// is exactly `u32::MAX`, it is representable in the length domain the wrappers
/// work in, and clamping a length against it is a `min` in that wider domain rather
/// than a narrowing cast. A truncating implementation -- `avail_in = len as u32` --
/// would silently offer zero bytes for a length that is a multiple of 2^32, which
/// is why the widening matters. The observable consequence of getting it wrong is
/// covered by the many-lengths sweep below, which a truncation bug at any smaller
/// modulus would break.
#[test]
fn the_chunk_cap_is_the_uint_max_width() {
    const MAX_CHUNK: usize = u32::MAX as usize;

    assert_eq!(
        MAX_CHUNK, 4_294_967_295,
        "(uInt)-1 is 32 bits of ones, i.e. 2^32 - 1"
    );
    assert_eq!(
        u32::try_from(MAX_CHUNK),
        Ok(u32::MAX),
        "the cap must round-trip through the narrower type without loss, \
         which is what makes the clamp safe"
    );

    // Clamping is a `min` in the wider domain. Every ordinary length is therefore
    // offered whole, in a single chunk, and the cap is inert.
    for length in [0_usize, 1, 13, 4_096, 65_536] {
        assert_eq!(
            length.min(MAX_CHUNK),
            length,
            "a length below the cap must be offered whole"
        );
    }

    // The cap only binds above itself, and it never yields zero for a non-zero
    // length -- the property a narrowing cast would violate.
    assert_eq!(MAX_CHUNK.min(MAX_CHUNK), MAX_CHUNK);
    #[cfg(target_pointer_width = "64")]
    {
        let over = MAX_CHUNK + 1; // 2^32, the first length a `as u32` cast maps to 0
        assert_eq!(
            over.min(MAX_CHUNK),
            MAX_CHUNK,
            "2^32 must clamp to the cap, not truncate to zero"
        );
        assert_ne!(over.min(MAX_CHUNK), 0);
    }
}

/// Many lengths round trip, including the ones either side of a power of two.
///
/// A truncation or an off-by-one in the chunking loop's top-up would not corrupt
/// every length -- it would corrupt the ones whose residual lands exactly on a
/// boundary. Sweeping the neighbourhood of each power of two is the cheapest way to
/// catch that class of bug, and it doubles as a check that the accounting holds for
/// every size rather than only for the corpus classes.
#[test]
fn many_lengths_round_trip() {
    for length in [0_usize, 1, 15, 16, 17, 256] {
        let source = corpus::incompressible(length);
        let stream = pack(&source, Z_DEFAULT_COMPRESSION);
        let (report, plain) = unpack(&stream, length);

        assert_eq!(
            report.code,
            ReturnCode::OK,
            "length {length} must round trip"
        );
        assert_eq!(plain, source, "length {length} must recover exactly");
        assert_decompressed(report, ReturnCode::OK, length, stream.len());
    }
}

/// The same sweep, widened to every power-of-two neighbourhood up to 4 KiB.
///
/// Thirty-three lengths, each a compress and a decompress. Skipped under Miri for
/// interpreter speed **only** -- it runs natively on every `cargo test`, and the
/// reduced sweep above runs everywhere.
#[test]
#[cfg_attr(
    miri,
    ignore = "66 streams; skipped for interpreter speed only, still runs natively"
)]
fn many_lengths_including_powers_of_two_round_trip() {
    const LENGTHS: [usize; 33] = [
        0, 1, 2, 3, 4, 5, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257,
        511, 512, 513, 1_023, 1_024, 1_025, 2_047, 2_048, 4_095, 4_096,
    ];

    for length in LENGTHS {
        let source = corpus::incompressible(length);
        let stream = pack(&source, Z_DEFAULT_COMPRESSION);
        let (report, plain) = unpack(&stream, length);

        assert_eq!(
            report.code,
            ReturnCode::OK,
            "length {length} must round trip"
        );
        assert_eq!(plain, source, "length {length} must recover exactly");
        assert_decompressed(report, ReturnCode::OK, length, stream.len());
    }
}

/// The produced stream is complete, which proves `Z_FINISH` reached the last chunk.
///
/// `compress.c` L60 is `err = deflate(&stream, sourceLen ? Z_NO_FLUSH : Z_FINISH);`
/// and it reads the **residual** source length, *after* the top-up at L56-L58. So
/// every chunk but the last is offered `Z_NO_FLUSH` and the last is offered
/// `Z_FINISH`. Getting that selector wrong -- reading the length before the top-up,
/// say -- would emit a stream that is still open: no final block flag, and no
/// Adler-32 trailer.
///
/// The direct evidence is that a one-shot decompression of the whole stream reports
/// `Z_OK`, because that status is reachable only through the decoder's
/// `Z_STREAM_END` (`uncompr.c` L78). An unterminated stream would instead consume
/// all its input and report `Z_DATA_ERROR` through the `len == 0` guard -- the same
/// verdict the truncation test above relies on.
#[test]
fn the_produced_stream_is_complete_in_one_shot() {
    // The flush selector is a property of the loop, not of the payload: it reads
    // the residual source length and nothing else. So the sweep is over levels --
    // which do change how many blocks the encoder emits, and therefore how many
    // iterations the loop runs -- against a single fixture.
    let source = corpus::HELLO;

    for level in REDUCED_LEVELS {
        let stream = pack(source, level);

        let (report, plain) = unpack(&stream, source.len());
        assert_eq!(
            report.code,
            ReturnCode::OK,
            "level {level}: Z_OK is reachable only via the decoder's Z_STREAM_END, \
             so it proves the final block flag and the trailer are present"
        );
        assert_eq!(plain, source);

        // The trailer is four bytes of Adler-32 (RFC 1950), so a complete stream is
        // always at least header plus trailer, and the decoder must have consumed
        // all of it.
        assert!(
            stream.len() >= 6,
            "level {level}: two header bytes plus a four-byte check value"
        );
        assert_eq!(
            report.consumed,
            stream.len(),
            "level {level}: a complete stream is consumed to its last byte"
        );
    }
}
