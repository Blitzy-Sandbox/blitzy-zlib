//! Adler-32 concatenation: the `adler32_combine` family.
//!
//! Mirrors `adler32.c` lines 133-155 -- the `local uLong adler32_combine_`
//! helper -- together with the two one-line C wrappers that share it,
//! `adler32_combine` at lines 158-160 and `adler32_combine64` at lines 162-164.
//!
//! # What is computed
//!
//! Given two byte sequences `seq1` and `seq2` whose Adler-32 checksums `adler1` and `adler2`
//! are already known, [`adler32_combine`] returns the Adler-32 checksum of `seq1` followed by
//! `seq2`. It needs only `adler1`, `adler2` and the length of the *second* sequence: neither
//! sequence has to be re-read, and `len1` is never required at all. `zlib.h` lines 1836-1846
//! state that contract for the exported C entry point.
//!
//! An Adler-32 value packs two running sums modulo `BASE` (`65_521`, the largest prime below
//! `65_536`) into a single 32-bit word: `s1` occupies the low sixteen bits and `s2` the high
//! sixteen. Writing `s1a`/`s2a` for the halves of `adler1` and `s1b`/`s2b` for the halves of
//! `adler2`, the checksum of the concatenation has halves
//!
//! ```text
//! s1 = s1a + s1b - 1                   (mod BASE)
//! s2 = len2 * s1a + s2a + s2b - len2   (mod BASE)
//! ```
//!
//! The `- 1` and `- len2` terms cancel the `s1 = 1` seed that `seq2`'s own checksum was started
//! from. `adler32.c` line 142 declines to show this, remarking only that "the derivation of this
//! formula is left as an exercise for the reader"; the identity is recorded here so that the code
//! below reads as an implementation of something stated rather than as an opaque run of additions
//! and comparisons. It is also machine-checked against this implementation, by the
//! `agrees_with_the_closed_form_identity` test at the bottom of this file. The code itself
//! nevertheless follows the C statement order rather than the formula, for the reasons given
//! under *Why the C statement order is preserved* below.
//!
//! # One body, two exported symbols
//!
//! `zlib.h` line 2001 redefines `adler32_combine` to `adler32_combine64` when a caller compiles
//! with `_FILE_OFFSET_BITS == 64`, so which of the two names a translation unit references is
//! decided by that caller and not by the library. Both symbols must therefore exist in the
//! shipped library, and both are backed by this single function -- exactly as the reference
//! sources back both wrappers with one `local` helper.
//!
//! That duality is also why `len2` is a plain `i64` here instead of a platform-dependent width.
//! `z_off64_t` is a *signed* 64-bit integer (`zconf.h` lines 519-531), and it is the widest of
//! the length types the two wrappers accept, so it is the only choice that can serve both.
//! Mapping the caller-visible `z_off_t`, `z_off64_t` and `uLong` widths onto Rust types is the
//! job of the C ABI facade in `crates/libz-rs-sys`, not of the engine.
//!
//! # Negative lengths are part of the contract
//!
//! `zlib.h` line 1844 warns that "the `z_off_t` type (like `off_t`) is a signed integer" and that
//! "if len2 is negative, the result has no meaning or utility". The reference implementation does
//! not report that as an error; it returns the sentinel `0xffff_ffff`, and the comment at
//! `adler32.c` lines 138-140 explains why -- "for negative len, return invalid adler32 as a clue
//! for debugging". The sentinel is unambiguous because it cannot be a legitimate Adler-32 value:
//! both halves of a legitimate one are strictly below `BASE`, so neither half can be `0xffff`.
//! This implementation reproduces the sentinel exactly. It is API, not an error path invented here, and in
//! particular it is not a panic: callers such as the reference coverage harness rely on the
//! checksum entry points never terminating the process.
//!
//! # Two widths, because the reference computes in `unsigned long`
//!
//! The reference accumulates into `unsigned long` and returns `uLong`, so on LP64 its result is a
//! **64-bit** value. Within the domain the exported function is documented for that makes no
//! difference: each of `adler1` and `adler2` is a genuine Adler-32 checksum -- both 16-bit halves
//! strictly below [`super::BASE`] (`65_521`) -- and then both internal sums finish below `0xffff`
//! and the packed result occupies exactly 32 bits.
//!
//! Outside that domain it does. When *both* arguments carry a high half at or above `BASE`, the
//! internal `sum2` can finish above `0xffff` -- brute-force search over the reachable
//! `(rem, sum2_pre)` space puts its maximum at `65_548` -- so `sum1 | (sum2 << 16)` carries a bit
//! at position 32 and the true result needs **33 bits**. Three witnesses are pinned in the test
//! vectors below, where an LP64 reference build returns `0x1_000b_ffef`, `0x1_000b_fffd` and
//! `0x1_000a_ffef`. `sum1` can exceed `0xffff` for the same reason and by the same bound, so the
//! two fields of the packed word genuinely overlap there; the reference lets them, and so does
//! this.
//!
//! Two entry points therefore exist, and which one a caller wants follows from the width it is
//! answering in:
//!
//! * [`adler32_combine_wide`] returns `u64`. It is what the C ABI facade calls, because
//!   `crates/libz-rs-sys` has to answer in `uLong`: on LP64 the `u64` reaches the caller intact,
//!   and on LLP64 Windows or ILP32 the facade's narrowing cast discards the same high bits a
//!   32-bit `unsigned long` would have discarded on its own. Every intermediate of the algorithm
//!   is proved below to fit a `u32` -- only the final packing can exceed one -- so truncating the
//!   64-bit result is identical to computing the whole thing in 32-bit arithmetic, which is what
//!   makes one function serve both integer models exactly.
//! * [`adler32_combine`] returns `u32`, the width of an Adler-32 checksum and the width the rest
//!   of this engine works in. It is the truncation of the wide form, which is the same value on
//!   every in-domain argument and is what the idiomatic Rust surface should offer: a type that
//!   cannot hold a non-checksum.
//!
//! Nothing is asserted and nothing is rejected, exactly as `adler32.c` L133-L155 asserts and
//! rejects nothing. A `debug_assert!` would be a panic, AAP 0.7.1(f) forbids panics in library
//! paths, and the reference coverage harness relies on the checksum entry points never
//! terminating the process.
//!
//! # Why the C statement order is preserved
//!
//! The order of the steps in `adler32_combine_` is behaviour rather than style, and it is what
//! keeps this function free of panics as well as of silent wrapping:
//!
//! * The negative-length test comes first, so the reduction that follows is known to act on a
//!   non-negative value and the narrowing it feeds is provably exact. The test is a comparison
//!   and never a negation, which is what makes `i64::MIN` -- the one value whose negation does
//!   not exist -- exactly as safe here as any other input.
//! * Reducing `len2` modulo `BASE` happens before `rem` is read anywhere, so `BASE - rem` cannot
//!   drop below zero and `rem * s1a` cannot leave `u32`. Delaying or skipping that reduction
//!   turns both into overflow, and overflow is a panic under the debug and test profiles.
//! * The four conditional subtractions run in the C order. `s1` genuinely needs two of them,
//!   because the step before can add as much as two whole multiples of `BASE`.
//!
//! Every intermediate bound is stated at the step that establishes it, and the tightest of them
//! -- that `rem * s1a` still fits a `u32` -- is asserted by the
//! `maximum_internal_product_does_not_overflow` test rather than left as prose.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use super::BASE;

/// `BASE` is the modulus every Adler-32 sum is taken over: `65_521`, the largest prime below
/// `65_536` (`adler32.c` line 10).
///
/// The overflow bounds proved step by step in `adler32_combine_` depend on that exact value, so
/// it is pinned at compile time here instead of merely assumed. If the shared constant ever
/// changed, this module would fail to build rather than quietly compute wrong checksums.
const _: () = assert!(
    BASE == 65_521,
    "the bounds proved in this module assume BASE == 65_521"
);

/// Combines two Adler-32 checksums into the checksum of the concatenated data.
///
/// Mirrors the C wrappers `adler32_combine` (`adler32.c` lines 158-160) and `adler32_combine64`
/// (lines 162-164). Both forward to the `local` helper at lines 133-155 and differ only in the
/// declared width of `len2`, so one Rust function taking the wider, signed length backs both
/// exported symbols. See the module documentation for why that is required rather than merely
/// convenient.
///
/// # Arguments
///
/// * `adler1` -- Adler-32 checksum of the first sequence, `seq1`.
/// * `adler2` -- Adler-32 checksum of the second sequence, `seq2`.
/// * `len2` -- length in bytes of `seq2`. The length of `seq1` is not needed.
///
/// # Returns
///
/// The Adler-32 checksum of `seq1` followed by `seq2`, or the sentinel `0xffff_ffff` when `len2`
/// is negative. Never panics, for any combination of arguments.
///
/// # Examples
///
/// Concatenating with an empty second sequence leaves the first checksum unchanged, and
/// concatenating the checksum of empty input -- which is `1`, the Adler-32 seed -- with a
/// checksum `a` taken over `n` bytes reproduces `a`:
///
/// ```text
/// adler32_combine(a, 1, 0) == a
/// adler32_combine(1, a, n) == a
/// ```
///
/// # Domain
///
/// `adler1` and `adler2` are each expected to be a genuine Adler-32 checksum, meaning both 16-bit
/// halves of each are strictly below [`super::BASE`]. Every checksum this crate produces
/// satisfies that, and within it this function's `u32` result is the reference's result on every
/// target. The function is nonetheless total -- it cannot panic and it validates nothing, exactly
/// as `adler32.c` L133-L155 does not -- and for arguments outside that domain it returns the low
/// 32 bits of a 33-bit value. [`adler32_combine_wide`] returns the whole of it, and the C ABI
/// facade calls that one; see *Two widths* in the module documentation.
///
/// A negative `len2` is *not* outside the domain: it is part of the contract and yields the
/// `0xffff_ffff` sentinel, matching the reference on every target.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn adler32_combine(adler1: u32, adler2: u32, len2: i64) -> u32 {
    // The truncation is the point: this is the checksum-width face of the pair, and on every
    // argument the exported C function is documented for the wide result already fits.
    adler32_combine_(adler1, adler2, len2) as u32
}

/// Combines two Adler-32 checksums at the full width the reference computes in.
///
/// Identical to [`adler32_combine`] except in the width of the result: this is the value an LP64
/// C build returns, which needs up to 33 bits for arguments that are not Adler-32 checksums.
/// `crates/libz-rs-sys/src/checksum.rs` calls this and narrows to `uLong`, which reproduces LP64
/// exactly and reproduces a 32-bit `unsigned long` exactly as well -- every intermediate of the
/// algorithm is proved to fit a `u32`, so narrowing the packed result and computing the packing in
/// 32 bits give the same bits.
///
/// # Arguments
///
/// As [`adler32_combine`]: the two checksums and the length of the *second* sequence only.
///
/// # Returns
///
/// The Adler-32 checksum of `seq1` followed by `seq2`, or the sentinel `0xffff_ffff` when `len2`
/// is negative -- `adler32.c` L140's `0xffffffffUL`, which is that same 32-bit pattern in a
/// 64-bit `unsigned long`. Never panics, for any combination of arguments.
#[must_use]
pub fn adler32_combine_wide(adler1: u32, adler2: u32, len2: i64) -> u64 {
    adler32_combine_(adler1, adler2, len2)
}

/// The `local` implementation shared by both C wrappers.
///
/// Mirrors `adler32.c` lines 133-155. `local` expands to `static` in the reference sources
/// (`zutil.h` lines 32-34), so this counterpart is private: it is reachable only from within this
/// module and forms no part of the crate's public surface, mirroring the fact that the C helper is
/// not an exported symbol either.
///
/// The body reproduces the C statement order literally; every step names the reference line it
/// comes from and states the bound that keeps it inside `u32`. The argument and accumulator names
/// are the reference implementation's own, because fidelity that has to be auditable against
/// the C source is better served by identical naming than by invented synonyms.
fn adler32_combine_(adler1: u32, adler2: u32, len2: i64) -> u64 {
    // Step 1 -- `adler32.c` lines 138-140: `if (len2 < 0) return 0xffffffffUL;`
    //
    // "for negative len, return invalid adler32 as a clue for debugging". This must stay first:
    // it is what lets step 2 treat `len2` as non-negative, and being a plain comparison it is
    // total even for `i64::MIN`, which has no representable negation.
    if len2 < 0 {
        return 0xffff_ffff;
    }

    // Step 2 -- `adler32.c` lines 143-144: `MOD63(len2);` then `rem = (unsigned)len2;`
    //
    // `MOD63` is `a %= BASE` in the shipped configuration (`adler32.c` lines 54-58); the
    // `NO_DIVIDE` shift-and-fold variant at lines 41-53 computes the same residue and is not
    // implemented. The reduction has to happen here, before `rem` is used by steps 4 and 6: without
    // it `rem` could exceed `BASE`, making `BASE - rem` underflow and `rem * sum1` overflow.
    //
    // Step 1 established `len2 >= 0`, so `len2 % BASE` lies in `0..=65_520`. The narrowing to
    // `u32` is therefore exact, and it discards a sign that is known to be positive -- the same
    // reasoning that makes the reference implementation's `(unsigned)len2` cast sound.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rem = (len2 % i64::from(BASE)) as u32;

    // Step 3 -- `adler32.c` line 145: `sum1 = adler1 & 0xffff;`
    //
    // The low half of `adler1`, so `sum1 <= 65_535`. Bits at or above position 32 of the C
    // `uLong` arguments are never read by this algorithm -- only these two 16-bit fields are --
    // which is what makes the facade's narrowing of `uLong` to `u32` lossless.
    let mut sum1 = adler1 & 0xffff;

    // Step 4 -- `adler32.c` lines 146-147: `sum2 = rem * sum1;` then `MOD(sum2);`
    //
    // `rem <= BASE - 1 == 65_520` from step 2 and `sum1 <= 65_535` from step 3, so the product is
    // at most `65_520 * 65_535 == 4_293_853_200`, which is `1_114_095` below `u32::MAX`. The two
    // statements are kept separate exactly as in the reference source, and the bound is asserted
    // by the `maximum_internal_product_does_not_overflow` test.
    let mut sum2 = rem * sum1;
    sum2 %= BASE;

    // Step 5 -- `adler32.c` line 148: `sum1 += (adler2 & 0xffff) + BASE - 1;`
    //
    // Left-to-right evaluation gives `((adler2 & 0xffff) + BASE) - 1`, whose left operand is at
    // most `65_535 + 65_521 == 131_056`, so nothing underflows and `sum1 <= 196_590`. This is
    // where `sum1` can gain as much as two multiples of `BASE`, which is why step 7 reduces it
    // twice.
    sum1 += (adler2 & 0xffff) + BASE - 1;

    // Step 6 -- `adler32.c` line 149:
    // `sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;`
    //
    // The two high halves contribute at most `131_070`; adding `BASE` before subtracting `rem`
    // keeps the running value at or above `65_521`, and `rem <= BASE - 1` from step 2, so the
    // subtraction cannot underflow. With `sum2 <= BASE - 1` on entry the result is at most
    // `65_520 + 131_070 + 65_521 == 262_111`.
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;

    // Step 7 -- `adler32.c` lines 150-153.
    //
    // Conditional subtraction in place of a division, in the reference order. `sum1` is reduced
    // twice because step 5 can add two multiples of `BASE`; `sum2` is reduced first by `2 * BASE`
    // and then by `BASE`, which the bounds above make sufficient. Each subtraction is guarded by
    // the comparison immediately preceding it, so none of them can underflow.
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    // `adler32.c` line 152 writes the doubled modulus as `((unsigned long)BASE << 1)`; it is
    // `131_042`.
    if sum2 >= (BASE << 1) {
        sum2 -= BASE << 1;
    }
    if sum2 >= BASE {
        sum2 -= BASE;
    }

    // Step 8 -- `adler32.c` line 154: `return sum1 | (sum2 << 16);`
    //
    // Repack the two sums into one Adler-32 word, **in the width the reference packs in**. C's
    // `sum1` and `sum2` are `unsigned long`, so on LP64 this statement is 64-bit arithmetic; for
    // legitimate arguments both sums are below `BASE` and occupy sixteen bits each, so the width
    // is invisible, and for the out-of-domain arguments described under *Two widths* in the module
    // documentation each can reach `65_548` and the packed value needs 33 bits. Widening here is
    // what lets one body serve LP64 and a 32-bit `unsigned long` alike: `adler32_combine`
    // truncates to `u32` and the facade truncates to `uLong`, and a truncated left shift equals a
    // left shift of the truncation, so neither can differ from C's own arithmetic.
    //
    // The shift amount is a constant `16`, well inside the width of `u64`, so this cannot panic.
    u64::from(sum1) | (u64::from(sum2) << 16)
}

#[cfg(test)]
mod tests {
    // `clippy::indexing_slicing` is denied across the workspace so that no library path can panic
    // on a bad index. The corpus tests below slice a `Vec` at constant, provably in-bounds
    // offsets; routing each of them through `get(..)` would add an error arm that cannot be taken
    // and would bury the property under inspection. The two cast lints are relaxed for the same
    // reason: every narrowing below acts on a value the surrounding line has already bounded --
    // a byte masked to `0xff`, a sum reduced modulo `BASE`, a length masked to 47 bits. Both
    // relaxations stop at this module, which is compiled only under `cfg(test)` and never ships.
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::indexing_slicing
    )]

    use super::{adler32_combine, adler32_combine_wide, BASE};
    use alloc::vec::Vec;

    /// Largest block the reference Adler-32 loop may run before reducing, from `adler32.c` line
    /// 11: the largest `n` for which `255 * n * (n + 1) / 2 + (n + 1) * (BASE - 1)` still fits a
    /// `u32`. Reducing at that cadence is what keeps `reference_adler32` free of overflow.
    const NMAX: usize = 5552;

    /// Length of the large deterministic corpus the split tests run over. The checksums pinned
    /// against the C implementation below are for exactly this length, so it is fixed.
    const CORPUS_LEN: usize = 100_000;

    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, and this
    // module has no `unsafe`, no raw pointers and no uninitialised memory for it to inspect -- the
    // only faults it can surface here are integer overflow and an out-of-bounds index. Both are
    // reached by the cheap tests, which run in full under Miri: the thirty-three pinned vectors
    // cover every branch of `adler32_combine_`, and the sweeps below still cross the `NMAX` block
    // edge from both sides. The three constants that follow, plus the two `cfg_attr(miri, ignore)`
    // attributes on the whole-corpus tests, keep this module's contribution to a shared Miri job
    // proportionate instead of letting a numeric-agreement check dominate it. Nothing is skipped
    // under a normal `cargo test`.

    /// Number of pseudo-random triples the closed-form agreement sweep examines.
    #[cfg(miri)]
    const ROUNDS: usize = 250;
    /// Number of pseudo-random triples the closed-form agreement sweep examines.
    #[cfg(not(miri))]
    const ROUNDS: usize = 20_000;

    /// Corpus lengths swept by `split_identity_holds_across_corpus_lengths`, bracketing the
    /// 16-byte unrolling edge and the `NMAX` block edge from both sides.
    #[cfg(miri)]
    const SWEEP_LENGTHS: &[usize] = &[0, 1, 16, 17, 5_551, 5_552, 5_553];
    /// Corpus lengths swept by `split_identity_holds_across_corpus_lengths`.
    #[cfg(not(miri))]
    const SWEEP_LENGTHS: &[usize] = &[0, 1, 16, 17, 5_551, 5_552, 5_553, 11_104];

    /// Corpus length used by the three-way associativity test, whose property holds at any length.
    #[cfg(miri)]
    const ASSOCIATIVITY_LEN: usize = 3_000;
    /// Corpus length used by the three-way associativity test.
    #[cfg(not(miri))]
    const ASSOCIATIVITY_LEN: usize = 30_000;

    /// Independent reference Adler-32, mirrors `adler32_z` (`adler32.c` lines 61-125).
    ///
    /// Deliberately a second, self-contained implementation rather than a call into the sibling
    /// checksum module: the concatenation tests exist to check [`adler32_combine`] against a
    /// checksum computed the long way, and an oracle that shares no code with the subject is worth
    /// more than one that does. Its own correctness is anchored to values taken from the reference
    /// C library, in `reference_adler32_matches_the_c_implementation`.
    fn reference_adler32(adler: u32, buf: &[u8]) -> u32 {
        // Reducing the seed up front makes the `NMAX` bound hold for the first block exactly as it
        // does for every later one, whatever the caller passes.
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

    /// The deterministic corpus the concatenation tests run over: `buf[i] == (i * 31 + 7) & 0xff`.
    ///
    /// Trivially reproducible in any language, yet varied enough that both halves of an Adler-32
    /// value change with every byte.
    fn corpus(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) & 0xff) as u8).collect()
    }

    /// Widens a corpus length to the signed 64-bit length the C API uses.
    ///
    /// The corpus is a few hundred kilobytes at most, so the conversion is exact; the fallback
    /// keeps the helper total rather than introducing a panicking path into the test support code.
    fn len2_of(n: usize) -> i64 {
        i64::try_from(n).unwrap_or(i64::MAX)
    }

    /// Deterministic 64-bit linear congruential generator, so the sweeps are reproducible without
    /// a dependency -- the crate's `[dependencies]` table is empty by design. Constants are the
    /// ones Knuth gives for MMIX; the low bits of an LCG are poor, so only the high 32 are used.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 32
    }

    /// The closed-form identity from the module documentation, evaluated independently of the code
    /// under test.
    ///
    /// Works in `u64` and reduces with `%` as each term is formed, rather than by conditional
    /// subtraction at the end, so agreement between this and `adler32_combine_` is agreement
    /// between two genuinely different reductions of the same identity.
    ///
    /// Valid only inside the documented contract: `len2` non-negative and all four checksum halves
    /// below `BASE`. Those are exactly the conditions under which the reference implementation's
    /// conditional subtractions are provably sufficient.
    // `s1a`/`s1b` and `s2a`/`s2b` name the low and high halves of the FIRST and SECOND checksum,
    // which is the lettering `adler32.c` L141-L148 uses for the same four quantities. Clippy's
    // `similar_names` reads the one-character difference as accidental; renaming them would break
    // the correspondence with the C derivation this function exists to cross-check.
    #[allow(clippy::similar_names)]
    fn closed_form(adler1: u32, adler2: u32, len2: i64) -> u32 {
        let base = u64::from(BASE);
        let s1a = u64::from(adler1 & 0xffff);
        let s2a = u64::from((adler1 >> 16) & 0xffff);
        let s1b = u64::from(adler2 & 0xffff);
        let s2b = u64::from((adler2 >> 16) & 0xffff);
        // `len2 >= 0` by contract, so this is a width change and never a negation.
        let len = len2.unsigned_abs() % base;

        // s1 = s1a + s1b - 1 (mod BASE). Adding `base` first keeps every intermediate positive.
        let s1 = (s1a + s1b + base - 1) % base;
        // s2 = len2 * s1a + s2a + s2b - len2 (mod BASE), using len2 == len (mod BASE).
        let s2 = ((len * s1a) % base + s2a + s2b + base - len) % base;

        (s1 | (s2 << 16)) as u32
    }

    #[test]
    fn reference_adler32_matches_the_c_implementation() {
        // Anchors for the test oracle itself, all produced by compiling the in-tree `adler32.c`.
        // The empty case must return the seed, `1`, which is also what `adler32(0, NULL, 0)` gives.
        assert_eq!(reference_adler32(1, b""), 0x0000_0001);
        assert_eq!(reference_adler32(1, b"hello"), 0x062c_0215);
        assert_eq!(reference_adler32(1, b", world!"), 0x0b32_0296);
        assert_eq!(reference_adler32(1, b"hello, world!"), 0x21fe_04aa);
    }

    #[test]
    fn combining_with_an_empty_second_sequence_is_the_identity() {
        assert_eq!(adler32_combine(1, 1, 0), 0x0000_0001);

        let hello = reference_adler32(1, b"hello");
        // Appending nothing to `seq1` leaves its checksum alone, ...
        assert_eq!(adler32_combine(hello, 1, 0), hello);
        // ... and appending `seq2` to nothing reproduces `seq2`'s own checksum, which is the case
        // that ties this function to the plain checksum entry point.
        assert_eq!(adler32_combine(1, hello, 5), hello);
        assert_eq!(hello, 0x062c_0215);
    }

    #[test]
    fn negative_length_returns_the_documented_sentinel() {
        // `adler32.c` lines 138-140. `i64::MIN` is included on purpose: it is the value that would
        // break any implementation reaching for a negation or an unchecked narrowing instead of a
        // comparison, and it must return the sentinel rather than panic.
        for len2 in [-1_i64, -2, -65_521, -1_000_000, i64::MIN + 1, i64::MIN] {
            assert_eq!(
                adler32_combine(0x1234_5678, 0x9abc_def0, len2),
                0xffff_ffff,
                "len2 = {len2}"
            );
            assert_eq!(adler32_combine(1, 1, len2), 0xffff_ffff, "len2 = {len2}");
            assert_eq!(adler32_combine(0, 0, len2), 0xffff_ffff, "len2 = {len2}");
        }
    }

    #[test]
    fn length_congruent_to_zero_modulo_base_behaves_like_length_zero() {
        // `rem == 0` here, so `BASE - rem` in step 6 is at its maximum.
        assert_eq!(
            adler32_combine(0x0000_ffff, 0x0000_ffff, 65_521),
            0x0001_000c
        );

        let head = reference_adler32(1, b"hello");
        let tail = reference_adler32(1, b", world!");
        let at_zero = adler32_combine(head, tail, 0);
        assert_eq!(at_zero, 0x115e_04aa);
        for multiple in [65_521_i64, 131_042, 196_563, 65_521 * 1_000] {
            assert_eq!(
                adler32_combine(head, tail, multiple),
                at_zero,
                "len2 = {multiple}"
            );
        }
    }

    #[test]
    fn huge_length_is_reduced_modulo_base() {
        // `(1 << 40) % 65_521 == 57_600`, so this exercises the reduction rather than the raw
        // length; without it the later `BASE - rem` would underflow.
        assert_eq!((1_i64 << 40) % i64::from(BASE), 57_600);
        assert_eq!(
            adler32_combine(0xffff_ffff, 0xffff_ffff, 1 << 40),
            0x6dc1_000c
        );

        let head = reference_adler32(1, b"hello");
        let tail = reference_adler32(1, b", world!");
        assert_eq!(adler32_combine(head, tail, 1 << 32), 0xe501_04aa);
        assert_eq!(adler32_combine(head, tail, 1 << 40), 0xc0bb_04aa);
        assert_eq!(adler32_combine(head, tail, i64::MAX), 0x95d7_04aa);
    }

    #[test]
    fn maximum_internal_product_does_not_overflow() {
        // Step 4 of `adler32_combine_` multiplies `rem <= BASE - 1` by `sum1 <= 0xffff` in `u32`.
        // Checking the bound here turns the comment at that step into a machine-verified fact.
        let widest = u64::from(BASE - 1) * u64::from(0xffff_u32);
        assert_eq!(widest, 4_293_853_200);
        assert!(u32::try_from(widest).is_ok(), "step 4 would overflow u32");

        // Then take that path: `len2 == BASE - 1` maximises `rem` while `0xffff_ffff` maximises
        // both halves, so this call forms the widest product the function can. Overflow checks are
        // on in the debug and test profiles, so a misordered reduction aborts here instead of
        // wrapping unnoticed.
        assert_eq!(
            adler32_combine(0xffff_ffff, 0xffff_ffff, i64::from(BASE - 1)),
            0x000f_000c
        );
    }

    #[test]
    fn matches_the_reference_c_implementation_on_pinned_vectors() {
        // Every expected value below came from compiling the in-tree `adler32.c` and calling
        // `adler32_combine64` directly, then truncating the `uLong` result to 32 bits:
        //
        //     gcc -O2 -D_LARGEFILE64_SOURCE=1 -I. driver.c adler32.c -o driver
        //
        // with `driver.c` printing `(uint32_t)adler32_combine64(adler1, adler2, len2)` per row.
        const HELLO: u32 = 0x062c_0215;
        const WORLD: u32 = 0x0b32_0296;
        const VECTORS: &[(u32, u32, i64, u32)] = &[
            // Two legitimate checksums -- adler32(1, "hello") and adler32(1, ", world!") -- swept
            // across the interesting lengths. The `len2 == 8` row is also a semantic check: it has
            // to equal adler32(1, "hello, world!"), which is 0x21fe_04aa.
            (HELLO, WORLD, 0, 0x115e_04aa),
            (HELLO, WORLD, 1, 0x1372_04aa),
            (HELLO, WORLD, 2, 0x1586_04aa),
            (HELLO, WORLD, 8, 0x21fe_04aa),
            (HELLO, WORLD, 5_552, 0x25c1_04aa),
            (HELLO, WORLD, 65_519, 0x0d36_04aa),
            (HELLO, WORLD, 65_520, 0x0f4a_04aa),
            (HELLO, WORLD, 65_521, 0x115e_04aa),
            (HELLO, WORLD, 65_522, 0x1372_04aa),
            (HELLO, WORLD, 131_041, 0x0f4a_04aa),
            (HELLO, WORLD, 131_042, 0x115e_04aa),
            (HELLO, WORLD, 131_043, 0x1372_04aa),
            (HELLO, WORLD, 1_000_000, 0x9a17_04aa),
            (HELLO, WORLD, 1 << 32, 0xe501_04aa),
            (HELLO, WORLD, 1 << 40, 0xc0bb_04aa),
            (HELLO, WORLD, i64::MAX, 0x95d7_04aa),
            (HELLO, WORLD, -1, 0xffff_ffff),
            (HELLO, WORLD, -2, 0xffff_ffff),
            (HELLO, WORLD, -65_521, 0xffff_ffff),
            (HELLO, WORLD, i64::MIN, 0xffff_ffff),
            // Arguments outside the documented contract. Callers can pass anything, and the
            // masking in steps 3, 5 and 6 is what makes that harmless, so the reference behaviour
            // for such arguments is pinned rather than left undefined by omission.
            (0x0000_0000, 0x0000_0000, 0, 0x0000_fff0),
            (0x0000_0001, 0x0000_0001, 1, 0x0000_0001),
            (0xffff_ffff, 0xffff_ffff, 0, 0x001d_000c),
            (0xffff_ffff, 0xffff_ffff, 1, 0x0029_000c),
            (0xffff_ffff, 0xffff_ffff, 65_520, 0x000f_000c),
            (0xffff_ffff, 0xffff_ffff, 65_521, 0x001d_000c),
            (0xffff_ffff, 0xffff_ffff, i64::MAX, 0x85b9_000c),
            (0xfff0_fff0, 0xfff0_fff0, 0, 0xffef_ffee),
            (0x0000_ffff, 0x0000_ffff, 65_521, 0x0001_000c),
            (0x1234_5678, 0x9abc_def0, -1, 0xffff_ffff),
            // The three witnesses for the module documentation's *Two widths* section: both
            // arguments carry a high half at or above `BASE`, which is the only way the reference
            // implementation's `unsigned long` result can exceed 32 bits. An LP64 reference build
            // returns 0x1_000b_ffef, 0x1_000b_fffd and 0x1_000a_ffef here; the values below are
            // those truncated to 32 bits, which is what this `u32` face is *for* and what a C build
            // whose `unsigned long` is 32 bits wide returns. `the_wide_face_keeps_the_bit_the_narrow
            // _one_drops` below pins the untruncated values, `adler32_combine` is asserted to be the
            // wide face's truncation on every vector in this table, and
            // `crates/zlib-rs-differential/tests/checksum_width.rs` compares the wide face against
            // the C library itself -- so all three widths are now measured rather than described.
            (0xffff_fff0, 0xffff_0000, 1, 0x000b_ffef),
            (0xffff_fff0, 0xffff_ffff, 1, 0x000b_fffd),
            (0xfffe_fff0, 0xffff_0000, 1, 0x000a_ffef),
        ];

        for &(adler1, adler2, len2, expected) in VECTORS {
            assert_eq!(
                adler32_combine(adler1, adler2, len2),
                expected,
                "adler32_combine({adler1:#010x}, {adler2:#010x}, {len2})"
            );
            // The narrow face is defined as the wide one truncated, and nothing else. Asserting it
            // over the whole table is what keeps the two from drifting apart if either body is ever
            // edited on its own.
            assert_eq!(
                adler32_combine(adler1, adler2, len2),
                adler32_combine_wide(adler1, adler2, len2) as u32,
                "the two faces disagree at ({adler1:#010x}, {adler2:#010x}, {len2})"
            );
        }
    }

    /// The wide face keeps the 33rd bit that the checksum-width face necessarily drops.
    ///
    /// These are the three values an LP64 C build returns for the out-of-domain witnesses at the
    /// foot of `VECTORS`, and reproducing them is what the C ABI facade needs in order to answer in
    /// `uLong` without truncating on this target.
    /// `crates/zlib-rs-differential/tests/checksum_width.rs` checks the same three against the
    /// reference library rather than against these literals.
    #[test]
    fn the_wide_face_keeps_the_bit_the_narrow_one_drops() {
        const WIDE: &[(u32, u32, i64, u64)] = &[
            (0xffff_fff0, 0xffff_0000, 1, 0x1_000b_ffef),
            (0xffff_fff0, 0xffff_ffff, 1, 0x1_000b_fffd),
            (0xfffe_fff0, 0xffff_0000, 1, 0x1_000a_ffef),
        ];

        for &(adler1, adler2, len2, expected) in WIDE {
            let wide = adler32_combine_wide(adler1, adler2, len2);
            assert_eq!(
                wide, expected,
                "adler32_combine_wide({adler1:#010x}, {adler2:#010x}, {len2})"
            );
            assert!(
                u32::try_from(wide).is_err(),
                "this witness is only interesting if it exceeds 32 bits"
            );
            assert_eq!(
                adler32_combine(adler1, adler2, len2),
                wide as u32,
                "the narrow face must be the truncation and not a second computation"
            );
        }
    }

    /// Every in-domain argument fits 32 bits, so the two faces are the same number there.
    ///
    /// This is the property that makes the narrow face a legitimate public API rather than a lossy
    /// convenience: within the domain `zlib.h` L1837-L1846 documents, nothing is lost.
    #[test]
    fn the_two_faces_agree_on_every_genuine_checksum() {
        // Both halves of each argument strictly below `BASE`, which is the domain.
        for high1 in [0_u32, 1, 2, BASE - 1, BASE / 2] {
            for low1 in [0_u32, 1, BASE - 1] {
                for high2 in [0_u32, 1, BASE - 1] {
                    for low2 in [0_u32, 1, 2, BASE - 1] {
                        let adler1 = (high1 << 16) | low1;
                        let adler2 = (high2 << 16) | low2;
                        for len2 in [0_i64, 1, 65_520, 65_521, 1 << 32, i64::MAX] {
                            let wide = adler32_combine_wide(adler1, adler2, len2);
                            assert!(
                                u32::try_from(wide).is_ok(),
                                "an in-domain combine must fit 32 bits: \
                                 ({adler1:#010x}, {adler2:#010x}, {len2}) gave {wide:#x}"
                            );
                            assert_eq!(u64::from(adler32_combine(adler1, adler2, len2)), wide);
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "numeric agreement with C; adds no undefined-behaviour coverage"
    )]
    fn corpus_splits_recombine_to_the_whole_checksum() {
        const WHOLE: u32 = 0x76f5_980f;

        let buf = corpus(CORPUS_LEN);
        assert_eq!(reference_adler32(1, &buf), WHOLE);

        // Split points chosen to straddle every structural boundary in the checksum loop: nothing,
        // one byte, the 16-byte unrolling edge, the `NMAX` block edge, and `BASE` itself.
        for split in [
            0_usize, 1, 15, 16, 5_552, 65_521, 65_522, 70_000, CORPUS_LEN,
        ] {
            let head = reference_adler32(1, &buf[..split]);
            let tail = reference_adler32(1, &buf[split..]);
            assert_eq!(
                adler32_combine(head, tail, len2_of(buf.len() - split)),
                WHOLE,
                "split at {split}"
            );
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "numeric agreement with C; adds no undefined-behaviour coverage"
    )]
    fn pinned_split_checksums_match_the_reference_c_implementation() {
        let buf = corpus(CORPUS_LEN);

        // Half-checksums for two of the split points above, taken from the C implementation, so a
        // regression in the test oracle cannot mask a regression in the function under test.
        assert_eq!(reference_adler32(1, &buf[..5_552]), 0x2cf4_cdbf);
        assert_eq!(reference_adler32(1, &buf[5_552..]), 0xe735_ca42);
        assert_eq!(reference_adler32(1, &buf[..65_521]), 0xc817_7f91);
        assert_eq!(reference_adler32(1, &buf[65_521..]), 0x2c7f_187f);

        assert_eq!(
            adler32_combine(0x2cf4_cdbf, 0xe735_ca42, len2_of(CORPUS_LEN - 5_552)),
            0x76f5_980f
        );
        assert_eq!(
            adler32_combine(0xc817_7f91, 0x2c7f_187f, len2_of(CORPUS_LEN - 65_521)),
            0x76f5_980f
        );
    }

    #[test]
    fn split_identity_holds_across_corpus_lengths() {
        // A bounded, deterministic property sweep: for every corpus length and every split point,
        // combining the two half-checksums must reproduce the whole-corpus checksum.
        for &len in SWEEP_LENGTHS {
            let buf = corpus(len);
            let whole = reference_adler32(1, &buf);
            let splits = [
                0,
                1,
                2,
                3,
                len / 4,
                len / 3,
                len / 2,
                len.saturating_sub(1),
                len,
            ];

            for split in splits.into_iter().filter(|&split| split <= len) {
                let head = reference_adler32(1, &buf[..split]);
                let tail = reference_adler32(1, &buf[split..]);
                assert_eq!(
                    adler32_combine(head, tail, len2_of(len - split)),
                    whole,
                    "len = {len}, split at {split}"
                );
            }
        }
    }

    #[test]
    // Same lettering as `closed_form` above, for the same reason: the four halves are named after
    // `adler32.c`'s derivation rather than after Rust's similarity heuristic.
    #[allow(clippy::similar_names)]
    fn agrees_with_the_closed_form_identity() {
        // Deterministic sweep over the documented contract: both arguments are legitimate Adler-32
        // values -- all four halves below `BASE` -- and `len2` is non-negative. This compares the
        // statement sequence below against an independent algebraic reduction of the same
        // identity, so it checks the derivation quoted in the module documentation rather than
        // re-checking the implementation against itself.
        let mut state = 0x0123_4567_89ab_cdef_u64;
        let base = u64::from(BASE);

        for _ in 0..ROUNDS {
            let s1a = lcg(&mut state) % base;
            let s2a = lcg(&mut state) % base;
            let s1b = lcg(&mut state) % base;
            let s2b = lcg(&mut state) % base;
            // 47 bits keeps the length far above `BASE` while staying comfortably positive.
            let len2 = (lcg(&mut state) & 0x7fff_ffff_ffff) as i64;

            let adler1 = (s1a | (s2a << 16)) as u32;
            let adler2 = (s1b | (s2b << 16)) as u32;

            assert_eq!(
                adler32_combine(adler1, adler2, len2),
                closed_form(adler1, adler2, len2),
                "adler1 = {adler1:#010x}, adler2 = {adler2:#010x}, len2 = {len2}"
            );
        }
    }

    #[test]
    fn is_associative_over_three_way_splits() {
        // Concatenation is associative, so combining left-to-right and right-to-left must agree,
        // and both must equal the checksum of the whole. This exercises the function with its own
        // output as input, which the single-split tests never do.
        let len = ASSOCIATIVITY_LEN;
        let buf = corpus(len);
        let whole = reference_adler32(1, &buf);

        // Cut points are expressed as fractions of the length so the property is checked the same
        // way whatever `ASSOCIATIVITY_LEN` is; `first <= second <= len` holds for every entry.
        for &(first, second) in &[
            (0, 0),
            (1, 1),
            (16, len / 4),
            (len / 4, len / 2),
            (len / 3, len),
            (len, len),
        ] {
            let a = reference_adler32(1, &buf[..first]);
            let b = reference_adler32(1, &buf[first..second]);
            let c = reference_adler32(1, &buf[second..]);

            let left = adler32_combine(
                adler32_combine(a, b, len2_of(second - first)),
                c,
                len2_of(buf.len() - second),
            );
            let right = adler32_combine(
                a,
                adler32_combine(b, c, len2_of(buf.len() - second)),
                len2_of(buf.len() - first),
            );

            assert_eq!(left, whole, "left fold, split at {first}/{second}");
            assert_eq!(right, whole, "right fold, split at {first}/{second}");
        }
    }
}
