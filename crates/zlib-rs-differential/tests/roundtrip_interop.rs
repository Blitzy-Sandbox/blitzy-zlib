//! Bidirectional stream interoperability between the port and the C reference.
//!
//! This is the third of the three differential gates, and the broadest. `tests/table_equality.rs`
//! compares transcribed constants, `tests/byte_identical.rs` compares the *encoder's* output bytes,
//! and this file compares what happens when the two implementations have to talk to **each other**.
//!
//! # Interoperability is asserted, never inferred
//!
//! AAP 0.8.1 directive 4 requires bidirectional interoperability to be "asserted as a **distinct**
//! test class, not inferred from round-tripping", and that wording is doing real work. A
//! port-compresses-then-port-decompresses round trip proves nothing about interoperability: an
//! implementation that made two *matching* mistakes -- an encoder that emitted a malformed distance
//! and a decoder that accepted it -- passes such a test every time. Only a **crossing** detects it.
//!
//! So every assertion in this file crosses the implementation boundary. There is no same-side round
//! trip anywhere, and the four directions are named in the test function names so that a failing CI
//! line says which crossing broke before anyone opens the log:
//!
//! ```text
//! 1.  c_deflate_to_rust_inflate     the C reference compresses, the port decompresses
//! 2.  rust_deflate_to_c_inflate     the port compresses, the C reference decompresses
//! 3.  c_gzwrite_to_rust_gzread      the C reference writes a .gz file, the port reads it
//! 4.  rust_gzwrite_to_c_gzread      the port writes a .gz file, the C reference reads it
//! ```
//!
//! # How the crossing is arranged
//!
//! Both implementations are linked into this one test binary -- the port through the `libz_rs_sys`
//! facade, the reference through the `c_`-prefixed static archive this crate's `build.rs` produces
//! -- so a crossing is an in-process handoff of a byte buffer, or of a file for the `gzFile` layer.
//! Nothing is shelled out to and no two-run diff has to be trusted.
//!
//! The two `z_stream` types, and likewise the two `gz_header` types, are **distinct Rust types with
//! identical layout**: `src/oracle.rs` transcribes its own `#[repr(C)]` mirrors from the C headers
//! rather than reusing the facade's, so a mistake in the port's mirror cannot cancel itself out.
//! (Both shipped crates are ordinary `[dependencies]` of this harness rather than dev-dependencies,
//! which is what lets the per-side gates live in its `lib` target at all.) One value of each type is
//! constructed and **neither is ever transmuted into the other**. Every stream is
//! zeroed before use with `zalloc`, `zfree` and `opaque` left null, so each side exercises its own
//! internal default allocator -- the `memset`-then-`inflateInit` sequence `test/example.c` performs.
//!
//! One generic driver per operation is instantiated twice over the [`Side`] trait, so the chunk
//! boundaries, the buffer sizes and the flush schedule are decided by the *same* Rust code on both
//! sides. Any divergence is therefore attributable to the implementations rather than to the
//! harness.
//!
//! # What is compared
//!
//! For every crossing: the decompressed bytes must equal the original fixture **exactly**, and the
//! statuses of every call, `total_in`, `total_out` and the running check value must agree between
//! the two sides. Comparing only the bytes would miss a decoder that produced the right output while
//! disagreeing about how much input it consumed, which is the difference between a working library
//! and one that corrupts the *next* member of a concatenated stream.
//!
//! **Raw `DEFLATE` computes and verifies no check value at all** (`zlib.h` L878-L885), so `adler`
//! stays at its initial value for the raw container. That is asserted as the documented behaviour
//! rather than compared against a checksum that neither side computes -- see
//! [`Container::has_check_value`].
//!
//! # Where to look when something here fails
//!
//! A failure in this file is a **decoder** problem far more often than an encoder one, because the
//! encoder is already pinned byte-for-byte by `tests/byte_identical.rs`. The two triage lists are
//! therefore different, and this one is the right one to start from:
//!
//! * the 32-variant `inflate_mode` state machine (`inflate.h` L54-L94) and its resumability at every
//!   state -- suspect when a chunked crossing fails and the same case passes single-shot;
//! * `inflate_table` construction (`inftrees.c`) -- suspect when the failure is confined to
//!   particular fixtures, since a bad table decodes most symbols correctly and a few wrongly;
//! * the `inflate_fast` path (`inffast.c`) -- suspect when large fixtures fail and small ones pass,
//!   because `inflate.c` only enters it with 6 or more input and 258 or more output bytes available;
//! * `updatewindow` (`inflate.c` L252) and the sliding window -- suspect on
//!   `window_boundary.bin` specifically, which is built to cross the 32 KiB boundary twice;
//! * header parsing and the `extra`/`name`/`comment` clamps (`inflate.c` `HEAD`..`HCRC`) -- suspect
//!   when a gzip crossing fails and the equivalent zlib crossing passes.
//!
//! If the failing crossing is one where the **port compressed**, and the same fixture also fails in
//! `tests/byte_identical.rs`, the encoder is the cause and that file's eight-decision-point list is
//! the one to use; it is deliberately not duplicated here. [`DECODER_TRIAGE`] carries the list above
//! into the failure message, because a maintainer reading a CI log has none of these comments in
//! sight.
//!
//! # What this file deliberately does not do
//!
//! `test/infcover.c` is disproportionately valuable to this port for three reasons none of which can
//! be reproduced here: its allocator fills every block with `0xa5` rather than zero, which exposes
//! any code path that assumes zero-initialised memory; its `mem_done` detects leaks, non-LIFO frees
//! and rogue frees; and its `mem_limit` forces `Z_MEM_ERROR` at controlled points, reaching error
//! paths fuzzing finds only by chance. That harness is compiled **unmodified** and linked against
//! the Rust library elsewhere in the workspace, and
//! `crates/libz-rs-sys/tests/gz_memory.rs` carries the allocation accounting. This file's job is the
//! crossing, and re-implementing the allocator instrumentation here would duplicate coverage that
//! already exists without adding a single new fact.
//!
//! # Coverage, and how to run it
//!
//! ```text
//! # The default run. Always runs; the `differential` CI job runs it first.
//! cargo test --locked -p zlib-rs-differential --release --test roundtrip_interop
//!
//! # The exhaustive window-bits sweep. The `differential` job in .github/workflows/rust.yml runs
//! # this line as well as the one above, so both halves are gated on every push.
//! ZLIB_RS_INTEROP_EXHAUSTIVE=1 cargo test --locked -p zlib-rs-differential --release \
//!     --test roundtrip_interop -- --ignored --test-threads 4
//! ```
//!
//! The variable and `--ignored` are **both** required, following the convention
//! `tests/byte_identical.rs` establishes: `#[ignore]` keeps the sweep out of a default run and the
//! variable is what makes it do its work rather than report that it was not asked to.
//!
//! Both lines are additionally worth running with `--features simd`, which forwards to the vectorised
//! Adler-32 and CRC-32 backends of both dependencies. AAP 0.8.2 ambiguity 9 confines SIMD to the two
//! checksums precisely because a checksum yields one scalar however it is computed, so it cannot
//! perturb the bitstream -- and this suite is one of the two places that claim is *measured* rather
//! than assumed, since it compares the check value on every crossing as well as the bytes:
//!
//! ```text
//! cargo test --locked -p zlib-rs-differential --release --features simd --test roundtrip_interop
//! ```
//!
//! # Contract
//!
//! * **A mismatch is a defect in the port, never in the test.** Do not weaken an assertion, accept
//!   "close enough" output, narrow the matrix or `#[ignore]` a failing direction.
//! * **No case may pass vacuously.** A crossing where both sides fail and produce nothing "agrees"
//!   trivially, so every success case asserts the specific expected `Z_OK` / `Z_STREAM_END`, and
//!   every negative case asserts **equal, specific** status codes on both sides rather than merely
//!   equal ones.
//! * **No `unsafe` at all, enforced by `#![forbid(unsafe_code)]` on this file's root.** Invoking an
//!   `extern "C"` entry point is an `unsafe` operation whichever implementation owns it, so both
//!   sides are reached through this crate's FFI boundary instead: `crate::port` for the facade and
//!   `oracle` for the C reference, one gate per entry point, each gate owning the single documented
//!   `unsafe` block and the `// SAFETY:` comment naming its invariant. The [`Side`] and [`GzSide`]
//!   implementations below therefore carry call shapes and the obligations a signature cannot
//!   express -- the retained `gz_header` pointer, the `*_max` buffer caps, exactly one `gzclose` --
//!   and nothing more. No `#[no_mangle]` anywhere, and never `extern "C-unwind"`.
//! * **No network, and nothing outside `corpus/minimal/`.** Fixtures are resolved from
//!   `CARGO_MANIFEST_DIR`. The Silesia tier is deliberately not read: it is opt-in and belongs to
//!   `benches/`.
//! * **Every scratch file lives in a unique directory and is removed, including on a failing run.**
//!   See [`Scratch`], whose `Drop` runs while a panicking test unwinds.
//! * **Run `tests/table_equality.rs` and `tests/byte_identical.rs` first.** A mistyped digit in a
//!   transcribed table or a divergent encoder would surface here as a wall of failures pointing at
//!   neither.
//! * **Outside Miri, inside AddressSanitizer.** Miri interprets Rust MIR and cannot execute the
//!   compiled C oracle at all, so the Miri gate is scoped to `-p zlib-rs` and this suite can never
//!   run under it. The nightly `AddressSanitizer` gate DOES run it, as part of
//!   `cargo +nightly test -p zlib-rs-differential --target x86_64-unknown-linux-gnu` with
//!   `RUSTFLAGS=-Zsanitizer=address` and the oracle's C compiled `-fsanitize=address` -- so every
//!   crossing below, including the `gzFile` ones that write real files, is exercised with both
//!   sides instrumented. `detect_leaks` is off for that step alone, because the C oracle keeps
//!   `local` tables alive by design; every other check is in force. The facade's own suites are
//!   instrumented in the same job (`-p libz-rs-sys` covers everything under
//!   `crates/libz-rs-sys/tests/`, including the `gz_memory.rs` accounting named above), so the
//!   `gzFile` and `inflateGetHeader` surface is reached from both directions rather than only from
//!   here.

// The workspace lint table denies the panic-prone quartet, which is right for library code and
// wrong for a test: a test asserts, a failed assertion panics, and slicing a buffer at an offset
// this file just computed from that same buffer's length is clearer than defensively matching on it.
// `clippy.toml` grants `unwrap`/`expect`/`panic` inside `#[test]` context, but there is no
// `allow-indexing-slicing-in-tests` option and most of the work here happens in file-scope helpers
// rather than in `#[test]` functions, so the relaxation is stated once here. Same quartet, same
// reason, as `tests/byte_identical.rs` and `tests/table_equality.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Both C ABIs this suite crosses are reached exclusively through the gates in `crate::port` and
// `oracle`, so nothing here needs `unsafe` and the compiler is asked to keep it that way. Those two
// boundary modules are the only files in this crate the attribute is deliberately absent from;
// `src/lib.rs` explains why at its ATTRIBUTES block.
#![forbid(unsafe_code)]

use core::ffi::{c_int, c_uint};
use std::ffi::{CStr, CString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

// The facade -- the artifact whose interoperability is the acceptance criterion. The spelling is
// `libz_rs_sys`, never `z`: `crates/libz-rs-sys` sets `[lib] name = "z"` so that cargo emits
// `libz.so`/`libz.a`, and this crate's manifest uses cargo's dependency-rename form to give the Rust
// path a useful name. That manifest is the authority; see its comment at the `libz_rs_sys` key.
use libz_rs_sys::{
    Z_BEST_COMPRESSION, Z_BUF_ERROR, Z_DATA_ERROR, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY,
    Z_DEFLATED, Z_ERRNO, Z_FINISH, Z_FULL_FLUSH, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK,
    Z_STREAM_END, Z_STREAM_ERROR, Z_SYNC_FLUSH, Z_VERSION_ERROR,
};

// The two sides of this crate's FFI boundary. Every `extern "C"` declaration and every `unsafe` call
// this suite performs lives in exactly one of these two modules -- `oracle` for the C reference,
// `port` for the facade -- and this file adds no `extern "C"` block and no `unsafe` block of its own.
use zlib_rs_differential::retain::Session;
use zlib_rs_differential::{oracle, port};

use crate::Length::{AtLeast, Exactly};

// =================================================================================================
//  Failure triage -- what a DECODER divergence implicates
//
//  Deliberately different from `tests/byte_identical.rs`'s list, which covers the encoder. Ordered
//  so that the symptom a reader already has in hand selects the entry.
// =================================================================================================

/// The decoder triage list, appended to every crossing failure report.
const DECODER_TRIAGE: &str = "\
where to look -- a crossing failure is usually a DECODER problem, because the encoder is already
pinned byte-for-byte by tests/byte_identical.rs (use that file's eight-decision-point list instead
if the port was the compressing side AND the same fixture fails there too):
  1. inflate_mode state machine  inflate.h:54-94, 32 variants HEAD..SYNC, driven by the exhaustive
                                 match in inflate.c.  Every state must be resumable, because that is
                                 the contract inflate() offers a caller feeding input in pieces ->
                                 suspect FIRST when a chunked crossing fails and the same case
                                 passes single-shot.
  2. inflate_table               inftrees.c:32, the ENOUGH_LENS/ENOUGH_DISTS bounds at
                                 inftrees.h:47-58, and the incomplete-code handling.  A wrong table
                                 decodes most symbols correctly and a few wrongly -> suspect when the
                                 failure is confined to particular fixtures rather than spread over
                                 all of them.
  3. inflate_fast                inffast.c:47, entered only when inflate.c has >= 6 input and >= 258
                                 output bytes available -> suspect when large fixtures fail and small
                                 ones pass, or when it fails only with a generous output window.
  4. updatewindow / window slide inflate.c:252 and the whave/wnext bookkeeping.  window_boundary.bin
                                 is built to cross the 32 KiB boundary twice on purpose -> suspect
                                 when that fixture alone fails.
  5. header parse and clamps     the HEAD..HCRC states, and the extra_max/name_max/comm_max clamps
                                 zlib.h:118-133 requires -> suspect when a gzip crossing fails while
                                 the equivalent zlib crossing passes, or when only the gz_header
                                 fields differ.
  6. check value                 Adler-32 for the zlib container, CRC-32 for gzip, and NONE for raw
                                 (zlib.h:878-885) -> suspect when the bytes match and only `adler`
                                 differs; that is the checksum being fed differently, not a decode
                                 error.";

// =================================================================================================
//  What each side's gates still owe
//
//  ONE GATE PER ENTRY POINT, and NO `unsafe` ANYWHERE IN THIS FILE: the root carries
//  `#![forbid(unsafe_code)]` and every method of [`Side`] and [`GzSide`] delegates to the matching
//  safe gate in this crate's FFI boundary -- `crate::port` for the facade, `oracle` for the C
//  reference -- which is where the single documented `unsafe` block per entry point lives together
//  with the invariant it discharges. Four of the seven obligations below are therefore discharged by
//  the gates' signatures and are recorded here only so that a reader of this file knows where they
//  went; three of them are genuinely still this side's, because no signature can express them.
//
//    (a) DISCHARGED BY THE GATE. `strm` arrives as `&mut Session<'_, Self::Stream>` and is passed straight
//        through, so it cannot be null, misaligned or aliased.
//    (b) THIS SIDE'S. `strm.state` must be null or a block THIS SAME implementation allocated
//        through its own `*Init2_`, so the state check on entry -- `inflateStateCheck` at
//        `inflate.c:88`, `deflateStateCheck` at `deflate.c:538`, and their counterparts in the
//        facade -- accepts or rejects it exactly as it would for a C caller. The two mirrors being
//        distinct Rust types is what makes handing a stream to the other side's gate a compile
//        error, so what remains here is only the discipline of not reusing an ended stream.
//    (c) DISCHARGED BY THE GATE, arranged here. The input window crosses as a `&[u8]`, but which
//        slice, and whether a zero-length window is installed as a NULL pointer -- the pairing
//        `zlib.h:91-92` permits explicitly -- is a chunking decision, so `install_port` and
//        `install_reference` own it.
//    (d) THIS SIDE'S, and semantic rather than sound: the driver never offers a zero-length output
//        window, because `inflate.c` and `deflate.c:995` both answer `Z_BUF_ERROR` for one and a run
//        that tripped that would be measuring the harness. Both installers assert it.
//    (e) DISCHARGED BY THE GATE. Each boundary gate supplies its own side's `version`/`stream_size`
//        pair -- the facade's `ZLIB_VERSION` and `size_of` of the facade's `z_stream`, the oracle's
//        `c_zlibVersion` and `size_of` of the oracle's mirror -- so crossing them is impossible
//        rather than merely avoided. Crossed, they would answer `Z_VERSION_ERROR` (`zlib.h:248`).
//    (f) THIS SIDE'S. A `gz_header` handed to `deflateSetHeader` or `inflateGetHeader` is a live
//        local of this side's own mirror type, and every non-null `extra`/`name`/`comment` pointer
//        in it must address at least `extra_max`/`name_max`/`comm_max` writable bytes in a buffer
//        that outlives the whole stream, because the library RETAINS the pointer to the header.
//        `Vec`'s heap allocation does not move when the `Vec` itself does, so a pointer taken from
//        one stays valid for as long as the `Vec` is neither dropped nor grown -- and neither
//        happens to any buffer here. [`HeaderBuffers`] is what sizes them.
//    (g) MOSTLY DISCHARGED BY THE GATE, one half this side's. Paths and modes cross as `&CStr`, so
//        termination is the type's business, and the library copies what it keeps
//        (`gzlib.c:200-204`). What stays here is that every `gzopen`/`gzdopen` is matched by exactly
//        one `gzclose` on every exit path and that a failed open is asserted before anything else
//        touches the handle: the boundary's handle is `Copy`, deliberately, so that this suite can
//        measure what a double close and a use-after-close actually report.
//
//  A `z_stream` must additionally NOT BE RELOCATED once `*Init2_` has seen it: `deflate.c:444` and
//  `inflate.c:236` store a back-pointer to it, and the state check tests that pointer on the way
//  into every subsequent entry point. Move the struct and every later call answers
//  `Z_STREAM_ERROR` -- identically on both sides, so a comparison of two empty outputs would pass
//  while measuring nothing at all. Every stream in this file is therefore `Box`ed, exactly as
//  `tests/byte_identical.rs` boxes its own for the same reason.
// =================================================================================================

// =================================================================================================
//  The committed corpus
// =================================================================================================

/// What `corpus/README.md` pins about a fixture's length.
///
/// Four fixtures have an exact size that is load-bearing -- 0, 1, 14 and 6 -- and one has a floor,
/// `window_boundary.bin`'s "> 32768 bytes". For the rest the README pins the content class and says
/// outright that the exact length is not pinned, so asserting one here would invent a contract the
/// corpus does not offer.
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
}

/// The whole of tier 1, in the order `corpus/README.md` tabulates it. There are no others.
///
/// `window_boundary.bin` is the most valuable fixture in this file specifically: it repeats a pinned
/// 32,768-byte block at a distance of exactly 32,768 and then repeats the head again, so a decoder
/// has to get `updatewindow`, `whave` and `wnext` right across one *and* two 32 KiB boundaries. A
/// decoder whose window bookkeeping is off by one byte reproduces every other fixture perfectly and
/// fails this one.
//
// `#[rustfmt::skip]` keeps this a table, matching `tests/byte_identical.rs`'s own fixture table so
// that the two can be read against each other and against `corpus/README.md`. `.rustfmt.toml`
// records this as the sanctioned escape hatch, and every row is inside the 100-column limit.
#[rustfmt::skip]
const FIXTURES: &[Fixture] = &[
    Fixture { name: "empty.bin",           len: Exactly(0)      },
    Fixture { name: "single_byte.bin",     len: Exactly(1)      },
    Fixture { name: "repetitive.bin",      len: AtLeast(1)      },
    Fixture { name: "random.bin",          len: AtLeast(1)      },
    Fixture { name: "text.txt",            len: AtLeast(1)      },
    Fixture { name: "binary.bin",          len: AtLeast(1)      },
    Fixture { name: "gray_list.bin",       len: AtLeast(1)      },
    Fixture { name: "window_boundary.bin", len: AtLeast(32_769) },
    Fixture { name: "hello.bin",           len: Exactly(14)     },
    Fixture { name: "dictionary.bin",      len: Exactly(6)      },
];

/// The fixture that is also `test/example.c`'s payload (`test/example.c` L35 plus its NUL).
const HELLO_FIXTURE: &str = "hello.bin";

/// The fixture that is also `test/example.c`'s preset dictionary (`test/example.c` L40 plus its NUL).
const DICTIONARY_FIXTURE: &str = "dictionary.bin";

/// The fixture built to cross the 32 KiB window boundary twice.
const WINDOW_BOUNDARY_FIXTURE: &str = "window_boundary.bin";

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
/// Never an absolute path baked into the source, and never derived from the current directory, which
/// cargo does not guarantee for a test binary.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join("minimal")
}

/// Loads every fixture and checks it against the pinned contract.
///
/// A missing file, or one whose length has drifted, fails here rather than producing a crossing over
/// the wrong bytes. `corpus/README.md` states that renaming, moving or removing a fixture is a
/// breaking change to be made in the same commit as the change to every consumer; this is the
/// consumer-side half of that.
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

/// The one fixture named `name`, or a panic naming what was asked for.
fn sample<'a>(corpus: &'a [Sample], name: &str) -> &'a Sample {
    corpus
        .iter()
        .find(|candidate| candidate.fixture.name == name)
        .unwrap_or_else(|| panic!("corpus fixture {name} is not in FIXTURES"))
}

// =================================================================================================
//  The three container formats
// =================================================================================================

/// Which wrapper the compressed stream carries, and therefore which normative text governs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    /// RFC 1950: a 2-byte header and an Adler-32 trailer. `windowBits` 9..=15.
    Zlib,
    /// RFC 1951: no header and no trailer, and **no check value computed or verified**.
    /// `windowBits` -9..=-15.
    Raw,
    /// RFC 1952: a 10-or-more-byte header and a CRC-32 trailer. `windowBits` 25..=31.
    Gzip,
}

impl Container {
    /// How this container is named in a failure message, with the RFC that defines it.
    const fn name(self) -> &'static str {
        match self {
            Self::Zlib => "zlib/RFC1950",
            Self::Raw => "raw/RFC1951",
            Self::Gzip => "gzip/RFC1952",
        }
    }

    /// Whether the wrapper carries a check value the stream's `adler` member tracks.
    ///
    /// False for exactly one container, and the consequence is load-bearing: raw `DEFLATE` neither
    /// computes nor verifies a check value (`zlib.h` L878-L885), so `adler` keeps the value
    /// `deflateReset`/`inflateReset` left in it. Asserting a checksum for the raw case would be
    /// asserting something neither implementation is required to produce.
    const fn has_check_value(self) -> bool {
        match self {
            Self::Zlib | Self::Gzip => true,
            Self::Raw => false,
        }
    }

    /// The `deflateInit2_` `windowBits` for this container at window size `bits`.
    ///
    /// `bits` is the base-two logarithm of the window and is always in 9..=15: `deflateInit2_`
    /// silently promotes 8 to 9 for the zlib container and refuses it outright for the other two
    /// (`zlib.h` L543-L600), which is why 8 is exercised only by the guard-rail parity test.
    fn deflate_window_bits(self, bits: c_int) -> c_int {
        assert!(
            (9..=15).contains(&bits),
            "a window size outside 9..=15 is a harness error, not a case: got {bits}"
        );
        match self {
            Self::Zlib => bits,
            Self::Raw => -bits,
            Self::Gzip => bits + 16,
        }
    }

    /// The matching `inflateInit2_` `windowBits`, for a decoder whose window is exactly as large as
    /// the encoder's.
    ///
    /// The inflate side must be **greater than or equal to** the deflate side or `inflate` answers
    /// `Z_DATA_ERROR` rather than growing its window (`zlib.h` L865-L873). Every interoperability
    /// case pairs the two deliberately through this function; the deliberately-too-small pairing is
    /// its own status-parity case.
    fn inflate_window_bits(self, bits: c_int) -> c_int {
        self.deflate_window_bits(bits)
    }
}

/// Every window size both `deflateInit2_` and `inflateInit2_` accept for all three containers.
const WINDOW_SIZES: &[c_int] = &[9, 10, 11, 12, 13, 14, 15];

/// The window size a case uses when the window itself is not what is under test.
const DEFAULT_WINDOW_SIZE: c_int = 15;

/// The three containers, in the order the RFCs are numbered.
const CONTAINERS: &[Container] = &[Container::Zlib, Container::Raw, Container::Gzip];

/// Every compression level `deflateInit2_` accepts, which is all ten.
const LEVELS: &[c_int] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// The level a case uses when the level itself is not what is under test.
const DEFAULT_LEVEL: c_int = 6;

/// `DEF_MEM_LEVEL` -- what `compress2` and `gz_init` pass (`zutil.h` L61, `gzwrite.c` L36).
const DEFAULT_MEM_LEVEL: c_int = 8;

/// `inflateInit2_`'s "read the window size out of the zlib header" request (`zlib.h` L875-L876).
const WINDOW_BITS_FROM_HEADER: c_int = 0;

/// The `+32` addend that enables automatic zlib-or-gzip header detection (`zlib.h` L889-L891).
const AUTODETECT_ADDEND: c_int = 32;

/// The `+16` addend that decodes **gzip only**, so that a zlib stream answers `Z_DATA_ERROR`
/// (`zlib.h` L889-L893).
const GZIP_ONLY_ADDEND: c_int = 16;

// =================================================================================================
//  Which side did what
// =================================================================================================

/// One crossing: which implementation compressed and which decompressed.
///
/// There is no same-side variant, and that is the point of the type existing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// The C reference compressed; the port decompressed.
    CToRust,
    /// The port compressed; the C reference decompressed.
    RustToC,
}

impl Direction {
    /// How this direction reads in a failure message.
    const fn name(self) -> &'static str {
        match self {
            Self::CToRust => "C reference compressed -> port decompressed",
            Self::RustToC => "port compressed -> C reference decompressed",
        }
    }
}

/// Both directions. Every interoperability test iterates this rather than hard-coding one.
const DIRECTIONS: &[Direction] = &[Direction::CToRust, Direction::RustToC];

/// How the driver feeds input to, and takes output from, a stream.
///
/// Chunk boundaries interact with the resumability of every `inflate_mode` state, which is why a
/// crossing that passes single-shot and fails at one byte at a time points straight at entry 1 of
/// [`DECODER_TRIAGE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chunking {
    /// How this chunking is named in a failure message.
    name: &'static str,
    /// Bytes offered per call, or `None` for the whole buffer at once.
    input: Option<usize>,
    /// Bytes of output window offered per call, or `None` for one generously sized window.
    output: Option<usize>,
}

/// The whole buffer in, one large window out. The cheapest case, and the one a caller writes first.
const SINGLE_SHOT: Chunking = Chunking {
    name: "single-shot",
    input: None,
    output: None,
};

/// The chunkings every crossing is measured at.
///
/// `one-byte-both` is the pathological one and it is the most valuable: it forces a state transition
/// to be resumed at essentially every point in the stream, which is exactly the property
/// `inflate()`'s contract promises and the one a state machine written as a straight-line loop
/// silently lacks.
//
// `#[rustfmt::skip]` for the same table-readability reason as `FIXTURES`.
#[rustfmt::skip]
const CHUNKINGS: &[Chunking] = &[
    SINGLE_SHOT,
    Chunking { name: "16k-in/16k-out",   input: Some(16_384), output: Some(16_384) },
    Chunking { name: "7-in/13-out",      input: Some(7),      output: Some(13)     },
    Chunking { name: "one-byte-both",    input: Some(1),      output: Some(1)      },
];

/// The chunkings a large fixture is measured at.
///
/// `one-byte-both` over `window_boundary.bin` is 66,560 calls per side and buys nothing the small
/// fixtures do not already buy, so it is reserved for the exhaustive sweep. Nothing else is held
/// back anywhere in this file.
//
// `#[rustfmt::skip]` for the same table-readability reason as `FIXTURES`.
#[rustfmt::skip]
const CHUNKINGS_LARGE: &[Chunking] = &[
    SINGLE_SHOT,
    Chunking { name: "16k-in/16k-out",   input: Some(16_384), output: Some(16_384) },
    Chunking { name: "997-in/1021-out",  input: Some(997),    output: Some(1_021)  },
];

/// Above this many bytes a fixture is driven with [`CHUNKINGS_LARGE`] rather than [`CHUNKINGS`].
const LARGE_FIXTURE_BYTES: usize = 32 * 1024;

/// The chunkings appropriate to a fixture of `len` bytes.
fn chunkings_for(len: usize) -> &'static [Chunking] {
    if len > LARGE_FIXTURE_BYTES {
        CHUNKINGS_LARGE
    } else {
        CHUNKINGS
    }
}

// =================================================================================================
//  Integer plumbing
//
//  Everything crossing the boundary is compared as a `u64` so that one comparison works on both
//  integer models. `uLong` is `unsigned long`: 64 bits on LP64 and 32 on LLP64 Windows, which is
//  precisely why neither side may be typed as a fixed-width integer and why the widening below is
//  target-dependent rather than decorative.
// =================================================================================================

/// Widens the facade's `uLong` to the `u64` this file compares in.
//
// `clippy::useless_conversion` fires on LP64, where `uLong` already *is* `u64`. Removing it would
// break the LLP64 build, where the widening is real. Written as a comment rather than the
// attribute's `reason` field because lint reasons were stabilised in Rust 1.81 and the workspace
// declares `rust-version = "1.80"`. Same relaxation, same reason, as `tests/byte_identical.rs`.
#[allow(clippy::useless_conversion)]
fn widen_ulong(value: libz_rs_sys::uLong) -> u64 {
    u64::from(value)
}

/// Widens the oracle's `uLong`, which is the same `c_ulong` reached through the other crate's alias.
//
// Relaxed for the reason given on [`widen_ulong`].
#[allow(clippy::useless_conversion)]
fn widen_ulong_oracle(value: oracle::uLong) -> u64 {
    u64::from(value)
}

/// Narrows a window length to the facade's `uInt`, which is what `avail_in`/`avail_out` are.
fn narrow_avail(len: usize) -> libz_rs_sys::uInt {
    libz_rs_sys::uInt::try_from(len).expect("a window length fits in a uInt")
}

/// Narrows a window length to the oracle's `uInt`.
fn narrow_avail_oracle(len: usize) -> oracle::uInt {
    oracle::uInt::try_from(len).expect("a window length fits in a uInt")
}

/// Narrows a length to the facade's `uLong`, which is what the one-shot wrappers take.
fn narrow_ulong(len: usize) -> libz_rs_sys::uLong {
    libz_rs_sys::uLong::try_from(len).expect("a fixture length fits in a uLong")
}

/// Narrows a length to the oracle's `uLong`.
fn narrow_ulong_oracle(len: usize) -> oracle::uLong {
    oracle::uLong::try_from(len).expect("a fixture length fits in a uLong")
}

/// Widens a `uInt` to the `usize` the driver counts in.
fn widen_avail(value: libz_rs_sys::uInt) -> usize {
    usize::try_from(value).expect("a uInt fits in a usize")
}

/// Widens the oracle's `uInt` to a `usize`.
fn widen_avail_oracle(value: oracle::uInt) -> usize {
    usize::try_from(value).expect("a uInt fits in a usize")
}

/// Widens the facade's `z_off_t`, which is `long`, to the `i64` this file compares offsets in.
//
// Relaxed for the reason given on [`widen_ulong`]: the conversion is the identity on LP64, where
// `z_off_t` already is `i64`, and a real widening on a 32-bit target.
#[allow(clippy::useless_conversion)]
fn widen_off(offset: libz_rs_sys::z_off_t) -> i64 {
    i64::from(offset)
}

/// Widens the oracle's `z_off_t`. Same alias, reached through the other crate.
//
// Relaxed for the reason given on [`widen_off`].
#[allow(clippy::useless_conversion)]
fn widen_off_oracle(offset: oracle::z_off_t) -> i64 {
    i64::from(offset)
}

/// Narrows a length to the `int` that `gzwrite` and `gzputs` answer with (`zlib.h` L1508, L1577).
///
/// The gates take and return the C types, so this is what an assertion needs to compare a reported
/// count against the length that produced it.
fn narrow_int(len: usize) -> c_int {
    c_int::try_from(len).expect("a written length fits in a C int")
}

/// Narrows an offset to the facade's `z_off_t`, which is `long`.
///
/// Checked rather than cast because `z_off_t` is 32 bits on some targets and this file's offsets are
/// `i64`: a silent truncation would turn a seek into a different seek.
fn narrow_off(offset: i64) -> libz_rs_sys::z_off_t {
    libz_rs_sys::z_off_t::try_from(offset).expect("an offset fits in a z_off_t")
}

/// Narrows an offset to the oracle's `z_off_t`.
fn narrow_off_oracle(offset: i64) -> oracle::z_off_t {
    oracle::z_off_t::try_from(offset).expect("an offset fits in a z_off_t")
}

/// The value `gzerror`'s out-parameter is seeded with.
///
/// A recognisable non-zero seed rather than 0, so that "the library wrote `Z_OK`" and "the library
/// wrote nothing" are distinguishable: `gzlib.c` L517-L518 returns `NULL` without writing `*errnum`
/// for a null handle, and a divergence in whether a side writes at all is worth seeing.
const GZ_ERRNUM_SENTINEL: c_int = 0x5eed;

/// Turns `gzerror`'s return into a comparable `String`.
///
/// A `NULL` return is a distinct outcome from an empty message -- `gzlib.c` L517 answers `NULL` only
/// for a null handle, whereas L527 answers `""` for a live one with no error -- so the two are
/// rendered differently rather than both becoming the empty string. The boundary gate keeps them
/// apart by handing back [`None`] for the null return, which is what makes the distinction available
/// here at all: a side that answered `""` where the other answered `NULL` would otherwise compare
/// equal.
fn gz_message(message: Option<&[u8]>) -> String {
    match message {
        None => "<NULL>".to_owned(),
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

// =================================================================================================
//  Scratch files
//
//  The `gzFile` layer is the one part of this suite that needs the filesystem, because a file is the
//  medium the crossing goes through. Three rules, all of them for the same reason -- two copies of
//  this binary may be running at once, and cargo runs separate `#[test]` functions in parallel
//  threads within one:
//
//    * the directory name carries the process id AND a per-process serial, so no two `Scratch`
//      values anywhere can name the same path;
//    * nothing is ever written into the source tree, and no fixed path such as `/tmp/foo.gz` is used;
//    * `Drop` removes the whole directory, so a failing test cleans up while it unwinds.
//
//  Any group of gz cases that shares a path or a mutable fixture lives inside ONE `#[test]` in a
//  deterministic order, which is the discipline `crates/libz-rs-sys/tests/c_api_parity.rs` adopts
//  for its ten order-dependent stages and for exactly this reason.
// =================================================================================================

/// A uniquely named scratch directory, removed when the guard is dropped.
#[derive(Debug)]
struct Scratch {
    /// The directory itself. Created by [`Scratch::new`] and removed by `Drop`.
    dir: PathBuf,
}

impl Scratch {
    /// How many unguessable names to try before giving up.
    ///
    /// A collision means another process holds that exact 128-bit name, which is vanishingly
    /// unlikely; the retries keep one unlucky draw from failing the suite, and the bound keeps a
    /// temporary directory that refuses *every* create from spinning.
    const ATTEMPTS: usize = 16;

    /// Creates one private, unguessable, owner-only directory for this instantiation.
    ///
    /// ★ THE NAME IS NOT DERIVED FROM THE PROCESS ID, AND NOTHING IS REMOVED HERE. The earlier
    /// form was `<temp dir>/zlib-rs-interop-<pid>-<serial>-<tag>` preceded by an unconditional
    /// `remove_dir_all`, and both halves were wrong in a shared temporary directory. A process id
    /// is neither secret nor unpredictable -- it is visible in `/proc`, drawn from a small space
    /// and reused -- so another user can precreate that path, or plant a symlink at it, and the
    /// `gzopen(.., "wb")` calls below (an `open()` with `O_CREAT` and no `O_EXCL`) then follow it
    /// and write wherever it points. The `remove_dir_all` compounded it by deleting whatever stood
    /// there, which for a symlinked or adopted directory means deleting a path this suite does not
    /// own, and by leaving a window between the delete and the create.
    ///
    /// So the *directory* carries the security property, exactly as
    /// `crates/libz-rs-sys/tests/c_api_parity.rs` does it:
    ///
    /// * **Unguessable** -- 128 bits from two OS-seeded [`RandomState`] draws.
    /// * **Owner-only in the same syscall that creates it** -- `mode(0o700)` on the
    ///   [`DirBuilder`], not a create followed by a `chmod` that leaves a window open.
    /// * **Exclusively** -- `create` is the non-recursive form, so an existing path is refused
    ///   with `AlreadyExists` rather than adopted, and this function never removes anything. The
    ///   only `remove_dir_all` in this file is the RAII one in `Drop`, on a directory this process
    ///   created empty.
    ///
    /// `tag` is kept, but only as a suffix for legibility in a failure message; it contributes no
    /// uniqueness and nothing depends on it.
    fn new(tag: &str) -> Self {
        use std::hash::{BuildHasher, RandomState};
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;

        let base = std::env::temp_dir();
        for _ in 0..Self::ATTEMPTS {
            // Two independent OS-seeded draws, so the name carries 128 bits rather than 64.
            let high = u128::from(RandomState::new().hash_one(0_u64));
            let low = u128::from(RandomState::new().hash_one(u64::MAX));
            let dir = base.join(format!(
                "zlib-rs-interop-{name:032x}-{tag}",
                name = (high << 64) | low
            ));

            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            builder.mode(0o700);

            match builder.create(&dir) {
                Ok(()) => return Self { dir },
                // Someone holds that name. Draw another; nothing is removed.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!(
                    "could not create a private scratch directory under {base}: {error}",
                    base = base.display()
                ),
            }
        }
        panic!(
            "{attempts} unguessable names under {base} were all taken",
            attempts = Self::ATTEMPTS,
            base = base.display()
        )
    }

    /// A path inside this scratch directory.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The same path as a `CString`, which is what `gzopen` takes.
    ///
    /// Discharges the first half of obligation (g): the result is NUL-terminated and, being owned by
    /// the caller, valid for reads for as long as the caller keeps it.
    fn c_path(&self, name: &str) -> CString {
        let path = self.path(name);
        let text = path.to_str().expect("a scratch path is valid UTF-8");
        assert!(
            text.len() < MAX_NAME_LEN,
            "the scratch path is {len} bytes and test/minigzip.c's MAX_NAME_LEN is \
             {MAX_NAME_LEN}, so the relinked C driver could not open it: {text}",
            len = text.len(),
        );
        CString::new(text).expect("a scratch path contains no interior NUL")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best-effort on purpose: a failing test is already reporting something more interesting
        // than a cleanup error, and turning a removal failure into a second panic during unwinding
        // would abort the process and lose the first one.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// =================================================================================================
//  What a call and a run report
// =================================================================================================

/// One `deflate` or `inflate` call, recorded so that the *sequence* of statuses is comparable and
/// not merely the last one.
///
/// A `Z_BUF_ERROR` in the middle of an otherwise successful run means the two implementations
/// disagree about when progress is possible, even if both eventually produce the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Call {
    /// What was returned.
    status: c_int,
    /// How many bytes were written into the output window.
    produced: usize,
    /// How many bytes of the input window this one call consumed.
    ///
    /// Recorded rather than derived, because it is the quantity `zlib.h`'s accounting quirks turn on:
    /// `inflate` leaves the bytes consumed by the call that answers `Z_NEED_DICT` out of `total_in`,
    /// and how many that is depends entirely on how the caller chunked its input.
    consumed: usize,
    /// `avail_in` as the call left it.
    avail_in_after: usize,
}

/// The accounting members of a `z_stream`, read straight off the struct after a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Snapshot {
    /// `total_in`.
    total_in: u64,
    /// `total_out`.
    total_out: u64,
    /// `adler` -- an Adler-32 under the zlib wrapper, a CRC-32 under gzip, and untouched for raw.
    adler: u64,
    /// `data_type`.
    data_type: c_int,
}

/// Everything one side's run of a stream reports.
#[derive(Clone, Debug)]
struct Run {
    /// Every call, in order.
    calls: Vec<Call>,
    /// The concatenated output bytes.
    output: Vec<u8>,
    /// The accounting members afterwards.
    snapshot: Snapshot,
    /// What the terminating call returned. Separated out because it is what most assertions name.
    final_status: c_int,
    /// What `deflateEnd` or `inflateEnd` returned.
    end: c_int,
}

/// A one-shot wrapper call: the status plus **both** in/out parameters.
///
/// `uncompress2` reports bytes actually consumed through `*sourceLen` as well as bytes produced
/// through `*destLen` (`uncompr.c` L86-L120), and both are part of the contract. Comparing only the
/// bytes would miss a wrapper that decompressed correctly while lying about how much of its input it
/// used -- which breaks every caller that walks a concatenated stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OneShot {
    /// The returned status.
    status: c_int,
    /// `*destLen` as the call left it: bytes written.
    dest_len: u64,
    /// `*sourceLen` as the call left it: bytes consumed. Unchanged by `compress2`, which takes its
    /// source length by value.
    source_len: u64,
}

/// The scalar members of a `gz_header`, normalised so the two mirrors are comparable.
///
/// The buffer *contents* are deliberately not here: they live in the caller's [`HeaderBuffers`], and
/// keeping them there is what makes the untouched tail of each buffer observable as well as the part
/// that was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeaderReport {
    /// `text`.
    text: c_int,
    /// `time`.
    time: u64,
    /// `xflags`.
    xflags: c_int,
    /// `os`.
    os: c_int,
    /// `extra_len` -- the field's **true** length once `done` is set, not the truncated one.
    extra_len: u64,
    /// `extra_max`, so that a side that rewrote its own cap is caught.
    extra_max: u64,
    /// `name_max`.
    name_max: u64,
    /// `comm_max`.
    comm_max: u64,
    /// `hcrc`.
    hcrc: c_int,
    /// `done`: 0 while the header is incomplete, 1 when finished, -1 for a zlib stream.
    done: c_int,
    /// Whether `extra` came back non-null, which `zlib.h` L1094-L1096 makes meaningful: an absent
    /// field has its pointer set to `Z_NULL`.
    extra_present: bool,
    /// Whether `name` came back non-null.
    name_present: bool,
    /// Whether `comment` came back non-null.
    comment_present: bool,
}

/// What a gzip header is asked to carry on the way out.
///
/// The three byte fields are owned here so that the pointers a `gz_header` derives from them stay
/// valid for the whole stream -- obligation (f). `name` and `comment` include their terminating NUL
/// because `deflateSetHeader` writes a zero-terminated string; `extra` is length-counted and has no
/// terminator.
#[derive(Clone, Debug)]
struct HeaderSpec {
    /// `text`.
    text: c_int,
    /// `time` -- the modification time RFC 1952 puts in `MTIME`, which is a 32-bit field.
    time: u32,
    /// `os` -- RFC 1952's `OS` byte. 3 is Unix.
    os: c_int,
    /// `hcrc`: request a header CRC-16, which exercises the `HCRC` state on the way back in.
    hcrc: c_int,
    /// The `FEXTRA` payload, length-counted.
    extra: Vec<u8>,
    /// The `FNAME` string, including its NUL.
    name: Vec<u8>,
    /// The `FCOMMENT` string, including its NUL.
    comment: Vec<u8>,
}

/// The caller-owned buffers `inflateGetHeader` is allowed to write into, and their caps.
///
/// Deliberately pre-filled with `0xa5` rather than zero, borrowing the trick
/// `test/infcover.c`'s allocator uses: a byte the library never writes makes "the library wrote a
/// terminator here" and "this byte was already zero" distinguishable, and it makes the untouched tail
/// of an undersized buffer visible in a failure message.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeaderBuffers {
    /// `extra`, `extra_max` bytes long.
    extra: Vec<u8>,
    /// `name`, `name_max` bytes long.
    name: Vec<u8>,
    /// `comment`, `comm_max` bytes long.
    comment: Vec<u8>,
}

/// The fill byte an untouched header buffer keeps.
const HEADER_FILL: u8 = 0xa5;

/// The value the four scalar `gz_header` members are seeded with before `inflateGetHeader`.
///
/// Outside every legitimate range -- `text` and `hcrc` are booleans, and `os` is a byte -- so that a
/// side which failed to write a member at all is distinguishable from one that wrote zero.
const HEADER_SENTINEL: c_int = -1;

/// How many entries `get_crc_table` publishes at minimum: the 256-entry byte-at-a-time table
/// (`crc32.h`, and `crc32.c` L216-L260).
const CRC_TABLE_LEN: usize = 256;

/// `get_crc_table()[1]`, as `crc32.h` publishes it -- the second entry of the generated
/// byte-at-a-time table, and the cheapest single value that distinguishes a real table from a
/// zeroed or uninitialised one.
const CRC_TABLE_ENTRY_1: u64 = 0x7707_3096;

impl HeaderBuffers {
    /// Three buffers of `cap` bytes each, filled with [`HEADER_FILL`].
    fn new(cap: usize) -> Self {
        Self {
            extra: vec![HEADER_FILL; cap],
            name: vec![HEADER_FILL; cap],
            comment: vec![HEADER_FILL; cap],
        }
    }
}

// =================================================================================================
//  The two sides
// =================================================================================================

/// The surface this suite drives, implemented once per implementation.
///
/// Every method is a thin gate over exactly one `extern "C"` entry point, so that the drivers -- the
/// code that decides chunk boundaries, buffer sizes and flush values -- are written once and executed
/// identically for both sides. That is what makes a difference in the result attributable to the
/// implementations rather than to the harness. The gates decide nothing; they only forward.
trait Side {
    /// How this side is named in failure messages.
    const NAME: &'static str;

    /// This side's `z_stream` type. Layout-identical to the other's, and a different Rust type.
    type Stream;

    /// This side's `gz_header` type. Likewise layout-identical and likewise distinct.
    type Header;

    /// A `z_stream` with every field zeroed and the allocator hooks left null, which selects this
    /// implementation's own internal default allocator.
    fn stream() -> Self::Stream;

    // ---- deflate ----------------------------------------------------------------------------

    /// `deflateInit2_(strm, level, Z_DEFLATED, windowBits, DEF_MEM_LEVEL, Z_DEFAULT_STRATEGY, ...)`.
    fn deflate_init(
        strm: &mut Session<'_, Self::Stream>,
        level: c_int,
        window_bits: c_int,
    ) -> c_int;

    /// `deflateSetDictionary(strm, dictionary, dictLength)` -- `zlib.h` L618.
    fn deflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int;

    /// `deflateSetHeader(strm, head)` -- `zlib.h` L724.
    fn deflate_set_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int;

    /// `deflate(strm, flush)`, with the two windows installed first.
    fn deflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call;

    /// `deflateBound(strm, sourceLen)` -- `zlib.h` L768.
    fn deflate_bound(strm: &mut Session<'_, Self::Stream>, source_len: usize) -> u64;

    /// `deflateEnd(strm)`.
    fn deflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int;

    // ---- inflate ----------------------------------------------------------------------------

    /// `inflateInit2_(strm, windowBits, ...)`.
    fn inflate_init(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int;

    /// `inflate(strm, flush)`, with the two windows installed first.
    fn inflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call;

    /// `inflateSetDictionary(strm, dictionary, dictLength)` -- `zlib.h` L913.
    fn inflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int;

    /// `inflateGetHeader(strm, head)` -- `zlib.h` L1070.
    fn inflate_get_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int;

    /// `inflateReset2(strm, windowBits)` -- `zlib.h` L995.
    fn inflate_reset2(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int;

    /// `inflateSync(strm)` -- `zlib.h` L951.
    fn inflate_sync(strm: &mut Session<'_, Self::Stream>) -> c_int;

    /// `inflateSyncPoint(strm)` -- `zlib.h` L2022.
    fn inflate_sync_point(strm: &mut Session<'_, Self::Stream>) -> c_int;

    /// `inflateEnd(strm)`.
    fn inflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int;

    // ---- state, read and written directly ---------------------------------------------------

    /// The accounting members, read straight off the struct.
    fn snapshot(strm: &Session<'_, Self::Stream>) -> Snapshot;

    /// Sets `avail_in` without touching `next_in`.
    ///
    /// The one place the harness writes a member rather than calling a function, and it exists
    /// because `test/example.c`'s `test_sync` (L370-L408) does exactly this: it offers the two header
    /// bytes, calls `inflate`, then widens `avail_in` to the rest of the buffer -- `next_in` having
    /// already advanced -- and calls `inflateSync`. Reproducing that shape is the point.
    fn set_avail_in(strm: &mut Session<'_, Self::Stream>, avail: usize);

    // ---- the one-shot wrappers ---------------------------------------------------------------

    /// `compress2(dest, &destLen, source, sourceLen, level)` -- `zlib.h` L1288.
    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> OneShot;

    /// `uncompress2(dest, &destLen, source, &sourceLen)` -- `zlib.h` L1330.
    fn uncompress2(dest: &mut [u8], source: &[u8]) -> OneShot;

    /// `compressBound(sourceLen)` -- `zlib.h` L1307.
    fn compress_bound(source_len: usize) -> u64;

    // ---- gzip headers ------------------------------------------------------------------------

    /// A `gz_header` describing `spec`, for `deflateSetHeader`.
    fn write_header(spec: &mut HeaderSpec) -> Self::Header;

    /// A `gz_header` pointing at `buffers`, for `inflateGetHeader`.
    fn read_header(buffers: &mut HeaderBuffers) -> Self::Header;

    /// The scalar members of a `gz_header`, normalised for comparison.
    fn header_report(head: &Self::Header) -> HeaderReport;

    // ---- introspection, for the plumbing anchor ----------------------------------------------

    /// `zlibVersion()` -- `zlib.h` L224.
    fn version() -> String;

    /// `get_crc_table()[index]` -- `zlib.h` L2035.
    fn crc_table_entry(index: usize) -> u64;

    /// `adler32(1, data, len)` seeded the way `zlib.h` L1809-L1827 requires.
    fn adler32(data: &[u8]) -> u64;

    /// `crc32(0, data, len)` seeded the way `zlib.h` L1848-L1864 requires.
    fn crc32(data: &[u8]) -> u64;
}

/// The port, reached through the `libz-rs-sys` facade.
#[derive(Debug)]
struct Port;

/// The C reference, reached through this crate's `c_`-prefixed oracle archive.
#[derive(Debug)]
struct Reference;

/// Installs the port's two windows for one call, establishing obligations (c) and (d).
///
/// `window` is `Some` to install a new input window and `None` to leave `next_in`/`avail_in` exactly
/// as the previous call left them, which is how a real caller drives a partially consumed chunk. A
/// zero-length input window becomes a NULL `next_in`, the pairing `zlib.h` L91-L92 permits; the output
/// window is asserted non-empty because both entry points answer `Z_BUF_ERROR` for `avail_out == 0`
/// and a run that tripped that would be measuring the harness.
fn install_port(strm: &mut libz_rs_sys::z_stream, window: Option<&[u8]>, out: &mut [u8]) {
    assert!(
        !out.is_empty(),
        "no call in this suite may be offered a zero-length output window"
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
fn install_reference(strm: &mut oracle::z_stream, window: Option<&[u8]>, out: &mut [u8]) {
    assert!(
        !out.is_empty(),
        "no call in this suite may be offered a zero-length output window"
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

impl Side for Port {
    const NAME: &'static str = "port (libz-rs-sys)";
    type Stream = libz_rs_sys::z_stream;
    type Header = libz_rs_sys::gz_header;

    fn stream() -> Self::Stream {
        // Built field by field rather than through `MaybeUninit::zeroed`, which is both stricter and
        // cheaper: it needs no `unsafe`, and it makes the three members a C caller actually sets
        // visible as the deliberate choices they are. `None` is C's `Z_NULL` for the two hooks, which
        // selects the library's own allocator.
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

    fn deflate_init(
        strm: &mut Session<'_, Self::Stream>,
        level: c_int,
        window_bits: c_int,
    ) -> c_int {
        // The gate supplies the facade's own `ZLIB_VERSION` and `size_of::<z_stream>()`, which is
        // obligation (e) discharged where the two cannot be crossed with the reference's pair.
        port::deflate_init2(
            strm,
            level,
            Z_DEFLATED,
            window_bits,
            DEFAULT_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        )
    }

    fn deflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int {
        // The dictionary crosses as a slice, so its length cannot disagree with its pointer. The
        // library copies what it keeps into the window rather than retaining what it was given.
        port::deflate_set_dictionary(strm, dictionary)
    }

    fn deflate_set_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int {
        // The library RETAINS the pointer to `head` until the header has been fully emitted, which
        // is why the caller keeps both the header and the buffers it points into alive for the whole
        // stream. The gate cannot express that lifetime, so it is this side's obligation.
        port::deflate_set_header(strm, head)
    }

    fn deflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call {
        install_port(strm, window, out);
        let (offered_out, offered_in) = (strm.avail_out, strm.avail_in);
        // The one call that reads through `next_in` and writes through `next_out`. `install_port`
        // immediately above establishes the null-with-zero-length pairing and the non-empty output
        // window; the gate makes the call.
        let status = port::deflate(strm, flush);
        Call {
            status,
            produced: widen_avail(offered_out - strm.avail_out),
            consumed: widen_avail(offered_in - strm.avail_in),
            avail_in_after: widen_avail(strm.avail_in),
        }
    }

    fn deflate_bound(strm: &mut Session<'_, Self::Stream>, source_len: usize) -> u64 {
        // `deflateBound` reads `wrap`, `strstart`, `w_bits` and `hash_bits` out of the state block
        // and writes nothing; it touches neither window. `Some(..)` is the gate's live-stream form.
        widen_ulong(port::deflate_bound(Some(strm), narrow_ulong(source_len)))
    }

    fn deflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // `deflateEnd` frees the state through the same allocator that produced it and nulls
        // `state`, so a second call is a documented no-op rather than a double free.
        port::deflate_end(strm)
    }

    fn inflate_init(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int {
        // The version/size pair is the gate's, exactly as for `deflate_init`.
        port::inflate_init2(strm, window_bits)
    }

    fn inflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call {
        install_port(strm, window, out);
        let (offered_out, offered_in) = (strm.avail_out, strm.avail_in);
        // As for `Port::deflate`: the installer above owns the windows, the gate owns the call.
        let status = port::inflate(strm, flush);
        Call {
            status,
            produced: widen_avail(offered_out - strm.avail_out),
            consumed: widen_avail(offered_in - strm.avail_in),
            avail_in_after: widen_avail(strm.avail_in),
        }
    }

    fn inflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int {
        // As for `Port::deflate_set_dictionary`: the dictionary crosses as a slice.
        port::inflate_set_dictionary(strm, dictionary)
    }

    fn inflate_get_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int {
        // Unsafe-site category 6 of AAP §0.6.1: the library writes through `head->extra`,
        // `head->name` and `head->comment` as it parses, so each non-null pointer must address at
        // least the matching `*_max` writable bytes. [`HeaderBuffers`] on this side is what sizes
        // them, and the pointer to `head` is retained until the header completes, so `head` outlives
        // every `inflate` call on this stream. Neither guarantee is expressible in the gate's
        // signature, which is why both stay stated here.
        port::inflate_get_header(strm, head)
    }

    fn inflate_reset2(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int {
        // `inflateReset2` may reallocate the window through the stream's own allocator and writes
        // only the state block.
        port::inflate_reset2(strm, window_bits)
    }

    fn inflate_sync(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // `inflateSync` scans forward through the `avail_in` bytes at `next_in` looking for a flush
        // point and writes no output at all, so it reads the window the previous call left installed.
        port::inflate_sync(strm)
    }

    fn inflate_sync_point(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // Reads two state members and writes nothing.
        port::inflate_sync_point(strm)
    }

    fn inflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // As for `Port::deflate_end`.
        port::inflate_end(strm)
    }

    fn snapshot(strm: &Session<'_, Self::Stream>) -> Snapshot {
        Snapshot {
            total_in: widen_ulong(strm.total_in),
            total_out: widen_ulong(strm.total_out),
            adler: widen_ulong(strm.adler),
            data_type: strm.data_type,
        }
    }

    fn set_avail_in(strm: &mut Session<'_, Self::Stream>, avail: usize) {
        strm.avail_in = narrow_avail(avail);
    }

    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> OneShot {
        // The gate seeds the in/out `destLen` from `dest.len()` and hands back what the call left
        // there. Both buffers cross as slices, so neither length can disagree with its pointer.
        let (status, dest_len) = port::compress2(dest, source, level);
        OneShot {
            status,
            dest_len: widen_ulong(dest_len),
            // `compress2` takes its source length by value, so nothing reports consumption.
            source_len: widen_ulong(narrow_ulong(source.len())),
        }
    }

    fn uncompress2(dest: &mut [u8], source: &[u8]) -> OneShot {
        // As for `Port::compress2`, and additionally `sourceLen` is an in/out parameter here --
        // `uncompr.c` L86 reads it and L120 writes back the bytes actually consumed -- which is why
        // this gate returns three values rather than two.
        let (status, dest_len, source_len) = port::uncompress2(dest, source);
        OneShot {
            status,
            dest_len: widen_ulong(dest_len),
            source_len: widen_ulong(source_len),
        }
    }

    fn compress_bound(source_len: usize) -> u64 {
        widen_ulong(libz_rs_sys::compressBound(narrow_ulong(source_len)))
    }

    fn write_header(spec: &mut HeaderSpec) -> Self::Header {
        libz_rs_sys::gz_header {
            text: spec.text,
            time: libz_rs_sys::uLong::from(spec.time),
            // `xflags` is documented as not used when writing (`zlib.h` L120), so it is left at 0 and
            // its value on the way back in is compared rather than dictated.
            xflags: 0,
            os: spec.os,
            extra: spec.extra.as_mut_ptr(),
            extra_len: narrow_avail(spec.extra.len()),
            // `extra_max`, `name_max` and `comm_max` are read-side only (`zlib.h` L124-L129).
            extra_max: 0,
            name: spec.name.as_mut_ptr(),
            name_max: 0,
            comment: spec.comment.as_mut_ptr(),
            comm_max: 0,
            hcrc: spec.hcrc,
            done: 0,
        }
    }

    fn read_header(buffers: &mut HeaderBuffers) -> Self::Header {
        let (extra_max, name_max, comm_max) = (
            narrow_avail(buffers.extra.len()),
            narrow_avail(buffers.name.len()),
            narrow_avail(buffers.comment.len()),
        );
        libz_rs_sys::gz_header {
            // The four scalars the library fills are seeded with values it cannot legitimately
            // produce, so that "the library wrote this" and "the library left it alone" are
            // distinguishable in the report.
            text: HEADER_SENTINEL,
            time: 0,
            xflags: HEADER_SENTINEL,
            os: HEADER_SENTINEL,
            extra: buffers.extra.as_mut_ptr(),
            extra_len: 0,
            extra_max,
            name: buffers.name.as_mut_ptr(),
            name_max,
            comment: buffers.comment.as_mut_ptr(),
            comm_max,
            hcrc: HEADER_SENTINEL,
            done: 0,
        }
    }

    fn header_report(head: &Self::Header) -> HeaderReport {
        HeaderReport {
            text: head.text,
            time: widen_ulong(head.time),
            xflags: head.xflags,
            os: head.os,
            extra_len: u64::from(head.extra_len),
            extra_max: u64::from(head.extra_max),
            name_max: u64::from(head.name_max),
            comm_max: u64::from(head.comm_max),
            hcrc: head.hcrc,
            done: head.done,
            extra_present: !head.extra.is_null(),
            name_present: !head.name.is_null(),
            comment_present: !head.comment.is_null(),
        }
    }

    fn version() -> String {
        // The gate hands back the library's own `'static` NUL-terminated version string.
        port::zlib_version().to_string_lossy().into_owned()
    }

    fn crc_table_entry(index: usize) -> u64 {
        assert!(
            index < CRC_TABLE_LEN,
            "index {index} is outside the CRC table"
        );
        // The gate presents the table as the `'static` slice the C contract describes -- at least
        // 256 `z_crc_t` entries in `const` storage (`crc32.c` L216-L260 and its const-evaluated
        // counterpart) -- so the index above is bounds-checked by the language.
        u64::from(port::crc_table()[index])
    }

    fn adler32(data: &[u8]) -> u64 {
        // The seed is 1, as `zlib.h` L1809-L1827 requires; the gate forms the pointer/length pair
        // and answers a zero-length slice without a dereference.
        widen_ulong(port::adler32(1, data))
    }

    fn crc32(data: &[u8]) -> u64 {
        // The seed is 0, as `zlib.h` L1848-L1864 requires; otherwise as for `Port::adler32`.
        widen_ulong(port::crc32(0, data))
    }
}

impl Side for Reference {
    const NAME: &'static str = "C reference (oracle)";
    type Stream = oracle::z_stream;
    type Header = oracle::gz_header;

    fn stream() -> Self::Stream {
        // Field by field, for the reason given on `Port::stream`.
        oracle::z_stream {
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

    fn deflate_init(
        strm: &mut Session<'_, Self::Stream>,
        level: c_int,
        window_bits: c_int,
    ) -> c_int {
        // The gate passes the oracle's own version string -- what `c_zlibVersion` points at -- and
        // `size_of` of the oracle's mirror, which is obligation (e) on this side. `deflateInit2` is a
        // `zlib.h` macro rather than a symbol, so the `_`-suffixed form is what the gate calls.
        oracle::deflate_init2(
            strm,
            level,
            Z_DEFLATED,
            window_bits,
            DEFAULT_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
        )
    }

    fn deflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int {
        // As for the port's gate: the dictionary crosses as a slice.
        oracle::deflate_set_dictionary(strm, dictionary)
    }

    fn deflate_set_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int {
        // As for the port's gate, including the retained `head` pointer this side must keep alive.
        oracle::deflate_set_header(strm, head)
    }

    fn deflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call {
        install_reference(strm, window, out);
        let (offered_out, offered_in) = (strm.avail_out, strm.avail_in);
        // As for the port's gate: the installer above owns the windows.
        let status = oracle::deflate(strm, flush);
        Call {
            status,
            produced: widen_avail_oracle(offered_out - strm.avail_out),
            consumed: widen_avail_oracle(offered_in - strm.avail_in),
            avail_in_after: widen_avail_oracle(strm.avail_in),
        }
    }

    fn deflate_bound(strm: &mut Session<'_, Self::Stream>, source_len: usize) -> u64 {
        // Reads the state block and writes nothing. `Some(..)` is the gate's live-stream form.
        widen_ulong_oracle(oracle::deflate_bound(
            Some(strm),
            narrow_ulong_oracle(source_len),
        ))
    }

    fn deflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // As for the port's gate.
        oracle::deflate_end(strm)
    }

    fn inflate_init(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int {
        // The version/size pair is the gate's, exactly as for `Reference::deflate_init`.
        oracle::inflate_init2(strm, window_bits)
    }

    fn inflate(
        strm: &mut Session<'_, Self::Stream>,
        window: Option<&[u8]>,
        out: &mut [u8],
        flush: c_int,
    ) -> Call {
        install_reference(strm, window, out);
        let (offered_out, offered_in) = (strm.avail_out, strm.avail_in);
        // As for the port's gate.
        let status = oracle::inflate(strm, flush);
        Call {
            status,
            produced: widen_avail_oracle(offered_out - strm.avail_out),
            consumed: widen_avail_oracle(offered_in - strm.avail_in),
            avail_in_after: widen_avail_oracle(strm.avail_in),
        }
    }

    fn inflate_set_dictionary(strm: &mut Session<'_, Self::Stream>, dictionary: &[u8]) -> c_int {
        // As for the port's gate.
        oracle::inflate_set_dictionary(strm, dictionary)
    }

    fn inflate_get_header<'r>(
        strm: &mut Session<'r, Self::Stream>,
        head: &'r mut Self::Header,
    ) -> c_int {
        // The reference half of unsafe-site category 6. The buffer caps [`HeaderBuffers`] sets are
        // what keep the C code's `state->head->extra_max` clamps within the caller's memory, and
        // `head` must outlive every `inflate` call on this stream because the pointer is retained.
        oracle::inflate_get_header(strm, head)
    }

    fn inflate_reset2(strm: &mut Session<'_, Self::Stream>, window_bits: c_int) -> c_int {
        // As for the port's gate.
        oracle::inflate_reset2(strm, window_bits)
    }

    fn inflate_sync(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // As for the port's gate: it scans the window the previous call left installed.
        oracle::inflate_sync(strm)
    }

    fn inflate_sync_point(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // Reads two state members and writes nothing.
        oracle::inflate_sync_point(strm)
    }

    fn inflate_end(strm: &mut Session<'_, Self::Stream>) -> c_int {
        // As for the port's gate.
        oracle::inflate_end(strm)
    }

    fn snapshot(strm: &Session<'_, Self::Stream>) -> Snapshot {
        Snapshot {
            total_in: widen_ulong_oracle(strm.total_in),
            total_out: widen_ulong_oracle(strm.total_out),
            adler: widen_ulong_oracle(strm.adler),
            data_type: strm.data_type,
        }
    }

    fn set_avail_in(strm: &mut Session<'_, Self::Stream>, avail: usize) {
        strm.avail_in = narrow_avail_oracle(avail);
    }

    fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> OneShot {
        // As for `Port::compress2`.
        let (status, dest_len) = oracle::compress2(dest, source, level);
        OneShot {
            status,
            dest_len: widen_ulong_oracle(dest_len),
            source_len: widen_ulong_oracle(narrow_ulong_oracle(source.len())),
        }
    }

    fn uncompress2(dest: &mut [u8], source: &[u8]) -> OneShot {
        // As for `Port::uncompress2`, including the in/out `sourceLen` the gate returns third.
        let (status, dest_len, source_len) = oracle::uncompress2(dest, source);
        OneShot {
            status,
            dest_len: widen_ulong_oracle(dest_len),
            source_len: widen_ulong_oracle(source_len),
        }
    }

    fn compress_bound(source_len: usize) -> u64 {
        // `compressBound` is a pure arithmetic function over its argument -- it dereferences nothing
        // at all (`compress.c` L86-L88) -- but it is still `extern "C"`, so it crosses through a gate
        // like every other reference entry point. The port's counterpart needs none, because the
        // facade declares its pointer-free entry points safe.
        widen_ulong_oracle(oracle::compress_bound(narrow_ulong_oracle(source_len)))
    }

    fn write_header(spec: &mut HeaderSpec) -> Self::Header {
        oracle::gz_header {
            text: spec.text,
            time: oracle::uLong::from(spec.time),
            xflags: 0,
            os: spec.os,
            extra: spec.extra.as_mut_ptr(),
            extra_len: narrow_avail_oracle(spec.extra.len()),
            extra_max: 0,
            name: spec.name.as_mut_ptr(),
            name_max: 0,
            comment: spec.comment.as_mut_ptr(),
            comm_max: 0,
            hcrc: spec.hcrc,
            done: 0,
        }
    }

    fn read_header(buffers: &mut HeaderBuffers) -> Self::Header {
        let (extra_max, name_max, comm_max) = (
            narrow_avail_oracle(buffers.extra.len()),
            narrow_avail_oracle(buffers.name.len()),
            narrow_avail_oracle(buffers.comment.len()),
        );
        oracle::gz_header {
            text: HEADER_SENTINEL,
            time: 0,
            xflags: HEADER_SENTINEL,
            os: HEADER_SENTINEL,
            extra: buffers.extra.as_mut_ptr(),
            extra_len: 0,
            extra_max,
            name: buffers.name.as_mut_ptr(),
            name_max,
            comment: buffers.comment.as_mut_ptr(),
            comm_max,
            hcrc: HEADER_SENTINEL,
            done: 0,
        }
    }

    fn header_report(head: &Self::Header) -> HeaderReport {
        HeaderReport {
            text: head.text,
            time: widen_ulong_oracle(head.time),
            xflags: head.xflags,
            os: head.os,
            extra_len: u64::from(head.extra_len),
            extra_max: u64::from(head.extra_max),
            name_max: u64::from(head.name_max),
            comm_max: u64::from(head.comm_max),
            hcrc: head.hcrc,
            done: head.done,
            extra_present: !head.extra.is_null(),
            name_present: !head.name.is_null(),
            comment_present: !head.comment.is_null(),
        }
    }

    fn version() -> String {
        // The gate hands back what `c_zlibVersion` points at: `ZLIB_VERSION`, a string literal with
        // static storage duration in `zutil.c`.
        oracle::reference_version().to_string_lossy().into_owned()
    }

    fn crc_table_entry(index: usize) -> u64 {
        assert!(
            index < CRC_TABLE_LEN,
            "index {index} is outside the CRC table"
        );
        // The gate presents the C library's own table (`crc32.c` L216-L260) as a `'static` slice of
        // at least 256 entries -- built once and never mutated afterwards -- so the index above is
        // bounds-checked by the language.
        u64::from(oracle::crc_table()[index])
    }

    fn adler32(data: &[u8]) -> u64 {
        // As for `Port::adler32`.
        widen_ulong_oracle(oracle::adler32(1, data))
    }

    fn crc32(data: &[u8]) -> u64 {
        // As for `Reference::adler32`, seeded with 0 rather than 1.
        widen_ulong_oracle(oracle::crc32(0, data))
    }
}

// =================================================================================================
//  The drivers
//
//  ONE function per operation, instantiated twice. Everything that decides *what* the library is
//  asked to do -- when to refill the input window, how large each window is, which flush value each
//  call carries, when to stop -- lives here and therefore happens identically on both sides.
// =================================================================================================

/// The `adler` a raw **deflate** stream reports.
///
/// `deflateResetKeep` sets `strm->adler = adler32(0, Z_NULL, 0)` whenever `wrap != 2`
/// (`deflate.c` L1002-L1006), and raw deflate has `wrap == 0`, so the member holds the Adler-32
/// identity and is never advanced. Asserting the constant is asserting the documented "no check
/// value" behaviour of RFC 1951 rather than a checksum neither side computes.
const RAW_DEFLATE_ADLER: u64 = 1;

/// The `adler` a raw **inflate** stream reports.
///
/// `inflateResetKeep` sets `strm->adler = state->wrap & 1` (`inflate.c` L118-L122), and raw inflate
/// has `wrap == 0`, so this member holds 0 rather than the Adler-32 identity. The asymmetry with
/// [`RAW_DEFLATE_ADLER`] is genuine, is in both implementations, and is exactly why the raw container
/// cannot have the two sides' `adler` compared to each other.
const RAW_INFLATE_ADLER: u64 = 0;

/// RFC 1950's 2-byte header plus the 4-byte `DICTID` that follows it when `FDICT` is set.
///
/// The number of bytes `inflate` consumes before it can answer `Z_NEED_DICT`, and therefore the number
/// it leaves out of `total_in` on that path. See the derivation at its use site in [`cross`].
const ZLIB_HEADER_AND_DICTID: u64 = 2 + 4;

/// How many consecutive calls may make no progress before the driver declares a hang.
///
/// A `deflate` or `inflate` call returning `Z_OK` with a non-empty output window must either consume
/// an input byte or produce an output byte, so a short run of calls doing neither is what a
/// non-progressing loop looks like. Turning that into a named failure rather than a hung test is one
/// of this file's stated obligations.
const STALL_LIMIT: usize = 3;

/// Slack added to `deflateBound` when sizing a single-shot output buffer.
///
/// The bound is exact for one `Z_FINISH` call, which is what the single-shot path performs; the slack
/// exists so that a bound divergence surfaces as a comparison failure in
/// `tests/byte_identical.rs` -- which is where bounds are actually gated -- rather than as a
/// `Z_BUF_ERROR` here that would look like an interoperability defect.
const BOUND_SLACK: usize = 64;

/// Compresses `input` on one side and reports the run plus the dictionary identifier.
///
/// The second element is `strm->adler` read **immediately after** `deflateSetDictionary`, which is
/// the `dictId` handshake `test/example.c` L429 captures; it is `None` when no dictionary was set.
fn deflate_run<S: Side>(
    input: &[u8],
    level: c_int,
    window_bits: c_int,
    dictionary: Option<&[u8]>,
    chunking: Chunking,
) -> (Run, Option<u64>) {
    // Boxed because a `z_stream` must not be relocated once `deflateInit2_` has stored its
    // back-pointer -- see the obligations block above.
    let mut stream = Session::new(S::stream());
    let init = S::deflate_init(&mut stream, level, window_bits);
    assert!(
        init == Z_OK,
        "{side}: deflateInit2_(level={level}, windowBits={window_bits}) returned {got} \
         rather than Z_OK; a case that cannot even open a stream measures nothing",
        side = S::NAME,
        got = status(init),
    );

    let dict_id = dictionary.map(|dictionary| {
        // `deflateSetDictionary` must precede any `deflate` call (`zlib.h` L618-L650), which is why
        // this happens before the output buffer is even sized.
        let set = S::deflate_set_dictionary(&mut stream, dictionary);
        assert!(
            set == Z_OK,
            "{side}: deflateSetDictionary returned {got} rather than Z_OK",
            side = S::NAME,
            got = status(set),
        );
        S::snapshot(&stream).adler
    });

    let bound = usize::try_from(S::deflate_bound(&mut stream, input.len()))
        .expect("a deflateBound fits in a usize");
    let window_len = chunking.output.unwrap_or(bound + BOUND_SLACK).max(1);
    let mut scratch = vec![0_u8; window_len];

    let mut output = Vec::with_capacity(bound + BOUND_SLACK);
    let mut calls = Vec::new();
    let mut offered = 0_usize;
    let mut need_input = true;
    let mut stalled = 0_usize;
    let final_status = loop {
        let window = if need_input {
            let take = chunking
                .input
                .unwrap_or(input.len() - offered)
                .min(input.len() - offered);
            let slice = &input[offered..offered + take];
            offered += take;
            Some(slice)
        } else {
            None
        };

        // `Z_FINISH` the moment the whole input has been handed over, and never before: it means "no
        // further input will be supplied", so raising it early would be lying to the library.
        let flush = if offered == input.len() {
            Z_FINISH
        } else {
            Z_NO_FLUSH
        };
        let call = S::deflate(&mut stream, window, &mut scratch, flush);
        output.extend_from_slice(&scratch[..call.produced]);
        calls.push(call);
        need_input = call.avail_in_after == 0;

        if call.status != Z_OK {
            break call.status;
        }
        if call.produced == 0 && call.consumed == 0 {
            stalled += 1;
            assert!(
                stalled < STALL_LIMIT,
                "{side}: deflate made no progress on {stalled} consecutive calls at \
                 level={level} windowBits={window_bits} chunking={chunking} \
                 (input {len} bytes, {given} handed over); this is a hang, not a slow case",
                side = S::NAME,
                chunking = chunking.name,
                len = input.len(),
                given = offered,
            );
        } else {
            stalled = 0;
        }
    };

    let snapshot = S::snapshot(&stream);
    let end = S::deflate_end(&mut stream);
    (
        Run {
            calls,
            output,
            snapshot,
            final_status,
            end,
        },
        dict_id,
    )
}

/// What the decoder observed about a preset dictionary, if one was in play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DictionaryHandshake {
    /// `strm->adler` at the moment `inflate` answered `Z_NEED_DICT`, which is the identifier the
    /// compressor chose (`zlib.h` L916-L919). `None` when `Z_NEED_DICT` was never returned, which is
    /// the correct and expected outcome for raw inflate.
    need_dict_adler: Option<u64>,
    /// What `inflateSetDictionary` returned, on whichever path called it.
    set_status: Option<c_int>,
    /// How many input bytes the call that answered `Z_NEED_DICT` consumed.
    ///
    /// Exactly the bytes `inflate` leaves out of `total_in` on that path, and chunking-dependent: a
    /// caller offering the whole stream at once loses all six of RFC 1950's header and `DICTID` bytes,
    /// while one offering a byte at a time loses only the last of them. Zero when `Z_NEED_DICT` was
    /// never returned.
    unaccounted_in: u64,
}

/// Decompresses `compressed` on one side and reports the run plus what it observed about the
/// dictionary.
///
/// `dictionary` drives two different code paths, and the difference is `zlib.h`'s, not this file's:
/// for **raw** inflate the dictionary must be installed up front because there is no `Z_NEED_DICT`
/// signal to wait for (`zlib.h` L921-L926), while for the zlib container it is installed in response
/// to that signal, which is the handshake `test/example.c` L466-L479 performs.
fn inflate_run<S: Side>(
    compressed: &[u8],
    window_bits: c_int,
    dictionary: Option<&[u8]>,
    chunking: Chunking,
) -> (Run, DictionaryHandshake) {
    let mut stream = Session::new(S::stream());
    let init = S::inflate_init(&mut stream, window_bits);
    assert!(
        init == Z_OK,
        "{side}: inflateInit2_(windowBits={window_bits}) returned {got} rather than Z_OK",
        side = S::NAME,
        got = status(init),
    );

    let mut handshake = DictionaryHandshake::default();
    if let Some(dictionary) = dictionary {
        if window_bits < 0 {
            handshake.set_status = Some(S::inflate_set_dictionary(&mut stream, dictionary));
        }
    }

    let window_len = chunking.output.unwrap_or(DEFAULT_INFLATE_WINDOW).max(1);
    let mut scratch = vec![0_u8; window_len];

    let mut output = Vec::new();
    let mut calls = Vec::new();
    let mut offered = 0_usize;
    let mut need_input = true;
    let mut stalled = 0_usize;
    let final_status = loop {
        let window = if need_input {
            let take = chunking
                .input
                .unwrap_or(compressed.len() - offered)
                .min(compressed.len() - offered);
            let slice = &compressed[offered..offered + take];
            offered += take;
            Some(slice)
        } else {
            None
        };

        // Always `Z_NO_FLUSH`: it is what a caller decompressing a whole stream uses, and it is what
        // `test/example.c` L468 passes. The flush values that stop early have their own cases.
        let call = S::inflate(&mut stream, window, &mut scratch, Z_NO_FLUSH);
        output.extend_from_slice(&scratch[..call.produced]);
        calls.push(call);
        need_input = call.avail_in_after == 0;

        if call.status == Z_NEED_DICT {
            handshake.need_dict_adler = Some(S::snapshot(&stream).adler);
            handshake.unaccounted_in =
                u64::try_from(call.consumed).expect("a window length fits in a u64");
            match dictionary {
                Some(dictionary) => {
                    let set = S::inflate_set_dictionary(&mut stream, dictionary);
                    handshake.set_status = Some(set);
                    if set != Z_OK {
                        break set;
                    }
                    // The dictionary is in place and the next call resumes where this one stopped.
                    // `need_input` was already set from `avail_in_after` above and must not be
                    // overridden: with a one-byte input window the `Z_NEED_DICT` call consumes the
                    // last byte it was given, so the next call does need a fresh one -- forcing
                    // `false` here starves the stream and answers `Z_BUF_ERROR`.
                    stalled = 0;
                    continue;
                }
                // No dictionary to offer: `Z_NEED_DICT` is the answer, and it is the answer both
                // sides must give.
                None => break Z_NEED_DICT,
            }
        }
        if call.status != Z_OK {
            break call.status;
        }
        if call.produced == 0 && call.avail_in_after == 0 && offered == compressed.len() {
            // Every byte has been handed over and consumed and nothing came out: the stream is
            // truncated or empty. `Z_OK` is the honest report, and both sides must agree on it.
            break Z_OK;
        }
        if call.produced == 0 && call.consumed == 0 {
            stalled += 1;
            assert!(
                stalled < STALL_LIMIT,
                "{side}: inflate made no progress on {stalled} consecutive calls at \
                 windowBits={window_bits} chunking={chunking} ({len} compressed bytes, \
                 {given} handed over, {out} produced so far); this is a hang, not a slow case",
                side = S::NAME,
                chunking = chunking.name,
                len = compressed.len(),
                given = offered,
                out = output.len(),
            );
        } else {
            stalled = 0;
        }
    };

    let snapshot = S::snapshot(&stream);
    let end = S::inflate_end(&mut stream);
    (
        Run {
            calls,
            output,
            snapshot,
            final_status,
            end,
        },
        handshake,
    )
}

/// The output window a single-shot inflate offers.
///
/// Generous rather than derived: the decompressed size of an arbitrary stream is not knowable in
/// advance, and the driver loops, so this is a chunk size and not a capacity. 64 KiB is two window
/// widths, which keeps `inflate_fast`'s 258-byte requirement comfortably satisfiable and so keeps the
/// fast path in play -- entry 3 of [`DECODER_TRIAGE`] would go unexercised with a small window.
const DEFAULT_INFLATE_WINDOW: usize = 64 * 1024;

// =================================================================================================
//  One crossing
// =================================================================================================

/// Everything that identifies a single crossing, for the failure message.
#[derive(Clone, Copy, Debug)]
struct Case {
    /// Which side compressed and which decompressed.
    direction: Direction,
    /// Which wrapper the stream carries.
    container: Container,
    /// The compressing side's `windowBits`.
    deflate_bits: c_int,
    /// The decompressing side's `windowBits`, which is not always the same number.
    inflate_bits: c_int,
    /// The compression level.
    level: c_int,
    /// How the drivers fed and drained both streams.
    chunking: Chunking,
    /// Which committed fixture supplied the input.
    fixture: &'static str,
    /// Whether a preset dictionary was in play.
    dictionary: bool,
}

/// The one-line description every failure quotes.
///
/// Names the direction, the container, **both** `windowBits`, the level, the fixture, the chunking and
/// whether a dictionary was involved -- which is the whole of what a reader needs to reproduce the
/// case by hand.
fn describe(case: Case, input_len: usize) -> String {
    format!(
        "  direction: {direction}\n  \
         container: {container}  windowBits: deflate={deflate_bits} inflate={inflate_bits}\n  \
         level: {level}  chunking: {chunking}  dictionary: {dictionary}\n  \
         fixture: {fixture} ({input_len} bytes)",
        direction = case.direction.name(),
        container = case.container.name(),
        deflate_bits = case.deflate_bits,
        inflate_bits = case.inflate_bits,
        level = case.level,
        chunking = case.chunking.name,
        dictionary = if case.dictionary {
            "yes (test/example.c's \"hello\" preset)"
        } else {
            "no"
        },
        fixture = case.fixture,
    )
}

/// Panics with the full crossing report: the case, every finding and the decoder triage list.
fn report(case: Case, input_len: usize, findings: &[String]) -> ! {
    let mut message = String::new();
    let _ = writeln!(
        message,
        "INTEROPERABILITY FAILURE -- one implementation could not read the other's stream\n\
         \n{case}\n",
        case = describe(case, input_len),
    );
    let _ = writeln!(message, "{count} finding(s):", count = findings.len());
    for (index, finding) in findings.iter().enumerate() {
        let _ = writeln!(message, "  [{n}] {finding}", n = index + 1);
    }
    let _ = writeln!(message, "\n{DECODER_TRIAGE}");
    panic!("{message}");
}

/// Runs one crossing: `C` compresses, `D` decompresses, and the result must be the original bytes.
///
/// Every difference is collected as a finding and reported together, because a crossing that got both
/// the bytes *and* the accounting wrong is more informative than the first of the two.
fn cross<C: Side, D: Side>(case: Case, input: &[u8], dictionary: Option<&[u8]>) {
    let (compressed, dict_id) = deflate_run::<C>(
        input,
        case.level,
        case.deflate_bits,
        dictionary,
        case.chunking,
    );
    let mut findings = Vec::new();

    if compressed.final_status != Z_STREAM_END {
        findings.push(format!(
            "the compressing side ({side}) did not finish: deflate returned {got} rather than \
             Z_STREAM_END after {calls} call(s), having produced {len} bytes",
            side = C::NAME,
            got = status(compressed.final_status),
            calls = compressed.calls.len(),
            len = compressed.output.len(),
        ));
    }
    if compressed.end != Z_OK {
        findings.push(format!(
            "the compressing side ({side}) reported {got} from deflateEnd",
            side = C::NAME,
            got = status(compressed.end),
        ));
    }
    if !findings.is_empty() {
        report(case, input.len(), &findings);
    }

    let (inflated, handshake) = inflate_run::<D>(
        &compressed.output,
        case.inflate_bits,
        dictionary,
        case.chunking,
    );

    if inflated.final_status != Z_STREAM_END {
        findings.push(format!(
            "the decompressing side ({side}) did not finish: inflate returned {got} rather than \
             Z_STREAM_END after {calls} call(s), having produced {out} of the expected {want} \
             bytes from the {clen}-byte stream the other side wrote",
            side = D::NAME,
            got = status(inflated.final_status),
            calls = inflated.calls.len(),
            out = inflated.output.len(),
            want = input.len(),
            clen = compressed.output.len(),
        ));
    }
    if inflated.end != Z_OK {
        findings.push(format!(
            "the decompressing side ({side}) reported {got} from inflateEnd",
            side = D::NAME,
            got = status(inflated.end),
        ));
    }
    if inflated.output != input {
        findings.push(describe_byte_difference(&inflated.output, input));
    }

    // Accounting. A decoder that produced the right bytes while disagreeing about how much input it
    // consumed still corrupts the next member of a concatenated stream, so this is not decoration.
    let want_out = u64::try_from(input.len()).expect("a fixture length fits in a u64");
    if inflated.snapshot.total_out != want_out {
        findings.push(format!(
            "total_out is {got} on the decompressing side and the fixture is {want_out} bytes",
            got = inflated.snapshot.total_out,
        ));
    }
    // The two `total_in` figures below each carry one documented adjustment, and both adjustments are
    // quirks of the reference implementation that the port has to reproduce rather than tidy up.
    // Deriving them here is what lets the assertions stay exact instead of being relaxed away for the
    // dictionary cases.
    //
    // `inflate` returns `Z_NEED_DICT` through `RESTORE(); return Z_NEED_DICT;` in the `DICT` state,
    // which bypasses the `inf_leave:` label where `strm->total_in += in` happens. So the bytes that
    // one call consumed are never counted -- at most RFC 1950's 2-byte header plus its 4-byte
    // `DICTID`, and fewer when the caller chunked its input finely enough to have consumed some of
    // them on an earlier call. Both implementations do this, and a port that "fixed" it would diverge.
    let unaccounted = handshake.unaccounted_in;
    // A ceiling rather than an equality, because the figure is chunking-dependent -- but a value above
    // the ceiling would mean the `Z_NEED_DICT` return had consumed payload bytes as well as wrapper
    // bytes, which is a defect and not a chunking artefact.
    assert!(
        unaccounted <= ZLIB_HEADER_AND_DICTID,
        "{unaccounted} unaccounted input byte(s) is more than the {ZLIB_HEADER_AND_DICTID} a \
         Z_NEED_DICT return can possibly have consumed"
    );
    let want_in = u64::try_from(compressed.output.len()).expect("a stream length fits in a u64")
        - unaccounted;
    if inflated.snapshot.total_in != want_in {
        findings.push(format!(
            "total_in is {got} on the decompressing side and {want_in} was expected for the \
             {clen}-byte stream the other side wrote ({unaccounted} byte(s) of it are consumed \
             before Z_NEED_DICT and so never counted); a decoder that stops short of the trailer \
             has not verified it",
            got = inflated.snapshot.total_in,
            clen = compressed.output.len(),
        ));
    }
    // `deflateSetDictionary` installs the dictionary by pointing `next_in`/`avail_in` at it and
    // calling `fill_window`, which reaches `read_buf` and its `strm->total_in += len`
    // (`deflate.c` L219-L250 and L1580-L1600). So the dictionary's bytes are counted as input on the
    // compressing side. Exact here because every dictionary in this file is smaller than the window,
    // which is the branch that reads all of it rather than only its tail.
    let dictionary_bytes = u64::try_from(dictionary.map_or(0, <[u8]>::len)).unwrap();
    let want_deflated =
        u64::try_from(input.len()).expect("a fixture length fits in a u64") + dictionary_bytes;
    if compressed.snapshot.total_in != want_deflated {
        findings.push(format!(
            "total_in is {got} on the compressing side and {want_deflated} was expected for the \
             {len}-byte fixture plus {dictionary_bytes} dictionary byte(s)",
            got = compressed.snapshot.total_in,
            len = input.len(),
        ));
    }

    check_values::<C, D>(case, input, &compressed, &inflated, &mut findings);
    dictionary_findings(case, dict_id, handshake, &mut findings);

    if !findings.is_empty() {
        report(case, input.len(), &findings);
    }
}

/// Compares the check value the two sides computed, honouring the raw container's lack of one.
fn check_values<C: Side, D: Side>(
    case: Case,
    input: &[u8],
    compressed: &Run,
    inflated: &Run,
    findings: &mut Vec<String>,
) {
    if !case.container.has_check_value() {
        // RFC 1951 defines no check value, so both members hold whatever their own reset left
        // there. The two constants differ from each other and that asymmetry is the reference
        // implementation's; asserting them is asserting the documented behaviour rather than
        // comparing two numbers that were never meant to match.
        if compressed.snapshot.adler != RAW_DEFLATE_ADLER {
            findings.push(format!(
                "raw deflate must leave adler at {RAW_DEFLATE_ADLER} (deflate.c:1002-1006, \
                 wrap == 0) and {side} reports {got}",
                side = C::NAME,
                got = compressed.snapshot.adler,
            ));
        }
        if inflated.snapshot.adler != RAW_INFLATE_ADLER {
            findings.push(format!(
                "raw inflate must leave adler at {RAW_INFLATE_ADLER} (inflate.c:118-122, \
                 wrap & 1 == 0) and {side} reports {got}",
                side = D::NAME,
                got = inflated.snapshot.adler,
            ));
        }
        return;
    }

    if compressed.snapshot.adler != inflated.snapshot.adler {
        findings.push(format!(
            "the two sides disagree about the check value: {cside} computed 0x{cval:08x} while \
             compressing and {dside} computed 0x{dval:08x} while decompressing; matching bytes with \
             a differing check value means the checksum is being fed differently, not that a symbol \
             was decoded wrongly",
            cside = C::NAME,
            cval = compressed.snapshot.adler,
            dside = D::NAME,
            dval = inflated.snapshot.adler,
        ));
    }

    // Independently computed, so that two implementations agreeing on the *wrong* value is caught as
    // well as two disagreeing. The zlib container carries an Adler-32 and gzip a CRC-32
    // (`zlib.h` L893-L894), and each is computed by both sides here.
    let (name, from_c, from_d) = match case.container {
        Container::Zlib => ("Adler-32", C::adler32(input), D::adler32(input)),
        Container::Gzip => ("CRC-32", C::crc32(input), D::crc32(input)),
        // Unreachable given the early return above, and written as a value rather than a panic so
        // that this function has exactly one exit shape.
        Container::Raw => ("none", inflated.snapshot.adler, inflated.snapshot.adler),
    };
    if from_c != from_d {
        findings.push(format!(
            "the two implementations' own {name} of the fixture differ: {cside} says 0x{cval:08x}, \
             {dside} says 0x{dval:08x}",
            cside = C::NAME,
            cval = from_c,
            dside = D::NAME,
            dval = from_d,
        ));
    }
    if inflated.snapshot.adler != from_d {
        findings.push(format!(
            "the stream's {name} is 0x{got:08x} but the fixture's is 0x{want:08x}",
            got = inflated.snapshot.adler,
            want = from_d,
        ));
    }
}

/// Adds a finding for every way the two sides could disagree about the dictionary handshake.
fn dictionary_findings(
    case: Case,
    dict_id: Option<u64>,
    handshake: DictionaryHandshake,
    findings: &mut Vec<String>,
) {
    let Some(dict_id) = dict_id else {
        // No dictionary in play. `Z_NEED_DICT` must therefore never have been reported.
        if handshake.need_dict_adler.is_some() {
            findings.push(
                "inflate answered Z_NEED_DICT for a stream compressed without a preset dictionary"
                    .to_owned(),
            );
        }
        return;
    };

    if let Some(set) = handshake.set_status {
        if set != Z_OK {
            findings.push(format!(
                "inflateSetDictionary returned {got} rather than Z_OK for the very dictionary the \
                 other side compressed with",
                got = status(set),
            ));
        }
    } else {
        findings.push(
            "the decompressing side never called inflateSetDictionary, so the dictionary path was \
             not exercised at all"
                .to_owned(),
        );
    }

    match case.container {
        // The zlib container is the only one that transmits a dictionary identifier: `DICTID` is a
        // wrapper field (`zlib.h` L640-L643, RFC 1950), so the handshake exists there and nowhere
        // else.
        Container::Zlib => match handshake.need_dict_adler {
            Some(reported) if reported == dict_id => {}
            Some(reported) => findings.push(format!(
                "the dictId handshake failed: the compressor chose 0x{dict_id:08x} and inflate \
                 reported 0x{reported:08x} with Z_NEED_DICT"
            )),
            None => findings.push(
                "inflate never answered Z_NEED_DICT for a zlib stream compressed with a preset \
                 dictionary, so the dictId handshake did not happen"
                    .to_owned(),
            ),
        },
        // Raw and gzip carry no DICTID, so there is no signal and the dictionary must have been
        // installed up front. `Z_NEED_DICT` appearing here would be a defect.
        Container::Raw | Container::Gzip => {
            if handshake.need_dict_adler.is_some() {
                findings.push(format!(
                    "inflate answered Z_NEED_DICT for a {container} stream, which carries no \
                     dictionary identifier (zlib.h:921-926)",
                    container = case.container.name(),
                ));
            }
        }
    }
}

// =================================================================================================
//  Diagnostics
// =================================================================================================

/// How many bytes of context a hex window shows before the offset of interest.
const HEX_WINDOW_BEFORE: usize = 8;

/// How many bytes of context a hex window shows from the offset of interest onwards.
const HEX_WINDOW_AFTER: usize = 8;

/// Where the decompressed bytes first differ from the fixture, and what they look like there.
///
/// The *position* of the first difference is the single most useful triage signal: one in the first
/// few bytes is a header problem, one deep in the payload a symbol-decoding or window problem, and one
/// at the very end a flush or trailer problem.
fn describe_byte_difference(got: &[u8], want: &[u8]) -> String {
    let mut out = String::new();
    let at = got
        .iter()
        .zip(want.iter())
        .position(|(left, right)| left != right);

    if let Some(offset) = at {
        let _ = write!(
            out,
            "DECOMPRESSED BYTES DIFFER at offset {offset} (0x{offset:x}): \
             got 0x{got_byte:02x}, fixture has 0x{want_byte:02x}\n    \
             lengths: got {got_len}, fixture {want_len}\n    \
             got      {got_window}\n    fixture  {want_window}",
            got_byte = got[offset],
            want_byte = want[offset],
            got_len = got.len(),
            want_len = want.len(),
            got_window = hex_window(got, offset),
            want_window = hex_window(want, offset),
        );
    } else {
        let (longer, shorter, whose) = if got.len() > want.len() {
            (got, want, "decompressed output")
        } else {
            (want, got, "fixture")
        };
        let _ = write!(
            out,
            "DECOMPRESSED LENGTHS DIFFER and the shorter is a prefix of the longer: \
             got {got_len}, fixture {want_len}\n    \
             the {whose} has {extra} extra trailing byte(s): {tail}\n    \
             a shortfall points at a truncated final block or a stored-block length, and a surplus \
             at the window being replayed past the end of a match",
            got_len = got.len(),
            want_len = want.len(),
            extra = longer.len() - shorter.len(),
            tail = hex_run(&longer[shorter.len()..]),
        );
    }
    out
}

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

/// A status code as its `zlib.h` name and value, so a log reads `Z_DATA_ERROR(-3)` rather than `-3`.
fn status(code: c_int) -> String {
    let name = match code {
        Z_OK => "Z_OK",
        Z_STREAM_END => "Z_STREAM_END",
        Z_NEED_DICT => "Z_NEED_DICT",
        Z_ERRNO => "Z_ERRNO",
        Z_STREAM_ERROR => "Z_STREAM_ERROR",
        Z_DATA_ERROR => "Z_DATA_ERROR",
        Z_MEM_ERROR => "Z_MEM_ERROR",
        Z_BUF_ERROR => "Z_BUF_ERROR",
        Z_VERSION_ERROR => "Z_VERSION_ERROR",
        _ => "unknown",
    };
    format!("{name}({code})")
}

/// Asserts that both sides answered with the same specific code, and says which code was expected.
///
/// The `expected` argument is what keeps a negative case from passing vacuously: two sides that both
/// answered `Z_STREAM_ERROR` where `Z_DATA_ERROR` was the contract agree with each other and are
/// both wrong, and an `assert_eq!(port, reference)` alone would not notice.
fn assert_status_parity(what: &str, expected: c_int, port: c_int, reference: c_int) {
    assert!(
        port == expected && reference == expected,
        "{what}: expected {want} from both implementations; the port answered {got_port} and the \
         C reference answered {got_reference}",
        want = status(expected),
        got_port = status(port),
        got_reference = status(reference),
    );
}

// =================================================================================================
//  The gzFile layer
//
//  A file is the crossing medium here, which is the one place this suite touches the filesystem. The
//  shape is `test/minigzip.c`'s: `gz_compress` (L369-L389) writes `BUFLEN`-sized pieces and requires
//  `gzwrite` to return exactly what it was given, consulting `gzerror` otherwise, and stops at a
//  zero-length read; `gz_uncompress` (L394-L414) reads `BUFLEN`-sized pieces, treats a negative
//  return as an error and a zero as end of input, and requires `gzclose` to answer `Z_OK`.
//
//  The port's gz layer is reached through the FACADE, never through `zlib-rs` directly: the core has
//  no `gz` feature -- its features are exactly `default`, `rust-api`, `simd` and `std` -- and its `gz`
//  module is gated on `std`, which `libz-rs-sys`'s own `gz = ["zlib-rs/std"]` turns on. Driving the
//  facade is also the right choice for symmetry, because the oracle's `c_gz*` family is the C
//  library's identical surface.
// =================================================================================================

/// `BUFLEN` -- `test/minigzip.c` L144.
const BUFLEN: usize = 16_384;

/// `GZ_SUFFIX` -- `test/minigzip.c` L140. Every scratch file this suite writes carries it.
const GZ_SUFFIX: &str = ".gz";

/// `MAX_NAME_LEN` -- `test/minigzip.c` L145, the buffer the C driver copies a path into.
///
/// Asserted against rather than merely quoted: the C driver would truncate a longer path, so a
/// scratch path that exceeded this would be a name the relinked driver could not open even though this
/// suite could. `std::env::temp_dir()` plus a short unique stem is far inside it on every supported
/// target, and the assertion is what keeps that true if either ever changes.
const MAX_NAME_LEN: usize = 1_024;

/// The internal buffer size both sides are asked for through `gzbuffer`.
///
/// Deliberately not the 8,192-byte default (`gzguts.h` `GZBUFSIZE`), so that the call has an
/// observable effect rather than confirming a value that was already in place.
const GZ_BUFFER_SIZE: c_uint = 8_192 * 2;

/// `SEEK_SET`, which `gzguts.h` L96-L101 defines as 0 when `<stdio.h>` has not supplied it.
const SEEK_SET: c_int = 0;

/// `SEEK_CUR`, likewise 1.
const SEEK_CUR: c_int = 1;

/// Where the read-side probe seeks to, in uncompressed bytes.
///
/// Non-zero and not a buffer boundary, so that the seek genuinely has to decompress forward rather
/// than land on something already in the output buffer.
const GZ_SEEK_TARGET: i64 = 100;

/// The `gzFile` surface this suite drives, implemented once per implementation.
///
/// `Self::File` is a raw pointer in both mirrors, hence `Copy`; every gate takes it by value exactly
/// as the C functions do.
trait GzSide {
    /// How this side is named in failure messages.
    const NAME: &'static str;

    /// This side's `gzFile`. A distinct Rust type from the other's, and never transmuted into it.
    type File: Copy;

    /// `gzopen(path, mode)` -- `zlib.h` L1357.
    fn open(path: &CStr, mode: &CStr) -> Self::File;

    /// `gzdopen(fd, mode)` -- `zlib.h` L1404. Takes ownership of `fd`, which `gzclose` closes.
    fn dopen(fd: c_int, mode: &CStr) -> Self::File;

    /// Whether the handle is `NULL`, which is how both `gzopen` and `gzdopen` report failure.
    fn is_null(file: Self::File) -> bool;

    /// `gzbuffer(file, size)` -- `zlib.h` L1429.
    fn buffer(file: Self::File, size: c_uint) -> c_int;

    /// `gzsetparams(file, level, strategy)` -- `zlib.h` L1445.
    fn setparams(file: Self::File, level: c_int, strategy: c_int) -> c_int;

    /// `gzwrite(file, buf, len)` -- `zlib.h` L1508.
    fn write(file: Self::File, data: &[u8]) -> c_int;

    /// `gzfwrite(buf, size, nitems, file)` -- `zlib.h` L1528.
    fn fwrite(file: Self::File, data: &[u8]) -> usize;

    /// `gzputc(file, c)` -- `zlib.h` L1570.
    fn putc(file: Self::File, byte: u8) -> c_int;

    /// `gzputs(file, s)` -- `zlib.h` L1577.
    fn puts(file: Self::File, text: &CStr) -> c_int;

    /// `gzflush(file, flush)` -- `zlib.h` L1652.
    fn flush(file: Self::File, flush: c_int) -> c_int;

    /// `gzread(file, buf, len)` -- `zlib.h` L1458.
    fn read(file: Self::File, buf: &mut [u8]) -> c_int;

    /// `gzfread(buf, size, nitems, file)` -- `zlib.h` L1481.
    fn fread(file: Self::File, buf: &mut [u8]) -> usize;

    /// `gzgets(file, buf, len)` -- `zlib.h` L1553. `None` for a `NULL` return.
    fn gets(file: Self::File, buf: &mut [u8]) -> Option<Vec<u8>>;

    /// `gzgetc(file)` -- `zlib.h` L1587, the real function rather than the `zlib.h` macro.
    fn getc(file: Self::File) -> c_int;

    /// `gzgetc_(file)` -- `zlib.h` L1961, the macro's fallback, which must exist as a symbol.
    fn getc_(file: Self::File) -> c_int;

    /// `gzungetc(c, file)` -- `zlib.h` L1596.
    fn ungetc(file: Self::File, byte: c_int) -> c_int;

    /// `gzseek(file, offset, whence)` -- `zlib.h` L1673, widened for comparison.
    fn seek(file: Self::File, offset: i64, whence: c_int) -> i64;

    /// `gzseek64(file, offset, whence)` -- the large-file form `zlib.h` L1983 publishes.
    fn seek64(file: Self::File, offset: i64, whence: c_int) -> i64;

    /// `gzrewind(file)` -- `zlib.h` L1663.
    fn rewind(file: Self::File) -> c_int;

    /// `gztell(file)` -- `zlib.h` L1690, widened for comparison.
    fn tell(file: Self::File) -> i64;

    /// `gztell64(file)` -- the large-file form.
    fn tell64(file: Self::File) -> i64;

    /// `gzoffset(file)` -- `zlib.h` L1700, widened for comparison.
    fn offset(file: Self::File) -> i64;

    /// `gzoffset64(file)` -- the large-file form.
    fn offset64(file: Self::File) -> i64;

    /// `gzeof(file)` -- `zlib.h` L1711.
    fn eof(file: Self::File) -> c_int;

    /// `gzdirect(file)` -- `zlib.h` L1726, which is what distinguishes a decompressed stream from a
    /// transparently copied one.
    fn direct(file: Self::File) -> c_int;

    /// `gzerror(file, &errnum)` -- `zlib.h` L1775, as the code and the message together.
    fn error(file: Self::File) -> (c_int, String);

    /// `gzclearerr(file)` -- `zlib.h` L1792.
    fn clearerr(file: Self::File);

    /// `gzclose(file)` -- `zlib.h` L1750. Consumes the handle.
    fn close(file: Self::File) -> c_int;
}

impl GzSide for Port {
    const NAME: &'static str = "port (libz-rs-sys)";
    type File = port::GzFile;

    fn open(path: &CStr, mode: &CStr) -> Self::File {
        // Both arguments cross as `&CStr`, so the gate needs no promise about termination. The
        // library copies the path it keeps (`gzlib.c` L200-L204), so neither has to outlive the
        // call.
        port::GzFile::open(path, mode)
    }

    fn dopen(fd: c_int, mode: &CStr) -> Self::File {
        // `fd` is a descriptor the caller has just obtained and does not use again: the handle
        // takes ownership and `gzclose` closes it (`zlib.h` L1412-L1419), so there is exactly one
        // owner at every moment. That is this side's obligation, not the gate's.
        port::GzFile::dopen(fd, mode)
    }

    fn is_null(file: Self::File) -> bool {
        file.is_null()
    }

    fn buffer(file: Self::File, size: c_uint) -> c_int {
        // Every gate below takes the handle by value exactly as C does, and the obligation that
        // stays on this side is the one no signature can express: `file` is a handle this side
        // opened and has not closed. [`port::GzFile`] is `Copy` precisely so that the suite can
        // exercise the double-close and use-after-close *diagnostics* the library returns, which
        // means the type cannot enforce single use for it.
        file.buffer(size)
    }

    fn setparams(file: Self::File, level: c_int, strategy: c_int) -> c_int {
        // As for `Port::buffer`.
        file.setparams(level, strategy)
    }

    fn write(file: Self::File, data: &[u8]) -> c_int {
        // As for `Port::buffer`. `data` crosses as a slice, and the library copies it into its own
        // buffer rather than retaining what it was given.
        file.write(data)
    }

    fn fwrite(file: Self::File, data: &[u8]) -> usize {
        // As for `Port::write`. The gate passes `size` 1 and `nitems` the length, so the product
        // cannot overflow and the region read is exactly `data`.
        file.fwrite(data)
    }

    fn putc(file: Self::File, byte: u8) -> c_int {
        // As for `Port::buffer`.
        file.putc(c_int::from(byte))
    }

    fn puts(file: Self::File, text: &CStr) -> c_int {
        // As for `Port::buffer`. `text` crosses as a `&CStr`, which is where its termination and
        // its read-only treatment come from.
        file.puts(text)
    }

    fn flush(file: Self::File, flush: c_int) -> c_int {
        // As for `Port::buffer`.
        file.flush(flush)
    }

    fn read(file: Self::File, buf: &mut [u8]) -> c_int {
        // As for `Port::buffer`. `buf` crosses as a mutable slice, so its length and its writable
        // extent are the same fact.
        file.read(buf)
    }

    fn fread(file: Self::File, buf: &mut [u8]) -> usize {
        // As for `Port::read`, with `size` 1 and `nitems` the length.
        file.fread(buf)
    }

    fn gets(file: Self::File, buf: &mut [u8]) -> Option<Vec<u8>> {
        // As for `Port::read`; `gzgets` writes at most `len` bytes including the terminating NUL
        // (`zlib.h` L1555-L1563), and `len` is the slice's length.
        // The gate reads the NUL-terminated string the library wrote into `buf` and answers `None`
        // for the null return, so the two outcomes `zlib.h` L1553-L1563 distinguishes arrive as
        // `Some` and `None` rather than as a pointer to be tested here.
        file.gets(buf)
    }

    fn getc(file: Self::File) -> c_int {
        // As for `Port::buffer`.
        file.getc()
    }

    fn getc_(file: Self::File) -> c_int {
        // As for `Port::buffer`.
        file.getc_()
    }

    fn ungetc(file: Self::File, byte: c_int) -> c_int {
        // As for `Port::buffer`.
        file.ungetc(byte)
    }

    fn seek(file: Self::File, offset: i64, whence: c_int) -> i64 {
        // As for `Port::buffer`.
        widen_off(file.seek(narrow_off(offset), whence))
    }

    fn seek64(file: Self::File, offset: i64, whence: c_int) -> i64 {
        // As for `Port::buffer`.
        file.seek64(offset, whence)
    }

    fn rewind(file: Self::File) -> c_int {
        // As for `Port::buffer`.
        file.rewind()
    }

    fn tell(file: Self::File) -> i64 {
        // As for `Port::buffer`.
        widen_off(file.tell())
    }

    fn tell64(file: Self::File) -> i64 {
        // As for `Port::buffer`.
        file.tell64()
    }

    fn offset(file: Self::File) -> i64 {
        // As for `Port::buffer`.
        widen_off(file.offset())
    }

    fn offset64(file: Self::File) -> i64 {
        // As for `Port::buffer`.
        file.offset64()
    }

    fn eof(file: Self::File) -> c_int {
        // As for `Port::buffer`.
        file.eof()
    }

    fn direct(file: Self::File) -> c_int {
        // As for `Port::buffer`.
        file.direct()
    }

    fn error(file: Self::File) -> (c_int, String) {
        // The gate seeds `errnum` with the sentinel and returns it with the message bytes, so "the
        // library wrote nothing" stays distinguishable from "the library wrote zero".
        let (errnum, message) = file.error(GZ_ERRNUM_SENTINEL);
        (errnum, gz_message(message.as_deref()))
    }

    fn clearerr(file: Self::File) {
        // As for `Port::buffer`.
        file.clearerr();
    }

    fn close(file: Self::File) -> c_int {
        // As for `Port::buffer`, and this is the call the handle-liveness obligation turns on: it
        // consumes the handle, so no gate is called with it afterwards except where a test is
        // deliberately measuring what a second close reports.
        file.close()
    }
}

impl GzSide for Reference {
    const NAME: &'static str = "C reference (oracle)";
    type File = oracle::GzFile;

    fn open(path: &CStr, mode: &CStr) -> Self::File {
        // As for `Port::open`.
        oracle::GzFile::open(path, mode)
    }

    fn dopen(fd: c_int, mode: &CStr) -> Self::File {
        // As for `Port::dopen`, including the single ownership of `fd` that this handle's
        // `gzclose` discharges.
        oracle::GzFile::dopen(fd, mode)
    }

    fn is_null(file: Self::File) -> bool {
        file.is_null()
    }

    fn buffer(file: Self::File, size: c_uint) -> c_int {
        // As for `Port::buffer`, including the handle-liveness obligation that stays here.
        file.buffer(size)
    }

    fn setparams(file: Self::File, level: c_int, strategy: c_int) -> c_int {
        // As for `Reference::buffer`.
        file.setparams(level, strategy)
    }

    fn write(file: Self::File, data: &[u8]) -> c_int {
        // As for `Port::write`.
        file.write(data)
    }

    fn fwrite(file: Self::File, data: &[u8]) -> usize {
        // As for `Port::fwrite`.
        file.fwrite(data)
    }

    fn putc(file: Self::File, byte: u8) -> c_int {
        // As for `Reference::buffer`.
        file.putc(c_int::from(byte))
    }

    fn puts(file: Self::File, text: &CStr) -> c_int {
        // As for `Port::puts`.
        file.puts(text)
    }

    fn flush(file: Self::File, flush: c_int) -> c_int {
        // As for `Reference::buffer`.
        file.flush(flush)
    }

    fn read(file: Self::File, buf: &mut [u8]) -> c_int {
        // As for `Port::read`.
        file.read(buf)
    }

    fn fread(file: Self::File, buf: &mut [u8]) -> usize {
        // As for `Port::fread`.
        file.fread(buf)
    }

    fn gets(file: Self::File, buf: &mut [u8]) -> Option<Vec<u8>> {
        // As for `Port::gets`.
        file.gets(buf)
    }

    fn getc(file: Self::File) -> c_int {
        // As for `Reference::buffer`. This is the real function: `gzread.c` undefines the
        // `zlib.h` macro before defining it, which is why `build.rs` has to rename it with `objcopy`
        // rather than with a `-D` flag.
        file.getc()
    }

    fn getc_(file: Self::File) -> c_int {
        // As for `Reference::buffer`.
        file.getc_()
    }

    fn ungetc(file: Self::File, byte: c_int) -> c_int {
        // As for `Reference::buffer`.
        file.ungetc(byte)
    }

    fn seek(file: Self::File, offset: i64, whence: c_int) -> i64 {
        // As for `Reference::buffer`.
        widen_off_oracle(file.seek(narrow_off_oracle(offset), whence))
    }

    fn seek64(file: Self::File, offset: i64, whence: c_int) -> i64 {
        // As for `Reference::buffer`.
        file.seek64(offset, whence)
    }

    fn rewind(file: Self::File) -> c_int {
        // As for `Reference::buffer`.
        file.rewind()
    }

    fn tell(file: Self::File) -> i64 {
        // As for `Reference::buffer`.
        widen_off_oracle(file.tell())
    }

    fn tell64(file: Self::File) -> i64 {
        // As for `Reference::buffer`.
        file.tell64()
    }

    fn offset(file: Self::File) -> i64 {
        // As for `Reference::buffer`.
        widen_off_oracle(file.offset())
    }

    fn offset64(file: Self::File) -> i64 {
        // As for `Reference::buffer`.
        file.offset64()
    }

    fn eof(file: Self::File) -> c_int {
        // As for `Reference::buffer`.
        file.eof()
    }

    fn direct(file: Self::File) -> c_int {
        // As for `Reference::buffer`.
        file.direct()
    }

    fn error(file: Self::File) -> (c_int, String) {
        // As for `Port::error`, including the sentinel.
        let (errnum, message) = file.error(GZ_ERRNUM_SENTINEL);
        (errnum, gz_message(message.as_deref()))
    }

    fn clearerr(file: Self::File) {
        // As for `Reference::buffer`.
        file.clearerr();
    }

    fn close(file: Self::File) -> c_int {
        // As for `Port::close`.
        file.close()
    }
}

// =================================================================================================
//  The gz drivers
// =================================================================================================

/// Everything one side's write pass reports, so that the two can be compared field for field.
///
/// Derives `PartialEq` on purpose: the assertion the brief asks for is "compare the return values and
/// the post-call state between the two implementations", and that is one `assert_eq!` over this type
/// rather than twenty over its members.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GzWriteReport {
    /// What `gzbuffer` returned. Must be `Z_OK`: it is called before any write, which is the only
    /// time `zlib.h` L1436-L1440 permits it.
    buffer: c_int,
    /// What `gzsetparams` returned.
    setparams: c_int,
    /// What `gzputc` returned: the character written, or -1.
    putc: c_int,
    /// What `gzputs` returned: the character count, excluding the terminating NUL.
    puts: c_int,
    /// What `gzfwrite` returned: items written, which with `size == 1` is bytes.
    fwrite: usize,
    /// Every `gzwrite` return, in order. `test/minigzip.c` L385 requires each to equal what was
    /// offered.
    writes: Vec<c_int>,
    /// What `gzflush(Z_SYNC_FLUSH)` returned mid-stream.
    flush: c_int,
    /// `gztell` and `gztell64` before the close: the uncompressed offset, which on a write stream is
    /// everything handed over so far.
    tell: (i64, i64),
    /// `gzoffset` and `gzoffset64` before the close: the *compressed* offset.
    offset: (i64, i64),
    /// `gzeof` on a write stream, which is never true.
    eof: c_int,
    /// `gzdirect` on a write stream, which `zlib.h` L1735-L1738 documents as true.
    direct: c_int,
    /// `gzerror`'s code and message with nothing wrong.
    error: (c_int, String),
    /// What `gzclose` returned. `test/minigzip.c` L388 requires `Z_OK`.
    close: c_int,
    /// How many bytes the finished file holds.
    file_len: u64,
}

/// Everything one side's read pass reports.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GzReadReport {
    /// `gzdirect` immediately after opening: 0 for a gzip stream, 1 for a transparently copied one.
    direct_before_read: c_int,
    /// Every `gzread` return, in order, `test/minigzip.c` L403-L405's loop shape.
    reads: Vec<c_int>,
    /// How many bytes came out in total.
    total: usize,
    /// What `gzfread` returned for the first pass after a rewind.
    fread: usize,
    /// `gzeof` once the reads have run out, which is 1.
    eof_at_end: c_int,
    /// `gztell` and `gztell64` at end of stream.
    tell_at_end: (i64, i64),
    /// `gzoffset` and `gzoffset64` at end of stream.
    offset_at_end: (i64, i64),
    /// What `gzrewind` returned.
    rewind: c_int,
    /// What `gzgetc` returned for the first byte after the rewind.
    getc: c_int,
    /// What `gzungetc` returned when that byte was pushed back.
    ungetc: c_int,
    /// What `gzgetc_` -- the macro's out-of-line fallback -- returned for the byte pushed back.
    getc_: c_int,
    /// What `gzseek(GZ_SEEK_TARGET, SEEK_SET)` returned.
    seek: i64,
    /// What `gzseek64(0, SEEK_CUR)` returned immediately afterwards, which must agree with it.
    seek_cur: i64,
    /// What `gzgets` returned after the seek: the bytes, or `None` for a `NULL`.
    gets: Option<Vec<u8>>,
    /// `gzerror`'s code and message with nothing wrong.
    error: (c_int, String),
    /// `gzeof` after `gzclearerr`, which clears it on a read stream.
    eof_after_clearerr: c_int,
    /// What `gzclose` returned. `test/minigzip.c` L413 requires `Z_OK`.
    close: c_int,
}

/// Writes `payload` to `path` through one side's whole write surface, `test/minigzip.c`-style.
///
/// The order of operations is fixed here rather than at the call site precisely so that both sides
/// execute it identically: `gzbuffer` before any write because that is the only time it is legal,
/// then `gzsetparams`, then the single-byte and string entry points, then the `BUFLEN` loop with a
/// mid-stream `gzflush`, then the accessors, then exactly one `gzclose`.
fn gz_write_pass<S: GzSide>(path: &CStr, payload: &[u8]) -> GzWriteReport {
    let file = S::open(path, c"wb9");
    assert!(
        !S::is_null(file),
        "{side}: gzopen for writing returned NULL",
        side = <S as GzSide>::NAME,
    );

    // `gzbuffer` is legal only before the first read or write (`zlib.h` L1436-L1440), so it is first.
    let buffer = S::buffer(file, GZ_BUFFER_SIZE);
    let setparams = S::setparams(file, Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY);

    // One byte, then a string, then a counted write, so that all three write entry points contribute
    // to the same stream and the reader has to reproduce their concatenation.
    let put_byte = S::putc(file, GZ_PREFIX_BYTE);
    let put_text = S::puts(file, GZ_PREFIX_TEXT);
    let fwrite = S::fwrite(file, GZ_PREFIX_ITEMS);

    // `test/minigzip.c` L380-L386: `BUFLEN`-sized pieces, each of which `gzwrite` must accept whole.
    let mut writes = Vec::new();
    let mut flush = Z_OK;
    let mut flushed = false;
    let mut offset = 0_usize;
    while offset < payload.len() {
        let take = BUFLEN.min(payload.len() - offset);
        let written = S::write(file, &payload[offset..offset + take]);
        writes.push(written);
        assert!(
            written == narrow_int(take),
            "{side}: gzwrite accepted {written} of {take} bytes -- {error}",
            side = <S as GzSide>::NAME,
            error = gz_error_text::<S>(file),
        );
        offset += take;
        // Exactly one mid-stream flush, after the first piece, so that the reader has to cope with a
        // sync point inside the stream as well as at its end.
        if !flushed {
            flush = S::flush(file, Z_SYNC_FLUSH);
            flushed = true;
        }
    }
    if !flushed {
        // An empty payload never entered the loop. The flush still has to happen, or the case would
        // silently stop measuring `gzflush` for `empty.bin`.
        flush = S::flush(file, Z_SYNC_FLUSH);
    }

    let report = GzWriteReport {
        buffer,
        setparams,
        putc: put_byte,
        puts: put_text,
        fwrite,
        writes,
        flush,
        tell: (S::tell(file), S::tell64(file)),
        offset: (S::offset(file), S::offset64(file)),
        eof: S::eof(file),
        direct: S::direct(file),
        error: S::error(file),
        close: S::close(file),
        // Read after the close, which is when the file is complete.
        file_len: 0,
    };

    let file_len = std::fs::metadata(Path::new(
        path.to_str().expect("a scratch path is valid UTF-8"),
    ))
    .expect("the written file exists")
    .len();

    GzWriteReport { file_len, ..report }
}

/// The byte `gzputc` contributes to every written stream.
const GZ_PREFIX_BYTE: u8 = b'X';

/// The string `gzputs` contributes, which is `test/example.c`'s payload minus its NUL.
const GZ_PREFIX_TEXT: &CStr = c"hello, hello!";

/// The bytes `gzfwrite` contributes.
const GZ_PREFIX_ITEMS: &[u8] = b"|items|";

/// Everything the three write entry points contribute, in the order they are called.
///
/// The reader must see this immediately followed by the payload, which is what makes the three
/// entry points' interaction observable rather than merely their return values.
fn gz_prefix() -> Vec<u8> {
    let mut prefix = vec![GZ_PREFIX_BYTE];
    prefix.extend_from_slice(GZ_PREFIX_TEXT.to_bytes());
    prefix.extend_from_slice(GZ_PREFIX_ITEMS);
    prefix
}

/// `gzerror` rendered for a failure message, exactly as `test/minigzip.c` L385 consults it.
fn gz_error_text<S: GzSide>(file: S::File) -> String {
    let (code, message) = S::error(file);
    format!("gzerror: {status} {message:?}", status = status(code))
}

/// Reads the whole of `path` through one side's read surface and reports the bytes and the state.
///
/// The order is fixed for the same reason `gz_write_pass`'s is. After the payload has been drained
/// the pass deliberately keeps going -- rewind, `gzgetc`, `gzungetc`, `gzgetc_`, `gzseek`, `gzgets`,
/// `gzclearerr` -- because the accessors are where a port is most likely to differ and least likely
/// to be exercised by a plain decompression loop.
fn gz_read_pass<S: GzSide>(path: &CStr, capacity_hint: usize) -> (Vec<u8>, GzReadReport) {
    let file = S::open(path, c"rb");
    assert!(
        !S::is_null(file),
        "{side}: gzopen for reading returned NULL",
        side = <S as GzSide>::NAME,
    );

    let direct_before_read = S::direct(file);

    // `test/minigzip.c` L400-L406: read `BUFLEN` at a time, a negative return is an error and a zero
    // is end of input.
    let mut bytes = Vec::with_capacity(capacity_hint);
    let mut reads = Vec::new();
    let mut buf = vec![0_u8; BUFLEN];
    loop {
        let read = S::read(file, &mut buf);
        reads.push(read);
        assert!(
            read >= 0,
            "{side}: gzread returned {read} -- {error}",
            side = <S as GzSide>::NAME,
            error = gz_error_text::<S>(file),
        );
        if read == 0 {
            break;
        }
        let read = usize::try_from(read).expect("a non-negative gzread return fits in a usize");
        bytes.extend_from_slice(&buf[..read]);
    }

    let eof_at_end = S::eof(file);
    let tell_at_end = (S::tell(file), S::tell64(file));
    let offset_at_end = (S::offset(file), S::offset64(file));

    // Back to the start, and then through the byte-at-a-time surface.
    let rewind = S::rewind(file);
    let getc = S::getc(file);
    let ungetc = S::ungetc(file, getc);
    let getc_ = S::getc_(file);
    let mut fread_buf = vec![0_u8; GZ_FREAD_ITEMS];
    let fread = S::fread(file, &mut fread_buf);

    // A seek forward, then a zero-length relative seek, which must report the same position.
    let seek = S::seek(file, GZ_SEEK_TARGET, SEEK_SET);
    let seek_cur = S::seek64(file, 0, SEEK_CUR);
    let mut line = vec![0_u8; GZ_GETS_BUFFER];
    let gets = S::gets(file, &mut line);

    let error = S::error(file);
    S::clearerr(file);
    let eof_after_clearerr = S::eof(file);
    let close = S::close(file);
    let total = bytes.len();

    (
        bytes,
        GzReadReport {
            direct_before_read,
            reads,
            total,
            fread,
            eof_at_end,
            tell_at_end,
            offset_at_end,
            rewind,
            getc,
            ungetc,
            getc_,
            seek,
            seek_cur,
            gets,
            error,
            eof_after_clearerr,
            close,
        },
    )
}

/// How many bytes the read pass asks `gzfread` for.
const GZ_FREAD_ITEMS: usize = 5;

/// How large a buffer the read pass hands `gzgets`.
///
/// Small on purpose: `gzgets` stops at a newline **or** at `len - 1` bytes (`zlib.h` L1555-L1563), and
/// most fixtures contain no newline at all, so the length-limited exit is the one that matters and a
/// generous buffer would not reach it.
const GZ_GETS_BUFFER: usize = 16;

// =================================================================================================
//  Sweeps
// =================================================================================================

/// Runs one crossing in whichever direction `case` names.
///
/// The two `Side` implementations are chosen here and nowhere else, which is what keeps every test
/// below able to iterate [`DIRECTIONS`] rather than duplicating itself.
fn run_case(case: Case, input: &[u8], dictionary: Option<&[u8]>) {
    match case.direction {
        Direction::CToRust => cross::<Reference, Port>(case, input, dictionary),
        Direction::RustToC => cross::<Port, Reference>(case, input, dictionary),
    }
}

/// One case with both `windowBits` derived from the container and the same window size.
fn matched_case(
    direction: Direction,
    container: Container,
    bits: c_int,
    level: c_int,
    chunking: Chunking,
    fixture: &'static str,
) -> Case {
    Case {
        direction,
        container,
        deflate_bits: container.deflate_window_bits(bits),
        inflate_bits: container.inflate_window_bits(bits),
        level,
        chunking,
        fixture,
        dictionary: false,
    }
}

/// Crosses every fixture, in every container, at every chunking appropriate to its size.
///
/// Returns how many crossings ran, so that a green run reports how much it measured rather than
/// merely that it passed -- a sweep that silently stopped enumerating would otherwise look exactly
/// like one that succeeded.
fn sweep_every_fixture(direction: Direction, corpus: &[Sample]) -> usize {
    let mut crossings = 0;
    for sample in corpus {
        for &container in CONTAINERS {
            for &chunking in chunkings_for(sample.bytes.len()) {
                let case = matched_case(
                    direction,
                    container,
                    DEFAULT_WINDOW_SIZE,
                    DEFAULT_LEVEL,
                    chunking,
                    sample.fixture.name,
                );
                run_case(case, &sample.bytes, None);
                crossings += 1;
            }
        }
    }
    crossings
}

// =================================================================================================
//  The tests -- the plumbing anchor
// =================================================================================================

/// The plumbing anchor: the fixtures load and both sides agree with the numbers measured during
/// planning.
///
/// Run this first when something looks wrong. It is deliberately tiny and deliberately quotes external
/// constants -- `zlibVersion() == "1.3.2.1-motley"`, `compressBound(14) == 27`, a 19-byte level-9 zlib
/// stream for the 14-byte `hello, hello!\0` payload, `get_crc_table()[1] == 0x77073096` and the
/// dictionary's Adler-32 of `0x08410215` -- all of which were measured from a C driver linked against
/// this very oracle. A failure here is a fixture-loading or linkage problem rather than an
/// interoperability one, and there is no point triaging a crossing until it passes.
#[test]
fn plumbing_anchor_matches_the_measured_numbers() {
    let corpus = load_corpus();
    assert!(
        corpus.len() == FIXTURES.len(),
        "the corpus loader returned {got} samples for {want} fixtures",
        got = corpus.len(),
        want = FIXTURES.len(),
    );

    // Both implementations must claim the same version, and specifically the in-tree one: substituting
    // a shorthand such as "1.3.2" would break every caller's `deflateInit_` check (`zlib.h` L248).
    let (port_version, reference_version) = (Port::version(), Reference::version());
    assert!(
        port_version == reference_version && reference_version == ANCHOR_VERSION,
        "zlibVersion must be {ANCHOR_VERSION:?} on both sides; the port says {port_version:?} and \
         the C reference says {reference_version:?}"
    );

    let hello = sample(&corpus, HELLO_FIXTURE);
    assert!(
        hello.bytes == ANCHOR_HELLO,
        "hello.bin must be exactly test/example.c's payload plus its NUL; it is {got:?}",
        got = hello.bytes,
    );

    let (port_bound, reference_bound) = (
        Port::compress_bound(hello.bytes.len()),
        Reference::compress_bound(hello.bytes.len()),
    );
    assert!(
        port_bound == reference_bound && reference_bound == ANCHOR_BOUND,
        "compressBound({len}) must be {ANCHOR_BOUND} on both sides; the port says {port_bound} and \
         the C reference says {reference_bound}",
        len = hello.bytes.len(),
    );

    // The oracle's own compress-then-uncompress, which is the exact figure the planning driver
    // printed. This is the one place a same-side round trip appears in this file, and it is here
    // BECAUSE it is not an interoperability assertion: it is the smoke check that the archive is
    // linked and the `c_` renaming worked, which every crossing below depends on.
    let mut compressed = vec![0_u8; usize::try_from(reference_bound).unwrap()];
    let deflated = Reference::compress2(&mut compressed, &hello.bytes, Z_BEST_COMPRESSION);
    assert!(
        deflated.status == Z_OK,
        "the oracle's compress2 returned {got}",
        got = status(deflated.status),
    );
    assert!(
        deflated.dest_len == ANCHOR_COMPRESSED_LEN,
        "the oracle must compress hello.bin to {ANCHOR_COMPRESSED_LEN} bytes at level 9 and \
         produced {got}",
        got = deflated.dest_len,
    );
    let clen = usize::try_from(deflated.dest_len).unwrap();
    let mut round_tripped = vec![0_u8; hello.bytes.len()];
    let inflated = Reference::uncompress2(&mut round_tripped, &compressed[..clen]);
    assert!(
        inflated.status == Z_OK,
        "the oracle's uncompress2 returned {got}",
        got = status(inflated.status),
    );
    assert!(
        round_tripped == hello.bytes,
        "the oracle's own round trip produced {round_tripped:?}"
    );

    // The generated CRC table, which `tests/table_equality.rs` compares in full. One entry here is
    // enough to prove the linkage; the point is the anchor, not the table.
    let (port_entry, reference_entry) = (Port::crc_table_entry(1), Reference::crc_table_entry(1));
    assert!(
        port_entry == reference_entry && reference_entry == CRC_TABLE_ENTRY_1,
        "get_crc_table()[1] must be 0x{CRC_TABLE_ENTRY_1:08x} on both sides; the port says \
         0x{port_entry:08x} and the C reference says 0x{reference_entry:08x}"
    );

    // The dictionary fixture and its pinned identifier. `corpus/README.md` records both candidate
    // lengths and their checksums, and 6 bytes -- `hello\0` -- is the contract.
    let dictionary = sample(&corpus, DICTIONARY_FIXTURE);
    assert!(
        dictionary.bytes == ANCHOR_DICTIONARY,
        "dictionary.bin must be test/example.c's preset plus its NUL; it is {got:?}",
        got = dictionary.bytes,
    );
    let (port_adler, reference_adler) = (
        Port::adler32(&dictionary.bytes),
        Reference::adler32(&dictionary.bytes),
    );
    assert!(
        port_adler == reference_adler && reference_adler == DICTIONARY_ADLER,
        "the dictionary's Adler-32 must be 0x{DICTIONARY_ADLER:08x} on both sides; the port says \
         0x{port_adler:08x} and the C reference says 0x{reference_adler:08x}"
    );

    println!(
        "anchor: version={reference_version} compressBound(14)={reference_bound} \
         level-9 length={clen} crc_table[1]=0x{reference_entry:08x} \
         dictId=0x{reference_adler:08x}"
    );
}

/// `ZLIB_VERSION` as the tree self-identifies (`zlib.h` L44).
const ANCHOR_VERSION: &str = "1.3.2.1-motley";

/// `test/example.c` L35's payload, plus the NUL the suite compresses along with it.
const ANCHOR_HELLO: &[u8] = b"hello, hello!\0";

/// `test/example.c` L40's preset dictionary, plus its NUL -- `sizeof(dictionary)` is 6.
const ANCHOR_DICTIONARY: &[u8] = b"hello\0";

/// `compressBound(14)`, measured.
const ANCHOR_BOUND: u64 = 27;

/// The length of the level-9 zlib stream for `hello.bin`, measured.
const ANCHOR_COMPRESSED_LEN: u64 = 19;

/// The Adler-32 of the 6-byte dictionary, which is the `dictId` a zlib stream transmits.
const DICTIONARY_ADLER: u64 = 0x0841_0215;

// =================================================================================================
//  The tests -- the four directions
// =================================================================================================

/// Direction 1: the C reference compresses, the port decompresses, over every fixture, every
/// container and every chunking.
#[test]
fn c_deflate_to_rust_inflate_reproduces_every_fixture() {
    let corpus = load_corpus();
    let crossings = sweep_every_fixture(Direction::CToRust, &corpus);
    println!("C deflate -> Rust inflate: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// Direction 2: the port compresses, the C reference decompresses, over the same matrix.
#[test]
fn rust_deflate_to_c_inflate_reproduces_every_fixture() {
    let corpus = load_corpus();
    let crossings = sweep_every_fixture(Direction::RustToC, &corpus);
    println!("Rust deflate -> C inflate: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// Every compression level crosses in both directions, in every container.
///
/// Level is swept separately from the fixture matrix because the two are independent here: what a
/// level changes is which symbols the encoder chooses, and a decoder that handles level 6 correctly
/// can still mishandle level 0's stored blocks or level 9's long matches.
#[test]
fn every_level_crosses_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &level in LEVELS {
            for &container in CONTAINERS {
                for name in LEVEL_SWEEP_FIXTURES {
                    let sample = sample(&corpus, name);
                    let case = matched_case(
                        direction,
                        container,
                        DEFAULT_WINDOW_SIZE,
                        level,
                        SINGLE_SHOT,
                        sample.fixture.name,
                    );
                    run_case(case, &sample.bytes, None);
                    crossings += 1;
                }
            }
        }
    }
    println!("level sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// The fixtures the level and window sweeps use: one highly compressible, one incompressible, one
/// tiny, and one that crosses the window boundary.
///
/// Chosen rather than exhaustive because level and window size interact with *match selection*, and
/// these four span the interesting cases: long matches, no matches, a degenerate Huffman tree, and a
/// window slide.
const LEVEL_SWEEP_FIXTURES: &[&str] = &[
    "repetitive.bin",
    "random.bin",
    HELLO_FIXTURE,
    WINDOW_BOUNDARY_FIXTURE,
];

/// Every window size crosses in both directions, in every container, with the two sides matched.
#[test]
fn every_window_size_crosses_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &bits in WINDOW_SIZES {
            for &container in CONTAINERS {
                for name in LEVEL_SWEEP_FIXTURES {
                    let sample = sample(&corpus, name);
                    let case = matched_case(
                        direction,
                        container,
                        bits,
                        DEFAULT_LEVEL,
                        SINGLE_SHOT,
                        sample.fixture.name,
                    );
                    run_case(case, &sample.bytes, None);
                    crossings += 1;
                }
            }
        }
    }
    println!("window-size sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// A decoder window **larger** than the encoder's is legal and must still cross.
///
/// `zlib.h` L865-L869 requires the inflate side to be greater than or equal to the deflate side, so
/// the strictly-greater case is as much part of the contract as the equal one -- and it is the case a
/// real caller hits, because `inflateInit` defaults to 15 whatever the producer chose.
#[test]
fn a_larger_decoder_window_crosses_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &container in CONTAINERS {
            for &bits in &[9, 10, 12] {
                let sample = sample(&corpus, WINDOW_BOUNDARY_FIXTURE);
                let case = Case {
                    direction,
                    container,
                    deflate_bits: container.deflate_window_bits(bits),
                    inflate_bits: container.inflate_window_bits(DEFAULT_WINDOW_SIZE),
                    level: DEFAULT_LEVEL,
                    chunking: SINGLE_SHOT,
                    fixture: sample.fixture.name,
                    dictionary: false,
                };
                run_case(case, &sample.bytes, None);
                crossings += 1;
            }
        }
    }
    println!("larger-decoder-window sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// `windowBits == 0` -- "take the window size from the zlib header" -- crosses in both directions.
///
/// `zlib.h` L875-L876. Only the zlib container has a header to read it from, which is why this case
/// exists for that container alone.
#[test]
fn window_bits_from_the_header_crosses_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &bits in WINDOW_SIZES {
            let sample = sample(&corpus, "repetitive.bin");
            let case = Case {
                direction,
                container: Container::Zlib,
                deflate_bits: Container::Zlib.deflate_window_bits(bits),
                inflate_bits: WINDOW_BITS_FROM_HEADER,
                level: DEFAULT_LEVEL,
                chunking: SINGLE_SHOT,
                fixture: sample.fixture.name,
                dictionary: false,
            };
            run_case(case, &sample.bytes, None);
            crossings += 1;
        }
    }
    println!("windowBits==0 sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// `+32` -- automatic zlib-or-gzip detection -- crosses for **both** wrapped containers.
///
/// `zlib.h` L889-L891. This is the setting most real callers use, so a port that got it wrong would
/// break almost everything while passing every fixed-container case.
#[test]
fn automatic_header_detection_crosses_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &container in &[Container::Zlib, Container::Gzip] {
            for name in LEVEL_SWEEP_FIXTURES {
                let sample = sample(&corpus, name);
                let case = Case {
                    direction,
                    container,
                    deflate_bits: container.deflate_window_bits(DEFAULT_WINDOW_SIZE),
                    inflate_bits: DEFAULT_WINDOW_SIZE + AUTODETECT_ADDEND,
                    level: DEFAULT_LEVEL,
                    chunking: SINGLE_SHOT,
                    fixture: sample.fixture.name,
                    dictionary: false,
                };
                run_case(case, &sample.bytes, None);
                crossings += 1;
            }
        }
    }
    println!("autodetect sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// `+16` -- gzip only -- crosses a gzip stream and **refuses a zlib one identically**.
///
/// `zlib.h` L889-L893 states outright that the zlib format then returns `Z_DATA_ERROR`, so both halves
/// are part of the contract and both are asserted: the positive half in both directions, and the
/// negative half as a status-parity case with the specific expected code.
#[test]
fn gzip_only_decoding_accepts_gzip_and_refuses_zlib_identically() {
    let corpus = load_corpus();
    let sample = sample(&corpus, HELLO_FIXTURE);

    // The positive half: a gzip stream, both directions.
    for &direction in DIRECTIONS {
        let case = Case {
            direction,
            container: Container::Gzip,
            deflate_bits: Container::Gzip.deflate_window_bits(DEFAULT_WINDOW_SIZE),
            inflate_bits: DEFAULT_WINDOW_SIZE + GZIP_ONLY_ADDEND,
            level: DEFAULT_LEVEL,
            chunking: SINGLE_SHOT,
            fixture: sample.fixture.name,
            dictionary: false,
        };
        run_case(case, &sample.bytes, None);
    }

    // The negative half. Each side compresses a zlib stream and BOTH sides are asked to decode it
    // with gzip-only decoding, so the refusal is measured for both implementations against both
    // producers -- four combinations, all of which must answer Z_DATA_ERROR.
    let gzip_only = DEFAULT_WINDOW_SIZE + GZIP_ONLY_ADDEND;
    for &producer in DIRECTIONS {
        let zlib_stream = stream_from(producer, &sample.bytes, DEFAULT_LEVEL, DEFAULT_WINDOW_SIZE);
        let port = inflate_run::<Port>(&zlib_stream, gzip_only, None, SINGLE_SHOT)
            .0
            .final_status;
        let reference = inflate_run::<Reference>(&zlib_stream, gzip_only, None, SINGLE_SHOT)
            .0
            .final_status;
        assert_status_parity(
            &format!(
                "gzip-only inflate (windowBits {gzip_only}) of a zlib stream written by the \
                 {producer:?} side"
            ),
            Z_DATA_ERROR,
            port,
            reference,
        );
    }
}

/// Compresses `input` on whichever side `producer` names, and hands back the stream.
///
/// The counterpart to [`run_case`] for the tests that need the stream itself rather than a completed
/// crossing -- the negative cases, which feed one side's output to *both* decoders so that each
/// implementation's refusal is measured against each implementation's producer.
fn stream_from(producer: Direction, input: &[u8], level: c_int, window_bits: c_int) -> Vec<u8> {
    let run = match producer {
        Direction::CToRust => {
            deflate_run::<Reference>(input, level, window_bits, None, SINGLE_SHOT).0
        }
        Direction::RustToC => deflate_run::<Port>(input, level, window_bits, None, SINGLE_SHOT).0,
    };
    assert!(
        run.final_status == Z_STREAM_END && run.end == Z_OK,
        "the producing side could not write the stream a negative case needs: deflate answered \
         {deflated} and deflateEnd answered {ended}",
        deflated = status(run.final_status),
        ended = status(run.end),
    );
    run.output
}

/// A decoder window **smaller** than the encoder's is refused identically -- for the zlib container.
///
/// `zlib.h` L871-L873: `inflate()` returns `Z_DATA_ERROR` rather than trying to allocate a larger
/// window. The zlib container is where that is detectable *at the header*, because RFC 1950's `CMF`
/// byte records the window size and `inflate.c` compares it against `state->wbits` before decoding a
/// single symbol -- which is why the refusal here is immediate and independent of the output window.
/// Both producers are used so that both implementations' decoders are measured, and the expected code
/// is named so the case cannot pass on two sides agreeing about the wrong one.
#[test]
fn an_undersized_decoder_window_is_refused_identically() {
    let corpus = load_corpus();
    let sample = sample(&corpus, WINDOW_BOUNDARY_FIXTURE);
    let deflate_bits = Container::Zlib.deflate_window_bits(DEFAULT_WINDOW_SIZE);
    let inflate_bits = Container::Zlib.inflate_window_bits(UNDERSIZED_WINDOW_SIZE);

    for &producer in DIRECTIONS {
        let stream = stream_from(producer, &sample.bytes, DEFAULT_LEVEL, deflate_bits);
        for &chunking in CHUNKINGS_LARGE {
            let port = inflate_run::<Port>(&stream, inflate_bits, None, chunking)
                .0
                .final_status;
            let reference = inflate_run::<Reference>(&stream, inflate_bits, None, chunking)
                .0
                .final_status;
            assert_status_parity(
                &format!(
                    "inflate of a zlib stream written at windowBits {deflate_bits} by the \
                     {producer:?} side, read at windowBits {inflate_bits}, chunking {chunking}",
                    chunking = chunking.name,
                ),
                Z_DATA_ERROR,
                port,
                reference,
            );
        }
    }
}

/// The window size the undersized-window cases decode with: as small as `inflateInit2_` allows.
const UNDERSIZED_WINDOW_SIZE: c_int = 9;

/// Raw and gzip carry no window size, so an undersized decoder window is detected only when the
/// decoder actually needs its window -- and both implementations draw that line in the same place.
///
/// This is the subtle half of `zlib.h` L871-L873 and it is worth pinning precisely, because "the
/// decoder window is too small" is not one behaviour but two:
///
/// * RFC 1951 and RFC 1952 record no window size anywhere, so there is nothing for `inflateInit2_` or
///   the header states to check. The mismatch is invisible until a match reaches back further than the
///   window holds.
/// * `inflate` satisfies a match from the **current output buffer** whenever the source is still in it
///   (`inflate.c`'s `MATCH` state compares `state->offset` against `state->whave + out - left`), so a
///   caller offering a generous output window never touches the sliding window at all and the
///   undersized window is never noticed. Shrink the output window and the same stream fails.
///
/// Both halves are asserted, and both directions of both halves. The payload is assembled from
/// `random.bin` rather than taken whole from any fixture because it has to contain a match at a
/// distance the small window cannot hold: the committed fixtures deliberately do not -- even
/// `window_boundary.bin`'s 32,768-byte repeat is beyond `MAX_DIST` and so is never matched -- which
/// makes them unable to reach this behaviour at all.
#[test]
fn an_undersized_window_without_a_header_is_detected_identically_only_when_needed() {
    let corpus = load_corpus();
    let payload = long_distance_payload(&sample(&corpus, "random.bin").bytes);

    for &container in &[Container::Raw, Container::Gzip] {
        let deflate_bits = container.deflate_window_bits(DEFAULT_WINDOW_SIZE);
        let inflate_bits = container.inflate_window_bits(UNDERSIZED_WINDOW_SIZE);

        // The generous-output-window half: a full, correct crossing in both directions.
        for &direction in DIRECTIONS {
            let case = Case {
                direction,
                container,
                deflate_bits,
                inflate_bits,
                level: DEFAULT_LEVEL,
                chunking: SINGLE_SHOT,
                fixture: "random.bin (long-distance assembly)",
                dictionary: false,
            };
            run_case(case, &payload, None);
        }

        // The small-output-window half: the same stream, refused identically. The produced byte count
        // is compared as well as the status, because two decoders that both gave up in the right place
        // is a stronger statement than two that both gave up.
        for &producer in DIRECTIONS {
            let stream = stream_from(producer, &payload, DEFAULT_LEVEL, deflate_bits);
            let chunking = Chunking {
                name: "small-output-window",
                input: None,
                output: Some(SMALL_OUTPUT_WINDOW),
            };
            let (port, _) = inflate_run::<Port>(&stream, inflate_bits, None, chunking);
            let (reference, _) = inflate_run::<Reference>(&stream, inflate_bits, None, chunking);
            assert_status_parity(
                &format!(
                    "inflate of a {container} stream written at windowBits {deflate_bits} by the \
                     {producer:?} side and read at windowBits {inflate_bits} through a \
                     {SMALL_OUTPUT_WINDOW}-byte output window",
                    container = container.name(),
                ),
                Z_DATA_ERROR,
                port.final_status,
                reference.final_status,
            );
            assert!(
                port.output == reference.output,
                "the two decoders gave up at different points: the port produced {port_len} bytes \
                 and the C reference produced {reference_len}\n  {difference}",
                port_len = port.output.len(),
                reference_len = reference.output.len(),
                difference = describe_byte_difference(&port.output, &reference.output),
            );
            assert!(
                !port.output.is_empty() && port.output.len() < payload.len(),
                "the refusal must happen part-way through: {len} of {want} bytes came out, which \
                 would make this case pass vacuously",
                len = port.output.len(),
                want = payload.len(),
            );
        }
    }
}

/// The output window the undersized-window case decodes through.
///
/// Small enough that a match reaching [`LONG_DISTANCE_FILLER`] bytes back cannot be satisfied from
/// the current output buffer, which is what forces the sliding window into play.
const SMALL_OUTPUT_WINDOW: usize = 256;

/// How many bytes separate the repeated prefix from its copy.
const LONG_DISTANCE_FILLER: usize = 2_000;

/// How long the repeated prefix is.
const LONG_DISTANCE_PREFIX: usize = 256;

/// `prefix + filler + prefix`, assembled from a fixture's bytes.
///
/// The repeat is at a distance of `LONG_DISTANCE_PREFIX + LONG_DISTANCE_FILLER`, comfortably inside a
/// 15-bit window and comfortably outside a 9-bit one, which is exactly the gap the case needs. The
/// bytes themselves come from `random.bin`, so the filler is incompressible and cannot accidentally
/// supply a shorter match that the encoder would prefer.
fn long_distance_payload(random: &[u8]) -> Vec<u8> {
    let needed = LONG_DISTANCE_PREFIX + LONG_DISTANCE_FILLER;
    assert!(
        random.len() >= needed,
        "random.bin is {got} bytes and the long-distance assembly needs {needed}",
        got = random.len(),
    );
    let mut payload = Vec::with_capacity(needed + LONG_DISTANCE_PREFIX);
    payload.extend_from_slice(&random[..LONG_DISTANCE_PREFIX]);
    payload.extend_from_slice(&random[LONG_DISTANCE_PREFIX..needed]);
    payload.extend_from_slice(&random[..LONG_DISTANCE_PREFIX]);
    payload
}

/// `deflateInit2_` and `inflateInit2_` answer identically for every `windowBits` outside the sweep.
///
/// This is a **status-parity** case and deliberately not an interoperability one, because most of
/// these values do not produce a stream at all. `zlib.h` L543-L600 and the measured behaviour agree:
/// a deflate `windowBits` of 8 is silently promoted to 9 and accepted, while -8, 0, 16, 24, 32 and 47
/// are refused outright -- and `inflateInit2_` accepts every one of them, because on that side they
/// mean the raw, from-the-header, gzip-only and autodetect variants.
#[test]
fn window_bits_guard_rails_agree() {
    for &(bits, deflate_expected, inflate_expected) in WINDOW_BITS_GUARD_RAILS {
        let mut port = Session::new(<Port as Side>::stream());
        let mut reference = Session::new(<Reference as Side>::stream());
        let port_status = Port::deflate_init(&mut port, DEFAULT_LEVEL, bits);
        let reference_status = Reference::deflate_init(&mut reference, DEFAULT_LEVEL, bits);
        assert_status_parity(
            &format!("deflateInit2_ with windowBits {bits}"),
            deflate_expected,
            port_status,
            reference_status,
        );
        // `deflateEnd` on both, whether the init succeeded or not: on the failure path it is the
        // documented no-op, and calling it unconditionally is what makes the success path leak-free
        // without a branch that could be got wrong.
        assert_status_parity(
            &format!("deflateEnd after deflateInit2_ with windowBits {bits}"),
            if deflate_expected == Z_OK {
                Z_OK
            } else {
                Z_STREAM_ERROR
            },
            Port::deflate_end(&mut port),
            Reference::deflate_end(&mut reference),
        );

        let mut port = Session::new(<Port as Side>::stream());
        let mut reference = Session::new(<Reference as Side>::stream());
        let port_status = Port::inflate_init(&mut port, bits);
        let reference_status = Reference::inflate_init(&mut reference, bits);
        assert_status_parity(
            &format!("inflateInit2_ with windowBits {bits}"),
            inflate_expected,
            port_status,
            reference_status,
        );
        assert_status_parity(
            &format!("inflateEnd after inflateInit2_ with windowBits {bits}"),
            if inflate_expected == Z_OK {
                Z_OK
            } else {
                Z_STREAM_ERROR
            },
            Port::inflate_end(&mut port),
            Reference::inflate_end(&mut reference),
        );
    }
}

/// `(windowBits, what deflateInit2_ must answer, what inflateInit2_ must answer)`.
///
/// Every row was measured against both implementations rather than inferred from the header prose,
/// because the prose and the code differ in one interesting place: `zlib.h` L560-L561 says a
/// `windowBits` of 8 is "silently reduced to 9" for the zlib container, and 8 is nonetheless refused
/// for raw and gzip -- which is why -8 and 24 are errors while 8 is not.
//
// `#[rustfmt::skip]` to keep the three columns aligned as a table.
#[rustfmt::skip]
const WINDOW_BITS_GUARD_RAILS: &[(c_int, c_int, c_int)] = &[
    // 8 is promoted to 9 by deflateInit2_ (zlib.h:560-561) and accepted by inflateInit2_.
    ( 8, Z_OK,           Z_OK),
    // -8 would be a 256-byte raw window, which deflateInit2_ rejects and inflateInit2_ accepts.
    (-8, Z_STREAM_ERROR, Z_OK),
    // 24 is 8 + 16: a 256-byte gzip window, rejected by deflateInit2_ for the same reason.
    (24, Z_STREAM_ERROR, Z_OK),
    // 0 means "from the zlib header", which is an inflate-only spelling.
    ( 0, Z_STREAM_ERROR, Z_OK),
    // 16 is gzip-only decoding with a 1-byte window: inflate-only, and deflateInit2_ refuses it.
    (16, Z_STREAM_ERROR, Z_OK),
    // 32 is autodetection with a 1-byte window: likewise inflate-only.
    (32, Z_STREAM_ERROR, Z_OK),
    // 47 is 15 + 32, the usual autodetect spelling, and equally not a deflate setting.
    (47, Z_STREAM_ERROR, Z_OK),
];

// =================================================================================================
//  The tests -- concatenated gzip members
// =================================================================================================

/// What one side observed while decoding a multi-member stream.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberReport {
    /// `(status, bytes produced, check value, total_out)` for each member, in order.
    members: Vec<(c_int, usize, u64, u64)>,
    /// What each `inflateReset2` between members returned.
    resets: Vec<c_int>,
    /// Everything that came out, concatenated.
    output: Vec<u8>,
    /// What `inflateEnd` returned.
    end: c_int,
}

/// Decodes a stream of concatenated gzip members on one side, resetting between them.
///
/// `zlib.h` L894-L899 is explicit that `inflate()` does **not** decode concatenated members: it
/// answers `Z_STREAM_END` at the end of one and "the state would need to be reset to continue ...
/// This *must* be done if there is more data after a gzip member, in order for the decompression to
/// be compliant with the gzip standard (RFC 1952)". So the reset is the caller's job, and this is what
/// a compliant caller does.
fn inflate_members<S: Side>(stream: &[u8], window_bits: c_int) -> MemberReport {
    let mut strm = Session::new(S::stream());
    let init = S::inflate_init(&mut strm, window_bits);
    assert!(
        init == Z_OK,
        "{side}: inflateInit2_({window_bits}) returned {got}",
        side = <S as Side>::NAME,
        got = status(init),
    );

    let mut members = Vec::new();
    let mut resets = Vec::new();
    let mut output = Vec::new();
    let mut scratch = vec![0_u8; DEFAULT_INFLATE_WINDOW];
    // The whole stream is offered once and `next_in` walks it, which is exactly how a caller with the
    // bytes already in memory drives this.
    let mut window = Some(stream);
    let mut member_bytes = 0_usize;
    loop {
        let call = S::inflate(&mut strm, window.take(), &mut scratch, Z_NO_FLUSH);
        output.extend_from_slice(&scratch[..call.produced]);
        member_bytes += call.produced;

        if call.status == Z_STREAM_END {
            let snapshot = S::snapshot(&strm);
            members.push((
                call.status,
                member_bytes,
                snapshot.adler,
                snapshot.total_out,
            ));
            member_bytes = 0;
            if call.avail_in_after == 0 {
                break;
            }
            resets.push(S::inflate_reset2(&mut strm, window_bits));
            continue;
        }
        if call.status != Z_OK {
            let snapshot = S::snapshot(&strm);
            members.push((
                call.status,
                member_bytes,
                snapshot.adler,
                snapshot.total_out,
            ));
            break;
        }
        if call.produced == 0 && call.avail_in_after == 0 {
            // Nothing left to work with and nothing came out: report it as the final member so that
            // the two sides' agreement on the shortfall is still compared.
            let snapshot = S::snapshot(&strm);
            members.push((Z_OK, member_bytes, snapshot.adler, snapshot.total_out));
            break;
        }
        assert!(
            members.len() < MEMBER_LIMIT,
            "{side}: decoding did not terminate after {count} members",
            side = <S as Side>::NAME,
            count = members.len(),
        );
    }

    let end = S::inflate_end(&mut strm);
    MemberReport {
        members,
        resets,
        output,
        end,
    }
}

/// How many members a multi-member decode may report before the driver calls it a hang.
const MEMBER_LIMIT: usize = 64;

/// Concatenated gzip members cross in both directions, and both sides need the same resets.
///
/// The member boundary is the interesting part and it is easy to get subtly wrong: `total_out` is reset
/// by `inflateReset2` but the caller's accumulated output is not, and a decoder that failed to reset
/// its CRC would produce the right bytes and the wrong check value. Both are compared, per member.
#[test]
fn concatenated_gzip_members_cross_in_both_directions() {
    let corpus = load_corpus();
    let first = sample(&corpus, HELLO_FIXTURE);
    let second = sample(&corpus, "text.txt");
    let third = sample(&corpus, "repetitive.bin");
    let gzip_bits = Container::Gzip.deflate_window_bits(DEFAULT_WINDOW_SIZE);
    let mut expected = Vec::new();
    expected.extend_from_slice(&first.bytes);
    expected.extend_from_slice(&second.bytes);
    expected.extend_from_slice(&third.bytes);

    for &producer in DIRECTIONS {
        let mut stream = Vec::new();
        for member in [first, second, third] {
            stream.extend_from_slice(&stream_from(
                producer,
                &member.bytes,
                DEFAULT_LEVEL,
                gzip_bits,
            ));
        }

        // Both decoders, so that the producer's stream is measured against both implementations and
        // the two reports can be compared field for field. Autodetection is used on the way in
        // because that is what a caller walking a multi-member file has.
        let autodetect = DEFAULT_WINDOW_SIZE + AUTODETECT_ADDEND;
        let port = inflate_members::<Port>(&stream, autodetect);
        let reference = inflate_members::<Reference>(&stream, autodetect);

        assert!(
            port == reference,
            "the two decoders disagree about a three-member gzip stream written by the \
             {producer:?} side\n  port:      {port:?}\n  reference: {reference:?}"
        );
        assert!(
            port.output == expected,
            "a three-member gzip stream written by the {producer:?} side did not reproduce its \
             members\n  {difference}",
            difference = describe_byte_difference(&port.output, &expected),
        );
        assert!(
            port.members.len() == 3
                && port.members.iter().all(|member| member.0 == Z_STREAM_END)
                && port.resets.iter().all(|reset| *reset == Z_OK)
                && port.end == Z_OK,
            "each member must end with Z_STREAM_END and each reset with Z_OK: {port:?}"
        );
        // Per-member accounting: `total_out` restarts at each reset, so it must equal that member's
        // own length rather than the running total.
        let lengths = [first.bytes.len(), second.bytes.len(), third.bytes.len()];
        for (index, (member, want)) in port.members.iter().zip(lengths.iter()).enumerate() {
            let want = u64::try_from(*want).unwrap();
            assert!(
                member.3 == want,
                "member {index} reported total_out {got} and is {want} bytes; inflateReset2 must \
                 restart the accounting",
                got = member.3,
            );
        }
    }
}

// =================================================================================================
//  The tests -- gzip header fields and their clamps
// =================================================================================================

/// Writes a gzip stream carrying `spec`'s `FEXTRA`, `FNAME`, `FCOMMENT` and header CRC.
///
/// Kept separate from [`deflate_run`] because `deflateSetHeader` has to happen between the init and the
/// first `deflate` (`zlib.h` L724-L740), and because the header struct has to outlive every one of
/// those calls -- obligation (f).
fn gzip_stream_with_header<S: Side>(input: &[u8], spec: &mut HeaderSpec) -> Vec<u8> {
    // ★ THE HEADER IS DECLARED FIRST, and the compiler is what requires it. `deflateSetHeader`
    // hands the library a pointer it keeps until the header has been emitted, so `Session` holds
    // the header borrowed for its own whole life; declaring the header after the session makes it
    // dropped while still borrowed, which is E0597 rather than a comment. The obligation used to be
    // stated in prose here and enforced by nothing.
    let mut header = S::write_header(spec);
    let mut strm = Session::new(S::stream());
    let bits = Container::Gzip.deflate_window_bits(DEFAULT_WINDOW_SIZE);
    let init = S::deflate_init(&mut strm, DEFAULT_LEVEL, bits);
    assert!(
        init == Z_OK,
        "{side}: deflateInit2_({bits}) returned {got}",
        side = <S as Side>::NAME,
        got = status(init),
    );

    let set = S::deflate_set_header(&mut strm, &mut header);
    assert!(
        set == Z_OK,
        "{side}: deflateSetHeader returned {got}",
        side = <S as Side>::NAME,
        got = status(set),
    );

    let bound = usize::try_from(S::deflate_bound(&mut strm, input.len())).unwrap();
    let mut scratch = vec![0_u8; bound + spec.header_bytes() + BOUND_SLACK];
    let call = S::deflate(&mut strm, Some(input), &mut scratch, Z_FINISH);
    assert!(
        call.status == Z_STREAM_END,
        "{side}: deflate(Z_FINISH) returned {got} while writing a headered gzip stream",
        side = <S as Side>::NAME,
        got = status(call.status),
    );
    let end = S::deflate_end(&mut strm);
    assert!(
        end == Z_OK,
        "{side}: deflateEnd returned {got}",
        side = <S as Side>::NAME,
        got = status(end),
    );
    scratch.truncate(call.produced);
    scratch
}

impl HeaderSpec {
    /// A generous upper bound on the bytes the header itself occupies.
    ///
    /// `deflateBound` does not account for `deflateSetHeader`'s fields (`deflate.c` L884-L892 covers
    /// only the fixed gzip header), so the output buffer has to be sized for them separately or a
    /// long name would produce a `Z_BUF_ERROR` that looked like a defect.
    fn header_bytes(&self) -> usize {
        // The 10-byte fixed header, the 2-byte CRC-16, the 2-byte `XLEN`, and each variable field.
        10 + 2 + 2 + self.extra.len() + self.name.len() + self.comment.len()
    }
}

/// What one side observed while reading a gzip header back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeaderPass {
    /// What `inflateGetHeader` returned.
    requested: c_int,
    /// The scalar members afterwards.
    report: HeaderReport,
    /// The three buffers afterwards, untouched tails and all.
    buffers: HeaderBuffers,
    /// What the final `inflate` returned.
    final_status: c_int,
    /// The decompressed payload.
    output: Vec<u8>,
    /// What `inflateEnd` returned.
    end: c_int,
}

/// Reads `stream`'s gzip header into buffers of `cap` bytes each, then decompresses the payload.
///
/// `cap` is deliberately allowed to be too small: `zlib.h` L1088-L1096 requires writes to be clamped to
/// `extra_max`, `name_max` and `comm_max`, and states that the application detects truncation "by the
/// absence of a terminating zero". That clamp is AAP 0.6.1's unsafe-site category 6 and historically the
/// source of gzip-header overflow defects, so the undersized case is the one that matters.
fn read_gzip_header<S: Side>(stream: &[u8], cap: usize) -> HeaderPass {
    // ★ DECLARATION ORDER IS THE CONTRACT, and it is now checked. The buffers outlive the header and
    // the header outlives the stream, which is what makes the pointers inside the header valid for
    // every `inflate` call below -- obligation (h). `Session` holds the header borrowed for its own
    // whole life, so writing these three lines in any other order is E0597 instead of a comment
    // saying they must not be. `inflate.c` writes THROUGH `head->extra`, `head->name` and
    // `head->comment` as it parses, so this is the retention that matters most in the whole harness.
    let mut buffers = HeaderBuffers::new(cap);
    let mut header = S::read_header(&mut buffers);
    let mut strm = Session::new(S::stream());
    let bits = Container::Gzip.inflate_window_bits(DEFAULT_WINDOW_SIZE);
    let init = S::inflate_init(&mut strm, bits);
    assert!(
        init == Z_OK,
        "{side}: inflateInit2_({bits}) returned {got}",
        side = <S as Side>::NAME,
        got = status(init),
    );

    let requested = S::inflate_get_header(&mut strm, &mut header);

    let mut scratch = vec![0_u8; DEFAULT_INFLATE_WINDOW];
    let mut output = Vec::new();
    let mut window = Some(stream);
    let mut stalled = 0_usize;
    let final_status = loop {
        let call = S::inflate(&mut strm, window.take(), &mut scratch, Z_NO_FLUSH);
        output.extend_from_slice(&scratch[..call.produced]);
        if call.status != Z_OK {
            break call.status;
        }
        if call.produced == 0 && call.avail_in_after == 0 {
            break Z_OK;
        }
        stalled += 1;
        assert!(
            stalled < STALL_LIMIT,
            "{side}: reading a headered gzip stream did not terminate",
            side = <S as Side>::NAME,
        );
    };

    // The stream is ended, and THEN the session is dropped, and only then is the header read. The
    // order is forced: the session borrows the header exclusively, so reading it while the session
    // is alive is E0502 -- which is the right answer, because until `inflateEnd` has run the
    // library may still write through that pointer.
    let end = S::inflate_end(&mut strm);
    drop(strm);
    let report = S::header_report(&header);
    HeaderPass {
        requested,
        report,
        buffers,
        final_status,
        output,
        end,
    }
}

/// `inflateGetHeader` agrees field for field in both directions, **including the clamps**.
///
/// Four crossings per capacity: each side writes the headered stream and each side reads it, so both
/// implementations' writers are measured against both implementations' readers. The capacities are
/// chosen either side of every field length, so that the same header is observed both truncated and
/// whole.
#[test]
fn inflate_get_header_agrees_including_the_clamps() {
    let corpus = load_corpus();
    let payload = &sample(&corpus, HELLO_FIXTURE).bytes;
    let mut spec = header_spec();

    for &cap in HEADER_CAPS {
        let from_reference = gzip_stream_with_header::<Reference>(payload, &mut spec);
        let from_port = gzip_stream_with_header::<Port>(payload, &mut spec);

        for (producer, stream) in [
            (Direction::CToRust, &from_reference),
            (Direction::RustToC, &from_port),
        ] {
            let port = read_gzip_header::<Port>(stream, cap);
            let reference = read_gzip_header::<Reference>(stream, cap);
            assert!(
                port == reference,
                "the two implementations disagree about a gzip header written by the \
                 {producer:?} side and read into {cap}-byte buffers\n  port:      {port:?}\n  \
                 reference: {reference:?}"
            );

            // Not vacuous: the header must actually have been parsed, and the payload must have
            // survived it.
            assert!(
                port.requested == Z_OK
                    && port.final_status == Z_STREAM_END
                    && port.end == Z_OK
                    && port.report.done == 1,
                "the header was not read to completion: {port:?}"
            );
            assert!(
                port.output == *payload,
                "the payload behind the header did not survive\n  {difference}",
                difference = describe_byte_difference(&port.output, payload),
            );

            // The fields that are transmitted verbatim.
            assert!(
                port.report.text == spec.text
                    && port.report.time == u64::from(spec.time)
                    && port.report.os == spec.os
                    && port.report.hcrc == spec.hcrc,
                "a scalar header field was not round-tripped: wrote text={text} time={time} \
                 os={os} hcrc={hcrc} and read back {report:?}",
                text = spec.text,
                time = spec.time,
                os = spec.os,
                hcrc = spec.hcrc,
                report = port.report,
            );
            // `extra_len` is the field's TRUE length even when the copy was truncated
            // (`zlib.h` L1090-L1092), which is what lets a caller detect the shortfall.
            assert!(
                port.report.extra_len == u64::try_from(spec.extra.len()).unwrap(),
                "extra_len must report the field's true length {want} and reports {got}",
                want = spec.extra.len(),
                got = port.report.extra_len,
            );
            assert_header_clamp(cap, &spec, &port.buffers);
        }
    }
}

/// The buffer capacities the header case reads into.
///
/// 0 exercises the "the pointer is non-null and there is no room at all" corner; 3 truncates every
/// field; 13 is the exact length of the name including its NUL, so the terminator lands on the last
/// byte; and 64 holds everything with room to spare.
const HEADER_CAPS: &[usize] = &[0, 3, 13, 64];

/// The header every case writes.
///
/// The three variable fields have deliberately different lengths so that one capacity truncates some
/// and not others, and `hcrc` is on so that the `HCRC` state is reached and the header checksum
/// verified rather than skipped.
fn header_spec() -> HeaderSpec {
    HeaderSpec {
        text: 1,
        time: 0x1234_5678,
        // 3 is RFC 1952's Unix, which is what a Unix build reports.
        os: 3,
        hcrc: 1,
        extra: b"EXTRAFIELD".to_vec(),
        name: b"filename.txt\0".to_vec(),
        comment: b"a comment here\0".to_vec(),
    }
}

/// Asserts each buffer holds the clamped prefix of its field and nothing beyond it.
///
/// The untouched tail is checked as well as the written prefix, because a write past `*_max` is
/// precisely the defect this case exists to catch and it would be invisible from the prefix alone.
/// `HEADER_FILL` is a byte no header field here contains, which is what makes "untouched" observable.
fn assert_header_clamp(cap: usize, spec: &HeaderSpec, buffers: &HeaderBuffers) {
    for (label, field, buffer) in [
        ("extra", &spec.extra, &buffers.extra),
        ("name", &spec.name, &buffers.name),
        ("comment", &spec.comment, &buffers.comment),
    ] {
        assert!(
            buffer.len() == cap,
            "the {label} buffer was resized from {cap} to {got}",
            got = buffer.len(),
        );
        let written = field.len().min(cap);
        assert!(
            buffer[..written] == field[..written],
            "the {label} field was not copied: wrote {field:?} and read back {buffer:?}"
        );
        assert!(
            buffer[written..].iter().all(|byte| *byte == HEADER_FILL),
            "the {label} field was written past its cap of {cap}: the tail should still be all \
             0x{HEADER_FILL:02x} and is {tail:?}",
            tail = &buffer[written..],
        );
    }
}

// =================================================================================================
//  The tests -- preset dictionaries
// =================================================================================================

/// A preset dictionary crosses in both directions, and the `dictId` handshake holds.
///
/// The whole of `test/example.c`'s dictionary pair (L414-L490), reproduced on both sides and crossed:
/// `deflateInit`, `deflateSetDictionary`, capture `strm->adler` as the identifier, `deflate(Z_FINISH)`;
/// then `inflateInit`, loop on `inflate`, and on `Z_NEED_DICT` check that the reported `adler` is that
/// same identifier before calling `inflateSetDictionary`.
///
/// The identifier is asserted against the pinned constant as well as between the sides, because two
/// implementations agreeing on the *wrong* Adler-32 would otherwise interoperate happily with each
/// other and with nothing else: `corpus/README.md` records both candidate dictionary lengths and
/// their checksums, and 6 bytes -- `hello\0` -- is the contract.
#[test]
fn a_preset_dictionary_crosses_in_both_directions() {
    let corpus = load_corpus();
    let payload = &sample(&corpus, HELLO_FIXTURE).bytes;
    let dictionary = &sample(&corpus, DICTIONARY_FIXTURE).bytes;

    // The identifier each compressor chooses, checked before any crossing runs: a divergence here
    // would make every finding below a consequence rather than a cause.
    let zlib_bits = Container::Zlib.deflate_window_bits(DEFAULT_WINDOW_SIZE);
    let port_id = deflate_run::<Port>(
        payload,
        Z_BEST_COMPRESSION,
        zlib_bits,
        Some(dictionary),
        SINGLE_SHOT,
    )
    .1;
    let reference_id = deflate_run::<Reference>(
        payload,
        Z_BEST_COMPRESSION,
        zlib_bits,
        Some(dictionary),
        SINGLE_SHOT,
    )
    .1;
    assert!(
        port_id == Some(DICTIONARY_ADLER) && reference_id == Some(DICTIONARY_ADLER),
        "the dictId must be 0x{DICTIONARY_ADLER:08x} on both sides; the port reports \
         {port_id:?} and the C reference reports {reference_id:?}"
    );

    // The crossings. The zlib container exercises the Z_NEED_DICT handshake; the raw container
    // exercises the other path `zlib.h` L921-L926 documents, where there is no signal at all and the
    // dictionary has to be installed before the first `inflate`.
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &container in &[Container::Zlib, Container::Raw] {
            for &chunking in CHUNKINGS {
                let case = Case {
                    direction,
                    container,
                    deflate_bits: container.deflate_window_bits(DEFAULT_WINDOW_SIZE),
                    inflate_bits: container.inflate_window_bits(DEFAULT_WINDOW_SIZE),
                    level: Z_BEST_COMPRESSION,
                    chunking,
                    fixture: HELLO_FIXTURE,
                    dictionary: true,
                };
                run_case(case, payload, Some(dictionary));
                crossings += 1;
            }
        }
    }
    println!("dictionary sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");

    // The handshake and the accounting compared BETWEEN the implementations rather than each against
    // its own invariant. This is what pins the `total_in` quirk as shared behaviour: `inflate` leaves
    // the bytes consumed by the call that answered `Z_NEED_DICT` out of `total_in`, and a port that
    // counted them would satisfy every crossing above and fail here.
    for &producer in DIRECTIONS {
        let stream = match producer {
            Direction::CToRust => {
                deflate_run::<Reference>(
                    payload,
                    Z_BEST_COMPRESSION,
                    zlib_bits,
                    Some(dictionary),
                    SINGLE_SHOT,
                )
                .0
                .output
            }
            Direction::RustToC => {
                deflate_run::<Port>(
                    payload,
                    Z_BEST_COMPRESSION,
                    zlib_bits,
                    Some(dictionary),
                    SINGLE_SHOT,
                )
                .0
                .output
            }
        };
        for &chunking in CHUNKINGS {
            let (port, port_handshake) =
                inflate_run::<Port>(&stream, zlib_bits, Some(dictionary), chunking);
            let (reference, reference_handshake) =
                inflate_run::<Reference>(&stream, zlib_bits, Some(dictionary), chunking);
            assert!(
                port_handshake == reference_handshake,
                "the two decoders disagree about the dictionary handshake for a stream from the \
                 {producer:?} side at chunking {chunking}: port {port_handshake:?}, reference \
                 {reference_handshake:?}",
                chunking = chunking.name,
            );
            assert!(
                port.snapshot == reference.snapshot && port.calls == reference.calls,
                "the two decoders disagree about the accounting for a dictionary stream from the \
                 {producer:?} side at chunking {chunking}\n  port:      {port_snapshot:?}\n  \
                 reference: {reference_snapshot:?}",
                chunking = chunking.name,
                port_snapshot = port.snapshot,
                reference_snapshot = reference.snapshot,
            );
            assert!(
                port.output == *payload && reference.output == *payload,
                "the dictionary crossing did not reproduce the payload at chunking {chunking}\n  \
                 {difference}",
                chunking = chunking.name,
                difference = describe_byte_difference(&port.output, payload),
            );
        }
    }
}

/// The wrong dictionary is refused identically, and no dictionary at all stops identically.
///
/// `zlib.h` L930-L934: `inflateSetDictionary` answers `Z_DATA_ERROR` when "the given dictionary doesn't
/// match the expected one (incorrect Adler-32 value)". Both producers and both consumers, so each
/// implementation's refusal is measured against each implementation's stream. The no-dictionary case is
/// asserted too, because `Z_NEED_DICT` is the documented answer there and a decoder that silently
/// carried on would produce plausible-looking rubbish.
#[test]
fn a_mismatched_dictionary_is_refused_identically() {
    let corpus = load_corpus();
    let payload = &sample(&corpus, HELLO_FIXTURE).bytes;
    let dictionary = &sample(&corpus, DICTIONARY_FIXTURE).bytes;
    let wrong = WRONG_DICTIONARY;
    assert!(
        Port::adler32(wrong) != DICTIONARY_ADLER,
        "the wrong dictionary must have a different Adler-32, or the case proves nothing"
    );

    let zlib_bits = Container::Zlib.deflate_window_bits(DEFAULT_WINDOW_SIZE);
    for &producer in DIRECTIONS {
        let stream = match producer {
            Direction::CToRust => {
                deflate_run::<Reference>(
                    payload,
                    Z_BEST_COMPRESSION,
                    zlib_bits,
                    Some(dictionary),
                    SINGLE_SHOT,
                )
                .0
                .output
            }
            Direction::RustToC => {
                deflate_run::<Port>(
                    payload,
                    Z_BEST_COMPRESSION,
                    zlib_bits,
                    Some(dictionary),
                    SINGLE_SHOT,
                )
                .0
                .output
            }
        };

        // The wrong dictionary.
        let (port, port_handshake) =
            inflate_run::<Port>(&stream, zlib_bits, Some(wrong), SINGLE_SHOT);
        let (reference, reference_handshake) =
            inflate_run::<Reference>(&stream, zlib_bits, Some(wrong), SINGLE_SHOT);
        assert_status_parity(
            &format!(
                "inflateSetDictionary with the wrong dictionary, on a stream written by the \
                 {producer:?} side"
            ),
            Z_DATA_ERROR,
            port.final_status,
            reference.final_status,
        );
        assert!(
            port_handshake == reference_handshake
                && port_handshake.need_dict_adler == Some(DICTIONARY_ADLER),
            "both sides must have reported the same dictId with Z_NEED_DICT before refusing: \
             port {port_handshake:?}, reference {reference_handshake:?}"
        );
        assert!(
            port.output.is_empty() && reference.output.is_empty(),
            "nothing may be produced from a stream whose dictionary was refused: port produced \
             {port_len} bytes and the C reference produced {reference_len}",
            port_len = port.output.len(),
            reference_len = reference.output.len(),
        );

        // No dictionary at all.
        let (port, _) = inflate_run::<Port>(&stream, zlib_bits, None, SINGLE_SHOT);
        let (reference, _) = inflate_run::<Reference>(&stream, zlib_bits, None, SINGLE_SHOT);
        assert_status_parity(
            &format!(
                "inflate with no dictionary at all, on a stream written by the {producer:?} side"
            ),
            Z_NEED_DICT,
            port.final_status,
            reference.final_status,
        );
    }
}

/// A dictionary whose Adler-32 is not the fixture's, for the mismatch case.
///
/// The same length as the real one so that the refusal is attributable to the checksum rather than to
/// the length.
const WRONG_DICTIONARY: &[u8] = b"wrong\0";

/// `deflateSetDictionary` is refused identically for the gzip container.
///
/// `deflate.c` L1553-L1556 rejects it when `wrap == 2`, which is the gzip wrapper, because RFC 1952
/// carries no `DICTID` field for a decompressor to check against. The refusal is a documented part of
/// the contract, so it is asserted rather than avoided -- and it is why the dictionary sweep above
/// covers the zlib and raw containers only.
#[test]
fn a_dictionary_is_refused_for_gzip_identically() {
    let corpus = load_corpus();
    let dictionary = &sample(&corpus, DICTIONARY_FIXTURE).bytes;
    let gzip_bits = Container::Gzip.deflate_window_bits(DEFAULT_WINDOW_SIZE);

    let mut port = Session::new(<Port as Side>::stream());
    let mut reference = Session::new(<Reference as Side>::stream());
    assert_status_parity(
        "deflateInit2_ for the gzip container",
        Z_OK,
        Port::deflate_init(&mut port, DEFAULT_LEVEL, gzip_bits),
        Reference::deflate_init(&mut reference, DEFAULT_LEVEL, gzip_bits),
    );
    assert_status_parity(
        "deflateSetDictionary on a gzip stream",
        Z_STREAM_ERROR,
        Port::deflate_set_dictionary(&mut port, dictionary),
        Reference::deflate_set_dictionary(&mut reference, dictionary),
    );
    assert_status_parity(
        "deflateEnd after the refused deflateSetDictionary",
        Z_OK,
        Port::deflate_end(&mut port),
        Reference::deflate_end(&mut reference),
    );
}

// =================================================================================================
//  The tests -- the one-shot wrappers
// =================================================================================================

/// `compress2` and `uncompress2` cross in both directions, and **both** out-parameters agree.
///
/// `uncompr.c` L86-L120 makes `sourceLen` an in/out parameter as well as `destLen`: it reports bytes
/// actually consumed, through the accounting `len += avail_in; left += avail_out; *sourceLen -= len;
/// *destLen -= left;`. A wrapper that decompressed correctly while misreporting consumption breaks
/// every caller that walks a concatenated stream, so both numbers are compared and not just the bytes.
#[test]
fn the_one_shot_wrappers_cross_in_both_directions() {
    let corpus = load_corpus();
    let mut crossings = 0;
    for sample in &corpus {
        for &level in ONE_SHOT_LEVELS {
            let bound = usize::try_from(Reference::compress_bound(sample.bytes.len())).unwrap();
            let mut from_reference = vec![0_u8; bound];
            let mut from_port = vec![0_u8; bound];
            let deflated_reference =
                Reference::compress2(&mut from_reference, &sample.bytes, level);
            let deflated_port = Port::compress2(&mut from_port, &sample.bytes, level);
            assert!(
                deflated_reference == deflated_port,
                "compress2 disagreed for {name} at level {level}: the C reference reports \
                 {deflated_reference:?} and the port reports {deflated_port:?}",
                name = sample.fixture.name,
            );
            assert!(
                deflated_port.status == Z_OK,
                "compress2 returned {got} for {name} at level {level} into a \
                 compressBound-sized buffer",
                got = status(deflated_port.status),
                name = sample.fixture.name,
            );

            let reference_stream =
                &from_reference[..usize::try_from(deflated_reference.dest_len).unwrap()];
            let port_stream = &from_port[..usize::try_from(deflated_port.dest_len).unwrap()];

            // The two crossings, and each stream is additionally given to its own producer's
            // decoder so that the two `OneShot` reports can be compared field for field.
            for (producer, stream) in [
                (Direction::CToRust, reference_stream),
                (Direction::RustToC, port_stream),
            ] {
                let mut into_port = vec![0_u8; sample.bytes.len()];
                let mut into_reference = vec![0_u8; sample.bytes.len()];
                let by_port = Port::uncompress2(&mut into_port, stream);
                let by_reference = Reference::uncompress2(&mut into_reference, stream);
                assert!(
                    by_port == by_reference,
                    "uncompress2 disagreed for {name} at level {level}, stream from the \
                     {producer:?} side: the port reports {by_port:?} and the C reference reports \
                     {by_reference:?}",
                    name = sample.fixture.name,
                );
                assert!(
                    by_port.status == Z_OK,
                    "uncompress2 returned {got} for {name} at level {level}",
                    got = status(by_port.status),
                    name = sample.fixture.name,
                );
                assert!(
                    by_port.dest_len == u64::try_from(sample.bytes.len()).unwrap()
                        && by_port.source_len == u64::try_from(stream.len()).unwrap(),
                    "uncompress2's accounting is wrong for {name}: destLen {dest} of {want_dest} \
                     and sourceLen {source} of {want_source}",
                    name = sample.fixture.name,
                    dest = by_port.dest_len,
                    want_dest = sample.bytes.len(),
                    source = by_port.source_len,
                    want_source = stream.len(),
                );
                assert!(
                    into_port == sample.bytes && into_reference == sample.bytes,
                    "the one-shot crossing did not reproduce {name}\n  {difference}",
                    name = sample.fixture.name,
                    difference = describe_byte_difference(&into_port, &sample.bytes),
                );
                crossings += 1;
            }
        }
    }
    println!("one-shot sweep: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// The levels the one-shot sweep uses: the two extremes plus the default, which is what
/// `compressBound` and `Z_DEFAULT_COMPRESSION` are actually sized for.
const ONE_SHOT_LEVELS: &[c_int] = &[0, Z_DEFAULT_COMPRESSION, 6, Z_BEST_COMPRESSION];

/// `uncompress2` refuses an undersized destination identically.
///
/// `zlib.h` L1332-L1336: `Z_BUF_ERROR` when "there was not enough room in the output buffer". Asserted
/// for both implementations against both producers, and the partial output is compared as well as the
/// status so that two wrappers giving up in different places is caught.
#[test]
fn the_one_shot_wrappers_refuse_an_undersized_destination_identically() {
    let corpus = load_corpus();
    // A fixture with more than one byte of output, so that "one byte short" is a real constraint.
    let sample = sample(&corpus, "text.txt");
    let bound = usize::try_from(Reference::compress_bound(sample.bytes.len())).unwrap();

    for &producer in DIRECTIONS {
        let mut buffer = vec![0_u8; bound];
        let deflated = match producer {
            Direction::CToRust => Reference::compress2(&mut buffer, &sample.bytes, DEFAULT_LEVEL),
            Direction::RustToC => Port::compress2(&mut buffer, &sample.bytes, DEFAULT_LEVEL),
        };
        assert!(
            deflated.status == Z_OK,
            "compress2 returned {got} for the {producer:?} side, so the undersized-destination \
             case would have nothing to decompress",
            got = status(deflated.status),
        );
        let stream = &buffer[..usize::try_from(deflated.dest_len).unwrap()];

        let short = sample.bytes.len() - 1;
        let mut into_port = vec![0_u8; short];
        let mut into_reference = vec![0_u8; short];
        let by_port = Port::uncompress2(&mut into_port, stream);
        let by_reference = Reference::uncompress2(&mut into_reference, stream);
        assert_status_parity(
            &format!(
                "uncompress2 into a {short}-byte buffer for a {want}-byte payload, stream from \
                 the {producer:?} side",
                want = sample.bytes.len(),
            ),
            Z_BUF_ERROR,
            by_port.status,
            by_reference.status,
        );
        assert!(
            by_port == by_reference && into_port == into_reference,
            "the two wrappers gave up differently: port {by_port:?}, reference {by_reference:?}"
        );
        assert!(
            into_port == sample.bytes[..short],
            "the partial output must be the payload's prefix\n  {difference}",
            difference = describe_byte_difference(&into_port, &sample.bytes[..short]),
        );
    }
}

// =================================================================================================
//  The tests -- truncation and corruption
// =================================================================================================

/// A truncated stream is refused identically, at every offset, in every container.
///
/// Neither side may panic, hang or read past the buffer, and both must reach the same conclusion. The
/// partial output is compared as well as the status, because two decoders that stopped at different
/// points disagree about the stream even when both eventually said the same word.
#[test]
fn truncated_streams_are_handled_identically() {
    let corpus = load_corpus();
    let sample = sample(&corpus, "repetitive.bin");
    let mut cases = 0;

    for &container in CONTAINERS {
        let bits = container.deflate_window_bits(DEFAULT_WINDOW_SIZE);
        for &producer in DIRECTIONS {
            let stream = stream_from(producer, &sample.bytes, DEFAULT_LEVEL, bits);
            for cut in truncation_offsets(stream.len()) {
                let truncated = &stream[..cut];
                let (port, _) = inflate_run::<Port>(truncated, bits, None, SINGLE_SHOT);
                let (reference, _) = inflate_run::<Reference>(truncated, bits, None, SINGLE_SHOT);
                assert!(
                    port.final_status == reference.final_status,
                    "a {container} stream from the {producer:?} side truncated to {cut} of \
                     {full} bytes: the port answered {port_status} and the C reference answered \
                     {reference_status}",
                    container = container.name(),
                    full = stream.len(),
                    port_status = status(port.final_status),
                    reference_status = status(reference.final_status),
                );
                assert!(
                    port.output == reference.output,
                    "the two decoders produced different amounts from a {container} stream \
                     truncated to {cut} bytes\n  {difference}",
                    container = container.name(),
                    difference = describe_byte_difference(&port.output, &reference.output),
                );
                // Whatever came out must be a prefix of the payload: a truncated stream may yield
                // less, never something else.
                assert!(
                    sample.bytes.starts_with(&port.output),
                    "a truncated {container} stream produced {len} bytes that are not a prefix \
                     of the payload\n  {difference}",
                    container = container.name(),
                    len = port.output.len(),
                    difference =
                        describe_byte_difference(&port.output, &sample.bytes[..port.output.len()]),
                );
                assert!(
                    port.end == reference.end && port.end == Z_OK,
                    "inflateEnd must still answer Z_OK after a truncated stream: port {port_end}, \
                     reference {reference_end}",
                    port_end = status(port.end),
                    reference_end = status(reference.end),
                );
                cases += 1;
            }
        }
    }
    println!("truncation sweep: {cases} cases");
    assert!(cases > 0, "the sweep enumerated nothing");
}

/// The offsets a stream is truncated at: the very start, inside the header, just inside the payload,
/// the middle, and one byte short of complete.
fn truncation_offsets(len: usize) -> Vec<usize> {
    let mut offsets = vec![0, 1, 2, 5, len / 2, len.saturating_sub(1)];
    offsets.retain(|offset| *offset < len);
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// A corrupted payload byte and a corrupted trailer are both refused identically.
///
/// Two distinct failures with two distinct causes, and separating them matters: a flipped payload byte
/// is caught by the Huffman decoder or the distance check, while a flipped trailer byte is caught only
/// by the check value -- so a port whose checksum was never actually compared would pass the first and
/// fail the second, and one whose decoder was too permissive the reverse.
#[test]
fn corrupted_streams_are_refused_identically() {
    let corpus = load_corpus();
    let sample = sample(&corpus, "text.txt");
    let mut cases = 0;

    for &container in CONTAINERS {
        let bits = container.deflate_window_bits(DEFAULT_WINDOW_SIZE);
        for &producer in DIRECTIONS {
            let stream = stream_from(producer, &sample.bytes, DEFAULT_LEVEL, bits);
            assert!(
                stream.len() > MINIMUM_CORRUPTIBLE_STREAM,
                "a {container} stream of {len} bytes is too short to corrupt meaningfully",
                container = container.name(),
                len = stream.len(),
            );

            // A payload byte, chosen past any header and before any trailer.
            let mut flipped = stream.clone();
            let at = stream.len() / 2;
            flipped[at] ^= 0xff;
            let (port, _) = inflate_run::<Port>(&flipped, bits, None, SINGLE_SHOT);
            let (reference, _) = inflate_run::<Reference>(&flipped, bits, None, SINGLE_SHOT);
            assert!(
                port.final_status == reference.final_status && port.output == reference.output,
                "a {container} stream from the {producer:?} side with byte {at} flipped: the port \
                 answered {port_status} with {port_len} bytes and the C reference answered \
                 {reference_status} with {reference_len}",
                container = container.name(),
                port_status = status(port.final_status),
                port_len = port.output.len(),
                reference_status = status(reference.final_status),
                reference_len = reference.output.len(),
            );
            cases += 1;

            // The trailer's own last byte. Raw carries no trailer, so the case applies to the two
            // wrapped containers only -- and for those the expected answer is specifically
            // Z_DATA_ERROR, because the payload decodes cleanly and only the check value disagrees.
            if container.has_check_value() {
                let mut trailer = stream.clone();
                let last = trailer.len() - 1;
                trailer[last] ^= 0xff;
                let (port, _) = inflate_run::<Port>(&trailer, bits, None, SINGLE_SHOT);
                let (reference, _) = inflate_run::<Reference>(&trailer, bits, None, SINGLE_SHOT);
                assert_status_parity(
                    &format!(
                        "inflate of a {container} stream from the {producer:?} side whose trailer \
                         check value was corrupted",
                        container = container.name(),
                    ),
                    Z_DATA_ERROR,
                    port.final_status,
                    reference.final_status,
                );
                assert!(
                    port.output == sample.bytes && reference.output == sample.bytes,
                    "the payload decodes cleanly and only the check value is wrong, so both sides \
                     must still have produced the whole payload: port {port_len} bytes, \
                     reference {reference_len}",
                    port_len = port.output.len(),
                    reference_len = reference.output.len(),
                );
                cases += 1;
            }
        }
    }
    println!("corruption sweep: {cases} cases");
    assert!(cases > 0, "the sweep enumerated nothing");
}

/// How long a stream has to be before flipping its middle byte is meaningful.
const MINIMUM_CORRUPTIBLE_STREAM: usize = 12;

// =================================================================================================
//  The tests -- inflateSync recovery
// =================================================================================================

/// A stream whose first block has been damaged, exactly as `test/example.c`'s `test_flush` builds one.
///
/// `test/example.c` L337-L367: compress the payload's first three bytes with `Z_FULL_FLUSH`, increment
/// the fourth byte of the output to corrupt that first block, then compress the rest with `Z_FINISH`.
/// The `Z_FULL_FLUSH` is what makes recovery possible at all -- it emits an empty stored block whose
/// byte pattern `inflateSync` scans for.
fn damaged_stream<S: Side>(payload: &[u8], head: usize) -> Vec<u8> {
    assert!(
        payload.len() > head,
        "the payload must be longer than the {head}-byte head the flush covers"
    );
    let mut strm = Session::new(S::stream());
    let init = S::deflate_init(&mut strm, Z_DEFAULT_COMPRESSION, DEFAULT_WINDOW_SIZE);
    assert!(
        init == Z_OK,
        "{side}: deflateInit2_ returned {got}",
        side = <S as Side>::NAME,
        got = status(init),
    );

    let bound = usize::try_from(S::deflate_bound(&mut strm, payload.len())).unwrap();
    let mut scratch = vec![0_u8; bound + BOUND_SLACK];

    // L349-L353: the first three bytes, flushed fully so that a sync point exists.
    let first = S::deflate(
        &mut strm,
        Some(&payload[..head]),
        &mut scratch,
        Z_FULL_FLUSH,
    );
    assert!(
        first.status == Z_OK,
        "{side}: deflate(Z_FULL_FLUSH) returned {got}",
        side = <S as Side>::NAME,
        got = status(first.status),
    );
    assert!(
        first.produced > DAMAGED_BYTE,
        "{side}: the flushed head is only {produced} bytes, so byte {DAMAGED_BYTE} cannot be \
         damaged",
        side = <S as Side>::NAME,
        produced = first.produced,
    );

    // L355: `compr[3]++` -- force an error in the first compressed block.
    scratch[DAMAGED_BYTE] = scratch[DAMAGED_BYTE].wrapping_add(1);

    // L356-L360: the rest, finished. The output window starts where the first call stopped, so the
    // two pieces concatenate into one stream.
    let rest = S::deflate(
        &mut strm,
        Some(&payload[head..]),
        &mut scratch[first.produced..],
        Z_FINISH,
    );
    assert!(
        rest.status == Z_STREAM_END,
        "{side}: deflate(Z_FINISH) returned {got}",
        side = <S as Side>::NAME,
        got = status(rest.status),
    );
    let end = S::deflate_end(&mut strm);
    assert!(
        end == Z_OK,
        "{side}: deflateEnd returned {got}",
        side = <S as Side>::NAME,
        got = status(end),
    );
    scratch.truncate(first.produced + rest.produced);
    scratch
}

/// Which output byte the damaged-stream builder corrupts -- `test/example.c` L355's `compr[3]`.
const DAMAGED_BYTE: usize = 3;

/// How many payload bytes the first, damaged block covers -- `test/example.c` L351's `avail_in = 3`.
const DAMAGED_HEAD: usize = 3;

/// What one side observed while recovering from a damaged first block.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SyncPass {
    /// What the first `inflate` returned, having been given only the two header bytes.
    header_status: c_int,
    /// What `inflateSyncPoint` reported before the sync.
    sync_point_before: c_int,
    /// What `inflateSync` returned.
    sync: c_int,
    /// What `inflateSyncPoint` reported after it.
    sync_point_after: c_int,
    /// What the final `inflate(Z_FINISH)` returned.
    final_status: c_int,
    /// The recovered bytes.
    output: Vec<u8>,
    /// The accounting afterwards.
    snapshot: Snapshot,
    /// What `inflateEnd` returned.
    end: c_int,
}

/// Recovers from a damaged first block, exactly as `test/example.c`'s `test_sync` does.
///
/// `test/example.c` L370-L408: offer only the two header bytes, `inflate`, then widen `avail_in` to the
/// whole remaining buffer -- `next_in` having already advanced past the header -- call `inflateSync` to
/// skip the damaged block, and finish with `inflate(Z_FINISH)`, which must answer `Z_STREAM_END`.
fn sync_recover<S: Side>(stream: &[u8]) -> SyncPass {
    assert!(
        stream.len() > ZLIB_HEADER_BYTES,
        "the stream must be longer than its own header"
    );
    let mut strm = Session::new(S::stream());
    let init = S::inflate_init(&mut strm, DEFAULT_WINDOW_SIZE);
    assert!(
        init == Z_OK,
        "{side}: inflateInit2_ returned {got}",
        side = <S as Side>::NAME,
        got = status(init),
    );

    let mut scratch = vec![0_u8; DEFAULT_INFLATE_WINDOW];
    // L385-L389: just the zlib header. The window is a prefix of `stream`, so `next_in` ends up
    // pointing into `stream` itself -- which is what makes the widening below legitimate.
    let header = S::inflate(
        &mut strm,
        Some(&stream[..ZLIB_HEADER_BYTES]),
        &mut scratch,
        Z_NO_FLUSH,
    );
    let sync_point_before = S::inflate_sync_point(&mut strm);

    // L391: `d_stream.avail_in = (uInt)comprLen - 2` -- read all the compressed data, damaged part
    // included. `next_in` already points at `stream[2]` and `stream` outlives every call here, so the
    // widened count describes memory that really is readable: obligation (c), discharged by the
    // caller rather than by the gate, which is why this is the one place a member is written directly.
    S::set_avail_in(&mut strm, stream.len() - ZLIB_HEADER_BYTES);
    let sync = S::inflate_sync(&mut strm);
    let sync_point_after = S::inflate_sync_point(&mut strm);

    // L394: finish, which must reach the end of the stream from the recovered position.
    let mut output = Vec::new();
    output.extend_from_slice(&scratch[..header.produced]);
    let finish = S::inflate(&mut strm, None, &mut scratch, Z_FINISH);
    output.extend_from_slice(&scratch[..finish.produced]);

    let snapshot = S::snapshot(&strm);
    let end = S::inflate_end(&mut strm);
    SyncPass {
        header_status: header.status,
        sync_point_before,
        sync,
        sync_point_after,
        final_status: finish.status,
        output,
        snapshot,
        end,
    }
}

/// RFC 1950's fixed header length.
const ZLIB_HEADER_BYTES: usize = 2;

/// `inflateSync` recovers identically in both directions.
///
/// The recovery is the interesting part and it is a genuine crossing: one implementation writes a
/// stream with a deliberately corrupted first block, and the other has to find the `Z_FULL_FLUSH` sync
/// point that follows it and resume. `test/example.c` prints `hel` plus whatever survived, and the
/// three bytes it loses are exactly the damaged block's -- so the recovered output being a strict
/// *suffix* of the payload is the expected outcome, not a defect.
#[test]
fn inflate_sync_recovers_identically_in_both_directions() {
    let corpus = load_corpus();
    let payload = &sample(&corpus, HELLO_FIXTURE).bytes;

    for &producer in DIRECTIONS {
        let stream = match producer {
            Direction::CToRust => damaged_stream::<Reference>(payload, DAMAGED_HEAD),
            Direction::RustToC => damaged_stream::<Port>(payload, DAMAGED_HEAD),
        };

        let port = sync_recover::<Port>(&stream);
        let reference = sync_recover::<Reference>(&stream);
        assert!(
            port == reference,
            "the two decoders recovered differently from a damaged stream written by the \
             {producer:?} side\n  port:      {port:?}\n  reference: {reference:?}"
        );

        // Not vacuous: the sync must have succeeded and the stream must have been finished.
        assert!(
            port.header_status == Z_OK && port.sync == Z_OK && port.final_status == Z_STREAM_END,
            "the recovery did not follow test/example.c's shape: {port:?}"
        );
        assert!(
            port.end == Z_OK,
            "inflateEnd returned {got} after a recovery",
            got = status(port.end),
        );
        // The damaged block's bytes are gone and the rest is intact, which is what `test/example.c`
        // L407 prints as `hel` plus the recovered tail.
        assert!(
            port.output == payload[DAMAGED_HEAD..],
            "the recovered bytes must be the payload minus its first {DAMAGED_HEAD}\n  \
             {difference}",
            difference = describe_byte_difference(&port.output, &payload[DAMAGED_HEAD..]),
        );
    }
}

// =================================================================================================
//  The tests -- the gzFile crossings
// =================================================================================================

/// Direction 3: the C reference writes a `.gz` file, the port reads it, for every fixture.
///
/// Every fixture shares one `Scratch` and one `#[test]`, with a distinct filename per fixture. The
/// single-test discipline is the one `crates/libz-rs-sys/tests/c_api_parity.rs` adopts for its ordered
/// stages: cargo runs separate `#[test]` functions on separate threads, so anything sharing a path or a
/// mutable fixture has to be sequenced inside one of them.
#[test]
fn c_gzwrite_to_rust_gzread_reproduces_every_fixture() {
    let corpus = load_corpus();
    let scratch = Scratch::new("c-to-rust");
    let crossings = gz_cross_every_fixture::<Reference, Port>(&scratch, &corpus);
    println!("C gzwrite -> Rust gzread: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// Direction 4: the port writes a `.gz` file, the C reference reads it, for every fixture.
#[test]
fn rust_gzwrite_to_c_gzread_reproduces_every_fixture() {
    let corpus = load_corpus();
    let scratch = Scratch::new("rust-to-c");
    let crossings = gz_cross_every_fixture::<Port, Reference>(&scratch, &corpus);
    println!("Rust gzwrite -> C gzread: {crossings} crossings");
    assert!(crossings > 0, "the sweep enumerated nothing");
}

/// Writes every fixture with `W` and reads it back with `R`, asserting the payload survives.
///
/// The expected bytes are the three write entry points' contributions followed by the fixture, which is
/// what makes `gzputc`, `gzputs`, `gzfwrite` and `gzwrite` interact rather than merely each work.
fn gz_cross_every_fixture<W: GzSide, R: GzSide>(scratch: &Scratch, corpus: &[Sample]) -> usize {
    let prefix = gz_prefix();
    let mut crossings = 0;
    for sample in corpus {
        let path = scratch.c_path(&format!("{name}{GZ_SUFFIX}", name = sample.fixture.name));
        let written = gz_write_pass::<W>(&path, &sample.bytes);
        assert!(
            written.close == Z_OK,
            "{side}: gzclose after writing {name} returned {got} -- test/minigzip.c L388 requires \
             Z_OK",
            side = <W as GzSide>::NAME,
            name = sample.fixture.name,
            got = status(written.close),
        );

        let mut expected = prefix.clone();
        expected.extend_from_slice(&sample.bytes);
        let (bytes, read) = gz_read_pass::<R>(&path, expected.len());
        assert!(
            read.close == Z_OK,
            "{side}: gzclose after reading {name} returned {got} -- test/minigzip.c L413 requires \
             Z_OK",
            side = <R as GzSide>::NAME,
            name = sample.fixture.name,
            got = status(read.close),
        );
        assert!(
            bytes == expected,
            "the gz crossing did not reproduce {name}: {writer} wrote {file_len} file bytes and \
             {reader} read {got} of {want} payload bytes\n  {difference}",
            name = sample.fixture.name,
            writer = <W as GzSide>::NAME,
            file_len = written.file_len,
            reader = <R as GzSide>::NAME,
            got = bytes.len(),
            want = expected.len(),
            difference = describe_byte_difference(&bytes, &expected),
        );
        // Not vacuous: the stream really was a gzip stream rather than a transparently copied file,
        // and the reader really did reach the end of it.
        assert!(
            read.direct_before_read == 0 && read.eof_at_end == 1,
            "a gzip file must report gzdirect 0 and reach gzeof: {read:?}"
        );
        crossings += 1;
    }
    crossings
}

/// Both gz crossings report the identical surface, and both produce the identical file.
///
/// The two directions above each assert that the payload survives; this asserts that the two
/// *implementations* answer identically at every one of the twenty-odd entry points a pass touches --
/// which is the difference between "it worked" and "they agree". The finished files are compared too:
/// byte-identical output is `tests/byte_identical.rs`'s criterion, and the `gzFile` layer is a caller
/// of `deflate` like any other, so two identical passes must leave two identical files.
#[test]
fn the_gz_surfaces_agree_in_both_directions() {
    let corpus = load_corpus();
    let scratch = Scratch::new("surfaces");
    let sample = sample(&corpus, "repetitive.bin");
    let prefix = gz_prefix();
    let mut expected = prefix;
    expected.extend_from_slice(&sample.bytes);

    let reference_name = format!("from-reference{GZ_SUFFIX}");
    let port_name = format!("from-port{GZ_SUFFIX}");
    let reference_path = scratch.c_path(&reference_name);
    let port_path = scratch.c_path(&port_name);
    let by_reference = gz_write_pass::<Reference>(&reference_path, &sample.bytes);
    let by_port = gz_write_pass::<Port>(&port_path, &sample.bytes);
    assert!(
        by_reference == by_port,
        "the two write surfaces disagree\n  reference: {by_reference:?}\n  port:      {by_port:?}"
    );

    let reference_bytes = std::fs::read(scratch.path(&reference_name)).unwrap();
    let port_bytes = std::fs::read(scratch.path(&port_name)).unwrap();
    assert!(
        reference_bytes == port_bytes,
        "the two implementations wrote different .gz files for the same input\n  {difference}",
        difference = describe_byte_difference(&port_bytes, &reference_bytes),
    );

    // Each file read by the other implementation -- the crossing -- and by its own, so that the two
    // read surfaces are compared as well as the two write surfaces.
    for (label, path) in [
        ("written by the C reference", &reference_path),
        ("written by the port", &port_path),
    ] {
        let (port_bytes, port_read) = gz_read_pass::<Port>(path, expected.len());
        let (reference_bytes, reference_read) = gz_read_pass::<Reference>(path, expected.len());
        assert!(
            port_read == reference_read,
            "the two read surfaces disagree on a file {label}\n  port:      {port_read:?}\n  \
             reference: {reference_read:?}"
        );
        assert!(
            port_bytes == expected && reference_bytes == expected,
            "a file {label} did not read back as written\n  {difference}",
            difference = describe_byte_difference(&port_bytes, &expected),
        );
    }
}

/// `gzread` copies a non-gzip file through transparently, and both sides agree about every kind of it.
///
/// `gzdirect` is what distinguishes the two modes (`zlib.h` L1726-L1740), and the interesting cases are
/// the ones that *look* compressed: a bare zlib stream and a bare raw `DEFLATE` stream are both valid
/// compressed data that the `gzFile` layer must nonetheless copy verbatim, because neither carries
/// gzip's magic. A port that sniffed for a zlib header instead of a gzip one would pass a plain-text
/// case and fail these two.
#[test]
fn gzread_passes_non_gzip_files_through_identically() {
    let corpus = load_corpus();
    let scratch = Scratch::new("transparent");
    let payload = &sample(&corpus, HELLO_FIXTURE).bytes;

    let plain = b"not a gzip file at all, and not compressed either\n".to_vec();
    let zlib_stream = stream_from(
        Direction::CToRust,
        payload,
        DEFAULT_LEVEL,
        Container::Zlib.deflate_window_bits(DEFAULT_WINDOW_SIZE),
    );
    let raw_stream = stream_from(
        Direction::CToRust,
        payload,
        DEFAULT_LEVEL,
        Container::Raw.deflate_window_bits(DEFAULT_WINDOW_SIZE),
    );

    for (label, contents) in [
        ("plain.txt", &plain),
        ("bare-zlib.bin", &zlib_stream),
        ("bare-raw.bin", &raw_stream),
    ] {
        let path = scratch.path(label);
        std::fs::write(&path, contents).unwrap();
        let c_path = scratch.c_path(label);

        let (port_bytes, port_read) = gz_read_pass::<Port>(&c_path, contents.len());
        let (reference_bytes, reference_read) = gz_read_pass::<Reference>(&c_path, contents.len());
        assert!(
            port_read == reference_read,
            "the two read surfaces disagree about the non-gzip file {label}\n  port:      \
             {port_read:?}\n  reference: {reference_read:?}"
        );
        assert!(
            port_bytes == *contents && reference_bytes == *contents,
            "the non-gzip file {label} was not copied through verbatim\n  {difference}",
            difference = describe_byte_difference(&port_bytes, contents),
        );
        assert!(
            port_read.direct_before_read == 1,
            "gzdirect must report 1 for the transparently copied file {label}: {port_read:?}"
        );
    }
}

/// The `gzFile` layer and the stream API cross each other, in both directions.
///
/// The complement to [`gzread_passes_non_gzip_files_through_identically`], and the other half of what
/// `gzdirect` distinguishes. That test feeds the file layer something the stream API produced and
/// watches it copy the bytes through untouched; this one closes the loop the other way:
///
/// * a `.gz` file `gzwrite` produced is decoded by the **stream** `inflate` on the other side, with
///   gzip `windowBits` and again with autodetection -- which proves the file layer emits an ordinary
///   RFC 1952 member and not something only its own reader understands;
/// * a gzip stream the **stream** `deflate` produced is written to a file and read by `gzread` on the
///   other side, with `gzdirect` reporting 0 because it really is a gzip stream.
///
/// The mid-stream `Z_SYNC_FLUSH` that [`gz_write_pass`] performs makes the first half worth having on
/// its own: a decoder that mishandled the empty stored block a sync flush emits would pass every
/// `gzread` case here, because the file layer's own reader would make the same mistake.
#[test]
fn the_gz_layer_and_the_stream_api_cross_each_other() {
    let corpus = load_corpus();
    let scratch = Scratch::new("layers");
    let sample = sample(&corpus, "text.txt");
    let mut expected = gz_prefix();
    expected.extend_from_slice(&sample.bytes);
    let gzip_bits = Container::Gzip.inflate_window_bits(DEFAULT_WINDOW_SIZE);
    let autodetect = DEFAULT_WINDOW_SIZE + AUTODETECT_ADDEND;

    // ---- gzwrite -> stream inflate ----------------------------------------------------------
    for (label, writer) in [
        (
            format!("gz-written-by-reference{GZ_SUFFIX}"),
            Direction::CToRust,
        ),
        (format!("gz-written-by-port{GZ_SUFFIX}"), Direction::RustToC),
    ] {
        let c_path = scratch.c_path(&label);
        let written = match writer {
            Direction::CToRust => gz_write_pass::<Reference>(&c_path, &sample.bytes),
            Direction::RustToC => gz_write_pass::<Port>(&c_path, &sample.bytes),
        };
        assert!(
            written.close == Z_OK,
            "gzclose returned {got} while writing {label}",
            got = status(written.close),
        );
        let bytes = std::fs::read(scratch.path(&label)).unwrap();

        // Both decoders on both settings, so the crossing runs and the two are compared.
        for &bits in &[gzip_bits, autodetect] {
            let (port, _) = inflate_run::<Port>(&bytes, bits, None, SINGLE_SHOT);
            let (reference, _) = inflate_run::<Reference>(&bytes, bits, None, SINGLE_SHOT);
            assert!(
                port.final_status == Z_STREAM_END && reference.final_status == Z_STREAM_END,
                "a .gz file {label} must decode through the stream API at windowBits {bits}: the \
                 port answered {port_status} and the C reference answered {reference_status}",
                port_status = status(port.final_status),
                reference_status = status(reference.final_status),
            );
            assert!(
                port.output == expected && reference.output == expected,
                "the stream API decoded {label} to the wrong bytes at windowBits {bits}\n  \
                 {difference}",
                difference = describe_byte_difference(&port.output, &expected),
            );
            assert!(
                port.snapshot == reference.snapshot,
                "the two stream decoders disagree about the accounting for {label} at windowBits \
                 {bits}\n  port: {port_snapshot:?}\n  reference: {reference_snapshot:?}",
                port_snapshot = port.snapshot,
                reference_snapshot = reference.snapshot,
            );
        }
    }

    // ---- stream deflate -> gzread ------------------------------------------------------------
    for (label, producer) in [
        (
            format!("stream-written-by-reference{GZ_SUFFIX}"),
            Direction::CToRust,
        ),
        (
            format!("stream-written-by-port{GZ_SUFFIX}"),
            Direction::RustToC,
        ),
    ] {
        let stream = stream_from(
            producer,
            &sample.bytes,
            DEFAULT_LEVEL,
            Container::Gzip.deflate_window_bits(DEFAULT_WINDOW_SIZE),
        );
        std::fs::write(scratch.path(&label), &stream).unwrap();
        let c_path = scratch.c_path(&label);

        let (port_bytes, port_read) = gz_read_pass::<Port>(&c_path, sample.bytes.len());
        let (reference_bytes, reference_read) =
            gz_read_pass::<Reference>(&c_path, sample.bytes.len());
        assert!(
            port_read == reference_read,
            "the two gz readers disagree about {label}\n  port:      {port_read:?}\n  reference: \
             {reference_read:?}"
        );
        assert!(
            port_bytes == sample.bytes && reference_bytes == sample.bytes,
            "gzread did not reproduce a stream-API gzip member from {label}\n  {difference}",
            difference = describe_byte_difference(&port_bytes, &sample.bytes),
        );
        assert!(
            port_read.direct_before_read == 0,
            "a gzip member written by the stream API is still a gzip stream, so gzdirect must \
             report 0 for {label}: {port_read:?}"
        );
    }
}

/// `gzdopen` writes a stream the other implementation reads, and the two agree throughout.
///
/// `gzdopen` is a distinct entry point from `gzopen` -- it adopts a descriptor the caller already has
/// rather than opening a path, and `gzclose` is what closes it (`zlib.h` L1404-L1425) -- so the
/// ownership handoff is part of what this measures. `gzsetparams` mid-stream is exercised here too,
/// because a stream opened at one level and switched to another is where a port most easily loses
/// track of its pending output.
///
/// Unix-only, and this is the single target-dependent case in the file. `gzdopen` takes a **CRT file
/// descriptor**, which on Unix is what `IntoRawFd` yields; on Windows a `File` yields a `HANDLE`, and
/// turning one into a descriptor needs the CRT's `_open_osfhandle`, which a test cannot reach without a
/// further dependency that AAP 0.7.1(i) rules out. The same write surface is covered on every target by
/// [`the_gz_surfaces_agree_in_both_directions`], which reaches it through `gzopen`.
#[cfg(unix)]
#[test]
fn gzdopen_writes_a_stream_the_other_side_reads() {
    let corpus = load_corpus();
    let scratch = Scratch::new("dopen");
    let sample = sample(&corpus, "text.txt");
    let mut expected = gz_prefix();
    expected.extend_from_slice(&sample.bytes);

    let mut reports = Vec::new();
    for (label, writer) in [
        (format!("from-reference{GZ_SUFFIX}"), Direction::CToRust),
        (format!("from-port{GZ_SUFFIX}"), Direction::RustToC),
    ] {
        let path = scratch.path(&label);
        let c_path = scratch.c_path(&label);
        // The descriptor is handed straight to `gzdopen`, which adopts it; `into_raw_fd` gives up
        // Rust's ownership at the same moment, so there is exactly one owner throughout and the
        // handle's `gzclose` is what closes it.
        let file = std::fs::File::create(&path).unwrap();
        let fd = raw_descriptor(file);

        let written = match writer {
            Direction::CToRust => gz_dopen_write_pass::<Reference>(fd, &sample.bytes),
            Direction::RustToC => gz_dopen_write_pass::<Port>(fd, &sample.bytes),
        };
        reports.push(written);

        // The crossing: whichever side did not write reads it back.
        let (bytes, read) = match writer {
            Direction::CToRust => gz_read_pass::<Port>(&c_path, expected.len()),
            Direction::RustToC => gz_read_pass::<Reference>(&c_path, expected.len()),
        };
        assert!(
            bytes == expected,
            "a gzdopen-written stream did not cross: {label}\n  {difference}",
            difference = describe_byte_difference(&bytes, &expected),
        );
        assert!(
            read.direct_before_read == 0 && read.eof_at_end == 1 && read.close == Z_OK,
            "a gzdopen-written stream must read back as a gzip stream: {read:?}"
        );
    }

    assert!(
        reports[0] == reports[1],
        "the two gzdopen write surfaces disagree\n  reference: {reference:?}\n  port:      \
         {port:?}",
        reference = reports[0],
        port = reports[1],
    );
}

/// The descriptor behind an owned `File`, given up to the library.
///
/// Split out so that the platform-specific trait import is in exactly one place, and gated exactly as
/// its one caller is.
#[cfg(unix)]
fn raw_descriptor(file: std::fs::File) -> c_int {
    use std::os::unix::io::IntoRawFd as _;
    file.into_raw_fd()
}

/// Writes through a descriptor `gzdopen` adopts, exercising `gzsetparams` mid-stream.
#[cfg(unix)]
fn gz_dopen_write_pass<S: GzSide>(fd: c_int, payload: &[u8]) -> GzWriteReport {
    let file = S::dopen(fd, c"wb1");
    assert!(
        !S::is_null(file),
        "{side}: gzdopen returned NULL",
        side = <S as GzSide>::NAME,
    );

    let buffer = S::buffer(file, GZ_BUFFER_SIZE);
    // Mid-stream, and to a different level than the mode string asked for, which is the case
    // `zlib.h` L1445-L1454 exists for.
    let setparams = S::setparams(file, Z_BEST_COMPRESSION, Z_DEFAULT_STRATEGY);
    let put_byte = S::putc(file, GZ_PREFIX_BYTE);
    let put_text = S::puts(file, GZ_PREFIX_TEXT);
    let fwrite = S::fwrite(file, GZ_PREFIX_ITEMS);
    let written = S::write(file, payload);
    assert!(
        written == narrow_int(payload.len()),
        "{side}: gzwrite accepted {written} of {want} bytes -- {error}",
        side = <S as GzSide>::NAME,
        want = payload.len(),
        error = gz_error_text::<S>(file),
    );
    let flush = S::flush(file, Z_SYNC_FLUSH);

    GzWriteReport {
        buffer,
        setparams,
        putc: put_byte,
        puts: put_text,
        fwrite,
        writes: vec![written],
        flush,
        tell: (S::tell(file), S::tell64(file)),
        offset: (S::offset(file), S::offset64(file)),
        eof: S::eof(file),
        direct: S::direct(file),
        error: S::error(file),
        close: S::close(file),
        // A descriptor has no path to stat, and the crossing below reads the file anyway, so the
        // length is reported as zero rather than guessed at. The two reports are compared with each
        // other, so a constant in both is neither a false pass nor a false failure.
        file_len: 0,
    }
}

// =================================================================================================
//  The tests -- the exhaustive sweep
// =================================================================================================

/// The variable that arms the exhaustive sweep, named to the tree's `ZLIB_RS_*` convention and to the
/// same shape `tests/byte_identical.rs` uses.
const ENV_EXHAUSTIVE: &str = "ZLIB_RS_INTEROP_EXHAUSTIVE";

/// Reports that the exhaustive sweep was reached without being armed, and returns whether to proceed.
///
/// The one place this suite declines to do work, and it is not a skipped comparison: every dimension it
/// enumerates is covered somewhere in the default run, just not multiplied by every other dimension.
/// The message names the remedy, following the convention `crates/libz-rs-sys/tests/symbol_parity.rs`
/// establishes for a genuine precondition.
fn armed() -> bool {
    match std::env::var(ENV_EXHAUSTIVE) {
        Ok(value) if !value.is_empty() && value != "0" => true,
        _ => {
            println!(
                "SKIP: the exhaustive interoperability sweep was reached with {ENV_EXHAUSTIVE} \
                 unset, so it did no work.\n      CI must run: {ENV_EXHAUSTIVE}=1 cargo test \
                 --locked -p zlib-rs-differential --release --test roundtrip_interop -- --ignored \
                 --test-threads 4"
            );
            false
        }
    }
}

/// The exact number of crossings [`exhaustive_interoperability_matrix`] must perform.
///
/// ★ THE PRODUCT IS PINNED, NOT MERELY POSITIVE. This sweep used to end at
/// `assert!(crossings > 0)`, and the CI step that runs it accepted any positive count it could
/// parse out of the log. Between them that admitted every silent narrowing this suite exists to
/// prevent: deleting a level, dropping the raw container, reducing `WINDOW_SIZES` to `[15]`,
/// removing a fixture from `FIXTURES`, or making `chunkings_for` answer one chunking would all have
/// left a green run advertising itself as "the full interoperability product". The number below is
/// what the dimensions multiply out to, [`EXHAUSTIVE_CROSSING_DIMENSIONS`] proves the multiplication
/// from the constants themselves, and the sweep asserts the count it actually performed against
/// both -- so a dimension that shrinks fails with the arithmetic in the message rather than passing.
///
/// 2 directions x 10 levels x 7 window sizes x 3 containers x 39 fixture-chunkings = 16,380, where
/// the 39 is 9 fixtures at 4 [`CHUNKINGS`] plus `window_boundary.bin` at 3 [`CHUNKINGS_LARGE`]
/// (it is the one fixture above [`LARGE_FIXTURE_BYTES`]). Measured at 4.5 s in release.
const EXHAUSTIVE_CROSSINGS: usize = 16_380;

/// The cardinality of every dimension the exhaustive sweep multiplies, as a name and a count.
///
/// Kept beside [`EXHAUSTIVE_CROSSINGS`] so that a shrunken dimension is reported as itself -- "the
/// `windowBits` dimension carries 1 value, not 7" -- instead of as an opaque total that no longer
/// matches. The fixture-chunking count is a sum rather than a product because `chunkings_for`
/// answers a shorter list for a fixture above [`LARGE_FIXTURE_BYTES`], which is why it cannot be
/// checked as two independent factors.
fn exhaustive_crossing_dimensions(corpus: &[Sample]) -> [(&'static str, usize, usize); 5] {
    let fixture_chunkings: usize = corpus
        .iter()
        .map(|sample| chunkings_for(sample.bytes.len()).len())
        .sum();
    [
        ("direction", DIRECTIONS.len(), 2),
        ("level", LEVELS.len(), 10),
        ("windowBits", WINDOW_SIZES.len(), 7),
        ("container", CONTAINERS.len(), 3),
        ("fixture x chunking", fixture_chunkings, 39),
    ]
}

/// The full cross product: every level, every window size, every container, every fixture, every
/// chunking, in both directions.
///
/// Held out of the default run for runtime rather than for coverage. The default tests sweep each
/// dimension against the others' defaults; this multiplies them together, which is what makes it the
/// product rather than the coverage. It is cheap enough in release that the `differential` CI job
/// runs it, and expensive enough in debug -- where a bare `cargo test --workspace` lives -- to keep
/// behind the variable rather than let it dominate that run. Every crossing decompresses as well as
/// compresses, and every one is asserted byte-exact.
///
/// The accounting is part of the test: see [`EXHAUSTIVE_CROSSINGS`].
#[test]
#[ignore = "the full interoperability product; armed by ZLIB_RS_INTEROP_EXHAUSTIVE, run by the \
              differential CI job"]
fn exhaustive_interoperability_matrix() {
    if !armed() {
        return;
    }
    let corpus = load_corpus();

    // Every dimension is checked BEFORE the sweep runs, so a narrowed one is reported by name in a
    // second rather than after the product has been enumerated.
    let mut product = 1_usize;
    for (name, actual, expected) in exhaustive_crossing_dimensions(&corpus) {
        assert!(
            actual == expected,
            "the {name} dimension carries {actual} value(s) and the pinned product is built on \
             {expected}; the exhaustive interoperability sweep must not narrow silently -- if the \
             change is deliberate, update EXHAUSTIVE_CROSSINGS and this table in the same commit"
        );
        product *= actual;
    }
    assert!(
        product == EXHAUSTIVE_CROSSINGS,
        "the dimensions multiply out to {product} crossings and EXHAUSTIVE_CROSSINGS says \
         {EXHAUSTIVE_CROSSINGS}; the constant and the table disagree"
    );

    let started = std::time::Instant::now();
    let mut crossings = 0;
    for &direction in DIRECTIONS {
        for &level in LEVELS {
            for &bits in WINDOW_SIZES {
                for &container in CONTAINERS {
                    for sample in &corpus {
                        for &chunking in chunkings_for(sample.bytes.len()) {
                            let case = matched_case(
                                direction,
                                container,
                                bits,
                                level,
                                chunking,
                                sample.fixture.name,
                            );
                            run_case(case, &sample.bytes, None);
                            crossings += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "exhaustive interoperability matrix: {crossings} crossings in {elapsed:.1} s",
        elapsed = started.elapsed().as_secs_f64(),
    );
    // The count the sweep PERFORMED, against the count the dimensions promise. The two can only
    // differ if a loop stopped early or skipped a case, which no `continue` in this file does today
    // and which this assertion is what keeps true.
    assert!(
        crossings == EXHAUSTIVE_CROSSINGS,
        "the sweep performed {crossings} crossings and the pinned product is \
         {EXHAUSTIVE_CROSSINGS}; a loop skipped or repeated cases"
    );
}
