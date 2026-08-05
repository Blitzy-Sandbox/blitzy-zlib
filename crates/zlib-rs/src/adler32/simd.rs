//! The optional vectorisation-friendly Adler-32 engine -- the same checksum as the scalar
//! path, computed in a data-parallel arrangement of the same integer recurrence.
//!
//! Compiled only when the crate's `simd` feature is enabled, which the parent module gates
//! with `#[cfg(feature = "simd")]`. Nothing here is reachable in the default build, and
//! nothing here is required for correctness: [`Adler32Simd`] and the scalar
//! `Adler32Generic` beside it are interchangeable at every call site, on every input, from
//! every starting value.
//!
//! # What this module may change, and what it may not
//!
//! It may change **speed**. It may change nothing else.
//!
//! An Adler-32 value is the four-byte trailer of every zlib stream
//! (`doc/rfc1950.txt` L318-L329), so it is part of the wire format rather than an internal
//! detail: one divergent bit here becomes a stream that the reference implementation
//! rejects, or a valid stream this implementation refuses. What licenses vectorising it at
//! all is that the checksum is a single scalar however it is computed -- the recurrence is
//! a sum, and summation is associative and commutative, so a different order of
//! accumulation cannot perturb the result. That is emphatically not true of match finding
//! in the compressor, where the order in which candidates are examined decides which
//! match is emitted; vectorised match finding is therefore prohibited, and the two cases
//! must not be conflated.
//!
//! Output neutrality here is verified rather than asserted. The sweep at the bottom of
//! this file compares this backend against the scalar one for every length from `0` to
//! `1_024`, for every reduction boundary up to `100_000` bytes, and for starting values
//! whose halves are deliberately not reduced; the differential suite in
//! `crates/zlib-rs-differential` then re-checks both against the C implementation itself.
//!
//! # Why there is not one intrinsic in this file
//!
//! The crate root forbids the escape hatch from the compiler's memory-safety guarantees,
//! and that prohibition is honoured here in full -- which rules out the entire
//! conventional toolkit for hand-vectorising a loop:
//!
//! * `core::arch` intrinsics are declared unsafe, every one of them;
//! * `#[target_feature(enable = ...)]` makes a function unsafe to call at this MSRV;
//! * the portable `core::simd` API is nightly-only and this workspace pins stable;
//! * reinterpreting `&[u8]` as wider lanes needs a transmute, a raw pointer or a union.
//!
//! What is left is the shape of the code itself, and that turns out to be sufficient. The
//! loop below is written to be vectorisable rather than vectorised -- fixed-size accumulator
//! arrays, a `const` offset table, straight-line arithmetic, no data-dependent branch in the
//! hot path and no early exit -- and at `opt-level = 3`, the release profile this library
//! ships with, that is enough: the emitted x86-64 code for this module contains packed
//! `paddd` accumulation, `punpck` widening of bytes to 32-bit lanes and `pshufd` horizontal
//! reductions. Verified by inspecting the generated assembly, not assumed.
//!
//! The corollary is a maintenance rule. If a future toolchain stops vectorising this, the
//! symptom will be a throughput regression rather than a wrong answer, and the fix is to
//! reshape the loop -- never to reach for an intrinsic, which would move the port's `unsafe`
//! boundary into the algorithmic core and cost far more than the throughput is worth.
//!
//! The upshot is worth stating plainly, because it is what makes this file safe by
//! construction rather than by review: there is no architecture-specific instruction here
//! at all, so this backend computes the right answer on every target -- including targets
//! with no vector unit whatsoever -- and [`is_supported`] is a throughput hint, never a
//! correctness gate.
//!
//! # The kernel identity
//!
//! The scalar recurrence walks one byte at a time (`adler32.c` L14, the `DO1` macro):
//!
//! ```text
//! sum1 += b_j;
//! sum2 += sum1;
//! ```
//!
//! Unrolling that over a run of `n` bytes `b_0 .. b_(n-1)` gives a closed form. After the
//! `j`-th byte the first sum is `sum1 + sum_(i<=j) b_i`, and `sum2` accumulates every one of
//! those `n` partial sums, so each byte `b_i` is counted once for each `j` in `i..n` --
//! that is, `n - i` times:
//!
//! ```text
//! sum1' = sum1 + sum_(j<n) b_j
//! sum2' = sum2 + n * sum1 + sum_(j<n) (n - j) * b_j
//! ```
//!
//! This is an exact identity over the integers. No rounding, no truncation and no reduction
//! is introduced by using it, which is precisely why it may be used at all.
//!
//! Both right-hand sides depend on the *pre-run* value of `sum1`, and folding the new `sum1`
//! into `sum2` instead is the one mistake this arrangement invites. The kernel therefore
//! never mutates in place: `fold_lanes` returns the pair
//! `(sum1 + S, sum2 + n * sum1 + W)`, in which `sum1` is structurally the incoming value, so
//! the ordering requirement cannot be violated by a later edit.
//!
//! # The lane decomposition, which is what makes it fast
//!
//! Applying the closed form once per 16-byte sub-chunk would already be correct, but it
//! would also be *slower than the scalar path* -- measured, not assumed: the two sums it
//! needs are horizontal reductions, and paying for two of them every sixteen bytes costs
//! more than the two dependent adds per byte it replaces. The arrangement below pays for
//! them once per `NMAX` block instead, and that is the whole difference between this backend
//! being worth compiling and being dead weight.
//!
//! Number the whole sub-chunks of a block `t = 0 .. m-1` and the byte offsets within one
//! sub-chunk `i = 0 .. k-1`, so byte `b_(t,i)` sits at position `j = k*t + i` of the
//! `n = k*m` byte prefix. Two arrays of `k` accumulators -- one *lane* per offset -- are
//! carried across the whole block:
//!
//! ```text
//! for each sub-chunk t:
//!     carried[i] += totals[i]        // for every lane i, before the byte lands
//!     totals[i]  += b_(t,i)
//! ```
//!
//! so that at the end of the block `totals[i] = sum_t b_(t,i)` and
//! `carried[i] = sum_t (m - 1 - t) * b_(t,i)`: each lane's running total is added once more
//! for every sub-chunk that follows it. Substituting `j = k*t + i` and `n = k*m` into the
//! closed form turns the two sums it needs into three horizontal reductions over those
//! arrays:
//!
//! ```text
//! S = sum_i totals[i]                                   the block's byte sum
//! C = sum_i carried[i]
//! P = sum_i i * totals[i]                               the within-sub-chunk offsets
//!
//! W = sum_(j<n) (n - j) * b_j = k * (C + S) - P
//! ```
//!
//! The derivation is three lines: `n - j` is `k*(m - t) - i`; splitting that gives
//! `k * sum (m - t) * b - sum i * b`; and `sum (m - t) * b` is `C + S` because
//! `m - t = (m - 1 - t) + 1`. The identity is exact over the integers, so `W` is the very
//! same number the byte-at-a-time recurrence would accumulate.
//!
//! What this buys is that the inner loop becomes `k` independent accumulator chains of plain
//! adds -- no multiply, no reduction, no dependence between lanes -- which is what a
//! vectoriser wants, and what an out-of-order core pipelines even when the vectoriser
//! declines. Measured on this machine against the scalar backend on identical data: about
//! `3.0x` from 4 KiB upwards, `2.5x` at 512 bytes and `1.9x` at 256. Below
//! [`LANE_THRESHOLD_LEN`] the fixed cost is not recovered, so the call is handed to the scalar
//! backend instead and this backend is never the slower of the two. A 32- and a 64-lane
//! variant were also measured and were slower at *every* size, so the lane count is 16.
//!
//! Two implementation details are load-bearing for those numbers, and both are recorded where
//! they appear: the lane state must not be merged into the running sums until the block is
//! finished, and `accumulate_block` and `fold_lanes` must carry `#[inline(always)]` rather
//! than the ordinary hint -- with a hint, the optimiser leaves the lane arrays behind a call
//! boundary and the kernel measures about `0.8x`, slower than the code it replaces.
//!
//! # Block structure and where the reductions happen
//!
//! Reduction placement is observable, so it is not rearranged. The engine mirrors
//! `adler32.c` L97-L121 exactly: the input is cut into blocks of at most `NMAX` bytes, each
//! block is accumulated without any reduction, and *both* halves are then reduced modulo
//! `BASE` -- the two `MOD`s at `adler32.c` L104-L105 for a full block and at L119-L120 for
//! the final partial one. `NMAX` is `5_552` = `16 * 347`, so a full block is an exact number
//! of 16-byte sub-chunks and only the last block can leave a tail; that tail is consumed one
//! byte at a time, exactly as the C source consumes it at L115-L118.
//!
//! The two short paths are not re-derived here at all. They are delegated to the scalar
//! module, because their reduction schedules differ from the block engine's and from each
//! other -- `adler32.c` L70-L78 reduces both halves by a single conditional subtraction,
//! while L85-L94 reduces `sum1` by subtraction and `sum2` by `%`. Re-implementing either
//! would be a standing invitation to a one-value divergence that only shows up for a
//! starting value whose halves are at or above `BASE`.
//!
//! # Why `u32` accumulators cannot overflow
//!
//! `NMAX` is the largest block length for which 32-bit accumulation is provably safe: the
//! largest `n` with `255 * n * (n + 1) / 2 + (n + 1) * (BASE - 1) <= 2^32 - 1`
//! (`adler32.c` L11-L12). The worst case is the very first block, entered from the largest
//! value a split can produce -- both halves `65_535` -- and fed exactly `NMAX` bytes of
//! `0xff`:
//!
//! ```text
//! sum2_peak = 65_535 + 5_552 * 65_535 + 255 * (5_552 * 5_553 / 2) = 4_294_773_495
//! u32::MAX                                                        = 4_294_967_295
//! margin                                                          =       193_800
//! ```
//!
//! `sum1` peaks at `65_535 + 5_552 * 255` = `1_481_295` and is never in danger.
//!
//! The lane arrangement must be shown not to raise that ceiling, because it forms
//! intermediates the byte-at-a-time path never does. With `m` = `NMAX / 16` = `347`
//! sub-chunks of `0xff` bytes -- the worst case -- each quantity is bounded as follows, and
//! each bound is re-checked numerically by `lane_accumulators_and_the_fold_fit_in_u32`:
//!
//! ```text
//! totals[i]  <= 347 * 255                       =        88_485
//! carried[i] <= 255 * (346 * 347 / 2)           =    15_307_905
//! S          <= 16 * 88_485                     =     1_415_760
//! C          <= 16 * 15_307_905                 =   244_926_480
//! k * (C + S)                                   = 3_941_475_840   <- largest intermediate
//! P          <= 120 * 88_485                    =    10_618_200
//! W = k * (C + S) - P                           = 3_930_857_640
//! n * sum1   <= 5_552 * 65_535                  =   363_850_320
//! sum2 + n * sum1 + W                           = 4_294_773_495   <- the same peak as above
//! ```
//!
//! `W` cannot underflow either: `P <= (k - 1) * S`, which is strictly below `k * (C + S)`.
//!
//! **The order of the final fold is load-bearing.** `W` must be formed *before* it meets
//! `sum2`. Adding `k * (C + S)` to `sum2 + n * sum1` and subtracting `P` afterwards computes
//! the same value mathematically but reaches `4_305_391_695` on the way -- `10_424_400` past
//! `u32::MAX`. That is why `W` is a separate binding in `fold_lanes` rather than an inline
//! subexpression, and why the parentheses there are not decoration.
//!
//! Two further consequences follow, and both are load-bearing too. A block must never exceed
//! `NMAX` bytes before being reduced, however tempting larger blocks look for throughput: the
//! headroom is `193_800`, which is `0.0045%` of the range. And every addition below is plain
//! `+`, never `wrapping_add`: the bound proves overflow impossible, so wrapping arithmetic
//! could only serve to hide a future mistake that a debug build would otherwise catch
//! outright.
//!
//! # Correspondence with the reference implementation
//!
//! | This module | `adler32.c` |
//! |---|---|
//! | `POSITIONS`, `fold_lanes` | L14-L18, the `DO1`..`DO16` accumulation, re-expressed |
//! | `accumulate_bytes` | L86-L89 and L115-L118, the byte-at-a-time loops |
//! | `accumulate_block` | L100-L118, one `NMAX`-bounded block |
//! | `adler32_blocks` | L97-L121, the block loop and its two reduction points |
//! | [`Adler32Simd::checksum`] | L61-L125 as a whole, dispatch order included |
//!
//! # The backend contract
//!
//! [`Adler32Simd`] implements the parent module's `Adler32Backend` trait and nothing else;
//! selection between backends belongs to the parent, which owns the dispatch and calls
//! [`is_supported`] to make it. The contract is:
//!
//! ```text
//! pub trait Adler32Backend {
//!     /// Updates a running Adler-32 checksum with `buf` and returns the new value.
//!     fn checksum(adler: u32, buf: &[u8]) -> u32;
//! }
//! ```
//!
//! [`Adler32Simd`] is public, and deliberately so: the checksum benchmark measures the
//! scalar and vectorised backends against each other from outside this crate, which it can
//! only do if it can name both. Every kernel helper stays private.
//!
//! # Layering and safety posture
//!
//! The module names `core` only -- plus, in exactly one cfg-gated helper, the standard
//! library's target-feature detection macro, which is safe to call. It allocates nothing,
//! holds no raw pointer, declares neither a C-visible layout nor C-visible linkage for
//! anything, and cannot panic on any input: there is no indexing, no slicing by range, no
//! `unwrap`, and -- as proved above -- no arithmetic that can overflow. Exporting the
//! C-visible `adler32` and `adler32_z` symbols is the business of the `libz-rs-sys` facade.
//!
//! # Examples
//!
//! ```ignore
//! // Interchangeable with the scalar backend, which is the whole point.
//! assert_eq!(
//!     Adler32Simd::checksum(1, b"hello, hello!"),
//!     Adler32Generic::checksum(1, b"hello, hello!"),
//! );
//!
//! // And identical to the reference implementation's own answer.
//! assert_eq!(Adler32Simd::checksum(1, b"hello"), 0x062c_0215);
//!
//! // Resumable across arbitrary splits, because the running value is the whole state.
//! let staged = Adler32Simd::checksum(Adler32Simd::checksum(1, b"hel"), b"lo");
//! assert_eq!(staged, Adler32Simd::checksum(1, b"hello"));
//!
//! // A hint, never a gate: the answer below changes nothing about the values above.
//! let _profitable: bool = is_supported();
//! ```

use super::generic;
use super::{Adler32Backend, BASE, NMAX};

// -----------------------------------------------------------------------------
//  Loop shape constants
// -----------------------------------------------------------------------------

/// Byte offset of each lane within a sub-chunk: `i` for `i` in `0..k`.
///
/// These are the coefficients of `P` in the lane decomposition -- byte `b_(t,i)` sits `i`
/// bytes into its sub-chunk, and the closed form has to discount it by exactly that -- so
/// the table is part of the identity rather than a tuning parameter.
///
/// Held as `const` data for two reasons. It lets the compiler materialise a constant vector
/// instead of deriving `i` per element, and it removes the one narrowing conversion this
/// kernel would otherwise need: `i` is naturally a `usize` index while the accumulators are
/// `u32`, and a cast between them is exactly the kind of silent-truncation risk a port like
/// this one has no reason to introduce.
const POSITIONS: [u32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Number of bytes one vectorisable step of the block loop consumes -- the lane count `k`.
///
/// Derived from [`POSITIONS`] rather than written twice, so the two cannot drift apart: the
/// offset table *is* the step, and a table of a different length would change this constant
/// with it.
///
/// Sixteen matches the `DO16` grouping of the reference implementation (`adler32.c` L18)
/// and, more importantly, divides `NMAX`: `5_552` = `16 * 347`, the iteration count `n` at
/// `adler32.c` L99. A full block is therefore an exact number of steps and leaves no tail,
/// which is what keeps the tail handling confined to the final partial block. Sixteen bytes
/// is also one 128-bit vector register, so the accumulation the compiler is being invited to
/// widen is the natural width on every Tier-1 target -- and it measured fastest: 32 and 64
/// lanes were both slower at every input size, as recorded in the module documentation.
const SUB_CHUNK: usize = POSITIONS.len();

/// [`SUB_CHUNK`] as the accumulators' own type: the `k` of the lane decomposition, both the
/// multiplier applied to `C + S` and the number of bytes each sub-chunk contributes to `n`.
///
/// Stated as its own `u32` constant rather than converted from the `usize` length, because a
/// `usize`-to-`u32` conversion is a narrowing one on a 16-bit target and this kernel has no
/// business performing narrowing arithmetic. The compile-time assertion below pins it to
/// [`POSITIONS`], and `sub_chunk_weight_is_the_sub_chunk_length` re-checks the same identity
/// against [`SUB_CHUNK`] directly.
const SUB_CHUNK_WEIGHT: u32 = 16;

/// Length below which the block engine is bypassed for the scalar short path.
///
/// The `len < 16` test at `adler32.c` L85. It is one constant with [`SUB_CHUNK`] rather
/// than a coincidence: the short path exists precisely to serve inputs too small to fill
/// one step of the block loop, and the scalar module spells the same relationship the same
/// way.
const SHORT_INPUT_LEN: usize = SUB_CHUNK;

/// Length below which the whole call is handed to the scalar backend instead.
///
/// The lane arrangement amortises three horizontal reductions and a multiply over a block, so
/// on a very short input it pays that cost without recovering it. Measured on this machine,
/// this backend against the scalar one on the same data:
///
/// ```text
/// bytes   16    32    48    64    96   128   256   512   1024   4096+
/// ratio 0.44  0.68  0.85  1.03  1.34  1.51  1.95  2.47   2.73    3.00
/// ```
///
/// Four sub-chunks is where the curve crosses one, so below that the call is delegated and
/// this backend is never the slower of the two at any length -- which is what makes selecting
/// it a free decision for the parent module rather than a trade-off.
///
/// Delegation is not a behavioural fork: the two backends agree bit for bit on every input and
/// every starting value, which is the property the equivalence sweep at the bottom of this file
/// establishes for exactly the lengths that straddle this threshold.
const LANE_THRESHOLD_LEN: usize = 4 * SUB_CHUNK;

// -----------------------------------------------------------------------------
//  Compile-time contract pins
// -----------------------------------------------------------------------------

/// The overflow argument in the module documentation is arithmetic about specific numbers,
/// not a general property, so the numbers it assumes are pinned here. Changing either
/// constant in the parent module now fails the build instead of quietly invalidating the
/// proof and, with it, the guarantee that this kernel cannot overflow a `u32`.
const _: () = assert!(
    BASE == 65_521,
    "the overflow bound proved in this module assumes BASE == 65_521"
);
const _: () = assert!(
    NMAX == 5_552,
    "the overflow bound proved in this module assumes NMAX == 5_552"
);

/// A block must be consumed by whole sub-chunks with nothing left over, which is what lets
/// the tail handling be reasoned about as belonging to the final partial block alone.
const _: () = assert!(
    NMAX % SUB_CHUNK == 0,
    "NMAX must be divisible by SUB_CHUNK, as `adler32.c` L99 requires of DO16"
);

/// The lane threshold must sit above the scalar short path, or the dispatch order in
/// [`Adler32Simd::checksum`] would make it unreachable and the delegation it exists to perform
/// would silently stop happening.
const _: () = assert!(
    LANE_THRESHOLD_LEN > SHORT_INPUT_LEN,
    "LANE_THRESHOLD_LEN must exceed SHORT_INPUT_LEN to be reachable"
);

/// The lane offsets must be `0 .. k` in order, and `SUB_CHUNK_WEIGHT` must be that same `k`.
/// Pinning both here makes a mistyped table or a mismatched multiplier a build error rather
/// than a wrong checksum.
const _: () = assert!(
    matches!(POSITIONS.first(), Some(&0)),
    "the first lane sits at offset 0"
);
const _: () = assert!(
    matches!(POSITIONS.last(), Some(&last) if last + 1 == SUB_CHUNK_WEIGHT),
    "SUB_CHUNK_WEIGHT must be the lane count, so the last lane offset is one below it"
);

// -----------------------------------------------------------------------------
//  The vectorisable kernel
// -----------------------------------------------------------------------------

/// Fold a block's lane accumulators into the component sums, reducing neither.
///
/// The closed form from the module documentation, evaluated once per block from the three
/// horizontal reductions the lane arrays support:
///
/// ```text
/// S = sum_i totals[i]              C = sum_i carried[i]        P = sum_i i * totals[i]
/// W = k * (C + S) - P              sum1' = sum1 + S            sum2' = sum2 + n * sum1 + W
/// ```
///
/// `consumed` is the `n` of that identity: the number of bytes the lane loop actually folded,
/// which is `k` per whole sub-chunk and therefore at most `NMAX`. It is passed in rather than
/// recomputed here so that this function makes no assumption about how many sub-chunks were
/// processed.
///
/// Written as a single returned pair on purpose: both new values are expressed in terms of
/// the *incoming* `sum1`, so the requirement that `sum1` still hold its pre-block value when
/// multiplied by `n` is enforced by the structure of the expression rather than by the order
/// of two assignments.
///
/// # Why `weighted` is its own binding
///
/// It is the one line in this module where evaluation order is a correctness question rather
/// than a style one. `W` peaks at `3_930_857_640` and `k * (C + S)` -- the value it is
/// computed from -- at `3_941_475_840`; the final `sum2` peaks at `4_294_773_495`, only
/// `193_800` below `u32::MAX`. Forming `W` first keeps every intermediate inside that
/// envelope, whereas letting `k * (C + S)` meet `sum2 + n * sum1` before `P` is subtracted
/// reaches `4_305_391_695` and overflows. See the module documentation for the full bound
/// table; `lane_accumulators_and_the_fold_fit_in_u32` checks every number in it.
///
/// # No underflow
///
/// `P <= (k - 1) * S` because each lane's offset is below `k`, and `(k - 1) * S < k * (C + S)`
/// because `C` is non-negative, so the subtraction is always in range.
// `#[inline(always)]` rather than the ordinary hint, and the difference is not cosmetic.
// With `#[inline]` the optimiser declines to inline this, the lane arrays stay behind a call
// boundary, the widening the loop shape was written for never happens, and the kernel measures
// about 0.8x the scalar backend -- slower than the code it exists to replace. With the
// directive it measures about 3x. Both numbers were taken on this machine against the scalar
// backend on identical data; re-measure before weakening this to a hint.
#[allow(clippy::inline_always)]
#[inline(always)]
#[must_use]
fn fold_lanes(
    sum1: u32,
    sum2: u32,
    consumed: u32,
    totals: &[u32; SUB_CHUNK],
    carried: &[u32; SUB_CHUNK],
) -> (u32, u32) {
    let byte_total: u32 = totals.iter().sum();
    let carried_total: u32 = carried.iter().sum();
    let positional: u32 = totals
        .iter()
        .zip(POSITIONS.iter())
        .map(|(&total, &offset)| offset * total)
        .sum();

    // `W`, formed before it can meet `sum2` -- see above. Nothing here may be inlined into
    // the expression below.
    let weighted = SUB_CHUNK_WEIGHT * (carried_total + byte_total) - positional;

    (sum1 + byte_total, sum2 + consumed * sum1 + weighted)
}

/// Accumulate bytes one at a time into the component sums, reducing neither.
///
/// The literal `DO1` recurrence (`adler32.c` L14) as the C source runs it at L86-L89 and
/// L115-L118. It serves the sub-chunk tail of a partial block, where a vector step would
/// have to be masked to be correct and the residue is at most fifteen bytes, so the scalar
/// form is both simpler and no slower.
///
/// # Contract
///
/// The caller must ensure that this call, together with everything else accumulated into
/// `sum1` and `sum2` since the last reduction, spans no more than `NMAX` bytes. The overflow
/// argument in the module documentation rests on exactly that.
#[inline]
#[must_use]
fn accumulate_bytes(mut sum1: u32, mut sum2: u32, bytes: &[u8]) -> (u32, u32) {
    for &byte in bytes {
        sum1 += u32::from(byte);
        sum2 += sum1;
    }

    (sum1, sum2)
}

/// Accumulate one block into the component sums, reducing neither.
///
/// Whole sub-chunks through the lane accumulators, then the fewer-than-`SUB_CHUNK` tail one
/// byte at a time -- the same two-stage shape the C source uses at `adler32.c` L110-L118, and
/// the same one the scalar module uses. Since `NMAX` is divisible by [`SUB_CHUNK`], a full
/// block leaves no tail and only the final partial block can reach the byte loop. A block with
/// no whole sub-chunk at all -- which the last block of an input can be -- folds nothing
/// through the lanes and everything through the tail, with no special case needed: the
/// reductions over zeroed lanes contribute exactly zero.
///
/// # The hot loop
///
/// Everything about the loop body is chosen so that a compiler may widen it: the accumulators
/// are two fixed-size arrays rather than scalars, the lanes are independent of one another, and
/// there is no multiply and no reduction inside it. Its one branch is the length check below,
/// on a slice whose length `chunks_exact` fixes at a compile-time constant, so the optimiser
/// folds it away rather than testing it per sub-chunk. The lane state is deliberately *not*
/// merged into `sum1`/`sum2` until the block is finished, because that merge is where the
/// multiply and the three horizontal reductions live, and paying for them per sub-chunk is
/// exactly what made an earlier arrangement of this kernel slower than the scalar path it
/// exists to beat.
///
/// # The `else` arm
///
/// `first_chunk` cannot fail for a sub-chunk produced by `chunks_exact(SUB_CHUNK)`, so the
/// fallback is unreachable. It is written to be exactly correct rather than merely to satisfy
/// the type system: it abandons the lane arrangement for this block and folds the block byte
/// at a time from the *incoming* sums -- which is exact for any length, because the lane
/// state is local and has not yet been merged into anything. Answering an unexpected length by
/// skipping the sub-chunk, which is the shape an `if let` with no `else` would produce, would
/// silently corrupt the checksum, and this engine has no business containing such a branch.
///
/// # Contract
///
/// `block` must be no longer than `NMAX` bytes. That is the entire reason the outer loop in
/// `adler32_blocks` exists, and the overflow argument depends on it.
// `#[inline(always)]` rather than the ordinary hint, and the difference is not cosmetic.
// With `#[inline]` the optimiser declines to inline this, the lane arrays stay behind a call
// boundary, the widening the loop shape was written for never happens, and the kernel measures
// about 0.8x the scalar backend -- slower than the code it exists to replace. With the
// directive it measures about 3x. Both numbers were taken on this machine against the scalar
// backend on identical data; re-measure before weakening this to a hint.
#[allow(clippy::inline_always)]
#[inline(always)]
#[must_use]
fn accumulate_block(sum1: u32, sum2: u32, block: &[u8]) -> (u32, u32) {
    // One accumulator pair per byte offset within a sub-chunk, zeroed for every block.
    // `totals[i]` sums the bytes that land in lane `i`; `carried[i]` sums the lane's running
    // total once for every later sub-chunk, which is the `m - 1 - t` weighting of the
    // decomposition.
    let mut totals = [0_u32; SUB_CHUNK];
    let mut carried = [0_u32; SUB_CHUNK];

    // Bytes folded through the lanes, which is the `n` the closed form needs. Accumulated
    // rather than derived from the block length so that it counts what was actually folded;
    // it is at most `NMAX`, so it cannot overflow.
    let mut consumed: u32 = 0;

    // Borrowed rather than consumed, so that `remainder` below is still reachable. The
    // iterator computes its remainder up front, so this is not a second pass over the data.
    let mut sub_chunks = block.chunks_exact(SUB_CHUNK);
    for sub_chunk in &mut sub_chunks {
        let Some(fixed) = sub_chunk.first_chunk::<SUB_CHUNK>() else {
            return accumulate_bytes(sum1, sum2, block);
        };

        for ((carry, total), &byte) in carried.iter_mut().zip(totals.iter_mut()).zip(fixed.iter()) {
            // `carried` must take the total from *before* this byte lands, which is what
            // makes it count each earlier sub-chunk exactly once.
            *carry += *total;
            *total += u32::from(byte);
        }

        consumed += SUB_CHUNK_WEIGHT;
    }

    let (sum1, sum2) = fold_lanes(sum1, sum2, consumed, &totals, &carried);

    // `adler32.c` L115-L118: the fewer-than-16-byte residue, in order, after the lanes.
    accumulate_bytes(sum1, sum2, sub_chunks.remainder())
}

/// Update a checksum with a buffer of any length, as reached for inputs of at least
/// [`LANE_THRESHOLD_LEN`] bytes.
///
/// Port of the block engine at `adler32.c` L97-L121: the `while (len >= NMAX)` loop that
/// consumes full blocks and the `if (len)` tail that consumes what remains, with both halves
/// reduced after each.
///
/// # Equivalence with the C control flow
///
/// Cutting the input into `NMAX`-sized chunks reproduces it exactly:
///
/// * a chunk of exactly `NMAX` bytes is consumed by whole sub-chunks with no tail and is
///   then reduced -- the loop body at `adler32.c` L98-L105;
/// * the final, shorter chunk is consumed by whole sub-chunks followed by a byte loop and is
///   then reduced -- the tail at `adler32.c` L110-L120;
/// * when the length is an exact multiple of `NMAX`, C's `if (len)` guard at L109 suppresses
///   a further reduction, and chunking agrees, because it never yields an empty final chunk.
///
/// The reductions are `%`, matching the `MOD` macro in the shipped configuration
/// (`adler32.c` L55-L57); the `NO_DIVIDE` variant at L36-L40 computes the same residues by
/// shift and subtract and is not the configuration this port reproduces.
#[must_use]
fn adler32_blocks(adler: u32, buf: &[u8]) -> u32 {
    let (mut sum1, mut sum2) = generic::split(adler);

    for block in buf.chunks(NMAX) {
        let (raw_sum1, raw_sum2) = accumulate_block(sum1, sum2, block);
        // The two `MOD`s at `adler32.c` L104-L105 and L119-L120. Both halves, every block,
        // never later: `NMAX` is exactly as far as a `u32` can carry the accumulation.
        sum1 = raw_sum1 % BASE;
        sum2 = raw_sum2 % BASE;
    }

    generic::combine_halves(sum1, sum2)
}

// -----------------------------------------------------------------------------
//  The backend
// -----------------------------------------------------------------------------

/// The vectorisation-friendly Adler-32 backend: the same checksum as the scalar path,
/// arranged so that a compiler can compute sixteen bytes at a time.
///
/// Available on every target -- it contains no architecture-specific instruction -- and
/// selected by the parent module when [`is_supported`] reports that the arrangement is
/// likely to pay for itself. Substituting it for the scalar backend, or the scalar backend
/// for it, cannot change any value this library produces.
///
/// This type carries no state. It exists so that a backend can be named: by the dispatcher
/// in the parent module, by the benchmark that compares scalar against vectorised
/// throughput, and by the equivalence tests, which must be able to pin a computation to one
/// specific implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Adler32Simd;

impl Adler32Backend for Adler32Simd {
    /// Update a running Adler-32 checksum with `buf` and return the new value.
    ///
    /// Dispatches as `adler32_z` does (`adler32.c` L61-L125), and in the same order, because
    /// its paths do not reduce alike and the order is therefore behaviour rather than style.
    /// One path is added to the C source's three, and it is a throughput decision only:
    ///
    /// 1. One byte -- delegated to the scalar module's port of the fast path at
    ///    `adler32.c` L70-L78, which reduces both halves by a single conditional
    ///    subtraction and can legitimately return a high half that is not fully reduced.
    /// 2. Fewer than [`SHORT_INPUT_LEN`] bytes, the empty slice included -- delegated to the
    ///    scalar module's port of `adler32.c` L85-L94, which reduces `sum1` by subtraction
    ///    but `sum2` by `%`.
    /// 3. Fewer than [`LANE_THRESHOLD_LEN`] bytes -- the scalar backend's own block engine,
    ///    because the lane arrangement does not recover its fixed cost on an input that
    ///    short. This is a throughput choice with no behavioural content: the two backends
    ///    agree bit for bit, as the sweep over every length either side of the threshold
    ///    shows.
    /// 4. Everything longer -- the vectorisable block engine above, `adler32.c` L97-L121.
    ///
    /// Only path 4 is implemented here. Paths 1 and 2 are delegated rather than duplicated
    /// on purpose: they are a handful of lines each, but they are the lines whose reduction
    /// schedule is asymmetric, and a second copy of them is the most plausible way for this
    /// backend to drift from the scalar one on some starting value nobody thought to try.
    ///
    /// Between paths 1 and 2 the C source tests `buf == Z_NULL` and returns `1L`
    /// (`adler32.c` L81-L82). A `&[u8]` cannot be null, so there is nothing to port at that
    /// position; the facade crate answers a null pointer with the initial value without ever
    /// calling in here. An **empty slice is not `Z_NULL`** -- it takes path 2 and returns the
    /// incoming value normalised, so `checksum(0, &[])` is `0` and not `1`.
    #[inline]
    fn checksum(adler: u32, buf: &[u8]) -> u32 {
        // `adler32.c` L70: in case the user likes doing a byte at a time, keep it fast.
        if let [byte] = *buf {
            return generic::adler32_len_1(adler, byte);
        }

        // `adler32.c` L85: in case short lengths are provided, keep it somewhat fast. Also
        // the empty slice, and every length a single vector step could not fill.
        if buf.len() < SHORT_INPUT_LEN {
            return generic::adler32_short(adler, buf);
        }

        // Too short for the lane arrangement to pay for itself. The scalar backend runs the
        // same reduction schedule and returns the same value; see `LANE_THRESHOLD_LEN`.
        if buf.len() < LANE_THRESHOLD_LEN {
            return generic::Adler32Generic::checksum(adler, buf);
        }

        // `adler32.c` L97-L121: the block engine, sixteen bytes at a step.
        adler32_blocks(adler, buf)
    }
}

// -----------------------------------------------------------------------------
//  Backend selection
// -----------------------------------------------------------------------------

/// Report whether this backend is expected to be profitable on the running target.
///
/// # This is a performance hint and never a correctness gate
///
/// The point bears repeating because the usual arrangement for a vectorised routine is the
/// opposite one. Elsewhere, a detection call guards code that would execute an illegal
/// instruction on hardware lacking a feature, so getting the answer wrong is fatal. Here it
/// cannot be: this module contains no architecture-specific instruction at all, only
/// ordinary integer arithmetic shaped so that a compiler may vectorise it. Both answers
/// therefore select a backend that computes the identical checksum, and a `simd`-enabled
/// build runs correctly on hardware with no vector unit whatsoever -- which is how the
/// requirement that it do so is met without a single line of unsafe code. The equivalence
/// sweep below is the proof: it compares the two backends directly, not through whatever
/// this function happens to return.
///
/// What the answer is *for* is throughput. The parent module uses it to avoid paying for the
/// sixteen-byte arrangement -- a multiply and two extra accumulators per step -- on a target
/// where nothing would come back in return.
///
/// # How the answer is reached
///
/// * On x86 and x86-64 with the `std` feature, by run-time detection of `SSE2`, the
///   instruction set that gives one 128-bit register per sub-chunk. Detection at run time is
///   what allows one binary to be built once and still make the right choice on each machine
///   it lands on. The macro that performs it is safe to call and needs no escape hatch.
/// * On x86 and x86-64 without `std`, by the compile-time answer instead, since run-time
///   detection lives in the standard library. A `no_std` build for a target whose baseline
///   already includes `SSE2` -- as x86-64's does -- still answers `true`.
/// * On every other architecture, `true`. The kernel is portable safe Rust, so there is
///   nothing to detect: either the compiler vectorises it, or it emits the same scalar
///   accumulation the other backend would.
///
/// # What this function deliberately does not do
///
/// It does not read the environment. `ZLIB_RS_SIMD` is a build-time toggle consumed by the
/// facade crate's build script; it reaches this module only by enabling or disabling the
/// `simd` feature, and a library that inspected the environment on a hot path would be both
/// slower and less predictable than one that did not.
#[must_use]
pub fn is_supported() -> bool {
    vector_unit_available()
}

/// Binds the standard library inside this module, for the one configuration that needs it.
///
/// Run-time feature detection lives in `std`, and this is the only item in the crate's
/// algorithmic core that reaches for it. Declaring the dependency here rather than relying on
/// the crate root is deliberate: the core is `no_std` by default, and this binding makes the
/// helper below resolve regardless of how the root spells its conditional `no_std` attribute.
/// It is scoped by the same `cfg` as its single user, so no other configuration -- and in
/// particular no `no_std` build -- links `std` on account of it.
#[cfg(all(feature = "std", any(target_arch = "x86", target_arch = "x86_64")))]
extern crate std;

/// Run-time detection of a vector unit wide enough for one sub-chunk, on x86 with `std`.
///
/// `is_x86_feature_detected!` expands to a cached `CPUID` query. It is a safe macro -- what
/// requires the unsafe escape hatch is *calling* an intrinsic once a feature is known to be
/// present, and this module never calls one.
#[cfg(all(feature = "std", any(target_arch = "x86", target_arch = "x86_64")))]
fn vector_unit_available() -> bool {
    std::arch::is_x86_feature_detected!("sse2")
}

/// Compile-time detection of the same feature, for an x86 build without `std`.
///
/// `cfg!` answers from the target features the crate was compiled for, which is the best
/// available answer when the run-time detection machinery is not linked in. It is exact
/// rather than conservative for x86-64, whose baseline mandates `SSE2`.
#[cfg(all(not(feature = "std"), any(target_arch = "x86", target_arch = "x86_64")))]
fn vector_unit_available() -> bool {
    cfg!(target_feature = "sse2")
}

/// The answer for every non-x86 architecture: yes.
///
/// There is no instruction set to probe, because there is no architecture-specific
/// instruction in this module. Whether the arrangement is actually turned into vector code
/// is the compiler's decision, and on a target with no vector unit the fallback is the same
/// scalar accumulation the other backend performs.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn vector_unit_available() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::generic::Adler32Generic;
    use super::{
        accumulate_block, accumulate_bytes, fold_lanes, is_supported, Adler32Backend, Adler32Simd,
        LANE_THRESHOLD_LEN, POSITIONS, SHORT_INPUT_LEN, SUB_CHUNK, SUB_CHUNK_WEIGHT,
    };
    use super::{BASE, NMAX};
    use alloc::{format, vec, vec::Vec};

    /// Length of the deterministic corpus the sweeps run over.
    const CORPUS_LEN: usize = 100_000;

    /// Adler-32 of the whole corpus, started from RFC 1950's initial value.
    const CORPUS_CHECKSUM: u32 = 0x76f5_980f;

    /// Starting values every sweep is repeated for.
    ///
    /// The first two are the values callers actually supply -- `1` is RFC 1950's seed and `0`
    /// the one `inflate` uses for a raw stream. `0x1234_5678` and `0x062c_0215` (the checksum
    /// of `"hello"`) are ordinary resumed values. `0xffff_ffff` is the important one: both of
    /// its halves are `65_535`, which is above `BASE`, so any deviation in *where* a reduction
    /// happens shows up as a different answer instead of being absorbed.
    ///
    /// Under Miri the list keeps the seed and that pathological value, and drops the three
    /// whose behaviour they already characterise. Miri's job here is to find undefined
    /// behaviour, which every start reaches the same code paths for; establishing bit-identical
    /// output across the full set of starts is the native run's job, where it is
    /// unconditional.
    #[cfg(not(miri))]
    const SWEEP_STARTS: [u32; 5] = [
        0x0000_0000,
        0x0000_0001,
        0xffff_ffff,
        0x1234_5678,
        0x062c_0215,
    ];
    #[cfg(miri)]
    const SWEEP_STARTS: [u32; 2] = [0x0000_0001, 0xffff_ffff];

    /// Further starting values whose halves are at or above `BASE`, used where the point under
    /// test is specifically that the short paths are delegated rather than re-derived.
    const PATHOLOGICAL_STARTS: [u32; 3] = [0xffff_ffff, 0xffff_fef1, 0xfff0_fff0];

    /// Lengths that bracket every structural boundary of the block engine: one either side of
    /// `NMAX`, of `2 * NMAX` and of `3 * NMAX`, so that both reduction points and every
    /// sub-chunk tail are exercised, plus two long prefixes that are simply more blocks of the
    /// same shapes.
    const BOUNDARY_LENGTHS: [usize; 14] = [
        5_550, 5_551, 5_552, 5_553, 5_554, 11_103, 11_104, 11_105, 16_655, 16_656, 16_657, 16_658,
        65_536, CORPUS_LEN,
    ];

    /// Longest fixture this run is prepared to build, in bytes.
    ///
    /// The native run covers everything: the cap is the whole corpus, so `in_budget` admits
    /// every length named in this module. Under Miri -- which interprets each operation
    /// individually and is orders of magnitude slower than native code -- the ceiling drops to
    /// two whole `NMAX` blocks and a tail. That still crosses both reduction points, the block
    /// boundary itself, the short-path threshold and every sub-chunk offset, which is the
    /// entire structure this file has to get right; the longer fixtures repeat those same
    /// shapes with more blocks and are covered by the native run, where they are unconditional.
    ///
    /// Lengths are *skipped* rather than clamped wherever an expected value is attached to one,
    /// because clamping a fixture would silently compare against the wrong answer.
    #[cfg(not(miri))]
    const FIXTURE_CAP: usize = CORPUS_LEN;
    #[cfg(miri)]
    const FIXTURE_CAP: usize = 2 * NMAX + 37;

    /// Longest prefix the exhaustive length sweep runs to.
    ///
    /// A thousand-odd lengths cover every sub-chunk offset many times over. The sweep's cost
    /// grows with the square of this bound -- it checks every prefix, not just the longest --
    /// so under Miri it is shortened separately from [`FIXTURE_CAP`], while still crossing the
    /// short-path threshold at `16` and several whole sub-chunks beyond it.
    #[cfg(not(miri))]
    const SWEEP_MAX_LEN: usize = 1_024;
    #[cfg(miri)]
    const SWEEP_MAX_LEN: usize = 96;

    /// `(length, checksum)` pairs over the pattern corpus, started from `1`.
    ///
    /// Every value was produced by the in-tree C implementation itself -- `adler32.c`
    /// compiled and called on the same fixture -- and each length is chosen to sit on a
    /// boundary: the short-path threshold (`15`, `16`, `17`), the first reduction point
    /// (`5_551`, `5_552`, `5_553`), the second (`11_104`), a partial third block (`16_657`),
    /// and a length that is neither (`65_536`, `100_000`).
    const KNOWN_ANSWERS: [(usize, u32); 12] = [
        (15, 0x3227_0721),
        (16, 0x3a20_07f9),
        (17, 0x4310_08f0),
        (5_551, 0x5f26_cd87),
        (5_552, 0x2cf4_cdbf),
        (5_553, 0xfb0a_ce16),
        (11_103, 0xa4ee_9b04),
        (11_104, 0x4089_9b8c),
        (11_105, 0xdcbc_9c33),
        (16_657, 0xdb0f_6a50),
        (65_536, 0x7b2e_8772),
        (CORPUS_LEN, CORPUS_CHECKSUM),
    ];

    /// Build the deterministic corpus `buf[i] = (i * 31 + 7) & 0xff`, without I/O and without
    /// randomness so that every expectation above is reproducible anywhere.
    ///
    /// Expressed as a `u8` accumulator advancing by `31` rather than as a masked product: `u8`
    /// arithmetic is modulo `256`, which is exactly what the mask means, so the two
    /// formulations agree byte for byte -- and this one needs no narrowing conversion. It is
    /// the same fixture the scalar module uses, which is what lets the known answers above be
    /// compared against that module's own.
    fn pattern_corpus(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut byte: u8 = 7;
        for _ in 0..len {
            out.push(byte);
            byte = byte.wrapping_add(31);
        }
        out
    }

    /// Whether a fixture of `len` bytes is within this run's budget -- see [`FIXTURE_CAP`],
    /// which is the whole corpus natively and a smaller ceiling under Miri.
    fn in_budget(len: usize) -> bool {
        len <= FIXTURE_CAP
    }

    /// The largest length at or below `len` that this run will build.
    ///
    /// For the sweeps that compare the two backends against each other rather than against a
    /// recorded value, a shorter fixture is a weaker test but never a wrong one, so those may
    /// clamp instead of skipping.
    fn budgeted(len: usize) -> usize {
        len.min(FIXTURE_CAP)
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
    /// Deliberately neither a port of `adler32.c` nor a copy of the kernel above: it is
    /// derived from the specification alone, so agreement with it is evidence about the
    /// algorithm rather than a restatement of the code under test. Reduction is a ring
    /// homomorphism, so reducing eagerly and reducing once per block yield the same residues;
    /// the two therefore agree for every starting value whose halves are already below `BASE`,
    /// which includes RFC 1950's seed of `1`.
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
    fn constants_mirror_the_c_source() {
        assert_eq!(BASE, 65_521, "`BASE`, `adler32.c` L10");
        assert_eq!(NMAX, 5_552, "`NMAX`, `adler32.c` L11");
        assert_eq!(SUB_CHUNK, 16, "the `DO16` grouping, `adler32.c` L18");
        assert_eq!(
            SHORT_INPUT_LEN, 16,
            "the `len < 16` test, `adler32.c` L85, and one constant with `SUB_CHUNK`"
        );

        // `adler32.c` L99 relies on this: a full block is whole steps and no tail, so only
        // the final partial block can reach the byte-at-a-time loop.
        assert_eq!(
            NMAX % SUB_CHUNK,
            0,
            "`NMAX` must be divisible by `SUB_CHUNK`"
        );
        assert_eq!(
            NMAX / SUB_CHUNK,
            347,
            "the iteration count `n`, `adler32.c` L99"
        );
    }

    #[test]
    fn sub_chunk_weight_is_the_sub_chunk_length() {
        // The multiplier applied to `sum1` is the sub-chunk length itself. The two are
        // separate constants only because one is a `usize` and the other the accumulator's
        // `u32`, and converting between them in library code would be a narrowing conversion.
        let length = u32::try_from(SUB_CHUNK).expect("a 16-byte sub-chunk length fits in u32");
        assert_eq!(SUB_CHUNK_WEIGHT, length);
    }

    #[test]
    fn the_lane_threshold_delegates_without_changing_a_value() {
        // Four sub-chunks, which is where the measured throughput curve in the constant's own
        // documentation crosses one. Pinned so that a future retune is a deliberate edit.
        assert_eq!(LANE_THRESHOLD_LEN, 4 * SUB_CHUNK);
        assert_eq!(LANE_THRESHOLD_LEN, 64);
        // That the threshold stays above the scalar short path -- otherwise the dispatch order
        // would make it unreachable -- is pinned at compile time beside the constant itself.

        // Delegation is a throughput decision with no behavioural content. Every length either
        // side of the threshold must agree with the scalar backend -- including the lengths
        // that are exactly on it -- for canonical and pathological starting values alike.
        let corpus = pattern_corpus(SWEEP_MAX_LEN);
        for start in SWEEP_STARTS
            .iter()
            .chain(PATHOLOGICAL_STARTS.iter())
            .copied()
        {
            for len in SHORT_INPUT_LEN..=SWEEP_MAX_LEN {
                let buf = prefix(&corpus, len);
                assert_eq!(
                    Adler32Simd::checksum(start, buf),
                    Adler32Generic::checksum(start, buf),
                    "checksum({start:#010x}, pattern[..{len}]) across the lane threshold",
                );
            }
        }
    }

    #[test]
    fn lane_offsets_are_zero_through_k_minus_one() {
        assert_eq!(POSITIONS.len(), SUB_CHUNK, "one offset per lane");

        // Entry `i` must be `i`: the byte's own offset within its sub-chunk, which is what the
        // `P` term of the decomposition discounts it by. Checked by iteration rather than by
        // re-listing the literals, so this is a statement about the identity and not a second
        // copy of the table.
        for (i, &offset) in POSITIONS.iter().enumerate() {
            let expected = u32::try_from(i).expect("an offset below 16 fits in u32");
            assert_eq!(offset, expected, "offset for lane {i}");
        }

        // Strictly ascending from 0, ending one below the lane count.
        assert!(POSITIONS.windows(2).all(|pair| match pair {
            [left, right] => *right == *left + 1,
            _ => false,
        }));
        assert_eq!(POSITIONS.first(), Some(&0));
        assert_eq!(POSITIONS.last(), Some(&(SUB_CHUNK_WEIGHT - 1)));
    }

    #[test]
    fn worst_case_block_accumulation_fits_in_u32() {
        // The tightest case for the whole engine: both halves of the starting value at their
        // maximum, then a full block of 0xff bytes. Evaluated in `u64` so every product is
        // exact; the literal stands in for `NMAX`, which the test above pins to it.
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

        // One byte more than a block would exceed `u32`, which is why the chunk length in
        // `adler32_blocks` is exactly `NMAX` and must never be enlarged for throughput.
        let over = 65_535 + (n + 1) * 65_535 + 255 * ((n + 1) * (n + 2) / 2);
        assert!(
            u32::try_from(over).is_err(),
            "a longer block would overflow `u32`"
        );

        // The lane arrangement forms intermediates the byte-at-a-time path never does; they
        // have their own bound table, checked by the next test.
        assert!(sum1_peak < sum2_peak);
    }

    #[test]
    fn lane_accumulators_and_the_fold_fit_in_u32() {
        // Every number in the module documentation's bound table, recomputed in `u64` so the
        // products are exact, for the worst case: a full block of 0xff bytes entered from the
        // largest value a split can produce.
        let k = u64::from(SUB_CHUNK_WEIGHT);
        let m = u64::try_from(NMAX / SUB_CHUNK).expect("347 fits in u64");
        assert_eq!(m, 347, "sub-chunks per full block");

        let totals_max = m * 255;
        let carried_max = 255 * ((m - 1) * m / 2);
        let byte_total_max = k * totals_max;
        let carried_total_max = k * carried_max;
        let scaled_max = k * (carried_total_max + byte_total_max);
        let positional_max = 255 * m * (k * (k - 1) / 2);
        let weighted_max = scaled_max - positional_max;
        let carried_in_max = u64::try_from(NMAX).expect("NMAX fits in u64") * 65_535;
        let peak = 65_535 + carried_in_max + weighted_max;

        assert_eq!(totals_max, 88_485, "the documented `totals[i]` bound");
        assert_eq!(carried_max, 15_307_905, "the documented `carried[i]` bound");
        assert_eq!(byte_total_max, 1_415_760, "the documented `S` bound");
        assert_eq!(carried_total_max, 244_926_480, "the documented `C` bound");
        assert_eq!(
            scaled_max, 3_941_475_840,
            "the documented `k * (C + S)` bound"
        );
        assert_eq!(positional_max, 10_618_200, "the documented `P` bound");
        assert_eq!(weighted_max, 3_930_857_640, "the documented `W` bound");
        assert_eq!(
            carried_in_max, 363_850_320,
            "the documented `n * sum1` bound"
        );
        assert_eq!(peak, 4_294_773_495, "the same peak as the scalar path");

        // Every one of them fits, and the largest intermediate has room to spare.
        for (name, value) in [
            ("totals[i]", totals_max),
            ("carried[i]", carried_max),
            ("S", byte_total_max),
            ("C", carried_total_max),
            ("k * (C + S)", scaled_max),
            ("P", positional_max),
            ("W", weighted_max),
            ("the sum2 peak", peak),
        ] {
            assert!(u32::try_from(value).is_ok(), "{name} must fit in u32");
        }
        assert_eq!(u64::from(u32::MAX) - peak, 193_800, "the documented margin");

        // The ordering hazard is real rather than theoretical: letting `k * (C + S)` meet
        // `sum2 + n * sum1` before `P` is subtracted overflows, which is exactly why
        // `fold_lanes` forms `W` as its own binding first.
        let wrong_order = 65_535 + carried_in_max + scaled_max;
        assert_eq!(wrong_order, 4_305_391_695);
        assert!(
            u32::try_from(wrong_order).is_err(),
            "the wrong evaluation order must overflow, or the precaution is pointless"
        );

        // And `W` cannot underflow: `P <= (k - 1) * S`, which is below `k * (C + S)`.
        assert!(positional_max <= (k - 1) * byte_total_max);
    }

    #[test]
    fn folding_lanes_agrees_with_the_byte_at_a_time_recurrence() {
        // The lane decomposition against the recurrence it re-expresses, over every block
        // length from empty to several whole sub-chunks and for ordinary as well as
        // pathological running values. Neither side reduces, so a divergence here would be a
        // divergence in the identity itself rather than in a reduction schedule.
        let corpus = pattern_corpus(8 * SUB_CHUNK);
        for start in PATHOLOGICAL_STARTS
            .iter()
            .chain(SWEEP_STARTS.iter())
            .copied()
        {
            let sum1 = start & 0xffff;
            let sum2 = start >> 16;
            for len in 0..=corpus.len() {
                let block = prefix(&corpus, len);
                assert_eq!(
                    accumulate_block(sum1, sum2, block),
                    accumulate_bytes(sum1, sum2, block),
                    "a {len}-byte block from {start:#010x}",
                );
            }
        }

        // A saturated block of the maximum length: the case the bound table is computed for.
        let saturated = vec![0xff_u8; NMAX];
        assert_eq!(
            accumulate_block(65_535, 65_535, &saturated),
            accumulate_bytes(65_535, 65_535, &saturated),
            "a saturated full block from the largest possible halves",
        );

        // `fold_lanes` over zeroed lanes must be the identity on both sums, which is what lets
        // a block with no whole sub-chunk need no special case.
        let zeroed = [0_u32; SUB_CHUNK];
        assert_eq!(fold_lanes(7, 11, 0, &zeroed, &zeroed), (7, 11));

        // Its three reductions on a hand-computed case: one sub-chunk holding the bytes 1..=16,
        // for which S = 136, C = 0 because nothing precedes it, and P = sum i * b_i = 1_360, so
        // W = 16 * 136 - 1_360 = 816. The recurrence produces the same pair for that run.
        let mut totals = [0_u32; SUB_CHUNK];
        for (total, offset) in totals.iter_mut().zip(POSITIONS.iter()) {
            *total = offset + 1;
        }
        assert_eq!(
            fold_lanes(0, 0, SUB_CHUNK_WEIGHT, &totals, &zeroed),
            (136, 816)
        );
        let ramp: [u8; SUB_CHUNK] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        assert_eq!(accumulate_bytes(0, 0, &ramp), (136, 816));
    }
    #[test]
    fn empty_buffer_returns_the_incoming_value_normalised() {
        // An empty slice is not `Z_NULL`: it reaches the delegated short path and returns the
        // running value, so a zero checksum stays zero instead of becoming the seed 1.
        assert_eq!(Adler32Simd::checksum(1, &[]), 0x0000_0001);
        assert_eq!(Adler32Simd::checksum(0, &[]), 0x0000_0000);

        // Both halves of 0xffff_ffff are 65_535, above `BASE`; the short path normalises them
        // to 14 -- by one conditional subtraction for `sum1` and by `%` for `sum2`.
        assert_eq!(Adler32Simd::checksum(0xffff_ffff, &[]), 0x000e_000e);

        // A start whose halves straddle `BASE`: the low half is below it and survives
        // untouched, the high half is above it and is reduced.
        assert_eq!(Adler32Simd::checksum(0xffff_fef1, &[]), 0x000e_fef1);

        // And the two backends agree on the empty input from every starting value.
        for start in SWEEP_STARTS
            .iter()
            .chain(PATHOLOGICAL_STARTS.iter())
            .copied()
        {
            assert_eq!(
                Adler32Simd::checksum(start, &[]),
                Adler32Generic::checksum(start, &[]),
                "the empty input from {start:#010x}",
            );
        }
    }

    #[test]
    fn buffers_longer_than_nmax_reduce_after_every_block() {
        let corpus = pattern_corpus(FIXTURE_CAP);

        // One block plus one byte is the shortest input that reaches the second reduction
        // point, and 0xff_ffff-style starting halves make a missing reduction visible rather
        // than absorbed. Compared against the C implementation's own answers.
        assert_eq!(
            Adler32Simd::checksum(1, prefix(&corpus, NMAX + 1)),
            0xfb0a_ce16
        );
        assert_eq!(
            Adler32Simd::checksum(0xffff_ffff, &vec![0xff_u8; NMAX + 1]),
            0xa843_9c98
        );

        // Several whole blocks and a partial one, so that a full block, a full block followed
        // by a tail, and a bare tail are all exercised in one call.
        if in_budget(3 * NMAX + 7) {
            assert_eq!(
                Adler32Simd::checksum(1, prefix(&corpus, 3 * NMAX + 7)),
                Adler32Generic::checksum(1, prefix(&corpus, 3 * NMAX + 7)),
            );
            assert_eq!(
                Adler32Simd::checksum(1, prefix(&corpus, 16_657)),
                0xdb0f_6a50
            );
        }

        // Deliberately not reduced on the way in: a caller may resume from anything, and the
        // block engine must reduce both halves after the very first block regardless.
        for start in PATHOLOGICAL_STARTS {
            let buf = prefix(&corpus, 2 * NMAX + 3);
            assert_eq!(
                Adler32Simd::checksum(start, buf),
                Adler32Generic::checksum(start, buf),
                "two blocks and a tail from {start:#010x}",
            );
        }
    }

    #[test]
    fn known_answer_vectors_from_the_reference_implementation() {
        // Short, delegated inputs.
        assert_eq!(Adler32Simd::checksum(1, b"a"), 0x0062_0062);
        assert_eq!(Adler32Simd::checksum(1, b"abc"), 0x024d_0127);
        assert_eq!(Adler32Simd::checksum(1, &[b'x'; 15]), 0x384f_0709);

        // `hello` is the preset dictionary and `hello, hello!` the payload used by
        // `test/example.c`, so these two values gate the acceptance suite.
        assert_eq!(Adler32Simd::checksum(1, b"hello"), 0x062c_0215);
        assert_eq!(Adler32Simd::checksum(1, b"hello, hello!"), 0x2170_0496);
        assert_eq!(Adler32Simd::checksum(1, b"Wikipedia"), 0x11e6_0398);

        // Sixteen bytes is the shortest input the block engine sees.
        assert_eq!(Adler32Simd::checksum(1, &[b'x'; 16]), 0x3fd0_0781);
        assert_eq!(Adler32Simd::checksum(1, &[b'x'; 17]), 0x47c9_07f9);
        assert_eq!(Adler32Simd::checksum(0xffff_ffff, &[0xff; 16]), 0x8866_0ffe);
        assert_eq!(Adler32Simd::checksum(0xffff_ffff, &[0xff; 15]), 0x7868_0eff);

        // And the corpus sweep, which crosses both reduction points.
        let corpus = pattern_corpus(FIXTURE_CAP);
        for &(len, expected) in KNOWN_ANSWERS.iter().filter(|&&(len, _)| in_budget(len)) {
            let got = Adler32Simd::checksum(1, prefix(&corpus, len));
            assert_eq!(got, expected, "checksum(1, pattern[..{len}]) = {got:#010x}");
        }
    }

    #[test]
    fn matches_the_scalar_backend_on_every_length_through_the_sweep_bound() {
        // The bit-identity test this backend exists to pass. Every length from the empty slice
        // upward crosses the single-byte path, the short path, the first whole sub-chunk and
        // every sub-chunk tail offset, and each is checked from five starting values --
        // including one whose halves are above `BASE`, which is what makes a misplaced
        // reduction visible instead of absorbed.
        let corpus = pattern_corpus(SWEEP_MAX_LEN);
        for start in SWEEP_STARTS {
            for len in 0..=SWEEP_MAX_LEN {
                let buf = prefix(&corpus, len);
                assert_eq!(
                    Adler32Simd::checksum(start, buf),
                    Adler32Generic::checksum(start, buf),
                    "checksum({start:#010x}, pattern[..{len}])",
                );
            }
        }
    }

    #[test]
    fn matches_the_scalar_backend_at_every_reduction_boundary() {
        let corpus = pattern_corpus(FIXTURE_CAP);
        let saturated = vec![0xff_u8; FIXTURE_CAP];

        for start in SWEEP_STARTS {
            for len in BOUNDARY_LENGTHS.into_iter().filter(|&len| in_budget(len)) {
                let buf = prefix(&corpus, len);
                assert_eq!(
                    Adler32Simd::checksum(start, buf),
                    Adler32Generic::checksum(start, buf),
                    "pattern corpus, checksum({start:#010x}, ..{len})",
                );

                // The same lengths over all-0xff input, which drives both accumulators to
                // their documented peaks instead of merely exercising the control flow.
                let worst = prefix(&saturated, len);
                assert_eq!(
                    Adler32Simd::checksum(start, worst),
                    Adler32Generic::checksum(start, worst),
                    "saturated corpus, checksum({start:#010x}, ..{len})",
                );
            }
        }
    }

    #[test]
    fn all_ones_worst_case_matches_the_reference() {
        let saturated = vec![0xff_u8; FIXTURE_CAP];

        // Exactly one full block from the largest possible starting value: the case with only
        // 193_800 of headroom. An accumulator narrower than the reductions assume, or a block
        // enlarged past `NMAX`, would overflow here -- and under the debug profile these tests
        // run in, an overflow is an outright panic rather than a silently wrong answer.
        assert_eq!(
            Adler32Simd::checksum(0xffff_ffff, prefix(&saturated, NMAX)),
            0x0bab_9b99
        );

        // Two whole blocks, and far beyond them.
        assert_eq!(
            Adler32Simd::checksum(1, prefix(&saturated, 2 * NMAX)),
            0xff6f_3726
        );
        if in_budget(CORPUS_LEN) {
            assert_eq!(
                Adler32Simd::checksum(1, prefix(&saturated, CORPUS_LEN)),
                0x149a_302c
            );
        }
    }

    #[test]
    fn chunked_feeding_matches_single_shot() {
        // `deflate` and `inflate` both accumulate the checksum incrementally, over whatever
        // chunk sizes their callers happen to produce, so every split of the same input must
        // land on the same value -- including splits that cut sub-chunks and blocks in half.
        let corpus = pattern_corpus(FIXTURE_CAP);
        let single_shot = Adler32Simd::checksum(1, &corpus);
        if in_budget(CORPUS_LEN) {
            assert_eq!(single_shot, CORPUS_CHECKSUM, "the whole corpus in one call");
        }

        let steps = [
            1,
            SUB_CHUNK - 1,
            SUB_CHUNK,
            SUB_CHUNK + 1,
            2 * SUB_CHUNK - 1,
            2 * SUB_CHUNK,
            2 * SUB_CHUNK + 1,
            NMAX - 1,
            NMAX,
            NMAX + 1,
            2 * NMAX,
            corpus.len() - 1,
        ];
        for step in steps {
            let mut adler = 1;
            for chunk in corpus.chunks(step) {
                adler = Adler32Simd::checksum(adler, chunk);
            }
            assert_eq!(
                adler, single_shot,
                "feeding the corpus {step} bytes at a time"
            );
        }
    }

    #[test]
    fn single_byte_and_short_inputs_delegate_to_the_scalar_backend() {
        // The single-byte path returns a high half that is deliberately *not* fully reduced
        // for this input, because one conditional subtraction is all the C source performs
        // (`adler32.c` L75-L76). Re-deriving that path instead of delegating to it is the most
        // plausible way for this backend to diverge, so the divergence is probed directly.
        let got = Adler32Simd::checksum(0xffff_fef1, &[0xff]);
        assert_eq!(
            got, 0xfffe_fff0,
            "the C `len == 1` path, `adler32.c` L70-L78"
        );
        assert!(
            got >> 16 >= BASE,
            "the high half is deliberately not fully reduced"
        );
        assert_ne!(
            naive_adler32(0xffff_fef1, &[0xff]),
            got,
            "a fully reducing model disagrees, which is what makes delegation observable"
        );

        // Every short length from every pathological start, against the scalar backend.
        let corpus = pattern_corpus(SHORT_INPUT_LEN);
        for start in PATHOLOGICAL_STARTS
            .iter()
            .chain(SWEEP_STARTS.iter())
            .copied()
        {
            for len in 0..SHORT_INPUT_LEN {
                let short = prefix(&corpus, len);
                assert_eq!(
                    Adler32Simd::checksum(start, short),
                    Adler32Generic::checksum(start, short),
                    "checksum({start:#010x}, pattern[..{len}])",
                );
            }

            for &byte in &[0x00_u8, 0x01, 0x7f, 0xfe, 0xff] {
                assert_eq!(
                    Adler32Simd::checksum(start, &[byte]),
                    Adler32Generic::checksum(start, &[byte]),
                    "checksum({start:#010x}, [{byte:#04x}])",
                );
            }
        }
    }

    #[test]
    fn matches_an_independent_rfc1950_model() {
        let corpus = pattern_corpus(FIXTURE_CAP);
        let lengths = [
            0,
            1,
            2,
            SUB_CHUNK - 1,
            SUB_CHUNK,
            SUB_CHUNK + 1,
            255,
            256,
            257,
            NMAX - 1,
            NMAX,
            NMAX + 1,
            2 * NMAX,
            2 * NMAX + 1,
            3 * NMAX + 7,
        ];
        for len in lengths.into_iter().filter(|&len| in_budget(len)) {
            let buf = prefix(&corpus, len);
            assert_eq!(
                Adler32Simd::checksum(1, buf),
                naive_adler32(1, buf),
                "the specification model disagrees at length {len}",
            );
        }
    }

    #[test]
    fn is_supported_is_callable_and_cannot_change_a_checksum() {
        // Callable, total, and stable: it reports a property of the machine, so two calls in
        // the same process must agree.
        let supported = is_supported();
        assert_eq!(supported, is_supported());

        // x86-64 mandates SSE2 in its baseline, so on that target the answer is known. The
        // assertion is scoped to it rather than written for every architecture, because
        // elsewhere the answer is a hint about code generation and not a fact about an
        // instruction set.
        #[cfg(target_arch = "x86_64")]
        assert!(supported, "SSE2 is part of the x86-64 baseline");

        // Whatever it answered, the checksum is unchanged. This is the property that makes
        // this file safe: it contains no architecture-specific instruction, so no dispatch
        // decision can alter a value. Both backends are called directly here rather than
        // through the parent module's dispatcher, so the claim is about the code and not
        // about how it happens to be selected.
        let corpus = pattern_corpus(budgeted(4 * NMAX + 13));
        for start in SWEEP_STARTS {
            assert_eq!(
                Adler32Simd::checksum(start, &corpus),
                Adler32Generic::checksum(start, &corpus),
                "the two backends must agree regardless of {supported} from {start:#010x}",
            );
        }
    }

    /// Construct a `T` through its [`Default`] implementation.
    ///
    /// Written generically on purpose: it exercises the derive rather than the unit-struct
    /// literal, which is what a direct `Adler32Simd::default()` would collapse into.
    fn default_of<T: Default>() -> T {
        T::default()
    }

    /// Duplicate a `T` through its [`Clone`] implementation, generically for the same reason:
    /// for a `Copy` type a direct `clone()` call is just a copy.
    fn clone_of<T: Clone>(value: &T) -> T {
        value.clone()
    }

    #[test]
    fn the_backend_marker_carries_the_derives_its_consumers_need() {
        // Zero-sized, so naming a backend costs nothing at run time, and carrying every derive
        // the dispatcher, the benchmark and the equivalence tests rely on.
        let backend: Adler32Simd = default_of();
        let cloned = clone_of(&backend);
        let copied = cloned;

        assert_eq!(size_of_val(&copied), 0, "the marker must be zero-sized");
        // `cloned` is still usable after the assignment above, which is `Copy` at work.
        assert_eq!(format!("{cloned:?}"), "Adler32Simd");
        assert_eq!(format!("{copied:?}"), "Adler32Simd");
    }
}
