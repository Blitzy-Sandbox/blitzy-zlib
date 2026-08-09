//! Byte-for-byte equality of the port's compressed output against the C reference's.
//!
//! This file is the instrument for the governing acceptance criterion. Everything else in the
//! workspace can be right while this is wrong: a decoder cannot tell a different-but-valid
//! `DEFLATE` stream from the reference's, so nothing in a round trip, a fuzz run or an `inflate`
//! test detects an encoder that made its own choices. This suite detects it, and it is the only
//! thing that does.
//!
//! # Why byte-identity rather than conformance
//!
//! RFC 1951 constrains the *format*, not the *choices*. Which of several equally valid matches to
//! emit, when to abandon a lazy match, how to break a Huffman frequency tie, when to switch block
//! type -- all of it is implementation-defined, and callers depend on this library's specific
//! answers. **A fully conformant `DEFLATE` encoder will still fail this test.** So will a better
//! one: an encoder that emitted *smaller* output would fail every comparison here, which is the
//! intended behaviour and not a defect in the test. That is why the eight decision points listed
//! below are verbatim ports of deliberately non-optimal heuristics, and why no encoder-level
//! "improvement" may be adopted.
//!
//! # How the comparison is arranged
//!
//! Both implementations are linked into this one test binary -- the port as an ordinary Rust
//! dependency through the `libz_rs_sys` facade, the reference as the `c_`-prefixed static archive
//! this crate's `build.rs` produces -- so a comparison is a buffer-to-buffer memory comparison in a
//! single process. Nothing is shelled out to, no output is round-tripped through a file, and no
//! two-run diff has to be trusted to have fed both halves the same bytes.
//!
//! Both sides are driven through **shape-identical `extern "C"` surfaces**: `deflateInit2_`,
//! `deflate`, `deflateEnd`, `deflateReset`, `deflateBound`, `deflateBound_z` and `deflatePending`
//! on the port; `c_deflateInit2_`, `c_deflate`, `c_deflateEnd`, `c_deflateReset`, `c_deflateBound`,
//! `c_deflateBound_z` and `c_deflatePending` on the reference. One generic driver -- [`open`],
//! [`cell`] and [`close`] -- is instantiated twice over the [`Side`] trait, so the two runs execute
//! the *same* Rust code, with the same chunk boundaries, the same flush schedule and the same
//! buffer sizes. Any translation-layer skew is therefore excluded by construction, which is the
//! whole point of a differential test.
//!
//! The two `z_stream` types are **distinct Rust types** with identical layout:
//! `crates/zlib-rs-differential/src/oracle.rs` transcribes its own `#[repr(C)]` mirror because the
//! two shipped crates are dev-dependencies and so are invisible to this crate's `lib` target. One
//! value of each type is constructed and neither is ever transmuted into the other.
//!
//! Both sides run with `zalloc`, `zfree` and `opaque` null, so each exercises its own internal
//! default allocator, and the whole `z_stream` is zeroed before use -- exactly the
//! `memset`-then-`deflateInit` sequence `test/example.c` performs.
//!
//! # What is compared, per cell
//!
//! The output bytes are the headline, but they are not the whole comparison. Every cell also
//! compares the status code of **every** call, the input and output totals, the running checksum,
//! the reported data type, the pending-output accounting and the four bound functions. A divergence
//! in `total_out` with matching bytes is a pending-buffer accounting bug; a divergence in `adler`
//! with matching bytes means the checksum is being fed differently; a divergence in a bound with
//! matching bytes is a caller-side buffer overflow waiting to happen. None of those is visible from
//! the output buffer alone.
//!
//! # Coverage, and how to run it
//!
//! Three sweeps, because the full cross product of every dimension multiplied by every chunking is
//! not affordable in one test -- see [Matrix size](#matrix-size) below.
//!
//! ```text
//! # The fast default. Always runs; representative rather than arbitrary.
//! cargo test --locked -p zlib-rs-differential --release --test byte_identical
//!
//! # The exhaustive sweeps. CI MUST run this line as well as the one above.
//! ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=1 cargo test --locked -p zlib-rs-differential --release \
//!     --test byte_identical -- --ignored --test-threads 4
//! ```
//!
//! The env var and `--ignored` are **both** required: `#[ignore]` keeps the sweeps out of a default
//! run, and the variable is what makes them do their work rather than report that they were not
//! asked to. Setting one without the other is reported rather than silently ignored.
//!
//! `--test-threads` is the parallelism knob and it matters: the exhaustive configuration sweep is
//! **sharded one level per `#[test]`** precisely so that cargo's own harness can run the shards
//! concurrently, which needs no dependency and no thread of this file's own. Set it to the runner's
//! core count.
//!
//! # Measured wall clock
//!
//! On the development machine -- 4 logical cores, `--test-threads 4`:
//!
//! ```text
//! default run, --release   (8 tests)             4.8 s     13,804 cells
//! default run, debug       (8 tests)              70 s     13,804 cells
//! exhaustive, --release    (11 tests, --ignored)  1,491 s  2,542,729 cells
//!   of which: exhaustive_chunking_matrix          31.5 s      29,029 cells
//!             each of the ten shards          40-510 s     251,370 cells
//! ```
//!
//! So the exhaustive form is about 25 minutes here, and every one of its 2.5 million cells was
//! byte-identical. **A CI timeout of 45 minutes leaves comfortable headroom** on a 4-core runner,
//! and ample on a larger one; the per-shard figures are what to scale, since the shards are what
//! run in parallel. Level 0 is the outlier at 40 s because `deflate_stored` does no match finding
//! at all.
//!
//! A debug build is roughly fifteen times slower, which is why `--release` is in both commands
//! above -- and why the default sweep is sized so that a bare `cargo test --workspace` in debug
//! pays about a minute for it rather than the eight it would otherwise. What the default gives up
//! is enumerated by [`exhaustive_chunking_matrix`]; see [`DEFAULT_TINY_WINDOW_LIMIT`].
//!
//! # Matrix size
//!
//! The dimensions are AAP 0.6.4.2's: 10 levels x 21 `windowBits` x 9 `memLevel`s x 5 strategies x 7
//! flush values x 10 fixtures x several chunkings. The first four multiply out to 9,450
//! configurations and 66,150 (configuration, flush) pairs, which is 661,500 cells per chunking;
//! multiplying *that* by the whole chunking set is what is unaffordable, and specifically because
//! of the tiny windows. `random.bin` and `window_boundary.bin` are incompressible, so a one-byte
//! window costs one `deflate` call per byte that passes through it -- about 66,600 per side for
//! `window_boundary.bin` in a single cell. The split is therefore:
//!
//! * The ten [`exhaustive_configuration_matrix_level_0`]-style shards take the **full** cross
//!   product of level x `windowBits` x `memLevel` x strategy x flush x fixture, one level per
//!   `#[test]` so that cargo's harness runs them concurrently, at the chunkings whose cost is
//!   bounded by the input size rather than by the compressed size.
//! * [`exhaustive_chunking_matrix`] takes the **full** chunking set, including the one-byte output
//!   window, over every fixture and a configuration set that spans every level, all three container
//!   formats, all five strategies, all seven flush values and the extreme `memLevel`s.
//!
//! So every configuration is compared on every fixture, and every chunking is compared on every
//! fixture. What is not enumerated is the product of the two, and that is a runtime-budget decision
//! recorded here rather than a narrowing of the matrix to obtain a green run.
//!
//! One further constraint applies, and it is the only place any comparison is held back:
//! [`small_windows_only_on_small_fixtures`] pairs the four tiniest chunkings only with fixtures
//! below a limit each sweep states for itself. The default sweep sets it low enough to stay fast
//! ([`DEFAULT_TINY_WINDOW_LIMIT`]), the exhaustive sweeps raise it
//! ([`EXHAUSTIVE_TINY_WINDOW_LIMIT`]) so that nothing the default held back goes unmeasured, and
//! `ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=full` removes it altogether for the truly unabridged run.
//!
//! # Contract
//!
//! * **A mismatch is a defect in the port, never in the test.** The C array of bytes is
//!   authoritative. Do not narrow the matrix, compare prefixes, normalise output, add a tolerance
//!   or `#[ignore]` a failing cell.
//! * **`unsafe` only where invoking an already-declared `extern "C"` function requires it.** It is
//!   confined to the [`Side`] implementations, one gate per entry point, each owning a single
//!   `unsafe` block whose `// SAFETY:` comment names the invariant it relies on. No `#[no_mangle]`
//!   anywhere -- this crate exports no C symbol -- and never `extern "C-unwind"`.
//! * **No network, and nothing outside `corpus/minimal/`.** Fixtures are resolved from
//!   `CARGO_MANIFEST_DIR` and loaded by exact filename, as `corpus/README.md` requires. The Silesia
//!   tier is deliberately not read here: it belongs to `benches/`, and reading it would make
//!   correctness contingent on a download.
//! * **Run `tests/table_equality.rs` first.** Its comparisons are the precondition for anything
//!   here being meaningful: a single mistyped digit in a transcribed table changes the emitted
//!   bytes for essentially every input, and would surface here as thousands of failing cells
//!   pointing at none of them.
//! * **Not under Miri, by design.** Miri interprets Rust MIR and cannot execute the compiled C
//!   oracle, so the Miri gate is scoped to `-p zlib-rs`; nightly `AddressSanitizer` covers this
//!   crate instead, which is where the raw pointers actually are. That is a CI job scope, not
//!   something to work around here.

// The workspace lint table denies the panic-prone quartet, which is right for library code and
// wrong for a test: a test asserts, a failed assertion panics, and slicing a fixture at an offset
// this file just computed from that same fixture's length is clearer than defensively matching on
// it. `clippy.toml` grants `unwrap`/`expect`/`panic` inside `#[test]` context, but there is no
// `allow-indexing-slicing-in-tests` option and most of the work here happens in file-scope helpers
// rather than in `#[test]` functions, so the relaxation is stated once here. Same quartet, same
// reason, as `tests/table_equality.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use core::ffi::{c_int, c_uint};
use core::mem::size_of;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

// The facade -- the artifact whose byte-identity is the acceptance criterion. The spelling is
// `libz_rs_sys`, never `z`: `crates/libz-rs-sys` sets `[lib] name = "z"` so that cargo emits
// `libz.so`/`libz.a`, and this crate's manifest uses cargo's dependency-rename form to give the
// Rust path a useful name. The manifest is the authority on that; see its comment at the
// `libz_rs_sys` key.
use libz_rs_sys::{
    ZLIB_VERSION, Z_BINARY, Z_BLOCK, Z_BUF_ERROR, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED,
    Z_FINISH, Z_FIXED, Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_FLUSH, Z_OK, Z_PARTIAL_FLUSH, Z_RLE,
    Z_STREAM_END, Z_STREAM_ERROR, Z_SYNC_FLUSH, Z_TEXT, Z_TREES,
};

// The C reference. Every name here is declared in exactly one place -- this crate's `src/oracle.rs`
// -- and this file adds no `extern "C"` block of its own.
use zlib_rs_differential::oracle;

// The two variants are imported unqualified so that the fixture table below stays readable as a
// table; everything else in this file spells its types out.
use crate::Length::{AtLeast, Exactly};

// =================================================================================================
//  Failure triage -- the eight decision points that determine the emitted bytes
//
//  Every mismatch this suite reports traces to one of these. Each line and value below was checked
//  against the in-tree sources rather than quoted from memory, and two of them correct the numbers
//  that are commonly quoted: `NIL` is at `deflate.c:85`, not 88, and `TOO_FAR`'s definition is at
//  `deflate.c:89` -- 88 is the `#ifndef` guarding it.
//
//  The list is reproduced in the failure message as well as here, because a maintainer reading a CI
//  log has this file's comments nowhere in sight.
// =================================================================================================

/// The triage list, appended to every mismatch report.
///
/// Ordered so that the cheapest hypothesis to test comes first, and each entry names the symptom
/// that implicates it. A reader who has just been handed one failing cell should be able to pick a
/// starting point from the shape of the failure alone.
const TRIAGE: &str = "\
the eight decision points that determine the emitted bytes (AAP 0.6.2):
  1. configuration_table    deflate.c:112, rows L114-L124.  Level 6 is {8,16,128,128,deflate_slow};
                            level 9 is {32,258,258,4096,deflate_slow}.  The FASTEST 2-row variant at
                            deflate.c:107 is NOT the default.  A wrong row changes which matches are
                            found at all -> suspect FIRST when the mismatch is confined to
                            particular levels.
  2. longest_match          deflate.c:1389, with limit = strstart > MAX_DIST(s)
                            ? strstart - MAX_DIST(s) : NIL at L1396-1397 and its deliberate refusal
                            to match window index 0 (#define NIL 0 is at deflate.c:85).  Chain
                            traversal order decides which of several equal-length matches wins, and
                            therefore the emitted DISTANCE -> suspect when lengths agree but
                            distances differ.
  3. early-rejection quartet
                            deflate.c:1482-1485, inside the #else /* UNALIGNED_OK */ arm that opens
                            at L1480: match[best_len] != scan_end || match[best_len-1] != scan_end1
                            || *match != *scan || *++match != scan[1] -> continue.  THE ORDER IS
                            LOAD-BEARING because of the *++match side effect.  The comment at
                            L1487-1488 records that the best_len-1 test 'is not always a win': it is
                            deliberately non-optimal and must not be 'improved'.
  4. TOO_FAR = 4096         the macro is at deflate.c:89 (L88 is the #ifndef).  Applied at
                            deflate.c:1997-2002 under #if TOO_FAR <= 32767: a match is discarded
                            when match_length <= 5 && (strategy == Z_FILTERED || (match_length ==
                            MIN_MATCH && strstart - match_start > TOO_FAR)), setting match_length to
                            MIN_MATCH-1.  Pure heuristic, no RFC basis -> suspect when short matches
                            at long distances differ.
  5. lazy-emit and hash insertion
                            deflate.c:2013-2014: if (prev_length >= MIN_MATCH && match_length <=
                            prev_length) { max_insert = strstart + lookahead - MIN_MATCH.  This
                            changes symbol order AND which positions enter the hash chain, so it
                            perturbs all FUTURE matches -> suspect when the mismatch begins
                            mid-stream and then diverges wildly.
  6. Huffman depth tie-break
                            trees.c:499-501: tree[n].Freq < tree[m].Freq || (tree[n].Freq ==
                            tree[m].Freq && depth[n] <= depth[m]).  Equal-frequency symbols must
                            tie-break by depth identically or the code lengths differ -> suspect
                            when the first difference lands in a block header rather than the
                            payload.
  7. forced two codes       trees.c:655-660, preceded by the L650-654 comment 'the pkzip format
                            requires that at least one distance code exists' -> suspect on
                            single_byte.bin and empty.bin.
  8. block-type selection   trees.c:1027-1074: opt_lenb = (opt_len + 3 + 7) >> 3; static_lenb =
                            (static_len + 3 + 7) >> 3; then static_lenb <= opt_lenb || strategy ==
                            Z_FIXED picks static; stored_len + 4 <= opt_lenb && buf != NULL picks a
                            stored block; else static_lenb == opt_lenb picks STATIC_TREES, else
                            DYN_TREES.  Integer-truncating comparisons: no floating point, no
                            algebraic reordering -> suspect when only the three block-type bits
                            differ, or on random.bin where stored blocks are chosen.

if the mismatch looks SYSTEMATIC across the whole matrix, check the oracle's build before the port:
these compile-time knobs change the emitted bytes and the port deliberately exposes none of them --
FASTEST (deflate.c:106), LIT_MEM (deflate.h:28, commented out by default), UNALIGNED_OK,
FORCE_STATIC / FORCE_STORED (trees.c:1035, trees.c:1044) and GEN_TREES_H (trees.c:234).

run tests/table_equality.rs first: a mistyped digit in a transcribed table changes the output for
essentially every input and would show up here as thousands of failing cells naming none of them.";

// =================================================================================================
//  The corpus
//
//  `corpus/README.md` is the published contract and it is binding: the fixtures are loaded BY EXACT
//  FILENAME, with no globbing, no directory scan and no fallback, because a name that does not
//  match is a missing file rather than a case to skip. The pinned sizes below are asserted for the
//  same reason -- `hello.bin` is 14 bytes and `dictionary.bin` is 6 because both include a trailing
//  NUL, and both lengths are off-by-one traps that a silent truncation would turn into a comparison
//  of the wrong bytes.
//
//  The Silesia tier is deliberately absent. It is opt-in, it belongs to `benches/`, and reading it
//  from a correctness gate would make `cargo test` contingent on a download.
// =================================================================================================

/// What `corpus/README.md` pins about a fixture's length.
///
/// The distinction is the contract's own: four fixtures have an exact size that is load-bearing --
/// 0, 1, 14 and 6 -- and one has a floor, `window_boundary.bin`'s "> 32768 bytes". For the rest the
/// README pins the content class and says outright that "the exact length is not pinned", so
/// asserting one here would invent a contract the corpus does not offer.
#[derive(Clone, Copy, Debug)]
enum Length {
    /// The contract pins this exact byte count.
    Exactly(usize),
    /// The contract pins this floor only.
    AtLeast(usize),
}

/// One committed fixture: its exact filename and the length `corpus/README.md` pins for it.
#[derive(Clone, Copy, Debug)]
struct Fixture {
    /// The exact filename under `corpus/minimal/`.
    name: &'static str,
    /// What the contract pins about its length.
    len: Length,
    /// What `detect_data_type` (`trees.c:966-991`) must report for this fixture at a level above 0,
    /// where that classification is the *purpose* of the fixture and is robust against which bytes
    /// happen to become literals.
    ///
    /// `None` everywhere else, and deliberately so. The heuristic reads *literal* frequencies, so a
    /// byte swallowed by a length/distance pair is never counted -- which makes the verdict for a
    /// fixture with repeats a function of the level and strategy, not of the bytes alone. Pinning
    /// one for `hello.bin` or `repetitive.bin` would be pinning a match-selection outcome under
    /// another name. The four that are pinned each contain no repeat long enough to matter, or no
    /// bytes at all, so their verdict is a property of the data. Every other fixture's `data_type`
    /// is still compared between the two sides on every cell, which is the actual test; what is not
    /// done is asserting a constant this file would have had to guess.
    data_type: Option<c_int>,
}

/// The whole of tier 1, in the order `corpus/README.md` tabulates it. There are no others.
///
/// The four pinned classifications cover three of the partition `detect_data_type` imposes on all
/// 256 byte values, plus the empty case: `text.txt` holds allow-listed bytes only, `binary.bin`
/// holds every block-listed byte once and so returns early at `trees.c:977`, and `gray_list.bin`
/// holds only 7, 8, 11, 12, 26 and 27, which are tolerated -- so it reaches the third exit, the
/// fall-through at `trees.c:990`. `empty.bin` reaches that same fall-through with no bytes at all.
/// `binary.bin` and `gray_list.bin` are additionally sequences of *distinct* bytes, so no match is
/// possible and every byte is certain to be tallied as a literal.
//
// `#[rustfmt::skip]` keeps this a table. Expanded one field per line it becomes fifty lines in
// which the ten rows can no longer be read against `corpus/README.md`'s own table, which is the
// whole point of matching its order. `.rustfmt.toml` records this as the sanctioned escape hatch --
// "a module needing bespoke layout uses #[rustfmt::skip] in its own source" -- and every row below
// is still inside the 100-column limit, so nothing is being smuggled past the width policy.
#[rustfmt::skip]
const FIXTURES: &[Fixture] = &[
    Fixture { name: "empty.bin",           len: Exactly(0),      data_type: Some(Z_BINARY) },
    Fixture { name: "single_byte.bin",     len: Exactly(1),      data_type: None },
    Fixture { name: "repetitive.bin",      len: AtLeast(1),      data_type: None },
    Fixture { name: "random.bin",          len: AtLeast(1),      data_type: None },
    Fixture { name: "text.txt",            len: AtLeast(1),      data_type: Some(Z_TEXT) },
    Fixture { name: "binary.bin",          len: AtLeast(1),      data_type: Some(Z_BINARY) },
    Fixture { name: "gray_list.bin",       len: AtLeast(1),      data_type: Some(Z_BINARY) },
    Fixture { name: "window_boundary.bin", len: AtLeast(32_769), data_type: None },
    Fixture { name: "hello.bin",           len: Exactly(14),     data_type: None },
    Fixture { name: "dictionary.bin",      len: Exactly(6),      data_type: None },
];

/// A loaded fixture: the contract entry plus its bytes.
#[derive(Debug)]
struct Sample {
    /// The contract entry this was loaded from.
    fixture: Fixture,
    /// The bytes, exactly as committed.
    bytes: Vec<u8>,
}

/// `<this crate>/corpus/minimal`, resolved from the manifest directory at compile time.
///
/// Never an absolute path baked into the source and never derived from the current directory, which
/// cargo does not guarantee for a test binary.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join("minimal")
}

/// Loads every fixture and checks it against the pinned contract.
///
/// A missing file, or one whose length has drifted from the contract, fails here rather than
/// producing a comparison over the wrong bytes. That is deliberate: `corpus/README.md` states that
/// renaming, moving or removing a fixture is a breaking change to be made in the same commit as the
/// change to every consumer, and this is the consumer-side half of that.
fn load_corpus() -> Vec<Sample> {
    let dir = corpus_dir();
    FIXTURES
        .iter()
        .map(|fixture| {
            let path = dir.join(fixture.name);
            let bytes = std::fs::read(&path).unwrap_or_else(|err| {
                panic!(
                    "corpus fixture {name} is missing or unreadable at {path}: {err}\n\
                     it is a committed fixture, not an optional one -- corpus/README.md pins the \
                     exact filename and states that a name that does not match is a missing file, \
                     never a skipped case",
                    name = fixture.name,
                    path = path.display(),
                )
            });

            match fixture.len {
                Exactly(want) => assert!(
                    bytes.len() == want,
                    "corpus fixture {name} must be exactly {want} bytes and is {got}; \
                     corpus/README.md pins this length",
                    name = fixture.name,
                    got = bytes.len(),
                ),
                AtLeast(floor) => assert!(
                    bytes.len() >= floor,
                    "corpus fixture {name} must be at least {floor} bytes and is {got}; \
                     corpus/README.md pins this floor",
                    name = fixture.name,
                    got = bytes.len(),
                ),
            }

            Sample {
                fixture: *fixture,
                bytes,
            }
        })
        .collect()
}

// =================================================================================================
//  The matrix
// =================================================================================================

/// Every compression level `deflateInit2_` accepts, which is all ten.
const LEVELS: &[c_int] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// `windowBits` for the zlib container: `9..=15` (`zlib.h:543-600`, `zconf.h:287`).
///
/// 8 is absent on purpose and it is the interesting omission. `deflate.c:439` silently promotes it
/// to 9 "until 256-byte window bug fixed", so a cell for 8 would be a duplicate of the cell for 9
/// wearing a different label. The promotion is asserted directly, and much more precisely, by
/// [`window_bits_boundary_parity`].
const WINDOW_BITS_ZLIB: &[c_int] = &[9, 10, 11, 12, 13, 14, 15];

/// `windowBits` for raw deflate: `-9..=-15`, the negated forms that suppress the wrapper.
///
/// `-8` is absent because it is *invalid* rather than promoted: `deflate.c:422-426` sets `wrap = 0`
/// and negates, and the rejection test at L435-436 includes `windowBits == 8 && wrap != 1`, so a
/// 256-byte window is refused outright for raw output.
const WINDOW_BITS_RAW: &[c_int] = &[-9, -10, -11, -12, -13, -14, -15];

/// `windowBits` for the gzip container: `25..=31`, which `deflate.c:429-431` maps to `9..=15` with
/// `wrap = 2`.
///
/// 24 is absent for the reason `-8` is: it maps to a 256-byte window with `wrap == 2` and is
/// refused by the same test.
const WINDOW_BITS_GZIP: &[c_int] = &[25, 26, 27, 28, 29, 30, 31];

/// Every `memLevel` `deflateInit2_` accepts: `1..=MAX_MEM_LEVEL`, and `MAX_MEM_LEVEL` is 9
/// (`zconf.h:277`).
const MEM_LEVELS: &[c_int] = &[1, 2, 3, 4, 5, 6, 7, 8, 9];

/// All five strategies, with the values `zlib.h:200-204` assigns: `Z_DEFAULT_STRATEGY` 0,
/// `Z_FILTERED` 1, `Z_HUFFMAN_ONLY` 2, `Z_RLE` 3, `Z_FIXED` 4.
const STRATEGIES: &[c_int] = &[
    Z_DEFAULT_STRATEGY,
    Z_FILTERED,
    Z_HUFFMAN_ONLY,
    Z_RLE,
    Z_FIXED,
];

/// All seven flush values, with the values `zlib.h:172-178` assigns.
///
/// The ordering is not the one intuition suggests and the constants are written out rather than
/// counted for exactly that reason: `Z_FINISH` is **4** and sits *between* `Z_FULL_FLUSH` (3) and
/// `Z_BLOCK` (5), with `Z_TREES` at 6.
///
/// Two of the seven get special, documented treatment in [`run`], because a sweep value only means
/// "what to pass on the intermediate calls" if there can be intermediate calls:
///
/// * `Z_FINISH` means "no further input will be supplied", so a run that swept it would have to
///   either supply no further input -- which is single-shot -- or lie to the library. The driver
///   hands the whole input at once for this value, which is what the constant means.
/// * `Z_TREES` is **rejected by `deflate`**: `deflate.c:985` returns `Z_STREAM_ERROR` for `flush >
///   Z_BLOCK`. It is valid for `inflate` only. Both sides must refuse it identically, which is a
///   status-parity assertion rather than a byte-identity one, and the comparison says so instead of
///   counting a pair of empty outputs as a match.
const FLUSHES: &[c_int] = &[
    Z_NO_FLUSH,
    Z_PARTIAL_FLUSH,
    Z_SYNC_FLUSH,
    Z_FULL_FLUSH,
    Z_FINISH,
    Z_BLOCK,
    Z_TREES,
];

/// The `deflateInit`/`compress` defaults, for the sweeps that hold everything but one dimension
/// fixed: level 6 by way of `Z_DEFAULT_COMPRESSION` (`deflate.c:436`), the full 15-bit window, and
/// `DEF_MEM_LEVEL`, which is 8 because `MAX_MEM_LEVEL` is 9 (`zutil.h`).
const DEFAULT_LEVEL: c_int = 6;
/// The default `windowBits`: `MAX_WBITS`, a 32 KiB window with the zlib wrapper.
const DEFAULT_WINDOW_BITS: c_int = 15;
/// The default `memLevel`: `DEF_MEM_LEVEL`, 8 on every supported target.
const DEFAULT_MEM_LEVEL: c_int = 8;

/// One point in the configuration space -- the four arguments `deflateInit2_` takes beyond the
/// method, which is always `Z_DEFLATED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Config {
    /// `0..=9`.
    level: c_int,
    /// `9..=15`, `-9..=-15` or `25..=31`.
    window_bits: c_int,
    /// `1..=9`.
    mem_level: c_int,
    /// `Z_DEFAULT_STRATEGY` through `Z_FIXED`.
    strategy: c_int,
}

impl Config {
    /// A configuration at the `deflateInit` defaults with one dimension moved.
    const fn defaults() -> Self {
        Self {
            level: DEFAULT_LEVEL,
            window_bits: DEFAULT_WINDOW_BITS,
            mem_level: DEFAULT_MEM_LEVEL,
            strategy: Z_DEFAULT_STRATEGY,
        }
    }

    /// Which container format `window_bits` selects, for failure messages.
    const fn container(self) -> &'static str {
        if self.window_bits < 0 {
            "raw"
        } else if self.window_bits > 15 {
            "gzip"
        } else {
            "zlib"
        }
    }
}

/// How the input and output windows are sized across successive `deflate` calls.
///
/// `None` means "one window as large as it needs to be": the whole input for [`Chunking::input`],
/// and a single buffer sized from the compared `deflateBound` for [`Chunking::output`]. Anything
/// else drives that side of the stream in windows of exactly that many bytes.
///
/// The sizes are deliberately awkward relative to the library's internal buffers -- `lit_bufsize`
/// is a power of two derived from `memLevel`, and the pending buffer is a multiple of it -- so that
/// chunk boundaries land inside blocks, inside pending output and inside the sliding window rather
/// than tidily between them. A one-byte output window is the most aggressive case and the most
/// valuable: it forces `deflate` to suspend and resume in the middle of every block header, every
/// stored-block copy and every flush, which is exactly where a pending-buffer accounting bug hides
/// without changing single-shot output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chunking {
    /// A short label, quoted in failure messages.
    name: &'static str,
    /// Bytes handed to `avail_in` per refill, or `None` for the whole input at once.
    input: Option<usize>,
    /// Bytes offered as `avail_out` per call, or `None` for one bound-sized window.
    output: Option<usize>,
}

/// The single-shot baseline: the whole input, one bound-sized output window.
const SINGLE_SHOT: Chunking = Chunking {
    name: "single-shot",
    input: None,
    output: None,
};

/// The chunkings whose cost is proportional to the *input* size, so they can afford to ride the
/// full configuration cross product.
///
/// A small `avail_in` costs one call per chunk of input, which for this corpus is bounded by 91,231
/// bytes in total. A small `avail_out` costs one call per byte of *output*, which for the two
/// incompressible fixtures is very nearly as many calls but which, unlike the input side, cannot be
/// bounded by looking at the fixture -- hence the separation.
//
// `#[rustfmt::skip]` for the reason given on `FIXTURES`: these are tables, and a table whose rows
// are expanded four fields deep can no longer be read as one. Every row is inside the 100-column
// limit.
#[rustfmt::skip]
const CHUNKINGS_INPUT_SIDE: &[Chunking] = &[
    SINGLE_SHOT,
    Chunking { name: "in=1", input: Some(1), output: None },
    Chunking { name: "in=7", input: Some(7), output: None },
    Chunking { name: "in=251", input: Some(251), output: None },
];

/// The full chunking set, including the output-side windows.
///
/// 1, 3, 7 and 251 are odd or prime; 1024 and 16,384 are powers of two chosen to *coincide* with
/// internal buffer sizes, because a boundary that lands exactly on one is as interesting as a
/// boundary that never does.
//
/// The chunkings the default sweep uses: one of each shape, so that no *kind* of boundary goes
/// unexercised on a bare `cargo test`.
///
/// Whole-input and single-byte input; a prime input window and one large enough to cover a fixture
/// in two bites; a single-byte output window and a moderate one; and two cells that squeeze both
/// sides at once. The remaining chunkings in [`CHUNKINGS_ALL`] vary the sizes rather than the
/// shapes, which is what [`exhaustive_chunking_matrix`] is for.
//
// `#[rustfmt::skip]` for the reason given on `FIXTURES`.
#[rustfmt::skip]
const CHUNKINGS_DEFAULT: &[Chunking] = &[
    SINGLE_SHOT,
    Chunking { name: "in=1",            input: Some(1),   output: None },
    Chunking { name: "in=251",          input: Some(251), output: None },
    Chunking { name: "in=16384",        input: Some(16384), output: None },
    Chunking { name: "out=1",           input: None,      output: Some(1) },
    Chunking { name: "out=251",         input: None,      output: Some(251) },
    Chunking { name: "in=1,out=1",      input: Some(1),   output: Some(1) },
    Chunking { name: "in=7,out=3",      input: Some(7),   output: Some(3) },
];

// `#[rustfmt::skip]` for the reason given on `FIXTURES`: these are tables, and a table whose rows
// are expanded four fields deep can no longer be read as one. Every row is inside the 100-column
// limit.
#[rustfmt::skip]
const CHUNKINGS_ALL: &[Chunking] = &[
    SINGLE_SHOT,
    Chunking { name: "in=1", input: Some(1), output: None },
    Chunking { name: "in=3", input: Some(3), output: None },
    Chunking { name: "in=7", input: Some(7), output: None },
    Chunking { name: "in=251", input: Some(251), output: None },
    Chunking { name: "in=1024", input: Some(1024), output: None },
    Chunking { name: "in=16384", input: Some(16384), output: None },
    Chunking { name: "out=1", input: None, output: Some(1) },
    Chunking { name: "out=3", input: None, output: Some(3) },
    Chunking { name: "out=251", input: None, output: Some(251) },
    Chunking { name: "out=1024", input: None, output: Some(1024) },
    Chunking { name: "in=1,out=1", input: Some(1), output: Some(1) },
    Chunking { name: "in=7,out=3", input: Some(7), output: Some(3) },
    Chunking { name: "in=251,out=1024", input: Some(251), output: Some(1024) },
    Chunking { name: "in=16384,out=251", input: Some(16384), output: Some(251) },
];

/// One cell of the matrix: a configuration, the flush value to pass on intermediate calls, and how
/// the two windows are driven.
#[derive(Clone, Copy, Debug)]
struct Cell {
    /// The `deflateInit2_` arguments.
    config: Config,
    /// What to pass on every call that still has input to give.
    flush: c_int,
    /// How the input and output windows are sized.
    chunking: Chunking,
}

// =================================================================================================
//  Width conversions between C's integer model and the numbers this file compares in
//
//  Everything crossing the boundary is compared as a `u64` so that one comparison works on both
//  integer models. `uLong` is `unsigned long`: 64 bits on LP64 and 32 on LLP64 Windows, which is
//  precisely why neither side may be typed as a fixed-width integer and why the widening here is
//  target-dependent rather than decorative.
// =================================================================================================

/// Widens C's `uLong` to the `u64` this file compares in.
///
/// Lossless in both integer models, because [`u64::from`] exists for `u32` and for `u64`.
//
// `clippy::useless_conversion` fires on LP64, where `uLong` already *is* `u64` and the conversion
// is the identity. Removing it would break the LLP64 build, where the widening is real, so the
// conversion is target-dependent rather than useless. Same relaxation, same reason, as
// `widen_uLong` in `crates/libz-rs-sys/src/inflate.rs`. Written as a comment rather than the
// attribute's `reason` field because lint reasons were stabilised in Rust 1.81 and this workspace
// declares `rust-version = "1.80"`.
#[allow(clippy::useless_conversion)]
fn widen_ulong(value: libz_rs_sys::uLong) -> u64 {
    u64::from(value)
}

/// Widens the oracle's `uLong`, which is the same `c_ulong` reached through the other crate's
/// alias.
//
// Relaxed for the reason given on [`widen_ulong`].
#[allow(clippy::useless_conversion)]
fn widen_ulong_oracle(value: oracle::uLong) -> u64 {
    u64::from(value)
}

/// Widens a `z_size_t`, which is `size_t`, to the comparison width.
///
/// Checked rather than cast: a `usize` that did not fit a `u64` would mean a 128-bit address space,
/// which should fail loudly rather than truncate a length silently.
fn widen_usize(value: usize) -> u64 {
    u64::try_from(value).expect("a byte count fits in a u64")
}

/// `(int) sizeof(z_stream)` -- the second half of the handshake the `deflateInit2` macro arranges
/// (`zlib.h:543`).
///
/// Checked rather than cast, for the reason `crates/libz-rs-sys/tests/c_api_parity.rs` gives: a
/// `size_of` that did not fit a `c_int` would mean the ABI mirror had grown past anything `zlib.h`
/// could describe.
fn stream_size<T>() -> c_int {
    c_int::try_from(size_of::<T>()).expect("sizeof(z_stream) fits in a C int")
}

// =================================================================================================
//  What a run reports
// =================================================================================================

/// One `deflate` call, recorded so that the *sequence* of statuses can be compared and not merely
/// the last one.
///
/// `zlib.h` documents `deflate`'s return value as part of the contract, and the sequence carries
/// information the final status does not: a `Z_BUF_ERROR` in the middle of an otherwise successful
/// run means the two implementations disagree about when progress is possible, even if both
/// eventually produce the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CallRecord {
    /// What was passed as `flush`.
    flush: c_int,
    /// What `deflate` returned.
    status: c_int,
    /// How many bytes it wrote into the output window.
    produced: usize,
    /// `avail_in` as the call left it.
    avail_in_after: usize,
}

/// `deflateBound` and `deflateBound_z` for one `sourceLen`, as one comparable pair.
///
/// Both are compared because they are *not* the same function: `deflateBound_z` computes in
/// `z_size_t` and `deflateBound` narrows the result, answering `(uLong)-1` when it does not fit
/// (`deflate.c:926-928`). On LP64 the two agree for every length a test can supply; on LLP64 they
/// diverge above 4 GiB, and comparing both is what pins the narrowing rather than assuming it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    /// `deflateBound` / `compressBound` -- the `uLong` form.
    narrow: u64,
    /// `deflateBound_z` / `compressBound_z` -- the `z_size_t` form.
    wide: u64,
}

/// What `deflatePending` reports: the bytes and bits generated but not yet handed to the caller.
///
/// `zlib.h:790-795` documents both, with the bit count between 0 and 7. Comparing it is close to
/// free and catches an accounting divergence that matching output bytes cannot: two implementations
/// can have emitted the same bytes so far and disagree about how much is still held back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    /// `deflatePending`'s return value.
    status: c_int,
    /// The `*pending` out-parameter.
    bytes: c_uint,
    /// The `*bits` out-parameter.
    bits: c_int,
}

/// The stream members that describe what a run accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Snapshot {
    /// `strm.total_in`.
    total_in: u64,
    /// `strm.total_out`.
    total_out: u64,
    /// `strm.adler` -- Adler-32 under the zlib wrapper, CRC-32 under gzip, and left alone for raw
    /// deflate, so its *value* is format-dependent and its *agreement* is not.
    adler: u64,
    /// `strm.data_type` -- what `detect_data_type` (`trees.c:966-991`) concluded.
    data_type: c_int,
}

/// Everything one side reports for one cell.
///
/// `deflateInit2_`'s and `deflateEnd`'s statuses are deliberately not here: one initialised stream
/// serves every cell of a configuration, so those two are compared once per configuration rather
/// than once per cell. See [`open`] and [`close`].
#[derive(Clone, Debug)]
struct Outcome {
    /// `deflateBound` on the live, configured stream *before* any data has been deflated, so
    /// `strstart` is 0 and the zlib wrapper term is `6 + 0` (`deflate.c:884`).
    bound_before: Bounds,
    /// Every `deflate` call, in order.
    calls: Vec<CallRecord>,
    /// The concatenated output bytes -- the headline comparison.
    output: Vec<u8>,
    /// The accounting members after the run.
    snapshot: Snapshot,
    /// `deflatePending` after the run.
    pending: Pending,
    /// `deflateBound` after the run, so `strstart` is non-zero for any non-empty input and the zlib
    /// wrapper term becomes `6 + 4`. Comparing both sides of that branch is why the bound is taken
    /// twice rather than once.
    bound_after: Bounds,
    /// What `deflateReset` returned.
    reset: c_int,
    /// `deflateBound` after the reset, which must return to its pre-run value if the reset really
    /// reset `strstart`.
    bound_after_reset: Bounds,
    /// How large the output window actually was, in bytes.
    ///
    /// Recorded so that the caller can drive the second side with the identical number in the
    /// bound-sized case, which is what keeps the two runs comparable when the two bounds are the
    /// very thing under test.
    out_window: usize,
}

// =================================================================================================
//  The two sides
//
//  ONE GATE PER ENTRY POINT, each owning a single documented `unsafe` block. That shape is the
//  workspace's stated policy for a repeated call -- see the call-gate section of
//  `crates/libz-rs-sys/tests/c_api_parity.rs` -- and it is what keeps the invariant stated once
//  instead of some hundreds of times. The obligation every gate discharges is identical on both
//  sides, so it is stated here rather than repeated:
//
//    (a) `strm` arrives as `&mut Self::Stream`, so it is non-null, aligned and uniquely borrowed
//        for the duration of the call. A reference cannot be otherwise.
//    (b) `strm.state` is either null or a block this same implementation allocated through its own
//        `deflateInit2_`, so the state check on entry -- `deflateStateCheck` at `deflate.c:538` and
//        its counterpart in the facade -- accepts or rejects it exactly as it would for a C caller.
//        No stream is ever handed to the other side's functions.
//    (c) Where `next_in` is non-null, `avail_in` bytes really are readable there: the pointer is
//        always taken from the `&[u8]` the driver was handed, which outlives every call in the run,
//        and a zero-length window is installed as a NULL pointer instead -- the pairing `zlib.h:91`
//        permits explicitly and the one `deflate.c:990` tests for.
//    (d) `avail_out` bytes really are writable at `next_out`: the pointer is always taken from the
//        scratch buffer `cell` owns, which likewise outlives every call, and the driver never
//        offers a zero-length output window, because `deflate.c:995` answers `Z_BUF_ERROR` for one.
//    (e) The `version`/`stream_size` pair is this side's own: the facade is initialised with the
//        facade's `ZLIB_VERSION` and `size_of` of the facade's `z_stream`, the oracle with what
//        `c_zlibVersion` returns and `size_of` of the oracle's mirror. Crossing them would answer
//        `Z_VERSION_ERROR` (`zlib.h:248`) rather than produce a working stream.
//
//  The two `z_stream` types are distinct Rust types with identical layout, and nothing here
//  transmutes one into the other: each `impl` names its own and touches nothing else.
// =================================================================================================

/// The `deflate` surface this suite drives, implemented once per implementation.
///
/// Every method is a thin gate over one `extern "C"` entry point, so that [`run`] -- the code that
/// actually decides chunk boundaries and flush values -- is written once and executed identically
/// for both sides. That is what makes a difference in the output attributable to the
/// implementations rather than to the harness.
trait Side {
    /// How this side is named in failure messages.
    const NAME: &'static str;

    /// This side's `z_stream` type. Layout-identical to the other's, and a different Rust type.
    type Stream;

    /// A `z_stream` with every field zeroed and the allocator hooks left null, which is the
    /// `memset`-then-`deflateInit` sequence `test/example.c` performs and which selects each
    /// implementation's own internal default allocator.
    fn stream() -> Self::Stream;

    /// `deflateInit2_(strm, level, Z_DEFLATED, windowBits, memLevel, strategy, version, size)`.
    fn init(strm: &mut Self::Stream, config: Config) -> c_int;

    /// `deflate(strm, flush)`, with the two windows installed first.
    ///
    /// `window` is `Some` to install a new input window and `None` to leave `next_in`/`avail_in`
    /// exactly as the previous call left them, which is how a real caller drives a partially
    /// consumed chunk. `out` is installed every time and is never empty.
    fn pump(
        strm: &mut Self::Stream,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> CallRecord;

    /// The accounting members, read straight off the struct.
    fn snapshot(strm: &Self::Stream) -> Snapshot;

    /// `deflatePending(strm, &pending, &bits)`.
    fn pending(strm: &mut Self::Stream) -> Pending;

    /// `deflateBound(strm, sourceLen)` and `deflateBound_z(strm, sourceLen)` on a live stream.
    fn bound(strm: &mut Self::Stream, source_len: usize) -> Bounds;

    /// The same pair with a NULL `z_streamp`, which is the `deflateStateCheck`-fails path at
    /// `deflate.c:876-879` and needs no initialised stream on either side.
    fn bound_of_null_stream(source_len: usize) -> Bounds;

    /// `compressBound(sourceLen)` and `compressBound_z(sourceLen)`, which take no stream at all.
    fn compress_bounds(source_len: usize) -> Bounds;

    /// `deflateReset(strm)`.
    fn reset(strm: &mut Self::Stream) -> c_int;

    /// `deflateEnd(strm)`.
    fn end(strm: &mut Self::Stream) -> c_int;

    /// `compress2(dest, &destLen, source, sourceLen, level)`, returning the status and `*destLen`.
    ///
    /// The one-shot wrapper is included because it is what most callers actually use, and because
    /// it pins a configuration no `deflateInit2_` cell reaches by the same route: `compress2`
    /// supplies `Z_DEFLATED`, `MAX_WBITS`, `DEF_MEM_LEVEL` and `Z_DEFAULT_STRATEGY` itself.
    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> (c_int, usize);
}

/// The port, reached through the `libz-rs-sys` facade -- the artifact whose bytes are the
/// acceptance criterion.
#[derive(Debug)]
struct Port;

/// The C reference, reached through this crate's `c_`-prefixed oracle archive -- the authority.
#[derive(Debug)]
struct Reference;

impl Side for Port {
    const NAME: &'static str = "port (libz-rs-sys)";
    type Stream = libz_rs_sys::z_stream;

    fn stream() -> Self::Stream {
        // Built field by field rather than through `MaybeUninit::zeroed`, which is both stricter
        // and cheaper: it needs no `unsafe`, and it makes the three members a C caller actually
        // sets visible as the deliberate choices they are. `None` is C's `Z_NULL` for the two
        // hooks, which selects the library's own allocator. `z_stream` deliberately does not derive
        // `Default`, because a zeroed stream is not yet a valid one.
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

    fn init(strm: &mut Self::Stream, config: Config) -> c_int {
        // SAFETY: obligations (a), (b) and (e). `deflateInit2_` reads at most one byte of
        // `version`, having tested it for null first, and `ZLIB_VERSION` is a `&CStr` constant with
        // static storage duration; it writes `state` and touches neither window, which is why a C
        // caller may leave `next_in`/`next_out` unset until after the call.
        unsafe {
            libz_rs_sys::deflateInit2_(
                strm,
                config.level,
                Z_DEFLATED,
                config.window_bits,
                config.mem_level,
                config.strategy,
                ZLIB_VERSION.as_ptr(),
                stream_size::<Self::Stream>(),
            )
        }
    }

    fn pump(
        strm: &mut Self::Stream,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> CallRecord {
        install_windows_port(strm, window, out);
        let offered = strm.avail_out;
        // SAFETY: obligations (a) through (d) -- this is the one gate that relies on all four,
        // because it is the one that reads through `next_in` and writes through `next_out`. Both
        // pointers were installed immediately above from slices that outlive this call, and
        // `install_windows_port` is what establishes the null-with-zero-length pairing and the
        // non-empty output window.
        let status = unsafe { libz_rs_sys::deflate(strm, flush) };
        CallRecord {
            flush,
            status,
            produced: usize::try_from(offered - strm.avail_out)
                .expect("bytes produced fit in a usize"),
            avail_in_after: usize::try_from(strm.avail_in).expect("avail_in fits in a usize"),
        }
    }

    fn snapshot(strm: &Self::Stream) -> Snapshot {
        Snapshot {
            total_in: widen_ulong(strm.total_in),
            total_out: widen_ulong(strm.total_out),
            adler: widen_ulong(strm.adler),
            data_type: strm.data_type,
        }
    }

    fn pending(strm: &mut Self::Stream) -> Pending {
        let mut bytes: c_uint = SENTINEL_PENDING;
        let mut bits: c_int = SENTINEL_BITS;
        // `addr_of_mut!` rather than `&mut`, for the same reason `src/oracle.rs` uses it: these are
        // out-parameters C writes through, so the raw pointer is taken straight from the place
        // instead of materialising a Rust mutable reference whose aliasing rules C is under no
        // obligation to respect. It is also what keeps `clippy::borrow_as_ptr` quiet without
        // reaching for `&raw mut`, which is Rust 1.82 syntax and not the idiom this tree uses.
        //
        // SAFETY: obligations (a) and (b), plus the two out-parameters formed here: both address
        // live locals of exactly the declared types, initialised before the call and outliving it,
        // and `zlib.h:795` says the library either writes them or leaves them alone. Neither
        // pointer is retained past the call.
        let status = unsafe {
            libz_rs_sys::deflatePending(
                strm,
                core::ptr::addr_of_mut!(bytes),
                core::ptr::addr_of_mut!(bits),
            )
        };
        Pending {
            status,
            bytes,
            bits,
        }
    }

    fn bound(strm: &mut Self::Stream, source_len: usize) -> Bounds {
        // SAFETY: obligations (a) and (b). Both entry points read `wrap`, `strstart`, `w_bits` and
        // `hash_bits` out of the state block and write nothing at all; neither touches a window, so
        // no pointer/length pair is involved.
        unsafe {
            Bounds {
                narrow: widen_ulong(libz_rs_sys::deflateBound(strm, narrow_len(source_len))),
                wide: widen_usize(libz_rs_sys::deflateBound_z(strm, source_len)),
            }
        }
    }

    fn bound_of_null_stream(source_len: usize) -> Bounds {
        // SAFETY: a NULL `z_streamp` is a documented argument here, not a violation: both entry
        // points funnel through the `deflateStateCheck` test at `deflate.c:876`, which is a null
        // test before it is anything else, and answer `max(fixedlen, storelen) + 18` without a
        // dereference. Passing null is the only way to reach that branch, which is why it is
        // exercised rather than avoided.
        unsafe {
            Bounds {
                narrow: widen_ulong(libz_rs_sys::deflateBound(
                    core::ptr::null_mut(),
                    narrow_len(source_len),
                )),
                wide: widen_usize(libz_rs_sys::deflateBound_z(
                    core::ptr::null_mut(),
                    source_len,
                )),
            }
        }
    }

    fn compress_bounds(source_len: usize) -> Bounds {
        // No gate and no `unsafe`: both are pure arithmetic on their argument and take no pointer,
        // so the facade declares them `extern "C"` without `unsafe`.
        Bounds {
            narrow: widen_ulong(libz_rs_sys::compressBound(narrow_len(source_len))),
            wide: widen_usize(libz_rs_sys::compressBound_z(source_len)),
        }
    }

    fn reset(strm: &mut Self::Stream) -> c_int {
        // SAFETY: obligations (a) and (b). The state block is reinitialised in place and the
        // allocations it holds are kept, which is exactly why a reset is cheaper than an
        // end-and-init pair.
        unsafe { libz_rs_sys::deflateReset(strm) }
    }

    fn end(strm: &mut Self::Stream) -> c_int {
        // SAFETY: obligations (a) and (b). The state block is released through the same allocator
        // it was taken from -- the pairing this call exists to perform -- and `state` is left null,
        // so a second call would be rejected rather than freeing twice.
        unsafe { libz_rs_sys::deflateEnd(strm) }
    }

    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> (c_int, usize) {
        let mut dest_len = narrow_len(dest.len());
        // `addr_of_mut!` on `dest_len`, and the choice is not cosmetic: `compress.c` L14-L16 makes
        // `destLen` an in/out parameter, so C both reads and writes through the pointer, and taking
        // a `&mut` first would materialise a Rust mutable reference whose aliasing rules C is under
        // no obligation to respect.
        //
        // SAFETY: `dest` is a live slice of `dest_len` writable bytes and `source` a live slice of
        // `sourceLen` readable ones, both outliving the call; `dest_len` addresses a live local
        // initialised to the destination's true capacity, which is the in/out contract
        // `zlib.h:1291` documents. The two slices are distinct borrows and therefore cannot
        // overlap. No pointer is retained past the call.
        let status = unsafe {
            libz_rs_sys::compress2(
                dest.as_mut_ptr(),
                core::ptr::addr_of_mut!(dest_len),
                source.as_ptr(),
                narrow_len(source.len()),
                level,
            )
        };
        (
            status,
            usize::try_from(dest_len).expect("a destination length fits in a usize"),
        )
    }
}

impl Side for Reference {
    const NAME: &'static str = "reference (in-tree C)";
    type Stream = oracle::z_stream;

    fn stream() -> Self::Stream {
        // The oracle's mirror does derive `Default`, documented there as "the `memset(&strm, 0,
        // sizeof strm)` a C caller performs before `deflateInit_`", so it is used rather than
        // duplicated. The result is the same all-zero stream the port gets.
        oracle::z_stream::default()
    }

    fn init(strm: &mut Self::Stream, config: Config) -> c_int {
        // The version string is the oracle's own, as `src/oracle.rs` directs: call the `_`-suffixed
        // form and pass what the `deflateInit2` macro passes. Taking it from `c_zlibVersion` rather
        // than from the facade's constant is what keeps obligation (e) honest -- the C library
        // compares the caller's version against its own.
        //
        // SAFETY: `c_zlibVersion` is niladic and returns a pointer to a string literal with static
        // storage duration compiled into the oracle archive, so there is nothing for a caller to
        // get wrong and the pointer outlives the `c_deflateInit2_` call below.
        let version = unsafe { oracle::c_zlibVersion() };
        // SAFETY: obligations (a), (b) and (e), exactly as for the port's gate. `version` is the
        // oracle's own NUL-terminated literal, obtained immediately above and valid for reads
        // through its terminator for the whole program.
        unsafe {
            oracle::c_deflateInit2_(
                strm,
                config.level,
                Z_DEFLATED,
                config.window_bits,
                config.mem_level,
                config.strategy,
                version,
                stream_size::<Self::Stream>(),
            )
        }
    }

    fn pump(
        strm: &mut Self::Stream,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> CallRecord {
        install_windows_reference(strm, window, out);
        let offered = strm.avail_out;
        // SAFETY: obligations (a) through (d), as for the port's gate. The installer immediately
        // above is what establishes (c) and (d) for this call.
        let status = unsafe { oracle::c_deflate(strm, flush) };
        CallRecord {
            flush,
            status,
            produced: usize::try_from(offered - strm.avail_out)
                .expect("bytes produced fit in a usize"),
            avail_in_after: usize::try_from(strm.avail_in).expect("avail_in fits in a usize"),
        }
    }

    fn snapshot(strm: &Self::Stream) -> Snapshot {
        Snapshot {
            total_in: widen_ulong_oracle(strm.total_in),
            total_out: widen_ulong_oracle(strm.total_out),
            adler: widen_ulong_oracle(strm.adler),
            data_type: strm.data_type,
        }
    }

    fn pending(strm: &mut Self::Stream) -> Pending {
        let mut bytes: c_uint = SENTINEL_PENDING;
        let mut bits: c_int = SENTINEL_BITS;
        // `addr_of_mut!` for the out-parameters, exactly as for the port's gate above.
        //
        // SAFETY: obligations (a) and (b), plus the two out-parameters, exactly as for the port's
        // gate: live locals of the declared types, initialised before the call, outliving it, and
        // not retained past it.
        let status = unsafe {
            oracle::c_deflatePending(
                strm,
                core::ptr::addr_of_mut!(bytes),
                core::ptr::addr_of_mut!(bits),
            )
        };
        Pending {
            status,
            bytes,
            bits,
        }
    }

    fn bound(strm: &mut Self::Stream, source_len: usize) -> Bounds {
        // SAFETY: obligations (a) and (b). `deflateBound_z` at `deflate.c:857` reads the state and
        // writes nothing; `deflateBound` at `deflate.c:926` forwards to it and narrows the result.
        // Neither touches a window.
        unsafe {
            Bounds {
                narrow: widen_ulong_oracle(oracle::c_deflateBound(
                    strm,
                    narrow_len_oracle(source_len),
                )),
                wide: widen_usize(oracle::c_deflateBound_z(strm, source_len)),
            }
        }
    }

    fn bound_of_null_stream(source_len: usize) -> Bounds {
        // SAFETY: a NULL `z_streamp` is the documented way to reach `deflate.c:876-879`, whose
        // first act is `deflateStateCheck`, whose first act is a null test. No dereference occurs.
        unsafe {
            Bounds {
                narrow: widen_ulong_oracle(oracle::c_deflateBound(
                    core::ptr::null_mut(),
                    narrow_len_oracle(source_len),
                )),
                wide: widen_usize(oracle::c_deflateBound_z(core::ptr::null_mut(), source_len)),
            }
        }
    }

    fn compress_bounds(source_len: usize) -> Bounds {
        // SAFETY: both are pure arithmetic functions of one integer argument. The oracle declares
        // them `extern "C"`, which makes them `unsafe` to call regardless, so the block is what the
        // language requires rather than an obligation to discharge.
        unsafe {
            Bounds {
                narrow: widen_ulong_oracle(oracle::c_compressBound(narrow_len_oracle(source_len))),
                wide: widen_usize(oracle::c_compressBound_z(source_len)),
            }
        }
    }

    fn reset(strm: &mut Self::Stream) -> c_int {
        // SAFETY: obligations (a) and (b), as for the port's gate.
        unsafe { oracle::c_deflateReset(strm) }
    }

    fn end(strm: &mut Self::Stream) -> c_int {
        // SAFETY: obligations (a) and (b), as for the port's gate.
        unsafe { oracle::c_deflateEnd(strm) }
    }

    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> (c_int, usize) {
        let mut dest_len = narrow_len_oracle(dest.len());
        // `addr_of_mut!` on the in/out `destLen`, exactly as for the port's gate above.
        //
        // SAFETY: as for the port's gate -- two live, distinct, non-overlapping slices that outlive
        // the call, and a live local carrying the destination's true capacity in and its used
        // length out.
        let status = unsafe {
            oracle::c_compress2(
                dest.as_mut_ptr(),
                core::ptr::addr_of_mut!(dest_len),
                source.as_ptr(),
                narrow_len_oracle(source.len()),
                level,
            )
        };
        (
            status,
            usize::try_from(dest_len).expect("a destination length fits in a usize"),
        )
    }
}

/// The value `pending`'s out-parameter is seeded with before every `deflatePending` call.
///
/// A recognisable non-zero seed rather than 0, so that "the library wrote 0" and "the library wrote
/// nothing" are distinguishable: `zlib.h:795` permits it to leave the out-parameters alone when
/// they are `Z_NULL`, and a divergence in whether a side writes at all is worth seeing.
const SENTINEL_PENDING: c_uint = 0xdead_beef;

/// The seed for `bits`, chosen for the same reason and outside the documented `0..=7` range.
const SENTINEL_BITS: c_int = -12_345;

/// Narrows a length to the facade's `uLong`, which is what `deflateBound` and `compressBound` take.
fn narrow_len(len: usize) -> libz_rs_sys::uLong {
    libz_rs_sys::uLong::try_from(len).expect("a fixture length fits in a uLong")
}

/// Narrows a length to the oracle's `uLong`. Same alias, reached through the other crate.
fn narrow_len_oracle(len: usize) -> oracle::uLong {
    oracle::uLong::try_from(len).expect("a fixture length fits in a uLong")
}

/// Installs the port's two windows for one `deflate` call, establishing obligations (c) and (d).
///
/// A zero-length input window becomes a NULL `next_in`, which is the pairing `zlib.h:91-92` permits
/// and the one `deflate.c:990` tests for; anything else would hand the library a pointer it has no
/// business holding. The output window is asserted non-empty because `deflate.c:995` answers
/// `Z_BUF_ERROR` for `avail_out == 0`, and a run that tripped that would be measuring the harness.
fn install_windows_port(strm: &mut libz_rs_sys::z_stream, window: Option<&[u8]>, out: &mut [u8]) {
    assert!(
        !out.is_empty(),
        "deflate must never be offered a zero-length output window"
    );
    if let Some(window) = window {
        strm.next_in = if window.is_empty() {
            core::ptr::null()
        } else {
            window.as_ptr()
        };
        strm.avail_in = narrow_avail(window.len());
    }
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = narrow_avail(out.len());
}

/// Installs the reference's two windows. Identical rules; a different `z_stream` type.
fn install_windows_reference(strm: &mut oracle::z_stream, window: Option<&[u8]>, out: &mut [u8]) {
    assert!(
        !out.is_empty(),
        "deflate must never be offered a zero-length output window"
    );
    if let Some(window) = window {
        strm.next_in = if window.is_empty() {
            core::ptr::null()
        } else {
            window.as_ptr()
        };
        strm.avail_in = narrow_avail_oracle(window.len());
    }
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = narrow_avail_oracle(out.len());
}

/// Narrows a window length to the facade's `uInt`, which is what `avail_in`/`avail_out` are.
fn narrow_avail(len: usize) -> libz_rs_sys::uInt {
    libz_rs_sys::uInt::try_from(len).expect("a window length fits in a uInt")
}

/// Narrows a window length to the oracle's `uInt`.
fn narrow_avail_oracle(len: usize) -> oracle::uInt {
    oracle::uInt::try_from(len).expect("a window length fits in a uInt")
}

// =================================================================================================
//  The driver
//
//  ONE function, instantiated twice. Everything that decides *what* the library is asked to do --
//  when to refill the input window, how large each window is, which flush value each call carries,
//  when to stop -- lives here and therefore happens identically on both sides. The `Side` gates
//  above decide nothing; they only forward.
// =================================================================================================

/// Opens one side's stream for a configuration, reporting the stream and `deflateInit2_`'s status.
///
/// One open serves every cell of the configuration; [`cell`] leaves the stream reset and ready for
/// the next one. That is the brief's "prefer `deflateReset` over re-init where the C code permits
/// it", and it is not a micro-optimisation: an init at `memLevel` 9 and `windowBits` 15 allocates
/// and zeroes roughly half a megabyte across the window, the two hash arrays and the pending
/// buffer, and the exhaustive sweep would otherwise pay for that on every one of its cells.
///
/// # Why the stream is boxed
///
/// **A `z_stream` must not be relocated once `deflateInit2_` has seen it.** `deflate.c:444` stores
/// a back-pointer, `s->strm = strm`, into the state block, and `deflateStateCheck` at
/// `deflate.c:544` tests `s->strm != strm` on the way into every subsequent entry point. Move the
/// struct and the pointer no longer matches: `deflate` answers `Z_STREAM_ERROR`, `deflateEnd`
/// answers `Z_STREAM_ERROR` without freeing anything, and -- the part that matters for a
/// differential test -- **both implementations answer identically**, so a comparison of two empty
/// outputs would pass while measuring nothing at all.
///
/// This is not hypothetical. Returning the stream by value from this function, which is the obvious
/// Rust shape, moves it out of this frame and did exactly that; it was caught by
/// [`assert_flush_was_honoured`], which exists for precisely this class of silent no-op. Boxing
/// gives the struct an address that outlives the frame it was initialised in, and the box is what
/// callers pass around. A C caller gets this for free by declaring `z_stream strm;` in the frame
/// that also calls `deflateEnd`; a Rust caller has to arrange it.
fn open<S: Side>(config: Config) -> (Box<S::Stream>, c_int) {
    let mut stream = Box::new(S::stream());
    let status = S::init(&mut stream, config);
    (stream, status)
}

/// Closes one side's stream, reporting `deflateEnd`'s status.
fn close<S: Side>(stream: &mut S::Stream) -> c_int {
    S::end(stream)
}

/// Drives one cell to completion on an already-open stream and reports everything worth comparing.
///
/// `out_window` is `Some` to pin the output window to a given size and `None` to size it from this
/// side's own `deflateBound`. The caller drives the port first with `None` and then the reference
/// with `Some(port.out_window)`, so that both sides are driven with the *same* buffer sizes even in
/// the bound-sized case -- which is what keeps the comparison apples to apples, and why the bound
/// agreement is asserted separately rather than assumed here.
///
/// The stream is left **reset**, not ended, so the next cell of the same configuration can reuse
/// it. The reset's status and the bound taken after it are part of the reported outcome, so the
/// reuse is measured rather than trusted: a `deflateReset` that failed to return `strstart` to zero
/// would show up as a bound divergence on the very next cell.
///
/// # The flush schedule
///
/// `cell.flush` is what every call carries while there is still input to give. Once all of it has
/// been handed over *and* consumed, the call carries `Z_FINISH`, because that is the only way to
/// close the stream and because `deflate.c:1018` answers `Z_BUF_ERROR` for a non-`Z_FINISH` call
/// that has neither input nor a higher flush rank than the previous one. The two special cases are
/// documented on [`FLUSHES`].
fn cell<S: Side>(
    strm: &mut S::Stream,
    cell: Cell,
    input: &[u8],
    out_window: Option<usize>,
) -> Outcome {
    let bound_before = S::bound(strm, input.len());

    // The output window. `max(1)` because `deflate.c:995` answers `Z_BUF_ERROR` for `avail_out ==
    // 0`, and a bound is never zero anyway: `compressBound(0)` is 13, and `deflateBound(strm, 0)`
    // is 13 under the zlib wrapper and 7 for raw deflate. So the clamp is a belt-and-braces guard
    // rather than a live case.
    let window_len = out_window
        .unwrap_or_else(|| usize::try_from(bound_before.wide).expect("a bound fits in a usize"));
    let window_len = window_len.max(1);
    let mut scratch = vec![0_u8; window_len];

    // `Z_FINISH` means "no further input will be supplied". Sweeping it while feeding the input in
    // chunks would either drop the remainder or lie to the library, so for that one value the whole
    // input is handed over at once -- which is what the constant means.
    let input_chunk = if cell.flush == Z_FINISH {
        None
    } else {
        cell.chunking.input
    };

    // Two tripwires, and neither is a formality. They turn a pathological run into a named failure
    // rather than a hung test or an exhausted machine, and each is shaped so that a legitimate run
    // cannot reach it.
    //
    // The first, below, is a genuine progress guarantee: a `deflate` call returning `Z_OK` must
    // either consume an input byte or produce an output byte, and a short run of calls doing
    // neither is what a non-progressing loop looks like.
    //
    // The second caps the accumulated output, and it is deliberately NOT derived from
    // `deflateBound`. That is the interesting part: a bound describes a single `Z_FINISH` call, and
    // a run that flushes on every one of thousands of input bytes legitimately exceeds it, because
    // each intermediate flush emits its own block boundary. An earlier version of this tripwire
    // used the bound and fired on exactly that -- level 0, raw, `Z_PARTIAL_FLUSH`, one byte in and
    // one byte out, over `repetitive.bin` -- which was the harness misjudging the contract rather
    // than the encoder misbehaving. The allowance below is 64 output bytes per input byte, which no
    // correct encoder approaches and which still stops a runaway before it matters.
    let output_cap = 4096
        + 64 * (input.len() + 1)
        + usize::try_from(bound_before.wide).expect("a bound fits in a usize");

    let mut output = Vec::new();
    let mut calls = Vec::new();
    let mut handed = 0_usize;
    let mut avail_in = 0_usize;
    let mut ever_installed = false;
    let mut stalled = 0_u32;

    loop {
        // Refill only when the stream has consumed what it was last given. The first iteration
        // always installs a window, even for empty input, because `next_in` starts null and the
        // library has to be told that explicitly rather than left to find out.
        let mut window = None;
        if avail_in == 0 && (handed < input.len() || !ever_installed) {
            let remaining = input.len() - handed;
            let take = input_chunk.map_or(remaining, |chunk| chunk.min(remaining));
            window = Some(&input[handed..handed + take]);
            handed += take;
            avail_in = take;
            ever_installed = true;
        }

        let flush = if handed >= input.len() && avail_in == 0 {
            Z_FINISH
        } else {
            cell.flush
        };

        let record = S::pump(strm, window, &mut scratch, flush);
        output.extend_from_slice(&scratch[..record.produced]);
        let consumed = avail_in.saturating_sub(record.avail_in_after);
        avail_in = record.avail_in_after;
        calls.push(record);

        // Stop on the end of the stream, and on anything that is not plain progress. A refusal is
        // data to compare, not a reason to keep pushing.
        if record.status != Z_OK {
            break;
        }

        // The progress guarantee. A handful of consecutive no-ops is allowed before this fires, so
        // that a single call whose whole job was to install a window cannot trip it.
        stalled = if record.produced == 0 && consumed == 0 {
            stalled + 1
        } else {
            0
        };
        assert!(
            stalled < 4,
            "{side}: {stalled} consecutive deflate calls returned Z_OK consuming no input and \
             producing no output, after {calls} calls -- the stream is not progressing\n{describe}",
            side = S::NAME,
            calls = calls.len(),
            describe = describe(cell, input.len()),
        );

        assert!(
            output.len() <= output_cap,
            "{side}: {produced} output bytes for {len} input bytes exceeds the {output_cap}-byte \
             runaway cap after {calls} calls\n{describe}",
            side = S::NAME,
            produced = output.len(),
            len = input.len(),
            calls = calls.len(),
            describe = describe(cell, input.len()),
        );
    }

    let snapshot = S::snapshot(strm);
    let pending = S::pending(strm);
    let bound_after = S::bound(strm, input.len());
    let reset = S::reset(strm);
    let bound_after_reset = S::bound(strm, input.len());

    Outcome {
        bound_before,
        calls,
        output,
        snapshot,
        pending,
        bound_after,
        reset,
        bound_after_reset,
        out_window: window_len,
    }
}

// =================================================================================================
//  The comparison
//
//  Every difference is a finding, findings are accumulated rather than raced, and the report names
//  the full cell, the byte offset, both byte values, a hex window around the offset and both
//  lengths. A bare "not equal" would be nearly useless here: the value of this suite is measured by
//  how quickly a failing run points at one of the eight decision points.
// =================================================================================================

/// Compares one cell's two outcomes, panicking with a full report if anything differs.
fn compare(cell: Cell, sample: &Sample, port: &Outcome, reference: &Outcome) {
    let mut findings: Vec<String> = Vec::new();

    // 1. The bytes. This is the headline, and it is reported first so that it heads the message.
    if port.output != reference.output {
        findings.push(describe_byte_difference(&port.output, &reference.output));
    }

    // 2. Every status the run produced, call by call. Compared as a whole because the *shape* of a
    //    run carries information the final status does not, and because this is where the swept
    //    flush value is actually observable.
    if port.calls != reference.calls {
        findings.push(describe_call_difference(&port.calls, &reference.calls));
    }

    // 3. The swept flush value has to have reached the library for the cell to mean anything, and
    //    for `Z_TREES` what it means is a refusal: `deflate.c:985` answers `Z_STREAM_ERROR` for
    //    `flush > Z_BLOCK`, so `Z_TREES` is valid for `inflate` only. Asserting that explicitly is
    //    what stops a pair of empty outputs from being counted as a byte-identity match.
    assert_flush_was_honoured(cell, sample, port, reference);

    // 4. The accounting. A divergence here with matching bytes is the interesting case: matching
    //    output and a different `total_out` is a pending-buffer accounting bug, and a different
    //    `adler` means the checksum is being fed differently even though the encoder agrees.
    if port.snapshot.total_in != reference.snapshot.total_in {
        findings.push(format!(
            "total_in differs: port {port_total}, reference {ref_total}",
            port_total = port.snapshot.total_in,
            ref_total = reference.snapshot.total_in,
        ));
    }
    if port.snapshot.total_out != reference.snapshot.total_out {
        findings.push(format!(
            "total_out differs: port {port_total}, reference {ref_total} \
             (a divergence here with matching bytes is a pending-buffer accounting bug)",
            port_total = port.snapshot.total_out,
            ref_total = reference.snapshot.total_out,
        ));
    }
    if port.snapshot.adler != reference.snapshot.adler {
        findings.push(format!(
            "adler differs: port {port_adler:#010x}, reference {ref_adler:#010x} \
             (Adler-32 under the zlib wrapper, CRC-32 under gzip, untouched for raw)",
            port_adler = port.snapshot.adler,
            ref_adler = reference.snapshot.adler,
        ));
    }
    if port.snapshot.data_type != reference.snapshot.data_type {
        findings.push(format!(
            "data_type differs: port {port_type}, reference {ref_type} \
             (detect_data_type, trees.c:966-991, mask 0xf3ffc07f)",
            port_type = data_type(port.snapshot.data_type),
            ref_type = data_type(reference.snapshot.data_type),
        ));
    }

    // 5. Pending output. Close to free and it catches an accounting divergence the buffer cannot.
    if port.pending != reference.pending {
        findings.push(format!(
            "deflatePending differs: port {{status {port_status}, bytes {port_bytes}, \
             bits {port_bits}}}, reference {{status {ref_status}, bytes {ref_bytes}, \
             bits {ref_bits}}}",
            port_status = status(port.pending.status),
            port_bytes = port.pending.bytes,
            port_bits = port.pending.bits,
            ref_status = status(reference.pending.status),
            ref_bytes = reference.pending.bytes,
            ref_bits = reference.pending.bits,
        ));
    }

    // 6. The bounds, on a live stream, at all three points. Callers size their output buffers with
    //    these: a bound that is too small becomes a buffer overflow in CALLER code, and one that is
    //    too large breaks tests asserting exact sizes.
    compare_bounds(
        "deflateBound before deflating",
        port.bound_before,
        reference.bound_before,
        &mut findings,
    );
    compare_bounds(
        "deflateBound after deflating",
        port.bound_after,
        reference.bound_after,
        &mut findings,
    );
    compare_bounds(
        "deflateBound after deflateReset",
        port.bound_after_reset,
        reference.bound_after_reset,
        &mut findings,
    );

    // 7. The teardown pair. `deflateReset` is what lets a cell reuse an initialised stream, so its
    //    behaviour is compared rather than trusted, and the bound taken after it is what proves the
    //    reset really returned `strstart` to zero.
    if port.reset != reference.reset {
        findings.push(format!(
            "deflateReset status differs: port {port_status}, reference {ref_status}",
            port_status = status(port.reset),
            ref_status = status(reference.reset),
        ));
    }

    if !findings.is_empty() {
        report(cell, sample, port, reference, &findings);
    }
}

/// Asserts that the cell actually exercised what it claims to, and that a refusal is a refusal on
/// both sides.
///
/// Two cells could otherwise pass while measuring nothing. The first is `Z_TREES`, which `deflate`
/// rejects outright, so a cell sweeping it must assert the *refusal* rather than compare two empty
/// buffers. The second is any cell whose input is empty: there can be no intermediate call, so the
/// swept value never reaches the library and the cell is a duplicate of the `Z_FINISH` one. Both
/// are legitimate and both are stated, so that a reader of a green run knows which cells carried
/// weight.
fn assert_flush_was_honoured(cell: Cell, sample: &Sample, port: &Outcome, reference: &Outcome) {
    let swept = |outcome: &Outcome| {
        outcome
            .calls
            .iter()
            .find(|record| record.flush == cell.flush)
            .copied()
    };
    let (Some(port_call), Some(reference_call)) = (swept(port), swept(reference)) else {
        // The swept value never reached the library, which happens exactly when the input is empty
        // and the very first call is therefore already the closing `Z_FINISH`. Both sides agree on
        // that, because the schedule is computed by shared code before either is called.
        assert!(
            sample.bytes.is_empty() || cell.flush == Z_FINISH,
            "the swept flush value {flush} never reached deflate on a non-empty input, so the cell \
             measured nothing\n{describe}",
            flush = flush_value(cell.flush),
            describe = describe(cell, sample.bytes.len()),
        );
        return;
    };

    if cell.flush == Z_TREES {
        // `Z_TREES` is `inflate`-only. Both sides must refuse it, and with the same status.
        assert!(
            port_call.status == Z_STREAM_ERROR && reference_call.status == Z_STREAM_ERROR,
            "Z_TREES must be refused by deflate with Z_STREAM_ERROR on both sides \
             (zlib.h:178, deflate.c:985): port {port_status}, reference {ref_status}\n{describe}",
            port_status = status(port_call.status),
            ref_status = status(reference_call.status),
            describe = describe(cell, sample.bytes.len()),
        );
        return;
    }

    // Every other swept value is accepted, so a refusal means the matrix has drifted from the
    // header rather than that the encoder disagreed. Reporting it as such saves a reader from
    // triaging the encoder over a configuration error.
    assert!(
        port_call.status == Z_OK || port_call.status == Z_STREAM_END,
        "the port refused the swept flush value {flush} with {status}, which zlib.h:172-177 \
         documents as valid for deflate\n{describe}",
        flush = flush_value(cell.flush),
        status = status(port_call.status),
        describe = describe(cell, sample.bytes.len()),
    );
}

/// Compares one status both sides produced outside a cell -- `deflateInit2_` and `deflateEnd`.
///
/// Kept separate from [`compare`] because one initialised stream serves a whole configuration:
/// these two are compared once per configuration, not once per cell.
fn compare_status(what: &str, config: Config, port: c_int, reference: c_int) {
    assert!(
        port == reference,
        "{what} status differs: port {port_status}, reference {ref_status}\n  \
         config: level={level} windowBits={bits} ({container}) memLevel={mem} strategy={strategy}",
        port_status = status(port),
        ref_status = status(reference),
        level = config.level,
        bits = config.window_bits,
        container = config.container(),
        mem = config.mem_level,
        strategy = strategy(config.strategy),
    );
}

/// Compares one bound pair, pushing a finding that names which of the two forms disagreed.
fn compare_bounds(what: &str, port: Bounds, reference: Bounds, findings: &mut Vec<String>) {
    if port == reference {
        return;
    }
    findings.push(format!(
        "{what} differs: port {{deflateBound {port_narrow}, deflateBound_z {port_wide}}}, \
         reference {{deflateBound {ref_narrow}, deflateBound_z {ref_wide}}}\n    \
         a bound smaller than the reference's is a buffer overflow in caller code; a larger one \
         breaks callers asserting exact sizes (deflate.c:857-928)",
        port_narrow = port.narrow,
        port_wide = port.wide,
        ref_narrow = reference.narrow,
        ref_wide = reference.wide,
    ));
}

// =================================================================================================
//  Diagnostics
// =================================================================================================

/// The one-line description of a cell that every failure quotes.
///
/// Names every dimension of the matrix, because "the encoder disagrees" is only actionable once the
/// reader knows which level, container, `memLevel`, strategy, flush value, fixture and chunking
/// produced it.
fn describe(cell: Cell, input_len: usize) -> String {
    format!(
        "  cell: level={level} windowBits={bits} ({container}) memLevel={mem} \
         strategy={strategy} flush={flush} chunking={chunking} input={len} bytes",
        level = cell.config.level,
        bits = cell.config.window_bits,
        container = cell.config.container(),
        mem = cell.config.mem_level,
        strategy = strategy(cell.config.strategy),
        flush = flush_value(cell.flush),
        chunking = cell.chunking.name,
        len = input_len,
    )
}

/// Where two output buffers first differ, and what they look like around that point.
///
/// Reports the offset, both byte values, both lengths and a hex window on each side, because the
/// *position* of the first difference is the single most useful triage signal there is: a
/// difference in the first few bytes is a header or block-type problem, one deep in the payload is
/// a match selection problem, and one at the very end is a flush or checksum problem.
fn describe_byte_difference(port: &[u8], reference: &[u8]) -> String {
    let mut out = String::new();
    let at = port
        .iter()
        .zip(reference.iter())
        .position(|(left, right)| left != right);

    if let Some(offset) = at {
        let _ = write!(
            out,
            "OUTPUT BYTES DIFFER at offset {offset} (0x{offset:x}): \
             port 0x{port_byte:02x}, reference 0x{ref_byte:02x}\n    \
             lengths: port {port_len}, reference {ref_len}\n    \
             port      {port_window}\n    reference {ref_window}",
            port_byte = port[offset],
            ref_byte = reference[offset],
            port_len = port.len(),
            ref_len = reference.len(),
            port_window = hex_window(port, offset),
            ref_window = hex_window(reference, offset),
        );
    } else {
        // One is a strict prefix of the other. That is a distinct failure mode and worth saying so:
        // it is what a truncated final flush, a dropped checksum or a missing empty stored block
        // looks like, rather than a match-selection divergence.
        let (longer, shorter, whose) = if port.len() > reference.len() {
            (port, reference, "port")
        } else {
            (reference, port, "reference")
        };
        let _ = write!(
            out,
            "OUTPUT LENGTHS DIFFER and the shorter is a prefix of the longer: \
             port {port_len}, reference {ref_len}\n    \
             the {whose} emitted {extra} extra trailing byte(s): {tail}\n    \
             a trailing-only difference points at the final flush, the wrapper checksum or the \
             empty stored block, not at match selection",
            port_len = port.len(),
            ref_len = reference.len(),
            extra = longer.len() - shorter.len(),
            tail = hex_run(&longer[shorter.len()..]),
        );
    }
    out
}

/// How many bytes of context a hex window shows before the offset of interest.
const HEX_WINDOW_BEFORE: usize = 8;

/// How many bytes of context a hex window shows from the offset of interest onwards.
const HEX_WINDOW_AFTER: usize = 8;

/// A short hex window around `offset`, clamped to the buffer.
fn hex_window(bytes: &[u8], offset: usize) -> String {
    let start = offset.saturating_sub(HEX_WINDOW_BEFORE);
    let end = offset.saturating_add(HEX_WINDOW_AFTER).min(bytes.len());
    let mut out = format!("[{start}..{end}]");
    for byte in &bytes[start..end] {
        let _ = write!(out, " {byte:02x}");
    }
    out
}

/// A hex run, truncated so that a long tail cannot swamp the report.
fn hex_run(bytes: &[u8]) -> String {
    let shown = bytes.len().min(HEX_WINDOW_BEFORE + HEX_WINDOW_AFTER);
    let mut out = String::new();
    for byte in &bytes[..shown] {
        let _ = write!(out, "{byte:02x} ");
    }
    if shown < bytes.len() {
        let _ = write!(out, "... ({rest} more)", rest = bytes.len() - shown);
    }
    out
}

/// Where two call sequences first differ, in the terms a reader can act on.
fn describe_call_difference(port: &[CallRecord], reference: &[CallRecord]) -> String {
    let at = port
        .iter()
        .zip(reference.iter())
        .position(|(left, right)| left != right);

    match at {
        Some(index) => format!(
            "CALL SEQUENCE DIFFERS at call {index} (of port {port_len}, reference {ref_len})\n    \
             port      flush={port_flush} -> {port_status}, produced {port_made}, \
             avail_in after {port_left}\n    \
             reference flush={ref_flush} -> {ref_status}, produced {ref_made}, \
             avail_in after {ref_left}",
            port_len = port.len(),
            ref_len = reference.len(),
            port_flush = flush_value(port[index].flush),
            port_status = status(port[index].status),
            port_made = port[index].produced,
            port_left = port[index].avail_in_after,
            ref_flush = flush_value(reference[index].flush),
            ref_status = status(reference[index].status),
            ref_made = reference[index].produced,
            ref_left = reference[index].avail_in_after,
        ),
        None => format!(
            "CALL COUNT DIFFERS: port made {port_len} deflate calls, reference made {ref_len}, \
             and the shorter sequence is a prefix of the longer\n    \
             one side needed more calls to deliver the same bytes, which is a pending-output or \
             avail_out accounting divergence",
            port_len = port.len(),
            ref_len = reference.len(),
        ),
    }
}

/// Panics with the full report: the cell, every finding, and the triage list.
fn report(cell: Cell, sample: &Sample, port: &Outcome, reference: &Outcome, findings: &[String]) {
    let mut message = String::new();
    let _ = writeln!(
        message,
        "BYTE-IDENTICAL DIFFERENTIAL FAILURE -- the port and the C reference disagree\n\
         \n\
         fixture: {name} ({len} bytes)\n{cell}\n  output window: {window} bytes\n",
        name = sample.fixture.name,
        len = sample.bytes.len(),
        cell = describe(cell, sample.bytes.len()),
        window = port.out_window,
    );

    let _ = writeln!(message, "{count} finding(s):", count = findings.len());
    for (index, finding) in findings.iter().enumerate() {
        let _ = writeln!(message, "  [{n}] {finding}", n = index + 1);
    }

    let _ = writeln!(
        message,
        "\nsummary: port produced {port_len} bytes, reference produced {ref_len}\n\
         \n{TRIAGE}",
        port_len = port.output.len(),
        ref_len = reference.output.len(),
    );

    panic!("{message}");
}

/// A status code as its `zlib.h` name and value, so a log reads `Z_BUF_ERROR(-5)` rather than `-5`.
fn status(code: c_int) -> String {
    let name = match code {
        Z_OK => "Z_OK",
        Z_STREAM_END => "Z_STREAM_END",
        Z_STREAM_ERROR => "Z_STREAM_ERROR",
        Z_BUF_ERROR => "Z_BUF_ERROR",
        -1 => "Z_ERRNO",
        2 => "Z_NEED_DICT",
        -3 => "Z_DATA_ERROR",
        -4 => "Z_MEM_ERROR",
        -6 => "Z_VERSION_ERROR",
        _ => "unknown",
    };
    format!("{name}({code})")
}

/// A flush value as its `zlib.h` name and value. The values are not in the order intuition suggests
/// -- `Z_FINISH` is 4 and `Z_BLOCK` is 5 -- which is exactly why this exists.
fn flush_value(code: c_int) -> String {
    let name = match code {
        Z_NO_FLUSH => "Z_NO_FLUSH",
        Z_PARTIAL_FLUSH => "Z_PARTIAL_FLUSH",
        Z_SYNC_FLUSH => "Z_SYNC_FLUSH",
        Z_FULL_FLUSH => "Z_FULL_FLUSH",
        Z_FINISH => "Z_FINISH",
        Z_BLOCK => "Z_BLOCK",
        Z_TREES => "Z_TREES",
        _ => "unknown",
    };
    format!("{name}({code})")
}

/// A strategy as its `zlib.h` name and value (`zlib.h:200-204`).
fn strategy(code: c_int) -> String {
    let name = match code {
        Z_DEFAULT_STRATEGY => "Z_DEFAULT_STRATEGY",
        Z_FILTERED => "Z_FILTERED",
        Z_HUFFMAN_ONLY => "Z_HUFFMAN_ONLY",
        Z_RLE => "Z_RLE",
        Z_FIXED => "Z_FIXED",
        _ => "unknown",
    };
    format!("{name}({code})")
}

/// A `data_type` as its `zlib.h` name and value (`zlib.h:207-210`).
fn data_type(code: c_int) -> String {
    let name = match code {
        Z_BINARY => "Z_BINARY",
        Z_TEXT => "Z_TEXT",
        2 => "Z_UNKNOWN",
        _ => "unknown",
    };
    format!("{name}({code})")
}

// =================================================================================================
//  The sweep
// =================================================================================================

/// Runs every cell of `configs` x `flushes` x `chunkings` x `samples` and compares each one.
///
/// One stream per side per configuration, reset between cells. `tiny_window_fixture_limit` is the
/// single (chunking, fixture) constraint this file applies -- see
/// [`small_windows_only_on_small_fixtures`], which is the only place any comparison is held back
/// and which states exactly why. Pass `usize::MAX` for no constraint at all.
///
/// Returns the number of cells compared. The callers print it, so that a green run reports how much
/// it measured rather than merely that it passed -- a suite that silently stopped enumerating would
/// otherwise look exactly like a suite that passed.
fn sweep(
    configs: &[Config],
    flushes: &[c_int],
    chunkings: &[Chunking],
    samples: &[Sample],
    tiny_window_fixture_limit: usize,
) -> usize {
    let mut compared = 0_usize;

    for &config in configs {
        let (mut port_stream, port_init) = open::<Port>(config);
        let (mut reference_stream, reference_init) = open::<Reference>(config);
        compare_status("deflateInit2_", config, port_init, reference_init);
        assert!(
            port_init == Z_OK,
            "both sides refused a configuration this matrix asserts is valid: {status}\n  \
             config: level={level} windowBits={bits} ({container}) memLevel={mem} \
             strategy={strategy}\n  \
             deflateInit2_ rejects only what deflate.c:435-436 rejects, so reaching here means the \
             matrix has drifted from the header rather than that the encoder disagreed",
            status = status(port_init),
            level = config.level,
            bits = config.window_bits,
            container = config.container(),
            mem = config.mem_level,
            strategy = strategy(config.strategy),
        );

        for &flush in flushes {
            for &chunking in chunkings {
                for sample in samples {
                    if !small_windows_only_on_small_fixtures(
                        chunking,
                        sample,
                        tiny_window_fixture_limit,
                    ) {
                        continue;
                    }
                    let current = Cell {
                        config,
                        flush,
                        chunking,
                    };

                    // The port goes first so that the bound-sized output window is resolved once
                    // and the reference is driven with the identical number. The bounds themselves
                    // are compared inside `compare`, so a divergence there is reported rather than
                    // silently changing what the second run was asked to do.
                    let port =
                        cell::<Port>(&mut port_stream, current, &sample.bytes, chunking.output);
                    let reference = cell::<Reference>(
                        &mut reference_stream,
                        current,
                        &sample.bytes,
                        Some(port.out_window),
                    );

                    compare(current, sample, &port, &reference);
                    compared += 1;
                }
            }
        }

        let port_end = close::<Port>(&mut port_stream);
        let reference_end = close::<Reference>(&mut reference_stream);
        compare_status("deflateEnd", config, port_end, reference_end);
    }

    compared
}

/// The one (chunking, fixture) constraint this file applies, and the only place any comparison is
/// held back.
///
/// **A tiny window costs one `deflate` call per byte that passes through it.** That is the whole
/// cost model, and it is worth stating in numbers: `in=1` over `window_boundary.bin` is 66,560
/// calls per side for a single cell, and `out=1` over the same fixture is 66,591, because the
/// fixture is incompressible. A 251-byte window over it is 266 calls -- three orders of magnitude
/// cheaper -- so the discriminator is the *window size*, not which side of the stream it is on.
///
/// The rule: a window below [`SMALL_WINDOW_BYTES`] is paired only with fixtures at or below
/// `fixture_limit`. Every sweep states its own limit, so the constraint is visible at each call
/// site rather than buried here:
///
/// * The default sweep passes [`DEFAULT_TINY_WINDOW_LIMIT`], which keeps the tiny windows on the
///   seven fixtures of a few dozen bytes. Those are where suspending and resuming mid-header,
///   mid-flush-marker and mid-checksum actually happens, and they cost nothing; the three larger
///   fixtures are still driven at every window of 251 bytes or more.
/// * The exhaustive sweeps pass [`EXHAUSTIVE_TINY_WINDOW_LIMIT`], which admits nine of the ten
///   fixtures -- including both incompressible ones -- and holds back only `window_boundary.bin`.
/// * `ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=full` passes `usize::MAX`, lifting the constraint entirely.
///
/// Nothing is held back in one sweep that another does not enumerate, and `window_boundary.bin` is
/// driven at every larger window everywhere -- which is what exercises the window sliding and
/// maximum-distance rejection it exists for, since those depend on the total input rather than on
/// the chunk size.
fn small_windows_only_on_small_fixtures(
    chunking: Chunking,
    sample: &Sample,
    fixture_limit: usize,
) -> bool {
    let tiny = |window: Option<usize>| window.is_some_and(|bytes| bytes < SMALL_WINDOW_BYTES);
    if tiny(chunking.input) || tiny(chunking.output) {
        return sample.bytes.len() <= fixture_limit;
    }
    true
}

/// The window size below which a chunking's cost tracks the fixture size closely enough to matter.
///
/// 64 bytes. Every chunking in [`CHUNKINGS_ALL`] is either at or below 7 or at or above 251, so
/// this threshold does not sit near any of them and moving it a little either way changes nothing.
const SMALL_WINDOW_BYTES: usize = 64;

/// The default sweep's fixture limit for the tiniest windows: 1 KiB.
///
/// Admits the seven fixtures of a few dozen bytes and holds back `random.bin` (8,192),
/// `repetitive.bin` (16,384) and `window_boundary.bin` (66,560). That is what keeps a bare
/// `cargo test` cheap: those three carry essentially all of the corpus's bytes, and a one-byte
/// window over them is one `deflate` call per byte on both sides. The held-back cells are
/// enumerated by [`exhaustive_chunking_matrix`], which is the sweep whose job the chunking
/// dimension is.
const DEFAULT_TINY_WINDOW_LIMIT: usize = 1024;

/// The exhaustive sweeps' fixture limit for the tiniest windows: 20 KiB.
///
/// Admits nine of the ten fixtures -- including `random.bin`, whose 8,192 incompressible bytes are
/// what make a one-byte window bite, and `repetitive.bin` at 16,384 -- and holds back only
/// `window_boundary.bin`, whose 66,560 incompressible bytes are on their own the single largest
/// contributor to the total call count. `ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=full` lifts even this.
const EXHAUSTIVE_TINY_WINDOW_LIMIT: usize = 20 * 1024;

/// Every `windowBits` the matrix enumerates, across all three container formats: 21 values.
fn all_window_bits() -> Vec<c_int> {
    WINDOW_BITS_ZLIB
        .iter()
        .chain(WINDOW_BITS_RAW)
        .chain(WINDOW_BITS_GZIP)
        .copied()
        .collect()
}

/// The full cross product for the given levels: `levels` x 21 `windowBits` x 9 `memLevel`s x 5
/// strategies, which is 945 configurations per level and 9,450 for all ten.
///
/// Taking the levels as a parameter is what lets the exhaustive configuration sweep be *sharded*
/// one level per `#[test]`, so that cargo's own harness runs the shards concurrently. That is the
/// whole parallelisation strategy, and it needs no dependency and no thread of this file's own:
/// `--test-threads N` is the knob.
fn configurations_for_levels(levels: &[c_int]) -> Vec<Config> {
    let window_bits_values = all_window_bits();
    let mut configs = Vec::with_capacity(
        levels.len() * window_bits_values.len() * MEM_LEVELS.len() * STRATEGIES.len(),
    );
    for &level in levels {
        for &window_bits in &window_bits_values {
            for &mem_level in MEM_LEVELS {
                for &strategy in STRATEGIES {
                    configs.push(Config {
                        level,
                        window_bits,
                        mem_level,
                        strategy,
                    });
                }
            }
        }
    }
    configs
}

/// The representative configuration set the default sweep uses.
///
/// Representative rather than arbitrary, and each group is here for a stated reason:
///
/// * **Every level at the defaults.** The level selects a row of `configuration_table`
///   (`deflate.c:112`), which is decision point 1 and the one a mismatch most often implicates.
/// * **One level per algorithm family, in all three container formats.** `deflate.c`'s table binds
///   level 0 to `deflate_stored`, 1-3 to `deflate_fast` and 4-9 to `deflate_slow`, and the
///   representative of each is crossed with the zlib, raw and gzip wrappers. The grouping is not
///   hardcoded: it is read back from the oracle's own `configuration_table` accessors, so a change
///   to the C table is a test failure rather than a silently stale subset.
/// * **Every strategy.** `Z_FILTERED` and `Z_RLE` change match acceptance, `Z_HUFFMAN_ONLY`
///   disables matching, and `Z_FIXED` forces the static trees -- decision points 4 and 8.
/// * **The extreme `memLevel`s.** 1 and 9 are the two ends of `deflateBound_z`'s `w_bits <=
///   hash_bits` branch (`deflate.c:918-922`) and of the `lit_bufsize` sizing, and 2 is the value
///   the bound's own comment calls out as the lowest that may not use stored blocks.
fn representative_configurations() -> Vec<Config> {
    let mut configs = Vec::new();

    for &level in LEVELS {
        configs.push(Config {
            level,
            ..Config::defaults()
        });
    }

    for level in algorithm_family_representatives() {
        for window_bits in [15, -15, 31] {
            configs.push(Config {
                level,
                window_bits,
                ..Config::defaults()
            });
        }
    }

    for &strategy in STRATEGIES {
        configs.push(Config {
            strategy,
            ..Config::defaults()
        });
    }

    for mem_level in [1, 2, 9] {
        for level in algorithm_family_representatives() {
            configs.push(Config {
                level,
                mem_level,
                ..Config::defaults()
            });
        }
    }

    configs.sort_unstable_by_key(|config| {
        (
            config.level,
            config.window_bits,
            config.mem_level,
            config.strategy,
        )
    });
    configs.dedup();
    configs
}

/// One level from each of the reference's three compressor families, read back from the oracle's
/// own `configuration_table` rather than hardcoded.
///
/// `deflate.c:114-124` binds level 0 to `deflate_stored`, levels 1-3 to `deflate_fast` and levels
/// 4-9 to `deflate_slow`; `src/oracle.rs` exposes that binding through `oracle_config(level).func`.
/// Deriving the representatives from it means a change to the C table shows up as a changed subset
/// -- or, if the grouping vanished entirely, as the assertion below.
fn algorithm_family_representatives() -> Vec<c_int> {
    let rows = oracle::oracle_config_table_len();
    let mut seen: Vec<oracle::OracleCompressFunc> = Vec::new();
    let mut levels = Vec::new();

    for row in 0..rows {
        let Some(config) = oracle::oracle_config(row) else {
            continue;
        };
        let Some(func) = config.func else {
            continue;
        };
        if seen.contains(&func) {
            continue;
        }
        seen.push(func);
        levels.push(c_int::try_from(row).expect("a table row index fits in a c_int"));
    }

    assert!(
        levels.len() == 3,
        "the reference's configuration_table names {found} compressor families, expected 3 \
         (deflate_stored at level 0, deflate_fast at 1-3, deflate_slow at 4-9, deflate.c:114-124); \
         an oracle built with FASTEST would report 2",
        found = levels.len(),
    );
    levels
}

// =================================================================================================
//  The exhaustive gate
// =================================================================================================

/// The variable that arms the two exhaustive sweeps, named to the tree's `ZLIB_RS_*` convention.
const ENV_EXHAUSTIVE: &str = "ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE";

/// The value that additionally lifts the fixture-size constraint inside the chunking sweep.
const ENV_EXHAUSTIVE_FULL: &str = "full";

/// How the exhaustive sweeps were asked to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exhaustive {
    /// The variable is unset or empty: the sweep reports that it was not asked and does nothing.
    Disarmed,
    /// Armed at the affordable scope.
    Armed,
    /// Armed with the fixture-size constraint lifted.
    Unabridged,
}

/// Reads [`ENV_EXHAUSTIVE`].
///
/// Any non-empty value other than `0` arms the sweeps, and `full` additionally lifts the
/// constraint. `0` is honoured as "off" so that a CI matrix can carry the variable with a per-job
/// value rather than adding and removing it.
fn exhaustive() -> Exhaustive {
    match std::env::var(ENV_EXHAUSTIVE) {
        Ok(value) if value == ENV_EXHAUSTIVE_FULL => Exhaustive::Unabridged,
        Ok(value) if !value.is_empty() && value != "0" => Exhaustive::Armed,
        _ => Exhaustive::Disarmed,
    }
}

/// Reports that an exhaustive sweep was reached without being armed, and returns whether to
/// proceed.
///
/// This is the one place the suite declines to do work, and it is not a skipped comparison: the
/// cells it would have run are enumerated by the default sweep's superset in CI, where the variable
/// is set. The message names the remedy, following the convention
/// `crates/libz-rs-sys/tests/symbol_parity.rs` establishes for a genuine precondition.
fn armed(what: &str) -> bool {
    let mode = exhaustive();
    if mode == Exhaustive::Disarmed {
        println!(
            "SKIP: {what} was reached with {ENV_EXHAUSTIVE} unset, so it did no work.\n      \
             CI must run: {ENV_EXHAUSTIVE}=1 cargo test --locked -p zlib-rs-differential \
             --release --test byte_identical -- --ignored --test-threads 4\n      \
             set {ENV_EXHAUSTIVE}={ENV_EXHAUSTIVE_FULL} to additionally lift the fixture-size \
             constraint inside the chunking sweep."
        );
        return false;
    }
    true
}

// =================================================================================================
//  The tests
// =================================================================================================

/// The plumbing anchor: the fixtures load, and both sides agree with the numbers measured during
/// planning.
///
/// Run this first when something looks wrong. It is deliberately tiny and deliberately quotes
/// external constants: `compressBound(14) == 27` and a 19-byte level-6 zlib stream for the 14-byte
/// `hello, hello!\0` payload were both measured from a C driver linked against this very oracle, so
/// a failure here is a fixture-loading or bound-plumbing problem rather than an encoder one -- and
/// there is no point triaging the encoder until it passes.
#[test]
fn plumbing_anchor_matches_the_measured_numbers() {
    let corpus = load_corpus();
    assert!(
        corpus.len() == FIXTURES.len(),
        "every fixture in the contract must load"
    );

    let hello = sample(&corpus, "hello.bin");
    assert!(
        hello.bytes == b"hello, hello!\0",
        "hello.bin must be the test/example.c payload including its trailing NUL"
    );

    // `compressBound(14) == 27`, on both sides and in both widths.
    let expected = Bounds {
        narrow: 27,
        wide: 27,
    };
    let port = Port::compress_bounds(hello.bytes.len());
    let reference = Reference::compress_bounds(hello.bytes.len());
    assert!(
        port == expected && reference == expected,
        "compressBound(14) is 27 for this configuration: port {port:?}, reference {reference:?}"
    );

    // The 14-byte payload compresses to 19 bytes at the default level through `compress2`.
    let mut port_out = vec![0_u8; 27];
    let mut reference_out = vec![0_u8; 27];
    let (port_status, port_len) = Port::compress2(&mut port_out, &hello.bytes, -1);
    let (reference_status, reference_len) =
        Reference::compress2(&mut reference_out, &hello.bytes, -1);
    assert!(
        port_status == Z_OK && reference_status == Z_OK,
        "compress2 must succeed: port {port_status}, reference {reference_status}"
    );
    assert!(
        port_len == 19 && reference_len == 19,
        "level-6 zlib output for this payload is 19 bytes: \
         port {port_len}, reference {reference_len}"
    );
    assert!(
        port_out[..port_len] == reference_out[..reference_len],
        "the anchor payload's 19 bytes must be identical\n{difference}",
        difference =
            describe_byte_difference(&port_out[..port_len], &reference_out[..reference_len]),
    );

    // And the same version string, since `deflateInit2_` compares the caller's against the
    // library's and a mismatch in the first byte would answer `Z_VERSION_ERROR` rather than
    // compress anything. SAFETY: `c_zlibVersion` is niladic and returns a pointer to a
    // NUL-terminated string literal with static storage duration compiled into the oracle archive,
    // valid for reads through its terminator for the whole program.
    let reference_version = unsafe { core::ffi::CStr::from_ptr(oracle::c_zlibVersion()) };
    assert!(
        reference_version == ZLIB_VERSION,
        "the two sides must report the same ZLIB_VERSION: \
         port {ZLIB_VERSION:?}, reference {reference_version:?}"
    );
}

/// The default sweep: a representative subset of the matrix, over every fixture, at several
/// chunkings.
///
/// This is what a bare `cargo test` runs, so it is built to be fast enough to run on every change
/// and broad enough that a real divergence has nowhere obvious to hide: every level, every
/// strategy, every flush value, all three container formats, the extreme `memLevel`s, all ten
/// fixtures, and one chunking of each shape including a one-byte output window.
#[test]
fn default_matrix_is_byte_identical() {
    let corpus = load_corpus();
    let configs = representative_configurations();
    let started = Instant::now();

    let compared = sweep(
        &configs,
        FLUSHES,
        CHUNKINGS_DEFAULT,
        &corpus,
        DEFAULT_TINY_WINDOW_LIMIT,
    );

    println!(
        "default matrix: {compared} cells over {configs} configurations x {flushes} flush values \
         x {chunkings} chunkings x {fixtures} fixtures in {elapsed:.1?}",
        configs = configs.len(),
        flushes = FLUSHES.len(),
        chunkings = CHUNKINGS_DEFAULT.len(),
        fixtures = corpus.len(),
        elapsed = started.elapsed(),
    );
    assert!(compared > 0, "the default sweep must compare something");
}

/// One level's slice of the full configuration cross product: 21 `windowBits` x 9 `memLevel`s x 5
/// strategies x 7 flush values x 4 chunkings x 10 fixtures.
///
/// The ten shards together are the full product -- 9,450 configurations and 66,150 (configuration,
/// flush) pairs -- and sharding by level is what lets cargo's harness run them concurrently. The
/// chunkings are [`CHUNKINGS_INPUT_SIDE`]; the rest of the chunking dimension is enumerated by
/// [`exhaustive_chunking_matrix`], for the reason the module docs record.
fn exhaustive_configuration_shard(what: &str, level: c_int) {
    if !armed(what) {
        return;
    }

    let corpus = load_corpus();
    let configs = configurations_for_levels(&[level]);
    assert!(
        configs.len() == 945,
        "one level's slice is 21 windowBits x 9 memLevels x 5 strategies = 945 configurations, \
         not {found}",
        found = configs.len(),
    );

    let started = Instant::now();
    let compared = sweep(
        &configs,
        FLUSHES,
        CHUNKINGS_INPUT_SIDE,
        &corpus,
        EXHAUSTIVE_TINY_WINDOW_LIMIT,
    );

    println!(
        "exhaustive configuration matrix, level {level}: {compared} cells over {configs} \
         configurations in {elapsed:.1?}",
        configs = configs.len(),
        elapsed = started.elapsed(),
    );
}

/// Declares one `#[ignore]`d shard of the exhaustive configuration sweep.
///
/// Ten near-identical `#[test]` functions is what cargo's harness needs in order to run the shards
/// in parallel -- it parallelises across test functions, not within one -- and a macro is how they
/// are written once. Nothing else in this file is generated.
macro_rules! configuration_shard {
    ($name:ident, $level:expr) => {
        #[test]
        #[ignore = "one shard of the full cross product; armed by ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE"]
        fn $name() {
            exhaustive_configuration_shard(stringify!($name), $level);
        }
    };
}

configuration_shard!(exhaustive_configuration_matrix_level_0, 0);
configuration_shard!(exhaustive_configuration_matrix_level_1, 1);
configuration_shard!(exhaustive_configuration_matrix_level_2, 2);
configuration_shard!(exhaustive_configuration_matrix_level_3, 3);
configuration_shard!(exhaustive_configuration_matrix_level_4, 4);
configuration_shard!(exhaustive_configuration_matrix_level_5, 5);
configuration_shard!(exhaustive_configuration_matrix_level_6, 6);
configuration_shard!(exhaustive_configuration_matrix_level_7, 7);
configuration_shard!(exhaustive_configuration_matrix_level_8, 8);
configuration_shard!(exhaustive_configuration_matrix_level_9, 9);

/// The ten shards must cover exactly the levels `zlib.h` documents: none missing, none twice.
///
/// A sharded sweep has one failure mode a monolithic one does not -- a shard nobody declared -- and
/// the ten invocations above are hand-written. This asserts that the set of levels they name is
/// exactly [`LEVELS`] and that the ten slices reconstitute the full 9,450-configuration product,
/// which is the one part of the sharding a reader cannot check by looking in a single place.
#[test]
fn exhaustive_configuration_shards_cover_every_level() {
    let declared = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    assert!(
        declared == LEVELS,
        "the configuration_shard! invocations name {declared:?}, but the matrix's levels are \
         {LEVELS:?}; every level needs a shard or the exhaustive sweep silently skips it"
    );
    let total = configurations_for_levels(LEVELS).len();
    assert!(
        total == 9_450,
        "the ten shards together must be the full cross product: 10 levels x 21 windowBits x 9 \
         memLevels x 5 strategies = 9,450 configurations, not {total}"
    );
}

/// The full chunking set, including the one-byte output window, over every fixture.
///
/// The configuration set here spans every level, all three container formats, all five strategies
/// and the extreme `memLevel`s -- [`representative_configurations`] -- because it is the *chunking*
/// dimension this sweep exists to enumerate exhaustively, and the product of both full sets is what
/// the module docs record as unaffordable.
///
/// This sweep is also where the cells the *default* sweep holds back are enumerated: it raises the
/// tiny-window fixture limit from [`DEFAULT_TINY_WINDOW_LIMIT`] to
/// [`EXHAUSTIVE_TINY_WINDOW_LIMIT`], which brings `random.bin` and `repetitive.bin` under every
/// chunking including the one-byte windows.
///
/// # The one constraint, and how to lift it
///
/// A tiny window costs one `deflate` call per byte through it, and `window_boundary.bin` is 66,560
/// incompressible bytes, so a one-byte window over it is some 66,600 calls per side for a single
/// cell. It is therefore the one fixture [`EXHAUSTIVE_TINY_WINDOW_LIMIT`] holds back from the four
/// tiniest chunkings; every larger window still covers it.
/// `ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE=full` raises the limit to `usize::MAX` and pays for the
/// unabridged run.
#[test]
#[ignore = "the full chunking sweep; armed by ZLIB_RS_DIFFERENTIAL_EXHAUSTIVE and run by CI"]
fn exhaustive_chunking_matrix() {
    if !armed("exhaustive_chunking_matrix") {
        return;
    }

    let corpus = load_corpus();
    let configs = representative_configurations();
    let unabridged = exhaustive() == Exhaustive::Unabridged;
    let limit = if unabridged {
        usize::MAX
    } else {
        EXHAUSTIVE_TINY_WINDOW_LIMIT
    };
    let started = Instant::now();

    let compared = sweep(&configs, FLUSHES, CHUNKINGS_ALL, &corpus, limit);

    println!(
        "exhaustive chunking matrix: {compared} cells over {configs} configurations x {chunkings} \
         chunkings in {elapsed:.1?}{note}",
        configs = configs.len(),
        chunkings = CHUNKINGS_ALL.len(),
        elapsed = started.elapsed(),
        note = if unabridged {
            " (unabridged: the fixture-size constraint was lifted)"
        } else {
            ""
        },
    );
}

/// `windowBits` 8, `-8` and 24 -- the three boundary values the byte-identity matrix deliberately
/// excludes -- must be handled identically by both sides.
///
/// Verified against `deflate.c:422-439`, which is worth reading in order because the three cases
/// are produced by two different lines:
///
/// * `windowBits == 8` with the zlib wrapper is **accepted and silently promoted to 9** at L439,
///   commented "until 256-byte window bug fixed". So the two are not merely both valid: they are
///   the same encoder, and this test asserts the promotion by comparing the *output* of 8 against
///   the output of 9 rather than merely comparing statuses.
/// * `-8` and 24 are **refused**. L422-426 makes `-8` a raw 8-bit window and L429-431 makes 24 a
///   gzip 8-bit one, and the rejection test at L435-436 includes `windowBits == 8 && wrap != 1`, so
///   both answer `Z_STREAM_ERROR`.
///
/// A status-parity assertion is the right shape for the refusals: two refusals produce no bytes, so
/// counting them as a byte-identity match would prove nothing, which is exactly why they are here
/// rather than in the matrix.
#[test]
fn window_bits_boundary_parity() {
    let corpus = load_corpus();

    // The two refusals.
    for window_bits in [-8, 24] {
        let config = Config {
            window_bits,
            ..Config::defaults()
        };
        let (mut port_stream, port_init) = open::<Port>(config);
        let (mut reference_stream, reference_init) = open::<Reference>(config);
        compare_status("deflateInit2_", config, port_init, reference_init);
        assert!(
            port_init == Z_STREAM_ERROR,
            "windowBits {window_bits} requests a 256-byte window for a wrapped format, which \
             deflate.c:435-436 refuses: got {status}",
            status = status(port_init),
        );
        // Nothing was allocated, so `deflateEnd` sees an uninitialised stream. Both sides must say
        // so identically, which is a free extra comparison on the refusal path.
        let port_end = close::<Port>(&mut port_stream);
        let reference_end = close::<Reference>(&mut reference_stream);
        compare_status(
            "deflateEnd after a refused init",
            config,
            port_end,
            reference_end,
        );
    }

    // The promotion. `windowBits` 8 must produce byte-for-byte what 9 produces, on both sides.
    for sample in &corpus {
        for &flush in FLUSHES {
            let promoted = Cell {
                config: Config {
                    window_bits: 8,
                    ..Config::defaults()
                },
                flush,
                chunking: SINGLE_SHOT,
            };
            let nine = Cell {
                config: Config {
                    window_bits: 9,
                    ..Config::defaults()
                },
                flush,
                chunking: SINGLE_SHOT,
            };

            let promoted_bytes = one_off::<Port>(promoted, &sample.bytes);
            let nine_bytes = one_off::<Port>(nine, &sample.bytes);
            assert!(
                promoted_bytes == nine_bytes,
                "the port must promote windowBits 8 to 9 (deflate.c:439), so the two must emit the \
                 same bytes for {name}\n{difference}",
                name = sample.fixture.name,
                difference = describe_byte_difference(&promoted_bytes, &nine_bytes),
            );

            let reference_promoted = one_off::<Reference>(promoted, &sample.bytes);
            assert!(
                promoted_bytes == reference_promoted,
                "windowBits 8 must be byte-identical across implementations for \
                 {name}\n{difference}",
                name = sample.fixture.name,
                difference = describe_byte_difference(&promoted_bytes, &reference_promoted),
            );
        }
    }
}

/// Runs one cell on a stream of its own and returns just the output bytes.
///
/// For the handful of comparisons that are about a configuration rather than about a cell, where
/// carrying the whole [`Outcome`] would obscure the point.
fn one_off<S: Side>(current: Cell, input: &[u8]) -> Vec<u8> {
    let (mut stream, init) = open::<S>(current.config);
    assert!(
        init == Z_OK,
        "{side}: deflateInit2_ refused a configuration this comparison asserts is valid: {status}",
        side = S::NAME,
        status = status(init),
    );
    let outcome = cell::<S>(&mut stream, current, input, current.chunking.output);
    let end = close::<S>(&mut stream);
    assert!(
        end == Z_OK,
        "{side}: deflateEnd returned {status}",
        side = S::NAME,
        status = status(end),
    );
    outcome.output
}

/// The four bound functions, compared numerically over a `sourceLen` sweep that reaches the
/// saturation branches.
///
/// These are not informational. Callers size their output buffers with them, so a bound *smaller*
/// than the reference's is a buffer overflow in caller code and a *larger* one breaks every caller
/// asserting an exact size. What is pinned here, from `deflate.c:857-928` and `compress.c:91-99`:
///
/// * `compressBound_z(n) = n + (n>>12) + (n>>14) + (n>>25) + 13`, saturating to `(z_size_t)-1`;
///   `compressBound(n)` narrows and answers `(uLong)-1` when the value does not fit.
/// * `deflateBound_z` is **state-dependent**: `fixedlen = n + (n>>3) + (n>>8) + (n>>9) + 4` and
///   `storelen = n + (n>>5) + (n>>7) + (n>>11) + 7`, both saturating; when `deflateStateCheck`
///   fails it answers `max(fixedlen, storelen) + 18` with no stream at all; otherwise it adds a
///   `wraplen` of 0 for raw, `6 + (strstart ? 4 : 0)` for zlib and 18 for gzip, and at anything
///   other than `w_bits == 15 && hash_bits == 15` it returns one of the two conservative bounds
///   instead of the tight one.
///
/// The per-cell comparison in [`compare`] already takes `deflateBound` on a live, identically
/// configured stream before and after deflating -- which is what exercises the `strstart ? 4 : 0`
/// term in both directions. This test adds the two things a cell cannot: the NULL-stream branch,
/// and lengths large enough to saturate.
#[test]
fn bound_functions_agree_numerically() {
    let corpus = load_corpus();

    // The stream-free pair, and the NULL-stream branch of the stream-taking one. Neither needs an
    // initialised stream on either side, which is exactly what makes the NULL case worth including.
    for &source_len in &bound_source_lengths(&corpus) {
        let port = Port::compress_bounds(source_len);
        let reference = Reference::compress_bounds(source_len);
        assert!(
            port == reference,
            "compressBound({source_len}) differs: port {port:?}, reference {reference:?}\n    \
             compress.c:91-99 pins n + (n>>12) + (n>>14) + (n>>25) + 13, saturating"
        );

        let port = Port::bound_of_null_stream(source_len);
        let reference = Reference::bound_of_null_stream(source_len);
        assert!(
            port == reference,
            "deflateBound(NULL, {source_len}) differs: port {port:?}, reference {reference:?}\n    \
             deflate.c:876-879 pins max(fixedlen, storelen) + 18 for a stream that fails \
             deflateStateCheck"
        );
    }

    // The state-dependent form, on live streams across the whole representative configuration set
    // and both sides of the `strstart` branch. `bound_after` is taken once the fixture has been
    // deflated, so the zlib wrapper term is `6 + 4` there and `6 + 0` before.
    let configs = representative_configurations();
    let mut compared = 0_usize;
    for &config in &configs {
        let (mut port_stream, port_init) = open::<Port>(config);
        let (mut reference_stream, reference_init) = open::<Reference>(config);
        compare_status("deflateInit2_", config, port_init, reference_init);

        for &source_len in &bound_source_lengths(&corpus) {
            let port = Port::bound(&mut port_stream, source_len);
            let reference = Reference::bound(&mut reference_stream, source_len);
            assert!(
                port == reference,
                "deflateBound(strm, {source_len}) differs on a freshly configured stream: \
                 port {port:?}, reference {reference:?}\n  \
                 config: level={level} windowBits={bits} ({container}) memLevel={mem} \
                 strategy={strategy}\n    \
                 wraplen is 0 for raw, 6 + (strstart ? 4 : 0) for zlib and 18 for gzip \
                 (deflate.c:882-895), and w_bits != 15 || hash_bits != 15 selects a conservative \
                 bound instead of the tight one (deflate.c:918-922)",
                level = config.level,
                bits = config.window_bits,
                container = config.container(),
                mem = config.mem_level,
                strategy = strategy(config.strategy),
            );
            compared += 1;
        }

        let port_end = close::<Port>(&mut port_stream);
        let reference_end = close::<Reference>(&mut reference_stream);
        compare_status("deflateEnd", config, port_end, reference_end);
    }

    println!(
        "bound parity: {compared} live-stream comparisons over {configs} configurations, plus the \
         stream-free and NULL-stream forms",
        configs = configs.len(),
    );
}

/// The `sourceLen` values every bound comparison sweeps.
///
/// 0 and 1 are the degenerate ends; the fixture sizes are the lengths the rest of the suite
/// actually deflates, so a bound divergence shows up on the same numbers a byte divergence would;
/// and the three large values reach the saturation branches. `usize::MAX` is the only argument for
/// which `fixedlen < sourceLen` is certain, so it is the one that proves the saturation is
/// implemented at all rather than merely never reached.
fn bound_source_lengths(corpus: &[Sample]) -> Vec<usize> {
    let mut lengths = vec![0, 1, 2, 3, 255, 256, 4096];
    lengths.extend(corpus.iter().map(|sample| sample.bytes.len()));
    lengths.extend([
        usize::from(u16::MAX),
        1 << 20,
        1 << 25,
        (1 << 25) + 1,
        usize::MAX / 2,
        usize::MAX - 1,
        usize::MAX,
    ]);
    lengths.sort_unstable();
    lengths.dedup();
    lengths
}

/// `detect_data_type`'s three exits, on the fixtures whose purpose is to reach them.
///
/// The per-cell comparison already holds the two sides to the same `data_type` everywhere. This
/// adds the absolute expectation for the four fixtures whose verdict is a property of the bytes
/// rather than of the level: any block-listed byte with a nonzero literal frequency yields
/// `Z_BINARY` immediately (`trees.c:975-977`); failing that, a nonzero frequency at 9, 10, 13 or
/// anywhere in `32..LITERALS` yields `Z_TEXT` (`trees.c:980-985`); failing both, the fall-through
/// yields `Z_BINARY` (`trees.c:990`).
///
/// The level matters and is fixed at the default for a documented reason: `_tr_flush_block`
/// consults the heuristic only when `s->level > 0`, so a level-0 stream leaves `data_type` at
/// `Z_UNKNOWN`.
#[test]
fn data_type_classification_agrees_and_matches_the_heuristic() {
    let corpus = load_corpus();
    let config = Config::defaults();
    assert!(config.level > 0, "the heuristic only runs above level 0");

    let (mut port_stream, port_init) = open::<Port>(config);
    let (mut reference_stream, reference_init) = open::<Reference>(config);
    compare_status("deflateInit2_", config, port_init, reference_init);

    for sample in &corpus {
        let current = Cell {
            config,
            flush: Z_NO_FLUSH,
            chunking: SINGLE_SHOT,
        };
        let port = cell::<Port>(&mut port_stream, current, &sample.bytes, None);
        let reference = cell::<Reference>(
            &mut reference_stream,
            current,
            &sample.bytes,
            Some(port.out_window),
        );
        compare(current, sample, &port, &reference);

        if let Some(expected) = sample.fixture.data_type {
            assert!(
                port.snapshot.data_type == expected,
                "{name} must be classified {want}, and both sides reported {got} \
                 (detect_data_type, trees.c:966-991, mask 0xf3ffc07f)",
                name = sample.fixture.name,
                want = data_type(expected),
                got = data_type(port.snapshot.data_type),
            );
        }
    }

    let port_end = close::<Port>(&mut port_stream);
    let reference_end = close::<Reference>(&mut reference_stream);
    compare_status("deflateEnd", config, port_end, reference_end);
}

/// `deflateReset` must leave a stream indistinguishable from a freshly initialised one, on both
/// sides.
///
/// The whole suite reuses one stream per configuration and resets between cells, so this is the
/// assumption every other cell rests on: if a reset left any residue -- a stale `strstart`, an
/// un-rewound pending buffer, a surviving hash chain -- the *second* cell of every configuration
/// would be comparing a different encoder state than the first, and a real divergence could hide
/// behind it. Rather than trust that, this compares a cell run after N resets against the same cell
/// run on a virgin stream, on each side independently and then across the two.
#[test]
fn deflate_reset_restores_a_virgin_stream() {
    let corpus = load_corpus();
    let config = Config::defaults();

    for sample in &corpus {
        let current = Cell {
            config,
            flush: Z_NO_FLUSH,
            chunking: SINGLE_SHOT,
        };
        let virgin_port = one_off::<Port>(current, &sample.bytes);
        let virgin_reference = one_off::<Reference>(current, &sample.bytes);

        let (mut port_stream, port_init) = open::<Port>(config);
        let (mut reference_stream, reference_init) = open::<Reference>(config);
        compare_status("deflateInit2_", config, port_init, reference_init);

        // Three cells on one stream. `cell` resets at the end of each, so the second and third both
        // start from a reset rather than from an init.
        for round in 0..3 {
            let port = cell::<Port>(&mut port_stream, current, &sample.bytes, None);
            let reference = cell::<Reference>(
                &mut reference_stream,
                current,
                &sample.bytes,
                Some(port.out_window),
            );
            compare(current, sample, &port, &reference);

            assert!(
                port.output == virgin_port,
                "the port's output after {round} reset(s) differs from a virgin stream's for \
                 {name}\n{difference}",
                name = sample.fixture.name,
                difference = describe_byte_difference(&port.output, &virgin_port),
            );
            assert!(
                reference.output == virgin_reference,
                "the reference's output after {round} reset(s) differs from a virgin stream's for \
                 {name}; the oracle is authoritative, so this means the harness is misusing \
                 deflateReset rather than that the port is wrong\n{difference}",
                name = sample.fixture.name,
                difference = describe_byte_difference(&reference.output, &virgin_reference),
            );
            assert!(
                port.reset == Z_OK,
                "deflateReset must succeed on a live stream, got {status}",
                status = status(port.reset),
            );
        }

        let port_end = close::<Port>(&mut port_stream);
        let reference_end = close::<Reference>(&mut reference_stream);
        compare_status("deflateEnd", config, port_end, reference_end);
    }
}

/// The one-shot wrappers, at every level, on every fixture -- including the safe Rust surface.
///
/// Three implementations of the same operation are compared here rather than two, and the third is
/// the point: `zlib_rs::compress::compress2_z` is the idiomatic Rust entry point, reached without
/// going through the C ABI at all, and `crates/zlib-rs/src/compress.rs` names this file as the
/// owner of its byte-identity claim. If the facade agreed with the reference while the safe surface
/// did not, the divergence would be in the facade's adaptation rather than in the encoder -- and
/// only a three-way comparison distinguishes those.
///
/// `compress2` also pins a configuration no `deflateInit2_` cell reaches by the same route: it
/// supplies `Z_DEFLATED`, `MAX_WBITS`, `DEF_MEM_LEVEL` and `Z_DEFAULT_STRATEGY` itself
/// (`compress.c:42`), so a divergence in *those* defaults shows up here and nowhere else.
#[test]
fn one_shot_wrappers_are_byte_identical() {
    let corpus = load_corpus();

    for sample in &corpus {
        // `Z_DEFAULT_COMPRESSION` is included alongside 0..=9 because `deflate.c:436` maps it to 6,
        // and a caller that passes it must get exactly what a caller passing 6 gets.
        for level in [-1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
            let capacity = usize::try_from(Reference::compress_bounds(sample.bytes.len()).wide)
                .expect("a bound fits in a usize");

            let mut port_out = vec![0_u8; capacity];
            let (port_status, port_len) = Port::compress2(&mut port_out, &sample.bytes, level);

            let mut reference_out = vec![0_u8; capacity];
            let (reference_status, reference_len) =
                Reference::compress2(&mut reference_out, &sample.bytes, level);

            assert!(
                port_status == reference_status,
                "compress2 status differs for {name} at level {level}: port {port_status}, \
                 reference {reference_status}",
                name = sample.fixture.name,
                port_status = status(port_status),
                reference_status = status(reference_status),
            );
            assert!(
                port_status == Z_OK,
                "compress2 must succeed for {name} at level {level} with a compressBound-sized \
                 destination, got {status}",
                name = sample.fixture.name,
                status = status(port_status),
            );
            assert!(
                port_out[..port_len] == reference_out[..reference_len],
                "compress2 output differs for {name} at level {level}\n{difference}\n{TRIAGE}",
                name = sample.fixture.name,
                difference = describe_byte_difference(
                    &port_out[..port_len],
                    &reference_out[..reference_len],
                ),
            );

            // The safe core, over the same bytes and the same level.
            let mut safe_out = vec![0_u8; capacity];
            let safe = zlib_rs::compress::compress2_z(&mut safe_out, &sample.bytes, level);
            assert!(
                safe.code == zlib_rs::ReturnCode::OK,
                "the safe core's compress2_z must succeed for {name} at level {level}, \
                 got {code:?}",
                name = sample.fixture.name,
                code = safe.code,
            );
            assert!(
                safe_out[..safe.produced] == reference_out[..reference_len],
                "the safe core's compress2_z disagrees with the reference for {name} at level \
                 {level}, while the facade agrees or disagrees separately -- compare both findings \
                 before triaging the encoder\n{difference}\n{TRIAGE}",
                name = sample.fixture.name,
                difference = describe_byte_difference(
                    &safe_out[..safe.produced],
                    &reference_out[..reference_len],
                ),
            );

            // And the bound the caller would have used, in both widths.
            let port_bound = Port::compress_bounds(sample.bytes.len());
            let reference_bound = Reference::compress_bounds(sample.bytes.len());
            assert!(
                port_bound == reference_bound,
                "compressBound differs for {name}: port {port_bound:?}, \
                 reference {reference_bound:?}",
                name = sample.fixture.name,
            );
            assert!(
                port_len <= capacity,
                "compress2 wrote {port_len} bytes into a {capacity}-byte compressBound-sized \
                 destination for {name}: the bound is too small, which is a buffer overflow in \
                 caller code",
                name = sample.fixture.name,
            );
        }
    }
}

/// Looks a fixture up by its exact name.
///
/// Panics rather than returning an `Option`: the contract in `corpus/README.md` says a name that
/// does not match is a missing file, and a comparison that quietly did not happen is worse than a
/// loud failure.
fn sample<'corpus>(corpus: &'corpus [Sample], name: &str) -> &'corpus Sample {
    corpus
        .iter()
        .find(|sample| sample.fixture.name == name)
        .unwrap_or_else(|| panic!("corpus fixture {name} is not in the contract"))
}
