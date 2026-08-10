//! Decompression throughput: the Rust port against the in-process C oracle.
//!
//! This is the second of the three suites AAP §0.3.1 places at `benches/`, and it exists to
//! answer one question with a number: *does the port decompress within 10% of the speed of the C
//! reference?* That is the gate AAP §0.8.4 states for decompression, and AAP §0.6.4.6 requires it
//! to be read from both implementations running in **one process on one buffer**, so that two
//! build artifacts, two page-cache states and two process start-ups cannot be mistaken for a
//! throughput difference.
//!
//! The idioms come from `benches/checksum_bench.rs`, which established them: committed-corpus
//! loading, named call gates that own a single `unsafe` block each, an agreement check taken
//! strictly outside every timed region, graceful skipping in place of assertions, and the
//! `zlib-rs` / `c-oracle` row labels. What this suite adds on top is *stream state* — a decoder
//! has to be initialised, driven, possibly reset and always ended, and each of those is a
//! measurable cost that has to be attributed deliberately rather than by accident.
//!
//! # What is measured
//!
//! Six groups. Every group pairs the port and the reference as two rows under one benchmark id so
//! that criterion prints them side by side, and every row carries
//! `Throughput::Bytes(uncompressed_len)` so the reported rate is *decompressed* bytes per second
//! — which is the quantity the gate is expressed in.
//!
//! * **`inflate_steady_state`** — ★ **this is the group the ≤10% gate is read from.** One decoder
//!   is initialised once per row and `inflateReset2` returns it to the start of a stream between
//!   iterations, so what is timed is the decode itself plus the cheap half of the stream lifecycle
//!   and *not* the allocator. `inflateReset2` keeps the window allocation when the window size is
//!   unchanged, which is exactly why it is the right call here. Axis: each committed fixture at
//!   compression levels 1, 6 and 9.
//! * **`inflate_lifecycle`** — the same work with `inflateInit2_` and `inflateEnd` inside the
//!   timed region, so one iteration is a whole stream from nothing to nothing. Kept as a separate
//!   signal rather than folded into the gate, because on a small payload the allocator dominates
//!   and would swamp the decode it is supposed to be measuring. Read the two groups together: the
//!   difference between them *is* the per-stream setup cost. The ids are deliberately spelled the
//!   same way in both groups, so three of the four rows here — `binary.bin-L6`,
//!   `repetitive.bin-L6` and `window_boundary.bin-L6` — have an exact counterpart above and can be
//!   subtracted directly. `hello.bin-L6` has none, and is here alone because a 14-byte payload is
//!   the clearest view of setup cost there is.
//! * **`inflate_container`** — the three container formats, selected by `windowBits`: 15 for zlib
//!   (RFC 1950), −15 for raw DEFLATE (RFC 1951) and 31 for gzip (RFC 1952). Each takes a different
//!   header path through `inflate.c`'s state machine — a two-byte header and an Adler-32 trailer,
//!   no header at all, or a ten-plus-byte header and a CRC-32 trailer — so all three are measured
//!   rather than assumed equivalent.
//! * **`inflate_feeding`** — single-shot against chunked, and chunked at three chunk sizes. AAP
//!   §0.6.4.2 records that chunk boundaries interact with flush handling and pending-buffer state,
//!   and the codegen lever AAP §0.3.2.4 describes is explicitly chunk-size sensitive, so a 16-byte
//!   and a 1024-byte case sit beside the reference driver's 32768.
//! * **`uncompress_oneshot`** — `uncompress` and `uncompress2` against `c_uncompress` and
//!   `c_uncompress2`. A distinct and heavily used entry path: `uncompr.c` runs a whole stream
//!   internally, so its cost profile is the lifecycle group's rather than the steady state's.
//! * **`inflate_silesia`** — the same steady-state measurement over the opt-in tier-2 corpus,
//!   skipped entirely and with one clear note when that corpus is not present.
//!
//! # Provenance
//!
//! The subject is `inflate.c` (1,413 lines), whose `inflate` entry point is at L474 and whose
//! window bookkeeping is `updatewindow` at L252 — the function `window_boundary.bin` exists to
//! exercise. `uncompr.c` (101 lines) supplies the one-shot group: `uncompress` at L97 forwards to
//! `uncompress2` at L83, which forwards to `uncompress2_z` at L29.
//!
//! The harness *shape* is the repository's own benchmark driver, `contrib/testzlib/testzlib.c`
//! (275 lines), which AAP §0.4.1.5 names as this target's source. Three things are taken from it
//! verbatim and one is deliberately left behind:
//!
//! * the 32768-byte default chunk (L147-L148, `0x8000` for both directions);
//! * the decompression loop at L245-L254 — set `next_in` and `next_out` **once** (L241-L242, the
//!   library advances both itself), then each round offer `min(remaining, chunk)` input and a
//!   chunk of output space, call `inflate` with `Z_SYNC_FLUSH`, and continue while the status is
//!   `Z_OK`;
//! * the acceptance check at L267-L270 — the produced length equals the original length *and*
//!   the bytes compare equal;
//! * and **not** its three hand-rolled clocks (`GetTickCount`, `QueryPerformanceCounter` and an
//!   `rdtsc` pair), which criterion's warmed, resampled, outlier-aware sampling replaces. That
//!   driver is Windows-only — it includes `windows.h` and contains inline `_asm` — so it is a
//!   design reference and is never ported literally, never compiled and never linked. Its
//!   `ReadFileMemory` (L118-L141) becomes `std::fs::read`, which must tolerate a missing file.
//!
//! Two deliberate departures from the reference driver, both recorded so that neither reads as an
//! oversight:
//!
//! * **Output buffers are sized from the known uncompressed length, not from `deflateBound`.**
//!   `deflateBound` bounds the *compressed* size and is used here only to size the setup
//!   compressor's output. A decoder's buffer is sized from the fixture's own length plus one byte
//!   of slack, so that the final call still has somewhere to go and can consume the trailer and
//!   return `Z_STREAM_END` rather than stopping at `Z_OK` with a full buffer.
//! * **`avail_out` is clamped to the space that remains** instead of being provisioned by
//!   over-allocating a whole extra chunk as `testzlib.c` L227 does. Same loop; the bound is made
//!   explicit rather than paid for.
//!
//! # This is not a correctness gate
//!
//! Every case is decoded once by each implementation before either is timed, and both results are
//! compared against the original bytes. That check is a guard on the *measurement*, not a test: a
//! throughput number for a decoder that produced the wrong bytes is worse than no number. On a
//! mismatch this file prints a diagnostic naming the fixture, the level, the `windowBits` and the
//! feeding mode, skips that one case, and carries on — because a suite that aborted on case three
//! would say nothing about the other eighty.
//!
//! Correctness itself is gated under `cargo test`, where a failure should stop the run and name
//! the offending configuration: `crates/zlib-rs-differential/tests/byte_identical.rs` owns the
//! encoder's byte identity, `tests/roundtrip_interop.rs` owns the two crossings in both
//! directions, and `tests/table_equality.rs` owns the transcribed constants. Nothing here
//! duplicates them.
//!
//! # The ≤10% gate, and the summary format CI consumes
//!
//! criterion is a measurement tool and does not fail a build, and the pass/fail decision belongs
//! to the criterion regression job that AAP §0.4.1.6 places in `.github/workflows/rust.yml`. So
//! this file *reports* the gate and never enforces it: nothing here panics, exits or aborts on a
//! regression.
//!
//! Alongside criterion's own estimates, each case is measured by a small bounded self-timed pass
//! and one line per case is written to **stderr** in a stable, greppable form:
//!
//! ```text
//! inflate_bench: RATIO group=<group> case=<case> bytes=<n> port_ns=<f> oracle_ns=<f> ratio=<f> limit=1.10 gate=counted|informational verdict=within|over
//! ```
//!
//! and one aggregate line per group:
//!
//! ```text
//! inflate_bench: RATIO-SUMMARY group=<group> cases=<n> gated=<k> over=<m> informational=<i> limit=1.10
//! ```
//!
//! The field order is fixed and the keys are stable. `bytes` is the uncompressed length, which is
//! what the throughput is per. `verdict=over` marks a case slower than `limit`. `cases` is
//! `gated + informational`, and **`over` counts only gated cases**, so a workflow that reads `over`
//! from the `inflate_steady_state` summary line is reading exactly the AAP §0.8.4 gate.
//!
//! ★ `gate=informational` is what separates a throughput comparison from a per-call-overhead
//! comparison, and it is a statement about what the gate *means* rather than a way of avoiding it.
//! A payload below [`GATE_MIN_BYTES`] -- one page -- is decoded in a few hundred nanoseconds of
//! which almost none is decoding, so its ratio measures entry validation and stream reset. That
//! number is worth having and is printed in full with its own verdict; it is simply not the
//! bytes-per-second bound AAP §0.8.4 states, which AAP §0.6.4.6 reads from the multi-megabyte
//! Silesia members. The floor's derivation is at [`GATE_MIN_BYTES`].
//!
//! Two further properties of these numbers matter to anyone reading them:
//!
//! * They are **indicative**, not authoritative. The pass is bounded to a few milliseconds per
//!   side, takes the minimum of three rounds, and reports one figure rather than a confidence
//!   interval. criterion's own estimate remains the authoritative measurement, and a regression
//!   job that wants statistics should read it from
//!   `target/criterion/<group>/<label>/<case>/new/estimates.json` -- note that a
//!   `BenchmarkId::new(label, case)` becomes *two* path components under the group, not one. The
//!   ratio lines exist so that a ratio is available at all from a plain `cargo bench` run, and so
//!   that the gate is visible in the log next to the rows it summarises.
//! * They are measured **before** the criterion rows for the same case, so the calibration also
//!   serves as an identical warm-up for both sides.
//!
//! # Running it
//!
//! ```text
//! cargo bench -p zlib-rs-differential --bench inflate_bench
//! cargo bench -p zlib-rs-differential --bench inflate_bench -- --test    # one iteration each
//! cargo bench -p zlib-rs-differential --bench inflate_bench -- inflate_steady_state
//! ```
//!
//! The groups run with an explicit 50-sample, 1-second warm-up, 3-second measurement budget
//! rather than criterion's 100/3/5 defaults. A full default run registers exactly eighty-six rows
//! and took eight minutes when this file was written; the reasoning is recorded at [`configure`].
//! `--test` executes every case exactly once in a few seconds and is the cheapest end-to-end proof
//! that the harness, the oracle linkage, the version-and-size handshake and the corpus paths are
//! all correct. Filtering by group name is the way to spend the whole budget on one axis.
//!
//! Comparing two whole configurations — a `simd` build against a scalar one, say — is what
//! criterion's baselines are for, and the row labels here are stable across configurations
//! precisely so that it works:
//!
//! ```text
//! cargo bench -p zlib-rs-differential --bench inflate_bench -- --save-baseline scalar
//! cargo bench -p zlib-rs-differential --bench inflate_bench --features simd \
//!     -- --baseline-lenient scalar
//! ```
//!
//! The `simd` feature is worth exercising here and not only in the checksum suite: `inflate`
//! verifies an Adler-32 or a CRC-32 over everything it produces, so the checksum backend is on
//! the decoder's hot path even though it cannot change a single emitted byte.
//!
//! # Code generation
//!
//! Cargo's `bench` profile inherits `[profile.release]`, which the repository root sets to
//! `lto = "fat"`, `codegen-units = 1` and `opt-level = 3`, so these measurements are taken
//! against the same code generation as the shipped library. `panic = "abort"` is ignored for
//! bench targets — cargo does not honour the key there — which is expected and harmless.
//! Profiles are honoured only in the workspace root, so nothing here declares one and none is
//! needed.
//!
//! `-Cllvm-args=-enable-dfa-jump-thread` is the output-neutral codegen lever AAP §0.3.2.4
//! measured at roughly a 10% gain for small chunked input, which is exactly the shape the
//! 16-byte and 1024-byte rows of `inflate_feeding` expose. It is passed through `RUSTFLAGS` at
//! measurement time and is **never** committed to a manifest, because it is an LLVM-internal
//! flag that a non-LLVM toolchain would reject:
//!
//! ```text
//! RUSTFLAGS="-Cllvm-args=-enable-dfa-jump-thread" \
//!     cargo bench -p zlib-rs-differential --bench inflate_bench
//! ```
//!
//! # Inputs — two tiers, and no network, ever
//!
//! Decompression needs compressed input, and this suite produces it **with the C reference**
//! (`c_deflateInit2_`, `c_deflate`, `c_deflateEnd`, sized by `c_deflateBound`) during setup,
//! never inside a timed closure. Two reasons: the honest baseline for "how fast does the port
//! decode real zlib output" is real zlib output, and decoding it also exercises the C→Rust
//! direction of the interoperability requirement in AAP §0.8.1 directive 4 on every single case.
//!
//! **Tier 1 — the committed minimal corpus, always available.** Read by exact filename from
//! `<CARGO_MANIFEST_DIR>/corpus/minimal`, whose inventory `corpus/README.md` pins. The five this
//! suite measures are the ones whose decode profiles differ: `repetitive.bin` (16 KiB, long
//! matches and the largest expansion ratio), `random.bin` (8 KiB, incompressible, so stored
//! blocks and a near-1:1 ratio), `text.txt` (45 bytes) and `binary.bin` (23 bytes) for the
//! short-input paths where per-call overhead is visible, and `window_boundary.bin` (65 KiB), the
//! only committed fixture past the 32 KiB window and therefore the only one that exercises
//! `updatewindow` at `inflate.c` L252. `empty.bin` is deliberately absent: a zero-byte case would
//! hand criterion a `Throughput::Bytes(0)` to divide by. `hello.bin` joins the lifecycle group
//! only, where a 14-byte payload is the point.
//!
//! ★ For a `[[bench]]` target `CARGO_MANIFEST_DIR` expands to the **host package's** directory —
//! `crates/zlib-rs-differential`, the crate whose manifest carries the `[[bench]]` entry that
//! attaches this file — and *not* to the `benches/` directory this file lives in. That is
//! genuinely surprising, so both paths derived from it are named once and only once, in
//! [`minimal_corpus_dir`] and [`repository_root`].
//!
//! **Tier 2 — Silesia, opt-in only.** `corpus/README.md` publishes one canonical resolution
//! order and `corpus/fetch_silesia.sh` implements the same one: `$ZLIB_RS_SILESIA_DIR` when it is
//! set and non-empty, otherwise `<repo-root>/target/silesia`. [`silesia_dir`] implements exactly
//! that and nothing else. Absence is normal, not an error: when the directory is missing or holds
//! no readable member this file prints one note naming
//! `crates/zlib-rs-differential/corpus/fetch_silesia.sh` as the way to obtain the corpus, skips
//! the Silesia groups, and the run still succeeds.
//!
//! The twelve member names `corpus/README.md` pins are resolved by exact name, exactly as tier 1
//! is, and a directory holding some of them is measured over those. A directory holding none of
//! them but holding other readable files is measured over the first twelve of those, sorted, and
//! says so in as many words — that keeps a partially extracted or hand-populated directory useful
//! for a local run while stating plainly that the numbers are not the corpus AAP §0.8.4 names. See
//! [`silesia_members`].
//!
//! **Nothing here touches the network, spawns a process, or executes that script.** AAP §0.6.4.4
//! requires `cargo test` and CI to be network-free, and `corpus/README.md` states that the script
//! is invoked by a human and by nothing else — it is referenced by no manifest, no build script,
//! no test and no workflow, and this file keeps it that way. No crate is introduced for any of
//! this either: `std::fs` and `std::path` suffice.
//!
//! # Hygiene
//!
//! `crates/zlib-rs-differential` is dev-only and appears in neither shipped crate's
//! `[dependencies]` (AAP §0.6.4.1), which is what makes `unsafe` acceptable in this file at all:
//! it exists solely to call the oracle's `extern "C"` declarations and the facade's exported C
//! ABI, every such call is funnelled through one named gate that owns a single `unsafe` block,
//! and every block states the invariant that discharges it. AAP §0.8.1 directive 5 and §0.7.1(a)
//! bind the *shipped* crates, and they are untouched. `extern "C-unwind"` appears nowhere,
//! nothing here is `#[no_mangle]`, and no `extern "C"` block or `#[link]` attribute is declared —
//! `crates/zlib-rs-differential/src/oracle.rs` owns every oracle declaration and
//! `crates/zlib-rs-differential/build.rs` already emits the link directives.
//!
//! Every stream is ended on every path, including the skip paths, by the three RAII guards
//! [`PortInflate`], [`OracleInflate`] and [`OracleDeflate`]. A leaked stream would surface in the
//! nightly `AddressSanitizer` job, which is the gate this crate does sit inside; Miri is scoped
//! to `-p zlib-rs` and cannot execute the C oracle, so this file is outside it by construction.
//!
//! A `harness = false` bench compiles without `--test`, so `cfg(test)` is false here and these
//! functions are not `#[test]`. `clippy.toml`'s `allow-unwrap-in-tests`, `allow-expect-in-tests`
//! and `allow-panic-in-tests` therefore do not reach this file and the workspace's denied panic
//! family is fully in force. Nothing below unwraps, expects, panics or indexes: a fallible step
//! logs to stderr and returns early, which is also the behaviour a benchmark should have. Lengths
//! are converted with `try_from` rather than `as`, so a length that could not be represented is
//! reported and skipped instead of silently truncated.

use core::ffi::{c_char, c_int};
use core::mem::size_of;
use std::ffi::{CStr, OsStr};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use criterion::measurement::WallTime;
use criterion::{
    criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion, Throughput,
};

// The facade -- the artifact whose throughput is the acceptance criterion. The spelling is
// `libz_rs_sys`, never `z`: `crates/libz-rs-sys` sets `[lib] name = "z"` so that cargo emits
// `libz.so`/`libz.a`, and `crates/zlib-rs-differential/Cargo.toml` uses cargo's dependency-rename
// form to give the Rust path a useful name. That manifest is the authority.
//
// The `Z_*` constants come from here for BOTH implementations, and deliberately so:
// `src/oracle.rs` declares functions and type mirrors but publishes no constants, and these are
// `zlib.h` `#define`s rather than anything either library owns. `crates/libz-rs-sys/src/lib.rs`
// cites the header line for each one.
use libz_rs_sys::{
    ZLIB_VERSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH, Z_OK, Z_STREAM_END, Z_SYNC_FLUSH,
};

// The C reference. Every name here is declared in exactly one place --
// `crates/zlib-rs-differential/src/oracle.rs` -- and this file adds no `extern "C"` block and no
// `#[link]` attribute of its own.
use zlib_rs_differential::oracle;

// =================================================================================================
//  Row labels
// =================================================================================================

/// Row label for the C reference implementation.
///
/// A row under this name is `inflate.c` and `uncompr.c` themselves, compiled from the in-tree
/// sources by `crates/zlib-rs-differential/build.rs` with every export renamed to a `c_` prefix so
/// that both implementations can be linked into one binary.
const ORACLE_LABEL: &str = "c-oracle";

/// Row label for the port, reached through its exported C ABI.
///
/// ★ Stable across configurations, deliberately. criterion keys a saved baseline by benchmark id,
/// so a label that changed with the feature set would make the two runs of the
/// `--save-baseline` / `--baseline-lenient` procedure non-comparable. The configuration is
/// reported in the startup banner and carried by the baseline name instead; see
/// [`report_configuration`].
const PORT_LABEL: &str = "zlib-rs";

/// The stable prefix every stderr line from this suite carries, so that a log can be filtered.
const LOG_PREFIX: &str = "inflate_bench:";

/// The greppable per-case ratio line's leading key. Documented in the module header, because the
/// criterion regression job of AAP §0.4.1.6 consumes it.
const RATIO_KEY: &str = "RATIO";

/// The greppable per-group aggregate line's leading key.
const RATIO_SUMMARY_KEY: &str = "RATIO-SUMMARY";

// =================================================================================================
//  The gate
// =================================================================================================

/// The throughput gate of AAP §0.8.4: the port may be at most 10% slower than the C reference.
///
/// Expressed as a ratio of per-operation times, so 1.10 is the limit and a larger number is a
/// regression. Reported, never enforced -- see the module header.
const GATE_RATIO: f64 = 1.10;

/// The smallest payload whose ratio is counted against [`GATE_RATIO`]: one 4096-byte page.
///
/// ★ Not a loophole, and worth reading before changing. The gate is stated as a *throughput* bound,
/// and throughput is bytes per second. Decoding an `N`-byte payload costs a fixed amount plus `N`
/// divided by the true rate, so the rate a measurement recovers approaches the true rate only when
/// the second term dominates. Below a page it does not: the 23-byte and 45-byte committed fixtures
/// are measured in a few hundred nanoseconds, essentially all of it entry validation, stream reset
/// and the two `Z_SYNC_FLUSH` calls it takes to reach the trailer. Their ratio is a real and useful
/// number -- it is the per-call overhead ratio -- but it is not the quantity AAP §0.8.4 bounds, and
/// AAP §0.6.4.6 reads that quantity from the multi-megabyte Silesia members.
///
/// So every case still gets a `RATIO` line with its own `verdict`, and nothing is hidden. What the
/// floor changes is the `gate=` field: a case at or above it is `counted` and enters the group's
/// `over` tally, and a case below it is `informational` and is tallied separately. A reader who
/// wants the small-payload comparison reads the lines; a workflow that wants the gate reads
/// `over` on the `RATIO-SUMMARY` line.
///
/// 4096 rather than a rounder-sounding number because it is a page, it is the same "long enough to
/// amortize, short enough to stay in L1" length `benches/checksum_bench.rs` uses for its backend
/// sweep, and it admits every committed fixture whose decode is rate-dominated -- `random.bin` at
/// 8 KiB, `repetitive.bin` at 16 KiB, `window_boundary.bin` at 65 KiB -- and every Silesia member.
const GATE_MIN_BYTES: usize = 4096;

// =================================================================================================
//  Compression parameters for the inputs this suite decodes
// =================================================================================================

/// The three compression levels the AAP §0.8.4 gate names, and the reason a decoder cares.
///
/// The encoder's choices change the decoder's work profile, because they change what is in the
/// stream. `deflate.c`'s `configuration_table[10]` (L112-L124) binds level 1 to `deflate_fast`
/// with `max_chain` 4, level 6 to `deflate_slow` with `max_chain` 128, and level 9 to
/// `deflate_slow` with `max_chain` 4096: a level-1 stream carries more literals and shorter
/// matches, a level-9 stream fewer symbols and longer copies. Decoding those is not the same work,
/// so the level is an axis here even though this suite never measures compression.
const LEVELS: [c_int; 3] = [1, 6, 9];

/// The level used wherever the level is held fixed so another axis can vary.
///
/// 6 is `Z_DEFAULT_COMPRESSION`'s resolved value and the level the overwhelming majority of
/// callers actually get.
const DEFAULT_LEVEL: c_int = 6;

/// `DEF_MEM_LEVEL` -- what `compress2` and `gz_init` pass (`zutil.h` L81).
///
/// Held at the default because a benchmark must not change what the encoder emits: AAP §0.7.1(c)
/// confines this suite to observing the default configuration.
const DEFAULT_MEM_LEVEL: c_int = 8;

/// One of the three container formats, and the `windowBits` that selects it.
///
/// All three are measured because each takes a different path through `inflate.c`'s state machine
/// before it reaches a single compressed byte, and a different one after the last: a zlib stream
/// has a two-byte header and a four-byte Adler-32 trailer, a raw stream has neither, and a gzip
/// stream has a ten-byte minimum header with optional fields and a CRC-32-plus-length trailer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Container {
    /// RFC 1950 zlib: `windowBits` 15, the default wrapper.
    Zlib,
    /// RFC 1951 raw DEFLATE: `windowBits` −15, no wrapper at all.
    Raw,
    /// RFC 1952 gzip: `windowBits` 31, that is 15 with the `+16` gzip addend.
    Gzip,
}

impl Container {
    /// The `windowBits` argument that selects this container, for both `deflateInit2_` and
    /// `inflateInit2_`.
    ///
    /// 15 is `MAX_WBITS`; negating it requests the raw format and adding 16 requests gzip, exactly
    /// as `zlib.h` documents for each entry point.
    const fn window_bits(self) -> c_int {
        match self {
            Self::Zlib => 15,
            Self::Raw => -15,
            Self::Gzip => 31,
        }
    }

    /// The short, filesystem-safe token this container contributes to a benchmark id.
    ///
    /// No `/` anywhere in an id: criterion turns an id into a path under `target/criterion`, so a
    /// separator would silently become a directory level.
    const fn tag(self) -> &'static str {
        match self {
            Self::Zlib => "zlib",
            Self::Raw => "raw",
            Self::Gzip => "gzip",
        }
    }
}

/// The container used wherever the container is held fixed so another axis can vary.
const DEFAULT_CONTAINER: Container = Container::Zlib;

/// All three containers, in the order a reader wants them: the default, then the two variants.
const CONTAINERS: [Container; 3] = [Container::Zlib, Container::Raw, Container::Gzip];

// =================================================================================================
//  How the decoder is fed
// =================================================================================================

/// The reference driver's chunk size: `0x8000`, that is 32768 bytes.
///
/// `contrib/testzlib/testzlib.c` L147-L148 sets both `BlockSizeCompress` and
/// `BlockSizeUncompress` to `0x8000`, and the decompression loop at L245-L254 offers exactly that
/// much input and output per round. It is also the 32 KiB window size, which makes it the
/// most natural chunk for a decoder to receive.
const CHUNK_DEFAULT: usize = 0x8000;

/// A deliberately tiny chunk, to expose per-call overhead.
///
/// 16 bytes: far below anything the decoder can amortize, so this row measures the cost of
/// crossing the C ABI and re-entering the resumable state machine rather than the cost of
/// decoding. It is also the size at which AAP §0.3.2.4's codegen lever was measured at roughly a
/// 10% gain, which makes it the row that lever should move.
const CHUNK_SMALL: usize = 16;

/// A kilobyte: the smallest chunk at which per-call overhead is comfortably amortized, and a
/// common size for a caller feeding a socket or a file in pieces.
const CHUNK_MEDIUM: usize = 1024;

/// How one decode is driven.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Feeding {
    /// One `inflate` call with the whole input and the whole output buffer, flushing `Z_FINISH`.
    ///
    /// `Z_FINISH` with enough output space is the decoder's own fast path: `inflate.c` can write
    /// straight into the caller's buffer and skip the sliding-window copy that a resumable call has
    /// to perform. That is why it is measured separately rather than mixed into the chunked numbers.
    ///
    /// Do not expect it to be uniformly the fastest row, though — measured on this corpus, the port
    /// is faster single-shot than chunked on both axis fixtures while the reference is *slower*
    /// single-shot on `repetitive.bin`, which is exactly the kind of asymmetry a separate row is for.
    SingleShot,
    /// The reference driver's loop, at the given chunk size, flushing `Z_SYNC_FLUSH`.
    ///
    /// Faithful to `contrib/testzlib/testzlib.c` L245-L254.
    Chunked(usize),
}

impl Feeding {
    /// The short, filesystem-safe token this feeding mode contributes to a benchmark id.
    fn tag(self) -> String {
        match self {
            Self::SingleShot => String::from("single"),
            Self::Chunked(chunk) => format!("chunk{chunk}"),
        }
    }
}

/// The feeding mode used wherever the feeding mode is held fixed so another axis can vary.
///
/// Chunked at 32768, because that is what the reference driver does and what a streaming caller
/// does; the single-shot fast path is the special case, not the norm.
const DEFAULT_FEEDING: Feeding = Feeding::Chunked(CHUNK_DEFAULT);

/// Every feeding mode, in decreasing order of how much work each call is given.
///
/// That is the order the *shape* of the feed varies in, not a prediction of the resulting rates; see
/// the note on [`Feeding::SingleShot`] for why the two implementations do not rank identically.
const FEEDINGS: [Feeding; 4] = [
    Feeding::SingleShot,
    Feeding::Chunked(CHUNK_DEFAULT),
    Feeding::Chunked(CHUNK_MEDIUM),
    Feeding::Chunked(CHUNK_SMALL),
];

/// One byte of slack past the known uncompressed length, for every decode buffer.
///
/// Without it the last `inflate` call can fill the buffer exactly, return `Z_OK` with
/// `avail_out == 0`, and never be given the chance to read the four-byte Adler-32 or the eight-byte
/// gzip trailer that turns the status into `Z_STREAM_END`. The reference driver reaches the same
/// end by over-allocating a whole chunk (`testzlib.c` L227); one byte is all that is actually
/// required, because the trailer is consumed from `avail_in` and produces no output.
const OUTPUT_SLACK: usize = 1;

// =================================================================================================
//  Tier 1 -- the committed minimal corpus
// =================================================================================================

/// The committed fixtures this suite decodes, by exact filename.
///
/// `corpus/README.md` pins the inventory and requires that fixtures be loaded by exact name: there
/// is no globbing and no directory scan, so a name that does not match is a missing file rather
/// than a skipped case. These five are the ones whose *decode* profiles differ:
///
/// * `repetitive.bin` (16 KiB) -- long matches and the largest expansion ratio, so the copy loop
///   of the decoder's fast path dominates.
/// * `random.bin` (8 KiB) -- incompressible, so the encoder selects stored blocks and the decoder
///   is essentially a bounded `memcpy` with header parsing around it.
/// * `text.txt` (45 bytes) -- natural-language text, short enough that per-call overhead shows.
/// * `binary.bin` (23 bytes) -- shorter still, and the shortest row in the throughput groups.
/// * `window_boundary.bin` (65 KiB) -- the only committed fixture past the 32 KiB window, and
///   therefore the only real-data case that reaches `updatewindow` (`inflate.c` L252) and exercises
///   `whave`/`wnext` bookkeeping across a boundary.
///
/// `empty.bin` is deliberately absent: a zero-byte case would hand criterion a
/// `Throughput::Bytes(0)` to divide by. Its behaviour is contract, tested where contract is tested.
/// `single_byte.bin`, `gray_list.bin` and `dictionary.bin` are absent because each exists to drive
/// an *encoder* decision -- the degenerate Huffman fixup, the data-type fall-through, the preset
/// dictionary -- and none of them changes how the decoder spends its time.
const FIXTURE_NAMES: [&str; 5] = [
    "repetitive.bin",
    "random.bin",
    "text.txt",
    "binary.bin",
    "window_boundary.bin",
];

/// The fixtures the lifecycle group uses, smallest first.
///
/// The full stream lifecycle is dominated by `inflateInit2_`'s allocation and `inflateEnd`'s free,
/// so the interesting axis here is payload size rather than content class: `hello.bin` is 14 bytes
/// and is almost entirely setup cost, while `window_boundary.bin` at 65 KiB is almost entirely
/// decode. `hello.bin` appears here and nowhere else in this file -- it is `test/example.c`'s own
/// payload (L35 plus its NUL), which makes it the smallest payload the existing suite has always
/// fed.
const LIFECYCLE_FIXTURE_NAMES: [&str; 4] = [
    "hello.bin",
    "binary.bin",
    "repetitive.bin",
    "window_boundary.bin",
];

/// The fixtures whose size makes them worth using when one axis is being varied and the others are
/// held fixed.
///
/// Two, not five: the container and feeding-mode groups multiply out, and a five-fixture cross
/// product would triple the run time to restate what the level group already shows. `repetitive.bin`
/// is the maximum-expansion case and `window_boundary.bin` is the past-the-window case, which are
/// the two shapes those axes can actually interact with.
const AXIS_FIXTURE_NAMES: [&str; 2] = ["repetitive.bin", "window_boundary.bin"];

/// `<this crate>/corpus/minimal`, resolved from the manifest directory at compile time.
///
/// ★ `CARGO_MANIFEST_DIR` is the **host package's** directory for a bench target -- that is
/// `crates/zlib-rs-differential`, the crate whose manifest carries the `[[bench]]` entry that
/// attaches this file -- and not the `benches/` directory the file sits in. Never an absolute path
/// baked into the source, and never derived from the current directory, which cargo does not
/// guarantee for a bench binary.
fn minimal_corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join("minimal")
}

/// The repository root, which is `<CARGO_MANIFEST_DIR>/../..`.
///
/// The only consumer is [`silesia_dir`]'s default branch. Kept as its own named function because
/// the two `..` components are the other half of the `CARGO_MANIFEST_DIR` surprise described on
/// [`minimal_corpus_dir`], and a reader should find both facts stated once each rather than inlined
/// at a use site.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every fixture from `names` that is present and non-empty, loaded fresh.
///
/// Absence is not an error. A fixture that cannot be read, or that is empty and therefore has no
/// rate to report, is named on stderr and left out of the returned set, so the run continues and
/// the remaining cases still produce numbers. `corpus/README.md` treats a missing committed fixture
/// as a repository problem rather than an optional input, and this diagnostic says so; it is the
/// correctness suites under `tests/` that turn it into a failure.
fn load_fixtures(names: &[&'static str]) -> Vec<Fixture> {
    let dir = minimal_corpus_dir();
    let mut loaded = Vec::with_capacity(names.len());

    for name in names {
        let path = dir.join(name);
        match fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => {
                eprintln!(
                    "{LOG_PREFIX} skipping fixture {name}: {} is empty, and a zero-byte case has \
                     no rate to report",
                    path.display()
                );
            }
            Ok(bytes) => loaded.push(Fixture { name, bytes }),
            Err(error) => {
                eprintln!(
                    "{LOG_PREFIX} skipping fixture {name}: cannot read {}: {error}. \
                     corpus/README.md pins this exact filename, so a name that does not match is a \
                     missing file rather than a skipped case -- the remaining cases still run and \
                     nothing is downloaded.",
                    path.display()
                );
            }
        }
    }

    if loaded.is_empty() {
        eprintln!(
            "{LOG_PREFIX} no committed fixture could be read from {}; the affected groups will be \
             empty.",
            dir.display()
        );
    }

    loaded
}

/// One loaded input: the name a benchmark id will carry, and the bytes to decode.
#[derive(Debug)]
struct Fixture {
    /// The exact filename, used verbatim as the fixture component of every benchmark id.
    name: &'static str,
    /// The uncompressed bytes, exactly as committed.
    bytes: Vec<u8>,
}

/// The tier-1 corpus, loaded once for the whole process.
///
/// The [`OnceLock`] is what makes "reported once" literal: four groups ask for overlapping fixture
/// sets and none of them should reprint a diagnostic. All ten committed fixtures together are under
/// 92 KiB, so caching them costs nothing; the tier-2 corpus is hundreds of megabytes and is
/// deliberately *not* cached this way -- see [`silesia_members`].
fn tier1_corpus() -> &'static [Fixture] {
    static CACHE: OnceLock<Vec<Fixture>> = OnceLock::new();

    CACHE.get_or_init(|| {
        // The union of the three name lists, in FIXTURE_NAMES order first so that the gate group's
        // fixtures are loaded and reported before any other. `hello.bin` is the only name the other
        // two lists add.
        let mut names: Vec<&'static str> = Vec::with_capacity(FIXTURE_NAMES.len() + 1);
        names.extend_from_slice(&FIXTURE_NAMES);
        for name in LIFECYCLE_FIXTURE_NAMES
            .into_iter()
            .chain(AXIS_FIXTURE_NAMES)
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }

        load_fixtures(&names)
    })
}

/// The loaded fixtures named by `names`, in the order `names` gives them.
///
/// A name with no loaded fixture behind it is simply absent from the result: [`load_fixtures`] has
/// already explained why on stderr, and repeating it per group would bury the explanation in noise.
fn selected(names: &[&'static str]) -> Vec<&'static Fixture> {
    let corpus = tier1_corpus();
    names
        .iter()
        .filter_map(|name| corpus.iter().find(|fixture| fixture.name == *name))
        .collect()
}

// =================================================================================================
//  Tier 2 -- Silesia, opt-in and never fetched
// =================================================================================================

/// The twelve members `corpus/README.md` pins as the Silesia inventory, in its tabulated order.
///
/// `corpus/fetch_silesia.sh` enforces exactly this list after extraction through its
/// `ZLIB_RS_SILESIA_MEMBERS` default, and rejects an archive that is missing a member or carries an
/// extra one -- so an archive that is not this corpus is refused rather than measured. This file
/// consumes the same list, by exact name, so that the two never disagree about what the corpus is.
#[rustfmt::skip]
const SILESIA_MEMBERS: [&str; 12] = [
    "dickens", "mozilla", "mr",      "nci",     "ooffice", "osdb",
    "reymont", "sao",     "samba",   "webster", "x-ray",   "xml",
];

/// The environment variable that overrides where the Silesia corpus lives.
const SILESIA_DIR_VAR: &str = "ZLIB_RS_SILESIA_DIR";

/// Where to obtain the corpus, quoted in the skip note so a reader does not have to go looking.
const SILESIA_SCRIPT: &str = "crates/zlib-rs-differential/corpus/fetch_silesia.sh";

/// The Silesia corpus directory, by the one canonical resolution rule.
///
/// `corpus/README.md`'s "The Silesia tier: path contract" publishes exactly two steps, and
/// `corpus/fetch_silesia.sh`'s `resolve_destination` implements the same two:
///
/// 1. `$ZLIB_RS_SILESIA_DIR`, when it is set and non-empty.
/// 2. Otherwise `<repo-root>/target/silesia`.
///
/// An empty value counts as unset, which is what makes `ZLIB_RS_SILESIA_DIR= cargo bench` mean
/// "use the default" rather than "use the filesystem root" -- the same reading the script's
/// `${ZLIB_RS_SILESIA_DIR:-}` default gives it. The default sits under `target/` because the root
/// `.gitignore` ignores `/target/`, so a fetched corpus can never be committed by accident.
///
/// This function reads an environment variable and joins paths. It does not create the directory,
/// probe the network, or spawn anything -- and neither does anything else in this file.
fn silesia_dir() -> PathBuf {
    match std::env::var(SILESIA_DIR_VAR) {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        Ok(_) | Err(_) => repository_root().join("target").join("silesia"),
    }
}

/// The Silesia member files that are present, as paths, or an empty vector with one note.
///
/// The canonical [`SILESIA_MEMBERS`] names are resolved by exact name first, exactly as tier 1 is.
/// A directory holding some of the twelve is measured over those; a directory holding none of them
/// but holding other readable files is measured over the first twelve of those, sorted, with a note
/// saying plainly that it is not the pinned inventory -- that keeps a hand-populated or partially
/// extracted directory usable for a local throughput run without ever pretending it is the corpus
/// AAP §0.8.4 names.
///
/// **Absence is normal, not an error.** `corpus/README.md` is explicit: a benchmark run without a
/// fetched corpus reports, skips and exits zero, and under no circumstances downloads. This
/// function returns an empty vector and prints one note naming [`SILESIA_SCRIPT`]; the caller skips
/// its group and the run still succeeds. The note is printed once per process, because both Silesia
/// rows ask the same question.
fn silesia_members() -> &'static [PathBuf] {
    static CACHE: OnceLock<Vec<PathBuf>> = OnceLock::new();

    CACHE.get_or_init(|| {
        let dir = silesia_dir();

        let canonical: Vec<PathBuf> = SILESIA_MEMBERS
            .iter()
            .map(|member| dir.join(member))
            .filter(|path| path.is_file())
            .collect();

        if !canonical.is_empty() {
            eprintln!(
                "{LOG_PREFIX} Silesia: {} of the {} pinned members found under {}.",
                canonical.len(),
                SILESIA_MEMBERS.len(),
                dir.display()
            );
            return canonical;
        }

        let salvaged = salvage_silesia_dir(&dir);
        if salvaged.is_empty() {
            eprintln!(
                "{LOG_PREFIX} Silesia not present under {} -- skipping the tier-2 groups. This is \
                 normal and not an error: tier 1 covers every correctness gate and this run exits \
                 zero. To measure the corpus AAP §0.8.4 names, fetch it yourself with \
                 {SILESIA_SCRIPT} (opt-in, run by a human, never by cargo or CI), or point \
                 {SILESIA_DIR_VAR} at a directory that already holds it.",
                dir.display()
            );
        }

        salvaged
    })
}

/// Up to twelve readable regular files from `dir`, sorted, when none of the pinned members is there.
///
/// The sort is what makes the selection deterministic across runs and machines; the cap is what
/// keeps a directory that happens to hold thousands of files from turning a benchmark into an
/// afternoon. Hidden entries are skipped so that an editor's or a fetcher's bookkeeping file is
/// never measured as though it were data.
fn salvage_silesia_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| !name.starts_with('.'))
        })
        .collect();

    if found.is_empty() {
        return found;
    }

    found.sort();
    found.truncate(SILESIA_MEMBERS.len());

    eprintln!(
        "{LOG_PREFIX} Silesia: none of the {} pinned members is present under {}, but {} other \
         readable file(s) are -- measuring those instead. This is NOT the inventory \
         corpus/README.md pins and {SILESIA_SCRIPT} enforces, so the resulting numbers are a local \
         signal and not the AAP §0.8.4 Silesia measurement.",
        SILESIA_MEMBERS.len(),
        dir.display(),
        found.len()
    );

    found
}

// =================================================================================================
//  Checked conversions
// =================================================================================================
//
//  Lengths are converted with `try_from`, never with `as`. Two reasons, and the second is the one
//  that matters: `clippy::pedantic` denies the cast family, and on a target where `uInt` is
//  narrower than `usize` an `as` cast would silently truncate a large case into a small one and
//  report a throughput figure for work that was never done.

/// `(int) sizeof(T)`, where `T` is **that side's own** `z_stream` mirror.
///
/// This is the second half of the handshake the `inflateInit2` macro arranges (`zlib.h` L1939-L1941)
/// and it must never be crossed between sides: `zlib_rs_differential::oracle::z_stream` and the
/// facade's `z_stream` are distinct Rust types that happen to have identical layout, and each
/// library compares the number it is given against its *own* `sizeof`.
///
/// Checked rather than cast. On the impossible failure -- a `z_stream` larger than `INT_MAX`, which
/// would mean the ABI mirror had grown past anything `zlib.h` could describe -- it yields 0, and
/// every `*Init*_` entry point rejects 0 with `Z_VERSION_ERROR` because 0 cannot equal
/// `sizeof(z_stream)`. That is a self-reporting fallback rather than a panic, which is what a
/// benchmark should have.
fn stream_size<T>() -> c_int {
    c_int::try_from(size_of::<T>()).unwrap_or(0)
}

/// The version string both `*Init*_` calls are given: `ZLIB_VERSION` from `zlib.h` L44, verbatim.
///
/// This is precisely what the C macros pass -- the *caller's* compile-time constant -- and passing
/// it to the oracle therefore exercises the reference's real major-version check rather than
/// side-stepping it. It is a `&CStr` constant with static storage duration, so `as_ptr` yields a
/// NUL-terminated pointer that outlives every call; nothing allocates a `CString`.
///
/// Be precise about what that check is, because it is narrower than it looks and the difference
/// matters when reading a failure. `deflate.c` L394 and `inflate.c` L178 compare **only
/// `version[0]`** -- the major digit -- against the library's own, and the facade documents and tests
/// the same rule. Measured against both implementations from this file: a wrong major such as
/// `"2.3.2.1-motley"` makes every `*Init*_` answer `Z_VERSION_ERROR` (−6), while the shorthand
/// `"1.3.2"` is *accepted*, because its major digit agrees. So the shorthand is wrong for a different
/// reason than a failed handshake -- it is not the string this library reports, and `zlibVersion` and
/// the `test/example.c` startup check are about identity rather than about the init gate. Passing the
/// `zlib.h` L44 constant verbatim is what a real caller does and is the only spelling that is right on
/// both counts. [`report_configuration`] prints the reference's own `c_zlibVersion()` beside this
/// constant so that a drift is visible in the output rather than only in a failure.
fn version_ptr() -> *const c_char {
    ZLIB_VERSION.as_ptr()
}

/// The outcome of one decode: the status the library returned and how many bytes it produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Decode {
    /// The `inflate` return code from the call that ended the drive.
    status: c_int,
    /// Total bytes written into the output buffer across the whole drive.
    produced: usize,
}

// =================================================================================================
//  Zeroed streams
// =================================================================================================

/// A facade `z_stream` with every field zeroed and the allocator hooks left null.
///
/// The `memset(&strm, 0, sizeof strm)` a C caller performs before an `*Init*_` call
/// (`contrib/testzlib/testzlib.c` L238 does exactly that). Built field by field rather than through
/// `MaybeUninit::zeroed`, which is both stricter and cheaper: it needs no `unsafe`, and it makes the
/// three members that actually carry meaning visible as the deliberate choices they are. `None` is
/// C's `Z_NULL` for the two hooks, which selects the library's own internal allocator.
///
/// `crates/libz-rs-sys` deliberately does not implement `Default` for `z_stream`, on the grounds
/// that a zeroed stream is not yet a valid one; this function is the local spelling of the same
/// thing, named so that its single purpose is obvious.
fn zeroed_port_stream() -> libz_rs_sys::z_stream {
    libz_rs_sys::z_stream {
        next_in: core::ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: core::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: core::ptr::null(),
        state: core::ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: core::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// An oracle `z_stream` with every field zeroed.
///
/// The oracle's mirror does implement `Default`, and its documentation says so in the same terms:
/// `Z_NULL` is 0 for every field that has one, so an all-zero stream is exactly what the documented
/// initialisation sequence expects. Named explicitly rather than reached through
/// `Default::default()` so that the two sides read alike at every call site.
fn zeroed_oracle_stream() -> oracle::z_stream {
    oracle::z_stream::default()
}

// =================================================================================================
//  Window installation
// =================================================================================================
//
//  `contrib/testzlib/testzlib.c` L241-L242 sets `next_in` and `next_out` ONCE, before the loop,
//  because the library advances both itself as it consumes and produces. These two helpers do the
//  same, and the drivers below refresh only `avail_in` and `avail_out` per round -- which is what
//  makes the chunked driver a faithful reproduction rather than an approximation of it.
//
//  A zero-length input becomes a NULL `next_in`, the pairing `zlib.h` L91-L92 explicitly permits.
//  No decode in this suite is offered a zero-length output window, because `inflate` answers
//  `Z_BUF_ERROR` for `avail_out == 0` and a run that tripped that would be measuring the harness.

/// Point the port's stream at `input` and `out`, leaving both `avail_*` counts to the driver.
fn install_port(strm: &mut libz_rs_sys::z_stream, input: &[u8], out: &mut [u8]) {
    strm.next_in = if input.is_empty() {
        core::ptr::null()
    } else {
        input.as_ptr()
    };
    strm.next_out = out.as_mut_ptr();
}

/// Point the oracle's stream at `input` and `out`. Identical rules; a different `z_stream` type.
fn install_oracle(strm: &mut oracle::z_stream, input: &[u8], out: &mut [u8]) {
    strm.next_in = if input.is_empty() {
        core::ptr::null()
    } else {
        input.as_ptr()
    };
    strm.next_out = out.as_mut_ptr();
}

// =================================================================================================
//  RAII stream guards -- the whole reason no path can leak
// =================================================================================================
//
//  ★ A GUARD MUST NOT BE MOVED ONCE IT IS LIVE, and this is a hard requirement rather than a
//  stylistic one. `inflate.c` L203 stores a pointer back to the stream inside the state block
//  (`state->strm = strm`) and L94's `inflateStateCheck` compares it by identity on entry to every
//  other entry point, so an oracle stream that changed address after `c_inflateInit2_` would fail
//  every subsequent call with `Z_STREAM_ERROR`. The port is immune -- its state carries an allocator
//  rather than a back-pointer -- but the harness must be correct for both, so the discipline is
//  uniform: construct the guard, bind it to a local, and only then call `init` through `&mut`.
//  Never `init` inside an expression whose value is then moved.
//
//  The guards mirror `z_stream zcpr;` in the reference driver: an inline stream in the caller's
//  frame, not a heap allocation. Boxing would make moving safe but would add one global-allocator
//  round trip per stream that the C driver does not pay, which the lifecycle group exists to
//  measure precisely.
//
//  Every guard's `Drop` calls the matching `*End`, so a skip path, an early return and a panicking
//  unwind all release the stream. That is what keeps this file clean under the nightly
//  `AddressSanitizer` job, which is the verification gate this crate does sit inside.

/// The port's inflate stream, ended on every path by [`Drop`].
#[derive(Debug)]
struct PortInflate {
    /// The stream itself. Must not move once [`PortInflate::live`] is set -- see the section note.
    strm: libz_rs_sys::z_stream,
    /// Whether `inflateInit2_` succeeded and `inflateEnd` therefore owes a call.
    live: bool,
}

impl PortInflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_port_stream(),
            live: false,
        }
    }

    /// `inflateInit2_(strm, windowBits, ZLIB_VERSION, sizeof(z_stream))` -- `zlib.h` L1911.
    fn init(&mut self, window_bits: c_int) -> c_int {
        // SAFETY: `strm` is a live, zeroed, correctly-typed local of this side's own `z_stream`
        // type that outlives the call; `version` points at `ZLIB_VERSION`, a NUL-terminated
        // `'static` byte string; and `stream_size` is `size_of` of that same type, never the other
        // side's. `inflateInit2_` reads at most one byte of `version` after testing it for null,
        // writes `state`, and touches neither window -- which is why a C caller may leave
        // `next_in`/`next_out` unset until after the call.
        let status = unsafe {
            libz_rs_sys::inflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                window_bits,
                version_ptr(),
                stream_size::<libz_rs_sys::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// `inflateReset2(strm, windowBits)` -- `zlib.h` L995.
    ///
    /// Returns the stream to the start of a fresh stream while keeping the window allocation when
    /// the window size is unchanged, which is exactly why the steady-state group uses it: what is
    /// then timed is the decode plus the cheap half of the lifecycle, and not the allocator.
    fn reset(&mut self, window_bits: c_int) -> c_int {
        // SAFETY: as for `init`, minus the version handshake. `strm` is live and initialised, so
        // `inflateReset2` finds the state block it wrote; it reallocates the window only if
        // `windowBits` changed, and touches neither the input nor the output window.
        unsafe { libz_rs_sys::inflateReset2(core::ptr::addr_of_mut!(self.strm), window_bits) }
    }
}

impl Drop for PortInflate {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        // SAFETY: `strm` is live and was initialised by `inflateInit2_` on this same address.
        // `inflateEnd` frees the state through the same allocator that produced it and nulls
        // `state`, so this runs at most once per stream and a second call would be a documented
        // no-op rather than a double free.
        unsafe { libz_rs_sys::inflateEnd(core::ptr::addr_of_mut!(self.strm)) };
    }
}

/// The reference's inflate stream, ended on every path by [`Drop`].
#[derive(Debug)]
struct OracleInflate {
    /// The stream itself. Must not move once [`OracleInflate::live`] is set -- and here the
    /// requirement has teeth, because `inflate.c` L94 checks `state->strm` by identity.
    strm: oracle::z_stream,
    /// Whether `c_inflateInit2_` succeeded and `c_inflateEnd` therefore owes a call.
    live: bool,
}

impl OracleInflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_oracle_stream(),
            live: false,
        }
    }

    /// `inflateInit2_` in the reference, reached as `c_inflateInit2_`.
    fn init(&mut self, window_bits: c_int) -> c_int {
        // SAFETY: as for `PortInflate::init`, with `stream_size` taken from the ORACLE's own
        // `z_stream` mirror rather than the facade's -- the two are distinct Rust types and each
        // library compares the number against its own `sizeof`.
        let status = unsafe {
            oracle::c_inflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                window_bits,
                version_ptr(),
                stream_size::<oracle::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// `inflateReset2` in the reference, reached as `c_inflateReset2`.
    fn reset(&mut self, window_bits: c_int) -> c_int {
        // SAFETY: as for `PortInflate::reset`. `strm` is live, initialised at this address, and
        // still the address the state block's back-pointer holds.
        unsafe { oracle::c_inflateReset2(core::ptr::addr_of_mut!(self.strm), window_bits) }
    }
}

impl Drop for OracleInflate {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        // SAFETY: as for `PortInflate::drop`, against the reference's own allocator.
        unsafe { oracle::c_inflateEnd(core::ptr::addr_of_mut!(self.strm)) };
    }
}

/// The reference's deflate stream, used only to build the inputs this suite decodes.
///
/// Setup only: no timed closure ever touches one of these. The compressed inputs are produced by
/// the *reference* encoder on purpose -- the honest baseline for "how fast does the port decode real
/// zlib output" is real zlib output, and decoding it exercises the C-to-Rust direction of AAP §0.8.1
/// directive 4 on every case as a side effect.
#[derive(Debug)]
struct OracleDeflate {
    /// The stream itself. Must not move once [`OracleDeflate::live`] is set; `deflate.c` keeps the
    /// same kind of back-pointer `inflate.c` does.
    strm: oracle::z_stream,
    /// Whether `c_deflateInit2_` succeeded and `c_deflateEnd` therefore owes a call.
    live: bool,
}

impl OracleDeflate {
    /// A zeroed, not-yet-initialised stream. Bind the result to a local before calling `init`.
    fn new() -> Self {
        Self {
            strm: zeroed_oracle_stream(),
            live: false,
        }
    }

    /// `deflateInit2_(strm, level, Z_DEFLATED, windowBits, DEF_MEM_LEVEL, Z_DEFAULT_STRATEGY, ...)`
    /// -- `zlib.h` L1907-L1910.
    ///
    /// Every parameter but the level and the window bits is held at the default the shipped library
    /// uses, because AAP §0.7.1(c) forbids this suite from changing what the encoder emits.
    fn init(&mut self, level: c_int, window_bits: c_int) -> c_int {
        // SAFETY: as for `OracleInflate::init` -- a live zeroed local of the oracle's own type, the
        // `'static` NUL-terminated version string, and that type's own `size_of`. `deflateInit2_`
        // writes `state` and touches neither window.
        let status = unsafe {
            oracle::c_deflateInit2_(
                core::ptr::addr_of_mut!(self.strm),
                level,
                Z_DEFLATED,
                window_bits,
                DEFAULT_MEM_LEVEL,
                Z_DEFAULT_STRATEGY,
                version_ptr(),
                stream_size::<oracle::z_stream>(),
            )
        };
        self.live = status == Z_OK;
        status
    }

    /// `deflateBound(strm, sourceLen)` -- `zlib.h` L768 -- as a `usize`, or `None` if it does not
    /// fit one.
    ///
    /// This is what sizes the setup compressor's output buffer, in preference to the reference
    /// driver's `size + size / 0x10 + 0x200` guard (`testzlib.c` L183): `deflateBound` is the
    /// library's own exact upper bound, and its numeric value is gated by
    /// `crates/zlib-rs-differential/tests/byte_identical.rs`. It must be called after `init`,
    /// because it reads `wrap`, `w_bits` and `hash_bits` out of the state block.
    fn bound(&mut self, source_len: usize) -> Option<usize> {
        let source = oracle::uLong::try_from(source_len).ok()?;
        // SAFETY: `strm` is live and initialised at this address. `deflateBound` only reads the
        // state block and writes nothing; it touches neither window, so no pointer needs to be
        // installed first.
        let bound = unsafe { oracle::c_deflateBound(core::ptr::addr_of_mut!(self.strm), source) };
        usize::try_from(bound).ok()
    }
}

impl Drop for OracleDeflate {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        // SAFETY: as for `OracleInflate::drop`. `deflateEnd` frees the state through the allocator
        // that produced it and nulls `state`.
        unsafe { oracle::c_deflateEnd(core::ptr::addr_of_mut!(self.strm)) };
    }
}

// =================================================================================================
//  Setup: produce the compressed input with the reference encoder
// =================================================================================================

/// Compress `data` with the C reference at `level` into `container`, in one `Z_FINISH` call.
///
/// Setup only, and never reachable from a timed closure. Returns `None` and prints why on any
/// failure, so the caller skips the case instead of measuring one that cannot be trusted.
///
/// One shot rather than the reference driver's chunked encode loop (`testzlib.c` L204-L213), and
/// deliberately: the *bytes* are what this suite consumes and they are identical either way, while a
/// single `Z_FINISH` call is simpler to read and cannot leave a partially flushed stream behind if
/// something goes wrong. The encoder's speed is not measured here at all -- that is
/// `benches/deflate_bench.rs`'s subject.
fn compress_with_oracle(data: &[u8], level: c_int, container: Container) -> Option<Vec<u8>> {
    let window_bits = container.window_bits();

    let mut deflater = OracleDeflate::new();
    let init = deflater.init(level, window_bits);
    if init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} cannot build an input: c_deflateInit2_(level={level}, \
             windowBits={window_bits}) returned {init}",
        );
        return None;
    }

    let bound = deflater.bound(data.len())?;
    let mut out = vec![0_u8; bound];

    let avail_in = oracle::uInt::try_from(data.len()).ok()?;
    let avail_out = oracle::uInt::try_from(out.len()).ok()?;
    install_oracle(&mut deflater.strm, data, &mut out);
    deflater.strm.avail_in = avail_in;
    deflater.strm.avail_out = avail_out;

    // SAFETY: `avail_in` bytes are readable at `next_in` and `avail_out` bytes writable at
    // `next_out`, both installed immediately above from live slices that outlive this call, and both
    // counts are the slices' own lengths converted with `try_from`. `strm` is live and initialised at
    // this address. `deflate` retains neither pointer past the return.
    let status = unsafe { oracle::c_deflate(core::ptr::addr_of_mut!(deflater.strm), Z_FINISH) };
    if status != Z_STREAM_END {
        eprintln!(
            "{LOG_PREFIX} cannot build an input: c_deflate(Z_FINISH) returned {status} for \
             {} bytes at level={level}, windowBits={window_bits} (deflateBound gave {bound})",
            data.len(),
        );
        return None;
    }

    let left = usize::try_from(deflater.strm.avail_out).ok()?;
    let produced = out.len().checked_sub(left)?;
    out.truncate(produced);

    Some(out)
}

// =================================================================================================
//  The decode drivers
// =================================================================================================
//
//  Four functions: two feeding modes on each side. They are written out per side rather than behind
//  a trait because `zlib_rs_differential::oracle::z_stream` and the facade's `z_stream` are distinct
//  Rust types with identical layout -- nothing is ever transmuted or aliased between them -- and
//  because a reader auditing one `unsafe` block should not have to follow a generic to find the type
//  it applies to.
//
//  None of them allocates, reads a file, prints, or compares anything: they are exactly the work the
//  criterion rows are supposed to be timing, and nothing else. A conversion that could not be
//  represented yields `None` silently, because the sanity check outside the timed region has already
//  established that it cannot happen for the case being measured.

/// One `inflate(Z_FINISH)` call with the whole input and the whole output buffer, through the port.
///
/// `Z_FINISH` with sufficient output space is the decoder's own fast path: `inflate.c` can write
/// straight into the caller's buffer and skip the sliding-window copy a resumable call must perform.
fn port_single_shot(stream: &mut PortInflate, input: &[u8], out: &mut [u8]) -> Option<Decode> {
    let out_len = out.len();
    let avail_in = libz_rs_sys::uInt::try_from(input.len()).ok()?;
    let avail_out = libz_rs_sys::uInt::try_from(out_len).ok()?;

    install_port(&mut stream.strm, input, out);
    stream.strm.avail_in = avail_in;
    stream.strm.avail_out = avail_out;

    // SAFETY: `avail_in` bytes are readable at `next_in` and `avail_out` bytes writable at
    // `next_out`, both installed immediately above from live slices that outlive this call, and both
    // counts are those slices' own lengths converted with `try_from`. `strm` is live, initialised at
    // this address, and reset by the caller when the caller intends a fresh stream.
    let status = unsafe { libz_rs_sys::inflate(core::ptr::addr_of_mut!(stream.strm), Z_FINISH) };

    let left = usize::try_from(stream.strm.avail_out).ok()?;
    Some(Decode {
        status,
        produced: out_len.checked_sub(left)?,
    })
}

/// The reference driver's decompression loop, through the port.
///
/// Faithful to `contrib/testzlib/testzlib.c` L245-L254: `next_in` and `next_out` are installed once
/// because the library advances them itself, and each round offers `min(remaining, chunk)` input and
/// a chunk of output space, calls `inflate` with `Z_SYNC_FLUSH`, and continues while the status is
/// `Z_OK`.
///
/// Two guards the reference loop does not need. `avail_out` is clamped to the space that remains,
/// rather than provisioned by over-allocating an extra chunk (`testzlib.c` L227), so the loop stops
/// when the buffer is full instead of overrunning it. And a round that consumes nothing and produces
/// nothing ends the drive: `inflate` cannot report `Z_OK` without progress, so this cannot trigger on
/// a well-formed stream, but it makes termination a property of the loop rather than a property of
/// the library.
fn port_chunked(
    stream: &mut PortInflate,
    input: &[u8],
    out: &mut [u8],
    chunk: usize,
) -> Option<Decode> {
    let out_len = out.len();
    install_port(&mut stream.strm, input, out);

    let mut in_left = input.len();
    let mut out_left = out_len;
    let mut status = Z_OK;

    while out_left > 0 {
        let offer_in = in_left.min(chunk);
        let offer_out = out_left.min(chunk);
        stream.strm.avail_in = libz_rs_sys::uInt::try_from(offer_in).ok()?;
        stream.strm.avail_out = libz_rs_sys::uInt::try_from(offer_out).ok()?;

        // SAFETY: as for `port_single_shot` -- `avail_in` bytes readable at `next_in` and
        // `avail_out` bytes writable at `next_out`, both derived from the live slices installed
        // before the loop, and both counts clamped to what those slices still hold. The library
        // advances the two pointers itself, so re-offering counts without re-installing pointers is
        // the documented way to resume.
        status =
            unsafe { libz_rs_sys::inflate(core::ptr::addr_of_mut!(stream.strm), Z_SYNC_FLUSH) };

        let consumed = offer_in.checked_sub(usize::try_from(stream.strm.avail_in).ok()?)?;
        let produced = offer_out.checked_sub(usize::try_from(stream.strm.avail_out).ok()?)?;
        in_left = in_left.checked_sub(consumed)?;
        out_left = out_left.checked_sub(produced)?;

        if status != Z_OK || (consumed == 0 && produced == 0) {
            break;
        }
    }

    Some(Decode {
        status,
        produced: out_len.checked_sub(out_left)?,
    })
}

/// One `inflate(Z_FINISH)` call through the reference. Same shape as [`port_single_shot`].
fn oracle_single_shot(stream: &mut OracleInflate, input: &[u8], out: &mut [u8]) -> Option<Decode> {
    let out_len = out.len();
    let avail_in = oracle::uInt::try_from(input.len()).ok()?;
    let avail_out = oracle::uInt::try_from(out_len).ok()?;

    install_oracle(&mut stream.strm, input, out);
    stream.strm.avail_in = avail_in;
    stream.strm.avail_out = avail_out;

    // SAFETY: as for `port_single_shot`, against the oracle's own `z_stream` type and its own
    // entry point. The stream has not moved since `c_inflateInit2_` wrote the back-pointer that
    // `inflate.c` L94 checks by identity.
    let status = unsafe { oracle::c_inflate(core::ptr::addr_of_mut!(stream.strm), Z_FINISH) };

    let left = usize::try_from(stream.strm.avail_out).ok()?;
    Some(Decode {
        status,
        produced: out_len.checked_sub(left)?,
    })
}

/// The reference driver's decompression loop, through the reference. Same shape as [`port_chunked`].
fn oracle_chunked(
    stream: &mut OracleInflate,
    input: &[u8],
    out: &mut [u8],
    chunk: usize,
) -> Option<Decode> {
    let out_len = out.len();
    install_oracle(&mut stream.strm, input, out);

    let mut in_left = input.len();
    let mut out_left = out_len;
    let mut status = Z_OK;

    while out_left > 0 {
        let offer_in = in_left.min(chunk);
        let offer_out = out_left.min(chunk);
        stream.strm.avail_in = oracle::uInt::try_from(offer_in).ok()?;
        stream.strm.avail_out = oracle::uInt::try_from(offer_out).ok()?;

        // SAFETY: as for `port_chunked`, against the oracle's own type and entry point.
        status = unsafe { oracle::c_inflate(core::ptr::addr_of_mut!(stream.strm), Z_SYNC_FLUSH) };

        let consumed = offer_in.checked_sub(usize::try_from(stream.strm.avail_in).ok()?)?;
        let produced = offer_out.checked_sub(usize::try_from(stream.strm.avail_out).ok()?)?;
        in_left = in_left.checked_sub(consumed)?;
        out_left = out_left.checked_sub(produced)?;

        if status != Z_OK || (consumed == 0 && produced == 0) {
            break;
        }
    }

    Some(Decode {
        status,
        produced: out_len.checked_sub(out_left)?,
    })
}

/// Drive the port for one whole stream, in whichever feeding mode `feeding` names.
fn port_decode(
    stream: &mut PortInflate,
    input: &[u8],
    out: &mut [u8],
    feeding: Feeding,
) -> Option<Decode> {
    match feeding {
        Feeding::SingleShot => port_single_shot(stream, input, out),
        Feeding::Chunked(chunk) => port_chunked(stream, input, out, chunk),
    }
}

/// Drive the reference for one whole stream, in whichever feeding mode `feeding` names.
fn oracle_decode(
    stream: &mut OracleInflate,
    input: &[u8],
    out: &mut [u8],
    feeding: Feeding,
) -> Option<Decode> {
    match feeding {
        Feeding::SingleShot => oracle_single_shot(stream, input, out),
        Feeding::Chunked(chunk) => oracle_chunked(stream, input, out, chunk),
    }
}

// =================================================================================================
//  The one-shot wrappers of `uncompr.c`
// =================================================================================================
//
//  A distinct entry path, and a heavily used one: `uncompress` (L97) forwards to `uncompress2` (L83),
//  which forwards to `uncompress2_z` (L29), which runs a whole stream internally -- `inflateInit`,
//  a loop, `inflateEnd`. Two consequences this suite depends on. The cost profile belongs beside the
//  lifecycle group rather than the steady-state one, because every call pays for a stream. And the
//  container is fixed at zlib, because `uncompress2_z` reaches `inflate` through `inflateInit` with
//  the default `windowBits`; a raw or gzip stream would need `inflateInit2` and there is no argument
//  to ask for one.

/// `uncompress(dest, &destLen, source, sourceLen)` -- `zlib.h` L1315 -- through the port.
///
/// `destLen` is in-out: it carries the buffer's capacity in and the produced length out, which is
/// why it is seeded from `dest.len()` on every call rather than once.
fn port_uncompress(dest: &mut [u8], source: &[u8]) -> Option<Decode> {
    let mut dest_len = libz_rs_sys::uLongf::try_from(dest.len()).ok()?;
    let source_len = libz_rs_sys::uLong::try_from(source.len()).ok()?;

    // SAFETY: `dest_len` bytes are writable at `dest.as_mut_ptr()` and `source_len` bytes readable
    // at `source.as_ptr()`, both counts being those live slices' own lengths converted with
    // `try_from`; `destLen` points at a live local that outlives the call. `uncompress` retains no
    // pointer past the return and allocates and frees its own stream internally.
    let status = unsafe {
        libz_rs_sys::uncompress(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
        )
    };

    Some(Decode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// `uncompress2(dest, &destLen, source, &sourceLen)` -- `zlib.h` L1330 -- through the port.
///
/// The two-out-parameter form: `sourceLen` is in-out as well, reporting how much of the input was
/// consumed, which is what lets a caller find the end of a stream inside a larger buffer.
fn port_uncompress2(dest: &mut [u8], source: &[u8]) -> Option<Decode> {
    let mut dest_len = libz_rs_sys::uLongf::try_from(dest.len()).ok()?;
    let mut source_len = libz_rs_sys::uLong::try_from(source.len()).ok()?;

    // SAFETY: as for `port_uncompress`, with `sourceLen` additionally pointing at a live local that
    // outlives the call and is written with the number of input bytes consumed.
    let status = unsafe {
        libz_rs_sys::uncompress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            core::ptr::addr_of_mut!(source_len),
        )
    };

    Some(Decode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// `uncompress` through the reference, reached as `c_uncompress`.
fn oracle_uncompress(dest: &mut [u8], source: &[u8]) -> Option<Decode> {
    let mut dest_len = oracle::uLongf::try_from(dest.len()).ok()?;
    let source_len = oracle::uLong::try_from(source.len()).ok()?;

    // SAFETY: as for `port_uncompress`, against the reference's own entry point and its own scalar
    // aliases. Both are `core::ffi::c_ulong` on every supported target, so no width is assumed.
    let status = unsafe {
        oracle::c_uncompress(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source_len,
        )
    };

    Some(Decode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// `uncompress2` through the reference, reached as `c_uncompress2`.
fn oracle_uncompress2(dest: &mut [u8], source: &[u8]) -> Option<Decode> {
    let mut dest_len = oracle::uLongf::try_from(dest.len()).ok()?;
    let mut source_len = oracle::uLong::try_from(source.len()).ok()?;

    // SAFETY: as for `port_uncompress2`, against the reference's own entry point.
    let status = unsafe {
        oracle::c_uncompress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            core::ptr::addr_of_mut!(source_len),
        )
    };

    Some(Decode {
        status,
        produced: usize::try_from(dest_len).ok()?,
    })
}

/// Which one-shot entry point one `uncompress_oneshot` row exercises.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OneShot {
    /// `uncompress` -- `uncompr.c` L97, one out-parameter.
    Uncompress,
    /// `uncompress2` -- `uncompr.c` L83, two out-parameters.
    Uncompress2,
}

impl OneShot {
    /// The short token this entry point contributes to a benchmark id.
    const fn tag(self) -> &'static str {
        match self {
            Self::Uncompress => "uncompress",
            Self::Uncompress2 => "uncompress2",
        }
    }

    /// Call this entry point through the port.
    fn port(self, dest: &mut [u8], source: &[u8]) -> Option<Decode> {
        match self {
            Self::Uncompress => port_uncompress(dest, source),
            Self::Uncompress2 => port_uncompress2(dest, source),
        }
    }

    /// Call this entry point through the reference.
    fn oracle(self, dest: &mut [u8], source: &[u8]) -> Option<Decode> {
        match self {
            Self::Uncompress => oracle_uncompress(dest, source),
            Self::Uncompress2 => oracle_uncompress2(dest, source),
        }
    }
}

/// Both one-shot entry points, in `uncompr.c`'s own forwarding order.
const ONE_SHOTS: [OneShot; 2] = [OneShot::Uncompress, OneShot::Uncompress2];

// =================================================================================================
//  One prepared case
// =================================================================================================

/// A case that is ready to measure: the bytes to reproduce, the reference-encoded stream to decode,
/// and the parameters that produced it.
///
/// Everything expensive happens in [`prepare`] and [`Prepared::verify_stream`], both of which run
/// before any timed closure exists. A timed closure only ever reads [`Prepared::compressed`] and
/// writes an output buffer allocated for it in advance.
#[derive(Debug)]
struct Prepared<'a> {
    /// The benchmark id parameter, and the case identity in every diagnostic and ratio line. It
    /// always begins with the fixture or member name, and it never contains `/`, because criterion
    /// turns an id into a path under `target/criterion`.
    id: String,
    /// The uncompressed bytes both implementations must reproduce exactly.
    original: &'a [u8],
    /// The reference-encoded stream both implementations decode.
    compressed: Vec<u8>,
    /// The container the stream is wrapped in, and hence the `windowBits` to decode it with.
    container: Container,
    /// How the decoder is fed.
    feeding: Feeding,
    /// The level the stream was encoded at. Carried for diagnostics.
    level: c_int,
}

/// Compress `original` with the reference and return a case ready to measure, or `None`.
///
/// `None` is not an error path to be silenced: [`compress_with_oracle`] has already printed why, and
/// the caller simply moves on to the next case.
fn prepare(
    id: String,
    original: &[u8],
    level: c_int,
    container: Container,
    feeding: Feeding,
) -> Option<Prepared<'_>> {
    let compressed = compress_with_oracle(original, level, container)?;

    Some(Prepared {
        id,
        original,
        compressed,
        container,
        feeding,
        level,
    })
}

impl Prepared<'_> {
    /// The size of the buffer a streaming decode is given: the known uncompressed length plus
    /// [`OUTPUT_SLACK`].
    ///
    /// Sized from the *uncompressed* length, deliberately, and not from `deflateBound` -- that bounds
    /// the compressed size and is used only to size the setup compressor's output. The one extra byte
    /// is what lets the final call read the trailer and return `Z_STREAM_END` instead of stopping at
    /// `Z_OK` with a full buffer.
    fn stream_output_len(&self) -> usize {
        self.original.len().saturating_add(OUTPUT_SLACK)
    }

    /// The size of the buffer a one-shot call is given: exactly the known uncompressed length.
    ///
    /// No slack, because `uncompress` takes the capacity as an in-out `destLen` and reports `Z_OK`
    /// when the data fits exactly -- which is what a real caller who knows the size passes.
    fn one_shot_output_len(&self) -> usize {
        self.original.len()
    }

    /// Everything a diagnostic should name, in one line.
    fn describe(&self) -> String {
        format!(
            "case={id} level={level} windowBits={bits} container={container} feeding={feeding} \
             compressed={compressed}B original={original}B",
            id = self.id,
            level = self.level,
            bits = self.container.window_bits(),
            container = self.container.tag(),
            feeding = self.feeding.tag(),
            compressed = self.compressed.len(),
            original = self.original.len(),
        )
    }

    /// Check one decode outcome against the original bytes, reporting rather than asserting.
    ///
    /// This is the acceptance check of `contrib/testzlib/testzlib.c` L267-L270 -- the produced length
    /// equals the original length *and* the bytes compare equal -- with the status checked as well,
    /// since a driver that stopped early would otherwise pass on a truncated prefix of the right
    /// length only by coincidence.
    ///
    /// Deliberately not an assertion. Byte identity and interoperability are gated under
    /// `cargo test` by `crates/zlib-rs-differential/tests/byte_identical.rs` and
    /// `tests/roundtrip_interop.rs`, where a failure should stop the run and name the configuration.
    /// Here a mismatch means one case is not measured.
    fn accepts(&self, label: &str, expected: c_int, outcome: Option<Decode>, out: &[u8]) -> bool {
        let Some(decode) = outcome else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: a length could not be represented in this \
                 target's uInt/uLong while driving {}",
                self.describe()
            );
            return false;
        };

        if decode.status != expected {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: expected status {expected} and got \
                 {status} for {description}",
                status = decode.status,
                description = self.describe(),
            );
            return false;
        }

        if decode.produced != self.original.len() {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: produced {produced} bytes, expected \
                 {expected_len}, for {description}",
                produced = decode.produced,
                expected_len = self.original.len(),
                description = self.describe(),
            );
            return false;
        }

        let Some(produced) = out.get(..decode.produced) else {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: reported {produced} bytes into a {capacity}-byte \
                 buffer for {description}",
                produced = decode.produced,
                capacity = out.len(),
                description = self.describe(),
            );
            return false;
        };

        if produced != self.original {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{label}]: the {len} bytes produced do not match the \
                 original for {description}. Correctness is gated by \
                 crates/zlib-rs-differential/tests/, not here.",
                len = decode.produced,
                description = self.describe(),
            );
            return false;
        }

        true
    }

    /// Decode this case once with each implementation and confirm both reproduce the original.
    ///
    /// Runs strictly outside every timed region, once per case. A throughput number for a decoder
    /// that produced the wrong bytes is worse than no number, and this is what prevents one being
    /// published.
    fn verify_stream(&self) -> bool {
        let window_bits = self.container.window_bits();
        let mut out = vec![0_u8; self.stream_output_len()];

        let mut port = PortInflate::new();
        let port_init = port.init(window_bits);
        if port_init != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{PORT_LABEL}]: inflateInit2_ returned {port_init} for {}",
                self.describe()
            );
            return false;
        }
        let port_decoded = port_decode(&mut port, &self.compressed, &mut out, self.feeding);
        if !self.accepts(PORT_LABEL, Z_STREAM_END, port_decoded, &out) {
            return false;
        }

        out.fill(0);

        let mut reference = OracleInflate::new();
        let reference_init = reference.init(window_bits);
        if reference_init != Z_OK {
            eprintln!(
                "{LOG_PREFIX} SKIPPING [{ORACLE_LABEL}]: c_inflateInit2_ returned \
                 {reference_init} for {}",
                self.describe()
            );
            return false;
        }
        let reference_decoded =
            oracle_decode(&mut reference, &self.compressed, &mut out, self.feeding);

        self.accepts(ORACLE_LABEL, Z_STREAM_END, reference_decoded, &out)
    }

    /// The one-shot counterpart of [`Prepared::verify_stream`].
    ///
    /// `uncompress` reports `Z_OK` rather than `Z_STREAM_END`: it consumes the whole stream
    /// internally and returns the wrapper's verdict, not the state machine's.
    fn verify_one_shot(&self, entry: OneShot) -> bool {
        let mut out = vec![0_u8; self.one_shot_output_len()];

        let port_decoded = entry.port(&mut out, &self.compressed);
        if !self.accepts(PORT_LABEL, Z_OK, port_decoded, &out) {
            return false;
        }

        out.fill(0);

        let reference_decoded = entry.oracle(&mut out, &self.compressed);
        self.accepts(ORACLE_LABEL, Z_OK, reference_decoded, &out)
    }
}

// =================================================================================================
//  Throughput
// =================================================================================================

/// A byte count for [`Throughput::Bytes`], reporting rather than panicking on the impossible.
///
/// Every fixture and every Silesia member is far below `u64::MAX`, so this cannot fail on any target
/// Rust supports. It is written fallibly because `usize` is not `u64` by definition and because the
/// workspace denies `unwrap`: a benchmark has a correct answer for an unrepresentable length, and it
/// is to say so and move on.
fn throughput_bytes(len: usize) -> Option<Throughput> {
    let Ok(bytes) = u64::try_from(len) else {
        eprintln!(
            "{LOG_PREFIX} skipping a {len}-byte case: the length does not fit a u64 and so has no \
             expressible throughput."
        );
        return None;
    };

    Some(Throughput::Bytes(bytes))
}

// =================================================================================================
//  Criterion budget
// =================================================================================================

/// Samples per row.
///
/// 50 rather than criterion's default 100. A default run registers about eighty-six rows across five
/// groups, and 100 samples at the default five-second measurement would put a full run past twenty
/// minutes -- long enough that it stops being run. Fifty samples still gives criterion enough to
/// resample from for a usable confidence interval, and criterion refuses fewer than ten outright.
const SAMPLE_SIZE: usize = 50;

/// Warm-up per row: 1 second rather than the default 3.
///
/// Decompression reaches steady state quickly -- the window and the decode tables are touched within
/// the first iteration or two -- so the remaining two seconds bought precision that the 10% gate does
/// not need.
const WARM_UP: Duration = Duration::from_secs(1);

/// Measurement per row: 3 seconds rather than the default 5.
///
/// With the warm-up that is about four seconds per row, and a full default run -- eighty-six rows --
/// was measured at eight minutes. Pass `--sample-size`, `--warm-up-time` or `--measurement-time` on
/// the command line to trade
/// wall clock for precision, or filter to one group to spend the whole budget on one axis.
const MEASUREMENT: Duration = Duration::from_secs(3);

/// Apply the budget above to a group.
///
/// Every group is configured through this one function so that the numbers are stated once and no
/// group silently drifts onto criterion's defaults.
fn configure(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.sample_size(SAMPLE_SIZE);
    group.warm_up_time(WARM_UP);
    group.measurement_time(MEASUREMENT);
}

// =================================================================================================
//  The indicative ratio pass, and the ledger that reports it
// =================================================================================================
//
//  criterion measures, and the criterion regression job of AAP §0.4.1.6 decides. But criterion's
//  harness API hands no timing back to the benchmark that produced it, so a plain `cargo bench` run
//  would print two rates per case and no ratio -- and the gate is a ratio. This pass supplies one.
//
//  It is deliberately small: bounded to a couple of milliseconds per round, three rounds, minimum
//  reported. The minimum rather than the mean because the shortest observed time is the least
//  contaminated by scheduling, and because the figure is a comparison rather than a distribution.
//  criterion's `target/criterion/<group>/<label>/<case>/new/estimates.json` remains the
//  authoritative measurement.

/// Nanoseconds in a second, as a float, for turning a [`Duration`] into a per-operation figure.
const NANOS_PER_SEC: f64 = 1_000_000_000.0;

/// Timed rounds per side. The minimum of the three is reported.
const CALIBRATION_ROUNDS: u32 = 3;

/// Wall-clock budget for one round, from which the repetition count is derived.
const CALIBRATION_BUDGET: Duration = Duration::from_millis(2);

/// Ceiling on repetitions per round, so that a nanosecond-scale operation cannot turn the budget
/// calculation into a long loop.
const CALIBRATION_MAX_REPS: u32 = 100_000;

/// The mean nanoseconds per call of `op`, from a bounded self-timed pass, or `None` if `op` failed.
///
/// One probe call establishes the scale, an integer division turns [`CALIBRATION_BUDGET`] into a
/// repetition count, and [`CALIBRATION_ROUNDS`] rounds of that many calls are timed. All arithmetic
/// on the count is integer arithmetic on nanoseconds, so no float is ever cast from an integer and
/// no precision is lost silently.
///
/// `op` returns `false` to mean "this did not work", which aborts the pass and yields `None`; the
/// caller reports the case as unmeasured rather than publishing a ratio for a failed operation.
fn indicative_ns<F: FnMut() -> bool>(mut op: F) -> Option<f64> {
    let probe_started = Instant::now();
    if !op() {
        return None;
    }
    let probe = probe_started.elapsed();

    let probe_ns = probe.as_nanos().max(1);
    let wanted = CALIBRATION_BUDGET.as_nanos() / probe_ns;
    let reps = u32::try_from(wanted.clamp(1, u128::from(CALIBRATION_MAX_REPS)))
        .unwrap_or(CALIBRATION_MAX_REPS);

    let mut best: Option<Duration> = None;
    for _ in 0..CALIBRATION_ROUNDS {
        let started = Instant::now();
        for _ in 0..reps {
            if !op() {
                return None;
            }
        }
        let elapsed = started.elapsed();
        best = match best {
            Some(current) if current <= elapsed => Some(current),
            _ => Some(elapsed),
        };
    }

    Some(best?.as_secs_f64() * NANOS_PER_SEC / f64::from(reps))
}

/// Accumulates the per-case ratios of one group and prints the greppable lines CI consumes.
///
/// The format is fixed and documented in the module header. Nothing here panics, exits or aborts on
/// a regression: `verdict=over` is the whole of this file's enforcement, and the pass/fail decision
/// belongs to the workflow.
#[derive(Debug)]
struct RatioLedger {
    /// The group name every line carries.
    group: &'static str,
    /// How many cases produced a ratio at all.
    cases: u32,
    /// How many of those had a payload of at least [`GATE_MIN_BYTES`] and so count against the gate.
    gated: u32,
    /// How many *gated* cases exceeded [`GATE_RATIO`]. This is the number a workflow reads.
    over: u32,
    /// How many cases were below the floor and are therefore reported but not counted.
    informational: u32,
}

impl RatioLedger {
    /// An empty ledger for `group`.
    const fn new(group: &'static str) -> Self {
        Self {
            group,
            cases: 0,
            gated: 0,
            over: 0,
            informational: 0,
        }
    }

    /// Record one case, printing its `RATIO` line.
    ///
    /// `bytes` is the case's *uncompressed* length, which is both the quantity the throughput is
    /// expressed per and the quantity [`GATE_MIN_BYTES`] is compared against.
    ///
    /// A side that could not be timed, or a reference time of zero that no ratio can be taken
    /// against, is reported as unmeasured on its own line and does not enter any count -- so an
    /// aggregate of zero `over` never quietly means "nothing was compared".
    fn record(&mut self, case: &str, bytes: usize, port_ns: Option<f64>, oracle_ns: Option<f64>) {
        let (Some(port_ns), Some(oracle_ns)) = (port_ns, oracle_ns) else {
            eprintln!(
                "{LOG_PREFIX} no ratio for group={group} case={case}: one side could not be timed.",
                group = self.group,
            );
            return;
        };

        if oracle_ns <= 0.0 {
            eprintln!(
                "{LOG_PREFIX} no ratio for group={group} case={case}: the reference measured \
                 {oracle_ns} ns, which nothing can be divided by.",
                group = self.group,
            );
            return;
        }

        let ratio = port_ns / oracle_ns;
        let verdict = if ratio > GATE_RATIO { "over" } else { "within" };
        let counted = bytes >= GATE_MIN_BYTES;
        let gate = if counted { "counted" } else { "informational" };

        self.cases = self.cases.saturating_add(1);
        if counted {
            self.gated = self.gated.saturating_add(1);
            if ratio > GATE_RATIO {
                self.over = self.over.saturating_add(1);
            }
        } else {
            self.informational = self.informational.saturating_add(1);
        }

        eprintln!(
            "{LOG_PREFIX} {RATIO_KEY} group={group} case={case} bytes={bytes} \
             port_ns={port_ns:.1} oracle_ns={oracle_ns:.1} ratio={ratio:.3} \
             limit={GATE_RATIO:.2} gate={gate} verdict={verdict}",
            group = self.group,
        );
    }

    /// Print the group's aggregate `RATIO-SUMMARY` line.
    ///
    /// `cases` is `gated + informational`, and `over` counts only gated cases -- so a workflow that
    /// reads `over` from the `inflate_steady_state` line is reading exactly the AAP §0.8.4 gate.
    fn finish(&self) {
        eprintln!(
            "{LOG_PREFIX} {RATIO_SUMMARY_KEY} group={group} cases={cases} gated={gated} \
             over={over} informational={informational} limit={GATE_RATIO:.2}",
            group = self.group,
            cases = self.cases,
            gated = self.gated,
            over = self.over,
            informational = self.informational,
        );
    }
}

// =================================================================================================
//  Configuration banner
// =================================================================================================

/// The reference's own version string, or `None` if it cannot be read as UTF-8.
///
/// Reported rather than asserted, and it answers a question the init handshake cannot. That
/// handshake compares only the major digit (`deflate.c` L394, `inflate.c` L178), so it would pass
/// against a reference reporting any `1.x`; this line prints the reference's *whole* string beside the
/// `zlib.h` L44 constant [`version_ptr`] passes, which is how a reader confirms the two agree
/// character for character rather than merely in their first byte.
fn oracle_version() -> Option<&'static str> {
    // SAFETY: `zlibVersion` returns a pointer to `ZLIB_VERSION`, a string literal with static
    // storage duration compiled into the oracle archive, so the pointee outlives the process and is
    // never written through. It is documented never to return null, and the check below does not
    // rely on that.
    let raw = unsafe { oracle::c_zlibVersion() };
    if raw.is_null() {
        return None;
    }

    // SAFETY: `raw` is non-null, as just checked, and addresses that same NUL-terminated `'static`
    // literal, so the string it describes is valid for reads for the whole program and the `'static`
    // lifetime this borrow claims is honest.
    let text = unsafe { CStr::from_ptr(raw) };
    text.to_str().ok()
}

/// Print the compiled configuration, the version handshake and the tier-2 state once, to stderr.
///
/// Every group calls this; [`Once`] makes it happen exactly one time however the run is filtered.
/// stderr rather than stdout because criterion's machine-readable output goes to stdout and must not
/// be polluted -- which is also why the `RATIO` lines go to stderr.
///
/// The point is that no row in the output is ambiguous. `simd` is resolved at compile time and
/// `inflate` verifies an Adler-32 or a CRC-32 over everything it produces, so the checksum backend is
/// on the decoder's hot path and a rate on its own does not say which backend produced it.
fn report_configuration() {
    static BANNER: Once = Once::new();

    BANNER.call_once(|| {
        eprintln!(
            "{LOG_PREFIX} measuring decompression throughput: the Rust port against the in-process \
             C oracle."
        );
        eprintln!(
            "{LOG_PREFIX}   port rows are labelled {PORT_LABEL:?}, reference rows {ORACLE_LABEL:?}"
        );
        eprintln!(
            "{LOG_PREFIX}   the AAP §0.8.4 gate is ratio <= {GATE_RATIO:.2} and is read from the \
             {GROUP_STEADY_STATE} group; {GROUP_LIFECYCLE} is a separate signal that includes \
             inflateInit2_ and inflateEnd."
        );
        eprintln!(
            "{LOG_PREFIX}   per-case lines are `{RATIO_KEY} group=... case=... bytes=... \
             port_ns=... oracle_ns=... ratio=... limit=... gate=counted|informational \
             verdict=within|over`, one `{RATIO_SUMMARY_KEY}` line per group. Reported, never \
             enforced -- the workflow decides."
        );
        eprintln!(
            "{LOG_PREFIX}   only payloads of at least {GATE_MIN_BYTES} bytes are gate=counted: \
             below one page a decode is fixed cost rather than throughput, so those rows carry \
             their ratio but do not enter the `over` tally."
        );

        match oracle_version() {
            Some(version) => eprintln!(
                "{LOG_PREFIX}   version handshake: both *Init*_ calls are passed \
                 {expected:?} (zlib.h L44) and the reference reports {version:?}",
                expected = ZLIB_VERSION.to_str().unwrap_or("<not UTF-8>"),
            ),
            None => eprintln!(
                "{LOG_PREFIX}   version handshake: the reference's zlibVersion() could not be read \
                 as UTF-8; the *Init*_ calls are still passed the zlib.h L44 constant."
            ),
        }

        eprintln!(
            "{LOG_PREFIX}   cargo feature \"simd\": {}",
            if cfg!(feature = "simd") {
                "ENABLED -- vectorization-friendly Adler-32 and CRC-32 backends compiled in, and \
                 inflate verifies a checksum over everything it produces"
            } else {
                "disabled (the default) -- scalar checksum backends only"
            }
        );

        // `ZLIB_RS_SIMD` is a build-time consistency check in crates/libz-rs-sys/build.rs, not a
        // selector: cargo resolves features before it runs a build script, so a build script cannot
        // enable one. Reporting it here makes a disagreement between what was asked for and what was
        // compiled visible in the measurement output rather than only in a build log.
        match option_env!("ZLIB_RS_SIMD") {
            Some(value) => eprintln!(
                "{LOG_PREFIX}   ZLIB_RS_SIMD={value:?} at compile time (a consistency check against \
                 the feature above, never a second selector)"
            ),
            None => eprintln!("{LOG_PREFIX}   ZLIB_RS_SIMD unset at compile time"),
        }

        eprintln!(
            "{LOG_PREFIX}   feeding: single-shot Z_FINISH, and chunked Z_SYNC_FLUSH at \
             {CHUNK_DEFAULT}/{CHUNK_MEDIUM}/{CHUNK_SMALL} bytes (contrib/testzlib/testzlib.c \
             L147-L148, L245-L254)"
        );
        eprintln!(
            "{LOG_PREFIX}   budget per row: {SAMPLE_SIZE} samples, {WARM_UP:?} warm-up, \
             {MEASUREMENT:?} measurement"
        );
    });
}

// =================================================================================================
//  The three measurement shapes
// =================================================================================================

/// Register the steady-state pair for one case: `inflateReset2` plus a decode, per iteration.
///
/// The stream is initialised once here, outside every timed closure, and `inflateReset2` returns it
/// to the start of a stream inside the closure. `inflateReset2` keeps the window allocation when the
/// window size is unchanged, so what is timed is the decode plus the cheap half of the lifecycle and
/// not the allocator -- which is why this is the shape the AAP §0.8.4 gate is read from.
///
/// The reset is inside the timed region on purpose: it is what makes each iteration decode the same
/// stream from its beginning rather than continue a finished one, so it is part of the work, not
/// setup. It costs a few dozen field assignments.
fn measure_steady_state(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
) {
    let Some(throughput) = throughput_bytes(case.original.len()) else {
        return;
    };
    if !case.verify_stream() {
        return;
    }

    let window_bits = case.container.window_bits();
    let mut out = vec![0_u8; case.stream_output_len()];

    // Bound to locals and initialised in place: a guard must not move once it is live, because the
    // reference's state block holds a pointer back to the stream and checks it by identity
    // (`inflate.c` L94, L203).
    let mut port = PortInflate::new();
    let port_init = port.init(window_bits);
    let mut reference = OracleInflate::new();
    let reference_init = reference.init(window_bits);
    if port_init != Z_OK || reference_init != Z_OK {
        eprintln!(
            "{LOG_PREFIX} SKIPPING {}: inflateInit2_ returned {port_init} for the port and \
             {reference_init} for the reference after the verification pass had succeeded.",
            case.describe()
        );
        return;
    }

    let port_ns = indicative_ns(|| {
        port.reset(window_bits) == Z_OK
            && port_decode(&mut port, &case.compressed, &mut out, case.feeding).is_some()
    });
    let oracle_ns = indicative_ns(|| {
        reference.reset(window_bits) == Z_OK
            && oracle_decode(&mut reference, &case.compressed, &mut out, case.feeding).is_some()
    });
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    group.throughput(throughput);
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            let reset = port.reset(window_bits);
            let decoded = port_decode(
                &mut port,
                black_box(case.compressed.as_slice()),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((reset, decoded))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            let reset = reference.reset(window_bits);
            let decoded = oracle_decode(
                &mut reference,
                black_box(case.compressed.as_slice()),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((reset, decoded))
        });
    });
}

/// Register the full-lifecycle pair for one case: `inflateInit2_`, a decode and `inflateEnd`.
///
/// One iteration is a whole stream from nothing to nothing, `inflateEnd` included -- the guard's
/// [`Drop`] runs at the end of each iteration and is therefore inside the timed region, which is the
/// entire point of this shape. Read against [`measure_steady_state`], the difference between the two
/// groups *is* the per-stream setup cost, and on a small payload it is most of the number.
fn measure_lifecycle(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
) {
    let Some(throughput) = throughput_bytes(case.original.len()) else {
        return;
    };
    if !case.verify_stream() {
        return;
    }

    let window_bits = case.container.window_bits();
    let mut out = vec![0_u8; case.stream_output_len()];

    let port_ns = indicative_ns(|| {
        let mut port = PortInflate::new();
        port.init(window_bits) == Z_OK
            && port_decode(&mut port, &case.compressed, &mut out, case.feeding).is_some()
    });
    let oracle_ns = indicative_ns(|| {
        let mut reference = OracleInflate::new();
        reference.init(window_bits) == Z_OK
            && oracle_decode(&mut reference, &case.compressed, &mut out, case.feeding).is_some()
    });
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    group.throughput(throughput);
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            let mut port = PortInflate::new();
            let init = port.init(window_bits);
            let decoded = port_decode(
                &mut port,
                black_box(case.compressed.as_slice()),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((init, decoded))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            let mut reference = OracleInflate::new();
            let init = reference.init(window_bits);
            let decoded = oracle_decode(
                &mut reference,
                black_box(case.compressed.as_slice()),
                black_box(out.as_mut_slice()),
                case.feeding,
            );
            black_box((init, decoded))
        });
    });
}

/// Register the one-shot pair for one case and one entry point.
///
/// No guard, because there is no caller-visible stream: `uncompr.c` allocates, drives and frees one
/// internally per call, so the lifecycle is inside the entry point and cannot be separated from the
/// decode. That is exactly why this group exists beside the other two rather than inside them.
fn measure_one_shot(
    group: &mut BenchmarkGroup<'_, WallTime>,
    ledger: &mut RatioLedger,
    case: &Prepared<'_>,
    entry: OneShot,
) {
    let Some(throughput) = throughput_bytes(case.original.len()) else {
        return;
    };
    if !case.verify_one_shot(entry) {
        return;
    }

    let mut out = vec![0_u8; case.one_shot_output_len()];

    let port_ns = indicative_ns(|| entry.port(&mut out, &case.compressed).is_some());
    let oracle_ns = indicative_ns(|| entry.oracle(&mut out, &case.compressed).is_some());
    ledger.record(&case.id, case.original.len(), port_ns, oracle_ns);

    group.throughput(throughput);
    group.bench_function(BenchmarkId::new(PORT_LABEL, &case.id), |b| {
        b.iter(|| {
            black_box(entry.port(
                black_box(out.as_mut_slice()),
                black_box(case.compressed.as_slice()),
            ))
        });
    });
    group.bench_function(BenchmarkId::new(ORACLE_LABEL, &case.id), |b| {
        b.iter(|| {
            black_box(entry.oracle(
                black_box(out.as_mut_slice()),
                black_box(case.compressed.as_slice()),
            ))
        });
    });
}

// =================================================================================================
//  The groups
// =================================================================================================

/// The group the AAP §0.8.4 gate is read from.
const GROUP_STEADY_STATE: &str = "inflate_steady_state";

/// The group that includes the per-stream setup cost.
const GROUP_LIFECYCLE: &str = "inflate_lifecycle";

/// The group that varies the container format.
const GROUP_CONTAINER: &str = "inflate_container";

/// The group that varies how the decoder is fed.
const GROUP_FEEDING: &str = "inflate_feeding";

/// The group that measures the `uncompr.c` wrappers.
const GROUP_ONE_SHOT: &str = "uncompress_oneshot";

/// The opt-in tier-2 group.
const GROUP_SILESIA: &str = "inflate_silesia";

/// Steady-state decompression throughput, per fixture and per compression level.
///
/// ★ This is the group the ≤10% gate of AAP §0.8.4 is read from. The container is zlib and the
/// feeding is the reference driver's 32768-byte chunking, so the axis is exactly the one the gate
/// names: the three levels AAP §0.8.4 lists, over every committed fixture whose decode profile
/// differs.
fn inflate_steady_state(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_STEADY_STATE);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_STEADY_STATE);

    for fixture in selected(&FIXTURE_NAMES) {
        for level in LEVELS {
            let id = format!("{name}-L{level}", name = fixture.name);
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                level,
                DEFAULT_CONTAINER,
                DEFAULT_FEEDING,
            ) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case);
        }
    }

    group.finish();
    ledger.finish();
}

/// Whole-stream decompression: `inflateInit2_`, decode, `inflateEnd`, per iteration.
///
/// Ordered smallest payload first, because that is the axis that matters here: at 14 bytes almost the
/// whole measurement is the allocator, at 65 KiB almost none of it is. Held at level 6 and zlib so
/// that the only thing varying is the payload size.
fn inflate_lifecycle(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_LIFECYCLE);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_LIFECYCLE);

    for fixture in selected(&LIFECYCLE_FIXTURE_NAMES) {
        let id = format!("{name}-L{DEFAULT_LEVEL}", name = fixture.name);
        let Some(case) = prepare(
            id,
            &fixture.bytes,
            DEFAULT_LEVEL,
            DEFAULT_CONTAINER,
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_lifecycle(&mut group, &mut ledger, &case);
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput across the three container formats.
///
/// zlib, raw DEFLATE and gzip, at level 6 and the default chunking. Each takes a different path
/// through `inflate.c`'s state machine before the first compressed byte and after the last -- a
/// two-byte header and an Adler-32 trailer, nothing at all, or a ten-byte-minimum header and a
/// CRC-32-plus-length trailer -- and the two fixtures are the ones large enough for that fixed cost
/// to be a small, measurable fraction rather than the whole number.
fn inflate_container(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_CONTAINER);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_CONTAINER);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for container in CONTAINERS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = container.tag());
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                DEFAULT_LEVEL,
                container,
                DEFAULT_FEEDING,
            ) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case);
        }
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput across the four feeding modes.
///
/// Single-shot `Z_FINISH`, then `Z_SYNC_FLUSH` chunked at 32768, 1024 and 16 bytes. AAP §0.6.4.2
/// records that chunk boundaries interact with flush handling and pending-buffer state, so the shape
/// of the feed is a real axis and not a formality; and AAP §0.3.2.4's
/// `-Cllvm-args=-enable-dfa-jump-thread` lever was measured on small chunked input, which makes the
/// 16-byte row the one it should move.
fn inflate_feeding(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_FEEDING);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_FEEDING);

    for fixture in selected(&AXIS_FIXTURE_NAMES) {
        for feeding in FEEDINGS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = feeding.tag());
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                DEFAULT_LEVEL,
                DEFAULT_CONTAINER,
                feeding,
            ) else {
                continue;
            };
            measure_steady_state(&mut group, &mut ledger, &case);
        }
    }

    group.finish();
    ledger.finish();
}

/// The one-shot wrappers of `uncompr.c`, per fixture and per entry point.
///
/// zlib only, and that is a property of the entry points rather than a choice: `uncompress2_z`
/// (`uncompr.c` L29) reaches the state machine through `inflateInit` with the default `windowBits`,
/// so there is no argument by which a caller could ask for a raw or a gzip stream. Level 6
/// throughout, since the level axis is already covered where a stream can be configured.
fn uncompress_oneshot(c: &mut Criterion) {
    report_configuration();

    let mut group = c.benchmark_group(GROUP_ONE_SHOT);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_ONE_SHOT);

    for fixture in selected(&FIXTURE_NAMES) {
        for entry in ONE_SHOTS {
            let id = format!("{name}-{tag}", name = fixture.name, tag = entry.tag());
            let Some(case) = prepare(
                id,
                &fixture.bytes,
                DEFAULT_LEVEL,
                Container::Zlib,
                DEFAULT_FEEDING,
            ) else {
                continue;
            };
            measure_one_shot(&mut group, &mut ledger, &case, entry);
        }
    }

    group.finish();
    ledger.finish();
}

/// Steady-state throughput over the opt-in tier-2 corpus, or one note and nothing else.
///
/// This is the corpus AAP §0.8.4 names for the throughput gate, and it is measured in the gate
/// configuration only -- level 6, zlib, 32768-byte chunking -- because the container and feeding axes
/// are already covered on tier 1, where a case costs milliseconds rather than seconds. Skipped
/// entirely, with a single note from [`silesia_members`], when the corpus is not present.
///
/// Each member is read, compressed, measured and dropped inside one loop iteration, so the peak
/// memory is one member plus its compressed copy plus one output buffer rather than the whole
/// two-hundred-megabyte corpus at once.
fn inflate_silesia(c: &mut Criterion) {
    report_configuration();

    let members = silesia_members();
    if members.is_empty() {
        return;
    }

    let mut group = c.benchmark_group(GROUP_SILESIA);
    configure(&mut group);
    let mut ledger = RatioLedger::new(GROUP_SILESIA);

    for path in members {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            eprintln!(
                "{LOG_PREFIX} skipping a Silesia member: {} has no name this platform can render \
                 as UTF-8, and a benchmark id has to be printable.",
                path.display()
            );
            continue;
        };

        let bytes = match fs::read(path) {
            Ok(bytes) if bytes.is_empty() => {
                eprintln!(
                    "{LOG_PREFIX} skipping Silesia member {name}: it is empty, and a zero-byte case \
                     has no rate to report."
                );
                continue;
            }
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "{LOG_PREFIX} skipping Silesia member {name}: cannot read {}: {error}",
                    path.display()
                );
                continue;
            }
        };

        let id = format!("{name}-L{DEFAULT_LEVEL}");
        let Some(case) = prepare(
            id,
            &bytes,
            DEFAULT_LEVEL,
            DEFAULT_CONTAINER,
            DEFAULT_FEEDING,
        ) else {
            continue;
        };
        measure_steady_state(&mut group, &mut ledger, &case);
    }

    group.finish();
    ledger.finish();
}

// =================================================================================================
//  Harness
// =================================================================================================
//
//  `harness = false` in the `[[bench]]` entry means libtest supplies no `main`, so `criterion_main!`
//  does. There is no `#[bench]` anywhere and no libtest is linked.
//
//  Ordered the way a reader wants them: the gate group first, because that is the number AAP §0.8.4
//  is about; then the lifecycle group, which is only interpretable against it; then the two axis
//  groups; then the one-shot wrappers; and the opt-in tier-2 group last, because on most machines it
//  prints one line and returns.

criterion_group!(
    inflate_benches,
    inflate_steady_state,
    inflate_lifecycle,
    inflate_container,
    inflate_feeding,
    uncompress_oneshot,
    inflate_silesia,
);
criterion_main!(inflate_benches);
