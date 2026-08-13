//! Adler-32 and CRC-32 throughput: the Rust port against the in-process C oracle, and the
//! scalar backends against the vectorization-friendly `simd` ones.
//!
//! This is the first and simplest of the three suites AAP §0.3.1 places at `benches/`. The
//! idioms it establishes -- deterministic input generation, committed-corpus loading, oracle
//! call gates, criterion group layout and graceful skipping -- are reused by
//! `benches/inflate_bench.rs` and `benches/deflate_bench.rs`.
//!
//! # What is measured
//!
//! Nine groups, in five families:
//!
//! * **Throughput against the reference** -- `adler32_throughput` and `crc32_throughput` over the
//!   synthetic length sweep, `adler32_fixtures` and `crc32_fixtures` over the committed corpus.
//!   `adler32_z` and `crc32_z` as exported by `crates/libz-rs-sys` -- the C ABI the port actually
//!   ships -- against `c_adler32_z` and `c_crc32_z`, the same two functions compiled from the
//!   in-tree C sources by `crates/zlib-rs-differential/build.rs`. Both implementations run in ONE
//!   process on ONE buffer, which AAP §0.6.4.6 requires so that cross-process noise cannot be
//!   mistaken for a throughput difference.
//! * **The `uInt` forwarders** -- `uint_forwarders`. `adler32` and `crc32` take a `uInt` length
//!   where the `_z` forms take a `z_size_t`; in the reference both are one-line forwarders
//!   (`adler32.c` L128-L130, `crc32.c` L946-L951) and in the port both reduce to the same slice
//!   call. One size is enough to show the forwarding costs nothing.
//! * **Backend against backend** -- `adler32_backends` and `crc32_backends`. `crates/zlib-rs`
//!   exposes its engines as named types behind the `Adler32Backend` and `Crc32Backend` traits, so
//!   the scalar and autovectorizable paths are measured directly rather than inferred from two
//!   whole-binary runs. `Adler32Generic`, `crc32::Generic` and `crc32::Braid` are always present;
//!   `Adler32Simd` and `crc32::StrideBraid` exist only under the `simd` feature. Each group also carries a
//!   `dispatch` row -- the safe core's own entry point -- so that selection cost and, against the
//!   throughput groups above, the cost of crossing the C ABI are both isolated.
//!
//!   ★ **How to read a `simd` row, because the name overpromises.** Neither backend contains an
//!   architecture intrinsic or any vector type: the core crate is `#![forbid(unsafe_code)]`, which
//!   rules those out. Both are portable integer code *shaped so that LLVM can autovectorize it*,
//!   and whether it does is a property of the compiler and of the target baseline being built --
//!   this repository sets no `-C target-cpu` anywhere, so that baseline is the default for the
//!   triple. A `simd` row that measures the same as its `generic` row is therefore a legitimate
//!   outcome on a given machine and not a bug in this bench. Selection is likewise not what the
//!   name suggests: `crc32::StrideBraid` performs no run-time feature probe at all, and the Adler-32
//!   dispatcher checks the input length first and treats detection only as a throughput hint --
//!   which is why [`detected_cpu_features`] is printed as an *explanation of a rate* rather than as
//!   a statement about which code ran.
//! * **The combine family** -- `combine`. `adler32_combine`, `crc32_combine`,
//!   `crc32_combine_gen`, `crc32_combine_op` and the three `*64` forms. These are O(log n)
//!   bit-matrix operations over `x2n_table`, not byte pipelines, so they are measured per call and
//!   carry no `Throughput`.
//! * **The table accessor** -- `get_crc_table`. Two rows and a one-shot note, present to document
//!   in the output that the port's tables are `const` data with no lazy initialization to measure.
//!
//! # Provenance
//!
//! Derived from the two engines it measures: `adler32.c` (164 lines) and `crc32.c` (983
//! lines), which AAP §0.4.1.5 names as this target's source. Every constant and every chosen
//! length below cites the line it comes from, so a maintainer can trace a number back to the
//! oracle. The framing -- read a real input, run the same operation through both
//! implementations, report a rate -- is borrowed from the repository's existing benchmark
//! driver, `contrib/testzlib/testzlib.c`, which times `compress`/`uncompress` over a file with
//! `QueryPerformanceCounter` and prints one line per phase. criterion replaces that hand-rolled
//! timer with a warmed, resampled, outlier-aware estimate; nothing else about the shape changes.
//!
//! # SIMD may change the speed and never the answer
//!
//! AAP §0.8.2 ambiguity 9 confines vectorization to CRC-32 and Adler-32, and the reason is
//! structural rather than cautious: a check value is one scalar however its recurrence is
//! evaluated, so independent partial remainders recombine exactly and no arrangement of lanes
//! can perturb a single emitted byte. Vectorizing match finding would change the compressed
//! output and is prohibited outright. This suite exists to quantify the speed-only benefit that
//! confinement leaves available -- which is also why every case here compares the port's answer
//! against the oracle's *before* timing it: a throughput number for a wrong value is worse than
//! no number at all.
//!
//! # This is not a performance gate either
//!
//! Every number this file produces is an **informational backend comparison**, and nothing reads it
//! automatically. Unlike `benches/deflate_bench.rs` and `benches/inflate_bench.rs`, this suite emits
//! no `RATIO-SUMMARY` line and no `MEMORY-SUMMARY` line, so the parser in the `bench` job of
//! `.github/workflows/rust.yml` finds nothing here to gate: a checksum regression cannot fail that
//! job. That is deliberate rather than an omission. The two throughput limits AAP §0.8.4 states are
//! for *compression* and *decompression*, and AAP §0.6.4.6 reads them from the multi-megabyte
//! Silesia members through the two suites that do emit summary lines. A checksum sits on the decode
//! and encode hot paths, so it contributes to those numbers, but its own rate is not one of the
//! stated targets and no threshold is defined for it.
//!
//! What the rows are for, then: choosing between the scalar and vectorized backends on a given
//! host, confirming that the `simd` feature is worth enabling there, seeing what crossing the C ABI
//! costs, and noticing a change in the shape of the length curve. Read them by hand, and use
//! criterion's baselines (below) when comparing two configurations.
//!
//! # This is not a correctness gate
//!
//! The agreement check below is a guard on the measurement, not a test. Table transcription is
//! gated by `crates/zlib-rs-differential/tests/table_equality.rs`, and byte identity by the
//! `tests/byte_identical.rs` that AAP §0.4.1.4 places beside it; those run under `cargo test`,
//! where a failure is an error and names the offending index or configuration. Here a mismatch
//! prints a diagnostic naming the algorithm, the case and both values, and the case is skipped --
//! because a benchmark that aborts tells a developer nothing about the other ninety-four cases, and
//! because a suite nobody can run is a suite nobody runs.
//!
//! # Running it
//!
//! ```text
//! cargo bench --manifest-path benches/Cargo.toml --bench checksum_bench
//! # one iteration per case:
//! cargo bench --manifest-path benches/Cargo.toml --bench checksum_bench -- --test
//! ```
//!
//! A full run measures the whole case set -- the `simd` feature adds the vectorized backend rows
//! rather than replacing any -- at criterion's default three-second warm-up and five-second
//! measurement per case, so the wall clock is roughly `cases x 8 s` plus build time. Pass
//! `--sample-size`, `--warm-up-time` or `--measurement-time` to trade precision for wall clock.
//! `--test` executes every case exactly once and is the cheapest end-to-end check that the harness,
//! the oracle linkage and the corpus paths are all correct.
//!
//! Scalar versus SIMD is available two ways, and they answer different questions.
//!
//! *Backend against backend, in one binary* -- what the `adler32_backends` and `crc32_backends`
//! groups do. Without the feature they compare the always-present engines; with it they add the
//! vectorized one, so one run shows all of them side by side:
//!
//! ```text
//! cargo bench --manifest-path benches/Cargo.toml --bench checksum_bench --features simd
//! ```
//!
//! *Configuration against configuration, across two runs* -- what criterion's baselines are
//! for. This is the form to use when the question is what the shipped library does, because it
//! measures the exported C ABI rather than a named type:
//!
//! ```text
//! cargo bench --manifest-path benches/Cargo.toml --bench checksum_bench -- --save-baseline scalar
//! cargo bench --manifest-path benches/Cargo.toml --bench checksum_bench --features simd \
//!     -- --baseline-lenient scalar
//! ```
//!
//! The second command prints a `change:` line per row against the first. Two details of it are
//! load-bearing, and both were arrived at by measurement rather than by reading the help text.
//!
//! ★ It works at all only because the benchmark **ids are stable across configurations**.
//! criterion keys a baseline by id, so folding the feature set into a row label makes the second
//! run abort with "Baseline 'scalar' must exist before comparison is allowed". That is why the
//! configuration is not in the labels; see [`PORT_LABEL`].
//!
//! ★ `--baseline-lenient`, not `--baseline`. The two backend groups legitimately gain rows under
//! `--features simd` -- `adler32_backends/simd/*` and `crc32_backends/simd/*` cannot exist in a
//! scalar build, because the types they name are behind the feature -- and those ids have no
//! counterpart in the scalar baseline. `--baseline` treats a missing baseline as a hard error and
//! panics on the first such row; `--baseline-lenient` compares every row that has a counterpart and
//! runs the rest normally, which is precisely the intended behaviour here.
//!
//! Name the baseline after the configuration, as above, and it lands as the last path component
//! under `target/criterion/<group>/<id>/`, so saved data stays self-describing.
//!
//! Either way no number is unlabelled. Within one run, the banner on stderr names the backend
//! configuration, the `ZLIB_RS_SIMD` environment toggle and the CPU features detected at run time,
//! and the two backend groups label their rows `generic`, `braid`, `simd` and `dispatch` outright.
//! Across runs, the baseline name carries it. A reader never has to guess which backend produced a
//! row.
//!
//! `ZLIB_RS_SIMD` is read by `crates/libz-rs-sys/build.rs` and cannot by itself select a
//! backend: cargo resolves features before any build script runs, so that variable reconciles a
//! caller's request against whatever the `simd` feature resolved to and fails the build on a
//! contradiction. The banner reports it so that a mismatch between what a developer asked for
//! and what got compiled is visible in the output rather than only in the build log.
//!
//! # Code generation
//!
//! Cargo's `bench` profile inherits `[profile.release]`, which the repository root sets to
//! `lto = "fat"`, `codegen-units = 1` and `opt-level = 3`, so these measurements are taken
//! against the same code generation as the shipped library. `panic = "abort"` is ignored for
//! bench targets -- cargo does not honour the key there -- which is expected and harmless.
//! Profiles are honoured only in the workspace root, so nothing here declares one and none is
//! needed. `-Cllvm-args=-enable-dfa-jump-thread` is an opt-in `RUSTFLAGS` lever for performance
//! runs and is deliberately not committed to any manifest.
//!
//! # Inputs
//!
//! Two sources, and the split is deliberate.
//!
//! The **length sweep** is synthesised in-process by the xorshift64 generator below, from one
//! hard-coded seed, because the interesting lengths are properties of the two algorithms rather
//! than of any file: no committed fixture is 5551, 5552 or 5553 bytes long, and the corpus is
//! not this suite's to extend. `corpus/README.md` forbids synthesising *fixture* bytes, and the
//! reason it gives is that "a 'pass' over whatever bytes today's run produced is a weaker claim
//! than the one being made" -- a statement about correctness claims. Nothing here claims
//! correctness: the numbers are rates, both implementations see the identical buffer, and a
//! fixed seed makes the buffer identical from run to run, which is exactly the reproducibility
//! that rule is protecting.
//!
//! The **fixture cases** read the committed tier-1 corpus, so the rates are also reported over
//! real data of the kinds the port meets in practice. A checksum's cost is content-insensitive,
//! so length is the primary axis and the fixtures are realistic secondary cases rather than the
//! main event. A missing fixture logs once and is skipped; nothing here downloads anything.
//!
//! Silesia is deliberately **not** consulted. `corpus/README.md` names its two consumers --
//! `benches/deflate_bench.rs` and `benches/inflate_bench.rs` -- and a checksum measured over
//! several hundred megabytes of third-party data says nothing the 1 MiB steady-state case does
//! not already say. `fetch_silesia.sh` is invoked in FETCHING mode by a human and by nothing else;
//! the only automated invocation of it anywhere is `rust.yml`'s `--verify-only` check, which
//! obtains nothing and does not concern this suite.
//!
//! ★ For a `[[bench]]` target `CARGO_MANIFEST_DIR` expands to the **host package's** directory,
//! not to the file's own -- and the host package of this suite is `benches/Cargo.toml`, the
//! excluded package `zlib-rs-benches`, which declares the `[[bench]]` entry that names this file.
//! So the value is the `benches/` directory itself, the repository root is one component up, and
//! the committed corpus hangs off the differential crate that owns it. The one path derived from
//! that is named once, in [`minimal_corpus_dir`], so that a future case reads it there rather than
//! re-deriving it.
//!
//! # Hygiene
//!
//! `crates/zlib-rs-differential` is dev-only and appears in neither shipped crate's
//! `[dependencies]` (AAP §0.6.4.1), and it is also where every `unsafe` this file's measurements
//! need actually lives: its `port` and `oracle` modules hold one gate per entry point, each owning
//! the single documented `unsafe` block and the invariant that discharges it. This file's root
//! carries `#![forbid(unsafe_code)]`, so its own freedom from `unsafe` is compiler-enforced.
//! `extern "C-unwind"` appears nowhere and nothing here is `#[no_mangle]`.
//!
//! A `harness = false` bench compiles without `--test`, so `cfg(test)` is false here and these
//! functions are not `#[test]`. `clippy.toml`'s `allow-unwrap-in-tests`,
//! `allow-expect-in-tests` and `allow-panic-in-tests` therefore do not reach this file and the
//! workspace's denied panic family is fully in force. Nothing below unwraps, expects, panics or
//! indexes: a fallible step logs to stderr and returns early, which is also the behaviour a
//! benchmark should have.

// Every FFI call this file makes goes through a gate in `crate::port` or `oracle`, so nothing here
// needs `unsafe` and the compiler is asked to keep it that way.
#![forbid(unsafe_code)]

use std::fmt;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
#[cfg(feature = "simd")]
use std::time::{Duration, Instant};

use criterion::measurement::WallTime;
use criterion::{
    criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion, Throughput,
};

use libz_rs_sys::{uInt, uLong, z_off64_t, z_off_t};
use zlib_rs::adler32::{Adler32Backend, Adler32Generic, ADLER32_INITIAL_VALUE, NMAX};
use zlib_rs::crc32::tables::{N, W};
use zlib_rs::crc32::{Braid, Crc32Backend, Generic};
// The two sides of the harness FFI boundary: `oracle` for the C reference's `extern "C"`
// declarations, `port` for the facade's. Every `unsafe` call either measurement makes lives in one
// of those two files and nowhere else.
use zlib_rs_differential::{oracle, port};

#[cfg(feature = "simd")]
use zlib_rs::adler32::Adler32Simd;
#[cfg(feature = "simd")]
use zlib_rs::crc32::StrideBraid;

// =============================================================================
//  Labels
// =============================================================================

/// Row label for the C reference implementation.
///
/// The oracle is the unmodified in-tree C sources with every export renamed to a `c_` prefix by
/// `crates/zlib-rs-differential/build.rs`, so a row under this name is `adler32.c` or `crc32.c`
/// itself and not a second transcription of it.
const ORACLE_LABEL: &str = "c-oracle";

/// Row label for the port. Stable across configurations, **deliberately**.
///
/// ★ Do not fold [`PORT_CONFIGURATION`] into this string, however tempting it is. criterion keys a
/// saved baseline by benchmark id, so a label that changed with the feature set would make the two
/// runs of the scalar-versus-SIMD procedure non-comparable, and the second one would not degrade
/// gracefully -- it aborts. Measured, with the configuration in the label:
///
/// ```text
/// thread 'main' panicked at criterion-0.8.2/src/analysis/mod.rs:56:13:
/// Baseline 'scalar' must exist before comparison is allowed; try --save-baseline scalar
/// ```
///
/// The configuration is reported instead in the three places where it does not break anything:
/// the startup banner on every run, the criterion baseline name the documentation tells a reader
/// to use (which becomes the last path component under `target/criterion/<group>/<id>/`, so saved
/// data stays self-describing), and the explicit `generic` / `braid` / `simd` / `dispatch` row
/// labels of the two backend groups, which are within-run comparisons and therefore free to name
/// an engine.
///
/// Those backend labels are also why the second run of the two-run procedure needs
/// `--baseline-lenient` rather than `--baseline`: they are the one place where the *set* of ids
/// legitimately differs between configurations. See the module documentation.
const PORT_LABEL: &str = "zlib-rs";

/// The backend configuration this binary was compiled with: `"scalar"` or `"simd"`.
///
/// AAP §0.5.1.4 makes `simd` a build-time feature that is off by default, so which engine the
/// exported `adler32_z`/`crc32_z` dispatch to is fixed when this file is compiled and cannot be
/// recovered from a rate. This is the string the banner prints and the name to pass to
/// `--save-baseline`.
const PORT_CONFIGURATION: &str = if cfg!(feature = "simd") {
    "simd"
} else {
    "scalar"
};

// =============================================================================
//  Seeds and lengths
// =============================================================================

/// The Adler-32 starting value, `1`.
///
/// `zlib.h` L1812-L1814 documents `adler32(0L, Z_NULL, 0)` as returning "the required initial
/// value for the checksum", and RFC 1950 fixes it: `s1` starts at 1 and `s2` at 0, which packs
/// to `1`. Held equal to the port's own constant by the assertion below rather than by comment,
/// so a drift in either is a build failure.
const ADLER32_SEED: uLong = 1;

const _: () = assert!(
    ADLER32_INITIAL_VALUE == 1,
    "zlib.h L1812-L1814: adler32(0L, Z_NULL, 0) is the initial value 1"
);

/// The CRC-32 starting value, `0`.
///
/// `zlib.h` L1851-L1852 documents `crc32(0L, Z_NULL, 0)` as returning the initial value. The
/// leading and trailing one's complements of `crc32.c` L635 and L940 are applied inside the
/// entry point, so the value a caller passes in and reads out is the plain zero-seeded one.
const CRC32_SEED: uLong = 0;

/// The default braided stride of `crc32.c`: `N * W` bytes.
///
/// `crc32.c` L64 sets `N` to 5 (L66-L68 rejects anything outside 1..6) and L85-L101 sets `W` to 8
/// where a 64-bit word type exists and 4 otherwise, so this is 40 bytes on the Tier-1 64-bit
/// targets and 20 elsewhere. Both numbers are taken from the port's own `crc32::tables`, which
/// mirrors that selection, rather than hardcoded -- a hardcoded 40 would measure the wrong
/// boundary on a target that compiled the narrow word.
const BRAID_STRIDE: usize = N * W;

/// The length at or above which `crc32.c`'s braided body runs at all.
///
/// `crc32.c` L640 tests `len >= N * W + W - 1` and hands anything shorter to the byte-at-a-time
/// path, so this is the exact point where the two engines swap over -- 47 bytes where `W` is 8.
const BRAID_ENTRY_LEN: usize = BRAID_STRIDE + W - 1;

/// The synthetic length sweep. Every entry earns its place.
///
/// * `1` -- `adler32.c` L69-L70 carries an explicit single-byte fast path, added because "in
///   case user likes doing a byte at a time, keep it fast". A caller streaming one byte per call
///   is a real caller, and it is the length least able to absorb dispatch overhead.
/// * `16` -- the exact boundary of `adler32.c` L85's `len < 16` short path, and therefore the
///   shortest input that reaches the `DO16` unrolling of L14-L18 (applied at L101 and L112). Also
///   below every vectorized lane threshold, so it measures the scalar tail both engines share.
/// * [`BRAID_STRIDE`] -- `crc32.c`'s braided stride, `N * W`; the point at which one full
///   braided iteration first fits.
/// * [`BRAID_ENTRY_LEN`] -- `crc32.c` L640's `N * W + W - 1` swap-over between the braided body
///   and the byte-at-a-time path. A sweep that misses this measures one engine twice.
/// * `1024` -- a kilobyte: the smallest length where per-call overhead is comfortably amortized,
///   and a common chunk size for callers feeding a stream.
/// * `NMAX - 1`, `NMAX`, `NMAX + 1` -- `adler32.c` L11-L12 fixes `NMAX` at 5552 as "the largest
///   n such that 255n(n+1)/2 + (n+1)(BASE-1) <= 2^32-1", so the modulo schedule changes across
///   exactly this boundary: one reduction below it, two at and above. Straddling it is the only
///   way to see the second reduction appear.
/// * `65536` -- eleven whole `NMAX` blocks plus a 4464-byte remainder, so both `adler32.c`'s block
///   loop (L97-L106) and its tail (L109-L121) run: steady state with the reduction cost amortized,
///   and larger than the 32 KiB window, so it is representative of bulk stream traffic.
/// * `1 << 20` -- a mebibyte: the bulk row, long enough that per-call cost, the reduction
///   schedule and the loop prologue are all amortized away and what remains is the steady-state
///   inner loop. Whether it also exceeds the machine's last-level cache depends on the machine and
///   is not asserted here; on a host with a multi-megabyte cache this row is warm, and the
///   comparison is still apples to apples because both implementations read the same allocation.
const SIZE_SWEEP: [usize; 10] = [
    1,
    16,
    BRAID_STRIDE,
    BRAID_ENTRY_LEN,
    1024,
    NMAX - 1,
    NMAX,
    NMAX + 1,
    65_536,
    1 << 20,
];

/// The three lengths the backend-against-backend groups use.
///
/// * `64` -- the lane threshold the port's Adler-32 dispatcher tests before it consults
///   target-feature detection at all (`crates/zlib-rs/src/adler32/mod.rs`), so it is the
///   shortest input on which the vectorized arrangement is asked to do any work.
/// * `4096` -- a page: long enough for a vectorized loop to amortize its reductions, short
///   enough to stay in L1.
/// * `1 << 20` -- steady state, as in [`SIZE_SWEEP`].
///
/// Three lengths rather than ten because this comparison is between engines at a fixed scale,
/// and the scale sweep is already covered above.
const BACKEND_SWEEP: [usize; 3] = [64, 4096, 1 << 20];

/// The length used for the single `uInt`-forwarder measurement.
///
/// One kilobyte: large enough that the forwarding itself cannot show up as a rate difference,
/// small enough to cost nothing to run.
const FORWARDER_LEN: usize = 1024;

/// The committed tier-1 fixtures this suite reads, from `corpus/README.md`'s inventory.
///
/// The four `corpus/README.md` singles out for compression behaviour are all here, plus the
/// large one:
///
/// * `repetitive.bin` (16 KiB) -- highly compressible, and the largest of the four.
/// * `random.bin` (8 KiB) -- incompressible.
/// * `text.txt` (45 bytes) -- natural-language text; short enough to exercise the tail paths.
/// * `binary.bin` (23 bytes) -- binary data; shorter still.
/// * `window_boundary.bin` (65 KiB) -- the only committed fixture past the 32 KiB window, and
///   therefore the only real-data case that reaches steady state.
///
/// `empty.bin` is deliberately absent: a zero-byte case would give criterion a
/// `Throughput::Bytes(0)` to divide by. Its behaviour -- an empty non-null buffer is an ordinary
/// zero-length update and is *not* the `Z_NULL` seed request -- is contract, tested where
/// contract is tested, and not a rate.
const FIXTURE_NAMES: [&str; 5] = [
    "repetitive.bin",
    "random.bin",
    "text.txt",
    "binary.bin",
    "window_boundary.bin",
];

/// Seed for the synthetic length sweep: the 64-bit multiplier from Marsaglia's xorshift paper,
/// used here only as an arbitrary fixed nonzero constant.
///
/// Any nonzero value would do -- a xorshift generator's only requirement is that its state never
/// be zero, since zero is a fixed point. What matters is that the value is *written down*, so
/// that two runs of this suite, and a run on another machine, see byte-for-byte the same input.
const SWEEP_SEED: u64 = 0x2545_F491_4F6C_DD1D;

// =============================================================================
//  Input generation -- deterministic, dependency-free
// =============================================================================

/// `len` bytes from a xorshift64 sequence started at [`SWEEP_SEED`].
///
/// Marsaglia's `13 / 7 / 17` shift triple: a full 2^64 - 1 period in three shifts and three
/// exclusive-ors, and no crate. AAP §0.7.1(i) keeps third-party crates out of this workspace
/// beyond the dev tooling already pinned, and `rand` would be a new one for no benefit. The output
/// only has to be *fixed* and *not degenerate* -- a checksum's cost does not depend on its input's
/// entropy, so this is a length generator that happens to produce unpredictable-looking bytes, not
/// a randomness source.
///
/// Bytes are taken eight at a time through [`u64::to_le_bytes`], so the sequence is identical on
/// a big-endian target and no narrowing cast appears anywhere. An over-long final block is
/// truncated, which means `len` and `len + 1` share a prefix -- exactly what is wanted when the
/// point of the sweep is to straddle a boundary.
fn sweep_bytes(len: usize) -> Vec<u8> {
    let mut state = SWEEP_SEED;
    let mut out = Vec::with_capacity(len.saturating_add(8));

    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);

    out
}

/// The committed tier-1 corpus directory, `crates/zlib-rs-differential/corpus/minimal`.
///
/// ★ `CARGO_MANIFEST_DIR` is the **host package's** directory for a bench target, and the host
/// package of this file is `benches/` -- the excluded package whose manifest carries the
/// `[[bench]]` entry that attaches it -- so the repository root is one component up and the corpus
/// hangs off the differential crate, which owns the fixtures and documents them in
/// `corpus/README.md`. It was `<CARGO_MANIFEST_DIR>/corpus/minimal` while these suites were
/// attached to that crate; the path moved with the hosting.
///
/// Never an absolute path baked into the source, and never derived from the current directory,
/// which cargo does not guarantee for a bench binary.
fn minimal_corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("crates")
        .join("zlib-rs-differential")
        .join("corpus")
        .join("minimal")
}

/// Every fixture from [`FIXTURE_NAMES`] that is present, loaded exactly once.
///
/// Absence is not an error. A fixture that cannot be read is reported on stderr and left out of
/// the returned set, so the run continues and the remaining cases still produce numbers; a
/// benchmark that aborted over one optional input would tell a developer nothing about all the
/// cases that were fine. The [`OnceLock`] is what makes "reported once" literal -- both the
/// Adler-32 and the CRC-32 fixture group ask for this set, and neither should reprint the
/// diagnostic.
///
/// Nothing here reaches the network. Tier 2 (Silesia) is not consulted at all; see the module
/// documentation.
fn fixtures() -> &'static [(&'static str, Vec<u8>)] {
    static CACHE: OnceLock<Vec<(&'static str, Vec<u8>)>> = OnceLock::new();

    CACHE.get_or_init(|| {
        let dir = minimal_corpus_dir();
        let mut loaded = Vec::with_capacity(FIXTURE_NAMES.len());

        for name in FIXTURE_NAMES {
            let path = dir.join(name);
            match fs::read(&path) {
                Ok(bytes) if bytes.is_empty() => {
                    eprintln!(
                        "checksum_bench: skipping fixture {name}: {} is empty, and a zero-byte \
                         case has no rate to report",
                        path.display()
                    );
                }
                Ok(bytes) => loaded.push((name, bytes)),
                Err(error) => {
                    eprintln!(
                        "checksum_bench: skipping fixture {name}: cannot read {}: {error}. \
                         The synthetic length sweep is unaffected; nothing is downloaded.",
                        path.display()
                    );
                }
            }
        }

        if loaded.is_empty() {
            eprintln!(
                "checksum_bench: no committed fixture could be read from {}. Only the synthetic \
                 length sweep and the backend comparison will report numbers.",
                dir.display()
            );
        }

        loaded
    })
}

// =============================================================================
//  Oracle and facade call gates
// =============================================================================
//
//  Every FFI call in this file goes through one of the gates below, and NO gate contains
//  `unsafe`: this file's root carries `#![forbid(unsafe_code)]`, and each gate delegates to the
//  matching safe wrapper in `crates/zlib-rs-differential/src` -- `port` for the facade, `oracle`
//  for the C reference -- which is where the single documented `unsafe` block per entry point
//  lives together with the invariant that discharges it.
//
//  The gates below therefore exist for two reasons that survive that move. They keep each
//  measurement's *shape* in one place, so the port row and the reference row of a comparison pay
//  for exactly the same conversions; and they keep the `uInt` bounds test of the forwarder pair
//  out of the timed closure's control flow. A benchmark that let one side convert and the other
//  not would report the conversion as a throughput difference.
//
//  The `uLong` a gate returns is `core::ffi::c_ulong` on both sides -- `crates/libz-rs-sys`'s
//  `types.rs` and the oracle's own aliases both track `unsigned long` rather than a fixed width,
//  which is what keeps `total_in`, `total_out` and every checksum return value correct across
//  LP64 and LLP64. The two aliases are therefore the same type and a comparison between them
//  compiles; should they ever diverge on some target, that comparison becomes a compile error,
//  which is the outcome to want.

/// `adler32_z` through the exported C ABI (`zlib.h` L1829-L1830, ported from `adler32.c` L61).
///
/// No length conversion: `z_size_t` is `size_t`, and `types.rs` pins its equality with `usize` at
/// compile time, so the slice's own length is the argument.
fn port_adler32_z(adler: uLong, buf: &[u8]) -> uLong {
    // The slice crosses the boundary whole, so the length cannot disagree with the pointer and
    // no conversion happens on the way in.
    port::adler32_z(adler, buf)
}

/// `c_adler32_z` -- the C reference's `adler32_z` (`adler32.c` L61-L125).
fn oracle_adler32_z(adler: oracle::uLong, buf: &[u8]) -> oracle::uLong {
    // As `port_adler32_z`, against `adler32.c` L61-L125, which only reads the buffer. The
    // `extern "C"` declaration it reaches carries the C prototype verbatim, and this crate's
    // `build.rs` is what links the archive that defines it.
    oracle::adler32_z(adler, buf)
}

/// `crc32_z` through the exported C ABI (`zlib.h` L1866-L1867, ported from `crc32.c` L508/L626).
///
/// The reference has two mutually exclusive definitions of this function -- the braided,
/// word-at-a-time body at `crc32.c` L508 when a word type is available and the byte-at-a-time one
/// at L626 otherwise -- and the port mirrors the choice with its `Braid` and `Generic` backends.
fn port_crc32_z(crc: uLong, buf: &[u8]) -> uLong {
    // As `port_adler32_z`: the slice crosses whole and unconverted.
    port::crc32_z(crc, buf)
}

/// `c_crc32_z` -- the C reference's `crc32_z` (`crc32.c` L508 or L626, whichever `W` selected).
fn oracle_crc32_z(crc: oracle::uLong, buf: &[u8]) -> oracle::uLong {
    // As `port_crc32_z`; both C bodies -- braided and byte-at-a-time -- only read the buffer.
    oracle::crc32_z(crc, buf)
}

/// `adler32` -- the `uInt`-length forwarder (`zlib.h` L1809, ported from `adler32.c` L128-L130).
///
/// `None` when `buf` is longer than a `uInt` can express, which is what the `_z` form exists to
/// avoid. The conversion sits inside this gate rather than at the call site so that both this and
/// [`oracle_adler32`] pay for it identically and the comparison stays fair; at
/// [`FORWARDER_LEN`] it is one bounds test against a checksum over a kilobyte.
fn port_adler32(adler: uLong, buf: &[u8]) -> Option<uLong> {
    // The bounds test stays here rather than in the gate, and it is what this row measures
    // against the `_z` form: `adler32` takes a `uInt` length, so a slice that cannot be expressed
    // in one has no answer. Having established that it can, the gate's own narrowing is exact.
    let _len = uInt::try_from(buf.len()).ok()?;

    Some(port::adler32(adler, buf))
}

/// `c_adler32` -- the C reference's `uInt`-length forwarder (`adler32.c` L128-L130).
fn oracle_adler32(adler: oracle::uLong, buf: &[u8]) -> Option<oracle::uLong> {
    // As `port_adler32`, including where the bounds test sits, so that both rows of the
    // comparison pay for exactly the same conversion.
    let _len = oracle::uInt::try_from(buf.len()).ok()?;

    Some(oracle::adler32(adler, buf))
}

/// `crc32` -- the `uInt`-length forwarder (`zlib.h` L1848, ported from `crc32.c` L946-L951).
fn port_crc32(crc: uLong, buf: &[u8]) -> Option<uLong> {
    // As `port_adler32`.
    let _len = uInt::try_from(buf.len()).ok()?;

    Some(port::crc32(crc, buf))
}

/// `c_crc32` -- the C reference's `uInt`-length forwarder (`crc32.c` L946-L951).
fn oracle_crc32(crc: oracle::uLong, buf: &[u8]) -> Option<oracle::uLong> {
    // As `oracle_adler32`.
    let _len = oracle::uInt::try_from(buf.len()).ok()?;

    Some(oracle::crc32(crc, buf))
}

// The seven combine gates below take no pointer at all: every argument is a by-value integer and
// every result is one. Their `unsafe` -- now discharged in `oracle` rather than here -- is the
// irreducible kind: calling a foreign function whose Rust declaration is asserted, and not
// checked, to match its C prototype. The port's own combine entry points are safe
// `extern "C" fn`s and need no gate at all, which is exactly the asymmetry a hand-written
// `extern` block introduces and a reason to keep those declarations in one audited file.
//
// `adler32_combine_` (`adler32.c` L133-L155) backs both Adler-32 forms and `x2nmodp`/`multmodp`
// (`crc32.c` L184, L163) back all five CRC-32 ones, so each pair cannot disagree with itself; what
// is being compared is the port against the reference.

/// `c_adler32_combine` -- the C reference's narrow combine (`adler32.c` L158-L160).
fn oracle_adler32_combine(
    adler1: oracle::uLong,
    adler2: oracle::uLong,
    len2: oracle::z_off_t,
) -> oracle::uLong {
    // Three by-value integers in, one out. A negative `len2` is well defined rather than
    // undefined: `adler32.c` L134-L137 returns `0xffffffff`.
    oracle::adler32_combine(adler1, adler2, len2)
}

/// `c_adler32_combine64` -- the C reference's wide combine (`adler32.c` L162-L164).
fn oracle_adler32_combine64(
    adler1: oracle::uLong,
    adler2: oracle::uLong,
    len2: oracle::z_off64_t,
) -> oracle::uLong {
    // As `oracle_adler32_combine`, with the 64-bit length.
    oracle::adler32_combine64(adler1, adler2, len2)
}

/// `c_crc32_combine` -- the C reference's narrow combine (`crc32.c` L981-L983).
fn oracle_crc32_combine(
    crc1: oracle::uLong,
    crc2: oracle::uLong,
    len2: oracle::z_off_t,
) -> oracle::uLong {
    // By-value integers only. The callee reads `x2n_table`, which has static storage duration.
    oracle::crc32_combine(crc1, crc2, len2)
}

/// `c_crc32_combine64` -- the C reference's wide combine (`crc32.c` L976-L978).
fn oracle_crc32_combine64(
    crc1: oracle::uLong,
    crc2: oracle::uLong,
    len2: oracle::z_off64_t,
) -> oracle::uLong {
    // As `oracle_crc32_combine`, with the 64-bit length.
    oracle::crc32_combine64(crc1, crc2, len2)
}

/// `c_crc32_combine_gen` -- the C reference's operator generator (`crc32.c` L964-L966).
fn oracle_crc32_combine_gen(len2: oracle::z_off_t) -> oracle::uLong {
    // One by-value integer in, one out.
    oracle::crc32_combine_gen(len2)
}

/// `c_crc32_combine_gen64` -- the wide operator generator (`crc32.c` L954-L956).
fn oracle_crc32_combine_gen64(len2: oracle::z_off64_t) -> oracle::uLong {
    // As `oracle_crc32_combine_gen`, with the 64-bit length.
    oracle::crc32_combine_gen64(len2)
}

/// `c_crc32_combine_op` -- applies a precomputed operator (`crc32.c` L969-L971).
fn oracle_crc32_combine_op(
    crc1: oracle::uLong,
    crc2: oracle::uLong,
    op: oracle::uLong,
) -> oracle::uLong {
    // Three by-value integers in, one out. `op` is opaque to this caller and is only ever a
    // value `crc32_combine_gen` produced.
    oracle::crc32_combine_op(crc1, crc2, op)
}

/// `c_get_crc_table` -- the reference's table accessor (`crc32.c` L482-L487).
///
/// The pointer is returned rather than dereferenced: what is being measured is the cost of the
/// accessor, not of a read through it. `oracle::crc_table` is the gate, and taking `as_ptr` of the
/// `'static` slice it returns costs nothing at run time -- `slice::from_raw_parts` over a constant
/// length compiles away -- so what the row times is `crc32.c` L482-L487 and the call to it.
fn oracle_get_crc_table() -> *const oracle::z_crc_t {
    oracle::crc_table().as_ptr()
}

// =============================================================================
//  Measurement guards
// =============================================================================

/// Report whether the port and the reference agree, and say so loudly when they do not.
///
/// Called once per case, **outside** every timed closure -- a comparison inside `Bencher::iter`
/// would be measured as part of the checksum. On disagreement this prints the algorithm, the case
/// and both values and returns `false`, and the caller skips the case rather than publishing a
/// rate for a wrong answer.
///
/// It is deliberately not an assertion. Byte identity and table transcription are gated under
/// `cargo test` by `crates/zlib-rs-differential/tests/byte_identical.rs` and
/// `tests/table_equality.rs`, where a failure should stop the run; a benchmark that aborted on
/// case three would hide the other ninety.
fn agrees<C: fmt::Display>(algorithm: &str, case: C, port: uLong, reference: uLong) -> bool {
    if port == reference {
        return true;
    }

    eprintln!(
        "checksum_bench: SKIPPING {algorithm} [{case}]: the port returned {port:#x} and the C \
         oracle returned {reference:#x}. A throughput number for a wrong value is worse than \
         none, so this case is not measured. Correctness is gated by \
         crates/zlib-rs-differential/tests/, not here."
    );

    false
}

/// Convert a byte count for [`Throughput::Bytes`], reporting rather than panicking on the
/// impossible.
///
/// Every length in [`SIZE_SWEEP`] and every committed fixture is far below `u64::MAX`, so this
/// cannot fail on any target Rust supports. It is written fallibly because `usize` is not
/// `u64` by definition and because the workspace denies `unwrap`: a benchmark has a correct
/// answer for an unrepresentable length, and it is to say so and move on.
fn throughput_bytes(len: usize) -> Option<Throughput> {
    let Ok(bytes) = u64::try_from(len) else {
        eprintln!(
            "checksum_bench: skipping a {len}-byte case: the length does not fit a u64 and so has \
             no expressible throughput."
        );
        return None;
    };

    Some(Throughput::Bytes(bytes))
}

// =============================================================================
//  Configuration banner
// =============================================================================

/// Print the compiled configuration and the detected CPU features once, to stderr.
///
/// Every group calls this; [`Once`] makes it happen exactly one time however the run is filtered.
/// stderr rather than stdout because criterion's machine-readable output goes to stdout and must
/// not be polluted.
///
/// The point is that no row in the output is ambiguous. `simd` is resolved at compile time, so a
/// rate on its own does not say which engine produced it; between this banner and [`PORT_LABEL`]
/// a reader always knows.
fn report_configuration() {
    static BANNER: Once = Once::new();

    BANNER.call_once(|| {
        eprintln!("checksum_bench: measuring the Rust port against the in-process C oracle.");
        eprintln!(
            "checksum_bench:   port rows are labelled {PORT_LABEL:?}, reference rows \
             {ORACLE_LABEL:?}"
        );
        eprintln!(
            "checksum_bench:   cargo feature \"simd\": {}",
            if cfg!(feature = "simd") {
                "ENABLED -- vectorization-friendly Adler-32 and CRC-32 backends compiled in"
            } else {
                "disabled (the default) -- scalar backends only"
            }
        );
        // The row labels are stable across configurations on purpose -- see `PORT_LABEL` -- so this
        // is the line that says which backend produced the numbers below, and the name to save a
        // baseline under so that the answer survives the run.
        eprintln!(
            "checksum_bench:   BACKEND CONFIGURATION: {PORT_CONFIGURATION}. Save it as \
             `-- --save-baseline {PORT_CONFIGURATION}` so the configuration is recorded in the \
             criterion path rather than only here."
        );

        // `ZLIB_RS_SIMD` is a build-time consistency check in crates/libz-rs-sys/build.rs, not a
        // selector: cargo resolves features before it runs a build script, so a build script
        // cannot enable one. Reporting it here makes a disagreement between what was asked for
        // and what was compiled visible in the measurement output rather than only in a build log.
        match option_env!("ZLIB_RS_SIMD") {
            Some(value) => eprintln!(
                "checksum_bench:   ZLIB_RS_SIMD={value:?} at compile time (a consistency check \
                 against the feature above, never a second selector)"
            ),
            None => eprintln!("checksum_bench:   ZLIB_RS_SIMD unset at compile time"),
        }

        eprintln!(
            "checksum_bench:   target {}, CPU features detected at run time: {}",
            std::env::consts::ARCH,
            detected_cpu_features()
        );
        eprintln!(
            "checksum_bench:   Adler-32 NMAX={NMAX} (adler32.c L11); CRC-32 braid N={N} W={W}, \
             stride {BRAID_STRIDE}, braided body from {BRAID_ENTRY_LEN} bytes (crc32.c L64, \
             L85-L101, L640)"
        );
    });
}

/// The vector extensions this machine reports, as a comma-separated list.
///
/// Correctness never depends on any of them: both of the port's vectorized backends are portable
/// fixed-width integer arithmetic that LLVM is free to autovectorize, with no architecture
/// intrinsic to guard, so an enabled build computes the right answer on a machine with no vector
/// unit at all. The list is here to explain a *rate*, which is the only thing detection can
/// affect -- the port's Adler-32 dispatcher consults `is_supported()` as a throughput hint, and
/// its CRC-32 dispatcher performs no detection whatsoever.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detected_cpu_features() -> String {
    let mut present = Vec::new();

    for (name, detected) in [
        ("sse2", std::arch::is_x86_feature_detected!("sse2")),
        ("ssse3", std::arch::is_x86_feature_detected!("ssse3")),
        ("sse4.2", std::arch::is_x86_feature_detected!("sse4.2")),
        (
            "pclmulqdq",
            std::arch::is_x86_feature_detected!("pclmulqdq"),
        ),
        ("avx2", std::arch::is_x86_feature_detected!("avx2")),
    ] {
        if detected {
            present.push(name);
        }
    }

    if present.is_empty() {
        return String::from("none of sse2/ssse3/sse4.2/pclmulqdq/avx2");
    }

    present.join(",")
}

/// The vector extensions this machine reports; see the `x86` variant for why they are only ever
/// an explanation of a rate.
#[cfg(target_arch = "aarch64")]
fn detected_cpu_features() -> String {
    let mut present = Vec::new();

    for (name, detected) in [
        ("neon", std::arch::is_aarch64_feature_detected!("neon")),
        ("crc", std::arch::is_aarch64_feature_detected!("crc")),
        ("aes", std::arch::is_aarch64_feature_detected!("aes")),
    ] {
        if detected {
            present.push(name);
        }
    }

    if present.is_empty() {
        return String::from("none of neon/crc/aes");
    }

    present.join(",")
}

/// The vector extensions this machine reports; see the `x86` variant for why they are only ever
/// an explanation of a rate.
///
/// The standard library offers no stable run-time detection macro outside x86 and AArch64, and
/// inventing one would be a portability hazard for a diagnostic line. Compile-time
/// `target_feature` state is the honest answer on such a target.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn detected_cpu_features() -> String {
    String::from(
        "run-time detection unavailable on this architecture; the backends are portable \
         integer arithmetic and require none",
    )
}

// =============================================================================
//  Throughput: the exported C ABI against the C reference
// =============================================================================

/// The `u32` Adler-32 starting value, as the safe core spells it.
///
/// The same value as [`ADLER32_SEED`]; the two differ only in width, because the ABI carries a
/// `uLong` and the core carries the checksum's own 32 bits.
const ADLER32_START: u32 = ADLER32_INITIAL_VALUE;

/// The `u32` CRC-32 starting value, as the safe core spells it.
const CRC32_START: u32 = 0;

/// Time the port and the reference over one synthetic buffer per length.
///
/// Both implementations see the *identical* buffer and the identical starting value, and the two
/// answers are compared before either is timed, so a rate is only ever published for a case where
/// the port and the reference agree. Feeding both from one allocation is what AAP §0.6.4.6 means
/// by measuring in one process: the alternative -- two runs, two buffers, two page-cache states --
/// puts noise into a ratio that is only useful if it is noise-free.
fn time_against_reference(
    group: &mut BenchmarkGroup<'_, WallTime>,
    algorithm: &str,
    seed: uLong,
    port: fn(uLong, &[u8]) -> uLong,
    reference: fn(uLong, &[u8]) -> uLong,
    lengths: &[usize],
) {
    for &len in lengths {
        let data = sweep_bytes(len);

        let Some(throughput) = throughput_bytes(len) else {
            continue;
        };

        // Computed here, outside every timed closure: a comparison inside `Bencher::iter` would
        // be measured as part of the checksum and would corrupt the very number it guards.
        let port_value = port(seed, &data);
        let reference_value = reference(seed, &data);
        if !agrees(
            algorithm,
            format_args!("len={len}"),
            port_value,
            reference_value,
        ) {
            continue;
        }

        group.throughput(throughput);
        group.bench_with_input(BenchmarkId::new(PORT_LABEL, len), &data, |b, buffer| {
            b.iter(|| black_box(port(black_box(seed), black_box(buffer.as_slice()))));
        });
        group.bench_with_input(BenchmarkId::new(ORACLE_LABEL, len), &data, |b, buffer| {
            b.iter(|| black_box(reference(black_box(seed), black_box(buffer.as_slice()))));
        });
    }
}

/// Time the port and the reference over every committed fixture that is present.
///
/// A checksum's cost is content-insensitive, so these cases add realism rather than a new axis:
/// the sizes are whatever the corpus committed, and the point is that the rates hold over data of
/// the kinds the port actually meets. When no fixture can be read the group is simply empty, and
/// [`fixtures`] has already said why on stderr.
fn time_fixtures(
    group: &mut BenchmarkGroup<'_, WallTime>,
    algorithm: &str,
    seed: uLong,
    port: fn(uLong, &[u8]) -> uLong,
    reference: fn(uLong, &[u8]) -> uLong,
) {
    for (name, data) in fixtures() {
        let Some(throughput) = throughput_bytes(data.len()) else {
            continue;
        };

        // Outside every timed closure, as in `time_against_reference`.
        let port_value = port(seed, data);
        let reference_value = reference(seed, data);
        if !agrees(algorithm, name, port_value, reference_value) {
            continue;
        }

        group.throughput(throughput);
        group.bench_with_input(BenchmarkId::new(PORT_LABEL, name), data, |b, buffer| {
            b.iter(|| black_box(port(black_box(seed), black_box(buffer.as_slice()))));
        });
        group.bench_with_input(BenchmarkId::new(ORACLE_LABEL, name), data, |b, buffer| {
            b.iter(|| black_box(reference(black_box(seed), black_box(buffer.as_slice()))));
        });
    }
}

/// Adler-32 throughput across [`SIZE_SWEEP`], port against reference.
///
/// `adler32_z` is where the whole algorithm lives (`adler32.c` L61-L125): the single-byte fast
/// path at L70, the sub-16-byte loop at L85 and the `NMAX`-block engine at L97. The sweep is
/// chosen so that each of those, and the reduction boundary between them, is measured.
fn adler32_throughput(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("adler32_throughput");
    time_against_reference(
        &mut group,
        "adler32_z",
        ADLER32_SEED,
        port_adler32_z,
        oracle_adler32_z,
        &SIZE_SWEEP,
    );
    group.finish();
}

/// CRC-32 throughput across [`SIZE_SWEEP`], port against reference.
///
/// `crc32_z` has two mutually exclusive definitions in the reference and the sweep straddles the
/// point where they meet: the braided, word-at-a-time body of `crc32.c` L508 runs from
/// [`BRAID_ENTRY_LEN`] bytes and the byte-at-a-time body of L626 below it (L640). The port mirrors
/// the split with its `Braid` and `Generic` backends, which the `crc32_backends` group compares
/// directly.
fn crc32_throughput(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("crc32_throughput");
    time_against_reference(
        &mut group,
        "crc32_z",
        CRC32_SEED,
        port_crc32_z,
        oracle_crc32_z,
        &SIZE_SWEEP,
    );
    group.finish();
}

/// Adler-32 throughput over the committed fixtures.
fn adler32_fixtures(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("adler32_fixtures");
    time_fixtures(
        &mut group,
        "adler32_z",
        ADLER32_SEED,
        port_adler32_z,
        oracle_adler32_z,
    );
    group.finish();
}

/// CRC-32 throughput over the committed fixtures.
fn crc32_fixtures(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("crc32_fixtures");
    time_fixtures(
        &mut group,
        "crc32_z",
        CRC32_SEED,
        port_crc32_z,
        oracle_crc32_z,
    );
    group.finish();
}

/// Report agreement for a pair of `uInt`-length results, either of which may be absent.
///
/// `None` means the buffer was longer than a `uInt` -- the very case the `_z` forms exist to
/// avoid. It is reported and skipped rather than unwrapped.
fn narrow_results_agree(
    algorithm: &str,
    len: usize,
    port: Option<uLong>,
    reference: Option<uLong>,
) -> bool {
    let (Some(port_value), Some(reference_value)) = (port, reference) else {
        eprintln!(
            "checksum_bench: skipping {algorithm} [len={len}]: the length exceeds what a uInt can \
             express, which is exactly what the _z forms exist for."
        );
        return false;
    };

    agrees(
        algorithm,
        format_args!("len={len}"),
        port_value,
        reference_value,
    )
}

/// The two `uInt`-length entry points, at one representative size.
///
/// `adler32` (`adler32.c` L128-L130) and `crc32` (`crc32.c` L946-L951) are one-line forwarders to
/// their `_z` counterparts in the reference, and in the port the `uInt`/`z_size_t` distinction
/// disappears entirely once the buffer is a slice. Both are part of the frozen `zlib.h` surface,
/// so both are measured -- once each, because the only question is whether forwarding costs
/// anything, and one kilobyte answers it.
fn uint_forwarders(c: &mut Criterion) {
    report_configuration();

    let data = sweep_bytes(FORWARDER_LEN);
    let Some(throughput) = throughput_bytes(FORWARDER_LEN) else {
        return;
    };

    let mut group = c.benchmark_group("uint_forwarders");
    group.throughput(throughput);

    // Outside every timed closure.
    let adler_agrees = narrow_results_agree(
        "adler32",
        FORWARDER_LEN,
        port_adler32(ADLER32_SEED, &data),
        oracle_adler32(ADLER32_SEED, &data),
    );
    if adler_agrees {
        group.bench_with_input(
            BenchmarkId::new(PORT_LABEL, "adler32"),
            &data,
            |b, buffer| {
                b.iter(|| {
                    black_box(port_adler32(
                        black_box(ADLER32_SEED),
                        black_box(buffer.as_slice()),
                    ))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new(ORACLE_LABEL, "adler32"),
            &data,
            |b, buffer| {
                b.iter(|| {
                    black_box(oracle_adler32(
                        black_box(ADLER32_SEED),
                        black_box(buffer.as_slice()),
                    ))
                });
            },
        );
    }

    let crc_agrees = narrow_results_agree(
        "crc32",
        FORWARDER_LEN,
        port_crc32(CRC32_SEED, &data),
        oracle_crc32(CRC32_SEED, &data),
    );
    if crc_agrees {
        group.bench_with_input(BenchmarkId::new(PORT_LABEL, "crc32"), &data, |b, buffer| {
            b.iter(|| {
                black_box(port_crc32(
                    black_box(CRC32_SEED),
                    black_box(buffer.as_slice()),
                ))
            });
        });
        group.bench_with_input(
            BenchmarkId::new(ORACLE_LABEL, "crc32"),
            &data,
            |b, buffer| {
                b.iter(|| {
                    black_box(oracle_crc32(
                        black_box(CRC32_SEED),
                        black_box(buffer.as_slice()),
                    ))
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
//  Backend against backend, inside one binary
// =============================================================================
//
//  This is the scalar-versus-SIMD comparison in its direct form. It is possible because
//  `crates/zlib-rs` publishes its engines as named zero-sized types behind the `Adler32Backend`
//  and `Crc32Backend` traits (AAP §0.3.3.5) rather than hiding them behind the dispatcher, and
//  `Crc32Backend` even carries a `const NAME` whose documented purpose is to label a benchmark
//  row. `Adler32Generic`, `crc32::Generic` and `crc32::Braid` are present in every configuration;
//  `Adler32Simd` and `crc32::StrideBraid` exist only under the `simd` feature, so the rows they produce
//  appear only in a run that compiled them -- which is why [`PORT_LABEL`] and the banner name the
//  configuration.
//
//  The traits' contract is that every implementation returns the identical value for every input,
//  which is what makes vectorizing a check value admissible at all (AAP §0.8.2 ambiguity 9). Each
//  backend is therefore checked against the C reference before it is timed: a backend that has
//  become a *different function* rather than a faster one has no meaningful rate.

/// Fold `buf` through one named CRC-32 backend and return the value `crc32()` would.
///
/// `Crc32Backend::update` is handed pre-conditioned state and returns pre-conditioned state:
/// `crc32.c` L635 applies the leading `crc = (~crc) & 0xffffffff` and L940 the trailing
/// `crc ^ 0xffffffff`, and both belong to the entry point rather than to any backend. Applying
/// them here is what makes a backend's output comparable with `crc32_z`'s -- an implementation
/// that complemented either end would produce the exact complement of every right answer.
fn crc32_through<B: Crc32Backend>(start: u32, buf: &[u8]) -> u32 {
    !B::update(!start, buf)
}

/// Check one CRC-32 backend against the reference and, if it agrees, time it.
///
/// Generic over the backend so that the check, the label and the timed call all come from one
/// place and cannot drift apart; `B::NAME` is the trait's own `"generic"` / `"braid"` / `"simd"`.
fn bench_crc32_backend<B: Crc32Backend>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    len: usize,
    data: &[u8],
    reference_value: uLong,
) {
    // Outside every timed closure.
    let value = uLong::from(crc32_through::<B>(CRC32_START, data));
    if !agrees(B::NAME, format_args!("len={len}"), value, reference_value) {
        return;
    }

    group.bench_function(BenchmarkId::new(B::NAME, len), |b| {
        b.iter(|| black_box(crc32_through::<B>(black_box(CRC32_START), black_box(data))));
    });
}

/// Adler-32: the scalar backend, the vectorized one where it exists, and the dispatcher.
///
/// The `dispatch` row is the safe core's `adler32_z` (`crates/zlib-rs/src/adler32/mod.rs`), which
/// tests the input's length against the vectorized backend's lane threshold *before* consulting
/// target-feature detection, because a caller may legitimately feed one byte at a time and
/// detection on that path would be work with no possible payoff. Comparing it against the backend
/// rows shows what the dispatch costs; comparing it against the `adler32_throughput` rows for the
/// same lengths shows what crossing the C ABI costs on top.
fn adler32_backends(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("adler32_backends");

    for &len in &BACKEND_SWEEP {
        let data = sweep_bytes(len);

        let Some(throughput) = throughput_bytes(len) else {
            continue;
        };
        group.throughput(throughput);

        // Every value below is computed once, here, outside every timed closure.
        let reference_value = oracle_adler32_z(ADLER32_SEED, &data);

        let generic = uLong::from(Adler32Generic::checksum(ADLER32_START, &data));
        if agrees(
            "Adler32Generic",
            format_args!("len={len}"),
            generic,
            reference_value,
        ) {
            // `Adler32Backend` has no `NAME` associated constant -- unlike `Crc32Backend`, whose
            // documentation states that the constant exists to label exactly this row -- so the
            // two Adler-32 labels are written out here.
            group.bench_function(BenchmarkId::new("generic", len), |b| {
                b.iter(|| {
                    black_box(Adler32Generic::checksum(
                        black_box(ADLER32_START),
                        black_box(data.as_slice()),
                    ))
                });
            });
        }

        #[cfg(feature = "simd")]
        {
            let simd = uLong::from(Adler32Simd::checksum(ADLER32_START, &data));
            if agrees(
                "Adler32Simd",
                format_args!("len={len}"),
                simd,
                reference_value,
            ) {
                group.bench_function(BenchmarkId::new("simd", len), |b| {
                    b.iter(|| {
                        black_box(Adler32Simd::checksum(
                            black_box(ADLER32_START),
                            black_box(data.as_slice()),
                        ))
                    });
                });
            }
        }

        let dispatch = uLong::from(zlib_rs::adler32::adler32_z(ADLER32_START, &data));
        if agrees(
            "adler32_z dispatch",
            format_args!("len={len}"),
            dispatch,
            reference_value,
        ) {
            group.bench_function(BenchmarkId::new("dispatch", len), |b| {
                b.iter(|| {
                    black_box(zlib_rs::adler32::adler32_z(
                        black_box(ADLER32_START),
                        black_box(data.as_slice()),
                    ))
                });
            });
        }
    }

    group.finish();
}

/// CRC-32: the byte-at-a-time engine, the braided one, the vectorized one where it exists, and the
/// dispatcher.
///
/// `Generic` is `crc32.c` L626-L941's byte-at-a-time body and `Braid` is L508-L920's
/// word-at-a-time one, which is why both are always present rather than one being a fallback: the
/// reference picks between them with `#if`, and the braided body itself hands anything shorter than
/// [`BRAID_ENTRY_LEN`] to the byte loop (L640). At `len = 64` the braided path therefore runs its
/// full body for the first time, which is what makes the shortest entry of [`BACKEND_SWEEP`]
/// informative rather than noise.
fn crc32_backends(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group("crc32_backends");

    for &len in &BACKEND_SWEEP {
        let data = sweep_bytes(len);

        let Some(throughput) = throughput_bytes(len) else {
            continue;
        };
        group.throughput(throughput);

        let reference_value = oracle_crc32_z(CRC32_SEED, &data);

        bench_crc32_backend::<Generic>(&mut group, len, &data, reference_value);
        bench_crc32_backend::<Braid>(&mut group, len, &data, reference_value);
        #[cfg(feature = "simd")]
        bench_crc32_backend::<StrideBraid>(&mut group, len, &data, reference_value);

        // The dispatcher, for the same reason the Adler-32 group carries one: it isolates what
        // selection and conditioning cost from what the engine costs. `crc32.c` has no dispatcher
        // to compare against -- the choice there is a `#if` -- so this row has no reference twin.
        let dispatch = uLong::from(zlib_rs::crc32::crc32_z(CRC32_START, &data));
        if agrees(
            "crc32_z dispatch",
            format_args!("len={len}"),
            dispatch,
            reference_value,
        ) {
            group.bench_function(BenchmarkId::new("dispatch", len), |b| {
                b.iter(|| {
                    black_box(zlib_rs::crc32::crc32_z(
                        black_box(CRC32_START),
                        black_box(data.as_slice()),
                    ))
                });
            });
        }
    }

    group.finish();
}

// =============================================================================
//  The combine family
// =============================================================================

/// The seven combine entry points, measured per call rather than per byte.
///
/// None of these touches a buffer. `adler32_combine_` (`adler32.c` L133-L155) works from the two
/// checksums and the second sequence's length alone, and the CRC-32 forms multiply polynomials
/// modulo the CRC generator with `multmodp` and `x2nmodp` (`crc32.c` L163, L184) over the
/// thirty-two-entry `x2n_table`. The cost is therefore logarithmic in the length rather than
/// linear, and reporting bytes per second would be meaningless -- so no [`Throughput`] is set on
/// this group, and the numbers are times per call.
///
/// ★ `crc32_combine_gen` and `crc32_combine_op` are measured separately *because* they split the
/// work: `gen` (`crc32.c` L964-L966) derives an operator for one fixed second-sequence length and
/// `op` (L969-L971) applies it. A caller concatenating many equally sized blocks calls `gen` once
/// and `op` repeatedly, so the pair is faster than the same number of `crc32_combine` calls exactly
/// to the extent that `op` is cheaper than `combine`. Two rows are what make that visible; one
/// would not.
///
/// The operands are real check values over the two halves of a real buffer rather than arbitrary
/// bit patterns, so a mismatch cannot be blamed on an operand no caller would ever supply.
fn combine_family(c: &mut Criterion) {
    report_configuration();

    let data = sweep_bytes(65_536);
    let (first, second) = data.split_at(data.len() / 2);
    let len2 = second.len();

    let adler1 = port_adler32_z(ADLER32_SEED, first);
    let adler2 = port_adler32_z(ADLER32_SEED, second);
    let crc1 = port_crc32_z(CRC32_SEED, first);
    let crc2 = port_crc32_z(CRC32_SEED, second);

    // `z_off_t` is `off_t` -- `core::ffi::c_long`, so 8 bytes on LP64 and 4 on a 32-bit target --
    // while `z_off64_t` is always 64 bits (`zconf.h` L494-L532). Converting with `try_from` rather
    // than a cast is what keeps the narrow form honest on a 32-bit host, where a 64 KiB length
    // fits comfortably but a larger one might not. The port's and the oracle's aliases are
    // converted separately because each crate derives its own from the header.
    let Ok(narrow) = z_off_t::try_from(len2) else {
        eprintln!("checksum_bench: skipping the combine group: {len2} does not fit a z_off_t.");
        return;
    };
    let Ok(wide) = z_off64_t::try_from(len2) else {
        eprintln!("checksum_bench: skipping the combine group: {len2} does not fit a z_off64_t.");
        return;
    };
    let Ok(oracle_narrow) = oracle::z_off_t::try_from(len2) else {
        eprintln!("checksum_bench: skipping the combine group: {len2} does not fit a z_off_t.");
        return;
    };
    let Ok(oracle_wide) = oracle::z_off64_t::try_from(len2) else {
        eprintln!("checksum_bench: skipping the combine group: {len2} does not fit a z_off64_t.");
        return;
    };

    let mut group = c.benchmark_group("combine");

    // Adler-32, both widths. `adler32.c` L158-L164: two exported names over one `local`
    // implementation, so they cannot disagree with each other -- only with the port.
    let port_value = libz_rs_sys::adler32_combine(adler1, adler2, narrow);
    let reference_value = oracle_adler32_combine(adler1, adler2, oracle_narrow);
    if agrees(
        "adler32_combine",
        format_args!("len2={len2}"),
        port_value,
        reference_value,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "adler32_combine"), |b| {
            b.iter(|| {
                black_box(libz_rs_sys::adler32_combine(
                    black_box(adler1),
                    black_box(adler2),
                    black_box(narrow),
                ))
            });
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "adler32_combine"), |b| {
            b.iter(|| {
                black_box(oracle_adler32_combine(
                    black_box(adler1),
                    black_box(adler2),
                    black_box(oracle_narrow),
                ))
            });
        });
    }

    let port_value = libz_rs_sys::adler32_combine64(adler1, adler2, wide);
    let reference_value = oracle_adler32_combine64(adler1, adler2, oracle_wide);
    if agrees(
        "adler32_combine64",
        format_args!("len2={len2}"),
        port_value,
        reference_value,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "adler32_combine64"), |b| {
            b.iter(|| {
                black_box(libz_rs_sys::adler32_combine64(
                    black_box(adler1),
                    black_box(adler2),
                    black_box(wide),
                ))
            });
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "adler32_combine64"), |b| {
            b.iter(|| {
                black_box(oracle_adler32_combine64(
                    black_box(adler1),
                    black_box(adler2),
                    black_box(oracle_wide),
                ))
            });
        });
    }

    // CRC-32, both widths. `crc32.c` L976-L983.
    let port_value = libz_rs_sys::crc32_combine(crc1, crc2, narrow);
    let reference_value = oracle_crc32_combine(crc1, crc2, oracle_narrow);
    if agrees(
        "crc32_combine",
        format_args!("len2={len2}"),
        port_value,
        reference_value,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "crc32_combine"), |b| {
            b.iter(|| {
                black_box(libz_rs_sys::crc32_combine(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(narrow),
                ))
            });
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "crc32_combine"), |b| {
            b.iter(|| {
                black_box(oracle_crc32_combine(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(oracle_narrow),
                ))
            });
        });
    }

    let port_value = libz_rs_sys::crc32_combine64(crc1, crc2, wide);
    let reference_value = oracle_crc32_combine64(crc1, crc2, oracle_wide);
    if agrees(
        "crc32_combine64",
        format_args!("len2={len2}"),
        port_value,
        reference_value,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "crc32_combine64"), |b| {
            b.iter(|| {
                black_box(libz_rs_sys::crc32_combine64(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(wide),
                ))
            });
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "crc32_combine64"), |b| {
            b.iter(|| {
                black_box(oracle_crc32_combine64(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(oracle_wide),
                ))
            });
        });
    }

    // The operator generators. `crc32.c` L954-L966.
    let port_op = libz_rs_sys::crc32_combine_gen(narrow);
    let reference_op = oracle_crc32_combine_gen(oracle_narrow);
    if agrees(
        "crc32_combine_gen",
        format_args!("len2={len2}"),
        port_op,
        reference_op,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "crc32_combine_gen"), |b| {
            b.iter(|| black_box(libz_rs_sys::crc32_combine_gen(black_box(narrow))));
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "crc32_combine_gen"), |b| {
            b.iter(|| black_box(oracle_crc32_combine_gen(black_box(oracle_narrow))));
        });
    }

    let port_op64 = libz_rs_sys::crc32_combine_gen64(wide);
    let reference_op64 = oracle_crc32_combine_gen64(oracle_wide);
    if agrees(
        "crc32_combine_gen64",
        format_args!("len2={len2}"),
        port_op64,
        reference_op64,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "crc32_combine_gen64"), |b| {
            b.iter(|| black_box(libz_rs_sys::crc32_combine_gen64(black_box(wide))));
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "crc32_combine_gen64"), |b| {
            b.iter(|| black_box(oracle_crc32_combine_gen64(black_box(wide))));
        });
    }

    // Applying an operator. `crc32.c` L969-L971. Each side is handed the operator its own
    // implementation generated, so a difference in `op` would show up as a `crc32_combine_gen`
    // mismatch above rather than being smuggled into this row.
    let port_value = libz_rs_sys::crc32_combine_op(crc1, crc2, port_op);
    let reference_value = oracle_crc32_combine_op(crc1, crc2, reference_op);
    if agrees(
        "crc32_combine_op",
        format_args!("len2={len2}"),
        port_value,
        reference_value,
    ) {
        group.bench_function(BenchmarkId::new(PORT_LABEL, "crc32_combine_op"), |b| {
            b.iter(|| {
                black_box(libz_rs_sys::crc32_combine_op(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(port_op),
                ))
            });
        });
        group.bench_function(BenchmarkId::new(ORACLE_LABEL, "crc32_combine_op"), |b| {
            b.iter(|| {
                black_box(oracle_crc32_combine_op(
                    black_box(crc1),
                    black_box(crc2),
                    black_box(reference_op),
                ))
            });
        });
    }

    group.finish();
}

// =============================================================================
//  The table accessor
// =============================================================================

/// `get_crc_table`, measured once on each side, and a one-shot note on what the port's answer
/// means.
///
/// This is deliberately not a throughput case; it is here to document a design consequence in the
/// output. `crc32.c` L478-L481 gives the accessor two purposes, and the second is "to force the
/// generation of the CRC tables in a threaded application" -- necessary because under
/// `DYNAMIC_CRC_TABLE` the reference builds its tables on first use behind a `z_once_t` and,
/// as L12-L17 warns, "there is no mutex or semaphore protection on the static variables used to
/// control the first-use generation of the crc tables". The port has no such state: the tables are
/// `const` arrays, so the accessor is a borrow of static data, always valid and callable from any
/// thread at any time. AAP §0.6.3.5 records the honest consequence -- bit 13 of
/// `zlibCompileFlags()`, `DYNAMIC_CRC_TABLE`, is reported CLEAR.
///
/// The two rows should therefore be indistinguishable and both essentially free; that they are is
/// the observation. The table's *contents* are compared through `oracle::oracle_crc_table`, whose
/// safe wrapper discharges the slice construction in the harness crate, and the comparison is
/// reported rather than asserted -- element-for-element table equality is gated by
/// `crates/zlib-rs-differential/tests/table_equality.rs`, which is where a failure should stop a
/// run.
fn crc_table_accessor(c: &mut Criterion) {
    report_configuration();

    let port_table = zlib_rs::crc32::get_crc_table();
    let reference_table = oracle::oracle_crc_table();

    if port_table.len() == reference_table.len() && port_table.iter().eq(reference_table.iter()) {
        eprintln!(
            "checksum_bench: the port's const crc_table agrees with the reference's, all {} \
             entries (crc32.h L5). No lazy initialization exists to measure, so zlibCompileFlags \
             bit 13 DYNAMIC_CRC_TABLE is reported clear.",
            reference_table.len()
        );
    } else {
        eprintln!(
            "checksum_bench: the port's const crc_table does NOT match the reference's ({} entries \
             against {}). This is reported, not asserted -- \
             crates/zlib-rs-differential/tests/table_equality.rs is the gate that names the \
             offending index.",
            port_table.len(),
            reference_table.len()
        );
    }

    let mut group = c.benchmark_group("get_crc_table");
    group.bench_function(BenchmarkId::new(PORT_LABEL, "get_crc_table"), |b| {
        b.iter(|| black_box(libz_rs_sys::get_crc_table()));
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, "get_crc_table"), |b| {
        b.iter(|| black_box(oracle_get_crc_table()));
    });
    group.finish();
}

// =============================================================================
//  Harness
// =============================================================================
//
//  `harness = false` in the `[[bench]]` entry means libtest supplies no `main`, so
//  `criterion_main!` does. Ordering is the order a reader wants to see: throughput against the
//  reference first, because the port-versus-reference ratio is the primary question; then the
//  forwarders, the real-data cases, the backend comparison, the combine family, and the table
//  accessor last because it is an observation rather than a measurement.

// =================================================================================================
//  The backend acceptance measurement -- the one part of this file that is enforced
// =================================================================================================
//
//  ★ Everything above this line is an informational comparison and the module header says so. This
//  section is different, and it is the answer to a specific question that the informational rows
//  could not answer: **is the `simd` feature worth compiling in at all?**
//
//  The feature is optional, it is the only optional code path in the shipped library, and AAP
//  §0.8.2 item 9 permits it solely because a check value is one scalar however its recurrence is
//  evaluated -- so it may move a rate and can never move a byte. A feature admitted on those terms
//  has to earn the compile-time cost and the second code path it creates, and "earn" is a
//  measurement, not a design intent. Until this section existed the repository committed no result
//  either way, which meant the CRC-32 backend's own module header had to say, accurately, that its
//  throughput claim was "intent, not a result".
//
//  What is enforced, per backend family:
//
//    1. **Never slower.** At every measured length the feature-on backend's time must not exceed
//       the scalar baseline it replaces. A feature that is faster on one input size and slower on
//       another is not an optimization -- it is a bet on the caller's input, and a system library
//       does not get to make that bet on a caller's behalf.
//    2. **Faster somewhere.** At least one measured length must show a real speedup. Without this,
//       a backend that merely forwarded to the baseline would pass condition 1 perfectly, and the
//       gate would certify a no-op.
//
//  Both are decided from the same paired, order-alternating rounds that `benches/deflate_bench.rs`
//  uses, for the same reason: two sweeps taken seconds apart on a shared machine differ by more
//  than the effect being measured, whereas two equal-work runs taken microseconds apart in
//  alternating order do not, and the residual is published rather than hidden.
//
//  The baseline is the backend the feature actually displaces, which is different for the two
//  families and is why they are measured separately: `crc32/mod.rs` selects `StrideBraid` in place of
//  `Braid`, so `braid` is CRC's baseline and the byte-at-a-time `generic` would be an unfairly slow
//  one; `adler32/mod.rs` selects `Adler32Simd` in place of `Adler32Generic`, so `generic` is
//  Adler's baseline and it has no third path.

/// How many bytes the acceptance sweep measures, from below the shortest wide-path threshold to
/// steady state.
///
/// Wider than [`BACKEND_SWEEP`] and for a different purpose. The informational rows compare engines
/// at three representative scales; this sweep has to be able to *fail* the "never slower" condition,
/// so it needs points on both sides of every threshold either family switches on. Reading them in
/// order:
///
/// * `16` -- below both CRC thresholds and below Adler's lane threshold, so every backend runs its
///   byte or word tail and nothing else. A regression here is pure per-call overhead.
/// * `32`, `48` -- still below Adler's lane threshold, so the delegation the threshold performs is
///   measured rather than assumed. `adler32/simd.rs`'s own documentation used to note that the
///   benchmark never descended below the threshold and therefore could not say what the delegation
///   cost; these two points and `16` are what closed that gap.
/// * `55` -- exactly `crc32/simd.rs`'s `MIN_STRIDE_LEN` (`N * W + 2 * W - 1`), the first length at
///   which the CRC feature path runs its own body rather than delegating.
/// * `64` -- `adler32/simd.rs`'s lane threshold, the first length at which the Adler feature path
///   runs its vector body. Shared with [`BACKEND_SWEEP`] so the two sets have a comparable point.
/// * `256`, `4096` -- L1-resident, the range a `gzread` or an `inflate` call actually checksums.
/// * `65536` -- past L1 on a typical host and past `NMAX` (5552) several times over, so Adler's
///   modulo schedule has run its full period repeatedly.
/// * `1 << 20` -- steady state, memory-bandwidth bound, the length at which a body that is one
///   instruction longer per word shows up and a shortened epilogue does not.
#[cfg(feature = "simd")]
const ACCEPTANCE_SWEEP: [usize; 9] = [16, 32, 48, 55, 64, 256, 4096, 65536, 1 << 20];

/// Paired rounds per acceptance case.
///
/// Nine, as in `benches/deflate_bench.rs`: an odd count so the median is a sample rather than a mean
/// of two, and enough rounds that one descheduled round cannot move it.
#[cfg(feature = "simd")]
const ACCEPTANCE_ROUNDS: u32 = 9;

/// Wall-clock budget for the calibration probe that sizes a round.
///
/// ★ Twenty milliseconds, and the figure is load-bearing rather than arbitrary. It was 2 ms, which
/// is enough to average out the timer but *not* enough to average out the scheduler: on a shared
/// machine a 2 ms round can be descheduled in its entirety, and the null measurement showed exactly
/// that -- two timed runs of identical code differing by 20% or more on the 6-to-20-nanosecond
/// cases, where a round is millions of repetitions of a call shorter than a scheduling decision. At
/// 20 ms a single deschedule is a few percent of the round rather than all of it.
///
/// The cost is bounded and small: three timed runs per round, nine rounds, nine lengths, two
/// families, so about ten seconds of measurement for the whole acceptance sweep. That is the right
/// trade against the alternative, which is widening the pass band until noise fits inside it.
#[cfg(feature = "simd")]
const ACCEPTANCE_CALIBRATION: Duration = Duration::from_millis(20);

/// Ceiling on the repetitions one round may run, so a fast case cannot spin unboundedly.
#[cfg(feature = "simd")]
const ACCEPTANCE_MAX_REPS: u32 = 1 << 22;

/// Nanoseconds in a second, as a float, so no integer is ever cast to one implicitly.
#[cfg(feature = "simd")]
const NANOS_PER_SEC: f64 = 1_000_000_000.0;

/// The "never slower" limit: the feature-on backend may not take longer than its baseline.
///
/// One, exactly. A tolerance is applied on top of it per case rather than baked in here, because a
/// fixed percentage would be too loose for the microsecond cases and too tight for the millisecond
/// ones -- see [`acceptance_tolerance`].
const ACCEPTANCE_LIMIT: f64 = 1.00;

/// The "faster somewhere" bar: at least one measured length must reach this ratio or better.
///
/// 0.90, i.e. a 10% improvement at some length. Chosen to match the only throughput figure the AAP
/// states -- §0.8.4's 10% -- rather than invented: a backend that cannot beat its own baseline by
/// as much as the margin the whole port is allowed to lose against C is not carrying its weight as
/// an optional second code path.
const ACCEPTANCE_BENEFIT: f64 = 0.90;

/// The candidate backend's label for the CRC-32 family, available with or without the feature.
///
/// Taken from [`StrideBraid::NAME`] when the feature is on, so the two cannot drift; written out
/// when it is off, because the type does not exist to ask. A feature-off run still prints its
/// `BACKEND-SUMMARY` line -- with `expected=0`, so a consumer can read "no candidate" rather than
/// having to interpret an absent line -- and that line carries this label.
#[cfg(feature = "simd")]
const CRC32_CANDIDATE: &str = StrideBraid::NAME;

/// See the feature-on spelling above.
#[cfg(not(feature = "simd"))]
const CRC32_CANDIDATE: &str = "stride-braid";

/// The candidate backend's label for the Adler-32 family.
///
/// Written out in both configurations: `Adler32Backend` has no `NAME` associated constant, which
/// `adler32_backends` already notes, so there is no constant to take it from.
const ADLER32_CANDIDATE: &str = "simd";

/// Which of an acceptance pair a timed closure is being asked to run.
///
/// One closure serving both, rather than two closures, because both need the same borrowed input
/// buffer; a single closure captures it once and dispatches on this.
#[cfg(feature = "simd")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Which {
    /// The scalar backend the feature displaces.
    Baseline,
    /// The backend the `simd` feature selects.
    Candidate,
}

/// One acceptance case's paired result.
#[cfg(feature = "simd")]
#[derive(Clone, Copy, Debug)]
struct Acceptance {
    /// Median nanoseconds per call for the baseline.
    baseline_ns: f64,
    /// Median nanoseconds per call for the candidate.
    candidate_ns: f64,
    /// The worst single round's ratio, so the spread is published rather than hidden by the median.
    ratio_hi: f64,
    /// ★ The measured noise floor: how much two runs of the **same** backend differ.
    ///
    /// Each round times the baseline twice, and this is the **second-largest** per-round spread
    /// between those two identical measurements -- the largest with a single outlier round
    /// discarded.
    ///
    /// ★ Both extremes were tried and both were wrong, which is why the statistic is worth spelling
    /// out. The question is "how large a ratio *can* arise from nothing", so the median is too
    /// generous a reading of it: a case whose identical runs agreed closely on most rounds published
    /// a tolerance narrower than its own worst round and was intermittently failed by it -- that was
    /// the 48-byte Adler case, where both backends run the same delegated code and the true ratio is
    /// 1.000. The plain maximum is too pessimistic in the other direction: on a shared four-core
    /// machine one round in nine is routinely descheduled, and letting that round define the floor
    /// pushed most cases past [`ACCEPTANCE_NOISE_CEILING`] and had them refused as unmeasurable.
    /// Discarding exactly one round is what makes the figure describe the measurement rather than
    /// the scheduler, and nine rounds is enough that discarding one leaves eight. It is the answer to a question a fixed tolerance can only
    /// guess at: on *this* host, in *this* run, at *this* length, how large a ratio can arise from
    /// nothing at all? A candidate/baseline ratio inside this figure is indistinguishable from two
    /// runs of one backend, so it is not evidence of a regression -- and one outside it is.
    ///
    /// This is what the review asked for when it objected to constants that were "design choices,
    /// not measured crossovers": the allowance is now an observation. Nothing about it is tuned,
    /// and it costs one extra timed run per round.
    noise: f64,
    /// How many paired rounds produced the medians.
    rounds: u32,
}

/// Repetitions per round, from one probe call of `which`.
#[cfg(feature = "simd")]
fn acceptance_calibrate<F: FnMut(Which) -> u64>(op: &mut F, which: Which) -> u32 {
    let started = Instant::now();
    black_box(op(which));
    let probe_ns = started.elapsed().as_nanos().max(1);
    let wanted = ACCEPTANCE_CALIBRATION.as_nanos() / probe_ns;
    u32::try_from(wanted.clamp(1, u128::from(ACCEPTANCE_MAX_REPS))).unwrap_or(ACCEPTANCE_MAX_REPS)
}

/// Mean nanoseconds per call over `reps` calls of one side.
///
/// The checksum each call returns is folded into an accumulator and handed to [`black_box`], so the
/// optimizer cannot hoist the call out of the loop or discard it as dead -- a timed loop whose body
/// has been deleted measures the loop.
#[cfg(feature = "simd")]
fn acceptance_round<F: FnMut(Which) -> u64>(op: &mut F, which: Which, reps: u32) -> f64 {
    let mut sink = 0_u64;
    let started = Instant::now();
    for _ in 0..reps {
        sink = sink.wrapping_add(op(which));
    }
    let elapsed = started.elapsed().as_secs_f64();
    black_box(sink);
    elapsed * NANOS_PER_SEC / f64::from(reps)
}

/// Times the two backends against each other in [`ACCEPTANCE_ROUNDS`] paired, alternating rounds.
///
/// Both sides get the **same** repetition count -- the smaller of the two probes' -- so a round is
/// two equal-work measurements taken microseconds apart. Even rounds run the baseline first, odd
/// rounds the candidate, so neither collects the second-mover cache advantage a fixed order hands
/// out. This is `benches/deflate_bench.rs`'s protocol, and it is the same protocol because the
/// alternative -- one sweep of each -- is what produced the ordering bias that made the earlier
/// informational rows unusable as evidence.
#[cfg(feature = "simd")]
fn acceptance_ns<F: FnMut(Which) -> u64>(mut op: F) -> Option<Acceptance> {
    let reps = acceptance_calibrate(&mut op, Which::Baseline)
        .min(acceptance_calibrate(&mut op, Which::Candidate));

    let mut baseline_samples = Vec::with_capacity(ACCEPTANCE_ROUNDS as usize);
    let mut candidate_samples = Vec::with_capacity(ACCEPTANCE_ROUNDS as usize);
    let mut noise_samples = Vec::with_capacity(ACCEPTANCE_ROUNDS as usize);
    let mut ratio_hi = f64::NEG_INFINITY;

    for round in 0..ACCEPTANCE_ROUNDS {
        // Three timed runs per round, not two: the baseline is timed on both sides of the
        // candidate. The pair of baseline runs is the null measurement -- two runs of identical
        // code -- and their spread is what [`Acceptance::noise`] reports. Timing them either side
        // of the candidate rather than back to back is deliberate: back-to-back runs share a warm
        // cache and a stable frequency and would understate the noise the real comparison sees.
        let (baseline_ns, candidate_ns, other_ns) = if round % 2 == 0 {
            let first = acceptance_round(&mut op, Which::Baseline, reps);
            let candidate = acceptance_round(&mut op, Which::Candidate, reps);
            (
                first,
                candidate,
                acceptance_round(&mut op, Which::Baseline, reps),
            )
        } else {
            let first = acceptance_round(&mut op, Which::Candidate, reps);
            let baseline = acceptance_round(&mut op, Which::Baseline, reps);
            (
                baseline,
                first,
                acceptance_round(&mut op, Which::Baseline, reps),
            )
        };
        if baseline_ns > 0.0 {
            ratio_hi = ratio_hi.max(candidate_ns / baseline_ns);
        }
        // The symmetric spread of the two identical runs, as a ratio above one. Symmetric because
        // which of the two happened to be faster is meaningless.
        let (low, high) = if baseline_ns <= other_ns {
            (baseline_ns, other_ns)
        } else {
            (other_ns, baseline_ns)
        };
        if low > 0.0 {
            noise_samples.push(high / low - 1.0);
        }
        baseline_samples.push(baseline_ns);
        candidate_samples.push(candidate_ns);
    }

    baseline_samples.sort_by(f64::total_cmp);
    candidate_samples.sort_by(f64::total_cmp);
    noise_samples.sort_by(f64::total_cmp);
    // The second-largest spread: ascending order, so one index below the last. `saturating_sub`
    // handles the degenerate one-sample case by taking that sample.
    let noise = noise_samples
        .len()
        .checked_sub(2)
        .and_then(|at| noise_samples.get(at).copied())
        .or_else(|| noise_samples.last().copied())
        .unwrap_or(0.0);
    Some(Acceptance {
        baseline_ns: *baseline_samples.get(baseline_samples.len() / 2)?,
        candidate_ns: *candidate_samples.get(candidate_samples.len() / 2)?,
        ratio_hi: if ratio_hi.is_finite() {
            ratio_hi
        } else {
            f64::NAN
        },
        noise: if noise.is_finite() { noise } else { 0.0 },
        rounds: ACCEPTANCE_ROUNDS,
    })
}

/// The smallest allowance any case gets, however quiet the host looked.
///
/// A machine can be quiet enough during the null runs that the measured spread is near zero, and
/// publishing a near-zero tolerance would make the gate fail on the next slightly noisier run. Half
/// a percent is small enough that no real regression hides under it -- the smallest effect this
/// suite is built to detect is 10% -- and large enough to survive a quiet-host measurement.
#[cfg(feature = "simd")]
const ACCEPTANCE_NOISE_FLOOR: f64 = 0.005;

/// The largest allowance any case gets, however noisy the host was.
///
/// ★ The safeguard on a self-measured tolerance. A derived allowance is better evidence than a
/// constant, but it has a failure mode a constant does not: on a badly contended machine the null
/// spread grows without limit, and a tolerance that grew with it would eventually admit anything.
/// Ten percent is where that stops. A case whose two identical runs differ by more than 10% has not
/// measured its backend, and the honest response is to refuse the result rather than to widen the
/// bar until it passes -- which is why exceeding this is reported as a case that could not be
/// measured, and why `.github/scripts/bench_gate.py` independently refuses any published tolerance
/// above the same figure.
#[cfg(feature = "simd")]
const ACCEPTANCE_NOISE_CEILING: f64 = 0.10;

/// The noise allowance for one acceptance case, from that case's own null measurement.
///
/// ★ This is an observation, not a constant. [`Acceptance::noise`] is the median spread between two
/// timed runs of *identical* code at this length on this host, so it is exactly the size of ratio
/// that can arise from nothing; a candidate/baseline ratio inside it is not evidence. The earlier
/// form of this function converted a guessed 0.5 ns timer allowance into a ratio, and the guess was
/// wrong in a way that only measurement showed: the 32-byte CRC case, where both backends run the
/// *same* delegated code, reported ratios from 0.97 to 1.03 across runs -- a spread of 1 ns, twice
/// the guess -- and was intermittently failed by it.
///
/// Bounded at both ends, for the reasons [`ACCEPTANCE_NOISE_FLOOR`] and
/// [`ACCEPTANCE_NOISE_CEILING`] give.
#[cfg(feature = "simd")]
fn acceptance_tolerance(noise: f64) -> f64 {
    if noise.is_finite() {
        noise.clamp(ACCEPTANCE_NOISE_FLOOR, ACCEPTANCE_NOISE_CEILING)
    } else {
        ACCEPTANCE_NOISE_CEILING
    }
}

/// Accumulates one backend family's acceptance cases and prints its summary line.
struct AcceptanceLedger {
    /// The family: `"crc32"` or `"adler32"`.
    family: &'static str,
    /// The scalar backend's label, as it appears on every case line.
    baseline: &'static str,
    /// The feature-on backend's label.
    candidate: &'static str,
    /// How many cases the sweep set out to measure, so an absent case cannot read as a pass.
    expected: u32,
    /// How many produced a ratio.
    cases: u32,
    /// How many exceeded [`ACCEPTANCE_LIMIT`] plus their own tolerance.
    over: u32,
    /// ★ How many cases were refused because the host was too noisy to measure them.
    ///
    /// A case whose two identical null runs differ by more than [`ACCEPTANCE_NOISE_CEILING`] has
    /// not measured its backend, and publishing a ratio for it would be publishing the scheduler.
    /// Counting those separately is what lets a consumer tell "the candidate is fine" from "this
    /// machine could not tell" -- and `cases + unmeasured == expected` is then an exact identity a
    /// gate can check, so a case that vanished for any *other* reason is still caught.
    unmeasured: u32,
    /// The best ratio any case reached, which decides the "faster somewhere" condition.
    best: f64,
}

impl AcceptanceLedger {
    /// An empty ledger for one family.
    fn new(
        family: &'static str,
        baseline: &'static str,
        candidate: &'static str,
        expected: u32,
    ) -> Self {
        Self {
            family,
            baseline,
            candidate,
            expected,
            cases: 0,
            over: 0,
            unmeasured: 0,
            best: f64::INFINITY,
        }
    }

    /// Record one length's paired result, printing its `BACKEND` line.
    #[cfg(feature = "simd")]
    fn record(&mut self, len: usize, paired: &Acceptance) {
        if paired.baseline_ns <= 0.0 || paired.candidate_ns <= 0.0 {
            eprintln!(
                "checksum_bench: no acceptance ratio for family={family} case={len}: the paired \
                 rounds measured {baseline} at {b} ns and {candidate} at {c} ns, and a zero on \
                 either side is a broken measurement rather than an infinitely fast backend.",
                family = self.family,
                baseline = self.baseline,
                candidate = self.candidate,
                b = paired.baseline_ns,
                c = paired.candidate_ns,
            );
            return;
        }

        if paired.noise > ACCEPTANCE_NOISE_CEILING {
            eprintln!(
                "checksum_bench: no acceptance ratio for family={family} case={len}: two timed \
                 runs of the SAME backend differed by {noise:.1}%, above the {ceiling:.0}% \
                 ceiling, so this host was too contended for the comparison to mean anything. \
                 Re-run on a quiet machine rather than reading the ratio.",
                family = self.family,
                noise = paired.noise * 100.0,
                ceiling = ACCEPTANCE_NOISE_CEILING * 100.0,
            );
            self.unmeasured = self.unmeasured.saturating_add(1);
            return;
        }

        let ratio = paired.candidate_ns / paired.baseline_ns;
        let tolerance = acceptance_tolerance(paired.noise);
        let allowed = ACCEPTANCE_LIMIT + tolerance;
        let verdict = if ratio > allowed { "over" } else { "within" };

        self.cases = self.cases.saturating_add(1);
        if ratio > allowed {
            self.over = self.over.saturating_add(1);
        }
        self.best = self.best.min(ratio);

        eprintln!(
            "checksum_bench: BACKEND family={family} case={len} baseline={baseline} \
             candidate={candidate} baseline_ns={b:.1} candidate_ns={c:.1} ratio={ratio:.3} \
             ratio_hi={hi:.3} noise={noise:.4} tolerance={tolerance:.4} rounds={rounds} \
             order=alternating limit={ACCEPTANCE_LIMIT:.2} verdict={verdict}",
            family = self.family,
            baseline = self.baseline,
            candidate = self.candidate,
            b = paired.baseline_ns,
            c = paired.candidate_ns,
            hi = paired.ratio_hi,
            noise = paired.noise,
            rounds = paired.rounds,
        );
    }

    /// Print the family's aggregate `BACKEND-SUMMARY` line.
    ///
    /// `best` is the field that answers "is the feature worth having": it is the strongest speedup
    /// any measured length showed, and a consumer requires it at or below [`ACCEPTANCE_BENEFIT`].
    /// `expected=0` is printed by a build without the feature, where there is no candidate to
    /// measure and therefore nothing to accept -- which a consumer must be able to tell apart from
    /// a feature-on run that measured nothing, and can, because only the latter has `expected>0`.
    fn finish(&self) {
        eprintln!(
            "checksum_bench: BACKEND-SUMMARY family={family} baseline={baseline} \
             candidate={candidate} expected={expected} cases={cases} over={over} \
             unmeasured={unmeasured} best={best:.3} limit={ACCEPTANCE_LIMIT:.2} \
             benefit={ACCEPTANCE_BENEFIT:.2}",
            family = self.family,
            baseline = self.baseline,
            candidate = self.candidate,
            expected = self.expected,
            cases = self.cases,
            over = self.over,
            unmeasured = self.unmeasured,
            best = if self.best.is_finite() {
                self.best
            } else {
                f64::NAN
            },
        );
    }
}

/// Whether the `simd` feature was compiled in, as the count of cases the sweep will produce.
///
/// Zero without the feature. A feature-off run still prints both `BACKEND-SUMMARY` lines, with
/// `expected=0`, so that a consumer can require one line per family in every run and read the
/// count rather than having to interpret an absent line.
#[cfg(feature = "simd")]
const fn acceptance_expected() -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        ACCEPTANCE_SWEEP.len() as u32
    }
}

/// See the feature-on spelling above: zero, because there is no candidate backend to measure.
#[cfg(not(feature = "simd"))]
const fn acceptance_expected() -> u32 {
    0
}

/// CRC-32: the feature-on backend against `braid`, the backend it displaces, at every sweep length.
///
/// Registers no criterion row. The paired protocol above is the measurement, and adding criterion
/// rows for the same comparison would double the run time to produce a second, order-biased answer
/// to a question already answered.
#[allow(unused_variables, unused_mut)]
fn crc32_acceptance(_c: &mut Criterion) {
    report_configuration();

    let mut ledger =
        AcceptanceLedger::new("crc32", Braid::NAME, CRC32_CANDIDATE, acceptance_expected());

    #[cfg(feature = "simd")]
    for &len in &ACCEPTANCE_SWEEP {
        let data = sweep_bytes(len);

        // Correctness before speed, exactly as the informational rows do it: a rate for a wrong
        // value is worse than no rate. Both backends are checked against the C oracle rather than
        // against each other, so a shared mistake cannot agree its way past this.
        let reference = oracle_crc32_z(0, &data);
        let baseline = uLong::from(crc32_through::<Braid>(CRC32_START, &data));
        let candidate = uLong::from(crc32_through::<StrideBraid>(CRC32_START, &data));
        if !agrees(
            Braid::NAME,
            format_args!("acceptance len={len}"),
            baseline,
            reference,
        ) || !agrees(
            CRC32_CANDIDATE,
            format_args!("acceptance len={len}"),
            candidate,
            reference,
        ) {
            continue;
        }

        let Some(paired) = acceptance_ns(|which| match which {
            Which::Baseline => u64::from(crc32_through::<Braid>(CRC32_START, &data)),
            Which::Candidate => u64::from(crc32_through::<StrideBraid>(CRC32_START, &data)),
        }) else {
            continue;
        };
        ledger.record(len, &paired);
    }

    ledger.finish();
}

/// Adler-32: the feature-on backend against `generic`, at every sweep length. Same shape as
/// [`crc32_acceptance`]; a different family and a different baseline.
#[allow(unused_variables, unused_mut)]
fn adler32_acceptance(_c: &mut Criterion) {
    report_configuration();

    let mut ledger = AcceptanceLedger::new(
        "adler32",
        "generic",
        ADLER32_CANDIDATE,
        acceptance_expected(),
    );

    #[cfg(feature = "simd")]
    for &len in &ACCEPTANCE_SWEEP {
        let data = sweep_bytes(len);

        let reference = oracle_adler32_z(ADLER32_SEED, &data);
        let baseline = uLong::from(Adler32Generic::checksum(ADLER32_START, &data));
        let candidate = uLong::from(Adler32Simd::checksum(ADLER32_START, &data));
        if !agrees(
            "Adler32Generic",
            format_args!("acceptance len={len}"),
            baseline,
            reference,
        ) || !agrees(
            "Adler32Simd",
            format_args!("acceptance len={len}"),
            candidate,
            reference,
        ) {
            continue;
        }

        let Some(paired) = acceptance_ns(|which| match which {
            Which::Baseline => u64::from(Adler32Generic::checksum(ADLER32_START, &data)),
            Which::Candidate => u64::from(Adler32Simd::checksum(ADLER32_START, &data)),
        }) else {
            continue;
        };
        ledger.record(len, &paired);
    }

    ledger.finish();
}

criterion_group!(
    checksum_benches,
    crc32_acceptance,
    adler32_acceptance,
    adler32_throughput,
    crc32_throughput,
    uint_forwarders,
    adler32_fixtures,
    crc32_fixtures,
    adler32_backends,
    crc32_backends,
    combine_family,
    crc_table_accessor,
);
criterion_main!(checksum_benches);
