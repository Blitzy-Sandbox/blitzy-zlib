//! The portable, scalar Adler-32 engine -- the reference every other Adler-32 path
//! in this workspace is measured against.
//!
//! This module is the mirror of `adler32_z` (`adler32.c` L61-L125), the single function
//! that every Adler-32 entry point of the reference implementation funnels through:
//! `adler32` is a one-line forwarder to it (`adler32.c` L128-L130), `deflate` reaches
//! it through `read_buf` when a stream is wrapped in the zlib container, and `inflate`
//! reaches it again to verify the trailer. The C-visible symbols themselves are not
//! defined here; they belong to the `libz-rs-sys` facade crate.
//!
//! # The checksum
//!
//! RFC 1950 defines the algorithm in four sentences (`doc/rfc1950.txt` L325-L329): `s1`
//! is the sum of all bytes and `s2` is the sum of all `s1` values, both taken modulo
//! 65521 -- the largest prime below 65536, spelled `BASE` in the C source (`adler32.c`
//! L10) and re-exported by this module's parent; `s1` starts at 1 and `s2` at 0; and the
//! checksum is stored as `s2 * 65536 + s1`. A running checksum is therefore a pair of
//! 16-bit residues packed into one 32-bit word, and everything in this module is phrased
//! in terms of that pair -- see `split` and `combine_halves`.
//!
//! # Why the exact shape of this code matters
//!
//! Two things make a literal, decision-for-decision correspondence with `adler32.c`
//! mandatory rather than merely tidy.
//!
//! The first is that this module is the arbiter of correctness for the optional
//! vectorised backend beside it. That backend is required to be output-neutral, and
//! "output-neutral" is defined here as agreeing with this file bit for bit on every
//! input and every starting value. Agreement is made cheap rather than aspirational: the
//! three pieces a vectorised implementation has no reason to re-derive -- the
//! single-byte path, the sub-16-byte path, and the two packing helpers -- are visible to
//! the rest of the crate so that it can delegate to them instead of duplicating them.
//!
//! The second is that an Adler-32 value is not an internal detail. It is the four-byte
//! trailer of every zlib stream (`doc/rfc1950.txt` L318-L329), so one divergent bit here
//! does not stay here: it becomes a stream that the reference implementation rejects, or
//! a valid stream this implementation refuses to accept.
//!
//! # Correspondence with the reference implementation
//!
//! | This module | `adler32.c` |
//! |---|---|
//! | `split` | L65-L67, the split into component sums |
//! | `combine_halves` | L124 (and the identical expressions at L77 and L93) |
//! | `adler32_len_1` | L70-L78, the `len == 1` fast path |
//! | `adler32_short` | L85-L94, the `len < 16` path |
//! | `accumulate_block` | L100-L103 and L110-L118, the `DO16` loops and the byte tail |
//! | `adler32_blocks` | L97-L121, the `NMAX` block loop and the final partial block |
//! | [`Adler32Generic::checksum`] | L61-L125 as a whole, branch order included |
//!
//! Only the **default** build configuration is implemented. The `NO_DIVIDE` variant
//! (`adler32.c` L22-L53) replaces each reduction with a shift-and-subtract sequence for
//! processors without hardware division; in the default configuration selected at
//! `adler32.c` L54-L58 the `MOD`, `MOD28` and `MOD63` macros are all a plain
//! `a %= BASE`, and that is what appears below. The two variants agree on every result,
//! so nothing observable is lost by implementing one of them.
//!
//! # Where a reduction happens is observable
//!
//! What is *not* freely interchangeable is the position of each reduction, because the
//! reference implementation deliberately does not normalise its component sums at every
//! opportunity. The three paths reduce differently, and that difference is visible to any
//! caller that supplies a starting value whose halves are not already below `BASE`:
//!
//! * `adler32_len_1` reduces both halves with a single conditional subtraction. For
//!   `s1` that is always sufficient. For `s2` it is not: `s2` can enter the subtraction
//!   as large as `65_535 + 65_520 = 131_055`, leaving `65_534` behind -- a value at or
//!   above `BASE`. This path can therefore return a checksum that is not fully reduced,
//!   which is exactly what the reference implementation returns.
//! * `adler32_short` reduces `s1` with a conditional subtraction but `s2` with `%`
//!   (`MOD28` at `adler32.c` L92). The asymmetry is intentional in the C source, whose
//!   comment records the reason: only so many multiples of `BASE` can have accumulated.
//! * `adler32_blocks` reduces both halves with `%` after every block.
//!
//! Collapsing the three into one "obviously equivalent" loop changes results, so they are
//! kept distinct and dispatched in the same order as the C source.
//!
//! # Layering and safety posture
//!
//! The module names `core` only. It allocates nothing, holds no raw pointer, declares
//! neither a C-visible layout nor C-visible linkage for any item, and needs no escape
//! hatch from the compiler's memory-safety guarantees, so the crate root's blanket
//! prohibition on such escape hatches costs it nothing. It cannot panic: there is no
//! indexing, no slicing, no `unwrap`, and -- as proved in `adler32_blocks` -- no
//! arithmetic that can overflow, on any input.
//!
//! Every value here is a `u32`, which is the width of the checksum itself. The reference
//! implementation returns it in a `uLong`, a type whose width is 64 bits on LP64 targets
//! and 32 bits on LLP64 Windows; reconciling the two is the business of the facade
//! crate's type module, and a fixed-width 64-bit integer must never be substituted for
//! `uLong` anywhere. Nothing in this module depends on that choice, because an Adler-32
//! value never needs more than 32 bits.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::adler32::{Adler32Backend, Adler32Generic};
//!
//! // RFC 1950's initial value is 1, and an empty input leaves it alone.
//! assert_eq!(Adler32Generic::checksum(1, &[]), 1);
//!
//! // "hello" is the preset dictionary used by zlib's own test suite.
//! assert_eq!(Adler32Generic::checksum(1, b"hello"), 0x062c_0215);
//!
//! // A checksum can be accumulated incrementally, which is what `deflate` does.
//! let staged = Adler32Generic::checksum(Adler32Generic::checksum(1, b"hel"), b"lo");
//! assert_eq!(staged, Adler32Generic::checksum(1, b"hello"));
//! ```

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use super::{Adler32Backend, BASE, NMAX};

/// Number of bytes one unrolled step of the block loop consumes.
///
/// Mirrors `DO16` (`adler32.c` L18), which expands through `DO8`, `DO4`, `DO2` and
/// `DO1` (`adler32.c` L14-L17) into sixteen consecutive accumulations. The unrolling is a
/// C micro-optimisation and is not observable, so the step below groups the same sixteen
/// accumulations without hand-expanding them; keeping the grouping at all is what makes
/// the correspondence with the C loops checkable line by line.
///
/// `NMAX` is divisible by this value -- 5552 / 16 = 347, the iteration count `n` at
/// `adler32.c` L99 -- so a full block is consumed entirely by whole steps and leaves no
/// tail, exactly as the C `do { ... } while (--n)` loop assumes.
const UNROLLED_STEP: usize = 16;

/// Length at or above which the block engine is used instead of the short path.
///
/// Mirrors the `len < 16` test at `adler32.c` L85. The C source writes the literal
/// 16 there for the same reason it writes it in `DO16`: the short path exists precisely
/// to handle inputs too small to fill one unrolled step, so the two constants are one
/// constant and are spelled that way here.
const SHORT_INPUT_LEN: usize = UNROLLED_STEP;

/// Split a packed Adler-32 into its `(sum1, sum2)` component sums.
///
/// Mirrors `adler32.c` L65-L67. `sum1` is the low half and `sum2` the high half, matching
/// RFC 1950's `s2 * 65536 + s1` layout (`doc/rfc1950.txt` L328-L329).
///
/// Both halves are masked, as the C source masks them. On a 32-bit value the mask applied
/// to the high half is a no-op that the compiler folds away; it is retained because in the
/// C source `adler` is a `uLong`, which is wider than the checksum on LP64 targets, and
/// dropping the mask would silently change what widening the accumulator to such a type
/// would mean.
///
/// Neither half is reduced modulo `BASE`. The reference implementation does not reduce
/// here either, and reducing would change the result of [`adler32_len_1`] for a starting
/// value whose halves are at or above `BASE`.
#[inline]
#[must_use]
pub(crate) fn split(adler: u32) -> (u32, u32) {
    let sum1 = adler & 0xffff;
    let sum2 = (adler >> 16) & 0xffff;
    (sum1, sum2)
}

/// Recombine two component sums into a packed Adler-32.
///
/// Mirrors the `adler | (sum2 << 16)` recombination at `adler32.c` L124; the identical
/// expression also ends the two early-return paths at `adler32.c` L77 and L93.
///
/// Both arguments must be at most `0xffff`, which is what makes the shift lossless. Every
/// caller in the crate guarantees it: `sum2` arrives either reduced modulo `BASE` or from
/// a conditional subtraction that cannot leave more than 16 bits set, and `sum1` is
/// bounded the same way.
#[inline]
#[must_use]
pub(crate) fn combine_halves(sum1: u32, sum2: u32) -> u32 {
    sum1 | (sum2 << 16)
}

/// Update a checksum with exactly one byte.
///
/// Mirrors the `len == 1` fast path at `adler32.c` L70-L78, which exists, in the words of
/// the comment at `adler32.c` L69, "in case user likes doing a byte at a time".
///
/// Both halves are reduced by a single conditional subtraction rather than by `%`, and
/// that is a behavioural commitment, not an optimisation:
///
/// * `sum1` enters the subtraction as at most `65_535 + 255 = 65_790`, so subtracting
///   `BASE` once always lands below `BASE`. The result is a true residue.
/// * `sum2` enters as at most `65_535 + 65_520 = 131_055`, so subtracting `BASE` once can
///   leave as much as `65_534`, which is *not* below `BASE`. For a starting value whose
///   high half is at or above `BASE` this path can therefore return a checksum that is not
///   fully reduced -- `0xffff_fef1` and a byte of `0xff` yield `0xfffe_fff0`. That is the
///   value the reference implementation returns, so it is the value returned here. Sending
///   the same call through [`adler32_short`] instead would yield `0x000d_fff0`.
#[inline]
#[must_use]
pub(crate) fn adler32_len_1(adler: u32, byte: u8) -> u32 {
    let (mut sum1, mut sum2) = split(adler);

    sum1 += u32::from(byte);
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    sum2 += sum1;
    if sum2 >= BASE {
        sum2 -= BASE;
    }

    combine_halves(sum1, sum2)
}

/// Update a checksum with fewer than `SHORT_INPUT_LEN` bytes.
///
/// Mirrors the `len < 16` path at `adler32.c` L85-L94, which the comment at
/// `adler32.c` L84 introduces as keeping short lengths "somewhat fast": it skips the block
/// machinery entirely and pays for exactly one reduction of each half.
///
/// The two halves are reduced differently, and the asymmetry is deliberate and observable:
///
/// * `sum1` gets one conditional subtraction. It can only reach
///   `65_535 + 15 * 255 = 69_360`, and `69_360 - 65_521 = 3_839`, so one subtraction is
///   provably enough to produce a true residue.
/// * `sum2` gets `%` -- the `MOD28` macro at `adler32.c` L92, whose trailing comment
///   ("only added so many BASE's") records that a bounded number of multiples of `BASE`
///   has accumulated. A conditional subtraction would not be enough here.
///
/// This path also serves the empty input, which is why an empty slice returns the
/// incoming value with `sum1` conditionally reduced and `sum2` taken modulo `BASE`
/// rather than returning it untouched.
///
/// # Contract
///
/// The caller must pass fewer than `SHORT_INPUT_LEN` bytes; [`Adler32Generic::checksum`]
/// dispatches on exactly that condition, and the vectorised backend delegates here under
/// the same condition. The bound is what licenses the single conditional subtraction of
/// `sum1` above and what keeps both accumulators far from overflowing, so a longer slice
/// would not merely be slow -- past roughly 257 bytes `sum1` would stop being fully
/// reduced, and past a few thousand `sum2` would exceed `u32`.
#[must_use]
pub(crate) fn adler32_short(adler: u32, buf: &[u8]) -> u32 {
    let (mut sum1, mut sum2) = split(adler);

    // `adler32.c` L86-L89: accumulate every byte, reducing neither half as it goes.
    for &byte in buf {
        sum1 += u32::from(byte);
        sum2 += sum1;
    }

    // `adler32.c` L90-L92: one conditional subtraction, then one modulo.
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    sum2 %= BASE;

    combine_halves(sum1, sum2)
}

/// Accumulate one block into the component sums, reducing neither of them.
///
/// Mirrors the two unrolled inner loops of `adler32_z`: the `do { DO16(buf); buf += 16; }
/// while (--n)` loop at `adler32.c` L100-L103, which consumes a full block, and the
/// `while (len >= 16) { DO16(buf); ... }` plus `while (len--)` pair at
/// `adler32.c` L110-L118, which consumes a partial one. The two are one loop here because
/// they differ only in how the iteration count is computed.
///
/// # Contract
///
/// `block` must be no longer than `NMAX` bytes. That is the whole reason the outer loop in
/// [`adler32_blocks`] exists, and the overflow argument recorded there depends on it.
#[inline]
fn accumulate_block(mut sum1: u32, mut sum2: u32, block: &[u8]) -> (u32, u32) {
    // Whole unrolled steps first -- the `DO16` iterations. A full block leaves no
    // remainder here, because `NMAX` is divisible by `UNROLLED_STEP`.
    let mut steps = block.chunks_exact(UNROLLED_STEP);
    for step in &mut steps {
        for &byte in step {
            sum1 += u32::from(byte);
            sum2 += sum1;
        }
    }

    // Then the fewer-than-16-byte tail, one byte at a time (`adler32.c` L115-L118).
    for &byte in steps.remainder() {
        sum1 += u32::from(byte);
        sum2 += sum1;
    }

    (sum1, sum2)
}

/// Update a checksum with at least `SHORT_INPUT_LEN` bytes.
///
/// Mirrors the block engine at `adler32.c` L97-L121: the `while (len >= NMAX)` loop that
/// consumes full blocks, and the `if (len)` tail that consumes what is left.
///
/// # Equivalence with the C loop structure
///
/// Splitting the input into `NMAX`-sized chunks reproduces the C control flow exactly:
///
/// * A chunk of exactly `NMAX` bytes is consumed by whole unrolled steps with no tail,
///   because `NMAX` is divisible by `UNROLLED_STEP`, and is then reduced -- the C loop
///   body at `adler32.c` L98-L105.
/// * The final, shorter chunk is consumed by whole steps followed by a byte loop, and is
///   then reduced -- the C tail at `adler32.c` L110-L120.
/// * When the input length is an exact multiple of `NMAX`, C's `if (len)` guard at
///   `adler32.c` L109 suppresses a further reduction. Chunking agrees, because it never
///   yields an empty final chunk.
///
/// # Why `u32` accumulators cannot overflow
///
/// `NMAX` is not a tuning knob. It is the largest block length for which 32-bit
/// accumulation is provably safe, which is what the comment at `adler32.c` L12 states:
/// `NMAX` is the largest `n` with `255n(n+1)/2 + (n+1)(BASE-1) <= 2^32 - 1`.
///
/// The worst case is the first block, reached with a starting value of `0xffff_ffff` --
/// both halves `65_535`, the largest a split can produce -- followed by exactly `NMAX`
/// bytes of `0xff`:
///
/// ```text
/// sum2_peak = 65_535 + 5_552 * 65_535 + 255 * (5_552 * 5_553 / 2) = 4_294_773_495
/// u32::MAX                                                        = 4_294_967_295
/// margin                                                          =       193_800
/// ```
///
/// `sum1` peaks at `65_535 + 5_552 * 255 = 1_481_295` and is never in danger. Every later
/// block starts from halves already reduced below `BASE`, so the margin only grows.
///
/// Two consequences follow. Enlarging a block beyond `NMAX` before reducing overflows, so
/// the chunk length below must stay `NMAX`. And because overflow is impossible, the
/// additions are written as plain `+`: wrapping or saturating arithmetic would silently
/// absorb a future mistake instead of surfacing it as the overflow panic that a debug
/// build would raise.
#[must_use]
fn adler32_blocks(adler: u32, buf: &[u8]) -> u32 {
    let (mut sum1, mut sum2) = split(adler);

    for block in buf.chunks(NMAX) {
        let (raw_sum1, raw_sum2) = accumulate_block(sum1, sum2, block);
        // The two `MOD`s at `adler32.c` L104-L105 and L119-L120.
        sum1 = raw_sum1 % BASE;
        sum2 = raw_sum2 % BASE;
    }

    combine_halves(sum1, sum2)
}

/// The portable scalar Adler-32 backend: a faithful mirror of the reference implementation,
/// available on every target and selected whenever a vectorised backend is not.
///
/// This type carries no state. It exists so that a backend can be named -- by the
/// dispatcher beside it, by the benchmarks that compare scalar against vectorised
/// throughput, and by the equivalence tests that must be able to pin a computation to one
/// specific implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Adler32Generic;

impl Adler32Backend for Adler32Generic {
    /// Update a running Adler-32 checksum with `buf` and return the new value.
    ///
    /// Mirrors `adler32_z` (`adler32.c` L61-L125) in full, dispatch order included.
    /// Exactly one of three paths runs, and each begins with the same `split` the C
    /// source performs once up front at `adler32.c` L65-L67 -- masking is pure, so where
    /// it happens is not observable, whereas which path performs it is:
    ///
    /// 1. One byte -- `adler32_len_1`, the fast path at `adler32.c` L70-L78.
    /// 2. Fewer than `SHORT_INPUT_LEN` bytes, the empty slice included --
    ///    `adler32_short`, `adler32.c` L85-L94.
    /// 3. Everything longer -- `adler32_blocks`, `adler32.c` L97-L121.
    ///
    /// The order is load-bearing. Paths 1 and 2 reduce `sum2` differently, so a one-byte
    /// input must reach path 1 to agree with the reference implementation, and the
    /// difference is visible whenever the incoming halves are not already below `BASE`.
    ///
    /// # The `Z_NULL` guard has no counterpart here
    ///
    /// Between paths 1 and 2 the C source tests `buf == Z_NULL` and returns `1L`, the
    /// initial value RFC 1950 mandates (`adler32.c` L81-L82, documented at
    /// `zlib.h` L1813-L1814). A `&[u8]` cannot be null, so that test has no counterpart
    /// here; the null-pointer contract is honoured one layer up, at the FFI
    /// boundary in the facade crate's checksum module, which answers a null `buf` with the
    /// initial value without ever calling in here.
    ///
    /// Note carefully that an **empty slice is not `Z_NULL`**. It falls through to path 2
    /// and returns the incoming value normalised -- so `checksum(0, &[])` is `0`, not `1`.
    /// Conflating the two would corrupt every stream that starts from a zero checksum.
    ///
    /// The C source's own ordering here is a known hazard worth recording: because the
    /// `len == 1` test precedes the null test, `adler32(a, NULL, 1)` dereferences a null
    /// pointer rather than returning `1L`. Rust cannot express that call at all, which
    /// removes the hazard without changing a single defined behaviour.
    #[inline]
    fn checksum(adler: u32, buf: &[u8]) -> u32 {
        // `adler32.c` L70: in case the user likes doing a byte at a time, keep it fast.
        if let [byte] = *buf {
            return adler32_len_1(adler, byte);
        }

        // `adler32.c` L81-L82 would test for a null pointer here. See the note above.

        // `adler32.c` L85: in case short lengths are provided, keep it somewhat fast.
        if buf.len() < SHORT_INPUT_LEN {
            return adler32_short(adler, buf);
        }

        // `adler32.c` L97-L121: the block engine.
        adler32_blocks(adler, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        adler32_len_1, adler32_short, combine_halves, split, Adler32Backend, Adler32Generic, BASE,
        NMAX, SHORT_INPUT_LEN, UNROLLED_STEP,
    };
    use alloc::{format, vec, vec::Vec};

    /// Length of the deterministic corpus the sweeps run over.
    const CORPUS_LEN: usize = 100_000;

    /// Adler-32 of the whole corpus, started from RFC 1950's initial value.
    const CORPUS_CHECKSUM: u32 = 0x76f5_980f;

    /// Starting values whose halves are already below `BASE`, so that every path
    /// normalises its result and the three of them can be compared freely.
    const CANONICAL_STARTS: [u32; 4] = [0x0000_0001, 0x0000_0000, 0x000e_000e, 0x1234_5678];

    /// Starting values whose halves are at or above `BASE`. A caller may legitimately
    /// supply these -- nothing in the C API rejects them -- and they are what makes the
    /// placement of each reduction observable.
    const PATHOLOGICAL_STARTS: [u32; 3] = [0xffff_ffff, 0xffff_fef1, 0xfff0_fff0];

    /// `(length, checksum)` pairs over the pattern corpus, started from 1.
    ///
    /// Every value was produced by the in-tree C implementation and re-verified against it
    /// for this implementation. The lengths straddle each path boundary (1, 2, 15, 16, 17) and each
    /// reduction boundary (`NMAX` and its multiples, one either side).
    const SWEEP: [(usize, u32); 15] = [
        (1, 0x0008_0008),
        (2, 0x0036_002e),
        (15, 0x3227_0721),
        (16, 0x3a20_07f9),
        (17, 0x4310_08f0),
        (5_551, 0x5f26_cd87),
        (5_552, 0x2cf4_cdbf),
        (5_553, 0xfb0a_ce16),
        (11_103, 0xa4ee_9b04),
        (11_104, 0x4089_9b8c),
        (11_105, 0xdcbc_9c33),
        (16_656, 0x70bf_6959),
        (16_657, 0xdb0f_6a50),
        (65_536, 0x7b2e_8772),
        (100_000, 0x76f5_980f),
    ];

    /// Build the deterministic corpus `buf[i] = (i * 31 + 7) & 0xff`, without I/O and
    /// without randomness so that every expectation above is reproducible anywhere.
    ///
    /// Expressed as a `u8` accumulator advancing by 31 rather than as a masked product:
    /// `u8` arithmetic is modulo 256, which is precisely what the mask means, so the two
    /// formulations agree byte for byte -- and this one needs no narrowing conversion.
    fn pattern_corpus(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut byte: u8 = 7;
        for _ in 0..len {
            out.push(byte);
            byte = byte.wrapping_add(31);
        }
        out
    }

    /// Take a prefix of `buf`, asserting that it is long enough, without indexing.
    fn prefix(buf: &[u8], len: usize) -> &[u8] {
        let taken = buf.get(..len).unwrap_or_default();
        assert_eq!(taken.len(), len, "the fixture is shorter than {len} bytes");
        taken
    }

    /// An independent model of RFC 1950 (`doc/rfc1950.txt` L325-L329), reducing both sums
    /// after every single byte.
    ///
    /// Deliberately *not* a mirror of `adler32.c`: it is derived from the specification
    /// alone, so agreement with [`Adler32Generic::checksum`] is evidence about the
    /// algorithm rather than a restatement of the same code. Reduction is a ring
    /// homomorphism, so reducing eagerly and reducing lazily produce the same residues;
    /// the two therefore agree for every starting value whose halves are already below
    /// `BASE`, which includes RFC 1950's initial value of 1. They may disagree only where
    /// the code deliberately returns a value that is not fully reduced, which is
    /// covered by its own test below.
    fn naive_adler32(adler: u32, buf: &[u8]) -> u32 {
        let mut sum1 = (adler & 0xffff) % BASE;
        let mut sum2 = ((adler >> 16) & 0xffff) % BASE;
        for &byte in buf {
            sum1 = (sum1 + u32::from(byte)) % BASE;
            sum2 = (sum2 + sum1) % BASE;
        }
        sum1 | (sum2 << 16)
    }

    #[test]
    fn constants_match_the_c_source() {
        assert_eq!(BASE, 65_521, "`BASE`, `adler32.c` L10");
        assert_eq!(NMAX, 5_552, "`NMAX`, `adler32.c` L11");
        assert_eq!(UNROLLED_STEP, 16, "`DO16`, `adler32.c` L18");
        assert_eq!(SHORT_INPUT_LEN, 16, "the `len < 16` test, `adler32.c` L85");

        // `adler32.c` L99 relies on this: a full block is whole steps and no tail.
        assert_eq!(
            NMAX % UNROLLED_STEP,
            0,
            "`NMAX` must be divisible by `UNROLLED_STEP`"
        );
        assert_eq!(
            NMAX / UNROLLED_STEP,
            347,
            "the iteration count `n`, `adler32.c` L99"
        );

        // `adler32.c` L12: `NMAX` is the LARGEST `n` with
        // 255n(n+1)/2 + (n+1)(BASE-1) <= 2^32 - 1. Evaluated in `u64` so the products
        // are exact; the literal is sound because the assertion above pins `NMAX` to it.
        let bound = |n: u64| 255 * n * (n + 1) / 2 + (n + 1) * (u64::from(BASE) - 1);
        assert!(
            u32::try_from(bound(5_552)).is_ok(),
            "`NMAX` must satisfy the bound"
        );
        assert!(
            u32::try_from(bound(5_553)).is_err(),
            "`NMAX` must be the largest such n"
        );
    }

    #[test]
    fn worst_case_block_accumulation_fits_in_u32() {
        // The tightest case: both halves of the starting value at their maximum, then a
        // whole block of 0xff bytes. This is the arithmetic quoted in `adler32_blocks`,
        // evaluated in `u64` so every product is exact. The literal stands in for `NMAX`,
        // which `constants_match_the_c_source` pins to the same value.
        let n: u64 = 5_552;
        assert_eq!(
            NMAX, 5_552,
            "the peak below is computed for this block length"
        );

        let sum2_peak = 65_535 + n * 65_535 + 255 * (n * (n + 1) / 2);
        assert_eq!(sum2_peak, 4_294_773_495, "the documented peak");
        assert!(
            u32::try_from(sum2_peak).is_ok(),
            "a block must not overflow `u32`"
        );
        assert_eq!(
            u64::from(u32::MAX) - sum2_peak,
            193_800,
            "the documented margin"
        );

        let sum1_peak = 65_535 + n * 255;
        assert_eq!(sum1_peak, 1_481_295, "the documented `sum1` peak");
        assert!(
            u32::try_from(sum1_peak).is_ok(),
            "`sum1` must not overflow `u32`"
        );

        // One byte more than a block would exceed `u32`, which is why the chunk length in
        // `adler32_blocks` is exactly `NMAX` and must never be enlarged.
        let m = n + 1;
        let over = 65_535 + m * 65_535 + 255 * (m * (m + 1) / 2);
        assert!(
            u32::try_from(over).is_err(),
            "a longer block would overflow `u32`"
        );
    }

    #[test]
    fn split_and_combine_halves_round_trip() {
        for adler in [0x0000_0000, 0x0000_0001, 0xffff_ffff, 0x1234_5678_u32] {
            let (sum1, sum2) = split(adler);
            assert_eq!(sum1, adler & 0xffff, "low half of {adler:#010x}");
            assert_eq!(sum2, adler >> 16, "high half of {adler:#010x}");
            assert_eq!(
                combine_halves(sum1, sum2),
                adler,
                "round trip of {adler:#010x}"
            );
        }
    }

    #[test]
    fn empty_input_returns_the_incoming_value_normalised() {
        // An empty slice is not `Z_NULL`: it takes the short path and returns the running
        // value, so a zero checksum stays zero instead of becoming the initial value 1.
        assert_eq!(Adler32Generic::checksum(1, &[]), 0x0000_0001);
        assert_eq!(Adler32Generic::checksum(0, &[]), 0x0000_0000);

        // Both halves of 0xffff_ffff are 65_535, which is above `BASE`; the short path
        // normalises them to 14 -- 65_535 - 65_521 for `sum1`, and the same value from
        // `sum2 %= BASE`.
        assert_eq!(Adler32Generic::checksum(0xffff_ffff, &[]), 0x000e_000e);
    }

    #[test]
    fn single_byte_matches_the_c_fast_path() {
        assert_eq!(Adler32Generic::checksum(1, b"a"), 0x0062_0062);
        assert_eq!(Adler32Generic::checksum(0xffff_ffff, b"a"), 0x007d_006f);
        assert_eq!(Adler32Generic::checksum(0xffff_ffff, &[0xff]), 0x011b_010d);
        assert_eq!(Adler32Generic::checksum(0xffff_0000, &[0x00]), 0x000e_0000);
        assert_eq!(Adler32Generic::checksum(0x0000_0000, &[0x00]), 0x0000_0000);
        assert_eq!(Adler32Generic::checksum(0xfff0_fff0, &[0xff]), 0x00fd_00fe);
    }

    #[test]
    fn single_byte_path_can_return_a_non_canonical_high_half() {
        // `sum1` becomes 65_520 and `sum2` becomes 65_535 + 65_520 = 131_055, which one
        // conditional subtraction reduces only to 65_534 -- still at or above `BASE`. The
        // reference implementation returns that, so this implementation must too.
        let got = Adler32Generic::checksum(0xffff_fef1, &[0xff]);
        assert_eq!(
            got, 0xfffe_fff0,
            "the C `len == 1` path, `adler32.c` L70-L78"
        );
        assert!(
            got >> 16 >= BASE,
            "the high half is deliberately not fully reduced"
        );

        // Proof that the branch order is behaviour rather than style: the short path would
        // answer the very same call differently, because it reduces `sum2` with `%`.
        assert_eq!(adler32_short(0xffff_fef1, &[0xff]), 0x000d_fff0);
        assert_ne!(adler32_short(0xffff_fef1, &[0xff]), got);

        // And a fully reduced model disagrees for the same reason.
        assert_ne!(naive_adler32(0xffff_fef1, &[0xff]), got);
    }

    #[test]
    fn short_inputs_match_the_c_short_path() {
        assert_eq!(Adler32Generic::checksum(1, b"abc"), 0x024d_0127);
        assert_eq!(Adler32Generic::checksum(1, &[b'x'; 15]), 0x384f_0709);
        assert_eq!(
            Adler32Generic::checksum(0xffff_ffff, &[0xff; 15]),
            0x7868_0eff
        );
        assert_eq!(
            Adler32Generic::checksum(0xffff_ffff, &[0xff; 2]),
            0x0327_020c
        );
    }

    #[test]
    fn sixteen_bytes_take_the_block_path() {
        assert_eq!(Adler32Generic::checksum(1, &[b'x'; 16]), 0x3fd0_0781);
        assert_eq!(
            Adler32Generic::checksum(0xffff_ffff, &[0xff; 16]),
            0x8866_0ffe
        );
    }

    #[test]
    fn strings_from_the_reference_test_suite() {
        // `hello` is the preset dictionary and `hello, hello!` the payload used by
        // `test/example.c`, so these two values gate the acceptance suite.
        assert_eq!(Adler32Generic::checksum(1, b"hello"), 0x062c_0215);
        assert_eq!(Adler32Generic::checksum(1, b"hello, hello!"), 0x2170_0496);
        assert_eq!(Adler32Generic::checksum(1, b"Wikipedia"), 0x11e6_0398);
    }

    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, and
    // this module has no `unsafe`, no raw pointer and no uninitialised memory for it to inspect:
    // the only faults it can surface here are integer overflow and an out-of-bounds index, and
    // both are reached by the cheap tests above, which run in full. Folding a 100 000-byte corpus
    // ten different ways adds numeric confidence and no undefined-behaviour coverage, so it is
    // skipped under Miri only -- exactly the trade `adler32/combine.rs` documents for its own
    // whole-corpus tests. Nothing is skipped under a normal `cargo test`.
    #[cfg_attr(
        miri,
        ignore = "folds a 100 000-byte corpus; numeric agreement, no UB coverage"
    )]
    #[test]
    fn reduction_boundary_sweep() {
        let corpus = pattern_corpus(CORPUS_LEN);
        for &(len, expected) in &SWEEP {
            let got = Adler32Generic::checksum(1, prefix(&corpus, len));
            assert_eq!(got, expected, "checksum(1, pattern[..{len}]) = {got:#010x}");
        }
    }

    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, and
    // this module has no `unsafe`, no raw pointer and no uninitialised memory for it to inspect:
    // the only faults it can surface here are integer overflow and an out-of-bounds index, and
    // both are reached by the cheap tests above, which run in full. Folding a 100 000-byte corpus
    // ten different ways adds numeric confidence and no undefined-behaviour coverage, so it is
    // skipped under Miri only -- exactly the trade `adler32/combine.rs` documents for its own
    // whole-corpus tests. Nothing is skipped under a normal `cargo test`.
    #[cfg_attr(
        miri,
        ignore = "folds a 100 000-byte corpus; numeric agreement, no UB coverage"
    )]
    #[test]
    fn worst_case_accumulator_stress() {
        // Every byte 0xff maximises both accumulators. Two whole blocks, then a length
        // far beyond them.
        let saturated = vec![0xff_u8; CORPUS_LEN];
        assert_eq!(
            Adler32Generic::checksum(1, prefix(&saturated, 11_104)),
            0xff6f_3726
        );
        assert_eq!(Adler32Generic::checksum(1, &saturated), 0x149a_302c);

        // The exact bound: the largest possible starting value followed by exactly one
        // full block of 0xff, which is the case with only 193_800 of headroom. A wider
        // block, or accumulators narrower than the reductions assume, would overflow here
        // -- and an overflow would be an outright panic under the debug profile this test
        // runs in, not a silently wrong answer.
        assert_eq!(
            Adler32Generic::checksum(0xffff_ffff, prefix(&saturated, NMAX)),
            0x0bab_9b99,
        );

        // One byte past the bound, so the peak block is followed by a second reduction.
        assert_eq!(
            Adler32Generic::checksum(0xffff_ffff, prefix(&saturated, NMAX + 1)),
            0xa843_9c98,
        );
    }

    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, and
    // this module has no `unsafe`, no raw pointer and no uninitialised memory for it to inspect:
    // the only faults it can surface here are integer overflow and an out-of-bounds index, and
    // both are reached by the cheap tests above, which run in full. Folding a 100 000-byte corpus
    // ten different ways adds numeric confidence and no undefined-behaviour coverage, so it is
    // skipped under Miri only -- exactly the trade `adler32/combine.rs` documents for its own
    // whole-corpus tests. Nothing is skipped under a normal `cargo test`.
    #[cfg_attr(
        miri,
        ignore = "folds a 100 000-byte corpus; numeric agreement, no UB coverage"
    )]
    #[test]
    fn chunked_feeding_matches_single_shot() {
        // `deflate` and `inflate` both accumulate the checksum incrementally, over
        // whatever chunk sizes their callers happen to produce, so every split of the same
        // input must land on the same value.
        let corpus = pattern_corpus(CORPUS_LEN);
        assert_eq!(Adler32Generic::checksum(1, &corpus), CORPUS_CHECKSUM);

        let steps = [
            1,
            SHORT_INPUT_LEN - 1,
            SHORT_INPUT_LEN,
            SHORT_INPUT_LEN + 1,
            4_096,
            NMAX - 1,
            NMAX,
            NMAX + 1,
            2 * NMAX,
            CORPUS_LEN - 1,
        ];
        for step in steps {
            let mut adler = 1;
            for chunk in corpus.chunks(step) {
                adler = Adler32Generic::checksum(adler, chunk);
            }
            assert_eq!(
                adler, CORPUS_CHECKSUM,
                "feeding the corpus {step} bytes at a time"
            );
        }
    }

    #[test]
    fn the_crate_visible_helpers_are_faithful_delegates() {
        // The vectorised backend delegates to these instead of re-deriving them, so each
        // must be indistinguishable from the dispatcher on the length it serves -- for
        // pathological starting values as much as for canonical ones.
        let corpus = pattern_corpus(SHORT_INPUT_LEN);
        for start in CANONICAL_STARTS
            .iter()
            .chain(PATHOLOGICAL_STARTS.iter())
            .copied()
        {
            for &byte in &[0x00_u8, 0x01, 0x7f, 0xfe, 0xff] {
                assert_eq!(
                    adler32_len_1(start, byte),
                    Adler32Generic::checksum(start, &[byte]),
                    "`adler32_len_1({start:#010x}, {byte:#04x})`",
                );
            }

            for len in (0..SHORT_INPUT_LEN).filter(|&len| len != 1) {
                let short = prefix(&corpus, len);
                assert_eq!(
                    adler32_short(start, short),
                    Adler32Generic::checksum(start, short),
                    "`adler32_short({start:#010x}, pattern[..{len}])`",
                );
            }
        }
    }

    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, and
    // this module has no `unsafe`, no raw pointer and no uninitialised memory for it to inspect:
    // the only faults it can surface here are integer overflow and an out-of-bounds index, and
    // both are reached by the cheap tests above, which run in full. Folding a 100 000-byte corpus
    // ten different ways adds numeric confidence and no undefined-behaviour coverage, so it is
    // skipped under Miri only -- exactly the trade `adler32/combine.rs` documents for its own
    // whole-corpus tests. Nothing is skipped under a normal `cargo test`.
    #[cfg_attr(
        miri,
        ignore = "folds a 100 000-byte corpus; numeric agreement, no UB coverage"
    )]
    #[test]
    fn matches_an_independent_rfc1950_model() {
        let corpus = pattern_corpus(CORPUS_LEN);
        let lengths = [
            0,
            1,
            2,
            SHORT_INPUT_LEN - 1,
            SHORT_INPUT_LEN,
            SHORT_INPUT_LEN + 1,
            255,
            256,
            257,
            NMAX - 1,
            NMAX,
            NMAX + 1,
            2 * NMAX,
            2 * NMAX + 1,
            CORPUS_LEN,
        ];
        for len in lengths {
            let buf = prefix(&corpus, len);
            assert_eq!(
                Adler32Generic::checksum(1, buf),
                naive_adler32(1, buf),
                "the specification model disagrees at length {len}",
            );
        }

        // Canonical starting values agree as well, for the same homomorphism reason.
        for start in CANONICAL_STARTS {
            let buf = prefix(&corpus, 3 * NMAX + 7);
            assert_eq!(
                Adler32Generic::checksum(start, buf),
                naive_adler32(start, buf),
                "the specification model disagrees from {start:#010x}",
            );
        }
    }

    /// Construct a `T` through its [`Default`] implementation.
    ///
    /// Written generically on purpose: it exercises the derive rather than the unit-struct
    /// literal, which is what a direct `Adler32Generic::default()` would collapse into.
    fn default_of<T: Default>() -> T {
        T::default()
    }

    /// Duplicate a `T` through its [`Clone`] implementation, generically for the same
    /// reason: for a `Copy` type a direct `clone()` call is just a copy.
    fn clone_of<T: Clone>(value: &T) -> T {
        value.clone()
    }

    #[test]
    fn the_backend_marker_carries_the_derives_its_consumers_need() {
        // Zero-sized, so naming a backend costs nothing at run time, and carrying every
        // derive the dispatcher, the benchmarks and the equivalence tests rely on.
        let backend: Adler32Generic = default_of();
        let cloned = clone_of(&backend);
        let copied = cloned;

        assert_eq!(size_of_val(&copied), 0, "the marker must be zero-sized");
        // `cloned` is still usable after the assignment above, which is `Copy` at work.
        assert_eq!(format!("{cloned:?}"), "Adler32Generic");
        assert_eq!(format!("{copied:?}"), "Adler32Generic");
    }
}
