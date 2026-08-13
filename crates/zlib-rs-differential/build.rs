//! Build script for `zlib-rs-differential`, the dev-only differential oracle harness.
//!
//! # What this script produces
//!
//! Exactly one linkable artifact: the static archive `libzlib_c_oracle.a` in `OUT_DIR`,
//! containing
//!
//! * the fifteen in-tree C translation units of the reference implementation -- fourteen compiled
//!   directly, and `deflate.c` compiled through the generated translation unit described below,
//!   which includes it verbatim, and
//! * two generated C files that expose the reference implementation's `static` tables: a shim
//!   that hands out pointers to the tables the generated headers define, and the `deflate.c`
//!   wrapper that reaches the one table no header defines,
//!
//! with **every** externally visible symbol renamed to carry a `c_` prefix. The prefix is what
//! lets this workspace's Rust library and the C reference implementation be linked into one test
//! binary and called alternately so their output buffers can be compared directly, which is how
//! "compressed output is byte-identical to the reference" stops being an assertion and becomes a
//! measured fact. The intermediate objects, both generated C sources and the `objcopy` map also
//! remain in `OUT_DIR` as ordinary build intermediates; nothing is generated in the source tree.
//!
//! The archive is consumed only by this crate's tests and benches. It is never a dependency of
//! `zlib-rs` or `libz-rs-sys`, so `cargo build --release` for `libz.so`/`libz.a` never invokes a
//! C compiler. That one-way edge is the mechanical proof that reference zlib is an *oracle* and
//! not a build dependency.
//!
//! # The C sources are read-only
//!
//! Every C file in the repository root is reference material: it is compiled, never edited,
//! preprocessed in place, or regenerated. Objects, both generated C sources and the archive are
//! all written to `OUT_DIR` and nowhere else, so the C build stays green and keeps its standing as
//! the oracle. Nothing here touches the network, vendors a copy of anything, or needs a
//! submodule -- the reference sources are already in the tree.
//!
//! `deflate.c` is read-only under that rule too, and the generated translation unit honours it by
//! `#include`-ing it rather than copying it: the authoritative file is what the compiler reads,
//! from its own location in the tree, so no copy of it can ever go stale or be edited by mistake.
//!
//! # The fifteen translation units
//!
//! The set is `Makefile.in`'s `OBJZ` plus `OBJG` object lists, taken verbatim so that this
//! script and the C build can never disagree about what "the reference implementation" is:
//!
//! * `OBJZ`: `adler32.c`, `crc32.c`, `deflate.c`, `infback.c`, `inffast.c`, `inflate.c`,
//!   `inftrees.c`, `trees.c`, `zutil.c`
//! * `OBJG`: `compress.c`, `uncompr.c`, `gzclose.c`, `gzlib.c`, `gzread.c`, `gzwrite.c`
//!
//! Note `uncompr.c`. There is no `uncompress.c` in this repository; prose that says otherwise is
//! wrong about the filename.
//!
//! # Compile flags, and why `_LARGEFILE64_SOURCE` is not optional
//!
//! `-O3 -fPIC -D_LARGEFILE64_SOURCE=1 -DHAVE_HIDDEN -I<repo root>` -- the flags the in-tree
//! `configure` script actually applies (`-fPIC` at L285, `-D_LARGEFILE64_SOURCE=1` at L637-L639,
//! `-DHAVE_HIDDEN` at L947-L948).
//!
//! `-D_LARGEFILE64_SOURCE=1` is mandatory rather than a nicety: it is what makes the oracle
//! declare and define the large-file family -- `gzopen64`, `gzseek64`, `gztell64`, `gzoffset64`,
//! `adler32_combine64`, `crc32_combine64`, `crc32_combine_gen64` -- that the differential tests
//! compare against. Without it those symbols simply are not in the archive and the `*64` half of
//! the public surface goes untested. `test/CMakeLists.txt` propagates the same definition through
//! `INTERFACE_COMPILE_DEFINITIONS`, which corroborates the choice.
//!
//! Equally important is what is *not* defined. The Rust port implements the *default*
//! configuration, so the oracle has to be built in that same default configuration. The macros
//! that have to stay undefined fall into two distinct classes, though, and collapsing them into
//! one sentence misdescribes what several of them actually do.
//!
//! ## Class A -- knobs that change emitted bytes
//!
//! These are the ones a byte-identity comparison is directly sensitive to.
//!
//! * `FASTEST` substitutes a two-entry `configuration_table` (`deflate.c` L106-L110) and a
//!   separate, cut-down `longest_match` (L1532-L1588), so match selection differs at every level.
//! * `UNALIGNED_OK` selects the other of the two `longest_match` bodies (`deflate.c`
//!   L1404-L1512): a two-byte `ush` early rejection (L1446-L1451) in place of the four-way byte
//!   quartet the port reproduces verbatim (L1480-L1484). `zconf.h` L208-L210 defines it only
//!   under `MSDOS`, so it is already off on every target this harness builds for. This file makes
//!   no claim that the two bodies select identical matches -- it only requires the oracle to use
//!   the same body the port ports.
//! * `FORCE_STATIC` (`trees.c` L1034) and `FORCE_STORED` (L1044) override block-type selection
//!   outright, which rewrites the three block-type bits and the whole block encoding.
//!
//! ## Class B -- output-neutral knobs that must still stay undefined
//!
//! None of these changes a compressed byte or a table *value*. Each one instead changes
//! initialization strategy, the reported compile flags, the per-stream memory footprint, or the
//! exported surface. The suites that compare those four things -- `tests/` beside this file and
//! the benchmark suites of the `benches/` package, which depends on this one -- are the reason each
//! one still
//! has to stay undefined: they matter for their own reasons rather than for byte identity, and an
//! oracle built with any of them defined would fail those comparisons while emitting identical
//! bytes.
//!
//! * `DYNAMIC_CRC_TABLE` computes the CRC tables at run time instead of using the committed
//!   constants in `crc32.h`. The values are identical, because `crc32.h` is generated by that very
//!   code, so this is *not* an output change. It matters because it flips `zlibCompileFlags()`
//!   bit 13, which `crates/libz-rs-sys/tests/c_api_parity.rs` compares, and because `crc32.c`
//!   L13-L17 records
//!   that no mutex or semaphore guards the construction -- enabling it would introduce a data race
//!   into the oracle.
//! * `MAKECRCH` implies `DYNAMIC_CRC_TABLE` (`crc32.c` L23-L27), pins `W` to 8 regardless of
//!   target (L86-L88), and compiles in a `main()` that writes `crc32.h`. A second `main()` in a
//!   test binary is a link failure.
//! * `GEN_TREES_H` makes `tr_static_init()` compute the static trees at run time and compiles in
//!   `gen_trees_header()` (`trees.c` L83, L296, L370-L435). The computed tables equal the
//!   committed `trees.h` ones, so this too is an initialization change, not an output change.
//! * `LIT_MEM` splits `sym_buf` into `d_buf`/`l_buf` and raises `LIT_BUFS` from 4 to 5
//!   (`deflate.h` L224-L231, documented at L26-L28 as a 1-2% speed gain bought with memory). The
//!   block-flush threshold is the same *symbol* count either way -- `sym_end` is `lit_bufsize - 1`
//!   at one slot per symbol against `(lit_bufsize - 1) * 3` at three bytes per symbol
//!   (`deflate.c` L515-L521) -- so emitted bytes do not move. What moves is the per-stream memory
//!   footprint the benchmarks in `benches/` measure, and `deflatePrime`'s `Z_BUF_ERROR` boundary
//!   (L751-L758).
//! * `Z_SOLO` drops everything inside `zlib.h`'s `#ifndef Z_SOLO` region (L1258-L1799): the
//!   one-shot wrappers and the whole `gz*` file layer. The oracle would then fail to define a
//!   large part of the 95-name surface this file renames and the suites in `tests/` compare.
//! * `ZLIB_DEBUG` compiles in the `Assert`/`Trace` machinery and flips `zlibCompileFlags()`
//!   bit 8.
//!
//! # Symbol-collision resolution
//!
//! `libz-rs-sys` exports `deflate`, `inflate`, `crc32` and the rest as `#[no_mangle] extern "C"`
//! functions. Linking a plain oracle archive into the same test binary would duplicate every one
//! of them, so each externally visible C symbol is renamed to `c_<name>`. Two mechanisms are
//! needed, because neither one covers the whole surface.
//!
//! ## 1. Preprocessor renames: 95 `-D<name>=c_<name>` macros
//!
//! The rename list is derived from the PUBLIC API CONTRACT, which is the set of function names
//! `zlib.h` declares in text the preprocessor can reach: **96 distinct names**, from the 107
//! `ZEXTERN` declaration sites that remain once comment blocks are stripped. (The header holds 119
//! lines mentioning `ZEXTERN`; twelve of them are inside documentation comments, which is why a
//! `grep -c` over the raw text answers a different question and must not be used here.
//! `crates/libz-rs-sys/src/lib.rs` derives the same 96 and sets out the arithmetic in full.)
//!
//! From those 96 names, exactly one is removed:
//!
//! * **minus 1** name that is also a `#define` macro in `zlib.h`: `gzgetc`. It is declared as a
//!   function AND defined as a macro (L1966-L1968), and it is the only name in the 96 for which
//!   that is true -- the other five macro entry points, `deflateInit`, `deflateInit2`,
//!   `inflateInit`, `inflateInit2` and `inflateBackInit`, appear only inside comment blocks and are
//!   therefore not in the 96 at all.
//!
//! giving `96 - 1 = 95` macros, which is the length `PUBLIC_RENAMES` pins in its own type. The
//! rename set and the contract set are checked against each other rather than merely stated to
//! agree: `PUBLIC_RENAMES` is exactly the 96 names minus `gzgetc`.
//!
//! That subtraction is load-bearing, not tidiness. `-Dgzgetc=c_gzgetc` on the command line turns
//! `zlib.h`'s own `#define gzgetc(g) ...` into a redefinition, so every translation unit emits a
//! `"gzgetc" redefined` warning and the build fails. It would not even work if it compiled:
//! `gzread.c` does `#undef gzgetc` before defining the real function, which deletes the
//! command-line definition too. `gzgetc` is renamed on the object file instead, in pass 2 below.
//! With it excluded the fifteen files compile with zero warnings.
//!
//! Two neighbouring quantities are deliberately NOT the basis of this list, because each is a
//! different question with an answer that happens to be close by: the reference shared library
//! exports **95 function symbols** plus **16 symbol-version nodes** (the 111 dynamic globals the
//! symbol-parity gate diffs against), and 95 of the 96 names are exported on a non-Windows target
//! because `gzopen_w` is `#if defined(_WIN32)`. Neither number determines the rename list, and the
//! coincidence that two of them are 95 is exactly why each is named by the noun that defines it.
//!
//! The names are a hardcoded `const` array rather than something scraped out of `zlib.h` at
//! build time: the set is part of the frozen public contract, and a hardcoded list is auditable
//! and cannot silently shrink because a regex stopped matching.
//!
//! ## 2. `objcopy --redefine-syms`, for the 19 names macros cannot reach
//!
//! `gzgetc` is the reason this second pass exists at all. `gzread.c` does `#undef gzgetc`
//! immediately before defining the real function -- it has to, because `zlib.h` makes `gzgetc` a
//! function-like macro -- and that `#undef` deletes any command-line `-D` along with it. The
//! only way to rename it is on the object file.
//!
//! The same pass also covers the `ZLIB_INTERNAL` surface, which the 95 public renames do not
//! touch because those names are not `ZEXTERN` declarations. `inflate_table` is the concrete
//! case, and it is a property of the *C reference build*, which can be measured today: in that
//! build `inflate_table` is a defined external symbol of `libz.a` -- the unmodified
//! `test/infcover.c` calls it directly from `cover_trees()` (`test/infcover.c` L625-L638), so the
//! static archive has to keep it reachable -- while `zlib.map`'s `local:` block keeps it out of
//! `libz.so`'s dynamic symbol table entirely. Measured on the reference artifacts, `inflate_table`
//! is present once in `libz.a` and absent from all 111 dynamic globals of
//! `libz.so.1.3.2.1-motley`.
//!
//! The Rust facade is *intended* to reproduce that same split, and it is built to: its `inflate`
//! module exports `inflate_table` as a real symbol so `libz.a` keeps it reachable, while
//! `zlib.map` -- applied by the packaging relink, not by rustc -- keeps it out of `libz.so`'s
//! dynamic symbol table. `crates/libz-rs-sys/tests/symbol_parity.rs` asserts that split inside
//! `cargo test`, `nm`-ing both artifacts and requiring `inflate_table` to be a defined global in
//! `libz.a` and absent from `libz.so`'s dynamic table, and `Makefile.in`'s `rust-test` target
//! checks the same property against the staged library. Either way the consequence for *this*
//! file is
//! unconditional: a Rust `libz.a` that keeps `inflate_table` reachable and an un-renamed oracle
//! archive would both define the name, and the test binary would not link.
//!
//! The 19 names are the union of both `local:` blocks in `zlib.map` (`ZLIB_1.2.0` and
//! `ZLIB_1.3.2`) with the `_tr_*` / `_dist_code` / `_length_code` family that `zlib.map` covers
//! there under its `_*` wildcard. That union is the specification the list is derived from, and
//! it is intended to be exactly the set of defined external symbols that survives the
//! preprocessor pass -- no more, no less. `verify_every_export_is_prefixed()` below is the
//! in-build check that covers one direction of it, and it runs on every build of this crate: it
//! fails the build if *any* defined external symbol of the archive is left without the `c_`
//! prefix. It deliberately does not police the other direction, so a name listed here that the C
//! sources no longer define would go unnoticed rather than reported.
//!
//! `objcopy` rewrites undefined references as well as definitions, so cross-translation-unit
//! calls stay wired up: after the pass `deflate.o` refers to `c__tr_init`, not `_tr_init`. The
//! pass is also idempotent, because a second run finds no un-prefixed name left to match.
//!
//! A note on the alternative: compiling with `-DZ_PREFIX` would rename most of this surface in
//! one step, and it handles `gzgetc` (`gzread.c` undefines `z_gzgetc` instead when
//! `Z_PREFIX_SET` is defined). It is not used here for two reasons. The prefix the harness is
//! specified to expose is `c_`, not `z_`; and `zconf.h` states that standard zlib is compiled
//! *without* `Z_PREFIX`, which puts it at odds with the requirement that the oracle be the
//! default configuration.
//!
//! # Why a generated shim is required for the tables
//!
//! Every generated table is unreachable by the linker. `zutil.h` defines `local` as `static`,
//! and each table is declared with it: `crc_table`, `crc_big_table`, `crc_braid_table`,
//! `crc_braid_big_table` and `x2n_table` in `crc32.h`; `static_ltree`, `static_dtree`,
//! `base_length` and `base_dist` in `trees.h`; `lenfix` and `distfix` in `inffixed.h` are plain
//! `static`. A table-equality test therefore cannot declare any of them `extern "C"` -- there is
//! no symbol to bind to.
//!
//! The fix is a small C file, generated into `OUT_DIR`, that includes those three headers and
//! returns pointers to the tables from `c_oracle_*` accessor functions. It is compiled with the
//! same flags as the oracle and lands in the same archive. Every array accessor is paired with a
//! length accessor so the Rust side never hardcodes a bound.
//!
//! Three details in that shim are easy to get wrong and each one is handled explicitly:
//!
//! * `trees.h` also *defines* `_dist_code` and `_length_code` as non-`static` globals, so the
//!   shim would duplicate the definitions already in `trees.o`. It is compiled with
//!   `-D_dist_code=c_shim_dist_code -D_length_code=c_shim_length_code` to give its copies
//!   private names -- still `c_`-prefixed, so the archive has no un-prefixed export at all.
//! * `trees.h` references `DIST_CODE_LEN`, which is defined in `trees.c` and appears in no
//!   header, so the shim has to define it itself.
//! * `crc32.h` selects between many `#if`-guarded variants of `crc_braid_table` and
//!   `crc_big_table` using `N`, `W` and `z_word_t`. The shim replicates `crc32.c`'s own
//!   derivation of those three verbatim; deriving them any other way silently compiles the wrong
//!   variant.
//!
//! # Why the configuration table needs a second, different generated file
//!
//! One generated table is out of even that shim's reach. `configuration_table` -- the per-level
//! `{good_length, max_lazy, nice_length, max_chain, func}` rows at `deflate.c` L112-L124 -- is
//! `local` like the others, but unlike the others it is not declared in any header: it lives
//! inside `deflate.c`, so no file can include it. Including a header is what the table shim does,
//! and there is no header to include.
//!
//! That table is not an incidental omission to leave for later. It is the *first* of the eight
//! decision points that determine whether compressed output is byte-identical to the reference:
//! its four numbers per level decide which candidate matches `longest_match` examines and accepts,
//! and therefore which literal/length/distance symbols reach the Huffman coder. A single altered
//! digit changes the emitted bytes at that level for essentially every input.
//!
//! The only code that can read a `static` object is code compiled into the same translation unit,
//! so the fix is a second generated file -- `zlib_c_oracle_deflate.c` -- that `#include`s the
//! authoritative `deflate.c` and appends the accessors after it. `build.rs` compiles that file
//! *instead of* plain `deflate.c`, so the reference translation unit is in the archive exactly
//! once, under exactly the same flags and the same 95 `-D` renames as before.
//!
//! Two properties of this arrangement are the whole point of doing it this way:
//!
//! * **It compares the C object, not a copy of it.** Transcribing the forty numbers into a
//!   separate shim would produce a test that compares one transcription against another: if the
//!   value was copied wrongly, it would be copied wrongly into both sides and the comparison
//!   would agree. Reading the actual `configuration_table` is what makes the later
//!   table-equality gate able to fail.
//! * **The compressor is identified by function pointer.** Each row's fifth member is a
//!   `compress_func`, and `deflateParams` compares two rows' `func` fields **by identity**
//!   (`deflate.c` L791) to decide whether a level change must first flush the open block. The
//!   accessor therefore compares the stored address against `deflate_stored`, `deflate_fast`,
//!   `deflate_slow`, `deflate_rle` and `deflate_huff` -- all five `local` to `deflate.c`, and all
//!   five reachable from the same vantage point the table is -- and reports a small stable
//!   integer. That grouping is what the port has to reproduce, because getting it wrong makes
//!   `deflateParams` flush where C does not, or not flush where C does, and either changes the
//!   emitted bytes.
//!
//! `deflate.c` stays in `C_SOURCES`. That array is the authoritative fifteen-file set, and it is
//! what drives both the `rerun-if-changed` list and the repository-root verification;
//! [`compile_reference_sources`] is the single place that substitutes the generated file for it,
//! and it fails the build unless the substitution matches exactly one entry -- no match would mean
//! `deflate.c` is compiled twice, two would mean the source set is not what this file thinks.
//!
//! # Failure posture
//!
//! This script fails loudly. The "library code must not panic" rule constrains library paths and
//! deliberately does not apply here: a build script that swallowed an error to keep the build
//! green would produce an archive with a missing or half-renamed oracle, and every differential
//! test would then pass without comparing anything. A false pass is the worst outcome this crate
//! can produce, so every error path aborts with a diagnostic that names the artifact, the
//! command and the invariant at stake.
//!
//! # Platform notes
//!
//! Supported hosts are Rust's Tier 1 set, which is what the port targets: Linux (gnu and musl),
//! macOS and Windows on `x86_64` and `aarch64`, under either the `gnu` or the `msvc` environment.
//! Two of the external tools this script drives differ across that set, and neither difference
//! is optional to handle -- one is a missing program, the other an incompatible command line.
//!
//! ## `objcopy` for the rename pass
//!
//! The rename pass needs an `objcopy` that understands `--redefine-syms`. `OBJCOPY` overrides the
//! choice; otherwise [`locate_objcopy`] probes `PATH` for `objcopy`, `llvm-objcopy` and
//! `gobjcopy`, and then -- this is the part that makes the crate buildable off a GNU host -- the
//! bin directory of the active toolchain's own sysroot, for `llvm-objcopy` and `rust-objcopy`.
//! On macOS it additionally asks `xcrun`.
//!
//! The sysroot candidate matters because a stock host may have none of the three `PATH` names.
//! Stock macOS ships `nm` and `otool` but no `objcopy` at all; `gobjcopy` is Homebrew binutils,
//! not a system tool. A Windows MSVC host has no `objcopy` either. Every rustup toolchain, on
//! every one of those hosts, ships `rust-objcopy` in its own sysroot -- and it is a real
//! `llvm-objcopy`, not a stub. Measured on this machine: it answers "llvm-objcopy, compatible
//! with GNU objcopy" and lists `--redefine-syms=filename`. So the fallback is not a
//! nice-to-have; it is what makes this crate buildable off a GNU host at all, with nothing
//! installed beyond the toolchain itself.
//!
//! One subtlety worth writing down, because it inverts what a shell test would tell you.
//! `rust-objcopy` links against the toolchain's own `libLLVM`, which is not on the default
//! loader path, so running it straight from a terminal fails with `error while loading shared
//! libraries: libLLVM...`. Inside a build script it works, because cargo puts the toolchain's
//! `lib` directory on the dynamic library path for the scripts it runs. Both were measured here:
//! the same binary is unusable from a shell and usable from this script. That is exactly why
//! every candidate is probed by RUNNING it in this process rather than by testing for the file --
//! a file test would have accepted a broken program, and a shell test would have rejected a
//! working one.
//!
//! Where even that fails, `rustup component add llvm-tools` installs `llvm-objcopy` beside the
//! toolchain, and that is the remedy the failure message names: one rustup command works
//! identically on every supported host, whereas naming a package manager only helps on the host
//! that has it.
//!
//! ## The archiver
//!
//! The archive is assembled with the archiver `cc` selects, but NOT with one fixed command line.
//! `ar crs <archive> <objects>` is the Unix spelling, accepted by GNU `ar`, LLVM `ar` and BSD
//! `ar`. Under the `msvc` environment `cc` selects `lib.exe`, which rejects that spelling
//! outright: it takes `/OUT:<archive>` and no operation letters. [`archive_objects`] therefore
//! chooses the form, and the archive NAME with it -- `libz.a`-style `lib<stem>.a` for Unix,
//! `<stem>.lib` for MSVC, because `cargo::rustc-link-lib=static=<stem>` makes rustc look for
//! exactly those two spellings and nothing else.
//!
//! # Unsafe
//!
//! There is none, and `#![forbid(unsafe_code)]` below makes that a compiler-enforced property
//! rather than a claim. This script drives `cc`, `objcopy`, `ar` and `nm` through
//! [`std::process::Command`] and inspects their output as bytes; nothing it does needs a raw
//! pointer. The crate's `unsafe` lives in exactly two places -- `src/oracle.rs` and `src/port.rs`,
//! its two FFI boundary modules -- and this file is deliberately not a third.

#![forbid(unsafe_code)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Base name of the oracle archive. Deliberately not `z`: `libzlib_c_oracle.a` can never be
/// confused with a system `libz.a` on a link line.
const ARCHIVE_STEM: &str = "zlib_c_oracle";

/// File name of the generated table-exposure shim inside `OUT_DIR`.
const SHIM_FILE_NAME: &str = "zlib_c_oracle_tables.c";

/// File name of the generated `deflate.c` wrapper inside `OUT_DIR`.
///
/// Deliberately not `deflate.c`: a distinct name keeps the two apart in build logs, in
/// `OUT_DIR` listings and in any debugger, and makes it obvious which one a diagnostic refers to.
const CONFIG_TU_FILE_NAME: &str = "zlib_c_oracle_deflate.c";

/// File name of the generated `trees.c` wrapper inside `OUT_DIR`.
///
/// Named on the same principle as [`CONFIG_TU_FILE_NAME`], and for the same reason: a build log
/// that says `zlib_c_oracle_trees.c` cannot be mistaken for one that says `trees.c`.
const TREES_TU_FILE_NAME: &str = "zlib_c_oracle_trees.c";

/// File name of the `objcopy --redefine-syms` map inside `OUT_DIR`.
const REDEFINE_MAP_FILE_NAME: &str = "zlib_c_oracle_redefine.map";

/// The prefix every externally visible symbol in the archive must carry.
const SYMBOL_PREFIX: &str = "c_";

// -------------------------------------------------------------------------------------------------
//  Baseline identity published to this crate's benches
// -------------------------------------------------------------------------------------------------
//
//  A ratio between two implementations is only a fact about the implementations if both were built
//  the same way, and the two sides here are *not* built by the same tool: the oracle below is
//  compiled by `cc`, one translation unit at a time, while the port is compiled by rustc under a
//  cargo profile that the AAP fixes at fat LTO with a single codegen unit (AAP §0.3.1, root
//  `Cargo.toml` `[profile.release]`). That asymmetry is real and it favours the port, so it has to
//  travel with every number rather than live in a reviewer's memory.
//
//  These four variables are what makes that possible. Each is forwarded with
//  `cargo::rustc-env`, which applies to every target in this package -- including the three
//  `[[bench]]` targets -- so a bench reads them with `env!` and prints what was *actually*
//  compiled instead of a hand-copied duplicate that can silently drift from
//  [`reference_build`].

/// The full C compiler command-line flags, verbatim from `cc`'s own [`cc::Tool`].
///
/// Not a transcription: `cc` also absorbs `CFLAGS`, `TARGET_CFLAGS` and `HOST_CFLAGS` from the
/// environment, so the only trustworthy source for "what the oracle was compiled with" is the tool
/// object `cc` is about to invoke.
const C_FLAGS_ENV: &str = "ZLIB_RS_DIFFERENTIAL_C_FLAGS";

/// A short, space-free token summarising the oracle's build shape, for a `c_profile=` log field.
const C_PROFILE_TAG_ENV: &str = "ZLIB_RS_DIFFERENTIAL_C_PROFILE_TAG";

/// The cargo profile label the invoker declared for the Rust side, or `undeclared`.
const RUST_PROFILE_ENV: &str = "ZLIB_RS_DIFFERENTIAL_RUST_PROFILE";

/// The rustc optimisation level and profile kind cargo reported for this build, plus the
/// `RUSTFLAGS` in force -- the parts of the Rust side's identity cargo will actually tell a build
/// script.
const RUST_BUILD_ENV: &str = "ZLIB_RS_DIFFERENTIAL_RUST_BUILD";

/// The variable an invoker sets to declare which cargo profile is driving a measurement run.
///
/// Cargo does not expose a custom profile's *name* to a build script (it reports only `PROFILE`,
/// which flattens every release-like profile to `release`), so the name has to be declared. It is
/// read here, at build time, and baked into the binary rather than read at run time, so the label
/// travels with the artifact and a runner cannot relabel a binary it did not build.
const RUST_PROFILE_DECLARATION_VAR: &str = "ZLIB_RS_BENCH_RUST_PROFILE";

/// The two profiles a measurement run may declare, and the identity each one asserts.
///
/// `bench` is the shipped measurement profile (fat LTO, one codegen unit); `bench-parity` is the
/// deliberately non-LTO, multi-codegen-unit profile whose whole purpose is to be structurally
/// comparable to the per-translation-unit oracle. Both are release-like and both are `-O3`, which
/// is what [`emit_baseline_metadata`] cross-checks a declaration against.
const MEASUREMENT_PROFILES: [&str; 2] = ["bench", "bench-parity"];

/// The label used when no profile was declared.
const UNDECLARED_PROFILE: &str = "undeclared";

/// The fifteen reference translation units, in `Makefile.in` `OBJZ` + `OBJG` order.
const C_SOURCES: [&str; 15] = [
    // OBJZ
    "adler32.c",
    "crc32.c",
    "deflate.c",
    "infback.c",
    "inffast.c",
    "inflate.c",
    "inftrees.c",
    "trees.c",
    "zutil.c",
    // OBJG
    "compress.c",
    "uncompr.c",
    "gzclose.c",
    "gzlib.c",
    "gzread.c",
    "gzwrite.c",
];

/// The member of [`C_SOURCES`] that carries `configuration_table`, compiled through a generated
/// wrapper instead of directly.
///
/// `configuration_table` is `local` to `deflate.c` and declared in no header, so the only way to
/// read it is from code compiled into that same translation unit. [`CONFIG_TU_SOURCE`] is that
/// code, and [`compile_reference_sources`] substitutes it for this entry -- so `deflate.c` is
/// still compiled exactly once, with the same flags and the same renames, and is still listed in
/// [`C_SOURCES`] for the `rerun-if-changed` and repository-root checks.
const CONFIG_TU_ORIGIN: &str = "deflate.c";

/// The member of [`C_SOURCES`] that carries the four `trees.c`-local tables, compiled through a
/// generated wrapper instead of directly.
///
/// `extra_lbits`, `extra_dbits`, `extra_blbits` and `bl_order` (`trees.c` L62, L65, L68 and L71)
/// are in exactly the position `configuration_table` is: declared `local`, and declared in no
/// header -- `trees.h` carries only the six tables `-DGEN_TREES_H` generates, and these four are
/// not among them. So they are reachable only from code compiled into `trees.c`'s own translation
/// unit, and [`TREES_TU_SOURCE`] is that code.
const TREES_TU_ORIGIN: &str = "trees.c";

/// Every member of [`C_SOURCES`] reached through a generated wrapper, with the wrapper that
/// carries it.
///
/// Each entry is `(origin, wrapper file name, wrapper source)`. [`write_wrapper_tus`] writes them
/// and [`compile_reference_sources`] substitutes each `origin` for its wrapper, so every reference
/// translation unit is still compiled exactly once with the identical flags and renames. Keeping
/// the set in one place is what lets both of those functions check themselves rather than trust a
/// hand-maintained count.
const WRAPPER_TUS: [(&str, &str, &str); 2] = [
    (CONFIG_TU_ORIGIN, CONFIG_TU_FILE_NAME, CONFIG_TU_SOURCE),
    (TREES_TU_ORIGIN, TREES_TU_FILE_NAME, TREES_TU_SOURCE),
];

/// Every header the oracle or the generated shim consumes, so that editing one rebuilds the
/// archive. `zconf.h` is committed in the tree (it is a `configure`/`CMake` product, but the
/// generated copy is tracked), which is why `-I<repo root>` is sufficient and why this script
/// never runs `configure` -- doing so would overwrite tracked files.
const C_HEADERS: [&str; 11] = [
    "zlib.h",
    "zconf.h",
    "zutil.h",
    "deflate.h",
    "inflate.h",
    "inftrees.h",
    "inffast.h",
    "gzguts.h",
    "crc32.h",
    "trees.h",
    "inffixed.h",
];

/// The 95 public names renamed by the preprocessor, `-D<name>=c_<name>`.
///
/// Derived as `94 + 7 - 6` exactly as the module documentation describes: the 94 unique
/// column-zero `ZEXTERN` names in `zlib.h`, plus the 7 large-file names declared only inside the
/// indented `Z_LARGE64` / `Z_WANT64` block, minus the 6 names `zlib.h` defines as macros.
///
/// The list is sorted, so a diff against a freshly extracted set is a one-line comparison. Do not
/// add `deflateInit`, `deflateInit2`, `inflateInit`, `inflateInit2`, `inflateBackInit` or
/// `gzgetc`: they are macros, and defining them here makes every translation unit fail to
/// compile. `gzgetc` is handled by the `objcopy` pass instead.
///
/// `gzopen_w` is declared only on Windows. Renaming a name no translation unit defines is
/// harmless, and keeping it here means a Windows build needs no separate list.
const PUBLIC_RENAMES: [&str; 95] = [
    "adler32",
    "adler32_combine",
    "adler32_combine64",
    "adler32_z",
    "compress",
    "compress2",
    "compress2_z",
    "compressBound",
    "compressBound_z",
    "compress_z",
    "crc32",
    "crc32_combine",
    "crc32_combine64",
    "crc32_combine_gen",
    "crc32_combine_gen64",
    "crc32_combine_op",
    "crc32_z",
    "deflate",
    "deflateBound",
    "deflateBound_z",
    "deflateCopy",
    "deflateEnd",
    "deflateGetDictionary",
    "deflateInit2_",
    "deflateInit_",
    "deflateParams",
    "deflatePending",
    "deflatePrime",
    "deflateReset",
    "deflateResetKeep",
    "deflateSetDictionary",
    "deflateSetHeader",
    "deflateTune",
    "deflateUsed",
    "get_crc_table",
    "gzbuffer",
    "gzclearerr",
    "gzclose",
    "gzclose_r",
    "gzclose_w",
    "gzdirect",
    "gzdopen",
    "gzeof",
    "gzerror",
    "gzflush",
    "gzfread",
    "gzfwrite",
    "gzgetc_",
    "gzgets",
    "gzoffset",
    "gzoffset64",
    "gzopen",
    "gzopen64",
    "gzopen_w",
    "gzprintf",
    "gzputc",
    "gzputs",
    "gzread",
    "gzrewind",
    "gzseek",
    "gzseek64",
    "gzsetparams",
    "gztell",
    "gztell64",
    "gzungetc",
    "gzvprintf",
    "gzwrite",
    "inflate",
    "inflateBack",
    "inflateBackEnd",
    "inflateBackInit_",
    "inflateCodesUsed",
    "inflateCopy",
    "inflateEnd",
    "inflateGetDictionary",
    "inflateGetHeader",
    "inflateInit2_",
    "inflateInit_",
    "inflateMark",
    "inflatePrime",
    "inflateReset",
    "inflateReset2",
    "inflateResetKeep",
    "inflateSetDictionary",
    "inflateSync",
    "inflateSyncPoint",
    "inflateUndermine",
    "inflateValidate",
    "uncompress",
    "uncompress2",
    "uncompress2_z",
    "uncompress_z",
    "zError",
    "zlibCompileFlags",
    "zlibVersion",
];

/// The 19 names `objcopy --redefine-syms` renames on the object files.
///
/// This is the union of both `local:` blocks in `zlib.map` -- `deflate_copyright`,
/// `inflate_copyright`, `inflate_fast`, `inflate_table`, `zcalloc`, `zcfree`, `z_errmsg`,
/// `gz_error`, `gz_intmax` from `ZLIB_1.2.0` and `inflate_fixed` from `ZLIB_1.3.2` -- together
/// with the `_tr_*` / `_dist_code` / `_length_code` family that `zlib.map` hides there through
/// its `_*` wildcard, plus `gzgetc`, which no `-D` can reach because `gzread.c` undefines it.
///
/// Measured against a real build this is precisely the set of defined external symbols left
/// un-prefixed after `PUBLIC_RENAMES` is applied. The `nm` gate at the end of this script is what
/// keeps that claim true as the reference sources evolve.
const INTERNAL_REDEFINES: [&str; 19] = [
    "_dist_code",
    "_length_code",
    "_tr_align",
    "_tr_flush_bits",
    "_tr_flush_block",
    "_tr_init",
    "_tr_stored_block",
    "_tr_tally",
    "deflate_copyright",
    "gz_error",
    "gz_intmax",
    "gzgetc",
    "inflate_copyright",
    "inflate_fast",
    "inflate_fixed",
    "inflate_table",
    "z_errmsg",
    "zcalloc",
    "zcfree",
];

/// Renames applied to the generated shim only.
///
/// `trees.h` *defines* these two as non-`static` globals, so including it from the shim would
/// duplicate the definitions already emitted by `trees.o`. Renaming the shim's copies keeps the
/// archive linkable, and the `c_shim_` spelling keeps every export in the archive `c_`-prefixed.
const SHIM_LOCAL_RENAMES: [(&str, &str); 2] = [
    ("_dist_code", "c_shim_dist_code"),
    ("_length_code", "c_shim_length_code"),
];

/// The only `__`-prefixed globals the oracle archive may export, as (prefix, kind) pairs.
///
/// [`verify_every_export_is_prefixed`] tolerates a defined global beginning with `__` only when it
/// matches one of these families AND its payload is accountable, which for an ODR indicator means
/// naming a global this script itself renames. Everything else fails the build.
///
/// ★ MEASURED, not assumed. Building this crate with `RUSTFLAGS=-Zsanitizer=address` and
/// `CFLAGS='-fsanitize=address -fno-omit-frame-pointer -O1 -g'` -- the exact configuration the
/// `asan` job of `.github/workflows/rust.yml` uses -- leaves precisely seven such symbols, all of
/// them ODR indicators:
///
/// ```text
/// __odr_asan._dist_code          __odr_asan.c_shim_dist_code    __odr_asan.deflate_copyright
/// __odr_asan._length_code        __odr_asan.c_shim_length_code  __odr_asan.inflate_copyright
///                                                              __odr_asan.z_errmsg
/// ```
///
/// Two spellings appear because the two rename mechanisms differ in what the indicator is named
/// after. `objcopy --redefine-syms` rewrites the DATA symbol after compilation, so the indicator
/// keeps the source spelling (`__odr_asan.z_errmsg` beside the renamed `c_z_errmsg`); the shim's
/// two copies are renamed by `-D` before the compiler sees them, so their indicators carry the new
/// spelling (`__odr_asan.c_shim_dist_code`). Both are checked against the tables rather than
/// pattern-matched loosely.
///
/// An ODR indicator can never collide with `libz-rs-sys`: it is emitted by the C compiler's
/// sanitizer instrumentation, rustc emits no symbol of that shape, and it is referenced only by
/// the ASan runtime's own duplicate-global detection.
///
/// `__x86.get_pc_thunk.*` is the second family, and it is here for the 32-bit `-fPIC` leg of the
/// `integer-models` job: gcc emits one PC thunk per register on i686, they are `.gnu.linkonce`
/// COMDAT globals, and rustc's own i686 objects contain identical copies that the linker folds.
const TOOLCHAIN_SYMBOL_FAMILIES: [(&str, ToolchainSymbol); 3] = [
    // gcc / clang, `-fsanitize=address`: one ODR indicator per instrumented global.
    ("__odr_asan.", ToolchainSymbol::OdrIndicator),
    // clang's older spelling of the same thing, kept so a clang-driven `cc` build is not a
    // surprise failure on a developer's machine.
    ("__odr_asan_gen_", ToolchainSymbol::OdrIndicator),
    // gcc, 32-bit x86 `-fPIC`: position-independent-code thunks.
    ("__x86.get_pc_thunk.", ToolchainSymbol::PcThunk),
];

/// What a [`TOOLCHAIN_SYMBOL_FAMILIES`] entry describes, and therefore how its payload is checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolchainSymbol {
    /// An AddressSanitizer ODR indicator. The payload must name a global this script renames,
    /// under either its source spelling or its renamed one.
    OdrIndicator,
    /// A 32-bit x86 PC thunk. The payload is a register name; any non-empty payload is accepted,
    /// because the register set is the compiler's business and none of the spellings can collide
    /// with a zlib name.
    PcThunk,
}

/// The generated table-exposure shim, written verbatim into `OUT_DIR`.
///
/// Kept as one deterministic string constant: the file has no timestamp, no host paths and no
/// configuration-dependent content, so an unchanged build regenerates a byte-identical shim and
/// nothing downstream is invalidated spuriously.
///
/// The `N` / `W` / `z_word_t` block is a verbatim transcription of `crc32.c`, including its
/// `Z_TESTN` and `Z_TESTW` escape hatches. That fidelity is the point: `crc32.h` contains one
/// `crc_braid_table` and one `crc_braid_big_table` per `(N, W)` combination, and the only way to
/// be sure the shim sees the same variant `crc32.c` compiled is to derive `N` and `W` the same
/// way rather than re-deriving them independently.
///
/// The word-typed tables (`crc_big_table`, `crc_braid_big_table`) are handed out as `const void *`
/// with a companion `c_oracle_word_size()`, because their element type is `z_word_t` -- 8 bytes
/// where `W == 8`, 4 bytes where `W == 4`, and non-existent where a braided calculation is
/// compiled out entirely. A `void` pointer plus an explicit element width keeps one stable C
/// signature across all three cases; `crc_braid_table` is `z_crc_t` in every variant and so keeps
/// its concrete type.
const TABLE_SHIM_SOURCE: &str = r#"/*
 * zlib_c_oracle_tables.c -- GENERATED FILE. DO NOT EDIT.
 *
 * Written into OUT_DIR by crates/zlib-rs-differential/build.rs; edit that script instead.
 *
 * Every generated table in the reference implementation is declared `local`, and zutil.h defines
 * `local` as `static`, so none of them has a linkable symbol.  This file includes the three
 * generated-table headers and hands out pointers to the tables through c_oracle_* accessors so
 * that the differential table-equality suite can compare the Rust ports against the C arrays
 * element for element.
 *
 * This covers every table a HEADER declares, which is every table but one: deflate.c's
 * configuration_table is declared in no header, so it is reached by the sibling generated file
 * zlib_c_oracle_deflate.c, which includes deflate.c itself.  Nothing about it belongs here.
 *
 * Every array accessor is paired with a length accessor: the Rust side must never hardcode a
 * bound that the C headers own.
 */

/* zutil.h pulls in zlib.h and zconf.h, and supplies z_crc_t, FAR, local, ZLIB_INTERNAL, uch,
 * Z_U4 and Z_U8. */
#include "zutil.h"
/* deflate.h supplies ct_data, L_CODES, D_CODES, LENGTH_CODES, MIN_MATCH and MAX_MATCH, all of
 * which trees.h uses to dimension its arrays. */
#include "deflate.h"
/* inftrees.h supplies `code`, the element type of the fixed inflate tables. */
#include "inftrees.h"

/* ---------------------------------------------------------------------------------------------
 * N, W and z_word_t, transcribed verbatim from crc32.c.
 *
 * crc32.h holds a separate crc_braid_table and crc_braid_big_table for every (N, W) pair and
 * selects between them with #if.  Deriving N and W any other way here would silently compile a
 * different variant than crc32.c did, and the table comparison would then be meaningless.
 * ------------------------------------------------------------------------------------------- */

#ifdef Z_TESTN
#  define N Z_TESTN
#else
#  define N 5
#endif
#if N < 1 || N > 6
#  error N must be in 1..6
#endif

#ifdef Z_TESTW
#  if Z_TESTW-1 != -1
#    define W Z_TESTW
#  endif
#else
#  if defined(__x86_64__) || defined(__aarch64__)
#    define W 8
#  else
#    define W 4
#  endif
#endif
#ifdef W
#  if W == 8 && defined(Z_U8)
     typedef Z_U8 z_word_t;
#  elif defined(Z_U4)
#    undef W
#    define W 4
     typedef Z_U4 z_word_t;
#  else
#    undef W
#  endif
#endif

#include "crc32.h"

/* DIST_CODE_LEN is defined in trees.c and appears in no header, but trees.h dimensions
 * _dist_code with it. */
#define DIST_CODE_LEN 512
#include "trees.h"

#include "inffixed.h"

/* Element count of a statically sized array.  Applied to a two-dimensional table it yields the
 * row count; applied to [0] of the same table it yields the column count. */
#define ORACLE_LEN(a) ((unsigned)(sizeof(a) / sizeof((a)[0])))

/* --- crc32.h ------------------------------------------------------------------------------- */

const z_crc_t *c_oracle_crc_table(void) { return crc_table; }
unsigned c_oracle_crc_table_len(void) { return ORACLE_LEN(crc_table); }

const z_crc_t *c_oracle_x2n_table(void) { return x2n_table; }
unsigned c_oracle_x2n_table_len(void) { return ORACLE_LEN(x2n_table); }

/* Number of braids.  Always defined, even when the braided path is compiled out. */
unsigned c_oracle_crc_braid_n(void) { return (unsigned)N; }

#ifdef W

/* Bytes per word, and the row count of both braid tables: crc32.c declares them as
 * crc_braid_table[W][256] and crc_braid_big_table[W][256]. */
unsigned c_oracle_crc_braid_w(void) { return (unsigned)W; }
unsigned c_oracle_word_size(void) { return (unsigned)sizeof(z_word_t); }

const void *c_oracle_crc_big_table(void) { return (const void *)crc_big_table; }
unsigned c_oracle_crc_big_table_len(void) { return ORACLE_LEN(crc_big_table); }

const z_crc_t *c_oracle_crc_braid_table(void) { return &crc_braid_table[0][0]; }
unsigned c_oracle_crc_braid_table_rows(void) { return ORACLE_LEN(crc_braid_table); }
unsigned c_oracle_crc_braid_table_cols(void) { return ORACLE_LEN(crc_braid_table[0]); }

const void *c_oracle_crc_braid_big_table(void) { return (const void *)crc_braid_big_table; }
unsigned c_oracle_crc_braid_big_table_rows(void) { return ORACLE_LEN(crc_braid_big_table); }
unsigned c_oracle_crc_braid_big_table_cols(void) { return ORACLE_LEN(crc_braid_big_table[0]); }

#else /* !W -- no braided calculation, so crc32.h defined none of the word tables. */

unsigned c_oracle_crc_braid_w(void) { return 0u; }
unsigned c_oracle_word_size(void) { return 0u; }

const void *c_oracle_crc_big_table(void) { return NULL; }
unsigned c_oracle_crc_big_table_len(void) { return 0u; }

const z_crc_t *c_oracle_crc_braid_table(void) { return NULL; }
unsigned c_oracle_crc_braid_table_rows(void) { return 0u; }
unsigned c_oracle_crc_braid_table_cols(void) { return 0u; }

const void *c_oracle_crc_braid_big_table(void) { return NULL; }
unsigned c_oracle_crc_braid_big_table_rows(void) { return 0u; }
unsigned c_oracle_crc_braid_big_table_cols(void) { return 0u; }

#endif /* W */

/* --- trees.h ------------------------------------------------------------------------------- */

const ct_data *c_oracle_static_ltree(void) { return static_ltree; }
unsigned c_oracle_static_ltree_len(void) { return ORACLE_LEN(static_ltree); }

const ct_data *c_oracle_static_dtree(void) { return static_dtree; }
unsigned c_oracle_static_dtree_len(void) { return ORACLE_LEN(static_dtree); }

const int *c_oracle_base_length(void) { return base_length; }
unsigned c_oracle_base_length_len(void) { return ORACLE_LEN(base_length); }

const int *c_oracle_base_dist(void) { return base_dist; }
unsigned c_oracle_base_dist_len(void) { return ORACLE_LEN(base_dist); }

/* _dist_code and _length_code reach this file renamed to c_shim_dist_code and
 * c_shim_length_code, so that the shim's copies do not clash with the definitions in trees.o. */
const uch *c_oracle_dist_code(void) { return _dist_code; }
unsigned c_oracle_dist_code_len(void) { return ORACLE_LEN(_dist_code); }

const uch *c_oracle_length_code(void) { return _length_code; }
unsigned c_oracle_length_code_len(void) { return ORACLE_LEN(_length_code); }

/* --- inffixed.h ---------------------------------------------------------------------------- */

const code *c_oracle_lenfix(void) { return lenfix; }
unsigned c_oracle_lenfix_len(void) { return ORACLE_LEN(lenfix); }

const code *c_oracle_distfix(void) { return distfix; }
unsigned c_oracle_distfix_len(void) { return ORACLE_LEN(distfix); }

/* --- layout, so the Rust mirrors of these two structs can be checked rather than assumed --- */

unsigned c_oracle_ct_data_size(void) { return (unsigned)sizeof(ct_data); }
unsigned c_oracle_code_size(void) { return (unsigned)sizeof(code); }
"#;

/// The generated `deflate.c` wrapper, written verbatim into `OUT_DIR`.
///
/// Like [`TABLE_SHIM_SOURCE`] this is one deterministic constant with no timestamp, host path or
/// configuration-dependent content, so an unchanged build regenerates a byte-identical file.
///
/// It is a *wrapper*, not a copy. The `#include "deflate.c"` line reaches the authoritative file
/// through the `-I<repo root>` search path [`reference_build`] already sets, which is what keeps
/// the reference source read-only and keeps the forty numbers of `configuration_table` in exactly
/// one place. The accessors then sit in the same translation unit as the table, which is the only
/// vantage point from which a `static` object can be read at all.
///
/// The `unsigned` return type of the four field accessors is deliberate. C declares those fields
/// as `ush` (`unsigned short`), and an accessor returning `unsigned short` would silently truncate
/// if the reference ever widened them; `unsigned` cannot. `c_oracle_config_field_size` reports the
/// C compiler's own `sizeof` of one of those fields alongside, so the Rust port's choice of `u16`
/// is checked rather than assumed -- the same discipline `c_oracle_ct_data_size` and
/// `c_oracle_code_size` apply to the two mirrored struct types.
const CONFIG_TU_SOURCE: &str = r#"/*
 * zlib_c_oracle_deflate.c -- GENERATED FILE. DO NOT EDIT.
 *
 * Written into OUT_DIR by crates/zlib-rs-differential/build.rs; edit that script instead.
 *
 * This file IS the reference deflate.c, plus accessors.  It includes the authoritative source
 * verbatim -- through the -I<repo root> search path, so the file in the tree is what the compiler
 * reads and nothing is copied -- and build.rs compiles this file INSTEAD of plain deflate.c, so
 * the translation unit lands in the archive exactly once with the same flags and the same c_
 * renames as every other reference source.
 *
 * Why an include and not a shim of its own: configuration_table (deflate.c L112-L124) is declared
 * `local`, zutil.h defines `local` as `static`, and -- unlike crc_table, static_ltree or lenfix --
 * it is declared in NO header.  There is nothing for a separate file to include and no symbol for
 * a linker to bind, so the only code that can read the table is code compiled into deflate.c's own
 * translation unit.  Transcribing the values into a shim instead would compare one transcription
 * against another: a mistake would appear identically on both sides and the comparison would
 * agree.  Reading the C object itself is what lets the table-equality suite fail.
 *
 * Why the table matters this much: it is the first of the eight decision points that determine
 * byte-identical output.  Its four numbers per level decide which candidate matches longest_match
 * examines and accepts, so one altered digit changes the emitted bytes at that level for
 * essentially every input.  deflate.c reads it at five sites: lm_init L689-L692, deflateParams
 * L789, L791 and L809-L812, and deflate L1220.
 */

#include "deflate.c"

/* Row count of the table, taken from the table itself so no bound is ever hardcoded.  Ten in the
 * default configuration; two under FASTEST (L106-L110), which this build deliberately does not
 * define.  ORACLE_LEN is spelled out here rather than shared with the table shim because the two
 * files are separate translation units. */
#define ORACLE_CONFIG_LEN \
    ((unsigned)(sizeof(configuration_table) / sizeof(configuration_table[0])))

/* The discriminators c_oracle_config_func returns.  Macros rather than an enum so the return type
 * stays a plain int and the Rust declaration needs no assumption about C enum width.
 *
 * ORACLE_CONFIG_FUNC_UNKNOWN can only happen if deflate.c gained a sixth compressor, and
 * ORACLE_CONFIG_FUNC_RANGE only if a caller asked for a level the table does not have; both are
 * negative so neither can be mistaken for one of the five families. */
#define ORACLE_CONFIG_FUNC_STORED 0
#define ORACLE_CONFIG_FUNC_FAST   1
#define ORACLE_CONFIG_FUNC_SLOW   2
#define ORACLE_CONFIG_FUNC_RLE    3
#define ORACLE_CONFIG_FUNC_HUFF   4
#define ORACLE_CONFIG_FUNC_UNKNOWN (-1)
#define ORACLE_CONFIG_FUNC_RANGE   (-2)

/* --- configuration_table, deflate.c L112-L124 ---------------------------------------------- */

unsigned c_oracle_config_table_len(void) { return ORACLE_CONFIG_LEN; }

/* sizeof of one numeric field, so the width the port transcribes them at is checked, not assumed.
 * struct config_s declares all four as ush (L99-L102). */
unsigned c_oracle_config_field_size(void)
{
    return (unsigned)sizeof(configuration_table[0].good_length);
}

/* The four numeric fields of one row.  A level at or above the row count answers 0 rather than
 * reading past the end of the array; c_oracle_config_table_len is how a caller stays in range. */

unsigned c_oracle_config_good_length(unsigned level)
{
    if (level >= ORACLE_CONFIG_LEN) return 0u;
    return (unsigned)configuration_table[level].good_length;
}

unsigned c_oracle_config_max_lazy(unsigned level)
{
    if (level >= ORACLE_CONFIG_LEN) return 0u;
    return (unsigned)configuration_table[level].max_lazy;
}

unsigned c_oracle_config_nice_length(unsigned level)
{
    if (level >= ORACLE_CONFIG_LEN) return 0u;
    return (unsigned)configuration_table[level].nice_length;
}

unsigned c_oracle_config_max_chain(unsigned level)
{
    if (level >= ORACLE_CONFIG_LEN) return 0u;
    return (unsigned)configuration_table[level].max_chain;
}

/* Which compressor the row names, as one of the discriminators above.
 *
 * The comparison is on the function ADDRESS, which is the same test deflateParams makes at L791 --
 * `func != configuration_table[level].func` -- when it decides whether a level change must first
 * flush the open block.  Reporting the address's identity therefore reports exactly the property
 * that decides emitted bytes, rather than a re-derived guess at it.
 *
 * All five compressors are `local` to deflate.c and are named here for the same
 * same-translation-unit reason the table is: deflate_stored and deflate_fast at L73-L74,
 * deflate_slow at L76 behind #ifndef FASTEST, deflate_rle and deflate_huff at L78-L79.  The table
 * itself holds only the first three; deflate_rle and deflate_huff are reachable through the
 * strategy rather than the level, and are compared here so that the discriminator describes the
 * whole compress_func family and stays stable if a future row ever named one. */
int c_oracle_config_func(unsigned level)
{
    compress_func func;

    if (level >= ORACLE_CONFIG_LEN) return ORACLE_CONFIG_FUNC_RANGE;

    func = configuration_table[level].func;
    if (func == deflate_stored) return ORACLE_CONFIG_FUNC_STORED;
    if (func == deflate_fast) return ORACLE_CONFIG_FUNC_FAST;
#ifndef FASTEST
    if (func == deflate_slow) return ORACLE_CONFIG_FUNC_SLOW;
#endif
    if (func == deflate_rle) return ORACLE_CONFIG_FUNC_RLE;
    if (func == deflate_huff) return ORACLE_CONFIG_FUNC_HUFF;
    return ORACLE_CONFIG_FUNC_UNKNOWN;
}
"#;

/// The generated `trees.c` wrapper, written verbatim into `OUT_DIR`.
///
/// Like [`CONFIG_TU_SOURCE`] this is one deterministic constant with no timestamp, host path or
/// configuration-dependent content, so an unchanged build regenerates a byte-identical file, and
/// like it this is a *wrapper* rather than a copy: the `#include "trees.c"` line reaches the
/// authoritative file through the `-I<repo root>` search path [`reference_build`] already sets.
///
/// Why these four tables need it, when the other six `trees.h` tables do not: `-DGEN_TREES_H`
/// generates `trees.h` from `static_ltree`, `static_dtree`, `_dist_code`, `_length_code`,
/// `base_length` and `base_dist` only. `extra_lbits`, `extra_dbits`, `extra_blbits` and `bl_order`
/// are hand-written `local` declarations in `trees.c` and appear in no header at all, so there is
/// nothing for the table shim to include and no symbol for a linker to bind -- exactly
/// `configuration_table`'s situation, and exactly the same remedy.
///
/// Why they are worth comparing at all, given that they are short: all four decide emitted bytes.
/// `extra_lbits` and `extra_dbits` are added to a code width by `gen_bitlen` (`trees.c` L570-L574),
/// so a wrong entry changes `opt_len` and `static_len` and therefore which block type
/// `_tr_flush_block` picks -- the eighth of the byte-identity decision points; `compress_block`
/// then reads the same arrays to decide whether a residue follows a code (`trees.c` L926-L929), so
/// a wrong entry there truncates or over-writes the bitstream directly. `extra_blbits` carries the
/// three RFC 1951 3.2.7 repeat-count widths, and `bl_order` fixes the transmission order of the
/// nineteen bit-length codes; either one wrong makes every dynamic block's header unreadable.
///
/// The element-size accessors are the same discipline `c_oracle_config_field_size`,
/// `c_oracle_ct_data_size` and `c_oracle_code_size` apply: C declares the three `extra_*` arrays as
/// `int` and `bl_order` as `uch`, and reporting the C compiler's own `sizeof` is what lets the
/// port's `i32` and `u8` be checked rather than assumed.
const TREES_TU_SOURCE: &str = r#"/*
 * zlib_c_oracle_trees.c -- GENERATED FILE. DO NOT EDIT.
 *
 * Written into OUT_DIR by crates/zlib-rs-differential/build.rs; edit that script instead.
 *
 * This file IS the reference trees.c, plus accessors.  It includes the authoritative source
 * verbatim -- through the -I<repo root> search path, so the file in the tree is what the compiler
 * reads and nothing is copied -- and build.rs compiles this file INSTEAD of plain trees.c, so the
 * translation unit lands in the archive exactly once with the same flags and the same c_ renames
 * as every other reference source.
 *
 * Why an include and not a shim of its own: extra_lbits (L62), extra_dbits (L65), extra_blbits
 * (L68) and bl_order (L71) are all declared `local', zutil.h defines `local' as `static', and --
 * unlike static_ltree, base_length or _dist_code -- none of them is declared in trees.h, because
 * gen_trees_header() does not generate them.  There is nothing for a separate file to include and
 * no symbol for a linker to bind, so the only code that can read them is code compiled into
 * trees.c's own translation unit.  Transcribing the values into a shim instead would compare one
 * transcription against another: a mistake would appear identically on both sides and the
 * comparison would agree.
 *
 * Every array accessor is paired with a length accessor, and each element type with a sizeof
 * accessor: the Rust side must never hardcode a bound or an element width that the C sources own.
 */

#include "trees.c"

/* Element count of a statically sized array.  Spelled out here rather than shared with the table
 * shim or the deflate wrapper because the three are separate translation units. */
#define ORACLE_TREES_LEN(a) ((unsigned)(sizeof(a) / sizeof((a)[0])))

/* --- trees.c L62-L72: the four tables no header declares ----------------------------------- */

const int *c_oracle_extra_lbits(void) { return extra_lbits; }
unsigned c_oracle_extra_lbits_len(void) { return ORACLE_TREES_LEN(extra_lbits); }

const int *c_oracle_extra_dbits(void) { return extra_dbits; }
unsigned c_oracle_extra_dbits_len(void) { return ORACLE_TREES_LEN(extra_dbits); }

const int *c_oracle_extra_blbits(void) { return extra_blbits; }
unsigned c_oracle_extra_blbits_len(void) { return ORACLE_TREES_LEN(extra_blbits); }

const uch *c_oracle_bl_order(void) { return bl_order; }
unsigned c_oracle_bl_order_len(void) { return ORACLE_TREES_LEN(bl_order); }

/* --- element widths, so the port's transcription types are checked rather than assumed ------ */

/* All three extra_* arrays are `const int', reached by static_tree_desc_s through one
 * `const intf *extra_bits' member (trees.c L119), so one accessor covers the three. */
unsigned c_oracle_extra_bits_element_size(void)
{
    return (unsigned)sizeof(extra_lbits[0]);
}

unsigned c_oracle_bl_order_element_size(void) { return (unsigned)sizeof(bl_order[0]); }
"#;

/// `PATH` names probed, in order, when `OBJCOPY` is unset or unusable.
///
/// `gobjcopy` is the name GNU binutils takes on Homebrew, where the system `objcopy` may be
/// absent altogether.
const OBJCOPY_CANDIDATES: [&str; 3] = ["objcopy", "llvm-objcopy", "gobjcopy"];

/// Names probed inside the active toolchain's own sysroot when no `PATH` candidate works.
///
/// `llvm-objcopy` is what `rustup component add llvm-tools` installs; `rust-objcopy` is the
/// wrapper the toolchain ships unconditionally, which only works once that component is present.
/// Trying both, in that order, means the working one is found whichever way the host is set up.
const SYSROOT_OBJCOPY_CANDIDATES: [&str; 2] = ["llvm-objcopy", "rust-objcopy"];

/// Names asked of `xcrun`, which is the only tool locator a stock macOS install has.
const XCRUN_OBJCOPY_CANDIDATES: [&str; 2] = ["llvm-objcopy", "objcopy"];

/// Programs probed, in order, when `NM` is unset.
const NM_CANDIDATES: [&str; 3] = ["nm", "llvm-nm", "gnm"];

/// Argument spellings tried, in order, for the symbol listing -- see `nm_listing`.
///
/// All three produce POSIX format, whose first field is the symbol name and whose second is the
/// type letter, which parses identically for objects and archives. They differ only in how much
/// filtering they ask the tool to do, and `verify_every_export_is_prefixed` applies that filter
/// itself in every case, so the three are interchangeable rather than degrees of coverage:
///
///   1. GNU and LLVM long options, which is what a Linux or `llvm-tools` host has.
///   2. The short forms of the same three requests, which BSD-derived tools accept.
///   3. POSIX format alone. Every `nm` worth the name accepts `-P`, and the defined-and-global
///      filter is this script's own work regardless.
const NM_ARGUMENT_LADDER: [&[&str]; 3] = [
    &["--defined-only", "--extern-only", "--format=posix"],
    &["-U", "-g", "-P"],
    &["-P"],
];

/// The dynamic-library search paths removed from this process before any tool is spawned.
///
/// See `sanitize_library_search_path` for the measurement that makes this necessary. The ELF and
/// Mach-O spellings are both listed so the behaviour does not depend on the host.
const CHILD_LIBRARY_PATH_VARS: [&str; 2] = ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"];

fn main() {
    // First, before anything is spawned. See the function's own comment: this has to precede the
    // `cc` invocation as well as the binutils ones.
    sanitize_library_search_path();

    let manifest_dir = required_env_path("CARGO_MANIFEST_DIR");
    let out_dir = required_env_path("OUT_DIR");
    let repo_root = locate_repo_root(&manifest_dir);

    emit_rerun_directives(&repo_root);

    // BEFORE a single object exists.  An `-flto` in the environment makes the symbol rename
    // below unsound, and the whole crate depends on that rename, so the answer is refused
    // here -- where the diagnostic can name the variable -- rather than surfacing later as an
    // `objcopy` exit status, or (worse) as a suite that compares the port against itself.
    refuse_link_time_optimisation(&repo_root);

    // Everything generated goes to OUT_DIR. The repository working tree is never written to.
    let shim = write_table_shim(&out_dir);
    let wrappers = write_wrapper_tus(&out_dir);
    let objects = compile_reference_sources(&repo_root, &wrappers, &shim);

    // Rename before archiving: the archive must never exist in a collidable state, not even
    // transiently, or a concurrent link could pick it up.
    let objcopy = locate_objcopy();
    let redefine_map = write_redefine_map(&out_dir);
    apply_symbol_renames(&objcopy, &redefine_map, &objects);

    let archive = archive_objects(&out_dir, &objects);
    verify_every_export_is_prefixed(&archive);

    emit_link_directives(&out_dir);

    // Last, and only once the oracle it describes exists: the compilation identity of both sides,
    // so that every ratio this crate's benches print says which two builds produced it.
    emit_baseline_metadata(&repo_root);
}

/// Removes the dynamic-library search path from this process, so that every tool this script
/// spawns resolves `libz` the way the system intends rather than out of the build directory.
///
/// # The problem this solves, measured rather than supposed
///
/// Cargo puts `target/<profile>` and `target/<profile>/deps` on `LD_LIBRARY_PATH` for every build
/// script it launches, and the host binutils are themselves linked against zlib: `ld.so --list`
/// on `nm`, `objcopy`, `ar`, `ld`, `as` and `ld.bfd` shows `DT_NEEDED libz.so.1` with no `RPATH`
/// of their own. `target/<profile>` is where cargo writes this workspace's own `libz.so`. So any
/// file there that satisfies a `libz.so.1` lookup makes the tools that INSPECT the artifact load
/// the artifact.
///
/// It used to be reachable and it was measured. `crates/libz-rs-sys/build.rs` staged `libz.so.1`
/// and `libz.so.<ZLIB_VERSION>` beside cargo's `libz.so`, and one build of this crate then emitted
/// 32 `no version information available` notices, because a rustc-linked cdylib carries no
/// symbol-version nodes (rustc supplies its own anonymous version script, so `zlib.map` cannot be
/// layered on) while `nm` asks for versioned symbols. glibc warns and binds anyway, so nothing
/// failed -- it just meant the verification tools were running code out of the library under
/// verification, and it broke the zero-warnings bar. Worse shapes existed: with a zero-export or
/// truncated library at that name, `as` and `rustc` could not start at all.
///
/// # Why it stays now that nothing stages those names
///
/// That staging is gone -- job 4 of `crates/libz-rs-sys/build.rs` prunes the names instead, for
/// exactly the reasons above -- so on a clean tree there is currently nothing in
/// `target/<profile>` for a `libz.so.1` lookup to find. This call is the property rather than the
/// symptom, and the property is what has to hold: a script that verifies one zlib against another
/// must not let either one under the tools doing the verifying. It costs two lines and it holds
/// whatever a caller, a packaging step or a future revision leaves in that directory.
///
/// It is also safe unconditionally. This script loads no dylib from the build directory -- its own
/// process is already loaded, and everything it does afterwards is spawning `cc`, `objcopy`, `ar`
/// and `nm`, all of them system tools with their own resolution rules.
///
/// Removing the variables from THIS process is what covers all four, including the `cc` crate's
/// compiler invocation, which offers no hook for a child environment: children inherit the
/// environment as modified. `DYLD_LIBRARY_PATH` is included for the Mach-O equivalent; macOS
/// strips it from system binaries under SIP, so removing it is belt and braces rather than the
/// load-bearing half.
///
/// Any future build script that shells out to a zlib-linked tool needs these same two lines.
fn sanitize_library_search_path() {
    for name in CHILD_LIBRARY_PATH_VARS {
        env::remove_var(name);
    }
}

/// Aborts the build with `message`.
///
/// Every failure path in this script routes through here, which is what makes the single
/// `#[allow]` below sufficient and auditable.
///
/// `clippy::panic` is denied across the workspace, and rightly so for library code: a compression
/// library is linked into processes that must not die on malformed input, so it returns
/// `ReturnCode` instead. A build script is the opposite case. If the oracle cannot be built,
/// renamed and archived exactly as specified, then a differential test that "passes" has compared
/// nothing at all -- the worst outcome this crate can produce. Aborting the build is therefore the
/// correct behaviour, and swallowing an error to keep the build green would be the defect.
#[allow(clippy::panic)]
fn fail(message: &str) -> ! {
    panic!("{message}");
}

/// Reads an environment variable cargo is contractually required to set.
///
/// Uses the `OsString` form rather than the `String` form so a non-UTF-8 build directory is not a
/// spurious failure.
fn required_env_path(name: &str) -> PathBuf {
    let Some(value) = env::var_os(name) else {
        fail(&format!(
            "build.rs: the environment variable `{name}` is not set. This file is a cargo build \
             script and cargo is what supplies it; running it directly will not work."
        ))
    };
    PathBuf::from(value)
}

/// Derives the repository root from `CARGO_MANIFEST_DIR` and then proves the guess.
///
/// This crate lives at `<repo root>/crates/zlib-rs-differential`, so the root is two levels up.
/// The path is never hardcoded and the process working directory is never consulted, because
/// cargo does not promise what it will be.
///
/// The existence check is not ceremony. A silently wrong root would make `cc` fail with fifteen
/// "no such file" errors that say nothing about the cause, whereas this reports the resolved path
/// and the missing files.
fn locate_repo_root(manifest_dir: &Path) -> PathBuf {
    let Some(root) = manifest_dir.parent().and_then(Path::parent) else {
        fail(&format!(
            "build.rs: CARGO_MANIFEST_DIR is `{}`, which does not have the two parent \
             directories that this crate's location, <repo root>/crates/zlib-rs-differential, \
             implies. The repository root cannot be derived from it.",
            manifest_dir.display()
        ))
    };

    let mut missing: Vec<&str> = Vec::new();
    for name in C_SOURCES.into_iter().chain(C_HEADERS) {
        if !root.join(name).is_file() {
            missing.push(name);
        }
    }

    if !missing.is_empty() {
        fail(&format!(
            "build.rs: resolved the repository root to `{}` (two levels above \
             CARGO_MANIFEST_DIR `{}`), but {} of the required reference file(s) are not there: \
             {}. The differential oracle is compiled from the in-tree C sources, so this crate \
             must stay at <repo root>/crates/zlib-rs-differential.",
            root.display(),
            manifest_dir.display(),
            missing.len(),
            join_names(&missing)
        ));
    }

    root.to_path_buf()
}

/// Declares every input whose modification must rebuild the oracle.
///
/// Emitting any `rerun-if-changed` at all switches cargo from "rerun when anything in the package
/// changed" to "rerun when one of these changed", so the list has to be complete: all fifteen
/// translation units, all eleven headers they and the shim include, and this script itself.
fn emit_rerun_directives(repo_root: &Path) {
    println!("cargo::rerun-if-changed=build.rs");
    for name in C_SOURCES.into_iter().chain(C_HEADERS) {
        println!("cargo::rerun-if-changed={}", repo_root.join(name).display());
    }
}

/// Writes the generated table shim into `OUT_DIR` and returns its path.
fn write_table_shim(out_dir: &Path) -> PathBuf {
    let path = out_dir.join(SHIM_FILE_NAME);
    if let Err(error) = fs::write(&path, TABLE_SHIM_SOURCE) {
        fail(&format!(
            "build.rs: could not write the generated table shim to `{}`: {error}",
            path.display()
        ));
    }
    path
}

/// Writes every generated wrapper translation unit into `OUT_DIR` and returns their paths.
///
/// One entry per [`WRAPPER_TUS`] row, in that order, so the caller can hand the whole set to
/// [`compile_reference_sources`] without knowing how many there are.
///
/// Each `#include` line is checked rather than trusted. A wrapper's source names the reference file
/// in its own text while its `origin` is what [`compile_reference_sources`] excludes from the
/// direct source list, and the two have to agree: if they ever drifted apart, that reference file
/// would be compiled twice -- once directly and once through the include -- and the archive would
/// carry two definitions of every symbol in it. The check turns that into a build failure here
/// instead of a duplicate-symbol error much later.
///
/// The wrapper file names are checked against each other too, because two wrappers writing the same
/// name would silently leave one of the two reference translation units out of the archive.
fn write_wrapper_tus(out_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::with_capacity(WRAPPER_TUS.len());

    for (origin, file_name, source) in WRAPPER_TUS {
        let include = format!("#include \"{origin}\"");
        if !source.contains(&include) {
            fail(&format!(
                "build.rs: the generated wrapper `{file_name}` does not contain `{include}`, so it \
                 would not compile the reference translation unit that WRAPPER_TUS excludes from \
                 the direct source list. Keep the origin and the wrapper source in step."
            ));
        }

        let path = out_dir.join(file_name);
        if paths.contains(&path) {
            fail(&format!(
                "build.rs: two WRAPPER_TUS entries both write `{file_name}`, so one reference \
                 translation unit would be overwritten by the other and would be missing from the \
                 oracle archive entirely. Give each wrapper its own file name."
            ));
        }

        if let Err(error) = fs::write(&path, source) {
            fail(&format!(
                "build.rs: could not write the generated `{origin}` wrapper to `{}`: {error}",
                path.display()
            ));
        }
        paths.push(path);
    }

    paths
}

/// The flags shared by the oracle and the shim, so the two can never drift apart.
///
/// `opt_level` and `pic` are set explicitly rather than inherited from `OPT_LEVEL`, because the
/// reference build is `-O3 -fPIC` regardless of which cargo profile happens to be driving the
/// build. `debug` is deliberately left at `cc`'s default, which follows the profile: debug info is
/// output-neutral, and having it in a dev build is what makes an AddressSanitizer backtrace
/// through the oracle readable.
///
/// `warnings` is likewise left at `cc`'s default, which adds `-Wall -Wextra`. The reference
/// sources compile clean under both, so the extra scrutiny costs nothing and would report a real
/// regression in the C tree.
fn reference_build(repo_root: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    build
        .include(repo_root)
        .define("_LARGEFILE64_SOURCE", "1")
        .define("HAVE_HIDDEN", None)
        .opt_level(3)
        .pic(true);
    build
}

/// Publishes the oracle's compilation identity, and the Rust side's, for the benches to print.
///
/// # Why a benchmark needs this
///
/// The AAP's throughput gates (§0.8.4: decompression and levels 1/6/9 within 10%; §0.6.4.6) are
/// ratios of the port against this oracle. A ratio is a fact about two *implementations* only when
/// both were built comparably, and here they are not: [`reference_build`] compiles the reference
/// `-O3 -fPIC` one translation unit at a time with no cross-unit optimisation, while the port is
/// built by cargo's `bench` profile, which inherits the shipped release profile's `lto = "fat"` and
/// `codegen-units = 1`. Whole-program optimisation across the port's crates has no counterpart on
/// the C side, so the asymmetry systematically flatters the port -- and a number that flatters one
/// side is exactly the number a reviewer must not have to take on trust.
///
/// So every measurement log states both identities, from the source that cannot drift: the flag
/// string comes from the [`cc::Tool`] `cc` is about to invoke, not from a comment, because `cc`
/// also absorbs `CFLAGS`/`TARGET_CFLAGS`/`HOST_CFLAGS` from the environment and a hand-maintained
/// copy would be wrong the first time someone set one.
///
/// # Why the C side is not simply built with LTO instead
///
/// It cannot be, and the reason is mechanical rather than a preference. Every externally visible
/// symbol in this archive is renamed to carry a `c_` prefix by `objcopy --redefine-syms`
/// ([`apply_symbol_renames`]), which is what lets the port and the reference be linked into one
/// binary and called alternately. `objcopy` rewrites the ELF symbol table; under `-flto` the
/// object files carry GCC's serialised IR in `.gnu.lto_*` sections instead, and the names inside
/// that IR are invisible to `--redefine-syms`. An LTO oracle would therefore emerge from the
/// rename pass with its *real* names still in the IR and rebind, at link time, to the port's
/// unprefixed definitions -- a differential test comparing the port against itself and passing
/// trivially. Losing the oracle to gain a comparable flag set is not a trade worth making, so an
/// `-flto` that reaches this build through the environment is a HARD FAILURE here rather than a
/// note in the tag: by the time a log could be read, the suites would already have passed.
///
/// The comparability is therefore restored from the Rust side, where it costs nothing: the root
/// `Cargo.toml` defines a second measurement profile, `bench-parity`, identical to `bench` except
/// that LTO is off and codegen units are not collapsed, so a second pass measures the port
/// *without* the whole-program optimisation the oracle never had. Which profile produced a binary
/// is declared through [`RUST_PROFILE_DECLARATION_VAR`] and baked in here, and the workflow decides
/// a gate on the stronger -- that is, the worse-for-the-port -- of the two passes' ratios.
///
/// # What is cross-checked
///
/// A declaration of `bench` or `bench-parity` asserts a release-like `-O3` build. Cargo tells a
/// build script both of those things (`PROFILE`, `OPT_LEVEL`), so a declaration that contradicts
/// them -- most usefully, a dev build labelled as a measurement profile -- fails the build here
/// rather than producing a plausible-looking log.
/// Refuses to build the oracle when the C flags carry link-time optimisation.
///
/// # Why this is a hard failure and not a recorded asymmetry
///
/// Every externally visible symbol in the oracle archive is renamed to carry a `c_` prefix by
/// `objcopy --redefine-syms` ([`apply_symbol_renames`]); that rename is what lets the reference
/// implementation and the port be linked into one test binary and called alternately. `objcopy`
/// rewrites the ELF symbol table. Under `-flto` the object files carry GCC's serialised IR in
/// `.gnu.lto_*` sections instead, and the names inside that IR are invisible to
/// `--redefine-syms` -- so the oracle would emerge from the rename pass with its *real* names
/// still in the IR and rebind, at link time, to the port's own unprefixed definitions. Every
/// differential comparison in this crate would then compare the port against itself and pass
/// trivially.
///
/// A vacuous pass is the worst outcome this crate can produce, and worse than a failed build:
/// an earlier revision detected the flag, folded the answer into the profile tag and continued,
/// on the reasoning that a log line saying `lto` was enough for a reader to notice. It is not --
/// by the time the log exists the suites have already reported success. `-flto` can only arrive
/// here through an external `CFLAGS`, `TARGET_CFLAGS`, `HOST_CFLAGS` or the per-target spelling,
/// all of which `cc` absorbs, so the flag set `cc` is about to use is the authority for the
/// question and this runs before any object exists.
///
/// Comparable measurement of the port is not lost by refusing: the root `Cargo.toml`'s
/// `bench-parity` profile turns LTO off on the RUST side, which is the direction that does not
/// break the rename pass.
fn refuse_link_time_optimisation(repo_root: &Path) {
    let tool = reference_build(repo_root).get_compiler();
    let offending: Vec<String> = tool
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|flag| flag.starts_with("-flto") || flag == "/GL")
        .collect();

    if offending.is_empty() {
        return;
    }

    fail(&format!(
        "build.rs: the C oracle cannot be built with link-time optimisation, and {offending:?} \
         is in the flag set `{cc}` is about to use.\n\
         \n\
         This crate renames every exported symbol in the oracle to a `c_` prefix with `objcopy \
         --redefine-syms`, which is what lets the reference and the port live in one test binary. \
         Under LTO the names live inside `.gnu.lto_*` IR sections that `--redefine-syms` does not \
         touch: the oracle would keep its real names, rebind to the port's definitions at link \
         time, and every differential comparison in this crate would silently compare the port \
         against itself.\n\
         \n\
         Remove the flag from CFLAGS, TARGET_CFLAGS, HOST_CFLAGS or CFLAGS_<target> for this \
         build. Whole-program optimisation is compared instead from the Rust side, with the \
         `bench-parity` profile, which turns LTO off there.",
        cc = tool.path().display(),
    ));
}

fn emit_baseline_metadata(repo_root: &Path) {
    // The tool `cc` will invoke, built from the same function the oracle itself uses. The 95
    // `-D` renames are deliberately not part of this build: they are name substitutions with no
    // bearing on code generation, they would swamp the line, and they are reported as a count.
    let tool = reference_build(repo_root).get_compiler();
    let flags: Vec<String> = tool
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let family = if tool.is_like_msvc() {
        "msvc"
    } else if tool.is_like_clang() {
        "clang"
    } else if tool.is_like_gnu() {
        "gnu"
    } else {
        "unknown"
    };
    // `no-lto` is unconditional because the assertion above is what makes it true, rather
    // than a hope: an LTO build never reaches this line.
    let tag = format!("{family}-O3-fPIC-per-tu-no-lto");

    println!(
        "cargo::rustc-env={C_FLAGS_ENV}=cc={cc} renames={renames} units=per-translation-unit \
         flags={flags}",
        cc = tool.path().display(),
        renames = PUBLIC_RENAMES.len(),
        flags = flags.join(" "),
    );
    println!("cargo::rustc-env={C_PROFILE_TAG_ENV}={tag}");

    // The Rust side. `PROFILE` and `OPT_LEVEL` are cargo's own report; the profile *name* is the
    // invoker's declaration, because cargo does not expose it.
    println!("cargo::rerun-if-env-changed={RUST_PROFILE_DECLARATION_VAR}");
    let declared = env::var(RUST_PROFILE_DECLARATION_VAR).unwrap_or_default();
    let profile = if declared.is_empty() {
        UNDECLARED_PROFILE.to_owned()
    } else {
        declared
    };
    let kind = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned());
    let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".to_owned());

    if MEASUREMENT_PROFILES.contains(&profile.as_str()) && (kind != "release" || opt_level != "3") {
        fail(&format!(
            "build.rs: {RUST_PROFILE_DECLARATION_VAR} declares the cargo profile `{profile}`, \
             which is a measurement profile and therefore release-like and -O3, but cargo reports \
             PROFILE=`{kind}` and OPT_LEVEL=`{opt_level}`. A throughput ratio carrying that label \
             would be attributed to a build it did not come from, so the build stops here. Run \
             `cargo bench --profile {profile}`, or leave the variable unset for an unlabelled \
             exploratory build."
        ));
    }

    println!("cargo::rustc-env={RUST_PROFILE_ENV}={profile}");
    println!(
        "cargo::rustc-env={RUST_BUILD_ENV}=cargo_profile={kind} opt_level={opt_level} \
         target={target} rustflags={rustflags}",
        target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned()),
        rustflags = encoded_rustflags(),
    );
}

/// The `RUSTFLAGS` in force, as a single space-separated string, or `none`.
///
/// Cargo hands a build script `CARGO_ENCODED_RUSTFLAGS` with the flags separated by `0x1f` unit
/// separators so that a flag may itself contain spaces. Rendering them space-separated is lossy in
/// that one respect and exactly right for a log line, which is what this is for. It matters to a
/// throughput ratio because the AAP's admissible optimisation levers live here -- `-C target-cpu`
/// and `-C llvm-args=-enable-dfa-jump-thread` (AAP §0.3.2.4) -- and a ratio measured with one of
/// them is not comparable with a ratio measured without it.
fn encoded_rustflags() -> String {
    match env::var("CARGO_ENCODED_RUSTFLAGS") {
        Ok(value) if !value.is_empty() => value.replace('\u{1f}', " "),
        Ok(_) | Err(_) => "none".to_owned(),
    }
}

/// Compiles the fifteen reference translation units and the generated table shim into `OUT_DIR`.
///
/// Thirteen of the fifteen are compiled from the repository root directly. The two named by
/// [`WRAPPER_TUS`] -- `deflate.c`, which carries `configuration_table`, and `trees.c`, which
/// carries the four tables no header declares -- are compiled through their generated wrappers
/// instead, and those wrappers join the *same* `cc::Build` as the other thirteen so that they are
/// built with the identical flags and the identical 95 `-D` renames. Compiling either in the shim's
/// build instead would leave every public name in it un-prefixed and the archive would collide with
/// `libz-rs-sys`.
///
/// `compile_intermediates` is used instead of `compile` on purpose: it stops after the objects and
/// emits no link metadata, which leaves room for the `objcopy` pass before anything is archived.
///
/// The `cc::Build`s cannot collide in `OUT_DIR` even though the shim and `crc32.c` would both
/// like to be `crc32.o`-style names, because `cc` derives each object name from a hash of its
/// source's *directory* and the generated files live in `OUT_DIR` rather than the repository root.
/// The two wrappers cannot collide with each other either: [`write_wrapper_tus`] proves their file
/// names differ, and `cc` derives the object name from the source stem.
fn compile_reference_sources(repo_root: &Path, wrappers: &[PathBuf], shim: &Path) -> Vec<PathBuf> {
    let mut oracle = reference_build(repo_root);
    let mut substituted = 0_usize;
    for name in C_SOURCES {
        if WRAPPER_TUS.iter().any(|(origin, _, _)| *origin == name) {
            // Compiled through its generated wrapper, which includes it verbatim. Adding both
            // would define every symbol in it twice.
            substituted += 1;
            continue;
        }
        oracle.file(repo_root.join(name));
    }
    for wrapper in wrappers {
        oracle.file(wrapper);
    }

    if substituted != WRAPPER_TUS.len() || wrappers.len() != WRAPPER_TUS.len() {
        fail(&format!(
            "build.rs: WRAPPER_TUS names {} reference translation unit(s), but {substituted} of the \
             {} entries in C_SOURCES matched and {} wrapper(s) were written. Each wrapper compiles \
             exactly one C_SOURCES entry, so a mismatch means one is either compiled twice or not \
             at all.",
            WRAPPER_TUS.len(),
            C_SOURCES.len(),
            wrappers.len()
        ));
    }

    for name in PUBLIC_RENAMES {
        oracle.define(name, prefixed(name).as_str());
    }
    let mut objects = oracle.compile_intermediates();

    let mut shim_build = reference_build(repo_root);
    shim_build.file(shim);
    for (original, replacement) in SHIM_LOCAL_RENAMES {
        shim_build.define(original, replacement);
    }
    objects.extend(shim_build.compile_intermediates());

    // One object per reference translation unit -- whether compiled directly or through its
    // wrapper, since each wrapper stands in for exactly one entry -- plus one for the table shim.
    let expected = C_SOURCES.len() + 1;
    if objects.len() != expected {
        fail(&format!(
            "build.rs: expected {expected} object files ({} reference translation units compiled \
             directly, the {} generated wrapper(s) that carry the rest, and the generated table \
             shim) but the C compiler produced {}. The oracle would be incomplete, so the build \
             cannot continue.",
            C_SOURCES.len() - WRAPPER_TUS.len(),
            WRAPPER_TUS.len(),
            objects.len()
        ));
    }

    objects
}

/// Returns `name` with the archive's mandatory symbol prefix applied.
fn prefixed(name: &str) -> String {
    let mut renamed = String::with_capacity(SYMBOL_PREFIX.len() + name.len());
    renamed.push_str(SYMBOL_PREFIX);
    renamed.push_str(name);
    renamed
}

/// Finds an `objcopy`, honouring `$OBJCOPY` first.
///
/// A missing `objcopy` is fatal rather than something to work around. Without the object-level
/// rename the archive would define `gzgetc`, `inflate_table` and the whole `ZLIB_INTERNAL` set
/// under their real names, and any test binary that also links `libz-rs-sys` would fail with
/// duplicate symbols -- or, worse, silently bind the wrong definition.
fn locate_objcopy() -> OsString {
    println!("cargo::rerun-if-env-changed=OBJCOPY");

    if let Some(explicit) = env::var_os("OBJCOPY").filter(|value| !value.is_empty()) {
        if !can_run(&explicit) {
            fail(&format!(
                "build.rs: OBJCOPY is set to `{}`, but that program could not be executed. \
                 Point it at a working GNU or LLVM objcopy, or unset it to let this script probe \
                 for one.",
                Path::new(&explicit).display()
            ));
        }
        return explicit;
    }

    for candidate in OBJCOPY_CANDIDATES {
        let program = OsString::from(candidate);
        if can_run(&program) {
            return program;
        }
    }

    // Nothing on PATH. Fall back to the toolchain's own sysroot, which is the one place every
    // Tier 1 host can be made to have an objcopy without installing a system package.
    if let Some(program) = locate_sysroot_objcopy() {
        return program;
    }

    // Stock macOS has no PATH objcopy and no sysroot one unless llvm-tools was added, but it does
    // have xcrun, which resolves tools out of the active Xcode or Command Line Tools.
    if let Some(program) = locate_xcrun_objcopy() {
        return program;
    }

    fail(&format!(
        "build.rs: no usable objcopy was found. Tried $OBJCOPY, then {} on PATH, then {} in the \
         toolchain sysroot, then xcrun. One of them is required: it is the only way to rename \
         gzgetc -- gzread.c undefines the macro, so no -D can reach it -- and the ZLIB_INTERNAL \
         symbols that zlib.map hides, notably inflate_table, which libz.a deliberately keeps for \
         test/infcover.c's cover_trees(). The remedy that works on every supported host is \
         `rustup component add llvm-tools`, which installs llvm-objcopy beside the toolchain; \
         GNU binutils (or Homebrew binutils, as gobjcopy) works too where it is available.",
        OBJCOPY_CANDIDATES.join(", "),
        SYSROOT_OBJCOPY_CANDIDATES.join(", ")
    ))
}

/// Looks for an objcopy in the active toolchain's sysroot, at
/// `<sysroot>/lib/rustlib/<host>/bin/`.
///
/// This is where `rustup component add llvm-tools` puts `llvm-objcopy`, and it is the only
/// location that is available in the same way on Linux, macOS and Windows alike -- which is what
/// makes it the answer to "this crate will not build on a stock macOS or MSVC host".
///
/// `RUSTC` and `HOST` are both supplied by cargo to every build script, so the sysroot is queried
/// from the compiler that is actually driving this build rather than from whatever `rustc` happens
/// to be first on `PATH`. A toolchain that cannot answer `--print sysroot` is simply skipped: this
/// is a fallback, and the caller has a diagnostic of its own.
fn locate_sysroot_objcopy() -> Option<OsString> {
    let rustc = env::var_os("RUSTC")?;
    let host = env::var_os("HOST")?;

    let output = Command::new(&rustc)
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let bin = Path::new(sysroot.trim())
        .join("lib")
        .join("rustlib")
        .join(host)
        .join("bin");

    SYSROOT_OBJCOPY_CANDIDATES
        .into_iter()
        .find_map(|candidate| {
            let program = OsString::from(bin.join(candidate));
            can_run(&program).then_some(program)
        })
}

/// Asks `xcrun --find` for an objcopy, which is how tools are located on macOS.
///
/// Returns `None` everywhere else, and on macOS whenever `xcrun` cannot answer -- either because
/// no developer directory is selected or because the active one ships no such tool. Both are
/// ordinary outcomes for a fallback rather than errors to report here.
fn locate_xcrun_objcopy() -> Option<OsString> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return None;
    }

    XCRUN_OBJCOPY_CANDIDATES.into_iter().find_map(|candidate| {
        let output = Command::new("xcrun")
            .arg("--find")
            .arg(candidate)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?;
        let program = OsString::from(path.trim());
        (!program.is_empty() && can_run(&program)).then_some(program)
    })
}

/// Lists the archive's symbols, probing by RUNNING THE REAL INVOCATION rather than by asking a
/// candidate for its version.
///
/// # Why the probe is the real command
///
/// The previous form of this code accepted a candidate only if `<nm> --version` exited zero, and
/// that test answers the wrong question twice over. Apple's cctools `nm` does not implement
/// `--version` -- it prints usage and exits nonzero -- so a perfectly usable tool was rejected and
/// the audit was skipped on exactly the host where the fallback chain is thinnest. In the other
/// direction, a program that answers `--version` says nothing about whether it accepts
/// `--defined-only --extern-only --format=posix`, so a candidate could be accepted and then fail at
/// the point of use.
///
/// So each candidate is tried with the invocation it will actually be used for, and the ARGUMENTS
/// degrade rather than the check: [`NM_ARGUMENT_LADDER`] moves from the GNU/LLVM long options to the
/// portable short ones to a bare `-P`, whose output this script filters itself. The first spelling
/// that succeeds wins. Nothing is inferred; if none of them works, the caller has the complete list
/// of what was tried.
///
/// # Honouring `$NM`
///
/// When `NM` is set it is the ONLY candidate. Falling back to a different tool after an explicit
/// override fails would mean auditing with something the operator did not choose, and reporting
/// success for it; the caller turns that case into a build failure naming the variable.
fn nm_listing(archive: &Path) -> Result<(OsString, String), String> {
    let explicit = env::var_os("NM").filter(|value| !value.is_empty());
    let candidates: Vec<OsString> = match explicit {
        Some(ref program) => vec![program.clone()],
        None => NM_CANDIDATES.iter().map(OsString::from).collect(),
    };

    let mut attempts: Vec<String> = Vec::new();
    for program in candidates {
        for arguments in NM_ARGUMENT_LADDER {
            match Command::new(&program).args(arguments).arg(archive).output() {
                Ok(output) if output.status.success() => {
                    let listing = String::from_utf8_lossy(&output.stdout).into_owned();
                    // A zero exit with nothing to show is not an answer. Measured: `NM=/bin/true`
                    // satisfied a status-only test and left the audit inspecting an empty listing,
                    // which passes every check by having nothing to check.
                    if listing_has_symbols(&listing) {
                        return Ok((program, listing));
                    }
                    attempts.push(format!(
                        "`{} {}` exited 0 but listed no symbols",
                        Path::new(&program).display(),
                        arguments.join(" ")
                    ));
                }
                Ok(output) => attempts.push(format!(
                    "`{} {}` exited with {}: {}",
                    Path::new(&program).display(),
                    arguments.join(" "),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => {
                    // The program itself cannot be run, so the remaining spellings cannot help.
                    attempts.push(format!(
                        "`{}` could not be executed: {error}",
                        Path::new(&program).display()
                    ));
                    break;
                }
            }
        }
    }

    let attempts = attempts.join("\n  ");
    Err(match explicit {
        Some(program) => format!(
            "NM is set to `{}` and no spelling of the symbol listing worked with it:\n  {attempts}",
            Path::new(&program).display()
        ),
        None => format!(
            "none of {} could list the symbols of `{}`:\n  {attempts}",
            NM_CANDIDATES.join(", "),
            archive.display()
        ),
    })
}

/// Reports whether a symbol listing contains at least one symbol line.
///
/// POSIX format gives every symbol a name and a type letter, so two whitespace-separated fields are
/// the minimum; archive member headers (`<archive>[<member>]:`) carry one. An archive built from the
/// reference sources always has thousands of symbols, so an empty result means the tool did not
/// understand what it was asked -- whatever it reported as its exit status.
fn listing_has_symbols(listing: &str) -> bool {
    listing
        .lines()
        .any(|line| line.split_whitespace().nth(1).is_some())
}

/// Reports whether `program` can be executed at all.
///
/// The exit STATUS is deliberately ignored: what a probe can establish is that the name resolves to
/// something executable, and several real tools answer an unrecognised `--version` with usage text
/// and a nonzero status (Apple's cctools tools among them). Whether the program accepts the
/// arguments it will be given is settled where those arguments are used -- `apply_symbol_renames`
/// checks `objcopy`'s exit status and aborts the build on failure -- rather than guessed here.
fn can_run(program: &OsStr) -> bool {
    Command::new(program).arg("--version").output().is_ok()
}

/// Writes the `objcopy --redefine-syms` map into `OUT_DIR` and returns its path.
///
/// One `old new` pair per line, which is the format `--redefine-syms` expects.
fn write_redefine_map(out_dir: &Path) -> PathBuf {
    let mut contents = String::new();
    for name in INTERNAL_REDEFINES {
        contents.push_str(name);
        contents.push(' ');
        contents.push_str(SYMBOL_PREFIX);
        contents.push_str(name);
        contents.push('\n');
    }

    let path = out_dir.join(REDEFINE_MAP_FILE_NAME);
    if let Err(error) = fs::write(&path, contents) {
        fail(&format!(
            "build.rs: could not write the objcopy symbol map to `{}`: {error}",
            path.display()
        ));
    }
    path
}

/// Rewrites the residual symbol names in every object, in place.
///
/// `--redefine-syms` rewrites undefined references as well as definitions, so the objects stay
/// linked to each other: after this pass `deflate.o` calls `c__tr_init`, not `_tr_init`. The pass
/// is idempotent, because a second run finds no un-prefixed name left to match, and a name absent
/// from a given object is simply ignored -- which is why one map can be applied to all sixteen.
fn apply_symbol_renames(objcopy: &OsStr, redefine_map: &Path, objects: &[PathBuf]) {
    let mut flag = OsString::from("--redefine-syms=");
    flag.push(redefine_map);

    for object in objects {
        match Command::new(objcopy).arg(&flag).arg(object).status() {
            Ok(status) if status.success() => {}
            Ok(status) => fail(&format!(
                "build.rs: `{} --redefine-syms={} {}` exited with {status}. Without the rename \
                 the oracle archive collides with libz-rs-sys, so the build cannot continue.",
                Path::new(objcopy).display(),
                redefine_map.display(),
                object.display()
            )),
            Err(error) => fail(&format!(
                "build.rs: could not execute `{}` to rename the symbols in `{}`: {error}",
                Path::new(objcopy).display(),
                object.display()
            )),
        }
    }
}

/// Bundles the renamed objects into the static library `OUT_DIR` link search will find.
///
/// The name and the command line are both chosen from the target environment, because the two
/// supported archivers agree on neither. Under `msvc` the archiver is `lib.exe`, which takes
/// `/OUT:<archive>` and rejects operation letters; everywhere else it is an `ar`, which takes
/// `crs <archive>` and has no `/OUT:`. The names differ for the same reason:
/// `cargo::rustc-link-lib=static=<stem>` makes rustc look for `<stem>.lib` under `msvc` and
/// `lib<stem>.a` otherwise, and only those.
///
/// The archive is removed first. `ar` in replace mode updates members it recognises but leaves
/// unknown ones behind, so a stale member from an earlier source list would otherwise survive and
/// quietly contribute a second definition.
fn archive_objects(out_dir: &Path, objects: &[PathBuf]) -> PathBuf {
    let mut archiver = match cc::Build::new().try_get_archiver() {
        Ok(command) => command,
        Err(error) => fail(&format!(
            "build.rs: could not determine which archiver to bundle the oracle objects with: \
             {error}"
        )),
    };

    let msvc_style = uses_msvc_archiver(archiver.get_program());
    let archive = if msvc_style {
        out_dir.join(format!("{ARCHIVE_STEM}.lib"))
    } else {
        out_dir.join(format!("lib{ARCHIVE_STEM}.a"))
    };
    remove_if_present(&archive);

    // Mirrors what `cc` does for its own archives: the Apple archiver zeroes member timestamps
    // when this is set, so a rebuild from unchanged inputs produces an identical archive.
    archiver.env("ZERO_AR_DATE", "1");
    if msvc_style {
        // `lib.exe` has no operation letters. `/NOLOGO` keeps the banner out of the build log,
        // where cargo would surface it as build-script output.
        let mut out_flag = OsString::from("/OUT:");
        out_flag.push(&archive);
        archiver.arg("/NOLOGO").arg(out_flag).args(objects);
    } else {
        // `c` create, `r` replace, `s` write the symbol index. GNU ar, LLVM ar and BSD ar all
        // accept this spelling, and the index is what lets the linker resolve members without a
        // ranlib pass.
        archiver.arg("crs").arg(&archive).args(objects);
    }

    match archiver.status() {
        Ok(status) if status.success() => {}
        Ok(status) => fail(&format!(
            "build.rs: archiving the oracle objects into `{}` exited with {status}.",
            archive.display()
        )),
        Err(error) => fail(&format!(
            "build.rs: could not execute the archiver to create `{}`: {error}",
            archive.display()
        )),
    }

    archive
}

/// Whether the archive has to be produced the MSVC way rather than the `ar` way.
///
/// Two independent signals, because either one alone has a gap. `CARGO_CFG_TARGET_ENV` is the
/// authoritative description of the toolchain cargo is building for, and it is what decides the
/// archive extension rustc will look for. The program name is checked as well so that an
/// explicitly configured `AR=lib.exe` -- which `cc` honours, and which a cross build can perfectly
/// well set on a non-`msvc` target -- is still driven with the command line that program accepts.
fn uses_msvc_archiver(program: &OsStr) -> bool {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        return true;
    }

    Path::new(program)
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem.eq_ignore_ascii_case("lib"))
}

/// Deletes `path` if it exists, and fails loudly if it exists but cannot be deleted.
fn remove_if_present(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        let already_absent = error.kind() == io::ErrorKind::NotFound;
        if !already_absent {
            fail(&format!(
                "build.rs: could not remove the stale artifact `{}`: {error}",
                path.display()
            ));
        }
    }
}

/// The collision-freedom gate: every defined external symbol in the archive must be prefixed.
///
/// This is what keeps the two rename lists honest as the reference sources evolve. A newly
/// exported C symbol shows up here as a precise diagnostic naming the symbol and the list to add
/// it to, instead of as a `multiple definition of ...` from the linker weeks later.
///
/// ★ THE TOOLCHAIN ALLOWANCE IS AN EXACT LIST, NOT A PREFIX RULE. This used to tolerate *every*
/// defined global whose name began with a double underscore, on the reasoning that the spelling is
/// reserved to the implementation and zlib defines no such name. Both halves of that are true and
/// the conclusion was still too wide: `__` is also the spelling a mistake takes when a future
/// reference source, a sanitizer, a profiler or an LTO plugin introduces a global this script has
/// never seen, and a blanket prefix rule passes all of them while printing a warning nobody reads.
/// The allowance is now [`TOOLCHAIN_SYMBOL_FAMILIES`] applied to a payload this script can
/// ACCOUNT FOR -- an ODR indicator, or a PC thunk, and in the ODR case only for a global whose name
/// appears in this script's own rename tables -- and any other `__` global fails the build with the
/// same diagnostic an unprefixed name gets.
///
/// It is also no longer an unconditional `cargo::warning`. The seven `__odr_asan.*` indicators an
/// `-fsanitize=address` build emits are a KNOWN, EXPECTED consequence of a configuration this
/// project asks for in CI, and warning about them on every sanitizer build made
/// `cargo +nightly test -p zlib-rs-differential` non-quiet forever, which contradicted the
/// zero-warning bar the project holds itself to. Expected families are recorded on this script's
/// own stdout, which cargo captures per build (`run/stdout` beside this crate's `out` directory on
/// current cargo, `output` on older layouts), and nothing else; only an unaccountable name is
/// loud, and it is loud by failing.
///
/// AN AUDIT THAT CANNOT RUN IS A BUILD FAILURE, not a warning.
///
/// It used to be a warning, on the reasoning that `nm` only observes and so its absence cannot
/// produce a wrong archive. That reasoning is true of the ARCHIVE and false of the CRATE. This is a
/// differential harness: its entire purpose is to hold the Rust implementation against the C one in
/// a single process, which is possible only because every C symbol was renamed out of the way. If
/// the rename is incomplete and nothing checks, the two definitions collide -- and the failure mode
/// is not a link error but the linker silently binding one definition for both, at which point the
/// tests compare an implementation against itself and pass. A harness that has verified nothing
/// while reporting success is the worst outcome this crate can produce, so the check is mandatory
/// and its absence stops the build with the complete list of what was tried.
fn verify_every_export_is_prefixed(archive: &Path) {
    println!("cargo::rerun-if-env-changed=NM");

    let (nm, listing) = match nm_listing(archive) {
        Ok(result) => result,
        Err(reason) => fail(&format!(
            "build.rs: the oracle archive's exports could not be audited for the \
             `{SYMBOL_PREFIX}` prefix, so this build is stopped rather than shipping an \
             unverified oracle: {reason}\n\
             That audit is what keeps the two rename lists complete. Without it an unrenamed C \
             symbol collides with the Rust definition of the same name, the linker binds one for \
             both, and the differential tests compare an implementation against itself and pass. \
             Install any of {}, or point NM at one:\n\
             \x20   NM=llvm-nm cargo test\n\
             `rustup component add llvm-tools` installs llvm-nm beside the toolchain on every \
             supported host.",
            NM_CANDIDATES.join(", ")
        )),
    };

    let mut unprefixed: Vec<&str> = Vec::new();
    let mut toolchain: Vec<&str> = Vec::new();
    let mut unaccountable: Vec<&str> = Vec::new();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let Some(symbol) = fields.next() else {
            continue;
        };
        // POSIX output interleaves `<archive>[<member>]:` header lines, which carry a single
        // field; a real symbol line always has at least a name and a type.
        let Some(kind) = fields.next().and_then(|field| field.chars().next()) else {
            continue;
        };
        // Defined and global, applied here rather than delegated, because the third rung of
        // NM_ARGUMENT_LADDER asks the tool for no filtering at all. Uppercase is global and `U` is
        // undefined; `u` is the one lowercase letter that means a defined global (GNU nm's unique
        // global), while `v` and `w` are weak UNDEFINED and are correctly excluded.
        let defined_global = (kind.is_ascii_uppercase() && kind != 'U') || kind == 'u';
        if !defined_global || symbol.starts_with(SYMBOL_PREFIX) {
            continue;
        }
        if symbol.starts_with("__") {
            if accountable_toolchain_symbol(symbol) {
                toolchain.push(symbol);
            } else {
                unaccountable.push(symbol);
            }
        } else {
            unprefixed.push(symbol);
        }
    }

    if !toolchain.is_empty() {
        toolchain.sort_unstable();
        toolchain.dedup();
        // Recorded, not warned: see this function's doc comment. These names are an expected
        // consequence of the configuration being built (a sanitizer build emits one ODR indicator
        // per instrumented global), they cannot collide with anything Rust defines, and every one
        // of them was matched against this script's own rename tables before it got here.
        println!(
            "zlib-rs-differential: the oracle archive exports {} accounted-for toolchain \
             symbol(s), each an artifact of the configuration rather than a zlib name: {}",
            toolchain.len(),
            join_names(&toolchain)
        );
    }

    if !unaccountable.is_empty() {
        unaccountable.sort_unstable();
        unaccountable.dedup();
        fail(&format!(
            "build.rs: {} global symbol(s) exported by `{}` begin with `__` but are not one of \
             the accounted-for toolchain families: {}.\n\
             The `__` spelling is reserved to the implementation, so a name like this is normally \
             a sanitizer, profiler or codegen artifact -- but it is not passed over on that \
             assumption, because it is also what a newly introduced global in the reference \
             sources would look like if it were spelled that way, and a blanket prefix allowance \
             would hide it. Either add the family to TOOLCHAIN_SYMBOL_FAMILIES in this script, \
             stating what emits it and why it cannot collide with libz-rs-sys, or add the \
             underlying name to PUBLIC_RENAMES / INTERNAL_REDEFINES so it is renamed like every \
             other export.",
            unaccountable.len(),
            archive.display(),
            join_names(&unaccountable)
        ));
    }

    if !unprefixed.is_empty() {
        unprefixed.sort_unstable();
        unprefixed.dedup();
        fail(&format!(
            "build.rs: {} symbol(s) exported by `{}`, as listed by `{}`, do not carry the \
             mandatory `{SYMBOL_PREFIX}` prefix: {}. Each one will collide with libz-rs-sys when \
             both are linked into a test binary. Add every public name to PUBLIC_RENAMES and every \
             ZLIB_INTERNAL name to INTERNAL_REDEFINES in this script.",
            unprefixed.len(),
            archive.display(),
            Path::new(&nm).display(),
            join_names(&unprefixed)
        ));
    }
}

/// Formats a name list for a diagnostic.
fn join_names(names: &[&str]) -> String {
    names.join(", ")
}

/// Whether `symbol` is a `__`-prefixed global this script can account for.
///
/// The gate [`verify_every_export_is_prefixed`] applies. Returns `false` for anything that is not
/// one of [`TOOLCHAIN_SYMBOL_FAMILIES`], and for an ODR indicator whose payload is not a global
/// this script renames -- so a sanitizer build of a reference source that gained a new global
/// stops the build rather than passing under the family allowance.
fn accountable_toolchain_symbol(symbol: &str) -> bool {
    for (prefix, kind) in TOOLCHAIN_SYMBOL_FAMILIES {
        let Some(payload) = symbol.strip_prefix(prefix) else {
            continue;
        };
        if payload.is_empty() {
            continue;
        }
        return match kind {
            ToolchainSymbol::PcThunk => true,
            ToolchainSymbol::OdrIndicator => renames_global_named(payload),
        };
    }
    false
}

/// Whether `name` is a global this script renames, under either spelling.
///
/// The three tables are the complete record of what the archive defines under a name of its own:
/// [`PUBLIC_RENAMES`] and [`INTERNAL_REDEFINES`] are renamed after compilation by `objcopy`, so a
/// derived symbol may carry either the source name or the `c_`-prefixed one, and
/// [`SHIM_LOCAL_RENAMES`] is applied by `-D` before compilation, so only the replacement spelling
/// can appear.
fn renames_global_named(name: &str) -> bool {
    let source_spelling = PUBLIC_RENAMES
        .iter()
        .chain(INTERNAL_REDEFINES.iter())
        .any(|renamed| *renamed == name);
    let prefixed_spelling = name
        .strip_prefix(SYMBOL_PREFIX)
        .is_some_and(|bare| PUBLIC_RENAMES.contains(&bare) || INTERNAL_REDEFINES.contains(&bare));
    let shim_spelling = SHIM_LOCAL_RENAMES
        .iter()
        .any(|(_, replacement)| *replacement == name);

    source_spelling || prefixed_spelling || shim_spelling
}

/// Publishes the archive to this crate's own targets: the library and the three test suites.
///
/// It reaches the `benches/` package too, and not by being listed there: that package depends on
/// this crate, so cargo replays these directives for anything that links this rlib. Nothing is
/// attached to this package by a `[[bench]]` entry.
///
/// Linking it crate-wide rather than for tests alone is harmless -- the crate is dev-only, so
/// nothing shipped is affected, and the linker pulls no member that nothing references.
fn emit_link_directives(out_dir: &Path) {
    println!("cargo::rustc-link-search=native={}", out_dir.display());
    println!("cargo::rustc-link-lib=static={ARCHIVE_STEM}");
}
