//! Byte-at-a-time CRC-32 -- the reference path the rest of this module is measured against.
//!
//! One table lookup and one shift per input byte is the whole of the algorithm. This is
//! deliberately not the fast path: `braid.rs` consumes a machine word at a time and `simd.rs`
//! more than that. It is instead the *definition* of what those paths must produce, because both
//! are validated by agreeing with this module bit for bit. The code below is therefore written
//! for evident correctness first and for speed second.
//!
//! # The step
//!
//! `crc32.c` writes the recurrence identically in all three places it appears -- once in the
//! word-alignment prologue of the braided path (L649), and twice in the tail that finishes
//! whatever the braids left over (L923-L932, hand-unrolled eight ways, and L936):
//!
//! ```text
//! crc = (crc >> 8) ^ crc_table[(crc ^ *buf++) & 0xff];
//! ```
//!
//! `crc_table` holds the CRC of every possible byte value. `crc32.c` L248-L252 builds it by
//! running each of the 256 values through eight bit steps of the reflected polynomial
//! `POLY = 0xedb88320` (`crc32.c` L157), so one lookup stands in for eight conditional shifts
//! and nothing else about the computation changes. The table itself is transcribed in the
//! sibling `tables` module as `CRC_TABLE`; this module never rebuilds it and never owns a copy.
//!
//! # Pre-conditioning is the caller's business
//!
//! **Every function here takes and returns an already pre-conditioned state, and applies neither
//! the leading nor the trailing one's complement.** Pass a state that has already been
//! complemented; expect a state that still needs complementing. The two conditioning steps --
//! `crc = (~crc) & 0xffffffff` at `crc32.c` L635 and `return crc ^ 0xffffffff` at L940 -- happen
//! exactly once each, in the parent module, together with the `buf == Z_NULL` early return at
//! `crc32.c` L628.
//!
//! Two things depend on that division of labour:
//!
//! * `braid.rs` and `simd.rs` call in here *mid-computation*, for the bytes their wide paths
//!   cannot consume: the prologue that reaches a word boundary and the remainder that trails the
//!   last full block. Conditioning applied here would be applied a second time, and the result
//!   would be a plausible-looking CRC that matches nothing.
//! * The C entry point is resumable -- `crc32_z(crc32_z(0, a, la), b, lb)` is the CRC of `a`
//!   followed by `b`, because one call's post-conditioning is undone by the next call's
//!   pre-conditioning. Hoisting both complements to the boundary makes that identity structural
//!   rather than incidental, and it is what the gzip layer leans on while it accumulates a
//!   checksum over successive `deflate` calls.
//!
//! Getting this wrong yields exactly the complement of the right answer, which looks like a
//! table fault or an endianness fault and is neither.
//!
//! # The backend contract
//!
//! [`Generic`] is this path's implementation of the `Crc32Backend` trait. The trait belongs to
//! the parent module, which owns backend selection; this module supplies one implementation of
//! it and nothing else. The contract is:
//!
//! ```text
//! pub trait Crc32Backend {
//!     /// Short identifier for diagnostics and benchmark labels.
//!     const NAME: &'static str;
//!
//!     /// Folds `buf` into the already pre-conditioned state `crc`.
//!     fn update(crc: u32, buf: &[u8]) -> u32;
//! }
//! ```
//!
//! [`crc32_generic`] stays available as a plain function too, because the other backends and the
//! parent module call it directly on prologue and remainder bytes rather than through the trait,
//! and because a free function over `(u32, &[u8])` that keeps no hidden state is what makes the
//! differential comparison against the C `crc32_z` trivial to write.
//!
//! # Layering and safety posture
//!
//! The module names only `core` and the sibling table: no allocation, no interior mutability, no
//! platform detection, no third-party crate. It holds no raw pointer and declares neither a
//! C-visible layout nor C-visible linkage for anything, so the crate root's blanket prohibition
//! on escape hatches from the compiler's memory-safety guarantees costs it nothing. Every index
//! it forms is derived from a `u8` and the one table it reads has 256 entries, so no access here
//! can be out of range and none of them can panic. Exporting `crc32` and `crc32_z` themselves is
//! the business of the `libz-rs-sys` facade.
//!
//! # Examples
//!
//! ```ignore
//! // A whole buffer in one call, with the conditioning the parent module applies.
//! let state = crc32_generic(!0u32, b"123456789");
//! assert_eq!(state ^ 0xffff_ffff, 0xcbf4_3926);
//!
//! // Resumable: the state carries across calls, so any split gives the same answer.
//! assert_eq!(crc32_generic(crc32_generic(!0u32, b"12345"), b"6789"), state);
//! ```

use super::tables::CRC_TABLE;

// The table has to cover every byte value, because the lookup below is indexed by one. A build
// error here means `CRC_TABLE` is no longer 256 entries long, which would quietly turn that
// lookup into its fallback; pinning the length at compile time makes the fallback provably dead
// code rather than merely improbable. Deliberately not a `debug_assert!` -- there is nothing to
// check at run time once the compiler has settled it.
const _: () = assert!(CRC_TABLE.len() == 256);

/// Bytes consumed per iteration of the bulk loop.
///
/// Mirrors the eight-fold hand-unrolling of `while (len >= 8)` at `crc32.c` L923-L933. The value
/// is output-neutral: it changes only how many steps the compiler sees per iteration, never the
/// state those steps produce.
const UNROLL: usize = 8;

/// One byte of CRC-32: the single expression the reference implementation repeats everywhere.
///
/// Computes `(crc >> 8) ^ crc_table[(crc ^ byte) & 0xff]`, which is `crc32.c` L649, L923-L932
/// and L936 verbatim. `crc` is an already pre-conditioned state and the result is another one;
/// no complement is applied at either end.
#[inline]
fn crc32_byte(crc: u32, byte: u8) -> u32 {
    // `to_le_bytes` yields the least-significant byte first whatever the target's endianness, so
    // this is `crc & 0xff` obtained without a narrowing cast.
    let [low, ..] = crc.to_le_bytes();

    // Widening a `u8` with `usize::from` can only produce `0..=255`, and the assertion above
    // pins the table at 256 entries, so the `None` arm is unreachable. The fallback is here to
    // keep the hottest loop in the module free of a panicking path, not to paper over a real
    // miss -- and it is free: at `-O` this compiles to precisely the instruction sequence that
    // `CRC_TABLE[index]` does, with no bounds check emitted either way.
    let entry = CRC_TABLE.get(usize::from(low ^ byte)).copied().unwrap_or(0);

    (crc >> 8) ^ entry
}

/// Folds `buf` into the CRC state `crc`, one byte at a time.
///
/// This is the tail of `crc32_z` (`crc32.c` L922-L940) lifted into a function of its own, and it
/// is a complete CRC-32 engine in its own right: handed a whole buffer it produces exactly what
/// the braided path produces, only slower.
///
/// Pass an already pre-conditioned state and treat the result as one: apply the leading `!crc`
/// before the first call and the trailing `crc ^ 0xffff_ffff` after the last, both of which
/// belong to this module's parent (`crc32.c` L635 and L940). Nothing here complements anything,
/// which is exactly what lets `braid.rs` and `simd.rs` call this in the middle of a computation
/// for the bytes their wide paths cannot take.
///
/// # Examples
///
/// ```ignore
/// // The published CRC-32 check value, computed through the parent module's conditioning.
/// assert_eq!(crc32_generic(!0u32, b"123456789") ^ 0xffff_ffff, 0xcbf4_3926);
///
/// // An empty slice is the identity on the state, so a zero-length flush is free.
/// assert_eq!(crc32_generic(0x1234_5678, &[]), 0x1234_5678);
/// ```
#[must_use]
#[inline]
pub fn crc32_generic(mut crc: u32, buf: &[u8]) -> u32 {
    // `crc32.c` L923-L933 takes eight bytes per iteration and L934-L937 mops up what is left.
    // Splitting the loop that way is a performance device and nothing more: the step is a fold
    // over the bytes in order, so grouping them changes only how much of the loop the optimiser
    // sees at once, never the value produced. `chunks_exact` is the safe spelling of that
    // grouping, and `remainder` is the tail it holds back -- fixed when the iterator is built,
    // so it stays correct however far the loop above ran.
    let mut blocks = buf.chunks_exact(UNROLL);

    for block in blocks.by_ref() {
        for &byte in block {
            crc = crc32_byte(crc, byte);
        }
    }

    for &byte in blocks.remainder() {
        crc = crc32_byte(crc, byte);
    }

    crc
}

/// The byte-at-a-time backend: `crc32.c` L922-L940 with no wide path underneath it.
///
/// A zero-sized marker -- the work is [`crc32_generic`], which callers inside the crate may also
/// invoke directly. It exists so that backend selection in the parent module can name this path
/// the same way it names the braided and vectorised ones, and so benchmarks can label it.
///
/// Like every function in this module, the backend takes and returns a pre-conditioned state and
/// complements nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Generic;

impl super::Crc32Backend for Generic {
    /// Identifies the scalar reference path in diagnostics and benchmark labels.
    const NAME: &'static str = "generic";

    /// Folds `buf` into the already pre-conditioned state `crc` by forwarding, unchanged, to
    /// [`crc32_generic`].
    #[inline]
    fn update(crc: u32, buf: &[u8]) -> u32 {
        crc32_generic(crc, buf)
    }
}

#[cfg(test)]
mod tests {
    use crate::crc32::Crc32Backend;

    use super::{crc32_byte, crc32_generic, Generic, UNROLL};

    /// The reflected CRC-32 polynomial, `#define POLY 0xedb88320` at `crc32.c` L157.
    ///
    /// Restated here rather than imported from the `tables` module on purpose: the reference
    /// implementation below has to be independent of the transcribed table for its agreement with
    /// the table-driven path to establish anything.
    const POLY: u32 = 0xedb8_8320;

    /// Length of the [`ramp`] fixture: ten whole unrolled blocks, so that truncations of it cover
    /// a full block, a partial block and every offset in between.
    const RAMP_LEN: usize = 10 * UNROLL;

    /// Length of the [`pseudo_random`] fixture: sixteen whole unrolled blocks.
    const PSEUDO_RANDOM_LEN: usize = 16 * UNROLL;

    /// Oracle-verified CRC-32 values, each one the result of `crc32_z(0, buf, len)` from the
    /// in-tree C implementation.
    ///
    /// `"123456789"` is the published check value for this CRC. `"hello, hello!"` and `"hello"`
    /// are the payload and the preset dictionary of `test/example.c` (L35 and L40), so they are
    /// the strings the existing suite really does push through the library.
    const VECTORS: [(&[u8], u32); 7] = [
        (b"", 0x0000_0000),
        (b"a", 0xe8b7_be43),
        (&[0x00], 0xd202_ef8d),
        (b"hello", 0x3610_a686),
        (b"hello, hello!", 0xb39a_dc9b),
        (b"123456789", 0xcbf4_3926),
        (b"The quick brown fox jumps over the lazy dog", 0x414f_a339),
    ];

    /// Pre-conditioned states to seed the sweeps with: the two extremes (`0`, and the `!0` that
    /// `crc32.c` L635 produces from a zero seed) plus three interior values -- one with bits set
    /// only in the byte the table lookup consumes, one with bits set only in the byte that
    /// survives the shift, and one with bits scattered across all four.
    const SEEDS: [u32; 5] = [
        0x0000_0000,
        0xffff_ffff,
        0x0000_00ff,
        0xff00_0000,
        0x9e37_79b9,
    ];

    /// One CRC-32 byte step computed from [`POLY`] alone: eight conditional shifts, no table.
    ///
    /// `crc32.c` L248-L252 fills `crc_table` with exactly this inner loop, and the step is linear
    /// over GF(2), so folding a byte through eight bit steps and looking that byte up in the table
    /// are the same function. Agreement between this and [`crc32_byte`] therefore checks the
    /// transcribed table and the shift-and-exclusive-or step against first principles at once.
    fn bitwise_byte(crc: u32, byte: u8) -> u32 {
        let mut value = crc ^ u32::from(byte);
        for _ in 0..8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                (value >> 1) ^ POLY
            };
        }
        value
    }

    /// Table-free CRC-32 over `bytes`, seeded with the pre-conditioned state `crc`.
    fn bitwise(crc: u32, bytes: &[u8]) -> u32 {
        bytes
            .iter()
            .fold(crc, |state, &byte| bitwise_byte(state, byte))
    }

    /// Wrap [`crc32_generic`] in the conditioning that `crc32_z` applies at `crc32.c` L635 and
    /// L940, so that a result can be compared against a published CRC-32 value.
    fn conditioned(bytes: &[u8]) -> u32 {
        crc32_generic(!0, bytes) ^ 0xffff_ffff
    }

    /// Deterministic byte ramp, `b[i] = (3 + 7 * i) mod 256`.
    ///
    /// Built by repeated `wrapping_add` so the fixture needs no narrowing cast, and stepped by 7
    /// because `gcd(7, 256) == 1`: no byte value repeats inside the fixture, which keeps a
    /// transposition of two of them detectable.
    fn ramp() -> [u8; RAMP_LEN] {
        let mut bytes = [0u8; RAMP_LEN];
        let mut value = 3u8;
        for slot in &mut bytes {
            *slot = value;
            value = value.wrapping_add(7);
        }
        bytes
    }

    /// Every byte value exactly once, ascending: the fixture that touches all 256 table rows.
    fn every_byte_value() -> [u8; 256] {
        let mut bytes = [0u8; 256];
        for (slot, value) in bytes.iter_mut().zip(0u8..=u8::MAX) {
            *slot = value;
        }
        bytes
    }

    /// Pseudo-random fixture from a linear congruential generator.
    ///
    /// Takes bits 16 to 23 of each state, which is byte 2 of the little-endian encoding and so
    /// needs no cast: the low bits of such a generator are famously non-random, and a fixture with
    /// a short period in its low bits would exercise far fewer table rows than it appears to.
    fn pseudo_random() -> [u8; PSEUDO_RANDOM_LEN] {
        let mut bytes = [0u8; PSEUDO_RANDOM_LEN];
        let mut state = 0x2b3c_4d5e_u32;
        for slot in &mut bytes {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let [_, _, high_middle, _] = state.to_le_bytes();
            *slot = high_middle;
        }
        bytes
    }

    #[test]
    fn check_value_matches_the_crc_32_standard() {
        assert_eq!(conditioned(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn reference_vectors_match_the_c_implementation() {
        for (bytes, expected) in VECTORS {
            assert_eq!(conditioned(bytes), expected);
            assert_eq!(crc32_generic(!0, bytes), bitwise(!0, bytes));
        }
    }

    #[test]
    fn array_fixtures_match_the_c_implementation() {
        let all_bytes = every_byte_value();

        assert_eq!(conditioned(&[0x00; 64]), 0x758d_6336);
        assert_eq!(conditioned(&[0xff; 64]), 0x0f61_87ba);
        assert_eq!(conditioned(&all_bytes), 0x2905_8c73);
        assert_eq!(conditioned(&pseudo_random()), 0x2782_cff8);
        assert_eq!(crc32_generic(!0, &all_bytes), bitwise(!0, &all_bytes));

        let ramp = ramp();
        assert_eq!(conditioned(&ramp), 0x3f42_d103);

        // Truncations placed either side of the unrolled block boundary: eight whole blocks, one
        // whole block, and one byte short of a block so that only the tail loop runs.
        let (first_64, _) = ramp.split_at(8 * UNROLL);
        assert_eq!(conditioned(first_64), 0xcbd9_ecf0);
        let (first_8, _) = ramp.split_at(UNROLL);
        assert_eq!(conditioned(first_8), 0xe2e3_5978);
        let (first_7, _) = ramp.split_at(UNROLL - 1);
        assert_eq!(conditioned(first_7), 0x5449_1cdb);
    }

    #[test]
    fn the_returned_state_is_still_pre_conditioned() {
        let state = crc32_generic(!0, b"123456789");

        // What comes back is one complement away from the value a caller of `crc32_z` sees, and
        // applying that complement is the parent module's job (`crc32.c` L940). If this module
        // ever conditioned its own result, `state` would equal the published value below and
        // every mid-computation caller in `braid.rs` and `simd.rs` would be silently wrong.
        assert_eq!(state, 0xcbf4_3926 ^ 0xffff_ffff);
        assert_eq!(state, 0x340b_c6d9);
        assert_ne!(state, 0xcbf4_3926);
    }

    #[test]
    fn an_empty_slice_leaves_the_state_untouched() {
        for state in SEEDS {
            assert_eq!(crc32_generic(state, &[]), state);
        }
    }

    #[test]
    fn splitting_the_input_anywhere_yields_the_same_state() {
        let ramp = ramp();
        let whole = crc32_generic(!0, &ramp);

        // Exhaustive over every split point, so every combination of block-aligned and unaligned
        // head with block-aligned and unaligned tail is covered. This is the property the gzip
        // layer depends on when it accumulates a checksum across successive `deflate` calls.
        for split in 0..=RAMP_LEN {
            let (head, tail) = ramp.split_at(split);
            assert_eq!(crc32_generic(crc32_generic(!0, head), tail), whole);
        }
    }

    #[test]
    fn feeding_one_byte_at_a_time_matches_one_call() {
        let fixture = pseudo_random();
        let byte_at_a_time = fixture
            .iter()
            .fold(!0, |state, &byte| crc32_generic(state, &[byte]));

        assert_eq!(byte_at_a_time, crc32_generic(!0, &fixture));
    }

    #[test]
    fn every_length_agrees_with_the_table_free_reference() {
        let fixture = pseudo_random();

        // Lengths 0 through 128 cover an empty input, both loops on their own, and both loops
        // together with every possible remainder size.
        for state in SEEDS {
            for len in 0..=PSEUDO_RANDOM_LEN {
                let (head, _) = fixture.split_at(len);
                assert_eq!(crc32_generic(state, head), bitwise(state, head));
            }
        }
    }

    #[test]
    fn a_length_that_is_not_a_whole_number_of_blocks_uses_both_loops() {
        // Thirteen bytes is one full unrolled block plus a five-byte remainder, so a single call
        // has to run both loops of `crc32.c` L923-L937 to arrive at the right answer.
        const ODD_LEN: usize = UNROLL + 5;

        let ramp = ramp();
        let (odd, _) = ramp.split_at(ODD_LEN);

        assert_eq!(conditioned(odd), 0xa97a_d5c1);
        assert_eq!(crc32_generic(!0, odd), bitwise(!0, odd));

        // Fed as block-then-remainder the same bytes must land on the same state, which is what
        // makes the split of the loop a performance device and not a behavioural one.
        let (block, remainder) = odd.split_at(UNROLL);
        assert_eq!(
            crc32_generic(crc32_generic(!0, block), remainder),
            crc32_generic(!0, odd)
        );
    }

    #[test]
    fn the_single_byte_step_agrees_with_eight_bit_steps() {
        for state in SEEDS {
            for byte in 0u8..=u8::MAX {
                assert_eq!(crc32_byte(state, byte), bitwise_byte(state, byte));
            }
        }
    }

    #[test]
    fn the_backend_forwards_to_the_free_function() {
        assert_eq!(Generic::NAME, "generic");

        let fixture = ramp();
        for (bytes, _) in VECTORS {
            assert_eq!(Generic::update(!0, bytes), crc32_generic(!0, bytes));
        }
        for state in SEEDS {
            assert_eq!(
                Generic::update(state, &fixture),
                crc32_generic(state, &fixture)
            );
        }
    }
}
