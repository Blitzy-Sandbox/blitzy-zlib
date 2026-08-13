//! The Adler-32 engine: shared constants, the backend contract, the two entry points and
//! the dispatch between backends.
//!
//! This module is the public face of the Adler-32 implementation and, together with the
//! three sibling modules it declares, the mirror of `adler32.c` -- 164 lines that remain in
//! the tree unmodified, serving as the differential oracle every value produced here is
//! measured against. Not one sum is computed in this file: it owns the two constants the
//! algorithm is defined by, the named initial value RFC 1950 mandates, the contract a
//! backend satisfies, and the choice of which backend runs. The arithmetic lives next
//! door.
//!
//! # The checksum
//!
//! RFC 1950 defines it in four sentences (`doc/rfc1950.txt` L325-L329). `s1` is the sum of
//! all bytes and `s2` is the sum of all `s1` values; both sums are taken modulo 65521, the
//! largest prime below 65536, which the reference sources spell [`BASE`]
//! (`adler32.c` L10); `s1` is initialized to 1 and `s2` to zero; and the checksum is stored
//! as `s2 * 65536 + s1` in most-significant-byte-first (network) order. A running checksum
//! is therefore a single 32-bit word holding two 16-bit residues, which is why every
//! signature below takes and returns one `u32` and no pair of sums is ever exposed.
//!
//! The two sums make the checksum sensitive to byte *order* as well as to byte *values*,
//! which a plain sum is not, and RFC 1950 records the lineage: Adler-32 is "a 32-bit
//! extension and improvement of the Fletcher algorithm" used in ITU-T X.224 / ISO 8073
//! (`doc/rfc1950.txt` L321-L323). `zlib.h` L1816-L1817 states the trade it makes -- almost
//! as reliable as a CRC-32, but computable much faster.
//!
//! # Where this checksum is used
//!
//! It is the check value of the zlib container rather than an internal convenience, so its
//! correctness is a wire-format property and not merely an implementation detail:
//!
//! * `deflate` accumulates it over the **uncompressed** input as that input is copied into
//!   the window -- `read_buf` at `deflate.c` L229, reached only when `wrap == 1`, whose
//!   counterpart in this crate is `read_buf.rs` -- and writes the finished value into the
//!   four-byte stream trailer.
//! * `inflate` recomputes it over the bytes it produces and compares the result against
//!   the trailer it read. The reference sources reach the checksum through the
//!   `UPDATE_CHECK` macro at `inflate.c` L300-L305, which selects Adler-32 for a zlib
//!   stream and CRC-32 for a gzip one.
//! * A preset dictionary is *identified* by it. `deflateSetDictionary` leaves the
//!   dictionary's Adler-32 in `strm->adler` (`deflate.c` L576) so that a decompressor can
//!   tell its caller which dictionary it needs, and `inflateSetDictionary` recomputes that
//!   identifier and rejects a mismatch with `Z_DATA_ERROR` (`inflate.c` L1201-L1204).
//!   `test/example.c` exercises exactly that handshake: it saves `c_stream.adler` as
//!   `dictId` after setting the dictionary and compares it against `d_stream.adler` when
//!   `inflate` answers `Z_NEED_DICT`.
//! * Dictionary bytes are excluded from the stream check value
//!   (`doc/rfc1950.txt` L319-L320), which is why `deflateSetDictionary` clears `wrap` while
//!   it loads the dictionary into the window (`deflate.c` L577) and restores it afterwards
//!   (`deflate.c` L620).
//!
//! # Module map
//!
//! | Module | Mirrors | Role |
//! |---|---|---|
//! | `generic` | `adler32.c` L61-L125 | The scalar reference: [`Adler32Generic`] |
//! | `combine` | `adler32.c` L133-L164 | Concatenation: [`adler32_combine`] |
//! | `simd` | -- | Optional, output-neutral vectorization; `simd` feature only |
//!
//! `simd` is compiled only when the crate's `simd` feature is enabled and has no
//! counterpart in the C sources; it re-arranges the same integer recurrence and is required
//! to agree with `generic` on every input.
//!
//! # Two entry points, one algorithm
//!
//! [`adler32()`] and [`adler32_z`] both exist, and [`adler32()`] delegates. That mirrors the
//! reference sources exactly, where `adler32` (`adler32.c` L128-L130) is a one-line
//! forwarder to `adler32_z` (`adler32.c` L61-L125) and the two differ only in the declared
//! width of the length argument: `uInt` for one, `z_size_t` for the other
//! (`zlib.h` L1809 and L1829). A Rust slice carries its own length, so that distinction
//! disappears here and the two functions are genuinely interchangeable.
//!
//! Keeping both names is nonetheless deliberate. `libz-rs-sys` has to export both C
//! symbols, and giving each one a same-named Rust function to call preserves the
//! one-to-one correspondence with the C sources that the rest of this implementation is organised
//! around. Note that the widths themselves are *not* modelled here: `uLong` is 64 bits on
//! LP64 targets and 32 bits on LLP64 Windows, so substituting a fixed-width integer for it
//! would silently corrupt values on one platform or the other. Reconciling the caller's
//! types with these `u32` and `usize` signatures is the facade's job, and its alone.
//!
//! # Backend selection is a throughput decision only
//!
//! [`adler32_z`] picks a backend twice over: at build time through the `simd` feature, and
//! at run time through target-feature detection. Both paths are required to return the
//! *identical* value for every input and every starting value, so the choice can never be
//! observed in the output -- only in the time taken.
//!
//! The run-time stage is specific to *this* checksum. [`mod@crate::crc32`] has no probe at all,
//! because its wide backend is portable integer arithmetic with no architecture intrinsic
//! to guard, whereas the arrangement here is worth taking only where a vector unit exists.
//! Neither module is gated on an intrinsic, so a `simd` build is correct on every target.
//! And the build-time stage has exactly one input, the `simd` feature: `ZLIB_RS_SIMD` in the
//! facade's build script cannot enable a Cargo feature, so it reconciles a caller's request
//! against the resolved feature set and fails the build on a contradiction.
//!
//! That this is permissible is specific to the checksum computation. For a fixed byte
//! sequence, a vector backend can evaluate algebraically equivalent groups of the same
//! recurrence while preserving every byte's positional weight, and modular integer
//! arithmetic then produces the same scalar. It does **not** reorder the input bytes:
//! Adler-32 remains order-sensitive. The same reasoning emphatically does not extend to the
//! compressor, where the order in which match candidates are examined decides which match
//! is emitted, so vectorised match finding is prohibited in this implementation while vectorised
//! checksums are welcome. The two cases look alike and must not be conflated.
//!
//! Output neutrality is verified rather than assumed: the equivalence sweep in `simd`
//! compares the two backends directly across every path boundary, the tests at the foot of
//! this file re-check the *dispatcher* against the scalar backend so that the wiring itself
//! is covered, and the differential suite in `zlib-rs-differential` then measures both
//! against the C implementation.
//!
//! # `Z_NULL`, and why an empty slice is not it
//!
//! The C API overloads its buffer argument. `zlib.h` L1813-L1814 promises that "if buf is
//! `Z_NULL`, this function returns the required initial value for the checksum", and
//! `zlib.h` L1821 shows the idiom callers are expected to write:
//! `uLong adler = adler32(0L, Z_NULL, 0);`. The reference implementation itself uses that
//! idiom in five places purely to obtain the value 1 -- `deflate.c` L671 and L1055, and
//! `inflate.c` L550, L705 and L1201.
//!
//! A `&[u8]` cannot be null, so this module offers [`ADLER32_INITIAL_VALUE`] to those
//! internal callers instead, and the null-pointer contract is honoured one layer up, at the
//! FFI boundary in `libz-rs-sys`, which answers a null pointer with the initial value
//! without ever calling in here.
//!
//! **An empty slice is not `Z_NULL`.** The distinction is the single most likely defect at
//! this boundary, so it is spelled out, and every value below was taken from the C
//! implementation:
//!
//! | Call | Result |
//! |---|---|
//! | `adler32(0, Z_NULL, 0)` in C | `0x0000_0001` -- the initial value |
//! | `adler32(ADLER32_INITIAL_VALUE, &[])` | `0x0000_0001` |
//! | `adler32(0, &[])` | `0x0000_0000` -- **not** 1 |
//! | `adler32(0xffff_ffff, &[])` | `0x000e_000e` |
//!
//! An empty slice is an ordinary zero-length update: it returns the incoming checksum with
//! its halves normalised, and it has no way to mean "give me the initial value". A caller
//! that needs the initial value must name [`ADLER32_INITIAL_VALUE`]; a caller that reaches
//! for `adler32(0, &[])` instead gets 0 and every stream it goes on to build is wrong.
//!
//! One hazard in the C source is worth recording for whoever compares the two
//! implementations. `adler32_z` tests `len == 1` at `adler32.c` L70, *before* it tests
//! `buf == Z_NULL` at `adler32.c` L81 -- the comment at L80 calls the ordering a "deferred
//! check for len == 1 speed". A call of `adler32(a, NULL, 1)` therefore dereferences a null
//! pointer rather than returning 1. Rust cannot express that call at all, so the implementation
//! removes the hazard without changing a single defined behaviour.
//!
//! # Layering and safety posture
//!
//! This module is a leaf of the crate's dependency graph. It names `core` only, reaches
//! into no other subsystem -- not `crc32`, `deflate`, `inflate` or `gz` -- and the
//! dependency runs the other way, from `read_buf.rs` to here. That keeps the crate free of
//! internal cycles and lets Miri exercise the checksum path in isolation.
//!
//! Nothing here declares C-visible linkage or a C-visible layout. The four exported C
//! symbols of this family -- `adler32`, `adler32_z`, `adler32_combine` and
//! `adler32_combine64` -- are defined in `libz-rs-sys`, which calls into this module; the
//! separation is absolute, and it is what lets `zlib.map`'s `local:` block keep every
//! internal name hidden. It follows that no item below may be given a stable exported
//! symbol name, however convenient that might seem.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::adler32::{adler32, adler32_combine, ADLER32_INITIAL_VALUE};
//!
//! // Start from the value RFC 1950 mandates -- never from `adler32(0, &[])`.
//! let mut adler = ADLER32_INITIAL_VALUE;
//!
//! // Accumulate incrementally, exactly as `deflate` does through `read_buf`.
//! adler = adler32(adler, b"hello, ");
//! adler = adler32(adler, b"hello!");
//! assert_eq!(adler, 0x2170_0496);
//!
//! // Or all at once: chunking cannot change the answer.
//! assert_eq!(adler32(ADLER32_INITIAL_VALUE, b"hello, hello!"), 0x2170_0496);
//!
//! // Two checksums can be concatenated without re-reading either sequence.
//! let head = adler32(ADLER32_INITIAL_VALUE, b"hello, ");
//! let tail = adler32(ADLER32_INITIAL_VALUE, b"hello!");
//! assert_eq!(adler32_combine(head, tail, 6), 0x2170_0496);
//! ```
//!
//! # Provenance
//!
//! Adler-32: the checksum RFC 1950 puts in the zlib trailer.
//!
//! Ported from `adler32.c`, including the `NMAX` chunking schedule and `adler32_combine_`.
//!
//! [`ADLER32_INITIAL_VALUE`]: crate::adler32::ADLER32_INITIAL_VALUE
//! [`Adler32Generic`]: crate::adler32::Adler32Generic
//! [`BASE`]: crate::adler32::BASE

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

mod combine;
mod generic;
#[cfg(feature = "simd")]
mod simd;

/// Concatenation of two Adler-32 checksums -- see [`combine::adler32_combine`].
///
/// Re-exported here so that the whole Adler-32 surface is reachable from one path, and
/// because `libz-rs-sys` backs both `adler32_combine` and `adler32_combine64` with it.
pub use self::combine::{adler32_combine, adler32_combine_wide};

/// The scalar reference backend -- see [`generic::Adler32Generic`].
///
/// Always available, on every target and in every feature configuration.
pub use self::generic::Adler32Generic;

/// The optional vectorization-friendly backend -- see [`simd::Adler32Simd`].
///
/// Present only when the crate's `simd` feature is enabled, and interchangeable with
/// [`Adler32Generic`] wherever it is.
#[cfg(feature = "simd")]
pub use self::simd::Adler32Simd;

/// The modulus both component sums are taken over: 65521, the largest prime smaller than
/// 65536.
///
/// Mirrors `BASE` (`adler32.c` L10), and the value RFC 1950 fixes for the algorithm
/// (`doc/rfc1950.txt` L326-L327). Being the largest prime below 65536 is what lets each
/// residue occupy 16 bits, so that the two of them pack losslessly into one `u32`, while
/// primality is what spreads the sums well.
///
/// The type is `u32`, the width of the checksum, because this is a modulus applied to the
/// sums and never a length. It is deliberately not modelled on the C `uLong` that carries
/// it at the API edge; see the module documentation.
pub const BASE: u32 = 65_521;

/// The largest number of bytes that may be accumulated before the sums must be reduced.
///
/// Mirrors `NMAX` (`adler32.c` L11), whose defining property the comment at
/// `adler32.c` L12 states: `NMAX` is the largest `n` such that
/// `255n(n+1)/2 + (n+1)(BASE-1) <= 2^32-1`. In other words it is the point at which
/// 32-bit accumulation stops being provably safe -- feeding a longer run of bytes into
/// `u32` sums before reducing them can overflow, and `5_552` is exactly the largest length
/// for which it cannot. The bound is checked numerically, for `NMAX` and for `NMAX + 1`, by
/// the tests at the foot of this file.
///
/// Reducing *less* often than this is a correctness bug; reducing more often is merely
/// slower, and would change nothing observable. Both backends therefore cut their input
/// into blocks of at most this many bytes and reduce once per block, mirroring the loop at
/// `adler32.c` L97-L106.
///
/// The type is `usize` because this is a buffer length, and it is divisible by 16, which
/// the C block loop relies on when it computes its iteration count as `NMAX / 16`
/// (`adler32.c` L99).
pub const NMAX: usize = 5_552;

/// The required initial value of an Adler-32 checksum: `1`.
///
/// This is what the C API returns for `adler32(0L, Z_NULL, 0)` (`zlib.h` L1813-L1814,
/// implemented at `adler32.c` L81-L82), and it is RFC 1950's initialization of `s1` to 1
/// and `s2` to zero (`doc/rfc1950.txt` L327) packed into one word.
///
/// It exists as a named constant because a Rust slice cannot be null and so the C idiom is
/// unavailable in this crate. Every internal caller that needs a fresh checksum -- the
/// counterparts of `deflate.c` L671 and L1055 and of `inflate.c` L550, L705 and L1201 --
/// names this constant. The alternative spellings are both traps: a bare literal `1`
/// carries no explanation, and `adler32(0, &[])` returns `0`, because an empty slice is an
/// ordinary zero-length update and not `Z_NULL`.
pub const ADLER32_INITIAL_VALUE: u32 = 1;

/// A full block must be an exact number of 16-byte unrolled steps.
///
/// The block loop at `adler32.c` L97-L106 computes its iteration count once, as
/// `n = NMAX / 16`, and then runs a `do { DO16(buf); } while (--n)` loop that has no
/// provision for a leftover tail. The comment at `adler32.c` L99 states the assumption
/// outright -- "NMAX is divisible by 16" -- and both backends inherit it when they split
/// their input into `NMAX`-sized blocks. Pinning it here turns a future edit of [`NMAX`]
/// into a build failure rather than into a silently dropped tail.
const _: () = assert!(
    NMAX % 16 == 0,
    "adler32.c L99 requires that a full NMAX block be a whole number of 16-byte steps"
);

/// Each component sum must fit in 16 bits for the packed representation to be lossless.
///
/// A checksum is `s2 * 65536 + s1` (`doc/rfc1950.txt` L328-L329), which is exact only while
/// both residues stay below 65536. [`BASE`] being smaller than 65536 is what guarantees it,
/// and it is the reason the two halves can be recovered by a mask and a shift at all.
const _: () = assert!(
    BASE < 0x1_0000,
    "both Adler-32 component sums must fit in 16 bits to pack into one u32"
);

/// The initial value must decode to RFC 1950's `s1 = 1`, `s2 = 0`.
///
/// Stated as a property of the two halves rather than as the literal `1` so that the
/// constant is tied to the specification it comes from (`doc/rfc1950.txt` L327) instead of
/// to a magic number.
const _: () = assert!(
    ADLER32_INITIAL_VALUE & 0xffff == 1 && ADLER32_INITIAL_VALUE >> 16 == 0,
    "RFC 1950 initializes s1 to 1 and s2 to zero"
);

/// A swappable Adler-32 computation backend.
///
/// The trait exists so that the scalar engine and the optional vectorised one are
/// interchangeable *by construction* rather than by convention: [`adler32_z`] selects
/// between them through this one method, the benchmarks compare them through it, and the
/// equivalence tests pin a computation to one specific implementation through it.
///
/// # Implementors are required to be indistinguishable
///
/// Every implementation must return the same value as [`Adler32Generic`] for every
/// `adler`/`buf` pair, including starting values whose halves are not below [`BASE`], for
/// which the reference implementation's three code paths deliberately reduce differently.
/// That is a hard contract, not an aspiration: an Adler-32 value is part of the zlib wire
/// format, so a backend that differed in one bit would produce streams the reference
/// implementation rejects. A backend may differ only in how long it takes.
///
/// # Shape of the method
///
/// `checksum` is an associated function and takes no `self`, so a backend is named rather
/// than constructed and the implementing types can be zero-sized:
/// `Adler32Generic::checksum(adler, buf)`. Consequently the trait is deliberately not
/// `dyn`-compatible -- nothing in this library needs dynamic dispatch, and routing a
/// checksum through a vtable would defeat the inlining the throughput targets depend on.
pub trait Adler32Backend {
    /// Update the running checksum `adler` with `buf` and return the new packed value.
    ///
    /// `adler` and the return value both hold `s2` in the high 16 bits and `s1` in the low
    /// 16 (`doc/rfc1950.txt` L328-L329). A fresh checksum starts from
    /// [`ADLER32_INITIAL_VALUE`], and successive calls accumulate, so feeding a sequence in
    /// chunks of any sizes yields the same result as feeding it in one call.
    ///
    /// An empty `buf` is a well-defined zero-length update that returns the incoming value
    /// with its halves normalised. It does **not** mean `Z_NULL` and does not yield the
    /// initial value; see the module documentation.
    ///
    /// Implementations never panic and never fail, for any input, which is why the return
    /// type is a plain `u32` rather than a fallible one.
    fn checksum(adler: u32, buf: &[u8]) -> u32;
}

/// Update a running Adler-32 checksum with `buf` and return the updated checksum.
///
/// Mirrors `adler32_z` (`adler32.c` L61-L125), and the primary entry point: everything
/// else in this module either delegates to it or is consumed by it. The reference sources
/// declare it with a `z_size_t` length (`zlib.h` L1829-L1830); a Rust slice carries its own
/// length, so the declaration collapses to a slice here and [`adler32`] becomes an exact
/// synonym.
///
/// # What it computes
///
/// `adler` is the running checksum, holding `s2` in the high 16 bits and `s1` in the low 16
/// (`doc/rfc1950.txt` L328-L329). A fresh checksum starts from [`ADLER32_INITIAL_VALUE`];
/// successive calls accumulate, so any chunking of an input yields the same value as one
/// call over the whole of it. An empty `buf` is an ordinary zero-length update and returns
/// the incoming value normalised -- it is **not** `Z_NULL` and does not yield the initial
/// value. See the module documentation for the full table of empty-slice results and for
/// why the null-pointer contract belongs to the facade crate instead.
///
/// # Which backend runs
///
/// This function is the dispatcher, and the choice is a throughput decision with no
/// observable consequence: both backends return the identical value for every input and
/// every starting value, as the tests below and the sweep in `simd` establish. The choice
/// is made in two stages.
///
/// * At build time, by the crate's `simd` feature. With the feature off -- the default --
///   the conditional block vanishes entirely at compile time, leaving a direct call into
///   the scalar backend with no runtime test and no dead code to skip.
/// * At run time, by the input's length **and then** by target-feature detection, so that one
///   binary built once still makes the right choice on each machine it lands on. Detection is
///   deliberately not cached in a `static`: that would require interior mutability and, in a
///   `no_std` build, atomics, for no benefit, since the query is cheap and the standard library's
///   own detection macro already caches internally.
///
/// ★ **The length is tested first, and the order is the point.** The vectorised backend hands every
/// input below the vectorised backend's own lane threshold -- 64 bytes -- straight back to the
/// scalar one, because
/// the lane arrangement cannot amortise its reductions over a shorter block. Asking whether the
/// machine has a vector unit before discovering that no vector code will run is work with no
/// possible payoff, and it lands on the inputs least able to absorb it: `deflate` and `inflate`
/// update the checksum once per `read_buf`, which for a caller feeding a stream in small chunks is
/// once per chunk, and a caller may legitimately feed one byte at a time (`adler32.c` L70 exists
/// for exactly that caller). Cheap is not free at that frequency. Testing the length first costs a
/// comparison against a constant and removes the detection call from every one of those calls.
///
/// The environment is never consulted, at run time or at build time. The `simd` feature is
/// the only thing that selects a backend: `ZLIB_RS_SIMD` in the facade's build script cannot
/// enable it -- cargo resolves features before it runs a build script -- so that variable
/// checks that the feature agrees with the caller's request and fails the build if it does
/// not. A library that inspected the environment on a hot path would be both slower and less
/// predictable than one that did not.
#[inline]
#[must_use]
pub fn adler32_z(adler: u32, buf: &[u8]) -> u32 {
    // Compiled away entirely without the `simd` feature, leaving the scalar call below as
    // the whole of this function.
    #[cfg(feature = "simd")]
    {
        // The length test comes first so that a short update never pays for detection; the
        // vectorised backend would only delegate straight back. See this function's documentation.
        //
        // `is_supported` is a hint about throughput, never about correctness: the vectorised
        // backend is portable safe Rust and computes the right answer on any target, so a wrong
        // answer here would cost speed and nothing else -- which is also why the two tests may be
        // reordered at all.
        if buf.len() >= simd::LANE_THRESHOLD_LEN && simd::is_supported() {
            return Adler32Simd::checksum(adler, buf);
        }
    }

    // Every input the block above declined, plus every input at all without the feature. The
    // scalar backend carries the same four-path structure C's `adler32_z` does -- the single byte
    // at L70, the short-length loop at L85, the block engine at L97 -- so a delegated short input
    // takes the identical path it would have taken through the vectorised backend, and returns the
    // identical value.
    Adler32Generic::checksum(adler, buf)
}

/// Update a running Adler-32 checksum with `buf` and return the updated checksum.
///
/// Mirrors `adler32` (`adler32.c` L128-L130), which is itself a one-line forwarder to
/// `adler32_z`. The two C declarations differ only in the width of the length argument --
/// `uInt` here (`zlib.h` L1809), `z_size_t` there (`zlib.h` L1829) -- a distinction a Rust
/// slice erases, so this function is an exact synonym for [`adler32_z`] and forwards to it
/// unchanged.
///
/// Both names are kept because `libz-rs-sys` must export both C symbols, and because this
/// is the name the rest of the implementation uses: it is what `read_buf.rs` calls when a stream is
/// wrapped in the zlib container, mirroring `deflate.c` L229. See [`adler32_z`] for the
/// semantics, the empty-slice contract and the backend dispatch, all of which apply here
/// unchanged.
#[inline]
#[must_use]
pub fn adler32(adler: u32, buf: &[u8]) -> u32 {
    adler32_z(adler, buf)
}

#[cfg(test)]
mod tests {
    use super::{
        adler32, adler32_combine, adler32_z, Adler32Backend, Adler32Generic, ADLER32_INITIAL_VALUE,
        BASE, NMAX,
    };
    use alloc::vec::Vec;

    #[cfg(feature = "simd")]
    use super::{simd, Adler32Simd};

    /// Length of the deterministic corpus every long-input expectation is taken over.
    ///
    /// 100 000 is 18 whole `NMAX` blocks plus a 64-byte remainder, so one call over the
    /// whole of it exercises the full-block loop, the partial-block tail and the byte tail
    /// within it.
    const CORPUS_LEN: usize = 100_000;

    /// Adler-32 of the whole corpus, started from [`ADLER32_INITIAL_VALUE`].
    const CORPUS_CHECKSUM: u32 = 0x76f5_980f;

    /// Exactly two `NMAX` blocks: `2 * 5_552` = 11 104 bytes.
    ///
    /// The smallest length that forces the block engine through two complete reduction
    /// cycles, which is the property this file is required to cover.
    const TWO_BLOCKS_LEN: usize = 2 * NMAX;

    /// [`TWO_BLOCKS_LEN`] really is more than one block, and a whole number of them.
    ///
    /// Checked at compile time so that the property survives an edit of [`NMAX`], and stated
    /// here rather than inside a test body because a runtime assertion over constants is
    /// dead weight the compiler folds away.
    const _: () = assert!(
        TWO_BLOCKS_LEN > NMAX && TWO_BLOCKS_LEN % NMAX == 0,
        "the long-input fixture must span a whole number of NMAX blocks, more than one"
    );

    /// Adler-32 of the first [`TWO_BLOCKS_LEN`] corpus bytes, started from 1.
    const TWO_BLOCKS_CHECKSUM: u32 = 0x4089_9b8c;

    /// Offset the concatenation test splits the corpus at, chosen as one whole block.
    const SPLIT_AT: usize = NMAX;

    /// Chunk sizes the incremental test feeds the corpus in.
    ///
    /// One byte drives the `len == 1` fast path on every call; 15, 16 and 17 straddle the
    /// short-path boundary at `adler32.c` L85; and the last three straddle the `NMAX`
    /// reduction boundary, so that a reduction lands mid-chunk, on a chunk edge and just
    /// past one.
    const CHUNK_SIZES: [usize; 7] = [1, 15, 16, 17, NMAX - 1, NMAX, NMAX + 1];

    /// Lengths that bracket every path boundary in both backends.
    ///
    /// 0/1/2 pick out the empty, single-byte and two-byte cases; 15/16/17 straddle the
    /// short-path boundary; 63/64/65 straddle the vectorised backend's lane threshold; and
    /// the rest straddle `NMAX` and its multiples.
    const BOUNDARY_LENS: [usize; 17] = [
        0,
        1,
        2,
        15,
        16,
        17,
        63,
        64,
        65,
        255,
        1_024,
        NMAX - 1,
        NMAX,
        NMAX + 1,
        TWO_BLOCKS_LEN,
        TWO_BLOCKS_LEN + 1,
        65_536,
    ];

    /// Starting values the dispatcher is swept from.
    ///
    /// The first four have halves already below [`BASE`]; the last three deliberately do
    /// not. A caller may legitimately supply either -- nothing in the C API rejects an
    /// unreduced starting value -- and the unreduced ones are what make the *placement* of
    /// each reduction observable, so they are the ones that would expose a backend that
    /// merged the reference implementation's three differently-reducing paths.
    const START_VALUES: [u32; 7] = [
        ADLER32_INITIAL_VALUE,
        0x0000_0000,
        0x000e_000e,
        0x1234_5678,
        0xffff_ffff,
        0xffff_fef1,
        0xfff0_fff0,
    ];

    /// `(start, len, expected)` triples produced by the in-tree C implementation.
    ///
    /// Every value here was read off `adler32.c` compiled with gcc, so these pin the
    /// *dispatcher* to the oracle rather than merely to the scalar backend beside it. The
    /// two unreduced starting values are included on purpose: they are where an
    /// almost-right implementation diverges.
    const ORACLE_VECTORS: [(u32, usize, u32); 8] = [
        (0xffff_ffff, 0, 0x000e_000e),
        (0xffff_ffff, 1, 0x0023_0015),
        (0xffff_ffff, 16, 0x3afe_0806),
        (0xffff_ffff, 1_024, 0x47e1_fe1d),
        (0xffff_ffff, TWO_BLOCKS_LEN, 0x7495_9b99),
        (0xffff_fef1, 1, 0xff06_fef8),
        (0xffff_fef1, 16, 0x2a1e_06f8),
        (0xffff_fef1, TWO_BLOCKS_LEN, 0xb2a3_9a8b),
    ];

    /// Build the deterministic corpus `buf[i] = (i * 31 + 7) & 0xff`.
    ///
    /// No I/O and no randomness, so every expectation above is reproducible on any machine
    /// and against the C implementation. Written as a `u8` accumulator advancing by 31
    /// rather than as a masked product because `u8` arithmetic *is* arithmetic modulo 256,
    /// which is exactly what the mask means, and this formulation needs no narrowing
    /// conversion.
    fn pattern_corpus(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut byte: u8 = 7;
        for _ in 0..len {
            out.push(byte);
            byte = byte.wrapping_add(31);
        }
        out
    }

    /// Take the first `len` bytes of `buf`, asserting the fixture is long enough.
    ///
    /// Goes through `get` rather than a range index so that these tests do not depend on
    /// indexing being permitted, and so that a fixture built too short fails with a clear
    /// message instead of a bare slice panic.
    fn prefix(buf: &[u8], len: usize) -> &[u8] {
        let taken = buf.get(..len).unwrap_or_default();
        assert_eq!(taken.len(), len, "the fixture is shorter than {len} bytes");
        taken
    }

    /// Take everything from `from` onwards, asserting the fixture reaches that far.
    fn suffix(buf: &[u8], from: usize) -> &[u8] {
        let taken = buf.get(from..).unwrap_or_default();
        assert_eq!(
            taken.len(),
            buf.len() - from,
            "the fixture is shorter than {from} bytes"
        );
        taken
    }

    #[test]
    fn constants_match_the_c_source() {
        assert_eq!(BASE, 65_521, "`BASE`, `adler32.c` L10");
        assert_eq!(NMAX, 5_552, "`NMAX`, `adler32.c` L11");
        assert_eq!(
            ADLER32_INITIAL_VALUE, 1,
            "the value `adler32(0L, Z_NULL, 0)` returns, `zlib.h` L1813-L1814"
        );

        // `adler32.c` L99 computes the block loop's iteration count as `NMAX / 16` and its
        // comment states the assumption: "NMAX is divisible by 16".
        assert_eq!(NMAX % 16, 0, "NMAX must be whole 16-byte steps");
        assert_eq!(NMAX / 16, 347, "the iteration count `n` at `adler32.c` L99");

        // What makes the two 16-bit residues pack losslessly into one `u32` is that `BASE`
        // is below 65536; that is enforced at compile time by the `const _` assertion beside
        // the constant itself, so restating it at runtime would only be folded away.
        assert_eq!(BASE >> 16, 0, "each component sum must fit in 16 bits");
    }

    /// `NMAX` is the largest `n` with `255n(n+1)/2 + (n+1)(BASE-1) <= 2^32-1`
    /// (`adler32.c` L11-L12) -- checked for `NMAX`, and checked to fail for `NMAX + 1`.
    ///
    /// The arithmetic runs in `u64` over literals that the first two assertions tie back to
    /// [`NMAX`] and [`BASE`], which keeps the test free of narrowing conversions while still
    /// testing the real constants.
    #[test]
    fn nmax_is_the_largest_length_that_cannot_overflow_a_u32() {
        let n: u64 = 5_552;
        let base: u64 = 65_521;
        let ceiling = u64::from(u32::MAX);

        assert_eq!(NMAX, 5_552, "the literals below must track `NMAX`");
        assert_eq!(BASE, 65_521, "the literals below must track `BASE`");

        // Grouped as `255 * (n(n+1)/2)` so that no intermediate exceeds the bound itself.
        let worst_case = |n: u64| 255 * (n * (n + 1) / 2) + (n + 1) * (base - 1);

        assert!(
            worst_case(n) <= ceiling,
            "NMAX bytes must be accumulable in a u32: {} > {ceiling}",
            worst_case(n)
        );
        assert!(
            worst_case(n + 1) > ceiling,
            "NMAX must be the LARGEST such length, but NMAX + 1 also fits"
        );
    }

    #[test]
    fn adler32_and_adler32_z_are_the_same_function() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for start in START_VALUES {
            for len in BOUNDARY_LENS {
                let buf = prefix(&corpus, len);
                assert_eq!(
                    adler32(start, buf),
                    adler32_z(start, buf),
                    "`adler32` and `adler32_z` disagree at start {start:#010x}, len {len}"
                );
            }
        }

        // Also over the byte strings the C test suite itself uses.
        for text in [
            b"".as_slice(),
            b"hello".as_slice(),
            b"hello, hello!".as_slice(),
            b"Wikipedia".as_slice(),
        ] {
            assert_eq!(adler32(ADLER32_INITIAL_VALUE, text), adler32_z(1, text));
        }
    }

    /// Vectors read off the C implementation, including the `"hello"` preset dictionary and
    /// the `"hello, hello!"` payload that `test/example.c` compresses.
    #[test]
    fn reference_vectors_from_the_c_implementation() {
        assert_eq!(adler32(ADLER32_INITIAL_VALUE, b"hello"), 0x062c_0215);
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, b"hello, hello!"),
            0x2170_0496
        );
        assert_eq!(adler32(ADLER32_INITIAL_VALUE, b"Wikipedia"), 0x11e6_0398);
    }

    /// An empty buffer is an ordinary zero-length update, **not** `Z_NULL`.
    ///
    /// The C entry point answers a null pointer with 1 (`adler32.c` L81-L82,
    /// `zlib.h` L1813-L1814), which is [`ADLER32_INITIAL_VALUE`]. An empty slice cannot
    /// carry that meaning: it returns the incoming checksum with its halves normalised. The
    /// two are conflated only once, and this test is what catches it.
    #[test]
    fn an_empty_buffer_is_an_ordinary_update_and_not_z_null() {
        assert_eq!(adler32(ADLER32_INITIAL_VALUE, &[]), 0x0000_0001);
        assert_eq!(adler32(0x0000_0000, &[]), 0x0000_0000);
        assert_eq!(adler32(0xffff_ffff, &[]), 0x000e_000e);

        // The value the C `Z_NULL` path returns is reachable only by naming the constant.
        assert_eq!(ADLER32_INITIAL_VALUE, 0x0000_0001);
        assert_ne!(
            adler32(0, &[]),
            ADLER32_INITIAL_VALUE,
            "an empty slice must not be treated as Z_NULL"
        );

        // Empty updates are idempotent, wherever they appear in a sequence.
        let once = adler32(ADLER32_INITIAL_VALUE, b"hello");
        let padded = adler32(adler32(adler32(ADLER32_INITIAL_VALUE, &[]), b"hello"), &[]);
        assert_eq!(once, padded);
    }

    /// Buffers longer than `NMAX`, so that the block engine reduces more than once.
    ///
    /// `TWO_BLOCKS_LEN` is exactly two full blocks and therefore exactly two reduction
    /// cycles with no tail; `CORPUS_LEN` is 18 blocks plus a 64-byte remainder, which adds
    /// the partial-block path on top.
    #[test]
    fn buffers_longer_than_nmax_cross_multiple_reduction_cycles() {
        let corpus = pattern_corpus(CORPUS_LEN);

        assert_eq!(TWO_BLOCKS_LEN, 11_104, "two whole NMAX blocks");
        assert_eq!(TWO_BLOCKS_LEN / NMAX, 2, "exactly two reduction cycles");

        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, prefix(&corpus, TWO_BLOCKS_LEN)),
            TWO_BLOCKS_CHECKSUM
        );
        assert_eq!(
            adler32(ADLER32_INITIAL_VALUE, &corpus),
            CORPUS_CHECKSUM,
            "18 whole blocks plus a 64-byte remainder"
        );

        // One byte either side of the first reduction, so an off-by-one in the block
        // boundary cannot pass unnoticed.
        for len in [
            NMAX - 1,
            NMAX,
            NMAX + 1,
            TWO_BLOCKS_LEN - 1,
            TWO_BLOCKS_LEN + 1,
        ] {
            let buf = prefix(&corpus, len);
            assert_eq!(
                adler32(ADLER32_INITIAL_VALUE, buf),
                Adler32Generic::checksum(ADLER32_INITIAL_VALUE, buf),
                "dispatch diverged from the scalar reference at len {len}"
            );
        }
    }

    /// Feeding the corpus in chunks reproduces the single-call value.
    ///
    /// This is the call pattern `deflate` and `inflate` actually use: start from
    /// [`ADLER32_INITIAL_VALUE`] and update as each buffer of input is consumed
    /// (`deflate.c` L229, `inflate.c` L300-L305). Because a chunk boundary can fall
    /// anywhere, the reduction schedule must be invariant under re-chunking.
    ///
    /// Skipped under Miri, and the last member of its class to be: `generic.rs` and `combine.rs`
    /// already skip their own whole-corpus folds there for the same reason. This is the heaviest of
    /// them, because it is the only test in the file that calls the entry point once *per byte* --
    /// the `chunks(1)` pass is 100 000 separate calls -- and the interpreter's cost for that
    /// pattern grows with the square of the corpus: measured on an external clock at 1 s for 500
    /// bytes and 86 s for 8 000, which extrapolates to hours at 100 000 and was observed reaching
    /// 4.0 GB of interpreter memory before the process was killed. Skipping it there costs no
    /// undefined-behaviour coverage: this module has no `unsafe`, no raw pointer and no
    /// uninitialised memory for Miri to inspect, the only faults it could surface are integer
    /// overflow and an out-of-bounds index, and both are reached by the cheap tests above, which
    /// run in full. The chunk-boundary path itself is interpreted anyway, by the `deflate` and
    /// `inflate` shards, which feed input buffer by buffer through this same entry point at the two
    /// C lines cited above. Nothing is skipped under a normal `cargo test`, where all seven
    /// chunkings run.
    #[cfg_attr(
        miri,
        ignore = "folds a 100 000-byte corpus a byte at a time; numeric agreement, no UB coverage"
    )]
    #[test]
    fn incremental_updates_agree_with_a_single_call() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for step in CHUNK_SIZES {
            let mut running = ADLER32_INITIAL_VALUE;
            for chunk in corpus.chunks(step) {
                running = adler32(running, chunk);
            }
            assert_eq!(
                running, CORPUS_CHECKSUM,
                "chunked in {step}-byte pieces, expected {CORPUS_CHECKSUM:#010x}"
            );
        }

        // A ragged split, to show the sizes need not be uniform.
        let mut running = ADLER32_INITIAL_VALUE;
        let mut rest: &[u8] = &corpus;
        let mut step = 1;
        while !rest.is_empty() {
            let take = step.min(rest.len());
            running = adler32(running, prefix(rest, take));
            rest = suffix(rest, take);
            step = step * 3 + 1;
        }
        assert_eq!(running, CORPUS_CHECKSUM, "ragged chunking changed it");
    }

    /// The concatenation entry point is reachable through this module and agrees with a
    /// single pass over the joined input.
    #[test]
    fn combine_is_reachable_through_this_module() {
        let corpus = pattern_corpus(CORPUS_LEN);
        let head = prefix(&corpus, SPLIT_AT);
        let tail = suffix(&corpus, SPLIT_AT);

        assert_eq!(tail.len(), 94_448, "the tail length passed as len2");
        assert_eq!(
            adler32_combine(
                adler32(ADLER32_INITIAL_VALUE, head),
                adler32(ADLER32_INITIAL_VALUE, tail),
                94_448,
            ),
            CORPUS_CHECKSUM
        );

        // A negative length is documented as meaningless (`zlib.h` L1844) and answered with
        // the debugging sentinel, never with a panic (`adler32.c` L138-L140).
        assert_eq!(adler32_combine(1, 1, -1), 0xffff_ffff);

        // Concatenating with an empty second sequence is the identity.
        let whole = adler32(ADLER32_INITIAL_VALUE, &corpus);
        assert_eq!(adler32_combine(whole, ADLER32_INITIAL_VALUE, 0), whole);
    }

    /// Whichever backend the dispatcher selects, it must agree with the scalar reference.
    ///
    /// Compiled in every feature configuration, so with `simd` off it confirms the
    /// dispatcher forwards faithfully, and with `simd` on it confirms the selected
    /// vectorised backend is output-neutral *through the dispatcher* -- which is the part
    /// the sweeps inside the backend modules cannot themselves cover.
    #[test]
    fn the_dispatcher_agrees_with_the_scalar_backend() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for start in START_VALUES {
            for len in BOUNDARY_LENS {
                let buf = prefix(&corpus, len);
                assert_eq!(
                    adler32_z(start, buf),
                    Adler32Generic::checksum(start, buf),
                    "dispatch diverged at start {start:#010x}, len {len}"
                );
            }
        }
    }

    /// The dispatcher reproduces the C implementation's own output, unreduced starting
    /// values included.
    #[test]
    fn the_dispatcher_matches_the_c_oracle() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for (start, len, expected) in ORACLE_VECTORS {
            let buf = prefix(&corpus, len);
            assert_eq!(
                adler32_z(start, buf),
                expected,
                "oracle mismatch at start {start:#010x}, len {len}"
            );
        }
    }

    /// Both backends and the dispatcher compute the identical value.
    ///
    /// The vectorised backend may change throughput and nothing else, so this is the
    /// property that licenses selecting it at all. The exhaustive sweep lives in the backend
    /// module; what is added here is the third term -- the dispatcher itself.
    #[cfg(feature = "simd")]
    #[test]
    fn both_backends_and_the_dispatcher_agree() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for start in START_VALUES {
            for len in BOUNDARY_LENS {
                let buf = prefix(&corpus, len);
                let scalar = Adler32Generic::checksum(start, buf);
                let vector = Adler32Simd::checksum(start, buf);
                let dispatched = adler32(start, buf);

                assert_eq!(
                    scalar, vector,
                    "backends diverged at start {start:#010x}, len {len}"
                );
                assert_eq!(
                    dispatched, scalar,
                    "dispatch diverged at start {start:#010x}, len {len}"
                );
            }
        }
    }

    /// The dispatcher routes to the backend the build actually compiled.
    ///
    /// The Adler-32 counterpart of `crc32`'s `backend_selection_follows_the_simd_feature`,
    /// and the in-crate half of the `ZLIB_RS_SIMD` contract: `crates/libz-rs-sys/build.rs`
    /// reconciles a caller's request against the resolved `simd` feature, refuses a build in
    /// which the two disagree, and records the outcome as
    /// `ZLIB_RS_CHECKSUM_BACKEND=simd|scalar`. That record is only worth anything if the
    /// dispatcher then routes to the recorded backend, which is what this asserts -- through
    /// both stages of the choice, the feature and, where the feature is on, the run-time
    /// probe.
    ///
    /// It cannot fail while `both_backends_and_the_dispatcher_agree` passes, and that is the
    /// point: the two backends are interchangeable, so nothing but an assertion on the route
    /// itself can tell which one ran.
    #[test]
    fn backend_selection_follows_the_simd_feature() {
        let corpus = pattern_corpus(CORPUS_LEN);

        for start in START_VALUES {
            for len in BOUNDARY_LENS {
                let buf = prefix(&corpus, len);

                #[cfg(feature = "simd")]
                let expected = if simd::is_supported() {
                    Adler32Simd::checksum(start, buf)
                } else {
                    Adler32Generic::checksum(start, buf)
                };
                #[cfg(not(feature = "simd"))]
                let expected = Adler32Generic::checksum(start, buf);

                assert_eq!(
                    adler32(start, buf),
                    expected,
                    "dispatch took the wrong backend at start {start:#010x}, len {len}"
                );
            }
        }
    }
}
