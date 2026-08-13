// The exported symbol surface only exists when the C ABI facade is compiled, and the 95-function
// contract includes the 32 `gz*` entry points, so the parity gate is meaningful only with both
// features on. `--no-default-features` is a supported configuration for this crate (it yields the
// `#[repr(C)]` ABI mirrors alone), and under it there is nothing to diff -- so the whole binary
// compiles away rather than reporting a false failure. `Makefile.in`'s `rust' target refuses a
// feature list missing either one for the same reason.
#![cfg(all(feature = "libz-compat", feature = "gz"))]
// The workspace denies the panic-prone family in `[workspace.lints.clippy]`, which is right for
// `src/**` -- a panic there would abort a C caller's process -- and wrong for a test, whose
// assertions panic by design. `clippy.toml`'s `allow-unwrap-in-tests` and friends key on `#[test]`
// context only, so the file-scope helpers below still need this, and `indexing_slicing` has no
// in-tests key at all. Nothing else is relaxed; in particular this file contains no `unsafe`.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The ABI **export** gate: the dynamic symbol table of the built library, diffed against the
//! measured C baseline.
//!
//! `layout_assertions.rs` catches struct drift at compile time. This suite catches the other half
//! of the contract, which no amount of type checking can see: a **missing**, **surplus**, or
//! **wrongly versioned** exported symbol. That is the failure mode which turns "drop-in
//! replacement" into a load-time `undefined symbol` error for every consumer on the machine, and it
//! is invisible to `cargo build`, to `cargo clippy` and to every test that calls a function by name
//! rather than by symbol.
//!
//! AAP 0.4.1.3 specifies this file as *"diff `nm -D --defined-only --extern-only` output against
//! the 111-symbol C baseline; assert the `local:` symbols stay hidden"*; AAP 0.6.3.6 states the
//! baseline, and AAP 0.8.6 gives the shell form of the same diff.
//!
//! # ★ Which artifact is under test, and why it is not the one `cargo build` emits
//!
//! This is the single most important thing to understand before editing this file, because the
//! obvious reading -- "`nm` the `libz.so` in `target/release`" -- checks the wrong file and would
//! have to be weakened to pass. `src/lib.rs` carries the authoritative artifact matrix; the two
//! rows this suite depends on are:
//!
//! | Artifact | Produced by | Dynamic globals |
//! |---|---|---|
//! | `target/<profile>/libz.so` (`cdylib`) | `cargo build` | **93**, of which **0** are version nodes |
//! | `target/dropin/libz.so.<ZLIB_VERSION>` | `make rust`, relinked from `libz.a` | **111** = 95 functions + 16 version nodes |
//!
//! rustc hands every `cdylib` link a version script of its own -- an anonymous tag listing the
//! crate's `#[no_mangle]` items under `global:`, with `local: *`. `zlib.map` therefore cannot be
//! layered on top of it (`ld.bfd` refuses outright: *"anonymous version tag cannot be combined with
//! other version tags"*), so the cargo `cdylib` carries **zero** of the 16 zlib version nodes -- 14
//! in the `ZLIB_1.2.*` family, plus `ZLIB_1.3.1.2` and `ZLIB_1.3.2`, all enumerated in
//! [`VERSION_NODES`] below -- and
//! no argument to cargo changes that. The installable shared object is produced by one documented
//! step that relinks the *complete archive* under `zlib.map`, and that step is where the 111-symbol
//! surface comes from:
//!
//! ```text
//! cargo build --release -p libz-rs-sys --features libz-compat,gz   # libz.a + libz.so + libz.rlib
//! make rust                                                        # target/dropin/: the drop-in
//! ```
//!
//! So the suite tests **both**, with different expectations, and encodes the difference rather than
//! papering over it:
//!
//! * The **packaged** library is held to full parity -- 111 symbols, the 95/16 type split, the exact
//!   decorated strings in both directions, no hidden name leaked, `SONAME libz.so.1`, and the
//!   versioned symlink chain.
//! * The **cargo** `cdylib` is held to its own measured shape: 93 type-`T` symbols, no version
//!   nodes, and a delta from the contract that is exactly `gzprintf` and `gzvprintf` absent -- no
//!   internal helper is exported, because the `.hidden` directives in
//!   `crates/libz-rs-sys/src/lib.rs` give all three ELF `STV_HIDDEN` visibility, which rustc's
//!   own version script cannot undo. That is a *development* artifact -- convenient, produced by
//!   every `cargo build`, and marked not installable in the artifact matrix -- so pinning its shape
//!   is not a concession and not a parity claim either: it detects an unintended new export, or an
//!   accidental loss of one, and it stops the deviation from drifting into something wider without a
//!   test going red. What remains between it and the contract is two variadic C entry points and
//!   sixteen version nodes, and both gaps are structural rather than editable: see the artifact
//!   matrix in `src/lib.rs`.
//! * The **cargo** `libz.a` is held to the *whole* contract -- all 95 functions defined as global
//!   text symbols, `gzprintf` and `gzvprintf` among them. That is the positive claim the artifact
//!   matrix makes about the direct-cargo command, and
//!   [`cargo_staticlib_defines_the_whole_contract`] is what turns it into a gate. It matters that
//!   the two cargo tests sit side by side: one pins an artifact that is deliberately incomplete,
//!   and without the other the suite would describe a shortfall while asserting nothing about the
//!   artifact the shared library is relinked from.
//!
//! # ★ Full decorated strings, never bare names
//!
//! Every comparison uses the string `nm` prints, `@@`-decoration included. The version node a
//! symbol is bound to **is part of the ABI**: a consumer that resolved `deflateUsed@@ZLIB_1.3.1.2`
//! must keep resolving it, and a build that emitted `deflateUsed@@ZLIB_1.2.9` instead would satisfy
//! a bare-name comparison while breaking every such consumer. Comparing bare names is a silently
//! weaker test that looks identical in the source.
//!
//! # ★ Skip versus fail -- the distinction is load-bearing
//!
//! A missing *artifact* or a missing *tool* is not a contract violation, and reporting one as a
//! failure would train readers to ignore this suite. Every test therefore prints one `SKIP:` line
//! naming the remedy and returns when the library it inspects has not been built, when `nm` or
//! `readelf` is unavailable, when the host is not ELF, or when running under Miri (which cannot
//! spawn a process). This is not defensive padding: `cargo test` **never builds the `cdylib`** --
//! measured by deleting `target/release/libz.so` and running both `cargo test -p libz-rs-sys` and a
//! single `--test` invocation, neither of which recreated it -- and nothing obliges a checkout to
//! have run `make rust` at all.
//!
//! What is *forbidden* is the converse: a real diff must never be downgraded to a skip, and a name
//! must never be removed from the baseline to make a build pass. AAP 0.7.1 (b) makes the exported
//! surface immutable; if the diff fails, the facade's exports are wrong, not the baseline.
//!
//! ★ And a skip is only ever legitimate while nobody has claimed the artifact is there. A harness
//! that has just staged the drop-in sets **`ZLIB_RS_REQUIRE_PACKAGED=1`**, and every skip in the
//! packaged group then becomes a failure naming what went unverified -- see [`packaged_required`].
//! Without that switch the strongest gates in this file are also the easiest to satisfy vacuously: a
//! CI job could run `make rust`, run this suite, print eight `SKIP:` lines and be recorded green,
//! which is the state the `symbols` job of `.github/workflows/rust.yml` now forbids twice over -- it
//! arms the variable, and it fails on any `SKIP:` line in the output.
//!
//! # ★ Why `SONAME` and the symlink chain are asserted here, and must not be deleted as redundant
//!
//! The `SONAME` is `libz.so.1`, so that is the name the dynamic loader searches for. When a
//! directory offers only `libz.so`, a program linked `-L<dir> -lz` still builds and then binds
//! `/lib/.../libz.so.1` -- the **system** zlib -- at run time. That was reproduced during planning:
//! the probe printed the system version until `-Wl,-rpath` was added. A "drop-in replacement" check
//! can therefore pass while exercising a different library entirely, and the only defence is to
//! require the chain to exist and to check the binding rather than infer it from a passing run.
//! `make rust-test` does the `ldd` half; this file does the `SONAME`-and-chain half.
//!
//! Note that `build.rs` deliberately keeps the versioned names **out** of the cargo artifact
//! directory: a build script runs before the link, so it cannot tell a `cargo check` from a
//! `cargo build`, and each of `cargo check`, a failed build and `cargo clean -p` used to leave the
//! two names behind as dangling links -- which the loader skips exactly as if they were absent, and
//! which once produced a hard `as: symbol lookup error: undefined symbol: deflate` because cargo
//! puts that directory first on the library search path of the build scripts it launches. Their
//! absence there is asserted as a positive requirement, for the same reason their presence is
//! asserted in `target/dropin`.
//!
//! # ★ The `inflate_table` asymmetry is a requirement, not a leak
//!
//! `inflate_table` is in `zlib.map`'s `local:` block, yet it must be a defined global in `libz.a`:
//! `test/infcover.c`'s `cover_trees()` calls it **directly**, twice (L632 and L636), and
//! `test/CMakeLists.txt` L95-96 links `infcover` against `ZLIB::ZLIBSTATIC`. Hide it from the
//! archive and `infcover` does not link; leave it in either shared object's dynamic table and the
//! parity diff fails. Two mechanisms resolve the two requirements: the version script for the
//! packaged library, and `.hidden` for the `cdylib` cargo links directly. Neither reaches a static
//! archive. The C build behaves identically -- verified here: `nm libz.a` reports `T inflate_table`
//! while `nm -D` on the reference `libz.so.1.3.2.1-motley` reports zero occurrences -- so this is
//! parity, not an exception to it. It is asserted positively so that a later reader cannot mistake
//! it for a defect and "fix" it.
//!
//! The symbol is defined in Rust, in `src/inflate.rs`, over a plain `int` that it validates; the C
//! translation unit that used to present `inftrees.h`'s `codetype` prototype for it is gone.
//!
//! # ★ `zlib.map` has two `local:` blocks
//!
//! `ZLIB_1.2.0` (L9-L19) hides nine names plus the `_*` pattern, and `ZLIB_1.3.2` (L114-L115) hides
//! `inflate_fixed`. A test that read only the first would let `inflate_fixed` leak while looking
//! thorough. Both are covered, and [`the_hidden_name_baseline_matches_zlib_map`] proves the coverage
//! by parsing the contract itself rather than by assertion.
//!
//! # How the baseline was produced
//!
//! `./configure && make` on the in-tree C sources, then
//! `nm -D --defined-only --extern-only libz.so.1.3.2.1-motley`. It is measured, and it was
//! regenerated independently while writing this file: the C reference, the packaged Rust library and
//! the constants below all produced byte-identical sorted symbol lists. Per-module export tallies in
//! the facade's own documentation are a cross-check, never the authority -- the 111-symbol diff is.
//!
//! # Dependencies
//!
//! `std` only, by AAP 0.7.1 (i): `std::process::Command` drives `nm` and `readelf`, and
//! `std::env`/`std::fs`/`std::path` locate the artifacts. No `[dev-dependencies]` are introduced --
//! no `tempfile`, no `assert_cmd`, no `object`/`goblin` -- and no `unsafe` appears anywhere in this
//! file, per AAP 0.7.1 (a).

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

// Puts the `rlib` on this test binary's link line, so `cargo test` cannot run this suite against a
// crate it failed to compile. It deliberately does NOT imply that the `cdylib` or the staticlib
// exist: cargo builds neither for a test target, which is precisely why every test below is
// skip-guarded on the artifact it inspects.
use z as _;

// =================================================================================================
// The measured baseline
// =================================================================================================

/// The 16 `nm` type-`A` symbol version nodes, exactly as `zlib.map` declares them.
///
/// A version node is a symbol in its own right, which is why the total is 111 rather than 95. Note
/// that they are not all one family: fourteen are `ZLIB_1.2.*`, and the two most recent are
/// `ZLIB_1.3.1.2` and `ZLIB_1.3.2`. "The 16 `ZLIB_1.2.*` nodes" is therefore the wrong shorthand for
/// this set, however tempting; say "the 16 version nodes" and let the list below be the authority.
///
/// The order here follows `zlib.map`'s declaration order (each node inherits the previous one), not
/// lexicographic order; [`the_version_node_baseline_matches_zlib_map`] checks the set against the
/// contract text.
const VERSION_NODES: [&str; 16] = [
    "ZLIB_1.2.0",
    "ZLIB_1.2.0.2",
    "ZLIB_1.2.0.8",
    "ZLIB_1.2.2",
    "ZLIB_1.2.2.3",
    "ZLIB_1.2.2.4",
    "ZLIB_1.2.3.3",
    "ZLIB_1.2.3.4",
    "ZLIB_1.2.3.5",
    "ZLIB_1.2.5.1",
    "ZLIB_1.2.5.2",
    "ZLIB_1.2.7.1",
    "ZLIB_1.2.9",
    "ZLIB_1.2.12",
    "ZLIB_1.3.1.2",
    "ZLIB_1.3.2",
];

/// The 54 exported functions that carry `@@`-decoration, as the full strings `nm` prints.
///
/// Grouped by the node each one binds to, in `zlib.map`'s declaration order, so that the grouping
/// is itself reviewable against the contract: a name in the wrong group is a name bound to the
/// wrong version, which is an ABI break that a bare-name comparison cannot see.
const VERSIONED_EXPORTS: [&str; 54] = [
    // ZLIB_1.2.0 -- zlib.map L1-L20 (6)
    "compressBound@@ZLIB_1.2.0",
    "deflateBound@@ZLIB_1.2.0",
    "inflateBack@@ZLIB_1.2.0",
    "inflateBackEnd@@ZLIB_1.2.0",
    "inflateBackInit_@@ZLIB_1.2.0",
    "inflateCopy@@ZLIB_1.2.0",
    // ZLIB_1.2.0.2 -- L22-L26 (3)
    "gzclearerr@@ZLIB_1.2.0.2",
    "gzungetc@@ZLIB_1.2.0.2",
    "zlibCompileFlags@@ZLIB_1.2.0.2",
    // ZLIB_1.2.0.8 -- L28-L30 (1)
    "deflatePrime@@ZLIB_1.2.0.8",
    // ZLIB_1.2.2 -- L32-L37 (4)
    "adler32_combine@@ZLIB_1.2.2",
    "crc32_combine@@ZLIB_1.2.2",
    "deflateSetHeader@@ZLIB_1.2.2",
    "inflateGetHeader@@ZLIB_1.2.2",
    // ZLIB_1.2.2.3 -- L39-L42 (2)
    "deflateTune@@ZLIB_1.2.2.3",
    "gzdirect@@ZLIB_1.2.2.3",
    // ZLIB_1.2.2.4 -- L44-L46 (1)
    "inflatePrime@@ZLIB_1.2.2.4",
    // ZLIB_1.2.3.3 -- L48-L55 (6)
    "adler32_combine64@@ZLIB_1.2.3.3",
    "crc32_combine64@@ZLIB_1.2.3.3",
    "gzopen64@@ZLIB_1.2.3.3",
    "gzseek64@@ZLIB_1.2.3.3",
    "gztell64@@ZLIB_1.2.3.3",
    "inflateUndermine@@ZLIB_1.2.3.3",
    // ZLIB_1.2.3.4 -- L57-L60 (2)
    "inflateMark@@ZLIB_1.2.3.4",
    "inflateReset2@@ZLIB_1.2.3.4",
    // ZLIB_1.2.3.5 -- L62-L68 (5)
    "gzbuffer@@ZLIB_1.2.3.5",
    "gzclose_r@@ZLIB_1.2.3.5",
    "gzclose_w@@ZLIB_1.2.3.5",
    "gzoffset@@ZLIB_1.2.3.5",
    "gzoffset64@@ZLIB_1.2.3.5",
    // ZLIB_1.2.5.1 -- L70-L72 (1)
    "deflatePending@@ZLIB_1.2.5.1",
    // ZLIB_1.2.5.2 -- L74-L78 (3)
    "deflateResetKeep@@ZLIB_1.2.5.2",
    "gzgetc_@@ZLIB_1.2.5.2",
    "inflateResetKeep@@ZLIB_1.2.5.2",
    // ZLIB_1.2.7.1 -- L80-L83 (2)
    "gzvprintf@@ZLIB_1.2.7.1",
    "inflateGetDictionary@@ZLIB_1.2.7.1",
    // ZLIB_1.2.9 -- L85-L94 (8)
    "adler32_z@@ZLIB_1.2.9",
    "crc32_z@@ZLIB_1.2.9",
    "deflateGetDictionary@@ZLIB_1.2.9",
    "gzfread@@ZLIB_1.2.9",
    "gzfwrite@@ZLIB_1.2.9",
    "inflateCodesUsed@@ZLIB_1.2.9",
    "inflateValidate@@ZLIB_1.2.9",
    "uncompress2@@ZLIB_1.2.9",
    // ZLIB_1.2.12 -- L96-L100 (3)
    "crc32_combine_gen@@ZLIB_1.2.12",
    "crc32_combine_gen64@@ZLIB_1.2.12",
    "crc32_combine_op@@ZLIB_1.2.12",
    // ZLIB_1.3.1.2 -- L102-L104 (1)
    "deflateUsed@@ZLIB_1.3.1.2",
    // ZLIB_1.3.2 -- L106-L116 (6)
    "compress2_z@@ZLIB_1.3.2",
    "compressBound_z@@ZLIB_1.3.2",
    "compress_z@@ZLIB_1.3.2",
    "uncompress2_z@@ZLIB_1.3.2",
    "uncompress_z@@ZLIB_1.3.2",
    "deflateBound_z@@ZLIB_1.3.2",
];

/// The 41 exported functions with no `@@`-decoration -- the base set, present since before
/// `zlib.map` began versioning additions.
///
/// Two entries deserve a note, because both look like mistakes and are not:
///
/// * `gzgetc` is here **and** `gzgetc_@@ZLIB_1.2.5.2` is in [`VERSIONED_EXPORTS`]. `zlib.h` L1967
///   defines `gzgetc` as a macro whose fallback branch is `(gzgetc)(g)` -- parenthesised precisely
///   so it calls the function -- so both symbols are required.
/// * `gzopen_w` is deliberately **absent**. `zlib.h` L2041-L2044 declares it only under
///   `#if defined(_WIN32) && !defined(Z_SOLO)`, the facade gates it on `#[cfg(windows)]`, and the
///   measured Linux reference library does not export it.
const UNVERSIONED_EXPORTS: [&str; 41] = [
    "adler32",
    "compress",
    "compress2",
    "crc32",
    "deflate",
    "deflateCopy",
    "deflateEnd",
    "deflateInit2_",
    "deflateInit_",
    "deflateParams",
    "deflateReset",
    "deflateSetDictionary",
    "get_crc_table",
    "gzclose",
    "gzdopen",
    "gzeof",
    "gzerror",
    "gzflush",
    "gzgetc",
    "gzgets",
    "gzopen",
    "gzprintf",
    "gzputc",
    "gzputs",
    "gzread",
    "gzrewind",
    "gzseek",
    "gzsetparams",
    "gztell",
    "gzwrite",
    "inflate",
    "inflateEnd",
    "inflateInit2_",
    "inflateInit_",
    "inflateReset",
    "inflateSetDictionary",
    "inflateSync",
    "inflateSyncPoint",
    "uncompress",
    "zError",
    "zlibVersion",
];

/// The ten names `zlib.map` lists under `local:`, across **both** of its `local:` blocks:
/// `ZLIB_1.2.0` (L10-L18) supplies the first nine, and `ZLIB_1.3.2` (L115) supplies
/// `inflate_fixed`.
///
/// None may appear in the packaged library's dynamic table. Several are nonetheless defined globals
/// in `libz.a`, where no version script applies -- see [`inflate_table_static_only`].
const HIDDEN_SYMBOLS: [&str; 10] = [
    "deflate_copyright",
    "inflate_copyright",
    "inflate_fast",
    "inflate_table",
    "zcalloc",
    "zcfree",
    "z_errmsg",
    "gz_error",
    "gz_intmax",
    "inflate_fixed",
];

/// The two contract functions the cargo `cdylib` cannot export.
///
/// Both are defined in `csrc/gzprintf_shim.c` because `gzprintf` is variadic and `gzvprintf` takes
/// a `va_list`, neither of which stable Rust can define. rustc's anonymous version script ends in
/// `local: *`, so a symbol contributed by a C object gets no dynamic entry, and
/// `-Wl,--export-dynamic-symbol` does not override a version script. They reappear in the packaged
/// library, which is relinked from the archive under `zlib.map`.
///
/// ★ **What this file establishes about these two, and what it does not.** Everything here is an
/// ABI assertion: the name is present at the right version node in the packaged library, absent from
/// the `cdylib`, and neither hidden nor duplicated. That is a different claim from "the function
/// works", and for a variadic entry point the gap between the two is wider than usual -- `nm` cannot
/// see a calling convention. The behavioural half is owned by `tests/c_api_parity.rs`:
/// `gzprintf` is called there variadically with a format and a matched argument, and `gzvprintf` is
/// called through `csrc/gzvprintf_probe.c`, a test-only C translation unit that builds a real
/// `va_list`. The two halves are deliberately separate tests, because a symbol can be correctly
/// exported and correctly versioned and still be an adapter nobody ever ran.
const CDYLIB_ABSENT: [&str; 2] = ["gzprintf", "gzvprintf"];

/// The three internal helpers **neither** library exports, and the reason the two agree.
///
/// The packaged library hides them because it is relinked under `zlib.map`: `inflate_table` is
/// named in the `ZLIB_1.2.0` `local:` block outright at L13, and the two `_zlib_rs_gzprintf_*`
/// helpers match its trailing `_*` pattern. The cargo `cdylib` is linked under rustc's own
/// anonymous script, which lists every `#[no_mangle]` item of the crate under `global:` -- so all
/// three USED to appear in its dynamic table, and it exported 96 names where the contract has 93.
/// The `.hidden` directives in `crates/libz-rs-sys/src/lib.rs` are what closed that gap: ELF
/// `STV_HIDDEN` keeps a symbol out of a shared object's dynamic table on both `rust-lld` and
/// `ld.bfd`, in debug and under fat LTO, and affects static linking not at all.
///
/// So this array is now an ABSENCE assertion for both artifacts rather than a presence
/// assertion for one, which is why [`underscore_symbols_hidden`] can require zero
/// underscore-prefixed names from either artifact and [`version_script_local_symbols_hidden`]
/// zero `local:` names. [`inflate_table_static_only`] is the other half: all three remain
/// ordinary global text symbols in `libz.a`, which is what `test/infcover.c` and
/// `csrc/gzprintf_shim.c` need.
const CDYLIB_INTERNALS: [&str; 3] = [
    "_zlib_rs_gzprintf_begin",
    "_zlib_rs_gzprintf_commit",
    "inflate_table",
];

/// Functions the packaged library exports: 54 decorated plus 41 undecorated.
const FUNCTION_TOTAL: usize = VERSIONED_EXPORTS.len() + UNVERSIONED_EXPORTS.len();

/// Dynamic globals the packaged library exports: the functions plus the version nodes.
const DYNAMIC_GLOBAL_TOTAL: usize = FUNCTION_TOTAL + VERSION_NODES.len();

/// Dynamic globals the cargo `cdylib` exports: the contract, less the two variadic functions a
/// C object contributes and rustc's `local: *` therefore cannot re-export. Nothing is added,
/// because [`CDYLIB_INTERNALS`] is hidden in this artifact too.
const CDYLIB_TOTAL: usize = FUNCTION_TOTAL - CDYLIB_ABSENT.len();

// The arithmetic is asserted at compile time, not merely commented, so that an edit which adds a
// name to one array without adjusting the others cannot silently unbalance the baseline. AAP 0.6.3.6
// fixes all four figures.
const _: () = assert!(
    FUNCTION_TOTAL == 95,
    "zlib.h declares 95 functions off Windows"
);
const _: () = assert!(
    VERSION_NODES.len() == 16,
    "zlib.map declares 16 version nodes"
);
const _: () = assert!(
    DYNAMIC_GLOBAL_TOTAL == 111,
    "the measured C baseline is 111 symbols"
);
const _: () = assert!(
    CDYLIB_TOTAL == 93,
    "the measured cargo cdylib exports 93 symbols"
);

/// The `SONAME` both libraries must record, and the name the loader searches for.
const EXPECTED_SONAME: &str = "libz.so.1";

/// The immutable public contract, pinned into the test binary at compile time.
///
/// Read for one value -- `ZLIB_VERSION` -- from which the packaged library's file name is derived,
/// exactly as `Makefile.in`'s `RUSTZVERCMD` derives it. Deriving beats hardcoding
/// `libz.so.1.3.2.1-motley` here: a version bump then moves this test with the contract instead of
/// breaking it, and the value cannot drift out of step with the header.
const ZLIB_H: &str = include_str!("../../../zlib.h");

/// The immutable version script, pinned the same way.
///
/// Read so that the constants above can be checked against the contract they claim to mirror. That
/// is not circular: [`VERSION_NODES`] and [`HIDDEN_SYMBOLS`] were measured from the built C library,
/// while this text is the source the linker consumes, so agreement between them is evidence that
/// neither the library nor the contract has drifted.
const ZLIB_MAP: &str = include_str!("../../../zlib.map");

// =================================================================================================
// Reading the contract
// =================================================================================================

/// `ZLIB_VERSION` as `zlib.h` L44 defines it -- `1.3.2.1-motley` on this tree.
///
/// Panics rather than skipping if the header does not define it: the header is pinned into this
/// binary at compile time, so its absence is a broken contract, not a missing artifact.
fn zlib_version() -> &'static str {
    for line in ZLIB_H.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#define ZLIB_VERSION") else {
            continue;
        };
        // `#define ZLIB_VERSION "1.3.2.1-motley"` -- take what the quotes enclose.
        if let Some((_, after_open)) = rest.split_once('"') {
            if let Some((value, _)) = after_open.split_once('"') {
                assert!(!value.is_empty(), "zlib.h defines an empty ZLIB_VERSION");
                return value;
            }
        }
    }
    panic!("zlib.h does not define ZLIB_VERSION; the pinned public contract is unrecognisable");
}

/// The file name `make rust` gives the real shared object, e.g. `libz.so.1.3.2.1-motley`.
fn packaged_file_name() -> String {
    format!("libz.so.{}", zlib_version())
}

/// The symbol version node names `zlib.map` declares, in declaration order.
///
/// A node opens with `<name> {` at the start of a line; the closing `} <predecessor>;` lines and the
/// `global:` / `local:` labels are skipped because neither shape matches.
fn map_version_nodes() -> Vec<&'static str> {
    ZLIB_MAP
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix('{'))
        .map(str::trim_end)
        .filter(|name| !name.is_empty() && !name.starts_with('}'))
        .collect()
}

/// Every name listed under a `local:` label in `zlib.map`, from **all** of its blocks, with the
/// wildcard entries reported separately.
///
/// Returns `(names, patterns)`. On this tree that is the nine names of `ZLIB_1.2.0` plus
/// `inflate_fixed` from `ZLIB_1.3.2`, and the single pattern `_*`. Walking every block is the whole
/// point: a reader who stopped at the first `local:` would miss `inflate_fixed` entirely.
fn map_local_entries() -> (Vec<&'static str>, Vec<&'static str>) {
    let mut names = Vec::new();
    let mut patterns = Vec::new();
    let mut inside_local = false;

    for raw in ZLIB_MAP.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        // A node header (`NAME {`) or a node terminator (`} PRED;`) ends any local section, and a
        // `global:` label switches back out of one. Only `local:` switches in.
        if line.ends_with('{') || line.starts_with('}') || line == "global:" {
            inside_local = false;
            continue;
        }
        if line == "local:" {
            inside_local = true;
            continue;
        }
        if !inside_local {
            continue;
        }
        let Some(entry) = line.strip_suffix(';') else {
            continue;
        };
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.contains('*') {
            patterns.push(entry);
        } else {
            names.push(entry);
        }
    }

    (names, patterns)
}

// =================================================================================================
// Locating the artifacts
// =================================================================================================

/// The repository root, resolved from this crate's manifest directory at compile time.
fn repo_root() -> PathBuf {
    // `<repo>/crates/libz-rs-sys` -> `<repo>`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("CARGO_MANIFEST_DIR has fewer than two ancestors")
}

/// Cargo's target directory: `CARGO_TARGET_DIR` when the environment sets it, else `<repo>/target`.
fn target_root() -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => repo_root().join("target"),
    }
}

/// The profile directory this test binary was built into -- `target/release`, `target/debug`, or the
/// same under a `--target <triple>` prefix or a custom profile name.
///
/// Derived from the running executable rather than guessed from `cfg!(debug_assertions)`, which
/// tracks a compiler flag and not the profile's name: a release profile with debug assertions on
/// would send that guess to the wrong directory. Cargo places an integration-test binary in
/// `<profile>/deps/`, so one step up from `deps` is the answer, and the `deps` check is conditional
/// so that a harness which relocates the binary still yields a plausible directory.
fn profile_dir() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let holder = exe.parent()?;
    if holder.file_name().is_some_and(|name| name == "deps") {
        return holder.parent().map(Path::to_path_buf);
    }
    Some(holder.to_path_buf())
}

/// Where `make rust` stages the packaged drop-in.
///
/// `Makefile.in` sets `RUSTLIBDIR=$(RUSTTARGETDIR)/dropin` with `RUSTTARGETDIR=target` relative to
/// the build directory, so `<repo>/target/dropin` is the canonical location. A `CARGO_TARGET_DIR`
/// override moves cargo's output but not the Makefile's, so both are probed and the first that
/// exists wins.
fn packaged_dir() -> Option<PathBuf> {
    [
        target_root().join("dropin"),
        repo_root().join("target").join("dropin"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_dir())
}

// =================================================================================================
// Skips
// =================================================================================================

/// Reports that a test could not run, and why.
///
/// Skips are printed rather than asserted, and the wording always names the remedy so that a reader
/// can tell "not built yet" from "broken". `cargo test` captures stdout, so add `--nocapture` to see
/// these lines on an otherwise green run.
fn skip(reason: &str) {
    println!("SKIP: {reason}");
}

/// The environment variable that arms the packaged-artifact gates.
///
/// See [`packaged_required`]. Named here as a constant because three places have to agree on the
/// spelling: this file, the `symbols` job of `.github/workflows/rust.yml`, and the `README`
/// paragraph that documents the packaging step.
const REQUIRE_PACKAGED_VAR: &str = "ZLIB_RS_REQUIRE_PACKAGED";

/// Whether the caller has declared that the packaged drop-in **must** exist and be inspectable.
///
/// # ★ Why an arming switch exists at all
///
/// The packaged gates below -- full 111-symbol parity, the 95/16 split, the hidden set, the
/// `SONAME`, the symlink topology -- describe the artifact that actually ships. Every one of them
/// skips when `target/dropin` has not been populated, which is right for a developer who has only
/// run `cargo test`, and *wrong* for continuous integration: a job that stages the drop-in and then
/// runs a suite which silently skips every assertion about it reports success while verifying
/// nothing. That is not a hypothetical -- it is the state this switch was added to end, and it is
/// the reason the AAP's parity requirement (0.4.1.3, 0.6.3.6) could be satisfied on paper by a
/// green run that never opened the file.
///
/// So the harness that staged the artifact says so, and every skip in the packaged group becomes a
/// failure naming what went unverified. `ZLIB_RS_REQUIRE_PACKAGED=1 cargo test -p libz-rs-sys
/// --test symbol_parity` is the armed form; unset, or `0`, is the developer default.
///
/// A value that is neither is a hard error rather than a fall-through to disarmed. A typo in a CI
/// expression -- `ZLIB_RS_REQUIRE_PACKAGED=true `, `yes`, `on` -- must not quietly turn the gate
/// off, because the whole point of the switch is that its absence is invisible. `Makefile.in`
/// refuses a malformed `ZLIB_RS_SIMD` for the same reason.
fn packaged_required() -> bool {
    match env::var(REQUIRE_PACKAGED_VAR) {
        Err(_) => false,
        Ok(value) => match value.trim() {
            "" | "0" => false,
            "1" => true,
            other => panic!(
                "{REQUIRE_PACKAGED_VAR}={other:?} is not a recognised value: use 1 to require the \
                 packaged drop-in staged by `make rust`, or 0 (or leave it unset) to let the \
                 packaged gates skip when it has not been built. Anything else is refused rather \
                 than treated as 0, because a silently disarmed gate is exactly the failure this \
                 variable exists to prevent"
            ),
        },
    }
}

/// Reports that a **packaged**-artifact gate could not run: a skip when disarmed, a failure when
/// [`packaged_required`] says the artifact had to be there.
///
/// The reason string is written once and used in both modes, so the armed failure and the disarmed
/// skip name the same remedy and cannot drift apart.
fn packaged_unavailable(reason: &str) {
    assert!(
        !packaged_required(),
        "{REQUIRE_PACKAGED_VAR}=1 declares that the packaged drop-in was staged, but {reason}. \
         The packaged library is the artifact that ships -- cargo's own `libz.so` is not \
         installable (see the artifact matrix in src/lib.rs) -- so this is a failure rather than a \
         skip: with it skipped, nothing in this suite has inspected the shared library at all"
    );
    skip(reason);
}

/// Whether this host can be inspected with ELF tooling at all.
///
/// Three conditions, each a legitimate skip rather than a failure:
///
/// * Miri cannot spawn a process, so every `nm` call would abort. `.github/workflows/rust.yml` scopes
///   Miri to `-p zlib-rs`, so this should never fire -- it costs one line and removes the doubt.
/// * A non-ELF host (macOS, Windows) has no `SONAME`, no `@@`-decorated symbols and a different
///   library naming scheme, so the baseline simply does not describe it. AAP 0.2.2.3 puts non-Tier-1
///   targets out of scope and the reference measurements are `x86_64-unknown-linux-gnu`.
/// * `nm` and `readelf` come from binutils, which a minimal container need not carry.
fn elf_tooling_available() -> bool {
    if cfg!(miri) {
        skip("running under Miri, which cannot spawn `nm`/`readelf`");
        return false;
    }
    if !cfg!(target_os = "linux") {
        skip("host is not ELF/Linux; the measured symbol baseline describes a Linux shared object");
        return false;
    }
    for tool in ["nm", "readelf"] {
        if tool_output(tool, &["--version"]).is_none() {
            return false;
        }
    }
    true
}

/// Runs a binutils tool and returns its stdout.
///
/// `None` means the program could not be spawned -- a missing tool, which is a skip. A tool that ran
/// and *failed* is a different matter and panics: the artifact was already confirmed to exist, so a
/// non-zero exit means the file is not the object this suite believes it is, and hiding that behind a
/// skip would suppress a real finding.
fn tool_output(program: &str, args: &[&str]) -> Option<String> {
    let outcome = match Command::new(program).args(args).output() {
        Ok(outcome) => outcome,
        Err(error) => {
            skip(&format!(
                "`{program}` is unavailable on PATH ({error}); install binutils"
            ));
            return None;
        }
    };
    assert!(
        outcome.status.success(),
        "`{program} {}` failed with {}:\n{}",
        args.join(" "),
        outcome.status,
        String::from_utf8_lossy(&outcome.stderr)
    );
    Some(String::from_utf8_lossy(&outcome.stdout).into_owned())
}

// =================================================================================================
// Reading a symbol table
// =================================================================================================

/// One entry of a dynamic symbol table: the `nm` type letter and the name exactly as printed,
/// `@@`-decoration included.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DynSymbol {
    kind: char,
    name: String,
}

/// Parses `nm` output into symbols, keeping the printed spelling verbatim.
///
/// `nm` emits `<address> <type> <name>` for a function and `<type> <name>` for a version node, which
/// has no address -- so the fields are taken from the **end** of the line rather than by column. A
/// line with fewer than two fields, or whose type field is not a single character, is not a symbol
/// (archive member headers such as `gzprintf_shim.o:` take that shape) and is skipped.
fn parse_nm(output: &str) -> Vec<DynSymbol> {
    let mut symbols = Vec::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace().rev();
        let (Some(name), Some(kind)) = (fields.next(), fields.next()) else {
            continue;
        };
        let mut letters = kind.chars();
        let (Some(letter), None) = (letters.next(), letters.next()) else {
            continue;
        };
        symbols.push(DynSymbol {
            kind: letter,
            name: name.to_owned(),
        });
    }
    symbols
}

/// The dynamic symbol table of a shared object, as the AAP 0.8.6 command reads it.
fn dynamic_symbols(library: &Path) -> Option<Vec<DynSymbol>> {
    let path = library.to_str().expect("artifact path is not valid UTF-8");
    let output = tool_output("nm", &["-D", "--defined-only", "--extern-only", path])?;
    Some(parse_nm(&output))
}

/// The outcome of asking `readelf` for a shared object's `DT_SONAME`.
///
/// Three outcomes have to stay distinguishable, and they mean quite different things, so they get a
/// named type rather than a nested `Option`: a missing tool is a skip, a missing `DT_SONAME` is a
/// packaging failure, and a recorded value is something to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SonameProbe {
    /// `readelf` could not be spawned. The skip has already been reported.
    ToolUnavailable,
    /// `readelf` ran, and the object records no `DT_SONAME` at all.
    Absent,
    /// The value the object records.
    Recorded(String),
}

/// The `DT_SONAME` a shared object records, e.g. `libz.so.1`.
///
/// `readelf -d` prints `0x...e (SONAME)  Library soname: [libz.so.1]`; the value is what the
/// brackets enclose.
fn recorded_soname(library: &Path) -> SonameProbe {
    let path = library.to_str().expect("artifact path is not valid UTF-8");
    let Some(output) = tool_output("readelf", &["-d", path]) else {
        return SonameProbe::ToolUnavailable;
    };
    for line in output.lines() {
        if !line.contains("(SONAME)") {
            continue;
        }
        if let Some((_, after)) = line.split_once('[') {
            if let Some((value, _)) = after.split_once(']') {
                return SonameProbe::Recorded(value.trim().to_owned());
            }
        }
    }
    SonameProbe::Absent
}

// =================================================================================================
// Guards that resolve an artifact or explain its absence
// =================================================================================================

/// The packaged shared object `make rust` stages, or `None` with a `SKIP:` line -- or a failure,
/// when [`packaged_required`] is armed.
fn packaged_library() -> Option<PathBuf> {
    if !elf_tooling_available() {
        packaged_unavailable(
            "the ELF tooling this suite reads the packaged library with is unavailable, so its \
             symbol table, SONAME and version nodes went unverified; install binutils (`nm`, \
             `readelf`)",
        );
        return None;
    }
    let Some(directory) = packaged_dir() else {
        packaged_unavailable(&format!(
            "no packaged library directory at {}; run `make rust` (or `make rust CARGO=cargo`) \
             to relink libz.a under zlib.map and stage the drop-in",
            target_root().join("dropin").display()
        ));
        return None;
    };
    let library = directory.join(packaged_file_name());
    if !library.is_file() {
        packaged_unavailable(&format!(
            "packaged library not found at {}; run `make rust` to stage it",
            library.display()
        ));
        return None;
    }
    Some(library)
}

/// The `cdylib` cargo emits, or `None` with a `SKIP:` line.
///
/// Absence is entirely routine: cargo builds no `cdylib` for a test target, so a checkout whose only
/// command has been `cargo test` will not have one. `cargo build -p libz-rs-sys` produces it.
fn cargo_cdylib() -> Option<PathBuf> {
    cargo_artifact("libz.so", "cdylib")
}

/// The staticlib cargo emits, or `None` with a `SKIP:` line.
fn cargo_staticlib() -> Option<PathBuf> {
    cargo_artifact("libz.a", "staticlib")
}

/// Shared body of the two functions above: locate `file_name` in the active profile directory.
fn cargo_artifact(file_name: &str, kind: &str) -> Option<PathBuf> {
    if !elf_tooling_available() {
        return None;
    }
    let Some(directory) = profile_dir() else {
        skip("the running test executable has no resolvable parent directory");
        return None;
    };
    let artifact = directory.join(file_name);
    if !artifact.is_file() {
        skip(&format!(
            "{kind} not found at {}; run `cargo build -p libz-rs-sys --features libz-compat,gz` \
             first (`cargo test` does not build it)",
            artifact.display()
        ));
        return None;
    }
    Some(artifact)
}

// =================================================================================================
// Reporting a difference
// =================================================================================================

/// Every decorated string the packaged library must export -- functions and version nodes together.
fn expected_packaged_symbols() -> BTreeSet<&'static str> {
    VERSION_NODES
        .iter()
        .chain(VERSIONED_EXPORTS.iter())
        .chain(UNVERSIONED_EXPORTS.iter())
        .copied()
        .collect()
}

/// Formats a two-sided difference as sorted lists, so that a CI log is diagnosable on its own.
///
/// A reader should never have to re-run `nm` by hand to find out what changed, and the two
/// directions are reported separately because they mean different things: a missing symbol breaks
/// existing consumers at load time, while a surplus symbol is a promise the library did not intend
/// to make and cannot later withdraw. AAP 0.7.1 (b) treats both as breaks.
fn difference_report(
    subject: &str,
    missing: &BTreeSet<String>,
    surplus: &BTreeSet<String>,
) -> String {
    let mut lines = vec![format!(
        "{subject}: {} symbol(s) missing, {} unexpectedly present",
        missing.len(),
        surplus.len()
    )];
    if !missing.is_empty() {
        lines.push(format!("  missing from the build ({}):", missing.len()));
        lines.extend(missing.iter().map(|name| format!("    - {name}")));
    }
    if !surplus.is_empty() {
        lines.push(format!("  unexpectedly present ({}):", surplus.len()));
        lines.extend(surplus.iter().map(|name| format!("    + {name}")));
    }
    lines.push(
        "  The exported surface is frozen (AAP 0.7.1 (b)). Fix the facade's exports or the \
         packaging link -- never the baseline in this file."
            .to_owned(),
    );
    lines.join("\n")
}

// =================================================================================================
// The baseline is internally consistent, and agrees with the contract
// =================================================================================================

/// The three arrays reconcile, and none of them repeats a name.
///
/// The totals are already `const`-asserted, so this adds the property a length check cannot see: a
/// duplicate entry would keep `len()` correct while shrinking the set the diff actually compares,
/// quietly excusing a missing export.
#[test]
fn the_baseline_arithmetic_reconciles() {
    let nodes: BTreeSet<&str> = VERSION_NODES.iter().copied().collect();
    let versioned: BTreeSet<&str> = VERSIONED_EXPORTS.iter().copied().collect();
    let plain: BTreeSet<&str> = UNVERSIONED_EXPORTS.iter().copied().collect();

    assert_eq!(
        nodes.len(),
        VERSION_NODES.len(),
        "VERSION_NODES repeats a name"
    );
    assert_eq!(
        versioned.len(),
        VERSIONED_EXPORTS.len(),
        "VERSIONED_EXPORTS repeats a name"
    );
    assert_eq!(
        plain.len(),
        UNVERSIONED_EXPORTS.len(),
        "UNVERSIONED_EXPORTS repeats a name"
    );

    assert_eq!(
        versioned.len() + plain.len(),
        FUNCTION_TOTAL,
        "54 + 41 must be 95"
    );
    assert_eq!(
        expected_packaged_symbols().len(),
        DYNAMIC_GLOBAL_TOTAL,
        "the three arrays must contribute 111 distinct symbols with no overlap between them"
    );

    // Decoration is what distinguishes the two function arrays; a stray `@@` on the wrong side
    // would make the 54/41 split meaningless while leaving every total correct.
    for name in &versioned {
        assert!(
            name.contains("@@"),
            "{name} is in VERSIONED_EXPORTS but carries no @@ decoration"
        );
    }
    for name in &plain {
        assert!(
            !name.contains("@@"),
            "{name} is in UNVERSIONED_EXPORTS but carries @@ decoration"
        );
    }

    // Every decorated name must bind to one of the declared nodes.
    for name in &versioned {
        let node = name
            .split("@@")
            .nth(1)
            .expect("decorated name has a node suffix");
        assert!(
            nodes.contains(node),
            "{name} binds to {node}, which is not a declared node"
        );
    }
}

/// [`VERSION_NODES`] is exactly the set `zlib.map` declares.
///
/// The constants were measured from the built C library and the map is what the linker consumes, so
/// this pins the two together: an edit to the immutable version script now fails a test instead of
/// silently redefining the ABI.
#[test]
fn the_version_node_baseline_matches_zlib_map() {
    let declared = map_version_nodes();
    assert_eq!(
        declared.len(),
        VERSION_NODES.len(),
        "zlib.map declares {} version nodes, baseline expects {}: {declared:?}",
        declared.len(),
        VERSION_NODES.len()
    );
    assert_eq!(
        declared.iter().copied().collect::<BTreeSet<_>>(),
        VERSION_NODES.iter().copied().collect::<BTreeSet<_>>(),
        "zlib.map's version nodes differ from the measured baseline"
    );
    // Declaration order is inheritance order -- each node extends the previous one -- so the arrays
    // are kept in that order too, and drift in it is worth catching.
    assert_eq!(
        declared.as_slice(),
        VERSION_NODES.as_slice(),
        "node declaration order changed"
    );
}

/// [`HIDDEN_SYMBOLS`] covers **both** of `zlib.map`'s `local:` blocks, and the `_*` pattern is
/// present.
///
/// This is the test that makes the "two blocks" hazard structural rather than a comment. Reading only
/// `ZLIB_1.2.0`'s block yields nine names; `inflate_fixed` lives alone in `ZLIB_1.3.2`'s block at
/// L115, and a baseline that omitted it would let it leak while every count still looked right.
#[test]
fn the_hidden_name_baseline_matches_zlib_map() {
    let (names, patterns) = map_local_entries();

    assert_eq!(
        names.iter().copied().collect::<BTreeSet<_>>(),
        HIDDEN_SYMBOLS.iter().copied().collect::<BTreeSet<_>>(),
        "zlib.map's local: names differ from HIDDEN_SYMBOLS (found {names:?})"
    );
    assert!(
        names.contains(&"inflate_fixed"),
        "inflate_fixed was not found; the second local: block (zlib.map L114-L115) was not read"
    );
    assert_eq!(
        patterns,
        vec!["_*"],
        "zlib.map's local: wildcard set changed; underscore_symbols_hidden encodes exactly `_*`"
    );
}

// =================================================================================================
// The packaged library -- full 111-symbol parity
// =================================================================================================

/// The dynamic table holds exactly 111 globals, split 95 type-`T` to 16 type-`A`.
#[test]
fn dynamic_symbol_count_and_split() {
    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let functions = symbols.iter().filter(|symbol| symbol.kind == 'T').count();
    let nodes = symbols.iter().filter(|symbol| symbol.kind == 'A').count();

    // Anything that is neither is an ABI difference in its own right: a `D`/`B` entry would be an
    // exported data object and a `W` entry a weak definition, and zlib's contract has neither.
    let unexpected: Vec<&DynSymbol> = symbols
        .iter()
        .filter(|symbol| symbol.kind != 'T' && symbol.kind != 'A')
        .collect();
    assert!(
        unexpected.is_empty(),
        "{} exports neither T nor A: {unexpected:?}",
        library.display()
    );

    assert_eq!(
        symbols.len(),
        DYNAMIC_GLOBAL_TOTAL,
        "{} exports {} dynamic globals, expected {DYNAMIC_GLOBAL_TOTAL}",
        library.display(),
        symbols.len()
    );
    assert_eq!(
        functions, FUNCTION_TOTAL,
        "expected {FUNCTION_TOTAL} type-T functions"
    );
    assert_eq!(
        nodes,
        VERSION_NODES.len(),
        "expected {} type-A version nodes",
        VERSION_NODES.len()
    );

    let decorated = symbols
        .iter()
        .filter(|symbol| symbol.name.contains("@@"))
        .count();
    assert_eq!(
        decorated,
        VERSIONED_EXPORTS.len(),
        "expected {} @@-decorated exports, found {decorated}",
        VERSIONED_EXPORTS.len()
    );
}

/// Every expected decorated string is present -- nothing a consumer resolves has disappeared.
#[test]
fn no_missing_exports() {
    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let present: BTreeSet<String> = symbols.into_iter().map(|symbol| symbol.name).collect();
    let missing: BTreeSet<String> = expected_packaged_symbols()
        .into_iter()
        .filter(|name| !present.contains(*name))
        .map(str::to_owned)
        .collect();

    assert!(
        missing.is_empty(),
        "{}",
        difference_report(
            &format!("{} is missing exports", library.display()),
            &missing,
            &BTreeSet::new()
        )
    );
}

/// No unexpected decorated string is present -- the library promises nothing extra.
///
/// The reverse direction matters as much as the forward one. Once a symbol is published, consumers
/// may bind to it and it can never be withdrawn without an ABI break, so an accidental export is a
/// permanent liability rather than a harmless extra.
#[test]
fn no_surplus_exports() {
    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let expected = expected_packaged_symbols();
    let surplus: BTreeSet<String> = symbols
        .into_iter()
        .map(|symbol| symbol.name)
        .filter(|name| !expected.contains(name.as_str()))
        .collect();

    assert!(
        surplus.is_empty(),
        "{}",
        difference_report(
            &format!("{} exports surplus symbols", library.display()),
            &BTreeSet::new(),
            &surplus
        )
    );
}

/// None of `zlib.map`'s ten `local:` names reaches the dynamic table.
///
/// Checked against both the bare name and any `@@`-decorated form, because a name bound to a version
/// node is still exported -- matching only the bare spelling would miss it.
#[test]
fn version_script_local_symbols_hidden() {
    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let leaked: Vec<String> = symbols
        .iter()
        .filter(|symbol| {
            let bare = symbol.name.split("@@").next().unwrap_or(&symbol.name);
            HIDDEN_SYMBOLS.contains(&bare)
        })
        .map(|symbol| format!("{} {}", symbol.kind, symbol.name))
        .collect();

    assert!(
        leaked.is_empty(),
        "{} leaks names zlib.map marks local: {leaked:?}\n  \
         The packaging link must pass --version-script={}",
        library.display(),
        repo_root().join("zlib.map").display()
    );
}

/// No underscore-prefixed name reaches the dynamic table, as `zlib.map`'s `_*` pattern requires.
///
/// One pattern covers the whole `_tr_*` family -- `_tr_init`, `_tr_stored_block`, `_tr_flush_bits`,
/// `_tr_align`, `_tr_flush_block`, `_tr_tally` -- and the port's own `_zlib_rs_*` helpers, which the
/// cargo `cdylib` does export because no version script hides them there. Verified against the
/// reference library: zero matches.
#[test]
fn underscore_symbols_hidden() {
    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let leaked: Vec<&str> = symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .filter(|name| name.starts_with('_'))
        .collect();

    assert!(
        leaked.is_empty(),
        "{} exports {} underscore-prefixed name(s), which zlib.map's `_*` pattern forbids: {leaked:?}",
        library.display(),
        leaked.len()
    );
}

/// The `SONAME` is `libz.so.1`, byte for byte what the C build records.
///
/// This is what makes an already-linked binary carrying `DT_NEEDED libz.so.1` bind to this library,
/// and it is the reason the symlink chain below is required rather than cosmetic.
#[test]
fn soname_is_libz_so_1() {
    let Some(library) = packaged_library() else {
        return;
    };

    match recorded_soname(&library) {
        // `tool_output` has already printed why the tool could not run; this adds what went
        // unverified because of it, so a skipped SONAME check is attributable to this assertion
        // rather than only to a generic "binutils missing" line further up the log.
        SonameProbe::ToolUnavailable => packaged_unavailable(&format!(
            "cannot read DT_SONAME from {}: `readelf` is unavailable, so the packaged library's \
             SONAME was not verified",
            library.display()
        )),
        SonameProbe::Absent => panic!(
            "{} records no DT_SONAME; the packaging link must pass -Wl,-soname,{EXPECTED_SONAME}, \
             or an already-linked consumer carrying DT_NEEDED {EXPECTED_SONAME} will never bind it",
            library.display()
        ),
        SonameProbe::Recorded(soname) => assert_eq!(
            soname,
            EXPECTED_SONAME,
            "{} records SONAME {soname}, expected {EXPECTED_SONAME}",
            library.display()
        ),
    }
}

/// `libz.so`, `libz.so.1` and `libz.so.<ZLIB_VERSION>` all exist in the staged directory and all
/// resolve to one file.
///
/// The chain is not decoration. Because the `SONAME` is `libz.so.1`, the loader looks for a file with
/// that exact name; a directory offering only `libz.so` lets a program link successfully and then
/// bind the **system** zlib at run time -- reproduced during planning, where the probe printed the
/// system version until `-Wl,-rpath` was added. Deleting this test would let a "drop-in replacement"
/// suite pass while exercising a different library.
///
/// ★ The **topology** is asserted too, not merely that the three names resolve to one object:
/// `libz.so.<ZLIB_VERSION>` must be the regular file, and `libz.so.1` and `libz.so` must each be a
/// symlink whose recorded target is that bare versioned name. That is exactly what `Makefile.in`
/// does for the C library (`ln -s $@ libz.so` and `ln -s $@ libz.so.1`, L2500-L2501) and for the
/// Rust one (L1123-L1126), so requiring it is parity rather than over-specification. The weaker
/// "all three exist and agree" form this test used to carry is satisfied by three independent
/// regular files, or by a copy taken with `cp -L`, and both of those *look* installed while
/// behaving differently: `make install` and the `CMake` install rules recreate links, a versioned
/// tree of copies triples on disk, and a consumer inspecting the chain to find the real object --
/// as the `dropin` job of `.github/workflows/rust.yml` does -- cannot tell a producer that never
/// made the links from one whose links were repaired downstream. `read_link` is used rather than
/// `canonicalize` for that half deliberately: it reports what the producer *recorded*, which is
/// the thing under test.
#[test]
fn versioned_symlink_chain_present() {
    if !elf_tooling_available() {
        packaged_unavailable(
            "the ELF tooling guard failed, so the staged symlink topology went unverified; \
             install binutils (`nm`, `readelf`)",
        );
        return;
    }
    let Some(directory) = packaged_dir() else {
        packaged_unavailable("no packaged library directory; run `make rust` to stage the drop-in");
        return;
    };
    let versioned = packaged_file_name();
    let chain = ["libz.so", EXPECTED_SONAME, versioned.as_str()];

    let real = directory.join(&versioned);
    if !real.is_file() {
        packaged_unavailable(&format!(
            "packaged library not found at {}; run `make rust`",
            real.display()
        ));
        return;
    }

    // The producer's own topology, read without dereferencing anything.
    let kind = real
        .symlink_metadata()
        .unwrap_or_else(|error| panic!("{} cannot be inspected ({error})", real.display()))
        .file_type();
    assert!(
        kind.is_file(),
        "{} is not a regular file; the versioned name is the object itself in both the C recipe \
         and the Rust one, with the two shorter names pointing at it",
        real.display()
    );
    for alias in ["libz.so", EXPECTED_SONAME] {
        let path = directory.join(alias);
        let kind = path
            .symlink_metadata()
            .unwrap_or_else(|error| panic!("{} cannot be inspected ({error})", path.display()))
            .file_type();
        assert!(
            kind.is_symlink(),
            "{} is not a symlink; `make rust` records it as `ln -s {versioned}`, and a copy in its \
             place hides a producer that never staged the chain",
            path.display()
        );
        let target = path
            .read_link()
            .unwrap_or_else(|error| panic!("{} cannot be read ({error})", path.display()));
        assert_eq!(
            target,
            Path::new(&versioned),
            "{} records target {} rather than the bare {versioned}",
            path.display(),
            target.display()
        );
    }

    let mut resolved = Vec::new();
    for name in chain {
        let path = directory.join(name);
        assert!(
            path.symlink_metadata().is_ok(),
            "{} is absent; the loader searches for {EXPECTED_SONAME} by that exact name, and a \
             directory without the full chain falls through to the system zlib",
            path.display()
        );
        let target = fs::canonicalize(&path)
            .unwrap_or_else(|error| panic!("{} does not resolve ({error})", path.display()));
        resolved.push((name, target));
    }

    let (_, first) = &resolved[0];
    for (name, target) in &resolved[1..] {
        assert_eq!(
            target,
            first,
            "{name} resolves to {} but libz.so resolves to {}; the chain must name one object",
            target.display(),
            first.display()
        );
    }
    assert_eq!(
        fs::canonicalize(&real).expect("the staged library resolves"),
        *first,
        "the chain does not resolve to {}",
        real.display()
    );
}

// =================================================================================================
// The `inflate_table` asymmetry
// =================================================================================================

/// `inflate_table` is a defined global in `libz.a` and absent from the packaged library's dynamic
/// table.
///
/// Both halves are requirements, and they pull in opposite directions, which is exactly why this is
/// asserted positively instead of left to the two general tests above. `test/infcover.c`'s
/// `cover_trees()` calls `inflate_table` directly at L632 and L636 and links `ZLIB::ZLIBSTATIC`
/// (`test/CMakeLists.txt` L95-L96), so removing it from the archive breaks that build; leaving it in
/// the shared library's dynamic table breaks parity, because `zlib.map` lists it under `local:`. The
/// C build resolves the tension the same way -- measured: `nm libz.a` shows `T inflate_table` while
/// `nm -D` on the reference `libz.so.1.3.2.1-motley` shows none.
///
/// The same asymmetry holds for `inflate_fast`, `zcalloc`, `zcfree`, `inflate_fixed` and the `_tr_*`
/// family; `inflate_table` is singled out because it is the one with a hard external consumer.
///
/// ★ The archive half is what makes the `.hidden` directive in `src/lib.rs` safe to add. ELF
/// hidden visibility keeps a symbol out of a *shared object's* dynamic table and changes static
/// linking not at all, which is why `nm` still reports `T` here -- and why this assertion is the
/// one that would fail if a future edit reached for a mechanism that does remove the definition.
#[test]
fn inflate_table_static_only() {
    if let Some(archive) = cargo_staticlib() {
        let path = archive.to_str().expect("artifact path is not valid UTF-8");
        if let Some(output) = tool_output("nm", &["--defined-only", "--extern-only", path]) {
            let defines_it = parse_nm(&output)
                .iter()
                .any(|symbol| symbol.name == "inflate_table" && symbol.kind == 'T');
            assert!(
                defines_it,
                "{} does not define inflate_table as a global text symbol; test/infcover.c \
                 calls it directly and links the static library, so it would fail to link",
                archive.display()
            );
        }
    }

    let Some(library) = packaged_library() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };
    let exported: Vec<&str> = symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .filter(|name| name.split("@@").next().unwrap_or(name) == "inflate_table")
        .collect();
    assert!(
        exported.is_empty(),
        "{} exports inflate_table ({exported:?}); zlib.map's local: block must hide it",
        library.display()
    );
}

// =================================================================================================
// The cargo artifacts -- their own measured shape, not the packaged one
// =================================================================================================

/// The cargo **static** archive defines the whole 95-function contract, `gzprintf` included.
///
/// This is the positive half of the artifact matrix in `src/lib.rs`, and it is deliberately a gate
/// rather than a sentence: that table's first row claims `cargo build -p libz-rs-sys --features
/// libz-compat` yields a *complete* static library, and until this test existed the claim rested on
/// a measurement someone took once by hand. It is also the half that matters most to a reader of
/// [`cargo_cdylib_matches_its_measured_shape`], which pins an artifact that is deliberately
/// **in**complete: without this test beside it, the suite would document a shortfall and assert
/// nothing about the artifact that does not have one.
///
/// `gzprintf` and `gzvprintf` are the two names the distinction turns on. They are variadic, stable
/// Rust cannot define a variadic function (`c_variadic`, rust-lang/rust#44930), so `build.rs`
/// compiles them from `csrc/gzprintf_shim.c` and rustc merges that object into `libz.a`. A `cdylib`
/// gives a C-contributed symbol no dynamic entry, which is why [`CDYLIB_ABSENT`] exists; an archive
/// has no such filter, so here they must be present. Asserting them by name — rather than trusting
/// the 95-name loop to notice — is what makes a regression in the shim's compilation legible.
///
/// The symbol type is required to be `T`: a name that arrived as a common or undefined entry would
/// satisfy a presence check and then fail to link.
#[test]
fn cargo_staticlib_defines_the_whole_contract() {
    let Some(archive) = cargo_staticlib() else {
        return;
    };
    let path = archive.to_str().expect("artifact path is not valid UTF-8");
    let Some(output) = tool_output("nm", &["--defined-only", "--extern-only", path]) else {
        return;
    };

    // `parse_nm` returns owned names, so the parse is bound to a local and the set borrows from it.
    let symbols = parse_nm(&output);
    let defined: BTreeSet<&str> = symbols
        .iter()
        .filter(|symbol| symbol.kind == 'T')
        .map(|symbol| {
            let name = symbol.name.as_str();
            name.split("@@").next().unwrap_or(name)
        })
        .collect();
    let contract: BTreeSet<&str> = VERSIONED_EXPORTS
        .iter()
        .map(|name| name.split("@@").next().unwrap_or(name))
        .chain(UNVERSIONED_EXPORTS.iter().copied())
        .collect();
    assert_eq!(
        contract.len(),
        FUNCTION_TOTAL,
        "the undecorated contract must have 95 names"
    );

    let missing: BTreeSet<String> = contract
        .iter()
        .filter(|name| !defined.contains(**name))
        .map(|name| (*name).to_owned())
        .collect();
    assert!(
        missing.is_empty(),
        "{}",
        difference_report(
            &format!(
                "{} does not define the whole zlib.h contract, so it is not the complete static \
                 drop-in src/lib.rs's artifact matrix says it is",
                archive.display()
            ),
            &missing,
            &BTreeSet::new()
        )
    );

    for shimmed in CDYLIB_ABSENT {
        assert!(
            defined.contains(shimmed),
            "{} does not define {shimmed} as a global text symbol. It is variadic, so it comes \
             from csrc/gzprintf_shim.c rather than from Rust; its absence here means build.rs did \
             not compile the shim or did not emit `-l static=` for the archive it goes into, and \
             the packaged shared library relinked from this archive would lose it too",
            archive.display()
        );
    }
}

/// The arming switch parses, and the mode it selected is printed.
///
/// Unconditional, and that is the whole reason it exists. [`packaged_required`] is otherwise
/// consulted only when an artifact turns out to be missing, so a misspelled
/// `ZLIB_RS_REQUIRE_PACKAGED` -- `true`, `yes`, `ON` -- would go unnoticed on any run where
/// everything happened to be in place, and would then be discovered as a *skip* on the one run where
/// the artifact was absent and the gate was needed. Validating the value here fails the run
/// immediately instead, and printing the resolved mode puts "armed" or "unarmed" in the log so a
/// reader of a green CI run can tell which of the two they are looking at.
#[test]
fn the_arming_switch_parses() {
    let armed = packaged_required();
    println!(
        "{REQUIRE_PACKAGED_VAR}={}: the packaged-artifact gates are {}",
        env::var(REQUIRE_PACKAGED_VAR).unwrap_or_else(|_| "<unset>".to_owned()),
        if armed {
            "REQUIRED -- a missing drop-in fails"
        } else {
            "advisory -- a missing drop-in skips"
        }
    );
}

// =================================================================================================
// The flags word against the artifacts that ship
// =================================================================================================

/// The two variadic entry points `zlibCompileFlags()` bit 27 speaks about.
///
/// The same pair as [`CDYLIB_ABSENT`] by construction, and that is the point rather than a
/// coincidence: they are absent from the `cdylib` *because* they are the ones that cannot be
/// defined in Rust, and bit 27 is the ABI's way of saying whether they exist. Spelled separately so
/// that the test below reads as a statement about the flags word instead of borrowing a name that
/// describes a different artifact.
const VARIADIC_EXPORTS: [&str; 2] = CDYLIB_ABSENT;

/// `zlibCompileFlags()` bit 27 agrees with the symbol table of every artifact that ships.
///
/// # What the bit means, and why a *symbol* test owns it
///
/// `zlib.h` L1252 defines bit 27 as "0 = `gzprintf()` present, 1 = not -- 1 means `gzprintf()`
/// returns an error", and `src/util.rs` derives it from `cfg(zlib_rs_gzprintf)`, which `build.rs`
/// sets only when it actually compiled `csrc/gzprintf_shim.c`. That ties the bit to the
/// *compilation*. It does not, by itself, tie it to the **export**, and the two can diverge: the
/// shim's objects go into `libz.a`, and a `cdylib` gives a C-contributed symbol no dynamic entry at
/// all, so cargo's `libz.so` reports bit 27 clear while its `.dynsym` has neither name. One
/// compilation of this crate produces all three artifact kinds from the same object code -- rustc
/// receives `--crate-type` three times in a single invocation -- so a *different* flags word per
/// artifact is not something a `cfg` could express even in principle.
///
/// The resolution is the one the artifact matrix in `src/lib.rs` already states: the `cdylib` is not
/// a shipping artifact. What ships is `libz.a` and the packaged
/// `libz.so.<ZLIB_VERSION>` relinked from it, and for both of those the bit must be honest. This
/// test is what makes that a gate:
///
/// * bit 27 **clear** requires `gzprintf` and `gzvprintf` to be present in the artifact -- a
///   consumer that reads the flags word and then calls `gzprintf` must not meet an undefined
///   symbol;
/// * bit 27 **set** requires them to be *absent* -- a build that advertises the stub behaviour of
///   `gzwrite.c` L406-L412 while exporting the real thing is lying in the other direction.
///
/// The `cdylib`'s deviation is pinned separately, and deliberately, by
/// [`cargo_cdylib_matches_its_measured_shape`] -- so it cannot spread to an artifact that ships
/// without a test going red.
///
/// This file compiles only with `libz-compat` and `gz` (see the crate-level `#![cfg]`), so the
/// expected answer here is "bit clear, both names present". That is not a tautology: if `build.rs`
/// stopped compiling the shim, or emitted the archive without `-l static=`, the cfg would clear and
/// the bit would set while the packaged library still had to be relinked from an archive that no
/// longer defined them -- which is precisely the drift this catches.
#[test]
fn compile_flags_bit_27_agrees_with_the_shipping_artifacts() {
    let flags = z::zlibCompileFlags();
    let claims_present = flags >> 27 & 1 == 0;
    let mut inspected = 0_usize;

    // The packaged shared library: the dynamic table is what a run-time consumer resolves against.
    if let Some(library) = packaged_library() {
        if let Some(symbols) = dynamic_symbols(&library) {
            let exported: BTreeSet<&str> = symbols
                .iter()
                .map(|symbol| {
                    let name = symbol.name.as_str();
                    name.split("@@").next().unwrap_or(name)
                })
                .collect();
            for name in VARIADIC_EXPORTS {
                assert_eq!(
                    exported.contains(name),
                    claims_present,
                    "zlibCompileFlags() = 0x{flags:x} has bit 27 {}, so {name} must be {} in {}, \
                     and it is not. The flags word is part of the ABI: a consumer reads it to \
                     decide whether gzprintf is usable",
                    if claims_present { "clear" } else { "set" },
                    if claims_present { "exported" } else { "absent" },
                    library.display()
                );
            }
            inspected += 1;
        }
    }

    // The static archive: the same claim, resolved at link time instead.
    if let Some(archive) = cargo_staticlib() {
        let path = archive.to_str().expect("artifact path is not valid UTF-8");
        if let Some(output) = tool_output("nm", &["--defined-only", "--extern-only", path]) {
            let symbols = parse_nm(&output);
            let defined: BTreeSet<&str> = symbols
                .iter()
                .filter(|symbol| symbol.kind == 'T')
                .map(|symbol| symbol.name.as_str())
                .collect();
            for name in VARIADIC_EXPORTS {
                assert_eq!(
                    defined.contains(name),
                    claims_present,
                    "zlibCompileFlags() = 0x{flags:x} has bit 27 {}, so {name} must be {} in {}",
                    if claims_present { "clear" } else { "set" },
                    if claims_present {
                        "a defined global text symbol"
                    } else {
                        "undefined"
                    },
                    archive.display()
                );
            }
            inspected += 1;
        }
    }

    if inspected == 0 {
        skip(
            "neither shipping artifact was available, so bit 27 was checked against nothing; run \
             `make rust` for target/dropin and `cargo build -p libz-rs-sys` for libz.a",
        );
    }
}

// =================================================================================================
// The cargo artifacts' own measured shape
// =================================================================================================

/// The cargo `cdylib` exports 96 type-`T` symbols and no version nodes, and its delta from the
/// contract is exactly the documented one.
///
/// ★ This pins a **development** artifact, and it must not be read as a statement that the artifact
/// is fit to install. `cargo build` emits it, `cargo test` links nothing against it, and the
/// artifact matrix in `src/lib.rs` marks it not installable: it is short two exports and all 16
/// version nodes, and no argument to cargo closes either gap. The shipping shared library is what
/// `make rust` relinks from `libz.a` and stages in `target/dropin`, which the packaged tests above
/// hold to full parity.
///
/// So this is not a relaxation of the parity gate; it is a second, independent gate whose subject is
/// the deviation itself. rustc attaches its own anonymous version script to a `cdylib` link, which
/// has three measured consequences: `zlib.map` cannot be layered on (so zero version nodes), a
/// symbol defined by a C object in the archive gets no dynamic entry (so `gzprintf` and `gzvprintf`
/// are absent), and the `_zlib_rs_*` helpers that `zlib.map`'s `_*` pattern would hide are visible.
/// Pinning that shape catches a new or lost export in the cargo build immediately, months before
/// anyone runs `make rust` -- and, just as importantly, makes any *change* to the deviation visible
/// rather than absorbed.
#[test]
fn cargo_cdylib_matches_its_measured_shape() {
    let Some(library) = cargo_cdylib() else {
        return;
    };
    let Some(symbols) = dynamic_symbols(&library) else {
        return;
    };

    let names: BTreeSet<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();

    let nodes: Vec<&DynSymbol> = symbols.iter().filter(|symbol| symbol.kind == 'A').collect();
    assert!(
        nodes.is_empty(),
        "{} carries version nodes {nodes:?}; rustc's anonymous version script makes that \
         impossible, so the build has changed in a way this suite does not model",
        library.display()
    );
    assert!(
        !names.iter().any(|name| name.contains("@@")),
        "{} carries @@-decorated symbols, which a rustc-linked cdylib cannot produce",
        library.display()
    );

    // The 95 contract functions, undecorated, are the reference point here: without `zlib.map` there
    // is no decoration to compare against.
    let contract: BTreeSet<&str> = VERSIONED_EXPORTS
        .iter()
        .map(|name| name.split("@@").next().unwrap_or(name))
        .chain(UNVERSIONED_EXPORTS.iter().copied())
        .collect();
    assert_eq!(
        contract.len(),
        FUNCTION_TOTAL,
        "the undecorated contract must have 95 names"
    );

    let missing: BTreeSet<String> = contract
        .iter()
        .filter(|name| !names.contains(**name) && !CDYLIB_ABSENT.contains(*name))
        .map(|name| (*name).to_owned())
        .collect();
    let surplus: BTreeSet<String> = names
        .iter()
        .filter(|name| !contract.contains(**name) && !CDYLIB_INTERNALS.contains(*name))
        .map(|name| (*name).to_owned())
        .collect();
    assert!(
        missing.is_empty() && surplus.is_empty(),
        "{}",
        difference_report(
            &format!("{} diverges from the cdylib baseline", library.display()),
            &missing,
            &surplus
        )
    );

    for absent in CDYLIB_ABSENT {
        assert!(
            !names.contains(absent),
            "{} exports {absent}; it is defined in csrc/gzprintf_shim.c, and rustc's `local: *` \
             leaves a C-contributed symbol out of the dynamic table -- if that has changed, the \
             packaging story in src/lib.rs needs revisiting",
            library.display()
        );
    }
    for internal in CDYLIB_INTERNALS {
        assert!(
            !names.contains(internal),
            "{} exports the internal helper {internal}; the `.hidden` directives in \
             crates/libz-rs-sys/src/lib.rs must keep it out of the dynamic table, and `libz.a` \
             is where csrc/gzprintf_shim.c and test/infcover.c resolve against it",
            library.display()
        );
    }

    assert_eq!(
        symbols.len(),
        CDYLIB_TOTAL,
        "{} exports {} dynamic globals, expected {CDYLIB_TOTAL} (95 contract functions, less the \
         {} a C object defines, plus none of the {} internal helpers)",
        library.display(),
        symbols.len(),
        CDYLIB_ABSENT.len(),
        CDYLIB_INTERNALS.len()
    );

    // The cdylib does record the contract SONAME even though it is not installable, so that a
    // relinked or repackaged copy cannot silently lose it. `build.rs` emits the argument only when
    // `libz-compat` is on -- an artifact exporting nothing must not claim to be `libz.so.1` -- and
    // this suite compiles only with that feature, so a recorded value is *required* here, not merely
    // compared when one happens to be present.
    //
    // ★ The three outcomes are matched exhaustively on purpose. An `if let Recorded(..)` swallows
    // `Absent`, and `Absent` is exactly the regression this assertion exists to catch: it is what a
    // `build.rs` that stopped emitting `-Wl,-soname` produces, and a check that passes in that state
    // reports success for an artifact no already-linked consumer could ever bind. Only a genuinely
    // missing tool is a skip, and it says so.
    match recorded_soname(&library) {
        SonameProbe::ToolUnavailable => skip(&format!(
            "cannot read DT_SONAME from {}: `readelf` is unavailable, so the cdylib's SONAME was \
             not verified",
            library.display()
        )),
        SonameProbe::Absent => panic!(
            "{} records no DT_SONAME; build.rs must emit -Wl,-soname,{EXPECTED_SONAME} whenever \
             `libz-compat` is on, and this suite compiles only with that feature -- an artifact \
             without it cannot be bound by a consumer carrying DT_NEEDED {EXPECTED_SONAME}",
            library.display()
        ),
        SonameProbe::Recorded(soname) => assert_eq!(
            soname,
            EXPECTED_SONAME,
            "{} records SONAME {soname}, expected {EXPECTED_SONAME}",
            library.display()
        ),
    }
}

/// The cargo artifact directory holds **no** versioned library names.
///
/// `build.rs` prunes `libz.so.1` and `libz.so.<ZLIB_VERSION>` from that directory on purpose, and the
/// reason is measured rather than theoretical. A build script runs *before* the link, so it cannot
/// distinguish a `cargo check` from a `cargo build`; each of `cargo check`, a failed build and
/// `cargo clean -p` left the two names behind as **dangling** links, which the loader skips exactly
/// as if they were absent. Worse, cargo puts that directory first on the library search path of every
/// build script it launches, so an incomplete `libz.so.1` there is loaded by the `as`, `ar`, `nm` and
/// `rustc` doing the building -- once producing a hard
/// `as: symbol lookup error: undefined symbol: deflate`. The versioned names therefore exist in
/// exactly one place, which is what `make rust` stages, and `build.rs` leaves a
/// `README-cargo-artifacts.txt` in their place saying so.
#[test]
fn cargo_artifact_directory_holds_no_versioned_names() {
    let Some(library) = cargo_cdylib() else {
        return;
    };
    let directory = library.parent().expect("the cdylib has a parent directory");

    for name in [EXPECTED_SONAME.to_owned(), packaged_file_name()] {
        let path = directory.join(&name);
        // ★ `is_err()` is NOT absence, and the distinction matters here in the direction that
        // hides a defect: an `EACCES` on the directory, or any other inspection failure, would
        // satisfy a bare `is_err()` while the alias sat there. Only `NotFound` means "not
        // present"; anything else is reported as what it is. This is the same idiom `build.rs`'s
        // `prune_retired_alias` is held to, for the same reason.
        match path.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "{} could not be inspected ({error}); this test cannot conclude the versioned \
                 alias is absent, and an alias that is present here is loaded by the tools \
                 building the workspace",
                path.display()
            ),
            Ok(_) => panic!(
                "{} exists; the versioned names belong only to what `make rust` stages, because a \
                 build script cannot keep them non-dangling and cargo puts this directory on the \
                 library search path of the tools building the workspace",
                path.display()
            ),
        }
    }
}
