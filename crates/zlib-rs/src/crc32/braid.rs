//! Braided, word-at-a-time CRC-32 -- the fast path, and the module the throughput gate rests on.
//!
//! A CRC is a running remainder, and a remainder is inherently serial: byte `i + 1` cannot be
//! folded in until byte `i` has been. `generic.rs` does exactly that, one table lookup and one
//! shift per byte, and on a pipelined core it leaves most of the arithmetic-logic units idle
//! waiting on the dependency chain. The braid breaks the chain. `crc32.c` L39-L49 states the
//! construction:
//!
//! > A CRC of a message is computed on N braids of words in the message, where each word consists
//! > of W bytes (4 or 8). If N is 3, for example, then three running sparse CRCs are calculated
//! > respectively on each braid, at these indices in the array of words: 0, 3, 6, ..., 1, 4, 7,
//! > ..., and 2, 5, 8, ... This is done starting at a word boundary, and continues until as many
//! > blocks of N * W bytes as are available have been processed. The results are combined into a
//! > single CRC at the end.
//!
//! The `N` remainders are independent, so the processor can have all of them in flight at once;
//! the interleaved design is due to Kadatch and Jenkins (2010), credited at `crc32.c` L5-L7, and
//! the paper is carried in this distribution at `doc/crc-doc.1.0.pdf`. `N` is `5` and `W` is `8`
//! on a 64-bit target (`crc32.c` L60-L106), both supplied by the sibling `tables` module, which
//! also holds the tables that make a braid step one lookup per byte instead of a loop over bits.
//!
//! Neither `N` nor `W` is observable. They partition the input differently and combine the pieces
//! differently, and they arrive at the same 32-bit residue as the byte-at-a-time path for every
//! input, which is what the tests below assert at every length and every start offset.
//!
//! # Shape of the computation
//!
//! [`crc32_braid`] is `crc32_z`'s `#ifdef W` block (`crc32.c` L637-L920) followed by the handover
//! at L917, and it runs in four stages:
//!
//! 1. **Threshold** (`crc32.c` L640). Below `N * W + W - 1` bytes there is not enough input to
//!    pay for the setup, and the reference implementation falls straight through to the byte
//!    loop. So does this: [`crc32_braid`] delegates the whole buffer to `generic::crc32_generic`.
//! 2. **Prologue** (`crc32.c` L646-L650). Bytes are taken one at a time until the read cursor
//!    reaches a `W`-byte boundary.
//! 3. **Braided blocks** (`crc32.c` L652-L912). Whole `N * W`-byte blocks are folded, the first
//!    `blks - 1` of them advancing the `N` remainders independently and the last combining them
//!    into one.
//! 4. **Epilogue** (`crc32.c` L922-L937). Whatever the blocks left over goes back through
//!    `generic::crc32_generic`.
//!
//! Stages 2 and 4 are deliberately not reimplemented here: the byte step exists once, in
//! `generic.rs`, and both ends of this module call it.
//!
//! # Pre-conditioning is the caller's business
//!
//! **Every function here takes and returns an already pre-conditioned state, and applies neither
//! the leading nor the trailing one's complement**, exactly as `generic.rs` does. The two
//! conditioning steps -- `crc = (~crc) & 0xffffffff` at `crc32.c` L635 and
//! `return crc ^ 0xffffffff` at L940 -- happen once each in the parent module, together with the
//! `buf == Z_NULL` early return at L628. Conditioning anything here would corrupt every
//! multi-call checksum, because this path is entered mid-computation whenever the gzip layer
//! accumulates a checksum across successive `deflate` calls.
//!
//! # Why alignment cannot make this incorrect, and why the prologue is kept anyway
//!
//! The reference implementation reads words through `*(z_word_t const *)buf` (`crc32.c` L712),
//! which requires `buf` to be suitably aligned -- hence the prologue at L646-L650. This implementation
//! reads each word by copying `W` bytes into an array and calling `Word::from_le_bytes` or
//! `Word::from_be_bytes`, which have no alignment precondition whatsoever. **Correctness here
//! therefore does not depend on the prologue at all**, and the module needs no unaligned-load
//! escape hatch: no `align_to`, no `read_unaligned`, no transmute, nothing that would breach the
//! crate root's prohibition on unsafe code.
//!
//! The prologue is kept regardless, for two reasons. It reproduces the reference
//! implementation's memory-access pattern, which is what the "within 10% of C zlib" throughput
//! requirement is measured against -- a word load that straddles a cache line costs real time
//! even where it is legal. And it keeps the correspondence with `crc32.c` legible: a reader
//! comparing the two sources finds the same four stages in the same order. It is also
//! *output-neutral*, which is worth stating plainly: moving a byte between the byte-at-a-time
//! path and the braided path cannot change the answer, because both compute the same function of
//! the same bytes in the same order. That is precisely what the alignment sweep in the tests
//! below demonstrates.
//!
//! # The endian branch, and why each half is compiled everywhere
//!
//! `crc32.c` L657-L662 chooses between two block loops with a deliberate run-time test:
//!
//! > Do endian check at execution time instead of compile time, since ARM processors can change
//! > the endianness at execution time. If the compiler knows what the endianness will be, it can
//! > optimize out the check and the unused branch.
//!
//! The test is reproduced as a run-time test for the same reason, and both halves --
//! [`fold_blocks_little_endian`] and [`fold_blocks_big_endian`] -- are compiled on **every**
//! target rather than one being `cfg`-ed away. Handling only little-endian words would silently
//! produce wrong checksums on s390x or big-endian PowerPC, and this portable software path is the
//! only thing standing behind those targets: the S390X vector CRC hooks in `contrib/crc32vx/` are
//! out of scope for this implementation.
//!
//! One substitution deserves its exactness argument spelled out, because it looks like a change
//! and is not. The C branches read words with the *native* load in both halves and differ in
//! which tables they consult; the two halves exist because the byte the machine puts in the
//! low-order position differs. This implementation instead reads words with an *explicit* endianness in
//! each half -- `from_le_bytes` in the little half, `from_be_bytes` in the big half. On a
//! little-endian target `from_le_bytes` **is** the native load and the little half is the C's
//! little branch instruction for instruction; on a big-endian target `from_be_bytes` **is** the
//! native load and the big half is the C's big branch. Since the run-time test always selects
//! the half whose load is native, the generated code is the C's on both, and no byte-swap is
//! introduced on either.
//!
//! What the substitution buys is testability. Each half now computes the same function of the
//! same byte stream on any host, so the big-endian block loop -- tables, `crc_word_big`, the
//! `byte_swap` at both ends of it -- can be executed and checked on the `x86_64` machines
//! this project is tested on, instead of being dead code no test can reach. The tests below do
//! exactly that, and assert the two halves agree with each other and with the byte-at-a-time
//! path.
//!
//! # No table is ever generated at run time
//!
//! `crc32.c` L461-L473 carries a `braid()` generator, live only under `DYNAMIC_CRC_TABLE`, that
//! fills the braid tables on first use. This implementation has no such path: the tables are `const` data
//! in the `tables` module, so there is no `z_once_t`, no `OnceLock`, no `static mut` and no lazy
//! initialization here or anywhere near here -- and therefore none of the data race `crc32.c`
//! L12-L17 warns about. `braid()` itself is reproduced only inside `#[cfg(test)]`, where it
//! regenerates the tables from the polynomial and asserts the transcription matches.
//!
//! # Layering and safety posture
//!
//! The module names only `core` and its two siblings, `tables` and `generic`: no allocation, no
//! interior mutability, no platform detection, no third-party crate, no raw pointer beyond the
//! read-only address arithmetic of the prologue, and no `unsafe`. Every table subscript is
//! derived from a `u8` and every table row has 256 entries, so no access can be out of range;
//! every loop is bounded by a slice length or by `W`; and every fallible step answers with the
//! byte-at-a-time engine, so each of this module's provably unreachable arms still returns the
//! *correct* checksum rather than merely avoiding a panic. Exporting `crc32` and `crc32_z`
//! themselves is the business of the `libz-rs-sys` facade.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::crc32::{Braid, Crc32Backend, Generic};
//!
//! // Long enough to take the braided path, with the conditioning the parent module applies.
//! let data = [0xa5u8; 1024];
//! let state = Braid::update(!0u32, &data);
//! assert_eq!(state, Generic::update(!0u32, &data));
//!
//! // Resumable, and independent of where the input is split.
//! let (head, tail) = data.split_at(37);
//! assert_eq!(Braid::update(Braid::update(!0u32, head), tail), state);
//! ```
//!
//! The examples name the [`Braid`](super::Braid) and [`Generic`](super::Generic) backends
//! rather than the free functions, which are crate-private and which the backends forward
//! to unchanged.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use super::generic::crc32_generic;
use super::tables::{Word, CRC_BIG_TABLE, CRC_BRAID_BIG_TABLE, CRC_BRAID_TABLE, CRC_TABLE, N, W};

// `W` and `Word` must describe the same width, because the byte arrays this module forms are
// `[u8; W]` while the values it builds from them are `Word`. `tables` derives both from one
// `cfg`, so this can only break if that pairing is edited apart -- which would otherwise show up
// as a confusing type error deep in a loop rather than as a statement about the invariant.
const _: () = assert!(
    W == size_of::<Word>(),
    "crc32.c L96-L106 pairs W with z_word_t"
);

// The braid tables have `W` rows -- one per byte position within a word -- not `N`.
// `crc32.c` L205-L206 declares them `[W][256]`, and the loops below zip the rows against the `W`
// bytes of a word, so a row count that disagreed with `W` would quietly stop folding some byte
// positions in.
const _: () = assert!(
    CRC_BRAID_TABLE.len() == W,
    "crc32.c L205: crc_braid_table[W][256]"
);
const _: () = assert!(
    CRC_BRAID_BIG_TABLE.len() == W,
    "crc32.c L206: crc_braid_big_table[W][256]"
);

// Both byte-wise tables must cover every byte value, because the lookups below are indexed by
// one. Pinning the lengths makes each lookup's fallback provably dead code.
const _: () = assert!(CRC_TABLE.len() == 256, "crc32.c L202: crc_table[256]");
const _: () = assert!(
    CRC_BIG_TABLE.len() == 256,
    "crc32.c L204: crc_big_table[256]"
);

// At least one braid must exist: the final block seeds the combination from braid 0 and folds the
// rest into it, so `N == 0` would have nothing to seed from. `crc32.c` L66-L68 restricts `N` to
// `1..=6` with an `#error`, and this is that check.
const _: () = assert!(N >= 1 && N <= 6, "crc32.c L66-L68 restricts N to 1..=6");

/// Bytes in one braided block: one `W`-byte word for each of the `N` braids.
///
/// This is the `N * W` that `crc32.c` L653-L654 divides the remaining length by, and the stride
/// `words += N` advances by at L728 and L850.
const BLOCK: usize = N * W;

/// Smallest input for which the braided path is entered at all.
///
/// Mirrors `if (len >= N * W + W - 1)` at `crc32.c` L640: one whole block, plus the up to
/// `W - 1` bytes the prologue may have to spend reaching a word boundary. Below this length the
/// reference implementation runs the byte loop alone, and so does `crc32_braid`.
///
/// The threshold is output-neutral -- a shorter or longer one would produce the same checksums --
/// and is reproduced exactly anyway, so that this implementation's work is split across the two paths at
/// the same lengths as the reference implementation's. That keeps the throughput comparison an
/// honest one and makes an intermediate state from either implementation directly comparable with
/// the other's.
const MIN_BRAID_LEN: usize = BLOCK + W - 1;

/// Widen a 32-bit CRC residue to a [`Word`], on a target where [`Word`] is 64 bits.
///
/// The reference implementation gets this widening for free: `crc0` is a `z_crc_t` and `words[0]`
/// a `z_word_t`, so `crc0 ^ words[0]` (`crc32.c` L712) promotes the narrower operand by C's usual
/// arithmetic conversions. Rust has no such promotion, so the widening is written out -- as a
/// `From` conversion rather than an `as` cast, because it cannot lose information and should not
/// be spelled like something that might.
///
/// The width is selected with the same `cfg` the `tables` module uses to select [`Word`] itself.
#[cfg(target_pointer_width = "64")]
#[inline]
fn widen(residue: u32) -> Word {
    Word::from(residue)
}

/// Widen a 32-bit CRC residue to a [`Word`]. The identity when [`Word`] is itself 32 bits wide.
///
/// See the 64-bit variant above for the derivation. Written as a separate function rather than as
/// `Word::from(residue)` in both configurations because `u32::from(u32)` is a no-op conversion,
/// and a no-op conversion is a lint failure under this workspace's `-D warnings`.
#[cfg(not(target_pointer_width = "64"))]
#[inline]
fn widen(residue: u32) -> Word {
    residue
}

/// Keep the low 32 bits of a [`Word`], discarding anything above them.
///
/// This is the `return (z_crc_t)data;` at `crc32.c` L612 and the assignment of a `z_word_t` to
/// the `uLong crc` at L911: in both places the C narrows deliberately, having established that
/// the value has nothing above bit 31 left in it. Spelled through the little-endian byte
/// encoding rather than as `data as u32` so that the truncation is explicit about *which* bits it
/// keeps and needs no cast lint suppressed to say so.
#[inline]
fn low_u32(word: Word) -> u32 {
    // `to_le_bytes` yields least-significant byte first on every target, so the first four bytes
    // are the low 32 bits whatever the host's endianness. The trailing `..` matches the four
    // remaining bytes when `Word` is 64 bits and nothing at all when it is 32.
    let [b0, b1, b2, b3, ..] = word.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// Copy `bytes` into a `W`-byte array, or return `None` if it is not exactly `W` bytes long.
///
/// This is the whole of what replaces `*(z_word_t const *)buf` (`crc32.c` L712): the bytes are
/// **copied** into an array and the value is built from the copy, so the load has no alignment
/// precondition and needs no unsafe unaligned read. The callers all pass a `chunks_exact(W)`
/// item, so `None` is unreachable; they answer it with the byte-at-a-time engine rather than a
/// panic, which keeps the result correct as well as the code total.
#[inline]
fn word_bytes(bytes: &[u8]) -> Option<[u8; W]> {
    <[u8; W]>::try_from(bytes).ok()
}

/// Reverse the bytes of a [`Word`], converting between little- and big-endian representations.
///
/// Mirrors `byte_swap` at `crc32.c` L121-L139, whose comment at L115-L120 reads: "Swap the
/// bytes in a `z_word_t` to convert between little and big endian. Any self-respecting compiler
/// will optimize this to a single machine byte-swap instruction, if one is available."
///
/// `Word::swap_bytes` *is* that mask-and-shift chain, term for term -- `(word & 0xff) << 56`
/// through `(word & 0xff00000000000000) >> 56` for the 64-bit case at L122-L131, and the
/// four-term 32-bit case at L133-L137 -- and it compiles to the single instruction the comment
/// hopes for. The function is kept rather than inlined at its call sites so that the C name and
/// the provenance survive in the code, and so that the tests can address it directly; the tests
/// also check it against a literal transcription of the mask-and-shift chain.
#[inline]
fn byte_swap(word: Word) -> Word {
    word.swap_bytes()
}

/// CRC-32 of the `W` bytes held in `data`, least-significant byte first, unconditioned.
///
/// Mirrors `crc_word` at `crc32.c` L608-L613, whose comment at L603-L606 reads: "Return the
/// CRC of the W bytes in the `word_t` data, taking the least-significant byte of the word as
/// the first byte of data, without any pre or post conditioning. This is used to combine the
/// CRCs of each braid."
///
/// Each iteration is the byte step of `generic.rs` with the message bytes riding in the high end
/// of the same register: the low byte selects the table entry, the shift brings the next byte
/// down, and after `W` iterations nothing is left above bit 31 -- which is why the narrowing at
/// the end is exact. Equivalently, and as the tests assert, this is
/// `crc32_generic(0, &data.to_le_bytes())`.
#[inline]
fn crc_word(mut data: Word) -> u32 {
    for _ in 0..W {
        // `data & 0xff`, obtained without a narrowing cast.
        let [low, ..] = data.to_le_bytes();
        // `usize::from` of a `u8` is `0..=255` and the table has 256 entries, so the fallback is
        // unreachable; it is here to keep this loop free of a panicking path, and it costs
        // nothing -- the compiler emits no bounds check either way.
        let entry = CRC_TABLE.get(usize::from(low)).copied().unwrap_or(0);
        data = (data >> 8) ^ widen(entry);
    }
    low_u32(data)
}

/// The byte-reversed counterpart of [`crc_word`], for the big-endian block loop.
///
/// Mirrors `crc_word_big` at `crc32.c` L615-L621. The structure mirrors [`crc_word`] with
/// every direction reversed: the *most*-significant byte selects the table entry, the shift is to
/// the left, and the entries come from `crc_big_table`, whose values are the byte-reversed
/// residues of `CRC_TABLE` (`crc32.c` L254).
///
/// Because byte reversal distributes over exclusive-or and turns a right shift into a left one,
/// this is [`crc_word`] conjugated by [`byte_swap`]:
/// `crc_word_big(byte_swap(x)) == byte_swap(widen(crc_word(x)))` for every `x`, the widening being
/// the one C performs implicitly. The tests assert that identity, and it is what makes the
/// `byte_swap` at each end of the big-endian block loop (`crc32.c` L811 and L911) correct rather
/// than merely plausible. It also settles where the result's bits live: [`crc_word`]'s has nothing
/// above bit 31, so on a 64-bit word this one has nothing *below* bit 32, and on a 32-bit word --
/// where a reversal moves nothing out of the word -- it occupies the word entire.
#[inline]
fn crc_word_big(mut data: Word) -> Word {
    for _ in 0..W {
        // `(data >> ((W - 1) << 3)) & 0xff`: the most-significant byte, obtained without a cast.
        let [high, ..] = data.to_be_bytes();
        let entry = CRC_BIG_TABLE.get(usize::from(high)).copied().unwrap_or(0);
        data = (data << 8) ^ entry;
    }
    data
}

/// Fold whole braided blocks into `crc`, reading each word little-endian first.
///
/// Mirrors the little-endian branch of `crc32_z` at `crc32.c` L663-L789. `body` must be a
/// whole number of `BLOCK`-byte blocks; any other length is answered with the byte-at-a-time
/// engine, which is the same checksum by a slower route.
///
/// `crc` is an already pre-conditioned state and so is the result. On a little-endian target
/// `Word::from_le_bytes` is the native load the C performs, so this is that branch instruction
/// for instruction; on a big-endian target the module's dispatcher picks
/// [`fold_blocks_big_endian`] instead, and this function is not called. See the module
/// documentation for why each half is nevertheless correct on either host, which is what lets
/// both be tested everywhere.
fn fold_blocks_little_endian(crc: u32, body: &[u8]) -> u32 {
    // The final block is folded differently from the others, so hold it back. `checked_sub`
    // yields `None` only for a `body` shorter than one block, which the gate in [`crc32_braid`]
    // already excludes -- `crc32.c` L640 guarantees `blks >= 1` at L653 for the same reason.
    let Some(sparse_len) = body.len().checked_sub(BLOCK) else {
        return crc32_generic(crc, body);
    };

    // A partial trailing block would be dropped by the `chunks_exact` below rather than folded,
    // so a `body` that is not a whole number of blocks goes to the byte engine entire. The
    // caller never produces one; this keeps the function total and, more importantly, correct if
    // it ever did.
    if body.len() % BLOCK != 0 {
        return crc32_generic(crc, body);
    }

    let (Some(sparse), Some(last)) = (body.get(..sparse_len), body.get(sparse_len..)) else {
        return crc32_generic(crc, body);
    };

    // `crc32.c` L688-L704: braid 0 carries the incoming state, the rest start empty. The C
    // spells this out as `crc0 = crc; crc1 = 0; ...` behind one `#if` per braid; an array plus
    // the run-time `N` is the same thing without the preprocessor, and the compiler unrolls a
    // loop over a fixed `N` just as it unrolls the C's hand-written sequence.
    let mut braids = [0u32; N];
    if let Some(first) = braids.first_mut() {
        *first = crc;
    }

    // `crc32.c` L710-L766: the first `blks - 1` blocks, each braid advancing on its own word.
    // The C loads all `N` words before updating any braid, to give the pipeline the loads early;
    // braid `j` reads only word `j` and only its own remainder, so loading and updating one braid
    // at a time is the same computation.
    for block in sparse.chunks_exact(BLOCK) {
        for (braid, word) in braids.iter_mut().zip(block.chunks_exact(W)) {
            let Some(chunk) = word_bytes(word) else { break };

            // `word0 = crc0 ^ words[0]` at L712, with the widening C performs implicitly.
            let mixed = widen(*braid) ^ Word::from_le_bytes(chunk);

            // `crc0 = crc_braid_table[0][word0 & 0xff]` at L732, then the `k = 1..W` loop at
            // L748-L765. Row `k` is paired with byte `k` of the word, and `to_le_bytes()[k]` is
            // `(word >> (k << 3)) & 0xff` on every target, so zipping the rows against the bytes
            // reproduces the subscripts exactly -- and makes both of them impossible to get out
            // of range. Seeding the accumulator with zero folds row 0 in on the same footing as
            // the others, since exclusive-or with zero is the identity.
            let bytes = mixed.to_le_bytes();
            let mut next = 0u32;
            for (row, &byte) in CRC_BRAID_TABLE.iter().zip(bytes.iter()) {
                next ^= row.get(usize::from(byte)).copied().unwrap_or(0);
            }
            *braid = next;
        }
    }

    // `crc32.c` L768-L788: the last block, combining the `N` braids as it goes. The C writes
    // `crc = crc_word(crc0 ^ words[0])` for the first braid and
    // `crc = crc_word(crcJ ^ words[J] ^ crc)` for each one after it; starting the running value
    // at zero makes the first line an instance of the second, again because exclusive-or with
    // zero is the identity.
    let mut folded = 0u32;
    for (braid, word) in braids.iter().zip(last.chunks_exact(W)) {
        let Some(chunk) = word_bytes(word) else { break };
        folded = crc_word(widen(*braid) ^ Word::from_le_bytes(chunk) ^ widen(folded));
    }
    folded
}

/// Fold whole braided blocks into `crc`, reading each word big-endian first.
///
/// Mirrors the big-endian branch of `crc32_z` at `crc32.c` L790-L912. Structurally identical
/// to [`fold_blocks_little_endian`], with four differences, all of them the C's:
///
/// * the remainders are full `z_word_t`s rather than 32-bit residues (`crc32.c` L793-L803),
///   because they are held byte-reversed and a reversed 32-bit residue occupies the *high* half
///   of a word;
/// * braid 0 is seeded with `byte_swap(crc)` rather than `crc` (L811);
/// * the tables are `crc_braid_big_table` and, inside [`crc_word_big`], `crc_big_table`;
/// * the combined result is `byte_swap`-ed back on the way out (L911).
///
/// `crc` is an already pre-conditioned state and so is the result. On a big-endian target
/// `Word::from_be_bytes` is the native load the C performs, so this is that branch instruction
/// for instruction. It is also correct on a little-endian host, which is the property the tests
/// use to exercise this loop on `x86_64` rather than leaving it unreachable.
fn fold_blocks_big_endian(crc: u32, body: &[u8]) -> u32 {
    let Some(sparse_len) = body.len().checked_sub(BLOCK) else {
        return crc32_generic(crc, body);
    };

    if body.len() % BLOCK != 0 {
        return crc32_generic(crc, body);
    }

    let (Some(sparse), Some(last)) = (body.get(..sparse_len), body.get(sparse_len..)) else {
        return crc32_generic(crc, body);
    };

    // `crc32.c` L810-L826. The seed is byte-reversed because everything in this branch is:
    // `crc_big_table`'s entries are `byte_swap`-ed residues (`crc32.c` L254), so the remainders
    // must live in the same representation for the exclusive-ors to line up.
    let mut braids: [Word; N] = [0; N];
    if let Some(first) = braids.first_mut() {
        *first = byte_swap(widen(crc));
    }

    // `crc32.c` L832-L888, the sparse blocks. Note that the byte extraction is `to_le_bytes` here
    // too: L871 subscripts with `(word0 >> (k << 3)) & 0xff` in both branches, because it is the
    // *word value* being taken apart, not the memory image. The big tables' rows are already
    // stored in the reversed order that compensates (`crc32.c` L467-L470).
    for block in sparse.chunks_exact(BLOCK) {
        for (braid, word) in braids.iter_mut().zip(block.chunks_exact(W)) {
            let Some(chunk) = word_bytes(word) else { break };
            let mixed = *braid ^ Word::from_be_bytes(chunk);

            let bytes = mixed.to_le_bytes();
            let mut next: Word = 0;
            for (row, &byte) in CRC_BRAID_BIG_TABLE.iter().zip(bytes.iter()) {
                next ^= row.get(usize::from(byte)).copied().unwrap_or(0);
            }
            *braid = next;
        }
    }

    // `crc32.c` L890-L911: the last block combines the braids through `comb`, and the result is
    // reversed back into the ordinary representation. The narrowing is exact -- see
    // [`crc_word_big`]: on a 64-bit word nothing survives below bit 32 of `comb`, so nothing
    // survives above bit 31 once it is reversed, and on a 32-bit word the narrowing is the
    // identity. The C relies on the same fact when it assigns `byte_swap(comb)` to a `uLong` at
    // L911.
    let mut comb: Word = 0;
    for (braid, word) in braids.iter().zip(last.chunks_exact(W)) {
        let Some(chunk) = word_bytes(word) else { break };
        comb = crc_word_big(*braid ^ Word::from_be_bytes(chunk) ^ comb);
    }
    low_u32(byte_swap(comb))
}

/// Folds `buf` into the CRC state `crc`, a machine word at a time across `N` interleaved braids.
///
/// This is `crc32_z`'s braided section (`crc32.c` L637-L920) together with the handover to the
/// byte loop at L917, and it is a complete CRC-32 engine: handed any buffer it produces exactly
/// what `generic::crc32_generic` produces, only faster once the input is long enough to be worth
/// braiding.
///
/// Pass an already pre-conditioned state and treat the result as one: apply the leading `!crc`
/// before the first call and the trailing `crc ^ 0xffff_ffff` after the last, both of which
/// belong to this module's parent (`crc32.c` L635 and L940). Nothing here complements anything.
///
/// # Behaviour by length
///
/// * Shorter than `N * W + W - 1` bytes -- 47 on a 64-bit target, 23 on a 32-bit one -- the whole
///   buffer goes to `generic::crc32_generic`, as `crc32.c` L640 does.
/// * Otherwise the prologue takes bytes one at a time to a word boundary, whole `N * W`-byte
///   blocks are braided, and the remainder goes back to `generic::crc32_generic`.
///
/// Neither the threshold nor the prologue is observable in the result; see the module
/// documentation for why, and for why both are reproduced anyway.
///
/// # Examples
///
/// ```
/// use zlib_rs::crc32::{Braid, Crc32Backend};
///
/// // The published CRC-32 check value, through the parent module's conditioning. This input is
/// // below the threshold, so it exercises the delegation rather than the braid.
/// assert_eq!(Braid::update(!0u32, b"123456789") ^ 0xffff_ffff, 0xcbf4_3926);
///
/// // An empty slice is the identity on the state.
/// assert_eq!(Braid::update(0x1234_5678, &[]), 0x1234_5678);
/// ```
///
/// [`Braid`](super::Braid) forwards to this function unchanged and is the reachable name for
/// it outside the subsystem.
#[must_use]
pub fn crc32_braid(crc: u32, buf: &[u8]) -> u32 {
    // `crc32.c` L640: too short to braid, so run the byte loop over everything.
    if buf.len() < MIN_BRAID_LEN {
        return crc32_generic(crc, buf);
    }

    // `crc32.c` L646-L650: `while (len && ((z_size_t)buf & (W - 1)) != 0)`, which consumes
    // `(W - (address % W)) % W` bytes. `align_offset` computes precisely that value, and does it
    // without casting a pointer to an integer. Its contract permits a conforming implementation
    // to return a larger offset, or `usize::MAX` to decline: both are handled below and neither
    // can produce a wrong answer, because the split between the two paths is output-neutral.
    let prefix_len = buf.as_ptr().align_offset(W);
    let (Some(prefix), Some(aligned)) = (buf.get(..prefix_len), buf.get(prefix_len..)) else {
        return crc32_generic(crc, buf);
    };
    let crc = crc32_generic(crc, prefix);

    // `crc32.c` L652-L655: `blks = len / (N * W); len -= blks * N * W;`. The gate above plus a
    // prologue of at most `W - 1` bytes leaves at least `N * W` bytes here, so `body` is at least
    // one whole block.
    let body_len = (aligned.len() / BLOCK) * BLOCK;
    let (Some(body), Some(tail)) = (aligned.get(..body_len), aligned.get(body_len..)) else {
        return crc32_generic(crc, aligned);
    };

    // `crc32.c` L657-L662: `endian = 1; if (*(unsigned char *)&endian)`, a run-time test rather
    // than a compile-time one because -- as the comment there explains -- an ARM processor can
    // change endianness during execution, and where the compiler does know the answer it folds
    // the test away and drops the unused branch. Reading the first byte of the native encoding of
    // `1` is that test, expressed without a pointer cast; both branches are compiled on every
    // target.
    let [lowest_byte, ..] = 1u32.to_ne_bytes();
    let crc = if lowest_byte == 1 {
        fold_blocks_little_endian(crc, body)
    } else {
        fold_blocks_big_endian(crc, body)
    };

    // `crc32.c` L917 hands the unbraided remainder back to the byte loop at L922-L937.
    crc32_generic(crc, tail)
}

/// The braided backend: `crc32_z`'s word-at-a-time path over `N` interleaved remainders.
///
/// A zero-sized marker -- the work is `crc32_braid`, which callers inside the crate may also
/// invoke directly, and which the benchmarks and the differential suite drive as a plain function
/// over `(u32, &[u8])` with no hidden state. This type exists so that backend selection in the
/// parent module can name this path the same way it names the scalar and vectorised ones, and so
/// benchmarks can label it.
///
/// Like every function in this module, the backend takes and returns a pre-conditioned state and
/// complements nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Braid;

impl super::Crc32Backend for Braid {
    /// Identifies the braided word-at-a-time path in diagnostics and benchmark labels.
    const NAME: &'static str = "braid";

    /// Folds `buf` into the already pre-conditioned state `crc` by forwarding, unchanged, to
    /// `crc32_braid`.
    #[inline]
    fn update(crc: u32, buf: &[u8]) -> u32 {
        crc32_braid(crc, buf)
    }
}

// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use crate::crc32::combine::{multmodp, x2nmodp};
    use crate::crc32::Crc32Backend;

    use super::{
        byte_swap, crc32_braid, crc32_generic, crc_word, crc_word_big, fold_blocks_big_endian,
        fold_blocks_little_endian, low_u32, widen, word_bytes, Braid, Word, BLOCK,
        CRC_BRAID_BIG_TABLE, CRC_BRAID_TABLE, MIN_BRAID_LEN, N, W,
    };

    /// Length of the [`pseudo_random`] fixture.
    ///
    /// Twelve whole blocks plus 32 bytes on a 64-bit target and twenty-five whole blocks plus 12
    /// bytes on a 32-bit one, so truncations of it cover several sparse iterations, a partial
    /// trailing block, and every remainder size.
    const FIXTURE_LEN: usize = 512;

    /// Upper bound of the exhaustive length sweep: enough for four whole blocks plus a remainder
    /// on either word width, and small enough that the sweep stays usable under Miri.
    const SWEEP_LEN: usize = 200;

    /// Pre-conditioned states to seed the sweeps with, matching `generic.rs`: the two extremes
    /// (`0`, and the `!0` that `crc32.c` L635 produces from a zero seed) plus three interior
    /// values -- one with bits set only in the byte a table lookup consumes, one with bits set only
    /// in the byte that survives the shift, and one with bits scattered across all four.
    const SEEDS: [u32; 5] = [
        0x0000_0000,
        0xffff_ffff,
        0x0000_00ff,
        0xff00_0000,
        0x9e37_79b9,
    ];

    /// Oracle-verified CRC-32 values, each one the result of `crc32_z(0, buf, len)` from the
    /// in-tree C implementation.
    ///
    /// `"123456789"` is the published check value for this CRC. `"hello, hello!"` is the payload of
    /// `test/example.c` (L35). The first three are below [`MIN_BRAID_LEN`] on both word widths and
    /// so exercise the delegation to the byte engine; the last two are above it on both.
    const VECTORS: [(&[u8], u32); 5] = [
        (b"", 0x0000_0000),
        (b"hello, hello!", 0xb39a_dc9b),
        (b"123456789", 0xcbf4_3926),
        (b"The quick brown fox jumps over the lazy dog", 0x414f_a339),
        (b"123456789123456789123456789123456789", 0x3e29_169c),
    ];

    /// Pseudo-random fixture from a linear congruential generator.
    ///
    /// Byte for byte the fixture `generic.rs` uses, at greater length: same multiplier, same
    /// increment, same seed, and the same extraction of bits 16 to 23 of each state, because the
    /// low bits of such a generator are famously non-random and a fixture with a short period in
    /// them would exercise far fewer table rows than it appears to. Sharing the generator means a
    /// value asserted in either module is directly comparable with the other's.
    fn pseudo_random() -> [u8; FIXTURE_LEN] {
        let mut bytes = [0u8; FIXTURE_LEN];
        let mut state = 0x2b3c_4d5e_u32;
        for slot in &mut bytes {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let [_, _, high_middle, _] = state.to_le_bytes();
            *slot = high_middle;
        }
        bytes
    }

    /// Wrap `crc32_braid` in the conditioning `crc32_z` applies at `crc32.c` L635 and L940, so
    /// that a result can be compared against a published CRC-32 value.
    fn conditioned(bytes: &[u8]) -> u32 {
        crc32_braid(!0, bytes) ^ 0xffff_ffff
    }

    /// Literal transcription of `byte_swap` at `crc32.c` L122-L131, for the 64-bit word.
    #[cfg(target_pointer_width = "64")]
    fn byte_swap_by_hand(word: Word) -> Word {
        (word & 0xff00_0000_0000_0000) >> 56
            | (word & 0x00ff_0000_0000_0000) >> 40
            | (word & 0x0000_ff00_0000_0000) >> 24
            | (word & 0x0000_00ff_0000_0000) >> 8
            | (word & 0x0000_0000_ff00_0000) << 8
            | (word & 0x0000_0000_00ff_0000) << 24
            | (word & 0x0000_0000_0000_ff00) << 40
            | (word & 0x0000_0000_0000_00ff) << 56
    }

    /// Literal transcription of `byte_swap` at `crc32.c` L133-L137, for the 32-bit word.
    #[cfg(not(target_pointer_width = "64"))]
    fn byte_swap_by_hand(word: Word) -> Word {
        (word & 0xff00_0000) >> 24
            | (word & 0x00ff_0000) >> 8
            | (word & 0x0000_ff00) << 8
            | (word & 0x0000_00ff) << 24
    }

    /// Deterministic word fixtures for the word-level helpers: the two extremes, single bits at
    /// each end of every byte lane, and values with all lanes populated.
    fn word_fixtures() -> [Word; 10] {
        let ramp = pseudo_random();
        let mut from_fixture: [Word; 2] = [0; 2];
        for (slot, chunk) in from_fixture.iter_mut().zip(ramp.chunks_exact(W)) {
            *slot = word_bytes(chunk).map_or(0, Word::from_le_bytes);
        }

        [
            0,
            Word::MAX,
            1,
            1 << (8 * W - 1),
            0xff,
            widen(0xffff_ffff),
            widen(0x8000_0001),
            widen(0xedb8_8320),
            from_fixture[0],
            from_fixture[1],
        ]
    }

    /// Regenerate both braid tables exactly as `braid()` does at `crc32.c` L461-L473.
    ///
    /// Test-only, and deliberately so: `braid()` is compiled only under `DYNAMIC_CRC_TABLE`, and
    /// this implementation has no dynamic table at all. Nothing in the shipped code performs modular
    /// arithmetic or fills a table at run time, and nothing here introduces a `OnceLock`, a
    /// `static mut` or any other lazy initialization -- this function exists so the transcribed
    /// tables the block loops read can be re-derived from `crc32.c`'s own generator.
    fn regenerate_braid_tables() -> ([[u32; 256]; W], [[Word; 256]; W]) {
        let mut little = [[0u32; 256]; W];
        let mut big: [[Word; 256]; W] = [[0; 256]; W];

        for k in 0..W {
            // `p = x2nmodp((n * w + 3 - k) << 3, 0)`, `crc32.c` L465.
            let exponent = u64::try_from((N * W + 3 - k) << 3).unwrap();
            let p = x2nmodp(exponent, 0);

            for i in 0..256usize {
                let index = u32::try_from(i).unwrap();
                // `ltl[k][0] = 0` at L466 is a special case because `multmodp` requires a
                // non-zero first argument (`crc32.c` L160-L162); every other entry is
                // `multmodp(i << 24, p)` at L469.
                let q = if index == 0 {
                    0
                } else {
                    multmodp(index << 24, p)
                };
                little[k][i] = q;
                // `big[w - 1 - k][i] = byte_swap(q)` at L467 and L470: the same residue, byte
                // reversed, in the row mirrored about the middle of the table.
                big[W - 1 - k][i] = byte_swap(widen(q));
            }
        }

        (little, big)
    }

    #[test]
    fn constants_mirror_the_c_configuration() {
        assert_eq!(BLOCK, N * W, "crc32.c L653: blks = len / (N * W)");
        assert_eq!(
            MIN_BRAID_LEN,
            N * W + W - 1,
            "crc32.c L640: if (len >= N * W + W - 1)"
        );
        assert_eq!(
            W,
            size_of::<Word>(),
            "crc32.c L96-L106 pairs W with z_word_t"
        );

        // The concrete values, so that a change to `N` or `W` in the tables module cannot pass
        // unnoticed: N = 5 with W = 8 on a 64-bit target and W = 4 elsewhere.
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(BLOCK, 40);
            assert_eq!(MIN_BRAID_LEN, 47);
        }
        #[cfg(not(target_pointer_width = "64"))]
        {
            assert_eq!(BLOCK, 20);
            assert_eq!(MIN_BRAID_LEN, 23);
        }
    }

    #[test]
    fn byte_swap_matches_the_mask_and_shift_chain() {
        for word in word_fixtures() {
            assert_eq!(byte_swap(word), byte_swap_by_hand(word));
            // Byte reversal is an involution, which is what lets the big-endian block loop seed
            // with `byte_swap(crc)` and finish with `byte_swap(comb)`.
            assert_eq!(byte_swap(byte_swap(word)), word);
        }
    }

    #[test]
    fn low_u32_keeps_the_low_half_of_the_word() {
        assert_eq!(low_u32(0), 0);
        assert_eq!(low_u32(widen(0xffff_ffff)), 0xffff_ffff);
        assert_eq!(low_u32(widen(0x1234_5678)), 0x1234_5678);
        // On a 64-bit word the high half must be discarded, which is the narrowing `crc32.c` L612
        // performs with `(z_crc_t)data`.
        assert_eq!(low_u32(Word::MAX), 0xffff_ffff);
        for residue in SEEDS {
            assert_eq!(low_u32(widen(residue)), residue);
        }
    }

    #[test]
    fn word_bytes_accepts_exactly_one_word() {
        let fixture = pseudo_random();
        assert!(word_bytes(&fixture[..W]).is_some());
        assert!(word_bytes(&fixture[..W - 1]).is_none());
        assert!(word_bytes(&fixture[..=W]).is_none());
        assert!(word_bytes(&[]).is_none());
    }

    #[test]
    fn crc_word_is_the_byte_at_a_time_crc_of_the_words_bytes() {
        // `crc32.c` L603-L606: the CRC of the W bytes in the word, least-significant byte first,
        // with no conditioning -- which is exactly the byte engine seeded with zero.
        for word in word_fixtures() {
            assert_eq!(crc_word(word), crc32_generic(0, &word.to_le_bytes()));
        }
    }

    #[test]
    fn crc_word_big_is_crc_word_conjugated_by_byte_swap() {
        // Byte reversal distributes over exclusive-or and turns the right shift of `crc_word`
        // into the left shift of `crc_word_big`, so the two are related by conjugation. This is
        // the identity the big-endian branch's `byte_swap` at each end depends on.
        for word in word_fixtures() {
            assert_eq!(
                crc_word_big(byte_swap(word)),
                byte_swap(widen(crc_word(word)))
            );

            // The narrowing that `fold_blocks_big_endian` performs after reversing `comb` --
            // `crc32.c` L911 assigning `byte_swap(comb)` to a `uLong` -- loses nothing. True at
            // both word widths, and the reason the big-endian loop may return a `u32`.
            let reversed = byte_swap(crc_word_big(word));
            assert_eq!(widen(low_u32(reversed)), reversed);

            // Why it loses nothing differs with the width, so the sharper statement is `cfg`-ed:
            // on a 64-bit word the residue sits wholly above bit 31 before the reversal, whereas
            // on a 32-bit word a reversal moves nothing out of the word at all.
            #[cfg(target_pointer_width = "64")]
            assert_eq!(low_u32(crc_word_big(word)), 0);
        }
    }

    #[test]
    fn check_value_matches_the_crc_32_standard() {
        assert_eq!(conditioned(b"123456789"), 0xcbf4_3926);

        // The same input repeated until it is long enough to take the braided path, so the check
        // value is corroborated on both sides of the threshold.
        let repeated = *b"123456789123456789123456789123456789123456789123456789123456789123456789";
        assert!(repeated.len() >= MIN_BRAID_LEN);
        assert_eq!(conditioned(&repeated), 0x8811_a440);
    }

    #[test]
    fn reference_vectors_match_the_c_implementation() {
        for (bytes, expected) in VECTORS {
            assert_eq!(conditioned(bytes), expected);
            assert_eq!(crc32_braid(!0, bytes), crc32_generic(!0, bytes));
        }
    }

    #[test]
    fn array_fixtures_match_the_c_implementation() {
        assert_eq!(conditioned(&[0x00; 64]), 0x758d_6336);
        assert_eq!(conditioned(&[0xff; 64]), 0x0f61_87ba);
        // 0xa5 is the byte `test/infcover.c` fills every allocation with, over an input long
        // enough for twenty-five whole blocks on either word width.
        assert_eq!(conditioned(&[0xa5; 1024]), 0xfe55_da7c);

        let fixture = pseudo_random();
        assert_eq!(conditioned(&fixture), 0x0ff0_be3d);
        assert_eq!(conditioned(&fixture[..80]), 0x37b7_b48d);
    }

    #[test]
    fn every_length_agrees_with_the_byte_at_a_time_path() {
        let fixture = pseudo_random();

        // Every length from empty to several whole blocks plus a remainder, for every seed. This
        // is the load-bearing test of the module: it covers both sides of the threshold, the
        // prologue, one and many sparse iterations, the final block, and every remainder size.
        for state in SEEDS {
            for len in 0..=SWEEP_LEN {
                let head = &fixture[..len];
                assert_eq!(
                    crc32_braid(state, head),
                    crc32_generic(state, head),
                    "length {len} with state {state:#010x}"
                );
            }
        }
    }

    #[test]
    fn the_threshold_and_block_boundaries_agree() {
        let fixture = pseudo_random();

        // The lengths an off-by-one would hide in: either side of the threshold, either side of a
        // block, and exact multiples of a block.
        let boundaries = [
            0,
            1,
            W - 1,
            W,
            W + 1,
            BLOCK - 1,
            BLOCK,
            BLOCK + 1,
            MIN_BRAID_LEN - 2,
            MIN_BRAID_LEN - 1,
            MIN_BRAID_LEN,
            MIN_BRAID_LEN + 1,
            2 * BLOCK - 1,
            2 * BLOCK,
            2 * BLOCK + 1,
            2 * BLOCK + W - 1,
            3 * BLOCK,
            4 * BLOCK + W - 2,
        ];

        for state in SEEDS {
            for len in boundaries {
                let head = &fixture[..len];
                assert_eq!(
                    crc32_braid(state, head),
                    crc32_generic(state, head),
                    "boundary length {len}"
                );
            }
        }
    }

    #[test]
    fn every_start_offset_agrees() {
        let fixture = pseudo_random();

        // Sweeping the start offset over two whole words guarantees that the slice begins at every
        // possible misalignment relative to a `W`-byte boundary, whatever the fixture's own
        // address happens to be. This is what establishes that the prologue is right -- and, since
        // every one of these agrees with the byte engine, that it is output-neutral.
        for offset in 0..2 * W {
            for len in [
                MIN_BRAID_LEN - 1,
                MIN_BRAID_LEN,
                MIN_BRAID_LEN + 1,
                BLOCK,
                2 * BLOCK,
                2 * BLOCK + 3,
                SWEEP_LEN,
            ] {
                let slice = &fixture[offset..offset + len];
                assert_eq!(
                    crc32_braid(!0, slice),
                    crc32_generic(!0, slice),
                    "offset {offset}, length {len}"
                );
            }
        }
    }

    #[test]
    fn splitting_the_input_anywhere_yields_the_same_state() {
        let fixture = pseudo_random();
        let (head, _) = fixture.split_at(SWEEP_LEN);
        let whole = crc32_braid(!0, head);

        // Exhaustive over every split point, so every combination of a block-aligned or unaligned
        // head with a block-aligned or unaligned tail is covered, including splits that leave one
        // side below the threshold. This is the property the gzip layer depends on while it
        // accumulates a checksum across successive `deflate` calls.
        for split in 0..=SWEEP_LEN {
            let (first, second) = head.split_at(split);
            assert_eq!(
                crc32_braid(crc32_braid(!0, first), second),
                whole,
                "split at {split}"
            );
        }

        // And once across the whole fixture, at offsets chosen to straddle a block boundary.
        for split in [1, W, BLOCK, BLOCK + 1, 7 * BLOCK, FIXTURE_LEN - 1] {
            let (first, second) = fixture.split_at(split);
            assert_eq!(
                crc32_braid(crc32_braid(!0, first), second),
                crc32_braid(!0, &fixture)
            );
        }
    }

    #[test]
    fn the_two_endian_block_loops_agree_with_each_other_and_with_the_byte_path() {
        let fixture = pseudo_random();

        // Executing the big-endian loop on a little-endian host is the point of this test: CI runs
        // on x86_64, where the dispatcher in `crc32_braid` never selects it, so without calling it
        // directly the whole of `crc32.c` L790-L912 -- the big braid tables, `crc_word_big`, and
        // the `byte_swap` at each end -- would go untested until someone built for s390x.
        //
        // One block exercises the final combination with no sparse iterations at all (`blks == 1`,
        // where the C's `while (--blks)` body never runs); four exercise three of them.
        for blocks in 1..=4 {
            let body = &fixture[..blocks * BLOCK];
            for state in SEEDS {
                let reference = crc32_generic(state, body);
                assert_eq!(
                    fold_blocks_little_endian(state, body),
                    reference,
                    "{blocks} block(s), little-endian loop"
                );
                assert_eq!(
                    fold_blocks_big_endian(state, body),
                    reference,
                    "{blocks} block(s), big-endian loop"
                );
            }
        }
    }

    #[test]
    fn the_block_loops_answer_a_body_that_is_not_whole_blocks_correctly() {
        let fixture = pseudo_random();

        // `crc32_braid` never hands either loop a partial block, but neither loop is allowed to be
        // wrong if it were handed one: `chunks_exact` would silently drop the partial tail, so both
        // fall back to the byte engine over the whole body instead.
        for len in [0, 1, W, BLOCK - 1, BLOCK + 1, 2 * BLOCK - 1, 2 * BLOCK + W] {
            let body = &fixture[..len];
            for state in SEEDS {
                let reference = crc32_generic(state, body);
                assert_eq!(
                    fold_blocks_little_endian(state, body),
                    reference,
                    "len {len}"
                );
                assert_eq!(fold_blocks_big_endian(state, body), reference, "len {len}");
            }
        }
    }

    #[test]
    fn the_returned_state_is_still_pre_conditioned() {
        let repeated = *b"123456789123456789123456789123456789123456789123456789123456789123456789";
        let state = crc32_braid(!0, &repeated);

        // What comes back is one complement away from the value a caller of `crc32_z` sees, and
        // applying that complement is the parent module's job (`crc32.c` L940). If this module ever
        // conditioned its own result, the braided path and the byte path could not be composed --
        // and `crc32_braid` composes them itself, on every call above the threshold.
        assert_eq!(state, 0x8811_a440 ^ 0xffff_ffff);
        assert_ne!(state, 0x8811_a440);
    }

    #[test]
    fn an_empty_slice_leaves_the_state_untouched() {
        for state in SEEDS {
            assert_eq!(crc32_braid(state, &[]), state);
        }
    }

    #[test]
    fn feeding_one_byte_at_a_time_matches_one_call() {
        let fixture = pseudo_random();
        let (head, _) = fixture.split_at(SWEEP_LEN);

        // Every call here is below the threshold, so this pins the delegation path in particular.
        let byte_at_a_time = head
            .iter()
            .fold(!0, |state, &byte| crc32_braid(state, &[byte]));

        assert_eq!(byte_at_a_time, crc32_braid(!0, head));
    }

    #[test]
    fn braid_tables_match_the_c_generator() {
        let (little, big) = regenerate_braid_tables();

        // Row-by-row rather than whole-table, so a failure names the row instead of printing
        // thousands of values.
        for (k, (regenerated, transcribed)) in little.iter().zip(CRC_BRAID_TABLE.iter()).enumerate()
        {
            assert_eq!(regenerated, transcribed, "crc_braid_table row {k}");
        }
        for (k, (regenerated, transcribed)) in
            big.iter().zip(CRC_BRAID_BIG_TABLE.iter()).enumerate()
        {
            assert_eq!(regenerated, transcribed, "crc_braid_big_table row {k}");
        }
    }

    #[test]
    fn the_backend_forwards_to_the_free_function() {
        assert_eq!(Braid::NAME, "braid");

        let fixture = pseudo_random();
        for (bytes, _) in VECTORS {
            assert_eq!(Braid::update(!0, bytes), crc32_braid(!0, bytes));
        }
        for state in SEEDS {
            assert_eq!(Braid::update(state, &fixture), crc32_braid(state, &fixture));
        }
    }
}
