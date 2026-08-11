//! libFuzzer target for the two checksum families, derived from `crc32.c` and `adler32.c`.
//!
//! # What this target does
//!
//! It is the one target in this directory that runs **both implementations in a single
//! process** and compares them. Every assertion below is a differential equality: the
//! Rust port's answer against the in-tree C reference's answer for the same arguments,
//! bit for bit. Nothing here settles for "the result looks like a checksum" -- for a
//! checksum, equality with the reference *is* the specification, because an Adler-32 and
//! a CRC-32 are both part of the zlib and gzip wire formats and a single wrong bit
//! produces a stream the reference implementation rejects.
//!
//! Both sides are reached through the harness crate's safe boundary gates:
//! `zlib_rs_differential::oracle` wraps the `c_`-prefixed declarations of the in-tree C
//! sources, compiled by that crate's `build.rs` and renamed so that `crc32` can mean two
//! things in one binary, and `zlib_rs_differential::port` wraps the facade. So
//! `oracle::crc32` here is exactly `crc32` in `crc32.c` and `port::crc32` is the port's
//! `crc32`, and this file opens no `unsafe` block of its own -- the crate-level
//! `#![forbid(unsafe_code)]` below is what holds that, and the two gate files are where
//! the FFI obligations are discharged and documented.
//!
//! Six families of assertion are made:
//!
//! - **One-shot equality.** All four entry points -- `adler32`, `adler32_z`, `crc32`,
//!   `crc32_z` -- over the fuzzer's payload, against their oracle counterparts. The `_z`
//!   and unsuffixed forms are two ABI faces over one algorithm, so they must also agree
//!   with *each other*, and the safe core reached through the `rust-api` surface must
//!   agree with the facade that wraps it, which is what exercises the `uLong`/`uInt`/
//!   `z_size_t` conversions the boundary layer performs.
//! - **The null-buffer contract.** `adler32(0, Z_NULL, 0)` returning `1` and
//!   `crc32(0, Z_NULL, 0)` returning `0` is the documented way to obtain a seed
//!   (`zlib.h` L1812-L1814, L1851-L1852), so a null pointer is *contract-legal* input
//!   here rather than misuse. It is also a genuine trap for a Rust port, because
//!   `slice::from_raw_parts(null, 0)` is undefined behaviour, and this is the target
//!   that catches it under AddressSanitizer.
//! - **Streaming equivalence.** Feeding a payload in pieces must yield exactly what
//!   feeding it in one call yields, for both implementations, including one byte at a
//!   time. A fixed-length synthetic buffer longer than Adler-32's `NMAX` (`adler32.c`
//!   L11) is checksummed on every invocation so that the modulo schedule's block
//!   boundary -- the likeliest place for an Adler-32 port to diverge -- is crossed every
//!   time rather than only when the fuzzer happens to produce a long enough input.
//! - **Combination.** `adler32_combine`, `crc32_combine`, their `*64` counterparts, and
//!   the `crc32_combine_gen`/`crc32_combine_op` operator pair, each against the oracle.
//!   Combining the checksums of two halves must reproduce the checksum of the whole, and
//!   one generated operator reused across several check-value pairs must answer exactly
//!   what `crc32_combine` answers each time -- which is the documented reason
//!   `crc32_combine_gen` exists at all (`zlib.h` L1884-L1894).
//! - **The three sentinels.** A negative `len2` makes the Adler-32 combine return
//!   `0xffffffff` (`adler32.c` L138-L140) and the CRC-32 generator return `0`
//!   (`crc32.c` L955-L956), and a zero operator makes `crc32_combine_op` return `0`
//!   (`crc32.c` L970-L971). All three are implementation-defined answers rather than
//!   consequences of the format, so each is asserted against its literal value *and*
//!   against the oracle's return, so that a misremembered constant cannot pass.
//! - **Backend interchangeability.** `Crc32Backend` and `Adler32Backend` exist so that
//!   the scalar and vectorized engines are interchangeable by construction. This target
//!   is one of the consumers those traits were made public for, and it holds them to it.
//!
//! # Why this target is the executable form of the SIMD requirement
//!
//! The `simd` feature may change how long a checksum takes and must never change what it
//! returns. That claim is worth nothing unasserted, so this target is one of the places it
//! is checked: build it with `--features simd` and every equality above still has to hold
//! against a scalar C oracle that knows nothing about vectors. See
//! [`check_simd_neutrality`] for the direct backend-against-backend comparison that runs
//! on top of that.
//!
//! **Which of those checks CI actually performs, precisely.** The `fuzz` job in
//! `.github/workflows/rust.yml` runs this target TWICE. Its matrix is six rows over five
//! fuzz binaries: one row each for `fuzz_deflate`, `fuzz_inflate`, `fuzz_inflate_back` and
//! `fuzz_gz_roundtrip`, then `fuzz_checksum` with the default feature set, then
//! `fuzz_checksum` again with `--features simd`. Both checksum rows get the same 300-second
//! budget and upload artifacts under distinct slugs (`fuzz_checksum` and
//! `fuzz_checksum_simd`), so the neutrality assertions above are compiled IN and exercised
//! on every push rather than only when somebody remembers to pass the feature. This comment
//! said the opposite for a while -- that the job ran scalar only and that a green `fuzz` job
//! was no evidence about SIMD -- and the sixth row is why that was wrong.
//!
//! The same command by hand, when you want it locally:
//!
//! ```text
//! cd fuzz
//! cargo +nightly fuzz run fuzz_checksum --features simd -- -max_total_time=300
//! ```
//!
//! Neutrality is additionally enforced from a second, independent direction: the
//! `differential` job runs `cargo test -p zlib-rs-differential --release --features simd`,
//! and because the byte-identity cells compare the running check value as well as the
//! emitted bytes, a vectorized checksum that returned a different answer would fail there
//! too. So this target is the arbitrary-input form of the check and the differential suite
//! is the fixed-corpus form; both run automatically, and neither is a substitute for the
//! other.
//!
//! # What this target deliberately does NOT do
//!
//! **It asserts nothing about compressed bytes.** Byte-identity of the DEFLATE stream is
//! owned by `crates/zlib-rs-differential/tests/byte_identical.rs`. It also does not
//! duplicate the exhaustive table comparison in
//! `crates/zlib-rs-differential/tests/table_equality.rs`: the `get_crc_table` check here
//! is a spot check behind a fuzzer-chosen boolean, because constant work repeated on
//! every invocation would only spend the time budget on an answer that cannot change.
//!
//! # Running it
//!
//! ```text
//! cd fuzz
//! cargo +nightly fuzz run fuzz_checksum -- -max_total_time=300
//! cargo +nightly fuzz run fuzz_checksum --features simd -- -max_total_time=300
//! ```
//!
//! `+nightly` is required because the repository-root `rust-toolchain.toml` pins stable
//! and cargo-fuzz's sanitizer instrumentation is nightly-only. The gate is zero crashes,
//! zero hangs, zero timeouts and zero out-of-memory reports over at least 300 seconds,
//! matching `fuzz-seconds: 300` in `.github/workflows/fuzz.yml`.
//!
//! Two limits on what that instrumentation reaches, stated so the coverage is not read as
//! wider than it is. AddressSanitizer and coverage feedback apply to the Rust crates, so
//! they cover the checksum entry points this target drives and not the boundary layer as a
//! whole. And the oracle archive is compiled WITHOUT instrumentation, so a memory error
//! inside the C reference is not detected here and libFuzzer's coverage feedback is not
//! guided by the oracle's branches. That trade is accepted rather than free: the oracle is
//! a comparison value rather than the subject, the C implementation is fuzzed separately
//! and continuously by OSS-Fuzz, and a disagreement between the two answers is still
//! reported because that assertion lives on the Rust side.
//!
//! # Self-containment
//!
//! cargo-fuzz compiles every file in `fuzz_targets/` as an independent binary, so a
//! shared helper module in this directory would simply never be compiled. Everything this
//! target needs is therefore either in this file or in `zlib-rs-differential`, which is a
//! real dependency and does get compiled; do not try to factor any of it into a sibling
//! of this file.

#![no_main]
#![forbid(unsafe_code)]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use libz_rs_sys::{uInt, uLong, z_off64_t, z_off_t, z_size_t};
use zlib_rs::adler32::{ADLER32_INITIAL_VALUE, NMAX};
use zlib_rs::crc32::{Braid, Crc32Backend, Generic, CRC_TABLE};
use zlib_rs_differential::oracle::oracle_crc_table;
use zlib_rs_differential::{oracle, port};

// The vectorized engines are named only by the neutrality check, so their imports are
// gated exactly as it is. Without the feature the types do not exist at all -- `zlib-rs`
// declares `mod simd` under the same `cfg` -- so an ungated import would not merely warn,
// it would fail to resolve.
#[cfg(feature = "simd")]
use zlib_rs::adler32::{Adler32Backend, Adler32Generic, Adler32Simd};
#[cfg(feature = "simd")]
use zlib_rs::crc32::Simd;

// ---------------------------------------------------------------------------
//  Termination bounds
// ---------------------------------------------------------------------------
//
// Checksumming is linear in its input and allocates nothing, so this target cannot hang in
// the way a compressor can. What it *can* do is spend the shared 300-second budget
// unproductively, and the executions-per-second figure is the whole point of a fuzzer: a
// checksum defect is found by covering the reduction schedule's branch points -- the
// single-byte path, the short-length path, the block-at-a-time path, the braid threshold --
// and every one of those is reachable within a few kilobytes. Longer inputs buy nothing and
// cost throughput linearly. These constants are therefore bounds, not tuning knobs; raising
// them trades the gate away for coverage that is already had.
//
// The bounds below are set from where the cost actually is, and two properties govern that.
// Both are worth recording, because neither is what a byte count would suggest:
//
//   * Per-call cost dominates, not bytes hashed. Each piece of a streaming pass is a call
//     across the C ABI carrying the facade's panic guard, and under cargo-fuzz also
//     AddressSanitizer and coverage instrumentation. So the iteration bounds are the lever
//     that matters, ahead of the payload length.
//   * The `NMAX` probe is the single most expensive thing here, and it cannot be shortened --
//     its length is fixed by `NMAX` itself. What is bounded instead is the number of times it
//     is traversed.
//
// The execution rate this target sustains is therefore lower than a checksum fuzzer's would
// otherwise be, and that is the price of the coverage rather than something left on the table:
// a byte-for-byte differential against a second implementation costs two computations of
// everything it checks, and the `NMAX` crossing costs five and a half kilobytes of that on
// every invocation by design. Re-measure on the host in question before changing a bound;
// raising one trades the no-hang half of the gate away.

/// Longest payload handed to either checksum, in bytes.
///
/// Half a kibibyte reaches every length-dependent branch in both reference implementations:
/// `len == 1` (`adler32.c` L70), `len < 16` (L85), the `NMAX` block loop (L97) via the probe
/// below, and the braided CRC-32 path, which `crc32.c` L640 enters at `N * W + W - 1` -- 47
/// bytes where a 64-bit word type is available, and which then runs about a dozen full
/// braid rounds at this length. Nothing in either algorithm branches on a length above this,
/// so a longer payload costs throughput linearly and buys no coverage. It is written as a bound
/// rather than as a preference for that reason: raising it buys nothing and costs executions.
const MAX_PAYLOAD_LEN: usize = 512;

/// Longest slice handed to the backend-interchangeability comparison, in bytes.
///
/// Bounded separately because that comparison runs the *byte-at-a-time* engine, which is the
/// most expensive path per byte in the whole file, and because its coverage does not improve
/// with length. It does, however, have a floor rather than only a ceiling: the braided engine
/// hands anything below `N * W + W - 1` straight back to the byte-at-a-time one
/// (`crc32.c` L640), so a slice shorter than 47 bytes would make the two engines the same
/// code and the comparison vacuous. This length clears that threshold several times over
/// whenever the fuzzer supplies enough bytes to reach it.
const MAX_BACKEND_LEN: usize = 256;

/// Length of the synthetic buffer that crosses Adler-32's `NMAX` block boundary.
///
/// Fixed rather than fuzzer-derived, and that is deliberate. `NMAX` is 5552, while
/// libFuzzer's default `-max_len` is 4096, so an input long enough to cross the boundary
/// is not merely rare under the default configuration -- it is unreachable. Leaving this
/// to chance would leave `adler32.c` L97-L106, the one place the sums are reduced modulo
/// `BASE` mid-buffer, untested by this target. The 33-byte tail past the boundary is what
/// then drives the remaining-bytes epilogue at L108-L121: two whole 16-byte groups and a
/// single-byte remainder.
const NMAX_PROBE_LEN: usize = NMAX + 33;

/// Most pieces any one streaming pass may split its input into.
///
/// [`select_chunk`] raises the fuzzer's chunk size until this holds, so a pathological choice
/// of 1 against a full payload cannot turn a linear pass into hundreds of FFI calls. The
/// byte-at-a-time case is not lost to that floor: it is exercised deterministically over
/// [`BYTEWISE_PREFIX_LEN`] instead.
///
/// This is the bound that matters most for throughput, and the reason is worth recording
/// because it is not obvious from the byte counts. Each piece is a call across the C ABI, and
/// each such call carries the facade's panic guard and, under cargo-fuzz, sanitizer and
/// coverage instrumentation -- so per-call overhead, not bytes hashed, is what dominates a
/// streaming pass. Four passes at this bound cost a little over a hundred calls, which is
/// what keeps the accumulation coverage while leaving the time budget to the fuzzer.
const MAX_CHUNK_ITERATIONS: usize = 32;

/// Length of the prefix fed one byte at a time, in bytes.
///
/// The single-byte path (`adler32.c` L70) is a distinct branch with its own reduction, so it
/// must be covered on every invocation rather than when the fuzzer happens to pick a chunk
/// size of one. It is a fixed branch with no length dependence, so a short prefix covers it
/// exactly as well as a long one while costing a fraction of the calls. Bounded so the pass
/// costs at most [`MAX_CHUNK_ITERATIONS`] calls.
const BYTEWISE_PREFIX_LEN: usize = 32;

/// Number of `z_crc_t` entries `get_crc_table` publishes -- `crc32.h` L5.
///
/// Not negotiable and not inferred: `zlib.h` L2034-L2035 declares the return as a bare
/// `const z_crc_t FAR *` with no length, so the length is contract knowledge that has to
/// be written down somewhere. `crc32.c` L482-L487 hands back `crc_table`, which has one
/// entry per byte value.
const CRC_TABLE_LEN: usize = 256;

/// Both bounded lengths fit a `uInt` with room to spare.
///
/// This is what licenses the `usize` to [`uInt`] narrowing in [`as_uint`]: `uInt` is
/// `unsigned int` and therefore at least 16 bits wide on any conforming target, so a
/// bound below 65536 cannot lose a bit anywhere this target passes a length to the
/// unsuffixed entry points.
const _: () = assert!(MAX_PAYLOAD_LEN < 1 << 16);
const _: () = assert!(NMAX_PROBE_LEN < 1 << 16);

/// The byte-at-a-time pass must respect the iteration bound by construction.
const _: () = assert!(BYTEWISE_PREFIX_LEN <= MAX_CHUNK_ITERATIONS);

/// The backend comparison must be able to clear the braided engine's own threshold.
///
/// `crc32.c` L640 enters the braided path at `N * W + W - 1`, which is 47 bytes for the
/// eight-byte word this target's host uses and 23 for a four-byte word. A backend slice
/// shorter than that would make both engines run the same code.
const _: () = assert!(MAX_BACKEND_LEN >= 47);
const _: () = assert!(MAX_BACKEND_LEN <= MAX_PAYLOAD_LEN);

/// The probe has to actually cross the boundary it exists to cross.
const _: () = assert!(NMAX_PROBE_LEN > NMAX);

/// `NMAX` must still be what `adler32.c` L11 defines it as.
///
/// The probe length is derived from the ported constant rather than from a literal, so
/// this pins the ported constant to the reference. A silent change to it would otherwise
/// move the probe without moving the boundary.
const _: () = assert!(NMAX == 5552, "adler32.c L11: #define NMAX 5552");

// ---------------------------------------------------------------------------
//  Contract constants read from the C sources
// ---------------------------------------------------------------------------
//
// Each of these is an implementation-defined answer transcribed from the reference, with
// the line it comes from. None is a `Z_*` constant or an ABI type alias -- those are
// consumed from `libz_rs_sys`, never redeclared -- and none is asserted on its own:
// every use below also compares the oracle's return, so a value misremembered here fails
// against the C implementation instead of quietly redefining the contract.

/// The Adler-32 seed, and the answer to every null-buffer call -- `adler32.c` L81-L82.
const ADLER32_SEED: uLong = 1;

/// The seed above must be the value the safe core also starts from.
///
/// The two are written in different integer domains -- `uLong` at the ABI edge, `u32` in
/// the core -- so they are pinned to the same literal rather than to each other, which
/// keeps the assertion free of a conversion that would be an identity on one target and a
/// widening on another. RFC 1950 L327 is where the `1` ultimately comes from.
const _: () = assert!(
    ADLER32_SEED == 1 && ADLER32_INITIAL_VALUE == 1,
    "adler32.c L82 returns 1L and the core starts from the same value"
);

/// The CRC-32 seed, and the answer to every null-buffer call -- `crc32.c` L627-L628.
const CRC32_SEED: uLong = 0;

/// What the Adler-32 combines answer for a negative `len2` -- `adler32.c` L138-L140.
///
/// The in-source comment is "for negative len, return invalid adler32 as a clue for
/// debugging", and the value carries that information: a genuine Adler-32 has both halves
/// below `BASE`, so `0xffff` in either half is unreachable. It must not be confused with
/// the CRC-32 answer, which is `0`.
const ADLER32_COMBINE_INVALID: uLong = 0xffff_ffff;

/// What the CRC-32 operator generators answer for a negative `len2` -- `crc32.c` L955-L956.
const CRC32_COMBINE_GEN_INVALID: uLong = 0;

/// What `crc32_combine_op` answers for a zero operator -- `crc32.c` L970-L971.
///
/// The guard is not cosmetic: `multmodp` (`crc32.c` L158-L161) documents that "for speed,
/// this requires that a not be zero", so the early return is what keeps a zero operator
/// out of a loop that cannot handle it.
const CRC32_COMBINE_OP_ZERO: uLong = 0;

/// The operator a zero `len2` generates -- `crc32.c` L184-L196.
///
/// `x2nmodp` starts from `p = (uLong)1 << 31`, commented "x^0 == 1", and its `while (n)`
/// loop does not run at all for `n == 0`. So the operator for an empty second sequence is
/// exactly that, `crc32_combine_op` reduces to `crc1 ^ crc2`, and -- because the check
/// value of an empty sequence is `0` -- combining with an empty tail is the identity.
const CRC32_COMBINE_GEN_ZERO_LEN: uLong = 0x8000_0000;

/// The `uLong` bits above the low 32, which every reference entry point masks away.
///
/// `uLong` is `unsigned long`: 64 bits on LP64 and 32 on LLP64 Windows. Two shifts of 16
/// give the right answer on both without a `cfg` and without a shift that could overflow
/// the narrower width -- `0xffff_ffff << 16 << 16` is `0`, which is correct, because a
/// 32-bit `uLong` has no bits above the low 32 to set.
///
/// Setting these bits in a seed is a real ABI test rather than a curiosity. `adler32_z`
/// reads only `(adler >> 16) & 0xffff` and `adler & 0xffff` (`adler32.c` L66-L67) and
/// `crc32_z` only `(~crc) & 0xffffffff` (`crc32.c` L635), so the reference ignores them;
/// the port must reach the same answer through its own narrowing conversion.
const HIGH_BITS: uLong = uLong::MAX << 16 << 16;

/// Null-buffer lengths both implementations answer identically.
///
/// `1` is deliberately absent, and the reason is a real hazard rather than caution.
/// `adler32_z` tests `len == 1` at `adler32.c` L70 -- the comment at L80 calls the
/// ordering a "deferred check for len == 1 speed" -- *before* it tests `buf == Z_NULL` at
/// L81, so `c_adler32(a, NULL, 1)` dereferences a null pointer in C. See
/// [`check_null_buffer`], which covers that length against the port alone and says why.
const NULL_LENGTHS: [uInt; 4] = [0, 2, 77, 99];

// ---------------------------------------------------------------------------
//  Structured input
// ---------------------------------------------------------------------------

/// One fuzzer-chosen checksum session.
///
/// `payload` is last so that the derived `arbitrary_take_rest` -- which is what
/// `fuzz_target!` calls for a typed input -- hands it every remaining byte verbatim. It is
/// a borrowed slice rather than a `Vec<u8>` for the two reasons the sibling targets record:
/// a `Vec<u8>` is drawn through `ArbitraryTakeRestIter`, which spends a continuation byte
/// per element and so makes the length geometric with p = 1/2, and working around that
/// with `#[arbitrary(with = ...)]` reports `size_of::<Vec<u8>>()` as the field's
/// `size_hint` lower bound, which makes `fuzz_target!` reject every short input outright.
/// A `&[u8]` has neither problem: its lower bound is zero and its take-rest is the tail.
#[derive(Arbitrary, Debug)]
struct ChecksumInput<'a> {
    /// Selector narrowed by [`select_chunk`] into the bytes fed per streaming call.
    chunk: u8,
    /// Selector reduced into `0..=payload.len()`, where the payload is cut in two for the
    /// combine assertions, and independently into the probe for its resumption check.
    split: u16,
    /// Starting value for every Adler-32 assertion.
    ///
    /// Unconstrained on purpose, but note carefully what that does and does not buy.
    ///
    /// The reference reduces a starting value whose halves are not below `BASE` differently
    /// in each of its three code paths (`adler32.c` L72-L76, L90-L92, L104-L105), and the
    /// port has to reproduce each of those reductions exactly -- which is what
    /// [`check_adler_one_shot`] asserts against `adler32.c` on this raw value, and a seed
    /// drawn only from the well-formed range would never reach.
    ///
    /// Those paths do NOT, however, agree with one another outside the well-formed range,
    /// and the reference does not claim they do: a single conditional subtraction and a
    /// modulo give different answers once a half is already >= `BASE`. So the assertions
    /// that compare *chunkings against each other* normalise this value first --
    /// see [`normalised_adler_seed`] -- because otherwise they would report the port as
    /// broken for agreeing with the reference.
    adler_seed: u32,
    /// Starting value for every CRC-32 assertion.
    crc_seed: u32,
    /// Synthetic first check value for the operator and sentinel assertions.
    ///
    /// Synthetic rather than computed, because `crc32_combine_op` masks both check values
    /// with `0xffffffff` (`crc32.c` L972) and is otherwise indifferent to where they came
    /// from -- so drawing them freely covers the operator's own arithmetic far better than
    /// feeding it only values a real payload could produce.
    synthetic_crc1: u32,
    /// Synthetic second check value for the operator and sentinel assertions.
    synthetic_crc2: u32,
    /// Magnitude for the negative lengths in [`check_adler_sentinel`] and
    /// [`check_crc_sentinels`], widened by [`negative_offsets`].
    ///
    /// A `u16` rather than an `i64` so the sign cannot be lost: `z_off_t` is a platform
    /// type and may be narrower than 64 bits, and casting a large negative `i64` into a
    /// narrower signed type can land on zero or a positive value -- which would silently
    /// stop testing the sentinel at all.
    negative_magnitude: u16,
    /// Whether to spot-check `get_crc_table` on this invocation.
    ///
    /// Guarded because the answer is `const` data and therefore identical every time:
    /// running it unconditionally would spend roughly half of a very cheap invocation on
    /// work whose result cannot change.
    check_tables: bool,
    /// Whether to add the resumption pass over the `NMAX` probe on this invocation.
    ///
    /// The probe's *differential* comparison is unconditional -- crossing the boundary on
    /// every invocation is the whole point of it -- but the resumption pass is a second
    /// traversal of five and a half kilobytes, and measurement showed the probe to be by far
    /// the most expensive thing this target does. Sampling the second pass halves that cost
    /// while still running the check hundreds of thousands of times over a 300-second gate.
    probe_resumption: bool,
    /// Bytes to checksum, borrowed from the fuzzer's own buffer and truncated to
    /// [`MAX_PAYLOAD_LEN`] in [`drive`].
    payload: &'a [u8],
}

// ---------------------------------------------------------------------------
//  Selectors and conversions
// ---------------------------------------------------------------------------

/// Narrows a selector to the number of bytes fed per streaming call, at least one.
///
/// The floor is what bounds the work: a chunk of 1 against a full payload would make
/// [`MAX_PAYLOAD_LEN`] FFI calls per pass and four passes per invocation, so the requested
/// size is raised until the pass fits [`MAX_CHUNK_ITERATIONS`] pieces. For a payload at or
/// below that many bytes the floor is 1 and the selector reaches every size, including the
/// degenerate one; for longer payloads the byte-at-a-time case is covered separately over
/// [`BYTEWISE_PREFIX_LEN`].
fn select_chunk(selector: u8, len: usize) -> usize {
    let floor = len.div_ceil(MAX_CHUNK_ITERATIONS).max(1);
    usize::from(selector).max(1).max(floor)
}

/// Reduces a selector to an index in `0..=len`, so both halves of a split may be empty.
///
/// `len + 1` can never be zero, so the remainder is always defined -- which is the point
/// of normalising here rather than at each use: nothing downstream needs a bounds check
/// and nothing downstream can panic on an index.
fn select_split(selector: u16, len: usize) -> usize {
    usize::from(selector) % (len + 1)
}

/// Builds the negative lengths the sentinel assertions need, in both offset widths.
///
/// The magnitude comes in as a `u16` and is widened with `From`, so the result is
/// representable in a `z_off_t` of any width a target might choose and the sign is
/// preserved by construction rather than by luck. The range is `-65536..=-1`;
/// [`check_adler_sentinel`] and [`check_crc_sentinels`] additionally cover `-1` and the
/// minimum of each type, which is the value a naive bounds check is most likely to
/// mishandle.
fn negative_offsets(magnitude: u16) -> (z_off_t, z_off64_t) {
    (
        -(z_off_t::from(magnitude) + 1),
        -(z_off64_t::from(magnitude) + 1),
    )
}

/// Widens a core check value into the C API's `uLong` domain.
///
/// `From` rather than `as`: this conversion is lossless on every target -- `uLong` is
/// `unsigned long`, which is at least 32 bits -- and spelling it as a cast would hide that
/// fact from the reader and from clippy alike.
fn widen(value: u32) -> uLong {
    uLong::from(value)
}

/// Narrows a `uLong` check value to the 32 bits a check value actually occupies.
///
/// Information-preserving for every value this target passes onward, and that is a
/// property of the reference rather than an assumption: `adler32_z` reads only the low two
/// 16-bit halves, `crc32_z` masks with `0xffffffff`, `adler32_combine_` masks with
/// `0xffff` throughout, and `crc32_combine_op` masks both check values outright. No
/// reference entry point examines a bit at or above position 32.
fn narrow(value: uLong) -> u32 {
    value as u32
}

// ---------------------------------------------------------------------------
//  The eight buffer entry points, behind the harness's boundary gates
// ---------------------------------------------------------------------------
//
// Eight one-line forwards into `zlib_rs_differential::{port, oracle}`, which is where this
// harness keeps the whole of its FFI. Each gate turns the `&[u8]` into the
// `(*const Bytef, len)` pair the C signature takes and discharges the obligation that goes
// with it -- a live shared borrow is non-null, `u8`-aligned and readable for exactly
// `data.len()` bytes, and neither implementation writes through the pointer or retains it
// past the call. Naming the gate eight times here rather than at each of the several
// hundred call sites below is what lets every phase function be ordinary safe Rust.
//
// Two properties of the gates are load-bearing for this target specifically.
//
// The slice's own pointer is passed verbatim, empty slice included. A non-null zero-length
// buffer and `Z_NULL` are two different documented inputs with two different answers --
// `adler32.c` L81-L82 answers `1` only for the null pointer, so a zero-length update
// through a live pointer returns the seed it was handed -- and this target asserts both.
// The null face has its own four gates per side, used by [`check_null_buffer`] and
// [`check_null_at_length_one`].
//
// Length narrowing into `uInt` is the gates' as well, and it saturates rather than
// truncates. This target never reaches either behaviour: every buffer it hands over is
// bounded by [`MAX_PAYLOAD_LEN`] or by [`NMAX_PROBE_LEN`], and the compile-time assertions
// above hold both below 65536.

/// The port's `adler32` -- the `uInt`-length face, `zlib.h` L1809.
fn rust_adler32(seed: uLong, data: &[u8]) -> uLong {
    port::adler32(seed, data)
}

/// The port's `adler32_z` -- the `z_size_t`-length face, `zlib.h` L1829.
fn rust_adler32_z(seed: uLong, data: &[u8]) -> uLong {
    port::adler32_z(seed, data)
}

/// The port's `crc32` -- the `uInt`-length face, `zlib.h` L1848.
fn rust_crc32(seed: uLong, data: &[u8]) -> uLong {
    port::crc32(seed, data)
}

/// The port's `crc32_z` -- the `z_size_t`-length face, `zlib.h` L1866.
fn rust_crc32_z(seed: uLong, data: &[u8]) -> uLong {
    port::crc32_z(seed, data)
}

/// The reference's `adler32` -- `adler32.c` L128-L130, renamed by the oracle's `build.rs`.
fn oracle_adler32(seed: uLong, data: &[u8]) -> uLong {
    oracle::adler32(seed, data)
}

/// The reference's `adler32_z` -- `adler32.c` L61-L125.
fn oracle_adler32_z(seed: uLong, data: &[u8]) -> uLong {
    oracle::adler32_z(seed, data)
}

/// The reference's `crc32` -- `crc32.c` L946-L951.
fn oracle_crc32(seed: uLong, data: &[u8]) -> uLong {
    oracle::crc32(seed, data)
}

/// The reference's `crc32_z` -- `crc32.c` L626-L941.
fn oracle_crc32_z(seed: uLong, data: &[u8]) -> uLong {
    oracle::crc32_z(seed, data)
}

/// One checksum entry point, as this target drives it.
///
/// The four wrappers above all have this shape, which is what lets [`stream`] be written
/// once instead of four times -- and, more usefully, what lets the same streaming
/// assertion be applied to the port and to the reference by passing a different function.
type Checksum = fn(uLong, &[u8]) -> uLong;

/// Folds `data` into `seed` in `chunk`-sized pieces through `update`.
///
/// The documented accumulation property of both families is that any chunking of an input
/// yields the value of one call over the whole of it (`zlib.h` L1819-L1826, L1856-L1863),
/// so the result must equal `update(seed, data)` for every `chunk`. That is the assertion
/// [`check_streaming`] makes; this function only produces the left-hand side.
///
/// An empty `data` is deliberately routed through one empty call rather than through zero
/// calls, and the difference is observable: a zero-length update returns the incoming value
/// with its halves *normalised*, so for a seed whose halves are not below `BASE` a
/// no-op would not equal the one-shot answer and the assertion would fail on a correct
/// implementation.
///
/// `chunk` must be non-zero; [`select_chunk`] guarantees it, which is why this function
/// carries no check of its own and cannot panic.
fn stream(update: Checksum, seed: uLong, data: &[u8], chunk: usize) -> uLong {
    if data.is_empty() {
        return update(seed, data);
    }

    let mut running = seed;
    for piece in data.chunks(chunk) {
        running = update(running, piece);
    }
    running
}

// ---------------------------------------------------------------------------
//  One-shot equality
// ---------------------------------------------------------------------------

/// Holds all three Adler-32 surfaces to one answer for one buffer.
///
/// Three surfaces, because a divergence between any two of them is a real defect:
///
/// * the port against the reference, which is the differential assertion proper;
/// * `adler32` against `adler32_z`, which differ *only* in the width of their length
///   argument (`uInt` at `zlib.h` L1809, `z_size_t` at L1829) and are two ABI faces over
///   one algorithm, so they cannot legitimately disagree for a length that fits both;
/// * the facade against the safe core it wraps, which is what exercises the `uLong`
///   narrowing and widening the boundary layer performs -- a bug there would be invisible
///   to a comparison that only ever went through the facade.
fn check_adler_one_shot(data: &[u8], seed: uLong) {
    let len = data.len();
    let rust = rust_adler32(seed, data);
    let reference = oracle_adler32(seed, data);

    assert_eq!(
        rust, reference,
        "adler32 diverged from adler32.c: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        rust_adler32_z(seed, data),
        "adler32 and adler32_z disagree in the port: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        reference,
        oracle_adler32_z(seed, data),
        "adler32 and adler32_z disagree in the reference: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        widen(zlib_rs::adler32::adler32(narrow(seed), data)),
        "the adler32 facade and the safe core disagree: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        widen(zlib_rs::adler32::adler32_z(narrow(seed), data)),
        "the adler32_z facade and the safe core disagree: seed {seed:#x}, len {len}"
    );
}

/// Holds all three CRC-32 surfaces to one answer for one buffer.
///
/// The same three comparisons as [`check_adler_one_shot`], for the same reasons; the
/// unsuffixed and `_z` forms here are `zlib.h` L1848 and L1866.
fn check_crc_one_shot(data: &[u8], seed: uLong) {
    let len = data.len();
    let rust = rust_crc32(seed, data);
    let reference = oracle_crc32(seed, data);

    assert_eq!(
        rust, reference,
        "crc32 diverged from crc32.c: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        rust_crc32_z(seed, data),
        "crc32 and crc32_z disagree in the port: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        reference,
        oracle_crc32_z(seed, data),
        "crc32 and crc32_z disagree in the reference: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        widen(zlib_rs::crc32::crc32(narrow(seed), data)),
        "the crc32 facade and the safe core disagree: seed {seed:#x}, len {len}"
    );
    assert_eq!(
        rust,
        widen(zlib_rs::crc32::crc32_z(narrow(seed), data)),
        "the crc32_z facade and the safe core disagree: seed {seed:#x}, len {len}"
    );
}

/// Repeats the one-shot comparison with the `uLong` bits above the low 32 set.
///
/// Both reference entry points discard those bits -- `adler32.c` L66-L67 keeps only two
/// 16-bit halves and `crc32.c` L635 masks with `0xffffffff` -- and the port has to reach
/// the same answer through a narrowing conversion instead. On a target where `uLong` is
/// only 32 bits wide [`HIGH_BITS`] is zero and this repeats the plain comparison, which
/// costs a few microseconds and keeps the assertion portable.
///
/// Run over a bounded prefix rather than the whole payload, because the interesting part
/// is the seed rather than the length, and the length is already swept elsewhere.
fn check_wide_seed(data: &[u8], adler_seed: uLong, crc_seed: uLong) {
    check_adler_one_shot(data, adler_seed | HIGH_BITS);
    check_crc_one_shot(data, crc_seed | HIGH_BITS);
}

// ---------------------------------------------------------------------------
//  The null-buffer contract
// ---------------------------------------------------------------------------

/// Holds both families to the seed for a null buffer, at every length C defines.
///
/// This is contract-legal input, not misuse: `zlib.h` L1812-L1814 documents "If buf is
/// Z_NULL, this function returns the required initial value for the checksum" and L1851-L1852
/// says the same for the CRC-32, so `adler32(0L, Z_NULL, 0)` is the *canonical* way to
/// obtain a seed and appears verbatim in that header's own usage example.
///
/// Two properties are asserted, and the second is the one that matters most for a Rust
/// port. The answer is the family's seed **unconditionally** -- the incoming value is
/// discarded and the length is ignored -- so a seeded call must still return `1` or `0`
/// rather than the seed it was handed. And reaching that answer must not construct a slice
/// from a null pointer on the way: `slice::from_raw_parts(null, 0)` is undefined behaviour
/// even for a zero length, so this is the assertion that catches it, under
/// AddressSanitizer here and under Miri in the facade's own test suite.
fn check_null_buffer(adler_seed: uLong, crc_seed: uLong) {
    for &len in &NULL_LENGTHS {
        let wide_len = len as z_size_t;

        // The null face has its own gates, so the null pointer is formed inside the
        // harness crate and never here. `len` is never 1, by the construction of
        // `NULL_LENGTHS`, which is what keeps the two oracle gates below out of the one
        // reference path that reads a byte before it tests the pointer -- they assert that
        // precondition themselves, and would panic rather than crash if it were violated.
        let observed = [
            port::adler32_null(adler_seed, len),
            port::adler32_z_null(adler_seed, wide_len),
            oracle::adler32_null(adler_seed, len),
            oracle::adler32_z_null(adler_seed, wide_len),
        ];
        for value in observed {
            assert_eq!(
                value, ADLER32_SEED,
                "a null Adler-32 buffer must answer the seed: seed {adler_seed:#x}, len {len}"
            );
        }

        // As above, and with one fewer caveat -- `crc32_z` tests the pointer first
        // (`crc32.c` L627-L628), so no length is excluded for this family and the oracle
        // gates carry no precondition of their own.
        let observed = [
            port::crc32_null(crc_seed, len),
            port::crc32_z_null(crc_seed, wide_len),
            oracle::crc32_null(crc_seed, len),
            oracle::crc32_z_null(crc_seed, wide_len),
        ];
        for value in observed {
            assert_eq!(
                value, CRC32_SEED,
                "a null CRC-32 buffer must answer the seed: seed {crc_seed:#x}, len {len}"
            );
        }
    }
}

/// Covers the one null-buffer length the oracle cannot be asked about.
///
/// `adler32_z` tests `len == 1` at `adler32.c` L70 and reads `buf[0]` there, *before* it
/// tests `buf == Z_NULL` at L81; the comment at L80 calls the ordering a "deferred check
/// for len == 1 speed". So `c_adler32(a, Z_NULL, 1)` dereferences a null pointer in the
/// reference implementation, and calling it here would crash on correct C rather than
/// report a defect in the port.
///
/// The port reaches the null test first and answers the seed for every length, which
/// removes that undefined behaviour without changing a single defined result. This
/// function asserts exactly that, and it is Rust-only **by design rather than by
/// omission** -- there is no reference answer to compare against, because the reference
/// has no defined behaviour to produce one.
///
/// The CRC-32 family has no such hazard, so its `len == 1` case is compared against the
/// oracle in full.
fn check_null_at_length_one(adler_seed: uLong, crc_seed: uLong) {
    // No pointer is dereferenced on this path at all. `crates/libz-rs-sys/src/checksum.rs`
    // tests `buf.is_null()` before it builds any slice and returns the seed directly, so the
    // null pointer the gate forms reaches nothing that could read it. The reference is
    // deliberately not called with these arguments -- see this function's documentation --
    // and `oracle::adler32_null` would refuse this length if it were.
    let observed = [
        port::adler32_null(adler_seed, 1),
        port::adler32_z_null(adler_seed, 1),
    ];
    for value in observed {
        assert_eq!(
            value, ADLER32_SEED,
            "the port must answer the Adler-32 seed for a null buffer of length 1, \
             where adler32.c would dereference it: seed {adler_seed:#x}"
        );
    }

    // As in `check_null_buffer` -- `crc32_z` tests the pointer against `Z_NULL` first
    // (`crc32.c` L627-L628), so a length of 1 reads nothing and both implementations are
    // defined here, which is why the reference is included for this family.
    let observed = [
        port::crc32_null(crc_seed, 1),
        port::crc32_z_null(crc_seed, 1),
        oracle::crc32_null(crc_seed, 1),
        oracle::crc32_z_null(crc_seed, 1),
    ];
    for value in observed {
        assert_eq!(
            value, CRC32_SEED,
            "a null CRC-32 buffer of length 1 must answer the seed: seed {crc_seed:#x}"
        );
    }
}

// ---------------------------------------------------------------------------
//  Streaming equivalence
// ---------------------------------------------------------------------------

/// Largest prime below 65536 -- `adler32.c` L10's `BASE`, and the modulus both halves of an
/// Adler-32 value are reduced by.
const ADLER_BASE: uLong = 65521;

/// Reduces both halves of an Adler-32 starting value into `0..BASE`.
///
/// ★ Needed because **resumability is a property of well-formed Adler-32 values only**, and
/// asserting it for any other starting value asserts something the reference implementation
/// does not do either.
///
/// `adler32` reduces a starting value in three different places, and they are not
/// equivalent when the value is already out of range. The byte-at-a-time branch
/// (`adler32.c` L69-L77, entered only at `len == 1`) applies a *single conditional
/// subtraction* to each half -- `if (sum2 >= BASE) sum2 -= BASE;` -- whereas the general
/// path applies a full `MOD`. So a `sum2` seeded at `0xffff` (65535, which is `BASE + 14`)
/// comes back as 14 from one path and as something else from the other, and the difference
/// then propagates through the rest of the accumulation.
///
/// Measured on this tree, seed `0xffffffef` over the three bytes `00 00 00`:
///
/// | | one-shot | one byte at a time |
/// |---|---|---|
/// | `adler32.c` | `0x0008ffef` | `0xfff9ffef` |
/// | this port | `0x0008ffef` | `0xfff9ffef` |
///
/// The two implementations agree exactly -- zero divergence -- and *neither* is resumable
/// there. So a chunking assertion over a raw fuzzer seed reports a defect in the port for
/// faithfully reproducing the reference, which is the opposite of this target's purpose. The
/// seed is therefore normalised for the resumability assertions specifically, which is
/// exactly the domain `zlib.h` L1819-L1826 describes: every value `adler32` *returns* has
/// both halves below `BASE`, so a caller accumulating across calls is always in this domain.
///
/// Coverage of out-of-range seeds is not lost, and that matters: [`check_adler_one_shot`]
/// compares the port against `adler32.c` on the *raw* seed and is where the three reduction
/// paths are held to the reference. What is dropped here is only the claim that those paths
/// agree with *each other* outside the well-formed range -- a claim the reference refutes.
///
/// CRC-32 needs no equivalent. It has no modulus and no distinguished range: every `u32` is
/// a valid running CRC, so its resumability assertions take the raw seed unchanged.
fn normalised_adler_seed(seed: uLong) -> uLong {
    let low = (seed & 0xffff) % ADLER_BASE;
    let high = ((seed >> 16) & 0xffff) % ADLER_BASE;
    low | (high << 16)
}

/// Requires that chunking an input changes nothing about the answer.
///
/// Both families document that successive calls accumulate, so that feeding a sequence in
/// pieces of any sizes yields the value of one call over the whole of it (`zlib.h`
/// L1819-L1826 and L1856-L1863 spell it out as the intended usage pattern). Callers rely on
/// it directly -- `deflate` updates the running check value once per `read_buf`, which for a
/// caller feeding a stream in small pieces is once per piece -- so a port that lost
/// resumability would break every streaming consumer while still passing a one-shot test.
///
/// Two chunk sizes per invocation: the fuzzer's, and one larger than the whole payload so
/// the single-call degenerate case is always covered. Both the port and the reference are
/// streamed, and both are held to the *port's* one-shot answer, which chains the
/// equalities -- reference-streamed equals port-one-shot only if the two implementations
/// agree and both are resumable.
///
/// The Adler-32 seed is normalised first; [`normalised_adler_seed`] records why, and why the
/// CRC-32 seed is not.
fn check_streaming(data: &[u8], adler_seed: uLong, crc_seed: uLong, chunk: usize) {
    let len = data.len();
    let adler_seed = normalised_adler_seed(adler_seed);
    let one_shot_adler = rust_adler32(adler_seed, data);
    let one_shot_crc = rust_crc32(crc_seed, data);

    for size in [chunk, len + 1] {
        assert_eq!(
            stream(rust_adler32, adler_seed, data, size),
            one_shot_adler,
            "the port's adler32 is not resumable: seed {adler_seed:#x}, len {len}, chunk {size}"
        );
        assert_eq!(
            stream(oracle_adler32, adler_seed, data, size),
            one_shot_adler,
            "chunked adler32.c disagrees with one-shot adler32: \
             seed {adler_seed:#x}, len {len}, chunk {size}"
        );
        assert_eq!(
            stream(rust_crc32, crc_seed, data, size),
            one_shot_crc,
            "the port's crc32 is not resumable: seed {crc_seed:#x}, len {len}, chunk {size}"
        );
        assert_eq!(
            stream(oracle_crc32, crc_seed, data, size),
            one_shot_crc,
            "chunked crc32.c disagrees with one-shot crc32: \
             seed {crc_seed:#x}, len {len}, chunk {size}"
        );
    }
}

/// Feeds a bounded prefix one byte at a time.
///
/// Covered deterministically rather than left to the fuzzer's chunk selector, because
/// `adler32.c` L70-L78 is a *distinct branch with its own reduction* -- a conditional
/// subtraction on each half instead of a modulo -- that exists precisely for the caller who
/// checksums a byte at a time, and it is reached only at a length of exactly one. The
/// reference's own comment at L69 is "in case user likes doing a byte at a time, keep it
/// fast", so this is a supported access pattern and not an edge case.
///
/// The prefix is bounded by [`BYTEWISE_PREFIX_LEN`] so the pass costs a fixed, small number
/// of calls no matter how long the payload is.
fn check_bytewise(data: &[u8], adler_seed: uLong, crc_seed: uLong) {
    let prefix = &data[..data.len().min(BYTEWISE_PREFIX_LEN)];
    let len = prefix.len();
    // Normalised for the reason [`normalised_adler_seed`] records, and this is the function
    // where it matters most: it drives `len == 1` on every call, which is precisely the
    // branch whose single conditional subtraction differs from the general path's modulo.
    let adler_seed = normalised_adler_seed(adler_seed);
    let one_shot_adler = rust_adler32(adler_seed, prefix);
    let one_shot_crc = rust_crc32(crc_seed, prefix);

    assert_eq!(
        stream(rust_adler32, adler_seed, prefix, 1),
        one_shot_adler,
        "byte-at-a-time adler32 diverged in the port: seed {adler_seed:#x}, len {len}"
    );
    assert_eq!(
        stream(oracle_adler32, adler_seed, prefix, 1),
        one_shot_adler,
        "byte-at-a-time adler32.c diverged from the port: seed {adler_seed:#x}, len {len}"
    );
    assert_eq!(
        stream(rust_crc32, crc_seed, prefix, 1),
        one_shot_crc,
        "byte-at-a-time crc32 diverged in the port: seed {crc_seed:#x}, len {len}"
    );
    assert_eq!(
        stream(oracle_crc32, crc_seed, prefix, 1),
        one_shot_crc,
        "byte-at-a-time crc32.c diverged from the port: seed {crc_seed:#x}, len {len}"
    );
}

/// Crosses Adler-32's `NMAX` block boundary, on every invocation.
///
/// This is the highest-value assertion in the streaming family and the reason
/// [`NMAX_PROBE_LEN`] is a fixed constant. `adler32.c` L96-L106 accumulates in blocks of
/// `NMAX` bytes and reduces both sums modulo `BASE` once per block, which is the only place
/// in the algorithm where a full modulo happens mid-buffer; L108-L121 then handles the tail
/// with a second reduction. A port that got the block length, the reduction points or the
/// unrolled group size wrong would agree with the reference on every input shorter than
/// 5552 bytes and disagree on every input longer -- so an assertion that only ran when the
/// fuzzer produced a long input would, under libFuzzer's default `-max_len` of 4096, never
/// run at all.
///
/// Adler-32 only, and deliberately so. `NMAX` is an Adler-32 constant: it exists because the
/// 32-bit sums stop being provably safe past that many bytes, and the CRC-32 has no analogous
/// mid-buffer boundary at all. The braided engine's only length threshold is 47 bytes
/// (`crc32.c` L640), which the ordinary payload clears on most invocations and then runs a
/// dozen times over -- so a five-kilobyte CRC-32 pass would repeat coverage already had, at
/// the single largest per-invocation cost in the file.
///
/// The differential comparison is unconditional and the resumption pass is sampled, which is
/// a measured split rather than an arbitrary one. The port's one-shot pass is what drives its
/// own block loop, so it has to run every time or the boundary is not really covered; the
/// resumption pass is a second traversal of the same bytes, and it adds a distinct fact --
/// that sums carried across a call still compose once the total exceeds `NMAX` -- that
/// `check_streaming` already establishes for shorter inputs. Sampling the second and keeping
/// the first is what that difference in value buys.
fn check_nmax_boundary(probe: &[u8], adler_seed: uLong, split: usize, resumption: bool) {
    let len = probe.len();
    let rust = rust_adler32(adler_seed, probe);

    assert_eq!(
        rust,
        oracle_adler32(adler_seed, probe),
        "adler32 diverged from adler32.c across the NMAX boundary: \
         seed {adler_seed:#x}, len {len}"
    );

    if resumption {
        // Normalised, and the one-shot answer recomputed from the same value, so that the
        // two sides of the equality start from one state.  A split of 1 reaches the
        // byte-at-a-time branch, so this arm is exposed to exactly the asymmetry
        // [`normalised_adler_seed`] describes.
        let seed = normalised_adler_seed(adler_seed);
        let (head, tail) = probe.split_at(split);
        assert_eq!(
            rust_adler32(rust_adler32(seed, head), tail),
            rust_adler32(seed, probe),
            "adler32 is not resumable across the NMAX boundary: \
             seed {seed:#x}, len {len}, split {split}"
        );
    }
}

// ---------------------------------------------------------------------------
//  Combination
// ---------------------------------------------------------------------------

/// Requires that combining two halves' Adler-32 sums reproduces the whole's.
///
/// `adler32_combine` answers "the checksum of `seq1` followed by `seq2`" from `adler1`,
/// `adler2` and `len2` alone, touching neither sequence and never needing `len1`
/// (`zlib.h` L1839-L1845). That closed form is what is asserted: the combined value must
/// equal both the direct checksum of the concatenation and the incremental
/// `adler32(adler32(1, head), tail)`, which is the same number reached two other ways.
///
/// The `*64` variant is asserted alongside it because `zconf.h` L1976-L2013 redirects
/// `adler32_combine` to `adler32_combine64` depending on the *caller's* `_FILE_OFFSET_BITS`,
/// so both symbols are part of the exported contract and a consumer may reference either.
/// `adler32.c` L158-L164 backs both with one `local` function, so they cannot legitimately
/// disagree -- which makes an assertion that they do not a cheap facade test.
fn check_adler_combine(data: &[u8], split: usize) {
    let (head, tail) = data.split_at(split);
    let tail_len = tail.len() as z_off_t;
    let tail_len_64 = tail.len() as z_off64_t;

    let head_sum = rust_adler32(ADLER32_SEED, head);
    let tail_sum = rust_adler32(ADLER32_SEED, tail);
    let combined = port::adler32_combine(head_sum, tail_sum, tail_len);

    // Three integers by value in each direction and no pointer anywhere: `adler32.c`
    // L158-L164 forwards each entry point to the `local` `adler32_combine_`, which is pure
    // arithmetic. So the only FFI obligation these four gates carry is that the reference
    // declarations match the definitions, which the oracle crate establishes by auditing
    // the renamed archive with `nm`; the port's two need no `unsafe` at all, because the
    // facade declares pointer-free entry points as safe `extern "C"`.
    let (reference, reference_64) = (
        oracle::adler32_combine(head_sum, tail_sum, tail_len),
        oracle::adler32_combine64(head_sum, tail_sum, tail_len_64),
    );

    assert_eq!(
        combined,
        rust_adler32(ADLER32_SEED, data),
        "adler32_combine does not reproduce the whole: split {split}, tail_len {tail_len}"
    );
    assert_eq!(
        combined,
        rust_adler32(head_sum, tail),
        "adler32_combine disagrees with an incremental update: split {split}"
    );
    assert_eq!(
        combined, reference,
        "adler32_combine diverged from adler32.c: split {split}, tail_len {tail_len}"
    );
    assert_eq!(
        combined,
        port::adler32_combine64(head_sum, tail_sum, tail_len_64),
        "adler32_combine and adler32_combine64 disagree in the port: split {split}"
    );
    assert_eq!(
        combined, reference_64,
        "adler32_combine64 diverged from adler32.c: split {split}, tail_len {tail_len_64}"
    );
    assert_eq!(
        combined,
        widen(zlib_rs::adler32::adler32_combine(
            narrow(head_sum),
            narrow(tail_sum),
            tail_len_64
        )),
        "the adler32_combine facade and the safe core disagree: split {split}"
    );
}

/// Requires that combining two halves' CRC-32 check values reproduces the whole's.
///
/// The same three-way equality as [`check_adler_combine`], against `crc32_combine` and
/// `crc32_combine64` (`zlib.h` L1874-L1881 and L1983). `crc32.c` L982-L984 makes the
/// unsuffixed form a one-line forward to the `*64` form, so a disagreement between them
/// would be a facade defect rather than an arithmetic one -- which is exactly why both are
/// driven here.
fn check_crc_combine(data: &[u8], split: usize) {
    let (head, tail) = data.split_at(split);
    let tail_len = tail.len() as z_off_t;
    let tail_len_64 = tail.len() as z_off64_t;

    let head_crc = rust_crc32(CRC32_SEED, head);
    let tail_crc = rust_crc32(CRC32_SEED, tail);
    let combined = port::crc32_combine(head_crc, tail_crc, tail_len);

    // As in `check_adler_combine` -- three integers by value, no pointer and no dereference
    // anywhere on the path (`crc32.c` L977-L984 reduces both to `multmodp` arithmetic over
    // the operator table).
    let (reference, reference_64) = (
        oracle::crc32_combine(head_crc, tail_crc, tail_len),
        oracle::crc32_combine64(head_crc, tail_crc, tail_len_64),
    );

    assert_eq!(
        combined,
        rust_crc32(CRC32_SEED, data),
        "crc32_combine does not reproduce the whole: split {split}, tail_len {tail_len}"
    );
    assert_eq!(
        combined,
        rust_crc32(head_crc, tail),
        "crc32_combine disagrees with an incremental update: split {split}"
    );
    assert_eq!(
        combined, reference,
        "crc32_combine diverged from crc32.c: split {split}, tail_len {tail_len}"
    );
    assert_eq!(
        combined,
        port::crc32_combine64(head_crc, tail_crc, tail_len_64),
        "crc32_combine and crc32_combine64 disagree in the port: split {split}"
    );
    assert_eq!(
        combined, reference_64,
        "crc32_combine64 diverged from crc32.c: split {split}, tail_len {tail_len_64}"
    );
    assert_eq!(
        combined,
        widen(zlib_rs::crc32::crc32_combine(
            narrow(head_crc),
            narrow(tail_crc),
            tail_len_64
        )),
        "the crc32_combine facade and the safe core disagree: split {split}"
    );
}

// ---------------------------------------------------------------------------
//  The operator pair: crc32_combine_gen and crc32_combine_op
// ---------------------------------------------------------------------------

/// Checks the four generator surfaces agree, and returns the operator for reuse.
///
/// `crc32_combine_gen` turns a length into an operator to be used with
/// `crc32_combine_op` (`zlib.h` L1884-L1888), and both the unsuffixed and `*64` spellings
/// are exported because `zconf.h`'s large-file redirection decides which name a caller
/// references at that caller's own compile time. `crc32.c` L964-L966 makes the unsuffixed
/// form a one-line forward to the `*64` form, so all four values here -- two per
/// implementation -- must be one number.
fn check_combine_gen(tail_len: z_off_t, tail_len_64: z_off64_t) -> uLong {
    let op = port::crc32_combine_gen(tail_len);

    // One integer by value into each gate and no dereference anywhere; `crc32.c`
    // L954-L966 is a sign test followed by `x2nmodp`, which only walks a `static const`
    // table by value. No pointer crosses the boundary in either direction.
    let (reference, reference_64) = (
        oracle::crc32_combine_gen(tail_len),
        oracle::crc32_combine_gen64(tail_len_64),
    );

    assert_eq!(
        op,
        port::crc32_combine_gen64(tail_len_64),
        "crc32_combine_gen and crc32_combine_gen64 disagree in the port: len {tail_len}"
    );
    assert_eq!(
        op, reference,
        "crc32_combine_gen diverged from crc32.c: len {tail_len}"
    );
    assert_eq!(
        op, reference_64,
        "crc32_combine_gen64 diverged from crc32.c: len {tail_len_64}"
    );
    assert_eq!(
        op,
        widen(zlib_rs::crc32::crc32_combine_gen64(tail_len_64)),
        "the crc32_combine_gen64 facade and the safe core disagree: len {tail_len_64}"
    );

    if tail_len_64 == 0 {
        assert_eq!(
            op, CRC32_COMBINE_GEN_ZERO_LEN,
            "the operator for a zero length must be x^0"
        );
    }

    op
}

/// Requires that one generated operator answers exactly what `crc32_combine` answers.
///
/// This is the whole reason `crc32_combine_gen` exists: `zlib.h` L1891-L1894 says
/// `crc32_combine_op` "Give[s] the same result as crc32_combine(), using op in place of
/// len2", and that it "will be faster than crc32_combine() if the generated op is used more
/// than once". So the operator has to be *reusable* -- correct for every pair of check
/// values it is applied to, not merely for the one it was generated alongside. That is what
/// the loop over several pairs establishes, and it is why the pairs include values no real
/// payload produced.
///
/// The identity asserted here is structural in the reference rather than incidental:
/// `crc32.c` L977-L979 defines `crc32_combine64(crc1, crc2, len2)` as literally
/// `crc32_combine_op(crc1, crc2, crc32_combine_gen64(len2))`. A port that computed the
/// combine some other way would still be free to disagree with its own operator path, and
/// this is what forbids it.
fn check_combine_op_reuse(op: uLong, tail_len: z_off_t, tail_len_64: z_off64_t, pairs: &[Pair]) {
    for &(left, right) in pairs {
        let by_op = port::crc32_combine_op(left, right, op);

        // Three integers by value into `crc32.c` L969-L984, which dereferences nothing:
        // `crc32_combine_op` masks both check values and calls `multmodp`, and
        // `crc32_combine` forwards through `crc32_combine64` to that same function.
        let (reference_by_op, reference_by_len) = (
            oracle::crc32_combine_op(left, right, op),
            oracle::crc32_combine(left, right, tail_len),
        );

        assert_eq!(
            by_op,
            port::crc32_combine(left, right, tail_len),
            "crc32_combine_op and crc32_combine disagree in the port: \
             crc1 {left:#x}, crc2 {right:#x}, op {op:#x}, len {tail_len}"
        );
        assert_eq!(
            by_op,
            port::crc32_combine64(left, right, tail_len_64),
            "crc32_combine_op and crc32_combine64 disagree in the port: \
             crc1 {left:#x}, crc2 {right:#x}, op {op:#x}"
        );
        assert_eq!(
            by_op, reference_by_op,
            "crc32_combine_op diverged from crc32.c: \
             crc1 {left:#x}, crc2 {right:#x}, op {op:#x}"
        );
        assert_eq!(
            by_op, reference_by_len,
            "the reused operator disagrees with crc32.c's crc32_combine: \
             crc1 {left:#x}, crc2 {right:#x}, len {tail_len}"
        );
        assert_eq!(
            by_op,
            widen(zlib_rs::crc32::crc32_combine_op(
                narrow(left),
                narrow(right),
                narrow(op)
            )),
            "the crc32_combine_op facade and the safe core disagree: \
             crc1 {left:#x}, crc2 {right:#x}, op {op:#x}"
        );
    }
}

// ---------------------------------------------------------------------------
//  The three sentinels
// ---------------------------------------------------------------------------

/// A `(crc1, crc2)` pair as the combine and operator entry points take them.
type Pair = (uLong, uLong);

/// The check-value pairs every operator and sentinel assertion is applied to.
///
/// Four pairs, each chosen for a reason:
///
/// * the payload's own two halves, which is the case a real caller produces;
/// * `(uLong::MAX, uLong::MAX)`, which is the masking itself under test -- every bit at or
///   above position 32 must be discarded identically by both implementations, and on LP64
///   that is thirty-two bits the reference never looks at;
/// * two values drawn freely by the fuzzer, because `crc32_combine_op` masks both with
///   `0xffffffff` (`crc32.c` L972) and is otherwise indifferent to their provenance, so
///   unconstrained values cover its arithmetic far better than realistic ones;
/// * `(0, 0)`, the identity-adjacent case.
///
/// The order is load-bearing: the sentinel assertions take only the first
/// [`SENTINEL_PAIRS`] of them, so the two whose *values* are most likely to be mishandled
/// come first.
fn checksum_pairs(input: &ChecksumInput<'_>, head: uLong, tail: uLong) -> [Pair; 4] {
    [
        (head, tail),
        (uLong::MAX, uLong::MAX),
        (widen(input.synthetic_crc1), widen(input.synthetic_crc2)),
        (0, 0),
    ]
}

/// How many of [`checksum_pairs`] the sentinel assertions are applied to.
///
/// Fewer than all of them, and that is a reasoned reduction rather than a shortcut. Every
/// sentinel path returns *before* it examines either check value: `adler32_combine_` tests
/// `len2 < 0` first (`adler32.c` L138-L140), `crc32_combine_gen64` likewise
/// (`crc32.c` L955-L956), and `crc32_combine_op` returns on `op == 0` before it reaches
/// `multmodp` (L970-L971). So the answer is pair-independent by construction, and sweeping
/// four pairs across three negative lengths would assert one fact twelve times -- at a cost
/// the whole file is measured on. Two pairs still demonstrate the independence, and the
/// ordering above puts the realistic pair and the all-ones pair in that position.
const SENTINEL_PAIRS: usize = 2;

/// Requires the Adler-32 combines to answer `0xffffffff` for a negative length.
///
/// `adler32_combine_` opens with `if (len2 < 0) return 0xffffffffUL;` (`adler32.c`
/// L138-L140) under the comment "for negative len, return invalid adler32 as a clue for
/// debugging". `zlib.h` L1843-L1845 says only that a negative `len2` leaves the result with
/// "no meaning or utility", so this exact value is *implementation-defined* -- which is
/// precisely why it has to be reproduced rather than reasoned about, and why it is asserted
/// against the reference's own return as well as against the literal. It must also not be
/// unified with the CRC-32 family's answer, which is `0`: the two sentinels are different
/// values carrying different information.
fn check_adler_sentinel(negative: z_off_t, negative_64: z_off64_t, pairs: &[Pair]) {
    for &(left, right) in pairs {
        let rust = port::adler32_combine(left, right, negative);
        let rust_64 = port::adler32_combine64(left, right, negative_64);

        // Integers by value into `adler32.c` L158-L164, which returns before doing anything
        // at all for a negative length. No pointer is involved.
        let (reference, reference_64) = (
            oracle::adler32_combine(left, right, negative),
            oracle::adler32_combine64(left, right, negative_64),
        );

        assert_eq!(
            rust, ADLER32_COMBINE_INVALID,
            "adler32_combine must answer the invalid sentinel for len2 {negative}"
        );
        assert_eq!(
            rust, reference,
            "adler32_combine diverged from adler32.c for len2 {negative}"
        );
        assert_eq!(
            rust_64, ADLER32_COMBINE_INVALID,
            "adler32_combine64 must answer the invalid sentinel for len2 {negative_64}"
        );
        assert_eq!(
            rust_64, reference_64,
            "adler32_combine64 diverged from adler32.c for len2 {negative_64}"
        );
        assert_eq!(
            rust,
            widen(zlib_rs::adler32::adler32_combine(
                narrow(left),
                narrow(right),
                negative_64
            )),
            "the adler32_combine facade and the safe core disagree for len2 {negative_64}"
        );
    }
}

/// Requires the CRC-32 generators to answer `0` for a negative length.
///
/// `crc32_combine_gen64` opens with `if (len2 < 0) return 0;` (`crc32.c` L955-L956), which
/// `zlib.h` L1886-L1888 documents as "len2 must be non-negative, otherwise zero is
/// returned". The consequence propagates: `crc32_combine64` is defined as
/// `crc32_combine_op(crc1, crc2, crc32_combine_gen64(len2))` (L977-L979), so a negative
/// length produces a zero operator, which the operator guard then turns into a zero result.
/// Both steps are asserted, so a port that returned the right answer by the wrong route
/// would still be caught.
fn check_crc_gen_sentinel(negative: z_off_t, negative_64: z_off64_t) {
    let gen = port::crc32_combine_gen(negative);
    let gen_64 = port::crc32_combine_gen64(negative_64);

    // One integer by value into `crc32.c` L954-L966, which returns immediately for a
    // negative length. No pointer is involved.
    let (reference, reference_64) = (
        oracle::crc32_combine_gen(negative),
        oracle::crc32_combine_gen64(negative_64),
    );

    assert_eq!(
        gen, CRC32_COMBINE_GEN_INVALID,
        "crc32_combine_gen must answer zero for len2 {negative}"
    );
    assert_eq!(
        gen, reference,
        "crc32_combine_gen diverged from crc32.c for len2 {negative}"
    );
    assert_eq!(
        gen_64, CRC32_COMBINE_GEN_INVALID,
        "crc32_combine_gen64 must answer zero for len2 {negative_64}"
    );
    assert_eq!(
        gen_64, reference_64,
        "crc32_combine_gen64 diverged from crc32.c for len2 {negative_64}"
    );
    assert_eq!(
        gen,
        widen(zlib_rs::crc32::crc32_combine_gen64(negative_64)),
        "the crc32_combine_gen64 facade and the safe core disagree for len2 {negative_64}"
    );
}

/// Requires `crc32_combine_op` to answer `0` for a zero operator, and the combines with it.
///
/// `crc32_combine_op` opens with `if (op == 0) return 0;` (`crc32.c` L970-L971), and the
/// guard is load-bearing rather than defensive: `multmodp` is documented at `crc32.c`
/// L158-L161 as requiring "that a not be zero", so the early return is what keeps a zero
/// operator out of a loop that would not terminate correctly on it. The same function is
/// then what makes a negative length produce a zero *combine*, which is asserted here for
/// both the unsuffixed and the `*64` spelling.
fn check_crc_op_sentinel(negative: z_off_t, negative_64: z_off64_t, pairs: &[Pair]) {
    for &(left, right) in pairs {
        let zero_op = port::crc32_combine_op(left, right, 0);
        let by_negative = port::crc32_combine(left, right, negative);
        let by_negative_64 = port::crc32_combine64(left, right, negative_64);

        // Three integers by value into `crc32.c` L969-L984. The zero-operator path returns
        // before reaching `multmodp` and the negative-length path returns before reaching
        // the operator table; neither dereferences anything.
        let (reference_zero_op, reference_negative) = (
            oracle::crc32_combine_op(left, right, 0),
            oracle::crc32_combine(left, right, negative),
        );

        assert_eq!(
            zero_op, CRC32_COMBINE_OP_ZERO,
            "crc32_combine_op must answer zero for a zero operator: \
             crc1 {left:#x}, crc2 {right:#x}"
        );
        assert_eq!(
            zero_op, reference_zero_op,
            "crc32_combine_op diverged from crc32.c for a zero operator: \
             crc1 {left:#x}, crc2 {right:#x}"
        );
        assert_eq!(
            by_negative, CRC32_COMBINE_GEN_INVALID,
            "crc32_combine must answer zero for len2 {negative}"
        );
        assert_eq!(
            by_negative, reference_negative,
            "crc32_combine diverged from crc32.c for len2 {negative}"
        );
        assert_eq!(
            by_negative_64, CRC32_COMBINE_GEN_INVALID,
            "crc32_combine64 must answer zero for len2 {negative_64}"
        );
        assert_eq!(
            zero_op,
            widen(zlib_rs::crc32::crc32_combine_op(
                narrow(left),
                narrow(right),
                0
            )),
            "the crc32_combine_op facade and the safe core disagree for a zero operator"
        );
    }
}

// ---------------------------------------------------------------------------
//  get_crc_table
// ---------------------------------------------------------------------------

/// Compares the published CRC-32 table against the reference's, three ways.
///
/// `zlib.h` L2034-L2035 declares `get_crc_table` among the undocumented entry points as
/// returning a bare `const z_crc_t FAR *`, and `crc32.c` L478-L481 gives its purpose: to let
/// an assembler `crc32()` share the table, and "to force the generation of the CRC tables in
/// a threaded application". The second purpose does not apply to this port and cannot -- the
/// tables are `const` data rather than lazily built behind the unsynchronised `z_once_t` that
/// `crc32.c` L12-L17 warns about -- which is also why no warm-up call is needed before the
/// comparison and why `zlibCompileFlags` reports bit 13, `DYNAMIC_CRC_TABLE`, clear.
///
/// Three comparisons, because they establish different things. `c_get_crc_table` is what a C
/// caller of the reference would receive; `oracle_crc_table` is the linkable view of the
/// `local` array in `crc32.h`, which has no symbol of its own and so cannot be reached any
/// other way; and `CRC_TABLE` is the transcription the port actually computes with. Slice
/// equality is the element-for-element comparison, performed by the standard library rather
/// than by a hand-rolled loop that could get its own bounds wrong.
///
/// Guarded by a fuzzer-chosen boolean in [`drive`]. The exhaustive version of this check
/// belongs to `crates/zlib-rs-differential/tests/table_equality.rs`, which runs it once
/// deterministically; repeating constant work on every invocation here would only spend the
/// time budget on an answer that cannot change.
fn check_crc_table() {
    // Both gates present the pointer `get_crc_table` returns as the `&'static [z_crc_t]` the
    // C contract describes -- at least 256 entries in `const` storage that nothing ever
    // writes, `crc32.c` L216-L232 and L482-L487 -- so non-nullness and the element count are
    // properties of the type here rather than assertions to make, and the comparisons below
    // are slice equality performed by the standard library.
    let rust_table = port::crc_table();
    let reference_table = oracle::crc_table();

    assert_eq!(
        rust_table.len(),
        CRC_TABLE_LEN,
        "get_crc_table must publish one entry per byte value"
    );
    assert_eq!(
        reference_table.len(),
        CRC_TABLE_LEN,
        "c_get_crc_table must publish one entry per byte value"
    );

    let local = oracle_crc_table();
    assert_eq!(
        local.len(),
        CRC_TABLE_LEN,
        "crc32.h's crc_table must have one entry per byte value"
    );
    assert_eq!(
        rust_table, reference_table,
        "get_crc_table diverged from c_get_crc_table"
    );
    assert_eq!(
        rust_table, local,
        "get_crc_table diverged from crc32.h's crc_table"
    );
    assert_eq!(
        rust_table,
        &CRC_TABLE[..],
        "get_crc_table diverged from the table the port computes with"
    );
    assert_eq!(
        rust_table.as_ptr(),
        port::crc_table().as_ptr(),
        "get_crc_table must return the same address on every call"
    );
}

// ---------------------------------------------------------------------------
//  Backend interchangeability
// ---------------------------------------------------------------------------

/// Requires every CRC-32 backend to answer identically, and the dispatcher to match.
///
/// `Crc32Backend` exists so that the engines are interchangeable *by construction*, and its
/// contract is that an implementation "may differ only in how long it takes". This runs in
/// the default build as well as under `--features simd`, and it is not vacuous there:
/// without the feature the dispatcher selects `Braid`, the word-at-a-time engine of
/// `crc32.c` L637-L920, while `Generic` is the byte-at-a-time reference path -- two genuinely
/// different computations that must produce one number.
///
/// The backends fold *pre-conditioned* state, so the two complements `crc32` applies
/// (`crc32.c` L635 and L940) are applied here instead, once each, which is what makes the
/// result comparable with the dispatcher's.
fn check_crc_backends(data: &[u8], seed: u32) {
    let generic_name = <Generic as Crc32Backend>::NAME;
    let braid_name = <Braid as Crc32Backend>::NAME;

    let generic = !<Generic as Crc32Backend>::update(!seed, data);
    let braid = !<Braid as Crc32Backend>::update(!seed, data);
    let len = data.len();

    assert_eq!(
        generic, braid,
        "the {generic_name} and {braid_name} CRC-32 backends disagree: \
         seed {seed:#x}, len {len}"
    );
    assert_eq!(
        generic,
        zlib_rs::crc32::crc32(seed, data),
        "the CRC-32 dispatcher disagrees with the {generic_name} backend: \
         seed {seed:#x}, len {len}"
    );
}

/// Requires the vectorized backends to answer exactly what the scalar ones answer.
///
/// This is the direct form of the requirement that the `simd` feature be output-neutral, and
/// it is available because the core exposes the engines by name for precisely this purpose --
/// `zlib-rs`'s own dispatch documentation records that a caller needing one specific engine
/// names it and reaches it through the backend trait, and lists the fuzz targets as one of
/// the consumers that do.
///
/// Worth stating why this is not the only thing that establishes neutrality, so the assertion
/// is not mistaken for the whole of it: with `--features simd` the dispatcher selects the
/// vectorized engine, so *every* differential equality in this file -- one-shot, streaming,
/// the `NMAX` probe, the combines -- is then a comparison of vectorized Rust against scalar
/// C. That is the stronger check, and it needs no feature-gated code at all. What this
/// function adds is locality: when it fails, the backend is named, rather than leaving a
/// divergence somewhere in the dispatch chain to be bisected.
#[cfg(feature = "simd")]
fn check_simd_neutrality(data: &[u8], adler_seed: u32, crc_seed: u32) {
    let len = data.len();

    let generic_adler = Adler32Generic::checksum(adler_seed, data);
    let simd_adler = Adler32Simd::checksum(adler_seed, data);
    assert_eq!(
        generic_adler, simd_adler,
        "the vectorized Adler-32 backend changed the answer: seed {adler_seed:#x}, len {len}"
    );
    assert_eq!(
        simd_adler,
        zlib_rs::adler32::adler32(adler_seed, data),
        "the Adler-32 dispatcher disagrees with the vectorized backend: \
         seed {adler_seed:#x}, len {len}"
    );

    let generic_crc = !<Generic as Crc32Backend>::update(!crc_seed, data);
    let simd_crc = !<Simd as Crc32Backend>::update(!crc_seed, data);
    assert_eq!(
        generic_crc, simd_crc,
        "the vectorized CRC-32 backend changed the answer: seed {crc_seed:#x}, len {len}"
    );
    assert_eq!(
        simd_crc,
        zlib_rs::crc32::crc32(crc_seed, data),
        "the CRC-32 dispatcher disagrees with the vectorized backend: \
         seed {crc_seed:#x}, len {len}"
    );
}

// ---------------------------------------------------------------------------
//  The NMAX probe buffer
// ---------------------------------------------------------------------------

/// Fills the fixed-length probe with content derived from the fuzzer's payload.
///
/// The *length* is fixed, because that is what guarantees the `NMAX` boundary is crossed on
/// every invocation; the *content* is not, because identical bytes every time would exercise
/// one path through the sums rather than the whole reduction schedule. Repeating the payload
/// is what ties the content to the corpus, so that a mutation libFuzzer finds interesting for
/// the short assertions is also interesting for the long one.
///
/// An empty payload has nothing to repeat, so it falls back to an arithmetic sequence seeded
/// from the fuzzer. The stride of 31 is coprime with 256, so the sequence visits every byte
/// value rather than a subset -- a constant fill would still cross the boundary but would
/// leave the per-byte accumulation trivially predictable.
///
/// Both paths fill in whole slices rather than byte by byte, and that is a measured decision
/// rather than a stylistic one. A `zip` against `payload.iter().cycle()` is the obvious
/// spelling and it was the first one here, but it costs a bounds-checked write and a branch
/// for each of five and a half thousand bytes -- and under cargo-fuzz each of those carries
/// sanitizer and coverage instrumentation, which made *filling* the probe cost more than
/// checksumming it. [`fill_repeating`] hands the same work to `copy_from_slice`, where the
/// instrumentation pays once per slice instead of once per byte.
fn fill_probe(probe: &mut [u8; NMAX_PROBE_LEN], payload: &[u8], seed: u8) {
    if payload.is_empty() {
        let mut pattern = [0_u8; 256];
        let mut value = seed;
        for slot in pattern.as_mut_slice() {
            *slot = value;
            value = value.wrapping_add(31);
        }
        fill_repeating(probe, &pattern);
        return;
    }

    fill_repeating(probe, payload);
}

/// Tiles `target` with repetitions of `source`, truncating the last one.
///
/// `source` must be non-empty; both callers guarantee it, which is why this cannot loop
/// forever and carries no check of its own. Every slice index is derived from `min`, so none
/// of them can be out of range and none can panic.
fn fill_repeating(target: &mut [u8], source: &[u8]) {
    let mut offset = 0;
    while offset < target.len() {
        let take = source.len().min(target.len() - offset);
        target[offset..offset + take].copy_from_slice(&source[..take]);
        offset += take;
    }
}

// ---------------------------------------------------------------------------
//  Driver
// ---------------------------------------------------------------------------

/// Runs every assertion family for one fuzzer-chosen input.
///
/// Every field is normalised into range in the opening block, before anything is asserted.
/// That ordering is deliberate: an index computed out of range or a chunk size of zero would
/// panic, libFuzzer would report the panic as a crash, and the crash would be a defect in
/// this harness rather than in the library -- which is the most expensive kind of false
/// positive a differential target can produce, because it looks exactly like a real finding.
/// Normalising up front is also what keeps the body free of `unwrap`, of `expect` and of any
/// fallible indexing at all.
fn drive(input: &ChecksumInput<'_>) {
    let payload = &input.payload[..input.payload.len().min(MAX_PAYLOAD_LEN)];
    let prefix = &payload[..payload.len().min(BYTEWISE_PREFIX_LEN)];
    let adler_seed = widen(input.adler_seed);
    let crc_seed = widen(input.crc_seed);
    let split = select_split(input.split, payload.len());
    let chunk = select_chunk(input.chunk, payload.len());
    let (negative, negative_64) = negative_offsets(input.negative_magnitude);

    // One-shot equality, including the seed's high bits and the null-buffer contract.
    check_adler_one_shot(payload, adler_seed);
    check_crc_one_shot(payload, crc_seed);
    check_wide_seed(prefix, adler_seed, crc_seed);
    check_null_buffer(adler_seed, crc_seed);
    check_null_at_length_one(adler_seed, crc_seed);

    // Streaming equivalence, over the payload and then across the NMAX block boundary.
    check_streaming(payload, adler_seed, crc_seed, chunk);
    check_bytewise(payload, adler_seed, crc_seed);

    let mut probe = [0_u8; NMAX_PROBE_LEN];
    fill_probe(&mut probe, payload, input.chunk);
    let probe_split = select_split(input.split, NMAX_PROBE_LEN);
    check_nmax_boundary(&probe, adler_seed, probe_split, input.probe_resumption);

    // Combination, then the operator pair that `crc32_combine64` is defined in terms of.
    check_adler_combine(payload, split);
    check_crc_combine(payload, split);

    let (head, tail) = payload.split_at(split);
    let tail_len = tail.len() as z_off_t;
    let tail_len_64 = tail.len() as z_off64_t;
    let pairs = checksum_pairs(
        input,
        rust_crc32(CRC32_SEED, head),
        rust_crc32(CRC32_SEED, tail),
    );
    let op = check_combine_gen(tail_len, tail_len_64);
    check_combine_op_reuse(op, tail_len, tail_len_64, &pairs);

    // The sentinels, at three negative lengths: the smallest in magnitude, one the fuzzer
    // chose, and the minimum of each offset type -- which is the value a bounds check
    // implemented as arithmetic rather than as a comparison is most likely to mishandle.
    let sentinel_pairs = &pairs[..SENTINEL_PAIRS];
    for (len2, len2_64) in [
        (-1, -1),
        (negative, negative_64),
        (z_off_t::MIN, z_off64_t::MIN),
    ] {
        check_adler_sentinel(len2, len2_64, sentinel_pairs);
        check_crc_gen_sentinel(len2, len2_64);
        check_crc_op_sentinel(len2, len2_64, sentinel_pairs);
    }

    if input.check_tables {
        check_crc_table();
    }

    // Backend interchangeability: the scalar engines always, the vectorized ones when the
    // feature that selects them is on. Bounded separately, because the byte-at-a-time engine
    // is the costliest path per byte here and gains nothing from a longer slice.
    let backend_slice = &payload[..payload.len().min(MAX_BACKEND_LEN)];
    check_crc_backends(backend_slice, input.crc_seed);
    #[cfg(feature = "simd")]
    check_simd_neutrality(backend_slice, input.adler_seed, input.crc_seed);
}

fuzz_target!(|input: ChecksumInput<'_>| {
    drive(&input);
});
