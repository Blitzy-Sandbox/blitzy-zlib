//! The CRC-32 engine: the module tree, the entry points, the backend contract, the choice of
//! backend, and the one place the one's-complement conditioning is applied.
//!
//! This module is the public face of the CRC-32 implementation and, together with the five
//! sibling modules it declares, the port of `crc32.c` -- 983 lines that remain in the tree
//! unmodified, serving as the differential oracle every value produced here is measured against
//! -- plus its generated companion `crc32.h`, 9,446 lines of tables. Not one table lookup
//! happens in this file. It owns the three entry points the public header declares, the contract
//! a backend satisfies, the choice of which backend runs, and the two complements that turn a
//! backend's running state into a CRC-32 value. The arithmetic lives next door.
//!
//! # The check value
//!
//! A CRC-32 is the remainder left by dividing the message polynomial by the generator
//! `x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 + x^4 + x^2 + x + 1`
//! over `GF(2)` (`crc32.c` L219-L220). Coefficients are held reflected, lowest power in the most
//! significant bit, which is what makes adding polynomials an exclusive-or and multiplying by `x`
//! a right shift (`crc32.c` L222-L229); in that form the generator is the constant
//! [`tables::POLY`] `= 0xedb88320` (`crc32.c` L157), with the `x^32` term implied. This is the
//! CRC-32 of ISO 3309 and of section 8.1.1.6.2 of ITU-T recommendation V.42
//! (`doc/rfc1952.txt` L419-L426). `zlib.h` L1849-L1850 fixes its range -- "A CRC-32 value is in
//! the range of a 32-bit unsigned integer" -- which is why every signature below takes and
//! returns one `u32` and no wider type appears anywhere in this module.
//!
//! Two one's complements bracket the computation: the register is inverted on the way in and
//! inverted again on the way out. `zlib.h` L1852-L1854 makes that the library's business and not
//! the caller's -- "Pre- and post-conditioning (one's complement) is performed within this
//! function so it shouldn't be done by the application" -- and applying those two complements,
//! exactly once each, is the whole of what this file computes.
//!
//! # Where this check value is used
//!
//! It is the check value of the gzip container rather than an internal convenience, so its
//! correctness is a wire-format property and not merely an implementation detail. RFC 1952 puts
//! it in the four bytes immediately before `ISIZE` in a member trailer
//! (`doc/rfc1952.txt` L276) and defines it over the *uncompressed* data
//! (`doc/rfc1952.txt` L419-L426). Four call sites in this port therefore depend on this module:
//!
//! * `deflate` accumulates it over the uncompressed input as that input is copied into the
//!   window -- `read_buf` at `deflate.c` L219-L240, reached at L233 only when `wrap == 2`, whose
//!   counterpart in this crate is `read_buf.rs` -- and writes the finished value into the gzip
//!   trailer. That call site is the one that decides whether this port's gzip output is
//!   byte-identical to the reference implementation's, which is why [`crc32()`] carries exactly
//!   that name and that signature.
//! * `inflate` recomputes it over the bytes it produces and compares the result against the
//!   trailer it read. The reference sources reach the check value through the `UPDATE_CHECK`
//!   macro at `inflate.c` L300-L305, which selects CRC-32 for a gzip stream and Adler-32 for a
//!   zlib one.
//! * The optional gzip header check, `FHCRC`, is "the two least significant bytes of the CRC32
//!   for all bytes of the gzip header up to and not including the CRC16"
//!   (`doc/rfc1952.txt` L325-L328). Both sides compute it over header bytes rather than over
//!   payload: `deflate.c` L976 and L1111 run [`crc32_z`] across the pending buffer, and
//!   `inflate.c` L314, L323 and L623-L667 run [`crc32()`] across each header field as it is
//!   parsed.
//! * The `gz*` file layer computes it for every member it writes and verifies it for every
//!   member it reads, which is what makes a truncated or corrupted `.gz` file detectable.
//!
//! The reference implementation also uses the API's own null-buffer idiom, `crc32(0L, Z_NULL, 0)`,
//! purely to obtain a fresh check value -- `deflate.c` L669, L1068 and L1198, and
//! `inflate.c` L516 and L690. See the `Z_NULL` section below for why that idiom has no spelling
//! in this crate and needs none.
//!
//! # Module map
//!
//! | Module | Ported from | Role |
//! |---|---|---|
//! | `tables` | `crc32.h` | The generated tables as `const` data: [`CRC_TABLE`] and friends |
//! | `generic` | `crc32.c` L922-L937 | Byte-at-a-time reference path: [`Generic`] |
//! | `braid` | `crc32.c` L637-L920 | Braided word-at-a-time fast path: [`Braid`] |
//! | `combine` | `crc32.c` L156-L195, L954-L983 | Concatenation: [`crc32_combine64`] and friends |
//! | `simd` | -- | Optional, output-neutral throughput work; `simd` feature only |
//!
//! `tables` is the only one of the five that is `pub`, because
//! `crates/zlib-rs-differential/tests/table_equality.rs` compares its arrays against `crc32.h`
//! element for element -- the cheapest high-signal check in the whole port. The other four are
//! private, and the items this module re-exports from them are the entirety of their reachable
//! surface: everything the C sources declare `local` -- `braid`, `crc_word`, `crc_word_big`,
//! `byte_swap`, `multmodp`, `x2nmodp` -- stays inside the subsystem, exactly as it does in C.
//!
//! `simd` is compiled only when the crate's `simd` feature is enabled and has no counterpart in
//! the C sources; it re-associates the same `GF(2)` arithmetic and is required to agree with
//! `generic` on every input.
//!
//! # The conditioning boundary
//!
//! **This file is the only place in the subsystem where a complement is applied, and it applies
//! each of them exactly once.** Every function in `generic`, `braid` and `simd` takes and returns
//! an already pre-conditioned state and complements nothing, so [`crc32()`] is precisely
//! `crc32.c` L635 and L940 wrapped around a backend call:
//!
//! ```text
//! crc = (~crc) & 0xffffffff;      /* crc32.c L635  -- pre-condition  */
//! ...                             /* the braided body and the byte tail */
//! return crc ^ 0xffffffff;        /* crc32.c L940  -- post-condition */
//! ```
//!
//! Three consequences follow, and all three are load-bearing:
//!
//! * **The entry point is resumable.** One call's post-conditioning is undone by the next call's
//!   pre-conditioning, so `crc32(crc32(0, a), b)` is the check value of `a` followed by `b` for
//!   any split. That identity is what lets `deflate` accumulate a member's check value across
//!   however many calls the caller happens to make, with whatever `avail_in` each one carries.
//! * **The wide paths can be entered mid-computation.** `braid` and `simd` hand their prologue
//!   and remainder bytes to `generic`, and all three can be called with a partial state, because
//!   none of them conditions anything.
//! * **Getting it wrong yields the exact complement of the right answer**, which looks like a
//!   table fault or an endianness fault and is neither. A second complement anywhere below this
//!   file would produce plausible check values that match nothing.
//!
//! Working in `u32` throughout is exactly faithful rather than a simplification. The C code holds
//! `crc` in a `uLong`, which is 64 bits on an LP64 target, but masks it on entry: `(~crc) &
//! 0xffffffff` keeps only the low 32 bits of the complement, and that is `!(crc as u32)` -- the
//! high half of the argument cannot influence the result. Symmetrically `crc ^ 0xffffffff` is
//! `!crc` in `u32`. So the mask is carried by the type here, no stray high bits can survive, and
//! reconciling the caller's `uLong` with these `u32` signatures is the facade's job and its
//! alone.
//!
//! # Three entry points, one algorithm
//!
//! [`crc32()`], [`crc32_z`] and [`get_crc_table`] are the three functions `zlib.h` declares for
//! this family, and the first two are the same function. That mirrors the reference sources,
//! where `crc32` (`crc32.c` L946-L951) is a one-line forwarder to `crc32_z`
//! (`crc32.c` L626-L941) and the two differ only in the declared width of the length argument:
//! `uInt` for one (`zlib.h` L1848), `z_size_t` for the other (`zlib.h` L1866-L1867). A Rust slice
//! carries its own length, so that distinction disappears here and the two are interchangeable.
//!
//! Keeping both names is nonetheless deliberate. `libz-rs-sys` has to export both C symbols, and
//! the reference implementation itself calls both from inside the library -- `crc32` over input
//! bytes at `deflate.c` L233, `crc32_z` over the pending buffer at `deflate.c` L976 and L1111 --
//! so giving each one a same-named Rust function preserves the one-to-one correspondence with the
//! C sources that the rest of this port is organised around.
//!
//! The combine family is re-exported here rather than reimplemented: [`crc32_combine`],
//! [`crc32_combine64`], [`crc32_combine_gen`], [`crc32_combine_gen64`] and [`crc32_combine_op`]
//! splice two independently computed check values into the check value of their concatenation
//! without re-reading either sequence. The C library exports five symbols for that operation and
//! `combine` implements it once, because only the argument width differs between the suffixed and
//! unsuffixed forms (`zlib.h` L1874, L1884, L1890, L1983-L1984 and L2002-L2003).
//!
//! # Backend selection is a throughput decision only
//!
//! [`crc32()`] folds its input through one backend, chosen at build time by the crate's `simd`
//! feature: `Simd` when it is on, [`Braid`] when it is off. Both are required to return the
//! *identical* value for every input and every starting value, so the choice can never be
//! observed in the output -- only in the time taken. [`Generic`] stays reachable as well, so that
//! the differential suite, the fuzz targets and the benchmarks can pin a computation to the
//! byte-at-a-time reference path and compare.
//!
//! Short inputs take the same path they take in C whichever backend is selected, because each
//! wide backend falls back on its own: [`Braid`] delegates the whole buffer to [`Generic`] below
//! `N * W + W - 1` bytes -- 47 where `W` is 8, 19 where it is 4 -- reproducing the threshold at
//! `crc32.c` L640, and `Simd` delegates to [`Braid`] below its own slightly larger threshold.
//!
//! There is no run-time dispatch here, and none is needed: the vectorization-friendly backend is
//! written in portable safe integer arithmetic with no architecture intrinsic to guard, so a
//! `simd`-enabled build computes the right answer on a machine with no vector unit at all. That
//! is why nothing in this module queries target features, and why nothing should be added that
//! does. The `ZLIB_RS_SIMD` environment toggle is read once, at build time, by
//! `crates/libz-rs-sys/build.rs`, which maps it onto the `simd` feature; a library that consulted
//! the environment on a hot path would be both slower and less predictable than one that did not.
//!
//! That vectorizing a check value is permissible at all is specific to checksums. A CRC-32 is a
//! single scalar in `GF(2)` however its linear recurrence is evaluated, so independent partial
//! remainders can be recombined exactly. The same reasoning emphatically does not extend to the
//! compressor, where the order in which match candidates are examined decides which match is
//! emitted; vectorised match finding is therefore prohibited in this port while vectorised
//! checksums are welcome. The two cases look alike and must not be conflated.
//!
//! # `Z_NULL`, and why an empty slice is not it
//!
//! The C API overloads its buffer argument. `zlib.h` L1851-L1852 promises that "If buf is
//! `Z_NULL`, this function returns the required initial value for the crc", implemented as the
//! early return `if (buf == Z_NULL) return 0;` at `crc32.c` L627-L628, and `zlib.h` L1858 shows
//! the idiom callers are expected to write: `uLong crc = crc32(0L, Z_NULL, 0);`.
//!
//! A `&[u8]` cannot be null, so that case cannot arise in this crate at all; it is honoured one
//! layer up, at the FFI boundary in `crates/libz-rs-sys/src/checksum.rs`, which must answer a
//! null pointer with `0` *without* calling in here. The asymmetry is worth stating precisely,
//! because it is the one place a faithful facade differs from a naive one: the C entry point
//! returns the initial value regardless of the `crc` argument, so `crc32(5, Z_NULL, 0)` is `0`
//! and not `5`.
//!
//! **An empty slice is not `Z_NULL`.** Every value in this table was read off the C
//! implementation:
//!
//! | Call | Result |
//! |---|---|
//! | `crc32(0, Z_NULL, 0)` in C | `0x0000_0000` -- the initial value |
//! | `crc32(5, Z_NULL, 0)` in C | `0x0000_0000` -- still the initial value, **not** `5` |
//! | `crc32(0, &[])` | `0x0000_0000` |
//! | `crc32(5, &[])` | `0x0000_0005` -- the incoming value, **not** the initial value |
//! | `crc32(0xffff_ffff, &[])` | `0xffff_ffff` |
//!
//! An empty slice is an ordinary zero-length update: the two complements cancel, `((~crc) &
//! 0xffffffff) ^ 0xffffffff` is `crc`, and the incoming value is returned unchanged. That the
//! two rows agree for a zero starting value -- both `0` -- is exactly why the C usage idiom
//! works, and it is also why this family needs no named initial-value constant, unlike Adler-32,
//! whose initial value is `1` and whose module therefore exports one.
//!
//! # No runtime initialization
//!
//! The tables are `const` data, materialized by the compiler, so [`get_crc_table`] is a trivial
//! always-valid accessor. The reference implementation's `get_crc_table` exists partly for the
//! opposite reason: under `DYNAMIC_CRC_TABLE` it calls `z_once(&made, make_crc_table)`
//! (`crc32.c` L482-L487) and callers are told to invoke it before letting a second thread near
//! `crc32()`, because "there is no mutex or semaphore protection on the static variables used to
//! control the first-use generation of the crc tables" (`crc32.c` L12-L17). None of that exists
//! here. There is no `z_once_t`, no `OnceLock`, no `static mut`, no interior mutability and no
//! lazily built table anywhere in this subsystem, so the hazard is absent rather than mitigated
//! and every function in this module is safe to call from any thread at any time.
//!
//! One externally visible consequence follows: because this port has no dynamic CRC table, bit 13
//! of `zlibCompileFlags()` -- `DYNAMIC_CRC_TABLE` -- is honestly reported CLEAR by
//! `crates/libz-rs-sys/src/util.rs`.
//!
//! # Layering and safety posture
//!
//! This module is a leaf of the crate's dependency graph. It names `core` only, reaches into no
//! other subsystem -- not `adler32`, `deflate`, `inflate` or `gz` -- and the dependency runs the
//! other way, from `read_buf.rs` to here. That keeps the crate free of internal cycles and lets
//! Miri exercise the check-value path in isolation.
//!
//! Nothing here declares C-visible linkage or a C-visible layout. The eight exported C symbols of
//! this family -- `crc32`, `crc32_z`, `get_crc_table`, `crc32_combine`, `crc32_combine64`,
//! `crc32_combine_gen`, `crc32_combine_gen64` and `crc32_combine_op`, all eight present in the
//! reference library's dynamic symbol table -- are defined in
//! `crates/libz-rs-sys/src/checksum.rs`, which calls into this module. The separation is absolute:
//! no item below may be given a stable exported symbol name, however convenient that might seem.
//! Note that `zlib.map` does not hide any of the eight, since they are public API; the `local:`
//! block at `zlib.map` L9-L19 hides internals of other subsystems. That is the facade's concern
//! either way, and the internals named in the module map above stay crate-private here
//! regardless.
//!
//! The braided design the fast path implements is due to Kadatch and Jenkins (2010), credited at
//! `crc32.c` L5-L7; the paper is carried in this distribution at `doc/crc-doc.1.0.pdf`.
//!
//! # Examples
//!
//! ```ignore
//! // A fresh check value starts from zero -- the same value the C `Z_NULL` idiom returns.
//! let mut crc = 0;
//!
//! // Accumulate incrementally, exactly as `deflate` does through `read_buf` when `wrap == 2`.
//! crc = crc32(crc, b"hello, ");
//! crc = crc32(crc, b"hello!");
//! assert_eq!(crc, 0xb39a_dc9b);
//!
//! // Or all at once: chunking cannot change the answer.
//! assert_eq!(crc32(0, b"hello, hello!"), 0xb39a_dc9b);
//!
//! // Two check values can be spliced without re-reading either sequence.
//! let head = crc32(0, b"hello, ");
//! let tail = crc32(0, b"hello!");
//! assert_eq!(crc32_combine64(head, tail, 6), 0xb39a_dc9b);
//!
//! // The published check value of the nine ASCII digits, and the table `get_crc_table` returns.
//! assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
//! assert_eq!(get_crc_table()[1], 0x7707_3096);
//! ```

pub mod tables;

mod braid;
mod combine;
mod generic;
#[cfg(feature = "simd")]
mod simd;

// -----------------------------------------------------------------------------
//  Re-exports: the whole CRC-32 surface reachable from one path
// -----------------------------------------------------------------------------

/// The braided word-at-a-time backend -- see [`braid::Braid`].
///
/// Always available, on every target and in every feature configuration, and selected by
/// [`crc32`] unless the `simd` feature is enabled. Falls back to [`Generic`] for inputs shorter
/// than `N * W + W - 1` bytes, exactly as `crc32.c` L640 does.
pub use self::braid::Braid;

/// Combination of two CRC-32 check values, narrow-length form -- see
/// [`combine::crc32_combine`].
///
/// Backs the exported C symbol `crc32_combine` (`zlib.h` L1874). Identical to
/// [`crc32_combine64`]; the C library has two symbols only because `z_off_t` and `z_off64_t` may
/// differ (`zlib.h` L2002-L2003, L2020-L2021).
pub use self::combine::crc32_combine;

/// Combination of two CRC-32 check values -- see [`combine::crc32_combine64`].
///
/// Backs the exported C symbol `crc32_combine64` (`zlib.h` L1983) and, through
/// [`crc32_combine`], the unsuffixed `crc32_combine` as well. Returns `0` for a negative
/// `len2` and `crc1 ^ crc2` for a zero one, per `zlib.h` L1875-L1881.
pub use self::combine::crc32_combine64;

/// The combination operator for a given second-sequence length, narrow-length form -- see
/// [`combine::crc32_combine_gen`].
///
/// Backs the exported C symbol `crc32_combine_gen` (`zlib.h` L1884). Identical to
/// [`crc32_combine_gen64`], for the same width reason as [`crc32_combine`].
pub use self::combine::crc32_combine_gen;

/// The combination operator for a given second-sequence length -- see
/// [`combine::crc32_combine_gen64`].
///
/// Backs the exported C symbol `crc32_combine_gen64` (`zlib.h` L1984). Computing the operator
/// once and reusing it with [`crc32_combine_op`] beats recomputing it from the same length, which
/// is the advice at `zlib.h` L1892-L1894 and the entire purpose of the split.
pub use self::combine::crc32_combine_gen64;

/// Application of a precomputed combination operator -- see [`combine::crc32_combine_op`].
///
/// Backs the exported C symbol `crc32_combine_op` (`zlib.h` L1890). It takes no length, so the
/// large-file duality never touches it and there is no `*64` variant to export. Returns `0` for a
/// zero operator, per `crc32.c` L970-L971.
pub use self::combine::crc32_combine_op;

/// The byte-at-a-time reference backend -- see [`generic::Generic`].
///
/// Always available, on every target and in every feature configuration. It is the *definition*
/// of what the wide backends must produce, and it is kept reachable so that the differential
/// suite, the fuzz targets and the benchmarks can pin a computation to it and compare.
pub use self::generic::Generic;

/// The optional throughput-oriented backend -- see [`simd::Simd`].
///
/// Present only when the crate's `simd` feature is enabled, and interchangeable with [`Braid`]
/// and [`Generic`] wherever it is. It contains no architecture intrinsic, so it needs no
/// support query and no run-time dispatch.
#[cfg(feature = "simd")]
pub use self::simd::Simd;

/// Word-sized big-endian CRC-32 table -- see [`tables::CRC_BIG_TABLE`].
///
/// Re-exported so the whole subsystem is reachable from one path and so that
/// `crates/zlib-rs-differential/tests/table_equality.rs` can compare it against `crc32.h`.
pub use self::tables::CRC_BIG_TABLE;

/// Braided big-endian CRC-32 tables -- see [`tables::CRC_BRAID_BIG_TABLE`].
///
/// Re-exported for the same reason as [`CRC_BIG_TABLE`].
pub use self::tables::CRC_BRAID_BIG_TABLE;

/// Braided little-endian CRC-32 tables -- see [`tables::CRC_BRAID_TABLE`].
///
/// Re-exported for the same reason as [`CRC_BIG_TABLE`].
pub use self::tables::CRC_BRAID_TABLE;

/// Byte-wise CRC-32 table: the check value of every possible eight-bit value -- see
/// [`tables::CRC_TABLE`].
///
/// This is the table [`get_crc_table`] hands back, transcribed from `crc_table[]` at
/// `crc32.h` L5-L57. `crates/zlib-rs/src/lib.rs` re-exports this name from the crate root, so it
/// is reachable both as `crc32::CRC_TABLE` and as `crc32::tables::CRC_TABLE`.
pub use self::tables::CRC_TABLE;

/// Table of powers of `x` used to combine check values -- see [`tables::X2N_TABLE`].
///
/// Transcribed from `x2n_table[]` at `crc32.h` L9439-L9446 and consumed by `combine`. Re-exported
/// for the same reason as [`CRC_BIG_TABLE`].
pub use self::tables::X2N_TABLE;

// -----------------------------------------------------------------------------
//  The backend contract
// -----------------------------------------------------------------------------

/// A swappable CRC-32 computation backend.
///
/// The trait exists so that the byte-at-a-time engine, the braided engine and the optional
/// throughput-oriented engine are interchangeable *by construction* rather than by convention:
/// [`crc32`] selects between them through this one function, the benchmarks compare them through
/// it, and the equivalence tests pin a computation to one specific implementation through it.
///
/// # Implementors take and return pre-conditioned state
///
/// [`update`](Crc32Backend::update) is handed a state that has already had the leading one's
/// complement applied and returns one that still needs the trailing one -- `crc32.c` L635 and
/// L940 respectively, both of which belong to [`crc32`] and to nothing else. An implementation
/// that complemented either end would be wrong in a way that looks like a table or endianness
/// fault: every value it produced would be the exact complement of the right one.
///
/// # Implementors are required to be indistinguishable
///
/// Every implementation must be pure -- a function of `(crc, buf)` alone, with no hidden state,
/// no interior mutability and no lazily built table -- and must return the same value as
/// [`Generic`] for every `crc`/`buf` pair, including a starting state whose bits are arbitrary
/// and an empty `buf`. That is a hard contract, not an aspiration: a CRC-32 is part of the gzip
/// wire format, so a backend that differed in one bit would produce members the reference
/// implementation rejects. A backend may differ only in how long it takes.
///
/// This clause is the whole reason vectorising a check value is permitted in this port at all. A
/// CRC-32 is one scalar in `GF(2)` however its recurrence is evaluated, so independent partial
/// remainders recombine exactly; the trait boundary is the mechanism that keeps that guarantee
/// checkable, and the tests at the foot of this file are where it is checked.
///
/// # Shape of the items
///
/// Both items are associated and neither takes `self`, so a backend is named rather than
/// constructed and every implementing type can be zero-sized: `Generic::update(crc, buf)`,
/// `Braid::NAME`. Consequently the trait is deliberately not `dyn`-compatible -- nothing in this
/// library needs dynamic dispatch, and routing a check value through a vtable would defeat the
/// inlining the throughput targets depend on.
//
// The name repeats the module name, which `clippy::module_name_repetitions` objects to. That lint
// sits in the pedantic set this workspace denies at the declared MSRV of 1.80 and has since been
// reclassified, so it does not fire on every toolchain -- but the allow is kept, because the name
// is fixed regardless: the plan mandates it, `crates/zlib-rs/src/lib.rs` names it in its documented
// re-export surface, and all three backend modules are already written against it, so renaming it
// would break three implementations and the crate root at once. Scoped to this one item rather
// than applied to the module, and matching how `config.rs`, `deflate/state.rs` and
// `inflate/state.rs` handle the same collision.
#[allow(clippy::module_name_repetitions)]
pub trait Crc32Backend {
    /// Short identifier for diagnostics and benchmark labels.
    ///
    /// A stable, lowercase, human-readable name: `"generic"`, `"braid"`, `"simd"`. It exists so a
    /// failing comparison or a benchmark row can say which engine produced a value, and it is an
    /// associated `const` rather than a function so that it can be used in a format string
    /// without constructing anything.
    const NAME: &'static str;

    /// Fold `buf` into the already pre-conditioned check-value state `crc`.
    ///
    /// Returns the new state, still pre-conditioned. Pass a state that has had the leading
    /// complement applied and treat the result as one that still needs the trailing complement;
    /// [`crc32`] is the only function that applies either.
    ///
    /// Successive calls accumulate, so feeding a sequence in chunks of any sizes yields the same
    /// state as feeding it in one call. An empty `buf` is a well-defined zero-length update that
    /// returns `crc` unchanged.
    ///
    /// Implementations never panic and never fail, for any input, which is why the return type is
    /// a plain `u32` rather than a fallible one.
    fn update(crc: u32, buf: &[u8]) -> u32;
}

// -----------------------------------------------------------------------------
//  Invariants, pinned at compile time
// -----------------------------------------------------------------------------

/// The byte-wise table must have one entry per byte value.
///
/// [`get_crc_table`] returns `&'static [u32; 256]`, and `crc32.c` L482-L487 hands back a pointer
/// into a 256-entry `crc_table[]` declared at `crc32.h` L5. Pinning the length here makes a future
/// edit of the transcription a build failure instead of a facade that publishes a shorter table
/// than the C API promises.
const _: () = assert!(
    CRC_TABLE.len() == 256,
    "crc32.h L5: crc_table[] has one entry per byte value"
);

/// Truncating to `u32` is exactly the mask the C source applies, so no high bit can survive.
///
/// `crc32.c` L635 pre-conditions with `crc = (~crc) & 0xffffffff` on a `uLong` that is 64 bits
/// wide on an LP64 target, and `crc32.c` L940 post-conditions with `crc ^ 0xffffffff`. This
/// checks the equivalence this module's `u32` signatures rest on -- that masking the complement of
/// a wide value equals complementing its low half, and that the trailing exclusive-or is the same
/// complement -- against a witness with every high bit set. If the two ever diverged, working in
/// `u32` would silently drop information that the C entry point keeps.
///
/// The cast is the one this argument is *about*: it is the `& 0xffffffff` of the C source written
/// in the type system, so it can only discard bits the C source discards too.
#[allow(clippy::cast_possible_truncation)]
const _: () = {
    let wide: u64 = 0xdead_beef_1234_5678;
    assert!(((!wide) & 0xffff_ffff) as u32 == !(wide as u32));
    assert!((!(wide as u32)) ^ 0xffff_ffff == wide as u32);
};

// -----------------------------------------------------------------------------
//  Backend selection
// -----------------------------------------------------------------------------

/// The backend [`crc32`] folds its input through, chosen at build time.
///
/// `Simd` when the crate's `simd` feature is enabled, [`Braid`] otherwise. The alias is private
/// because it is an implementation detail of dispatch: a caller that needs one *specific* engine
/// names [`Generic`], [`Braid`] or `Simd` itself and reaches it through [`Crc32Backend`], which is
/// what the differential suite, the fuzz targets and the benchmarks do.
///
/// Selection is output-neutral by contract -- see [`Crc32Backend`] -- so this alias can change
/// which instructions run and how long they take, never which bytes a stream carries. It is also
/// the *only* switch: there is no run-time probe, because the vectorization-friendly backend is
/// portable integer arithmetic with no architecture intrinsic to guard, and `ZLIB_RS_SIMD` is read
/// at build time by `crates/libz-rs-sys/build.rs`, never here.
#[cfg(feature = "simd")]
type Selected = Simd;

/// The backend [`crc32`] folds its input through, chosen at build time.
///
/// See the `simd` variant above for the full derivation. Without that feature -- the default --
/// the braided engine of `crc32.c` L637-L920 runs, which itself hands short inputs to [`Generic`]
/// below the `N * W + W - 1` threshold of `crc32.c` L640.
#[cfg(not(feature = "simd"))]
type Selected = Braid;

// -----------------------------------------------------------------------------
//  Entry points
// -----------------------------------------------------------------------------

/// Update a running CRC-32 with `buf` and return the updated check value.
///
/// Port of `crc32` (`crc32.c` L946-L951) and of the body of `crc32_z` (`crc32.c` L626-L941) that
/// it forwards to. This is the function the rest of the port calls: `read_buf.rs` uses it to
/// accumulate a gzip member's check value over uncompressed input when `wrap == 2`, mirroring
/// `deflate.c` L233, and the header path mirrors `inflate.c` L314-L323 and L623-L667.
///
/// # What it computes
///
/// `start` is the running check value and the return value is the updated one, both plain 32-bit
/// values (`zlib.h` L1849-L1850). A fresh check value starts from `0`; successive calls
/// accumulate, so any chunking of an input yields the same value as one call over the whole of it.
///
/// # Conditioning
///
/// This function is the one place in the subsystem where the one's complements are applied, and it
/// applies each exactly once: `!start` is `crc32.c` L635's `crc = (~crc) & 0xffffffff`, and the
/// closing `!state` is `crc32.c` L940's `return crc ^ 0xffffffff`. Everything in between runs on
/// pre-conditioned state and complements nothing. Because the two cancel across a call boundary,
/// the function is resumable: `crc32(crc32(0, a), b)` is the check value of `a` followed by `b`.
///
/// # An empty slice is not `Z_NULL`
///
/// `crc32(start, &[])` returns `start`: the two complements cancel and the state is untouched. The
/// C entry point additionally answers a *null* pointer with the initial value `0` regardless of
/// its `crc` argument (`crc32.c` L627-L628, `zlib.h` L1851-L1852), so `crc32(5, Z_NULL, 0)` is `0`
/// while `crc32(5, &[])` is `5`. A slice cannot be null, so that case belongs to
/// `crates/libz-rs-sys/src/checksum.rs`; see the module documentation for the full table.
///
/// # Which backend runs
///
/// Whichever the `simd` feature selected at build time. Both are required to return the identical
/// value for every input and every starting value, as the tests below establish, so the choice
/// costs or saves time and can never be observed in the output. There is no run-time dispatch.
#[must_use]
pub fn crc32(start: u32, buf: &[u8]) -> u32 {
    // `crc32.c` L635: `crc = (~crc) & 0xffffffff`. The C source masks because its `uLong` is wider
    // than a check value on an LP64 target; the `u32` here carries that mask in the type, and the
    // compile-time assertion above pins the equivalence.
    let state = !start;

    // The braided body and the byte tail, `crc32.c` L637-L937. The backend neither conditions nor
    // inspects `start`, which is what makes the call resumable.
    let state = <Selected as Crc32Backend>::update(state, buf);

    // `crc32.c` L940: `return crc ^ 0xffffffff`, which is the same complement written without a
    // magic constant.
    !state
}

/// Update a running CRC-32 with `buf` and return the updated check value.
///
/// Port of `crc32_z` (`crc32.c` L626-L941). The two C declarations differ only in the width of the
/// length argument -- `uInt` for `crc32` (`zlib.h` L1848), `z_size_t` for `crc32_z`
/// (`zlib.h` L1866-L1867), whose documentation says merely "Same as `crc32()`, but with a `size_t`
/// length" -- and a Rust slice carries its own length, so that distinction disappears here. This
/// function is an exact synonym for [`crc32`] and forwards to it unchanged.
///
/// Both names are kept because `crates/libz-rs-sys/src/checksum.rs` must export both C symbols
/// from this one implementation, and because the reference sources call both from inside the
/// library: `crc32` over input bytes at `deflate.c` L233, `crc32_z` over the pending buffer while
/// computing a gzip header check at `deflate.c` L976 and L1111. Giving each C name a same-named
/// Rust function keeps that correspondence legible.
///
/// See [`crc32`] for the semantics, the conditioning, the empty-slice contract and the backend
/// dispatch, all of which apply here unchanged.
#[inline]
#[must_use]
pub fn crc32_z(start: u32, buf: &[u8]) -> u32 {
    crc32(start, buf)
}

/// Return the byte-wise CRC-32 table: the check value of every possible eight-bit value.
///
/// Port of `get_crc_table` (`crc32.c` L482-L487), declared among the undocumented functions at
/// `zlib.h` L2034-L2035 as returning `const z_crc_t FAR *`. The comment at `crc32.c` L478-L481
/// gives its two purposes: to let an assembler implementation of `crc32()` share the table, and
/// "to force the generation of the CRC tables in a threaded application".
///
/// The safe core deliberately hands back a reference to the data rather than a raw pointer;
/// `crates/libz-rs-sys/src/checksum.rs` converts this reference into the `const z_crc_t *` the C
/// signature promises, which is the only place a pointer needs to exist. The returned reference is
/// `'static` and stable across calls: it borrows one compiler-materialized copy of
/// [`CRC_TABLE`], not a fresh temporary.
///
/// # No lazy initialization
///
/// The second of those purposes does not apply here, and cannot. Under `DYNAMIC_CRC_TABLE` the C
/// function calls `z_once(&made, make_crc_table)` before returning, and `crc32.c` L12-L17 warns
/// that "there is no mutex or semaphore protection on the static variables used to control the
/// first-use generation of the crc tables", so such a build must call `get_crc_table()` before
/// letting a second thread near `crc32()`. Here the tables are `const` data, so this accessor is
/// always valid, always returns the same address, and is safe to call from any thread at any time
/// -- and bit 13 of `zlibCompileFlags()`, `DYNAMIC_CRC_TABLE`, is honestly reported CLEAR by
/// `crates/libz-rs-sys/src/util.rs` as a result.
#[must_use]
pub fn get_crc_table() -> &'static [u32; 256] {
    // `crc32.c` L486: `return (const z_crc_t FAR *)crc_table;`. Borrowing a `const` array promotes
    // it to an anonymous `'static`, so every call yields one and the same read-only table.
    &CRC_TABLE
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::ptr;

    use super::tables::{N, W};
    use super::{
        crc32, crc32_combine, crc32_combine64, crc32_combine_gen, crc32_combine_gen64,
        crc32_combine_op, crc32_z, get_crc_table, tables, Braid, Crc32Backend, Generic, Selected,
        CRC_BIG_TABLE, CRC_BRAID_BIG_TABLE, CRC_BRAID_TABLE, CRC_TABLE, X2N_TABLE,
    };

    #[cfg(feature = "simd")]
    use super::Simd;

    /// Smallest input for which the braided path is entered, `crc32.c` L640.
    ///
    /// 47 where `W` is 8 and 19 where it is 4. Computed from the sibling module's constants rather
    /// than written out, so the boundary sweep below tracks the real threshold on either target.
    const MIN_BRAID_LEN: usize = N * W + W - 1;

    /// The threshold really is the one `crc32.c` L640 computes, on either word width.
    ///
    /// Checked at compile time rather than inside a test body, because a runtime assertion over
    /// constants is dead weight the compiler folds away -- and because a wrong value here would
    /// silently move the boundary sweep off the boundary it exists to straddle.
    const _: () = assert!(
        MIN_BRAID_LEN == 47 || MIN_BRAID_LEN == 19,
        "crc32.c L640: N * W + W - 1 is 47 where W is 8 and 19 where W is 4"
    );

    /// Length of the deterministic pattern fixture the sweeps run over.
    ///
    /// Large enough for the longest oracle vector below. Miri interprets every operation, so it
    /// gets a much smaller fixture and the vectors that need more are skipped; the two
    /// configurations therefore cover the same code with different amounts of data.
    #[cfg(not(miri))]
    const PATTERN_LEN: usize = 65_536;

    /// Length of the deterministic pattern fixture the sweeps run over, under Miri.
    #[cfg(miri)]
    const PATTERN_LEN: usize = 1_024;

    /// Longest prefix the per-length sweeps take from the pattern fixture.
    #[cfg(not(miri))]
    const SWEEP_MAX_LEN: usize = 512;

    /// Longest prefix the per-length sweeps take from the pattern fixture, under Miri.
    #[cfg(miri)]
    const SWEEP_MAX_LEN: usize = 128;

    /// Length of the repetitive, gzip-shaped fixture.
    ///
    /// A megabyte of eight-byte-period data: long enough that the braided engine runs thousands of
    /// blocks and that every possible remainder size is reached by the irregular chunking below.
    #[cfg(not(miri))]
    const REPETITIVE_LEN: usize = 1 << 20;

    /// Length of the repetitive, gzip-shaped fixture, under Miri.
    #[cfg(miri)]
    const REPETITIVE_LEN: usize = 4_096;

    /// Check value of the repetitive fixture, from the in-tree C implementation.
    #[cfg(not(miri))]
    const REPETITIVE_CHECK: u32 = 0xfecf_fca8;

    /// Check value of the repetitive fixture, from the in-tree C implementation.
    #[cfg(miri)]
    const REPETITIVE_CHECK: u32 = 0x0198_c78f;

    /// Starting values every sweep is run from.
    ///
    /// `0` is the fresh check value; `1`, `0x1234_5678` and `0xdead_beef` are arbitrary running
    /// values a caller may legitimately resume from; `0xffff_ffff` is the all-ones value, which is
    /// what a *pre-conditioned* fresh state looks like and therefore the value that would be
    /// returned unchanged by an implementation that forgot one of the two complements.
    const SEEDS: [u32; 5] = [0, 1, 0x1234_5678, 0xdead_beef, 0xffff_ffff];

    /// Lengths that bracket every path boundary in every backend.
    ///
    /// 0/1/2/3 pick out the empty, single-byte and sub-word cases; 7/8/9 straddle the byte tail's
    /// eight-fold unrolling at `crc32.c` L923-L933; 15..=20 straddle the 32-bit braid threshold of
    /// 19; 39..=48 straddle the 64-bit braid threshold of 47; 54/55/56 straddle the vectorised
    /// backend's slightly larger threshold; and the rest add whole and partial blocks on top.
    const BOUNDARY_LENGTHS: [usize; 33] = [
        0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 18, 19, 20, 31, 32, 39, 40, 46, 47, 48, 54, 55, 56, 63,
        64, 65, 127, 128, 129, 255, 256, 257, 512,
    ];

    /// `(start, len, expected)` triples produced by the in-tree C implementation.
    ///
    /// Every value was read off `crc32.c` compiled with gcc over the fixture [`pattern`] builds, so
    /// these pin this module's *dispatch and conditioning* to the oracle rather than merely to the
    /// backend beside it. The lengths straddle both braid thresholds and the vectorised one, and
    /// the starting values include `0xffff_ffff`, which is where an implementation that dropped or
    /// doubled a complement diverges most visibly.
    const ORACLE_VECTORS: [(u32, usize, u32); 45] = [
        (0x0000_0000, 1, 0x4c66_7a2e),
        (0x0000_0000, 16, 0x0636_a895),
        (0x0000_0000, 19, 0x575d_14d6),
        (0x0000_0000, 46, 0xde6e_9ea4),
        (0x0000_0000, 47, 0x8ab4_cd02),
        (0x0000_0000, 55, 0x7bce_8a99),
        (0x0000_0000, 56, 0x90cb_b96b),
        (0x0000_0000, 1_024, 0x7c32_1b5d),
        (0x0000_0000, 65_536, 0x7bee_c92a),
        (0x0000_0001, 1, 0x3b61_4ab8),
        (0x0000_0001, 16, 0xa85e_3904),
        (0x0000_0001, 19, 0x80bf_948e),
        (0x0000_0001, 46, 0xa1a7_0537),
        (0x0000_0001, 47, 0xe3cd_c667),
        (0x0000_0001, 55, 0x59cd_3989),
        (0x0000_0001, 56, 0x8d5e_aabc),
        (0x0000_0001, 1_024, 0x84a3_ea32),
        (0x0000_0001, 65_536, 0x60ab_d8c4),
        (0x1234_5678, 1, 0x12aa_b776),
        (0x1234_5678, 16, 0x6001_6c3d),
        (0x1234_5678, 19, 0x971d_af2d),
        (0x1234_5678, 46, 0x7104_14c0),
        (0x1234_5678, 47, 0xc0c4_02c9),
        (0x1234_5678, 55, 0x7f36_73b3),
        (0x1234_5678, 56, 0x4b74_8844),
        (0x1234_5678, 1_024, 0xf678_a4a0),
        (0x1234_5678, 65_536, 0x6314_03bc),
        (0xdead_beef, 1, 0x7c0d_2879),
        (0xdead_beef, 16, 0x28d8_1f74),
        (0xdead_beef, 19, 0x134b_596f),
        (0xdead_beef, 46, 0x2ea7_ab0f),
        (0xdead_beef, 47, 0xcb40_7e57),
        (0xdead_beef, 55, 0x9c05_08f0),
        (0xdead_beef, 56, 0xa442_ab15),
        (0xdead_beef, 1_024, 0x17a5_b0ce),
        (0xdead_beef, 65_536, 0xdbea_9ec9),
        (0xffff_ffff, 1, 0x619b_6a5c),
        (0xffff_ffff, 16, 0x1572_1c3f),
        (0xffff_ffff, 19, 0x72aa_2246),
        (0xffff_ffff, 46, 0x22ee_69b0),
        (0xffff_ffff, 47, 0x9092_9988),
        (0xffff_ffff, 55, 0x950a_b727),
        (0xffff_ffff, 56, 0xbcfc_e3dd),
        (0xffff_ffff, 1_024, 0x6c78_4b8c),
        (0xffff_ffff, 65_536, 0x5386_b83e),
    ];

    /// Chunk sizes the incremental test feeds a fixture in.
    ///
    /// Every size from one byte to 64 is used, which straddles both braid thresholds and the
    /// vectorised one from below and from above, and drives the prologue through every alignment.
    const MAX_CHUNK: usize = 64;

    /// Irregular chunk sizes, cycled, for the gzip-shaped scenario.
    ///
    /// Chosen so that consecutive chunks land the read cursor at every offset modulo `W`, and so
    /// that some chunks are shorter than the braid threshold and some are much longer. This is the
    /// call pattern a caller with arbitrary `avail_in` produces.
    const IRREGULAR_CHUNKS: [usize; 9] = [1, 7, 3, 47, 64, 5, 129, 2, 1_000];

    /// Build the deterministic fixture `buf[i] = (i * 31 + 7) & 0xff`.
    ///
    /// No I/O and no randomness, so every expectation above is reproducible on any machine and
    /// against the C implementation. Written as a `u8` accumulator advancing by 31 rather than as a
    /// masked product because `u8` arithmetic *is* arithmetic modulo 256, which is what the mask
    /// means, and this formulation needs no narrowing conversion. 31 is odd, so the sequence has
    /// period 256 and every byte value appears equally often.
    fn pattern(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut byte: u8 = 7;
        for _ in 0..len {
            out.push(byte);
            byte = byte.wrapping_add(31);
        }
        out
    }

    /// Build a repetitive fixture: the eight bytes `zlib-rs ` repeated to `len`.
    ///
    /// Highly compressible, eight-byte-period data, which is the shape a real gzip member's
    /// uncompressed input often has, and which makes every braid see the same word repeatedly --
    /// the case where a mis-ordered braid combination would still look plausible.
    fn repetitive(len: usize) -> Vec<u8> {
        b"zlib-rs ".iter().copied().cycle().take(len).collect()
    }

    /// Take the first `len` bytes of `buf`, asserting the fixture is long enough.
    ///
    /// Goes through `get` rather than a range index so that a fixture built too short fails with a
    /// clear message instead of a bare slice panic.
    fn prefix(buf: &[u8], len: usize) -> &[u8] {
        let taken = buf.get(..len).unwrap_or_default();
        assert_eq!(taken.len(), len, "the fixture is shorter than {len} bytes");
        taken
    }

    /// Run one named backend through this module's conditioning, as [`crc32`] does.
    ///
    /// This is the only way a test can compare backends *as check values* rather than as internal
    /// states: the backends complement nothing, so a test that compared their raw output would
    /// leave the file's central responsibility untested.
    fn through<B: Crc32Backend>(start: u32, buf: &[u8]) -> u32 {
        !B::update(!start, buf)
    }

    /// The published CRC-32 check value: the nine ASCII digits from a fresh check value.
    ///
    /// `0xcbf43926` is the catalogued check value of this CRC-32 (ISO 3309, ITU-T V.42, named at
    /// `doc/rfc1952.txt` L419-L426) and was confirmed against the in-tree C implementation. If
    /// this fails, nothing else in the subsystem is worth reading.
    #[test]
    fn the_canonical_check_value_matches_the_standard() {
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32_z(0, b"123456789"), 0xcbf4_3926);
    }

    /// Vectors read off the C implementation, including the fixtures the C test suite itself uses.
    ///
    /// `"hello, hello!"` is the payload `test/example.c` compresses and `"hello"` is the preset
    /// dictionary it installs. The 256-byte all-`0xa5` buffer is the fill byte
    /// `test/infcover.c`'s instrumented allocator writes into every block, so it is the exact
    /// content a rogue read of uninitialized memory would checksum.
    #[test]
    fn reference_vectors_from_the_c_implementation() {
        assert_eq!(crc32(0, b"hello, hello!"), 0xb39a_dc9b);
        assert_eq!(crc32(0, b"hello"), 0x3610_a686);
        assert_eq!(crc32(0, &[0x00; 256]), 0x0d96_8558);
        assert_eq!(crc32(0, &[0xa5; 256]), 0xa072_5f1f);

        // Accumulating the payload in two calls must reproduce the single-call value, which is the
        // identity `deflate` relies on when a caller splits its input.
        assert_eq!(crc32(crc32(0, b"hello, "), b"hello!"), 0xb39a_dc9b);
    }

    /// Every `(start, len)` triple measured against the in-tree C implementation.
    ///
    /// This is the test that makes the port's agreement with the oracle a property of *this* file:
    /// it exercises the pre-condition, the backend the build selected, and the post-condition
    /// together, at lengths on both sides of every threshold.
    #[test]
    fn oracle_vectors_match_the_c_implementation() {
        let fixture = pattern(PATTERN_LEN);

        for (start, len, expected) in ORACLE_VECTORS {
            if len > PATTERN_LEN {
                continue;
            }
            let buf = prefix(&fixture, len);
            assert_eq!(
                crc32(start, buf),
                expected,
                "crc32({start:#010x}, pattern[..{len}]) disagrees with the C implementation"
            );
        }
    }

    /// An empty slice is an ordinary zero-length update, **not** `Z_NULL`.
    ///
    /// The C entry point answers a null pointer with `0` regardless of its `crc` argument
    /// (`crc32.c` L627-L628, `zlib.h` L1851-L1852). An empty slice cannot carry that meaning: the
    /// two complements cancel and the incoming value is returned unchanged. The two are conflated
    /// only once, and this test is what catches it.
    #[test]
    fn an_empty_slice_is_an_ordinary_update_and_not_z_null() {
        for start in SEEDS {
            assert_eq!(
                crc32(start, &[]),
                start,
                "an empty slice must be the identity on the check value"
            );
            assert_eq!(crc32_z(start, &[]), start);
        }

        // A fresh check value is the one case where the empty-slice answer and the C null-pointer
        // answer coincide, which is why `crc32(0L, Z_NULL, 0)` is a usable idiom at all.
        assert_eq!(crc32(0, &[]), 0);

        // Any other starting value is where they part company: the C null path would answer 0.
        assert_ne!(
            crc32(5, &[]),
            0,
            "an empty slice must not be treated as Z_NULL"
        );
        assert_eq!(crc32(5, &[]), 5);

        // Empty updates are idempotent, wherever they appear in a sequence.
        let once = crc32(0, b"hello");
        let padded = crc32(crc32(crc32(0, &[]), b"hello"), &[]);
        assert_eq!(once, padded);
    }

    /// [`crc32`] and [`crc32_z`] are the same function, at every length and every start.
    ///
    /// The C library declares two entry points only because one takes a `uInt` length and the other
    /// a `z_size_t` (`zlib.h` L1848 and L1866-L1867); a slice erases that distinction, and this
    /// pins the two Rust names together so that a future edit cannot let them drift.
    #[test]
    fn crc32_and_crc32_z_are_the_same_function() {
        let fixture = pattern(PATTERN_LEN);

        for start in SEEDS {
            for len in BOUNDARY_LENGTHS {
                if len > SWEEP_MAX_LEN {
                    continue;
                }
                let buf = prefix(&fixture, len);
                assert_eq!(
                    crc32(start, buf),
                    crc32_z(start, buf),
                    "crc32 and crc32_z disagree at start {start:#010x}, len {len}"
                );
            }
        }

        for text in [
            b"".as_slice(),
            b"hello".as_slice(),
            b"hello, hello!".as_slice(),
            b"123456789".as_slice(),
        ] {
            assert_eq!(crc32(0, text), crc32_z(0, text));
        }
    }

    /// Feeding a fixture in chunks reproduces the single-call value, for every chunk size.
    ///
    /// This is the property `read_buf.rs` depends on: `deflate` accumulates a gzip member's check
    /// value across successive calls whose `avail_in` the caller chooses, so a chunk boundary can
    /// fall anywhere -- mid-prologue, mid-block, mid-tail. It is also the property the two
    /// complements provide, since one call's post-conditioning is undone by the next call's
    /// pre-conditioning.
    #[test]
    fn incremental_updates_agree_with_a_single_call() {
        let fixture = pattern(PATTERN_LEN);
        let buf = prefix(&fixture, SWEEP_MAX_LEN);

        for start in SEEDS {
            let single = crc32(start, buf);

            for step in 1..=MAX_CHUNK {
                let mut running = start;
                for chunk in buf.chunks(step) {
                    running = crc32(running, chunk);
                }
                assert_eq!(
                    running, single,
                    "chunking by {step} changed the check value at start {start:#010x}"
                );
            }

            // A handful of uneven splits, including both degenerate ones.
            for split in [0, 1, 2, MIN_BRAID_LEN - 1, MIN_BRAID_LEN, buf.len()] {
                let (head, tail) = buf.split_at(split);
                assert_eq!(
                    crc32(crc32(start, head), tail),
                    single,
                    "splitting at {split} changed the check value at start {start:#010x}"
                );
            }
        }
    }

    /// Every backend returns the identical check value, through the trait, at every length.
    ///
    /// The backends are interchangeable by contract, and this is where that contract is enforced.
    /// The comparison runs through [`through`], so the conditioning this file owns is part of what
    /// is compared; the lengths span both braid thresholds and the vectorised one in both
    /// directions, which is where a partitioning error would show.
    #[test]
    fn every_backend_agrees_through_the_trait() {
        let fixture = pattern(PATTERN_LEN);

        for start in SEEDS {
            for len in BOUNDARY_LENGTHS {
                if len > SWEEP_MAX_LEN {
                    continue;
                }
                let buf = prefix(&fixture, len);
                let reference = through::<Generic>(start, buf);

                assert_eq!(
                    through::<Braid>(start, buf),
                    reference,
                    "Braid disagrees with Generic at start {start:#010x}, len {len}"
                );

                #[cfg(feature = "simd")]
                assert_eq!(
                    through::<Simd>(start, buf),
                    reference,
                    "Simd disagrees with Generic at start {start:#010x}, len {len}"
                );

                assert_eq!(
                    crc32(start, buf),
                    reference,
                    "dispatch diverged from the byte-at-a-time reference at len {len}"
                );
            }
        }
    }

    /// The selected backend is the one the feature set asks for, and it is one of the three.
    ///
    /// `Selected` is private, so this is the only place its identity can be checked; asserting on
    /// [`Crc32Backend::NAME`] keeps the check readable without making the alias public.
    #[test]
    fn backend_selection_follows_the_simd_feature() {
        assert_eq!(Generic::NAME, "generic");
        assert_eq!(Braid::NAME, "braid");

        #[cfg(feature = "simd")]
        {
            assert_eq!(Simd::NAME, "simd");
            assert_eq!(<Selected as Crc32Backend>::NAME, "simd");
        }

        #[cfg(not(feature = "simd"))]
        assert_eq!(<Selected as Crc32Backend>::NAME, "braid");

        // Whatever ran, `crc32` must be exactly that backend wrapped in the two complements.
        let fixture = pattern(SWEEP_MAX_LEN);
        for start in SEEDS {
            assert_eq!(crc32(start, &fixture), through::<Selected>(start, &fixture));
        }
    }

    /// [`get_crc_table`] returns the generated table, and returns the same one every time.
    ///
    /// The C signature hands back a bare pointer (`crc32.c` L482-L487, `zlib.h` L2035), and the
    /// facade will publish this reference as that pointer, so stability across calls is part of the
    /// contract: a caller may cache what it receives. Since the table is `const` data borrowed from
    /// one promoted `'static`, the address cannot change -- and, unlike the C function under
    /// `DYNAMIC_CRC_TABLE`, this accessor has nothing to initialize on first use.
    #[test]
    fn get_crc_table_returns_the_stable_generated_table() {
        let table = get_crc_table();

        assert_eq!(table.len(), 256, "crc32.h L5: one entry per byte value");
        assert_eq!(table[0], 0x0000_0000, "crc32.h L6");
        assert_eq!(table[1], 0x7707_3096, "crc32.h L6");
        assert_eq!(table[255], 0x2d02_ef8d, "crc32.h L57");
        assert_eq!(*table, CRC_TABLE, "it must be the transcribed table itself");

        assert!(
            ptr::eq(table, get_crc_table()),
            "successive calls must return the same table"
        );
    }

    /// The re-exported surface is reachable from this module and names the generated data.
    ///
    /// `crates/zlib-rs-differential/tests/table_equality.rs` compares these arrays against
    /// `crc32.h` element for element, and `crates/zlib-rs/src/lib.rs` re-exports [`CRC_TABLE`] from
    /// the crate root, so each name has to stay reachable here. Naming all five in one test makes a
    /// removal or a rename a compile error rather than a downstream surprise.
    ///
    /// The assertions are shapes and width-independent anchors: the element-for-element
    /// transcription check and the byte-swap and row-reversal identities between the four tables
    /// belong to `tables` and to the differential suite, and are exhaustive there.
    #[test]
    fn the_re_exported_tables_are_the_generated_tables() {
        assert_eq!(CRC_TABLE.len(), 256, "crc32.h L5: one entry per byte value");
        assert_eq!(CRC_TABLE[0], 0x0000_0000, "crc32.h L6");
        assert_eq!(CRC_TABLE[1], 0x7707_3096, "crc32.h L6");
        assert_eq!(CRC_TABLE[255], 0x2d02_ef8d, "crc32.h L57");
        assert_eq!(
            *get_crc_table(),
            CRC_TABLE,
            "the accessor hands back this table"
        );

        assert_eq!(CRC_BIG_TABLE.len(), 256, "crc32.c L204");
        assert_eq!(CRC_BIG_TABLE[0], 0, "the zero residue swaps to zero");

        assert_eq!(CRC_BRAID_TABLE.len(), W, "crc32.c L205: W rows, not N");
        assert_eq!(CRC_BRAID_BIG_TABLE.len(), W, "crc32.c L206: W rows, not N");
        for row in CRC_BRAID_TABLE {
            assert_eq!(row.len(), 256, "each braid row covers every byte value");
            assert_eq!(row[0], 0);
        }
        for row in CRC_BRAID_BIG_TABLE {
            assert_eq!(row.len(), 256, "each braid row covers every byte value");
            assert_eq!(row[0], 0);
        }

        assert_eq!(X2N_TABLE.len(), 32, "crc32.h L9439-L9446");
        assert_eq!(X2N_TABLE[0], 0x4000_0000, "x^(2^0) == x, reflected");
        assert_eq!(
            X2N_TABLE[5],
            tables::POLY,
            "x^(2^5) == x^32, which reduces to the polynomial itself"
        );
        assert_eq!(X2N_TABLE[31], 0xc4e2_2c3c, "crc32.h L9446");
    }

    /// The combine family splices two check values into the check value of the concatenation.
    ///
    /// Exercised through this module's re-exports rather than through `combine` directly, so that
    /// the barrel's wiring is what is under test. The splits include both degenerate ones, and the
    /// operator form is composed to prove it agrees with the length form.
    #[test]
    fn the_combine_family_splices_check_values() {
        let fixture = pattern(SWEEP_MAX_LEN);
        let whole = crc32(0, &fixture);

        for split in [0, 1, MIN_BRAID_LEN, SWEEP_MAX_LEN / 2, fixture.len()] {
            let (head, tail) = fixture.split_at(split);
            let crc1 = crc32(0, head);
            let crc2 = crc32(0, tail);
            let len2 = i64::try_from(tail.len()).unwrap();

            assert_eq!(
                crc32_combine64(crc1, crc2, len2),
                whole,
                "combining at split {split} did not reproduce the whole check value"
            );
            assert_eq!(crc32_combine(crc1, crc2, len2), whole);

            // `gen` then `op` is the two-step form `zlib.h` L1892-L1894 recommends when one length
            // recurs; it must agree with the one-step form exactly.
            let op = crc32_combine_gen64(len2);
            assert_eq!(crc32_combine_op(crc1, crc2, op), whole);
            assert_eq!(crc32_combine_gen(len2), op);
        }

        // The documented degenerate answers, which are easy to "improve" by accident.
        let crc1 = crc32(0, b"hello, ");
        let crc2 = crc32(0, b"hello!");
        assert_eq!(
            crc32_combine64(crc1, crc2, -1),
            0,
            "zlib.h L1875-L1881: a negative len2 yields zero"
        );
        assert_eq!(
            crc32_combine64(crc1, crc2, 0),
            crc1 ^ crc2,
            "a zero len2 applies the identity operator"
        );
        assert_eq!(crc32_combine_gen64(0), 1 << 31, "the identity operator");
        assert_eq!(
            crc32_combine_op(crc1, crc2, 0),
            0,
            "crc32.c L970-L971: a zero operator yields zero"
        );
    }

    /// A gzip-shaped member: a long repetitive fixture accumulated in irregular chunks.
    ///
    /// This is the whole subsystem doing the job RFC 1952 asks of it
    /// (`doc/rfc1952.txt` L419-L426): a member's uncompressed bytes arrive in whatever sizes the
    /// caller supplies, the check value accumulates across them, and the result must equal both the
    /// single-call value and the C implementation's. The irregular chunk sizes drive the braided
    /// engine's prologue through every alignment and its remainder through every length.
    #[test]
    fn a_gzip_shaped_member_accumulates_in_irregular_chunks() {
        let fixture = repetitive(REPETITIVE_LEN);

        assert_eq!(
            crc32(0, &fixture),
            REPETITIVE_CHECK,
            "the single-call value must match the C implementation"
        );

        let mut running = 0;
        let mut rest: &[u8] = &fixture;
        let mut sizes = IRREGULAR_CHUNKS.iter().copied().cycle();

        while !rest.is_empty() {
            let want = sizes.next().unwrap_or(1).min(rest.len());
            let (chunk, remainder) = rest.split_at(want);
            running = crc32(running, chunk);
            rest = remainder;
        }

        assert_eq!(
            running, REPETITIVE_CHECK,
            "accumulating in irregular chunks changed the check value"
        );

        // And the same member spliced from two independently checksummed halves.
        let (head, tail) = fixture.split_at(REPETITIVE_LEN / 3);
        let len2 = i64::try_from(tail.len()).unwrap();
        assert_eq!(
            crc32_combine64(crc32(0, head), crc32(0, tail), len2),
            REPETITIVE_CHECK
        );
    }

    /// The braid threshold is where the wide path takes over, and it is crossed cleanly.
    ///
    /// `crc32.c` L640 enters the braided body only at `N * W + W - 1` bytes, so one byte either
    /// side of that boundary runs entirely different code and must produce the same answer. The
    /// threshold is output-neutral, and this is the test that says so.
    #[test]
    fn the_braid_threshold_is_output_neutral() {
        let fixture = pattern(PATTERN_LEN);

        for len in [
            MIN_BRAID_LEN - 1,
            MIN_BRAID_LEN,
            MIN_BRAID_LEN + 1,
            2 * MIN_BRAID_LEN,
        ] {
            let buf = prefix(&fixture, len);
            for start in SEEDS {
                assert_eq!(
                    crc32(start, buf),
                    through::<Generic>(start, buf),
                    "the wide path diverged from the byte path at len {len}"
                );
            }
        }
    }
}
