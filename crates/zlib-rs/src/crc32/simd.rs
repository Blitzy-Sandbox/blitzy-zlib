//! Portable high-throughput CRC-32 backend.
//!
//! This module is the optional, output-neutral checksum optimization enabled by the
//! `simd` feature. It reformulates the braided section of `crc32_z` (`crc32.c`
//! L637-L920); it does not define a different checksum.
//!
//! # Why the implementation is portable integer code
//!
//! The crate-wide prohibition on unchecked operations rules out the standard
//! architecture intrinsics, and stable Rust at the MSRV of 1.80 has no stable portable
//! vector API. The useful option left is fixed-width, independent integer work that LLVM
//! can autovectorize. The workspace release profile helps that shape along with `lto = "fat"`,
//! `codegen-units = 1` and `opt-level = 3` -- and **only** those. `-C target-cpu` is *not* set
//! anywhere in this repository: there is no `.cargo/config.toml`, and neither `Cargo.toml` nor
//! `rust-toolchain.toml` sets it, so builds target the default baseline for the triple unless the
//! person building passes it themselves. Do not write code here that depends on a raised baseline.
//! Table loads also remain scalar because portable Rust has no gather operation; the gain comes
//! from independent accumulators and from shortening the serial final combine and byte tail.
//!
//! No run-time target-feature probe is performed here -- **none at all**, in contrast to
//! `adler32/simd.rs`, which exposes an `is_supported()` its parent consults purely as a
//! throughput hint. Every operation below is ordinary portable integer arithmetic, so the code
//! remains valid when a machine has no vector unit whatsoever. A `simd`-enabled binary therefore
//! meets the runtime-compatibility requirement structurally rather than by probing: there is no
//! target-specific instruction that can be absent.
//!
//! # Output neutrality
//!
//! CRC-32 is one scalar in the field `GF(2)` regardless of the order used to evaluate its
//! linear recurrence. Independent braid residues can therefore be XOR-combined exactly.
//! This backend may change throughput and must never change a checksum or any emitted byte.
//! That is why checksum parallelism is allowed even though parallel match finding is not.
//!
//! # How this backend is actually selected
//!
//! **The Cargo feature `simd` on `crates/zlib-rs` is the only thing that compiles this module
//! in** -- that is what the `#![cfg(feature = "simd")]` below says, and there is no other path.
//!
//! `ZLIB_RS_SIMD=0/1` is a related but *separate* mechanism, and it does not switch this feature
//! on. It is read by `crates/libz-rs-sys/build.rs`, which asks Cargo to rerun when the value
//! changes and then publishes the request as `cfg(zlib_rs_simd)` for the facade crate --
//! because **a build script cannot enable a Cargo feature at all**, in this crate or any other.
//! That `cfg` is not consulted anywhere under `crates/zlib-rs/src/`. So: to get this backend, pass
//! `--features simd`; setting `ZLIB_RS_SIMD=1` on its own will not produce it.
//!
//! The S390X path in `contrib/crc32vx/` and the `ARMCRC32` assembly path at `crc32.c` L498-L599
//! are outside this port's scope. Omitting either can cost throughput only, never correctness.
//!
//! # Throughput claims
//!
//! The design intent is that this backend beats `braid.rs` above its delegation threshold. That
//! is intent, not a result: no measurement is committed in this repository, and a claim about
//! throughput belongs to a run rather than to a comment. `benches/checksum_bench.rs` is where the
//! comparison belongs, and it compares the scalar and vectorised backends against each other and
//! against the C oracle. If this turns out not to win, the honest resolution is to simplify or
//! remove this optimization, never to weaken bit-for-bit equality.

// There is deliberately no `#![cfg(feature = "simd")]` here.  `crc32/mod.rs` already declares this
// module as `#[cfg(feature = "simd")] mod simd;`, so restating the condition inside the file
// applies the same attribute twice -- `rustc`'s `duplicated_attributes` lint says so, and under
// `-D warnings` on the 1.80 floor that is an error rather than a note.  One gate, at the module
// declaration, is also where the sibling `adler32/simd.rs` keeps it.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use core::mem::size_of;

use super::braid::crc32_braid;
use super::generic::crc32_generic;
use super::tables::{Word, CRC_BRAID_TABLE, CRC_TABLE, N, POLY, W, X2N_TABLE};
use super::Crc32Backend;

/// Number of entries in each byte-indexed CRC table row.
const TABLE_LEN: usize = 256;

/// Independent residue streams encoded by the shipped `crc32.h` tables.
const STREAMS: usize = N;

/// Bytes consumed by one complete braided iteration.
const CHUNK: usize = STREAMS * W;

/// Threshold used by the ordinary braided backend (`crc32.c` L640).
#[cfg(test)]
const BRAID_MIN_LEN: usize = CHUNK + W - 1;

/// Smallest input for which the shortened final combine and word tail always pay off.
///
/// The alignment prefix consumes at most `W - 1` bytes. Starting with
/// `CHUNK + 2 * W - 1` therefore leaves one full braided chunk and at least one full
/// `W`-byte tail word. Shorter inputs retain the already tuned `crc32_braid` path.
const MIN_SIMD_LEN: usize = CHUNK + 2 * W - 1;

/// Polynomial representation of one in zlib's reflected modular arithmetic.
const X_POW_0: u32 = 1 << 31;

/// Multiply two reflected CRC polynomials modulo [`POLY`].
///
/// This is the const-evaluated form of `multmodp()` at `crc32.c` L157-L176. It runs only
/// while the compiler builds [`STRIDE_W_TABLE`].
const fn multmodp(a: u32, mut b: u32) -> u32 {
    let mut mask = X_POW_0;
    let mut product = 0;

    while mask != 0 {
        if a & mask != 0 {
            product ^= b;
            if a & (mask - 1) == 0 {
                break;
            }
        }
        mask >>= 1;
        b = if b & 1 == 0 { b >> 1 } else { (b >> 1) ^ POLY };
    }

    product
}

/// Return `x^(n * 2^k)` modulo [`POLY`], in zlib's reflected representation.
///
/// The table subscript is bounded by `k & 31`. Const evaluation turns any bad subscript
/// into a compile error, which is why the narrow indexing allowance is preferable to a
/// run-time fallback in this compiler-only code.
#[allow(clippy::cast_possible_truncation, clippy::indexing_slicing)]
const fn x2nmodp(mut n: u64, mut k: u32) -> u32 {
    let mut product = X_POW_0;

    while n != 0 {
        if n & 1 != 0 {
            let index = (k & 31) as usize;
            product = multmodp(X2N_TABLE[index], product);
        }
        n >>= 1;
        k += 1;
    }

    product
}

/// Generate the little-endian braid table for a byte distance of `stride`.
///
/// This is a const-evaluated mirror of `braid()` at `crc32.c` L461-L473. The shipped table
/// is generated with `stride = N * W`; this module additionally generates `stride = W`,
/// which is exactly the `#if N == 1` table already published in `crc32.h`.
///
/// Both loop counters are bounded by their destination arrays. Const evaluation makes an
/// invalid subscript a compile error, and the casts are exact for `W <= 8` and
/// `TABLE_LEN == 256`.
#[allow(clippy::cast_possible_truncation, clippy::indexing_slicing)]
const fn braid_table(stride: usize) -> [[u32; TABLE_LEN]; W] {
    let mut table = [[0; TABLE_LEN]; W];
    let mut row = 0;

    while row < W {
        let exponent = ((stride + 3 - row) << 3) as u64;
        let power = x2nmodp(exponent, 0);
        let mut byte = 1;

        while byte < TABLE_LEN {
            table[row][byte] = multmodp((byte as u32) << 24, power);
            byte += 1;
        }
        row += 1;
    }

    table
}

/// Table that advances each byte lane by one machine word.
const STRIDE_W_TABLE: [[u32; TABLE_LEN]; W] = braid_table(W);

// Verify the generated tables against zlib's checked-in tables during compilation. The
// second loop also proves the high-signal identity that the final row of the stride-`W`
// table is `CRC_TABLE`: that row advances a byte by exactly 32 polynomial bits.
#[allow(clippy::indexing_slicing)]
const _: () = {
    let generated_braid = braid_table(CHUNK);
    let generated_stride = braid_table(W);
    let mut row = 0;

    while row < W {
        let mut byte = 0;
        while byte < TABLE_LEN {
            assert!(generated_braid[row][byte] == CRC_BRAID_TABLE[row][byte]);
            byte += 1;
        }
        row += 1;
    }

    let mut byte = 0;
    while byte < TABLE_LEN {
        assert!(generated_stride[W - 1][byte] == CRC_TABLE[byte]);
        byte += 1;
    }
};

const _: () = assert!(N == 5);
const _: () = assert!(STREAMS == N);
const _: () = assert!(W == size_of::<Word>());
const _: () = assert!(CHUNK == N * W);
const _: () = assert!(CHUNK + W - 1 < MIN_SIMD_LEN);

/// Widen a CRC residue to the machine word used by the braid.
#[inline]
fn widen(value: u32) -> Word {
    Word::from(value)
}

/// Advance one CRC residue across one little-endian machine word.
///
/// The table rows are independent, so the `W` loads expose instruction-level parallelism.
/// Each index comes from a byte and is therefore in `0..TABLE_LEN`; using `get` preserves
/// the module's no-panic contract while still letting the optimizer remove the range check.
#[inline]
fn table_step(table: &[[u32; TABLE_LEN]; W], crc: u32, word: Word) -> u32 {
    let bytes = (widen(crc) ^ word).to_le_bytes();
    let mut next = 0;

    for (row, &byte) in table.iter().zip(bytes.iter()) {
        if let Some(&entry) = row.get(usize::from(byte)) {
            next ^= entry;
        }
    }

    next
}

/// Fold an exact, non-empty sequence of braided chunks.
///
/// The first `body.len() - CHUNK` bytes update `STREAMS` independent residues through the
/// shipped `N == 5` table. The final chunk combines those residues in stream order. CRC-32
/// is linear over `GF(2)`, so applying the stride-`W` transform to
/// `braid ^ word ^ folded` is identical to zlib's serial `crc_word` transform: both advance
/// the same polynomial by one machine word before combining the next stream with XOR.
#[inline]
fn fold_braided_body(crc: u32, body: &[u8]) -> Option<u32> {
    if body.len() < CHUNK || body.len() % CHUNK != 0 {
        return None;
    }

    let sparse_len = body.len() - CHUNK;
    let sparse = body.get(..sparse_len)?;
    let last = body.get(sparse_len..)?;
    let mut braids = [0; STREAMS];
    let first = braids.first_mut()?;
    *first = crc;

    for chunk in sparse.chunks_exact(CHUNK) {
        for (braid, bytes) in braids.iter_mut().zip(chunk.chunks_exact(W)) {
            let Ok(array) = <[u8; W]>::try_from(bytes) else {
                return None;
            };
            *braid = table_step(&CRC_BRAID_TABLE, *braid, Word::from_le_bytes(array));
        }
    }

    let mut folded = 0;
    for (braid, bytes) in braids.into_iter().zip(last.chunks_exact(W)) {
        let Ok(array) = <[u8; W]>::try_from(bytes) else {
            return None;
        };
        folded = table_step(
            &STRIDE_W_TABLE,
            0,
            widen(braid) ^ Word::from_le_bytes(array) ^ widen(folded),
        );
    }

    Some(folded)
}

/// Fold all complete machine words in `tail`, returning the shorter byte residue.
///
/// The stride-`W` table reduces the serial tail from at most `N * W - 1` byte steps in
/// `braid.rs` to at most `W - 1`. The returned slice is borrowed from `tail`.
#[inline]
fn fold_tail_words(mut crc: u32, tail: &[u8]) -> Option<(u32, &[u8])> {
    let mut words = tail.chunks_exact(W);

    for bytes in &mut words {
        let Ok(array) = <[u8; W]>::try_from(bytes) else {
            return None;
        };
        crc = table_step(&STRIDE_W_TABLE, crc, Word::from_le_bytes(array));
    }

    Some((crc, words.remainder()))
}

/// Fold `buf` into the already pre-conditioned CRC-32 state `crc`.
///
/// This is an output-neutral reformulation of the braided body in `crc32_z`
/// (`crc32.c` L637-L920):
///
/// 1. Inputs shorter than [`MIN_SIMD_LEN`] use `crc32_braid`.
/// 2. A byte prefix reaches the next `W`-byte address boundary through
///    `crc32_generic`.
/// 3. Whole `N * W` chunks update the same five braid residues as `braid.rs`.
/// 4. A stride-`W` table parallelizes each final word transform and consumes further
///    complete tail words.
/// 5. Fewer than `W` remaining bytes return to `crc32_generic`.
///
/// The function uses one little-endian kernel on every target. `Word::from_le_bytes`
/// performs the needed byte reversal on a big-endian machine, avoiding a hot-loop endian
/// branch without changing the polynomial represented by a word.
///
/// The caller owns zlib's leading and trailing one's-complement operations
/// (`crc32.c` L635 and L940). This function applies neither, so sequential calls can feed
/// their returned state directly into the next call.
#[must_use]
#[inline]
pub fn crc32_simd(crc: u32, buf: &[u8]) -> u32 {
    if buf.len() < MIN_SIMD_LEN {
        return crc32_braid(crc, buf);
    }

    let initial_crc = crc;

    // `crc32.c` L646-L650. The operation may decline with `usize::MAX`; no-panic slicing
    // turns that case, or any larger conforming offset, into an exact braided retry.
    let prefix_len = buf.as_ptr().align_offset(W);
    let (Some(prefix), Some(aligned)) = (buf.get(..prefix_len), buf.get(prefix_len..)) else {
        return crc32_braid(initial_crc, buf);
    };
    let crc = crc32_generic(crc, prefix);

    // `crc32.c` L652-L655. Holding back the last chunk lets the independent residues be
    // combined with the stride-`W` transform instead of a byte-dependent word recurrence.
    let body_len = (aligned.len() / CHUNK) * CHUNK;
    let (Some(body), Some(tail)) = (aligned.get(..body_len), aligned.get(body_len..)) else {
        return crc32_braid(initial_crc, buf);
    };
    let Some(crc) = fold_braided_body(crc, body) else {
        return crc32_braid(initial_crc, buf);
    };

    let Some((crc, remainder)) = fold_tail_words(crc, tail) else {
        return crc32_braid(initial_crc, buf);
    };
    crc32_generic(crc, remainder)
}

/// The optional portable high-throughput CRC-32 backend.
///
/// A zero-sized marker around `crc32_simd`, used by the parent module's backend
/// selection and by checksum benchmarks. It takes and returns the same pre-conditioned
/// state as the generic and braided backends.
///
/// There is deliberately no support-query API. The implementation has no optional machine
/// instruction to guard, and selecting this marker can affect speed only.
#[derive(Clone, Copy, Debug, Default)]
pub struct Simd;

impl Crc32Backend for Simd {
    /// Identifies this backend in diagnostics and benchmark labels.
    const NAME: &'static str = "simd";

    /// Forward to `crc32_simd` without changing the state or buffer.
    #[inline]
    fn update(crc: u32, buf: &[u8]) -> u32 {
        crc32_simd(crc, buf)
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{size_of, size_of_val};

    use super::{
        braid_table, crc32_braid, crc32_generic, crc32_simd, fold_braided_body, Crc32Backend, Simd,
        Word, BRAID_MIN_LEN, CHUNK, CRC_BRAID_TABLE, CRC_TABLE, MIN_SIMD_LEN, N, STREAMS,
        STRIDE_W_TABLE, TABLE_LEN, W,
    };

    /// Length of the deterministic fixture used by native sweeps.
    const FIXTURE_LEN: usize = 1_024;

    /// Native builds cover the mandated inclusive `0..=512` sweep.
    #[cfg(not(miri))]
    const SWEEP_MAX_LEN: usize = 512;

    /// Miri crosses every short-input boundary while keeping interpretation time bounded.
    #[cfg(miri)]
    const SWEEP_MAX_LEN: usize = 128;

    /// Pre-conditioned states covering zero, all bits, isolated edge bytes, and mixed bits.
    const SEEDS: [u32; 5] = [
        0x0000_0000,
        0xffff_ffff,
        0x0000_00ff,
        0xff00_0000,
        0x9e37_79b9,
    ];

    /// Lengths around every dispatch boundary and several exact chunk multiples.
    const BOUNDARY_LENGTHS: [usize; 23] = [
        0,
        1,
        W - 1,
        W,
        W + 1,
        CHUNK - 1,
        CHUNK,
        CHUNK + 1,
        BRAID_MIN_LEN - 1,
        BRAID_MIN_LEN,
        BRAID_MIN_LEN + 1,
        MIN_SIMD_LEN - 1,
        MIN_SIMD_LEN,
        MIN_SIMD_LEN + 1,
        2 * CHUNK - 1,
        2 * CHUNK,
        2 * CHUNK + 1,
        3 * CHUNK - 1,
        3 * CHUNK,
        3 * CHUNK + 1,
        4 * CHUNK - 1,
        4 * CHUNK,
        4 * CHUNK + 1,
    ];

    /// Every class in the differential suite's committed minimal corpus.
    #[cfg(not(miri))]
    const CORPUS_CASES: [(&str, &[u8]); 10] = [
        (
            "empty",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/empty.bin"),
        ),
        (
            "single byte",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/single_byte.bin"),
        ),
        (
            "repetitive",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/repetitive.bin"),
        ),
        (
            "random",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/random.bin"),
        ),
        (
            "text",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/text.txt"),
        ),
        (
            "binary",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/binary.bin"),
        ),
        (
            "window boundary",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/window_boundary.bin"),
        ),
        (
            "dictionary",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/dictionary.bin"),
        ),
        (
            "gray list",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/gray_list.bin"),
        ),
        (
            "hello",
            include_bytes!("../../../zlib-rs-differential/corpus/minimal/hello.bin"),
        ),
    ];

    /// Deterministic pseudo-random bytes shared by all local sweeps.
    fn pseudo_random() -> [u8; FIXTURE_LEN] {
        let mut bytes = [0; FIXTURE_LEN];
        let mut state = 0x2b3c_4d5e_u32;

        for slot in &mut bytes {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let [_, _, high_middle, _] = state.to_le_bytes();
            *slot = high_middle;
        }

        bytes
    }

    /// Borrow a prefix and make a too-short fixture fail at its call site.
    fn prefix(bytes: &[u8], len: usize) -> &[u8] {
        let part = bytes.get(..len);
        assert!(part.is_some(), "fixture shorter than {len} bytes");
        part.map_or(&[], |slice| slice)
    }

    /// Assert direct agreement among all three pre-conditioned backends.
    fn assert_three_way(state: u32, bytes: &[u8], label: &str) {
        let simd = crc32_simd(state, bytes);
        let braid = crc32_braid(state, bytes);
        let generic = crc32_generic(state, bytes);

        assert_eq!(
            simd, braid,
            "simd versus braid: {label}, state {state:#010x}"
        );
        assert_eq!(
            braid, generic,
            "braid versus generic: {label}, state {state:#010x}"
        );
    }

    #[test]
    fn constants_and_generated_tables_match_zlib() {
        assert_eq!(N, 5, "`crc32.c` L93 default");
        assert_eq!(STREAMS, N);
        assert_eq!(W, size_of::<Word>());
        assert_eq!(CHUNK, N * W);
        assert_eq!(BRAID_MIN_LEN, CHUNK + W - 1);
        assert_eq!(MIN_SIMD_LEN, CHUNK + 2 * W - 1);
        assert_eq!(TABLE_LEN, 256);
        assert_eq!(braid_table(CHUNK), CRC_BRAID_TABLE);
        assert_eq!(STRIDE_W_TABLE[W - 1], CRC_TABLE);

        // The fixed constants stay far below every arithmetic limit used to form lengths and
        // table addresses. `checked_*` keeps this statement valid on every pointer width.
        assert_eq!(CHUNK.checked_add(2 * W - 1), Some(MIN_SIMD_LEN));
        assert_eq!(
            W.checked_mul(TABLE_LEN),
            Some(STRIDE_W_TABLE.len() * TABLE_LEN)
        );
    }

    #[test]
    fn stride_table_has_header_anchor_values() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(
                &STRIDE_W_TABLE[0][1..4],
                &[0xccaa_009e, 0x4225_077d, 0x8e8f_07e3]
            );
            assert_eq!(STRIDE_W_TABLE[7][1], 0x7707_3096);
            assert_eq!(STRIDE_W_TABLE[7][255], 0x2d02_ef8d);
        }

        #[cfg(not(target_pointer_width = "64"))]
        {
            assert_eq!(
                &STRIDE_W_TABLE[0][1..4],
                &[0xb8bc_6765, 0xaa09_c88b, 0x12b5_afee]
            );
            assert_eq!(STRIDE_W_TABLE[3][1], 0x7707_3096);
            assert_eq!(STRIDE_W_TABLE[3][255], 0x2d02_ef8d);
        }
    }

    #[test]
    fn all_lengths_through_the_sweep_match_three_ways() {
        let fixture = pseudo_random();

        for state in SEEDS {
            for len in 0..=SWEEP_MAX_LEN {
                assert_three_way(state, prefix(&fixture, len), "inclusive length sweep");
            }
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn every_committed_corpus_class_matches_three_ways() {
        for (name, bytes) in CORPUS_CASES {
            for state in SEEDS {
                assert_three_way(state, bytes, name);
            }
        }
    }

    #[test]
    fn every_word_alignment_matches_three_ways() {
        let fixture = pseudo_random();
        let length = FIXTURE_LEN - W;

        // One extra offset repeats the first residue class and also checks a fully shifted
        // window. Whatever alignment the stack gave the array, these starts cover every
        // address modulo `W`.
        for offset in 0..=W {
            let bytes = fixture.get(offset..offset + length);
            assert!(bytes.is_some(), "alignment window {offset} must fit");
            let bytes: &[u8] = bytes.map_or(&[], |slice| slice);
            for state in SEEDS {
                assert_three_way(state, bytes, "alignment sweep");
            }
        }
    }

    #[test]
    fn dispatch_and_chunk_boundaries_match_three_ways() {
        let fixture = pseudo_random();

        for state in SEEDS {
            for len in BOUNDARY_LENGTHS {
                assert_three_way(state, prefix(&fixture, len), "boundary length");
            }
        }
    }

    #[test]
    fn fixed_array_kernel_matches_the_complete_backends() {
        let fixture = pseudo_random();

        for state in SEEDS {
            for chunks in 1..=8 {
                let len = chunks * CHUNK;
                let body = prefix(&fixture, len);
                let folded = fold_braided_body(state, body);
                assert!(folded.is_some(), "{chunks} complete chunks must fold");
                let folded = folded.map_or(0, |value| value);

                assert_eq!(
                    folded,
                    crc32_braid(state, body),
                    "kernel versus braid over {chunks} chunks"
                );
                assert_eq!(
                    folded,
                    crc32_generic(state, body),
                    "kernel versus generic over {chunks} chunks"
                );
            }
        }
    }

    #[test]
    fn sequential_chunks_equal_one_call() {
        let fixture = pseudo_random();
        let steps = [
            1,
            W - 1,
            W,
            W + 1,
            CHUNK - 1,
            CHUNK,
            CHUNK + 1,
            MIN_SIMD_LEN - 1,
            MIN_SIMD_LEN,
            MIN_SIMD_LEN + 1,
            257,
        ];

        for state in SEEDS {
            let single = crc32_simd(state, &fixture);
            for step in steps {
                let mut split = state;
                for piece in fixture.chunks(step) {
                    split = crc32_simd(split, piece);
                }
                assert_eq!(
                    split, single,
                    "feeding {step}-byte pieces from state {state:#010x}"
                );
            }
        }
    }

    #[test]
    fn published_check_value_is_preserved() {
        let crc = crc32_simd(!0, b"123456789") ^ 0xffff_ffff;
        assert_eq!(crc, 0xcbf4_3926);
    }

    /// Compile-time assertion helper for the marker's consumer-facing traits.
    fn assert_marker_traits<T: Clone + Copy + core::fmt::Debug + Default>() {}

    #[test]
    fn marker_backend_is_zero_sized_and_forwards_exactly() {
        assert_marker_traits::<Simd>();
        let marker = Simd;
        let fixture = pseudo_random();

        assert_eq!(size_of_val(&marker), 0);
        assert_eq!(Simd::NAME, "simd");
        for state in SEEDS {
            assert_eq!(Simd::update(state, &fixture), crc32_simd(state, &fixture));
        }
    }
}
