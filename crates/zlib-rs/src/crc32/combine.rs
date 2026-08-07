//! CRC-32 combination: the modular polynomial arithmetic that splices two
//! independently computed check values into the check value of their
//! concatenation.
//!
//! Mirrors `crc32.c`: the polynomial constant at L156-L157, `multmodp` at
//! L159-L178, `x2nmodp` at L180-L195, and the public entry points at
//! L953-L983. `zlib.h` L1874-L1894 states the contract those entry points
//! satisfy, and this module reproduces it exactly -- including the two
//! documented degenerate answers, which are easy to "improve" by accident and
//! must not be.
//!
//! # What combining means
//!
//! A CRC-32 is the remainder of the message polynomial divided by the
//! generator `p(x)`, so appending `len2` bytes to a message multiplies its
//! polynomial by `x^(8 * len2)`. Two check values can therefore be spliced
//! without re-reading either sequence: multiply the first by `x^(8 * len2)`
//! modulo `p(x)` and add (exclusive-or) the second. `crc32_combine_gen`
//! computes that multiplier -- the *operator* -- and `crc32_combine_op`
//! applies it. That split exists because the operator is the expensive half,
//! which is exactly the advice `zlib.h` L1892-L1894 gives: reusing one
//! operator beats recomputing it from the same length.
//!
//! Coefficients are held reflected, lowest power in the most significant bit,
//! as `crc32.c` L222-L229 describes. In that representation the polynomial
//! `1` is `1 << 31`, not `1`.
//!
//! # Widths, and why there is one implementation here rather than five
//!
//! The C library exports five symbols for this operation because `zconf.h`
//! redirects the unsuffixed names according to the *caller's*
//! `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` settings: `crc32_combine`
//! and `crc32_combine_gen` take `z_off_t` (`zlib.h` L2020-L2021 and
//! L2027-L2028), `crc32_combine64` and `crc32_combine_gen64` take
//! `z_off64_t` (`zlib.h` L1983-L1984 and L2011-L2012), and `Z_WANT64` turns
//! the former into aliases of the latter (`zlib.h` L2002-L2003).
//! `crc32_combine_op` takes `uLong` and needs no variant at all.
//!
//! That duality is a C header concern, and the planned
//! `crates/libz-rs-sys/src/checksum.rs` is where it is to be resolved: that crate owns
//! the C types and is to perform every
//! width conversion. This module models a length as `i64` -- the widest form
//! `z_off64_t` takes on any target (`zconf.h` L523-L531) -- and a check value
//! as `u32`, and it implements each operation exactly once. The narrow-length
//! entry points below are one-line delegations, precisely as `crc32.c`
//! L963-L966 and L980-L983 are.
//!
//! Modelling a check value as `u32` is not a simplification either. A CRC-32
//! value "is in the range of a 32-bit unsigned integer" (`zlib.h`
//! L1849-L1850), and the C code enforces that itself by masking both check
//! values with `0xffff_ffff` before use (`crc32.c` L972). Here the type system
//! applies the mask.
//!
//! # No runtime initialization
//!
//! `crc32.c` L957-L959 calls `z_once(&made, make_crc_table)` from inside
//! `crc32_combine_gen64`, so that a `DYNAMIC_CRC_TABLE` build populates
//! `x2n_table[]` before it is read. This implementation has no such call and needs none:
//! the tables are compile-time `const` data in the sibling `tables` module.
//!
//! # Safety and robustness posture
//!
//! The module names only `core` and the sibling `tables` module, allocates
//! nothing, and holds no raw pointer, so the crate root's
//! `#![forbid(unsafe_code)]` costs it nothing. Every loop below carries a
//! proven upper bound, every table read goes through a checked accessor, and no
//! operation can panic.
//!
//! Termination deserves its own sentence here, because this is the one part of
//! the CRC-32 subsystem where the reference implementation can be made to spin
//! for ever, and it can be made to do so in *two* independent ways: `multmodp`
//! loops without bound when its multiplier has no set bit, and `x2nmodp` loops
//! without bound when its exponent is negative, since an arithmetic right shift
//! converges on `-1` and stays there. Both are documented preconditions in the
//! C source, both are honoured by every in-tree caller, and neither can be
//! relied upon here, because these functions are reachable from values that
//! arrive through the C ABI. Each is removed structurally -- a bounded trip
//! count and an unsigned parameter respectively -- without altering a single
//! value the C code actually returns. The functions' own documentation carries
//! the equivalence arguments; do not undo either change.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::crc32::{crc32, crc32_combine64, crc32_combine_gen64, crc32_combine_op};
//!
//! // Splice the check values of "hello, " and "world" into the check value of
//! // "hello, world", without re-reading either sequence.
//! let head = crc32(0, b"hello, ");
//! let tail = crc32(0, b"world");
//! assert_eq!(crc32_combine64(head, tail, 5), crc32(0, b"hello, world"));
//!
//! // When one length recurs, hoist the operator out of the loop and reuse it.
//! let op = crc32_combine_gen64(5);
//! assert_eq!(crc32_combine_op(head, tail, op), crc32(0, b"hello, world"));
//! ```

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use super::tables;

/// The polynomial `x^0 == 1` in the reflected representation.
///
/// `crc32.c` L187 spells this `(uLong)1 << 31` where it seeds `x2nmodp`, and
/// `crc32.c` L222-L229 explains why: coefficients run from the lowest power in
/// the most significant bit, so the constant polynomial `1` is the top bit
/// alone.
///
/// Two of its properties are load-bearing below. It is the multiplicative
/// identity, so `multmodp(X_POW_0, b) == b` for every `b`; and it is what
/// `crc32_combine_gen64(0)` returns, which is what makes combining with a
/// zero-length second sequence collapse to `crc1 ^ crc2`.
const X_POW_0: u32 = 1 << 31;

/// Return `a(x) * b(x) mod p(x)` over `GF(2)`, all three polynomials
/// reflected.
///
/// Mirrors `multmodp` at `crc32.c` L163-L178, whose comment at L159-L162
/// reads: "Return a(x) multiplied by b(x) modulo p(x), where p(x) is the CRC
/// polynomial, reflected. For speed, this requires that a not be zero."
///
/// The method is schoolbook shift-and-add. Walk the bits of `a` from the most
/// significant downwards; accumulate `b` into the product wherever `a` has a
/// set bit; and after each step multiply `b` by `x`, which in the reflected
/// representation is a right shift plus `POLY` whenever the bit shifted out
/// was set. The walk stops as soon as the *lowest* set bit of `a` has been
/// consumed, because every bit still unexamined is zero and could only
/// contribute zero.
///
/// # Why the loop is bounded, and why the bound must not be removed
///
/// The C loop is `for (;;)` and its only exit is the `break` nested inside
/// `if (a & m)`. `m` walks `1 << 31, 1 << 30, ..., 1 << 0` and then becomes
/// zero, after which `a & m` is false forever. So whenever `a` has no set bit
/// among its low 32 bits, **the C function never returns**. The precondition
/// recorded in its comment is therefore load-bearing rather than advisory, and
/// every in-tree caller honours it: `crc32_combine_op` rejects a zero operator
/// before calling (`crc32.c` L970-L971).
///
/// This implementation cannot lean on a caller's discipline. Operator values reach this
/// subsystem from C through `crates/libz-rs-sys`, and the fuzz gate requires
/// the checksum target to survive its entire time budget with no crash *and no
/// hang*. The loop is consequently bounded at 32 iterations, which is exactly
/// the number the C loop performs whenever it terminates at all:
///
/// * For any `a` with a set bit, the lowest one sits at position `31 - i` for
///   some `i` in `0..32`. Iteration `i` -- having performed precisely the same
///   sequence of accumulations into the product and the same sequence of
///   updates to `b` as the C loop -- takes the same `break`. The value
///   returned is identical.
/// * For `a == 0` the C loop spins, whereas this returns `0`: the value the
///   accumulator was initialized to, and the correct product. That single
///   divergence converts a hang into a defined answer, and it is unreachable
///   through the public entry points anyway, because `crc32_combine_op`
///   screens out `op == 0` first.
///
/// The C parameters are `uLong`, 64 bits wide on LP64 (`zconf.h` L406).
/// Narrowing them to `u32` changes no result, because the C loop only ever
/// examines bits `0..32` of `a`: `m` never exceeds `1 << 31`, and `a & (m - 1)`
/// inspects only bits below `m`. The values a narrowing could alter are those
/// whose low 32 bits are zero, and those are exactly the ones the C loop hangs
/// on.
///
/// A 33rd iteration must never be introduced. `m` is `0` once the 32nd
/// iteration's shift has run, and `m - 1` would then underflow -- an overflow
/// panic in a debug build and a wrong mask in a release build. Holding the trip
/// count at 32 keeps that subtraction on `m` in `1 << 0 ..= 1 << 31`, where
/// `m - 1` is a well-defined mask between `0` and `0x7fff_ffff`.
#[must_use]
pub(crate) fn multmodp(a: u32, mut b: u32) -> u32 {
    // Bit-walk mask over `a`, from the most significant bit down. This is a
    // mask, not a polynomial, so it is written as a shift rather than as
    // `X_POW_0`.
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    for _ in 0..32 {
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                // Every remaining bit of `a` is zero; nothing further can be
                // accumulated. `crc32.c` L171-L172.
                break;
            }
        }
        m >>= 1;
        // Multiply `b` by `x` modulo `p(x)`: `crc32.c` L175.
        b = if b & 1 != 0 {
            (b >> 1) ^ tables::POLY
        } else {
            b >> 1
        };
    }
    p
}

/// Read `x2n_table[k & 31]`, the reflected polynomial `x^(2^k)`.
///
/// The mask reproduces `x2n_table[k & 31]` at `crc32.c` L190, and it is
/// load-bearing rather than defensive: `x2nmodp` starts `k` at `3` and
/// increments it once per bit of `n`, so `k` runs past `31` for any second
/// sequence of `2^29` bytes or more, and the C code depends on the mask to keep
/// the subscript inside a 32-element table.
///
/// The mask is also *exact*, which is easy to miss and worth stating plainly.
/// `x2n_table[j]` is `x^(2^j)` (`crc32.c` L259-L262 seeds `x^1` and squares),
/// and squaring the last entry returns the first: `x^(2^32)` is congruent to
/// `x^(2^0)` modulo `p(x)`, so the table is cyclic with period 32. Reducing the
/// subscript therefore selects the same polynomial the unreduced exponent
/// names, and `x2nmodp` computes `x^(n * 2^k)` exactly for every `n` and `k`
/// rather than only for small ones. The cyclicity is asserted by
/// `the_table_of_powers_is_cyclic_so_the_masked_subscript_is_exact` below,
/// because it is the premise the whole large-length path rests on.
///
/// Neither step below can fail. `k & 31` is at most `31`, `usize` is at least
/// 16 bits wide on every Rust target, and `x2n_table[]` has 32 entries
/// (`crc32.c` L147). Both steps still supply a total answer rather than a
/// panicking one, and both name `X_POW_0` for it, because `X_POW_0` is
/// `multmodp`'s identity: an impossible failure would leave a running product
/// untouched instead of annihilating it.
fn x2n_power(k: u32) -> u32 {
    let Ok(index) = usize::try_from(k & 31) else {
        return X_POW_0;
    };
    tables::X2N_TABLE.get(index).copied().unwrap_or(X_POW_0)
}

/// Return `x^(n * 2^k) mod p(x)`, reflected.
///
/// Mirrors `x2nmodp` at `crc32.c` L184-L195, whose comment at L180-L183
/// reads: "Return x^(n * 2^k) modulo p(x). Requires that `x2n_table[]` has
/// been initialized. n must not be negative."
///
/// `x2n_table[j]` holds `x^(2^j)`, because `crc32.c` L259-L262 seeds the table
/// with `x^1` and then repeatedly squares it. The exponent `n * 2^k` is
/// therefore realized by walking the bits of `n` and multiplying in
/// `x^(2^(k + i))` for every set bit `i`, which is what the loop below does.
/// Subscripts beyond the end of the table wrap, and wrapping loses nothing:
/// see `x2n_power` for why the reduction is exact rather than approximate.
///
/// # Why `n` is unsigned here when the C parameter is signed
///
/// The C parameter is `z_off64_t`, a signed type whose contract says it must
/// not be negative -- and that precondition is load-bearing for the same reason
/// `multmodp`'s is. Shifting a negative signed integer right is an arithmetic
/// shift, so the sign bit replicates: `-5` becomes `-3`, then `-2`, then `-1`,
/// and `-1 >> 1` is `-1` for ever. `while (n)` never sees zero, so **the C
/// function does not return for a negative `n`** either. This is the second
/// non-termination hazard in the file, and it is the less visible of the two,
/// because nothing about the loop looks unbounded.
///
/// Taking `u64` removes it structurally rather than by convention: the
/// precondition becomes part of the signature, and the check moves to the one
/// point at which a length enters this module -- `crc32_combine_gen64`, which
/// returns zero for a negative length exactly as `crc32.c` L955-L956 does.
/// A plain `n >>= 1` on a value whose sign has not been established must
/// therefore never be reintroduced here.
///
/// With an unsigned `n` the loop runs at most 64 times, once per bit.
#[must_use]
pub(crate) fn x2nmodp(mut n: u64, mut k: u32) -> u32 {
    // x^0 == 1: `crc32.c` L187.
    let mut p: u32 = X_POW_0;
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(x2n_power(k), p);
        }
        n >>= 1;
        // No caller can drive `k` to `u32::MAX` inside 64 iterations, but
        // `wrapping_add` keeps that a fact about callers rather than a
        // debug-build panic in waiting. Wrapping would be harmless regardless:
        // `x2n_power` masks with 31, and 2^32 is a multiple of 32, so the
        // sequence of table subscripts is unbroken across the wrap.
        k = k.wrapping_add(1);
    }
    p
}

/// Return the combination operator for a second sequence of `len2` bytes.
///
/// Mirrors `crc32_combine_gen64` at `crc32.c` L953-L961. This is the one
/// implementation behind both exported C symbols, `crc32_combine_gen`
/// (`zlib.h` L1884) and `crc32_combine_gen64` (`zlib.h` L1984);
/// the planned `crates/libz-rs-sys/src/checksum.rs` will widen the caller's `z_off_t`
/// or `z_off64_t` to the `i64` taken here.
///
/// The operator is `x^(8 * len2) mod p(x)`. Hand it to `crc32_combine_op`
/// along with two check values; computing it once and reusing it is the entire
/// purpose of the split, per `zlib.h` L1892-L1894.
///
/// # Degenerate lengths
///
/// * `len2 < 0` returns `0`, matching `crc32.c` L955-L956 and the documented
///   promise at `zlib.h` L1886-L1888 that "len2 must be non-negative,
///   otherwise zero is returned". `i64::MIN` is covered like any other
///   negative value, because `u64::try_from` rejects exactly the negatives --
///   which is the C predicate, expressed without a lossy cast.
/// * `len2 == 0` returns `1 << 31`, and that is emphatically *not* the error
///   answer: it is the identity operator, and it is what makes
///   `crc32_combine64(crc1, crc2, 0)` collapse to `crc1 ^ crc2`.
#[must_use]
pub fn crc32_combine_gen64(len2: i64) -> u32 {
    // `crc32.c` L955-L956: a negative length yields zero. `u64::try_from`
    // fails on precisely the negative values, so this is the C predicate and
    // the widening in one step, with no cast to justify.
    let Ok(len2) = u64::try_from(len2) else {
        return 0;
    };
    // The literal `3` scales bytes to bits: x^(len2 * 2^3) == x^(8 * len2).
    // `crc32.c` L960.
    x2nmodp(len2, 3)
}

/// Return the combination operator for a second sequence of `len2` bytes.
///
/// Mirrors `crc32_combine_gen` at `crc32.c` L963-L966, which is nothing
/// but a widening cast onto `crc32_combine_gen64`. It is retained here so that
/// the planned `crates/libz-rs-sys/src/checksum.rs` will have a core function per
/// exported C symbol, and so that a reader tracing `crc32_combine_gen` out of `zlib.h`
/// L1884 lands somewhere. The width conversion itself belongs to the facade;
/// by the time a length arrives here it is already `i64`.
///
/// The degenerate answers are `crc32_combine_gen64`'s: zero for a negative
/// length, the identity operator `1 << 31` for a zero length.
#[must_use]
pub fn crc32_combine_gen(len2: i64) -> u32 {
    crc32_combine_gen64(len2)
}

/// Combine two CRC-32 check values using a precomputed operator.
///
/// Mirrors `crc32_combine_op` at `crc32.c` L968-L973, and exported
/// unsuffixed and unduplicated as `crc32_combine_op` (`zlib.h` L1890) --
/// it takes no length, so the large-file duality never touches it.
///
/// Given `crc1` over a first sequence, `crc2` over a second, and `op` from
/// `crc32_combine_gen64` for the second sequence's length, the check value of
/// the concatenation is `op * crc1 + crc2` in `GF(2)[x] / p(x)`: one
/// `multmodp` and one exclusive-or.
///
/// # A zero operator returns zero
///
/// `crc32.c` L970-L971 returns `0` when `op == 0`, and this reproduces that
/// literally. It is worth being explicit about what it does *not* return,
/// because every one of these is a plausible-looking "fix" and all of them are
/// wrong: not `crc1`, not `crc2`, and not `crc1 ^ crc2`.
///
/// The guard is also what keeps `multmodp` inside its precondition, since a
/// zero multiplier is the one input the reference implementation cannot handle
/// (`crc32.c` L159-L162). Its practical reach is wider than it looks: because
/// `crc32_combine_gen64` answers `0` for a negative length, a caller who
/// passes a negative length to `crc32_combine64` receives `0`, not `crc1`.
#[must_use]
pub fn crc32_combine_op(crc1: u32, crc2: u32, op: u32) -> u32 {
    // `crc32.c` L970-L971.
    if op == 0 {
        return 0;
    }
    // `crc32.c` L972. The C source masks both check values with `0xffff_ffff`
    // because `uLong` is wider than a CRC-32 on LP64; here the `u32` parameters
    // carry that mask in the type.
    multmodp(op, crc1) ^ crc2
}

/// Combine two CRC-32 check values, given the length of the second sequence.
///
/// Mirrors `crc32_combine64` at `crc32.c` L975-L978. This is the one
/// implementation behind both exported C symbols, `crc32_combine` (`zlib.h`
/// L1874) and `crc32_combine64` (`zlib.h` L1983).
///
/// For sequences `seq1` and `seq2` whose check values are `crc1` and `crc2`,
/// and where `seq2` is `len2` bytes long, the result is the CRC-32 check value
/// of `seq1` followed by `seq2` -- computed from those three numbers alone,
/// touching neither sequence. Use `crc32_combine_gen64` plus
/// `crc32_combine_op` instead when one length recurs, per `zlib.h`
/// L1892-L1894.
///
/// # Degenerate lengths
///
/// * `len2 < 0` returns `0`, per `zlib.h` L1875-L1881: "len2 must be
///   non-negative, otherwise zero is returned". The composition is what
///   produces it -- `crc32_combine_gen64` answers `0`, and
///   `crc32_combine_op` answers `0` for a zero operator -- so the result is
///   `0` rather than `crc1`.
/// * `len2 == 0` yields `crc1 ^ crc2`, because the operator is then the
///   identity. That is the arithmetically right answer: an empty second
///   sequence has check value `0`, and `crc1 ^ 0 == crc1`.
#[must_use]
pub fn crc32_combine64(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine_op(crc1, crc2, crc32_combine_gen64(len2))
}

/// Combine two CRC-32 check values, given the length of the second sequence.
///
/// Mirrors `crc32_combine` at `crc32.c` L980-L983, which is nothing but a
/// widening cast onto `crc32_combine64`. It is retained for the same reason
/// `crc32_combine_gen` is: one core function per exported C symbol, so that
/// the planned `crates/libz-rs-sys/src/checksum.rs` can wire up `zlib.h` L1874 without having
/// to know that the two C entry points share an implementation. The width
/// conversion belongs to the facade; a length reaching here is already `i64`.
///
/// The degenerate answers are `crc32_combine64`'s: `0` for a negative length,
/// `crc1 ^ crc2` for a zero length.
#[must_use]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: i64) -> u32 {
    crc32_combine64(crc1, crc2, len2)
}

#[cfg(test)]
// No `#[allow]` is needed here, and that is deliberate. The workspace denies the
// panic family, and these tests honour it as written: they assert with `assert!`
// family macros, which `clippy::panic` does not flag, and every fallible
// conversion or table read below uses `unwrap_or` with a documented total
// answer, never `unwrap` or a subscript. Should a future test need an escape
// hatch, add it narrowly and say why -- do not blanket-allow the module.
mod tests {
    use super::{
        crc32_combine, crc32_combine64, crc32_combine_gen, crc32_combine_gen64, crc32_combine_op,
        multmodp, tables, x2n_power, x2nmodp, X_POW_0,
    };

    /// One byte-wise CRC-32 step, `crc = (crc >> 8) ^ crc_table[(crc ^ byte) & 0xff]`
    /// -- the inner loop of `crc32_z` at `crc32.c` L936.
    ///
    /// `crc & 0xff` is a single byte by construction and `usize::from(u8)` is
    /// infallible, so neither the conversion nor the lookup can fail.
    fn step(crc: u32, byte: u8) -> u32 {
        let low = u8::try_from(crc & 0xff).unwrap_or(0);
        let entry = tables::CRC_TABLE
            .get(usize::from(low ^ byte))
            .copied()
            .unwrap_or(0);
        (crc >> 8) ^ entry
    }

    /// Fully conditioned reference CRC-32 of `bytes`, seeded as
    /// `crc32(0, buf, len)` is.
    ///
    /// Deliberately independent of every other CRC-32 module in this crate.
    /// It is driven only by `crc_table[]`, transcribed in the sibling `tables`
    /// module and independently re-derived there from `POLY` alone, so a bug
    /// shared with the byte-wise, braided or SIMD engines cannot hide inside
    /// this oracle. `crc32.c` L913 and L939-L940 are the pre- and
    /// post-conditioning steps it reproduces, and
    /// `the_reference_crc_matches_the_catalogue_check_value` pins it to the
    /// published CRC-32 check value before anything else relies on it.
    ///
    /// Taking an iterator rather than a slice is what lets these tests
    /// checksum a concatenation without allocating: chaining, or simply
    /// extending, a byte iterator is literally the CRC of the concatenated
    /// sequence, which keeps this crate's `no_std` posture intact and keeps the
    /// suite fast enough to run under Miri.
    fn reference_crc<I: IntoIterator<Item = u8>>(bytes: I) -> u32 {
        bytes.into_iter().fold(0xffff_ffff, step) ^ 0xffff_ffff
    }

    /// Byte `index` of the deterministic test corpus.
    ///
    /// A multiplicative hash of the position rather than a stored array, so the
    /// corpus is arbitrarily long, allocation-free and free of subscripting.
    /// The shift leaves a value below 256, so the conversion is total.
    fn corpus_byte(index: usize) -> u8 {
        let position = u64::try_from(index).unwrap_or(0);
        let mixed = position.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5bf0_3635;
        u8::try_from(mixed >> 56).unwrap_or(0)
    }

    /// `len` bytes of the corpus beginning at `start`.
    ///
    /// Because the corpus is positional, the first `len1 + len2` bytes *are*
    /// `corpus(0, len1)` followed by `corpus(len1, len2)`. That identity is
    /// what the concatenation tests below rest on.
    fn corpus(start: usize, len: usize) -> impl Iterator<Item = u8> {
        (start..start.saturating_add(len)).map(corpus_byte)
    }

    /// Present a corpus length as the `i64` the combine entry points take.
    ///
    /// Every length used below is a small literal, so the conversion always
    /// succeeds. `-1` is the deliberate choice of impossible fallback: it would
    /// drive the operator to zero and fail the assertion loudly, rather than
    /// quietly agreeing with something.
    fn as_len(len: usize) -> i64 {
        i64::try_from(len).unwrap_or(-1)
    }

    /// Length pairs for the defining property.
    ///
    /// Covers both empty cases and both singleton cases, and straddles the
    /// braided decoder's `N * W + W - 1` threshold in `crc32_z` -- 47 bytes for
    /// the shipped `N == 5`, `W == 8` configuration -- so that either sequence
    /// can be shorter than, exactly, or longer than one braided block.
    const LENGTH_PAIRS: [(usize, usize); 18] = [
        (0, 0),
        (0, 1),
        (1, 0),
        (1, 1),
        (2, 3),
        (7, 8),
        (8, 7),
        (0, 47),
        (47, 0),
        (46, 1),
        (1, 46),
        (39, 8),
        (40, 40),
        (47, 48),
        (63, 64),
        (100, 155),
        (255, 1),
        (1, 255),
    ];

    /// Lengths for the operator paths, spanning one bit, several bits, the
    /// `1 << 29` point at which the table subscript first wraps, and the top of
    /// the `i64` range.
    const GEN_LENGTHS: [i64; 14] = [
        0,
        1,
        2,
        3,
        7,
        8,
        47,
        48,
        255,
        4096,
        1 << 20,
        1 << 29,
        1 << 40,
        i64::MAX >> 1,
    ];

    /// Assorted check values, including both extremes.
    const CHECK_VALUES: [u32; 6] = [0, 1, 0x1234_5678, 0x9abc_def0, 0xdead_beef, 0xffff_ffff];

    #[test]
    fn the_reference_crc_matches_the_catalogue_check_value() {
        // The published CRC-32 "check" value: the check value of the nine ASCII
        // bytes "123456789". The same anchor the sibling `tables` module uses,
        // and the reason this oracle can be trusted by the tests below.
        assert_eq!(reference_crc(b"123456789".iter().copied()), 0xcbf4_3926);
        // The empty message leaves the seed untouched, so its check value is
        // zero -- which is also the value `crc32(0, Z_NULL, 0)` returns per
        // `zlib.h` L1851-L1852.
        assert_eq!(reference_crc(core::iter::empty::<u8>()), 0);
    }

    #[test]
    fn combine_equals_a_direct_crc_of_the_concatenation() {
        for (len1, len2) in LENGTH_PAIRS {
            let first = reference_crc(corpus(0, len1));
            let second = reference_crc(corpus(len1, len2));
            let joined = reference_crc(corpus(0, len1.saturating_add(len2)));
            assert_eq!(
                crc32_combine64(first, second, as_len(len2)),
                joined,
                "crc32_combine64 over lengths ({len1}, {len2})"
            );
            assert_eq!(
                crc32_combine(first, second, as_len(len2)),
                joined,
                "crc32_combine over lengths ({len1}, {len2})"
            );
            // The same result through the split operator form.
            assert_eq!(
                crc32_combine_op(first, second, crc32_combine_gen64(as_len(len2))),
                joined,
                "crc32_combine_op over lengths ({len1}, {len2})"
            );
        }
    }

    #[test]
    fn combining_three_sequences_matches_a_direct_crc() {
        // The shape a caller that checksums chunks independently actually uses:
        // splice a with b, then that with c, and require agreement with one
        // pass over the whole thing. Also checked right-associated, through a
        // reused operator.
        let (first_len, second_len, third_len) = (37_usize, 48_usize, 5_usize);
        let first = reference_crc(corpus(0, first_len));
        let second = reference_crc(corpus(first_len, second_len));
        let third = reference_crc(corpus(first_len + second_len, third_len));
        let whole = reference_crc(corpus(0, first_len + second_len + third_len));

        let left = crc32_combine64(first, second, as_len(second_len));
        assert_eq!(left, reference_crc(corpus(0, first_len + second_len)));
        assert_eq!(crc32_combine64(left, third, as_len(third_len)), whole);

        let right = crc32_combine_op(second, third, crc32_combine_gen64(as_len(third_len)));
        assert_eq!(
            crc32_combine64(first, right, as_len(second_len + third_len)),
            whole
        );
    }

    #[test]
    fn negative_lengths_yield_zero_and_not_the_first_check_value() {
        // `crc32.c` L955-L956 and `zlib.h` L1875-L1888. The composition matters
        // as much as the guard: because the operator is zero,
        // `crc32_combine_op` returns zero too, so the answer is zero -- not
        // `crc1`, not `crc2`, not `crc1 ^ crc2`.
        for len2 in [-1_i64, -2, -47, -(1 << 40), i64::MIN + 1, i64::MIN] {
            assert_eq!(crc32_combine_gen64(len2), 0, "gen64({len2})");
            assert_eq!(crc32_combine_gen(len2), 0, "gen({len2})");
            assert_eq!(crc32_combine64(0x1234_5678, 0x9abc_def0, len2), 0);
            assert_eq!(crc32_combine(0x1234_5678, 0x9abc_def0, len2), 0);
            assert_ne!(
                crc32_combine64(0x1234_5678, 0x9abc_def0, len2),
                0x1234_5678,
                "a negative length must not fall back to crc1"
            );
        }
    }

    #[test]
    fn a_zero_operator_yields_zero() {
        // `crc32.c` L970-L971. Zero for every pair of check values, including
        // pairs where zero is not any of the plausible alternatives.
        for first in CHECK_VALUES {
            for second in CHECK_VALUES {
                assert_eq!(
                    crc32_combine_op(first, second, 0),
                    0,
                    "op == 0 with ({first:#010x}, {second:#010x})"
                );
            }
        }
    }

    #[test]
    fn a_zero_length_second_sequence_uses_the_identity_operator() {
        // A zero length is emphatically not the negative case: the operator is
        // the reflected `x^0 == 1`, and `multmodp` returns its second argument
        // unchanged for it.
        assert_eq!(crc32_combine_gen64(0), X_POW_0);
        assert_eq!(crc32_combine_gen64(0), 1 << 31);
        assert_eq!(crc32_combine_gen(0), X_POW_0);

        for first in CHECK_VALUES {
            // The check value of an empty sequence is zero, so combining with
            // it is the identity on `crc1`.
            assert_eq!(crc32_combine64(first, 0, 0), first);
            assert_eq!(crc32_combine(first, 0, 0), first);
            for second in CHECK_VALUES {
                assert_eq!(crc32_combine64(first, second, 0), first ^ second);
            }
        }

        // The same statement against real data: appending nothing changes
        // nothing.
        let first = reference_crc(corpus(0, 40));
        assert_eq!(
            crc32_combine64(first, reference_crc(core::iter::empty::<u8>()), 0),
            first
        );
    }

    #[test]
    fn gen_and_op_compose_into_combine() {
        // The documented purpose of the split at `zlib.h` L1892-L1894: an
        // operator generated once must give the same answer as the all-in-one
        // entry point, for every length.
        for len2 in GEN_LENGTHS {
            let op = crc32_combine_gen64(len2);
            for first in CHECK_VALUES {
                for second in CHECK_VALUES {
                    assert_eq!(
                        crc32_combine_op(first, second, op),
                        crc32_combine64(first, second, len2),
                        "len2 = {len2}, ({first:#010x}, {second:#010x})"
                    );
                }
            }
        }
    }

    #[test]
    fn the_narrow_aliases_delegate_to_the_wide_implementations() {
        for len2 in GEN_LENGTHS {
            assert_eq!(crc32_combine_gen(len2), crc32_combine_gen64(len2));
            for first in CHECK_VALUES {
                assert_eq!(
                    crc32_combine(first, 0x9abc_def0, len2),
                    crc32_combine64(first, 0x9abc_def0, len2),
                    "len2 = {len2}, crc1 = {first:#010x}"
                );
            }
        }
    }

    #[test]
    fn multmodp_is_total_and_has_the_expected_identity() {
        for value in [0, 1, 2, 0x8000_0000, 0x1234_5678, 0xffff_ffff, tables::POLY] {
            // The identity element in both argument positions: the reflected
            // `x^0 == 1` accumulates its partner and breaks immediately,
            // because no lower bit of the multiplier is set.
            assert_eq!(
                multmodp(X_POW_0, value),
                value,
                "left identity, {value:#010x}"
            );
            assert_eq!(
                multmodp(value, X_POW_0),
                value,
                "right identity, {value:#010x}"
            );
            // A zero multiplier is the input the C loop spins on for ever.
            // Reaching this assertion at all is the substance of the check; the
            // value returned is the correct product besides.
            assert_eq!(multmodp(0, value), 0, "zero multiplier, {value:#010x}");
            assert_eq!(multmodp(value, 0), 0, "zero multiplicand, {value:#010x}");
        }
    }

    #[test]
    fn multmodp_is_commutative_and_associative() {
        // Commutativity and associativity are properties of the polynomial
        // product itself, so they hold for any correct implementation of it --
        // which makes them a cheap simultaneous check on the bit walk, the
        // multiply-by-`x` step and the early break. A transposed shift or a
        // misplaced break breaks one of them.
        let samples = [
            X_POW_0,
            tables::POLY,
            0x4000_0000,
            0x0000_0001,
            0xffff_ffff,
            0x1234_5678,
        ];
        for a in samples {
            for b in samples {
                assert_eq!(multmodp(a, b), multmodp(b, a), "{a:#010x} * {b:#010x}");
                for c in samples {
                    assert_eq!(
                        multmodp(multmodp(a, b), c),
                        multmodp(a, multmodp(b, c)),
                        "({a:#010x} * {b:#010x}) * {c:#010x}"
                    );
                }
            }
        }
    }

    #[test]
    fn x2nmodp_returns_the_identity_for_a_zero_exponent() {
        // `while (n)` never runs, so the seed is the answer -- for any `k`,
        // including values far past the table.
        for k in [0_u32, 3, 31, 32, 99, u32::MAX] {
            assert_eq!(x2nmodp(0, k), X_POW_0, "k = {k}");
        }
    }

    #[test]
    fn x2nmodp_masks_its_table_subscript_with_31() {
        // `k` reaches the table only as `k & 31`, so advancing it by a whole
        // period must change nothing. Without the mask the subscript would
        // leave a 32-element table altogether, which is why this is the check
        // that covers it.
        for n in [1_u64, 2, 3, 0xff, 0x8000_0000, 0xffff_ffff, u64::MAX] {
            for k in [0_u32, 1, 3, 17, 31] {
                assert_eq!(
                    x2nmodp(n, k.wrapping_add(32)),
                    x2nmodp(n, k),
                    "n = {n:#x}, k = {k}"
                );
                assert_eq!(
                    x2nmodp(n, k.wrapping_add(64)),
                    x2nmodp(n, k),
                    "n = {n:#x}, k = {k}"
                );
            }
        }
        // Every subscript the mask can produce is in range, and reading past
        // the period repeats rather than escapes.
        for k in 0..96_u32 {
            assert_eq!(x2n_power(k), x2n_power(k & 31), "k = {k}");
        }
        // `k` wrapping the whole of `u32` is harmless as well, because 2^32 is
        // a multiple of 32.
        assert_eq!(x2n_power(u32::MAX), x2n_power(31));
        assert_eq!(x2nmodp(u64::MAX, u32::MAX), x2nmodp(u64::MAX, 31));
    }

    #[test]
    fn a_length_past_2_pow_29_drives_the_masked_subscript() {
        // `crc32_combine_gen64` starts `k` at 3 and advances it once per bit of
        // the length, so `k` first exceeds 31 at a length of `1 << 29`. That
        // length has a single set bit, at position 29, so exactly one table
        // read happens and its subscript is `(3 + 29) & 31 == 0`: the operator
        // is `x2n_table[0]` itself.
        assert_eq!(x2n_power(0), 0x4000_0000);
        assert_eq!(crc32_combine_gen64(1 << 29), x2n_power(0));
        assert_eq!(crc32_combine_gen64(1 << 29), x2nmodp(1, 0));
        // One bit further along wraps to the next entry.
        assert_eq!(crc32_combine_gen64(1 << 30), x2n_power(1));
        assert_eq!(crc32_combine_gen64(1 << 31), x2n_power(2));
    }

    #[test]
    fn the_operator_is_multiplicative_in_the_length() {
        // `gen(a + b) == gen(a) * gen(b)`, because `x^(8(a+b))` factors as
        // `x^(8a) * x^(8b)`. Combined with
        // `combine_equals_a_direct_crc_of_the_concatenation`, which ties the
        // operator to real data, this is what justifies combining
        // incrementally.
        //
        // The last four pairs push the table subscript past 31 in one or both
        // operands, so the law is checked on both sides of the wrap. It holds
        // there for the reason the next test asserts: the reduction is exact.
        for (first_len, second_len) in [
            (0_i64, 0_i64),
            (0, 1),
            (1, 1),
            (7, 40),
            (47, 48),
            (1000, 12_345),
            (1 << 27, 1 << 27),
            (1 << 28, 1 << 28),
            (1 << 29, 1 << 29),
            (1 << 40, 1 << 40),
            ((1 << 45) + 7, (1 << 50) + 11),
        ] {
            assert_eq!(
                crc32_combine_gen64(first_len + second_len),
                multmodp(
                    crc32_combine_gen64(first_len),
                    crc32_combine_gen64(second_len)
                ),
                "({first_len}, {second_len})"
            );
        }
    }

    #[test]
    fn the_table_of_powers_is_cyclic_so_the_masked_subscript_is_exact() {
        // This is the premise every length of `1 << 29` bytes or more depends
        // on, and it is not obvious, so it is asserted rather than assumed.
        //
        // `x2n_table[j]` is `x^(2^j)`, built by squaring (`crc32.c`
        // L259-L262). Squaring entry `j` must therefore give entry `j + 1` --
        // and squaring the LAST entry gives entry 0 again, because `x^(2^32)`
        // is congruent to `x^(2^0)` modulo the CRC-32 polynomial. The table is
        // consequently cyclic with period 32, which is why reducing a subscript
        // with `k & 31` (`crc32.c` L190) selects exactly the polynomial the
        // unreduced exponent names instead of merely a bounded stand-in for it.
        //
        // Anyone tempted to widen the table or to drop the mask should start
        // here: the mask is not a truncation, and there is nothing to fix.
        for k in 0..32_u32 {
            assert_eq!(
                multmodp(x2n_power(k), x2n_power(k)),
                x2n_power(k.wrapping_add(1)),
                "squaring x2n_table[{k}]"
            );
        }
        // Spelled out for the wrap specifically, since that is the case at
        // issue.
        assert_eq!(multmodp(x2n_power(31), x2n_power(31)), x2n_power(0));

        // And the consequence at the public entry point: a length of `1 << 29`
        // has a single set bit, at position 29, so exactly one table read
        // happens, with subscript `(3 + 29) & 31 == 0`.
        assert_eq!(
            crc32_combine_gen64(1 << 29),
            multmodp(crc32_combine_gen64(1 << 28), crc32_combine_gen64(1 << 28))
        );
    }

    #[test]
    fn very_large_lengths_terminate_without_panicking() {
        // At most 63 iterations, one per bit of the length, and every one of
        // them is plain shifting and masking. Reaching the end of this test is
        // the assertion; running it in a debug build, where the arithmetic
        // overflow checks are live, is what gives it teeth.
        for len2 in [
            (1 << 40) + 1,
            1 << 62,
            i64::MAX >> 1,
            i64::MAX - 1,
            i64::MAX,
        ] {
            let op = crc32_combine_gen64(len2);
            // A product of units is a unit, so the operator is never zero for a
            // non-negative length -- which is what keeps `crc32_combine_op`
            // out of its zero-operator branch here.
            assert_ne!(op, 0, "len2 = {len2}");
            assert_eq!(
                crc32_combine_op(0x1234_5678, 0x9abc_def0, op),
                crc32_combine64(0x1234_5678, 0x9abc_def0, len2),
                "len2 = {len2}"
            );
        }
    }
}
